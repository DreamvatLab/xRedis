//! `KeyView` 和它的部件(显示、编辑器草稿、成员表)的测试,不经过 App。
//! 移植 v1 `binaryReadOnly.test.ts`(二进制只读)和 `keyVMMembersCursor.test.ts`(重新打开从头扫),
//! 再加 R-07b 的新规则:KeepTtl、有序的多步保存和中途失败、TTL 校验、新建 key 不覆盖。
//! 测试数据用中文名,免得哪里偷偷按 ASCII 处理。

use bytes::Bytes;
use xredis_core::Error;
use xredis_core::config::{ConnId, Settings};
use xredis_core::error::Entity;
use xredis_core::keys::{KeyDetails, KeyType, Ttl};
use xredis_core::members::{ApplyFailure, Member, MemberOp, MemberPage, NewContent, WriteOp};
use xredis_core::types::list::Side;
use xredis_core::types::string::{SetMode, StringValue};

use super::display::{Format, detect_format, format_json, hex_dump, is_lossy, minify_json, show};
use super::editor::{Editor, FormError, Target};
use super::table::{MemberColumn, ScanState, TableMsg};
use super::*;
use crate::errors::human_bytes;
use crate::keylist::SortDir;

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

/// 不是合法 UTF-8 的字节。
fn binary() -> Bytes {
    Bytes::from_static(&[0xff, 0x01, b'j', b'o', b'b'])
}

// ---------------------------------------------------------------------------
// 显示
// ---------------------------------------------------------------------------

#[test]
fn only_undecodable_bytes_are_lossy() {
    for ok in ["", "plain text", "中文", "0", "42"] {
        assert!(!is_lossy(ok.as_bytes()), "{ok:?}");
        assert!(!show(ok.as_bytes()).lossy);
    }
    let s = show(&binary());
    assert!(s.lossy && s.text.contains('\u{fffd}'), "{s:?}");
    assert!(is_lossy(&[0xc3, 0x28]), "an invalid two-byte sequence");
}

#[test]
fn hex_dump_matches_the_hexdump_c_layout() {
    let data: Vec<u8> = (0u8..20).map(|i| i + 0x41).collect(); // A..T
    let dump = hex_dump(&data);
    let lines: Vec<&str> = dump.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        "00000000  41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  |ABCDEFGHIJKLMNOP|"
    );
    assert!(
        lines[1].starts_with("00000010  51 52 53 54 "),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with("|QRST|"), "{}", lines[1]);
    // 不可打印字节显示成点,换行也是
    assert!(hex_dump(&[0, b'\n', 0xff, b'a']).ends_with("|...a|\n"));
    assert_eq!(hex_dump(&[]), "");
}

#[test]
fn json_is_detected_only_for_objects_and_arrays() {
    assert_eq!(detect_format(r#"{"名字": 1}"#), Format::Json);
    assert_eq!(detect_format("  [1, 2, 3]"), Format::Json);
    for plain in ["123", r#""x""#, "true", "{broken", "hello", ""] {
        assert_eq!(detect_format(plain), Format::Plain, "{plain:?}");
    }
}

#[test]
fn json_formatting_only_touches_whitespace() {
    // 键的顺序、数字的写法、字符串里的转义和空白都原样保留
    let src = r#"{"b":1.0,"a":[],"大":{"x":12345678901234567890,"y":"a \" b  {not,a:struct}"},"z":[1,[2,{}]]}"#;
    let pretty = format_json(src).unwrap();
    assert_eq!(
        pretty,
        "{\n  \"b\": 1.0,\n  \"a\": [],\n  \"大\": {\n    \"x\": 12345678901234567890,\n    \"y\": \"a \\\" b  {not,a:struct}\"\n  },\n  \"z\": [\n    1,\n    [\n      2,\n      {}\n    ]\n  ]\n}"
    );
    assert_eq!(
        minify_json(&pretty).unwrap(),
        src,
        "minify undoes pretty exactly"
    );
    assert_eq!(minify_json(src).unwrap(), src);
    assert_eq!(format_json("[ ]").unwrap(), "[]");
    // 字符串里的反斜杠在末尾也不会把后面的引号吃掉
    let tricky = r#"{"p":"c:\\","q":1}"#;
    assert_eq!(minify_json(&format_json(tricky).unwrap()).unwrap(), tricky);
}

#[test]
fn invalid_json_is_reported_not_changed() {
    assert!(format_json("{oops").is_err());
    assert!(minify_json("not json").is_err());
}

// ---------------------------------------------------------------------------
// 编辑器草稿
// ---------------------------------------------------------------------------

fn string_editor(key: &str, value: &[u8]) -> Editor {
    let mut e = Editor::string_key(b(key));
    assert!(
        e.loading && !e.can_save(),
        "cannot save before the value is read"
    );
    e.loaded_string(Bytes::copy_from_slice(value), value.len() as u64, false);
    e
}

fn hash_row(f: &str, v: &str) -> Member {
    Member::Hash {
        field: b(f),
        value: b(v),
    }
}

fn member_editor(kind: KeyType, row: &Member) -> Editor {
    Editor::member(b("用户:1"), kind, row)
}

/// v1 `binaryReadOnly`:值是二进制:只读、改了也不算有改动、保存不写回。
#[test]
fn a_binary_value_is_read_only() {
    let mut e = string_editor("会话", &binary());
    assert!(e.read_only());
    e.set_value("edited".into());
    assert!(!e.has_changes() && !e.can_save());
    assert_eq!(e.value.text, show(&binary()).text, "the draft did not move");
    assert_eq!(e.save_ops().unwrap_err(), FormError::ReadOnly);
    // 十六进制视图看得到真实字节
    assert!(e.hex_text().contains("ff 01 6a 6f 62"), "{}", e.hex_text());
}

/// v1 `binaryReadOnly`:成员名是二进制:同样只读(按有损名字改名或写入会落到别的 field 上)。
#[test]
fn a_binary_member_name_is_read_only() {
    let row = Member::Hash {
        field: binary(),
        value: b("v"),
    };
    let mut e = member_editor(KeyType::Hash, &row);
    assert!(e.read_only());
    e.set_member_name("renamed".into());
    e.set_value("edited".into());
    assert!(!e.has_changes());
    assert_eq!(e.save_ops().unwrap_err(), FormError::ReadOnly);
}

#[test]
fn plain_text_is_editable() {
    let mut e = member_editor(KeyType::Hash, &hash_row("字段", "值"));
    assert!(!e.read_only() && !e.has_changes());
    e.set_value("新值".into());
    assert!(e.has_changes() && e.can_save());
}

#[test]
fn new_things_are_never_read_only() {
    for e in [
        Editor::new_key(KeyType::String),
        Editor::new_member(b("k"), KeyType::Hash),
    ] {
        assert!(!e.read_only());
    }
}

/// P-30:读了一部分的大 String 只读,并且记着真实长度。
#[test]
fn a_truncated_string_is_read_only() {
    let mut e = Editor::string_key(b("大"));
    e.loaded_string(Bytes::from(vec![b'x'; 1024]), 9 * 1024 * 1024, true);
    assert!(e.read_only());
    assert_eq!(e.truncated, Some(9 * 1024 * 1024));
    e.set_value("y".into());
    assert!(!e.has_changes());
    assert_eq!(e.save_ops().unwrap_err(), FormError::ReadOnly);
}

/// 第 16 节的备忘:编辑已有 String 的值用 KeepTtl,不能把 TTL 丢掉。
#[test]
fn editing_a_string_value_keeps_the_ttl() {
    let mut e = string_editor("计数", b"1");
    e.set_value("2".into());
    assert_eq!(
        e.save_ops().unwrap(),
        vec![WriteOp::SetString {
            key: b("计数"),
            value: b("2"),
            mode: SetMode::KeepTtl,
        }]
    );
}

/// P-21、P-27:改名和改值一起保存:先改名(RENAMENX),再往新名字上写。
#[test]
fn renaming_and_editing_a_string_runs_in_order() {
    let mut e = string_editor("旧名", b"1");
    e.set_key_name("新名".into());
    e.set_value("2".into());
    assert_eq!(
        e.save_ops().unwrap(),
        vec![
            WriteOp::RenameKey {
                old: b("旧名"),
                new: b("新名"),
            },
            WriteOp::SetString {
                key: b("新名"),
                value: b("2"),
                mode: SetMode::KeepTtl,
            },
        ]
    );
    // 只改名
    let mut e = string_editor("旧名", b"1");
    e.set_key_name("新名".into());
    assert_eq!(e.save_ops().unwrap().len(), 1);
}

#[test]
fn saving_validates_names_and_scores() {
    let mut e = string_editor("k", b"1");
    assert_eq!(e.save_ops().unwrap_err(), FormError::NothingChanged);
    e.set_key_name(String::new());
    assert_eq!(e.save_ops().unwrap_err(), FormError::KeyNameEmpty);

    let mut e = Editor::new_key(KeyType::String);
    e.set_value("v".into());
    assert_eq!(
        e.save_ops().unwrap_err(),
        FormError::KeyNameEmpty,
        "a new key needs a name"
    );

    let mut e = Editor::new_key(KeyType::Hash);
    e.set_key_name("h".into());
    assert_eq!(e.save_ops().unwrap_err(), FormError::MemberNameEmpty);

    for bad in ["abc", "", "nan", "NaN"] {
        let mut e = Editor::new_key(KeyType::ZSet);
        e.set_key_name("z".into());
        e.set_member_name("m".into());
        e.set_value(bad.into());
        assert_eq!(
            e.save_ops().unwrap_err(),
            FormError::ScoreInvalid,
            "{bad:?}"
        );
    }
    for good in ["1", "-1.5", " 7 ", "inf", "-inf", "1e3"] {
        let mut e = Editor::new_key(KeyType::ZSet);
        e.set_key_name("z".into());
        e.set_member_name("m".into());
        e.set_value(good.into());
        assert!(e.save_ops().is_ok(), "{good:?}");
    }
}

#[test]
fn a_binary_key_name_cannot_be_renamed_but_its_value_can_be_edited() {
    let mut e = Editor::string_key(binary());
    e.loaded_string(b("v"), 1, false);
    assert!(!e.key_name_editable());
    e.set_key_name("other".into());
    assert!(!e.key.changed(), "the rename is ignored");
    e.set_value("w".into());
    // 用原始字节写,不是有损文本
    assert_eq!(
        e.save_ops().unwrap(),
        vec![WriteOp::SetString {
            key: binary(),
            value: b("w"),
            mode: SetMode::KeepTtl,
        }]
    );
}

fn only_member_op(ops: Vec<WriteOp>) -> Vec<MemberOp> {
    ops.into_iter()
        .map(|o| match o {
            WriteOp::Member { op, .. } => op,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// P-23:改 field 名和值:先 HSET 之前先改名,值写到新名字上。
#[test]
fn hash_member_edits() {
    let mut e = member_editor(KeyType::Hash, &hash_row("旧", "1"));
    e.set_member_name("新".into());
    e.set_value("2".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![
            MemberOp::HashRename {
                old: b("旧"),
                new: b("新"),
            },
            MemberOp::HashSet {
                field: b("新"),
                value: b("2"),
            },
        ]
    );
    // 只改值
    let mut e = member_editor(KeyType::Hash, &hash_row("旧", "1"));
    e.set_value("2".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::HashSet {
            field: b("旧"),
            value: b("2"),
        }]
    );
}

/// P-24:List 的下标不能改,改值是 LSET;新增选头插还是尾插。
#[test]
fn list_member_edits() {
    let row = Member::List {
        index: 3,
        value: b("甲"),
    };
    let mut e = member_editor(KeyType::List, &row);
    assert!(!e.member_name_editable());
    e.set_member_name("9".into());
    assert!(!e.member.changed());
    e.set_value("乙".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::ListSet {
            index: 3,
            value: b("乙"),
        }]
    );

    let mut e = Editor::new_member(b("队列"), KeyType::List);
    assert!(e.inputs().position && !e.inputs().member_name);
    e.set_value("头".into());
    e.position = Side::Head;
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::ListPush {
            value: b("头"),
            side: Side::Head,
        }]
    );
}

/// P-25:Set 的成员就是值;改它是改名(SREM 加 SADD),新增已存在由后端报错。
#[test]
fn set_member_edits() {
    let mut e = member_editor(KeyType::Set, &Member::Set { member: b("甲") });
    assert!(!e.inputs().member_name && e.inputs().value);
    e.set_value("乙".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::SetRename {
            old: b("甲"),
            new: b("乙"),
        }]
    );
    let mut e = Editor::new_member(b("集合"), KeyType::Set);
    e.set_value("丙".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::SetAdd(b("丙"))]
    );
}

