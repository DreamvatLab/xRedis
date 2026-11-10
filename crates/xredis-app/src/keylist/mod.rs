//! Keys 子页的视图模型(R-07a,P-10 至 P-16、P-40 至 P-42 的界面逻辑)。
//!
//! 每个 Tab 一个 [`KeyList`]。和 `App` 一样是同步纯逻辑:`update(ctx, msg)` 返回要执行的命令和要记的消息。
//!
//! **扫描用“一批一条命令”驱动**:每次只发一批 `Scan`,结果回来再决定继续还是停。所以:
//! - Stop 就是“不再发下一批”,不需要长时间运行的后台任务和取消令牌;停止后游标保留,还能 More、All。
//! - 迟到的批次靠扫描代号 `generation` 丢弃(新搜索、切库、关 Tab 之后旧批次的结果回来对不上)。
//!
//! 三档(P-11):
//! - **搜索**:先扫一批;结果为空但游标没归零就继续扫(v1 这段不响应 Stop,v2 响应);然后自动续扫,
//!   直到已加载数量达到“自动续扫上限”(0 表示关闭)。
//! - **More**:再扫一批。
//! - **All**:一直扫到游标归零,不设上限,可以中途 Stop。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use bytes::Bytes;
use xredis_core::config::{ConnId, Settings};
use xredis_core::consts::{CLIPBOARD_PREFIX, POOL_SIZE};
use xredis_core::error::Skipped;
use xredis_core::keys::{KeyDetails, KeyType, ScanPage, Ttl};
use xredis_core::transfer::{CopyOutcome, PasteOutcome};
use xredis_core::{Error, Result};

use crate::errors::describe;
use crate::i18n::{tr, tr_with};
use crate::messages::Level;
use crate::msg::{BackendCmd, Cmd, PlatformCmd};

/// 列表刷新的最短间隔。扫到底时每批都重排会是 O(N²)(v1 的备忘:2000 一批加载 100 万个 key 约 500 批),
/// 所以按时间节流:仍然渐进更新,但把刷新频率压到人眼够用的程度。扫描结束时一定会刷新。
pub const VIEW_REFRESH_MS: i64 = 300;

/// 用户操作和异步结果。
#[derive(Debug)]
pub enum KeysMsg {
    // ---- 用户操作 ----
    SetPattern(String),
    /// 搜索(Scan 按钮或回车)。
    Search,
    LoadMore,
    LoadAll,
    Stop,
    /// 列头过滤:在已加载的数据里按子串过滤(不区分大小写)。
    SetFilter(String),
    /// 点列头排序:同一列再点一次换方向。
    SortBy(SortColumn),
    /// GUI 报告当前可见的行(虚拟滚动的可见范围),打开“显示 key 详情”时据此自动加载详情。
    Visible(Vec<Bytes>),
    /// 手动刷新一行的详情。
    RefreshDetails(Bytes),
    Select {
        key: Bytes,
        on: bool,
    },
    /// 全选 = 当前过滤后已加载的全部行。
    SelectAllFiltered(bool),
    DeleteSelected,
    ConfirmDelete,
    CancelDelete,
    CopySelected,
    PasteRequested,
    SetPasteOverwrite(bool),
    ConfirmPaste,
    CancelPaste,

    // ---- 异步结果 ----
    ScanBatch {
        generation: u64,
        result: Result<ScanPage<Bytes>>,
    },
    DetailsLoaded {
        generation: u64,
        key: Bytes,
        result: Result<KeyDetails>,
    },
    Deleted(Result<u64>),
    DbSize(Result<u64>),
    Copied(Result<CopyOutcome>),
    ClipboardWritten(std::result::Result<(), String>),
    ClipboardRead(std::result::Result<String, String>),
    Pasted(Result<PasteOutcome>),
}

