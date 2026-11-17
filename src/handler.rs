//! Axum 路由：API + HTML 页面。

use axum::extract::{Multipart, Path, Query, Request, State};
use axum::middleware::Next;
use axum::http::header::{CACHE_CONTROL, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::config::{Action, Config};
use crate::config_patch::{self, ConfigPatch};
use crate::state::AppState;
use crate::templates;

/// baseline 解析结果缓存：按原文本 FNV-1a 哈希命中，
/// 省掉一次全量 YAML 解析 + 一次全量 Value 序列化。
/// 只缓存只读的 baseline，写前回读校验与原子替换完全不变。
/// 缓存键为 (哈希, 原文长度)：FNV-1a 64 位无密钥哈希存在碰撞可能，
/// 附加长度作为第二道校验，避免碰撞时基于错误 baseline 打补丁（CWE-328）。
static BASELINE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(u64, usize, serde_yaml::Value)>>,
> = std::sync::OnceLock::new();

fn content_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn baseline_mapping(original: &str) -> Result<serde_yaml::Mapping, String> {
    let hash = content_hash(original);
    let len = original.len();
    let cache = BASELINE_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let hit = {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some((h, l, v)) if *h == hash && *l == len => Some(v.clone()),
            _ => None,
        }
    };
    let value = match hit {
        Some(v) => v,
        None => {
            let cfg: Config = serde_yaml::from_str(original)
                .map_err(|e| format!("原配置解析失败: {e}"))?;
            let v = serde_yaml::to_value(&cfg)
                .map_err(|e| format!("基线序列化失败: {e}"))?;
            *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((hash, len, v.clone()));
            v
        }
    };
    value
        .as_mapping()
        .cloned()
        .ok_or("基线配置不是 mapping".to_string())
}

/// 生成本次「只同步 actions」的补丁。
fn actions_patch(cfg: &Config) -> ConfigPatch {
    let seq = serde_yaml::to_value(&cfg.actions)
        .ok()
        .and_then(|v| v.as_sequence().cloned())
        .unwrap_or_default();
    ConfigPatch::default().with_actions(seq)
}

/// 统一配置写回入口：原文本最小差异补丁 + 写前回读校验 + 原子替换。
///
/// 取代过去各处的 `serde_yaml::to_string(&new_config)` 全量重写：
/// - 注释、行尾注释、未知配置项（如 `push.quota_per_day`）原样保留；
/// - 只更新 `patch` 声明的路径，未提交的敏感字段不会被写回；
/// - 写盘前把补丁结果重新反序列化成 Config，逐一核对 action id，
///   任何 action 丢失都直接放弃写入，绝不落盘半成品配置。
fn persist_config(state: &AppState, new_config: &Config, patch: ConfigPatch) -> Result<(), String> {
    let path = state.get_config_path();
    if path.is_empty() {
        return Err("config_path 未初始化".to_string());
    }
    let original =
        std::fs::read_to_string(&path).map_err(|e| format!("读取原配置失败: {e}"))?;
    let baseline = baseline_mapping(&original)?;
    let new_val = serde_yaml::to_value(new_config).map_err(|e| format!("序列化失败: {e}"))?;
    let new_root = new_val.as_mapping().ok_or("新配置不是 mapping".to_string())?;

    let text = config_patch::apply(&original, &baseline, new_root, &patch)?;

    // 回读校验：补丁结果必须是合法 YAML，且 action 集合与内存配置完全一致
    let verify: Config = serde_yaml::from_str(&text)
        .map_err(|e| format!("写回结果校验失败（已放弃写入）: {e}"))?;
    if verify.actions.len() != new_config.actions.len() {
        return Err(format!(
            "写回结果校验失败（已放弃写入）：action 数量 {} != {}",
            verify.actions.len(),
            new_config.actions.len()
        ));
    }
    for a in &new_config.actions {
        if verify.find_action(&a.id).is_none() {
            return Err(format!(
                "写回结果校验失败（已放弃写入）：action {} 丢失",
                a.id
            ));
        }
    }
    crate::log::write_file_atomic(std::path::Path::new(&path), &text)
        .map_err(|e| format!("写回配置失败: {e}"))
}

/// 构建路由。
/// 所有写接口共用的限流：独立的 120/min 令牌桶，
/// 不复用 state.rate_limiter，避免与 run_action / upload_version 内部限流双重计数。
static WRITE_LIMITER: std::sync::OnceLock<tokio::sync::Mutex<crate::rate_limit::RateLimiter>> =
    std::sync::OnceLock::new();

async fn write_rate_limit(req: Request, next: Next) -> Result<Response, StatusCode> {
    if matches!(
        req.method(),
        &axum::http::Method::POST | &axum::http::Method::PUT
            | &axum::http::Method::DELETE | &axum::http::Method::PATCH
    ) {
        let limiter = WRITE_LIMITER
            .get_or_init(|| tokio::sync::Mutex::new(crate::rate_limit::RateLimiter::new(120)));
        if !limiter.lock().await.allow() {
            return Ok((
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "60")],
                Json(json!({"error": "rate_limited"})),
            )
                .into_response());
        }
    }
    Ok(next.run(req).await)
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/actions", get(list_actions).post(create_action))
        .route("/api/actions/:id", axum::routing::get(get_action).put(update_action).delete(delete_action))
        .route("/api/actions/:id/run", post(run_action))
        .route("/api/actions/:id/stop", post(stop_action))
        .route("/api/actions/:id/versions", get(list_versions).post(upload_version))
        .route("/api/actions/:id/versions/:name/activate", post(activate_version))
        .route("/api/actions/:id/versions/rollback", post(rollback_version))
        .route("/api/actions/:id/config", get(get_action_config).post(update_action_config))
        .route("/api/actions/:id/discover", post(trigger_discover))
        .route("/api/scripts/inspect", post(inspect_script))
        .route("/api/scripts/list", post(list_scripts))
        .route("/api/logs", get(list_logs))
        .route("/api/logs/clear", post(clear_log_files))
        .route("/api/auth/reset-token", post(reset_token))
        .route("/api/settings", get(get_settings).post(update_settings))
        .route("/api/groups/order", post(update_group_order))
        .route("/api/push/test", post(test_push))
        .route("/", get(index_page))
        .route("/logs", get(logs_page))
        .route("/settings", get(settings_page))
        .route("/action/:id", get(action_page))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::auth_middleware,
        ));

    let public = Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/callback", get(auth_callback));

    // F-10i：版本信息（commit / 构建时间 / rustc）移入 protected，减少指纹暴露。
    let protected = protected.route("/api/version", get(api_version));

    public
        .merge(protected)
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn(write_rate_limit))
}

// ============ 页面 ============

/// 页面 HTML 只随 (页面类型, auth_enabled, 构建版本) 变化。
/// ETag 命中返回 304，避免每次刷新都重传 165KB 内联 CSS/JS。
fn page_response(headers: &HeaderMap, page: templates::Page, auth_enabled: bool) -> Response {
    use axum::http::header::{ETAG, IF_NONE_MATCH};
    // ETag 纳入 auth_enabled：渲染缓存 key 已按 (page, auth_enabled) 区分，
    // 若 ETag 只有 BUILD_ID，切换 auth.enabled 后同一 URL 的 If-None-Match
    // 仍会命中 304，客户端拿到旧的 HTML（auth 状态与页面不匹配）。
    let etag = format!("\"{}-{}\"", env!("BUILD_ID"), if auth_enabled { 1 } else { 0 });
    let matched = headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "*" || v.split(',').any(|p| p.trim() == etag))
        .unwrap_or(false);
    if matched {
        return (StatusCode::NOT_MODIFIED, [(ETAG, etag.as_str())]).into_response();
    }
    (
        [(CACHE_CONTROL, "no-cache"), (ETAG, etag.as_str())],
        Html(templates::render_cached(page, auth_enabled)),
    )
        .into_response()
}

async fn index_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    page_response(&headers, templates::Page::Actions, state.config.load().auth.enabled)
}

async fn settings_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    page_response(&headers, templates::Page::Settings, state.config.load().auth.enabled)
}

async fn logs_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    page_response(&headers, templates::Page::Logs, state.config.load().auth.enabled)
}

async fn action_page(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    page_response(&headers, templates::Page::Action, state.config.load().auth.enabled)
}

// ============ 认证回调 ============

