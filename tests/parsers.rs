//! help 解析器与类型启发式测试。

use olivetin_rs::discover::{infer_type, parse_help};

#[test]
fn infer_int() {
    assert_eq!(infer_type("port", Some("<int>")), ("int", "option"));
}

#[test]
fn infer_string() {
    assert_eq!(infer_type("name", Some("<string>")), ("string", "option"));
    assert_eq!(infer_type("path", Some("<path>")), ("string", "option"));
}

#[test]
fn infer_default() {
    assert_eq!(infer_type("foo", None), ("string", "option"));
}

#[test]
fn parse_clap_style() {
    let text = "Usage: x\nOptions:\n  --port <int>  port\n  --verbose     verbose flag\n  --name <string> name\n";
    let params = parse_help(text, "clap").unwrap();
    let port = params.iter().find(|p| p.name == "port").unwrap();
    assert_eq!(port.r#type, "int");
    let verbose = params.iter().find(|p| p.name == "verbose").unwrap();
    assert_eq!(verbose.r#type, "bool");
    assert_eq!(verbose.style, "flag");
}

#[test]
fn parse_dedup() {
    let text = "--a x\n--a y\n--b z\n";
    let params = parse_help(text, "lines").unwrap();
    let names: Vec<_> = params.iter().map(|p| p.name.clone()).collect();
    assert_eq!(names.len(), 2);
}

#[test]
fn parse_empty_returns_err() {
    assert!(parse_help("", "clap").is_err());
}

#[test]
fn parse_garbage_no_panic() {
    let garbage: Vec<u8> = (0..512).map(|i| (i * 73) as u8).collect();
    let s = String::from_utf8_lossy(&garbage);
    let _ = parse_help(&s, "clap");
    let _ = parse_help(&s, "cobra");
    let _ = parse_help(&s, "sections");
    let _ = parse_help(&s, "lines");
}
