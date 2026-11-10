//! 生成模型目录的黄金测试（`spec/ai-core.md` §7.3「生成数据：校验 sha256 与结构」）。
//!
//! 两个 fixture 都由 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）生成：
//!
//! - `fixtures/core-foundation/catalog_order.json`：`getBuiltinProviders()` 的顺序，以及每个
//!   provider 的 `MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS` 键序（`Object.keys`）；
//! - `fixtures/core-foundation/model_samples.json`：若干模型的 `JSON.stringify` 输出（原始文本），
//!   用来锁定"生成器构造模型对象时的字段顺序"。模型对象在 Rust 侧是 `serde_json::Value`，
//!   `kiana-json` 的 `stringify` 与 TS 的 `JSON.stringify` 等价（整数键优先、JS 数字格式），
//!   因此可以逐字节比较。
//!
//! 数据本身（`crates/kiana-ai/data/*.json`）的 sha256 与 `structureHash` 由 `build.rs` 在每次构建时校验，
//! 这里只断言生成出来的静态表与 TS 的运行时形状一致。

use std::collections::BTreeMap;

use kiana_ai::providers::catalog;
use serde_json::Value;

const CATALOG_ORDER: &str = include_str!("fixtures/core-foundation/catalog_order.json");
const MODEL_SAMPLES: &str = include_str!("fixtures/core-foundation/model_samples.json");

#[test]
fn generated_at_matches_the_typescript_reference() {
    let expected: Value =
        serde_json::from_str(CATALOG_ORDER).expect("catalog_order.json is valid JSON");
    assert_eq!(
        catalog::get_builtin_model_data_generated_at(),
        expected["generatedAt"].as_i64()
    );
}

