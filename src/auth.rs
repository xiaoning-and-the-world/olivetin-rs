//! Token 认证中间件与 CSRF 校验。
//!
//! 三种载体：Authorization Bearer / ?token= query / olivetin_token cookie。
//! Token 比较使用恒定时间比较；cookie 认证的 POST 必须通过 CSRF（Origin/Referer 与 Host 匹配）。

use axum::extract::Request;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN, REFERER};
use axum::http::StatusCode;
use axum::response::Response;
use axum::middleware::Next;
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// 提取请求中的 Token（Bearer > cookie > query）。
pub fn extract_token(req: &Request) -> Option<String> {
    // 1. Bearer
    if let Some(auth) = req.headers().get(AUTHORIZATION) {
        if let Ok(v) = auth.to_str() {
            if let Some(rest) = v.strip_prefix("Bearer ") {
                return Some(rest.trim().to_string());
            }
        }
    }
    // 2. cookie
    if let Some(cookie_header) = req.headers().get(COOKIE) {
        if let Ok(v) = cookie_header.to_str() {
            for pair in v.split(';') {
                let pair = pair.trim();
                if let Some((k, val)) = pair.split_once('=') {
                    if k.trim() == "olivetin_token" {
                        return Some(val.trim().to_string());
                    }
                }
            }
        }
    }
    // 3. query ?token=
    {
        let uri = req.uri().to_string();
        if let Some(idx) = uri.find('?') {
            let query = &uri[idx + 1..];
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "token" {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 构造认证 cookie 字符串（单一入口，避免多处硬编码 Max-Age 造成语义漂移）。
/// `ttl_hours` 为有效时长（小时），转成 Max-Age 秒。
pub fn make_auth_cookie(token: &str, ttl_hours: u32) -> String {
    format!(
        "olivetin_token={}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        token,
        ttl_hours.saturating_mul(3600)
    )
}

/// 恒定时间比较两个 Token。
pub fn token_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// 认证结果。
pub enum AuthResult {
    Ok,
    Unauthorized,
}

/// 校验 Token 是否正确。
pub fn verify(state: &AppState, provided: &str) -> AuthResult {
    let current = state.token();
    if token_eq(provided, &current) {
        AuthResult::Ok
    } else {
        AuthResult::Unauthorized
    }
}

/// 从 `host[:port]` 字符串里提取 hostname，正确处理 IPv6 字面量（`[::1]:1337` → `::1`）。
fn hostname_of(host_port: &str) -> &str {
    let host_port = host_port.trim();
    if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 字面量：host 在 `]` 之前
        return rest.split(']').next().unwrap_or(host_port);
    }
    // 普通 host[:port]：冒号前的部分（IPv6 不带括号时无法可靠区分，保守取原值）
    host_port.split(':').next().unwrap_or(host_port)
}

/// CSRF 校验（基于 HeaderMap 版本）：校验 Origin/Referer 与 Host 匹配。
pub fn csrf_ok_from_headers(headers: &axum::http::HeaderMap) -> bool {
    let host = headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let origin = headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let s = s.trim();
            let no_scheme = s
                .strip_prefix("https://")
                .or_else(|| s.strip_prefix("http://"))
                .unwrap_or(s);
            let host_part = no_scheme.split(['/', '?', '#']).next().unwrap_or("");
            if host_part.is_empty() {
                None
            } else {
                Some(host_part.to_string())
            }
        });
    match (host, origin) {
        (_, None) => false,
        (Some(h), Some(o)) => {
            let h_host = hostname_of(&h);
            let o_host = hostname_of(&o);
            h_host == o_host
        }
        (None, _) => false,
    }
}

/// 宽松 CSRF：浏览器跨站写一定会带 Origin/Referer，此时严格比对 Host；
/// 无来源头的 CLI / 脚本调用放行，不破坏现有 API 用法。
pub fn csrf_ok_if_present(headers: &axum::http::HeaderMap) -> bool {
    if headers.get(ORIGIN).is_none() && headers.get(REFERER).is_none() {
        return true;
    }
    csrf_ok_from_headers(headers)
}

/// 认证中间件：校验 Token；未通过返回 401。
/// 对 cookie 认证的写操作（POST/PUT/DELETE/PATCH）额外校验 CSRF。
/// query token 认证通过时自动种 cookie（便于浏览器后续访问）。
///
/// 安全模式（`auth.enabled`）关闭时整体放行 —— 默认首次访问无需 token，
/// 也不做 CSRF 校验（没有身份凭据可伪造）。设置页仍可访问，以便随时开启。
pub async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_enabled = state.config.load().auth.enabled;
    if !auth_enabled {
        // 安全模式关闭不等于可以跨站改配置：带 Origin 的写请求仍校验来源。
        let is_write = matches!(
            req.method(),
            &axum::http::Method::POST | &axum::http::Method::PUT
                | &axum::http::Method::DELETE | &axum::http::Method::PATCH
        );
        if is_write && !csrf_ok_if_present(req.headers()) {
            return Err(StatusCode::FORBIDDEN);
        }
        return Ok(next.run(req).await);
    }
    let provided = extract_token(&req);
    let tok = match provided {
        Some(tok) => tok,
        None => return Err(StatusCode::UNAUTHORIZED),
    };
    if !matches!(verify(&state, &tok), AuthResult::Ok) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // CSRF：仅对「非 Bearer 载体」的写方法校验
    let is_bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("Bearer "))
        .unwrap_or(false);
    let is_write = matches!(
        req.method(),
        &axum::http::Method::POST | &axum::http::Method::PUT | &axum::http::Method::DELETE | &axum::http::Method::PATCH
    );
    if is_write && !is_bearer && !csrf_ok_from_headers(req.headers()) {
        return Err(StatusCode::FORBIDDEN);
    }

    // query token 认证通过：种 cookie（响应头），后续请求用 cookie 免 query
    let uses_query_token = !is_bearer
        && req.headers().get(COOKIE).is_none()
        && req.uri().to_string().contains("token=");

    let mut resp = next.run(req).await;
    if uses_query_token {
        let cookie_ttl = state.config.load().auth.cookie_ttl_hours;
        let cookie = make_auth_cookie(&tok, cookie_ttl);
        if let Ok(v) = axum::http::HeaderValue::from_str(&cookie) {
            resp.headers_mut().insert(axum::http::header::SET_COOKIE, v);
        }
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_auth_cookie_uses_ttl_seconds() {
        let cookie = make_auth_cookie("tok123", 168);
        assert!(cookie.contains("olivetin_token=tok123"));
        assert!(cookie.contains("Max-Age=604800"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
    }
}
