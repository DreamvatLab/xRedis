//! Pub/Sub 页的视图模型(R-07c,P-52):输入频道名订阅和退订(同时只订阅一个频道)、发布多行文本、
//! 列出频道、清空、带时间戳的消息行。
//!
//! 时间戳只存毫秒数(`PubSubLine::at_ms`),按本地时区怎么写由 GUI 定。消息的频道名和内容是字节,
//! 二进制的有损显示(v1 显示成空串)。

use std::collections::VecDeque;

use bytes::Bytes;
use xredis_core::Result;
use xredis_core::pubsub::PubsubMessage;

use crate::errors::describe;
use crate::i18n::{tr, tr_with};
use crate::keylist::{Ctx, Note, Out};
use crate::keyview::display::show;
use crate::messages::Level;
use crate::msg::{BackendCmd, Cmd};

/// 消息区最多保留的行数,超出丢最早的。
pub const LINES_MAX: usize = 10_000;

#[derive(Debug)]
pub enum PubSubMsg {
    // ---- 用户操作 ----
    SetChannel(String),
    SetMessage(String),
    Subscribe,
    Unsubscribe,
    Publish,
    /// `PUBSUB CHANNELS`。
    ListChannels,
    Clear,

    // ---- 异步结果 ----
    Subscribed {
        generation: u64,
        result: Result<()>,
    },
    Unsubscribed {
        generation: u64,
        result: Result<()>,
    },
    Published {
        generation: u64,
        result: Result<u64>,
    },
    Channels {
        generation: u64,
        result: Result<Vec<Bytes>>,
    },
    /// 订阅客户端收到一条消息(Runtime 从 core 的出口转进来)。
    Incoming(PubsubMessage),
}

impl PubSubMsg {
    pub fn is_result(&self) -> bool {
        matches!(
            self,
            PubSubMsg::Subscribed { .. }
                | PubSubMsg::Unsubscribed { .. }
                | PubSubMsg::Published { .. }
                | PubSubMsg::Channels { .. }
                | PubSubMsg::Incoming(_)
        )
    }
}

/// 订阅状态。同时只订阅一个频道;退订用的是**记下来的**频道,不是输入框里现在的内容
/// (v1 订阅后改了输入框再点退订,会退错频道)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subscription {
    None,
    Subscribing(Bytes),
    Subscribed(Bytes),
    Unsubscribing(Bytes),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSubLine {
    pub at_ms: i64,
    pub text: String,
}

impl PubSubLine {
    /// 这一行的时间文字(`HH:MM:SS.mmm`,按给定的 UTC 偏移),见 [`clock_text`]。
    pub fn clock_text(&self, utc_offset_minutes: i32) -> String {
        clock_text(self.at_ms, utc_offset_minutes)
    }
}

