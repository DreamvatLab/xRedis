//! 控制台输出格式:模仿 redis-cli 的非 TTY 之外的交互样式(移植 v1 的 `format_custom_command_output`)。
//!
//! 和 v1 的差别:RESP3 的 Map 也要格式化(v1 显示 `(map unsupported yet)`)。
//! 字节用有损 UTF-8 显示,无效字节显示为 U+FFFD;`\xNN` 转义的是控制字符。

use std::fmt::Write;

use fred::types::Value;

use crate::value::value_to_bytes;

fn quote(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\x{:02x}", ch as u32 as u8);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => "(nil)".into(),
        Value::Boolean(b) => format!("({b})"),
        Value::Integer(i) => format!("(integer) {i}"),
        Value::Double(f) => format!("(float) {f}"),
        Value::String(s) => quote(s.as_bytes()),
        Value::Bytes(b) => quote(b),
        Value::Queued => "(queued)".into(),
        // 嵌套类型不走这里
        Value::Map(_) | Value::Array(_) => String::new(),
    }
}

/// 数组和 Map 的行。`level` 是嵌套深度,决定序号前的缩进:沿用 v1 的缩进规则
/// (第 0 层和第 1 层首元素前 2 个空格,更深层首元素前 1 个空格,其余前 3×(level+1) 个空格),
/// 这样 v1 用户看到的输出不变。
fn push_items(out: &mut String, items: &[Item<'_>], level: usize) {
    if items.is_empty() {
        out.push_str("(empty array)\n");
        return;
    }
    for (i, item) in items.iter().enumerate() {
        if level == 0 || (level == 1 && i == 0) {
            out.push_str("  ");
        } else if i == 0 {
            out.push(' ');
        } else {
            out.push_str(&" ".repeat(3 * (level + 1)));
        }
        let n = i + 1;
        match item {
            Item::Value(Value::Array(inner)) => {
                let _ = write!(out, "{n})");
                push_items(
                    out,
                    &inner.iter().map(Item::Value).collect::<Vec<_>>(),
                    level + 1,
                );
            }
            Item::Value(Value::Map(map)) => {
                let _ = write!(out, "{n})");
                push_items(out, &map_items(map), level + 1);
            }
            Item::Value(v) => {
                let _ = writeln!(out, "{n}) {}", scalar(v));
            }
            Item::Key(k) => {
                let _ = writeln!(out, "{n}) {}", quote(k));
            }
        }
    }
}

enum Item<'a> {
    Value(&'a Value),
    /// Map 的键(字节)。
    Key(Vec<u8>),
}

/// Map 按 redis-cli 的 RESP3 样式展开成“键、值、键、值”的平铺序列(redis-cli 的
/// `1# key => value` 样式要不同的缩进,这里先和数组一样编号,键排在值前面)。
fn map_items(map: &fred::types::Map) -> Vec<Item<'_>> {
    let mut entries: Vec<_> = map.iter().collect();
    // HashMap 的顺序不确定,按键排序让输出稳定
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut items = Vec::with_capacity(entries.len() * 2);
    for (k, v) in entries {
        items.push(Item::Key(k.as_bytes().to_vec()));
        items.push(Item::Value(v));
    }
    items
}

/// 格式化一个返回值。数组和 Map 逐行带序号,标量一行。
pub fn format_output(value: &Value) -> String {
    match value {
        Value::Array(arr) => {
            let mut out = String::new();
            push_items(
                &mut out,
                &arr.iter().map(Item::Value).collect::<Vec<_>>(),
                0,
            );
            out
        }
        Value::Map(map) => {
            let mut out = String::new();
            push_items(&mut out, &map_items(map), 0);
            out
        }
        other => scalar(other),
    }
}