/// P-26:ZSet 分数和成员名都能改。
#[test]
fn zset_member_edits() {
    let row = Member::ZSet {
        member: b("玩家"),
        score: 12.5,
    };
    let mut e = member_editor(KeyType::ZSet, &row);
    assert_eq!(e.value.text, "12.5");
    e.set_member_name("选手".into());
    e.set_value("13".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![
            MemberOp::ZRename {
                old: b("玩家"),
                new: b("选手"),
            },
            MemberOp::ZSetScore {
                member: b("选手"),
                score: 13.0,
            },
        ]
    );
    let mut e = Editor::new_member(b("榜"), KeyType::ZSet);
    e.set_member_name("新人".into());
    e.set_value("1".into());
    assert_eq!(
        only_member_op(e.save_ops().unwrap()),
        vec![MemberOp::ZAdd {
            member: b("新人"),
            score: 1.0,
        }]
    );
}

/// P-20:新建 key,五种类型各自的第一个内容。
#[test]
fn new_key_content_per_type() {
    let mk = |kind: KeyType, f: &dyn Fn(&mut Editor)| {
        let mut e = Editor::new_key(kind);
        e.set_key_name("新键".into());
        f(&mut e);
        match e.save_ops().unwrap().remove(0) {
            WriteOp::CreateKey { key, content } => {
                assert_eq!(key, b("新键"));
                content
            }
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        mk(KeyType::String, &|e| e.set_value("值".into())),
        NewContent::String(b("值"))
    );
    assert_eq!(
        mk(KeyType::Hash, &|e| {
            e.set_member_name("f".into());
            e.set_value("v".into());
        }),
        NewContent::Hash {
            field: b("f"),
            value: b("v"),
        }
    );
    assert_eq!(
        mk(KeyType::List, &|e| e.set_value("x".into())),
        NewContent::List {
            value: b("x"),
            side: Side::Tail,
        }
    );
    assert_eq!(
        mk(KeyType::Set, &|e| e.set_value("m".into())),
        NewContent::Set(b("m"))
    );
    assert_eq!(
        mk(KeyType::ZSet, &|e| {
            e.set_member_name("m".into());
            e.set_value("2".into());
        }),
        NewContent::ZSet {
            member: b("m"),
            score: 2.0,
        }
    );
}

#[test]
fn undo_restores_every_field() {
    let mut e = string_editor("名", b"1");
    e.set_key_name("别名".into());
    e.set_value("2".into());
    assert!(e.has_changes());
    e.undo();
    assert!(!e.has_changes());
    assert_eq!((e.key.text.as_str(), e.value.text.as_str()), ("名", "1"));
}

#[test]
fn format_and_minify_only_edit_editable_values() {
    let mut e = string_editor("k", br#"{"a":1}"#);
    assert_eq!(e.format, Format::Json);
    e.format_value().unwrap();
    assert!(e.value.text.contains('\n') && e.has_changes());
    e.minify_value().unwrap();
    assert_eq!(e.value.text, r#"{"a":1}"#);
    assert!(!e.has_changes(), "back to the original text");

    e.set_value("not json".into());
    assert!(e.format_value().is_err());
    assert_eq!(e.value.text, "not json", "left alone on error");

    let mut e = string_editor("k", &binary());
    let before = e.value.text.clone();
    e.format_value().unwrap();
    assert_eq!(e.value.text, before);
}

/// 中途失败后(key 已经改名),编辑器的原值跟上,再保存就不会重复改名。
#[test]
fn partial_success_moves_the_originals() {
    let mut e = string_editor("旧", b"1");
    e.set_key_name("新".into());
    e.set_value("2".into());
    let ops = e.save_ops().unwrap();
    e.apply_partial(&ops[..1]);
    assert_eq!(
        e.target,
        Target::StringKey { key: b("新") },
        "the editor now edits the renamed key"
    );
    assert_eq!(
        e.save_ops().unwrap(),
        vec![WriteOp::SetString {
            key: b("新"),
            value: b("2"),
            mode: SetMode::KeepTtl,
        }]
    );

    let mut e = member_editor(KeyType::Hash, &hash_row("旧", "1"));
    e.set_member_name("新".into());
    e.set_value("2".into());
    let ops = e.save_ops().unwrap();
    e.apply_partial(&ops[..1]);
    assert_eq!(
        only_member_op(e.save_ops().unwrap()).len(),
        1,
        "only the value is left"
    );
}

// ---------------------------------------------------------------------------
// KeyView
// ---------------------------------------------------------------------------

struct H {
    view: KeyView,
    settings: Settings,
    now: i64,
    id: ConnId,
}

impl H {
    fn open(key: &str, kind: KeyType) -> (Self, ViewOut) {
        let settings = Settings {
            scan_batch: 2,
            auto_scan_limit: 4,
            ..Settings::default()
        };
        let id = ConnId::new();
        let ctx = Ctx {
            id,
            lang: "en",
            settings: &settings,
            now: 1_000,
        };
        let (view, out) = KeyView::open(&ctx, 7, b(key), kind).expect("openable");
        (
            Self {
                view,
                settings,
                now: 1_000,
                id,
            },
            out,
        )
    }

    fn send(&mut self, msg: ViewMsg) -> ViewOut {
        self.now += 1_000;
        let ctx = Ctx {
            id: self.id,
            lang: "en",
            settings: &self.settings,
            now: self.now,
        };
        self.view.update(&ctx, msg)
    }

    fn generation(&self) -> u64 {
        self.view.generation()
    }

    fn details(&mut self, kind: KeyType, length: u64, ttl: Ttl) -> ViewOut {
        let generation = self.generation();
        self.send(ViewMsg::Details {
            generation,
            result: Ok(KeyDetails {
                kind,
                length: Some(length),
                ttl,
            }),
        })
    }

    fn string_read(&mut self, v: &[u8]) {
        let generation = self.generation();
        self.send(ViewMsg::StringRead {
            generation,
            result: Ok(Some(StringValue {
                data: Bytes::copy_from_slice(v),
                total_len: v.len() as u64,
                truncated: false,
            })),
        });
    }

    fn batch(&mut self, scan_generation: u64, items: Vec<Member>, cursor: u64) -> ViewOut {
        let generation = self.generation();
        self.send(ViewMsg::Members {
            generation,
            scan_generation,
            result: Ok(MemberPage { items, cursor }),
        })
    }

    fn applied(&mut self, token: u64, result: Result<(), ApplyFailure>) -> ViewOut {
        let generation = self.generation();
        self.send(ViewMsg::Applied {
            generation,
            token,
            result,
        })
    }
}

fn backend(out: &ViewOut) -> Vec<&BackendCmd> {
    out.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Backend(b) => Some(b),
            _ => None,
        })
        .collect()
}

fn applies(out: &ViewOut) -> Vec<(u64, Vec<WriteOp>)> {
    backend(out)
        .into_iter()
        .filter_map(|c| match c {
            BackendCmd::ViewApply { token, ops, .. } => Some((*token, ops.clone())),
            _ => None,
        })
        .collect()
}

fn scans(out: &ViewOut) -> Vec<(u64, u64)> {
    backend(out)
        .into_iter()
        .filter_map(|c| match c {
            BackendCmd::ViewScan {
                scan_generation,
                cursor,
                ..
            } => Some((*scan_generation, *cursor)),
            _ => None,
        })
        .collect()
}

fn last_note(out: &ViewOut) -> &Note {
    out.notes.last().expect("a note")
}

fn hash_rows(n: usize) -> Vec<Member> {
    (0..n)
        .map(|i| hash_row(&format!("字段{i}"), &format!("值{i}")))
        .collect()
}

#[test]
fn opening_a_string_reads_details_and_the_value() {
    let (mut h, out) = H::open("文章", KeyType::String);
    assert!(matches!(
        backend(&out)[..],
        [
            BackendCmd::ViewDetails { generation: 7, .. },
            BackendCmd::ViewReadString { generation: 7, .. }
        ]
    ));
    let ed = h.view.editor.as_ref().unwrap();
    assert!(ed.loading && !ed.can_save());
    // 值还在读时点保存什么也不发
    assert!(h.send(ViewMsg::Save).cmds.is_empty());
    h.string_read("内容".as_bytes());
    let ed = h.view.editor.as_ref().unwrap();
    assert!(!ed.loading && ed.value.text == "内容");
}

#[test]
fn opening_something_that_cannot_be_opened_is_an_error() {
    let settings = Settings::default();
    let ctx = Ctx {
        id: ConnId::new(),
        lang: "en",
        settings: &settings,
        now: 0,
    };
    for kind in [
        KeyType::Stream,
        KeyType::None,
        KeyType::Other("ReJSON-RL".into()),
    ] {
        assert!(matches!(
            KeyView::open(&ctx, 1, b("k"), kind),
            Err(Error::NotOpenable { .. })
        ));
    }
}

/// 一批一条命令:自动续扫到上限就停,游标保留,More 再来一批,All 扫到底。
#[test]
fn the_member_table_loads_in_three_tiers() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, cursor) = scans(&out)[0];
    assert_eq!(cursor, 0);
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(t.scan_state(), ScanState::Searching);

    // 自动续扫上限 4,每批 2 个:第二批之后停
    let out = h.batch(sg, hash_rows(2), 2);
    assert_eq!(scans(&out), vec![(sg, 2)]);
    let out = h.batch(sg, hash_rows(4)[2..].to_vec(), 4);
    assert!(scans(&out).is_empty());
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(
        (t.loaded_count(), t.has_more(), t.scan_state()),
        (4, true, ScanState::Idle)
    );

    // More:一批
    let out = h.send(ViewMsg::Table(TableMsg::LoadMore));
    assert_eq!(scans(&out), vec![(sg, 4)]);
    let out = h.batch(sg, hash_rows(6)[4..].to_vec(), 6);
    assert!(scans(&out).is_empty());

    // All:一直到游标归零
    let out = h.send(ViewMsg::Table(TableMsg::LoadAll));
    assert_eq!(scans(&out), vec![(sg, 6)]);
    let out = h.batch(sg, hash_rows(8)[6..].to_vec(), 8);
    assert_eq!(scans(&out), vec![(sg, 8)]);
    let out = h.batch(sg, hash_rows(9)[8..].to_vec(), 0);
    assert!(scans(&out).is_empty());
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(
        (t.loaded_count(), t.has_more(), t.scan_state()),
        (9, false, ScanState::Idle)
    );
}

