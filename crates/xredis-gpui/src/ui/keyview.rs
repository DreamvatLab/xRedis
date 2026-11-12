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
pub(super) fn defer_dispatch(main: &Entity<MainView>, msg: Msg, window: &mut Window, cx: &mut App) {
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

/// 每个 Tab 一份:当前视图(用代号区分)的输入控件。
pub(crate) struct ViewUi {
    generation: u64,
    /// String 和新建 key 的页面编辑器。
    pub(crate) page: Option<EditorForm>,
    /// 成员编辑器弹窗里的控件,带编辑器的序号(`KeyView::editor_seq`),换了编辑器就重建。
    pub(crate) member: Option<(u64, EditorForm)>,
    /// 成员表的搜索框、过滤框和滚动位置。
    pub(crate) table: Option<super::members::TableUi>,
}

pub(crate) type ViewUiMap = std::collections::HashMap<ConnId, ViewUi>;

impl MainView {
    /// 让每个 Tab 的输入控件和 `App` 里的视图一致:开了就建,换了就重建,关了就释放。
    pub(crate) fn sync_view_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let main = cx.entity();
        let app = &self.app;
        let ui = &mut self.view_ui;
        ui.retain(|id, u| {
            app.tab(*id)
                .and_then(|t| t.view.as_ref())
                .is_some_and(|v| v.generation() == u.generation)
        });
        for tab in &app.tabs {
            let id = tab.id;
            let Some(view) = &tab.view else { continue };
            let u = ui.entry(id).or_insert_with(|| ViewUi {
                generation: view.generation(),
                page: None,
                member: None,
                table: None,
            });
            let editor = view.editor.as_ref();
            // 页面编辑器
            match editor.filter(|e| e.is_key_level()) {
                Some(e) => match &mut u.page {
                    Some(form) => form.sync(e, window, cx),
                    None => u.page = Some(EditorForm::new(&main, id, e, window, cx)),
                },
                None => u.page = None,
            }
            // 成员编辑器弹窗
            match editor.filter(|e| !e.is_key_level()) {
                Some(e) => match &mut u.member {
                    Some((seq, form)) if *seq == view.editor_seq() => form.sync(e, window, cx),
                    _ => {
                        u.member =
                            Some((view.editor_seq(), EditorForm::new(&main, id, e, window, cx)))
                    }
                },
                None => u.member = None,
            }
            // 成员表
            if view.table.is_some() && u.table.is_none() {
                u.table = Some(super::members::TableUi::new(
                    &main, id, &self.app, window, cx,
                ));
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
        let ui = self.view_ui.get(&id);
        match (&view.editor, ui.and_then(|u| u.page.as_ref())) {
            (Some(editor), Some(form)) if editor.is_key_level() => {
                self.render_key_editor(id, view, editor, form, cx)
            }
            _ => self.render_members_page(id, view, cx),
        }
    }

    /// 页面顶部:标题、长度、TTL、Rename 和 TTL 按钮(String 编辑器和成员表共用)。
    pub(crate) fn render_view_header(
        &self,
        id: ConnId,
        view: &KeyView,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let t = |k: &str| self.app.t(k);
        let lang = &self.app.lang;
        let view_actions = view.actions();
        let creating = view.is_creating();
        let dash = || "-".to_string();
        h_flex()
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
            })
    }

    fn render_key_editor(
        &self,
        id: ConnId,
        view: &KeyView,
        editor: &Editor,
        form: &EditorForm,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header = self.render_view_header(id, view, cx);
        let body = self.render_editor_body("string", id, editor, form, cx);
        v_flex()
            .id("keyview")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl MainView {
    /// 编辑器的主体(页面里的 String 编辑器和成员编辑器弹窗共用):key 名、成员名、头插尾插、
    /// 工具栏、只读提示、值、Undo 和 Cancel 和 Save。控件 id 以 `prefix` 开头(页面 `string`,弹窗 `member`)。
    pub(crate) fn render_editor_body(
        &self,
        prefix: &'static str,
        id: ConnId,
        editor: &Editor,
        form: &EditorForm,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let t = |k: &str| self.app.t(k);
        let lang = &self.app.lang;
        let inputs = editor.inputs();
        let actions = editor.actions();
        // 语言下拉:v2.0 只有纯文本和 JSON,选择只影响 Format、Minify 能不能用
        let view_entity = cx.entity();
        let current = editor.format;
        let lang_owned = lang.to_string();
        let language = Button::new(SharedString::from(format!("{prefix}-language")))
            .compact()
            .label(current.label(lang))
            .icon(IconName::ChevronDown)
            .tooltip(t("view.language"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for f in Format::ALL {
                    menu = menu.item(
                        PopupMenuItem::new(f.label(&lang_owned))
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
            .id(SharedString::from(format!("{prefix}-toolbar")))
            .test_support()
            .gap_2()
            .items_center()
            .child(language)
            .child(
                Button::new(SharedString::from(format!("{prefix}-format")))
                    .compact()
                    .label(t("view.btn.format"))
                    .disabled(!actions.format)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::FormatJson))),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-minify")))
                    .compact()
                    .label(t("view.btn.minify"))
                    .disabled(!actions.minify)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::MinifyJson))),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-hex")))
                    .compact()
                    .label(t("view.btn.hex"))
                    .when(hex_on, |b| b.primary())
                    .disabled(!actions.hex)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::SetHex(!hex_on)))),
            );

        let notice = editor.notice(lang).map(|text| {
            div()
                .id(SharedString::from(format!("{prefix}-notice")))
                .test_support()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.warning.opacity(0.15))
                .text_sm()
                .child(text)
        });

        let key_field = inputs.key_name.then(|| {
            field(
                SharedString::from(format!("{prefix}-key")),
                t("view.key_name"),
                &form.key,
            )
        });
        let member_label = match editor.kind() {
            KeyType::List => t("view.index"),
            _ => t("view.member_name"),
        };
        let member_field = inputs.member_name.then(|| {
            field(
                SharedString::from(format!("{prefix}-member")),
                member_label,
                &form.member,
            )
        });
        let position = inputs.position.then(|| {
            let side = editor.position;
            h_flex()
                .id(SharedString::from(format!("{prefix}-position")))
                .test_support()
                .gap_2()
                .items_center()
                .child(div().text_sm().child(t("view.position")))
                .child(
                    Button::new(SharedString::from(format!("{prefix}-position-head")))
                        .compact()
                        .label(t("view.position.head"))
                        .when(side == Side::Head, |b| b.primary())
                        .on_click(send(cx, move || {
                            view_msg(id, ViewMsg::SetPosition(Side::Head))
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}-position-tail")))
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
                        .id(SharedString::from(format!("{prefix}-value")))
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
                            .id(SharedString::from(format!("{prefix}-loading")))
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
                Button::new(SharedString::from(format!("{prefix}-undo")))
                    .label(t("view.btn.undo"))
                    .disabled(!actions.undo)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::Undo))),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-cancel")))
                    .label(t("btn.cancel"))
                    .on_click(send(cx, move || view_msg(id, ViewMsg::CloseEditor))),
            )
            .child(
                Button::new(SharedString::from(format!("{prefix}-save")))
                    .primary()
                    .label(t("btn.save"))
                    .loading(editor.saving)
                    .disabled(!actions.save)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::Save))),
            );

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(key_field)
            .children(member_field)
            .children(position)
            .child(toolbar)
            .children(notice)
            .child(value)
            .child(footer)
    }
}

#[cfg(test)]
impl MainView {
    /// 编辑器三个输入控件里现在的文字:(key 名, 成员名, 值)(测试用)。
    pub(crate) fn member_form_texts(&self, id: ConnId, cx: &App) -> Option<(String, String)> {
        let f = &self.view_ui.get(&id)?.member.as_ref()?.1;
        Some((
            f.member.read(cx).value().to_string(),
            f.value.read(cx).value().to_string(),
        ))
    }

    pub(crate) fn form_texts(&self, id: ConnId, cx: &App) -> Option<(String, String, String)> {
        let f = self.view_ui.get(&id)?.page.as_ref()?;
        Some((
            f.key.read(cx).value().to_string(),
            f.member.read(cx).value().to_string(),
            f.value.read(cx).value().to_string(),
        ))
    }
}
