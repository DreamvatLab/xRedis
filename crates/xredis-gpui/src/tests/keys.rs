//! Keys 子页的无头测试(R-09a):假后端驱动整个 `MainView`。
//! 点击、打字、回车走真实的事件路径,断言 `App` 的状态和后端调用。
//! 外观(颜色、布局、滚动手感)和 1 万行的流畅度只能试用看;这里只验证能渲染、滚动不崩、给出耗时。

use std::time::Instant;

use gpui_kit::{AppContext as _, TestAppContext};
use xredis_app::Msg;
use xredis_app::keylist::{KeysMsg, ScanState, SortColumn};
use xredis_core::config::ConnId;
use xredis_core::keys::{KeyType, Ttl};

use super::dialogs::{press, type_into};
use super::{Env, click, dispatch, exists, is_ready, read, setup, wait};

/// 改 `App` 里的设置(测试里模拟用户在设置面板里改过)。
fn tweak(cx: &mut TestAppContext, env: &Env, f: impl FnOnce(&mut xredis_app::App)) {
    cx.update_window(env.window, |_, _, cx| {
        env.view.update(cx, |this, _| f(&mut this.app));
    })
    .unwrap();
}

fn keys_of<R>(
    cx: &mut TestAppContext,
    env: &Env,
    id: ConnId,
    f: impl FnOnce(&xredis_app::keylist::KeyList) -> R,
) -> R {
    read(cx, env, |a| f(&a.tab(id).unwrap().keys))
}

/// 打开连接,等到就绪并且扫描结束。
fn open(cx: &mut TestAppContext, env: &Env, id: ConnId) {
    dispatch(cx, env, Msg::OpenConnection(id));
    wait(cx, env, "keys loaded", |a| {
        is_ready(a)
            && a.tab(id)
                .is_some_and(|t| t.keys.scan_state() == ScanState::Idle)
    });
}

fn names(cx: &mut TestAppContext, env: &Env, id: ConnId) -> Vec<String> {
    keys_of(cx, env, id, |k| {
        k.view().map(|r| r.display.clone()).collect()
    })
}

fn small_batches(cx: &mut TestAppContext, env: &Env, batch: u32, auto: u32) {
    tweak(cx, env, |a| {
        a.settings.scan_batch = batch;
        a.settings.auto_scan_limit = auto;
    });
}

// ---------------------------------------------------------------- 页面和搜索 (P-10)

#[gpui_kit::test]
fn the_keys_page_renders_after_connecting_and_the_first_search_runs(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    env.fake.put_many("user:", 5);
    open(cx, &env, ids[0]);
    for id in [
        "keys-page",
        "keys-toolbar",
        "keys-db",
        "keys-search",
        "keys-scan",
        "keys-table",
        "keys-header",
        "keys-select-all",
        "keys-sort-key",
        "keys-sort-type",
        "keys-sort-length",
        "keys-sort-ttl",
        "keys-filter",
        "keys-footer",
        "keys-status",
        "keys-more",
        "keys-all",
        "key-row-0",
        "key-row-4",
    ] {
        assert!(exists(cx, &env, id), "{id}");
    }
    assert!(!exists(cx, &env, "key-row-5"));
    assert!(!exists(cx, &env, "keys-busy"));
    assert!(!exists(cx, &env, "keys-stop"));
    assert!(!exists(cx, &env, "keys-empty"));
    assert_eq!(names(cx, &env, ids[0]).len(), 5);
    // 底部文字来自 App:默认总数取当前库的 key 数(假后端的库 key 数是 0,所以没有百分比)
    assert_eq!(
        read(cx, &env, |a| a.keys_status(ids[0]).unwrap().loaded),
        "Loaded 5 / 0"
    );
}

