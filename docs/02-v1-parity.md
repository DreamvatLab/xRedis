# 02 · v1 功能清单(v2.0 的验收标准)

> 架构师 2026-11-10,依据通读 v1 代码(`D:\git\Lukiya\xRedis`,1.0.2)。v2.0 要逐条做到;标"差别"的按 `01-architecture.md`「v2.0 和 v1 的有意差别」做。
> 每条有编号(P-xx),工单、测试和验收记录引用编号。v1 源文件路径都相对于 v1 仓库。

## 1. 连接

- **P-01 连接类型**:只做单机 TCP,可选 TLS。认证:密码,或 ACL 用户名加密码。v1 不支持 Cluster、Sentinel、SSH、Unix socket,v2.0 也不做。
- **P-02 连接编辑器**:Name、Host、Port(1–65535)、Username、Password、Ordinal(-65535..65535)、TLS 开关、CA 证书、客户端证书、客户端私钥(三个都是文件路径,用文件对话框选)。编辑已有连接时密码框为空:留空表示不改,有专门的方式清空密码。TLS 下 CA 可选(v1 前端强制要求填,是 bug)。
- **P-03 连接列表**:侧栏,按 ordinal 升序,同序号时更新时间新的在前。右键 Edit、Delete(确认)。侧栏可折叠,折叠状态持久化。v1 菜单里的 Export、Import 是空壳,v2.0 不放这两项。
- **P-04 多 Tab**:每个连接一个可关闭的 Tab,"+" 新建连接。Tab 内有三个子页:Keys、Pub/Sub、Console。连接中、超时期间也能立即关 Tab(先移除 Tab,后台关连接)。
- **P-05 切库**:下拉框,库数量来自 `CONFIG GET databases`(失败回落 16),每个库显示 key 数(来自 `INFO keyspace`)。切库写回连接配置的 `selected_db`。
- **P-06 自动重连**:Redis 重启后能自动恢复,界面不会永久转圈。
- **P-07 连接失败**:显示错误框,可打开消息日志。

## 2. Key 浏览

- **P-10 搜索**:输入 pattern(支持 `*`),Scan 按钮或回车。pattern 为 `*` 时 SCAN 不带 MATCH。
- **P-11 三档扫描**:
  - 搜索:先扫一批(设置的"单批数量")。结果为空但游标没归零就继续扫。然后自动续扫,直到已加载数量达到"自动续扫上限"(0 表示关闭自动续扫)。
  - More:手动再扫一批。
  - All:一直扫到游标归零,不设上限。
  - 搜索和 All 可以中途 Stop,停止后游标保留,还能 More。
- **P-12 去重**:SCAN 会重复返回 key,要去重。
- **P-13 列表**:Key 列默认升序;列头可以在已加载的数据里过滤(不区分大小写的子串)。底部显示已加载数/总数(悬停看游标)、已加载百分比(只在 pattern 为空或 `*` 时显示),还有没加载完的提示。**差别**:虚拟滚动,不分页。
- **P-14 key 详情**:类型、长度、TTL 三列。设置"显示 key 详情"打开时,可见的行自动加载;每行可以手动刷新。长度按类型用 STRLEN/HLEN/LLEN/SCARD/ZCARD/XLEN。TTL 为 -2(已不存在)的行标记为已删除。
- **P-15 类型标签**:String 绿、Hash 蓝、List 紫、Set 青、ZSet 橙、Stream 洋红,其他显示 None。
- **P-16 可打开的类型**:String、Hash、List、Set、ZSet。Stream 和模块类型(ReJSON 等)只显示,不能打开。

## 3. 查看和编辑值

