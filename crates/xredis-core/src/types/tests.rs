//! keys 和 types 的测试,全部需要 Redis(标 ignore,用 `scripts/test-redis.sh test` 跑,
//! 只连测试容器 127.0.0.1:6391)。
//!
//! 覆盖:P-10、P-12、P-14、P-16、P-20 至 P-28、P-30、P-40 的后端部分;L-08、L-15。
//! 红线:不执行 FLUSHDB、FLUSHALL、CONFIG SET、SHUTDOWN;每个测试用唯一前缀
//! `xredis_test:<ULID>:`,结束时只删自己前缀下的 key。

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use bytes::Bytes;
use fred::clients::Pool;
use fred::interfaces::ClientLike;
use fred::types::FromValue;

use crate::conn::testutil::*;
use crate::error::Error;
use crate::keys::{self, KeyDetails, KeyType, Ttl};
use crate::types::string::{SetMode, StringValue};
use crate::types::{hash, list, set, string, zset};
use crate::value::{arg, command};

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

    /// 本测试前缀下的 key 名(字节)。
    fn key(&self, name: &str) -> Vec<u8> {
        format!("{}{name}", self.prefix).into_bytes()
    }

    /// 前缀加任意字节(二进制名字)。
    fn bin_key(&self, suffix: &[u8]) -> Vec<u8> {
        let mut k = self.prefix.clone().into_bytes();
        k.extend_from_slice(suffix);
        k
    }

    /// 任意命令,参数是字节。
    async fn cmd<T: FromValue>(&self, name: &'static str, args: &[&[u8]]) -> T {
        self.pool
            .custom(
                command(name),
                args.iter().map(|a| arg(a)).collect::<Vec<_>>(),
            )
            .await
            .unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    /// 只删本测试前缀下的 key。
    async fn cleanup(&self) {
        let pattern = format!("{}*", self.prefix).into_bytes();
        let mut cursor = 0;
        let mut found = Vec::new();
        loop {
            let page = keys::scan_keys(&self.pool, &pattern, 500, cursor)
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

/// 拿读锁(会重启容器的测试拿写锁)、套超时、测完清理。
async fn run<F, Fut>(f: F)
where
    F: FnOnce(std::sync::Arc<Fx>) -> Fut,
    Fut: Future<Output = ()>,
{
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(120, async {
        let fx = std::sync::Arc::new(Fx::new().await);
        f(fx.clone()).await;
        fx.cleanup().await;
    })
    .await;
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

/// 二进制数据:不是合法 UTF-8。
const BIN1: &[u8] = &[0xff, 0xfe, 0x00, 0x80];
const BIN2: &[u8] = &[0x80, 0x81, 0x82];

// ---------------------------------------------------------------------------
// keys
// ---------------------------------------------------------------------------

/// P-10、P-12:游标扫描直到归零能拿到全部匹配的 key(SCAN 可能重复,这里去重比较);
/// pattern 为 `*` 时不带 MATCH;二进制 key 名原样返回。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn scan_keys_walks_the_cursor_and_filters_by_pattern() {
    run(|fx| async move {
        let mut expected = HashSet::new();
        for i in 0..1200 {
            let k = fx.key(&format!("k{i}"));
            fx.cmd::<()>("SET", &[&k, b"v"]).await;
            expected.insert(k);
        }
        let other = fx.key("other");
        fx.cmd::<()>("SET", &[&other, b"v"]).await;
        let bin = fx.bin_key(&[b'k', 0xff, 0xfe]);
        fx.cmd::<()>("SET", &[&bin, b"v"]).await;
        expected.insert(bin.clone());

        let pattern = fx.key("k*");
        let mut got = HashSet::new();
        let mut cursor = 0;
        let mut pages = 0;
        loop {
            let page = keys::scan_keys(&fx.pool, &pattern, 100, cursor)
                .await
                .unwrap();
            got.extend(page.items.iter().map(|k| k.to_vec()));
            cursor = page.cursor;
            pages += 1;
            if cursor == 0 {
                break;
            }
        }
        assert!(
            pages > 1,
            "COUNT 100 over 1200 keys must take several pages"
        );
        assert_eq!(got, expected.iter().cloned().collect::<HashSet<_>>());
        assert!(!got.contains(&other));
        assert!(
            got.contains(&bin),
            "binary key name must come back byte-exact"
        );

        // pattern 为 `*`:不带 MATCH,整库扫描能看到我们的 key
        let mut all = HashSet::new();
        let mut cursor = 0;
        loop {
            let page = keys::scan_keys(&fx.pool, b"*", 1000, cursor).await.unwrap();
            all.extend(page.items.iter().map(|k| k.to_vec()));
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
        }
        assert!(all.contains(&other) && expected.iter().all(|k| all.contains(k)));
    })
    .await;
}

/// P-14、P-15、P-16:五种类型加 Stream 的详情(类型、长度、TTL);不存在的 key 是
/// `None` 和 `Missing`。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn key_details_for_every_type_and_missing_key() {
    run(|fx| async move {
        let (s, h, l, st, z, x) = (
            fx.key("s"),
            fx.key("h"),
            fx.key("l"),
            fx.key("set"),
            fx.key("z"),
            fx.key("x"),
        );
        fx.cmd::<()>("SET", &[&s, b"hello"]).await;
        fx.cmd::<i64>("HSET", &[&h, b"f1", b"v1", b"f2", b"v2"])
            .await;
        fx.cmd::<i64>("RPUSH", &[&l, b"a", b"b", b"c"]).await;
        fx.cmd::<i64>("SADD", &[&st, b"m1", b"m2", b"m3", b"m4"])
            .await;
        fx.cmd::<i64>("ZADD", &[&z, b"1", b"a", b"2", b"b"]).await;
        fx.cmd::<String>("XADD", &[&x, b"*", b"f", b"v"]).await;
        fx.cmd::<i64>("EXPIRE", &[&s, b"100"]).await;

        let d = |k: Vec<u8>| {
            let pool = fx.pool.clone();
            async move { keys::key_details(&pool, &k).await.unwrap() }
        };
        let ds = d(s).await;
        assert_eq!((ds.kind.clone(), ds.length), (KeyType::String, Some(5)));
        assert!(
            matches!(ds.ttl, Ttl::Seconds(n) if (1..=100).contains(&n)),
            "{ds:?}"
        );
        assert_eq!(
            d(h).await,
            KeyDetails {
                kind: KeyType::Hash,
                length: Some(2),
                ttl: Ttl::Persistent
            }
        );
        assert_eq!(d(l).await.length, Some(3));
        assert_eq!(d(st).await.length, Some(4));
        assert_eq!(d(z).await.length, Some(2));
        let dx = d(x).await;
        assert_eq!((dx.kind.clone(), dx.length), (KeyType::Stream, Some(1)));
        assert!(!dx.kind.openable(), "Stream is shown but cannot be opened");

        let missing = d(fx.key("nope")).await;
        assert_eq!(
            missing,
            KeyDetails {
                kind: KeyType::None,
                length: None,
                ttl: Ttl::Missing
            }
        );
    })
    .await;
}

/// 二进制名字的 key 能删除、能改 TTL(v1 做不到),全程用字节。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn binary_key_name_can_get_ttl_and_be_deleted() {
    run(|fx| async move {
        let k = fx.bin_key(BIN1);
        assert!(
            std::str::from_utf8(&k).is_err(),
            "test key must not be UTF-8"
        );
        fx.cmd::<()>("SET", &[&k, b"v"]).await;
        keys::set_ttl(&fx.pool, &k, 100).await.unwrap();
        let d = keys::key_details(&fx.pool, &k).await.unwrap();
        assert!(
            matches!(d.ttl, Ttl::Seconds(n) if (1..=100).contains(&n)),
            "{d:?}"
        );
        keys::set_ttl(&fx.pool, &k, -1).await.unwrap();
        assert_eq!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Persistent);

        let deleted = keys::delete_keys(&fx.pool, &[Bytes::from(k.clone())])
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(!keys::exists(&fx.pool, &k).await.unwrap());
    })
    .await;
}

