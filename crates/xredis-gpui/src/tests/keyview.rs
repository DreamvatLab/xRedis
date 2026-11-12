//! String 编辑器、新建 key、改名和 TTL 对话框的无头测试(R-10a)。
//! 从 Keys 页点开 key 开始,点击、打字、回车和 Esc 走真实的事件路径,断言 `App` 的状态、
//! 输入控件里的文字和假后端收到的写操作。外观(布局、字体、滚动手感)只能试用看。

use gpui_kit::TestAppContext;
use xredis_app::Msg;
use xredis_app::keylist::ScanState;
use xredis_app::keyview::ViewMsg;
use xredis_app::keyview::display::Format;
use xredis_app::keyview::editor::Editor;
use xredis_core::config::ConnId;
use xredis_core::keys::{KeyType, Ttl};
use xredis_core::members::WriteOp;
use xredis_core::types::list::Side;
use xredis_core::types::string::SetMode;

use super::dialogs::{open_dialogs, press, type_into};
use super::{Env, click, dispatch, exists, is_ready, read, setup, wait};
use crate::ui::DialogKind;

pub(super) fn open_tab(cx: &mut TestAppContext, env: &Env, id: ConnId) {
    dispatch(cx, env, Msg::OpenConnection(id));
    wait(cx, env, "keys loaded", |a| {
        is_ready(a)
            && a.tab(id)
                .is_some_and(|t| t.keys.scan_state() == ScanState::Idle)
    });
}

/// 点开第 `row` 行的 key,等到编辑器读完值。
fn open_key(cx: &mut TestAppContext, env: &Env, id: ConnId, row: usize) {
    click(cx, env, &format!("key-open-{row}"));
    wait(cx, env, "string loaded", |a| {
        a.tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| v.editor.as_ref())
            .is_some_and(|e| !e.loading)
    });
    cx.run_until_parked();
}

pub(super) fn editor<R>(
    cx: &mut TestAppContext,
    env: &Env,
    id: ConnId,
    f: impl FnOnce(&Editor) -> R,
) -> R {
    read(cx, env, |a| {
        f(a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .editor
            .as_ref()
            .unwrap())
    })
}

pub(super) fn view_open(cx: &mut TestAppContext, env: &Env, id: ConnId) -> bool {
    read(cx, env, |a| a.tab(id).unwrap().view.is_some())
}

/// 输入控件里现在的文字:(key 名, 成员名, 值)。
fn texts(cx: &mut TestAppContext, env: &Env, id: ConnId) -> (String, String, String) {
    cx.read(|cx| env.view.read(cx).form_texts(id, cx).expect("a form"))
}

pub(super) fn send(cx: &mut TestAppContext, env: &Env, id: ConnId, msg: ViewMsg) {
    dispatch(cx, env, Msg::View { id, msg });
    cx.run_until_parked();
}

fn dialog_text(cx: &mut TestAppContext, env: &Env, id: ConnId, ttl: bool) -> Option<String> {
    read(cx, env, |a| {
        let v = a.tab(id).unwrap().view.as_ref().unwrap();
        let d = if ttl { &v.ttl } else { &v.rename };
        d.as_ref().map(|d| d.text.clone())
    })
}

// ---------------------------------------------------------------- 打开 (P-21)

