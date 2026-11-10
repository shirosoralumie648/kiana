//! `packages/ai/test/openrouter-cost.test.ts`。

use kiana_ai::types::{Usage, UsageCost};
use kiana_ai::utils::openrouter_cost::{
    OpenRouterCostDetails, OpenRouterUsageAccounting, apply_open_router_reported_cost,
    is_open_router_model,
};

fn usage(total: f64) -> Usage {
    Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 2,
        cost: UsageCost {
            input: 0.25,
            output: 0.75,
            cache_read: 0.0,
            cache_write: 0.0,
            total,
            total_from_provider: None,
        },
    }
}

fn default_usage() -> Usage {
    usage(1.0)
}

fn accounting(
    cost: Option<f64>,
    is_byok: bool,
    cost_details: Option<OpenRouterCostDetails>,
) -> OpenRouterUsageAccounting {
    OpenRouterUsageAccounting {
        cost,
        is_byok,
        cost_details,
    }
}

fn details(
    upstream_inference_cost: Option<f64>,
    prompt: f64,
    completions: f64,
) -> OpenRouterCostDetails {
    OpenRouterCostDetails {
        upstream_inference_cost,
        upstream_inference_prompt_cost: prompt,
        upstream_inference_completions_cost: completions,
        server_tool_cost: None,
    }
}

// describe("isOpenRouterModel")

#[test]
fn recognizes_both_the_built_in_provider_and_custom_providers_using_openrouters_endpoint() {
    assert!(is_open_router_model("openrouter", "https://example.com"));
    assert!(is_open_router_model(
        "openrouter-work",
        "https://openrouter.ai/api/v1"
    ));
    assert!(!is_open_router_model("custom", "https://example.com/v1"));
}

// describe("applyOpenRouterReportedCost")

#[test]
fn uses_openrouters_reported_cost_for_non_byok_requests() {
    let mut value = default_usage();
    apply_open_router_reported_cost(
        &mut value,
        &accounting(Some(0.0), false, Some(details(Some(0.0), 0.0, 0.0))),
    );
    assert_eq!(value.cost.total, 0.0);
    assert_eq!(value.cost.total_from_provider, Some(true));
}

#[test]
fn adds_the_upstream_inference_cost_for_byok_requests() {
    let mut value = default_usage();
    apply_open_router_reported_cost(
        &mut value,
        &accounting(Some(0.1), true, Some(details(Some(0.9), 0.6, 0.3))),
    );
    assert_eq!(value.cost.total, 1.0);
    assert_eq!(value.cost.total_from_provider, Some(true));
}

#[test]
fn uses_upstream_cost_components_when_openrouter_omits_their_total() {
    let mut value = default_usage();
    apply_open_router_reported_cost(
        &mut value,
        &accounting(Some(0.1), true, Some(details(None, 0.6, 0.3))),
    );
    assert!(
        (value.cost.total - 1.0).abs() < 1e-9,
        "total: {}",
        value.cost.total
    );
    assert_eq!(value.cost.total_from_provider, Some(true));
}

#[test]
fn keeps_the_catalog_total_when_openrouters_accounting_is_incomplete() {
    let mut value = default_usage();
    value.cost.total_from_provider = Some(false);
    apply_open_router_reported_cost(&mut value, &accounting(None, false, None));
    apply_open_router_reported_cost(&mut value, &accounting(Some(0.1), true, None));
    assert_eq!(value.cost.total, 1.0);
    assert_eq!(value.cost.total_from_provider, Some(false));
}
