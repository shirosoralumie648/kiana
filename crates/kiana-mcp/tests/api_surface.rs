//! 公开 API 面：`spec/telemetry-mcp.md` §19.2 的 113 个名字（67 + 44 + 2）与 §30.1 的模块级名字。
//!
//! 这里的 `use` 列表是**编译期**断言：任何一个名字缺失、大小写不一致或路径不对，本测试都无法编译。
//! 运行时部分逐字核对常量与错误 `name`/消息（§27.2、§27.3）。
//!
//! TS 的四个错误类（`McpError`/`McpConnectionClosedError`/`McpTimeoutError`/`McpAbortError`）
//! 在冻结契约里合并成一个 [`McpError`] 枚举，`name()` 逐字保留四个类名，因此
//! `mcp/src/index.ts` 的 67 个名字在 crate 根是 64 个（见 `src/index.rs` 的文档）。

#![allow(unused_imports)]

// crate 根：`mcp/src/index.ts` 的 64 个 Rust 名字。
use kiana_mcp::{
    AudioContent, AuthProvider, BlobResourceContents, CallToolResult, CancelledNotification,
    ClientCapabilities, ContentAnnotations, ContentBlock, EmbeddedResourceContent, ImageContent,
    Implementation, InitializeParams, InitializeResult, JSON_RPC_ERROR_CODES, JsonRpcErrorObject,
    JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, JsonRpcSuccessResponse, LATEST_PROTOCOL_VERSION, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, LlmContent, McpAuthRequiredError, McpClient,
    McpClientOptions, McpError, McpFetch, McpHttpError, McpRequestOptions, McpSessionExpiredError,
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, ProgressNotification, ReadResourceResult,
    Resource, ResourceLinkContent, ResourceTemplate, Root, SUPPORTED_PROTOCOL_VERSIONS,
    ServerCapabilities, StdioTransport, StdioTransportOptions, StreamableHttpTransport,
    StreamableHttpTransportOptions, SupportedProtocolVersion, TextContent, TextResourceContents,
    Tool, ToolAnnotations, ToolExecution, UnauthorizedContext, is_json_rpc_notification,
    is_json_rpc_request, is_json_rpc_response, parse_json_rpc_message, to_llm_content,
};

// `kiana_mcp::oauth`：`mcp/src/oauth/index.ts` 的 44 个名字。
use kiana_mcp::oauth::{
    AddClientAuthentication, AuthorizationServerMetadata, McpOAuthAuthorizationRequiredError,
    McpOAuthProvider, McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore,
    MemoryOAuthStateStore, OAuthCallback, OAuthCallbackPage, OAuthCallbackServer,
    OAuthCallbackServerOptions, OAuthChallenge, OAuthClientInformation, OAuthClientInformationFull,
    OAuthClientInformationMixed, OAuthClientMetadata, OAuthClientMetadataDocument,
    OAuthClientProvider, OAuthDiscoveryState, OAuthError, OAuthFlowOptions, OAuthFlowResult,
    OAuthInsecureEndpointError, OAuthIssuerMismatchError, OAuthProtectedResourceMetadata,
    OAuthRegistrationError, OAuthServerInfo, OAuthTokens, TokenRequestOptions,
    adapt_oauth_provider, authorize_mcp, build_authorization_server_discovery_urls,
    discover_authorization_server_metadata, discover_oauth_server_info,
    discover_protected_resource_metadata, exchange_authorization_code, parse_www_authenticate,
    refresh_authorization, register_client, resource_url_from_server_url, select_resource,
    start_authorization, step_up_scope,
};

// `kiana_mcp::testing`：`mcp/src/testing/index.ts` 的 2 个名字。
use kiana_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};

// §30.1：包内但不在包根的模块级名字（TS 测试按相对路径 import）。
use kiana_mcp::client::{DEFAULT_REQUEST_TIMEOUT_MS, MAX_LIST_PAGES};
use kiana_mcp::jsonrpc::{is_json_rpc_id, is_object, to_error};
use kiana_mcp::oauth::callback::OAuthCallbackServer as _CallbackServerByPath;
use kiana_mcp::oauth::discovery::{discard, fetch_metadata, field, is_discovery_miss, path_suffix};
use kiana_mcp::oauth::flow::{
    TokenRequestOptions as _TokenRequestOptionsByPath, application_type,
    apply_client_authentication, loopback, pkce, run_flow, secure_endpoint,
    select_client_auth_method, token_request, with_scope,
};
use kiana_mcp::oauth::types::{
    absent, compact, is_http_url, object, optional_string, optional_strings,
    parse_authorization_server_metadata, parse_client_information, parse_oauth_tokens,
    parse_protected_resource_metadata, required_string, safe_url,
};
use kiana_mcp::protocol::content::{
    CallToolResult as _CallToolResultByPath, to_llm_content as _to_llm,
};
use kiana_mcp::protocol::types::LATEST_PROTOCOL_VERSION as _LATEST_BY_PATH;
use kiana_mcp::transports::streamable_http::{
    ConsumeSseOptions, DEFAULT_RECONNECT_INITIAL_DELAY_MS, DEFAULT_RECONNECT_MAX_DELAY_MS,
    DEFAULT_RECONNECT_MAX_RETRIES, ERROR_MESSAGE_BODY_CHARS, MAX_ERROR_BODY_BYTES, SseEvent,
    StreamableHttpReconnectOptions, consume_sse_stream,
};
use kiana_mcp::transports::{DEFAULT_MAX_MESSAGE_BYTES, TransportEvents};