#[gpui_kit::test]
fn opening_a_string_draws_the_editor_with_its_value_and_header(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"hello");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    for c in [
        "keyview",
        "keyview-title",
        "keyview-length",
        "keyview-ttl",
        "key-rename",
        "key-ttl",
        "string-key",
        "string-toolbar",
        "string-language",
        "string-format",
        "string-minify",
        "string-hex",
        "string-value",
        "string-undo",
        "string-cancel",
        "string-save",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert!(!exists(cx, &env, "string-notice"), "nothing is read-only");
    assert!(!exists(cx, &env, "string-member"));
    assert!(!exists(cx, &env, "keys-table"));
    let (key, _, value) = texts(cx, &env, id);
    assert_eq!((key.as_str(), value.as_str()), ("doc", "hello"));
    // 标题、长度、TTL 来自 App
    wait(cx, &env, "details", |a| {
        a.tab(id).unwrap().view.as_ref().unwrap().details.is_some()
    });
    let (title, length, ttl) = read(cx, &env, |a| {
        let v = a.tab(id).unwrap().view.as_ref().unwrap();
        (v.title("en"), v.length_text(), v.ttl_text())
    });
    assert_eq!(title, "doc");
    assert_eq!(length.as_deref(), Some("5"));
    assert_eq!(ttl.as_deref(), Some("-1"));
    // 没改动:不能 Save、不能 Undo
    let a = editor(cx, &env, id, |e| e.actions());
    assert!(!a.save && !a.undo);
}

// ---------------------------------------------------------------- 改值、保存、撤销、取消

#[gpui_kit::test]
fn typing_a_value_enables_save_and_save_writes_it_keeping_the_ttl(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"hello");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);

    type_into(cx, &env, "string-value", "world");
    assert_eq!(editor(cx, &env, id, |e| e.value.text.clone()), "world");
    assert!(editor(cx, &env, id, |e| e.actions().save && e.actions().undo));

    click(cx, &env, "string-save");
    wait(cx, &env, "saved and closed", |a| {
        a.tab(id).unwrap().view.is_none()
    });
    assert_eq!(env.fake.string_value(b"doc").unwrap().as_ref(), b"world");
    // 改值用 KeepTtl(第 16 节)
    assert!(env.fake.applied().iter().any(|op| matches!(
        op,
        WriteOp::SetString {
            mode: SetMode::KeepTtl,
            ..
        }
    )));
    assert!(exists(cx, &env, "keys-table"), "back to the list");
}

#[gpui_kit::test]
fn undo_puts_the_original_back_into_the_controls(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"hello");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);

    type_into(cx, &env, "string-value", "changed");
    type_into(cx, &env, "string-key", "doc2");
    assert_eq!(texts(cx, &env, id).0, "doc2");
    click(cx, &env, "string-undo");
    cx.run_until_parked();
    let (key, _, value) = texts(cx, &env, id);
    assert_eq!((key.as_str(), value.as_str()), ("doc", "hello"));
    assert!(!editor(cx, &env, id, |e| e.has_changes()));
    assert!(!editor(cx, &env, id, |e| e.actions().save));
}

#[gpui_kit::test]
fn cancel_closes_without_writing(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"hello");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    type_into(cx, &env, "string-value", "never saved");
    click(cx, &env, "string-cancel");
    assert!(!view_open(cx, &env, id));
    assert!(env.fake.applied().is_empty());
    assert_eq!(env.fake.string_value(b"doc").unwrap().as_ref(), b"hello");
}

#[gpui_kit::test]
fn renaming_in_the_editor_and_saving_renames_then_writes(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"hello");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    type_into(cx, &env, "string-key", "doc-new");
    type_into(cx, &env, "string-value", "v2");
    click(cx, &env, "string-save");
    wait(cx, &env, "closed", |a| a.tab(id).unwrap().view.is_none());
    let ops = env.fake.applied();
    assert!(matches!(ops[0], WriteOp::RenameKey { .. }), "{ops:?}");
    assert!(matches!(ops[1], WriteOp::SetString { .. }), "{ops:?}");
    assert_eq!(env.fake.string_value(b"doc-new").unwrap().as_ref(), b"v2");
    assert!(env.fake.string_value(b"doc").is_none());
}

// ---------------------------------------------------------------- 语言、Format、Minify (P-21)

