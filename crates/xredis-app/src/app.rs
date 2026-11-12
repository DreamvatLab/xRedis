//! `App`:界面状态加 `update`。同步、纯逻辑,单测的主要对象。
//!
//! 约定:
//! - 所有会被新请求覆盖或被关闭作废的异步结果都带**纪元**,对不上就丢掉(第 10 节:关 Tab 后
//!   不能再为这个 Tab 发请求,结果回来也要丢弃)。
//! - **连接还没就绪(`Ready`)之前不允许发任何键操作**(L-02):[`App::can_request`]。
//! - 协议没同意之前(或启动结果还没回来),用户操作一律忽略,只有同意、拒绝和纯查看类操作可用(P-62)。

use std::collections::HashMap;
use std::sync::Arc;

use xredis_core::config::{ConnId, ConnectionConfig, Settings, now_ms};
use xredis_core::conn::info::ConnectionInfo;
use xredis_core::consts::WINDOW_MIN_HEIGHT;
use xredis_core::consts::WINDOW_MIN_WIDTH;
use xredis_core::{Error, Result};

use crate::console::{Console, ConsoleMsg};
use crate::errors::{describe, describe_import_warning, describe_startup_warning};
use crate::forms::{ConnectionEditor, SettingsEditor};
use crate::i18n::{export_template, resolve_language, tr, tr_with};
use crate::keylist::{Ctx as KeysCtx, KeyList, KeyStatus, KeysMsg};
use crate::keyview::{KeyView, ListEffect, ViewMsg};
use crate::messages::{Level, MessageBar};
use crate::msg::{BackendCmd, Cmd, Epoch, Msg, PlatformCmd, Saved, StartupData, SubPage};
use crate::pubsub_view::{PubSubMsg, PubSubView};

/// 免责协议的版本。同意的版本等于它才算同意过(v1 同);协议改版时改这个值,用户会被重新询问。
pub const AGREEMENT_VERSION: &str = "1.0.0";

/// 窗口尺寸保存的防抖时间(P-61)。
pub const WINDOW_SAVE_DEBOUNCE_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabStatus {
    /// 正在建连、取连接信息。
    Connecting,
    /// 就绪:可以发键操作。
    Ready(ConnectionInfo),
    /// 正在切库(连接会被关掉重建)。`info` 是切之前的,切失败时恢复。
    Switching { info: ConnectionInfo, db: u32 },
    /// 连接失败,带翻译好的原因。
    Failed(String),
}

#[derive(Debug)]
pub struct Tab {
    pub id: ConnId,
    pub status: TabStatus,
    /// 当前这次请求的纪元。
    pub epoch: Epoch,
    pub sub_page: SubPage,
    /// Keys 子页的状态(R-07a)。
    pub keys: KeyList,
    /// 打开的 key(值编辑器、成员表、新建 key,R-07b),没有就是 `None`。
    pub view: Option<KeyView>,
    /// 控制台页的状态(R-07c)。
    pub console: Console,
    /// Pub/Sub 页的状态(R-07c)。
    pub pubsub: PubSubView,
    /// 视图代号的计数器,只增不减:换了视图,旧视图的迟到结果对不上就丢弃。
    view_seq: u64,
}

