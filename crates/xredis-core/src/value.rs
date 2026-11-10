//! Redis 返回值和命令参数的字节转换。
//!
//! Redis 的 key、field、member、value 都是二进制,core 里一律用 `Bytes`,不做 UTF-8 解码。
//! fred 会把合法 UTF-8 的批量字符串给成 `Value::String`,其余给成 `Value::Bytes`,两种都要认。

use bytes::Bytes;
use fred::types::{ClusterHash, CustomCommand, Value};

use crate::error::{Error, Result};

/// 自定义命令。名字必须是静态字符串。
pub(crate) fn command(name: &'static str) -> CustomCommand {
    CustomCommand::new_static(name, ClusterHash::FirstKey, false)
}

/// 命令参数:原样传字节,不经过字符串。
pub(crate) fn arg(bytes: &[u8]) -> Value {
    Value::Bytes(Bytes::copy_from_slice(bytes))
}

pub(crate) fn arg_int(n: i64) -> Value {
    Value::Integer(n)
}

pub(crate) fn arg_str(s: &'static str) -> Value {
    Value::from(s)
}

/// 返回值转字节。不丢元素:空值也给一个空的 `Bytes`,保证下标不错位(L-15)。
pub(crate) fn value_to_bytes(value: Value) -> Bytes {
    match value {
        Value::Bytes(b) => b,
        Value::String(s) => Bytes::copy_from_slice(s.as_bytes()),
        Value::Null => Bytes::new(),
        other => Bytes::from(other.into_string().unwrap_or_default().into_bytes()),
    }
}

/// 返回值转 `Option<Bytes>`:不存在是 `None`。
pub(crate) fn value_to_opt_bytes(value: Value) -> Option<Bytes> {
    match value {
        Value::Null => None,
        other => Some(value_to_bytes(other)),
    }
}

/// ZSCAN / ZSCORE 返回的分数。
pub(crate) fn parse_score(bytes: &[u8]) -> Result<f64> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| {
            Error::Redis(format!(
                "Invalid score returned by server: {}",
                String::from_utf8_lossy(bytes)
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_convert_to_bytes_without_loss() {
        assert_eq!(value_to_bytes(Value::from("abc")), Bytes::from("abc"));
        assert_eq!(
            value_to_bytes(Value::Bytes(Bytes::from_static(&[0xff, 0xfe]))),
            Bytes::from_static(&[0xff, 0xfe])
        );
        assert_eq!(value_to_bytes(Value::Integer(42)), Bytes::from("42"));
        // 空值给空字节而不是丢掉,后面元素的下标才不会错位(L-15)
        assert_eq!(value_to_bytes(Value::Null), Bytes::new());
        assert_eq!(value_to_opt_bytes(Value::Null), None);
        assert_eq!(value_to_opt_bytes(Value::from("x")), Some(Bytes::from("x")));
    }

    #[test]
    fn scores_parse_including_infinity() {
        assert_eq!(parse_score(b"1.5").unwrap(), 1.5);
        assert_eq!(parse_score(b"-3").unwrap(), -3.0);
        assert_eq!(parse_score(b"inf").unwrap(), f64::INFINITY);
        assert_eq!(parse_score(b"-inf").unwrap(), f64::NEG_INFINITY);
        assert!(parse_score(b"abc").is_err());
        assert!(parse_score(&[0xff]).is_err());
    }
}
