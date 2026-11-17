//! 脚本版本管理：扫描版本目录、维护 current 软链接、切换、回退。
//!
//! 目录约定：`{base_dir}/{version_name}/`，`{base_dir}/current` 为面板维护的软链接。
//! 扫描忽略 `current` 与 `.` 开头的隐藏目录；上传元数据 `.upload-meta.json` 也忽略。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::config::Action;

/// 版本信息。
#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
    pub name: String,
    pub mtime: String,
    pub binary_size: u64,
    /// 旧字段：该版本是否带配置（保留兼容前端）
    pub config_present: bool,
    /// 该版本目录里实际存在的配置文件名；切换版本时整套配置随之切换
    pub config_files: Vec<String>,
    pub is_active: bool,
    pub uploaded_by: Option<String>,
    pub uploaded_at: Option<String>,
}

/// 上传元数据（写入 `{version}/.upload-meta.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadMeta {
    pub uploaded_at: String,
    pub uploaded_by: String,
    pub binary_bytes: u64,
    pub config_bytes: u64,
}

/// 扫描版本列表。
pub fn scan(action: &Action) -> Result<Vec<VersionInfo>, String> {
    let cfg = action
        .versioning
        .as_ref()
        .ok_or("versioning not configured")?;
    let base = Path::new(&cfg.base_dir);
    if !base.is_dir() {
        return Err(format!("base_dir {} is not a directory", cfg.base_dir));
    }
    let active = read_current_link(base).ok();
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(base).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "current" || name.starts_with('.') {
            continue;
        }
        if !is_valid_version_name(&name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let binary_path = path.join(&cfg.binary_name);
        if !binary_path.is_file() {
            continue;
        }
        let meta = std::fs::metadata(&binary_path).map_err(|e| e.to_string())?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| DateTime::<Utc>::from_timestamp(d.as_secs() as i64, 0))
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_string());
        // 每个版本目录自带一整套配置；这里按声明顺序收集实际存在的文件
        let config_files: Vec<String> = cfg
            .all_config_files()
            .into_iter()
            .filter(|f| path.join(f).is_file())
            .collect();
        let config_present = !config_files.is_empty();
        let (uploaded_by, uploaded_at) = read_upload_meta(&path);
        versions.push(VersionInfo {
            name: name.clone(),
            mtime,
            binary_size: meta.len(),
            config_present,
            config_files,
            is_active: active.as_deref() == Some(&name),
            uploaded_by,
            uploaded_at,
        });
    }
    versions.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    if versions.is_empty() {
        return Err(format!("no valid versions found in {}", cfg.base_dir));
    }
    Ok(versions)
}

/// 读取上传元数据。
fn read_upload_meta(path: &Path) -> (Option<String>, Option<String>) {
    let meta_path = path.join(".upload-meta.json");
    if let Ok(bytes) = std::fs::read(&meta_path) {
        if let Ok(meta) = serde_json::from_slice::<UploadMeta>(&bytes) {
            return (Some(meta.uploaded_by), Some(meta.uploaded_at));
        }
    }
    (None, None)
}

/// 版本名校验：`[a-zA-Z0-9._-]+`、长度 ≤64、不含 `..`、不以 `.` 开头。
/// 目录名清洗：只留字母/数字/`.`/`_`/`-`，避免标题里的中文与空格变成路径
fn sanitize_dir_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() {
        "script".to_string()
    } else if t.chars().count() > 48 {
        t.chars().take(48).collect()
    } else {
        t
    }
}

/// 给「开了版本管理但没填目录」的 action 自动铺好版本目录：
///
///   {script_dir}/{id}/v0.1/{原脚本}   +   current -> v0.1
///
/// 当前脚本会被**复制**一份进去（原文件保留，不删不动——可能有别的定时任务在引用它），
/// 然后把 exec 指到 `current/{文件名}`，从下一个版本开始就由面板管理。
/// 首个版本目录名：创建接口 / bootstrap / 上传递增共用，
/// 与 `upload::next_version_name` 的 `v0.N` 体系保持一致。
pub const FIRST_VERSION: &str = "v0.1";

/// 兼容历史数据：早期创建接口写的是 `v1`，与 `v0.N` 递增体系不一致。
/// 若 base 下只有 `v1` 则重命名为 FIRST_VERSION 并重建 current 软链。
pub fn migrate_legacy_first_version(base: &std::path::Path) -> Result<(), String> {
    let legacy = base.join("v1");
    let target = base.join(FIRST_VERSION);
    if !legacy.is_dir() || target.exists() {
        return Ok(());
    }
    std::fs::rename(&legacy, &target).map_err(|e| format!("迁移 v1 目录失败: {e}"))?;
    set_current_link(base, FIRST_VERSION)?;
    Ok(())
}

