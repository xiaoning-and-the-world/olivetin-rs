//! 时区解析测试。

use olivetin_rs::time_util::{format_local, parse_hhmm, parse_timezone};

#[test]
fn parse_positive_offset() {
    let tz = parse_timezone("+08:00").unwrap();
    assert_eq!(tz.local_minus_utc(), 8 * 3600);
}

#[test]
fn parse_negative_offset() {
    let tz = parse_timezone("-05:30").unwrap();
    assert_eq!(tz.local_minus_utc(), -(5 * 3600 + 30 * 60));
}

#[test]
fn parse_utc() {
    assert_eq!(parse_timezone("UTC").unwrap().local_minus_utc(), 0);
    assert_eq!(parse_timezone("Z").unwrap().local_minus_utc(), 0);
}

#[test]
fn parse_compact() {
    assert_eq!(parse_timezone("+0800").unwrap().local_minus_utc(), 8 * 3600);
}

#[test]
fn parse_invalid() {
    assert!(parse_timezone("bogus").is_err());
    assert!(parse_timezone("+99:99").is_err());
}

#[test]
fn parse_hhmm_valid() {
    assert_eq!(parse_hhmm("08:00").unwrap(), (8, 0));
    assert_eq!(parse_hhmm("23:59").unwrap(), (23, 59));
}

#[test]
fn parse_hhmm_invalid() {
    assert!(parse_hhmm("24:00").is_err());
    assert!(parse_hhmm("8").is_err());
    assert!(parse_hhmm("aa:bb").is_err());
}

#[test]
fn format_local_basic() {
    let tz = parse_timezone("+08:00").unwrap();
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
    let s = format_local(&dt, &tz);
    assert_eq!(s, "1970-01-01 08:00:00");
}
