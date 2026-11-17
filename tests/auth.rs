//! 认证测试：三载体提取、恒定时间比较。

use olivetin_rs::auth::{extract_token, token_eq};

fn build_request(uri: &str, auth: Option<&str>, cookie: Option<&str>) -> axum::http::Request<axum::body::Body> {
    let mut builder = axum::http::Request::builder().uri(uri);
    if let Some(a) = auth {
        builder = builder.header(axum::http::header::AUTHORIZATION, a);
    }
    if let Some(c) = cookie {
        builder = builder.header(axum::http::header::COOKIE, c);
    }
    builder.body(axum::body::Body::empty()).unwrap()
}

#[test]
fn extract_bearer() {
    let req = build_request("/api/actions", Some("Bearer abc123"), None);
    assert_eq!(extract_token(&req), Some("abc123".to_string()));
}

#[test]
fn extract_cookie() {
    let req = build_request("/", None, Some("olivetin_token=xyz789; other=1"));
    assert_eq!(extract_token(&req), Some("xyz789".to_string()));
}

#[test]
fn extract_query() {
    let req = build_request("/?token=querytoken", None, None);
    assert_eq!(extract_token(&req), Some("querytoken".to_string()));
}

#[test]
fn extract_none() {
    let req = build_request("/", None, None);
    assert_eq!(extract_token(&req), None);
}

#[test]
fn constant_time_eq() {
    assert!(token_eq("same", "same"));
    assert!(!token_eq("same", "different"));
    assert!(!token_eq("short", "shortlong"));
    assert!(!token_eq("", "x"));
}