/// P-40:批量删除按每批 500 个分批 DEL,返回实际删掉的数量(不存在的不计)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn delete_keys_in_batches_counts_only_existing() {
    run(|fx| async move {
        let mut names = Vec::new();
        for i in 0..1100 {
            let k = fx.key(&format!("d{i}"));
            fx.cmd::<()>("SET", &[&k, b"v"]).await;
            names.push(Bytes::from(k));
        }
        for i in 0..7 {
            names.push(Bytes::from(fx.key(&format!("never-existed-{i}"))));
        }
        assert!(names.len() > 2 * crate::consts::DELETE_BATCH);
        let deleted = keys::delete_keys(&fx.pool, &names).await.unwrap();
        assert_eq!(deleted, 1100);
        assert_eq!(keys::delete_keys(&fx.pool, &[]).await.unwrap(), 0);
    })
    .await;
}

/// P-27:RENAMENX。目标已存在报错且两个 key 都不动;源不存在是 NotFound。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn rename_key_uses_renamenx() {
    run(|fx| async move {
        let (a, bb, c) = (fx.key("a"), fx.key("b"), fx.key("c"));
        fx.cmd::<()>("SET", &[&a, b"A"]).await;
        fx.cmd::<()>("SET", &[&bb, b"B"]).await;

        let err = keys::rename_key(&fx.pool, &a, &bb).await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(
            string::get(&fx.pool, &a).await.unwrap().unwrap().data,
            b("A")
        );
        assert_eq!(
            string::get(&fx.pool, &bb).await.unwrap().unwrap().data,
            b("B")
        );

        keys::rename_key(&fx.pool, &a, &c).await.unwrap();
        assert!(!keys::exists(&fx.pool, &a).await.unwrap());
        assert_eq!(
            string::get(&fx.pool, &c).await.unwrap().unwrap().data,
            b("A")
        );

        let err = keys::rename_key(&fx.pool, &a, &fx.key("z"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Missing { .. }), "{err:?}");
    })
    .await;
}

