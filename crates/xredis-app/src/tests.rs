//! App 的测试:只用假 `Backend`,不需要 Redis。`App::update` 是纯逻辑,
//! 这里直接喂 `Msg`、检查状态和返回的 `Cmd`、再把结果 `Msg` 手动喂回去。
//!
//! 覆盖 P-02 至 P-05、P-07、P-60 至 P-62、P-64、P-66;L-02;第 6 节(密钥存储降级只提示一次)、
//! 第 10 节(关 Tab 后迟到的结果要丢弃)、第 14 节(语言)。

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use xredis_core::Error;
use xredis_core::config::{ConnId, ConnectionConfig, PasswordChange, Settings, TlsConfig};
use xredis_core::conn::info::{ConnectionInfo, KeyspaceInfo};

use crate::app::*;
use crate::forms::FormError;
use crate::messages::{Level, MAX_MESSAGES};
use crate::msg::*;

#[test]
fn app_sees_core() {
    assert_eq!(super::core_name(), "xredis-core");
}

// ---------------------------------------------------------------------------
// 测试用的小工具
// ---------------------------------------------------------------------------

fn cfg(name: &str, ordinal: i32, updated: i64) -> ConnectionConfig {
    ConnectionConfig {
        id: ConnId::new(),
        name: name.into(),
        host: "127.0.0.1".into(),
        port: 6379,
        ordinal,
        selected_db: 0,
        username: None,
        tls: None,
        created_on_utc: updated,
        updated_on_utc: updated,
    }
}

fn info(dbs: usize) -> ConnectionInfo {
    ConnectionInfo {
        databases: (0..dbs as u32).map(KeyspaceInfo::empty).collect(),
    }
}

/// 已经启动、同意过协议的 App,带给定的连接。
fn ready_app(conns: Vec<ConnectionConfig>) -> App {
    let mut app = App::new();
    let settings = Settings {
        agreement: AGREEMENT_VERSION.into(),
        ..Settings::default()
    };
    app.update(Msg::StartupDone(Box::new(StartupData {
        settings,
        connections: conns,
        ..Default::default()
    })));
    app
}

fn backend_cmds(cmds: &[Cmd]) -> Vec<&BackendCmd> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Backend(b) => Some(b),
            _ => None,
        })
        .collect()
}

fn platform_cmds(cmds: &[Cmd]) -> Vec<&PlatformCmd> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Platform(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// 打开连接并让它就绪,返回连接 id。
fn open_ready(app: &mut App, id: ConnId, dbs: usize) {
    let cmds = app.update(Msg::OpenConnection(id));
    let epoch = match backend_cmds(&cmds).as_slice() {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        other => panic!("{other:?}"),
    };
    app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(info(dbs)),
    });
}

fn latest_text(app: &App) -> String {
    app.messages.latest().expect("a message").text.clone()
}

// ---------------------------------------------------------------------------
// 启动(P-66)、协议(P-62)
// ---------------------------------------------------------------------------

#[test]
fn start_asks_the_backend_to_load_everything() {
    let mut app = App::new();
    let cmds = app.start();
    assert!(matches!(backend_cmds(&cmds)[..], [BackendCmd::Startup]));
    assert!(!app.started);
}

/// P-03:连接列表按 ordinal 升序,同序号更新时间新的在前。
#[test]
fn p03_connections_are_sorted_by_ordinal_then_updated_desc() {
    let (a, b, c) = (cfg("a", 5, 100), cfg("b", 1, 50), cfg("c", 5, 900));
    let want = vec![b.id, c.id, a.id];
    let mut app = ready_app(vec![a, b, c]);
    let got: Vec<_> = app.connections.iter().map(|c| c.id).collect();
    assert_eq!(got, want);
    // 重新加载同样排序
    let (x, y) = (cfg("x", 2, 1), cfg("y", 1, 1));
    let want = vec![y.id, x.id];
    app.update(Msg::ConnectionsLoaded(vec![x, y]));
    assert_eq!(
        app.connections.iter().map(|c| c.id).collect::<Vec<_>>(),
        want
    );
}

/// P-66:启动时配置有问题(坏文件已备份、目录不可用)用默认值继续,提示用户,不崩溃。
#[test]
fn p66_startup_warnings_are_shown_and_the_app_keeps_running() {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        warnings: vec!["Config file broken; it was saved as config.json.broken.".into()],
        ..Default::default()
    })));
    assert!(app.started);
    assert_eq!(app.connections.len(), 0);
    assert_eq!(app.settings, Settings::default());
    let m = app.messages.latest().unwrap();
    assert_eq!(m.level, Level::Warn);
    assert!(m.text.contains("config.json.broken"), "{}", m.text);
    assert_eq!(app.messages.toasts().len(), 1);
}

/// P-62:第一次启动(没同意过)显示协议,这时用户操作一律不生效,只有同意和拒绝能点。
#[test]
fn p62_agreement_blocks_everything_until_accepted() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        connections: vec![c],
        ..Default::default()
    })));
    assert!(app.show_agreement && !app.agreed() && app.blocked());

    for m in [
        Msg::OpenConnection(id),
        Msg::OpenEditor(None),
        Msg::OpenSettings,
        Msg::ToggleSider,
    ] {
        assert!(app.update(m).is_empty());
    }
    assert!(app.tabs.is_empty() && app.editor.is_none() && app.settings_editor.is_none());

    // 同意:记下版本,关弹窗,保存
    let cmds = app.update(Msg::AgreeDisclaimer);
    assert!(!app.show_agreement && app.agreed() && !app.blocked());
    match backend_cmds(&cmds).as_slice() {
        [BackendCmd::SaveSettings { settings, .. }] => {
            assert_eq!(settings.agreement, AGREEMENT_VERSION)
        }
        other => panic!("{other:?}"),
    }
    // 现在可以操作了
    assert!(!app.update(Msg::OpenConnection(id)).is_empty());
}

/// P-62:拒绝 = 保存“没同意”,**保存完**再退出(不能让退出把写入截断);保存失败也照样退出。
#[test]
fn p62_decline_saves_first_then_exits_even_if_saving_fails() {
    for save_fails in [false, true] {
        let mut app = App::new();
        app.update(Msg::StartupDone(Box::default()));
        let cmds = app.update(Msg::DeclineDisclaimer);
        let token = match backend_cmds(&cmds).as_slice() {
            [BackendCmd::SaveSettings { settings, token }] => {
                assert_eq!(settings.agreement, "");
                *token
            }
            other => panic!("{other:?}"),
        };
        assert!(
            platform_cmds(&cmds).is_empty(),
            "must not exit before the save finishes"
        );
        let after = app.update(Msg::SettingsSaved {
            token,
            result: if save_fails {
                Err(Error::Config("disk full".into()))
            } else {
                Ok(())
            },
        });
        assert!(
            matches!(platform_cmds(&after)[..], [PlatformCmd::Exit]),
            "save_fails={save_fails}"
        );
    }
}

