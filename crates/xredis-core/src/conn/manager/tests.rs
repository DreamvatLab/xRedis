//! ConnManager 的测试。不需要 Redis 的直接跑;需要 Redis 的标 ignore,
//! 用 `scripts/test-redis.sh test` 跑(只连测试容器 127.0.0.1:6391)。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fred::clients::{Client, Pool};
use fred::cmd;
use fred::interfaces::ClientLike;
use ulid::Ulid;

use super::*;
use crate::config::TlsConfig;
use crate::conn::testutil::*;

async fn ping(pool: &Pool) -> std::result::Result<String, fred::error::Error> {
    pool.custom(cmd!("PING"), Vec::<String>::new()).await
}

/// 该客户端当前所在的库(`CLIENT INFO` 里的 `db=N`)。
async fn client_db(client: &Client) -> u32 {
    let info: String = client.custom(cmd!("CLIENT"), vec!["INFO"]).await.unwrap();
    info.split_whitespace()
        .find_map(|kv| kv.strip_prefix("db="))
        .expect("db= in CLIENT INFO")
        .parse()
        .unwrap()
}

// ---------------------------------------------------------------------------
// 不需要 Redis
// ---------------------------------------------------------------------------

/// 只 accept、不回应的假服务端:客户端会一直停在「连接中」。
async fn silent_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    (port, handle)
}

/// L-02(P-04):建连进行中关闭立刻返回,建连方拿到 Closed,排队等锁的请求也立刻放弃。
#[tokio::test(flavor = "multi_thread")]
async fn l02_close_while_connecting_returns_immediately() {
    let (port, server) = silent_server().await;
    let env = TestEnv::with("127.0.0.1", port, None, None);
    let (m, id) = (env.manager.clone(), env.id);

    // 一个在建连(持锁),另外两个在锁上排队
    let tasks: Vec<_> = (0..3)
        .map(|_| {
            let m = m.clone();
            tokio::spawn(async move { m.pool(id).await.err() })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    with_timeout(5, m.close(id)).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "close took {:?}",
        started.elapsed()
    );
    for t in tasks {
        let err = with_timeout(5, t).await.unwrap();
        assert!(matches!(err, Some(Error::Closed)), "{err:?}");
    }
    assert_eq!(m.cached(id), (false, false, false));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn close_without_connecting_is_ok_and_idempotent() {
    let env = TestEnv::with("127.0.0.1", 1, None, None);
    with_timeout(5, env.manager.close(env.id)).await.unwrap();
    with_timeout(5, env.manager.close(env.id)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_connection_is_not_found() {
    let env = TestEnv::with("127.0.0.1", 1, None, None);
    let err = with_timeout(5, env.manager.pool(ConnId::new()))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}

/// 单机配置里的三层防护都落到了 Builder 上(重连策略、命令超时、建连超时、半开探测)。
#[test]
fn builder_sets_reconnect_timeouts_and_probe() {
    let env_dir = tempfile::tempdir().unwrap();
    let (store, _) = ConfigStore::open(
        env_dir.path(),
        Arc::new(crate::keystore::MemoryStore::new()),
    )
    .unwrap();
    let id = store
        .save_connection(crate::config::ConnectionDraft {
            id: None,
            name: "t".into(),
            host: "h".into(),
            port: 6379,
            ordinal: 0,
            username: None,
            tls: None,
            password: crate::config::PasswordChange::Keep,
        })
        .unwrap()
        .id;
    let cfg = store.connection(id).unwrap();
    let builder = build_builder(&cfg, Some("p")).unwrap();
    let policy = builder.get_policy().expect("reconnect policy");
    assert_eq!(
        format!("{policy:?}"),
        format!(
            "{:?}",
            ReconnectPolicy::new_exponential(
                0,
                RECONNECT_MIN_DELAY_MS,
                RECONNECT_MAX_DELAY_MS,
                RECONNECT_BACKOFF_BASE
            )
        )
    );
    assert_eq!(
        builder.get_performance_config().default_command_timeout,
        Duration::from_millis(COMMAND_TIMEOUT_MS)
    );
    let conn = builder.get_connection_config();
    assert_eq!(
        conn.connection_timeout,
        Duration::from_millis(CONNECT_TIMEOUT_MS)
    );
    assert_eq!(
        conn.unresponsive.max_timeout,
        Some(Duration::from_millis(UNRESPONSIVE_TIMEOUT_MS))
    );
    assert_eq!(
        conn.unresponsive.interval,
        Duration::from_millis(UNRESPONSIVE_INTERVAL_MS)
    );
}

// ---------------------------------------------------------------------------
// 需要 Redis
// ---------------------------------------------------------------------------

/// P-05:连接信息每个库一条,库数量来自 CONFIG GET databases(容器起在 --databases 20),
/// 有 key 的库显示 key 数。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn p05_connection_info_lists_every_db_with_key_counts() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let env = TestEnv::new();
        let pool = env.manager.pool(env.id).await.unwrap();
        let key = format!("{}k", unique_prefix());
        pool.custom::<i64, _>(cmd!("SET"), vec![key.as_str(), "v"])
            .await
            .ok();
        let info = env.manager.connection_info(env.id).await.unwrap();
        assert_eq!(info.databases.len(), 20);
        assert_eq!(info.databases[0].db, 0);
        assert!(info.databases[0].keys >= 1, "{:?}", info.databases[0]);
        assert_eq!(info.databases[19].db, 19);
        pool.custom::<i64, _>(cmd!("DEL"), vec![key.as_str()])
            .await
            .unwrap();
    })
    .await;
}

/// 认证失败返回 Auth 错误,不是别的。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn wrong_password_is_auth_error() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let env = TestEnv::with_creds(None, Some("definitely-wrong-password"));
        let err = env.manager.pool(env.id).await.expect_err("should fail");
        assert!(matches!(err, Error::Auth(_)), "{err:?}");
        // 连不上时没有残留的缓存
        assert_eq!(env.manager.cached(env.id), (false, false, false));
    })
    .await;
}