async fn auth_callback(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    // ?token=xxx → 校验有效性 → 种 cookie → 302 去掉 query
    if let Some(token) = params.get("token") {
        // 校验 token 有效性：之前无条件把任意值种进 cookie，未校验有效性，
        // 配合会话固定（CWE-384）可让攻击者预置 cookie。无效则拒绝。
        if !matches!(crate::auth::verify(&state, token), crate::auth::AuthResult::Ok) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        // 统一用 make_auth_cookie：之前硬编码 Max-Age=604800，与其它三处
        // cookie_ttl_hours*3600 双源，认证语义易漂移（M-30）。
        let cookie = crate::auth::make_auth_cookie(
            token,
            state.config.load().auth.cookie_ttl_hours,
        );
        // 不再 unwrap：Response 构造失败时降级为 500，避免 panic=abort 直接终止进程。
        return match Response::builder()
            .status(StatusCode::FOUND)
            .header(axum::http::header::LOCATION, "/")
            .header(SET_COOKIE, cookie)
            .body(axum::body::Body::empty())
        {
            Ok(r) => r,
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR).into_response(),
        };
    }
    StatusCode::UNAUTHORIZED.into_response()
}

// ============ API ============

// ============ 新增脚本 ============

/// 创建/编辑时把 shell 命令落成 sh 脚本文件，并把 exec 固定为
/// `/bin/sh <脚本绝对路径>`。命令为空的旧 action 保持原 exec 直接执行。
/// 返回脚本路径供日志与前端编辑界面展示。
fn prepare_action_script(
    action: &mut crate::config::Action,
    script_dir: Option<&str>,
) -> Result<Option<String>, String> {
    if !action.uses_shell_script() {
        return Ok(None);
    }
    let path = crate::executor::write_shell_script(action, script_dir)?;
    let display = path.to_string_lossy().to_string();
    action.exec = vec!["/bin/sh".to_string(), display.clone()];
    Ok(Some(display))
}

/// 创建新 action。multipart 字段：
/// - `action`：action 定义的 JSON 字符串（必填）
/// - `binary`：二进制文件（有 versioning 时必填）
/// - `config`：配置文件（可多个，每个字段用原始文件名）
async fn create_action(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    let mut action_json: Option<String> = None;
    let mut binary: Option<Vec<u8>> = None;
    // 多个配置文件：每个 config 字段用浏览器给的原始文件名
    let mut config_payloads: Vec<(String, Vec<u8>)> = Vec::new();
    // 内存放大防护：binary/config 全量入内存，需卡总量与单文件上限（CWE-770）。
    // binary 上限对齐 upload 侧 max_binary_mb 默认 50MB；config 累计对齐 max_config_kb×4。
    const CREATE_MAX_BINARY_BYTES: usize = 50 * 1024 * 1024;
    const CREATE_MAX_CONFIG_BYTES: usize = 256 * 1024 * 4;
    let mut config_total: usize = 0;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "multipart_error"})))
                    .into_response();
            }
        };
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "action" => {
                if let Ok(t) = field.text().await {
                    action_json = Some(t);
                }
            }
            "binary" => {
                match field.bytes().await {
                    Ok(d) if d.len() > CREATE_MAX_BINARY_BYTES => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": "binary_too_large"})),
                        )
                            .into_response();
                    }
                    Ok(d) => binary = Some(d.to_vec()),
                    Err(_) => {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error": "multipart_error"})))
                            .into_response();
                    }
                }
            }
            "config" => {
                let fname = field.file_name().unwrap_or("").to_string();
                match field.bytes().await {
                    Ok(d) if config_total.saturating_add(d.len()) > CREATE_MAX_CONFIG_BYTES => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": "config_too_large"})),
                        )
                            .into_response();
                    }
                    Ok(d) => {
                        config_total += d.len();
                        config_payloads.push((fname, d.to_vec()));
                    }
                    Err(_) => {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error": "multipart_error"})))
                            .into_response();
                    }
                }
            }
            _ => {}
        }
    }

    let Some(action_json) = action_json else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "action_missing"}))).into_response();
    };
    let mut action: crate::config::Action = match serde_json::from_str(&action_json) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("action 解析失败: {e}")})),
            )
                .into_response();
        }
    };

    // 重名检查：新增时 id 已存在则拒绝（避免静默覆盖）
    let mut new_config = state.config.load().as_ref().clone();
    if new_config.find_action(&action.id).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "id_exists", "message": format!("脚本 ID「{}」已存在", action.id)})),
        )
            .into_response();
    }

    // 校验前置：在写脚本文件 / 建版本目录 / 写 binary / 建软链等任何副作用之前，
    // 先对用户提交的 action 做完整校验。base_dir 的 `..`、script_full_path 穿越、
    // binary_name / config_files 非法等危险输入在此被拦截，杜绝「先落盘再校验」
    // 导致非法字段在被拒绝前已产生不可逆副作用（CWE-22/CWE-73）。
    new_config.upsert_action(action.clone());
    if let Err(e) = new_config.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }

    // 带 shell 命令的 action：先把命令写成 sh 脚本，exec 固定为 /bin/sh + 脚本路径
    let script_dir = state.config.load().script_directory.clone();
    let created_script = match prepare_action_script(&mut action, script_dir.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": e, "message": e}))).into_response();
        }
    };

    // 有 versioning 且 enabled：需要 binary，落盘第一个版本
    if let Some(vcfg) = &action.versioning {
        if vcfg.enabled {
            let binary_data = match binary {
                Some(b) => b,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "binary_missing"})),
                    )
                        .into_response();
                }
            };
            // 创建 base_dir + 首个版本目录（统一 v0.N 体系）+ current 软链接
            let base = std::path::Path::new(&vcfg.base_dir);
            // 兼容早期写出的 v1 目录：归一为 v0.1，保证后续 next_version_name 递增连续。
            if let Err(e) = crate::versioning::migrate_legacy_first_version(base) {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e}))).into_response();
            }
            let vdir = base.join(crate::versioning::FIRST_VERSION);
            if let Err(e) = std::fs::create_dir_all(&vdir) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("创建版本目录失败: {e}")})),
                )
                    .into_response();
            }
            let bin_path = vdir.join(&vcfg.binary_name);
            if let Err(e) = std::fs::write(&bin_path, &binary_data) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("写二进制失败: {e}")})),
                )
                    .into_response();
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755));
            }
            // 把这一整套配置写进 v1：每个版本目录自带配置，切换 current 即整套切换
            let declared = vcfg.all_config_files();
            for (fname, bytes) in &config_payloads {
                if fname.is_empty() || !declared.contains(fname) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("未登记的配置文件: {fname}")})),
                    )
                        .into_response();
                }
                if let Err(e) = std::fs::write(vdir.join(fname), bytes) {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": format!("写配置失败: {e}")})),
                    )
                        .into_response();
                }
            }
            // 首个版本没有旧版本可继承，已登记的配置必须齐全
            for f in &declared {
                if !config_payloads.iter().any(|(n, _)| n == f) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("缺少配置文件: {f}")})),
                    )
                        .into_response();
                }
            }
            // 写上传元数据（版本 = 二进制 + 整套配置 的快照）
            let config_total: u64 = config_payloads.iter().map(|(_, b)| b.len() as u64).sum();
            let meta = crate::versioning::UploadMeta {
                uploaded_at: chrono::Utc::now().to_rfc3339(),
                uploaded_by: "panel".to_string(),
                binary_bytes: binary_data.len() as u64,
                config_bytes: config_total,
            };
            if let Ok(json) = serde_json::to_vec(&meta) {
                let _ = std::fs::write(vdir.join(".upload-meta.json"), json);
            }
            // 创建 current 软链接
            if let Err(e) = crate::versioning::set_current_link(base, crate::versioning::FIRST_VERSION) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("创建软链接失败: {e}")})),
                )
                    .into_response();
            }
        }
    }

    // 写回 config.yaml
    let config_path = state.get_config_path();
    if config_path.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "config_path 未初始化"})),
        )
            .into_response();
    }
    let action_id = action.id.clone();
    let action_title = action.title.clone();
    let action_group = action.group.clone();
    // 重新 upsert：prepare_action_script 已把 exec 固定为 /bin/sh + 脚本路径，
    // 最终态再校验一次（exec 已是绝对路径，不会触发新的失败）。
    new_config.upsert_action(action);
    if let Err(e) = new_config.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    if let Err(e) = persist_config(&state, &new_config, actions_patch(&new_config)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }

    // 热加载：新增的定时脚本必须立即获得调度循环（历史 bug：只在进程启动时启动过）
    state.config.store(std::sync::Arc::new(new_config));
    crate::scheduler::sync_all(&state).await;

    // 记录新增操作到日志
    {
        let cfg = state.config.load();
        let tz_offset = crate::time_util::parse_timezone(&cfg.default_timezone)
            .unwrap_or_else(|_| chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        let entry = crate::log::make_operation_log(
            state.log_next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            "create",
            &action_id,
            &action_title,
            &action_group,
            &format!("已新增脚本「{}」", action_title),
            &tz_offset,
        );
        // 锁内只做内存环形缓冲操作；落盘放到锁外，避免持锁做磁盘 IO / await。
        {
            let mut logs = state.logs.lock().await;
            logs.push_back(entry.clone());
            let capacity = cfg.log_capacity;
            while logs.len() > capacity {
                logs.pop_front();
            }
        }
        if let Some(log_dir) = &cfg.log_directory {
            crate::log::write_script_log(&state, &entry, log_dir, &cfg.default_timezone).await;
            crate::log::append_total_log(log_dir, &entry);
        }
    }

    tracing::info!("新增脚本已保存");
    Json(json!({"ok": true, "script_path": created_script})).into_response()
}

