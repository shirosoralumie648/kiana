//! `anthropic-strict-tool-schema.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context_with_tools};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{
    CacheRetention, Model, ModelCost, ModelInputModality, Tool, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        id: "claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "anthropic-messages".into(),
        provider: "test-anthropic".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat: Some(json!({ "forceAdaptiveThinking": true, "supportsStrictTools": true })),
        ..Model::default()
    }
}

fn tool(parameters: Value, strict: Option<&str>) -> Tool {
    Tool {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters,
        constrained_sampling: strict.map(|strict| {
            kiana_ai::types::ConstrainedSampling::Config(
                kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: if strict == "require" {
                        kiana_ai::types::StrictMode::Require
                    } else {
                        kiana_ai::types::StrictMode::Prefer
                    },
                },
            )
        }),
    }
}

async fn capture_first_tool(tool: Tool) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut base = harness::stream_options(fetch);
    base.cache_retention = Some(CacheRetention::None);
    let mut options = AnthropicOptions {
        base,
        ..Default::default()
    };
    options.base.api_key = Some("test-key".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = context_with_tools(
        None,
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Use the tool".into()),
            timestamp: 0,
        })],
        vec![tool],
    );
    let stream = stream(&model(), &context, options);
    let _ = stream.result().await;
    let payload = captured.lock().unwrap().clone().expect("payload");
    payload["tools"][0].clone()
}

#[tokio::test]
async fn only_sends_the_full_input_schema_for_strict_json_schema_tools() {
    let legacy_parameters = json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
        "additionalProperties": false,
        "title": "LookupInput",
    });
    let legacy_tool = capture_first_tool(tool(legacy_parameters.clone(), None)).await;
    assert!(legacy_tool.get("strict").is_none());
    assert_eq!(
        legacy_tool["input_schema"],
        json!({
            "type": "object",
            "properties": legacy_parameters["properties"],
            "required": legacy_parameters["required"],
        })
    );

    let strict_tool = capture_first_tool(tool(
        json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" },
                "optional": { "type": "number" },
            },
            "required": ["value"],
            "title": "StrictLookupInput",
        }),
        Some("prefer"),
    ))
    .await;
    assert_eq!(strict_tool["strict"], Value::Bool(true));
    assert_eq!(
        strict_tool["input_schema"]["additionalProperties"],
        Value::Bool(false)
    );
    assert_eq!(
        strict_tool["input_schema"]["required"],
        json!(["value", "optional"])
    );
    assert_eq!(
        strict_tool["input_schema"]["properties"]["optional"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
    assert_eq!(
        strict_tool["input_schema"]["title"],
        Value::String("StrictLookupInput".into())
    );
}

#[tokio::test]
async fn sends_prefer_tools_non_strict_when_keywords_are_unsupported() {
    let unsupported = [
        json!({
            "type": "object",
            "properties": { "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 300000 } },
        }),
        json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "object",
                    "properties": { "tags": { "type": "array", "items": { "type": "string" }, "minItems": 2 } },
                },
            },
        }),
        json!({
            "type": "object",
            "properties": { "expression": { "type": "string", "format": "regex" } },
        }),
    ];
    for parameters in unsupported {
        let tool = capture_first_tool(tool(parameters, Some("prefer"))).await;
        assert!(tool.get("strict").is_none());
    }

    let supported_tool = capture_first_tool(tool(
        json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "minLength": 1, "maxLength": 1000, "pattern": "^[a-z]+$" },
                "url": { "type": "string", "format": "uri" },
                "tags": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            },
            "required": ["code", "url", "tags"],
        }),
        Some("prefer"),
    ))
    .await;
    assert_eq!(supported_tool["strict"], Value::Bool(true));
}

#[tokio::test]
async fn require_tools_report_unsupported_schemas_as_errors() {
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let options = AnthropicOptions {
        base: harness::stream_options(fetch),
        ..Default::default()
    };
    let context = context_with_tools(
        None,
        vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Use the tool".into()),
            timestamp: 0,
        })],
        vec![tool(
            json!({
                "type": "object",
                "properties": { "expression": { "type": "string", "format": "regex" } },
            }),
            Some("require"),
        )],
    );
    let result = harness::run_to_result(stream(&model(), &context, options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("requires JSON-schema constrained sampling"))
    );
}
