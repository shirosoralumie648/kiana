//! `test/mistral-reasoning-mode.test.ts` 的移植。

use std::sync::{Arc, Mutex};

use kiana_ai::api::mistral_conversations::{MistralOptions, stream_simple};
use kiana_ai::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, ModelThinkingLevel, SimpleStreamOptions,
    StreamOptions, ThinkingLevel, ThinkingLevelMap, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::Value;

struct FailingClient;

impl HttpClient for FailingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move { Err(HttpError::new("connection refused")) })
    }
}

fn make_model(id: &str, reasoning: bool, map: Option<ThinkingLevelMap>) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(reasoning),
        thinking_level_map: map,
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    }
}

fn none_high_levels() -> ThinkingLevelMap {
    let mut map = ThinkingLevelMap::new();
    map.insert(ModelThinkingLevel::Off, Some("none".into()));
    map.insert(ModelThinkingLevel::Minimal, None);
    map.insert(ModelThinkingLevel::Low, None);
    map.insert(ModelThinkingLevel::Medium, None);
    map.insert(ModelThinkingLevel::High, Some("high".into()));
    map.insert(ModelThinkingLevel::Xhigh, None);
    map.insert(ModelThinkingLevel::Max, None);
    map
}

fn glm_5_2_levels() -> ThinkingLevelMap {
    let mut map = none_high_levels();
    map.insert(ModelThinkingLevel::Max, Some("max".into()));
    map
}

fn glm_5_3_levels() -> ThinkingLevelMap {
    let mut map = none_high_levels();
    map.insert(ModelThinkingLevel::Off, None);
    map.insert(ModelThinkingLevel::Low, Some("low".into()));
    map.insert(ModelThinkingLevel::Max, Some("max".into()));
    map
}

fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

async fn capture_payload(model: &Model, options: SimpleStreamOptions) -> Value {
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_cb = Arc::clone(&captured);
    let stream = stream_simple(
        model,
        &context(),
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("fake-key".into()),
                fetch: Some(Arc::new(FailingClient)),
                on_payload: Some(Arc::new(move |payload, _model| {
                    *captured_cb.lock().unwrap() = Some(payload.clone());
                    None
                })),
                ..options.base
            },
            ..options
        },
    )
    .expect("stream");
    let _ = stream.result().await;
    captured
        .lock()
        .unwrap()
        .clone()
        .expect("payload captured before request failure")
}

fn reasoning_options(reasoning: Option<ThinkingLevel>) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions::default(),
        reasoning,
        ..Default::default()
    }
}

#[tokio::test]
async fn uses_prompt_mode_for_reasoning_models_without_a_thinking_level_map() {
    let payload = capture_payload(
        &make_model("magistral-medium-latest", true, None),
        reasoning_options(Some(ThinkingLevel::Medium)),
    )
    .await;
    assert_eq!(payload["promptMode"], serde_json::json!("reasoning"));
    assert!(payload.get("reasoningEffort").is_none());
}

#[tokio::test]
async fn omits_reasoning_controls_for_magistral_when_thinking_is_off() {
    let payload = capture_payload(
        &make_model("magistral-medium-latest", true, None),
        reasoning_options(None),
    )
    .await;
    assert!(payload.get("promptMode").is_none());
    assert!(payload.get("reasoningEffort").is_none());
}

async fn assert_effort_model(model_id: &str, map: ThinkingLevelMap) {
    let payload = capture_payload(
        &make_model(model_id, true, Some(map.clone())),
        reasoning_options(Some(ThinkingLevel::High)),
    )
    .await;
    assert_eq!(payload["reasoningEffort"], serde_json::json!("high"));
    assert!(payload.get("promptMode").is_none());

    let payload = capture_payload(
        &make_model(model_id, true, Some(map.clone())),
        reasoning_options(Some(ThinkingLevel::Low)),
    )
    .await;
    assert_eq!(payload["reasoningEffort"], serde_json::json!("high"));

    let payload = capture_payload(
        &make_model(model_id, true, Some(map)),
        reasoning_options(None),
    )
    .await;
    assert_eq!(payload["reasoningEffort"], serde_json::json!("none"));
    assert!(payload.get("promptMode").is_none());
}