#[gpui_kit::test]
fn json_is_detected_and_format_and_minify_rewrite_the_control(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"j", br#"{"a":[1,2]}"#);
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    assert_eq!(editor(cx, &env, id, |e| e.format), Format::Json);
    assert!(editor(cx, &env, id, |e| e.actions().format));

    click(cx, &env, "string-format");
    cx.run_until_parked();
    assert!(texts(cx, &env, id).2.contains('\n'));
    assert!(editor(cx, &env, id, |e| e.has_changes()));
    click(cx, &env, "string-minify");
    cx.run_until_parked();
    assert_eq!(texts(cx, &env, id).2, r#"{"a":[1,2]}"#);
    assert!(!editor(cx, &env, id, |e| e.has_changes()));

    // 选了纯文本:Format 和 Minify 不可用(语言下拉的菜单项在无头里点不到,发它会发的 Msg)
    send(cx, &env, id, ViewMsg::SetFormat(Format::Plain));
    assert!(!editor(cx, &env, id, |e| e.actions().format));
    assert!(!editor(cx, &env, id, |e| e.actions().minify));
}

#[gpui_kit::test]
fn formatting_text_that_is_not_json_warns_and_keeps_the_text(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"t", b"just text");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    send(cx, &env, id, ViewMsg::SetFormat(Format::Json));
    click(cx, &env, "string-format");
    cx.run_until_parked();
    assert_eq!(texts(cx, &env, id).2, "just text");
    wait(cx, &env, "warning toast", |a| {
        !a.messages.toasts().is_empty()
    });
}

// ---------------------------------------------------------------- 二进制、十六进制、大值 (P-29、P-30)

#[gpui_kit::test]
fn a_binary_value_is_read_only_with_a_notice_and_the_hex_view_toggles(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"bin", &[0xff, 0x01, b'j', b'o', b'b']);
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    assert!(exists(cx, &env, "string-notice"));
    assert!(editor(cx, &env, id, |e| e.read_only()));
    let shown = texts(cx, &env, id).2;

    // 打字不会进草稿,Save 和 Undo 不可用
    type_into(cx, &env, "string-value", "overwrite");
    assert!(!editor(cx, &env, id, |e| e.has_changes()));
    assert!(!editor(cx, &env, id, |e| e.actions().save));
    assert_eq!(texts(cx, &env, id).2, shown, "the control is read-only");
    click(cx, &env, "string-save");
    assert!(env.fake.applied().is_empty());

    // 十六进制视图
    click(cx, &env, "string-hex");
    cx.run_until_parked();
    assert!(editor(cx, &env, id, |e| e.hex));
    let hex = texts(cx, &env, id).2;
    assert!(hex.starts_with("00000000  ff 01 6a 6f 62"), "{hex}");
    click(cx, &env, "string-hex");
    cx.run_until_parked();
    assert!(!editor(cx, &env, id, |e| e.hex));
    assert_eq!(texts(cx, &env, id).2, shown);
}

#[gpui_kit::test]
fn the_hex_view_of_a_text_value_keeps_the_draft(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"abc");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    type_into(cx, &env, "string-value", "abcd");
    click(cx, &env, "string-hex");
    cx.run_until_parked();
    assert!(texts(cx, &env, id).2.starts_with("00000000  61 62 63"));
    // 十六进制视图里打字没有用,草稿还在
    type_into(cx, &env, "string-value", "zzz");
    click(cx, &env, "string-hex");
    cx.run_until_parked();
    assert_eq!(texts(cx, &env, id).2, "abcd");
    assert_eq!(editor(cx, &env, id, |e| e.value.text.clone()), "abcd");
}

#[gpui_kit::test]
fn a_big_string_is_read_only_with_the_size_in_the_notice(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .put_big_string(b"big", &vec![b'x'; 1024 * 1024], 70 * 1024 * 1024);
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    assert!(exists(cx, &env, "string-notice"));
    assert!(editor(cx, &env, id, |e| e.read_only()));
    let notice = editor(cx, &env, id, |e| e.notice("en")).unwrap();
    assert!(notice.contains("70.0 MiB"), "{notice}");
    assert!(!editor(cx, &env, id, |e| e.actions().save));
}

// ---------------------------------------------------------------- 改名 (P-27)

