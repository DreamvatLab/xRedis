//! 连接编辑器(P-02)。输入规则、能不能保存、错误文案都读 `App`(`ConnectionEditor`),这里只画。

use std::path::PathBuf;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::msg::{EditorText, FileField};

use super::dialogs::{bind_input, error_list, field, new_input};
use super::send_to;
use crate::model::MainView;

pub(crate) struct EditorBody {
    main: Entity<MainView>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    ordinal: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl EditorBody {
    pub(crate) fn new(
        main: Entity<MainView>,
        app: &xredis_app::App,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (draft, t) = {
            let draft = app.editor.clone().expect("the editor is open");
            let t = |k: &str| app.t(k);
            let editing = draft.editing.is_some();
            let hints = (
                t("editor.username_hint"),
                if editing {
                    t("editor.password_unchanged")
                } else {
                    String::new()
                },
            );
            (draft, hints)
        };
        let name = new_input(window, cx, &draft.name, "", false);
        let host = new_input(window, cx, &draft.host, "", false);
        let port = new_input(window, cx, &draft.port, "", false);
        let username = new_input(window, cx, &draft.username, t.0, false);
        let password = new_input(window, cx, &draft.password, t.1, true);
        let ordinal = new_input(window, cx, &draft.ordinal, "", false);
        let mut subs = vec![cx.observe(&main, |_, _, cx| cx.notify())];
        for (input, field) in [
            (&name, EditorText::Name),
            (&host, EditorText::Host),
            (&port, EditorText::Port),
            (&username, EditorText::Username),
            (&password, EditorText::Password),
            (&ordinal, EditorText::Ordinal),
        ] {
            subs.push(bind_input(
                window,
                cx,
                &main,
                input,
                move |text| Msg::EditorSetText(field, text),
                || Msg::EditorSave,
            ));
        }
        Self {
            main,
            name,
            host,
            port,
            username,
            password,
            ordinal,
            _subs: subs,
        }
    }

    pub(crate) fn first_input(&self) -> Entity<InputState> {
        self.name.clone()
    }
}

#[allow(clippy::too_many_arguments)]
fn file_row(
    main: &Entity<MainView>,
    id: &'static str,
    label: String,
    hint: Option<String>,
    path: Option<&PathBuf>,
    field: FileField,
    app: &xredis_app::App,
    cx: &App,
) -> impl IntoElement {
    let shown = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| app.t("editor.file_none"));
    v_flex()
        .gap_1()
        .child(div().text_sm().child(label))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-path")))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_sm()
                        .when(path.is_none(), |d| {
                            d.text_color(cx.theme().muted_foreground)
                        })
                        .child(shown),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-choose")))
                        .compact()
                        .label(app.t("btn.choose"))
                        .on_click(send_to(main, move || Msg::EditorPickFile(field))),
                )
                .when(path.is_some(), |d| {
                    d.child(
                        Button::new(SharedString::from(format!("{id}-clear")))
                            .compact()
                            .ghost()
                            .label(app.t("btn.clear"))
                            .on_click(send_to(main, move || Msg::EditorClearFile(field))),
                    )
                }),
        )
        .when_some(hint, |d, hint| {
            d.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(hint),
            )
        })
}

impl Render for EditorBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.main.clone();
        let app = &main.read(cx).app;
        let Some(e) = &app.editor else {
            return div().into_any_element(); // App 已经关了它,弹窗马上会被收掉
        };
        let t = |k: &str| app.t(k);
        let mut errors: Vec<String> = e.errors().into_iter().map(|f| f.text(&app.lang)).collect();
        errors.extend(e.save_error.clone());

        v_flex()
            .id("editor")
            .test_support()
            .gap_3()
            .px_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("editor.title")),
            )
            .child(field("editor-name", t("editor.name"), &self.name))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(field("editor-host", t("editor.host"), &self.host)),
                    )
                    .child(div().w(px(120.)).child(field(
                        "editor-port",
                        t("editor.port"),
                        &self.port,
                    ))),
            )
            .child(field(
                "editor-username",
                t("editor.username"),
                &self.username,
            ))
            .child(field(
                "editor-password",
                t("editor.password"),
                &self.password,
            ))
            .when(e.editing.is_some(), |d| {
                let view = main.clone();
                d.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t("editor.password_hint_keep")),
                        )
                        .child(
                            Checkbox::new("editor-clear-password")
                                .label(t("editor.password_clear"))
                                .checked(e.clear_password)
                                .on_click(move |on, window, cx| {
                                    let on = *on;
                                    view.update(cx, |this, cx| {
                                        this.dispatch(Msg::EditorClearPassword(on), window, cx)
                                    });
                                }),
                        ),
                )
            })
            .child(field("editor-ordinal", t("editor.ordinal"), &self.ordinal))
            .child({
                let view = main.clone();
                Checkbox::new("editor-tls")
                    .label(t("editor.tls"))
                    .checked(e.tls)
                    .on_click(move |on, window, cx| {
                        let on = *on;
                        view.update(cx, |this, cx| {
                            this.dispatch(Msg::EditorSetTls(on), window, cx)
                        });
                    })
            })
            .when(e.tls, |d| {
                d.child(file_row(
                    &main,
                    "editor-ca",
                    t("editor.ca_cert"),
                    Some(t("editor.ca_cert_hint")),
                    e.ca_cert.as_ref(),
                    FileField::CaCert,
                    app,
                    cx,
                ))
                .child(file_row(
                    &main,
                    "editor-cert",
                    t("editor.client_cert"),
                    None,
                    e.client_cert.as_ref(),
                    FileField::ClientCert,
                    app,
                    cx,
                ))
                .child(file_row(
                    &main,
                    "editor-key",
                    t("editor.client_key"),
                    Some(t("editor.client_key_hint")),
                    e.client_key.as_ref(),
                    FileField::ClientKey,
                    app,
                    cx,
                ))
            })
            .children(error_list("editor-errors", errors, cx))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("editor-cancel")
                            .label(t("btn.cancel"))
                            .on_click(send_to(&main, || Msg::EditorCancel)),
                    )
                    .child(
                        Button::new("editor-save")
                            .primary()
                            .label(t("btn.save"))
                            .disabled(!e.can_save())
                            .on_click(send_to(&main, || Msg::EditorSave)),
                    ),
            )
            .into_any_element()
    }
}