/// P-28、L-08:TTL 规则。0 被拒绝且 key 还在;-1 是 PERSIST,对本来就没有 TTL 的 key 不算错
/// (v1 把它当成“key 不存在”);不存在的 key 是 NotFound;超过上限被拒。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn ttl_rules_against_a_real_server() {
    run(|fx| async move {
        let k = fx.key("t");
        fx.cmd::<()>("SET", &[&k, b"v"]).await;

        // L-08:EXPIRE 0 会删除 key,必须拒绝,key 还在
        let err = keys::set_ttl(&fx.pool, &k, 0).await.unwrap_err();
        assert!(matches!(err, Error::TtlZero), "{err:?}");
        assert!(keys::exists(&fx.pool, &k).await.unwrap());

        // PERSIST 一个本来就没有 TTL 的 key:成功
        assert_eq!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Persistent);
        keys::set_ttl(&fx.pool, &k, -1).await.unwrap();

        keys::set_ttl(&fx.pool, &k, 100).await.unwrap();
        assert!(matches!(
            keys::key_ttl(&fx.pool, &k).await.unwrap(),
            Ttl::Seconds(_)
        ));
        keys::set_ttl(&fx.pool, &k, -1).await.unwrap();
        assert_eq!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Persistent);

        keys::set_ttl(&fx.pool, &k, crate::consts::TTL_MAX)
            .await
            .unwrap();
        assert!(matches!(
            keys::set_ttl(&fx.pool, &k, crate::consts::TTL_MAX + 1).await,
            Err(Error::TtlOutOfRange { .. })
        ));

        // 不存在的 key
        let missing = fx.key("missing");
        for ttl in [-1, 100] {
            let err = keys::set_ttl(&fx.pool, &missing, ttl).await.unwrap_err();
            assert!(matches!(err, Error::Missing { .. }), "ttl {ttl}: {err:?}");
        }
        assert!(!keys::exists(&fx.pool, &missing).await.unwrap());
    })
    .await;
}

// ---------------------------------------------------------------------------
// String
// ---------------------------------------------------------------------------

/// P-20、P-21:新建不覆盖;覆盖会清 TTL,KEEPTTL 保留;空字符串和不存在要分得清;
/// 二进制值原样往返。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn string_create_overwrite_keepttl_and_binary() {
    run(|fx| async move {
        let k = fx.key("s");
        assert_eq!(string::get(&fx.pool, &k).await.unwrap(), None);

        string::set(&fx.pool, &k, b"one", SetMode::Create)
            .await
            .unwrap();
        let err = string::set(&fx.pool, &k, b"two", SetMode::Create)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(
            string::get(&fx.pool, &k).await.unwrap().unwrap().data,
            b("one")
        );

        keys::set_ttl(&fx.pool, &k, 100).await.unwrap();
        string::set(&fx.pool, &k, b"keep", SetMode::KeepTtl)
            .await
            .unwrap();
        assert!(matches!(
            keys::key_ttl(&fx.pool, &k).await.unwrap(),
            Ttl::Seconds(_)
        ));
        string::set(&fx.pool, &k, b"clear", SetMode::Overwrite)
            .await
            .unwrap();
        assert_eq!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Persistent);

        // 空字符串是 Some(空),不是 None
        string::set(&fx.pool, &k, b"", SetMode::Overwrite)
            .await
            .unwrap();
        assert_eq!(
            string::get(&fx.pool, &k).await.unwrap(),
            Some(StringValue {
                data: Bytes::new(),
                total_len: 0,
                truncated: false
            })
        );

        // 二进制值
        string::set(&fx.pool, &k, BIN1, SetMode::Overwrite)
            .await
            .unwrap();
        assert_eq!(
            &string::get(&fx.pool, &k).await.unwrap().unwrap().data[..],
            BIN1
        );
    })
    .await;
}

