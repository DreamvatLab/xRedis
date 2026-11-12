//! 成员表页(P-22 至 P-26):Hash、List、Set、ZSet 的成员,和成员编辑器弹窗。
//! 按钮可用状态、状态栏文字、行的文字和二进制标记都读 `App`(`KeyView`、`MemberTable`),这里只画。
//!
//! 表格是 `uniform_list`(只画可见的行,同 Keys 页),整页替换 Keys 页(设计文档:成员表沿用整页)。
//! 过滤框对名字、值、分数一起过滤(两列都能过滤),点两个列头排序。

use std::ops::Range;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::keylist::SortDir;
use xredis_app::keyview::table::{MemberColumn, MemberRow, TableMsg};
use xredis_app::keyview::{KeyView, ViewMsg};
use xredis_core::config::ConnId;

use super::dialogs::new_input;
use super::keyview::{defer_dispatch, view_msg};
use super::send;
use crate::model::MainView;

const ROW_HEIGHT: f32 = 28.0;
const COL_CHECK: f32 = 28.0;

fn table_msg(id: ConnId, msg: TableMsg) -> Msg {
    view_msg(id, ViewMsg::Table(msg))
}

/// 成员表的搜索框、过滤框和滚动位置。内容的真相在 `App`,输入框每次改动立刻变成 `Msg`。
pub(crate) struct TableUi {
    search: Entity<InputState>,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

impl TableUi {
    pub(crate) fn new(
        main: &Entity<MainView>,
        id: ConnId,
        app: &xredis_app::App,
        window: &mut Window,
        cx: &mut Context<MainView>,
    ) -> Self {
        let (pattern, filter) = app
            .tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| v.table.as_ref())
            .map(|t| (t.pattern.clone(), t.filter.clone()))
            .unwrap_or_default();
        let search = new_input(
            window,
            cx,
            &pattern,
            app.t("view.members.search_placeholder"),
            false,
        );
        let filter_input = new_input(
            window,
            cx,
            &filter,
            app.t("view.members.filter_placeholder"),
            false,
        );
        let m1 = main.clone();
        let on_search = cx.subscribe_in(
            &search,
            window,
            move |_: &mut MainView, state, event: &InputEvent, window, cx| {
                let msg = match event {
                    InputEvent::Change => TableMsg::SetPattern(state.read(cx).value().to_string()),
                    InputEvent::PressEnter { .. } => TableMsg::Search,
                    _ => return,
                };
                defer_dispatch(&m1, table_msg(id, msg), window, cx);
            },
        );
        let m2 = main.clone();
        let on_filter = cx.subscribe_in(
            &filter_input,
            window,
            move |_: &mut MainView, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let msg = TableMsg::SetFilter(state.read(cx).value().to_string());
                    defer_dispatch(&m2, table_msg(id, msg), window, cx);
                }
            },
        );
        Self {
            search,
            filter: filter_input,
            scroll: UniformListScrollHandle::new(),
            _subs: vec![on_search, on_filter],
        }
    }
}

