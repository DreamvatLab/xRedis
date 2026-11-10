//! 控制台命令执行:用独立的控制台客户端,拒绝会改变连接模式的命令,阻塞命令加客户端侧超时。

use std::time::Duration;

use fred::clients::Client;
use fred::interfaces::ClientLike;
use fred::types::{ClusterHash, CustomCommand, Value};

use crate::consts::CONSOLE_BLOCKING_TIMEOUT_SECS;
use crate::error::{Error, Result};
use crate::value::arg;

/// 会让连接进入特殊模式的命令(不区分大小写)。
///
/// 这些命令执行后,连接不再接受普通命令(订阅模式只能收消息,MONITOR 会持续推送)。而控制台的
/// 客户端是**共享**的 —— `PUBSUB CHANNELS` 也用它,一旦进入特殊模式,控制台和频道列表会一起失效,
/// 只能靠重连恢复。订阅在界面里有专门的 Pub/Sub 页面,这里直接拒绝并引导过去(L-06 的延伸)。
pub const CONNECTION_MODE_COMMANDS: &[&str] = &[
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "UNSUBSCRIBE",
    "PUNSUBSCRIBE",
    "SUNSUBSCRIBE",
    "MONITOR",
    "RESET",
];

/// 会阻塞连接直到超时或有数据的命令。合法用法(比如等队列消息),但界面里若写成无限等待
/// (`BLPOP key 0`)就会一直转圈且无法取消,所以统一加一个客户端侧超时兜底。
pub const BLOCKING_COMMANDS: &[&str] = &[
    "BLPOP",
    "BRPOP",
    "BLMOVE",
    "BRPOPLPUSH",
    "BLMPOP",
    "BZPOPMIN",
    "BZPOPMAX",
    "BZMPOP",
    "WAIT",
    "WAITAOF",
];

/// 这行命令是不是阻塞的:命令名在名单里,或者参数里带 `BLOCK`(XREAD、XREADGROUP)。
pub(crate) fn is_blocking(name_upper: &str, args: &[Vec<u8>]) -> bool {
    BLOCKING_COMMANDS.contains(&name_upper) || args.iter().any(|a| a.eq_ignore_ascii_case(b"BLOCK"))
}

/// 命令名大写(命令名一定是 ASCII;不是的话原样大写,Redis 会报未知命令)。
pub(crate) fn command_name_upper(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).trim().to_uppercase()
}

/// 检查命令是否允许在控制台执行。
pub(crate) fn check_allowed(name_upper: &str) -> Result<()> {
    if CONNECTION_MODE_COMMANDS.contains(&name_upper) {
        return Err(Error::CommandRejected {
            command: name_upper.to_string(),
        });
    }
    Ok(())
}

/// 执行已经分好词的一行:第一个是命令名,其余是参数(字节,原样传给 Redis,L-07)。
/// 空行返回 `None`(什么也不做)。
pub async fn run_command(client: &Client, tokens: &[Vec<u8>]) -> Result<Option<Value>> {
    run_command_with_timeout(
        client,
        tokens,
        Duration::from_secs(CONSOLE_BLOCKING_TIMEOUT_SECS),
    )
    .await
}

pub(crate) async fn run_command_with_timeout(
    client: &Client,
    tokens: &[Vec<u8>],
    blocking_timeout: Duration,
) -> Result<Option<Value>> {
    let Some((name, args)) = tokens.split_first() else {
        return Ok(None);
    };
    let upper = command_name_upper(name);
    check_allowed(&upper)?;
    let blocking = is_blocking(&upper, args);
    // 命令名要 'static 或拥有的字符串,用拥有的版本
    let cmd = CustomCommand::new(upper.clone(), ClusterHash::FirstKey, blocking);
    let values: Vec<Value> = args.iter().map(|a| arg(a)).collect();
    let pending = client.custom::<Value, _>(cmd, values);
    let value = if blocking {
        match tokio::time::timeout(blocking_timeout, pending).await {
            Ok(r) => r?,
            Err(_) => {
                return Err(Error::BlockingTimeout {
                    command: upper,
                    secs: blocking_timeout.as_secs(),
                });
            }
        }
    } else {
        pending.await?
    };
    Ok(Some(value))
}
