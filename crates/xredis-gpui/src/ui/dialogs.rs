//! 弹窗的外壳:`App` 里哪个弹窗的状态是开的,界面就开哪个(`App` 是唯一的真相)。
//!
//! 每个弹窗的内容是一个独立的视图(`Entity`),自己观察 `MainView`,`App` 一变就重画,不依赖组件库
//! 弹窗层的重建时机。输入框的初始值取自 `App` 里的草稿;之后每次改动都立刻变成 `Msg` 发给 `App`,
//! 所以任何时候关掉再按 `App` 的状态重开,内容都不会丢。
//!
//! 键盘:回车提交、Esc 取消。组件库的弹窗层把回车和 Esc 变成 `Confirm`、`Cancel` 动作;输入框里的
//! 回车另外由输入框的 `PressEnter` 事件接住。弹窗什么时候关由 `App` 的状态决定,所以动作处理函数
//! 只发 `Msg`,一律返回 `false`(不让组件库自己关)。

use std::cell::Cell;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, dialog::Dialog, v_flex};
use gpui_kit::*;
use xredis_app::Msg;

use super::editor_dialog::EditorBody;
use super::notice_dialogs::NoticeBody;
use super::settings_dialog::SettingsBody;
use crate::model::MainView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Agreement,
    Privacy,
    Settings,
    Editor,
    Confirm,
    Error,
}

impl DialogKind {
    /// 同时开着时从下到上的顺序。
    pub const ALL: [DialogKind; 6] = [
        DialogKind::Agreement,
        DialogKind::Privacy,
        DialogKind::Settings,
        DialogKind::Editor,
        DialogKind::Confirm,
        DialogKind::Error,
    ];
}

/// 回车或 Esc 对应的 `Msg`(`None` 是这个键在这个弹窗里不起作用)。
type KeyMsg = Option<fn() -> Msg>;

impl MainView {
    /// `App` 里这个弹窗的状态是不是开着。
    fn dialog_wanted(&self, kind: DialogKind) -> bool {
        let app = &self.app;
        match kind {
            DialogKind::Agreement => app.show_agreement,
            DialogKind::Privacy => app.show_privacy,
            DialogKind::Settings => app.settings_editor.is_some(),
            DialogKind::Editor => app.editor.is_some(),
            DialogKind::Confirm => app.confirm.is_some(),
            DialogKind::Error => app.error_dialog.is_some(),
        }
    }

    /// 当前开着的弹窗,从下到上(测试用)。
    pub fn open_dialogs(&self) -> &[DialogKind] {
        &self.dialogs
    }

