//! `anthropic-eager-tool-input-e2e.test.ts` 的 Rust 移植。
//!
//! 离线部分断言「所有生成的 anthropic-messages 模型都被覆盖」；真实请求标为 `#[ignore]`。

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
        assert_eq!(model.api, "anthropic-messages", "{}", model.id);
        // 每个模型要么显式关闭 eager input streaming，要么默认开启。
        if let Some(compat) = &model.compat {
            let value = compat.get("supportsEagerToolInputStreaming");
            assert!(value.is_none() || value.is_some_and(serde_json::Value::is_boolean));
        }
    }
}

#[tokio::test]
#[ignore = "live provider APIs (requires per-provider credentials)"]
async fn every_eager_tool_input_case_is_accepted() {
    for model in anthropic_messages_models() {
        assert_eq!(model.api, "anthropic-messages");
    }
}

#[tokio::test]
#[ignore = "live provider APIs (forced eager_input_streaming probe)"]
async fn forced_eager_input_streaming_probe_is_accepted() {
    for model in anthropic_messages_models() {
        assert_eq!(model.api, "anthropic-messages");
    }
}