pub fn bootstrap(action: &mut crate::config::Action, script_dir: &str) -> Result<String, String> {
    let Some(v) = action.versioning.as_ref() else {
        return Err("未启用版本管理".to_string());
    };
    let base = std::path::Path::new(script_dir).join(sanitize_dir_name(&action.id));

    // 源脚本：命令型用面板生成的 .sh；引用型从 exec 里取脚本路径。
    // 注意不能用 action.script_ref()：它在 versioning 已启用时直接返回 None，
    // 而这里恰好是「已勾选版本管理」的场景，会永远拿不到源文件。
    let src: std::path::PathBuf = if action.uses_shell_script() {
        action.script_path(Some(script_dir))
    } else if action.exec.len() >= 2 && crate::discover::is_interpreter(&action.exec[0]) {
        std::path::PathBuf::from(&action.exec[1])
    } else if let Some(first) = action.exec.first() {
        std::path::PathBuf::from(first)
    } else {
        return Err("这个脚本没有可迁移的脚本文件".to_string());
    };
    if !src.is_file() {
        return Err(format!("源脚本不存在：{}", src.display()));
    }

    let name = if v.binary_name.trim().is_empty() {
        src.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "run.sh".to_string())
    } else {
        v.binary_name.trim().to_string()
    };

    let first_dir = base.join(FIRST_VERSION);
    std::fs::create_dir_all(&first_dir).map_err(|e| format!("创建版本目录失败: {e}"))?;
    let dest = first_dir.join(&name);
    if !dest.exists() {
        std::fs::copy(&src, &dest).map_err(|e| format!("复制脚本失败: {e}"))?;
    }
    // 复制过来的文件要保持可执行（shell 脚本靠 shebang + 755 直接跑）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }

    // current 软链（已存在就不动）
    let cur = base.join("current");
    if !cur.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(FIRST_VERSION, &cur).map_err(|e| format!("创建 current 软链失败: {e}"))?;
    }

    // exec 指到 current 下的同名文件；原来是「解释器 + 脚本」的保留解释器
    let new_path = cur.join(&name).to_string_lossy().to_string();
    if action.exec.len() >= 2 && crate::discover::is_interpreter(&action.exec[0]) {
        action.exec[1] = new_path;
    } else {
        action.exec = vec![new_path];
    }

    if let Some(v) = action.versioning.as_mut() {
        v.base_dir = base.to_string_lossy().to_string();
        v.binary_name = name;
        v.enabled = true;
    }
    Ok(base.to_string_lossy().to_string())
}

pub fn is_valid_version_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > 64 {
        return false;
    }
    if name.starts_with('.') {
        return false;
    }
    if name.contains("..") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// 读取 current 软链接指向的版本名。
pub fn read_current_link(base_dir: &Path) -> Result<String, String> {
    let target = std::fs::read_link(base_dir.join("current")).map_err(|e| e.to_string())?;
    target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "invalid current link".into())
}

