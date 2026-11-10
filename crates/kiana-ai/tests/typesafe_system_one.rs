//! `test/typesafe-system-one.test.ts` 的 Rust 移植。
//!
//! TS 的 `vi.fn(async (url, init) => Response.json(...))` → [`mock_http::MockHttpClient`]。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::api::typesafe_system_one::classify;
use kiana_ai::http::HttpError;
use kiana_ai::models::ClassifierOptions;
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierStopReason, Model, ModelCost, ModelInputModality, StreamOptions,
};
use serde_json::{Value, json};

use mock_http::{MockHttpClient, MockResponse, classifier_model, context, image, questions};

fn model() -> Model {
    Model {
        context_window: Some(64_000),
        ..classifier_model(
            "typesafe-system-one",
            "typesafe",
            "jev-latest",
            "https://api.typesafe.ai/v1/",
        )
    }
}

fn context_value() -> ClassifierContext {
    context(
        json!({"text": "The deployment succeeded, thank you."}),
        questions(vec![
            (
                "category",
                ClassifierQuestion::Choice {
                    instructions: "Classify the message".into(),
                    criteria: [
                        ("success".to_string(), "Successful".to_string()),
                        ("failure".to_string(), "Failed".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                },
            ),
            (
                "satisfaction",
                ClassifierQuestion::Score {
                    instructions: "Score satisfaction".into(),
                    criteria: vec!["low".into(), "neutral".into(), "high".into()],
                },
            ),
            (
                "approved",
                ClassifierQuestion::Bool {
                    instructions: "Does the user approve?".into(),
                    criteria: BoolQuestionCriteria {
                        r#true: "Approval".into(),
                        r#false: "No approval".into(),
                    },
                },
            ),
        ]),
    )
}

fn wire_answers() -> Value {
    json!({
        "category": {
            "type": "choice",
            "choice": "success",
            "probabilities": {"success": 0.9, "failure": 0.1},
            "confidence": 0.8
        },
        "satisfaction": {"type": "score", "score": 2, "confidence": 0.7},
        "approved": {"type": "noul", "noul": 0.95}
    })
}

fn options(client: &Arc<MockHttpClient>) -> ClassifierOptions {
    StreamOptions {
        api_key: Some("secret".into()),
        fetch: client.fetch(),
        ..StreamOptions::default()
    }
}

#[tokio::test]
async fn maps_public_bool_questions_and_answers_to_typesafe_noul_values() {
    let client = MockHttpClient::new(|request: &kiana_ai::http::HttpRequest| {
        let payload: Value =
            serde_json::from_slice(request.body.as_deref().unwrap_or_default()).expect("json");
        assert_eq!(payload["model"], json!("jev-latest"));
        assert_eq!(payload["questions"]["category"]["type"], json!("choice"));
        assert_eq!(payload["questions"]["satisfaction"]["type"], json!("score"));
        assert_eq!(payload["questions"]["approved"]["type"], json!("noul"));
        // System One has no temperature field; the option is ignored.
        assert!(payload.get("temperature").is_none());
        assert_eq!(
            request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.as_str()),
            Some("Bearer secret")
        );
        MockResponse::json(json!({"answers": wire_answers()}))
    });

    let mut temperature_options = options(&client);
    temperature_options.temperature = Some(1.5);
    let result = classify(&model(), &context_value(), Some(&temperature_options)).await;

    let usage_client = MockHttpClient::always(MockResponse::json(json!({
        "answers": wire_answers(),
        "usage": {"input_tokens": 308, "output_tokens": 23}
    })));
    let priced_model = Model {
        cost: ModelCost {
            input: 0.042,
            ..ModelCost::default()
        },
        ..model()
    };
    let priced_result = classify(
        &priced_model,
        &context_value(),
        Some(&options(&usage_client)),
    )
    .await;

    assert_eq!(client.request_count(), 1);
    assert_eq!(
        client.requests()[0].request.url,
        "https://api.typesafe.ai/v1/systemone"
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["approved"],
        ClassifierAnswer::Bool { probability: 0.95 }
    );
    assert!(matches!(
        &result.answers["category"],
        ClassifierAnswer::Choice { choice, .. } if choice == "success"
    ));
    assert_eq!(
        result.answers["satisfaction"],
        ClassifierAnswer::Score {
            score: 2.0,
            confidence: 0.7
        }
    );
    assert!(result.usage.is_none());
    let usage = priced_result.usage.expect("usage");
    assert_eq!(
        (usage.input, usage.output, usage.total_tokens),
        (308, 23, 331)
    );
    assert!((usage.cost.total - 0.000012936).abs() < 1e-15);
}

#[tokio::test]
async fn posts_openrouter_system_one_requests_to_its_typesafe_compatible_endpoint() {
    let client = MockHttpClient::new(|request: &kiana_ai::http::HttpRequest| {
        let payload: Value =
            serde_json::from_slice(request.body.as_deref().unwrap_or_default()).expect("json");
        assert_eq!(payload["model"], json!("typesafe/jev-1.13"));
        assert_eq!(
            payload["state"],
            json!({"text": "The deployment succeeded, thank you."})
        );
        // Response shape observed from the live OpenRouter endpoint.
        MockResponse::json(json!({
            "id": "gen-dec-1",
            "provider": "TypeSafe",
            "answers": wire_answers(),
            "usage": {"input_tokens": 308, "output_tokens": 23, "cost": 0.000012936}
        }))
    });
    let open_router_model = Model {
        id: "typesafe/jev-1.13".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        cost: ModelCost {
            input: 0.042,
            ..ModelCost::default()
        },
        ..model()
    };

    let result = classify(
        &open_router_model,
        &context_value(),
        Some(&options(&client)),
    )
    .await;

    assert_eq!(
        client.requests()[0].request.url,
        "https://openrouter.ai/api/v1/systemone"
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["approved"],
        ClassifierAnswer::Bool { probability: 0.95 }
    );
    // Priced from the catalog like chat usage; matches OpenRouter's reported cost.
    assert!((result.usage.expect("usage").cost.total - 0.000012936).abs() < 1e-15);
}

#[tokio::test]
async fn rejects_models_for_other_classifier_apis() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let other = Model {
        api: "cloudflare-workers-ai-system-one".into(),
        ..model()
    };
    let result = classify(&other, &context_value(), Some(&options(&client))).await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.error_message.as_deref().is_some_and(|message| {
        message.contains("Unsupported classifier API: cloudflare-workers-ai-system-one")
    }));
}

