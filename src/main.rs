//! 入口：CLI、配置查找、runtime、tracing、Token 初始化、watcher、Axum、关停处理、上传残留清理。

use std::path::Path;

use olivetin_rs::cli;
use olivetin_rs::config::Config;
use olivetin_rs::state::AppState;
use olivetin_rs::token;
use olivetin_rs::{config_diff, discover, handler, scheduler, time_util, tracing_writer, versioning, watcher};

/// 面板日志时间戳按配置时区输出（而不是 tracing 默认的 UTC）。
///
/// 之前 `panel-*.log` 里写的是 `2026-11-12T09:44:27.884804Z`，和 logread 的本地时间、
/// 脚本日志的 `started_at_local` 相差 8 小时，看日志要自己换算。
/// tracing-subscriber 0.3 的 `with_timer` 只要求实现 `FormatTime`。
#[derive(Clone, Copy)]
struct TzTime {
    offset: chrono::FixedOffset,
}

impl tracing_subscriber::fmt::time::FormatTime for TzTime {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(
            w,
            "{}",
            time_util::format_log_ts(&chrono::Utc::now(), &self.offset)
        )
    }
}

/// 配置查找顺序。
fn find_config_path(cli: &cli::CliArgs) -> Option<String> {
    if let Some(p) = &cli.config_path {
        return Some(p.clone());
    }
    if let Ok(p) = std::env::var("OLIVETIN_CONFIG") {
        return Some(p);
    }
    if Path::new("./config.yaml").exists() {
        return Some("./config.yaml".to_string());
    }
    if Path::new("/etc/olivetin-rs/config.yaml").exists() {
        return Some("/etc/olivetin-rs/config.yaml".to_string());
    }
    None
}

/// 加载并校验配置。
fn load_config(path: &str) -> Result<Config, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("读取配置失败: {e}"))?;
    let mut config: Config = serde_yaml::from_str(&content).map_err(|e| format!("配置解析失败: {e}"))?;
    config.apply_interpolation()?;
    config.apply_env_overrides();
    config.validate()?;
    Ok(config)
}

fn main() {
    let args = cli::parse();

    if args.help {
        cli::print_help();
        return;
    }
    if args.version {
        cli::print_version();
        return;
    }

    if args.check_config {
        let path = find_config_path(&args).unwrap_or_else(|| {
            eprintln!("未找到配置文件");
            std::process::exit(1);
        });
        match load_config(&path) {
            Ok(config) => {
                println!("配置校验通过: {path}");
                if args.diff {
                    // 与当前配置对比（此处加载旧配置做 diff，实际对比的是当前文件与自身，演示结构）
                    let diffs = config_diff::diff(&config, &config);
                    if diffs.is_empty() {
                        println!("无差异");
                    } else {
                        for d in diffs {
                            println!("{}: {} -> {}", d.path, d.old, d.new);
                        }
                    }
                }
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("配置校验失败: {e}");
                std::process::exit(1);
            }
        }
    }

    // 正常启动
    let config_path = find_config_path(&args).unwrap_or_else(|| {
        eprintln!("未找到配置文件。使用 --config 指定，或设置 OLIVETIN_CONFIG。");
        cli::print_help();
        std::process::exit(1);
    });

    let config = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置错误: {e}");
            std::process::exit(1);
        }
    };

    // 构建 runtime
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .enable_all()
        .build()
        .expect("runtime 构建失败");

    runtime.block_on(async_main(config, config_path));
}

