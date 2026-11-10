//! 控制台执行的测试。不需要 Redis 的直接跑;需要 Redis 的标 ignore。覆盖 P-50、P-51;L-06、L-07。

use std::time::{Duration, Instant};

use super::exec::{check_allowed, command_name_upper, is_blocking};
use super::*;
use crate::error::Error;

fn toks(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.as_bytes().to_vec()).collect()
}

/// P-51:八个会改变连接模式的命令都被拒绝,不区分大小写,前后空白不影响。
#[test]
fn p51_connection_mode_commands_are_rejected_case_insensitively() {
    for name in [
        "SUBSCRIBE",
        "PSUBSCRIBE",
        "SSUBSCRIBE",
        "UNSUBSCRIBE",
        "PUNSUBSCRIBE",
        "SUNSUBSCRIBE",
        "MONITOR",
        "RESET",
    ] {
        for variant in [name.to_string(), name.to_lowercase(), format!("  {name} ")] {
            let upper = command_name_upper(variant.as_bytes());
            let err = check_allowed(&upper).unwrap_err();
            assert!(
                matches!(err, Error::CommandRejected { ref command } if command == name),
                "{variant:?}: {err:?}"
            );
        }
    }
    assert_eq!(CONNECTION_MODE_COMMANDS.len(), 8);
    for ok in ["GET", "PUBSUB", "PUBLISH", "CLIENT", "SUBSCRIBED"] {
        assert!(check_allowed(ok).is_ok(), "{ok}");
    }
}

/// P-51:阻塞命令名单,以及参数里带 BLOCK(XREAD、XREADGROUP)。
#[test]
fn p51_blocking_detection() {
    for name in [
        "BLPOP",
        "BRPOP",
        "BLMOVE",
        "BRPOPLPUSH",
        "BLMPOP",
        "BZPOPMIN",
        "BZPOPMAX",
        "BZMPOP",
        "WAIT",
        "WAITAOF",
    ] {
        assert!(is_blocking(name, &[]), "{name}");
    }
    assert_eq!(BLOCKING_COMMANDS.len(), 10);
    assert!(is_blocking(
        "XREAD",
        &toks(&["block", "0", "STREAMS", "s", "$"])
    ));
    assert!(is_blocking(
        "XREADGROUP",
        &toks(&["GROUP", "g", "c", "BLOCK", "5"])
    ));
    assert!(!is_blocking("XREAD", &toks(&["STREAMS", "s", "0"])));
    assert!(!is_blocking("GET", &toks(&["k"])));
}

#[test]
fn command_name_is_uppercased_and_trimmed() {
    assert_eq!(command_name_upper(b" get "), "GET");
}

// ---------------------------------------------------------------------------
// 需要 Redis
// ---------------------------------------------------------------------------

mod redis {
    use super::*;
    use crate::conn::testutil::*;
    use crate::keys;
    use bytes::Bytes;
    use fred::clients::Client;
    use fred::types::Value;

    struct Fx {
        env: TestEnv,
        client: Client,
        prefix: String,
    }

    impl Fx {
        async fn new() -> Self {
            let env = TestEnv::new();
            let client = env.manager.console_client(env.id).await.unwrap();
            Self {
                env,
                client,
                prefix: unique_prefix(),
            }
        }

        /// 把一行命令分词后执行。
        async fn run(&self, line: &str) -> crate::error::Result<Option<Value>> {
            run_command(&self.client, &split(line).expect("split")).await
        }

        async fn cleanup(&self) {
            let pool = self.env.manager.pool(self.env.id).await.unwrap();
            let pattern = format!("{}*", self.prefix);
            let mut found: Vec<Bytes> = Vec::new();
            let mut cursor = 0;
            loop {
                let page = keys::scan_keys(&pool, pattern.as_bytes(), 500, cursor)
                    .await
                    .unwrap();
                found.extend(page.items);
                cursor = page.cursor;
                if cursor == 0 {
                    break;
                }
            }
            keys::delete_keys(&pool, &found).await.unwrap();
            self.env.manager.close(self.env.id).await.unwrap();
        }
    }

    async fn run<F, Fut>(f: F)
    where
        F: FnOnce(std::sync::Arc<Fx>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let _g = CONTAINER_LOCK.read().await;
        with_timeout(120, async {
            let fx = std::sync::Arc::new(Fx::new().await);
            f(fx.clone()).await;
            fx.cleanup().await;
        })
        .await;
    }

    fn text(v: &Value) -> String {
        format_output(v)
    }

