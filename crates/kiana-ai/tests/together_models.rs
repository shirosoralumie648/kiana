//! `packages/ai/test/together-models.test.ts` 的 Rust 移植。
//!
//! 目录读取用 `providers::all`（TS 的 `compat.getModel` 属于 compat 单元，当前是 stub）。

use kiana_ai::env_api_keys::{find_env_keys, get_env_api_key};
use kiana_ai::providers::all::get_builtin_model;
use kiana_ai::types::ModelThinkingLevel;
use serde_json::json;

fn model(provider: &str, id: &str) -> kiana_ai::types::Model {
    get_builtin_model(provider, id)
        .unwrap_or_else(|| panic!("missing catalog model {provider}/{id}"))
}

#[test]
fn registers_the_default_kimi_k3_model_via_openai_compatible_chat_completions() {
    let model = model("together", "moonshotai/Kimi-K3");

    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.provider, "together");
    assert_eq!(model.base_url, "https://api.together.ai/v1");
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(model.input.len(), 2);
    assert_eq!(model.context_window, Some(1_048_576));
    assert_eq!(model.max_tokens, Some(131_072));
    assert_eq!(model.cost.input, 3.0);
    assert_eq!(model.cost.output, 15.0);
    assert_eq!(model.cost.cache_read, 0.3);
    assert_eq!(model.cost.cache_write, 0.0);

    let thinking_map = model
        .thinking_level_map
        .as_ref()
        .expect("thinking level map");
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Low), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Medium), Some(&None));
    assert_eq!(thinking_map.len(), 3);

    assert_eq!(
        model.compat,
        Some(json!({
            "supportsStore": false,
            "supportsDeveloperRole": false,
            "supportsReasoningEffort": false,
            "maxTokensField": "max_tokens",
            "thinkingFormat": "together",
            "supportsStrictMode": false,
            "supportsLongCacheRetention": false,
        }))
    );
}

#[test]
fn models_together_reasoning_controls_from_the_together_api_surface() {
    let gpt_oss = model("together", "openai/gpt-oss-120b");
    let thinking_map = gpt_oss
        .thinking_level_map
        .as_ref()
        .expect("thinking level map");
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Off), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(
        thinking_map.get(&ModelThinkingLevel::Low),
        Some(&Some("low".to_string()))
    );
    assert_eq!(
        thinking_map.get(&ModelThinkingLevel::Medium),
        Some(&Some("medium".to_string()))
    );
    assert_eq!(
        thinking_map.get(&ModelThinkingLevel::High),
        Some(&Some("high".to_string()))
    );
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Max), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Xhigh), Some(&None));

    let compat = gpt_oss.compat.as_ref().expect("compat");
    assert_eq!(compat["supportsReasoningEffort"], json!(true));
    assert_eq!(compat["thinkingFormat"], json!("openai"));

    let deepseek = model("together", "deepseek-ai/DeepSeek-V4-Pro-0813");
    let thinking_map = deepseek
        .thinking_level_map
        .as_ref()
        .expect("thinking level map");
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Low), Some(&None));
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Medium), Some(&None));
    assert_eq!(
        thinking_map.get(&ModelThinkingLevel::High),
        Some(&Some("high".to_string()))
    );
    assert_eq!(thinking_map.get(&ModelThinkingLevel::Xhigh), Some(&None));

    let compat = deepseek.compat.as_ref().expect("compat");
    assert_eq!(compat["supportsReasoningEffort"], json!(true));
    assert_eq!(compat["thinkingFormat"], json!("together"));
}

#[test]
fn resolves_together_api_key_from_the_environment() {
    const VARIABLE: &str = "TOGETHER_API_KEY";
    let original = std::env::var(VARIABLE).ok();
    unsafe { std::env::set_var(VARIABLE, "test-together-key") };

    assert_eq!(find_env_keys("together", None), Some(vec![VARIABLE]));
    assert_eq!(
        get_env_api_key("together", None),
        Some("test-together-key".to_string())
    );

    match original {
        Some(value) => unsafe { std::env::set_var(VARIABLE, value) },
        None => unsafe { std::env::remove_var(VARIABLE) },
    }
}
