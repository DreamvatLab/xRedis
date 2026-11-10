//! 所有硬编码的限制和默认值(见 docs/02-v1-parity.md 第 8 节)。

/// 连接池大小,也是 key 详情的并发上限。
pub const POOL_SIZE: usize = 12;
/// `CONFIG GET databases` 失败时回落的库数量。
pub const DEFAULT_DB_COUNT: u32 = 16;

/// 单批扫描数量:范围、默认值。
pub const SCAN_BATCH_MIN: u32 = 100;
pub const SCAN_BATCH_MAX: u32 = 2000;
pub const SCAN_BATCH_DEFAULT: u32 = 500;
/// 自动续扫上限:范围、默认值(0 表示关闭自动续扫)。
pub const AUTO_SCAN_LIMIT_MAX: u32 = 100_000;
pub const AUTO_SCAN_LIMIT_DEFAULT: u32 = 100_000;

/// 窗口尺寸。
pub const WINDOW_MIN_WIDTH: u32 = 1024;
pub const WINDOW_MIN_HEIGHT: u32 = 840;
pub const WINDOW_DEFAULT_WIDTH: u32 = 1600;
pub const WINDOW_DEFAULT_HEIGHT: u32 = 903;

/// 端口、序号的合法范围。
pub const ORDINAL_MIN: i32 = -65535;
pub const ORDINAL_MAX: i32 = 65535;

/// 系统钥匙串里的 service 名。
pub const KEYRING_SERVICE: &str = "xRedis";

/// 重连退避:100ms 起,倍数 2,上限 10s,不限次数。
pub const RECONNECT_MIN_DELAY_MS: u32 = 100;
pub const RECONNECT_MAX_DELAY_MS: u32 = 10_000;
pub const RECONNECT_BACKOFF_BASE: u32 = 2;
/// 命令超时、建连超时。
pub const COMMAND_TIMEOUT_MS: u64 = 15_000;
pub const CONNECT_TIMEOUT_MS: u64 = 10_000;
/// 半开连接探测:间隔、超时。
pub const UNRESPONSIVE_INTERVAL_MS: u64 = 2_000;
pub const UNRESPONSIVE_TIMEOUT_MS: u64 = 8_000;
