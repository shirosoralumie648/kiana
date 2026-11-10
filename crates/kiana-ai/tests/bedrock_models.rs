//! `test/bedrock-models.test.ts` 的移植。
//!
//! 目录数据由 catalog 单元提供；本文件只锁定 Bedrock 目录的两条结构性断言。
//! 逐模型的实网测试（`BEDROCK_EXTENSIVE_MODEL_TEST`）标记 `#[ignore]`。

use kiana_ai::providers::catalog::get_models;

#[test]
fn gets_all_available_bedrock_models() {
    let models = get_models("amazon-bedrock");
    assert!(!models.is_empty());
}

#[test]
fn exposes_claude_opus_5_through_an_inference_profile_only() {
    let models = get_models("amazon-bedrock");
    let ids: Vec<&str> = models
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .collect();
    assert!(ids.contains(&"global.anthropic.claude-opus-5"));
    assert!(!ids.contains(&"anthropic.claude-opus-5"));
}

#[tokio::test]
#[ignore = "needs BEDROCK_EXTENSIVE_MODEL_TEST + AWS credentials"]
async fn makes_a_simple_request_with_every_model() {
    let models = get_models("amazon-bedrock");
    for model_value in models {
        let model: kiana_ai::types::Model =
            serde_json::from_value(model_value.clone()).expect("model");
        let context = kiana_ai::utils::transcript::normalize_context(&kiana_ai::types::Context {
            system_prompt: Some("You are a helpful assistant. Be extremely concise.".into()),
            messages: vec![kiana_ai::types::Message::User(
                kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("Reply with exactly: 'OK'".into()),
                    timestamp: 0,
                },
            )],
            ..Default::default()
        });
        let response = kiana_ai::api::bedrock_converse_stream::stream(
            &model,
            &context,
            kiana_ai::api::bedrock_converse_stream::BedrockOptions::default(),
        )
        .result()
        .await
        .expect("result");
        assert_eq!(response.role, kiana_ai::types::AssistantRole::Assistant);
        assert!(!response.content.is_empty());
        assert!(response.usage.input + response.usage.cache_read > 0);
        assert!(response.usage.output > 0);
        assert!(
            response.error_message.is_none(),
            "{:?}",
            response.error_message
        );
    }
}
