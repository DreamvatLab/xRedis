//! 成员表和成员编辑器的无头测试(R-10b):从 Keys 页点开 Hash、List、Set、ZSet 开始,
//! 点击、打字、回车和 Esc 走真实的事件路径,断言 `App` 的状态和假后端收到的写操作。
//! 外观和滚动手感只能试用看。

use bytes::Bytes;
use gpui_kit::{AppContext as _, TestAppContext};
use xredis_app::Msg;
use xredis_app::keyview::table::ScanState;
use xredis_core::config::ConnId;
use xredis_core::keys::KeyType;
use xredis_core::members::Member;
use xredis_core::types::list::Side;

use super::dialogs::{open_dialogs, press, type_into};
use super::keyview::{editor, open_tab, view_open};
use super::{Env, click, dispatch, exists, read, setup, wait};
use crate::ui::DialogKind;

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn hash(f: &str, v: &str) -> Member {
    Member::Hash {
        field: b(f),
        value: b(v),
    }
}

fn hashes(n: usize) -> Vec<Member> {
    (0..n)
        .map(|i| hash(&format!("f{i:02}"), &format!("v{i:02}")))
        .collect()
}

fn tweak(cx: &mut TestAppContext, env: &Env, f: impl FnOnce(&mut xredis_app::App)) {
    cx.update_window(env.window, |_, _, cx| {
        env.view.update(cx, |this, _| f(&mut this.app));
    })
    .unwrap();
}

/// 点开第 0 行的 key,等到成员表扫完。
fn open_members(cx: &mut TestAppContext, env: &Env, id: ConnId) {
    click(cx, env, "key-open-0");
    wait(cx, env, "members loaded", |a| {
        a.tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| v.table.as_ref())
            .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() > 0)
    });
    cx.run_until_parked();
}

fn names(cx: &mut TestAppContext, env: &Env, id: ConnId) -> Vec<String> {
    read(cx, env, |a| {
        let t = a
            .tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        (0..t.view_len())
            .map(|i| t.view_row(i).unwrap().name_text)
            .collect()
    })
}

fn table_state<R>(
    cx: &mut TestAppContext,
    env: &Env,
    id: ConnId,
    f: impl FnOnce(&xredis_app::keyview::table::MemberTable) -> R,
) -> R {
    read(cx, env, |a| {
        f(a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap())
    })
}

fn member_texts(cx: &mut TestAppContext, env: &Env, id: ConnId) -> (String, String) {
    cx.read(|cx| env.view.read(cx).member_form_texts(id, cx).expect("a form"))
}

// ---------------------------------------------------------------- 打开和页面 (P-22)

#[gpui_kit::test]
fn opening_a_hash_draws_the_member_table_with_every_control(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(b"h", KeyType::Hash, hashes(3));
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    for c in [
        "keyview-members",
        "keyview-title",
        "key-rename",
        "key-ttl",
        "keyview-close",
        "members-toolbar",
        "members-search",
        "members-scan",
        "members-add",
        "members-delete",
        "members-table",
        "members-header",
        "members-select-all",
        "members-sort-name",
        "members-sort-value",
        "members-filter",
        "members-footer",
        "members-status",
        "members-more",
        "members-all",
        "member-row-0",
        "member-row-2",
        "member-check-0",
        "member-open-0",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert!(!exists(cx, &env, "member-row-3"));
    assert!(!exists(cx, &env, "members-busy"));
    assert!(!exists(cx, &env, "keyview-placeholder"));
    assert_eq!(names(cx, &env, id), vec!["f00", "f01", "f02"]);
    // 没选中:不能删除;没有更多:More 和 All 不可用
    let a = read(cx, &env, |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .member_actions()
            .unwrap()
    });
    assert!(a.add && !a.delete && !a.more && !a.all);
    click(cx, &env, "keyview-close");
    assert!(!view_open(cx, &env, id));
}

