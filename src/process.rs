//! 进程组创建与整组 kill。
//!
//! Unix 下用 `Command::process_group(0)` 创建独立进程组，超时/关闭时
//! `libc::kill(-pgid, SIGKILL)` 整组杀掉，避免残留子/孙进程。

use tokio::process::Command;

/// 为命令设置进程组（Unix）。非 Unix 下为空操作。
pub fn setup_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// 杀掉整个进程组。`pgid` 为进程组 id（通常等于子进程 pid，因 process_group(0)）。
pub fn kill_process_group(pgid: i32) {
    #[cfg(unix)]
    {
        // SAFETY: `pgid` 来自子进程 pid（正数），取负得到进程组 id 传给 libc::kill；
        // 该调用不触碰内存，仅向内核发送信号，返回值用于上报失败。
        let ret = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            // ESRCH 表示进程组已不存在（正常完成后的清理），无需告警；其余错误上报。
            if err.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(pgid = pgid, error = %err, "kill 进程组失败");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_does_not_panic() {
        let mut cmd = Command::new("true");
        setup_process_group(&mut cmd);
    }
}
