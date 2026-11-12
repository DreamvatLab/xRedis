//! 多语言(作者 2026-11-10,docs/01「多语言」)。
//!
//! - 内置英文:`locales/app.yml`,编译时由 `rust_i18n::i18n!`(在 lib.rs)嵌入。
//! - 用户语言包:配置目录 `locales/` 下的文件(格式和内置包相同,一个文件可带一种或多种语言),
//!   [`load_user_packs`] 在运行时读进来,叠加在内置包之上。**一个文件一个文件地读**,坏文件只跳过
//!   它自己并带回文件名和原因,不影响启动,也不影响其他语言包。内置的 `en` 优先:用户包里的 `en` 列会被丢掉,不能改英文。
//! - 缺的 key 回落英文(`fallback = "en"`)。
//! - **我们自己的文字用显式的语言参数查**([`tr`]),不依赖全局 locale,所以 App 的逻辑是纯的、
//!   测试互不干扰。全局的 `rust_i18n::set_locale` 只用来切换 gpui-component 自带的文字,
//!   那是 GUI 层的事([`apply_global_locale`])。

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{LazyLock, RwLock};

use rust_i18n::{Backend, SimpleBackend};
use xredis_core::consts::DEFAULT_LANGUAGE;

/// 运行时加载的用户语言包。整个替换,重新加载时旧的丢掉。
static USER_PACKS: LazyLock<RwLock<SimpleBackend>> =
    LazyLock::new(|| RwLock::new(SimpleBackend::new()));

/// 挂在 `i18n!` 宏上的后端:查用户语言包。
pub struct UserPacks;

impl Backend for UserPacks {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        owned_locales(&USER_PACKS.read().unwrap())
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        USER_PACKS
            .read()
            .unwrap()
            .translate(locale, key)
            .map(|c| Cow::Owned(c.into_owned()))
    }

    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        USER_PACKS
            .read()
            .unwrap()
            .messages_for_locale(locale)
            .map(|v| {
                v.into_iter()
                    .map(|(k, val)| (Cow::Owned(k.into_owned()), Cow::Owned(val.into_owned())))
                    .collect()
            })
    }
}

fn owned_locales(b: &SimpleBackend) -> Vec<Cow<'static, str>> {
    b.available_locales()
        .into_iter()
        .map(|l| Cow::Owned(l.into_owned()))
        .collect()
}

/// 一个成功加载的语言包文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPack {
    pub file: String,
    pub languages: Vec<String>,
}

/// 一个被跳过的语言包文件和原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackError {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackReport {
    pub loaded: Vec<LoadedPack>,
    pub errors: Vec<PackError>,
}

