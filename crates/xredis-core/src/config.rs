//! 配置存储:设置和连接列表,JSON 整份原子写。
//!
//! 写法照 v1(L-17):内存全量缓存;每次写整份落盘,先写 `.tmp` 再替换;落盘失败回滚内存;
//! 解析失败的文件改名为 `.broken` 留存,用默认配置继续运行。**密码不进配置文件**,
//! 在 [`KeyStore`] 里按连接 id 存取。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::consts::*;
use crate::error::{Error, Result};
use crate::keystore::KeyStore;

const CONFIG_FILE_NAME: &str = "config.json";

/// v2 的配置目录:`dirs::data_local_dir()/xRedis/v2`。不和 v1 共用文件。
pub fn default_config_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("xRedis").join("v2"))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConnId(Ulid);

impl ConnId {
    pub fn new() -> Self {
        Self(Ulid::generate())
    }
}

impl Default for ConnId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ConnId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ulid::from_string(s)
            .map(Self)
            .map_err(|e| Error::Rule(format!("Invalid connection id '{s}': {e}")))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsConfig {
    pub ca_cert: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
}

/// 连接配置,不含密码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: ConnId,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub ordinal: i32,
    #[serde(default)]
    pub selected_db: u32,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub created_on_utc: i64,
    #[serde(default)]
    pub updated_on_utc: i64,
}

fn default_port() -> u16 {
    6379
}

/// 对应 v1 的“留空表示不改、传空串表示清空”。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PasswordChange {
    #[default]
    Keep,
    Clear,
    /// 空串等同 `Clear`。
    Set(String),
}

/// 保存连接时的输入:`id` 为 `None` 是新建。编辑时 `selected_db` 和创建时间保持不变。
#[derive(Debug, Clone)]
pub struct ConnectionDraft {
    pub id: Option<ConnId>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub ordinal: i32,
    pub username: Option<String>,
    pub tls: Option<TlsConfig>,
    pub password: PasswordChange,
}

/// [`ConfigStore::save_connection`] 的结果:连接已保存,`warnings` 是要显示给用户的提示(比如密码没存上)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub id: ConnId,
    /// 英文提示,给日志用。
    pub warnings: Vec<String>,
    /// 密码没存上时密钥存储的原始报错(界面按它翻译提示,不用上面的英文句子)。
    pub password_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 同意免责协议的记录,空串表示还没同意。
    pub agreement: String,
    pub sider_menu_collapsed: bool,
    pub show_key_details: bool,
    /// 单批扫描数量(v1 的 manual_scan_limit)。
    pub scan_batch: u32,
    /// 自动续扫上限,0 表示关闭。
    pub auto_scan_limit: u32,
    pub window_width: u32,
    pub window_height: u32,
    pub is_maximized: bool,
    /// 界面语言(语言包的名字,如 `en`、`zh-CN`),默认英文。
    pub language: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            agreement: String::new(),
            sider_menu_collapsed: false,
            show_key_details: true,
            scan_batch: SCAN_BATCH_DEFAULT,
            auto_scan_limit: AUTO_SCAN_LIMIT_DEFAULT,
            window_width: WINDOW_DEFAULT_WIDTH,
            window_height: WINDOW_DEFAULT_HEIGHT,
            is_maximized: false,
            language: DEFAULT_LANGUAGE.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub settings: Settings,
    pub connections: Vec<ConnectionConfig>,
    /// 从 v1 导入的时间(毫秒),有值就不再导入。
    pub imported_from_v1: Option<i64>,
}

pub struct ConfigStore {
    path: PathBuf,
    secrets: Arc<dyn KeyStore>,
    state: Mutex<ConfigFile>,
    first_run: bool,
}

