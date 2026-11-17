//! 【v13】面板内版本上传：multipart 解析、流式写盘、原子 rename、自动切换、触发发现。
//!
//! 严格顺序：认证/CSRF/速率限制 → 校验 upload.enabled → 校验版本名 → 校验目标目录
//! 不存在 → 锁 → 临时目录 → 流式写 binary/config → statvfs → chmod +x → 写元数据
//! → 原子 rename → 自动切换 → 触发重新发现。

use axum::extract::Multipart;
use std::path::Path;

use crate::config::Action;
use crate::discover::DiscoverState;
use crate::state::AppState;
use crate::versioning::{self, UploadMeta};

/// 上传错误。
pub enum UploadError {
    Forbidden,
    InvalidVersionName(String),
    VersionExists(String),
    BinaryTooLarge,
    ConfigTooLarge,
    ConfigRequired,
    UnknownConfigFile(String),
    BinaryMissing,
    InsufficientStorage,
    MultipartError(String),
    IoError(String),
    SwitchFailed(String),
}

impl UploadError {
    pub fn code(&self) -> &'static str {
        match self {
            UploadError::Forbidden => "upload_disabled",
            UploadError::InvalidVersionName(_) => "invalid_version_name",
            UploadError::VersionExists(_) => "version_exists",
            UploadError::BinaryTooLarge => "binary_too_large",
            UploadError::ConfigTooLarge => "config_too_large",
            UploadError::ConfigRequired => "config_required",
            UploadError::UnknownConfigFile(_) => "unknown_config_file",
            UploadError::BinaryMissing => "binary_missing",
            UploadError::InsufficientStorage => "insufficient_storage",
            UploadError::MultipartError(_) => "multipart_error",
            UploadError::IoError(_) => "io_error",
            UploadError::SwitchFailed(_) => "switch_failed",
        }
    }
    pub fn http_status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            UploadError::Forbidden => StatusCode::FORBIDDEN,
            UploadError::InvalidVersionName(_) => StatusCode::BAD_REQUEST,
            UploadError::VersionExists(_) => StatusCode::CONFLICT,
            UploadError::BinaryTooLarge => StatusCode::BAD_REQUEST,
            UploadError::ConfigTooLarge => StatusCode::BAD_REQUEST,
            UploadError::ConfigRequired => StatusCode::BAD_REQUEST,
            UploadError::UnknownConfigFile(_) => StatusCode::BAD_REQUEST,
            UploadError::BinaryMissing => StatusCode::BAD_REQUEST,
            UploadError::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
            UploadError::MultipartError(_) => StatusCode::BAD_REQUEST,
            UploadError::IoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            UploadError::SwitchFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
    pub fn message(&self) -> String {
        match self {
            UploadError::Forbidden => "该 Action 未开启上传".to_string(),
            UploadError::InvalidVersionName(v) => format!("版本名非法: {v}"),
            UploadError::VersionExists(v) => format!("版本已存在: {v}"),
            UploadError::BinaryTooLarge => "二进制超过大小上限".to_string(),
            UploadError::ConfigTooLarge => "配置文件超过大小上限".to_string(),
            UploadError::ConfigRequired => {
                "缺少配置文件：首次上传需提供全部已登记的配置，或先切换到一个已有版本作为继承来源"
                    .to_string()
            }
            UploadError::UnknownConfigFile(v) => format!("未登记的配置文件: {v}"),
            UploadError::BinaryMissing => "请选择二进制文件".to_string(),
            UploadError::InsufficientStorage => "磁盘空间不足".to_string(),
            UploadError::MultipartError(e) => format!("请求解析失败: {e}"),
            UploadError::IoError(e) => format!("写盘失败: {e}"),
            UploadError::SwitchFailed(e) => format!("切换失败: {e}"),
        }
    }
}

/// 上传响应。
#[derive(serde::Serialize)]
pub struct UploadResponse {
    pub version: String,
    pub active: String,
    pub switched_from: Option<String>,
    pub discover: DiscoverState,
    pub binary_bytes: u64,
    pub config_bytes: u64,
    /// 本次因超出「可回退版本数量」上限而被自动清理的最旧版本
    pub pruned_versions: Vec<String>,
    /// 新版本目录里最终落盘的配置文件名（含继承来的）
    pub config_files: Vec<String>,
    /// 未在本次上传中提供、从上一版本继承的配置文件名
    pub inherited_configs: Vec<String>,
}