/// 已经同意过的用户从 Help 菜单再打开协议、隐私政策,不受“挡住”规则影响;外链走平台命令。
#[test]
fn help_menu_items() {
    let mut app = ready_app(vec![]);
    app.update(Msg::ShowAgreement(true));
    assert!(app.show_agreement && app.agreed() && !app.blocked());
    app.update(Msg::ShowAgreement(false));
    app.update(Msg::ShowPrivacy(true));
    assert!(app.show_privacy);
    let cmds = app.update(Msg::OpenHelpLink(HelpLink::ReportBugs));
    assert!(matches!(
        platform_cmds(&cmds)[..],
        [PlatformCmd::OpenUrl(u)] if u == "https://github.com/DreamvatLab/xRedis/issues"
    ));
    let cmds = app.update(Msg::OpenHelpLink(HelpLink::BuyCoffee));
    assert!(matches!(
        platform_cmds(&cmds)[..],
        [PlatformCmd::OpenUrl(u)] if u == "https://www.buymeacoffee.com/dreamvat"
    ));
}

// ---------------------------------------------------------------------------
// Tab(P-04)、L-02、第 10 节
// ---------------------------------------------------------------------------

/// P-04:每个连接一个 Tab,再点同一个连接只是激活;关 Tab 后激活第一个剩下的。
#[test]
fn p04_tabs_open_activate_and_close() {
    let (a, b) = (cfg("a", 0, 1), cfg("b", 1, 1));
    let (ia, ib) = (a.id, b.id);
    let mut app = ready_app(vec![a, b]);
    assert!(app.active.is_none());

    app.update(Msg::OpenConnection(ia));
    app.update(Msg::OpenConnection(ib));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active, Some(ib));

    // 再点已经打开的:不新建、不重连,只激活
    let cmds = app.update(Msg::OpenConnection(ia));
    assert!(cmds.is_empty());
    assert_eq!((app.tabs.len(), app.active), (2, Some(ia)));

    // 关当前激活的:激活剩下的第一个
    app.update(Msg::CloseTab(ia));
    assert_eq!((app.tabs.len(), app.active), (1, Some(ib)));
    app.update(Msg::CloseTab(ib));
    assert!(app.tabs.is_empty() && app.active.is_none());

    // 不存在的连接打不开
    assert!(app.update(Msg::OpenConnection(ConnId::new())).is_empty());
}

/// L-02:没就绪之前不能发键操作(连接中、失败、已关闭都不行),就绪后可以。
#[test]
fn l02_no_requests_until_the_connection_is_ready() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    assert!(!app.can_request(id), "no tab yet");

    let cmds = app.update(Msg::OpenConnection(id));
    assert_eq!(app.tab(id).unwrap().status, TabStatus::Connecting);
    assert!(
        !app.can_request(id),
        "connecting: a scan now would build the pool a second time"
    );
    let epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(info(16)),
    });
    assert!(app.can_request(id));

    app.update(Msg::CloseTab(id));
    assert!(!app.can_request(id), "closed");
}

/// P-04、L-02:关 Tab 要立刻生效(先移除 Tab,后台关连接),即使连接还在建、后端关连接会挂住。
#[test]
fn p04_closing_a_tab_is_immediate_and_closes_the_connection_in_the_background() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    app.update(Msg::OpenConnection(id)); // 还在 Connecting
    let cmds = app.update(Msg::CloseTab(id));
    assert!(
        app.tabs.is_empty(),
        "the tab is gone before the backend answers"
    );
    assert!(matches!(backend_cmds(&cmds)[..], [BackendCmd::CloseConnection(i)] if *i == id));
    assert!(
        app.update(Msg::CloseTab(id)).is_empty(),
        "closing twice is a no-op"
    );
}

/// 第 10 节:关 Tab 之后迟到的结果要丢弃,不能让已经关掉的 Tab 复活,也不能弹错误框;
/// 重新打开后,旧请求的结果(纪元对不上)同样丢弃。
#[test]
fn late_results_after_closing_or_reopening_are_discarded() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);

    let cmds = app.update(Msg::OpenConnection(id));
    let old_epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    app.update(Msg::CloseTab(id));
    // 旧请求的成功和失败都迟到了
    app.update(Msg::Connected {
        id,
        epoch: old_epoch,
        result: Ok(info(16)),
    });
    assert!(app.tabs.is_empty());
    app.update(Msg::Connected {
        id,
        epoch: old_epoch,
        result: Err(Error::Timeout("late".into())),
    });
    assert!(app.tabs.is_empty() && app.error_dialog.is_none() && app.messages.is_empty());

    // 重新打开:新纪元;旧纪元的结果不能覆盖它
    let cmds = app.update(Msg::OpenConnection(id));
    let new_epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    assert_ne!(old_epoch, new_epoch);
    app.update(Msg::Connected {
        id,
        epoch: old_epoch,
        result: Err(Error::Timeout("stale".into())),
    });
    assert_eq!(app.tab(id).unwrap().status, TabStatus::Connecting);
    assert!(app.error_dialog.is_none());
    app.update(Msg::Connected {
        id,
        epoch: new_epoch,
        result: Ok(info(16)),
    });
    assert!(app.can_request(id));
}

/// P-07:连接失败显示错误框(带原因),可以打开消息日志,可以重试。
#[test]
fn p07_connection_failure_shows_a_dialog_with_the_reason_and_can_retry() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let cmds = app.update(Msg::OpenConnection(id));
    let epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    app.update(Msg::Connected {
        id,
        epoch,
        result: Err(Error::Auth(
            "WRONGPASS invalid username-password pair".into(),
        )),
    });
    let dialog = app.error_dialog.clone().expect("error dialog");
    assert_eq!(dialog.title, "Connection failed");
    assert!(
        dialog.message.contains("Authentication failed") && dialog.message.contains("WRONGPASS"),
        "{}",
        dialog.message
    );
    assert!(matches!(&app.tab(id).unwrap().status, TabStatus::Failed(m) if m == &dialog.message));
    assert!(!app.can_request(id));
    assert_eq!(app.messages.latest().unwrap().level, Level::Error);

    // 打开消息日志:关掉错误框,展开消息列表
    app.update(Msg::OpenMessageLog);
    assert!(app.error_dialog.is_none() && app.messages.open);

    // 重试:新纪元,回到连接中
    let cmds = app.update(Msg::Reconnect(id));
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::Connect { .. }]
    ));
    assert_eq!(app.tab(id).unwrap().status, TabStatus::Connecting);
    // 不是失败状态时重试什么也不做
    assert!(app.update(Msg::Reconnect(id)).is_empty());
}

/// 连接被别处关掉(`Closed`)也不能让 Tab 永远停在“连接中”:按失败显示,可以重试。
#[test]
fn a_closed_error_does_not_leave_the_tab_connecting_forever() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let cmds = app.update(Msg::OpenConnection(id));
    let epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    app.update(Msg::Connected {
        id,
        epoch,
        result: Err(Error::Closed),
    });
    assert!(matches!(app.tab(id).unwrap().status, TabStatus::Failed(_)));
}

