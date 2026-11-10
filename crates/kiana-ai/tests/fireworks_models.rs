//! `packages/ai/test/fireworks-models.test.ts` 的 Rust 移植。
//!
//! TS 的本地 `node:http` 捕获服务器由 [`harness`] 的 mock HTTP 客户端替代：
//! 请求仍经过同一条 `anthropic-messages` 构建路径，响应是空 SSE（与 TS 的
//! `writeEmptySseResponse` 一致）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context, context_with_tools, stream_options};
use kiana_ai::api::anthropic_messages::stream_simple as anthropic_stream_simple;
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream as anthropic_stream};
use kiana_ai::api::openai_completions::stream_simple as openai_stream_simple;
use kiana_ai::env_api_keys::{find_env_keys, get_env_api_key};
use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::providers::all::get_builtin_model;
use kiana_ai::types::{
    CacheRetention, Message, Model, ModelCost, ModelInputModality, ModelThinkingLevel,
    SimpleStreamOptions, ThinkingLevel, Tool, TranscriptContext, UserContent, UserMessage,
    UserRole,
};
use serde_json::{Value, json};

fn fireworks_model(id: &str) -> Model {
    get_builtin_model("fireworks", id).unwrap_or_else(|| panic!("missing fireworks/{id}"))
}

fn thinking_map(
    entries: &[(ModelThinkingLevel, Option<&str>)],
) -> kiana_ai::types::ThinkingLevelMap {
    entries
        .iter()
        .map(|(level, value)| (*level, value.map(str::to_string)))
        .collect()
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn user_context(text: &str) -> TranscriptContext {
    context(None, vec![user_message(text)])
}

async fn capture_payload_with(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    cache_retention: Option<CacheRetention>,
    session_id: Option<&str>,
) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut base = stream_options(fetch);
    base.cache_retention = cache_retention;
    base.session_id = session_id.map(str::to_string);
    let mut options = SimpleStreamOptions {
        base,
        reasoning,
        ..Default::default()
    };
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let stream = if model.api == "anthropic-messages" {
        anthropic_stream_simple(model, &user_context("test"), options)
    } else {
        openai_stream_simple(model, &user_context("test"), options)
    }
    .expect("stream starts");
    let _ = stream.result().await;
    captured
        .lock()
        .unwrap()
        .clone()
        .expect("payload captured before the request")
}

async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    capture_payload_with(model, reasoning, None, None).await
}

fn compat_of(model: &Model) -> &Value {
    model.compat.as_ref().expect("model has compat")
}

#[test]
fn registers_non_glm_non_kimi_k3_models_via_anthropic_compatible_messages_api() {
    let model = fireworks_model("accounts/fireworks/models/deepseek-v4p1-flash");
    assert_eq!(model.api, "anthropic-messages");
    assert_eq!(model.provider, "fireworks");
    assert_eq!(model.base_url, "https://api.fireworks.ai/inference");
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(
        model.input,
        vec![ModelInputModality::Text, ModelInputModality::Image]
    );
}

#[test]
fn aligns_glm_5_3_fast_with_glm_5_3s_openai_compatible_config() {
    let base = fireworks_model("accounts/fireworks/models/glm-5p3");
    let fast = fireworks_model("accounts/fireworks/routers/glm-5p3-fast");
    assert_eq!(fast.api, base.api);
    assert_eq!(fast.base_url, base.base_url);
    assert_eq!(fast.compat, base.compat);
    assert_eq!(fast.thinking_level_map, base.thinking_level_map);
}

#[tokio::test]
async fn omits_unsupported_long_cache_retention() {
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
    ] {
        let model = fireworks_model(model_id);
        let payload = capture_payload_with(
            &model,
            None,
            Some(CacheRetention::Long),
            Some("test-fireworks-session"),
        )
        .await;
        assert!(
            payload.get("prompt_cache_retention").is_none(),
            "{model_id}"
        );
    }
}