/// 把二进制安全的值显示成有损文本,供 Pub/Sub 等处复用。
pub fn lossy(value: Value) -> String {
    String::from_utf8_lossy(&value_to_bytes(value)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use fred::types::Map;

    // ---- 以下移植自 v1 `src-tauri/src/tests/pure.rs` 的 command_output ----

    #[test]
    fn nil_and_scalars() {
        assert_eq!(format_output(&Value::Null), "(nil)");
        assert_eq!(format_output(&Value::Integer(42)), "(integer) 42");
        assert_eq!(format_output(&Value::Integer(-7)), "(integer) -7");
        assert_eq!(format_output(&Value::Double(1.5)), "(float) 1.5");
        assert_eq!(format_output(&Value::Boolean(true)), "(true)");
        assert_eq!(format_output(&Value::Boolean(false)), "(false)");
        assert_eq!(format_output(&Value::Queued), "(queued)");
    }

    #[test]
    fn strings_are_quoted() {
        assert_eq!(format_output(&Value::String("hello".into())), "\"hello\"");
        assert_eq!(format_output(&Value::String("".into())), "\"\"");
    }

    #[test]
    fn strings_escape_control_chars() {
        assert_eq!(format_output(&Value::String("a\nb".into())), "\"a\\nb\"");
        assert_eq!(
            format_output(&Value::String("a\tb\rc".into())),
            "\"a\\tb\\rc\""
        );
        assert_eq!(
            format_output(&Value::String("quote\"and\\slash".into())),
            "\"quote\\\"and\\\\slash\""
        );
        assert_eq!(
            format_output(&Value::String("a\u{0001}b".into())),
            "\"a\\x01b\""
        );
    }

    #[test]
    fn bytes_rendered_as_lossy_string() {
        assert_eq!(format_output(&Value::Bytes("raw".into())), "\"raw\"");
        // 非 UTF-8 字节显示为替换字符,不 panic
        assert_eq!(
            format_output(&Value::Bytes(Bytes::from_static(&[b'a', 0xff, b'b']))),
            "\"a\u{fffd}b\""
        );
    }

    #[test]
    fn empty_array() {
        assert_eq!(format_output(&Value::Array(vec![])), "(empty array)\n");
    }

    #[test]
    fn flat_array_is_numbered_from_one() {
        let v = Value::Array(vec![
            Value::String("a".into()),
            Value::Integer(2),
            Value::Null,
        ]);
        assert_eq!(
            format_output(&v),
            "  1) \"a\"\n  2) (integer) 2\n  3) (nil)\n"
        );
    }

    /// 嵌套数组:锁定 v1 的缩进(外层 "1)" 后面紧接内层首元素,中间两个空格)。
    #[test]
    fn nested_array_indentation_is_stable() {
        let v = Value::Array(vec![
            Value::Array(vec![Value::String("x".into()), Value::String("y".into())]),
            Value::String("z".into()),
        ]);
        let out = format_output(&v);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "  1)  1) \"x\"");
        assert_eq!(lines[1], "      2) \"y\"");
        assert_eq!(lines[2], "  2) \"z\"");
        assert_eq!(lines.len(), 3, "实际输出:\n{out}");
    }

    // ---- v2 新增:RESP3 Map ----

    fn map_of(pairs: Vec<(&str, Value)>) -> Map {
        pairs.into_iter().collect::<Map>()
    }

    /// v1 对 Map 显示 `(map unsupported yet)`;v2 展开成编号的键、值序列,键按字节序排列(稳定)。
    #[test]
    fn resp3_map_is_formatted_with_sorted_keys() {
        let m = map_of(vec![
            ("b", Value::Integer(2)),
            ("a", Value::String("x".into())),
        ]);
        let out = format_output(&Value::Map(m));
        assert!(!out.contains("unsupported"), "{out}");
        assert_eq!(
            out,
            "  1) \"a\"\n  2) \"x\"\n  3) \"b\"\n  4) (integer) 2\n"
        );
    }

    #[test]
    fn empty_map() {
        assert_eq!(format_output(&Value::Map(Map::new())), "(empty array)\n");
    }

    #[test]
    fn map_nested_in_an_array() {
        let m = map_of(vec![("k", Value::Integer(1))]);
        let v = Value::Array(vec![Value::Map(m), Value::Integer(9)]);
        let out = format_output(&v);
        assert!(out.contains("\"k\""), "{out}");
        assert!(out.contains("(integer) 1"), "{out}");
        assert!(out.trim_end().ends_with("(integer) 9"), "{out}");
    }

    #[test]
    fn binary_map_key_does_not_panic() {
        let mut m = Map::new();
        m.insert(
            fred::types::Key::from(Bytes::from_static(&[0xff, 0x00])),
            Value::Null,
        );
        let out = format_output(&Value::Map(m));
        assert!(out.contains("(nil)"), "{out}");
    }
}
