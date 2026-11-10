//! `anthropic-strict-tool-fallback.test.ts` 的 Rust 移植。
//!
//! 省略了 `kiana-messages` 网关用例（另一个单元的实现）与“onPayload 抛错”用例
//! （Rust 回调不能抛错）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context_with_tools};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{
    CacheRetention, Message, Model, ModelCost, ModelInputModality, StopReason, Tool, UserMessage,
    UserRole,
};
use serde_json::{Value, json};

const GRAMMAR_ERROR: &str = "The compiled grammar is too large, which would cause performance issues. Simplify your tool schemas or reduce the number of strict tools.";

fn model() -> Model {
    Model {
        id: "claude-haiku-4-5".into(),
        name: "Test Claude".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "https://anthropic.example".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(100),
        compat: Some(json!({ "supportsStrictTools": true })),
        ..Model::default()
    }
}

fn tool(name: &str, strict: Option<&str>) -> Tool {
    Tool {
        name: name.into(),
        description: "Look up a value".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "value": { "type": "string" },
                "optional": { "type": "number" },
            },
            "required": ["value"],
        }),
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

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn context(tools: Vec<Tool>) -> kiana_ai::types::TranscriptContext {
    context_with_tools(None, vec![user("Hello")], tools)
}

fn error_response(message: &str, status: u16, kind: &str) -> MockResponse {
    MockResponse::json(
        status,
        json!({ "type": "error", "error": { "type": kind, "message": message } }).to_string(),
    )
}

fn success_response() -> MockResponse {
    let events = [
        (
            "message_start",
            json!({ "type": "message_start", "message": { "id": "msg_test", "model": "claude-haiku-4-5", "usage": { "input_tokens": 10, "output_tokens": 0 } } }),
        ),
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ];
    MockResponse::sse(
        events
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>(),
    )
}

struct Harness {
    fetch: Arc<MockHttpClient>,
    options: AnthropicOptions,
}

fn harness(respond: impl Fn(&Value, usize) -> MockResponse + Send + Sync + 'static) -> Harness {
    let attempts = Arc::new(Mutex::new(0usize));
    let fetch = MockHttpClient::with_handler(move |request| {
        let body = request
            .body
            .as_deref()
            .and_then(harness::decode_body)
            .unwrap_or(Value::Null);
        let mut attempts = attempts.lock().unwrap();
        *attempts += 1;
        Ok(respond(&body, *attempts))
    });
    let mut base = harness::stream_options(fetch.clone());
    base.max_retries = Some(0);
    let options = AnthropicOptions {
        base,
        ..Default::default()
    };
    Harness { fetch, options }
}

fn request_bodies(fetch: &MockHttpClient) -> Vec<Value> {
    fetch
        .requests()
        .iter()
        .map(|request| harness::decode_body(request.body.as_deref().unwrap()).unwrap())
        .collect()
}

