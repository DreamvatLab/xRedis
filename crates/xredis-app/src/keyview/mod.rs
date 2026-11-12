//! 打开一个 key 之后的视图模型(R-07b,P-20 至 P-30):String 的值编辑器、成员表加成员编辑器、
//! 新建 key、改名和改 TTL 的对话框。
//!
//! 每个 Tab 最多一个 [`KeyView`]。和 `KeyList` 一样是同步纯逻辑:`update(ctx, msg)` 返回要执行的命令、
//! 要记的消息和对 key 列表的影响。异步结果带着 `generation`(这个视图的代号),视图关了或换了,
//! 迟到的结果对不上就丢弃。

pub mod display;
pub mod editor;
pub mod table;

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::keys::{KeyDetails, KeyType, Ttl, validate_ttl};
use xredis_core::members::{ApplyFailure, MemberPage, WriteOp};
use xredis_core::types::list::Side;
use xredis_core::types::string::StringValue;

use crate::errors::describe;
use crate::i18n::{tr, tr_with};
use crate::keylist::{Ctx, Note};
use crate::messages::Level;
use crate::msg::{BackendCmd, Cmd};
use display::show;
use editor::{Editor, FormError, Target};
use table::{MemberActions, MemberColumn, MemberStatus, MemberTable, ScanRequest, TableMsg};

/// 用户操作和异步结果。
#[derive(Debug)]
pub enum ViewMsg {
    // ---- 打开和关闭(`App` 处理,因为要创建或丢掉视图)----
    Open {
        key: Bytes,
        kind: KeyType,
    },
    /// 工具栏 New 下拉:选一种类型,打开新建 key 的编辑器。
    NewKey(KeyType),
    Close,

    // ---- 用户操作 ----
    /// 刷新:重读详情,成员表从头扫。
    Refresh,
    SetKeyName(String),
    SetMemberName(String),
    SetValue(String),
    SetPosition(Side),
    SetHex(bool),
    /// 语言下拉(只影响 Format、Minify 能不能用)。
    SetFormat(display::Format),
    Undo,
    FormatJson,
    MinifyJson,
    Save,
    /// 关编辑器:String 和新建 key 的编辑器就是整个视图,关了就回到 key 列表;成员编辑器只关弹窗。
    CloseEditor,
    /// 点成员打开编辑器(参数是成员名;List 是下标的十进制文本)。
    OpenMember(Bytes),
    NewMember,
    Table(TableMsg),
    OpenRename,
    SetRenameText(String),
    ConfirmRename,
    CancelRename,
    OpenTtl,
    SetTtlText(String),
    ConfirmTtl,
    CancelTtl,

    // ---- 异步结果 ----
    Details {
        generation: u64,
        result: xredis_core::Result<KeyDetails>,
    },
    StringRead {
        generation: u64,
        result: xredis_core::Result<Option<StringValue>>,
    },
    Members {
        generation: u64,
        scan_generation: u64,
        result: xredis_core::Result<MemberPage>,
    },
    Applied {
        generation: u64,
        token: u64,
        result: Result<(), ApplyFailure>,
    },
}

impl ViewMsg {
    /// 异步结果(不是用户操作):被协议弹窗挡住或 Tab 没就绪时也要处理,不能丢。
    pub fn is_result(&self) -> bool {
        matches!(
            self,
            ViewMsg::Details { .. }
                | ViewMsg::StringRead { .. }
                | ViewMsg::Members { .. }
                | ViewMsg::Applied { .. }
        )
    }
}

/// 视图对 key 列表的影响。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEffect {
    /// 重新搜索(新建、改名之后)。
    Search,
    /// 只刷新这一行的长度和 TTL。
    RefreshDetails(Bytes),
}

#[derive(Debug, Default)]
pub struct ViewOut {
    pub cmds: Vec<Cmd>,
    pub notes: Vec<Note>,
    pub list: Vec<ListEffect>,
    /// 视图要关掉(保存成功、key 没了)。
    pub close: bool,
}

