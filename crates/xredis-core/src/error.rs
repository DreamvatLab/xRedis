//! 统一的错误类型。

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 配置读写失败(目录、文件、序列化)或配置内容不合法(TLS 证书等)。
    #[error("Config error: {0}")]
    Config(String),
    /// 配置目录或文件的 IO 错误,带路径。
    #[error("Failed to {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 密钥存储(系统钥匙串)失败。
    #[error("Key store error: {0}")]
    KeyStore(String),
    /// 业务规则不允许(key 已存在、EXPIRE 0、输入越界等)。
    #[error("{0}")]
    Rule(String),
    /// 按 id 找不到。
    #[error("Not found: {0}")]
    NotFound(String),
    /// v1 导入失败。
    #[error("v1 import failed: {0}")]
    Import(String),
    /// 连不上 Redis(网络、TLS 握手)。
    #[error("Connection failed: {0}")]
    Connect(String),
    /// 认证失败(密码或 ACL 用户不对)。
    #[error("Authentication failed: {0}")]
    Auth(String),
    /// 命令或建连超时。
    #[error("Timed out: {0}")]
    Timeout(String),
    /// 连接被关闭(用户关了 Tab,正在建的连接被叫停)。
    #[error("Connection closed")]
    Closed,
    /// Redis 返回的错误或其他协议层错误。
    #[error("Redis error: {0}")]
    Redis(String),
}

impl From<fred::error::Error> for Error {
    fn from(err: fred::error::Error) -> Self {
        use fred::error::ErrorKind;
        let msg = err.details().to_string();
        match err.kind() {
            ErrorKind::Auth => Error::Auth(msg),
            ErrorKind::Timeout => Error::Timeout(msg),
            ErrorKind::IO | ErrorKind::Tls => Error::Connect(msg),
            ErrorKind::Canceled => Error::Closed,
            ErrorKind::Config | ErrorKind::Url => Error::Config(msg),
            _ => Error::Redis(msg),
        }
    }
}
