//! `packages/ai/test/zai-coding-plan-models.test.ts` 的 Rust 移植。

use kiana_ai::providers::all::get_builtin_model;
use serde_json::json;

#[test]
fn exposes_glm_4_6v_on_the_china_coding_plan_catalog() {
    let model = get_builtin_model("zai-coding-cn", "glm-4.6v").expect("glm-4.6v");

    assert_eq!(model.id, "glm-4.6v");
    assert_eq!(model.provider, "zai-coding-cn");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(
        model.base_url,
        "https://open.bigmodel.cn/api/coding/paas/v4"
    );
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(
        model.input,
        vec![
            kiana_ai::types::ModelInputModality::Text,
            kiana_ai::types::ModelInputModality::Image
        ]
    );
    assert_eq!(model.cost.input, 0.3);
    assert_eq!(model.cost.output, 0.9);
    assert_eq!(model.cost.cache_read, 0.05);
    assert_eq!(model.cost.cache_write, 0.0);
    assert_eq!(model.context_window, Some(128_000));
    assert_eq!(model.max_tokens, Some(32_768));

    let compat = model.compat.expect("compat");
    assert_eq!(compat["maxTokensField"], json!("max_tokens"));
    assert_eq!(compat["thinkingFormat"], json!("zai"));
    assert_eq!(compat["zaiToolStream"], json!(true));
}

#[test]
fn uses_api_equivalent_reference_costs_for_coding_plan_models() {
    let expected = json!({
        "input": 1.4,
        "output": 4.4,
        "cacheRead": 0.26,
        "cacheWrite": 0.0,
    });
    let glm52 = get_builtin_model("zai", "glm-5.2").expect("zai glm-5.2");
    assert_eq!(serde_json::to_value(&glm52.cost).expect("cost"), expected);
    for provider in ["zai", "zai-coding-cn"] {
        let model = get_builtin_model(provider, "glm-5.3")
            .unwrap_or_else(|| panic!("missing {provider}/glm-5.3"));
        assert_eq!(
            serde_json::to_value(&model.cost).expect("cost"),
            expected,
            "{provider}"
        );
    }
}

#[test]
fn keeps_zero_costs_for_coding_plan_models_without_a_matching_api_price() {
    let zero_cost = json!({
        "input": 0.0,
        "output": 0.0,
        "cacheRead": 0.0,
        "cacheWrite": 0.0,
    });

    let model = get_builtin_model("zai", "glm-5.2-highspeed").expect("zai glm-5.2-highspeed");
    assert_eq!(serde_json::to_value(&model.cost).expect("cost"), zero_cost);

    for provider in ["zai", "zai-coding-cn"] {
        let model = get_builtin_model(provider, "glm-5.3-highspeed")
            .unwrap_or_else(|| panic!("missing {provider}/glm-5.3-highspeed"));
        assert_eq!(
            serde_json::to_value(&model.cost).expect("cost"),
            zero_cost,
            "{provider}"
        );
    }
}
