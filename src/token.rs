//! Token 加载、生成、重置与文件原子写入。
//!
//! Token 唯一来源是 `auth.token_file`。首次启动文件不存在时自动生成 32 字节随机数
//! 并转 64 hex 字符；写入采用 `.tmp` + rename 原子方式，`chmod 600`。

use std::path::Path;

/// Token 初始化结果：从文件加载，或首次生成。
pub enum TokenInit {
    Loaded(String),
    Generated(String),
}

/// 生成 32 字节随机数，转 64 hex 字符。
/// 失败（熵源异常）返回错误，不再 `.expect` 让 `panic=abort` 终止进程。
pub fn generate_token() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("getrandom 失败: {e}"))?;
    // 预分配 + write! 直写，避免 64 个 hex 字符各分配一次 String。
    let mut out = String::with_capacity(64);
    use std::fmt::Write as _;
    for b in &buf {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// 原子写入 token 文件（`.tmp` + rename + chmod 600）。
pub fn write_token_atomic(path: &Path, token: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, token).map_err(|e| format!("写入 token 临时文件失败: {e}"))?;
    // chmod 600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("token 文件 rename 失败: {e}"))?;
    Ok(())
}

/// 初始化 Token。
///
/// - 文件存在且非空 → 读取返回 `Loaded`
/// - 文件存在但为空 → 报错
/// - 文件不存在 → 生成、原子写入、返回 `Generated`
pub fn init_token(path: &Path) -> Result<TokenInit, String> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 token 文件失败: {e}"))?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(format!("token 文件为空: {}", path.display()));
        }
        return Ok(TokenInit::Loaded(trimmed.to_string()));
    }
    // 文件不存在：生成
    let token = generate_token()?;
    // 确保父目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 token 父目录失败: {e}"))?;
    }
    write_token_atomic(path, &token)?;
    Ok(TokenInit::Generated(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_generates_when_missing() {
        let dir = std::env::temp_dir().join(format!("olivetin-token-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("token.txt");
        let _ = std::fs::remove_file(&p);
        match init_token(&p).unwrap() {
            TokenInit::Generated(t) => assert_eq!(t.len(), 64),
            _ => panic!("应生成"),
        }
        // 第二次应 Loaded
        match init_token(&p).unwrap() {
            TokenInit::Loaded(t) => assert_eq!(t.len(), 64),
            _ => panic!("应加载"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_errors_on_empty() {
        let dir = std::env::temp_dir().join(format!("olivetin-token-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("token.txt");
        std::fs::write(&p, "").unwrap();
        assert!(init_token(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
