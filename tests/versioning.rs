//! 版本管理测试：scan / set_current_link / 版本名校验 / 回退语义。

use olivetin_rs::versioning::{is_valid_version_name, read_current_link, set_current_link};

#[test]
fn version_name_valid() {
    assert!(is_valid_version_name("v1.2-beta"));
    assert!(is_valid_version_name("abc_123"));
    assert!(is_valid_version_name("v1"));
}

#[test]
fn version_name_invalid() {
    assert!(!is_valid_version_name(".hidden"));
    assert!(!is_valid_version_name("a..b"));
    assert!(!is_valid_version_name(""));
    assert!(!is_valid_version_name("a/b"));
    assert!(!is_valid_version_name("a\\b"));
    assert!(!is_valid_version_name("a b"));
    assert!(!is_valid_version_name(&"x".repeat(65)));
}

#[cfg(unix)]
#[test]
fn set_and_read_current_link() {
    let dir = std::env::temp_dir().join(format!("ver-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("v1")).unwrap();
    set_current_link(&dir, "v1").unwrap();
    assert_eq!(read_current_link(&dir).unwrap(), "v1");
    // 原子替换
    std::fs::create_dir_all(dir.join("v2")).unwrap();
    set_current_link(&dir, "v2").unwrap();
    assert_eq!(read_current_link(&dir).unwrap(), "v2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_missing_current_errors() {
    let dir = std::env::temp_dir().join(format!("ver-missing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(read_current_link(&dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
