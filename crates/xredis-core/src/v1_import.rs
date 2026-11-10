//! 从 v1 的配置导入(只读 v1 的文件,绝不写)。
//!
//! v1 的 `config.json` 在 `dirs::data_local_dir()/xRedis/`。密码是 v1 用写死的密钥加密后
//! 放在配置里的,这里解密后存进 [`KeyStore`](crate::keystore::KeyStore)。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use magic_crypt::{MagicCryptTrait, new_magic_crypt};
use serde::Deserialize;

use crate::config::{ConfigStore, ConnId, ConnectionConfig, TlsConfig, now_ms};
use crate::consts::*;
use crate::error::{Error, Result};

/// v1 的配置文件路径。
pub fn v1_config_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("xRedis").join("config.json"))
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub connections: usize,
    /// 要显示给用户的提示:解密失败跳过的密码、跳过的坏记录。
    pub warnings: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct V1File {
    settings: V1Settings,
    connections: Vec<V1Connection>,
}

#[derive(Deserialize)]
#[serde(default)]
struct V1Settings {
    agreement: String,
    sider_menu_collapsed: bool,
    show_key_details: bool,
    manual_scan_limit: u32,
    auto_scan_limit: u32,
    window_width: u32,
    window_height: u32,
    is_maximized: bool,
}

impl Default for V1Settings {
    fn default() -> Self {
        Self {
            agreement: String::new(),
            sider_menu_collapsed: false,
            show_key_details: true,
            manual_scan_limit: SCAN_BATCH_DEFAULT,
            auto_scan_limit: AUTO_SCAN_LIMIT_DEFAULT,
            window_width: WINDOW_DEFAULT_WIDTH,
            window_height: WINDOW_DEFAULT_HEIGHT,
            is_maximized: false,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct V1Connection {
    id: String,
    host: String,
    name: Option<String>,
    port: u32,
    ordinal: i32,
    selected_db: Option<u32>,
    username: Option<String>,
    /// v1 的密文(base64)。
    password: Option<String>,
    tls: Option<bool>,
    ca_cert: Option<String>,
    client_cert: Option<String>,
    client_key: Option<String>,
    updated_on_utc: i64,
    created_on_utc: i64,
}

/// v1 写死的密钥(见 v1 `src-tauri/src/handlers/settings.rs`),只用来解 v1 留下的密码。
fn v1_decrypt(encrypted: &str) -> Option<String> {
    let crypt = new_magic_crypt!(b"BD_su:Sm4H}Qg?7vGr#&e3)w5*Wf2/6J", 256);
    crypt.decrypt_base64_to_string(encrypted).ok()
}

fn non_empty(s: Option<String>) -> Option<PathBuf> {
    s.filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// v2 配置是第一次创建、且还没导入过时,读 `v1_path` 导入。返回 `None` 表示没做(不是首次、
/// 已导入过、v1 文件不存在)。v1 文件解析失败返回 `Err`,此时不改 v2 的任何东西。
pub fn import_v1_if_first_run(store: &ConfigStore, v1_path: &Path) -> Result<Option<ImportReport>> {
    if !store.was_first_run() || store.imported_from_v1().is_some() {
        return Ok(None);
    }
    let bytes = match fs::read(v1_path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                action: "读取 v1 配置",
                path: v1_path.to_path_buf(),
                source,
            });
        }
    };
    let v1: V1File = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Import(format!("{} 解析失败: {e}", v1_path.display())))?;

    let mut report = ImportReport::default();
    let mut connections = Vec::new();
    let mut passwords: Vec<(ConnId, String)> = Vec::new();
    for c in v1.connections {
        let label = c
            .name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or(c.host.clone());
        let id: ConnId = match c.id.parse() {
            Ok(id) => id,
            Err(_) => {
                report
                    .warnings
                    .push(format!("连接 {label} 的 id 不合法,已跳过"));
                continue;
            }
        };
        let port = match u16::try_from(c.port) {
            Ok(p) if p != 0 => p,
            _ => {
                report
                    .warnings
                    .push(format!("连接 {label} 的端口 {} 不合法,已跳过", c.port));
                continue;
            }
        };
        if let Some(enc) = c.password.as_deref().filter(|p| !p.is_empty()) {
            match v1_decrypt(enc) {
                Some(plain) if !plain.is_empty() => passwords.push((id, plain)),
                Some(_) => {}
                None => report
                    .warnings
                    .push(format!("连接 {label} 的密码解密失败,已跳过,请重新输入")),
            }
        }
        let tls = (c.tls == Some(true)).then(|| TlsConfig {
            ca_cert: non_empty(c.ca_cert),
            client_cert: non_empty(c.client_cert),
            client_key: non_empty(c.client_key),
        });
        connections.push(ConnectionConfig {
            id,
            name: c.name.unwrap_or_default(),
            host: c.host,
            port,
            ordinal: c.ordinal.clamp(ORDINAL_MIN, ORDINAL_MAX),
            selected_db: c.selected_db.unwrap_or(0),
            username: c.username.filter(|u| !u.is_empty()),
            tls,
            created_on_utc: c.created_on_utc,
            updated_on_utc: c.updated_on_utc,
        });
    }
    report.connections = connections.len();

