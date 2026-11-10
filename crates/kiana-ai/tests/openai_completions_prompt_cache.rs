//! `openai-completions-prompt-cache.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_completions::{OpenAICompletionsOptions, convert_messages};
use kiana_ai::types::{CacheRetention, Context, ProviderHeaders, StreamOptions};
use serde_json::{Value, json};

fn create_model(overrides: serde_json::Value) -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.compat = None;
    patch_model(&mut model, overrides);
    model
}

fn patch_model(model: &mut kiana_ai::types::Model, overrides: Value) {
    let object = overrides.as_object().cloned().unwrap_or_default();
    for (key, value) in object {
        match key.as_str() {
            "baseUrl" => model.base_url = value.as_str().unwrap().to_string(),
            "provider" => model.provider = value.as_str().unwrap().to_string(),
            "compat" => model.compat = Some(value),
            other => panic!("unsupported override {other}"),
        }
    }
}

struct Captured {
    payload: Value,
    mock: mock_http::MockHttp,
}

async fn capture(model: &kiana_ai::types::Model, base: StreamOptions) -> Captured {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })]));
    let options = OpenAICompletionsOptions {
        base: StreamOptions {
            fetch: Some(Arc::new(mock.clone())),
            ..base
        },
        ..Default::default()
    };
    let context = mock_http::context_with(&Context {
        system_prompt: Some("sys".into()),
        messages: vec![mock_http::user("hi")],
        tools: None,
    });
    let _ = mock_http::run_completions(model, &context, options).await;
    Captured {
        payload: mock.last_body(),
        mock,
    }
}

fn base(api_key: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(api_key.into()),
        ..StreamOptions::default()
    }
}

fn with(
    mut options: StreamOptions,
    session_id: Option<&str>,
    retention: Option<CacheRetention>,
    headers: Option<ProviderHeaders>,
) -> StreamOptions {
    options.session_id = session_id.map(str::to_string);
    options.cache_retention = retention;
    options.headers = headers;
    options
}