/// P-30:超过 8 MiB 只读前 1 MiB 并标记;正好 8 MiB 完整读取。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn large_string_reads_only_the_first_mib() {
    run(|fx| async move {
        let k = fx.key("big");
        let limit = crate::consts::LARGE_VALUE_THRESHOLD as usize;
        let preview = crate::consts::LARGE_VALUE_PREVIEW as usize;

        // 正好 8 MiB:不截断
        let exact: Vec<u8> = (0..limit).map(|i| (i % 251) as u8).collect();
        string::set(&fx.pool, &k, &exact, SetMode::Overwrite)
            .await
            .unwrap();
        let v = string::get(&fx.pool, &k).await.unwrap().unwrap();
        assert!(!v.truncated);
        assert_eq!(v.total_len as usize, limit);
        assert_eq!(v.data.len(), limit);

        // 8 MiB 加 1 字节:只读前 1 MiB
        let mut big = exact.clone();
        big.push(7);
        string::set(&fx.pool, &k, &big, SetMode::Overwrite)
            .await
            .unwrap();
        let v = string::get(&fx.pool, &k).await.unwrap().unwrap();
        assert!(v.truncated);
        assert_eq!(v.total_len as usize, limit + 1);
        assert_eq!(v.data.len(), preview);
        assert_eq!(&v.data[..], &big[..preview]);
    })
    .await;
}

// ---------------------------------------------------------------------------
// Hash
// ---------------------------------------------------------------------------

/// P-23:增删改查和扫描,field 名和值都可以是二进制。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn hash_crud_scan_and_binary() {
    run(|fx| async move {
        let k = fx.key("h");
        hash::set(&fx.pool, &k, b"f1", b"v1").await.unwrap();
        hash::set(&fx.pool, &k, BIN1, BIN2).await.unwrap();
        hash::add(&fx.pool, &k, b"f2", b"v2").await.unwrap();
        let err = hash::add(&fx.pool, &k, b"f2", b"other").await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(hash::get(&fx.pool, &k, b"f2").await.unwrap(), Some(b("v2")));
        assert_eq!(hash::get(&fx.pool, &k, b"nope").await.unwrap(), None);
        hash::set(&fx.pool, &k, b"f1", b"changed").await.unwrap();

        let mut all = Vec::new();
        let mut cursor = 0;
        loop {
            let page = hash::scan(&fx.pool, &k, b"*", 100, cursor).await.unwrap();
            all.extend(page.items);
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
        }
        all.sort();
        let mut want = vec![
            (b("f1"), b("changed")),
            (b("f2"), b("v2")),
            (Bytes::from_static(BIN1), Bytes::from_static(BIN2)),
        ];
        want.sort();
        assert_eq!(all, want);

        // 二进制 field 能删
        assert_eq!(
            hash::delete(&fx.pool, &k, &[Bytes::from_static(BIN1), b("f2"), b("zzz")])
                .await
                .unwrap(),
            2
        );
        assert_eq!(hash::get(&fx.pool, &k, BIN1).await.unwrap(), None);
    })
    .await;
}

