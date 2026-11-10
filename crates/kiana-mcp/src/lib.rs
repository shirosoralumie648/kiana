//! 对应 packages/mcp：JSON-RPC、协议类型、stdio/streamable-http/in-memory 传输、OAuth。
//!
//! TS 源：packages/mcp/src/**
//!
//! 模块树与 `ARCHITECTURE.md` §5 一致（一个 TS 文件 → 一个 `.rs`；`oauth.rs`/`transports.rs`
//! 各自合并了多个 TS 文件）：
//!
//! | 文件 | TS 源 |
//! |---|---|
//! | [`auth_provider`] | `auth-provider.ts` + `fetch`/`Headers` 桥 |
//! | [`client`] | `client.ts` |
//! | [`index`] | `index.ts`（crate 根再导出面，逐字 67 个名字） |
//! | [`jsonrpc`] | `protocol/jsonrpc.ts` |
//! | [`oauth`] | `oauth/{callback,discovery,errors,flow,provider,types}.ts` |
//! | [`protocol_types`] | `protocol/types.ts` + `protocol/content.ts` |
//! | [`testing`] | `testing/index.ts` |
//! | [`transports`] | `transports/{transport,stdio,in-memory,streamable-http}.ts` |
//!
//! # 这个 crate 解决什么问题
//!
//! `packages/mcp` 是一个**独立的 Model Context Protocol 客户端**（`spec/telemetry-mcp.md` §1.2）。
//! 它把“coding agent 如何与外部 MCP 服务器对话”拆成四层，本 crate 逐层对应：
//!
//! 1. 线格式：[`jsonrpc`] 提供 JSON-RPC 2.0 消息结构、判别函数与 `McpError`；一次会话里的
//!    每一帧都是这类对象。
//! 2. 协议类型：[`protocol_types`] 提供 `Initialize*`、`Tool`、`Resource`、`CallToolResult`
//!    与内容块等 MCP 结构，以及内容块到模型内容的转换。
//! 3. 会话状态机：[`McpClient`] 维护连接状态、请求 id 与 pending 表、超时、取消、分页和
//!    通知分发。
//! 4. 传输与鉴权：[`transports`] 定义 `McpTransport` 接口并给出 stdio / in-memory /
//!    streamable-http 实现；[`auth_provider`] 把 HTTP 访问抽象成 `McpFetch`；[`oauth`] 实现
//!    discovery、PKCE、DCR 与令牌刷新。
//!
//! 没有它，agent 无法连接 `mcp.json` 里配置的 MCP 服务器，也就拿不到服务器提供的
//! tools/resources（实际调用点是 `kiana-builtin-extensions/src/mcp/{runtime,tools,resources}.rs`）。
//! 本 crate **只承担客户端角色**（spec §1.2 明确列出“不支持：服务端实现”）；[`testing`] 里的
//! `InMemoryTransport` 只是测试用的对端夹具，不是可部署的服务器。
//!
//! # 在系统中的位置
//!
//! - 上游（调用方）：`kiana-builtin-extensions` 的 `src/mcp/` 下六个文件
//!   （`runtime.rs`、`tools.rs`、`resources.rs`、`oauth.rs`、`cli.rs`、`index.rs`）及其集成测试。
//! - 下游（依赖）：`Cargo.toml` 里唯一的 workspace 内 crate 依赖是 `kiana-json`，用于复刻
//!   JS 的 JSON 语义（例如输出边界的 `JSON.stringify` 与键序，理由见 [`transports`] 的模块
//!   文档）。
//! - 冻结契约：`ARCHITECTURE.md` §4.9 把 [`jsonrpc`] 的 `parse_json_rpc_message` 判别顺序、
//!   `McpError` 与 [`protocol_types`] 的 `LATEST_PROTOCOL_VERSION` 列为跨 crate 契约；
//!   `tests/api_surface.rs` 用一份 `use` 清单在编译期断言这些公开名字存在且路径正确。
//!
//! # 核心概念与类型
//!
//! - [`McpClient`]：一次 MCP 会话的句柄，内部由 `Arc` 共享，克隆只是复制句柄、指向同一份
//!   会话状态。正常路径是 `Idle → Connecting → Connected`；`Idle`/`Connecting`/`Connected`
//!   任一状态都可通过 `close()`、握手失败（`connect` 内部随后调用 `close()`）或传输触发的
//!   `on_close` 进入终态 `Closed`。`Closed` 没有出边，因此**不能重连**，上层若需重连只能新建
//!   客户端（`runtime.rs` 的 `connect_once` 就是这么做的）。完整转换列表见
//!   [`client::ConnectionState`] 的文档。
//! - [`McpClientOptions`]：握手用的 `clientInfo`（`name`/`version`/`title`）、默认超时、协议
//!   版本与 `roots`。[`McpRequestOptions`]：单个请求的取消令牌、取消原因（`signal_reason`）、
//!   超时与进度回调；它的 `Clone` 是浅拷贝，令牌与回调都与原值共享同一个底层对象。
//! - [`transports::McpTransport`]：传输接口（`start`/`send`/`close` 与消息、错误、关闭三类
//!   监听器）；内置 `StdioTransport`（按行分帧的子进程）与 `StreamableHttpTransport`
//!   （HTTP + SSE）。
//! - [`auth_provider::McpTransportError`]：跨层统一的错误信封，`name()` 保留稳定的
//!   `error.name`；[`McpError`] 是四个 TS 错误类（JSON-RPC 错误、连接关闭、超时、取消）
//!   合并成的枚举，可通过 `From` 收进该信封。
//! - [`protocol_types`] 的结构体是“已知字段 + `extra` 兜底”，反序列化**宽松**：缺失的必填
//!   字段不会让整条消息失败。**未知键**与**可选字段**类型不符时原值留在 `extra` 里逐字
//!   往返；但必填字段（`uri`/`name`/`text`/`blob`/`inputSchema`/`capabilities`/`clientInfo`
//!   等）类型不符时会被 `String()`/空串/默认值改写，数字可选字段
//!   `ProgressNotification::total` 的非数字原值则既不进类型化字段也不进 `extra`（往返即
//!   丢失）。完整清单见 [`protocol_types`] 的「已知偏差」一节。
//!
//! # 一次典型调用
//!
//! 下面的顺序取自 `kiana-builtin-extensions/src/mcp/runtime.rs` 的连接流程：
//!
//! 1. 按配置构造传输并包成 `Arc<dyn McpTransport>`（`StdioTransport::new` 或
//!    `StreamableHttpTransport::new`）。
//! 2. `McpClient::new(McpClientOptions { name, version, request_timeout_ms, roots, .. })`。
//!    构造函数不发 IO，只预置内置请求 handler：`ping`，以及配置了 `roots` 时的 `roots/list`。
//! 3. `client.connect(transport).await`：安装传输监听器 → `transport.start()` → 发
//!    `initialize` → 校验并记录服务器能力 → `notifications/initialized`；任一步失败都会关闭
//!    客户端，最终停在 `Closed`。
//! 4. `client.list_tools(&McpRequestOptions::new()).await`：自动跟随 `nextCursor` 取完所有
//!    分页（实现里有重复 cursor 与页数上限的保护，见该方法文档）。
//! 5. `client.call_tool(name, args, &options).await` 得到 `CallToolResult`，需要时再用
//!    `to_llm_content` 转成模型内容。
//! 6. `client.close().await`（或传输 `on_close` 触发）：pending 请求以
//!    `McpConnectionClosedError` 结束，`Closed` 是终态。
//!
//! # 边界与失败模式
//!
//! - 会话与传输层的对外错误信封是 [`auth_provider::McpTransportError`]（`name()`/`status()`/
//!   `body()` 都是访问器方法，其中 `status()`/`body()` 仅在 HTTP 错误上有值）与 [`McpError`]
//!   （JSON-RPC 错误与请求生命周期）；工作区禁止 `anyhow`（`ARCHITECTURE.md` §4.1）。
//!   `oauth` 子系统另有自己的信封 [`oauth::errors::OAuthFailure`] 和五个错误结构体，离开
//!   `oauth` 时经 `From` 转成 [`auth_provider::McpTransportError`]（见 `oauth/errors.rs`
//!   的模块文档）。
//! - `connect` 只在 `Idle` 状态可用；其他状态会返回
//!   `Cannot connect MCP client in <state> state`。
//! - 出站请求在**调用方法时**就同步登记到 pending 表：drop 返回的 future 不会撤销已发出的
//!   请求；取消必须显式传 [`McpRequestOptions`] 的 `signal`。
//! - [`protocol`] 子模块只是 TS 文件树的路径别名，类型只有 [`protocol_types`] 一份；新增导出
//!   要同时改真实定义与别名列表，否则按 `kiana_mcp::protocol::*` 路径导入的调用方会编译失败。
//!
//! # 阅读顺序
//!
//! [`jsonrpc`] → [`protocol_types`] → [`transports`] → [`client`] → [`auth_provider`] /
//! [`oauth`] → [`index`] → [`testing`]。
pub mod auth_provider;
pub mod client;
pub mod index;
pub mod jsonrpc;
pub mod oauth;
pub mod protocol_types;
pub mod testing;
pub mod transports;