// ---------------------------------------------------------------------------
// 切库(P-05)
// ---------------------------------------------------------------------------

#[test]
fn p05_select_db_switches_reconnects_and_remembers_the_db() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    open_ready(&mut app, id, 16);

    let cmds = app.update(Msg::SelectDb { id, db: 3 });
    let epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::SelectDb { epoch, db: 3, .. }] => *epoch,
        _ => panic!("{cmds:?}"),
    };
    assert!(matches!(
        app.tab(id).unwrap().status,
        TabStatus::Switching { db: 3, .. }
    ));
    assert!(
        !app.can_request(id),
        "no requests while the connection is being rebuilt"
    );

    // 切成功:记下库号,重新取连接信息
    let cmds = app.update(Msg::DbSelected {
        id,
        epoch,
        db: 3,
        result: Ok(()),
    });
    assert_eq!(app.connection(id).unwrap().selected_db, 3);
    let epoch2 = match backend_cmds(&cmds)[..] {
        [BackendCmd::Connect { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    assert_eq!(app.tab(id).unwrap().status, TabStatus::Connecting);
    app.update(Msg::Connected {
        id,
        epoch: epoch2,
        result: Ok(info(16)),
    });
    assert!(app.can_request(id));
}

#[test]
fn p05_failed_switch_restores_the_previous_state_and_reports() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    open_ready(&mut app, id, 16);
    let cmds = app.update(Msg::SelectDb { id, db: 2 });
    let epoch = match backend_cmds(&cmds)[..] {
        [BackendCmd::SelectDb { epoch, .. }] => *epoch,
        _ => panic!(),
    };
    app.update(Msg::DbSelected {
        id,
        epoch,
        db: 2,
        result: Err(Error::Redis("ERR DB index is out of range".into())),
    });
    assert!(app.can_request(id), "back to ready");
    assert_eq!(
        app.connection(id).unwrap().selected_db,
        0,
        "the db did not change"
    );
    assert!(latest_text(&app).contains("DB index"));
}

/// 没就绪、库号不在列表里都不切;切库时不会白白消耗纪元导致别的结果对不上。
#[test]
fn p05_select_db_is_refused_when_not_ready_or_out_of_range() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    assert!(app.update(Msg::SelectDb { id, db: 1 }).is_empty(), "no tab");
    app.update(Msg::OpenConnection(id));
    assert!(
        app.update(Msg::SelectDb { id, db: 1 }).is_empty(),
        "still connecting"
    );
    let epoch = app.tab(id).unwrap().epoch;
    app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(info(4)),
    });
    assert!(
        app.update(Msg::SelectDb { id, db: 4 }).is_empty(),
        "db 4 does not exist (0 to 3)"
    );
    assert!(app.can_request(id));
    assert!(!app.update(Msg::SelectDb { id, db: 3 }).is_empty());
}

// ---------------------------------------------------------------------------
// 连接编辑器(P-02)
// ---------------------------------------------------------------------------

fn editor_of(app: &App) -> &crate::forms::ConnectionEditor {
    app.editor.as_ref().expect("editor open")
}

#[test]
fn p02_new_connection_has_v1_defaults_and_is_valid() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    let e = editor_of(&app);
    assert_eq!(
        (e.host.as_str(), e.port.as_str(), e.ordinal.as_str()),
        ("localhost", "6379", "0")
    );
    assert!(e.editing.is_none() && !e.tls);
    let draft = e.validate().unwrap();
    assert!(draft.id.is_none() && draft.tls.is_none());
    assert_eq!(
        (draft.host.as_str(), draft.port, draft.ordinal),
        ("localhost", 6379, 0)
    );
    assert_eq!(draft.password, PasswordChange::Keep);
}

/// P-02:Host 必填、Port 1 到 65535、Ordinal 在 -65535 到 65535;所有错误一起报,不是只报第一个。
#[test]
fn p02_input_rules() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    let set = |app: &mut App, f, v: &str| {
        app.update(Msg::EditorSetText(f, v.into()));
    };
    set(&mut app, EditorText::Host, "   ");
    set(&mut app, EditorText::Port, "0");
    set(&mut app, EditorText::Ordinal, "70000");
    assert_eq!(
        editor_of(&app).errors(),
        vec![
            FormError::HostRequired,
            FormError::PortInvalid,
            FormError::OrdinalInvalid
        ]
    );
    assert!(!editor_of(&app).can_save());

    for bad_port in ["", "abc", "65536", "-1", "1.5", "99999999999"] {
        set(&mut app, EditorText::Host, "h");
        set(&mut app, EditorText::Ordinal, "0");
        set(&mut app, EditorText::Port, bad_port);
        assert_eq!(
            editor_of(&app).errors(),
            vec![FormError::PortInvalid],
            "port {bad_port:?}"
        );
    }
    for ok_port in ["1", "6379", "65535", " 6380 "] {
        set(&mut app, EditorText::Port, ok_port);
        assert!(editor_of(&app).errors().is_empty(), "port {ok_port:?}");
    }
    for (ord, ok) in [
        ("-65535", true),
        ("65535", true),
        ("-65536", false),
        ("65536", false),
        ("x", false),
        ("", true),
    ] {
        set(&mut app, EditorText::Ordinal, ord);
        assert_eq!(editor_of(&app).errors().is_empty(), ok, "ordinal {ord:?}");
    }
    // 空的序号当 0
    set(&mut app, EditorText::Ordinal, "");
    assert_eq!(editor_of(&app).validate().unwrap().ordinal, 0);
}

/// P-02:TLS 下 CA 证书**可选**(v1 前端强制要求填,是 bug);客户端证书和私钥要成对;
/// 关掉 TLS 后证书路径不进草稿。
#[test]
fn p02_tls_ca_is_optional_and_client_cert_and_key_come_together() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSetTls(true));
    assert!(
        editor_of(&app).errors().is_empty(),
        "TLS without a CA must be allowed"
    );
    assert_eq!(
        editor_of(&app).validate().unwrap().tls,
        Some(TlsConfig::default())
    );

    app.update(Msg::EditorFilePicked(
        FileField::ClientCert,
        Some("c.pem".into()),
    ));
    assert_eq!(editor_of(&app).errors(), vec![FormError::CertPair]);
    app.update(Msg::EditorFilePicked(
        FileField::ClientKey,
        Some("k.pem".into()),
    ));
    app.update(Msg::EditorFilePicked(
        FileField::CaCert,
        Some("ca.pem".into()),
    ));
    let tls = editor_of(&app).validate().unwrap().tls.unwrap();
    assert_eq!(tls.ca_cert.as_deref(), Some("ca.pem".as_ref()));
    assert_eq!(tls.client_key.as_deref(), Some("k.pem".as_ref()));

    // 清掉一个文件
    app.update(Msg::EditorClearFile(FileField::ClientKey));
    assert_eq!(editor_of(&app).errors(), vec![FormError::CertPair]);

    // 取消文件对话框不改字段;关掉 TLS 后证书对不成对也不要紧,草稿里没有 TLS
    app.update(Msg::EditorFilePicked(FileField::ClientKey, None));
    app.update(Msg::EditorSetTls(false));
    assert!(editor_of(&app).errors().is_empty());
    assert!(editor_of(&app).validate().unwrap().tls.is_none());
}

