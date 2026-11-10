# 01 · 架构

> 架构师 2026-11-10。v2 是用原生 Rust GUI(GPUI)重写的 Redis 桌面客户端,v2.0 的目标是**追平 v1 的功能**(清单见 `02-v1-parity.md`),不加新功能。

## 目标

- **资源占用低**:不用 webview。验收时量空闲内存和安装包体积,和 v1 对比(R-12)。
- **换 GUI 容易**:业务逻辑和界面状态都在与 GUI 无关的层里,GUI 层只负责画和转发操作。
- **全平台**:Windows、macOS、Linux。代码里不许出现没加 `cfg` 的单平台 API。目前只有 Windows 施工者,其他两个平台的编译和试用等作者安排(见 `09-progress.md`)。
- **把 v1 修过的坑带过来**:`02-v1-parity.md`「必须带过来的经验」每条都要有测试。

## 分层

```
xredis-gpui   视图层:GPUI 窗口和控件。只画 App 的状态,把用户操作变成 Msg。执行平台能力(剪贴板、文件对话框、开网址、退出)。
     │  依赖
xredis-app    视图模型层:App 状态、Msg、Cmd、update、Runtime。与 GUI 无关,全部可单测。
     │  依赖
xredis-core   领域层:配置、密钥、Redis 连接和命令、复制粘贴编码、控制台分词和输出格式。与 GUI 无关。
```

Cargo workspace:

```
Cargo.toml                 workspace;edition 2024;publish = false;profile 照 xPass(见下)
crates/xredis-core/
crates/xredis-app/
crates/xredis-gpui/        二进制名 xredis
scripts/test-redis.sh      起测试用 Redis 容器并跑需要 Redis 的测试
docs/
```

依赖规则:

- `xredis-core`、`xredis-app` **不许依赖 gpui、gpui-kit**,也不许依赖任何窗口、剪贴板库。
- `xredis-gpui` 只通过 `gpui-kit = "=0.9.1"` 引入 GPUI 和组件库(它钉死了配套的 gpui-pre)。官方 `gpui` 在 crates.io 上停在 0.2.2,和 gpui-kit 不兼容,不要混用。
- 主要依赖:`fred`(Redis,10.x,开 rustls;v1 用它踩平了重连和超时的坑)、`tokio`、`keyring`(系统钥匙串)、`serde`/`serde_json`、`flate2`、`base64`、`ulid`、`thiserror`、`tracing` + `tracing-appender`、`dirs`。新加依赖在提交记录里说明理由。

`[profile.dev]` 照 xPass:`incremental = false`、`debug = "line-tables-only"`,`[profile.dev.package."*"] debug = false`。`[profile.release]`:`lto = true`、`codegen-units = 1`、`strip = true`(`opt-level` 先用 3,R-12 量完再定要不要改成 `"s"`)。

## xredis-core

### 数据都用字节

Redis 的 key、field、member、value 都是二进制。**core 里一律用 `bytes::Bytes`**,不在 core 里做 UTF-8 解码。怎么显示(文本、有损文本、十六进制)由 app 层决定。这样二进制名字的 key 和成员也能正常删除、改 TTL(v1 做不到)。

### 模块

| 模块 | 内容 |
|---|---|
| `error` | 统一的 `Error`(thiserror),区分:连接失败、认证失败、超时、Redis 返回的错误、业务规则(key 已存在、EXPIRE 0 等)、配置、体量超限。替代 v1 的约 40 处错误样板 |
| `config` | `ConfigStore`:设置和连接列表,JSON 原子写,缺字段补默认值,坏文件改名留存。v1 导入。见下 |
| `keystore` | `trait KeyStore`(按连接 id 存取删密码);`KeyringStore`(系统钥匙串)和 `MemoryStore`(测试用,也是钥匙串不可用时的降级) |
| `conn` | `ConnManager`:每个连接的连接池、订阅客户端、控制台客户端;建连串行化和关闭纪元;切库;TLS |
| `keys` | 通用的游标扫描(SCAN/HSCAN/SSCAN/ZSCAN 共用一份实现)、key 详情、删除、改名、TTL |
| `types` | String、Hash、List、Set、ZSet 的读写,改名类操作用事务 |
| `transfer` | 复制粘贴的编码解码(兼容 v1 的剪贴板格式)、体量上限、DUMP/RESTORE |
| `console` | 分词(redis-cli 的 sdssplitargs 规则)、拒绝切换模式的命令、阻塞命令超时、redis-cli 风格的输出格式(含 RESP3 Map) |
| `pubsub` | 订阅、退订、发布、`PUBSUB CHANNELS`;消息用 channel 推出去 |
| `consts` | 所有硬编码的限制,值见 `02-v1-parity.md` 第 6 节 |

