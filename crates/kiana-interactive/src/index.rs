//! `modes/index.ts` 的交互侧再导出。
//!
//! TS 的 `modes/index.ts`（16 行，纯再导出，无运行时代码）共导出 **15** 个名字：
//!
//! ```text
//! export { InteractiveMode, type InteractiveModeOptions } from "./interactive/interactive-mode.ts";
//! export type { JsonAgentSessionEvent } from "./json-event.ts";
//! export { type PrintModeOptions, runPrintMode } from "./print-mode.ts";
//! export { type ModelInfo, RpcClient, ... } from "./rpc/rpc-client.ts";
//! export { runRpcMode } from "./rpc/rpc-mode.ts";
//! export type { RpcCommand, ... } from "./rpc/rpc-types.ts";
//! ```
//!
//! 其中只有前两个属于本 crate；其余 13 个（print/json/rpc）属 `kiana-rpc-print`
//! （见该 crate 的 `index.rs`，Rust 侧按 crate 拆分，避免把整个 TUI 子树拖进 RPC 二进制）。
//! 因此本模块**只**再导出交互侧的两个名字，与 TS 的 `modes/index.ts` 逐名对应。
//!
//! `createInteractiveTui` / `createInteractiveTuiReference` 在 TS 里由
//! `interactive-mode.ts` 从 `tui-renderer.ts` 再导出（`interactive-mode.ts:206`），
//! `modes/index.ts` 并不导出它们；Rust 侧同样只放在
//! [`interactive_mode`](crate::interactive_mode) 与 crate 根，本模块不重复导出。
//!
//! `interactive-mode.ts` 的其余公开符号（`format_crash_extension_hint`、
//! `format_resume_command`）不是 `modes/index.ts` 的成员，由 crate 根直接再导出。

/// 交互模式入口类型（`InteractiveMode` 与其构造选项）。
pub use crate::interactive_mode::{InteractiveMode, InteractiveModeOptions};