#[test]
fn p02_pick_file_asks_the_platform_for_a_dialog() {
    let mut app = ready_app(vec![]);
    assert!(
        app.update(Msg::EditorPickFile(FileField::CaCert))
            .is_empty(),
        "no editor open"
    );
    app.update(Msg::OpenEditor(None));
    let cmds = app.update(Msg::EditorPickFile(FileField::ClientKey));
    assert!(matches!(
        platform_cmds(&cmds)[..],
        [PlatformCmd::PickFile(FileField::ClientKey)]
    ));
}

/// P-02:编辑已有连接时密码框是空的(密码不会读出来),留空 = 不改;输入 = 改;勾选“清除” = 清空。
#[test]
fn p02_editing_an_existing_connection_password_semantics() {
    let mut c = cfg("prod", 7, 1);
    c.username = Some("alice".into());
    c.tls = Some(TlsConfig {
        ca_cert: Some("ca.pem".into()),
        ..Default::default()
    });
    let id = c.id;
    let mut app = ready_app(vec![c]);
    app.update(Msg::OpenEditor(Some(id)));
    let e = editor_of(&app);
    assert_eq!(e.editing, Some(id));
    assert_eq!(
        (e.name.as_str(), e.username.as_str(), e.ordinal.as_str()),
        ("prod", "alice", "7")
    );
    assert!(e.tls && e.ca_cert.is_some());
    assert_eq!(e.password, "", "the saved password is never shown");

    assert_eq!(
        editor_of(&app).validate().unwrap().password,
        PasswordChange::Keep
    );
    app.update(Msg::EditorSetText(EditorText::Password, "new-pw".into()));
    assert_eq!(
        editor_of(&app).validate().unwrap().password,
        PasswordChange::Set("new-pw".into())
    );
    app.update(Msg::EditorSetText(EditorText::Password, String::new()));
    app.update(Msg::EditorClearPassword(true));
    assert_eq!(
        editor_of(&app).validate().unwrap().password,
        PasswordChange::Clear
    );
    // 填了新密码就是改,不是清
    app.update(Msg::EditorSetText(EditorText::Password, "x".into()));
    assert_eq!(
        editor_of(&app).validate().unwrap().password,
        PasswordChange::Set("x".into())
    );
    assert_eq!(editor_of(&app).validate().unwrap().id, Some(id));

    // 新建连接没有“清除”这回事
    app.update(Msg::EditorCancel);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorClearPassword(true));
    assert_eq!(
        editor_of(&app).validate().unwrap().password,
        PasswordChange::Keep
    );
    // 打开一个不存在的连接:编辑器不打开
    app.update(Msg::EditorCancel);
    app.update(Msg::OpenEditor(Some(ConnId::new())));
    assert!(app.editor.is_none());
}

#[test]
fn p02_username_and_text_are_trimmed_and_blank_username_is_none() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSetText(
        EditorText::Host,
        "  redis.local ".into(),
    ));
    app.update(Msg::EditorSetText(EditorText::Username, "   ".into()));
    app.update(Msg::EditorSetText(EditorText::Name, "  我的 Redis ".into()));
    let d = editor_of(&app).validate().unwrap();
    assert_eq!(
        (d.host.as_str(), d.name.as_str(), d.username),
        ("redis.local", "我的 Redis", None)
    );
    app.update(Msg::EditorSetText(EditorText::Username, " bob ".into()));
    assert_eq!(
        editor_of(&app).validate().unwrap().username.as_deref(),
        Some("bob")
    );
}

/// 保存:合法才发命令,保存中不能重复发(防止点两次造成两个连接);非法时不发。
#[test]
fn editor_save_sends_once_and_only_when_valid() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSetText(EditorText::Port, "0".into()));
    assert!(
        app.update(Msg::EditorSave).is_empty(),
        "invalid input is not sent"
    );
    assert!(!editor_of(&app).saving);

    app.update(Msg::EditorSetText(EditorText::Port, "6380".into()));
    let cmds = app.update(Msg::EditorSave);
    assert!(matches!(backend_cmds(&cmds)[..], [BackendCmd::SaveConnection(d)] if d.port == 6380));
    assert!(editor_of(&app).saving && !editor_of(&app).can_save());
    assert!(
        app.update(Msg::EditorSave).is_empty(),
        "a second click while saving must not create a duplicate connection"
    );
}

#[test]
fn editor_save_success_closes_the_editor_and_reloads_the_list() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSave);
    let id = ConnId::new();
    let cmds = app.update(Msg::ConnectionSaved(Ok(Saved {
        outcome: xredis_core::config::SaveOutcome {
            id,
            warnings: vec![],
            password_error: None,
        },
        keystore_degraded: false,
    })));
    assert!(app.editor.is_none());
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::LoadConnections]
    ));
    assert_eq!(latest_text(&app), "Connection saved.");
    assert_eq!(app.messages.latest().unwrap().level, Level::Success);
}

/// 第 7 节:连接已保存、密码没存上不当成保存失败,编辑器照样关闭(否则用户会再点一次,出现重复连接),
/// 另外给一条翻译过的警告。
#[test]
fn a_password_that_could_not_be_stored_is_a_warning_not_a_failure() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(Ok(Saved {
        outcome: xredis_core::config::SaveOutcome {
            id: ConnId::new(),
            warnings: vec!["English sentence for logs".into()],
            password_error: Some("locked".into()),
        },
        keystore_degraded: false,
    })));
    assert!(
        app.editor.is_none(),
        "the connection was saved, the editor closes"
    );
    let texts: Vec<_> = app.messages.entries().map(|e| e.text.clone()).collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("password could not be stored: locked")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .all(|t| !t.contains("English sentence for logs")),
        "the UI must not show core's English sentence"
    );
}

/// 保存失败:编辑器保持打开、可以再点、显示原因;改动输入后清掉旧错误。
#[test]
fn editor_save_failure_keeps_the_editor_open_with_the_reason() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(Err(Error::Config("disk full".into()))));
    let e = editor_of(&app);
    assert!(!e.saving && e.can_save());
    assert!(e.save_error.as_deref().unwrap().contains("disk full"));
    app.update(Msg::EditorSetText(EditorText::Name, "x".into()));
    assert!(editor_of(&app).save_error.is_none());
}

// ---------------------------------------------------------------------------
// 删除连接(P-03)
// ---------------------------------------------------------------------------

