//! 控制台(Console)的后端:分词、输出格式、命令执行。

mod exec;
pub mod format;
pub mod split;

#[cfg(test)]
mod tests;

pub use exec::{BLOCKING_COMMANDS, CONNECTION_MODE_COMMANDS, run_command};
pub use format::format_output;
pub use split::{SplitError, split};