#[tokio::test]
async fn uses_reasoning_effort_for_models_with_a_map() {
    assert_effort_model("mistral-small-2603", none_high_levels()).await;
    assert_effort_model("mistral-medium-latest", none_high_levels()).await;
    assert_effort_model("zai-glm-5-2", glm_5_2_levels()).await;
}

#[tokio::test]
async fn sends_max_for_glm_5_2() {
    let payload = capture_payload(
        &make_model("zai-glm-5-2", true, Some(glm_5_2_levels())),
        reasoning_options(Some(ThinkingLevel::Max)),
    )
    .await;
    assert_eq!(payload["reasoningEffort"], serde_json::json!("max"));
}

#[tokio::test]
async fn sends_reasoning_effort_levels_for_glm_5_3() {
    for level in [ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max] {
        let payload = capture_payload(
            &make_model("zai-glm-5-3", true, Some(glm_5_3_levels())),
            reasoning_options(Some(level)),
        )
        .await;
        let expected = match level {
            ThinkingLevel::Low => "low",
            ThinkingLevel::High => "high",
            _ => "max",
        };
        assert_eq!(payload["reasoningEffort"], serde_json::json!(expected));
        assert!(payload.get("promptMode").is_none());
    }

    let payload = capture_payload(
        &make_model("zai-glm-5-3", true, Some(glm_5_3_levels())),
        reasoning_options(Some(ThinkingLevel::Medium)),
    )
    .await;
    assert_eq!(payload["reasoningEffort"], serde_json::json!("high"));
}

#[tokio::test]
async fn omits_reasoning_controls_for_non_reasoning_models() {
    let payload = capture_payload(
        &make_model("mistral-medium-2505", false, None),
        reasoning_options(Some(ThinkingLevel::Medium)),
    )
    .await;
    assert!(payload.get("reasoningEffort").is_none());
    assert!(payload.get("promptMode").is_none());
}

#[tokio::test]
async fn uses_the_session_id_as_prompt_cache_key() {
    let model = make_model("mistral-large-latest", false, None);
    let mut options = reasoning_options(None);
    options.base.session_id = Some("session-123".into());
    let payload = capture_payload(&model, options).await;
    assert_eq!(payload["promptCacheKey"], serde_json::json!("session-123"));
}

#[tokio::test]
async fn omits_prompt_cache_key_when_cache_retention_is_disabled() {
    let model = make_model("mistral-large-latest", false, None);
    let mut options = reasoning_options(None);
    options.base.session_id = Some("session-123".into());
    options.base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
    let payload = capture_payload(&model, options).await;
    assert!(payload.get("promptCacheKey").is_none());
}

// `MistralOptions` 被引用以确认类型存在（payload 断言走 onPayload 的 camelCase 形态）。
#[allow(dead_code)]
fn _options(_: MistralOptions) {}

#[test]
fn cached_prompt_tokens_skips_an_explicit_null_candidate_like_nullish_coalescing() {
    use kiana_ai::api::mistral_conversations::get_mistral_cached_prompt_tokens;
    use serde_json::json;

    // `??` 跳过 null：第一个形态显式为 null 时继续看后面的候选。
    let usage = json!({
        "promptTokensDetails": {"cachedTokens": null},
        "prompt_tokens_details": {"cached_tokens": 7}
    });
    assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10.0), 7.0);

    let usage = json!({
        "promptTokensDetails": {"cachedTokens": null},
        "numCachedTokens": 4
    });
    assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10.0), 4.0);

    // 全部缺失/null → 0。
    let usage = json!({"promptTokensDetails": {"cachedTokens": null}});
    assert_eq!(get_mistral_cached_prompt_tokens(&usage, 10.0), 0.0);
}