#[test]
fn routes_kimi_k3_through_the_openai_compatible_api_with_native_effort_controls() {
    let base = fireworks_model("accounts/fireworks/models/kimi-k3");
    let fast = fireworks_model("accounts/fireworks/routers/kimi-k3-fast");
    let compat = json!({
        "supportsStore": false,
        "supportsDeveloperRole": false,
        "supportsStrictMode": true,
        "requiresReasoningContentOnAssistantMessages": true,
        "thinkingFormat": "openai",
        "supportsMidConvoSystemMessages": true,
        "supportsMidConvoToolAdditions": true,
        "sendSessionAffinityHeaders": true,
        "supportsLongCacheRetention": false,
    });
    let thinking_level_map = thinking_map(&[
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low")),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::Xhigh, None),
        (ModelThinkingLevel::Max, Some("max")),
    ]);

    assert_eq!(base.api, "openai-completions");
    assert_eq!(base.base_url, "https://api.fireworks.ai/inference/v1");
    assert_eq!(compat_of(&base), &compat);
    assert_eq!(base.thinking_level_map.as_ref(), Some(&thinking_level_map));
    assert_eq!(fast.api, base.api);
    assert_eq!(fast.base_url, base.base_url);
    assert_eq!(compat_of(&fast), &compat);
    assert_eq!(fast.thinking_level_map.as_ref(), Some(&thinking_level_map));
}

#[tokio::test]
async fn sends_kimi_k3_reasoning_effort_max() {
    let base = fireworks_model("accounts/fireworks/models/kimi-k3");
    let payload = capture_payload(&base, Some(ThinkingLevel::Max)).await;
    assert_eq!(payload["reasoning_effort"], json!("max"));
}

// Regression for #9323: native effort must reach Messages without budget-based fallback.
#[tokio::test]
async fn sends_native_messages_effort_levels() {
    let cases: [(&str, &[ModelThinkingLevel]); 3] = [
        (
            "accounts/fireworks/models/deepseek-v4p1-flash",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "accounts/fireworks/models/qwen3p8-max",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::Xhigh,
            ],
        ),
        (
            "accounts/fireworks/models/qwen3p8-2p4t-a95b",
            &[
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::Xhigh,
            ],
        ),
    ];

    for (model_id, levels) in cases {
        let model = fireworks_model(model_id);
        assert_eq!(model.api, "anthropic-messages");
        assert_eq!(
            compat_of(&model)["forceAdaptiveThinking"],
            Value::Bool(true),
            "{model_id}"
        );
        assert_eq!(
            get_supported_thinking_levels(&model),
            levels.to_vec(),
            "{model_id}"
        );

        for level in levels {
            let reasoning = match level {
                ModelThinkingLevel::Off => None,
                _ => Some(thinking_level_from_model(*level)),
            };
            let payload = capture_payload(&model, reasoning).await;
            if *level == ModelThinkingLevel::Off {
                assert_eq!(
                    payload["thinking"],
                    json!({ "type": "disabled" }),
                    "{model_id}"
                );
                assert!(
                    payload.get("output_config").is_none(),
                    "{model_id} off must not send output_config"
                );
            } else {
                assert_eq!(
                    payload["thinking"],
                    json!({ "type": "adaptive", "display": "summarized" }),
                    "{model_id}/{level:?}"
                );
                assert_eq!(
                    payload["output_config"],
                    json!({ "effort": model_thinking_level_name(*level) }),
                    "{model_id}/{level:?}"
                );
            }
        }
    }
}

fn thinking_level_from_model(level: ModelThinkingLevel) -> ThinkingLevel {
    match level {
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
        ModelThinkingLevel::Off => panic!("off is not a ThinkingLevel"),
    }
}

fn model_thinking_level_name(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

// Regression for #9323: accepted aliases are not distinct native effort levels.
#[test]
fn exposes_distinct_native_effort_levels() {
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
        "accounts/fireworks/models/kimi-k3",
        "accounts/fireworks/routers/kimi-k3-fast",
    ] {
        assert_eq!(
            get_supported_thinking_levels(&fireworks_model(model_id)),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max
            ],
            "{model_id}"
        );
    }
}

#[tokio::test]
async fn keeps_toggle_only_messages_models_on_budget_based_thinking() {
    let model = fireworks_model("accounts/fireworks/models/nemotron-3-ultra-nvfp4");
    assert!(compat_of(&model).get("forceAdaptiveThinking").is_none());
    let payload = capture_payload(&model, Some(ThinkingLevel::High)).await;
    assert_eq!(
        payload["thinking"],
        json!({ "type": "enabled", "budget_tokens": 16384, "display": "summarized" })
    );
    assert!(payload.get("output_config").is_none());
}

