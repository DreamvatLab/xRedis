//! 测试用的假 `Backend`。配置部分用真的 `ConfigStore`(临时目录加内存版 `KeyStore`,不碰真实配置和钥匙串),
//! 连接、关闭、切库是假的,不需要 Redis;可以注入延迟、错误、让某个操作一直挂着。
//! GUI 层的测试也可以用它。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use tempfile::TempDir;
use xredis_core::config::{ConfigStore, ConnId, ConnectionConfig, ConnectionDraft, Settings};
use xredis_core::conn::info::{ConnectionInfo, KeyspaceInfo};
use xredis_core::keys::{KeyDetails, KeyType, ScanPage, Ttl};
use xredis_core::keystore::MemoryStore;
use xredis_core::members::{ApplyFailure, Member, MemberOp, MemberPage, NewContent, WriteOp};
use xredis_core::pubsub::PubsubMessage;
use xredis_core::transfer::{ClipboardEntry, CopyOutcome, PasteOutcome, PastePreview};
use xredis_core::types::list::Side;
use xredis_core::types::string::{SetMode, StringValue};
use xredis_core::{Error, Result};

use crate::backend::Backend;
use crate::msg::{Saved, StartupData, StartupWarning};

#[derive(Default)]
struct Control {
    calls: Vec<String>,
    failures: HashMap<&'static str, VecDeque<Error>>,
    hung: HashSet<&'static str>,
    delay: Duration,
    keystore_degraded: bool,
    startup_warnings: Vec<StartupWarning>,
    /// `connect` 返回的每个库的 key 数(下标是库号)。
    key_counts: Vec<i64>,
    /// 内存里的 keyspace(按名字排序),`scan_keys` 按它分页。
    keyspace: BTreeMap<Bytes, KeyDetails>,
    /// 每一页还会把上一页的最后 N 个 key 再返回一遍(模拟 SCAN 的重复)。
    overlap: usize,
    last_page: Vec<Bytes>,
    /// `key_details` 正在进行的数量和出现过的最大值(验证并发上限)。
    details_now: usize,
    details_max: usize,
    /// String 的值(R-07b);`Some(total)` 表示只“读到”前面一部分,真实长度是 total。
    strings: HashMap<Bytes, (Bytes, Option<u64>)>,
    /// 成员(Hash、List、Set、ZSet),按插入顺序,`scan_members` 按它分页。
    members: HashMap<Bytes, Vec<Member>>,
    /// 成功执行过的写操作。
    applied: Vec<WriteOp>,
    /// 控制台执行过的命令(分好词的)。
    commands: Vec<Vec<Vec<u8>>>,
    /// 订阅收到的消息交给谁(`Runtime::new` 设置)。
    sink: Option<Arc<dyn Fn(PubsubMessage) + Send + Sync>>,
    /// 每个连接订阅的频道(同时只订阅一个)。
    subs: HashMap<ConnId, Bytes>,
    /// `channels` 返回的频道列表。
    channel_list: Vec<Bytes>,
}

