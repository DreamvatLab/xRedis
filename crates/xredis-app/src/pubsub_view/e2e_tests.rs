//! Pub/Sub 的端到端:App 加 Runtime 加假后端。假后端的 publish 会回环给订阅了这个频道的连接,
//! `emit` 模拟别的客户端发来消息;消息经过和生产一样的出口(`Runtime::new` 设置的 sink)进到 App。

use bytes::Bytes;
use xredis_core::config::ConnId;

use super::*;
use crate::msg::Msg;
use crate::runtime::tests::Harness;
use crate::testing::FakeBackend;

async fn ready(fake: &std::sync::Arc<FakeBackend>, names: &[&str]) -> (Harness, Vec<ConnId>) {
    let ids: Vec<ConnId> = names
        .iter()
        .enumerate()
        .map(|(i, n)| fake.add_connection(n, i as i32))
        .collect();
    let mut h = Harness::new(fake);
    h.boot().await;
    for id in &ids {
        h.feed(Msg::OpenConnection(*id));
        h.settle(150).await;
    }
    (h, ids)
}

fn view(h: &Harness, id: ConnId) -> &PubSubView {
    &h.app.tab(id).expect("a tab").pubsub
}

fn send(h: &mut Harness, id: ConnId, msg: PubSubMsg) {
    h.feed(Msg::PubSub { id, msg });
}

fn texts(h: &Harness, id: ConnId) -> Vec<String> {
    view(h, id).lines().map(|l| l.text.clone()).collect()
}

async fn subscribe(h: &mut Harness, id: ConnId, channel: &str) {
    send(h, id, PubSubMsg::SetChannel(channel.into()));
    send(h, id, PubSubMsg::Subscribe);
    h.settle(100).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_publish_and_receive_on_the_same_connection() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a"]).await;
    let id = ids[0];
    subscribe(&mut h, id, "新闻").await;
    assert_eq!(
        *view(&h, id).subscription(),
        Subscription::Subscribed(Bytes::from("新闻"))
    );
    assert_eq!(fake.subscribed_channel(id), Some(Bytes::from("新闻")));

    send(&mut h, id, PubSubMsg::SetMessage("第一行\n第二行".into()));
    send(&mut h, id, PubSubMsg::Publish);
    h.settle(100).await;
    let t = texts(&h, id);
    assert!(
        t.contains(&"Channel <新闻> received: \"第一行\n第二行\"".to_string()),
        "{t:?}"
    );
    assert!(t.iter().any(|l| l.contains("1 receiver(s)")), "{t:?}");

    // 别的客户端发来的
    fake.emit(id, "新闻".as_bytes(), "来自别处".as_bytes());
    h.settle(100).await;
    assert!(texts(&h, id).last().unwrap().contains("来自别处"));
}

#[tokio::test(flavor = "multi_thread")]
async fn after_unsubscribing_nothing_more_is_shown() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a"]).await;
    let id = ids[0];
    subscribe(&mut h, id, "c").await;
    send(&mut h, id, PubSubMsg::Unsubscribe);
    h.settle(100).await;
    assert_eq!(*view(&h, id).subscription(), Subscription::None);
    assert_eq!(fake.subscribed_channel(id), None);
    let n = view(&h, id).line_count();
    fake.emit(id, b"c", b"late");
    h.settle(100).await;
    assert_eq!(view(&h, id).line_count(), n);
}

/// 两个 Tab 各订各的:消息按连接分流,不会串到别的 Tab。
#[tokio::test(flavor = "multi_thread")]
async fn messages_are_routed_to_the_connection_that_received_them() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a", "b"]).await;
    let (a, b) = (ids[0], ids[1]);
    subscribe(&mut h, a, "chan-a").await;
    subscribe(&mut h, b, "chan-b").await;
    fake.emit(a, b"chan-a", b"for a");
    fake.emit(b, b"chan-b", b"for b");
    h.settle(100).await;
    assert!(texts(&h, a).iter().any(|t| t.contains("for a")));
    assert!(!texts(&h, a).iter().any(|t| t.contains("for b")));
    assert!(texts(&h, b).iter().any(|t| t.contains("for b")));
    assert!(!texts(&h, b).iter().any(|t| t.contains("for a")));
}

/// 切库(连接重建)之后订阅结束,留一行说明;之后到的消息不显示。
#[tokio::test(flavor = "multi_thread")]
async fn switching_db_ends_the_subscription() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a"]).await;
    let id = ids[0];
    subscribe(&mut h, id, "orders").await;
    h.feed(Msg::SelectDb { id, db: 1 });
    h.settle(300).await;
    assert_eq!(*view(&h, id).subscription(), Subscription::None);
    assert!(
        texts(&h, id)
            .last()
            .unwrap()
            .contains("subscription to channel <orders> has ended")
    );
    let n = view(&h, id).line_count();
    fake.emit(id, b"orders", b"ghost");
    h.settle(100).await;
    assert_eq!(view(&h, id).line_count(), n);
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_channels_and_failures() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a"]).await;
    let id = ids[0];
    fake.set_channel_list(vec![Bytes::from("x"), Bytes::from("y")]);
    send(&mut h, id, PubSubMsg::ListChannels);
    h.settle(100).await;
    assert_eq!(texts(&h, id), ["##### Existing channels:", "x", "y"]);

    // 订阅失败:报错、回到没订阅
    fake.fail_next("subscribe", xredis_core::Error::Timeout("SUBSCRIBE".into()));
    subscribe(&mut h, id, "z").await;
    assert_eq!(*view(&h, id).subscription(), Subscription::None);
    assert!(
        h.app
            .messages
            .entries()
            .any(|e| e.text.to_lowercase().contains("timed out"))
    );
}

/// 没就绪的 Tab 上的操作忽略,不发请求;不存在的 Tab 收到消息不出错。
#[tokio::test(flavor = "multi_thread")]
async fn nothing_happens_while_not_ready_or_for_a_closed_tab() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.hang("connect");
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    h.settle(100).await;
    subscribe(&mut h, id, "c").await;
    assert_eq!(*view(&h, id).subscription(), Subscription::None);
    assert!(!fake.calls().iter().any(|c| c.starts_with("subscribe")));

    h.feed(Msg::CloseTab(id));
    h.settle(100).await;
    fake.emit(id, b"c", b"after close");
    h.feed(Msg::PubSub {
        id,
        msg: PubSubMsg::Subscribed {
            generation: 0,
            result: Ok(()),
        },
    });
    h.settle(100).await;
    assert!(h.app.tab(id).is_none());
}

/// 关 Tab 会关连接(core 随之结束监听任务):后端收到 close_connection。
#[tokio::test(flavor = "multi_thread")]
async fn closing_the_tab_closes_the_connection() {
    let fake = FakeBackend::new();
    let (mut h, ids) = ready(&fake, &["a"]).await;
    let id = ids[0];
    subscribe(&mut h, id, "c").await;
    h.feed(Msg::CloseTab(id));
    h.settle(200).await;
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.starts_with("close_connection")),
        "{:?}",
        fake.calls()
    );
}
