//! Console 子页(P-50):一个只读、可选中复制的输出控件,下面一行提示符加输入框。
//! 只画 `App` 里 `Console` 的状态,键盘变成 `ConsoleMsg`。
//!
//! - 输出用组件库的只读 `EditorState`:它自己做虚拟滚动(只画可见的行,官方说 5 万行以内没问题,
//!   输出上限 1 万行,由 `Console` 管),自带鼠标选中和 Ctrl+C 复制(P-50 的“可以选中复制”)。
//!   `Console::output_revision` 变了才整段重设(到了上限行数不变,不能靠行数),并滚到底。
//! - 输入框用组件库的单行 `InputState`:光标、选区、输入法、Home/End、按单词移动(Ctrl+左右)和
//!   按单词删除(Ctrl+退格、Ctrl+Delete)都是它自带的,每次改动把整行发给 `App`(`SetInput`)。
//!   组件库的单词边界和 `Console::word_left` 不完全一样(App 那份留着,给不用组件库输入框的前端),
//!   这里用组件库的。
//! - GUI 翻译的键:回车 `Submit`;↑↓ `HistoryPrev`、`HistoryNext`(在输入框的祖先上用 `capture_action`
//!   先截住 `MoveUp`、`MoveDown`,单行输入框本来就不用它们);Ctrl+C 在输入框里没有选中文字时是
//!   `Interrupt`,有选中时还是复制。
//! - 等待动画的帧和什么时候重画都是 `App` 的:帧读 `App::console_waiting_frame`,定时是 `App` 排的
//!   `ConsoleMsg::Tick`(`PlatformCmd::After`)。