#[test]
fn stop_ends_the_scan_after_the_batch_in_flight_and_keeps_the_cursor() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.send(ViewMsg::Table(TableMsg::Stop));
    assert_eq!(
        h.view.table.as_ref().unwrap().scan_state(),
        ScanState::Stopping
    );
    let out = h.batch(sg, hash_rows(2), 2);
    assert!(scans(&out).is_empty(), "no next batch after Stop");
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(
        (t.scan_state(), t.has_more(), t.loaded_count()),
        (ScanState::Idle, true, 2)
    );
    // 之后还能继续
    assert_eq!(
        scans(&h.send(ViewMsg::Table(TableMsg::LoadMore))),
        vec![(sg, 2)]
    );
}

/// v1 `keyVMMembersCursor`:重新搜索(重新打开)必须从游标 0 开始,不能接着上次的游标,
/// 也不能残留上一次的成员;旧搜索迟到的批次作废。
#[test]
fn searching_again_starts_from_cursor_zero_and_drops_the_old_batches() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg1, _) = scans(&out)[0];
    h.batch(sg1, hash_rows(2), 512);
    h.send(ViewMsg::Table(TableMsg::Stop));
    h.batch(sg1, vec![], 512);
    assert!(h.view.table.as_ref().unwrap().has_more());

    let out = h.send(ViewMsg::Table(TableMsg::Search));
    let (sg2, cursor) = scans(&out)[0];
    assert_eq!(cursor, 0, "from the start, not from 512");
    assert_ne!(sg1, sg2);
    assert_eq!(
        h.view.table.as_ref().unwrap().loaded_count(),
        0,
        "nothing left over"
    );

    // 旧搜索的迟到批次丢弃
    let out = h.batch(sg1, hash_rows(5), 0);
    assert!(out.cmds.is_empty());
    assert_eq!(h.view.table.as_ref().unwrap().loaded_count(), 0);

    h.batch(sg2, vec![hash_row("c", "3")], 0);
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(t.loaded_count(), 1);
    assert_eq!(t.view().next().unwrap().name(), b("c"));
}