impl KeysMsg {
    /// 异步结果(不是用户操作):被协议弹窗挡住或 Tab 没就绪时也要处理,不能丢。
    pub fn is_result(&self) -> bool {
        matches!(
            self,
            KeysMsg::ScanBatch { .. }
                | KeysMsg::DetailsLoaded { .. }
                | KeysMsg::Deleted(_)
                | KeysMsg::DbSize(_)
                | KeysMsg::Copied(_)
                | KeysMsg::ClipboardWritten(_)
                | KeysMsg::ClipboardRead(_)
                | KeysMsg::Pasted(_)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Key,
    Type,
    Length,
    Ttl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: SortColumn,
    pub dir: SortDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    Idle,
    Searching,
    LoadingMore,
    LoadingAll,
    /// 用户点了 Stop:当前这批回来之后就不再继续。
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Deleting,
    Copying,
    Pasting,
}

/// 一行的详情加载状态。“已加载”是显式的状态,不能用 -1 或 0 推断(L-16):
/// TTL 为 -1 是合法值,空集合长度为 0 也是合法值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Details {
    NotLoaded,
    Queued,
    Loading,
    /// 加载失败。不会被自动重试(否则可见范围一报告就重试,失败的会一直重试),要手动刷新。
    Failed,
    Loaded(KeyDetails),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    pub key: Bytes,
    /// 显示用的文本(有损 UTF-8)。
    pub display: String,
    /// 名字不是合法 UTF-8,显示是有损的。
    pub binary: bool,
    pub details: Details,
    /// 加载过程中又点了刷新:当前这次回来后再取一次。
    refresh_pending: bool,
}

impl KeyRow {
    fn new(key: Bytes) -> Self {
        let binary = std::str::from_utf8(&key).is_err();
        Self {
            display: String::from_utf8_lossy(&key).into_owned(),
            key,
            binary,
            details: Details::NotLoaded,
            refresh_pending: false,
        }
    }

    pub fn loaded(&self) -> Option<&KeyDetails> {
        match &self.details {
            Details::Loaded(d) => Some(d),
            _ => None,
        }
    }

    pub fn kind(&self) -> Option<&KeyType> {
        self.loaded().map(|d| &d.kind)
    }

    pub fn length(&self) -> Option<u64> {
        self.loaded().and_then(|d| d.length)
    }

    pub fn ttl(&self) -> Option<Ttl> {
        self.loaded().map(|d| d.ttl)
    }

    /// TTL 为 -2:这个 key 已经不存在了(P-14)。
    pub fn is_deleted(&self) -> bool {
        self.ttl() == Some(Ttl::Missing)
    }