impl ConfigStore {
    /// 打开(必要时创建目录)。返回的警告字符串要显示给用户,比如坏文件已改名留存。
    /// 目录建不出来(比如路径被文件占用)返回 `Err`,调用方自己决定怎么继续。
    pub fn open(dir: &Path, secrets: Arc<dyn KeyStore>) -> Result<(Self, Vec<String>)> {
        fs::create_dir_all(dir).map_err(|source| Error::Io {
            action: "create config directory",
            path: dir.to_path_buf(),
            source,
        })?;
        let path = dir.join(CONFIG_FILE_NAME);
        let mut warnings = Vec::new();
        let (config, first_run) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ConfigFile>(&bytes) {
                Ok(config) => (config, false),
                Err(err) => {
                    let detail = match backup_broken_file(&path) {
                        Some(to) => format!("it was saved as {}", to.display()),
                        None => "backing it up failed".to_string(),
                    };
                    warnings.push(format!(
                        "Config file {} could not be parsed ({err}); {detail}. Continuing with default settings.",
                        path.display()
                    ));
                    (ConfigFile::default(), false)
                }
            },
            // 首次运行:文件还不存在,不算错;第一次写入时才创建文件
            Err(err) if err.kind() == io::ErrorKind::NotFound => (ConfigFile::default(), true),
            Err(source) => {
                return Err(Error::Io {
                    action: "read config file",
                    path,
                    source,
                });
            }
        };
        let store = Self {
            path,
            secrets,
            state: Mutex::new(config),
            first_run,
        };
        Ok((store, warnings))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 打开时配置文件不存在(用于决定是否导入 v1)。
    pub fn was_first_run(&self) -> bool {
        self.first_run
    }

    pub fn secrets(&self) -> &Arc<dyn KeyStore> {
        &self.secrets
    }

    pub fn imported_from_v1(&self) -> Option<i64> {
        self.state.lock().unwrap().imported_from_v1
    }

    /// 改内存、整份落盘;落盘失败回滚内存。持锁期间串行,并发写不丢(L-17)。
    pub(crate) fn mutate<T>(&self, f: impl FnOnce(&mut ConfigFile) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().unwrap();
        let previous = guard.clone();
        let out = match f(&mut guard) {
            Ok(out) => out,
            Err(err) => {
                *guard = previous;
                return Err(err);
            }
        };
        if let Err(err) = persist(&guard, &self.path) {
            *guard = previous;
            return Err(err);
        }
        Ok(out)
    }

    // ---- 设置 ----

    pub fn settings(&self) -> Settings {
        self.state.lock().unwrap().settings.clone()
    }

    /// 保存设置面板能改的字段;窗口尺寸由 [`save_window_size`](Self::save_window_size) 负责,
    /// 设置面板一保存不会把窗口尺寸打回旧值。
    pub fn save_settings(&self, new: &Settings) -> Result<()> {
        if !(SCAN_BATCH_MIN..=SCAN_BATCH_MAX).contains(&new.scan_batch) {
            return Err(Error::Rule(format!(
                "Batch size must be between {SCAN_BATCH_MIN} and {SCAN_BATCH_MAX}"
            )));
        }
        if new.auto_scan_limit > AUTO_SCAN_LIMIT_MAX {
            return Err(Error::Rule(format!(
                "Auto-scan limit cannot exceed {AUTO_SCAN_LIMIT_MAX}"
            )));
        }
        self.mutate(|c| {
            let s = &mut c.settings;
            s.agreement = new.agreement.clone();
            s.sider_menu_collapsed = new.sider_menu_collapsed;
            s.show_key_details = new.show_key_details;
            s.scan_batch = new.scan_batch;
            s.auto_scan_limit = new.auto_scan_limit;
            s.language = if new.language.trim().is_empty() {
                DEFAULT_LANGUAGE.to_string()
            } else {
                new.language.trim().to_string()
            };
            Ok(())
        })
    }

    /// 无条件写入(v1 用数据库时,首次启动同意协议之前调整的窗口大小存不下来)。
    pub fn save_window_size(&self, width: u32, height: u32, is_maximized: bool) -> Result<()> {
        self.mutate(|c| {
            c.settings.window_width = width;
            c.settings.window_height = height;
            c.settings.is_maximized = is_maximized;
            Ok(())
        })
    }

    // ---- 连接 ----

    /// 按 ordinal 升序,同序号时更新时间新的在前(P-03)。
    pub fn connections(&self) -> Vec<ConnectionConfig> {
        let mut list = self.state.lock().unwrap().connections.clone();
        list.sort_by(|a, b| {
            a.ordinal
                .cmp(&b.ordinal)
                .then(b.updated_on_utc.cmp(&a.updated_on_utc))
        });
        list
    }

