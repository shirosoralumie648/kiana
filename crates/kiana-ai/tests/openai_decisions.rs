//! `test/openai-decisions.test.ts` 的 Rust 移植。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::api::openai_decisions::classify;
use kiana_ai::http::HttpRequest;
use kiana_ai::models::ClassifierOptions;
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion,
    ClassifierStopReason, Model, ModelCost, ModelCostTier, ModelInputModality, ModelType,
    StreamOptions,
};
use serde_json::{Value, json};

use mock_http::{MockHttpClient, MockResponse, context, image, questions};

fn model() -> Model {
    Model {
        id: "gpt-6-luna".into(),
        name: "GPT-6 Luna".into(),
        api: "openai-decisions".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 0.1,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: Some(vec![ModelCostTier {
                input_tokens_above: 272_000.0,
                input: 0.2,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }]),
        },
        model_type: Some(ModelType::Classifier),
        context_window: Some(922_000),
        ..Model::default()
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
                        ("failure".to_string(), String::new()),
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

/// Response shape from the API reference and live `gpt-6-luna` requests.
fn wire_answers() -> Value {
    json!([
        {
            "type": "choice",
            "name": "category",
            "choice": "success",
            "probabilities": [
                {"value": "success", "probability": 0.9},
                {"value": "failure", "probability": 0.1}
            ],
            "confidence": 0.8
        },
        {
            "type": "score",
            "name": "satisfaction",
            "score": 1.8,
            "probabilities": [
                {"value": 0, "label": "low", "probability": 0.05},
                {"value": 1, "label": "neutral", "probability": 0.1},
                {"value": 2, "label": "high", "probability": 0.85}
            ],
            "confidence": 0.7
        },
        {"type": "predicate", "name": "approved", "probability": 0.95}
    ])
}

fn wire_usage() -> Value {
    json!({
        "input_tokens": 164,
        "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
        "output_tokens": 0,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 164
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
async fn maps_questions_to_decisions_types_and_answers_back_by_name() {
    let mut answers = wire_answers();
    answers.as_array_mut().expect("array").reverse();
    let response = json!({"model": "gpt-6-luna", "answers": answers, "usage": wire_usage()});
    let client = MockHttpClient::always(MockResponse::json(response));

    let mut request_options = options(&client);
    request_options.temperature = Some(1.5);
    let result = classify(&model(), &context_value(), Some(&request_options)).await;

    assert_eq!(client.request_count(), 1);
    let request = &client.requests()[0];
    assert_eq!(request.request.url, "https://api.openai.com/v1/decisions");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(
        request.json(),
        json!({
            "model": "gpt-6-luna",
            "input": "{\"text\":\"The deployment succeeded, thank you.\"}",
            "questions": [
                {
                    "type": "choice",
                    "name": "category",
                    "instructions": "Classify the message",
                    // Empty descriptions are omitted.
                    "choices": [{"value": "success", "description": "Successful"}, {"value": "failure"}]
                },
                {
                    "type": "score",
                    "name": "satisfaction",
                    "instructions": "Score satisfaction",
                    "levels": [{"label": "low"}, {"label": "neutral"}, {"label": "high"}]
                },
                {
                    "type": "predicate",
                    "name": "approved",
                    "instructions": "Does the user approve?\n\nTrue means: Approval\nFalse means: No approval"
                }
            ]
        })
    );
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers["category"],
        ClassifierAnswer::Choice {
            choice: "success".into(),
            probabilities: [("success".to_string(), 0.9), ("failure".to_string(), 0.1)]
                .into_iter()
                .collect(),
            confidence: 0.8,
        }
    );
    assert_eq!(
        result.answers["satisfaction"],
        ClassifierAnswer::Score {
            score: 1.8,
            confidence: 0.7
        }
    );
    assert_eq!(
        result.answers["approved"],
        ClassifierAnswer::Bool { probability: 0.95 }
    );
    let usage = result.usage.expect("usage");
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.total_tokens
        ),
        (164, 0, 0, 164)
    );
    assert!((usage.cost.total - 0.0000164).abs() < 1e-15);
}

#[tokio::test]
async fn prices_long_context_requests_at_the_long_context_input_rate() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": wire_answers(),
        "usage": {"input_tokens": 300000, "output_tokens": 0}
    })));

    let result = classify(&model(), &context_value(), Some(&options(&client))).await;

    assert!((result.usage.expect("usage").cost.total - 0.06).abs() < 1e-12);
}

