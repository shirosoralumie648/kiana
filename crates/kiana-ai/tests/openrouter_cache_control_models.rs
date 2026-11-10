//! `packages/ai/test/openrouter-cache-control-models.test.ts` 的 Rust 移植。

use kiana_ai::providers::all::get_builtin_model;
use serde_json::json;

const OPENROUTER_ANTHROPIC_LATEST_MODEL_IDS: &[&str] = &[
    "~anthropic/claude-fable-latest",
    "~anthropic/claude-haiku-latest",
    "~anthropic/claude-opus-latest",
    "~anthropic/claude-sonnet-latest",
];

#[test]
fn keeps_completions_cache_control_for_anthropic_latest_aliases() {
    for model_id in OPENROUTER_ANTHROPIC_LATEST_MODEL_IDS {
        let model = get_builtin_model("openrouter", model_id)
            .unwrap_or_else(|| panic!("missing openrouter/{model_id}"));
        assert_eq!(model.api, "openai-completions", "{model_id}");
        let compat = model
            .compat
            .unwrap_or_else(|| panic!("missing compat for {model_id}"));
        assert_eq!(
            compat["cacheControlFormat"],
            json!("anthropic"),
            "{model_id}"
        );
    }
}
