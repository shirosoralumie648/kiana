//! `index.ts` 再导出面：crate 根的公开 API 逐字等于 TS 包根（`spec/telemetry-mcp.md` §19.2）。
//!
//! 顺序与 `src/index.ts` 的 `export ... from` 分组一致：auth-provider → client → content →
//! jsonrpc → types → stdio → streamable-http → transport。
//!
//! # 当前状态
//!
//! §19.2 的 67 个名字全部导出：`McpClient`/`McpClientOptions`/`McpRequestOptions`
//! （`client.rs`）、`StdioTransport`/`StdioTransportOptions`（`transports/stdio.rs`）、
//! `McpAuthRequiredError`/`McpHttpError`/`McpSessionExpiredError`/`StreamableHttpTransport`/
//! `StreamableHttpTransportOptions`（`transports/streamable_http.rs`）都由其他单元实现，
//! 并在下面按 `src/index.ts` 的分组顺序逐名导出。
//!
//! 四个错误类合并之后，crate 根实际绑定 **64** 个 Rust 名字（67 个 TS 名字里有 4 个映射到
//! 同一个 `McpError`）；`tests/api_surface.rs` 用一份逐名 `use` 清单在编译期核对它们。
//!
//! # 与 TS 清单的差异（全部来自 `ARCHITECTURE.md` §4.9 的冻结契约）
//!
//! - TS 的 4 个错误类 `McpError` / `McpConnectionClosedError` / `McpTimeoutError` /
//!   `McpAbortError` 在骨架里合并成 **1 个** `enum McpError`（`name()` 保留四个名字）。
//!   因此这里只有 `McpError`；判定靠 `McpError::name()` / `is_abort()`，字段靠
//!   `code()`/`data()`/`timeout_ms()`。
//! - `JSON_RPC_ERROR_CODES` 在骨架里是 `JsonRpcErrorCodes`（关联常量），这里用别名保留 TS 名。
//! - `McpTransportMessageListener` 等三个监听器别名在 Rust 里是 `Arc<dyn Fn ...>`。
//!
//! # 刻意**不**在这里再导出的名字（§30.1）
//!
//! `isObject`、`toError`、`isJsonRpcId`、`TransportEvents`、`DEFAULT_MAX_MESSAGE_BYTES`：
//! TS 包根没有导出它们，所以 crate 根也不导出（模块里仍然是 `pub`，包内与测试直接按路径用）。
//!
//! # 名字映射（TS → Rust）
//!
//! 类型名两边相同；函数按 Rust 命名习惯改成 snake_case，其余保持逐字：
//! `toLlmContent → to_llm_content`、`isJsonRpcNotification → is_json_rpc_notification`、
//! `isJsonRpcRequest → is_json_rpc_request`、`isJsonRpcResponse → is_json_rpc_response`、
//! `parseJsonRpcMessage → parse_json_rpc_message`、`JsonRpcId → JsonRpcId`、
//! `JSON_RPC_ERROR_CODES → JSON_RPC_ERROR_CODES`（[`crate::jsonrpc::JsonRpcErrorCodes`] 的别名）。
//! 四个错误类合并成 [`McpError`]（见上）。
//!
//! # 由其他单元提供的名字
//!
//! `client`（`McpClient`、`McpClientOptions`、`McpRequestOptions`）、`stdio`
//! （`StdioTransport`、`StdioTransportOptions`）与 `streamable_http`（`McpAuthRequiredError`、
//! `McpHttpError`、`McpSessionExpiredError`、`StreamableHttpTransport`、
//! `StreamableHttpTransportOptions`）分别由 `src/client.rs` / `src/transports/{stdio,streamable_http}.rs`
//! 提供，均已在下方导出。
//!
//! 注意 `client` 模块里还有**不应**进 crate 根的名字（§30.1）：`ConnectionState`、`Roots`、
//! `RequestContext`、`RequestHandler`、`NotificationListener`、`ErrorListener`、`CloseListener`、
//! `ProgressCallback`、`McpSubscription`、`DEFAULT_REQUEST_TIMEOUT_MS`、`MAX_LIST_PAGES`、
//! `abort_reason_string`。因此这里是逐名 `pub use`，不是 glob。
//!
//! 之所以不用 `pub use crate::client::*;` 之类的 glob：`streamable_http` 里还有
//! `consume_sse_stream`/`SseEvent`/`ConsumeSseOptions`/`StreamableHttpReconnectOptions` 等
//! **不应**进 crate 根的名字（§30.1），glob 会把它们泄漏出去。
//!
//! # 维护约束
//!
//! 下面的 `pub use` 是逐名清单而不是 glob（原因见上）：目标模块里任何一个名字被删除或改名，
//! 本文件都会编译失败。`crates/kiana-mcp/tests/api_surface.rs` 用同样的 `use kiana_mcp::{...}`
//! 清单做编译期断言。因此增删 crate 根名字时要同时改三处：本文件、`spec/telemetry-mcp.md`
//! §19.2 的清单、`tests/api_surface.rs`；只改其中一处会让实际公开 API 与冻结契约脱节。

