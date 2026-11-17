//! 日志：内存环形缓冲 + 原子文件持久化 + 保留清理 + 游标分页 + 存储保护。
//!
//! 内存日志用 `VecDeque<LogEntry>`，id 自增。文件日志（`log_directory` 非 null 时）
//! 采用 `.tmp` + rename 原子写入，写前做磁盘空间保护。

use serde::{Deserialize, Serialize};

use crate::push::PushStatus;

/// 触发来源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TriggerSource {
    Manual,
    Schedule,
}

/// 单条日志（执行日志 + 操作审计日志）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: u64,
    pub action_id: String,
    pub action_title: String,
    pub action_group: String,
    pub started_at: String,
    pub started_at_local: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub trigger: TriggerSource,
    pub push_status: PushStatus,
    pub remote_addr: Option<String>,
    pub action_version: Option<String>,
    /// 日志类型：run（脚本执行，默认）/ delete（删除脚本）/ create（新增脚本）/ update（编辑脚本）。
    #[serde(default = "default_kind")]
    pub kind: String,
    /// 本次执行实际用到的 sh 脚本完整路径（command 型 action 才有）。
    /// 直接显示在日志/详情里，方便确认脚本落盘位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
}

fn default_kind() -> String {
    "run".to_string()
}

/// 构造一条操作审计日志（删除/新增/编辑脚本）。
/// `kind` 取值：create / update / delete；`desc` 为操作描述文本。
pub fn make_operation_log(
    id: u64,
    kind: &str,
    action_id: &str,
    action_title: &str,
    action_group: &str,
    desc: &str,
    tz_offset: &chrono::FixedOffset,
) -> LogEntry {
    let now = chrono::Utc::now();
    LogEntry {
        id,
        action_id: action_id.to_string(),
        action_title: action_title.to_string(),
        action_group: action_group.to_string(),
        started_at: now.to_rfc3339(),
        started_at_local: crate::time_util::format_local(&now, tz_offset),
        duration_ms: 0,
        exit_code: Some(0),
        stdout: desc.to_string(),
        stderr: String::new(),
        truncated: false,
        timed_out: false,
        trigger: TriggerSource::Manual,
        push_status: PushStatus::Disabled,
        remote_addr: None,
        action_version: None,
        kind: kind.to_string(),
        script_path: None,
    }
}

/// 日志文件头（脚本日志）。
pub fn log_file_header(entry: &LogEntry, timezone: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("action: {}\n", entry.action_title));
    out.push_str(&format!("action_id: {}\n", entry.action_id));
    out.push_str(&format!("group: {}\n", entry.action_group));
    out.push_str(&format!(
        "version: {}\n",
        entry.action_version.as_deref().unwrap_or("none")
    ));
    if let Some(p) = &entry.script_path {
        out.push_str(&format!("script_path: {p}\n"));
    }
    out.push_str(&format!(
        "trigger: {}\n",
        match entry.trigger {
            TriggerSource::Manual => "manual",
            TriggerSource::Schedule => "schedule",
        }
    ));
    out.push_str(&format!("started_at: {}\n", entry.started_at));
    out.push_str(&format!("timezone: {}\n", timezone));
    out.push_str(&format!(
        "exit_code: {}\n",
        entry
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "none".to_string())
    ));
    out.push_str(&format!("timed_out: {}\n", entry.timed_out));
    out.push_str(&format!("truncated: {}\n", entry.truncated));
    out.push_str(&format!(
        "push_status: {}\n",
        match &entry.push_status {
            PushStatus::Disabled => "disabled",
            PushStatus::Ok => "ok",
            PushStatus::Failed(_) => "failed",
        }
    ));
    out.push_str(&format!(
        "remote_addr: {}\n",
        entry.remote_addr.as_deref().unwrap_or("none")
    ));
    out.push_str("--- stdout ---\n");
    out.push_str(&entry.stdout);
    out.push_str("\n--- stderr ---\n");
    out.push_str(&entry.stderr);
    out
}