#[tokio::test]
async fn rejects_image_input_for_text_only_models_before_sending() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("aW1hZ2U=", "image/png")]);

    let result = classify(&model(), &classifier_context, Some(&options(&client))).await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("System One API does not support image input for jev-latest")
    );
}

#[tokio::test]
async fn sends_images_to_typesafe_as_assets_with_the_opt_in_header() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let image_model = Model {
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        ..model()
    };
    let mut with_images = context_value();
    with_images.images = Some(vec![
        image("cG5n", "image/png"),
        image("anBlZw==", "image/jpeg"),
    ]);

    let result = classify(&image_model, &with_images, Some(&options(&client))).await;
    let without_images = classify(&image_model, &context_value(), Some(&options(&client))).await;

    let requests = client.requests();
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let first = requests[0].json();
    assert_eq!(
        first["state"],
        json!(["<image-1>", "<image-2>", {"text": "The deployment succeeded, thank you."}])
    );
    assert_eq!(
        first["assets"],
        json!({
            "<image-1>": {"type": "image", "format": "png", "data_base64": "cG5n"},
            "<image-2>": {"type": "image", "format": "jpeg", "data_base64": "anBlZw=="}
        })
    );
    assert_eq!(
        requests[0].header("typesafe-danger-zone-images"),
        Some("true")
    );
    // Requests without images keep the plain state and send no opt-in header.
    assert_eq!(without_images.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        requests[1].json()["state"],
        json!({"text": "The deployment succeeded, thank you."})
    );
    assert!(requests[1].json().get("assets").is_none());
    assert_eq!(requests[1].header("typesafe-danger-zone-images"), None);
}

#[tokio::test]
async fn rejects_image_formats_typesafe_does_not_accept_before_sending() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let image_model = Model {
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        ..model()
    };
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("d2VicA==", "image/webp")]);

    let result = classify(&image_model, &classifier_context, Some(&options(&client))).await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("System One API accepts PNG and JPEG images for jev-latest, got image/webp")
    );
}

#[tokio::test]
async fn sends_images_to_openrouter_as_image_url_parts_in_the_state() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let open_router_model = Model {
        id: "typesafe/jev-1.14".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        ..model()
    };
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("cG5n", "image/png")]);

    let result = classify(
        &open_router_model,
        &classifier_context,
        Some(&options(&client)),
    )
    .await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    // OpenRouter answers 200 for `assets` but drops them, and rejects a top-level `images` list.
    let request = client.requests();
    let body = request[0].json();
    assert_eq!(body["model"], json!("typesafe/jev-1.14"));
    assert_eq!(
        body["state"],
        json!([
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,cG5n"}},
            {"text": "The deployment succeeded, thank you."}
        ])
    );
    assert!(body.get("assets").is_none());
    assert_eq!(request[0].header("typesafe-danger-zone-images"), None);
}

