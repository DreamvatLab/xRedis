//! `CoreBackend` 的 key 操作对着真实 Redis 测一遍(只连 `scripts/test-redis.sh` 起的测试容器)。
//! core 的函数早已测过,这里验证的是**接线**:用对了连接、参数没传反、结果原样带回来。
//!
//! 红线:不执行 FLUSHDB、FLUSHALL、CONFIG SET、SHUTDOWN;key 用唯一前缀 `xredis_test:<ULID>:`,
//! 结束时只删自己前缀下的 key;配置写临时目录,密码用内存版密钥存储。

use std::sync::Arc;

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::config::{ConnId, ConnectionDraft, PasswordChange};
use xredis_core::keys::{KeyType, Ttl};
use xredis_core::keystore::MemoryStore;
use xredis_core::types::string::{self, SetMode};

use crate::backend::{Backend, CoreBackend};

/// 解析 `redis://[user:password@]host:port`,返回(host, port, password)。
fn test_redis() -> (String, u16, Option<String>) {
    let url = std::env::var("XREDIS_TEST_REDIS_URL").expect(
        "XREDIS_TEST_REDIS_URL is not set; run this test with scripts/test-redis.sh test -p xredis-app",
    );
    let rest = url.strip_prefix("redis://").expect("redis:// url");
    let (auth, hostport) = rest.rsplit_once('@').expect("credentials in the url");
    let password = auth.split_once(':').map(|(_, p)| p.to_string());
    let (host, port) = hostport.rsplit_once(':').expect("host:port");
    (host.to_string(), port.parse().unwrap(), password)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn core_backend_key_operations_against_a_real_redis() {
    let (host, port, password) = test_redis();
    let tmp = tempfile::tempdir().unwrap();
    let backend = CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None)
        .expect("open backend");
    let id = backend
        .save_connection(ConnectionDraft {
            id: None,
            name: "test".into(),
            host,
            port,
            ordinal: 0,
            username: None,
            tls: None,
            password: password.map_or(PasswordChange::Keep, PasswordChange::Set),
        })
        .await
        .unwrap()
        .outcome
        .id;

    let prefix = format!("xredis_test:{}:", ConnId::new());
    let key = |n: &str| Bytes::from(format!("{prefix}{n}"));
    let pool = backend
        .manager()
        .pool(id)
        .await
        .expect("connect to the test redis");
    for i in 0..25 {
        string::set(&pool, &key(&format!("k{i:02}")), b"v", SetMode::Overwrite)
            .await
            .unwrap();
    }
    xredis_core::keys::set_ttl(&pool, &key("k00"), 500)
        .await
        .unwrap();

    let result = async {
        // 扫描:按前缀 MATCH,小批次分页,走到游标归零,去重后正好 25 个
        let mut found = std::collections::HashSet::new();
        let mut cursor = 0;
        let pattern = Bytes::from(format!("{prefix}*"));
        loop {
            let page = backend.scan_keys(id, pattern.clone(), 10, cursor).await.unwrap();
            found.extend(page.items);
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
        }
        assert_eq!(found.len(), 25, "every key of this test, exactly once");
        assert!(found.iter().all(|k| k.starts_with(prefix.as_bytes())), "MATCH was applied");

        // 详情:类型、长度、TTL;不存在的 key 是 None 和 Missing
        let d = backend.key_details(id, key("k00")).await.unwrap();
        assert_eq!(d.kind, KeyType::String);
        assert_eq!(d.length, Some(1));
        assert!(matches!(d.ttl, Ttl::Seconds(n) if (1..=500).contains(&n)), "{d:?}");
        let gone = backend.key_details(id, key("nope")).await.unwrap();
        assert_eq!((gone.kind, gone.ttl), (KeyType::None, Ttl::Missing));

        // DBSIZE 至少包含我们的 25 个
        assert!(backend.db_size(id).await.unwrap() >= 25);

        // 复制三个,删掉,粘贴回来;再粘贴一次(不覆盖)整体拒绝;覆盖成功
        let three = vec![key("k01"), key("k02"), key("k03")];
        let copied = backend.copy_keys(id, three.clone()).await.unwrap();
        assert_eq!((copied.copied, copied.skipped.len()), (3, 0));
        assert!(copied.text.starts_with("REDIS:"));
        assert_eq!(backend.delete_keys(id, three.clone()).await.unwrap(), 3);
        assert_eq!(backend.key_details(id, key("k01")).await.unwrap().ttl, Ttl::Missing);

        let before = backend.preview_paste(id, copied.text.clone()).await.unwrap();
        assert_eq!((before.total, before.existing), (3, 0));
        let pasted = backend.paste_keys(id, copied.text.clone(), false).await.unwrap();
        let after = backend.preview_paste(id, copied.text.clone()).await.unwrap();
        assert_eq!((after.total, after.existing), (3, 3));
        assert_eq!(pasted.count, 3);
        assert_eq!(backend.key_details(id, key("k02")).await.unwrap().kind, KeyType::String);
        let conflict = backend.paste_keys(id, copied.text.clone(), false).await.unwrap_err();
        assert!(matches!(conflict, Error::PasteConflicts { ref conflicts, total: 3 } if conflicts.len() == 3), "{conflict:?}");
        assert_eq!(backend.paste_keys(id, copied.text, true).await.unwrap().count, 3);
    }
    .await;

    // 只清理自己前缀下的 key(不管上面成功与否)
    let mut cursor = 0;
    let mut mine: Vec<Bytes> = Vec::new();
    loop {
        let page = backend
            .scan_keys(id, Bytes::from(format!("{prefix}*")), 100, cursor)
            .await
            .unwrap();
        mine.extend(page.items);
        cursor = page.cursor;
        if cursor == 0 {
            break;
        }
    }
    backend.delete_keys(id, mine).await.unwrap();
    backend.close_connection(id).await.unwrap();
    result
}