    /// 能不能点开(P-16):只有五种基本类型,Stream 和模块类型只显示。
    pub fn openable(&self) -> bool {
        self.kind().is_some_and(KeyType::openable)
    }
}

/// 类型标签的文字(P-15)。颜色由 GUI 定。没加载或不认识的显示 `None`。
pub fn type_label(kind: Option<&KeyType>) -> &'static str {
    match kind {
        Some(KeyType::String) => "String",
        Some(KeyType::Hash) => "Hash",
        Some(KeyType::List) => "List",
        Some(KeyType::Set) => "Set",
        Some(KeyType::ZSet) => "ZSet",
        Some(KeyType::Stream) => "Stream",
        _ => "None",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteConfirm {
    /// 剪贴板里的文本(`REDIS:` 开头)。
    pub text: String,
    /// 覆盖同名 key。**默认不勾**(v1 后端有这个能力但界面总是覆盖)。
    pub overwrite: bool,
}

/// 调用方给的上下文。
pub struct Ctx<'a> {
    pub id: ConnId,
    pub lang: &'a str,
    pub settings: &'a Settings,
    /// 毫秒,UTC。
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    pub text: String,
    pub toast: bool,
}

#[derive(Debug, Default)]
pub struct Out {
    pub cmds: Vec<Cmd>,
    pub notes: Vec<Note>,
}

impl Out {
    fn note(&mut self, level: Level, text: String, toast: bool) {
        self.notes.push(Note { level, text, toast });
    }
}

#[derive(Debug)]
pub struct KeyList {
    pub pattern: String,
    pub filter: String,
    pub sort: Sort,
    rows: Vec<KeyRow>,
    /// key 名 → 行下标,用来给 SCAN 重复返回的 key 去重(P-12)。
    index: HashMap<Bytes, usize>,
    /// 过滤、排序之后的行下标。
    view: Vec<usize>,
    view_dirty: bool,
    last_view_ms: i64,
    cursor: u64,
    scan: ScanState,
    generation: u64,
    selected: HashSet<Bytes>,
    queue: VecDeque<Bytes>,
    inflight: usize,
    /// 当前库的 key 总数(删除、粘贴后用 DBSIZE 刷新);没有时用连接信息里的。
    pub db_total: Option<u64>,
    /// 确认删除框:要删的数量。
    pub confirm_delete: Option<usize>,
    pub paste: Option<PasteConfirm>,
    pub busy: Option<Busy>,
    pending_delete: Vec<Bytes>,
    copy_report: Option<(usize, Vec<Skipped>)>,
}

impl Default for KeyList {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyList {
    pub fn new() -> Self {
        Self {
            pattern: String::new(),
            filter: String::new(),
            sort: Sort {
                column: SortColumn::Key,
                dir: SortDir::Asc,
            },
            rows: Vec::new(),
            index: HashMap::new(),
            view: Vec::new(),
            view_dirty: false,
            last_view_ms: i64::MIN / 2,
            cursor: 0,
            scan: ScanState::Idle,
            generation: 0,
            selected: HashSet::new(),
            queue: VecDeque::new(),
            inflight: 0,
            db_total: None,
            confirm_delete: None,
            paste: None,
            busy: None,
            pending_delete: Vec::new(),
            copy_report: None,
        }
    }

    // ---- 查询 ----

    pub fn rows(&self) -> &[KeyRow] {
        &self.rows
    }

    /// 过滤、排序之后要显示的行。
    pub fn view(&self) -> impl Iterator<Item = &KeyRow> {
        self.view.iter().map(|&i| &self.rows[i])
    }

    pub fn view_len(&self) -> usize {
        self.view.len()
    }

    pub fn loaded_count(&self) -> usize {
        self.rows.len()
    }

    pub fn scan_state(&self) -> ScanState {
        self.scan
    }

    /// 遮罩上要不要显示 Stop:只有带批次循环的操作(搜索的自动续扫、All)能响应。
    /// 删除、复制、粘贴是单发命令,显示 Stop 是误导。
    pub fn cancellable(&self) -> bool {
        matches!(self.scan, ScanState::Searching | ScanState::LoadingAll)
    }

