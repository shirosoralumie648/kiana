//! 对应 ca-cli：启动编排、参数解析、CLI 子命令、迁移、SDK 导出面；二进制 `kiana` 与 `kiana-rpc`。
//!
//! TS 源：packages/coding-agent/src/{cli.ts,rpc-entry.ts,main.ts,migrations.ts,index.ts,package-manager-cli.ts}
//! 以及 `cli/**`、`experimental/source-resolver.ts`、`bun/**`。
//!
//! 两个二进制：
//! - `kiana`（`src/main.rs`）：等价 `cli.ts` → `setup_cli()` + `main(argv)`；
//! - `kiana-rpc`（`src/bin/kiana-rpc.rs`）：等价 `rpc-entry.ts` → 同上但强制 `--mode rpc`。
//!
//! # 公共面
//!
//! `lib.rs` 是 TS `src/index.ts` 的等价物：SDK 导出面在 [`sdk_exports`]（纯再导出），
//! 这里额外把两个二进制入口需要的符号提升到 crate 根。
//!
//! TS 源：`packages/coding-agent/src/index.ts`。
//!
//! # 这个 crate 解决什么问题
//!
//! 它是整个工作区的装配层（`ARCHITECTURE.md` §2.1 的 L13）。会话循环、工具、TUI、RPC、
//! 压缩等实现都在下层 crate；本 crate 把它们接成一个可执行程序：解析 argv 并选择运行
//! 模式、执行启动期迁移与包管理子命令、把库层的退出请求折算成进程退出码，并复刻 TS
//! `src/index.ts` 的 SDK 导出面。没有它，下层 crate 只是一组互不相连的库：没有进程入口，
//! 也没有统一的输出通道与退出码约定。
//!
//! # 在系统中的位置
//!
//! 上游（本 crate 依赖）：`Cargo.toml` 列出的 workspace crate；其中 `kiana-session`
//! 提供会话、`kiana-interactive`/`kiana-rpc-print` 提供两种前端、`kiana-extensions`
//! 提供扩展运行时。
//! 下游（使用本 crate）：本 crate 的两个二进制——`src/main.rs`（`kiana`）与
//! `src/bin/kiana-rpc.rs`（`kiana-rpc`）；`kiana-evals` 则只通过 [`sdk_exports`] 构造
//! 评测会话，不经过启动编排。
//!
//! 模块地图：
//!
//! - `app`：启动编排（`main.ts`），含迁移、模式分发与会话生命周期；
//! - `cli`：参数解析（`cli::args`）、认证子命令、启动期 UI 与共享输出基础设施；
//! - `migrations`：启动时执行的一次性迁移；
//! - `package_manager_cli`：`install`/`remove`/`update`/`list`/`config` 子命令；
//! - `sdk_exports`：对应 `index.ts` 的纯再导出面；
//! - `web`：Web 搜索（有序引擎链）与 fetch（并行、10k 截断、SSRF 防护）；
//! - `ide`：`~/.kiana/ide/<port>.lock` 发现、MCP Streamable HTTP、contextUpdate 与
//!   openDiff 往返；
//! - `cron`：无人值守 cron 的调度表达式、持久化账本与 tick 调度器；
//! - `bun`、`source_resolver`：documentation-only，只记录 TS 侧行为，没有运行期作用。
//!
//! # 核心类型与入口
//!
//! - [`main`]（`app::main`）：`main(args, Option<MainOptions>) -> i32`（异步）。`args` 是
//!   不含程序名的参数：`src/main.rs` 用 `std::env::args_os().skip(1)` 直接取得，
//!   `kiana-rpc` 在其前面插入 `--mode rpc`；返回值是进程退出码。它在正常路径上
//!   **不结束进程**；
//!   同一进程内可多次调用，每次启动会重置 `PROMPT_CONSUMED_STDIN` 这个“stdin 提示已被
//!   消费”的全局标记，因此测试可以直接断言退出码。`MainOptions::extension_factories`
//!   用于在内置扩展之后追加外部注入的扩展。
//! - [`CliExit`]：库函数表达 TS `process.exit(code)` 的方式；内部辅助函数返回
//!   `Result<T, CliExit>`，由 [`main`] 折算成 `i32`。常规路径不调用 `process::exit`，
//!   例外是下文启动期 panic 钩子。
//! - [`Output`]：按 `cli::setup` 的约定（模块内禁止 `println!`/`eprintln!`），所有用户
//!   可见字节都经它写出。二进制在 `std::process::exit` 前必须
//!   `Output::process().flush().await`，排空 `output_guard` 的协议输出队列，否则尚未
//!   flush 的字节会随进程退出丢失。同组的 [`Style`] 是 chalk v6 的 16 色样式，颜色
//!   级别每进程只检测一次。
//! - [`setup_cli`]：进程级一次性初始化（进程标题、`KIANA_CODING_AGENT`/`AI_AGENT`、
//!   HTTP dispatcher、启动期 panic 钩子）。它用 `unsafe` 写环境变量，必须在创建任何
//!   线程之前调用；`kiana-rpc` 不调用它（否则进程标题会被改成 `kiana`），而是按相同
//!   顺序显式执行需要的部分。
//! - [`sdk_exports`]：除一个本地类型别名 `ContextEditableContent` 外全部是 `pub use`；本文件
//!   再用 glob 把它整体提升到 crate 根，使
//!   `kiana_coding_agent::AgentSession` 之类的路径与 TS `index.ts` 的导出面一致。
//!   `main`/`MainOptions` 同时出现在显式再导出与 glob 中且指向同一项：Rust 中显式
//!   再导出优先于 glob，不会冲突。
//!
//! # 一次典型调用（`kiana` 二进制）
//!
//! 1. `src/main.rs` 安装主题 JSON 校验器；
//! 2. 调用 [`setup_cli`] 完成进程级初始化；
//! 3. 创建 `current_thread` tokio 运行时与 `LocalSet`——`AgentSession` 与扩展运行时是
//!    `!Send`（后者基于线程本地的 `rquickjs` 引擎），TUI 类型基于 `Rc`；
//! 4. 在 `LocalSet` 内**先**创建 `kiana_extensions::ExtensionHost`，让它存活到程序结束，
//!    再 `await` [`main`]；
//! 5. `Output::process().flush().await` 之后 `std::process::exit(code)`。
//!
//! `kiana-rpc` 的差异：进程标题为 `kiana-rpc`、在用户参数前插入 `--mode rpc`，并直接
//! 安装 panic 钩子、配置 HTTP dispatcher（因为它没有走 `setup_cli`）。
//!
//! # 边界与失败模式
//!
//! - 普通退出（如 `--help`、未知选项、一次会话正常结束）都经返回值表达，库内不终止
//!   进程；其中 `--help` 的 0 与未知选项的 1 由 `tests/cli_smoke.rs` 用真实二进制固定。
//! - 启动期致命错误（迁移中的文件系统异常）以 `FatalStartupError` panic 传播。只有安装
//!   了 `install_fatal_startup_panic_hook`（[`setup_cli`] 会安装；`kiana-rpc` 显式安装）
//!   后才映射成 Node 的 `Error: <message>` + 退出码 1，否则保持普通 Rust panic 行为。
//! - 扩展加载依赖线程本地的 `ExtensionHost`：未创建或提前 drop 时，扩展加载会报
//!   `Extension runtime engine is not installed`。
//!
//! # 阅读顺序
//!
//! [`sdk_exports`]（对外面）→ `app`（启动编排）→ `cli::args`（参数模型）→
//! `cli::setup`（输出/退出/初始化）→ `migrations` → `package_manager_cli`，最后读
//! `src/main.rs` 与 `src/bin/kiana-rpc.rs`，把整条启动链串起来。
pub mod app;
pub mod bun;
pub mod cli;
pub mod cron;
pub mod ide;
pub mod migrations;
pub mod package_manager_cli;
pub mod sdk_exports;
pub mod source_resolver;
pub mod web;

// `index.ts#main` / `MainOptions`。
pub use app::{MainOptions, main};
// `cli/setup.ts` 的进程级入口与共享基础设施。
pub use cli::setup::{CliExit, Output, Style, setup_cli};
// `index.ts` 的全量 SDK 再导出面。
// 上面的显式再导出（`main`/`MainOptions` 等）优先于这里的 glob，二者不会冲突。
pub use crate::sdk_exports::*;
