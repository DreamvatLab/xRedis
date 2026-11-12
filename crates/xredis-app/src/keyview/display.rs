//! 值和名字怎么显示:文本、有损文本、十六进制;JSON 的美化和压缩(P-21、P-29)。
//!
//! core 里一切都是字节,这里决定怎么给人看。含无法按 UTF-8 解码的字节时显示有损文本(无效字节
//! 变成 U+FFFD),并且**不能用有损文本写回**,否则原始字节会被永久替换掉,所以编辑器对这种值只读。

/// 一段字节的显示结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub text: String,
    /// 含无法解码的字节,`text` 是有损的。
    pub lossy: bool,
}

pub fn show(bytes: &[u8]) -> Shown {
    match std::str::from_utf8(bytes) {
        Ok(s) => Shown {
            text: s.to_owned(),
            lossy: false,
        },
        Err(_) => Shown {
            text: String::from_utf8_lossy(bytes).into_owned(),
            lossy: true,
        },
    }
}

pub fn is_lossy(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_err()
}

/// 十六进制视图:每行 16 字节,`偏移  十六进制  |ASCII|`,和 `hexdump -C` 的样子一致。
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 + 16);
    for (line, chunk) in bytes.chunks(16).enumerate() {
        out.push_str(&format!("{:08x} ", line * 16));
        for i in 0..16 {
            if i == 8 {
                out.push(' ');
            }
            match chunk.get(i) {
                Some(b) => out.push_str(&format!(" {b:02x}")),
                None => out.push_str("   "),
            }
        }
        out.push_str("  |");
        out.extend(chunk.iter().map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            }
        }));
        out.push_str("|\n");
    }
    out
}

/// 编辑器的语法类型。v2.0 只认 JSON,其余当纯文本(设计文档「有意差别」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Plain,
    Json,
}

impl Format {
    /// 语言下拉的选项(v2.0 只有这两种,设计文档「有意差别」)。
    pub const ALL: [Format; 2] = [Format::Plain, Format::Json];

    /// 下拉里显示的名字(专有名词,不翻译)。
    pub fn label(self) -> &'static str {
        match self {
            Format::Plain => "Plain text",
            Format::Json => "JSON",
        }
    }
}

/// 自动检测(P-21):能解析成 JSON 对象或数组才算 JSON;`123`、`"x"`、`true` 这类当纯文本。
pub fn detect_format(text: &str) -> Format {
    let t = text.trim_start();
    if (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        Format::Json
    } else {
        Format::Plain
    }
}

/// JSON 美化(两个空格缩进)。不是合法 JSON 返回 `Err(原因)`。
///
/// 按字符重排而不是解析再序列化:键的顺序、数字的写法(大整数、`1.0`、`1e5`)、字符串的转义都
/// 原样保留,格式化只动空白。
pub fn format_json(text: &str) -> Result<String, String> {
    check_json(text)?;
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    let indent = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            c if c.is_whitespace() => {}
            '{' | '[' => {
                out.push(c);
                let close = if c == '{' { '}' } else { ']' };
                // 跳过空白,空容器保持 `{}`、`[]`
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if chars.peek() == Some(&close) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    indent(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                indent(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                indent(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// JSON 压缩:去掉字符串之外的空白。
pub fn minify_json(text: &str) -> Result<String, String> {
    check_json(text)?;
    let mut out = String::with_capacity(text.len());
    let mut in_str = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    Ok(out)
}

fn check_json(text: &str) -> Result<(), String> {
    serde_json::from_str::<serde_json::Value>(text)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
