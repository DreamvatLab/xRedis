//! 连接 URI 拼装。凭据要百分号编码,否则密码或用户名里的 `@` `:` 会让 URL 结构错乱。

use urlencoding::encode;

use crate::config::ConnectionConfig;

/// 拼 `redis://` 或 `rediss://`(配了 TLS)的 URI,库号总是带上。
///
/// - 用户名和密码都编码(Redis ACL 用户名允许出现 `:` 和 `@`)。
/// - 只有用户名没有密码时不带认证段(和 v1 一致)。
/// - 空密码当作没有密码。
/// - IPv6 地址自动加方括号。
pub fn redis_uri(cfg: &ConnectionConfig, password: Option<&str>) -> String {
    let scheme = if cfg.tls.is_some() { "rediss" } else { "redis" };
    let password = password.filter(|p| !p.is_empty());
    let host = if cfg.host.contains(':') && !cfg.host.starts_with('[') {
        format!("[{}]", cfg.host)
    } else {
        cfg.host.clone()
    };
    let auth = match (cfg.username.as_deref().filter(|u| !u.is_empty()), password) {
        (Some(user), Some(pw)) => format!("{}:{}@", encode(user), encode(pw)),
        (None, Some(pw)) => format!(":{}@", encode(pw)),
        _ => String::new(),
    };
    format!("{scheme}://{auth}{host}:{}/{}", cfg.port, cfg.selected_db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnId, TlsConfig};

    fn cfg(username: Option<&str>) -> ConnectionConfig {
        ConnectionConfig {
            id: ConnId::new(),
            name: String::new(),
            host: "127.0.0.1".into(),
            port: 6379,
            ordinal: 0,
            selected_db: 0,
            username: username.map(str::to_string),
            tls: None,
            created_on_utc: 0,
            updated_on_utc: 0,
        }
    }

    #[test]
    fn no_credentials() {
        assert_eq!(redis_uri(&cfg(None), None), "redis://127.0.0.1:6379/0");
    }

    #[test]
    fn password_only() {
        assert_eq!(
            redis_uri(&cfg(None), Some("secret")),
            "redis://:secret@127.0.0.1:6379/0"
        );
    }

    #[test]
    fn username_and_password() {
        assert_eq!(
            redis_uri(&cfg(Some("alice")), Some("secret")),
            "redis://alice:secret@127.0.0.1:6379/0"
        );
    }

    #[test]
    fn username_without_password_omits_auth() {
        assert_eq!(
            redis_uri(&cfg(Some("alice")), None),
            "redis://127.0.0.1:6379/0"
        );
    }

    #[test]
    fn empty_password_is_no_password() {
        assert_eq!(redis_uri(&cfg(None), Some("")), "redis://127.0.0.1:6379/0");
    }

    #[test]
    fn password_special_chars_are_encoded() {
        let uri = redis_uri(&cfg(None), Some("p@ss:w/rd?#"));
        assert_eq!(uri, "redis://:p%40ss%3Aw%2Frd%3F%23@127.0.0.1:6379/0");
    }

    /// 用户名和密码一样要编码,否则 `we:ird` 会让 URL 结构错乱。
    #[test]
    fn username_special_chars_are_encoded() {
        assert_eq!(
            redis_uri(&cfg(Some("we:ird")), Some("pass")),
            "redis://we%3Aird:pass@127.0.0.1:6379/0"
        );
    }

    #[test]
    fn non_ascii_credentials_are_encoded() {
        let uri = redis_uri(&cfg(Some("用户")), Some("密码"));
        assert!(uri.is_ascii(), "{uri}");
    }

    #[test]
    fn tls_uses_rediss_scheme() {
        let mut c = cfg(None);
        c.tls = Some(TlsConfig::default());
        assert_eq!(redis_uri(&c, None), "rediss://127.0.0.1:6379/0");
    }

    #[test]
    fn selected_db_in_path() {
        let mut c = cfg(None);
        c.selected_db = 3;
        assert_eq!(
            redis_uri(&c, Some("secret")),
            "redis://:secret@127.0.0.1:6379/3"
        );
    }

    #[test]
    fn ipv6_host_is_bracketed() {
        let mut c = cfg(None);
        c.host = "::1".into();
        assert_eq!(redis_uri(&c, None), "redis://[::1]:6379/0");
        c.host = "[::1]".into();
        assert_eq!(redis_uri(&c, None), "redis://[::1]:6379/0");
    }
}
