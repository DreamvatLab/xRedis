//! Tab 栏和主区域(P-04):每个连接一个可关闭的 Tab,Tab 内三个子页(Keys、Pub/Sub、Console)。
//! 子页的内容在 R-09 到 R-11 做,这里先放占位;连接中、连接失败的状态画在这里。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::msg::SubPage;
use xredis_app::{Msg, TabStatus};

use super::send;
use crate::model::MainView;

impl MainView {
    pub(crate) fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let main = v_flex().id("main").flex_1().min_w_0().h_full();
        if self.app.tabs.is_empty() {
            return main.items_center().justify_center().child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.app.t("conn.new")),
            );
        }
        main.child(self.render_tab_bar(cx))
            .child(self.render_tab_body(cx))
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_ix = self
            .app
            .tabs
            .iter()
            .position(|t| Some(t.id) == self.app.active)
            .unwrap_or(0);
        let ids: Vec<_> = self.app.tabs.iter().map(|t| t.id).collect();
        let view = cx.entity();
        let tabs = self.app.tabs.iter().map(|t| {
            let id = t.id;
            let name = self
                .app
                .connection(id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            Tab::new().label(name).suffix(
                Button::new(SharedString::from(format!("tab-close-{id}")))
                    .ghost()
                    .compact()
                    .icon(IconName::Close)
                    .tooltip(self.app.t("tab.close"))
                    .on_click(send(cx, move || Msg::CloseTab(id))),
            )
        });
        TabBar::new("tabs")
            .underline()
            .selected_index(active_ix)
            .on_click(move |ix: &usize, window, cx| {
                if let Some(id) = ids.get(*ix).copied() {
                    view.update(cx, |this, cx| {
                        this.dispatch(Msg::ActivateTab(id), window, cx)
                    });
                }
            })
            .children(tabs)
    }

    fn render_tab_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = v_flex().id("tab-body").flex_1().min_h_0().p_2().gap_2();
        let Some(tab) = self.app.active_tab() else {
            return body;
        };
        let id = tab.id;
        match &tab.status {
            TabStatus::Connecting | TabStatus::Switching { .. } => {
                body.items_center().justify_center().child(
                    div()
                        .id("tab-connecting")
                        .test_support()
                        .child(self.app.t("tab.connecting")),
                )
            }
            TabStatus::Failed(reason) => body.items_center().justify_center().gap_2().child(
                v_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("tab-failed")
                            .test_support()
                            .text_color(cx.theme().danger)
                            .child(format!("{}: {reason}", self.app.t("tab.failed"))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("retry-{id}")))
                            .primary()
                            .label(self.app.t("btn.retry"))
                            .on_click(send(cx, move || Msg::Reconnect(id))),
                    ),
            ),
            TabStatus::Ready(_) => body
                .child(self.render_subpage_switcher(id, tab.sub_page, cx))
                .child(
                    div()
                        .id("subpage-content")
                        .flex_1()
                        .text_color(cx.theme().muted_foreground)
                        .child(match tab.sub_page {
                            SubPage::Keys => self.app.t("tab.sub.keys"),
                            SubPage::PubSub => self.app.t("tab.sub.pubsub"),
                            SubPage::Console => self.app.t("tab.sub.console"),
                        }),
                ),
        }
    }

    fn render_subpage_switcher(
        &self,
        id: xredis_core::config::ConnId,
        current: SubPage,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let item = |key: &'static str, page: SubPage, label: String, cx: &mut Context<Self>| {
            Button::new(key)
                .ghost()
                .label(label)
                .selected(current == page)
                .on_click(send(cx, move || Msg::SetSubPage(id, page)))
        };
        h_flex()
            .id("subpage-switcher")
            .gap_1()
            .child(item(
                "subpage-keys",
                SubPage::Keys,
                self.app.t("tab.sub.keys"),
                cx,
            ))
            .child(item(
                "subpage-pubsub",
                SubPage::PubSub,
                self.app.t("tab.sub.pubsub"),
                cx,
            ))
            .child(item(
                "subpage-console",
                SubPage::Console,
                self.app.t("tab.sub.console"),
                cx,
            ))
    }
}
