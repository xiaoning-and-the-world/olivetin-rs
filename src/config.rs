//! 配置模型与校验。
//!
//! 包含 Config / Action / VersioningConfig / UploadConfig / PushConfig / ParamDef /
//! AuthConfig / ScheduleConfig 结构定义，以及加载、环境变量插值、校验逻辑。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// 顶层配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "default_log_capacity")]
    pub log_capacity: usize,
    #[serde(default)]
    pub log_directory: Option<String>,
    /// 面板生成的 sh 脚本存放目录；未配置时用 DEFAULT_SCRIPT_DIRECTORY。
    #[serde(default)]
    pub script_directory: Option<String>,
    #[serde(default = "default_retention_days")]
    pub log_retention_days: Option<u32>,
    #[serde(default = "default_log_max_total_mb")]
    pub log_max_total_mb: u64,
    /// 版本管理总开关：关掉后所有 action 的版本管理都不生效（按普通 exec 执行），
    /// 面板内的版本上传入口也会被拒绝。单个 action 仍可用自己的 versioning 开关细化。
    #[serde(default = "default_true")]
    pub versioning_enabled: bool,
    #[serde(default = "default_min_free_mb")]
    pub min_free_mb: u64,
    #[serde(default = "default_max_output_kb")]
    pub max_output_kb: usize,
    #[serde(default = "default_timezone")]
    pub default_timezone: String,
    #[serde(default = "default_true")]
    pub inject_tz: bool,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub push: PushConfig,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// 可回退版本数量上限（默认 5，必须 ≥1；不提供「无限保存」选项）。
    #[serde(default = "default_max_versions")]
    pub max_versions: u32,
    /// 分组显示顺序；未列出的分组按名称排在后面。
    #[serde(default)]
    pub group_order: Vec<String>,
    /// 分组列表所在位置：top / left / right。
    #[serde(default = "default_group_position")]
    pub group_panel_position: String,
    /// 分组内脚本卡片排列：vertical（纵向）/ horizontal（横向）。
    #[serde(default = "default_group_layout")]
    pub group_layout: String,
    /// 首次打开动作页时是否默认收起分组列表。
    #[serde(default)]
    pub collapse_groups_by_default: bool,
}

fn default_max_versions() -> u32 {
    5
}
fn default_group_position() -> String {
    "top".to_string()
}
fn default_group_layout() -> String {
    "vertical".to_string()
}

/// 面板生成 sh 脚本的默认目录（script_directory 未配置时使用）。
pub const DEFAULT_SCRIPT_DIRECTORY: &str = "/opt/olivetin/scripts";

/// 返回实际生效的脚本目录：配置值非空则用它，否则用默认目录。
pub fn script_directory(configured: Option<&str>) -> &str {
    match configured.map(str::trim) {
        Some(s) if !s.is_empty() => s,
        _ => DEFAULT_SCRIPT_DIRECTORY,
    }
}

fn default_max_concurrent() -> usize {
    4
}
fn default_log_capacity() -> usize {
    200
}
fn default_retention_days() -> Option<u32> {
    Some(7)
}
fn default_log_max_total_mb() -> u64 {
    20
}
fn default_min_free_mb() -> u64 {
    50
}
fn default_max_output_kb() -> usize {
    64
}
fn default_timezone() -> String {
    "+08:00".to_string()
}
fn default_true() -> bool {
    true
}

/// 全局速率限制。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_per_minute")]
    pub per_minute: u32,
}
fn default_per_minute() -> u32 {
    30
}

/// 认证配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token_file: Option<String>,
    #[serde(default = "default_cookie_ttl")]
    pub cookie_ttl_hours: u32,
    #[serde(default)]
    pub trust_proxy: bool,
}
fn default_cookie_ttl() -> u32 {
    168
}

/// WxPusher 推送配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub spt: Option<String>,
    #[serde(default)]
    pub app_token: Option<String>,
    #[serde(default)]
    pub uids: Option<Vec<String>>,
    #[serde(default)]
    pub topic_ids: Option<Vec<u32>>,
    #[serde(default = "default_content_type")]
    pub content_type: u32,
    #[serde(default = "default_summary_prefix")]
    pub summary_prefix: String,
    /// 通知栏那行字（WxPusher 的 summary）怎么来：
    /// - `auto`（默认）：`{summary_prefix} {动作名} {耗时ms}`
    /// - `content`：取正文第一行有效文字（去掉 Markdown 标记），即「不单独起标题，直接正文」
    /// - `none`：不传 summary，完全交给 WxPusher 自行截取正文
    #[serde(default = "default_summary_mode")]
    pub summary_mode: String,
    /// 全局默认推送正文模板（支持 `${变量}`）。
    /// 配了它，所有未单独设置 `push_content` 的 action 都用这份模板，
    /// 典型用法：配合 `content_type: 3` 让全部通知都走 Markdown 排版。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_template: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_true")]
    pub include_output: bool,
    #[serde(default = "default_shutdown_wait")]
    pub shutdown_wait_ms: u64,
}
/// 手写 Default：`#[serde(default)]` 引用整个结构体时走的是这里，
/// 字段级 `#[serde(default = "...")]` 不会生效。derive(Default) 会让
/// content_type=0、summary_mode="" 这类非法默认值漏进来（validate 会拒，
/// 表现为「配置里省略 push 段就无法启动」）。
impl Default for PushConfig {
    fn default() -> Self {
        PushConfig {
            enabled: false,
            spt: None,
            app_token: None,
            uids: None,
            topic_ids: None,
            content_type: default_content_type(),
            summary_prefix: default_summary_prefix(),
            summary_mode: default_summary_mode(),
            content_template: None,
            timeout: default_timeout(),
            include_output: true,
            shutdown_wait_ms: default_shutdown_wait(),
        }
    }
}

fn default_content_type() -> u32 {
    1
}
fn default_summary_mode() -> String {
    "auto".to_string()
}
fn default_summary_prefix() -> String {
    "OliveTin".to_string()
}
fn default_timeout() -> u64 {
    10
}
fn default_shutdown_wait() -> u64 {
    3000
}

/// 把一段文本压成一行短描述（多行取首行，超长截断）。
fn one_line(s: &str, max: usize) -> String {
    let first = s.split('\n').next().unwrap_or("").trim();
    let t = if s.contains('\n') {
        format!("{first} …")
    } else {
        first.to_string()
    };
    if t.chars().count() > max {
        let mut out: String = t.chars().take(max).collect();
        out.push('…');
        out
    } else {
        t
    }
}

fn yn(v: bool) -> &'static str {
    if v { "开" } else { "关" }
}

/// 定时配置的一句话描述，用于变更日志（「每天 08:00」「每周 1,3 08:00」）。
fn schedule_summary(s: &ScheduleConfig) -> String {
    if !s.enabled {
        return "未启用".to_string();
    }
    match s.mode.as_str() {
        "daily" => format!("每天 {}", s.time.as_deref().unwrap_or("--:--")),
        "weekly" => {
            let days: Vec<String> = s
                .weekdays
                .as_ref()
                .map(|v| v.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();
            format!(
                "每周 {} {}",
                if days.is_empty() { "—".to_string() } else { days.join("、") },
                s.time.as_deref().unwrap_or("--:--")
            )
        }
        "monthly" => {
            let days: Vec<String> = s
                .days
                .as_ref()
                .map(|v| v.iter().map(|d| d.to_string()).collect())
                .unwrap_or_default();
            format!(
                "每月 {} 日 {}",
                if days.is_empty() { "—".to_string() } else { days.join("、") },
                s.time.as_deref().unwrap_or("--:--")
            )
        }
        "cron" => format!("cron {}", s.cron.as_deref().unwrap_or("")),
        other => other.to_string(),
    }
}

/// 命令内容差异：指出第几行变了 / 行数变化 / 新增或清空。
fn describe_command(old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    let o = old.trim();
    let n = new.trim();
    if o.is_empty() && !n.is_empty() {
        return Some(format!("命令已填写（{} 行）", n.lines().count()));
    }
    if !o.is_empty() && n.is_empty() {
        return Some("命令已清空".to_string());
    }
    let a: Vec<&str> = o.lines().collect();
    let b: Vec<&str> = n.lines().collect();
    if let Some(i) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
        return Some(format!(
            "命令第 {} 行已修改（{} 行→{} 行）",
            i + 1,
            a.len(),
            b.len()
        ));
    }
    if a.len() != b.len() {
        return Some(format!("命令行数 {}→{}", a.len(), b.len()));
    }
    Some("命令内容已修改".to_string())
}

