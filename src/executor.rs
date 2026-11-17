//! 执行器：run_action 编排，流式截断、超时、进程组、推送、TZ 注入、
//! 单 Action 并发、版本解析。

use std::collections::HashMap;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::log::{LogEntry, TriggerSource};
use crate::push::PushStatus;
use crate::state::AppState;
use crate::time_util::{format_local, parse_timezone};
use crate::versioning;

/// 把 `action.command` 写入 sh 脚本文件。
///
/// 路径规则见 `config::Action::script_path`：
/// - 配了 `script_full_path` 时用它（绝对路径，未填 `.sh` 自动补）；
/// - 否则落在 `script_directory`（默认 /opt/olivetin/scripts），
///   文件名为 `script_name`，留空时是 `{action.id}.sh`；
/// - 内容是 shebang + 命令原文（自带 shebang 时不重复添加，CRLF 统一为 LF）。
/// 每次执行前重写，保证脚本与当前配置一致（手动改过 config.yaml 也生效）。
/// 用原子写避免并发执行时读到写了一半的脚本。
pub fn write_shell_script(
    action: &crate::config::Action,
    configured_dir: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let raw = action.script_path(configured_dir);
    // 防御：相对路径按进程工作目录补全，保证 /bin/sh 拿到的一定是可定位的路径，
    // 脚本里的 $0 也就是这个绝对路径。
    let path = if raw.is_absolute() {
        raw
    } else {
        std::env::current_dir()
            .map_err(|e| format!("无法获取工作目录: {e}"))?
            .join(raw)
    };
    let content = action.script_content().ok_or("命令不能为空")?;
    let dir = path.parent().ok_or("脚本目录无效")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建脚本目录失败: {e}"))?;
    crate::log::write_file_atomic(&path, &content)
        .map_err(|e| format!("写入 sh 脚本失败: {e}"))?;
    Ok(path)
}

/// 运行错误。
#[derive(Debug)]
pub enum RunError {
    NotFound,
    Disabled,
    AlreadyRunning,
    InvalidArgs(String),
    SpawnFailed(String),
    Timeout,
    VersionUnavailable(String),
    /// 排队阶段被 stop_action 取消，未启动子进程。
    Cancelled,
}

impl RunError {
    pub fn http_status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            RunError::NotFound => StatusCode::NOT_FOUND,
            RunError::Disabled => StatusCode::NOT_FOUND,
            RunError::AlreadyRunning => StatusCode::CONFLICT,
            RunError::InvalidArgs(_) => StatusCode::BAD_REQUEST,
            RunError::SpawnFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RunError::Timeout => StatusCode::REQUEST_TIMEOUT,
            RunError::VersionUnavailable(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RunError::Cancelled => StatusCode::CONFLICT,
        }
    }
}

/// 解析 active 版本名。
fn resolve_version(action: &crate::config::Action) -> Result<Option<String>, RunError> {
    if let Some(v) = &action.versioning {
        if v.enabled {
            return versioning::read_current_link(std::path::Path::new(&v.base_dir))
                .map(Some)
                .map_err(|e| RunError::VersionUnavailable(e));
        }
    }
    Ok(None)
}

/// 校验手动参数并组装命令参数。
fn build_args(
    action: &crate::config::Action,
    extra_args: &HashMap<String, String>,
) -> Result<Vec<String>, RunError> {
    let mut out: Vec<String> = Vec::new();
    // 手动参数定义顺序
    for p in &action.args {
        let val = extra_args
            .get(&p.name)
            .or(p.default.as_ref())
            .map(|s| s.clone());
        if p.required && val.is_none() {
            return Err(RunError::InvalidArgs(format!("缺少必需参数: {}", p.name)));
        }
        if let Some(v) = val {
            // 白名单 + 长度 + 字符集校验
            if v.len() > 1024 {
                return Err(RunError::InvalidArgs(format!("参数 {} 过长", p.name)));
            }
            if v.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
                return Err(RunError::InvalidArgs(format!("参数 {} 含非法字符", p.name)));
            }
            match p.style.as_str() {
                "positional" => out.push(v),
                "flag" => {
                    if v == "true" || v == "1" {
                        out.push(format!("--{}", p.name));
                    }
                }
                _ => {
                    out.push(format!("--{}", p.name));
                    out.push(v);
                }
            }
        }
    }
    Ok(out)
}

