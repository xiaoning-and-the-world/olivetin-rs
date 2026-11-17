//! 时区解析与本地化格式化。
//!
//! 支持 "Local" / "UTC" / "±HH:MM" 三种形式，内部用 chrono::FixedOffset 表示。
//! 内部存储与比较统一 UTC，展示时按指定时区格式化。

use chrono::{DateTime, FixedOffset, Local, Offset, Utc};

/// 解析时区字符串为 `FixedOffset`。
/// 支持 "Local"、"UTC"、"Z"、"+08:00"、"-05:30" 等形式。
pub fn parse_timezone(s: &str) -> Result<FixedOffset, String> {
    let t = s.trim();
    match t.to_ascii_uppercase().as_str() {
        "LOCAL" => {
            let off = Local::now().offset().fix();
            Ok(off)
        }
        "UTC" | "Z" => Ok(FixedOffset::east_opt(0).unwrap()),
        _ => {
            // ±HH:MM 或 ±HHMM
            let bytes = t.as_bytes();
            let (sign, rest) = match bytes.first() {
                Some(b'+') => (1i32, &bytes[1..]),
                Some(b'-') => (-1i32, &bytes[1..]),
                _ => return Err(format!("无法解析时区: {s}")),
            };
            let rest = String::from_utf8_lossy(rest).to_string();
            let (h, m) = if let Some(idx) = rest.find(':') {
                let h = rest[..idx].parse::<i32>().map_err(|_| format!("无法解析时区: {s}"))?;
                let m = rest[idx + 1..].parse::<i32>().map_err(|_| format!("无法解析时区: {s}"))?;
                (h, m)
            } else if rest.len() == 4 {
                let h = rest[..2].parse::<i32>().map_err(|_| format!("无法解析时区: {s}"))?;
                let m = rest[2..].parse::<i32>().map_err(|_| format!("无法解析时区: {s}"))?;
                (h, m)
            } else {
                return Err(format!("无法解析时区: {s}"));
            };
            if m >= 60 || m < 0 || h > 23 {
                return Err(format!("无法解析时区: {s}"));
            }
            let secs = sign * (h * 3600 + m * 60);
            FixedOffset::east_opt(secs).ok_or_else(|| format!("无法解析时区: {s}"))
        }
    }
}

/// 将 UTC 时间格式化为指定时区的本地时间字符串 "YYYY-MM-DD HH:MM:SS"。
pub fn format_local(dt: &DateTime<Utc>, tz: &FixedOffset) -> String {
    dt.with_timezone(tz).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 将 UTC 时间格式化为 RFC3339 字符串。
pub fn format_rfc3339(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// 面板日志时间戳：RFC3339 + 指定时区偏移，微秒精度。
/// 例：`2026-11-12T17:44:27.884804+08:00`。
///
/// 之前 tracing 用默认 timer，输出的是 UTC（`...T09:44:27.884804Z`），
/// 比本地时间少 8 小时，和 logread、脚本日志对不上。
pub fn format_log_ts(dt: &DateTime<Utc>, tz: &FixedOffset) -> String {
    dt.with_timezone(tz)
        .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
        .to_string()
}

/// 将 "HH:MM" 解析为分钟数，用于 cron 编译。
pub fn parse_hhmm(s: &str) -> Result<(u32, u32), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(format!("时间格式应为 HH:MM: {s}"));
    }
    let h = parts[0].parse::<u32>().map_err(|_| format!("非法小时: {s}"))?;
    let m = parts[1].parse::<u32>().map_err(|_| format!("非法分钟: {s}"))?;
    if h > 23 || m > 59 {
        return Err(format!("时间越界: {s}"));
    }
    Ok((h, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_offsets() {
        assert_eq!(parse_timezone("+08:00").unwrap().local_minus_utc(), 8 * 3600);
        assert_eq!(parse_timezone("-05:30").unwrap().local_minus_utc(), -(5 * 3600 + 30 * 60));
        assert_eq!(parse_timezone("UTC").unwrap().local_minus_utc(), 0);
        assert_eq!(parse_timezone("+0800").unwrap().local_minus_utc(), 8 * 3600);
        assert!(parse_timezone("bogus").is_err());
    }

    #[test]
    fn parse_hhmm_ok() {
        assert_eq!(parse_hhmm("08:00").unwrap(), (8, 0));
        assert!(parse_hhmm("25:00").is_err());
        assert!(parse_hhmm("8").is_err());
    }

    #[test]
    fn log_ts_uses_configured_offset() {
        let dt = DateTime::parse_from_rfc3339("2026-11-12T09:44:27.884804Z")
            .unwrap()
            .with_timezone(&Utc);
        let plus8 = parse_timezone("+08:00").unwrap();
        assert_eq!(
            format_log_ts(&dt, &plus8),
            "2026-11-12T17:44:27.884804+08:00"
        );
        // UTC 配置下仍是 Z 语义（但写成 +00:00）
        let utc = parse_timezone("UTC").unwrap();
        assert_eq!(
            format_log_ts(&dt, &utc),
            "2026-11-12T09:44:27.884804+00:00"
        );
    }
}