impl MainView {
    pub(crate) fn render_members_page(
        &self,
        id: ConnId,
        view: &KeyView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lang = &self.app.lang;
        let (Some(table), Some(actions), Some(status), Some(ui)) = (
            view.table.as_ref(),
            view.member_actions(),
            view.member_status(lang),
            self.view_ui.get(&id).and_then(|u| u.table.as_ref()),
        ) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let t = |k: &str| self.app.t(k);

        let header = self.render_view_header(id, view, cx).child(
            Button::new("keyview-close")
                .compact()
                .label(t("btn.close"))
                .on_click(send(cx, move || view_msg(id, ViewMsg::Close))),
        );

        let toolbar = h_flex()
            .id("members-toolbar")
            .test_support()
            .gap_2()
            .items_center()
            .child(
                div()
                    .id("members-search")
                    .test_support()
                    .flex_1()
                    .child(Input::new(&ui.search).cleanable(true)),
            )
            .child(
                Button::new("members-scan")
                    .primary()
                    .label(t("keys.scan"))
                    .disabled(!actions.search)
                    .on_click(send(cx, move || table_msg(id, TableMsg::Search))),
            )
            .child(
                Button::new("members-add")
                    .label(t("view.members.add"))
                    .disabled(!actions.add)
                    .on_click(send(cx, move || view_msg(id, ViewMsg::NewMember))),
            )
            .child(
                Button::new("members-delete")
                    .danger()
                    .label(t("btn.delete"))
                    .disabled(!actions.delete)
                    .on_click(send(cx, move || table_msg(id, TableMsg::DeleteSelected))),
            );

        // 表头:全选、两列(点列头排序)、过滤框
        let (title_a, title_b) = view.column_titles().unwrap_or(("", ""));
        let two_columns = view.sortable_columns().len() > 1;
        let sort = table.sort;
        let sort_cell =
            |column: MemberColumn, name: &'static str, title: String, cx: &mut Context<Self>| {
                let arrow = if sort.column == column {
                    match sort.dir {
                        SortDir::Asc => " ▲",
                        SortDir::Desc => " ▼",
                    }
                } else {
                    ""
                };
                div()
                    .id(SharedString::from(format!("members-sort-{name}")))
                    .test_support()
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{title}{arrow}"))
                    .on_click(send(cx, move || table_msg(id, TableMsg::SortBy(column))))
            };
        let all_view = cx.entity();
        let select_all = table.all_filtered_selected();
        let header_row = h_flex()
            .id("members-header")
            .test_support()
            .h(px(ROW_HEIGHT + 4.))
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().w(px(COL_CHECK)).flex_none().child(
                    Checkbox::new("members-select-all")
                        .checked(select_all)
                        .on_click(move |on: &bool, window, cx| {
                            let msg = table_msg(id, TableMsg::SelectAllFiltered(*on));
                            all_view.update(cx, |this, cx| this.dispatch(msg, window, cx));
                        }),
                ),
            )
            .child(div().flex_1().min_w_0().child(sort_cell(
                MemberColumn::Name,
                "name",
                t(title_a),
                cx,
            )))
            .when(two_columns, |d| {
                d.child(div().flex_1().min_w_0().child(sort_cell(
                    MemberColumn::Value,
                    "value",
                    t(title_b),
                    cx,
                )))
            })
            .child(
                div()
                    .id("members-filter")
                    .test_support()
                    .w(px(220.))
                    .flex_none()
                    .child(Input::new(&ui.filter).small().cleanable(true)),
            );

        let count = table.view_len();
        let list = uniform_list(
            "members-list",
            count,
            cx.processor(move |this, range: Range<usize>, _window, cx| {
                this.member_rows(id, range, cx)
            }),
        )
        .track_scroll(&ui.scroll)
        .size_full();

        let busy = table.is_scanning();
        let overlay = busy.then(|| {
            v_flex()
                .id("members-busy")
                .test_support()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(theme.background.opacity(0.7))
                .child(t("keys.scanning"))
                .when(actions.stop, |d| {
                    d.child(
                        Button::new("members-stop")
                            .danger()
                            .compact()
                            .label(t("keys.stop"))
                            .on_click(send(cx, move || table_msg(id, TableMsg::Stop))),
                    )
                })
        });
        let empty = (count == 0 && !busy).then(|| {
            div()
                .id("members-empty")
                .test_support()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(t("view.members.empty"))
        });
        let body = v_flex()
            .id("members-table")
            .test_support()
            .flex_1()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(header_row)
            .child(
                div()
                    .id("members-body")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .children(empty)
                    .children(overlay),
            );

        let cursor_tip = t("keys.more_tip");
        let footer = h_flex()
            .id("members-footer")
            .test_support()
            .gap_2()
            .items_center()
            .justify_end()
            .when_some(status.more_hint, |d, hint| {
                d.child(
                    div()
                        .id("members-more-hint")
                        .test_support()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(hint),
                )
            })
            .when_some(status.filtered, |d, text| {
                d.child(
                    div()
                        .id("members-filtered")
                        .test_support()
                        .text_sm()
                        .child(text),
                )
            })
            .when_some(status.percent, |d, p| {
                d.child(
                    div()
                        .id("members-percent")
                        .test_support()
                        .text_sm()
                        .child(p),
                )
            })
            .child(
                div()
                    .id("members-status")
                    .test_support()
                    .text_sm()
                    .tooltip(move |window, cx| Tooltip::new(cursor_tip.clone()).build(window, cx))
                    .child(status.loaded),
            )
            .child(
                Button::new("members-more")
                    .compact()
                    .label(t("keys.more"))
                    .disabled(!actions.more)
                    .on_click(send(cx, move || table_msg(id, TableMsg::LoadMore))),
            )
            .child(
                Button::new("members-all")
                    .compact()
                    .label(t("keys.all"))
                    .disabled(!actions.all)
                    .on_click(send(cx, move || table_msg(id, TableMsg::LoadAll))),
            );

        v_flex()
            .id("keyview-members")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(header)
            .child(toolbar)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    /// 虚拟滚动:只给可见范围的行。
    fn member_rows(
        &mut self,
        id: ConnId,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(table) = self
            .app
            .tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| v.table.as_ref())
        else {
            return Vec::new();
        };
        let two = !matches!(table.kind, xredis_core::keys::KeyType::Set);
        let rows: Vec<(usize, MemberRow)> = range
            .filter_map(|ix| table.view_row(ix).map(|r| (ix, r)))
            .collect();
        let theme = cx.theme().clone();
        rows.into_iter()
            .map(|(ix, row)| self.member_row(id, ix, row, two, &theme, cx))
            .collect()
    }

    fn member_row(
        &self,
        id: ConnId,
        ix: usize,
        row: MemberRow,
        two_columns: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let select_name = row.name.clone();
        let selected = row.selected;
        let cell = |suffix: &'static str, text: String, row: &MemberRow, cx: &mut Context<Self>| {
            let name = row.name.clone();
            div()
                .id(SharedString::from(format!("member-{suffix}-{ix}")))
                .test_support()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .cursor_pointer()
                .when(row.binary, |d| d.italic())
                .tooltip(super::text_tooltip(text.clone()))
                .on_click(send(cx, move || {
                    view_msg(id, ViewMsg::OpenMember(name.clone()))
                }))
                .child(text)
        };
        h_flex()
            .id(SharedString::from(format!("member-row-{ix}")))
            .test_support()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_2()
            .items_center()
            .when(selected, |d| d.bg(theme.list_active))
            .when(!selected, |d| d.hover(|d| d.bg(theme.list_hover)))
            .child(
                div().w(px(COL_CHECK)).flex_none().child(
                    Checkbox::new(SharedString::from(format!("member-check-{ix}")))
                        .checked(selected)
                        .on_click(move |on: &bool, window, cx| {
                            let msg = table_msg(
                                id,
                                TableMsg::Select {
                                    name: select_name.clone(),
                                    on: *on,
                                },
                            );
                            view.update(cx, |this, cx| this.dispatch(msg, window, cx));
                        }),
                ),
            )
            .child(cell("open", row.name_text.clone(), &row, cx))
            .when(two_columns, |d| {
                d.child(cell(
                    "value",
                    row.value_text.clone().unwrap_or_default(),
                    &row,
                    cx,
                ))
            })
            .into_any_element()
    }

    /// 成员编辑器弹窗的内容(弹窗里的 `MemberEditorBody` 调它)。
    pub(crate) fn render_member_dialog(&self, id: ConnId, cx: &mut Context<Self>) -> AnyElement {
        let Some((view, editor, form)) = self
            .app
            .tab(id)
            .and_then(|t| t.view.as_ref())
            .and_then(|v| Some((v, v.editor.as_ref()?)))
            .and_then(|(v, e)| {
                let form = &self.view_ui.get(&id)?.member.as_ref()?.1;
                Some((v, e, form))
            })
        else {
            return div().into_any_element();
        };
        let title = view.editor_title(&self.app.lang).unwrap_or_default();
        let body = self.render_editor_body("member", id, editor, form, cx);
        v_flex()
            .id("member-dialog")
            .test_support()
            .gap_3()
            .px_4()
            .h(px(480.))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(body)
            .into_any_element()
    }
}

/// 成员编辑器弹窗的视图:自己不存状态,内容由 `MainView` 画(控件在 `ViewUi.member`)。
pub(crate) struct MemberEditorBody {
    main: Entity<MainView>,
    id: ConnId,
    _sub: Subscription,
}

impl MemberEditorBody {
    pub(crate) fn new(main: Entity<MainView>, id: ConnId, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&main, |_, _, cx| cx.notify());
        Self {
            main,
            id,
            _sub: sub,
        }
    }
}

impl Render for MemberEditorBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (main, id) = (self.main.clone(), self.id);
        main.update(cx, |this, cx| this.render_member_dialog(id, cx))
    }
}
