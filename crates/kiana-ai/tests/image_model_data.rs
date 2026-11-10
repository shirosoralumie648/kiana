//! 移植 `packages/ai/test/image-model-data.test.ts`（`buildOpenRouterCatalog` 的图像/分类器分支）。

mod model_data_support;

use kiana_ai::model_catalog::openrouter_catalog::build_open_router_catalog;
use model_data_support::{assert_contains, assert_js_eq};
use serde_json::{Value, json};

/// TS 的 `imageOnly`。
fn image_only() -> Value {
    json!({
        "id": "example/image-model",
        "name": "Example Image Model",
        "architecture": {
            "input_modalities": ["text", "image"],
            "output_modalities": ["image"],
        },
        "pricing": {
            "prompt": "0.000001",
            "completion": "0.000002",
        },
    })
}

/// TS 的 `chatWithImages`。
fn chat_with_images() -> Value {
    json!({
        "id": "example/multimodal",
        "name": "Example Multimodal",
        "supported_parameters": ["tools"],
        "architecture": {
            "modality": "text+image->text+image",
            "input_modalities": ["text", "image"],
            "output_modalities": ["text", "image"],
        },
        "context_length": 32000,
    })
}

/// TS 的 `chatOnly`。
fn chat_only() -> Value {
    json!({
        "id": "example/chat",
        "name": "Example Chat",
        "supported_parameters": ["tools"],
        "architecture": { "modality": "text->text", "output_modalities": ["text"] },
    })
}

/// TS 的 `decisionModel`。
fn decision_model() -> Value {
    json!({
        "id": "typesafe/jev-1.13",
        "name": "TypeSafe: Jev 1.13",
        "supported_parameters": [],
        "architecture": { "modality": "text->decisions", "input_modalities": ["text"], "output_modalities": ["decisions"] },
        "pricing": { "prompt": "0.000000042", "completion": "0" },
        "context_length": 32000,
        "top_provider": { "context_length": 32000, "max_completion_tokens": 28800 },
    })
}

fn ids(models: &[Value]) -> Vec<String> {
    models
        .iter()
        .map(|model| {
            model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

fn types(models: &[Value]) -> Vec<String> {
    models
        .iter()
        .map(|model| {
            model
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn emits_image_only_models_from_the_image_listing_as_image_models() {
    let catalog = build_open_router_catalog(&[], &[image_only()], &[]);
    assert_eq!(catalog.chat, Vec::<Value>::new());
    assert_eq!(catalog.images.len(), 1);
    assert_contains(
        &catalog.images[0],
        &json!({
            "type": "image",
            "id": "example/image-model",
            "api": "openrouter-images",
            "input": ["text", "image"],
            "output": ["image"],
            "cost": { "input": 1, "output": 2 },
        }),
        "images[0]",
    );
}

#[test]
fn emits_separate_chat_and_image_entries_for_an_id_that_supports_both_operations() {
    let catalog = build_open_router_catalog(
        &[chat_with_images(), chat_only()],
        &[chat_with_images(), image_only()],
        &[],
    );
    assert_eq!(
        ids(&catalog.chat),
        vec!["example/multimodal", "example/chat"]
    );
    assert_eq!(types(&catalog.chat), vec!["chat", "chat"]);
    assert_eq!(
        ids(&catalog.images),
        vec!["example/multimodal", "example/image-model"]
    );
    assert!(types(&catalog.images).iter().all(|kind| kind == "image"));
    assert_eq!(
        catalog
            .images
            .iter()
            .map(|model| model.get("output").cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        vec![json!(["text", "image"]), json!(["image"])]
    );
}

#[test]
fn ignores_listed_models_that_neither_support_tools_nor_emit_images() {
    let mut no_tools = chat_only();
    no_tools["supported_parameters"] = json!([]);
    let mut text_only_image = image_only();
    text_only_image["architecture"] = json!({ "output_modalities": ["text"] });
    let mut text_only_decision = decision_model();
    text_only_decision["architecture"] = json!({ "output_modalities": ["text"] });

    let catalog = build_open_router_catalog(&[no_tools], &[text_only_image], &[text_only_decision]);
    assert_eq!(catalog.chat, Vec::<Value>::new());
    assert_eq!(catalog.images, Vec::<Value>::new());
    assert_eq!(catalog.classifiers, Vec::<Value>::new());
}

#[test]
fn emits_decision_models_as_system_one_classifier_models() {
    let catalog = build_open_router_catalog(&[], &[], &[decision_model(), decision_model()]);
    assert_eq!(catalog.chat, Vec::<Value>::new());
    assert_eq!(catalog.classifiers.len(), 1);
    assert_js_eq(
        &catalog.classifiers[0],
        &json!({
            "type": "classifier",
            "id": "typesafe/jev-1.13",
            "name": "TypeSafe: Jev 1.13",
            "api": "typesafe-system-one",
            "provider": "openrouter",
            "baseUrl": "https://openrouter.ai/api/v1",
            "input": ["text"],
            "cost": { "input": 0.042, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 32000,
            "compat": { "imageInput": "state-image-urls", "sendImageOptInHeader": false },
        }),
        "classifiers[0]",
    );
}
