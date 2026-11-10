//! `Runtime`:持有 tokio 运行时和 `Arc<dyn Backend>`,执行 [`BackendCmd`],结果变成 [`Msg`] 发回。
//!
//! core 的异步函数跑在 tokio 上(fred 需要),tokio 运行时由这里创建和持有(多线程,后台线程);
//! GUI 用自己的执行器,两者只通过 channel 交互。

use std::sync::Arc;

use crate::backend::Backend;
use crate::msg::{BackendCmd, KeysMsg, Msg};

pub struct Runtime {
    /// `Option` 是为了 `Drop` 里能把它取出来 `shutdown_background`(在异步环境里丢弃 runtime 会 panic)。
    rt: Option<tokio::runtime::Runtime>,
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
        Ok(Self {
            rt: Some(rt),
            backend,
            tx,
        })
    }

    /// 在后台执行,不阻塞调用方;结果(如果有)发进 channel。
    pub fn spawn(&self, cmd: BackendCmd) {
        let backend = self.backend.clone();
        let tx = self.tx.clone();
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
