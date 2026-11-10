//! List。下标全程是 i64,元素是字节,二进制元素不会丢,下标不会错位(L-15)。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;
use ulid::Ulid;

use super::transaction;
use crate::consts::TTL_MAX;
use crate::error::{Error, Result};
use crate::value::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// LPUSH。
    Head,
    /// RPUSH。
    Tail,
}

pub async fn len(pool: &Pool, key: &[u8]) -> Result<u64> {
    let n: i64 = pool.custom(command("LLEN"), vec![arg(key)]).await?;
    Ok(n.max(0) as u64)
}

/// LRANGE,闭区间 `[start, stop]`。每个元素都保留,不丢,下标不错位。
pub async fn range(pool: &Pool, key: &[u8], start: i64, stop: i64) -> Result<Vec<Bytes>> {
    let items: Vec<Value> = pool
        .custom(
            command("LRANGE"),
            vec![arg(key), arg_int(start), arg_int(stop)],
        )
        .await?;
    Ok(items.into_iter().map(value_to_bytes).collect())
}

pub async fn get(pool: &Pool, key: &[u8], index: i64) -> Result<Option<Bytes>> {
    let v: Value = pool
        .custom(command("LINDEX"), vec![arg(key), arg_int(index)])
        .await?;
    Ok(value_to_opt_bytes(v))
}

pub async fn set(pool: &Pool, key: &[u8], index: i64, value: &[u8]) -> Result<()> {
    check_index(index)?;
    let _: Value = pool
        .custom(command("LSET"), vec![arg(key), arg_int(index), arg(value)])
        .await?;
    Ok(())
}

/// 头插或尾插,返回新长度。
pub async fn push(pool: &Pool, key: &[u8], value: &[u8], side: Side) -> Result<u64> {
    let name = match side {
        Side::Head => "LPUSH",
        Side::Tail => "RPUSH",
    };
    let n: i64 = pool
        .custom(command(name), vec![arg(key), arg(value)])
        .await?;
    Ok(n.max(0) as u64)
}

fn check_index(index: i64) -> Result<()> {
    if !(0..=TTL_MAX).contains(&index) {
        return Err(Error::ListIndexInvalid {
            index,
            max: TTL_MAX,
        });
    }
    Ok(())
}

/// 按下标删除:在事务里把目标下标 LSET 成哨兵值 `TO_BE_DELETED-<ULID>`,再 `LREM key 0 哨兵`。
/// 先查长度,有越界的下标整体拒绝,不会删一半。
pub async fn remove_at(pool: &Pool, key: &[u8], indexes: &[i64]) -> Result<()> {
    let mut indexes: Vec<i64> = indexes.to_vec();
    indexes.sort_unstable();
    indexes.dedup();
    if indexes.is_empty() {
        return Ok(());
    }
    for &i in &indexes {
        check_index(i)?;
    }
    let length = len(pool, key).await?;
    if let Some(&bad) = indexes.iter().find(|&&i| i as u64 >= length) {
        return Err(Error::ListIndexOutOfRange {
            index: bad,
            len: length,
        });
    }
    let sentinel = format!("TO_BE_DELETED-{}", Ulid::generate());
    let mut commands: Vec<(&'static str, Vec<Value>)> = indexes
        .iter()
        .map(|&i| ("LSET", vec![arg(key), arg_int(i), arg(sentinel.as_bytes())]))
        .collect();
    commands.push(("LREM", vec![arg(key), arg_int(0), arg(sentinel.as_bytes())]));
    transaction(pool, commands).await
}
