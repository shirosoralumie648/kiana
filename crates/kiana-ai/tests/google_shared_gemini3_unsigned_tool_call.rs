//! `test/google-shared-gemini3-unsigned-tool-call.test.ts` 的移植。

use kiana_ai::api::google_shared::{convert_messages, requires_tool_call_id};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, Model,
    ModelInputModality, ToolCall, ToolCallKind, ToolResultMessage, ToolResultRole, Usage,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

fn make_model(api: &str, provider: &str, id: &str) -> Model {
    Model {
        id: id.into(),
        name: "Gemini 3 Pro Preview".into(),
        api: api.into(),
        provider: provider.into(),
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
    thought_signature: Option<&str>,
) -> kiana_ai::types::TranscriptContext {
    let mut first = ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "call_1".into(),
        name: "bash".into(),
        arguments: [("command".to_string(), json!("echo hi"))]
            .into_iter()
            .collect(),
        ..ToolCall::default()
    };
    first.thought_signature = thought_signature.map(str::to_string);
    normalize_context(&Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Hi".into()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                role: AssistantRole::Assistant,
                content: vec![
                    AssistantContentBlock::ToolCall(first),
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "call_2".into(),
                        name: "bash".into(),
                        arguments: [("command".to_string(), json!("ls -la"))]
                            .into_iter()
                            .collect(),
                        ..ToolCall::default()
                    }),
                ],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: kiana_ai::types::StopReason::ToolUse,
                timestamp: 1,
                ..Default::default()
            }),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_1".into(),
                tool_name: "bash".into(),
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "hi".into(),
                        text_signature: None,
                    },
                )],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: "call_2".into(),
                tool_name: "bash".into(),
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "files".into(),
                        text_signature: None,
                    },
                )],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
        ],
        ..Context::default()
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
fn preserves_tool_call_ids_for_gemini_3_models() {
    for (api, provider, id) in [
        ("google-generative-ai", "google", "gemini-3-pro-preview"),
        ("google-generative-ai", "google", "gemini-3.6-flash"),
        ("google-vertex", "google-vertex", "gemini-3-pro-preview"),
    ] {
        let model = make_model(api, provider, id);
        let contents = convert_messages(&model, &make_context(&model, None));
        let mut call_ids = Vec::new();
        let mut response_ids = Vec::new();
        for content in contents.as_array().expect("contents") {
            for part in content
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                if let Some(id) = part
                    .get("functionCall")
                    .and_then(|call| call.get("id"))
                    .and_then(Value::as_str)
                {
                    call_ids.push(id.to_string());
                }
                if let Some(id) = part
                    .get("functionResponse")
                    .and_then(|call| call.get("id"))
                    .and_then(Value::as_str)
                {
                    response_ids.push(id.to_string());
                }
            }
        }
        assert_eq!(call_ids, vec!["call_1", "call_2"], "{id}");
        assert_eq!(response_ids, vec!["call_1", "call_2"], "{id}");
    }
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_calls() {
    let model = make_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert_messages(&model, &make_context(&model, None));
    let turn = model_turn(&contents);
    let parts: Vec<Value> = turn["parts"].as_array().cloned().unwrap_or_default();
    let function_calls: Vec<&Value> = parts
        .iter()
        .filter(|part| part.get("functionCall").is_some())
        .collect();
    assert_eq!(function_calls.len(), 2);
    assert!(function_calls[0].get("thoughtSignature").is_none());
    assert!(function_calls[1].get("thoughtSignature").is_none());
    assert!(
        !serde_json::to_string(&turn)
            .unwrap()
            .contains("skip_thought_signature_validator")
    );
    let texts: Vec<&Value> = parts
        .iter()
        .filter(|part| part.get("text").is_some())
        .collect();
    assert!(texts.iter().all(|part| {
        !part["text"]
            .as_str()
            .unwrap_or("")
            .contains("Historical context")
    }));
}

#[test]
fn preserves_valid_thought_signature_for_same_provider_and_model() {
    let valid_sig = "AAAAAAAAAAAAAAAAAAAAAA==";
    let model = make_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert_messages(&model, &make_context(&model, Some(valid_sig)));
    let turn = model_turn(&contents);
    let parts: Vec<Value> = turn["parts"].as_array().cloned().unwrap_or_default();
    let function_calls: Vec<&Value> = parts
        .iter()
        .filter(|part| part.get("functionCall").is_some())
        .collect();
    assert_eq!(function_calls.len(), 2);
    assert_eq!(function_calls[0]["thoughtSignature"], json!(valid_sig));
    assert!(function_calls[1].get("thoughtSignature").is_none());
}

#[test]
fn does_not_add_ids_or_signatures_for_non_gemini_3_models() {
    let model = make_model("google-generative-ai", "google", "gemini-2.5-flash");
    let mut context = make_context(&model, None);
    if let Message::Assistant(assistant) = &mut context.0.messages[1] {
        assistant.model = "other-model".into();
    }
    let contents = convert_messages(&model, &context);
    let turn = model_turn(&contents);
    let parts: Vec<Value> = turn["parts"].as_array().cloned().unwrap_or_default();
    let function_calls: Vec<&Value> = parts
        .iter()
        .filter(|part| part.get("functionCall").is_some())
        .collect();
    assert_eq!(function_calls.len(), 2);
    assert!(
        function_calls
            .iter()
            .all(|part| part["functionCall"].get("id").is_none())
    );
    assert!(
        function_calls
            .iter()
            .all(|part| part.get("thoughtSignature").is_none())
    );
    let function_responses: Vec<&Value> = contents
        .as_array()
        .expect("contents")
        .iter()
        .flat_map(|content| {
            content
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter(|part| part.get("functionResponse").is_some())
        .collect::<Vec<_>>()
        .into_iter()
        .map(|part| Box::leak(Box::new(part)) as &Value)
        .collect();
    assert_eq!(function_responses.len(), 2);
    assert!(
        function_responses
            .iter()
            .all(|part| part["functionResponse"].get("id").is_none())
    );
}

#[test]
fn requires_tool_call_id_matches_ts() {
    assert!(!requires_tool_call_id("gemini-2.5-flash"));
    assert!(requires_tool_call_id("gemini-3.6-flash"));
    assert!(requires_tool_call_id("claude-sonnet-4-5"));
    assert!(requires_tool_call_id("gpt-oss-120b"));
}
