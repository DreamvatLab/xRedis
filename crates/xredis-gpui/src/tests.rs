//! 主窗口的无头测试:假后端驱动整个 `MainView`(窗口、侧栏、Tab、消息栏、菜单)。
//! 测试模块里不能 `use super::*`(会带进 `gpui_kit::test` 宏,递归溢出),都显式导入。
//! 只测得了状态、焦点、元素有没有、点了之后 `App` 的变化;颜色和布局好不好看只能试用看。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, px, size};
use xredis_app::app::WINDOW_SAVE_DEBOUNCE_MS;
use xredis_app::messages::Level;
use xredis_app::msg::{HelpLink, Msg};
use xredis_app::testing::FakeBackend;
use xredis_core::config::ConnId;

use crate::MainView;

struct Env {
    window: AnyWindowHandle,
    view: Entity<MainView>,
    fake: Arc<FakeBackend>,
}

mod console;
mod dialogs;
mod keys;
mod keyview;
mod members;
mod pubsub;

/// 开一个窗口,启动并同意协议,等到启动完成。
fn setup(cx: &mut TestAppContext, names: &[&str]) -> (Env, Vec<ConnId>) {
    let (env, ids) = setup_unagreed(cx, names);
    dispatch(cx, &env, Msg::AgreeDisclaimer);
    wait(cx, &env, "agreement accepted", |app| !app.blocked());
    (env, ids)
}

/// 开一个窗口,等到启动完成;协议还没同意(首次启动)。
fn setup_unagreed(cx: &mut TestAppContext, names: &[&str]) -> (Env, Vec<ConnId>) {
    cx.update(gpui_kit::init);
    let fake = FakeBackend::new();
    let ids: Vec<ConnId> = names
        .iter()
        .enumerate()
        .map(|(i, n)| fake.add_connection(n, i as i32))
        .collect();
    let backend = fake.clone();
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                    origin: Default::default(),
                    size: size(px(1100.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                cx.new(|cx| {
                    MainView::with_runtime(backend, xredis_app::Runtime::inline, window, cx)
                })
            },
        )
        .expect("open window")
    });
    let env = Env { window, view, fake };
    wait(cx, &env, "startup", |app| app.started);
    (env, ids)
}

fn dispatch(cx: &mut TestAppContext, env: &Env, msg: Msg) {
    cx.update_window(env.window, |_, window, cx| {
        env.view
            .update(cx, |this, cx| this.dispatch(msg, window, cx));
    })
    .unwrap();
}

