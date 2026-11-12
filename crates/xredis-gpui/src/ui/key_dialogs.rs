//! 改 key 名(P-27)和改 TTL(P-28)的对话框。输入规则(空名、TTL 为 0、超范围、改名冲突)
//! 和错误文字都由 `App`(`KeyView` 的 `InputDialog`)决定,这里只画输入框和 `error`。
//! 回车确认、Esc 取消;弹窗什么时候关由 `App` 的状态决定。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use xredis_app::keyview::{InputDialog, ViewMsg};
use xredis_core::config::ConnId;

use super::dialogs::{DialogKind, error_list, field, new_input};
use super::keyview::view_msg;
use super::send_to;
use crate::model::MainView;

type MsgFn = fn() -> ViewMsg;

pub(crate) struct KeyInputBody {
    main: Entity<MainView>,
    id: ConnId,
    kind: DialogKind,
    input: Entity<InputState>,
    _subs: Vec<Subscription>,
}

/// 这个弹窗对应的 `App` 状态。
pub(crate) fn dialog_of(
    app: &xredis_app::App,
    id: ConnId,
    kind: DialogKind,
) -> Option<&InputDialog> {
    let view = app.tab(id)?.view.as_ref()?;
    match kind {
        DialogKind::Rename => view.rename.as_ref(),
        DialogKind::Ttl => view.ttl.as_ref(),
        _ => None,
    }
}

impl KeyInputBody {
    pub(crate) fn new(
        main: Entity<MainView>,
        id: ConnId,
        kind: DialogKind,
        app: &xredis_app::App,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 弹窗是在 `MainView` 的更新里建的,不能再读它:初始文字由调用方从 `App` 传进来
        let text = dialog_of(app, id, kind)
            .map(|d| d.text.clone())
            .unwrap_or_default();
        let input = new_input(window, cx, &text, "", false);
        let make: fn(String) -> ViewMsg = match kind {
            DialogKind::Ttl => ViewMsg::SetTtlText,
            _ => ViewMsg::SetRenameText,
        };
        let confirm: fn() -> ViewMsg = match kind {
            DialogKind::Ttl => || ViewMsg::ConfirmTtl,
            _ => || ViewMsg::ConfirmRename,
        };
        let sub = {
            let main = main.clone();
            cx.subscribe_in(
                &input,
                window,
                move |_: &mut Self, state, event: &InputEvent, window, cx| {
                    let msg = match event {
                        InputEvent::Change => make(state.read(cx).value().to_string()),
                        InputEvent::PressEnter { .. } => confirm(),
                        _ => return,
                    };
                    main.update(cx, |this, cx| this.dispatch(view_msg(id, msg), window, cx));
                },
            )
        };
        let observe = cx.observe(&main, |_, _, cx| cx.notify());
        Self {
            main,
            id,
            kind,
            input,
            _subs: vec![sub, observe],
        }
    }

    pub(crate) fn first_input(&self) -> Entity<InputState> {
        self.input.clone()
    }
}

impl Render for KeyInputBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.main.clone();
        let (id, kind) = (self.id, self.kind);
        let app = &main.read(cx).app;
        let t = |k: &str| app.t(k);
        let Some(dialog) = dialog_of(app, id, kind) else {
            return div().into_any_element();
        };
        let ttl = kind == DialogKind::Ttl;
        let (prefix, title, label, cancel, ok): (&'static str, _, _, MsgFn, MsgFn) = if ttl {
            (
                "ttl",
                t("view.ttl.title"),
                t("view.ttl.label"),
                || ViewMsg::CancelTtl,
                || ViewMsg::ConfirmTtl,
            )
        } else {
            (
                "rename",
                t("view.rename.title"),
                t("view.rename.label"),
                || ViewMsg::CancelRename,
                || ViewMsg::ConfirmRename,
            )
        };
        let input_id: &'static str = if ttl { "ttl-input" } else { "rename-input" };
        let error_id: &'static str = if ttl { "ttl-error" } else { "rename-error" };
        let errors = error_list(error_id, dialog.error.iter().cloned().collect(), cx);
        let footer = gpui_kit::component::dialog::DialogFooter::new()
            .child(
                Button::new(SharedString::from(format!("{prefix}-cancel")))
                    .label(t("btn.cancel"))
                    .on_click(send_to(&main, move || view_msg(id, cancel()))),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-ok")))
                    .primary()
                    .label(t("btn.ok"))
                    .on_click(send_to(&main, move || view_msg(id, ok()))),
            );
        v_flex()
            .id(SharedString::from(format!("{prefix}-dialog")))
            .test_support()
            .gap_3()
            .px_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(field(input_id, label, &self.input))
            .when(ttl, |d| {
                d.child(
                    div()
                        .id("ttl-hint")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("view.ttl.hint")),
                )
            })
            .children(errors)
            .child(footer)
            .into_any_element()
    }
}

use gpui_kit::prelude::FluentBuilder as _;
