//! 统一的错误类型。

use std::fmt;
use std::path::PathBuf;

use bytes::Bytes;

use crate::config::ConnId;

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

/// 配置和证书文件的 IO 操作,界面按它翻译(路径和系统报错原样带上)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoAction {
    ReadV1Config,
    ReadCertFile,
    CreateConfigDir,
    ReadConfigFile,
    WriteTempFile,
    ReplaceConfigFile,
}

impl fmt::Display for IoAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IoAction::ReadV1Config => "read v1 config",
            IoAction::ReadCertFile => "read certificate file",
            IoAction::CreateConfigDir => "create config directory",
            IoAction::ReadConfigFile => "read config file",
            IoAction::WriteTempFile => "write temporary file",
            IoAction::ReplaceConfigFile => "replace config file",
        })
    }
}

/// 出问题的表单字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    ConnectionId,
    Host,
    Port,
    Ordinal,
    ScanBatch,
    AutoScanLimit,
}

impl fmt::Display for FormField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FormField::ConnectionId => "Connection id",
            FormField::Host => "Host",
            FormField::Port => "Port",
            FormField::Ordinal => "Ordinal",
            FormField::ScanBatch => "Batch size",
            FormField::AutoScanLimit => "Auto-scan limit",
        })
    }
}

/// 字段违反的规则,带范围数值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldRule {
    Required,
    Range {
        min: i64,
        max: i64,
    },
    AtMost {
        max: i64,
    },
    /// 不是合法的值;`value` 是用户给的,`detail` 是解析库的原始报错。
    Invalid {
        value: String,
        detail: String,
    },
}

impl fmt::Display for FieldRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldRule::Required => f.write_str("is required"),
            FieldRule::Range { min, max } => write!(f, "must be between {min} and {max}"),
            FieldRule::AtMost { max } => write!(f, "cannot exceed {max}"),
            FieldRule::Invalid { value, detail } => write!(f, "'{value}' is not valid: {detail}"),
        }
    }
}

/// TLS 证书和私钥的问题。路径和库的原始报错原样带上,界面按种类翻译。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsProblem {
    /// 文件不是合法的 PEM。
    NotPem { path: PathBuf, detail: String },
    /// 私钥是加密的(要先解密)。
    KeyEncrypted,
    /// 私钥的 PEM 标签不认识。
    KeyFormatUnsupported { tag: String },
    /// CA 证书无效。
    CaInvalid { path: PathBuf, detail: String },
    /// 客户端证书和私钥对不上或无效。
    CertKeyMismatch { detail: String },
    /// 客户端证书和私钥只给了一个。
    CertKeyIncomplete,
}