/// 读取目录下所有语言包文件(`.yml`、`.yaml`、`.json`、`.toml`,不递归)。目录不存在不算错。
/// 成功的叠加进全局语言包(替换上一次加载的),失败的只记到报告里。
pub fn load_user_packs(dir: &Path) -> PackReport {
    let mut report = PackReport::default();
    let mut merged: BTreeMap<String, HashMap<Cow<'static, str>, Cow<'static, str>>> =
        BTreeMap::new();

    let mut files: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                matches!(
                    Path::new(n).extension().and_then(|e| e.to_str()),
                    Some("yml" | "yaml" | "json" | "toml")
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();

    let dir_str = dir.to_string_lossy().into_owned();
    for name in files {
        // rust-i18n 一次读一个目录;用 ignore_if 让它只读这一个文件
        let suffix = format!("/{name}");
        let result = rust_i18n::try_load_locales(
            &dir_str,
            |path| !path.replace('\\', "/").ends_with(&suffix),
            true,
        );
        match result {
            Ok(mut map) => {
                // 内置英文优先:用户包里的 en 列直接丢掉。用户按旧版模板翻译时模板里带着当时的英文列,
                // 应用之后改了英文文案,旧英文不能盖住新文案。
                map.remove(DEFAULT_LANGUAGE);
                if map.is_empty() {
                    report.errors.push(PackError {
                        file: name,
                        reason: "The file contains no translations other than English.".into(),
                    });
                    continue;
                }
                let languages: Vec<String> = map.keys().cloned().collect();
                for (locale, trs) in map {
                    let entry = merged.entry(locale).or_default();
                    for (k, v) in trs {
                        entry.insert(Cow::Owned(k), Cow::Owned(v));
                    }
                }
                report.loaded.push(LoadedPack {
                    file: name,
                    languages,
                });
            }
            Err(reason) => report.errors.push(PackError {
                file: name,
                reason: short_reason(&reason),
            }),
        }
    }

    let mut backend = SimpleBackend::new();
    for (locale, trs) in merged {
        backend.add_translations(Cow::Owned(locale), trs);
    }
    *USER_PACKS.write().unwrap() = backend;
    report
}

/// rust-i18n 的报错里带完整路径,界面上已经单独显示文件名,只留原因。
fn short_reason(full: &str) -> String {
    match full.split_once("reason: ") {
        Some((_, r)) => r.to_string(),
        None => full.to_string(),
    }
}

/// 所有可选的语言(内置加用户语言包),默认语言排第一,其余按名字排序。
pub fn available_languages() -> Vec<String> {
    let mut langs: Vec<String> = crate::_rust_i18n_available_locales()
        .into_iter()
        .map(|c| c.into_owned())
        .collect();
    langs.sort();
    langs.dedup();
    if let Some(i) = langs.iter().position(|l| l == DEFAULT_LANGUAGE) {
        let d = langs.remove(i);
        langs.insert(0, d);
    } else {
        langs.insert(0, DEFAULT_LANGUAGE.to_string());
    }
    langs
}

/// 这个语言能不能选(内置的或语言包里有的)。
pub fn is_available(language: &str) -> bool {
    available_languages().iter().any(|l| l == language)
}

/// 选择语言:不可用就回落默认语言。返回实际使用的语言。
pub fn resolve_language(wanted: &str) -> String {
    if is_available(wanted) {
        wanted.to_string()
    } else {
        DEFAULT_LANGUAGE.to_string()
    }
}

/// 取文字。缺的 key 回落英文;英文里也没有时返回 key 本身(测试保证代码里用到的 key 英文里都有)。
pub fn tr(locale: &str, key: &str) -> String {
    match crate::_rust_i18n_try_translate(locale, key) {
        Some(s) => s.into_owned(),
        None => key.to_string(),
    }
}

/// 取文字并替换 `%{名字}` 参数。
pub fn tr_with(locale: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = tr(locale, key);
    for (name, value) in args {
        s = s.replace(&format!("%{{{name}}}"), value);
    }
    s
}

/// 切换全局 locale,让 gpui-component 自带的文字(它也用 rust-i18n)跟着变。GUI 层调用。
pub fn apply_global_locale(language: &str) {
    rust_i18n::set_locale(language);
}

/// 导出语言包模板:全部 key 和英文,`target` 那一列先填英文,用户照着翻译。
/// 格式和内置包相同(`_version: 2`),存成配置目录 `locales/` 下的文件就能直接用。
pub fn export_template(target: &str) -> String {
    let mut messages: Vec<(String, String)> = crate::_rust_i18n_backend()
        .messages_for_locale(DEFAULT_LANGUAGE)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    messages.sort();
    let quote = |s: &str| serde_json::to_string(s).expect("string is always serializable");
    let mut out = String::new();
    out.push_str("_version: 2\n\n");
    out.push_str(&format!(
        "# Language pack template. Rename \"{target}\" to your language code (for example zh-CN) and translate the text under it.\n"
    ));
    out.push_str(
        "# Keep the %{name} parameters as they are. Missing keys fall back to English.\n\n",
    );
    for (key, en) in messages {
        out.push_str(&format!("{key}:\n  {DEFAULT_LANGUAGE}: {}\n", quote(&en)));
        if target != DEFAULT_LANGUAGE {
            out.push_str(&format!("  {target}: {}\n", quote(&en)));
        }
    }
    out
}

/// 全局语言包是共享状态:所有会加载用户语言包的测试(i18n 的、App 的、调 `CoreBackend::startup()` 的)
/// 用这把锁串行。用 tokio 的锁:异步测试可以 `lock().await` 跨 await 持有,同步测试用 `blocking_lock()`。
/// tokio 的锁不会中毒,一个测试 panic 不连累别的。
#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests;
