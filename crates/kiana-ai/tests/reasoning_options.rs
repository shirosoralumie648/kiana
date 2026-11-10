//! 移植 `packages/ai/test/reasoning-options.test.ts`（`getEffortThinkingLevelMap`）。

mod model_data_support;

use kiana_ai::model_catalog::reasoning_options::{
    ModelsDevReasoningOption, get_effort_thinking_level_map,
};
use kiana_ai::model_catalog::thinking_level_map_to_json;
use serde_json::{Value, json};

fn effort(values: &[Option<&str>]) -> ModelsDevReasoningOption {
    ModelsDevReasoningOption::Effort {
        values: values
            .iter()
            .map(|value| value.map(str::to_string))
            .collect(),
    }
}

fn map_json(options: &[ModelsDevReasoningOption]) -> Value {
    match get_effort_thinking_level_map(options) {
        Some(map) => thinking_level_map_to_json(&map),
        None => Value::Null,
    }
}

#[test]
fn exposes_only_verified_effort_values_and_none() {
    assert_eq!(
        map_json(&[
            ModelsDevReasoningOption::Toggle,
            effort(&[Some("none"), Some("low"), Some("high"), Some("max")])
        ]),
        json!({
            "off": "none",
            "minimal": null,
            "low": "low",
            "medium": null,
            "high": "high",
            "xhigh": null,
            "max": "max",
        })
    );
}

#[test]
fn does_not_infer_thinking_off_from_an_effort_list() {
    assert_eq!(
        map_json(&[effort(&[Some("low"), Some("high"), Some("max")])]),
        json!({
            "off": null,
            "minimal": null,
            "low": "low",
            "medium": null,
            "high": "high",
            "xhigh": null,
            "max": "max",
        })
    );
}

#[test]
fn leaves_toggle_and_budget_controls_for_their_adapter_specific_implementations() {
    assert_eq!(map_json(&[ModelsDevReasoningOption::Toggle]), Value::Null);
    assert_eq!(
        map_json(&[ModelsDevReasoningOption::BudgetTokens {
            min: Some(1024.0),
            max: Some(32000.0)
        }]),
        Value::Null
    );
    // `default` 与 JSON `null` 被有意忽略：只有它们时没有任何可用档位。
    assert_eq!(map_json(&[effort(&[None, Some("default")])]), Value::Null);
}