    /// 还有没加载完的(游标没归零)。
    pub fn has_more(&self) -> bool {
        self.cursor != 0
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn is_selected(&self, key: &[u8]) -> bool {
        self.selected.contains(key)
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// 在做会改变列表的事(扫描或删除、复制、粘贴):界面显示遮罩,其他用户操作忽略。
    pub fn is_busy(&self) -> bool {
        self.scan != ScanState::Idle || self.busy.is_some()
    }

    /// 已加载的百分比。只在 pattern 为空或 `*` 时有意义(P-13);总数取 DBSIZE 的最新值,
    /// 没有就用连接信息里的 `info_total`。
    pub fn loaded_percent(&self, info_total: Option<u64>) -> Option<f64> {
        let p = self.pattern.trim();
        if !(p.is_empty() || p == "*") {
            return None;
        }
        let total = self.db_total.or(info_total)?;
        if total == 0 {
            return None;
        }
        Some((self.rows.len() as f64 / total as f64 * 100.0).min(100.0))
    }

    // ---- 重置 ----

    /// 连接重新建立(第一次打开、切库之后):丢掉所有已加载的东西,旧请求的结果因代号对不上而被丢弃。
    /// 搜索条件、过滤、排序保留。
    pub fn reset(&mut self) {
        self.generation += 1;
        self.rows.clear();
        self.index.clear();
        self.view.clear();
        self.view_dirty = false;
        self.cursor = 0;
        self.scan = ScanState::Idle;
        self.selected.clear();
        self.queue.clear();
        self.inflight = 0;
        self.db_total = None;
        self.confirm_delete = None;
        self.paste = None;
        self.busy = None;
        self.pending_delete.clear();
        self.copy_report = None;
    }

    // ---- update ----

    pub fn update(&mut self, ctx: &Ctx, msg: KeysMsg) -> Out {
        let mut out = Out::default();
        match msg {
            KeysMsg::SetPattern(p) => self.pattern = p,
            KeysMsg::Search => self.search(ctx, &mut out),
            KeysMsg::LoadMore => {
                if self.can_start_scan() && self.cursor != 0 {
                    self.scan = ScanState::LoadingMore;
                    self.request_batch(ctx, &mut out);
                }
            }
            KeysMsg::LoadAll => {
                if self.can_start_scan() && self.cursor != 0 {
                    self.scan = ScanState::LoadingAll;
                    self.request_batch(ctx, &mut out);
                }
            }
            KeysMsg::Stop => {
                if self.cancellable() {
                    self.scan = ScanState::Stopping;
                }
            }
            KeysMsg::SetFilter(f) => {
                self.filter = f;
                self.view_dirty = true;
                self.refresh_view(ctx.now, true);
            }
            KeysMsg::SortBy(column) => {
                self.sort = if self.sort.column == column {
                    Sort {
                        column,
                        dir: match self.sort.dir {
                            SortDir::Asc => SortDir::Desc,
                            SortDir::Desc => SortDir::Asc,
                        },
                    }
                } else {
                    Sort {
                        column,
                        dir: SortDir::Asc,
                    }
                };
                self.view_dirty = true;
                self.refresh_view(ctx.now, true);
            }
            KeysMsg::Visible(keys) => self.on_visible(ctx, keys, &mut out),
            KeysMsg::RefreshDetails(key) => self.refresh_details(ctx, key, &mut out),
            KeysMsg::Select { key, on } => {
                if self.index.contains_key(&key) {
                    if on {
                        self.selected.insert(key);
                    } else {
                        self.selected.remove(&key);
                    }
                }
            }
            KeysMsg::SelectAllFiltered(on) => {
                if on {
                    let keys: Vec<Bytes> = self.view().map(|r| r.key.clone()).collect();
                    self.selected.extend(keys);
                } else {
                    self.selected.clear();
                }
            }
            KeysMsg::DeleteSelected => {
                if !self.is_busy() && !self.selected.is_empty() {
                    self.confirm_delete = Some(self.selected.len());
                }
            }
            KeysMsg::CancelDelete => self.confirm_delete = None,
            KeysMsg::ConfirmDelete => self.confirm_delete(ctx, &mut out),
            KeysMsg::CopySelected => self.copy_selected(ctx, &mut out),
            KeysMsg::PasteRequested => {
                if !self.is_busy() {
                    self.busy = Some(Busy::Pasting);
                    out.cmds
                        .push(Cmd::Platform(PlatformCmd::ReadClipboard { id: ctx.id }));
                }
            }
            KeysMsg::SetPasteOverwrite(on) => {
                if let Some(p) = &mut self.paste {
                    p.overwrite = on;
                }
            }
            KeysMsg::CancelPaste => self.paste = None,
            KeysMsg::ConfirmPaste => {
                if let Some(p) = self.paste.take() {
                    self.busy = Some(Busy::Pasting);
                    out.cmds.push(Cmd::Backend(BackendCmd::PasteKeys {
                        id: ctx.id,
                        text: p.text,
                        overwrite: p.overwrite,
                    }));
                }
            }

            KeysMsg::ScanBatch { generation, result } => {
                self.on_scan_batch(ctx, generation, result, &mut out)
            }
            KeysMsg::DetailsLoaded {
                generation,
                key,
                result,
            } => self.on_details_loaded(ctx, generation, key, result, &mut out),
            KeysMsg::Deleted(result) => self.on_deleted(ctx, result, &mut out),
            KeysMsg::DbSize(result) => {
                // 取不到就保持旧值,不打扰用户
                if let Ok(n) = result {
                    self.db_total = Some(n);
                }
            }
            KeysMsg::Copied(result) => self.on_copied(ctx, result, &mut out),
            KeysMsg::ClipboardWritten(result) => self.on_clipboard_written(ctx, result, &mut out),
            KeysMsg::ClipboardRead(result) => self.on_clipboard_read(ctx, result, &mut out),
            KeysMsg::Pasted(result) => self.on_pasted(ctx, result, &mut out),
        }
        out
    }

    // ---- 扫描 ----

    fn can_start_scan(&self) -> bool {
        !self.is_busy()
    }

    fn pattern_bytes(&self) -> Bytes {
        let p = self.pattern.trim();
        if p.is_empty() {
            Bytes::from_static(b"*")
        } else {
            Bytes::copy_from_slice(p.as_bytes())
        }
    }

    fn request_batch(&mut self, ctx: &Ctx, out: &mut Out) {
        out.cmds.push(Cmd::Backend(BackendCmd::Scan {
            id: ctx.id,
            generation: self.generation,
            pattern: self.pattern_bytes(),
            count: ctx.settings.scan_batch,
            cursor: self.cursor,
        }));
    }

    fn search(&mut self, ctx: &Ctx, out: &mut Out) {
        if !self.can_start_scan() {
            return;
        }
        // 新一次搜索:清空列表、去重集合、选择、详情队列;旧请求的结果因代号对不上被丢弃
        self.generation += 1;
        self.rows.clear();
        self.index.clear();
        self.view.clear();
        self.view_dirty = false;
        self.selected.clear();
        self.queue.clear();
        self.inflight = 0;
        self.cursor = 0;
        self.scan = ScanState::Searching;
        self.request_batch(ctx, out);
    }

    fn ingest(&mut self, keys: Vec<Bytes>) {
        for key in keys {
            // SCAN 迭代过程中可能多次返回同一个 key(包括跨批次),按名字只保留第一次
            if self.index.contains_key(&key) {
                continue;
            }
            self.index.insert(key.clone(), self.rows.len());
            self.rows.push(KeyRow::new(key));
            self.view_dirty = true;
        }
    }

    fn on_scan_batch(
        &mut self,
        ctx: &Ctx,
        generation: u64,
        result: Result<ScanPage<Bytes>>,
        out: &mut Out,
    ) {
        if generation != self.generation || self.scan == ScanState::Idle {
            return; // 迟到的:新搜索、切库、重置之后旧批次的结果
        }
        match result {
            Err(err) => {
                self.finish_scan(ctx);
                out.note(Level::Error, describe(ctx.lang, &err), true);
            }
            Ok(page) => {
                self.ingest(page.items);
                self.cursor = page.cursor;
                let auto = ctx.settings.auto_scan_limit as usize;
                let more = match self.scan {
                    // 空结果但游标没归零:这批没找到,继续扫(不受自动续扫上限限制)。
                    // 然后自动续扫,直到加载够了(0 表示关闭自动续扫)
                    ScanState::Searching => {
                        self.cursor != 0
                            && (self.rows.is_empty() || (auto > 0 && self.rows.len() < auto))
                    }
                    ScanState::LoadingAll => self.cursor != 0,
                    ScanState::LoadingMore | ScanState::Stopping | ScanState::Idle => false,
                };
                if more {
                    self.refresh_view(ctx.now, false);
                    self.request_batch(ctx, out);
                } else {
                    self.finish_scan(ctx);
                }
            }
        }
    }

    fn finish_scan(&mut self, ctx: &Ctx) {
        self.scan = ScanState::Idle;
        self.refresh_view(ctx.now, true);
    }

    // ---- 过滤和排序 ----

    /// 重建过滤、排序后的视图。`force` 为假时按 [`VIEW_REFRESH_MS`] 节流。
    fn refresh_view(&mut self, now: i64, force: bool) {
        if !self.view_dirty {
            return;
        }
        if !force && now - self.last_view_ms < VIEW_REFRESH_MS {
            return;
        }
        let needle = self.filter.trim().to_lowercase();
        let mut view: Vec<usize> = (0..self.rows.len())
            .filter(|&i| needle.is_empty() || self.rows[i].display.to_lowercase().contains(&needle))
            .collect();
        let sort = self.sort;
        view.sort_by(|&a, &b| {
            let ord = compare(&self.rows[a], &self.rows[b], sort.column);
            match sort.dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });
        self.view = view;
        self.view_dirty = false;
        self.last_view_ms = now;
    }

    // ---- 详情 ----

    fn on_visible(&mut self, ctx: &Ctx, keys: Vec<Bytes>, out: &mut Out) {
        if !ctx.settings.show_key_details {
            return; // 没开“显示 key 详情”:不自动加载,要手动刷新
        }
        // 还在排队的(没发出去的)按新的可见范围重排;已经发出去的继续
        for k in self.queue.drain(..) {
            if let Some(&i) = self.index.get(&k)
                && self.rows[i].details == Details::Queued
            {
                self.rows[i].details = Details::NotLoaded;
            }
        }
        for k in keys {
            if let Some(&i) = self.index.get(&k)
                && self.rows[i].details == Details::NotLoaded
            {
                self.rows[i].details = Details::Queued;
                self.queue.push_back(k);
            }
        }
        self.pump_details(ctx, out);
    }

    fn refresh_details(&mut self, ctx: &Ctx, key: Bytes, out: &mut Out) {
        let Some(&i) = self.index.get(&key) else {
            return;
        };
        match self.rows[i].details {
            Details::Loading => self.rows[i].refresh_pending = true,
            Details::Queued => {}
            _ => {
                self.rows[i].details = Details::Queued;
                self.queue.push_front(key);
            }
        }
        self.pump_details(ctx, out);
    }

    /// 同时最多 `POOL_SIZE` 个详情请求(和连接池大小用同一个常量,L-16)。
    fn pump_details(&mut self, ctx: &Ctx, out: &mut Out) {
        while self.inflight < POOL_SIZE {
            let Some(key) = self.queue.pop_front() else {
                break;
            };
            let Some(&i) = self.index.get(&key) else {
                continue;
            };
            self.rows[i].details = Details::Loading;
            self.inflight += 1;
            out.cmds.push(Cmd::Backend(BackendCmd::KeyDetails {
                id: ctx.id,
                generation: self.generation,
                key,
            }));
        }
    }

    fn on_details_loaded(
        &mut self,
        ctx: &Ctx,
        generation: u64,
        key: Bytes,
        result: Result<KeyDetails>,
        out: &mut Out,
    ) {
        if generation != self.generation {
            return; // 列表已经换过了,这个请求属于旧的,连计数都不能动
        }
        self.inflight = self.inflight.saturating_sub(1);
        if let Some(&i) = self.index.get(&key) {
            // 这一行在等待期间可能被删掉了(index 里没有就忽略)
            match result {
                Ok(d) => self.rows[i].details = Details::Loaded(d),
                Err(err) => {
                    tracing::debug!("加载 key 详情失败: {err}");
                    self.rows[i].details = Details::Failed;
                }
            }
            if self.rows[i].refresh_pending {
                self.rows[i].refresh_pending = false;
                self.rows[i].details = Details::Queued;
                self.queue.push_front(key);
            }
            if self.sort.column != SortColumn::Key {
                self.view_dirty = true;
                self.refresh_view(ctx.now, false);
            }
        }
        self.pump_details(ctx, out);
    }

    // ---- 删除、复制、粘贴 ----

    fn selected_in_row_order(&self) -> Vec<Bytes> {
        self.rows
            .iter()
            .filter(|r| self.selected.contains(&r.key))
            .map(|r| r.key.clone())
            .collect()
    }

    fn confirm_delete(&mut self, ctx: &Ctx, out: &mut Out) {
        if self.confirm_delete.take().is_none() || self.is_busy() {
            return;
        }
        let keys = self.selected_in_row_order();
        if keys.is_empty() {
            return;
        }
        self.pending_delete = keys.clone();
        self.busy = Some(Busy::Deleting);
        out.cmds
            .push(Cmd::Backend(BackendCmd::DeleteKeys { id: ctx.id, keys }));
    }

    fn on_deleted(&mut self, ctx: &Ctx, result: Result<u64>, out: &mut Out) {
        if self.busy != Some(Busy::Deleting) {
            return;
        }
        self.busy = None;
        match result {
            Ok(n) => {
                let gone: HashSet<Bytes> = std::mem::take(&mut self.pending_delete)
                    .into_iter()
                    .collect();
                self.remove_rows(&gone);
                self.selected.clear();
                self.refresh_view(ctx.now, true);
                out.note(
                    Level::Success,
                    tr_with(ctx.lang, "keys.deleted", &[("count", &n.to_string())]),
                    true,
                );
                // 删除后用 DBSIZE 刷新 key 数(P-40)
                out.cmds
                    .push(Cmd::Backend(BackendCmd::DbSize { id: ctx.id }));
            }
            Err(err) => {
                self.pending_delete.clear();
                out.note(Level::Error, describe(ctx.lang, &err), true);
            }
        }
    }

    /// 从列表里去掉这些 key(同时维护去重集合、选择、详情队列)。
    fn remove_rows(&mut self, gone: &HashSet<Bytes>) {
        self.rows.retain(|r| !gone.contains(&r.key));
        self.index = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.key.clone(), i))
            .collect();
        self.queue.retain(|k| !gone.contains(k));
        self.selected.retain(|k| !gone.contains(k));
        self.view_dirty = true;
    }

