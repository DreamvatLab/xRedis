//! Hash。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;

use super::{already_exists, missing, scan_pairs, transaction};
use crate::error::{Entity, Result};
use crate::keys::ScanPage;
use crate::value::*;

/// 扫一批(field,value)。
pub async fn scan(
    pool: &Pool,
    key: &[u8],
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<ScanPage<(Bytes, Bytes)>> {
    scan_pairs(pool, "HSCAN", key, pattern, count, cursor).await
}

pub async fn get(pool: &Pool, key: &[u8], field: &[u8]) -> Result<Option<Bytes>> {
    let v: Value = pool
        .custom(command("HGET"), vec![arg(key), arg(field)])
        .await?;
    Ok(value_to_opt_bytes(v))
}

/// 改值(新增或覆盖)。
pub async fn set(pool: &Pool, key: &[u8], field: &[u8], value: &[u8]) -> Result<()> {
    let _: i64 = pool
        .custom(command("HSET"), vec![arg(key), arg(field), arg(value)])
        .await?;
    Ok(())
}

/// 新增 field:已存在报错,不覆盖。
pub async fn add(pool: &Pool, key: &[u8], field: &[u8], value: &[u8]) -> Result<()> {
    let added: i64 = pool
        .custom(command("HSETNX"), vec![arg(key), arg(field), arg(value)])
        .await?;
    if added == 0 {
        return Err(already_exists(Entity::HashField, field, Some(key)));
    }
    Ok(())
}

/// 改 field 名:先检查旧的存在、新的不冲突,再在事务里 HDEL 加 HSET,值不变。
pub async fn rename_field(pool: &Pool, key: &[u8], old: &[u8], new: &[u8]) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let new_exists: i64 = pool
        .custom(command("HEXISTS"), vec![arg(key), arg(new)])
        .await?;
    if new_exists > 0 {
        return Err(already_exists(Entity::HashField, new, Some(key)));
    }
    let Some(value) = get(pool, key, old).await? else {
        return Err(missing(Entity::HashField, old, Some(key)));
    };
    transaction(
        pool,
        vec![
            ("HDEL", vec![arg(key), arg(old)]),
            ("HSET", vec![arg(key), arg(new), arg(&value)]),
        ],
    )
    .await
}

/// 删 field,返回实际删掉的数量。
pub async fn delete(pool: &Pool, key: &[u8], fields: &[Bytes]) -> Result<u64> {
    if fields.is_empty() {
        return Ok(0);
    }
    let mut args = vec![arg(key)];
    args.extend(fields.iter().map(|f| arg(f)));
    let n: i64 = pool.custom(command("HDEL"), args).await?;
    Ok(n.max(0) as u64)
}