use std::collections::HashMap;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::input::{
    Copy, Editor as CodeEditor, EditorState, Input, InputEvent, InputState, MoveDown, MoveUp,
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::console::{ConsoleMsg, PROMPT};
use xredis_app::msg::SubPage;
use xredis_core::config::ConnId;

use super::dialogs::new_input;
use super::keyview::defer_dispatch;
use crate::model::MainView;

/// 每个 Tab 一份:输出控件、输入框。内容的真相在 `App`。
pub(crate) struct ConsoleUi {
    input: Entity<InputState>,
    output: Entity<EditorState>,
    /// 输出控件里现在放的是 `Console::output_revision` 的哪一版。
    shown_revision: u64,
    /// 上一次同步时这一页是不是看得见(刚切过来时让输入框拿焦点)。
    was_visible: bool,
    _subs: Vec<Subscription>,
}

pub(crate) type ConsoleUiMap = HashMap<ConnId, ConsoleUi>;

fn console_msg(id: ConnId, msg: ConsoleMsg) -> Msg {
    Msg::Console { id, msg }
}

impl MainView {
    /// 每个 Tab 有自己的控件,关掉的 Tab 的释放;`App` 的输入行和输出变了就写回控件。
    pub(crate) fn sync_console_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<ConnId> = self.app.tabs.iter().map(|t| t.id).collect();
        self.console_ui.retain(|id, _| ids.contains(id));
        for id in ids {
            if !self.console_ui.contains_key(&id) {
                let ui = self.new_console_ui(id, window, cx);
                self.console_ui.insert(id, ui);
            }
            let visible = self.console_visible(id);
            let Some(console) = self.app.tab(id).map(|t| &t.console) else {
                continue;
            };
            let Some(ui) = self.console_ui.get_mut(&id) else {
                continue;
            };
            ui.input.update(cx, |s, cx| {
                if s.value().as_ref() != console.input() {
                    s.set_value(console.input().to_owned(), window, cx);
                }
            });
            if ui.shown_revision != console.output_revision() {
                ui.shown_revision = console.output_revision();
                let text = console.output_text();
                ui.output.update(cx, |s, cx| {
                    s.set_value(text, window, cx);
                    // 滚到底(偏移会按范围截断,下一次布局时生效)
                    s.set_scroll_offset(point(px(0.), px(-1.0e9)), cx);
                });
            }
            if visible && !ui.was_visible {
                ui.input.update(cx, |s, cx| s.focus(window, cx));
            }
            ui.was_visible = visible;
        }
    }

    /// 这个 Tab 的 Console 页现在是不是看得见。
    fn console_visible(&self, id: ConnId) -> bool {
        self.app.active == Some(id)
            && self.app.tab(id).is_some_and(|t| {
                t.sub_page == SubPage::Console
                    && matches!(t.status, xredis_app::TabStatus::Ready(_))
            })
    }

    fn new_console_ui(
        &mut self,
        id: ConnId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ConsoleUi {
        let (input_text, output_text, revision) = self
            .app
            .tab(id)
            .map(|t| {
                (
                    t.console.input().to_owned(),
                    t.console.output_text(),
                    t.console.output_revision(),
                )
            })
            .unwrap_or_default();
        let input = new_input(
            window,
            cx,
            &input_text,
            self.app.t("console.placeholder"),
            false,
        );
        let output = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .soft_wrap(true)
                .default_value(output_text);
            state.set_readonly(true, cx);
            state
        });
        let main = cx.entity();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |_: &mut MainView, state, event: &InputEvent, window, cx| {
                let msg = match event {
                    InputEvent::Change => ConsoleMsg::SetInput(state.read(cx).value().to_string()),
                    InputEvent::PressEnter { .. } => ConsoleMsg::Submit,
                    _ => return,
                };
                defer_dispatch(&main, console_msg(id, msg), window, cx);
            },
        );
        ConsoleUi {
            input,
            output,
            shown_revision: revision,
            was_visible: false,
            _subs: vec![sub],
        }
    }

    pub(crate) fn render_console_page(&self, id: ConnId, cx: &mut Context<Self>) -> AnyElement {
        let page = v_flex()
            .id("console-page")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_1();
        let Some(ui) = self.console_ui.get(&id) else {
            return page.into_any_element();
        };
        let theme = cx.theme().clone();
        let view = cx.entity();
        let input_state = ui.input.clone();
        let (up, down, copy) = (view.clone(), view.clone(), view);
        let frame = self.app.console_waiting_frame(id);
        let line = h_flex()
            .id("console-line")
            .test_support()
            .items_center()
            .gap_1()
            // 在输入框拿到按键之前截住:↑↓ 翻历史,没选中文字的 Ctrl+C 放弃当前行
            .capture_action(move |_: &MoveUp, window, cx| {
                defer_dispatch(&up, console_msg(id, ConsoleMsg::HistoryPrev), window, cx);
                cx.stop_propagation();
            })
            .capture_action(move |_: &MoveDown, window, cx| {
                defer_dispatch(&down, console_msg(id, ConsoleMsg::HistoryNext), window, cx);
                cx.stop_propagation();
            })
            .capture_action(move |_: &Copy, window, cx| {
                if input_state.read(cx).selected_range().is_empty() {
                    defer_dispatch(&copy, console_msg(id, ConsoleMsg::Interrupt), window, cx);
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(PROMPT.trim_end()),
            )
            .child(
                div()
                    .id("console-input")
                    .test_support()
                    .flex_1()
                    .child(Input::new(&ui.input)),
            )
            .children(frame.map(|f| {
                div()
                    .id("console-waiting")
                    .test_support()
                    .text_color(theme.muted_foreground)
                    .child(f.to_string())
            }));
        page.child(
            div()
                .id("console-output")
                .test_support()
                .flex_1()
                .min_h_0()
                .child(CodeEditor::new(&ui.output).h(relative(1.)).readonly(true)),
        )
        .child(line)
        .into_any_element()
    }

    /// 输入框里现在的文字(测试用)。
    #[cfg(test)]
    pub(crate) fn console_input_text(&self, id: ConnId, cx: &App) -> Option<String> {
        self.console_ui
            .get(&id)
            .map(|u| u.input.read(cx).value().to_string())
    }

    /// 输出控件里现在的文字和滚动位置(测试用)。
    #[cfg(test)]
    pub(crate) fn console_output_state(&self, id: ConnId, cx: &App) -> Option<(String, f32)> {
        self.console_ui.get(&id).map(|u| {
            let s = u.output.read(cx);
            (s.value().to_string(), f32::from(s.scroll_offset().y))
        })
    }

    /// 输入框有没有焦点(测试用)。
    #[cfg(test)]
    pub(crate) fn console_input_focused(&self, id: ConnId, window: &Window, cx: &App) -> bool {
        self.console_ui
            .get(&id)
            .is_some_and(|u| u.input.read(cx).focus_handle(cx).is_focused(window))
    }
}
