//! Pub/Sub 子页的无头测试(R-11b):订阅、收到消息、退订、发布、Clear、二进制消息、按钮可用状态。

use gpui_kit::TestAppContext;
use xredis_app::Msg;
use xredis_app::pubsub_view::{PubSubMsg, Subscription};
use xredis_core::config::ConnId;

use super::dialogs::type_into;
use super::keyview::open_tab;
use super::{Env, click, dispatch, exists, read, setup, wait};

fn open_pubsub(cx: &mut TestAppContext, env: &Env, id: ConnId) {
    open_tab(cx, env, id);
    click(cx, env, "subpage-pubsub");
    assert!(exists(cx, env, "pubsub-page"));
}

fn texts(cx: &mut TestAppContext, env: &Env, id: ConnId) -> Vec<String> {
    read(cx, env, |a| {
        a.tab(id)
            .unwrap()
            .pubsub
            .lines()
            .map(|l| l.text.clone())
            .collect()
    })
}

/// 消息记录控件里的文字和滚动位置(纵向,0 是顶,向下滚是负数)。
fn widget(cx: &mut TestAppContext, env: &Env, id: ConnId) -> (String, f32) {
    cx.read(|cx| env.view.read(cx).pubsub_output_state(id, cx).unwrap())
}

fn state(cx: &mut TestAppContext, env: &Env, id: ConnId) -> Subscription {
    read(cx, env, |a| {
        a.tab(id).unwrap().pubsub.subscription().clone()
    })
}

/// 点 Subscribe,等到订阅成功。
fn subscribe(cx: &mut TestAppContext, env: &Env, id: ConnId, channel: &str) {
    type_into(cx, env, "pubsub-channel", channel);
    click(cx, env, "pubsub-subscribe");
    wait(cx, env, "subscribed", |a| {
        matches!(
            a.tab(id).unwrap().pubsub.subscription(),
            Subscription::Subscribed(_)
        )
    });
}

#[gpui_kit::test]
fn the_page_has_its_controls_and_the_buttons_follow_the_app(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    for c in [
        "pubsub-channel",
        "pubsub-message",
        "pubsub-subscribe",
        "pubsub-unsubscribe",
        "pubsub-publish",
        "pubsub-channels",
        "pubsub-clear",
        "pubsub-list",
    ] {
        assert!(exists(cx, &env, c), "{c}");
    }
    // 没填频道:Subscribe、Publish、Unsubscribe、Clear 都不可用,点了没反应
    for b in [
        "pubsub-subscribe",
        "pubsub-unsubscribe",
        "pubsub-publish",
        "pubsub-clear",
    ] {
        click(cx, &env, b);
    }
    assert_eq!(state(cx, &env, id), Subscription::None);
    assert!(
        env.fake
            .calls()
            .iter()
            .all(|c| !c.starts_with("subscribe") && !c.starts_with("publish"))
    );
    type_into(cx, &env, "pubsub-channel", "news");
    assert!(read(cx, &env, |a| a
        .tab(id)
        .unwrap()
        .pubsub
        .can_subscribe()));
    assert!(
        !read(cx, &env, |a| a.tab(id).unwrap().pubsub.can_publish()),
        "no message yet"
    );
}

#[gpui_kit::test]
fn subscribe_receive_publish_and_unsubscribe(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    subscribe(cx, &env, id, "news");
    assert_eq!(
        env.fake.subscribed_channel(id).as_deref(),
        Some(&b"news"[..])
    );
    assert!(texts(cx, &env, id)[0].contains("Subscribed to channel <news>"));
    assert!(
        widget(cx, &env, id)
            .0
            .contains("Subscribed to channel <news>")
    );
    // 订阅着:Subscribe 不可用,Unsubscribe 可用
    assert!(!read(cx, &env, |a| a
        .tab(id)
        .unwrap()
        .pubsub
        .can_subscribe()));
    assert!(read(cx, &env, |a| a
        .tab(id)
        .unwrap()
        .pubsub
        .can_unsubscribe()));

    // 别的客户端发来一条
    env.fake.emit(id, b"news", b"hello");
    wait(cx, &env, "message", |a| {
        a.tab(id).unwrap().pubsub.line_count() == 2
    });
    assert!(texts(cx, &env, id)[1].contains("hello"));
    assert!(widget(cx, &env, id).0.contains("received: \"hello\""));

    // 自己发布(回环),多行消息原样发
    type_into(cx, &env, "pubsub-message", "line one\nline two");
    assert_eq!(
        cx.read(|cx| env.view.read(cx).pubsub_input_texts(id, cx).unwrap()),
        ("news".to_string(), "line one\nline two".to_string())
    );
    click(cx, &env, "pubsub-publish");
    wait(cx, &env, "echo", |a| {
        a.tab(id).unwrap().pubsub.line_count() >= 4
    });
    let t = texts(cx, &env, id);
    assert!(
        t.iter()
            .any(|l| l.contains("Published to channel <news>, 1 receiver(s)")),
        "{t:?}"
    );
    assert!(t.iter().any(|l| l.contains("line one\nline two")), "{t:?}");

    // 退订
    click(cx, &env, "pubsub-unsubscribe");
    wait(cx, &env, "unsubscribed", |a| {
        a.tab(id).unwrap().pubsub.subscription() == &Subscription::None
    });
    assert_eq!(env.fake.subscribed_channel(id), None);
    assert!(
        texts(cx, &env, id)
            .last()
            .unwrap()
            .contains("Unsubscribed from channel <news>")
    );
    // 退订后别人发来的不再显示
    let n = texts(cx, &env, id).len();
    env.fake.emit(id, b"news", b"late");
    cx.run_until_parked();
    assert_eq!(texts(cx, &env, id).len(), n);
}