    fn copy_selected(&mut self, ctx: &Ctx, out: &mut Out) {
        if self.is_busy() || self.selected.is_empty() {
            return;
        }
        self.busy = Some(Busy::Copying);
        out.cmds.push(Cmd::Backend(BackendCmd::CopyKeys {
            id: ctx.id,
            keys: self.selected_in_row_order(),
        }));
    }

    fn on_copied(&mut self, ctx: &Ctx, result: Result<CopyOutcome>, out: &mut Out) {
        if self.busy != Some(Busy::Copying) {
            return;
        }
        match result {
            Ok(done) => {
                self.copy_report = Some((done.copied, done.skipped));
                out.cmds.push(Cmd::Platform(PlatformCmd::WriteClipboard {
                    id: ctx.id,
                    text: done.text,
                }));
            }
            Err(err) => {
                self.busy = None;
                out.note(Level::Error, describe(ctx.lang, &err), true);
            }
        }
    }

    fn on_clipboard_written(
        &mut self,
        ctx: &Ctx,
        result: std::result::Result<(), String>,
        out: &mut Out,
    ) {
        if self.busy != Some(Busy::Copying) {
            return;
        }
        self.busy = None;
        let report = self.copy_report.take();
        match result {
            Ok(()) => {
                self.selected.clear();
                let (copied, skipped) = report.unwrap_or((0, Vec::new()));
                out.note(
                    Level::Success,
                    tr_with(ctx.lang, "keys.copied", &[("count", &copied.to_string())]),
                    true,
                );
                if !skipped.is_empty() {
                    let names: Vec<String> = skipped
                        .iter()
                        .take(10)
                        .map(|s| String::from_utf8_lossy(&s.key).into_owned())
                        .collect();
                    out.note(
                        Level::Warn,
                        tr_with(
                            ctx.lang,
                            "keys.copy_skipped",
                            &[
                                ("count", &skipped.len().to_string()),
                                ("keys", &names.join(", ")),
                            ],
                        ),
                        true,
                    );
                }
            }
            // 剪贴板可能暂时打不开(Windows 上被输入法、剪贴板管理器、远程桌面占着):必须提示,不能静默(L-13)
            Err(reason) => out.note(
                Level::Error,
                tr_with(ctx.lang, "err.clipboard_write", &[("reason", &reason)]),
                true,
            ),
        }
    }