#[test]
fn p03_delete_asks_for_confirmation_then_closes_the_tab_and_deletes() {
    let mut named = cfg("prod", 0, 1);
    named.name = "prod".into();
    let unnamed = cfg("", 1, 1);
    let (id, id2) = (named.id, unnamed.id);
    let mut app = ready_app(vec![named, unnamed]);
    open_ready(&mut app, id, 16);

    assert!(app.update(Msg::DeleteRequested(id)).is_empty());
    assert_eq!(
        app.confirm,
        Some(Confirm::DeleteConnection {
            id,
            name: "prod".into()
        })
    );
    // 取消:什么都不发生
    assert!(app.update(Msg::ConfirmCancelled).is_empty());
    assert!(app.confirm.is_none() && app.tab(id).is_some());

    // 没有名字的连接用 host:port 显示
    app.update(Msg::DeleteRequested(id2));
    assert_eq!(
        app.confirm,
        Some(Confirm::DeleteConnection {
            id: id2,
            name: "127.0.0.1:6379".into()
        })
    );
    app.update(Msg::ConfirmCancelled);

    app.update(Msg::DeleteRequested(id));
    let cmds = app.update(Msg::ConfirmAccepted);
    assert!(app.tab(id).is_none(), "the tab goes first");
    let cmds = backend_cmds(&cmds);
    assert!(
        matches!(cmds[..], [BackendCmd::CloseConnection(a), BackendCmd::DeleteConnection(b)] if *a == id && *b == id)
    );
    assert!(app.confirm.is_none());

    let cmds = app.update(Msg::ConnectionDeleted { id, result: Ok(()) });
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::LoadConnections]
    ));
    assert_eq!(latest_text(&app), "Connection deleted.");
    // 确认框没开时 ConfirmAccepted 什么也不做
    assert!(app.update(Msg::ConfirmAccepted).is_empty());
}

#[test]
fn a_failed_delete_reports_and_reloads_the_list_from_the_backend() {
    let c = cfg("a", 0, 1);
    let id = c.id;
    let mut app = ready_app(vec![c]);
    let cmds = app.update(Msg::ConnectionDeleted {
        id,
        result: Err(Error::Config("locked".into())),
    });
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::LoadConnections]
    ));
    assert_eq!(app.messages.latest().unwrap().level, Level::Error);
}

// ---------------------------------------------------------------------------
// 侧栏折叠(P-03)、设置(P-60)
// ---------------------------------------------------------------------------

#[test]
fn p03_sider_collapse_toggles_and_is_persisted() {
    let mut app = ready_app(vec![]);
    assert!(!app.settings.sider_menu_collapsed);
    let cmds = app.update(Msg::ToggleSider);
    assert!(app.settings.sider_menu_collapsed);
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::SaveSettings { settings, .. }] if settings.sider_menu_collapsed
    ));
    app.update(Msg::ToggleSider);
    assert!(!app.settings.sider_menu_collapsed);
}

/// P-60:单批 100 到 2000(步长 100),自动续扫上限 0 到 100000(步长 1000,0 是关闭);
/// 不合法时不发保存。
#[test]
fn p60_settings_validation() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenSettings);
    let check = |app: &mut App, batch: &str, auto: &str| {
        app.update(Msg::SettingsSetText(SettingsText::ScanBatch, batch.into()));
        app.update(Msg::SettingsSetText(
            SettingsText::AutoScanLimit,
            auto.into(),
        ));
        app.settings_editor.as_ref().unwrap().errors(&app.settings)
    };
    assert!(check(&mut app, "500", "100000").is_empty(), "the defaults");
    assert!(
        check(&mut app, "100", "0").is_empty(),
        "lower bounds, auto-scan off"
    );
    assert!(check(&mut app, "2000", "1000").is_empty());
    assert_eq!(check(&mut app, "99", "0"), vec![FormError::BatchInvalid]);
    assert_eq!(check(&mut app, "2100", "0"), vec![FormError::BatchInvalid]);
    assert_eq!(
        check(&mut app, "550", "0"),
        vec![FormError::BatchInvalid],
        "step is 100"
    );
    assert_eq!(check(&mut app, "abc", "0"), vec![FormError::BatchInvalid]);
    assert_eq!(
        check(&mut app, "500", "100001"),
        vec![FormError::AutoScanInvalid]
    );
    assert_eq!(
        check(&mut app, "500", "1500"),
        vec![FormError::AutoScanInvalid],
        "step is 1000"
    );
    assert_eq!(
        check(&mut app, "500", "-1"),
        vec![FormError::AutoScanInvalid]
    );
    assert_eq!(
        check(&mut app, "1", "1"),
        vec![FormError::BatchInvalid, FormError::AutoScanInvalid]
    );

    check(&mut app, "99", "0");
    assert!(
        app.update(Msg::SettingsSave).is_empty(),
        "invalid settings are not saved"
    );
}

/// P-60:保存只改设置面板能改的字段,窗口尺寸、折叠状态、协议记录保持不变;
/// 成功之后才变成正式设置并关面板;失败时面板留着。
#[test]
fn p60_settings_save_applies_only_after_the_backend_confirms() {
    let mut app = ready_app(vec![]);
    app.window = WindowState {
        width: 1111,
        height: 999,
        maximized: true,
    };
    app.settings.window_width = 1111;
    app.settings.sider_menu_collapsed = true;

    app.update(Msg::OpenSettings);
    app.update(Msg::SettingsSetText(SettingsText::ScanBatch, "800".into()));
    app.update(Msg::SettingsSetText(
        SettingsText::AutoScanLimit,
        "0".into(),
    ));
    app.update(Msg::SettingsSetShowDetails(false));
    let cmds = app.update(Msg::SettingsSave);
    let (token, sent) = match backend_cmds(&cmds)[..] {
        [BackendCmd::SaveSettings { settings, token }] => (*token, settings.clone()),
        _ => panic!(),
    };
    assert_eq!(
        (sent.scan_batch, sent.auto_scan_limit, sent.show_key_details),
        (800, 0, false)
    );
    assert_eq!(sent.window_width, 1111, "untouched fields are kept");
    assert!(sent.sider_menu_collapsed);
    assert_eq!(sent.agreement, AGREEMENT_VERSION);
    // 还没确认:正式设置没变
    assert_eq!(app.settings.scan_batch, 500);
    assert!(app.settings_editor.as_ref().unwrap().saving);
    assert!(app.update(Msg::SettingsSave).is_empty(), "no double submit");

    // 失败:面板留着,可以再点
    app.update(Msg::SettingsSaved {
        token,
        result: Err(Error::Config("disk full".into())),
    });
    assert!(app.settings_editor.is_some() && !app.settings_editor.as_ref().unwrap().saving);
    assert_eq!(app.settings.scan_batch, 500);
    assert_eq!(app.messages.latest().unwrap().level, Level::Error);

    // 再保存,成功
    let cmds = app.update(Msg::SettingsSave);
    let token = match backend_cmds(&cmds)[..] {
        [BackendCmd::SaveSettings { token, .. }] => *token,
        _ => panic!(),
    };
    app.update(Msg::SettingsSaved {
        token,
        result: Ok(()),
    });
    assert!(app.settings_editor.is_none());
    assert_eq!(
        (app.settings.scan_batch, app.settings.auto_scan_limit),
        (800, 0)
    );
    assert!(!app.settings.show_key_details);
    assert_eq!(latest_text(&app), "Settings saved.");
}

