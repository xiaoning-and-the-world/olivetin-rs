//! WxPusher 推送：两种模式、懒初始化客户端、推送状态返回。
//!
//! 极简模式（spt）→ /api/send/message/simple-push；应用模式（app_token）→ /api/send/message。
//! 推送异步（tokio::spawn），完成后回写 push_status。

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::log::LogEntry;

/// 推送状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PushStatus {
    Disabled,
    Ok,
    Failed(String),
}

/// 懒初始化 HTTP 客户端，按配置的 push.timeout 设定超时。
/// 之前固定 `.timeout(30s)`，导致配置里的 `push.timeout` 从未生效（幽灵配置）。
/// 按超时秒数缓存客户端实例（reqwest::Client 内部是 Arc，clone 成本极低）。
/// timeout 取值有限（配置项），不会无限增长。
pub fn client(timeout_secs: u64) -> reqwest::Client {
    use std::sync::Mutex;
    static CLIENTS: OnceLock<Mutex<std::collections::HashMap<u64, reqwest::Client>>> =
        OnceLock::new();
    let map = CLIENTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    {
        let guard = map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.get(&timeout_secs) {
            return c.clone();
        }
    }
    let c = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
        .build()
        // build 失败（TLS 后端不可用）时退化为默认客户端，避免 panic=abort 终止进程
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(timeout_secs).or_insert_with(|| c.clone());
    c
}

/// 构造推送内容。
pub fn build_content(entry: &LogEntry, include_output: bool) -> String {
    let mut s = String::new();
    s.push_str(&format!("动作: {}\n", entry.action_title));
    s.push_str(&format!(
        "版本: {}\n",
        entry.action_version.as_deref().unwrap_or("none")
    ));
    s.push_str(&format!(
        "触发: {}\n",
        match entry.trigger {
            crate::log::TriggerSource::Manual => "手动",
            crate::log::TriggerSource::Schedule => "定时",
        }
    ));
    s.push_str(&format!(
        "退出码: {}\n",
        entry
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));
    s.push_str(&format!("耗时: {}ms\n", entry.duration_ms));
    s.push_str(&format!("超时: {}\n", entry.timed_out));
    s.push_str(&format!("截断: {}\n", entry.truncated));
    if include_output {
        s.push_str("\n--- stdout ---\n");
        s.push_str(&truncate_chars(&entry.stdout, 2000));
        s.push_str("\n--- stderr ---\n");
        s.push_str(&truncate_chars(&entry.stderr, 1000));
    }
    // content 总长度 ≤ 4000 字符
    truncate_chars(&s, 4000)
}