#[tokio::test]
async fn sends_images_after_the_state_in_one_user_message() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![
        image("aW1hZ2U=", "image/png"),
        image("aW1hZ2U=", "image/jpeg"),
    ]);

    let result = classify(&model(), &classifier_context, Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        client.requests()[0].json()["input"],
        json!([{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "{\"text\":\"The deployment succeeded, thank you.\"}"},
                {"type": "input_image", "image_url": "data:image/png;base64,aW1hZ2U="},
                {"type": "input_image", "image_url": "data:image/jpeg;base64,aW1hZ2U="}
            ]
        }])
    );
}

#[tokio::test]
async fn rejects_more_than_128_images_before_sending() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let mut classifier_context = context_value();
    classifier_context.images = Some(vec![image("aW1hZ2U=", "image/png"); 129]);

    let result = classify(&model(), &classifier_context, Some(&options(&client))).await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("at most 128 images, got 129"))
    );
}

#[tokio::test]
async fn fails_the_result_when_a_question_is_refused_and_keeps_the_billed_usage() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": [wire_answers()[0], wire_answers()[1], {"type": "refusal", "name": "approved"}],
        "usage": wire_usage()
    })));

    let result = classify(&model(), &context_value(), Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    assert!(result.answers.is_empty());
    assert_eq!(
        result.error_message.as_deref(),
        Some("OpenAI Decisions refused to answer approved")
    );
    assert_eq!(result.usage.expect("usage").input, 164);
}

#[tokio::test]
async fn returns_missing_and_mistyped_answers_as_classifier_errors() {
    let missing_client = MockHttpClient::always(MockResponse::json(json!({
        "answers": [wire_answers()[0], wire_answers()[1]]
    })));
    let mistyped_client = MockHttpClient::always(MockResponse::json(json!({
        "answers": [
            wire_answers()[0],
            wire_answers()[1],
            {"type": "score", "name": "approved"}
        ]
    })));

    let missing = classify(&model(), &context_value(), Some(&options(&missing_client))).await;
    let mistyped = classify(&model(), &context_value(), Some(&options(&mistyped_client))).await;

    assert_eq!(missing.stop_reason, ClassifierStopReason::Error);
    assert!(
        missing
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("did not return an answer for approved"))
    );
    assert_eq!(mistyped.stop_reason, ClassifierStopReason::Error);
    assert!(
        mistyped.error_message.as_deref().is_some_and(
            |message| message.contains("did not return a predicate answer for approved")
        )
    );
}

#[tokio::test]
async fn preserves_prototype_sensitive_question_ids_in_answers() {
    let classifier_context: ClassifierContext = serde_json::from_value(json!({
        "state": {},
        "questions": {"__proto__": {"type": "bool", "instructions": "Is this true?", "criteria": {"true": "Yes", "false": "No"}}}
    }))
    .expect("context");
    let client = MockHttpClient::always(MockResponse::json(json!({
        "answers": [{"type": "predicate", "name": "__proto__", "probability": 0.75}]
    })));

    let result = classify(&model(), &classifier_context, Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    assert_eq!(
        result.answers.get("__proto__"),
        Some(&ClassifierAnswer::Bool { probability: 0.75 })
    );
}

#[tokio::test]
async fn does_not_retry_gateway_timeouts_and_explains_them_instead_of_returning_the_html_page() {
    let client = MockHttpClient::always(MockResponse::with_headers(
        504,
        "<!DOCTYPE html><html>Gateway time-out</html>",
        &[("retry-after-ms", "0")],
    ));
    // Default retries: the same input would time out again.
    let result = classify(&model(), &context_value(), Some(&options(&client))).await;

    assert_eq!(client.request_count(), 1);
    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    let message = result.error_message.expect("message");
    assert!(message.contains("OpenAI Decisions error (504): the request timed out at the gateway"));
    assert!(!message.contains("<html>"));
}

#[tokio::test]
async fn still_retries_other_server_errors() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let client = {
        let attempts = Arc::clone(&attempts);
        MockHttpClient::new(move |_: &HttpRequest| {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                MockResponse::with_headers(503, "busy", &[("retry-after-ms", "0")])
            } else {
                MockResponse::json(json!({"answers": wire_answers()}))
            }
        })
    };

    let result = classify(&model(), &context_value(), Some(&options(&client))).await;

    assert_eq!(client.request_count(), 2);
    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
}

#[tokio::test]
async fn includes_the_api_error_body_for_other_http_failures() {
    let client = MockHttpClient::always(MockResponse::json_status(
        400,
        json!({"error": {"message": "Decision input exceeds the token limit.", "type": "invalid_request_error"}}),
    ));
    let mut request_options = options(&client);
    request_options.max_retries = Some(0);

    let result = classify(&model(), &context_value(), Some(&request_options)).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Error);
    let message = result.error_message.expect("message");
    assert!(
        message.contains("OpenAI Decisions error (400)"),
        "{message}"
    );
    assert!(
        message.contains("Decision input exceeds the token limit."),
        "{message}"
    );
}

