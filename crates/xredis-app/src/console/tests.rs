//! 控制台视图模型的测试(不经过 App)。移植 v1 `consoleVM.test.ts`(历史、命令执行、clear、光标和编辑),
//! 再加 R-07c 的规则:输出 10000 行上限、等待动画、连接重建后迟到的结果丢弃、按字符的光标。

use xredis_core::Error;
use xredis_core::config::{ConnId, Settings};

use super::*;

struct H {
    c: Console,
    settings: Settings,
    id: ConnId,
    now: i64,
}

impl H {
    fn new() -> Self {
        Self {
            c: Console::new(),
            settings: Settings::default(),
            id: ConnId::new(),
            now: 10_000,
        }
    }

    fn send(&mut self, msg: ConsoleMsg) -> ConsoleOut {
        let ctx = Ctx {
            id: self.id,
            lang: "en",
            settings: &self.settings,
            now: self.now,
        };
        self.c.update(&ctx, msg)
    }

    fn type_text(&mut self, text: &str) {
        self.send(ConsoleMsg::Insert(text.into()));
    }

    /// 输入一行并回车,返回发出的命令(最多一条)。
    fn run(&mut self, line: &str) -> Option<(u64, Vec<Vec<u8>>)> {
        self.type_text(line);
        let out = self.send(ConsoleMsg::Submit);
        out.cmds.into_iter().find_map(|c| match c {
            Cmd::Backend(BackendCmd::RunCommand {
                generation, tokens, ..
            }) => Some((generation, tokens)),
            _ => None,
        })
    }

    fn finish(&mut self, generation: u64, text: &str) {
        self.send(ConsoleMsg::Executed {
            generation,
            result: Ok(text.into()),
        });
    }

    fn lines(&self) -> Vec<String> {
        self.c.output().cloned().collect()
    }
}

fn s(t: &[u8]) -> String {
    String::from_utf8_lossy(t).into_owned()
}

// ---------------------------------------------------------------------------
// 历史(P-50:最多 100 条,连续重复不记录)
// ---------------------------------------------------------------------------

#[test]
fn history_records_in_order_but_not_consecutive_repeats() {
    let mut h = H::new();
    for line in ["PING", "PING", "GET a", "PING", "PING"] {
        let (g, _) = h.run(line).unwrap();
        h.finish(g, "x");
    }
    assert_eq!(
        h.c.history(),
        ["PING", "GET a", "PING"],
        "only consecutive repeats are dropped"
    );
}

#[test]
fn history_keeps_the_last_hundred() {
    let mut h = H::new();
    for i in 0..105 {
        let (g, _) = h.run(&format!("GET k{i}")).unwrap();
        h.finish(g, "x");
    }
    let hist = h.c.history();
    assert_eq!(hist.len(), HISTORY_MAX);
    assert_eq!(
        hist.first().unwrap(),
        "GET k5",
        "the oldest five were dropped"
    );
    assert_eq!(hist.last().unwrap(), "GET k104");
}

#[test]
fn blank_lines_are_not_remembered() {
    let mut h = H::new();
    h.run("");
    h.run("   ");
    assert!(h.c.history().is_empty());
}

// ---------------------------------------------------------------------------
// 命令执行
// ---------------------------------------------------------------------------