fn truncate_chars(s: &str, max: usize) -> String {
    // 先判超长：不超长直接复用（避免无谓 to_string 整串复制）；
    // 超长时按字符边界截断（不能按字节截，中文会切坏）。
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// 模板变量替换：`${name}` → 对应值；未知变量**原样保留**（便于发现拼写错误，
/// 也不会因为多写一个变量就丢内容）。`$$` 输出字面 `$`。
///
/// 可用变量（脚本可在「推送正文/标题」里直接用）：
/// | 变量 | 含义 |
/// |---|---|
/// | `${action}` / `${title}` | 脚本标题 |
/// | `${action_id}` | 脚本 id |
/// | `${group}` | 分组 |
/// | `${version}` | 活动版本名（无版本管理为 none） |
/// | `${trigger}` | 触发方式：手动 / 定时 |
/// | `${exit_code}` | 退出码（超时为 none） |
/// | `${status}` | 成功 / 失败 |
/// | `${duration_ms}` / `${duration_s}` | 耗时毫秒 / 秒（1 位小数） |
/// | `${stdout}` / `${stderr}` / `${output}` | 标准输出 / 错误 / 两者拼接 |
/// | `${time}` / `${date}` | 开始时间（本地，含日期 / 仅日期） |
/// | `${script_path}` | 本次执行的脚本路径（有则给出） |
pub fn render_template(tpl: &str, entry: &LogEntry) -> String {
    let mut out = String::with_capacity(tpl.len() + 64);
    let chars: Vec<char> = tpl.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '$' {
                out.push('$');
                i += 2;
                continue;
            }
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                // 找配对的 }
                if let Some(end) = (i + 2..chars.len()).find(|&j| chars[j] == '}') {
                    let name: String = chars[i + 2..end].iter().collect();
                    match template_value(name.trim(), entry) {
                        Some(v) => out.push_str(&v),
                        // 未知变量：原样输出，避免静默丢字
                        None => {
                            out.push_str("${");
                            out.push_str(name.trim());
                            out.push('}');
                        }
                    }
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn template_value(name: &str, entry: &LogEntry) -> Option<String> {
    let v = match name {
        "action" | "title" => entry.action_title.clone(),
        "action_id" => entry.action_id.clone(),
        "group" => entry.action_group.clone(),
        "version" => entry
            .action_version
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        "trigger" => match entry.trigger {
            crate::log::TriggerSource::Manual => "手动".to_string(),
            crate::log::TriggerSource::Schedule => "定时".to_string(),
        },
        "exit_code" => entry
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".to_string()),
        "status" => {
            if entry.exit_code == Some(0) && !entry.timed_out {
                "成功".to_string()
            } else {
                "失败".to_string()
            }
        }
        "duration_ms" => entry.duration_ms.to_string(),
        "duration_s" => format!("{:.1}", entry.duration_ms as f64 / 1000.0),
        "stdout" => truncate_chars(&entry.stdout, 2000),
        "stderr" => truncate_chars(&entry.stderr, 1000),
        "output" => {
            let mut s = entry.stdout.clone();
            if !entry.stderr.is_empty() {
                s.push_str("\n--- stderr ---\n");
                s.push_str(&entry.stderr);
            }
            truncate_chars(&s, 2000)
        }
        "time" => entry.started_at_local.clone(),
        "date" => entry
            .started_at_local
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string(),
        "script_path" => entry.script_path.clone().unwrap_or_default(),
        _ => return None,
    };
    Some(v)
}

/// 正文：优先级 action.push_content > 全局 push.content_template > 内置模板。
pub fn content_for(
    entry: &LogEntry,
    push_cfg: &crate::config::PushConfig,
    action: &crate::config::Action,
) -> String {
    let custom = action
        .push_content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            push_cfg
                .content_template
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
    match custom {
        Some(tpl) => truncate_chars(&render_template(tpl, entry), 4000),
        None => build_content(entry, push_cfg.include_output),
    }
}

/// 标题（WxPusher summary）：action.push_title 模板 > 全局 summary_mode > 内置「前缀+动作+耗时」。
///
/// `content` 是已经渲染好的正文，`summary_mode = content` 时从它取第一行有意义的话，
/// 效果就是「不单独起标题，通知栏直接显示正文开头」。
pub fn summary_for(
    entry: &LogEntry,
    push_cfg: &crate::config::PushConfig,
    action: &crate::config::Action,
    content: &str,
) -> String {
    if let Some(tpl) = action
        .push_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return truncate_chars(&render_template(tpl, entry), 100);
    }
    match push_cfg.summary_mode.as_str() {
        "content" => summary_from_content(content),
        // 空串表示「不带 summary 字段」，由调用方决定是否省略
        "none" => String::new(),
        _ => summary(&push_cfg.summary_prefix, entry),
    }
}

/// 从正文里取通知栏摘要：第一行非空文字，去掉 Markdown 标记与行内强调符，截 100 字符。
/// 正文为空时回退到 "OliveTin"（不能给 WxPusher 传空 summary）。
pub fn summary_from_content(content: &str) -> String {
    let raw = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("OliveTin");
    // 逐行剥掉标题/引用/列表前缀，再去掉行内 ** * ` 这些标记
    let mut line = raw.trim_start();
    loop {
        let before = line.len();
        line = line.trim_start_matches('#').trim_start();
        if let Some(rest) = line.strip_prefix('>') {
            line = rest.trim_start();
        }
        for prefix in ["- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                line = rest.trim_start();
            }
        }
        if line.len() == before {
            break;
        }
    }
    let cleaned: String = line.chars().filter(|c| !matches!(c, '*' | '`')).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "OliveTin".to_string()
    } else {
        truncate_chars(cleaned, 100)
    }
}

/// summary 截断到 100 字符。
pub fn summary(prefix: &str, entry: &LogEntry) -> String {
    let base = format!("{} {} {}", prefix, entry.action_title, entry.duration_ms);
    if base.chars().count() > 100 {
        base.chars().take(100).collect()
    } else {
        base
    }
}

