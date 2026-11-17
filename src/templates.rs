//! askama 模板定义：base / actions / logs / settings。
//!
//! base.html 提供整体布局（含内联 CSS/JS），子模板通过 extends 继承。
//! CSS 与 JS 通过 include_str! 内联，不注册 /static/* 路由。

use askama::Template;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 内联 CSS：编译期常量，零拷贝。
fn css() -> &'static str {
    include_str!("../static/style.css")
}

/// 内联 JS：编译期常量，零拷贝。
fn js() -> &'static str {
    include_str!("../static/app.js")
}

/// 动作列表页（继承 base）。
#[derive(Template)]
#[template(path = "actions.html")]
pub struct ActionsTemplate {
    pub title: String,
    pub css: &'static str,
    pub js: &'static str,
    pub auth_enabled: bool,
    /// 顶栏显示的版本号（编译期写入 CARGO_PKG_VERSION）。
    pub version: String,
    /// 鼠标悬停时的完整信息：commit + 构建时间 + rustc。
    pub version_detail: String,
}

/// 日志页（继承 base）。
#[derive(Template)]
#[template(path = "logs.html")]
pub struct LogsTemplate {
    pub title: String,
    pub css: &'static str,
    pub js: &'static str,
    pub auth_enabled: bool,
    /// 顶栏显示的版本号（编译期写入 CARGO_PKG_VERSION）。
    pub version: String,
    /// 鼠标悬停时的完整信息：commit + 构建时间 + rustc。
    pub version_detail: String,
}

/// 设置页（继承 base）。
#[derive(Template)]
#[template(path = "settings.html")]
pub struct SettingsTemplate {
    pub title: String,
    pub css: &'static str,
    pub js: &'static str,
    pub auth_enabled: bool,
    /// 顶栏显示的版本号（编译期写入 CARGO_PKG_VERSION）。
    pub version: String,
    /// 鼠标悬停时的完整信息：commit + 构建时间 + rustc。
    pub version_detail: String,
}

/// 脚本详情页（继承 base）。
#[derive(Template)]
#[template(path = "action.html")]
pub struct ActionTemplate {
    pub title: String,
    pub css: &'static str,
    pub js: &'static str,
    pub auth_enabled: bool,
    /// 顶栏显示的版本号（编译期写入 CARGO_PKG_VERSION）。
    pub version: String,
    /// 鼠标悬停时的完整信息：commit + 构建时间 + rustc。
    pub version_detail: String,
}

/// 面板版本号：直接取编译期 Cargo 版本，避免「不知道跑的是哪一版」。
pub fn version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// 版本详情（悬停提示 / 设置页显示）：commit + 构建时间 + rustc。
pub fn version_detail() -> String {
    format!(
        "{} · commit {} · 构建于 {} · {}",
        version(),
        env!("GIT_COMMIT"),
        env!("BUILD_TIME"),
        env!("RUSTC_VERSION")
    )
}

/// 页面类型：渲染结果只随 (页面, auth_enabled) + 编译期版本常量变化。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Actions,
    Settings,
    Logs,
    Action,
}

/// 缓存渲染后的完整 HTML（含 165KB 内联资源），每请求不再重复 render/复制。
/// 用 Box::leak 换取 &'static str：条目数上限 8（4 页 × 2 种 auth 状态），不会无限增长。
pub fn render_cached(page: Page, auth_enabled: bool) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<(Page, bool), &'static str>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (page, auth_enabled);
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.get(&key) {
        return *cached;
    }
    let html = match page {
        Page::Actions => actions_template(auth_enabled).render(),
        Page::Settings => settings_template(auth_enabled).render(),
        Page::Logs => logs_template(auth_enabled).render(),
        Page::Action => action_template(auth_enabled).render(),
    }
    .unwrap_or_default();
    let leaked: &'static str = Box::leak(html.into_boxed_str());
    guard.insert(key, leaked);
    leaked
}

/// 构建 ActionsTemplate。
pub fn actions_template(auth_enabled: bool) -> ActionsTemplate {
    ActionsTemplate {
        title: "OliveTin-RS".to_string(),
        css: css(),
        js: js(),
        auth_enabled,
        version: version(),
        version_detail: version_detail(),
    }
}

/// 构建 LogsTemplate。
pub fn logs_template(auth_enabled: bool) -> LogsTemplate {
    LogsTemplate {
        title: "日志 · OliveTin-RS".to_string(),
        css: css(),
        js: js(),
        auth_enabled,
        version: version(),
        version_detail: version_detail(),
    }
}

/// 构建 SettingsTemplate。
pub fn settings_template(auth_enabled: bool) -> SettingsTemplate {
    SettingsTemplate {
        title: "设置 · OliveTin-RS".to_string(),
        css: css(),
        js: js(),
        auth_enabled,
        version: version(),
        version_detail: version_detail(),
    }
}

/// 构建 ActionTemplate（脚本详情页）。
pub fn action_template(auth_enabled: bool) -> ActionTemplate {
    ActionTemplate {
        title: "脚本详情 · OliveTin-RS".to_string(),
        css: css(),
        js: js(),
        auth_enabled,
        version: version(),
        version_detail: version_detail(),
    }
}
