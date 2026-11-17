//! 速率限制测试。

use olivetin_rs::rate_limit::RateLimiter;

#[test]
fn disabled_allows_all() {
    let mut r = RateLimiter::disabled();
    for _ in 0..10000 {
        assert!(r.allow());
    }
}

#[test]
fn enabled_limits_to_capacity() {
    let mut r = RateLimiter::new(5);
    let mut allowed = 0;
    for _ in 0..100 {
        if r.allow() {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 5);
}

#[test]
fn zero_capacity_denies() {
    let mut r = RateLimiter::new(0);
    assert!(!r.allow());
}
