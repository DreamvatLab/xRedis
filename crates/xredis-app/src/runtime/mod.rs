//! `Runtime`:持有 tokio 运行时和 `Arc<dyn Backend>`,执行 [`BackendCmd`],结果变成 [`Msg`] 发回。
//!
//! core 的异步函数跑在 tokio 上(fred 需要),tokio 运行时由这里创建和持有(多线程,后台线程);
//! GUI 用自己的执行器,两者只通过 channel 交互。

use std::sync::Arc;

use crate::backend::Backend;
use crate::console::ConsoleMsg;
use crate::keyview::ViewMsg;
use crate::msg::{BackendCmd, KeysMsg, Msg};
use crate::pubsub_view::PubSubMsg;

pub struct Runtime {
    /// `Option` 是为了 `Drop` 里能把它取出来 `shutdown_background`(在异步环境里丢弃 runtime 会 panic)。
    rt: Option<tokio::runtime::Runtime>,
    inline: bool,
    backend: Arc<dyn Backend>,
    tx: async_channel::Sender<Msg>,
}

impl Runtime {
    pub fn new(backend: Arc<dyn Backend>, tx: async_channel::Sender<Msg>) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("xredis-rt")
            .enable_all()
            .build()?;
        Ok(Self::with_rt(rt, false, backend, tx))
    }

    /// 同步执行版:`spawn` 在调用线程上跑完命令再返回,结果已经在 channel 里。给 GUI 的确定性测试用
    /// (GPUI 的测试调度器不许别的线程有动静);生产代码用 [`Runtime::new`]。
    pub fn inline(
        backend: Arc<dyn Backend>,
        tx: async_channel::Sender<Msg>,
    ) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self::with_rt(rt, true, backend, tx))
    }

    fn with_rt(
        rt: tokio::runtime::Runtime,
        inline: bool,
        backend: Arc<dyn Backend>,
        tx: async_channel::Sender<Msg>,
    ) -> Self {
        // 订阅收到的消息:按连接分流,变成 Msg 发回(无界 channel,不会阻塞 core 的监听任务)
        let sink_tx = tx.clone();
        backend.set_message_sink(Arc::new(move |m| {
            let _ = sink_tx.try_send(Msg::PubSub {
                id: m.conn,
                msg: PubSubMsg::Incoming(m),
            });
        }));
        Self {
            rt: Some(rt),
            inline,
            backend,
            tx,
        }
    }

    /// 在后台执行,不阻塞调用方;结果(如果有)发进 channel。
    pub fn spawn(&self, cmd: BackendCmd) {
        let backend = self.backend.clone();
        let tx = self.tx.clone();
        if self.inline {
            if let Some(msg) = self.rt().block_on(execute(backend.as_ref(), cmd)) {
                let _ = tx.try_send(msg);
            }
            return;
        }
        self.rt().spawn(async move {
            if let Some(msg) = execute(backend.as_ref(), cmd).await {
                // 接收方没了(程序在退出)就算了
                let _ = tx.send(msg).await;
            }
        });
    }

    pub fn handle(&self) -> &tokio::runtime::Handle {
        self.rt().handle()
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.rt.as_ref().expect("the runtime lives until drop")
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // 不等正在跑的任务:退出时不能被一个连不上的连接拖住。
        // `shutdown_background` 在任何环境(包括异步环境)里都不会 panic,直接丢弃 runtime 会。
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// 执行一条后端命令,返回结果对应的 `Msg`。没有结果要回报的命令(关闭连接)返回 `None`。
pub async fn execute(backend: &dyn Backend, cmd: BackendCmd) -> Option<Msg> {
    Some(match cmd {
        BackendCmd::Startup => Msg::StartupDone(Box::new(backend.startup().await)),
        BackendCmd::LoadConnections => Msg::ConnectionsLoaded(backend.connections().await),
        BackendCmd::SaveConnection(draft) => {
            Msg::ConnectionSaved(backend.save_connection(draft).await)
        }
        BackendCmd::DeleteConnection(id) => Msg::ConnectionDeleted {
            id,
            result: backend.delete_connection(id).await,
        },
        BackendCmd::Connect { id, epoch } => Msg::Connected {
            id,
            epoch,
            result: backend.connect(id).await,
        },
        BackendCmd::CloseConnection(id) => {
            if let Err(err) = backend.close_connection(id).await {
                tracing::debug!("关闭连接 {id} 出错: {err}");
            }
            return None;
        }
        BackendCmd::SelectDb { id, epoch, db } => Msg::DbSelected {
            id,
            epoch,
            db,
            result: backend.select_db(id, db).await,
        },
        BackendCmd::SaveSettings { settings, token } => Msg::SettingsSaved {
            token,
            result: backend.save_settings(settings).await,
        },
        BackendCmd::SaveWindowSize {
            width,
            height,
            maximized,
        } => Msg::WindowSaved(backend.save_window_size(width, height, maximized).await),

        // ---- Keys 子页 ----
        BackendCmd::Scan {
            id,
            generation,
            pattern,
            count,
            cursor,
        } => Msg::Keys {
            id,
            msg: KeysMsg::ScanBatch {
                generation,
                result: backend.scan_keys(id, pattern, count, cursor).await,
            },
        },
        BackendCmd::KeyDetails {
            id,
            generation,
            key,
        } => Msg::Keys {
            id,
            msg: KeysMsg::DetailsLoaded {
                generation,
                result: backend.key_details(id, key.clone()).await,
                key,
            },
        },
        BackendCmd::DeleteKeys { id, keys } => Msg::Keys {
            id,
            msg: KeysMsg::Deleted(backend.delete_keys(id, keys).await),
        },
        BackendCmd::DbSize { id } => Msg::Keys {
            id,
            msg: KeysMsg::DbSize(backend.db_size(id).await),
        },
        BackendCmd::CopyKeys { id, keys } => Msg::Keys {
            id,
            msg: KeysMsg::Copied(backend.copy_keys(id, keys).await),
        },
        BackendCmd::PreviewPaste { id, text } => Msg::Keys {
            id,
            msg: KeysMsg::PasteChecked {
                result: backend.preview_paste(id, text.clone()).await,
                text,
            },
        },
        BackendCmd::RunCommand {
            id,
            generation,
            tokens,
        } => Msg::Console {
            id,
            msg: ConsoleMsg::Executed {
                generation,
                result: backend.run_command(id, tokens).await,
            },
        },
        BackendCmd::PubSubSubscribe {
            id,
            generation,
            channel,
        } => Msg::PubSub {
            id,
            msg: PubSubMsg::Subscribed {
                generation,
                result: backend.subscribe(id, channel).await,
            },
        },
        BackendCmd::PubSubUnsubscribe {
            id,
            generation,
            channel,
        } => Msg::PubSub {
            id,
            msg: PubSubMsg::Unsubscribed {
                generation,
                result: backend.unsubscribe(id, channel).await,
            },
        },
        BackendCmd::PubSubPublish {
            id,
            generation,
            channel,
            message,
        } => Msg::PubSub {
            id,
            msg: PubSubMsg::Published {
                generation,
                result: backend.publish(id, channel, message).await,
            },
        },
        BackendCmd::PubSubChannels { id, generation } => Msg::PubSub {
            id,
            msg: PubSubMsg::Channels {
                generation,
                result: backend.channels(id).await,
            },
        },
        BackendCmd::ViewDetails {
            id,
            generation,
            key,
        } => Msg::View {
            id,
            msg: ViewMsg::Details {
                generation,
                result: backend.key_details(id, key).await,
            },
        },
        BackendCmd::ViewReadString {
            id,
            generation,
            key,
        } => Msg::View {
            id,
            msg: ViewMsg::StringRead {
                generation,
                result: backend.read_string(id, key).await,
            },
        },
        BackendCmd::ViewScan {
            id,
            generation,
            scan_generation,
            key,
            kind,
            pattern,
            count,
            cursor,
        } => Msg::View {
            id,
            msg: ViewMsg::Members {
                generation,
                scan_generation,
                result: backend
                    .scan_members(id, key, kind, pattern, count, cursor)
                    .await,
            },
        },
        BackendCmd::ViewApply {
            id,
            generation,
            token,
            ops,
        } => Msg::View {
            id,
            msg: ViewMsg::Applied {
                generation,
                token,
                result: backend.apply_writes(id, ops).await,
            },
        },
        BackendCmd::PasteKeys {
            id,
            text,
            overwrite,
        } => Msg::Keys {
            id,
            msg: KeysMsg::Pasted(backend.paste_keys(id, text, overwrite).await),
        },
    })
}

#[cfg(test)]
mod redis_tests;
#[cfg(test)]
pub(crate) mod tests;
