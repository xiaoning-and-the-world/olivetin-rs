//! 定时调度：四种模式编译为 cron、时区解析、调度循环、热重载、错过触发处理。

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, TimeZone, Utc};
use cron::Schedule;

use crate::config::ScheduleConfig;
use crate::state::AppState;
use crate::time_util::parse_timezone;

/// 将四种模式编译为 cron 表达式（6 段：秒 分 时 日 月 周）。
///
/// cron crate 0.12 要求含秒的 6 段（或 7 段含年）格式，因此内部统一为 6 段。
/// 用户配置的 `cron` 模式若为 5 段标准 Unix cron，自动补秒。
pub fn compile_cron(s: &ScheduleConfig) -> Result<String, String> {
    match s.mode.as_str() {
        "daily" => {
            let (h, m) = crate::time_util::parse_hhmm(s.time.as_deref().ok_or("缺少 time")?)?;
            Ok(format!("0 {m} {h} * * *"))
        }
        "weekly" => {
            let (h, m) = crate::time_util::parse_hhmm(s.time.as_deref().ok_or("缺少 time")?)?;
            let days = s.weekdays.as_ref().ok_or("缺少 weekdays")?;
            // 用户 1-7（周一至周日）→ cron crate 1-7（1=周日,2=周一,...,7=周六）
            // 转换：user_day % 7 + 1（周一=1→2，周日=7→1）
            let mapped: Vec<String> = days
                .iter()
                .map(|d| ((*d % 7) + 1).to_string())
                .collect();
            Ok(format!("0 {m} {h} * * {}", mapped.join(",")))
        }
        "monthly" => {
            let (h, m) = crate::time_util::parse_hhmm(s.time.as_deref().ok_or("缺少 time")?)?;
            let days = s.days.as_ref().ok_or("缺少 days")?;
            let mapped: Vec<String> = days.iter().map(|d| d.to_string()).collect();
            Ok(format!("0 {m} {h} {} * *", mapped.join(",")))
        }
        "cron" => {
            let expr = s.cron.as_ref().ok_or("缺少 cron 表达式")?;
            // 若为 5 段标准 cron（分 时 日 月 周），补秒为 6 段
            Ok(normalize_cron_expr(expr))
        }
        other => Err(format!("未知模式: {other}")),
    }
}

/// 将用户输入的 cron 表达式标准化为 6 段（秒 分 时 日 月 周）。
/// 5 段 → 补秒；6 段 → 原样；7 段 → 原样。
fn normalize_cron_expr(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {}", expr),
        _ => expr.to_string(),
    }
}

/// 计算下一次触发时间（UTC）。`cron_expr` 已编译，`tz` 为调度时区。
///
/// 注意：cron 库只做「日期时间各字段」的匹配，本身不带时区语义。
/// 因此这里用 Utc 时区承载「本地墙上时间」跑迭代，再把结果按 offset
/// 从本地时间还原为 UTC，避免 `Schedule::after(&DateTime<FixedOffset>)`
/// 在不同 cron 版本里把 naive 当 UTC 解释而产生双偏移。
pub fn next_run(cron_expr: &str, tz: &str, from: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let schedule = Schedule::from_str(cron_expr).map_err(|e| format!("cron 解析失败: {e}"))?;
    next_run_with(&schedule, tz, from)
}

/// 展示层专用：按 cron 表达式缓存已编译的 Schedule，
/// 避免 /api/actions（被 1s 轮询）每请求重新编译。调度循环仍走 next_run。
pub fn next_run_cached(
    cron_expr: &str,
    tz: &str,
    from: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Schedule>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let schedule = {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = guard.get(cron_expr) {
            s.clone()
        } else {
            let s = Arc::new(
                Schedule::from_str(cron_expr).map_err(|e| format!("cron 解析失败: {e}"))?,
            );
            // 配置反复改动时不让缓存无限增长。
            if guard.len() > 64 {
                guard.clear();
            }
            guard.insert(cron_expr.to_string(), s.clone());
            s
        }
    };
    next_run_with(&schedule, tz, from)
}

