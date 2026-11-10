//! `packages/ai/test/lax-message-content.test.ts` 的 Rust 移植。
//!
//! TS 的痛点：`Message` 要求 `content` 始终存在，但“无类型调用方”（自定义工具、手工拼的历史、
//! 旧会话文件）会违反契约。`transformMessages` 是每个 provider 请求前的咽喉，因此刻意宽松：
//! `null`/缺失的 `content` 归一成空数组（issues #6259、#6276）。
//!
//! ## 移植状态
//!
//! `transformMessages` 位于 `crates/kiana-ai/src/api/transform_messages.rs`，由 `api-shared` 单元
//! 实现。Rust 的 [`Message`] 在 serde 层要求 `content` 存在，因此 `api-shared` 提供了
//! `normalize_message_content`（对应 TS 的 `msg.content == null ? { ...msg, content: [] } : msg`）
//! 与 `transform_lax_messages`（归一化 → 解析 → `transform_messages`）作为无类型调用方的入口。
//! 正式用例调用后者；另一个用例用**不依赖 transformMessages** 的等价断言锁定同一批输入在
//! Rust 类型下的行为（`null`/缺失 `content` 的宽松解析），保证这些回归不会在 Rust 侧悄悄消失。

mod common;

use common::messages::minimal_tool_result;
use kiana_ai::types::{Message, Model, ToolResultMessage};
use serde_json::{Value, json};

/// `test/lax-message-content.test.ts#makeTextOnlyModel`。
fn make_text_only_model() -> Value {
    json!({
        "id": "test-model",
        "name": "Test Model",
        "api": "openai-completions",
        "provider": "openai",
        "baseUrl": "https://example.invalid/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128000,
        "maxTokens": 16000,
    })
}

/// TS 测试里的三条输入（`as unknown as Message[]`）。
///
/// 逐字对照 `lax-message-content.test.ts`：
/// 1. `{role:"user", content:null, timestamp}`；
/// 2. `{role:"assistant", content:null, api, provider, model, usage, stopReason, timestamp}`；
/// 3. `{role:"toolResult", toolCallId, toolName, isError, timestamp}`（**没有** `content`）。
fn lax_messages() -> Vec<Value> {
    vec![
        json!({ "role": "user", "content": null, "timestamp": 1_700_000_000_000i64 }),
        json!({
            "role": "assistant",
            "content": null,
            "api": "openai-completions",
            "provider": "openai",
            "model": "test-model",
            "usage": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
            },
            "stopReason": "stop",
            "timestamp": 1_700_000_000_000i64,
        }),
        json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "toolName": "web_search",
            "isError": false,
            "timestamp": 1_700_000_000_000i64,
        }),
    ]
}

/// TS: "normalizes null/missing content to an empty array instead of crashing"。
///
/// TS 断言：
///   const result = transformMessages(messages, makeTextOnlyModel());
///   expect(result).toHaveLength(3);
///   for (const msg of result) expect(msg.content).toEqual([]);
///
/// Rust 的类型化 `transform_messages` 无法接收 `content: null`（serde 在入口前就拒绝），
/// 因此无类型调用方走 `transform_lax_messages`，它先做与 TS 相同的归一化再调用
/// `transform_messages`。
#[test]
fn normalizes_null_or_missing_content_to_an_empty_array() {
    let messages = lax_messages();
    assert_eq!(messages.len(), 3);
    assert!(messages[0]["content"].is_null());
    assert!(messages[1]["content"].is_null());
    assert!(messages[2].get("content").is_none());

    let model: Model =
        serde_json::from_value(make_text_only_model()).expect("text-only model parses");
    let result = kiana_ai::api::transform_messages::transform_lax_messages(&messages, &model)
        .expect("lax messages transform");
    assert_eq!(result.len(), 3);
    for message in &result {
        let content = match message {
            Message::User(user) => serde_json::to_value(&user.content).unwrap(),
            Message::Assistant(assistant) => serde_json::to_value(&assistant.content).unwrap(),
            Message::ToolResult(tool_result) => serde_json::to_value(&tool_result.content).unwrap(),
            Message::System(system) => serde_json::to_value(&system.content).unwrap(),
        };
        assert_eq!(content, json!([]), "content is an empty array");
    }
}

