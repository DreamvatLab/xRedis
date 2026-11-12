//! 内嵌字体:整个界面和编辑器都用 JetBrains Mono(OFL 许可,许可文件在 `assets/fonts/OFL.txt`)。
//! 它没有的字形(中文等)靠系统回退。字体在 `gpui_kit::init` 之后、开窗口之前注册。

use std::borrow::Cow;

use gpui_kit::component::Theme;
use gpui_kit::{App, SharedString};

/// 字体族名(字体文件里的 family name)。
pub const FAMILY: &str = "JetBrains Mono";

/// 内嵌的静态 ttf:界面用到 Normal 和 SemiBold,编辑器和系统可能用 Medium、Bold。
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
];

/// 注册内嵌字体,并把主题的界面字体和等宽字体(编辑器用)都设成 JetBrains Mono。
pub fn install(cx: &mut App) -> Result<(), String> {
    cx.text_system()
        .add_fonts(FONT_FILES.iter().map(|b| Cow::Borrowed(*b)).collect())
        .map_err(|e| e.to_string())?;
    Theme::update(cx, |theme| {
        theme.font_family = SharedString::from(FAMILY);
        theme.mono_font_family = SharedString::from(FAMILY);
    });
    Ok(())
}