    let s = v1.settings;
    store.mutate(|cfg| {
        cfg.settings.agreement = s.agreement;
        cfg.settings.sider_menu_collapsed = s.sider_menu_collapsed;
        cfg.settings.show_key_details = s.show_key_details;
        cfg.settings.scan_batch = s.manual_scan_limit.clamp(SCAN_BATCH_MIN, SCAN_BATCH_MAX);
        cfg.settings.auto_scan_limit = s.auto_scan_limit.min(AUTO_SCAN_LIMIT_MAX);
        cfg.settings.window_width = s.window_width;
        cfg.settings.window_height = s.window_height;
        cfg.settings.is_maximized = s.is_maximized;
        cfg.connections = connections;
        cfg.imported_from_v1 = Some(now_ms());
        Ok(())
    })?;

    for (id, plain) in passwords {
        if let Err(err) = store.secrets().set(&id.to_string(), &plain) {
            report
                .warnings
                .push(format!("连接 {id} 的密码存入密钥存储失败: {err}"));
        }
    }
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{KeyStore, MemoryStore};
    use std::sync::Arc;

    /// 样本里的密码用 v1 的密钥和算法现算,不是从真实配置里拷的。
    fn enc(plain: &str) -> String {
        let crypt = new_magic_crypt!(b"BD_su:Sm4H}Qg?7vGr#&e3)w5*Wf2/6J", 256);
        crypt.encrypt_str_to_base64(plain)
    }

    fn sample(id1: ConnId, id2: ConnId) -> String {
        serde_json::json!({
            "settings": {
                "id": "default", "agreement": "accepted-1", "sider_menu_collapsed": true,
                "show_key_details": false, "page_size": 50, "manual_scan_limit": 300,
                "auto_scan_limit": 0, "window_width": 1280, "window_height": 800,
                "is_maximized": true
            },
            "connections": [
                {"id": id1.to_string(), "host": "10.0.0.1", "name": "prod", "port": 6380,
                 "ordinal": 2, "selected_db": 5, "username": "alice", "password": enc("p@ss:w/ord 中文"),
                 "tls": true, "ca_cert": "C:\\certs\\ca.pem", "client_cert": null, "client_key": "",
                 "updated_on_utc": 2000, "created_on_utc": 1000},
                {"id": id2.to_string(), "host": "localhost", "name": null, "port": 6379,
                 "ordinal": 1, "selected_db": null, "username": null, "password": null,
                 "tls": false, "ca_cert": "ignored.pem", "client_cert": null, "client_key": null,
                 "updated_on_utc": 3000, "created_on_utc": 3000}
            ]
        })
        .to_string()
    }

    fn open(dir: &Path) -> (ConfigStore, Arc<MemoryStore>) {
        let secrets = Arc::new(MemoryStore::new());
        let (store, _) = ConfigStore::open(dir, secrets.clone()).unwrap();
        (store, secrets)
    }

    #[test]
    fn imports_connections_settings_and_passwords_without_touching_v1_file() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1").join("config.json");
        fs::create_dir_all(v1.parent().unwrap()).unwrap();
        let (id1, id2) = (ConnId::new(), ConnId::new());
        fs::write(&v1, sample(id1, id2)).unwrap();
        let before = fs::read(&v1).unwrap();

        let (store, secrets) = open(&tmp.path().join("v2"));
        let report = import_v1_if_first_run(&store, &v1).unwrap().unwrap();
        assert_eq!(report.connections, 2);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        let c1 = store.connection(id1).unwrap();
        assert_eq!(
            (c1.host.as_str(), c1.port, c1.ordinal, c1.selected_db),
            ("10.0.0.1", 6380, 2, 5)
        );
        assert_eq!(c1.username.as_deref(), Some("alice"));
        assert_eq!((c1.created_on_utc, c1.updated_on_utc), (1000, 2000));
        let tls = c1.tls.unwrap();
        assert_eq!(tls.ca_cert, Some(PathBuf::from("C:\\certs\\ca.pem")));
        assert_eq!((tls.client_cert, tls.client_key), (None, None));
        let c2 = store.connection(id2).unwrap();
        assert!(c2.tls.is_none(), "tls=false 时丢掉证书路径");
        assert_eq!(c2.selected_db, 0);

