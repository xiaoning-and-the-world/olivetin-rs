//! 配置热加载：notify 监听 config.yaml，防抖 500ms，校验通过后 ArcSwap 替换。

use std::path::Path;
use std::time::Duration;

use notify::Watcher;

use crate::state::AppState;

/// 启动配置监听。
/// 校验通过后替换配置、重建信号量/限流器、触发发现重扫、通知调度重载。
pub async fn watch(state: AppState, config_path: String) {
    let path = Path::new(&config_path).to_path_buf();
    let Some(parent) = path.parent() else {
        tracing::error!("无法确定配置父目录");
        return;
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(16);

    // 必须按文件名过滤：watcher 监听的是配置父目录，日志文件、token 文件、
    // 以及 reload 自身写日志产生的目录事件都会冒泡上来。若不过滤，
    // 「reload → 写日志 → 又触发 reload」会形成 0.5s 一次的自激循环，
    // 使定时循环被不断重建、sleep 永远被重置，任务永远无法触发。
    let target_name = path.file_name().map(|s| s.to_os_string());
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            if !(event.kind.is_modify() || event.kind.is_create()) {
                return;
            }
            let hit = match &target_name {
                Some(name) => event
                    .paths
                    .iter()
                    .any(|p| p.file_name() == Some(name.as_os_str())),
                None => true,
            };
            if hit {
                let _ = tx.try_send(());
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "创建文件监听失败，热加载不可用");
            return;
        }
    };

    if let Err(e) = watcher.watch(parent, notify::RecursiveMode::NonRecursive) {
        tracing::warn!(error = %e, "注册监听失败");
        return;
    }

    // 防抖：500ms 内多次事件合并
    loop {
        // 等待第一个事件
        if rx.recv().await.is_none() {
            break;
        }
        // 防抖窗口
        let deadline = tokio::time::sleep(Duration::from_millis(500));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                _ = rx.recv() => {
                    // 重新计时
                    deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(500));
                }
            }
        }
        // 重新加载
        reload(&state, &path).await;
    }
}

/// 重新加载配置。
async fn reload(state: &AppState, path: &Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "读取配置文件失败，保留旧配置");
            return;
        }
    };
    // 内容与上次成功加载的完全一致 → 伪事件（例如日志写入），直接跳过。
    // 这是自激循环的第二道防线。
    {
        let snap = state.config_snapshot.lock().await;
        if *snap == content {
            return;
        }
    }
    let mut config: crate::config::Config = match serde_yaml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "配置解析失败，保留旧配置");
            return;
        }
    };
    if let Err(e) = config.apply_interpolation() {
        tracing::warn!(error = %e, "配置插值失败，保留旧配置");
        return;
    }
    config.apply_env_overrides();
    if let Err(e) = config.validate() {
        tracing::warn!(error = %e, "配置校验失败，保留旧配置");
        return;
    }
    // 校验通过后才记录快照；失败时保留旧值，便于后续修正文件后重试
    *state.config_snapshot.lock().await = content;

    // 重建信号量与限流器
    let action_semaphores: std::collections::HashMap<String, std::sync::Arc<tokio::sync::Semaphore>> =
        config
            .actions
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    std::sync::Arc::new(tokio::sync::Semaphore::new(a.concurrency.max(1))),
                )
            })
            .collect();
    state
        .action_semaphores
        .store(std::sync::Arc::new(action_semaphores));
    // 全局并发上限也要跟着重建：否则改 max_concurrent 只有重启才生效。
    state.semaphore.store(std::sync::Arc::new(tokio::sync::Semaphore::new(
        config.max_concurrent.max(1),
    )));
    let new_rate = if config.rate_limit.enabled {
        crate::rate_limit::RateLimiter::new(config.rate_limit.per_minute)
    } else {
        crate::rate_limit::RateLimiter::disabled()
    };
    *state.rate_limiter.lock().await = new_rate;

    // 重新检查 action 的可执行目标
    for action in &config.actions {
        let exists = check_action_binary(action);
        if !exists {
            tracing::warn!(action = %action.id, "可执行目标不存在或不可执行");
        }
        // 重新扫描 versioning（current 失效自动修复）
        if let Some(v) = &action.versioning {
            if v.enabled {
                if let Err(e) = crate::versioning::repair_current(std::path::Path::new(&v.base_dir)) {
                    tracing::warn!(action = %action.id, error = %e, "版本修复失败");
                }
            }
        }
    }

    // 存储新配置
    state.config.store(std::sync::Arc::new(config));

    // 触发发现重扫（异步）
    // 复用全局 discover_semaphore 上限：之前热加载直接 spawn 绕过并发上限，
    // 每次热加载对每个启用 discover 的 action 起一个 --help 探测任务（任务堆积）。
    let cfg = state.config.load();
    for action in cfg.actions.iter() {
        if action.discover.as_ref().map(|d| d.enabled).unwrap_or(false) {
            let state2 = state.clone();
            let id = action.id.clone();
            tokio::spawn(async move {
                let _permit = state2.discover_semaphore.clone().acquire_owned().await;
                crate::discover::run_and_cache(&state2, &id).await;
            });
        }
    }

    drop(cfg); // 释放 ArcSwap Guard，避免跨 await 持有
    // 按最新配置重建全部定时循环：只发 Notify 不会为新增的定时脚本创建循环
    crate::scheduler::sync_all(state).await;

    tracing::info!("配置热加载完成");
}

/// 检查 action 的可执行目标存在性与可执行位。
fn check_action_binary(action: &crate::config::Action) -> bool {
    if action.uses_shell_script() {
        // sh 脚本由执行器在执行前生成，这里只确认命令原文可用
        return action.script_content().is_some();
    }
    match &action.versioning {
        Some(v) if v.enabled => {
            // versioning 开启：检查 current/{binary_name}
            let p = std::path::Path::new(&v.base_dir)
                .join("current")
                .join(&v.binary_name);
            p.exists() && is_executable(&p)
        }
        _ => action
            .exec
            .first()
            .map(|e| {
                let p = std::path::Path::new(e);
                p.exists() && is_executable(p)
            })
            .unwrap_or(false),
    }
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        true
    }
}

/// 启动时校验所有 action 的可执行目标（供 main 调用）。
pub fn check_all_executables(config: &crate::config::Config) {
    for action in &config.actions {
        if !check_action_binary(action) {
            tracing::warn!(action = %action.id, "可执行目标不存在或不可执行");
        }
    }
}
