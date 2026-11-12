//! 侧栏:连接列表(P-03)。按 `App` 给的顺序(ordinal 升序、同序号更新时间新的在前)画,点一下打开 Tab,
//! 右键 Edit、Delete,可折叠(折叠状态由 `App` 持久化)。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;

use super::{send, send_to};
use crate::model::MainView;

const EXPANDED_WIDTH: f32 = 260.0;
const COLLAPSED_WIDTH: f32 = 48.0;

impl MainView {
    pub(crate) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.app.settings.sider_menu_collapsed;
        let view = cx.entity();
        let theme = cx.theme().clone();

        let toggle = Button::new("toggle-sider")
            .ghost()
            .compact()
            .icon(if collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            })
            .tooltip(self.app.t(if collapsed {
                "conn.sider.expand"
            } else {
                "conn.sider.collapse"
            }))
            .on_click(send(cx, || Msg::ToggleSider));
        let add = Button::new("add-connection")
            .ghost()
            .compact()
            .icon(IconName::Plus)
            .tooltip(self.app.t("conn.new"))
            .on_click(send(cx, || Msg::OpenEditor(None)));

        let header = if collapsed {
            v_flex().gap_1().items_center().child(toggle).child(add)
        } else {
            h_flex()
                .gap_1()
                .justify_between()
                .child(toggle)
                .child(div().flex_1())
                .child(add)
        };

        let rows = self.app.connections.iter().map(|c| {
            let id = c.id;
            let open = self.app.tab(id).is_some();
            let active = self.app.active == Some(id);
            let (edit, delete) = (self.app.t("conn.menu.edit"), self.app.t("conn.menu.delete"));
            let menu_view = view.clone();
            let initial: String = c.name.chars().next().map(String::from).unwrap_or_default();
            let row = div()
                .id(SharedString::from(format!("conn-{id}")))
                .test_support()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(active, |d| d.bg(theme.list_active))
                .when(!active, |d| d.hover(|d| d.bg(theme.list_hover)))
                .on_click(send(cx, move || Msg::OpenConnection(id)))
                .context_menu(move |menu, _, _| {
                    menu.item(
                        PopupMenuItem::new(edit.clone())
                            .on_click(send_to(&menu_view, move || Msg::OpenEditor(Some(id)))),
                    )
                    .item(
                        PopupMenuItem::new(delete.clone())
                            .on_click(send_to(&menu_view, move || Msg::DeleteRequested(id))),
                    )
                });
            if collapsed {
                row.child(div().w_full().text_center().child(initial))
            } else {
                row.child(
                    v_flex()
                        .child(
                            div()
                                .font_weight(if open {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(c.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{}:{}", c.host, c.port)),
                        ),
                )
            }
        });

        v_flex()
            .id("sidebar")
            .flex_none()
            .w(px(if collapsed {
                COLLAPSED_WIDTH
            } else {
                EXPANDED_WIDTH
            }))
            .h_full()
            .p_1()
            .gap_1()
            .border_r_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(header)
            .child(
                v_flex()
                    .id("connection-list")
                    .flex_1()
                    .min_h_0()
                    .gap_px()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}