/// crate 根的再导出面 == `index.ts`（见 [`index`]）。
///
/// 这里用 glob，是因为 [`index`] 模块本身已按 `src/index.ts` 的分组逐名 `pub use`：根上出现的
/// 名字因此严格等于 TS 包根的冻结清单，不会把模块内部名字顺带泄漏出去。TS 的 67 个导出名里，
/// 4 个错误类在冻结契约中合并成 1 个 [`McpError`]，所以 Rust 侧是 64 个；
/// `tests/api_surface.rs` 逐个 `use` 校验这份清单。
pub use index::*;

/// `packages/mcp/src/protocol/` 的模块路径别名（TS 的文件树镜像）。
///
/// `ARCHITECTURE.md` §5 把 `protocol/types.ts` + `protocol/content.ts` 合进了一个文件
/// [`protocol_types`]，但跨单元契约（以及 TS 的 import 路径）用的是 `kiana_mcp::protocol::content`
/// 与 `kiana_mcp::protocol::types`。这里用**再导出模块**同时满足两者：
///
/// ```
/// use kiana_mcp::protocol::content::{CallToolResult, LlmContent, to_llm_content};
/// use kiana_mcp::protocol::types::LATEST_PROTOCOL_VERSION;
/// # let _ = (CallToolResult::default(), LlmContent::text(""), LATEST_PROTOCOL_VERSION);
/// ```
///
/// 名字与 `src/protocol/content.ts`、`src/protocol/types.ts` 的导出一致（Rust 侧把函数改成
/// snake_case）；两个模块只是别名，实现与类型只有一个（[`protocol_types`]），不存在两份定义。
pub mod protocol {
    /// `packages/mcp/src/protocol/content.ts`。
    pub mod content {
        pub use crate::protocol_types::{
            AudioContent, AudioKind, BlobResourceContents, CallToolResult, ContentAnnotations,
            ContentBlock, EmbeddedResourceContent, EmbeddedResourceKind, ImageContent, ImageKind,
            LlmContent, ResourceContents, ResourceLinkContent, ResourceLinkKind, TextContent,
            TextKind, TextResourceContents, block_to_llm_content, to_llm_content,
            try_block_to_llm_content, try_to_llm_content, try_to_llm_content_from_value,
        };
    }

    /// `packages/mcp/src/protocol/types.ts`。
    pub mod types {
        pub use crate::protocol_types::{
            Audience, CancelledNotification, ClientCapabilities, GetPromptResult, Implementation,
            InitializeParams, InitializeResult, LATEST_PROTOCOL_VERSION, ListChangedCapability,
            ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
            ProgressNotification, Prompt, PromptArgument, PromptMessage, PromptRole,
            ReadResourceResult, Resource, ResourceTemplate, ResourcesCapability, Root,
            SUPPORTED_PROTOCOL_VERSIONS, ServerCapabilities, SupportedProtocolVersion, TaskSupport,
            Tool, ToolAnnotations, ToolExecution, is_supported_protocol_version,
        };
    }
}
