//! 值编辑器的草稿状态(P-20 至 P-30):String 的值、成员的值、新建 key、新建成员,共用这一个。
//!
//! 大段文本的编辑缓冲在 GUI 的输入控件里,每次改动把当前文本交给这里(`set_*`);这里保存
//! 原值,用来判断有没有改动、撤销、算出要执行的写操作(`save_ops`)。

use bytes::Bytes;
use xredis_core::keys::KeyType;
use xredis_core::members::{Member, MemberOp, NewContent, WriteOp};
use xredis_core::types::list::Side;
use xredis_core::types::string::SetMode;

use std::borrow::Cow;

use super::display::{
    Format, HEX_DUMP_LIMIT, Shown, detect_format, format_json, hex_dump, minify_json, show,
};
use crate::errors::human_bytes;
use crate::i18n::{tr, tr_with};

/// 一个输入框:原值(字节)和当前文本。
#[derive(Debug, Clone)]
pub struct Field {
    orig: Bytes,
    shown: Shown,
    pub text: String,
}

impl Field {
    pub fn new(orig: Bytes) -> Self {
        let shown = show(&orig);
        Self {
            text: shown.text.clone(),
            orig,
            shown,
        }
    }

    pub fn empty() -> Self {
        Self::new(Bytes::new())
    }

    pub fn changed(&self) -> bool {
        self.text != self.shown.text
    }

    /// 原值含无法解码的字节。
    pub fn lossy(&self) -> bool {
        self.shown.lossy
    }

    pub fn orig(&self) -> &Bytes {
        &self.orig
    }

    pub fn bytes(&self) -> Bytes {
        Bytes::from(self.text.clone().into_bytes())
    }

    fn undo(&mut self) {
        self.text = self.shown.text.clone();
    }
}

/// 编辑的是什么。
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// 已有的 String key:可以改 key 名和值。
    StringKey { key: Bytes },
    /// 新建 key(任意类型):key 名加第一个内容。
    NewKey { kind: KeyType },
    /// 已有成员。
    Member { key: Bytes, kind: KeyType },
    /// 给已有 key 加成员。
    NewMember { key: Bytes, kind: KeyType },
}

/// 哪些输入框要显示(GUI 照着画)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    pub key_name: bool,
    pub member_name: bool,
    /// List 新增时选头插还是尾插。
    pub position: bool,
    pub value: bool,
}

/// 保存前的校验失败。界面按种类翻译。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormError {
    ReadOnly,
    NothingChanged,
    KeyNameEmpty,
    MemberNameEmpty,
    /// 分数不是数字(含 NaN)。
    ScoreInvalid,
}

/// 编辑器的按钮哪些可用(GUI 照着画,不自己判断)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorActions {
    pub save: bool,
    pub undo: bool,
    /// Format 和 Minify:语言是 JSON、值可编辑、不在十六进制视图里。
    pub format: bool,
    pub minify: bool,
    /// 切十六进制视图:已有的值读完之后都可以看,新建的没有原值。
    pub hex: bool,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub target: Target,
    pub key: Field,
    pub member: Field,
    pub value: Field,
    pub position: Side,
    /// 值还在读(String);读完之前不能保存。
    pub loading: bool,
    pub saving: bool,
    /// 超过 8 MiB 的 String 只读了前 1 MiB:只读,值是真实长度。
    pub truncated: Option<u64>,
    /// 十六进制视图(只读查看,不改草稿)。
    pub hex: bool,
    pub format: Format,
}

impl Editor {
    fn blank(target: Target) -> Self {
        Self {
            target,
            key: Field::empty(),
            member: Field::empty(),
            value: Field::empty(),
            position: Side::Tail,
            loading: false,
            saving: false,
            truncated: None,
            hex: false,
            format: Format::Plain,
        }
    }

    /// 打开已有的 String:值要异步读,读完调 [`Editor::loaded_string`]。
    pub fn string_key(key: Bytes) -> Self {
        let mut e = Self::blank(Target::StringKey { key: key.clone() });
        e.key = Field::new(key);
        e.loading = true;
        e
    }

    /// 新建 key。
    pub fn new_key(kind: KeyType) -> Self {
        Self::blank(Target::NewKey { kind })
    }

