//! Runtime、`execute` 和生产 `CoreBackend` 的测试。不需要 Redis(连不上的端口用本机一个没人听的端口)。
//! 覆盖 P-04、P-07、P-66;L-02;第 6 节、第 10 节。

use std::sync::Arc;
use std::time::{Duration, Instant};

use xredis_core::Error;
use xredis_core::config::{ConnId, ConnectionDraft, PasswordChange};
use xredis_core::keystore::{KeyStore, MemoryStore};

use super::*;
use crate::app::{AGREEMENT_VERSION, App, TabStatus};
use crate::backend::CoreBackend;
use crate::msg::{Cmd, PlatformCmd};
use crate::testing::FakeBackend;

// ---------------------------------------------------------------------------
// execute:每条后端命令变成对应的 Msg
// ---------------------------------------------------------------------------

#[tokio::test]
async fn execute_turns_each_backend_command_into_its_msg() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);

    // 连接:纪元原样带回去
    match execute(fake.as_ref(), BackendCmd::Connect { id, epoch: 42 }).await {
        Some(Msg::Connected {
            id: i,
            epoch: 42,
            result: Ok(info),
        }) => {
            assert_eq!(i, id);
            assert_eq!(info.databases.len(), 16);
        }
        other => panic!("{other:?}"),
    }
    // 注入的错误原样带回去,只一次
    fake.fail_next("connect", Error::Auth("nope".into()));
    assert!(matches!(
        execute(fake.as_ref(), BackendCmd::Connect { id, epoch: 7 }).await,
        Some(Msg::Connected {
            epoch: 7,
            result: Err(Error::Auth(_)),
            ..
        })
    ));
    assert!(matches!(
        execute(fake.as_ref(), BackendCmd::Connect { id, epoch: 8 }).await,
        Some(Msg::Connected { result: Ok(_), .. })
    ));
    // 关闭没有结果 Msg,但确实调用了后端
    assert!(
        execute(fake.as_ref(), BackendCmd::CloseConnection(id))
            .await
            .is_none()
    );
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.starts_with("close_connection:"))
    );
    // 切库
    assert!(matches!(
        execute(
            fake.as_ref(),
            BackendCmd::SelectDb {
                id,
                epoch: 3,
                db: 5
            }
        )
        .await,
        Some(Msg::DbSelected {
            epoch: 3,
            db: 5,
            result: Ok(()),
            ..
        })
    ));
    assert_eq!(fake.store().connection(id).unwrap().selected_db, 5);
    // 设置:token 原样带回
    let settings = fake.store().settings();
    assert!(matches!(
        execute(
            fake.as_ref(),
            BackendCmd::SaveSettings {
                settings,
                token: 99
            }
        )
        .await,
        Some(Msg::SettingsSaved {
            token: 99,
            result: Ok(())
        })
    ));
    assert!(matches!(
        execute(
            fake.as_ref(),
            BackendCmd::SaveWindowSize {
                width: 1111,
                height: 999,
                maximized: true
            }
        )
        .await,
        Some(Msg::WindowSaved(Ok(())))
    ));
    assert_eq!(fake.store().settings().window_width, 1111);
    // 加载连接、删除
    assert!(matches!(
        execute(fake.as_ref(), BackendCmd::LoadConnections).await,
        Some(Msg::ConnectionsLoaded(v)) if v.len() == 1
    ));
    assert!(matches!(
        execute(fake.as_ref(), BackendCmd::DeleteConnection(id)).await,
        Some(Msg::ConnectionDeleted { result: Ok(()), .. })
    ));
    assert!(matches!(
        execute(fake.as_ref(), BackendCmd::Startup).await,
        Some(Msg::StartupDone(d)) if d.connections.is_empty()
    ));
}

// ---------------------------------------------------------------------------
// App 加 Runtime 加假 Backend 的端到端
// ---------------------------------------------------------------------------

struct Harness {
    app: App,
    rt: Runtime,
    rx: async_channel::Receiver<Msg>,
    platform: Vec<String>,
}

impl Harness {
    fn new(fake: &Arc<FakeBackend>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self {
            app: App::new(),
            rt: Runtime::new(fake.clone(), tx).unwrap(),
            rx,
            platform: Vec::new(),
        }
    }

    /// 喂一条 Msg,执行返回的命令(后端的交给 Runtime,平台的记下来)。
    fn feed(&mut self, msg: Msg) {
        let cmds = self.app.update(msg);
        self.run(cmds);
    }

