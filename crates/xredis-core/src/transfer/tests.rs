//! transfer 的测试。纯编码解码的直接跑;需要 Redis 的标 ignore(`scripts/test-redis.sh test`)。
//! 覆盖 P-41、P-42;L-09、L-14。

use std::io::Write;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::*;

/// v1 格式样本:**不是用本模块的编码器产出的**,而是用 Python 的 zlib(级别 1)和 base64
/// 按 v1 `copy_keys` 的逻辑(JSON 数组 `[{id, value: 字节数字数组, ttl_ms}]` → zlib fast → base64
/// → 前缀 `REDIS:`)独立生成,所以能验证我们解得开别的实现产出的数据。
/// 明文 JSON:
/// `[{"id":"user:1","value":[0,5,104,101,108,108,111,12,0,255,0,17],"ttl_ms":0},
///   {"id":"会话:中文","value":[0,1,2,3,128,129,250,251,252,253,254,255],"ttl_ms":86400000}]`
const V1_SAMPLE: &str = "REDIS:eAGLrlbKTFGyUiotTi2yMlTSUSpLzClNVbKKNtAx1TE0MNExNDDUMTSwgGBDQx1DIx0DHSNTUx0DHUPzWB2lkpKc+NxiJSuDWh2oUU/2zHqxfq7Vkx1rn01rRzHRUMdIx1jH0MhCx9DIUsfIFGSQoY6RqZGOkamxjpGpCchgJDMtzEwMQKA2FgB0/C7B";

fn sample_entries() -> Vec<ClipboardEntry> {
    vec![
        ClipboardEntry {
            id: "user:1".into(),
            value: vec![0, 5, 104, 101, 108, 108, 111, 12, 0, 255, 0, 17],
            ttl_ms: 0,
        },
        ClipboardEntry {
            id: "会话:中文".into(),
            value: vec![0, 1, 2, 3, 128, 129, 250, 251, 252, 253, 254, 255],
            ttl_ms: 86_400_000,
        },
    ]
}

