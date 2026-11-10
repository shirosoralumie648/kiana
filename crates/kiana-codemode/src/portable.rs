//! portable.ts 再导出面：不需要 Node 内建的入口。
//!
//! TS 源：`packages/codemode/src/portable.ts`。`InlineCodemodeSandbox` 在调用线程上跑 VM，
//! `RemoteCodemodeSandbox` 在另一个 isolate 里跑；两者都不导入 `node:*`，因此该入口在
//! Cloudflare Workers、Durable Objects 与浏览器里可用。`CodemodeSandbox`（worker 线程）
//! 留在主入口。
//!
//! 逐字对齐 spec §2.2.15 / §3.3 的清单：
//!
//! - 值：`toCodemodeIdentifier`、`CodemodeSandboxBase`、`DEFAULT_INTERRUPT_BUDGET`、
//!   `InlineCodemodeSandbox`、`MAX_OUTPUT_CHARS`、`MAX_OUTPUT_ITEMS`、`MAX_STORE_TOTAL_CHARS`、
//!   `MAX_STORE_VALUE_CHARS`、`RemoteCodemodeSandbox`、`serveCodemodeRemote`。
//! - 类型：`InlineCodemodeSandboxOptions`、`HostToWorkerMessage`/`VmStartData`/
//!   `WorkerToHostMessage`（协议层只在这里出现）、`CodemodeRemote*`、`CodemodeServeOptions`、
//!   `RemoteCodemodeSandboxOptions`、`CodemodeWasmModule`，以及除 `CodemodeSandboxOptions`
//!   外的全部 `Codemode*` 类型。
//!
//! **不**导出 `CodemodeSandbox`（worker 后端）、`loadQuickJSWasm`（文件系统）、
//! `render*`/`schemaToType`/`source` 解析器（这些走各自的子路径入口）。
//!
//! 本模块只有 `pub use`，不含实现（「同一 TS 文件只允许一个 crate 拥有实现」）。
//!
//! # 与主入口的差异
//!
//! 相对 `index`，本模块不含 `CodemodeSandbox`（worker 线程后端）、`loadQuickJSWasm`
//! （读文件系统）、`render*`/`schemaToType`（声明生成）与源码解析器；类型面不含
//! `CodemodeSandboxOptions`（worker 后端选项）。反过来它包含协议层的
//! `HostToWorkerMessage` / `VmStartData` / `WorkerToHostMessage`，但不导出协议守卫 `is_*`。
//!
//! # 验证
//!
//! `tests/api_surface.rs` 的 `portable_entry_exports_the_frozen_surface` 在编译期逐个引用
//! 本文件的导出；少一行 `pub use` 即编译失败。

// --- identifier.ts ---
pub use crate::identifier::to_codemode_identifier;
// --- runtime/prelude-source.ts（只导出 4 个上限） ---
pub use crate::runtime::prelude_source::{
    MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS,
};
// --- runtime/protocol.ts（portable 只导出这 3 个类型，不导出守卫） ---
pub use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerToHostMessage};
// --- types.ts（除 CodemodeSandboxOptions 外的全部） ---
pub use crate::types::{
    CodemodeCall, CodemodeCallStatus, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeDuring,
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeHostCall, CodemodeJsonSchema,
    CodemodeOutcome, CodemodeOutputItem, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeStoreWrites, CodemodeTool, CodemodeToolContext,
};
// --- wasm.ts（只有不透明类型） ---
pub use crate::wasm::CodemodeWasmModule;

// --- execution/inline/remote：`/portable` 的其余值导出 ---
//
// 这些行无条件生效（本文件没有 `#[cfg]` 属性），与 `index` 共用同一批源项。
// `tests/api_surface.rs` 的 `portable_entry_exports_the_frozen_surface` 在编译期逐个引用它们。
pub use crate::runtime::execution::CodemodeSandboxBase;
pub use crate::runtime::inline::{
    DEFAULT_INTERRUPT_BUDGET, InlineCodemodeSandbox, InlineCodemodeSandboxOptions,
};
pub use crate::runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply,
    CodemodeRemoteRequest, CodemodeServeOptions, RemoteCodemodeSandbox,
    RemoteCodemodeSandboxOptions, serve_codemode_remote,
};
