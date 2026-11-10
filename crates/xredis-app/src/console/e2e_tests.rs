//! 控制台的端到端:App 加 Runtime 加假后端(假后端的命令集很小:PING、BIG n、其余回显)。

use std::time::Duration;

use xredis_core::config::ConnId;

use super::*;
use crate::msg::Msg;
use crate::runtime::tests::Harness;
use crate::testing::FakeBackend;

async fn ready(fake: &std::sync::Arc<FakeBackend>) -> (Harness, ConnId) {
    let id = fake.add_connection("a", 0);
    let mut h = Harness::new(fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    h.settle(200).await;
    (h, id)
}

fn console(h: &Harness, id: ConnId) -> &Console {
    &h.app.tab(id).expect("a tab").console
}

fn send(h: &mut Harness, id: ConnId, msg: ConsoleMsg) {
    h.feed(Msg::Console { id, msg });
}

fn run(h: &mut Harness, id: ConnId, line: &str) {
    send(h, id, ConsoleMsg::Insert(line.into()));
    send(h, id, ConsoleMsg::Submit);
}

fn lines(h: &Harness, id: ConnId) -> Vec<String> {
    console(h, id).output().cloned().collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_goes_to_the_backend_and_the_reply_shows_up() {
    let fake = FakeBackend::new();
    let (mut h, id) = ready(&fake).await;
    run(&mut h, id, "PING");
    assert!(console(&h, id).is_running());
    h.settle(100).await;
    assert!(!console(&h, id).is_running());
    assert_eq!(lines(&h, id), ["> PING", "PONG"]);

    // 带空格和引号的参数整体传过去
    run(&mut h, id, r#"SET k "hello world""#);
    h.settle(100).await;
    let sent = fake.commands();
    assert_eq!(
        sent.last().unwrap(),
        &vec![b"SET".to_vec(), b"k".to_vec(), b"hello world".to_vec()]
    );
    assert_eq!(lines(&h, id).last().unwrap(), "OK k|hello world");
}

/// 本地就能判断的(clear、空行、引号没闭合)一个后端请求都不发。
#[tokio::test(flavor = "multi_thread")]
async fn local_commands_and_syntax_errors_never_reach_the_backend() {
    let fake = FakeBackend::new();
    let (mut h, id) = ready(&fake).await;
    let before = fake.calls().len();
    run(&mut h, id, "clear");
    run(&mut h, id, "   ");
    run(&mut h, id, r#"GET "oops"#);
    h.settle(100).await;
    assert_eq!(fake.calls().len(), before);
    assert!(fake.commands().is_empty());
    assert!(lines(&h, id).last().unwrap().contains("unbalanced quotes"));
}

/// P-50:命令超过 500 ms 没返回显示等待动画,返回后结束。
#[tokio::test(flavor = "multi_thread")]
async fn the_waiting_animation_shows_for_slow_commands() {
    let fake = FakeBackend::new();
    let (mut h, id) = ready(&fake).await;
    fake.set_delay(Duration::from_millis(900));
    run(&mut h, id, "PING");
    let now = || xredis_core::config::now_ms();
    assert!(
        console(&h, id).waiting_frame(now()).is_none(),
        "not right away"
    );
    h.pump(650).await; // 没有结果回来,只是等
    assert!(
        console(&h, id).waiting_frame(now()).is_some(),
        "after 500 ms"
    );
    h.settle(600).await;
    assert!(!console(&h, id).is_running());
    assert!(console(&h, id).waiting_frame(now()).is_none());
    assert_eq!(lines(&h, id).last().unwrap(), "PONG");
}

/// 命令还没返回连接就被重建(切库):结果作废,控制台可以继续用,历史和输出还在。
#[tokio::test(flavor = "multi_thread")]
async fn a_reconnect_abandons_the_running_command() {
    let fake = FakeBackend::new();
    let (mut h, id) = ready(&fake).await;
    run(&mut h, id, "PING");
    h.settle(100).await;
    fake.hang("run_command");
    run(&mut h, id, "BLPOP q 0");
    assert!(console(&h, id).is_running());

    h.feed(Msg::SelectDb { id, db: 1 });
    h.settle(300).await;
    assert!(
        !console(&h, id).is_running(),
        "the abandoned command no longer blocks the console"
    );
    assert_eq!(console(&h, id).history(), ["PING", "BLPOP q 0"]);
    assert!(lines(&h, id).contains(&"PONG".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_output_is_capped_at_ten_thousand_lines_end_to_end() {
    let fake = FakeBackend::new();
    let (mut h, id) = ready(&fake).await;
    run(&mut h, id, "BIG 12000");
    h.settle(300).await;
    let c = console(&h, id);
    assert_eq!(c.output_len(), OUTPUT_MAX_LINES);
    assert_eq!(c.output().last().unwrap(), "12000) line 12000");
}

/// L-02:连接没就绪时,控制台的输入一律忽略,不发请求。
#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_sent_while_the_connection_is_not_ready() {
    let fake = FakeBackend::new();
    let id = fake.add_connection("a", 0);
    fake.hang("connect");
    let mut h = Harness::new(&fake);
    h.boot().await;
    h.feed(Msg::OpenConnection(id));
    h.settle(100).await;
    run(&mut h, id, "PING");
    h.settle(100).await;
    assert_eq!(console(&h, id).input(), "", "the typing was ignored");
    assert!(fake.commands().is_empty());
}
