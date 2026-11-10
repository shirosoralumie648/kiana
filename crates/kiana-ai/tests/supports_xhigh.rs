//! `supports-xhigh.test.ts` 的 Rust 移植（数据来自 vendored 的模型目录）。

use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::types::{Model, ModelThinkingLevel};

fn get_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id)
        .unwrap_or_else(|| panic!("missing model {provider}/{id}"));
    serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("invalid model {provider}/{id}: {error}"))
}

fn levels(provider: &str, id: &str) -> Vec<ModelThinkingLevel> {
    get_supported_thinking_levels(&get_model(provider, id))
}

fn l(levels: &[ModelThinkingLevel], level: ModelThinkingLevel) -> bool {
    levels.contains(&level)
}

#[test]
fn includes_max_but_not_xhigh_for_anthropic_opus_4_6_on_anthropic_messages_api() {
    let model = get_model("anthropic", "claude-opus-4-6");
    assert!(l(
        &get_supported_thinking_levels(&model),
        ModelThinkingLevel::Max
    ));
    assert!(!l(
        &get_supported_thinking_levels(&model),
        ModelThinkingLevel::Xhigh
    ));
}

#[test]
fn includes_xhigh_and_max_for_anthropic_opus_4_8_on_anthropic_messages_api() {
    let levels = levels("anthropic", "claude-opus-4-8");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
}

#[test]
fn includes_xhigh_and_max_for_anthropic_opus_5_on_anthropic_messages_api() {
    let levels = levels("anthropic", "claude-opus-5");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
}

#[test]
fn includes_claude_opus_5_5_with_its_always_on_effort_levels_and_official_pricing() {
    let model = get_model("anthropic", "claude-opus-5-5");
    assert_eq!(model.cost.input, 4.0);
    assert_eq!(model.cost.output, 20.0);
    assert_eq!(model.cost.cache_read, 0.2);
    assert_eq!(model.cost.cache_write, 5.0);
    assert_eq!(model.context_window, Some(1_000_000));
    assert_eq!(model.max_tokens, Some(128_000));
    let compat = model.compat.as_ref().unwrap();
    assert_eq!(compat["forceAdaptiveThinking"], serde_json::json!(true));
    assert_eq!(compat["supportsMidConvoEffort"], serde_json::json!(true));
    assert_eq!(
        compat["supportsMidConvoSystemMessages"],
        serde_json::json!(true)
    );
    assert_eq!(
        compat["supportsMidConvoToolChanges"],
        serde_json::json!(true)
    );
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn includes_claude_sonnet_5_5_with_managed_effort_levels_and_official_pricing() {
    let model = get_model("anthropic", "claude-sonnet-5-5");
    assert_eq!(model.cost.input, 2.0);
    assert_eq!(model.cost.output, 10.0);
    assert_eq!(model.cost.cache_read, 0.1);
    assert_eq!(model.cost.cache_write, 2.5);
    assert_eq!(model.context_window, Some(1_000_000));
    assert_eq!(model.max_tokens, Some(128_000));
    let compat = model.compat.as_ref().unwrap();
    assert_eq!(compat["forceAdaptiveThinking"], serde_json::json!(true));
    assert_eq!(compat["supportsMidConvoEffort"], serde_json::json!(true));
    assert_eq!(
        compat["supportsMidConvoSystemMessages"],
        serde_json::json!(true)
    );
    assert_eq!(
        compat["supportsMidConvoToolChanges"],
        serde_json::json!(true)
    );
    assert_eq!(compat["supportsTemperature"], serde_json::json!(false));
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn includes_max_but_not_xhigh_for_anthropic_sonnet_4_6_on_anthropic_messages_api() {
    let levels = levels("anthropic", "claude-sonnet-4-6");
    assert!(l(&levels, ModelThinkingLevel::Max));
    assert!(!l(&levels, ModelThinkingLevel::Xhigh));
}

#[test]
fn includes_xhigh_and_max_for_anthropic_sonnet_5_on_anthropic_messages_api() {
    let levels = levels("anthropic", "claude-sonnet-5");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
}

#[test]
fn includes_xhigh_and_max_but_not_off_for_anthropic_claude_fable_5_on_anthropic_messages_api() {
    let levels = levels("anthropic", "claude-fable-5");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
    assert!(!l(&levels, ModelThinkingLevel::Off));
}

#[test]
fn does_not_include_xhigh_or_max_for_claude_sonnet_4_5() {
    let levels = levels("anthropic", "claude-sonnet-4-5");
    assert!(!l(&levels, ModelThinkingLevel::Xhigh));
    assert!(!l(&levels, ModelThinkingLevel::Max));
}

#[test]
fn includes_xhigh_for_openai_codex_models() {
    for model_id in [
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol",
    ] {
        let levels = levels("openai-codex", model_id);
        assert!(l(&levels, ModelThinkingLevel::Xhigh), "{model_id}");
    }
}

#[test]
fn includes_xhigh_and_max_for_openai_models() {
    for model_id in [
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
    ] {
        assert_eq!(
            levels("openai", model_id),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
            "{model_id}"
        );
    }
}

#[test]
fn does_not_support_off_for_gpt_6_1_sol() {
    let expected: [(&str, Vec<ModelThinkingLevel>); 3] = [
        (
            "openai",
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "azure",
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
        ),
        (
            "openai-codex",
            vec![
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh,
                ModelThinkingLevel::Max,
            ],
        ),
    ];
    for (provider, levels_expected) in expected {
        let model = get_model(provider, "gpt-6.1-sol");
        assert_eq!(
            get_supported_thinking_levels(&model),
            levels_expected,
            "{provider}"
        );
        assert_eq!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                .cloned(),
            Some(None),
            "{provider}"
        );
    }
}

#[test]
fn includes_official_metadata_for_openai_and_codex_models() {
    type CostTuple = (f64, f64, f64, f64);
    let cases: [(&str, CostTuple, u64); 3] = [
        ("gpt-6-sol", (2.0, 10.0, 0.2, 2.5), 272_000),
        ("gpt-6-luna", (0.1, 0.5, 0.01, 0.125), 272_000),
        // Codex serves GPT-6.1 Sol with a 500K default context window (#10840)
        ("gpt-6.1-sol", (2.0, 10.0, 0.1, 2.5), 500_000),
    ];
    for (model_id, (input, output, cache_read, cache_write), codex_context_window) in cases {
        for provider in ["openai", "openai-codex"] {
            let model = get_model(provider, model_id);
            assert_eq!(
                model.input,
                vec![
                    kiana_ai::types::ModelInputModality::Text,
                    kiana_ai::types::ModelInputModality::Image
                ],
                "{provider}/{model_id}"
            );
            assert_eq!(model.cost.input, input);
            assert_eq!(model.cost.output, output);
            assert_eq!(model.cost.cache_read, cache_read);
            assert_eq!(model.cost.cache_write, cache_write);
            let tier = &model.cost.tiers.as_ref().unwrap()[0];
            assert_eq!(tier.input_tokens_above, 272_000.0);
            assert_eq!(tier.input, input * 2.0);
            assert_eq!(tier.output, output * 1.5);
            assert_eq!(tier.cache_read, cache_read * 2.0);
            assert_eq!(tier.cache_write, cache_write * 2.0);
            assert_eq!(
                model.context_window,
                Some(if provider == "openai-codex" {
                    codex_context_window
                } else {
                    272_000
                })
            );
            assert_eq!(model.max_tokens, Some(128_000));
            let compat = model.compat.as_ref().unwrap();
            assert_eq!(compat["supportsAdditionalTools"], serde_json::json!(true));
            assert_eq!(
                compat["supportsMidConvoSystemMessages"],
                serde_json::json!(true)
            );
            assert_eq!(
                compat["supportsOpenAIGrammarTools"],
                serde_json::json!(true)
            );
            assert_eq!(compat["supportsToolSearch"], serde_json::json!(true));
        }
    }
}

#[test]
fn includes_only_medium_high_xhigh_for_openai_gpt_5_5_pro() {
    assert_eq!(
        levels("openai", "gpt-5.5-pro"),
        vec![
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
        ]
    );
}

#[test]
fn includes_only_medium_high_xhigh_for_openrouter_gpt_5_5_pro() {
    assert_eq!(
        levels("openrouter", "openai/gpt-5.5-pro"),
        vec![
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
        ]
    );
}

#[test]
fn includes_low_high_max_plus_off_for_deepseek_v4_1_flash_on_the_deepseek_provider() {
    assert_eq!(
        levels("deepseek", "deepseek-flash"),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn preserves_low_high_max_metadata_for_deepseek_v4_1_flash_on_openrouter() {
    assert_eq!(
        levels("openrouter", "deepseek/deepseek-v4.1-flash"),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn preserves_low_high_max_metadata_for_deepseek_v4_1_flash_on_opencode_go() {
    assert_eq!(
        levels("opencode-go", "deepseek-v4.1-flash"),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn excludes_thinking_off_for_moonshot_kimi_k2_7_code_models() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            levels(provider, "kimi-k2.7-code"),
            vec![
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
            ],
            "{provider}"
        );
    }
}

#[test]
fn uses_the_verified_effort_options_for_moonshot_kimi_k3() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            levels(provider, "kimi-k3"),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ],
            "{provider}"
        );
    }
}