/// P-23:改 field 名。冲突报错且两边都不动;旧的不存在报 NotFound 且不会凭空多出新 field;
/// 成功时值原样搬过去。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn hash_rename_field_checks_conflicts() {
    run(|fx| async move {
        let k = fx.key("h");
        hash::set(&fx.pool, &k, b"old", BIN2).await.unwrap();
        hash::set(&fx.pool, &k, b"taken", b"t").await.unwrap();

        let err = hash::rename_field(&fx.pool, &k, b"old", b"taken")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(
            &hash::get(&fx.pool, &k, b"old").await.unwrap().unwrap()[..],
            BIN2
        );
        assert_eq!(
            hash::get(&fx.pool, &k, b"taken").await.unwrap(),
            Some(b("t"))
        );

        let err = hash::rename_field(&fx.pool, &k, b"ghost", b"fresh")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Missing { .. }), "{err:?}");
        assert_eq!(hash::get(&fx.pool, &k, b"fresh").await.unwrap(), None);

        hash::rename_field(&fx.pool, &k, b"old", BIN1)
            .await
            .unwrap();
        assert_eq!(hash::get(&fx.pool, &k, b"old").await.unwrap(), None);
        assert_eq!(
            &hash::get(&fx.pool, &k, BIN1).await.unwrap().unwrap()[..],
            BIN2
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/// L-15:二进制元素不会被丢掉,后面元素的下标不会错位;P-24:头插尾插、LINDEX、LSET。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn list_keeps_binary_elements_and_indexes_aligned() {
    run(|fx| async move {
        let k = fx.key("l");
        for item in [BIN1, b"a", BIN2, b"b"] {
            list::push(&fx.pool, &k, item, list::Side::Tail)
                .await
                .unwrap();
        }
        let items = list::range(&fx.pool, &k, 0, -1).await.unwrap();
        assert_eq!(items.len(), 4, "binary elements must not be dropped");
        assert_eq!(&items[0][..], BIN1);
        assert_eq!(&items[1][..], b"a");
        assert_eq!(&items[2][..], BIN2);
        assert_eq!(&items[3][..], b"b");
        // 下标和内容一一对应
        assert_eq!(
            &list::get(&fx.pool, &k, 2).await.unwrap().unwrap()[..],
            BIN2
        );
        assert_eq!(list::get(&fx.pool, &k, 9).await.unwrap(), None);

        assert_eq!(
            list::push(&fx.pool, &k, b"head", list::Side::Head)
                .await
                .unwrap(),
            5
        );
        assert_eq!(
            &list::get(&fx.pool, &k, 0).await.unwrap().unwrap()[..],
            b"head"
        );

        list::set(&fx.pool, &k, 3, b"changed").await.unwrap();
        let items = list::range(&fx.pool, &k, 0, -1).await.unwrap();
        assert_eq!(&items[3][..], b"changed");
        assert_eq!(&items[1][..], BIN1, "untouched binary element is intact");
        assert!(list::set(&fx.pool, &k, 99, b"x").await.is_err());
        assert!(list::set(&fx.pool, &k, -1, b"x").await.is_err());
    })
    .await;
}

/// P-24、L-15:按下标删除(含二进制元素)删对的元素;越界或负下标整体拒绝且列表不变。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn list_remove_by_index_including_binary_elements() {
    run(|fx| async move {
        let k = fx.key("l");
        let fill = || async {
            for item in [BIN1, b"a", BIN2, b"b", b"a"] {
                list::push(&fx.pool, &k, item, list::Side::Tail)
                    .await
                    .unwrap();
            }
        };
        fill().await;
        // 删下标 2(二进制元素);另一个 "a"(下标 4)不能被误删
        list::remove_at(&fx.pool, &k, &[2]).await.unwrap();
        let items = list::range(&fx.pool, &k, 0, -1).await.unwrap();
        let got: Vec<&[u8]> = items.iter().map(|i| &i[..]).collect();
        assert_eq!(got, vec![BIN1, &b"a"[..], &b"b"[..], &b"a"[..]]);

        // 同时删头和一个重复值的位置:按下标,不是按值
        list::remove_at(&fx.pool, &k, &[0, 3, 3]).await.unwrap();
        let items = list::range(&fx.pool, &k, 0, -1).await.unwrap();
        let got: Vec<&[u8]> = items.iter().map(|i| &i[..]).collect();
        assert_eq!(got, vec![&b"a"[..], &b"b"[..]]);

        // 越界、负下标:整体拒绝,不删一半
        let before = list::range(&fx.pool, &k, 0, -1).await.unwrap();
        for bad in [vec![0, 99], vec![-1], vec![1, 2]] {
            let err = list::remove_at(&fx.pool, &k, &bad).await.unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::ListIndexOutOfRange { .. } | Error::ListIndexInvalid { .. }
                ),
                "{bad:?}: {err:?}"
            );
            assert_eq!(list::range(&fx.pool, &k, 0, -1).await.unwrap(), before);
        }
        // 没有下标:什么也不做
        list::remove_at(&fx.pool, &k, &[]).await.unwrap();
        // 删光
        list::remove_at(&fx.pool, &k, &[0, 1]).await.unwrap();
        assert_eq!(list::len(&fx.pool, &k).await.unwrap(), 0);
        let _ = fill; // 同一个闭包只用来建数据
    })
    .await;
}