/// 分词后第一个 token 是命令名,其余是参数;带空格的参数不会被切碎(v1 修过的坑)。
#[test]
fn the_line_is_split_into_a_command_and_arguments() {
    let mut h = H::new();
    let (_, tokens) = h.run(r#"SET k "hello world""#).unwrap();
    assert_eq!(
        tokens,
        vec![b"SET".to_vec(), b"k".to_vec(), b"hello world".to_vec()]
    );
}

#[test]
fn a_subcommand_goes_into_the_arguments_whole() {
    let mut h = H::new();
    let (_, tokens) = h.run("CONFIG GET maxmemory").unwrap();
    assert_eq!(
        tokens.iter().map(|t| s(t)).collect::<Vec<_>>(),
        ["CONFIG", "GET", "maxmemory"]
    );
}

/// L-07:转义出来的二进制字节原样交给后端。
#[test]
fn binary_arguments_keep_their_bytes() {
    let mut h = H::new();
    let (_, tokens) = h.run(r#"SET k "\xff\x00\n""#).unwrap();
    assert_eq!(tokens[2], vec![0xff, 0x00, b'\n']);
}

/// 引号没闭合:提示用户,不发送命令,输入行清掉可以接着输入。
#[test]
fn unbalanced_quotes_are_reported_and_nothing_is_sent() {
    let mut h = H::new();
    assert!(h.run(r#"SET k "oops"#).is_none());
    let lines = h.lines();
    assert!(
        lines.last().unwrap().contains("unbalanced quotes"),
        "{lines:?}"
    );
    assert!(!h.c.is_running());
    assert_eq!(h.c.input(), "");
    // 闭合引号后紧跟非空白
    assert!(h.run(r#"SET k "a"b"#).is_none());
    assert!(h.lines().last().unwrap().contains("Invalid command syntax"));
}

#[test]
fn empty_input_sends_nothing() {
    let mut h = H::new();
    assert!(h.run("").is_none());
    assert!(h.run("    ").is_none());
    assert!(!h.c.is_running());
}

/// clear 是本地命令:不发给后端,输出清空(不区分大小写,首尾空白无所谓)。
#[test]
fn clear_is_local_and_wipes_the_output() {
    for line in ["clear", "CLEAR", "  Clear  "] {
        let mut h = H::new();
        let (g, _) = h.run("PING").unwrap();
        h.finish(g, "PONG");
        assert!(h.c.output_len() >= 2);
        assert!(h.run(line).is_none(), "{line:?} must not be sent");
        assert_eq!(h.c.output_len(), 0, "{line:?}");
        assert!(!h.c.is_running());
    }
}

/// 执行后清空输入、重置光标和历史索引。
#[test]
fn submitting_resets_the_input_cursor_and_history_position() {
    let mut h = H::new();
    let (g, _) = h.run("GET a").unwrap();
    h.finish(g, "x");
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "GET a");
    h.type_text(" b");
    h.send(ConsoleMsg::Submit);
    assert_eq!((h.c.input(), h.c.cursor()), ("", 0));
    // 历史索引回到“没在浏览”:↓ 什么也不做
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "");
}

#[test]
fn the_command_is_echoed_with_the_prompt_and_the_result_follows() {
    let mut h = H::new();
    let (g, _) = h.run("MGET a b").unwrap();
    h.finish(g, "1) \"x\"\n2) (nil)");
    assert_eq!(h.lines(), ["> MGET a b", "1) \"x\"", "2) (nil)"]);
}

#[test]
fn a_new_line_cannot_be_submitted_while_one_is_running() {
    let mut h = H::new();
    let (g, _) = h.run("BLPOP q 0").unwrap();
    assert!(h.c.is_running());
    assert!(h.run("PING").is_none(), "ignored while running");
    assert_eq!(h.c.history(), ["BLPOP q 0"], "and not recorded");
    h.finish(g, "(nil)");
    assert!(!h.c.is_running());
    assert!(h.run("PING").is_some());
}

/// 错误按种类翻译:被拒绝的命令引导去 Pub/Sub 页。
#[test]
fn errors_are_translated() {
    let mut h = H::new();
    let (g, _) = h.run("SUBSCRIBE a").unwrap();
    h.send(ConsoleMsg::Executed {
        generation: g,
        result: Err(Error::CommandRejected {
            command: "SUBSCRIBE".into(),
        }),
    });
    let last = h.lines().pop().unwrap();
    assert!(
        last.contains("'SUBSCRIBE'") && last.contains("Pub/Sub page"),
        "{last}"
    );
    assert!(!h.c.is_running());

    let (g, _) = h.run("BLPOP q 0").unwrap();
    h.send(ConsoleMsg::Executed {
        generation: g,
        result: Err(Error::BlockingTimeout {
            command: "BLPOP".into(),
            secs: 30,
        }),
    });
    assert!(h.lines().pop().unwrap().contains("30s"));
}

#[test]
fn a_late_result_for_an_old_generation_is_dropped() {
    let mut h = H::new();
    let (g, _) = h.run("PING").unwrap();
    h.c.reset_connection();
    assert!(!h.c.is_running(), "the command is abandoned");
    let before = h.c.output_len();
    h.finish(g, "PONG");
    assert_eq!(h.c.output_len(), before, "the late result did not appear");
    // 新的命令用新的代号,旧代号不会误伤
    let (g2, _) = h.run("PING").unwrap();
    assert_ne!(g, g2);
    h.finish(g, "stale");
    assert!(h.c.is_running(), "still waiting for the real one");
    h.finish(g2, "PONG");
    assert!(!h.c.is_running());
}

#[test]
fn reconnecting_keeps_history_and_output_but_clears_the_line() {
    let mut h = H::new();
    let (g, _) = h.run("PING").unwrap();
    h.finish(g, "PONG");
    h.type_text("half typed");
    h.c.reset_connection();
    assert_eq!(h.c.history(), ["PING"]);
    assert_eq!(h.c.output_len(), 2);
    assert_eq!((h.c.input(), h.c.cursor()), ("", 0));
}

// ---------------------------------------------------------------------------
// 输出上限(P-50:最多 10000 行)
// ---------------------------------------------------------------------------

#[test]
fn the_output_keeps_the_last_ten_thousand_lines() {
    let mut h = H::new();
    let (g, _) = h.run("BIG 10050").unwrap();
    let text = (1..=10_050)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    h.finish(g, &text);
    assert_eq!(h.c.output_len(), OUTPUT_MAX_LINES);
    let lines = h.lines();
    assert_eq!(
        lines.first().unwrap(),
        "line 51",
        "the oldest lines (including the echo) went first"
    );
    assert_eq!(lines.last().unwrap(), "line 10050");

    // 之后每多一行,最早的丢一行
    let (g, _) = h.run("PING").unwrap();
    h.finish(g, "PONG");
    assert_eq!(h.c.output_len(), OUTPUT_MAX_LINES);
    assert_eq!(h.lines().last().unwrap(), "PONG");
}

/// 一次回复比上限还大:只留最后 10000 行。
#[test]
fn a_single_reply_larger_than_the_cap_is_trimmed() {
    let mut h = H::new();
    let (g, _) = h.run("BIG 1").unwrap();
    let text = (0..25_000)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    h.finish(g, &text);
    assert_eq!(h.c.output_len(), OUTPUT_MAX_LINES);
    assert_eq!(h.lines().last().unwrap(), "24999");
    assert_eq!(h.lines().first().unwrap(), "15000");
}

// ---------------------------------------------------------------------------
// 等待动画(P-50:超过 500 ms 才显示)
// ---------------------------------------------------------------------------

#[test]
fn the_waiting_animation_starts_after_500ms_and_advances() {
    let mut h = H::new();
    assert_eq!(h.c.waiting_frame(h.now), None, "nothing running");
    let (g, _) = h.run("BLPOP q 0").unwrap();
    let t0 = h.now;
    assert_eq!(h.c.waiting_frame(t0), None);
    assert_eq!(h.c.waiting_frame(t0 + 499), None, "not yet");
    let first = h.c.waiting_frame(t0 + 500).unwrap();
    assert_eq!(
        h.c.waiting_frame(t0 + 599),
        Some(first),
        "one frame lasts 100 ms"
    );
    let second = h.c.waiting_frame(t0 + 600).unwrap();
    assert_ne!(first, second);
    // 一圈十帧后回到第一帧
    assert_eq!(h.c.waiting_frame(t0 + 500 + 1000), Some(first));
    h.finish(g, "(nil)");
    assert_eq!(h.c.waiting_frame(t0 + 700), None, "stops with the result");
}

// ---------------------------------------------------------------------------
// 光标和编辑
// ---------------------------------------------------------------------------

#[test]
fn pasted_text_is_inserted_at_the_cursor() {
    let mut h = H::new();
    h.type_text("GET ");
    h.type_text("key");
    h.send(ConsoleMsg::Home);
    h.send(ConsoleMsg::Insert("MG".into()));
    assert_eq!((h.c.input(), h.c.cursor()), ("MGGET key", 2));
    h.send(ConsoleMsg::End);
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::Insert("XY".into()));
    assert_eq!(h.c.input(), "MGGET kXYey");
}

#[test]
fn a_pasted_newline_becomes_a_space() {
    let mut h = H::new();
    h.type_text("a\r\nb\nc");
    assert_eq!(h.c.input(), "a  b c");
}

#[test]
fn the_cursor_stays_inside_the_line() {
    let mut h = H::new();
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::Backspace);
    h.send(ConsoleMsg::Delete);
    assert_eq!((h.c.input(), h.c.cursor()), ("", 0));
    h.type_text("ab");
    h.send(ConsoleMsg::Right);
    h.send(ConsoleMsg::Delete);
    assert_eq!((h.c.input(), h.c.cursor()), ("ab", 2));
    for _ in 0..5 {
        h.send(ConsoleMsg::Left);
    }
    assert_eq!(h.c.cursor(), 0);
    h.send(ConsoleMsg::Backspace);
    assert_eq!(h.c.input(), "ab");
}

#[test]
fn home_and_end_move_to_both_ends() {
    let mut h = H::new();
    h.type_text("SET k v");
    h.send(ConsoleMsg::Home);
    assert_eq!(h.c.cursor(), 0);
    h.send(ConsoleMsg::End);
    assert_eq!(h.c.cursor(), 7);
}

/// 光标按字符:中文、表情不会被从中间切开(按字节会 panic 或乱码)。
#[test]
fn the_cursor_counts_characters_not_bytes() {
    let mut h = H::new();
    h.type_text("SET 键 值😀");
    assert_eq!(h.c.cursor(), 8);
    h.send(ConsoleMsg::Backspace);
    assert_eq!(h.c.input(), "SET 键 值");
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::Delete);
    assert_eq!(h.c.input(), "SET 键值");
    h.send(ConsoleMsg::Insert("😀".into()));
    assert_eq!(h.c.input(), "SET 键😀值");
    assert_eq!(h.c.cursor(), 6);
}

#[test]
fn word_movement() {
    let mut h = H::new();
    h.type_text("SET user:1 hello");
    h.send(ConsoleMsg::Home);
    h.send(ConsoleMsg::WordRight);
    assert_eq!(h.c.cursor(), 4, "start of the next word");
    h.send(ConsoleMsg::WordRight);
    assert_eq!(h.c.cursor(), 9, "':' is not a word character");
    h.send(ConsoleMsg::WordRight);
    assert_eq!(h.c.cursor(), 11);
    h.send(ConsoleMsg::WordRight);
    assert_eq!(h.c.cursor(), 16, "end of the line");
    h.send(ConsoleMsg::WordRight);
    assert_eq!(h.c.cursor(), 16);

    h.send(ConsoleMsg::WordLeft);
    assert_eq!(h.c.cursor(), 11, "start of 'hello'");
    h.send(ConsoleMsg::WordLeft);
    assert_eq!(h.c.cursor(), 9);
    h.send(ConsoleMsg::WordLeft);
    assert_eq!(h.c.cursor(), 4);
    h.send(ConsoleMsg::WordLeft);
    assert_eq!(h.c.cursor(), 0);
    h.send(ConsoleMsg::WordLeft);
    assert_eq!(h.c.cursor(), 0);
}

/// v1 `按单词向后删除`:Ctrl+退格删到当前单词开头(光标在空格后先跳过空格)。
#[test]
fn deleting_a_word_backward() {
    let mut h = H::new();
    h.type_text("SET key value");
    h.send(ConsoleMsg::DeleteWordBack);
    assert_eq!(h.c.input(), "SET key ");
    h.send(ConsoleMsg::DeleteWordBack);
    assert_eq!(h.c.input(), "SET ", "skips the space, then the word");
    h.send(ConsoleMsg::DeleteWordBack);
    assert_eq!((h.c.input(), h.c.cursor()), ("", 0));
    h.send(ConsoleMsg::DeleteWordBack);
    assert_eq!(h.c.input(), "");

    // 在中间删,光标之后的内容保留
    h.type_text("GET some key");
    for _ in 0..3 {
        h.send(ConsoleMsg::Left);
    }
    h.send(ConsoleMsg::DeleteWordBack);
    assert_eq!(h.c.input(), "GET key");
    assert_eq!(h.c.cursor(), 4);
}

#[test]
fn deleting_a_word_forward() {
    let mut h = H::new();
    h.type_text("SET key value");
    h.send(ConsoleMsg::Home);
    h.send(ConsoleMsg::DeleteWordForward);
    assert_eq!(h.c.input(), "key value", "the word and the spaces after it");
    h.send(ConsoleMsg::DeleteWordForward);
    h.send(ConsoleMsg::DeleteWordForward);
    assert_eq!(h.c.input(), "");
    h.send(ConsoleMsg::DeleteWordForward);
    assert_eq!(h.c.input(), "");
}

/// Ctrl+C:放弃当前行,留一行 `^C`,不进历史。
#[test]
fn ctrl_c_abandons_the_line() {
    let mut h = H::new();
    h.type_text("GET half");
    let out = h.send(ConsoleMsg::Interrupt);
    assert!(out.cmds.is_empty());
    assert_eq!((h.c.input(), h.c.cursor()), ("", 0));
    assert_eq!(h.lines(), ["> GET half^C"]);
    assert!(h.c.history().is_empty());
}

// ---------------------------------------------------------------------------
// 历史浏览(↑↓)
// ---------------------------------------------------------------------------

fn with_history(lines: &[&str]) -> H {
    let mut h = H::new();
    for l in lines {
        let (g, _) = h.run(l).unwrap();
        h.finish(g, "x");
    }
    h
}

#[test]
fn up_and_down_browse_the_history_and_restore_what_was_being_typed() {
    let mut h = with_history(&["one", "two", "three"]);
    h.type_text("draft");
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!((h.c.input(), h.c.cursor()), ("three", 5));
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "two");
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "one");
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "one", "stays at the oldest");
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "two");
    h.send(ConsoleMsg::HistoryNext);
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "draft", "back to what was typed");
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "draft", "nothing further");
}

