//! `core/index.ts` 的数据层再导出面（crate 的公开 barrel）。
//!
//! TS 的 `core/index.ts` 实际上**不**导出本 crate 的任何符号（`messages.ts`、
//! `session-cwd.ts`、`session-export.ts`、`nested-tool-calls.ts` 全部未从 `core/index.ts`
//! 导出；`session-manager.ts` 由 `sdk.ts` 与 modes 直接 import，同样不在 barrel 里）。
//! 按 `ARCHITECTURE.md` §4.10「跨 crate 接口一律 `pub` 且在 `lib.rs` 再导出」，本模块把
//! 数据层符号汇总为 kiana-session-core 的公开面；TS `core/index.ts` 的 `AgentSession` /
//! event-bus / extensions 导出属于 `kiana-session`、`kiana-core`、`kiana-extensions`，**不得**在此
//! 实现。
//!
//! # 作用与约束
//!
//! - 本文件只有 glob 再导出，不定义任何符号；`lib.rs` 再用 `pub use index::*` 把它展平到
//!   crate 根。新增公开符号只要在对应模块里声明为 `pub`，就会自动出现在 crate 根，无需
//!   修改本文件。
//! - 只导出 `pub` 项：模块内的 `pub(crate)` 助手（例如
//!   `messages::assistant_message_from_object`）不会被带出 crate。
//! - 不要在再导出列表里新增逐个 `pub use ... as ...` 别名：新增公开符号靠模块里的 `pub`
//!   声明即可；glob 之间的同名项会触发默认启用的 `ambiguous_glob_reexports` 警告（即使
//!   从未使用该名字），使用处还会报歧义错误，并且会改变 crate 根的公开面。
//!
//! # 阅读顺序
//!
//! 本文件无需顺序阅读；按 `crate::<模块名>` 跳转即可：`cache_stats` 是缓存统计，
//! `messages` 是自定义消息与 `convert_to_llm`，`session_manager` 是会话读写。

pub use crate::cache_stats::*;
pub use crate::messages::*;
pub use crate::nested_tool_calls::*;
pub use crate::recovery::*;
pub use crate::session_cwd::*;
pub use crate::session_export::*;
pub use crate::session_manager::*;
pub use crate::usage_totals::*;
pub use crate::virtual_models::*;
