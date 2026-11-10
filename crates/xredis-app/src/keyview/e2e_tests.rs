//! 打开的 key 的端到端:App 加 Runtime 加假后端(内存里的 String 和成员)。
//! 真的走一遍:打开 → 读值或扫成员 → 编辑 → 保存 → 列表和成员表刷新。

use std::time::Duration;

use bytes::Bytes;
use xredis_core::config::ConnId;
use xredis_core::keys::{KeyType, Ttl};
use xredis_core::members::{Member, MemberOp, WriteOp};
use xredis_core::types::list::Side;

use super::table::TableMsg;
use super::{KeyView, ViewMsg};
use crate::keylist::KeysMsg;
use crate::msg::*;
use crate::runtime::tests::Harness;
use crate::testing::FakeBackend;

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn view(h: &Harness, id: ConnId) -> Option<&KeyView> {
    h.app.tab(id).expect("a tab").view.as_ref()
}

fn send(h: &mut Harness, id: ConnId, msg: ViewMsg) {
    h.feed(Msg::View { id, msg });
}

fn texts(h: &Harness) -> Vec<String> {
    h.app.messages.entries().map(|e| e.text.clone()).collect()
}

fn hash_rows(n: usize) -> Vec<Member> {
    (0..n)
        .map(|i| Member::Hash {
            field: b(&format!("字段{i}")),
            value: b(&format!("值{i}")),
        })
        .collect()
}

