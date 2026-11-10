//! `extensions/llama/`（5 个文件）。
//!
//! 对应 TS 目录 `packages/coding-agent/src/extensions/llama/**` 的 5 个源文件；Rust 侧另有
//! 本 `mod.rs` 组织子模块。这个目录把 llama.cpp router 服务器接入 Kiana：注册一个原生
//! provider（模型目录动态来自服务器），并注册 `/llama` 命令管理模型的加载、卸载与下载。
//!
//! # 这个模块解决什么问题
//!
//! llama.cpp 的 router 模式把“哪些模型可选、是否已加载、下载进度”都放在服务器端，
//! Kiana 只在内存里保留最近一次读取的目录（provider 的 catalog），每次打开 `/llama`
//! 都重新查询。没有本模块，llama.cpp 不会被注册为 provider，也没有管理命令，用户就无法
//! 在 Kiana 内查看或切换服务器上的模型。本模块提供的就是「查询服务器 → 在 TUI 里展示 →
//! 执行动作 → 重新查询」这条链路。
//!
//! # 在系统中的位置
//!
//! - 上游：`crate::index::builtin_extensions`（`src/index.rs`）把 [`index::llama_extension`]
//!   包装成名为 `llama.cpp` 的内建扩展工厂，资源 loader 在扩展加载时调用该工厂。
//!   该内建项不可被同名扩展替换（`replaceable: None`）。
//! - 下游：不直接实现 HTTP 与 TUI，分别依赖 `kiana-ai` 的 HTTP 契约、`kiana-ext-api`
//!   的扩展接口和 `kiana-tui` 的组件。
//!
//! # 模块分工
//!
//! - [`index`]：扩展注册与 `/llama` 命令编排（命令处理器、模型管理循环）。
//! - [`client`]：llama.cpp router 的 HTTP/SSE 客户端（目录、加载/卸载/下载、进度）。
//! - [`provider`]：实现 `kiana_ai::models::Provider` 的原生 provider；目录由 [`index`]
//!   刷新后通过 `set_catalog` 写入。
//! - [`ui`]：`/llama` 的 TUI 视图，以及带进度与取消的 `run_with_progress`。
//! - [`huggingface`]：Hugging Face 搜索与模型详情客户端，供下载流程选择量化文件。
//!
//! # 一次 /llama 调用
//!
//! 1. 命令处理器读取已保存的 llama.cpp 凭据；没有配置就提示 `/login llama.cpp` 并结束。
//! 2. 读取服务器模型目录；连接失败时弹 Retry/Close 对话框。
//! 3. 在自定义 TUI 里循环：展示模型列表 → 按动作加载/卸载/下载 → 重新读取目录。
//!
//! # 重要约束
//!
//! - 扩展运行时（`kiana-extensions`）运行在单线程 `tokio::task::LocalSet` 上
//!   （`ARCHITECTURE.md` §4.2），因此本目录的 UI 回调使用 `Rc<RefCell<_>>` 与
//!   `LocalBoxFuture`，不能跨线程发送。
//! - 对外错误统一是 `kiana_ai::error::AiError`；用户可见文案是当前稳定契约（见
//!   [`index`] 的模块文档与 `tests/llama_extension.rs`）。

// --- 子模块 ---
pub mod client;
pub mod huggingface;
pub mod index;
pub mod provider;
pub mod ui;