        assert_eq!(
            secrets.get(&id1.to_string()).unwrap().as_deref(),
            Some("p@ss:w/ord 中文")
        );
        assert_eq!(secrets.get(&id2.to_string()).unwrap(), None);

        let s = store.settings();
        assert_eq!(s.agreement, "accepted-1");
        assert!(s.sider_menu_collapsed && !s.show_key_details && s.is_maximized);
        assert_eq!((s.scan_batch, s.auto_scan_limit), (300, 0));
        assert_eq!((s.window_width, s.window_height), (1280, 800));
        assert!(store.imported_from_v1().is_some());

        // v2 的配置文件里没有密码
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("p@ss"));
        // v1 文件原样没动
        assert_eq!(fs::read(&v1).unwrap(), before);
    }

    #[test]
    fn undecryptable_password_is_skipped_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1.json");
        let id = ConnId::new();
        fs::write(
            &v1,
            serde_json::json!({"connections": [
                {"id": id.to_string(), "host": "h", "name": "bad", "port": 6379, "password": "not-base64-!!"}
            ]})
            .to_string(),
        )
        .unwrap();
        let (store, secrets) = open(&tmp.path().join("v2"));
        let report = import_v1_if_first_run(&store, &v1).unwrap().unwrap();
        assert_eq!(report.connections, 1);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("bad"));
        assert_eq!(secrets.get(&id.to_string()).unwrap(), None);
    }

    #[test]
    fn imports_only_once() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1.json");
        fs::write(&v1, sample(ConnId::new(), ConnId::new())).unwrap();
        let dir = tmp.path().join("v2");
        let (store, _) = open(&dir);
        assert!(import_v1_if_first_run(&store, &v1).unwrap().is_some());
        // 同一个 store 再导一次、重新打开再导一次,都不再导入
        assert!(import_v1_if_first_run(&store, &v1).unwrap().is_none());
        let (store2, _) = open(&dir);
        assert!(import_v1_if_first_run(&store2, &v1).unwrap().is_none());
        assert_eq!(store2.connections().len(), 2);
        // 用户删光连接后也不会被重新导入
        for c in store2.connections() {
            store2.delete_connection(c.id).unwrap();
        }
        let (store3, _) = open(&dir);
        assert!(import_v1_if_first_run(&store3, &v1).unwrap().is_none());
        assert!(store3.connections().is_empty());
    }

    #[test]
    fn no_v1_file_is_not_an_error_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = open(&tmp.path().join("v2"));
        let none = import_v1_if_first_run(&store, &tmp.path().join("missing.json")).unwrap();
        assert!(none.is_none());
        assert!(!store.path().exists());
    }

    #[test]
    fn existing_v2_config_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1.json");
        fs::write(&v1, sample(ConnId::new(), ConnId::new())).unwrap();
        let dir = tmp.path().join("v2");
        let (store, _) = open(&dir);
        store.save_window_size(1000, 900, false).unwrap(); // v2 配置已存在
        let (store2, _) = open(&dir);
        assert!(import_v1_if_first_run(&store2, &v1).unwrap().is_none());
        assert!(store2.connections().is_empty());
    }

    #[test]
    fn corrupt_v1_file_is_error_and_leaves_v2_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1.json");
        fs::write(&v1, b"{ nope").unwrap();
        let (store, _) = open(&tmp.path().join("v2"));
        assert!(matches!(
            import_v1_if_first_run(&store, &v1),
            Err(Error::Import(_))
        ));
        assert!(!store.path().exists());
        assert!(store.imported_from_v1().is_none());
    }

    #[test]
    fn bad_records_are_skipped_with_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let v1 = tmp.path().join("v1.json");
        let good = ConnId::new();
        fs::write(
            &v1,
            serde_json::json!({"connections": [
                {"id": "not-a-ulid", "host": "a", "port": 6379},
                {"id": ConnId::new().to_string(), "host": "b", "port": 70000},
                {"id": good.to_string(), "host": "c", "port": 6379}
            ]})
            .to_string(),
        )
        .unwrap();
        let (store, _) = open(&tmp.path().join("v2"));
        let report = import_v1_if_first_run(&store, &v1).unwrap().unwrap();
        assert_eq!(report.connections, 1);
        assert_eq!(report.warnings.len(), 2);
        assert_eq!(store.connections()[0].id, good);
    }
}
