//! Set。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;

use super::{already_exists, missing, transaction};
use crate::error::{Entity, Result};
use crate::keys::{ScanPage, scan_raw};
use crate::value::*;

pub async fn scan(
    pool: &Pool,
    key: &[u8],
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<ScanPage<Bytes>> {
    let (cursor, batch) = scan_raw(pool, "SSCAN", Some(key), pattern, count, cursor).await?;
    Ok(ScanPage {
        items: batch.into_iter().map(value_to_bytes).collect(),
        cursor,
    })
}

pub async fn is_member(pool: &Pool, key: &[u8], member: &[u8]) -> Result<bool> {
    let n: i64 = pool
        .custom(command("SISMEMBER"), vec![arg(key), arg(member)])
        .await?;
    Ok(n > 0)
}

/// 新增成员:已存在报错。
pub async fn add(pool: &Pool, key: &[u8], member: &[u8]) -> Result<()> {
    let added: i64 = pool
        .custom(command("SADD"), vec![arg(key), arg(member)])
        .await?;
    if added < 1 {
        return Err(already_exists(Entity::SetMember, member, Some(key)));
    }
    Ok(())
}

/// 改成员名:先检查旧成员存在、新名字不冲突,再在事务里 SREM 加 SADD。
/// (v1 不检查旧成员是否存在,会凭空多出一个新成员。)
pub async fn rename_member(pool: &Pool, key: &[u8], old: &[u8], new: &[u8]) -> Result<()> {
    if old == new {
        return Ok(());
    }
    if is_member(pool, key, new).await? {
        return Err(already_exists(Entity::SetMember, new, Some(key)));
    }
    if !is_member(pool, key, old).await? {
        return Err(missing(Entity::SetMember, old, Some(key)));
    }
    transaction(
        pool,
        vec![
            ("SREM", vec![arg(key), arg(old)]),
            ("SADD", vec![arg(key), arg(new)]),
        ],
    )
    .await
}

/// 删成员,返回实际删掉的数量。
pub async fn remove(pool: &Pool, key: &[u8], members: &[Bytes]) -> Result<u64> {
    if members.is_empty() {
        return Ok(0);
    }
    let mut args = vec![arg(key)];
    args.extend(members.iter().map(|m| arg(m)));
    let n: i64 = pool.custom(command("SREM"), args).await?;
    Ok(n.max(0) as u64)
}
