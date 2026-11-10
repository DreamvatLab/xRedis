//! core 的错误翻译成给用户看的文字。core 的错误带结构化数据,`Display` 的英文只给日志用;
//! 这里按错误种类查语言包,名字等参数原样(有损 UTF-8)带进去。
//!
//! `match` 里**没有通配分支**:core 加了新的错误种类,这里编译不过,逼着加翻译。

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::config::ConfigWarning;
use xredis_core::error::{ClipboardStage, Entity, FieldRule, FormField, IoAction, TlsProblem};
use xredis_core::v1_import::ImportWarning;

use crate::msg::StartupWarning;

use crate::i18n::{tr, tr_with};

fn lossy(b: &Bytes) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn entity_suffix(e: Entity) -> &'static str {
    match e {
        Entity::Key => "key",
        Entity::HashField => "hash_field",
        Entity::SetMember => "set_member",
        Entity::ZSetMember => "zset_member",
    }
}

fn io_action_key(a: IoAction) -> &'static str {
    match a {
        IoAction::ReadV1Config => "err.io.read_v1_config",
        IoAction::ReadCertFile => "err.io.read_cert_file",
        IoAction::CreateConfigDir => "err.io.create_config_dir",
        IoAction::ReadConfigFile => "err.io.read_config_file",
        IoAction::WriteTempFile => "err.io.write_temp_file",
        IoAction::ReplaceConfigFile => "err.io.replace_config_file",
    }
}

fn field_key(f: FormField) -> &'static str {
    match f {
        FormField::ConnectionId => "field.connection_id",
        FormField::Host => "field.host",
        FormField::Port => "field.port",
        FormField::Ordinal => "field.ordinal",
        FormField::ScanBatch => "field.scan_batch",
        FormField::AutoScanLimit => "field.auto_scan_limit",
    }
}

fn describe_tls(lang: &str, p: &TlsProblem) -> String {
    let w = |key: &str, args: &[(&str, &str)]| tr_with(lang, key, args);
    match p {
        TlsProblem::NotPem { path, detail } => w(
            "err.tls.not_pem",
            &[("path", &path.display().to_string()), ("detail", detail)],
        ),
        TlsProblem::KeyEncrypted => tr(lang, "err.tls.key_encrypted"),
        TlsProblem::KeyFormatUnsupported { tag } => w("err.tls.key_unsupported", &[("tag", tag)]),
        TlsProblem::CaInvalid { path, detail } => w(
            "err.tls.ca_invalid",
            &[("path", &path.display().to_string()), ("detail", detail)],
        ),
        TlsProblem::CertKeyMismatch { detail } => {
            w("err.tls.cert_key_mismatch", &[("detail", detail)])
        }
        TlsProblem::CertKeyIncomplete => tr(lang, "err.tls.cert_key_incomplete"),
    }
}

fn stage_name(s: ClipboardStage) -> &'static str {
    match s {
        ClipboardStage::Base64 => "base64",
        ClipboardStage::Decompress => "decompress",
        ClipboardStage::Parse => "parse",
    }
}

/// 字节数的可读形式。
pub fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