impl ViewOut {
    fn note(&mut self, level: Level, text: String, toast: bool) {
        self.notes.push(Note { level, text, toast });
    }

    fn error(&mut self, ctx: &Ctx, err: &Error) {
        self.note(Level::Error, describe(ctx.lang, err), true);
    }
}

/// 改 key 名和改 TTL 的对话框。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDialog {
    pub text: String,
    /// 输入不合规矩或服务端拒绝的原因(已翻译),对话框不关。
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    SaveEditor,
    RenameKey,
    SetTtl,
    DeleteMembers,
}

#[derive(Debug)]
struct Pending {
    token: u64,
    purpose: Purpose,
    ops: Vec<WriteOp>,
}

/// 视图工具栏上的按钮哪些可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewActions {
    pub rename: bool,
    pub ttl: bool,
}

#[derive(Debug)]
pub struct KeyView {
    /// 当前 key 名(改名成功后更新)。新建 key 的编辑器里是空的。
    pub key: Bytes,
    pub kind: KeyType,
    /// 长度和 TTL,读到之前是 `None`(不能用 0 和 -1 推断)。
    pub details: Option<KeyDetails>,
    pub editor: Option<Editor>,
    pub table: Option<MemberTable>,
    pub rename: Option<InputDialog>,
    pub ttl: Option<InputDialog>,
    generation: u64,
    /// 每打开一个新编辑器加一(界面靠它判断要不要重建输入控件)。
    editor_seq: u64,
    token: u64,
    pending: Option<Pending>,
}

impl KeyView {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 有写操作在途:这时不接受别的写操作。
    pub fn is_writing(&self) -> bool {
        self.pending.is_some()
    }

    fn blank(generation: u64, key: Bytes, kind: KeyType) -> Self {
        Self {
            key,
            kind,
            details: None,
            editor: None,
            table: None,
            rename: None,
            ttl: None,
            generation,
            editor_seq: 0,
            token: 0,
            pending: None,
        }
    }

    /// 打开一个已有的 key。类型不能打开(Stream、模块类型)返回错误,不创建视图。
    pub fn open(
        ctx: &Ctx,
        generation: u64,
        key: Bytes,
        kind: KeyType,
    ) -> Result<(Self, ViewOut), Error> {
        if !kind.openable() {
            return Err(Error::NotOpenable {
                kind: format!("{kind:?}"),
            });
        }
        let mut v = Self::blank(generation, key.clone(), kind.clone());
        let mut out = ViewOut::default();
        v.request_details(ctx, &mut out);
        if kind == KeyType::String {
            v.editor = Some(Editor::string_key(key.clone()));
            v.editor_seq += 1;
            out.cmds.push(Cmd::Backend(BackendCmd::ViewReadString {
                id: ctx.id,
                generation,
                key,
            }));
        } else {
            v.table = Some(MemberTable::new(key, kind));
            v.search_members(ctx, &mut out);
        }
        Ok((v, out))
    }

    /// 新建 key 的编辑器(P-20)。
    pub fn new_key(generation: u64, kind: KeyType) -> Option<Self> {
        if !kind.openable() {
            return None;
        }
        let mut v = Self::blank(generation, Bytes::new(), kind.clone());
        v.editor = Some(Editor::new_key(kind));
        v.editor_seq += 1;
        Some(v)
    }