/// 伪随机字节(固定种子,可重复),Redis 的 LZF 压不动。
fn incompressible(n: usize) -> Vec<u8> {
    let mut x: u32 = 0x9e37_79b9;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

/// 手工按 v1 的算法编码(不经过 `encode`),用来对照。
fn v1_encode(json: &[u8]) -> String {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
    e.write_all(json).unwrap();
    format!("REDIS:{}", STANDARD.encode(e.finish().unwrap()))
}

/// P-41:v1(独立实现)产出的剪贴板数据,v2 解码后逐字段、逐字节一致。
#[test]
fn p41_decodes_a_clipboard_produced_by_an_independent_v1_encoder() {
    let got = decode(V1_SAMPLE).unwrap();
    assert_eq!(got, sample_entries());
    // 二进制值逐字节一致(含 0x00 和 0xff)
    assert_eq!(got[0].value[8], 0);
    assert_eq!(got[1].value[11], 255);
}

/// P-41:v2 编码出的数据,按 v1 的解码逻辑(base64 → zlib → JSON)读得出来,内容逐字节一致。
#[test]
fn p41_v2_output_is_readable_by_v1s_decode_path() {
    let text = encode(&sample_entries()).unwrap();
    assert!(text.starts_with("REDIS:"));
    // 独立走一遍 v1 的解码步骤
    let compressed = STANDARD.decode(&text["REDIS:".len()..]).unwrap();
    let mut json = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::ZlibDecoder::new(&compressed[..]),
        &mut json,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let arr = value.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // v1 的 IDValueDTO 字段名:id、value(数字数组)、ttl_ms
    assert_eq!(arr[0]["id"], "user:1");
    assert_eq!(arr[0]["ttl_ms"], 0);
    assert_eq!(arr[1]["id"], "会话:中文");
    assert_eq!(arr[1]["ttl_ms"], 86_400_000);
    let bytes: Vec<u8> = arr[1]["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u8)
        .collect();
    assert_eq!(
        bytes,
        vec![0, 1, 2, 3, 128, 129, 250, 251, 252, 253, 254, 255]
    );
    // 往返
    assert_eq!(decode(&text).unwrap(), sample_entries());
}

/// 更早的版本写进剪贴板的数据没有 `ttl_ms`,缺失视为不过期。
#[test]
fn missing_ttl_ms_means_no_expiry() {
    let text = v1_encode(br#"[{"id":"k","value":[1,2,3]}]"#);
    assert_eq!(
        decode(&text).unwrap(),
        vec![ClipboardEntry {
            id: "k".into(),
            value: vec![1, 2, 3],
            ttl_ms: 0
        }]
    );
}

#[test]
fn not_redis_data_is_rejected() {
    assert!(matches!(decode("hello"), Err(Error::ClipboardNotRedis)));
    assert!(matches!(decode(""), Err(Error::ClipboardNotRedis)));
    assert!(matches!(decode("redis:abc"), Err(Error::ClipboardNotRedis)));
}

#[test]
fn corrupt_data_reports_which_stage_failed() {
    let stage = |t: &str| match decode(t) {
        Err(Error::ClipboardCorrupt { stage, .. }) => stage,
        other => panic!("{other:?}"),
    };
    assert_eq!(stage("REDIS:!!!not base64!!!"), ClipboardStage::Base64);
    assert_eq!(
        stage(&format!("REDIS:{}", STANDARD.encode(b"not zlib at all"))),
        ClipboardStage::Decompress
    );
    assert_eq!(stage(&v1_encode(b"not json")), ClipboardStage::Parse);
    assert_eq!(stage(&v1_encode(br#"{"id":"k"}"#)), ClipboardStage::Parse);
}

/// L-14:粘贴时解压封顶,防解压炸弹。几十 KB 的数据能解出远超上限的内容,必须被拒绝,
/// 而且不能先全部解出来(内存里最多留 limit+1 字节)。
#[test]
fn l14_decompression_bomb_is_capped() {
    // 2 MiB 的零,fast 级别压缩后约 13 KB
    let json = {
        let mut v = br#"[{"id":"k","value":["#.to_vec();
        v.extend(std::iter::repeat_n(b'0', 2 * 1024 * 1024));
        v
    };
    let bomb = v1_encode(&json);
    assert!(
        bomb.len() < 32 * 1024, // 2 MiB 的原文压到 32 KiB 以内,压缩比 60 倍以上
        "the bomb should compress well: {}",
        bomb.len()
    );
    assert!(matches!(
        decode_with_limit(&bomb, 1024 * 1024),
        Err(Error::PasteTooLarge { limit }) if limit == 1024 * 1024
    ));
}

/// 恰好等于上限不算超限(多读 1 字节就是为了区分“恰好”和“被截断”)。
#[test]
fn l14_exactly_at_the_limit_is_accepted() {
    let json = br#"[{"id":"k","value":[1],"ttl_ms":0}]"#;
    let text = v1_encode(json);
    assert!(decode_with_limit(&text, json.len() as u64).is_ok());
    assert!(matches!(
        decode_with_limit(&text, json.len() as u64 - 1),
        Err(Error::PasteTooLarge { .. })
    ));
}

/// 粘贴上限必须严格大于复制上限:JSON 数字数组比原始数据大 3 到 5 倍,
/// 两者取齐会导致自己复制的数据反而粘不回来。
#[test]
fn paste_limit_must_exceed_copy_limit() {
    // 编译期检查:两个常量被改坏时直接编译不过
    const { assert!(crate::consts::PASTE_MAX_DECOMPRESSED_BYTES > crate::consts::COPY_MAX_TOTAL_BYTES) };
    assert_eq!(crate::consts::COPY_MAX_TOTAL_BYTES, 64 * 1024 * 1024);
    assert_eq!(
        crate::consts::PASTE_MAX_DECOMPRESSED_BYTES,
        256 * 1024 * 1024
    );
}

/// 全部 256 种字节值的 DUMP 负载往返不丢。
#[test]
fn every_byte_value_round_trips() {
    let all: Vec<u8> = (0..=255).collect();
    let entries = vec![ClipboardEntry {
        id: "all".into(),
        value: all.clone(),
        ttl_ms: 1,
    }];
    assert_eq!(decode(&encode(&entries).unwrap()).unwrap()[0].value, all);
}

// ---------------------------------------------------------------------------
// 需要 Redis
// ---------------------------------------------------------------------------

mod redis {
    use super::*;
    use crate::conn::testutil::*;
    use crate::keys::{self, Ttl};
    use crate::types::string::{self, SetMode};
    use fred::clients::Pool;

    struct Fx {
        env: TestEnv,
        pool: Pool,
        prefix: String,
    }

    impl Fx {
        async fn new() -> Self {
            let env = TestEnv::new();
            let pool = env.manager.pool(env.id).await.unwrap();
            Self {
                env,
                pool,
                prefix: unique_prefix(),
            }
        }
        fn key(&self, n: &str) -> Bytes {
            Bytes::from(format!("{}{n}", self.prefix))
        }
        async fn cleanup(&self) {
            let pattern = format!("{}*", self.prefix);
            let mut found = Vec::new();
            let mut cursor = 0;
            loop {
                let page = keys::scan_keys(&self.pool, pattern.as_bytes(), 500, cursor)
                    .await
                    .unwrap();
                found.extend(page.items);
                cursor = page.cursor;
                if cursor == 0 {
                    break;
                }
            }
            keys::delete_keys(&self.pool, &found).await.unwrap();
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

    async fn put(fx: &Fx, key: &Bytes, value: &[u8]) {
        string::set(&fx.pool, key, value, SetMode::Overwrite)
            .await
            .unwrap();
    }

    /// P-41、L-09:复制保留 TTL 和类型(用 DUMP,不止 String);粘贴到别的 key 名之前先删掉源,
    /// 值和 TTL 都回来。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn l09_copy_then_paste_keeps_value_type_and_ttl() {
        run(|fx| async move {
            let (s, h) = (fx.key("s"), fx.key("h"));
            put(&fx, &s, b"hello\x00\xff").await;
            keys::set_ttl(&fx.pool, &s, 1000).await.unwrap();
            crate::types::hash::set(&fx.pool, &h, b"f", b"v")
                .await
                .unwrap();

            let out = copy_keys(&fx.pool, &[s.clone(), h.clone()]).await.unwrap();
            assert_eq!((out.copied, out.skipped.len()), (2, 0));
            assert!(out.text.starts_with("REDIS:"));
            let entries = decode(&out.text).unwrap();
            assert_eq!(entries[0].id.as_bytes(), &s[..]);
            assert!(
                (990_000..=1_000_000).contains(&entries[0].ttl_ms),
                "{}",
                entries[0].ttl_ms
            );
            assert_eq!(entries[1].ttl_ms, 0, "no TTL → 0");

            keys::delete_keys(&fx.pool, &[s.clone(), h.clone()])
                .await
                .unwrap();
            let pasted = paste(&fx.pool, &out.text, false).await.unwrap();
            assert_eq!(pasted.count, 2);
            assert_eq!(
                &string::get(&fx.pool, &s).await.unwrap().unwrap().data[..],
                b"hello\x00\xff"
            );
            match keys::key_ttl(&fx.pool, &s).await.unwrap() {
                Ttl::Seconds(n) => assert!((990..=1000).contains(&n), "ttl {n}"),
                other => panic!("TTL was lost: {other:?}"),
            }
            assert_eq!(keys::key_ttl(&fx.pool, &h).await.unwrap(), Ttl::Persistent);
            assert_eq!(
                crate::types::hash::get(&fx.pool, &h, b"f").await.unwrap(),
                Some(Bytes::from("v"))
            );
        })
        .await;
    }

    /// R-07d:粘贴预检只读:数出已存在的 key,不写任何东西;不是 REDIS: 数据直接报错。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn r07d_preview_counts_existing_keys_and_writes_nothing() {
        run(|fx| async move {
            let (a, b, c) = (fx.key("a"), fx.key("b"), fx.key("c"));
            for (k, v) in [(&a, "A"), (&b, "B"), (&c, "C")] {
                put(&fx, k, v.as_bytes()).await;
            }
            let text = copy_keys(&fx.pool, &[a.clone(), b.clone(), c.clone()])
                .await
                .unwrap()
                .text;
            keys::delete_keys(&fx.pool, std::slice::from_ref(&b))
                .await
                .unwrap();
            let p = preview(&fx.pool, &text).await.unwrap();
            assert_eq!((p.total, p.existing), (3, 2));
            assert!(!keys::exists(&fx.pool, &b).await.unwrap(), "read-only");

            keys::delete_keys(&fx.pool, &[a, c]).await.unwrap();
            let p = preview(&fx.pool, &text).await.unwrap();
            assert_eq!((p.total, p.existing), (3, 0));

            assert!(matches!(
                preview(&fx.pool, "hello").await,
                Err(Error::ClipboardNotRedis)
            ));
        })
        .await;
    }

    /// L-09、P-42:不覆盖模式下有冲突就**整体拒绝**,列出冲突的 key,一个都不写,已有的值不变。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn l09_paste_without_overwrite_rejects_all_on_any_conflict() {
        run(|fx| async move {
            let (a, b, c) = (fx.key("a"), fx.key("b"), fx.key("c"));
            for (k, v) in [(&a, "A"), (&b, "B"), (&c, "C")] {
                put(&fx, k, v.as_bytes()).await;
            }
            let text = copy_keys(&fx.pool, &[a.clone(), b.clone(), c.clone()])
                .await
                .unwrap()
                .text;
            // a 变成别的值,b 删掉,c 保持:粘贴时只有 a 和 c 冲突
            put(&fx, &a, b"changed").await;
            keys::delete_keys(&fx.pool, std::slice::from_ref(&b))
                .await
                .unwrap();

            let err = paste(&fx.pool, &text, false).await.unwrap_err();
            match err {
                Error::PasteConflicts { conflicts, total } => {
                    assert_eq!(total, 3);
                    let mut got: Vec<_> = conflicts.iter().map(|c| c.to_vec()).collect();
                    got.sort();
                    let mut want = vec![a.to_vec(), c.to_vec()];
                    want.sort();
                    assert_eq!(got, want);
                }
                other => panic!("{other:?}"),
            }
            // 一个都没写:b 还是不存在,a 没被覆盖
            assert!(!keys::exists(&fx.pool, &b).await.unwrap());
            assert_eq!(
                &string::get(&fx.pool, &a).await.unwrap().unwrap().data[..],
                b"changed"
            );

            // 覆盖模式:全部写入
            assert_eq!(paste(&fx.pool, &text, true).await.unwrap().count, 3);
            assert_eq!(
                &string::get(&fx.pool, &a).await.unwrap().unwrap().data[..],
                b"A"
            );
            assert_eq!(
                &string::get(&fx.pool, &b).await.unwrap().unwrap().data[..],
                b"B"
            );
        })
        .await;
    }

    /// 已过期或不存在的 key 跳过并汇总,其余照常复制;一个都没复制成才算失败。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn copy_skips_missing_keys_and_fails_only_when_nothing_copied() {
        run(|fx| async move {
            let (ok, gone) = (fx.key("ok"), fx.key("gone"));
            put(&fx, &ok, b"v").await;
            let out = copy_keys(&fx.pool, &[gone.clone(), ok.clone()])
                .await
                .unwrap();
            assert_eq!(out.copied, 1);
            assert_eq!(
                out.skipped,
                vec![Skipped {
                    key: gone.clone(),
                    reason: SkipReason::Missing
                }]
            );

            let err = copy_keys(&fx.pool, std::slice::from_ref(&gone))
                .await
                .unwrap_err();
            assert!(
                matches!(err, Error::NothingCopied { ref skipped } if skipped.len() == 1),
                "{err:?}"
            );
            let err = copy_keys(&fx.pool, &[]).await.unwrap_err();
            assert!(
                matches!(err, Error::NothingCopied { ref skipped } if skipped.is_empty()),
                "{err:?}"
            );
        })
        .await;
    }

    /// v1 兼容格式里 key 名是字符串,二进制名字放不进去:跳过并汇总,不是悄悄变成乱码名字。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn copy_skips_binary_key_names_instead_of_corrupting_them() {
        run(|fx| async move {
            let ok = fx.key("ok");
            let mut bin = fx.prefix.clone().into_bytes();
            bin.extend_from_slice(&[0xff, 0xfe]);
            let bin = Bytes::from(bin);
            put(&fx, &ok, b"v").await;
            put(&fx, &bin, b"v").await;
            let out = copy_keys(&fx.pool, &[bin.clone(), ok]).await.unwrap();
            assert_eq!(out.copied, 1);
            assert_eq!(out.skipped[0].reason, SkipReason::BinaryName);
            assert_eq!(out.skipped[0].key, bin);
        })
        .await;
    }

    /// L-14:DUMP 字节累计超过上限就立即中止,报出越线的 key 和已复制的数量。
    /// (真实上限是 64 MiB,这里用小上限验证同一段逻辑,不往测试容器里塞 64 MiB。)
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn l14_copy_aborts_when_total_exceeds_the_limit() {
        run(|fx| async move {
            let keys_: Vec<Bytes> = (0..5).map(|i| fx.key(&format!("k{i}"))).collect();
            for k in &keys_ {
                put(&fx, k, &incompressible(4000)).await;
            }
            // 不可压缩的 4000 字节,每个 key 的 DUMP 约 4 KB(重复字节会被 LZF 压成几十字节,
            // 不能用);上限 10_000 → 第 3 个越线
            let err = copy_keys_with_limit(&fx.pool, &keys_, 10_000)
                .await
                .unwrap_err();
            match err {
                Error::CopyTooLarge {
                    total,
                    limit,
                    copied,
                    key,
                } => {
                    assert_eq!(limit, 10_000);
                    assert!(total > 10_000, "{total}");
                    assert_eq!(copied, 2);
                    assert_eq!(key, keys_[2]);
                }
                other => panic!("{other:?}"),
            }
            // 上限够大就能复制
            assert_eq!(
                copy_keys_with_limit(&fx.pool, &keys_, 1_000_000)
                    .await
                    .unwrap()
                    .copied,
                5
            );
        })
        .await;
    }

    /// RESTORE 失败(DUMP 数据损坏)时报出是哪个 key,前面的已经写入(和 v1 一样逐个写)。
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs redis"]
    async fn paste_reports_which_key_failed_to_restore() {
        run(|fx| async move {
            let (good, bad) = (fx.key("good"), fx.key("bad"));
            let entries = vec![
                ClipboardEntry {
                    id: String::from_utf8(good.to_vec()).unwrap(),
                    value: {
                        put(&fx, &good, b"v").await;
                        let o = copy_keys(&fx.pool, std::slice::from_ref(&good))
                            .await
                            .unwrap();
                        decode(&o.text).unwrap().remove(0).value
                    },
                    ttl_ms: 0,
                },
                ClipboardEntry {
                    id: String::from_utf8(bad.to_vec()).unwrap(),
                    value: vec![1, 2, 3],
                    ttl_ms: 0,
                },
            ];
            keys::delete_keys(&fx.pool, std::slice::from_ref(&good))
                .await
                .unwrap();
            let err = paste_entries(&fx.pool, entries, true).await.unwrap_err();
            assert!(
                matches!(err, Error::RestoreFailed { ref key, .. } if *key == bad),
                "{err:?}"
            );
            assert!(
                keys::exists(&fx.pool, &good).await.unwrap(),
                "the first key was already restored"
            );
            assert!(!keys::exists(&fx.pool, &bad).await.unwrap());
        })
        .await;
    }
}