// ============ 编辑 / 删除脚本 ============

/// 返回单个 action 的完整定义（含 exec/versioning/schedule/args，用于编辑）。
async fn get_action(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let config = state.config.load();
    match config.find_action(&id) {
        Some(action) => {
            let state_str = state.action_state(&id).await;
            let params = params_for(&state, action).await;
            let schedule_display = schedule_json(action, &config);
            Json(json!({
                "id": action.id,
                "title": action.title,
                "group": action.group,
                "enabled": action.enabled,
                "description": action.description,
                "exec": action.exec,
                "command": action.command,
                "script_name": action.script_name,
                // 自定义完整路径（编辑界面可改）；为空表示按目录 + 文件名拼接
                "script_full_path": action.script_full_path,
                // 引用型脚本路径（服务器上已有、面板不接管）；非引用型为 null
                "script_ref": action.script_ref(),
                // 编辑界面据此决定默认展示哪种来源：command / ref / versioning
                "script_mode": if action.uses_shell_script() {
                    "command"
                } else if action.script_ref().is_some() {
                    "ref"
                } else if action.versioning.as_ref().map(|v| v.enabled).unwrap_or(false) {
                    "versioning"
                } else {
                    "exec"
                },
                // 编辑界面展示的完整路径（与后端生成脚本时用的规则一致）
                "script_path": action.script_path(config.script_directory.as_deref()).to_string_lossy(),
                "script_directory": crate::config::script_directory(config.script_directory.as_deref()),
                "timeout": action.timeout,
                "concurrency": action.concurrency,
                "push": action.push,
                "push_condition": action.push_condition,
                // 脚本级推送覆盖项（为空表示沿用全局设置）
                "push_title": action.push_title,
                "push_content": action.push_content,
                "push_uids": action.push_uids,
                "push_topic_ids": action.push_topic_ids,
                "push_content_type": action.push_content_type,
                "env": action.env,
                "versioning": action.versioning,
                "schedule": action.schedule,
                "schedule_display": schedule_display,
                "args": action.args,
                "params": params,
                // 版本徽标：详情页要靠它显示「版本管理」入口（之前只有列表接口返回）
                "active_version": action
                    .versioning
                    .as_ref()
                    .filter(|v| v.enabled)
                    .and_then(|v| crate::versioning::read_current_link(std::path::Path::new(&v.base_dir)).ok()),
                // 下拉用：合并后的子命令（配置优先，否则取 --help 里的 flag 参数，已过滤 help）
                "subcommands": crate::executor::effective_subcommands(&state, &action).await,
                // 原始配置值：前端保存时原样回传，避免把自动生成的项写进 YAML
                "subcommands_config": action.subcommands,
                "discover": action.discover,
                "state": state_str,
            }))
            .into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response(),
    }
}

/// 编辑脚本元信息（标题/分组/描述/超时/并发/推送/参数/定时等）。
async fn update_action(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut body): Json<crate::config::Action>,
) -> Response {
    // 校验 id 一致性
    if body.id != id {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id 不一致"})),
        )
            .into_response();
    }
    let config_path = state.get_config_path();
    if config_path.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "config_path 未初始化"})),
        )
            .into_response();
    }
    let mut new_config = state.config.load().as_ref().clone();
    // 记录旧脚本路径：改名或切回直接 exec 时清理残留的 sh 文件
    let old_script_path = new_config
        .find_action(&id)
        .filter(|a| a.uses_shell_script())
        .map(|a| a.script_path(new_config.script_directory.as_deref()));
    let script_dir = new_config.script_directory.clone();

    // 开了版本管理但目录留空 → 自动建 {脚本目录}/{id}/v0.1 并把当前脚本复制进去。
    // 提前到 validate 之前：bootstrap 会补全 base_dir，使后续 validate 的
    // 「exec 必须以 {base_dir}/current/ 开头」检查能通过；且 bootstrap 的目标
    // 路径来自 script_dir + sanitize(id)，不依赖未校验的用户输入。
    let needs_bootstrap = body
        .versioning
        .as_ref()
        .map(|v| v.enabled && v.base_dir.trim().is_empty())
        .unwrap_or(false);
    if needs_bootstrap {
        let dir = script_dir.clone().unwrap_or_else(|| "/opt/olivetin/scripts".to_string());
        if let Err(e) = crate::versioning::bootstrap(&mut body, &dir) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e, "message": e})),
            )
                .into_response();
        }
    }

    // 校验前置：在写脚本 / 删旧脚本等副作用之前，先对更新后的 action 做完整校验，
    // 拦截 script_name 穿越、base_dir 的 `..`、binary_name/config_files 非法等
    // 危险输入，避免「先落盘/删文件再校验」产生不可逆副作用。
    new_config.upsert_action(body.clone());
    if let Err(e) = new_config.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }

    let saved_script = match prepare_action_script(&mut body, script_dir.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": e, "message": e})),
            )
                .into_response();
        }
    };

    let new_script_path = body
        .uses_shell_script()
        .then(|| body.script_path(script_dir.as_deref()));
    // 切换成「引用同一个文件」时不能把旧托管文件删掉：那正是用户刚指向的脚本
    let new_ref_path = body.script_ref();
    if let Some(old) = old_script_path {
        let referenced = new_ref_path
            .as_deref()
            .map(|r| std::path::Path::new(r) == old.as_path())
            .unwrap_or(false);
        if !referenced && Some(&old) != new_script_path.as_ref() {
            let _ = std::fs::remove_file(&old);
        }
    }
    let body_title = body.title.clone();
    let body_group = body.group.clone();
    // 旧值留一份：写操作日志时要逐字段对比出「改了什么」
    let before_action = new_config.find_action(&id).cloned();
    new_config.upsert_action(body);
    let after_action = new_config.find_action(&id).cloned();
    if let Err(e) = new_config.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    if let Err(e) = persist_config(&state, &new_config, actions_patch(&new_config)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    // 热加载：store 新配置 + 通知调度重载
    state.config.store(std::sync::Arc::new(new_config));
    crate::scheduler::sync_all(&state).await;

    // 记录编辑操作到日志
    {
        let cfg = state.config.load();
        let tz_offset = crate::time_util::parse_timezone(&cfg.default_timezone)
            .unwrap_or_else(|_| chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        // 日志必须写清「改了什么」：光写「已编辑脚本」等于没写，
        // 事后只能去翻配置历史才知道动了哪些字段。
        let change = match after_action.as_ref() {
            Some(a) => crate::config::describe_action_changes(before_action.as_ref(), a),
            None => "无字段变更".to_string(),
        };
        let entry = crate::log::make_operation_log(
            state.log_next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            "update",
            &id,
            &body_title,
            &body_group,
            &format!("已编辑脚本「{}」：{}", body_title, change),
            &tz_offset,
        );
        // 锁内只做内存环形缓冲操作；落盘放到锁外，避免持锁做磁盘 IO / await。
        {
            let mut logs = state.logs.lock().await;
            logs.push_back(entry.clone());
            let capacity = cfg.log_capacity;
            while logs.len() > capacity {
                logs.pop_front();
            }
        }
        if let Some(log_dir) = &cfg.log_directory {
            crate::log::write_script_log(&state, &entry, log_dir, &cfg.default_timezone).await;
            crate::log::append_total_log(log_dir, &entry);
        }
    }

    tracing::info!(action = %id, "脚本已更新");
    Json(json!({"ok": true, "script_path": saved_script})).into_response()
}

/// 删除脚本（可选删除其版本目录）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteActionQuery {
    #[serde(default)]
    remove_dir: bool,
}

/// 删除 action 对应的 sh 脚本文件（日志文件不动）。
///
/// 历史缺陷：面板删除时只从配置里移除 action，磁盘上的 `.sh` 留在原地，
/// 于是「删了又建同名脚本」会直接复用旧文件，看起来像没删干净。
/// 候选路径两条：action 实际生效的路径 + 默认 `{script_directory}/{id}.sh`
/// （后者覆盖「早期用 command 创建、后来改成 exec」留下的同名残留）。
/// 被其它 action 引用的路径一律跳过，避免误删共享脚本。
fn remove_action_script(
    cfg: &crate::config::Config,
    target: &crate::config::Action,
) -> Option<String> {
    let dir = cfg.script_directory.as_deref();
    let base_dir = std::path::Path::new(crate::config::script_directory(dir));
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if target.uses_shell_script() {
        candidates.push(target.script_path(dir));
    }
    let legacy = base_dir.join(format!("{}.sh", target.id));
    if !candidates.contains(&legacy) {
        candidates.push(legacy);
    }

    let mut removed: Vec<String> = Vec::new();
    for path in candidates {
        let shared = cfg
            .actions
            .iter()
            .filter(|a| a.id != target.id)
            .any(|a| a.script_path(dir) == path);
        if shared {
            tracing::info!(path = %path.display(), "脚本路径被其它 action 共用，跳过删除");
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path.to_string_lossy().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "删除脚本文件失败");
            }
        }
    }
    if removed.is_empty() {
        None
    } else {
        Some(removed.join(", "))
    }
}