    fn on_clipboard_read(
        &mut self,
        ctx: &Ctx,
        result: std::result::Result<String, String>,
        out: &mut Out,
    ) {
        if self.busy != Some(Busy::Pasting) {
            return;
        }
        self.busy = None;
        match result {
            Err(reason) => out.note(
                Level::Error,
                tr_with(ctx.lang, "err.clipboard_read", &[("reason", &reason)]),
                true,
            ),
            Ok(text) if !text.starts_with(CLIPBOARD_PREFIX) => out.note(
                Level::Error,
                describe(ctx.lang, &Error::ClipboardNotRedis),
                true,
            ),
            Ok(text) => {
                self.paste = Some(PasteConfirm {
                    text,
                    overwrite: false,
                });
            }
        }
    }

    fn on_pasted(&mut self, ctx: &Ctx, result: Result<PasteOutcome>, out: &mut Out) {
        if self.busy != Some(Busy::Pasting) {
            return;
        }
        self.busy = None;
        match result {
            Ok(done) => {
                out.note(
                    Level::Success,
                    tr_with(
                        ctx.lang,
                        "keys.pasted",
                        &[("count", &done.count.to_string())],
                    ),
                    true,
                );
                self.refresh_after_paste(ctx, out);
            }
            Err(err) => {
                let partial = matches!(err, Error::RestoreFailed { .. });
                out.note(Level::Error, describe(ctx.lang, &err), true);
                // 逐个写入,中途失败时前面的已经写进去了:刷新才看得到
                if partial {
                    self.refresh_after_paste(ctx, out);
                }
            }
        }
    }

