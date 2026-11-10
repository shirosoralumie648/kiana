//! `sampling-options.test.ts` 的 Rust 移植（共享层部分）。
//!
//! TS 测试通过 `compat.stream` / `compat.streamSimple` 捕获 provider 请求体，验证
//! `temperature` / `top_p` / `top_k` / `min_p` 的合并顺序。请求体由各适配器构造；共享层负责的
//! 是 `resolveSamplingParams` + `buildBaseOptions`（见 `spec/ai-api-anthropic-openai.md`
//! §7 的映射表）。因此这里直接断言这两个函数，而不是起 HTTP 服务器。
//!
//! 两条适配器专属用例在此显式说明覆盖位置：
//! - `uses medium sampling params for summary-only %s requests`：`reasoningSummary` 决定
//!   effort = `medium` 的逻辑在 `openai-responses` / `azure-openai-responses` 适配器里
//!   （本单元的 `resolve_sampling_params` 只接收已决定好的 level）。
//! - `is ignored by non-OpenAI-compatible APIs`：Anthropic 适配器不把 sampling params 写进
//!   payload，同样属于适配器行为。

use kiana_ai::api::simple_options::{build_base_options, resolve_sampling_params};
use kiana_ai::types::{
    Context, Message, Model, ModelThinkingLevel, SamplingParams, SimpleStreamOptions,
    StreamOptions, ThinkingLevel, TranscriptContext, UserContent, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn make_context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".into()),
            timestamp: 1_700_000_000_000,
        })],
        ..Context::default()
    })
}

fn make_model(api: &str, sampling_params: Option<Value>, overrides: Value) -> Model {
    let mut value = json!({
        "id": "custom-model",
        "name": "Custom Model",
        "api": api,
        "provider": "custom-provider",
        "baseUrl": "http://127.0.0.1:9/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128000,
        "maxTokens": 16384,
    });
    if let Some(sampling_params) = sampling_params {
        value["samplingParams"] = sampling_params;
    }
    if let Some(overrides) = overrides.as_object() {
        for (key, item) in overrides {
            value[key.as_str()] = item.clone();
        }
    }
    serde_json::from_value(value).expect("model")
}

fn sampling(value: Value) -> SamplingParams {
    serde_json::from_value(value).expect("sampling params")
}

fn params_json(params: &Option<SamplingParams>) -> Value {
    match params {
        Some(params) => serde_json::to_value(params).expect("serializes"),
        None => Value::Null,
    }
}

fn options_with_sampling(sampling_params: Option<Value>) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            sampling_params: sampling_params.map(sampling),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

#[test]
fn merges_request_sampling_params_into_the_request_body() {
    let model = make_model("openai-completions", None, json!({}));
    let options = options_with_sampling(Some(json!({ "top_p": 0.95, "top_k": 0, "min_p": 0 })));
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    let params = base.sampling_params.expect("sampling params");
    assert_eq!(params["top_p"], json!(0.95));
    assert_eq!(params["top_k"], json!(0));
    assert_eq!(params["min_p"], json!(0));
}

#[test]
fn omits_sampling_params_when_neither_options_nor_model_set_them() {
    let model = make_model("openai-completions", None, json!({}));
    let base = build_base_options(&model, &make_context(), None, None);
    assert_eq!(base.sampling_params, None);
    assert_eq!(base.temperature, None);
}

#[test]
fn applies_model_level_sampling_params_with_request_keys_taking_precedence() {
    for api in [
        "openai-completions",
        "openai-responses",
        "azure-openai-responses",
    ] {
        let model = make_model(
            api,
            Some(json!({ "top_p": 0.95, "min_p": 0.05 })),
            json!({}),
        );
        let options = options_with_sampling(Some(json!({ "top_p": 0.5 })));
        let base = build_base_options(&model, &make_context(), Some(&options), None);
        let params = base.sampling_params.expect("sampling params");
        assert_eq!(params["top_p"], json!(0.5), "{api}");
        assert_eq!(params["min_p"], json!(0.05), "{api}");
    }
}

#[test]
fn passes_request_sampling_params_through_stream_simple() {
    let model = make_model("openai-completions", None, json!({}));
    let options = options_with_sampling(Some(json!({ "top_p": 0.5 })));
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    assert_eq!(params_json(&base.sampling_params), json!({ "top_p": 0.5 }));
}