/// L-11:托管 Redis 禁用 CONFIG 时回落 16 个库。用 ACL 用户模拟(容器真实库数是 20,
/// 所以 16 一定来自回落)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l11_config_denied_falls_back_to_16_dbs() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let admin = TestEnv::new();
        let apool = admin.manager.pool(admin.id).await.unwrap();
        let user = format!("xredis_test_{}", Ulid::generate());
        let pw = "acl-test-pw";
        let setuser: String = apool
            .custom(
                cmd!("ACL"),
                vec![
                    "SETUSER",
                    user.as_str(),
                    "on",
                    ">acl-test-pw",
                    "~*",
                    "+@all",
                    "-config",
                ],
            )
            .await
            .unwrap();
        assert_eq!(setuser, "OK");

        let restricted = TestEnv::with_creds(Some(&user), Some(pw));
        let info = restricted.manager.connection_info(restricted.id).await;
        let _ = restricted.manager.close(restricted.id).await;
        // 先清理自己建的 ACL 用户,再断言
        let _: i64 = apool
            .custom(cmd!("ACL"), vec!["DELUSER", user.as_str()])
            .await
            .unwrap();
        let info = info.unwrap();
        assert_eq!(info.databases.len(), 16);
    })
    .await;
}

/// L-04:并发下每种客户端只建一个;订阅客户端的 on_created 只调用一次(L-05 的前提)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l04_concurrent_requests_build_one_client_of_each_kind() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let env = TestEnv::new();
        let created = Arc::new(AtomicUsize::new(0));
        let mut pools = Vec::new();
        let mut consoles = Vec::new();
        let mut subs = Vec::new();
        for _ in 0..20 {
            let m = env.manager.clone();
            let id = env.id;
            let created = created.clone();
            pools.push(tokio::spawn(async move { m.pool(id).await.unwrap() }));
            let m = env.manager.clone();
            consoles.push(tokio::spawn(
                async move { m.console_client(id).await.unwrap() },
            ));
            let m = env.manager.clone();
            subs.push(tokio::spawn(async move {
                m.subscriber_client(id, |_| {
                    created.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .unwrap()
            }));
        }
        let mut pool_ids = std::collections::HashSet::new();
        for p in pools {
            let p = p.await.unwrap();
            pool_ids.insert(p.clients().first().unwrap().id().to_string());
        }
        let mut console_ids = std::collections::HashSet::new();
        for c in consoles {
            console_ids.insert(c.await.unwrap().id().to_string());
        }
        let mut sub_ids = std::collections::HashSet::new();
        for s in subs {
            sub_ids.insert(s.await.unwrap().id().to_string());
        }
        assert_eq!(pool_ids.len(), 1, "多个连接池被建出来了");
        assert_eq!(console_ids.len(), 1, "多个控制台客户端被建出来了");
        assert_eq!(sub_ids.len(), 1, "多个订阅客户端被建出来了");
        assert_eq!(created.load(Ordering::SeqCst), 1, "on_created 被调用了多次");
        // 三种客户端互不相同
        assert!(pool_ids.is_disjoint(&console_ids) && console_ids.is_disjoint(&sub_ids));
        env.manager.close(env.id).await.unwrap();
    })
    .await;
}