#[gpui_kit::test]
fn typing_a_pattern_and_scan_or_enter_searches(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("user:", 3);
    env.fake.put_many("order:", 2);
    open(cx, &env, id);
    assert_eq!(names(cx, &env, id).len(), 5);

    type_into(cx, &env, "keys-search", "user:*");
    assert_eq!(keys_of(cx, &env, id, |k| k.pattern.clone()), "user:*");
    // 打字只改 pattern,不搜索
    assert_eq!(names(cx, &env, id).len(), 5);
    click(cx, &env, "keys-scan");
    wait(cx, &env, "search done", |a| {
        a.tab(id).unwrap().keys.scan_state() == ScanState::Idle
    });
    assert_eq!(names(cx, &env, id), vec!["user:0", "user:1", "user:2"]);

    // 回车也搜索
    type_into(cx, &env, "keys-search", "order:*");
    press(cx, &env, "enter");
    wait(cx, &env, "enter search", |a| {
        a.tab(id).unwrap().keys.scan_state() == ScanState::Idle
            && a.tab(id).unwrap().keys.view_len() == 2
    });
    assert_eq!(names(cx, &env, id), vec!["order:0", "order:1"]);

    // 没有匹配:显示空提示
    type_into(cx, &env, "keys-search", "nothing:*");
    click(cx, &env, "keys-scan");
    wait(cx, &env, "empty", |a| {
        a.tab(id).unwrap().keys.view_len() == 0
    });
    assert!(exists(cx, &env, "keys-empty"));
}

// ---------------------------------------------------------------- 三档扫描 (P-11、P-13)

#[gpui_kit::test]
fn more_and_all_load_the_rest_and_the_hint_follows_the_cursor(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 25);
    small_batches(cx, &env, 10, 0);
    open(cx, &env, id);
    assert_eq!(keys_of(cx, &env, id, |k| k.loaded_count()), 10);
    assert!(exists(cx, &env, "keys-more-hint"), "the cursor is live");

    click(cx, &env, "keys-more");
    wait(cx, &env, "more", |a| {
        let k = &a.tab(id).unwrap().keys;
        k.scan_state() == ScanState::Idle && k.loaded_count() == 20
    });
    assert!(exists(cx, &env, "keys-more-hint"));

    click(cx, &env, "keys-all");
    wait(cx, &env, "all", |a| {
        let k = &a.tab(id).unwrap().keys;
        k.scan_state() == ScanState::Idle && k.loaded_count() == 25
    });
    assert!(!exists(cx, &env, "keys-more-hint"), "the cursor is zero");
    assert!(!keys_of(cx, &env, id, |k| k.has_more()));
}

#[gpui_kit::test]
fn the_overlay_shows_stop_while_scanning_all_and_stop_ends_the_loop(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 50);
    small_batches(cx, &env, 10, 0);
    open(cx, &env, id);
    assert_eq!(keys_of(cx, &env, id, |k| k.loaded_count()), 10);

    // All:后端命令在当前线程同步跑完,结果还在 channel 里,正好是“扫描中”的状态
    dispatch(
        cx,
        &env,
        Msg::Keys {
            id,
            msg: KeysMsg::LoadAll,
        },
    );
    assert!(exists(cx, &env, "keys-busy"));
    assert!(exists(cx, &env, "keys-stop"));
    click(cx, &env, "keys-stop");
    wait(cx, &env, "stopped", |a| {
        a.tab(id).unwrap().keys.scan_state() == ScanState::Idle
    });
    // Stop 之后不再发下一批:只多了已经在路上的那一批
    assert_eq!(keys_of(cx, &env, id, |k| k.loaded_count()), 20);
    assert!(
        keys_of(cx, &env, id, |k| k.has_more()),
        "the cursor is kept"
    );
    assert!(!exists(cx, &env, "keys-busy"));
    // 停了之后 More、All 还能用
    click(cx, &env, "keys-all");
    wait(cx, &env, "all after stop", |a| {
        a.tab(id).unwrap().keys.loaded_count() == 50
    });
}

// ---------------------------------------------------------------- 过滤、排序、选择 (P-13)

