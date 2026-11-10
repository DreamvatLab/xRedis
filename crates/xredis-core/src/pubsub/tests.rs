//! Pub/Sub 的测试(都需要 Redis)。覆盖 P-52 的后端部分;L-05、L-06。
//! 还有架构师在第 16 节提的:订阅监听任务关闭后要结束,重新订阅后消息只投递一次。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::*;
use crate::conn::testutil::*;

#[derive(Clone, Default)]
struct Collector(Arc<Mutex<Vec<PubsubMessage>>>);

impl MessageSink for Collector {
    fn deliver(&self, message: PubsubMessage) {
        self.0.lock().unwrap().push(message);
    }
}

impl Collector {
    fn take(&self) -> Vec<PubsubMessage> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

struct Fx {
    env: TestEnv,
    ps: Arc<Pubsub>,
    sink: Collector,
    prefix: String,
}

impl Fx {
    fn new() -> Self {
        let env = TestEnv::new();
        let sink = Collector::default();
        let ps = Arc::new(Pubsub::new(env.manager.clone(), Arc::new(sink.clone())));
        Self {
            env,
            ps,
            sink,
            prefix: unique_prefix(),
        }
    }

    fn channel(&self, name: &str) -> Vec<u8> {
        format!("{}{name}", self.prefix).into_bytes()
    }

    async fn publish(&self, channel: &[u8], msg: &[u8]) -> u64 {
        publish(&self.env.manager, self.env.id, channel, msg)
            .await
            .unwrap()
    }

