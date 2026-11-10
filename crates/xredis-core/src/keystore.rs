//! 密码存取。配置文件里不存密码,密码按连接 id 放在 `KeyStore` 里。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::consts::KEYRING_SERVICE;
use crate::error::{Error, Result};

pub trait KeyStore: Send + Sync {
    /// 取密码;不存在返回 `Ok(None)`。
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, password: &str) -> Result<()>;
    /// 删除;本来就不存在不算错。
    fn delete(&self, account: &str) -> Result<()>;
}

/// 内存版:测试用,也是钥匙串不可用时的降级(只活到程序退出)。
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.map.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, password: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(account.to_string(), password.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.map.lock().unwrap().remove(account);
        Ok(())
    }
}

/// 系统钥匙串(Windows 凭据管理器、macOS Keychain、Linux Secret Service)。
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self::with_service(KEYRING_SERVICE)
    }

    /// 换 service 名,供手动验证时避免碰真实数据。
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, account).map_err(keyring_err)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

fn keyring_err(err: keyring::Error) -> Error {
    Error::KeyStore(err.to_string())
}

impl KeyStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match self.entry(account)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keyring_err(err)),
        }
    }

    fn set(&self, account: &str, password: &str) -> Result<()> {
        self.entry(account)?
            .set_password(password)
            .map_err(keyring_err)
    }

    fn delete(&self, account: &str) -> Result<()> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(keyring_err(err)),
        }
    }
}

/// 主存储不可用时降级到内存。第一次降级时 `degraded()` 变 true,界面据此在消息栏提示一次。
pub struct FallbackStore {
    primary: Arc<dyn KeyStore>,
    memory: MemoryStore,
    degraded: AtomicBool,
}

impl FallbackStore {
    pub fn new(primary: Arc<dyn KeyStore>) -> Self {
        Self {
            primary,
            memory: MemoryStore::new(),
            degraded: AtomicBool::new(false),
        }
    }

    /// 是否已经降级到内存(密码不会持久化)。
    pub fn degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }
}

impl KeyStore for FallbackStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        if !self.degraded() {
            match self.primary.get(account) {
                Ok(v) => return Ok(v),
                Err(err) => {
                    tracing::warn!("钥匙串读取失败,降级到内存: {err}");
                    self.degraded.store(true, Ordering::Relaxed);
                }
            }
        }
        self.memory.get(account)
    }

    fn set(&self, account: &str, password: &str) -> Result<()> {
        if !self.degraded() {
            match self.primary.set(account, password) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!("钥匙串写入失败,降级到内存: {err}");
                    self.degraded.store(true, Ordering::Relaxed);
                }
            }
        }
        self.memory.set(account, password)
    }

    fn delete(&self, account: &str) -> Result<()> {
        if !self.degraded() {
            match self.primary.delete(account) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    tracing::warn!("钥匙串删除失败,降级到内存: {err}");
                    self.degraded.store(true, Ordering::Relaxed);
                }
            }
        }
        self.memory.delete(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken;
    impl KeyStore for Broken {
        fn get(&self, _: &str) -> Result<Option<String>> {
            Err(Error::KeyStore("no service".into()))
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Err(Error::KeyStore("no service".into()))
        }
        fn delete(&self, _: &str) -> Result<()> {
            Err(Error::KeyStore("no service".into()))
        }
    }

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::new();
        assert_eq!(s.get("a").unwrap(), None);
        s.set("a", "pw").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("pw"));
        s.delete("a").unwrap();
        s.delete("a").unwrap(); // 不存在不算错
        assert_eq!(s.get("a").unwrap(), None);
    }

    #[test]
    fn fallback_degrades_to_memory_and_flags_once() {
        let s = FallbackStore::new(Arc::new(Broken));
        assert!(!s.degraded());
        s.set("a", "pw").unwrap();
        assert!(s.degraded());
        assert_eq!(s.get("a").unwrap().as_deref(), Some("pw"));
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
    }

    /// 手动验证系统钥匙串:`cargo test -p xredis-core keyring_manual -- --ignored`。
    /// 用独立的 service 名和随机 account,验证完自己删除,不碰真实的 "xRedis" 条目。
    #[test]
    #[ignore = "touches the real OS keyring; run manually"]
    fn keyring_manual_roundtrip() {
        let s = KeyringStore::with_service("xRedis-test");
        let account = format!("xredis_test:{}", ulid::Ulid::generate());
        assert_eq!(s.get(&account).unwrap(), None);
        s.set(&account, "p@ss:w/ord 中文").unwrap();
        assert_eq!(s.get(&account).unwrap().as_deref(), Some("p@ss:w/ord 中文"));
        s.delete(&account).unwrap();
        assert_eq!(s.get(&account).unwrap(), None);
    }
}
