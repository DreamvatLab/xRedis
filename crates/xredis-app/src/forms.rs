//! 表单的草稿和校验:连接编辑器(P-02)、设置(P-60)。输入规则都在这里,GUI 只负责显示和转发输入。

use std::path::PathBuf;

use xredis_core::config::{
    ConnId, ConnectionConfig, ConnectionDraft, PasswordChange, Settings, TlsConfig,
};
use xredis_core::consts::{
    AUTO_SCAN_LIMIT_MAX, ORDINAL_MAX, ORDINAL_MIN, SCAN_BATCH_MAX, SCAN_BATCH_MIN,
};

use crate::i18n::tr_with;
use crate::msg::{EditorText, FileField, SettingsText};

/// 表单校验不通过的原因。界面按种类翻译([`FormError::text`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    HostRequired,
    PortInvalid,
    OrdinalInvalid,
    CertPair,
    BatchInvalid,
    AutoScanInvalid,
}

impl FormError {
    pub fn text(self, lang: &str) -> String {
        let (min, max) = (ORDINAL_MIN.to_string(), ORDINAL_MAX.to_string());
        match self {
            FormError::HostRequired => tr_with(lang, "validation.host_required", &[]),
            FormError::PortInvalid => tr_with(lang, "validation.port_range", &[]),
            FormError::OrdinalInvalid => tr_with(
                lang,
                "validation.ordinal_range",
                &[("min", &min), ("max", &max)],
            ),
            FormError::CertPair => tr_with(lang, "validation.cert_pair", &[]),
            FormError::BatchInvalid => tr_with(
                lang,
                "validation.batch_range",
                &[
                    ("min", &SCAN_BATCH_MIN.to_string()),
                    ("max", &SCAN_BATCH_MAX.to_string()),
                ],
            ),
            FormError::AutoScanInvalid => tr_with(
                lang,
                "validation.auto_scan_range",
                &[("max", &AUTO_SCAN_LIMIT_MAX.to_string())],
            ),
        }
    }
}

/// 连接编辑器的草稿。文本字段都是用户输入的原样字符串,校验在 [`ConnectionEditor::validate`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionEditor {
    /// `None` 是新建。
    pub editing: Option<ConnId>,
    pub host: String,
    pub port: String,
    pub username: String,
    /// 编辑已有连接时密码框是空的(密码在密钥存储里,不会读出来显示):留空表示不改。
    pub password: String,
    /// 要清空已保存的密码(密码框留空时才有意义)。
    pub clear_password: bool,
    pub name: String,
    pub ordinal: String,
    pub tls: bool,
    pub ca_cert: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub saving: bool,
    /// 保存失败的原因(已翻译),显示在编辑器里。
    pub save_error: Option<String>,
}

impl ConnectionEditor {
    /// 新建连接的默认值(照 v1:localhost、6379、序号 0)。
    pub fn new_connection() -> Self {
        Self {
            editing: None,
            host: "localhost".into(),
            port: "6379".into(),
            username: String::new(),
            password: String::new(),
            clear_password: false,
            name: String::new(),
            ordinal: "0".into(),
            tls: false,
            ca_cert: None,
            client_cert: None,
            client_key: None,
            saving: false,
            save_error: None,
        }
    }

    /// 编辑已有连接:填入现有值,密码框留空。
    pub fn edit(cfg: &ConnectionConfig) -> Self {
        let tls = cfg.tls.clone();
        Self {
            editing: Some(cfg.id),
            host: cfg.host.clone(),
            port: cfg.port.to_string(),
            username: cfg.username.clone().unwrap_or_default(),
            password: String::new(),
            clear_password: false,
            name: cfg.name.clone(),
            ordinal: cfg.ordinal.to_string(),
            tls: tls.is_some(),
            ca_cert: tls.as_ref().and_then(|t| t.ca_cert.clone()),
            client_cert: tls.as_ref().and_then(|t| t.client_cert.clone()),
            client_key: tls.as_ref().and_then(|t| t.client_key.clone()),
            saving: false,
            save_error: None,
        }
    }

    pub fn set_text(&mut self, field: EditorText, value: String) {
        match field {
            EditorText::Host => self.host = value,
            EditorText::Port => self.port = value,
            EditorText::Username => self.username = value,
            EditorText::Password => self.password = value,
            EditorText::Name => self.name = value,
            EditorText::Ordinal => self.ordinal = value,
        }
        self.save_error = None;
    }

    pub fn set_file(&mut self, field: FileField, path: Option<PathBuf>) {
        match field {
            FileField::CaCert => self.ca_cert = path,
            FileField::ClientCert => self.client_cert = path,
            FileField::ClientKey => self.client_key = path,
        }
        self.save_error = None;
    }

