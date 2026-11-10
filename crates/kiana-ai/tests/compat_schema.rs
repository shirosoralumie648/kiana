//! `packages/ai/test/compat-schema.test.ts` 的 Rust 移植 + TypeBox 快照回归。
//!
//! TS 的 `Compile(schema).Default({})` 用 TypeBox 运行时；Rust 侧由
//! [`kiana_ai::providers::model_schema::apply_defaults`] 实现同一语义。golden fixture
//! `tests/fixtures/providers/schemas.json` 由 TS 模块导出直接生成（含 TypeBox 的默认值结果）。

use kiana_ai::providers::compat_schema::{
    anthropic_messages_compat_schema, bedrock_compat_schema, compat_schemas_by_api,
    merge_compat_properties, open_ai_completions_compat_schema, open_ai_responses_compat_schema,
    provider_compat_property_overrides, provider_compat_schema, session_affinity_format_schema,
};
use kiana_ai::providers::model_schema::{
    apply_defaults, cache_retention_schema, check, model_cost_rates_schema, model_cost_schema,
    model_cost_tier_schema, model_image_input_limits_schema, model_image_resize_options_schema,
    model_input_limits_schema, model_input_modality_schema, model_prompt_cache_schema,
    model_thinking_level_schema, thinking_level_map_schema, thinking_level_schema,
};
use serde_json::{Map, Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/providers/schemas.json")).expect("schema fixture")
}

fn model_schemas() -> Vec<(&'static str, &'static Value)> {
    vec![
        ("ThinkingLevelSchema", thinking_level_schema()),
        ("ModelThinkingLevelSchema", model_thinking_level_schema()),
        ("ModelInputModalitySchema", model_input_modality_schema()),
        ("CacheRetentionSchema", cache_retention_schema()),
        ("ThinkingLevelMapSchema", thinking_level_map_schema()),
        ("ModelPromptCacheSchema", model_prompt_cache_schema()),
        ("ModelCostRatesSchema", model_cost_rates_schema()),
        ("ModelCostTierSchema", model_cost_tier_schema()),
        ("ModelCostSchema", model_cost_schema()),
        (
            "ModelImageResizeOptionsSchema",
            model_image_resize_options_schema(),
        ),
        (
            "ModelImageInputLimitsSchema",
            model_image_input_limits_schema(),
        ),
        ("ModelInputLimitsSchema", model_input_limits_schema()),
    ]
}

fn compat_schemas() -> Vec<(&'static str, &'static Value)> {
    vec![
        (
            "SessionAffinityFormatSchema",
            session_affinity_format_schema(),
        ),
        (
            "ThinkingTokenBudgetFieldSchema",
            kiana_ai::providers::compat_schema::thinking_token_budget_field_schema(),
        ),
        (
            "ChatTemplateKwargValueSchema",
            kiana_ai::providers::compat_schema::chat_template_kwarg_value_schema(),
        ),
        (
            "OpenRouterRoutingSchema",
            kiana_ai::providers::compat_schema::open_router_routing_schema(),
        ),
        (
            "VercelGatewayRoutingSchema",
            kiana_ai::providers::compat_schema::vercel_gateway_routing_schema(),
        ),
        (
            "AnthropicAllowedFallbackModelSchema",
            kiana_ai::providers::compat_schema::anthropic_allowed_fallback_model_schema(),
        ),
        (
            "OpenAICompletionsCompatSchema",
            open_ai_completions_compat_schema(),
        ),
        (
            "OpenAIResponsesCompatSchema",
            open_ai_responses_compat_schema(),
        ),
        (
            "AnthropicMessagesCompatSchema",
            anthropic_messages_compat_schema(),
        ),
        ("BedrockCompatSchema", bedrock_compat_schema()),
        (
            "MistralConversationsCompatSchema",
            kiana_ai::providers::compat_schema::mistral_conversations_compat_schema(),
        ),
        (
            "SystemOneCompatSchema",
            kiana_ai::providers::compat_schema::system_one_compat_schema(),
        ),
    ]
}