#[tokio::test]
async fn rejects_models_for_other_classifier_apis_and_missing_api_keys() {
    let client = MockHttpClient::always(MockResponse::json(json!({"answers": wire_answers()})));
    let other = Model {
        api: "typesafe-system-one".into(),
        ..model()
    };
    let other_api = classify(&other, &context_value(), Some(&options(&client))).await;
    let no_key = classify(
        &model(),
        &context_value(),
        Some(&StreamOptions {
            fetch: client.fetch(),
            ..StreamOptions::default()
        }),
    )
    .await;

    assert_eq!(client.request_count(), 0);
    assert!(other_api.error_message.as_deref().is_some_and(|message| {
        message.contains("Unsupported classifier API: typesafe-system-one")
    }));
    assert!(
        no_key
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("No API key for provider: openai"))
    );
}

#[tokio::test]
async fn rewrites_body_read_timeouts_and_retries_them() {
    // TS 的读体发生在 attempt 的 try/catch 内：超时信号触发时任何失败（2xx 与非 2xx 的
    // body 都算）都改写成 `Request timed out after <ms>ms` 并按 maxRetries 重试。
    for status in [200_u16, 500] {
        let client = mock_http::BodyStreamErrorClient::new(
            status,
            kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE,
        );
        let request_options = StreamOptions {
            api_key: Some("test".into()),
            fetch: client.fetch(),
            timeout_ms: Some(20),
            max_retries: Some(2),
            ..StreamOptions::default()
        };

        let error = kiana_ai::api::classifier_shared::post_classifier_request(
            "OpenAI Decisions",
            url::Url::parse("https://api.openai.com/v1/decisions").expect("url"),
            &model(),
            json!({"model": "x"}),
            Some(&request_options),
            &[],
            None,
        )
        .await
        .expect_err("timeout");

        assert_eq!(client.attempts(), 3, "status: {status}");
        assert_eq!(error.message, "Request timed out after 20ms");
        assert_eq!(error.status, None);
        assert!(error.is_timeout);
    }
}

#[tokio::test]
async fn does_not_retry_non_timeout_body_read_errors() {
    for status in [200_u16, 500] {
        let client = mock_http::BodyStreamErrorClient::new(status, "boom");
        let request_options = StreamOptions {
            api_key: Some("test".into()),
            fetch: client.fetch(),
            timeout_ms: Some(20),
            max_retries: Some(2),
            ..StreamOptions::default()
        };

        let error = kiana_ai::api::classifier_shared::post_classifier_request(
            "OpenAI Decisions",
            url::Url::parse("https://api.openai.com/v1/decisions").expect("url"),
            &model(),
            json!({"model": "x"}),
            Some(&request_options),
            &[],
            None,
        )
        .await
        .expect_err("body error");

        assert_eq!(client.attempts(), 1, "status: {status}");
        assert_eq!(error.message, "boom");
    }
}

#[tokio::test]
async fn orders_choice_probability_keys_like_object_from_entries() {
    // TS 用 `Object.fromEntries(value.map(([value, probability]) => ...))` 建普通对象：数组
    // 索引键按数值升序排在前面，其余键保持插入序。服务器乱序返回时必须复刻这个枚举序。
    // 黄金值来自 Node：`{"10":0.1,"2":0.2,"b":0.3}` → `["2","10","b"]`。
    let response = json!({
        "answers": [{
            "type": "choice",
            "name": "category",
            "choice": "2",
            "probabilities": [
                {"value": "10", "probability": 0.1},
                {"value": "2", "probability": 0.2},
                {"value": "b", "probability": 0.3}
            ],
            "confidence": 0.5
        }],
        "usage": wire_usage()
    });
    let client = MockHttpClient::always(MockResponse::json(response));
    let context = context(
        json!({"text": "x"}),
        questions(vec![(
            "category",
            ClassifierQuestion::Choice {
                instructions: "Classify".into(),
                criteria: [
                    ("10".to_string(), String::new()),
                    ("2".to_string(), String::new()),
                    ("b".to_string(), String::new()),
                ]
                .into_iter()
                .collect(),
            },
        )]),
    );

    let result = classify(&model(), &context, Some(&options(&client))).await;

    assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
    match &result.answers["category"] {
        ClassifierAnswer::Choice { probabilities, .. } => assert_eq!(
            probabilities.keys().cloned().collect::<Vec<_>>(),
            vec!["2".to_string(), "10".to_string(), "b".to_string()]
        ),
        other => panic!("unexpected answer: {other:?}"),
    }
}
