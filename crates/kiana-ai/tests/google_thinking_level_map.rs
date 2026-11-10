//! `test/google-thinking-level-map.test.ts` 的移植。
//!
//! TS 用 `onPayload` 抛错截获 payload；Rust 的 `onPayload` 不能抛错，改为捕获后让请求连接失败
//! （`http://127.0.0.1:1`），断言 payload 内容不变。

use std::sync::{Arc, Mutex};

use kiana_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use kiana_ai::api::google_shared::resolve_google_thinking_level;
use kiana_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, ModelThinkingLevel, SimpleStreamOptions,
    StreamOptions, ThinkingBudgets, ThinkingLevel, ThinkingLevelMap, UserContent, UserMessage,
    UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

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

fn thinking_map(pairs: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    pairs
        .iter()
        .map(|(level, value)| (*level, value.map(str::to_string)))
        .collect()
}

fn google_model(id: &str, map: ThinkingLevelMap) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "google-generative-ai".into(),
        provider: "test-google".into(),
        base_url: "http://127.0.0.1:1/v1beta".into(),
        reasoning: Some(true),
        thinking_level_map: Some(map),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

fn vertex_model(id: &str, map: ThinkingLevelMap) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "google-vertex".into(),
        provider: "test-vertex".into(),
        base_url: "http://127.0.0.1:1/v1".into(),
        reasoning: Some(true),
        thinking_level_map: Some(map),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

fn stream_options_with(payload: Arc<Mutex<Option<Value>>>) -> StreamOptions {
    StreamOptions {
        api_key: Some("test".into()),
        on_payload: Some(Arc::new(move |request, _model| {
            *payload.lock().unwrap() = Some(request.clone());
            None
        })),
        ..Default::default()
    }
}

async fn capture_google_payload(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
) -> Value {
    let payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let result = stream_simple_google(
        model,
        &context(),
        SimpleStreamOptions {
            base: stream_options_with(Arc::clone(&payload)),
            reasoning,
            thinking_budgets,
            ..Default::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");
    assert!(result.error_message.is_some());
    payload.lock().unwrap().clone().expect("payload captured")
}

async fn capture_vertex_payload(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
) -> Value {
    let payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let result = stream_simple_vertex(
        model,
        &context(),
        SimpleStreamOptions {
            base: stream_options_with(Arc::clone(&payload)),
            reasoning,
            thinking_budgets,
            ..Default::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");
    assert!(result.error_message.is_some());
    payload.lock().unwrap().clone().expect("payload captured")
}

#[tokio::test]
async fn exhaustively_resolves_supported_logical_levels_and_mapping_values() {
    let empty = thinking_map(&[]);
    for (level, expected) in [
        (ThinkingLevel::Minimal, "minimal"),
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::High, "high"),
    ] {
        let resolved =
            resolve_google_thinking_level(&google_model("gemini-3.7-flash", empty.clone()), level)
                .expect("resolved");
        assert_eq!(resolved.as_str(), expected);
    }

    for (mapped, expected) in [
        ("minimal", "minimal"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("MINIMAL", "minimal"),
        ("LOW", "low"),
        ("MEDIUM", "medium"),
        ("HIGH", "high"),
    ] {
        let map = thinking_map(&[
            (ModelThinkingLevel::High, Some(mapped)),
            (ModelThinkingLevel::Xhigh, Some(mapped)),
            (ModelThinkingLevel::Max, Some(mapped)),
        ]);
        let model = google_model("gemini-3.7-flash", map);
        for level in [
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ] {
            assert_eq!(
                resolve_google_thinking_level(&model, level)
                    .expect("resolved")
                    .as_str(),
                expected,
                "{mapped} / {level:?}"
            );
        }
    }

    let invalid = google_model(
        "gemini-3.7-flash",
        thinking_map(&[(ModelThinkingLevel::Xhigh, Some("extreme"))]),
    );
    let error = resolve_google_thinking_level(&invalid, ThinkingLevel::Xhigh).expect_err("invalid");
    assert_eq!(
        error.message,
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: xhigh -> extreme"
    );
    let error = resolve_google_thinking_level(
        &google_model("gemini-3.7-flash", thinking_map(&[])),
        ThinkingLevel::Max,
    )
    .expect_err("invalid");
    assert_eq!(
        error.message,
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: max -> undefined"
    );
}

#[tokio::test]
async fn uses_the_lowest_supported_level_when_reasoning_is_omitted() {
    let map = thinking_map(&[
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low")),
        (ModelThinkingLevel::Medium, Some("medium")),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::Xhigh, None),
        (ModelThinkingLevel::Max, None),
    ]);
    let payload =
        capture_google_payload(&google_model("gemini-3.8-flash", map.clone()), None, None).await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"thinkingLevel": "LOW"})
    );
    let payload = capture_vertex_payload(&vertex_model("gemini-3.8-flash", map), None, None).await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"thinkingLevel": "LOW"})
    );
}

