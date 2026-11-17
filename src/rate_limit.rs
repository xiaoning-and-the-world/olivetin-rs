//! 全局速率限制：手写令牌桶。
//!
//! 手动触发受限，调度触发不受限。超限返回 true，由调用方返回 429 + Retry-After。

use std::time::Instant;

/// 令牌桶限流器。
pub struct RateLimiter {
    enabled: bool,
    per_minute: u32,
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
}

impl RateLimiter {
    /// 新建限流器。`enabled=false` 时 `allow` 恒返回 true。
    pub fn new(per_minute: u32) -> Self {
        RateLimiter {
            enabled: true,
            per_minute,
            tokens: per_minute as f64,
            last_refill: Instant::now(),
            capacity: per_minute as f64,
        }
    }

    /// 创建禁用状态的限流器。
    pub fn disabled() -> Self {
        RateLimiter {
            enabled: false,
            per_minute: 0,
            tokens: 0.0,
            last_refill: Instant::now(),
            capacity: 0.0,
        }
    }

    /// 尝试获取一个令牌。成功返回 true。
    pub fn allow(&mut self) -> bool {
        if !self.enabled {
            return true;
        }
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        // 每秒钟补充 per_minute/60 个令牌
        let added = elapsed * (self.per_minute as f64 / 60.0);
        self.tokens = (self.tokens + added).min(self.capacity);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_always_allows() {
        let mut r = RateLimiter::disabled();
        for _ in 0..1000 {
            assert!(r.allow());
        }
    }

    #[test]
    fn enabled_limits() {
        let mut r = RateLimiter::new(5);
        let mut allowed = 0;
        for _ in 0..100 {
            if r.allow() {
                allowed += 1;
            }
        }
        // 初始容量为 per_minute，所以最多 5 个
        assert_eq!(allowed, 5);
    }
}