    /// 粘贴完:刷新 key 数,重新搜索(P-42)。
    fn refresh_after_paste(&mut self, ctx: &Ctx, out: &mut Out) {
        out.cmds
            .push(Cmd::Backend(BackendCmd::DbSize { id: ctx.id }));
        self.search(ctx, out);
    }
}

/// 一列的比较。没加载的详情排在最后(升序时),同值再按 key 名排,保证稳定。
fn compare(a: &KeyRow, b: &KeyRow, column: SortColumn) -> Ordering {
    let by_key = || a.key.cmp(&b.key);
    match column {
        SortColumn::Key => by_key(),
        SortColumn::Type => cmp_opt(
            a.kind().map(|k| type_label(Some(k))),
            b.kind().map(|k| type_label(Some(k))),
        )
        .then_with(by_key),
        SortColumn::Length => cmp_opt(a.length(), b.length()).then_with(by_key),
        SortColumn::Ttl => cmp_opt(a.ttl().map(ttl_rank), b.ttl().map(ttl_rank)).then_with(by_key),
    }
}

/// `None`(没加载)排在最后。
fn cmp_opt<T: Ord>(a: Option<T>, b: Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// TTL 的排序:已不存在 < 有过期时间(按剩余秒数)< 永不过期。
fn ttl_rank(t: Ttl) -> (u8, i64) {
    match t {
        Ttl::Missing => (0, 0),
        Ttl::Seconds(n) => (1, n),
        Ttl::Persistent => (2, 0),
    }
}

/// 给 GUI 用:没有翻译时回落英文的小工具,避免 GUI 直接碰 i18n 模块。
pub fn label(lang: &str, key: &str) -> String {
    tr(lang, key)
}

#[cfg(test)]
mod app_tests;
#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;