/// 生成「编辑了什么」的一句话描述，写进操作日志。
///
/// 日志里只写「已编辑脚本「X」」等于没写：事后想知道改了什么只能翻配置历史。
/// 这里逐字段对比出具体变更（超时 30→180、推送条件 always→on_failure、命令第 3 行已修改…），
/// 单条最多 300 字符，超出截断。
pub fn describe_action_changes(old: Option<&Action>, new: &Action) -> String {
    let Some(old) = old else {
        return "新增条目".to_string();
    };
    let mut parts: Vec<String> = Vec::new();

    if old.title != new.title {
        parts.push(format!("标题「{}」→「{}」", old.title, new.title));
    }
    if old.group != new.group {
        parts.push(format!("分组 {}→{}", old.group, new.group));
    }
    if old.enabled != new.enabled {
        parts.push(format!("启用状态 {}→{}", yn(old.enabled), yn(new.enabled)));
    }
    if old.script_name != new.script_name {
        parts.push(format!(
            "脚本文件名 {}→{}",
            old.script_name.as_deref().unwrap_or("默认"),
            new.script_name.as_deref().unwrap_or("默认")
        ));
    }
    if old.script_full_path != new.script_full_path {
        parts.push(format!(
            "脚本保存路径 {}→{}",
            old.script_full_path.as_deref().unwrap_or("默认目录"),
            new.script_full_path.as_deref().unwrap_or("默认目录")
        ));
    }
    // 命令型 action 保存时后端会自动生成 exec=["/bin/sh", 脚本路径]，
    // 那不是用户改的「执行命令」，报出来只会让日志噪音化。
    let exec_auto_generated = old.exec.is_empty()
        && new
            .command
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
        && matches!(new.exec.first().map(String::as_str), Some("/bin/sh") | Some("/bin/bash"));
    if !exec_auto_generated && old.exec != new.exec {
        let joined = new.exec.join(" ");
        parts.push(format!("执行命令已改为 {}", one_line(&joined, 60)));
    }
    if let Some(d) = describe_command(
        old.command.as_deref().unwrap_or(""),
        new.command.as_deref().unwrap_or(""),
    ) {
        parts.push(d);
    }
    if old.timeout != new.timeout {
        parts.push(format!("超时 {}→{} 秒", old.timeout, new.timeout));
    }
    if old.concurrency != new.concurrency {
        parts.push(format!("并发 {}→{}", old.concurrency, new.concurrency));
    }
    if old.push != new.push {
        parts.push(format!("推送 {}→{}", yn(old.push), yn(new.push)));
    }
    if old.push_condition != new.push_condition {
        parts.push(format!(
            "推送条件 {}→{}",
            old.push_condition, new.push_condition
        ));
    }
    if old.push_content_type != new.push_content_type {
        fn ct(v: Option<u32>) -> String {
            match v {
                None => "跟随全局".to_string(),
                Some(1) => "纯文本".to_string(),
                Some(2) => "HTML".to_string(),
                Some(3) => "Markdown".to_string(),
                Some(x) => format!("{x}"),
            }
        }
        parts.push(format!(
            "推送内容类型 {}→{}",
            ct(old.push_content_type),
            ct(new.push_content_type)
        ));
    }
    if old.push_title != new.push_title {
        fn t(v: &Option<String>) -> String {
            match v.as_deref() {
                None | Some("") => "默认".to_string(),
                Some(s) => one_line(s, 24),
            }
        }
        parts.push(format!(
            "推送标题 {}→{}",
            t(&old.push_title),
            t(&new.push_title)
        ));
    }
    if old.push_content != new.push_content {
        parts.push("推送正文模板已修改".to_string());
    }
    if old.push_uids != new.push_uids {
        parts.push("推送接收者 UID 已修改".to_string());
    }
    if old.push_topic_ids != new.push_topic_ids {
        parts.push("推送主题 ID 已修改".to_string());
    }
    if old.description != new.description {
        parts.push("描述已修改".to_string());
    }
    match (&old.schedule, &new.schedule) {
        (None, Some(s)) => parts.push(format!("新增定时：{}", schedule_summary(s))),
        (Some(s), None) => parts.push(format!("移除定时（原 {}）", schedule_summary(s))),
        (Some(a), Some(b)) => {
            if a.enabled != b.enabled
                || a.mode != b.mode
                || a.time != b.time
                || a.weekdays != b.weekdays
                || a.days != b.days
                || a.cron != b.cron
            {
                parts.push(format!(
                    "定时 {}→{}",
                    schedule_summary(a),
                    schedule_summary(b)
                ));
            }
        }
        (None, None) => {}
    }
    if old.args.len() != new.args.len() {
        parts.push(format!("参数 {}→{} 个", old.args.len(), new.args.len()));
    } else if old.args != new.args {
        parts.push("参数定义已修改".to_string());
    }
    if old.env != new.env {
        let added: Vec<&str> = new
            .env
            .keys()
            .filter(|k| !old.env.contains_key(*k))
            .map(String::as_str)
            .collect();
        let removed: Vec<&str> = old
            .env
            .keys()
            .filter(|k| !new.env.contains_key(*k))
            .map(String::as_str)
            .collect();
        if added.is_empty() && removed.is_empty() {
            parts.push("环境变量值已修改".to_string());
        } else {
            let mut d = String::from("环境变量");
            if !added.is_empty() {
                d.push_str(&format!(" +{}", added.join(",")));
            }
            if !removed.is_empty() {
                d.push_str(&format!(" -{}", removed.join(",")));
            }
            parts.push(d);
        }
    }
    if old.versioning.is_some() != new.versioning.is_some() {
        parts.push(if new.versioning.is_some() {
            "启用版本管理".to_string()
        } else {
            "关闭版本管理".to_string()
        });
    }
    if old.discover.is_some() != new.discover.is_some() {
        parts.push("参数发现配置已修改".to_string());
    }

    if parts.is_empty() {
        return "无字段变更".to_string();
    }
    let joined = parts.join("，");
    if joined.chars().count() > 300 {
        let mut out: String = joined.chars().take(300).collect();
        out.push('…');
        out
    } else {
        joined
    }
}

/// 单个脚本 Action。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub title: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_group")]
    pub group: String,
    #[serde(default)]
    pub exec: Vec<String>,
    /// shell 命令原文：由面板写入 .sh 脚本后交给 /bin/sh 执行。
    /// 为空（旧配置）时仍按 exec 数组直接执行，不经过脚本文件。
    #[serde(default)]
    pub command: Option<String>,
    /// 生成的 sh 脚本文件名；留空时按 `{id}.sh` 规则生成。
    #[serde(default)]
    pub script_name: Option<String>,
    /// 脚本完整保存路径（可选，绝对路径）。填写后直接使用该路径，
    /// 不再按 `{script_directory}/{script_filename}` 拼接；未填 `.sh` 时自动补。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_full_path: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub push: bool,
    /// 推送条件：always（总是）/ on_failure（失败时）/ on_success（成功时）/ on_marker（输出含 [PUSH]）。
    /// 仅 push=true 时生效。
    #[serde(default = "default_push_condition")]
    pub push_condition: String,
    /// 自定义推送标题（WxPusher 的 summary，通知栏标题），≤100 字符。
    /// 支持 `${变量}` 模板，变量见 push.rs 的 render_template。为空用 `{summary_prefix} {动作名} {耗时}`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_title: Option<String>,
    /// 自定义推送正文。支持 `${变量}` 模板；为空用内置模板（动作/版本/触发/退出码/耗时 + 可选输出）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_content: Option<String>,
    /// 覆盖全局接收者 UID 列表（仅应用模式 app_token 生效）。为空则用全局 push.uids。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_uids: Option<Vec<String>>,
    /// 覆盖全局主题 ID 列表（仅应用模式）。为空则用全局 push.topic_ids。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_topic_ids: Option<Vec<u32>>,
    /// 覆盖全局 contentType：1 纯文本 / 2 HTML / 3 Markdown。为空则用全局 push.content_type。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_content_type: Option<u32>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub versioning: Option<VersioningConfig>,
    #[serde(default)]
    pub schedule: Option<ScheduleConfig>,
    #[serde(default)]
    pub args: Vec<ParamDef>,
    #[serde(default)]
    pub discover: Option<DiscoverConfig>,
    /// 预设子命令：如 `["--check", "--force"]`，执行时选中的项作为第一个参数传给脚本。
    ///
    /// 用 `Option` 区分三种状态：
    /// - `None`：从未配置 → 下拉由 `--help` 自动解析生成；
    /// - `Some([...])`：用户显式配置 → **完全以用户为准**（删掉的项不会再冒出来）；
    /// - `Some([])`：用户清空了 → 下拉里只剩「默认（不带参数）」。
    ///
    /// 之前用 `Vec` + `skip_serializing_if(empty)`，空数组不写盘，导致
    /// 「用户删除子命令」与「从未配置」无法区分，删了还会被自动解析补回来。
    #[serde(default)]
    pub subcommands: Option<Vec<String>>,
}
impl Action {
    /// sh 脚本文件名规则（唯一入口，前后端一致）：
    /// 1. `script_name` 留空 → `{action.id}.sh`；
    /// 2. 已指定 → 去掉首尾空白，不以 `.sh` 结尾时自动补 `.sh`；
    /// 3. 只允许字母/数字/`.`/`_`/`-`（由 validate 用 is_valid_name 校验），
    ///    禁止 `/`、`..`、空格，避免路径穿越。
    pub fn script_filename(&self) -> String {
        let raw = self.script_name.as_deref().unwrap_or("").trim();
        if raw.is_empty() {
            format!("{}.sh", self.id)
        } else if raw.ends_with(".sh") {
            raw.to_string()
        } else {
            format!("{raw}.sh")
        }
    }