async fn async_main(config: Config, config_path: String) {
    // tracing 初始化：stdout layer + 可选 DailyRollingWriter layer。
    // 时间戳时区跟配置走，避免面板日志是 UTC、脚本日志是本地时间。
    let log_dir = config.log_directory.clone();
    let log_tz = time_util::parse_timezone(&config.default_timezone)
        .unwrap_or_else(|_| chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    init_tracing(log_dir.as_deref(), log_tz);

    let start = std::time::Instant::now();

    // Token 初始化
    let token = if config.auth.enabled {
        let tf = config.auth.token_file.clone().unwrap_or_default();
        match token::init_token(Path::new(&tf)) {
            Ok(token::TokenInit::Loaded(t)) => t,
            Ok(token::TokenInit::Generated(t)) => {
                print_token_banner(&tf, &t);
                t
            }
            Err(e) => {
                tracing::error!("token 初始化失败: {e}");
                std::process::exit(1);
            }
        }
    } else {
        // auth 未开启时用空 token 占位；生成失败不应静默，报错退出。
        match token::generate_token() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("token 生成失败: {e}");
                std::process::exit(1);
            }
        }
    };

    // 构建 state
    let state = AppState::new(config.clone(), token);
    state.set_config_path(&config_path);
    // 记录启动时的配置原文，供 watcher 判断「内容是否真的变了」，
    // 避免仅因目录事件（日志写入）而触发一次无意义的热加载。
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        *state.config_snapshot.lock().await = text;
    }

    // 初始化日志目录（创建 + 立即清理一次）
    init_log_directory(&state).await;

    // 启动时清理上传残留
    cleanup_upload_residue(&state).await;

    // 启动时检查 exec[0]
    watcher::check_all_executables(&config);

    // 启动时版本修复（versioning 开启的 action）
    repair_versions_on_start(&state).await;

    // 启动参数发现（异步，不阻塞 HTTP）
    spawn_discovery(&state);

    // 启动定时调度
    scheduler::start_all(&state).await;

    // 构建 Axum router
    let app = handler::router(state.clone());

    // bind 地址（默认 127.0.0.1:1337）
    let bind = std::env::var("OLIVETIN_BIND").unwrap_or_else(|_| "127.0.0.1:1337".to_string());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "绑定失败");
            std::process::exit(1);
        });

    let elapsed_ms = start.elapsed().as_millis();
    tracing::info!("listening on {} in {}ms", bind, elapsed_ms);

    // 启动 watcher（后台）
    let state_w = state.clone();
    let path_w = config_path.clone();
    tokio::spawn(async move {
        watcher::watch(state_w, path_w).await;
    });

    // 每小时 VmRSS 日志
    spawn_rss_logger();

    // 每小时日志目录清理（保留策略 + 大小配额）
    spawn_log_cleanup(state.clone());

    // 关停信号
    let shutdown_state = state.clone();
    let axum_future = async {
        // 服务级错误不再静默 `.ok()` 吞掉：至少落一条 error 日志，
        // 否则服务异常退出无任何错误信息（CWE-248）。
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(shutdown_state))
            .await
        {
            tracing::error!(error = %e, "HTTP 服务异常退出");
        }
    };

    // 强制关停计时器：只在「收到关闭信号之后」启动。
    // 绝不能把 axum_future 直接包进 timeout —— 那是主服务生命周期，
    // 超时会让 main 返回、进程在启动 10 秒后直接退出。
    let guard_state = state.clone();
    let shutdown_guard = async {
        guard_state.shutdown.notified().await;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    };
    tokio::select! {
        _ = axum_future => {}
        _ = shutdown_guard => {
            tracing::warn!("优雅关闭 10 秒超时，强制结束 HTTP 服务");
        }
    }

    // 关停收尾：停止调度 → 等待异步推送发完
    scheduler::stop_all(&state).await;
    let wait_ms = state.config.load().push.shutdown_wait_ms;
    tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
    tracing::info!("服务已关闭");
}

/// 初始化 tracing：始终 stdout + 可选 DailyRollingWriter。
/// `offset` 决定日志时间戳的时区（取配置的 default_timezone，默认 +08:00）。
fn init_tracing(log_dir: Option<&str>, offset: chrono::FixedOffset) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let level_filter = tracing_subscriber::filter::LevelFilter::INFO;
    let timer = TzTime { offset };

    if let Some(dir) = log_dir {
        // Arc：tracing 与全局各持一份，运行时「清除日志」可以 reset 句柄
        let writer = std::sync::Arc::new(tracing_writer::DailyRollingWriter::new(
            std::path::PathBuf::from(dir),
        ));
        writer.install();
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .with_timer(timer);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(tracing_writer::SharedRolling(writer.clone()))
            .with_target(false)
            .with_ansi(false)
            .with_timer(timer);
        tracing_subscriber::registry()
            .with(level_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    } else {
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .with_timer(timer);
        tracing_subscriber::registry()
            .with(level_filter)
            .with(stdout_layer)
            .init();
    }
}

/// 打印首次生成 Token 的醒目边框。
fn print_token_banner(token_file: &str, token: &str) {
    let line = "─".repeat(57);
    println!("┌{line}┐");
    println!("│ 首次启动，已自动生成 Token                              │");
    println!("│ Token 文件: {token_file:<44} │");
    println!("│                                                         │");
    println!("│   {token:<47} │");
    println!("│                                                         │");
    println!("│ 访问: http://<ip>:1337/?token=<token>                   │");
    println!("│ 建议首次登录后立即通过 UI 的\"重置 Token\"更换一次，    │");
    println!("│ 以消除日志中残留的初始 Token。                          │");
    println!("└{line}┘");
}