    pub fn is_creating(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|e| matches!(e.target, Target::NewKey { .. }))
    }

    fn request_details(&self, ctx: &Ctx, out: &mut ViewOut) {
        out.cmds.push(Cmd::Backend(BackendCmd::ViewDetails {
            id: ctx.id,
            generation: self.generation,
            key: self.key.clone(),
        }));
    }

    fn scan_cmd(&self, ctx: &Ctx, req: ScanRequest) -> Cmd {
        Cmd::Backend(BackendCmd::ViewScan {
            id: ctx.id,
            generation: self.generation,
            scan_generation: req.generation,
            key: self.key.clone(),
            kind: self.kind.clone(),
            pattern: req.pattern,
            count: req.count,
            cursor: req.cursor,
        })
    }

    fn search_members(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        if let Some(t) = &mut self.table {
            let req = t.search(ctx.settings.scan_batch);
            let cmd = self.scan_cmd(ctx, req);
            out.cmds.push(cmd);
        }
    }

    /// 发一串写操作。有写操作在途时拒绝(返回 `false`)。
    fn write(&mut self, ctx: &Ctx, purpose: Purpose, ops: Vec<WriteOp>, out: &mut ViewOut) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.token += 1;
        out.cmds.push(Cmd::Backend(BackendCmd::ViewApply {
            id: ctx.id,
            generation: self.generation,
            token: self.token,
            ops: ops.clone(),
        }));
        self.pending = Some(Pending {
            token: self.token,
            purpose,
            ops,
        });
        true
    }

    pub fn update(&mut self, ctx: &Ctx, msg: ViewMsg) -> ViewOut {
        let mut out = ViewOut::default();
        match msg {
            // 这三个由 App 处理
            ViewMsg::Open { .. } | ViewMsg::NewKey(_) | ViewMsg::Close => {}

            ViewMsg::Refresh => {
                if !self.is_creating() {
                    self.request_details(ctx, &mut out);
                    self.search_members(ctx, &mut out);
                }
            }
            ViewMsg::SetKeyName(t) => self.with_editor(|e| e.set_key_name(t)),
            ViewMsg::SetMemberName(t) => self.with_editor(|e| e.set_member_name(t)),
            ViewMsg::SetValue(t) => self.with_editor(|e| e.set_value(t)),
            ViewMsg::SetPosition(side) => self.with_editor(|e| e.position = side),
            ViewMsg::SetHex(on) => self.with_editor(|e| e.hex = on && !e.is_new()),
            ViewMsg::SetFormat(f) => self.with_editor(|e| e.set_format(f)),
            ViewMsg::Undo => self.with_editor(Editor::undo),
            ViewMsg::FormatJson => self.reformat(ctx, true, &mut out),
            ViewMsg::MinifyJson => self.reformat(ctx, false, &mut out),
            ViewMsg::Save => self.save(ctx, &mut out),
            ViewMsg::CloseEditor => match &self.editor {
                Some(e) if e.is_key_level() => out.close = true,
                _ => {
                    // 保存在途时不关:结果回来还要用
                    if !self.is_writing() {
                        self.editor = None;
                    }
                }
            },
            ViewMsg::OpenMember(name) => {
                if let Some(t) = &self.table
                    && !self.is_writing()
                    && let Some(row) = t.find(&name)
                {
                    self.editor = Some(Editor::member(self.key.clone(), self.kind.clone(), row));
                    self.editor_seq += 1;
                }
            }
            ViewMsg::NewMember => {
                if self.table.is_some() && !self.is_writing() {
                    self.editor = Some(Editor::new_member(self.key.clone(), self.kind.clone()));
                    self.editor_seq += 1;
                }
            }
            ViewMsg::Table(m) => self.on_table(ctx, m, &mut out),

            ViewMsg::OpenRename => {
                if !self.is_creating() && !self.is_writing() {
                    self.rename = Some(InputDialog {
                        text: show(&self.key).text,
                        error: None,
                    });
                }
            }
            ViewMsg::SetRenameText(t) => {
                if let Some(d) = &mut self.rename {
                    d.text = t;
                    d.error = None;
                }
            }
            ViewMsg::ConfirmRename => self.confirm_rename(ctx, &mut out),
            ViewMsg::CancelRename => {
                if !self.is_writing() {
                    self.rename = None;
                }
            }
            ViewMsg::OpenTtl => {
                if !self.is_creating() && !self.is_writing() {
                    let text = match self.details.as_ref().map(|d| d.ttl) {
                        Some(Ttl::Seconds(n)) => n.to_string(),
                        _ => "-1".to_owned(),
                    };
                    self.ttl = Some(InputDialog { text, error: None });
                }
            }
            ViewMsg::SetTtlText(t) => {
                if let Some(d) = &mut self.ttl {
                    d.text = t;
                    d.error = None;
                }
            }
            ViewMsg::ConfirmTtl => self.confirm_ttl(ctx, &mut out),
            ViewMsg::CancelTtl => {
                if !self.is_writing() {
                    self.ttl = None;
                }
            }

            ViewMsg::Details { generation, result } => {
                if generation == self.generation {
                    self.on_details(ctx, result, &mut out);
                }
            }
            ViewMsg::StringRead { generation, result } => {
                if generation == self.generation {
                    self.on_string_read(ctx, result, &mut out);
                }
            }
            ViewMsg::Members {
                generation,
                scan_generation,
                result,
            } => {
                if generation == self.generation {
                    self.on_members(ctx, scan_generation, result, &mut out);
                }
            }
            ViewMsg::Applied {
                generation,
                token,
                result,
            } => {
                if generation == self.generation {
                    self.on_applied(ctx, token, result, &mut out);
                }
            }
        }
        out
    }

    fn with_editor(&mut self, f: impl FnOnce(&mut Editor)) {
        if let Some(e) = &mut self.editor {
            f(e);
        }
    }

    fn reformat(&mut self, ctx: &Ctx, pretty: bool, out: &mut ViewOut) {
        let Some(e) = &mut self.editor else { return };
        let result = if pretty {
            e.format_value()
        } else {
            e.minify_value()
        };
        if let Err(reason) = result {
            out.note(
                Level::Warn,
                tr_with(ctx.lang, "view.format_failed", &[("reason", &reason)]),
                true,
            );
        }
    }

    // ---- 保存 ----

    fn save(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        let Some(e) = &mut self.editor else { return };
        if e.loading || e.saving || self.pending.is_some() {
            return;
        }
        match e.save_ops() {
            Ok(ops) => {
                e.saving = true;
                self.write(ctx, Purpose::SaveEditor, ops, out);
            }
            Err(FormError::NothingChanged | FormError::ReadOnly) => {}
            Err(err) => out.note(Level::Warn, form_error_text(ctx.lang, &err), true),
        }
    }

    fn confirm_rename(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        let Some(d) = &mut self.rename else { return };
        if self.pending.is_some() {
            return;
        }
        if d.text.is_empty() {
            d.error = Some(form_error_text(ctx.lang, &FormError::KeyNameEmpty));
            return;
        }
        let new = Bytes::from(d.text.clone().into_bytes());
        if new == self.key {
            // 没改:当作取消
            self.rename = None;
            return;
        }
        let op = WriteOp::RenameKey {
            old: self.key.clone(),
            new,
        };
        self.write(ctx, Purpose::RenameKey, vec![op], out);
    }

    fn confirm_ttl(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        let Some(d) = &mut self.ttl else { return };
        if self.pending.is_some() {
            return;
        }
        let Ok(seconds) = d.text.trim().parse::<i64>() else {
            d.error = Some(tr(ctx.lang, "view.err.ttl_not_a_number"));
            return;
        };
        // -1 持久化,0 拒绝(EXPIRE 0 会删掉 key),超范围拒绝:规则在 core,这里先校验给出提示
        if let Err(err) = validate_ttl(seconds) {
            d.error = Some(describe(ctx.lang, &err));
            return;
        }
        let op = WriteOp::SetTtl {
            key: self.key.clone(),
            seconds,
        };
        self.write(ctx, Purpose::SetTtl, vec![op], out);
    }

    // ---- 成员表 ----

    fn on_table(&mut self, ctx: &Ctx, msg: TableMsg, out: &mut ViewOut) {
        let batch = ctx.settings.scan_batch;
        let Some(t) = &mut self.table else { return };
        let req = match msg {
            TableMsg::Search => Some(t.search(batch)),
            TableMsg::LoadMore => t.load_more(batch),
            TableMsg::LoadAll => t.load_all(batch),
            TableMsg::Stop => {
                t.stop();
                None
            }
            TableMsg::SetPattern(p) => {
                t.pattern = p;
                None
            }
            TableMsg::SetFilter(f) => {
                t.set_filter(f, ctx.now);
                None
            }
            TableMsg::SortBy(c) => {
                t.sort_by(c, ctx.now);
                None
            }
            TableMsg::Select { name, on } => {
                t.select(name, on);
                None
            }
            TableMsg::SelectAllFiltered(on) => {
                t.select_all_filtered(on);
                None
            }
            TableMsg::DeleteSelected => {
                if !t.is_scanning() && t.selected_count() > 0 && self.pending.is_none() {
                    t.confirm_delete = Some(t.selected_count());
                }
                None
            }
            TableMsg::CancelDelete => {
                t.confirm_delete = None;
                None
            }
            TableMsg::ConfirmDelete => {
                if t.confirm_delete.take().is_some()
                    && let Some(op) = t.delete_op()
                {
                    self.write(ctx, Purpose::DeleteMembers, vec![op], out);
                }
                None
            }
        };
        if let Some(req) = req {
            let cmd = self.scan_cmd(ctx, req);
            out.cmds.push(cmd);
        }
    }

    // ---- 异步结果 ----

    fn key_changed(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        out.note(
            Level::Warn,
            tr_with(
                ctx.lang,
                "view.key_changed",
                &[("key", &show(&self.key).text)],
            ),
            true,
        );
        out.close = true;
    }

    fn on_details(
        &mut self,
        ctx: &Ctx,
        result: xredis_core::Result<KeyDetails>,
        out: &mut ViewOut,
    ) {
        match result {
            Ok(d) if d.kind != self.kind || d.ttl == Ttl::Missing => self.key_changed(ctx, out),
            Ok(d) => {
                if let Some(t) = &mut self.table {
                    t.length = d.length;
                }
                self.details = Some(d);
            }
            // 取不到详情不打扰太多:提示一次,界面上长度和 TTL 保持“未加载”
            Err(err) => out.error(ctx, &err),
        }
    }

    fn on_string_read(
        &mut self,
        ctx: &Ctx,
        result: xredis_core::Result<Option<StringValue>>,
        out: &mut ViewOut,
    ) {
        match result {
            Ok(Some(v)) => {
                if let Some(e) = &mut self.editor {
                    e.loaded_string(v.data, v.total_len, v.truncated);
                }
            }
            Ok(None) => self.key_changed(ctx, out),
            Err(err) => {
                out.error(ctx, &err);
                out.close = true;
            }
        }
    }

    fn on_members(
        &mut self,
        ctx: &Ctx,
        scan_generation: u64,
        result: xredis_core::Result<MemberPage>,
        out: &mut ViewOut,
    ) {
        let batch = ctx.settings.scan_batch;
        let auto = ctx.settings.auto_scan_limit;
        let Some(t) = &mut self.table else { return };
        match result {
            Ok(page) => {
                if let Some(req) = t.on_batch(scan_generation, page, batch, auto, ctx.now) {
                    let cmd = self.scan_cmd(ctx, req);
                    out.cmds.push(cmd);
                }
            }
            Err(err) => {
                if t.on_scan_error(scan_generation) {
                    out.error(ctx, &err);
                }
            }
        }
    }

    fn on_applied(
        &mut self,
        ctx: &Ctx,
        token: u64,
        result: Result<(), ApplyFailure>,
        out: &mut ViewOut,
    ) {
        let Some(p) = self.pending.take_if(|p| p.token == token) else {
            return;
        };
        match result {
            Ok(()) => self.applied_ok(ctx, p, out),
            Err(ApplyFailure { done, error }) => self.applied_err(ctx, p, done, error, out),
        }
    }

    fn applied_ok(&mut self, ctx: &Ctx, p: Pending, out: &mut ViewOut) {
        let shown = |b: &Bytes| show(b).text;
        for op in &p.ops {
            if let WriteOp::RenameKey { old, new } = op {
                out.note(
                    Level::Success,
                    tr_with(
                        ctx.lang,
                        "view.renamed",
                        &[("old", &shown(old)), ("new", &shown(new))],
                    ),
                    true,
                );
            }
        }
        let renamed = p.ops.iter().find_map(|op| match op {
            WriteOp::RenameKey { new, .. } => Some(new.clone()),
            _ => None,
        });
        match p.purpose {
            Purpose::SaveEditor => {
                if p.ops
                    .iter()
                    .any(|op| !matches!(op, WriteOp::RenameKey { .. }))
                {
                    out.note(Level::Success, tr(ctx.lang, "view.saved"), true);
                }
                let editor = self.editor.take();
                let key_level = editor.as_ref().is_some_and(Editor::is_key_level);
                if key_level {
                    // String 编辑和新建 key:整个视图就是这个编辑器,关掉回列表
                    out.list
                        .push(if renamed.is_some() || p.ops.iter().any(is_create) {
                            ListEffect::Search
                        } else {
                            ListEffect::RefreshDetails(self.key.clone())
                        });
                    out.close = true;
                } else {
                    // 成员变了:成员表从头重读,长度也变了
                    self.reload(ctx, out);
                }
            }
            Purpose::RenameKey => {
                self.rename = None;
                out.list.push(ListEffect::Search);
                // 和 v1 一样:改名后关掉这个 key 的视图,回到列表
                out.close = true;
            }
            Purpose::SetTtl => {
                self.ttl = None;
                out.note(
                    Level::Success,
                    tr_with(ctx.lang, "view.ttl_updated", &[("key", &shown(&self.key))]),
                    true,
                );
                self.request_details(ctx, out);
                out.list.push(ListEffect::RefreshDetails(self.key.clone()));
            }
            Purpose::DeleteMembers => {
                let count = p
                    .ops
                    .iter()
                    .map(|op| match op {
                        WriteOp::Member { op, .. } => member_count(op),
                        _ => 0,
                    })
                    .sum::<usize>();
                out.note(
                    Level::Success,
                    tr_with(
                        ctx.lang,
                        "view.members_deleted",
                        &[("count", &count.to_string()), ("key", &shown(&self.key))],
                    ),
                    true,
                );
                self.reload(ctx, out);
            }
        }
    }

    /// 成员表从头重读 + 详情 + 列表这一行刷新。
    fn reload(&mut self, ctx: &Ctx, out: &mut ViewOut) {
        if let Some(t) = &mut self.table {
            t.clear_selection();
        }
        self.request_details(ctx, out);
        self.search_members(ctx, out);
        out.list.push(ListEffect::RefreshDetails(self.key.clone()));
    }

    fn applied_err(&mut self, ctx: &Ctx, p: Pending, done: usize, err: Error, out: &mut ViewOut) {
        out.error(ctx, &err);
        // 前面几步已经成功:把界面状态跟上,免得再保存时重复做
        let finished = &p.ops[..done.min(p.ops.len())];
        if let Some(WriteOp::RenameKey { new, .. }) = finished.first() {
            self.key = new.clone();
            out.list.push(ListEffect::Search);
        }
        if let Some(e) = &mut self.editor {
            e.saving = false;
            e.apply_partial(finished);
        }
        match p.purpose {
            Purpose::SaveEditor => {
                if !finished.is_empty() && self.table.is_some() {
                    self.reload(ctx, out);
                }
            }
            Purpose::RenameKey => {
                if let Some(d) = &mut self.rename {
                    d.error = Some(describe(ctx.lang, &err));
                }
            }
            Purpose::SetTtl => {
                if let Some(d) = &mut self.ttl {
                    d.error = Some(describe(ctx.lang, &err));
                }
            }
            Purpose::DeleteMembers => {}
        }
    }

    // ---- 给界面的小查询 ----

    pub fn title_key(&self) -> String {
        show(&self.key).text
    }

    /// 标题:已有 key 是 key 名,新建是“New 类型”。
    pub fn title(&self, lang: &str) -> String {
        if self.is_creating() {
            tr_with(
                lang,
                "view.title.new",
                &[("type", crate::keylist::type_label(Some(&self.kind)))],
            )
        } else {
            self.title_key()
        }
    }

    /// 工具栏的 Rename 和 TTL 按钮能不能点:新建 key 还没有 key,有写操作在途时不接受别的写。
    pub fn actions(&self) -> ViewActions {
        let on = !self.is_creating() && !self.is_writing();
        ViewActions {
            rename: on,
            ttl: on,
        }
    }

    pub fn editor_seq(&self) -> u64 {
        self.editor_seq
    }

    /// 成员编辑器弹窗的标题;不是成员编辑器是 `None`。
    pub fn editor_title(&self, lang: &str) -> Option<String> {
        match &self.editor.as_ref()?.target {
            Target::Member { .. } => Some(tr(lang, "view.member.edit")),
            Target::NewMember { .. } => Some(tr(lang, "view.member.new")),
            _ => None,
        }
    }

    /// 成员表的按钮可用状态;String 和新建 key 没有成员表。
    pub fn member_actions(&self) -> Option<MemberActions> {
        self.table.as_ref().map(|t| t.actions(self.is_writing()))
    }

    pub fn member_status(&self, lang: &str) -> Option<MemberStatus> {
        self.table.as_ref().map(|t| t.status(lang))
    }

    /// 长度的文字:没读到是 `None`(0 是合法值)。
    pub fn length_text(&self) -> Option<String> {
        self.details.as_ref()?.length.map(|n| n.to_string())
    }

    /// TTL 的文字(秒,-1 永不过期):没读到是 `None`。
    pub fn ttl_text(&self) -> Option<String> {
        self.details
            .as_ref()
            .map(|d| crate::keylist::ttl_text(d.ttl))
    }

    /// 成员表的列标题(翻译 key,GUI 用 `keyview::label` 取文字)。
    pub fn column_titles(&self) -> Option<(&'static str, &'static str)> {
        match self.kind {
            KeyType::Hash => Some(("view.col.field", "view.col.value")),
            KeyType::List => Some(("view.col.index", "view.col.value")),
            KeyType::Set => Some(("view.col.member", "view.col.member")),
            KeyType::ZSet => Some(("view.col.member", "view.col.score")),
            _ => None,
        }
    }

    pub fn sortable_columns(&self) -> &'static [MemberColumn] {
        match self.kind {
            KeyType::Set => &[MemberColumn::Name],
            _ => &[MemberColumn::Name, MemberColumn::Value],
        }
    }
}

fn is_create(op: &WriteOp) -> bool {
    matches!(op, WriteOp::CreateKey { .. })
}

fn member_count(op: &xredis_core::members::MemberOp) -> usize {
    use xredis_core::members::MemberOp::*;
    match op {
        HashDelete(v) | SetRemove(v) | ZRemove(v) => v.len(),
        ListRemove(v) => v.len(),
        _ => 0,
    }
}

/// 保存前校验失败的提示文字。
pub fn form_error_text(lang: &str, err: &FormError) -> String {
    let key = match err {
        FormError::ReadOnly => "view.err.read_only",
        FormError::NothingChanged => "view.err.nothing_changed",
        FormError::KeyNameEmpty => "view.err.key_name_empty",
        FormError::MemberNameEmpty => "view.err.member_name_empty",
        FormError::ScoreInvalid => "view.err.score_invalid",
    };
    tr(lang, key)
}

/// 给 GUI 用:取一条翻译(GUI 不直接碰 i18n 模块)。
pub fn label(lang: &str, key: &str) -> String {
    tr(lang, key)
}

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;