    /// L-07:参数原样传给 Redis(含空格、JSON、转义字节、中文),不被按空格拆碎。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn l07_arguments_reach_redis_verbatim() {
        run(|fx| async move {
            let k = format!("{}k", fx.prefix);
            let set = |v: &str| format!(r#"SET {k} {v}"#);

            fx.run(&set(r#""hello world""#)).await.unwrap();
            let got = fx.run(&format!("GET {k}")).await.unwrap().unwrap();
            assert_eq!(text(&got), "\"hello world\"");

            fx.run(&set(r#""{\"a\": 1, \"b\": [2, 3]}""#))
                .await
                .unwrap();
            let got = fx.run(&format!("GET {k}")).await.unwrap().unwrap();
            assert_eq!(text(&got), r#""{\"a\": 1, \"b\": [2, 3]}""#);

            fx.run(&set(r#""你好 😀""#)).await.unwrap();
            let got = fx.run(&format!("STRLEN {k}")).await.unwrap().unwrap();
            assert_eq!(text(&got), "(integer) 11", "UTF-8 bytes, not code points");

            // \xHH 是真正的字节:0xff 不能变成 UTF-8 的 c3 bf
            fx.run(&set(r#""\xff\x00\xfe""#)).await.unwrap();
            let got = fx.run(&format!("STRLEN {k}")).await.unwrap().unwrap();
            assert_eq!(text(&got), "(integer) 3");
            let raw = fx.run(&format!("GET {k}")).await.unwrap().unwrap();
            match raw {
                Value::Bytes(b) => assert_eq!(&b[..], &[0xff, 0x00, 0xfe]),
                other => panic!("{other:?}"),
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn output_formats_like_redis_cli() {
        run(|fx| async move {
            let (l, h) = (format!("{}l", fx.prefix), format!("{}h", fx.prefix));
            fx.run(&format!("RPUSH {l} a b c")).await.unwrap();
            let got = fx.run(&format!("LRANGE {l} 0 -1")).await.unwrap().unwrap();
            assert_eq!(text(&got), "  1) \"a\"\n  2) \"b\"\n  3) \"c\"\n");
            assert_eq!(text(&fx.run("PING").await.unwrap().unwrap()), "\"PONG\"");
            let got = fx
                .run(&format!("GET {}missing", fx.prefix))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(text(&got), "(nil)");
            assert_eq!(
                text(&fx.run(&format!("LLEN {l}")).await.unwrap().unwrap()),
                "(integer) 3"
            );
            fx.run(&format!("HSET {h} f1 v1")).await.unwrap();
            let got = fx.run(&format!("HGETALL {h}")).await.unwrap().unwrap();
            // RESP2 下是平铺数组,RESP3 下是 Map;两种都必须格式化成编号的行,不能是占位文字
            let out = text(&got);
            assert!(out.contains("\"f1\"") && out.contains("\"v1\""), "{out}");
            assert!(!out.contains("unsupported"), "{out}");
        })
        .await;
    }

    /// P-51:订阅类命令在到达服务器之前就被拒绝,控制台客户端不会进入订阅模式,之后还能正常用。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn p51_subscribe_is_rejected_and_console_keeps_working() {
        run(|fx| async move {
            for line in ["SUBSCRIBE ch", "psubscribe a*", "MONITOR", "reset"] {
                let err = fx.run(line).await.unwrap_err();
                assert!(
                    matches!(err, Error::CommandRejected { .. }),
                    "{line}: {err:?}"
                );
            }
            // 客户端没被搞坏
            assert_eq!(text(&fx.run("PING").await.unwrap().unwrap()), "\"PONG\"");
            let chans = fx.run("PUBSUB CHANNELS").await.unwrap().unwrap();
            assert!(matches!(chans, Value::Array(_)), "{chans:?}");
        })
        .await;
    }

    /// P-51:阻塞命令在客户端侧超时。测试用 1 秒(真实是 30 秒)验证同一段逻辑,
    /// 到点返回 `BlockingTimeout`,不能挂住;控制台客户端之后还能用的前提是服务端那边的阻塞自己结束。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn p51_blocking_command_times_out_client_side() {
        run(|fx| async move {
            let k = format!("{}never", fx.prefix);
            let tokens = split(&format!("BLPOP {k} 3")).unwrap();
            let started = Instant::now();
            let err = exec_with_timeout(&fx.client, &tokens, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(
                matches!(err, Error::BlockingTimeout { ref command, secs } if command == "BLPOP" && secs == 1),
                "{err:?}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "took {:?}",
                started.elapsed()
            );
            // 带 BLOCK 参数的 XREAD 同样走超时
            let s = format!("{}stream", fx.prefix);
            let tokens = split(&format!("XREAD BLOCK 3000 STREAMS {s} $")).unwrap();
            let err = exec_with_timeout(&fx.client, &tokens, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(matches!(err, Error::BlockingTimeout { .. }), "{err:?}");
            // 等服务端的阻塞自己到期,控制台就恢复了
            tokio::time::sleep(Duration::from_secs(3)).await;
            assert_eq!(text(&fx.run("PING").await.unwrap().unwrap()), "\"PONG\"");
        })
        .await;
    }

    /// 阻塞命令有数据时正常返回,不被当成超时。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn blocking_command_with_data_returns_normally() {
        run(|fx| async move {
            let k = format!("{}q", fx.prefix);
            fx.run(&format!("RPUSH {k} item")).await.unwrap();
            let got = fx.run(&format!("BLPOP {k} 1")).await.unwrap().unwrap();
            let out = text(&got);
            assert!(out.contains("\"item\""), "{out}");
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn empty_line_does_nothing_and_server_errors_are_errors() {
        run(|fx| async move {
            assert!(run_command(&fx.client, &[]).await.unwrap().is_none());
            let err = fx.run("NOSUCHCOMMAND x").await.unwrap_err();
            assert!(matches!(err, Error::Redis(_)), "{err:?}");
            let err = fx.run("GET").await.unwrap_err(); // 参数个数不对
            assert!(matches!(err, Error::Redis(_)), "{err:?}");
            assert_eq!(text(&fx.run("PING").await.unwrap().unwrap()), "\"PONG\"");
        })
        .await;
    }
}

/// 测试里用小超时调同一段实现。
async fn exec_with_timeout(
    client: &fred::clients::Client,
    tokens: &[Vec<u8>],
    t: Duration,
) -> crate::error::Result<Option<fred::types::Value>> {
    super::exec::run_command_with_timeout(client, tokens, t).await
}
