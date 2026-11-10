//! 多语言的测试。用户语言包是全局状态,涉及它的测试用一把锁串行。

use std::fs;
use std::path::Path;

use super::*;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    super::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

const ZH_PACK: &str = r#"_version: 2
editor.title:
  en: "should be ignored: built-in English wins"
  zh-CN: "连接编辑器"
btn.save:
  zh-CN: "保存"
db.item:
  zh-CN: "库%{index}(%{keys} 个 key)"
"#;

#[test]
fn builtin_english_is_the_default_and_the_only_builtin_language() {
    let _g = lock();
    load_user_packs(Path::new("Z:/definitely/not/here"));
    assert_eq!(available_languages(), vec!["en".to_string()]);
    assert_eq!(tr("en", "editor.title"), "Connection Editor");
    assert_eq!(resolve_language("en"), "en");
    assert_eq!(
        resolve_language("fr"),
        "en",
        "an unavailable language falls back to English"
    );
}

#[test]
fn args_are_substituted() {
    assert_eq!(
        tr_with("en", "db.item", &[("index", "3"), ("keys", "42")]),
        "db3 (42 keys)"
    );
    // 参数名不对时原样留着,不 panic
    assert_eq!(
        tr_with("en", "db.item", &[("nope", "x")]),
        "db%{index} (%{keys} keys)"
    );
}

/// 用户语言包叠加在内置包之上:新语言出现在可选列表里,翻译生效,
/// 缺的 key 回落英文,内置英文不被用户包改写。
#[test]
fn user_pack_adds_a_language_and_missing_keys_fall_back_to_english() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "zh-CN.yml", ZH_PACK);
    let report = load_user_packs(tmp.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        report.loaded,
        vec![LoadedPack {
            file: "zh-CN.yml".into(),
            languages: vec!["zh-CN".into()], // 包里的 en 列被丢掉了
        }]
    );

    assert_eq!(
        available_languages(),
        vec!["en".to_string(), "zh-CN".to_string()]
    );
    assert_eq!(resolve_language("zh-CN"), "zh-CN");
    assert_eq!(tr("zh-CN", "editor.title"), "连接编辑器");
    assert_eq!(tr("zh-CN", "btn.save"), "保存");
    assert_eq!(
        tr_with("zh-CN", "db.item", &[("index", "1"), ("keys", "7")]),
        "库1(7 个 key)"
    );
    // 没翻译的 key 回落英文
    assert_eq!(tr("zh-CN", "btn.cancel"), "Cancel");
    // 内置英文优先,用户包里的 en 改不了它
    assert_eq!(tr("en", "editor.title"), "Connection Editor");

    // 清理:重新加载一个空目录,别的测试不受影响
    load_user_packs(Path::new("Z:/definitely/not/here"));
    assert_eq!(available_languages(), vec!["en".to_string()]);
}

/// 坏语言包只跳过它自己,带回文件名和原因;好的照常加载;空文件、没有翻译的文件也是错误。
#[test]
fn a_broken_pack_is_skipped_with_its_file_name_and_does_not_affect_others() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a-good.yml", ZH_PACK);
    write(
        tmp.path(),
        "b-broken.yml",
        "_version: 2\nkey: [unclosed\n  - : :\n",
    );
    write(tmp.path(), "c-empty.yml", "_version: 2\n");
    write(tmp.path(), "d-bad.json", "{ not json");
    write(tmp.path(), "notes.txt", "not a pack, ignored by extension");

    let report = load_user_packs(tmp.path());
    assert_eq!(report.loaded.len(), 1, "{report:?}");
    assert_eq!(report.loaded[0].file, "a-good.yml");
    let mut bad: Vec<&str> = report.errors.iter().map(|e| e.file.as_str()).collect();
    bad.sort();
    assert_eq!(
        bad,
        vec!["b-broken.yml", "c-empty.yml", "d-bad.json"],
        "{report:?}"
    );
    for e in &report.errors {
        assert!(!e.reason.is_empty());
        assert!(
            !e.reason.contains(&*tmp.path().to_string_lossy()),
            "the reason should not repeat the path: {e:?}"
        );
    }
    // 好的包照常可用
    assert_eq!(tr("zh-CN", "btn.save"), "保存");

    load_user_packs(Path::new("Z:/definitely/not/here"));
}

/// 重新加载会替换上一次的内容,删掉的语言包不会残留。
#[test]
fn reloading_replaces_the_previous_packs() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "zh-CN.yml", ZH_PACK);
    load_user_packs(tmp.path());
    assert!(is_available("zh-CN"));
    fs::remove_file(tmp.path().join("zh-CN.yml")).unwrap();
    load_user_packs(tmp.path());
    assert!(!is_available("zh-CN"));
}