#[test]
fn applies_sampling_params_for_the_effective_thinking_level_over_model_defaults() {
    let model = make_model(
        "openai-completions",
        Some(json!({ "temperature": 1, "top_p": 0.95 })),
        json!({
            "reasoning": true,
            "thinkingLevelMap": { "low": null, "medium": null },
            "samplingParamsByThinkingLevel": { "high": { "temperature": 0.8, "top_k": 64 } }
        }),
    );
    let mut options = options_with_sampling(None);
    options.reasoning = Some(ThinkingLevel::Low);
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    let params = base.sampling_params.expect("sampling params");
    assert_eq!(params["temperature"], json!(0.8));
    assert_eq!(params["top_p"], json!(0.95));
    assert_eq!(params["top_k"], json!(64));
}

#[test]
fn applies_off_sampling_params_when_reasoning_is_disabled() {
    let model = make_model(
        "openai-completions",
        None,
        json!({ "samplingParamsByThinkingLevel": { "off": { "temperature": 0.7 } } }),
    );
    let base = build_base_options(&model, &make_context(), None, None);
    assert_eq!(
        params_json(&base.sampling_params),
        json!({ "temperature": 0.7 })
    );
}

#[test]
fn merges_stream_option_keys_over_thinking_level_keys() {
    let model = make_model(
        "openai-completions",
        None,
        json!({
            "reasoning": true,
            "samplingParamsByThinkingLevel": { "low": { "temperature": 0.6, "top_p": 0.95 } }
        }),
    );
    let mut options = options_with_sampling(Some(json!({ "top_p": 0.5 })));
    options.reasoning = Some(ThinkingLevel::Low);
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    let params = base.sampling_params.expect("sampling params");
    assert_eq!(params["temperature"], json!(0.6));
    assert_eq!(params["top_p"], json!(0.5));
}

#[test]
fn applies_thinking_level_params_between_model_and_request_params() {
    for api in [
        "openai-completions",
        "openai-responses",
        "azure-openai-responses",
    ] {
        let model = make_model(
            api,
            Some(json!({ "temperature": 1, "top_p": 0.95 })),
            json!({
                "reasoning": true,
                "samplingParamsByThinkingLevel": { "low": { "temperature": 0.6, "top_k": 64 } }
            }),
        );
        let params = resolve_sampling_params(
            &model,
            ModelThinkingLevel::Low,
            Some(&sampling(json!({ "top_p": 0.5 }))),
        )
        .expect("sampling params");
        assert_eq!(params["temperature"], json!(0.6), "{api}");
        assert_eq!(params["top_p"], json!(0.5), "{api}");
        assert_eq!(params["top_k"], json!(64), "{api}");
    }
}

#[test]
fn uses_medium_sampling_params_for_summary_only_requests() {
    // TS 用例里 `reasoningSummary: "auto"` 让适配器选择 effort = medium；共享层收到的就是
    // `ModelThinkingLevel::Medium`，这里验证 medium 档参数被选中。
    let model = make_model(
        "openai-responses",
        None,
        json!({
            "reasoning": true,
            "samplingParamsByThinkingLevel": {
                "off": { "temperature": 0.7 },
                "medium": { "temperature": 0.8 }
            }
        }),
    );
    let params =
        resolve_sampling_params(&model, ModelThinkingLevel::Medium, None).expect("sampling params");
    assert_eq!(params["temperature"], json!(0.8));
}

#[test]
fn overrides_named_request_fields() {
    let model = make_model("openai-completions", None, json!({}));
    let options = SimpleStreamOptions {
        base: StreamOptions {
            temperature: Some(0.0),
            sampling_params: Some(sampling(json!({ "temperature": 1 }))),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    assert_eq!(base.temperature, Some(0.0));
    // 适配器在写完具名字段后 `Object.assign(params, samplingParams)`，因此最终是 1。
    assert_eq!(
        base.sampling_params.expect("params")["temperature"],
        json!(1)
    );
}

#[test]
fn is_ignored_by_non_openai_compatible_apis() {
    // Anthropic 适配器不消费 `samplingParams`；共享层仍然解析它（供适配器自行决定）。
    let model = make_model("anthropic-messages", None, json!({}));
    let options = options_with_sampling(Some(json!({ "top_p": 0.9, "top_k": 40 })));
    let base = build_base_options(&model, &make_context(), Some(&options), None);
    let params = base.sampling_params.expect("params");
    assert_eq!(params["top_p"], json!(0.9));
    assert_eq!(params["top_k"], json!(40));
}