#[gpui_kit::test]
fn rename_conflict_keeps_the_dialog_with_the_reason_and_success_closes_the_view(
    cx: &mut TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"a-doc", b"1");
    env.fake.put_string(b"b-taken", b"2");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    assert_eq!(texts(cx, &env, id).0, "a-doc");

    click(cx, &env, "key-rename");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Rename]);
    for c in [
        "rename-dialog",
        "rename-input",
        "rename-ok",
        "rename-cancel",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert!(!exists(cx, &env, "rename-error"));
    assert_eq!(dialog_text(cx, &env, id, false).as_deref(), Some("a-doc"));

    // 目标已存在:对话框留着,红字写原因,key 没动
    type_into(cx, &env, "rename-input", "b-taken");
    click(cx, &env, "rename-ok");
    wait(cx, &env, "rename rejected", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.rename.as_ref().is_some_and(|d| d.error.is_some()))
    });
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Rename]);
    assert!(exists(cx, &env, "rename-error"));
    assert!(env.fake.has_key(b"a-doc"));
    assert_eq!(env.fake.string_value(b"b-taken").unwrap().as_ref(), b"2");

    // 改了文字,错误消失;空名字被 App 拒绝
    type_into(cx, &env, "rename-input", "x");
    press(cx, &env, "backspace");
    assert!(!exists(cx, &env, "rename-error"));
    press(cx, &env, "enter");
    assert!(exists(cx, &env, "rename-error"), "an empty name is refused");
    assert!(env.fake.has_key(b"a-doc"));

    // 回车确认
    type_into(cx, &env, "rename-input", "c-new");
    press(cx, &env, "enter");
    wait(cx, &env, "renamed", |a| a.tab(id).unwrap().view.is_none());
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(env.fake.has_key(b"c-new") && !env.fake.has_key(b"a-doc"));
}

#[gpui_kit::test]
fn escape_and_the_cancel_button_close_the_rename_dialog_without_writing(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"1");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    click(cx, &env, "key-rename");
    type_into(cx, &env, "rename-input", "other");
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(view_open(cx, &env, id), "the editor stays");
    assert!(env.fake.applied().is_empty());
    click(cx, &env, "key-rename");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Rename]);
    click(cx, &env, "rename-cancel");
    assert!(open_dialogs(cx, &env).is_empty());
}

// ---------------------------------------------------------------- TTL (P-28)

#[gpui_kit::test]
fn ttl_zero_and_garbage_are_refused_by_the_app_before_anything_is_sent(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"1");
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    click(cx, &env, "key-ttl");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Ttl]);
    for c in [
        "ttl-dialog",
        "ttl-input",
        "ttl-hint",
        "ttl-ok",
        "ttl-cancel",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert_eq!(dialog_text(cx, &env, id, true).as_deref(), Some("-1"));

    for bad in ["0", "abc", "2147483648", "-5"] {
        type_into(cx, &env, "ttl-input", bad);
        click(cx, &env, "ttl-ok");
        assert!(exists(cx, &env, "ttl-error"), "{bad} must be refused");
        assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Ttl], "{bad}");
        assert!(env.fake.applied().is_empty(), "{bad} reached the backend");
    }
    // 改了文字,错误消失
    type_into(cx, &env, "ttl-input", "30");
    assert!(!exists(cx, &env, "ttl-error"));

    press(cx, &env, "enter");
    wait(cx, &env, "ttl applied", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.ttl.is_none())
    });
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(env.fake.key_ttl(b"doc"), Some(Ttl::Seconds(30)));
    assert!(
        view_open(cx, &env, id),
        "the editor stays after a TTL change"
    );
    // 头部的 TTL 跟着刷新
    wait(cx, &env, "details refreshed", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .unwrap()
            .ttl_text()
            .as_deref()
            == Some("30")
    });
}