    /// sh 脚本完整路径。
    /// 1. 配了 `script_full_path`（绝对路径）→ 直接用它，未以 `.sh` 结尾则补 `.sh`；
    /// 2. 否则 `{script_directory}/{script_filename}`。
    pub fn script_path(&self, configured_dir: Option<&str>) -> std::path::PathBuf {
        if let Some(custom) = self.custom_script_path() {
            return std::path::PathBuf::from(custom);
        }
        std::path::Path::new(script_directory(configured_dir)).join(self.script_filename())
    }

    /// 规范化后的自定义完整路径：去空白、补 `.sh`；未配置返回 None。
    pub fn custom_script_path(&self) -> Option<String> {
        let raw = self.script_full_path.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        // 以 `/` 结尾视为「只指定目录」，文件名沿用 script_name / {id}.sh：
        // 否则 `/opt/` 会被拼成 `/opt/.sh`（用户填目录是很自然的写法）。
        if raw.ends_with('/') {
            return Some(format!("{}{}", raw, self.script_filename()));
        }
        Some(if raw.ends_with(".sh") {
            raw.to_string()
        } else {
            format!("{raw}.sh")
        })
    }

    /// 是否走 sh 脚本流程：命令原文非空且该 action 未启用版本管理。
    pub fn uses_shell_script(&self) -> bool {
        self.command
            .as_deref()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
            && !self
                .versioning
                .as_ref()
                .map(|v| v.enabled)
                .unwrap_or(false)
    }

    /// 「引用型」脚本路径：脚本文件已经在服务器上，面板只调用、不接管内容。
    /// 识别两种写法：
    /// - `/bin/sh /path/x.sh`（或 /bin/bash）→ `/path/x.sh`
    /// - exec 只有一个绝对路径（旧式直接执行，如 `/opt/olivetin/led-on.sh`）→ 该路径
    /// command 型（面板托管）、versioning、exec 为空的一律返回 None。
    pub fn script_ref(&self) -> Option<String> {
        if self.uses_shell_script() {
            return None;
        }
        if self.versioning.as_ref().map(|v| v.enabled).unwrap_or(false) {
            return None;
        }
        match self.exec.as_slice() {
            [one] => {
                let lower = one.to_ascii_lowercase();
                if lower.ends_with(".sh") || lower.ends_with(".bash") {
                    Some(one.clone())
                } else {
                    None
                }
            }
            [interp, script, ..] if interp.as_str() == "/bin/sh" || interp.as_str() == "/bin/bash" => {
                Some(script.clone())
            }
            _ => None,
        }
    }

    /// 脚本内容：保证有 shebang + 统一 LF + 结尾换行。
    /// 参数由 `/bin/sh <脚本> [args...]` 传入，命令里可用 `"$@"` 或 `$0` /
    /// `$OLIVETIN_SCRIPT_PATH` 取得自身路径。
    ///
    /// 从 Windows 导入的 .sh 常带 BOM 与 CRLF，直接落盘会让 busybox ash 报
    /// `not found`（BOM 被当成命令首字符）；这里一并清理。
    /// 内容自带的 shebang（如 `#!/bin/bash`）保留，不再重复加 `#!/bin/sh`。
    pub fn script_content(&self) -> Option<String> {
        let raw = self.command.as_deref()?;
        if raw.trim().is_empty() {
            return None;
        }
        let mut body = raw
            .trim_start_matches('\u{feff}')
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        if !body.trim_start().starts_with("#!") {
            body = format!("#!/bin/sh\n{}", body.trim_start());
        }
        if !body.ends_with('\n') {
            body.push('\n');
        }
        Some(body)
    }
}

fn default_group() -> String {
    "默认".to_string()
}
fn default_push_condition() -> String {
    "always".to_string()
}
fn default_concurrency() -> usize {
    1
}

/// 目录级版本管理配置。
///
/// 每个版本目录 `{base_dir}/{version}/` 自带「二进制 + 一整套配置文件」，
/// `current` 软链接指向当前版本；切换 current 即切换整套配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersioningConfig {
    pub enabled: bool,
    pub base_dir: String,
    pub binary_name: String,
    /// 旧版单配置文件名，保留以兼容已有配置。
    #[serde(default)]
    pub config_name: Option<String>,
    /// 该版本管理下的全部配置文件名（可任意增删账号配置）。
    #[serde(default)]
    pub config_files: Vec<String>,
    #[serde(default)]
    pub upload: UploadConfig,
}

impl VersioningConfig {
    /// 合并旧的单配置字段与新列表，去重且保持声明顺序。
    pub fn all_config_files(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(cn) = &self.config_name {
            if !cn.is_empty() {
                out.push(cn.clone());
            }
        }
        for f in &self.config_files {
            if !f.is_empty() && !out.contains(f) {
                out.push(f.clone());
            }
        }
        out
    }
}

/// 面板内上传子配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UploadConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_binary_mb")]
    pub max_binary_mb: u64,
    #[serde(default = "default_max_config_kb")]
    pub max_config_kb: u64,
}
fn default_max_binary_mb() -> u64 {
    50
}
fn default_max_config_kb() -> u64 {
    256
}

/// 定时调度配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub enabled: bool,
    pub mode: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub skip_if_running: bool,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub weekdays: Option<Vec<u8>>,
    #[serde(default)]
    pub days: Option<Vec<u8>>,
    #[serde(default)]
    pub cron: Option<String>,
}

/// 手动参数定义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default = "default_param_type")]
    pub r#type: String,
    #[serde(default = "default_style")]
    pub style: String,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}
fn default_param_type() -> String {
    "string".to_string()
}
fn default_style() -> String {
    "option".to_string()
}

/// 参数自动发现配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscoverConfig {
    /// 默认开启：显式写 `enabled: false` 才关闭
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default = "default_parser")]
    pub parser: String,
    #[serde(default = "default_discover_timeout")]
    pub timeout: u64,
}
fn default_parser() -> String {
    "clap".to_string()
}
fn default_discover_timeout() -> u64 {
    5
}

/// 危险环境变量 key，action.env 中禁止注入。
/// 覆盖解释器劫持（LD_*、DYLD_*）与外部命令查找劫持（PATH、BASH_ENV、ENV、IFS、
/// CDPATH）以及各语言运行时注入（NODE_OPTIONS、PYTHONPATH、RUBYOPT 等）。
/// 这些变量若由 API 提交，可在 root 下劫持脚本解释器与命令解析（CWE-426/CWE-78）。
const FORBIDDEN_ENV_KEYS: [&str; 18] = [
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "CDPATH",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONHOME",
    "PERL5LIB",
    "PERLLIB",
    "RUBYOPT",
    "RUBYLIB",
    "GEM_PATH",
];

