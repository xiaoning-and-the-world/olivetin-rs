//! 参数自动发现：执行 help 命令，解析参数（含类型启发式），带缓存与指纹失效。
//!
//! 四种解析器：clap / cobra / sections / lines。名称 `[a-zA-Z0-9_-]`、≤64，
//! 单次 ≤100 个，描述截断 200 字符。失败仅 warn，手动 args 始终有效。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::config::ParamDef;

/// 已发现的参数集合。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoveredParams {
    pub params: Vec<ParamDef>,
    pub discovered_at: Option<String>,
    pub fingerprint: Option<String>,
}

/// 发现状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverState {
    pub status: String,
    pub error: Option<String>,
}

/// 计算指纹（exec[0] mtime + size）。
pub fn fingerprint(path: &str) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{}-{}", mtime, meta.len()))
}

/// 类型启发式：根据原文特征推断参数类型与样式。
pub fn infer_type(flag: &str, value_hint: Option<&str>) -> (&'static str, &'static str) {
    // bool：--verbose 且后面无 <VALUE>
    if let Some(hint) = value_hint {
        let h = hint.to_ascii_lowercase();
        if h.contains("int") || h.contains("number") || h == "n" || h == "<n>" {
            return ("int", "option");
        }
        if h.contains("string") || h.contains("value") || h.contains("path") {
            return ("string", "option");
        }
    }
    // 默认 string + option
    ("string", "option")
}