async fn ready(fake: &std::sync::Arc<FakeBackend>) -> (Harness, ConnId) {
    let id = fake.add_connection("a", 0);
    let mut h = Harness::new(fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    h.settle(200).await;
    (h, id)
}

fn listed(h: &Harness, id: ConnId, name: &str) -> bool {
    h.app
        .tab(id)
        .unwrap()
        .keys
        .view()
        .any(|r| r.display == name)
}

/// P-21、第 16 节备忘:改 String 的值写进去了,TTL 还在(KeepTtl),key 列表那一行的长度刷新。
#[tokio::test(flavor = "multi_thread")]
async fn editing_a_string_keeps_its_ttl_and_refreshes_the_row() {
    let fake = FakeBackend::new();
    fake.put_string("文章".as_bytes(), "旧内容".as_bytes());
    fake.put_key(
        "文章".as_bytes(),
        KeyType::String,
        Some(9),
        Ttl::Seconds(500),
    );
    let (mut h, id) = ready(&fake).await;

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("文章"),
            kind: KeyType::String,
        },
    );
    h.settle(100).await;
    let v = view(&h, id).expect("opened");
    let e = v.editor.as_ref().unwrap();
    assert!(!e.loading && e.value.text == "旧内容");
    assert_eq!(v.details.as_ref().unwrap().ttl, Ttl::Seconds(500));

    send(&mut h, id, ViewMsg::SetValue("新内容更长一些".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    assert!(view(&h, id).is_none(), "the editor closed");
    assert_eq!(
        fake.string_value("文章".as_bytes()).unwrap(),
        b("新内容更长一些")
    );
    assert_eq!(
        fake.key_ttl("文章".as_bytes()),
        Some(Ttl::Seconds(500)),
        "the TTL survived the edit"
    );
    assert!(texts(&h).contains(&"Saved.".to_string()));
    let row = h
        .app
        .tab(id)
        .unwrap()
        .keys
        .view()
        .find(|r| r.display == "文章")
        .unwrap();
    assert_eq!(row.length(), Some(21), "the row shows the new length");
}

/// P-22、P-23:成员表加载、改 field 名加值、新增(重名报错)、删除选中。
#[tokio::test(flavor = "multi_thread")]
async fn hash_members_end_to_end() {
    let fake = FakeBackend::new();
    fake.put_members("用户".as_bytes(), KeyType::Hash, hash_rows(5));
    let (mut h, id) = ready(&fake).await;
    h.app.settings.scan_batch = 2;

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("用户"),
            kind: KeyType::Hash,
        },
    );
    h.settle(100).await;
    let t = view(&h, id).unwrap().table.as_ref().unwrap();
    assert_eq!(
        t.loaded_count(),
        5,
        "auto-scan reached the end: 3 batches of 2"
    );
    assert!(!t.has_more());
    assert_eq!(t.loaded_percent(), 100.0);

    // 改名加改值
    send(&mut h, id, ViewMsg::OpenMember(b("字段1")));
    send(&mut h, id, ViewMsg::SetMemberName("新字段".into()));
    send(&mut h, id, ViewMsg::SetValue("新值".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    let rows = fake.member_rows("用户".as_bytes());
    assert!(
        rows.contains(&Member::Hash {
            field: b("新字段"),
            value: b("新值")
        }),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|r| r.name() == b("字段1")));
    let v = view(&h, id).unwrap();
    assert!(v.editor.is_none(), "the member dialog closed");
    assert_eq!(v.table.as_ref().unwrap().loaded_count(), 5, "reloaded");
    assert!(v.table.as_ref().unwrap().find(&b("新字段")).is_some());

    // 新增一个已存在的 field:报错,编辑器留着
    send(&mut h, id, ViewMsg::NewMember);
    send(&mut h, id, ViewMsg::SetMemberName("字段0".into()));
    send(&mut h, id, ViewMsg::SetValue("x".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    assert!(view(&h, id).unwrap().editor.is_some());
    assert!(
        texts(&h)
            .iter()
            .any(|t| t.contains("字段0") && t.contains("already exists")),
        "{:?}",
        texts(&h)
    );
    assert_eq!(
        fake.member_rows("用户".as_bytes()).len(),
        5,
        "nothing was overwritten"
    );
    send(&mut h, id, ViewMsg::SetMemberName("字段9".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    assert_eq!(fake.member_rows("用户".as_bytes()).len(), 6);

    // 删除选中的两个
    for n in ["字段0", "字段2"] {
        send(
            &mut h,
            id,
            ViewMsg::Table(TableMsg::Select {
                name: b(n),
                on: true,
            }),
        );
    }
    send(&mut h, id, ViewMsg::Table(TableMsg::DeleteSelected));
    send(&mut h, id, ViewMsg::Table(TableMsg::ConfirmDelete));
    h.settle(150).await;
    assert_eq!(fake.member_rows("用户".as_bytes()).len(), 4);
    let v = view(&h, id).unwrap();
    assert_eq!(v.table.as_ref().unwrap().loaded_count(), 4);
    assert_eq!(
        v.details.as_ref().unwrap().length,
        Some(4),
        "length is read again"
    );
    assert!(texts(&h).iter().any(|t| t.contains("Deleted 2 member(s)")));
}

/// P-24、P-26:List 头插和改值、ZSet 改分数和改名。
#[tokio::test(flavor = "multi_thread")]
async fn list_and_zset_end_to_end() {
    let fake = FakeBackend::new();
    fake.put_members(
        "队列".as_bytes(),
        KeyType::List,
        (0..3)
            .map(|i| Member::List {
                index: i,
                value: b(&format!("v{i}")),
            })
            .collect(),
    );
    fake.put_members(
        "榜".as_bytes(),
        KeyType::ZSet,
        vec![
            Member::ZSet {
                member: b("甲"),
                score: 1.0,
            },
            Member::ZSet {
                member: b("乙"),
                score: 2.0,
            },
        ],
    );
    let (mut h, id) = ready(&fake).await;

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("队列"),
            kind: KeyType::List,
        },
    );
    h.settle(100).await;
    send(&mut h, id, ViewMsg::NewMember);
    send(&mut h, id, ViewMsg::SetValue("头".into()));
    send(&mut h, id, ViewMsg::SetPosition(Side::Head));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    let rows = fake.member_rows("队列".as_bytes());
    assert_eq!(
        rows[0],
        Member::List {
            index: 0,
            value: b("头")
        }
    );
    assert_eq!(rows.len(), 4);
    // 表里看到的下标跟着变了
    assert!(
        view(&h, id)
            .unwrap()
            .table
            .as_ref()
            .unwrap()
            .find(&b("3"))
            .is_some()
    );

    // 改下标 2 的值
    send(&mut h, id, ViewMsg::OpenMember(b("2")));
    send(&mut h, id, ViewMsg::SetValue("改".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    assert_eq!(
        fake.member_rows("队列".as_bytes())[2],
        Member::List {
            index: 2,
            value: b("改")
        }
    );
    send(&mut h, id, ViewMsg::Close);

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("榜"),
            kind: KeyType::ZSet,
        },
    );
    h.settle(100).await;
    send(&mut h, id, ViewMsg::OpenMember(b("甲")));
    send(&mut h, id, ViewMsg::SetMemberName("丙".into()));
    send(&mut h, id, ViewMsg::SetValue("9.5".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    let rows = fake.member_rows("榜".as_bytes());
    assert!(
        rows.contains(&Member::ZSet {
            member: b("丙"),
            score: 9.5
        }),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|r| r.name() == b("甲")));
    // ZSet 默认分数降序:丙排第一
    let first = view(&h, id)
        .unwrap()
        .table
        .as_ref()
        .unwrap()
        .view()
        .next()
        .unwrap()
        .name();
    assert_eq!(first, b("丙"));
}

/// P-20:新建 key:成功后列表重新搜索看得到;已存在报错不覆盖。
#[tokio::test(flavor = "multi_thread")]
async fn creating_a_key_end_to_end() {
    let fake = FakeBackend::new();
    fake.put_string("已有".as_bytes(), b"keep");
    let (mut h, id) = ready(&fake).await;

    send(&mut h, id, ViewMsg::NewKey(KeyType::Hash));
    assert!(view(&h, id).unwrap().is_creating());
    send(&mut h, id, ViewMsg::SetKeyName("新键".into()));
    send(&mut h, id, ViewMsg::SetMemberName("字段".into()));
    send(&mut h, id, ViewMsg::SetValue("值".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(200).await;
    assert!(view(&h, id).is_none());
    assert_eq!(fake.member_rows("新键".as_bytes()).len(), 1);
    assert!(listed(&h, id, "新键"), "the key list was searched again");

    // 同名的已有 key:报错,值不变,编辑器还在
    send(&mut h, id, ViewMsg::NewKey(KeyType::String));
    send(&mut h, id, ViewMsg::SetKeyName("已有".into()));
    send(&mut h, id, ViewMsg::SetValue("overwritten?".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(150).await;
    assert!(view(&h, id).is_some());
    assert_eq!(fake.string_value("已有".as_bytes()).unwrap(), b("keep"));
    assert!(
        texts(&h)
            .iter()
            .any(|t| t.contains("已有") && t.contains("already exists"))
    );
}

/// P-27:改 key 名(对话框),成功后回到列表,新名字在,旧名字不在。
#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_key_from_the_members_view() {
    let fake = FakeBackend::new();
    fake.put_members(
        "旧键".as_bytes(),
        KeyType::Set,
        vec![Member::Set {
            member: b("成员")
        }],
    );
    fake.put_string("占用".as_bytes(), b"x");
    let (mut h, id) = ready(&fake).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("旧键"),
            kind: KeyType::Set,
        },
    );
    h.settle(100).await;

    send(&mut h, id, ViewMsg::OpenRename);
    send(&mut h, id, ViewMsg::SetRenameText("占用".into()));
    send(&mut h, id, ViewMsg::ConfirmRename);
    h.settle(150).await;
    let v = view(&h, id).expect("still open after the rejection");
    assert!(
        v.rename
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("占用")
    );
    assert!(
        fake.has_key("旧键".as_bytes()),
        "RENAMENX did not overwrite"
    );

    send(&mut h, id, ViewMsg::SetRenameText("新键".into()));
    send(&mut h, id, ViewMsg::ConfirmRename);
    h.settle(200).await;
    assert!(view(&h, id).is_none());
    assert!(fake.has_key("新键".as_bytes()) && !fake.has_key("旧键".as_bytes()));
    assert!(listed(&h, id, "新键") && !listed(&h, id, "旧键"));
}

/// P-28:TTL 对话框:0 根本不发给后端;-1 持久化;正数设置。
#[tokio::test(flavor = "multi_thread")]
async fn the_ttl_dialog_end_to_end() {
    let fake = FakeBackend::new();
    fake.put_members("会话".as_bytes(), KeyType::Hash, hash_rows(1));
    let (mut h, id) = ready(&fake).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("会话"),
            kind: KeyType::Hash,
        },
    );
    h.settle(100).await;

    send(&mut h, id, ViewMsg::OpenTtl);
    send(&mut h, id, ViewMsg::SetTtlText("0".into()));
    send(&mut h, id, ViewMsg::ConfirmTtl);
    h.settle(100).await;
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("apply_writes")),
        "TTL 0 never reaches the server: {:?}",
        fake.calls()
    );
    assert!(view(&h, id).unwrap().ttl.as_ref().unwrap().error.is_some());

    send(&mut h, id, ViewMsg::SetTtlText("60".into()));
    send(&mut h, id, ViewMsg::ConfirmTtl);
    h.settle(150).await;
    assert_eq!(fake.key_ttl("会话".as_bytes()), Some(Ttl::Seconds(60)));
    let v = view(&h, id).unwrap();
    assert!(v.ttl.is_none());
    assert_eq!(
        v.details.as_ref().unwrap().ttl,
        Ttl::Seconds(60),
        "details were read again"
    );

    send(&mut h, id, ViewMsg::OpenTtl);
    assert_eq!(view(&h, id).unwrap().ttl.as_ref().unwrap().text, "60");
    send(&mut h, id, ViewMsg::SetTtlText("-1".into()));
    send(&mut h, id, ViewMsg::ConfirmTtl);
    h.settle(150).await;
    assert_eq!(fake.key_ttl("会话".as_bytes()), Some(Ttl::Persistent));
}