#[gpui_kit::test]
fn a_set_has_one_column_and_a_zset_sorts_by_score_descending(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"s",
        KeyType::Set,
        vec![
            Member::Set { member: b("m1") },
            Member::Set { member: b("m2") },
        ],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    assert!(exists(cx, &env, "members-sort-name"));
    assert!(
        !exists(cx, &env, "members-sort-value"),
        "a Set has no value column"
    );
    assert!(!exists(cx, &env, "member-value-0"));
    click(cx, &env, "keyview-close");

    env.fake.put_members(
        b"z",
        KeyType::ZSet,
        vec![
            Member::ZSet {
                member: b("low"),
                score: 1.0,
            },
            Member::ZSet {
                member: b("high"),
                score: 9.0,
            },
            Member::ZSet {
                member: b("mid"),
                score: 5.0,
            },
        ],
    );
    dispatch(
        cx,
        &env,
        Msg::Keys {
            id,
            msg: xredis_app::keylist::KeysMsg::Search,
        },
    );
    wait(cx, &env, "list", |a| {
        a.tab(id).unwrap().keys.view_len() == 2
    });
    // 排序后 z 在第 2 行(key 按名字升序:h? 这里只有 s、z)
    let row = read(cx, &env, |a| {
        a.tab(id)
            .unwrap()
            .keys
            .view()
            .position(|r| r.display == "z")
            .unwrap()
    });
    click(cx, &env, &format!("key-open-{row}"));
    wait(cx, &env, "zset loaded", |a| {
        a.tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| v.table.as_ref())
            .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 3)
    });
    cx.run_until_parked();
    assert_eq!(names(cx, &env, id), vec!["high", "mid", "low"]);
    // 点分数列头:换成升序
    click(cx, &env, "members-sort-value");
    assert_eq!(names(cx, &env, id), vec!["low", "mid", "high"]);
}

// ---------------------------------------------------------------- 三档扫描和进度

#[gpui_kit::test]
fn more_all_and_the_progress_texts_come_from_the_app(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(b"h", KeyType::Hash, hashes(10));
    tweak(cx, &env, |a| {
        a.settings.scan_batch = 2;
        a.settings.auto_scan_limit = 4;
    });
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    // 自动续扫到上限(4 行)就停,还有更多
    wait(cx, &env, "auto scan stops", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() >= 4)
        })
    });
    assert_eq!(table_state(cx, &env, id, |t| t.loaded_count()), 4);
    assert!(exists(cx, &env, "members-more-hint"));
    assert!(exists(cx, &env, "members-percent"));
    let loaded = read(cx, &env, |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .member_status("en")
            .unwrap()
            .loaded
    });
    assert_eq!(loaded, "Loaded 4 / 10");

    click(cx, &env, "members-more");
    wait(cx, &env, "more done", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 6)
        })
    });
    click(cx, &env, "members-all");
    wait(cx, &env, "all done", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 10)
        })
    });
    assert!(!exists(cx, &env, "members-more-hint"));
    assert_eq!(names(cx, &env, id).len(), 10);
}

// ---------------------------------------------------------------- 过滤和排序

#[gpui_kit::test]
fn filter_matches_names_and_values_and_sort_toggles(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"h",
        KeyType::Hash,
        vec![
            hash("apple", "red"),
            hash("banana", "yellow"),
            hash("cherry", "RED"),
        ],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    type_into(cx, &env, "members-filter", "red");
    cx.run_until_parked();
    // 按值过滤,不分大小写
    wait(cx, &env, "filtered", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap()
            .view_len()
            == 2
    });
    assert_eq!(names(cx, &env, id), vec!["apple", "cherry"]);
    assert!(exists(cx, &env, "members-filtered"));
    click(cx, &env, "members-sort-name");
    cx.run_until_parked();
    // 默认名字升序,再点一次换成降序
    wait(cx, &env, "sorted", |a| {
        let t = a
            .tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        t.view_row(0).is_some_and(|r| r.name_text == "cherry")
    });
    assert_eq!(names(cx, &env, id), vec!["cherry", "apple"]);
}

// ---------------------------------------------------------------- 选择和删除 (确认框回车不起作用)

#[gpui_kit::test]
fn deleting_members_asks_first_enter_does_not_confirm_esc_cancels_the_button_deletes(
    cx: &mut TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(b"h", KeyType::Hash, hashes(4));
    open_tab(cx, &env, id);
    open_members(cx, &env, id);

    click(cx, &env, "member-check-0");
    click(cx, &env, "member-check-1");
    assert_eq!(table_state(cx, &env, id, |t| t.selected_count()), 2);
    click(cx, &env, "members-delete");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::DeleteMembers]);
    for c in [
        "members-delete-confirm",
        "members-delete-body",
        "members-delete-ok",
        "members-delete-cancel",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    // 回车不确认
    press(cx, &env, "enter");
    cx.run_until_parked();
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::DeleteMembers]);
    assert!(env.fake.applied().is_empty(), "enter must not delete");
    // Esc 取消,选择还在
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(env.fake.applied().is_empty());
    assert_eq!(table_state(cx, &env, id, |t| t.selected_count()), 2);
    // 按钮确认:删除,表重读
    click(cx, &env, "members-delete");
    click(cx, &env, "members-delete-ok");
    wait(cx, &env, "deleted and reloaded", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 2)
        })
    });
    assert_eq!(env.fake.member_rows(b"h").len(), 2);
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(table_state(cx, &env, id, |t| t.selected_count()), 0);
}

