//! 弹窗的无头测试(R-08b):连接编辑器、设置、免责协议、隐私政策、错误框、删除确认。
//! 假后端驱动整个 `MainView`;点击、打字、回车、Esc 都走真实的事件路径,断言 `App` 的状态和后端调用。
//! 弹窗的外观(布局、颜色、动画)只能试用看。

use std::path::PathBuf;

use gpui_kit::AppContext as _;
use gpui_kit::test::TestWindowExt as _;
use xredis_app::TabStatus;
use xredis_app::msg::{FileField, Msg};
use xredis_core::config::ConnId;

use super::{Env, click, dispatch, exists, is_failed, read, setup, setup_unagreed, wait};
use crate::ui::DialogKind;

pub(super) fn open_dialogs(cx: &mut gpui_kit::TestAppContext, env: &Env) -> Vec<DialogKind> {
    cx.run_until_parked();
    cx.read(|cx| env.view.read(cx).open_dialogs().to_vec())
}

/// 点进一个输入框,把里面的内容换成 `text`(全选后打字)。
pub(super) fn type_into(cx: &mut gpui_kit::TestAppContext, env: &Env, id: &str, text: &str) {
    click(cx, env, id);
    let text = text.to_string();
    cx.update_window(env.window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input(&text, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

pub(super) fn press(cx: &mut gpui_kit::TestAppContext, env: &Env, key: &str) {
    let key = key.to_string();
    cx.update_window(env.window, |_, window, cx| window.press(&key, cx))
        .unwrap();
    cx.run_until_parked();
}

fn stored_hosts(env: &Env) -> Vec<String> {
    env.fake
        .store()
        .connections()
        .into_iter()
        .map(|c| c.host)
        .collect()
}

// ---------------------------------------------------------------- 连接编辑器 (P-02)

#[gpui_kit::test]
fn the_editor_opens_with_the_defaults_and_all_its_controls(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    assert!(open_dialogs(cx, &env).is_empty());
    click(cx, &env, "add-connection");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Editor]);
    for id in [
        "editor",
        "editor-name",
        "editor-host",
        "editor-port",
        "editor-username",
        "editor-password",
        "editor-ordinal",
        "editor-tls",
        "editor-save",
        "editor-cancel",
    ] {
        assert!(exists(cx, &env, id), "{id}");
    }
    // 新建没有“清除已保存的密码”;TLS 关着时没有证书行
    assert!(!exists(cx, &env, "editor-clear-password"));
    assert!(!exists(cx, &env, "editor-ca-choose"));
    // 默认值来自 App(localhost、6379、序号 0)
    let (host, port, ordinal) = read(cx, &env, |a| {
        let e = a.editor.as_ref().unwrap();
        (e.host.clone(), e.port.clone(), e.ordinal.clone())
    });
    assert_eq!(
        (host.as_str(), port.as_str(), ordinal.as_str()),
        ("localhost", "6379", "0")
    );
}

#[gpui_kit::test]
fn typing_reaches_the_app_and_save_stores_the_connection(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    type_into(cx, &env, "editor-name", "我的库");
    type_into(cx, &env, "editor-host", "10.0.0.5");
    type_into(cx, &env, "editor-port", "6400");
    type_into(cx, &env, "editor-username", "alice");
    type_into(cx, &env, "editor-password", "s3cret-pw");
    type_into(cx, &env, "editor-ordinal", "7");
    let e = read(cx, &env, |a| a.editor.clone().unwrap());
    assert_eq!(
        (
            e.name.as_str(),
            e.host.as_str(),
            e.port.as_str(),
            e.username.as_str(),
            e.password.as_str(),
            e.ordinal.as_str()
        ),
        ("我的库", "10.0.0.5", "6400", "alice", "s3cret-pw", "7")
    );

    click(cx, &env, "editor-save");
    wait(cx, &env, "editor closed", |a| a.editor.is_none());
    assert!(open_dialogs(cx, &env).is_empty());
    let saved = env.fake.store().connections();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (
            saved[0].name.as_str(),
            saved[0].host.as_str(),
            saved[0].port
        ),
        ("我的库", "10.0.0.5", 6400)
    );
    assert_eq!(saved[0].ordinal, 7);
    assert_eq!(saved[0].username.as_deref(), Some("alice"));
    // 保存成功弹一条 toast,侧栏里有了这个连接
    wait(cx, &env, "list reloaded", |a| a.connections.len() == 1);
    let id = read(cx, &env, |a| a.connections[0].id);
    assert!(exists(cx, &env, &format!("conn-{id}")));
}

