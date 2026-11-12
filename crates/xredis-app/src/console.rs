//! 控制台的视图模型(R-07c,P-50):一行输入加光标、命令历史、输出、等待动画。
//!
//! 和 `KeyList` 一样是同步纯逻辑:`update(ctx, msg)` 返回要执行的命令。GUI 把按键翻译成 [`ConsoleMsg`]
//! (字符、方向键、Ctrl+方向键……),按 `output()` 和 `input()` 画。分词、危险命令、阻塞命令超时、
//! 输出格式化在 core(`xredis_core::console`),这里只管界面状态。
//!
//! 光标位置按**字符**数(不是字节),中文和表情不会被切开。

use std::collections::VecDeque;

use xredis_core::Error;
use xredis_core::console::split;

use crate::errors::describe;
use crate::keylist::Ctx;
use crate::msg::{BackendCmd, Cmd, Msg, PlatformCmd};

/// 提示符。
pub const PROMPT: &str = "> ";
/// 输出最多保留的行数,超出丢最早的(P-50)。
pub const OUTPUT_MAX_LINES: usize = 10_000;
/// 历史最多 100 条,连续重复的不记录(P-50)。
pub const HISTORY_MAX: usize = 100;
/// 命令超过这么久没返回,显示等待动画(P-50)。
pub const WAIT_ANIMATION_AFTER_MS: i64 = 500;
/// 动画每一帧的时长。
pub const WAIT_FRAME_MS: i64 = 100;
const WAIT_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Debug)]
pub enum ConsoleMsg {
    // ---- 编辑 ----
    /// 在光标处插入文本(打字、粘贴)。换行换成空格:控制台只有一行。
    Insert(String),
    /// 整行换成这段文字,光标放到末尾(R-11a:GUI 的输入框自己处理光标、选区、输入法、按单词移动和删除,
    /// 改动后把整行同步进来)。和现在一样就什么也不做,光标不动。不动历史浏览的位置(和 `Insert` 一致)。
    SetInput(String),
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    /// Ctrl+退格:删到当前单词开头。
    DeleteWordBack,
    /// Ctrl+Delete:删到下一个单词开头。
    DeleteWordForward,
    // ---- 历史 ----
    HistoryPrev,
    HistoryNext,
    // ---- 执行 ----
    Submit,
    /// Ctrl+C:放弃当前行。
    Interrupt,

    // ---- 异步结果 ----
    /// 等待动画的定时器到点(`App` 排的,GUI 只负责到点发回来)。命令还在执行就再排下一帧。
    Tick {
        generation: u64,
    },
    Executed {
        generation: u64,
        result: xredis_core::Result<String>,
    },
}

impl ConsoleMsg {
    /// 异步结果(不是用户操作)。
    pub fn is_result(&self) -> bool {
        matches!(self, ConsoleMsg::Executed { .. } | ConsoleMsg::Tick { .. })
    }
}

#[derive(Debug, Clone, Copy)]
struct Running {
    generation: u64,
    since_ms: i64,
}

#[derive(Debug, Default)]
pub struct ConsoleOut {
    pub cmds: Vec<Cmd>,
}

#[derive(Debug)]
pub struct Console {
    input: String,
    /// 光标在第几个字符前(0 到字符数)。
    cursor: usize,
    history: Vec<String>,
    /// 正在浏览的历史:0 是最新的一条;`None` 是没在浏览(v1 的 -1)。
    history_pos: Option<usize>,
    /// 开始浏览历史时的输入,回到最新时恢复。
    temp_input: String,
    output: VecDeque<String>,
    running: Option<Running>,
    generation: u64,
    /// 输出每变一次加一(GUI 靠它知道要不要重设输出控件;输出到了上限时行数不变,不能靠行数)。
    output_revision: u64,
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Console {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            temp_input: String::new(),
            output: VecDeque::new(),
            running: None,
            generation: 0,
            output_revision: 0,
        }
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    /// 光标位置(字符数)。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// 输出的每一行(从最早到最新)。
    pub fn output(&self) -> impl Iterator<Item = &String> {
        self.output.iter()
    }

    pub fn output_len(&self) -> usize {
        self.output.len()
    }

    /// 输出每变一次就不一样。
    pub fn output_revision(&self) -> u64 {
        self.output_revision
    }

