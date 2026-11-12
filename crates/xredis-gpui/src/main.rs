use std::sync::Arc;

use gpui_kit::*;
use xredis_app::{Backend, CoreBackend};
use xredis_core::consts::{WINDOW_MIN_HEIGHT, WINDOW_MIN_WIDTH};

fn main() {
    // 配置目录和窗口尺寸要在创建窗口之前就知道:日志放配置目录下,窗口按上次的大小开(P-61、P-65)
    let backend = match CoreBackend::production() {
        Ok(b) => Arc::new(b),
        Err(err) => {
            eprintln!("xRedis cannot start: {err}");
            std::process::exit(1);
        }
    };
    let _log_guard = xredis_gpui::logging::init(backend.config_dir());
    let (width, height, maximized) = backend.saved_window();
    // 按设置的语言先调一次,避免界面先闪一下英文;启动完成后 App 还会按解析过的语言再调
    xredis_app::i18n::apply_global_locale(&backend.saved_language());
    tracing::info!("xRedis v{} starting", env!("CARGO_PKG_VERSION"));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let size = size(px(width as f32), px(height as f32));
            let bounds = Bounds::centered(None, size, cx);
            let options = WindowOptions {
                window_bounds: Some(if maximized {
                    WindowBounds::Maximized(bounds)
                } else {
                    WindowBounds::Windowed(bounds)
                }),
                window_min_size: Some(gpui_kit::size(
                    px(WINDOW_MIN_WIDTH as f32),
                    px(WINDOW_MIN_HEIGHT as f32),
                )),
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("xRedis v{}", env!("CARGO_PKG_VERSION")).into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let backend: Arc<dyn Backend> = backend;
            gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| xredis_gpui::MainView::new(backend, window, cx))
            })
            .expect("open the window");
            cx.activate(true);
        });
}
