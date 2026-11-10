//! `test/openrouter-images.test.ts` 的 Rust 移植。
//!
//! TS 用 `vi.mock("openai")` 替换 SDK；Rust 侧没有 SDK，直接观察发往
//! `{baseUrl}/chat/completions` 的请求（黄金值来自 TS 参考实现的探针：
//! 请求体、header 集合与错误文本均逐字对齐，只省略 SDK 的 `x-stainless-*` 遥测头）。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::api::openrouter_images::generate_images;
use kiana_ai::http::{HttpError, HttpRequest};
use kiana_ai::models::ImagesOptions;
use kiana_ai::types::{
    ImagesContext, ImagesStopReason, Model, ModelCost, ModelInputModality, ModelType,
    StreamOptions, UserContentBlock,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use mock_http::{BodyStreamErrorClient, MockHttpClient, MockResponse};

fn model() -> Model {
    Model {
        id: "google/gemini-3.1-flash-image-preview".into(),
        name: "Gemini 3.1 Flash Image Preview".into(),
        api: "openrouter-images".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        output: Some(vec!["text".into(), "image".into()]),
        cost: ModelCost {
            input: 0.015,
            output: 0.03,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        headers: Some(
            [(
                "HTTP-Referer".to_string(),
                "https://example.com".to_string(),
            )]
            .into_iter()
            .collect(),
        ),
        model_type: Some(ModelType::Image),
        ..Model::default()
    }
}

fn image_only_model() -> Model {
    Model {
        id: "black-forest-labs/flux.2-pro".into(),
        name: "FLUX.2 Pro".into(),
        output: Some(vec!["image".into()]),
        headers: None,
        ..model()
    }
}

fn context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text: "Generate a dog".into(),
            text_signature: None,
        })],
    }
}

fn options(client: &Arc<MockHttpClient>) -> ImagesOptions {
    StreamOptions {
        api_key: Some("test".into()),
        fetch: client.fetch(),
        ..StreamOptions::default()
    }
}

fn success_response() -> Value {
    json!({
        "id": "img-1",
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 34,
            "prompt_tokens_details": {"cached_tokens": 0}
        },
        "choices": [{
            "message": {
                "content": "Here is your image.",
                "images": [{"image_url": "data:image/png;base64,ZmFrZS1wbmc="}]
            }
        }]
    })
}

#[tokio::test]
async fn returns_text_plus_images_in_final_output() {
    let client = MockHttpClient::always(MockResponse::json(success_response()));

    let output = generate_images(&model(), &context(), Some(&options(&client))).await;

    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert_eq!(output.response_id.as_deref(), Some("img-1"));
    assert_eq!(output.output.len(), 2);
    match &output.output[0] {
        UserContentBlock::Text(text) => assert_eq!(text.text, "Here is your image."),
        other => panic!("unexpected block: {other:?}"),
    }
    match &output.output[1] {
        UserContentBlock::Image(image) => {
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(image.data, "ZmFrZS1wbmc=");
        }
        other => panic!("unexpected block: {other:?}"),
    }
    let usage = output.usage.expect("usage");
    assert_eq!(
        (usage.input, usage.output, usage.total_tokens),
        (12, 34, 46)
    );
    assert!((usage.cost.input - 1.799_999_999_999_999_7e-7).abs() < 1e-20);
    assert!((usage.cost.output - 0.00000102).abs() < 1e-18);
    assert!((usage.cost.total - 0.0000012).abs() < 1e-18);
    assert_eq!(usage.cost.total_from_provider, Some(false));

    let requests = client.requests();
    assert_eq!(
        requests[0].request.url,
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(requests[0].header("authorization"), Some("Bearer test"));
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert_eq!(requests[0].header("accept"), Some("application/json"));
    assert_eq!(
        requests[0].header("http-referer"),
        Some("https://example.com")
    );
    assert!(requests[0].header("user-agent").is_some());
    assert_eq!(
        requests[0].json(),
        json!({
            "model": "google/gemini-3.1-flash-image-preview",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Generate a dog"}]}],
            "stream": false,
            "modalities": ["image", "text"]
        })
    );
}

#[tokio::test]
async fn passes_through_the_abort_signal_and_returns_an_aborted_result() {
    let client = MockHttpClient::always(MockResponse::json(success_response()));
    let token = CancellationToken::new();
    token.cancel();
    let mut request_options = options(&client);
    request_options.signal = Some(token);

    let output = generate_images(&model(), &context(), Some(&request_options)).await;

    assert_eq!(output.stop_reason, ImagesStopReason::Aborted);
    assert_eq!(output.error_message.as_deref(), Some("Request aborted"));
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].signal_cancelled);
}

