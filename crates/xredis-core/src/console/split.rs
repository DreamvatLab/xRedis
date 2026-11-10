//! 控制台命令分词(对齐 redis-cli 的 sdssplitargs,移植 v1 的 `redisCommandParser.ts`)。
//!
//! 参数原样传给 Redis(含空格、JSON),不能让 fred 按空格拆碎(L-07)。
//!
//! 引号规则:
//! - 双引号内:支持 `\xHH`、`\n`、`\r`、`\t`、`\b`、`\a` 以及 `\\`、`\"` 转义
//! - 单引号内:只支持 `\'`,其余字符原样保留
//! - 引号闭合后必须紧跟空白或行尾(和 redis-cli 一致)
//!
//! 和 v1(按 UTF-16 字符处理,`\xHH` 变成一个 Unicode 码点)不同,这里**产出字节**:`\xHH` 就是
//! 一个字节,所以 `"\xff"` 能发出真正的 0xff,不是 UTF-8 的 `c3 bf`。

use std::fmt;

/// 分词失败的原因。出错时不发送命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitError {
    /// 引号没闭合。
    UnbalancedQuotes,
    /// 闭合引号后紧跟了非空白字符。
    TrailingGarbage,
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SplitError::UnbalancedQuotes => "unbalanced quotes",
            SplitError::TrailingGarbage => "unexpected characters after a closing quote",
        })
    }
}