    pub fn connection(&self, id: ConnId) -> Result<ConnectionConfig> {
        self.state
            .lock()
            .unwrap()
            .connections
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("connection {id}")))
    }

    /// 连接密码(来自密钥存储);没设过返回 `None`。
    pub fn password(&self, id: ConnId) -> Result<Option<String>> {
        self.secrets.get(&id.to_string())
    }

    /// 新建或编辑连接。先落配置,配置落盘成功后才动密码,配置失败时密码不变。
    pub fn save_connection(&self, draft: ConnectionDraft) -> Result<SaveOutcome> {
        validate_draft(&draft)?;
        let now = now_ms();
        let username = draft.username.clone().filter(|u| !u.is_empty());
        let id = self.mutate(|c| match draft.id {
            Some(id) => {
                let slot = c
                    .connections
                    .iter_mut()
                    .find(|x| x.id == id)
                    .ok_or_else(|| Error::NotFound(format!("connection {id}")))?;
                slot.name = draft.name.clone();
                slot.host = draft.host.clone();
                slot.port = draft.port;
                slot.ordinal = draft.ordinal;
                slot.username = username.clone();
                slot.tls = draft.tls.clone();
                slot.updated_on_utc = now;
                Ok(id)
            }
            None => {
                let id = ConnId::new();
                c.connections.push(ConnectionConfig {
                    id,
                    name: draft.name.clone(),
                    host: draft.host.clone(),
                    port: draft.port,
                    ordinal: draft.ordinal,
                    selected_db: 0,
                    username: username.clone(),
                    tls: draft.tls.clone(),
                    created_on_utc: now,
                    updated_on_utc: now,
                });
                Ok(id)
            }
        })?;
        let account = id.to_string();
        let password_result = match &draft.password {
            PasswordChange::Keep => Ok(()),
            PasswordChange::Clear => self.secrets.delete(&account),
            PasswordChange::Set(p) if p.is_empty() => self.secrets.delete(&account),
            PasswordChange::Set(p) => self.secrets.set(&account, p),
        };
        // 连接已经落盘,密码没存上不能当成保存失败(界面会重试造成重复连接),作为警告带回
        let mut warnings = Vec::new();
        let mut password_error = None;
        if let Err(err) = password_result {
            password_error = Some(err.to_string());
            tracing::warn!("连接 {id} 已保存,但密码写入密钥存储失败: {err}");
            warnings.push(format!(
                "The connection was saved, but its password could not be stored: {err}"
            ));
        }
        Ok(SaveOutcome {
            id,
            warnings,
            password_error,
        })
    }

    /// 切库时先写配置(L-03:先写配置再清连接缓存)。
    pub fn set_selected_db(&self, id: ConnId, db: u32) -> Result<()> {
        self.mutate(|c| {
            let slot = c
                .connections
                .iter_mut()
                .find(|x| x.id == id)
                .ok_or_else(|| Error::NotFound(format!("connection {id}")))?;
            slot.selected_db = db;
            Ok(())
        })
    }

    pub fn delete_connection(&self, id: ConnId) -> Result<()> {
        self.mutate(|c| {
            let before = c.connections.len();
            c.connections.retain(|x| x.id != id);
            if c.connections.len() == before {
                return Err(Error::NotFound(format!("connection {id}")));
            }
            Ok(())
        })?;
        // 连接已经删了,密码删不掉只留个孤儿条目,不让整个删除失败
        if let Err(err) = self.secrets.delete(&id.to_string()) {
            tracing::warn!("删除连接 {id} 的密码失败: {err}");
        }
        Ok(())
    }
}

fn validate_draft(d: &ConnectionDraft) -> Result<()> {
    if d.host.trim().is_empty() {
        return Err(Error::Rule("Host is required".into()));
    }
    if d.port == 0 {
        return Err(Error::Rule("Port must be between 1 and 65535".into()));
    }
    if !(ORDINAL_MIN..=ORDINAL_MAX).contains(&d.ordinal) {
        return Err(Error::Rule(format!(
            "Ordinal must be between {ORDINAL_MIN} and {ORDINAL_MAX}"
        )));
    }
    Ok(())
}

/// 把无法解析的配置文件改名保留,返回备份路径。
fn backup_broken_file(path: &Path) -> Option<PathBuf> {
    let backup = path.with_extension("json.broken");
    // 目标已存在时 rename 在部分平台会失败,先删
    let _ = fs::remove_file(&backup);
    match fs::rename(path, &backup) {
        Ok(()) => Some(backup),
        Err(err) => {
            tracing::error!("备份损坏的配置文件失败: {err}");
            None
        }
    }
}

