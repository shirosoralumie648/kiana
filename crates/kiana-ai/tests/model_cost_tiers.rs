//! 移植 `packages/ai/test/model-cost-tiers.test.ts`：
//! OpenRouter 的 `overrides` 与 Vercel AI Gateway 的 bracket 定价如何变成 `cost.tiers`。

mod model_data_support;

use kiana_ai::model_catalog::gateway_pricing::get_ai_gateway_cost;
use kiana_ai::model_catalog::openrouter_catalog::build_open_router_catalog;
use model_data_support::assert_js_eq;
use serde_json::{Value, json};

/// `openRouterChatCost(pricing)`：只放一个 tool-capable 的 chat 模型，取它的 `cost`。
fn open_router_chat_cost(pricing: Value) -> Value {
    let listed = vec![json!({
        "id": "anthropic/claude-haiku-5.5",
        "name": "Claude Haiku 5.5",
        "supported_parameters": ["tools"],
        "pricing": pricing,
    })];
    let catalog = build_open_router_catalog(&listed, &[], &[]);
    catalog.chat[0]
        .get("cost")
        .cloned()
        .expect("chat model cost")
}

fn gateway_cost(pricing: Value) -> Value {
    serde_json::to_value(get_ai_gateway_cost(Some(&pricing))).expect("cost serializes")
}

#[test]
fn turns_prompt_length_overrides_into_tiers() {
    assert_js_eq(
        &open_router_chat_cost(json!({
            "prompt": "0.0000001",
            "completion": "0.0000005",
            "input_cache_read": "0.00000001",
            "input_cache_write": "0.000000125",
            "overrides": [{
                "min_prompt_tokens": 100000,
                "prompt": "0.0000005",
                "completion": "0.0000025",
                "input_cache_read": "0.00000005",
                "input_cache_write": "0.000000625",
            }],
        })),
        &json!({
            "input": 0.1,
            "output": 0.5,
            "cacheRead": 0.01,
            "cacheWrite": 0.125,
            "tiers": [{ "inputTokensAbove": 100000, "input": 0.5, "output": 2.5, "cacheRead": 0.05, "cacheWrite": 0.625 }],
        }),
        "prompt-length override",
    );
}

#[test]
fn keeps_base_rates_missing_from_an_override() {
    let cost = open_router_chat_cost(json!({
        "prompt": "0.000002",
        "completion": "0.000012",
        "input_cache_read": "0.0000002",
        "input_cache_write": "0.000000375",
        "overrides": [{ "min_prompt_tokens": 200000, "prompt": "0.000004", "completion": "0.000018" }],
    }));
    assert_js_eq(
        cost.get("tiers").expect("tiers"),
        &json!([{ "inputTokensAbove": 200000, "input": 4, "output": 18, "cacheRead": 0.2, "cacheWrite": 0.375 }]),
        "override keeps base rates",
    );
}

#[test]
fn skips_time_of_day_overrides() {
    assert_js_eq(
        &open_router_chat_cost(json!({
            "prompt": "0.000000132",
            "completion": "0.000000528",
            "overrides": [
                { "utc_start": 0, "utc_end": 1600, "prompt": "0.000000132", "completion": "0.000000528" },
                { "utc_days": ["saturday"], "min_prompt_tokens": 1000, "prompt": "0.0000001" },
            ],
        })),
        &json!({ "input": 0.132, "output": 0.528, "cacheRead": 0, "cacheWrite": 0 }),
        "time-of-day overrides are skipped",
    );
}

#[test]
fn turns_bracket_starts_into_tiers_with_the_rates_in_effect_there() {
    assert_js_eq(
        &gateway_cost(json!({
            "input": "0.000001",
            "output": "0.000005",
            "input_cache_read": "0.0000002",
            "input_tiers": [
                { "cost": "0.000001", "min": 0, "max": 32001 },
                { "cost": "0.0000018", "min": 32001, "max": 128001 },
                { "cost": "0.000003", "min": 128001 },
            ],
            "output_tiers": [
                { "cost": "0.000005", "min": 0, "max": 32001 },
                { "cost": "0.000009", "min": 32001, "max": 128001 },
                { "cost": "0.000015", "min": 128001 },
            ],
        })),
        &json!({
            "input": 1,
            "output": 5,
            "cacheRead": 0.2,
            "cacheWrite": 0,
            "tiers": [
                { "inputTokensAbove": 32000, "input": 1.8, "output": 9, "cacheRead": 0.2, "cacheWrite": 0 },
                { "inputTokensAbove": 128000, "input": 3, "output": 15, "cacheRead": 0.2, "cacheWrite": 0 },
            ],
        }),
        "gateway brackets",
    );
}

#[test]
fn returns_base_rates_without_tiers() {
    assert_js_eq(
        &gateway_cost(json!({ "input": "0.000003", "output": 0.000015 })),
        &json!({ "input": 3, "output": 15, "cacheRead": 0, "cacheWrite": 0 }),
        "base rates",
    );
}

// ---------------------------------------------------------------------------
// `perMillion` 的 `Number.parseFloat(value ?? "")` 语义
//
// TS 类型上 `pricing.prompt` 是 `string | undefined`，但运行时若上游给了数字，
// `Number.parseFloat` 会先做 `String(value)` 再解析。因此 `1e-7` 走的是 `"1e-7"`
// 而不是 `undefined`；`"Infinity"` 会变成 `Infinity`，序列化成 `null`。
// 期望值来自 Node 26 直接调用 `openRouterCost`。
// ---------------------------------------------------------------------------

