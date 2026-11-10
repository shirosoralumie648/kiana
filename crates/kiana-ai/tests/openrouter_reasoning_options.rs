//! 移植 `packages/ai/test/openrouter-reasoning-options.test.ts` 的 `getOpenRouterThinkingLevelMap` 部分。
//!
//! TS 里同一文件还有 3 个 `streamSimple` payload 用例（`OpenRouter mandatory reasoning payloads`），
//! 它们验证的是 openai-completions 适配层发出的 `reasoning.effort`；`src/api/openai-completions.ts`
//! 属于 provider 适配器单元，本单元只移植目录侧的映射函数。

mod model_data_support;

use kiana_ai::model_catalog::reasoning_options::{
    OpenRouterReasoningMetadata, get_openrouter_thinking_level_map,
};
use kiana_ai::model_catalog::thinking_level_map_to_json;
use serde_json::{Value, json};

fn metadata(value: Value) -> OpenRouterReasoningMetadata {
    OpenRouterReasoningMetadata::from_json(&value).expect("metadata object")
}

fn map_json(metadata: Option<&OpenRouterReasoningMetadata>) -> Value {
    match get_openrouter_thinking_level_map(metadata) {
        Some(map) => thinking_level_map_to_json(&map),
        None => Value::Null,
    }
}

#[test]
fn marks_mandatory_reasoning_and_unsupported_efforts_unavailable() {
    let reasoning = metadata(json!({
        "mandatory": true,
        "default_enabled": true,
        "supported_efforts": ["max", "high", "low"],
        "default_effort": "max",
    }));
    assert_eq!(
        map_json(Some(&reasoning)),
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
fn still_marks_off_unavailable_when_openrouter_omits_effort_metadata() {
    let reasoning = metadata(json!({ "mandatory": true }));
    assert_eq!(map_json(Some(&reasoning)), json!({ "off": null }));
}

#[test]
fn keeps_off_available_while_restricting_optional_models_to_supported_efforts() {
    let reasoning = metadata(json!({
        "mandatory": false,
        "default_enabled": true,
        "supported_efforts": ["high", "low"],
    }));
    assert_eq!(
        map_json(Some(&reasoning)),
        json!({
            "off": "none",
            "minimal": null,
            "low": "low",
            "medium": null,
            "high": "high",
            "xhigh": null,
            "max": null,
        })
    );
}

#[test]
fn does_not_add_metadata_for_optional_models_without_effort_controls() {
    let reasoning = metadata(json!({ "mandatory": false }));
    assert_eq!(map_json(Some(&reasoning)), Value::Null);
    assert_eq!(map_json(None), Value::Null);
}
