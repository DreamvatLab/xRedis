//! 打开 key 之后的页面(P-20、P-21、P-27 至 P-30):String 的值编辑器和新建 key 的编辑器。
//! 哪些按钮可用、只读提示、标题、长度和 TTL 的文字都读 `App`(`KeyView`、`Editor`),这里只画。
//!
//! 输入框的内容在 `EditorForm` 的控件里,每次改动立刻变成 `ViewMsg`;`App` 里的草稿变了
//! (读完值、Undo、Format、十六进制视图)再由 `sync` 写回控件。语法高亮没有做(纯文本)。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor as CodeEditor, EditorState, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::keyview::display::Format;
use xredis_app::keyview::editor::Editor;
use xredis_app::keyview::{KeyView, ViewMsg};
use xredis_core::config::ConnId;
use xredis_core::keys::KeyType;
use xredis_core::types::list::Side;

use super::dialogs::{field, new_input};
use super::send;
use crate::model::MainView;

pub(crate) fn view_msg(id: ConnId, msg: ViewMsg) -> Msg {
    Msg::View { id, msg }
}

/// 输入控件的事件可能在 `MainView` 自己的更新里触发(这时不能再 `update` 它):推迟到更新之后发。
fn defer_dispatch(main: &Entity<MainView>, msg: Msg, window: &mut Window, cx: &mut App) {
    let main = main.clone();
    window.defer(cx, move |window, cx| {
        main.update(cx, |this, cx| this.dispatch(msg, window, cx));
    });
}

/// 编辑器的三个输入控件。页面里的 String 编辑器和(R-10b)成员编辑器弹窗共用。
pub(crate) struct EditorForm {
    pub(crate) key: Entity<InputState>,
    pub(crate) member: Entity<InputState>,
    pub(crate) value: Entity<EditorState>,
    /// 值控件里现在放的是不是十六进制转储(只在切换时重设,免得每次都生成大文本)。
    hex_shown: bool,
    _subs: Vec<Subscription>,
}

impl EditorForm {
    pub(crate) fn new<T: 'static>(
        main: &Entity<MainView>,
        id: ConnId,
        editor: &Editor,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let key = new_input(window, cx, &editor.key.text, "", false);
        let member = new_input(window, cx, &editor.member.text, "", false);
        let text = editor.value_text().into_owned();
        let value = cx.new(|cx| EditorState::new(window, cx).default_value(text));
        let mut subs = Vec::new();
        for (input, make) in [
            (&key, ViewMsg::SetKeyName as fn(String) -> ViewMsg),
            (&member, ViewMsg::SetMemberName as fn(String) -> ViewMsg),
        ] {
            let main = main.clone();
            subs.push(cx.subscribe_in(
                input,
                window,
                move |_: &mut T, state, event: &InputEvent, window, cx| {
                    let msg = match event {
                        InputEvent::Change => make(state.read(cx).value().to_string()),
                        InputEvent::PressEnter { .. } => ViewMsg::Save,
                        _ => return,
                    };
                    defer_dispatch(&main, view_msg(id, msg), window, cx);
                },
            ));
        }
        let main = main.clone();
        subs.push(cx.subscribe_in(
            &value,
            window,
            move |_: &mut T, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let msg = ViewMsg::SetValue(state.read(cx).value().to_string());
                    defer_dispatch(&main, view_msg(id, msg), window, cx);
                }
            },
        ));
        let mut form = Self {
            key,
            member,
            value,
            hex_shown: editor.hex,
            _subs: subs,
        };
        form.set_readonly(editor, cx);
        form
    }

    fn set_readonly<T: 'static>(&mut self, editor: &Editor, cx: &mut Context<T>) {
        let ro = editor.read_only() || editor.hex || editor.loading || editor.saving;
        self.value.update(cx, |s, cx| s.set_readonly(ro, cx));
    }

    /// `App` 里的草稿和控件对齐:不一样才重设(重设会把光标和滚动位置放回开头)。
    pub(crate) fn sync(&mut self, editor: &Editor, window: &mut Window, cx: &mut App) {
        for (state, text, ro) in [
            (&self.key, &editor.key.text, !editor.key_name_editable()),
            (
                &self.member,
                &editor.member.text,
                !editor.member_name_editable() || editor.read_only(),
            ),
        ] {
            state.update(cx, |s, cx| {
                if s.value().as_ref() != text.as_str() {
                    s.set_value(text.clone(), window, cx);
                }
                s.set_readonly(ro, cx);
            });
        }
        let ro = editor.read_only() || editor.hex || editor.loading || editor.saving;
        let hex_changed = self.hex_shown != editor.hex;
        self.hex_shown = editor.hex;
        self.value.update(cx, |s, cx| {
            if editor.hex {
                if hex_changed {
                    s.set_value(editor.value_text().into_owned(), window, cx);
                }
            } else if hex_changed || s.value().as_ref() != editor.value.text.as_str() {
                s.set_value(editor.value.text.clone(), window, cx);
            }
            s.set_readonly(ro, cx);
        });
    }
}