/// 别的保存(折叠侧栏)的结果,不能被当成设置面板的保存结果而把面板关掉。
#[test]
fn another_save_result_does_not_close_the_settings_panel() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenSettings);
    let cmds = app.update(Msg::ToggleSider);
    let toggle_token = match backend_cmds(&cmds)[..] {
        [BackendCmd::SaveSettings { token, .. }] => *token,
        _ => panic!(),
    };
    app.update(Msg::SettingsSave);
    app.update(Msg::SettingsSaved {
        token: toggle_token,
        result: Ok(()),
    });
    assert!(
        app.settings_editor.is_some(),
        "the panel's own save has not finished yet"
    );
    // 不认识的 token 忽略
    assert!(
        app.update(Msg::SettingsSaved {
            token: 9999,
            result: Ok(())
        })
        .is_empty()
    );
}

// ---------------------------------------------------------------------------
// 语言(第 14 节)
// ---------------------------------------------------------------------------

/// 选一个不可用的语言(语言包被删了)回落英文并提示;可用的话用它。
#[test]
fn startup_falls_back_to_english_when_the_saved_language_is_unavailable() {
    let mut app = App::new();
    let cmds = app.update(Msg::StartupDone(Box::new(StartupData {
        settings: Settings {
            language: "xx-GONE".into(),
            agreement: AGREEMENT_VERSION.into(),
            ..Default::default()
        },
        ..Default::default()
    })));
    assert_eq!(app.lang, "en");
    assert!(matches!(platform_cmds(&cmds)[..], [PlatformCmd::SetLocale(l)] if l == "en"));
    assert!(
        latest_text(&app).contains("xx-GONE"),
        "{}",
        latest_text(&app)
    );
}

/// 在设置里切换语言:保存成功后才切;切换时要通知 GUI 去切全局 locale。
#[test]
fn changing_the_language_is_applied_after_a_successful_save_and_tells_the_gui() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenSettings);
    // "en" 以外的语言要可用才会生效;这里用 en 本身验证“没变就不通知”
    let cmds = app.update(Msg::SettingsSave);
    let token = match backend_cmds(&cmds)[..] {
        [BackendCmd::SaveSettings { token, .. }] => *token,
        _ => panic!(),
    };
    let cmds = app.update(Msg::SettingsSaved {
        token,
        result: Ok(()),
    });
    assert!(
        platform_cmds(&cmds).is_empty(),
        "language unchanged: no SetLocale"
    );
}

/// 导出语言包模板:让平台弹“另存为”,内容是合法的语言包;结果提示翻译过。
#[test]
fn exporting_the_language_pack_template() {
    let mut app = ready_app(vec![]);
    app.update(Msg::OpenSettings);
    let cmds = app.update(Msg::ExportLanguageTemplate);
    match platform_cmds(&cmds)[..] {
        [
            PlatformCmd::SaveTextFile {
                suggested_name,
                contents,
            },
        ] => {
            assert_eq!(suggested_name, "xx.yml");
            assert!(contents.starts_with("_version: 2"));
            assert!(contents.contains("editor.title:"));
        }
        _ => panic!("{cmds:?}"),
    }
    app.update(Msg::TemplateSaved(Ok(Some("C:/x/xx.yml".into()))));
    assert!(latest_text(&app).contains("C:/x/xx.yml"));
    let n = app.messages.len();
    app.update(Msg::TemplateSaved(Ok(None))); // 用户取消
    assert_eq!(app.messages.len(), n);
    app.update(Msg::TemplateSaved(Err("access denied".into())));
    assert!(latest_text(&app).contains("access denied"));
}

// ---------------------------------------------------------------------------
// 消息栏(P-64)、第 6 节
// ---------------------------------------------------------------------------

/// P-64:最多保留 50 条,最新的显示在底部,老的被挤掉。
#[test]
fn p64_message_bar_keeps_the_latest_50() {
    let t = Arc::new(AtomicI64::new(1000));
    let clock = t.clone();
    let mut app = App::with_clock(Arc::new(move || clock.fetch_add(1, Ordering::SeqCst)));
    for i in 0..(MAX_MESSAGES + 20) {
        app.messages.push(
            Level::Info,
            format!("m{i}"),
            false,
            t.load(Ordering::SeqCst),
        );
    }
    assert_eq!(app.messages.len(), MAX_MESSAGES);
    assert_eq!(
        app.messages.latest().unwrap().text,
        format!("m{}", MAX_MESSAGES + 19)
    );
    assert_eq!(
        app.messages.entries().next().unwrap().text,
        "m20",
        "the oldest 20 were dropped"
    );

    // 消息带时间戳(App 自己记的消息用注入的时钟)
    let before = t.load(Ordering::SeqCst);
    app.update(Msg::WindowSaved(Err(Error::Config("x".into()))));
    assert!(app.messages.latest().unwrap().at_ms >= before);
}

/// P-64:toast 和消息栏是两回事:toast 可以单独关掉,消息还在;清空消息栏两个都清。
#[test]
fn p64_toasts_dismiss_independently_and_the_log_can_be_cleared() {
    let mut app = ready_app(vec![]);
    app.update(Msg::ConnectionDeleted {
        id: ConnId::new(),
        result: Ok(()),
    });
    assert_eq!(app.messages.toasts().len(), 1);
    let id = app.messages.toasts()[0].id;
    app.update(Msg::DismissToast(id));
    assert!(app.messages.toasts().is_empty());
    assert_eq!(app.messages.len(), 1, "the message stays in the log");
    assert!(!app.messages.open);
    app.update(Msg::ToggleMessageLog);
    assert!(app.messages.open);
    app.update(Msg::ClearMessages);
    assert!(app.messages.is_empty());
}

/// 第 6 节:系统密钥存储不可用,消息栏提示一次,之后不再重复(启动时和保存连接时都可能发现)。
#[test]
fn keystore_degradation_is_announced_exactly_once() {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        keystore_degraded: true,
        ..Default::default()
    })));
    let count = |app: &App| {
        app.messages
            .entries()
            .filter(|e| e.text.contains("key store is not available"))
            .count()
    };
    assert_eq!(count(&app), 1);
    // 保存连接时又发现降级:不重复
    app.update(Msg::ConnectionSaved(Ok(Saved {
        outcome: xredis_core::config::SaveOutcome {
            id: ConnId::new(),
            warnings: vec![],
            password_error: None,
        },
        keystore_degraded: true,
    })));
    assert_eq!(count(&app), 1);
}

#[test]
fn keystore_degradation_found_later_is_announced_once_too() {
    let mut app = ready_app(vec![]);
    let saved = || Saved {
        outcome: xredis_core::config::SaveOutcome {
            id: ConnId::new(),
            warnings: vec![],
            password_error: None,
        },
        keystore_degraded: true,
    };
    app.update(Msg::ConnectionSaved(Ok(saved())));
    app.update(Msg::ConnectionSaved(Ok(saved())));
    let n = app
        .messages
        .entries()
        .filter(|e| e.text.contains("key store is not available"))
        .count();
    assert_eq!(n, 1);
}

