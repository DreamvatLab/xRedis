//! Keys 子页的端到端:App 加 Runtime 加假后端(内存 keyspace)。
//! 真的让后端分页返回、制造重复、测详情的并发数;剪贴板在 Harness 里模拟。

use std::time::Duration;

use bytes::Bytes;
use xredis_core::consts::POOL_SIZE;

use crate::keylist::{Busy, ScanState};
use crate::msg::*;
use crate::runtime::tests::Harness;
use crate::testing::FakeBackend;

/// 等到 `quiet_ms` 毫秒内再没有结果回来。
async fn settle(h: &mut Harness, quiet_ms: u64) {
    while h.pump(quiet_ms).await {}
}

fn k(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn list(h: &Harness, id: xredis_core::config::ConnId) -> &crate::keylist::KeyList {
    &h.app.tab(id).expect("a tab").keys
}

fn send(h: &mut Harness, id: xredis_core::config::ConnId, msg: KeysMsg) {
    h.feed(Msg::Keys { id, msg });
}

fn message_texts(h: &Harness) -> Vec<String> {
    h.app.messages.entries().map(|e| e.text.clone()).collect()
}

/// P-10、P-11、P-12:分页扫描、去重(后端每页多返回上一页最后 5 个)、自动续扫上限、All 扫到底。
#[tokio::test(flavor = "multi_thread")]
async fn paged_scan_dedupes_respects_the_auto_scan_limit_and_all_loads_the_rest() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("k", 1000);
    fake.set_scan_overlap(5);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.app.settings.scan_batch = 100;
    h.app.settings.auto_scan_limit = 300;

    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 300).await;
    let l = list(&h, id);
    assert_eq!(
        l.loaded_count(),
        300,
        "3 batches of 100, duplicates ignored"
    );
    assert!(l.has_more());
    assert_eq!(l.scan_state(), ScanState::Idle);

    send(&mut h, id, KeysMsg::LoadMore);
    settle(&mut h, 300).await;
    assert_eq!(
        list(&h, id).loaded_count(),
        400,
        "More loads exactly one batch"
    );

    send(&mut h, id, KeysMsg::LoadAll);
    settle(&mut h, 300).await;
    let l = list(&h, id);
    assert_eq!(l.loaded_count(), 1000, "all of them, each exactly once");
    assert!(!l.has_more());
    let names: std::collections::HashSet<_> = l.view().map(|r| r.display.clone()).collect();
    assert_eq!(names.len(), 1000, "no duplicate rows");
}

/// P-11:搜索条件按 MATCH 过滤;一页可能是空的但游标没归零,要继续扫,直到找到。
#[tokio::test(flavor = "multi_thread")]
async fn a_pattern_that_matches_late_keeps_scanning_through_empty_batches() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("aaa", 300); // 排在前面,不匹配
    fake.put_key(
        b"zzz-target",
        xredis_core::keys::KeyType::String,
        Some(1),
        xredis_core::keys::Ttl::Persistent,
    );
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.app.settings.scan_batch = 50;
    h.app.settings.auto_scan_limit = 0; // 自动续扫关掉:空页继续扫不受它限制

    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    send(&mut h, id, KeysMsg::SetPattern("zzz*".into()));
    send(&mut h, id, KeysMsg::Search);
    settle(&mut h, 300).await;
    let l = list(&h, id);
    assert_eq!(l.loaded_count(), 1, "found it after several empty batches");
    assert_eq!(l.view().next().unwrap().display, "zzz-target");
    assert_eq!(l.scan_state(), ScanState::Idle);
}

/// P-11 Stop:All 扫到一半点 Stop,在途的一批收下,之后不再发;游标保留,More 可以接着扫,All 再扫到底。
#[tokio::test(flavor = "multi_thread")]
async fn stop_in_the_middle_of_all_keeps_the_cursor_and_loading_can_resume() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("s", 2000);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.app.settings.scan_batch = 100;
    h.app.settings.auto_scan_limit = 0;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    assert_eq!(list(&h, id).loaded_count(), 100);

    fake.set_delay(Duration::from_millis(40));
    send(&mut h, id, KeysMsg::LoadAll);
    for _ in 0..3 {
        assert!(h.pump(1000).await);
    }
    assert!(list(&h, id).cancellable());
    send(&mut h, id, KeysMsg::Stop);
    settle(&mut h, 300).await;
    let l = list(&h, id);
    assert_eq!(l.scan_state(), ScanState::Idle);
    assert!(l.has_more(), "stopped before the end");
    let stopped_at = l.loaded_count();
    assert!(stopped_at > 100 && stopped_at < 2000, "{stopped_at}");
    settle(&mut h, 300).await;
    assert_eq!(
        list(&h, id).loaded_count(),
        stopped_at,
        "nothing more arrives after Stop"
    );

    fake.set_delay(Duration::ZERO);
    send(&mut h, id, KeysMsg::LoadMore);
    settle(&mut h, 200).await;
    assert_eq!(
        list(&h, id).loaded_count(),
        stopped_at + 100,
        "More resumes from the kept cursor"
    );
    send(&mut h, id, KeysMsg::LoadAll);
    settle(&mut h, 300).await;
    assert_eq!(list(&h, id).loaded_count(), 2000);
}

