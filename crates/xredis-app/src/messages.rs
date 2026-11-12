//! 消息栏(P-64):底部显示最新一条消息,保留最近 50 条,点开可以翻看;同时有 toast 提示。

use std::collections::VecDeque;

/// 消息栏保留的条数。
pub const MAX_MESSAGES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub level: Level,
    pub text: String,
    /// 毫秒,UTC。
    pub at_ms: i64,
}

impl Entry {
    /// 消息日志每条前面的时间:`HH:MM:SS`,按给定的 UTC 偏移(分钟)。本地时区的偏移由 GUI 取,
    /// 格式规则和 Pub/Sub 的消息行共用 [`crate::pubsub_view::clock_text`],只是不带毫秒。
    pub fn time_text(&self, utc_offset_minutes: i32) -> String {
        let mut text = crate::pubsub_view::clock_text(self.at_ms, utc_offset_minutes);
        text.truncate("HH:MM:SS".len());
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub level: Level,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct MessageBar {
    entries: VecDeque<Entry>,
    toasts: Vec<Toast>,
    /// 消息列表是否展开。
    pub open: bool,
    next_id: u64,
}

impl MessageBar {
    /// 记一条消息;`toast` 为真时同时弹一个提示。返回这条消息的 id。
    pub fn push(&mut self, level: Level, text: impl Into<String>, toast: bool, at_ms: i64) -> u64 {
        let text = text.into();
        self.next_id += 1;
        let id = self.next_id;
        if toast {
            self.toasts.push(Toast {
                id,
                level,
                text: text.clone(),
            });
        }
        self.entries.push_back(Entry {
            id,
            level,
            text,
            at_ms,
        });
        while self.entries.len() > MAX_MESSAGES {
            self.entries.pop_front();
        }
        id
    }

    /// 底部显示的那一条:最新的。
    pub fn latest(&self) -> Option<&Entry> {
        self.entries.back()
    }

    /// 从旧到新。
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.toasts.clear();
    }
}