/// TS: `it("preserves property types in the provider superset")`（类型级断言在 Rust 侧转成
/// schema 形状断言）。
#[test]
fn preserves_property_types_in_the_provider_superset() {
    let schema = provider_compat_schema();
    let properties = schema["properties"].as_object().expect("properties");
    assert_eq!(properties["supportsStore"]["type"], json!("boolean"));
    let variants: Vec<&Value> = properties["sessionAffinityFormat"]["anyOf"]
        .as_array()
        .expect("sessionAffinityFormat anyOf")
        .iter()
        .map(|variant| &variant["const"])
        .collect();
    assert_eq!(
        variants,
        vec![
            &json!("openai"),
            &json!("openai-nosession"),
            &json!("openrouter")
        ]
    );
}

/// TS: `it("preserves API-specific defaults")`。
#[test]
fn preserves_api_specific_defaults() {
    let completions_defaults =
        apply_defaults(open_ai_completions_compat_schema(), Some(json!({}))).expect("defaults");
    assert!(completions_defaults.get("thinkingFormat").is_none());
    assert!(
        completions_defaults
            .get("supportsLongCacheRetention")
            .is_none()
    );

    let responses_defaults =
        apply_defaults(open_ai_responses_compat_schema(), Some(json!({}))).expect("defaults");
    assert_eq!(responses_defaults["supportsDeveloperRole"], json!(true));

    let anthropic_defaults =
        apply_defaults(anthropic_messages_compat_schema(), Some(json!({}))).expect("defaults");
    assert!(
        anthropic_defaults
            .get("sendSessionAffinityHeaders")
            .is_none()
    );
    assert_eq!(
        anthropic_defaults["supportsLongCacheRetention"],
        json!(true)
    );

    let bedrock_defaults =
        apply_defaults(bedrock_compat_schema(), Some(json!({}))).expect("defaults");
    assert_eq!(bedrock_defaults["supportsStrictMode"], json!(false));
}

/// TS: `it("does not assign API-specific defaults to the provider superset")`。
#[test]
fn does_not_assign_api_specific_defaults_to_the_provider_superset() {
    let defaults = apply_defaults(provider_compat_schema(), Some(json!({}))).expect("defaults");
    for name in [
        "supportsDeveloperRole",
        "sendSessionAffinityHeaders",
        "supportsLongCacheRetention",
        "supportsStrictMode",
    ] {
        assert!(defaults.get(name).is_none(), "{name}");
    }
}

#[test]
fn model_schemas_match_the_typescript_snapshot() {
    let fixture = fixture();
    for (name, schema) in model_schemas() {
        assert_eq!(
            (*schema).clone(),
            fixture["modelSchemas"][name],
            "model schema {name} diverges from TS"
        );
    }
}

#[test]
fn compat_schemas_match_the_typescript_snapshot() {
    let fixture = fixture();
    for (name, schema) in compat_schemas() {
        assert_eq!(
            (*schema).clone(),
            fixture["compatSchemas"][name],
            "compat schema {name} diverges from TS"
        );
    }
    assert_eq!(
        provider_compat_schema().clone(),
        fixture["providerCompatSchema"]
    );
}

#[test]
fn compat_schema_map_matches_the_typescript_order_and_values() {
    let fixture = fixture();
    let keys: Vec<&str> = compat_schemas_by_api().keys().copied().collect();
    let expected: Vec<&str> = fixture["compatByApiKeys"]
        .as_array()
        .expect("compatByApiKeys")
        .iter()
        .map(|key| key.as_str().expect("api key"))
        .collect();
    assert_eq!(keys, expected);
    for (api, schema) in compat_schemas_by_api() {
        assert_eq!((*schema).clone(), fixture["compatByApi"][*api], "api {api}");
    }
}

#[test]
fn provider_overrides_match_the_typescript_snapshot() {
    let fixture = fixture();
    let overrides = provider_compat_property_overrides();
    let names: Vec<&str> = overrides.keys().map(String::as_str).collect();
    let expected_names: Vec<&str> = fixture["overrideNames"]
        .as_array()
        .expect("overrideNames")
        .iter()
        .map(|name| name.as_str().expect("override name"))
        .collect();
    assert_eq!(names, expected_names);
    let actual = Value::Object(overrides.clone());
    assert_eq!(actual, fixture["overrides"]);
}

#[test]
fn typebox_defaults_match_the_typescript_snapshot() {
    let fixture = fixture();
    let defaults = fixture["defaults"].as_object().expect("defaults");
    let all_schemas: Vec<(&'static str, &'static Value)> = model_schemas()
        .into_iter()
        .chain(compat_schemas())
        .collect();
    for (name, expected) in defaults {
        let schema = all_schemas
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, schema)| *schema)
            .or_else(|| (name == "ProviderCompatSchema").then(provider_compat_schema))
            .unwrap_or_else(|| panic!("missing schema for default {name}"));
        let actual = apply_defaults(schema, Some(json!({}))).expect("defaults");
        assert_eq!(&actual, expected, "defaults for {name}");
    }
}

