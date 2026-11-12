//! 窗口顶部的菜单:File、Help(P-63)。一排带下拉菜单的 ghost 按钮;标题栏用系统原生的。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::msg::HelpLink;

use super::send_to;
use crate::model::MainView;

impl MainView {
    pub(crate) fn render_topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let app = &self.app;
        let (file, settings) = (app.t("menu.file"), app.t("menu.settings"));
        let help = app.t("menu.help");
        let (agreement, privacy) = (app.t("menu.agreement"), app.t("menu.privacy"));
        let (bugs, coffee) = (app.t("menu.report_bugs"), app.t("menu.coffee"));

        let file_view = view.clone();
        let help_view = view;
        h_flex()
            .id("topbar")
            .h(px(36.))
            .px_2()
            .gap_1()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("menu-file")
                    .ghost()
                    .label(file)
                    .dropdown_menu(move |menu, _, _| {
                        menu.item(
                            PopupMenuItem::new(settings.clone())
                                .on_click(send_to(&file_view, || Msg::OpenSettings)),
                        )
                    }),
            )
            .child(
                Button::new("menu-help")
                    .ghost()
                    .label(help)
                    .dropdown_menu(move |menu, _, _| {
                        menu.item(
                            PopupMenuItem::new(agreement.clone())
                                .on_click(send_to(&help_view, || Msg::ShowAgreement(true))),
                        )
                        .item(
                            PopupMenuItem::new(privacy.clone())
                                .on_click(send_to(&help_view, || Msg::ShowPrivacy(true))),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new(bugs.clone()).on_click(send_to(&help_view, || {
                                Msg::OpenHelpLink(HelpLink::ReportBugs)
                            })),
                        )
                        .item(
                            PopupMenuItem::new(coffee.clone())
                                .on_click(send_to(&help_view, || {
                                    Msg::OpenHelpLink(HelpLink::BuyCoffee)
                                })),
                        )
                    }),
            )
    }
}
