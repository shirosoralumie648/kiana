//! session 目录（按 TS 源 1:1 映射）。
//!
//! # 职责
//! 这个目录对应 TS 的 `packages/durable/src/session/`：`session.rs` 是会话内核（变更线、
//! 文档缓存、提交发布），`transaction.rs` 是一次提交里的事务 API，`forks.rs` 负责选出
//! fork 时要复制的文档。本文件只声明子模块并把它们的公开面再导出，自身没有逻辑。
//!
//! # 为什么保留同名子模块
//! TS 的文件名就是 `session.ts`，为了模块路径与源文件一一对应，Rust 侧保留 `session.rs`
//! 与父模块同名，因此需要 `#[allow(clippy::module_inception)]`；这是刻意豁免，不是疏漏。
//!
//! # 公开面
//! 末尾的 `pub use` 让调用方可以只写 `kiana_durable::session::SessionImpl` 这类短路径；
//! 完整路径 `kiana_durable::session::session::SessionImpl` 同样有效。`forks` 只导出
//! `ForkDocumentCopy` 与 `prepare_fork_document_copies`，后者由 `transaction` 的
//! `fork_conversation`（经 `stage_conversation`）调用。

// --- 子模块 ---
pub mod forks;
/// TS 文件名即 `session.ts`，模块名与父模块同名是刻意的（保持 1:1 映射）。
#[allow(clippy::module_inception)]
pub mod session;
pub mod transaction;

// 与 TS 的 `session/*` 一样，公开面在父模块即可用。
// 三个 glob 再导出共同构成 `kiana_durable::session` 的公开面；`forks::*` 只带来
// `ForkDocumentCopy` 与 `prepare_fork_document_copies`。
pub use forks::*;
pub use session::*;
pub use transaction::*;