async fn delete_action(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteActionQuery>,
) -> Response {
    let config_path = state.get_config_path();
    if config_path.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "config_path 未初始化"})),
        )
            .into_response();
    }
    let mut new_config = state.config.load().as_ref().clone();
    let Some(action) = new_config.find_action(&id).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    // 从配置移除
    new_config.actions.retain(|a| a.id != id);
    if let Err(e) = new_config.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }
    if let Err(e) = persist_config(&state, &new_config, actions_patch(&new_config)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    // 配置已落盘，再删磁盘上的 sh 脚本（失败只告警，不回滚配置）
    let removed_script = remove_action_script(&new_config, &action);
    // 若要求删除版本目录：必须在配置写回成功、且校验通过之后执行，
    // 避免「删除目录先于 validate/persist」导致非法配置被拒绝时目录已不可逆删除（CWE-73）。
    // 删除失败要上报，不再吞错误。
    if q.remove_dir {
        if let Some(vcfg) = &action.versioning {
            if vcfg.enabled && !vcfg.base_dir.trim().is_empty() {
                if let Err(e) = std::fs::remove_dir_all(&vcfg.base_dir) {
                    tracing::error!(action = %id, dir = %vcfg.base_dir, error = %e, "删除版本目录失败");
                }
            }
        }
    }
    // 清理运行状态
    state.unmark_running(&id).await;
    state.last_active_versions.lock().await.remove(&id);
    state.config.store(std::sync::Arc::new(new_config));
    crate::scheduler::sync_all(&state).await;

    // 记录删除操作到日志（审计留痕）
    {
        let tz_offset = crate::time_util::parse_timezone(&state.config.load().default_timezone)
            .unwrap_or_else(|_| chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        let entry = crate::log::make_operation_log(
            state.log_next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            "delete",
            &id,
            &action.title,
            &action.group,
            &format!("已删除脚本「{}」", action.title),
            &tz_offset,
        );
        // 锁内只做内存环形缓冲操作；落盘放到锁外。
        {
            let mut logs = state.logs.lock().await;
            logs.push_back(entry.clone());
            let capacity = state.config.load().log_capacity;
            while logs.len() > capacity {
                logs.pop_front();
            }
        }
        // 写文件日志
        let cfg = state.config.load();
        if let Some(log_dir) = &cfg.log_directory {
            crate::log::write_script_log(&state, &entry, log_dir, &cfg.default_timezone).await;
            crate::log::append_total_log(log_dir, &entry);
        }
    }

    tracing::info!(action = %id, "脚本已删除");
    Json(json!({"ok": true, "removed_script": removed_script})).into_response()
}

// ============ 服务器上已有脚本：读取内容 / 列目录 ============
//
// 面板的文件选择器只能拿到「浏览器所在电脑」上的文件，路由器上的 .sh 没法直接选。
// 这两个接口补上这个缺口：
//   - inspect：读一个服务器路径的脚本内容（用于预览、或转成面板托管的命令）
//   - list   ：列某个目录下的 .sh/.bash（用于点选，免得手打路径）

/// 单次可读取的最大脚本字节数（够覆盖正常脚本，避免误读大文件卡住浏览器）。
const SCRIPT_READ_MAX_BYTES: u64 = 512 * 1024;
/// 目录列表最多返回条数。
const SCRIPT_LIST_LIMIT: usize = 500;

fn is_script_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".sh") || lower.ends_with(".bash")
}


/// 校验要读取的脚本路径：绝对路径 + .sh/.bash + 不含 `..`。
/// 抽成纯函数便于单测（后台接口和前端提示用同一套规则）。
fn validate_script_read_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("脚本路径不能为空".to_string());
    }
    if p.contains("..") {
        return Err("脚本路径不能包含 `..`".to_string());
    }
    let path = std::path::Path::new(p);
    if !path.is_absolute() {
        return Err("脚本路径必须是绝对路径（以 / 开头）".to_string());
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(n) if is_script_ext(n) => {}
        _ => return Err("只支持 .sh / .bash 脚本".to_string()),
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
fn script_is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn script_is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptReadBody {
    #[serde(default)]
    path: String,
}

/// POST /api/scripts/inspect —— 读服务器上某个 .sh 的内容与状态。
async fn inspect_script(
    State(_state): State<AppState>,
    Json(body): Json<ScriptReadBody>,
) -> Response {
    let path = match validate_script_read_path(&body.path) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    let display = path.to_string_lossy().to_string();
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Json(json!({
                "path": display, "exists": false,
                "message": "文件不存在（可先创建，或改用「编辑命令内容」模式）"
            }))
            .into_response()
        }
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("读取文件信息失败: {e}")})),
            )
                .into_response()
        }
    };
    if !meta.is_file() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "目标不是普通文件"}))).into_response();
    }
    let size = meta.len();
    let truncated = size > SCRIPT_READ_MAX_BYTES;
    // 符号链接绕过防护：canonicalize 后若目标不再是 .sh/.bash（如指向 /etc/passwd），
    // 说明字符串后缀检查被 symlink 绕过，拒绝读取（CWE-200）。
    if let Ok(real) = std::fs::canonicalize(&path) {
        match real.file_name().and_then(|n| n.to_str()) {
            Some(n) if is_script_ext(n) => {}
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "符号链接目标不是脚本文件"})),
                )
                    .into_response();
            }
        }
    }
    // 流式读取并按上限截断：不再 std::fs::read 全量载入，避免大文件整段进内存（CWE-770）。
    let mut bytes: Vec<u8> = Vec::with_capacity(
        (size.min(SCRIPT_READ_MAX_BYTES) as usize).min(SCRIPT_READ_MAX_BYTES as usize),
    );
    {
        use std::io::Read;
        match std::fs::File::open(&path) {
            Ok(f) => {
                let mut limited = f.take(SCRIPT_READ_MAX_BYTES + 1);
                if let Err(e) = limited.read_to_end(&mut bytes) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("读取文件失败: {e}")})),
                    )
                        .into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!("读取文件失败: {e}")})),
                )
                    .into_response();
            }
        }
    }
    let take = bytes.len().min(SCRIPT_READ_MAX_BYTES as usize);
    let slice = &bytes[..take];
    if slice.contains(&0u8) {
        // 含 NUL 说明不是文本脚本，读出来只会是乱码
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "目标不是文本脚本（含二进制内容）"})),
        )
            .into_response();
    }
    let content = String::from_utf8_lossy(slice).to_string();
    let shebang = content
        .lines()
        .next()
        .filter(|l| l.starts_with("#!"))
        .map(|l| l.trim().to_string());
    Json(json!({
        "path": display,
        "exists": true,
        "is_file": true,
        "size": size,
        "truncated": truncated,
        "executable": script_is_executable(&path),
        "shebang": shebang,
        "content": content,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptListBody {
    #[serde(default)]
    dir: String,
}

/// POST /api/scripts/list —— 列目录下的 .sh/.bash（不递归）。
async fn list_scripts(
    State(state): State<AppState>,
    Json(body): Json<ScriptListBody>,
) -> Response {
    let cfg = state.config.load();
    let dir_raw = body.dir.trim();
    let dir = if dir_raw.is_empty() {
        crate::config::script_directory(cfg.script_directory.as_deref()).to_string()
    } else {
        dir_raw.to_string()
    };
    if dir.contains("..") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "目录不能包含 `..`"}))).into_response();
    }
    if !std::path::Path::new(&dir).is_absolute() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "目录必须是绝对路径"}))).into_response();
    }
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("打开目录失败: {e}"), "dir": dir})),
            )
                .into_response()
        }
    };
    let mut items: Vec<serde_json::Value> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_script_ext(&name) {
            continue;
        }
        let p = entry.path();
        let meta = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        items.push(json!({
            "name": name,
            "path": p.to_string_lossy(),
            "size": meta.len(),
            "executable": script_is_executable(&p),
        }));
        if items.len() >= SCRIPT_LIST_LIMIT {
            break;
        }
    }
    items.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
    });
    Json(json!({"dir": dir, "items": items})).into_response()
}

