//! Pub/Sub:订阅、退订、发布。
//!
//! 同时只订阅一个频道(界面规则,在 app 层保证);订阅用独立的订阅客户端,退订只关它,
//! 不动连接池和控制台客户端(L-06)。
//!
//! **监听器只在订阅客户端新建时注册一次**(L-05)。fred 的 `on_message` 每次调用都会新建一个
//! 广播接收者并 spawn 一个任务,而且接收者不按频道分流 —— 注册 N 次,同一条消息就会被投递 N 次。
//! 所以注册放在 [`ConnManager::subscriber_client`](crate::conn::ConnManager::subscriber_client)
//! 的 `on_created` 里,它保证每个客户端只调用一次。客户端被关掉后监听任务会随广播通道关闭而结束,
//! 这里再 `abort` 一次兜底,并且在测试里确认任务真的结束了、重新订阅后消息只投递一次。

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use fred::clients::Client;
use fred::interfaces::{ClientLike, EventInterface, PubsubInterface};
use fred::types::Value;
use tokio::task::JoinHandle;

use crate::config::ConnId;
use crate::conn::ConnManager;
use crate::error::Result;
use crate::value::{arg, command, value_to_bytes};

/// 收到的一条消息。频道名和内容都是字节(二进制消息界面有损显示)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubsubMessage {
    /// 哪个连接收到的(多个 Tab 共用一个出口,靠它分流)。
    pub conn: ConnId,
    pub channel: Bytes,
    pub payload: Bytes,
    /// 收到的时间(毫秒,UTC),界面显示时间戳用。
    pub at_ms: i64,
}

/// 消息的出口。生产实现把消息推进 channel;测试里换成计数器,验证“一条消息只投递一次”。
pub trait MessageSink: Send + Sync + 'static {
    fn deliver(&self, message: PubsubMessage);
}

impl<F> MessageSink for F
where
    F: Fn(PubsubMessage) + Send + Sync + 'static,
{
    fn deliver(&self, message: PubsubMessage) {
        self(message)
    }
}

/// 订阅管理:每个连接记着自己监听任务的句柄,关闭时一并结束。
pub struct Pubsub {
    manager: Arc<ConnManager>,
    sink: Arc<dyn MessageSink>,
    listeners: Mutex<std::collections::HashMap<ConnId, JoinHandle<()>>>,
    /// 监听器注册了几次(测试用:每个订阅客户端只能注册一次)。
    registered: std::sync::atomic::AtomicUsize,
}

impl Pubsub {
    pub fn new(manager: Arc<ConnManager>, sink: Arc<dyn MessageSink>) -> Self {
        Self {
            manager,
            sink,
            listeners: Mutex::new(Default::default()),
            registered: Default::default(),
        }
    }

    /// 订阅频道。订阅客户端不存在就新建,并**只在新建时**注册监听器。
    pub async fn subscribe(&self, id: ConnId, channel: &[u8]) -> Result<()> {
        let sink = self.sink.clone();
        let mut handle: Option<JoinHandle<()>> = None;
        let client = self
            .manager
            .subscriber_client(id, |client: &Client| {
                self.registered
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                handle = Some(spawn_listener(client, sink, id));
            })
            .await?;
        if let Some(h) = handle {
            // 同一个连接上一个监听任务(对应已经关掉的旧订阅客户端)应该早就结束了,保险起见 abort
            if let Some(old) = self.listeners.lock().unwrap().insert(id, h) {
                old.abort();
            }
        }
        client.subscribe(channel_name(channel)).await?;
        self.wait_until_registered(id, channel).await;
        Ok(())
    }