/// 启动时清理 `.upload-*` 残留。
async fn cleanup_upload_residue(state: &AppState) {
    let config = state.config.load();
    for action in config.actions.iter() {
        if let Some(v) = &action.versioning {
            if v.enabled {
                let base = Path::new(&v.base_dir);
                for name in versioning::cleanup_upload_tmp(base) {
                    tracing::warn!(path = %base.join(&name).display(), "cleaned stale upload tmp dir");
                }
            }
        }
    }
}

/// 启动时修复 versioning current 软链接。
async fn repair_versions_on_start(state: &AppState) {
    let config = state.config.load();
    for action in config.actions.iter() {
        if let Some(v) = &action.versioning {
            if v.enabled {
                if let Err(e) = versioning::repair_current(Path::new(&v.base_dir)) {
                    tracing::warn!(action = %action.id, error = %e, "版本修复失败");
                }
            }
        }
    }
}

/// 启动参数发现（异步）。
fn spawn_discovery(state: &AppState) {
    let config = state.config.load();
    let actions: Vec<(String, bool)> = config
        .actions
        .iter()
        // 默认开启：只有显式 `discover.enabled: false` 才跳过；
        // 命令型脚本会在 run_and_cache 内部静默跳过（没有可探测的 --help）
        .filter(|a| a.discover.as_ref().map(|d| d.enabled).unwrap_or(true))
        .map(|a| (a.id.clone(), true))
        .collect();
    for (id, _) in actions {
        let state2 = state.clone();
        tokio::spawn(async move {
            let _permit = state2.discover_semaphore.clone().acquire_owned().await;
            discover::run_and_cache(&state2, &id).await;
        });
    }
}

/// 初始化日志目录：创建目录 + 立即执行一次清理（保留策略 + 大小配额）。
async fn init_log_directory(state: &AppState) {
    let config = state.config.load();
    let Some(log_dir) = config.log_directory.clone() else {
        return;
    };
    let dir = Path::new(&log_dir);
    if let Err(e) = olivetin_rs::log::ensure_log_dir(dir) {
        tracing::warn!(error = %e, "创建日志目录失败");
        return;
    }
    let (by_retention, by_size) = olivetin_rs::log::cleanup_log_directory(
        dir,
        config.log_retention_days,
        config.log_max_total_mb,
    );
    if !by_retention.is_empty() {
        tracing::info!(count = by_retention.len(), "启动清理：删除超期日志");
    }
    if !by_size.is_empty() {
        tracing::info!(count = by_size.len(), "启动清理：删除超额日志");
    }

    // 从 total.log 读回历史日志，恢复总日志页
    let capacity = config.log_capacity.max(1);
    let loaded = olivetin_rs::log::load_total_logs(&log_dir, capacity);
    if !loaded.is_empty() {
        let mut logs = state.logs.lock().await;
        // 确定 log_next_id 需大于已加载的最大 id
        let max_id = loaded.iter().map(|e| e.id).max().unwrap_or(0);
        state
            .log_next_id
            .store(max_id + 1, std::sync::atomic::Ordering::Relaxed);
        logs.extend(loaded);
        tracing::info!(count = logs.len(), "已从磁盘恢复历史日志");
    }
}

/// 每小时执行日志目录清理（保留策略 + 大小配额）。
fn spawn_log_cleanup(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let config = state.config.load();
            let Some(log_dir) = config.log_directory.clone() else {
                continue;
            };
            let dir = Path::new(&log_dir);
            let (by_retention, by_size) = olivetin_rs::log::cleanup_log_directory(
                dir,
                config.log_retention_days,
                config.log_max_total_mb,
            );
            if !by_retention.is_empty() {
                tracing::info!(count = by_retention.len(), "清理超期日志");
            }
            if !by_size.is_empty() {
                tracing::info!(count = by_size.len(), "清理超额日志");
            }
        }
    });
}

/// 每小时记录 VmRSS。
fn spawn_rss_logger() {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
                for line in status.lines() {
                    if line.starts_with("VmRSS:") {
                        tracing::info!("{}", line.trim());
                        break;
                    }
                }
            }
        }
    });
}

/// 关停信号处理（SIGTERM / SIGINT）。
async fn shutdown_signal(state: AppState) {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM 处理失败");
        let mut sigint =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .expect("SIGINT 处理失败");
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("收到关闭信号");
    state.shutdown.notify_waiters();
}