// ---------------------------------------------------------------------------
// Set
// ---------------------------------------------------------------------------

/// P-25:新增已存在的成员报错;扫描;删除;二进制成员。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn set_add_scan_remove_and_binary_members() {
    run(|fx| async move {
        let k = fx.key("set");
        set::add(&fx.pool, &k, b"m1").await.unwrap();
        set::add(&fx.pool, &k, BIN1).await.unwrap();
        let err = set::add(&fx.pool, &k, b"m1").await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");

        let mut all = Vec::new();
        let mut cursor = 0;
        loop {
            let page = set::scan(&fx.pool, &k, b"*", 100, cursor).await.unwrap();
            all.extend(page.items);
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
        }
        all.sort();
        let mut want = vec![b("m1"), Bytes::from_static(BIN1)];
        want.sort();
        assert_eq!(all, want);

        assert_eq!(
            set::remove(&fx.pool, &k, &[Bytes::from_static(BIN1), b("ghost")])
                .await
                .unwrap(),
            1
        );
        assert!(!set::is_member(&fx.pool, &k, BIN1).await.unwrap());
    })
    .await;
}

/// P-25:改成员名。冲突报错两边不动;旧成员不存在报 NotFound,不会凭空多出新成员
/// (v1 会);成功时旧的没了新的有。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn set_rename_member_checks_conflicts() {
    run(|fx| async move {
        let k = fx.key("set");
        set::add(&fx.pool, &k, b"old").await.unwrap();
        set::add(&fx.pool, &k, b"taken").await.unwrap();

        let err = set::rename_member(&fx.pool, &k, b"old", b"taken")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert!(set::is_member(&fx.pool, &k, b"old").await.unwrap());

        let err = set::rename_member(&fx.pool, &k, b"ghost", b"fresh")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Missing { .. }), "{err:?}");
        assert!(
            !set::is_member(&fx.pool, &k, b"fresh").await.unwrap(),
            "no phantom member"
        );

        set::rename_member(&fx.pool, &k, b"old", BIN1)
            .await
            .unwrap();
        assert!(!set::is_member(&fx.pool, &k, b"old").await.unwrap());
        assert!(set::is_member(&fx.pool, &k, BIN1).await.unwrap());
    })
    .await;
}

// ---------------------------------------------------------------------------
// ZSet
// ---------------------------------------------------------------------------

/// P-26:新增已存在报错;改分数;分数读写(含 inf);NaN 被拒;扫描带分数;二进制成员。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn zset_add_score_scan_and_binary_members() {
    run(|fx| async move {
        let k = fx.key("z");
        zset::add(&fx.pool, &k, b"a", 1.5).await.unwrap();
        zset::add(&fx.pool, &k, BIN1, -2.0).await.unwrap();
        zset::add(&fx.pool, &k, b"top", f64::INFINITY)
            .await
            .unwrap();
        let err = zset::add(&fx.pool, &k, b"a", 9.0).await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(
            zset::score(&fx.pool, &k, b"a").await.unwrap(),
            Some(1.5),
            "not overwritten"
        );
        assert!(matches!(
            zset::add(&fx.pool, &k, b"nan", f64::NAN).await,
            Err(Error::ScoreNaN)
        ));

        zset::set_score(&fx.pool, &k, b"a", 3.25).await.unwrap();
        assert_eq!(zset::score(&fx.pool, &k, b"a").await.unwrap(), Some(3.25));
        assert_eq!(zset::score(&fx.pool, &k, b"ghost").await.unwrap(), None);
        let err = zset::set_score(&fx.pool, &k, b"ghost", 1.0)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Missing { .. }), "{err:?}");

        let mut all = Vec::new();
        let mut cursor = 0;
        loop {
            let page = zset::scan(&fx.pool, &k, b"*", 100, cursor).await.unwrap();
            all.extend(page.items);
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
        }
        all.sort_by(|x, y| x.0.cmp(&y.0));
        let mut want = vec![
            (b("a"), 3.25),
            (Bytes::from_static(BIN1), -2.0),
            (b("top"), f64::INFINITY),
        ];
        want.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(all, want);

        assert_eq!(
            zset::remove(&fx.pool, &k, &[Bytes::from_static(BIN1), b("ghost")])
                .await
                .unwrap(),
            1
        );
    })
    .await;
}