#[tokio::test]
async fn retries_with_original_non_strict_schemas_and_preserves_required_tools() {
    for message in [
        GRAMMAR_ERROR,
        "Too many strict tools (32). The maximum number of strict tools supported is 20. Try reducing the number of tools marked as strict.",
        "Schemas contains too many parameters with union types (480 parameters with type arrays or anyOf). This causes exponential compilation cost. Reduce the number of nullable or union-typed parameters (limit: 16 parameters with unions).",
    ] {
        let harness = harness(move |_payload, attempt| {
            if attempt == 1 {
                error_response(message, 400, "invalid_request_error")
            } else {
                success_response()
            }
        });
        let on_payload_count = Arc::new(Mutex::new(0usize));
        let on_payload_count_for_callback = on_payload_count.clone();
        let on_response_count = Arc::new(Mutex::new(0usize));
        let on_response_count_for_callback = on_response_count.clone();
        let mut options = harness.options;
        options.base.cache_retention = Some(CacheRetention::Long);
        options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
            *on_payload_count_for_callback.lock().unwrap() += 1;
            let mut payload = payload;
            payload["inference_geo"] = Value::String("us".into());
            payload["stream"] = Value::Bool(false);
            Some(payload)
        }));
        options.base.on_response = Some(Arc::new(move |_response, _model| {
            *on_response_count_for_callback.lock().unwrap() += 1;
        }));
        let transcript = context(vec![
            tool("preferred", Some("prefer")),
            tool("required", Some("require")),
            tool("ordinary", None),
        ]);
        let result = harness::run_to_result(stream(&model(), &transcript, options)).await;
        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(*on_payload_count.lock().unwrap(), 2);
        assert_eq!(*on_response_count.lock().unwrap(), 1);

        let payloads = request_bodies(&harness.fetch);
        assert_eq!(payloads.len(), 2);
        let first = &payloads[0]["tools"];
        let second = &payloads[1]["tools"];
        assert_eq!(first[0]["strict"], Value::Bool(true));
        assert_eq!(
            first[0]["input_schema"]["required"],
            json!(["value", "optional"])
        );
        assert_eq!(
            first[0]["input_schema"]["properties"]["optional"],
            json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
        );
        assert!(second[0].get("strict").is_none());
        assert_eq!(
            second[0]["input_schema"],
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" },
                    "optional": { "type": "number" },
                },
                "required": ["value"],
            })
        );
        assert_eq!(second[1], first[1]);
        assert_eq!(second[1]["strict"], Value::Bool(true));
        assert_eq!(second[2], first[2]);
        assert_eq!(
            second[2]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        assert_eq!(payloads[0]["stream"], Value::Bool(true));
        assert_eq!(payloads[1]["stream"], Value::Bool(true));
        assert_eq!(payloads[1]["inference_geo"], Value::String("us".into()));
        let diagnostics = result.diagnostics.expect("diagnostics");
        assert_eq!(diagnostics[0].kind, "anthropic_strict_tools_fallback");
        assert_eq!(
            diagnostics[0].details.as_ref().unwrap()["reason"],
            Value::String("strict_tool_limits_exceeded".into())
        );
    }
}

