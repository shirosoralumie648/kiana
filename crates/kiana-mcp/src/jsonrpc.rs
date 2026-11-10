//! `protocol/jsonrpc.ts`：JSON-RPC 2.0 消息与 MCP 错误类型。
//!
//! TS 用类型守卫（`isJsonRpcRequest` 等）在 `unknown` 上判别；Rust 用 [`JsonRpcMessage`] 的
//! untagged 枚举 + **手写**判别函数 [`parse_json_rpc_message`]，判别顺序固定为
//! （request → notification → response → 否则 `invalidRequest`）。
//!
//! 错误类的 `name` 必须保持稳定（`McpError` / `McpConnectionClosedError` / `McpTimeoutError` /
//! `AbortError`——注意 `McpAbortError` 的 name 是 `AbortError`，因为上层按 `name` 判断取消）。
//!
//! # 这个文件解决什么问题
//!
//! MCP 的每一帧线上数据都是 JSON-RPC 2.0 对象。TS 侧靠一组类型守卫在 `unknown` 上判断
//! 形状（是否带 `id` / `method` / `result` / `error`），本文件把这组谓词逐条移植成 Rust
//! 函数，并提供对应的类型化结构体。四个错误类在 Rust 里合并成一个 [`McpError`] 枚举，
//! 靠 `name()` 保留稳定的错误类名——外部调用方按这个名字区分错误种类。
//!
//! # 边界与调用方
//!
//! - 本模块只判断**外层形状**，不校验 `params` / `result` 的内容；具体方法的参数与返回值
//!   形状由 `client` 与 `protocol_types` 负责。
//! - `client` 的 `handle_message` 用 [`classify_json_rpc_message`]（response 优先）分派；
//!   `client::handle_response` 用 [`parse_json_rpc_message_typed`] 取结构化响应。
//! - `transports/stdio` 与 `transports/streamable_http` 在把入站帧交给监听器前用本模块的
//!   判别函数拒绝非法帧；stdio 使用 [`parse_json_rpc_message_value`] 以保留原始对象。
//! - `index` 按 `index.ts` 的导出面再导出部分名字；[`JsonRpcVersion`]、
//!   [`JsonRpcClassification`]、[`parse_json_rpc_message_value`] 不在 crate 根再导出，
//!   只按 `kiana_mcp::jsonrpc::…` 路径使用（与 TS 的包内可见性一致）。
//! - [`JsonRpcId`] 同时是客户端 `pending` / `progressRequests` / `incoming` 三张表的键，
//!   它的 `Eq` / `Hash` 必须保持 JS `Map` 的 SameValueZero 语义，不能改为派生实现。
//!
//! # 两种判别顺序
//!
//! 1. `parseJsonRpcMessage` 的顺序：request → notification → response 依次短路，任一成立
//!    即返回输入。本文件的 [`parse_json_rpc_message`] 与 [`JsonRpcMessage`] 的 `Deserialize`
//!    走这个顺序。
//! 2. 客户端 `handleMessage` 的顺序：response → request → notification → invalid。request
//!    与 response 谓词**不互斥**（同时带 `method` 与 `result`/`error` 的消息两边都成立），
//!    所以客户端必须用 [`classify_json_rpc_message`] / [`parse_json_rpc_message_typed`]，
//!    不能复用 1 的判别结果。
//!
//! # 保真约束
//!
//! - 四个错误 `name` 与固定消息文本是跨 crate 的冻结契约（`tests/api_surface.rs` 逐字断言；
//!   `spec/telemetry-mcp.md` §27.2）。
//! - `id` 与 `error.code` 必须按 ECMAScript `Number::toString` 写出（`1` 而不是 `1.0`）；
//!   否则线上字节会变化，`tests/wire.rs` 断言的是逐字文本（当前稳定契约，改动需同步测试）。
//! - TS 的 `parseJsonRpcMessage` 在类型守卫通过后**返回入参本身**（未知顶层键与键序都保留）。
//!   类型化枚举做不到这一点，需要逐字往返时用 [`parse_json_rpc_message_value`]。
//!
//! # 失败模式
//!
//! 判别失败统一返回 [`McpError::invalid_request`]（`code = -32600`，消息
//! `"Invalid JSON-RPC message"`）。调用方决定如何呈现：stdio 把它交给 error listener 后
//! 继续读下一行；streamable-http 的 JSON 响应体路径把错误返回给 `send` 的调用方。
//! 判别函数本身不 panic：`parse_response` 等处的 `expect` 都以对应谓词已通过为前提。
//!
//! # 测试
//!
//! 文件末尾的测试模块分别锁定：SameValueZero 键语义（含 `-0`、`NaN`）、两种判别顺序、
//! `params: null` 的保留、原始对象逐字往返，以及错误消息文本。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `JsonRpcId`：字符串或数字。
///
/// `PartialEq`/`Eq`/`Hash` 手写为 JS **SameValueZero** 语义（`spec/telemetry-mcp.md` §30.2）：
/// 数字 `1` 与字符串 `"1"` 是不同的键；`-0` 与 `0` 是同一个键；`NaN` 等于 `NaN`
/// （`is_json_rpc_id` 不接受 `NaN`，但客户端可以构造出它）。
/// 这是 `McpClient.pending` / `progressRequests` / `incoming` 三张 `Map` 的键语义。
/// `Serialize` 也是手写的：数字必须按 ECMAScript `Number::toString` 写出（`1` 而非 `1.0`）。
///
/// `Deserialize` 是派生的，`#[serde(untagged)]` 让它按变体声明序尝试：JSON 字符串进
/// [`JsonRpcId::String`]，JSON 数字进 [`JsonRpcId::Number`]（统一存 `f64`）；JSON 的 `null`、
/// 布尔、对象、数组都会被拒绝。判别合法性请用 [`JsonRpcId::is_valid`] 或 [`is_json_rpc_id`]，
/// 不要只看能否反序列化。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    /// 字符串形式的 id；`"1"` 与数字 `1` 是**不同**的键（SameValueZero）。
    String(String),
    /// 数字形式的 id；`-0` 与 `0` 视为同一个键，`NaN` 与 `NaN` 视为同一个键。
    Number(f64),
}

