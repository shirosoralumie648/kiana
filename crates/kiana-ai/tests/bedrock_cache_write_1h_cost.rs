//! `test/bedrock-cache-write-1h-cost.test.ts` 的移植。

use serde_json::json;

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream_with_items};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, UserContent, UserMessage, UserRole,
};

fn model() -> Model {
    Model {
        id: "us.anthropic.claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
            ..Default::default()
        },
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

#[tokio::test]
async fn prices_the_1h_cache_details_at_2x_while_preserving_the_total_cache_write() {
    let context = kiana_ai::utils::transcript::normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hi".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });
    let result = stream_with_items(
        &model(),
        &context,
        BedrockOptions {
            base: kiana_ai::types::StreamOptions {
                cache_retention: Some(kiana_ai::types::CacheRetention::None),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
        vec![
            Ok(json!({"messageStart": {"role": "assistant"}})),
            Ok(json!({"metadata": {"usage": {
                "inputTokens": 100,
                "outputTokens": 5,
                "totalTokens": 1_000_105,
                "cacheWriteInputTokens": 1_000_000,
                "cacheDetails": [
                    {"ttl": "1h", "inputTokens": 150_000},
                    {"ttl": "5m", "inputTokens": 600_000},
                    {"ttl": "1h", "inputTokens": 250_000}
                ]
            }}})),
            Ok(json!({"messageStop": {"stopReason": "end_turn"}})),
        ],
    )
    .result()
    .await
    .expect("result");

    assert_eq!(result.usage.cache_write, 1_000_000);
    assert_eq!(result.usage.cache_write1h, Some(400_000));
    let model = model();
    let expected =
        (600_000.0 * model.cost.cache_write + 400_000.0 * model.cost.input * 2.0) / 1_000_000.0;
    assert!((result.usage.cost.cache_write - expected).abs() < 1e-10);
}