/// statvfs 结果 5 秒内复用：同一分钟内磁盘剩余空间不会突变。
static FREE_SPACE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Option<(std::time::Instant, bool)>>,
> = std::sync::OnceLock::new();
const FREE_SPACE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// 磁盘空间保护：检查剩余空间是否低于阈值。
/// `statvfs` 通过 libc 调用，Unix 专用；非 Unix 返回 true（跳过检查）。
pub fn enough_free_space(path: &std::path::Path, min_free_mb: u64) -> bool {
    if min_free_mb == 0 {
        return true;
    }
    let cache = FREE_SPACE_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, ok)) = *guard {
        if at.elapsed() < FREE_SPACE_TTL {
            return ok;
        }
    }
    let ok = enough_free_space_uncached(path, min_free_mb);
    *guard = Some((std::time::Instant::now(), ok));
    ok
}

fn enough_free_space_uncached(path: &std::path::Path, min_free_mb: u64) -> bool {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        // 路径含 NUL 字节时 CString 构造失败：这是非法路径，磁盘保护无法判定，
        // 保守拒绝（fail-closed），不再退化成空串导致 statvfs 对错误路径判空。
        let c_path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(path = %path.display(), "路径含 NUL 字节，磁盘空间检查拒绝放行");
                return false;
            }
        };
        // SAFETY: `stat` 是栈上未初始化的 libc::statvfs 缓冲区，`statvfs` 由 libc
        // 填充其全部字段后才可读；这里在 ret == 0（成功）时才读取其字段，
        // 成功路径下缓冲区已被完全初始化，无未初始化内存读取。
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret != 0 {
            // statvfs 失败：磁盘保护无法判定，保守拒绝（fail-closed），
            // 避免「磁盘已满但保护静默失效」导致写盘失败无预警（CWE-252）。
            let err = std::io::Error::last_os_error();
            tracing::warn!(path = %path.display(), error = %err, "statvfs 失败，磁盘空间检查拒绝放行");
            return false;
        }
        let free_bytes = (stat.f_bavail as u64) * (stat.f_frsize as u64);
        free_bytes >= min_free_mb * 1024 * 1024
    }
    #[cfg(not(unix))]
    {
        let _ = (path, min_free_mb);
        true
    }
}

/// 写脚本日志文件（原子写入 + 存储保护 + 保留清理）。
/// 文件名：`{action_id}.log`（一个脚本一个文件，追加式）。
/// 每次执行前追加一行执行时间分隔，便于按时间查找。
pub async fn write_script_log(
    state: &crate::state::AppState,
    entry: &LogEntry,
    log_dir: &str,
    timezone: &str,
) {
    let config = state.config.load();
    let dir = std::path::Path::new(log_dir);
    // 磁盘空间保护
    if !enough_free_space(dir, config.min_free_mb) {
        if state
            .warn_throttled("log_disk_space", 3600)
            .await
        {
            tracing::warn!("磁盘空间不足，跳过写脚本日志");
        }
        return;
    }
    // 一个脚本一个文件：{action_id}.log
    let filename = format!("{}.log", entry.action_id);
    let path = dir.join(filename);

    // 构造追加内容：执行时间分隔 + 头信息
    let mut content = String::new();
    content.push_str(&format!(
        "\n========== {} ==========\n",
        entry.started_at_local
    ));
    content.push_str(&log_file_header(entry, timezone));
    content.push('\n');

    // 追加写入（文件不存在则创建）
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(content.as_bytes()) {
                tracing::warn!(error = %e, "写脚本日志失败");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "打开脚本日志文件失败");
        }
    }
}

/// 总日志持久化文件：`{log_dir}/total.log`（JSON Lines，每条一行）。
pub const TOTAL_LOG_FILENAME: &str = "total.log";

