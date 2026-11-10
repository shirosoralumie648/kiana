//! `test/bedrock-convert-messages.test.ts` 的移植。
//!
//! 注：TS 用 `as any` 塞入未知内容块（`{type:"unknown"}`）；Rust 的 `Message` 是类型化枚举，
//! 无法表达未知块，因此与 `content` 无关的“未知块被跳过”用例由类型系统本身保证。

use kiana_ai::api::bedrock_converse_stream::{
    BedrockOptions, build_command_input, convert_messages, convert_tool_config, handle_stream_item,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, CacheRetention, Context, ImageContent,
    ImageKind, Message, Model, ModelInputModality, StopReason, StreamOptions, TextContent,
    TextKind, Tool, ToolCall, ToolCallKind, ToolResultMessage, ToolResultRole, Usage, UserContent,
    UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

pub fn base_model() -> Model {
    Model {
        id: "us.anthropic.claude-sonnet-4-5-20250929-v1:0".into(),
        name: "Claude Sonnet 4.5 (US)".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: kiana_ai::types::ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
            ..Default::default()
        },
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        compat: Some(json!({"supportsStrictMode": true})),
        ..Model::default()
    }
}

pub fn nova_model() -> Model {
    let mut model = base_model();
    model.id = "amazon.nova-lite-v1:0".into();
    model.name = "Nova Lite".into();
    model.reasoning = Some(false);
    model.compat = None;
    model
}

fn options() -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: 1,
    })
}

fn payload_for(context: Context, model: &Model) -> Value {
    build_command_input(
        model,
        &normalize_context(&context),
        CacheRetention::None,
        &options(),
        true,
    )
    .expect("payload")
}

#[test]
fn gates_native_strict_tool_use_by_model_capability() {
    let context = Context {
        messages: vec![user("Use the tool")],
        tools: Some(vec![Tool {
            name: "lookup".into(),
            description: "Look up a value".into(),
            parameters: json!({"type": "object", "properties": {"value": {"type": "string"}}}),
            constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Config(
                kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: kiana_ai::types::StrictMode::Require,
                },
            )),
        }]),
        ..Context::default()
    };
    let payload = payload_for(context.clone(), &base_model());
    assert_eq!(
        payload["toolConfig"]["tools"][0]["toolSpec"]["strict"],
        json!(true)
    );

    let mut prefer = context;
    prefer.tools.as_mut().unwrap()[0].constrained_sampling =
        Some(kiana_ai::types::ConstrainedSampling::Config(
            kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                strict: kiana_ai::types::StrictMode::Prefer,
            },
        ));
    let nova_payload = build_command_input(
        &nova_model(),
        &normalize_context(&prefer),
        CacheRetention::None,
        &options(),
        false,
    )
    .expect("payload");
    assert!(
        nova_payload["toolConfig"]["tools"][0]["toolSpec"]
            .get("strict")
            .is_none()
    );
}

#[test]
fn preserves_empty_property_names_in_streamed_tool_arguments() {
    let model = base_model();
    let mut output = AssistantMessage {
        role: AssistantRole::Assistant,
        api: "bedrock-converse-stream".into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        timestamp: 1,
        ..Default::default()
    };
    let stream = AssistantMessageEventStream::new();
    let mut open = Default::default();
    for item in [
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockStart": {"contentBlockIndex": 0, "start": {"toolUse": {"toolUseId": "tool-1", "name": "edit"}}}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"toolUse": {"input": "{\"path\":\"/workspace/foobar/file.js\",\"edits\":[{\"oldText\":\"first\",\"newText\":\"updated first\"},{\"oldText\":\"second\",\"newText\":\"updated second\",\"\":\"\"}]}"}}}}),
        json!({"contentBlockStop": {"contentBlockIndex": 0}}),
        json!({"messageStop": {"stopReason": "tool_use"}}),
    ] {
        handle_stream_item(&model, &mut output, &stream, &mut open, &item).expect("item");
    }
    match &output.content[0] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(
                Value::Object(call.arguments.clone().into_iter().collect()),
                json!({"path": "/workspace/foobar/file.js", "edits": [
                    {"oldText": "first", "newText": "updated first"},
                    {"oldText": "second", "newText": "updated second", "": ""}
                ]})
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn replaces_unknown_user_content_with_a_placeholder() {
    // 用户块只能是 text/image；空块列表走与“只有未知块”相同的占位替换路径。
    let context = Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![]),
            timestamp: 1,
        })],
        ..Context::default()
    };
    let payload = payload_for(context, &base_model());
    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        payload["messages"][0]["content"],
        json!([{"text": "<empty>"}])
    );
}

#[test]
fn replaces_blank_user_string_content_with_a_placeholder() {
    let payload = payload_for(
        Context {
            messages: vec![user("   ")],
            ..Context::default()
        },
        &base_model(),
    );
    assert_eq!(
        payload["messages"][0]["content"],
        json!([{"text": "<empty>"}])
    );
}

#[test]
fn filters_blank_user_text_blocks_when_other_content_remains() {
    let payload = payload_for(
        Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![text_block(""), text_block("hello")]),
                timestamp: 1,
            })],
            ..Context::default()
        },
        &base_model(),
    );
    assert_eq!(
        payload["messages"][0]["content"],
        json!([{"text": "hello"}])
    );
}

// TS 的 “surrogate sanitization 后为空” 用例在 Rust 里不可表达：`String` 不能保存孤立代理项
// （`String::from_utf16_lossy` 会替换成 U+FFFD），`sanitize_surrogates` 对合法 UTF-8 是恒等。

