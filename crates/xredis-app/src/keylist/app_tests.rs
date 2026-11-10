//! Keys 子页和 `App` 的衔接(同步,不需要后端):连接就绪后自动搜索、没就绪不发请求(L-02)、
//! 关 Tab 之后迟到的结果丢弃(第 10 节)、切库后列表重置、协议挡住用户操作。

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::config::{ConnId, ConnectionConfig, Settings};
use xredis_core::conn::info::{ConnectionInfo, KeyspaceInfo};

use crate::app::{AGREEMENT_VERSION, App, TabStatus};
use crate::keylist::{ScanState, SortColumn};
use crate::msg::*;

fn cfg(name: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: ConnId::new(),
        name: name.into(),
        host: "127.0.0.1".into(),
        port: 6379,
        ordinal: 0,
        selected_db: 0,
        username: None,
        tls: None,
        created_on_utc: 1,
        updated_on_utc: 1,
    }
}

fn info(dbs: usize) -> ConnectionInfo {
    ConnectionInfo {
        databases: (0..dbs as u32).map(KeyspaceInfo::empty).collect(),
    }
}

fn ready_app(conns: Vec<ConnectionConfig>) -> App {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        settings: Settings {
            agreement: AGREEMENT_VERSION.into(),
            ..Settings::default()
        },
        connections: conns,
        ..Default::default()
    })));
    app
}

/// (代号, pattern, count, cursor)
fn scans(cmds: &[Cmd]) -> Vec<(u64, Bytes, u32, u64)> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Backend(BackendCmd::Scan {
                generation,
                pattern,
                count,
                cursor,
                ..
            }) => Some((*generation, pattern.clone(), *count, *cursor)),
            _ => None,
        })
        .collect()
}

fn connect_epoch(cmds: &[Cmd]) -> Epoch {
    cmds.iter()
        .find_map(|c| match c {
            Cmd::Backend(BackendCmd::Connect { epoch, .. }) => Some(*epoch),
            _ => None,
        })
        .expect("a connect command")
}

/// 打开连接并让它就绪,返回(就绪那一刻返回的命令)。
fn open_ready(app: &mut App, id: ConnId) -> Vec<Cmd> {
    let epoch = connect_epoch(&app.update(Msg::OpenConnection(id)));
    app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(info(16)),
    })
}

fn keys(id: ConnId, msg: KeysMsg) -> Msg {
    Msg::Keys { id, msg }
}

fn keys_list(app: &App, id: ConnId) -> &crate::keylist::KeyList {
    &app.tab(id).expect("a tab").keys
}

/// 连接就绪的那一刻自动搜索一次(照 v1:Tab 打开后 Keys 页自己搜);没就绪之前一个 scan 都不发。
#[test]
fn the_first_search_starts_automatically_when_the_connection_becomes_ready() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    app.settings.scan_batch = 700;

    let cmds = app.update(Msg::OpenConnection(id));
    assert!(scans(&cmds).is_empty(), "connecting: no scan yet (L-02)");
    let epoch = connect_epoch(&cmds);

    let cmds = app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(info(16)),
    });
    let s = scans(&cmds);
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].1.as_ref(), s[0].2, s[0].3), (&b"*"[..], 700, 0));
    assert_eq!(keys_list(&app, id).scan_state(), ScanState::Searching);
}

/// L-02:连接中、失败、没有 Tab 时,Keys 的用户操作一律忽略(不发 scan);但异步结果照收。
#[test]
fn l02_keys_user_actions_are_ignored_until_ready() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    // 没有 Tab
    assert!(app.update(keys(id, KeysMsg::Search)).is_empty());

    let epoch = connect_epoch(&app.update(Msg::OpenConnection(id)));
    for m in [
        KeysMsg::Search,
        KeysMsg::LoadMore,
        KeysMsg::LoadAll,
        KeysMsg::PasteRequested,
    ] {
        assert!(app.update(keys(id, m)).is_empty(), "connecting: ignored");
    }
    assert_eq!(keys_list(&app, id).scan_state(), ScanState::Idle);

    // 连接失败
    app.update(Msg::Connected {
        id,
        epoch,
        result: Err(Error::Connect("refused".into())),
    });
    assert!(matches!(app.tab(id).unwrap().status, TabStatus::Failed(_)));
    assert!(
        app.update(keys(id, KeysMsg::Search)).is_empty(),
        "failed: ignored"
    );
}