async fn list_actions(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.config.load();
    let mut groups: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for action in config.actions.iter() {
        if !action.enabled {
            continue;
        }
        let params = params_for(&state, action).await;
        // 未启用版本管理时跳过 read_link：原先对 Path::new("") 做一次必然失败的 syscall。
        let active_version = action
            .versioning
            .as_ref()
            .filter(|v| v.enabled)
            .and_then(|v| {
                crate::versioning::read_current_link(std::path::Path::new(&v.base_dir)).ok()
            });
        let upload_enabled = action
            .versioning
            .as_ref()
            .map(|v| v.upload.enabled)
            .unwrap_or(false);
        let state_str = state.action_state(&action.id).await;
        let schedule_json = schedule_json(action, &config);
        let entry = json!({
            "id": action.id,
            "title": action.title,
            "enabled": action.enabled,
            "group": action.group,
            "description": action.description,
            "state": state_str,
            "concurrency": action.concurrency,
            "active_version": active_version,
            "upload_enabled": upload_enabled,
            "params": params,
            "subcommands": crate::executor::effective_subcommands(&state, &action).await,
            "args": action.args,
            "discover": discover_json(&state, &action.id).await,
            "schedule": schedule_json,
            // 是否为「命令写入 .sh」型：卡片删除确认要据此提示会连带删除脚本文件
            "has_script": action.uses_shell_script(),
            "script_path": action
                .uses_shell_script()
                .then(|| action.script_path(config.script_directory.as_deref()).to_string_lossy().to_string()),
            // 引用型：脚本在服务器上，面板不写它、删除时也不删它
            "script_ref": action.script_ref(),
        });
        groups
            .entry(action.group.clone())
            .or_default()
            .push(entry);
    }
    // 按设置里的 group_order 排列分组：已列出的按自定义顺序，
    // 未列出的按名称字典序排在后面。
    let mut ordered: Vec<(String, Vec<serde_json::Value>)> = groups.into_iter().collect();
    let order = &config.group_order;
    ordered.sort_by(|(a, _), (b, _)| {
        match (
            order.iter().position(|g| g == a),
            order.iter().position(|g| g == b),
        ) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
    let groups_json: Vec<serde_json::Value> = ordered
        .into_iter()
        .map(|(name, actions)| json!({"name": name, "actions": actions}))
        .collect();
    Json(json!({
        "groups": groups_json,
        "queued_total": state.queued.load(std::sync::atomic::Ordering::Relaxed),
    }))
}

/// 保存分组排列顺序（分组管理弹窗里的上移/下移）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupOrderRequest {
    #[serde(default)]
    order: Vec<String>,
}

async fn update_group_order(
    State(state): State<AppState>,
    Json(body): Json<GroupOrderRequest>,
) -> Response {
    let mut new_config = state.config.load().as_ref().clone();
    new_config.group_order = body.order;
    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e, "message": e})),
        )
            .into_response();
    }
    if let Err(e) = persist_config(
        &state,
        &new_config,
        ConfigPatch::default().scalar("group_order"),
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    state.config.store(std::sync::Arc::new(new_config));
    Json(json!({"ok": true})).into_response()
}

async fn params_for(state: &AppState, action: &Action) -> Vec<serde_json::Value> {
    // 手动 args 优先；发现未完成时返回手动
    let discovered = state.discovered.load();
    let disc_params = discovered
        .get(&action.id)
        .map(|d| d.params.clone())
        .unwrap_or_default();
    let merged = crate::discover::merge_params(&action.args, &disc_params);
    merged
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "description": p.description,
                "required": p.required,
                "default": p.default,
                "type": p.r#type,
                "style": p.style,
                "options": p.options,
            })
        })
        .collect()
}

async fn discover_json(state: &AppState, action_id: &str) -> serde_json::Value {
    let discovered = state.discovered.load();
    match discovered.get(action_id) {
        Some(d) => json!({
            "status": "ok",
            "error": null,
            "discovered_at": d.discovered_at,
        }),
        None => {
            // 判断是否在发现中
            let action = state.config.load().find_action(action_id).cloned();
            let enabled = action
                .and_then(|a| a.discover)
                .map(|d| d.enabled)
                .unwrap_or(false);
            if enabled {
                json!({"status": "discovering", "error": null, "discovered_at": null})
            } else {
                json!({"status": "ok", "error": null, "discovered_at": null})
            }
        }
    }
}

fn schedule_json(action: &Action, config: &crate::config::Config) -> serde_json::Value {
    match &action.schedule {
        Some(s) if s.enabled => {
            let desc = crate::scheduler::describe(s);
            let tz = s
                .timezone
                .clone()
                .unwrap_or_else(|| config.default_timezone.clone());
            let next = crate::scheduler::compile_cron(s)
                .ok()
                .and_then(|expr| crate::scheduler::next_run_cached(&expr, &tz, chrono::Utc::now()).ok());
            let next_local = next
                .as_ref()
                .and_then(|t| {
                    crate::time_util::parse_timezone(&tz)
                        .ok()
                        .map(|off| crate::time_util::format_local(t, &off))
                });
            // 跨时区时 next_run_human 追加 (tz)
            let schedule_tz = s.timezone.clone().unwrap_or_default();
            let cross_tz = !schedule_tz.is_empty() && schedule_tz != config.default_timezone;
            let next_human = if cross_tz {
                next_local
                    .as_ref()
                    .map(|n| format!("{n} ({tz})"))
            } else {
                next_local.clone()
            };
            json!({
                "enabled": true,
                "mode": s.mode,
                "description": desc,
                "timezone": tz,
                "next_run_at": next.map(|t| t.to_rfc3339()),
                "next_run_local": next_local,
                "next_run_schedule_tz": s.timezone,
                "next_run_human": next_human,
            })
        }
        _ => json!(null),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    #[serde(default)]
    args: std::collections::HashMap<String, String>,
    /// 预设子命令（必须在该 action 的 subcommands 列表内，否则忽略）
    #[serde(default)]
    subcommand: Option<String>,
}

async fn run_action(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RunRequest>,
) -> Response {
    // 速率限制（手动触发）
    {
        let mut rl = state.rate_limiter.lock().await;
        if !rl.allow() {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "60")],
                Json(json!({"error": "rate_limited"})),
            )
                .into_response();
        }
    }
    // 前置校验：查找 Action、enabled、running
    let config = state.config.load_full();
    let action = match config.find_action(&id) {
        Some(a) => a.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "not_found", "message": "脚本不存在"})),
            )
                .into_response();
        }
    };
    if !action.enabled {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "disabled", "message": "脚本已禁用"})),
        )
            .into_response();
    }
    if state.is_action_running(&id).await {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "already_running", "message": "脚本正在运行中"})),
        )
            .into_response();
    }

    // remote_addr：trust_proxy=true 时读 X-Forwarded-For，否则读 X-Real-IP
    let remote_addr = extract_remote_addr(&state, &headers);

    // 异步执行：立即返回 202，前端轮询日志获取结果，执行期间可点「停止」
    let st = state.clone();
    let id2 = id.clone();
    let args = body.args;
    let subcommand = body.subcommand;
    tokio::spawn(async move {
        // 不再静默丢弃 RunError：执行失败至少落一条 error 日志，
        // 否则「执行失败用户完全不可见、日志页无记录」（CWE-390）。
        if let Err(e) = crate::executor::run_action(
            &st,
            &id2,
            crate::log::TriggerSource::Manual,
            args,
            remote_addr,
            crate::executor::RunMode::Normal,
            subcommand,
        )
        .await
        {
            tracing::error!(action = %id2, error = ?e, "手动执行失败");
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({"accepted": true, "action_id": id})),
    )
        .into_response()
}

/// 停止正在运行的脚本（按进程组 kill）。
async fn stop_action(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let pgid = state.running_pids.lock().await.get(&id).copied();
    if let Some(pgid) = pgid {
        // 运行中：按进程组 kill，并清掉可能残留的取消标志。
        crate::process::kill_process_group(pgid);
        state.unmark_running(&id).await;
        state.running_pids.lock().await.remove(&id);
        state.cancel_flags.lock().await.remove(&id);
        tracing::info!(action = %id, pgid = pgid, "脚本已停止");
        return Json(json!({"ok": true, "stopped": "running"})).into_response();
    }
    // running 集合里有、但 running_pids 里没有 ⇒ 正在排队等信号量。
    if state.is_action_running(&id).await {
        state.cancel_flags.lock().await.insert(id.clone());
        let _ = state.cancel_tx.send(id.clone());
        tracing::info!(action = %id, "排队任务取消信号已发送");
        return Json(json!({"ok": true, "stopped": "queued"})).into_response();
    }
    (
        StatusCode::CONFLICT,
        Json(json!({"error": "not_running", "message": "脚本未在运行"})),
    )
        .into_response()
}