/// 追加一条日志到总日志持久化文件（JSON Lines，追加式）。
/// 重启后 load_total_logs 从此文件读回历史。
pub fn append_total_log(log_dir: &str, entry: &LogEntry) {
    let path = std::path::Path::new(log_dir).join(TOTAL_LOG_FILENAME);
    invalidate_usage_cache();
    // 大小轮转：total.log 持续被写、mtime 永远最新，清理逻辑几乎删不到它。
    // 超过 8MB 就切成 total.1.log，避免无限增长与启动期大文件读取。
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 8 * 1024 * 1024 {
            let rotated = std::path::Path::new(log_dir).join("total.1.log");
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(&path, &rotated);
        }
    }
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(_) => return,
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// 从总日志文件读回历史（最近 `limit` 条）。
/// 返回 Vec<LogEntry>，按 id 升序（旧在前）。损坏的行跳过。
pub fn load_total_logs(log_dir: &str, limit: usize) -> Vec<LogEntry> {
    use std::io::BufRead;
    let path = std::path::Path::new(log_dir).join(TOTAL_LOG_FILENAME);
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    // 流式扫描 + 固定容量环形缓冲：内存恒为 O(limit × 单条)，
    // 不再把整个文件 read_to_string 后解析全部历史行。
    let mut ring: std::collections::VecDeque<LogEntry> =
        std::collections::VecDeque::with_capacity(limit.min(1024));
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<LogEntry>(&line) {
            if limit > 0 && ring.len() >= limit {
                ring.pop_front();
            }
            ring.push_back(e);
        }
    }
    ring.into_iter().collect()
}

/// 原子写入文件：先写 `.tmp`，成功后 rename。失败仅返回错误，不 panic。
pub fn write_file_atomic(path: &std::path::Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// 确保日志目录存在（创建缺失目录）。
pub fn ensure_log_dir(dir: &std::path::Path) -> Result<(), String> {
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 按保留天数清理超期日志文件（含面板日志 panel-*.log 与脚本日志 *.log）。
/// `retention_days = 0` 或 `None` 表示不按时间清理。
pub fn cleanup_by_retention(dir: &std::path::Path, retention_days: Option<u32>) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(days) = retention_days else {
        return removed;
    };
    if days == 0 {
        return removed;
    }
    let now = std::time::SystemTime::now();
    let cutoff = now - std::time::Duration::from_secs(days as u64 * 86400);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return removed,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // 只清理 .log 文件（面板日志与脚本日志都 .log 结尾）
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".log") {
            continue;
        }
        // 跳过正在写入的临时文件
        if name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if modified < cutoff {
            if std::fs::remove_file(&path).is_ok() {
                removed.push(name);
            }
        }
    }
    removed
}

/// 按总大小配额清理日志目录：超 `max_total_mb` 时按 mtime 从最旧开始删除，直到低于配额。
/// `max_total_mb = 0` 表示不按大小清理。
pub fn cleanup_by_size(dir: &std::path::Path, max_total_mb: u64) -> Vec<String> {
    let mut removed = Vec::new();
    if max_total_mb == 0 {
        return removed;
    }
    let max_bytes = max_total_mb * 1024 * 1024;
    // 收集所有 .log 文件及其 mtime
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return removed,
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".log") || name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let size = meta.len();
        total += size;
        files.push((path, mtime, size));
    }
    if total <= max_bytes {
        return removed;
    }
    // 按 mtime 升序（最旧在前）删除，直到低于配额
    files.sort_by_key(|(_, mtime, _)| *mtime);
    for (path, _, size) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            removed.push(
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
        }
    }
    removed
}

/// 日志目录当前占用：返回 (总字节数, 文件数)。
/// 口径与 cleanup_by_retention / cleanup_by_size 完全一致：
/// 占用统计的 TTL：设置页每 30 秒刷新一次即可，避免每请求全目录 stat。
const USAGE_TTL: std::time::Duration = std::time::Duration::from_secs(30);
static USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, u64, usize)>>> =
    std::sync::OnceLock::new();