#[gpui_kit::test]
fn sorting_filtering_and_selecting_go_through_the_app(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    for k in ["banana", "Apple", "cherry", "avocado"] {
        env.fake
            .put_key(k.as_bytes(), KeyType::String, Some(1), Ttl::Persistent);
    }
    open(cx, &env, id);
    // 默认 Key 升序
    assert_eq!(
        names(cx, &env, id),
        vec!["Apple", "avocado", "banana", "cherry"]
    );
    click(cx, &env, "keys-sort-key");
    assert_eq!(
        names(cx, &env, id),
        vec!["cherry", "banana", "avocado", "Apple"]
    );
    assert_eq!(keys_of(cx, &env, id, |k| k.sort.column), SortColumn::Key);

    // 列头过滤:不区分大小写的子串
    type_into(cx, &env, "keys-filter", "AV");
    assert_eq!(keys_of(cx, &env, id, |k| k.filter.clone()), "AV");
    assert_eq!(names(cx, &env, id), vec!["avocado"]);
    assert!(exists(cx, &env, "keys-filtered"), "N shown");
    type_into(cx, &env, "keys-filter", "a");
    assert_eq!(names(cx, &env, id).len(), 3);

    // 选择:单选、全选(= 当前过滤后的全部行)
    click(cx, &env, "key-check-0");
    assert_eq!(keys_of(cx, &env, id, |k| k.selected_count()), 1);
    click(cx, &env, "keys-select-all");
    assert_eq!(keys_of(cx, &env, id, |k| k.selected_count()), 3);
    assert!(keys_of(cx, &env, id, |k| k.all_filtered_selected()));
    click(cx, &env, "keys-select-all");
    assert_eq!(keys_of(cx, &env, id, |k| k.selected_count()), 0);
    click(cx, &env, "key-check-1");
    click(cx, &env, "key-check-1");
    assert_eq!(keys_of(cx, &env, id, |k| k.selected_count()), 0);
}

// ---------------------------------------------------------------- 详情 (P-14、P-16)

#[gpui_kit::test]
fn visible_rows_load_their_details_and_vanished_keys_are_marked(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_key(b"a", KeyType::String, Some(3), Ttl::Persistent);
    env.fake
        .put_key(b"b", KeyType::List, Some(7), Ttl::Seconds(90));
    env.fake
        .put_key(b"c", KeyType::Stream, Some(1), Ttl::Persistent);
    open(cx, &env, id);
    wait(cx, &env, "details", |a| {
        a.tab(id).unwrap().keys.view().all(|r| r.loaded().is_some())
    });
    let cells: Vec<_> = keys_of(cx, &env, id, |k| {
        k.view()
            .map(|r| (r.length_text(), r.ttl_text(), r.openable()))
            .collect()
    });
    assert_eq!(
        cells,
        vec![
            (Some("3".into()), Some("-1".into()), true),
            (Some("7".into()), Some("90".into()), true),
            (Some("1".into()), Some("-1".into()), false), // Stream 只显示
        ]
    );

    // 别的客户端把 b 删了:点这一行的刷新按钮,TTL 变 -2,标记为已删除
    env.fake.remove_key(b"b");
    click(cx, &env, "key-refresh-1");
    wait(cx, &env, "refresh", |a| {
        a.tab(id).unwrap().keys.view_row(1).unwrap().is_deleted()
    });
}

#[gpui_kit::test]
fn details_stay_unloaded_when_the_setting_is_off_until_refreshed_by_hand(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_key(b"a", KeyType::Hash, Some(2), Ttl::Persistent);
    tweak(cx, &env, |a| a.settings.show_key_details = false);
    open(cx, &env, id);
    cx.run_until_parked();
    assert!(keys_of(cx, &env, id, |k| k
        .view_row(0)
        .unwrap()
        .loaded()
        .is_none()));
    assert!(
        !env.fake
            .calls()
            .iter()
            .any(|c| c.starts_with("key_details")),
        "no detail request"
    );
    click(cx, &env, "key-refresh-0");
    wait(cx, &env, "manual refresh", |a| {
        a.tab(id)
            .unwrap()
            .keys
            .view_row(0)
            .unwrap()
            .loaded()
            .is_some()
    });
}