    /// 打开一个已有成员(行里的数据已经是最新读到的)。
    pub fn member(key: Bytes, kind: KeyType, row: &Member) -> Self {
        let mut e = Self::blank(Target::Member { key, kind });
        match row {
            Member::Hash { field, value } => {
                e.member = Field::new(field.clone());
                e.value = Field::new(value.clone());
            }
            Member::List { index, value } => {
                e.member = Field::new(Bytes::from(index.to_string()));
                e.value = Field::new(value.clone());
            }
            // Set 的成员就是“值”
            Member::Set { member } => e.value = Field::new(member.clone()),
            Member::ZSet { member, score } => {
                e.member = Field::new(member.clone());
                e.value = Field::new(Bytes::from(score.to_string()));
            }
        }
        e.format = detect_format(&e.value.text);
        e
    }

    pub fn new_member(key: Bytes, kind: KeyType) -> Self {
        Self::blank(Target::NewMember { key, kind })
    }

    /// String 的值读到了。`total_len` 是服务端的真实长度,`truncated` 表示只读了前一部分。
    pub fn loaded_string(&mut self, data: Bytes, total_len: u64, truncated: bool) {
        self.value = Field::new(data);
        self.format = detect_format(&self.value.text);
        self.truncated = truncated.then_some(total_len);
        self.loading = false;
    }

    /// 这个编辑器就是整个视图(String 和新建 key),不是成员表上的弹窗。
    pub fn is_key_level(&self) -> bool {
        matches!(
            self.target,
            Target::StringKey { .. } | Target::NewKey { .. }
        )
    }

    pub fn is_new(&self) -> bool {
        matches!(
            self.target,
            Target::NewKey { .. } | Target::NewMember { .. }
        )
    }

    pub fn kind(&self) -> KeyType {
        match &self.target {
            Target::StringKey { .. } => KeyType::String,
            Target::NewKey { kind }
            | Target::Member { kind, .. }
            | Target::NewMember { kind, .. } => kind.clone(),
        }
    }

    pub fn inputs(&self) -> Inputs {
        let kind = self.kind();
        let new_key = matches!(self.target, Target::NewKey { .. });
        let hash_or_zset = matches!(kind, KeyType::Hash | KeyType::ZSet);
        Inputs {
            key_name: matches!(self.target, Target::StringKey { .. }) || new_key,
            // 已有 List 成员显示下标(不可改),新增 List 成员选位置
            member_name: hash_or_zset || (kind == KeyType::List && !self.is_new()),
            position: kind == KeyType::List && self.is_new(),
            value: true,
        }
    }

    /// 成员名(或 List 下标)能不能改。
    pub fn member_name_editable(&self) -> bool {
        !(self.kind() == KeyType::List && !self.is_new())
    }

    /// key 名能不能改:二进制的名字不能改(有损文本改回去会落到别的 key 上)。
    pub fn key_name_editable(&self) -> bool {
        match &self.target {
            Target::NewKey { .. } => true,
            Target::StringKey { .. } => !self.key.lossy(),
            _ => false,
        }
    }

    /// 值或成员名是二进制、或 String 只读了一部分时只读:改了也不算有改动,保存不写回。
    /// 新建的永远可编辑。
    pub fn read_only(&self) -> bool {
        if self.is_new() {
            return false;
        }
        self.value.lossy() || self.member.lossy() || self.truncated.is_some()
    }

    pub fn has_changes(&self) -> bool {
        if self.read_only() {
            return false;
        }
        if self.is_new() {
            return !(self.key.text.is_empty()
                && self.member.text.is_empty()
                && self.value.text.is_empty());
        }
        (self.key_name_editable() && self.key.changed())
            || (self.member_name_editable() && self.member.changed())
            || self.value.changed()
    }

    pub fn can_save(&self) -> bool {
        !self.loading && !self.saving && self.has_changes()
    }

    /// 值是不是在显示十六进制:十六进制只读,GUI 此时不接受输入。
    pub fn hex_text(&self) -> String {
        let orig = self.value.orig();
        hex_dump(&orig[..orig.len().min(HEX_DUMP_LIMIT)])
    }

    pub fn actions(&self) -> EditorActions {
        let formattable = self.format == Format::Json
            && !self.read_only()
            && !self.loading
            && !self.hex
            && !self.saving;
        EditorActions {
            save: self.can_save(),
            undo: !self.saving && self.has_changes(),
            format: formattable,
            minify: formattable,
            hex: !self.is_new() && !self.loading,
        }
    }

