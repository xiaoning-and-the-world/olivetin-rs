//! 配置 diff 测试。

use olivetin_rs::config::Config;
use olivetin_rs::config_diff::diff;

fn base() -> Config {
    Config::default()
}

#[test]
fn identical_configs_have_no_diff() {
    assert!(diff(&base(), &base()).is_empty());
}

#[test]
fn detects_scalar_change() {
    let mut a = base();
    a.max_concurrent = 2;
    let mut b = base();
    b.max_concurrent = 4;
    let d = diff(&a, &b);
    assert!(d.iter().any(|x| x.path == "max_concurrent"));
}

#[test]
fn detects_timezone_change() {
    let mut a = base();
    a.default_timezone = "+08:00".to_string();
    let mut b = base();
    b.default_timezone = "-05:00".to_string();
    let d = diff(&a, &b);
    assert!(d.iter().any(|x| x.path == "default_timezone"));
}
