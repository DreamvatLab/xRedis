//! Pub/Sub 视图模型的测试(不经过 App)。移植 v1 `pubsubVM` 的规则,修掉它的两个毛病:
//! 退订用记下来的频道(不是输入框现在的内容);二进制消息有损显示(v1 显示成空串)。

use xredis_core::Error;
use xredis_core::config::{ConnId, Settings};

use super::*;

struct H {
    v: PubSubView,
    settings: Settings,
    id: ConnId,
    now: i64,
}

impl H {
    fn new() -> Self {
        Self {
            v: PubSubView::new(),
            settings: Settings::default(),
            id: ConnId::new(),
            now: 5_000,
        }
    }

    fn send(&mut self, msg: PubSubMsg) -> Out {
        let ctx = Ctx {
            id: self.id,
            lang: "en",
            settings: &self.settings,
            now: self.now,
        };
        self.v.update(&ctx, msg)
    }

    fn texts(&self) -> Vec<String> {
        self.v.lines().map(|l| l.text.clone()).collect()
    }

    fn last(&self) -> String {
        self.v.lines().last().expect("a line").text.clone()
    }

    /// 订阅一个频道并让结果回来。
    fn subscribe(&mut self, channel: &str) {
        self.send(PubSubMsg::SetChannel(channel.into()));
        self.send(PubSubMsg::Subscribe);
        self.send(PubSubMsg::Subscribed {
            generation: 0,
            result: Ok(()),
        });
    }
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn cmds(out: &Out) -> Vec<&BackendCmd> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Backend(b) => Some(b),
            _ => None,
        })
        .collect()
}

fn incoming(conn: ConnId, channel: &[u8], payload: &[u8], at_ms: i64) -> PubSubMsg {
    PubSubMsg::Incoming(PubsubMessage {
        conn,
        channel: Bytes::copy_from_slice(channel),
        payload: Bytes::copy_from_slice(payload),
        at_ms,
    })
}

#[test]
fn subscribing_needs_a_channel_name() {
    let mut h = H::new();
    assert!(!h.v.can_subscribe());
    let out = h.send(PubSubMsg::Subscribe);
    assert!(out.cmds.is_empty());
    assert_eq!(h.last(), "Channel is required.");
    h.send(PubSubMsg::SetChannel("   ".into()));
    assert!(
        h.v.channel_is_empty() && !h.v.can_subscribe(),
        "whitespace is empty"
    );
}

#[test]
fn subscribing_goes_through_subscribing_to_subscribed() {
    let mut h = H::new();
    h.send(PubSubMsg::SetChannel("  新闻.频道  ".into()));
    assert!(h.v.can_subscribe());
    let out = h.send(PubSubMsg::Subscribe);
    assert!(matches!(
        cmds(&out)[..],
        [BackendCmd::PubSubSubscribe { generation: 0, channel, .. }] if *channel == b("新闻.频道")
    ));
    assert_eq!(
        *h.v.subscription(),
        Subscription::Subscribing(b("新闻.频道"))
    );
    assert!(!h.v.can_subscribe() && !h.v.can_unsubscribe());
    h.send(PubSubMsg::Subscribed {
        generation: 0,
        result: Ok(()),
    });
    assert_eq!(
        *h.v.subscription(),
        Subscription::Subscribed(b("新闻.频道"))
    );
    assert_eq!(h.last(), "Subscribed to channel <新闻.频道>");
    assert!(h.v.can_unsubscribe());
}

/// P-52:同时只订阅一个频道:已经订阅(或正在订阅)时再点 Subscribe 什么也不做。
#[test]
fn only_one_channel_at_a_time() {
    let mut h = H::new();
    h.send(PubSubMsg::SetChannel("a".into()));
    h.send(PubSubMsg::Subscribe);
    h.send(PubSubMsg::SetChannel("b".into()));
    assert!(
        h.send(PubSubMsg::Subscribe).cmds.is_empty(),
        "while subscribing"
    );
    h.send(PubSubMsg::Subscribed {
        generation: 0,
        result: Ok(()),
    });
    assert!(
        h.send(PubSubMsg::Subscribe).cmds.is_empty(),
        "while subscribed"
    );
    assert_eq!(*h.v.subscription(), Subscription::Subscribed(b("a")));
}