impl fmt::Display for TlsProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsProblem::NotPem { path, detail } => {
                write!(f, "{} is not a valid PEM file: {detail}", path.display())
            }
            TlsProblem::KeyEncrypted => f.write_str(
                "The client private key is encrypted. Decrypt it first: openssl pkcs8 -topk8 -nocrypt -in client.key -out decrypted.key",
            ),
            TlsProblem::KeyFormatUnsupported { tag } => write!(
                f,
                "Unsupported private key format '{tag}'. Supported: PRIVATE KEY (PKCS#8), RSA PRIVATE KEY (PKCS#1), EC PRIVATE KEY (SEC1)"
            ),
            TlsProblem::CaInvalid { path, detail } => {
                write!(f, "Invalid CA certificate {}: {detail}", path.display())
            }
            TlsProblem::CertKeyMismatch { detail } => write!(
                f,
                "Client certificate and key do not match or are invalid: {detail}"
            ),
            TlsProblem::CertKeyIncomplete => {
                f.write_str("Client certificate and client key must be provided together")
            }
        }
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
    /// 配置序列化失败(`detail` 是序列化库的原始报错)。
    #[error("Failed to serialize config: {detail}")]
    Serialize { detail: String },
    /// Redis 客户端库认为连接配置不对(地址、选项),`detail` 是库的原始报错。
    #[error("Client configuration error: {detail}")]
    ClientConfig { detail: String },
    /// TLS 证书和私钥的问题。
    #[error("{0}")]
    Tls(TlsProblem),
    /// 配置目录或文件的 IO 错误,带路径和系统报错。
    #[error("Failed to {action} {path}: {source}")]
    Io {
        action: IoAction,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 密钥存储(系统钥匙串)失败。
    #[error("Key store error: {0}")]
    KeyStore(String),
    /// 表单字段不合规矩(连接表单、设置面板)。
    #[error("{field} {rule}")]
    InvalidField { field: FormField, rule: FieldRule },
    /// 按 id 找不到连接。
    #[error("Connection {0} was not found")]
    ConnectionNotFound(ConnId),
    /// v1 的配置文件读到了但解析不了。
    #[error("v1 import failed: {} could not be parsed: {detail}", path.display())]
    ImportParse { path: PathBuf, detail: String },
    /// 服务器返回的分数不是数字。
    #[error("Invalid score returned by server: {}", lossy(.value))]
    InvalidScoreReply { value: Bytes },
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

    // ---- R-05:复制粘贴、控制台、Pub/Sub ----
    /// 复制时累计的 DUMP 字节超过上限,立即中止(L-14)。`key` 是越线的那个。
    #[error("Too much data to copy: reached {total} bytes at key '{}' after {copied} key(s), limit is {limit}", lossy(.key))]
    CopyTooLarge {
        total: u64,
        limit: u64,
        copied: usize,
        key: Bytes,
    },
    /// 一个 key 都没复制成(没选、或者全部被跳过)。
    #[error("Nothing was copied ({} key(s) skipped)", .skipped.len())]
    NothingCopied { skipped: Vec<Skipped> },
    /// 剪贴板里没有 `REDIS:` 开头的数据。
    #[error("No Redis data found in the clipboard")]
    ClipboardNotRedis,
    /// 剪贴板数据损坏:哪一步出的错和原始报错。
    #[error("Clipboard data is corrupt ({stage}): {detail}")]
    ClipboardCorrupt {
        stage: ClipboardStage,
        detail: String,
    },
    /// 解压后超过上限,防解压炸弹(L-14)。
    #[error("Clipboard data expands beyond the {limit} byte paste limit")]
    PasteTooLarge { limit: u64 },
    /// 不覆盖模式下有同名 key,整体拒绝,一个都没写。
    #[error("{} of {total} key(s) already exist and were not overwritten", .conflicts.len())]
    PasteConflicts { conflicts: Vec<Bytes>, total: usize },
    /// 逐个 RESTORE 时某个 key 失败(前面的已经写入)。
    #[error("Failed to restore key '{}': {detail}", lossy(.key))]
    RestoreFailed { key: Bytes, detail: String },
    /// 这种类型的 key 不能打开(Stream、模块类型,P-16)。
    #[error("A key of type '{kind}' cannot be opened")]
    NotOpenable { kind: String },
    /// 控制台命令分词失败,不发送。
    #[error("Invalid command syntax: {0}")]
    CommandSyntax(crate::console::SplitError),
    /// 会让连接进入特殊模式的命令,控制台拒绝。
    #[error(
        "'{command}' would put the connection into a special mode and is not allowed in the console"
    )]
    CommandRejected { command: String },
    /// 阻塞命令在控制台里等了太久,放弃。
    #[error("'{command}' timed out after {secs}s")]
    BlockingTimeout { command: String, secs: u64 },
}

/// 复制时被跳过的 key 和原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub key: Bytes,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// key 不存在(多半是刚好过期)。
    Missing,
    /// 名字不是合法 UTF-8:v1 兼容的剪贴板格式里 key 名是字符串,放不下。
    BinaryName,
    /// DUMP 失败,带 Redis 的报错。
    Failed(String),
}

/// 剪贴板数据解码到哪一步出错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardStage {
    Base64,
    Decompress,
    Parse,
}

impl fmt::Display for ClipboardStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClipboardStage::Base64 => "base64",
            ClipboardStage::Decompress => "decompress",
            ClipboardStage::Parse => "parse",
        })
    }
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
            ErrorKind::Config | ErrorKind::Url => Error::ClientConfig { detail: msg },
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