#[test]
fn includes_only_low_high_max_for_kimi_coding_k3() {
    assert_eq!(
        levels("kimi-coding", "k3"),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
}

#[test]
fn excludes_off_for_always_thinking_kimi_coding_highspeed() {
    assert!(!l(
        &levels("kimi-coding", "kimi-for-coding-highspeed"),
        ModelThinkingLevel::Off
    ));
}

#[test]
fn includes_only_high_for_opencode_grok_build() {
    assert_eq!(
        levels("opencode", "grok-build-0.1"),
        vec![ModelThinkingLevel::High]
    );
}

#[test]
fn includes_only_high_xhigh_plus_off_for_deepseek_v4_flash_on_openrouter() {
    assert_eq!(
        levels("openrouter", "deepseek/deepseek-v4-flash"),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
        ]
    );
}

#[test]
fn includes_max_but_not_xhigh_for_openrouter_opus_4_6_openai_completions_api() {
    let levels = levels("openrouter", "anthropic/claude-opus-4.6");
    assert!(l(&levels, ModelThinkingLevel::Max));
    assert!(!l(&levels, ModelThinkingLevel::Xhigh));
}

#[test]
fn includes_xhigh_and_max_for_bedrock_claude_opus_5() {
    let levels = levels("amazon-bedrock", "global.anthropic.claude-opus-5");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
}

#[test]
fn includes_xhigh_but_not_off_or_max_for_xai_grok_4_6() {
    assert_eq!(
        levels("xai", "grok-4.6"),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
        ]
    );
}

#[test]
fn includes_xhigh_and_max_but_not_off_for_bedrock_claude_fable_5() {
    let levels = levels("amazon-bedrock", "global.anthropic.claude-fable-5");
    assert!(l(&levels, ModelThinkingLevel::Xhigh));
    assert!(l(&levels, ModelThinkingLevel::Max));
    assert!(!l(&levels, ModelThinkingLevel::Off));
}
