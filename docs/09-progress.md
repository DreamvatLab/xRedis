# 09 · 当前状态

> **只放现在有效的状态,收工或"下一步"变了时整份重写。**历史在日志里。
> 最后重写:2026-11-12(架构师)。

## 怎么读

1. [`AGENTS.md`](../AGENTS.md):规矩。**特别注意「测试范围」(只跑受影响的测试,完整测试要作者和架构师都批准)和"文件名不许含 secret"。**
2. 本文件。
3. [`decisions.md`](decisions.md):作者的决定。先查这里再问作者。
4. [`tickets.md`](tickets.md):工单。
5. 设计:[`01-architecture.md`](01-architecture.md)(分层、模块、多语言、和 v1 的有意差别)、[`02-v1-parity.md`](02-v1-parity.md)(v1 功能清单,也是验收标准,带 P 和 L 编号)。
6. 日志 [`log/001-100.md`](log/001-100.md) 不通读,按节号查。目前写到第 58 节,新的一节接着最大的编号往下写。

## 做到哪了

v2.0 的目标是追平 v1(作者 2026-11-10)。

- **core 层(`xredis-core`)完成**:R-02 至 R-05 都已审核通过。
- **视图模型层(`xredis-app`)完成**:R-06、R-07 都已审核通过。做 GUI 时又补了一批给界面读的接口(按钮能不能点、状态栏文字、段落列表、十六进制上限等),都有测试。
- **GUI 层(`xredis-gpui`)完成**:R-08 至 R-11 都已审核通过(第 41 至 58 节)。
  - 壳:侧栏、Tab、消息栏和 toast、顶部菜单、文件日志、窗口尺寸。
  - 弹窗:连接编辑器、设置、免责协议、隐私政策、错误框、删除确认。
  - Keys 子页、String 编辑器、成员表和成员编辑器、十六进制视图、改名和 TTL、JSON 语法高亮。
  - Console 和 Pub/Sub 子页。
- **测试数**(各张单审核时最后一次跑的,只跑了受影响的):gpui 102;app 的 keylist 82、keyview 83、console 41、pubsub_view 28、i18n 10。全部通过。没跑过完整测试。
- **发布版** `xredis.exe` 约 30 MB(第 58 节)。

## 下一步:R-12 验收(要作者批准)

1. **完整测试**:`cargo test --workspace`、不带过滤的 `scripts/test-redis.sh test`、对全部 crate 的 clippy。按「测试范围」先写理由,得到作者批准再跑。
2. **真窗口试用**:到现在一次都没做过,无头测试看不到外观。要看的有:
   - 布局、菜单点击(包括 Help、库下拉、New、语言下拉,它们在无头测试里点不到);
   - 真实的文件对话框和剪贴板;
   - 真实的最大化和窗口尺寸恢复;
   - 滚动手感、高亮颜色、等待动画、输入法。
3. **逐条对照 `02` 的 P 和 L 编号**。
4. **要量的数据**:
   - 空闲内存;
   - 发布版体积,和 v1 比,考虑开 LTO、strip;
   - 控制台输出满 1 万行后连续输出的开销(第 56 节);
   - Keys 表格 1 万行的实际手感。
5. **R-12 时要清理的**:`Console::word_left`、`word_right` 生产代码已经不用了,还是没人用的话就连同测试一起删(第 56 节)。

## 等作者

- 批准 R-12 的完整测试;安排真窗口试用(作者自己试,还是让 agent 启动程序截图)。
- 分支 `wip/R-08a` 还留着(内容已经合进 dev),删不删由作者定。
- R-13:macOS、Linux 的机器。

## 2026-11-12 的工作方式

作者安排:只有一个会话。架构师统筹和审核,工单派给 Sonnet 5.5 子 agent,在主目录施工。子 agent 提交但不推送,架构师审核通过后再推送(第 41 节)。架构师在 `xRedis-arch` 里 detached 检出要审的提交来重跑测试,`CARGO_TARGET_DIR` 指向主目录的 `target/`。

## 会话和机器

- 架构师 worktree `D:\git\DreamvatLab\xRedis-arch`(detached HEAD),主目录 `D:\git\DreamvatLab\xRedis`。会话以作者给的为准。
- 机器 ECOMDEV-210:Rust 1.99.1,Docker 可用。测试用的 Redis 只用 `scripts/test-redis.sh` 起的容器(`127.0.0.1:6391` 普通、`6392` TLS),用完 `stop`。注意:`scripts/test-redis.sh test` 只跑带 `#[ignore]` 的测试。
- v1 源码:`D:\git\Lukiya\xRedis`(只读参考)。