#[test]
fn results_for_another_view_are_dropped() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    let out = h.send(ViewMsg::Members {
        generation: 999,
        scan_generation: sg,
        result: Ok(MemberPage {
            items: hash_rows(2),
            cursor: 0,
        }),
    });
    assert!(out.cmds.is_empty());
    assert_eq!(h.view.table.as_ref().unwrap().loaded_count(), 0);
    let out = h.send(ViewMsg::Details {
        generation: 999,
        result: Err(Error::Closed),
    });
    assert!(
        out.notes.is_empty(),
        "a late error from another view is not reported"
    );
}

#[test]
fn a_scan_error_stops_the_scan_and_is_reported() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(2), 2);
    let generation = h.generation();
    let out = h.send(ViewMsg::Members {
        generation,
        scan_generation: sg,
        result: Err(Error::Timeout("HSCAN".into())),
    });
    assert_eq!(last_note(&out).level, Level::Error);
    let t = h.view.table.as_ref().unwrap();
    assert_eq!(
        (t.scan_state(), t.loaded_count()),
        (ScanState::Idle, 2),
        "what was loaded stays"
    );
}

/// 成员去重(HSCAN、SSCAN、ZSCAN 可能重复返回),过滤不分大小写,排序,默认排序。
#[test]
fn the_member_table_dedupes_filters_and_sorts() {
    let (mut h, out) = H::open("榜", KeyType::ZSet);
    let (sg, _) = scans(&out)[0];
    let z = |m: &str, s: f64| Member::ZSet {
        member: b(m),
        score: s,
    };
    h.batch(sg, vec![z("甲", 1.0), z("Bob", 30.0), z("乙", 20.0)], 5);
    h.batch(sg, vec![z("Bob", 30.0), z("alice", 5.0)], 0);
    let names = |h: &H| -> Vec<String> {
        h.view
            .table
            .as_ref()
            .unwrap()
            .view()
            .map(|m| show(&m.name()).text)
            .collect()
    };
    assert_eq!(
        names(&h),
        vec!["Bob", "乙", "alice", "甲"],
        "four rows, ZSet defaults to score descending"
    );

    h.send(ViewMsg::Table(TableMsg::SortBy(MemberColumn::Value)));
    assert_eq!(
        names(&h),
        vec!["甲", "alice", "乙", "Bob"],
        "second click flips it"
    );
    h.send(ViewMsg::Table(TableMsg::SortBy(MemberColumn::Name)));
    assert_eq!(names(&h)[0], "Bob");
    assert_eq!(h.view.table.as_ref().unwrap().sort.dir, SortDir::Asc);

    h.send(ViewMsg::Table(TableMsg::SetFilter("ALI".into())));
    assert_eq!(names(&h), vec!["alice"]);
    h.send(ViewMsg::Table(TableMsg::SetFilter("20".into())));
    assert_eq!(names(&h), vec!["乙"], "the score text is filtered too");
}

#[test]
fn list_rows_sort_by_numeric_index() {
    let (mut h, out) = H::open("队列", KeyType::List);
    let (sg, _) = scans(&out)[0];
    let rows: Vec<Member> = (0..12)
        .map(|i| Member::List {
            index: i,
            value: b(&format!("v{i}")),
        })
        .collect();
    h.batch(sg, rows, 0);
    let t = h.view.table.as_ref().unwrap();
    let idx: Vec<u64> = t
        .view()
        .map(|m| match m {
            Member::List { index, .. } => *index,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(idx, (0..12).collect::<Vec<_>>(), "2 sorts before 10");
}

#[test]
fn load_progress_uses_the_key_length() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    assert_eq!(
        h.view.table.as_ref().unwrap().loaded_percent(),
        0.0,
        "unknown length is 0, not NaN"
    );
    h.details(KeyType::Hash, 8, Ttl::Persistent);
    h.batch(sg, hash_rows(2), 2);
    assert_eq!(h.view.table.as_ref().unwrap().loaded_percent(), 25.0);
}

#[test]
fn selecting_all_selects_the_filtered_rows_and_delete_asks_first() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(3), 0);
    h.send(ViewMsg::Table(TableMsg::SetFilter("字段1".into())));
    h.send(ViewMsg::Table(TableMsg::SelectAllFiltered(true)));
    assert_eq!(h.view.table.as_ref().unwrap().selected_count(), 1);
    // 不存在的成员选不上
    h.send(ViewMsg::Table(TableMsg::Select {
        name: b("没有"),
        on: true,
    }));
    assert_eq!(h.view.table.as_ref().unwrap().selected_count(), 1);

    let out = h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    assert!(out.cmds.is_empty(), "a confirmation first");
    assert_eq!(h.view.table.as_ref().unwrap().confirm_delete, Some(1));
    let out = h.send(ViewMsg::Table(TableMsg::CancelDelete));
    assert!(out.cmds.is_empty());
    assert_eq!(h.view.table.as_ref().unwrap().confirm_delete, None);

    h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    let out = h.send(ViewMsg::Table(TableMsg::ConfirmDelete));
    let a = applies(&out);
    assert_eq!(a.len(), 1);
    assert_eq!(
        a[0].1,
        vec![WriteOp::Member {
            key: b("用户"),
            op: MemberOp::HashDelete(vec![b("字段1")]),
        }]
    );
}

#[test]
fn deleting_list_members_sends_their_indexes() {
    let (mut h, out) = H::open("队列", KeyType::List);
    let (sg, _) = scans(&out)[0];
    let rows: Vec<Member> = (0..3)
        .map(|i| Member::List {
            index: i,
            value: b("x"),
        })
        .collect();
    h.batch(sg, rows, 0);
    h.send(ViewMsg::Table(TableMsg::Select {
        name: b("2"),
        on: true,
    }));
    h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    let out = h.send(ViewMsg::Table(TableMsg::ConfirmDelete));
    assert_eq!(
        applies(&out)[0].1,
        vec![WriteOp::Member {
            key: b("队列"),
            op: MemberOp::ListRemove(vec![2]),
        }]
    );
}