/// 导出的模板本身就是合法的语言包:存进 `locales/` 目录能直接读回来,
/// 目标语言那一列先是英文,全部 key 都在。
#[test]
fn exported_template_is_a_valid_pack_covering_every_key() {
    let _g = lock();
    let template = export_template("xx-TEST");
    assert!(template.starts_with("_version: 2"));
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "xx-TEST.yml", &template);
    let report = load_user_packs(tmp.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.loaded[0].languages.contains(&"xx-TEST".to_string()));

    let keys = all_english_keys();
    assert!(keys.len() > 100, "only {} keys", keys.len());
    for k in &keys {
        assert_eq!(tr("xx-TEST", k), tr("en", k), "key {k}");
    }
    // 带引号、换行的英文也能原样往返(免责协议里有引号)
    assert!(tr("xx-TEST", "disclaimer.p0").contains("(\"Agreement\")"));
    load_user_packs(Path::new("Z:/definitely/not/here"));
}

/// 模板里没有目标语言时(目标就是 en)不重复写 en 列。
#[test]
fn template_for_english_has_a_single_column() {
    let t = export_template("en");
    assert!(!t.contains("\n  en: \"\n  en:"), "{t}");
    assert_eq!(t.matches("  en: ").count(), all_english_keys().len());
}

// ---------------------------------------------------------------------------
// 代码里用到的每个 key,英文里都必须有
// ---------------------------------------------------------------------------

/// 内置英文里所有的 key(不含用户包)。
fn all_english_keys() -> Vec<String> {
    let _ = ();
    let yml = include_str!("../../locales/app.yml");
    yml.lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('#') && l.ends_with(':'))
        .map(|l| l.trim_end_matches(':').to_string())
        .collect()
}

/// 扫描 src 下所有源文件里形如 `"menu.file"` 的字符串字面量(前缀属于界面文字的命名空间),
/// 它们都必须在 en 里有。没有这个测试,拼错一个 key 只会在界面上显示 key 本身,没人发现。
#[test]
fn every_key_used_in_code_exists_in_english() {
    const NAMESPACES: &[&str] = &[
        "menu",
        "btn",
        "conn",
        "tab",
        "db",
        "editor",
        "validation",
        "settings",
        "msgbar",
        "msg",
        "disclaimer",
        "privacy",
        "err",
    ];
    let known: std::collections::HashSet<String> = all_english_keys().into_iter().collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = 0;
    let mut missing = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // 这个测试文件自己里面有故意写错的例子,不扫
            if path.file_name().and_then(|n| n.to_str()) == Some("tests.rs")
                && path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    == Some("i18n")
            {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for lit in string_literals(&text) {
                let Some((ns, rest)) = lit.split_once('.') else {
                    continue;
                };
                if !NAMESPACES.contains(&ns) || rest.is_empty() {
                    continue;
                }
                if !lit
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
                {
                    continue; // 句子或别的东西,不是 key
                }
                found += 1;
                if !known.contains(&lit) {
                    missing.push(format!("{lit}  ({})", path.display()));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "keys used in code but missing from locales/app.yml:\n{}",
        missing.join("\n")
    );
    // 防止扫描器坏掉而空过:代码里确实有几十个 key(下限设得低一点,免得以后合并重复 key 时误报)
    assert!(
        found >= 40,
        "the scanner only found {found} keys in the source; is it broken?"
    );
}

/// 取出源码里的双引号字符串字面量(处理转义,不处理原始字符串,够用)。
fn string_literals(src: &str) -> Vec<String> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // 跳过 // 行注释
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == '"' {
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    s.push(b[i + 1]);
                    i += 2;
                    continue;
                }
                s.push(b[i]);
                i += 1;
            }
            out.push(s);
        }
        i += 1;
    }
    out
}

/// 扫描器自己要可靠:认得出字面量、跳过注释、处理转义。
#[test]
fn the_literal_scanner_works() {
    let lits =
        string_literals("let a = \"menu.file\"; // \"btn.nope\"\nlet b = \"say \\\"hi\\\"\";");
    assert_eq!(
        lits,
        vec!["menu.file".to_string(), "say \"hi\"".to_string()]
    );
}

/// 只有 en 列的包没有意义:报错,不加载。包里的 en 列(比如旧模板)不能盖住内置英文。
#[test]
fn english_columns_in_user_packs_are_dropped() {
    let _g = lock();
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "only-en.yml",
        "_version: 2\neditor.title:\n  en: \"STALE ENGLISH\"\n",
    );
    let report = load_user_packs(tmp.path());
    assert_eq!(report.loaded, vec![]);
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert_eq!(report.errors[0].file, "only-en.yml");
    assert_eq!(tr("en", "editor.title"), "Connection Editor");
    load_user_packs(Path::new("Z:/definitely/not/here"));
}