#[tokio::test]
async fn downgrades_inline_additions_and_redefinitions_without_downgrading_require() {
    let harness = harness(|_payload, attempt| {
        if attempt == 1 {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        } else {
            success_response()
        }
    });
    let mut model = model();
    model.compat = Some(json!({
        "supportsStrictTools": true,
        "supportsMidConvoSystemMessages": true,
        "supportsMidConvoToolChanges": true,
    }));
    let mut messages = vec![user("Hello")];
    messages.push(Message::System(kiana_ai::types::SystemMessage {
        role: Default::default(),
        content: kiana_ai::types::SystemContent::Text(String::new()),
        sections: None,
        tools_added: Some(vec![tool("lookup", Some("prefer"))]),
        tools_removed: None,
        timestamp: 2,
    }));
    messages.push(Message::System(kiana_ai::types::SystemMessage {
        role: Default::default(),
        content: kiana_ai::types::SystemContent::Text(String::new()),
        sections: None,
        tools_added: Some(vec![tool("lookup", Some("require"))]),
        tools_removed: None,
        timestamp: 3,
    }));
    let transcript = context_with_tools(None, messages, vec![tool("lookup", Some("require"))]);
    let mut options = harness.options;
    options.base.api_key = Some("sk-ant-oat-test".into());
    let result = harness::run_to_result(stream(&model, &transcript, options)).await;
    assert_eq!(result.stop_reason, StopReason::Stop);
    let payloads = request_bodies(&harness.fetch);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[1]["tools"], payloads[0]["tools"]);
    let additions: Vec<Vec<Value>> = payloads
        .iter()
        .map(|payload| {
            payload["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|message| {
                    message["content"]
                        .as_array()
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter(|block| block["type"] == "tool_addition")
                                .map(|block| block["tool"]["definition"].clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    assert_eq!(
        additions[0]
            .iter()
            .map(|definition| definition.get("strict").cloned())
            .collect::<Vec<_>>(),
        vec![Some(Value::Bool(true)), Some(Value::Bool(true))]
    );
    assert_eq!(additions[1][0].get("strict"), None);
    assert_eq!(additions[1][1].get("strict"), Some(&Value::Bool(true)));
    assert_eq!(
        additions[1][0]["input_schema"]["required"],
        json!(["value"])
    );
    assert_eq!(additions[1][1], additions[0][1]);
}

#[tokio::test]
async fn does_not_retry_when_no_preferred_strict_tool_exists() {
    for tools in [
        vec![tool("lookup", Some("require"))],
        vec![tool("lookup", None)],
        vec![Tool {
            parameters: json!({
                "type": "object",
                "properties": { "value": { "type": "number", "minimum": 1 } },
            }),
            ..tool("lookup", Some("prefer"))
        }],
    ] {
        let harness = harness(|_payload, _attempt| {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        });
        let result =
            harness::run_to_result(stream(&model(), &context(tools), harness.options)).await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(
            result
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains(GRAMMAR_ERROR))
        );
        assert_eq!(request_bodies(&harness.fetch).len(), 1);
    }
}

#[tokio::test]
async fn does_not_downgrade_on_unrelated_errors() {
    for (message, status, kind) in [
        ("Invalid tool schema", 400, "invalid_request_error"),
        (GRAMMAR_ERROR, 400, "authentication_error"),
        (GRAMMAR_ERROR, 500, "invalid_request_error"),
    ] {
        let harness = harness(move |_payload, _attempt| error_response(message, status, kind));
        let result = harness::run_to_result(stream(
            &model(),
            &context(vec![tool("lookup", Some("prefer"))]),
            harness.options,
        ))
        .await;
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(request_bodies(&harness.fetch).len(), 1);
    }
}

#[tokio::test]
async fn surfaces_a_second_rejection_rather_than_retrying_again() {
    let harness = harness(|_payload, attempt| {
        if attempt == 1 {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        } else {
            error_response(
                "Required grammar is still too large",
                400,
                "invalid_request_error",
            )
        }
    });
    let result = harness::run_to_result(stream(
        &model(),
        &context(vec![
            tool("lookup", Some("prefer")),
            tool("required", Some("require")),
        ]),
        harness.options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Required grammar is still too large"))
    );
    let payloads = request_bodies(&harness.fetch);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[1]["tools"][1]["strict"], Value::Bool(true));
}

#[tokio::test]
async fn honors_cancellation_before_the_fallback_attempt() {
    let token = tokio_util::sync::CancellationToken::new();
    let token_for_handler = token.clone();
    let fetch = MockHttpClient::with_handler(move |_request| {
        token_for_handler.cancel();
        Ok(error_response(GRAMMAR_ERROR, 400, "invalid_request_error"))
    });
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.signal = Some(token);
    let result = harness::run_to_result(stream(
        &model(),
        &context(vec![tool("lookup", Some("prefer"))]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(fetch.requests().len(), 1);
}

#[tokio::test]
async fn does_not_retry_an_sse_error_after_response_headers() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(format!(
        "event: error\ndata: {}\n\n",
        json!({ "type": "error", "error": { "type": "invalid_request_error", "message": GRAMMAR_ERROR } })
    )));
    let options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    let result = harness::run_to_result(stream(
        &model(),
        &context(vec![tool("lookup", Some("prefer"))]),
        options,
    ))
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(fetch.requests().len(), 1);
}

#[tokio::test]
async fn does_not_disable_strict_sampling_on_later_requests() {
    let harness = harness(|_payload, attempt| {
        if attempt == 1 {
            error_response(GRAMMAR_ERROR, 400, "invalid_request_error")
        } else {
            success_response()
        }
    });
    let transcript = context(vec![tool("lookup", Some("prefer"))]);
    let make_options = || {
        let mut base = harness::stream_options(harness.fetch.clone());
        base.max_retries = Some(0);
        AnthropicOptions {
            base,
            ..Default::default()
        }
    };
    let _ = stream(&model(), &transcript, make_options()).result().await;
    // 第二次请求重新开始，strict 应该恢复（模块级状态不得泄漏）。
    let _ = stream(&model(), &transcript, make_options()).result().await;
    let payloads = request_bodies(&harness.fetch);
    assert_eq!(payloads.len(), 3);
    assert_eq!(payloads[0]["tools"][0]["strict"], Value::Bool(true));
    assert!(payloads[1]["tools"][0].get("strict").is_none());
    assert_eq!(payloads[2]["tools"][0]["strict"], Value::Bool(true));
}