#[tokio::test]
async fn image_only_models_must_not_request_text_output() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "id": "img-2",
        "choices": [{"message": {"images": [{"image_url": {"url": "data:image/jpeg;base64,ZmFrZQ=="}}]}}]
    })));

    let output = generate_images(&image_only_model(), &context(), Some(&options(&client))).await;

    assert!(matches!(output.output[0], UserContentBlock::Image(_)));
    assert_eq!(client.requests()[0].json()["modalities"], json!(["image"]));
}

#[tokio::test]
async fn maps_http_errors_like_the_openai_sdk() {
    // 黄金值来自 TS 参考（SDK 的 `APIError.generate` + `normalizeProviderError`）。
    let cases = [
        (
            MockResponse::json_status(
                400,
                json!({"error": {"message": "bad input", "type": "invalid_request_error"}}),
            ),
            "400: {\"message\":\"bad input\",\"type\":\"invalid_request_error\"}",
        ),
        (
            MockResponse::json_status(400, json!({"message": "plain"})),
            "400: {\"message\":\"plain\"}",
        ),
        (
            MockResponse::with_headers(400, "boom", &[("content-type", "text/plain")]),
            "400 boom",
        ),
    ];
    for (response, expected) in cases {
        let client = MockHttpClient::always(response);
        let mut request_options = options(&client);
        request_options.max_retries = Some(0);

        let output = generate_images(&model(), &context(), Some(&request_options)).await;

        assert_eq!(output.stop_reason, ImagesStopReason::Error);
        assert_eq!(output.error_message.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn reports_connection_errors_and_timeouts_with_the_sdk_text() {
    let network = MockHttpClient::try_new(|_: &HttpRequest| Err(HttpError::new("fetch failed")));
    let network_output = generate_images(&model(), &context(), Some(&options(&network))).await;
    assert_eq!(network_output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        network_output.error_message.as_deref(),
        Some("Connection error.")
    );

    let timeout = MockHttpClient::try_new(|request: &HttpRequest| {
        assert_eq!(request.timeout_ms, Some(20));
        Err(HttpError::new(
            kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE,
        ))
    });
    let mut request_options = options(&timeout);
    request_options.timeout_ms = Some(20);
    request_options.max_retries = Some(0);
    let timeout_output = generate_images(&model(), &context(), Some(&request_options)).await;
    assert_eq!(timeout_output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        timeout_output.error_message.as_deref(),
        Some("Request timed out.")
    );
}

#[tokio::test]
async fn retries_only_when_max_retries_allows_it() {
    async fn run(max_retries: Option<u32>) -> usize {
        let attempts = Arc::new(AtomicUsize::new(0));
        let client = {
            let attempts = Arc::clone(&attempts);
            MockHttpClient::new(move |_: &HttpRequest| {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    MockResponse::with_headers(503, "busy", &[("retry-after-ms", "0")])
                } else {
                    MockResponse::json(json!({"id": "img-3", "choices": []}))
                }
            })
        };
        let mut request_options = options(&client);
        request_options.max_retries = max_retries;
        generate_images(&model(), &context(), Some(&request_options)).await;
        client.request_count()
    }

    // SDK 的 `maxRetries` 缺省是 0：一次失败即结束。
    assert_eq!(run(None).await, 1);
    assert_eq!(run(Some(1)).await, 2);
}

#[tokio::test]
async fn splits_cache_reads_and_writes_like_the_sdk_usage_parser() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "id": "img-4",
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "prompt_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 20}
        },
        "choices": []
    })));

    let output = generate_images(&model(), &context(), Some(&options(&client))).await;

    let usage = output.usage.expect("usage");
    // cacheRead = max(0, 30 - 20) = 10; input = max(0, 100 - 10 - 20) = 70。
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
            usage.total_tokens
        ),
        (70, 10, 10, 20, 110)
    );
}

#[tokio::test]
async fn rejects_missing_api_keys_without_sending() {
    let client = MockHttpClient::always(MockResponse::json(success_response()));

    let output = generate_images(
        &model(),
        &context(),
        Some(&StreamOptions {
            fetch: client.fetch(),
            ..StreamOptions::default()
        }),
    )
    .await;

    assert_eq!(client.request_count(), 0);
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("No API key for provider: openrouter")
    );
}

