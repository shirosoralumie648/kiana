//! index.ts 再导出面：主入口（含 worker 后端）。
//!
//! TS 源：`packages/codemode/src/index.ts`。逐字对齐 spec §2.2.15 / §3.3 的清单。
//!
//! # 冻结的再导出契约（其他四个单元按这些名字实现，本单元只做 `pub use`）
//!
//! | TS 导出 | Rust 名 | 所在模块 |
//! |---|---|---|
//! | `DEFAULT_INPUT_SCHEMA_MAX_CHARS` | `DEFAULT_INPUT_SCHEMA_MAX_CHARS` | `declarations` |
//! | `RenderDeclarationsOptions` | `RenderDeclarationsOptions` | `declarations` |
//! | `renderDeclarations` | `render_declarations` | `declarations` |
//! | `renderToolOutputType` | `render_tool_output_type` | `declarations` |
//! | `renderToolSample` | `render_tool_sample` | `declarations` |
//! | `renderToolSignature` | `render_tool_signature` | `declarations` |
//! | `schemaToType` | `schema_to_type` | `declarations` |
//! | `toCodemodeIdentifier` | `to_codemode_identifier` | `identifier` |
//! | `CodemodeSandboxBase` | `CodemodeSandboxBase` | `runtime::execution` |
//! | `CodemodeSandbox` | `CodemodeSandbox` | `runtime::host` |
//! | `DEFAULT_INTERRUPT_BUDGET` | `DEFAULT_INTERRUPT_BUDGET` | `runtime::inline` |
//! | `InlineCodemodeSandbox` | `InlineCodemodeSandbox` | `runtime::inline` |
//! | `InlineCodemodeSandboxOptions` | `InlineCodemodeSandboxOptions` | `runtime::inline` |
//! | `MAX_OUTPUT_CHARS` 等 4 个 | 同名 | `runtime::prelude_source` |
//! | `RemoteCodemodeSandbox` | `RemoteCodemodeSandbox` | `runtime::remote` |
//! | `RemoteCodemodeSandboxOptions` | `RemoteCodemodeSandboxOptions` | `runtime::remote` |
//! | `serveCodemodeRemote` | `serve_codemode_remote` | `runtime::remote` |
//! | `CodemodeRemote*`（5 个类型） | 同名 | `runtime::remote` |
//! | `CodemodeServeOptions` | `CodemodeServeOptions` | `runtime::remote` |
//! | `CODEMODE_OPTIONS_PREFIX` / `CODEMODE_SOURCE_GRAMMAR` / `CodemodeSourceError` / `parseCodemodeSource` / `CodemodeSourceOptions` / `ParsedCodemodeSource` | 同名 / `parse_codemode_source` | `source` |
//! | `Codemode*` 类型（§2.2.15 的 21 个） | 同名 | `types` |
//! | `CodemodeWasmModule` / `loadQuickJSWasm` | 同名 / `load_quickjs_wasm` | `wasm` |
//!
//! **不**从主入口导出（spec §2.2.15）：`PRELUDE_SOURCE`（任何公开入口都不导出）、
//! `startVm`/`VmSession`/`StartVmOptions`、`inlineLauncher`/`INLINE_MAX_STACK_SIZE`、
//! 协议守卫 `isWorkerToHostMessage`/`isHostToWorkerMessage`/`VmStartData`/`WorkerData`、
//! `errorMessage`、`ScriptErrorJson`。
//!
//! 本模块只有 `pub use`，不含实现（「同一 TS 文件只允许一个 crate 拥有实现」）。
//!
//! # 维护方式
//!
//! - 本文件是根面（crate 根公开面）的唯一权威：`lib.rs` 用 `pub use index::*` 把这里的名字
//!   提升到 crate 根，增删根面导出只改这里，不要在 `lib.rs` 手抄第二份清单。
//! - 每行 `pub use` 的文档由被再导出的源项提供（rustdoc 继承），本文件不重复写 `///`；
//!   要改语义说明请改 `declarations` / `runtime::*` / `source` / `types` / `wasm` 的定义处。
//! - `tests/api_surface.rs` 的 `main_entry_exports_the_frozen_surface` 在编译期逐个引用清单
//!   里的值与类型；漏掉任何一行都会让该测试无法编译（该测试文件头说明了这个设计）。

// --- declarations.ts / runtime::{execution,host,inline,remote} ---
//
// 这些符号由本 crate 的 `declarations` 与 `runtime::{execution,host,inline,remote}` 提供，
// 主入口的公开面因此逐字等于 spec §2.2.15 的清单。
pub use crate::declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, RenderDeclarationsOptions, render_declarations,
    render_tool_output_type, render_tool_sample, render_tool_signature, schema_to_type,
};
pub use crate::runtime::execution::CodemodeSandboxBase;
pub use crate::runtime::host::CodemodeSandbox;
pub use crate::runtime::inline::{
    DEFAULT_INTERRUPT_BUDGET, InlineCodemodeSandbox, InlineCodemodeSandboxOptions,
};
pub use crate::runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteMessage, CodemodeRemoteReply,
    CodemodeRemoteRequest, CodemodeServeOptions, RemoteCodemodeSandbox,
    RemoteCodemodeSandboxOptions, serve_codemode_remote,
};
// --- identifier.ts ---
pub use crate::identifier::to_codemode_identifier;
// --- runtime/prelude-source.ts（只导出 4 个上限；PRELUDE_SOURCE 不在公开面） ---
pub use crate::runtime::prelude_source::{
    MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS,
};
// --- source.ts ---
pub use crate::source::{
    CODEMODE_OPTIONS_PREFIX, CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions,
    ParsedCodemodeSource, parse_codemode_source,
};
// --- types.ts（§2.2.15 的 21 个类型） ---
pub use crate::types::{
    CodemodeCall, CodemodeCallStatus, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeDuring,
    CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeHostCall, CodemodeJsonSchema,
    CodemodeOutcome, CodemodeOutputItem, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeSandboxOptions, CodemodeStoreWrites, CodemodeTool,
    CodemodeToolContext,
};
// --- wasm.ts ---
pub use crate::wasm::{CodemodeWasmModule, load_quickjs_wasm};
