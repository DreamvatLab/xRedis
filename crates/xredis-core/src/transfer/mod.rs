//! 复制粘贴:key 的 DUMP 和 RESTORE,以及和 v1 兼容的剪贴板编码。
//!
//! 剪贴板格式(和 v1 相同,v1 和 v2 之间可以互相复制粘贴):
//! `REDIS:` 加 base64(zlib(JSON `[{id, value: 字节的数字数组, ttl_ms}]`)),zlib 压缩级别 fast。
//! 写剪贴板和读剪贴板是 GUI 层的事,这里只产出和消费字符串。

use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::Value;
use serde::{Deserialize, Serialize};

use crate::consts::{CLIPBOARD_PREFIX, COPY_MAX_TOTAL_BYTES, PASTE_MAX_DECOMPRESSED_BYTES};
use crate::error::{ClipboardStage, Error, Result, SkipReason, Skipped};
use crate::keys::exists;
use crate::value::*;

/// 剪贴板里的一个 key。字段名和 v1 的 `IDValueDTO` 一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: String,
    /// DUMP 出来的字节。JSON 里是数字数组(v1 同)。
    pub value: Vec<u8>,
    /// 剩余存活时间(毫秒),0 表示不过期,和 RESTORE 的 ttl 参数语义一致。
    /// 缺失视为不过期(更早的版本没有这个字段)。
    #[serde(default)]
    pub ttl_ms: i64,
}

/// 编码成剪贴板文本。
pub fn encode(entries: &[ClipboardEntry]) -> Result<String> {
    let json = serde_json::to_vec(entries).map_err(|e| Error::ClipboardCorrupt {
        stage: ClipboardStage::Parse,
        detail: e.to_string(),
    })?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&json)
        .map_err(|e| Error::ClipboardCorrupt {
            stage: ClipboardStage::Decompress,
            detail: e.to_string(),
        })?;
    let compressed = encoder.finish().map_err(|e| Error::ClipboardCorrupt {
        stage: ClipboardStage::Decompress,
        detail: e.to_string(),
    })?;
    Ok(format!("{CLIPBOARD_PREFIX}{}", STANDARD.encode(compressed)))
}

/// 解码剪贴板文本,解压后超过 `PASTE_MAX_DECOMPRESSED_BYTES` 拒绝。
pub fn decode(text: &str) -> Result<Vec<ClipboardEntry>> {
    decode_with_limit(text, PASTE_MAX_DECOMPRESSED_BYTES)
}