/// 原子写入:先写临时文件,再替换目标(`fs::rename` 在各平台都会覆盖已存在的目标)。
fn persist(config: &ConfigFile, path: &Path) -> Result<()> {
    let json = serde_json::to_vec_pretty(config)
        .map_err(|e| Error::Config(format!("Failed to serialize config: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    if let Err(source) = fs::write(&tmp, &json) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io {
            action: "write temporary file",
            path: tmp,
            source,
        });
    }
    if let Err(source) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Io {
            action: "replace config file",
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::MemoryStore;

    fn open(dir: &Path) -> ConfigStore {
        ConfigStore::open(dir, Arc::new(MemoryStore::new()))
            .unwrap()
            .0
    }

    fn draft(name: &str) -> ConnectionDraft {
        ConnectionDraft {
            id: None,
            name: name.into(),
            host: "127.0.0.1".into(),
            port: 6379,
            ordinal: 0,
            username: None,
            tls: None,
            password: PasswordChange::Keep,
        }
    }

    #[test]
    fn first_run_creates_no_file_until_write() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        assert!(store.was_first_run());
        assert!(store.connections().is_empty());
        assert_eq!(store.settings(), Settings::default());
        assert!(!tmp.path().join("config.json").exists());
        store.save_connection(draft("a")).unwrap();
        assert!(tmp.path().join("config.json").exists());
        assert!(!open(tmp.path()).was_first_run());
    }

    #[test]
    fn creates_nested_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a").join("b").join("c");
        let store = open(&dir);
        store.save_connection(draft("a")).unwrap();
        assert!(dir.join("config.json").exists());
    }

    #[test]
    fn dir_path_occupied_by_file_is_error_not_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let occupied = tmp.path().join("v2");
        fs::write(&occupied, b"x").unwrap();
        let err = ConfigStore::open(&occupied, Arc::new(MemoryStore::new()))
            .err()
            .expect("should fail");
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    #[test]
    fn broken_file_renamed_and_defaults_used() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("config.json"), b"{ not json").unwrap();
        let (store, warnings) =
            ConfigStore::open(tmp.path(), Arc::new(MemoryStore::new())).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains(".broken"));
        assert_eq!(
            fs::read(tmp.path().join("config.json.broken")).unwrap(),
            b"{ not json"
        );
        assert!(store.connections().is_empty());
        // 之后还能正常保存
        store.save_connection(draft("a")).unwrap();
        assert_eq!(open(tmp.path()).connections().len(), 1);
    }

    #[test]
    fn missing_fields_get_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let id = ConnId::new();
        fs::write(
            tmp.path().join("config.json"),
            format!(
                r#"{{"settings":{{"show_key_details":false}},"connections":[{{"id":"{id}","host":"h"}}]}}"#
            ),
        )
        .unwrap();
        let store = open(tmp.path());
        let s = store.settings();
        assert!(!s.show_key_details);
        assert_eq!(s.scan_batch, 500);
        assert_eq!(s.auto_scan_limit, 100_000);
        assert_eq!(s.window_width, 1600);
        let c = store.connection(id).unwrap();
        assert_eq!(c.port, 6379);
        assert_eq!(c.selected_db, 0);
        assert_eq!(c.host, "h");
    }

    #[test]
    fn no_tmp_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        store.save_connection(draft("a")).unwrap();
        store.save_window_size(1000, 900, true).unwrap();
        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["config.json".to_string()]);
    }

    /// L-17:落盘失败回滚内存,不会界面显示成功、重启后丢失。
    #[test]
    fn persist_failure_rolls_back_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        store.save_connection(draft("keep")).unwrap();
        // 让临时文件的位置被目录占用,写入必然失败
        fs::create_dir(tmp.path().join("config.json.tmp")).unwrap();
        assert!(store.save_connection(draft("lost")).is_err());
        assert!(store.save_window_size(1, 2, true).is_err());
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.settings().window_width, 1600);
        fs::remove_dir(tmp.path().join("config.json.tmp")).unwrap();
        store.save_connection(draft("again")).unwrap();
        assert_eq!(open(tmp.path()).connections().len(), 2);
    }

    /// L-17:并发写不丢数据。
    #[test]
    fn concurrent_saves_all_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(open(tmp.path()));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = store.clone();
                std::thread::spawn(move || s.save_connection(draft(&format!("c{i}"))).unwrap().id)
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.connections().len(), 8);
        assert_eq!(open(tmp.path()).connections().len(), 8);
    }

    #[test]
    fn password_never_in_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let secrets = Arc::new(MemoryStore::new());
        let (store, _) = ConfigStore::open(tmp.path(), secrets.clone()).unwrap();
        let mut d = draft("a");
        d.password = PasswordChange::Set("sup3r-s3cret-pw".into());
        let id = store.save_connection(d).unwrap().id;
        let text = fs::read_to_string(tmp.path().join("config.json")).unwrap();
        assert!(!text.contains("sup3r-s3cret-pw"));
        assert!(!text.to_lowercase().contains("password"));
        assert_eq!(
            secrets.get(&id.to_string()).unwrap().as_deref(),
            Some("sup3r-s3cret-pw")
        );
    }

    #[test]
    fn password_change_keep_clear_set() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let mut d = draft("a");
        d.password = PasswordChange::Set("pw1".into());
        let id = store.save_connection(d).unwrap().id;
        let edit = |change| ConnectionDraft {
            id: Some(id),
            password: change,
            ..draft("a")
        };
        // 留空不改
        store.save_connection(edit(PasswordChange::Keep)).unwrap();
        assert_eq!(store.password(id).unwrap().as_deref(), Some("pw1"));
        // 设新值
        store
            .save_connection(edit(PasswordChange::Set("pw2".into())))
            .unwrap();
        assert_eq!(store.password(id).unwrap().as_deref(), Some("pw2"));
        // 空串等同清空
        store
            .save_connection(edit(PasswordChange::Set(String::new())))
            .unwrap();
        assert_eq!(store.password(id).unwrap(), None);
        // 显式清空
        store
            .save_connection(edit(PasswordChange::Set("pw3".into())))
            .unwrap();
        store.save_connection(edit(PasswordChange::Clear)).unwrap();
        assert_eq!(store.password(id).unwrap(), None);
    }

    #[test]
    fn persist_failure_does_not_touch_password() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let mut d = draft("a");
        d.password = PasswordChange::Set("old".into());
        let id = store.save_connection(d).unwrap().id;
        fs::create_dir(tmp.path().join("config.json.tmp")).unwrap();
        let mut d2 = draft("a");
        d2.id = Some(id);
        d2.password = PasswordChange::Set("new".into());
        assert!(store.save_connection(d2).is_err());
        assert_eq!(store.password(id).unwrap().as_deref(), Some("old"));
    }

    /// 密码写失败时连接已落盘,不能返回 Err(界面会重试造成重复连接),要作为警告带回。
    #[test]
    fn password_write_failure_is_a_warning_not_an_error() {
        struct FailingStore;
        impl KeyStore for FailingStore {
            fn get(&self, _: &str) -> Result<Option<String>> {
                Ok(None)
            }
            fn set(&self, _: &str, _: &str) -> Result<()> {
                Err(Error::KeyStore("locked".into()))
            }
            fn delete(&self, _: &str) -> Result<()> {
                Err(Error::KeyStore("locked".into()))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = ConfigStore::open(tmp.path(), Arc::new(FailingStore)).unwrap();
        let mut d = draft("a");
        d.password = PasswordChange::Set("pw".into());
        let out = store.save_connection(d).unwrap();
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("locked"));
        assert!(out.password_error.as_deref().unwrap().contains("locked"));
        // 连接确实已保存,且只有一条
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.connection(out.id).unwrap().name, "a");
        // 编辑时清密码失败同样是警告
        let mut d2 = draft("a");
        d2.id = Some(out.id);
        d2.password = PasswordChange::Clear;
        let out2 = store.save_connection(d2).unwrap();
        assert_eq!(out2.warnings.len(), 1);
        assert_eq!(store.connections().len(), 1);
        // 没碰密码时没有警告
        let ok = store.save_connection(draft("b")).unwrap();
        assert!(ok.warnings.is_empty());
        assert!(ok.password_error.is_none());
    }

    #[test]
    fn delete_removes_connection_and_password() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let mut d = draft("a");
        d.password = PasswordChange::Set("pw".into());
        let id = store.save_connection(d).unwrap().id;
        store.delete_connection(id).unwrap();
        assert!(store.connections().is_empty());
        assert_eq!(store.password(id).unwrap(), None);
        assert!(matches!(
            store.delete_connection(id),
            Err(Error::NotFound(_))
        ));
    }

    /// P-03:ordinal 升序,同序号时更新时间新的在前。
    #[test]
    fn connections_sorted_by_ordinal_then_updated_desc() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let mk = |name: &str, ordinal: i32| ConnectionDraft {
            ordinal,
            ..draft(name)
        };
        let a = store.save_connection(mk("a", 5)).unwrap().id;
        let b = store.save_connection(mk("b", 1)).unwrap().id;
        let c = store.save_connection(mk("c", 5)).unwrap().id;
        // 让 a 的更新时间最新
        store
            .mutate(|cfg| {
                cfg.connections
                    .iter_mut()
                    .find(|x| x.id == a)
                    .unwrap()
                    .updated_on_utc += 10_000;
                Ok(())
            })
            .unwrap();
        let order: Vec<_> = store.connections().iter().map(|x| x.id).collect();
        assert_eq!(order, vec![b, a, c]);
    }

    #[test]
    fn edit_keeps_selected_db_and_created_time() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let id = store.save_connection(draft("a")).unwrap().id;
        store.set_selected_db(id, 3).unwrap();
        let created = store.connection(id).unwrap().created_on_utc;
        store
            .save_connection(ConnectionDraft {
                id: Some(id),
                port: 7000,
                ..draft("a2")
            })
            .unwrap();
        let c = store.connection(id).unwrap();
        assert_eq!(
            (c.selected_db, c.created_on_utc, c.port),
            (3, created, 7000)
        );
        assert_eq!(c.name, "a2");
        assert_eq!(open(tmp.path()).connection(id).unwrap().selected_db, 3);
    }

    #[test]
    fn validation_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        let bad = |f: fn(&mut ConnectionDraft)| {
            let mut d = draft("a");
            f(&mut d);
            assert!(matches!(store.save_connection(d), Err(Error::Rule(_))));
        };
        bad(|d| d.port = 0);
        bad(|d| d.host = "  ".into());
        bad(|d| d.ordinal = 65536);
        bad(|d| d.ordinal = -65536);
        assert!(store.connections().is_empty());
        let mut s = Settings {
            scan_batch: 99,
            ..Settings::default()
        };
        assert!(store.save_settings(&s).is_err());
        s.scan_batch = 2001;
        assert!(store.save_settings(&s).is_err());
        s.scan_batch = 100;
        s.auto_scan_limit = 100_001;
        assert!(store.save_settings(&s).is_err());
        s.auto_scan_limit = 0;
        store.save_settings(&s).unwrap();
    }

    /// 语言设置会保存;空串回落到默认英文;旧配置文件没有这个字段时是英文。
    #[test]
    fn language_setting_is_saved_and_defaults_to_english() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        assert_eq!(store.settings().language, "en");
        let mut s = Settings {
            language: "zh-CN".into(),
            ..Settings::default()
        };
        store.save_settings(&s).unwrap();
        assert_eq!(open(tmp.path()).settings().language, "zh-CN");
        s.language = "  ".into();
        store.save_settings(&s).unwrap();
        assert_eq!(store.settings().language, "en");
        // 旧文件没有 language 字段
        fs::write(
            tmp.path().join("config.json"),
            r#"{"settings":{"show_key_details":false}}"#,
        )
        .unwrap();
        assert_eq!(open(tmp.path()).settings().language, "en");
    }

    #[test]
    fn save_settings_keeps_window_size() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path());
        store.save_window_size(1111, 999, true).unwrap();
        // 窗口字段是默认值,保存设置不能把窗口尺寸打回去
        let s = Settings {
            show_key_details: false,
            ..Settings::default()
        };
        store.save_settings(&s).unwrap();
        let got = open(tmp.path()).settings();
        assert_eq!(
            (got.window_width, got.window_height, got.is_maximized),
            (1111, 999, true)
        );
        assert!(!got.show_key_details);
    }
}