/// SameValueZero 的规范位：`-0` 归一成 `+0`，`NaN` 统一成一个位型。
fn same_value_zero_bits(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0f64.to_bits()
    } else {
        value.to_bits()
    }
}

impl PartialEq for JsonRpcId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (JsonRpcId::String(a), JsonRpcId::String(b)) => a == b,
            (JsonRpcId::Number(a), JsonRpcId::Number(b)) => {
                same_value_zero_bits(*a) == same_value_zero_bits(*b)
            }
            _ => false,
        }
    }
}

impl Eq for JsonRpcId {}

impl std::hash::Hash for JsonRpcId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            JsonRpcId::String(s) => {
                0u8.hash(state);
                s.hash(state);
            }
            JsonRpcId::Number(n) => {
                1u8.hash(state);
                same_value_zero_bits(*n).hash(state);
            }
        }
    }
}

impl JsonRpcId {
    /// `isJsonRpcId`：数字必须是有限值。
    ///
    /// 字符串恒合法；数字要求 `Number.isFinite`（`NaN` / `±Infinity` 为假）。与自由函数
    /// [`is_json_rpc_id`] 等价，区别只是作用于已经解析好的 id 而不是 `Value`。
    pub fn is_valid(&self) -> bool {
        match self {
            JsonRpcId::String(_) => true,
            JsonRpcId::Number(n) => n.is_finite(),
        }
    }
}

impl Serialize for JsonRpcId {
    /// 写出的数字必须是 ECMAScript `Number::toString` 的文本（`1` 而不是 serde 的 `1.0`），
    /// 否则请求 id 的线上字节会变化（§22.3 的键序与数字保真要求）。
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            JsonRpcId::String(s) => serializer.serialize_str(s),
            JsonRpcId::Number(n) => serialize_js_number(*n, serializer),
        }
    }
}

/// 以 JS 数字文本写出一个 `f64`（借助 `raw_value` 直接嵌入格式化后的数字）。
///
/// 非有限值写成 `null`：JSON 没有 `NaN` / `Infinity`，`JSON.stringify` 同样写 `null`。
/// 因此手工构造的 [`JsonRpcId::Number`] 若为 `NaN`，序列化结果是 `null` 而不是数字——
/// 这种 id 本来也通不过 [`JsonRpcId::is_valid`]。
fn serialize_js_number<S: serde::Serializer>(value: f64, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    let text = if value.is_finite() {
        kiana_json::js_num::format_js_number(value)
    } else {
        // JSON 里没有 NaN/Infinity（`JSON.stringify` 写 `null`）。
        "null".to_owned()
    };
    let raw = serde_json::value::RawValue::from_string(text).map_err(S::Error::custom)?;
    raw.serialize(serializer)
}

impl std::fmt::Display for JsonRpcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonRpcId::String(s) => f.write_str(s),
            JsonRpcId::Number(n) => f.write_str(&kiana_json::js_num::format_js_number(*n)),
        }
    }
}

/// `JsonRpcRequest`。
///
/// `jsonrpc` / `id` / `method` 是必需字段；`params` 可缺省，且“缺省”与“显式 `null`”是
/// 两种不同状态。序列化按结构体声明序写出 `jsonrpc,id,method,params`（`tests/wire.rs`
/// 断言逐字文本）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// 协议版本；线上只接受字符串 `"2.0"`。
    pub jsonrpc: JsonRpcVersion,
    /// 请求 id。必须满足 `isJsonRpcId`（字符串或有限数字），否则整个对象不算 request。
    pub id: JsonRpcId,
    /// 方法名，大小写敏感（如 `"tools/call"`）。
    pub method: String,
    /// `params?: unknown`。TS 里 `params: null` 是**存在且为 null**（`"params" in message` 为真），
    /// 因此显式 `null` 必须保留成 `Some(Value::Null)`（见 `deserialize_params`）。
    /// 序列化时 `None` 省略该键，`Some(Value::Null)` 写出 `"params":null`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_params"
    )]
    pub params: Option<Value>,
}

/// `JsonRpcNotification`。
///
/// 与请求的唯一结构差别是没有 `id`：TS 用 `!("id" in message)` 判断通知，因此
/// `id: null`（键存在）**不是**通知。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// 协议版本；线上只接受字符串 `"2.0"`。
    pub jsonrpc: JsonRpcVersion,
    /// 通知方法名，大小写敏感。
    pub method: String,
    /// 同 `JsonRpcRequest::params`：缺省 → `None`，显式 `null` → `Some(Value::Null)`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_params"
    )]
    pub params: Option<Value>,
}

/// `params` 的反序列化：字段缺失 → `None`，显式 `null` → `Some(Value::Null)`。
///
/// 派生 `Option<Value>` 会把 `null` 折成 `None`，从而丢掉 `"params":null`（TS 保留它，
/// `isJsonRpcNotification` 只看 `"id" in message`）。见 `spec/telemetry-mcp.md` §9。
///
/// 字段缺失由字段上的 `#[serde(default)]` 负责（走 `Option::default()` → `None`），
/// 只有键存在时才会调用本函数；所以这里无条件返回 `Some(..)` 是正确的，不要把它改成
/// “值为 null 时返回 None”，那会重新丢掉显式 `null`。
fn deserialize_params<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Ok(Some(Value::deserialize(deserializer)?))
}

/// `JsonRpcErrorObject`。
///
/// 响应里的 `error` 成员。`is_json_rpc_response` 只要求 `code` 是数字、`message` 是字符串，
/// 不检查 `code` 的具体取值，也不要求 `data` 存在。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcErrorObject {
    /// `code` 必须按 ECMAScript `Number::toString` 写出（`-32600` 而不是 serde 的 `-32600.0`）。
    ///
    /// TS 里是 `number`（f64）；`client` 转成 [`McpError::JsonRpc`] 的 `i64` 时会截断非整数，
    /// 见 `client` 模块文档的已知偏差。
    #[serde(serialize_with = "crate::protocol_types::serialize_js_number")]
    pub code: f64,
    /// 错误消息，原样透传（`McpError` 的 `Display` 只输出它）。
    pub message: String,
    /// 可选附加数据；`None` 时线上不写 `data` 键（TS 的 `undefined` 被 `JSON.stringify` 省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `JsonRpcSuccessResponse`。
