//! 成员读写入口的测试。都需要 Redis(`scripts/test-redis.sh test`),每个测试用唯一前缀,
//! 清理时只扫这个前缀再 DEL。不执行 FLUSHDB、FLUSHALL、CONFIG SET、SHUTDOWN。

use bytes::Bytes;
use fred::clients::Pool;

use super::*;
use crate::conn::testutil::*;
use crate::keys::{self, Ttl};
use crate::types::{hash, list, set, string, zset};

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

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn member_op(key: &Bytes, op: MemberOp) -> WriteOp {
    WriteOp::Member {
        key: key.clone(),
        op,
    }
}

/// 第 16 节备忘(R-07b):编辑已有 String 的值用 KeepTtl,TTL 还在;普通 Overwrite 会清掉它(对照)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn editing_a_string_with_keepttl_preserves_the_ttl() {
    run(|fx| async move {
        let k = fx.key("会话");
        string::set(&fx.pool, &k, b"1", SetMode::Overwrite)
            .await
            .unwrap();
        keys::set_ttl(&fx.pool, &k, 1000).await.unwrap();
        apply(
            &fx.pool,
            &[WriteOp::SetString {
                key: k.clone(),
                value: b("2"),
                mode: SetMode::KeepTtl,
            }],
        )
        .await
        .unwrap();
        let ttl = keys::key_ttl(&fx.pool, &k).await.unwrap();
        assert!(
            matches!(ttl, Ttl::Seconds(n) if (990..=1000).contains(&n)),
            "{ttl:?}"
        );
        assert_eq!(
            &string::get(&fx.pool, &k).await.unwrap().unwrap().data[..],
            b"2"
        );
    })
    .await;
}

/// 改名加改值是一串有序的步骤:先 RENAMENX 再写;目标已存在时第一步就失败,`done` 是 0,什么也没动。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn rename_then_write_stops_at_the_first_failure() {
    run(|fx| async move {
        let (old, taken, new) = (fx.key("旧"), fx.key("占用"), fx.key("新"));
        string::set(&fx.pool, &old, b"1", SetMode::Overwrite)
            .await
            .unwrap();
        string::set(&fx.pool, &taken, b"keep", SetMode::Overwrite)
            .await
            .unwrap();

        let ops = |target: &Bytes| {
            vec![
                WriteOp::RenameKey {
                    old: old.clone(),
                    new: target.clone(),
                },
                WriteOp::SetString {
                    key: target.clone(),
                    value: b("2"),
                    mode: SetMode::KeepTtl,
                },
            ]
        };
        let fail = apply(&fx.pool, &ops(&taken)).await.unwrap_err();
        assert_eq!(fail.done, 0);
        assert!(matches!(
            fail.error,
            Error::AlreadyExists {
                entity: Entity::Key,
                ..
            }
        ));
        assert_eq!(
            &string::get(&fx.pool, &taken).await.unwrap().unwrap().data[..],
            b"keep"
        );
        assert_eq!(
            &string::get(&fx.pool, &old).await.unwrap().unwrap().data[..],
            b"1"
        );

        apply(&fx.pool, &ops(&new)).await.unwrap();
        assert!(!keys::exists(&fx.pool, &old).await.unwrap());
        assert_eq!(
            &string::get(&fx.pool, &new).await.unwrap().unwrap().data[..],
            b"2"
        );

        // 第二步失败:`done` 是 1(key 已经改名了)
        let wrong_type = fx.key("哈希");
        hash::set(&fx.pool, &wrong_type, b"f", b"v").await.unwrap();
        let fail = apply(
            &fx.pool,
            &[
                WriteOp::RenameKey {
                    old: new.clone(),
                    new: fx.key("再改"),
                },
                WriteOp::Member {
                    key: wrong_type.clone(),
                    op: MemberOp::SetAdd(b("x")),
                },
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(fail.done, 1);
        assert!(keys::exists(&fx.pool, &fx.key("再改")).await.unwrap());
    })
    .await;
}

/// P-20:新建 key 五种类型都不覆盖已有的 key(包括已有 key 是别的类型)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn creating_a_key_never_overwrites_anything() {
    run(|fx| async move {
        let contents = [
            NewContent::String(b("v")),
            NewContent::Hash {
                field: b("f"),
                value: b("v"),
            },
            NewContent::List {
                value: b("v"),
                side: Side::Tail,
            },
            NewContent::Set(b("m")),
            NewContent::ZSet {
                member: b("m"),
                score: 1.0,
            },
        ];
        for (i, content) in contents.iter().enumerate() {
            let k = fx.key(&format!("新{i}"));
            let create = [WriteOp::CreateKey {
                key: k.clone(),
                content: content.clone(),
            }];
            apply(&fx.pool, &create).await.unwrap();
            // 再建一次:报已存在
            let fail = apply(&fx.pool, &create).await.unwrap_err();
            assert!(
                matches!(
                    fail.error,
                    Error::AlreadyExists {
                        entity: Entity::Key,
                        ..
                    }
                ),
                "{content:?}: {:?}",
                fail.error
            );
            assert_eq!(
                keys::key_details(&fx.pool, &k).await.unwrap().length,
                Some(1),
                "{content:?}"
            );
        }
        // 已有一个 String,用 Hash 的内容建同名 key:报已存在,而不是 WRONGTYPE,值不变
        let k = fx.key("占着");
        string::set(&fx.pool, &k, b"keep", SetMode::Overwrite)
            .await
            .unwrap();
        let fail = apply(
            &fx.pool,
            &[WriteOp::CreateKey {
                key: k.clone(),
                content: NewContent::Hash {
                    field: b("f"),
                    value: b("v"),
                },
            }],
        )
        .await
        .unwrap_err();
        assert!(matches!(fail.error, Error::AlreadyExists { .. }));
        assert_eq!(
            &string::get(&fx.pool, &k).await.unwrap().unwrap().data[..],
            b"keep"
        );
    })
    .await;
}

