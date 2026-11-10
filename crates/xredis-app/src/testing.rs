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
use xredis_core::transfer::{ClipboardEntry, CopyOutcome, PasteOutcome};
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

/// 最小的 glob:`*` 匹配任意长度,`?` 匹配一个字节,其余原样。
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob_match(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob_match(rest, &text[1..]),
    }
}
