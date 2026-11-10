//! 成员表(P-22):Hash、List、Set、ZSet 的成员列表。加载方式和 key 列表一样:一批一条命令,
//! 三档(搜索带自动续扫、More、All),Stop 就是“不再发下一批”,迟到的批次靠代号丢弃。

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use xredis_core::keys::KeyType;
use xredis_core::members::{Member, MemberOp, MemberPage, WriteOp};

use super::display::show;
use crate::keylist::{SortDir, VIEW_REFRESH_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    Idle,
    Searching,
    LoadingMore,
    LoadingAll,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberColumn {
    /// Hash 的 field、List 的下标、Set 和 ZSet 的成员。
    Name,
    /// Hash 和 List 的值、ZSet 的分数;Set 没有这一列。
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberSort {
    pub column: MemberColumn,
    pub dir: SortDir,
}

/// 用户在成员表上的操作和异步结果。
#[derive(Debug)]
pub enum TableMsg {
    Search,
    LoadMore,
    LoadAll,
    Stop,
    SetPattern(String),
    SetFilter(String),
    SortBy(MemberColumn),
    Select { name: Bytes, on: bool },
    SelectAllFiltered(bool),
    DeleteSelected,
    ConfirmDelete,
    CancelDelete,
}

/// 一次扫描要发的参数(`KeyView` 把它变成 `BackendCmd`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRequest {
    pub generation: u64,
    pub pattern: Bytes,
    pub count: u32,
    pub cursor: u64,
}

#[derive(Debug)]
pub struct MemberTable {
    pub kind: KeyType,
    pub key: Bytes,
    pub pattern: String,
    pub filter: String,
    pub sort: MemberSort,
    rows: Vec<Member>,
    /// 名字 → 行下标,SCAN 可能重复返回同一个成员(List 的下标不会重复)。
    index: HashMap<Bytes, usize>,
    view: Vec<usize>,
    view_dirty: bool,
    last_view_ms: i64,
    cursor: u64,
    scan: ScanState,
    generation: u64,
    selected: HashSet<Bytes>,
    /// key 的成员总数(来自 key 详情),算进度用。
    pub length: Option<u64>,
    /// 确认删除框:要删的数量。
    pub confirm_delete: Option<usize>,
}

/// 默认排序:ZSet 按分数降序,其他按名字(List 按下标)升序。
fn default_sort(kind: &KeyType) -> MemberSort {
    match kind {
        KeyType::ZSet => MemberSort {
            column: MemberColumn::Value,
            dir: SortDir::Desc,
        },
        _ => MemberSort {
            column: MemberColumn::Name,
            dir: SortDir::Asc,
        },
    }
}

impl MemberTable {
    pub fn new(key: Bytes, kind: KeyType) -> Self {
        let sort = default_sort(&kind);
        Self {
            kind,
            key,
            pattern: String::new(),
            filter: String::new(),
            sort,
            rows: Vec::new(),
            index: HashMap::new(),
            view: Vec::new(),
            view_dirty: false,
            last_view_ms: 0,
            cursor: 0,
            scan: ScanState::Idle,
            generation: 0,
            selected: HashSet::new(),
            length: None,
            confirm_delete: None,
        }
    }

    pub fn scan_state(&self) -> ScanState {
        self.scan
    }

    pub fn is_scanning(&self) -> bool {
        self.scan != ScanState::Idle
    }

    /// 还有没扫的。
    pub fn has_more(&self) -> bool {
        self.cursor != 0
    }

    pub fn loaded_count(&self) -> usize {
        self.rows.len()
    }

    /// 加载进度 0 到 100。总数不知道或是 0 时是 0。
    pub fn loaded_percent(&self) -> f64 {
        match self.length {
            Some(total) if total > 0 => (self.rows.len() as f64 / total as f64 * 100.0).min(100.0),
            _ => 0.0,
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    pub fn is_selected(&self, name: &Bytes) -> bool {
        self.selected.contains(name)
    }

    /// 过滤、排序之后的行。
    pub fn view(&self) -> impl Iterator<Item = &Member> {
        self.view.iter().map(|&i| &self.rows[i])
    }

    pub fn view_len(&self) -> usize {
        self.view.len()
    }

    pub fn find(&self, name: &Bytes) -> Option<&Member> {
        self.index.get(name).map(|&i| &self.rows[i])
    }

    fn pattern_bytes(&self) -> Bytes {
        let p = self.pattern.trim();
        if p.is_empty() {
            Bytes::from_static(b"*")
        } else {
            Bytes::copy_from_slice(p.as_bytes())
        }
    }

    fn request(&mut self, count: u32) -> ScanRequest {
        ScanRequest {
            generation: self.generation,
            pattern: self.pattern_bytes(),
            count,
            cursor: self.cursor,
        }
    }

    /// 从头扫:清空、代号加一(旧批次作废)。
    pub fn search(&mut self, count: u32) -> ScanRequest {
        self.generation += 1;
        self.rows.clear();
        self.index.clear();
        self.view.clear();
        self.view_dirty = false;
        self.selected.clear();
        self.confirm_delete = None;
        self.cursor = 0;
        self.scan = ScanState::Searching;
        self.request(count)
    }

    pub fn load_more(&mut self, count: u32) -> Option<ScanRequest> {
        if self.is_scanning() || !self.has_more() {
            return None;
        }
        self.scan = ScanState::LoadingMore;
        Some(self.request(count))
    }

    pub fn load_all(&mut self, count: u32) -> Option<ScanRequest> {
        if self.is_scanning() || !self.has_more() {
            return None;
        }
        self.scan = ScanState::LoadingAll;
        Some(self.request(count))
    }

    /// Stop:当前这批回来之后不再发下一批,游标保留。
    pub fn stop(&mut self) {
        if matches!(
            self.scan,
            ScanState::Searching | ScanState::LoadingMore | ScanState::LoadingAll
        ) {
            self.scan = ScanState::Stopping;
        }
    }

    /// 一批回来了。返回要继续扫的下一个请求(没有就是结束了)。
    /// `auto_limit` 是自动续扫上限(0 关闭)。批次的代号对不上就丢弃,返回 `None`。
    pub fn on_batch(
        &mut self,
        generation: u64,
        page: MemberPage,
        count: u32,
        auto_limit: u32,
        now: i64,
    ) -> Option<ScanRequest> {
        if generation != self.generation || self.scan == ScanState::Idle {
            return None;
        }
        for m in page.items {
            let name = m.name();
            if self.index.contains_key(&name) {
                continue;
            }
            self.index.insert(name, self.rows.len());
            self.rows.push(m);
        }
        self.cursor = page.cursor;
        self.view_dirty = true;

        let more = self.cursor != 0;
        let go_on = more
            && match self.scan {
                ScanState::LoadingAll => true,
                ScanState::LoadingMore | ScanState::Stopping | ScanState::Idle => false,
                // 搜索:自动续扫到上限;一个都没扫到(模式匹配不上)时一直扫下去
                ScanState::Searching => {
                    self.rows.is_empty()
                        || (auto_limit > 0 && (self.rows.len() as u32) < auto_limit)
                }
            };
        if go_on {
            self.refresh_view(now, false);
            Some(self.request(count))
        } else {
            self.scan = ScanState::Idle;
            self.refresh_view(now, true);
            None
        }
    }

    /// 扫描出错:停下,已加载的留着。代号对不上返回 `false`(迟到的错误不报)。
    pub fn on_scan_error(&mut self, generation: u64) -> bool {
        if generation != self.generation || self.scan == ScanState::Idle {
            return false;
        }
        self.scan = ScanState::Idle;
        self.refresh_view(0, true);
        true
    }

    // ---- 过滤、排序、选择 ----

    pub fn set_filter(&mut self, text: String, now: i64) {
        self.filter = text;
        self.view_dirty = true;
        self.refresh_view(now, true);
    }

    pub fn sort_by(&mut self, column: MemberColumn, now: i64) {
        self.sort = if self.sort.column == column {
            MemberSort {
                column,
                dir: match self.sort.dir {
                    SortDir::Asc => SortDir::Desc,
                    SortDir::Desc => SortDir::Asc,
                },
            }
        } else {
            MemberSort {
                column,
                dir: SortDir::Asc,
            }
        };
        self.view_dirty = true;
        self.refresh_view(now, true);
    }

    pub fn select(&mut self, name: Bytes, on: bool) {
        if self.index.contains_key(&name) {
            if on {
                self.selected.insert(name);
            } else {
                self.selected.remove(&name);
            }
        }
    }

    /// 全选 = 当前过滤后已加载的全部行。
    pub fn select_all_filtered(&mut self, on: bool) {
        if on {
            let names: Vec<Bytes> = self.view().map(Member::name).collect();
            self.selected.extend(names);
        } else {
            self.selected.clear();
        }
    }

    /// 删除选中成员要执行的写操作。没选中返回 `None`。
    pub fn delete_op(&self) -> Option<WriteOp> {
        if self.selected.is_empty() {
            return None;
        }
        let key = self.key.clone();
        let names: Vec<Bytes> = self.selected.iter().cloned().collect();
        let op = match self.kind {
            KeyType::Hash => MemberOp::HashDelete(names),
            KeyType::Set => MemberOp::SetRemove(names),
            KeyType::ZSet => MemberOp::ZRemove(names),
            KeyType::List => MemberOp::ListRemove(
                names
                    .iter()
                    .filter_map(|n| std::str::from_utf8(n).ok()?.parse::<i64>().ok())
                    .collect(),
            ),
            _ => return None,
        };
        Some(WriteOp::Member { key, op })
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    fn matches_filter(&self, m: &Member, needle: &str) -> bool {
        let hit = |b: &Bytes| show(b).text.to_lowercase().contains(needle);
        match m {
            Member::Hash { field, value } => hit(field) || hit(value),
            Member::List { index, value } => index.to_string().contains(needle) || hit(value),
            Member::Set { member } => hit(member),
            Member::ZSet { member, score } => hit(member) || score.to_string().contains(needle),
        }
    }

    /// 刷新过滤排序后的视图。按时间节流(渐进加载时每批都重排是 O(N²)),扫描结束时强制刷新。
    fn refresh_view(&mut self, now: i64, force: bool) {
        if !self.view_dirty {
            return;
        }
        if !force && now - self.last_view_ms < VIEW_REFRESH_MS {
            return;
        }
        self.last_view_ms = now;
        self.view_dirty = false;
        let needle = self.filter.trim().to_lowercase();
        let mut view: Vec<usize> = (0..self.rows.len())
            .filter(|&i| needle.is_empty() || self.matches_filter(&self.rows[i], &needle))
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
    }

    /// 给“打开表格时的第一次显示”和测试用:强制刷新。
    pub fn refresh_now(&mut self, now: i64) {
        self.view_dirty = true;
        self.refresh_view(now, true);
    }
}

fn compare(a: &Member, b: &Member, column: MemberColumn) -> Ordering {
    let by_name = || a.name().cmp(&b.name());
    match (a, b, column) {
        (Member::List { index: x, .. }, Member::List { index: y, .. }, MemberColumn::Name) => {
            x.cmp(y)
        }
        (Member::ZSet { score: x, .. }, Member::ZSet { score: y, .. }, MemberColumn::Value) => {
            x.total_cmp(y).then_with(by_name)
        }
        (Member::Hash { value: x, .. }, Member::Hash { value: y, .. }, MemberColumn::Value)
        | (Member::List { value: x, .. }, Member::List { value: y, .. }, MemberColumn::Value) => {
            x.cmp(y).then_with(by_name)
        }
        _ => by_name(),
    }
}
