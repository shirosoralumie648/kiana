//! 回归测试：TS `number` 的 JSON 文本（`ARCHITECTURE.md` §4.3）与 provider 模型判别式。
//!
//! 期望文本由 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）的 `JSON.stringify` 生成，
//! 见 `tests/fixtures/js_numbers.json`。覆盖 reviewer 报告的三类偏差：
//!
//! 1. `GrepToolDetails.matchLimitReached` / `FindToolDetails.resultLimitReached` /
//!    `LsToolDetails.entryLimitReached` 在 TS 里是未取整的 `number`（模型可给 `limit: 2.5`）；
//! 2. 所有 `f64` 字段必须按 ECMAScript `Number::toString` 写出（`1e-6` → `0.000001`、
//!    `9.3e18` → `9300000000000000000`），而不是 serde/ryu 的 `1e-6` / `9.3e18`；
//! 3. provider 模型配置的 `type` 是 TS 字面量类型（chat 只允许 `"chat"`，image/classifier
//!    的判别式不可写成别的值）。

use kiana_config::cache::warmer::{CacheWarmingAction, CacheWarmingDecisionEvent};
use kiana_ext_api::events::ExtensionEvent;
use kiana_ext_api::tool_types::{BashToolInput, FindToolDetails, GrepToolDetails, LsToolDetails};
use kiana_ext_api::types::{
    ContextUsage, ProviderChatModelConfig, ProviderChatModelType, ProviderClassifierModelConfig,
    ProviderClassifierModelType, ProviderImageModelConfig, ProviderImageModelType,
    ProviderModelConfig,
};
use serde::Deserialize;
use serde_json::json;

/// `tests/fixtures/js_numbers.json`（TS 参考实现生成）。
#[derive(Debug, Deserialize)]
struct Fixture {
    number_vectors: Vec<NumberVector>,
    grep_details_fractional_limit: String,
    find_details_fractional_limit: String,
    ls_details_fractional_limit: String,
    grep_notice_fractional_limit: String,
    find_notice_fractional_limit: String,
    ls_notice_fractional_limit: String,
    context_usage_null_json: String,
    cache_warming_decision_json: String,
}

#[derive(Debug, Deserialize)]
struct NumberVector {
    value: f64,
    context_usage_json: String,
    tool_input_json: String,
}

fn fixture() -> Fixture {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/js_numbers.json"
    );
    let text = std::fs::read_to_string(path).expect("读取 fixtures/js_numbers.json");
    serde_json::from_str(&text).expect("fixtures/js_numbers.json 必须是合法 JSON")
}

/// 每个 `f64` 都按 JS `JSON.stringify` 写出（整数不带 `.0`、小指数不用科学计数法）。
#[test]
fn js_number_vectors_match_ts_json_stringify() {
    let fixture = fixture();
    for vector in &fixture.number_vectors {
        let usage = ContextUsage {
            tokens: Some(1),
            context_window: 1,
            percent: Some(vector.value),
        };
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            vector.context_usage_json,
            "ContextUsage.percent 的 JS 数字文本不一致（value = {:?}）",
            vector.value
        );

        let input = BashToolInput {
            command: "true".to_string(),
            timeout: Some(vector.value),
        };
        assert_eq!(
            serde_json::to_string(&input).unwrap(),
            vector.tool_input_json,
            "BashToolInput.timeout 的 JS 数字文本不一致（value = {:?}）",
            vector.value
        );
    }
}

/// 特殊值：`-0` 写成 `0`，`NaN`/`±Infinity` 写成 `null`（与 `JSON.stringify` 一致；
/// 期望文本由 TS 参考实现生成）。
#[test]
fn js_number_specials_match_json_stringify() {
    let cases = [
        (-0.0_f64, r#"{"tokens":null,"contextWindow":0,"percent":0}"#),
        (
            f64::NAN,
            r#"{"tokens":null,"contextWindow":0,"percent":null}"#,
        ),
        (
            f64::INFINITY,
            r#"{"tokens":null,"contextWindow":0,"percent":null}"#,
        ),
        (
            f64::NEG_INFINITY,
            r#"{"tokens":null,"contextWindow":0,"percent":null}"#,
        ),
    ];
    for (value, expected) in cases {
        let usage = ContextUsage {
            tokens: None,
            context_window: 0,
            percent: Some(value),
        };
        assert_eq!(serde_json::to_string(&usage).unwrap(), expected);
    }
}

/// 未知用量写出必填键 `null`（不是省略键）。
#[test]
fn context_usage_writes_null_percent() {
    let fixture = fixture();
    let usage = ContextUsage {
        tokens: None,
        context_window: 0,
        percent: None,
    };
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        fixture.context_usage_null_json
    );
    let decoded: ContextUsage = serde_json::from_str(&fixture.context_usage_null_json).unwrap();
    assert_eq!(decoded, usage);
}

/// 三个工具详情保留未取整的 `effectiveLimit`（TS `limit: 2.5` → `details: 2.5`）。
#[test]
fn tool_details_preserve_fractional_limit_reached() {
    let fixture = fixture();

    let grep = GrepToolDetails {
        match_limit_reached: Some(2.5),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&grep).unwrap(),
        fixture.grep_details_fractional_limit
    );
    let decoded: GrepToolDetails =
        serde_json::from_str(&fixture.grep_details_fractional_limit).unwrap();
    assert_eq!(decoded.match_limit_reached, Some(2.5));

    let find = FindToolDetails {
        result_limit_reached: Some(2.5),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&find).unwrap(),
        fixture.find_details_fractional_limit
    );
    let decoded: FindToolDetails =
        serde_json::from_str(&fixture.find_details_fractional_limit).unwrap();
    assert_eq!(decoded.result_limit_reached, Some(2.5));

    let ls = LsToolDetails {
        entry_limit_reached: Some(2.5),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&ls).unwrap(),
        fixture.ls_details_fractional_limit
    );
    let decoded: LsToolDetails =
        serde_json::from_str(&fixture.ls_details_fractional_limit).unwrap();
    assert_eq!(decoded.entry_limit_reached, Some(2.5));
}