#[test]
fn a_successful_delete_reloads_the_table_and_the_row_in_the_key_list() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(3), 0);
    h.send(ViewMsg::Table(TableMsg::SelectAllFiltered(true)));
    h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    let out = h.send(ViewMsg::Table(TableMsg::ConfirmDelete));
    let token = applies(&out)[0].0;
    let out = h.applied(token, Ok(()));
    assert!(
        last_note(&out)
            .text
            .contains("Deleted 3 member(s) of '用户'"),
        "{}",
        last_note(&out).text
    );
    assert!(
        backend(&out)
            .iter()
            .any(|c| matches!(c, BackendCmd::ViewDetails { .. })),
        "length is read again"
    );
    let (sg2, cursor) = scans(&out)[0];
    assert_eq!(cursor, 0);
    assert_ne!(sg2, sg, "reloaded from the start");
    assert_eq!(out.list, vec![ListEffect::RefreshDetails(b("用户"))]);
    assert_eq!(h.view.table.as_ref().unwrap().selected_count(), 0);
}

#[test]
fn a_failed_delete_keeps_the_rows_and_the_selection() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(3), 0);
    h.send(ViewMsg::Table(TableMsg::SelectAllFiltered(true)));
    h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    let token = applies(&h.send(ViewMsg::Table(TableMsg::ConfirmDelete)))[0].0;
    let out = h.applied(
        token,
        Err(ApplyFailure {
            done: 0,
            error: Error::Timeout("HDEL".into()),
        }),
    );
    assert_eq!(last_note(&out).level, Level::Error);
    assert!(out.cmds.is_empty());
    let t = h.view.table.as_ref().unwrap();
    assert_eq!((t.loaded_count(), t.selected_count()), (3, 3));
    assert!(!h.view.is_writing());
}

// ---- 编辑器的保存 ----

fn open_string_for_edit(value: &str) -> H {
    let (mut h, _) = H::open("文章", KeyType::String);
    h.string_read(value.as_bytes());
    h
}

#[test]
fn saving_a_string_sends_one_apply_and_blocks_a_second_save() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetValue("新".into()));
    let out = h.send(ViewMsg::Save);
    let a = applies(&out);
    assert_eq!(a.len(), 1);
    assert_eq!(
        a[0].1,
        vec![WriteOp::SetString {
            key: b("文章"),
            value: b("新"),
            mode: SetMode::KeepTtl,
        }]
    );
    assert!(h.view.editor.as_ref().unwrap().saving);
    assert!(h.send(ViewMsg::Save).cmds.is_empty(), "no double save");
    // 在途时也不能开改名对话框
    h.send(ViewMsg::OpenRename);
    assert!(h.view.rename.is_none());
}

#[test]
fn a_string_save_closes_the_view_and_refreshes_the_row() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetValue("新".into()));
    let token = applies(&h.send(ViewMsg::Save))[0].0;
    let out = h.applied(token, Ok(()));
    assert!(out.close);
    assert_eq!(out.list, vec![ListEffect::RefreshDetails(b("文章"))]);
    assert_eq!(last_note(&out).text, "Saved.");
}

#[test]
fn renaming_while_saving_a_string_searches_the_list_again() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetKeyName("新文章".into()));
    let token = applies(&h.send(ViewMsg::Save))[0].0;
    let out = h.applied(token, Ok(()));
    assert!(out.close);
    assert_eq!(out.list, vec![ListEffect::Search]);
    assert!(last_note(&out).text.contains("renamed to '新文章'"));
}

/// 改名成功、写值失败:key 已经是新名字了,编辑器停在原地让用户重试,再保存只剩写值。
#[test]
fn a_failure_after_the_rename_leaves_the_editor_ready_to_retry() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetKeyName("新文章".into()));
    h.send(ViewMsg::SetValue("新".into()));
    let (token, ops) = applies(&h.send(ViewMsg::Save)).remove(0);
    assert_eq!(ops.len(), 2);
    let out = h.applied(
        token,
        Err(ApplyFailure {
            done: 1,
            error: Error::Timeout("SET".into()),
        }),
    );
    assert!(!out.close, "the editor stays");
    assert_eq!(
        out.list,
        vec![ListEffect::Search],
        "the list shows the new name"
    );
    assert_eq!(h.view.key, b("新文章"));
    assert!(!h.view.editor.as_ref().unwrap().saving);
    let retry = applies(&h.send(ViewMsg::Save));
    assert_eq!(
        retry[0].1,
        vec![WriteOp::SetString {
            key: b("新文章"),
            value: b("新"),
            mode: SetMode::KeepTtl,
        }],
        "not renamed a second time"
    );
}

#[test]
fn a_rejected_save_keeps_the_editor_open_with_the_reason() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetKeyName("已有".into()));
    let token = applies(&h.send(ViewMsg::Save))[0].0;
    let out = h.applied(
        token,
        Err(ApplyFailure {
            done: 0,
            error: Error::AlreadyExists {
                entity: Entity::Key,
                name: b("已有"),
                container: None,
            },
        }),
    );
    assert!(!out.close && out.list.is_empty());
    assert!(
        last_note(&out).text.contains("已有"),
        "{}",
        last_note(&out).text
    );
    assert!(
        h.view.editor.as_ref().unwrap().can_save(),
        "can fix the name and retry"
    );
}

#[test]
fn an_apply_result_with_the_wrong_token_is_ignored() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetValue("新".into()));
    let token = applies(&h.send(ViewMsg::Save))[0].0;
    let out = h.applied(token + 5, Ok(()));
    assert!(!out.close && out.notes.is_empty());
    assert!(h.view.is_writing(), "still waiting for the real result");
}

#[test]
fn form_errors_are_reported_without_sending_anything() {
    let mut h = open_string_for_edit("旧");
    h.send(ViewMsg::SetKeyName(String::new()));
    let out = h.send(ViewMsg::Save);
    assert!(out.cmds.is_empty());
    assert_eq!(last_note(&out).text, "The key name cannot be empty.");
    assert!(!h.view.is_writing());
}

#[test]
fn closing_a_string_editor_closes_the_view_but_a_member_editor_only_its_dialog() {
    let mut h = open_string_for_edit("旧");
    assert!(h.send(ViewMsg::CloseEditor).close);

    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(2), 0);
    h.send(ViewMsg::OpenMember(b("字段0")));
    assert!(h.view.editor.is_some());
    let out = h.send(ViewMsg::CloseEditor);
    assert!(!out.close && h.view.editor.is_none() && h.view.table.is_some());
}

#[test]
fn a_member_save_reloads_the_table_and_closes_only_the_editor() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(2), 0);
    h.send(ViewMsg::OpenMember(b("字段0")));
    h.send(ViewMsg::SetValue("改过".into()));
    let token = applies(&h.send(ViewMsg::Save))[0].0;
    let out = h.applied(token, Ok(()));
    assert!(!out.close);
    assert!(h.view.editor.is_none());
    assert_eq!(
        scans(&out)[0].1,
        0,
        "the table is read again from the start"
    );
    assert_eq!(out.list, vec![ListEffect::RefreshDetails(b("用户"))]);
}

#[test]
fn new_member_opens_an_empty_editor_and_creates_with_add() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(1), 0);
    h.send(ViewMsg::NewMember);
    assert!(h.view.editor.as_ref().unwrap().is_new());
    h.send(ViewMsg::SetMemberName("新字段".into()));
    h.send(ViewMsg::SetValue("新值".into()));
    let ops = applies(&h.send(ViewMsg::Save)).remove(0).1;
    assert_eq!(
        ops,
        vec![WriteOp::Member {
            key: b("用户"),
            op: MemberOp::HashAdd {
                field: b("新字段"),
                value: b("新值"),
            },
        }]
    );
}