#[gpui_kit::test]
fn invalid_input_shows_the_apps_errors_and_cannot_be_saved(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    assert!(!exists(cx, &env, "editor-errors"));
    type_into(cx, &env, "editor-port", "abc");
    assert!(!read(cx, &env, |a| a.editor.as_ref().unwrap().can_save()));
    assert!(
        exists(cx, &env, "editor-errors"),
        "the App's reason is shown"
    );

    // 按钮禁用:点了没有任何后端调用,编辑器还开着
    click(cx, &env, "editor-save");
    assert!(read(cx, &env, |a| !a.editor.as_ref().unwrap().saving));
    // 回车也一样:提交无效
    press(cx, &env, "enter");
    assert!(read(cx, &env, |a| a.editor.is_some()));
    assert!(stored_hosts(&env).is_empty());
    assert!(
        !env.fake
            .calls()
            .iter()
            .any(|c| c.contains("save_connection"))
    );

    // 改对之后错误消失,可以保存
    type_into(cx, &env, "editor-port", "6379");
    assert!(!exists(cx, &env, "editor-errors"));
    assert!(read(cx, &env, |a| a.editor.as_ref().unwrap().can_save()));
}

#[gpui_kit::test]
fn enter_submits_and_escape_cancels(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    // Esc 取消,什么都没保存
    click(cx, &env, "add-connection");
    type_into(cx, &env, "editor-host", "never-saved");
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| a.editor.is_none()));
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(stored_hosts(&env).is_empty());

    // 在输入框里按回车提交
    click(cx, &env, "add-connection");
    type_into(cx, &env, "editor-host", "enter.example");
    press(cx, &env, "enter");
    wait(cx, &env, "saved by Enter", |a| a.editor.is_none());
    assert_eq!(stored_hosts(&env), vec!["enter.example".to_string()]);
    assert!(open_dialogs(cx, &env).is_empty());
}

#[gpui_kit::test]
fn the_cancel_button_closes_the_editor(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    click(cx, &env, "editor-cancel");
    assert!(read(cx, &env, |a| a.editor.is_none()));
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(!exists(cx, &env, "editor-save"));
    // 再开一次是全新的默认值
    click(cx, &env, "add-connection");
    assert_eq!(
        read(cx, &env, |a| a.editor.as_ref().unwrap().host.clone()),
        "localhost"
    );
}

#[gpui_kit::test]
fn editing_fills_the_fields_keeps_the_password_blank_and_can_clear_it(
    cx: &mut gpui_kit::TestAppContext,
) {
    let (env, ids) = setup(cx, &["缓存库"]);
    dispatch(cx, &env, Msg::OpenEditor(Some(ids[0])));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Editor]);
    let e = read(cx, &env, |a| a.editor.clone().unwrap());
    assert_eq!(e.editing, Some(ids[0]));
    assert_eq!(e.name, "缓存库");
    assert!(e.password.is_empty(), "passwords are never read back");
    assert!(
        exists(cx, &env, "editor-clear-password"),
        "editing offers removing the saved password"
    );
    click(cx, &env, "editor-clear-password");
    assert!(read(cx, &env, |a| a
        .editor
        .as_ref()
        .unwrap()
        .clear_password));
    click(cx, &env, "editor-save");
    wait(cx, &env, "saved", |a| a.editor.is_none());
}

