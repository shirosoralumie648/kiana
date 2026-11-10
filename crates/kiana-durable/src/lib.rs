//! 对应 packages/durable。
//! TS 源：packages/durable/src/**
//!
//! 依赖方向与 TS 的 `package.json` 一致：`chord` / `kiana-ai` / `codemode` / `telemetry`，**不**依赖
//! `packages/agent`（TS 的 durable 只在一处注释里提到 agent loop，没有 import）。
//!
//! # 这个 crate 解决什么问题
//!
//! `kiana-durable` 是 agent 的持久化层与会话运行时：它定义文档、条目、任务、提交记录等数据
//! 形状与 JSON 编码，提供可替换的 [`crate::types::Storage`] 后端，并在其上组装两个门面：
//!
//! - [`crate::session::SessionImpl`]：把一次变更拆成“回调在事务里改文档 → Storage 原子持久化
//!   → 同步发布”，提交与关闭在同一串行变更线上排队；
//! - [`crate::harness::harness::HarnessImpl`]：在 Session 之上加入扩展 registry、任务调度、提交
//!   接纳与会话视图，驱动 agent 的模型/工具轮次并把这些状态持久化；同模块的
//!   [`crate::harness::harness::Harness`] 只是它的值面入口（`Harness::open()` 返回
//!   `Arc<HarnessImpl>`）。
//!
//! 没有这一层，上层就要自己维护“内存状态与磁盘记录如何对齐、并发提交如何串行、进程重启后
//! 如何恢复未完成任务”等不变量。
//!
//! # 在系统中的位置
//!
//! - 上游（本 crate 依赖）：`kiana-ai`（消息与模型类型）、`kiana-chord`（`Context` 与文档
//!   delta；chord 的 watch 仍是 TODO 骨架，本 crate 的 `types::WatchHandle` 是自有契约等价物，
//!   见 `types.rs` 的模块文档）、`kiana-codemode`、`kiana-telemetry`、`kiana-json`；完整清单见
//!   `Cargo.toml`。
//! - 下游（本仓库内）：声明依赖本 crate 的只有 `kiana-env-client`，它复用
//!   [`crate::env::ExecutionEnv`] 契约与 `RangeDecoder`；其余使用点都在本 crate 的
//!   examples/tests 里。
//! - 对应 TS：`packages/durable`；一个 `.rs` 对应一个 `.ts` 的文件映射见 `ARCHITECTURE.md`
//!   的“`kiana-durable`（阶段 1，84 文件）”清单。
//!
//! # 核心概念与类型
//!
//! - **类型化 ID 与记录**：[`crate::types::ConversationId`]、`TaskId`、`EntryId` 等是带
//!   phantom 品牌（编译期区分命名空间）的 `i64`；[`crate::types::Tx`]、
//!   [`crate::types::Storage`]、[`crate::types::Session`] 是行为契约。字段名、serde 属性与
//!   声明顺序就是落盘/线格式，重排字段或改属性会直接改变字节（见 `types.rs` 的模块文档）。
//! - **文档（document）**：带定义（`DocDefinition`）和稳定地址的 JSON 值。事务里先改内存
//!   草稿，结算时用 chord 的 `diff_revisions()` 与载入值做 diff，再把 op 随写入批次持久化。
//! - **变更线（line）**：Session 内部的 FIFO 票据队列；`commit_with` 在**调用时**取票，因此
//!   并发提交按调用顺序被接纳；需要一致读的操作经 `read_on_line` 与提交串行。宿主提交
//!   监听器只在批次持久化成功后运行。
//! - **Storage**：原子写入批次与查询/扫描契约。[`crate::storage::MemoryStorage`] 是参考语义
//!   实现，[`crate::storage::jsonl::JsonlStorage`] 与
//!   [`crate::storage::sqlite::SqliteStorage`] 是持久化后端，
//!   [`crate::storage::indexeddb::IndexedDbStorage`] 与 Cloudflare 后端是恒失败的占位。
//! - **env**：[`crate::env::ExecutionEnv`] 把文件系统与 shell 抽象成 trait；
//!   [`crate::env::node::NodeExecutionEnv`] 是本地实现，远程实现由 `kiana-env-client` 提供，
//!   工具代码只依赖 trait。
//! - **Harness**：registry 里的扩展提供工具、hook 与任务定义；调度器按存储中的任务记录推进，
//!   任务、提交与文档最终都经同一个 [`crate::types::Storage`] 提交路径落盘。
//!
//! # 一次典型调用
//!
//! 只用 Session（`examples/00_conversation.rs`）：
//! 1. 选后端并包成 `Arc<dyn Storage>`，如 `MemoryStorage::new()`；
//! 2. `SessionImpl::new(storage, SessionOptions::default())`；
//! 3. `commit_with(|tx| ...)` 在事务回调里创建会话、追加条目或改文档；回调成功后整批写入由
//!    Storage 原子提交，回调失败则整批丢弃；
//! 4. `close(context).await` 封闭接纳、结清在途提交并关闭后端。
//!
//! 使用 Harness（`examples/05_harness.rs`）：
//! 1. 用 `create_registry_impl()` 准备含内建任务的 registry，再
//!    `Harness::open(storage, options, context)`；打开成功只表示任务已载入并调和，调度尚未
//!    开始；
//! 2. `harness.root(..)` / `create_conversation(..)` 取得 `Conversation` 句柄，通过它
//!    `submit()` 输入、读取上下文与转录；
//! 3. 调用 `resume()` 启用调度（提交输入也会隐式触发），之后由调度器推进模型与工具任务。
//!
//! # 边界与失败模式
//!
//! - 可失败入口普遍使用 [`crate::types::BoxError`]（`Box<dyn Error + Send + Sync>`）；异步入口
//!   普遍接收 `&dyn Context`（或 `ContextSource`），上下文已取消时提前返回中止错误。
//! - 事务回调返回错误不会留下持久副作用：写入在回调成功后才由 `Storage::commit` 落盘
//!   （见 `session/transaction.rs` 的模块文档）。
//! - Session 的失败闩锁只保留第一个错误：Storage 提交或内部观察者失败后，后续入口返回
//!   `SessionFailed` 并开始关闭；没有失败而关闭开始时，入口返回 `SessionClosed`
//!   （见 `session/session.rs` 的 `assert_usable`）。
//! - 后端能力缺失是显式错误：IndexedDB 与 Cloudflare 的打开入口恒返回 not-supported，不做
//!   静默降级或模拟（见对应模块文档）。
//! - 少数入口按 TS 语义 panic（如 `env::get_or_throw`、事务封闭后无错误通道的
//!   `settle_submission`），调用前需确认所处阶段。
//!
//! # 编译边界与 feature
//!
//! `default = []`。`sqlite` 编译 `storage::sqlite::node` 的本地 rusqlite 驱动；`images` 启用
//! 原生 Photon 等价实现；`examples` 仅门控 `Cargo.toml` 中声明了
//! `required-features = ["examples"]` 的示例目标，未在 `Cargo.toml` 声明的示例文件仍会被
//! Cargo 自动发现并始终构建。`cloudflare` 只改变
//! `timers::in_workers()` 的编译期返回值；`wasm` 与 `node` 在源码中没有被 `cfg` 读取
//! （`wasm` 的现状见 `storage/indexeddb.rs` 的模块文档）。完整声明见 `Cargo.toml`。
//!
//! # 阅读顺序
//!
//! 1. `types.rs`：先看 ID、记录与 `Tx`/`Storage`/`Session` trait，这里定义所有跨模块交换的
//!    形状；
//! 2. `storage/`：`MemoryStorage` 是语义基准，再按需看 JSONL/SQLite 的落盘与恢复；
//! 3. `session/`：`transaction.rs`（一次提交如何暂存与结算）→ `session.rs`（变更线、发布、
//!    关闭）；
//! 4. `harness/`：`mod.rs` 的阅读顺序清单；主链路从 `types` → `define` → `registry` →
//!    `agent` → `harness` 开始，流式部分见 `events` → `task_graph` → `view`；
//! 5. `env/`、`tools/`、`codemode/`、`tool_search/`、`search.rs`、`truncate.rs`、`timers.rs`、
//!    `images/`：工具执行与宿主能力，通常从 `env/mod.rs` 的 trait 开始；
//! 6. `testing/`：各后端与环境实现的 conformance 用例，也是行为契约的可执行说明；
//!    `index.rs` 只是 TS `index.ts` 的再导出面，没有逻辑，最后看即可。

// `async_trait` 展开会给返回 `Result` 的方法加上重复的 `#[must_use]`；`kiana-ai` 等 crate 同样豁免。
#![allow(clippy::double_must_use)]
// 回调/异步签名直接展开书写；拆成别名会掩盖契约。
#![allow(clippy::type_complexity)]

// --- 子模块 ---
// 各模块的说明写在其文件内的 `//!` 里；不要在下面的 `pub mod` 上方新增外层 `///`：rustdoc 会把
// “外层文档 + 文件内 `//!`”合并后放到父模块（这里是 crate 根）作用域解析链接，使子模块里的
// 短链接失效。
pub mod codemode;
pub mod env;
pub mod harness;
pub mod images;
pub mod index;
pub mod search;
pub mod session;
pub mod storage;
pub mod testing;
pub mod timers;
pub mod tool_search;
pub mod tools;
pub mod truncate;
pub mod types;

// `types.ts` 的公开面在 crate 根可用：`crate::documents::define_doc`、`crate::errors::*`、
// `crate::TaskId` 等。`errors`/`documents`/`entries`/`tasks` 是 `types` 里的子模块，随通配再导出。
pub use types::*;