- **P-20 新建 key**:工具栏 New 下拉,选 String/Hash/List/Set/ZSet。key 已存在时报错,不覆盖。
- **P-21 String**:编辑器(代码编辑器,带语言选择和自动检测),可以改 key 名、改值(SET)、改 TTL。Save(有改动才可点)、Undo、Cancel。Format 和 Minify:**差别**,只支持 JSON。
- **P-22 成员表**(Hash/List/Set/ZSet):全屏弹窗,加载方式和 key 列表一样有自动续扫、More、All、进度。List 用 LRANGE 按页取,其他用 HSCAN/SSCAN/ZSCAN。两列都能过滤和排序;ZSet 默认按分数降序。标题栏:改 key 名、TTL、新增成员、删除选中成员。点成员打开编辑器。
- **P-23 Hash**:改值 HSET;改 field 名:先 HEXISTS 防冲突,再在事务里 HDEL 加 HSET;删除 HDEL。
- **P-24 List**:新增时选位置,头插 LPUSH 或尾插 RPUSH;改值 LSET;读单个 LINDEX;按下标删除:在事务里把目标下标 LSET 成哨兵值 `TO_BE_DELETED-<ULID>`,再 `LREM key 0 哨兵`。
- **P-25 Set**:SADD 时成员已存在报错;改名:先 SISMEMBER 防冲突,再在事务里 SREM 加 SADD;删除 SREM。
- **P-26 ZSet**:分数和成员名都能改;改名先检查冲突,保留分数,在事务里 ZREM 加 ZADD;改分数 ZADD;删除 ZREM。
- **P-27 改 key 名**:**差别**,用 RENAMENX。目标已存在时报错。
- **P-28 TTL**:单位秒,范围 -1 到 2147483647。-1 执行 PERSIST;**0 拒绝**(EXPIRE 0 会删除 key);正数 EXPIRE。
- **P-29 二进制**:含无法解码字节的值和名字有损显示,编辑器只读并提示。**差别**:可以切十六进制视图;二进制名字的成员可以删除。
- **P-30 大值**:**差别**,超过 8 MiB 的 String 只读前 1 MiB,只读并提示。

## 4. 删除、复制、粘贴

- **P-40 批量删除 key**:多选后 Delete,确认,DEL 后用 DBSIZE 刷新 key 数。**差别**:全选是当前过滤后已加载的全部行,分批 DEL。
- **P-41 复制**:选中的 key 逐个 DUMP 加 PTTL,编码成 `REDIS:` + base64(zlib(JSON `[{id, value: 字节的数字数组, ttl_ms}]`)),写进系统剪贴板。已过期或不存在的跳过并汇总提示,一个都没复制成才算失败。DUMP 字节累计超过 64 MiB 立即中止并提示。**格式必须和 v1 兼容**(v1 复制的能在 v2 粘贴,反之亦然),要有用 v1 实际产出的样本做的测试。
- **P-42 粘贴**:校验前缀;zlib 解压封顶 256 MiB(防解压炸弹);不覆盖模式下先检查所有 key 是否存在,有冲突整体拒绝并列出;然后逐个 `RESTORE key ttl_ms value [REPLACE]`。完成后刷新 key 数并重新搜索。**差别**:确认框有"覆盖同名 key"选项,默认不勾。

## 5. 控制台和 Pub/Sub

- **P-50 控制台**:提示符 `> `。分词按 redis-cli 规则:单双引号,`\xHH \n \r \t \b \a` 转义;引号没闭合或闭合引号后紧跟非空白都报错不发送。`clear` 是本地命令。历史最多 100 条,连续重复的不记录,↑↓ 浏览。光标按单词移动和删除,Ctrl+C 放弃当前行。命令超过 500ms 未返回显示等待动画。输出最多保留 10000 行。输出可以选中复制。
- **P-51 控制台后端**:用独立的控制台客户端。拒绝 SUBSCRIBE、PSUBSCRIBE、SSUBSCRIBE、UNSUBSCRIBE、PUNSUBSCRIBE、SUNSUBSCRIBE、MONITOR、RESET(不区分大小写)。阻塞命令客户端侧 30 秒超时:BLPOP、BRPOP、BLMOVE、BRPOPLPUSH、BLMPOP、BZPOPMIN、BZPOPMAX、BZMPOP、WAIT、WAITAOF,以及参数里带 BLOCK 的。输出模仿 redis-cli(**差别**:RESP3 Map 也要格式化)。
- **P-52 Pub/Sub**:输入频道名 Subscribe/Unsubscribe,同时只订阅一个频道;Publish(多行文本);Channels(PUBSUB CHANNELS);Clear。消息带时间戳显示。**差别**:二进制消息有损显示。

## 6. 设置和其他

