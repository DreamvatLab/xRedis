//! Console 子页的无头测试(R-11a):输入、回车、输出、历史、clear、分词错误、等待状态、输出上限、
//! 按词删除、Ctrl+C。用 `Runtime::inline` 加 `FakeBackend` 驱动 `MainView`,键盘走真实的事件路径。

use std::time::Duration;

use gpui_kit::{AppContext as _, TestAppContext};
use xredis_app::Msg;
use xredis_app::console::{ConsoleMsg, OUTPUT_MAX_LINES};
use xredis_core::config::ConnId;

use super::dialogs::{press, type_into};
use super::keyview::open_tab;
use super::{Env, click, dispatch, exists, read, setup, wait};

/// 开一个连接,切到 Console 子页。
fn open_console(cx: &mut TestAppContext, env: &Env, id: ConnId) {
    open_tab(cx, env, id);
    click(cx, env, "subpage-console");
    assert!(exists(cx, env, "console-page"));
}

fn output(cx: &mut TestAppContext, env: &Env, id: ConnId) -> Vec<String> {
    read(cx, env, |a| {
        a.tab(id).unwrap().console.output().cloned().collect()
    })
}

fn input_text(cx: &mut TestAppContext, env: &Env, id: ConnId) -> String {
    cx.read(|cx| env.view.read(cx).console_input_text(id, cx).unwrap())
}

fn app_input(cx: &mut TestAppContext, env: &Env, id: ConnId) -> String {
    read(cx, env, |a| a.tab(id).unwrap().console.input().to_owned())
}

/// 输出控件里的文字和滚动位置(纵向,0 是顶,向下滚是负数)。
fn output_widget(cx: &mut TestAppContext, env: &Env, id: ConnId) -> (String, f32) {
    cx.read(|cx| env.view.read(cx).console_output_state(id, cx).unwrap())
}

/// 输入一行并回车,等命令执行完。
fn run(cx: &mut TestAppContext, env: &Env, id: ConnId, line: &str) {
    type_into(cx, env, "console-input", line);
    press(cx, env, "enter");
    wait(cx, env, "command finished", |a| {
        !a.tab(id).unwrap().console.is_running()
    });
}

#[gpui_kit::test]
fn the_page_has_its_controls_and_the_input_has_the_focus(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    for c in ["console-output", "console-input", "console-line"] {
        assert!(exists(cx, &env, c), "{c}");
    }
    assert!(!exists(cx, &env, "console-waiting"));
    let focused = cx
        .update_window(env.window, |_, window, cx| {
            env.view.read(cx).console_input_focused(id, window, cx)
        })
        .unwrap();
    assert!(focused, "switching to the console focuses the input");
}

#[gpui_kit::test]
fn typing_a_command_and_enter_runs_it_and_shows_the_output(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    type_into(cx, &env, "console-input", "PING");
    assert_eq!(
        app_input(cx, &env, id),
        "PING",
        "every change reaches the App"
    );
    press(cx, &env, "enter");
    wait(cx, &env, "reply", |a| {
        a.tab(id).unwrap().console.output_len() >= 2
    });
    assert_eq!(output(cx, &env, id), vec!["> PING", "PONG"]);
    assert_eq!(env.fake.commands(), vec![vec![b"PING".to_vec()]]);
    assert_eq!(input_text(cx, &env, id), "", "the input widget is cleared");
    let (text, _) = output_widget(cx, &env, id);
    assert_eq!(
        text, "> PING\nPONG",
        "the selectable output shows the lines"
    );
}

#[gpui_kit::test]
fn up_and_down_browse_the_history_and_bring_back_what_was_being_typed(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    run(cx, &env, id, "PING");
    run(cx, &env, id, "GET a");
    type_into(cx, &env, "console-input", "half");
    press(cx, &env, "up");
    assert_eq!(input_text(cx, &env, id), "GET a");
    assert_eq!(app_input(cx, &env, id), "GET a");
    press(cx, &env, "up");
    assert_eq!(input_text(cx, &env, id), "PING");
    press(cx, &env, "up");
    assert_eq!(input_text(cx, &env, id), "PING", "stops at the oldest");
    press(cx, &env, "down");
    assert_eq!(input_text(cx, &env, id), "GET a");
    press(cx, &env, "down");
    assert_eq!(input_text(cx, &env, id), "half", "the typed line is back");
    // 翻出来的历史可以直接回车
    press(cx, &env, "up");
    press(cx, &env, "enter");
    wait(cx, &env, "reply", |a| {
        !a.tab(id).unwrap().console.is_running()
    });
    assert_eq!(
        output(cx, &env, id)
            .iter()
            .filter(|l| *l == "> GET a")
            .count(),
        2
    );
}

#[gpui_kit::test]
fn clear_empties_the_output_without_asking_the_backend(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    run(cx, &env, id, "PING");
    assert!(!output(cx, &env, id).is_empty());
    let before = env.fake.commands().len();
    type_into(cx, &env, "console-input", "  CLEAR ");
    press(cx, &env, "enter");
    assert!(output(cx, &env, id).is_empty());
    assert_eq!(output_widget(cx, &env, id).0, "");
    assert_eq!(env.fake.commands().len(), before);
}

