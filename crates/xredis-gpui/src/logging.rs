//! 文件日志(P-65):配置目录下的 `logs/`,按天轮转保留 7 个;debug 构建 debug 级,release 构建 info 级。

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// 保留的日志文件个数(含今天的)。
pub const KEEP_FILES: usize = 7;

/// 当前构建用的日志级别。
pub fn level() -> tracing::Level {
    if cfg!(debug_assertions) {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    }
}

pub fn log_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("logs")
}

/// 建按天轮转的文件写入器。目录建不出来或没权限返回 `None`(日志是附带的,不能因为它起不来而崩溃)。
pub fn appender(config_dir: &Path) -> Option<RollingFileAppender> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("xredis")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(log_dir(config_dir))
        .map_err(|e| eprintln!("cannot start the log file: {e}"))
        .ok()
}

/// 装上全局日志。返回的 guard 要一直拿着(丢掉它,缓冲里没写完的日志就丢了)。
pub fn init(config_dir: &Path) -> Option<WorkerGuard> {
    let appender = appender(config_dir)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_max_level(level())
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}

#[cfg(test)]
mod tests;
