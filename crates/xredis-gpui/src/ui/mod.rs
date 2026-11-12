//! 界面:读 `App` 的状态画,用户操作变成 `Msg`。不做业务判断。
//!
//! 每个可点的控件都有稳定的 id(`menu-file`、`conn-<连接 id>`、`tab-<连接 id>`、`add-connection`……),
//! 无头测试靠它们找元素。

mod dialogs;
mod editor_dialog;
mod messagebar;
mod notice_dialogs;
mod settings_dialog;
mod sidebar;
mod tabs;
mod topbar;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;

use crate::model::MainView;

pub use dialogs::DialogKind;

impl Render for MainView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_topbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(self.render_main(cx)),
            )
            .child(self.render_messagebar(cx))
    }
}

/// 点击时发一条 `Msg` 的处理函数。
pub(crate) fn send(
    cx: &mut Context<MainView>,
    msg: impl Fn() -> Msg + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = cx.entity();
    move |_, window, cx| {
        let msg = msg();
        view.update(cx, |this, cx| this.dispatch(msg, window, cx));
    }
}

/// 给不在 `Context<MainView>` 里的闭包(菜单项)用:拿着 `Entity` 发消息。
pub(crate) fn send_to(
    view: &Entity<MainView>,
    msg: impl Fn() -> Msg + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, window, cx| {
        let msg = msg();
        view.update(cx, |this, cx| this.dispatch(msg, window, cx));
    }
}