#[gpui_kit::test]
fn tls_shows_the_certificate_rows_and_files_come_from_the_dialog_result(
    cx: &mut gpui_kit::TestAppContext,
) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    click(cx, &env, "editor-tls");
    assert!(read(cx, &env, |a| a.editor.as_ref().unwrap().tls));
    for id in ["editor-ca", "editor-cert", "editor-key"] {
        assert!(exists(cx, &env, &format!("{id}-choose")), "{id}");
        assert!(
            !exists(cx, &env, &format!("{id}-clear")),
            "nothing to clear"
        );
    }

    // 选了一个文件:App 记下路径,界面显示出来、出现清除按钮
    dispatch(
        cx,
        &env,
        Msg::EditorFilePicked(FileField::CaCert, Some(PathBuf::from("/tmp/ca.pem"))),
    );
    assert_eq!(
        read(cx, &env, |a| a.editor.as_ref().unwrap().ca_cert.clone()),
        Some(PathBuf::from("/tmp/ca.pem"))
    );
    assert!(exists(cx, &env, "editor-ca-clear"));
    // 只给客户端证书不给私钥:App 判不通过,保存被禁用
    dispatch(
        cx,
        &env,
        Msg::EditorFilePicked(FileField::ClientCert, Some(PathBuf::from("/tmp/c.pem"))),
    );
    assert!(!read(cx, &env, |a| a.editor.as_ref().unwrap().can_save()));
    assert!(exists(cx, &env, "editor-errors"));
    // 取消文件对话框(None)不改
    dispatch(cx, &env, Msg::EditorFilePicked(FileField::ClientKey, None));
    assert!(read(cx, &env, |a| a
        .editor
        .as_ref()
        .unwrap()
        .client_key
        .is_none()));
    click(cx, &env, "editor-cert-clear");
    assert!(read(cx, &env, |a| a
        .editor
        .as_ref()
        .unwrap()
        .client_cert
        .is_none()));
    assert!(read(cx, &env, |a| a.editor.as_ref().unwrap().can_save()));
    click(cx, &env, "editor-tls");
    assert!(
        !exists(cx, &env, "editor-ca-choose"),
        "TLS off hides the rows"
    );
}

#[gpui_kit::test]
fn the_choose_button_asks_the_platform_for_a_file(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    click(cx, &env, "editor-tls");
    // 点“选择”:App 要求弹文件对话框(PlatformCmd::PickFile),无头平台里没人回答,
    // 编辑器保持不变、不崩溃;对话框的结果走 EditorFilePicked(上一个测试)
    click(cx, &env, "editor-key-choose");
    assert!(read(cx, &env, |a| a.editor.is_some()));
    assert!(read(cx, &env, |a| a
        .editor
        .as_ref()
        .unwrap()
        .client_key
        .is_none()));
}

#[gpui_kit::test]
fn a_failed_save_keeps_the_editor_open_with_the_reason(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    click(cx, &env, "add-connection");
    env.fake.fail_next(
        "save_connection",
        xredis_core::Error::Connect("disk full".into()),
    );
    click(cx, &env, "editor-save");
    wait(cx, &env, "failure reported", |a| {
        a.editor.as_ref().is_some_and(|e| e.save_error.is_some())
    });
    assert!(exists(cx, &env, "editor-errors"));
    assert!(
        read(cx, &env, |a| a.editor.as_ref().unwrap().can_save()),
        "can retry"
    );
    click(cx, &env, "editor-save");
    wait(cx, &env, "saved on retry", |a| a.editor.is_none());
    assert_eq!(stored_hosts(&env).len(), 1);
}

// ---------------------------------------------------------------- 设置 (P-60、P-67)