mod numeric_pricing {
    use super::{assert_js_eq, json, open_router_chat_cost};

    #[test]
    fn coerces_numeric_prices_through_javascript_string_form() {
        // `String(1e-7)` === `"1e-7"` -> 0.1 $/1M。
        assert_js_eq(
            &open_router_chat_cost(json!({ "prompt": 1e-7, "completion": 0.000001 })),
            &json!({ "input": 0.1, "output": 1, "cacheRead": 0, "cacheWrite": 0 }),
            "numeric prompt/completion",
        );
        assert_js_eq(
            &open_router_chat_cost(json!({ "prompt": 0.000002, "input_cache_read": 1e-9 })),
            &json!({ "input": 2, "output": 0, "cacheRead": 0.001, "cacheWrite": 0 }),
            "numeric cache read",
        );
    }

    #[test]
    fn keeps_the_infinity_and_negative_fallbacks() {
        // `Number.parseFloat("Infinity")` === Infinity -> `roundCost(Infinity)` === Infinity
        // -> `JSON.stringify` 写成 `null`（`perMillion` 的 `>= 0` 判断通过）。
        assert_js_eq(
            &open_router_chat_cost(json!({ "prompt": "Infinity" })),
            &json!({ "input": null, "output": 0, "cacheRead": 0, "cacheWrite": 0 }),
            "Infinity price",
        );
        // 负价格用 fallback 0。
        assert_js_eq(
            &open_router_chat_cost(json!({ "prompt": -0.000001 })),
            &json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }),
            "negative price",
        );
    }

    #[test]
    fn applies_numeric_tier_prices() {
        assert_js_eq(
            &open_router_chat_cost(json!({
                "prompt": 1e-7,
                "overrides": [{ "min_prompt_tokens": 1000, "prompt": 2e-7 }],
            })),
            &json!({
                "input": 0.1,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "tiers": [{ "inputTokensAbove": 1000, "input": 0.2, "output": 0, "cacheRead": 0, "cacheWrite": 0 }],
            }),
            "numeric tier price",
        );
    }
}

// ---------------------------------------------------------------------------
// 审查修复：JS relational / nullish 语义
// ---------------------------------------------------------------------------

#[test]
fn bracket_fields_follow_javascript_relational_and_nullish_semantics() {
    // `min` 是字符串：`"1000" > 0` 为 true，Set 保留原值，`start - 1` 做 ToNumber。
    assert_js_eq(
        &gateway_cost(json!({ "input_tiers": [{ "min": "1000", "cost": 0.000003 }] })),
        &json!({
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "tiers": [{ "inputTokensAbove": 999, "input": 3, "output": 0, "cacheRead": 0, "cacheWrite": 0 }],
        }),
        "string min participates in > 0 and ToNumber",
    );
    // `min: true` -> 1 - 1 === 0。
    assert_js_eq(
        &gateway_cost(json!({ "input_tiers": [{ "min": true, "cost": 0.000005 }] })),
        &json!({
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "tiers": [{ "inputTokensAbove": 0, "input": 5, "output": 0, "cacheRead": 0, "cacheWrite": 0 }],
        }),
        "boolean min",
    );
    // `max: null`：`candidate.max === undefined` 为 false，`start < null` 为 false -> 无档位覆盖。
    assert_js_eq(
        &gateway_cost(json!({
            "input": 0.000001,
            "input_tiers": [{ "min": 1000, "max": null, "cost": 0.000004 }],
        })),
        &json!({
            "input": 1,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "tiers": [{ "inputTokensAbove": 999, "input": 1, "output": 0, "cacheRead": 0, "cacheWrite": 0 }],
        }),
        "max null rejects the bracket",
    );
    // `cost: null`：`bracket?.cost !== undefined` 为 true，perMillion(null) === 0。
    assert_js_eq(
        &gateway_cost(json!({
            "input": 0.000001,
            "input_tiers": [{ "min": 1000, "cost": null }],
        })),
        &json!({
            "input": 1,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "tiers": [{ "inputTokensAbove": 999, "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }],
        }),
        "cost null zeroes the tier rate",
    );
}

#[test]
fn openrouter_context_length_preserves_non_number_upstream_values() {
    let listed = vec![json!({
        "id": "openrouter/model",
        "name": "Model",
        "supported_parameters": ["tools"],
        "context_length": "64000",
        "pricing": { "prompt": "0" },
    })];
    let catalog = build_open_router_catalog(&listed, &[], &[]);
    assert_js_eq(
        &catalog.chat[0]["contextWindow"],
        &json!("64000"),
        "string context_length is preserved",
    );

    // 字符串 `supported_parameters`：JS 的 `String.prototype.includes("tools")` 为 true。
    let listed = vec![json!({
        "id": "openrouter/string-params",
        "name": "Model",
        "supported_parameters": "tools,reasoning",
    })];
    let catalog = build_open_router_catalog(&listed, &[], &[]);
    assert_eq!(catalog.chat.len(), 1);
    assert_eq!(catalog.chat[0]["reasoning"], json!(true));
}