    /// 让界面上开着的弹窗和 `App` 的状态一致。
    pub(crate) fn sync_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted: Vec<DialogKind> = DialogKind::ALL
            .into_iter()
            .filter(|k| self.dialog_wanted(*k))
            .collect();
        let stale: Vec<usize> = (0..self.dialogs.len())
            .filter(|i| !wanted.contains(&self.dialogs[*i]))
            .collect();
        if let Some(&first) = stale.first() {
            if stale.len() == self.dialogs.len() - first {
                // 要关的正好是最上面的几个
                for _ in &stale {
                    window.close_dialog(cx);
                }
                self.dialogs.truncate(first);
            } else {
                // 关掉的在中间:全关,把剩下的按原顺序重开(内容取自 App,不会丢)
                window.close_all_dialogs(cx);
                let keep: Vec<DialogKind> = self
                    .dialogs
                    .iter()
                    .copied()
                    .filter(|k| wanted.contains(k))
                    .collect();
                self.dialogs.clear();
                for kind in keep {
                    self.open_dialog(kind, window, cx);
                    self.dialogs.push(kind);
                }
            }
        }
        for kind in wanted {
            if !self.dialogs.contains(&kind) {
                self.open_dialog(kind, window, cx);
                self.dialogs.push(kind);
            }
        }
    }

    fn open_dialog(&mut self, kind: DialogKind, window: &mut Window, cx: &mut Context<Self>) {
        let main = cx.entity();
        let notice = |cx: &mut Context<Self>| -> AnyView {
            let main = main.clone();
            cx.new(|cx| NoticeBody::new(main, kind, cx)).into()
        };
        let (width, enter, esc, body, first_input): (
            f32,
            KeyMsg,
            KeyMsg,
            AnyView,
            Option<Entity<InputState>>,
        ) = match kind {
            DialogKind::Editor => {
                let body = cx.new(|cx| EditorBody::new(main.clone(), &self.app, window, cx));
                let first = body.read(cx).first_input();
                (
                    520.,
                    Some(|| Msg::EditorSave),
                    Some(|| Msg::EditorCancel),
                    body.into(),
                    Some(first),
                )
            }
            DialogKind::Settings => {
                let body = cx.new(|cx| SettingsBody::new(main.clone(), &self.app, window, cx));
                let first = body.read(cx).first_input();
                (
                    480.,
                    Some(|| Msg::SettingsSave),
                    Some(|| Msg::SettingsCancel),
                    body.into(),
                    Some(first),
                )
            }
            DialogKind::Confirm => (
                420.,
                Some(|| Msg::ConfirmAccepted),
                Some(|| Msg::ConfirmCancelled),
                notice(cx),
                None,
            ),
            DialogKind::Error => (
                480.,
                Some(|| Msg::DismissError),
                Some(|| Msg::DismissError),
                notice(cx),
                None,
            ),
            DialogKind::Privacy => (
                640.,
                Some(|| Msg::ShowPrivacy(false)),
                Some(|| Msg::ShowPrivacy(false)),
                notice(cx),
                None,
            ),
            DialogKind::Agreement => {
                // 没同意过:必须点同意或拒绝,回车和 Esc 都不起作用(v1:不可关闭,点遮罩也不关)。
                // 同意过之后从 Help 打开只是查看:回车和 Esc 关闭。
                let close: KeyMsg = self
                    .app
                    .agreed()
                    .then_some((|| Msg::ShowAgreement(false)) as fn() -> Msg);
                (720., close, close, notice(cx), None)
            }
        };
        let focus_pending = Cell::new(first_input.is_some());
        window.open_dialog(cx, move |dialog: Dialog, window, cx| {
            if focus_pending.replace(false)
                && let Some(input) = &first_input
            {
                input.focus_handle(cx).focus(window, cx);
            }
            let (m1, m2) = (main.clone(), main.clone());
            dialog
                .w(px(width))
                .close_button(false)
                .overlay_closable(false)
                .on_ok(move |_, window, cx| {
                    if let Some(msg) = enter {
                        m1.update(cx, |this, cx| this.dispatch(msg(), window, cx));
                    }
                    false
                })
                .on_cancel(move |_, window, cx| {
                    if let Some(msg) = esc {
                        m2.update(cx, |this, cx| this.dispatch(msg(), window, cx));
                    }
                    false
                })
                .child(body.clone())
        });
    }
}

// ---- 弹窗内容共用的小零件 ----

/// 单行输入框,初始值取自 `App` 的草稿。
pub(super) fn new_input<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
    value: &str,
    placeholder: impl Into<SharedString>,
    masked: bool,
) -> Entity<InputState> {
    let (value, placeholder) = (value.to_string(), placeholder.into());
    cx.new(|cx| {
        let mut state = InputState::new(window, cx)
            .placeholder(placeholder)
            .masked(masked);
        state.set_value(value, window, cx);
        state
    })
}

/// 输入框的每次改动变成一条 `Msg`;回车发 `on_enter`(提交)。
pub(super) fn bind_input<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
    main: &Entity<MainView>,
    input: &Entity<InputState>,
    on_change: impl Fn(String) -> Msg + 'static,
    on_enter: fn() -> Msg,
) -> Subscription {
    let main = main.clone();
    cx.subscribe_in(
        input,
        window,
        move |_: &mut T, state, event: &InputEvent, window, cx| {
            let msg = match event {
                InputEvent::Change => on_change(state.read(cx).value().to_string()),
                InputEvent::PressEnter { .. } => on_enter(),
                _ => return,
            };
            main.update(cx, |this, cx| this.dispatch(msg, window, cx));
        },
    )
}

/// 带标签的一行。`id` 让无头测试能找到并点进输入框。
pub(super) fn field(
    id: &'static str,
    label: String,
    input: &Entity<InputState>,
) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_sm().child(label))
        .child(div().id(id).test_support().child(Input::new(input)))
}

/// 校验错误和保存失败的原因,红字列出来;没有就不画。
pub(super) fn error_list(
    id: &'static str,
    lines: Vec<String>,
    cx: &App,
) -> Option<impl IntoElement> {
    (!lines.is_empty()).then(|| {
        v_flex()
            .id(id)
            .test_support()
            .gap_px()
            .text_sm()
            .text_color(cx.theme().danger)
            .children(lines)
    })
}
