//! `test/cloudflare-workers-ai-system-one.test.ts` 的 Rust 移植。
//!
//! TS 用 `createModels()` + `cloudflareWorkersAIProvider()` 走 `models.classify`；Rust 侧
//! provider 工厂由 providers 单元交付（当前仍是 stub），因此这里直接调用适配器，并用
//! `providers/catalog.rs` 的真实目录数据构造模型（价格与 `compat` 都是线上值），再按
//! `cloudflareClassifier` 的规则把 `{CLOUDFLARE_ACCOUNT_ID}` 替换成 `account-id`。
//!
//! 未移植：`exposes Jev only through classifier catalog accessors`（断言的是
//! `Models`/provider 的目录访问器，属于 providers 单元；`tests/catalog.rs` 已覆盖目录本身）。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use kiana_ai::api::cloudflare_workers_ai_system_one::classify;
use kiana_ai::http::HttpRequest;
use kiana_ai::models::ClassifierOptions;
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierStopReason, Model, ModelInputModality, StreamOptions,
};
use serde_json::{Value, json};

use mock_http::{MockHttpClient, MockResponse, context, image, questions};

/// `cloudflareClassifier` 的 `resolveCloudflareModel(model, env)`（env 只给 account id）。
fn catalog_model(id: &str) -> Model {
    let entry = kiana_ai::providers::catalog::get_classifier_model("cloudflare-workers-ai", id)
        .unwrap_or_else(|| panic!("missing Cloudflare model {id}"));
    let mut model: Model = serde_json::from_value(entry.clone()).expect("catalog model");
    model.base_url = model
        .base_url
        .replace("{CLOUDFLARE_ACCOUNT_ID}", "account-id");
    model
}

fn context_value() -> ClassifierContext {
    context(
        json!({"message": "Help! My payouts have been failing for 3 days."}),
        questions(vec![
            (
                "is_urgent",
                ClassifierQuestion::Bool {
                    instructions: "Does this convey urgency?".into(),
                    criteria: BoolQuestionCriteria {
                        r#true: "Explicitly time-sensitive".into(),
                        r#false: "No urgency expressed".into(),
                    },
                },
            ),
            (
                "department",
                ClassifierQuestion::Choice {
                    instructions: "Which team should handle this?".into(),
                    criteria: [
                        ("billing".to_string(), "Payments".to_string()),
                        ("technical".to_string(), "Bugs".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                },
            ),
        ]),
    )
}

/// Model output from <https://developers.cloudflare.com/ai/models/typesafe/jev/>.
fn jev_output() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "is_urgent": {"type": "noul", "noul": 0.95},
            "department": {
                "type": "choice",
                "choice": "billing",
                "confidence": 0.8,
                "probabilities": {"billing": 0.87, "technical": 0.13}
            }
        },
        "usage": {"input_tokens": 426, "output_tokens": 73}
    })
}

/// REST envelope observed from the live `/ai/run` endpoint.
fn rest_response(state: &str, result: Value) -> Value {
    json!({
        "result": {"state": state, "result": result, "gatewayMetadata": {"keySource": "Unified"}},
        "success": true,
        "errors": [],
        "messages": []
    })
}

/// Cloudflare-hosted output observed from a live `/ai/run` call.
fn clef_output() -> Value {
    json!({
        "model": "clef",
        "answers": {
            "is_urgent": {"type": "noul", "noul": 0.9912},
            "department": {
                "type": "choice",
                "choice": "technical",
                "probabilities": {"billing": 0.1632, "technical": 0.8368},
                "confidence": 0.4538
            }
        },
        "usage": {"input_tokens": 222, "output_tokens": 0}
    })
}

fn options(client: &std::sync::Arc<MockHttpClient>) -> ClassifierOptions {
    StreamOptions {
        api_key: Some("cf-key".into()),
        fetch: client.fetch(),
        ..StreamOptions::default()
    }
}

#[tokio::test]
async fn runs_jev_through_the_account_scoped_ai_run_endpoint() {
    let client = MockHttpClient::new(|request: &HttpRequest| {
        let payload: Value =
            serde_json::from_slice(request.body.as_deref().unwrap_or_default()).expect("json");
        assert_eq!(payload["model"], json!("typesafe/jev"));
        assert_eq!(
            payload["input"]["state"],
            json!({"message": "Help! My payouts have been failing for 3 days."})
        );
        assert_eq!(
            payload["input"]["questions"]["is_urgent"]["type"],
            json!("noul")
        );
        assert_eq!(
            payload["input"]["questions"]["department"]["type"],
            json!("choice")
        );
        assert_eq!(
            request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.as_str()),
            Some("Bearer cf-key")
        );
        MockResponse::json(rest_response("Completed", jev_output()))
    });

    let result = classify(
        &catalog_model("typesafe/jev"),
        &context_value(),
        Some(&options(&client)),
    )
    .await;

    assert_eq!(
        client.requests()[0].request.url,
        "https://api.cloudflare.com/client/v4/accounts/account-id/ai/run"
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["is_urgent"],
        ClassifierAnswer::Bool { probability: 0.95 }
    );
    assert!(matches!(
        &result.answers["department"],
        ClassifierAnswer::Choice { choice, confidence, .. } if choice == "billing" && *confidence == 0.8
    ));
    let usage = result.usage.expect("usage");
    assert_eq!(
        (usage.input, usage.output, usage.total_tokens),
        (426, 73, 499)
    );
}