// ---- 新建 key ----

#[test]
fn creating_a_key_never_overwrites_and_refreshes_the_list_when_done() {
    let mut v = KeyView::new_key(3, KeyType::String).unwrap();
    assert!(v.is_creating() && v.key.is_empty());
    let settings = Settings::default();
    let id = ConnId::new();
    let ctx = Ctx {
        id,
        lang: "en",
        settings: &settings,
        now: 0,
    };
    v.update(&ctx, ViewMsg::SetKeyName("新键".into()));
    v.update(&ctx, ViewMsg::SetValue("v".into()));
    let out = v.update(&ctx, ViewMsg::Save);
    let (token, ops) = applies(&out).remove(0);
    assert!(matches!(
        &ops[..],
        [WriteOp::CreateKey {
            content: NewContent::String(_),
            ..
        }]
    ));

    // 已存在:报错,编辑器还在
    let out = v.update(
        &ctx,
        ViewMsg::Applied {
            generation: 3,
            token,
            result: Err(ApplyFailure {
                done: 0,
                error: Error::AlreadyExists {
                    entity: Entity::Key,
                    name: b("新键"),
                    container: None,
                },
            }),
        },
    );
    assert!(!out.close && last_note(&out).text.contains("新键"));

    let (token, _) = applies(&v.update(&ctx, ViewMsg::Save)).remove(0);
    let out = v.update(
        &ctx,
        ViewMsg::Applied {
            generation: 3,
            token,
            result: Ok(()),
        },
    );
    assert!(out.close);
    assert_eq!(out.list, vec![ListEffect::Search]);
}

#[test]
fn only_the_five_basic_types_can_be_created() {
    for kind in [KeyType::Stream, KeyType::None, KeyType::Other("x".into())] {
        assert!(KeyView::new_key(1, kind).is_none());
    }
    for kind in [
        KeyType::String,
        KeyType::Hash,
        KeyType::List,
        KeyType::Set,
        KeyType::ZSet,
    ] {
        assert!(KeyView::new_key(1, kind).is_some());
    }
}

// ---- 改名和 TTL 对话框 ----

#[test]
fn the_rename_dialog_validates_and_sends_renamenx() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(1), 0);
    h.send(ViewMsg::OpenRename);
    assert_eq!(h.view.rename.as_ref().unwrap().text, "用户");

    h.send(ViewMsg::SetRenameText(String::new()));
    let out = h.send(ViewMsg::ConfirmRename);
    assert!(out.cmds.is_empty());
    assert_eq!(
        h.view.rename.as_ref().unwrap().error.as_deref(),
        Some("The key name cannot be empty.")
    );

    // 没改:当作取消
    h.send(ViewMsg::SetRenameText("用户".into()));
    let out = h.send(ViewMsg::ConfirmRename);
    assert!(out.cmds.is_empty() && h.view.rename.is_none());

    h.send(ViewMsg::OpenRename);
    h.send(ViewMsg::SetRenameText("新用户".into()));
    let out = h.send(ViewMsg::ConfirmRename);
    assert_eq!(
        applies(&out)[0].1,
        vec![WriteOp::RenameKey {
            old: b("用户"),
            new: b("新用户"),
        }]
    );
}

#[test]
fn a_rejected_rename_keeps_the_dialog_open_with_the_reason_and_success_closes_the_view() {
    let (mut h, _) = H::open("用户", KeyType::Hash);
    h.send(ViewMsg::OpenRename);
    h.send(ViewMsg::SetRenameText("已有".into()));
    let token = applies(&h.send(ViewMsg::ConfirmRename))[0].0;
    let out = h.applied(
        token,
        Err(ApplyFailure {
            done: 0,
            error: Error::AlreadyExists {
                entity: Entity::Key,
                name: b("已有"),
                container: None,
            },
        }),
    );
    assert!(!out.close);
    let err = h.view.rename.as_ref().unwrap().error.clone().unwrap();
    assert!(err.contains("已有"), "{err}");
    assert!(!h.view.is_writing());

    let token = applies(&h.send(ViewMsg::ConfirmRename))[0].0;
    let out = h.applied(token, Ok(()));
    assert!(out.close && h.view.rename.is_none());
    assert_eq!(out.list, vec![ListEffect::Search]);
}

/// L-08、P-28:TTL 0 拒绝(EXPIRE 0 会删除 key);-1 是 PERSIST;范围 1 到 2147483647。
#[test]
fn the_ttl_dialog_applies_the_rules_before_sending_anything() {
    let (mut h, _) = H::open("会话", KeyType::Hash);
    h.details(KeyType::Hash, 1, Ttl::Seconds(90));
    h.send(ViewMsg::OpenTtl);
    assert_eq!(
        h.view.ttl.as_ref().unwrap().text,
        "90",
        "starts from the current TTL"
    );

    for (input, needle) in [
        ("0", "would delete the key"),
        ("-2", "is not allowed"),
        ("2147483648", "is not allowed"),
        ("abc", "whole number of seconds"),
        ("", "whole number of seconds"),
        ("1.5", "whole number of seconds"),
    ] {
        h.send(ViewMsg::SetTtlText(input.into()));
        let out = h.send(ViewMsg::ConfirmTtl);
        assert!(out.cmds.is_empty(), "{input:?} must not be sent");
        let err = h
            .view
            .ttl
            .as_ref()
            .unwrap()
            .error
            .clone()
            .unwrap_or_default();
        assert!(err.contains(needle), "{input:?}: {err}");
    }

    for (input, seconds) in [
        ("-1", -1),
        ("1", 1),
        (" 3600 ", 3600),
        ("2147483647", 2_147_483_647),
    ] {
        h.send(ViewMsg::SetTtlText(input.into()));
        let out = h.send(ViewMsg::ConfirmTtl);
        let (token, ops) = applies(&out).remove(0);
        assert_eq!(
            ops,
            vec![WriteOp::SetTtl {
                key: b("会话"),
                seconds,
            }]
        );
        // 让它失败,回到可再次提交的状态
        h.applied(
            token,
            Err(ApplyFailure {
                done: 0,
                error: Error::Timeout("EXPIRE".into()),
            }),
        );
    }
}

#[test]
fn a_ttl_change_closes_the_dialog_and_refreshes_the_details() {
    let (mut h, _) = H::open("会话", KeyType::Hash);
    h.send(ViewMsg::OpenTtl);
    assert_eq!(
        h.view.ttl.as_ref().unwrap().text,
        "-1",
        "unknown or no TTL starts at -1"
    );
    h.send(ViewMsg::SetTtlText("60".into()));
    let token = applies(&h.send(ViewMsg::ConfirmTtl))[0].0;
    let out = h.applied(token, Ok(()));
    assert!(h.view.ttl.is_none() && !out.close);
    assert!(
        backend(&out)
            .iter()
            .any(|c| matches!(c, BackendCmd::ViewDetails { .. }))
    );
    assert_eq!(out.list, vec![ListEffect::RefreshDetails(b("会话"))]);
}

// ---- key 被别人删了或换了类型 ----

#[test]
fn a_key_that_vanished_or_changed_type_closes_the_view() {
    let (mut h, _) = H::open("用户", KeyType::Hash);
    let out = h.details(KeyType::None, 0, Ttl::Missing);
    assert!(out.close);
    assert!(
        last_note(&out).text.contains("no longer exists"),
        "{}",
        last_note(&out).text
    );

    let (mut h, _) = H::open("用户", KeyType::Hash);
    assert!(
        h.details(KeyType::List, 3, Ttl::Persistent).close,
        "type changed"
    );

    let (mut h, _) = H::open("文章", KeyType::String);
    let generation = h.generation();
    let out = h.send(ViewMsg::StringRead {
        generation,
        result: Ok(None),
    });
    assert!(out.close);
}