/// L-06:退订只关订阅客户端,不动连接池和控制台客户端。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l06_closing_subscriber_keeps_pool_and_console() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let env = TestEnv::new();
        let m = &env.manager;
        let pool = m.pool(env.id).await.unwrap();
        let console = m.console_client(env.id).await.unwrap();
        m.subscriber_client(env.id, |_| {}).await.unwrap();
        assert_eq!(m.cached(env.id), (true, true, true));
        m.close_subscriber(env.id).await.unwrap();
        assert_eq!(m.cached(env.id), (true, false, true));
        assert!(ping(&pool).await.is_ok());
        let pong: String = console
            .custom(cmd!("PING"), Vec::<String>::new())
            .await
            .unwrap();
        assert_eq!(pong, "PONG");
        m.close(env.id).await.unwrap();
        assert_eq!(m.cached(env.id), (false, false, false));
    })
    .await;
}

/// L-03:切库期间的并发请求不能把连接池固定在旧库。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l03_select_db_with_concurrent_requests_ends_on_new_db() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(120, async {
        let env = TestEnv::new();
        env.manager.pool(env.id).await.unwrap();
        for round in 0..8u32 {
            let target = round % 3 + 1;
            let mut tasks = Vec::new();
            for _ in 0..30 {
                let m = env.manager.clone();
                let id = env.id;
                tasks.push(tokio::spawn(async move {
                    let _ = m.pool(id).await;
                }));
            }
            let m = env.manager.clone();
            let id = env.id;
            let switch = tokio::spawn(async move { m.select_db(id, target).await });
            switch.await.unwrap().unwrap();
            for t in tasks {
                t.await.unwrap();
            }
            // 切完之后取到的池,每个客户端都在新库
            let pool = env.manager.pool(env.id).await.unwrap();
            for c in pool.clients() {
                assert_eq!(client_db(c).await, target, "round {round}");
            }
            assert_eq!(env.store.connection(env.id).unwrap().selected_db, target);
        }
        env.manager.close(env.id).await.unwrap();
    })
    .await;
}

/// L-01:Redis 重启后能自动恢复,命令不会永久挂住。只重启测试容器。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l01_recovers_after_container_restart() {
    let _g = CONTAINER_LOCK.write().await;
    with_timeout(120, async {
        let env = TestEnv::new();
        let pool = env.manager.pool(env.id).await.unwrap();
        assert_eq!(ping(&pool).await.unwrap(), "PONG");

        let status = tokio::task::spawn_blocking(|| {
            std::process::Command::new("docker")
                .args(["restart", TEST_CONTAINER])
                .status()
        })
        .await
        .unwrap()
        .expect("run docker");
        assert!(status.success());

        // 每次尝试都必须在命令超时内返回(成功或失败),不能挂住;最终要恢复
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let attempt = tokio::time::timeout(Duration::from_secs(20), ping(&pool))
                .await
                .expect("a command hung instead of failing within the timeout");
            if attempt.is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "did not recover: {attempt:?}");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        env.manager.close(env.id).await.unwrap();
    })
    .await;
}

// ---------------------------------------------------------------------------
// R-03a:已连上之后服务器失联时关闭和切库;关闭或失败后不留还在重连的客户端
// ---------------------------------------------------------------------------

fn docker(args: &[&str]) -> bool {
    std::process::Command::new("docker")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 不管测试怎么结束,都把测试容器恢复成运行状态(只动测试容器)。
struct RestoreContainer;

impl Drop for RestoreContainer {
    fn drop(&mut self) {
        docker(&["unpause", TEST_CONTAINER]);
        docker(&["start", TEST_CONTAINER]);
    }
}

/// 用 `docker exec redis-cli` 读服务端的 `total_connections_received`。每次观察自己会新增
/// 恰好一个连接,所以两次观察之间如果有别的客户端连进来,差值就会大于 1。
/// 密码走环境变量,不出现在命令行上。
fn observe_total_connections() -> Option<u64> {
    let password = test_redis_creds().password.unwrap_or_default();
    let out = std::process::Command::new("docker")
        .args([
            "exec",
            "-e",
            "REDISCLI_AUTH",
            TEST_CONTAINER,
            "redis-cli",
            "INFO",
            "stats",
        ])
        .env("REDISCLI_AUTH", password)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("total_connections_received:"))
        .and_then(|v| v.parse().ok())
}

