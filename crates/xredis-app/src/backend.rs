//! `Backend`:app 访问 core 的**唯一入口**(连接、命令、配置、密钥)。做成 `Arc<dyn Backend>`,
//! 生产实现 [`CoreBackend`] 包 core,测试实现是内存假数据([`FakeBackend`](crate::testing::FakeBackend))。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use xredis_core::Result;
use xredis_core::config::{
    ConfigStore, ConnId, ConnectionConfig, ConnectionDraft, SaveOutcome, Settings,
};
use xredis_core::conn::ConnManager;
use xredis_core::conn::info::ConnectionInfo;
use xredis_core::keys::{KeyDetails, ScanPage};
use xredis_core::keystore::{FallbackStore, KeyStore, KeyringStore};
use xredis_core::transfer::{CopyOutcome, PasteOutcome, PastePreview};
use xredis_core::v1_import::{import_v1_if_first_run, v1_config_path};

use crate::msg::{ImportSummary, Saved, StartupData, StartupWarning};

#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// 启动时加载一切。**不会失败**:配置不可用时用默认值继续,问题放进 `warnings`(P-66)。
    async fn startup(&self) -> StartupData;
    async fn connections(&self) -> Vec<ConnectionConfig>;
    async fn save_connection(&self, draft: ConnectionDraft) -> Result<Saved>;
    async fn delete_connection(&self, id: ConnId) -> Result<()>;
    /// 建连并取连接信息(每个库的 key 数)。
    async fn connect(&self, id: ConnId) -> Result<ConnectionInfo>;
    async fn close_connection(&self, id: ConnId) -> Result<()>;
    async fn select_db(&self, id: ConnId, db: u32) -> Result<()>;
    async fn save_settings(&self, settings: Settings) -> Result<()>;
    async fn save_window_size(&self, width: u32, height: u32, maximized: bool) -> Result<()>;

    // ---- key 操作(R-07a)----
    /// 扫一批 key。`pattern` 为 `*` 时不带 MATCH。
    async fn scan_keys(
        &self,
        id: ConnId,
        pattern: Bytes,
        count: u32,
        cursor: u64,
    ) -> Result<ScanPage<Bytes>>;
    async fn key_details(&self, id: ConnId, key: Bytes) -> Result<KeyDetails>;
    /// 批量删除(core 内部每批 500 个),返回实际删掉的数量。
    async fn delete_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<u64>;
    async fn db_size(&self, id: ConnId) -> Result<u64>;
    async fn copy_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<CopyOutcome>;
    /// 粘贴前预检:解码并查每个 key 是否已存在。
    async fn preview_paste(&self, id: ConnId, text: String) -> Result<PastePreview>;
    async fn paste_keys(&self, id: ConnId, text: String, overwrite: bool) -> Result<PasteOutcome>;
}

/// 生产实现。
pub struct CoreBackend {
    store: Arc<ConfigStore>,
    manager: Arc<ConnManager>,
    keystore: Arc<FallbackStore>,
    config_dir: PathBuf,
    v1_path: Option<PathBuf>,
    /// 打开配置时的警告(改用了临时目录、坏文件已备份),在 `startup` 里带给界面。
    open_warnings: Vec<StartupWarning>,
}

impl CoreBackend {
    /// 生产环境:默认配置目录,系统钥匙串(不可用时降级到内存,界面提示一次),
    /// 第一次启动时从 v1 导入。
    pub fn production() -> Result<Self> {
        let dir = xredis_core::config::default_config_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("xRedis-v2"));
        Self::open(dir, Arc::new(KeyringStore::new()), v1_config_path())
    }

    /// 打开配置。配置目录建不出来(路径被文件占用、没有权限)时改用临时目录继续运行并带警告,
    /// 不崩溃(P-66);临时目录也不行才返回错误。
    pub fn open(
        config_dir: PathBuf,
        primary_keystore: Arc<dyn KeyStore>,
        v1_path: Option<PathBuf>,
    ) -> Result<Self> {
        let keystore = Arc::new(FallbackStore::new(primary_keystore));
        let mut open_warnings = Vec::new();
        let (store, dir) = match ConfigStore::open(&config_dir, keystore.clone()) {
            Ok((store, warnings)) => {
                open_warnings.extend(warnings.into_iter().map(StartupWarning::Config));
                (store, config_dir)
            }
            Err(err) => {
                let fallback = std::env::temp_dir().join(format!("xRedis-v2-{}", ConnId::new()));
                open_warnings.push(StartupWarning::ConfigDirUnusable {
                    reason: err.to_string(),
                    fallback: fallback.clone(),
                });
                let (store, warnings) = ConfigStore::open(&fallback, keystore.clone())?;
                open_warnings.extend(warnings.into_iter().map(StartupWarning::Config));
                (store, fallback)
            }
        };
        let store = Arc::new(store);
        Ok(Self {
            manager: Arc::new(ConnManager::new(store.clone())),
            store,
            keystore,
            config_dir: dir,
            v1_path,
            open_warnings,
        })
    }

    pub fn config_dir(&self) -> &std::path::Path {
        &self.config_dir
    }

    pub fn manager(&self) -> &Arc<ConnManager> {
        &self.manager
    }

    pub fn store(&self) -> &Arc<ConfigStore> {
        &self.store
    }
}