/// 每个 Tab 一份:当前视图(用代号区分)的编辑器控件。
pub(crate) struct ViewUi {
    generation: u64,
    form: EditorForm,
}

pub(crate) type ViewUiMap = std::collections::HashMap<ConnId, ViewUi>;

impl MainView {
    /// 让每个 Tab 的页面编辑器控件和 `App` 里的视图一致:开了就建,换了就重建,关了就释放。
    pub(crate) fn sync_view_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let main = cx.entity();
        let app = &self.app;
        let ui = &mut self.view_ui;
        let wanted: Vec<ConnId> = app
            .tabs
            .iter()
            .filter(|t| {
                t.view
                    .as_ref()
                    .and_then(|v| v.editor.as_ref())
                    .is_some_and(Editor::is_key_level)
            })
            .map(|t| t.id)
            .collect();
        ui.retain(|id, _| wanted.contains(id));
        for id in wanted {
            let Some((view, editor)) = app
                .tab(id)
                .and_then(|t| t.view.as_ref())
                .and_then(|v| Some((v, v.editor.as_ref()?)))
            else {
                continue;
            };
            let generation = view.generation();
            match ui.get_mut(&id) {
                Some(u) if u.generation == generation => u.form.sync(editor, window, cx),
                _ => {
                    let form = EditorForm::new(&main, id, editor, window, cx);
                    ui.insert(id, ViewUi { generation, form });
                }
            }
        }
    }

    /// 打开了 key 的页面。
    pub(crate) fn render_keyview(
        &self,
        id: ConnId,
        view: &KeyView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match (&view.editor, self.view_ui.get(&id)) {
            (Some(editor), Some(ui)) if editor.is_key_level() => {
                self.render_key_editor(id, view, editor, &ui.form, cx)
            }
            // 成员表(R-10b)
            _ => self.render_members_page(id, view, cx),
        }
    }

    fn render_key_editor(
        &self,
        id: ConnId,
        view: &KeyView,
        editor: &Editor,
        form: &EditorForm,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let t = |k: &str| self.app.t(k);
        let lang = &self.app.lang;
        let inputs = editor.inputs();
        let actions = editor.actions();
        let view_actions = view.actions();
        let creating = view.is_creating();
        let dash = || "-".to_string();

        let header = h_flex()
            .gap_3()
            .items_center()
            .child(
                div()
                    .id("keyview-title")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(view.title(lang)),
            )
            .when(!creating, |d| {
                d.child(
                    div()
                        .id("keyview-length")
                        .test_support()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{}: {}",
                            t("view.info.length"),
                            view.length_text().unwrap_or_else(dash)
                        )),
                )
                .child(
                    div()
                        .id("keyview-ttl")
                        .test_support()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{}: {}",
                            t("view.info.ttl"),
                            view.ttl_text().unwrap_or_else(dash)
                        )),
                )
                .child(
                    Button::new("key-rename")
                        .compact()
                        .label(t("view.btn.rename"))
                        .disabled(!view_actions.rename)
                        .on_click(send(cx, move || view_msg(id, ViewMsg::OpenRename))),
                )
                .child(
                    Button::new("key-ttl")
                        .compact()
                        .label(t("view.btn.ttl"))
                        .disabled(!view_actions.ttl)
                        .on_click(send(cx, move || view_msg(id, ViewMsg::OpenTtl))),
                )
            });

        // 语言下拉:v2.0 只有纯文本和 JSON,选择只影响 Format、Minify 能不能用
        let view_entity = cx.entity();
        let current = editor.format;
        let language = Button::new("string-language")
            .compact()
            .label(current.label())
            .icon(IconName::ChevronDown)
            .tooltip(t("view.language"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for f in Format::ALL {
                    menu = menu.item(
                        PopupMenuItem::new(f.label())
                            .checked(f == current)
                            .on_click(super::send_to(&view_entity, move || {
                                view_msg(id, ViewMsg::SetFormat(f))
                            })),
                    );
                }
                menu
            });
        let hex_on = editor.hex;
        let toolbar = h_flex()
            .id("string-toolbar")
            .test_support()
            .gap_2()
            .items_center()
            .child(language)
            .child(
                Button::new("string-format")
                    .compact()
                    .label(t("view.btn.format"))
                    .disabled(!actions.format)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::FormatJson))),
            )
            .child(
                Button::new("string-minify")
                    .compact()
                    .label(t("view.btn.minify"))
                    .disabled(!actions.minify)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::MinifyJson))),
            )
            .child(
                Button::new("string-hex")
                    .compact()
                    .label(t("view.btn.hex"))
                    .when(hex_on, |b| b.primary())
                    .disabled(!actions.hex)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::SetHex(!hex_on)))),
            );

        let notice = editor.notice(lang).map(|text| {
            div()
                .id("string-notice")
                .test_support()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.warning.opacity(0.15))
                .text_sm()
                .child(text)
        });

        let key_field = inputs
            .key_name
            .then(|| field("string-key", t("view.key_name"), &form.key));
        let member_label = match editor.kind() {
            KeyType::List => t("view.index"),
            _ => t("view.member_name"),
        };
        let member_field = inputs
            .member_name
            .then(|| field("string-member", member_label, &form.member));
        let position = inputs.position.then(|| {
            let side = editor.position;
            h_flex()
                .id("string-position")
                .test_support()
                .gap_2()
                .items_center()
                .child(div().text_sm().child(t("view.position")))
                .child(
                    Button::new("string-position-head")
                        .compact()
                        .label(t("view.position.head"))
                        .when(side == Side::Head, |b| b.primary())
                        .on_click(send(cx, move || {
                            view_msg(id, ViewMsg::SetPosition(Side::Head))
                        })),
                )
                .child(
                    Button::new("string-position-tail")
                        .compact()
                        .label(t("view.position.tail"))
                        .when(side == Side::Tail, |b| b.primary())
                        .on_click(send(cx, move || {
                            view_msg(id, ViewMsg::SetPosition(Side::Tail))
                        })),
                )
        });
        let value_label = match editor.kind() {
            KeyType::ZSet => t("view.score"),
            KeyType::Set => t("view.col.member"),
            _ => t("view.value"),
        };
        let loading = editor.loading;
        let value =
            v_flex()
                .flex_1()
                .min_h_0()
                .gap_1()
                .child(div().text_sm().child(value_label))
                .child(
                    div()
                        .id("string-value")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .child(CodeEditor::new(&form.value).h(relative(1.)).readonly(
                            editor.read_only() || editor.hex || loading || editor.saving,
                        )),
                )
                .when(loading, |d| {
                    d.child(
                        div()
                            .id("string-loading")
                            .test_support()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(t("view.loading")),
                    )
                });

        let footer = h_flex()
            .gap_2()
            .justify_end()
            .child(
                Button::new("string-undo")
                    .label(t("view.btn.undo"))
                    .disabled(!actions.undo)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::Undo))),
            )
            .child(
                Button::new("string-cancel")
                    .label(t("btn.cancel"))
                    .on_click(send(cx, move || view_msg(id, ViewMsg::CloseEditor))),
            )
            .child(
                Button::new("string-save")
                    .primary()
                    .label(t("btn.save"))
                    .loading(editor.saving)
                    .disabled(!actions.save)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::Save))),
            );

        v_flex()
            .id("keyview")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(header)
            .children(key_field)
            .children(member_field)
            .children(position)
            .child(toolbar)
            .children(notice)
            .child(value)
            .child(footer)
            .into_any_element()
    }
}

impl MainView {
    /// 成员表页(R-10b 画);这之前先放占位和关闭按钮。
    fn render_members_page(
        &self,
        id: ConnId,
        _view: &KeyView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("keyview-members")
            .test_support()
            .gap_2()
            .child(
                div()
                    .id("keyview-placeholder")
                    .test_support()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.app.t("keys.view_placeholder")),
            )
            .child(
                Button::new("keyview-close")
                    .label(self.app.t("btn.close"))
                    .on_click(send(cx, move || view_msg(id, ViewMsg::Close))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
impl MainView {
    /// 编辑器三个输入控件里现在的文字:(key 名, 成员名, 值)(测试用)。
    pub(crate) fn form_texts(&self, id: ConnId, cx: &App) -> Option<(String, String, String)> {
        let f = &self.view_ui.get(&id)?.form;
        Some((
            f.key.read(cx).value().to_string(),
            f.member.read(cx).value().to_string(),
            f.value.read(cx).value().to_string(),
        ))
    }
}