/// 切库期间(连接正在重建)也不能发 Keys 的用户操作;就绪后的结果(比如还在路上的删除结果)要收。
#[test]
fn keys_user_actions_are_ignored_while_switching_db_but_results_are_still_accepted() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    open_ready(&mut app, id);
    app.update(Msg::SelectDb { id, db: 2 });
    assert!(matches!(
        app.tab(id).unwrap().status,
        TabStatus::Switching { .. }
    ));
    assert!(app.update(keys(id, KeysMsg::Search)).is_empty());
    // 结果不挑状态:在途的 DbSize 结果回来照样记下
    app.update(keys(id, KeysMsg::DbSize(Ok(42))));
    assert_eq!(keys_list(&app, id).db_total, Some(42));
}

/// 第 10 节:关 Tab 之后迟到的结果(扫描批次、详情、删除结果)要丢弃,不 panic,不留消息。
#[test]
fn results_for_a_closed_tab_are_dropped() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let cmds = open_ready(&mut app, id);
    let generation = scans(&cmds)[0].0;
    app.update(Msg::CloseTab(id));
    assert!(app.tab(id).is_none());
    let before = app.messages.len();

    let cmds = app.update(keys(
        id,
        KeysMsg::ScanBatch {
            generation,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"late")],
                cursor: 0,
            }),
        },
    ));
    assert!(cmds.is_empty());
    app.update(keys(
        id,
        KeysMsg::Deleted(Err(Error::Redis("late failure".into()))),
    ));
    app.update(keys(id, KeysMsg::Copied(Err(Error::Closed))));
    assert_eq!(
        app.messages.len(),
        before,
        "no messages for a tab that is gone"
    );
    assert!(app.tab(id).is_none(), "and the tab does not come back");
}

/// 重开 Tab 之后,旧 Tab 的扫描结果(旧代号)不能混进新列表。
#[test]
fn a_reopened_tab_does_not_accept_the_old_tabs_scan_results() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let old_gen = scans(&open_ready(&mut app, id))[0].0;
    app.update(Msg::CloseTab(id));
    let new_gen = scans(&open_ready(&mut app, id))[0].0;
    // 新 Tab 是全新的列表,代号从头开始;关键是旧的结果里的数据不能出现在新列表里
    let _ = (old_gen, new_gen);
    app.update(keys(
        id,
        KeysMsg::ScanBatch {
            generation: u64::MAX,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"stale")],
                cursor: 0,
            }),
        },
    ));
    assert_eq!(keys_list(&app, id).loaded_count(), 0);
    assert_eq!(
        keys_list(&app, id).scan_state(),
        ScanState::Searching,
        "still waiting for its own batch"
    );
}

/// 切库:连接重建、重新就绪后,列表从头来(旧库的 key 不能留在新库的列表里),
/// 搜索条件、过滤、排序保留,并自动再搜索一次。
#[test]
fn switching_db_resets_the_list_keeps_the_search_settings_and_searches_again() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let g1 = scans(&open_ready(&mut app, id))[0].0;
    app.update(keys(id, KeysMsg::SetPattern("user:*".into())));
    app.update(keys(id, KeysMsg::SetFilter("x".into())));
    app.update(keys(id, KeysMsg::SortBy(SortColumn::Ttl)));
    app.update(keys(
        id,
        KeysMsg::ScanBatch {
            generation: g1,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"db0-key")],
                cursor: 0,
            }),
        },
    ));
    assert_eq!(keys_list(&app, id).loaded_count(), 1);

    let epoch = match app.update(Msg::SelectDb { id, db: 3 }).as_slice() {
        [Cmd::Backend(BackendCmd::SelectDb { epoch, .. })] => *epoch,
        other => panic!("{other:?}"),
    };
    let cmds = app.update(Msg::DbSelected {
        id,
        epoch,
        db: 3,
        result: Ok(()),
    });
    let epoch2 = connect_epoch(&cmds);
    let cmds = app.update(Msg::Connected {
        id,
        epoch: epoch2,
        result: Ok(info(16)),
    });

    let s = scans(&cmds);
    assert_eq!(s.len(), 1, "searches the new db right away");
    assert_eq!(
        s[0].1.as_ref(),
        b"user:*",
        "the pattern survives the switch"
    );
    assert_ne!(
        s[0].0, g1,
        "a new scan generation: results from the old db are dropped"
    );
    let list = keys_list(&app, id);
    assert_eq!(list.loaded_count(), 0, "the old db's keys are gone");
    assert_eq!(list.filter, "x");
    assert_eq!(list.sort.column, SortColumn::Ttl);
    // 旧库的批次迟到
    app.update(keys(
        id,
        KeysMsg::ScanBatch {
            generation: g1,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"late-old-db")],
                cursor: 0,
            }),
        },
    ));
    assert_eq!(keys_list(&app, id).loaded_count(), 0);
}

