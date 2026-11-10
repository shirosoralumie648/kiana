//! 冻结类型 JSON 形状的回归测试（review round 1 的 finding 1/2/3/4/9/13）。
//!
//! 每个用例锁定的都是 TS `types.ts` 的线协议形状：`AssistantMessageEvent` 的 `contentIndex`/
//! `toolCall`、`ClassifierQuestion` 的 `instructions`/`criteria`、`GrammarVariants` 的
//! `openai_lark`/`openai_regex`、`ImagesContext`/`AssistantImages`、`UserAgentMode` 的
//! `"detailed"`、`ClientIdentity.attribution` 的 null/缺省三态、`require_chat` 的错误文本。

use kiana_ai::error::ModelsErrorCode;
use kiana_ai::types::{
    AssistantImages, AssistantMessage, AssistantMessageEvent, ClassifierQuestion,
    ClientAttribution, ClientIdentity, ConstrainedSamplingConfig, GrammarVariants, ImagesContext,
    ImagesStopReason, Model, ModelType, StopReason, ToolCall, ToolCallKind, UserAgentMode,
    require_chat,
};
use serde_json::{Value, json};

fn minimal_assistant() -> AssistantMessage {
    serde_json::from_value(json!({
        "role": "assistant",
        "content": [],
        "api": "openai-responses",
        "provider": "openai",
        "model": "m",
        "usage": {
            "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
        },
        "stopReason": "stop",
        "timestamp": 1,
    }))
    .expect("minimal assistant message parses")
}

