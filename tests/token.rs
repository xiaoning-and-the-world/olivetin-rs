//! Token 测试。

use olivetin_rs::token::{generate_token, init_token, write_token_atomic, TokenInit};

#[test]
fn generate_is_64_hex() {
    let t = generate_token().unwrap();
    assert_eq!(t.len(), 64);
    assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn generate_unique() {
    let a = generate_token().unwrap();
    let b = generate_token().unwrap();
    assert_ne!(a, b);
}

#[test]
fn init_generates_then_loads() {
    let dir = std::env::temp_dir().join(format!("tok-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("token.txt");
    let _ = std::fs::remove_file(&p);
    match init_token(&p).unwrap() {
        TokenInit::Generated(t) => assert_eq!(t.len(), 64),
        _ => panic!("应生成"),
    }
    match init_token(&p).unwrap() {
        TokenInit::Loaded(t) => assert_eq!(t.len(), 64),
        _ => panic!("应加载"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_empty_file_errors() {
    let dir = std::env::temp_dir().join(format!("tok-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("token.txt");
    std::fs::write(&p, "").unwrap();
    assert!(init_token(&p).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_atomic_roundtrip() {
    let dir = std::env::temp_dir().join(format!("tok-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("t.txt");
    write_token_atomic(&p, "abc").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "abc");
    let _ = std::fs::remove_dir_all(&dir);
}