/// 库下拉里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbChoice {
    pub db: u32,
    pub keys: i64,
    pub selected: bool,
    /// 翻译好的标签,如 `db3 (5 keys)`。
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    DeleteConnection { id: ConnId, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorDialog {
    pub title: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

/// 发起设置保存时记下的用途,结果回来时按它处理。
#[derive(Debug)]
enum SaveIntent {
    /// 设置面板:成功后才把草稿变成正式设置、关面板。
    Editor(Settings),
    /// 折叠侧栏、同意协议这类:界面状态已经改了,保存只是持久化。
    Silent,
    /// 点了“拒绝”:保存完(不管成败)再退出,不能让退出把写入截断。
    Decline,
}

pub struct App {
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub started: bool,
    /// 当前界面语言(已经解析过的,一定是可用的)。
    pub lang: String,
    pub settings: Settings,
    /// 按 ordinal 升序,同序号更新时间新的在前(P-03)。
    pub connections: Vec<ConnectionConfig>,
    pub tabs: Vec<Tab>,
    pub active: Option<ConnId>,
    pub editor: Option<ConnectionEditor>,
    pub settings_editor: Option<SettingsEditor>,
    pub confirm: Option<Confirm>,
    pub error_dialog: Option<ErrorDialog>,
    pub messages: MessageBar,
    pub show_agreement: bool,
    pub show_privacy: bool,
    pub window: WindowState,
    epoch_counter: Epoch,
    window_gen: u64,
    save_token: u64,
    saves: HashMap<u64, SaveIntent>,
    degraded_notified: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// 排序规则(P-03):ordinal 升序,同序号时更新时间新的在前。
pub fn sort_connections(list: &mut [ConnectionConfig]) {
    list.sort_by(|a, b| {
        a.ordinal
            .cmp(&b.ordinal)
            .then(b.updated_on_utc.cmp(&a.updated_on_utc))
    });
}

/// 连接在界面上的名字:有名字用名字,否则 `host:port`。
pub fn display_name(c: &ConnectionConfig) -> String {
    if c.name.trim().is_empty() {
        format!("{}:{}", c.host, c.port)
    } else {
        c.name.clone()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(now_ms))
    }

    /// 测试里用固定的时钟。
    pub fn with_clock(clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let settings = Settings::default();
        Self {
            clock,
            started: false,
            lang: settings.language.clone(),
            window: WindowState {
                width: settings.window_width,
                height: settings.window_height,
                maximized: settings.is_maximized,
            },
            settings,
            connections: Vec::new(),
            tabs: Vec::new(),
            active: None,
            editor: None,
            settings_editor: None,
            confirm: None,
            error_dialog: None,
            messages: MessageBar::default(),
            show_agreement: false,
            show_privacy: false,
            epoch_counter: 0,
            window_gen: 0,
            save_token: 0,
            saves: HashMap::new(),
            degraded_notified: false,
        }
    }

    /// 启动:发出加载一切的命令。
    pub fn start(&mut self) -> Vec<Cmd> {
        vec![Cmd::Backend(BackendCmd::Startup)]
    }

    // ---- 查询 ----

    pub fn t(&self, key: &str) -> String {
        tr(&self.lang, key)
    }

    pub fn t_with(&self, key: &str, args: &[(&str, &str)]) -> String {
        tr_with(&self.lang, key, args)
    }

    /// 免责协议的段落:`(标题, 正文)`,已按界面语言翻译。最后一段是签名,没有标题。
    pub fn agreement_sections(&self) -> Vec<(String, String)> {
        (0..=6)
            .map(|i| {
                (
                    self.t(&format!("disclaimer.p{i}_title")),
                    self.t(&format!("disclaimer.p{i}")),
                )
            })
            .chain([(String::new(), self.t("disclaimer.signature"))])
            .collect()
    }

    /// 隐私政策的段落:`(标题, 正文)`,已按界面语言翻译。
    pub fn privacy_sections(&self) -> Vec<(String, String)> {
        ["intro", "data", "storage", "security", "updates", "contact"]
            .iter()
            .map(|k| {
                (
                    self.t(&format!("privacy.{k}_title")),
                    self.t(&format!("privacy.{k}")),
                )
            })
            .collect()
    }

    pub fn agreed(&self) -> bool {
        self.settings.agreement == AGREEMENT_VERSION
    }

    /// 界面被协议弹窗挡住(或启动还没完成):用户操作都不生效。
    pub fn blocked(&self) -> bool {
        !self.started || (self.show_agreement && !self.agreed())
    }

    pub fn tab(&self, id: ConnId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    /// 这个 Tab 现在能不能发键操作:只有就绪的才行。连接中、切库中、失败的、已经关掉的都不行(L-02)。
    pub fn can_request(&self, id: ConnId) -> bool {
        matches!(self.tab(id).map(|t| &t.status), Some(TabStatus::Ready(_)))
    }

    pub fn connection(&self, id: ConnId) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// 连接在界面上的名字(侧栏、Tab 标题):有名字用名字,否则 `host:port`。连接不存在时是空串。
    pub fn connection_name(&self, id: ConnId) -> String {
        self.connection(id).map(display_name).unwrap_or_default()
    }

    /// 侧栏第二行:有名字时是 `host:port`;没名字时显示名本身就是 `host:port`,不重复,返回 `None`。
    pub fn connection_subtitle(&self, id: ConnId) -> Option<String> {
        let c = self.connection(id)?;
        (!c.name.trim().is_empty()).then(|| format!("{}:{}", c.host, c.port))
    }

    /// 库下拉的选项(P-05):每个库一项,带 key 数和标签;连接没就绪时是空的。
    pub fn db_choices(&self, id: ConnId) -> Vec<DbChoice> {
        let Some(TabStatus::Ready(info)) = self.tab(id).map(|t| &t.status) else {
            return Vec::new();
        };
        let selected = self.connection(id).map_or(0, |c| c.selected_db);
        info.databases
            .iter()
            .map(|d| DbChoice {
                db: d.db,
                keys: d.keys,
                selected: d.db == selected,
                label: self.t_with(
                    "db.item",
                    &[("index", &d.db.to_string()), ("keys", &d.keys.to_string())],
                ),
            })
            .collect()
    }

    /// Keys 页底部状态栏的文字(P-13);Tab 没就绪时是 `None`。
    pub fn keys_status(&self, id: ConnId) -> Option<KeyStatus> {
        let tab = self.tab(id)?;
        let TabStatus::Ready(info) = &tab.status else {
            return None;
        };
        let selected = self.connection(id).map_or(0, |c| c.selected_db);
        let info_total = info
            .databases
            .iter()
            .find(|d| d.db == selected)
            .and_then(|d| u64::try_from(d.keys).ok());
        Some(tab.keys.status(&self.lang, info_total))
    }

    /// 控制台等待动画的当前一帧(P-50):命令执行超过 500 ms 才有。时钟在 `App` 里,GUI 只管画;
    /// 重画的时机是 `App` 排的 `ConsoleMsg::Tick`。
    pub fn console_waiting_frame(&self, id: ConnId) -> Option<char> {
        self.tab(id)?.console.waiting_frame(self.now())
    }

    // ---- 内部工具 ----

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn note(&mut self, level: Level, text: String, toast: bool) {
        let at = self.now();
        self.messages.push(level, text, toast, at);
    }

    fn describe(&self, err: &Error) -> String {
        describe(&self.lang, err)
    }

    fn next_epoch(&mut self) -> Epoch {
        self.epoch_counter += 1;
        self.epoch_counter
    }

    fn save_settings_cmd(&mut self, settings: Settings, intent: SaveIntent) -> Cmd {
        self.save_token += 1;
        let token = self.save_token;
        self.saves.insert(token, intent);
        Cmd::Backend(BackendCmd::SaveSettings { settings, token })
    }

    /// 用户主动的操作:被协议弹窗挡住时忽略。
    fn is_user_action(msg: &Msg) -> bool {
        matches!(
            msg,
            Msg::ToggleSider
                | Msg::OpenConnection(_)
                | Msg::CloseTab(_)
                | Msg::ActivateTab(_)
                | Msg::SetSubPage(..)
                | Msg::Reconnect(_)
                | Msg::SelectDb { .. }
                | Msg::OpenEditor(_)
                | Msg::EditorSetText(..)
                | Msg::EditorSetTls(_)
                | Msg::EditorClearPassword(_)
                | Msg::EditorPickFile(_)
                | Msg::EditorFilePicked(..)
                | Msg::EditorClearFile(_)
                | Msg::EditorSave
                | Msg::EditorCancel
                | Msg::DeleteRequested(_)
                | Msg::ConfirmAccepted
                | Msg::ConfirmCancelled
                | Msg::OpenSettings
                | Msg::SettingsSetText(..)
                | Msg::SettingsSetShowDetails(_)
                | Msg::SettingsSetLanguage(_)
                | Msg::SettingsSave
                | Msg::SettingsCancel
                | Msg::ExportLanguageTemplate
        ) || matches!(msg, Msg::Keys { msg, .. } if !msg.is_result())
            || matches!(msg, Msg::View { msg, .. } if !msg.is_result())
            || matches!(msg, Msg::Console { msg, .. } if !msg.is_result())
            || matches!(msg, Msg::PubSub { msg, .. } if !msg.is_result())
    }

    // ---- update ----

    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        if self.blocked() && Self::is_user_action(&msg) {
            return Vec::new();
        }
        match msg {
            Msg::StartupDone(data) => self.on_startup(*data),

            // ---- 连接列表和 Tab ----
            Msg::ToggleSider => {
                self.settings.sider_menu_collapsed = !self.settings.sider_menu_collapsed;
                let s = self.settings.clone();
                vec![self.save_settings_cmd(s, SaveIntent::Silent)]
            }
            Msg::OpenConnection(id) => self.open_connection(id),
            Msg::CloseTab(id) => self.close_tab(id),
            Msg::ActivateTab(id) => {
                if self.tab(id).is_some() {
                    self.active = Some(id);
                }
                Vec::new()
            }
            Msg::SetSubPage(id, page) => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    t.sub_page = page;
                }
                Vec::new()
            }
            Msg::Reconnect(id) => self.reconnect(id),
            Msg::SelectDb { id, db } => self.select_db(id, db),
            Msg::Connected { id, epoch, result } => self.on_connected(id, epoch, result),
            Msg::DbSelected {
                id,
                epoch,
                db,
                result,
            } => self.on_db_selected(id, epoch, db, result),
            Msg::ConnectionsLoaded(mut list) => {
                sort_connections(&mut list);
                self.connections = list;
                Vec::new()
            }

            // ---- 连接编辑器 ----
            Msg::OpenEditor(target) => {
                self.editor = match target {
                    None => Some(ConnectionEditor::new_connection()),
                    Some(id) => self.connection(id).map(ConnectionEditor::edit),
                };
                Vec::new()
            }
            Msg::EditorSetText(field, value) => {
                if let Some(e) = &mut self.editor {
                    e.set_text(field, value);
                }
                Vec::new()
            }
            Msg::EditorSetTls(on) => {
                if let Some(e) = &mut self.editor {
                    e.tls = on;
                }
                Vec::new()
            }
            Msg::EditorClearPassword(on) => {
                if let Some(e) = &mut self.editor {
                    e.clear_password = on;
                }
                Vec::new()
            }
            Msg::EditorPickFile(field) => {
                if self.editor.is_some() {
                    vec![Cmd::Platform(PlatformCmd::PickFile(field))]
                } else {
                    Vec::new()
                }
            }
            Msg::EditorFilePicked(field, path) => {
                if let (Some(e), Some(p)) = (&mut self.editor, path) {
                    e.set_file(field, Some(p));
                }
                Vec::new()
            }
            Msg::EditorClearFile(field) => {
                if let Some(e) = &mut self.editor {
                    e.set_file(field, None);
                }
                Vec::new()
            }
            Msg::EditorSave => self.editor_save(),
            Msg::EditorCancel => {
                self.editor = None;
                Vec::new()
            }
            Msg::ConnectionSaved(result) => self.on_connection_saved(result),

            // ---- 删除连接 ----
            Msg::DeleteRequested(id) => {
                if let Some(c) = self.connection(id) {
                    self.confirm = Some(Confirm::DeleteConnection {
                        id,
                        name: display_name(c),
                    });
                }
                Vec::new()
            }
            Msg::ConfirmAccepted => match self.confirm.take() {
                Some(Confirm::DeleteConnection { id, .. }) => {
                    // 先移除 Tab 再删(关闭连接在 CloseTab 里发出),不能反过来等
                    let mut cmds = self.close_tab(id);
                    cmds.push(Cmd::Backend(BackendCmd::DeleteConnection(id)));
                    cmds
                }
                None => Vec::new(),
            },
            Msg::ConfirmCancelled => {
                self.confirm = None;
                Vec::new()
            }
            Msg::ConnectionDeleted { result, .. } => match result {
                Ok(()) => {
                    let text = self.t("msg.connection_deleted");
                    self.note(Level::Success, text, true);
                    vec![Cmd::Backend(BackendCmd::LoadConnections)]
                }
                Err(err) => {
                    let text = self.describe(&err);
                    self.note(Level::Error, text, true);
                    // 删失败了,列表要以后端为准
                    vec![Cmd::Backend(BackendCmd::LoadConnections)]
                }
            },

            // ---- 设置 ----
            Msg::OpenSettings => {
                self.settings_editor = Some(SettingsEditor::from_settings(&self.settings));
                Vec::new()
            }
            Msg::SettingsSetText(field, value) => {
                if let Some(e) = &mut self.settings_editor {
                    e.set_text(field, value);
                }
                Vec::new()
            }
            Msg::SettingsSetShowDetails(on) => {
                if let Some(e) = &mut self.settings_editor {
                    e.show_key_details = on;
                }
                Vec::new()
            }
            Msg::SettingsSetLanguage(lang) => {
                if let Some(e) = &mut self.settings_editor {
                    e.language = lang;
                }
                Vec::new()
            }
            Msg::SettingsSave => self.settings_save(),
            Msg::SettingsCancel => {
                self.settings_editor = None;
                Vec::new()
            }
            Msg::SettingsSaved { token, result } => self.on_settings_saved(token, result),
            Msg::ExportLanguageTemplate => {
                let target = self
                    .settings_editor
                    .as_ref()
                    .map(|e| e.language.as_str())
                    .filter(|l| *l != xredis_core::consts::DEFAULT_LANGUAGE)
                    .unwrap_or("xx")
                    .to_string();
                vec![Cmd::Platform(PlatformCmd::SaveTextFile {
                    suggested_name: format!("{target}.yml"),
                    contents: export_template(&target),
                })]
            }
            Msg::TemplateSaved(result) => {
                match result {
                    Ok(Some(path)) => {
                        let text = self.t_with("settings.template_saved", &[("path", &path)]);
                        self.note(Level::Success, text, true);
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        let text = self.t_with("settings.template_failed", &[("reason", &reason)]);
                        self.note(Level::Error, text, true);
                    }
                }
                Vec::new()
            }

            // ---- 免责协议、隐私政策、Help ----
            Msg::AgreeDisclaimer => {
                self.settings.agreement = AGREEMENT_VERSION.to_string();
                self.show_agreement = false;
                let s = self.settings.clone();
                vec![self.save_settings_cmd(s, SaveIntent::Silent)]
            }
            Msg::DeclineDisclaimer => {
                self.settings.agreement = String::new();
                let s = self.settings.clone();
                vec![self.save_settings_cmd(s, SaveIntent::Decline)]
            }
            Msg::ShowAgreement(on) => {
                self.show_agreement = on;
                Vec::new()
            }
            Msg::ShowPrivacy(on) => {
                self.show_privacy = on;
                Vec::new()
            }
            Msg::OpenHelpLink(link) => {
                vec![Cmd::Platform(PlatformCmd::OpenUrl(link.url().to_string()))]
            }

            // ---- 消息栏和错误框 ----
            Msg::ToggleMessageLog => {
                self.messages.open = !self.messages.open;
                Vec::new()
            }
            Msg::OpenMessageLog => {
                self.error_dialog = None;
                self.messages.open = true;
                Vec::new()
            }
            Msg::ClearMessages => {
                self.messages.clear();
                Vec::new()
            }
            Msg::DismissToast(id) => {
                self.messages.dismiss_toast(id);
                Vec::new()
            }
            Msg::DismissError => {
                self.error_dialog = None;
                Vec::new()
            }

            // ---- Keys 子页 ----
            Msg::Keys { id, msg } => self.on_keys(id, msg),
            Msg::View { id, msg } => self.on_view(id, msg),
            Msg::Console { id, msg } => self.on_console(id, msg),
            Msg::PubSub { id, msg } => self.on_pubsub(id, msg),

            // ---- 窗口 ----
            Msg::WindowChanged {
                width,
                height,
                maximized,
            } => {
                let next = WindowState {
                    width,
                    height,
                    maximized,
                };
                if next == self.window {
                    return Vec::new();
                }
                self.window = next;
                self.window_gen += 1;
                vec![Cmd::Platform(PlatformCmd::After {
                    ms: WINDOW_SAVE_DEBOUNCE_MS,
                    msg: Box::new(Msg::WindowSaveDue(self.window_gen)),
                })]
            }
            Msg::WindowSaveDue(gen_) => {
                if gen_ != self.window_gen {
                    return Vec::new(); // 之后又变过了,等最新的那次
                }
                vec![Cmd::Backend(BackendCmd::SaveWindowSize {
                    width: self.window.width,
                    height: self.window.height,
                    maximized: self.window.maximized,
                })]
            }
            Msg::WindowSaved(result) => {
                if let Err(err) = result {
                    let reason = self.describe(&err);
                    let text = self.t_with("msg.window_save_failed", &[("reason", &reason)]);
                    self.note(Level::Warn, text, false);
                }
                Vec::new()
            }
        }
    }

    // ---- 启动 ----

    fn on_startup(&mut self, data: StartupData) -> Vec<Cmd> {
        self.started = true;
        self.settings = data.settings;
        let mut conns = data.connections;
        sort_connections(&mut conns);
        self.connections = conns;
        self.window = WindowState {
            width: self.settings.window_width,
            height: self.settings.window_height,
            maximized: self.settings.is_maximized,
        };
        let wanted = self.settings.language.clone();
        self.lang = resolve_language(&wanted);
        self.show_agreement = !self.agreed();

        let cmds = vec![Cmd::Platform(PlatformCmd::SetLocale(self.lang.clone()))];
        if self.lang != wanted {
            let text = self.t_with("msg.language_unavailable", &[("language", &wanted)]);
            self.note(Level::Warn, text, true);
        }
        // 配置的问题(坏文件已备份、目录不可用):提示,用默认配置继续,不崩溃(P-66)
        for w in data.warnings {
            let text = describe_startup_warning(&self.lang, &w);
            self.note(Level::Warn, text, true);
        }
        if let Some(import) = data.import {
            match import.error {
                Some(reason) => {
                    let text = self.t_with("msg.import_failed", &[("reason", &reason)]);
                    self.note(Level::Error, text, true);
                }
                None => {
                    let text = self.t_with(
                        "msg.import_done",
                        &[("count", &import.connections.to_string())],
                    );
                    self.note(Level::Success, text, true);
                    for w in import.warnings {
                        let text = describe_import_warning(&self.lang, &w);
                        self.note(Level::Warn, text, true);
                    }
                }
            }
        }
        for p in data.packs.loaded {
            let text = self.t_with(
                "msg.pack_loaded",
                &[("file", &p.file), ("languages", &p.languages.join(", "))],
            );
            self.note(Level::Info, text, false);
        }
        for e in data.packs.errors {
            let text = self.t_with(
                "msg.pack_error",
                &[("file", &e.file), ("reason", &e.reason)],
            );
            self.note(Level::Warn, text, true);
        }
        if data.keystore_degraded {
            self.notify_keystore_degraded();
        }
        cmds
    }

    /// 系统密钥存储不可用:只提示一次(第 6 节)。
    fn notify_keystore_degraded(&mut self) {
        if !self.degraded_notified {
            self.degraded_notified = true;
            let text = self.t("msg.keystore_degraded");
            self.note(Level::Warn, text, true);
        }
    }

    // ---- Tab ----

    fn open_connection(&mut self, id: ConnId) -> Vec<Cmd> {
        if self.connection(id).is_none() {
            return Vec::new();
        }
        if self.tab(id).is_some() {
            self.active = Some(id);
            return Vec::new();
        }
        let epoch = self.next_epoch();
        self.tabs.push(Tab {
            id,
            status: TabStatus::Connecting,
            epoch,
            sub_page: SubPage::Keys,
            keys: KeyList::new(),
            view: None,
            view_seq: 0,
            console: Console::new(),
            pubsub: PubSubView::new(),
        });
        self.active = Some(id);
        vec![Cmd::Backend(BackendCmd::Connect { id, epoch })]
    }

    fn reconnect(&mut self, id: ConnId) -> Vec<Cmd> {
        if !matches!(self.tab(id).map(|t| &t.status), Some(TabStatus::Failed(_))) {
            return Vec::new();
        }
        let epoch = self.next_epoch();
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.status = TabStatus::Connecting;
            t.epoch = epoch;
        }
        vec![Cmd::Backend(BackendCmd::Connect { id, epoch })]
    }

    /// 关 Tab:**先移除 Tab,后台关连接**。关连接可能要等(给一个已经断开的连接发 QUIT),
    /// Tab 不能因此点了没反应(P-04)。关掉之后迟到的结果因为找不到 Tab 或纪元对不上被丢弃。
    fn close_tab(&mut self, id: ConnId) -> Vec<Cmd> {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        self.tabs.remove(pos);
        if self.active == Some(id) {
            self.active = self.tabs.first().map(|t| t.id);
        }
        vec![Cmd::Backend(BackendCmd::CloseConnection(id))]
    }

    fn select_db(&mut self, id: ConnId, db: u32) -> Vec<Cmd> {
        let ready = match self.tab(id).map(|t| &t.status) {
            Some(TabStatus::Ready(info)) if (db as usize) < info.databases.len() => info.clone(),
            _ => return Vec::new(), // 没就绪不能切库;库号不在列表里也不切
        };
        let epoch = self.next_epoch();
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.status = TabStatus::Switching { info: ready, db };
            tab.epoch = epoch;
        }
        vec![Cmd::Backend(BackendCmd::SelectDb { id, epoch, db })]
    }

    fn on_connected(
        &mut self,
        id: ConnId,
        epoch: Epoch,
        result: Result<ConnectionInfo>,
    ) -> Vec<Cmd> {
        // Tab 已经关了(或又发起过新的请求):迟到的结果丢弃
        let Some(idx) = self
            .tabs
            .iter()
            .position(|t| t.id == id && t.epoch == epoch)
        else {
            return Vec::new();
        };
        match result {
            Ok(info) => {
                self.tabs[idx].status = TabStatus::Ready(info);
                // 第一次就绪、重连、切库之后:列表从头来(旧请求的结果因扫描代号对不上被丢弃),
                // 然后自动搜索一次。没就绪之前一个 scan 都不发(L-02)
                self.tabs[idx].keys.reset();
                // 重连或切库之后,打开着的 key 不一定还在:关掉
                self.tabs[idx].view = None;
                // 控制台在执行的命令和 Pub/Sub 的订阅都随旧连接结束,迟到的结果作废
                self.tabs[idx].console.reset_connection();
                let now = self.now();
                self.tabs[idx].pubsub.reset_connection(now, &self.lang);
                return self.on_keys(id, KeysMsg::Search);
            }
            Err(err) => {
                let message = self.describe(&err);
                self.tabs[idx].status = TabStatus::Failed(message.clone());
                self.error_dialog = Some(ErrorDialog {
                    title: self.t("tab.failed"),
                    message: message.clone(),
                });
                self.note(Level::Error, message, false);
            }
        }
        Vec::new()
    }

    /// Keys 子页的消息。用户操作要求 Tab 就绪(L-02);异步结果在 Tab 已经关掉时丢弃(第 10 节)。
    fn on_keys(&mut self, id: ConnId, msg: KeysMsg) -> Vec<Cmd> {
        if !msg.is_result() && !self.can_request(id) {
            return Vec::new();
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let now = self.now();
        let ctx = KeysCtx {
            id,
            lang: &self.lang,
            settings: &self.settings,
            now,
        };
        let out = self.tabs[idx].keys.update(&ctx, msg);
        for n in out.notes {
            self.note(n.level, n.text, n.toast);
        }
        out.cmds
    }

    /// 控制台(R-07c)。用户操作要等 Tab 就绪;结果不挡,在 `Console` 里按连接纪元丢弃迟到的。
    fn on_console(&mut self, id: ConnId, msg: ConsoleMsg) -> Vec<Cmd> {
        if !msg.is_result() && !self.can_request(id) {
            return Vec::new();
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let now = self.now();
        let ctx = KeysCtx {
            id,
            lang: &self.lang,
            settings: &self.settings,
            now,
        };
        self.tabs[idx].console.update(&ctx, msg).cmds
    }

    /// Pub/Sub(R-07c)。同上。收到的消息(`Incoming`)只在订阅着的时候显示。
    fn on_pubsub(&mut self, id: ConnId, msg: PubSubMsg) -> Vec<Cmd> {
        if !msg.is_result() && !self.can_request(id) {
            return Vec::new();
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let now = self.now();
        let ctx = KeysCtx {
            id,
            lang: &self.lang,
            settings: &self.settings,
            now,
        };
        let out = self.tabs[idx].pubsub.update(&ctx, msg);
        for n in out.notes {
            self.note(n.level, n.text, n.toast);
        }
        out.cmds
    }

    /// 打开的 key(R-07b)。和 Keys 一样:用户操作要等 Tab 就绪,异步结果不挡;
    /// 视图关了或换了,迟到的结果在 `KeyView` 里按代号丢弃。
    fn on_view(&mut self, id: ConnId, msg: ViewMsg) -> Vec<Cmd> {
        if !msg.is_result() && !self.can_request(id) {
            return Vec::new();
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let now = self.now();
        let ctx = KeysCtx {
            id,
            lang: &self.lang,
            settings: &self.settings,
            now,
        };
        let tab = &mut self.tabs[idx];
        let out = match msg {
            ViewMsg::Open { key, kind } => {
                tab.view_seq += 1;
                match KeyView::open(&ctx, tab.view_seq, key, kind) {
                    Ok((view, out)) => {
                        tab.view = Some(view);
                        out
                    }
                    Err(err) => {
                        let text = describe(&self.lang, &err);
                        self.note(Level::Error, text, true);
                        return Vec::new();
                    }
                }
            }
            ViewMsg::NewKey(kind) => {
                tab.view_seq += 1;
                tab.view = KeyView::new_key(tab.view_seq, kind);
                return Vec::new();
            }
            ViewMsg::Close => {
                tab.view = None;
                return Vec::new();
            }
            other => match &mut tab.view {
                Some(view) => view.update(&ctx, other),
                None => return Vec::new(),
            },
        };
        let mut cmds = out.cmds;
        if out.close {
            self.tabs[idx].view = None;
        }
        for n in out.notes {
            self.note(n.level, n.text, n.toast);
        }
        for effect in out.list {
            let msg = match effect {
                ListEffect::Search => KeysMsg::Search,
                ListEffect::RefreshDetails(key) => KeysMsg::RefreshDetails(key),
            };
            cmds.extend(self.on_keys(id, msg));
        }
        cmds
    }

    fn on_db_selected(
        &mut self,
        id: ConnId,
        epoch: Epoch,
        db: u32,
        result: Result<()>,
    ) -> Vec<Cmd> {
        let Some(idx) = self
            .tabs
            .iter()
            .position(|t| t.id == id && t.epoch == epoch)
        else {
            return Vec::new();
        };
        match result {
            Ok(()) => {
                if let Some(c) = self.connections.iter_mut().find(|c| c.id == id) {
                    c.selected_db = db;
                }
                // 连接已被关掉重建:重新取连接信息(每个库的 key 数)
                let new_epoch = self.next_epoch();
                self.tabs[idx].status = TabStatus::Connecting;
                self.tabs[idx].epoch = new_epoch;
                vec![Cmd::Backend(BackendCmd::Connect {
                    id,
                    epoch: new_epoch,
                })]
            }
            Err(err) => {
                if let TabStatus::Switching { info, .. } = &self.tabs[idx].status {
                    self.tabs[idx].status = TabStatus::Ready(info.clone());
                }
                let text = self.describe(&err);
                self.note(Level::Error, text, true);
                Vec::new()
            }
        }
    }

    // ---- 连接编辑器 ----

    fn editor_save(&mut self) -> Vec<Cmd> {
        let Some(editor) = &mut self.editor else {
            return Vec::new();
        };
        if editor.saving {
            return Vec::new();
        }
        match editor.validate() {
            Ok(draft) => {
                editor.saving = true;
                editor.save_error = None;
                vec![Cmd::Backend(BackendCmd::SaveConnection(draft))]
            }
            Err(_) => Vec::new(), // 按钮本来就不该能点;错误由 editor.errors() 给界面显示
        }
    }

    fn on_connection_saved(&mut self, result: Result<Saved>) -> Vec<Cmd> {
        match result {
            Ok(saved) => {
                self.editor = None;
                let text = self.t("msg.connection_saved");
                self.note(Level::Success, text, true);
                // 已经打开的 Tab 用的是旧配置建的连接,不会自动重连(和 v1 一致):提示重新打开才生效
                if self.tab(saved.outcome.id).is_some() {
                    let text = self.t("msg.reopen_tab");
                    self.note(Level::Info, text, true);
                }
                // 连接已保存,密码没存上:提示,不当成保存失败(界面会重试造成重复连接)
                if let Some(reason) = &saved.outcome.password_error {
                    let text = self.t_with("msg.password_not_saved", &[("reason", reason)]);
                    self.note(Level::Warn, text, true);
                }
                if saved.keystore_degraded {
                    self.notify_keystore_degraded();
                }
                vec![Cmd::Backend(BackendCmd::LoadConnections)]
            }
            Err(err) => {
                let text = self.describe(&err);
                if let Some(e) = &mut self.editor {
                    e.saving = false;
                    e.save_error = Some(text.clone());
                }
                self.note(Level::Error, text, false);
                Vec::new()
            }
        }
    }

    // ---- 设置 ----

    fn settings_save(&mut self) -> Vec<Cmd> {
        let Some(editor) = &mut self.settings_editor else {
            return Vec::new();
        };
        if editor.saving {
            return Vec::new();
        }
        match editor.apply(&self.settings) {
            Ok(new) => {
                editor.saving = true;
                vec![self.save_settings_cmd(new.clone(), SaveIntent::Editor(new))]
            }
            Err(_) => Vec::new(),
        }
    }

    fn on_settings_saved(&mut self, token: u64, result: Result<()>) -> Vec<Cmd> {
        let Some(intent) = self.saves.remove(&token) else {
            return Vec::new();
        };
        match intent {
            SaveIntent::Editor(new) => match result {
                Ok(()) => {
                    let language_changed = new.language != self.settings.language;
                    self.settings = new;
                    self.settings_editor = None;
                    let mut cmds = Vec::new();
                    if language_changed {
                        let wanted = self.settings.language.clone();
                        self.lang = resolve_language(&wanted);
                        cmds.push(Cmd::Platform(PlatformCmd::SetLocale(self.lang.clone())));
                    }
                    let text = self.t("msg.settings_saved");
                    self.note(Level::Success, text, true);
                    cmds
                }
                Err(err) => {
                    if let Some(e) = &mut self.settings_editor {
                        e.saving = false;
                    }
                    let text = self.describe(&err);
                    self.note(Level::Error, text, true);
                    Vec::new()
                }
            },
            SaveIntent::Silent => {
                if let Err(err) = result {
                    let text = self.describe(&err);
                    self.note(Level::Error, text, true);
                }
                Vec::new()
            }
            SaveIntent::Decline => {
                // 保存没成功也照样退出:用户选了拒绝
                if let Err(err) = result {
                    tracing::warn!("保存协议拒绝记录失败: {err}");
                }
                vec![Cmd::Platform(PlatformCmd::Exit)]
            }
        }
    }
}

/// 窗口的最小尺寸(P-61),GUI 创建窗口时用。
pub const MIN_WINDOW: (u32, u32) = (WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