#[test]
fn resolves_fireworks_api_key_from_the_environment() {
    const VARIABLE: &str = "FIREWORKS_API_KEY";
    let original = std::env::var(VARIABLE).ok();
    unsafe { std::env::set_var(VARIABLE, "test-fireworks-key") };

    assert_eq!(find_env_keys("fireworks", None), Some(vec![VARIABLE]));
    assert_eq!(
        get_env_api_key("fireworks", None),
        Some("test-fireworks-key".to_string())
    );

    match original {
        Some(value) => unsafe { std::env::set_var(VARIABLE, value) },
        None => unsafe { std::env::remove_var(VARIABLE) },
    }
}

#[test]
fn sets_fireworks_specific_compat_for_session_affinity_and_unsupported_tool_fields() {
    let model = fireworks_model("accounts/fireworks/models/nemotron-3-ultra-nvfp4");
    let compat = compat_of(&model);
    assert_eq!(compat["sendSessionAffinityHeaders"], Value::Bool(true));
    assert_eq!(
        compat["supportsEagerToolInputStreaming"],
        Value::Bool(false)
    );
    assert_eq!(compat["supportsCacheControlOnTools"], Value::Bool(false));
    assert_eq!(compat["supportsLongCacheRetention"], Value::Bool(false));
    assert_eq!(compat["allowEmptySignature"], Value::Bool(true));
}

// ---------------------------------------------------------------------------
// Anthropic-compatible session affinity and tool compat（本地捕获服务器）
// ---------------------------------------------------------------------------

const FIREWORKS_ANTHROPIC_COMPAT: fn() -> Value = || {
    json!({
        "allowEmptySignature": true,
        "sendSessionAffinityHeaders": true,
        "supportsEagerToolInputStreaming": false,
        "supportsCacheControlOnTools": false,
        "supportsLongCacheRetention": false,
    })
};

fn create_fireworks_model(compat: Value) -> Model {
    Model {
        id: "accounts/fireworks/models/kimi-k2p6".into(),
        name: "Kimi K2.6".into(),
        api: "anthropic-messages".into(),
        provider: "fireworks".into(),
        base_url: "http://127.0.0.1:0".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 0.95,
            output: 4.0,
            cache_read: 0.16,
            cache_write: 0.0,
            ..ModelCost::default()
        },
        context_window: Some(262_000),
        max_tokens: Some(262_000),
        compat: Some(compat),
        ..Model::default()
    }
}

fn create_anthropic_model() -> Model {
    Model {
        id: "claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "http://127.0.0.1:0".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        ..Model::default()
    }
}

fn create_openrouter_model() -> Model {
    Model {
        id: "anthropic/claude-opus-4.8".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api".into(),
        ..create_anthropic_model()
    }
}

fn tool() -> Tool {
    Tool {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
        }),
        constrained_sampling: None,
    }
}

fn create_context(tools: Vec<Tool>) -> TranscriptContext {
    context_with_tools(None, vec![user_message("Use the tool")], tools)
}

