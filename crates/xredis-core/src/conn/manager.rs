//! 连接生命周期:每个连接三份客户端(连接池、订阅客户端、控制台客户端),分开管理。
//!
//! 照 v1(`handlers/redis.rs`),这是踩坑最多的地方:
//!
//! - **每个连接一个槽**:一把 tokio Mutex 串行化「建客户端」「切库」「关闭」,拿锁后再查一次缓存
//!   (双检查),并发下每种客户端只建一个(L-04)。
//! - **关闭纪元**(watch channel):`close` 先把纪元加一再拿锁。建连方在等锁之前订阅,纪元一变就
//!   放弃等锁、放弃建连,所以连接中、超时期间也能立刻关 Tab(L-02)。
//! - **切库**:持锁,先写配置的 `selected_db`,再关掉该连接所有客户端,下次用时重建(L-03)。
//! - **三层重连和超时**(L-01):fred 默认不重连也不超时,Redis 一重启界面会永久转圈。
//! - 订阅客户端和控制台客户端分开,退订只关订阅客户端(L-06)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fred::clients::{Client, Pool};
use fred::cmd;
use fred::interfaces::ClientLike;
use fred::types::Builder;
use fred::types::config::{
    Config as FredConfig, ConnectionConfig as FredConnectionConfig, PerformanceConfig,
    ReconnectPolicy, TlsConfig as FredTlsConfig, TlsHostMapping, UnresponsiveConfig,
};
use tokio::sync::{MutexGuard, watch};

use super::info::{ConnectionInfo, db_count_or_default, parse_keyspace};
use super::{tls, uri};
use crate::config::{ConfigStore, ConnId, ConnectionConfig};
use crate::consts::*;
use crate::error::{Error, Result};

/// 一个连接的生命周期状态。
struct ConnSlot {
    /// 串行化「建客户端」「切库」「关闭」。
    lock: tokio::sync::Mutex<()>,
    /// 关闭纪元,每次 `close` 加一。
    closed: watch::Sender<u64>,
    pool: Mutex<Option<Pool>>,
    subscriber: Mutex<Option<Client>>,
    console: Mutex<Option<Client>>,
}

pub struct ConnManager {
    store: Arc<ConfigStore>,
    slots: Mutex<HashMap<ConnId, Arc<ConnSlot>>>,
}

/// 池和单个客户端的统一构造。
trait Built: ClientLike + Clone + Send + Sync + 'static {
    fn build(builder: Builder) -> Result<Self>;
}

impl Built for Pool {
    fn build(builder: Builder) -> Result<Self> {
        Ok(builder.build_pool(POOL_SIZE)?)
    }
}

impl Built for Client {
    fn build(builder: Builder) -> Result<Self> {
        Ok(builder.build()?)
    }
}

/// 按连接配置组装 fred 的 Builder,池和单客户端共用。
///
/// 三层防护缺一不可:指数退避重连(不限次数)、命令超时(命令必须失败返回而不是挂死)、
/// 半开连接探测(TCP 没断但对端不响应时,比等命令超时更早发现并重连)。
fn build_builder(cfg: &ConnectionConfig, password: Option<&str>) -> Result<Builder> {
    let mut config = FredConfig::from_url(&uri::redis_uri(cfg, password))?;
    if let Some(tls_cfg) = &cfg.tls {
        config.tls = Some(FredTlsConfig {
            connector: tls::connector(tls_cfg)?,
            hostnames: TlsHostMapping::DefaultHost,
        });
    }
    let mut builder = Builder::from_config(config);
    // 0 = 不限次数:桌面应用长期开着,Redis 可能重启很多次
    builder.set_policy(ReconnectPolicy::new_exponential(
        0,
        RECONNECT_MIN_DELAY_MS,
        RECONNECT_MAX_DELAY_MS,
        RECONNECT_BACKOFF_BASE,
    ));
    builder.set_performance_config(PerformanceConfig {
        default_command_timeout: Duration::from_millis(COMMAND_TIMEOUT_MS),
        ..Default::default()
    });
    builder.set_connection_config(FredConnectionConfig {
        connection_timeout: Duration::from_millis(CONNECT_TIMEOUT_MS),
        unresponsive: UnresponsiveConfig {
            max_timeout: Some(Duration::from_millis(UNRESPONSIVE_TIMEOUT_MS)),
            interval: Duration::from_millis(UNRESPONSIVE_INTERVAL_MS),
        },
        ..Default::default()
    });
    Ok(builder)
}

