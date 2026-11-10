//! harness 目录（23 个文件，按 TS 源 1:1 映射）。
//!
//! 每个文件对应 `packages/durable/src/harness/` 下的同名 `.ts`（本文件除外），模块边界、
//! 公开名字与调用关系都以 TS 为参照，而不是按 Rust 习惯重新分层。
//!
//! # 这个目录负责什么
//!
//! - 扩展模型与解析：`types`（trait 与数据形状）、`registry`（安装与校验）、`define`
//!   （构造器）、`agent`（合并扩展、解析 agent）。
//! - 运行循环：`harness`（会话门面）、`scheduler`（任务与 hook 调度）、`task_graph`、
//!   `generation`（一次模型生成）、`tool`（工具轮次）、`compaction`。
//! - 上下文与提示：`context`（转录与上下文视图）、`prompt`（系统提示分段与写入）、`view`。
//! - 持久文档与提交：`live`、`inbox`、`submissions`、`events`、`telemetry`、`usage`、
//!   `provider`、`partial_changes`、`json`、`util`。
//!
//! 依赖方向大体是单向的：`inbox` 不依赖 `submissions`，由后者调用前者；`json` 只被
//! `partial_changes` 与 `tool` 使用。
//!
//! # 阅读顺序
//!
//! 主链路：`types` → `define` → `registry` → `agent` → `harness` → `context` → `prompt`
//! → `inbox` → `submissions`。流式部分：`events` → `task_graph` → `view` →
//! `partial_changes` → `json`。`spec/durable-harness.md` 对每个文件的 TS 行数与导出面有
//! 逐条清单。
//!
//! # 注意事项
//!
//! 各模块的说明写在各自文件的 `//!` 里；不要在本文件的 `pub mod` 上方新增外层 `///`。
//! 外层文档与文件内 `//!` 同时存在时，rustdoc 会把合并后的文档放到父模块作用域解析链接，
//! 内部文档里的短链接会失效。`harness` 子模块是历史例外（已有外层 `///`），它的文件内
//! `//!` 链接因此必须写成 `crate::...` 全限定形式。

// --- 子模块 ---
pub mod agent;
pub mod compaction;
pub mod context;
pub mod define;
pub mod events;
pub mod generation;
/// TS 文件名即 `harness.ts`，模块名与父模块同名是刻意的（保持 1:1 映射）。
#[allow(clippy::module_inception)]
pub mod harness;
pub mod inbox;
pub mod json;
pub mod live;
pub mod partial_changes;
pub mod prompt;
pub mod provider;
pub mod record_store;
pub mod registry;
pub mod scheduler;
pub mod submissions;
pub mod task_graph;
pub mod telemetry;
pub mod tool;
pub mod types;
pub mod usage;
pub mod util;
pub mod view;