#[gpui_kit::test]
fn clicking_an_openable_key_opens_it_and_a_stream_does_not(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"s", b"hello");
    env.fake
        .put_key(b"x", KeyType::Stream, Some(1), Ttl::Persistent);
    open(cx, &env, id);
    wait(cx, &env, "details", |a| {
        a.tab(id).unwrap().keys.view().all(|r| r.loaded().is_some())
    });
    // x 是 Stream:点了没反应(P-16)
    click(cx, &env, "key-open-1");
    assert!(read(cx, &env, |a| a.tab(id).unwrap().view.is_none()));
    // s 是 String:打开
    click(cx, &env, "key-open-0");
    cx.run_until_parked();
    assert!(read(cx, &env, |a| a.tab(id).unwrap().view.is_some()));
    assert!(
        exists(cx, &env, "keyview-placeholder"),
        "R-10 draws the editor"
    );
    assert!(!exists(cx, &env, "keys-table"));
    // 关掉回到列表
    click(cx, &env, "keyview-close");
    assert!(read(cx, &env, |a| a.tab(id).unwrap().view.is_none()));
    assert!(exists(cx, &env, "keys-table"));
}

// ---------------------------------------------------------------- 切库 (P-05)

#[gpui_kit::test]
fn switching_the_db_reloads_the_list_and_keeps_the_search(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.set_key_counts(vec![5, 0, 0, 12]);
    env.fake.put_many("k:", 5);
    open(cx, &env, id);
    let choices = read(cx, &env, |a| a.db_choices(id));
    assert_eq!(choices.len(), 4);
    assert_eq!(choices[0].label, "db0 (5 keys)");
    assert!(choices[0].selected);
    type_into(cx, &env, "keys-search", "k:*");

    dispatch(cx, &env, Msg::SelectDb { id, db: 3 });
    wait(cx, &env, "reloaded", |a| {
        is_ready(a)
            && a.tab(id)
                .is_some_and(|t| t.keys.scan_state() == ScanState::Idle)
            && a.connection(id).is_some_and(|c| c.selected_db == 3)
    });
    assert!(
        env.fake
            .calls()
            .iter()
            .any(|c| c.ends_with(":3") && c.starts_with("select_db"))
    );
    assert_eq!(keys_of(cx, &env, id, |k| k.pattern.clone()), "k:*");
    let choices = read(cx, &env, |a| a.db_choices(id));
    assert!(choices[3].selected && !choices[0].selected);
    assert_eq!(
        read(cx, &env, |a| a.keys_status(id).unwrap().loaded),
        "Loaded 5 / 12",
        "the total follows the selected db"
    );
    assert!(exists(cx, &env, "keys-db"));
}

// ---------------------------------------------------------------- 1 万行 (性能)

/// 1 万行:能加载、能渲染、滚到末尾不崩;只有可见的行被画出来(虚拟滚动)。耗时打印出来记进日志。
/// 流畅度(帧率、滚动手感)无头测不了,只能试用看。
#[gpui_kit::test]
fn ten_thousand_rows_render_with_virtual_scrolling_and_scroll_to_the_end(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("key:", 10_000);
    small_batches(cx, &env, 5_000, 0);
    let t0 = Instant::now();
    open(cx, &env, id);
    dispatch(
        cx,
        &env,
        Msg::Keys {
            id,
            msg: KeysMsg::LoadAll,
        },
    );
    wait(cx, &env, "10k loaded", |a| {
        let k = &a.tab(id).unwrap().keys;
        k.scan_state() == ScanState::Idle && k.loaded_count() == 10_000
    });
    let loaded = t0.elapsed();
    assert_eq!(keys_of(cx, &env, id, |k| k.view_len()), 10_000);

    let t1 = Instant::now();
    assert!(exists(cx, &env, "key-row-0"));
    let first_frame = t1.elapsed();
    // 只画可见的行:远处的行不存在
    assert!(!exists(cx, &env, "key-row-500"));
    assert!(!exists(cx, &env, "key-row-9999"));

    // 滚到末尾
    let t2 = Instant::now();
    cx.update_window(env.window, |_, _, cx| {
        env.view
            .update(cx, |this, _| this.scroll_keys_to(id, 9_999));
    })
    .unwrap();
    let at_end = exists(cx, &env, "key-row-9999");
    let scrolled = t2.elapsed();
    assert!(at_end, "scrolled to the last row");
    assert!(!exists(cx, &env, "key-row-0"));

    // 过滤、排序一万行也不崩
    let t3 = Instant::now();
    click(cx, &env, "keys-sort-key");
    type_into(cx, &env, "keys-filter", "key:99");
    let filtered = keys_of(cx, &env, id, |k| k.view_len());
    let refilter = t3.elapsed();
    assert_eq!(filtered, 111); // key:99, key:990-999, key:9900-9999
    println!(
        "R-09 perf (10k rows, debug build, headless): load+scan {loaded:?}, first frame {first_frame:?}, \
         scroll to end + frame {scrolled:?}, sort+filter {refilter:?}"
    );
}