    /// 校验。通过返回可以交给后端保存的草稿;不通过返回所有错误(不止第一个)。
    ///
    /// 和 v1 的差别:TLS 下 **CA 证书是可选的**(v1 前端强制要求填,是 bug:托管 Redis 用公共 CA,
    /// 用户手上没有 CA 文件);客户端证书和私钥要同时给。
    pub fn validate(&self) -> Result<ConnectionDraft, Vec<FormError>> {
        let mut errors = Vec::new();
        let host = self.host.trim();
        if host.is_empty() {
            errors.push(FormError::HostRequired);
        }
        let port = match self.port.trim().parse::<u32>() {
            Ok(p) if (1..=65535).contains(&p) => Some(p as u16),
            _ => {
                errors.push(FormError::PortInvalid);
                None
            }
        };
        // 序号留空当作 0;填了就必须是范围内的整数
        let ordinal = match self.ordinal.trim() {
            "" => Some(0),
            s => match s.parse::<i32>() {
                Ok(n) if (ORDINAL_MIN..=ORDINAL_MAX).contains(&n) => Some(n),
                _ => {
                    errors.push(FormError::OrdinalInvalid);
                    None
                }
            },
        };
        if self.tls && self.client_cert.is_some() != self.client_key.is_some() {
            errors.push(FormError::CertPair);
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let username = self.username.trim();
        let password = if !self.password.is_empty() {
            PasswordChange::Set(self.password.clone())
        } else if self.clear_password && self.editing.is_some() {
            PasswordChange::Clear
        } else {
            PasswordChange::Keep
        };
        Ok(ConnectionDraft {
            id: self.editing,
            name: self.name.trim().to_string(),
            host: host.to_string(),
            port: port.expect("checked above"),
            ordinal: ordinal.expect("checked above"),
            username: (!username.is_empty()).then(|| username.to_string()),
            tls: self.tls.then(|| TlsConfig {
                ca_cert: self.ca_cert.clone(),
                client_cert: self.client_cert.clone(),
                client_key: self.client_key.clone(),
            }),
            password,
        })
    }

    pub fn errors(&self) -> Vec<FormError> {
        self.validate().err().unwrap_or_default()
    }

    /// 保存按钮能不能点:输入合法且不在保存中。
    pub fn can_save(&self) -> bool {
        !self.saving && self.validate().is_ok()
    }
}

/// 设置面板的草稿(P-60)。每页条数没有了(虚拟滚动不分页)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsEditor {
    pub scan_batch: String,
    pub auto_scan_limit: String,
    pub show_key_details: bool,
    pub language: String,
    pub saving: bool,
}

impl SettingsEditor {
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            scan_batch: s.scan_batch.to_string(),
            auto_scan_limit: s.auto_scan_limit.to_string(),
            show_key_details: s.show_key_details,
            language: s.language.clone(),
            saving: false,
        }
    }

    pub fn set_text(&mut self, field: SettingsText, value: String) {
        match field {
            SettingsText::ScanBatch => self.scan_batch = value,
            SettingsText::AutoScanLimit => self.auto_scan_limit = value,
        }
    }

    /// 校验并套到现有设置上:只改设置面板能改的字段,窗口尺寸、折叠状态、协议记录保持不变。
    /// 单批数量 100 到 2000(步长 100),自动续扫上限 0 到 100000(步长 1000,0 是关闭)。
    pub fn apply(&self, base: &Settings) -> Result<Settings, Vec<FormError>> {
        let mut errors = Vec::new();
        let batch = match self.scan_batch.trim().parse::<u32>() {
            Ok(n) if (SCAN_BATCH_MIN..=SCAN_BATCH_MAX).contains(&n) && n % 100 == 0 => Some(n),
            _ => {
                errors.push(FormError::BatchInvalid);
                None
            }
        };
        let auto = match self.auto_scan_limit.trim().parse::<u32>() {
            Ok(n) if n <= AUTO_SCAN_LIMIT_MAX && n % 1000 == 0 => Some(n),
            _ => {
                errors.push(FormError::AutoScanInvalid);
                None
            }
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Settings {
            scan_batch: batch.expect("checked above"),
            auto_scan_limit: auto.expect("checked above"),
            show_key_details: self.show_key_details,
            language: self.language.clone(),
            ..base.clone()
        })
    }

    pub fn errors(&self, base: &Settings) -> Vec<FormError> {
        self.apply(base).err().unwrap_or_default()
    }
}
