//! API 相关测试：日志环形缓冲、触发来源序列化、推送状态序列化。

use olivetin_rs::log::{LogEntry, TriggerSource};
use olivetin_rs::push::PushStatus;

#[test]
fn trigger_serializes_lowercase() {
    let manual = serde_json::to_string(&TriggerSource::Manual).unwrap();
    assert_eq!(manual, "\"manual\"");
    let schedule = serde_json::to_string(&TriggerSource::Schedule).unwrap();
    assert_eq!(schedule, "\"schedule\"");
}

#[test]
fn push_status_serializes_snake_case() {
    assert_eq!(serde_json::to_string(&PushStatus::Ok).unwrap(), "\"ok\"");
    assert_eq!(serde_json::to_string(&PushStatus::Disabled).unwrap(), "\"disabled\"");
    // 失败态携带原因，序列化为 {"failed":"boom"}；前端详情页按 .failed 取值。
    let failed = serde_json::to_string(&PushStatus::Failed("boom".to_string())).unwrap();
    assert_eq!(failed, "{\"failed\":\"boom\"}");
}

#[test]
fn log_entry_serializes() {
    let e = LogEntry {
        id: 1,
        action_id: "a".into(),
        action_title: "t".into(),
        action_group: "g".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        started_at_local: "2026-01-01 08:00:00".into(),
        duration_ms: 100,
        exit_code: Some(0),
        stdout: "out".into(),
        stderr: "".into(),
        truncated: false,
        timed_out: false,
        trigger: TriggerSource::Manual,
        push_status: PushStatus::Ok,
        remote_addr: None,
        action_version: Some("v1".into()),
        kind: "run".into(),
            script_path: None,
    };
    let s = serde_json::to_string(&e).unwrap();
    assert!(s.contains("\"action_version\":\"v1\""));
    assert!(s.contains("\"duration_ms\":100"));
}