/// 界面代码里用到的每个 `keys.*`、`db.*` 文字 key 都在英文包里(拼错了界面上会显示 key 本身)。
#[test]
fn every_keys_text_key_used_by_the_ui_exists() {
    let app = xredis_app::App::new();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let mut found = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for part in text.split('"').skip(1).step_by(2) {
            let is_key = (part.starts_with("keys.") || part.starts_with("db."))
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_');
            if is_key {
                found += 1;
                assert_ne!(app.t(part), part, "{part} in {}", path.display());
            }
        }
    }
    assert!(found >= 15, "scanner found only {found}");
}

// ---------------------------------------------------------------- R-09b:Delete、Copy、Paste、New

use super::dialogs::open_dialogs;
use crate::ui::DialogKind;

fn select_rows(cx: &mut TestAppContext, env: &Env, ixs: &[usize]) {
    for ix in ixs {
        click(cx, env, &format!("key-check-{ix}"));
    }
}

fn clipboard_text(cx: &mut TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|i| i.text()))
}

#[gpui_kit::test]
fn the_toolbar_has_all_actions_and_disabled_ones_do_nothing(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 3);
    open(cx, &env, id);
    for b in ["keys-delete", "keys-new", "keys-copy", "keys-paste"] {
        assert!(exists(cx, &env, b), "{b}");
    }
    // 没选中:Delete、Copy 点了没反应(按钮禁用)
    click(cx, &env, "keys-delete");
    click(cx, &env, "keys-copy");
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(clipboard_text(cx), None);
    assert!(!read(cx, &env, |a| a
        .tab(id)
        .unwrap()
        .keys
        .actions()
        .delete));
    select_rows(cx, &env, &[0]);
    assert!(read(cx, &env, |a| a.tab(id).unwrap().keys.actions().delete));
}

#[gpui_kit::test]
fn delete_asks_first_enter_does_not_confirm_esc_cancels_the_button_deletes(
    cx: &mut TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 4);
    open(cx, &env, id);
    select_rows(cx, &env, &[0, 1]);
    click(cx, &env, "keys-delete");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::DeleteKeys]);
    assert_eq!(
        read(cx, &env, |a| a.tab(id).unwrap().keys.confirm_delete),
        Some(2)
    );
    for b in [
        "keys-delete-confirm",
        "keys-delete-body",
        "keys-delete-ok",
        "keys-delete-cancel",
    ] {
        assert!(exists(cx, &env, b), "{b}");
    }
    // 回车不起作用(破坏性确认)
    press(cx, &env, "enter");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::DeleteKeys]);
    assert_eq!(env.fake.keyspace_len(), 4);
    // Esc 取消
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(env.fake.keyspace_len(), 4);
    assert_eq!(
        keys_of(cx, &env, id, |k| k.selected_count()),
        2,
        "selection kept"
    );

    // 取消按钮、再确认
    click(cx, &env, "keys-delete");
    click(cx, &env, "keys-delete-cancel");
    assert!(open_dialogs(cx, &env).is_empty());
    click(cx, &env, "keys-delete");
    click(cx, &env, "keys-delete-ok");
    wait(cx, &env, "deleted", |a| {
        a.tab(id).unwrap().keys.busy.is_none() && a.tab(id).unwrap().keys.loaded_count() == 2
    });
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(env.fake.keyspace_len(), 2);
    assert!(!env.fake.has_key(b"k:0") && !env.fake.has_key(b"k:1"));
    assert_eq!(names(cx, &env, id), vec!["k:2", "k:3"]);
}

