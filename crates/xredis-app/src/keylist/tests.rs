//! `KeyList` 的测试(不经过 App):喂 `KeysMsg`,检查状态和返回的命令。
//! 移植 v1 `keyListVMLoadAll.test.ts` 的全部场景,再加 R-07a 的新规则。
//! 覆盖 P-10 至 P-16、P-40 至 P-42;L-13、L-16。

use xredis_core::Error;
use xredis_core::error::{SkipReason, Skipped};
use xredis_core::transfer::PastePreview;

use super::*;

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

struct H {
    list: KeyList,
    settings: Settings,
    now: i64,
    id: ConnId,
}

impl H {
    fn new() -> Self {
        Self {
            list: KeyList::new(),
            settings: Settings::default(),
            now: 1_000_000,
            id: ConnId::new(),
        }
    }

    fn send(&mut self, msg: KeysMsg) -> Out {
        let ctx = Ctx {
            id: self.id,
            lang: "en",
            settings: &self.settings,
            now: self.now,
        };
        self.list.update(&ctx, msg)
    }

    /// 喂一批扫描结果。
    fn batch(&mut self, generation: u64, items: &[&str], cursor: u64) -> Out {
        self.send(KeysMsg::ScanBatch {
            generation,
            result: Ok(page(items, cursor)),
        })
    }

    /// 搜索并喂回第一批,返回(这一批请求的代号,喂回之后的输出)。
    fn search_with(&mut self, items: &[&str], cursor: u64) -> (u64, Out) {
        let out = self.send(KeysMsg::Search);
        let g = scans(&out).first().expect("a scan request").0;
        let out = self.batch(g, items, cursor);
        (g, out)
    }
}

fn k(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

fn page(items: &[&str], cursor: u64) -> ScanPage<Bytes> {
    ScanPage {
        items: items.iter().map(|s| k(s)).collect(),
        cursor,
    }
}

/// (代号, pattern, count, cursor)
fn scans(out: &Out) -> Vec<(u64, Bytes, u32, u64)> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Backend(BackendCmd::Scan {
                generation,
                pattern,
                count,
                cursor,
                ..
            }) => Some((*generation, pattern.clone(), *count, *cursor)),
            _ => None,
        })
        .collect()
}

fn detail_reqs(out: &Out) -> Vec<(u64, Bytes)> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Backend(BackendCmd::KeyDetails {
                generation, key, ..
            }) => Some((*generation, key.clone())),
            _ => None,
        })
        .collect()
}

fn backend(out: &Out) -> Vec<&BackendCmd> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Backend(b) => Some(b),
            _ => None,
        })
        .collect()
}

fn platform(out: &Out) -> Vec<&PlatformCmd> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Platform(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn names(list: &KeyList) -> Vec<String> {
    list.view().map(|r| r.display.clone()).collect()
}

fn det(kind: KeyType, length: Option<u64>, ttl: Ttl) -> KeyDetails {
    KeyDetails { kind, length, ttl }
}

fn loaded_ok(g: u64, key: &str, d: KeyDetails) -> KeysMsg {
    KeysMsg::DetailsLoaded {
        generation: g,
        key: k(key),
        result: Ok(d),
    }
}

fn last_note(out: &Out) -> &Note {
    out.notes.last().expect("a note")
}

// ---------------------------------------------------------------------------
// 搜索:参数、三档、自动续扫(P-10、P-11)
// ---------------------------------------------------------------------------

/// P-10:pattern 为空用 `*`(core 不带 MATCH);批次大小用设置里的单批数量。
#[test]
fn p10_search_uses_star_for_an_empty_pattern_and_the_configured_batch_size() {
    let mut h = H::new();
    h.settings.scan_batch = 300;
    let out = h.send(KeysMsg::Search);
    let s = scans(&out);
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].1.as_ref(), s[0].2, s[0].3), (&b"*"[..], 300, 0));
    assert_eq!(h.list.scan_state(), ScanState::Searching);

    let mut h = H::new();
    h.send(KeysMsg::SetPattern("  user:* ".into()));
    let out = h.send(KeysMsg::Search);
    assert_eq!(
        scans(&out)[0].1.as_ref(),
        b"user:*",
        "the pattern is trimmed"
    );
}

/// P-12:SCAN 会重复返回同一个 key(同一批里、跨批次),按名字去重,只保留第一次。
#[test]
fn p12_duplicate_keys_are_ingested_once() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 100;
    let (g, out) = h.search_with(&["a", "b", "a"], 7);
    assert_eq!(scans(&out).len(), 1, "the cursor is not 0: keep scanning");
    h.batch(g, &["b", "c", "c"], 0);
    assert_eq!(names(&h.list), vec!["a", "b", "c"]);
    assert_eq!(h.list.loaded_count(), 3);
}

/// P-11 搜索:加载够自动续扫上限就停,游标保留(还能 More)。
#[test]
fn p11_search_auto_scans_until_the_limit_then_stops_with_the_cursor_kept() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 5;
    let (g, out) = h.search_with(&["a", "b"], 11);
    assert_eq!(scans(&out)[0].3, 11, "continues from the returned cursor");
    let out = h.batch(g, &["c", "d"], 12);
    assert_eq!(scans(&out).len(), 1, "4 < 5: continue");
    let out = h.batch(g, &["e", "f"], 13);
    assert!(scans(&out).is_empty(), "6 >= 5: stop");
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.cursor(), 13);
    assert!(h.list.has_more());
    assert_eq!(h.list.loaded_count(), 6);
}

/// P-11:自动续扫上限为 0 表示关闭;但第一批是空的、游标没归零,说明还没找到,仍然继续扫。
#[test]
fn p11_auto_scan_off_stops_after_the_first_non_empty_batch_but_empty_batches_continue() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 0;
    let (g, out) = h.search_with(&[], 5);
    assert_eq!(
        scans(&out).len(),
        1,
        "an empty batch with a live cursor: not found yet, keep going"
    );
    let out = h.batch(g, &[], 9);
    assert_eq!(scans(&out).len(), 1);
    let out = h.batch(g, &["found"], 14);
    assert!(
        scans(&out).is_empty(),
        "auto-scan is off: stop after the first batch with data"
    );
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.cursor(), 14);
}

/// 游标归零就结束,不管上限。
#[test]
fn search_ends_when_the_cursor_reaches_zero() {
    let mut h = H::new();
    let (_, out) = h.search_with(&["a"], 0);
    assert!(scans(&out).is_empty());
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert!(!h.list.has_more());
}

/// 一直没有匹配、游标走到 0:结束,列表为空。
#[test]
fn search_with_no_matches_ends_with_an_empty_list() {
    let mut h = H::new();
    let (g, out) = h.search_with(&[], 3);
    assert_eq!(scans(&out).len(), 1);
    let out = h.batch(g, &[], 0);
    assert!(scans(&out).is_empty());
    assert_eq!(h.list.loaded_count(), 0);
    assert_eq!(h.list.scan_state(), ScanState::Idle);
}

