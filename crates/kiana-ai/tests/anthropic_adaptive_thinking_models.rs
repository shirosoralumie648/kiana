//! `anthropic-adaptive-thinking-models.test.ts` 的 Rust 移植（模型目录元数据）。

use kiana_ai::types::{Api, Model};

const EXPECTED_CURRENT_ADAPTIVE_THINKING_MODELS: [&str; 16] = [
    "anthropic/claude-fable-5",
    "anthropic/claude-haiku-5-5",
    "anthropic/claude-opus-4-8",
    "anthropic/claude-opus-5",
    "anthropic/claude-sonnet-5",
    "cloudflare-ai-gateway/claude-fable-5",
    "fireworks/accounts/fireworks/models/deepseek-v4p1-flash",
    "fireworks/accounts/fireworks/models/gpt-oss-120b",
    "fireworks/accounts/fireworks/models/qwen3p8-max",
    "minimax-cn/MiniMax-M3.1-Flash",
    "minimax/MiniMax-M3.1-Flash",
    "opencode/claude-opus-4-8",
    "opencode/claude-opus-5",
    "vercel-ai-gateway/anthropic/claude-opus-4.8",
    "vercel-ai-gateway/anthropic/claude-opus-5",
    "vercel-ai-gateway/anthropic/claude-sonnet-5",
];

fn all_models() -> Vec<Model> {
    let providers = kiana_ai::providers::catalog::provider_ids();
    let mut models = Vec::new();
    for provider in providers {
        for value in kiana_ai::providers::catalog::get_models(provider) {
            if let Ok(model) = serde_json::from_value::<Model>(value.clone()) {
                models.push(model);
            }
        }
    }
    models
}

#[test]
fn marks_built_in_anthropic_messages_models_that_use_adaptive_thinking() {
    let mut flagged: Vec<String> = all_models()
        .into_iter()
        .filter(|model| model.api == "anthropic-messages")
        .filter(|model| {
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.get("forceAdaptiveThinking"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect();
    flagged.sort();
    let mut expected: Vec<String> = EXPECTED_CURRENT_ADAPTIVE_THINKING_MODELS
        .iter()
        .map(|id| (*id).to_string())
        .collect();
    expected.sort();
    for id in &expected {
        assert!(flagged.contains(id), "missing adaptive model {id}");
    }
    for id in &flagged {
        let is_expected = id.starts_with("fireworks/")
            || id.starts_with("minimax/")
            || id.starts_with("minimax-cn/")
            || {
                let suffix = id.split_once('/').map(|(_, suffix)| suffix).unwrap_or(id);
                let lower = suffix.to_lowercase().replace('.', "-");
                lower.contains("opus-4-6")
                    || lower.contains("opus-4.6")
                    || lower.contains("opus-4-7")
                    || lower.contains("opus-4.7")
                    || lower.contains("opus-4-8")
                    || lower.contains("opus-4.8")
                    || lower.contains("opus-5")
                    || lower.contains("sonnet-4.6")
                    || lower.contains("sonnet-4-6")
                    || lower.contains("sonnet-5")
                    || lower.contains("haiku-5-5")
                    || lower.contains("fable-5")
            };
        assert!(is_expected, "unexpected adaptive model {id}");
    }
    let _: Option<Api> = None;
}