///
/// 成功响应的 `result` 分支；`result` 与 `error` 互斥（两者同时出现时
/// `is_json_rpc_response` 判假）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcSuccessResponse {
    /// 协议版本；线上只接受字符串 `"2.0"`。
    pub jsonrpc: JsonRpcVersion,
    /// 对应请求的 id；调用方据此在 pending 表里找回等待中的请求。
    pub id: JsonRpcId,
    /// 方法返回值，形状由方法决定，允许为 `null`。
    pub result: Value,
}

/// `JsonRpcErrorResponse`。
///
/// 失败响应的 `error` 分支；`id` 同样必须通过 `isJsonRpcId`（所以 `id: null` 不被接受）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    /// 协议版本；线上只接受字符串 `"2.0"`。
    pub jsonrpc: JsonRpcVersion,
    /// 对应请求的 id；字符串与数字都接受，能否命中 pending 表由 SameValueZero 键比较决定。
    pub id: JsonRpcId,
    /// 错误对象（`code` / `message` / 可选 `data`）。
    pub error: JsonRpcErrorObject,
}

/// `JsonRpcResponse`。
///
/// `Deserialize` 是**手写**的：派生的 untagged 版本会接受
/// `{"jsonrpc":"2.0","id":1,"result":{},"error":{}}`（`result` 与 `error` 互斥，TS 的
/// `isJsonRpcResponse` 判假），也会放过 `error.code` 非数字之类的形状。这里直接复用
/// [`is_json_rpc_response`] + `parse_response`，判别规则与谓词保持一致。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    /// 带 `result` 的响应（且不能同时带 `error`）。
    Success(JsonRpcSuccessResponse),
    /// 带 `error` 的响应（`error` 是对象，`code` 为数字，`message` 为字符串）。
    Error(JsonRpcErrorResponse),
}

impl<'de> serde::Deserialize<'de> for JsonRpcResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = Value::deserialize(deserializer)?;
        if !is_json_rpc_response(&value) {
            return Err(D::Error::custom("Invalid JSON-RPC message"));
        }
        match parse_response(&value).map_err(D::Error::custom)? {
            JsonRpcMessage::Response(response) => Ok(response),
            _ => unreachable!("parse_response returns a response"),
        }
    }
}

/// `JsonRpcMessage`。
///
/// `Deserialize` 是**手写**的，走与 [`parse_json_rpc_message`] 完全相同的判别（request →
/// notification → response，见 §9），因此
/// `serde_json::from_value::<JsonRpcMessage>(…)` 拒绝的输入与 TS 的 `parseJsonRpcMessage` 一致
/// （派生的 untagged 版本会接受 `result` 与 `error` 同时存在的"响应"）。
///
/// 与 TS 的剩余差异（类型化枚举固有）：未知顶层键被丢弃；`serde_json::to_value` 的键序是
/// 声明序。需要逐字往返时用 [`parse_json_rpc_message_value`]。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    /// 带 `id` 与 `method` 的请求。
    Request(JsonRpcRequest),
    /// 没有 `id`、只有 `method` 的通知。
    Notification(JsonRpcNotification),
    /// 带 `result` 或 `error` 的响应。
    Response(JsonRpcResponse),
}

impl<'de> serde::Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = Value::deserialize(deserializer)?;
        parse_json_rpc_message(&value).map_err(D::Error::custom)
    }
}

/// 字面量 `"2.0"`。
///
/// TS 把 `jsonrpc` 字段声明成字面量类型 `"2.0"`；Rust 用只有一个变体的枚举表达，使
/// “版本只能是 2.0”成为类型约束。`#[serde(rename = "2.0")]` 保证线上写/读的都是字符串
/// `"2.0"`，而不是枚举变体名 `V2`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    /// 唯一的合法版本。
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

/// `JSON_RPC_ERROR_CODES`。
///
/// TS 里的 `as const` 对象；Rust 用关联常量保持点号访问（`JsonRpcErrorCodes::METHOD_NOT_FOUND`）
/// 与相同数值，crate 根再用别名 `JSON_RPC_ERROR_CODES` 对齐 TS 名。常量是 `i64`，而线上
/// `error.code` 是 `f64`（[`JsonRpcErrorObject::code`]），使用处需要显式转换。
/// 数值由 `tests/api_surface.rs` 逐字断言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonRpcErrorCodes;

impl JsonRpcErrorCodes {
    /// `parseError`：JSON 文本无法解析（-32700）。
    pub const PARSE_ERROR: i64 = -32700;
    /// `invalidRequest`：不是合法的 JSON-RPC 消息（-32600）；本模块判别失败时使用。
    pub const INVALID_REQUEST: i64 = -32600;
    /// `methodNotFound`：对端请求了未注册的方法（-32601）。
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// `invalidParams`：方法参数不合法（-32602）。
    pub const INVALID_PARAMS: i64 = -32602;
    /// `internalError`：handler 内部错误（-32603）。
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// MCP 错误。`name` 用于区分类型（上层按名字判断）。
///
/// TS 的四个错误类（`McpError` / `McpConnectionClosedError` / `McpTimeoutError` /
/// `McpAbortError`）在冻结契约里合并成这一个枚举，跨模块按错误种类分支时以 `name()` 为准：
/// 像 `AbortError` 这样的名字是对外契约（`McpTransportError::is_abort` 就转调
/// [`McpError::is_abort`]）。各变体只携带自己那份字段，访问器
/// （[`McpError::code`] / [`McpError::data`] / [`McpError::timeout_ms`]）对其它变体返回
/// `None`。`Display` 输出 TS 的 `error.message`，`Timeout` 的消息由 `timeout_ms` 拼出。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum McpError {
    /// `McpError`（JSON-RPC 层错误，带 code/data）。
    #[error("{message}")]
    JsonRpc {
        /// JSON-RPC 错误码（TS 是 `number`，冻结契约这里是 `i64`）。
        code: i64,
        /// 错误消息；`Display` 原样输出它。
        message: String,
        /// 可选 `data`；不参与 `Display`，由 [`McpError::data`] 读取。
        data: Option<Value>,
    },
    /// `McpConnectionClosedError`；字段 `message` 是关闭原因，默认构造函数给出
    /// `"MCP connection closed"`。
    #[error("{message}")]
    ConnectionClosed { message: String },
    /// `McpTimeoutError`；字段 `timeout_ms` 是超时毫秒数（冻结契约 `u64`，小数超时值
    /// 在构造时已被截断，见 `client` 模块文档）。
    #[error("MCP request timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
    /// `McpAbortError`；`name` 是 `AbortError`。字段 `message` 是取消原因，默认构造函数
    /// 给出 `"MCP request aborted"`。
    #[error("{message}")]
    Aborted { message: String },
}