    /// fred 的 `subscribe()` 在订阅真正登记到服务端之前就返回了(实测:订阅后立刻发布,订阅者数量是 0),
    /// 界面显示“已订阅”之后马上有人发布就会漏掉消息。所以订阅后用控制台客户端查
    /// `PUBSUB CHANNELS <频道>`,等服务端列出这个频道再返回,最多等 `REGISTER_WAIT`。
    /// 等不到不算失败(订阅命令本身成功了),只记日志。
    async fn wait_until_registered(&self, id: ConnId, channel: &[u8]) {
        let deadline = std::time::Instant::now() + REGISTER_WAIT;
        loop {
            match self.channel_is_registered(id, channel).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(err) => {
                    tracing::debug!("确认订阅是否生效时出错,不再等待: {err}");
                    return;
                }
            }
            if std::time::Instant::now() >= deadline {
                tracing::debug!("订阅在 {REGISTER_WAIT:?} 内没有在服务端出现,不再等待");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn channel_is_registered(&self, id: ConnId, channel: &[u8]) -> Result<bool> {
        let client = self.manager.console_client(id).await?;
        let list: Vec<Value> = client
            .custom(
                command("PUBSUB"),
                vec![arg(b"CHANNELS"), arg(&glob_escape(channel))],
            )
            .await?;
        Ok(!list.is_empty())
    }

    /// 退订:先 UNSUBSCRIBE,再关掉订阅客户端(只关它,不动连接池和控制台客户端,L-06),
    /// 最后结束监听任务。
    pub async fn unsubscribe(&self, id: ConnId, channel: &[u8]) -> Result<()> {
        // 订阅客户端不存在(比如已经被关闭)就没有什么可退的;不要为了退订再建一个
        if !self.manager.cached(id).1 {
            self.stop_listener(id);
            return Ok(());
        }
        let client = self.manager.subscriber_client(id, |_| {}).await?;
        let result = client.unsubscribe(channel_name(channel)).await;
        // 不管 UNSUBSCRIBE 成败都关掉订阅客户端,不留一个还连着的
        let closed = self.manager.close_subscriber(id).await;
        self.stop_listener(id);
        result?;
        closed
    }

    /// 连接被整个关闭(关 Tab、切库)之后调用,结束监听任务。
    pub fn connection_closed(&self, id: ConnId) {
        self.stop_listener(id);
    }

    fn stop_listener(&self, id: ConnId) {
        if let Some(h) = self.listeners.lock().unwrap().remove(&id) {
            h.abort();
        }
    }

    /// 这个连接的监听任务是否还在跑(测试用)。
    pub fn listener_running(&self, id: ConnId) -> bool {
        self.listeners
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|h| !h.is_finished())
    }

    /// 监听器累计注册的次数(测试用)。
    pub fn registrations(&self) -> usize {
        self.registered.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// 订阅后等服务端登记的上限。
const REGISTER_WAIT: Duration = Duration::from_secs(2);

/// `PUBSUB CHANNELS` 的参数是 glob 模式,频道名里的特殊字符要转义才能精确匹配。
fn glob_escape(channel: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(channel.len());
    for &b in channel {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// fred 的频道名只收字符串:Pub/Sub 频道按文本处理(v1 同),二进制频道名有损。
fn channel_name(channel: &[u8]) -> String {
    String::from_utf8_lossy(channel).into_owned()
}

fn now_ms() -> i64 {
    crate::config::now_ms()
}

/// 在订阅客户端上挂监听任务,把消息交给 `sink`。**只能在客户端新建时调用一次。**
fn spawn_listener(client: &Client, sink: Arc<dyn MessageSink>, conn: ConnId) -> JoinHandle<()> {
    let mut rx = client.message_rx();
    tokio::spawn(async move {
        // 广播通道在客户端 quit 时关闭,recv 返回 Err,任务结束
        while let Ok(msg) = rx.recv().await {
            sink.deliver(PubsubMessage {
                conn,
                channel: Bytes::copy_from_slice(msg.channel.as_bytes()),
                payload: value_to_bytes(msg.value),
                at_ms: now_ms(),
            });
        }
    })
}

/// 发布(多行文本),用连接池里的客户端。返回收到消息的订阅者数量。
pub async fn publish(
    manager: &ConnManager,
    id: ConnId,
    channel: &[u8],
    message: &[u8],
) -> Result<u64> {
    let pool = manager.pool(id).await?;
    let n: i64 = pool
        .custom(command("PUBLISH"), vec![arg(channel), arg(message)])
        .await?;
    Ok(n.max(0) as u64)
}

/// `PUBSUB CHANNELS`(控制台客户端,和订阅客户端分开)。
pub async fn channels(manager: &ConnManager, id: ConnId) -> Result<Vec<Bytes>> {
    let client = manager.console_client(id).await?;
    let list: Vec<Value> = client
        .custom(command("PUBSUB"), vec![arg(b"CHANNELS")])
        .await?;
    Ok(list.into_iter().map(value_to_bytes).collect())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod glob_tests {
    use super::glob_escape;

    /// `PUBSUB CHANNELS` 的参数是 glob,频道名里的特殊字符要转义,否则 `news.*` 会匹配别的频道。
    #[test]
    fn glob_special_characters_are_escaped() {
        assert_eq!(glob_escape(b"plain:name"), b"plain:name");
        assert_eq!(glob_escape(b"a*b?c[d]e\\f"), b"a\\*b\\?c\\[d\\]e\\\\f");
        assert_eq!(glob_escape("会话".as_bytes()), "会话".as_bytes());
    }
}
