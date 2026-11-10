//! String:读写,大值只读前 1 MiB。

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;

use super::already_exists;
use crate::consts::{LARGE_VALUE_PREVIEW, LARGE_VALUE_THRESHOLD};
use crate::error::{Entity, Result};
use crate::value::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringValue {
    pub data: Bytes,
    /// 服务端的真实长度(STRLEN)。
    pub total_len: u64,
    /// 超过 8 MiB 时只读了前 1 MiB:界面要只读显示并提示。
    pub truncated: bool,
}

/// 写入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMode {
    /// 新建:key 已存在就报错,不覆盖(`SET NX`,原子)。
    Create,
    /// 覆盖,TTL 会被清掉(Redis 的 `SET` 默认行为,v1 同)。
    Overwrite,
    /// 覆盖但保留 TTL(`KEEPTTL`,需要 Redis 6.0 以上)。
    KeepTtl,
}

/// 读值。key 不存在返回 `None`。长度超过 `LARGE_VALUE_THRESHOLD` 只用 GETRANGE 读前
/// `LARGE_VALUE_PREVIEW` 字节。
pub async fn get(pool: &Pool, key: &[u8]) -> Result<Option<StringValue>> {
    let len: i64 = pool.custom(command("STRLEN"), vec![arg(key)]).await?;
    let total_len = len.max(0) as u64;
    if total_len > LARGE_VALUE_THRESHOLD {
        let end = LARGE_VALUE_PREVIEW as i64 - 1;
        let part: Value = pool
            .custom(
                command("GETRANGE"),
                vec![arg(key), arg_int(0), arg_int(end)],
            )
            .await?;
        return Ok(value_to_opt_bytes(part).map(|data| StringValue {
            data,
            total_len,
            truncated: true,
        }));
    }
    let v: Value = pool.custom(command("GET"), vec![arg(key)]).await?;
    Ok(value_to_opt_bytes(v).map(|data| StringValue {
        total_len: data.len() as u64,
        data,
        truncated: false,
    }))
}

pub async fn set(pool: &Pool, key: &[u8], value: &[u8], mode: SetMode) -> Result<()> {
    let mut args = vec![arg(key), arg(value)];
    match mode {
        SetMode::Create => args.push(arg_str("NX")),
        SetMode::Overwrite => {}
        SetMode::KeepTtl => args.push(arg_str("KEEPTTL")),
    }
    let reply: Value = pool.custom(command("SET"), args).await?;
    if mode == SetMode::Create && reply.is_null() {
        return Err(already_exists(Entity::Key, key, None));
    }
    Ok(())
}
