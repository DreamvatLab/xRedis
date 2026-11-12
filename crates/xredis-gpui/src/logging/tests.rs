//! 文件日志的测试:写进配置目录的 `logs/`、级别随构建、按天轮转最多留 7 个。

use std::io::Write;

use super::*;

fn log_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(log_dir(dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn logs_go_to_the_logs_folder_under_the_config_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = appender(tmp.path()).expect("an appender");
    writeln!(a, "hello log").unwrap();
    a.flush().unwrap();
    let files = log_files(tmp.path());
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].starts_with("xredis.") && files[0].ends_with(".log"),
        "{files:?}"
    );
    let text = std::fs::read_to_string(log_dir(tmp.path()).join(&files[0])).unwrap();
    assert!(text.contains("hello log"));
}

/// debug 构建 debug 级,release 构建 info 级。
#[test]
fn the_level_follows_the_build_profile() {
    if cfg!(debug_assertions) {
        assert_eq!(level(), tracing::Level::DEBUG);
    } else {
        assert_eq!(level(), tracing::Level::INFO);
    }
}

/// 轮转保留 7 个:已经有 9 个旧文件,建写入器时多的被删掉,只剩 7 个(含今天的)。
#[test]
fn only_the_last_seven_files_are_kept() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(log_dir(tmp.path())).unwrap();
    for day in 1..=9 {
        std::fs::write(
            log_dir(tmp.path()).join(format!("xredis.2020-01-0{day}.log")),
            "old",
        )
        .unwrap();
    }
    let mut a = appender(tmp.path()).expect("an appender");
    writeln!(a, "today").unwrap();
    a.flush().unwrap();
    let files = log_files(tmp.path());
    assert_eq!(files.len(), KEEP_FILES, "{files:?}");
    assert!(
        !files.contains(&"xredis.2020-01-01.log".to_string()),
        "the oldest went first: {files:?}"
    );
    assert!(
        files.contains(&"xredis.2020-01-09.log".to_string()),
        "newer old ones stay: {files:?}"
    );
}

/// 目录建不出来(路径被一个文件占着)时返回 None,不崩溃。
#[test]
fn an_unusable_directory_gives_no_logging_but_no_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let blocked = tmp.path().join("occupied");
    std::fs::write(&blocked, "a file, not a folder").unwrap();
    assert!(appender(&blocked).is_none());
    assert!(init(&blocked).is_none());
}
