//! `test/message-types.test.ts` 与 `test/lax-message-content.test.ts` 共用的消息样本。
//!
//! TS 里这两份测试直接写对象字面量、靠 `JSON.stringify` 比较；Rust 侧用
//! [`serde_json::Value`] 表达同样的 JSON 形状（`Message` 的 serde 形状与 TS 有已知偏差，
//! 见 `crates/kiana-ai/src/types.rs` 的 `Message` 文档与 `tests/message_types.rs` 的说明）。

use serde_json::{Map, Value, json};

/// `test/message-types.test.ts` 的 `base`：一条最小的 toolResult。
pub fn tool_result_base() -> Map<String, Value> {
    match minimal_tool_result() {
        Value::Object(map) => map,
        _ => unreachable!("tool result base is an object"),
    }
}

/// `{role:"toolResult", toolCallId:"call-1", toolName:"read", content:[], isError:false, timestamp:1}`。
pub fn minimal_tool_result() -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": "call-1",
        "toolName": "read",
        "content": [],
        "isError": false,
        "timestamp": 1,
    })
}

/// `test/event-stream.test.ts` 的 `message()` 的 JSON 形状。
pub fn assistant_message_json(timestamp: i64, duration_ms: Option<f64>) -> Value {
    let mut map = Map::new();
    map.insert("role".into(), "assistant".into());
    map.insert("content".into(), json!([]));
    map.insert("api".into(), "openai-responses".into());
    map.insert("provider".into(), "openai".into());
    map.insert("model".into(), "m".into());
    map.insert(
        "usage".into(),
        json!({
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        }),
    );
    map.insert("stopReason".into(), "stop".into());
    map.insert("timestamp".into(), json!(timestamp));
    if let Some(duration_ms) = duration_ms {
        map.insert("durationMs".into(), json!(duration_ms));
    }
    Value::Object(map)
}