/// 清除日志 / 写日志后让缓存立即失效，保证下次读取重新扫盘。
pub fn invalidate_usage_cache() {
    if let Some(m) = USAGE_CACHE.get() {
        *m.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// 只统计目录下 `.log` 结尾、且非 `.tmp` 的普通文件（面板日志 + 各脚本日志 + total.log）。
/// 保持实时语义：清理逻辑与单测依赖「写完文件立刻读到新值」。
pub fn log_directory_usage(dir: &std::path::Path) -> (u64, usize) {
    log_directory_usage_uncached(dir)
}

/// 设置页展示专用：30 秒 TTL 缓存，避免 /api/settings 每请求全目录 read_dir + N 次 stat。
/// 清除日志后由 invalidate_usage_cache() 主动失效。
pub fn log_directory_usage_cached(dir: &std::path::Path) -> (u64, usize) {
    let cache = USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, bytes, files)) = *guard {
        if at.elapsed() < USAGE_TTL {
            return (bytes, files);
        }
    }
    let (bytes, files) = log_directory_usage_uncached(dir);
    *guard = Some((std::time::Instant::now(), bytes, files));
    (bytes, files)
}

fn log_directory_usage_uncached(dir: &std::path::Path) -> (u64, usize) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".log") || name.ends_with(".tmp") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            bytes = bytes.saturating_add(meta.len());
            files += 1;
        }
    }
    (bytes, files)
}

/// 综合清理：先按保留天数，再按大小配额。返回被删的文件名列表。
pub fn cleanup_log_directory(
    dir: &std::path::Path,
    retention_days: Option<u32>,
    max_total_mb: u64,
) -> (Vec<String>, Vec<String>) {
    let by_retention = cleanup_by_retention(dir, retention_days);
    let by_size = cleanup_by_size(dir, max_total_mb);
    (by_retention, by_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_usage_only_counts_log_files_and_tracks_growth() {
        let dir = std::env::temp_dir().join(format!(
            "olivetin-usage-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("total.log"), "12345").unwrap();
        std::fs::write(dir.join("panel-20261112.log"), "abc").unwrap();
        std::fs::write(dir.join("note.txt"), "ignored").unwrap();
        std::fs::write(dir.join("wip.log.tmp"), "ignored").unwrap();
        assert_eq!(log_directory_usage(&dir), (8, 2));

        // 模拟日志增长：追加后数值必须同步变大
        std::fs::write(dir.join("total.log"), "1234567890").unwrap();
        assert_eq!(log_directory_usage(&dir), (13, 2));
        std::fs::write(dir.join("script-a.log"), "xy").unwrap();
        assert_eq!(log_directory_usage(&dir), (15, 3));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_usage_missing_directory_is_zero() {
        assert_eq!(
            log_directory_usage(std::path::Path::new("/definitely/not/here")),
            (0, 0)
        );
    }

    #[test]
    fn enough_free_space_zero_threshold() {
        // 阈值 0 表示不检查
        assert!(enough_free_space(std::path::Path::new("/tmp"), 0));
    }

    #[test]
    fn header_contains_expected_fields() {
        let e = LogEntry {
            id: 1,
            action_id: "a".into(),
            action_title: "t".into(),
            action_group: "g".into(),
            started_at: "now".into(),
            started_at_local: "now".into(),
            duration_ms: 10,
            exit_code: Some(0),
            stdout: "out".into(),
            stderr: "err".into(),
            truncated: false,
            timed_out: false,
            trigger: TriggerSource::Manual,
            push_status: PushStatus::Ok,
            remote_addr: Some("1.2.3.4".into()),
            action_version: Some(crate::versioning::FIRST_VERSION.into()),
            kind: "run".into(),
            script_path: None,
        };
        let h = log_file_header(&e, "+08:00");
        assert!(h.contains("action: t"));
        assert!(h.contains("version: v0.1"));
        assert!(h.contains("--- stdout ---"));
    }
}