/// 生成随机 16 字节 → 32 hex 字符。
fn random_suffix() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).unwrap_or_default();
    // 预分配 + write! 直写，避免 32 个 hex 字符各分配一次 String。
    let mut out = String::with_capacity(32);
    use std::fmt::Write as _;
    for b in &buf {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// 下一个版本号：扫描已存在的 `v0.N`，次版本 +1；一个都没有时从 `v0.1` 起。
///
/// 版本不再由用户命名：上传即自动递增，避免「同名覆盖」和「名字风格各异」。
fn next_version_name(base_dir: &std::path::Path) -> String {
    let mut max_minor: Option<u32> = None;
    if let Ok(rd) = std::fs::read_dir(base_dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("v0.") {
                if let Ok(n) = rest.parse::<u32>() {
                    max_minor = Some(max_minor.map_or(n, |m| m.max(n)));
                }
            }
        }
    }
    match max_minor {
        Some(n) => format!("v0.{}", n + 1),
        None => crate::versioning::FIRST_VERSION.to_string(),
    }
}

/// 处理上传（并发锁包装）。
///
/// 无论成功还是失败都要把 action 从 `upload_in_progress` 里移除。
/// 旧实现只在成功路径 `drop(guard)`，集合里的 id 从未被删除，
/// 导致同一个脚本上传过一次后，之后所有上传都报 `upload_in_progress`。
pub async fn handle_upload(
    state: &AppState,
    action_id: &str,
    multipart: Multipart,
    remote_addr: Option<String>,
) -> Result<UploadResponse, UploadError> {
    {
        let mut in_progress = state.upload_in_progress.lock().await;
        if in_progress.contains(action_id) {
            return Err(UploadError::IoError("upload_in_progress".to_string()));
        }
        in_progress.insert(action_id.to_string());
    }
    let result = handle_upload_inner(state, action_id, multipart, remote_addr).await;
    state.upload_in_progress.lock().await.remove(action_id);
    result
}

/// 上传临时目录的 RAII 守卫：Drop 时自动清理，除非已 `disarm`。
/// 解决「`?` 早退路径（File::create / chunk / write_all / rename 等十余处）
/// 不经过显式 cleanup 调用导致 `.upload-*` 目录残留」的问题（CWE-772）。
struct TmpGuard {
    path: std::path::PathBuf,
    armed: bool,
}

impl TmpGuard {
    fn new(path: std::path::PathBuf) -> Self {
        TmpGuard { path, armed: true }
    }
    /// rename 成功后调用：目录已晋升为正式版本目录，不再清理。
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// 上传主流程（不含并发锁）。
async fn handle_upload_inner(
    state: &AppState,
    action_id: &str,
    multipart: Multipart,
    remote_addr: Option<String>,
) -> Result<UploadResponse, UploadError> {
    use tokio::io::AsyncWriteExt;

    let config = state.config.load();
    let action: Action = config
        .find_action(action_id)
        .cloned()
        .ok_or(UploadError::Forbidden)?;
    let vcfg = action
        .versioning
        .as_ref()
        .ok_or(UploadError::Forbidden)?;
    if !vcfg.enabled || !vcfg.upload.enabled {
        return Err(UploadError::Forbidden);
    }
    let base_dir = Path::new(&vcfg.base_dir);
    let max_binary = vcfg.upload.max_binary_mb * 1024 * 1024;
    let max_config = vcfg.upload.max_config_kb * 1024;

    // 单遍处理：流式写盘到临时目录
    // 版本名缺省用时间戳；若在流中遇到 version_name 字段则覆盖。
    let mut version_name: Option<String> = None;
    let mut has_binary = false;
    let mut binary_total: u64 = 0;
    let mut config_total: u64 = 0;
    // 该 action 登记的全部配置文件；每个版本目录都要带齐这一套
    let expected_configs = vcfg.all_config_files();
    let mut uploaded_configs: Vec<String> = Vec::new();

    // 创建临时目录（先于解析，避免解析中才发现版本名非法却已消耗流）
    // 版本名校验放在流式处理中：遇到 version_name 字段后立即校验
    let tmp_dir = base_dir.join(format!(".upload-{}-{}", std::process::id(), random_suffix()));
    if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
        return Err(UploadError::IoError(e.to_string()));
    }
    // RAII 守卫：任何 `?` 早退（含 multipart 流错误、写盘失败、大小超限）都会在
    // Drop 时清理 tmp_dir，不再依赖逐处显式 cleanup() 调用。
    let mut tmp_guard = TmpGuard::new(tmp_dir.clone());

