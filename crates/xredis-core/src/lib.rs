//! xredis-core:领域层(配置、密钥、Redis 连接和命令)。不依赖任何 GUI。

pub mod config;
pub mod conn;
pub mod consts;
pub mod error;
pub mod keys;
pub mod keystore;
pub mod types;
pub mod v1_import;
mod value;

pub use error::{Error, Result};

pub fn crate_name() -> &'static str {
    "xredis-core"
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(super::crate_name(), "xredis-core");
    }
}
