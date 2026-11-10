//! 用户操作和异步结果(`Msg`),以及 `App::update` 返回的要执行的事(`Cmd`)。
//!
//! 模式:状态加消息加命令(docs/01「xredis-app」)。`App::update(msg)` 是同步的纯逻辑,
//! 返回的 `Cmd` 由别人执行:`BackendCmd` 由 [`Runtime`](crate::runtime::Runtime) 异步执行,
//! 结果变成 `Msg` 回来;`PlatformCmd` 由 GUI 层执行(剪贴板、文件对话框、开网址、退出……)。

use std::path::PathBuf;

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::config::{
    ConfigWarning, ConnId, ConnectionConfig, ConnectionDraft, SaveOutcome, Settings,
};
use xredis_core::conn::info::ConnectionInfo;
use xredis_core::keys::KeyType;
use xredis_core::members::WriteOp;
use xredis_core::v1_import::ImportWarning;

use crate::i18n::PackReport;
pub use crate::keylist::KeysMsg;

/// 异步请求的纪元。每个 Tab 每发一次请求就换一个新的,迟到的旧结果对不上就丢掉。
pub type Epoch = u64;

/// 一次启动加载出来的所有东西。**启动不会失败**:配置不可用时用默认值继续,问题放进 `warnings`(P-66)。
#[derive(Debug, Default)]
pub struct StartupData {
    pub settings: Settings,
    pub connections: Vec<ConnectionConfig>,
    /// 要显示给用户的提示(配置文件损坏已备份、配置目录不可用改用临时目录……)。结构化的,界面按种类翻译。
    pub warnings: Vec<StartupWarning>,
    /// 系统密钥存储不可用,密码只在内存里。
    pub keystore_degraded: bool,
    /// 第一次启动时从 v1 导入的结果。
    pub import: Option<ImportSummary>,
    /// 用户语言包的加载结果。
    pub packs: PackReport,
}

/// 启动时不影响继续运行的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupWarning {
    /// 打开配置时的问题(配置文件损坏已备份……)。
    Config(ConfigWarning),
    /// 配置目录不能用(路径被文件占用、没有权限),改用临时目录,改动退出后不保留。
    ConfigDirUnusable { reason: String, fallback: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub connections: usize,
    pub warnings: Vec<ImportWarning>,
    /// 导入本身失败(v1 文件损坏)。
    pub error: Option<String>,
}

/// 保存连接的结果加上密钥存储是否降级了(降级时 App 提示一次)。
#[derive(Debug)]
pub struct Saved {
    pub outcome: SaveOutcome,
    pub keystore_degraded: bool,
}

/// 连接编辑器里的文本字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorText {
    Host,
    Port,
    Username,
    Password,
    Name,
    Ordinal,
}

/// 三个证书文件字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileField {
    CaCert,
    ClientCert,
    ClientKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsText {
    ScanBatch,
    AutoScanLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubPage {
    Keys,
    PubSub,
    Console,
}

/// Help 菜单的外链(照 v1 `topMenus.tsx`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpLink {
    ReportBugs,
    BuyCoffee,
}

impl HelpLink {
    pub fn url(self) -> &'static str {
        match self {
            HelpLink::ReportBugs => "https://github.com/DreamvatLab/xRedis/issues",
            HelpLink::BuyCoffee => "https://www.buymeacoffee.com/dreamvat",
        }
    }
}

#[derive(Debug)]
pub enum Msg {
    // ---- 启动 ----
    StartupDone(Box<StartupData>),

    // ---- 连接列表和 Tab ----
    ToggleSider,
    OpenConnection(ConnId),
    CloseTab(ConnId),
    ActivateTab(ConnId),
    SetSubPage(ConnId, SubPage),
    /// 连接失败后点重试。
    Reconnect(ConnId),
    SelectDb {
        id: ConnId,
        db: u32,
    },
    Connected {
        id: ConnId,
        epoch: Epoch,
        result: Result<ConnectionInfo, Error>,
    },
    DbSelected {
        id: ConnId,
        epoch: Epoch,
        db: u32,
        result: Result<(), Error>,
    },
    ConnectionsLoaded(Vec<ConnectionConfig>),

    // ---- 连接编辑器 ----
    OpenEditor(Option<ConnId>),
    EditorSetText(EditorText, String),
    EditorSetTls(bool),
    EditorClearPassword(bool),
    EditorPickFile(FileField),
    /// 文件对话框的结果,`None` 是用户取消。
    EditorFilePicked(FileField, Option<PathBuf>),
    EditorClearFile(FileField),
    EditorSave,
    EditorCancel,
    ConnectionSaved(Result<Saved, Error>),

    // ---- 删除连接 ----
    DeleteRequested(ConnId),
    ConfirmAccepted,
    ConfirmCancelled,
    ConnectionDeleted {
        id: ConnId,
        result: Result<(), Error>,
    },

    // ---- 设置 ----
    OpenSettings,
    SettingsSetText(SettingsText, String),
    SettingsSetShowDetails(bool),
    SettingsSetLanguage(String),
    SettingsSave,
    SettingsCancel,
    /// `token` 对应发起保存时记下的用途(设置面板、折叠侧栏、协议同意……)。
    SettingsSaved {
        token: u64,
        result: Result<(), Error>,
    },
    ExportLanguageTemplate,
    /// 保存模板的结果:成功是写到的路径,失败是原因;`Ok(None)` 是用户取消了对话框。
    TemplateSaved(Result<Option<String>, String>),