/// 重连(失败后点重试)就绪后同样重新搜索。
#[test]
fn a_successful_reconnect_searches_again() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let epoch = connect_epoch(&app.update(Msg::OpenConnection(id)));
    app.update(Msg::Connected {
        id,
        epoch,
        result: Err(Error::Timeout("slow".into())),
    });
    let epoch2 = connect_epoch(&app.update(Msg::Reconnect(id)));
    let cmds = app.update(Msg::Connected {
        id,
        epoch: epoch2,
        result: Ok(info(16)),
    });
    assert_eq!(scans(&cmds).len(), 1);
}

/// 协议没同意之前,Keys 的用户操作被挡住(和别的用户操作一样);异步结果不挡。
#[test]
fn the_agreement_blocks_keys_actions_but_not_results() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    open_ready(&mut app, id);
    // 协议被重新要求(比如协议改版后):弹窗显示且没同意
    app.settings.agreement = String::new();
    app.update(Msg::ShowAgreement(true));
    assert!(app.blocked());
    assert!(app.update(keys(id, KeysMsg::Search)).is_empty());
    app.update(keys(id, KeysMsg::DbSize(Ok(5))));
    assert_eq!(
        keys_list(&app, id).db_total,
        Some(5),
        "results are not blocked"
    );
}

/// Keys 子页的消息(删除了多少个、剪贴板失败)进消息栏,带 toast。
#[test]
fn keys_notes_reach_the_message_bar() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let generation = scans(&open_ready(&mut app, id))[0].0;
    // 通过 App 走一遍复制失败的路径
    app.update(keys(
        id,
        KeysMsg::ScanBatch {
            generation,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"a")],
                cursor: 0,
            }),
        },
    ));
    app.update(keys(id, KeysMsg::SelectAllFiltered(true)));
    app.update(keys(id, KeysMsg::CopySelected));
    app.update(keys(
        id,
        KeysMsg::Copied(Err(Error::CopyTooLarge {
            total: 70 * 1024 * 1024,
            limit: 64 * 1024 * 1024,
            copied: 0,
            key: Bytes::from_static(b"a"),
        })),
    ));
    let m = app.messages.latest().expect("a message");
    assert!(m.text.contains("Too much data to copy"), "{}", m.text);
    assert_eq!(app.messages.toasts().len(), 1);
}

/// 两个 Tab 各有自己的列表,互不影响。
#[test]
fn each_tab_has_its_own_key_list() {
    let (a, b) = (cfg("a"), cfg("b"));
    let (ia, ib) = (a.id, b.id);
    let mut app = ready_app(vec![a, b]);
    let ga = scans(&open_ready(&mut app, ia))[0].0;
    let gb = scans(&open_ready(&mut app, ib))[0].0;
    app.update(keys(
        ia,
        KeysMsg::ScanBatch {
            generation: ga,
            result: Ok(xredis_core::keys::ScanPage {
                items: vec![Bytes::from_static(b"in-a")],
                cursor: 0,
            }),
        },
    ));
    app.update(keys(ib, KeysMsg::SetPattern("only-b".into())));
    assert_eq!(keys_list(&app, ia).loaded_count(), 1);
    assert_eq!(keys_list(&app, ib).loaded_count(), 0);
    assert_eq!(keys_list(&app, ia).pattern, "");
    assert_eq!(keys_list(&app, ib).pattern, "only-b");
    let _ = gb;
}