/// 合法版本名 / binary_name / config_name 正则（字符集校验）。
pub fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// 合法 action id / 参数名。
pub fn is_valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 对字符串做 `${VAR}` 环境变量插值，`$$` 转义为字面 `$`。
/// 引用不存在的环境变量返回错误。
pub fn interpolate(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            if chars.peek() == Some(&'$') {
                chars.next();
                out.push('$');
            } else if chars.peek() == Some(&'{') {
                chars.next();
                let mut name = String::new();
                for c2 in chars.by_ref() {
                    if c2 == '}' {
                        break;
                    }
                    name.push(c2);
                }
                match std::env::var(&name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => return Err(format!("环境变量 {name} 未定义")),
                }
            } else {
                out.push('$');
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

impl Config {
    /// 对全量配置的字符串字段做插值展开。
    /// 在 YAML 解析后、OLIVETIN_* 覆盖前调用。
    pub fn apply_interpolation(&mut self) -> Result<(), String> {
        if let Some(d) = &mut self.log_directory {
            *d = interpolate(d)?;
        }
        if let Some(d) = &mut self.script_directory {
            *d = interpolate(d)?;
        }
        // action.command 是 shell 原文，故意不做插值：${VAR} 属于 shell 语义，
        // 若在这里展开会破坏脚本行为（命令里的变量应交给 /bin/sh 处理）。
        self.default_timezone = interpolate(&self.default_timezone)?;
        if let Some(tf) = &mut self.auth.token_file {
            *tf = interpolate(tf)?;
        }
        if let Some(spt) = &mut self.push.spt {
            *spt = interpolate(spt)?;
        }
        if let Some(at) = &mut self.push.app_token {
            *at = interpolate(at)?;
        }
        for action in &mut self.actions {
            for arg in &mut action.exec {
                *arg = interpolate(arg)?;
            }
            if let Some(d) = &mut action.description {
                *d = interpolate(d)?;
            }
            for v in action.env.values_mut() {
                *v = interpolate(v)?;
            }
            if let Some(v) = &mut action.versioning {
                v.base_dir = interpolate(&v.base_dir)?;
                v.binary_name = interpolate(&v.binary_name)?;
                if let Some(cn) = &mut v.config_name {
                    *cn = interpolate(cn)?;
                }
            }
            for p in &mut action.args {
                if let Some(d) = &mut p.description {
                    *d = interpolate(d)?;
                }
                if let Some(d) = &mut p.default {
                    *d = interpolate(d)?;
                }
            }
        }
        Ok(())
    }

    /// 应用 OLIVETIN_* 环境变量覆盖（优先级高于配置文件）。不含 Token。
    pub fn apply_env_overrides(&mut self) {
        macro_rules! env_str {
            ($name:expr) => {
                std::env::var($name).ok()
            };
        }
        if let Some(v) = env_str!("OLIVETIN_PUSH_SPT") {
            self.push.spt = Some(v);
        }
        if let Some(v) = env_str!("OLIVETIN_PUSH_APP_TOKEN") {
            self.push.app_token = Some(v);
        }
        if let Some(v) = env_str!("OLIVETIN_PUSH_UIDS") {
            self.push.uids = Some(v.split(',').map(|s| s.trim().to_string()).collect());
        }
        if let Some(v) = env_str!("OLIVETIN_PUSH_TOPIC_IDS") {
            // 不再静默丢弃非法项：解析失败时告警，便于发现「幽灵配置」。
            let mut ids = Vec::new();
            for item in v.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                match item.parse::<u32>() {
                    Ok(id) => ids.push(id),
                    Err(_) => {
                        tracing::warn!(item = %item, "OLIVETIN_PUSH_TOPIC_IDS 含非法项，已忽略");
                    }
                }
            }
            self.push.topic_ids = Some(ids);
        }
        if let Some(v) = env_str!("OLIVETIN_LOG_DIR") {
            self.log_directory = Some(v);
        }
        if let Some(v) = env_str!("OLIVETIN_DEFAULT_TIMEZONE") {
            self.default_timezone = v;
        }
        if let Some(v) = env_str!("OLIVETIN_SCRIPT_DIR") {
            self.script_directory = Some(v);
        }
    }

    /// 校验配置，返回错误信息（第一个错误）。
    pub fn validate(&self) -> Result<(), String> {
        // config_version
        if let Some(v) = self.config_version {
            if v != 1 {
                return Err(format!("config_version 必须为 1，当前为 {v}"));
            }
        }
        // actions 非空
        if self.actions.is_empty() {
            return Err("actions 不能为空".to_string());
        }
        // 正整数校验
        if self.max_concurrent == 0 {
            return Err("max_concurrent 必须为正整数".to_string());
        }
        if self.max_output_kb == 0 {
            return Err("max_output_kb 必须为正整数".to_string());
        }
        if self.log_capacity == 0 {
            return Err("log_capacity 必须为正整数".to_string());
        }
        // 速率限制：enabled=true 时 per_minute 必须 ≥1，否则令牌桶容量为 0，
        // 热加载后所有写请求恒 429（之前配 0 会静默导致写接口全被限流）。
        if self.rate_limit.enabled && self.rate_limit.per_minute == 0 {
            return Err("rate_limit.per_minute 必须为正整数（enabled=true 时）".to_string());
        }
        // 日志内存上限：阈值需容纳默认值 200×64×1024 = 12.5 MB，
        // 否则「按文档省略这两个字段」的配置会在启动时被拒。
        // 用 checked_mul 避免 release 下算术回绕绕过 16MB 上限（CWE-190）。
        let mem = match (self.log_capacity as u64)
            .checked_mul(self.max_output_kb as u64)
            .and_then(|v| v.checked_mul(1024))
        {
            Some(v) => v,
            None => {
                return Err("log_capacity × max_output_kb × 1024 溢出".to_string());
            }
        };
        if mem > 16 * 1024 * 1024 {
            return Err(format!(
                "log_capacity × max_output_kb × 1024 超过 16 MB（当前 {mem} 字节）"
            ));
        }
        // 全局推送接收者校验：与 action 级 push_uids 同口径（之前全局无校验，强度不一致）
        if let Some(uids) = &self.push.uids {
            for u in uids {
                let u = u.trim();
                if u.is_empty() || u.len() > 64 {
                    return Err(format!("push.uids 含空值或超长项: {u}"));
                }
                if !u
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    return Err(format!("push.uids 含非法字符: {u}"));
                }
            }
        }
        // 全局推送 content_type 上界：仅 1(文本)/2(HTML)/3(Markdown) 合法
        if self.push.content_type != 0 && !(1..=3).contains(&self.push.content_type) {
            return Err(format!(
                "push.content_type 只能是 1(文本)/2(HTML)/3(Markdown)，当前 {}",
                self.push.content_type
            ));
        }
        // 通知栏摘要来源
        if !matches!(
            self.push.summary_mode.as_str(),
            "auto" | "content" | "none"
        ) {
            return Err(format!(
                "push.summary_mode 只能是 auto / content / none（当前 {}）",
                self.push.summary_mode
            ));
        }
        // 全局推送正文模板：与发送侧的 4000 字符上限对齐
        if let Some(tpl) = &self.push.content_template {
            if tpl.chars().count() > 4000 {
                return Err(format!(
                    "push.content_template 超长（≤4000 字符，当前 {}）",
                    tpl.chars().count()
                ));
            }
        }
        // sh 脚本目录必须为绝对路径（生成的脚本要被 /bin/sh 直接调用）
        let script_dir = script_directory(self.script_directory.as_deref());
        if !Path::new(script_dir).is_absolute() {
            return Err(format!("script_directory 必须为绝对路径: {script_dir}"));
        }
        // 时区：必须在加载期可解析，否则运行时 4 处 unwrap_or_else 静默回退 +08:00，
        // 导致配置写错时区不报错、定时任务/日志时区错 8 小时。
        if crate::time_util::parse_timezone(&self.default_timezone).is_err() {
            return Err(format!(
                "default_timezone 无法解析: {}",
                self.default_timezone
            ));
        }
        // 认证校验
        if self.auth.enabled {
            let tf = self
                .auth
                .token_file
                .as_ref()
                .ok_or("auth.enabled = true 时 token_file 必须配置")?;
            if !Path::new(tf).is_absolute() {
                return Err("auth.token_file 必须为绝对路径".to_string());
            }
            if let Some(parent) = Path::new(tf).parent() {
                if !parent.exists() || !parent.is_dir() {
                    return Err(format!("token_file 父目录不存在或不可写: {}", parent.display()));
                }
            }
            if self.auth.cookie_ttl_hours == 0 {
                return Err("auth.cookie_ttl_hours 必须为正整数".to_string());
            }
        }
        // 版本回退数量：必须有限且为正，禁止「无限保存」
        if self.max_versions < 1 || self.max_versions > 50 {
            return Err(format!(
                "max_versions 必须在 1~50 之间（当前 {}）；不允许无限保存版本",
                self.max_versions
            ));
        }
        // 界面选项枚举校验
        if !matches!(self.group_layout.as_str(), "vertical" | "horizontal") {
            return Err(format!(
                "group_layout 只能是 vertical 或 horizontal（当前 {}）",
                self.group_layout
            ));
        }
        if !matches!(self.group_panel_position.as_str(), "top" | "left" | "right") {
            return Err(format!(
                "group_panel_position 只能是 top / left / right（当前 {}）",
                self.group_panel_position
            ));
        }
        for g in &self.group_order {
            if g.trim().is_empty() || g.chars().count() > 32 {
                return Err(format!("group_order 中的分组名非法: {g}"));
            }
        }
        // actions 校验
        let mut ids = HashSet::new();
        let mut titles = HashSet::new();
        for a in &self.actions {
            if !is_valid_id(&a.id) {
                return Err(format!("action id 非法: {}", a.id));
            }
            if !ids.insert(a.id.clone()) {
                return Err(format!("action id 重复: {}", a.id));
            }
            if a.title.trim().is_empty() {
                return Err(format!("action {} 的 title 不能为空", a.id));
            }
            if !titles.insert(a.title.clone()) {
                return Err(format!("action title 重复: {}", a.title));
            }
            if a.group.chars().count() > 32 {
                return Err(format!("action {} 的 group 超过 32 字符", a.id));
            }
            // sh 脚本流程的 exec 由 /bin/sh 执行生成的脚本，不指向版本目录，
            // 因此与 versioning 互斥；放在 exec 校验前，保证报错信息准确。
            if a.command
                .as_deref()
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false)
                && a.versioning.as_ref().map(|v| v.enabled).unwrap_or(false)
            {
                return Err(format!(
                    "action {} 不能同时配置 command 与 versioning（版本管理脚本请直接填 exec）",
                    a.id
                ));
            }
            // sh 脚本流程由后端生成 exec（/bin/sh + 脚本绝对路径），
            // 因此不要求配置里手写 exec；其余 action 仍沿用原有校验。
            let shell_mode = a.uses_shell_script();
            if shell_mode {
                let name = a.script_filename();
                // 未自定义完整路径时，才需要校验文件名本身
                if a.custom_script_path().is_none()
                    && (!is_valid_name(&name) || name.contains(".."))
                {
                    return Err(format!("action {} 的 script_name 非法: {name}", a.id));
                }
                // 自定义完整路径：必须是绝对路径，且不允许 `..` 穿越
                if let Some(custom) = a.custom_script_path() {
                    if custom.contains("..") {
                        return Err(format!(
                            "action {} 的脚本路径不能包含 `..`: {custom}",
                            a.id
                        ));
                    }
                    if !Path::new(&custom).is_absolute() {
                        return Err(format!(
                            "action {} 的脚本完整路径必须为绝对路径: {custom}",
                            a.id
                        ));
                    }
                    if Path::new(&custom).file_name().is_none() {
                        return Err(format!(
                            "action {} 的脚本完整路径缺少文件名: {custom}",
                            a.id
                        ));
                    }
                }
                if !Path::new(&a.script_path(self.script_directory.as_deref())).is_absolute() {
                    return Err(format!("action {} 的脚本路径必须为绝对路径", a.id));
                }
            } else {
                if a.exec.is_empty() {
                    return Err(format!("action {} 的 exec 不能为空", a.id));
                }
                if !Path::new(&a.exec[0]).is_absolute() {
                    return Err(format!("action {} 的 exec[0] 必须为绝对路径", a.id));
                }
            }
            if a.timeout == 0 || a.concurrency == 0 {
                return Err(format!("action {} 的 timeout/concurrency 必须为正整数", a.id));
            }
            // versioning 校验
            if let Some(v) = &a.versioning {
                if !Path::new(&v.base_dir).is_absolute() {
                    return Err(format!("action {} 的 versioning.base_dir 必须为绝对路径", a.id));
                }
                // 拦截 `..` 路径穿越：`/opt/../etc` 这类 base_dir 会让
                // 删除接口的 remove_dir_all 递归删到任意目录（CWE-22/CWE-73）。
                if v.base_dir
                    .split(['/', '\\'])
                    .any(|seg| seg == "..")
                {
                    return Err(format!(
                        "action {} 的 versioning.base_dir 不能包含 `..`: {}",
                        a.id, v.base_dir
                    ));
                }
                if !is_valid_name(&v.binary_name) {
                    return Err(format!("action {} 的 binary_name 非法", a.id));
                }
                // 多配置文件：逐一校验，避免路径穿越 / 非法字符
                for cf in v.all_config_files() {
                    if !is_valid_name(&cf) {
                        return Err(format!("action {} 的配置文件名非法: {cf}", a.id));
                    }
                    if cf == v.binary_name {
                        return Err(format!(
                            "action {} 的配置文件名不能与 binary_name 相同: {cf}",
                            a.id
                        ));
                    }
                }
                let prefix = format!("{}/current/", v.base_dir.trim_end_matches('/'));
                // 解释器形式（["/bin/sh", ".../current/run.sh"]）要检查 exec[1]：
                // 版本管理的目标是被解释执行的那个脚本，而不是 /bin/sh 本身
                let version_target = if a.exec.len() >= 2
                    && crate::discover::is_interpreter(&a.exec[0])
                {
                    &a.exec[1]
                } else {
                    &a.exec[0]
                };
                // sh 脚本流程的 exec 由后端生成，不指向版本目录
                if !shell_mode && !version_target.starts_with(&prefix) {
                    return Err(format!(
                        "action {} 开启 versioning 时执行路径必须以 {prefix} 开头（当前 {}）",
                        a.id, version_target
                    ));
                }
                // upload 校验
                if v.upload.max_binary_mb > u64::MAX / 1024 / 1024 {
                    return Err(format!("action {} 的 max_binary_mb 过大", a.id));
                }
            }
            // action.env 危险 key
            for key in a.env.keys() {
                let upper = key.to_ascii_uppercase();
                if FORBIDDEN_ENV_KEYS.contains(&upper.as_str()) {
                    return Err(format!("action {} 的 env 包含禁止的 key: {key}", a.id));
                }
            }
            // 子命令：每项必须是「单个参数」——不含空白，避免拆成多个 argv
            let subs: &[String] = a.subcommands.as_deref().unwrap_or(&[]);
            if subs.len() > 20 {
                return Err(format!("action {} 的 subcommands 最多 20 项", a.id));
            }
            for sub in subs {
                let t = sub.trim();
                if t.is_empty() || t.chars().count() > 32 {
                    return Err(format!(
                        "action {} 的 subcommands 含空值或超长项（≤32 字符）",
                        a.id
                    ));
                }
                if t.chars().any(|c| c.is_whitespace()) {
                    return Err(format!(
                        "action {} 的 subcommands 不能含空格（每项必须是一个参数）: {t}",
                        a.id
                    ));
                }
            }
            // 推送条件：前端只给四种，API/配置文件也必须限定在这四种内。
            // 否则拼错的值会在 executor 里落到 `_ => true` 分支，静默退化成「每次都推」。
            if !matches!(
                a.push_condition.as_str(),
                "always" | "on_failure" | "on_success" | "on_marker"
            ) {
                return Err(format!(
                    "action {} 的 push_condition 只能是 always / on_failure / on_success / on_marker（当前 {}）",
                    a.id, a.push_condition
                ));
            }
            // 推送覆盖项校验：contentType 只有 1/2/3；UID 形如 UID_xxx（字母数字下划线减号）
            if let Some(ct) = a.push_content_type {
                if !(1..=3).contains(&ct) {
                    return Err(format!(
                        "action {} 的 push_content_type 只能是 1(文本)/2(HTML)/3(Markdown)，当前 {ct}",
                        a.id
                    ));
                }
            }
            if let Some(uids) = &a.push_uids {
                for u in uids {
                    let u = u.trim();
                    if u.is_empty() || u.len() > 64 {
                        return Err(format!("action {} 的 push_uids 含空值或超长项", a.id));
                    }
                    if !u
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        return Err(format!("action {} 的 push_uids 含非法字符: {u}", a.id));
                    }
                }
            }
            if let Some(tpl) = &a.push_title {
                // 与 push.rs 的发送侧对齐：WxPusher summary 上限 100 字符，
                // 校验放 200 会让「配置里写了 150 字」在发送时被静默截断。
                if tpl.chars().count() > 100 {
                    return Err(format!(
                        "action {} 的 push_title 超长（≤100 字符，当前 {}）",
                        a.id,
                        tpl.chars().count()
                    ));
                }
            }
            if let Some(tpl) = &a.push_content {
                if tpl.chars().count() > 4000 {
                    return Err(format!("action {} 的 push_content 超长（>4000 字符）", a.id));
                }
            }
            // 参数名校验
            for p in &a.args {
                if !is_valid_id(&p.name) {
                    return Err(format!("action {} 的参数名非法: {}", a.id, p.name));
                }
            }
            // schedule 校验
            if let Some(s) = &a.schedule {
                if s.enabled {
                    // time 格式：一旦配置了 time 必须是合法 HH:MM，否则运行时
                    // compile_cron 才失败、静默走非预期分支（定时任务永不触发）。
                    if let Some(t) = &s.time {
                        if crate::time_util::parse_hhmm(t).is_err() {
                            return Err(format!(
                                "action {} 的 schedule.time 格式非法（应为 HH:MM）: {}",
                                a.id, t
                            ));
                        }
                    }
                    match s.mode.as_str() {
                        "daily" => {
                            if s.time.is_none() {
                                return Err(format!("action {} 的 daily 模式需要 time", a.id));
                            }
                        }
                        "weekly" => {
                            if s.weekdays.is_none() || s.time.is_none() {
                                return Err(format!("action {} 的 weekly 模式需要 weekdays 和 time", a.id));
                            }
                            // weekdays 取值 1-7
                            if let Some(ds) = &s.weekdays {
                                if ds.iter().any(|d| !(1..=7).contains(d)) {
                                    return Err(format!("action {} 的 weekdays 必须在 1-7 之间", a.id));
                                }
                            }
                        }
                        "monthly" => {
                            if s.days.is_none() || s.time.is_none() {
                                return Err(format!("action {} 的 monthly 模式需要 days 和 time", a.id));
                            }
                            // days 取值 1-31
                            if let Some(ds) = &s.days {
                                if ds.iter().any(|d| !(1..=31).contains(d)) {
                                    return Err(format!("action {} 的 days 必须在 1-31 之间", a.id));
                                }
                            }
                        }
                        "cron" => {
                            if s.cron.is_none() {
                                return Err(format!("action {} 的 cron 模式需要 cron 表达式", a.id));
                            }
                            // cron 表达式必须能编译，否则运行时才失败、定时永不触发
                            if let Some(c) = &s.cron {
                                if crate::scheduler::compile_cron(s).is_err() {
                                    return Err(format!("action {} 的 cron 表达式无法解析: {}", a.id, c));
                                }
                            }
                        }
                        other => {
                            return Err(format!("action {} 的未知 schedule 模式: {other}", a.id));
                        }
                    }
                }
            }
            // discover.parser 枚举：只允许 clap/cobra/sections/lines，否则运行时静默退化
            if let Some(d) = &a.discover {
                if !matches!(d.parser.as_str(), "clap" | "cobra" | "sections" | "lines") {
                    return Err(format!(
                        "action {} 的 discover.parser 只能是 clap/cobra/sections/lines（当前 {}）",
                        a.id, d.parser
                    ));
                }
            }
            // args 枚举：style 只能是 option/flag/positional，type 只能是 string/int/bool
            for p in &a.args {
                if !matches!(p.style.as_str(), "option" | "flag" | "positional") {
                    return Err(format!(
                        "action {} 的参数 {} 的 style 只能是 option/flag/positional（当前 {}）",
                        a.id, p.name, p.style
                    ));
                }
                if !matches!(p.r#type.as_str(), "string" | "int" | "bool") {
                    return Err(format!(
                        "action {} 的参数 {} 的 type 只能是 string/int/bool（当前 {}）",
                        a.id, p.name, p.r#type
                    ));
                }
            }
        }
        Ok(())
    }

    /// 根据 id 查找 Action。
    pub fn find_action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// 新增或替换一个 Action（按 id 匹配；已存在则替换）。
    pub fn upsert_action(&mut self, action: Action) {
        if let Some(existing) = self.actions.iter_mut().find(|a| a.id == action.id) {
            *existing = action;
        } else {
            self.actions.push(action);
        }
    }

    /// 应用设置页的配置更新（日志保留 + 推送）。返回后调用方负责写回并校验。
    pub fn apply_settings(&mut self, s: &SettingsUpdate) {
        if let Some(days) = s.log_retention_days {
            self.log_retention_days = if days == 0 { None } else { Some(days) };
        }
        if let Some(v) = s.versioning_enabled {
            self.versioning_enabled = v;
        }
        if let Some(mb) = s.log_max_total_mb {
            self.log_max_total_mb = mb;
        }
        if let Some(enabled) = s.push_enabled {
            self.push.enabled = enabled;
        }
        // spt / app_token：Some 表示更新（含空串表示清空），None 表示不变
        if let Some(spt) = &s.push_spt {
            if spt.is_empty() {
                self.push.spt = None;
            } else {
                self.push.spt = Some(spt.clone());
            }
        }
        if let Some(tok) = &s.push_app_token {
            if tok.is_empty() {
                self.push.app_token = None;
            } else {
                self.push.app_token = Some(tok.clone());
            }
        }
        if let Some(uids) = &s.push_uids {
            self.push.uids = Some(
                uids.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
            );
        }
        if let Some(include_output) = s.push_include_output {
            self.push.include_output = include_output;
        }
        if let Some(mode) = &s.push_summary_mode {
            // 不再静默忽略非法值：直接赋值，交给后续 validate() 拦截并报错，
            // 否则设置页提交非法 summary_mode 会返回 ok:true 但字段未生效。
            self.push.summary_mode = mode.trim().to_string();
        }
        // 版本回退数量上限
        if let Some(n) = s.max_versions {
            self.max_versions = n;
        }
        // 安全模式开关：由「关」切到「开」时补齐 token_file，
        // 避免 validate() 因缺少路径而拒绝，也避免开启后没有可用的 Token 文件。
        if let Some(enabled) = s.auth_enabled {
            self.auth.enabled = enabled;
            if enabled && self.auth.token_file.is_none() {
                self.auth.token_file = Some("/etc/olivetin-rs/token.txt".to_string());
            }
        }
        // 分组列表相关界面选项
        if let Some(v) = s.collapse_groups_by_default {
            self.collapse_groups_by_default = v;
        }
        if let Some(v) = &s.group_layout {
            self.group_layout = v.clone();
        }
        if let Some(v) = &s.group_panel_position {
            self.group_panel_position = v.clone();
        }
        if let Some(v) = &s.group_order {
            self.group_order = v.clone();
        }
    }
}