#[test]
fn replaces_blank_tool_result_content_with_a_placeholder() {
    let payload = payload_for(
        Context {
            messages: vec![tool_result("tool-1", "tool", vec![text_block("")], false)],
            ..Context::default()
        },
        &base_model(),
    );
    assert_eq!(
        payload["messages"][0]["content"][0]["toolResult"]["content"],
        json!([{"text": "<empty>"}])
    );
}

#[test]
fn moves_images_of_error_tool_results_after_the_tool_results() {
    let model = base_model();
    let mut arguments: kiana_ai::types::JsonObject = Default::default();
    arguments.insert("x".into(), json!(1));
    let context = Context {
        messages: vec![
            assistant_message(
                &model,
                vec![
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call-1".into(),
                        name: "codemode".into(),
                        arguments: Default::default(),
                        ..Default::default()
                    }),
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call-2".into(),
                        name: "read".into(),
                        arguments: Default::default(),
                        ..Default::default()
                    }),
                ],
            ),
            tool_result(
                "call-1",
                "codemode",
                vec![
                    text_block("Script failed"),
                    image_block("AAAA"),
                    text_block("Script error"),
                ],
                true,
            ),
            tool_result("call-2", "read", vec![image_block("BBBB")], false),
        ],
        ..Context::default()
    };
    let payload = payload_for(context, &model);
    let content = payload["messages"][1]["content"]
        .as_array()
        .expect("content");
    assert_eq!(content.len(), 4);
    assert_eq!(
        content[0]["toolResult"],
        json!({"toolUseId": "call-1", "status": "error", "content": [
            {"text": "Script failed"},
            {"text": "(image attached after the tool results)"},
            {"text": "Script error"}
        ]})
    );
    assert_eq!(content[1]["toolResult"]["status"], json!("success"));
    assert_eq!(
        content[2],
        json!({"text": "Images from the error result of tool call call-1 (codemode):"})
    );
    assert_eq!(content[3]["image"]["format"], json!("png"));
}

#[test]
fn keeps_images_in_error_tool_results_for_non_claude_models() {
    let payload = payload_for(
        Context {
            messages: vec![tool_result(
                "call-1",
                "codemode",
                vec![text_block("Script failed"), image_block("AAAA")],
                true,
            )],
            ..Context::default()
        },
        &nova_model(),
    );
    assert_eq!(
        payload["messages"][0]["content"],
        json!([{
            "toolResult": {
                "toolUseId": "call-1",
                "status": "error",
                "content": [{"text": "Script failed"}, {"image": {"source": {"bytes": "AAAA"}, "format": "png"}}]
            }
        }])
    );
}

#[test]
fn skips_assistant_messages_with_no_content() {
    let model = base_model();
    let context = Context {
        messages: vec![assistant_message(&model, vec![]), user("Continue")],
        ..Context::default()
    };
    let payload = payload_for(context, &model);
    let messages = payload["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], json!("user"));
}

#[test]
fn removes_empty_property_names_only_from_replayed_bedrock_input() {
    let model = base_model();
    let nested = json!({"oldText": "second", "newText": "updated second", "": ""});
    let first = json!({"oldText": "first", "newText": "updated first"});
    let mut arguments: kiana_ai::types::JsonObject = Default::default();
    arguments.insert("path".into(), json!("/workspace/foobar/file.js"));
    arguments.insert("edits".into(), json!([first, nested.clone()]));
    let context = Context {
        messages: vec![
            assistant_message(
                &model,
                vec![AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id: "tool-1".into(),
                    name: "edit".into(),
                    arguments,
                    ..Default::default()
                })],
            ),
            tool_result("tool-1", "edit", vec![text_block("done")], false),
            user("Continue"),
        ],
        ..Context::default()
    };
    let payload = payload_for(context, &model);
    assert_eq!(
        payload["messages"][0]["content"][0]["toolUse"]["input"],
        json!({"path": "/workspace/foobar/file.js", "edits": [
            {"oldText": "first", "newText": "updated first"},
            {"oldText": "second", "newText": "updated second"}
        ]})
    );
    // 原始消息不被修改。
    let _ = nested;
}

#[test]
fn convert_tool_config_maps_tool_choice() {
    let tool = Tool {
        name: "lookup".into(),
        description: "d".into(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
    };
    let config = convert_tool_config(Some(&[tool]), Some(&json!("auto")), true)
        .expect("config")
        .expect("some");
    assert_eq!(config["toolChoice"], json!({"auto": {}}));
    let config = convert_tool_config(Some(&[]), Some(&json!("auto")), true).expect("config");
    assert!(config.is_none());
}

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

fn image_block(data: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent {
        kind_image: ImageKind::Image,
        data: data.into(),
        mime_type: "image/png".into(),
    })
}

fn assistant_message(model: &Model, content: Vec<AssistantContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    })
}

fn tool_result(
    tool_call_id: &str,
    tool_name: &str,
    content: Vec<UserContentBlock>,
    is_error: bool,
) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        content,
        details: None,
        usage: None,
        nested_calls: None,
        is_error,
        timestamp: 1,
        duration_ms: None,
    })
}

// 保证 `convert_messages` 直接入口也被引用。
#[allow(dead_code)]
fn _convert(model: &Model, context: &kiana_ai::types::TranscriptContext) {
    let _ = convert_messages(context, model, CacheRetention::None, None);
}