/// P-11 More:只扫一批,用当前游标;游标为 0 或正在扫描时不动。
#[test]
fn p11_load_more_scans_exactly_one_batch() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 0;
    let (g, _) = h.search_with(&["a"], 20);
    let out = h.send(KeysMsg::LoadMore);
    let s = scans(&out);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].3, 20);
    assert_eq!(h.list.scan_state(), ScanState::LoadingMore);
    assert!(!h.list.cancellable(), "a single batch cannot be stopped");
    assert!(
        h.send(KeysMsg::LoadMore).cmds.is_empty(),
        "already scanning"
    );
    let out = h.batch(g, &["b"], 40);
    assert!(
        scans(&out).is_empty(),
        "More stops after its one batch even though the cursor is live"
    );
    assert_eq!(h.list.cursor(), 40);
    assert_eq!(h.list.scan_state(), ScanState::Idle);

    // 游标为 0:没有更多可加载
    let mut h = H::new();
    h.search_with(&["a"], 0);
    assert!(h.send(KeysMsg::LoadMore).cmds.is_empty());
    assert!(h.send(KeysMsg::LoadAll).cmds.is_empty());
}

/// P-11 All:一直扫到游标归零,不设上限。(v1:正常扫到底时循环自行结束)
#[test]
fn p11_load_all_scans_to_the_end_ignoring_the_auto_scan_limit() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 2;
    let (g, _) = h.search_with(&["a", "b"], 10);
    // 搜索已经因为 2 >= 2 停了
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    let out = h.send(KeysMsg::LoadAll);
    assert_eq!(scans(&out)[0].3, 10);
    assert_eq!(h.list.scan_state(), ScanState::LoadingAll);
    assert!(h.list.cancellable());
    let out = h.batch(g, &["c", "d"], 20);
    assert_eq!(scans(&out).len(), 1, "beyond the limit: All does not care");
    let out = h.batch(g, &["e"], 0);
    assert!(scans(&out).is_empty());
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.loaded_count(), 5);
    assert!(!h.list.cancellable());
}

// ---------------------------------------------------------------------------
// Stop(移植 v1 keyListVMLoadAll.test.ts)
// ---------------------------------------------------------------------------

/// v1「stopLoadAll 会让循环停下来,而不是扫到底」+「停止后 cursor 保留,可以接着扫」。
#[test]
fn stop_ends_the_all_loop_and_keeps_the_cursor() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 0;
    let (g, _) = h.search_with(&["a"], 10);
    h.send(KeysMsg::LoadAll);
    let out = h.send(KeysMsg::Stop);
    assert!(
        out.cmds.is_empty(),
        "Stop sends nothing: it only stops asking for more"
    );
    assert_eq!(h.list.scan_state(), ScanState::Stopping);
    assert!(!h.list.cancellable(), "already stopping");
    // 在途的这一批回来:收下,但不再发下一批
    let out = h.batch(g, &["b", "c"], 30);
    assert!(scans(&out).is_empty(), "no further batch after Stop");
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.cursor(), 30, "the cursor is kept");
    assert_eq!(
        h.list.loaded_count(),
        3,
        "the in-flight batch was still ingested"
    );
    assert!(h.list.has_more());
}

/// v1「Stop 之后仍能重新加载(取消标志每次任务开头复位)」:不能出现“点过一次 Stop,All 再也不工作”。
#[test]
fn after_stop_more_and_all_work_again() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 0;
    let (g, _) = h.search_with(&["a"], 10);
    h.send(KeysMsg::LoadAll);
    h.send(KeysMsg::Stop);
    h.batch(g, &["b"], 20);

    let out = h.send(KeysMsg::LoadMore);
    assert_eq!(scans(&out)[0].3, 20, "continues from the kept cursor");
    h.batch(g, &["c"], 30);
    let out = h.send(KeysMsg::LoadAll);
    assert_eq!(scans(&out).len(), 1);
    let out = h.batch(g, &["d"], 40);
    assert_eq!(
        scans(&out).len(),
        1,
        "a stale Stop must not end this All early"
    );
    let out = h.batch(g, &["e"], 0);
    assert!(scans(&out).is_empty());
    assert_eq!(h.list.loaded_count(), 5);
}

/// v1「search 的自动续扫也能被取消」。
#[test]
fn stop_also_cancels_the_auto_scan_after_a_search() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 1000;
    let (g, out) = h.search_with(&["a"], 10);
    assert_eq!(scans(&out).len(), 1, "auto-scan continues");
    assert!(h.list.cancellable());
    h.send(KeysMsg::Stop);
    let out = h.batch(g, &["b"], 20);
    assert!(scans(&out).is_empty());
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.cursor(), 20);
    assert_eq!(h.list.loaded_count(), 2);
}

/// v2 的改进:搜索时“结果为空就继续扫”这一段 v1 不响应 Stop(可以在一个很大的空 keyspace 里转很久),
/// v2 响应。
#[test]
fn stop_also_interrupts_the_empty_batch_continuation() {
    let mut h = H::new();
    let (g, out) = h.search_with(&[], 5);
    assert_eq!(scans(&out).len(), 1, "empty and not done: keep going");
    h.send(KeysMsg::Stop);
    let out = h.batch(g, &[], 9);
    assert!(
        scans(&out).is_empty(),
        "stopped even though nothing was found yet"
    );
    assert_eq!(h.list.cursor(), 9);
    assert_eq!(h.list.scan_state(), ScanState::Idle);
}

/// v1「只有 search / loadAll 期间 cancellable 为真」;Idle 时 Stop 什么也不做。
#[test]
fn only_search_and_all_are_cancellable() {
    let mut h = H::new();
    assert!(!h.list.cancellable());
    assert!(h.send(KeysMsg::Stop).cmds.is_empty());
    assert_eq!(
        h.list.scan_state(),
        ScanState::Idle,
        "Stop while idle must not leave a stale stop flag"
    );
    h.send(KeysMsg::Search);
    assert!(h.list.cancellable());
}

/// v1「不可取消的操作不会打开 cancellable」:删除、复制、粘贴是单发命令,显示 Stop 是误导。
#[test]
fn delete_copy_and_paste_are_not_cancellable() {
    let mut h = H::new();
    let (_, _) = h.search_with(&["a"], 0);
    h.send(KeysMsg::Select {
        key: k("a"),
        on: true,
    });
    h.send(KeysMsg::CopySelected);
    assert_eq!(h.list.busy, Some(Busy::Copying));
    assert!(!h.list.cancellable());
}

/// v1「抛错时 cancellable 也要复位,否则 Stop 会挂在后续所有遮罩上」。
#[test]
fn a_scan_error_resets_the_state_and_reports() {
    let mut h = H::new();
    let out = h.send(KeysMsg::Search);
    let g = scans(&out)[0].0;
    assert!(h.list.cancellable());
    let out = h.send(KeysMsg::ScanBatch {
        generation: g,
        result: Err(Error::Timeout("scan took too long".into())),
    });
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert!(!h.list.cancellable());
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(last_note(&out).text.contains("scan took too long"));
    // 之后还能再搜索
    assert_eq!(scans(&h.send(KeysMsg::Search)).len(), 1);
}