#[gpui_kit::test]
fn settings_open_validate_save_and_cancel(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    dispatch(cx, &env, Msg::OpenSettings);
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Settings]);
    for id in [
        "settings",
        "settings-batch",
        "settings-auto-scan",
        "settings-show-details",
        "settings-export",
        "settings-lang-en",
        "settings-save",
        "settings-cancel",
    ] {
        assert!(exists(cx, &env, id), "{id}");
    }
    assert!(!exists(cx, &env, "settings-errors"));

    // 不合法:App 给出原因,保存没有效果
    type_into(cx, &env, "settings-batch", "550");
    assert!(exists(cx, &env, "settings-errors"));
    assert!(!read(cx, &env, |a| a
        .settings_editor
        .as_ref()
        .unwrap()
        .can_save(&a.settings)));
    click(cx, &env, "settings-save");
    press(cx, &env, "enter");
    assert!(read(cx, &env, |a| a.settings_editor.is_some()));
    assert_ne!(env.fake.store().settings().scan_batch, 550);

    // 合法之后保存:后端存了,面板关了
    type_into(cx, &env, "settings-batch", "800");
    type_into(cx, &env, "settings-auto-scan", "5000");
    click(cx, &env, "settings-show-details");
    assert!(!read(cx, &env, |a| a
        .settings_editor
        .as_ref()
        .unwrap()
        .show_key_details));
    click(cx, &env, "settings-save");
    wait(cx, &env, "settings closed", |a| a.settings_editor.is_none());
    assert!(open_dialogs(cx, &env).is_empty());
    let s = env.fake.store().settings();
    assert_eq!(
        (s.scan_batch, s.auto_scan_limit, s.show_key_details),
        (800, 5000, false)
    );
    assert_eq!(read(cx, &env, |a| a.settings.scan_batch), 800);

    // Esc 取消:不保存
    dispatch(cx, &env, Msg::OpenSettings);
    type_into(cx, &env, "settings-batch", "1000");
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| a.settings_editor.is_none()));
    assert_eq!(env.fake.store().settings().scan_batch, 800);
    assert!(open_dialogs(cx, &env).is_empty());

    // 取消按钮
    dispatch(cx, &env, Msg::OpenSettings);
    click(cx, &env, "settings-cancel");
    assert!(read(cx, &env, |a| a.settings_editor.is_none()));
}

#[gpui_kit::test]
fn enter_saves_the_settings(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    dispatch(cx, &env, Msg::OpenSettings);
    type_into(cx, &env, "settings-batch", "300");
    press(cx, &env, "enter");
    wait(cx, &env, "saved by Enter", |a| a.settings_editor.is_none());
    assert_eq!(env.fake.store().settings().scan_batch, 300);
}

#[gpui_kit::test]
fn the_language_buttons_and_export_go_through_the_app(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    dispatch(cx, &env, Msg::OpenSettings);
    click(cx, &env, "settings-lang-en");
    assert_eq!(
        read(cx, &env, |a| a
            .settings_editor
            .as_ref()
            .unwrap()
            .language
            .clone()),
        "en"
    );
    // 导出模板:App 发 SaveTextFile,无头平台里对话框没人回答;界面不崩溃、面板还在
    click(cx, &env, "settings-export");
    assert!(read(cx, &env, |a| a.settings_editor.is_some()));
}

// ---------------------------------------------------------------- 免责协议 (P-62)、隐私政策

#[gpui_kit::test]
fn the_agreement_blocks_until_accepted_and_ignores_enter_and_escape(
    cx: &mut gpui_kit::TestAppContext,
) {
    let (env, _) = setup_unagreed(cx, &["a"]);
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Agreement]);
    for id in [
        "agreement",
        "agreement-text",
        "agreement-accept",
        "agreement-decline",
    ] {
        assert!(exists(cx, &env, id), "{id}");
    }
    assert!(!exists(cx, &env, "agreement-close"));
    // v1:不可关闭。Esc、回车都没有效果
    press(cx, &env, "escape");
    press(cx, &env, "enter");
    assert!(read(cx, &env, |a| a.show_agreement && a.blocked()));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Agreement]);

    click(cx, &env, "agreement-accept");
    assert!(read(cx, &env, |a| a.agreed() && !a.show_agreement));
    assert!(open_dialogs(cx, &env).is_empty());
    wait(cx, &env, "agreement saved", |_| {
        env.fake.store().settings().agreement == xredis_app::app::AGREEMENT_VERSION
    });
}