    fn run(&mut self, cmds: Vec<Cmd>) {
        for c in cmds {
            match c {
                Cmd::Backend(b) => self.rt.spawn(b),
                Cmd::Platform(p) => self.platform.push(format!("{p:?}")),
            }
        }
    }

    /// 等下一条结果(最多 `ms` 毫秒),喂给 App。没等到返回 false。
    async fn pump(&mut self, ms: u64) -> bool {
        match tokio::time::timeout(Duration::from_millis(ms), self.rx.recv()).await {
            Ok(Ok(msg)) => {
                self.feed(msg);
                true
            }
            _ => false,
        }
    }

    /// 启动并同意协议。
    async fn boot(&mut self) {
        let cmds = self.app.start();
        self.run(cmds);
        assert!(self.pump(2000).await, "startup result");
        self.feed(Msg::AgreeDisclaimer);
        while self.pump(300).await {}
    }
}

/// L-02、P-04:连接流程。打开 → 连接中 → 结果回来 → 就绪;就绪之前 `can_request` 一直是 false。
#[tokio::test(flavor = "multi_thread")]
async fn open_connection_goes_connecting_then_ready() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.set_delay(Duration::from_millis(100));
    fake.set_key_counts(vec![5, 0, 9]);
    let mut h = Harness::new(&fake);
    h.boot().await;
    assert!(h.app.agreed());

    h.feed(Msg::OpenConnection(id));
    assert_eq!(h.app.tab(id).unwrap().status, TabStatus::Connecting);
    assert!(!h.app.can_request(id));
    assert!(h.pump(2000).await);
    assert!(h.app.can_request(id));
    match &h.app.tab(id).unwrap().status {
        TabStatus::Ready(info) => {
            assert_eq!(info.databases.len(), 3);
            assert_eq!(info.databases[2].keys, 9, "P-05: key counts per db");
        }
        other => panic!("{other:?}"),
    }
}

/// P-04、L-02:连接还在建(连不上的主机)时关 Tab:Tab 立刻没了,后台去关连接;
/// 建连一直挂着也不会有任何结果回来把 Tab 复活或弹错误框。
#[tokio::test(flavor = "multi_thread")]
async fn closing_a_tab_while_connecting_is_immediate_even_if_the_connect_never_returns() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("dead-host", 0);
    fake.hang("connect");
    let mut h = Harness::new(&fake);
    h.boot().await;

    h.feed(Msg::OpenConnection(id));
    assert_eq!(h.app.tab(id).unwrap().status, TabStatus::Connecting);
    let started = Instant::now();
    h.feed(Msg::CloseTab(id));
    assert!(h.app.tabs.is_empty());
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "closing must not wait for anything"
    );

    // 给后台一点时间去关连接
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.starts_with("close_connection:")),
        "{:?}",
        fake.calls()
    );
    assert!(
        !h.pump(300).await,
        "nothing comes back from the hung connect"
    );
    assert!(h.app.error_dialog.is_none() && h.app.tabs.is_empty());
}

/// 后端关连接挂住了(已经断开的连接发 QUIT):Tab 照样立刻没了,Runtime 也没被拖住,别的连接照常用。
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_close_does_not_block_the_ui_or_other_connections() {
    let fake = FakeBackend::new();
    let (a, b) = (fake.add_connection("a", 0), fake.add_connection("b", 1));
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(a));
    assert!(h.pump(2000).await);
    assert!(h.app.can_request(a));

    fake.hang("close_connection");
    h.feed(Msg::CloseTab(a));
    assert!(h.app.tab(a).is_none());

    h.feed(Msg::OpenConnection(b));
    assert!(
        h.pump(2000).await,
        "the runtime is still serving other connections"
    );
    assert!(h.app.can_request(b));
}

/// 第 10 节:关 Tab 之后迟到的结果(后端比较慢)回来了,要丢弃:不复活 Tab,不弹错误框。
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_result_arriving_after_the_tab_was_closed_is_dropped() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("slow", 0);
    fake.set_delay(Duration::from_millis(250));
    fake.fail_next("connect", Error::Timeout("late failure".into()));
    let mut h = Harness::new(&fake);
    h.boot().await;

    h.feed(Msg::OpenConnection(id));
    h.feed(Msg::CloseTab(id));
    assert!(
        h.pump(2000).await,
        "the late (failed) connect result does arrive"
    );
    assert!(h.app.tabs.is_empty(), "the tab must stay closed");
    assert!(
        h.app.error_dialog.is_none(),
        "no error dialog for a tab the user already closed"
    );
}

