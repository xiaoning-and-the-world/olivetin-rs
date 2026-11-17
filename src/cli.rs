//! CLI 解析：--config / --check-config / --diff / --version / --help。
//!
//! 只做参数识别与帮助打印，不涉及配置加载（加载在 main.rs 中编排）。

/// CLI 解析结果。
#[derive(Debug, Default)]
pub struct CliArgs {
    pub config_path: Option<String>,
    pub check_config: bool,
    pub diff: bool,
    pub version: bool,
    pub help: bool,
}

/// 解析命令行参数。`--diff` 依赖 `--check-config`，单独出现时视为非法（main 中校验）。
pub fn parse() -> CliArgs {
    let mut args = CliArgs::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                args.config_path = it.next();
            }
            "--check-config" => args.check_config = true,
            "--diff" => args.diff = true,
            "--version" | "-V" => args.version = true,
            "--help" | "-h" => args.help = true,
            other => {
                // 未知参数：保留原样以便报错，不静默吞掉
                if args.config_path.is_none() && !other.starts_with('-') {
                    args.config_path = Some(other.to_string());
                } else {
                    eprintln!("未知参数: {other}");
                }
            }
        }
    }
    args
}

/// 打印帮助信息。
pub fn print_help() {
    println!(
        "\
olivetin-rs —— 极轻量 Web 脚本管理面板

用法:
  olivetin-rs [--config <path>] [--check-config] [--diff] [--version] [--help]

选项:
  --config <path>    指定配置文件路径
  --check-config     只加载并校验配置，退出码 0（通过）/ 1（失败）
  --diff             配合 --check-config，输出与当前配置的结构差异
  --version          打印版本、git commit、rustc、构建时间
  --help             显示本帮助

配置查找顺序:
  1. --config <path>
  2. 环境变量 OLIVETIN_CONFIG
  3. ./config.yaml
  4. /etc/olivetin-rs/config.yaml
"
    );
}

/// 打印版本信息。
pub fn print_version() {
    let version = env!("CARGO_PKG_VERSION");
    let git_commit = env!("GIT_COMMIT");
    let rustc = env!("RUSTC_VERSION");
    let built_at = env!("BUILD_TIME");
    println!("olivetin-rs {version}");
    println!("  git_commit: {git_commit}");
    println!("  rustc:      {rustc}");
    println!("  built_at:   {built_at}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_prints() {
        print_help();
    }

    #[test]
    fn version_prints() {
        print_version();
    }
}
