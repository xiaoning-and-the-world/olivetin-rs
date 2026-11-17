//! AppState：全局共享状态，Clone 安全。
//!
//! 所有并发访问通过 Arc<ArcSwap> / Arc<Mutex> / Arc<Semaphore> 保护。
//! 含当前 Token、运行集合、上传进行中集合、版本活跃记录等。

use crate::config::Config;
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, Notify, Semaphore};

use crate::discover::DiscoveredParams;
use crate::log::LogEntry;
use crate::rate_limit::RateLimiter;

/// 全局共享状态。
pub struct AppState {
    pub config: Arc<ArcSwap<Config>>,
    pub current_token: Arc<ArcSwap<String>>,
    pub semaphore: Arc<ArcSwap<Semaphore>>,
    pub action_semaphores: Arc<ArcSwap<HashMap<String, Arc<Semaphore>>>>,
    pub discover_semaphore: Arc<Semaphore>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
    pub logs: Arc<Mutex<VecDeque<LogEntry>>>,
    pub log_next_id: Arc<AtomicU64>,
    pub discovered: Arc<ArcSwap<HashMap<String, DiscoveredParams>>>,
    pub shutdown: Arc<Notify>,
    pub schedule_reload: Arc<Notify>,
    pub schedule_handles: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// 当前已排期的定时任务签名；签名未变时不必重建循环（否则会重置 sleep）。
    pub schedule_signature: Arc<Mutex<String>>,
    /// 上次成功加载的配置原文；内容未变时跳过热加载，防止日志写入引发自激循环。
    pub config_snapshot: Arc<Mutex<String>>,
    pub running: Arc<Mutex<HashSet<String>>>,
    pub queued: Arc<AtomicUsize>,
    pub start_time: DateTime<Utc>,
    pub pid: u32,
    pub warn_throttle: Arc<Mutex<HashMap<String, Instant>>>,
    pub last_active_versions: Arc<Mutex<HashMap<String, String>>>,
    pub upload_in_progress: Arc<Mutex<HashSet<String>>>,
    pub config_path: Arc<ArcSwap<String>>,
    pub running_pids: Arc<Mutex<HashMap<String, i32>>>,
    /// 已被请求取消、但尚未被执行器清理的 action_id 集合。
    pub cancel_flags: Arc<Mutex<HashSet<String>>>,
    /// 取消信号广播：排队中的执行器 subscribe 后可在等信号量时被唤醒。
    pub cancel_tx: Arc<tokio::sync::broadcast::Sender<String>>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        AppState {
            config: Arc::clone(&self.config),
            current_token: Arc::clone(&self.current_token),
            semaphore: Arc::clone(&self.semaphore),
            action_semaphores: Arc::clone(&self.action_semaphores),
            discover_semaphore: Arc::clone(&self.discover_semaphore),
            rate_limiter: Arc::clone(&self.rate_limiter),
            logs: Arc::clone(&self.logs),
            log_next_id: Arc::clone(&self.log_next_id),
            discovered: Arc::clone(&self.discovered),
            shutdown: Arc::clone(&self.shutdown),
            schedule_reload: Arc::clone(&self.schedule_reload),
            schedule_handles: Arc::clone(&self.schedule_handles),
            schedule_signature: Arc::clone(&self.schedule_signature),
            config_snapshot: Arc::clone(&self.config_snapshot),
            running: Arc::clone(&self.running),
            queued: Arc::clone(&self.queued),
            start_time: self.start_time,
            pid: self.pid,
            warn_throttle: Arc::clone(&self.warn_throttle),
            last_active_versions: Arc::clone(&self.last_active_versions),
            upload_in_progress: Arc::clone(&self.upload_in_progress),
            config_path: Arc::clone(&self.config_path),
            running_pids: Arc::clone(&self.running_pids),
            cancel_flags: Arc::clone(&self.cancel_flags),
            cancel_tx: Arc::clone(&self.cancel_tx),
        }
    }
}

impl AppState {
    /// 新建 AppState，由 main 调用一次。
    pub fn new(config: Config, token: String) -> Self {
        let max_concurrent = config.max_concurrent.max(1);
        let action_semaphores: HashMap<String, Arc<Semaphore>> = config
            .actions
            .iter()
            .map(|a| (a.id.clone(), Arc::new(Semaphore::new(a.concurrency.max(1)))))
            .collect();
        AppState {
            config: Arc::new(ArcSwap::from_pointee(config)),
            current_token: Arc::new(ArcSwap::from_pointee(token)),
            semaphore: Arc::new(ArcSwap::from_pointee(Semaphore::new(max_concurrent))),
            action_semaphores: Arc::new(ArcSwap::from_pointee(action_semaphores)),
            discover_semaphore: Arc::new(Semaphore::new(2)),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(30))),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            log_next_id: Arc::new(AtomicU64::new(1)),
            discovered: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            shutdown: Arc::new(Notify::new()),
            schedule_reload: Arc::new(Notify::new()),
            schedule_handles: Arc::new(Mutex::new(HashMap::new())),
            schedule_signature: Arc::new(Mutex::new(String::new())),
            config_snapshot: Arc::new(Mutex::new(String::new())),
            running: Arc::new(Mutex::new(HashSet::new())),
            queued: Arc::new(AtomicUsize::new(0)),
            start_time: Utc::now(),
            pid: std::process::id(),
            warn_throttle: Arc::new(Mutex::new(HashMap::new())),
            last_active_versions: Arc::new(Mutex::new(HashMap::new())),
            upload_in_progress: Arc::new(Mutex::new(HashSet::new())),
            config_path: Arc::new(ArcSwap::from_pointee(String::new())),
            running_pids: Arc::new(Mutex::new(HashMap::new())),
            cancel_flags: Arc::new(Mutex::new(HashSet::new())),
            cancel_tx: Arc::new(tokio::sync::broadcast::channel(64).0),
        }
    }

    /// 设置配置文件路径（用于设置页持久化写回）。
    pub fn set_config_path(&self, path: &str) {
        self.config_path.store(Arc::new(path.to_string()));
    }

    /// 当前配置文件路径。
    pub fn get_config_path(&self) -> String {
        self.config_path.load().as_ref().clone()
    }

    /// 当前 Token。
    pub fn token(&self) -> String {
        self.current_token.load().as_ref().clone()
    }

    /// 是否正在运行。
    pub async fn is_action_running(&self, id: &str) -> bool {
        self.running.lock().await.contains(id)
    }

    /// 标记运行。返回 false 表示已在运行。
    pub async fn mark_running(&self, id: &str) -> bool {
        self.running.lock().await.insert(id.to_string())
    }

    /// 取消运行标记。
    pub async fn unmark_running(&self, id: &str) {
        self.running.lock().await.remove(id);
    }

    /// 运行中数量。
    pub async fn running_count(&self) -> usize {
        self.running.lock().await.len()
    }

    /// Action 状态（idle / running / queued）。
    pub async fn action_state(&self, id: &str) -> String {
        if self.running.lock().await.contains(id) {
            "running".to_string()
        } else {
            "idle".to_string()
        }
    }

    /// 警告节流：同一 key 在指定间隔内只打印一次 warn。
    pub async fn warn_throttled(&self, key: &str, interval_secs: u64) -> bool {
        let mut map = self.warn_throttle.lock().await;
        let now = Instant::now();
        if let Some(last) = map.get(key) {
            if now.duration_since(*last).as_secs() < interval_secs {
                return false;
            }
        }
        map.insert(key.to_string(), now);
        true
    }
}
