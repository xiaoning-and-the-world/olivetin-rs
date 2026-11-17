//! OliveTin-RS 库入口：暴露所有模块供集成测试引用。
//!
//! main.rs 通过 `use olivetin_rs::...` 引用这些模块，二进制仅保留 CLI/runtime 编排。

pub mod auth;
pub mod cli;
pub mod config;
pub mod config_diff;
pub mod config_patch;
pub mod discover;
pub mod executor;
pub mod handler;
pub mod log;
pub mod process;
pub mod push;
pub mod rate_limit;
pub mod scheduler;
pub mod state;
pub mod templates;
pub mod time_util;
pub mod token;
pub mod tracing_writer;
pub mod upload;
pub mod versioning;
pub mod watcher;
