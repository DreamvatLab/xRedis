//! Pub/Sub 子页(P-52):频道输入加 Subscribe、Unsubscribe、Channels、Clear,多行消息框加 Publish,
//! 下面是虚拟滚动的消息列表(`uniform_list`,上限 1 万行由 `PubSubView` 管)。
//! 按钮能不能点读 `PubSubView`(`can_subscribe` 等),时间文字的格式是 `PubSubLine::clock_text`;
//! 这里只取本地时区的偏移(平台的事)。二进制消息的有损显示在 `PubSubView`(`show`)。

use std::collections::HashMap;
use std::ops::Range;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::pubsub_view::PubSubMsg;
use xredis_core::config::ConnId;

use super::dialogs::new_input;
use super::keyview::defer_dispatch;
use super::send;
use crate::model::MainView;

const ROW_HEIGHT: f32 = 22.0;
const TIME_WIDTH: f32 = 96.0;

/// 每个 Tab 一份:两个输入控件和列表的滚动位置。内容的真相在 `App`。
pub(crate) struct PubSubUi {
    channel: Entity<InputState>,
    message: Entity<TextareaState>,
    scroll: UniformListScrollHandle,
    /// 列表滚到底时对应的 `PubSubView::revision`。
    shown_revision: u64,
    _subs: Vec<Subscription>,
}

pub(crate) type PubSubUiMap = HashMap<ConnId, PubSubUi>;

fn pubsub_msg(id: ConnId, msg: PubSubMsg) -> Msg {
    Msg::PubSub { id, msg }
}

/// 毫秒时间戳那一刻的本地时区偏移(分钟)。
fn local_offset_minutes(at_ms: i64) -> i32 {
    use chrono::{Local, TimeZone as _};
    Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map_or(0, |t| t.offset().local_minus_utc() / 60)
}