/// P-29:二进制的值和成员名只读,保存什么也不发给后端。
#[tokio::test(flavor = "multi_thread")]
async fn binary_values_and_names_are_read_only_end_to_end() {
    let fake = FakeBackend::new();
    let bin = Bytes::from_static(&[0xff, 0x00, b'a']);
    fake.put_string("二进制".as_bytes(), &bin);
    fake.put_members(
        "哈希".as_bytes(),
        KeyType::Hash,
        vec![Member::Hash {
            field: bin.clone(),
            value: b("v"),
        }],
    );
    let (mut h, id) = ready(&fake).await;

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("二进制"),
            kind: KeyType::String,
        },
    );
    h.settle(100).await;
    assert!(view(&h, id).unwrap().editor.as_ref().unwrap().read_only());
    send(&mut h, id, ViewMsg::SetValue("changed".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(100).await;
    assert_eq!(
        fake.string_value("二进制".as_bytes()).unwrap(),
        bin,
        "the bytes are untouched"
    );
    send(&mut h, id, ViewMsg::Close);

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("哈希"),
            kind: KeyType::Hash,
        },
    );
    h.settle(100).await;
    // 二进制名字用字节选中、打开
    send(&mut h, id, ViewMsg::OpenMember(bin.clone()));
    assert!(view(&h, id).unwrap().editor.as_ref().unwrap().read_only());
    send(&mut h, id, ViewMsg::SetValue("changed".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(100).await;
    assert!(fake.applied().is_empty(), "nothing was written");
    send(&mut h, id, ViewMsg::CloseEditor);

    // 但二进制名字的成员可以删除(全程用字节)
    send(
        &mut h,
        id,
        ViewMsg::Table(TableMsg::Select {
            name: bin.clone(),
            on: true,
        }),
    );
    send(&mut h, id, ViewMsg::Table(TableMsg::DeleteSelected));
    send(&mut h, id, ViewMsg::Table(TableMsg::ConfirmDelete));
    h.settle(150).await;
    assert!(fake.member_rows("哈希".as_bytes()).is_empty());
    assert_eq!(
        fake.applied(),
        vec![WriteOp::Member {
            key: b("哈希"),
            op: MemberOp::HashDelete(vec![bin])
        }]
    );
}

/// P-30:大 String 只读前面一部分,只读并记着真实长度。
#[tokio::test(flavor = "multi_thread")]
async fn a_big_string_is_read_only_end_to_end() {
    let fake = FakeBackend::new();
    fake.put_big_string("大".as_bytes(), &vec![b'x'; 1024], 9 * 1024 * 1024);
    let (mut h, id) = ready(&fake).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("大"),
            kind: KeyType::String,
        },
    );
    h.settle(100).await;
    let e = view(&h, id).unwrap().editor.as_ref().unwrap();
    assert!(e.read_only() && e.truncated == Some(9 * 1024 * 1024));
    send(&mut h, id, ViewMsg::SetValue("y".into()));
    send(&mut h, id, ViewMsg::Save);
    h.settle(100).await;
    assert!(fake.applied().is_empty());
}