#[tokio::test]
async fn preserves_native_medium_effort_for_gemini_31_pro() {
    let map = thinking_map(&[
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low")),
        (ModelThinkingLevel::Medium, Some("medium")),
        (ModelThinkingLevel::High, Some("high")),
        (ModelThinkingLevel::Xhigh, None),
        (ModelThinkingLevel::Max, None),
    ]);
    let payload = capture_google_payload(
        &google_model("gemini-3.1-pro-preview", map.clone()),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"})
    );
    let payload = capture_vertex_payload(
        &vertex_model("gemini-3.1-pro-preview", map),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"})
    );
}

#[tokio::test]
async fn disables_gemini_25_thinking_when_reasoning_is_omitted() {
    let payload = capture_google_payload(
        &google_model("gemini-2.5-flash", thinking_map(&[])),
        None,
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"thinkingBudget": 0})
    );
    let payload = capture_vertex_payload(
        &vertex_model("gemini-2.5-flash", thinking_map(&[])),
        None,
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"thinkingBudget": 0})
    );
}

#[tokio::test]
async fn uses_thinking_levels_for_gemini_4_models() {
    let payload = capture_google_payload(
        &google_model("gemini-4-argon", thinking_map(&[])),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"})
    );
    let payload = capture_vertex_payload(
        &vertex_model("gemini-4-argon", thinking_map(&[])),
        Some(ThinkingLevel::Medium),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"})
    );
}

#[tokio::test]
async fn maps_extended_levels_to_supported_levels() {
    let map = thinking_map(&[
        (ModelThinkingLevel::Xhigh, Some("high")),
        (ModelThinkingLevel::Max, Some("high")),
    ]);
    for level in [ThinkingLevel::Xhigh, ThinkingLevel::Max] {
        let payload = capture_google_payload(
            &google_model("gemini-3.7-flash", map.clone()),
            Some(level),
            None,
        )
        .await;
        assert_eq!(
            payload["config"]["thinkingConfig"],
            json!({"includeThoughts": true, "thinkingLevel": "HIGH"})
        );
    }
    let payload = capture_vertex_payload(
        &vertex_model(
            "gemini-3.7-flash",
            thinking_map(&[(ModelThinkingLevel::Xhigh, Some("high"))]),
        ),
        Some(ThinkingLevel::Xhigh),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "HIGH"})
    );
}

#[tokio::test]
async fn honors_uppercase_provider_values() {
    let payload = capture_google_payload(
        &google_model(
            "gemini-3.7-flash",
            thinking_map(&[(ModelThinkingLevel::High, Some("LOW"))]),
        ),
        Some(ThinkingLevel::High),
        None,
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "LOW"})
    );
}

#[tokio::test]
async fn uses_mapped_levels_for_token_budgets() {
    let payload = capture_google_payload(
        &google_model(
            "gemini-2.5-flash",
            thinking_map(&[(ModelThinkingLevel::Xhigh, Some("high"))]),
        ),
        Some(ThinkingLevel::Xhigh),
        Some(ThinkingBudgets {
            high: Some(1234),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingBudget": 1234})
    );
    let payload = capture_vertex_payload(
        &vertex_model(
            "gemini-2.5-flash",
            thinking_map(&[(ModelThinkingLevel::Max, Some("high"))]),
        ),
        Some(ThinkingLevel::Max),
        Some(ThinkingBudgets {
            high: Some(4321),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        payload["config"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingBudget": 4321})
    );
}