    /// 整段输出,行之间用换行(GUI 放进一个只读的可选中控件,P-50 的“可以选中复制”)。
    pub fn output_text(&self) -> String {
        let mut text = String::new();
        for (i, line) in self.output.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            text.push_str(line);
        }
        text
    }

    /// 有命令在执行(这时不接受新的提交)。
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// 等待动画的当前一帧:命令执行超过 500 ms 才有,GUI 定时调用并重画。
    pub fn waiting_frame(&self, now: i64) -> Option<char> {
        let r = self.running?;
        let waited = now - r.since_ms;
        if waited < WAIT_ANIMATION_AFTER_MS {
            return None;
        }
        let i = ((waited - WAIT_ANIMATION_AFTER_MS) / WAIT_FRAME_MS) as usize % WAIT_FRAMES.len();
        Some(WAIT_FRAMES[i])
    }

    /// 连接重建了(重连、切库):在执行的命令作废,迟到的结果丢弃;输出和历史保留,输入行清空。
    pub fn reset_connection(&mut self) {
        self.generation += 1;
        self.running = None;
        self.reset_line();
    }

    fn reset_line(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.temp_input.clear();
    }

    // ---- 字符和下标 ----

    fn char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// 第 `n` 个字符的字节下标(`n` 等于字符数时是字符串长度)。
    fn byte_at(&self, n: usize) -> usize {
        self.input
            .char_indices()
            .nth(n)
            .map_or(self.input.len(), |(i, _)| i)
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// 向右一个单词:跳过当前单词剩下的部分,再跳过空白,停在下一个单词开头(v1 同)。
    fn word_right(&self, from: usize) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = from;
        while i < chars.len() && Self::is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && !Self::is_word(chars[i]) {
            i += 1;
        }
        i
    }

    /// 向左一个单词:先跳过光标前的非单词字符,再跳过单词,停在单词开头。
    /// (v1 的向左是反的:光标在空格后时停在上一个单词的末尾。)
    fn word_left(&self, from: usize) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = from.min(chars.len());
        while i > 0 && !Self::is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && Self::is_word(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    fn remove_range(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let (a, b) = (self.byte_at(from), self.byte_at(to));
        self.input.replace_range(a..b, "");
        self.cursor = from;
    }

    pub fn update(&mut self, ctx: &Ctx, msg: ConsoleMsg) -> ConsoleOut {
        let mut out = ConsoleOut::default();
        match msg {
            ConsoleMsg::Insert(text) => {
                let text = one_line(&text);
                let at = self.byte_at(self.cursor);
                self.input.insert_str(at, &text);
                self.cursor += text.chars().count();
            }
            ConsoleMsg::SetInput(text) => {
                let text = one_line(&text);
                if text != self.input {
                    self.input = text;
                    self.cursor = self.char_len();
                }
            }
            ConsoleMsg::Backspace => {
                if self.cursor > 0 {
                    self.remove_range(self.cursor - 1, self.cursor);
                }
            }
            ConsoleMsg::Delete => {
                if self.cursor < self.char_len() {
                    self.remove_range(self.cursor, self.cursor + 1);
                }
            }
            ConsoleMsg::Left => self.cursor = self.cursor.saturating_sub(1),
            ConsoleMsg::Right => self.cursor = (self.cursor + 1).min(self.char_len()),
            ConsoleMsg::WordLeft => self.cursor = self.word_left(self.cursor),
            ConsoleMsg::WordRight => self.cursor = self.word_right(self.cursor),
            ConsoleMsg::Home => self.cursor = 0,
            ConsoleMsg::End => self.cursor = self.char_len(),
            ConsoleMsg::DeleteWordBack => {
                let from = self.word_left(self.cursor);
                self.remove_range(from, self.cursor);
            }
            ConsoleMsg::DeleteWordForward => {
                let to = self.word_right(self.cursor);
                self.remove_range(self.cursor, to);
            }
            ConsoleMsg::HistoryPrev => self.browse_history(true),
            ConsoleMsg::HistoryNext => self.browse_history(false),
            ConsoleMsg::Interrupt => {
                self.push_line(format!("{PROMPT}{}^C", self.input));
                self.reset_line();
            }
            ConsoleMsg::Submit => self.submit(ctx, &mut out),
            ConsoleMsg::Tick { generation } => {
                if self.running.is_some_and(|r| r.generation == generation) {
                    out.cmds.push(tick(ctx, generation, WAIT_FRAME_MS));
                }
            }
            ConsoleMsg::Executed { generation, result } => {
                self.on_executed(ctx, generation, result)
            }
        }
        out
    }

    // ---- 历史 ----

    fn add_history(&mut self, line: &str) {
        if self.history.last().is_some_and(|last| last == line) {
            return;
        }
        self.history.push(line.to_owned());
        if self.history.len() > HISTORY_MAX {
            let extra = self.history.len() - HISTORY_MAX;
            self.history.drain(..extra);
        }
    }

    /// ↑ 浏览更早的,↓ 浏览更新的;到最新再按 ↓ 恢复开始浏览前输入的内容。
    fn browse_history(&mut self, older: bool) {
        if older && self.history_pos.is_none() {
            self.temp_input = self.input.clone();
        }
        let new = match (older, self.history_pos) {
            (true, _) if self.history.is_empty() => return,
            (true, None) => Some(0),
            (true, Some(i)) => Some((i + 1).min(self.history.len() - 1)),
            (false, None) => return,
            (false, Some(0)) => None,
            (false, Some(i)) => Some(i - 1),
        };
        self.history_pos = new;
        self.input = match new {
            None => self.temp_input.clone(),
            Some(i) => self.history[self.history.len() - 1 - i].clone(),
        };
        self.cursor = self.char_len();
    }

    // ---- 执行 ----

    fn push_line(&mut self, line: String) {
        self.output_revision += 1;
        self.output.push_back(line);
        while self.output.len() > OUTPUT_MAX_LINES {
            self.output.pop_front();
        }
    }

    /// 一段输出按行加入。只留最后 `OUTPUT_MAX_LINES` 行,巨大的输出不会先全部变成行再丢。
    fn push_text(&mut self, text: &str) {
        let lines: Vec<&str> = text.split('\n').collect();
        let skip = lines.len().saturating_sub(OUTPUT_MAX_LINES);
        for l in &lines[skip..] {
            self.push_line(l.trim_end_matches('\r').to_owned());
        }
    }

    fn submit(&mut self, ctx: &Ctx, out: &mut ConsoleOut) {
        if self.running.is_some() {
            return;
        }
        let line = self.input.clone();
        self.reset_line();
        self.push_line(format!("{PROMPT}{line}"));
        if line.trim().is_empty() {
            return;
        }
        self.add_history(&line);
        // clear 是本地命令,不发给后端
        if line.trim().eq_ignore_ascii_case("clear") {
            self.output.clear();
            self.output_revision += 1;
            return;
        }
        // 先在本地分词:引号没闭合、闭合引号后紧跟非空白都报错,不发送
        let tokens = match split(&line) {
            Ok(t) => t,
            Err(e) => {
                let text = describe(ctx.lang, &Error::CommandSyntax(e));
                self.push_text(&text);
                return;
            }
        };
        if tokens.is_empty() {
            return;
        }
        self.generation += 1;
        self.running = Some(Running {
            generation: self.generation,
            since_ms: ctx.now,
        });
        out.cmds.push(Cmd::Backend(BackendCmd::RunCommand {
            id: ctx.id,
            generation: self.generation,
            tokens,
        }));
        out.cmds
            .push(tick(ctx, self.generation, WAIT_ANIMATION_AFTER_MS));
    }

    fn on_executed(&mut self, ctx: &Ctx, generation: u64, result: xredis_core::Result<String>) {
        if self.running.is_none_or(|r| r.generation != generation) {
            return;
        }
        self.running = None;
        match result {
            Ok(text) => self.push_text(&text),
            Err(err) => {
                let text = describe(ctx.lang, &err);
                self.push_text(&text);
            }
        }
    }
}

/// 控制台只有一行:换行换成空格,去掉 NUL。
fn one_line(text: &str) -> String {
    text.chars()
        .filter(|c| *c != '\0')
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

/// 过 `ms` 毫秒发一条 `Tick`:动画从 500 ms 起,之后每帧一次,命令结束就不再排。
fn tick(ctx: &Ctx, generation: u64, ms: i64) -> Cmd {
    Cmd::Platform(PlatformCmd::After {
        ms: ms.unsigned_abs(),
        msg: Box::new(Msg::Console {
            id: ctx.id,
            msg: ConsoleMsg::Tick { generation },
        }),
    })
}

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;