/// Stream 和模块类型不能打开:提示,没有视图,一个后端请求都不发。
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_cannot_be_opened() {
    let fake = FakeBackend::new();
    fake.put_key("流".as_bytes(), KeyType::Stream, Some(3), Ttl::Persistent);
    let (mut h, id) = ready(&fake).await;
    let before = fake.calls().len();
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("流"),
            kind: KeyType::Stream,
        },
    );
    h.settle(100).await;
    assert!(view(&h, id).is_none());
    assert_eq!(fake.calls().len(), before, "no request");
    assert!(
        texts(&h)
            .iter()
            .any(|t| t.contains("Stream") && t.contains("cannot be opened"))
    );
}

/// L-02:连接还没就绪时,打开 key 的操作被忽略,什么请求都不发。
#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_opened_while_the_connection_is_not_ready() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.hang("connect");
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    h.settle(100).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("k"),
            kind: KeyType::Hash,
        },
    );
    send(&mut h, id, ViewMsg::NewKey(KeyType::String));
    h.settle(100).await;
    assert!(view(&h, id).is_none());
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| c.starts_with("scan_members") || c.starts_with("read_string"))
    );
}

/// 关视图、换视图之后,旧视图迟到的结果丢弃,不会冒出旧数据。
#[tokio::test(flavor = "multi_thread")]
async fn late_results_of_a_closed_or_replaced_view_are_dropped() {
    let fake = FakeBackend::new();
    fake.put_members("甲".as_bytes(), KeyType::Hash, hash_rows(3));
    fake.put_members(
        "乙".as_bytes(),
        KeyType::Set,
        vec![Member::Set {
            member: b("只有一个"),
        }],
    );
    let (mut h, id) = ready(&fake).await;
    fake.set_delay(Duration::from_millis(150));

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("甲"),
            kind: KeyType::Hash,
        },
    );
    send(&mut h, id, ViewMsg::Close);
    h.settle(400).await;
    assert!(
        view(&h, id).is_none(),
        "the late batch did not resurrect the view"
    );

    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("甲"),
            kind: KeyType::Hash,
        },
    );
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("乙"),
            kind: KeyType::Set,
        },
    );
    h.settle(500).await;
    let v = view(&h, id).unwrap();
    assert_eq!(v.title_key(), "乙");
    assert_eq!(
        v.table.as_ref().unwrap().loaded_count(),
        1,
        "only the second view's rows"
    );
}