#[gpui_kit::test]
fn a_bad_quote_is_reported_and_nothing_is_sent(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    type_into(cx, &env, "console-input", "GET \"oops");
    press(cx, &env, "enter");
    let lines = output(cx, &env, id);
    assert_eq!(lines[0], "> GET \"oops");
    assert!(
        lines.last().unwrap().contains("unbalanced quotes"),
        "{lines:?}"
    );
    assert!(env.fake.commands().is_empty());
    assert!(!read(cx, &env, |a| a.tab(id).unwrap().console.is_running()));
}

/// 命令还在执行:App 说在等,超过 500 ms 画等待帧;结果回来就消失。
/// (`Runtime::inline` 在 `dispatch` 里同步跑完命令,结果在通道里等着,这时 App 还认为在执行。)
#[gpui_kit::test]
fn a_slow_command_shows_the_wait_frame_until_the_result_arrives(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    for msg in [ConsoleMsg::SetInput("PING".into()), ConsoleMsg::Submit] {
        dispatch(cx, &env, Msg::Console { id, msg });
    }
    assert!(read(cx, &env, |a| a.tab(id).unwrap().console.is_running()));
    assert!(!exists(cx, &env, "console-waiting"), "not before 500 ms");
    std::thread::sleep(Duration::from_millis(560));
    assert!(read(cx, &env, |a| a.console_waiting_frame(id).is_some()));
    assert!(exists(cx, &env, "console-waiting"), "after 500 ms");
    wait(cx, &env, "result", |a| {
        !a.tab(id).unwrap().console.is_running()
    });
    assert!(!exists(cx, &env, "console-waiting"));
    assert_eq!(output(cx, &env, id).last().unwrap(), "PONG");
}

#[gpui_kit::test]
fn the_output_keeps_the_last_ten_thousand_lines_and_scrolls_to_the_end(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    run(cx, &env, id, "BIG 10050");
    assert_eq!(
        read(cx, &env, |a| a.tab(id).unwrap().console.output_len()),
        OUTPUT_MAX_LINES
    );
    let (text, _) = output_widget(cx, &env, id);
    assert_eq!(text.lines().count(), OUTPUT_MAX_LINES);
    assert!(text.ends_with("10050) line 10050"));
    assert!(!text.contains("> BIG"), "the oldest lines are gone");
    assert!(exists(cx, &env, "console-output"), "renders");
    let (_, y) = output_widget(cx, &env, id);
    assert!(y < -100.0, "scrolled down to the end, offset {y}");
    // 到了上限,再来一条:内容变了,控件跟着变
    run(cx, &env, id, "PING");
    let (text, _) = output_widget(cx, &env, id);
    assert!(
        text.ends_with("> PING\nPONG"),
        "{}",
        &text[text.len().saturating_sub(40)..]
    );
}

#[gpui_kit::test]
fn ctrl_backspace_deletes_a_word_and_ctrl_left_moves_by_word(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    type_into(cx, &env, "console-input", "GET foo bar");
    press(cx, &env, "ctrl-backspace");
    assert_eq!(app_input(cx, &env, id), "GET foo ");
    press(cx, &env, "ctrl-left");
    press(cx, &env, "ctrl-backspace");
    assert_eq!(
        app_input(cx, &env, id),
        "foo ",
        "ctrl-left stopped at the start of foo, ctrl-backspace then deleted the word before it"
    );
}

#[gpui_kit::test]
fn ctrl_c_abandons_the_line_but_still_copies_a_selection(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a"]);
    let id = ids[0];
    open_console(cx, &env, id);
    type_into(cx, &env, "console-input", "GET abc");
    // 有选中的文字:复制,不放弃
    press(cx, &env, "secondary-a");
    press(cx, &env, "secondary-c");
    assert_eq!(app_input(cx, &env, id), "GET abc");
    assert!(output(cx, &env, id).is_empty());
    // 取消选中后再按:放弃当前行,留一行 ^C
    press(cx, &env, "right");
    press(cx, &env, "secondary-c");
    assert_eq!(output(cx, &env, id), vec!["> GET abc^C"]);
    assert_eq!(app_input(cx, &env, id), "");
    assert_eq!(input_text(cx, &env, id), "");
    assert!(env.fake.commands().is_empty());
}

#[gpui_kit::test]
fn each_tab_has_its_own_console(cx: &mut TestAppContext) {
    let (env, ids) = setup(cx, &["a", "b"]);
    open_console(cx, &env, ids[0]);
    run(cx, &env, ids[0], "PING");
    dispatch(cx, &env, Msg::OpenConnection(ids[1]));
    wait(cx, &env, "second tab", |a| {
        a.tabs.len() == 2 && a.can_request(ids[1])
    });
    click(cx, &env, "subpage-console");
    assert!(output(cx, &env, ids[1]).is_empty());
    assert_eq!(output(cx, &env, ids[0]).len(), 2);
    assert_eq!(output_widget(cx, &env, ids[1]).0, "");
}