/// 提取客户端来源地址：trust_proxy=true 时读 X-Forwarded-For（取第一个），否则读 X-Real-IP。
fn extract_remote_addr(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let trust_proxy = state.config.load().auth.trust_proxy;
    if trust_proxy {
        if let Some(v) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            // X-Forwarded-For 是逗号分隔的链，取第一个（真实客户端）
            let first = v.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsQuery {
    limit: Option<usize>,
    before_id: Option<u64>,
    group: Option<String>,
    trigger: Option<String>,
    action_id: Option<String>,
}

async fn list_logs(
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> Json<serde_json::Value> {
    // limit=0 会让游标失效，这里保证至少取 1 条。
    let limit = q.limit.unwrap_or(50).min(500).max(1);
    // VecDeque 按 id 升序（尾部最新）：倒序遍历取够 limit+1 条即停。
    // 不再全量 clone + 全量排序，也不在持锁状态下做 JSON 序列化。
    let (items, has_more) = {
        let logs = state.logs.lock().await;
        let mut items: Vec<crate::log::LogEntry> = Vec::with_capacity(limit + 1);
        for e in logs.iter().rev() {
            if let Some(aid) = &q.action_id {
                if &e.action_id != aid { continue; }
            }
            if let Some(g) = &q.group {
                if &e.action_group != g { continue; }
            }
            if let Some(t) = &q.trigger {
                let tstr = match e.trigger {
                    crate::log::TriggerSource::Manual => "manual",
                    crate::log::TriggerSource::Schedule => "schedule",
                };
                if tstr != t { continue; }
            }
            if let Some(bid) = q.before_id {
                if e.id >= bid { continue; }
            }
            items.push(e.clone());
            if items.len() > limit { break; }
        }
        let has_more = items.len() > limit;
        items.truncate(limit);
        (items, has_more)
    };
    let next_before_id = if has_more { items.last().map(|e| e.id) } else { None };
    Json(json!({
        "items": items,
        "has_more": has_more,
        "next_before_id": next_before_id,
    }))
}

/// POST /api/logs/clear —— 删除日志目录下的所有 .log 文件。
///
/// 只删「目录下、直接子级、扩展名 .log」的普通文件，不递归、不碰子目录与其它文件。
/// 删除前先 reset 写入器句柄：否则进程仍握着已 unlink 的 inode，
/// 文件删了却不会重建，后续日志写进虚空。
async fn clear_log_files(State(state): State<AppState>) -> Response {
    let dir = match state.config.load().log_directory.clone() {
        Some(d) if !d.trim().is_empty() => d,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "未配置 log_directory，无法清理"})),
            )
                .into_response()
        }
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("打开日志目录失败: {e}"), "dir": dir})),
            )
                .into_response()
        }
    };

    // 先放掉句柄，再删文件；之后写审计日志会重新创建 panel-*.log
    crate::tracing_writer::reset_current();
    crate::log::invalidate_usage_cache();

    let mut removed = 0u64;
    let mut freed = 0u64;
    let mut errors: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let is_log = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("log"))
            .unwrap_or(false);
        if !is_log || !path.is_file() {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(_) => {
                removed += 1;
                freed += size;
            }
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }

    // 同步清空内存里的日志环形缓冲。
    // 只删文件不清内存的话：日志页仍会显示旧记录（看起来「没清干净」），
    // 而重启后又突然全部消失，行为前后不一致。
    // 注意**不重置** log_next_id：id 必须单调递增，否则新日志会复用前端已渲染过的 id，
    // 分页/去重逻辑就会把新记录当成重复项。
    let cleared = {
        let mut logs = state.logs.lock().await;
        let n = logs.len();
        logs.clear();
        n
    };

    tracing::info!(removed, freed_bytes = freed, cleared, dir = %dir, "已清除日志文件与内存记录");

    Json(json!({
        "ok": errors.is_empty(),
        "dir": dir,
        "removed": removed,
        "freed_bytes": freed,
        "cleared_entries": cleared,
        "errors": errors,
    }))
    .into_response()
}

// ============ 版本管理 ============

async fn list_versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let config = state.config.load();
    let Some(action) = config.find_action(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(vcfg) = &action.versioning else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    if !vcfg.enabled {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "versioning_disabled"}))).into_response();
    }
    match crate::versioning::scan(action) {
        Ok(versions) => {
            let active = crate::versioning::read_current_link(std::path::Path::new(&vcfg.base_dir))
                .unwrap_or_default();
            let has_previous = state
                .last_active_versions
                .lock()
                .await
                .contains_key(&id);
            Json(json!({
                "active": active,
                "versions": versions,
                "has_previous": has_previous,
                "upload_enabled": vcfg.upload.enabled,
                "max_versions": config.max_versions,
                "version_count": versions.len(),
                // action 登记的配置文件名；每个版本目录里各自带一套
                "config_files": vcfg.all_config_files(),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

async fn upload_version(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    // 速率限制
    {
        let mut rl = state.rate_limiter.lock().await;
        if !rl.allow() {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "rate_limited"})),
            )
                .into_response();
        }
    }
    // CSRF 已在 auth_middleware 统一校验；这里只提取 remote_addr
    let remote_addr = headers
        .get("x-forwarded-for")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    match crate::upload::handle_upload(&state, &id, multipart, remote_addr).await {
        Ok(resp) => (
            StatusCode::CREATED,
            [(CACHE_CONTROL, "no-store")],
            Json(json!(resp)),
        )
            .into_response(),
        Err(e) => {
            let status = e.http_status();
            (status, Json(json!({"error": e.code(), "message": e.message()}))).into_response()
        }
    }
}

async fn activate_version(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Response {
    let config = state.config.load();
    let Some(action) = config.find_action(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(vcfg) = action.versioning.clone() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let base = std::path::Path::new(&vcfg.base_dir);
    // 版本名校验：拒绝 `..`、路径分隔符等，防止 current 软链被劫持到任意目录
    if !crate::versioning::is_valid_version_name(&name) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_version_name"}))).into_response();
    }
    // 校验版本存在且 binary 可执行
    let binary = base.join(&name).join(&vcfg.binary_name);
    if !binary.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "version_not_found"}))).into_response();
    }
    let old = crate::versioning::read_current_link(base).ok();
    if let Err(e) = crate::versioning::set_current_link(base, &name) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    if let Some(o) = old {
        if o != name {
            state
                .last_active_versions
                .lock()
                .await
                .insert(id.clone(), o);
        }
    }
    // current 切换后，执行器的工作目录 {base_dir}/current 随之指向新版本，
    // 脚本读到的是该版本目录里的整套配置 —— 这里把实际生效的配置集回显给面板。
    let switched_files: Vec<String> = vcfg
        .all_config_files()
        .into_iter()
        .filter(|f| base.join(&name).join(f).is_file())
        .collect();
    tracing::info!(
        action = %id,
        version = %name,
        config_count = switched_files.len(),
        "版本已切换，对应配置集已生效"
    );
    // 切换后触发重新发现
    let state2 = state.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        crate::discover::run_and_cache(&state2, &id2).await;
    });
    Json(json!({"active": name, "config_files": switched_files})).into_response()
}

// ============ 脚本配置读取 / 编辑（按版本目录，支持多配置文件） ============

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileQuery {
    #[serde(default)]
    file: Option<String>,
}

/// 选出本次要读写的配置文件：优先指定文件名，且必须是 action 已登记的名字。
/// 配置写在 {base_dir}/current/ 下，所以切换版本后读到的就是那一版自己的配置。
fn pick_config_file(
    vcfg: &crate::config::VersioningConfig,
    requested: Option<&str>,
) -> Result<String, String> {
    let files = vcfg.all_config_files();
    if let Some(f) = requested {
        if !files.iter().any(|x| x == f) {
            return Err(format!("未登记的配置文件: {f}"));
        }
        return Ok(f.to_string());
    }
    files
        .first()
        .cloned()
        .ok_or_else(|| "config_name_not_configured".to_string())
}

/// 读取当前版本目录里指定配置文件的内容，并返回该 action 登记的全部配置文件名。
async fn get_action_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ConfigFileQuery>,
) -> Response {
    let config = state.config.load();
    let Some(action) = config.find_action(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(vcfg) = &action.versioning else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "versioning_not_configured"})),
        )
            .into_response();
    };
    let config_name = match pick_config_file(vcfg, q.file.as_deref()) {
        Ok(n) => n,
        Err(e) => {
            let status = if e == "config_name_not_configured" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            return (status, Json(json!({"error": e}))).into_response();
        }
    };
    let base = std::path::Path::new(&vcfg.base_dir);
    let active = match crate::versioning::read_current_link(base) {
        Ok(a) if crate::versioning::is_valid_version_name(&a) => a,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "current_link_invalid"})),
            )
                .into_response();
        }
    };
    let config_path = base.join(&active).join(&config_name);
    match std::fs::read_to_string(&config_path) {
        Ok(content) => Json(json!({
            "version": active,
            "config_name": config_name,
            "config_files": vcfg.all_config_files(),
            "content": content,
        }))
        .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "config_file_missing", "version": active, "file": config_name})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigUpdateRequest {
    content: String,
    #[serde(default)]
    file: Option<String>,
}