use serde_json::json;

#[test]
fn protocol_constants_match_the_typescript_source() {
    // protocol/types.ts: LATEST_PROTOCOL_VERSION / SUPPORTED_PROTOCOL_VERSIONS。
    assert_eq!(LATEST_PROTOCOL_VERSION, "2025-11-25");
    assert_eq!(
        SUPPORTED_PROTOCOL_VERSIONS,
        ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]
    );
    // 类型层面：四个变体都能构造（大小写一致）。
    let _versions = [
        SupportedProtocolVersion::V2025_11_25,
        SupportedProtocolVersion::V2025_06_18,
        SupportedProtocolVersion::V2025_03_26,
        SupportedProtocolVersion::V2024_11_05,
    ];
}

#[test]
fn json_rpc_error_codes_match_the_typescript_object() {
    // JSON_RPC_ERROR_CODES = { parseError: -32700, invalidRequest: -32600,
    //   methodNotFound: -32601, invalidParams: -32602, internalError: -32603 }.
    assert_eq!(JSON_RPC_ERROR_CODES::PARSE_ERROR, -32700);
    assert_eq!(JSON_RPC_ERROR_CODES::INVALID_REQUEST, -32600);
    assert_eq!(JSON_RPC_ERROR_CODES::METHOD_NOT_FOUND, -32601);
    assert_eq!(JSON_RPC_ERROR_CODES::INVALID_PARAMS, -32602);
    assert_eq!(JSON_RPC_ERROR_CODES::INTERNAL_ERROR, -32603);
}

#[test]
fn mcp_error_names_and_messages_match_the_typescript_classes() {
    // §27.2：四个 TS 类的 name / 消息。
    let jsonrpc = McpError::new(-32600, "Invalid JSON-RPC message");
    assert_eq!(jsonrpc.name(), "McpError");
    assert_eq!(jsonrpc.to_string(), "Invalid JSON-RPC message");
    assert_eq!(jsonrpc.code(), Some(-32600));

    let closed = McpError::connection_closed();
    assert_eq!(closed.name(), "McpConnectionClosedError");
    assert_eq!(closed.to_string(), "MCP connection closed");

    let closed_with = McpError::connection_closed_with("MCP client is closed");
    assert_eq!(closed_with.name(), "McpConnectionClosedError");
    assert_eq!(closed_with.to_string(), "MCP client is closed");

    let timeout = McpError::Timeout { timeout_ms: 30_000 };
    assert_eq!(timeout.name(), "McpTimeoutError");
    assert_eq!(timeout.to_string(), "MCP request timed out after 30000ms");

    let aborted = McpError::aborted();
    assert_eq!(aborted.name(), "AbortError");
    assert_eq!(aborted.to_string(), "MCP request aborted");
    assert!(aborted.is_abort());

    // data 只在显式传入时存在。
    let with_data = McpError::with_data(-32601, "nope", json!({"a": 1}));
    assert_eq!(with_data.data(), Some(&json!({"a": 1})));
}

#[test]
fn http_error_names_status_and_messages_match_the_typescript_classes() {
    // §27.2：McpHttpError / McpAuthRequiredError / McpSessionExpiredError。
    let http = McpHttpError::http(500, "MCP HTTP request failed with status 500", "boom");
    assert_eq!(http.name(), "McpHttpError");
    assert_eq!(http.status(), Some(500));
    assert_eq!(http.body(), Some("boom"));
    assert_eq!(http.to_string(), "MCP HTTP request failed with status 500");

    let auth = McpAuthRequiredError::auth_required(Some("Bearer".to_owned()), "denied");
    assert_eq!(auth.name(), "McpAuthRequiredError");
    assert_eq!(auth.status(), Some(401));
    assert_eq!(auth.to_string(), "MCP server requires authentication");
    assert_eq!(auth.www_authenticate(), Some("Bearer"));
    assert!(auth.is_auth_required());

    let expired = McpSessionExpiredError::session_expired("gone");
    assert_eq!(expired.name(), "McpSessionExpiredError");
    assert_eq!(expired.status(), Some(404));
    assert_eq!(expired.to_string(), "MCP session expired");
    assert!(expired.is_session_expired());
}