// ---------------------------------------------------------------------------
// 过期的结果、重置
// ---------------------------------------------------------------------------

/// 新搜索之后,旧搜索迟到的批次要丢弃(代号对不上),不能混进新列表。
#[test]
fn a_late_batch_from_an_older_search_is_dropped() {
    let mut h = H::new();
    let out = h.send(KeysMsg::Search);
    let old = scans(&out)[0].0;
    h.send(KeysMsg::Stop);
    h.batch(old, &["a"], 5); // 旧搜索收尾,回到 Idle
    let out = h.send(KeysMsg::Search);
    let new = scans(&out)[0].0;
    assert_ne!(old, new);
    let out = h.batch(old, &["stale1", "stale2"], 9);
    assert!(out.cmds.is_empty());
    assert_eq!(
        h.list.loaded_count(),
        0,
        "the stale batch must not be ingested"
    );
    assert_eq!(
        h.list.scan_state(),
        ScanState::Searching,
        "and must not end the current scan"
    );
    h.batch(new, &["fresh"], 0);
    assert_eq!(names(&h.list), vec!["fresh"]);
}

/// 新一次搜索清空列表、去重集合和选择。
#[test]
fn a_new_search_clears_rows_selection_and_the_dedup_set() {
    let mut h = H::new();
    h.search_with(&["a", "b"], 0);
    h.send(KeysMsg::Select {
        key: k("a"),
        on: true,
    });
    assert_eq!(h.list.selected_count(), 1);
    let out = h.send(KeysMsg::Search);
    assert_eq!(h.list.loaded_count(), 0);
    assert_eq!(h.list.selected_count(), 0);
    let g = scans(&out)[0].0;
    h.batch(g, &["a"], 0);
    assert_eq!(
        names(&h.list),
        vec!["a"],
        "'a' is a new key again after the list was cleared"
    );
}

/// 搜索进行中再点搜索、More、All:忽略(遮罩期间不能叠加操作)。
#[test]
fn scan_requests_are_ignored_while_scanning() {
    let mut h = H::new();
    h.send(KeysMsg::Search);
    assert!(h.send(KeysMsg::Search).cmds.is_empty());
    assert!(h.send(KeysMsg::LoadMore).cmds.is_empty());
    assert!(h.send(KeysMsg::LoadAll).cmds.is_empty());
}

/// 连接重新建立(切库、重连):列表清空、旧请求的结果丢弃;搜索条件、过滤、排序保留。
#[test]
fn reset_clears_the_list_but_keeps_pattern_filter_and_sort() {
    let mut h = H::new();
    h.send(KeysMsg::SetPattern("user:*".into()));
    h.send(KeysMsg::SetFilter("abc".into()));
    h.send(KeysMsg::SortBy(SortColumn::Length));
    let out = h.send(KeysMsg::Search);
    let g = scans(&out)[0].0;
    h.list.reset();
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    assert_eq!(h.list.loaded_count(), 0);
    h.batch(g, &["late"], 0);
    assert_eq!(
        h.list.loaded_count(),
        0,
        "a result from before the reset is dropped"
    );
    assert_eq!(h.list.pattern, "user:*");
    assert_eq!(h.list.filter, "abc");
    assert_eq!(h.list.sort.column, SortColumn::Length);
}

// ---------------------------------------------------------------------------
// 过滤、排序、显示(P-13)
// ---------------------------------------------------------------------------

/// P-13:列头过滤在已加载的数据里做,不区分大小写,按子串,首尾空白不算;不访问后端。
#[test]
fn p13_filter_is_a_case_insensitive_substring_over_loaded_keys() {
    let mut h = H::new();
    h.search_with(&["User:1", "user:2", "order:1", "会话:中文", "ORDER:9"], 0);
    assert_eq!(h.list.view_len(), 5);
    let out = h.send(KeysMsg::SetFilter("  USER ".into()));
    assert!(out.cmds.is_empty(), "filtering never goes to the server");
    assert_eq!(names(&h.list), vec!["User:1", "user:2"]);
    h.send(KeysMsg::SetFilter("中文".into()));
    assert_eq!(names(&h.list), vec!["会话:中文"]);
    h.send(KeysMsg::SetFilter("order".into()));
    assert_eq!(
        names(&h.list),
        vec!["ORDER:9", "order:1"],
        "default sort is by key, byte order"
    );
    h.send(KeysMsg::SetFilter("   ".into()));
    assert_eq!(h.list.view_len(), 5, "a blank filter shows everything");
    h.send(KeysMsg::SetFilter("nothing-matches".into()));
    assert_eq!(h.list.view_len(), 0);
    assert_eq!(
        h.list.loaded_count(),
        5,
        "filtering does not drop loaded keys"
    );
}

/// P-13:Key 列默认升序;点列头换方向,换列从升序开始。
#[test]
fn p13_default_sort_is_key_ascending_and_headers_toggle() {
    let mut h = H::new();
    h.search_with(&["b", "c", "a"], 0);
    assert_eq!(
        h.list.sort,
        Sort {
            column: SortColumn::Key,
            dir: SortDir::Asc
        }
    );
    assert_eq!(names(&h.list), vec!["a", "b", "c"]);
    h.send(KeysMsg::SortBy(SortColumn::Key));
    assert_eq!(names(&h.list), vec!["c", "b", "a"]);
    h.send(KeysMsg::SortBy(SortColumn::Key));
    assert_eq!(names(&h.list), vec!["a", "b", "c"]);
    h.send(KeysMsg::SortBy(SortColumn::Length));
    assert_eq!(
        h.list.sort,
        Sort {
            column: SortColumn::Length,
            dir: SortDir::Asc
        }
    );
}

/// 按详情列排序:没加载的排最后(升序),同值按 key 名;TTL:已不存在 < 有 TTL(按剩余秒数)< 永不过期。
#[test]
fn sorting_by_detail_columns_puts_unloaded_rows_last() {
    let mut h = H::new();
    let (g, _) = h.search_with(&["a", "b", "c", "d", "e"], 0);
    h.send(loaded_ok(
        g,
        "a",
        det(KeyType::Hash, Some(30), Ttl::Persistent),
    ));
    h.send(loaded_ok(
        g,
        "b",
        det(KeyType::String, Some(5), Ttl::Seconds(50)),
    ));
    h.send(loaded_ok(g, "c", det(KeyType::List, Some(5), Ttl::Missing)));
    h.send(loaded_ok(
        g,
        "d",
        det(KeyType::Set, Some(0), Ttl::Seconds(9)),
    ));
    // e 没加载
    h.send(KeysMsg::SortBy(SortColumn::Length));
    assert_eq!(
        names(&h.list),
        vec!["d", "b", "c", "a", "e"],
        "length 0, 5, 5(by name), 30, unloaded"
    );
    h.send(KeysMsg::SortBy(SortColumn::Ttl));
    assert_eq!(
        names(&h.list),
        vec!["c", "d", "b", "a", "e"],
        "missing < 9s < 50s < persistent < unloaded"
    );
    h.send(KeysMsg::SortBy(SortColumn::Type));
    assert_eq!(
        names(&h.list),
        vec!["a", "c", "d", "b", "e"],
        "Hash, List, Set, String, unloaded"
    );
}

