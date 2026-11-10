//! key 级别的操作:游标扫描(四种共用一份)、key 详情、删除、改名、TTL。
//!
//! 全程用字节,二进制名字的 key 也能删除、改 TTL。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;

use crate::consts::*;
use crate::error::{Entity, Error, Result};
use crate::types::{already_exists, missing};
use crate::value::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPage<T> {
    pub items: Vec<T>,
    /// 下一个游标,0 表示扫完了。
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyType {
    String,
    Hash,
    List,
    Set,
    ZSet,
    Stream,
    /// key 不存在(`TYPE` 返回 none)。
    None,
    /// 模块类型(ReJSON 等),只显示不能打开。
    Other(String),
}

impl KeyType {
    pub fn from_redis(name: &str) -> Self {
        match name {
            "string" => Self::String,
            "hash" => Self::Hash,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::ZSet,
            "stream" => Self::Stream,
            "none" => Self::None,
            other => Self::Other(other.to_string()),
        }
    }

    /// 能不能打开编辑(P-16):Stream 和模块类型只显示。
    pub fn openable(&self) -> bool {
        matches!(
            self,
            Self::String | Self::Hash | Self::List | Self::Set | Self::ZSet
        )
    }
}

/// TTL 不用裸 i64。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ttl {
    /// -1:没有过期时间。
    Persistent,
    /// -2:key 不存在。
    Missing,
    Seconds(i64),
}