/// P-07:连接失败时 Tab 显示失败和原因,错误框可见;重试成功后恢复。
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connect_shows_the_reason_and_retry_recovers() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.fail_next(
        "connect",
        Error::Connect("Connection refused (os error 111)".into()),
    );
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    assert!(h.pump(2000).await);
    assert!(matches!(
        h.app.tab(id).unwrap().status,
        TabStatus::Failed(_)
    ));
    assert!(
        h.app
            .error_dialog
            .as_ref()
            .unwrap()
            .message
            .contains("Connection refused")
    );

    h.feed(Msg::DismissError);
    h.feed(Msg::Reconnect(id));
    assert!(h.pump(2000).await);
    assert!(h.app.can_request(id));
}

/// P-05:切库的完整往返:切 → 后端关掉重建 → 重新取连接信息 → 就绪,库号被记住。
#[tokio::test(flavor = "multi_thread")]
async fn switching_db_round_trip() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    assert!(h.pump(2000).await);
    h.feed(Msg::SelectDb { id, db: 4 });
    assert!(!h.app.can_request(id));
    assert!(h.pump(2000).await, "db selected");
    assert!(h.pump(2000).await, "connection info reloaded");
    assert!(h.app.can_request(id));
    assert_eq!(h.app.connection(id).unwrap().selected_db, 4);
    assert_eq!(
        fake.store().connection(id).unwrap().selected_db,
        4,
        "persisted to the config"
    );
    let calls = fake.calls();
    let sel = calls
        .iter()
        .position(|c| c.starts_with("select_db:"))
        .unwrap();
    let conn_after = calls
        .iter()
        .rposition(|c| c.starts_with("connect:"))
        .unwrap();
    assert!(
        conn_after > sel,
        "reconnect happens after the switch: {calls:?}"
    );
}

/// 编辑器保存连接的完整往返:新建 → 列表刷新 → 排序;密码走密钥存储(内存版),不在配置里。
#[tokio::test(flavor = "multi_thread")]
async fn saving_a_connection_through_the_editor_refreshes_the_list() {
    let fake = FakeBackend::new();
    let mut h = Harness::new(&fake);
    h.boot().await;
    assert!(h.app.connections.is_empty());

    h.feed(Msg::OpenEditor(None));
    h.feed(Msg::EditorSetText(
        crate::msg::EditorText::Name,
        "新连接".into(),
    ));
    h.feed(Msg::EditorSetText(
        crate::msg::EditorText::Password,
        "s3cret".into(),
    ));
    h.feed(Msg::EditorSave);
    assert!(h.pump(2000).await, "saved");
    assert!(h.app.editor.is_none());
    assert!(h.pump(2000).await, "list reloaded");
    assert_eq!(h.app.connections.len(), 1);
    assert_eq!(h.app.connections[0].name, "新连接");
    let text = std::fs::read_to_string(fake.store().path()).unwrap();
    assert!(!text.contains("s3cret"), "the config file has no password");
}

// ---------------------------------------------------------------------------
// CoreBackend(生产实现)
// ---------------------------------------------------------------------------

/// 一直失败的密钥存储(模拟 Linux 没有 Secret Service)。
struct BrokenKeyStore;

impl KeyStore for BrokenKeyStore {
    fn get(&self, _: &str) -> xredis_core::Result<Option<String>> {
        Err(Error::KeyStore("no secret service".into()))
    }
    fn set(&self, _: &str, _: &str) -> xredis_core::Result<()> {
        Err(Error::KeyStore("no secret service".into()))
    }
    fn delete(&self, _: &str) -> xredis_core::Result<()> {
        Err(Error::KeyStore("no secret service".into()))
    }
}

fn draft(name: &str) -> ConnectionDraft {
    ConnectionDraft {
        id: None,
        name: name.into(),
        host: "127.0.0.1".into(),
        port: 6379,
        ordinal: 0,
        username: None,
        tls: None,
        password: PasswordChange::Keep,
    }
}

/// P-66:配置目录建不出来(路径被文件占用)时不崩溃:改用临时目录继续运行,启动结果带警告,
/// 之后还能保存和读取连接。
#[tokio::test(flavor = "multi_thread")]
async fn p66_an_unusable_config_dir_falls_back_to_a_temporary_one_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let occupied = tmp.path().join("v2");
    std::fs::write(&occupied, b"i am a file, not a directory").unwrap();

    let backend = CoreBackend::open(occupied.clone(), Arc::new(MemoryStore::new()), None)
        .expect("must not fail when the temp dir works");
    assert_ne!(backend.config_dir(), occupied.as_path());
    let data = backend.startup().await;
    assert!(
        data.warnings
            .iter()
            .any(|w| w.contains("not usable") && w.contains("temporary")),
        "{:?}",
        data.warnings
    );
    assert_eq!(data.settings, Default::default());

    let saved = backend.save_connection(draft("works")).await.unwrap();
    assert!(saved.outcome.warnings.is_empty());
    assert_eq!(backend.connections().await.len(), 1);
    assert_eq!(
        std::fs::read(&occupied).unwrap(),
        b"i am a file, not a directory",
        "the occupying file is untouched"
    );
    let _ = std::fs::remove_dir_all(backend.config_dir());
}