/// L-16:100 行同时可见,后端又慢,详情请求同时进行的最大数量正好是连接池大小,不会更多;最后全部加载。
#[tokio::test(flavor = "multi_thread")]
async fn l16_detail_requests_never_exceed_the_pool_size_against_a_slow_backend() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("k", 100);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    assert_eq!(list(&h, id).loaded_count(), 100);

    fake.set_delay(Duration::from_millis(20));
    let visible: Vec<Bytes> = list(&h, id).view().map(|r| r.key.clone()).collect();
    send(&mut h, id, KeysMsg::Visible(visible));
    settle(&mut h, 400).await;

    assert!(
        list(&h, id).view().all(|r| r.loaded().is_some()),
        "every row got its details"
    );
    assert_eq!(
        fake.max_concurrent_details(),
        POOL_SIZE,
        "the limiter lets exactly {POOL_SIZE} through at a time"
    );
}

/// 没开“显示 key 详情”:可见行不会触发任何详情请求。
#[tokio::test(flavor = "multi_thread")]
async fn no_detail_requests_when_show_key_details_is_off() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("k", 10);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.app.settings.show_key_details = false;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    let visible: Vec<Bytes> = list(&h, id).view().map(|r| r.key.clone()).collect();
    send(&mut h, id, KeysMsg::Visible(visible));
    settle(&mut h, 200).await;
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("key_details")),
        "{:?}",
        fake.calls()
    );
    assert!(list(&h, id).view().all(|r| r.loaded().is_none()));
}

/// P-14:删掉的 key(TTL -2)标记为已删除;类型、长度、TTL 都按后端的值显示。
#[tokio::test(flavor = "multi_thread")]
async fn details_show_type_length_and_ttl_and_mark_vanished_keys() {
    use xredis_core::keys::{KeyType, Ttl};
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_key(b"hash1", KeyType::Hash, Some(7), Ttl::Seconds(90));
    fake.put_key(b"str1", KeyType::String, Some(0), Ttl::Persistent);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    // 扫描之后、取详情之前,str1 被别人删掉了
    fake.remove_key(b"str1");
    let visible: Vec<Bytes> = list(&h, id).view().map(|r| r.key.clone()).collect();
    send(&mut h, id, KeysMsg::Visible(visible));
    settle(&mut h, 200).await;
    let l = list(&h, id);
    let hash = l.view().find(|r| r.display == "hash1").unwrap();
    assert_eq!(
        (hash.kind().cloned(), hash.length(), hash.ttl()),
        (Some(KeyType::Hash), Some(7), Some(Ttl::Seconds(90)))
    );
    assert!(hash.openable());
    let gone = l.view().find(|r| r.display == "str1").unwrap();
    assert!(gone.is_deleted(), "ttl -2: marked as deleted");
}

/// P-40:选中几个删除,确认后后端真的少了,列表同步,key 数用 DBSIZE 刷新,提示删了多少。
#[tokio::test(flavor = "multi_thread")]
async fn delete_selected_keys_end_to_end() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("k", 20);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;

    let first5: Vec<Bytes> = list(&h, id).view().take(5).map(|r| r.key.clone()).collect();
    for key in &first5 {
        send(
            &mut h,
            id,
            KeysMsg::Select {
                key: key.clone(),
                on: true,
            },
        );
    }
    send(&mut h, id, KeysMsg::DeleteSelected);
    assert_eq!(list(&h, id).confirm_delete, Some(5));
    send(&mut h, id, KeysMsg::ConfirmDelete);
    assert_eq!(list(&h, id).busy, Some(Busy::Deleting));
    settle(&mut h, 200).await;

    assert_eq!(fake.keyspace_len(), 15);
    let l = list(&h, id);
    assert_eq!(l.loaded_count(), 15);
    assert_eq!(l.db_total, Some(15), "refreshed with DBSIZE");
    assert!(first5.iter().all(|k| !fake.has_key(k)));
    assert!(message_texts(&h).contains(&"5 key(s) deleted.".to_string()));
    assert_eq!(l.busy, None);
}

