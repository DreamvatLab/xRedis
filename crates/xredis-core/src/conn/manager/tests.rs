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