#[tokio::test]
async fn sends_openrouters_image_form_to_proxies_and_regional_endpoints() {
    let cases = [
        (
            "OpenRouter behind a proxy",
            "openrouter",
            "https://proxy.example.com/v1",
            None,
        ),
        (
            "a regional OpenRouter endpoint",
            "custom",
            "https://us.openrouter.ai/api/v1",
            None,
        ),
        (
            "a proxy declared through compat",
            "custom",
            "https://proxy.example.com/v1",
            Some(json!({"imageInput": "state-image-urls", "sendImageOptInHeader": false})),
        ),
    ];
    for (_name, provider, base_url, compat) in cases {
        let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
        let image_model = Model {
            id: "typesafe/jev-1.14".into(),
            provider: provider.into(),
            base_url: base_url.into(),
            input: vec![ModelInputModality::Text, ModelInputModality::Image],
            compat,
            ..model()
        };
        let mut classifier_context = context_value();
        classifier_context.images = Some(vec![image("cG5n", "image/png")]);

        let result = classify(&image_model, &classifier_context, Some(&options(&client))).await;

        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        let requests = client.requests();
        assert_eq!(
            requests[0].json()["state"],
            json!([
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,cG5n"}},
                {"text": "The deployment succeeded, thank you."}
            ])
        );
        assert!(requests[0].json().get("assets").is_none());
        assert_eq!(requests[0].header("typesafe-danger-zone-images"), None);
    }
}

#[tokio::test]
async fn lets_compat_override_detected_image_settings_per_field() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    // Only the header is overridden; the detected `assets` form stays.
    let image_model = Model {
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        compat: Some(json!({"sendImageOptInHeader": false})),
        ..model()
    };
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("cG5n", "image/png")]);

    classify(&image_model, &classifier_context, Some(&options(&client))).await;

    let requests = client.requests();
    assert!(requests[0].json().get("assets").is_some());
    assert_eq!(requests[0].header("typesafe-danger-zone-images"), None);
}

#[tokio::test]
async fn merges_headers_case_insensitively_and_supports_null_suppression() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let model_with_headers = Model {
        headers: Some(
            [
                ("authorization".to_string(), "Bearer model".to_string()),
                ("X-Source".to_string(), "model".to_string()),
            ]
            .into_iter()
            .collect(),
        ),
        ..model()
    };

    let mut request_options = options(&client);
    request_options.headers = Some(
        [
            (
                "Authorization".to_string(),
                Some("Bearer request".to_string()),
            ),
            ("x-source".to_string(), Some("request".to_string())),
        ]
        .into_iter()
        .collect(),
    );
    classify(
        &model_with_headers,
        &context_value(),
        Some(&request_options),
    )
    .await;

    let mut suppressed = options(&client);
    suppressed.headers = Some([("Authorization".to_string(), None)].into_iter().collect());
    classify(&model_with_headers, &context_value(), Some(&suppressed)).await;

    let requests = client.requests();
    assert_eq!(requests[0].header("authorization"), Some("Bearer request"));
    assert_eq!(requests[0].header("x-source"), Some("request"));
    assert_eq!(
        requests[0]
            .header_names()
            .iter()
            .filter(|name| name.eq_ignore_ascii_case("authorization"))
            .count(),
        1
    );
    assert_eq!(requests[1].header("authorization"), None);
}

#[tokio::test]
async fn preserves_prototype_sensitive_question_ids_in_answers() {
    let classifier_context: ClassifierContext = serde_json::from_value(json!({
        "state": {},
        "questions": {"__proto__": {"type": "bool", "instructions": "Is this true?", "criteria": {"true": "Yes", "false": "No"}}}
    }))
    .expect("context");
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": {"__proto__": {"type": "noul", "noul": 0.75}}
    })));

    let result = classify(&model(), &classifier_context, Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.keys().cloned().collect::<Vec<_>>(),
        vec!["__proto__"]
    );
    assert_eq!(
        result.answers.get("__proto__"),
        Some(&ClassifierAnswer::Bool { probability: 0.75 })
    );
}

#[tokio::test]
async fn reports_request_timeouts_separately_from_caller_cancellation() {
    let client = MockHttpClient::try_new(|request: &kiana_ai::http::HttpRequest| {
        assert_eq!(request.timeout_ms, Some(5));
        Err(HttpError::new(
            kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE,
        ))
    });

    let mut request_options = options(&client);
    request_options.timeout_ms = Some(5);
    request_options.max_retries = Some(0);
    let result = classify(&model(), &context_value(), Some(&request_options)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Request timed out after 5ms")
    );
}