fn next_run_with(
    schedule: &Schedule,
    tz: &str,
    from: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let offset = parse_timezone(tz)?;
    let local_naive = from.with_timezone(&offset).naive_local();
    let pivot = chrono::DateTime::<Utc>::from_naive_utc_and_offset(local_naive, Utc);
    let next_naive = schedule
        .after(&pivot)
        .next()
        .ok_or("无下一次触发")?
        .naive_utc();
    let local_dt = offset
        .from_local_datetime(&next_naive)
        .single()
        .ok_or("本地时间不存在或存在歧义（夏令时）")?;
    Ok(local_dt.with_timezone(&Utc))
}

/// 生成定时描述（人类可读）。
pub fn describe(s: &ScheduleConfig) -> String {
    match s.mode.as_str() {
        "daily" => format!("每天 {}", s.time.as_deref().unwrap_or("")),
        "weekly" => {
            let days = s
                .weekdays
                .as_ref()
                .map(|d| {
                    d.iter()
                        .map(|x| {
                            let names = ["一", "二", "三", "四", "五", "六", "日"];
                            // x 为 1-7；非法值用 ? 占位，避免下标越界 panic
                            names.get(x.saturating_sub(1) as usize).copied().unwrap_or("?")
                        })
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_default();
            format!("每周{} {}", days, s.time.as_deref().unwrap_or(""))
        }
        "monthly" => {
            let days = s
                .days
                .as_ref()
                .map(|d| {
                    d.iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_default();
            format!("每月{}日 {}", days, s.time.as_deref().unwrap_or(""))
        }
        "cron" => format!("Cron: {}", s.cron.as_deref().unwrap_or("")),
        _ => "未知".to_string(),
    }
}

/// 调度循环。
pub async fn scheduler_loop(action_id: String, state: AppState) {
    loop {
        // 只 clone 出本轮需要的数据，立刻释放 ArcSwap Guard。
        // 旧实现把 Guard 一直持有到长达 24h 的 sleep 结束，既阻止旧配置回收，
        // 也让「配置已更新但循环还抱着旧快照」变得难以排查。
        let (enabled, sched_opt, default_tz) = {
            let config = state.config.load();
            match config.find_action(&action_id) {
                Some(a) => (a.enabled, a.schedule.clone(), config.default_timezone.clone()),
                None => (false, None, config.default_timezone.clone()),
            }
        };
        let Some(sched) = sched_opt else {
            break; // action 已删除 / 已移除定时配置
        };
        if !enabled || !sched.enabled {
            tokio::select! {
                _ = state.shutdown.notified() => break,
                _ = state.schedule_reload.notified() => continue,
            }
        }

        let cron_expr = match compile_cron(&sched) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(action = %action_id, error = %e, "cron 编译失败");
                tokio::select! {
                    _ = state.shutdown.notified() => break,
                    _ = state.schedule_reload.notified() => continue,
                }
            }
        };
        let tz = sched.timezone.clone().unwrap_or(default_tz);
        let now = Utc::now();
        let next_utc = match next_run(&cron_expr, &tz, now) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(action = %action_id, error = %e, "计算下次触发失败");
                tokio::select! {
                    _ = state.shutdown.notified() => break,
                    _ = state.schedule_reload.notified() => continue,
                }
            }
        };
        // 下限 1s：next_utc 落在过去时避免 sleep(0) 造成忙循环
        let sleep_dur = (next_utc - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO)
            .max(std::time::Duration::from_secs(1));
        tracing::info!(
            action = %action_id,
            cron = %cron_expr,
            tz = %tz,
            next_run_utc = %next_utc.to_rfc3339(),
            wait_secs = sleep_dur.as_secs(),
            "定时任务已排期"
        );

        tokio::select! {
            _ = tokio::time::sleep(sleep_dur) => {
                let now = Utc::now();
                let drift = now - next_utc;
                if drift.num_seconds() > 60 {
                    tracing::warn!(action = %action_id, missed_by_secs = drift.num_seconds(), "scheduled run missed, skipping");
                    continue;
                }
                if sched.skip_if_running && state.is_action_running(&action_id).await {
                    tracing::info!(action = %action_id, "skipped: already running");
                    continue;
                }
                tracing::info!(action = %action_id, trigger = "schedule", "定时触发");
                let state2 = state.clone();
                let id2 = action_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::executor::run_action(
                        &state2,
                        &id2,
                        crate::log::TriggerSource::Schedule,
                        std::collections::HashMap::new(),
                        None,
                        crate::executor::RunMode::Normal,
                        None,
                    )
                    .await {
                        // 上报具体错误类型，不再 `let _ = e` 吞掉详情
                        tracing::error!(action = %id2, error = ?e, "定时任务执行失败");
                    }
                });
            }
            _ = state.shutdown.notified() => break,
            _ = state.schedule_reload.notified() => continue,
        }
    }
}

