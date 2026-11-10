//! 消息栏(P-64):底部显示最新一条消息;点开可以翻看最近 50 条,可以清空。toast 在 `model` 里弹。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::messages::Level;

use super::send;
use crate::model::MainView;

fn level_color(level: Level, cx: &App) -> Hsla {
    let t = cx.theme();
    match level {
        Level::Error => t.danger,
        Level::Warn => t.warning,
        Level::Success => t.success,
        Level::Info | Level::Debug => t.muted_foreground,
    }
}

impl MainView {
    pub(crate) fn render_messagebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = &self.app.messages;
        let latest = bar.latest().map(|e| (e.level, e.text.clone()));
        let open = bar.open;
        let count = bar.len();

        let log = open.then(|| {
            let entries: Vec<_> = bar
                .entries()
                .map(|e| (e.id, e.level, e.text.clone()))
                .collect();
            v_flex()
                .id("message-log")
                .test_support()
                .max_h(px(220.))
                .overflow_y_scroll()
                .p_2()
                .gap_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .justify_between()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.app.t("msgbar.title")),
                        )
                        .child(
                            Button::new("msgbar-clear")
                                .ghost()
                                .compact()
                                .label(self.app.t("msgbar.clear"))
                                .on_click(send(cx, || Msg::ClearMessages)),
                        ),
                )
                .when(entries.is_empty(), |d| {
                    d.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.app.t("msgbar.empty")),
                    )
                })
                // 最新的在上面
                .children(entries.into_iter().rev().map(|(id, level, text)| {
                    div()
                        .id(SharedString::from(format!("message-{id}")))
                        .test_support()
                        .text_sm()
                        .text_color(level_color(level, cx))
                        .child(text)
                }))
        });

        v_flex()
            .id("messagebar")
            .flex_none()
            .border_t_1()
            .border_color(cx.theme().border)
            .children(log)
            .child(
                h_flex()
                    .id("message-latest")
                    .test_support()
                    .h(px(28.))
                    .px_2()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(send(cx, || Msg::ToggleMessageLog))
                    .child(match latest {
                        Some((level, text)) => div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .text_color(level_color(level, cx))
                            .child(text),
                        None => div().flex_1(),
                    })
                    .child(
                        div()
                            .id("message-count")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{count}")),
                    ),
            )
    }
}