#![allow(non_camel_case_types)]

// --- auth-provider.ts -------------------------------------------------------
pub use crate::auth_provider::{AuthProvider, McpFetch, UnauthorizedContext};

// --- client.ts --------------------------------------------------------------
pub use crate::client::{McpClient, McpClientOptions, McpRequestOptions};

// --- protocol/content.ts ----------------------------------------------------
pub use crate::protocol_types::{
    AudioContent, BlobResourceContents, CallToolResult, ContentAnnotations, ContentBlock,
    EmbeddedResourceContent, ImageContent, LlmContent, ResourceLinkContent, TextContent,
    TextResourceContents, to_llm_content,
};

// --- protocol/jsonrpc.ts ----------------------------------------------------
pub use crate::jsonrpc::{
    JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, JsonRpcSuccessResponse, McpError, is_json_rpc_notification,
    is_json_rpc_request, is_json_rpc_response, parse_json_rpc_message,
};

/// `JSON_RPC_ERROR_CODES`（TS 的对象常量；Rust 是 [`crate::jsonrpc::JsonRpcErrorCodes`] 的别名）。
///
/// 保持 TS 的 SCREAMING_CASE 名字，调用方 `JSON_RPC_ERROR_CODES::METHOD_NOT_FOUND` 才与 TS 的
/// `JSON_RPC_ERROR_CODES.methodNotFound` 对得上（重命名的 `pub use` 触发
/// `non_camel_case_types`，整文件 `#![allow]` 是这个别名唯一的例外）。
pub use crate::jsonrpc::JsonRpcErrorCodes as JSON_RPC_ERROR_CODES;

// --- protocol/types.ts ------------------------------------------------------
pub use crate::protocol_types::{
    CancelledNotification, ClientCapabilities, GetPromptResult, Implementation, InitializeParams,
    InitializeResult, LATEST_PROTOCOL_VERSION, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, ProgressNotification, Prompt, PromptArgument,
    PromptMessage, PromptRole, ReadResourceResult, Resource, ResourceTemplate, Root,
    SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities, SupportedProtocolVersion, Tool,
    ToolAnnotations, ToolExecution,
};

// --- transports/stdio.ts ----------------------------------------------------
// 按行分帧的 stdio 传输；`StdioStderrMode` 等模块级名字不进包根（§30.1）。
pub use crate::transports::stdio::{StdioTransport, StdioTransportOptions};

// --- transports/streamable-http.ts ------------------------------------------
// 三个错误类在骨架里统一成 `McpTransportError` 信封（`name()` 保留三个类名），
// 因此这里导出的是**别名**；`consume_sse_stream` 等模块级名字**不**进包根（§30.1）。
pub use crate::transports::streamable_http::{
    McpAuthRequiredError, McpHttpError, McpSessionExpiredError, StreamableHttpTransport,
    StreamableHttpTransportOptions,
};

// --- transports/transport.ts ------------------------------------------------
pub use crate::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions,
};
