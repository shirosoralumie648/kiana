//! SSE 响应体构造（`text/event-stream`），供 stream.test.ts / event-stream 相关测试使用。
//!
//! Rust 侧给几个小构造函数，输出严格遵循 SSE 线上格式（wire format），保证假流与真实
//! 上游逐字节同形（`data: <json>\n\n`、`event: <name>\ndata: <json>\n\n`、`[DONE]` 结尾）。

/// 一条 `data: <payload>` 记录（payload 原样；调用方传 `JSON.stringify` 的结果）。
pub fn data(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// 一条带 `event:` 名的记录。
pub fn event(name: &str, payload: &str) -> String {
    format!("event: {name}\ndata: {payload}\n\n")
}

/// OpenAI 风格的流结束标记。
pub const DONE: &str = "data: [DONE]\n\n";

/// Anthropic 风格的 `event: message_stop`。
pub fn message_stop() -> String {
    event("message_stop", "{}")
}

/// 把若干记录拼成完整响应体。
pub fn concat(chunks: &[&str]) -> String {
    chunks.concat()
}

/// 常见响应头：SSE 的 `content-type`。
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

/// 常见响应头：JSON 的 `content-type`。
pub const JSON_CONTENT_TYPE: &str = "application/json";