/// 把错误翻译成界面文字。
pub fn describe(lang: &str, err: &Error) -> String {
    let w = |key: &str, args: &[(&str, &str)]| tr_with(lang, key, args);
    match err {
        Error::Serialize { detail } => w("err.serialize", &[("detail", detail)]),
        Error::ClientConfig { detail } => w("err.client_config", &[("detail", detail)]),
        Error::Tls(problem) => describe_tls(lang, problem),
        Error::Io {
            action,
            path,
            source,
        } => w(
            io_action_key(*action),
            &[
                ("path", &path.display().to_string()),
                ("detail", &source.to_string()),
            ],
        ),
        Error::KeyStore(d) => w("err.keystore", &[("detail", d)]),
        Error::InvalidField { field, rule } => {
            let field = tr(lang, field_key(*field));
            match rule {
                FieldRule::Required => w("err.field.required", &[("field", &field)]),
                FieldRule::Range { min, max } => w(
                    "err.field.range",
                    &[
                        ("field", &field),
                        ("min", &min.to_string()),
                        ("max", &max.to_string()),
                    ],
                ),
                FieldRule::AtMost { max } => w(
                    "err.field.at_most",
                    &[("field", &field), ("max", &max.to_string())],
                ),
                FieldRule::Invalid { value, detail } => w(
                    "err.field.invalid",
                    &[("field", &field), ("value", value), ("detail", detail)],
                ),
            }
        }
        Error::ConnectionNotFound(id) => w("err.connection_not_found", &[("id", &id.to_string())]),
        Error::ImportParse { path, detail } => w(
            "err.import_parse",
            &[("path", &path.display().to_string()), ("detail", detail)],
        ),
        Error::InvalidScoreReply { value } => {
            w("err.invalid_score_reply", &[("value", &lossy(value))])
        }
        Error::Connect(d) => w("err.connect", &[("detail", d)]),
        Error::Auth(d) => w("err.auth", &[("detail", d)]),
        Error::Timeout(d) => w("err.timeout", &[("detail", d)]),
        Error::Closed => tr(lang, "err.closed"),
        Error::Redis(d) => w("err.redis", &[("detail", d)]),
        Error::AlreadyExists {
            entity,
            name,
            container,
        } => {
            let key = format!("err.already_exists.{}", entity_suffix(*entity));
            let container = container.as_ref().map(lossy).unwrap_or_default();
            w(&key, &[("name", &lossy(name)), ("container", &container)])
        }
        Error::Missing {
            entity,
            name,
            container,
        } => {
            let key = format!("err.missing.{}", entity_suffix(*entity));
            let container = container.as_ref().map(lossy).unwrap_or_default();
            w(&key, &[("name", &lossy(name)), ("container", &container)])
        }
        Error::TtlZero => tr(lang, "err.ttl_zero"),
        Error::TtlOutOfRange { value, max } => w(
            "err.ttl_range",
            &[("value", &value.to_string()), ("max", &max.to_string())],
        ),
        Error::ListIndexInvalid { index, max } => w(
            "err.list_index_invalid",
            &[("index", &index.to_string()), ("max", &max.to_string())],
        ),
        Error::ListIndexOutOfRange { index, len } => w(
            "err.list_index_range",
            &[("index", &index.to_string()), ("len", &len.to_string())],
        ),
        Error::ScoreNaN => tr(lang, "err.score_nan"),
        Error::CopyTooLarge {
            total,
            limit,
            copied,
            key,
        } => w(
            "err.copy_too_large",
            &[
                ("total", &human_bytes(*total)),
                ("limit", &human_bytes(*limit)),
                ("copied", &copied.to_string()),
                ("key", &lossy(key)),
            ],
        ),
        Error::NothingCopied { skipped } => w(
            "err.nothing_copied",
            &[("skipped", &skipped.len().to_string())],
        ),
        Error::NotOpenable { kind } => w("err.not_openable", &[("kind", kind)]),
        Error::ClipboardNotRedis => tr(lang, "err.clipboard_not_redis"),
        Error::ClipboardCorrupt { stage, detail } => w(
            "err.clipboard_corrupt",
            &[("stage", stage_name(*stage)), ("detail", detail)],
        ),
        Error::PasteTooLarge { limit } => {
            w("err.paste_too_large", &[("limit", &human_bytes(*limit))])
        }
        Error::PasteConflicts { conflicts, total } => {
            // 最多列 10 个,其余只说数量
            let shown: Vec<String> = conflicts.iter().take(10).map(lossy).collect();
            let mut keys = shown.join(", ");
            if conflicts.len() > shown.len() {
                keys.push_str(&format!(", ... (+{})", conflicts.len() - shown.len()));
            }
            w(
                "err.paste_conflicts",
                &[
                    ("count", &conflicts.len().to_string()),
                    ("total", &total.to_string()),
                    ("keys", &keys),
                ],
            )
        }
        Error::RestoreFailed { key, detail } => w(
            "err.restore_failed",
            &[("key", &lossy(key)), ("detail", detail)],
        ),
        Error::CommandSyntax(e) => w("err.command_syntax", &[("detail", &e.to_string())]),
        Error::CommandRejected { command } => w("err.command_rejected", &[("command", command)]),
        Error::BlockingTimeout { command, secs } => w(
            "err.blocking_timeout",
            &[("command", command), ("secs", &secs.to_string())],
        ),
    }
}

/// 启动警告翻译成界面文字。`match` 里没有通配分支,加了新的警告种类这里编译不过。
pub fn describe_startup_warning(lang: &str, w: &StartupWarning) -> String {
    match w {
        StartupWarning::Config(ConfigWarning::CorruptConfig {
            path,
            reason,
            backup,
        }) => match backup {
            Some(b) => tr_with(
                lang,
                "warn.config_corrupt_backed_up",
                &[
                    ("path", &path.display().to_string()),
                    ("reason", reason),
                    ("backup", &b.display().to_string()),
                ],
            ),
            None => tr_with(
                lang,
                "warn.config_corrupt_not_backed_up",
                &[("path", &path.display().to_string()), ("reason", reason)],
            ),
        },
        StartupWarning::ConfigDirUnusable { reason, fallback } => tr_with(
            lang,
            "warn.config_dir_unusable",
            &[
                ("reason", reason),
                ("fallback", &fallback.display().to_string()),
            ],
        ),
    }
}

/// v1 导入的提示翻译成界面文字。
pub fn describe_import_warning(lang: &str, w: &ImportWarning) -> String {
    match w {
        ImportWarning::InvalidId { label } => {
            tr_with(lang, "import.invalid_id", &[("label", label)])
        }
        ImportWarning::InvalidPort { label, port } => tr_with(
            lang,
            "import.invalid_port",
            &[("label", label), ("port", &port.to_string())],
        ),
        ImportWarning::PasswordUndecryptable { label } => {
            tr_with(lang, "import.password_undecryptable", &[("label", label)])
        }
        ImportWarning::PasswordNotStored { label, reason } => tr_with(
            lang,
            "import.password_not_stored",
            &[("label", label), ("reason", reason)],
        ),
    }
}
