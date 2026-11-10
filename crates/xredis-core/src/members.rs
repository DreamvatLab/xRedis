//! 成员表和值编辑器用的读写入口(R-07b):按类型扫成员,把一次保存拆成有序的写操作逐个执行。
//!
//! 一次保存可能是几步(先改 key 名再改值;先改成员名再改分数),所以 [`apply`] 收一串 [`WriteOp`],
//! 遇到第一个失败就停下,并告诉调用方前面做完了几步(`ApplyFailure::done`),界面据此刷新。

use bytes::Bytes;
use fred::clients::Pool;

use crate::error::{Entity, Error, Result};
use crate::keys::{self, KeyType, ScanPage};
use crate::types::list::Side;
use crate::types::string::SetMode;
use crate::types::{hash, list, set, string, zset};

/// 成员表的一行。
#[derive(Debug, Clone, PartialEq)]
pub enum Member {
    Hash {
        field: Bytes,
        value: Bytes,
    },
    /// List 的行用下标。
    List {
        index: u64,
        value: Bytes,
    },
    Set {
        member: Bytes,
    },
    ZSet {
        member: Bytes,
        score: f64,
    },
}

impl Member {
    /// 这一行的“名字”(Hash 的 field、Set 和 ZSet 的成员、List 的下标的十进制文本);
    /// 选中、去重、改名都用它。
    pub fn name(&self) -> Bytes {
        match self {
            Member::Hash { field, .. } => field.clone(),
            Member::List { index, .. } => Bytes::from(index.to_string()),
            Member::Set { member } | Member::ZSet { member, .. } => member.clone(),
        }
    }
}

/// 一批成员和下一个游标(0 表示扫完)。List 的游标是下一页的起始下标。
#[derive(Debug, Clone, PartialEq)]
pub struct MemberPage {
    pub items: Vec<Member>,
    pub cursor: u64,
}

/// 对一个已有 key 的成员操作。
#[derive(Debug, Clone, PartialEq)]
pub enum MemberOp {
    /// 改值(HSET),field 不存在就新增。
    HashSet {
        field: Bytes,
        value: Bytes,
    },
    /// 新增 field:已存在报错。
    HashAdd {
        field: Bytes,
        value: Bytes,
    },
    HashRename {
        old: Bytes,
        new: Bytes,
    },
    HashDelete(Vec<Bytes>),
    ListPush {
        value: Bytes,
        side: Side,
    },
    ListSet {
        index: i64,
        value: Bytes,
    },
    ListRemove(Vec<i64>),
    /// 新增成员:已存在报错。
    SetAdd(Bytes),
    SetRename {
        old: Bytes,
        new: Bytes,
    },
    SetRemove(Vec<Bytes>),
    /// 新增成员:已存在报错。
    ZAdd {
        member: Bytes,
        score: f64,
    },
    ZSetScore {
        member: Bytes,
        score: f64,
    },
    ZRename {
        old: Bytes,
        new: Bytes,
    },
    ZRemove(Vec<Bytes>),
}

/// 新建 key 时的第一个内容。
#[derive(Debug, Clone, PartialEq)]
pub enum NewContent {
    String(Bytes),
    Hash { field: Bytes, value: Bytes },
    List { value: Bytes, side: Side },
    Set(Bytes),
    ZSet { member: Bytes, score: f64 },
}

/// 一步写操作。
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOp {
    RenameKey {
        old: Bytes,
        new: Bytes,
    },
    SetTtl {
        key: Bytes,
        seconds: i64,
    },
    /// 写 String。编辑已有的值用 `KeepTtl`(v1 的普通 SET 会清掉 TTL,是缺陷,不照搬)。
    SetString {
        key: Bytes,
        value: Bytes,
        mode: SetMode,
    },
    /// 新建 key:已存在报错,不覆盖。String 用 `SET NX`(原子);其他类型先 EXISTS 再写,
    /// 两步之间不是原子的(和改名类操作一样的竞态)。
    CreateKey {
        key: Bytes,
        content: NewContent,
    },
    Member {
        key: Bytes,
        op: MemberOp,
    },
}

/// 一串写操作中途失败:前 `done` 步已经成功。
#[derive(Debug)]
pub struct ApplyFailure {
    pub done: usize,
    pub error: Error,
}

/// 依次执行,第一个失败就停。
pub async fn apply(pool: &Pool, ops: &[WriteOp]) -> std::result::Result<(), ApplyFailure> {
    for (done, op) in ops.iter().enumerate() {
        run(pool, op)
            .await
            .map_err(|error| ApplyFailure { done, error })?;
    }
    Ok(())
}

