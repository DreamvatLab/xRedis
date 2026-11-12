//! 侧栏:连接列表(P-03)。按 `App` 给的顺序(ordinal 升序、同序号更新时间新的在前)画,点一下打开 Tab,
//! 右键 Edit、Delete,可折叠(折叠状态由 `App` 持久化)。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;

use super::{send, send_to, text_tooltip};
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
            let button_menu = (edit.clone(), delete.clone(), view.clone());
            let name = self.app.connection_name(id);
            let subtitle = self.app.connection_subtitle(id);
            let initial: String = name.chars().next().map(String::from).unwrap_or_default();
            let row = div()
                .id(SharedString::from(format!("conn-{id}")))
                .test_support()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(active, |d| d.bg(theme.list_active))
                .when(!active, |d| d.hover(|d| d.bg(theme.list_hover)))
                .tooltip(text_tooltip(name.clone()))
                .on_click(send(cx, move || Msg::OpenConnection(id)))
                .context_menu(move |menu, _, _| {
                    connection_menu(menu, &menu_view, id, &edit, &delete)
                });
            if collapsed {
                row.child(div().w_full().text_center().child(initial))
            } else {
                let (edit, delete, menu_view) = button_menu;
                row.child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    Label::new(name)
                                        .font_weight(if open {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .overflow_hidden()
                                        .text_ellipsis(),
                                )
                                .children(subtitle.map(|s| {
                                    Label::new(s).text_xs().text_color(theme.muted_foreground)
                                })),
                        )
                        // 看得见的菜单按钮(右键菜单保留);外面这层挡住按下事件,点按钮不会同时打开连接
                        .child(
                            div()
                                .flex_none()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(
                                    Button::new(SharedString::from(format!("conn-menu-{id}")))
                                        .ghost()
                                        .compact()
                                        .icon(IconName::EllipsisVertical)
                                        .dropdown_menu(move |menu, _, _| {
                                            connection_menu(menu, &menu_view, id, &edit, &delete)
                                        }),
                                ),
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

/// Edit、Delete 两项:右键菜单和 ⋮ 按钮的下拉共用。
fn connection_menu(
    menu: PopupMenu,
    view: &Entity<MainView>,
    id: xredis_core::config::ConnId,
    edit: &str,
    delete: &str,
) -> PopupMenu {
    menu.item(
        PopupMenuItem::new(edit.to_string())
            .on_click(send_to(view, move || Msg::OpenEditor(Some(id)))),
    )
    .item(
        PopupMenuItem::new(delete.to_string())
            .on_click(send_to(view, move || Msg::DeleteRequested(id))),
    )
}
