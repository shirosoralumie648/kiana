//! `packages/agent/src/index.ts` 的再导出面（纯 barrel，无副作用）。
//!
//! TS 源逐字：
//!
//! ```ts
//! export * from "./agent.ts";
//! export * from "./agent-loop.ts";
//! export * from "./proxy.ts";
//! export { setDefaultStreamFn } from "./stream-fn.ts";
//! export * from "./types.ts";
//! ```
//!
//! 注意 `getDefaultStreamFn` 只被 `agent-loop.ts` 与 `agent.ts` 内部 import，包公开面**不含**它：
//! 这里只显式再导出 `set_default_stream_fn`，`get_default_stream_fn` 仅可通过
//! [`crate::stream_fn`] 访问。
//!
//! ```compile_fail
//! // index.ts 的公开面没有 getDefaultStreamFn（只有 stream-fn.ts 模块级导出）。
//! use kiana_agent_core::index::get_default_stream_fn;
//! ```
//!
//! # 设计原因与边界
//!
//! 这个模块是 TS `index.ts` 的 Rust 镜像：`export *` 对应 glob 再导出，逐字列举的
//! `export { ... }` 对应具名再导出。glob 会把被再导出模块里的**所有** `pub` 项带进
//! `index` 命名空间，因此新增公开项会自动出现在这里（与 `export *` 的语义一致），
//! 不需要改本文件。
//!
//! crate 根的 `lib.rs` 另有一份**具名**再导出清单（下游更常用的短路径）；两处都源于
//! `index.ts` 的公开面，但覆盖范围不同，增删公开项时需要同时检查。
//! 仓库内 `kiana_agent_core::index::*` 的主要消费者是公开面一致性测试
//! `tests/public_api.rs`：它逐项 import，用编译期错误锁住名字与可见性。

// 并行实现期间 agent.rs / agent_loop.rs 可能暂时是空模块；barrel 必须照样写 glob
// （契约见模块文档），空 glob 的 `unused_imports` 不是问题。
#[allow(unused_imports)]
pub use crate::agent::*;
#[allow(unused_imports)]
pub use crate::agent_loop::*;
pub use crate::proxy::*;
// TS 的 `DEFAULT_AGENT_IDENTITY` 在 agent-loop.ts 里是 `export const`，属于包公开面；
// Rust 侧它的实现在 identity.rs，因此这里显式再导出。
pub use crate::identity::default_agent_identity;
pub use crate::stream_fn::set_default_stream_fn;
pub use crate::types::*;
