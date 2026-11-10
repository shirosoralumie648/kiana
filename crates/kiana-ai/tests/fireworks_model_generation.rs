//! 移植 `packages/ai/test/fireworks-model-generation.test.ts`。
//!
//! TS 侧用 `spawnSync(node --import mock-catalog.mjs scripts/generate-models.ts --json-only
//! --json-output <dir>)`，断言 `<dir>/providers/fireworks.json`。Rust 侧直接调用生成管线的
//! 纯函数部分（[`generate_catalog`]），断言同一个 provider 的 chat 表。
//!
//! 只走目录生成，不涉及 `streamSimple`/`normalizeContext` 那条断言的网络路径（那条断言验证的是
//! provider 适配层，属于其它单元）。

mod model_data_support;

use kiana_ai::model_catalog::generator::{
    GeneratorInputs, GeneratorOptions, GeneratorState, generate_catalog,
};
use kiana_ai::model_catalog::reasoning_options::ModelsDevReasoningOption;
use serde_json::{Value, json};

const FIREWORKS_BASE_ID: &str = "accounts/fireworks/models/";

/// `generateFireworksModels(options)`：用最小上游快照跑一遍目录生成。
fn generate_fireworks_models(
    options: &[(&str, Option<Vec<ModelsDevReasoningOption>>)],
) -> serde_json::Map<String, Value> {
    let mut models = serde_json::Map::new();
    for (id, reasoning_options) in options {
        let mut entry = serde_json::Map::new();
        entry.insert("id".to_string(), json!(id));
        entry.insert("tool_call".to_string(), json!(true));
        entry.insert("reasoning".to_string(), json!(true));
        let options_value = match reasoning_options {
            Some(options) => Value::Array(
                options
                    .iter()
                    .map(|option| match option {
                        ModelsDevReasoningOption::Toggle => json!({ "type": "toggle" }),
                        ModelsDevReasoningOption::Effort { values } => json!({
                            "type": "effort",
                            "values": values
                                .iter()
                                .map(|value| match value {
                                    Some(value) => Value::String(value.clone()),
                                    None => Value::Null,
                                })
                                .collect::<Vec<_>>(),
                        }),
                        ModelsDevReasoningOption::BudgetTokens { min, max } => {
                            let mut object = serde_json::Map::new();
                            object.insert("type".to_string(), json!("budget_tokens"));
                            if let Some(min) = min {
                                object.insert("min".to_string(), json!(min));
                            }
                            if let Some(max) = max {
                                object.insert("max".to_string(), json!(max));
                            }
                            Value::Object(object)
                        }
                    })
                    .collect(),
            ),
            None => continue,
        };
        entry.insert("reasoning_options".to_string(), options_value);
        models.insert(format!("{FIREWORKS_BASE_ID}{id}"), Value::Object(entry));
    }

    let mut fireworks_provider = serde_json::Map::new();
    fireworks_provider.insert("models".to_string(), Value::Object(models));

    let mut models_dev = serde_json::Map::new();
    models_dev.insert(
        "fireworks-ai".to_string(),
        Value::Object(fireworks_provider),
    );

    let inputs = GeneratorInputs {
        models_dev: Some(Value::Object(models_dev)),
        models_dev_decision: Some(json!({
            "typesafe/jev-latest": { "name": "Jev", "type": "decision", "limit": { "context": 64000, "output": 0 } }
        })),
        radius_config: Some(json!({
            "baseUrl": "https://radius.kiana.dev",
            "models": [{
                "id": "test",
                "name": "Test",
                "reasoning": false,
                "input": ["text"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 4096,
                "maxTokens": 4096
            }]
        })),
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let catalog = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
        .expect("catalog generation should succeed");
    let fireworks = catalog
        .providers
        .get("fireworks")
        .expect("fireworks provider should be present");
    let mut chat = serde_json::Map::new();
    for (id, model) in &fireworks.chat {
        chat.insert(id.clone(), model.clone());
    }
    chat
}

/// 取 `thinkingLevelMap` 的 JSON（缺失为 `None`）。
fn thinking_level_map(model: &Value) -> Option<&Value> {
    model.get("thinkingLevelMap")
}

fn off_none_minimal_null_low_high_max() -> Value {
    json!({
        "off": "none",
        "minimal": null,
        "low": "low",
        "medium": null,
        "high": "high",
        "xhigh": null,
        "max": "max",
    })
}

fn off_none_low_medium_xhigh() -> Value {
    json!({
        "off": "none",
        "minimal": null,
        "low": "low",
        "medium": "medium",
        "high": null,
        "xhigh": "xhigh",
        "max": null,
    })
}

fn toggle() -> Option<Vec<ModelsDevReasoningOption>> {
    Some(vec![ModelsDevReasoningOption::Toggle])
}

fn effort(values: &[&str]) -> Option<Vec<ModelsDevReasoningOption>> {
    Some(vec![ModelsDevReasoningOption::Effort {
        values: values
            .iter()
            .map(|value| Some((*value).to_string()))
            .collect(),
    }])
}

fn toggle_plus_effort(values: &[&str]) -> Option<Vec<ModelsDevReasoningOption>> {
    let mut options = vec![ModelsDevReasoningOption::Toggle];
    options.push(ModelsDevReasoningOption::Effort {
        values: values
            .iter()
            .map(|value| Some((*value).to_string()))
            .collect(),
    });
    Some(options)
}

#[test]
fn combines_upstream_effort_and_toggle_metadata_with_narrow_corrections() {
    let models = generate_fireworks_models(&[
        (
            "deepseek-v4-flash-0731",
            toggle_plus_effort(&["low", "high", "max"]),
        ),
        (
            "deepseek-v4-flash-vision-exp",
            toggle_plus_effort(&["low", "high", "max"]),
        ),
        ("deepseek-v4-pro-0813", toggle_plus_effort(&["high", "max"])),
        ("qwen3p8-max", toggle()),
        ("qwen3p8-2p4t-a95b", effort(&["low", "medium", "xhigh"])),
        ("kimi-k2p6", toggle()),
    ]);

    for id in [
        "deepseek-v4-flash-0731",
        "deepseek-v4-flash-vision-exp",
        "deepseek-v4-pro-0813",
    ] {
        let model = &models[&format!("{FIREWORKS_BASE_ID}{id}")];
        assert_eq!(
            thinking_level_map(model),
            Some(&off_none_minimal_null_low_high_max()),
            "{id}"
        );
    }
    for id in ["qwen3p8-max", "qwen3p8-2p4t-a95b"] {
        let model = &models[&format!("{FIREWORKS_BASE_ID}{id}")];
        assert_eq!(
            thinking_level_map(model),
            Some(&off_none_low_medium_xhigh()),
            "{id}"
        );
    }
    for model in models.values() {
        assert_eq!(
            model.get("api").and_then(Value::as_str),
            Some("anthropic-messages")
        );
        let compat = model.get("compat").expect("compat");
        assert_eq!(
            compat.get("allowEmptySignature").and_then(Value::as_bool),
            Some(true)
        );
        let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
        let expected = if id.ends_with("kimi-k2p6") {
            None
        } else {
            Some(true)
        };
        assert_eq!(
            compat.get("forceAdaptiveThinking").and_then(Value::as_bool),
            expected,
            "{id}"
        );
    }
    assert_eq!(
        thinking_level_map(&models[&format!("{FIREWORKS_BASE_ID}kimi-k2p6")]),
        None
    );
}

#[test]
fn does_not_infer_adaptive_thinking_from_toggle_budget_or_missing_metadata() {
    let models = generate_fireworks_models(&[
        ("toggle-only", toggle()),
        (
            "budget-only",
            Some(vec![ModelsDevReasoningOption::BudgetTokens {
                min: Some(1024.0),
                max: None,
            }]),
        ),
        ("fixed-reasoning", Some(Vec::new())),
        ("missing-metadata", None),
    ]);
    for model in models.values() {
        let compat = model.get("compat").expect("compat");
        assert_eq!(
            compat.get("forceAdaptiveThinking").and_then(Value::as_bool),
            None
        );
        assert_eq!(thinking_level_map(model), None);
    }
}

#[test]
fn prefers_updated_upstream_efforts_over_fixed_maps_and_the_qwen_fallback() {
    let models = generate_fireworks_models(&[
        ("deepseek-v4-flash-0731", effort(&["high", "max"])),
        ("qwen3p8-max", toggle_plus_effort(&["medium", "xhigh"])),
    ]);
    assert_eq!(
        thinking_level_map(&models[&format!("{FIREWORKS_BASE_ID}deepseek-v4-flash-0731")]),
        Some(&json!({
            "off": null,
            "minimal": null,
            "low": null,
            "medium": null,
            "high": "high",
            "xhigh": null,
            "max": "max",
        }))
    );
    assert_eq!(
        thinking_level_map(&models[&format!("{FIREWORKS_BASE_ID}qwen3p8-max")]),
        Some(&json!({
            "off": "none",
            "minimal": null,
            "low": null,
            "medium": "medium",
            "high": null,
            "xhigh": "xhigh",
            "max": null,
        }))
    );
}

/// TS 用例 2 的目录侧断言：新目录的 Messages 模型无需加入白名单也拿到
/// `forceAdaptiveThinking`，且 `thinkingLevelMap` 只暴露 upstream 声明的档位。
///
/// TS 同一用例后半段用 `streamSimple` + `onPayload` 断言发出的
/// `thinking: {type:"adaptive", display:"summarized"}` 与 `output_config: {effort:"max"}`；
/// 那是 openai-completions / anthropic-messages 适配层的职责（provider 单元），不在本单元。
#[test]
fn sends_native_effort_for_newly_cataloged_messages_models() {
    let models =
        generate_fireworks_models(&[("new-reasoner", toggle_plus_effort(&["low", "max"]))]);
    let model = &models[&format!("{FIREWORKS_BASE_ID}new-reasoner")];
    assert_eq!(
        model.get("api").and_then(Value::as_str),
        Some("anthropic-messages")
    );
    let compat = model.get("compat").expect("compat");
    assert_eq!(
        compat.get("forceAdaptiveThinking").and_then(Value::as_bool),
        Some(true)
    );
    // `getSupportedThinkingLevels(model)` == ["off", "low", "max"]：
    // 只有映射到具体字符串的档位可选。
    let map = thinking_level_map(model).expect("thinkingLevelMap");
    let supported: Vec<&str> = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| map.get(*level).and_then(Value::as_str).is_some())
        .collect();
    assert_eq!(supported, vec!["off", "low", "max"]);
}