/// 二进制名字:显示有损,并标记出来(界面据此提示、只读)。
#[test]
fn binary_key_names_are_displayed_lossily_and_flagged() {
    let mut h = H::new();
    let out = h.send(KeysMsg::Search);
    let g = scans(&out)[0].0;
    h.send(KeysMsg::ScanBatch {
        generation: g,
        result: Ok(ScanPage {
            items: vec![Bytes::from_static(&[b'k', 0xff, b'x']), k("plain")],
            cursor: 0,
        }),
    });
    let rows: Vec<_> = h.list.view().collect();
    let bin = rows.iter().find(|r| r.binary).expect("a binary row");
    assert_eq!(bin.display, "k\u{fffd}x");
    assert_eq!(
        &bin.key[..],
        &[b'k', 0xff, b'x'],
        "the real bytes are kept for deleting and TTL"
    );
    assert!(!rows.iter().find(|r| r.display == "plain").unwrap().binary);
    h.send(KeysMsg::SetFilter("\u{fffd}".into()));
    assert_eq!(h.list.view_len(), 1);
}

/// P-13:已加载百分比只在 pattern 为空或 `*` 时显示;总数优先用 DBSIZE 的最新值;没有总数或为 0 不显示;封顶 100。
#[test]
fn p13_loaded_percent() {
    let mut h = H::new();
    h.search_with(&["a", "b", "c", "d"], 7);
    assert_eq!(h.list.loaded_percent(Some(8)), Some(50.0));
    assert_eq!(h.list.loaded_percent(None), None, "no total known");
    assert_eq!(h.list.loaded_percent(Some(0)), None);
    h.list.db_total = Some(16);
    assert_eq!(
        h.list.loaded_percent(Some(8)),
        Some(25.0),
        "the DBSIZE value wins"
    );
    assert_eq!(h.list.loaded_percent(Some(2)), Some(25.0));
    h.list.db_total = Some(2);
    assert_eq!(
        h.list.loaded_percent(None),
        Some(100.0),
        "capped at 100 when keys were added meanwhile"
    );
    h.send(KeysMsg::SetPattern("user:*".into()));
    assert_eq!(
        h.list.loaded_percent(Some(8)),
        None,
        "a real pattern: the percentage means nothing"
    );
    h.send(KeysMsg::SetPattern("*".into()));
    assert!(h.list.loaded_percent(Some(8)).is_some());
    h.send(KeysMsg::SetPattern("  ".into()));
    assert!(h.list.loaded_percent(Some(8)).is_some());
}

/// 列表刷新节流:扫到底时每批都重排会是 O(N²)。节流间隔内的批次不重建视图,扫描结束时一定重建。
#[test]
fn the_view_refresh_is_throttled_during_a_long_scan_but_always_final() {
    let mut h = H::new();
    h.settings.auto_scan_limit = 0;
    let (g, _) = h.search_with(&["a"], 10);
    assert_eq!(h.list.view_len(), 1, "the first batch shows right away");
    h.send(KeysMsg::LoadAll);
    h.now += 50; // 不到 300ms
    h.batch(g, &["b"], 20);
    assert_eq!(
        h.list.view_len(),
        1,
        "within 300ms: the view is not rebuilt yet"
    );
    assert_eq!(h.list.loaded_count(), 2, "but the data is all there");
    h.now += 400;
    h.batch(g, &["c"], 30);
    assert_eq!(h.list.view_len(), 3, "after the interval it catches up");
    h.now += 1;
    h.batch(g, &["d"], 0);
    assert_eq!(h.list.view_len(), 4, "the last batch always refreshes");
}

// ---------------------------------------------------------------------------
// 详情加载(P-14、L-16)
// ---------------------------------------------------------------------------

fn many(h: &mut H, n: usize) -> (u64, Vec<Bytes>) {
    let keys: Vec<String> = (0..n).map(|i| format!("key{i:03}")).collect();
    let refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    let (g, _) = h.search_with(&refs, 0);
    (g, keys.iter().map(|s| k(s)).collect())
}

/// L-16:详情请求同时最多 `POOL_SIZE` 个(和连接池大小同一个常量);一个回来再发下一个;任何时刻不超限。
#[test]
fn l16_detail_requests_are_capped_at_the_pool_size() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 40);
    let out = h.send(KeysMsg::Visible(keys.clone()));
    let first = detail_reqs(&out);
    assert_eq!(first.len(), POOL_SIZE, "12 at once, not 40");
    assert_eq!(first[0].1, keys[0], "in the order the GUI reported them");

    let mut inflight: Vec<Bytes> = first.into_iter().map(|(_, k)| k).collect();
    let mut requested = inflight.len();
    let mut max = inflight.len();
    while let Some(done) = inflight.pop() {
        let name = String::from_utf8(done.to_vec()).unwrap();
        let out = h.send(loaded_ok(
            g,
            &name,
            det(KeyType::String, Some(1), Ttl::Persistent),
        ));
        let next = detail_reqs(&out);
        assert!(next.len() <= 1, "one slot freed, at most one new request");
        requested += next.len();
        inflight.extend(next.into_iter().map(|(_, k)| k));
        max = max.max(inflight.len());
    }
    assert_eq!(requested, 40, "every visible key got exactly one request");
    assert!(max <= POOL_SIZE);
    assert!(h.list.view().all(|r| r.loaded().is_some()));
}

/// 同一个 key 的请求合并:已经排队、加载中、加载完的,再次报告可见也不重复请求。
#[test]
fn detail_requests_for_the_same_key_are_merged() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 5);
    let out = h.send(KeysMsg::Visible(keys.clone()));
    assert_eq!(detail_reqs(&out).len(), 5);
    let out = h.send(KeysMsg::Visible(keys.clone()));
    assert!(detail_reqs(&out).is_empty(), "already loading");
    h.send(loaded_ok(
        g,
        "key000",
        det(KeyType::String, Some(1), Ttl::Persistent),
    ));
    let out = h.send(KeysMsg::Visible(keys));
    assert!(
        detail_reqs(&out).is_empty(),
        "key000 is loaded, the rest still loading"
    );
}