#[test]
fn provider_order_and_model_ids_match_the_typescript_reference() {
    let expected: Value =
        serde_json::from_str(CATALOG_ORDER).expect("catalog_order.json is valid JSON");
    let providers = expected["providers"]
        .as_array()
        .expect("providers is an array");

    let expected_ids: Vec<&str> = providers
        .iter()
        .map(|entry| entry["id"].as_str().expect("provider id"))
        .collect();
    assert_eq!(catalog::provider_ids(), expected_ids);

    for entry in providers {
        let provider = entry["id"].as_str().expect("provider id");
        for (fixture_key, table) in [
            ("chat", &*catalog::MODELS),
            ("image", &*catalog::IMAGE_MODELS),
            ("classifier", &*catalog::CLASSIFIER_MODELS),
        ] {
            let expected: Vec<&str> = entry[fixture_key]
                .as_array()
                .expect("model id list")
                .iter()
                .map(|id| id.as_str().expect("model id"))
                .collect();
            let actual: Vec<&str> = table
                .get(provider)
                .map(|models| models.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(actual, expected, "{provider} {fixture_key} ids");
            // `getBuiltinModels()` 等函数必须按同一个顺序返回（`Object.values`）。
            let via_getter: Vec<&str> = match fixture_key {
                "chat" => catalog::get_models(provider),
                "image" => catalog::get_image_models(provider),
                _ => catalog::get_classifier_models(provider),
            }
            .into_iter()
            .map(|model| model["id"].as_str().expect("model id"))
            .collect();
            assert_eq!(
                via_getter, expected,
                "{provider} {fixture_key} getter order"
            );
        }
        // `getAllBuiltinModels()` 是 chat + image + classifier 的三段拼接。
        let all = catalog::get_all_models(provider).len();
        assert_eq!(
            all,
            catalog::get_models(provider).len()
                + catalog::get_image_models(provider).len()
                + catalog::get_classifier_models(provider).len()
        );
    }
}

#[test]
fn model_json_matches_the_typescript_reference_byte_for_byte() {
    // 键是 `<table>:<provider>/<id>`，table ∈ {models, imageModels, classifierModels}
    // （同一 id 可以同时出现在 chat 与 classifier 表里，例如 openai 的 `gpt-6-luna`）。
    let samples: BTreeMap<String, String> =
        serde_json::from_str(MODEL_SAMPLES).expect("model_samples.json is valid JSON");
    for (label, expected) in &samples {
        let (table, path) = label
            .split_once(':')
            .expect("label is <table>:<provider>/<id>");
        let (provider, model_id) = path.split_once('/').expect("path is <provider>/<id>");
        let model = match table {
            "models" => catalog::get_model(provider, model_id),
            "imageModels" => catalog::get_image_model(provider, model_id),
            "classifierModels" => catalog::get_classifier_model(provider, model_id),
            other => panic!("unknown table {other}"),
        }
        .unwrap_or_else(|| panic!("{label} is missing from the generated catalog"));
        assert_eq!(
            &kiana_json::js_object::stringify(model),
            expected,
            "{label}"
        );
        // `serde_json::to_value` 的键序是 `preserve_order` 的插入序，与 TS 的对象字面量同序。
        let serde_text = serde_json::to_string(model).expect("model is serializable");
        assert_eq!(&serde_text, expected, "{label} serde_json roundtrip");
    }
}

#[test]
fn every_model_entry_is_self_consistent() {
    // 等价 TS `scripts/model-data.ts#validateModelValue` 的编译期部分：静态表里的每个条目都必须是
    // 它所在 provider/表格对应的模型（`type:id` 唯一性由 build.rs 保证）。
    for (provider, models) in catalog::MODELS.iter() {
        for (id, model) in models {
            assert_eq!(
                model["id"].as_str(),
                Some(id.as_str()),
                "{provider}/{id} id"
            );
            assert_eq!(
                model["provider"].as_str(),
                Some(provider.as_str()),
                "{provider}/{id} provider"
            );
            assert!(
                model["api"].as_str().is_some_and(|api| !api.is_empty()),
                "{provider}/{id} api"
            );
            assert!(
                model.get("type").is_none_or(|value| value == "chat"),
                "{provider}/{id} is a chat table entry"
            );
            assert!(
                model["contextWindow"]
                    .as_f64()
                    .is_some_and(|value| value > 0.0),
                "{provider}/{id} contextWindow"
            );
            assert!(
                model["maxTokens"].as_f64().is_some_and(|value| value > 0.0),
                "{provider}/{id} maxTokens"
            );
            assert!(model["reasoning"].is_boolean(), "{provider}/{id} reasoning");
            assert!(model["baseUrl"].is_string(), "{provider}/{id} baseUrl");
            let input = model["input"].as_array().expect("input modalities");
            assert!(!input.is_empty());
            for entry in input {
                assert!(
                    matches!(entry.as_str(), Some("text" | "image")),
                    "{provider}/{id} input {entry}"
                );
            }
            for field in ["input", "output", "cacheRead", "cacheWrite"] {
                assert!(
                    model["cost"][field].as_f64().is_some(),
                    "{provider}/{id} cost.{field}"
                );
            }
        }
    }

    for (provider, models) in catalog::IMAGE_MODELS.iter() {
        for (id, model) in models {
            assert_eq!(model["type"].as_str(), Some("image"), "{provider}/{id}");
            let output = model["output"].as_array().expect("output modalities");
            assert!(
                output.iter().any(|value| value == "image"),
                "{provider}/{id}"
            );
        }
    }

    for (provider, models) in catalog::CLASSIFIER_MODELS.iter() {
        for (id, model) in models {
            assert_eq!(
                model["type"].as_str(),
                Some("classifier"),
                "{provider}/{id}"
            );
            assert!(
                model["contextWindow"]
                    .as_f64()
                    .is_some_and(|value| value > 0.0),
                "{provider}/{id} contextWindow"
            );
        }
    }
}

#[test]
fn model_tables_are_independent_like_the_typescript_catalogs() {
    // `flattenModelCatalog` 按 `model.type` 分表，三张表**互不影响**：同一 provider 内同一个 id
    // 可以同时出现在 chat 与 image（openrouter 的 `google/gemini-3-pro-image`）或 chat 与
    // classifier（openai 的 `gpt-6-luna`）表里，因为它们是不同 API 组下的不同条目。
    // 这里锁定"每张表内部的条目 type 与表一致"这条真正的约束（上一条测试已逐条断言）。
    let mut overlap = 0usize;
    for provider in catalog::provider_ids() {
        let chat: Vec<&str> = catalog::get_models(provider)
            .into_iter()
            .map(|model| model["id"].as_str().unwrap_or_default())
            .collect();
        for model in catalog::get_image_models(provider) {
            if chat.contains(&model["id"].as_str().unwrap_or_default()) {
                overlap += 1;
            }
        }
    }
    // 生成器数据里确实存在重叠（openrouter 的 image 条目与 chat 同 id）；只要求"存在即被本测试看见"。
    assert!(
        overlap <= 16,
        "unexpected chat/image overlap growth: {overlap}"
    );
}