/// 写回当前版本目录里指定配置文件；只影响当前版本，其它版本的配置不受影响。
async fn update_action_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ConfigUpdateRequest>,
) -> Response {
    let config = state.config.load();
    let Some(action) = config.find_action(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(vcfg) = &action.versioning else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "versioning_not_configured"})),
        )
            .into_response();
    };
    let config_name = match pick_config_file(vcfg, body.file.as_deref()) {
        Ok(n) => n,
        Err(e) => {
            let status = if e == "config_name_not_configured" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            return (status, Json(json!({"error": e}))).into_response();
        }
    };
    let base = std::path::Path::new(&vcfg.base_dir);
    let active = match crate::versioning::read_current_link(base) {
        Ok(a) if crate::versioning::is_valid_version_name(&a) => a,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "current_link_invalid"})),
            )
                .into_response();
        }
    };
    let config_path = base.join(&active).join(&config_name);
    if let Err(e) = crate::log::write_file_atomic(&config_path, &body.content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("写配置失败: {e}")})),
        )
            .into_response();
    }
    tracing::info!(action = %id, version = %active, file = %config_name, "版本内配置文件已更新");
    Json(json!({"ok": true, "version": active, "config_name": config_name})).into_response()
}

async fn rollback_version(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let prev = {
        let map = state.last_active_versions.lock().await;
        map.get(&id).cloned()
    };
    let Some(prev) = prev else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no_previous_version"})),
        )
            .into_response();
    };
    let config = state.config.load();
    let Some(action) = config.find_action(&id) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let Some(vcfg) = action.versioning.clone() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    };
    let base = std::path::Path::new(&vcfg.base_dir);
    // 回退目标版本名同样必须合法，否则 current 软链可被劫持
    if !crate::versioning::is_valid_version_name(&prev) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_version_name"}))).into_response();
    }
    let binary = base.join(&prev).join(&vcfg.binary_name);
    if !binary.is_file() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "version_not_found"}))).into_response();
    }
    let cur = crate::versioning::read_current_link(base).ok();
    if let Err(e) = crate::versioning::set_current_link(base, &prev) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    if let Some(ref c) = cur {
        state
            .last_active_versions
            .lock()
            .await
            .insert(id.clone(), c.clone());
    }
    let switched_files: Vec<String> = vcfg
        .all_config_files()
        .into_iter()
        .filter(|f| base.join(&prev).join(f).is_file())
        .collect();
    tracing::info!(
        action = %id,
        version = %prev,
        config_count = switched_files.len(),
        "已回退版本，对应配置集已生效"
    );
    Json(json!({
        "active": prev,
        "rolled_back_from": cur,
        "config_files": switched_files,
    }))
    .into_response()
}

async fn trigger_discover(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let state2 = state.clone();
    let id2 = id.clone();
    tokio::spawn(async move {
        crate::discover::run_and_cache(&state2, &id2).await;
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "discovering"})),
    )
        .into_response()
}

// ============ Token 重置 ============

async fn reset_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if !state.config.load().auth.enabled {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "auth_disabled",
                "message": "安全模式未开启，请先在设置页开启后再重置 Token"
            })),
        )
            .into_response();
    }
    let new_token = match crate::token::generate_token() {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e})),
            )
                .into_response();
        }
    };
    let token_file = state
        .config
        .load()
        .auth
        .token_file
        .clone()
        .unwrap_or_default();
    let path = std::path::Path::new(&token_file);
    if let Err(e) = crate::token::write_token_atomic(path, &new_token) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }
    // 更新内存 Token
    state.current_token.store(std::sync::Arc::new(new_token.clone()));
    // 设置新 cookie（统一用 make_auth_cookie，避免硬编码语义漂移）
    let cookie = crate::auth::make_auth_cookie(
        &new_token,
        state.config.load().auth.cookie_ttl_hours,
    );
    tracing::warn!(remote_addr = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("unknown"), "auth token reset");

    let mut resp = Json(json!({"token": new_token, "note": "仅本次返回，请立即保存"})).into_response();
    resp.headers_mut().insert(
        CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    resp.headers_mut().insert(
        SET_COOKIE,
        axum::http::HeaderValue::from_str(&cookie).unwrap_or_else(|_| axum::http::HeaderValue::from_static("")),
    );
    resp
}

// ============ 设置（日志保留 + 推送） ============

/// 脱敏：非空则显示前 4 位 + ***，空则 null。
fn mask_secret(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|v| {
        if v.is_empty() {
            "".to_string()
        } else if v.len() <= 4 {
            "***".to_string()
        } else {
            format!("{}***", &v[..4])
        }
    })
}

async fn get_settings(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.config.load();
    let log_dir = config.log_directory.clone();
    let log_retention_days = config.log_retention_days;
    let push = &config.push;
    // 每次请求实时统计日志目录占用，保证日志增长后前端数值准确
    let (log_usage_bytes, log_usage_files) = log_dir
        .as_deref()
        .map(|d| crate::log::log_directory_usage_cached(std::path::Path::new(d)))
        .unwrap_or((0, 0));
    Json(json!({
        "log_directory": log_dir,
        "script_directory": crate::config::script_directory(config.script_directory.as_deref()),
        "log_retention_days": log_retention_days,
        "log_max_total_mb": config.log_max_total_mb,
        "log_usage_bytes": log_usage_bytes,
        "log_usage_files": log_usage_files,
        "log_usage_rule": "1 KB = 1024 B，1 MB = 1024 KB；仅统计 .log 文件，30 秒内复用缓存",
        "min_free_mb": config.min_free_mb,
        "max_versions": config.max_versions,
        "versioning_enabled": config.versioning_enabled,
        "max_versions_min": 1,
        "max_versions_max": 50,
        "auth_enabled": config.auth.enabled,
        "collapse_groups_by_default": config.collapse_groups_by_default,
        "group_layout": config.group_layout,
        "group_panel_position": config.group_panel_position,
        "group_order": config.group_order,
        "push": {
            "enabled": push.enabled,
            "spt": mask_secret(&push.spt),
            "spt_configured": push.spt.is_some(),
            "app_token": mask_secret(&push.app_token),
            "app_token_configured": push.app_token.is_some(),
            "uids": push.uids.clone().unwrap_or_default().join(","),
            "include_output": push.include_output,
            "summary_mode": push.summary_mode,
        },
    }))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<crate::config::SettingsUpdate>,
) -> Response {
    let config_path = state.get_config_path();
    if config_path.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "config_path 未初始化"})),
        )
            .into_response();
    }

    // 读当前配置 → clone → 应用更新
    let mut new_config = state.config.load().as_ref().clone();
    let was_auth_enabled = new_config.auth.enabled;
    new_config.apply_settings(&body);

    // 校验：非法 max_versions / 界面枚举在这里被拦截，
    // 返回可直接展示给用户的中文说明（前端读 message 字段）。
    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e, "message": e})),
        )
            .into_response();
    }

    // 推送校验：启用推送但未配置任何凭据时拒绝
    if new_config.push.enabled
        && new_config.push.spt.is_none()
        && new_config.push.app_token.is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "push_missing_credentials",
                "message": "已启用推送，但未配置极简 SPT 或应用 AppToken，请至少填写一种"
            })),
        )
            .into_response();
    }

    // 安全模式由「关」切到「开」：先生成并落盘 Token。
    // 否则一切换，当前会话会被自己的认证中间件挡在门外。
    let turning_on_auth = !was_auth_enabled && new_config.auth.enabled;
    let generated_token: Option<String> = if turning_on_auth {
        let t = match crate::token::generate_token() {
            Ok(t) => t,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "token_generate_failed", "message": e})),
                )
                    .into_response();
            }
        };
        let tf = new_config.auth.token_file.clone().unwrap_or_default();
        if let Err(e) = crate::token::write_token_atomic(std::path::Path::new(&tf), &t) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "token_write_failed",
                    "message": format!("开启安全模式失败：无法写入 Token 文件 {tf}：{e}")
                })),
            )
                .into_response();
        }
        Some(t)
    } else {
        None
    };

    // 写回：最小差异补丁（保留注释 / 未知字段 / action），写前回读校验
    if let Err(e) = persist_config(&state, &new_config, body.to_patch()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        )
            .into_response();
    }

    // 热加载：store 新配置 + 重建调度循环
    if let Some(t) = &generated_token {
        state.current_token.store(std::sync::Arc::new(t.clone()));
    }
    state.config.store(std::sync::Arc::new(new_config));
    crate::scheduler::sync_all(&state).await;

    // 版本上限变更：立即对所有启用版本管理的脚本清理超量旧版本
    if body.max_versions.is_some() {
        let cfg = state.config.load_full();
        let previous_map = state.last_active_versions.lock().await.clone();
        for a in cfg.actions.iter() {
            if let Some(v) = &a.versioning {
                if !v.enabled {
                    continue;
                }
                let base = std::path::Path::new(&v.base_dir);
                let mut keep = Vec::new();
                if let Ok(active) = crate::versioning::read_current_link(base) {
                    keep.push(active);
                }
                if let Some(prev) = previous_map.get(&a.id) {
                    keep.push(prev.clone());
                }
                match crate::versioning::enforce_version_limit(
                    base,
                    cfg.max_versions as usize,
                    &keep,
                ) {
                    Ok(list) if !list.is_empty() => tracing::info!(
                        action = %a.id, count = list.len(), max = cfg.max_versions,
                        "设置更新后清理超量历史版本"
                    ),
                    Err(e) => tracing::warn!(action = %a.id, error = %e, "版本数量清理失败"),
                    _ => {}
                }
            }
        }
    }

    // 若日志大小/保留限制变更，立即执行一次清理
    if let Some(log_dir) = state.config.load().log_directory.clone() {
        let cfg = state.config.load();
        let dir = std::path::Path::new(&log_dir);
        let (by_retention, by_size) = crate::log::cleanup_log_directory(
            dir,
            cfg.log_retention_days,
            cfg.log_max_total_mb,
        );
        if !by_retention.is_empty() {
            tracing::info!(count = by_retention.len(), "设置更新后清理超期日志");
        }
        if !by_size.is_empty() {
            tracing::info!(count = by_size.len(), "设置更新后清理超额日志");
        }
    }

    tracing::info!("设置已更新并持久化");

    let mut resp = Json(json!({
        "ok": true,
        "auth_enabled": state.config.load().auth.enabled,
        "new_token": generated_token,
    }))
    .into_response();
    // 开启安全模式时给当前浏览器种 cookie，保证本次会话不被中断
    if let Some(t) = &generated_token {
        let cookie = crate::auth::make_auth_cookie(
            t,
            state.config.load().auth.cookie_ttl_hours,
        );
        if let Ok(v) = axum::http::HeaderValue::from_str(&cookie) {
            resp.headers_mut().insert(SET_COOKIE, v);
        }
    }
    resp
}

