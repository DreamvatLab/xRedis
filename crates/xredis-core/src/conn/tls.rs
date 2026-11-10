//! TLS 连接器(rustls)。CA 可选,客户端私钥按 PEM 标签分派(L-12)。

use std::fs;
use std::path::Path;

use fred::rustls::pki_types::{
    PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
};
use fred::rustls::{ClientConfig, RootCertStore};
use fred::types::config::TlsConnector;

use crate::config::TlsConfig;
use crate::error::{Error, IoAction, Result, TlsProblem};

/// 读 PEM 文件,返回(标签,DER 内容)。标签就是 `-----BEGIN xxx-----` 里的 xxx。
fn read_pem(path: &Path) -> Result<(String, Vec<u8>)> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        action: IoAction::ReadCertFile,
        path: path.to_path_buf(),
        source,
    })?;
    let pem = pem::parse(bytes).map_err(|e| {
        Error::Tls(TlsProblem::NotPem {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })
    })?;
    Ok((pem.tag().to_string(), pem.into_contents()))
}

/// 按 PEM 标签识别私钥格式,rustls 需要正确的变体才能解析。
///
/// | 标签 | 格式 |
/// |---|---|
/// | `PRIVATE KEY` | PKCS#8 |
/// | `RSA PRIVATE KEY` | PKCS#1 |
/// | `EC PRIVATE KEY` | SEC1 |
pub(crate) fn parse_private_key(tag: &str, der: Vec<u8>) -> Result<PrivateKeyDer<'static>> {
    match tag {
        "PRIVATE KEY" => Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der))),
        "RSA PRIVATE KEY" => Ok(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der))),
        "EC PRIVATE KEY" => Ok(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der))),
        "ENCRYPTED PRIVATE KEY" => Err(Error::Tls(TlsProblem::KeyEncrypted)),
        other => Err(Error::Tls(TlsProblem::KeyFormatUnsupported {
            tag: other.to_string(),
        })),
    }
}