#[test]
fn a_failed_string_read_closes_the_view_with_the_error() {
    let (mut h, _) = H::open("文章", KeyType::String);
    let generation = h.generation();
    let out = h.send(ViewMsg::StringRead {
        generation,
        result: Err(Error::Timeout("GET".into())),
    });
    assert!(out.close);
    assert_eq!(last_note(&out).level, Level::Error);
}

#[test]
fn details_fill_in_length_and_ttl() {
    let (mut h, _) = H::open("用户", KeyType::Hash);
    assert!(h.view.details.is_none(), "not loaded is not 0 or -1");
    h.details(KeyType::Hash, 42, Ttl::Seconds(5));
    let d = h.view.details.clone().unwrap();
    assert_eq!((d.length, d.ttl), (Some(42), Ttl::Seconds(5)));
    assert_eq!(h.view.table.as_ref().unwrap().length, Some(42));
}

#[test]
fn column_titles_follow_the_key_type() {
    let titles = |kind| H::open("k", kind).0.view.column_titles();
    assert_eq!(
        titles(KeyType::Hash),
        Some(("view.col.field", "view.col.value"))
    );
    assert_eq!(
        titles(KeyType::List),
        Some(("view.col.index", "view.col.value"))
    );
    assert_eq!(
        titles(KeyType::ZSet),
        Some(("view.col.member", "view.col.score"))
    );
    assert_eq!(titles(KeyType::String), None);
}

// ---- R-10a:给界面的查询(哪些按钮可用、提示文字、语言) ----

#[test]
fn the_editor_says_which_buttons_are_usable() {
    // 值还在读:什么都不能用
    let mut e = Editor::string_key(b("k"));
    assert!(e.is_key_level());
    assert!(!member_editor(KeyType::Hash, &hash_row("f", "v")).is_key_level());
    assert!(!Editor::new_member(b("k"), KeyType::Hash).is_key_level());
    assert!(Editor::new_key(KeyType::Hash).is_key_level());
    let a = e.actions();
    assert!(
        !a.save && !a.undo && !a.format && !a.minify && !a.hex,
        "{a:?}"
    );

    // 读完、没改、纯文本:只能切十六进制
    e.loaded_string(Bytes::from_static(b"plain"), 5, false);
    let a = e.actions();
    assert!(
        !a.save && !a.undo && !a.format && !a.minify && a.hex,
        "{a:?}"
    );

    // 改了:Save 和 Undo 可用;语言是纯文本时 Format、Minify 不可用
    e.set_value("plain 2".into());
    let a = e.actions();
    assert!(a.save && a.undo && !a.format && !a.minify, "{a:?}");
    e.undo();
    assert!(!e.actions().save);

    // 自动检测出 JSON:Format、Minify 可用;十六进制视图里不可用
    let mut j = string_editor("k", br#"{"a":1}"#);
    assert_eq!(j.format, Format::Json);
    assert!(j.actions().format && j.actions().minify);
    j.hex = true;
    assert!(!j.actions().format && !j.actions().minify);
    j.hex = false;
    j.set_format(Format::Plain);
    assert!(!j.actions().format, "the language decides");
    j.set_format(Format::Json);
    assert!(j.actions().format);

    // 二进制:只读,只能切十六进制
    let bin = string_editor("k", &binary());
    let a = bin.actions();
    assert!(
        !a.save && !a.undo && !a.format && !a.minify && a.hex,
        "{a:?}"
    );

    // 新建:没有十六进制;输入了东西才能保存和撤销
    let mut n = Editor::new_key(KeyType::String);
    let a = n.actions();
    assert!(!a.save && !a.undo && !a.hex && !a.format, "{a:?}");
    n.set_key_name("x".into());
    assert!(n.actions().save && n.actions().undo);
    // 保存在途时不能再保存
    n.saving = true;
    assert!(!n.actions().save);
}

#[test]
fn the_hex_view_is_read_only_and_leaves_the_draft_alone() {
    let mut e = string_editor("k", b"abc");
    e.set_value("abcd".into());
    e.hex = true;
    assert!(
        e.value_text().starts_with("00000000  61 62 63"),
        "{}",
        e.value_text()
    );
    e.set_value("zzz".into());
    assert_eq!(e.value.text, "abcd", "typing is ignored in the hex view");
    e.hex = false;
    assert_eq!(e.value_text(), "abcd", "the draft is still there");
}

#[test]
fn set_format_changes_only_the_language() {
    let (mut h, _) = H::open("文章", KeyType::String);
    h.string_read(b"text");
    assert_eq!(h.view.editor.as_ref().unwrap().format, Format::Plain);
    h.send(ViewMsg::SetFormat(Format::Json));
    let e = h.view.editor.as_ref().unwrap();
    assert_eq!(e.format, Format::Json);
    assert!(
        !e.has_changes(),
        "the language is not a change of the value"
    );
    // 纯文本值切成 JSON 后点 Format:不是合法 JSON,提示并保持原文
    let out = h.send(ViewMsg::FormatJson);
    assert!(last_note(&out).text.contains("not valid JSON"), "{out:?}");
    assert_eq!(h.view.editor.as_ref().unwrap().value.text, "text");
}

#[test]
fn the_editor_explains_why_it_is_read_only() {
    let plain = string_editor("k", b"plain");
    assert_eq!(plain.notice("en"), None);

    let bin = string_editor("k", &binary());
    let text = bin.notice("en").expect("a notice");
    assert!(
        text.contains("binary") && text.contains("hex view"),
        "{text}"
    );

    let mut big = Editor::string_key(b("k"));
    big.loaded_string(Bytes::from(vec![b'a'; 1024 * 1024]), 70 * 1024 * 1024, true);
    let text = big.notice("en").expect("a notice");
    assert!(
        text.contains("70.0 MiB") && text.contains("1.0 MiB") && text.contains("read-only"),
        "{text}"
    );

    // 新建的永远可编辑,没有提示
    assert_eq!(Editor::new_key(KeyType::String).notice("en"), None);
}

#[test]
fn the_view_gives_its_title_and_the_header_texts() {
    let (mut h, _) = H::open("文章", KeyType::String);
    assert_eq!(h.view.title("en"), "文章");
    assert_eq!(h.view.length_text(), None, "not loaded is not zero");
    assert_eq!(h.view.ttl_text(), None);
    h.details(KeyType::String, 0, Ttl::Persistent);
    assert_eq!(h.view.length_text().as_deref(), Some("0"));
    assert_eq!(h.view.ttl_text().as_deref(), Some("-1"));
    h.details(KeyType::String, 12, Ttl::Seconds(90));
    assert_eq!(h.view.length_text().as_deref(), Some("12"));
    assert_eq!(h.view.ttl_text().as_deref(), Some("90"));

    let new = KeyView::new_key(3, KeyType::ZSet).unwrap();
    assert_eq!(new.title("en"), "New ZSet");
    assert_eq!(new.ttl_text(), None);
}

#[test]
fn the_languages_to_choose_from() {
    assert_eq!(Format::ALL, [Format::Plain, Format::Json]);
    assert_eq!(Format::Plain.label("en"), "Plain text");
    assert_eq!(Format::Json.label("en"), "JSON");
}

#[test]
fn rename_and_ttl_are_not_offered_while_creating_or_writing() {
    let (mut h, _) = H::open("文章", KeyType::String);
    h.string_read(b"x");
    assert_eq!(
        h.view.actions(),
        ViewActions {
            rename: true,
            ttl: true
        }
    );
    // 保存在途:不能同时改名和改 TTL
    h.send(ViewMsg::SetValue("y".into()));
    h.send(ViewMsg::Save);
    assert_eq!(
        h.view.actions(),
        ViewActions {
            rename: false,
            ttl: false
        }
    );
    // 新建 key 还没有 key,没有可改名和改 TTL 的
    let new = KeyView::new_key(1, KeyType::String).unwrap();
    assert_eq!(
        new.actions(),
        ViewActions {
            rename: false,
            ttl: false
        }
    );
}

#[test]
fn one_function_turns_a_ttl_into_text() {
    use crate::keylist::ttl_text;
    assert_eq!(ttl_text(Ttl::Persistent), "-1");
    assert_eq!(ttl_text(Ttl::Missing), "-2");
    assert_eq!(ttl_text(Ttl::Seconds(90)), "90");
}

/// 十六进制只转储前 64 KiB,超出时只读提示说明"只显示前 64 KiB,共 N"。
#[test]
fn the_hex_view_dumps_at_most_64_kib_and_says_so() {
    use super::display::HEX_DUMP_LIMIT;
    assert_eq!(HEX_DUMP_LIMIT, 64 * 1024);
    let data = vec![0xabu8; HEX_DUMP_LIMIT + 10];
    let mut e = string_editor("k", &data);
    e.hex = true;
    let dump = e.hex_text();
    assert_eq!(dump.lines().count(), HEX_DUMP_LIMIT / 16, "4096 lines");
    let notice = e.notice("en").expect("a notice");
    assert!(
        notice.contains("64.0 KiB") && notice.contains(&human_bytes(data.len() as u64)),
        "{notice}"
    );
    // 不在十六进制视图里:没有这条提示(值是文本,可编辑)
    let mut text = string_editor("k", &vec![b'a'; HEX_DUMP_LIMIT + 10]);
    assert_eq!(text.notice("en"), None);
    text.hex = true;
    assert!(text.notice("en").is_some());
    // 刚好 64 KiB:不截断,没有提示
    let mut exact = string_editor("k", &vec![b'a'; HEX_DUMP_LIMIT]);
    exact.hex = true;
    assert_eq!(exact.notice("en"), None);
    assert_eq!(exact.hex_text().lines().count(), HEX_DUMP_LIMIT / 16);
}

// ---- R-10b:成员表给界面的查询 ----

#[test]
fn member_rows_come_ready_to_draw() {
    use super::table::MemberRow;
    // Hash:名字加值;二进制的名字或值标出来
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(
        sg,
        vec![
            hash_row("名", "值"),
            Member::Hash {
                field: binary(),
                value: b("x"),
            },
        ],
        0,
    );
    h.send(ViewMsg::Table(TableMsg::Select {
        name: b("名"),
        on: true,
    }));
    let t = h.view.table.as_ref().unwrap();
    let r0 = t.view_row(0).unwrap();
    assert_eq!(
        r0,
        MemberRow {
            name: b("名"),
            name_text: "名".into(),
            value_text: Some("值".into()),
            binary: false,
            selected: true,
        }
    );
    let r1 = t.view_row(1).unwrap();
    assert!(r1.binary && !r1.selected && r1.name_text.contains('\u{fffd}'));
    assert!(t.view_row(2).is_none());

    // List 用下标;Set 没有值列;ZSet 的值列是分数
    let (mut h, out) = H::open("列表", KeyType::List);
    let (sg, _) = scans(&out)[0];
    h.batch(
        sg,
        vec![Member::List {
            index: 3,
            value: b("v"),
        }],
        0,
    );
    let r = h.view.table.as_ref().unwrap().view_row(0).unwrap();
    assert_eq!(
        (r.name_text.as_str(), r.value_text.as_deref()),
        ("3", Some("v"))
    );
    let (mut h, out) = H::open("集合", KeyType::Set);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, vec![Member::Set { member: b("m") }], 0);
    let r = h.view.table.as_ref().unwrap().view_row(0).unwrap();
    assert_eq!((r.name_text.as_str(), r.value_text), ("m", None));
    let (mut h, out) = H::open("榜", KeyType::ZSet);
    let (sg, _) = scans(&out)[0];
    h.batch(
        sg,
        vec![Member::ZSet {
            member: b("a"),
            score: 1.5,
        }],
        0,
    );
    let r = h.view.table.as_ref().unwrap().view_row(0).unwrap();
    assert_eq!(r.value_text.as_deref(), Some("1.5"));
}

