//! `anthropic-tool-name-normalization.test.ts` 的 Rust 移植。
//!
//! TS 用真实 OAuth token 跑 E2E；Rust 侧用 mock HTTP 覆盖同样的出站/入站归一化，
//! 并把真正的联网用例标为 `#[ignore]`。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context_with_tools};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{AssistantContentBlock, Model, Tool, UserMessage, UserRole};
use serde_json::{Value, json};

fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id).expect("catalog model");
    serde_json::from_value(value.clone()).expect("model parses")
}

fn user(text: &str) -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn tool(name: &str, property: &str) -> Tool {
    Tool {
        name: name.into(),
        description: format!("The {name} tool"),
        parameters: json!({
            "type": "object",
            "properties": { property: { "type": "string" } },
            "required": [property],
        }),
        constrained_sampling: None,
    }
}

fn sse_with_tool_call(name: &str) -> String {
    [
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "tool_use", "id": "toolu_1", "name": name, "input": {} },
            }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 1 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]
    .iter()
    .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
    .collect()
}

async fn run(
    model: &Model,
    tools: Vec<Tool>,
    response: MockResponse,
) -> (kiana_ai::types::AssistantMessage, Arc<MockHttpClient>) {
    let fetch = MockHttpClient::fixed(response);
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some("sk-ant-oat-test".into());
    let context = context_with_tools(None, vec![user("Use the tool.")], tools);
    let result = harness::run_to_result(stream(model, &context, options)).await;
    (result, fetch)
}

#[tokio::test]
async fn normalizes_user_defined_tools_matching_cc_names_outbound_and_back() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    let (result, fetch) = run(
        &model,
        vec![tool("todowrite", "task")],
        MockResponse::sse(sse_with_tool_call("TodoWrite")),
    )
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tools"][0]["name"], Value::String("TodoWrite".into()));
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::ToolUse);
    let name = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.name.clone()),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(name, "todowrite");
}

#[tokio::test]
async fn handles_kiana_built_in_tools() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    let (result, fetch) = run(
        &model,
        vec![tool("read", "path")],
        MockResponse::sse(sse_with_tool_call("Read")),
    )
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tools"][0]["name"], Value::String("Read".into()));
    let name = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.name.clone()),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(name, "read");
}

#[tokio::test]
async fn does_not_map_find_to_glob() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    let (result, fetch) = run(
        &model,
        vec![tool("find", "pattern")],
        MockResponse::sse(sse_with_tool_call("find")),
    )
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(body["tools"][0]["name"], Value::String("find".into()));
    let name = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.name.clone()),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(name, "find");
}

#[tokio::test]
async fn passes_custom_tool_names_through() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    let (result, fetch) = run(
        &model,
        vec![tool("my_custom_tool", "input")],
        MockResponse::sse(sse_with_tool_call("my_custom_tool")),
    )
    .await;
    let body = fetch.first_json_body().expect("body");
    assert_eq!(
        body["tools"][0]["name"],
        Value::String("my_custom_tool".into())
    );
    let name = result
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.name.clone()),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(name, "my_custom_tool");
}

#[tokio::test]
#[ignore = "live Anthropic API (OAuth)"]
async fn normalizes_tool_names_against_the_live_api() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    let mut options = AnthropicOptions::default();
    options.base.api_key = std::env::var("ANTHROPIC_API_KEY").ok();
    let context = context_with_tools(
        Some("You are a helpful assistant. Use the todowrite tool when asked to add todos."),
        vec![user("Add a todo: buy milk. Use the todowrite tool.")],
        vec![tool("todowrite", "task")],
    );
    let result = harness::run_to_result(stream(&model, &context, options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::ToolUse);
}

use std::sync::Arc;

#[tokio::test]
async fn replaces_astral_characters_in_tool_call_ids_per_utf16_code_unit() {
    let model = catalog_model("anthropic", "claude-sonnet-4-6");
    // `😀` 在 UTF-16 里是两个 code unit，TS 的 `/[^a-zA-Z0-9_-]/g` 会替换成两个下划线。
    let raw_id = "tool-ab\u{1F600}cd";
    let assistant = kiana_ai::types::Message::Assistant(kiana_ai::types::AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(kiana_ai::types::ToolCall {
            id: raw_id.into(),
            name: "read".into(),
            ..Default::default()
        })],
        // 来自另一个模型：`transformMessages` 只对跨模型的 tool call 归一化 ID。
        api: "openai-completions".into(),
        provider: "openai".into(),
        model: "gpt-4o".into(),
        usage: Default::default(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    });
    let tool_result = kiana_ai::types::Message::ToolResult(kiana_ai::types::ToolResultMessage {
        role: Default::default(),
        tool_call_id: raw_id.into(),
        tool_name: "read".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: "ok".into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 2,
        duration_ms: None,
    });
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_with_tool_call("Read")));
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = Some("sk-ant-oat-test".into());
    let context = context_with_tools(
        None,
        vec![user("Use the tool."), assistant, tool_result],
        vec![tool("read", "path")],
    );
    let _ = harness::run_to_result(stream(&model, &context, options)).await;
    let body = fetch.first_json_body().expect("body");
    let expected = Value::String("tool-ab__cd".into());
    let mut ids = Vec::new();
    for message in body["messages"].as_array().expect("messages") {
        for block in message["content"].as_array().cloned().unwrap_or_default() {
            match block["type"].as_str() {
                Some("tool_use") => ids.push(block["id"].clone()),
                Some("tool_result") => ids.push(block["tool_use_id"].clone()),
                _ => {}
            }
        }
    }
    assert_eq!(ids, vec![expected.clone(), expected]);
}