/// 取建客户端的锁;等锁期间连接被关闭则放弃。
///
/// `closed` 必须在调用前订阅好(`subscribe` 把当前纪元标记为已读),
/// 这样订阅之后发生的关闭一定能被看到。
async fn lock_unless_closed<'a>(
    slot: &'a ConnSlot,
    closed: &mut watch::Receiver<u64>,
) -> Result<MutexGuard<'a, ()>> {
    tokio::select! {
        biased;
        _ = closed.changed() => Err(Error::Closed),
        guard = slot.lock.lock() => Ok(guard),
    }
}

/// 初始化 `client` 并等它连上;期间连接被关闭则立刻放弃。
///
/// fred 的 `init` 一旦开始就叫不停:丢弃 future 不会停掉它 spawn 出去的路由任务。
/// 所以放弃时把 init 交给后台跑完 —— 失败的话 fred 自己会收掉路由任务,
/// 成功的话立刻 `quit()`,不留下一条没人要的连接。
async fn init_unless_closed<C: Built>(client: C, closed: &mut watch::Receiver<u64>) -> Result<C> {
    let mut init = tokio::spawn({
        let client = client.clone();
        async move { client.init().await }
    });
    tokio::select! {
        biased;
        _ = closed.changed() => {
            tokio::spawn(async move {
                if let Ok(Ok(_)) = init.await {
                    let _ = client.quit().await;
                }
            });
            Err(Error::Closed)
        }
        r = &mut init => {
            r.map_err(|e| Error::Redis(e.to_string()))??;
            Ok(client)
        }
    }
}