/// 成员表的扫描:四种类型各自的格式;二进制的名字和值原样回来;List 按页取下标不错位。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn scanning_members_of_every_type() {
    run(|fx| async move {
        let bin = Bytes::from_static(&[0xff, 0x00, 0xfe]);
        let h = fx.key("哈希");
        hash::set(&fx.pool, &h, "字段".as_bytes(), "值".as_bytes())
            .await
            .unwrap();
        hash::set(&fx.pool, &h, &bin, &bin).await.unwrap();
        let page = scan_members(&fx.pool, &h, &KeyType::Hash, b"*", 100, 0)
            .await
            .unwrap();
        assert_eq!(page.cursor, 0);
        let mut got: Vec<Member> = page.items;
        got.sort_by_key(|m| m.name());
        assert!(
            got.contains(&Member::Hash {
                field: bin.clone(),
                value: bin.clone()
            }),
            "{got:?}"
        );
        assert!(got.contains(&Member::Hash {
            field: b("字段"),
            value: b("值")
        }));

        let s = fx.key("集合");
        set::add(&fx.pool, &s, &bin).await.unwrap();
        let page = scan_members(&fx.pool, &s, &KeyType::Set, b"*", 100, 0)
            .await
            .unwrap();
        assert_eq!(
            page.items,
            vec![Member::Set {
                member: bin.clone()
            }]
        );

        let z = fx.key("有序");
        zset::add(&fx.pool, &z, "甲".as_bytes(), 1.5).await.unwrap();
        let page = scan_members(&fx.pool, &z, &KeyType::ZSet, b"*", 100, 0)
            .await
            .unwrap();
        assert_eq!(
            page.items,
            vec![Member::ZSet {
                member: b("甲"),
                score: 1.5
            }]
        );

        // List:7 个元素,每页 3 个:0..3、3..6、6..7,游标是下一页的起始下标,最后归零
        let l = fx.key("列表");
        for i in 0..7 {
            list::push(&fx.pool, &l, format!("v{i}").as_bytes(), Side::Tail)
                .await
                .unwrap();
        }
        let mut all = Vec::new();
        let mut cursor = 0;
        let mut pages = 0;
        loop {
            let page = scan_members(&fx.pool, &l, &KeyType::List, b"*", 3, cursor)
                .await
                .unwrap();
            pages += 1;
            all.extend(page.items);
            cursor = page.cursor;
            if cursor == 0 {
                break;
            }
            assert!(pages < 10);
        }
        assert_eq!(pages, 3);
        let want: Vec<Member> = (0..7)
            .map(|i| Member::List {
                index: i,
                value: b(&format!("v{i}")),
            })
            .collect();
        assert_eq!(all, want);
        // 正好整除时最后一页游标也归零
        let page = scan_members(&fx.pool, &l, &KeyType::List, b"*", 7, 0)
            .await
            .unwrap();
        assert_eq!((page.items.len(), page.cursor), (7, 0));
        // 游标越界(列表在两次读之间变短了):空页,归零
        let page = scan_members(&fx.pool, &l, &KeyType::List, b"*", 3, 100)
            .await
            .unwrap();
        assert_eq!((page.items.len(), page.cursor), (0, 0));

        assert!(matches!(
            scan_members(&fx.pool, &l, &KeyType::Stream, b"*", 3, 0).await,
            Err(Error::NotOpenable { .. })
        ));
    })
    .await;
}