/// 发送测试推送，验证通道配置是否正确。
async fn test_push(State(state): State<AppState>) -> Response {
    let config = state.config.load();
    let push_cfg = config.push.clone();
    if !push_cfg.enabled {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "push_disabled", "message": "推送未启用，请先启用推送"})),
        )
            .into_response();
    }
    if push_cfg.spt.is_none() && push_cfg.app_token.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "push_missing_credentials", "message": "未配置 SPT 或 AppToken"})),
        )
            .into_response();
    }
    match crate::push::send_test(&push_cfg).await {
        Ok(()) => Json(json!({"ok": true, "message": "测试推送已发送"})).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "send_failed", "message": format!("发送失败: {e}")})),
        )
            .into_response(),
    }
}

// ============ 健康检查 / 版本 ============

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    // 收敛返回字段：healthz 是公网可达的存活探测，不应暴露 config_version /
    // scheduler_count / running_actions 等内部状态（轻微信息泄露）。只保留存活语义。
    let uptime = (chrono::Utc::now() - state.start_time).num_seconds();
    Json(json!({
        "status": "ok",
        "uptime_secs": uptime,
    }))
}

async fn api_version() -> Json<serde_json::Value> {
    Json(json!({
        // 与页面顶栏徽标共用同一来源，避免接口返回 0.2.0、页面显示 v0.2.0 的漂移。
        "version": templates::version(),
        "git_commit": env!("GIT_COMMIT"),
        "rustc": env!("RUSTC_VERSION"),
        "built_at": env!("BUILD_TIME"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_version_label_matches_topbar() {
        // 接口、顶栏徽标、设置页关于卡片必须共用同一格式。
        assert_eq!(templates::version(), format!("v{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn prepare_action_script_writes_file_and_fixes_exec() {
        let dir = std::env::temp_dir().join(format!(
            "olivetin-script-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let dir_str = dir.to_string_lossy().to_string();
        let mut a: crate::config::Action = serde_json::from_value(json!({
            "id": "led-on",
            "title": "led",
            "exec": [],
            "command": "echo 1 > /tmp/brightness"
        }))
        .unwrap();
        let got = prepare_action_script(&mut a, Some(&dir_str))
            .unwrap()
            .unwrap();
        assert_eq!(a.exec, vec!["/bin/sh".to_string(), got.clone()]);
        assert_eq!(
            std::fs::read_to_string(&got).unwrap(),
            "#!/bin/sh\necho 1 > /tmp/brightness\n"
        );
        a.script_name = Some("my-led".into());
        let got2 = prepare_action_script(&mut a, Some(&dir_str))
            .unwrap()
            .unwrap();
        assert!(got2.ends_with("/my-led.sh"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_action_script_noop_for_legacy_exec() {
        let mut a: crate::config::Action = serde_json::from_value(json!({
            "id": "legacy", "title": "legacy", "exec": ["/bin/echo", "hi"]
        }))
        .unwrap();
        assert!(prepare_action_script(&mut a, None).unwrap().is_none());
        assert_eq!(a.exec, vec!["/bin/echo", "hi"]);
    }

    fn vcfg_with_configs() -> crate::config::VersioningConfig {
        crate::config::VersioningConfig {
            enabled: true,
            base_dir: "/opt/x".into(),
            binary_name: "wb".into(),
            config_name: Some("wb-signin.json".into()),
            config_files: vec!["xiaoning.json".into(), "xiaoyu.json".into()],
            upload: Default::default(),
        }
    }

    #[test]
    fn pick_config_file_only_allows_registered_names() {
        let v = vcfg_with_configs();
        assert_eq!(pick_config_file(&v, None).unwrap(), "wb-signin.json");
        assert_eq!(
            pick_config_file(&v, Some("xiaoyu.json")).unwrap(),
            "xiaoyu.json"
        );
        assert!(pick_config_file(&v, Some("../evil.json")).is_err());
    }

    #[test]
    fn pick_config_file_reports_when_no_config_registered() {
        let mut v = vcfg_with_configs();
        v.config_name = None;
        v.config_files.clear();
        assert_eq!(
            pick_config_file(&v, None).unwrap_err(),
            "config_name_not_configured"
        );
    }

    // ===== 服务器上已有脚本：路径校验 & 引用识别 =====

    #[test]
    fn validate_script_read_path_accepts_only_absolute_sh() {
        assert_eq!(
            validate_script_read_path("  /opt/olivetin/led-on.sh  ").unwrap(),
            std::path::PathBuf::from("/opt/olivetin/led-on.sh")
        );
        assert!(validate_script_read_path("/opt/x/a.bash").is_ok());
        // 相对路径
        assert!(validate_script_read_path("opt/x.sh").unwrap_err().contains("绝对路径"));
        // 非脚本后缀
        assert!(validate_script_read_path("/opt/x.txt").unwrap_err().contains(".sh"));
        // 路径穿越
        assert!(validate_script_read_path("/opt/../etc/x.sh").unwrap_err().contains(".."));
        // 空
        assert!(validate_script_read_path("   ").unwrap_err().contains("不能为空"));
    }

    #[test]
    fn action_script_ref_detects_reference_forms() {
        // 旧式直接执行：exec 只有一个 .sh
        let a: crate::config::Action = serde_json::from_value(json!({
            "id": "led", "title": "led", "exec": ["/opt/olivetin/led-on.sh"]
        }))
        .unwrap();
        assert_eq!(a.script_ref().as_deref(), Some("/opt/olivetin/led-on.sh"));

        // /bin/sh 前缀形式
        let b: crate::config::Action = serde_json::from_value(json!({
            "id": "led2", "title": "led2", "exec": ["/bin/sh", "/opt/olivetin/led-off.sh"]
        }))
        .unwrap();
        assert_eq!(b.script_ref().as_deref(), Some("/opt/olivetin/led-off.sh"));

        // command 型（面板托管）不算引用
        let c: crate::config::Action = serde_json::from_value(json!({
            "id": "c", "title": "c", "exec": ["/bin/sh", "/opt/x.sh"], "command": "echo hi"
        }))
        .unwrap();
        assert_eq!(c.script_ref(), None);

        // 普通二进制执行不算引用
        let d: crate::config::Action = serde_json::from_value(json!({
            "id": "d", "title": "d", "exec": ["/bin/echo", "hi"]
        }))
        .unwrap();
        assert_eq!(d.script_ref(), None);
    }
}