- **P-60 设置**:单批数量 100–2000(步长 100,默认 500);自动续扫上限 0–100000(步长 1000,默认 100000,0 为关闭);显示 key 详情(默认开)。**差别**:没有每页条数。
- **P-61 窗口**:标题 `xRedis v{版本}`;最小 1024×840,默认 1600×903;大小和最大化状态持久化(防抖 500ms)。
- **P-62 免责协议**:首次启动必须同意,拒绝就退出;另有隐私政策。文案照 v1 `src/views/disclaimer.tsx` 和隐私政策弹窗。
- **P-63 Help 菜单**:外链照 v1 `src/layouts/topMenus.tsx`。
- **P-64 消息栏**:底部显示最新消息,保留最近 50 条,点开可以翻看;同时有 toast 提示。
- **P-65 日志**:文件日志按天轮转保留 7 个;开发版 debug 级,发布版 info 级。
- **P-66 启动容错**:配置加载失败时提示,用默认配置继续运行,不崩溃。
- **P-67 界面语言**:英文。

## 7. 必须带过来的经验(每条都要有测试)

| 编号 | 坑 | 要求 |
|---|---|---|
| L-01 | fred 默认不重连也不超时,Redis 一重启界面永久转圈 | 重连、命令超时、半开探测三层都配 |
| L-02 | 关 Tab 卡 10–20 秒 | 关闭纪元;建连中也能立刻关;没连好之前不发 scan |
| L-03 | 切库竞态:并发请求把连接池固定在旧库,且不会自愈 | 持锁,先写配置再清缓存 |
| L-04 | 并发建池、建客户端造成泄漏 | 双检查加锁,并发下每种客户端只建一个 |
| L-05 | Pub/Sub 消息重复投递 | 消息监听只在建客户端时注册一次 |
| L-06 | 退订误关控制台客户端 | 订阅客户端和控制台客户端分开管理 |
| L-07 | 控制台参数被按空格拆碎 | 自己分词,参数原样传给 Redis(含空格、JSON) |
| L-08 | EXPIRE 0 删除 key | 拒绝 0 |
| L-09 | 粘贴静默覆盖、丢 TTL | 不覆盖模式检查冲突;保留 TTL |
| L-10 | INFO keyspace 解析越界 panic、伪造 db0 | 解析健壮;忽略不认识的字段(Redis 8 的 subexpiry) |
| L-11 | 托管 Redis 禁用 CONFIG | 回落 16 个库 |
| L-12 | TLS 私钥格式 | 按 PEM 标签分派;CA 可选 |
| L-13 | 剪贴板可能暂时打不开(Windows) | 失败要提示,不能静默 |
| L-14 | 复制太大、粘贴解压炸弹 | 64 MiB 和 256 MiB 上限 |
| L-15 | LRANGE 曾经静默丢掉二进制元素,后面的下标错位 | 全程用字节,下标不能错 |
| L-16 | 用 -1 或 0 推断"是否已加载" | 显式的已加载标志 |
| L-17 | 配置存储并发写丢数据 | JSON 整份原子写,失败回滚 |

## 8. 常量

| 名字 | 值 |
|---|---|
| `POOL_SIZE`(也是 key 详情并发上限) | 12 |
| `DEFAULT_DB_COUNT` | 16 |
| 重连退避 | 100ms 起,倍数 2,上限 10000ms,不限次数 |
| 命令超时 / 建连超时 | 15000ms / 10000ms |
| 半开探测间隔 / 超时 | 2000ms / 8000ms |
| `COPY_MAX_TOTAL_BYTES` | 64 MiB |
| `PASTE_MAX_DECOMPRESSED_BYTES` | 256 MiB |
| 剪贴板前缀 | `REDIS:` |
| zlib 压缩级别 | fast |
| 控制台阻塞命令超时 | 30s |
| 控制台历史 / 输出行数 | 100 / 10000 |
| 控制台等待动画阈值 | 500ms |
| 消息栏保留条数 | 50 |
| 窗口最小 / 默认 | 1024×840 / 1600×903 |
| 窗口尺寸保存防抖 | 500ms |
| TTL、List 下标输入上限 | 2147483647 |
| 批量删除每批 | 500(v2 新增) |
| 大值阈值 / 预览长度 | 8 MiB / 1 MiB(v2 新增) |

## 9. v1 的测试(移植参考)

v1 的 Rust 测试在 `src-tauri/src/tests/`(`pure.rs`、`config_store.rs`、`integration.rs`、`pubsub.rs`、`tls.rs`),前端 VM 测试在 `src/vms/__tests__`、`src/utils/__tests__`,GUI e2e 在 `e2e/`(`smoke.mjs`、`crud.mjs`)。v2 的 core 测试覆盖 v1 Rust 测试的全部场景;app 测试覆盖 v1 VM 测试的场景;e2e 的场景写进 R-12 的验收清单。
