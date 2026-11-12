//! Keys 子页(P-05、P-10 至 P-16):工具栏、虚拟滚动的表格、底部状态栏。
//! 只画 `App` 里 `KeyList` 的状态,用户操作变成 `Msg::Keys`。文字、可用状态、百分比都从 `App` 取。
//!
//! 表格是 GPUI 的 `uniform_list`(只画可见的行,组件库的 `Table` 内部也是它),不分页(P-13)。
//! 行高固定;滚动时把可见的 key 报告给 `App`(`KeysMsg::Visible`),要不要加载详情由 `App` 判断。

use std::collections::HashMap;
use std::ops::Range;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, ColorName, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::keylist::{Details, KeyRow, KeysMsg, SortColumn, SortDir, type_label};
use xredis_app::keyview::ViewMsg;
use xredis_core::config::ConnId;
use xredis_core::keys::KeyType;

use super::dialogs::new_input;
use super::{send, send_to};
use crate::model::MainView;

const ROW_HEIGHT: f32 = 28.0;
const COL_CHECK: f32 = 28.0;
const COL_TYPE: f32 = 70.0;
const COL_LENGTH: f32 = 80.0;
const COL_TTL: f32 = 80.0;
const COL_ACTIONS: f32 = 60.0;

/// 每个 Tab 一份界面本地的东西:输入框和滚动位置。内容(搜索条件、过滤词)的真相在 `App`,
/// 输入框的每次改动立刻变成 `Msg`。
pub(crate) struct KeysUi {
    search: Entity<InputState>,
    filter: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

pub(crate) type KeysUiMap = HashMap<ConnId, KeysUi>;

fn keys_msg(id: ConnId, msg: KeysMsg) -> Msg {
    Msg::Keys { id, msg }
}

/// 类型标签的颜色(P-15):String 绿、Hash 蓝、List 紫、Set 青、ZSet 橙、Stream 洋红,其他灰。
fn type_color(kind: Option<&KeyType>) -> ColorName {
    match kind {
        Some(KeyType::String) => ColorName::Green,
        Some(KeyType::Hash) => ColorName::Blue,
        Some(KeyType::List) => ColorName::Purple,
        Some(KeyType::Set) => ColorName::Cyan,
        Some(KeyType::ZSet) => ColorName::Orange,
        Some(KeyType::Stream) => ColorName::Fuchsia,
        _ => ColorName::Gray,
    }
}

impl MainView {
    /// 让每个 Tab 都有自己的输入框,关掉的 Tab 的释放掉。
    pub(crate) fn sync_keys_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<ConnId> = self.app.tabs.iter().map(|t| t.id).collect();
        self.keys_ui.retain(|id, _| ids.contains(id));
        for id in ids {
            if !self.keys_ui.contains_key(&id) {
                let ui = self.new_keys_ui(id, window, cx);
                self.keys_ui.insert(id, ui);
            }
        }
    }

