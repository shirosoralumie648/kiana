//! `test/google-shared-signed-empty-blocks.test.ts` 的移植。

use kiana_ai::api::google_shared::convert_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, Model,
    ModelInputModality, TextContent, TextKind, ThinkingContent, ThinkingKind, ToolCall,
    ToolCallKind, Usage, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn make_model(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "google-generative-ai".into(),
        provider: "google".into(),
        base_url: "https://example.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        ..Model::default()
    }
}

fn make_context(
    model: &Model,
    content: Vec<AssistantContentBlock>,
) -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Hi".into()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                role: AssistantRole::Assistant,
                content,
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: kiana_ai::types::StopReason::ToolUse,
                timestamp: 1,
                ..Default::default()
            }),
        ],
        ..Context::default()
    })
}

fn tool_call() -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "call_1".into(),
        name: "bash".into(),
        arguments: [("command".to_string(), json!("ls"))].into_iter().collect(),
        ..ToolCall::default()
    })
}

fn model_turn(contents: &Value) -> Value {
    contents
        .as_array()
        .expect("contents")
        .iter()
        .find(|content| content.get("role").and_then(Value::as_str) == Some("model"))
        .cloned()
        .expect("model turn")
}

#[test]
fn keeps_a_signed_empty_thinking_block_so_its_signature_is_echoed_back() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &make_context(
            &model,
            vec![
                AssistantContentBlock::Thinking(ThinkingContent {
                    kind_thinking: ThinkingKind::Thinking,
                    thinking: String::new(),
                    thinking_signature: Some(VALID_SIG.into()),
                    redacted: None,
                }),
                tool_call(),
            ],
        ),
    );
    let turn = model_turn(&contents);
    let signed: Vec<&Value> = turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part.get("thoughtSignature").and_then(Value::as_str) == Some(VALID_SIG))
        .collect();
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0]["thought"], json!(true));
}

#[test]
fn keeps_a_signed_empty_text_block_the_same_way() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &make_context(
            &model,
            vec![
                AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: String::new(),
                    text_signature: Some(VALID_SIG.into()),
                }),
                tool_call(),
            ],
        ),
    );
    let turn = model_turn(&contents);
    let signed: Vec<&Value> = turn["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .filter(|part| part.get("thoughtSignature").and_then(Value::as_str) == Some(VALID_SIG))
        .collect();
    assert_eq!(signed.len(), 1);
}

#[test]
fn still_drops_unsigned_empty_blocks() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &make_context(
            &model,
            vec![
                AssistantContentBlock::Thinking(ThinkingContent {
                    kind_thinking: ThinkingKind::Thinking,
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                }),
                AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "   ".into(),
                    text_signature: None,
                }),
                tool_call(),
            ],
        ),
    );
    let parts = model_turn(&contents)["parts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(parts.len(), 1);
    assert!(parts[0].get("functionCall").is_some());
}

#[test]
fn still_drops_signed_empty_blocks_from_a_different_model() {
    let model = make_model("gemini-3-pro-preview");
    let mut context = make_context(
        &model,
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                kind_thinking: ThinkingKind::Thinking,
                thinking: String::new(),
                thinking_signature: Some(VALID_SIG.into()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: String::new(),
                text_signature: Some(VALID_SIG.into()),
            }),
            tool_call(),
        ],
    );
    // 把 assistant 消息的 model 改成别的模型（签名不可复用）。
    if let Message::Assistant(assistant) = &mut context.0.messages[1] {
        assistant.model = "other-model".into();
    }
    let contents = convert_messages(&model, &context);
    let turn = model_turn(&contents);
    let parts = turn["parts"].as_array().cloned().unwrap_or_default();
    assert_eq!(parts.len(), 1);
    assert!(parts[0].get("functionCall").is_some());
    assert!(!serde_json::to_string(&turn).unwrap().contains(VALID_SIG));
}
