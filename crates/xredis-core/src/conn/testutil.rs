//! 需要 Redis 的测试的公共设施。只连 `scripts/test-redis.sh` 起的容器(`XREDIS_TEST_REDIS_URL`)。
//!
//! 红线:测试绝不执行 FLUSHDB、FLUSHALL、CONFIG SET、SHUTDOWN;每个测试用唯一前缀
//! `xredis_test:<ULID>:`,清理时只删自己的前缀;配置写临时目录,密码用内存版 KeyStore。

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::RwLock;
use ulid::Ulid;

use super::ConnManager;
use crate::config::{ConfigStore, ConnId, ConnectionDraft, PasswordChange};
use crate::keystore::MemoryStore;

/// 测试容器名。
pub(crate) const TEST_CONTAINER: &str = "xredis-test-redis";

/// 会重启容器的测试拿写锁,其他需要 Redis 的测试拿读锁,避免重启时把别的测试的连接搞断。
pub(crate) static CONTAINER_LOCK: RwLock<()> = RwLock::const_new(());

pub(crate) fn unique_prefix() -> String {
    format!("xredis_test:{}:", Ulid::generate())
}

/// 把测试挂在 `secs` 秒的超时下,超时就失败,不静默挂住。
pub(crate) async fn with_timeout<T>(secs: u64, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), fut)
        .await
        .unwrap_or_else(|_| panic!("test timed out after {secs}s"))
}

pub(crate) struct Creds {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// 解析 `redis://[user[:password]@]host:port`。
pub(crate) fn test_redis_creds() -> Creds {
    let url = std::env::var("XREDIS_TEST_REDIS_URL").expect(
        "XREDIS_TEST_REDIS_URL is not set; run these tests with scripts/test-redis.sh test",
    );
    parse_url(&url)
}

fn parse_url(url: &str) -> Creds {
    let rest = url.strip_prefix("redis://").expect("redis:// url");
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let (host, port) = hostport.rsplit_once(':').expect("host:port");
    let decode = |s: &str| urlencoding::decode(s).unwrap().into_owned();
    let (username, password) = match auth {
        Some(a) => match a.split_once(':') {
            Some((u, p)) => (Some(decode(u)), Some(decode(p))),
            None => (Some(decode(a)), None),
        },
        None => (None, None),
    };
    Creds {
        host: host.to_string(),
        port: port.parse().unwrap(),
        username: username.filter(|u| !u.is_empty()),
        password,
    }
}

pub(crate) struct TestEnv {
    pub manager: Arc<ConnManager>,
    pub store: Arc<ConfigStore>,
    pub id: ConnId,
    _dir: TempDir,
}

impl TestEnv {
    /// 连测试容器(默认凭据)。
    pub(crate) fn new() -> Self {
        let c = test_redis_creds();
        Self::with(
            &c.host,
            c.port,
            c.username.as_deref(),
            c.password.as_deref(),
        )
    }

    /// 同一个容器,换凭据(认证失败、ACL 用户的测试用)。
    pub(crate) fn with_creds(username: Option<&str>, password: Option<&str>) -> Self {
        let c = test_redis_creds();
        Self::with(&c.host, c.port, username, password)
    }

    /// 任意地址(不需要 Redis 的测试用)。
    pub(crate) fn with(
        host: &str,
        port: u16,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = ConfigStore::open(dir.path(), Arc::new(MemoryStore::new())).unwrap();
        let store = Arc::new(store);
        let id = store
            .save_connection(ConnectionDraft {
                id: None,
                name: "test".into(),
                host: host.into(),
                port,
                ordinal: 0,
                username: username.map(str::to_string),
                tls: None,
                password: match password {
                    Some(p) => PasswordChange::Set(p.into()),
                    None => PasswordChange::Keep,
                },
            })
            .unwrap()
            .id;
        Self {
            manager: Arc::new(ConnManager::new(store.clone())),
            store,
            id,
            _dir: dir,
        }
    }
}