#[tokio::test]
async fn skips_non_data_url_and_malformed_image_entries() {
    let client = MockHttpClient::always(MockResponse::json(json!({
        "id": "img-5",
        "choices": [{
            "message": {
                "images": [
                    {"image_url": "https://example.com/cat.png"},
                    {"image_url": "data:image/png;notbase64,abc"},
                    {"image_url": {"url": "data:image/webp;base64,d2VicA=="}}
                ]
            }
        }]
    })));

    let output = generate_images(&model(), &context(), Some(&options(&client))).await;

    assert_eq!(output.output.len(), 1);
    match &output.output[0] {
        UserContentBlock::Image(image) => {
            assert_eq!(image.mime_type, "image/webp");
            assert_eq!(image.data, "d2VicA==");
        }
        other => panic!("unexpected block: {other:?}"),
    }
}

#[tokio::test]
async fn reproduces_the_sdk_type_errors_for_malformed_success_bodies() {
    // 黄金值来自 TS 参考实现（真实 openai SDK 的 TypeError 文本）。
    let cases = [
        (
            "null",
            None,
            "Cannot read properties of null (reading 'id')",
        ),
        (
            "\"abc\"",
            None,
            "Cannot read properties of undefined (reading '0')",
        ),
        (
            "[]",
            None,
            "Cannot read properties of undefined (reading '0')",
        ),
        (
            "{}",
            None,
            "Cannot read properties of undefined (reading '0')",
        ),
        (
            "{\"id\":\"x\"}",
            Some("x"),
            "Cannot read properties of undefined (reading '0')",
        ),
        (
            "{\"choices\":[{}]}",
            None,
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            "{\"choices\":[{\"message\":null}]}",
            None,
            "Cannot read properties of null (reading 'content')",
        ),
        (
            "{\"choices\":[{\"message\":{\"images\":5}}]}",
            None,
            "number 5 is not iterable (cannot read property Symbol(Symbol.iterator))",
        ),
    ];
    for (body, response_id, expected) in cases {
        let client = MockHttpClient::always(MockResponse::with_headers(
            200,
            body,
            &[("content-type", "application/json")],
        ));

        let output = generate_images(&model(), &context(), Some(&options(&client))).await;

        assert_eq!(output.stop_reason, ImagesStopReason::Error, "body: {body}");
        assert_eq!(
            output.error_message.as_deref(),
            Some(expected),
            "body: {body}"
        );
        assert_eq!(output.response_id.as_deref(), response_id, "body: {body}");
    }

    // `choices: []` / `{"message": {}}` / 非数组 choices 都不报错。
    for body in [
        "{\"choices\":[]}",
        "{\"choices\":[{\"message\":{}}]}",
        "{\"choices\":5}",
    ] {
        let client = MockHttpClient::always(MockResponse::with_headers(
            200,
            body,
            &[("content-type", "application/json")],
        ));
        let output = generate_images(&model(), &context(), Some(&options(&client))).await;
        assert_eq!(output.stop_reason, ImagesStopReason::Stop, "body: {body}");
    }
}

#[tokio::test]
async fn applies_the_sdk_default_request_timeout() {
    let client = MockHttpClient::always(MockResponse::json(json!({"id": "x", "choices": []})));

    generate_images(&model(), &context(), Some(&options(&client))).await;
    assert_eq!(client.requests()[0].request.timeout_ms, Some(600_000));

    let mut request_options = options(&client);
    request_options.timeout_ms = Some(1234);
    generate_images(&model(), &context(), Some(&request_options)).await;
    assert_eq!(client.requests()[1].request.timeout_ms, Some(1234));
}

#[tokio::test]
async fn rewrites_body_read_timeouts_and_retries_them() {
    // TS：body 读取在 attempt 的 try/catch 里，超时信号触发时改写为 SDK 的
    // `APIConnectionTimeoutError`（"Request timed out."）并按 maxRetries 重试。
    for status in [200_u16, 500] {
        let client =
            BodyStreamErrorClient::new(status, kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE);
        let request_options = StreamOptions {
            api_key: Some("test".into()),
            fetch: client.fetch(),
            timeout_ms: Some(20),
            max_retries: Some(2),
            ..StreamOptions::default()
        };

        let output = generate_images(&model(), &context(), Some(&request_options)).await;

        assert_eq!(client.attempts(), 3, "status: {status}");
        assert_eq!(output.stop_reason, ImagesStopReason::Error);
        assert_eq!(
            output.error_message.as_deref(),
            Some("Request timed out."),
            "status: {status}"
        );
    }
}