/// L-16:“已加载”是显式状态,不能拿 -1 或 0 推断:TTL 永不过期(-1)和空集合(长度 0)都是合法的已加载值,
/// 不会被当成没加载而反复请求。
#[test]
fn l16_loaded_is_an_explicit_state_not_inferred_from_minus_one_or_zero() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 3);
    h.send(KeysMsg::Visible(keys.clone()));
    h.send(loaded_ok(
        g,
        "key000",
        det(KeyType::Set, Some(0), Ttl::Persistent),
    ));
    let row = h.list.view().next().unwrap();
    assert_eq!(row.length(), Some(0), "length 0 is a loaded value");
    assert_eq!(row.ttl(), Some(Ttl::Persistent), "ttl -1 is a loaded value");
    assert!(row.loaded().is_some());
    let out = h.send(KeysMsg::Visible(keys));
    assert!(detail_reqs(&out).is_empty(), "nothing is re-requested");
    // 没加载的行:长度和 TTL 都是 None,而不是 0 或 -1
    let unloaded = h.list.view().nth(2).unwrap();
    assert_eq!(
        (unloaded.length(), unloaded.ttl(), unloaded.kind()),
        (None, None, None)
    );
}

/// P-14:TTL 为 -2 的行标记为已删除;类型 None 和模块类型、Stream 不能打开(P-16)。
#[test]
fn p14_missing_ttl_marks_the_row_deleted_and_p16_only_basic_types_open() {
    let mut h = H::new();
    let (g, _) = h.search_with(&["gone", "s", "stream", "json"], 0);
    h.send(KeysMsg::Visible(vec![
        k("gone"),
        k("s"),
        k("stream"),
        k("json"),
    ]));
    h.send(loaded_ok(g, "gone", det(KeyType::None, None, Ttl::Missing)));
    h.send(loaded_ok(
        g,
        "s",
        det(KeyType::String, Some(3), Ttl::Persistent),
    ));
    h.send(loaded_ok(
        g,
        "stream",
        det(KeyType::Stream, Some(9), Ttl::Persistent),
    ));
    h.send(loaded_ok(
        g,
        "json",
        det(KeyType::Other("ReJSON-RL".into()), None, Ttl::Persistent),
    ));
    let by = |n: &str| h.list.view().find(|r| r.display == n).unwrap().clone();
    assert!(by("gone").is_deleted() && !by("gone").openable());
    assert!(by("s").openable() && !by("s").is_deleted());
    assert!(
        !by("stream").openable(),
        "Stream is displayed but cannot be opened"
    );
    assert!(
        !by("json").openable(),
        "module types are displayed but cannot be opened"
    );
    assert_eq!(type_label(by("stream").kind()), "Stream");
    assert_eq!(
        type_label(by("json").kind()),
        "None",
        "unknown types are labelled None"
    );
    assert_eq!(type_label(None), "None");
}

/// P-15:类型标签的文字。
#[test]
fn p15_type_labels() {
    for (kind, label) in [
        (KeyType::String, "String"),
        (KeyType::Hash, "Hash"),
        (KeyType::List, "List"),
        (KeyType::Set, "Set"),
        (KeyType::ZSet, "ZSet"),
        (KeyType::Stream, "Stream"),
        (KeyType::None, "None"),
    ] {
        assert_eq!(type_label(Some(&kind)), label);
    }
}

/// 没开“显示 key 详情”:可见行不自动加载;手动刷新照样可以(P-14)。
#[test]
fn details_are_not_auto_loaded_when_the_setting_is_off_but_manual_refresh_works() {
    let mut h = H::new();
    h.settings.show_key_details = false;
    let (_, keys) = many(&mut h, 3);
    assert!(detail_reqs(&h.send(KeysMsg::Visible(keys.clone()))).is_empty());
    let out = h.send(KeysMsg::RefreshDetails(keys[1].clone()));
    assert_eq!(detail_reqs(&out).len(), 1);
    assert_eq!(detail_reqs(&out)[0].1, keys[1]);
}

/// 可见范围变了:还在排队(没发出去)的按新的重排;已经发出去的继续。
#[test]
fn a_new_visible_range_replaces_the_queued_requests() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 40);
    let out = h.send(KeysMsg::Visible(keys[..30].to_vec()));
    assert_eq!(detail_reqs(&out).len(), POOL_SIZE);
    // 滚到别处:新的可见范围是最后 5 个
    let out = h.send(KeysMsg::Visible(keys[35..].to_vec()));
    assert!(detail_reqs(&out).is_empty(), "all 12 slots are still taken");
    // 一个回来,接着发的应该是新范围的第一个,不是旧范围排队的
    let out = h.send(loaded_ok(
        g,
        "key000",
        det(KeyType::String, Some(1), Ttl::Persistent),
    ));
    let next = detail_reqs(&out);
    assert_eq!(next.len(), 1);
    assert_eq!(
        next[0].1, keys[35],
        "the old queued keys were dropped in favour of what is visible now"
    );
}

/// 列表换过了(新搜索、重置)之后,旧请求的结果要丢弃,而且**不能腾出名额**:
/// 否则新列表会同时有 12 加上旧的在途请求个请求。
#[test]
fn stale_detail_results_are_dropped_and_do_not_free_slots() {
    let mut h = H::new();
    let (old_g, old_keys) = many(&mut h, 20);
    let out = h.send(KeysMsg::Visible(old_keys.clone()));
    assert_eq!(detail_reqs(&out).len(), POOL_SIZE);

    let out = h.send(KeysMsg::Search);
    let new_g = scans(&out)[0].0;
    let keys: Vec<String> = (0..20).map(|i| format!("new{i:02}")).collect();
    let refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    h.batch(new_g, &refs, 0);
    let out = h.send(KeysMsg::Visible(keys.iter().map(|s| k(s)).collect()));
    assert_eq!(
        detail_reqs(&out).len(),
        POOL_SIZE,
        "a fresh list has its own full set of slots"
    );

    // 旧请求的结果迟到:不改新列表,也不让新列表多发
    let out = h.send(loaded_ok(
        old_g,
        "key000",
        det(KeyType::Hash, Some(9), Ttl::Persistent),
    ));
    assert!(
        detail_reqs(&out).is_empty(),
        "a stale result must not free a slot"
    );
    assert!(h.list.view().all(|r| r.loaded().is_none()));
}

/// 详情加载失败的行不会被可见范围自动重试(否则一直重试);手动刷新才重试。
#[test]
fn a_failed_detail_load_is_not_retried_automatically() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 2);
    h.send(KeysMsg::Visible(keys.clone()));
    h.send(KeysMsg::DetailsLoaded {
        generation: g,
        key: keys[0].clone(),
        result: Err(Error::Timeout("slow".into())),
    });
    assert_eq!(h.list.view().next().unwrap().details, Details::Failed);
    assert!(detail_reqs(&h.send(KeysMsg::Visible(keys.clone()))).is_empty());
    let out = h.send(KeysMsg::RefreshDetails(keys[0].clone()));
    assert_eq!(detail_reqs(&out).len(), 1, "manual refresh retries");
}