#[gpui_kit::test]
fn declining_the_agreement_records_it_and_exits(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup_unagreed(cx, &[]);
    click(cx, &env, "agreement-decline");
    wait(cx, &env, "decline saved", |_| {
        env.fake.store().settings().agreement.is_empty()
    });
    assert!(!read(cx, &env, |a| a.agreed()));
}

#[gpui_kit::test]
fn the_agreement_from_the_help_menu_is_view_only(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    dispatch(cx, &env, Msg::ShowAgreement(true));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Agreement]);
    assert!(exists(cx, &env, "agreement-close"));
    assert!(!exists(cx, &env, "agreement-accept"));
    click(cx, &env, "agreement-close");
    assert!(read(cx, &env, |a| !a.show_agreement));
    assert!(open_dialogs(cx, &env).is_empty());

    dispatch(cx, &env, Msg::ShowAgreement(true));
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| !a.show_agreement));
    assert!(
        read(cx, &env, |a| a.agreed()),
        "viewing does not change the agreement"
    );
}

#[gpui_kit::test]
fn the_privacy_policy_opens_and_closes(cx: &mut gpui_kit::TestAppContext) {
    let (env, _) = setup(cx, &[]);
    dispatch(cx, &env, Msg::ShowPrivacy(true));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Privacy]);
    assert!(exists(cx, &env, "privacy-text") && exists(cx, &env, "privacy-close"));
    click(cx, &env, "privacy-close");
    assert!(read(cx, &env, |a| !a.show_privacy));
    assert!(open_dialogs(cx, &env).is_empty());

    dispatch(cx, &env, Msg::ShowPrivacy(true));
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| !a.show_privacy));
    dispatch(cx, &env, Msg::ShowPrivacy(true));
    press(cx, &env, "enter");
    assert!(read(cx, &env, |a| !a.show_privacy));
}

// ---------------------------------------------------------------- 删除确认 (P-03)

#[gpui_kit::test]
fn delete_asks_first_and_only_the_ok_button_deletes(cx: &mut gpui_kit::TestAppContext) {
    let (env, ids) = setup(cx, &["a", "b"]);
    dispatch(cx, &env, Msg::DeleteRequested(ids[0]));
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Confirm]);
    for id in ["confirm", "confirm-body", "confirm-ok", "confirm-cancel"] {
        assert!(exists(cx, &env, id), "{id}");
    }
    // 取消按钮、Esc:都不删
    click(cx, &env, "confirm-cancel");
    assert!(read(cx, &env, |a| a.confirm.is_none()));
    assert!(open_dialogs(cx, &env).is_empty());
    dispatch(cx, &env, Msg::DeleteRequested(ids[0]));
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| a.confirm.is_none()));
    assert_eq!(env.fake.store().connections().len(), 2);

    // 确定:删了,侧栏刷新
    dispatch(cx, &env, Msg::DeleteRequested(ids[0]));
    click(cx, &env, "confirm-ok");
    wait(cx, &env, "deleted", |a| a.connections.len() == 1);
    assert_eq!(env.fake.store().connections().len(), 1);
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(!exists(cx, &env, &format!("conn-{}", ids[0])));
}

#[gpui_kit::test]
fn enter_does_not_confirm_the_delete(cx: &mut gpui_kit::TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    dispatch(cx, &env, Msg::DeleteRequested(ids[0]));
    press(cx, &env, "enter");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Confirm]);
    assert!(read(cx, &env, |a| a.confirm.is_some()));
    assert_eq!(env.fake.store().connections().len(), 1);
    assert_eq!(read(cx, &env, |a| a.connections.len()), 1);
    // Esc 仍然取消
    press(cx, &env, "escape");
    assert!(open_dialogs(cx, &env).is_empty());
    assert_eq!(env.fake.store().connections().len(), 1);
}