/// 流式读取一个流（stdout/stderr），截断到 `max_bytes`，返回 (buf, truncated)。
async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    stream: Option<R>,
    max_bytes: usize,
) -> (Vec<u8>, bool) {
    let Some(mut r) = stream else {
        return (Vec::new(), false);
    };
    let mut buf = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let n = match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if buf.len() < max_bytes {
            let take = (max_bytes - buf.len()).min(n);
            buf.extend_from_slice(&chunk[..take]);
        } else {
            truncated = true;
        }
    }
    (buf, truncated)
}

/// 执行一个 Action，返回 LogEntry。
/// 等待针对本 action 的取消广播；其它 action 的取消消息忽略。
/// Lagged 时可能错过发给自己的取消消息，因此每次 Lagged 都重新核对取消标志，
/// 而不是简单 `continue`（否则排队任务可能无法取消）。
async fn wait_cancel(
    rx: &mut tokio::sync::broadcast::Receiver<String>,
    id: &str,
    state: &AppState,
) {
    loop {
        match rx.recv().await {
            Ok(v) if v == id => return,
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                // 落后了：可能漏掉发给自己的取消，重新核对标志
                if state.cancel_flags.lock().await.contains(id) {
                    return;
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// 取消后统一清理：running 集合 / 取消标志 / 进程组记录都不能残留。
async fn cleanup_cancelled(state: &AppState, id: &str) {
    state.unmark_running(id).await;
    state.cancel_flags.lock().await.remove(id);
    state.running_pids.lock().await.remove(id);
}

pub async fn run_action(
    state: &AppState,
    action_id: &str,
    trigger: TriggerSource,
    extra_args: HashMap<String, String>,
    remote_addr: Option<String>,
    mode: RunMode,
    subcommand: Option<String>,
) -> Result<LogEntry, RunError> {
    // 克隆 Arc<Config>，避免 arc_swap Guard 跨 await 导致的 Send 约束
    let config: std::sync::Arc<crate::config::Config> = state.config.load_full();
    let action = config
        .find_action(action_id)
        .ok_or(RunError::NotFound)?
        .clone();
    if !action.enabled {
        return Err(RunError::Disabled);
    }

    // versioning 解析 active 版本
    // 全局开关关闭时，版本管理整体不生效（按普通 exec 执行）
    let active_version = if config.versioning_enabled {
        resolve_version(&action)?
    } else {
        None
    };

    // running 集合
    if !state.mark_running(action_id).await {
        return Err(RunError::AlreadyRunning);
    }

    // 参数组装
    let args = match build_args(&action, &extra_args) {
        Ok(a) => a,
        Err(e) => {
            state.unmark_running(action_id).await;
            return Err(e);
        }
    };

    // 获取信号量（全局 + Action 级）。排队阶段可被 stop_action 取消：
    // 两个 acquire 合并成一个 future，与取消广播一起 select。
    // load_full() 取当前快照的 Arc<Semaphore>；热加载替换快照后新任务立即用新上限。
    let global_sem = state.semaphore.load_full();
    let action_sem = {
        let map = state.action_semaphores.load();
        map.get(action_id)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)))
    };
    let mut cancel_rx = state.cancel_tx.subscribe();
    // 排队计数：进入等待信号量阶段 +1，拿到 permit 或取消后 -1。
    // 使 healthz / list_actions 返回的 queued 真实反映排队任务数（之前恒为 0）。
    state.queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let permits = tokio::select! {
        pair = async {
            let g = global_sem.acquire_owned().await;
            let a = action_sem.acquire_owned().await;
            (g, a)
        } => pair,
        _ = wait_cancel(&mut cancel_rx, action_id, state) => {
            state.queued.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            cleanup_cancelled(state, action_id).await;
            tracing::info!(action = %action_id, "排队任务已取消");
            return Err(RunError::Cancelled);
        }
    };
    state.queued.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    let _global_permit = match permits.0 {
        Ok(p) => p,
        Err(_) => {
            cleanup_cancelled(state, action_id).await;
            return Err(RunError::SpawnFailed("semaphore 关闭".to_string()));
        }
    };
    let _action_permit = match permits.1 {
        Ok(p) => p,
        Err(_) => {
            cleanup_cancelled(state, action_id).await;
            return Err(RunError::SpawnFailed("semaphore 关闭".to_string()));
        }
    };
    // 覆盖「刚拿到 permit、还没 spawn」的窄窗口。
    if state.cancel_flags.lock().await.contains(action_id) {
        cleanup_cancelled(state, action_id).await;
        tracing::info!(action = %action_id, "排队任务在启动前被取消");
        return Err(RunError::Cancelled);
    }

    let started = chrono::Utc::now();
    let tz_offset = parse_timezone(&config.default_timezone).unwrap_or_else(|_| {
        chrono::FixedOffset::east_opt(8 * 3600).unwrap()
    });

    // 组装命令：带 command 的 action 先把命令写入 sh 脚本，再由 /bin/sh 执行该脚本。
    // 这样重定向、管道、变量展开等 shell 语法全部生效，输出仍由下面的流式读取回显。
    let script_path = if action.uses_shell_script() {
        match write_shell_script(&action, config.script_directory.as_deref()) {
            Ok(p) => Some(p),
            Err(e) => {
                state.unmark_running(action_id).await;
                return Err(RunError::SpawnFailed(e));
            }
        }
    } else {
        None
    };

    let mut cmd = match &script_path {
        Some(p) => {
            let mut c = Command::new("/bin/sh");
            c.arg(p);
            c
        }
        None => {
            let mut c = Command::new(&action.exec[0]);
            c.args(&action.exec[1..]);
            c
        }
    };
    // 本次执行真正用到的脚本文件：
    // - 面板托管型 → 刚写出的 {script_directory}/{name}.sh
    // - 引用型     → 服务器上那个已有文件（面板没写它）
    // 统一用它注入 $OLIVETIN_SCRIPT_* 并回传日志，两种模式行为一致。
    let effective_script: Option<std::path::PathBuf> = script_path
        .clone()
        .or_else(|| action.script_ref().map(std::path::PathBuf::from));
    // 参数顺序：子命令 → 自定义参数
    // 子命令来自下拉（可能是 --help 自动解析出来的，未必出现在配置里），
    // 因此校验「形状」而不是白名单：单个、以 - 开头、不含空白、≤64 字符。
    if let Some(sub) = subcommand.as_deref() {
        let sub = sub.trim();
        if !sub.is_empty() {
            // 白名单 = 下拉里实际可选项（手填 + --help 自动解析），
            // 请求体里塞列表外的参数一律忽略，避免绕过 UI 注入任意参数
            let allowed = effective_subcommands(state, &action).await;
            if allowed.iter().any(|x| x == sub) {
                cmd.arg(sub);
            } else {
                tracing::warn!(action = %action.id, %sub, "子命令不在可选列表内，已忽略");
            }
        }
    }
    // 手动参数追加在脚本路径之后，脚本里可用 "$@" 取得
    cmd.args(&args);


    // 版本管理：把工作目录设为 {base_dir}/current。
    // 每个版本目录里放「二进制 + 该版本的一整套配置」，脚本用相对路径读同目录
    // JSON 时，切换 current 软链接后下一次执行就会自动加载新版本的配置，
    // 不需要修改 exec、参数或环境变量。
    if let Some(v) = &action.versioning {
        if v.enabled && config.versioning_enabled {
            let cwd = std::path::Path::new(&v.base_dir).join("current");
            if cwd.is_dir() {
                cmd.current_dir(&cwd);
            } else {
                tracing::warn!(
                    action = %action.id,
                    path = %cwd.display(),
                    "版本目录 current 不存在，按默认工作目录执行"
                );
            }
        }
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    // TZ 注入：inject_tz 时先注入 TZ=CST-8，再应用 action.env（action.env 覆盖）
    if config.inject_tz {
        cmd.env("TZ", "CST-8");
    }
    // 脚本自身路径注入：脚本里可以直接 echo "$0" 或 "$OLIVETIN_SCRIPT_PATH"
    // 得到自己的完整路径（用于定位同目录的配置/资源，或把路径打进输出）。
    // 注意放在 action.env 之前，action.env 仍可覆盖。
    if let Some(p) = &effective_script {
        cmd.env("OLIVETIN_SCRIPT_PATH", p);
        if let Some(d) = p.parent() {
            cmd.env("OLIVETIN_SCRIPT_DIR", d);
        }
        if let Some(n) = p.file_name() {
            cmd.env("OLIVETIN_SCRIPT_NAME", n);
        }
    }
    cmd.env("OLIVETIN_ACTION_ID", &action.id);
    cmd.env("OLIVETIN_ACTION_TITLE", &action.title);
    // 推送凭据注入：脚本里可以直接 curl WxPusher 发自定义内容，不必把 spt/appToken 写死在脚本中。
    // 按需注入：仅当该 action 开启推送（action.push）时才注入全局凭据，
    // 避免「不推送的脚本也能读到全局推送凭据」导致凭据随日志/输出外泄（CWE-522/CWE-532）。
    if action.push {
        if let Some(spt) = &config.push.spt {
            cmd.env("OLIVETIN_PUSH_SPT", spt);
        }
        if let Some(tok) = &config.push.app_token {
            cmd.env("OLIVETIN_PUSH_APP_TOKEN", tok);
        }
        if let Some(uids) = &config.push.uids {
            cmd.env("OLIVETIN_PUSH_UIDS", uids.join(","));
        }
        if let Some(topics) = &config.push.topic_ids {
            let joined: Vec<String> = topics.iter().map(|t| t.to_string()).collect();
            cmd.env("OLIVETIN_PUSH_TOPIC_IDS", joined.join(","));
        }
    }
    for (k, v) in &action.env {
        cmd.env(k, v);
    }

    crate::process::setup_process_group(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            state.unmark_running(action_id).await;
            return Err(RunError::SpawnFailed(e.to_string()));
        }
    };

    let pgid = match child.id() {
        Some(id) => id as i32,
        None => {
            // 获取 pid 失败：清理 running 状态并回收子进程，避免状态残留与僵尸进程。
            cleanup_cancelled(state, action_id).await;
            let _ = child.kill().await;
            return Err(RunError::SpawnFailed("无法获取子进程 id".to_string()));
        }
    };

    // 记录进程组 id，供停止功能使用
    {
        state.running_pids.lock().await.insert(action_id.to_string(), pgid);
    }

    // 流式读取 stdout/stderr 并截断
    let max_bytes = config.max_output_kb * 1024;
    let stdout_opt = child.stdout.take();
    let stderr_opt = child.stderr.take();

    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let mut truncated = false;
    let mut timed_out = false;
    let mut exit_code: Option<i32> = None;

    let timeout_dur = std::time::Duration::from_secs(action.timeout);
    let fut = async {
        // 并发读 stdout 与 stderr，各自返回 (buf, truncated)
        let out_fut = read_stream(stdout_opt, max_bytes);
        let err_fut = read_stream(stderr_opt, max_bytes);
        let wait_fut = async {
            let status = child.wait().await;
            exit_code = status.ok().map(|s| s.code().unwrap_or(-1));
        };
        let (o, e, _) = tokio::join!(out_fut, err_fut, wait_fut);
        out_buf = o.0;
        err_buf = e.0;
        truncated = o.1 || e.1;
    };

    match tokio::time::timeout(timeout_dur, fut).await {
        Ok(_) => {}
        Err(_) => {
            timed_out = true;
            exit_code = None;
            crate::process::kill_process_group(pgid);
            let _ = child.wait().await;
        }
    }

    let duration_ms = (chrono::Utc::now() - started).num_milliseconds() as u64;

    state.unmark_running(action_id).await;
    state.running_pids.lock().await.remove(action_id);
    // 正常完成路径也要清理取消标志：若执行期间收到过 stop 但已越过
    // 「245 行检查点」、任务正常结束，cancel_flags 会残留到下次执行，
    // 使下一次执行在启动前被误判取消（状态残留）。
    state.cancel_flags.lock().await.remove(action_id);

    // 构造 LogEntry
    let entry = LogEntry {
        id: state.log_next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        action_id: action.id.clone(),
        action_title: action.title.clone(),
        action_group: action.group.clone(),
        started_at: started.to_rfc3339(),
        started_at_local: format_local(&started, &tz_offset),
        duration_ms,
        exit_code,
        // into_owned(): 合法 UTF-8 时直接复用 Cow::Owned，少一次整串拷贝。
        stdout: String::from_utf8_lossy(&out_buf).into_owned(),
        stderr: String::from_utf8_lossy(&err_buf).into_owned(),
        truncated,
        timed_out,
        trigger: trigger.clone(),
        push_status: PushStatus::Disabled,
        remote_addr,
        action_version: active_version,
        kind: "run".to_string(),
        // 脚本路径随日志一起回传，前端在结果摘要里直接显示
        script_path: effective_script
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    };

    // 写入环形缓冲
    {
        let mut logs = state.logs.lock().await;
        logs.push_back(entry.clone());
        let capacity = config.log_capacity;
        while logs.len() > capacity {
            logs.pop_front();
        }
    }

    // 可选写日志文件（脚本日志 + 总日志持久化）
    if let Some(log_dir) = &config.log_directory {
        crate::log::write_script_log(state, &entry, log_dir, &config.default_timezone).await;
        crate::log::append_total_log(log_dir, &entry);
    }

    // 推送（异步）：仅 action.push 开启，且满足 push_condition 时触发。
    // 把 action 一起传下去：脚本级可覆盖标题/正文/接收者/contentType。
    if mode == RunMode::Normal
        && action.push
        && push_should_fire(&action.push_condition, entry.exit_code, &entry.stdout)
    {
        crate::push::dispatch(state, entry.id, entry.clone(), action.clone()).await;
    }

    Ok(entry)
}

/// 执行模式：Normal 走日志 + 推送；TestOnly 只执行（用于推送测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Normal,
    TestOnly,
}