/// P-41、P-42、L-09:复制三个 key,删掉,粘贴回来(默认不覆盖);再粘贴一次冲突被整体拒绝并列出;
/// 勾上覆盖才成功。剪贴板在 Harness 里模拟。
#[tokio::test(flavor = "multi_thread")]
async fn copy_delete_paste_round_trip_with_the_overwrite_option() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("c", 5);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;

    for name in ["c0", "c1", "c2"] {
        send(
            &mut h,
            id,
            KeysMsg::Select {
                key: k(name),
                on: true,
            },
        );
    }
    send(&mut h, id, KeysMsg::CopySelected);
    settle(&mut h, 200).await;
    assert!(
        h.clipboard
            .as_deref()
            .is_some_and(|t| t.starts_with("REDIS:")),
        "{:?}",
        h.clipboard
    );
    assert!(message_texts(&h).contains(&"Copied 3 key(s) to the clipboard.".to_string()));
    assert_eq!(
        list(&h, id).selected_count(),
        0,
        "cleared after a successful copy"
    );

    // 删掉这三个
    for name in ["c0", "c1", "c2"] {
        send(
            &mut h,
            id,
            KeysMsg::Select {
                key: k(name),
                on: true,
            },
        );
    }
    send(&mut h, id, KeysMsg::DeleteSelected);
    send(&mut h, id, KeysMsg::ConfirmDelete);
    settle(&mut h, 200).await;
    assert_eq!(fake.keyspace_len(), 2);

    // 粘贴回来:默认不覆盖,但没有冲突,成功
    send(&mut h, id, KeysMsg::PasteRequested);
    settle(&mut h, 100).await;
    let p = list(&h, id).paste.clone().expect("a paste confirmation");
    assert!(!p.overwrite, "not overwriting by default");
    assert_eq!((p.total, p.existing), (3, 0), "the three were deleted");
    assert!(!p.can_overwrite(), "nothing to overwrite: option hidden");
    send(&mut h, id, KeysMsg::ConfirmPaste);
    settle(&mut h, 200).await;
    assert_eq!(fake.keyspace_len(), 5);
    assert!(message_texts(&h).contains(&"Pasted 3 key(s) from the clipboard.".to_string()));
    assert_eq!(
        list(&h, id).loaded_count(),
        5,
        "the list was searched again"
    );

    // 再粘贴一次,不覆盖:整体拒绝并列出冲突,一个都没写
    send(&mut h, id, KeysMsg::PasteRequested);
    settle(&mut h, 100).await;
    let p = list(&h, id).paste.clone().expect("a paste confirmation");
    assert_eq!((p.total, p.existing), (3, 3));
    assert!(p.can_overwrite());
    send(&mut h, id, KeysMsg::ConfirmPaste);
    settle(&mut h, 200).await;
    let texts = message_texts(&h);
    let conflict = texts
        .iter()
        .find(|t| t.contains("already exist"))
        .expect("a conflict message");
    assert!(
        conflict.contains("3 of 3") && conflict.contains("c0"),
        "{conflict}"
    );
    assert_eq!(fake.keyspace_len(), 5);

    // 勾上覆盖:成功
    send(&mut h, id, KeysMsg::PasteRequested);
    settle(&mut h, 100).await;
    send(&mut h, id, KeysMsg::SetPasteOverwrite(true));
    send(&mut h, id, KeysMsg::ConfirmPaste);
    settle(&mut h, 200).await;
    assert_eq!(
        texts_with(&h, "Pasted 3 key(s)"),
        2,
        "the overwriting paste succeeded"
    );
}

fn texts_with(h: &Harness, needle: &str) -> usize {
    message_texts(h)
        .iter()
        .filter(|t| t.contains(needle))
        .count()
}

/// L-13:剪贴板是空的(读失败)要提示,不能静默。
#[tokio::test(flavor = "multi_thread")]
async fn l13_pasting_from_an_empty_clipboard_reports_instead_of_failing_silently() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    settle(&mut h, 200).await;
    send(&mut h, id, KeysMsg::PasteRequested);
    settle(&mut h, 100).await;
    assert!(list(&h, id).paste.is_none());
    assert_eq!(list(&h, id).busy, None);
    assert!(
        message_texts(&h)
            .iter()
            .any(|t| t.contains("Could not read the clipboard")
                && t.contains("the clipboard is empty")),
        "{:?}",
        message_texts(&h)
    );
}

/// L-02(端到端):连接一直建不起来时,Keys 的操作不会发出任何 scan。
#[tokio::test(flavor = "multi_thread")]
async fn l02_no_scan_is_ever_sent_while_the_connection_is_still_connecting() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("dead-host", 0);
    fake.hang("connect");
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    send(&mut h, id, KeysMsg::Search);
    send(&mut h, id, KeysMsg::LoadAll);
    settle(&mut h, 300).await;
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("scan_keys")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(list(&h, id).scan_state(), ScanState::Idle);
}

/// 关 Tab 时扫描还在进行:Tab 立刻没了,迟到的批次丢弃,不留消息。
#[tokio::test(flavor = "multi_thread")]
async fn closing_the_tab_in_the_middle_of_a_scan_drops_the_late_batches() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.put_many("k", 500);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.app.settings.scan_batch = 50;
    h.feed(Msg::OpenConnection(id));
    assert!(h.pump(1000).await, "connected");
    fake.set_delay(Duration::from_millis(80));
    h.feed(Msg::CloseTab(id));
    assert!(h.app.tab(id).is_none());
    let before = h.app.messages.len();
    settle(&mut h, 400).await;
    assert!(
        h.app.tab(id).is_none(),
        "late scan batches do not bring the tab back"
    );
    assert_eq!(h.app.messages.len(), before);
}