#[test]
fn merge_compat_properties_requires_an_override_for_duplicates() {
    let mut first = Map::new();
    first.insert("supportsStore".into(), json!({ "type": "boolean" }));
    let mut second = Map::new();
    second.insert(
        "supportsStore".into(),
        json!({ "type": "boolean", "description": "other" }),
    );
    let first_group = first.clone();
    let error = merge_compat_properties(&[&first_group, &second], &Map::new())
        .expect_err("duplicate without override");
    assert_eq!(
        error,
        "Duplicate compatibility schema property requires an override: supportsStore"
    );

    let mut unused_override = Map::new();
    unused_override.insert("supportsDeveloperRole".into(), json!({ "type": "boolean" }));
    let empty_group = Map::new();
    let error = merge_compat_properties(&[&first_group, &empty_group], &unused_override)
        .expect_err("override that resolves nothing");
    assert_eq!(
        error,
        "Compatibility schema override does not resolve a duplicate property: supportsDeveloperRole"
    );

    let mut override_value = Map::new();
    override_value.insert(
        "supportsStore".into(),
        json!({ "type": "boolean", "description": "resolved" }),
    );
    let merged = merge_compat_properties(&[&first_group, &second], &override_value)
        .expect("duplicate resolved by override");
    assert_eq!(merged["supportsStore"]["description"], json!("resolved"));
}

/// TypeBox `Value.Default(schema, undefined)` 在 schema 没有根 `default` 时返回 `undefined`，
/// Rust 用 `None` 表示（而不是 `null`）。
#[test]
fn typebox_default_preserves_undefined_for_missing_values() {
    assert_eq!(apply_defaults(thinking_level_map_schema(), None), None);
    assert_eq!(
        apply_defaults(&json!({ "type": "string", "default": "x" }), None),
        Some(json!("x"))
    );
    // 传入 `null` 不是缺失：null 原样保留，不套用 default。
    assert_eq!(
        apply_defaults(
            &json!({ "type": "string", "default": "x" }),
            Some(json!(null))
        ),
        Some(json!(null))
    );
}

/// TypeBox `FromRecord`：只有值 schema 带根 `default` 时才递归（`IsDefault(recordValue)`）。
#[test]
fn typebox_default_only_recurses_into_record_values_with_a_default() {
    let schema = json!({
        "type": "object",
        "patternProperties": {
            "^.*$": { "type": "object", "properties": { "a": { "type": "string", "default": "x" } } }
        }
    });
    let value = json!({ "k": {} });
    assert_eq!(apply_defaults(&schema, Some(value.clone())), Some(value));

    let with_default = json!({
        "type": "object",
        "patternProperties": {
            "^.*$": {
                "type": "object",
                "default": { "a": "d" },
                "properties": { "a": { "type": "string", "default": "x" } }
            }
        }
    });
    assert_eq!(
        apply_defaults(&with_default, Some(json!({ "k": {} }))),
        Some(json!({ "k": { "a": "x" } }))
    );
}

/// `Value.Check` 递归校验 object 属性 / array items / additionalProperties（union 分支选择用）。
#[test]
fn typebox_check_validates_nested_shapes() {
    let string_props = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
    assert!(check(&string_props, &json!({ "a": "x" })));
    assert!(!check(&string_props, &json!({ "a": 1 })));

    assert!(!check(
        &json!({ "type": "object", "required": ["a"] }),
        &json!({})
    ));
    assert!(!check(
        &json!({ "type": "object", "additionalProperties": false }),
        &json!({ "a": 1 })
    ));
    let number_extras = json!({ "type": "object", "additionalProperties": { "type": "number" } });
    assert!(check(&number_extras, &json!({ "a": 1 })));
    assert!(!check(&number_extras, &json!({ "a": "x" })));

    let string_items = json!({ "type": "array", "items": { "type": "string" } });
    assert!(check(&string_items, &json!(["a"])));
    assert!(!check(&string_items, &json!([1])));

    assert!(check(
        &json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
        &json!(null)
    ));
}