    // ---- 免责协议、隐私政策、Help ----
    AgreeDisclaimer,
    DeclineDisclaimer,
    ShowAgreement(bool),
    ShowPrivacy(bool),
    OpenHelpLink(HelpLink),

    // ---- 消息栏和错误框 ----
    ToggleMessageLog,
    /// 错误框上的“打开消息日志”:关掉错误框,展开消息列表。
    OpenMessageLog,
    ClearMessages,
    DismissToast(u64),
    DismissError,

    // ---- 窗口 ----
    WindowChanged {
        width: u32,
        height: u32,
        maximized: bool,
    },
    /// 防抖到点:代号还是最新的才保存。
    WindowSaveDue(u64),
    WindowSaved(Result<(), Error>),

    // ---- Keys 子页(R-07a):用户操作和异步结果都在 `KeysMsg` 里 ----
    Keys {
        id: ConnId,
        msg: KeysMsg,
    },
    // ---- 打开的 key(R-07b):值编辑器、成员表、新建 key、改名和改 TTL ----
    View {
        id: ConnId,
        msg: crate::keyview::ViewMsg,
    },
}

/// 要异步执行的后端操作,结果由 Runtime 变成 `Msg` 发回。
#[derive(Debug)]
pub enum BackendCmd {
    Startup,
    LoadConnections,
    SaveConnection(ConnectionDraft),
    DeleteConnection(ConnId),
    Connect {
        id: ConnId,
        epoch: Epoch,
    },
    CloseConnection(ConnId),
    SelectDb {
        id: ConnId,
        epoch: Epoch,
        db: u32,
    },
    SaveSettings {
        settings: Settings,
        token: u64,
    },
    SaveWindowSize {
        width: u32,
        height: u32,
        maximized: bool,
    },

    // ---- Keys 子页(R-07a)。`generation` 是扫描代号:结果带着它回来,对不上就丢弃 ----
    /// 扫一批 key。`pattern` 为 `*` 时 core 不带 MATCH。
    Scan {
        id: ConnId,
        generation: u64,
        pattern: Bytes,
        count: u32,
        cursor: u64,
    },
    /// 取一个 key 的类型、长度、TTL。
    KeyDetails {
        id: ConnId,
        generation: u64,
        key: Bytes,
    },
    DeleteKeys {
        id: ConnId,
        keys: Vec<Bytes>,
    },
    DbSize {
        id: ConnId,
    },
    CopyKeys {
        id: ConnId,
        keys: Vec<Bytes>,
    },
    /// 粘贴前预检:解码剪贴板,查每个 key 是否已存在(R-07d)。
    PreviewPaste {
        id: ConnId,
        text: String,
    },
    // ---- 打开的 key(R-07b)。`generation` 是视图代号,结果带着它回来 ----
    /// 取 key 的类型、长度、TTL(视图用)。
    ViewDetails {
        id: ConnId,
        generation: u64,
        key: Bytes,
    },
    /// 读 String 的值(超过 8 MiB 只读前 1 MiB)。
    ViewReadString {
        id: ConnId,
        generation: u64,
        key: Bytes,
    },
    /// 扫一批成员。`scan_generation` 是成员表自己的扫描代号。
    ViewScan {
        id: ConnId,
        generation: u64,
        scan_generation: u64,
        key: Bytes,
        kind: KeyType,
        pattern: Bytes,
        count: u32,
        cursor: u64,
    },
    /// 依次执行一串写操作(保存、改名、改 TTL、删成员),第一个失败就停。
    ViewApply {
        id: ConnId,
        generation: u64,
        token: u64,
        ops: Vec<WriteOp>,
    },
    PasteKeys {
        id: ConnId,
        text: String,
        overwrite: bool,
    },
}

/// 平台能力,由 GUI 层执行。
#[derive(Debug)]
pub enum PlatformCmd {
    OpenUrl(String),
    Exit,
    /// 弹文件对话框选文件,结果用 `Msg::EditorFilePicked` 发回。
    PickFile(FileField),
    /// 弹“另存为”对话框并写文件,结果用 `Msg::TemplateSaved` 发回。
    SaveTextFile {
        suggested_name: String,
        contents: String,
    },
    /// 过 `ms` 毫秒再把 `msg` 发回给 App(防抖用)。
    After {
        ms: u64,
        msg: Box<Msg>,
    },
    /// 界面语言变了:切换全局 locale,让组件库自带的文字跟着变,并刷新界面。
    SetLocale(String),
    /// 写系统剪贴板,结果用 `Msg::Keys { id, ClipboardWritten }` 发回。
    /// 失败(比如 Windows 上剪贴板被别的程序占着)必须报回来,不能静默(L-13)。
    WriteClipboard {
        id: ConnId,
        text: String,
    },
    /// 读系统剪贴板的文本,结果用 `Msg::Keys { id, ClipboardRead }` 发回。
    ReadClipboard {
        id: ConnId,
    },
}

#[derive(Debug)]
pub enum Cmd {
    Backend(BackendCmd),
    Platform(PlatformCmd),
}