/// 判断是否按条件推送。
/// - always / on_failure / on_success：按退出码判断（None 视为失败）。
/// - on_marker：脚本输出含 `[PUSH]` 标记时推送（脚本自主决定，与退出码无关）。
/// 判断是否按条件推送。
/// - always / on_failure / on_success：按退出码判断（None 视为失败）。
/// - on_marker：脚本输出含 `[PUSH]` 标记时推送（脚本自主决定，与退出码无关）。
/// 把「自由参数」文本拆成 argv：空白分隔，支持单/双引号包裹与 `\` 转义。
///
/// **不经过 shell**：拆出来的结果直接作为独立参数传给子进程，
/// 所以 `a b "c d"` 会变成三个参数，`; rm -rf /` 也只是普通字符串而已。
pub fn split_free_args(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                _ if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                _ => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// help/usage 这类参数不作为子命令展示（用户从 help 里"选 --help"没有意义）。
pub fn is_help_like(s: &str) -> bool {
    let t = s.trim().trim_start_matches('-').to_ascii_lowercase();
    t == "help" || t == "h" || t == "usage" || t.starts_with("help-") || t.ends_with("-help")
}

/// 下拉里的子命令 = 用户手填的 + 从 `--help` 自动解析出的 flag 参数（过滤 help 自身）。
///
/// 顺序：先用户填的（他显式指定的优先级高），再补上自动解析出来的，去重。
/// 只收 flag 类型：option 类参数需要跟值，放进"点一下就执行"的下拉里没法用。
pub async fn effective_subcommands(
    state: &AppState,
    action: &crate::config::Action,
) -> Vec<String> {
    // 显式配置过（哪怕是空数组）就完全听用户的：删掉的项不会再被自动解析补回来
    if let Some(list) = &action.subcommands {
        let mut out: Vec<String> = Vec::new();
        for s in list {
            let t = s.trim();
            if !t.is_empty() && !is_help_like(t) && !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        return out;
    }

    let mut out: Vec<String> = Vec::new();
    {
        let discovered = state.discovered.load();
        if let Some(d) = discovered.get(&action.id) {
            for p in &d.params {
                if p.style != "flag" || is_help_like(&p.name) {
                    continue;
                }
                let flag = format!("--{}", p.name.trim_start_matches('-'));
                if !out.iter().any(|x| x == &flag) {
                    out.push(flag);
                }
            }
        }
    }
    out
}

fn push_should_fire(condition: &str, exit_code: Option<i32>, stdout: &str) -> bool {
    match condition {
        "on_failure" => exit_code != Some(0),
        "on_success" => exit_code == Some(0),
        "on_marker" => stdout.contains("[PUSH]"),
        _ => true, // always 或缺省
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_required_missing() {
        let action = crate::config::Action {
            id: "a".into(),
            title: "t".into(),
            enabled: true,
            group: "g".into(),
            exec: vec!["/bin/true".into()],
            command: None,
            script_name: None,
            script_full_path: None,
            timeout: 10,
            concurrency: 1,
            description: None,
            push: true,
            push_condition: "always".into(),
            push_title: None,
            push_content: None,
            push_uids: None,
            push_topic_ids: None,
            push_content_type: None,
            env: Default::default(),
            versioning: None,
            schedule: None,
            args: vec![crate::config::ParamDef {
                name: "x".into(),
                description: None,
                required: true,
                default: None,
                r#type: "string".into(),
                style: "option".into(),
                options: None,
            }],
            discover: None,
            subcommands: None,
        };
        let empty = HashMap::new();
        assert!(matches!(build_args(&action, &empty), Err(RunError::InvalidArgs(_))));
    }
}
