//! 对应 ca-builtin-extensions：mcp / codemode / llama.cpp / tool-search 四个内置扩展。
//!
//! TS 源：packages/coding-agent/src/extensions/**（24 个源文件）。
//!
//! 每个 TS 目录对应一个 Rust 模块目录，目录内一个 TS 文件一个模块（`*.lazy.ts` 是动态 import 包装，
//! 并入同名模块，见 `ARCHITECTURE.md` §5.2）。
//!
//! # 对外接口（spec §5.2）
//!
//! - `kiana mcp …` 子命令：[`load_mcp_command`] / [`run_mcp_command`] / [`McpCommandOptions`]。
//! - 内建扩展清单与四个工厂：[`builtin_extensions`]、[`llama_extension`]、
//!   [`create_codemode_extension`]、[`create_tool_search_extension`]、[`create_mcp_extension`]。
//! - 系统提示 section：[`MCP_SERVERS_SECTION`]、[`render_servers_section`]。
//! - SDK 用户直接构造内建扩展所需的选项类型与 [`McpTransportFactory`]。
//! - downstream 复用的 codemode/tool-search/llama 纯函数（renderer、store、URL 归一、BM25 等）。
//!
//! # 这个 crate 解决什么问题
//!
//! Kiana 的扩展通过扩展 API（`kiana_ext_api::ExtensionApi`）向宿主注册工具、命令、
//! 工具渲染器与 provider。本 crate 提供随主程序发布的四个内置扩展，并把它们各自的
//! 协议、沙箱与 HTTP 细节收敛在对应模块里：MCP 客户端、codemode 脚本工具、BM25 工具搜索、
//! llama.cpp provider 与 `/llama` 管理器。
//!
//! 这四者本身不实现底层协议：MCP 的 JSON-RPC 与传输在 `kiana-mcp`，QuickJS 沙箱在
//! `kiana-codemode`，模型与 provider 类型在 `kiana-ai`，注册接口在 `kiana-ext-api`。
//! 本 crate 的边界是“把外部能力适配成扩展 API 的注册动作”，底层协议由上述 crate 负责。
//!
//! # 在系统中的位置
//!
//! 调用方（上游）：
//!
//! - `kiana-coding-agent` 的启动编排：先调用 [`builtin_extensions`] 取得 4 个内建工厂，
//!   再把用户注入的工厂追加在后面；首个参数为 `mcp` 时直接调用 [`run_mcp_command`]
//!   （`crates/kiana-coding-agent/src/app.rs`）。
//! - `kiana-coding-agent` 的 SDK 导出面：按 TS `src/index.ts` 的名字重导出
//!   [`create_codemode_extension`]、[`create_mcp_extension`]、
//!   [`create_tool_search_extension`] 与相关选项类型（`src/sdk_exports.rs`）。
//! - `kiana-interactive`：为 Radius provider 写入 MCP 服务器时直接使用 `mcp::config` 的
//!   `load_mcp_config` / `add_mcp_server_config`（`src/interactive_mode/commands/auth.rs`）。
//!   该模块不在根部的重导出清单里，但 `pub mod mcp;` 使这个路径成为公开 API。
//!
//! 被调用方（下游）与直接依赖：`kiana-mcp`、`kiana-codemode`、`kiana-ai`、
//! `kiana-ext-api`、`kiana-extensions`、`kiana-tui`、`kiana-ui-kit`、`kiana-tools`、
//! `kiana-core`、`kiana-config`、`kiana-session-core`、`kiana-json` 等，完整清单见
//! `crates/kiana-builtin-extensions/Cargo.toml`。
//!
//! # 目录与再导出策略
//!
//! 源码按 TS 目录镜像：`codemode/`、`llama/`、`mcp/`、`tool_search/` 各对应一个 TS 目录，
//! 目录内文件名与 TS 文件同名。注意 `index.rs` 对应 TS 的 `index.ts`，是扩展工厂或注册表
//! 所在模块，不是 Rust 常见的“目录汇总文件”；`worker.rs` 只有文档，真正的 worker 入口
//! 在 `kiana-codemode`。
//!
//! 根部同时使用 `pub mod` 与 `pub use`，两者职责不同：
//!
//! - `pub mod` 让完整模块路径（如 `kiana_builtin_extensions::mcp::config`）成为公开 API；
//!   外部调用方与集成测试会直接按模块路径导入。
//! - `pub use` 把跨 crate 最常用的入口提升到根部，是对外门面，对应 spec §5.2
//!   “本单元对外提供的接口”。它不是子模块的完整镜像：内部函数默认留在子模块，
//!   只有确有外部消费者时才加到根部。
//!
//! `theme_snapshot` 是唯一的私有模块：`mcp/ui.rs` 与 `llama/ui.rs` 共用它把宿主 `Theme`
//! 的 ANSI 序列复制成可拥有的快照（`pub(crate)` 类型），不属于对外契约。
//!
//! # 一次典型调用
//!
//! 1. 启动：[`builtin_extensions`] 返回 4 个 `InlineExtension::Named`，名字依次为
//!    `llama.cpp`、`codemode`、`tool-search`、`mcp`。此时只构造工厂闭包，不会执行它。
//! 2. 加载：资源 loader 按 `builtin:<name>` 实例化工厂，并在扩展运行时的单线程
//!    `LocalSet` 上轮询工厂返回的 future。工厂向 `ExtensionApi` 注册工具、命令、
//!    渲染器或 provider；future 完成后，扩展行为全部由宿主事件驱动。
//! 3. 失败：工厂 future 的输出是 `()`，没有 `Result`，安装失败不会回流给 loader。
//! 4. 命令行：`kiana mcp …` 不经过扩展加载，[`run_mcp_command`] 直接执行子命令并返回
//!    进程退出码（0 表示成功或帮助，1 表示任意失败）。
//!
//! 两个工具型扩展（`codemode`、`tool_search`）注册的工具默认不激活
//! （`default_active = false`），需要 `--tools`/`defaultTools` 或 `setActiveTools()`
//! 才会声明给模型；MCP 扩展在工具只能经脚本触达时会主动激活 codemode。
//!
//! # 边界与失败模式
//!
//! - 扩展工厂没有错误返回通道（future 输出 `()`）。工厂内部的失败没有任何返回值可以
//!   承载，调用方无法用返回值判断安装是否成功。
//! - 扩展运行时是单线程 `LocalSet`。codemode 的 `appendEntry` 转发与 tool-search 的
//!   `get_all_tools`/`set_active_tools` 转发都用 `spawn_local` 与同线程通道桥接 `!Send`
//!   的 `ExtensionApi`；codemode 工厂的文档明确写着在普通线程调用会 panic。
//! - [`run_mcp_command`] 只返回 0/1，人类可读消息与诊断分别走 `options.log` /
//!   `options.error`。`add`/`remove` 直接重写 `mcp.json`（默认是全局 `agent_dir/mcp.json`，
//!   `-l`/`--local` 选项目文件）；其余子命令先读 `trust.json`，读取失败即返回 1。
//! - 内建扩展名字是配置契约：settings 与 CLI 用 `builtin:<name>` 引用它们，改名会让
//!   已有配置变成“未知扩展”（见 `index.rs` 的模块文档与 `tests/builtin_registry.rs`）。
//!
//! # 建议阅读顺序
//!
//! 1. `index.rs`：四个内建项的名字、顺序与 `replaceable`/`builtin` 标志。
//! 2. `tool_search/` 与 `codemode/`：两个工具型扩展，先理解注册与延迟激活。
//! 3. `mcp/`：先 `config.rs`（配置形状与合并），再 `runtime.rs`（连接与错误状态机），
//!    `index.rs` 是生命周期主干。
//! 4. `llama/`：provider 与 `/llama` 管理命令，依赖 `kiana-ai` 的 Provider 契约。
//!
//! 各模块的详细约束以其文件内 `//!` 为准；本文件只描述 crate 级结构与调用顺序。