fn is_space(c: u8) -> bool {
    // 和 JS 的 /\s/ 对 ASCII 一致:空格、制表、换行、回车、垂直制表、换页
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// 把一行命令拆成参数(字节)。空行得到空数组。
pub fn split(line: &str) -> Result<Vec<Vec<u8>>, SplitError> {
    let b = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && is_space(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let mut cur: Vec<u8> = Vec::new();
        #[derive(PartialEq)]
        enum Mode {
            Bare,
            Double,
            Single,
        }
        let mut mode = Mode::Bare;
        loop {
            match mode {
                Mode::Double => {
                    if i >= b.len() {
                        return Err(SplitError::UnbalancedQuotes);
                    }
                    let c = b[i];
                    if c == b'\\'
                        && i + 3 < b.len()
                        && b[i + 1] == b'x'
                        && let (Some(h), Some(l)) = (hex(b[i + 2]), hex(b[i + 3]))
                    {
                        cur.push(h * 16 + l);
                        i += 4;
                        continue;
                    }
                    if c == b'\\' && i + 1 < b.len() {
                        cur.push(match b[i + 1] {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            b'b' => 0x08,
                            b'a' => 0x07,
                            other => other,
                        });
                        i += 2;
                        continue;
                    }
                    if c == b'"' {
                        if i + 1 < b.len() && !is_space(b[i + 1]) {
                            return Err(SplitError::TrailingGarbage);
                        }
                        i += 1;
                        break;
                    }
                    cur.push(c);
                    i += 1;
                }
                Mode::Single => {
                    if i >= b.len() {
                        return Err(SplitError::UnbalancedQuotes);
                    }
                    let c = b[i];
                    if c == b'\\' && i + 1 < b.len() && b[i + 1] == b'\'' {
                        cur.push(b'\'');
                        i += 2;
                        continue;
                    }
                    if c == b'\'' {
                        if i + 1 < b.len() && !is_space(b[i + 1]) {
                            return Err(SplitError::TrailingGarbage);
                        }
                        i += 1;
                        break;
                    }
                    cur.push(c);
                    i += 1;
                }
                Mode::Bare => {
                    if i >= b.len() {
                        break;
                    }
                    let c = b[i];
                    if is_space(c) {
                        i += 1;
                        break;
                    }
                    match c {
                        b'"' => mode = Mode::Double,
                        b'\'' => mode = Mode::Single,
                        _ => cur.push(c),
                    }
                    i += 1;
                }
            }
        }
        tokens.push(cur);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(line: &str) -> Vec<Vec<u8>> {
        split(line).unwrap_or_else(|e| panic!("split failed for {line:?}: {e}"))
    }

    fn strs(line: &str) -> Vec<String> {
        tokens(line)
            .into_iter()
            .map(|t| String::from_utf8(t).unwrap())
            .collect()
    }

    // ---- 以下移植自 v1 `src/utils/__tests__/redisCommandParser.test.ts`,逐条对应 ----

    #[test]
    fn splits_plain_commands_on_whitespace() {
        assert_eq!(strs("GET mykey"), ["GET", "mykey"]);
        assert_eq!(strs("  PING  "), ["PING"]);
        assert_eq!(strs("SET a b"), ["SET", "a", "b"]);
        assert_eq!(strs("SET   a     b"), ["SET", "a", "b"]);
    }

    #[test]
    fn empty_input_gives_empty_array() {
        assert!(tokens("").is_empty());
        assert!(tokens("   ").is_empty());
    }

    /// L-07:参数含空格不能被拆碎,引号被去掉。
    #[test]
    fn l07_double_quoted_spaces_are_kept_and_quotes_removed() {
        assert_eq!(strs(r#"SET k "hello world""#), ["SET", "k", "hello world"]);
        assert_eq!(strs(r#"SET k "a b c d""#), ["SET", "k", "a b c d"]);
    }

    #[test]
    fn single_quotes_work_too() {
        assert_eq!(strs("SET k 'hello world'"), ["SET", "k", "hello world"]);
    }

    #[test]
    fn double_quote_escapes() {
        assert_eq!(
            strs(r#"SET k "line1\nline2""#),
            ["SET", "k", "line1\nline2"]
        );
        assert_eq!(strs(r#"SET k "tab\there""#), ["SET", "k", "tab\there"]);
        assert_eq!(
            strs(r#"SET k "quote\"inside""#),
            ["SET", "k", "quote\"inside"]
        );
        assert_eq!(strs(r#"SET k "back\\slash""#), ["SET", "k", "back\\slash"]);
        assert_eq!(strs(r#"SET k "\x41\x42""#), ["SET", "k", "AB"]);
    }

    #[test]
    fn single_quotes_only_handle_escaped_single_quote() {
        assert_eq!(strs(r"SET k 'it\'s'"), ["SET", "k", "it's"]);
        // 单引号里的 \n 不是换行,是字面量
        assert_eq!(strs(r"SET k 'a\nb'"), ["SET", "k", r"a\nb"]);
    }

    /// L-07:JSON 值原样传入。
    #[test]
    fn l07_json_values_pass_through() {
        assert_eq!(
            strs(r#"SET cfg "{\"a\": 1, \"b\": [2, 3]}""#),
            ["SET", "cfg", r#"{"a": 1, "b": [2, 3]}"#]
        );
    }

    #[test]
    fn subcommands_stay_separate_tokens() {
        assert_eq!(strs("CONFIG GET databases"), ["CONFIG", "GET", "databases"]);
        assert_eq!(strs("CLIENT LIST"), ["CLIENT", "LIST"]);
        assert_eq!(
            strs("ACL SETUSER alice on"),
            ["ACL", "SETUSER", "alice", "on"]
        );
    }

    #[test]
    fn unclosed_quote_is_an_error_not_wrong_tokens() {
        assert_eq!(
            split(r#"SET k "unclosed"#),
            Err(SplitError::UnbalancedQuotes)
        );
        assert_eq!(split("SET k 'unclosed"), Err(SplitError::UnbalancedQuotes));
    }

    #[test]
    fn garbage_after_closing_quote_is_an_error() {
        assert_eq!(split(r#"SET k "abc"def"#), Err(SplitError::TrailingGarbage));
        assert_eq!(split("SET k 'abc'def"), Err(SplitError::TrailingGarbage));
    }

    #[test]
    fn empty_string_argument_is_kept() {
        assert_eq!(
            tokens(r#"SET k """#),
            vec![b"SET".to_vec(), b"k".to_vec(), vec![]]
        );
    }

    // ---- v2 新增:字节层 ----

    /// `\xHH` 是一个字节,不是 UTF-8 编码的码点(v1 在这里会得到 c3 bf)。
    #[test]
    fn hex_escapes_produce_raw_bytes() {
        assert_eq!(tokens(r#"SET k "\xff\xfe\x00""#)[2], vec![0xff, 0xfe, 0x00]);
    }

    #[test]
    fn other_escapes_and_bad_hex() {
        assert_eq!(tokens(r#"X "\a\b\r""#)[1], vec![0x07, 0x08, b'\r']);
        // 不是合法的十六进制:\x 后面按普通转义处理,取下一个字符本身
        assert_eq!(tokens(r#"X "\xZZ""#)[1], b"xZZ".to_vec());
        // 未知转义取字符本身
        assert_eq!(tokens(r#"X "\q""#)[1], b"q".to_vec());
    }

    #[test]
    fn non_ascii_text_passes_through_as_utf8() {
        assert_eq!(
            strs(r#"SET 会话:中文 "你好 😀""#),
            ["SET", "会话:中文", "你好 😀"]
        );
    }

    /// 引号可以在 token 中间(redis-cli 同):`ab"c d"` 在闭合引号处结束,因为后面紧跟空白。
    #[test]
    fn quote_in_the_middle_of_a_bare_token() {
        assert_eq!(strs(r#"SET a"b c" d"#), ["SET", "ab c", "d"]);
    }

    #[test]
    fn tabs_and_newlines_separate_tokens() {
        assert_eq!(strs("GET\tk\nv"), ["GET", "k", "v"]);
    }

    #[test]
    fn trailing_backslash_inside_quotes_is_unbalanced() {
        assert_eq!(split(r#"SET k "abc\"#), Err(SplitError::UnbalancedQuotes));
    }
}