/// 切库(连接重建)之后,打开着的 key 视图关掉,key 列表重新开始。
#[tokio::test(flavor = "multi_thread")]
async fn switching_db_closes_the_open_view() {
    let fake = FakeBackend::new();
    fake.put_members("甲".as_bytes(), KeyType::Hash, hash_rows(1));
    let (mut h, id) = ready(&fake).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("甲"),
            kind: KeyType::Hash,
        },
    );
    h.settle(100).await;
    assert!(view(&h, id).is_some());
    h.feed(Msg::SelectDb { id, db: 1 });
    h.settle(300).await;
    assert!(view(&h, id).is_none());
}

/// 刷新:重读详情,成员表从头扫(看得到别的客户端的改动)。
#[tokio::test(flavor = "multi_thread")]
async fn refresh_rereads_everything() {
    let fake = FakeBackend::new();
    fake.put_members("甲".as_bytes(), KeyType::Hash, hash_rows(2));
    let (mut h, id) = ready(&fake).await;
    send(
        &mut h,
        id,
        ViewMsg::Open {
            key: b("甲"),
            kind: KeyType::Hash,
        },
    );
    h.settle(100).await;
    fake.put_members("甲".as_bytes(), KeyType::Hash, hash_rows(4));
    send(&mut h, id, ViewMsg::Refresh);
    h.settle(100).await;
    let v = view(&h, id).unwrap();
    assert_eq!(v.table.as_ref().unwrap().loaded_count(), 4);
    assert_eq!(v.details.as_ref().unwrap().length, Some(4));
    // 顺带验证 Keys 的消息通道不受影响
    h.feed(Msg::Keys {
        id,
        msg: KeysMsg::Search,
    });
    h.settle(100).await;
}