/// 启动结果里的 v1 导入、语言包加载结果都要告诉用户;失败的语言包带文件名和原因。
#[test]
fn startup_reports_v1_import_and_language_packs() {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        import: Some(ImportSummary {
            connections: 3,
            warnings: vec!["Skipped connection x: invalid port 70000".into()],
            error: None,
        }),
        packs: crate::i18n::PackReport {
            loaded: vec![crate::i18n::LoadedPack {
                file: "zh-CN.yml".into(),
                languages: vec!["zh-CN".into()],
            }],
            errors: vec![crate::i18n::PackError {
                file: "bad.yml".into(),
                reason: "Invalid YAML format".into(),
            }],
        },
        ..Default::default()
    })));
    let texts: Vec<_> = app
        .messages
        .entries()
        .map(|e| (e.level, e.text.clone()))
        .collect();
    assert!(
        texts
            .iter()
            .any(|(l, t)| *l == Level::Success && t.contains("Imported 3 connection(s)")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(l, t)| *l == Level::Warn && t.contains("invalid port 70000")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(_, t)| t.contains("zh-CN.yml") && t.contains("loaded")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(l, t)| *l == Level::Warn && t.contains("bad.yml") && t.contains("Invalid YAML")),
        "{texts:?}"
    );

    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        import: Some(ImportSummary {
            connections: 0,
            warnings: vec![],
            error: Some("v1 file is damaged".into()),
        }),
        ..Default::default()
    })));
    assert_eq!(app.messages.latest().unwrap().level, Level::Error);
    assert!(latest_text(&app).contains("v1 file is damaged"));
}

// ---------------------------------------------------------------------------
// 窗口(P-61)
// ---------------------------------------------------------------------------

/// P-61:窗口尺寸变化防抖 500ms:每次变化只发一个“过 500ms 再说”的命令,到点时只有最新的那次才保存。
#[test]
fn p61_window_size_save_is_debounced() {
    let mut app = ready_app(vec![]);
    let cmds = app.update(Msg::WindowChanged {
        width: 1300,
        height: 900,
        maximized: false,
    });
    let g1 = match platform_cmds(&cmds)[..] {
        [PlatformCmd::After { ms, msg }] => {
            assert_eq!(*ms, 500);
            match **msg {
                Msg::WindowSaveDue(g) => g,
                _ => panic!(),
            }
        }
        _ => panic!("{cmds:?}"),
    };
    assert!(backend_cmds(&cmds).is_empty(), "nothing is written yet");
    // 又变了:新的代号;第一次到点时代号过期,不保存
    let cmds = app.update(Msg::WindowChanged {
        width: 1400,
        height: 950,
        maximized: false,
    });
    let g2 = match platform_cmds(&cmds)[..] {
        [PlatformCmd::After { msg, .. }] => match **msg {
            Msg::WindowSaveDue(g) => g,
            _ => panic!(),
        },
        _ => panic!(),
    };
    assert_ne!(g1, g2);
    assert!(
        app.update(Msg::WindowSaveDue(g1)).is_empty(),
        "a stale timer must not save"
    );
    let cmds = app.update(Msg::WindowSaveDue(g2));
    assert!(matches!(
        backend_cmds(&cmds)[..],
        [BackendCmd::SaveWindowSize {
            width: 1400,
            height: 950,
            maximized: false
        }]
    ));
    // 尺寸没变就不触发
    assert!(
        app.update(Msg::WindowChanged {
            width: 1400,
            height: 950,
            maximized: false
        })
        .is_empty()
    );
    // 最大化也是一种变化,也会保存
    let cmds = app.update(Msg::WindowChanged {
        width: 1400,
        height: 950,
        maximized: true,
    });
    assert_eq!(platform_cmds(&cmds).len(), 1);
}

/// 窗口尺寸在协议同意之前也要能存(v1 用数据库时,首次启动同意协议之前调整的窗口大小存不下来)。
#[test]
fn window_size_is_saved_even_before_the_agreement_is_accepted() {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::default()));
    assert!(app.blocked());
    let cmds = app.update(Msg::WindowChanged {
        width: 1200,
        height: 850,
        maximized: false,
    });
    assert_eq!(platform_cmds(&cmds).len(), 1);
}

/// 启动时窗口尺寸来自保存的设置;默认值和最小尺寸是 P-61 规定的。
#[test]
fn p61_window_defaults_and_minimum() {
    let app = App::new();
    assert_eq!(
        (app.window.width, app.window.height, app.window.maximized),
        (1600, 903, false)
    );
    assert_eq!(MIN_WINDOW, (1024, 840));
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        settings: Settings {
            window_width: 1280,
            window_height: 800,
            is_maximized: true,
            ..Default::default()
        },
        ..Default::default()
    })));
    assert_eq!(
        (app.window.width, app.window.height, app.window.maximized),
        (1280, 800, true)
    );
}

// ---------------------------------------------------------------------------
// 错误翻译:每种 core 错误都有对应的翻译,名字等参数原样带进去
// ---------------------------------------------------------------------------