    /// 等收到至少 `n` 条消息,最多 5 秒。
    async fn wait_for(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.sink.count() < n {
            assert!(
                Instant::now() < deadline,
                "only {} of {n} messages arrived",
                self.sink.count()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 静观 `ms` 毫秒,用来确认多余的消息不会出现。
    async fn settle(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

async fn run<F, Fut>(f: F)
where
    F: FnOnce(Fx) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _g = CONTAINER_LOCK.read().await;
    with_timeout(60, async {
        let fx = Fx::new();
        let (manager, id) = (fx.env.manager.clone(), fx.env.id);
        f(fx).await;
        manager.close(id).await.unwrap();
    })
    .await;
}

/// P-52:订阅后能收到发布的消息,带频道、内容、时间戳;二进制内容原样(字节)交出去;
/// 发布返回订阅者数量。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn subscribe_receives_published_messages() {
    run(|fx| async move {
        let ch = fx.channel("news");
        fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
        let before = crate::config::now_ms();
        assert_eq!(fx.publish(&ch, b"line1\nline2").await, 1);
        assert_eq!(fx.publish(&ch, &[0xff, 0xfe]).await, 1);
        fx.wait_for(2).await;
        let got = fx.sink.take();
        assert_eq!(got[0].channel, Bytes::from(ch.clone()));
        assert_eq!(&got[0].payload[..], b"line1\nline2", "multi-line text");
        assert_eq!(
            &got[1].payload[..],
            &[0xff, 0xfe],
            "binary payload is delivered as bytes"
        );
        assert!(got[0].at_ms >= before);
        // 发给别的频道的不会收到
        fx.publish(&fx.channel("other"), b"x").await;
        fx.settle(300).await;
        assert_eq!(fx.sink.count(), 0);
    })
    .await;
}

/// L-05:监听器只注册一次。订阅多个频道、同一频道订阅两次,同一条消息也只投递一次。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l05_each_message_is_delivered_exactly_once() {
    run(|fx| async move {
        let (a, b) = (fx.channel("a"), fx.channel("b"));
        fx.ps.subscribe(fx.env.id, &a).await.unwrap();
        fx.ps.subscribe(fx.env.id, &b).await.unwrap();
        fx.ps.subscribe(fx.env.id, &a).await.unwrap(); // 重复订阅
        assert_eq!(
            fx.ps.registrations(),
            1,
            "the listener must be registered once per client"
        );

        fx.publish(&a, b"one").await;
        fx.publish(&b, b"two").await;
        fx.wait_for(2).await;
        fx.settle(500).await; // 如果有重复投递,这段时间里会冒出来
        let got = fx.sink.take();
        assert_eq!(got.len(), 2, "duplicated delivery: {got:?}");
    })
    .await;
}

/// 架构师第 16 节:关闭后监听任务要结束;重新订阅(新的订阅客户端)后消息只投递一次,
/// 不能是新旧两个监听任务各投递一份。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn listener_ends_after_unsubscribe_and_resubscribe_delivers_once() {
    run(|fx| async move {
        let ch = fx.channel("c");
        fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
        assert!(fx.ps.listener_running(fx.env.id));
        fx.publish(&ch, b"first").await;
        fx.wait_for(1).await;
        fx.sink.take();

        fx.ps.unsubscribe(fx.env.id, &ch).await.unwrap();
        assert!(
            !fx.ps.listener_running(fx.env.id),
            "the listener task must end"
        );
        assert!(
            !fx.env.manager.cached(fx.env.id).1,
            "the subscriber client must be closed"
        );
        assert_eq!(fx.publish(&ch, b"nobody listening").await, 0);
        fx.settle(300).await;
        assert_eq!(fx.sink.count(), 0);

        // 重新订阅:新客户端、新监听器,注册总共两次(每个客户端一次),消息只来一份
        for round in 0..3 {
            fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
            fx.publish(&ch, b"again").await;
            fx.wait_for(1).await;
            fx.settle(400).await;
            assert_eq!(
                fx.sink.take().len(),
                1,
                "round {round}: duplicated delivery"
            );
            fx.ps.unsubscribe(fx.env.id, &ch).await.unwrap();
        }
        assert_eq!(fx.ps.registrations(), 4);
    })
    .await;
}

/// L-06:退订只关订阅客户端,连接池和控制台客户端不受影响,控制台还能查 PUBSUB CHANNELS。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn l06_unsubscribe_leaves_pool_and_console_alone() {
    run(|fx| async move {
        let ch = fx.channel("keep");
        let console = fx.env.manager.console_client(fx.env.id).await.unwrap();
        let _pool = fx.env.manager.pool(fx.env.id).await.unwrap();
        fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
        assert_eq!(fx.env.manager.cached(fx.env.id), (true, true, true));

        // 订阅中,控制台能看到这个频道
        let listed = channels(&fx.env.manager, fx.env.id).await.unwrap();
        assert!(listed.contains(&Bytes::from(ch.clone())), "{listed:?}");

        fx.ps.unsubscribe(fx.env.id, &ch).await.unwrap();
        assert_eq!(fx.env.manager.cached(fx.env.id), (true, false, true));
        use fred::interfaces::ClientLike;
        let pong: String = console
            .custom(
                crate::value::command("PING"),
                Vec::<fred::types::Value>::new(),
            )
            .await
            .unwrap();
        assert_eq!(pong, "PONG");
        let listed = channels(&fx.env.manager, fx.env.id).await.unwrap();
        assert!(!listed.contains(&Bytes::from(ch)), "{listed:?}");
    })
    .await;
}

/// 没有订阅时退订不报错、不新建订阅客户端;整个连接关闭时监听任务一并结束。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn unsubscribe_without_subscribing_is_a_noop_and_connection_close_stops_listener() {
    run(|fx| async move {
        let ch = fx.channel("x");
        fx.ps.unsubscribe(fx.env.id, &ch).await.unwrap();
        assert!(
            !fx.env.manager.cached(fx.env.id).1,
            "must not build a client just to unsubscribe"
        );

        fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
        assert!(fx.ps.listener_running(fx.env.id));
        fx.env.manager.close(fx.env.id).await.unwrap();
        fx.ps.connection_closed(fx.env.id);
        assert!(!fx.ps.listener_running(fx.env.id));
    })
    .await;
}

/// 发布多行文本;没有订阅者时返回 0。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn publish_returns_subscriber_count() {
    run(|fx| async move {
        let ch = fx.channel("p");
        assert_eq!(fx.publish(&ch, b"nobody").await, 0);
        fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
        assert_eq!(fx.publish(&ch, b"a\nb\nc").await, 1);
        fx.wait_for(1).await;
    })
    .await;
}

/// 订阅返回之后订阅就必须已经在服务端生效:订阅后**立刻**发布,订阅者数量必须是 1。
/// fred 的 `subscribe()` 在订阅登记前就返回,单次检查时序太宽松(没有等待也常常能过),
/// 所以用 40 个新频道各试一次,只要有一次漏了就失败。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs redis"]
async fn subscription_is_effective_when_subscribe_returns() {
    run(|fx| async move {
        for i in 0..40 {
            let ch = fx.channel(&format!("eff{i}"));
            fx.ps.subscribe(fx.env.id, &ch).await.unwrap();
            let delivered_to = fx.publish(&ch, b"right away").await;
            assert_eq!(
                delivered_to, 1,
                "channel {i}: the message was published before the subscription took effect"
            );
        }
        fx.wait_for(40).await;
    })
    .await;
}
