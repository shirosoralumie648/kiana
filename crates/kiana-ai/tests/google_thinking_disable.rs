//! `test/google-thinking-disable.test.ts` 的移植。
//!
//! 除 Vertex 的实网用例外，其余都是 E2E（需要 `GEMINI_API_KEY` / `ANTHROPIC_API_KEY` /
//! `OPENAI_API_KEY` / `OPENROUTER_API_KEY`）。Rust 侧保留逐字断言并标记 `#[ignore]`。
//!
//! 本文件还覆盖“关闭 thinking 时发什么配置”的离线部分（`getDisabledGoogleThinkingConfig`）。

use kiana_ai::api::google_shared::get_disabled_google_thinking_config;
use kiana_ai::types::{Model, ModelInputModality, ModelThinkingLevel, ThinkingLevelMap};
use serde_json::json;

fn model(id: &str, map: Option<ThinkingLevelMap>) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "google-generative-ai".into(),
        provider: "google".into(),
        base_url: "https://example.com".into(),
        reasoning: Some(true),
        thinking_level_map: map,
        input: vec![ModelInputModality::Text],
        context_window: Some(1_000_000),
        max_tokens: Some(65_536),
        ..Model::default()
    }
}

#[test]
fn disabled_thinking_uses_budget_zero_for_budget_models() {
    assert_eq!(
        get_disabled_google_thinking_config(&model("gemini-2.5-flash", None)).expect("config"),
        json!({"thinkingBudget": 0})
    );
}

#[test]
fn disabled_thinking_uses_budget_zero_when_off_is_supported() {
    let map: ThinkingLevelMap = [(ModelThinkingLevel::Off, Some("none".to_string()))]
        .into_iter()
        .collect();
    assert_eq!(
        get_disabled_google_thinking_config(&model("gemini-3-flash-preview", Some(map)))
            .expect("config"),
        json!({"thinkingBudget": 0})
    );
}

#[test]
fn disabled_thinking_uses_the_lowest_level_when_off_is_unsupported() {
    let map: ThinkingLevelMap = [
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low".to_string())),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        get_disabled_google_thinking_config(&model("gemini-3-flash-preview", Some(map)))
            .expect("config"),
        json!({"thinkingLevel": "LOW"})
    );
}

#[tokio::test]
#[ignore = "needs GEMINI_API_KEY (live API)"]
async fn disables_thinking_for_gemini_25_e2e() {
    // 逐字移植的 E2E：断言没有 thinking 事件、文本是 40 个 pong。
    let model = model("gemini-2.5-flash", None);
    let stream = kiana_ai::api::google_generative_ai::stream_simple(
        &model,
        &kiana_ai::utils::transcript::normalize_context(&kiana_ai::types::Context {
            system_prompt: Some(
                "You are a precise assistant. Follow the requested output format exactly.".into(),
            ),
            messages: vec![kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text(
                    "Before replying, carefully solve 36863 * 5279 internally. Then reply with the word pong repeated exactly 40 times, separated by single spaces. Do not add any other text.".into(),
                ),
                timestamp: 0,
            })],
            ..Default::default()
        }),
        kiana_ai::types::SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                max_tokens: Some(160),
                temperature: Some(0.0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("stream");
    let mut thinking_events = 0;
    let events = stream.clone();
    while let Some(event) = events.recv().await {
        if matches!(
            event,
            kiana_ai::types::AssistantMessageEvent::ThinkingStart { .. }
                | kiana_ai::types::AssistantMessageEvent::ThinkingEnd { .. }
                | kiana_ai::types::AssistantMessageEvent::ThinkingDelta { .. }
        ) {
            thinking_events += 1;
        }
    }
    let response = stream.result().await.expect("result");
    assert_eq!(
        response.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        response.error_message
    );
    assert_eq!(thinking_events, 0);
    assert!(
        !response
            .content
            .iter()
            .any(|block| matches!(block, kiana_ai::types::AssistantContentBlock::Thinking(_)))
    );
}

#[tokio::test]
#[ignore = "needs GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT + GOOGLE_CLOUD_LOCATION or GOOGLE_CLOUD_API_KEY"]
async fn disables_thinking_for_vertex_gemini_25_e2e() {
    let mut model = model("gemini-2.5-flash", None);
    model.api = "google-vertex".into();
    model.provider = "google-vertex".into();
    let stream = kiana_ai::api::google_vertex::stream_simple(
        &model,
        &kiana_ai::utils::transcript::normalize_context(&kiana_ai::types::Context {
            messages: vec![kiana_ai::types::Message::User(
                kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("Reply with pong.".into()),
                    timestamp: 0,
                },
            )],
            ..Default::default()
        }),
        kiana_ai::types::SimpleStreamOptions::default(),
    )
    .expect("stream");
    let response = stream.result().await.expect("result");
    assert_ne!(
        response.stop_reason,
        kiana_ai::types::StopReason::Error,
        "{:?}",
        response.error_message
    );
}

#[test]
fn disabled_thinking_resolves_the_fallback_through_the_model_mapping() {
    // map 只保留 high，且 high 映射到非同名值：TS 走 resolveGoogleThinkingLevel(model, "high")
    // → "minimal" → "MINIMAL"（不是未映射的 "HIGH"）。
    let map: ThinkingLevelMap = [
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, None),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("minimal".to_string())),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        get_disabled_google_thinking_config(&model("gemini-3-flash-preview", Some(map)))
            .expect("config"),
        json!({"thinkingLevel": "MINIMAL"})
    );
}

#[test]
fn disabled_thinking_rejects_an_unsupported_fallback_mapping() {
    let map: ThinkingLevelMap = [
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, None),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("ultra".to_string())),
    ]
    .into_iter()
    .collect();
    let error = get_disabled_google_thinking_config(&model("gemini-3-flash-preview", Some(map)))
        .expect_err("unsupported mapping");
    assert_eq!(
        error.message,
        "Unsupported Google thinking level mapping for google/gemini-3-flash-preview: high -> ultra"
    );
}
