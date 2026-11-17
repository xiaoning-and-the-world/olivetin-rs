//! 新旧配置结构对比（--check-config --diff）。
//!
//! 对两份 `Config` 做逐字段 diff，输出人类可读的结构差异列表。

use crate::config::Config;

/// 单条差异。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDiff {
    pub path: String,
    pub old: String,
    pub new: String,
}

/// 计算两份配置的差异。
/// 只比较「结构字段」，不做语义判断；相同字段不输出。
pub fn diff(old: &Config, new: &Config) -> Vec<ConfigDiff> {
    let mut out = Vec::new();
    cmp_u32_opt(&mut out, "config_version", old.config_version, new.config_version);
    cmp_usize(&mut out, "max_concurrent", old.max_concurrent, new.max_concurrent);
    cmp_usize(&mut out, "log_capacity", old.log_capacity, new.log_capacity);
    cmp_str_opt(&mut out, "log_directory", old.log_directory.as_deref(), new.log_directory.as_deref());
    cmp_u32_opt(&mut out, "log_retention_days", old.log_retention_days, new.log_retention_days);
    cmp_u64(&mut out, "log_max_total_mb", old.log_max_total_mb, new.log_max_total_mb);
    cmp_u64(&mut out, "min_free_mb", old.min_free_mb, new.min_free_mb);
    cmp_usize(&mut out, "max_output_kb", old.max_output_kb, new.max_output_kb);
    cmp_str(&mut out, "default_timezone", &old.default_timezone, &new.default_timezone);
    cmp_bool(&mut out, "inject_tz", old.inject_tz, new.inject_tz);
    cmp_bool(&mut out, "rate_limit.enabled", old.rate_limit.enabled, new.rate_limit.enabled);
    cmp_u32(&mut out, "rate_limit.per_minute", old.rate_limit.per_minute, new.rate_limit.per_minute);
    cmp_bool(&mut out, "auth.enabled", old.auth.enabled, new.auth.enabled);
    cmp_str_opt(&mut out, "auth.token_file", old.auth.token_file.as_deref(), new.auth.token_file.as_deref());
    cmp_bool(&mut out, "push.enabled", old.push.enabled, new.push.enabled);

    // actions：按 id 对齐
    let old_ids: Vec<String> = old.actions.iter().map(|a| a.id.clone()).collect();
    let new_ids: Vec<String> = new.actions.iter().map(|a| a.id.clone()).collect();
    for id in old_ids.iter().chain(new_ids.iter()) {
        let o = old.actions.iter().find(|a| &a.id == id);
        let n = new.actions.iter().find(|a| &a.id == id);
        match (o, n) {
            (Some(a), Some(b)) => {
                cmp_str(&mut out, &format!("actions.{id}.title"), &a.title, &b.title);
                cmp_bool(&mut out, &format!("actions.{id}.enabled"), a.enabled, b.enabled);
                cmp_str(&mut out, &format!("actions.{id}.group"), &a.group, &b.group);
                let old_exec = a.exec.join(" ");
                let new_exec = b.exec.join(" ");
                cmp_str(&mut out, &format!("actions.{id}.exec"), &old_exec, &new_exec);
            }
            (Some(_), None) => out.push(ConfigDiff {
                path: format!("actions.{id}"),
                old: "存在".to_string(),
                new: "已删除".to_string(),
            }),
            (None, Some(_)) => out.push(ConfigDiff {
                path: format!("actions.{id}"),
                old: "不存在".to_string(),
                new: "新增".to_string(),
            }),
            (None, None) => {}
        }
    }
    out
}

fn cmp_str(out: &mut Vec<ConfigDiff>, path: &str, old: &str, new: &str) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
}
fn cmp_str_opt(out: &mut Vec<ConfigDiff>, path: &str, old: Option<&str>, new: Option<&str>) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.map(|s| s.to_string()).unwrap_or_else(|| "null".to_string()),
            new: new.map(|s| s.to_string()).unwrap_or_else(|| "null".to_string()),
        });
    }
}
fn cmp_bool(out: &mut Vec<ConfigDiff>, path: &str, old: bool, new: bool) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
}
fn cmp_usize(out: &mut Vec<ConfigDiff>, path: &str, old: usize, new: usize) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
}
fn cmp_u32(out: &mut Vec<ConfigDiff>, path: &str, old: u32, new: u32) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
}
fn cmp_u64(out: &mut Vec<ConfigDiff>, path: &str, old: u64, new: u64) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.to_string(),
            new: new.to_string(),
        });
    }
}
fn cmp_u32_opt(out: &mut Vec<ConfigDiff>, path: &str, old: Option<u32>, new: Option<u32>) {
    if old != new {
        out.push(ConfigDiff {
            path: path.to_string(),
            old: old.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
            new: new.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config::default()
    }

    // 结构差异检测的基础覆盖已由 tests/config_diff.rs 集成测试承担，
    // 此处不再重复同名用例。
}