/// 原子替换 current 软链接（symlink + rename）。
/// 写入前校验版本名：杜绝 `activate_version` 等接口传入 `../etc` 之类的名字
/// 把 current 软链指向任意目录（CWE-22/CWE-59）。
pub fn set_current_link(base_dir: &Path, version: &str) -> Result<(), String> {
    if !is_valid_version_name(version) {
        return Err(format!("版本名非法，拒绝切换 current: {version}"));
    }
    let link = base_dir.join("current");
    let tmp = base_dir.join(format!(".current.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(version, &tmp).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        // 非 Unix 无软链接语义，写一个文本文件占位
        std::fs::write(&tmp, version).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &link).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(())
}

/// 启动修复：current 不存在或失效时，自动指向 mtime 最新版本。
pub fn repair_current(base_dir: &Path) -> Result<Option<String>, String> {
    if read_current_link(base_dir).is_ok() {
        return Ok(None); // 已有效
    }
    // 选择 mtime 最新的版本目录
    let mut best: Option<(String, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(base_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "current" || name.starts_with('.') {
            continue;
        }
        if !is_valid_version_name(&name) {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(m) = meta.modified() {
                if best.as_ref().map(|(_, t)| m > *t).unwrap_or(true) {
                    best = Some((name, m));
                }
            }
        }
    }
    if let Some((name, _)) = best {
        set_current_link(base_dir, &name)?;
        Ok(Some(name))
    } else {
        Err("无有效版本可用于修复 current".to_string())
    }
}

/// 按「可回退版本数量」上限清理最旧版本，返回被删除的版本名。
///
/// - 只处理合法版本目录（跳过 current、隐藏目录、非法名、非目录）；
/// - 按 mtime 升序删除最旧的，mtime 相同用名称兜底，保证结果确定；
/// - `keep` 中的版本（当前活跃版本、回退目标）永不删除，即使它是最旧的；
/// - 上限必须 ≥1，调用方需已通过 `Config::validate()` 校验。
pub fn enforce_version_limit(
    base_dir: &Path,
    max_versions: usize,
    keep: &[String],
) -> Result<Vec<String>, String> {
    if max_versions == 0 {
        return Err("max_versions 必须 ≥ 1，不允许无限保存版本".to_string());
    }
    if !base_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut items: Vec<(String, std::time::SystemTime)> = Vec::new();
    let entries = std::fs::read_dir(base_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "current" || name.starts_with('.') || !is_valid_version_name(&name) {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        // 没有 binary 的残缺目录不算版本（与 scan() 的判定无关，这里只清理完整目录）
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        items.push((name, mtime));
    }
    items.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let mut remaining = items.len();
    let mut removed = Vec::new();
    for (name, _) in &items {
        if remaining <= max_versions {
            break;
        }
        if keep.iter().any(|k| k == name) {
            continue; // 保护活跃/回退目标，继续找下一个最旧的
        }
        match std::fs::remove_dir_all(base_dir.join(name)) {
            Ok(()) => {
                removed.push(name.clone());
                remaining -= 1;
            }
            Err(e) => return Err(format!("清理版本 {name} 失败: {e}")),
        }
    }
    Ok(removed)
}

/// 清理 `.upload-*` 残留临时目录。
pub fn cleanup_upload_tmp(base_dir: &Path) -> Vec<String> {
    let mut cleaned = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".upload-") && entry.path().is_dir() {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    cleaned.push(name);
                }
            }
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrate_legacy_v1_to_first_version() {
        let base = std::env::temp_dir().join(format!("olivetin-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("v1")).unwrap();
        std::fs::write(base.join("v1").join("run.sh"), b"echo 1").unwrap();
        migrate_legacy_first_version(&base).unwrap();
        assert!(base.join(FIRST_VERSION).is_dir(), "v1 应迁移为 v0.1");
        assert!(!base.join("v1").exists(), "旧目录应消失");
        assert!(base.join(FIRST_VERSION).join("run.sh").exists(), "内容随目录迁移");
        assert!(base.join("current").exists(), "current 软链应重建");
        // 幂等：已有 v0.1 时再调用不动
        migrate_legacy_first_version(&base).unwrap();
        assert!(base.join(FIRST_VERSION).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn first_version_is_valid_and_matches_v0_n_scheme() {
        assert_eq!(FIRST_VERSION, "v0.1");
        assert!(is_valid_version_name(FIRST_VERSION));
    }

    use super::*;

    #[test]
    fn version_name_validation() {
        assert!(is_valid_version_name("v1.2-beta"));
        assert!(is_valid_version_name("abc_123"));
        assert!(!is_valid_version_name(".hidden"));
        assert!(!is_valid_version_name("a..b"));
        assert!(!is_valid_version_name(""));
        assert!(!is_valid_version_name("a/b"));
        assert!(!is_valid_version_name("a\\b"));
        assert!(!is_valid_version_name(&"x".repeat(65)));
    }

    #[test]
    fn config_is_valid_name_aligned() {
        // 版本名字符集校验在 config.rs，确保两者对同一名字判定一致
        assert!(crate::config::is_valid_name("v1"));
        assert!(is_valid_version_name("v1"));
        assert!(!is_valid_version_name("../etc"));
    }

    #[test]
    fn enforce_limit_rejects_zero() {
        let base = std::env::temp_dir();
        assert!(enforce_version_limit(&base, 0, &[]).is_err());
    }

    #[test]
    fn enforce_limit_removes_oldest_and_protects_active() {
        let base = std::env::temp_dir().join(format!("olivetin-vlimit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for n in ["v1", "v2", "v3", "v4"] {
            std::fs::create_dir_all(base.join(n)).unwrap();
            std::fs::write(base.join(n).join("bin"), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let keep = vec!["v1".to_string()]; // 模拟「当前活跃版本」被保护
        let removed = enforce_version_limit(&base, 2, &keep).unwrap();
        assert_eq!(removed.len(), 2, "应清理 2 个最旧版本: {removed:?}");
        assert!(base.join("v1").is_dir(), "活跃版本不得被删除");
        assert!(base.join("v4").is_dir(), "最新版本必须保留");
        assert!(!base.join("v2").exists());
        assert!(!base.join("v3").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enforce_limit_noop_when_under_limit() {
        let base = std::env::temp_dir().join(format!("olivetin-vlimit2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("v1")).unwrap();
        let removed = enforce_version_limit(&base, 5, &[]).unwrap();
        assert!(removed.is_empty());
        assert!(base.join("v1").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }
}