/// WxPusher 业务失败会返回 HTTP 200 + {"code":1001,...}，只判状态码会误报成功。
fn wxpusher_business_ok(text: &str) -> Result<(), String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
            if code != 1000 {
                let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("未知错误");
                return Err(format!("WxPusher 业务失败 code={} msg={}", code, msg));
            }
        }
    }
    Ok(())
}

async fn check_wxpusher_response(resp: reqwest::Response) -> Result<(), String> {
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("WxPusher 返回 {} {}", status, text.chars().take(120).collect::<String>()));
    }
    wxpusher_business_ok(&text)
}

/// 实际发送推送（极简模式）。
async fn send_simple(
    spt: &str,
    content: &str,
    summary: &str,
    content_type: u32,
    timeout_secs: u64,
) -> Result<(), String> {
    let url = "https://wxpusher.zjiecode.com/api/send/message/simple-push";
    let mut body = serde_json::json!({
        "spt": spt,
        "content": content,
        "contentType": content_type,
    });
    // summary_mode = none 时完全不传这个字段，由 WxPusher 自行处理
    if !summary.is_empty() {
        body["summary"] = serde_json::json!(summary);
    }
    let resp = client(timeout_secs)
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    check_wxpusher_response(resp).await
}

/// 实际发送推送（应用模式）。
async fn send_app(
    app_token: &str,
    uids: &[String],
    topic_ids: &[u32],
    content: &str,
    summary: &str,
    content_type: u32,
    timeout_secs: u64,
) -> Result<(), String> {
    let url = "https://wxpusher.zjiecode.com/api/send/message";
    let mut body = serde_json::json!({
        "appToken": app_token,
        "content": content,
        "contentType": content_type,
        "uids": uids,
        "topicIds": topic_ids,
    });
    if !summary.is_empty() {
        body["summary"] = serde_json::json!(summary);
    }
    let resp = client(timeout_secs)
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    check_wxpusher_response(resp).await
}

/// 发送测试推送，验证通道是否可用。返回 Ok(()) 或 Err(错误信息)。
pub async fn send_test(cfg: &crate::config::PushConfig) -> Result<(), String> {
    let content = "OliveTin-RS 测试推送：通道配置成功！".to_string();
    let summary = "OliveTin 测试".to_string();
    if let Some(spt) = &cfg.spt {
        send_simple(spt, &content, &summary, cfg.content_type, cfg.timeout).await
    } else if let Some(app_token) = &cfg.app_token {
        let uids = cfg.uids.clone().unwrap_or_default();
        let topics = cfg.topic_ids.clone().unwrap_or_default();
        send_app(
            app_token,
            &uids,
            &topics,
            &content,
            &summary,
            cfg.content_type,
            cfg.timeout,
        )
        .await
    } else {
        Err("未配置 spt 或 app_token".to_string())
    }
}

/// 发送推送的编排：spawn 异步、回写 push_status。
/// `entry_id` 用于回写日志条目。调用方（executor）已根据 action.push 与 push_condition 决定是否调用。
///
/// `action` 提供按脚本的覆盖项：标题/正文模板、接收者（uids / topicIds）、contentType。
/// 未设置的项回落到全局 `push` 配置。
pub async fn dispatch(
    state: &crate::state::AppState,
    entry_id: u64,
    entry: LogEntry,
    action: crate::config::Action,
) {
    let config = state.config.load();
    let push_cfg = config.push.clone();
    if !push_cfg.enabled {
        update_push_status(state, entry_id, PushStatus::Disabled).await;
        return;
    }

    // 接收者：脚本级覆盖 > 全局；空列表视为未设置
    let uids = action
        .push_uids
        .clone()
        .filter(|v| !v.is_empty())
        .or_else(|| push_cfg.uids.clone())
        .unwrap_or_default();
    let topics = action
        .push_topic_ids
        .clone()
        .filter(|v| !v.is_empty())
        .or_else(|| push_cfg.topic_ids.clone())
        .unwrap_or_default();
    let content_type = action.push_content_type.unwrap_or(push_cfg.content_type);

    let state2 = state.clone();
    tokio::spawn(async move {
        let content = content_for(&entry, &push_cfg, &action);
        let summary = summary_for(&entry, &push_cfg, &action, &content);
        let result = if let Some(spt) = &push_cfg.spt {
            send_simple(spt, &content, &summary, content_type, push_cfg.timeout).await
        } else if let Some(app_token) = &push_cfg.app_token {
            send_app(app_token, &uids, &topics, &content, &summary, content_type, push_cfg.timeout).await
        } else {
            Err("未配置 spt 或 app_token".to_string())
        };
        let status = match result {
            Ok(_) => PushStatus::Ok,
            Err(e) => PushStatus::Failed(e),
        };
        update_push_status(&state2, entry_id, status).await;
    });
}

