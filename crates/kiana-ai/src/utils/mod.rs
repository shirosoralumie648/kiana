//! utils 目录：纯函数与事件流。
//!
//! TS: `packages/ai/src/utils/*`。每个子模块头部的文档注释给出对应的 TS 文件；
//! `event_stream` 只是冻结的 [`crate::event_stream`] 的再导出 + TS 工厂/计时助手，
//! `identity` 归 models-runtime 单元。
//!
//! # 这个目录为什么存在
//!
//! `utils` 是 `kiana-ai` 的**基础设施层**：它不实现某家服务商的协议，而是被 `api/*`、
//! `models`、`auth`、`providers` 以及仓库内其它 crate 复用的小步骤——头合并、身份与 UA、
//! JSON 宽容解析、重试判定、取消包装、文本截断等。这些步骤都可以单独测试，把它们抽出来
//! 也避免了各适配器各写一份而导致行为漂移。
//!
//! # 边界
//!
//! - 本目录不编排真实网络请求，也不访问凭证存储；它只做请求前/响应后的辅助计算与小工具
//!   （少数函数有进程级副作用，例如 `identity` 的运行时探测，见该模块说明）。请求编排见
//!   `models::apply_auth` 与各 `api/*` 适配器，HTTP 传输见 [`crate::http`]。
//! - 本文件（`mod.rs`）**刻意不做任何 `pub use`**：所有路径都是
//!   `crate::utils::<模块>::<条目>`。对外再导出由 [`crate::index`] 按 TS 根入口挑选
//!   （`event_stream`、`identity`、`json_parse`、`streaming_json`、`retry` 等），不要让
//!   同一个符号同时拥有两套路径。
//! - 依赖方向：除 `identity`（它要包装 [`crate::models::Models`]，因此引用 `models`/`auth`）
//!   与 `event_stream`（本身不实现流，只再导出 [`crate::event_stream`]）外，其余子模块只
//!   依赖 `crate::types`、`crate::error` 与同目录工具。新增依赖时不要把 `api/*` 的具体协议
//!   实现拉进来，否则会制造模块环。
//!
//! # 子模块分类（便于按需阅读）
//!
//! - 解析/文本：`json_parse`（本单元）、`streaming_json`（增量 JSON 状态机）、
//!   `sanitize_unicode`、`text`、`transcript`。
//! - 请求基础设施：`headers`（本单元）、`identity`（本单元）、`provider_env`、
//!   `node_http_proxy`（只解析代理配置，不发请求）、`error_body`。
//! - 时间/取消/重试：`abort`、`abort_signals`、`sleep`、`retry`、`provider_retry`、
//!   `diagnostics`。
//! - 模型与上下文辅助：`estimate`、`overflow`、`model_operations`、`models_error`、
//!   `openrouter_cost`、`uuid`、`validation`、`typebox_helpers`、`version`。
//!
//! # 测试
//!
//! 多数子模块的 `#[cfg(test)] mod tests` 锁定其 TS 对照行为；跨模块/端到端的断言在
//! `crates/kiana-ai/tests/`（例如 `tests/json_parse.rs`、`tests/identity.rs`）。
//! `validation` 与 `version` 没有内联测试，它们的行为分别由 `tests/validation.rs` 与
//! `tests/identity.rs`（其中的 version 常量断言）覆盖。
//! 测试文件本身不逐条注释，阅读时先看被调用的公开入口文档。

// --- 子模块 ---
pub mod abort;
pub mod abort_signals;
pub mod diagnostics;
pub mod error_body;
pub mod estimate;
pub mod event_stream;
pub mod hash;
pub mod headers;
pub mod identity;
pub mod json_parse;
pub mod model_operations;
pub mod models_error;
pub mod node_http_proxy;
pub mod oauth_page;
pub mod openrouter_cost;
pub mod overflow;
pub mod provider_env;
pub mod provider_retry;
pub mod retry;
pub mod sanitize_unicode;
pub mod sleep;
pub mod streaming_json;
pub mod text;
pub mod transcript;
pub mod typebox_helpers;
pub mod uuid;
pub mod validation;
pub mod version;