#[gpui_kit::test]
fn select_all_selects_the_filtered_rows(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(b"h", KeyType::Hash, hashes(3));
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "members-select-all");
    assert_eq!(table_state(cx, &env, id, |t| t.selected_count()), 3);
    assert!(table_state(cx, &env, id, |t| t.all_filtered_selected()));
    click(cx, &env, "members-select-all");
    assert_eq!(table_state(cx, &env, id, |t| t.selected_count()), 0);
}

#[gpui_kit::test]
fn a_binary_named_member_is_read_only_but_can_be_deleted(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"h",
        KeyType::Hash,
        vec![
            Member::Hash {
                field: Bytes::from_static(&[0xff, 0x01]),
                value: b("x"),
            },
            hash("ok", "v"),
        ],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    let binary_row = read(cx, &env, |a| {
        let t = a
            .tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        (0..t.view_len())
            .find(|i| t.view_row(*i).unwrap().binary)
            .unwrap()
    });
    click(cx, &env, &format!("member-open-{binary_row}"));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::MemberEditor]);
    assert!(exists(cx, &env, "member-notice"));
    assert!(editor(cx, &env, id, |e| e.read_only() && !e.actions().save));
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());

    click(cx, &env, &format!("member-check-{binary_row}"));
    click(cx, &env, "members-delete");
    click(cx, &env, "members-delete-ok");
    wait(cx, &env, "deleted", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 1)
        })
    });
    assert_eq!(env.fake.member_rows(b"h").len(), 1);
}

// ---------------------------------------------------------------- 成员编辑器 (P-23 至 P-26)

#[gpui_kit::test]
fn clicking_a_member_opens_the_editor_and_save_writes_and_reloads(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_members(b"h", KeyType::Hash, vec![hash("f", "old")]);
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "member-open-0");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::MemberEditor]);
    for c in [
        "member-dialog",
        "member-member",
        "member-value",
        "member-toolbar",
        "member-format",
        "member-hex",
        "member-undo",
        "member-cancel",
        "member-save",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert!(
        !exists(cx, &env, "member-key"),
        "a member has no key-name input"
    );
    assert_eq!(member_texts(cx, &env, id), ("f".into(), "old".into()));
    assert!(!editor(cx, &env, id, |e| e.actions().save));

    type_into(cx, &env, "member-value", "new");
    assert!(editor(cx, &env, id, |e| e.actions().save && e.actions().undo));
    // Undo 写回控件
    click(cx, &env, "member-undo");
    cx.run_until_parked();
    assert_eq!(member_texts(cx, &env, id).1, "old");
    type_into(cx, &env, "member-value", "new");
    click(cx, &env, "member-save");
    wait(cx, &env, "saved", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.editor.is_none())
    });
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(view_open(cx, &env, id), "the table stays");
    wait(cx, &env, "reloaded", |a| {
        let t = a
            .tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        t.scan_state() == ScanState::Idle
            && t.view_row(0)
                .is_some_and(|r| r.value_text.as_deref() == Some("new"))
    });
    assert_eq!(env.fake.member_rows(b"h"), vec![hash("f", "new")]);
}

#[gpui_kit::test]
fn renaming_a_hash_field_in_the_member_editor_writes_the_rename(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_members(b"h", KeyType::Hash, vec![hash("f", "v")]);
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "member-open-0");
    type_into(cx, &env, "member-member", "g");
    click(cx, &env, "member-save");
    wait(cx, &env, "reloaded", |a| {
        let t = a
            .tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .table
            .as_ref()
            .unwrap();
        t.scan_state() == ScanState::Idle && t.view_row(0).is_some_and(|r| r.name_text == "g")
    });
    assert_eq!(env.fake.member_rows(b"h"), vec![hash("g", "v")]);
}

#[gpui_kit::test]
fn escape_closes_the_member_editor_without_writing(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_members(b"h", KeyType::Hash, vec![hash("f", "v")]);
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "member-open-0");
    type_into(cx, &env, "member-value", "changed");
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(env.fake.applied().is_empty());
    click(cx, &env, "member-open-0");
    click(cx, &env, "member-cancel");
    assert!(open_dialogs(cx, &env).is_empty());
    // 重新打开是新的编辑器,里面是原值
    click(cx, &env, "member-open-0");
    assert_eq!(member_texts(cx, &env, id), ("f".into(), "v".into()));
}