#[test]
fn every_core_error_kind_has_a_translation_and_carries_its_parameters() {
    use bytes::Bytes;
    use xredis_core::error::{ClipboardStage, Entity, SkipReason, Skipped};
    let d = |e: Error| crate::errors::describe("en", &e);
    let b = |s: &'static str| Bytes::from_static(s.as_bytes());

    let cases: Vec<(Error, Vec<&str>)> = vec![
        (Error::Config("c".into()), vec!["Configuration error", "c"]),
        (Error::KeyStore("k".into()), vec!["Key store error", "k"]),
        (Error::Rule("r".into()), vec!["r"]),
        (Error::NotFound("n".into()), vec!["Not found", "n"]),
        (Error::Import("i".into()), vec!["Import failed", "i"]),
        (
            Error::Connect("refused".into()),
            vec!["Could not connect", "refused"],
        ),
        (
            Error::Auth("WRONGPASS".into()),
            vec!["Authentication failed", "WRONGPASS"],
        ),
        (Error::Timeout("t".into()), vec!["timed out"]),
        (Error::Closed, vec!["closed"]),
        (Error::Redis("ERR x".into()), vec!["Redis error", "ERR x"]),
        (
            Error::AlreadyExists {
                entity: Entity::Key,
                name: b("k1"),
                container: None,
            },
            vec!["Key 'k1' already exists"],
        ),
        (
            Error::AlreadyExists {
                entity: Entity::HashField,
                name: b("f"),
                container: Some(b("h")),
            },
            vec!["Field 'f'", "hash 'h'"],
        ),
        (
            Error::AlreadyExists {
                entity: Entity::SetMember,
                name: b("m"),
                container: Some(b("s")),
            },
            vec!["Member 'm'", "set 's'"],
        ),
        (
            Error::AlreadyExists {
                entity: Entity::ZSetMember,
                name: b("m"),
                container: Some(b("z")),
            },
            vec!["Member 'm'", "sorted set 'z'"],
        ),
        (
            Error::Missing {
                entity: Entity::Key,
                name: b("k1"),
                container: None,
            },
            vec!["Key 'k1' does not exist"],
        ),
        (
            Error::Missing {
                entity: Entity::HashField,
                name: b("f"),
                container: Some(b("h")),
            },
            vec!["Field 'f' does not exist", "'h'"],
        ),
        (
            Error::Missing {
                entity: Entity::SetMember,
                name: b("m"),
                container: Some(b("s")),
            },
            vec!["'m' does not exist", "'s'"],
        ),
        (
            Error::Missing {
                entity: Entity::ZSetMember,
                name: b("m"),
                container: Some(b("z")),
            },
            vec!["'m' does not exist", "'z'"],
        ),
        (Error::TtlZero, vec!["TTL of 0", "-1"]),
        (
            Error::TtlOutOfRange {
                value: 5_000_000_000,
                max: 2_147_483_647,
            },
            vec!["5000000000", "2147483647"],
        ),
        (
            Error::ListIndexInvalid {
                index: -1,
                max: 2_147_483_647,
            },
            vec!["-1", "2147483647"],
        ),
        (
            Error::ListIndexOutOfRange { index: 9, len: 3 },
            vec!["9", "3 elements"],
        ),
        (Error::ScoreNaN, vec!["score must be a number"]),
        (
            Error::CopyTooLarge {
                total: 70 * 1024 * 1024,
                limit: 64 * 1024 * 1024,
                copied: 12,
                key: b("big"),
            },
            vec!["70.0 MiB", "64.0 MiB", "12", "'big'"],
        ),
        (
            Error::NothingCopied {
                skipped: vec![Skipped {
                    key: b("a"),
                    reason: SkipReason::Missing,
                }],
            },
            vec!["Nothing was copied", "1 key(s)"],
        ),
        (
            Error::ClipboardNotRedis,
            vec!["does not contain Redis data"],
        ),
        (
            Error::ClipboardCorrupt {
                stage: ClipboardStage::Decompress,
                detail: "bad".into(),
            },
            vec!["damaged", "decompress", "bad"],
        ),
        (
            Error::PasteTooLarge {
                limit: 256 * 1024 * 1024,
            },
            vec!["256.0 MiB"],
        ),
        (
            Error::RestoreFailed {
                key: b("k"),
                detail: "BUSYKEY".into(),
            },
            vec!["'k'", "BUSYKEY"],
        ),
        (
            Error::CommandSyntax(xredis_core::console::SplitError::UnbalancedQuotes),
            vec!["Invalid command syntax", "unbalanced quotes"],
        ),
        (
            Error::CommandRejected {
                command: "SUBSCRIBE".into(),
            },
            vec!["'SUBSCRIBE'", "Pub/Sub page"],
        ),
        (
            Error::BlockingTimeout {
                command: "BLPOP".into(),
                secs: 30,
            },
            vec!["'BLPOP'", "30s"],
        ),
    ];
    for (err, needles) in cases {
        let text = d(err);
        for n in needles {
            assert!(text.contains(n), "expected {n:?} in {text:?}");
        }
        assert!(!text.contains("%{"), "an unreplaced parameter in {text:?}");
        // 缺翻译时 tr 返回 key 本身(以 err. 开头)
        assert!(!text.starts_with("err."), "missing translation: {text:?}");
    }
}

/// 冲突的 key 太多时只列前 10 个,其余只说数量;二进制名字有损显示不 panic。
#[test]
fn paste_conflicts_list_is_capped_and_binary_names_do_not_panic() {
    use bytes::Bytes;
    let conflicts: Vec<Bytes> = (0..25).map(|i| Bytes::from(format!("key{i}"))).collect();
    let text = crate::errors::describe(
        "en",
        &Error::PasteConflicts {
            conflicts,
            total: 30,
        },
    );
    assert!(
        text.contains("25 of 30") && text.contains("key9") && !text.contains("key10"),
        "{text}"
    );
    assert!(text.contains("(+15)"), "{text}");
    let text = crate::errors::describe(
        "en",
        &Error::AlreadyExists {
            entity: xredis_core::error::Entity::Key,
            name: Bytes::from_static(&[0xff, b'x']),
            container: None,
        },
    );
    assert!(text.contains("\u{fffd}x"), "{text}");
}

// ---------------------------------------------------------------------------
// 语言切换到另一种可用语言(第 14 节)
// ---------------------------------------------------------------------------

/// 放一个语言包,在设置里切过去:保存成功后界面文字变成那种语言,并通知 GUI 切全局 locale;
/// 再切回英文。这条路径要用到全局语言包,所以和 i18n 的测试共用一把锁。
#[test]
fn switching_to_an_available_language_translates_the_ui_and_tells_the_gui() {
    let _g = crate::i18n::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("zh-CN.yml"),
        "_version: 2\nbtn.save:\n  zh-CN: \"保存\"\nmsg.settings_saved:\n  zh-CN: \"设置已保存。\"\n",
    )
    .unwrap();
    let report = crate::i18n::load_user_packs(tmp.path());
    assert!(report.errors.is_empty(), "{report:?}");

    let mut app = ready_app(vec![]);
    assert_eq!(app.t("btn.save"), "Save");
    app.update(Msg::OpenSettings);
    app.update(Msg::SettingsSetLanguage("zh-CN".into()));
    let cmds = app.update(Msg::SettingsSave);
    let token = match backend_cmds(&cmds)[..] {
        [BackendCmd::SaveSettings { settings, token }] => {
            assert_eq!(settings.language, "zh-CN");
            *token
        }
        _ => panic!("{cmds:?}"),
    };
    assert_eq!(
        app.lang, "en",
        "the language switches only after the save succeeded"
    );
    let cmds = app.update(Msg::SettingsSaved {
        token,
        result: Ok(()),
    });
    assert_eq!(app.lang, "zh-CN");
    assert!(matches!(platform_cmds(&cmds)[..], [PlatformCmd::SetLocale(l)] if l == "zh-CN"));
    assert_eq!(app.t("btn.save"), "保存");
    assert_eq!(
        latest_text(&app),
        "设置已保存。",
        "the confirmation is already in the new language"
    );
    assert_eq!(
        app.t("btn.cancel"),
        "Cancel",
        "keys missing from the pack fall back to English"
    );
    assert_eq!(app.settings.language, "zh-CN");

    // 启动时保存的是 zh-CN 且语言包还在:直接用它
    let mut app2 = App::new();
    let cmds = app2.update(Msg::StartupDone(Box::new(StartupData {
        settings: Settings {
            language: "zh-CN".into(),
            agreement: AGREEMENT_VERSION.into(),
            ..Default::default()
        },
        ..Default::default()
    })));
    assert_eq!(app2.lang, "zh-CN");
    assert!(matches!(platform_cmds(&cmds)[..], [PlatformCmd::SetLocale(l)] if l == "zh-CN"));

    crate::i18n::load_user_packs(std::path::Path::new("Z:/definitely/not/here"));
}