impl McpError {
    /// `error.name`（逐字）。
    ///
    /// 四个返回值是 TS 错误对象 `.name` 的逐字复刻；`Aborted` 对应 `AbortError`，而不是
    /// 类名 `McpAbortError`。这个名字是对外契约（`client` 的 `mark_closed` 会把它写进取消
    /// 原因对象的 `"name"` 字段），不能改字面量。[`McpError::is_abort`] 用变体匹配实现、
    /// 不读取这个字符串；按 `"AbortError"` 做字符串比较的是 `McpTransportError::is_abort`
    /// 的 `Dom` 分支（`Mcp` 分支转调本枚举的 `is_abort`，见 `auth_provider.rs`）。
    pub fn name(&self) -> &'static str {
        match self {
            McpError::JsonRpc { .. } => "McpError",
            McpError::ConnectionClosed { .. } => "McpConnectionClosedError",
            McpError::Timeout { .. } => "McpTimeoutError",
            McpError::Aborted { .. } => "AbortError",
        }
    }

    /// `new McpError(code, "Invalid JSON-RPC message")`。
    pub fn invalid_request() -> Self {
        McpError::JsonRpc {
            code: JsonRpcErrorCodes::INVALID_REQUEST,
            message: "Invalid JSON-RPC message".to_owned(),
            data: None,
        }
    }

    /// `new McpConnectionClosedError()`。
    pub fn connection_closed() -> Self {
        McpError::ConnectionClosed {
            message: "MCP connection closed".to_owned(),
        }
    }

    /// `new McpAbortError()`。
    pub fn aborted() -> Self {
        McpError::Aborted {
            message: "MCP request aborted".to_owned(),
        }
    }

    /// 是否取消类错误（上层按 `name === "AbortError"` 判断）。
    pub fn is_abort(&self) -> bool {
        matches!(self, McpError::Aborted { .. })
    }

    /// `new McpError(code, message)`；`data` 缺省。
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        McpError::JsonRpc {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `new McpError(code, message, data)`。
    pub fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        McpError::JsonRpc {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    /// `new McpConnectionClosedError(message)`（`client.requireTransport` 用
    /// `` `MCP client is ${state}` ``）。
    pub fn connection_closed_with(message: impl Into<String>) -> Self {
        McpError::ConnectionClosed {
            message: message.into(),
        }
    }

    /// `new McpAbortError(message)`。
    pub fn aborted_with(message: impl Into<String>) -> Self {
        McpError::Aborted {
            message: message.into(),
        }
    }

    /// `new McpError(-32600, "Invalid MCP initialize result")`（`client.validateInitializeResult`）。
    pub fn invalid_initialize_result() -> Self {
        McpError::new(
            JsonRpcErrorCodes::INVALID_REQUEST,
            "Invalid MCP initialize result",
        )
    }

    /// `client.validateListPage` 的 `` `Invalid MCP ${method} result` ``。
    pub fn invalid_result(method: &str) -> Self {
        McpError::new(
            JsonRpcErrorCodes::INVALID_REQUEST,
            format!("Invalid MCP {method} result"),
        )
    }

    /// `client.validateListPage` 的 `` `Invalid entry in MCP ${method} result` ``。
    pub fn invalid_result_entry(method: &str) -> Self {
        McpError::new(
            JsonRpcErrorCodes::INVALID_REQUEST,
            format!("Invalid entry in MCP {method} result"),
        )
    }

    /// `client.validateListPage` 的 `` `Invalid MCP ${method} cursor` ``。
    pub fn invalid_cursor(method: &str) -> Self {
        McpError::new(
            JsonRpcErrorCodes::INVALID_REQUEST,
            format!("Invalid MCP {method} cursor"),
        )
    }

    /// `jsonrpc.ts` 的 `data` 字段（`McpError` 才有）。
    pub fn data(&self) -> Option<&Value> {
        match self {
            McpError::JsonRpc { data, .. } => data.as_ref(),
            _ => None,
        }
    }

    /// `jsonrpc.ts` 的 `code` 字段（只有 `McpError` 才有）。
    pub fn code(&self) -> Option<i64> {
        match self {
            McpError::JsonRpc { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// `timeoutMs` 字段（只有 `McpTimeoutError` 才有）。
    pub fn timeout_ms(&self) -> Option<u64> {
        match self {
            McpError::Timeout { timeout_ms } => Some(*timeout_ms),
            _ => None,
        }
    }
}

/// `toError(value)` 的 `String(v)` 那一路（`undefined` → `"undefined"`、对象 → `"[object Object]"`、
/// 数组用 `","` 连接，见 §22.2）。
///
/// TS 的 `toError(v)` 对 `instanceof Error` 原样返回；Rust 侧的"已是错误"分支由
/// [`crate::auth_provider::McpTransportError`] 的 `From` 实现承担，这里只负责值 → 文本。
///
/// 参数是 `&Value`，所以这里**没有** `undefined` 分支：JSON 的 `null` 输出 `"null"`。
/// 要表达 TS 的 `undefined`，用 [`to_error_message_opt`] 传 `None`。
pub fn to_error_message(value: &Value) -> String {
    kiana_json::js_string::js_string(Some(value))
}

/// `toError(value)`，`undefined` 用 `None` 表示。
///
/// `None` 输出 `"undefined"`，`Some(v)` 与 [`to_error_message`] 相同。
pub fn to_error_message_opt(value: Option<&Value>) -> String {
    kiana_json::js_string::js_string(value)
}

/// `toError(value)`：任意 JSON 值 → crate 统一的错误信封（`name` 为 `"Error"` 的那一路）。
///
/// 传输层用它把"捕获到的任意值"变成错误（§30.4 要求 listener 异常向上传播而不是被吞掉）。
///
/// 结果的 `name` 是 `"Error"`（不是 `TypeError`），消息是 `String(value)`；见
/// `tests` 里的 `to_error_tests`。
pub fn to_error(value: &Value) -> crate::auth_provider::McpTransportError {
    crate::auth_provider::McpTransportError::new(to_error_message(value))
}

/// `isObject(value)`。
///
/// 对应 TS 的 `typeof v === "object" && v !== null && !Array.isArray(v)`：只有 JSON 对象
/// 为真，`null`、数组、数字、字符串、布尔都为假。`oauth::types::object` 用它拒绝非对象输入。
pub fn is_object(value: &Value) -> bool {
    value.is_object()
}

/// `isJsonRpcId(value)`。
///
/// 字符串恒真；数字要求 `Number.isFinite`（`NaN` / `±Infinity` 为假，但 JSON 文本里本来
/// 也无法表示它们）。`null`、布尔、对象、数组一律为假。
pub fn is_json_rpc_id(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        _ => false,
    }
}

/// JSON 值 → [`JsonRpcId`]；不是合法 id（`isJsonRpcId` 为假）时返回 `None`。
///
/// TS 的 `JsonRpcId = string | number`；这里与 [`is_json_rpc_id`] 保持一致：只有字符串与有限
/// 数字可以转换。
///
/// 反方向（id → 线上值）由 [`JsonRpcId`] 的 `Serialize` 实现负责，数字同样按 ECMAScript
/// 文本写出；不要用 `serde_json::to_value(f64)` 直接转，那会得到 `1.0`。
pub fn value_to_json_rpc_id(value: &Value) -> Option<JsonRpcId> {
    match value {
        Value::String(id) => Some(JsonRpcId::String(id.clone())),
        Value::Number(number) => {
            let number = number.as_f64()?;
            number.is_finite().then_some(JsonRpcId::Number(number))
        }
        _ => None,
    }
}

fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

fn is_v2(value: &Value) -> bool {
    get(value, "jsonrpc") == Some(&Value::String("2.0".to_owned()))
}

/// `isJsonRpcRequest`。
///
/// 要求对象、`jsonrpc === "2.0"`、`id` 通过 `isJsonRpcId`、`method` 是字符串。只看形状，
/// 不检查 `params`；`id: null` 或缺少 `id` 都不算请求。
pub fn is_json_rpc_request(value: &Value) -> bool {
    is_object(value)
        && is_v2(value)
        && get(value, "id").is_some_and(is_json_rpc_id)
        && get(value, "method").is_some_and(Value::is_string)
}

/// `isJsonRpcNotification`。
///
/// 要求对象、`jsonrpc === "2.0"`、**没有** `id` 键（`"id" in m` 为假）、`method` 是字符串。
/// 注意判断的是“键是否存在”而不是值是否为空：`{"id":null,...}` 含 `id` 键，因此不是通知
/// （它也不是请求，因为 `null` 不是合法 id）。
pub fn is_json_rpc_notification(value: &Value) -> bool {
    is_object(value)
        && is_v2(value)
        && !value.as_object().is_some_and(|o| o.contains_key("id"))
        && get(value, "method").is_some_and(Value::is_string)
}

/// `isJsonRpcResponse`。
///
/// 要求对象、`jsonrpc === "2.0"`、`id` 通过 `isJsonRpcId`，然后按两个互斥分支判断：
///
/// 1. 有 `result` 键 → 只有**没有** `error` 键才为真（两者同时出现判假）；
/// 2. 否则 `error` 必须是对象，且 `code` 是数字、`message` 是字符串。
///
/// 两个键都没有时返回 `false`。这里不检查 `result`/`error.data` 的内容，也不要求
/// `id` 与某个待决请求对应（那是 `client` 的职责）。`code` 只做“是数字”的检查，
/// 因为 JSON 文本无法表示 `NaN`/`Infinity`。
pub fn is_json_rpc_response(value: &Value) -> bool {
    if !is_object(value) || !is_v2(value) || !get(value, "id").is_some_and(is_json_rpc_id) {
        return false;
    }
    let obj = value.as_object().expect("checked object");
    if obj.contains_key("result") {
        return !obj.contains_key("error");
    }
    let Some(error) = obj.get("error").filter(|e| e.is_object()) else {
        return false;
    };
    let err = error.as_object().expect("checked object");
    matches!(err.get("code"), Some(Value::Number(n)) if n.as_f64().is_some())
        && err.get("message").is_some_and(Value::is_string)
}

/// `parseJsonRpcMessage(value)` 的**逐字**版本：校验后原样返回传入的对象。
///
/// TS 的 `parseJsonRpcMessage` 通过类型守卫后 `return value`——返回的就是调用方传进来的同一个
/// 对象，因此未知键、键序、`params` 的原始形状都保留。类型化枚举 [`JsonRpcMessage`] 会重建对象
/// （丢掉未知的顶层键、按声明序写出），所以需要逐字往返的调用方（例如把客户端消息回显给服务端）
/// 应当用本函数；[`parse_json_rpc_message`] 保留给需要结构化字段的调用方。
///
/// 代价是返回一个 `Value` 的克隆，且只证明“形状合法”，不给出任何结构化字段。stdio 传输
/// 派发子进程 stdout 的每一帧前用它，监听器拿到的是该对象的克隆（`StdoutOutput::Message`）。
pub fn parse_json_rpc_message_value(value: &Value) -> Result<Value, McpError> {
    if is_json_rpc_request(value) || is_json_rpc_notification(value) || is_json_rpc_response(value)
    {
        return Ok(value.clone());
    }
    Err(McpError::invalid_request())
}

/// `parseJsonRpcMessage(value)`：按 request → notification → response 顺序判别，
/// 失败抛 `McpError(invalidRequest, "Invalid JSON-RPC message")`。
///
/// 返回**重建**的类型化枚举：顶层未知键会被丢弃、键序变成结构体声明序。TS 返回原对象
/// （见 [`parse_json_rpc_message_value`]）；传输层把原始 `&Value` 交给监听器，因此客户端路径
/// 不受影响。
///
/// 失败只有一种：[`McpError::invalid_request`]。谓词通过后函数内直接 `expect` 取字段，
/// 改动谓词条件时必须同步保证这些前提仍成立（否则会 panic）。
pub fn parse_json_rpc_message(value: &Value) -> Result<JsonRpcMessage, McpError> {
    if is_json_rpc_request(value) {
        return Ok(JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: parse_id(get(value, "id").expect("request has id")),
            method: value["method"].as_str().expect("checked string").to_owned(),
            params: value.get("params").cloned(),
        }));
    }
    if is_json_rpc_notification(value) {
        return Ok(JsonRpcMessage::Notification(JsonRpcNotification {
            jsonrpc: JsonRpcVersion::V2,
            method: value["method"].as_str().expect("checked string").to_owned(),
            params: value.get("params").cloned(),
        }));
    }
    if is_json_rpc_response(value) {
        let id = parse_id(get(value, "id").expect("response has id"));
        let obj = value.as_object().expect("checked object");
        if let Some(result) = obj.get("result") {
            return Ok(JsonRpcMessage::Response(JsonRpcResponse::Success(
                JsonRpcSuccessResponse {
                    jsonrpc: JsonRpcVersion::V2,
                    id,
                    result: result.clone(),
                },
            )));
        }
        let error = &obj["error"];
        return Ok(JsonRpcMessage::Response(JsonRpcResponse::Error(
            JsonRpcErrorResponse {
                jsonrpc: JsonRpcVersion::V2,
                id,
                error: JsonRpcErrorObject {
                    code: error["code"].as_f64().expect("checked number"),
                    message: error["message"]
                        .as_str()
                        .expect("checked string")
                        .to_owned(),
                    data: error.get("data").cloned(),
                },
            },
        )));
    }
    Err(McpError::invalid_request())
}

/// `McpClient.handleMessage` 的归类结果（**response 优先**，与
/// [`parse_json_rpc_message`] 的 request → notification → response 顺序不同，§9/§13.7）。
///
/// 两个谓词并不互斥：`{jsonrpc:"2.0",id:1,method:"m",result:{}}` 既是 request 也是 response，
/// 客户端按 response 处理（pending 里没有该 id → `Received response for unknown MCP request 1`）。
/// 因此归类**不能**靠 [`parse_json_rpc_message`] 的返回枚举，必须用这里的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonRpcClassification {
    /// 满足 `is_json_rpc_response`；优先级最高，重叠消息归到这里。
    Response,
    /// 不满足 response 谓词，但满足 `is_json_rpc_request`。
    Request,
    /// 前两者都不满足，但满足 `is_json_rpc_notification`。
    Notification,
}