### 主要类型(示意,细节由施工者定)

```rust
pub struct ConnId(Ulid);
pub struct ConnectionConfig {          // 不含密码,密码在 KeyStore
    pub id: ConnId, pub name: String, pub host: String, pub port: u16,
    pub ordinal: i32, pub selected_db: u32, pub username: Option<String>,
    pub tls: Option<TlsConfig>, pub created_on_utc: i64, pub updated_on_utc: i64,
}
pub struct TlsConfig { pub ca_cert: Option<PathBuf>, pub client_cert: Option<PathBuf>, pub client_key: Option<PathBuf> }
pub enum PasswordChange { Keep, Clear, Set(String) }   // 对应 v1"留空表示不改、传空串表示清空"

pub enum KeyType { String, Hash, List, Set, ZSet, Stream, None, Other(String) }
pub enum Ttl { Persistent, Missing, Seconds(i64) }      // -1、-2、正数,不用裸 i64
pub struct KeyDetails { pub kind: KeyType, pub length: Option<u64>, pub ttl: Ttl }
pub struct ScanPage<T> { pub items: Vec<T>, pub cursor: u64 }
```

### 配置和密钥

- 位置:`dirs::data_local_dir()/xRedis/v2/config.json`(Windows 是 `%LOCALAPPDATA%\xRedis\v2\`),日志在同目录的 `logs/`,按天轮转,保留 7 个。**不和 v1 共用文件。**
- 写法照 v1:内存全量缓存;每次写整份落盘,先写 `.tmp` 再替换;落盘失败回滚内存;解析失败的文件改名为 `.broken` 留存,然后用默认配置继续运行。
- **密码存系统钥匙串**(service `xRedis`,account 是连接 id),配置文件里不存密码,任何形式都不存。钥匙串不可用时(比如 Linux 上没有可用的钥匙串服务)降级为只在内存里保存到程序退出,并在消息栏提示一次。
- **v1 导入**:第一次启动时如果 v2 的配置不存在、v1 的 `%LOCALAPPDATA%\xRedis\config.json` 存在(其他平台按 v1 的 `dirs::data_local_dir()/xRedis/config.json`),就**只读**导入:连接(保留 id)、设置、窗口尺寸;密码用 v1 写死的密钥解密(见 v1 `src-tauri/src/handlers/settings.rs`)后存进钥匙串。解密失败的密码跳过并提示。v2 配置里记下 `imported_from_v1` 时间,以后不再导入。**绝不写 v1 的文件。**

### 连接管理(照 v1,这是踩坑最多的地方)

- 每个连接三份客户端:连接池(大小 `POOL_SIZE = 12`)用于普通命令;订阅客户端;控制台客户端(也用于 `PUBSUB CHANNELS`)。三者分开管理,退订只关订阅客户端。
- 每个连接一个槽:一把 tokio Mutex 加一个关闭纪元(watch channel)。建池、切库、关闭都持锁串行;拿锁后再查一次缓存(双检查)。建连期间纪元变了就放弃,没建完的交给后台,连上了立刻 quit。
- 重连和超时三层都要配:指数退避重连(100ms 起,倍数 2,上限 10s,不限次数)、命令超时 15s、建连超时 10s、半开连接探测(间隔 2s,超时 8s)。
- 切库:持锁,先写配置的 `selected_db`,再关掉该连接的所有客户端,下次用时重建。
- TLS:rustls。CA 可选,不填用系统根证书(rustls-native-certs);私钥按 PEM 标签分派 PKCS#8、PKCS#1、SEC1;加密的私钥报错并提示用 openssl 解密。
- 连接信息:`CONFIG GET databases` 失败时回落 16 个库;`INFO keyspace` 的解析不 panic、不伪造 db0、忽略不认识的字段。

### 运行时

core 的异步函数跑在 tokio 上(fred 需要 tokio)。**tokio 运行时由 app 层的 `Runtime` 创建和持有**(多线程,后台线程),GPUI 用自己的执行器,两者只通过 channel 交互。

## xredis-app

### 模式:状态 + 消息 + 命令

```rust
pub struct App { /* 连接列表、Tab、设置、消息栏、弹窗状态…… */ }

pub enum Msg {        // 用户操作和异步结果都是 Msg
    // 例:ConnectClicked(ConnId)、ScanRequested { tab, pattern }、ScanBatch { tab, gen, page }、
    //     StopClicked(tab)、KeyDetailsLoaded { tab, key, gen, result } ……
}

pub enum Cmd {
    Backend(BackendCmd),     // 异步的 Redis 或配置操作,由 Runtime 执行,结果变成 Msg 回来
    Platform(PlatformCmd),   // 剪贴板、文件对话框、开网址、退出、保存窗口尺寸等,由 GUI 层执行
}

impl App {
    pub fn update(&mut self, msg: Msg) -> Vec<Cmd>;   // 同步、纯逻辑,单测的主要对象
}
```

- `trait Backend`:app 访问 core 的唯一入口(连接、命令、配置、密钥),要能做成 `Arc<dyn Backend>`(用 `async-trait` 或返回 `BoxFuture`)。生产实现包 core;测试实现是内存假数据,可以注入延迟和错误。
- `Runtime`:持有 tokio 运行时、`Arc<dyn Backend>` 和 `async_channel::Sender<Msg>`。`spawn(BackendCmd)` 执行后把结果发回 Msg。会持续出结果的任务(扫到底、订阅)每批发一个 Msg。
- **取消**:~~可停止的任务带 `CancellationToken`~~。改为(架构师 2026-11-10,第 24 节):长任务拆成"一批一条命令",每批的结果回来后由 App 决定要不要发下一批,Stop 就是不再发下一批;已经在路上的那一批结果照常收下(或按代号丢弃)。不需要取消令牌。
- **过期结果**:会被新请求覆盖的结果(扫描、key 详情、成员列表)带代号 `gen`,App 只收当前代号的。
- **并发上限**:key 详情同时最多 `POOL_SIZE` 个请求,同一个 key 的请求合并。和连接池大小用同一个常量。
- **已加载标志**:长度和 TTL 用 `Option` 或显式标志表示是否已加载,不能用 -1 或 0 推断。

### 什么放 App,什么放 GUI

放 App(都要有单测):

- 所有业务规则和判断:能不能保存、按钮能不能点、能不能打开某个类型、输入范围、确认提示的内容、排序规则、过滤结果、扫描的三档逻辑、加载进度百分比。
- 显示用的数据:key 和值的显示文本、是否有损(含无法解码的字节)、十六进制视图的内容、类型标签的文字。
- 表单的草稿(连接编辑器、设置、TTL、新建 key)。

放 GUI:

- 布局、颜色、字体、图标、动画。
- **大段文本的编辑缓冲**:值编辑器的文本留在 GPUI 的输入控件里;保存时才把文本作为 Msg 交给 App。App 保存原值,用于判断有没有改动和撤销。
- 平台能力的执行(`PlatformCmd`)。

判断方法:换成另一个 GUI 库时要重写的只能是 `xredis-gpui`;如果某段逻辑换 GUI 时还得照抄一遍,它就该在 App 里。

## 多语言(作者 2026-11-10)

界面文字做成**语言包**,默认英文,用户可以自己加语言包。不自己造轮子,用 **`rust-i18n` 4**:gpui-component 自己就用它(同一家公司 longbridge 的库),`rust_i18n::set_locale` 一次切换我们的文字和组件库自带的文字(组件库内置 en、zh-CN、zh-HK、zh-TW、pt-BR、ru)。

- **内置英文**:`crates/xredis-app/locales/app.yml`(rust-i18n 的 `_version: 2` 格式,key 下按语言列翻译),编译时嵌入。内置包只有 `en`。
- **用户语言包**:放在配置目录的 `locales/` 下(Windows 是 `%LOCALAPPDATA%\xRedis\v2\locales\`),格式和内置包相同,一个文件可以带一种或多种语言。启动时用 rust-i18n 自带的加载函数(`rust_i18n_support::try_load_locales`,`codegen` 特性)读进 `SimpleBackend`,通过 `i18n!("locales", fallback = "en", backend = ...)` 叠加在内置包之上。我们只写把两者接起来的几十行,不写解析器。
- **缺的 key 回落英文**;语言包格式错误时跳过这个文件,在消息栏提示文件名和原因,不影响启动。
- **切换**:设置里的语言下拉框列出内置和语言包里的所有语言,默认英文,选择保存在配置里,切换后界面立即刷新。
- **翻译只有一处**:所有界面文字的 key 和英文都在 `xredis-app/locales/app.yml`;`xredis-gpui` 不建自己的语言文件,通过 `xredis-app` 提供的函数取文字(`t!` 宏展开到调用方 crate,所以由 `xredis-app` 包一层函数给 GUI 用)。
- **core 不拼给用户看的句子**:`xredis-core` 的错误带结构化数据(哪种错误、哪个字段、哪个文件、Redis 原始报错),`Display` 的英文只用于日志;给用户看的文字由 `xredis-app` 按错误类型翻译。R-02a 改成英文的文案保留作日志用,R-06 加翻译。
- **提供模板**:设置里可以"导出语言包模板"(把全部 key 和英文写成一个 yml 文件),用户照着翻。测试保证代码里用到的每个 key 在 `en` 里都有。

## xredis-gpui

- 一个 `Entity<AppModel>` 持有 `xredis_app::App` 和 `Runtime`。`dispatch(msg)` 调 `App::update`,执行返回的 Cmd,然后 `cx.notify()`。
- 启动时 `cx.spawn` 一个前台任务,循环从 `async_channel::Receiver<Msg>` 收消息并 dispatch。
- 列表一律用虚拟列表(gpui-component 的 Table 或 `uniform_list`),不分页。
- 控件优先用 gpui-component 现成的(Input、Table、Tabs、Dialog、Select、Sidebar、Notification 等),不够再自己写。

## v2.0 和 v1 的有意差别

这些是架构师定的,作者可以推翻。完整清单在 `02-v1-parity.md`。

- key 列表和成员表用虚拟滚动,不分页;"全选"选中当前过滤后已加载的全部行,确认框显示数量;批量删除按每批 500 个分批 DEL。
- 密码存系统钥匙串,不再用写死密钥的"加密"。
- 二进制值和名字:有损显示加只读,另外可以切到十六进制视图;二进制名字的 key 和成员可以删除和改 TTL(因为全程用字节)。
- 大值:STRLEN 超过 8 MiB 的 String 只用 GETRANGE 读前 1 MiB,只读显示并提示。
- 改名 key 用 `RENAMENX`(原子),不再先 EXISTS 再 RENAME。
- 值的格式化只做 JSON(美化、压缩);v1 的 XML、HTML、CSS、JS 美化依赖 prettier,v2.0 不做,这几种只做语法高亮(组件库支持多少就做多少)。
- 粘贴的确认框加"覆盖同名 key"选项,默认不覆盖(v1 后端有这个能力但界面总是覆盖)。
- Pub/Sub 的二进制消息有损显示(v1 显示成空串)。
- 控制台不做"选中即复制"和"右键粘贴"这类终端习惯,用普通的选中复制和 Ctrl+V。
- 设置项去掉每页条数(没有分页了),其他默认值以 v1 后端为准:单批 500、自动续扫上限 100000、显示 key 详情开。
- 剪贴板格式和 v1 相同(`REDIS:` 前缀),v1 和 v2 之间可以互相复制粘贴。