#[tokio::test]
async fn creates_a_fresh_timeout_for_every_retry_attempt() {
    // TS 断言两个 `AbortSignal` 对象不同；Rust 的 deadline 由客户端按每次 `HttpRequest`
    // 新建，等价断言是两次尝试都带上了同一个 `timeoutMs`、且确实发生了重试。
    let attempts = Arc::new(AtomicUsize::new(0));
    let client = {
        let attempts = Arc::clone(&attempts);
        MockHttpClient::new(move |request: &kiana_ai::http::HttpRequest| {
            assert_eq!(request.timeout_ms, Some(1000));
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                MockResponse::with_headers(500, "retry", &[("retry-after-ms", "0")])
            } else {
                MockResponse::json(json!({"answers": wire_answers()}))
            }
        })
    };
    let mut request_options = options(&client);
    request_options.timeout_ms = Some(1000);
    request_options.max_retries = Some(1);

    let result = classify(&model(), &context_value(), Some(&request_options)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(client.request_count(), 2);
}

#[tokio::test]
async fn returns_malformed_responses_as_classifier_errors() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": {},
        "usage": {"input_tokens": 10, "output_tokens": 2}
    })));

    let result = classify(&model(), &context_value(), Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("did not return an answer for category"))
    );
    // The request was billed, so its usage is kept.
    let usage = result.usage.expect("usage");
    assert_eq!((usage.input, usage.output), (10, 2));
}

#[tokio::test]
async fn ignores_malformed_usage() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": wire_answers(),
        "usage": {"input_tokens": "many", "output_tokens": 3}
    })));
    let without_tokens = MockHttpClient::always(MockResponse::json(json!({
        "answers": wire_answers(),
        "usage": {"cost": 0.1}
    })));

    let result = classify(&model(), &context_value(), Some(&options(&client))).await;
    let without = classify(&model(), &context_value(), Some(&options(&without_tokens))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    let usage = result.usage.expect("usage");
    assert_eq!((usage.input, usage.output, usage.total_tokens), (0, 3, 3));
    assert_eq!(without.stop_reason, ClassifierStopReason::Stop);
    assert!(without.usage.is_none());
}

#[tokio::test]
async fn treats_unknown_compat_image_values_like_the_js_else_branch() {
    // TS 探针：`compat.imageInput` 的未知字符串/非字符串都落进 `assets` 分支；
    // `sendImageOptInHeader` 按 JS truthiness 解释（字符串 "false" 也是真值）。
    let cases: Vec<Option<Value>> = vec![
        Some(json!({"imageInput": "bogus"})),
        Some(json!({"imageInput": 5})),
        Some(json!({"sendImageOptInHeader": "false"})),
        None,
    ];
    for compat in cases {
        let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
        let image_model = Model {
            provider: "custom".into(),
            base_url: "https://proxy.example.com/v1".into(),
            input: vec![ModelInputModality::Text, ModelInputModality::Image],
            compat,
            ..model()
        };
        let mut classifier_context = context_value();
        classifier_context.images = Some(vec![image("cG5n", "image/png")]);

        classify(&image_model, &classifier_context, Some(&options(&client))).await;

        let requests = client.requests();
        assert_eq!(
            requests[0].json()["state"],
            json!(["<image-1>", {"text": "The deployment succeeded, thank you."}])
        );
        assert!(requests[0].json().get("assets").is_some());
        assert_eq!(
            requests[0].header("typesafe-danger-zone-images"),
            Some("true")
        );
    }
}

#[tokio::test]
async fn reads_missing_answers_through_the_js_prototype_chain() {
    // TS 的 `value[id]` 包含原型链：缺失的 "__proto__" 读到 Object.prototype（普通对象），
    // 于是走到类型检查并报 bool answer 错误，而不是缺少答案。
    let bool_question = || ClassifierQuestion::Bool {
        instructions: "OK?".into(),
        criteria: BoolQuestionCriteria {
            r#true: String::new(),
            r#false: String::new(),
        },
    };
    let proto_context = context(json!({}), questions(vec![("__proto__", bool_question())]));
    let missing_constructor = context(json!({}), questions(vec![("constructor", bool_question())]));

    let client = MockHttpClient::always(MockResponse::json(json!({"answers": {}})));
    let proto = classify(&model(), &proto_context, Some(&options(&client))).await;
    let constructor = classify(&model(), &missing_constructor, Some(&options(&client))).await;

    assert_eq!(proto.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        proto.error_message.as_deref(),
        Some("System One API did not return a bool answer for __proto__")
    );
    // 其余原型属性是函数：`isRecord` 为假，与真正的缺失答案一样。
    assert_eq!(constructor.stop_reason, ClassifierStopReason::Error);
    assert_eq!(
        constructor.error_message.as_deref(),
        Some("System One API did not return an answer for constructor")
    );
}