    let binary_path = tmp_dir.join(&vcfg.binary_name);
    // 写 binary 的文件句柄（惰性打开）
    let mut binary_file: Option<tokio::fs::File> = None;

    // 全局版本管理关闭时，直接拒绝上传（避免传了却不生效）
    if !state.config.load().versioning_enabled {
        return Err(UploadError::IoError("版本管理已在设置中关闭".to_string()));
    }

    let mut mp = multipart;
    while let Some(mut field) = mp
        .next_field()
        .await
        .map_err(|e| UploadError::MultipartError(e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "version_name" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| UploadError::MultipartError(e.to_string()))?;
                if !versioning::is_valid_version_name(&v) {
                    return Err(UploadError::InvalidVersionName(v));
                }
                version_name = Some(v);
            }
            "binary" => {
                has_binary = true;
                binary_file = Some(
                    tokio::fs::File::create(&binary_path)
                        .await
                        .map_err(|e| UploadError::IoError(e.to_string()))?,
                );
                let f = binary_file.as_mut().unwrap();
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| UploadError::MultipartError(e.to_string()))?
                {
                    binary_total += chunk.len() as u64;
                    if max_binary > 0 && binary_total > max_binary {
                        return Err(UploadError::BinaryTooLarge);
                    }
                    f.write_all(&chunk)
                        .await
                        .map_err(|e| UploadError::IoError(e.to_string()))?;
                }
            }
            "config" => {
                // 每个 config 字段用浏览器给的原始文件名命名，可重复出现多次。
                // 只允许写入 action 已登记的名字，避免上传任意文件到版本目录。
                let fname = field.file_name().unwrap_or("").to_string();
                if fname.is_empty()
                    || !crate::config::is_valid_name(&fname)
                    || !expected_configs.contains(&fname)
                {
                    return Err(UploadError::UnknownConfigFile(fname));
                }
                let cp = tmp_dir.join(&fname);
                let mut f = tokio::fs::File::create(&cp)
                    .await
                    .map_err(|e| UploadError::IoError(e.to_string()))?;
                let mut file_total: u64 = 0;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| UploadError::MultipartError(e.to_string()))?
                {
                    file_total += chunk.len() as u64;
                    if max_config > 0 && file_total > max_config {
                        return Err(UploadError::ConfigTooLarge);
                    }
                    f.write_all(&chunk)
                        .await
                        .map_err(|e| UploadError::IoError(e.to_string()))?;
                }
                drop(f);
                config_total += file_total;
                // 单文件上限之外再卡总量，避免 N 个配置文件累计打满磁盘。
                if max_config > 0 && config_total > max_config.saturating_mul(4) {
                    return Err(UploadError::ConfigTooLarge);
                }
                if !uploaded_configs.contains(&fname) {
                    uploaded_configs.push(fname);
                }
            }
            _ => {
                // 忽略未知字段
            }
        }
    }

    // 关闭文件句柄
    drop(binary_file);

    // 校验 binary 存在
    if !has_binary {
        return Err(UploadError::BinaryMissing);
    }

    // 本次未提供的配置从当前活跃版本继承，保证每个版本目录都带齐一整套配置：
    // 切换 current 时整套配置随之切换；只改一个账号配置时不必重复上传其余文件。
    let previous = versioning::read_current_link(base_dir).ok();
    let mut inherited_configs: Vec<String> = Vec::new();
    for f in &expected_configs {
        if uploaded_configs.contains(f) {
            continue;
        }
        let src = previous
            .as_ref()
            .map(|p| base_dir.join(p).join(f))
            .filter(|p| p.is_file());
        match src {
            Some(p) => {
                std::fs::copy(&p, tmp_dir.join(f)).map_err(|e| {
                    UploadError::IoError(format!("继承配置 {f} 失败: {e}"))
                })?;
                config_total += std::fs::metadata(tmp_dir.join(f))
                    .map(|m| m.len())
                    .unwrap_or(0);
                inherited_configs.push(f.clone());
            }
            None => {
                // 首次上传且无旧版本可继承：已登记的配置必须由本次上传提供
                return Err(UploadError::ConfigRequired);
            }
        }
    }
    let config_files: Vec<String> = expected_configs
        .iter()
        .filter(|f| tmp_dir.join(f).is_file())
        .cloned()
        .collect();

    // 版本名
    // 未指定名称时自动递增 v0.1 / v0.2 …（API 仍允许显式传 version_name）
    let vname = version_name.unwrap_or_else(|| next_version_name(&base_dir));

    // 校验目标目录不存在
    let target_dir = base_dir.join(&vname);
    if target_dir.exists() {
        return Err(UploadError::VersionExists(vname));
    }

    // statvfs 检查
    if !crate::log::enough_free_space(base_dir, config.min_free_mb) {
        return Err(UploadError::InsufficientStorage);
    }

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&binary_path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&binary_path, perms);
        }
    }

    // 校验 binary 存在且可执行
    if !binary_path.is_file() {
        return Err(UploadError::IoError("binary 写入后不存在".to_string()));
    }

    // 写元数据
    let meta = UploadMeta {
        uploaded_at: chrono::Utc::now().to_rfc3339(),
        uploaded_by: remote_addr.clone().unwrap_or_else(|| "unknown".to_string()),
        binary_bytes: binary_total,
        config_bytes: config_total,
    };
    let meta_path = tmp_dir.join(".upload-meta.json");
    if let Err(e) = std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap_or_default()) {
        return Err(UploadError::IoError(e.to_string()));
    }

    // 原子 rename
    if let Err(e) = std::fs::rename(&tmp_dir, &target_dir) {
        return Err(UploadError::IoError(format!("rename 失败: {e}")));
    }
    // rename 成功：tmp_dir 已晋升为正式版本目录，解除守卫清理（避免把新版本删掉）。
    tmp_guard.disarm();

    // 自动切换
    let old = versioning::read_current_link(base_dir).ok();
    if let Err(e) = versioning::set_current_link(base_dir, &vname) {
        // 版本目录保留，让用户手动切换
        return Err(UploadError::SwitchFailed(e));
    }
    if old.as_deref() != Some(vname.as_str()) {
        if let Some(ref o) = old {
            state
                .last_active_versions
                .lock()
                .await
                .insert(action_id.to_string(), o.clone());
        }
    }

    // 版本数量上限：超出后自动清理最旧版本，绝不删除当前活跃版本与回退目标。
    // 默认上限 5，由 config.max_versions 提供，Config::validate() 保证其 ≥1。
    let mut keep = vec![vname.clone()];
    if let Some(ref o) = old {
        if o != &vname {
            keep.push(o.clone());
        }
    }
    let pruned_versions = match crate::versioning::enforce_version_limit(
        base_dir,
        config.max_versions as usize,
        &keep,
    ) {
        Ok(list) => {
            for name in &list {
                tracing::info!(
                    action = %action_id,
                    version = %name,
                    max_versions = config.max_versions,
                    "超出可回退版本数量，已清理最旧版本"
                );
            }
            list
        }
        Err(e) => {
            // 清理失败不影响本次上传结果，但要留下明确告警
            tracing::warn!(action = %action_id, error = %e, "版本数量上限清理失败");
            Vec::new()
        }
    };

    // 触发参数重新发现（异步）
    let state2 = state.clone();
    let id2 = action_id.to_string();
    tokio::spawn(async move {
        crate::discover::run_and_cache(&state2, &id2).await;
    });

    tracing::info!(
        action = %action_id,
        version = %vname,
        binary_bytes = binary_total,
        config_bytes = config_total,
        remote_addr = remote_addr.as_deref().unwrap_or("unknown"),
        "version uploaded and activated"
    );

    Ok(UploadResponse {
        version: vname.clone(),
        active: vname,
        switched_from: old,
        discover: DiscoverState {
            status: "discovering".to_string(),
            error: None,
        },
        binary_bytes: binary_total,
        config_bytes: config_total,
        pruned_versions,
        config_files,
        inherited_configs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_version_name_starts_at_v0_1_and_increments() {
        let dir = std::env::temp_dir().join(format!("ver-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(next_version_name(&dir), "v0.1");
        std::fs::create_dir_all(dir.join("v0.1")).unwrap();
        assert_eq!(next_version_name(&dir), "v0.2");
        std::fs::create_dir_all(dir.join("v0.9")).unwrap();
        assert_eq!(next_version_name(&dir), "v0.10");
        // 非 v0.N 的目录不影响计数
        std::fs::create_dir_all(dir.join("v1")).unwrap();
        assert_eq!(next_version_name(&dir), "v0.10");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_codes_mapping() {
        assert_eq!(UploadError::Forbidden.http_status(), 403);
        assert_eq!(UploadError::BinaryTooLarge.http_status(), 400);
        assert_eq!(UploadError::InsufficientStorage.http_status(), 507);
        assert_eq!(UploadError::VersionExists("x".into()).http_status(), 409);
    }
}