/// P-26:改成员名保留分数。冲突报错两边不动;旧成员不存在报 NotFound 且不创建新成员。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn zset_rename_member_keeps_score_and_checks_conflicts() {
    run(|fx| async move {
        let k = fx.key("z");
        zset::add(&fx.pool, &k, b"old", 4.5).await.unwrap();
        zset::add(&fx.pool, &k, b"taken", 7.0).await.unwrap();

        let err = zset::rename_member(&fx.pool, &k, b"old", b"taken")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AlreadyExists { .. }), "{err:?}");
        assert_eq!(zset::score(&fx.pool, &k, b"old").await.unwrap(), Some(4.5));
        assert_eq!(
            zset::score(&fx.pool, &k, b"taken").await.unwrap(),
            Some(7.0)
        );

        let err = zset::rename_member(&fx.pool, &k, b"ghost", b"fresh")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Missing { .. }), "{err:?}");
        assert_eq!(zset::score(&fx.pool, &k, b"fresh").await.unwrap(), None);

        zset::rename_member(&fx.pool, &k, b"old", BIN1)
            .await
            .unwrap();
        assert_eq!(zset::score(&fx.pool, &k, b"old").await.unwrap(), None);
        assert_eq!(zset::score(&fx.pool, &k, BIN1).await.unwrap(), Some(4.5));
    })
    .await;
}

// ---------------------------------------------------------------------------
// 事务原子性、类型错误
// ---------------------------------------------------------------------------

/// 改名在 MULTI/EXEC 里:并发观察者用单条命令(HKEYS、SMEMBERS、ZRANGE)任何时刻都只能看到
/// 一个名字。如果删旧和加新是两个独立命令,观察者会看到空(两个都没有)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn renames_are_atomic_for_concurrent_readers() {
    run(|fx| async move {
        let (h, s, z) = (fx.key("h"), fx.key("s"), fx.key("z"));
        hash::set(&fx.pool, &h, b"a", b"v").await.unwrap();
        set::add(&fx.pool, &s, b"a").await.unwrap();
        zset::add(&fx.pool, &z, b"a", 1.0).await.unwrap();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer = {
            let (fx, stop) = (fx.clone(), stop.clone());
            let (h, s, z) = (h.clone(), s.clone(), z.clone());
            tokio::spawn(async move {
                let mut reads = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let hk: Vec<Bytes> = fx.cmd("HKEYS", &[&h]).await;
                    let sm: Vec<Bytes> = fx.cmd("SMEMBERS", &[&s]).await;
                    let zr: Vec<Bytes> = fx.cmd("ZRANGE", &[&z, b"0", b"-1"]).await;
                    assert_eq!(hk.len(), 1, "hash showed {hk:?} mid-rename");
                    assert_eq!(sm.len(), 1, "set showed {sm:?} mid-rename");
                    assert_eq!(zr.len(), 1, "zset showed {zr:?} mid-rename");
                    reads += 1;
                }
                reads
            })
        };
        let (mut from, mut to) = (&b"a"[..], &b"b"[..]);
        for _ in 0..200 {
            hash::rename_field(&fx.pool, &h, from, to).await.unwrap();
            set::rename_member(&fx.pool, &s, from, to).await.unwrap();
            zset::rename_member(&fx.pool, &z, from, to).await.unwrap();
            std::mem::swap(&mut from, &mut to);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = tokio::time::timeout(Duration::from_secs(30), observer)
            .await
            .expect("observer hung")
            .unwrap();
        assert!(reads > 5, "observer barely ran: {reads}");
    })
    .await;
}

