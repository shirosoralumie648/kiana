//! `model-runtime-classifiers.test.ts` 的移植（3 个用例）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use indexmap::IndexMap;
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierStopReason, ImageContent, ImageKind, ModelType,
};
use kiana_config::models::runtime::ModelRuntime;
use serde_json::json;
use support::*;

fn test_context() -> ClassifierContext {
    let mut questions = IndexMap::new();
    questions.insert(
        "approved".to_string(),
        ClassifierQuestion::Bool {
            instructions: "Does this express approval?".to_string(),
            criteria: BoolQuestionCriteria {
                r#true: "Approval".to_string(),
                r#false: "No approval".to_string(),
            },
        },
    );
    ClassifierContext {
        state: json_object(json!({ "text": "Looks good" })),
        images: None,
        questions,
    }
}

fn test_image() -> ImageContent {
    ImageContent {
        kind_image: ImageKind::Image,
        data: "cG5n".to_string(),
        mime_type: "image/png".to_string(),
    }
}

fn answer_json() -> serde_json::Value {
    json!({ "answers": { "approved": { "type": "noul", "noul": 0.8 } } })
}

/// 分类一次并返回请求记录（TS 的 `captureImageRequest`）。
async fn capture_image_request(
    runtime: &ModelRuntime,
    provider: &str,
    id: &str,
) -> RecordedRequest {
    runtime
        .set_runtime_api_key(provider, "secret", Default::default())
        .await
        .expect("runtime key");
    let model = runtime
        .get_model_of_type(ModelType::Classifier, provider, id)
        .expect("classifier model");
    let client = MockHttpClient::new(|_request| Ok(MockResponse::json(answer_json())));
    let mut context = test_context();
    context.images = Some(vec![test_image()]);

    let result = runtime
        .classify(
            &model,
            &context,
            kiana_ai::models::ModelsClassifierOptions {
                base: kiana_ai::types::StreamOptions {
                    fetch: client.fetch(),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    requests.into_iter().next().expect("request")
}

#[tokio::test]
async fn lists_jev_separately_and_classifies_with_runtime_resolved_auth() {
    let harness = Harness::new();
    let runtime = harness.create().await;

    let jev = runtime
        .get_model_of_type(ModelType::Classifier, "typesafe", "jev-latest")
        .expect("jev");
    assert_eq!(jev.model_type, Some(ModelType::Classifier));
    assert!(runtime.get_model("typesafe", "jev-latest").is_none());

    let unconfigured = runtime
        .classify(&jev, &test_context(), Default::default())
        .await;
    assert_eq!(unconfigured.stop_reason, ClassifierStopReason::Error);
    assert!(
        unconfigured
            .error_message
            .as_deref()
            .expect("error message")
            .contains("not configured")
    );

    runtime
        .set_runtime_api_key("typesafe", "sk-typesafe", Default::default())
        .await
        .expect("runtime key");
    assert_eq!(
        runtime
            .get_available_of_type(ModelType::Classifier, Some("typesafe"), Default::default())
            .await
            .expect("available"),
        vec![jev.clone()]
    );

    let client = MockHttpClient::new(|_request| Ok(MockResponse::json(answer_json())));
    let result = runtime
        .classify(
            &jev,
            &test_context(),
            kiana_ai::models::ModelsClassifierOptions {
                base: kiana_ai::types::StreamOptions {
                    fetch: client.fetch(),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    let requests = client.requests();
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer sk-typesafe")
    );
    assert_eq!(
        result.answers.get("approved"),
        Some(&ClassifierAnswer::Bool { probability: 0.8 })
    );
}

#[tokio::test]
async fn keeps_open_routers_image_form_when_models_json_routes_open_router_through_a_proxy() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "openrouter": { "baseUrl": "https://proxy.example.com/v1" }
    }));
    let runtime = harness.create().await;

    let request = capture_image_request(&runtime, "openrouter", "typesafe/jev-1.14").await;

    // OpenRouter answers TypeSafe `assets` with 200 but drops the images.
    assert_eq!(request.url, "https://proxy.example.com/v1/systemone");
    let body = request.json();
    assert_eq!(
        body["state"],
        json!([
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,cG5n" } },
            { "text": "Looks good" }
        ])
    );
    assert!(body.get("assets").is_none());
    assert!(request.header("typesafe-danger-zone-images").is_none());
}

#[tokio::test]
async fn applies_provider_compat_from_models_json_to_classifier_models() {
    let harness = Harness::new();
    harness.write_providers(json!({
        "typesafe": {
            "baseUrl": "https://proxy.example.com/v1",
            "compat": { "imageInput": "state-image-urls", "sendImageOptInHeader": false }
        }
    }));
    let runtime = harness.create().await;

    let request = capture_image_request(&runtime, "typesafe", "jev-latest").await;

    assert_eq!(request.url, "https://proxy.example.com/v1/systemone");
    let body = request.json();
    assert_eq!(
        body["state"],
        json!([
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,cG5n" } },
            { "text": "Looks good" }
        ])
    );
    assert!(request.header("typesafe-danger-zone-images").is_none());
}
