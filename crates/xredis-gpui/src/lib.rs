//! xredis-gpui:只负责画和转发用户操作。业务逻辑和界面状态都在 `xredis-app`,
//! 这里发现需要的判断 `App` 里没有,先去 `xredis-app` 加(带测试),再画。

pub mod logging;
pub mod model;
mod ui;

pub use model::MainView;

#[cfg(test)]
mod tests;