/// 不依赖 `transformMessages` 的那一半：宽松输入的**形状**。
///
/// `Message` 的 serde 目前要求 `content` 存在（`Vec<T>` 无 `#[serde(default)]`），因此
/// `null`/缺失的 `content` 在 Rust 侧会**解析失败**——这正是 `transformMessages` 需要在
/// 归一化之前先处理“原始 JSON”的原因。本测试锁定这一点，并用 `ToolResultMessage`（唯一
/// 通过 `serde_json::from_value` 可直接验证的变体之外）演示“补齐 content: [] 之后即可解析”。
#[test]
fn null_or_missing_content_needs_normalization_before_typed_parsing() {
    let messages = lax_messages();
    assert_eq!(messages.len(), 3);

    // (1) `content: null`：Rust 的 `UserContent` 是 untagged `String | Blocks`，`null` 两者都不匹配
    //     —— TS 的 `as unknown as Message[]` 正是绕过类型检查；Rust 侧同样不是合法值。
    assert!(
        serde_json::from_value::<Message>(messages[0].clone()).is_err(),
        "content: null is not a valid typed Message"
    );
    // (2) 助手消息同理。
    assert!(
        serde_json::from_value::<Message>(messages[1].clone()).is_err(),
        "assistant content: null is not a valid typed Message"
    );
    // (3) toolResult 缺 `content`：`ToolResultMessage.content` 没有默认值。
    assert!(
        serde_json::from_value::<Message>(messages[2].clone()).is_err(),
        "missing content is not a valid typed ToolResultMessage"
    );

    // 归一化（`transformMessages` 的职责）：`null`/缺失 → `[]`。补齐后三条都能解析。
    let normalized: Vec<Value> = messages
        .into_iter()
        .map(|mut message| {
            let object = message.as_object_mut().expect("message is an object");
            let empty = object.get("content").is_none_or(Value::is_null);
            if empty {
                object.insert("content".into(), json!([]));
            }
            message
        })
        .collect();
    let parsed: Vec<Message> = normalized
        .into_iter()
        .map(|message| serde_json::from_value(message).expect("normalized message parses"))
        .collect();
    assert_eq!(parsed.len(), 3);
    for message in &parsed {
        let content = match message {
            Message::User(user) => json!(user.content),
            Message::Assistant(assistant) => json!(assistant.content),
            Message::ToolResult(tool_result) => json!(tool_result.content),
            Message::System(system) => json!(system.content),
        };
        assert_eq!(content, json!([]), "content is an empty array");
    }

    // `makeTextOnlyModel()` 的数据形状本身合法（TS 用它触发
    // `replaceImagesWithPlaceholder`，这里只锁定字段集合）。
    let model = make_text_only_model();
    assert_eq!(model["input"], json!(["text"]));
    assert_eq!(model["contextWindow"], json!(128000));
    assert_eq!(model["maxTokens"], json!(16000));
}

/// 补充：`ToolResultMessage` 的最小形状（`content: []`）本身可以解析
/// （`common::messages::minimal_tool_result()` 与 TS 的 `base` 逐字段一致）。
#[test]
fn minimal_tool_result_parses_with_empty_content() {
    let tool_result: ToolResultMessage =
        serde_json::from_value(minimal_tool_result()).expect("minimal tool result parses");
    assert!(tool_result.content.is_empty());
    assert_eq!(tool_result.tool_call_id, "call-1");
    assert_eq!(tool_result.tool_name, "read");
    assert!(!tool_result.is_error);
    assert_eq!(tool_result.timestamp, 1);
}