pub struct FakeBackend {
    store: ConfigStore,
    ctl: Mutex<Control>,
    _dir: TempDir,
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        let dir = tempfile::tempdir().expect("temp dir");
        let (store, _) =
            ConfigStore::open(dir.path(), Arc::new(MemoryStore::new())).expect("open config");
        Arc::new(Self {
            store,
            ctl: Mutex::new(Control {
                key_counts: vec![0; 16],
                ..Default::default()
            }),
            _dir: dir,
        })
    }

    /// 预置一个连接,返回它的 id。
    pub fn add_connection(&self, name: &str, ordinal: i32) -> ConnId {
        self.store
            .save_connection(ConnectionDraft {
                id: None,
                name: name.into(),
                host: "127.0.0.1".into(),
                port: 6379,
                ordinal,
                username: None,
                tls: None,
                password: Default::default(),
            })
            .expect("save connection")
            .id
    }

    pub fn store(&self) -> &ConfigStore {
        &self.store
    }

    /// 下一次调用 `op`(如 `"connect"`)返回这个错误。可以连着排几个。
    pub fn fail_next(&self, op: &'static str, err: Error) {
        self.ctl
            .lock()
            .unwrap()
            .failures
            .entry(op)
            .or_default()
            .push_back(err);
    }

    /// 让 `op` 一直挂着不返回(模拟连不上的主机)。
    pub fn hang(&self, op: &'static str) {
        self.ctl.lock().unwrap().hung.insert(op);
    }

    pub fn set_delay(&self, d: Duration) {
        self.ctl.lock().unwrap().delay = d;
    }

    pub fn set_keystore_degraded(&self, v: bool) {
        self.ctl.lock().unwrap().keystore_degraded = v;
    }

    pub fn set_startup_warnings(&self, w: Vec<StartupWarning>) {
        self.ctl.lock().unwrap().startup_warnings = w;
    }

    pub fn set_key_counts(&self, counts: Vec<i64>) {
        self.ctl.lock().unwrap().key_counts = counts;
    }

    /// 往 keyspace 里放一个 key。
    pub fn put_key(&self, key: &[u8], kind: KeyType, length: Option<u64>, ttl: Ttl) {
        self.ctl.lock().unwrap().keyspace.insert(
            Bytes::copy_from_slice(key),
            KeyDetails { kind, length, ttl },
        );
    }

    /// 放 `n` 个 String key:`prefix0`、`prefix1`……,TTL 都是永不过期。
    pub fn put_many(&self, prefix: &str, n: usize) {
        for i in 0..n {
            self.put_key(
                format!("{prefix}{i}").as_bytes(),
                KeyType::String,
                Some(i as u64),
                Ttl::Persistent,
            );
        }
    }

    /// 放一个 String key 和它的值。
    pub fn put_string(&self, key: &[u8], value: &[u8]) {
        let mut c = self.ctl.lock().unwrap();
        c.keyspace.insert(
            Bytes::copy_from_slice(key),
            KeyDetails {
                kind: KeyType::String,
                length: Some(value.len() as u64),
                ttl: Ttl::Persistent,
            },
        );
        c.strings.insert(
            Bytes::copy_from_slice(key),
            (Bytes::copy_from_slice(value), None),
        );
    }

    /// 放一个“大”String:只读得到 `shown`,真实长度是 `total`。
    pub fn put_big_string(&self, key: &[u8], shown: &[u8], total: u64) {
        self.put_string(key, shown);
        let mut c = self.ctl.lock().unwrap();
        c.strings.insert(
            Bytes::copy_from_slice(key),
            (Bytes::copy_from_slice(shown), Some(total)),
        );
    }

    /// 放一个带成员的 key。
    pub fn put_members(&self, key: &[u8], kind: KeyType, members: Vec<Member>) {
        let mut c = self.ctl.lock().unwrap();
        c.keyspace.insert(
            Bytes::copy_from_slice(key),
            KeyDetails {
                kind,
                length: Some(members.len() as u64),
                ttl: Ttl::Persistent,
            },
        );
        c.members.insert(Bytes::copy_from_slice(key), members);
    }

    pub fn string_value(&self, key: &[u8]) -> Option<Bytes> {
        self.ctl
            .lock()
            .unwrap()
            .strings
            .get(key)
            .map(|(v, _)| v.clone())
    }

    pub fn member_rows(&self, key: &[u8]) -> Vec<Member> {
        self.ctl
            .lock()
            .unwrap()
            .members
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn key_ttl(&self, key: &[u8]) -> Option<Ttl> {
        self.ctl.lock().unwrap().keyspace.get(key).map(|d| d.ttl)
    }

    /// 控制台执行过的命令,按顺序。
    pub fn commands(&self) -> Vec<Vec<Vec<u8>>> {
        self.ctl.lock().unwrap().commands.clone()
    }

    pub fn set_channel_list(&self, list: Vec<Bytes>) {
        self.ctl.lock().unwrap().channel_list = list;
    }

    /// 这个连接现在订阅的频道。
    pub fn subscribed_channel(&self, id: ConnId) -> Option<Bytes> {
        self.ctl.lock().unwrap().subs.get(&id).cloned()
    }

    /// 模拟服务端推来一条消息(别的客户端发布的)。
    pub fn emit(&self, conn: ConnId, channel: &[u8], payload: &[u8]) {
        let sink = self.ctl.lock().unwrap().sink.clone();
        if let Some(sink) = sink {
            sink(PubsubMessage {
                conn,
                channel: Bytes::copy_from_slice(channel),
                payload: Bytes::copy_from_slice(payload),
                at_ms: xredis_core::config::now_ms(),
            });
        }
    }

    /// 成功执行过的写操作,按顺序。
    pub fn applied(&self) -> Vec<WriteOp> {
        self.ctl.lock().unwrap().applied.clone()
    }

    /// 让每一页多返回上一页最后 `n` 个 key(重复)。
    pub fn set_scan_overlap(&self, n: usize) {
        self.ctl.lock().unwrap().overlap = n;
    }

    pub fn keyspace_len(&self) -> usize {
        self.ctl.lock().unwrap().keyspace.len()
    }

    pub fn has_key(&self, key: &[u8]) -> bool {
        self.ctl.lock().unwrap().keyspace.contains_key(key)
    }

    /// 悄悄删掉一个 key(模拟别的客户端删的)。
    pub fn remove_key(&self, key: &[u8]) {
        self.ctl.lock().unwrap().keyspace.remove(key);
    }

    /// `key_details` 同时进行的最大数量。
    pub fn max_concurrent_details(&self) -> usize {
        self.ctl.lock().unwrap().details_max
    }

    /// 已经发生的调用,形如 `"connect:<id>"`、`"select_db:<id>:3"`。
    pub fn calls(&self) -> Vec<String> {
        self.ctl.lock().unwrap().calls.clone()
    }

    /// 记录调用、等延迟、挂起或按注入的错误失败。
    async fn enter(&self, op: &'static str, detail: String) -> Result<()> {
        let (delay, hung, failure) = {
            let mut c = self.ctl.lock().unwrap();
            c.calls.push(if detail.is_empty() {
                op.to_string()
            } else {
                format!("{op}:{detail}")
            });
            (
                c.delay,
                c.hung.contains(op),
                c.failures.get_mut(op).and_then(|q| q.pop_front()),
            )
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if hung {
            std::future::pending::<()>().await;
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl Backend for FakeBackend {
    async fn startup(&self) -> StartupData {
        let _ = self.enter("startup", String::new()).await;
        let c = self.ctl.lock().unwrap();
        StartupData {
            settings: self.store.settings(),
            connections: self.store.connections(),
            warnings: c.startup_warnings.clone(),
            keystore_degraded: c.keystore_degraded,
            ..Default::default()
        }
    }

    async fn connections(&self) -> Vec<ConnectionConfig> {
        let _ = self.enter("connections", String::new()).await;
        self.store.connections()
    }

    async fn save_connection(&self, draft: ConnectionDraft) -> Result<Saved> {
        self.enter("save_connection", String::new()).await?;
        let outcome = self.store.save_connection(draft)?;
        Ok(Saved {
            outcome,
            keystore_degraded: self.ctl.lock().unwrap().keystore_degraded,
        })
    }

    async fn delete_connection(&self, id: ConnId) -> Result<()> {
        self.enter("delete_connection", id.to_string()).await?;
        self.store.delete_connection(id)
    }

    async fn connect(&self, id: ConnId) -> Result<ConnectionInfo> {
        self.enter("connect", id.to_string()).await?;
        // 连接必须存在(和真实后端一样:配置里找不到就是 NotFound)
        self.store.connection(id)?;
        let counts = self.ctl.lock().unwrap().key_counts.clone();
        Ok(ConnectionInfo {
            databases: counts
                .iter()
                .enumerate()
                .map(|(db, keys)| KeyspaceInfo {
                    db: db as u32,
                    keys: *keys,
                    expires: 0,
                    avg_ttl: 0,
                })
                .collect(),
        })
    }

    async fn close_connection(&self, id: ConnId) -> Result<()> {
        self.enter("close_connection", id.to_string()).await
    }

    async fn select_db(&self, id: ConnId, db: u32) -> Result<()> {
        self.enter("select_db", format!("{id}:{db}")).await?;
        self.store.set_selected_db(id, db)
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        self.enter("save_settings", String::new()).await?;
        self.store.save_settings(&settings)
    }

    async fn save_window_size(&self, width: u32, height: u32, maximized: bool) -> Result<()> {
        self.enter("save_window_size", format!("{width}x{height}:{maximized}"))
            .await?;
        self.store.save_window_size(width, height, maximized)
    }

    async fn scan_keys(
        &self,
        id: ConnId,
        pattern: Bytes,
        count: u32,
        cursor: u64,
    ) -> Result<ScanPage<Bytes>> {
        self.enter("scan_keys", format!("{id}:{cursor}")).await?;
        let mut c = self.ctl.lock().unwrap();
        let all: Vec<Bytes> = c.keyspace.keys().cloned().collect();
        let start = (cursor as usize).min(all.len());
        let end = (start + count as usize).min(all.len());
        // 和真实的 SCAN 一样:先取 COUNT 个候选,再按 MATCH 过滤,所以一页可能是空的但游标没归零
        let mut items: Vec<Bytes> = all[start..end]
            .iter()
            .filter(|k| glob_match(&pattern, k))
            .cloned()
            .collect();
        let overlap = c.overlap;
        let dup: Vec<Bytes> = c.last_page.iter().rev().take(overlap).cloned().collect();
        c.last_page = items.clone();
        items.extend(dup);
        let next = if end >= all.len() { 0 } else { end as u64 };
        Ok(ScanPage {
            items,
            cursor: next,
        })
    }

    async fn key_details(&self, id: ConnId, key: Bytes) -> Result<KeyDetails> {
        {
            let mut c = self.ctl.lock().unwrap();
            c.details_now += 1;
            c.details_max = c.details_max.max(c.details_now);
        }
        let entered = self.enter("key_details", format!("{id}")).await;
        let mut c = self.ctl.lock().unwrap();
        c.details_now -= 1;
        entered?;
        Ok(c.keyspace.get(&key).cloned().unwrap_or(KeyDetails {
            kind: KeyType::None,
            length: None,
            ttl: Ttl::Missing,
        }))
    }

    async fn delete_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<u64> {
        self.enter("delete_keys", format!("{id}:{}", keys.len()))
            .await?;
        let mut c = self.ctl.lock().unwrap();
        Ok(keys
            .iter()
            .filter(|k| c.keyspace.remove(*k).is_some())
            .count() as u64)
    }

    async fn db_size(&self, id: ConnId) -> Result<u64> {
        self.enter("db_size", format!("{id}")).await?;
        Ok(self.ctl.lock().unwrap().keyspace.len() as u64)
    }

    async fn copy_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<CopyOutcome> {
        self.enter("copy_keys", format!("{id}:{}", keys.len()))
            .await?;
        let c = self.ctl.lock().unwrap();
        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        for k in keys {
            match (c.keyspace.get(&k), String::from_utf8(k.to_vec())) {
                (Some(_), Ok(name)) => entries.push(ClipboardEntry {
                    value: name.as_bytes().to_vec(),
                    id: name,
                    ttl_ms: 0,
                }),
                _ => skipped.push(xredis_core::error::Skipped {
                    key: k,
                    reason: xredis_core::error::SkipReason::Missing,
                }),
            }
        }
        if entries.is_empty() {
            return Err(Error::NothingCopied { skipped });
        }
        Ok(CopyOutcome {
            text: xredis_core::transfer::encode(&entries)?,
            copied: entries.len(),
            skipped,
        })
    }

    async fn run_command(&self, id: ConnId, tokens: Vec<Vec<u8>>) -> Result<String> {
        let name = String::from_utf8_lossy(tokens.first().map_or(&[][..], |t| t)).to_uppercase();
        self.enter("run_command", format!("{id}:{name}")).await?;
        self.ctl.lock().unwrap().commands.push(tokens.clone());
        // 最小的命令集:够测控制台的流程
        Ok(match name.as_str() {
            "PING" => "PONG".to_string(),
            "BIG" => {
                // BIG n:n 行输出,测输出行数上限
                let n: usize = tokens
                    .get(1)
                    .and_then(|t| String::from_utf8_lossy(t).parse().ok())
                    .unwrap_or(1);
                (1..=n)
                    .map(|i| format!("{i}) line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => format!(
                "OK {}",
                tokens
                    .iter()
                    .skip(1)
                    .map(|t| String::from_utf8_lossy(t).into_owned())
                    .collect::<Vec<_>>()
                    .join("|")
            ),
        })
    }

    async fn subscribe(&self, id: ConnId, channel: Bytes) -> Result<()> {
        self.enter(
            "subscribe",
            format!("{id}:{}", String::from_utf8_lossy(&channel)),
        )
        .await?;
        self.ctl.lock().unwrap().subs.insert(id, channel);
        Ok(())
    }

    async fn unsubscribe(&self, id: ConnId, channel: Bytes) -> Result<()> {
        self.enter(
            "unsubscribe",
            format!("{id}:{}", String::from_utf8_lossy(&channel)),
        )
        .await?;
        // 和 core 一样:不管成败订阅都没了
        self.ctl.lock().unwrap().subs.remove(&id);
        Ok(())
    }

    async fn publish(&self, id: ConnId, channel: Bytes, message: Bytes) -> Result<u64> {
        self.enter(
            "publish",
            format!("{id}:{}", String::from_utf8_lossy(&channel)),
        )
        .await?;
        // 回环:订阅了这个频道的连接都收到
        let (sink, targets): (_, Vec<ConnId>) = {
            let c = self.ctl.lock().unwrap();
            (
                c.sink.clone(),
                c.subs
                    .iter()
                    .filter(|(_, ch)| **ch == channel)
                    .map(|(conn, _)| *conn)
                    .collect(),
            )
        };
        if let Some(sink) = sink {
            for conn in &targets {
                sink(PubsubMessage {
                    conn: *conn,
                    channel: channel.clone(),
                    payload: message.clone(),
                    at_ms: xredis_core::config::now_ms(),
                });
            }
        }
        Ok(targets.len() as u64)
    }

    async fn channels(&self, id: ConnId) -> Result<Vec<Bytes>> {
        self.enter("channels", format!("{id}")).await?;
        Ok(self.ctl.lock().unwrap().channel_list.clone())
    }

    fn set_message_sink(&self, sink: Arc<dyn Fn(PubsubMessage) + Send + Sync>) {
        self.ctl.lock().unwrap().sink = Some(sink);
    }

    async fn read_string(&self, id: ConnId, key: Bytes) -> Result<Option<StringValue>> {
        self.enter("read_string", format!("{id}")).await?;
        let c = self.ctl.lock().unwrap();
        Ok(c.strings.get(&key).map(|(data, total)| StringValue {
            data: data.clone(),
            total_len: total.unwrap_or(data.len() as u64),
            truncated: total.is_some(),
        }))
    }

    async fn scan_members(
        &self,
        id: ConnId,
        key: Bytes,
        kind: KeyType,
        _pattern: Bytes,
        count: u32,
        cursor: u64,
    ) -> Result<MemberPage> {
        self.enter("scan_members", format!("{id}:{cursor}")).await?;
        if !kind.openable() {
            return Err(Error::NotOpenable {
                kind: format!("{kind:?}"),
            });
        }
        let c = self.ctl.lock().unwrap();
        let all = c.members.get(&key).cloned().unwrap_or_default();
        let start = (cursor as usize).min(all.len());
        let end = (start + count as usize).min(all.len());
        Ok(MemberPage {
            items: all[start..end].to_vec(),
            cursor: if end >= all.len() { 0 } else { end as u64 },
        })
    }

    async fn apply_writes(
        &self,
        id: ConnId,
        ops: Vec<WriteOp>,
    ) -> std::result::Result<(), ApplyFailure> {
        self.enter("apply_writes", format!("{id}:{}", ops.len()))
            .await
            .map_err(|error| ApplyFailure { done: 0, error })?;
        let mut c = self.ctl.lock().unwrap();
        for (done, op) in ops.iter().enumerate() {
            c.apply_one(op)
                .map_err(|error| ApplyFailure { done, error })?;
            c.applied.push(op.clone());
        }
        Ok(())
    }

    async fn preview_paste(&self, id: ConnId, text: String) -> Result<PastePreview> {
        self.enter("preview_paste", format!("{id}")).await?;
        let entries = xredis_core::transfer::decode(&text)?;
        let c = self.ctl.lock().unwrap();
        let existing = entries
            .iter()
            .filter(|e| c.keyspace.contains_key(e.id.as_bytes()))
            .count();
        Ok(PastePreview {
            total: entries.len(),
            existing,
        })
    }

    async fn paste_keys(&self, id: ConnId, text: String, overwrite: bool) -> Result<PasteOutcome> {
        self.enter("paste_keys", format!("{id}:{overwrite}"))
            .await?;
        let entries = xredis_core::transfer::decode(&text)?;
        let mut c = self.ctl.lock().unwrap();
        if !overwrite {
            let conflicts: Vec<Bytes> = entries
                .iter()
                .filter(|e| c.keyspace.contains_key(e.id.as_bytes()))
                .map(|e| Bytes::copy_from_slice(e.id.as_bytes()))
                .collect();
            if !conflicts.is_empty() {
                return Err(Error::PasteConflicts {
                    conflicts,
                    total: entries.len(),
                });
            }
        }
        let count = entries.len();
        for e in entries {
            c.keyspace.insert(
                Bytes::from(e.id),
                KeyDetails {
                    kind: KeyType::String,
                    length: Some(e.value.len() as u64),
                    ttl: Ttl::Persistent,
                },
            );
        }
        Ok(PasteOutcome { count })
    }
}

fn exists_err(key: &Bytes) -> Error {
    Error::AlreadyExists {
        entity: xredis_core::error::Entity::Key,
        name: key.clone(),
        container: None,
    }
}

fn missing_err(key: &Bytes) -> Error {
    Error::Missing {
        entity: xredis_core::error::Entity::Key,
        name: key.clone(),
        container: None,
    }
}

impl Control {
    fn refresh_len(&mut self, key: &Bytes) {
        // List 的下标跟着重排
        if let Some(rows) = self.members.get_mut(key) {
            let mut i = 0;
            for r in rows.iter_mut() {
                if let Member::List { index, .. } = r {
                    *index = i;
                    i += 1;
                }
            }
        }
        let n = self.members.get(key).map(|m| m.len() as u64);
        if let (Some(n), Some(d)) = (n, self.keyspace.get_mut(key)) {
            d.length = Some(n);
        }
    }

    fn put_new(&mut self, key: &Bytes, kind: KeyType) {
        self.keyspace.insert(
            key.clone(),
            KeyDetails {
                kind,
                length: Some(0),
                ttl: Ttl::Persistent,
            },
        );
    }

    /// 内存里的最小实现:够测视图模型的流程,不是 Redis 的完整语义。
    fn apply_one(&mut self, op: &WriteOp) -> Result<()> {
        match op {
            WriteOp::RenameKey { old, new } => {
                if !self.keyspace.contains_key(old) {
                    return Err(missing_err(old));
                }
                if self.keyspace.contains_key(new) {
                    return Err(exists_err(new));
                }
                let d = self.keyspace.remove(old).unwrap();
                self.keyspace.insert(new.clone(), d);
                if let Some(v) = self.strings.remove(old) {
                    self.strings.insert(new.clone(), v);
                }
                if let Some(v) = self.members.remove(old) {
                    self.members.insert(new.clone(), v);
                }
            }
            WriteOp::SetTtl { key, seconds } => {
                let d = self.keyspace.get_mut(key).ok_or_else(|| missing_err(key))?;
                d.ttl = if *seconds == -1 {
                    Ttl::Persistent
                } else {
                    Ttl::Seconds(*seconds)
                };
            }
            WriteOp::SetString { key, value, mode } => {
                let exists = self.keyspace.contains_key(key);
                if *mode == SetMode::Create && exists {
                    return Err(exists_err(key));
                }
                let ttl = match mode {
                    SetMode::KeepTtl => self.keyspace.get(key).map(|d| d.ttl),
                    _ => None,
                };
                self.keyspace.insert(
                    key.clone(),
                    KeyDetails {
                        kind: KeyType::String,
                        length: Some(value.len() as u64),
                        ttl: ttl.unwrap_or(Ttl::Persistent),
                    },
                );
                self.strings.insert(key.clone(), (value.clone(), None));
            }
            WriteOp::CreateKey { key, content } => {
                if self.keyspace.contains_key(key) {
                    return Err(exists_err(key));
                }
                match content {
                    NewContent::String(v) => {
                        self.put_new(key, KeyType::String);
                        self.keyspace.get_mut(key).unwrap().length = Some(v.len() as u64);
                        self.strings.insert(key.clone(), (v.clone(), None));
                    }
                    NewContent::Hash { field, value } => {
                        self.put_new(key, KeyType::Hash);
                        self.members.insert(
                            key.clone(),
                            vec![Member::Hash {
                                field: field.clone(),
                                value: value.clone(),
                            }],
                        );
                    }
                    NewContent::List { value, .. } => {
                        self.put_new(key, KeyType::List);
                        self.members.insert(
                            key.clone(),
                            vec![Member::List {
                                index: 0,
                                value: value.clone(),
                            }],
                        );
                    }
                    NewContent::Set(m) => {
                        self.put_new(key, KeyType::Set);
                        self.members
                            .insert(key.clone(), vec![Member::Set { member: m.clone() }]);
                    }
                    NewContent::ZSet { member, score } => {
                        self.put_new(key, KeyType::ZSet);
                        self.members.insert(
                            key.clone(),
                            vec![Member::ZSet {
                                member: member.clone(),
                                score: *score,
                            }],
                        );
                    }
                }
                self.refresh_len(key);
            }
            WriteOp::Member { key, op } => {
                if !self.keyspace.contains_key(key) {
                    return Err(missing_err(key));
                }
                self.member_op(key, op)?;
                self.refresh_len(key);
            }
        }
        Ok(())
    }

    fn member_op(&mut self, key: &Bytes, op: &MemberOp) -> Result<()> {
        use xredis_core::error::Entity;
        let dup = |entity, name: &Bytes| Error::AlreadyExists {
            entity,
            name: name.clone(),
            container: Some(key.clone()),
        };
        let gone = |entity, name: &Bytes| Error::Missing {
            entity,
            name: name.clone(),
            container: Some(key.clone()),
        };
        let rows = self.members.entry(key.clone()).or_default();
        let hash_pos = |rows: &[Member], f: &Bytes| {
            rows.iter()
                .position(|r| matches!(r, Member::Hash { field, .. } if field == f))
        };
        let set_pos = |rows: &[Member], m: &Bytes| {
            rows.iter()
                .position(|r| matches!(r, Member::Set { member } if member == m))
        };
        let zset_pos = |rows: &[Member], m: &Bytes| {
            rows.iter()
                .position(|r| matches!(r, Member::ZSet { member, .. } if member == m))
        };
        match op {
            MemberOp::HashSet { field, value } => match hash_pos(rows, field) {
                Some(i) => {
                    rows[i] = Member::Hash {
                        field: field.clone(),
                        value: value.clone(),
                    }
                }
                None => rows.push(Member::Hash {
                    field: field.clone(),
                    value: value.clone(),
                }),
            },
            MemberOp::HashAdd { field, value } => {
                if hash_pos(rows, field).is_some() {
                    return Err(dup(Entity::HashField, field));
                }
                rows.push(Member::Hash {
                    field: field.clone(),
                    value: value.clone(),
                });
            }
            MemberOp::HashRename { old, new } => {
                if hash_pos(rows, new).is_some() {
                    return Err(dup(Entity::HashField, new));
                }
                let i = hash_pos(rows, old).ok_or_else(|| gone(Entity::HashField, old))?;
                if let Member::Hash { field, .. } = &mut rows[i] {
                    *field = new.clone();
                }
            }
            MemberOp::HashDelete(fields) => {
                rows.retain(|r| !matches!(r, Member::Hash { field, .. } if fields.contains(field)))
            }
            MemberOp::ListPush { value, side } => {
                let row = Member::List {
                    index: 0,
                    value: value.clone(),
                };
                match side {
                    Side::Head => rows.insert(0, row),
                    Side::Tail => rows.push(row),
                }
            }
            MemberOp::ListSet { index, value } => match rows.get_mut(*index as usize) {
                Some(Member::List { value: v, .. }) => *v = value.clone(),
                _ => {
                    return Err(Error::ListIndexOutOfRange {
                        index: *index,
                        len: rows.len() as u64,
                    });
                }
            },
            MemberOp::ListRemove(indexes) => {
                let mut i = 0i64;
                rows.retain(|_| {
                    let keep = !indexes.contains(&i);
                    i += 1;
                    keep
                });
            }
            MemberOp::SetAdd(m) => {
                if set_pos(rows, m).is_some() {
                    return Err(dup(Entity::SetMember, m));
                }
                rows.push(Member::Set { member: m.clone() });
            }
            MemberOp::SetRename { old, new } => {
                if set_pos(rows, new).is_some() {
                    return Err(dup(Entity::SetMember, new));
                }
                let i = set_pos(rows, old).ok_or_else(|| gone(Entity::SetMember, old))?;
                rows[i] = Member::Set {
                    member: new.clone(),
                };
            }
            MemberOp::SetRemove(ms) => {
                rows.retain(|r| !matches!(r, Member::Set { member } if ms.contains(member)))
            }
            MemberOp::ZAdd { member, score } => {
                if zset_pos(rows, member).is_some() {
                    return Err(dup(Entity::ZSetMember, member));
                }
                rows.push(Member::ZSet {
                    member: member.clone(),
                    score: *score,
                });
            }
            MemberOp::ZSetScore { member, score } => {
                let i = zset_pos(rows, member).ok_or_else(|| gone(Entity::ZSetMember, member))?;
                rows[i] = Member::ZSet {
                    member: member.clone(),
                    score: *score,
                };
            }
            MemberOp::ZRename { old, new } => {
                if zset_pos(rows, new).is_some() {
                    return Err(dup(Entity::ZSetMember, new));
                }
                let i = zset_pos(rows, old).ok_or_else(|| gone(Entity::ZSetMember, old))?;
                if let Member::ZSet { member, .. } = &mut rows[i] {
                    *member = new.clone();
                }
            }
            MemberOp::ZRemove(ms) => {
                rows.retain(|r| !matches!(r, Member::ZSet { member, .. } if ms.contains(member)))
            }
        }
        Ok(())
    }
}

/// 最小的 glob:`*` 匹配任意长度,`?` 匹配一个字节,其余原样。
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob_match(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob_match(rest, &text[1..]),
    }
}