    fn new_keys_ui(&mut self, id: ConnId, window: &mut Window, cx: &mut Context<Self>) -> KeysUi {
        let (pattern, filter) = self
            .app
            .tab(id)
            .map(|t| (t.keys.pattern.clone(), t.keys.filter.clone()))
            .unwrap_or_default();
        let search = new_input(
            window,
            cx,
            &pattern,
            self.app.t("keys.search_placeholder"),
            false,
        );
        let filter_input = new_input(
            window,
            cx,
            &filter,
            self.app.t("keys.filter_placeholder"),
            false,
        );
        // 搜索框:每次改动发 SetPattern,回车搜索。过滤框:每次改动发 SetFilter。
        let on_search = cx.subscribe_in(
            &search,
            window,
            move |this: &mut MainView, state, event: &InputEvent, window, cx| {
                let msg = match event {
                    InputEvent::Change => {
                        keys_msg(id, KeysMsg::SetPattern(state.read(cx).value().to_string()))
                    }
                    InputEvent::PressEnter { .. } => keys_msg(id, KeysMsg::Search),
                    _ => return,
                };
                this.dispatch(msg, window, cx);
            },
        );
        let on_filter = cx.subscribe_in(
            &filter_input,
            window,
            move |this: &mut MainView, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.dispatch(keys_msg(id, KeysMsg::SetFilter(text)), window, cx);
                }
            },
        );
        KeysUi {
            search,
            filter: filter_input,
            scroll: UniformListScrollHandle::new(),
            _subs: vec![on_search, on_filter],
        }
    }

    /// 把列表滚到第 `ix` 行(测试用;真实滚动由鼠标滚轮和滚动条完成)。
    #[cfg(test)]
    pub(crate) fn scroll_keys_to(&self, id: ConnId, ix: usize) {
        if let Some(ui) = self.keys_ui.get(&id) {
            ui.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
    }

    pub(crate) fn render_keys_page(&self, id: ConnId, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.app.tab(id) else {
            return div().into_any_element();
        };
        let page = v_flex()
            .id("keys-page")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2();
        // 打开了 key:值编辑器是 R-10 的内容,这里先放占位和关闭按钮
        if tab.view.is_some() {
            return page
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
                        .on_click(send(cx, move || Msg::View {
                            id,
                            msg: ViewMsg::Close,
                        })),
                )
                .into_any_element();
        }
        let Some(ui) = self.keys_ui.get(&id) else {
            return page.into_any_element();
        };
        page.child(self.render_keys_toolbar(id, ui, cx))
            .child(self.render_keys_table(id, ui, cx))
            .child(self.render_keys_footer(id, cx))
            .into_any_element()
    }

    fn render_keys_toolbar(&self, id: ConnId, ui: &KeysUi, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.app.tab(id) else {
            return div().into_any_element();
        };
        let actions = tab.keys.actions();
        let choices = self.app.db_choices(id);
        let current = choices
            .iter()
            .find(|c| c.selected)
            .map(|c| c.label.clone())
            .unwrap_or_default();
        let view = cx.entity();
        let db = Button::new("keys-db")
            .label(current)
            .icon(IconName::ChevronDown)
            .disabled(!actions.search)
            .tooltip(self.app.t("keys.db"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.max_h(px(360.)).scrollable(true);
                for c in &choices {
                    let (db, label) = (c.db, c.label.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(c.selected)
                            .on_click(send_to(&view, move || Msg::SelectDb { id, db })),
                    );
                }
                menu
            });
        h_flex()
            .id("keys-toolbar")
            .test_support()
            .gap_2()
            .items_center()
            .child(db)
            .child(
                div()
                    .id("keys-search")
                    .test_support()
                    .flex_1()
                    .child(Input::new(&ui.search).cleanable(true)),
            )
            .child(
                Button::new("keys-scan")
                    .primary()
                    .label(self.app.t("keys.scan"))
                    .disabled(!actions.search)
                    .on_click(send(cx, move || keys_msg(id, KeysMsg::Search))),
            )
            .into_any_element()
    }

    fn render_keys_table(&self, id: ConnId, ui: &KeysUi, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.app.tab(id) else {
            return div().into_any_element();
        };
        let keys = &tab.keys;
        let theme = cx.theme().clone();
        let sort = keys.sort;
        let header_cell =
            |col: SortColumn, text: String, width: Option<f32>, cx: &mut Context<Self>| {
                let arrow = if sort.column == col {
                    match sort.dir {
                        SortDir::Asc => " ▲",
                        SortDir::Desc => " ▼",
                    }
                } else {
                    ""
                };
                let name = match col {
                    SortColumn::Key => "key",
                    SortColumn::Type => "type",
                    SortColumn::Length => "length",
                    SortColumn::Ttl => "ttl",
                };
                div()
                    .id(SharedString::from(format!("keys-sort-{name}")))
                    .test_support()
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when_some(width, |d, w| d.w(px(w)).flex_none())
                    .child(format!("{text}{arrow}"))
                    .on_click(send(cx, move || keys_msg(id, KeysMsg::SortBy(col))))
            };
        let select_all = keys.all_filtered_selected();
        let all_view = cx.entity();
        let header = h_flex()
            .id("keys-header")
            .test_support()
            .h(px(ROW_HEIGHT + 4.))
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().w(px(COL_CHECK)).flex_none().child(
                    Checkbox::new("keys-select-all")
                        .checked(select_all)
                        .on_click(move |on: &bool, window, cx| {
                            let msg = keys_msg(id, KeysMsg::SelectAllFiltered(*on));
                            all_view.update(cx, |this, cx| this.dispatch(msg, window, cx));
                        }),
                ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(header_cell(
                        SortColumn::Key,
                        self.app.t("keys.col.key"),
                        None,
                        cx,
                    ))
                    .child(
                        div()
                            .id("keys-filter")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&ui.filter).small().cleanable(true)),
                    ),
            )
            .child(header_cell(
                SortColumn::Type,
                self.app.t("keys.col.type"),
                Some(COL_TYPE),
                cx,
            ))
            .child(header_cell(
                SortColumn::Length,
                self.app.t("keys.col.length"),
                Some(COL_LENGTH),
                cx,
            ))
            .child(header_cell(
                SortColumn::Ttl,
                self.app.t("keys.col.ttl"),
                Some(COL_TTL),
                cx,
            ))
            .child(
                div()
                    .w(px(COL_ACTIONS))
                    .flex_none()
                    .child(self.app.t("keys.col.actions")),
            );

        let count = keys.view_len();
        let list = uniform_list(
            "keys-list",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.key_rows(id, range, window, cx)
            }),
        )
        .track_scroll(&ui.scroll)
        .size_full();

        let busy = keys.is_busy();
        let overlay = busy.then(|| {
            let text = if keys.cancellable() {
                self.app.t("keys.scanning")
            } else {
                self.app.t("keys.working")
            };
            v_flex()
                .id("keys-busy")
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
                .child(text)
                .when(keys.actions().stop, |d| {
                    d.child(
                        Button::new("keys-stop")
                            .danger()
                            .compact()
                            .label(self.app.t("keys.stop"))
                            .on_click(send(cx, move || keys_msg(id, KeysMsg::Stop))),
                    )
                })
        });
        let empty = (count == 0 && !busy).then(|| {
            div()
                .id("keys-empty")
                .test_support()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(self.app.t("keys.empty"))
        });
        v_flex()
            .id("keys-table")
            .test_support()
            .flex_1()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(header)
            .child(
                div()
                    .id("keys-body")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .children(empty)
                    .children(overlay),
            )
            .into_any_element()
    }

    /// 虚拟滚动:只给可见范围的行。顺便把可见的 key 报给 `App`(它判断要不要加载详情)。
    fn key_rows(
        &mut self,
        id: ConnId,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(tab) = self.app.tab(id) else {
            return Vec::new();
        };
        let keys = &tab.keys;
        let rows: Vec<(usize, KeyRow, bool)> = range
            .filter_map(|ix| {
                keys.view_row(ix)
                    .map(|r| (ix, r.clone(), keys.is_selected(&r.key)))
            })
            .collect();
        let visible: Vec<_> = rows.iter().map(|(_, r, _)| r.key.clone()).collect();
        if keys.wants_details(&self.app.settings, &visible) {
            cx.defer_in(window, move |this, window, cx| {
                this.dispatch(keys_msg(id, KeysMsg::Visible(visible)), window, cx);
            });
        }
        let theme = cx.theme().clone();
        rows.into_iter()
            .map(|(ix, row, selected)| self.key_row(id, ix, row, selected, &theme, cx))
            .collect()
    }

    fn key_row(
        &self,
        id: ConnId,
        ix: usize,
        row: KeyRow,
        selected: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = row.key.clone();
        let deleted = row.is_deleted();
        let openable = row.openable();
        let placeholder = |details: &Details| match details {
            Details::NotLoaded => "-",
            Details::Queued | Details::Loading => "...",
            Details::Failed => "!",
            Details::Loaded(_) => "",
        };
        let cell = |text: Option<String>, w: f32, row: &KeyRow| {
            div()
                .w(px(w))
                .flex_none()
                .text_right()
                .text_color(if matches!(row.details, Details::Failed) {
                    theme.danger
                } else {
                    theme.foreground
                })
                .child(text.unwrap_or_else(|| placeholder(&row.details).to_string()))
        };
        let label = row.display.clone();
        let open_kind = row.kind().cloned();
        let open_key = key.clone();
        let refresh_key = key.clone();
        let select_key = key;
        let view = cx.entity();
        let type_tag = Tag::color(type_color(row.kind()))
            .small()
            .child(type_label(row.kind()));
        h_flex()
            .id(SharedString::from(format!("key-row-{ix}")))
            .test_support()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_2()
            .items_center()
            .when(selected, |d| d.bg(theme.list_active))
            .when(!selected, |d| d.hover(|d| d.bg(theme.list_hover)))
            .child(
                div().w(px(COL_CHECK)).flex_none().child(
                    Checkbox::new(SharedString::from(format!("key-check-{ix}")))
                        .checked(selected)
                        .on_click(move |on: &bool, window, cx| {
                            let msg = keys_msg(
                                id,
                                KeysMsg::Select {
                                    key: select_key.clone(),
                                    on: *on,
                                },
                            );
                            view.update(cx, |this, cx| this.dispatch(msg, window, cx));
                        }),
                ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("key-open-{ix}")))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when(deleted, |d| {
                        d.line_through().text_color(theme.muted_foreground)
                    })
                    .when(row.binary, |d| d.italic())
                    .when(openable, |d| {
                        d.cursor_pointer().on_click(send(cx, move || Msg::View {
                            id,
                            msg: ViewMsg::Open {
                                key: open_key.clone(),
                                kind: open_kind.clone().unwrap_or(KeyType::None),
                            },
                        }))
                    })
                    .child(label),
            )
            .child(div().w(px(COL_TYPE)).flex_none().child(type_tag))
            .child(cell(row.length_text(), COL_LENGTH, &row))
            .child(cell(row.ttl_text(), COL_TTL, &row))
            .child(
                div().w(px(COL_ACTIONS)).flex_none().child(
                    Button::new(SharedString::from(format!("key-refresh-{ix}")))
                        .ghost()
                        .compact()
                        .icon(IconName::RefreshCw)
                        .tooltip(self.app.t("keys.refresh_row"))
                        .loading(matches!(row.details, Details::Loading))
                        .on_click(send(cx, move || {
                            keys_msg(id, KeysMsg::RefreshDetails(refresh_key.clone()))
                        })),
                ),
            )
            .into_any_element()
    }

    fn render_keys_footer(&self, id: ConnId, cx: &mut Context<Self>) -> AnyElement {
        let (Some(tab), Some(status)) = (self.app.tab(id), self.app.keys_status(id)) else {
            return div().into_any_element();
        };
        let actions = tab.keys.actions();
        let theme = cx.theme().clone();
        let cursor = status.cursor.clone();
        h_flex()
            .id("keys-footer")
            .test_support()
            .gap_2()
            .items_center()
            .justify_end()
            .when_some(status.more_hint, |d, hint| {
                d.child(
                    div()
                        .id("keys-more-hint")
                        .test_support()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(hint),
                )
            })
            .when_some(status.filtered, |d, text| {
                d.child(
                    div()
                        .id("keys-filtered")
                        .test_support()
                        .text_sm()
                        .child(text),
                )
            })
            .when_some(status.percent, |d, p| {
                d.child(div().id("keys-percent").test_support().text_sm().child(p))
            })
            .child(
                div()
                    .id("keys-status")
                    .test_support()
                    .text_sm()
                    .tooltip(move |window, cx| Tooltip::new(cursor.clone()).build(window, cx))
                    .child(status.loaded),
            )
            .child(
                Button::new("keys-more")
                    .compact()
                    .label(self.app.t("keys.more"))
                    .tooltip(self.app.t("keys.more_tip"))
                    .disabled(!actions.more)
                    .on_click(send(cx, move || keys_msg(id, KeysMsg::LoadMore))),
            )
            .child(
                Button::new("keys-all")
                    .compact()
                    .label(self.app.t("keys.all"))
                    .tooltip(self.app.t("keys.all_tip"))
                    .disabled(!actions.all)
                    .on_click(send(cx, move || keys_msg(id, KeysMsg::LoadAll))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::ColorName;
    use xredis_core::keys::KeyType;

    use super::type_color;

    /// P-15:六种类型六种颜色,其他(没加载、None、模块类型)是灰。
    #[test]
    fn p15_each_type_has_its_own_color() {
        let kinds = [
            KeyType::String,
            KeyType::Hash,
            KeyType::List,
            KeyType::Set,
            KeyType::ZSet,
            KeyType::Stream,
        ];
        let colors: Vec<_> = kinds.iter().map(|k| type_color(Some(k)) as u8).collect();
        let mut unique = colors.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 6, "{colors:?}");
        let gray = ColorName::Gray as u8;
        assert!(!colors.contains(&gray));
        for other in [
            None,
            Some(&KeyType::None),
            Some(&KeyType::Other("ReJSON".into())),
        ] {
            assert_eq!(type_color(other) as u8, gray);
        }
        assert_eq!(
            type_color(Some(&KeyType::String)) as u8,
            ColorName::Green as u8
        );
        assert_eq!(
            type_color(Some(&KeyType::Stream)) as u8,
            ColorName::Fuchsia as u8
        );
    }
}