/// 成员的写操作:改名加改值(Hash、ZSet)、Set 改名、List 头插和按下标删除、批量删除、
/// 新增已存在报错。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn member_writes_of_every_type() {
    run(|fx| async move {
        let h = fx.key("哈希");
        hash::set(&fx.pool, &h, b"old", b"1").await.unwrap();
        apply(
            &fx.pool,
            &[
                member_op(
                    &h,
                    MemberOp::HashRename {
                        old: b("old"),
                        new: b("new"),
                    },
                ),
                member_op(
                    &h,
                    MemberOp::HashSet {
                        field: b("new"),
                        value: b("2"),
                    },
                ),
            ],
        )
        .await
        .unwrap();
        assert_eq!(hash::get(&fx.pool, &h, b"new").await.unwrap(), Some(b("2")));
        assert_eq!(hash::get(&fx.pool, &h, b"old").await.unwrap(), None);
        let dup = apply(
            &fx.pool,
            &[member_op(
                &h,
                MemberOp::HashAdd {
                    field: b("new"),
                    value: b("x"),
                },
            )],
        )
        .await
        .unwrap_err();
        assert!(matches!(
            dup.error,
            Error::AlreadyExists {
                entity: Entity::HashField,
                ..
            }
        ));
        assert_eq!(
            hash::get(&fx.pool, &h, b"new").await.unwrap(),
            Some(b("2")),
            "not overwritten"
        );

        let z = fx.key("有序");
        zset::add(&fx.pool, &z, b"a", 1.0).await.unwrap();
        apply(
            &fx.pool,
            &[
                member_op(
                    &z,
                    MemberOp::ZRename {
                        old: b("a"),
                        new: b("b"),
                    },
                ),
                member_op(
                    &z,
                    MemberOp::ZSetScore {
                        member: b("b"),
                        score: 9.5,
                    },
                ),
            ],
        )
        .await
        .unwrap();
        assert_eq!(zset::score(&fx.pool, &z, b"b").await.unwrap(), Some(9.5));
        assert_eq!(zset::score(&fx.pool, &z, b"a").await.unwrap(), None);

        let s = fx.key("集合");
        set::add(&fx.pool, &s, b"x").await.unwrap();
        apply(
            &fx.pool,
            &[member_op(
                &s,
                MemberOp::SetRename {
                    old: b("x"),
                    new: b("y"),
                },
            )],
        )
        .await
        .unwrap();
        assert!(set::is_member(&fx.pool, &s, b"y").await.unwrap());
        assert!(!set::is_member(&fx.pool, &s, b"x").await.unwrap());

        let l = fx.key("列表");
        apply(
            &fx.pool,
            &[
                member_op(
                    &l,
                    MemberOp::ListPush {
                        value: b("b"),
                        side: Side::Tail,
                    },
                ),
                member_op(
                    &l,
                    MemberOp::ListPush {
                        value: b("c"),
                        side: Side::Tail,
                    },
                ),
                member_op(
                    &l,
                    MemberOp::ListPush {
                        value: b("a"),
                        side: Side::Head,
                    },
                ),
                member_op(
                    &l,
                    MemberOp::ListSet {
                        index: 1,
                        value: b("B"),
                    },
                ),
            ],
        )
        .await
        .unwrap();
        let all = list::range(&fx.pool, &l, 0, -1).await.unwrap();
        assert_eq!(all, vec![b("a"), b("B"), b("c")]);
        apply(&fx.pool, &[member_op(&l, MemberOp::ListRemove(vec![0, 2]))])
            .await
            .unwrap();
        assert_eq!(
            list::range(&fx.pool, &l, 0, -1).await.unwrap(),
            vec![b("B")]
        );

        // 批量删除
        apply(
            &fx.pool,
            &[member_op(&h, MemberOp::HashDelete(vec![b("new")]))],
        )
        .await
        .unwrap();
        assert!(
            !keys::exists(&fx.pool, &h).await.unwrap(),
            "an empty hash is gone"
        );
    })
    .await;
}

/// TTL 的写操作:-1 持久化,正数设置;0 在 core 被拒绝(不会发 EXPIRE 0 把 key 删掉)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn ttl_writes_follow_the_rules() {
    run(|fx| async move {
        let k = fx.key("会话");
        string::set(&fx.pool, &k, b"1", SetMode::Overwrite)
            .await
            .unwrap();
        apply(
            &fx.pool,
            &[WriteOp::SetTtl {
                key: k.clone(),
                seconds: 60,
            }],
        )
        .await
        .unwrap();
        assert!(matches!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Seconds(n) if n <= 60));
        apply(
            &fx.pool,
            &[WriteOp::SetTtl {
                key: k.clone(),
                seconds: -1,
            }],
        )
        .await
        .unwrap();
        assert_eq!(keys::key_ttl(&fx.pool, &k).await.unwrap(), Ttl::Persistent);
        let fail = apply(
            &fx.pool,
            &[WriteOp::SetTtl {
                key: k.clone(),
                seconds: 0,
            }],
        )
        .await
        .unwrap_err();
        assert!(matches!(fail.error, Error::TtlZero));
        assert!(
            keys::exists(&fx.pool, &k).await.unwrap(),
            "the key was not deleted"
        );
    })
    .await;
}
