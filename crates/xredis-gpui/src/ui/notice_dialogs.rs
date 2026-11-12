//! 删除确认、错误框、免责协议、隐私政策(P-03、P-62)。一个视图按种类画不同的内容。
//! 文字都走 `App::t`;要不要同意过、点了什么发什么,都由 `App` 决定。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::app::Confirm;

use super::dialogs::DialogKind;
use super::send_to;
use crate::model::MainView;

pub(crate) struct NoticeBody {
    main: Entity<MainView>,
    kind: DialogKind,
    _sub: Subscription,
}

impl NoticeBody {
    pub(crate) fn new(main: Entity<MainView>, kind: DialogKind, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&main, |_, _, cx| cx.notify());
        Self {
            main,
            kind,
            _sub: sub,
        }
    }
}

fn title(text: String) -> impl IntoElement {
    div()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

/// 长文本(协议、隐私政策):标题加段落,可以滚动。
fn document(id: &'static str, sections: Vec<(String, String)>) -> impl IntoElement {
    v_flex()
        .id(id)
        .test_support()
        .max_h(px(420.))
        .overflow_y_scroll()
        .gap_2()
        .text_sm()
        .children(sections.into_iter().map(|(heading, body)| {
            v_flex()
                .gap_1()
                .when_some((!heading.is_empty()).then_some(heading), |d, h| {
                    d.child(div().font_weight(FontWeight::SEMIBOLD).child(h))
                })
                .child(body)
        }))
}

use gpui_kit::prelude::FluentBuilder as _;

fn footer() -> gpui_kit::component::dialog::DialogFooter {
    gpui_kit::component::dialog::DialogFooter::new()
}

impl Render for NoticeBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.main.clone();
        let app = &main.read(cx).app;
        let t = |k: &str| app.t(k);
        let root = v_flex().gap_3().px_4();
        match self.kind {
            DialogKind::Confirm => {
                let Some(Confirm::DeleteConnection { name, .. }) = &app.confirm else {
                    return div().into_any_element();
                };
                root.id("confirm")
                    .test_support()
                    .child(title(t("conn.delete.title")))
                    .child(
                        div()
                            .id("confirm-body")
                            .test_support()
                            .child(app.t_with("conn.delete.body", &[("name", name)])),
                    )
                    .child(
                        footer()
                            .child(
                                Button::new("confirm-cancel")
                                    .label(t("btn.cancel"))
                                    .on_click(send_to(&main, || Msg::ConfirmCancelled)),
                            )
                            .child(
                                Button::new("confirm-ok")
                                    .danger()
                                    .label(t("btn.delete"))
                                    .on_click(send_to(&main, || Msg::ConfirmAccepted)),
                            ),
                    )
                    .into_any_element()
            }
            DialogKind::Error => {
                let Some(err) = &app.error_dialog else {
                    return div().into_any_element();
                };
                root.id("error-dialog")
                    .test_support()
                    .child(title(err.title.clone()))
                    .child(
                        div()
                            .id("error-message")
                            .test_support()
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .text_color(cx.theme().danger)
                            .child(err.message.clone()),
                    )
                    .child(
                        footer()
                            .child(
                                Button::new("error-open-log")
                                    .label(t("btn.open_log"))
                                    .on_click(send_to(&main, || Msg::OpenMessageLog)),
                            )
                            .child(
                                Button::new("error-ok")
                                    .primary()
                                    .label(t("btn.ok"))
                                    .on_click(send_to(&main, || Msg::DismissError)),
                            ),
                    )
                    .into_any_element()
            }
            DialogKind::Privacy => {
                let sections = ["intro", "data", "storage", "security", "updates", "contact"]
                    .iter()
                    .map(|k| (t(&format!("privacy.{k}_title")), t(&format!("privacy.{k}"))))
                    .collect();
                root.id("privacy")
                    .test_support()
                    .child(title(t("privacy.title")))
                    .child(document("privacy-text", sections))
                    .child(
                        footer().child(
                            Button::new("privacy-close")
                                .primary()
                                .label(t("btn.close"))
                                .on_click(send_to(&main, || Msg::ShowPrivacy(false))),
                        ),
                    )
                    .into_any_element()
            }
            DialogKind::Agreement => {
                let sections = (0..=6)
                    .map(|i| {
                        (
                            t(&format!("disclaimer.p{i}_title")),
                            t(&format!("disclaimer.p{i}")),
                        )
                    })
                    .chain([(String::new(), t("disclaimer.signature"))])
                    .collect();
                let buttons = if app.agreed() {
                    // 已经同意过,从 Help 打开只是查看
                    h_flex().gap_2().justify_end().child(
                        Button::new("agreement-close")
                            .primary()
                            .label(t("btn.close"))
                            .on_click(send_to(&main, || Msg::ShowAgreement(false))),
                    )
                } else {
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("agreement-decline")
                                .label(t("disclaimer.decline"))
                                .tooltip(t("disclaimer.decline_tip"))
                                .on_click(send_to(&main, || Msg::DeclineDisclaimer)),
                        )
                        .child(
                            Button::new("agreement-accept")
                                .primary()
                                .label(t("disclaimer.agree"))
                                .on_click(send_to(&main, || Msg::AgreeDisclaimer)),
                        )
                };
                root.id("agreement")
                    .test_support()
                    .child(title(t("disclaimer.title")))
                    .child(document("agreement-text", sections))
                    .child(buttons)
                    .into_any_element()
            }
            DialogKind::Settings | DialogKind::Editor => div().into_any_element(),
        }
    }
}
