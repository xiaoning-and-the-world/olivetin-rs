//! 构建脚本：注入 GIT_COMMIT / BUILD_TIME / RUSTC_VERSION 为编译期常量。
//!
//! 三个常量通过 `env!` 在 `main.rs` 中读取，缺失时回退到占位字符串。
//! 跨平台：时间用 Rust 原生生成（不依赖 shell 的 date 命令），git 缺失时回退。

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// 生成编译时刻（本地时区，默认 +08:00）。返回 (可读时间, 短标识)。
///
/// 用本地时间而不是 UTC：面板日志、脚本日志都按 +08:00 显示，
/// 版本信息里的构建时间要和它们对得上，否则「这版是不是刚编的」又要心算。
fn build_time() -> (String, String) {
    let offset = std::env::var("BUILD_TZ_OFFSET")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(8)
        * 3600;
    // CI 可用 SOURCE_DATE_EPOCH 固定构建时间，让增量构建不因 BUILD_ID 变化
    // 触发 fat LTO 全量重链；本地不设时保持「每次编译生成新标识」的运维语义。
    let base = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let secs = (base + offset).max(0) as u64;
    // 简单的手写 UTC 转年月日时分秒（避免引入 chrono 依赖到 build 脚本）
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // 从 Unix epoch 1970-01-01 推算日期（Gregorian 算法）
    let mut year = 1970i64;
    let mut d = days as i64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if d < days_in_year {
            break;
        }
        d -= days_in_year;
        year += 1;
    }
    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1usize;
    for (i, md) in month_days.iter().enumerate() {
        if d < *md {
            month = i + 1;
            break;
        }
        d -= *md;
    }
    let day = d + 1;
    let sign = if offset < 0 { '-' } else { '+' };
    let oh = offset.abs() / 3600;
    let om = (offset.abs() % 3600) / 60;
    (
        format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02} {sign}{oh:02}:{om:02}"),
        format!("{year:04}{month:02}{day:02}-{h:02}{m:02}{s:02}"),
    )
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

fn main() {
    // 每次源码/前端/模板变化都重新生成构建时间：
    // 不声明这些，cargo 会因为只监听了 build.rs 而跳过重跑，
    // 结果版本信息里的「构建于」永远停在第一次编译的时刻。
    // 保留 src 目录：改 Rust 源码后 BUILD_ID 必须变，否则线上版本标识无法区分新旧。
    // 静态资源/模板收窄到实际被 include_str! / askama 使用的文件，
    // 避免目录里无关文件（预览 HTML 等）变动也触发重编译。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=static/app.js");
    println!("cargo:rerun-if-changed=static/style.css");
    println!("cargo:rerun-if-changed=templates/base.html");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=BUILD_TZ_OFFSET");
    println!("cargo:rerun-if-env-changed=RUSTC_VERSION");

    let (build_time, build_id) = build_time();

    // GIT_COMMIT：优先环境变量（CI 场景），其次 git 命令，最后回退 "unknown"
    let git_commit = std::env::var("GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        // 没有 git 时用构建标识代替：至少能让两次编译区分开
        .unwrap_or_else(|| format!("nogit-{build_id}"));

    // RUSTC_VERSION：优先 CI 注入，其次 cargo 提供的 RUSTC 路径，最后回退 rustc 命令。
    let rustc_version = std::env::var("RUSTC_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("RUSTC")
                .ok()
                .and_then(|rustc| {
                    Command::new(rustc)
                        .arg("--version")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                })
        })
        .or_else(|| {
            Command::new("rustc")
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT={git_commit}");
    println!("cargo:rustc-env=BUILD_TIME={build_time}");
    println!("cargo:rustc-env=BUILD_ID={build_id}");
    println!("cargo:rustc-env=RUSTC_VERSION={rustc_version}");
    println!("cargo:rerun-if-changed=build.rs");
}