async fn capture_anthropic_request(
    model: &Model,
    ctx: &TranscriptContext,
    session_id: Option<&str>,
    cache_retention: CacheRetention,
) -> (Vec<(String, String)>, Value) {
    let fetch = MockHttpClient::fixed(MockResponse::sse(""));
    let mut base = stream_options(fetch.clone());
    base.cache_retention = Some(cache_retention);
    base.session_id = session_id.map(str::to_string);
    let stream = anthropic_stream(
        model,
        ctx,
        AnthropicOptions {
            base,
            ..Default::default()
        },
    );
    let _ = stream.result().await;
    let request = fetch
        .requests()
        .into_iter()
        .next()
        .expect("Anthropic request was captured");
    let body = harness::decode_body(request.body.as_deref().expect("body")).expect("json body");
    (request.headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn tools_of(body: &Value) -> &[Value] {
    body["tools"].as_array().expect("tools in request body")
}

#[tokio::test]
async fn sends_x_session_affinity_header_for_fireworks_models() {
    let (headers, _) = capture_anthropic_request(
        &create_fireworks_model(FIREWORKS_ANTHROPIC_COMPAT()),
        &create_context(vec![tool()]),
        Some("fireworks-session-1"),
        CacheRetention::Short,
    )
    .await;
    assert_eq!(
        header(&headers, "x-session-affinity"),
        Some("fireworks-session-1")
    );
}

#[tokio::test]
async fn omits_x_session_affinity_header_for_native_anthropic_models() {
    let (headers, _) = capture_anthropic_request(
        &create_anthropic_model(),
        &create_context(vec![tool()]),
        Some("anthropic-session-1"),
        CacheRetention::Short,
    )
    .await;
    assert_eq!(header(&headers, "x-session-affinity"), None);
}

#[tokio::test]
async fn omits_x_session_affinity_header_when_cache_retention_is_none() {
    let (headers, _) = capture_anthropic_request(
        &create_fireworks_model(FIREWORKS_ANTHROPIC_COMPAT()),
        &create_context(vec![tool()]),
        Some("fireworks-session-2"),
        CacheRetention::None,
    )
    .await;
    assert_eq!(header(&headers, "x-session-affinity"), None);
}

// Regression test for https://github.com/earendil-works/kiana/issues/9102
#[tokio::test]
async fn sends_only_x_session_id_for_openrouter_models() {
    let (headers, _) = capture_anthropic_request(
        &create_openrouter_model(),
        &create_context(vec![tool()]),
        Some("openrouter-session-1"),
        CacheRetention::Short,
    )
    .await;
    assert_eq!(
        header(&headers, "x-session-id"),
        Some("openrouter-session-1")
    );
    assert_eq!(header(&headers, "x-session-affinity"), None);
}

#[tokio::test]
async fn omits_openrouter_session_headers_when_cache_retention_is_none() {
    let (headers, _) = capture_anthropic_request(
        &create_openrouter_model(),
        &create_context(vec![tool()]),
        Some("openrouter-session-2"),
        CacheRetention::None,
    )
    .await;
    assert_eq!(header(&headers, "x-session-id"), None);
    assert_eq!(header(&headers, "x-session-affinity"), None);
}

#[tokio::test]
async fn allows_openrouter_session_headers_to_be_disabled() {
    let mut model = create_openrouter_model();
    model.compat = Some(json!({ "sendSessionAffinityHeaders": false }));
    let (headers, _) = capture_anthropic_request(
        &model,
        &create_context(vec![tool()]),
        Some("openrouter-session-3"),
        CacheRetention::Short,
    )
    .await;
    assert_eq!(header(&headers, "x-session-id"), None);
}

#[tokio::test]
async fn omits_cache_control_on_tools_for_fireworks_models() {
    let (_, body) = capture_anthropic_request(
        &create_fireworks_model(FIREWORKS_ANTHROPIC_COMPAT()),
        &create_context(vec![tool()]),
        None,
        CacheRetention::Short,
    )
    .await;
    let tools = tools_of(&body);
    assert!(tools.last().unwrap().get("cache_control").is_none());
}

#[tokio::test]
async fn omits_eager_input_streaming_on_tools_for_fireworks_models() {
    let (_, body) = capture_anthropic_request(
        &create_fireworks_model(FIREWORKS_ANTHROPIC_COMPAT()),
        &create_context(vec![tool()]),
        None,
        CacheRetention::Short,
    )
    .await;
    for tool in tools_of(&body) {
        assert!(tool.get("eager_input_streaming").is_none());
    }
}

#[tokio::test]
async fn sends_cache_control_on_tools_for_native_anthropic_models() {
    let (_, body) = capture_anthropic_request(
        &create_anthropic_model(),
        &create_context(vec![tool()]),
        None,
        CacheRetention::Short,
    )
    .await;
    let tools = tools_of(&body);
    let last_tool = tools.last().unwrap();
    assert_eq!(last_tool["cache_control"]["type"], json!("ephemeral"));
}

#[tokio::test]
async fn sends_eager_input_streaming_on_tools_for_native_anthropic_models() {
    let (_, body) = capture_anthropic_request(
        &create_anthropic_model(),
        &create_context(vec![tool()]),
        None,
        CacheRetention::Short,
    )
    .await;
    assert_eq!(tools_of(&body)[0]["eager_input_streaming"], json!(true));
}
