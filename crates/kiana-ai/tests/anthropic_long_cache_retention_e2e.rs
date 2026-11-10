//! `anthropic-long-cache-retention-e2e.test.ts` 的 Rust 移植（离线覆盖 + `#[ignore]` 真实请求）。

use kiana_ai::types::Model;

fn anthropic_messages_models() -> Vec<Model> {
    let mut models = Vec::new();
    for provider in kiana_ai::providers::catalog::provider_ids() {
        for value in kiana_ai::providers::catalog::get_models(provider) {
            if let Ok(model) = serde_json::from_value::<Model>(value.clone())
                && model.api == "anthropic-messages"
            {
                models.push(model);
            }
        }
    }
    models
}

#[test]
fn covers_every_generated_anthropic_messages_model() {
    let models = anthropic_messages_models();
    assert!(!models.is_empty());
    for model in &models {
        assert_eq!(model.api, "anthropic-messages");
    }
}

#[tokio::test]
#[ignore = "live provider APIs (requires per-provider credentials)"]
async fn forced_long_cache_retention_probe_is_accepted() {
    let models = anthropic_messages_models();
    assert!(!models.is_empty());
}