/// P-66:配置文件损坏:改名留存,用默认值继续,警告带给界面。
#[tokio::test(flavor = "multi_thread")]
async fn p66_a_corrupt_config_file_is_kept_aside_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.json"), b"{ broken").unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None).unwrap();
    let data = backend.startup().await;
    assert!(
        data.warnings
            .iter()
            .any(|w| w.contains("config.json.broken")),
        "{:?}",
        data.warnings
    );
    assert!(tmp.path().join("config.json.broken").exists());
    assert!(data.connections.is_empty());
}

/// 第 6 节:生产环境用 `FallbackStore` 包钥匙串。钥匙串不可用时启动就能发现(探测一次),
/// 密码降级到内存,保存连接时密码照样"存"上了(内存里),界面提示一次。
#[tokio::test(flavor = "multi_thread")]
async fn a_broken_keystore_is_detected_at_startup_and_falls_back_to_memory() {
    let tmp = tempfile::tempdir().unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(BrokenKeyStore), None).unwrap();
    let data = backend.startup().await;
    assert!(data.keystore_degraded, "the startup probe must notice");

    let mut d = draft("a");
    d.password = PasswordChange::Set("pw".into());
    let saved = backend.save_connection(d).await.unwrap();
    assert!(saved.keystore_degraded);
    assert!(
        saved.outcome.password_error.is_none(),
        "memory fallback accepted it"
    );
    let id = saved.outcome.id;
    assert_eq!(backend.store().password(id).unwrap().as_deref(), Some("pw"));
    let text = std::fs::read_to_string(backend.store().path()).unwrap();
    assert!(!text.contains("\"pw\""), "no password in the config file");
}

/// 钥匙串正常时不降级,启动结果里没有降级标志。
#[tokio::test(flavor = "multi_thread")]
async fn a_working_keystore_is_not_reported_as_degraded() {
    let tmp = tempfile::tempdir().unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None).unwrap();
    assert!(!backend.startup().await.keystore_degraded);
}

/// 第一次启动时从 v1 导入(只读 v1 的文件);之后不再导入。
#[tokio::test(flavor = "multi_thread")]
async fn first_start_imports_from_v1_once_and_never_writes_the_v1_file() {
    let tmp = tempfile::tempdir().unwrap();
    let v1 = tmp.path().join("v1-config.json");
    let (id1, id2) = (ConnId::new(), ConnId::new());
    let json = format!(
        r#"{{"settings":{{"agreement":"1.0.0","manual_scan_limit":300}},"connections":[
            {{"id":"{id1}","host":"10.0.0.1","name":"prod","port":6380,"ordinal":2}},
            {{"id":"{id2}","host":"localhost","port":6379,"ordinal":1}}]}}"#
    );
    std::fs::write(&v1, &json).unwrap();
    let before = std::fs::read(&v1).unwrap();
    let dir = tmp.path().join("v2");

    let backend =
        CoreBackend::open(dir.clone(), Arc::new(MemoryStore::new()), Some(v1.clone())).unwrap();
    let data = backend.startup().await;
    let import = data.import.expect("imported");
    assert_eq!((import.connections, import.error), (2, None));
    assert_eq!(data.connections.len(), 2);
    assert_eq!(data.settings.scan_batch, 300);
    assert_eq!(
        data.settings.agreement, AGREEMENT_VERSION,
        "the agreement the user already gave carries over"
    );
    assert_eq!(
        std::fs::read(&v1).unwrap(),
        before,
        "the v1 file is never written"
    );

    // 再启动一次(同一个进程里 startup 再调,或下次启动):不再导入
    assert!(backend.startup().await.import.is_none());
    let again = CoreBackend::open(dir, Arc::new(MemoryStore::new()), Some(v1)).unwrap();
    let data = again.startup().await;
    assert!(data.import.is_none());
    assert_eq!(data.connections.len(), 2);

    // 用户的 v1 文件损坏:报告导入失败,不崩溃,不写 v2 的连接
    let bad = tmp.path().join("bad-v1.json");
    std::fs::write(&bad, b"{ nope").unwrap();
    let backend = CoreBackend::open(
        tmp.path().join("v2b"),
        Arc::new(MemoryStore::new()),
        Some(bad),
    )
    .unwrap();
    let data = backend.startup().await;
    assert!(data.import.as_ref().unwrap().error.is_some());
    assert!(data.connections.is_empty());
    assert!(
        data.warnings.iter().any(|w| w.contains("v1")),
        "{:?}",
        data.warnings
    );
}

