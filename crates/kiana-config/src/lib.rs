//! 对应 ca-config-models 单元：settings / auth / models / trust / keybindings / cache。
//!
//! TS 源：packages/coding-agent/src/core/{settings-manager,settings-schema,settings-defaults,settings-diagnostics,
//! provider-composer,model-resolver,auth-storage,cache-warmer,keybindings,keybindings-schema,key-id-schema,
//! resolve-config-value,trust-manager,model-runtime,model-registry,remote-catalog-provider,model-config,
//! models-store,http-dispatcher,usage-totals,runtime-credentials,auth-guidance,client-identity,telemetry,
//! project-trust,radius,experimental,defaults}.ts
//!
//! 说明：`core/defaults.ts` / `experimental.ts` / `radius.ts` / `telemetry.ts` 从 `kiana-core` 移到这里，
//! 理由见 `ARCHITECTURE.md` §3.3。`http-dispatcher.ts` 留在 `kiana-core`（它是 `reqwest` 客户端装配，
//! 不读 settings），`usage-totals.ts` 留在 `kiana-session-core`（它读 `SessionEntry`）。
//!
//! # 这个 crate 解决什么问题
//!
//! 它集中实现 `coding-agent` 的配置与身份能力：读取 `settings.json`、`auth.json`、
//! `trust.json`、`models.json`、`keybindings.json`，并持久化 `settings.json`、
//! `auth.json`、`trust.json` 与动态目录缓存 `models-store.json`（`models.json` 与
//! `keybindings.json` 在本 crate 内只读：前者只有 `models::config_file` 的读入口，
//! 后者只在 `keybindings` 中被读取，`keybindings.json` 的迁移写回在
//! `kiana-coding-agent` 的迁移流程里），此外还合成并解析模型目录、管理 provider
//! 凭据与运行时覆盖、做项目信任决策、展开配置值里的 `$ENV` / `!command` 模板，
//! 并在缓存过期前预热。这些能力被多个上层 crate 直接依赖（见下节），集中在本
//! crate 是为了让这些文件的读写路径不散落到各调用方各自实现——但并非"每份文件
//! 只有本 crate 一套写路径"：`kiana-coding-agent` 的启动迁移也会写 `auth.json`、
//! `settings.json` 与 `keybindings.json`。
//!
//! 这些文件都是外部可改的，格式与 TS 参照实现兼容；锁还需要与 Node 版工具互操作。
//! `settings.json` 的键序与“无结尾换行”、`trust.json` 的 JS 排序与“有结尾换行”、
//! `auth.json` 中未知条目的保留、锁目录布局，都有黄金测试或互操作要求。改动持久化
//! 代码前先读对应子模块的 `//!` 与 `tests/`。
//!
//! 多数有行为逻辑的子模块移植自 TS 的 `packages/coding-agent/src/core/*.ts`；各子模块
//! 的 `//!` 按需给出 TS 源路径、`spec/ca-config-models.md` 的对应章节与已知差异——
//! 不逐文件保证三项都有（`index` 与各级纯索引 `mod.rs` 没有 TS 对应，部分叶子模块
//! 也无 spec 章节或差异小节）。本文只给出 crate 的地图，具体行为以子模块文档为准。
//!
//! # 在系统中的位置
//!
//! 本 crate 位于依赖分层 L5（`ARCHITECTURE.md` §2.1），只依赖更低层，不依赖任何前端
//! crate，因此可以被所有前端共用。
//!
//! - 上游（`Cargo.toml` 中的依赖）：`kiana-ai`（模型/provider 类型与内置目录）、
//!   `kiana-core`（路径、配置目录、JSON/文本基础、HTTP dispatcher）、
//!   `kiana-session-core`（`virtual_models` 的实现）、`kiana-agent-core`
//!   （`ThinkingLevel`）、`kiana-tui`（键位管理的底层实现）、`kiana-json`
//!   （JS 的序列化与解析语义）。
//! - 下游（`Cargo.toml` 中直接依赖本 crate 的 crate）：`kiana-theme`、`kiana-ext-api`、
//!   `kiana-tools`、`kiana-extensions`、`kiana-builtin-extensions`、`kiana-resources`、
//!   `kiana-session`、`kiana-interactive`、`kiana-rpc-print`、`kiana-coding-agent`。
//!
//! 公开面按模块保留命名空间（为什么不能全部摊平见 `index`）：调用方通常写
//! `kiana_config::settings::SettingsManager`、`kiana_config::models::runtime::ModelRuntime`
//! 这样的路径。只有 `auth_guidance`、`client_identity`、`defaults`、
//! `resolve_config_value` 四个叶子模块同时把符号提到 crate 根。
//!
//! # 核心模块与类型
//!
//! - `settings`：`SettingsManager` 持有 global（`<agent-dir>/settings.json`）与 project
//!   （`<cwd>/.kiana/settings.json`）两个作用域的内存快照，getter 同步读，setter 进入
//!   异步串行写队列；`schema` 定义 `Settings` 与宽松解析，`storage` 负责文件锁与读写，
//!   `migrations` 负责旧格式迁移，`diagnostics` 汇总加载问题，`tools` 与 `compaction`
//!   是派生设置。
//! - `models`：`runtime::ModelRuntime` 是组合层（内置 provider + models.json + 扩展注册
//!   + 虚拟模型），并实现 `kiana_ai::models::Models`；`resolver` 负责 `provider/model`
//!   引用、scope 与模糊匹配；`composer` 实现 provider 的四层合成；`config_file` 解析
//!   JSONC 格式的 models.json；`remote_catalog` 与 `catalog_http` 负责远端目录刷新；
//!   `registry::ModelRegistry` 是暴露给扩展的同步门面；`virtual_models` 只再导出
//!   `kiana-session-core` 的实现。
//! - `auth`：`storage::AuthStorage` 等类型实现 `CredentialStore`（`auth.json` 加
//!   `proper-lockfile` 目录锁）；`runtime_credentials::RuntimeCredentials` 是非持久化的
//!   运行时 API key 覆盖层；`models_store::FileModelsStore` 持久化动态模型目录；
//!   `lock` 是可与 Node 版互操作的目录锁。
//! - `trust`：`manager::ProjectTrustStore` 读写 `trust.json`；`project_trust` 定义
//!   “是否信任当前项目”的判定顺序，以及 UI/扩展钩子的接入点。
//! - `keybindings`：默认键位表、旧键名迁移与 `KeybindingsManager`（在 `kiana-tui`
//!   的实现之上叠加 `keybindings.json`）。
//! - `cache`：缓存预热器。注意 `cache` 模块没有再导出 `warmer` 的条目，路径必须写成
//!   `kiana_config::cache::warmer::CacheWarmer`。
//! - `resolve_config_value`：`$NAME` / `${NAME}` / `$$` / `$!` 模板与 `!command` 执行。
//! - `client_identity`、`defaults`、`auth_guidance`：无状态助手，分别决定客户端身份
//!   内容（如是否声明归因）、默认值，以及用户可见的登录提示文案。
//!
//! # 一次典型调用（启动路径）
//!
//! 顺序取自 `kiana-coding-agent::app::run_main` 与
//! `kiana-session::agent_session_services::create_agent_session_services`：
//!
//! 1. 启动早期先创建 **bootstrap** 的 `SettingsManager`，此时项目尚未被信任，构造选项
//!    为 `project_trusted: Some(false)`；它只用于读取 HTTP 代理等启动早期设置。
//! 2. 执行迁移后创建启动用的 `SettingsManager::create(cwd, agent_dir)`，并用
//!    `collect_settings_diagnostics` 把加载问题转成启动诊断（Warning）。
//! 3. 创建 `ProjectTrustStore::new(agent_dir)`；项目资源是否加载由它和 `project_trust`
//!    的判定顺序决定。
//! 4. 装配运行期服务：`ModelRuntime::create` 选择凭证来源——未显式注入 credentials
//!    时按 `auth_path` 打开 `AuthStorage::create(path)`（启动路径传入
//!    `<agent-dir>/auth.json`），只有 `auth_path` 也缺省时才回落到
//!    `AuthStorage::create_default()`——此外加载 models.json、选择 `ModelsStore`，
//!    并为内置 provider 包装远端目录（radius 保持原样，OpenRouter 走个性化目录）；
//!    随后 `resolver` 根据 CLI 参数与设置里的默认模型选出模型。
//! 5. 运行期：设置 getter 同步读快照；setter 同步更新内存快照并标记本会话修改过的
//!    字段后入队持久化——在 tokio 运行时内由后台 worker 串行写盘，没有运行时的同步
//!    上下文则由调用线程就地排空队列完成同步写盘（见 `settings::storage` 的 `//!`）；
//!    模型目录刷新与凭据操作经 `ModelRuntime` 暴露；`CacheWarmer` 由
//!    `kiana-session::sdk` 装配并在请求路径上调用 `start`（见 `cache` 模块文档）。
//!
//! # 边界与失败模式
//!
//! - **设置写盘不向调用方抛错**：加载与写盘失败进入错误队列，由
//!   `SettingsManager::drain_errors` 取走；`reload` 失败时保留该作用域上一次的值。
//!   同步返回 `Result` 的是非法设置值（getter / 嵌套 setter 的 `SettingsValueError`）
//!   与项目未受信时写项目设置（`PROJECT_NOT_TRUSTED_WRITE_MESSAGE`）；文件 I/O 与解析
//!   失败不走同步返回。
//! - **models.json 损坏不阻断启动**：首次刷新失败会让 `ModelRuntime::create` 返回
//!   `Err`，但 models.json 的读取/解析错误不会；它保存在 `ModelConfig` 中，由
//!   `ModelRuntime::get_error` 暴露。
//! - **外部可改文件按宽松读、原样写**：`auth.json` 中本实现无法表达的条目读取时投影为
//!   `None`，写回时仍保留其原始 JSON；`models-store.json` 同样保留未知字段与键序。
//! - **配置值解析失败等价于 `undefined`**：`resolve_config_value` 在 `!command` 非零退出、
//!   超时、spawn 失败或 stdout 超过 Node 默认 `maxBuffer`（1 MiB）时返回 `None`；需要区分
//!   错误时用 `resolve_config_value_or_throw` / `resolve_config_value_or_error`。
//! - **测试位置**：有行为逻辑的子模块均有对应测试（如 `settings_manager.rs`、
//!   `trust_manager.rs`、`auth_storage.rs`），`index` 等纯索引/再导出模块没有也不会
//!   单独编写测试。`settings_manager.rs` 有直接比较文件原始文本的用例（2 空格缩进、
//!   键序、无结尾换行），`trust_manager.rs` 断言 `trust.json` 的排序与结尾换行，
//!   `settings_manager_bug.rs` 锁定“只覆盖本会话修改过的字段、保留外部编辑”。
//!
//! # 阅读顺序
//!
//! 1. `defaults` → `settings::{schema, storage, mod}`：先看配置的形状与持久化。
//! 2. `models::{config_file, composer, runtime, resolver}`：模型如何被发现、合成与选择。
//! 3. `models::{remote_catalog, catalog_http, registry, virtual_models}`：目录刷新与门面。
//! 4. `auth::{lock, storage, runtime_credentials, models_store}` 与
//!    `trust::{manager, project_trust}`。
//! 5. `keybindings`、`cache::warmer`、`resolve_config_value`、`client_identity`、
//!    `auth_guidance` 等独立能力。
//! 6. 最后看 `index`：它列出 crate 的公开再导出面与维护规则。
pub mod auth;
pub mod auth_guidance;
pub mod cache;
pub mod client_identity;
pub mod defaults;
pub mod index;
pub mod keybindings;
pub mod models;
pub mod resolve_config_value;
pub mod settings;
pub mod trust;

pub use index::*;