impl Ttl {
    pub fn from_redis(n: i64) -> Self {
        match n {
            -1 => Self::Persistent,
            n if n < 0 => Self::Missing,
            n => Self::Seconds(n),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDetails {
    pub kind: KeyType,
    /// 长度按类型:STRLEN、HLEN、LLEN、SCARD、ZCARD、XLEN;key 不存在或模块类型是 `None`。
    pub length: Option<u64>,
    pub ttl: Ttl,
}

/// 通用游标扫描:SCAN / HSCAN / SSCAN / ZSCAN 共用这一份。
///
/// `key` 为 `None` 是 SCAN。pattern 为空或 `*` 时不带 MATCH。返回下一个游标和原始元素
/// (HSCAN、ZSCAN 是交替的 field、值),由调用方按类型拆。
pub(crate) async fn scan_raw(
    pool: &Pool,
    command_name: &'static str,
    key: Option<&[u8]>,
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<(u64, Vec<Value>)> {
    let mut args = Vec::with_capacity(6);
    if let Some(key) = key {
        args.push(arg(key));
    }
    // 游标是无符号 64 位,按字符串传,不能转 i64
    args.push(arg(cursor.to_string().as_bytes()));
    if !pattern.is_empty() && pattern != b"*" {
        args.push(arg_str("MATCH"));
        args.push(arg(pattern));
    }
    args.push(arg_str("COUNT"));
    args.push(arg_int(i64::from(count)));
    let (next, batch): (u64, Vec<Value>) = pool.custom(command(command_name), args).await?;
    Ok((next, batch))
}

/// 扫一批 key。去重、三档逻辑和自动续扫由 app 层做。
pub async fn scan_keys(
    pool: &Pool,
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<ScanPage<Bytes>> {
    let (cursor, batch) = scan_raw(pool, "SCAN", None, pattern, count, cursor).await?;
    Ok(ScanPage {
        items: batch.into_iter().map(value_to_bytes).collect(),
        cursor,
    })
}

pub async fn key_type(pool: &Pool, key: &[u8]) -> Result<KeyType> {
    let name: String = pool.custom(command("TYPE"), vec![arg(key)]).await?;
    Ok(KeyType::from_redis(&name))
}

pub async fn key_ttl(pool: &Pool, key: &[u8]) -> Result<Ttl> {
    let ttl: i64 = pool.custom(command("TTL"), vec![arg(key)]).await?;
    Ok(Ttl::from_redis(ttl))
}

async fn key_length(pool: &Pool, kind: &KeyType, key: &[u8]) -> Result<Option<u64>> {
    let name = match kind {
        KeyType::String => "STRLEN",
        KeyType::Hash => "HLEN",
        KeyType::List => "LLEN",
        KeyType::Set => "SCARD",
        KeyType::ZSet => "ZCARD",
        KeyType::Stream => "XLEN",
        KeyType::None | KeyType::Other(_) => return Ok(None),
    };
    let n: i64 = pool.custom(command(name), vec![arg(key)]).await?;
    Ok(Some(n.max(0) as u64))
}

/// key 详情:类型、长度、TTL。key 不存在时类型是 `None`、TTL 是 `Missing`。
pub async fn key_details(pool: &Pool, key: &[u8]) -> Result<KeyDetails> {
    let kind = key_type(pool, key).await?;
    let (length, ttl) = tokio::join!(key_length(pool, &kind, key), key_ttl(pool, key));
    Ok(KeyDetails {
        kind,
        length: length?,
        ttl: ttl?,
    })
}

pub async fn exists(pool: &Pool, key: &[u8]) -> Result<bool> {
    let n: i64 = pool.custom(command("EXISTS"), vec![arg(key)]).await?;
    Ok(n > 0)
}

pub async fn db_size(pool: &Pool) -> Result<u64> {
    let n: i64 = pool.custom(command("DBSIZE"), Vec::<Value>::new()).await?;
    Ok(n.max(0) as u64)
}

/// 批量删除,每批 `DELETE_BATCH` 个。返回实际删掉的数量。
pub async fn delete_keys(pool: &Pool, keys: &[Bytes]) -> Result<u64> {
    let mut deleted = 0u64;
    for chunk in keys.chunks(DELETE_BATCH) {
        let args: Vec<Value> = chunk.iter().map(|k| arg(k)).collect();
        let n: i64 = pool.custom(command("DEL"), args).await?;
        deleted += n.max(0) as u64;
    }
    Ok(deleted)
}

/// 改 key 名:RENAMENX(原子),目标已存在报错;源不存在报 `NotFound`。
pub async fn rename_key(pool: &Pool, old: &[u8], new: &[u8]) -> Result<()> {
    let renamed: std::result::Result<i64, _> = pool
        .custom(command("RENAMENX"), vec![arg(old), arg(new)])
        .await;
    match renamed {
        Ok(1) => Ok(()),
        Ok(_) => Err(already_exists(Entity::Key, new, None)),
        Err(err) if err.details().to_lowercase().contains("no such key") => {
            Err(missing(Entity::Key, old, None))
        }
        Err(err) => Err(err.into()),
    }
}

/// TTL 变更,已经按规则校验过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlChange {
    Persist,
    Expire(i64),
}

/// 校验用户输入的 TTL(秒):-1 是 PERSIST,0 拒绝(EXPIRE 0 会立刻删除 key,L-08),
/// 正数是 EXPIRE,上限 2147483647。
pub fn validate_ttl(seconds: i64) -> Result<TtlChange> {
    match seconds {
        -1 => Ok(TtlChange::Persist),
        0 => Err(Error::TtlZero),
        n if n < -1 => Err(Error::TtlOutOfRange {
            value: n,
            max: TTL_MAX,
        }),
        n if n > TTL_MAX => Err(Error::TtlOutOfRange {
            value: n,
            max: TTL_MAX,
        }),
        n => Ok(TtlChange::Expire(n)),
    }
}

/// 改 TTL。key 不存在报 `NotFound`;对没有 TTL 的 key 执行 PERSIST 不算错
/// (PERSIST 返回 0 既可能是 key 不存在,也可能是 key 本来就没有 TTL,要再查一次)。
pub async fn set_ttl(pool: &Pool, key: &[u8], seconds: i64) -> Result<()> {
    let change = validate_ttl(seconds)?;
    let not_found = || missing(Entity::Key, key, None);
    match change {
        TtlChange::Expire(n) => {
            let ok: i64 = pool
                .custom(command("EXPIRE"), vec![arg(key), arg_int(n)])
                .await?;
            if ok == 1 { Ok(()) } else { Err(not_found()) }
        }
        TtlChange::Persist => {
            let ok: i64 = pool.custom(command("PERSIST"), vec![arg(key)]).await?;
            if ok == 1 || exists(pool, key).await? {
                Ok(())
            } else {
                Err(not_found())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_type_names() {
        assert_eq!(KeyType::from_redis("string"), KeyType::String);
        assert_eq!(KeyType::from_redis("zset"), KeyType::ZSet);
        assert_eq!(KeyType::from_redis("none"), KeyType::None);
        assert_eq!(
            KeyType::from_redis("ReJSON-RL"),
            KeyType::Other("ReJSON-RL".into())
        );
    }

    /// P-16:只有五种基本类型能打开。
    #[test]
    fn only_basic_types_are_openable() {
        for k in [
            KeyType::String,
            KeyType::Hash,
            KeyType::List,
            KeyType::Set,
            KeyType::ZSet,
        ] {
            assert!(k.openable(), "{k:?}");
        }
        for k in [KeyType::Stream, KeyType::None, KeyType::Other("x".into())] {
            assert!(!k.openable(), "{k:?}");
        }
    }

    #[test]
    fn ttl_is_not_a_bare_integer() {
        assert_eq!(Ttl::from_redis(-1), Ttl::Persistent);
        assert_eq!(Ttl::from_redis(-2), Ttl::Missing);
        assert_eq!(Ttl::from_redis(0), Ttl::Seconds(0));
        assert_eq!(Ttl::from_redis(90), Ttl::Seconds(90));
    }

    /// L-08(P-28):EXPIRE 0 会删除 key,必须拒绝。
    #[test]
    fn l08_ttl_zero_is_rejected() {
        assert!(matches!(validate_ttl(0), Err(Error::TtlZero)));
    }

    #[test]
    fn ttl_range_rules() {
        assert_eq!(validate_ttl(-1).unwrap(), TtlChange::Persist);
        assert_eq!(validate_ttl(1).unwrap(), TtlChange::Expire(1));
        assert_eq!(
            validate_ttl(TTL_MAX).unwrap(),
            TtlChange::Expire(2_147_483_647)
        );
        for bad in [TTL_MAX + 1, -2, i64::MIN] {
            assert!(
                matches!(validate_ttl(bad), Err(Error::TtlOutOfRange { value, .. }) if value == bad),
                "{bad}"
            );
        }
    }
}