/// 该文件里涉及 `KIANA_CACHE_RETENTION` 的用例与其它用例放在同一个测试里顺序执行，
/// 避免进程级环境变量在并行测试间串扰。
#[tokio::test]
async fn prompt_cache_and_session_affinity_behavior() {
    // direct OpenAI requests：默认 short → prompt_cache_key，无 retention。
    let model = create_model(json!({}));
    let captured = capture(
        &model,
        with(base("test-key"), Some("session-123"), None, None),
    )
    .await;
    assert_eq!(captured.payload["prompt_cache_key"], json!("session-123"));
    assert!(captured.payload.get("prompt_cache_retention").is_none());

    // long → 24h。
    let captured = capture(
        &model,
        with(
            base("test-key"),
            Some("session-456"),
            Some(CacheRetention::Long),
            None,
        ),
    )
    .await;
    assert_eq!(captured.payload["prompt_cache_key"], json!("session-456"));
    assert_eq!(captured.payload["prompt_cache_retention"], json!("24h"));

    // 64 码点截断。
    let captured = capture(
        &model,
        with(base("test-key"), Some(&"x".repeat(67)), None, None),
    )
    .await;
    assert_eq!(captured.payload["prompt_cache_key"], json!("x".repeat(64)));

    // none → 两个字段都不出现。
    let captured = capture(
        &model,
        with(
            base("test-key"),
            Some("session-789"),
            Some(CacheRetention::None),
            None,
        ),
    )
    .await;
    assert!(captured.payload.get("prompt_cache_key").is_none());
    assert!(captured.payload.get("prompt_cache_retention").is_none());

    // 非 OpenAI baseUrl 且不支持 long retention → 都不出现。
    let proxy = create_model(json!({
        "baseUrl": "https://proxy.example.com/v1",
        "compat": { "supportsLongCacheRetention": false },
    }));
    let captured = capture(
        &proxy,
        with(
            base("test-key"),
            Some("session-proxy"),
            Some(CacheRetention::Long),
            None,
        ),
    )
    .await;
    assert!(captured.payload.get("prompt_cache_key").is_none());
    assert!(captured.payload.get("prompt_cache_retention").is_none());

    // KIANA_CACHE_RETENTION=long。
    unsafe { std::env::set_var("KIANA_CACHE_RETENTION", "long") };
    let captured = capture(
        &model,
        with(base("test-key"), Some("session-env"), None, None),
    )
    .await;
    unsafe { std::env::remove_var("KIANA_CACHE_RETENTION") };
    assert_eq!(captured.payload["prompt_cache_key"], json!("session-env"));
    assert_eq!(captured.payload["prompt_cache_retention"], json!("24h"));

    // sendSessionAffinityHeaders → 三个 header。
    let affinity = create_model(json!({
        "baseUrl": "https://proxy.example.com/v1",
        "compat": { "sendSessionAffinityHeaders": true },
    }));
    let captured = capture(
        &affinity,
        with(base("test-key"), Some("session-affinity"), None, None),
    )
    .await;
    assert_eq!(
        captured.mock.last_header("session_id").as_deref(),
        Some("session-affinity")
    );
    assert_eq!(
        captured.mock.last_header("x-client-request-id").as_deref(),
        Some("session-affinity")
    );
    assert_eq!(
        captured.mock.last_header("x-session-affinity").as_deref(),
        Some("session-affinity")
    );

    // Fireworks 内置模型。
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
    ] {
        let model = mock_http::catalog_model("fireworks", model_id);
        let captured = capture(
            &model,
            with(base("test-key"), Some("fireworks-session"), None, None),
        )
        .await;
        assert_eq!(
            captured.mock.last_header("x-session-affinity").as_deref(),
            Some("fireworks-session"),
            "{model_id}"
        );
    }

    // Baseten 内置模型。
    let model = mock_http::catalog_model("baseten", "zai-org/GLM-5.2");
    let captured = capture(
        &model,
        with(
            base("test-key"),
            Some("baseten-catalog-session"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(
        captured.mock.last_header("x-session-affinity").as_deref(),
        Some("baseten-catalog-session")
    );
    assert_eq!(
        captured.mock.last_header("x-client-request-id").as_deref(),
        Some("baseten-catalog-session")
    );

    // openai-nosession：不发 session_id，其它两个照发。
    let nosession = create_model(json!({
        "compat": { "sendSessionAffinityHeaders": true, "sessionAffinityFormat": "openai-nosession" },
    }));
    let captured = capture(
        &nosession,
        with(base("test-key"), Some("session-nosession"), None, None),
    )
    .await;
    assert!(captured.payload.get("session_id").is_none());
    assert_eq!(
        captured.payload["prompt_cache_key"],
        json!("session-nosession")
    );
    assert!(captured.mock.last_header("session_id").is_none());
    assert_eq!(
        captured.mock.last_header("x-client-request-id").as_deref(),
        Some("session-nosession")
    );
    assert_eq!(
        captured.mock.last_header("x-session-affinity").as_deref(),
        Some("session-nosession")
    );
    assert!(captured.mock.last_header("x-session-id").is_none());

    // openrouter 格式。
    let openrouter_format = create_model(json!({
        "baseUrl": "https://proxy.example.com/v1",
        "compat": { "sendSessionAffinityHeaders": true, "sessionAffinityFormat": "openrouter" },
    }));
    let captured = capture(
        &openrouter_format,
        with(base("test-key"), Some("session-proxy"), None, None),
    )
    .await;
    assert!(captured.payload.get("session_id").is_none());
    assert!(captured.payload.get("prompt_cache_key").is_none());
    assert_eq!(
        captured.mock.last_header("x-session-id").as_deref(),
        Some("session-proxy")
    );
    assert!(captured.mock.last_header("session_id").is_none());
    assert!(captured.mock.last_header("x-client-request-id").is_none());
    assert!(captured.mock.last_header("x-session-affinity").is_none());

    // 内置 OpenRouter 模型默认 openrouter 格式。
    let model = mock_http::catalog_model("openrouter", "auto");
    let captured = capture(
        &model,
        with(base("test-key"), Some("session-openrouter"), None, None),
    )
    .await;
    assert!(captured.payload.get("session_id").is_none());
    assert!(captured.payload.get("prompt_cache_key").is_none());
    assert_eq!(
        captured.mock.last_header("x-session-id").as_deref(),
        Some("session-openrouter")
    );
    assert!(captured.mock.last_header("session_id").is_none());
    assert!(captured.mock.last_header("x-client-request-id").is_none());
    assert!(captured.mock.last_header("x-session-affinity").is_none());

    // 显式关闭。
    let disabled = create_model(json!({
        "provider": "openrouter",
        "baseUrl": "https://openrouter.ai/api/v1",
        "compat": { "sendSessionAffinityHeaders": false },
    }));
    let captured = capture(
        &disabled,
        with(base("test-key"), Some("session-openrouter"), None, None),
    )
    .await;
    assert!(captured.payload.get("session_id").is_none());
    assert!(captured.payload.get("prompt_cache_key").is_none());
    assert!(captured.mock.last_header("x-session-id").is_none());

    // cacheRetention none → 不发 session-affinity。
    let affinity = create_model(json!({
        "baseUrl": "https://proxy.example.com/v1",
        "compat": { "sendSessionAffinityHeaders": true },
    }));
    let captured = capture(
        &affinity,
        with(
            base("test-key"),
            Some("session-affinity"),
            Some(CacheRetention::None),
            None,
        ),
    )
    .await;
    assert!(captured.mock.last_header("session_id").is_none());
    assert!(captured.mock.last_header("x-client-request-id").is_none());
    assert!(captured.mock.last_header("x-session-affinity").is_none());

    // 显式 header 覆盖生成的 affinity header。
    let mut headers = ProviderHeaders::new();
    headers.insert("session_id".into(), Some("override-session".into()));
    headers.insert(
        "x-client-request-id".into(),
        Some("override-request".into()),
    );
    headers.insert(
        "x-session-affinity".into(),
        Some("override-affinity".into()),
    );
    let captured = capture(
        &affinity,
        with(
            base("test-key"),
            Some("session-affinity"),
            None,
            Some(headers),
        ),
    )
    .await;
    assert_eq!(
        captured.mock.last_header("session_id").as_deref(),
        Some("override-session")
    );
    assert_eq!(
        captured.mock.last_header("x-client-request-id").as_deref(),
        Some("override-request")
    );
    assert_eq!(
        captured.mock.last_header("x-session-affinity").as_deref(),
        Some("override-affinity")
    );

    // 保证 create_model/convert_messages 未被优化掉（编译期覆盖导出面）。
    let _ = convert_messages(
        &model,
        &mock_http::text_context("hi"),
        &kiana_ai::api::openai_completions::get_compat(&model),
        None,
    );
}
