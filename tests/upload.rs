//! 上传模块测试：版本名校验、错误码映射、大小校验逻辑。

use olivetin_rs::upload::UploadError;
use olivetin_rs::versioning::is_valid_version_name;

#[test]
fn version_name_rejects_traversal() {
    assert!(is_valid_version_name("v1"));
    assert!(!is_valid_version_name(".."));
    assert!(!is_valid_version_name("."));
    assert!(!is_valid_version_name(".hidden"));
    assert!(!is_valid_version_name("a/../b"));
    assert!(!is_valid_version_name("a\\..\\b"));
}

#[test]
fn version_name_length_boundary() {
    assert!(is_valid_version_name(&"x".repeat(64)));
    assert!(!is_valid_version_name(&"x".repeat(65)));
}

#[test]
fn error_status_codes() {
    assert_eq!(UploadError::Forbidden.http_status(), 403);
    assert_eq!(UploadError::BinaryTooLarge.http_status(), 400);
    assert_eq!(UploadError::ConfigTooLarge.http_status(), 400);
    assert_eq!(UploadError::BinaryMissing.http_status(), 400);
    assert_eq!(UploadError::ConfigRequired.http_status(), 400);
    assert_eq!(UploadError::VersionExists("v".into()).http_status(), 409);
    assert_eq!(UploadError::InsufficientStorage.http_status(), 507);
    assert_eq!(UploadError::IoError("x".into()).http_status(), 500);
    assert_eq!(UploadError::SwitchFailed("x".into()).http_status(), 500);
}

#[test]
fn error_codes() {
    assert_eq!(UploadError::Forbidden.code(), "upload_disabled");
    assert_eq!(UploadError::BinaryTooLarge.code(), "binary_too_large");
    assert_eq!(UploadError::InsufficientStorage.code(), "insufficient_storage");
    assert_eq!(UploadError::VersionExists("v".into()).code(), "version_exists");
}

#[test]
fn error_messages() {
    assert_eq!(UploadError::Forbidden.message(), "该 Action 未开启上传");
    assert!(UploadError::BinaryTooLarge.message().contains("上限"));
}
