//! R-06a:core 的警告结构化之后,app 按种类翻译;编辑已打开连接后的提示。

use xredis_core::config::{ConfigWarning, ConnId, ConnectionConfig, SaveOutcome, Settings};
use xredis_core::conn::info::{ConnectionInfo, KeyspaceInfo};
use xredis_core::v1_import::ImportWarning;

use crate::app::{AGREEMENT_VERSION, App};
use crate::errors::{describe_import_warning, describe_startup_warning};
use crate::messages::Level;
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

fn saved(id: ConnId) -> Result<Saved, xredis_core::Error> {
    Ok(Saved {
        outcome: SaveOutcome {
            id,
            password_error: None,
        },
        keystore_degraded: false,
    })
}

/// 配置文件损坏:带路径、原因、备份路径;备份也失败了是另一句话。
#[test]
fn a_corrupt_config_warning_is_translated_with_its_data() {
    let w = StartupWarning::Config(ConfigWarning::CorruptConfig {
        path: "C:/data/config.json".into(),
        reason: "expected value at line 1 column 1".into(),
        backup: Some("C:/data/config.json.broken".into()),
    });
    let text = describe_startup_warning("en", &w);
    for needle in [
        "C:/data/config.json",
        "expected value at line 1 column 1",
        "saved as C:/data/config.json.broken",
        "default settings",
    ] {
        assert!(text.contains(needle), "{needle:?} missing in {text:?}");
    }
    assert!(!text.contains("%{"), "{text}");

    let w = StartupWarning::Config(ConfigWarning::CorruptConfig {
        path: "C:/data/config.json".into(),
        reason: "eof".into(),
        backup: None,
    });
    let text = describe_startup_warning("en", &w);
    assert!(
        text.contains("backing it up failed") && !text.contains("saved as"),
        "{text}"
    );
}

#[test]
fn an_unusable_config_dir_warning_names_the_fallback_and_the_reason() {
    let w = StartupWarning::ConfigDirUnusable {
        reason: "Failed to create config directory C:/x/v2: Access is denied".into(),
        fallback: "C:/Temp/xRedis-v2-01ABC".into(),
    };
    let text = describe_startup_warning("en", &w);
    assert!(text.contains("Access is denied"), "{text}");
    assert!(text.contains("C:/Temp/xRedis-v2-01ABC"), "{text}");
    assert!(text.contains("will not be kept"), "{text}");
}

/// 四种导入提示都有翻译,标签(连接名)和端口、原因原样带进去。
#[test]
fn every_import_warning_kind_is_translated() {
    let cases = [
        (
            ImportWarning::InvalidId {
                label: "prod".into(),
            },
            vec!["prod", "id is not valid"],
        ),
        (
            ImportWarning::InvalidPort {
                label: "cache".into(),
                port: 70000,
            },
            vec!["cache", "70000"],
        ),
        (
            ImportWarning::PasswordUndecryptable {
                label: "会话库".into(),
            },
            vec!["会话库", "could not be decrypted", "enter it again"],
        ),
        (
            ImportWarning::PasswordNotStored {
                label: "db1".into(),
                reason: "locked".into(),
            },
            vec!["db1", "key store", "locked"],
        ),
    ];
    for (w, needles) in cases {
        let text = describe_import_warning("en", &w);
        for n in needles {
            assert!(text.contains(n), "{n:?} missing in {text:?}");
        }
        assert!(
            !text.contains("%{") && !text.starts_with("import."),
            "{text}"
        );
    }
}

/// 启动结果里的警告按种类进消息栏(Warn,带 toast),不再是 core 的英文句子原样显示。
#[test]
fn startup_warnings_reach_the_message_bar_translated() {
    let mut app = App::new();
    app.update(Msg::StartupDone(Box::new(StartupData {
        warnings: vec![StartupWarning::ConfigDirUnusable {
            reason: "read-only".into(),
            fallback: "C:/Temp/x".into(),
        }],
        ..Default::default()
    })));
    let m = app.messages.latest().unwrap();
    assert_eq!(m.level, Level::Warn);
    assert!(
        m.text.starts_with("The configuration folder is not usable"),
        "{}",
        m.text
    );
    assert_eq!(app.messages.toasts().len(), 1);
}

fn open_ready(app: &mut App, id: ConnId) {
    let epoch = app
        .update(Msg::OpenConnection(id))
        .iter()
        .find_map(|c| match c {
            Cmd::Backend(BackendCmd::Connect { epoch, .. }) => Some(*epoch),
            _ => None,
        })
        .expect("connect");
    app.update(Msg::Connected {
        id,
        epoch,
        result: Ok(ConnectionInfo {
            databases: (0..16).map(KeyspaceInfo::empty).collect(),
        }),
    });
}

fn reopen_notes(app: &App) -> usize {
    app.messages
        .entries()
        .filter(|e| e.text.contains("reopen its tab"))
        .count()
}

/// 编辑一个已经打开了 Tab 的连接并保存:Tab 不会自动重连(和 v1 一致),
/// 但要提示“重新打开 Tab 后生效”,不然用户以为改动已经生效。
#[test]
fn saving_changes_to_a_connection_with_an_open_tab_says_to_reopen_it() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    open_ready(&mut app, id);
    app.update(Msg::OpenEditor(Some(id)));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(saved(id)));
    assert_eq!(reopen_notes(&app), 1);
    let m = app
        .messages
        .entries()
        .find(|e| e.text.contains("reopen its tab"))
        .unwrap();
    assert_eq!(m.level, Level::Info);
    assert!(
        app.tab(id).is_some(),
        "the tab is left alone: no automatic reconnect"
    );
}

/// 没有打开的 Tab 时不提示(下次打开就是新配置);新建的连接不可能有 Tab,也不提示。
#[test]
fn no_reopen_hint_without_an_open_tab_or_for_a_new_connection() {
    let c = cfg("a");
    let id = c.id;
    let mut app = ready_app(vec![c]);
    app.update(Msg::OpenEditor(Some(id)));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(saved(id)));
    assert_eq!(reopen_notes(&app), 0, "no tab open");

    app.update(Msg::OpenEditor(None));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(saved(ConnId::new())));
    assert_eq!(reopen_notes(&app), 0, "a brand new connection has no tab");
}

/// 只给打开的那一个连接提示:别的连接的 Tab 开着不相干。
#[test]
fn the_reopen_hint_is_only_for_the_edited_connection() {
    let (a, b) = (cfg("a"), cfg("b"));
    let (ia, ib) = (a.id, b.id);
    let mut app = ready_app(vec![a, b]);
    open_ready(&mut app, ib);
    app.update(Msg::OpenEditor(Some(ia)));
    app.update(Msg::EditorSave);
    app.update(Msg::ConnectionSaved(saved(ia)));
    assert_eq!(
        reopen_notes(&app),
        0,
        "only b's tab is open, and b was not edited"
    );
}