#[gpui_kit::test]
fn ttl_minus_one_persists_and_escape_cancels(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"doc", b"1");
    env.fake
        .put_key(b"doc", KeyType::String, Some(1), Ttl::Seconds(500));
    open_tab(cx, &env, id);
    open_key(cx, &env, id, 0);
    click(cx, &env, "key-ttl");
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(env.fake.applied().is_empty());

    click(cx, &env, "key-ttl");
    type_into(cx, &env, "ttl-input", "-1");
    click(cx, &env, "ttl-ok");
    wait(cx, &env, "persisted", |a| {
        a.tab(id)
            .unwrap()
            .view
            .as_ref()
            .is_some_and(|v| v.ttl.is_none())
    });
    assert_eq!(env.fake.key_ttl(b"doc"), Some(Ttl::Persistent));
}

// ---------------------------------------------------------------- 新建 key (P-20)

#[gpui_kit::test]
fn new_string_key_creates_without_overwriting(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake.put_string(b"exists", b"old");
    open_tab(cx, &env, id);
    send(cx, &env, id, ViewMsg::NewKey(KeyType::String));
    assert!(exists(cx, &env, "keyview"));
    assert!(!exists(cx, &env, "key-rename"), "a new key has no name yet");
    assert!(!exists(cx, &env, "key-ttl"));
    assert!(!editor(cx, &env, id, |e| e.actions().save));
    assert!(!editor(cx, &env, id, |e| e.actions().hex));
    let title = read(cx, &env, |a| {
        a.tab(id).unwrap().view.as_ref().unwrap().title("en")
    });
    assert_eq!(title, "New String");

    // 已存在:报错,不覆盖,编辑器留着
    type_into(cx, &env, "string-key", "exists");
    type_into(cx, &env, "string-value", "new");
    click(cx, &env, "string-save");
    wait(cx, &env, "error toast", |a| !a.messages.toasts().is_empty());
    assert!(view_open(cx, &env, id));
    assert_eq!(env.fake.string_value(b"exists").unwrap().as_ref(), b"old");

    // 换一个名字就成功,回到列表
    type_into(cx, &env, "string-key", "fresh");
    click(cx, &env, "string-save");
    wait(cx, &env, "created", |a| a.tab(id).unwrap().view.is_none());
    assert_eq!(env.fake.string_value(b"fresh").unwrap().as_ref(), b"new");
}

#[gpui_kit::test]
fn new_hash_zset_list_and_set_show_the_inputs_they_need(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_tab(cx, &env, id);
    for (kind, member, position) in [
        (KeyType::Hash, true, false),
        (KeyType::ZSet, true, false),
        (KeyType::List, false, true),
        (KeyType::Set, false, false),
    ] {
        send(cx, &env, id, ViewMsg::NewKey(kind.clone()));
        assert_eq!(exists(cx, &env, "string-member"), member, "{kind:?}");
        assert_eq!(exists(cx, &env, "string-position"), position, "{kind:?}");
        assert!(exists(cx, &env, "string-key"), "{kind:?}");
        click(cx, &env, "string-cancel");
        assert!(!view_open(cx, &env, id));
    }
}

#[gpui_kit::test]
fn a_new_list_key_remembers_head_or_tail_and_a_zset_score_must_be_a_number(
    cx: &mut TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_tab(cx, &env, id);
    send(cx, &env, id, ViewMsg::NewKey(KeyType::List));
    click(cx, &env, "string-position-head");
    assert_eq!(editor(cx, &env, id, |e| e.position), Side::Head);
    click(cx, &env, "string-position-tail");
    assert_eq!(editor(cx, &env, id, |e| e.position), Side::Tail);
    click(cx, &env, "string-cancel");

    send(cx, &env, id, ViewMsg::NewKey(KeyType::ZSet));
    type_into(cx, &env, "string-key", "scores");
    type_into(cx, &env, "string-member", "alice");
    type_into(cx, &env, "string-value", "high");
    click(cx, &env, "string-save");
    cx.run_until_parked();
    assert!(view_open(cx, &env, id), "a bad score is refused");
    assert!(env.fake.applied().is_empty());
    type_into(cx, &env, "string-value", "9.5");
    click(cx, &env, "string-save");
    wait(cx, &env, "created", |a| a.tab(id).unwrap().view.is_none());
    assert!(env.fake.has_key(b"scores"));
}