#[gpui_kit::test]
fn copy_then_paste_with_and_without_conflicts(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 3);
    open(cx, &env, id);
    select_rows(cx, &env, &[0, 1]);
    click(cx, &env, "keys-copy");
    wait(cx, &env, "copied", |a| {
        a.tab(id).unwrap().keys.busy.is_none()
    });
    let text = clipboard_text(cx).expect("text on the clipboard");
    assert!(text.starts_with("REDIS:"), "{text}");

    // 两个 key 都还在:确认框说明“都已存在”,有覆盖选项(默认不勾)
    click(cx, &env, "keys-paste");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::PasteKeys]);
    assert_eq!(
        read(cx, &env, |a| a
            .tab(id)
            .unwrap()
            .keys
            .paste
            .as_ref()
            .map(|p| (p.total, p.existing, p.overwrite))),
        Some((2, 2, false))
    );
    assert!(exists(cx, &env, "keys-paste-overwrite"));
    assert!(exists(cx, &env, "keys-paste-summary"));
    // 回车不起作用,Esc 取消,什么都没写
    press(cx, &env, "enter");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::PasteKeys]);
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(!env.fake.calls().iter().any(|c| c.starts_with("paste_keys")));

    // 不勾覆盖直接粘贴:有冲突整体拒绝,弹窗关闭,没写
    click(cx, &env, "keys-paste");
    click(cx, &env, "keys-paste-ok");
    wait(cx, &env, "paste rejected", |a| {
        a.tab(id).unwrap().keys.busy.is_none()
    });
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(env.fake.keyspace_len(), 3);

    // 勾上覆盖后粘贴成功
    click(cx, &env, "keys-paste");
    click(cx, &env, "keys-paste-overwrite");
    assert!(read(cx, &env, |a| a
        .tab(id)
        .unwrap()
        .keys
        .paste
        .as_ref()
        .unwrap()
        .overwrite));
    click(cx, &env, "keys-paste-ok");
    wait(cx, &env, "pasted", |a| {
        a.tab(id).unwrap().keys.busy.is_none()
    });
    assert!(
        env.fake
            .calls()
            .iter()
            .any(|c| c.ends_with(":true") && c.starts_with("paste_keys"))
    );

    // 删掉再粘贴:没有同名 key,不显示覆盖选项,确认后恢复
    env.fake.remove_key(b"k:0");
    env.fake.remove_key(b"k:1");
    click(cx, &env, "keys-paste");
    assert!(!exists(cx, &env, "keys-paste-overwrite"));
    click(cx, &env, "keys-paste-ok");
    wait(cx, &env, "restored", |a| {
        a.tab(id).unwrap().keys.busy.is_none() && a.tab(id).unwrap().keys.loaded_count() == 3
    });
    assert!(env.fake.has_key(b"k:0") && env.fake.has_key(b"k:1"));
}

#[gpui_kit::test]
fn pasting_an_empty_clipboard_shows_no_dialog(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_many("k:", 1);
    open(cx, &env, id);
    cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("not a key dump".into()))
    });
    click(cx, &env, "keys-paste");
    cx.run_until_parked();
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(keys_of(cx, &env, id, |k| k.paste.is_none()));
}

#[gpui_kit::test]
fn new_key_opens_the_editor_view_for_the_chosen_type(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open(cx, &env, id);
    // 菜单项在无头里点不到:用 dispatch 发菜单项会发的那条 Msg
    dispatch(
        cx,
        &env,
        Msg::View {
            id,
            msg: xredis_app::keyview::ViewMsg::NewKey(KeyType::Hash),
        },
    );
    cx.run_until_parked();
    assert!(read(cx, &env, |a| a.tab(id).unwrap().view.is_some()));
    assert!(
        exists(cx, &env, "keyview-placeholder"),
        "R-10 draws the editor"
    );
    click(cx, &env, "keyview-close");
    assert!(exists(cx, &env, "keys-new"));
}