    /// 值输入框里该显示的文字:十六进制视图是原值的转储,否则是草稿。
    pub fn value_text(&self) -> Cow<'_, str> {
        if self.hex {
            Cow::Owned(self.hex_text())
        } else {
            Cow::Borrowed(&self.value.text)
        }
    }

    /// 只读的原因(已翻译):二进制,或大 String 只读了一部分。可编辑时是 `None`。
    pub fn notice(&self, lang: &str) -> Option<String> {
        if self.is_new() {
            return None;
        }
        let hex_cut = (self.hex && self.value.orig().len() > HEX_DUMP_LIMIT).then(|| {
            tr_with(
                lang,
                "view.hex_truncated",
                &[
                    ("shown", &human_bytes(HEX_DUMP_LIMIT as u64)),
                    ("total", &human_bytes(self.value.orig().len() as u64)),
                ],
            )
        });
        let readonly = self.readonly_notice(lang);
        match (readonly, hex_cut) {
            (Some(a), Some(b)) => Some(format!("{a} {b}")),
            (a, b) => a.or(b),
        }
    }

    fn readonly_notice(&self, lang: &str) -> Option<String> {
        if let Some(total) = self.truncated {
            return Some(tr_with(
                lang,
                "view.truncated_readonly",
                &[
                    ("size", &human_bytes(total)),
                    ("shown", &human_bytes(self.value.orig().len() as u64)),
                ],
            ));
        }
        (self.value.lossy() || self.member.lossy()).then(|| tr(lang, "view.binary_readonly"))
    }

    /// 选语言:只影响 Format、Minify 能不能用(v2.0 没有语法高亮),不算改动。
    pub fn set_format(&mut self, format: Format) {
        self.format = format;
    }

    pub fn set_key_name(&mut self, text: String) {
        if self.key_name_editable() {
            self.key.text = text;
        }
    }

    pub fn set_member_name(&mut self, text: String) {
        if self.member_name_editable() && !self.read_only() {
            self.member.text = text;
        }
    }

    pub fn set_value(&mut self, text: String) {
        // 十六进制视图是只读的转储,输入不能落到草稿里
        if !self.read_only() && !self.hex {
            self.value.text = text;
        }
    }

    /// 一串写操作中途失败,前面几步已经成功:把原值更新成已经生效的样子,
    /// 再次保存时不会重复做(比如 key 已经改名了,就只剩改值)。
    pub fn apply_partial(&mut self, done: &[WriteOp]) {
        for op in done {
            match op {
                WriteOp::RenameKey { new, .. } => {
                    if let Target::StringKey { key } = &mut self.target {
                        *key = new.clone();
                    }
                    self.key = Field::new(new.clone());
                }
                WriteOp::Member {
                    op: MemberOp::HashRename { new, .. } | MemberOp::ZRename { new, .. },
                    ..
                } => self.member = Field::new(new.clone()),
                _ => {}
            }
        }
    }

    pub fn undo(&mut self) {
        self.key.undo();
        self.member.undo();
        self.value.undo();
    }

    /// 美化。只在值可编辑时做;不是合法 JSON 返回原因。
    pub fn format_value(&mut self) -> Result<(), String> {
        if self.read_only() {
            return Ok(());
        }
        self.value.text = format_json(&self.value.text)?;
        Ok(())
    }

    pub fn minify_value(&mut self) -> Result<(), String> {
        if self.read_only() {
            return Ok(());
        }
        self.value.text = minify_json(&self.value.text)?;
        Ok(())
    }

    fn score(&self) -> Result<f64, FormError> {
        match self.value.text.trim().parse::<f64>() {
            Ok(s) if !s.is_nan() => Ok(s),
            _ => Err(FormError::ScoreInvalid),
        }
    }

    /// 算出保存要执行的写操作(有序),不合规矩返回 `FormError`。
    pub fn save_ops(&self) -> Result<Vec<WriteOp>, FormError> {
        if self.read_only() {
            return Err(FormError::ReadOnly);
        }
        let kind = self.kind();
        match &self.target {
            Target::StringKey { key } => {
                let mut ops = Vec::new();
                let mut current = key.clone();
                if self.key_name_editable() && self.key.changed() {
                    if self.key.text.is_empty() {
                        return Err(FormError::KeyNameEmpty);
                    }
                    let new = self.key.bytes();
                    ops.push(WriteOp::RenameKey {
                        old: key.clone(),
                        new: new.clone(),
                    });
                    current = new;
                }
                if self.value.changed() {
                    // 改值保留 TTL(v1 的普通 SET 会清掉 TTL)
                    ops.push(WriteOp::SetString {
                        key: current,
                        value: self.value.bytes(),
                        mode: SetMode::KeepTtl,
                    });
                }
                nonempty(ops)
            }
            Target::NewKey { .. } => {
                if self.key.text.is_empty() {
                    return Err(FormError::KeyNameEmpty);
                }
                let content = match kind {
                    KeyType::String => NewContent::String(self.value.bytes()),
                    KeyType::Hash => {
                        if self.member.text.is_empty() {
                            return Err(FormError::MemberNameEmpty);
                        }
                        NewContent::Hash {
                            field: self.member.bytes(),
                            value: self.value.bytes(),
                        }
                    }
                    KeyType::List => NewContent::List {
                        value: self.value.bytes(),
                        side: self.position,
                    },
                    KeyType::Set => NewContent::Set(self.value.bytes()),
                    KeyType::ZSet => {
                        if self.member.text.is_empty() {
                            return Err(FormError::MemberNameEmpty);
                        }
                        NewContent::ZSet {
                            member: self.member.bytes(),
                            score: self.score()?,
                        }
                    }
                    _ => return Err(FormError::NothingChanged),
                };
                Ok(vec![WriteOp::CreateKey {
                    key: self.key.bytes(),
                    content,
                }])
            }
            Target::NewMember { key, .. } => {
                let key = key.clone();
                let op = match kind {
                    KeyType::Hash => {
                        if self.member.text.is_empty() {
                            return Err(FormError::MemberNameEmpty);
                        }
                        MemberOp::HashAdd {
                            field: self.member.bytes(),
                            value: self.value.bytes(),
                        }
                    }
                    KeyType::List => MemberOp::ListPush {
                        value: self.value.bytes(),
                        side: self.position,
                    },
                    KeyType::Set => MemberOp::SetAdd(self.value.bytes()),
                    KeyType::ZSet => {
                        if self.member.text.is_empty() {
                            return Err(FormError::MemberNameEmpty);
                        }
                        MemberOp::ZAdd {
                            member: self.member.bytes(),
                            score: self.score()?,
                        }
                    }
                    _ => return Err(FormError::NothingChanged),
                };
                Ok(vec![WriteOp::Member { key, op }])
            }
            Target::Member { key, .. } => {
                let key = key.clone();
                let member_changed = self.member_name_editable() && self.member.changed();
                if member_changed && self.member.text.is_empty() {
                    return Err(FormError::MemberNameEmpty);
                }
                let mut ops = Vec::new();
                let mut push = |op| {
                    ops.push(WriteOp::Member {
                        key: key.clone(),
                        op,
                    })
                };
                match kind {
                    KeyType::Hash => {
                        let name = if member_changed {
                            push(MemberOp::HashRename {
                                old: self.member.orig().clone(),
                                new: self.member.bytes(),
                            });
                            self.member.bytes()
                        } else {
                            self.member.orig().clone()
                        };
                        if self.value.changed() {
                            push(MemberOp::HashSet {
                                field: name,
                                value: self.value.bytes(),
                            });
                        }
                    }
                    KeyType::List => {
                        if self.value.changed() {
                            let index = std::str::from_utf8(self.member.orig())
                                .ok()
                                .and_then(|s| s.parse::<i64>().ok())
                                .ok_or(FormError::NothingChanged)?;
                            push(MemberOp::ListSet {
                                index,
                                value: self.value.bytes(),
                            });
                        }
                    }
                    KeyType::Set => {
                        if self.value.changed() {
                            push(MemberOp::SetRename {
                                old: self.value.orig().clone(),
                                new: self.value.bytes(),
                            });
                        }
                    }
                    KeyType::ZSet => {
                        let name = if member_changed {
                            push(MemberOp::ZRename {
                                old: self.member.orig().clone(),
                                new: self.member.bytes(),
                            });
                            self.member.bytes()
                        } else {
                            self.member.orig().clone()
                        };
                        if self.value.changed() {
                            push(MemberOp::ZSetScore {
                                member: name,
                                score: self.score()?,
                            });
                        }
                    }
                    _ => {}
                }
                nonempty(ops)
            }
        }
    }
}

fn nonempty(ops: Vec<WriteOp>) -> Result<Vec<WriteOp>, FormError> {
    if ops.is_empty() {
        Err(FormError::NothingChanged)
    } else {
        Ok(ops)
    }
}