/// 按客户端的顺序（response → request → notification）归类，无法归类时 `None`。
///
/// `None` 表示三个谓词都不成立；`client::handle_message` 把它转成
/// `-32600 "Received invalid JSON-RPC message"` 交给 error listener。
pub fn classify_json_rpc_message(value: &Value) -> Option<JsonRpcClassification> {
    if is_json_rpc_response(value) {
        return Some(JsonRpcClassification::Response);
    }
    if is_json_rpc_request(value) {
        return Some(JsonRpcClassification::Request);
    }
    if is_json_rpc_notification(value) {
        return Some(JsonRpcClassification::Notification);
    }
    None
}

/// [`parse_json_rpc_message`] 的 **response 优先**变体（`handleMessage` 的归类顺序）。
///
/// 与 `parse_json_rpc_message` 的唯一区别是重叠消息的归属：同时带 `method` 与
/// `result`/`error` 的消息归为 [`JsonRpcMessage::Response`]。失败时的错误相同
/// （[`McpError::invalid_request`]）。返回的变体与 [`classify_json_rpc_message`] 的结果一一
/// 对应：Response 分支只会产出 [`JsonRpcMessage::Response`]；`client::handle_response`
/// 依赖这一点从中取出 [`JsonRpcResponse`]，其它分支都按非法响应处理。
pub fn parse_json_rpc_message_typed(value: &Value) -> Result<JsonRpcMessage, McpError> {
    match classify_json_rpc_message(value) {
        Some(JsonRpcClassification::Response) => parse_response(value),
        Some(JsonRpcClassification::Request) => Ok(JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: parse_id(get(value, "id").expect("request has id")),
            method: value["method"].as_str().expect("checked string").to_owned(),
            params: value.get("params").cloned(),
        })),
        Some(JsonRpcClassification::Notification) => {
            Ok(JsonRpcMessage::Notification(JsonRpcNotification {
                jsonrpc: JsonRpcVersion::V2,
                method: value["method"].as_str().expect("checked string").to_owned(),
                params: value.get("params").cloned(),
            }))
        }
        None => Err(McpError::invalid_request()),
    }
}

