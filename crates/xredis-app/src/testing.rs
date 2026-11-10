//! 测试用的假 `Backend`。配置部分用真的 `ConfigStore`(临时目录加内存版 `KeyStore`,不碰真实配置和钥匙串),
//! 连接、关闭、切库是假的,不需要 Redis;可以注入延迟、错误、让某个操作一直挂着。
//! GUI 层的测试也可以用它。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;
use xredis_core::config::{ConfigStore, ConnId, ConnectionConfig, ConnectionDraft, Settings};
use xredis_core::conn::info::{ConnectionInfo, KeyspaceInfo};
use xredis_core::keystore::MemoryStore;
use xredis_core::{Error, Result};

use crate::backend::Backend;
use crate::msg::{Saved, StartupData};

#[derive(Default)]
struct Control {
    calls: Vec<String>,
    failures: HashMap<&'static str, VecDeque<Error>>,
    hung: HashSet<&'static str>,
    delay: Duration,
    keystore_degraded: bool,
    startup_warnings: Vec<String>,
    /// `connect` 返回的每个库的 key 数(下标是库号)。
    key_counts: Vec<i64>,
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

    pub fn set_startup_warnings(&self, w: Vec<String>) {
        self.ctl.lock().unwrap().startup_warnings = w;
    }

    pub fn set_key_counts(&self, counts: Vec<i64>) {
        self.ctl.lock().unwrap().key_counts = counts;
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
}