#[test]
fn browsing_an_empty_history_does_nothing() {
    let mut h = H::new();
    h.type_text("draft");
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "draft");
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "draft");
}

// ---------------------------------------------------------------------------
// R-11a:GUI 用组件库的输入框编辑,整行同步进来;动画定时器由 App 排;输出的版本号
// ---------------------------------------------------------------------------

fn after_ticks(out: &ConsoleOut) -> Vec<(u64, u64)> {
    use crate::msg::{Msg, PlatformCmd};
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Platform(PlatformCmd::After { ms, msg }) => match msg.as_ref() {
                Msg::Console {
                    msg: ConsoleMsg::Tick { generation },
                    ..
                } => Some((*ms, *generation)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn set_input_replaces_the_line_and_ignores_the_same_text() {
    let mut h = H::new();
    h.send(ConsoleMsg::SetInput("GET a".into()));
    assert_eq!(h.c.input(), "GET a");
    assert_eq!(h.c.cursor(), 5);
    h.send(ConsoleMsg::Left);
    h.send(ConsoleMsg::SetInput("GET a".into()));
    assert_eq!(h.c.cursor(), 4, "same text: nothing moves");
    h.send(ConsoleMsg::SetInput("SET 中\n文".into()));
    assert_eq!(h.c.input(), "SET 中 文", "one line only");
    assert_eq!(h.c.cursor(), 7, "the cursor counts characters");
}

#[test]
fn a_line_typed_through_set_input_comes_back_after_browsing_history() {
    let mut h = H::new();
    let (g, _) = h.run("PING").unwrap();
    h.finish(g, "PONG");
    h.send(ConsoleMsg::SetInput("half a comm".into()));
    h.send(ConsoleMsg::HistoryPrev);
    assert_eq!(h.c.input(), "PING");
    h.send(ConsoleMsg::HistoryNext);
    assert_eq!(h.c.input(), "half a comm");
}

#[test]
fn a_sent_command_arms_the_wait_ticker_and_it_stops_with_the_result() {
    let mut h = H::new();
    h.type_text("BLPOP q 0");
    let out = h.send(ConsoleMsg::Submit);
    let ticks = after_ticks(&out);
    assert_eq!(ticks.len(), 1);
    let (ms, g) = ticks[0];
    assert_eq!(
        ms,
        WAIT_ANIMATION_AFTER_MS.unsigned_abs(),
        "the first tick is when the animation starts"
    );
    // 还在等:再排一次,间隔是一帧
    let out = h.send(ConsoleMsg::Tick { generation: g });
    assert_eq!(after_ticks(&out), vec![(WAIT_FRAME_MS.unsigned_abs(), g)]);
    // 别的命令的旧 tick 不续
    let out = h.send(ConsoleMsg::Tick { generation: g + 7 });
    assert!(after_ticks(&out).is_empty());
    // 结果回来后 tick 到了也不再续
    h.finish(g, "(nil)");
    let out = h.send(ConsoleMsg::Tick { generation: g });
    assert!(after_ticks(&out).is_empty());
    assert!(
        ConsoleMsg::Tick { generation: g }.is_result(),
        "a tick is not a user action"
    );
}

#[test]
fn local_commands_and_syntax_errors_arm_no_ticker() {
    let mut h = H::new();
    h.type_text("clear");
    assert!(after_ticks(&h.send(ConsoleMsg::Submit)).is_empty());
    h.type_text("GET \"oops");
    assert!(after_ticks(&h.send(ConsoleMsg::Submit)).is_empty());
}

#[test]
fn the_output_revision_changes_with_every_change_and_the_text_joins_the_lines() {
    let mut h = H::new();
    let r0 = h.c.output_revision();
    let (g, _) = h.run("PING").unwrap();
    let r1 = h.c.output_revision();
    assert_ne!(r0, r1, "the echo line");
    h.finish(g, "PONG");
    assert_ne!(r1, h.c.output_revision());
    assert_eq!(h.c.output_text(), "> PING\nPONG");
    let r2 = h.c.output_revision();
    h.send(ConsoleMsg::Left); // 不动输出
    assert_eq!(r2, h.c.output_revision());
    h.type_text("clear");
    h.send(ConsoleMsg::Submit);
    assert_ne!(r2, h.c.output_revision(), "clear is a change too");
    assert_eq!(h.c.output_text(), "");
}

/// 输出到了上限,行数不变但内容变了:版本号还是要变,GUI 才会重画。
#[test]
fn the_revision_still_changes_when_the_output_is_at_its_cap() {
    let mut h = H::new();
    let (g, _) = h.run("BIG 10000").unwrap();
    h.finish(
        g,
        &(0..OUTPUT_MAX_LINES)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert_eq!(h.c.output_len(), OUTPUT_MAX_LINES);
    let r = h.c.output_revision();
    h.send(ConsoleMsg::Interrupt);
    assert_eq!(h.c.output_len(), OUTPUT_MAX_LINES);
    assert_ne!(r, h.c.output_revision());
}
