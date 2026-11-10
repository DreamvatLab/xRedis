# 09 · 当前状态

> **只放现在有效的状态,收工或"下一步"变了时整份重写。**历史在日志里。
> 最后重写:2026-11-10(架构师)。

## 怎么读

1. [`AGENTS.md`](../AGENTS.md):规矩。
2. 本文件。
3. [`decisions.md`](decisions.md):作者的决定。先查这里再问作者。
4. [`tickets.md`](tickets.md):工单。
5. 设计:[`01-architecture.md`](01-architecture.md)(分层和模块)、[`02-v1-parity.md`](02-v1-parity.md)(v1 功能清单,也是验收标准)。
6. 日志 [`log/`](log/) 不通读,按节号查。

## 现在在做

- v2.0 追平 v1。工单 R-01 至 R-12 按顺序做,范围和完成标准在第 1 节。Windows 施工者从 R-01 开始。

## 等作者

- R-13:macOS、Linux 的机器和施工者。

## 会话和机器

- 架构师:Happier 会话 `cmwb2h23r479nde01o6rj6fut`,worktree `D:\git\DreamvatLab\xRedis-arch`(detached HEAD)。
- Windows 施工者:Happier 会话 `cmwb2pw9k48mrde01l2d8hwas`,主目录 `D:\git\DreamvatLab\xRedis`。
- 机器 ECOMDEV-210:Rust 1.99.1,Docker 可用。测试用 Redis 只用 `scripts/test-redis.sh` 起的容器(`127.0.0.1:6391`)。