#[test]
fn all_filtered_selected_follows_the_selection() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(2), 0);
    assert!(!h.view.table.as_ref().unwrap().all_filtered_selected());
    h.send(ViewMsg::Table(TableMsg::SelectAllFiltered(true)));
    assert!(h.view.table.as_ref().unwrap().all_filtered_selected());
    h.send(ViewMsg::Table(TableMsg::Select {
        name: b("字段0"),
        on: false,
    }));
    assert!(!h.view.table.as_ref().unwrap().all_filtered_selected());
    // 没有行时不算全选
    let (h2, _) = H::open("空", KeyType::Hash);
    assert!(!h2.view.table.as_ref().unwrap().all_filtered_selected());
}

#[test]
fn the_member_toolbar_buttons_follow_the_scan_selection_and_writes() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    // 扫描中:只有 Stop,不能新增和删除
    let a = h.view.member_actions().unwrap();
    assert!(
        !a.search && !a.more && !a.all && a.stop && !a.add && !a.delete,
        "{a:?}"
    );
    // 扫完、还有更多、没选中
    h.batch(sg, hash_rows(2), 2);
    h.batch(sg, hash_rows(4)[2..].to_vec(), 4);
    let a = h.view.member_actions().unwrap();
    assert!(
        a.search && a.more && a.all && !a.stop && a.add && !a.delete,
        "{a:?}"
    );
    // 选中后可以删除
    h.send(ViewMsg::Table(TableMsg::SelectAllFiltered(true)));
    assert!(h.view.member_actions().unwrap().delete);
    // 删除在途:不能再新增和删除
    h.send(ViewMsg::Table(TableMsg::DeleteSelected));
    h.send(ViewMsg::Table(TableMsg::ConfirmDelete));
    let a = h.view.member_actions().unwrap();
    assert!(!a.add && !a.delete, "{a:?}");
    // String 没有成员表
    let (s, _) = H::open("文章", KeyType::String);
    assert!(s.view.member_actions().is_none());
}

#[test]
fn the_member_status_texts_come_from_the_app() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.details(KeyType::Hash, 8, Ttl::Persistent);
    h.batch(sg, hash_rows(2), 2);
    h.batch(sg, hash_rows(4)[2..].to_vec(), 4);
    let s = h.view.member_status("en").unwrap();
    assert_eq!(s.loaded, "Loaded 4 / 8");
    assert_eq!(s.percent.as_deref(), Some("50.00%"));
    assert!(s.more_hint.is_some());
    assert_eq!(s.filtered, None);
    h.send(ViewMsg::Table(TableMsg::SetFilter("字段1".into())));
    h.send(ViewMsg::Table(TableMsg::Search));
    let s = h.view.member_status("en").unwrap();
    assert_eq!(s.loaded, "Loaded 0 / 8");
    assert!(h.view.member_status("en").is_some());
}

#[test]
fn each_opened_editor_gets_a_new_sequence_number_and_a_title() {
    let (mut h, out) = H::open("用户", KeyType::Hash);
    let (sg, _) = scans(&out)[0];
    h.batch(sg, hash_rows(2), 0);
    let s0 = h.view.editor_seq();
    h.send(ViewMsg::OpenMember(b("字段0")));
    let s1 = h.view.editor_seq();
    assert_ne!(s0, s1);
    assert_eq!(h.view.editor_title("en").as_deref(), Some("Edit member"));
    h.send(ViewMsg::CloseEditor);
    h.send(ViewMsg::NewMember);
    assert_ne!(h.view.editor_seq(), s1);
    assert_eq!(h.view.editor_title("en").as_deref(), Some("New member"));
    h.send(ViewMsg::CloseEditor);
    assert_eq!(h.view.editor_title("en"), None);
}