/// 加载中又点了刷新:当前这次回来后再取一次,不并行发两个。
#[test]
fn refresh_during_loading_requests_again_after_the_first_returns() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 1);
    h.send(KeysMsg::Visible(keys.clone()));
    let out = h.send(KeysMsg::RefreshDetails(keys[0].clone()));
    assert!(
        detail_reqs(&out).is_empty(),
        "never two requests for one key at the same time"
    );
    let out = h.send(loaded_ok(
        g,
        "key000",
        det(KeyType::String, Some(1), Ttl::Persistent),
    ));
    assert_eq!(
        detail_reqs(&out).len(),
        1,
        "now the pending refresh goes out"
    );
}

/// 一行在等详情期间被删掉了,结果回来要忽略,不 panic。
#[test]
fn a_detail_result_for_a_row_that_is_gone_is_ignored() {
    let mut h = H::new();
    let (g, keys) = many(&mut h, 2);
    h.send(KeysMsg::Visible(keys.clone()));
    h.send(KeysMsg::Select {
        key: keys[0].clone(),
        on: true,
    });
    h.send(KeysMsg::DeleteSelected);
    h.send(KeysMsg::ConfirmDelete);
    h.send(KeysMsg::Deleted(Ok(1)));
    assert_eq!(h.list.loaded_count(), 1);
    let out = h.send(loaded_ok(
        g,
        "key000",
        det(KeyType::String, Some(1), Ttl::Persistent),
    ));
    assert!(out.notes.is_empty());
    assert_eq!(h.list.loaded_count(), 1);
}

// ---------------------------------------------------------------------------
// 选择、批量删除(P-40)
// ---------------------------------------------------------------------------

/// P-40:全选 = 当前过滤后已加载的全部行;取消全选清空;不认识的 key 选不上。
#[test]
fn p40_select_all_means_the_filtered_loaded_rows() {
    let mut h = H::new();
    h.search_with(&["user:1", "user:2", "order:1"], 7);
    h.send(KeysMsg::SetFilter("user".into()));
    h.send(KeysMsg::SelectAllFiltered(true));
    assert_eq!(h.list.selected_count(), 2);
    assert!(h.list.is_selected(b"user:1") && !h.list.is_selected(b"order:1"));
    h.send(KeysMsg::SelectAllFiltered(false));
    assert_eq!(h.list.selected_count(), 0);
    h.send(KeysMsg::Select {
        key: k("nope"),
        on: true,
    });
    assert_eq!(
        h.list.selected_count(),
        0,
        "only loaded rows can be selected"
    );
    h.send(KeysMsg::Select {
        key: k("order:1"),
        on: true,
    });
    h.send(KeysMsg::Select {
        key: k("order:1"),
        on: false,
    });
    assert_eq!(h.list.selected_count(), 0);
}

/// P-40:删除要确认,确认框显示数量;取消不动;空选择不弹确认。
#[test]
fn p40_delete_needs_confirmation_showing_the_count() {
    let mut h = H::new();
    h.search_with(&["a", "b", "c"], 0);
    h.send(KeysMsg::DeleteSelected);
    assert_eq!(
        h.list.confirm_delete, None,
        "nothing selected: no confirmation"
    );
    h.send(KeysMsg::SelectAllFiltered(true));
    let out = h.send(KeysMsg::DeleteSelected);
    assert!(
        out.cmds.is_empty(),
        "nothing is sent before the user confirms"
    );
    assert_eq!(h.list.confirm_delete, Some(3));
    h.send(KeysMsg::CancelDelete);
    assert_eq!(h.list.confirm_delete, None);
    assert_eq!(h.list.selected_count(), 3, "cancelling keeps the selection");
    assert!(
        h.send(KeysMsg::ConfirmDelete).cmds.is_empty(),
        "confirming without a pending confirmation does nothing"
    );
}

/// P-40:确认后发删除命令(key 按列表顺序);成功后从列表、去重集合、选择里去掉,提示删了多少,
/// 并用 DBSIZE 刷新 key 数;之后列表状态一致(再选、再扫都对)。
#[test]
fn p40_confirmed_delete_removes_rows_reports_and_refreshes_the_key_count() {
    let mut h = H::new();
    // 关掉自动续扫:否则游标非 0 时搜索还在继续,扫描期间不能叠加删除
    h.settings.auto_scan_limit = 0;
    let (g, _) = h.search_with(&["a", "b", "c", "d"], 9);
    h.send(KeysMsg::Select {
        key: k("d"),
        on: true,
    });
    h.send(KeysMsg::Select {
        key: k("b"),
        on: true,
    });
    h.send(KeysMsg::DeleteSelected);
    let out = h.send(KeysMsg::ConfirmDelete);
    match backend(&out)[..] {
        [BackendCmd::DeleteKeys { keys, .. }] => {
            assert_eq!(keys, &vec![k("b"), k("d")], "in list order")
        }
        _ => panic!("{:?}", out.cmds),
    }
    assert_eq!(h.list.busy, Some(Busy::Deleting));
    assert!(h.list.is_busy());

    let out = h.send(KeysMsg::Deleted(Ok(2)));
    assert_eq!(names(&h.list), vec!["a", "c"]);
    assert_eq!(h.list.selected_count(), 0);
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Success);
    assert_eq!(last_note(&out).text, "2 key(s) deleted.");
    assert!(matches!(backend(&out)[..], [BackendCmd::DbSize { .. }]));
    h.send(KeysMsg::DbSize(Ok(7)));
    assert_eq!(h.list.db_total, Some(7));

    // 删掉的 key 之后又被扫到:当成新 key(v1:scannedKeySet 一并删掉)
    h.send(KeysMsg::LoadMore);
    h.batch(g, &["b"], 0);
    assert_eq!(names(&h.list), vec!["a", "b", "c"]);
    // 行下标重排之后选择、详情还是对的
    h.send(KeysMsg::Select {
        key: k("c"),
        on: true,
    });
    assert!(h.list.is_selected(b"c"));
}

/// 删除失败:列表和选择都不动,提示错误,遮罩解除。
#[test]
fn a_failed_delete_keeps_rows_and_selection_and_reports() {
    let mut h = H::new();
    h.search_with(&["a", "b"], 0);
    h.send(KeysMsg::SelectAllFiltered(true));
    h.send(KeysMsg::DeleteSelected);
    h.send(KeysMsg::ConfirmDelete);
    let out = h.send(KeysMsg::Deleted(Err(Error::Redis(
        "READONLY You can't write against a read only replica.".into(),
    ))));
    assert_eq!(h.list.loaded_count(), 2);
    assert_eq!(h.list.selected_count(), 2);
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(last_note(&out).text.contains("READONLY"));
    // 可以再试
    h.send(KeysMsg::DeleteSelected);
    assert_eq!(h.list.confirm_delete, Some(2));
}