/// 组装 rustls 的客户端配置。
///
/// CA 可选:托管 Redis(Upstash、Redis Cloud 等)用公共 CA 签发,用户手上没有 CA 文件,
/// 不填就用系统根证书。客户端证书和私钥要么都给要么都不给。
pub fn connector(tls: &TlsConfig) -> Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    match tls.ca_cert.as_deref().filter(|p| !p.as_os_str().is_empty()) {
        Some(path) => {
            let (_, der) = read_pem(path)?;
            roots.add(der.into()).map_err(|e| {
                Error::Tls(TlsProblem::CaInvalid {
                    path: path.to_path_buf(),
                    detail: e.to_string(),
                })
            })?;
        }
        None => {
            let loaded = rustls_native_certs::load_native_certs();
            let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
            tracing::debug!("未提供 CA 证书,回落到系统根证书:加载 {added} 条,忽略 {ignored} 条");
        }
    }

    let cert = tls
        .client_cert
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty());
    let key = tls
        .client_key
        .as_deref()
        .filter(|p| !p.as_os_str().is_empty());
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let config = match (cert, key) {
        (Some(cert), Some(key)) => {
            let (_, cert_der) = read_pem(cert)?;
            let (tag, key_der) = read_pem(key)?;
            let key = parse_private_key(&tag, key_der)?;
            builder
                .with_client_auth_cert(vec![cert_der.into()], key)
                .map_err(|e| {
                    Error::Tls(TlsProblem::CertKeyMismatch {
                        detail: e.to_string(),
                    })
                })?
        }
        (None, None) => builder.with_no_client_auth(),
        _ => {
            return Err(Error::Tls(TlsProblem::CertKeyIncomplete));
        }
    };
    Ok(config.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn private_key_dispatches_on_pem_tag() {
        let der = vec![1, 2, 3];
        assert!(matches!(
            parse_private_key("PRIVATE KEY", der.clone()).unwrap(),
            PrivateKeyDer::Pkcs8(_)
        ));
        assert!(matches!(
            parse_private_key("RSA PRIVATE KEY", der.clone()).unwrap(),
            PrivateKeyDer::Pkcs1(_)
        ));
        assert!(matches!(
            parse_private_key("EC PRIVATE KEY", der).unwrap(),
            PrivateKeyDer::Sec1(_)
        ));
    }

    #[test]
    fn encrypted_and_unknown_private_keys_are_rejected_with_advice() {
        let err = parse_private_key("ENCRYPTED PRIVATE KEY", vec![1])
            .unwrap_err()
            .to_string();
        assert!(err.contains("openssl pkcs8"), "{err}");
        let err = parse_private_key("DSA PRIVATE KEY", vec![1])
            .unwrap_err()
            .to_string();
        assert!(err.contains("DSA PRIVATE KEY"), "{err}");
    }

    #[test]
    fn pem_tag_and_contents_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("k.pem");
        fs::write(
            &path,
            "-----BEGIN RSA PRIVATE KEY-----\nAQID\n-----END RSA PRIVATE KEY-----\n",
        )
        .unwrap();
        let (tag, der) = read_pem(&path).unwrap();
        assert_eq!(tag, "RSA PRIVATE KEY");
        assert_eq!(der, vec![1, 2, 3]);
    }

    #[test]
    fn missing_or_bad_pem_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_pem(&tmp.path().join("nope.pem")),
            Err(Error::Io { .. })
        ));
        let bad = tmp.path().join("bad.pem");
        fs::write(&bad, "not a pem").unwrap();
        assert!(matches!(
            read_pem(&bad),
            Err(Error::Tls(TlsProblem::NotPem { .. }))
        ));
    }

    /// L-12:CA 可选,不填回落到系统根证书,不在配置层拒绝。
    #[test]
    fn ca_is_optional() {
        assert!(connector(&TlsConfig::default()).is_ok());
    }

    #[test]
    fn client_cert_and_key_must_come_together() {
        let tls = TlsConfig {
            client_cert: Some(PathBuf::from("c.pem")),
            ..TlsConfig::default()
        };
        assert!(matches!(
            connector(&tls),
            Err(Error::Tls(TlsProblem::CertKeyIncomplete))
        ));
        let tls = TlsConfig {
            client_key: Some(PathBuf::from("k.pem")),
            ..TlsConfig::default()
        };
        assert!(matches!(
            connector(&tls),
            Err(Error::Tls(TlsProblem::CertKeyIncomplete))
        ));
    }

    /// R-06b:私钥问题是结构化的种类(加密的、格式不认识的),带出标签,不是一句英文。
    #[test]
    fn private_key_problems_are_structured() {
        assert!(matches!(
            parse_private_key("ENCRYPTED PRIVATE KEY", vec![1]),
            Err(Error::Tls(TlsProblem::KeyEncrypted))
        ));
        match parse_private_key("OPENSSH PRIVATE KEY", vec![1]) {
            Err(Error::Tls(TlsProblem::KeyFormatUnsupported { tag })) => {
                assert_eq!(tag, "OPENSSH PRIVATE KEY")
            }
            other => panic!("{:?}", other.err()),
        }
    }

    /// R-06b:CA 文件是合法 PEM 但内容不是证书:`CaInvalid` 带路径和库的报错。
    #[test]
    fn an_invalid_ca_certificate_carries_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let ca = tmp.path().join("ca.pem");
        fs::write(
            &ca,
            "-----BEGIN CERTIFICATE-----
AQID
-----END CERTIFICATE-----
",
        )
        .unwrap();
        let tls = TlsConfig {
            ca_cert: Some(ca.clone()),
            ..TlsConfig::default()
        };
        match connector(&tls) {
            Err(Error::Tls(TlsProblem::CaInvalid { path, detail })) => {
                assert_eq!(path, ca);
                assert!(!detail.is_empty());
            }
            other => panic!("{:?}", other.err()),
        }
    }

    #[test]
    fn missing_ca_file_is_error() {
        let tls = TlsConfig {
            ca_cert: Some(PathBuf::from("Z:/definitely/not/here.pem")),
            ..TlsConfig::default()
        };
        assert!(matches!(connector(&tls), Err(Error::Io { .. })));
    }
}