fn tool_call() -> ToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "call_1".into(),
        name: "read".into(),
        arguments: [("path".to_string(), json!("a.ts"))].into_iter().collect(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

/// finding 1：`AssistantMessageEvent` 的流式字段在线上必须是 `contentIndex` / `toolCall`
/// （TS `types.ts:821-834`；`spec/ca-rpc-print.md:2477-2482`）。
#[test]
fn assistant_message_events_use_the_typescript_field_names() {
    let partial = minimal_assistant();
    let event = AssistantMessageEvent::TextDelta {
        content_index: 0,
        delta: "hi".into(),
        partial: Box::new(partial.clone()),
    };
    let value = serde_json::to_value(&event).expect("event serializes");
    assert_eq!(value["type"], json!("text_delta"));
    assert_eq!(value["contentIndex"], json!(0));
    assert_eq!(value["delta"], json!("hi"));
    assert!(value.get("content_index").is_none(), "{value}");

    let toolcall_end = AssistantMessageEvent::ToolCallEnd {
        content_index: 1,
        tool_call: tool_call(),
        partial: Box::new(partial),
    };
    let value = serde_json::to_value(&toolcall_end).expect("event serializes");
    assert_eq!(value["type"], json!("toolcall_end"));
    assert_eq!(value["contentIndex"], json!(1));
    assert_eq!(value["toolCall"]["type"], json!("toolCall"));
    assert_eq!(value["toolCall"]["name"], json!("read"));
    assert!(value.get("tool_call").is_none(), "{value}");

    // TS 形状的事件必须能反序列化回来（agent/RPC 的线协议）。
    let parsed: AssistantMessageEvent = serde_json::from_value(json!({
        "type": "text_delta",
        "contentIndex": 2,
        "delta": "x",
        "partial": minimal_assistant(),
    }))
    .expect("TS-shaped text_delta parses");
    assert!(matches!(
        parsed,
        AssistantMessageEvent::TextDelta {
            content_index: 2,
            ..
        }
    ));
    // 旧（错误）形状不再被接受：`content_index` 不是 TS 的键。
    let error = serde_json::from_value::<AssistantMessageEvent>(json!({
        "type": "text_delta",
        "content_index": 2,
        "delta": "x",
        "partial": minimal_assistant(),
    }))
    .expect_err("snake_case field names are not the TS shape");
    assert!(!error.to_string().is_empty());
}

/// finding 2：`ClassifierQuestion` 的三种形态都必须带 `instructions`，`criteria` 分别是
/// `Record<string, string>` / `string[]` / `{true, false}`（TS `types.ts:682-700`）。
#[test]
fn classifier_questions_match_the_typescript_shapes() {
    let choice: ClassifierQuestion = serde_json::from_value(json!({
        "type": "choice",
        "instructions": "pick one",
        "criteria": { "a": "first", "b": "second" },
    }))
    .expect("choice question parses");
    let ClassifierQuestion::Choice {
        instructions,
        criteria,
    } = &choice
    else {
        panic!("expected a choice question, got {choice:?}");
    };
    assert_eq!(instructions, "pick one");
    assert_eq!(criteria.get("a").map(String::as_str), Some("first"));
    assert_eq!(
        serde_json::to_value(&choice).expect("serializes")["type"],
        json!("choice")
    );

    let score: ClassifierQuestion = serde_json::from_value(json!({
        "type": "score",
        "instructions": "rate it",
        "criteria": ["low", "high"],
    }))
    .expect("score question parses");
    assert!(matches!(score, ClassifierQuestion::Score { .. }));

    let boolean: ClassifierQuestion = serde_json::from_value(json!({
        "type": "bool",
        "instructions": "yes or no",
        "criteria": { "true": "yes", "false": "no" },
    }))
    .expect("bool question parses");
    let value = serde_json::to_value(&boolean).expect("serializes");
    assert_eq!(value["criteria"], json!({ "true": "yes", "false": "no" }));
    // 键顺序与 TS 接口声明序一致（`true` 在前）。
    assert_eq!(
        kiana_json::js_object::stringify(&value),
        r#"{"type":"bool","instructions":"yes or no","criteria":{"true":"yes","false":"no"}}"#
    );
}

/// finding 3：`GrammarVariants` 的键是字面量 `openai_lark` / `openai_regex`
/// （TS `GrammarFormat = "openai_lark" | "openai_regex"`）。
#[test]
fn grammar_variants_keep_the_typescript_snake_case_keys() {
    let variants: GrammarVariants = serde_json::from_value(json!({
        "openai_lark": "L",
        "openai_regex": "R",
    }))
    .expect("variants parse");
    assert_eq!(variants.openai_lark.as_deref(), Some("L"));
    assert_eq!(variants.openai_regex.as_deref(), Some("R"));
    let value = serde_json::to_value(&variants).expect("serializes");
    assert_eq!(value, json!({ "openai_lark": "L", "openai_regex": "R" }));

    let config: ConstrainedSamplingConfig = serde_json::from_value(json!({
        "type": "grammar",
        "variants": { "openai_lark": "L" },
    }))
    .expect("grammar config parses");
    assert!(matches!(config, ConstrainedSamplingConfig::Grammar { .. }));
}

/// finding 4：`ImagesContext` 是 `{ input }`，`AssistantImages` 是
/// `{api, provider, model, output, responseId?, usage?, stopReason, errorMessage?, timestamp}`
/// 且 `stopReason` 只允许 `"stop" | "error" | "aborted"`。
#[test]
fn images_shapes_match_the_typescript_api() {
    let context: ImagesContext = serde_json::from_value(json!({
        "input": [
            { "type": "text", "text": "describe" },
            { "type": "image", "data": "AAA", "mimeType": "image/png" },
        ],
    }))
    .expect("images context parses");
    assert_eq!(context.input.len(), 2);
    let value = serde_json::to_value(&context).expect("serializes");
    assert_eq!(value["input"][0]["type"], json!("text"));
    assert_eq!(value["input"][1]["mimeType"], json!("image/png"));

    let images: AssistantImages = serde_json::from_value(json!({
        "api": "openrouter-images",
        "provider": "openrouter",
        "model": "google/gemini-3-pro-image",
        "output": [{ "type": "image", "data": "AAA", "mimeType": "image/png" }],
        "responseId": "resp_1",
        "stopReason": "aborted",
        "timestamp": 5,
    }))
    .expect("assistant images parses");
    assert_eq!(images.stop_reason, ImagesStopReason::Aborted);
    let value = serde_json::to_value(&images).expect("serializes");
    assert_eq!(value["stopReason"], json!("aborted"));
    assert_eq!(value["responseId"], json!("resp_1"));
    assert!(value.get("errorMessage").is_none());
    // `pending` / `toolUse` 不是 `ImagesStopReason`。
    assert!(
        serde_json::from_value::<AssistantImages>(json!({
            "api": "openrouter-images",
            "provider": "openrouter",
            "model": "m",
            "output": [],
            "stopReason": "toolUse",
            "timestamp": 5,
        }))
        .is_err()
    );
}

/// finding 13：`UserAgentMode` 接受 `"detailed"`；`ClientIdentity.attribution` 区分
/// `null`（清除）与缺省（不改动）。
#[test]
fn user_agent_mode_and_attribution_keep_the_typescript_three_states() {
    let detailed: ClientIdentity =
        serde_json::from_value(json!({ "userAgent": "detailed" })).expect("detailed parses");
    assert_eq!(
        detailed.user_agent,
        Some(UserAgentMode::Detailed(Default::default()))
    );
    let short: ClientIdentity =
        serde_json::from_value(json!({ "userAgent": "short" })).expect("short parses");
    assert!(
        short
            .user_agent
            .as_ref()
            .is_some_and(UserAgentMode::is_short)
    );
    let custom: ClientIdentity =
        serde_json::from_value(json!({ "userAgent": { "custom": "kiana/1" } }))
            .expect("custom parses");
    assert_eq!(
        custom.user_agent.as_ref().and_then(UserAgentMode::custom),
        Some("kiana/1")
    );
    // 序列化回同一个字面量。
    assert_eq!(
        serde_json::to_value(&detailed).expect("serializes")["userAgent"],
        json!("detailed")
    );

    let attribution = ClientAttribution {
        name: "kiana".into(),
        url: "https://kiana.dev".into(),
        categories: Some(vec!["cli-agent".into()]),
    };
    let absent: ClientIdentity = serde_json::from_value(json!({})).expect("absent parses");
    assert_eq!(absent.attribution, None);
    let cleared: ClientIdentity =
        serde_json::from_value(json!({ "attribution": null })).expect("null parses");
    assert_eq!(cleared.attribution, Some(None));
    let set: ClientIdentity =
        serde_json::from_value(json!({ "attribution": attribution })).expect("object parses");
    assert_eq!(set.attribution, Some(Some(attribution.clone())));
    // 往返：`null` 仍写成 `null`，缺省不写键。
    assert_eq!(
        serde_json::to_value(&cleared).expect("serializes")["attribution"],
        Value::Null
    );
    assert!(
        serde_json::to_value(&absent)
            .expect("serializes")
            .get("attribution")
            .is_none()
    );
}

/// finding 9：`require_chat` 的错误文本是当前稳定契约（`spec/ca-config-models.md:3765`，
/// 对应原 `assertChatModel` 的行为），改动需同步测试；并带 `ModelsError` 的 `name`/`code`。
#[test]
fn require_chat_matches_assert_chat_model() {
    let chat: Model = serde_json::from_value(json!({
        "id": "gpt-5",
        "name": "GPT-5",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": "https://api.openai.com/v1",
        "input": ["text"],
        "cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
        "maxTokens": 100,
    }))
    .expect("chat model parses");
    assert!(require_chat(&chat).is_ok());

    let image = Model {
        model_type: Some(ModelType::Image),
        ..chat
    };
    let error = require_chat(&image).expect_err("image model is rejected");
    assert_eq!(error.to_string(), "Model openai/gpt-5 is not a chat model");
    assert_eq!(error.name(), "ModelsError");
    assert_eq!(error.code(), ModelsErrorCode::Provider);
    assert_eq!(error.into_models_error().code, ModelsErrorCode::Provider);
}

/// 补充：`AssistantMessageEvent` 的 `done` / `error` 事件仍用 `reason` 与 `message`/`error`
/// （camelCase 只改字段名，不改变体名或语义）。
#[test]
fn terminal_events_keep_their_shape() {
    let message = minimal_assistant();
    let done = AssistantMessageEvent::Done {
        reason: kiana_ai::types::DoneReason::Stop,
        message: Box::new(message.clone()),
    };
    let value = serde_json::to_value(&done).expect("serializes");
    assert_eq!(value["type"], json!("done"));
    assert_eq!(value["reason"], json!("stop"));
    assert_eq!(value["message"]["stopReason"], json!("stop"));

    let error = AssistantMessageEvent::Error {
        reason: kiana_ai::types::ErrorReason::Aborted,
        error: Box::new(AssistantMessage {
            stop_reason: StopReason::Aborted,
            ..message
        }),
    };
    let value = serde_json::to_value(&error).expect("serializes");
    assert_eq!(value["type"], json!("error"));
    assert_eq!(value["reason"], json!("aborted"));
    assert_eq!(value["error"]["stopReason"], json!("aborted"));
}