/// 遮罩期间(扫描、删除、复制、粘贴)用户操作互不叠加。
#[test]
fn actions_do_not_stack_while_busy() {
    let mut h = H::new();
    // 自动续扫关掉,搜索收尾后回到 Idle,这样下面的 busy 真的是“正在删除”,不是“还在扫描”
    h.settings.auto_scan_limit = 0;
    h.search_with(&["a", "b"], 5);
    assert_eq!(h.list.scan_state(), ScanState::Idle);
    h.send(KeysMsg::SelectAllFiltered(true));
    h.send(KeysMsg::DeleteSelected);
    h.send(KeysMsg::ConfirmDelete);
    assert_eq!(h.list.busy, Some(Busy::Deleting));
    assert!(h.list.is_busy());
    assert!(h.send(KeysMsg::Search).cmds.is_empty());
    assert!(h.send(KeysMsg::LoadMore).cmds.is_empty());
    assert!(h.send(KeysMsg::CopySelected).cmds.is_empty());
    assert!(h.send(KeysMsg::PasteRequested).cmds.is_empty());
    h.send(KeysMsg::DeleteSelected);
    assert_eq!(h.list.confirm_delete, None);
    // 不是自己发起的结果不处理
    assert!(h.send(KeysMsg::Copied(Err(Error::Closed))).notes.is_empty());
}

// ---------------------------------------------------------------------------
// 复制(P-41、L-13)
// ---------------------------------------------------------------------------

fn copy_outcome(n: usize, skipped: &[&str]) -> CopyOutcome {
    CopyOutcome {
        text: "REDIS:xyz".into(),
        copied: n,
        skipped: skipped
            .iter()
            .map(|s| Skipped {
                key: k(s),
                reason: SkipReason::Missing,
            })
            .collect(),
    }
}

/// P-41:复制 = 选中的 key 取 DUMP,文本交给平台写剪贴板,写成功后才清选择并提示;跳过的 key 汇总提示。
#[test]
fn p41_copy_writes_the_clipboard_then_clears_the_selection() {
    let mut h = H::new();
    h.search_with(&["a", "b", "c"], 0);
    assert!(
        h.send(KeysMsg::CopySelected).cmds.is_empty(),
        "nothing selected"
    );
    h.send(KeysMsg::Select {
        key: k("c"),
        on: true,
    });
    h.send(KeysMsg::Select {
        key: k("a"),
        on: true,
    });
    let out = h.send(KeysMsg::CopySelected);
    match backend(&out)[..] {
        [BackendCmd::CopyKeys { keys, .. }] => assert_eq!(keys, &vec![k("a"), k("c")]),
        _ => panic!(),
    }
    let out = h.send(KeysMsg::Copied(Ok(copy_outcome(2, &["gone1", "gone2"]))));
    match platform(&out)[..] {
        [PlatformCmd::WriteClipboard { text, .. }] => assert_eq!(text, "REDIS:xyz"),
        _ => panic!("{:?}", out.cmds),
    }
    assert_eq!(
        h.list.selected_count(),
        2,
        "the selection stays until the clipboard write succeeded"
    );
    assert_eq!(h.list.busy, Some(Busy::Copying));

    let out = h.send(KeysMsg::ClipboardWritten(Ok(())));
    assert_eq!(h.list.selected_count(), 0);
    assert_eq!(h.list.busy, None);
    assert_eq!(out.notes[0].text, "Copied 2 key(s) to the clipboard.");
    assert_eq!(out.notes[0].level, Level::Success);
    assert_eq!(out.notes[1].level, Level::Warn);
    assert!(out.notes[1].text.contains("gone1, gone2") && out.notes[1].text.contains('2'));
}

/// L-13:剪贴板暂时打不开(被输入法、剪贴板管理器、远程桌面占着)要提示,不能静默;选择保留,可以再试。
#[test]
fn l13_a_clipboard_write_failure_is_reported_and_the_selection_is_kept() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::SelectAllFiltered(true));
    h.send(KeysMsg::CopySelected);
    h.send(KeysMsg::Copied(Ok(copy_outcome(1, &[]))));
    let out = h.send(KeysMsg::ClipboardWritten(Err(
        "OpenClipboard failed: Access is denied".into(),
    )));
    assert_eq!(
        h.list.selected_count(),
        1,
        "nothing was copied: keep the selection"
    );
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(last_note(&out).text.contains("Access is denied"));
    assert!(
        last_note(&out).text.contains("held by another app"),
        "{}",
        last_note(&out).text
    );
    assert!(!out.notes.iter().any(|n| n.level == Level::Success));
}

/// 复制失败(一个都没复制成、超限):提示原因,遮罩解除。
#[test]
fn a_failed_copy_reports_the_reason() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::SelectAllFiltered(true));
    h.send(KeysMsg::CopySelected);
    let out = h.send(KeysMsg::Copied(Err(Error::CopyTooLarge {
        total: 70 * 1024 * 1024,
        limit: 64 * 1024 * 1024,
        copied: 3,
        key: k("big"),
    })));
    assert_eq!(h.list.busy, None);
    assert!(last_note(&out).text.contains("70.0 MiB") && last_note(&out).text.contains("'big'"));
    assert!(
        platform(&out).is_empty(),
        "nothing is written to the clipboard"
    );
}

// ---------------------------------------------------------------------------
// 粘贴(P-42、L-13)
// ---------------------------------------------------------------------------

fn checked(total: usize, existing: usize) -> KeysMsg {
    KeysMsg::PasteChecked {
        text: "REDIS:abc".into(),
        result: Ok(PastePreview { total, existing }),
    }
}

/// P-42:粘贴先读剪贴板;不是 `REDIS:` 开头的直接提示,不弹确认框。
#[test]
fn p42_pasting_something_that_is_not_redis_data_is_rejected() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    let out = h.send(KeysMsg::PasteRequested);
    assert!(matches!(
        platform(&out)[..],
        [PlatformCmd::ReadClipboard { .. }]
    ));
    assert_eq!(h.list.busy, Some(Busy::Pasting));
    let out = h.send(KeysMsg::ClipboardRead(Ok(Some("just some text".into()))));
    assert!(h.list.paste.is_none());
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(last_note(&out).text.contains("does not contain Redis data"));
}

/// L-13:读剪贴板失败要提示,不能静默。
#[test]
fn l13_a_clipboard_read_failure_is_reported() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    let out = h.send(KeysMsg::ClipboardRead(Err("the clipboard is busy".into())));
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(last_note(&out).text.contains("the clipboard is busy"));
}

/// R-08c:剪贴板里没有文本不是平台错误:GUI 只报告"没有"(`Ok(None)`),措辞由 App 按界面语言翻译。
#[test]
fn r08c_an_empty_clipboard_is_reported_by_the_app_not_the_platform_layer() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    let out = h.send(KeysMsg::ClipboardRead(Ok(None)));
    assert_eq!(h.list.busy, None);
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(
        last_note(&out).text.contains("clipboard is empty"),
        "{}",
        last_note(&out).text
    );
    assert!(!last_note(&out).text.contains("Could not read"));
}

