//! `extensions/tool-search/`（2 个文件）。
//!
//! 本文件只做目录映射：TS 的 `extensions/tool-search/` 下有 `tool.ts` 与 `index.ts`，
//! Rust 分别对应 [`tool`] 与 [`index`]。这里没有额外的门面类型；对外入口经 crate 根
//! 重导出（`create_tool_search_extension`，以及 `tool` 的部分公开项——
//! `Bm25Ranker`、`tokenize` 等；tool 项非通配导入，完整清单见 `lib.rs` 的 `pub use`）。
//!
//! # 两个子模块的分工
//!
//! - [`tool`]：与扩展运行时无关的纯逻辑——分词、`stem`、BM25 排名、搜索文档构造，以及
//!   `tool_search` 的 `ToolDefinition`（schema、参数校验、结果文案）。codemode 的
//!   `searchTools()` 也复用这里的 [`tool::Bm25Ranker`] 与
//!   [`tool::create_tool_search_document`]（见 `crate::codemode::execute`）。
//! - [`index`]：把 `tool_search` 注册为扩展，并解决 `ExtensionApi` 是 `!Send` 而工具执行器
//!   要求 `Send + Sync` 的阻抗不匹配；同时把 `default_active` 设为 `false`，使工具默认不激活。
//!
//! # 阅读顺序
//!
//! 先读 [`tool`] 的 [`tool::ToolSearchDocument`]、[`tool::Bm25Ranker`] 与
//! [`tool::create_tool_search_document`]，再看 [`tool::create_tool_search_tool_definition`]
//! 注册的 schema 与执行流程，最后读 [`index`] 的同线程转发任务。`tool_search` 的激活语义
//! （默认不激活；用 `--tools`、`defaultTools` 或 `setActiveTools()` 激活）见 [`index`] 的
//! 模块文档。

// --- 子模块 ---
pub mod index;
pub mod tool;