async fn run(pool: &Pool, op: &WriteOp) -> Result<()> {
    match op {
        WriteOp::RenameKey { old, new } => keys::rename_key(pool, old, new).await,
        WriteOp::SetTtl { key, seconds } => keys::set_ttl(pool, key, *seconds).await,
        WriteOp::SetString { key, value, mode } => string::set(pool, key, value, *mode).await,
        WriteOp::CreateKey { key, content } => create_key(pool, key, content).await,
        WriteOp::Member { key, op } => member_op(pool, key, op).await,
    }
}

async fn create_key(pool: &Pool, key: &Bytes, content: &NewContent) -> Result<()> {
    if let NewContent::String(value) = content {
        return string::set(pool, key, value, SetMode::Create).await;
    }
    if keys::exists(pool, key).await? {
        return Err(Error::AlreadyExists {
            entity: Entity::Key,
            name: key.clone(),
            container: None,
        });
    }
    match content {
        NewContent::String(_) => unreachable!("handled above"),
        NewContent::Hash { field, value } => hash::add(pool, key, field, value).await,
        NewContent::List { value, side } => list::push(pool, key, value, *side).await.map(|_| ()),
        NewContent::Set(member) => set::add(pool, key, member).await,
        NewContent::ZSet { member, score } => zset::add(pool, key, member, *score).await,
    }
}

async fn member_op(pool: &Pool, key: &Bytes, op: &MemberOp) -> Result<()> {
    match op {
        MemberOp::HashSet { field, value } => hash::set(pool, key, field, value).await,
        MemberOp::HashAdd { field, value } => hash::add(pool, key, field, value).await,
        MemberOp::HashRename { old, new } => hash::rename_field(pool, key, old, new).await,
        MemberOp::HashDelete(fields) => hash::delete(pool, key, fields).await.map(|_| ()),
        MemberOp::ListPush { value, side } => list::push(pool, key, value, *side).await.map(|_| ()),
        MemberOp::ListSet { index, value } => list::set(pool, key, *index, value).await,
        MemberOp::ListRemove(indexes) => list::remove_at(pool, key, indexes).await,
        MemberOp::SetAdd(member) => set::add(pool, key, member).await,
        MemberOp::SetRename { old, new } => set::rename_member(pool, key, old, new).await,
        MemberOp::SetRemove(members) => set::remove(pool, key, members).await.map(|_| ()),
        MemberOp::ZAdd { member, score } => zset::add(pool, key, member, *score).await,
        MemberOp::ZSetScore { member, score } => zset::set_score(pool, key, member, *score).await,
        MemberOp::ZRename { old, new } => zset::rename_member(pool, key, old, new).await,
        MemberOp::ZRemove(members) => zset::remove(pool, key, members).await.map(|_| ()),
    }
}

/// 扫一批成员。Hash、Set、ZSet 用 HSCAN、SSCAN、ZSCAN(`pattern` 匹配名字);
/// List 用 LRANGE 按页取(`pattern` 不起作用,过滤由界面在已加载的行里做),
/// 游标是下一页的起始下标,取完归零。Stream 和其他类型不能打开。
pub async fn scan_members(
    pool: &Pool,
    key: &[u8],
    kind: &KeyType,
    pattern: &[u8],
    count: u32,
    cursor: u64,
) -> Result<MemberPage> {
    match kind {
        KeyType::Hash => {
            let ScanPage { items, cursor } = hash::scan(pool, key, pattern, count, cursor).await?;
            Ok(MemberPage {
                items: items
                    .into_iter()
                    .map(|(field, value)| Member::Hash { field, value })
                    .collect(),
                cursor,
            })
        }
        KeyType::Set => {
            let ScanPage { items, cursor } = set::scan(pool, key, pattern, count, cursor).await?;
            Ok(MemberPage {
                items: items
                    .into_iter()
                    .map(|member| Member::Set { member })
                    .collect(),
                cursor,
            })
        }
        KeyType::ZSet => {
            let ScanPage { items, cursor } = zset::scan(pool, key, pattern, count, cursor).await?;
            Ok(MemberPage {
                items: items
                    .into_iter()
                    .map(|(member, score)| Member::ZSet { member, score })
                    .collect(),
                cursor,
            })
        }
        KeyType::List => {
            let start = cursor;
            let total = list::len(pool, key).await?;
            if start >= total {
                return Ok(MemberPage {
                    items: Vec::new(),
                    cursor: 0,
                });
            }
            let stop = (start + u64::from(count)).min(total) - 1;
            let values = list::range(pool, key, start as i64, stop as i64).await?;
            let next = stop + 1;
            let items: Vec<Member> = values
                .into_iter()
                .enumerate()
                .map(|(i, value)| Member::List {
                    index: start + i as u64,
                    value,
                })
                .collect();
            Ok(MemberPage {
                items,
                cursor: if next >= total { 0 } else { next },
            })
        }
        other => Err(Error::NotOpenable {
            kind: format!("{other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests;