/// R-07d:读到 `REDIS:` 数据后不直接弹框,先让后端解码并查冲突(`PreviewPaste`),
/// 预检结果回来才弹,框里带“共 N 个,其中 M 个已存在”。
#[test]
fn r07d_the_confirmation_waits_for_the_preview_and_shows_the_counts() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    let out = h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    assert!(matches!(
        backend(&out)[..],
        [BackendCmd::PreviewPaste { .. }]
    ));
    assert!(h.list.paste.is_none(), "no dialog before the preview");
    assert_eq!(h.list.busy, Some(Busy::Pasting));

    h.send(checked(5, 2));
    let p = h.list.paste.clone().expect("a confirmation");
    assert_eq!((p.total, p.existing), (5, 2));
    assert!(p.can_overwrite());
    assert_eq!(h.list.busy, None);
    let text = p.summary("en");
    assert!(
        text.contains('5') && text.contains('2') && text.contains("already exist"),
        "{text}"
    );
}

/// R-07d:没有同名 key 时不显示“覆盖同名 key”,也不接受把它打开。
#[test]
fn r07d_no_overwrite_option_when_nothing_exists() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    h.send(checked(3, 0));
    assert!(!h.list.paste.as_ref().unwrap().can_overwrite());
    h.send(KeysMsg::SetPasteOverwrite(true));
    assert!(!h.list.paste.as_ref().unwrap().overwrite);
    let out = h.send(KeysMsg::ConfirmPaste);
    assert!(matches!(
        backend(&out)[..],
        [BackendCmd::PasteKeys {
            overwrite: false,
            ..
        }]
    ));
}

/// R-07d:解码失败或超限:立即报错,不弹框,不再忙。
#[test]
fn r07d_preview_failures_are_reported_without_a_dialog() {
    for (err, needle) in [
        (Error::ClipboardNotRedis, "does not contain Redis data"),
        (Error::PasteTooLarge { limit: 1024 * 1024 }, "paste limit"),
        (
            Error::ClipboardCorrupt {
                stage: xredis_core::error::ClipboardStage::Base64,
                detail: "bad".into(),
            },
            "bad",
        ),
    ] {
        let mut h = H::new();
        h.search_with(&["a"], 0);
        h.send(KeysMsg::PasteRequested);
        h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
        let out = h.send(KeysMsg::PasteChecked {
            text: "REDIS:abc".into(),
            result: Err(err),
        });
        assert!(h.list.paste.is_none());
        assert_eq!(h.list.busy, None);
        assert_eq!(last_note(&out).level, Level::Error);
        assert!(
            last_note(&out).text.contains(needle),
            "{}",
            last_note(&out).text
        );
    }
}

/// R-07d:迟到的预检结果(忙状态已被别的流程清掉)不能凭空弹框。
#[test]
fn r07d_a_stray_preview_result_does_not_open_a_dialog() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(checked(3, 1));
    assert!(h.list.paste.is_none());
}

/// P-42 的差别:确认框有“覆盖同名 key”选项,**默认不勾**(v1 界面总是覆盖);确认后带着选择发给后端。
#[test]
fn p42_the_paste_confirmation_defaults_to_not_overwriting() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    h.send(checked(3, 2));
    let p = h.list.paste.clone().expect("a confirmation");
    assert!(!p.overwrite, "must not overwrite by default");
    assert_eq!(
        h.list.busy, None,
        "the confirmation dialog is up, nothing is running"
    );

    let out = h.send(KeysMsg::ConfirmPaste);
    match backend(&out)[..] {
        [
            BackendCmd::PasteKeys {
                text, overwrite, ..
            },
        ] => {
            assert_eq!(text, "REDIS:abc");
            assert!(!overwrite);
        }
        _ => panic!(),
    }
    assert!(h.list.paste.is_none());
    assert_eq!(h.list.busy, Some(Busy::Pasting));

    // 勾上覆盖
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    h.send(checked(3, 2));
    h.send(KeysMsg::SetPasteOverwrite(true));
    let out = h.send(KeysMsg::ConfirmPaste);
    assert!(matches!(
        backend(&out)[..],
        [BackendCmd::PasteKeys {
            overwrite: true,
            ..
        }]
    ));

    // 取消
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    h.send(checked(3, 2));
    h.send(KeysMsg::CancelPaste);
    assert!(h.list.paste.is_none() && h.list.busy.is_none());
    assert!(h.send(KeysMsg::ConfirmPaste).cmds.is_empty());
}

/// P-42:粘贴成功:提示数量,刷新 key 数,重新搜索。
#[test]
fn p42_a_successful_paste_refreshes_the_count_and_searches_again() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    h.send(KeysMsg::PasteRequested);
    h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
    h.send(checked(3, 2));
    h.send(KeysMsg::ConfirmPaste);
    let out = h.send(KeysMsg::Pasted(Ok(PasteOutcome { count: 4 })));
    assert_eq!(last_note(&out).text, "Pasted 4 key(s) from the clipboard.");
    assert!(
        backend(&out)
            .iter()
            .any(|c| matches!(c, BackendCmd::DbSize { .. }))
    );
    assert_eq!(scans(&out).len(), 1, "searches again");
    assert_eq!(h.list.scan_state(), ScanState::Searching);
    assert_eq!(
        h.list.loaded_count(),
        0,
        "the list is rebuilt from the search"
    );
}

/// 不覆盖模式下有冲突:整体拒绝,列出冲突的 key,不刷新(什么也没写);
/// 逐个写入中途失败:前面的已经写进去了,要刷新才看得到。
#[test]
fn p42_paste_failures_report_and_refresh_only_when_something_was_written() {
    let mut h = H::new();
    h.search_with(&["a"], 0);
    let paste = |h: &mut H| {
        h.send(KeysMsg::PasteRequested);
        h.send(KeysMsg::ClipboardRead(Ok(Some("REDIS:abc".into()))));
        h.send(checked(3, 2));
        h.send(KeysMsg::ConfirmPaste);
    };
    paste(&mut h);
    let out = h.send(KeysMsg::Pasted(Err(Error::PasteConflicts {
        conflicts: vec![k("a"), k("b")],
        total: 3,
    })));
    assert_eq!(h.list.busy, None);
    assert!(last_note(&out).text.contains("2 of 3") && last_note(&out).text.contains("a, b"));
    assert!(out.cmds.is_empty(), "nothing was written: no refresh");
    assert_eq!(h.list.loaded_count(), 1, "the list is untouched");

    paste(&mut h);
    let out = h.send(KeysMsg::Pasted(Err(Error::RestoreFailed {
        key: k("b"),
        detail: "BUSYKEY Target key name already exists.".into(),
    })));
    assert!(last_note(&out).text.contains("BUSYKEY"));
    assert_eq!(
        scans(&out).len(),
        1,
        "keys before the failure were written: refresh"
    );
}