/// 运行 help 命令并解析参数。异步执行，失败返回错误字符串。
pub async fn run(
    state: &crate::state::AppState,
    action_id: &str,
) -> Result<DiscoveredParams, String> {
    let config = state.config.load();
    let action = config
        .find_action(action_id)
        .ok_or_else(|| format!("action {action_id} 不存在"))?;
    // 默认开启：action 没写 discover 字段也探测；只有显式 enabled=false 才关闭。
    // 前端因此不需要"自动读取参数"开关 —— 加完脚本就自动带出参数。
    let fallback = crate::config::DiscoverConfig {
        enabled: true,
        command: None,
        parser: "clap".to_string(),
        timeout: 5,
    };
    let disc: &crate::config::DiscoverConfig = match &action.discover {
        Some(d) if !d.enabled => return Err("discover 已显式关闭".to_string()),
        Some(d) => d,
        None => &fallback,
    };

    // 面板托管的命令型脚本（command 非空）没有独立可执行文件：
    // 探测会变成 `/bin/sh --help`，输出的是 shell 自己的用法，毫无意义。
    if action.uses_shell_script() {
        return Err("命令型脚本没有可探测的 --help".to_string());
    }

    // 解析 help 命令：probe_target + discover.command
    // exec[0] 是解释器时（/bin/sh /path/x.sh），真正探测的是脚本本身
    let (program, prefix) = probe_target(action);
    if program.is_empty() {
        return Err("没有可探测的程序".to_string());
    }
    let binary = program.clone();
    let base: Vec<String> = prefix
        .iter()
        .cloned()
        .chain(std::iter::once(program))
        .collect();
    let mut candidates: Vec<Vec<String>> = Vec::new();
    if let Some(extra) = &disc.command {
        let mut cmd = base.clone();
        cmd.extend(extra.iter().cloned());
        candidates.push(cmd);
    } else {
        // 缺省：依次尝试 --help / -h / help（不同脚本习惯不同）
        for flag in ["--help", "-h", "help"] {
            let mut cmd = base.clone();
            cmd.push(flag.to_string());
            candidates.push(cmd);
        }
    }

    let mut text = String::new();
    let mut ok = false;
    for cmd in &candidates {
        let (b, args) = (&cmd[0], &cmd[1..]);
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(disc.timeout.max(1)),
            tokio::process::Command::new(b).args(args).output(),
        )
        .await;
        match output {
            Ok(Ok(out)) => {
                let t = String::from_utf8_lossy(&out.stdout).to_string();
                if !t.trim().is_empty() {
                    text = t;
                    ok = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    if !ok {
        return Err("help/-h/--help 均无有效输出".to_string());
    }

    // 计算指纹（versioning 开启时基于 current/binary_name 解析后的文件）
    let fp = fingerprint(&binary);
    // 解析器回退：脚本的 --help 通常不是 clap 格式，
    // 依次尝试 配置的解析器 → sections → lines，取第一个解析出参数的结果。
    let mut params: Vec<crate::config::ParamDef> = Vec::new();
    let mut last_err = String::new();
    for parser in [disc.parser.as_str(), "sections", "lines"] {
        match parse_help(&text, parser) {
            Ok(p) if !p.is_empty() => {
                params = p;
                break;
            }
            Ok(_) => last_err = format!("{parser} 未解析出参数"),
            Err(e) => last_err = e,
        }
    }
    if params.is_empty() {
        return Err(format!("help 输出无法解析出参数：{last_err}"));
    }

    Ok(DiscoveredParams {
        params,
        discovered_at: Some(
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        fingerprint: fp,
    })
}

/// 解析 help 文本为参数列表。按 parser 类型区分四种解析策略。
pub fn parse_help(text: &str, parser: &str) -> Result<Vec<ParamDef>, String> {
    let params = match parser {
        "cobra" => parse_cobra(text),
        "sections" => parse_sections(text),
        "lines" => parse_lines(text),
        _ => parse_clap(text), // clap 为默认
    };
    if params.is_empty() {
        return Err("未解析到任何参数".to_string());
    }
    Ok(params)
}

/// clap 风格：`--flag <VALUE>`，每行一个选项。
fn parse_clap(text: &str) -> Vec<ParamDef> {
    let mut params: Vec<ParamDef> = Vec::new();
    for line in text.lines() {
        if params.len() >= 100 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, rest)) = extract_flag(line) else {
            continue;
        };
        push_param(&mut params, name, rest);
    }
    params
}

/// cobra 风格：`-f, --flag value`（短/长别名 + 逗号），值不带尖括号。
fn parse_cobra(text: &str) -> Vec<ParamDef> {
    let mut params: Vec<ParamDef> = Vec::new();
    for line in text.lines() {
        if params.len() >= 100 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // cobra 格式：-f, --flag string  或  --flag string
        // 优先取长选项 --flag，若只有短选项 -f 则取之
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let mut name: Option<String> = None;
        let mut rest_start = 0usize;
        for (i, tok) in tokens.iter().enumerate() {
            let t = tok.trim_end_matches(',');
            if t.starts_with("--") {
                name = Some(t.trim_start_matches('-').to_string());
                rest_start = i + 1;
                break;
            }
        }
        if name.is_none() {
            for (i, tok) in tokens.iter().enumerate() {
                let t = tok.trim_end_matches(',');
                if t.starts_with('-') && t.len() == 2 {
                    name = Some(t.trim_start_matches('-').to_string());
                    rest_start = i + 1;
                    break;
                }
            }
        }
        let Some(name) = name else { continue };
        // 值 = 剩余 token 中非选项的部分
        let rest: Vec<&str> = tokens[rest_start..]
            .iter()
            .filter(|t| !t.starts_with('-'))
            .copied()
            .collect();
        let rest_str = if rest.is_empty() {
            None
        } else {
            Some(rest.join(" "))
        };
        // cobra 值无尖括号，类型启发式按值名判断
        let value_hint = rest_str.as_deref().map(|r| r.to_string());
        push_param_with_hint(&mut params, name, rest_str, value_hint);
    }
    params
}

/// sections 风格：`Options:` / `Flags:` 等分节标题，节内每行一个参数。
fn parse_sections(text: &str) -> Vec<ParamDef> {
    let mut params: Vec<ParamDef> = Vec::new();
    for line in text.lines() {
        if params.len() >= 100 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 跳过纯标题行（不含 - 或 -- 的行）
        if !line.contains('-') {
            continue;
        }
        let Some((name, rest)) = extract_flag(line) else {
            continue;
        };
        push_param(&mut params, name, rest);
    }
    params
}

/// lines 风格：每行 `flag=value` 或 `flag` 或 `--flag value`。
fn parse_lines(text: &str) -> Vec<ParamDef> {
    let mut params: Vec<ParamDef> = Vec::new();
    for line in text.lines() {
        if params.len() >= 100 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 支持 flag=value 形式
        if let Some((k, v)) = line.split_once('=') {
            let name = k.trim().trim_start_matches('-').to_string();
            if name.is_empty() || name.chars().count() > 64 {
                continue;
            }
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                continue;
            }
            if params.iter().any(|p| p.name == name) {
                continue;
            }
            let value_hint = Some(v.trim().to_string());
            params.push(ParamDef {
                name,
                description: None,
                required: false,
                default: None,
                r#type: infer_type("", value_hint.as_deref()).0.to_string(),
                style: "option".to_string(),
                options: None,
            });
            continue;
        }
        let Some((name, rest)) = extract_flag(line) else {
            continue;
        };
        push_param(&mut params, name, rest);
    }
    params
}

/// 从一行提取 flag 名和剩余文本（clap/sections 通用）。
/// 返回 (name, rest)。
fn extract_flag(line: &str) -> Option<(String, Option<String>)> {
    for word in line.split_whitespace() {
        if word.starts_with("--") || (word.starts_with('-') && word.len() == 2) {
            let name = word.trim_start_matches('-').to_string();
            let idx = line.find(word).unwrap_or(0);
            let rest = line[idx + word.len()..].trim().to_string();
            let rest = if rest.is_empty() { None } else { Some(rest) };
            return Some((name, rest));
        }
    }
    None
}

/// 添加一个参数（clap/sections 风格，含 <VALUE> 尖括号判断）。
fn push_param(params: &mut Vec<ParamDef>, name: String, rest: Option<String>) {
    if name.is_empty() || name.chars().count() > 64 {
        return;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return;
    }
    if params.iter().any(|p| p.name == name) {
        return;
    }
    // 描述截断 200
    let description = rest
        .as_deref()
        .map(|r| {
            let mut d = r.to_string();
            if d.chars().count() > 200 {
                d = d.chars().take(200).collect();
            }
            d
        })
        .filter(|d| !d.is_empty());
    // 类型启发式：寻找 <VALUE> 之类的占位
    let value_hint = rest.as_deref().and_then(|r| {
        let start = r.find('<')?;
        let end = r[start..].find('>')? + start;
        Some(r[start..=end].to_string())
    });
    let (ptype, pstyle) = if rest.as_deref().map(|r| r.contains('<')).unwrap_or(false) {
        infer_type(&name, value_hint.as_deref())
    } else {
        ("bool", "flag")
    };
    params.push(ParamDef {
        name,
        description,
        required: false,
        default: None,
        r#type: ptype.to_string(),
        style: pstyle.to_string(),
        options: None,
    });
}

/// 添加一个参数（cobra 风格，值无尖括号）。
fn push_param_with_hint(
    params: &mut Vec<ParamDef>,
    name: String,
    rest: Option<String>,
    value_hint: Option<String>,
) {
    if name.is_empty() || name.chars().count() > 64 {
        return;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return;
    }
    if params.iter().any(|p| p.name == name) {
        return;
    }
    let description = rest
        .as_deref()
        .map(|r| {
            let mut d = r.to_string();
            if d.chars().count() > 200 {
                d = d.chars().take(200).collect();
            }
            d
        })
        .filter(|d| !d.is_empty());
    let (ptype, pstyle) = if value_hint.is_some() {
        infer_type(&name, value_hint.as_deref())
    } else {
        ("bool", "flag")
    };
    params.push(ParamDef {
        name,
        description,
        required: false,
        default: None,
        r#type: ptype.to_string(),
        style: pstyle.to_string(),
        options: None,
    });
}

/// 合并手动参数与发现参数：手动优先（同名覆盖）。
pub fn merge_params(manual: &[ParamDef], discovered: &[ParamDef]) -> Vec<ParamDef> {
    let mut map: HashMap<String, ParamDef> = HashMap::new();
    for p in discovered {
        map.insert(p.name.clone(), p.clone());
    }
    for p in manual {
        map.insert(p.name.clone(), p.clone());
    }
    let mut out: Vec<ParamDef> = map.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 解析 discover 用的二进制路径（versioning 开启时基于 current/{binary_name}）。
/// 是否是脚本解释器前缀（`/bin/sh`、`bash`、`env` 这类）。
/// 这类 action 的 exec 形如 `["/bin/sh", "/path/x.sh"]`，真正要探测的是脚本本身。
pub fn is_interpreter(p: &str) -> bool {
    matches!(
        p.rsplit('/').next().unwrap_or(""),
        "sh" | "bash" | "ash" | "dash" | "ksh" | "env"
    )
}

/// 探测目标：返回 (真正的程序路径, 前缀参数)。
/// 例：`["/bin/sh", "/opt/x/run.sh"]` → (`/opt/x/run.sh`, `["/bin/sh"]`)
fn probe_target(action: &crate::config::Action) -> (String, Vec<String>) {
    let exec = &action.exec;
    if exec.is_empty() {
        return (String::new(), Vec::new());
    }
    if is_interpreter(&exec[0]) && exec.len() > 1 {
        return (exec[1].clone(), vec![exec[0].clone()]);
    }
    (exec[0].clone(), Vec::new())
}

fn resolve_binary_path(action: &crate::config::Action) -> String {
    if let Some(v) = &action.versioning {
        if v.enabled {
            let base = std::path::Path::new(&v.base_dir);
            if let Ok(active) = crate::versioning::read_current_link(base) {
                let p = base.join(&active).join(&v.binary_name);
                return p.to_string_lossy().to_string();
            }
        }
    }
    probe_target(action).0
}

/// 对 Action 执行发现，并更新缓存。指纹未变时跳过重新发现（复用缓存）。
pub async fn run_and_cache(state: &crate::state::AppState, action_id: &str) {
    // 指纹失效判断：指纹未变且已有缓存 → 跳过
    {
        let config = state.config.load();
        if let Some(action) = config.find_action(action_id) {
            let binary = resolve_binary_path(action);
            let current_fp = fingerprint(&binary);
            let cached = state.discovered.load();
            if let Some(existing) = cached.get(action_id) {
                if current_fp.is_some()
                    && existing.fingerprint.is_some()
                    && current_fp == existing.fingerprint
                {
                    // 指纹未变，复用缓存
                    return;
                }
            }
        }
    }

    // 命令型脚本必然探测失败，静默跳过，别在启动时刷一屏 warn
    {
        let config = state.config.load();
        if let Some(action) = config.find_action(action_id) {
            if action.uses_shell_script() {
                return;
            }
        }
    }

    match run(state, action_id).await {
        Ok(params) => {
            // 用 rcu 做无丢失更新：load→clone→store 存在「并发发现后写覆盖先写」的
            // 丢失更新竞态；rcu 基于当前值原子重算，避免覆盖并发写入的其它 action。
            state.discovered.rcu(|current| {
                let mut map = current.as_ref().clone();
                map.insert(action_id.to_string(), params.clone());
                Arc::new(map)
            });
        }
        Err(e) => {
            tracing::warn!(action = %action_id, error = %e, "参数发现失败");
        }
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    fn action(exec: Vec<&str>) -> crate::config::Action {
        serde_json::from_value(serde_json::json!({
            "id": "a", "title": "a", "exec": exec
        }))
        .unwrap()
    }

    #[test]
    fn unwraps_interpreter_prefix() {
        let a = action(vec!["/bin/sh", "/opt/x/run.sh"]);
        let (prog, prefix) = probe_target(&a);
        assert_eq!(prog, "/opt/x/run.sh");
        assert_eq!(prefix, vec!["/bin/sh".to_string()]);

        // bash / env 同样识别
        let b = action(vec!["bash", "/opt/y.sh"]);
        assert_eq!(probe_target(&b).0, "/opt/y.sh");

        // 直接执行的二进制不剥
        let c = action(vec!["/opt/z/wb", "--flag"]);
        let (p3, pre3) = probe_target(&c);
        assert_eq!(p3, "/opt/z/wb");
        assert!(pre3.is_empty());
    }

    #[test]
    fn is_interpreter_matches_basename() {
        assert!(is_interpreter("/bin/sh"));
        assert!(is_interpreter("/usr/bin/env"));
        assert!(!is_interpreter("/opt/sh-wrapper"));
        assert!(!is_interpreter("/opt/busybox"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_types() {
        assert_eq!(infer_type("port", Some("<int>")), ("int", "option"));
        assert_eq!(infer_type("name", Some("<string>")), ("string", "option"));
        assert_eq!(infer_type("verbose", None), ("string", "option"));
    }

    #[test]
    fn parse_clap_help() {
        let text = "\
Usage: app [OPTIONS]

Options:
  --port <int>    listen port
  --verbose       enable verbose
  --name <string> name to use
";
        let params = parse_help(text, "clap").unwrap();
        assert!(!params.is_empty());
        assert!(params.iter().any(|p| p.name == "port" && p.r#type == "int"));
        assert!(params.iter().any(|p| p.name == "verbose" && p.r#type == "bool"));
    }

    #[test]
    fn merge_manual_wins() {
        let manual = vec![ParamDef {
            name: "port".into(),
            description: Some("manual".into()),
            required: true,
            default: Some("8080".into()),
            r#type: "int".into(),
            style: "option".into(),
            options: None,
        }];
        let discovered = vec![ParamDef {
            name: "port".into(),
            description: Some("discovered".into()),
            required: false,
            default: None,
            r#type: "int".into(),
            style: "option".into(),
            options: None,
        }];
        let merged = merge_params(&manual, &discovered);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].description.as_deref(), Some("manual"));
        assert_eq!(merged[0].required, true);
    }
}