async fn update_push_status(
    state: &crate::state::AppState,
    entry_id: u64,
    status: PushStatus,
) {
    let mut logs = state.logs.lock().await;
    if let Some(entry) = logs.iter_mut().find(|e| e.id == entry_id) {
        entry.push_status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wxpusher_http200_business_error_is_failure() {
        assert!(wxpusher_business_ok(r#"{"code":1000,"msg":"处理成功"}"#).is_ok());
        let e = wxpusher_business_ok(r#"{"code":1001,"msg":"SPT失效"}"#).unwrap_err();
        assert!(e.contains("1001") && e.contains("SPT失效"), "实际: {e}");
        assert!(wxpusher_business_ok("not-json").is_ok(), "非 JSON 响应不应误判失败");
    }

    #[test]
    fn truncate_respects_chars() {
        let s = "中文测试内容".repeat(100);
        let t = truncate_chars(&s, 10);
        assert_eq!(t.chars().count(), 10);
    }

    #[test]
    fn summary_truncated_to_100() {
        let e = LogEntry {
            id: 1,
            action_id: "a".into(),
            action_title: "t".into(),
            action_group: "g".into(),
            started_at: "".into(),
            started_at_local: "".into(),
            duration_ms: 10,
            exit_code: None,
            stdout: "".into(),
            stderr: "".into(),
            truncated: false,
            timed_out: false,
            trigger: crate::log::TriggerSource::Manual,
            push_status: PushStatus::Ok,
            remote_addr: None,
            action_version: None,
            kind: "run".into(),
            script_path: None,
        };
        let s = summary("OliveTin", &e);
        assert!(s.chars().count() <= 100);
    }

    // ===== 自定义推送：模板与覆盖项 =====

    fn sample_entry() -> LogEntry {
        LogEntry {
            id: 7,
            action_id: "led".into(),
            action_title: "开灯".into(),
            action_group: "路由器".into(),
            started_at: "2026-11-12T00:00:00Z".into(),
            started_at_local: "2026-11-12 08:00:00".into(),
            duration_ms: 1500,
            exit_code: Some(0),
            stdout: "绿灯已开启\n".into(),
            stderr: "".into(),
            truncated: false,
            timed_out: false,
            trigger: crate::log::TriggerSource::Schedule,
            push_status: PushStatus::Disabled,
            remote_addr: None,
            action_version: None,
            kind: "run".into(),
            script_path: Some("/opt/olivetin/led-on.sh".into()),
        }
    }

    fn empty_action() -> crate::config::Action {
        serde_json::from_value(serde_json::json!({
            "id": "led", "title": "开灯", "exec": ["/bin/true"]
        }))
        .unwrap()
    }

    fn push_cfg() -> crate::config::PushConfig {
        serde_json::from_value(serde_json::json!({
            "enabled": true, "spt": "SPT_x", "content_type": 1
        }))
        .unwrap()
    }

    #[test]
    fn global_content_template_is_used_when_action_has_none() {
        let e = sample_entry();
        let mut cfg = push_cfg();
        let mut a = empty_action();

        // 全局模板（Markdown 骨架）在 action 未配置时生效
        cfg.content_template = Some("### ${action} · ${status}\n\n```\n${stdout}```\n".into());
        let out = content_for(&e, &cfg, &a);
        assert!(out.starts_with("### 开灯 · 成功"), "{out}");
        assert!(out.contains("绿灯已开启"), "{out}");
        assert!(!out.contains("--- stdout ---"), "不应再用内置模板: {out}");

        // action 自己的模板优先级更高
        a.push_content = Some("ACTION-LEVEL ${status}".into());
        assert_eq!(content_for(&e, &cfg, &a), "ACTION-LEVEL 成功");

        // 全局模板留空/纯空白时回落到内置模板
        cfg.content_template = Some("   ".into());
        a.push_content = None;
        assert!(content_for(&e, &cfg, &a).contains("动作: 开灯"));
    }

    #[test]
    fn template_replaces_known_vars_and_keeps_unknown() {
        let e = sample_entry();
        let out = render_template(
            "${action}|${group}|${trigger}|${status}|${exit_code}|${duration_s}|${date}|${script_path}",
            &e,
        );
        assert_eq!(
            out,
            "开灯|路由器|定时|成功|0|1.5|2026-11-12|/opt/olivetin/led-on.sh"
        );
        // 未知变量原样保留，不静默丢内容
        assert_eq!(render_template("a${nope}b", &e), "a${nope}b");
        // $$ 转义：输出字面 $，后面的 {action} 不再被替换
        assert_eq!(render_template("$${action}", &e), "${action}");
        // 未闭合的 ${ 原样输出
        assert_eq!(render_template("x${action", &e), "x${action");
    }

    #[test]
    fn content_for_uses_custom_template_then_default() {
        let e = sample_entry();
        let cfg = push_cfg();

        let mut a = empty_action();
        // 无自定义：走内置模板，含动作名与退出码
        let def = content_for(&e, &cfg, &a);
        assert!(def.contains("动作: 开灯"));
        assert!(def.contains("退出码: 0"));

        // 自定义模板 + 输出变量
        a.push_content = Some("【${status}】${action} 用时 ${duration_s}s\n${stdout}".into());
        let custom = content_for(&e, &cfg, &a);
        assert_eq!(custom, "【成功】开灯 用时 1.5s\n绿灯已开启\n");

        // 超长内容截断到 4000 字符
        a.push_content = Some("x".repeat(5000));
        assert_eq!(content_for(&e, &cfg, &a).chars().count(), 4000);
        // 纯空白视为未设置
        a.push_content = Some("   ".into());
        assert!(content_for(&e, &cfg, &a).contains("动作: 开灯"));
    }

    #[test]
    fn summary_mode_content_takes_first_line_of_body() {
        let e = sample_entry();
        let mut cfg = push_cfg();
        let mut a = empty_action();
        let md = "### 开灯 · 成功\n\n- **时间**：2026-11-12 08:00:00\n";

        // content 模式：通知栏就是正文首行（去 Markdown 标记）
        cfg.summary_mode = "content".into();
        assert_eq!(summary_for(&e, &cfg, &a, md), "开灯 · 成功");

        // 列表 / 引用 / 强调符都能剥掉
        assert_eq!(
            summary_from_content("- 磁盘告警：剩余 62MB"),
            "磁盘告警：剩余 62MB"
        );
        assert_eq!(
            summary_from_content("> **注意** 下划线 a_b"),
            "注意 下划线 a_b"
        );
        // 空正文不能让 WxPusher 收到空 summary
        assert_eq!(summary_from_content("\n\n  \n"), "OliveTin");

        // none 模式：返回空串，调用方不传该字段
        cfg.summary_mode = "none".into();
        assert_eq!(summary_for(&e, &cfg, &a, md), "");

        // auto 保持原行为
        cfg.summary_mode = "auto".into();
        assert_eq!(summary_for(&e, &cfg, &a, md), "OliveTin 开灯 1500");

        // 脚本级 push_title 永远优先于全局模式
        a.push_title = Some("自定义标题".into());
        cfg.summary_mode = "content".into();
        assert_eq!(summary_for(&e, &cfg, &a, md), "自定义标题");
    }

    #[test]
    fn summary_for_custom_title_truncated_100() {
        let e = sample_entry();
        let cfg = push_cfg();
        let mut a = empty_action();
        // auto 模式下没有自定义标题：前缀 + 动作名 + 耗时
        assert_eq!(summary_for(&e, &cfg, &a, ""), "OliveTin 开灯 1500");

        a.push_title = Some("${action} ${status}".into());
        assert_eq!(summary_for(&e, &cfg, &a, ""), "开灯 成功");

        // 超长标题截到 100 字符（WxPusher summary 上限）
        a.push_title = Some("标".repeat(300));
        assert_eq!(summary_for(&e, &cfg, &a, "").chars().count(), 100);
    }
}
