//! ZSet。分数是 f64,成员是字节。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;

use super::{already_exists, missing, scan_pairs, transaction};
use crate::error::{Entity, Error, Result};
use crate::keys::ScanPage;
use crate::value::*;

/// 扫一批(成员,分数)。
pub async fn scan(
    pool: &Pool,
    key: &[u8],
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<ScanPage<(Bytes, f64)>> {
    let page = scan_pairs(pool, "ZSCAN", key, pattern, count, cursor).await?;
    let items = page
        .items
        .into_iter()
        .map(|(member, score)| Ok((member, parse_score(&score)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(ScanPage {
        items,
        cursor: page.cursor,
    })
}

fn score_arg(score: f64) -> Result<Value> {
    if score.is_nan() {
        return Err(Error::ScoreNaN);
    }
    Ok(arg(score.to_string().as_bytes()))
}

pub async fn score(pool: &Pool, key: &[u8], member: &[u8]) -> Result<Option<f64>> {
    let v: Value = pool
        .custom(command("ZSCORE"), vec![arg(key), arg(member)])
        .await?;
    match value_to_opt_bytes(v) {
        Some(b) => Ok(Some(parse_score(&b)?)),
        None => Ok(None),
    }
}

/// 新增成员:已存在报错(`ZADD NX`)。
pub async fn add(pool: &Pool, key: &[u8], member: &[u8], score: f64) -> Result<()> {
    let added: i64 = pool
        .custom(
            command("ZADD"),
            vec![arg(key), arg_str("NX"), score_arg(score)?, arg(member)],
        )
        .await?;
    if added < 1 {
        return Err(already_exists(Entity::ZSetMember, member, Some(key)));
    }
    Ok(())
}

/// 改分数:成员不存在报错。
pub async fn set_score(pool: &Pool, key: &[u8], member: &[u8], score: f64) -> Result<()> {
    if self::score(pool, key, member).await?.is_none() {
        return Err(missing(Entity::ZSetMember, member, Some(key)));
    }
    let _: i64 = pool
        .custom(
            command("ZADD"),
            vec![arg(key), arg_str("XX"), score_arg(score)?, arg(member)],
        )
        .await?;
    Ok(())
}

/// 改成员名:先检查新名字不冲突、旧成员存在,保留分数,在事务里 ZREM 加 ZADD。
pub async fn rename_member(pool: &Pool, key: &[u8], old: &[u8], new: &[u8]) -> Result<()> {
    if old == new {
        return Ok(());
    }
    if self::score(pool, key, new).await?.is_some() {
        return Err(already_exists(Entity::ZSetMember, new, Some(key)));
    }
    let Some(old_score) = self::score(pool, key, old).await? else {
        return Err(missing(Entity::ZSetMember, old, Some(key)));
    };
    transaction(
        pool,
        vec![
            ("ZREM", vec![arg(key), arg(old)]),
            ("ZADD", vec![arg(key), score_arg(old_score)?, arg(new)]),
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
    let n: i64 = pool.custom(command("ZREM"), args).await?;
    Ok(n.max(0) as u64)
}