// ---------------------------------------------------------------- 错误框

fn fail_a_connection(cx: &mut gpui_kit::TestAppContext, env: &Env, id: ConnId) {
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    dispatch(cx, env, Msg::OpenConnection(id));
    wait(cx, env, "failed", is_failed);
}

/// 重试一次让它再失败,等错误框出来。
fn refail(cx: &mut gpui_kit::TestAppContext, env: &Env, id: ConnId) {
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    dispatch(cx, env, Msg::Reconnect(id));
    wait(cx, env, "failed again", |a| a.error_dialog.is_some());
}

#[gpui_kit::test]
fn a_connection_failure_opens_the_error_box_with_the_reason(cx: &mut gpui_kit::TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    fail_a_connection(cx, &env, ids[0]);
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Error]);
    for id in [
        "error-dialog",
        "error-message",
        "error-ok",
        "error-open-log",
    ] {
        assert!(exists(cx, &env, id), "{id}");
    }
    let shown = read(cx, &env, |a| a.error_dialog.clone().unwrap());
    assert!(shown.message.contains("refused"), "{}", shown.message);

    click(cx, &env, "error-ok");
    assert!(read(cx, &env, |a| a.error_dialog.is_none()));
    assert!(open_dialogs(cx, &env).is_empty());
    // Tab 还在失败状态,重试按钮可用
    assert!(matches!(
        read(cx, &env, |a| a.active_tab().map(|t| t.status.clone())),
        Some(TabStatus::Failed(_))
    ));
    assert!(exists(cx, &env, &format!("retry-{}", ids[0])));
}

#[gpui_kit::test]
fn the_error_box_closes_on_enter_and_escape_and_can_open_the_message_log(
    cx: &mut gpui_kit::TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    fail_a_connection(cx, &env, ids[0]);
    press(cx, &env, "enter");
    assert!(read(cx, &env, |a| a.error_dialog.is_none()));

    refail(cx, &env, ids[0]);
    press(cx, &env, "escape");
    assert!(read(cx, &env, |a| a.error_dialog.is_none()));

    // “打开消息日志”:关掉错误框,展开消息列表
    refail(cx, &env, ids[0]);
    click(cx, &env, "error-open-log");
    assert!(read(cx, &env, |a| a.error_dialog.is_none() && a.messages.open));
    assert!(open_dialogs(cx, &env).is_empty());
    assert!(exists(cx, &env, "message-log"));
}

// ---------------------------------------------------------------- 叠在一起

#[gpui_kit::test]
fn dialogs_stack_and_closing_one_in_the_middle_keeps_the_others(cx: &mut gpui_kit::TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    click(cx, &env, "add-connection");
    type_into(cx, &env, "editor-host", "kept.example");
    fail_a_connection(cx, &env, ids[0]);
    assert_eq!(
        open_dialogs(cx, &env),
        vec![DialogKind::Editor, DialogKind::Error]
    );

    // 下面的编辑器被取消:错误框留着
    dispatch(cx, &env, Msg::EditorCancel);
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Error]);
    assert!(exists(cx, &env, "error-ok"));
    click(cx, &env, "error-ok");
    assert!(open_dialogs(cx, &env).is_empty());

    // 编辑器在下、错误框在上,关掉上面的:编辑器的内容还在
    click(cx, &env, "add-connection");
    type_into(cx, &env, "editor-host", "still-here.example");
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    dispatch(cx, &env, Msg::Reconnect(ids[0]));
    wait(cx, &env, "error", |a| a.error_dialog.is_some());
    assert_eq!(
        open_dialogs(cx, &env),
        vec![DialogKind::Editor, DialogKind::Error]
    );
    click(cx, &env, "error-ok");
    assert_eq!(open_dialogs(cx, &env), vec![DialogKind::Editor]);
    assert_eq!(
        read(cx, &env, |a| a.editor.as_ref().unwrap().host.clone()),
        "still-here.example"
    );
}