#[gpui_kit::test]
fn adding_a_member_creates_it_and_a_duplicate_is_refused(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_members(b"h", KeyType::Hash, vec![hash("f", "v")]);
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "members-add");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::MemberEditor]);
    assert!(exists(cx, &env, "member-member"));
    assert!(
        !editor(cx, &env, id, |e| e.actions().save),
        "empty is not savable"
    );
    // 重名:报错,编辑器留着
    type_into(cx, &env, "member-member", "f");
    type_into(cx, &env, "member-value", "dup");
    click(cx, &env, "member-save");
    wait(cx, &env, "error toast", |a| !a.messages.toasts().is_empty());
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::MemberEditor]);
    assert_eq!(env.fake.member_rows(b"h"), vec![hash("f", "v")]);
    // 换名字就成功
    type_into(cx, &env, "member-member", "g");
    click(cx, &env, "member-save");
    wait(cx, &env, "added", |a| {
        let v = a.tab(id).unwrap().view.as_ref().unwrap();
        v.editor.is_none()
            && v.table
                .as_ref()
                .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 2)
    });
    assert_eq!(env.fake.member_rows(b"h").len(), 2);
}

#[gpui_kit::test]
fn a_new_list_member_asks_head_or_tail(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"l",
        KeyType::List,
        vec![Member::List {
            index: 0,
            value: b("first"),
        }],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "members-add");
    assert!(exists(cx, &env, "member-position"));
    assert!(
        !exists(cx, &env, "member-member"),
        "a new list item has no name"
    );
    click(cx, &env, "member-position-head");
    assert_eq!(editor(cx, &env, id, |e| e.position), Side::Head);
    type_into(cx, &env, "member-value", "zero");
    click(cx, &env, "member-save");
    wait(cx, &env, "pushed", |a| {
        a.tab(id).unwrap().view.as_ref().is_some_and(|v| {
            v.editor.is_none()
                && v.table
                    .as_ref()
                    .is_some_and(|t| t.scan_state() == ScanState::Idle && t.loaded_count() == 2)
        })
    });
    let rows = env.fake.member_rows(b"l");
    assert!(
        matches!(&rows[0], Member::List { value, .. } if value.as_ref() == b"zero"),
        "{rows:?}"
    );
}

#[gpui_kit::test]
fn a_list_member_edit_shows_the_index_read_only_and_sets_the_value(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"l",
        KeyType::List,
        vec![
            Member::List {
                index: 0,
                value: b("a"),
            },
            Member::List {
                index: 1,
                value: b("b"),
            },
        ],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "member-open-1");
    assert_eq!(member_texts(cx, &env, id), ("1".into(), "b".into()));
    // 下标不能改
    type_into(cx, &env, "member-member", "9");
    assert_eq!(editor(cx, &env, id, |e| e.member.text.clone()), "1");
    type_into(cx, &env, "member-value", "B");
    click(cx, &env, "member-save");
    wait(cx, &env, "saved", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.editor.is_none())
    });
    let rows = env.fake.member_rows(b"l");
    assert!(
        matches!(&rows[1], Member::List { value, .. } if value.as_ref() == b"B"),
        "{rows:?}"
    );
}

#[gpui_kit::test]
fn a_zset_member_score_must_be_a_number(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(
        b"z",
        KeyType::ZSet,
        vec![Member::ZSet {
            member: b("m"),
            score: 1.0,
        }],
    );
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "member-open-0");
    type_into(cx, &env, "member-value", "high");
    click(cx, &env, "member-save");
    cx.run_until_parked();
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::MemberEditor]);
    assert!(env.fake.applied().is_empty());
    type_into(cx, &env, "member-value", "7.5");
    click(cx, &env, "member-save");
    wait(cx, &env, "saved", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.editor.is_none())
    });
    assert_eq!(
        env.fake.member_rows(b"z"),
        vec![Member::ZSet {
            member: b("m"),
            score: 7.5
        }]
    );
}

#[gpui_kit::test]
fn rename_and_ttl_work_from_the_member_page(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_members(b"h", KeyType::Hash, hashes(1));
    open_tab(cx, &env, id);
    open_members(cx, &env, id);
    click(cx, &env, "key-ttl");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Ttl]);
    type_into(cx, &env, "ttl-input", "0");
    click(cx, &env, "ttl-ok");
    assert!(exists(cx, &env, "ttl-error"), "0 is refused by the App");
    press(cx, &env, "escape");
    click(cx, &env, "key-rename");
    type_into(cx, &env, "rename-input", "h2");
    press(cx, &env, "enter");
    wait(cx, &env, "renamed", |a| a.tab(id).unwrap().view.is_none());
    assert!(env.fake.has_key(b"h2"));
}
