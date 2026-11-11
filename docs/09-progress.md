# 09 · 当前状态

> **只放现在有效的状态,收工或"下一步"变了时整份重写。**历史在日志里。
> 最后重写:2026-11-10 收工(架构师)。

## 怎么读

1. [`AGENTS.md`](../AGENTS.md):规矩。**特别注意「测试范围」(只跑受影响的,完整测试要作者和架构师批准)和"文件名不许含 secret"。**
2. 本文件。
3. [`decisions.md`](decisions.md):作者的决定。先查这里再问作者。
4. [`tickets.md`](tickets.md):工单。
5. 设计:[`01-architecture.md`](01-architecture.md)(分层、模块、多语言、和 v1 的有意差别)、[`02-v1-parity.md`](02-v1-parity.md)(v1 功能清单,也是验收标准,P 和 L 编号)。
6. 日志 [`log/001-100.md`](log/001-100.md) 不通读,按节号查。目前写到第 39 节,新的一节接着最大的编号往下写。

## 做到哪了

v2.0 的目标是追平 v1(作者 2026-11-10)。

- **core 层(`xredis-core`)完成**:配置、钥匙串、v1 导入、连接管理(三种客户端、关闭纪元、切库、重连、TLS)、key 和五种类型、成员写操作、复制粘贴(和 v1 剪贴板格式兼容)、控制台、Pub/Sub。错误全部结构化,不含给用户看的句子。R-02 至 R-05 及返工单 R-02a、R-03a、R-04a 都已审核通过。
- **视图模型层(`xredis-app`)完成**:`App`/`Msg`/`Cmd`/`Runtime`/`Backend`、多语言(rust-i18n,内置英文加用户语言包)、连接、Tab、设置、消息栏、协议、key 列表、值编辑器、成员表、控制台、Pub/Sub。R-06、R-07(拆成 R-07a 至 R-07e)及返工单 R-06a、R-06b 都已审核通过。
- **GUI 层(`xredis-gpui`)刚开始**:R-08 拆成 R-08a(壳)和 R-08b(弹窗),开工前的设计问题在第 38 节答复了。**R-08a 没做完**:代码在分支 `wip/R-08a`(`71643b1`),`dev` 上只有第 39 节和工单状态。GUI 壳和文件日志已写好并编译通过(日志的 4 个测试通过);9 个无头 GUI 测试卡在 gpui 测试调度器的"非确定性"检查(tokio 线程唤醒 GPUI 前台任务),原因、坑和接着做的顺序见第 39 节。
- 测试数(收工前最后一次):app lib 284、core 单元约 110、Redis 测试 core 约 64 和 app 2,都通过。

## 下一步

1. 施工者从 `wip/R-08a` 接着做 R-08a(先解决第 39 节的测试调度器问题),做完合进 `dev` 再审。架构师先读第 39 节,判断那个问题要不要改设计(比如测试里不用真的 tokio Runtime、改用同步执行的假 Runtime)。
2. 按顺序:R-08b → R-09(Keys 子页)→ R-10(String 编辑器、成员表、十六进制视图;语法高亮要 gpui-kit 的 tree-sitter 特性,施工者到时报)→ R-11(Console、Pub/Sub)→ R-12(验收,要作者批准完整测试)。
3. 每张 GUI 工单审核时专门查 `xredis-gpui` 里有没有业务判断(红线)。

## 等作者

- R-13:macOS、Linux 的机器和施工者。
- R-12 验收要跑完整测试,到时按「测试范围」申请。

## 收工时的约定(这一任架构师的做法,供参考)

- 施工者推送后发"R-xx 已推 <hash>,第 N 节",架构师读差异、只重跑受影响的测试、写一节审核、改工单、回消息。架构师和施工者都往同一个日志文件末尾写,**写之前先 fetch 看最大节号**,用了哪一节就在消息里告诉对方下一节从几号开始。
- 架构师每 10 分钟巡检一次施工者会话(`session.events.get`)和 `origin/dev`,发现停了就查原因。施工者曾经因为后台命令里套 `&` 收不到完成通知而停住 20 分钟。
- 返工小单用 `R-xxa`、`R-xxb` 编号,和原单一起记在 `tickets.md`。

## 会话和机器

- 架构师:Happier 会话 `cmwb2h23r479nde01o6rj6fut`(本任到此为止,下一任换新会话),worktree `D:\git\DreamvatLab\xRedis-arch`(detached HEAD,用 `git push origin HEAD:dev` 推)。
- Windows 施工者:Happier 会话 `cmwb2pw9k48mrde01l2d8hwas`,主目录 `D:\git\DreamvatLab\xRedis`。作者可能也会换新会话,以作者给的为准。
- 机器 ECOMDEV-210:Rust 1.99.1,Docker 可用。测试用 Redis 只用 `scripts/test-redis.sh` 起的容器(`127.0.0.1:6391` 普通、`6392` TLS),用完 `stop`。
- v1 源码:`D:\git\Lukiya\xRedis`(只读参考)。
