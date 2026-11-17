//! tracing_writer 按天轮转测试。

use olivetin_rs::tracing_writer::DailyRollingWriter;
use std::io::Write;

#[test]
fn writes_file_with_panel_prefix() {
    let dir = std::env::temp_dir().join(format!("dw-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut w = DailyRollingWriter::new(dir.clone());
        w.write_all(b"line one\n").unwrap();
        w.flush().unwrap();
    }
    let found = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().starts_with("panel-"));
    assert!(found);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multiple_writes_same_day_single_file() {
    let dir = std::env::temp_dir().join(format!("dw-same-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut w = DailyRollingWriter::new(dir.clone());
        w.write_all(b"a\n").unwrap();
        w.write_all(b"b\n").unwrap();
        w.flush().unwrap();
    }
    // 同一天应只有一个 panel 文件
    let count = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("panel-"))
        .count();
    assert_eq!(count, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