/// 启动时加载用户语言包,结果带给界面。
#[test]
fn startup_loads_user_language_packs_from_the_config_dir() {
    // 同步测试:std 的锁不能跨 await 持有,所以在锁里自己起一个 runtime block_on
    let _g = crate::i18n::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let locales = tmp.path().join("locales");
    std::fs::create_dir_all(&locales).unwrap();
    std::fs::write(
        locales.join("zh-CN.yml"),
        "_version: 2\nbtn.save:\n  zh-CN: \"保存\"\n",
    )
    .unwrap();
    std::fs::write(locales.join("broken.yml"), "_version: 2\nx: [unclosed\n").unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None).unwrap();
    let data = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(backend.startup());
    assert_eq!(data.packs.loaded.len(), 1);
    assert_eq!(data.packs.errors.len(), 1);
    assert_eq!(data.packs.errors[0].file, "broken.yml");
    assert!(crate::i18n::available_languages().contains(&"zh-CN".to_string()));
    crate::i18n::load_user_packs(std::path::Path::new("Z:/definitely/not/here"));
}

/// 保存、编辑、删除:配置和密码一致;编辑时密码 Keep 不动、Clear 清掉。
#[tokio::test(flavor = "multi_thread")]
async fn save_edit_and_delete_keep_config_and_password_consistent() {
    let tmp = tempfile::tempdir().unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None).unwrap();
    let mut d = draft("a");
    d.password = PasswordChange::Set("pw1".into());
    let id = backend.save_connection(d).await.unwrap().outcome.id;
    assert_eq!(
        backend.store().password(id).unwrap().as_deref(),
        Some("pw1")
    );

    let mut edit = draft("a2");
    edit.id = Some(id);
    backend.save_connection(edit.clone()).await.unwrap();
    assert_eq!(
        backend.store().password(id).unwrap().as_deref(),
        Some("pw1"),
        "Keep leaves it"
    );
    edit.password = PasswordChange::Clear;
    backend.save_connection(edit).await.unwrap();
    assert_eq!(backend.store().password(id).unwrap(), None);

    backend.delete_connection(id).await.unwrap();
    assert!(backend.connections().await.is_empty());
    assert!(matches!(
        backend.delete_connection(id).await,
        Err(Error::NotFound(_))
    ));
}

/// P-07:连不上的端口,首次连接要给出错误,不能永远转圈。fred 的重连策略是不限次数的,
/// 这条测试确认首次建连失败会报出来(本机一个没人听的端口,最多等 40 秒)。
#[tokio::test(flavor = "multi_thread")]
async fn p07_connecting_to_a_closed_port_reports_an_error_instead_of_spinning_forever() {
    let tmp = tempfile::tempdir().unwrap();
    let backend =
        CoreBackend::open(tmp.path().to_path_buf(), Arc::new(MemoryStore::new()), None).unwrap();
    let mut d = draft("nobody-home");
    d.port = 1; // 没有任何东西在听
    let id = backend.save_connection(d).await.unwrap().outcome.id;

    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(40), backend.connect(id)).await;
    let elapsed = started.elapsed();
    match result {
        Ok(Err(Error::Connect(_) | Error::Timeout(_) | Error::Redis(_))) => {
            eprintln!("closed port reported an error after {elapsed:?}");
        }
        Ok(other) => panic!("unexpected result after {elapsed:?}: {other:?}"),
        Err(_) => panic!(
            "connecting to a closed port was still pending after {elapsed:?}: the UI would spin forever"
        ),
    }
    // 失败之后关闭也要立刻返回,不留客户端
    let t = Instant::now();
    backend.close_connection(id).await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(3));
}

/// 平台命令不会被 Runtime 执行(那是 GUI 的事):Runtime 只认后端命令。
#[test]
fn platform_commands_are_not_the_runtimes_business() {
    // 编译期保证:Runtime::spawn 只接受 BackendCmd。这里只确认 Cmd 的两个分支能区分。
    let c = Cmd::Platform(PlatformCmd::Exit);
    assert!(matches!(c, Cmd::Platform(_)));
}