#[test]
fn a_failed_subscribe_is_reported_and_leaves_you_unsubscribed() {
    let mut h = H::new();
    h.send(PubSubMsg::SetChannel("a".into()));
    h.send(PubSubMsg::Subscribe);
    let out = h.send(PubSubMsg::Subscribed {
        generation: 0,
        result: Err(Error::Timeout("SUBSCRIBE".into())),
    });
    assert_eq!(*h.v.subscription(), Subscription::None);
    assert_eq!(out.notes.last().unwrap().level, Level::Error);
    assert!(
        h.last().contains("Timed out") || h.last().to_lowercase().contains("timed out"),
        "{}",
        h.last()
    );
    assert!(h.v.can_subscribe(), "can try again");
}

/// v1 的毛病:订阅后改了输入框再点退订,会退错频道。这里退订用记下来的频道。
#[test]
fn unsubscribing_uses_the_remembered_channel_not_the_input_box() {
    let mut h = H::new();
    h.subscribe("orders");
    h.send(PubSubMsg::SetChannel("something-else".into()));
    let out = h.send(PubSubMsg::Unsubscribe);
    assert!(matches!(
        cmds(&out)[..],
        [BackendCmd::PubSubUnsubscribe { channel, .. }] if *channel == b("orders")
    ));
    assert_eq!(
        *h.v.subscription(),
        Subscription::Unsubscribing(b("orders"))
    );
    h.send(PubSubMsg::Unsubscribed {
        generation: 0,
        result: Ok(()),
    });
    assert_eq!(*h.v.subscription(), Subscription::None);
    assert_eq!(h.last(), "Unsubscribed from channel <orders>");
    // 没订阅时退订什么也不发
    assert!(h.send(PubSubMsg::Unsubscribe).cmds.is_empty());
}

/// core 不管 UNSUBSCRIBE 成败都会关订阅客户端:失败也算退订了,但要报错。
#[test]
fn a_failed_unsubscribe_still_ends_the_subscription_and_is_reported() {
    let mut h = H::new();
    h.subscribe("a");
    h.send(PubSubMsg::Unsubscribe);
    let out = h.send(PubSubMsg::Unsubscribed {
        generation: 0,
        result: Err(Error::Timeout("UNSUBSCRIBE".into())),
    });
    assert_eq!(*h.v.subscription(), Subscription::None);
    assert_eq!(out.notes.last().unwrap().level, Level::Error);
}

#[test]
fn incoming_messages_show_the_real_channel_payload_and_timestamp() {
    let mut h = H::new();
    h.subscribe("a");
    h.send(incoming(
        h.id,
        "来源".as_bytes(),
        "你好 \"世界\"".as_bytes(),
        123_456,
    ));
    let line = h.v.lines().last().unwrap();
    assert_eq!(
        line.at_ms, 123_456,
        "the time the message arrived, not 'now'"
    );
    assert_eq!(line.text, "Channel <来源> received: \"你好 \"世界\"\"");
}

/// 差别:二进制消息有损显示(v1 显示成空串)。
#[test]
fn a_binary_message_is_shown_lossily_not_as_an_empty_string() {
    let mut h = H::new();
    h.subscribe("a");
    h.send(incoming(h.id, b"a", &[b'x', 0xff, b'y'], 1));
    let text = h.last();
    assert!(
        text.contains('x') && text.contains('y') && text.contains('\u{fffd}'),
        "{text}"
    );
}

/// 订阅着才显示:退订之后、监听任务结束之前还在路上的消息不显示;没订阅过也不显示。
#[test]
fn messages_are_only_shown_while_subscribed() {
    let mut h = H::new();
    h.send(incoming(h.id, b"a", b"early", 1));
    assert_eq!(h.v.line_count(), 0, "never subscribed");
    h.subscribe("a");
    h.send(PubSubMsg::Unsubscribe);
    h.send(incoming(h.id, b"a", b"in flight", 2));
    assert_eq!(
        h.v.line_count(),
        2,
        "subscribed line + the in-flight one while unsubscribing"
    );
    h.send(PubSubMsg::Unsubscribed {
        generation: 0,
        result: Ok(()),
    });
    let n = h.v.line_count();
    h.send(incoming(h.id, b"a", b"late", 3));
    assert_eq!(h.v.line_count(), n, "dropped after the unsubscribe");
}

/// 订阅结果回来之前就可能有消息到(订阅命令已经登记到服务端):不能丢。
#[test]
fn a_message_that_beats_the_subscribe_result_is_kept() {
    let mut h = H::new();
    h.send(PubSubMsg::SetChannel("a".into()));
    h.send(PubSubMsg::Subscribe);
    h.send(incoming(h.id, b"a", b"fast", 1));
    assert!(h.last().contains("fast"));
}