impl ConnManager {
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self {
            store,
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub fn store(&self) -> &Arc<ConfigStore> {
        &self.store
    }

    /// 取出槽(没有就建)。不能在持有 `slots` 的锁时 await,所以先克隆出 `Arc`。
    fn slot(&self, id: ConnId) -> Arc<ConnSlot> {
        self.slots
            .lock()
            .unwrap()
            .entry(id)
            .or_insert_with(|| {
                Arc::new(ConnSlot {
                    lock: tokio::sync::Mutex::new(()),
                    closed: watch::Sender::new(0),
                    pool: Mutex::new(None),
                    subscriber: Mutex::new(None),
                    console: Mutex::new(None),
                })
            })
            .clone()
    }

    /// 取缓存的客户端,没有则新建并写入缓存。`on_created` 只在真正新建时调用一次。
    async fn ensure<T: Built>(
        &self,
        id: ConnId,
        pick: fn(&ConnSlot) -> &Mutex<Option<T>>,
        on_created: impl FnOnce(&T),
    ) -> Result<T> {
        let slot = self.slot(id);
        // 快路径:已有缓存直接返回,不必加锁
        let cached = pick(&slot).lock().unwrap().clone();
        if let Some(c) = cached {
            return Ok(c);
        }
        // 慢路径:先订阅纪元再等锁
        let mut closed = slot.closed.subscribe();
        let _guard = lock_unless_closed(&slot, &mut closed).await?;
        // 等锁期间可能已被其他任务建好,二次检查避免重复建立(L-04)
        let cached = pick(&slot).lock().unwrap().clone();
        if let Some(c) = cached {
            return Ok(c);
        }
        // 每次都读配置:selected_db 可能变了
        let cfg = self.store.connection(id)?;
        let password = self.store.password(id)?;
        let built = T::build(build_builder(&cfg, password.as_deref())?)?;
        let client = init_unless_closed(built, &mut closed).await?;
        on_created(&client);
        *pick(&slot).lock().unwrap() = Some(client.clone());
        Ok(client)
    }

    /// 普通命令用的连接池。
    pub async fn pool(&self, id: ConnId) -> Result<Pool> {
        self.ensure(id, |s| &s.pool, |_| {}).await
    }

    /// 控制台客户端(也用于 `PUBSUB CHANNELS`)。
    pub async fn console_client(&self, id: ConnId) -> Result<Client> {
        self.ensure(id, |s| &s.console, |_| {}).await
    }

    /// 订阅客户端。`on_created` 只在新建时调用一次,用来注册消息监听器 —— 监听器只能注册一次,
    /// 否则同一条消息会被投递多次(L-05)。
    pub async fn subscriber_client(
        &self,
        id: ConnId,
        on_created: impl FnOnce(&Client),
    ) -> Result<Client> {
        self.ensure(id, |s| &s.subscriber, on_created).await
    }

    /// 哪几种客户端已经建好:(连接池, 订阅客户端, 控制台客户端)。
    pub fn cached(&self, id: ConnId) -> (bool, bool, bool) {
        let slot = self.slot(id);
        (
            slot.pool.lock().unwrap().is_some(),
            slot.subscriber.lock().unwrap().is_some(),
            slot.console.lock().unwrap().is_some(),
        )
    }

    /// 关闭连接的所有客户端。先把纪元加一,正在建的连接和排队等锁的请求立刻放弃,所以总能很快拿到锁。
    pub async fn close(&self, id: ConnId) -> Result<()> {
        let slot = self.slot(id);
        slot.closed.send_modify(|e| *e = e.wrapping_add(1));
        let _guard = slot.lock.lock().await;
        close_all(&slot).await
    }

    /// 只关订阅客户端(退订用),不动连接池和控制台客户端(L-06)。
    pub async fn close_subscriber(&self, id: ConnId) -> Result<()> {
        let slot = self.slot(id);
        let _guard = slot.lock.lock().await;
        let client = slot.subscriber.lock().unwrap().take();
        match client {
            Some(c) => c.quit().await.map_err(Error::from),
            None => Ok(()),
        }
    }

    /// 切库:持锁,**先写配置,再清缓存**(L-03)。这样即使本函数返回后立刻有请求建池,
    /// 它从配置里读到的也已经是新库;少了这把锁,并发请求会把连接池固定在旧库且不会自愈。
    pub async fn select_db(&self, id: ConnId, db: u32) -> Result<()> {
        let slot = self.slot(id);
        let _guard = slot.lock.lock().await;
        self.store.set_selected_db(id, db)?;
        close_all(&slot).await
    }

    /// 连接信息:每个库一条(库数量来自 `CONFIG GET databases`,失败回落 16;统计来自
    /// `INFO keyspace`,失败显示 0)。托管 Redis 常禁用 CONFIG,不能让整个调用失败(L-11)。
    pub async fn connection_info(&self, id: ConnId) -> Result<ConnectionInfo> {
        let pool = self.pool(id).await?;
        let config_get: std::result::Result<Vec<String>, fred::error::Error> =
            pool.custom(cmd!("CONFIG"), vec!["GET", "databases"]).await;
        if let Err(err) = &config_get {
            tracing::debug!("CONFIG GET databases 不可用,回落到 {DEFAULT_DB_COUNT} 个库: {err}");
        }
        let db_count = db_count_or_default(config_get.as_deref().ok());
        let keyspace: std::result::Result<String, fred::error::Error> =
            pool.custom(cmd!("INFO"), vec!["keyspace"]).await;
        let keyspace = match keyspace {
            Ok(text) => parse_keyspace(&text),
            Err(err) => {
                tracing::debug!("INFO keyspace 不可用,各库 key 数显示为 0: {err}");
                Default::default()
            }
        };
        Ok(ConnectionInfo::build(db_count, &keyspace))
    }
}

/// 关闭槽里所有客户端(调用方已持锁)。每个都尝试关,返回第一个错误。
async fn close_all(slot: &ConnSlot) -> Result<()> {
    let pool = slot.pool.lock().unwrap().take();
    let subscriber = slot.subscriber.lock().unwrap().take();
    let console = slot.console.lock().unwrap().take();
    let mut first_err: Option<Error> = None;
    let pool_result = match pool {
        Some(p) => p.quit().await,
        None => Ok(()),
    };
    if let Err(e) = pool_result {
        first_err.get_or_insert(e.into());
    }
    for c in [subscriber, console].into_iter().flatten() {
        if let Err(e) = c.quit().await {
            first_err.get_or_insert(e.into());
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