/// 已确认是 response 的值 → [`JsonRpcMessage`]。
///
/// 前提：调用者已用 `is_json_rpc_response` 校验过（否则 `expect` 会 panic）。两个调用点
/// 分别是 `JsonRpcResponse` 的手写 `Deserialize` 与 [`parse_json_rpc_message_typed`]；
/// 函数内部不会产生 `Err`。逻辑与 [`parse_json_rpc_message`] 内联的 response 分支相同。
fn parse_response(value: &Value) -> Result<JsonRpcMessage, McpError> {
    let id = parse_id(get(value, "id").expect("response has id"));
    let obj = value.as_object().expect("checked object");
    if obj.contains_key("result") {
        return Ok(JsonRpcMessage::Response(JsonRpcResponse::Success(
            JsonRpcSuccessResponse {
                jsonrpc: JsonRpcVersion::V2,
                id,
                result: obj["result"].clone(),
            },
        )));
    }
    let error = &obj["error"];
    Ok(JsonRpcMessage::Response(JsonRpcResponse::Error(
        JsonRpcErrorResponse {
            jsonrpc: JsonRpcVersion::V2,
            id,
            error: JsonRpcErrorObject {
                code: error["code"].as_f64().expect("checked number"),
                message: error["message"]
                    .as_str()
                    .expect("checked string")
                    .to_owned(),
                data: error.get("data").cloned(),
            },
        },
    )))
}

