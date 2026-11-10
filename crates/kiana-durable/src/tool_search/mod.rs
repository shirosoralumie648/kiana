//! tool-search 目录（按 TS 源 1:1 映射）。
//!
//! # 这个目录为什么存在
//!
//! 本文件只做目录分层：TS 的 `src/tool-search/` 在 Rust 侧按路径映射成 `src/tool_search/`，
//! 目录里唯一的子模块源文件 `tool_search.rs` 承载 `tool_search` 工具的全部实现（工具定义、
//! 执行逻辑与扩展构造器）。
//!
//! 因为 TS 源文件名是 `tool-search/tool-search.ts`，父模块与子模块同名是刻意的：改名会同时
//! 破坏与 TS 路径的对应关系，以及公开路径
//! `kiana_durable::tool_search::tool_search::create_tool_search_extension`（集成测试按这个
//! 路径导入）。`clippy::module_inception` 的豁免就是为这层同名保留的。
//!
//! # 边界
//!
//! 这里不重新导出子模块的条目，使用方按完整路径导入；当前仓库内这样用到的只有
//! `crates/kiana-durable/tests/tool_search.rs`。工具在运行期怎样拿到候选、怎样把命中写回
//! 会话，见子模块的文件级文档。

// --- 子模块 ---
/// TS 文件名即 `tool_search.ts`，模块名与父模块同名是刻意的（保持 1:1 映射）。
#[allow(clippy::module_inception)]
pub mod tool_search;