/// 对错误类型的 key 操作返回 Redis 错误(WRONGTYPE),不 panic、不改数据。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn wrong_type_is_a_redis_error_and_changes_nothing() {
    run(|fx| async move {
        let k = fx.key("s");
        string::set(&fx.pool, &k, b"text", SetMode::Create)
            .await
            .unwrap();
        let err = hash::get(&fx.pool, &k, b"f").await.unwrap_err();
        assert!(
            matches!(err, Error::Redis(ref m) if m.contains("WRONGTYPE")),
            "{err:?}"
        );
        assert!(hash::rename_field(&fx.pool, &k, b"a", b"b").await.is_err());
        assert!(list::remove_at(&fx.pool, &k, &[0]).await.is_err());
        assert_eq!(
            string::get(&fx.pool, &k).await.unwrap().unwrap().data,
            b("text")
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// R-04a:KeepTtl 在旧版 Redis 上的回退
// ---------------------------------------------------------------------------

/// 强制走回退分支(对 8.0 服务器):有 TTL 的保留(有一点时间误差),没有 TTL 的仍然没有,
/// 不存在的 key 被创建且没有 TTL。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn keepttl_fallback_branch_preserves_ttl() {
    run(|fx| async move {
        let k = fx.key("ttl");
        string::set(&fx.pool, &k, b"one", SetMode::Overwrite)
            .await
            .unwrap();
        keys::set_ttl(&fx.pool, &k, 100).await.unwrap();
        string::set_keep_ttl_fallback(&fx.pool, &k, b"two")
            .await
            .unwrap();
        assert_eq!(
            string::get(&fx.pool, &k).await.unwrap().unwrap().data,
            b("two")
        );
        match keys::key_ttl(&fx.pool, &k).await.unwrap() {
            Ttl::Seconds(n) => assert!((95..=100).contains(&n), "ttl drifted to {n}"),
            other => panic!("TTL was lost: {other:?}"),
        }

        let plain = fx.key("plain");
        string::set(&fx.pool, &plain, b"a", SetMode::Overwrite)
            .await
            .unwrap();
        string::set_keep_ttl_fallback(&fx.pool, &plain, b"b")
            .await
            .unwrap();
        assert_eq!(
            keys::key_ttl(&fx.pool, &plain).await.unwrap(),
            Ttl::Persistent
        );

        let fresh = fx.key("fresh");
        string::set_keep_ttl_fallback(&fx.pool, &fresh, b"c")
            .await
            .unwrap();
        assert_eq!(
            string::get(&fx.pool, &fresh).await.unwrap().unwrap().data,
            b("c")
        );
        assert_eq!(
            keys::key_ttl(&fx.pool, &fresh).await.unwrap(),
            Ttl::Persistent
        );
    })
    .await;
}

/// 真正的旧版服务器(`redis:5`,`scripts/test-redis.sh test-old` 起的临时容器,127.0.0.1:6393):
/// `SET KEEPTTL` 会返回语法错误,`SetMode::KeepTtl` 要自动回退并保住 TTL。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs old redis (scripts/test-redis.sh test-old)"]
async fn old_redis_keepttl_falls_back_automatically() {
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let c = old_redis_creds();
        let env = TestEnv::with(
            &c.host,
            c.port,
            c.username.as_deref(),
            c.password.as_deref(),
        );
        let pool = env.manager.pool(env.id).await.unwrap();
        // 确认真的连到了旧版服务器,而不是 8.0
        let info: String = pool
            .custom(command("INFO"), vec![arg(b"server")])
            .await
            .unwrap();
        let version = info
            .lines()
            .find_map(|l| l.trim().strip_prefix("redis_version:"))
            .expect("redis_version")
            .to_string();
        assert!(
            version.starts_with("5."),
            "expected a 5.x server, got {version}"
        );
        // 旧版确实不认 KEEPTTL(这条证明回退不是白做)
        let raw: std::result::Result<fred::types::Value, _> = pool
            .custom(
                command("SET"),
                vec![arg(b"xredis_test:probe"), arg(b"v"), arg(b"KEEPTTL")],
            )
            .await;
        assert!(
            raw.as_ref()
                .err()
                .is_some_and(|e| e.details().to_lowercase().contains("syntax error")),
            "a {version} server should reject KEEPTTL with a syntax error: {raw:?}"
        );

        let prefix = unique_prefix();
        let k = format!("{prefix}k").into_bytes();
        string::set(&pool, &k, b"one", SetMode::Overwrite)
            .await
            .unwrap();
        keys::set_ttl(&pool, &k, 100).await.unwrap();
        string::set(&pool, &k, b"two", SetMode::KeepTtl)
            .await
            .unwrap();
        assert_eq!(
            string::get(&pool, &k).await.unwrap().unwrap().data,
            b("two")
        );
        match keys::key_ttl(&pool, &k).await.unwrap() {
            Ttl::Seconds(n) => assert!((95..=100).contains(&n), "ttl drifted to {n}"),
            other => panic!("TTL was lost on {version}: {other:?}"),
        }
        keys::delete_keys(&pool, &[Bytes::from(k)]).await.unwrap();
        env.manager.close(env.id).await.unwrap();
    })
    .await;
}
