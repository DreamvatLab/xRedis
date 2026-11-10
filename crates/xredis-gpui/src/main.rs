use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;

struct Root {
    input: Entity<InputState>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child("xRedis v2 空窗口 / 中文 日本語 😀")
            .child(Button::new("ok").primary().label("OK"))
            .child("↓ 输入法测试框:在这里用中文输入法打字")
            .child(Input::new(&self.input))
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1600.), px(903.)),
                cx,
            ))),
            window_min_size: Some(size(px(1024.), px(840.))),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("xRedis v{}", env!("CARGO_PKG_VERSION")).into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            cx.new(|_| Root { input })
        }).expect("open window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    // 不能 use super::*:会带进 gpui_kit::test 宏,#[test] 展开后指回自己导致无限递归
    use super::Root;
    use gpui_kit::component::input::InputState;
    use gpui_kit::{AppContext, TestAppContext};

    // R-01 风险 4:TestAppContext 能通过 gpui-kit 用上
    #[gpui_kit::test]
    fn root_renders_in_headless_window(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            Root { input }
        });
        cx.run_until_parked();
        assert!(window.update(cx, |_, _, _| ()).is_ok());
    }
}