/// 等 `App` 满足条件(后端结果从别的线程回来,要等)。超时就失败,不静默挂住。
fn wait(cx: &mut TestAppContext, env: &Env, what: &str, ok: impl Fn(&xredis_app::App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| ok(&env.view.read(cx).app)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn read<R>(cx: &mut TestAppContext, env: &Env, f: impl FnOnce(&xredis_app::App) -> R) -> R {
    cx.read(|cx| f(&env.view.read(cx).app))
}

fn click(cx: &mut TestAppContext, env: &Env, id: &str) {
    let id = gpui_kit::SharedString::from(id.to_string());
    cx.update_window(env.window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn exists(cx: &mut TestAppContext, env: &Env, id: &str) -> bool {
    let id = gpui_kit::SharedString::from(id.to_string());
    cx.update_window(env.window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id.clone()).is_some()
    })
    .unwrap()
}

fn is_ready(app: &xredis_app::App) -> bool {
    app.active_tab().is_some_and(|t| app.can_request(t.id))
}

fn is_failed(app: &xredis_app::App) -> bool {
    matches!(
        app.active_tab().map(|t| &t.status),
        Some(xredis_app::TabStatus::Failed(_))
    )
}

#[gpui_kit::test]
fn the_sidebar_lists_connections_and_a_click_opens_a_tab(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["缓存库", "会话库"]);
    for id in &ids {
        assert!(exists(cx, &env, &format!("conn-{id}")), "a row for {id}");
    }
    assert!(!exists(cx, &env, &format!("tab-close-{}", ids[0])));

    click(cx, &env, &format!("conn-{}", ids[0]));
    wait(cx, &env, "tab ready", is_ready);
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 1);
    assert!(
        exists(cx, &env, &format!("tab-close-{}", ids[0])),
        "the tab has a close button"
    );
    for page in ["subpage-keys", "subpage-pubsub", "subpage-console"] {
        assert!(exists(cx, &env, page), "{page}");
    }

    // 同一个连接再点只是激活,不开第二个 Tab
    click(cx, &env, &format!("conn-{}", ids[0]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 1);
}

#[gpui_kit::test]
fn closing_a_tab_removes_it_at_once(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    click(cx, &env, &format!("conn-{}", ids[0]));
    assert_eq!(
        read(cx, &env, |a| a.tabs.len()),
        1,
        "the tab is there immediately, even while connecting"
    );
    click(cx, &env, &format!("tab-close-{}", ids[0]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 0);
    assert!(!exists(cx, &env, &format!("tab-close-{}", ids[0])));
}

#[gpui_kit::test]
fn a_failed_connection_shows_the_reason_and_retry_recovers(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    click(cx, &env, &format!("conn-{}", ids[0]));
    wait(cx, &env, "failed", is_failed);
    assert!(exists(cx, &env, "tab-failed"));
    // 错误框(R-08b)是模态的,盖住了重试按钮:先关掉它
    click(cx, &env, "error-ok");
    assert!(exists(cx, &env, &format!("retry-{}", ids[0])));
    click(cx, &env, &format!("retry-{}", ids[0]));
    wait(cx, &env, "ready after retry", is_ready);
    assert!(!exists(cx, &env, "tab-failed"));
}

#[gpui_kit::test]
fn the_sidebar_toggle_collapses_and_persists_the_setting(cx: &mut TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    assert!(!read(cx, &env, |a| a.settings.sider_menu_collapsed));
    click(cx, &env, "toggle-sider");
    wait(cx, &env, "collapsed", |a| a.settings.sider_menu_collapsed);
    // 持久化:配置文件里也变了
    wait(cx, &env, "saved", |_| {
        env.fake.store().settings().sider_menu_collapsed
    });
    click(cx, &env, "toggle-sider");
    wait(cx, &env, "expanded", |a| !a.settings.sider_menu_collapsed);
}

#[gpui_kit::test]
fn the_add_button_opens_the_connection_editor_state(cx: &mut TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    click(cx, &env, "add-connection");
    assert!(
        read(cx, &env, |a| a.editor.is_some()),
        "App has the editor open (the dialog itself is R-08b)"
    );
}

#[gpui_kit::test]
fn the_message_bar_shows_the_latest_and_opens_the_log(cx: &mut TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    dispatch(cx, &env, Msg::ClearMessages);
    assert!(!exists(cx, &env, "message-log"));
    cx.update_window(env.window, |_, _, cx| {
        env.view.update(cx, |this, cx| {
            this.app.messages.push(Level::Warn, "第一条", false, 1);
            this.app.messages.push(Level::Error, "第二条", false, 2);
            cx.notify();
        });
    })
    .unwrap();
    click(cx, &env, "message-latest");
    assert!(read(cx, &env, |a| a.messages.open));
    assert!(exists(cx, &env, "message-log"));
    // 两条都在日志里
    let ids: Vec<u64> = read(cx, &env, |a| a.messages.entries().map(|e| e.id).collect());
    assert_eq!(ids.len(), 2);
    for id in ids {
        assert!(exists(cx, &env, &format!("message-{id}")));
    }
    click(cx, &env, "msgbar-clear");
    assert_eq!(read(cx, &env, |a| a.messages.len()), 0);
}

/// 菜单按钮在;Help 的各项、协议和隐私弹窗的状态由 `App` 管(弹窗本身在 R-08b)。
#[gpui_kit::test]
fn the_top_menu_buttons_exist_and_help_state_changes_reach_the_app(cx: &mut TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    assert!(exists(cx, &env, "menu-file") && exists(cx, &env, "menu-help"));
    dispatch(cx, &env, Msg::OpenHelpLink(HelpLink::ReportBugs));
    dispatch(cx, &env, Msg::ShowAgreement(true));
    assert!(read(cx, &env, |a| a.show_agreement));
    dispatch(cx, &env, Msg::ShowPrivacy(true));
    assert!(read(cx, &env, |a| a.show_privacy));
}

/// 连接失败走错误框加消息栏,不弹 toast(`App` 的设计);删除失败是弹 toast 的一条路径。
#[gpui_kit::test]
fn an_error_toast_is_pushed_once_and_dismissal_goes_back_to_the_app(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    assert_eq!(read(cx, &env, |a| a.messages.toasts().len()), 0);
    env.fake.fail_next(
        "delete_connection",
        xredis_core::Error::Connect("disk full".into()),
    );
    dispatch(cx, &env, Msg::DeleteRequested(ids[0]));
    dispatch(cx, &env, Msg::ConfirmAccepted);
    wait(cx, &env, "toast", |a| !a.messages.toasts().is_empty());
    let toasts = read(cx, &env, |a| a.messages.toasts().len());
    assert_eq!(toasts, 1);
    assert!(read(cx, &env, |a| a
        .messages
        .entries()
        .any(|e| e.level == Level::Error)));
    let shown = cx.read(|cx| env.view.read(cx).shown_toast_count());
    assert_eq!(shown, toasts, "every live toast was pushed exactly once");
    let id = read(cx, &env, |a| a.messages.toasts().first().map(|t| t.id)).unwrap();
    dispatch(cx, &env, Msg::DismissToast(id));
    assert!(read(cx, &env, |a| a.messages.toasts().is_empty()));
    assert_eq!(cx.read(|cx| env.view.read(cx).shown_toast_count()), 0);
}

/// 窗口大小变化通知 App(`observe_window_bounds` 在无头窗口里会触发),App 防抖后让后端保存(P-61)。
#[gpui_kit::test]
fn resizing_the_window_is_saved_after_the_debounce(cx: &mut TestAppContext) {
    let (env, _) = setup(cx, &["a"]);
    let vcx = gpui_kit::VisualTestContext::from_window(env.window, cx);
    vcx.simulate_resize(size(px(1300.), px(900.)));
    vcx.run_until_parked();
    // 没有手动 dispatch:观察者自己把新尺寸送进了 App
    assert_eq!(
        read(cx, &env, |a| (a.window.width, a.window.height)),
        (1300, 900)
    );
    // 防抖时间没到之前不保存
    assert_ne!(env.fake.store().settings().window_width, 1300);
    cx.executor()
        .advance_clock(Duration::from_millis(WINDOW_SAVE_DEBOUNCE_MS + 50));
    wait(cx, &env, "window size saved", |_| {
        env.fake.store().settings().window_width == 1300
    });
    assert_eq!(env.fake.store().settings().window_height, 900);
}

/// 最大化时记的是还原后的尺寸加最大化标记;全屏不当成最大化。
#[test]
fn a_maximized_window_reports_its_restore_size_and_the_flag() {
    use gpui_kit::{Bounds, WindowBounds};
    let b = Bounds {
        origin: Default::default(),
        size: size(px(1000.4), px(700.6)),
    };
    assert_eq!(
        crate::model::state_of(WindowBounds::Windowed(b)),
        (1000, 701, false)
    );
    assert_eq!(
        crate::model::state_of(WindowBounds::Maximized(b)),
        (1000, 701, true)
    );
    assert_eq!(
        crate::model::state_of(WindowBounds::Fullscreen(b)),
        (1000, 701, false)
    );
}

/// 窗口上一帧画出来的文字。
fn painted_texts(cx: &mut TestAppContext, env: &Env) -> Vec<String> {
    cx.update_window(env.window, |_, window, cx| {
        window.render_frame(cx);
        gpui_kit::base::test_support::texts(window)
            .iter()
            .map(|t| t.text().to_string())
            .collect()
    })
    .unwrap()
}

#[gpui_kit::test]
fn r12a_an_unnamed_connection_shows_host_and_port_in_sidebar_and_tab(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["", "缓存库"]);
    let texts = painted_texts(cx, &env);
    let count = |s: &str| texts.iter().filter(|t| t.as_str() == s).count();
    assert_eq!(
        count("127.0.0.1:6379"),
        2,
        "unnamed: host:port once as the name; named: once as the subtitle. got {texts:?}"
    );
    assert_eq!(count("缓存库"), 1, "{texts:?}");

    click(cx, &env, &format!("conn-{}", ids[0]));
    wait(cx, &env, "tab ready", is_ready);
    let label = cx
        .update_window(env.window, |_, window, cx| {
            window.render_frame(cx);
            window.find(0usize).label().map(str::to_string)
        })
        .unwrap();
    assert_eq!(label.as_deref(), Some("127.0.0.1:6379"), "the tab title");
}

#[gpui_kit::test]
fn r12a_clicking_a_tab_activates_it_and_only_the_close_button_closes(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["", "缓存库"]);
    for id in &ids {
        click(cx, &env, &format!("conn-{id}"));
        wait(cx, &env, "tab ready", is_ready);
    }
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 2);
    assert_eq!(read(cx, &env, |a| a.active), Some(ids[1]));

    // Tab 的元素 id 是它在 TabBar 里的序号;点标题区(不是 ×)切换,不关
    click_index(cx, &env, 0);
    assert_eq!(read(cx, &env, |a| a.active), Some(ids[0]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 2);
    click_index(cx, &env, 1);
    assert_eq!(read(cx, &env, |a| a.active), Some(ids[1]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 2);

    click(cx, &env, &format!("tab-close-{}", ids[1]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 1);
    assert_eq!(read(cx, &env, |a| a.active), Some(ids[0]));
}

fn click_index(cx: &mut TestAppContext, env: &Env, ix: usize) {
    cx.update_window(env.window, |_, window, cx| {
        window.render_frame(cx);
        // 点标题区的左侧(离右边的 × 远);无头平台没有真字体,按中心点会落在 ×(宽度由字体决定)上
        window.click_at(ix, gpui_kit::point(px(8.), px(18.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn r12b_the_embedded_font_is_registered_and_the_theme_uses_it(cx: &mut TestAppContext) {
    use gpui_kit::component::Theme;
    cx.update(gpui_kit::init);
    // 无头(测试)平台的文本系统不加载字体,`all_font_names()` 恒为空,所以这里只能断言
    // 注册调用不报错;字体文件本身有没有 JetBrains Mono 由下一个测试查。
    cx.update(|cx| crate::fonts::install(cx).expect("register the embedded fonts"));
    cx.update(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.font_family.as_ref(), "JetBrains Mono");
        // 编辑器(EditorState)读的是 mono_font_family
        assert_eq!(theme.mono_font_family.as_ref(), "JetBrains Mono");
    });
}

#[test]
fn r12b_the_embedded_font_files_are_real_ttf_files() {
    assert_eq!(crate::fonts::FONT_FILES.len(), 4);
    for bytes in crate::fonts::FONT_FILES {
        assert!(bytes.len() > 100_000);
        // TrueType 的文件头(0x00010000)
        assert_eq!(&bytes[..4], &[0, 1, 0, 0]);
        // name 表里的字体族名,和主题里设的一致
        // (Windows 平台的记录是 UTF-16BE)
        let family: Vec<u8> = crate::fonts::FAMILY
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert!(bytes.windows(family.len()).any(|w| w == family.as_slice()));
    }
}

/// R-12c:侧栏每行有看得见的 ⋮ 按钮;点它不会同时打开连接;折叠时不显示。
#[gpui_kit::test]
fn r12c_the_connection_menu_button_does_not_open_the_connection(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a", "b"]);
    for id in &ids {
        assert!(
            exists(cx, &env, &format!("conn-menu-{id}")),
            "button for {id}"
        );
    }
    click(cx, &env, &format!("conn-menu-{}", ids[0]));
    assert_eq!(
        read(cx, &env, |a| a.tabs.len()),
        0,
        "the menu button must not open the connection"
    );
    assert!(read(cx, &env, |a| a.editor.is_none()));
    // 对照:点行本身还是打开
    click(cx, &env, &format!("conn-{}", ids[1]));
    assert_eq!(read(cx, &env, |a| a.tabs.len()), 1);

    click(cx, &env, "toggle-sider");
    wait(cx, &env, "collapsed", |a| a.settings.sider_menu_collapsed);
    assert!(!exists(cx, &env, &format!("conn-menu-{}", ids[0])));
}

/// R-12c:连接失败页有"打开消息日志"按钮。
#[gpui_kit::test]
fn r12c_the_failed_page_can_open_the_message_log(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    click(cx, &env, &format!("conn-{}", ids[0]));
    wait(cx, &env, "failed", is_failed);
    click(cx, &env, "error-ok");
    assert!(!read(cx, &env, |a| a.messages.open));
    click(cx, &env, "tab-open-log");
    assert!(read(cx, &env, |a| a.messages.open));
    assert!(exists(cx, &env, "message-log"));
}

/// R-12c:消息日志每条前面有 `HH:MM:SS` 的本地时间(格式规则在 App,这里只看它被画出来)。
#[gpui_kit::test]
fn r12c_message_log_entries_show_a_clock_time(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    env.fake
        .fail_next("connect", xredis_core::Error::Connect("refused".into()));
    click(cx, &env, &format!("conn-{}", ids[0]));
    wait(cx, &env, "failed", is_failed);
    click(cx, &env, "error-ok");
    click(cx, &env, "message-latest");
    let texts = painted_texts(cx, &env);
    let is_clock = |t: &String| {
        t.len() == 8
            && t.char_indices().all(|(i, c)| {
                if i == 2 || i == 5 {
                    c == ':'
                } else {
                    c.is_ascii_digit()
                }
            })
    };
    assert!(texts.iter().any(is_clock), "no HH:MM:SS in {texts:?}");
}
