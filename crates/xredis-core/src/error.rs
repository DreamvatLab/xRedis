//! 统一的错误类型。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 配置读写失败(目录、文件、序列化)。
    #[error("配置错误: {0}")]
    Config(String),
    /// 配置目录或文件的 IO 错误,带路径。
    #[error("{action} {path} 失败: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 密钥存储(系统钥匙串)失败。
    #[error("密钥存储错误: {0}")]
    KeyStore(String),
    /// 业务规则不允许(key 已存在、EXPIRE 0、输入越界等)。
    #[error("{0}")]
    Rule(String),
    /// 按 id 找不到。
    #[error("找不到: {0}")]
    NotFound(String),
    /// v1 导入失败。
    #[error("v1 导入失败: {0}")]
    Import(String),
}