async fn observe() -> Option<u64> {
    tokio::task::spawn_blocking(observe_total_connections)
        .await
        .unwrap()
}

/// 等容器能响应,返回第一次观察到的计数。
async fn wait_observable(secs: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(n) = observe().await {
            return n;
        }
        assert!(
            Instant::now() < deadline,
            "test container did not come back"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 观察 `window` 内服务端有没有新连接进来(重连中的残留客户端会让计数增长)。
///
/// 容器刚恢复时,Docker 的端口代理会把失联期间客户端排在它那里的连接一次性转给 Redis
/// (对照实验:没有任何客户端时计数只涨观察者自己的 1;关闭后的客户端状态一直是 Disconnecting),
/// 所以先等这一阵过去再开始计数。真有残留客户端在重连的话,退避上限是 10 秒,
/// 在窗口(≥5 秒,失联场景 12 秒)内一定会再冒出来。
async fn assert_no_new_connections(window: Duration) {
    wait_observable(60).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let before = observe().await.expect("observe");
    tokio::time::sleep(window).await;
    let after = observe().await.expect("observe");
    assert_eq!(
        after,
        before + 1,
        "{} extra connection(s) arrived within {window:?}: a client is still reconnecting in the background",
        after - before - 1
    );
}

/// 关掉的客户端不能处在连接中或已连接状态(状态是直接证据,计数只是旁证)。
fn assert_clients_stopped(clients: &[Client]) {
    use fred::types::ClientState;
    for c in clients {
        assert!(
            matches!(
                c.state(),
                ClientState::Disconnecting | ClientState::Disconnected
            ),
            "client still {:?} after close",
            c.state()
        );
    }
}

async fn restore_and_wait() {
    docker(&["unpause", TEST_CONTAINER]);
    docker(&["start", TEST_CONTAINER]);
    wait_observable(60).await;
}

#[derive(Clone, Copy, Debug)]
enum Outage {
    /// `docker pause`:连接还在,但对端不响应(半开)。
    Paused,
    /// `docker stop`:连接被断开,连不上。
    Stopped,
}

impl Outage {
    fn start(self) {
        let ok = match self {
            Outage::Paused => docker(&["pause", TEST_CONTAINER]),
            Outage::Stopped => docker(&["stop", TEST_CONTAINER]),
        };
        assert!(ok, "could not simulate outage {self:?}");
    }
    /// 失联后观察残留重连要看得更久:退避上限是 10 秒。
    fn window(self) -> Duration {
        match self {
            Outage::Paused => Duration::from_secs(5),
            Outage::Stopped => Duration::from_secs(12),
        }
    }
}

async fn close_when_server_is_gone(outage: Outage) {
    let _g = CONTAINER_LOCK.write().await;
    let _restore = RestoreContainer;
    with_timeout(180, async {
        let env = TestEnv::new();
        let pool = env.manager.pool(env.id).await.unwrap();
        assert_eq!(ping(&pool).await.unwrap(), "PONG");
        outage.start();

        let started = Instant::now();
        let _ = env.manager.close(env.id).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "close took {:?} with the server {outage:?}",
            started.elapsed()
        );
        assert_eq!(env.manager.cached(env.id), (false, false, false));

        restore_and_wait().await;
        assert_no_new_connections(outage.window()).await;
        assert_clients_stopped(pool.clients());
    })
    .await;
}

async fn select_db_when_server_is_gone(outage: Outage) {
    let _g = CONTAINER_LOCK.write().await;
    let _restore = RestoreContainer;
    with_timeout(180, async {
        let env = TestEnv::new();
        let pool = env.manager.pool(env.id).await.unwrap();
        let console = env.manager.console_client(env.id).await.unwrap();
        outage.start();

        let started = Instant::now();
        let result = env.manager.select_db(env.id, 2).await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "select_db took {:?} with the server {outage:?}",
            started.elapsed()
        );
        // 配置要写上,缓存要清掉,不管关客户端有没有出错
        let _ = result;
        assert_eq!(env.store.connection(env.id).unwrap().selected_db, 2);
        assert_eq!(env.manager.cached(env.id), (false, false, false));

        restore_and_wait().await;
        assert_no_new_connections(outage.window()).await;
        assert_clients_stopped(pool.clients());
        assert_clients_stopped(std::slice::from_ref(&console));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_close_returns_quickly_and_leaves_nothing_when_server_paused() {
    close_when_server_is_gone(Outage::Paused).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_close_returns_quickly_and_leaves_nothing_when_server_stopped() {
    close_when_server_is_gone(Outage::Stopped).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_select_db_returns_quickly_and_leaves_nothing_when_server_paused() {
    select_db_when_server_is_gone(Outage::Paused).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_select_db_returns_quickly_and_leaves_nothing_when_server_stopped() {
    select_db_when_server_is_gone(Outage::Stopped).await;
}

/// 基线:服务器正常时关闭后也不留下重连的客户端。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_normal_close_leaves_no_reconnecting_client() {
    let _g = CONTAINER_LOCK.write().await;
    with_timeout(60, async {
        let env = TestEnv::new();
        env.manager.pool(env.id).await.unwrap();
        env.manager.console_client(env.id).await.unwrap();
        env.manager.subscriber_client(env.id, |_| {}).await.unwrap();
        env.manager.close(env.id).await.unwrap();
        assert_no_new_connections(Duration::from_secs(5)).await;
    })
    .await;
}

/// 建连失败(密码错误、不存在的 ACL 用户)后,fred 不能在后台一直重连;错误类型是 Auth。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn r03a_failed_init_returns_auth_and_leaves_no_reconnecting_client() {
    let _g = CONTAINER_LOCK.write().await;
    with_timeout(120, async {
        let cases = [
            TestEnv::with_creds(None, Some("definitely-wrong-password")),
            // 不存在的 ACL 用户也是 Auth
            TestEnv::with_creds(Some("xredis_test_no_such_user"), Some("whatever")),
        ];
        for env in &cases {
            let err = env.manager.pool(env.id).await.expect_err("should fail");
            assert!(matches!(err, Error::Auth(_)), "{err:?}");
            assert_eq!(env.manager.cached(env.id), (false, false, false));
        }
        assert_no_new_connections(Duration::from_secs(6)).await;
    })
    .await;
}

// ---------------------------------------------------------------------------
// R-03a:TLS 实连(`xredis-test-redis-tls`,127.0.0.1:6392,要求客户端证书)
// ---------------------------------------------------------------------------

async fn tls_connects_with_key(key_file: &str) {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let fx = TlsFixture::from_env();
        let env = fx.env(fx.full_tls(key_file));
        let pool = env.manager.pool(env.id).await.expect("TLS connect");
        assert_eq!(ping(&pool).await.unwrap(), "PONG");
        env.manager.close(env.id).await.unwrap();
    })
    .await;
}

/// L-12:CA 加客户端证书,PKCS#8 私钥("PRIVATE KEY")。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l12_tls_connects_with_pkcs8_client_key() {
    tls_connects_with_key("client.pkcs8.key").await;
}

/// L-12:同一把私钥的 PKCS#1 编码("RSA PRIVATE KEY")。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l12_tls_connects_with_pkcs1_client_key() {
    tls_connects_with_key("client.pkcs1.key").await;
}