impl MainView {
    pub(crate) fn sync_pubsub_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<ConnId> = self.app.tabs.iter().map(|t| t.id).collect();
        self.pubsub_ui.retain(|id, _| ids.contains(id));
        for id in ids {
            if !self.pubsub_ui.contains_key(&id) {
                let ui = self.new_pubsub_ui(id, window, cx);
                self.pubsub_ui.insert(id, ui);
            }
            let Some(view) = self.app.tab(id).map(|t| &t.pubsub) else {
                continue;
            };
            if let Some(ui) = self.pubsub_ui.get_mut(&id)
                && ui.shown_revision != view.revision()
            {
                ui.shown_revision = view.revision();
                // 新消息来了就跟到底(和 v1 的终端一样)
                if view.line_count() > 0 {
                    ui.scroll
                        .scroll_to_item(view.line_count() - 1, ScrollStrategy::Bottom);
                }
            }
        }
    }

    fn new_pubsub_ui(
        &mut self,
        id: ConnId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PubSubUi {
        let (channel_text, message_text, revision) = self
            .app
            .tab(id)
            .map(|t| {
                (
                    t.pubsub.channel.clone(),
                    t.pubsub.message.clone(),
                    t.pubsub.revision(),
                )
            })
            .unwrap_or_default();
        let channel = new_input(
            window,
            cx,
            &channel_text,
            self.app.t("pubsub.channel_placeholder"),
            false,
        );
        let placeholder = self.app.t("pubsub.message_placeholder");
        let message = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder(placeholder);
            state.set_value(message_text, window, cx);
            state
        });
        let main = cx.entity();
        let on_channel = cx.subscribe_in(
            &channel,
            window,
            move |_: &mut MainView, state, event: &InputEvent, window, cx| {
                let msg = match event {
                    InputEvent::Change => PubSubMsg::SetChannel(state.read(cx).value().to_string()),
                    InputEvent::PressEnter { .. } => PubSubMsg::Subscribe,
                    _ => return,
                };
                defer_dispatch(&main, pubsub_msg(id, msg), window, cx);
            },
        );
        let main = cx.entity();
        let on_message = cx.subscribe_in(
            &message,
            window,
            move |_: &mut MainView, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let msg = PubSubMsg::SetMessage(state.read(cx).value().to_string());
                    defer_dispatch(&main, pubsub_msg(id, msg), window, cx);
                }
            },
        );
        PubSubUi {
            channel,
            message,
            scroll: UniformListScrollHandle::new(),
            shown_revision: revision,
            _subs: vec![on_channel, on_message],
        }
    }

    pub(crate) fn render_pubsub_page(&self, id: ConnId, cx: &mut Context<Self>) -> AnyElement {
        let page = v_flex()
            .id("pubsub-page")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2();
        let (Some(ui), Some(tab)) = (self.pubsub_ui.get(&id), self.app.tab(id)) else {
            return page.into_any_element();
        };
        let view = &tab.pubsub;
        let theme = cx.theme().clone();
        let channel_row = h_flex()
            .id("pubsub-channel-row")
            .gap_2()
            .items_center()
            .child(
                div()
                    .id("pubsub-channel")
                    .test_support()
                    .flex_1()
                    .child(Input::new(&ui.channel)),
            )
            .child(
                Button::new("pubsub-subscribe")
                    .primary()
                    .label(self.app.t("pubsub.subscribe"))
                    .disabled(!view.can_subscribe())
                    .on_click(send(cx, move || pubsub_msg(id, PubSubMsg::Subscribe))),
            )
            .child(
                Button::new("pubsub-unsubscribe")
                    .label(self.app.t("pubsub.unsubscribe"))
                    .disabled(!view.can_unsubscribe())
                    .on_click(send(cx, move || pubsub_msg(id, PubSubMsg::Unsubscribe))),
            )
            .child(
                Button::new("pubsub-channels")
                    .label(self.app.t("pubsub.channels"))
                    .on_click(send(cx, move || pubsub_msg(id, PubSubMsg::ListChannels))),
            )
            .child(
                Button::new("pubsub-clear")
                    .label(self.app.t("btn.clear"))
                    .disabled(!view.can_clear())
                    .on_click(send(cx, move || pubsub_msg(id, PubSubMsg::Clear))),
            );
        let message_row = h_flex()
            .id("pubsub-message-row")
            .gap_2()
            .items_start()
            .child(
                div()
                    .id("pubsub-message")
                    .test_support()
                    .flex_1()
                    .child(Textarea::new(&ui.message).h(px(80.))),
            )
            .child(
                Button::new("pubsub-publish")
                    .primary()
                    .label(self.app.t("pubsub.publish"))
                    .disabled(!view.can_publish())
                    .on_click(send(cx, move || pubsub_msg(id, PubSubMsg::Publish))),
            );
        let list = uniform_list(
            "pubsub-rows",
            view.line_count(),
            cx.processor(move |this, range: Range<usize>, _, cx| this.pubsub_rows(id, range, cx)),
        )
        .track_scroll(&ui.scroll)
        .size_full();
        page.child(channel_row)
            .child(message_row)
            .child(
                div()
                    .id("pubsub-list")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .child(list),
            )
            .into_any_element()
    }

    /// 虚拟滚动:只给可见范围的行。
    fn pubsub_rows(
        &self,
        id: ConnId,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(tab) = self.app.tab(id) else {
            return Vec::new();
        };
        let theme = cx.theme().clone();
        let start = range.start;
        tab.pubsub
            .lines()
            .skip(start)
            .take(range.len())
            .enumerate()
            .map(|(i, line)| {
                let ix = start + i;
                h_flex()
                    .id(SharedString::from(format!("pubsub-row-{ix}")))
                    .test_support()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(TIME_WIDTH))
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(line.clock_text(local_offset_minutes(line.at_ms))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(line.text.clone()),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// 消息框和频道框里现在的文字(测试用)。
    #[cfg(test)]
    pub(crate) fn pubsub_input_texts(&self, id: ConnId, cx: &App) -> Option<(String, String)> {
        self.pubsub_ui.get(&id).map(|u| {
            (
                u.channel.read(cx).value().to_string(),
                u.message.read(cx).value().to_string(),
            )
        })
    }
}