/// 按当前配置全量重建所有定时循环，幂等，供启动 / 配置写回 / 热重载调用。
///
/// 关键修复：`start_all` 过去只在进程启动时执行一次，之后所有配置变更都只发
/// `schedule_reload` 通知；Notify 只能唤醒「已经存在」的循环，不会为面板里
/// 后新增的定时脚本创建循环。结果就是新增的 8:00 亮灯任务永远没有调度者。
/// 这里直接对比「当前应启用定时的 action」重建全部 handle，新增、删除、
/// 改时间、改时区都能立即生效，也不依赖 Notify 是否恰好被等到。
pub async fn sync_all(state: &AppState) {
    let config = state.config.load_full();
    let scheduled: Vec<&crate::config::Action> = config
        .actions
        .iter()
        .filter(|a| {
            a.enabled
                && a.schedule
                    .as_ref()
                    .map(|s| s.enabled)
                    .unwrap_or(false)
        })
        .collect();
    let desired: Vec<String> = scheduled.iter().map(|a| a.id.clone()).collect();

    // 签名 = 所有定时 action 的 id + 完整 schedule 配置。
    // 签名未变且现有循环都还活着时直接返回 —— 否则每次热重载都 abort 重建，
    // 会把 sleep 重置到「下一次整点」，任何任务都永远等不到触发点。
    let signature = scheduled
        .iter()
        .map(|a| format!("{}|{:?}", a.id, a.schedule))
        .collect::<Vec<_>>()
        .join("||");
    {
        let mut sig = state.schedule_signature.lock().await;
        let handles = state.schedule_handles.lock().await;
        let all_alive = handles.len() == desired.len()
            && handles.values().all(|h| !h.is_finished());
        if *sig == signature && all_alive {
            return;
        }
        *sig = signature;
    }

    let mut handles = state.schedule_handles.lock().await;
    let drained: Vec<_> = handles.drain().collect();
    for (_, h) in drained {
        h.abort();
    }
    for id in &desired {
        let state2 = state.clone();
        let id2 = id.clone();
        let handle = tokio::spawn(async move {
            scheduler_loop(id2, state2).await;
        });
        handles.insert(id.clone(), handle);
    }
    drop(handles);
    tracing::info!(count = desired.len(), "定时任务已同步（新增/删除/改时间均生效）");
}

/// 为所有启用的定时 Action 启动调度循环（启动时）。
pub async fn start_all(state: &AppState) {
    sync_all(state).await;
}

/// 停止所有调度循环（关闭时调用）。
pub async fn stop_all(state: &AppState) {
    *state.schedule_signature.lock().await = String::new();
    let mut handles = state.schedule_handles.lock().await;
    let drained: Vec<_> = handles.drain().collect();
    for (_, h) in drained {
        h.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // compile_cron / describe 的基础覆盖已由 tests/schedule.rs 集成测试承担，
    // 这里只保留 src 独有、集成测试未覆盖的 next_run 时区精确语义。

    #[test]
    fn next_run_daily_0800_beijing_is_0000_utc() {
        // 北京时间 2026-11-12 09:00 = UTC 01:00，下一次 08:00 应为次日 UTC 00:00
        let now: DateTime<Utc> = "2026-11-12T01:00:00Z".parse().unwrap();
        let next = next_run("0 0 8 * * *", "+08:00", now).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-11-13T00:00:00+00:00");
    }

    #[test]
    fn next_run_daily_0800_beijing_before_trigger() {
        // 北京时间 07:00 = UTC 前一日 23:00，下一次 08:00 应为 1 小时后
        let now: DateTime<Utc> = "2026-11-11T23:00:00Z".parse().unwrap();
        let next = next_run("0 0 8 * * *", "+08:00", now).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-11-12T00:00:00+00:00");
    }

    #[test]
    fn next_run_utc_timezone() {
        let now: DateTime<Utc> = "2026-11-12T01:00:00Z".parse().unwrap();
        let next = next_run("0 0 8 * * *", "UTC", now).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-11-12T08:00:00+00:00");
    }
}