/// 不填 CA 时只信系统根证书,自签证书要很快失败并返回连接类(TLS)错误,不能挂住。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l12_tls_without_ca_rejects_self_signed_server_quickly() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let fx = TlsFixture::from_env();
        let mut tls = fx.full_tls("client.pkcs8.key");
        tls.ca_cert = None;
        let env = fx.env(tls);
        let started = Instant::now();
        let err = env
            .manager
            .pool(env.id)
            .await
            .expect_err("self-signed must be rejected");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}: {err:?}",
            started.elapsed()
        );
        assert!(matches!(err, Error::Connect(_)), "{err:?}");
        assert_eq!(env.manager.cached(env.id), (false, false, false));
    })
    .await;
}

/// 服务端要求客户端证书,不给就会被拒,同样要很快失败。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn tls_without_client_cert_is_rejected_quickly() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let fx = TlsFixture::from_env();
        let tls = TlsConfig {
            ca_cert: Some(fx.file("ca.crt")),
            ..TlsConfig::default()
        };
        let env = fx.env(tls);
        let started = Instant::now();
        let err = env
            .manager
            .pool(env.id)
            .await
            .expect_err("client cert is required");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}: {err:?}",
            started.elapsed()
        );
        // 服务端握手阶段把我们拒了:连接类错误,不是配置、关闭或超时
        assert!(matches!(err, Error::Connect(_)), "{err:?}");
    })
    .await;
}
