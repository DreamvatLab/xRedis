//! xredis-app:视图模型层(App、Msg、Cmd、Runtime、Backend)。不依赖任何 GUI。

// 内置英文语言包(编译时嵌入),用户语言包通过 `i18n::UserPacks` 叠加(docs/01「多语言」)
rust_i18n::i18n!("locales", fallback = "en", backend = crate::i18n::UserPacks);

pub mod app;
pub mod backend;
pub mod errors;
pub mod forms;
pub mod i18n;
pub mod keylist;
pub mod keyview;
pub mod messages;
pub mod msg;
pub mod runtime;
// 假 Backend:只给测试用(本 crate 自己的测试,或别的 crate 在 dev-dependencies 里打开 `test-support`)
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod warning_tests;

pub use app::{App, Tab, TabStatus};
pub use backend::{Backend, CoreBackend};
pub use msg::{BackendCmd, Cmd, Msg, PlatformCmd};
pub use runtime::Runtime;

pub fn core_name() -> &'static str {
    xredis_core::crate_name()
}