/// 把已经通过 `isJsonRpcId` 的值转成 [`JsonRpcId`]。
///
/// 字符串原样保留；数字转 `f64`。字符串/数字以外的输入、以及数字转换失败的情况都退化为
/// `NaN`（[`JsonRpcId::is_valid`] 为假）——正常公开路径在调用前都做过谓词检查，不会走到
/// 兜底分支。
fn parse_id(value: &Value) -> JsonRpcId {
    match value {
        Value::String(s) => JsonRpcId::String(s.clone()),
        Value::Number(n) => JsonRpcId::Number(n.as_f64().unwrap_or(f64::NAN)),
        _ => JsonRpcId::Number(f64::NAN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_messages_like_typescript() {
        assert!(is_json_rpc_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"a"})
        ));
        assert!(is_json_rpc_notification(
            &json!({"jsonrpc":"2.0","method":"a"})
        ));
        assert!(is_json_rpc_response(
            &json!({"jsonrpc":"2.0","id":"x","result":null})
        ));
        assert!(!is_json_rpc_response(
            &json!({"jsonrpc":"2.0","id":"x","result":1,"error":{}})
        ));
        assert!(!is_json_rpc_response(
            &json!({"jsonrpc":"2.0","id":"x","error":{"code":"1","message":"m"}})
        ));
    }

    #[test]
    fn invalid_message_throws_invalid_request() {
        let err = parse_json_rpc_message(&json!({"jsonrpc":"1.0"})).unwrap_err();
        assert_eq!(err.name(), "McpError");
        assert_eq!(err.to_string(), "Invalid JSON-RPC message");
    }

    #[test]
    fn abort_error_name_is_plain_abort() {
        assert_eq!(McpError::aborted().name(), "AbortError");
        assert_eq!(
            McpError::Timeout { timeout_ms: 30 }.to_string(),
            "MCP request timed out after 30ms"
        );
    }
}

#[cfg(test)]
mod same_value_zero_tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_of(id: &JsonRpcId) -> u64 {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn number_one_differs_from_string_one() {
        assert_ne!(JsonRpcId::Number(1.0), JsonRpcId::String("1".to_owned()));
        assert_ne!(
            hash_of(&JsonRpcId::Number(1.0)),
            hash_of(&JsonRpcId::String("1".to_owned()))
        );
    }

    #[test]
    fn negative_zero_equals_positive_zero_and_hashes_equal() {
        assert_eq!(JsonRpcId::Number(-0.0), JsonRpcId::Number(0.0));
        assert_eq!(
            hash_of(&JsonRpcId::Number(-0.0)),
            hash_of(&JsonRpcId::Number(0.0))
        );
    }

    #[test]
    fn nan_equals_nan() {
        assert_eq!(JsonRpcId::Number(f64::NAN), JsonRpcId::Number(f64::NAN));
    }

    #[test]
    fn ids_work_as_hash_map_keys() {
        let mut map: std::collections::HashMap<JsonRpcId, &str> = std::collections::HashMap::new();
        map.insert(JsonRpcId::Number(1.0), "number");
        map.insert(JsonRpcId::String("1".to_owned()), "string");
        assert_eq!(map.get(&JsonRpcId::Number(1.0)), Some(&"number"));
        assert_eq!(map.get(&JsonRpcId::String("1".to_owned())), Some(&"string"));
        assert_eq!(map.len(), 2);
    }
}

#[cfg(test)]
mod classification_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_order_is_response_first() {
        // §9 的实测向量：request 与 response 谓词可以同时成立。
        let overlapping =
            json!({"jsonrpc":"2.0","id":1,"method":"m","error":{"code":-1,"message":"x"}});
        assert!(is_json_rpc_request(&overlapping));
        assert!(is_json_rpc_response(&overlapping));
        assert_eq!(
            classify_json_rpc_message(&overlapping),
            Some(JsonRpcClassification::Response)
        );
        assert!(matches!(
            parse_json_rpc_message_typed(&overlapping).unwrap(),
            JsonRpcMessage::Response(JsonRpcResponse::Error(_))
        ));
        // 旧的判别顺序仍按 request 归类（parseJsonRpcMessage 只判断"任一成立"）。
        assert!(matches!(
            parse_json_rpc_message(&overlapping).unwrap(),
            JsonRpcMessage::Request(_)
        ));
    }

    #[test]
    fn result_and_error_together_is_no_response() {
        let both = json!({"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-1,"message":"x"}});
        assert!(!is_json_rpc_response(&both));
        assert!(!is_json_rpc_request(&both));
        assert!(!is_json_rpc_notification(&both));
        assert!(classify_json_rpc_message(&both).is_none());
        assert!(parse_json_rpc_message(&both).is_err());
    }

    #[test]
    fn id_must_be_finite_for_requests_and_responses() {
        // 测试向量里 `id: NaN` / `id: undefined`（键存在）都全 false。
        assert!(!is_json_rpc_request(&json!({"jsonrpc":"2.0","method":"m"})));
        assert!(!is_json_rpc_notification(
            &json!({"jsonrpc":"2.0","id":null,"method":"m"})
        ));
    }

    #[test]
    fn response_ids_serialize_as_js_numbers() {
        let response = JsonRpcMessage::Response(JsonRpcResponse::Error(JsonRpcErrorResponse {
            jsonrpc: JsonRpcVersion::V2,
            id: JsonRpcId::Number(1.0),
            error: JsonRpcErrorObject {
                code: -32601.0,
                message: "nope".to_owned(),
                data: None,
            },
        }));
        assert_eq!(
            kiana_json::js_object::stringify(&serde_json::to_value(&response).unwrap()),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#
        );
    }

    #[test]
    fn string_ids_round_trip() {
        let value = json!({"jsonrpc":"2.0","id":"abc","result":null});
        let message = parse_json_rpc_message(&value).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap()["id"], json!("abc"));
    }

    /// 回归测试（评审 finding #7）：TS 的 `parseJsonRpcMessage` 直接返回原对象，
    /// 未知顶层键与键序都保留。`parse_json_rpc_message_value` 复刻这一点。
    #[test]
    fn raw_parse_preserves_unknown_keys_and_key_order() {
        let values = [
            json!({"jsonrpc":"2.0","id":1,"method":"m","params":{"a":1},"extra":true,"_meta":{"x":1}}),
            json!({"jsonrpc":"2.0","id":"a","result":{"z":1},"ts":5}),
            json!({"jsonrpc":"2.0","method":"m","params":{"b":2},"foo":"bar"}),
            json!({"jsonrpc":"2.0","id":7,"error":{"code":-1,"message":"x"},"trace":"t"}),
        ];
        for value in values {
            let parsed = parse_json_rpc_message_value(&value).expect("valid message");
            assert_eq!(
                kiana_json::js_object::stringify(&parsed),
                kiana_json::js_object::stringify(&value),
                "value: {value}"
            );
        }
        assert!(parse_json_rpc_message_value(&json!({"jsonrpc":"1.0"})).is_err());
    }
}