#[test]
fn transport_and_client_defaults_match_the_typescript_source() {
    assert_eq!(DEFAULT_MAX_MESSAGE_BYTES, 16 * 1024 * 1024);
    assert_eq!(DEFAULT_REQUEST_TIMEOUT_MS, 30_000.0);
    assert_eq!(MAX_LIST_PAGES, 1_000);
    assert_eq!(MAX_ERROR_BODY_BYTES, 8 * 1024);
    assert_eq!(ERROR_MESSAGE_BODY_CHARS, 500);
    assert_eq!(DEFAULT_RECONNECT_INITIAL_DELAY_MS, 1_000);
    assert_eq!(DEFAULT_RECONNECT_MAX_DELAY_MS, 30_000);
    assert_eq!(DEFAULT_RECONNECT_MAX_RETRIES, 5);
}

/// 只引用类型/函数，确保它们作为**值**也存在（防止 `pub type` 之外的意外缺失）。
#[test]
fn exported_items_are_usable() {
    let _message: JsonRpcMessage =
        parse_json_rpc_message(&json!({"jsonrpc": "2.0", "method": "ping"}))
            .expect("notification parses");
    assert!(is_json_rpc_request(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
    ));
    assert!(is_json_rpc_notification(
        &json!({"jsonrpc": "2.0", "method": "ping"})
    ));
    assert!(is_json_rpc_response(
        &json!({"jsonrpc": "2.0", "id": 1, "result": null})
    ));
    assert!(is_object(&json!({})));
    assert!(!is_object(&json!([])));
    assert!(is_json_rpc_id(&json!(1.5)));
    assert!(!is_json_rpc_id(&json!(null)));
    let _error: McpTransportErrorListener = std::sync::Arc::new(|_| Ok(()));
    let _message_listener: McpTransportMessageListener = std::sync::Arc::new(|_| Ok(()));
    let _close_listener: McpTransportCloseListener = std::sync::Arc::new(|| Ok(()));
    let _send_options = McpTransportSendOptions::none();
    let _content: Vec<LlmContent> = to_llm_content(&CallToolResult::default());
    // OAuth 辅助函数的名字与形态。
    assert!(!loopback("example.com"));
    assert_eq!(application_type(&["http://127.0.0.1/cb"]), "native");
    let _ = with_scope(OAuthTokens::default(), Some("org:read"));
    assert_eq!(step_up_scope(Some("a"), Some("b")), Some("a b".to_owned()));
    // 逐个取函数项（不能用大元组：`impl AsRef<str>` 的实参是匿名泛型，元组里无法推断）。
    let _ = safe_url;
    let _ = object;
    let _ = compact;
    let _ = required_string;
    let _ = absent;
    let _ = optional_string;
    let _ = optional_strings;
    let _ = is_http_url;
    let _ = parse_protected_resource_metadata;
    let _ = parse_authorization_server_metadata;
    let _ = parse_oauth_tokens;
    let _ = parse_client_information;
    let _ = select_client_auth_method;
    let _ = apply_client_authentication;
    let _ = secure_endpoint;
    let _ = pkce;
    // `token_request` 的实参是 `impl AsRef<str>`（匿名泛型），不能取函数项；构造一个
    // 不会被 poll 的 future 即可确认名字、参数与返回类型。
    let _token_future = token_request(
        "http://127.0.0.1:9/token",
        TokenRequestOptions::default(),
        Vec::new(),
    );
    let _ = run_flow;
    let _ = path_suffix;
    let _ = is_discovery_miss;
    let _ = field;
    let _ = fetch_metadata;
    let _ = discard;
    // SSE 解析器与传输事件基类。
    let _sse_item = consume_sse_stream;
    let _sse_event: Option<SseEvent> = None;
    let _sse_options: Option<ConsumeSseOptions> = None;
    let _reconnect: Option<StreamableHttpReconnectOptions> = None;
    let _events: Option<TransportEvents> = None;
    // 三个入口的类型与工厂。
    let _pair: fn() -> (InMemoryTransport, InMemoryTransport) =
        || create_in_memory_transport_pair().into_parts();
    let _client: Option<McpClient> = None;
    let _provider: Option<McpOAuthProvider> = None;
    let _store: Option<MemoryOAuthStateStore> = None;
}