pub mod codemode;
pub mod index;
pub mod llama;
pub mod mcp;
pub mod tool_search;

mod theme_snapshot;

// --- 内建扩展清单 ---
pub use crate::index::builtin_extensions;

// --- codemode ---
pub use crate::codemode::execute::read_codemode_store;
pub use crate::codemode::index::{CodemodeExtensionOptions, create_codemode_extension};
pub use crate::codemode::renderer::codemode_renderers;
pub use crate::codemode::tool::{CODEMODE_STORE_ENTRY_TYPE, CODEMODE_TOOL_NAME};

// --- tool-search ---
pub use crate::tool_search::index::create_tool_search_extension;
pub use crate::tool_search::tool::{
    Bm25Ranker, Bm25RankerOptions, DEFAULT_TOOL_SEARCH_LIMIT, TOOL_SEARCH_TOOL_NAME,
    create_tool_search_document, tokenize,
};

// --- llama.cpp ---
pub use crate::llama::client::{format_bytes, llama_inference_url, normalize_llama_server_url};
pub use crate::llama::index::llama_extension;
pub use crate::llama::provider::{
    DEFAULT_LLAMA_SERVER_URL, LLAMA_PROVIDER_ID, LlamaProviderController, create_llama_provider,
    create_llama_provider_with_http,
};

// --- mcp ---
pub use crate::mcp::cli::{McpCommandOptions, load_mcp_command, run_mcp_command};
pub use crate::mcp::index::{
    MAX_SERVERS_SECTION_CHARS, MCP_SERVERS_SECTION, McpExtensionOptions, McpServerConnectionInfo,
    McpServerListing, McpTransportFactory, create_mcp_extension, render_servers_section,
};
pub use crate::mcp::runtime::{default_mcp_sandbox, install_default_mcp_sandbox};
pub use crate::mcp::tools::{
    MCP_OUTPUT_MAX_BYTES, create_mcp_tool_name, create_mcp_tool_renderers,
};