#[test]
fn publish_validates_then_sends_the_multiline_text_as_is() {
    let mut h = H::new();
    assert!(h.send(PubSubMsg::Publish).cmds.is_empty());
    assert_eq!(h.last(), "Channel is required.");
    h.send(PubSubMsg::SetChannel("ch".into()));
    assert!(h.send(PubSubMsg::Publish).cmds.is_empty());
    assert_eq!(h.last(), "Message is required.");

    h.send(PubSubMsg::SetMessage("第一行\n第二行\n  缩进\n".into()));
    let out = h.send(PubSubMsg::Publish);
    assert!(matches!(
        cmds(&out)[..],
        [BackendCmd::PubSubPublish { channel, message, .. }]
            if *channel == b("ch") && *message == b("第一行\n第二行\n  缩进\n")
    ));
    // 订阅状态不受影响
    assert_eq!(*h.v.subscription(), Subscription::None);
}

#[test]
fn publish_results_are_reported() {
    let mut h = H::new();
    h.send(PubSubMsg::SetChannel("ch".into()));
    h.send(PubSubMsg::Published {
        generation: 0,
        result: Ok(3),
    });
    assert_eq!(h.last(), "Published to channel <ch>, 3 receiver(s).");
    let out = h.send(PubSubMsg::Published {
        generation: 0,
        result: Err(Error::Timeout("PUBLISH".into())),
    });
    assert_eq!(out.notes.last().unwrap().level, Level::Error);
}

#[test]
fn listing_channels() {
    let mut h = H::new();
    let out = h.send(PubSubMsg::ListChannels);
    assert!(matches!(
        cmds(&out)[..],
        [BackendCmd::PubSubChannels { generation: 0, .. }]
    ));
    h.send(PubSubMsg::Channels {
        generation: 0,
        result: Ok(vec![b("a"), b("新闻")]),
    });
    assert_eq!(h.texts(), ["##### Existing channels:", "a", "新闻"]);
    h.send(PubSubMsg::Clear);
    h.send(PubSubMsg::Channels {
        generation: 0,
        result: Ok(vec![]),
    });
    assert_eq!(h.texts(), ["(no channels)"]);
}

#[test]
fn clear_empties_the_messages_but_keeps_the_subscription() {
    let mut h = H::new();
    h.subscribe("a");
    h.send(incoming(h.id, b"a", b"x", 1));
    assert!(h.v.line_count() >= 2);
    h.send(PubSubMsg::Clear);
    assert_eq!(h.v.line_count(), 0);
    assert!(h.v.can_unsubscribe());
}

#[test]
fn the_message_area_keeps_the_last_lines_only() {
    let mut h = H::new();
    h.subscribe("a");
    for i in 0..(LINES_MAX + 20) {
        h.send(incoming(h.id, b"a", i.to_string().as_bytes(), i as i64));
    }
    assert_eq!(h.v.line_count(), LINES_MAX);
    assert!(h.last().contains(&(LINES_MAX + 19).to_string()));
}

/// 连接重建(重连、切库):订阅随旧连接结束,留一行说明;迟到的结果和消息作废。
#[test]
fn a_connection_reset_ends_the_subscription_and_drops_late_results() {
    let mut h = H::new();
    h.subscribe("orders");
    h.send(PubSubMsg::Publish); // 只为留下一个在途结果的代号
    h.v.reset_connection(9_000, "en");
    assert_eq!(*h.v.subscription(), Subscription::None);
    assert!(
        h.last().contains("orders") && h.last().contains("connection was reset"),
        "{}",
        h.last()
    );
    let before = h.v.line_count();
    // 旧连接上迟到的结果
    h.send(PubSubMsg::Published {
        generation: 0,
        result: Ok(1),
    });
    h.send(PubSubMsg::Subscribed {
        generation: 0,
        result: Ok(()),
    });
    assert_eq!(h.v.line_count(), before);
    assert_eq!(
        *h.v.subscription(),
        Subscription::None,
        "a late 'subscribed' does not resurrect it"
    );
    // 新连接上正常
    h.send(PubSubMsg::SetChannel("orders".into()));
    let out = h.send(PubSubMsg::Subscribe);
    assert!(matches!(
        cmds(&out)[..],
        [BackendCmd::PubSubSubscribe { generation: 1, .. }]
    ));
}

#[test]
fn a_reset_without_a_subscription_adds_no_line() {
    let mut h = H::new();
    h.v.reset_connection(1, "en");
    assert_eq!(h.v.line_count(), 0);
}
