//! Pub/Sub 子页(P-52):频道输入加 Subscribe、Unsubscribe、Channels、Clear,多行消息框加 Publish,
//! 下面是消息记录:和 Console 的输出一样用只读 `EditorState`(自己做虚拟滚动、换行、能选中复制;
//! 长消息和多行消息能看全),文字整体由 `PubSubView::transcript` 给(时间列、多行缩进都在那里),
//! 这里只取本地时区的偏移(平台的事)。上限 1 万行由 `PubSubView` 管。
//! 按钮能不能点读 `PubSubView`(`can_subscribe` 等)。二进制消息的有损显示在 `PubSubView`(`show`)。

use std::collections::HashMap;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor as CodeEditor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::pubsub_view::PubSubMsg;
use xredis_core::config::ConnId;

use super::dialogs::new_input;
use super::keyview::defer_dispatch;
use super::send;
use crate::model::MainView;

/// 每个 Tab 一份:两个输入控件和列表的滚动位置。内容的真相在 `App`。
pub(crate) struct PubSubUi {
    channel: Entity<InputState>,
    message: Entity<TextareaState>,
    output: Entity<EditorState>,
    /// 列表滚到底时对应的 `PubSubView::revision`。
    shown_revision: u64,
    _subs: Vec<Subscription>,
}

pub(crate) type PubSubUiMap = HashMap<ConnId, PubSubUi>;

fn pubsub_msg(id: ConnId, msg: PubSubMsg) -> Msg {
    Msg::PubSub { id, msg }
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
                let text = view.transcript(super::local_offset_minutes);
                ui.output.update(cx, |s, cx| {
                    s.set_value(text, window, cx);
                    // 新消息来了就跟到底(和 v1 的终端一样;偏移按范围截断,下一次布局时生效)
                    s.set_scroll_offset(point(px(0.), px(-1.0e9)), cx);
                });
            }
        }
    }

    fn new_pubsub_ui(
        &mut self,
        id: ConnId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PubSubUi {
        let (channel_text, message_text, revision, transcript) = self
            .app
            .tab(id)
            .map(|t| {
                (
                    t.pubsub.channel.clone(),
                    t.pubsub.message.clone(),
                    t.pubsub.revision(),
                    t.pubsub.transcript(super::local_offset_minutes),
                )
            })
            .unwrap_or_default();
        let output = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .soft_wrap(true)
                .default_value(transcript);
            state.set_readonly(true, cx);
            state
        });
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
            output,
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
                    .child(CodeEditor::new(&ui.output).h(relative(1.)).readonly(true)),
            )
            .into_any_element()
    }

    /// 消息记录控件里现在的文字和滚动位置(测试用)。
    #[cfg(test)]
    pub(crate) fn pubsub_output_state(&self, id: ConnId, cx: &App) -> Option<(String, f32)> {
        self.pubsub_ui.get(&id).map(|u| {
            let s = u.output.read(cx);
            (s.value().to_string(), f32::from(s.scroll_offset().y))
        })
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