#[gpui_kit::test]
fn changing_the_channel_box_after_subscribing_still_unsubscribes_the_right_one(
    cx: &mut TestAppContext,
) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    subscribe(cx, &env, id, "news");
    type_into(cx, &env, "pubsub-channel", "other");
    click(cx, &env, "pubsub-unsubscribe");
    wait(cx, &env, "unsubscribed", |a| {
        a.tab(id).unwrap().pubsub.subscription() == &Subscription::None
    });
    assert!(texts(cx, &env, id).last().unwrap().contains("<news>"));
    assert!(
        env.fake
            .calls()
            .iter()
            .any(|c| c.starts_with("unsubscribe") && c.ends_with(":news"))
    );
}

#[gpui_kit::test]
fn a_binary_message_shows_lossily_and_clear_empties_the_list(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    subscribe(cx, &env, id, "bin");
    env.fake.emit(id, b"bin", &[0xff, 0xfe, b'o', b'k']);
    wait(cx, &env, "message", |a| {
        a.tab(id).unwrap().pubsub.line_count() == 2
    });
    let line = texts(cx, &env, id)[1].clone();
    assert!(line.contains("<bin>") && line.contains("ok"), "{line}");
    let shown = widget(cx, &env, id).0;
    assert!(shown.contains("<bin>") && shown.contains("ok"), "{shown}");
    click(cx, &env, "pubsub-clear");
    assert_eq!(
        read(cx, &env, |a| a.tab(id).unwrap().pubsub.line_count()),
        0
    );
    assert_eq!(widget(cx, &env, id).0, "");
}

#[gpui_kit::test]
fn the_channels_button_lists_channels(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    env.fake
        .set_channel_list(vec!["alpha".into(), "beta".into()]);
    open_pubsub(cx, &env, id);
    click(cx, &env, "pubsub-channels");
    wait(cx, &env, "channels", |a| {
        a.tab(id).unwrap().pubsub.line_count() == 3
    });
    let t = texts(cx, &env, id);
    assert_eq!(&t[1..], ["alpha", "beta"]);
}

/// 列表很长时只画可见的行(虚拟滚动),并跟到最新一行。
#[gpui_kit::test]
fn a_long_list_draws_only_the_visible_rows_and_follows_the_newest(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    subscribe(cx, &env, id, "c");
    for i in 0..400 {
        env.fake.emit(id, b"c", format!("m{i}").as_bytes());
    }
    wait(cx, &env, "all messages", |a| {
        a.tab(id).unwrap().pubsub.line_count() == 401
    });
    assert!(exists(cx, &env, "pubsub-list"), "renders");
    let (text, y) = widget(cx, &env, id);
    assert_eq!(text.lines().count(), 401);
    assert!(
        text.ends_with("received: \"m399\""),
        "the newest line is in the widget"
    );
    assert!(y < -100.0, "scrolled down to the end, offset {y}");
}

/// R-12c:长消息和多行消息能看全(只读编辑控件里是全文,能选中复制),时间在第一行,
/// 后续行缩进到文字列。
#[gpui_kit::test]
fn r12c_a_multiline_and_a_long_message_show_in_full(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    subscribe(cx, &env, id, "c");
    let long = "x".repeat(2000);
    env.fake.emit(
        id,
        b"c",
        format!(
            "first
second {long}"
        )
        .as_bytes(),
    );
    wait(cx, &env, "message", |a| {
        a.tab(id).unwrap().pubsub.line_count() == 2
    });
    let (text, _) = widget(cx, &env, id);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[1].ends_with("received: \"first"), "{}", lines[1]);
    assert!(lines[2].starts_with(&" ".repeat(14)));
    assert!(
        lines[2].contains(&long),
        "the whole message is there, not cut"
    );
}

#[gpui_kit::test]
fn publishing_without_a_message_is_refused_by_the_app(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_pubsub(cx, &env, id);
    type_into(cx, &env, "pubsub-channel", "news");
    // 按钮不可用;直接发消息给 App,它也拒绝并留一行说明
    click(cx, &env, "pubsub-publish");
    assert!(env.fake.calls().iter().all(|c| !c.starts_with("publish")));
    dispatch(
        cx,
        &env,
        Msg::PubSub {
            id,
            msg: PubSubMsg::Publish,
        },
    );
    assert_eq!(texts(cx, &env, id), vec!["Message is required."]);
}
