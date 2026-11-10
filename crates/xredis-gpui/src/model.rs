//! `MainView`:一个 `Entity` 持有 `xredis_app::App` 和 `Runtime`。
//!
//! 流程:用户操作 → `dispatch(Msg)` → `App::update` → 返回的 `Cmd`:后端命令交给 `Runtime`(tokio),
//! 平台命令在这里执行(开网址、文件对话框、剪贴板、定时……),结果再变成 `Msg` 回来。
//! 后端的结果从 `async_channel` 进来:每次先把 channel 里已有的都取完再重画一次(All 扫描时一批
//! 一条消息,不能每批都画一遍)。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::*;
use xredis_app::messages::Level;
use xredis_app::msg::{FileField, PlatformCmd};
use xredis_app::{App, Backend, Cmd, Msg, Runtime};

pub struct MainView {
    pub(crate) app: App,
    runtime: Runtime,
    /// 已经弹过的 toast(`App` 里的 toast 在用户关掉或自动消失之前一直在)。
    shown_toasts: HashSet<u64>,
    _subs: Vec<Subscription>,
}

impl MainView {
    pub fn new(backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let runtime = Runtime::new(backend, tx).expect("start the runtime");
        let mut app = App::new();
        let start = app.start();

        // 窗口大小和最大化状态变化时通知 App(App 做防抖和保存,P-61)
        let bounds_sub = cx.observe_window_bounds(window, |this, window, cx| {
            let (width, height, maximized) = window_state(window);
            this.dispatch(
                Msg::WindowChanged {
                    width,
                    height,
                    maximized,
                },
                window,
                cx,
            );
        });

        let mut view = Self {
            app,
            runtime,
            shown_toasts: HashSet::new(),
            _subs: vec![bounds_sub],
        };
        view.run(start, window, cx);

        // 后端结果:一次取完 channel 里已有的,处理完只通知一次
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let done = this.update_in(cx, |this, window, cx| {
                    for msg in batch {
                        this.apply(msg, window, cx);
                    }
                    this.after_batch(window, cx);
                });
                if done.is_err() {
                    break; // 界面没了
                }
            }
        })
        .detach();
        view
    }

    /// 已经弹出过、还没消失的 toast 个数(测试用)。
    pub fn shown_toast_count(&self) -> usize {
        self.shown_toasts.len()
    }

    /// 用户操作入口:处理一条消息并重画。
    pub fn dispatch(&mut self, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
        self.apply(msg, window, cx);
        self.after_batch(window, cx);
    }

    /// 处理一条消息,执行返回的命令;不通知重画(由 `after_batch` 统一做)。
    fn apply(&mut self, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
        let cmds = self.app.update(msg);
        self.run(cmds, window, cx);
    }

    fn after_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_toasts(window, cx);
        cx.notify();
    }

    fn run(&mut self, cmds: Vec<Cmd>, window: &mut Window, cx: &mut Context<Self>) {
        for cmd in cmds {
            match cmd {
                Cmd::Backend(b) => self.runtime.spawn(b),
                Cmd::Platform(p) => self.platform(p, window, cx),
            }
        }
    }

    /// 平台能力的执行。结果变成 `Msg` 发回 `App`。
    fn platform(&mut self, cmd: PlatformCmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            PlatformCmd::OpenUrl(url) => cx.open_url(&url),
            PlatformCmd::Exit => cx.quit(),
            PlatformCmd::SetLocale(language) => {
                // 组件库自带的文字(它也用 rust-i18n)跟着变;重画由 after_batch 做
                xredis_app::i18n::apply_global_locale(&language);
            }
            PlatformCmd::After { ms, msg } => {
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(ms))
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| this.dispatch(*msg, window, cx));
                })
                .detach();
            }
            PlatformCmd::PickFile(field) => self.pick_file(field, window, cx),
            PlatformCmd::SaveTextFile {
                suggested_name,
                contents,
            } => self.save_text_file(suggested_name, contents, window, cx),
            PlatformCmd::WriteClipboard { id, text } => {
                // GPUI 的写剪贴板没有错误返回:能报告的只有“成功”,写失败(被别的程序占着)
                // 在这一层看不到(L-13 在 R-12 验收里记差别)
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.apply(
                    Msg::Keys {
                        id,
                        msg: xredis_app::msg::KeysMsg::ClipboardWritten(Ok(())),
                    },
                    window,
                    cx,
                );
            }
            PlatformCmd::ReadClipboard { id } => {
                let result = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .ok_or_else(|| "the clipboard is empty or does not contain text".to_string());
                self.apply(
                    Msg::Keys {
                        id,
                        msg: xredis_app::msg::KeysMsg::ClipboardRead(result),
                    },
                    window,
                    cx,
                );
            }
        }
    }

    /// 文件对话框选一个文件,结果用 `Msg::EditorFilePicked` 发回;取消是 `None`。
    fn pick_file(&mut self, field: FileField, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match rx.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.dispatch(Msg::EditorFilePicked(field, picked), window, cx)
            });
        })
        .detach();
    }

    /// “另存为”对话框加写文件,结果用 `Msg::TemplateSaved` 发回(`Ok(None)` 是用户取消)。
    fn save_text_file(
        &mut self,
        suggested_name: String,
        contents: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested_name));
        cx.spawn_in(window, async move |this, cx| {
            let result = match rx.await {
                Ok(Ok(Some(path))) => std::fs::write(&path, contents)
                    .map(|()| Some(path.display().to_string()))
                    .map_err(|e| format!("{}: {e}", path.display())),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Ok(None),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.dispatch(Msg::TemplateSaved(result), window, cx)
            });
        })
        .detach();
    }

    /// `App` 里新出现的 toast 弹出来;用户关掉或自动消失时通知 `App` 去掉它。
    fn sync_toasts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fresh: Vec<_> = self
            .app
            .messages
            .toasts()
            .iter()
            .filter(|t| !self.shown_toasts.contains(&t.id))
            .cloned()
            .collect();
        for toast in fresh {
            self.shown_toasts.insert(toast.id);
            let id = toast.id;
            let entity = cx.entity();
            let note = Notification::new()
                .message(toast.text)
                .with_type(match toast.level {
                    Level::Success => NotificationType::Success,
                    Level::Warn => NotificationType::Warning,
                    Level::Error => NotificationType::Error,
                    Level::Info | Level::Debug => NotificationType::Info,
                })
                .on_close(move |window, cx| {
                    entity.update(cx, |this, cx| {
                        this.dispatch(Msg::DismissToast(id), window, cx);
                    });
                });
            window.push_notification(note, cx);
        }
        let live: HashSet<u64> = self.app.messages.toasts().iter().map(|t| t.id).collect();
        self.shown_toasts.retain(|id| live.contains(id));
    }
}

/// 窗口当前的大小和最大化状态。最大化时记的是还原后的尺寸,下次启动不会把“最大化的大小”当成普通尺寸。
pub(crate) fn window_state(window: &Window) -> (u32, u32, bool) {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (b, false),
        WindowBounds::Maximized(b) => (b, true),
        WindowBounds::Fullscreen(b) => (b, false),
    };
    (
        f32::from(bounds.size.width).round() as u32,
        f32::from(bounds.size.height).round() as u32,
        maximized,
    )
}
