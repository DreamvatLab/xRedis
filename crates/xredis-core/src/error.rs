//! 统一的错误类型。

use std::fmt;
use std::path::PathBuf;

use bytes::Bytes;

pub type Result<T> = std::result::Result<T, Error>;

/// 出错的是哪一种东西。界面按它翻译,`Display` 的英文只用于日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Key,
    HashField,
    SetMember,
    ZSetMember,
}

impl fmt::Display for Entity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Entity::Key => "key",
            Entity::HashField => "hash field",
            Entity::SetMember => "set member",
            Entity::ZSetMember => "zset member",
        })
    }
}

/// 名字是字节,日志里有损显示。
fn lossy(bytes: &Bytes) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

fn within(container: &Option<Bytes>) -> String {
    match container {
        Some(c) => format!(" in '{}'", String::from_utf8_lossy(c)),
        None => String::new(),
    }
}

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

    // ---- 以下带结构化数据,界面按种类翻译,不要在这里拼给用户看的句子 ----
    /// 新建或改名时名字已经存在(key、Hash field、Set 或 ZSet 成员)。
    #[error("{entity} '{}' already exists{}", lossy(.name), within(.container))]
    AlreadyExists {
        entity: Entity,
        name: Bytes,
        /// 所在的 key(field 和成员有,key 本身没有)。
        container: Option<Bytes>,
    },
    /// 要操作的东西不存在。
    #[error("{entity} '{}' does not exist{}", lossy(.name), within(.container))]
    Missing {
        entity: Entity,
        name: Bytes,
        container: Option<Bytes>,
    },
    /// TTL 0 会立刻删除 key(L-08),拒绝。
    #[error("TTL 0 would delete the key immediately; use a positive value, or -1 to persist it")]
    TtlZero,
    /// TTL 不是 -1,也不在 1 到 `max` 之间。
    #[error("TTL {value} is out of range (-1, or 1 to {max})")]
    TtlOutOfRange { value: i64, max: i64 },
    /// List 下标不在 0 到 `max` 之间。
    #[error("List index {index} must be between 0 and {max}")]
    ListIndexInvalid { index: i64, max: i64 },
    /// List 下标超出当前长度。
    #[error("List index {index} is out of range (the list has {len} elements)")]
    ListIndexOutOfRange { index: i64, len: u64 },
    /// ZSet 分数不是数字。
    #[error("Score must be a number")]
    ScoreNaN,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 结构化错误把名字原样(字节)带出去,界面按种类翻译;Display 的英文只用于日志。
    #[test]
    fn structured_errors_carry_raw_bytes_and_log_friendly_display() {
        let e = Error::AlreadyExists {
            entity: Entity::HashField,
            name: Bytes::from_static(&[0xff, 0xfe, b'x']),
            container: Some(Bytes::from_static(b"h")),
        };
        match &e {
            Error::AlreadyExists {
                entity,
                name,
                container,
            } => {
                assert_eq!(*entity, Entity::HashField);
                assert_eq!(&name[..], &[0xff, 0xfe, b'x']);
                assert_eq!(container.as_deref(), Some(&b"h"[..]));
            }
            other => panic!("{other:?}"),
        }
        let text = e.to_string();
        assert!(
            text.contains("hash field") && text.contains("already exists"),
            "{text}"
        );
        assert!(text.contains("in 'h'"), "{text}");

        let missing = Error::Missing {
            entity: Entity::Key,
            name: Bytes::from_static(b"k"),
            container: None,
        };
        assert_eq!(missing.to_string(), "key 'k' does not exist");
        assert_eq!(
            Error::TtlOutOfRange { value: 5, max: 3 }.to_string(),
            "TTL 5 is out of range (-1, or 1 to 3)"
        );
        assert!(Error::TtlZero.to_string().contains("0"));
    }
}