/// R-07c:控制台和 Pub/Sub 的接线对着真实 Redis 测一遍:命令分词后的字节原样到服务端、输出格式、
/// 被拒绝的命令、订阅后发布能收到(带连接 id、二进制载荷原样)、退订后收不到、关连接后监听结束。
/// 频道名和 key 用唯一前缀,不碰别的东西。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn core_backend_console_and_pubsub_against_a_real_redis() {
    use std::sync::Mutex;
    use xredis_core::console::split;
    use xredis_core::pubsub::PubsubMessage;

    let (host, port, password) = test_redis();
    let tmp = tempfile::tempdir().unwrap();
    let backend = CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None)
        .expect("open backend");
    let received: Arc<Mutex<Vec<PubsubMessage>>> = Default::default();
    {
        let received = received.clone();
        backend.set_message_sink(Arc::new(move |m| received.lock().unwrap().push(m)));
    }
    let id = backend
        .save_connection(ConnectionDraft {
            id: None,
            name: "test".into(),
            host,
            port,
            ordinal: 0,
            username: None,
            tls: None,
            password: password.map_or(PasswordChange::Keep, PasswordChange::Set),
        })
        .await
        .unwrap()
        .outcome
        .id;

    let prefix = format!("xredis_test:{}:", ConnId::new());
    let run = |line: String| {
        let backend = &backend;
        async move { backend.run_command(id, split(&line).unwrap()).await }
    };

    let result = async {
        // 控制台:带空格的参数整体到服务端;输出是 redis-cli 的样子
        // 状态回复(OK、PONG)也带引号:fred 把简单字符串和批量字符串都变成 Value::String,
        // 分不出来,和 redis-cli(OK 不带引号)有差别,记在第 32 节
        assert_eq!(
            run(format!(r#"SET {prefix}k "hello world""#))
                .await
                .unwrap(),
            "\"OK\""
        );
        assert_eq!(
            run(format!("GET {prefix}k")).await.unwrap(),
            "\"hello world\""
        );
        assert_eq!(run(format!("GET {prefix}missing")).await.unwrap(), "(nil)");
        assert!(run(format!("DEL {prefix}k")).await.unwrap().contains('1'));
        // 被拒绝的命令不会发出去,控制台客户端还能用
        let rejected = run("SUBSCRIBE a".into()).await.unwrap_err();
        assert!(
            matches!(rejected, Error::CommandRejected { .. }),
            "{rejected:?}"
        );
        assert_eq!(run("PING".into()).await.unwrap(), "\"PONG\"");

        // Pub/Sub
        let chan = Bytes::from(format!("{prefix}chan"));
        backend.subscribe(id, chan.clone()).await.unwrap();
        let listed = backend.channels(id).await.unwrap();
        assert!(
            listed.contains(&chan),
            "the subscription is visible: {listed:?}"
        );
        let n = backend
            .publish(
                id,
                chan.clone(),
                Bytes::from_static(&[b'a', 0xff, b'\n', b'b']),
            )
            .await
            .unwrap();
        assert_eq!(n, 1, "one subscriber");
        for _ in 0..50 {
            if !received.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        {
            let got = received.lock().unwrap();
            assert_eq!(got.len(), 1, "delivered exactly once: {got:?}");
            assert_eq!(got[0].conn, id, "tagged with the connection");
            assert_eq!(got[0].channel, chan);
            assert_eq!(
                &got[0].payload[..],
                &[b'a', 0xff, b'\n', b'b'],
                "binary payload is intact"
            );
        }

        backend.unsubscribe(id, chan.clone()).await.unwrap();
        let n = backend
            .publish(id, chan.clone(), Bytes::from_static(b"nobody"))
            .await
            .unwrap();
        assert_eq!(n, 0, "no subscribers after the unsubscribe");
        assert!(!backend.channels(id).await.unwrap().contains(&chan));
    }
    .await;

    backend.close_connection(id).await.unwrap();
    result
}