/// 解压上限可调,测试用小值。剪贴板内容不可信:zlib 的压缩比可以非常高,几 KB 的数据能解出几 GB,
/// 所以用 `take(limit + 1)` 封顶,多读 1 字节是为了分清“恰好等于上限”和“被截断”(L-14)。
pub(crate) fn decode_with_limit(text: &str, limit: u64) -> Result<Vec<ClipboardEntry>> {
    let body = text
        .strip_prefix(CLIPBOARD_PREFIX)
        .ok_or(Error::ClipboardNotRedis)?;
    let compressed = STANDARD
        .decode(body.trim())
        .map_err(|e| Error::ClipboardCorrupt {
            stage: ClipboardStage::Base64,
            detail: e.to_string(),
        })?;
    let mut json = Vec::new();
    ZlibDecoder::new(&compressed[..])
        .take(limit + 1)
        .read_to_end(&mut json)
        .map_err(|e| Error::ClipboardCorrupt {
            stage: ClipboardStage::Decompress,
            detail: e.to_string(),
        })?;
    if json.len() as u64 > limit {
        return Err(Error::PasteTooLarge { limit });
    }
    serde_json::from_slice(&json).map_err(|e| Error::ClipboardCorrupt {
        stage: ClipboardStage::Parse,
        detail: e.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyOutcome {
    /// 要写进剪贴板的文本。
    pub text: String,
    pub copied: usize,
    /// 被跳过的 key 和原因,界面汇总提示。
    pub skipped: Vec<Skipped>,
}

/// 复制 key:逐个 DUMP 加 PTTL。已过期或不存在的、名字不是合法 UTF-8 的跳过并汇总,
/// **一个都没复制成才算失败**。DUMP 字节累计超过 64 MiB 立即中止(边走边判,不是等全部
/// DUMP 完再查 —— 那时内存已经吃下去了,后面 JSON、压缩、base64 还要翻几倍)。
pub async fn copy_keys(pool: &Pool, keys: &[Bytes]) -> Result<CopyOutcome> {
    copy_keys_with_limit(pool, keys, COPY_MAX_TOTAL_BYTES).await
}

pub(crate) async fn copy_keys_with_limit(
    pool: &Pool,
    keys: &[Bytes],
    limit: u64,
) -> Result<CopyOutcome> {
    let mut entries: Vec<ClipboardEntry> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut total: u64 = 0;
    for key in keys {
        let skip = |reason| Skipped {
            key: key.clone(),
            reason,
        };
        // v1 兼容格式里 key 名是字符串;二进制名字放不进去,跳过而不是悄悄变成乱码名字
        let Ok(id) = String::from_utf8(key.to_vec()) else {
            skipped.push(skip(SkipReason::BinaryName));
            continue;
        };
        let dump: std::result::Result<Value, _> =
            pool.custom(command("DUMP"), vec![arg(key)]).await;
        let data = match dump {
            Ok(Value::Null) => {
                skipped.push(skip(SkipReason::Missing));
                continue;
            }
            Ok(v) => value_to_bytes(v),
            Err(err) => {
                tracing::debug!("复制跳过 {id}:DUMP 失败: {err}");
                skipped.push(skip(SkipReason::Failed(err.details().to_string())));
                continue;
            }
        };
        total += data.len() as u64;
        if total > limit {
            return Err(Error::CopyTooLarge {
                total,
                limit,
                copied: entries.len(),
                key: key.clone(),
            });
        }
        // 一并取剩余存活时间,否则粘贴后过期时间会丢。PTTL:-1 无过期,-2 不存在(刚好过期),
        // 两者对 RESTORE 都是 0(不过期);-2 说明 key 在 DUMP 之后消失了,按不存在跳过
        let pttl: i64 = pool
            .custom(command("PTTL"), vec![arg(key)])
            .await
            .unwrap_or(-1);
        if pttl == -2 {
            total -= data.len() as u64;
            skipped.push(skip(SkipReason::Missing));
            continue;
        }
        entries.push(ClipboardEntry {
            id,
            value: data.to_vec(),
            ttl_ms: pttl.max(0),
        });
    }
    if entries.is_empty() {
        return Err(Error::NothingCopied { skipped });
    }
    Ok(CopyOutcome {
        text: encode(&entries)?,
        copied: entries.len(),
        skipped,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteOutcome {
    pub count: usize,
}

/// 粘贴前的预检结果:剪贴板里共几个 key,其中几个在当前库已存在(给确认框用)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastePreview {
    pub total: usize,
    pub existing: usize,
}

/// 解码剪贴板并对每个 key 执行 `EXISTS`。解码失败(不是 REDIS: 数据、损坏、超限)原样返回错误,
/// 这时不该弹确认框。只读,不写任何东西。
pub async fn preview(pool: &Pool, text: &str) -> Result<PastePreview> {
    let entries = decode(text)?;
    let mut existing = 0;
    for e in &entries {
        if exists(pool, e.id.as_bytes()).await? {
            existing += 1;
        }
    }
    Ok(PastePreview {
        total: entries.len(),
        existing,
    })
}

/// 粘贴:校验前缀,解压封顶 256 MiB;不覆盖模式下先检查**所有** key 是否存在,有冲突整体拒绝
/// 并列出,一个都不写(避免写到一半留下半成品);然后逐个 `RESTORE key ttl_ms value [REPLACE]`。
pub async fn paste(pool: &Pool, text: &str, overwrite: bool) -> Result<PasteOutcome> {
    let entries = decode(text)?;
    paste_entries(pool, entries, overwrite).await
}

pub(crate) async fn paste_entries(
    pool: &Pool,
    entries: Vec<ClipboardEntry>,
    overwrite: bool,
) -> Result<PasteOutcome> {
    let total = entries.len();
    if !overwrite {
        let mut conflicts = Vec::new();
        for e in &entries {
            if exists(pool, e.id.as_bytes()).await? {
                conflicts.push(Bytes::copy_from_slice(e.id.as_bytes()));
            }
        }
        if !conflicts.is_empty() {
            return Err(Error::PasteConflicts { conflicts, total });
        }
    }
    for e in entries {
        let mut args = vec![
            arg(e.id.as_bytes()),
            arg_int(e.ttl_ms.max(0)),
            arg(&e.value),
        ];
        if overwrite {
            args.push(arg_str("REPLACE"));
        }
        let restored: std::result::Result<Value, _> = pool.custom(command("RESTORE"), args).await;
        if let Err(err) = restored {
            return Err(Error::RestoreFailed {
                key: Bytes::copy_from_slice(e.id.as_bytes()),
                detail: err.details().to_string(),
            });
        }
    }
    Ok(PasteOutcome { count: total })
}

#[cfg(test)]
mod tests;
