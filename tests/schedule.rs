//! 定时调度测试。

use olivetin_rs::config::ScheduleConfig;
use olivetin_rs::scheduler::{compile_cron, describe, next_run};

fn sched(mode: &str) -> ScheduleConfig {
    ScheduleConfig {
        enabled: true,
        mode: mode.to_string(),
        timezone: Some("+08:00".to_string()),
        skip_if_running: true,
        time: Some("08:00".to_string()),
        weekdays: Some(vec![1, 3, 5]),
        days: Some(vec![1, 15]),
        cron: None,
    }
}

#[test]
fn daily_compile() {
    assert_eq!(compile_cron(&sched("daily")).unwrap(), "0 0 8 * * *");
}

#[test]
fn weekly_compile() {
    let mut s = sched("weekly");
    s.weekdays = Some(vec![1, 3, 5]);
    // 周一=2, 周三=4, 周五=6（cron crate 1=周日）
    assert_eq!(compile_cron(&s).unwrap(), "0 0 8 * * 2,4,6");
}

#[test]
fn weekly_sunday_maps_to_one() {
    let mut s = sched("weekly");
    s.weekdays = Some(vec![7]); // 周日 → cron crate 1
    assert_eq!(compile_cron(&s).unwrap(), "0 0 8 * * 1");
}

#[test]
fn monthly_compile() {
    assert_eq!(compile_cron(&sched("monthly")).unwrap(), "0 0 8 1,15 * *");
}

#[test]
fn cron_passthrough() {
    let mut s = sched("cron");
    s.cron = Some("*/5 * * * *".to_string());
    // 5 段自动补秒
    assert_eq!(compile_cron(&s).unwrap(), "0 */5 * * * *");
}

#[test]
fn cron_six_fields_kept() {
    let mut s = sched("cron");
    s.cron = Some("30 2 * * *".to_string());
    assert_eq!(compile_cron(&s).unwrap(), "0 30 2 * * *");
}

#[test]
fn unknown_mode_errors() {
    let mut s = sched("daily");
    s.mode = "bogus".to_string();
    assert!(compile_cron(&s).is_err());
}

#[test]
fn next_run_in_future() {
    let now = chrono::Utc::now();
    let next = next_run("0 0 8 * * *", "+08:00", now).unwrap();
    assert!(next > now);
}

#[test]
fn describe_variants() {
    assert_eq!(describe(&sched("daily")), "每天 08:00");
    let mut w = sched("weekly");
    w.weekdays = Some(vec![1]);
    assert_eq!(describe(&w), "每周一 08:00");
}