#[tokio::test]
async fn runs_clef_models_through_ai_run_and_parses_their_direct_output() {
    for (id, input_price) in [
        ("@cf/cloudflare/clef", 0.24),
        ("@cf/cloudflare/clef-flash", 0.038),
        ("@cf/cloudflare/clef-omni", 0.15),
    ] {
        let model = catalog_model(id);
        let expected_id = id.to_string();
        let client = MockHttpClient::new(move |request: &HttpRequest| {
            let payload: Value =
                serde_json::from_slice(request.body.as_deref().unwrap_or_default()).expect("json");
            assert_eq!(payload["model"], json!(expected_id));
            assert_eq!(
                payload["input"]["state"],
                json!({"message": "Help! My payouts have been failing for 3 days."})
            );
            assert_eq!(
                payload["input"]["questions"]["is_urgent"]["type"],
                json!("noul")
            );
            MockResponse::json(json!({
                "result": clef_output(),
                "success": true,
                "errors": [],
                "messages": []
            }))
        });

        let result = classify(&model, &context_value(), Some(&options(&client))).await;

        assert_eq!(
            client.requests()[0].request.url,
            "https://api.cloudflare.com/client/v4/accounts/account-id/ai/run"
        );
        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        assert_eq!(
            result.answers["is_urgent"],
            ClassifierAnswer::Bool {
                probability: 0.9912
            }
        );
        assert!(matches!(
            &result.answers["department"],
            ClassifierAnswer::Choice { choice, confidence, .. }
                if choice == "technical" && (*confidence - 0.4538).abs() < 1e-12
        ));
        let usage = result.usage.expect("usage");
        assert_eq!(
            (usage.input, usage.output, usage.total_tokens),
            (222, 0, 222)
        );
        assert!((usage.cost.input - (222.0 * input_price) / 1_000_000.0).abs() < 1e-15);
    }
}

#[tokio::test]
async fn sends_images_to_clef_as_data_urls() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "result": clef_output(),
        "success": true,
        "errors": [],
        "messages": []
    })));
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![
        image("cG5n", "image/png"),
        image("d2VicA==", "image/webp"),
    ]);

    let result = classify(
        &catalog_model("@cf/cloudflare/clef"),
        &classifier_context,
        Some(&options(&client)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let body = client.requests();
    let input = &body[0].json()["input"];
    // Clef rejects `assets` with a 422, so the state stays as is.
    assert_eq!(
        input["state"],
        json!({"message": "Help! My payouts have been failing for 3 days."})
    );
    assert_eq!(
        input["images"],
        json!([
            "data:image/png;base64,cG5n",
            "data:image/webp;base64,d2VicA=="
        ])
    );
    assert!(input.get("assets").is_none());
    assert_eq!(body[0].header("typesafe-danger-zone-images"), None);
}

#[tokio::test]
async fn reads_the_image_form_from_compat_before_the_model_id() {
    let clef = catalog_model("@cf/cloudflare/clef");
    assert_eq!(clef.compat, Some(json!({"imageInput": "images"})));
    let jev = catalog_model("typesafe/jev");
    assert_eq!(jev.compat, Some(json!({"imageInput": "assets"})));

    let client = MockHttpClient::always(MockResponse::json(json!({
        "result": clef_output(),
        "success": true,
        "errors": [],
        "messages": []
    })));
    // A Clef deployment under an ID the prefix check does not recognize.
    let custom = Model {
        id: "@cf/acme/clef-finetune".into(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        compat: Some(json!({"imageInput": "images"})),
        ..jev
    };
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("cG5n", "image/png")]);

    classify(&custom, &classifier_context, Some(&options(&client))).await;

    let body = client.requests();
    assert_eq!(
        body[0].json()["input"]["images"],
        json!(["data:image/png;base64,cG5n"])
    );
    assert!(body[0].json()["input"].get("assets").is_none());
}

#[tokio::test]
async fn rejects_more_than_four_images_for_clef_before_sending() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "result": clef_output(),
        "success": true
    })));
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("cG5n", "image/png"); 5]);

    let result = classify(
        &catalog_model("@cf/cloudflare/clef-flash"),
        &classifier_context,
        Some(&options(&client)),
    )
    .await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Cloudflare Workers AI accepts at most 4 images for @cf/cloudflare/clef-flash, got 5")
    );
}

#[tokio::test]
async fn sends_images_to_typesafe_jev_as_system_one_assets() {
    for id in ["typesafe/jev", "typesafe/jev-1.14.0"] {
        let client =
            MockHttpClient::always(MockResponse::json(rest_response("Completed", jev_output())));
        let mut classifier_context = context_value();
        classifier_context.images = Some(vec![image("cG5n", "image/png")]);

        let result = classify(
            &catalog_model(id),
            &classifier_context,
            Some(&options(&client)),
        )
        .await;

        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        let requests = client.requests();
        let body = requests[0].json();
        assert_eq!(body["model"], json!(id));
        assert_eq!(
            body["input"]["state"],
            json!([
                "<image-1>",
                {"message": "Help! My payouts have been failing for 3 days."}
            ])
        );
        assert_eq!(
            body["input"]["assets"],
            json!({"<image-1>": {"type": "image", "format": "png", "data_base64": "cG5n"}})
        );
        // Workers AI needs no TypeSafe opt-in header.
        assert_eq!(requests[0].header("typesafe-danger-zone-images"), None);
    }
}

#[tokio::test]
async fn reports_runs_that_did_not_complete() {
    let client = MockHttpClient::always(MockResponse::json(rest_response("Queued", Value::Null)));

    let result = classify(
        &catalog_model("typesafe/jev"),
        &context_value(),
        Some(&options(&client)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("run did not complete (state: Queued)"))
    );
}

#[tokio::test]
async fn reports_cloudflare_envelope_errors() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "success": false,
        "errors": [{"code": 5007, "message": "No such model"}],
        "result": null
    })));

    let result = classify(
        &catalog_model("typesafe/jev"),
        &context_value(),
        Some(&options(&client)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Cloudflare Workers AI error: No such model"))
    );
}