#[tokio::test]
async fn parses_success_bodies_like_the_sdk() {
    // SDK `defaultParseResponse`：204 → null；JSON content-type 才 JSON.parse（空 body /
    // content-length 0 → undefined）；其他 content-type 返回原始文本。黄金值来自真实 SDK。
    let cases: &[(MockResponse, ImagesStopReason, Option<&str>, Option<&str>)] = &[
        (
            MockResponse::with_headers(
                200,
                "{\"id\":\"x\",\"choices\":[]}",
                &[("content-type", "text/plain")],
            ),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "", &[("content-type", "text/plain")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(
                200,
                "",
                &[("content-type", "text/plain"), ("content-length", "0")],
            ),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "", &[("content-type", "application/json")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading 'id')"),
            None,
        ),
        (
            MockResponse::with_headers(
                200,
                "",
                &[
                    ("content-type", "application/json"),
                    ("content-length", "0"),
                ],
            ),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading 'id')"),
            None,
        ),
        (
            MockResponse::with_headers(204, "", &[]),
            ImagesStopReason::Error,
            Some("Cannot read properties of null (reading 'id')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "<html>ok</html>", &[("content-type", "text/html")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(
                200,
                "{\"id\":\"x\",\"choices\":[]}",
                &[("content-type", "application/vnd.api+json; charset=utf-8")],
            ),
            ImagesStopReason::Stop,
            None,
            Some("x"),
        ),
        (
            MockResponse::with_headers(200, "{\"id\":\"x\",\"choices\":[]}", &[]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "null", &[("content-type", "application/json")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of null (reading 'id')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "5", &[("content-type", "application/json")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "\"abc\"", &[("content-type", "application/json")]),
            ImagesStopReason::Error,
            Some("Cannot read properties of undefined (reading '0')"),
            None,
        ),
        (
            MockResponse::with_headers(200, "{bad", &[("content-type", "application/json")]),
            ImagesStopReason::Error,
            Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)"),
            None,
        ),
    ];
    for (response, stop_reason, error_message, response_id) in cases {
        let client = MockHttpClient::always(response.clone());
        let mut request_options = options(&client);
        request_options.max_retries = Some(0);

        let output = generate_images(&model(), &context(), Some(&request_options)).await;

        assert_eq!(output.stop_reason, *stop_reason, "body: {response:?}");
        assert_eq!(
            output.error_message.as_deref(),
            *error_message,
            "body: {response:?}"
        );
        assert_eq!(
            output.response_id.as_deref(),
            *response_id,
            "body: {response:?}"
        );
    }
}

async fn output_for_body(body: Value) -> kiana_ai::types::AssistantImages {
    let client = MockHttpClient::always(MockResponse::json(body));
    generate_images(&model(), &context(), Some(&options(&client))).await
}

#[tokio::test]
async fn skips_falsy_first_choices_like_js_truthiness() {
    // `const choice = imageResponse.choices[0]; if (choice) { ... }`：假值直接跳过。
    for first in [json!(null), json!(0), json!(false), json!("")] {
        let output = output_for_body(json!({"id": "x", "choices": [first]})).await;
        assert_eq!(
            output.stop_reason,
            ImagesStopReason::Stop,
            "choice: {first}"
        );
        assert!(output.output.is_empty(), "choice: {first}");
        assert_eq!(output.response_id.as_deref(), Some("x"));
    }
    for choices in [json!(""), json!({})] {
        let output = output_for_body(json!({"id": "x", "choices": choices})).await;
        assert_eq!(
            output.stop_reason,
            ImagesStopReason::Stop,
            "choices: {choices}"
        );
    }
    // `imageResponse.choices` 是对象时按 JS 属性访问取 "0" 键。
    let output = output_for_body(json!({
        "id": "x",
        "choices": {"0": {"message": {"content": "hi"}}}
    }))
    .await;
    assert_eq!(output.stop_reason, ImagesStopReason::Stop);
    assert_eq!(output.output.len(), 1);
    // `message` 是原始值/数组时 `.content`/`.images` 都是 undefined（不抛错）。
    for message in [json!("x"), json!(5), json!(true), json!([]), json!(["a"])] {
        let output = output_for_body(json!({"id": "x", "choices": [{"message": message}]})).await;
        assert_eq!(
            output.stop_reason,
            ImagesStopReason::Stop,
            "message: {message}"
        );
        assert!(output.output.is_empty(), "message: {message}");
    }
}

#[tokio::test]
async fn reports_the_sdk_type_errors_for_truthy_malformed_choices() {
    for (choices, expected) in [
        (
            json!(["a"]),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!([5]),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!([true]),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!([[]]),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!([{}]),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!("ab"),
            "Cannot read properties of undefined (reading 'content')",
        ),
        (
            json!([{"message": null}]),
            "Cannot read properties of null (reading 'content')",
        ),
        (
            json!({"0": 5}),
            "Cannot read properties of undefined (reading 'content')",
        ),
    ] {
        let output = output_for_body(json!({"id": "x", "choices": choices})).await;
        assert_eq!(
            output.stop_reason,
            ImagesStopReason::Error,
            "choices: {choices}"
        );
        assert_eq!(
            output.error_message.as_deref(),
            Some(expected),
            "choices: {choices}"
        );
        assert_eq!(output.response_id.as_deref(), Some("x"));
    }
}

#[tokio::test]
async fn applies_js_property_access_to_image_entries() {
    // null 条目：`image.image_url` 抛 TypeError，且已推入的文本保留。
    let output = output_for_body(json!({
        "id": "x",
        "choices": [{"message": {"content": "hi", "images": [null]}}]
    }))
    .await;
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("Cannot read properties of null (reading 'image_url')")
    );
    assert_eq!(output.output.len(), 1);

    // 原始值/无 url 的条目：属性访问得到 undefined，全部跳过。
    for image in [
        json!(5),
        json!(false),
        json!("x"),
        json!({"image_url": 5}),
        json!({"image_url": {"url": null}}),
        json!({"image_url": null}),
    ] {
        let output = output_for_body(json!({
            "id": "x",
            "choices": [{"message": {"content": "hi", "images": [image]}}]
        }))
        .await;
        assert_eq!(output.stop_reason, ImagesStopReason::Stop, "image: {image}");
        assert_eq!(output.output.len(), 1, "image: {image}");
    }

    // 非字符串 url：`imageUrl?.startsWith is not a function`。
    let output = output_for_body(json!({
        "id": "x",
        "choices": [{"message": {"content": "hi", "images": [{"image_url": {"url": 5}}]}}]
    }))
    .await;
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("imageUrl?.startsWith is not a function")
    );
    assert_eq!(output.output.len(), 1);
}