/// 设置页的配置更新请求。所有字段均为 Option：None 表示「不改动」。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    pub log_retention_days: Option<u32>,
    pub log_max_total_mb: Option<u64>,
    /// 版本管理总开关
    pub versioning_enabled: Option<bool>,
    pub push_enabled: Option<bool>,
    pub push_spt: Option<String>,
    pub push_app_token: Option<String>,
    pub push_uids: Option<String>,
    pub push_include_output: Option<bool>,
    pub max_versions: Option<u32>,
    pub auth_enabled: Option<bool>,
    pub collapse_groups_by_default: Option<bool>,
    pub group_layout: Option<String>,
    pub group_panel_position: Option<String>,
    pub group_order: Option<Vec<String>>,
    /// 通知栏标题来源：auto / content / none（对应 push.summary_mode）
    pub push_summary_mode: Option<String>,
}

impl SettingsUpdate {
    /// 生成本次写回要触及的路径。
    /// 只包含调用方显式提交的字段 —— 未提交的敏感字段（如 push.spt）
    /// 不会被写回，从而不会把 `${VAR}` 插值后的明文固化进配置文件。
    pub fn to_patch(&self) -> crate::config_patch::ConfigPatch {
        use crate::config_patch::ConfigPatch;
        let mut p = ConfigPatch::default();
        macro_rules! scalar_if {
            ($field:expr, $key:expr) => {
                if $field.is_some() {
                    p = p.scalar($key);
                }
            };
        }
        macro_rules! nested_if {
            ($field:expr, $section:expr, $key:expr) => {
                if $field.is_some() {
                    p = p.nested($section, $key);
                }
            };
        }
        scalar_if!(self.log_retention_days, "log_retention_days");
        scalar_if!(self.log_max_total_mb, "log_max_total_mb");
        scalar_if!(self.versioning_enabled, "versioning_enabled");
        scalar_if!(self.max_versions, "max_versions");
        scalar_if!(self.collapse_groups_by_default, "collapse_groups_by_default");
        scalar_if!(self.group_layout, "group_layout");
        scalar_if!(self.group_panel_position, "group_panel_position");
        scalar_if!(self.group_order, "group_order");
        nested_if!(self.push_enabled, "push", "enabled");
        nested_if!(self.push_spt, "push", "spt");
        nested_if!(self.push_app_token, "push", "app_token");
        nested_if!(self.push_uids, "push", "uids");
        nested_if!(self.push_include_output, "push", "include_output");
        nested_if!(self.push_summary_mode, "push", "summary_mode");
        nested_if!(self.auth_enabled, "auth", "enabled");
        nested_if!(self.auth_enabled, "auth", "token_file");
        p
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn validate_accepts_summary_mode_none() {
        let base = "actions:\n- id: a\n  title: a\n  exec: []\n  command: \"echo 1\"\npush:\n  summary_mode: none\n";
        let c: Config = serde_yaml::from_str(base).unwrap();
        assert_eq!(c.push.summary_mode, "none");
        assert!(c.validate().is_ok(), "none 是合法的标题来源模式: {:?}", c.validate());
    }

    #[test]
    fn validate_accepts_omitted_log_defaults() {
        // 回归：省略这两个字段时 serde 用 200/64（12.5MB），阈值必须容得下，
        // 否则「按文档省略字段」的配置会在启动时被拒。
        let base = "actions:\n- id: a\n  title: a\n  exec: []\n  command: \"echo 1\"\n";
        let c: Config = serde_yaml::from_str(base).unwrap();
        assert_eq!(c.log_capacity, 200);
        assert_eq!(c.max_output_kb, 64);
        assert!(c.validate().is_ok(), "省略字段走默认值时应通过校验: {:?}", c.validate());
    }

    use super::*;

    #[test]
    fn test_interpolate_basic() {
        std::env::set_var("OLIVETIN_TEST_VAR", "hello");
        assert_eq!(interpolate("x${OLIVETIN_TEST_VAR}y").unwrap(), "xhelloy");
        assert_eq!(interpolate("a$$b").unwrap(), "a$b");
        assert!(interpolate("${OLIVETIN_NOT_EXIST_XYZ}").is_err());
    }

    #[test]
    fn test_is_valid_name() {
        assert!(is_valid_name("v1.2-beta"));
        assert!(is_valid_name("abc_123"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("../etc"));
        assert!(!is_valid_name("a b"));
        assert!(!is_valid_name(&"x".repeat(65)));
    }

    #[test]
    fn test_is_valid_id() {
        assert!(is_valid_id("checkin"));
        assert!(is_valid_id("check_in-1"));
        assert!(!is_valid_id("has space"));
        assert!(!is_valid_id(""));
    }

    #[test]
    fn test_forbidden_env_keys() {
        // 直接验证列表内容
        assert!(FORBIDDEN_ENV_KEYS.contains(&"LD_PRELOAD"));
        assert!(FORBIDDEN_ENV_KEYS.contains(&"DYLD_INSERT_LIBRARIES"));
    }

    #[test]
    fn script_filename_default_and_custom() {
        let base = "id: led-on\ntitle: led\nexec: [\"/bin/true\"]\n";
        let mut a: Action = serde_yaml::from_str(base).unwrap();
        assert_eq!(a.script_filename(), "led-on.sh");
        assert_eq!(
            a.script_path(None).to_string_lossy(),
            format!("{}/led-on.sh", DEFAULT_SCRIPT_DIRECTORY)
        );
        a.script_name = Some("my-led".into());
        assert_eq!(a.script_filename(), "my-led.sh");
        a.script_name = Some("custom.sh".into());
        assert_eq!(a.script_filename(), "custom.sh");
        a.script_name = None;
        a.command = Some("echo 1 > /tmp/led".into());
        assert!(a.uses_shell_script());
        assert_eq!(
            a.script_content().unwrap(),
            "#!/bin/sh\necho 1 > /tmp/led\n"
        );
    }

    #[test]
    fn command_action_needs_no_exec_but_rejects_unsafe_name_and_versioning() {
        let base = "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: []\n  command: \"echo 1 > /tmp/x\"\n";
        let c: Config = serde_yaml::from_str(base).unwrap();
        assert!(c.validate().is_ok(), "sh 脚本流程不要求 exec");

        let bad = base.replace("  command:", "  script_name: \"../evil.sh\"\n  command:");
        let c2: Config = serde_yaml::from_str(&bad).unwrap();
        assert!(c2.validate().unwrap_err().contains("script_name 非法"));

        let mixed = base.replace(
            "  command:",
            "  versioning:\n    enabled: true\n    base_dir: /opt/x\n    binary_name: wb\n  command:",
        );
        let c3: Config = serde_yaml::from_str(&mixed).unwrap();
        assert!(c3.validate().unwrap_err().contains("不能同时配置 command"));
    }

    #[test]
    fn script_directory_defaults_and_override() {
        assert_eq!(script_directory(None), DEFAULT_SCRIPT_DIRECTORY);
        assert_eq!(script_directory(Some("  ")), DEFAULT_SCRIPT_DIRECTORY);
        assert_eq!(script_directory(Some("/tmp/scripts")), "/tmp/scripts");
    }

    #[test]
    fn script_full_path_overrides_directory_and_appends_sh() {
        let mut a: Action = serde_yaml::from_str(
            "id: led-on\ntitle: led\nexec: []\ncommand: \"echo 1\"\n",
        )
        .unwrap();
        // 未配置：目录 + 文件名
        assert_eq!(
            a.script_path(Some("/opt/s")).to_string_lossy(),
            "/opt/s/led-on.sh"
        );
        // 配了完整路径：直接用，且不再受 script_directory 影响
        a.script_full_path = Some("/usr/local/bin/my-led".into());
        assert!(a.uses_shell_script());
        assert_eq!(
            a.custom_script_path().as_deref(),
            Some("/usr/local/bin/my-led.sh")
        );
        assert_eq!(
            a.script_path(Some("/opt/s")).to_string_lossy(),
            "/usr/local/bin/my-led.sh"
        );
        // 已带 .sh 不重复补
        a.script_full_path = Some("/usr/local/bin/x.sh".into());
        assert_eq!(
            a.script_path(None).to_string_lossy(),
            "/usr/local/bin/x.sh"
        );
        // 只给目录（以 / 结尾）：文件名沿用 script_name / {id}.sh，
        // 不能拼成 `/opt/.sh` —— 面板里填目录是很自然的写法
        a.script_full_path = Some("/opt/".into());
        assert_eq!(
            a.script_path(None).to_string_lossy(),
            "/opt/led-on.sh"
        );
        a.script_name = Some("my-led.sh".into());
        assert_eq!(a.script_path(None).to_string_lossy(), "/opt/my-led.sh");
        a.script_name = None;

        // 空白视为未配置
        a.script_full_path = Some("   ".into());
        assert_eq!(a.custom_script_path(), None);
        assert_eq!(
            a.script_path(Some("/opt/s")).to_string_lossy(),
            "/opt/s/led-on.sh"
        );
    }

    #[test]
    fn script_content_keeps_shebang_and_normalizes_crlf_and_bom() {
        let mut a: Action = serde_yaml::from_str(
            "id: a\ntitle: a\nexec: []\ncommand: \"x\"\n",
        )
        .unwrap();
        // 自带 shebang 不重复添加
        a.command = Some("#!/bin/bash\necho hi".into());
        assert_eq!(a.script_content().unwrap(), "#!/bin/bash\necho hi\n");
        // Windows 导入：BOM + CRLF 被清理，避免 busybox ash 报 not found
        a.command = Some("\u{feff}echo a\r\necho b\r\n".into());
        assert_eq!(a.script_content().unwrap(), "#!/bin/sh\necho a\necho b\n");
        // 空命令返回 None（不会写出空脚本）
        a.command = Some("   \n".into());
        assert!(a.script_content().is_none());
    }

    #[test]
    fn describe_action_changes_reports_concrete_fields() {
        let v = serde_json::json!({
            "id": "a", "title": "磁盘巡检", "exec": ["/bin/sh", "/x.sh"],
            "command": null, "timeout": 30, "push": true, "push_condition": "always"
        });
        let old: Action = serde_json::from_value(v).unwrap();

        // 没改任何字段：明确写「无字段变更」，而不是笼统一句「已编辑脚本」
        assert_eq!(describe_action_changes(Some(&old), &old), "无字段变更");

        let mut new = old.clone();
        new.timeout = 60;
        new.push_condition = "on_failure".into();
        new.push_content_type = Some(3);
        let d = describe_action_changes(Some(&old), &new);
        assert!(d.contains("超时 30→60 秒"), "{d}");
        assert!(d.contains("推送条件 always→on_failure"), "{d}");
        assert!(d.contains("推送内容类型 跟随全局→Markdown"), "{d}");

        // 命令型 action 的 exec 是后端自动生成的，不应报成「执行命令已改为」
        let mut e1 = old.clone();
        e1.command = Some("echo a".into());
        e1.exec = vec!["/bin/sh".into(), "/x.sh".into()];
        let d_exec = describe_action_changes(Some(&old), &e1);
        assert!(!d_exec.contains("执行命令已改为"), "自动生成的 exec 不该上报: {d_exec}");
        assert!(d_exec.contains("命令已填写"), "{d_exec}");

        // 命令改了第 2 行：必须指出行号，而不是「命令已修改」
        let mut a = old.clone();
        a.command = Some("echo 1\necho 2\n".into());
        let mut b = a.clone();
        b.command = Some("echo 1\necho 3\n".into());
        let d2 = describe_action_changes(Some(&a), &b);
        assert!(d2.contains("命令第 2 行已修改"), "{d2}");

        // 定时从无到有
        let mut c = old.clone();
        c.schedule = Some(ScheduleConfig {
            enabled: true,
            mode: "daily".into(),
            timezone: None,
            skip_if_running: true,
            time: Some("08:00".into()),
            weekdays: None,
            days: None,
            cron: None,
        });
        let d3 = describe_action_changes(Some(&old), &c);
        assert!(d3.contains("新增定时：每天 08:00"), "{d3}");

        // 新增条目
        assert_eq!(describe_action_changes(None, &old), "新增条目");
    }

    #[test]
    fn validate_push_title_matches_send_limit() {
        let base = "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: [\"/bin/true\"]\n";
        // 发送侧 summary 截断到 100 字符，校验必须同口径
        let ok: Config = serde_yaml::from_str(&format!(
            "{base}  push_title: \"{}\"\n",
            "标".repeat(100)
        ))
        .unwrap();
        assert!(ok.validate().is_ok(), "100 字符应允许");
        let bad: Config = serde_yaml::from_str(&format!(
            "{base}  push_title: \"{}\"\n",
            "标".repeat(101)
        ))
        .unwrap();
        let err = bad.validate().unwrap_err();
        assert!(err.contains("push_title"), "报错未指出字段: {err}");
    }

    #[test]
    fn validate_push_condition_enum() {
        let base = "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: [\"/bin/true\"]\n";
        // 四种合法值都要通过
        for cond in ["always", "on_failure", "on_success", "on_marker"] {
            let c: Config =
                serde_yaml::from_str(&format!("{base}  push_condition: {cond}\n")).unwrap();
            assert!(c.validate().is_ok(), "{cond} 应合法");
        }
        // 拼错的值必须被拦住（否则运行时会静默退化成每次都推）
        let bad: Config =
            serde_yaml::from_str(&format!("{base}  push_condition: on_fail\n")).unwrap();
        let err = bad.validate().unwrap_err();
        assert!(err.contains("push_condition"), "报错未指出字段: {err}");
        assert!(err.contains("on_marker"), "报错未列出合法值: {err}");
    }

    #[test]
    fn validate_rejects_bad_custom_script_path() {        let base = "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: []\n  command: \"echo 1\"\n";
        let ok: Config = serde_yaml::from_str(&format!(
            "{base}  script_full_path: /usr/local/bin/led.sh\n"
        ))
        .unwrap();
        assert!(ok.validate().is_ok(), "绝对路径自定义应合法");

        let rel: Config = serde_yaml::from_str(&format!(
            "{base}  script_full_path: rel/led.sh\n"
        ))
        .unwrap();
        assert!(rel.validate().unwrap_err().contains("必须为绝对路径"));

        let dotdot: Config = serde_yaml::from_str(&format!(
            "{base}  script_full_path: /opt/../etc/led.sh\n"
        ))
        .unwrap();
        assert!(dotdot.validate().unwrap_err().contains("不能包含"));
    }

    #[test]
    fn max_versions_rejects_unlimited() {
        // 用 YAML 反序列化，让 serde 的字段默认值生效；
        // Config::default() 不会套用 #[serde(default = "...")]，会先撞上别的校验。
        let mut c: Config = serde_yaml::from_str(
            "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: [\"/bin/true\"]\n",
        )
        .unwrap();
        assert_eq!(c.max_versions, 5, "默认应为 5");
        assert!(c.validate().is_ok(), "基线配置应合法");
        c.max_versions = 0;
        assert!(c.validate().unwrap_err().contains("max_versions"));
        c.max_versions = 51;
        assert!(c.validate().unwrap_err().contains("max_versions"));
        c.max_versions = 5;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn settings_patch_only_contains_submitted_fields() {
        let s = SettingsUpdate {
            log_retention_days: Some(14),
            ..Default::default()
        };
        let p = s.to_patch();
        assert!(p.scalars.contains(&"log_retention_days".to_string()));
        assert!(p.nested.is_empty(), "未提交的 push 字段不应写回");
        assert!(p.actions.is_none());
    }

    #[test]
    fn all_config_files_merges_legacy_and_list_without_duplicates() {
        let v = VersioningConfig {
            enabled: true,
            base_dir: "/opt/olivetin/signin".into(),
            binary_name: "wb".into(),
            config_name: Some("wb-signin.json".into()),
            config_files: vec![
                "wb-signin.json".into(),
                "xiaoning.json".into(),
                "xiaoyu.json".into(),
            ],
            upload: UploadConfig::default(),
        };
        assert_eq!(
            v.all_config_files(),
            vec!["wb-signin.json", "xiaoning.json", "xiaoyu.json"]
        );
    }

    #[test]
    fn validate_accepts_multiple_config_files_and_rejects_unsafe_names() {
        let base = "max_concurrent: 2\nmax_output_kb: 32\nlog_capacity: 100\nactions:\n- id: a\n  title: a\n  exec: [\"/opt/x/current/wb\"]\n  versioning:\n    enabled: true\n    base_dir: /opt/x\n    binary_name: wb\n    config_files: [\"wb-signin.json\", \"xiaoning.json\", \"xiaoyu.json\"]\n";
        let c: Config = serde_yaml::from_str(base).unwrap();
        assert_eq!(c.actions[0].versioning.as_ref().unwrap().all_config_files().len(), 3);
        assert!(c.validate().is_ok(), "多配置文件应当合法");

        let bad = base.replace("\"xiaoyu.json\"", "\"../evil.json\"");
        let c2: Config = serde_yaml::from_str(&bad).unwrap();
        assert!(c2.validate().unwrap_err().contains("配置文件名非法"));
    }
}
