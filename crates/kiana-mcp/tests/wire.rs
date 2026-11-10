//! 线上字节形状：`JSON.stringify(message)` 的键序与数字格式。
//!
//! 黄金值由 TS 参考实现在 node 26 上跑出（`/home/ctyun/kiana-work/scratch/kiana-mcp-core-protocol/wire.mjs`，
//! `createInMemoryTransportPair` + `McpClient`，在服务端监听器里记录每条入站消息的
//! `JSON.stringify`）。服务器可能比较原始字节，所以这里断言的是**逐字文本**。

use kiana_mcp::jsonrpc::{
    JsonRpcErrorObject, JsonRpcErrorResponse, JsonRpcId, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, JsonRpcSuccessResponse, JsonRpcVersion,
};
use kiana_mcp::protocol_types::{
    ClientCapabilities, Implementation, InitializeParams, LATEST_PROTOCOL_VERSION,
};
use serde_json::{Map, Value, json};

fn stringify(message: &JsonRpcMessage) -> String {
    kiana_json::js_object::stringify(&serde_json::to_value(message).expect("message serializes"))
}

fn request(id: f64, method: &str, params: Option<Value>) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: JsonRpcVersion::V2,
        id: JsonRpcId::Number(id),
        method: method.to_owned(),
        params,
    })
}

#[test]
fn client_requests_match_the_typescript_wire_format() {
    // initialize：params 的键序 protocolVersion, capabilities, clientInfo（TS 的对象字面量序）。
    let mut capabilities = ClientCapabilities::default();
    assert!(capabilities.extra.is_empty());
    let _ = &mut capabilities;
    let init = InitializeParams {
        protocol_version: LATEST_PROTOCOL_VERSION.to_owned(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation::new("kiana-test", "1.2.3"),
        extra: Map::new(),
    };
    assert_eq!(
        stringify(&request(1.0, "initialize", Some(json!(init)))),
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"kiana-test","version":"1.2.3"}}}"#
    );

    // 通知：没有 id，也没有 params。
    let notification = JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/initialized".to_owned(),
        params: None,
    });
    assert_eq!(
        stringify(&notification),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
    );

    // tools/call：name 在 arguments 之前。
    assert_eq!(
        stringify(&request(
            2.0,
            "tools/call",
            Some(json!({"name": "echo", "arguments": {"text": "hi"}}))
        )),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#
    );

    // 无 params 的请求不带 `params` 键。
    assert_eq!(
        stringify(&request(3.0, "tools/list", None)),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#
    );
    assert_eq!(
        stringify(&request(4.0, "ping", None)),
        r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#
    );

    // 带 params 的通知。
    let notification = JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "notifications/cancelled".to_owned(),
        params: Some(json!({"requestId": 7, "reason": "r"})),
    });
    assert_eq!(
        stringify(&notification),
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7,"reason":"r"}}"#
    );
}

#[test]
fn responses_match_the_typescript_wire_format() {
    let success = JsonRpcMessage::Response(JsonRpcResponse::Success(JsonRpcSuccessResponse {
        jsonrpc: JsonRpcVersion::V2,
        id: JsonRpcId::Number(2.0),
        result: json!({"content": [{"type": "text", "text": "ok"}]}),
    }));
    assert_eq!(
        stringify(&success),
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"ok"}]}}"#
    );

    let error = JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
        jsonrpc: JsonRpcVersion::V2,
        id: JsonRpcId::String("abc".to_owned()),
        error: JsonRpcErrorObject {
            code: -32601.0,
            message: "Method not found: x".to_owned(),
            data: Some(json!({"retryable": false})),
        },
    }));
    assert_eq!(
        stringify(&error),
        r#"{"jsonrpc":"2.0","id":"abc","error":{"code":-32601,"message":"Method not found: x","data":{"retryable":false}}}"#
    );

    // 没有 data 时不写 `data` 键（TS 的 undefined 被省略）。
    let error = JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
        jsonrpc: JsonRpcVersion::V2,
        id: JsonRpcId::Number(1.0),
        error: JsonRpcErrorObject {
            code: -32600.0,
            message: "Invalid JSON-RPC message".to_owned(),
            data: None,
        },
    }));
    assert_eq!(
        stringify(&error),
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid JSON-RPC message"}}"#
    );
}

#[test]
fn integer_like_keys_are_reordered_at_the_output_boundary() {
    // serde_json 的 preserve_order 保插入序，JS 的整数键规则由 `sort_js_keys` 在输出边界补上。
    let message = request(1.0, "m", Some(json!({"b": 1, "2": 2, "a": 3, "1": 4})));
    assert_eq!(
        stringify(&message),
        r#"{"jsonrpc":"2.0","id":1,"method":"m","params":{"1":4,"2":2,"b":1,"a":3}}"#
    );
}
