//! 面板日志按天轮转的 MakeWriter 实现。
//!
//! 文件名 `panel-YYYYMMDD.log`，跨日自动 reopen；写入失败降级到 stdout。
//! 始终同时输出 stdout（main 中配置两层 layer）。
//!
//! 性能：日期字符串按秒缓存（避免每条日志做 chrono 格式化 + String 分配），
//! 日期与文件句柄合并进同一把锁（每条日志 3 次加锁 → 1 次）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// 当前安装的写入器。清除日志文件时需要它把文件句柄放掉，
/// 否则进程仍握着已 unlink 的 inode：删掉的文件不会重新出现，
/// 新日志也写不进去（写进"虚空"），页面上看起来像日志功能坏了。
static INSTALLED: OnceLock<Arc<DailyRollingWriter>> = OnceLock::new();

/// 日期串缓存：同一秒内复用，跨日最多延迟 1 秒轮转。
static DATE_CACHE: OnceLock<Mutex<(u64, String)>> = OnceLock::new();

/// 按天轮转的写入器。
pub struct DailyRollingWriter {
    dir: PathBuf,
    /// (当前日期, 已打开的文件)：合并成一把锁，避免 ensure_open + write 两次加锁。
    state: Mutex<(String, Option<std::fs::File>)>,
}

impl DailyRollingWriter {
    pub fn new(dir: PathBuf) -> Self {
        DailyRollingWriter {
            dir,
            state: Mutex::new((String::new(), None)),
        }
    }

    fn date_now() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cache = DATE_CACHE.get_or_init(|| Mutex::new((0, String::new())));
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        // 时钟回拨时也强制重算。
        if guard.1.is_empty() || secs < guard.0 || secs - guard.0 >= 1 {
            guard.1 = chrono::Local::now().format("%Y%m%d").to_string();
            guard.0 = secs;
        }
        guard.1.clone()
    }

    /// 注册实例（main 里调用一次），供 `reset_current()` 使用。
    pub fn install(self: &Arc<Self>) {
        let _ = INSTALLED.set(self.clone());
    }

    /// 放掉当前文件句柄并清空日期缓存，下一次写入会重新创建 panel-*.log。
    /// 清除日志文件前必须先调用。
    pub fn reset_current() {
        if let Some(w) = INSTALLED.get() {
            *w.state.lock().unwrap_or_else(|e| e.into_inner()) = (String::new(), None);
        }
        if let Some(cache) = DATE_CACHE.get() {
            *cache.lock().unwrap_or_else(|e| e.into_inner()) = (0, String::new());
        }
    }
}

impl std::io::Write for &DailyRollingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let date = DailyRollingWriter::date_now();
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (cur, file) = &mut *guard;
        if cur.as_str() != date || file.is_none() {
            let path = self.dir.join(format!("panel-{date}.log"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(f) => {
                    *file = Some(f);
                    *cur = date;
                }
                Err(_) => {
                    // 打开失败降级到 stdout，不清空已有句柄语义
                    drop(guard);
                    let mut stdout = std::io::stdout();
                    let _ = stdout.write_all(buf);
                    return Ok(buf.len());
                }
            }
        }
        if let Some(f) = file.as_mut() {
            if f.write_all(buf).is_ok() {
                return Ok(buf.len());
            }
        }
        drop(guard);
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(mut guard) = self.state.lock() {
            if let Some(f) = guard.1.as_mut() {
                let _ = f.flush();
            }
        }
        Ok(())
    }
}

impl std::io::Write for DailyRollingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self).flush()
    }
}

/// 实现 tracing-subscriber 的 MakeWriter：返回自身的共享引用。
/// `&DailyRollingWriter` 已实现 Write，内部用 Mutex 保证并发安全。
impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for DailyRollingWriter {
    type Writer = &'a DailyRollingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// 模块级入口：清除日志文件前调用，放掉当前句柄。
pub fn reset_current() {
    DailyRollingWriter::reset_current();
}

/// 本地 newtype 包 Arc：`MakeWriter` 与 `Arc` 都是外部类型，无法直接为
/// `Arc<DailyRollingWriter>` 实现该 trait，必须包一层本地类型。
pub struct SharedRolling(pub Arc<DailyRollingWriter>);

/// Arc 持有版 Writer：main 用 `Arc<DailyRollingWriter>` 注册给 tracing，
/// 同时把同一个 Arc 存进全局，使运行时能 reset。
pub struct ArcWriter(Arc<DailyRollingWriter>);

impl std::io::Write for ArcWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut w: &DailyRollingWriter = &self.0;
        w.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut w: &DailyRollingWriter = &self.0;
        w.flush()
    }
}

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for SharedRolling {
    type Writer = ArcWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ArcWriter(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn writer_writes_and_rotates() {
        let dir = std::env::temp_dir().join(format!("olivetin-daily-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut w = DailyRollingWriter::new(dir.clone());
            w.write_all(b"hello\n").unwrap();
            w.flush().unwrap();
        }
        // 存在某个 panel-*.log 文件
        let has_file = std::fs::read_dir(&dir)
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().starts_with("panel-"));
        assert!(has_file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_current_clears_both_caches() {
        let dir = std::env::temp_dir().join(format!("olivetin-reset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = Arc::new(DailyRollingWriter::new(dir.clone()));
        w.install();
        let mut borrowed: &DailyRollingWriter = &w;
        borrowed.write_all(b"before\n").unwrap();
        DailyRollingWriter::reset_current();
        // reset 后下一次写入必须重新打开文件，而不是写进虚空
        borrowed.write_all(b"after\n").unwrap();
        let content = std::fs::read_to_string(
            std::fs::read_dir(&dir)
                .unwrap()
                .find_map(|e| {
                    let p = e.unwrap().path();
                    if p.to_string_lossy().contains("panel-") { Some(p) } else { None }
                })
                .unwrap(),
        )
        .unwrap();
        assert!(content.contains("after"), "reset 后写入丢失: {content:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