#[cfg(test)]
mod to_error_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_of_values_matches_ecmascript() {
        // 向量来自 node 26：`String(v)`（probe: scratch/kiana-mcp-core-protocol/toerr.mjs）。
        assert_eq!(to_error_message_opt(None), "undefined");
        assert_eq!(to_error_message(&Value::Null), "null");
        assert_eq!(to_error_message(&json!(true)), "true");
        assert_eq!(to_error_message(&json!(0)), "0");
        assert_eq!(to_error_message(&json!(-0.0)), "0");
        assert_eq!(to_error_message(&json!(1.5)), "1.5");
        assert_eq!(to_error_message(&json!("abc")), "abc");
        assert_eq!(to_error_message(&json!([1, [2, 3]])), "1,2,3");
        assert_eq!(to_error_message(&json!([])), "");
        assert_eq!(to_error_message(&json!([null, 1])), ",1");
        assert_eq!(to_error_message(&json!({})), "[object Object]");
        assert_eq!(to_error_message(&json!({"a": 1})), "[object Object]");
        assert_eq!(to_error_message(&json!(1e21)), "1e+21");
        assert_eq!(to_error_message(&json!(1e-7)), "1e-7");
    }

    #[test]
    fn to_error_wraps_the_message_as_a_plain_error() {
        let error = to_error(&json!({"a": 1}));
        assert_eq!(error.name(), "Error");
        assert_eq!(error.to_string(), "[object Object]");
    }

    #[test]
    fn error_constructors_keep_typescript_messages() {
        assert_eq!(
            McpError::invalid_initialize_result().to_string(),
            "Invalid MCP initialize result"
        );
        assert_eq!(
            McpError::invalid_result("tools/list").to_string(),
            "Invalid MCP tools/list result"
        );
        assert_eq!(
            McpError::invalid_result_entry("resources/list").to_string(),
            "Invalid entry in MCP resources/list result"
        );
        assert_eq!(
            McpError::invalid_cursor("tools/list").to_string(),
            "Invalid MCP tools/list cursor"
        );
        assert_eq!(
            McpError::connection_closed_with("MCP client is connecting").to_string(),
            "MCP client is connecting"
        );
        assert_eq!(McpError::aborted_with("Aborted").name(), "AbortError");
        assert_eq!(McpError::aborted_with("Aborted").to_string(), "Aborted");
        assert_eq!(
            McpError::new(-32601, "Method not found: x").code(),
            Some(-32601)
        );
        assert_eq!(
            McpError::with_data(1234, "tool failed", json!({"retryable": false})).data(),
            Some(&json!({"retryable": false}))
        );
        assert_eq!(McpError::Timeout { timeout_ms: 30 }.timeout_ms(), Some(30));
        assert_eq!(
            McpError::connection_closed().name(),
            "McpConnectionClosedError"
        );
    }
}

#[cfg(test)]
mod deserialize_tests {
    use super::*;
    use serde_json::json;

    /// 回归测试（评审 finding #7）：派生 `Deserialize` 会接受 TS 谓词拒绝的文档。
    /// 现在 `serde_json::from_value::<JsonRpcMessage>` 走与 `parse_json_rpc_message` 相同的
    /// 判别（request → notification → response）。
    #[test]
    fn derived_path_rejects_result_and_error_together() {
        let both = json!({"jsonrpc":"2.0","id":1,"result":{},"error":{}});
        assert!(!is_json_rpc_response(&both));
        assert!(
            serde_json::from_value::<JsonRpcMessage>(both.clone()).is_err(),
            "result+error must not deserialize as a response"
        );
        assert!(serde_json::from_value::<JsonRpcResponse>(both).is_err());
    }

    /// `params: null` 在 TS 里是"键存在且为 null"；派生 `Option<Value>` 会把它折成缺失。
    #[test]
    fn explicit_null_params_are_preserved() {
        let value = json!({"jsonrpc":"2.0","id":1,"method":"m","params":null});
        let message: JsonRpcMessage = serde_json::from_value(value).expect("valid request");
        let JsonRpcMessage::Request(request) = &message else {
            panic!("expected a request, got {message:?}");
        };
        assert_eq!(request.params, Some(Value::Null));
        assert_eq!(
            kiana_json::js_object::stringify(&serde_json::to_value(&message).unwrap()),
            r#"{"jsonrpc":"2.0","id":1,"method":"m","params":null}"#
        );

        let value = json!({"jsonrpc":"2.0","method":"m","params":null});
        let message: JsonRpcMessage = serde_json::from_value(value).expect("valid notification");
        assert_eq!(
            kiana_json::js_object::stringify(&serde_json::to_value(&message).unwrap()),
            r#"{"jsonrpc":"2.0","method":"m","params":null}"#
        );
    }

    /// 判别顺序与 `parse_json_rpc_message` 一致（request → notification → response）。
    #[test]
    fn deserialize_uses_the_documented_classification_order() {
        let overlapping = json!({"jsonrpc":"2.0","id":1,"method":"m","result":{}});
        let message: JsonRpcMessage = serde_json::from_value(overlapping).expect("valid message");
        assert!(
            matches!(message, JsonRpcMessage::Request(_)),
            "parseJsonRpcMessage checks isJsonRpcRequest first"
        );
        assert!(
            serde_json::from_value::<JsonRpcMessage>(json!({"jsonrpc":"1.0"})).is_err(),
            "non-2.0 documents are rejected"
        );
        let error = serde_json::from_value::<JsonRpcMessage>(json!({"jsonrpc":"1.0"})).unwrap_err();
        assert!(error.to_string().contains("Invalid JSON-RPC message"));
    }
}