/// 毫秒时间戳显示成 `HH:MM:SS.mmm`(按 UTC 偏移的分钟数,不显示日期:消息列表是实时看的)。
/// 本地时区的偏移由平台层取(GUI),这里只管格式,所以测试不依赖机器的时区。
pub fn clock_text(at_ms: i64, utc_offset_minutes: i32) -> String {
    let local = at_ms + i64::from(utc_offset_minutes) * 60_000;
    let day = local.rem_euclid(86_400_000);
    let (h, m, s, ms) = (
        day / 3_600_000,
        day / 60_000 % 60,
        day / 1000 % 60,
        day % 1000,
    );
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[derive(Debug)]
pub struct PubSubView {
    pub channel: String,
    pub message: String,
    subscription: Subscription,
    lines: VecDeque<PubSubLine>,
    /// 连接纪元:连接重建后加一,旧连接上迟到的结果对不上就丢弃。
    generation: u64,
    /// 消息行每变一次加一(到了上限行数不变,GUI 靠它知道要滚到底)。
    revision: u64,
}

impl Default for PubSubView {
    fn default() -> Self {
        Self::new()
    }
}

impl PubSubView {
    pub fn new() -> Self {
        Self {
            channel: String::new(),
            message: String::new(),
            subscription: Subscription::None,
            lines: VecDeque::new(),
            generation: 0,
            revision: 0,
        }
    }

    pub fn subscription(&self) -> &Subscription {
        &self.subscription
    }

    pub fn lines(&self) -> impl Iterator<Item = &PubSubLine> {
        self.lines.iter()
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 频道名输入框(去掉首尾空白)是空的:Subscribe、Publish 按钮不可点。
    pub fn channel_is_empty(&self) -> bool {
        self.channel.trim().is_empty()
    }

    pub fn message_is_empty(&self) -> bool {
        self.message.trim().is_empty()
    }

    pub fn can_subscribe(&self) -> bool {
        self.subscription == Subscription::None && !self.channel_is_empty()
    }

    /// Publish 按钮可用:频道和消息都有内容(和 `update` 的判断一致)。
    pub fn can_publish(&self) -> bool {
        !self.channel_is_empty() && !self.message.is_empty()
    }

    /// Clear 按钮可用:有行可清。
    pub fn can_clear(&self) -> bool {
        !self.lines.is_empty()
    }

    /// 消息行每变一次就不一样。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn can_unsubscribe(&self) -> bool {
        matches!(self.subscription, Subscription::Subscribed(_))
    }

    /// 连接重建了(重连、切库):订阅随旧连接结束,迟到的结果作废;消息记录保留。
    pub fn reset_connection(&mut self, now: i64, lang: &str) {
        self.generation += 1;
        if let Subscription::Subscribed(ch) | Subscription::Subscribing(ch) =
            std::mem::replace(&mut self.subscription, Subscription::None)
        {
            self.push(
                now,
                tr_with(
                    lang,
                    "pubsub.connection_reset",
                    &[("channel", &show(&ch).text)],
                ),
            );
        }
    }

    fn push(&mut self, at_ms: i64, text: String) {
        self.revision += 1;
        self.lines.push_back(PubSubLine { at_ms, text });
        while self.lines.len() > LINES_MAX {
            self.lines.pop_front();
        }
    }

    fn fail(&mut self, ctx: &Ctx, err: &xredis_core::Error, out: &mut Out) {
        let text = describe(ctx.lang, err);
        self.push(ctx.now, text.clone());
        out.notes.push(Note {
            level: Level::Error,
            text,
            toast: true,
        });
    }

    pub fn update(&mut self, ctx: &Ctx, msg: PubSubMsg) -> Out {
        let mut out = Out::default();
        let name = |b: &Bytes| show(b).text;
        match msg {
            PubSubMsg::SetChannel(c) => self.channel = c,
            PubSubMsg::SetMessage(m) => self.message = m,
            PubSubMsg::Subscribe => {
                if self.subscription != Subscription::None {
                    return out;
                }
                if self.channel_is_empty() {
                    self.push(ctx.now, tr(ctx.lang, "pubsub.channel_required"));
                    return out;
                }
                let channel = Bytes::from(self.channel.trim().to_owned().into_bytes());
                self.subscription = Subscription::Subscribing(channel.clone());
                out.cmds.push(Cmd::Backend(BackendCmd::PubSubSubscribe {
                    id: ctx.id,
                    generation: self.generation,
                    channel,
                }));
            }
            PubSubMsg::Unsubscribe => {
                if let Subscription::Subscribed(channel) = self.subscription.clone() {
                    self.subscription = Subscription::Unsubscribing(channel.clone());
                    out.cmds.push(Cmd::Backend(BackendCmd::PubSubUnsubscribe {
                        id: ctx.id,
                        generation: self.generation,
                        channel,
                    }));
                }
            }
            PubSubMsg::Publish => {
                if self.channel_is_empty() {
                    self.push(ctx.now, tr(ctx.lang, "pubsub.channel_required"));
                } else if self.message.is_empty() {
                    self.push(ctx.now, tr(ctx.lang, "pubsub.message_required"));
                } else {
                    out.cmds.push(Cmd::Backend(BackendCmd::PubSubPublish {
                        id: ctx.id,
                        generation: self.generation,
                        channel: Bytes::from(self.channel.trim().to_owned().into_bytes()),
                        message: Bytes::from(self.message.clone().into_bytes()),
                    }));
                }
            }
            PubSubMsg::ListChannels => out.cmds.push(Cmd::Backend(BackendCmd::PubSubChannels {
                id: ctx.id,
                generation: self.generation,
            })),
            PubSubMsg::Clear => {
                self.lines.clear();
                self.revision += 1;
            }

            PubSubMsg::Subscribed { generation, result } => {
                if generation != self.generation {
                    return out;
                }
                let Subscription::Subscribing(channel) = self.subscription.clone() else {
                    return out;
                };
                match result {
                    Ok(()) => {
                        self.push(
                            ctx.now,
                            tr_with(
                                ctx.lang,
                                "pubsub.subscribed",
                                &[("channel", &name(&channel))],
                            ),
                        );
                        self.subscription = Subscription::Subscribed(channel);
                    }
                    Err(err) => {
                        self.subscription = Subscription::None;
                        self.fail(ctx, &err, &mut out);
                    }
                }
            }
            PubSubMsg::Unsubscribed { generation, result } => {
                if generation != self.generation {
                    return out;
                }
                let Subscription::Unsubscribing(channel) = self.subscription.clone() else {
                    return out;
                };
                // core 不管 UNSUBSCRIBE 成败都会关掉订阅客户端,所以失败时订阅也已经没了
                self.subscription = Subscription::None;
                match result {
                    Ok(()) => self.push(
                        ctx.now,
                        tr_with(
                            ctx.lang,
                            "pubsub.unsubscribed",
                            &[("channel", &name(&channel))],
                        ),
                    ),
                    Err(err) => self.fail(ctx, &err, &mut out),
                }
            }
            PubSubMsg::Published { generation, result } => {
                if generation != self.generation {
                    return out;
                }
                match result {
                    Ok(n) => self.push(
                        ctx.now,
                        tr_with(
                            ctx.lang,
                            "pubsub.published",
                            &[("channel", self.channel.trim()), ("count", &n.to_string())],
                        ),
                    ),
                    Err(err) => self.fail(ctx, &err, &mut out),
                }
            }
            PubSubMsg::Channels { generation, result } => {
                if generation != self.generation {
                    return out;
                }
                match result {
                    Ok(list) if list.is_empty() => {
                        self.push(ctx.now, tr(ctx.lang, "pubsub.no_channels"))
                    }
                    Ok(list) => {
                        self.push(ctx.now, tr(ctx.lang, "pubsub.channels_header"));
                        for c in list {
                            self.push(ctx.now, name(&c));
                        }
                    }
                    Err(err) => self.fail(ctx, &err, &mut out),
                }
            }
            PubSubMsg::Incoming(m) => {
                // 退订之后、监听任务结束之前还在路上的消息不显示
                if matches!(self.subscription, Subscription::None) {
                    return out;
                }
                let text = tr_with(
                    ctx.lang,
                    "pubsub.received",
                    &[
                        ("channel", &name(&m.channel)),
                        ("payload", &show(&m.payload).text),
                    ],
                );
                self.push(m.at_ms, text);
            }
        }
        out
    }
}

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;