#[tokio::test]
async fn rejects_invalid_default_header_names_like_the_openai_sdk() {
    // SDK 的 `buildHeaders` 用 `/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/` 校验头名（request build
    // 阶段，`onPayload` 之后），抛 `TypeError` 且不发出请求。黄金值来自真实 openai@7.19.0。
    let client = MockHttpClient::always(MockResponse::json(success_response()));
    let payloads = Arc::new(AtomicUsize::new(0));
    let mut request_options = options(&client);
    request_options.headers = Some(
        [("bad name".to_string(), Some("1".to_string()))]
            .into_iter()
            .collect(),
    );
    request_options.on_payload = Some(Arc::new({
        let payloads = Arc::clone(&payloads);
        move |params: Value, _model: &Model| {
            payloads.fetch_add(1, Ordering::SeqCst);
            Some(params)
        }
    }));

    let output = generate_images(&model(), &context(), Some(&request_options)).await;

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("Header name must be a valid HTTP token [\"bad name\"]")
    );
    assert_eq!(output.output.len(), 0);
    // 没有设置 `OPENAI_CUSTOM_HEADERS` 时，校验发生在 `onPayload` 之后。
    assert_eq!(payloads.load(Ordering::SeqCst), 1);
    assert_eq!(client.request_count(), 0);
}

#[tokio::test]
async fn reports_invalid_base_urls_before_header_validation() {
    // SDK 的 `buildRequest` 先 `buildURL(...)` 再 `buildHeaders(...)`：baseUrl 非法时
    // 报 `Invalid URL`（黄金值来自真实 openai@7.19.0 探针）。
    let client = MockHttpClient::always(MockResponse::json(success_response()));
    let mut model = model();
    model.base_url = "not a url".into();
    let mut request_options = options(&client);
    request_options.headers = Some(
        [("bad name".to_string(), Some("1".to_string()))]
            .into_iter()
            .collect(),
    );

    let output = generate_images(&model, &context(), Some(&request_options)).await;

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    assert_eq!(output.error_message.as_deref(), Some("Invalid URL"));
    assert_eq!(client.request_count(), 0);
}
