//! 五种类型的读写。全程用字节(`Bytes`),二进制的 key、field、member、value 都能处理。
//!
//! 改名类操作(Hash field、Set member、ZSet member)先检查冲突,再在 MULTI/EXEC 事务里
//! 删旧加新。检查和事务之间不是原子的(和 v1 一样),并发改同一个 key 时可能出现竞态。

pub mod hash;
pub mod list;
pub mod set;
pub mod string;
pub mod zset;

#[cfg(test)]
mod tests;

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::{ClientLike, TransactionInterface};
use fred::types::Value;

use crate::error::{Entity, Error, Result};
use crate::keys::{ScanPage, scan_raw};
use crate::value::*;

/// HSCAN / ZSCAN 返回的是交替的(名字,值),拆成对。
pub(crate) async fn scan_pairs(
    pool: &Pool,
    command_name: &'static str,
    key: &[u8],
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<ScanPage<(Bytes, Bytes)>> {
    let (cursor, batch) = scan_raw(pool, command_name, Some(key), pattern, count, cursor).await?;
    let mut items = Vec::with_capacity(batch.len() / 2);
    let mut it = batch.into_iter();
    while let (Some(name), Some(value)) = (it.next(), it.next()) {
        items.push((value_to_bytes(name), value_to_bytes(value)));
    }
    Ok(ScanPage { items, cursor })
}

/// 在 MULTI/EXEC 里依次执行命令;任何一条失败整体报错。
pub(crate) async fn transaction(
    pool: &Pool,
    commands: Vec<(&'static str, Vec<Value>)>,
) -> Result<()> {
    let tx = pool.multi();
    for (name, args) in commands {
        tx.custom::<Value, _>(command(name), args).await?;
    }
    tx.exec::<Value>(true).await?;
    Ok(())
}

pub(crate) fn already_exists(entity: Entity, name: &[u8], container: Option<&[u8]>) -> Error {
    Error::AlreadyExists {
        entity,
        name: Bytes::copy_from_slice(name),
        container: container.map(Bytes::copy_from_slice),
    }
}

pub(crate) fn missing(entity: Entity, name: &[u8], container: Option<&[u8]>) -> Error {
    Error::Missing {
        entity,
        name: Bytes::copy_from_slice(name),
        container: container.map(Bytes::copy_from_slice),
    }
}