/// 配置和密钥存储的操作会碰磁盘和系统钥匙串(Linux 的 Secret Service 可能慢),不放在异步线程上直接跑。
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a blocking task panicked")
}

#[async_trait]
impl Backend for CoreBackend {
    async fn startup(&self) -> StartupData {
        let store = self.store.clone();
        let keystore = self.keystore.clone();
        let v1_path = self.v1_path.clone();
        let locales_dir = self.config_dir.join("locales");
        let warnings = self.open_warnings.clone();
        blocking(move || {
            let packs = crate::i18n::load_user_packs(&locales_dir);
            // 探测一次钥匙串:不可用时 FallbackStore 当场降级,界面就能在启动时提示
            let _ = keystore.get("xredis-startup-probe");
            let import = v1_path.and_then(|path| match import_v1_if_first_run(&store, &path) {
                Ok(Some(r)) => Some(ImportSummary {
                    connections: r.connections,
                    warnings: r.warnings,
                    error: None,
                }),
                Ok(None) => None,
                Err(err) => Some(ImportSummary {
                    connections: 0,
                    warnings: Vec::new(),
                    error: Some(err.to_string()),
                }),
            });
            StartupData {
                settings: store.settings(),
                connections: store.connections(),
                warnings,
                keystore_degraded: keystore.degraded(),
                import,
                packs,
            }
        })
        .await
    }

    async fn connections(&self) -> Vec<ConnectionConfig> {
        self.store.connections()
    }

    async fn save_connection(&self, draft: ConnectionDraft) -> Result<Saved> {
        let store = self.store.clone();
        let keystore = self.keystore.clone();
        blocking(move || {
            let outcome: SaveOutcome = store.save_connection(draft)?;
            Ok(Saved {
                outcome,
                keystore_degraded: keystore.degraded(),
            })
        })
        .await
    }

    async fn delete_connection(&self, id: ConnId) -> Result<()> {
        // 先关连接(含订阅和控制台客户端),再删配置和密码
        let _ = self.manager.close(id).await;
        let store = self.store.clone();
        blocking(move || store.delete_connection(id)).await
    }

    async fn connect(&self, id: ConnId) -> Result<ConnectionInfo> {
        self.manager.connection_info(id).await
    }

    async fn close_connection(&self, id: ConnId) -> Result<()> {
        self.manager.close(id).await
    }

    async fn select_db(&self, id: ConnId, db: u32) -> Result<()> {
        self.manager.select_db(id, db).await
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        let store = self.store.clone();
        blocking(move || store.save_settings(&settings)).await
    }

    async fn save_window_size(&self, width: u32, height: u32, maximized: bool) -> Result<()> {
        let store = self.store.clone();
        blocking(move || store.save_window_size(width, height, maximized)).await
    }

    async fn scan_keys(
        &self,
        id: ConnId,
        pattern: Bytes,
        count: u32,
        cursor: u64,
    ) -> Result<ScanPage<Bytes>> {
        let pool = self.manager.pool(id).await?;
        xredis_core::keys::scan_keys(&pool, &pattern, count, cursor).await
    }

    async fn key_details(&self, id: ConnId, key: Bytes) -> Result<KeyDetails> {
        let pool = self.manager.pool(id).await?;
        xredis_core::keys::key_details(&pool, &key).await
    }

    async fn delete_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<u64> {
        let pool = self.manager.pool(id).await?;
        xredis_core::keys::delete_keys(&pool, &keys).await
    }

    async fn db_size(&self, id: ConnId) -> Result<u64> {
        let pool = self.manager.pool(id).await?;
        xredis_core::keys::db_size(&pool).await
    }

    async fn copy_keys(&self, id: ConnId, keys: Vec<Bytes>) -> Result<CopyOutcome> {
        let pool = self.manager.pool(id).await?;
        xredis_core::transfer::copy_keys(&pool, &keys).await
    }

    async fn preview_paste(&self, id: ConnId, text: String) -> Result<PastePreview> {
        let pool = self.manager.pool(id).await?;
        xredis_core::transfer::preview(&pool, &text).await
    }

    async fn paste_keys(&self, id: ConnId, text: String, overwrite: bool) -> Result<PasteOutcome> {
        let pool = self.manager.pool(id).await?;
        xredis_core::transfer::paste(&pool, &text, overwrite).await
    }
}