/// 详情里的 `2.5` 会进入通知文本（文本由 kiana-tools 生成，这里钉住数字格式化）。
///
/// TS：`${effectiveLimit} matches limit reached. Use limit=${effectiveLimit * 2} ...`。
#[test]
fn fractional_limit_notices_use_js_number_text() {
    let fixture = fixture();
    let limit = 2.5_f64;
    let limit_text = kiana_json::js_num::format_js_number(limit);
    let doubled_text = kiana_json::js_num::format_js_number(limit * 2.0);

    assert_eq!(
        format!(
            "{limit_text} matches limit reached. Use limit={doubled_text} for more, or refine pattern"
        ),
        fixture.grep_notice_fractional_limit
    );
    assert_eq!(
        format!(
            "{limit_text} results limit reached. Use limit={doubled_text} for more, or refine pattern"
        ),
        fixture.find_notice_fractional_limit
    );
    assert_eq!(
        format!("{limit_text} entries limit reached. Use limit={doubled_text} for more"),
        fixture.ls_notice_fractional_limit
    );
}

/// cache warming 决策事件的成本字段（常见 `3e-6` 量级）也按 JS 文本写出。
#[test]
fn cache_warming_costs_use_js_number_text() {
    let fixture = fixture();
    let event = ExtensionEvent::CacheWarmingDecision(CacheWarmingDecisionEvent {
        warm_cost: 3e-6,
        miss_cost: 1.5e-6,
        continuation_probability: 9.3e18,
        action: CacheWarmingAction::Warm,
    });
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        fixture.cache_warming_decision_json
    );
    let decoded: ExtensionEvent =
        serde_json::from_str(&fixture.cache_warming_decision_json).unwrap();
    match decoded {
        ExtensionEvent::CacheWarmingDecision(value) => {
            assert_eq!(value.warm_cost, 3e-6);
            assert_eq!(value.miss_cost, 1.5e-6);
            assert_eq!(value.continuation_probability, 9.3e18);
            assert_eq!(value.action, CacheWarmingAction::Warm);
        }
        _ => panic!("unexpected variant"),
    }
}

/// provider 模型配置的 `type` 是字面量：chat 只能携带 `"chat"`（或省略）。
#[test]
fn provider_chat_type_is_literal_chat() {
    let base = json!({
        "id": "m",
        "name": "M",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "reasoning": true,
        "contextWindow": 100,
        "maxTokens": 10
    });

    let mut with_type = base.clone();
    with_type["type"] = json!("chat");
    let chat: ProviderChatModelConfig = serde_json::from_value(with_type).unwrap();
    assert_eq!(chat.model_type, Some(ProviderChatModelType::Chat));
    assert!(
        serde_json::to_string(&chat)
            .unwrap()
            .contains(r#""type":"chat""#)
    );

    // 省略 `type` 是合法的（TS `type?: "chat"`）。
    let without_type: ProviderChatModelConfig = serde_json::from_value(base.clone()).unwrap();
    assert_eq!(without_type.model_type, None);

    // 其他判别式不能塞进 chat 配置（TS 里不可表示）。
    let mut wrong = base;
    wrong["type"] = json!("image");
    assert!(serde_json::from_value::<ProviderChatModelConfig>(wrong).is_err());
}

/// image / classifier 的判别式各自只有一个合法值。
#[test]
fn provider_image_and_classifier_types_are_literal() {
    let image = json!({
        "id": "i",
        "name": "I",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "type": "image",
        "output": ["image"]
    });
    let decoded: ProviderImageModelConfig = serde_json::from_value(image.clone()).unwrap();
    assert_eq!(decoded.model_type, ProviderImageModelType::Image);
    assert!(
        serde_json::to_string(&decoded)
            .unwrap()
            .contains(r#""type":"image""#)
    );
    let mut wrong = image;
    wrong["type"] = json!("classifier");
    assert!(serde_json::from_value::<ProviderImageModelConfig>(wrong).is_err());

    let classifier = json!({
        "id": "c",
        "name": "C",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "type": "classifier",
        "contextWindow": 128
    });
    let decoded: ProviderClassifierModelConfig =
        serde_json::from_value(classifier.clone()).unwrap();
    assert_eq!(decoded.model_type, ProviderClassifierModelType::Classifier);
    assert!(
        serde_json::to_string(&decoded)
            .unwrap()
            .contains(r#""type":"classifier""#)
    );
    let mut wrong = classifier;
    wrong["type"] = json!("image");
    assert!(serde_json::from_value::<ProviderClassifierModelConfig>(wrong).is_err());
}

/// 联合的判别式分派保持原样（缺省 `type` → chat）。
#[test]
fn provider_model_config_dispatch_by_type() {
    let base = json!({
        "id": "m",
        "name": "M",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "reasoning": true,
        "contextWindow": 100,
        "maxTokens": 10
    });
    let chat: ProviderModelConfig = serde_json::from_value(base).unwrap();
    assert!(matches!(chat, ProviderModelConfig::Chat(_)));

    let unknown = json!({
        "id": "x",
        "name": "X",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "type": "video"
    });
    let error = serde_json::from_value::<ProviderModelConfig>(unknown).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown provider model type: video")
    );
}
