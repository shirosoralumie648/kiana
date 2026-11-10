//! `azure-openai-completions.test.ts`。
//!
//! 该 TS 文件通过 `azureProvider()` 间接测试 completions 适配器；Rust 侧 provider 工厂
//! 还没实现，因此这里用与 `providers/azure.ts#azureStreams` 等价的本地包装（归一 baseUrl +
//! 把 deployment 名称注入 `onPayload`），直接驱动 `openai-completions` 适配器。
//! 纯 provider 分派（api map）与 `createProvider` 相关的用例标记为 `#[ignore]`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::azure_openai_config::{
    AzureEndpointOptions, resolve_azure_base_url, resolve_deployment_name,
};
use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::providers::azure::azure_provider;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheRetention, Context, Message, Model,
    SimpleStreamOptions, StreamOptions, ThinkingContent, ThinkingLevel,
};
use serde_json::{Value, json};

fn deepseek_model() -> Model {
    let mut model = mock_http::catalog_model("azure", "deepseek-v4-pro");
    // provider 层会把 baseUrl 归一后交给适配器；这里给一个等效的固定端点。
    model.base_url = "https://my-resource.services.ai.azure.com/openai/v1".into();
    model
}

/// `azureStreams()` 的等价物：归一 baseUrl，并把 deployment 名称写进请求体。
fn azure_streams(model: &Model, options: StreamOptions) -> (Model, StreamOptions) {
    let endpoint_options = AzureEndpointOptions {
        base: StreamOptions {
            env: options.env.clone(),
            ..StreamOptions::default()
        },
        ..Default::default()
    };
    let mut request_model = model.clone();
    request_model.base_url =
        resolve_azure_base_url(model, Some(&endpoint_options)).expect("azure base url");
    let deployment_name = resolve_deployment_name(model, Some(&endpoint_options));
    let mut options = options;
    if deployment_name != model.id {
        let deployment_name_for_payload = deployment_name.clone();
        let existing = options.on_payload.take();
        options.on_payload = Some(Arc::new(move |payload, model| {
            let mut params = payload.as_object().cloned().unwrap_or_default();
            params.insert(
                "model".into(),
                Value::String(deployment_name_for_payload.clone()),
            );
            let params = Value::Object(params);
            match &existing {
                Some(existing) => existing(params.clone(), model).or(Some(params)),
                None => Some(params),
            }
        }));
    }
    (request_model, options)
}

fn success_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
        },
    })])
}

fn responses_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_azure_test",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2,
                "input_tokens_details": { "cached_tokens": 0 },
            },
        },
    })])
}

fn context() -> kiana_ai::types::TranscriptContext {
    mock_http::context_with(&Context {
        system_prompt: Some("sys".into()),
        messages: vec![mock_http::user("hi")],
        tools: None,
    })
}

async fn capture_simple(
    model: &Model,
    context: &kiana_ai::types::TranscriptContext,
    options: SimpleStreamOptions,
) -> (Value, mock_http::MockHttp) {
    let mock = mock_http::MockHttp::single(success_sse());
    let base = StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(Arc::new(mock.clone())),
        ..options.base
    };
    let (request_model, base) = azure_streams(model, base);
    let simple = SimpleStreamOptions {
        base,
        reasoning: options.reasoning,
        tool_choice: options.tool_choice,
        deferred: options.deferred,
        thinking_budgets: options.thinking_budgets,
    };
    let _ = mock_http::run_completions_simple(&request_model, context, simple).await;
    (mock.last_body(), mock)
}

async fn capture_full(
    model: &Model,
    context: &kiana_ai::types::TranscriptContext,
    base: StreamOptions,
) -> (AssistantMessage, mock_http::MockHttp) {
    let mock = mock_http::MockHttp::single(success_sse());
    let base = StreamOptions {
        fetch: Some(Arc::new(mock.clone())),
        ..base
    };
    let (request_model, base) = azure_streams(model, base);
    let options = OpenAICompletionsOptions {
        base,
        ..Default::default()
    };
    let message = mock_http::run_completions(&request_model, context, options).await;
    (message, mock)
}

#[tokio::test]
async fn turns_thinking_on_with_reasoning_effort_instead_of_deepseek_thinking_field() {
    for (reasoning, expected) in [(ThinkingLevel::High, "high"), (ThinkingLevel::Max, "high")] {
        let (body, _) = capture_simple(
            &deepseek_model(),
            &context(),
            SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-key".into()),
                    ..Default::default()
                },
                reasoning: Some(reasoning),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(body["reasoning_effort"], json!(expected), "{reasoning:?}");
        assert!(body.get("thinking").is_none(), "{reasoning:?}");
    }
}

#[tokio::test]
async fn sends_no_reasoning_effort_when_no_thinking_level_is_requested() {
    let (body, _) = capture_simple(
        &deepseek_model(),
        &context(),
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("thinking").is_none());
}

#[tokio::test]
async fn sends_the_system_prompt_under_the_system_role_and_keeps_mid_conversation_system_messages()
{
    let (body, _) = capture_full(
        &deepseek_model(),
        &context(),
        StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
    )
    .await;
    let _ = body;
    let resumed = mock_http::context_with(&Context {
        system_prompt: Some("first".into()),
        messages: vec![
            mock_http::user("hi"),
            Message::System(kiana_ai::types::SystemMessage {
                role: Default::default(),
                content: kiana_ai::types::SystemContent::Text("second".into()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: 2,
            }),
            mock_http::user("again"),
        ],
        tools: None,
    });
    let (_, mock) = capture_full(
        &deepseek_model(),
        &resumed,
        StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
    )
    .await;
    let body = mock.last_body();
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "system", "user"]);
}

#[tokio::test]
async fn omits_prompt_cache_parameters_even_when_long_retention_is_requested() {
    let (_, mock) = capture_full(
        &deepseek_model(),
        &context(),
        StreamOptions {
            api_key: Some("test-key".into()),
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-1".into()),
            ..Default::default()
        },
    )
    .await;
    let body = mock.last_body();
    assert!(body.get("prompt_cache_key").is_none());
    assert!(body.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn replays_reasoning_content_on_assistant_turns() {
    let assistant = AssistantMessage {
        role: Default::default(),
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "internal reasoning".into(),
                thinking_signature: Some("reasoning_content".into()),
                ..Default::default()
            }),
            AssistantContentBlock::Text(kiana_ai::types::TextContent {
                text: "answer".into(),
                ..Default::default()
            }),
        ],
        provider: "azure".into(),
        api: "openai-completions".into(),
        model: "deepseek-v4-pro".into(),
        timestamp: 1,
        stop_reason: kiana_ai::types::StopReason::Stop,
        ..Default::default()
    };
    let resumed = mock_http::context_with(&Context {
        system_prompt: Some("sys".into()),
        messages: vec![
            mock_http::user("first"),
            Message::Assistant(assistant),
            mock_http::user("second"),
        ],
        tools: None,
    });
    let (_, mock) = capture_full(
        &deepseek_model(),
        &resumed,
        StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
    )
    .await;
    let assistant_message = mock.last_body()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("assistant"))
        .cloned()
        .unwrap();
    assert_eq!(
        assistant_message["reasoning_content"],
        json!("internal reasoning")
    );
}

/// env 相关的端点解析/部署映射用例集中在一个测试里顺序执行（进程级环境变量）。
#[tokio::test]
async fn azure_chat_completions_endpoint_resolution() {
    let _guard = ENV_LOCK.lock().await;
    let model = deepseek_model();

    unsafe {
        std::env::set_var(
            "AZURE_OPENAI_BASE_URL",
            "https://my-resource.services.ai.azure.com",
        )
    };
    let (message, mock) = capture_full(
        &model,
        &context(),
        StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_request().url,
        "https://my-resource.services.ai.azure.com/openai/v1/chat/completions"
    );
    assert_eq!(mock.last_body()["model"], json!("deepseek-v4-pro"));
    assert_eq!(message.model, "deepseek-v4-pro");
    unsafe { std::env::remove_var("AZURE_OPENAI_BASE_URL") };

    // deployment 名称映射：请求体用 deployment，消息里保留 catalog id。
    unsafe {
        std::env::set_var(
            "AZURE_OPENAI_BASE_URL",
            "https://my-resource.services.ai.azure.com",
        )
    };
    unsafe {
        std::env::set_var(
            "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
            "deepseek-v4-pro=my-deepseek",
        )
    };
    let (message, mock) = capture_full(
        &model,
        &context(),
        StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(mock.last_body()["model"], json!("my-deepseek"));
    assert_eq!(message.model, "deepseek-v4-pro");

    // deployment 名称先写进 payload，再交给调用方的 onPayload（TS 的 withDeploymentName）。
    let seen: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let seen_for_payload = seen.clone();
    let mock = mock_http::MockHttp::single(success_sse());
    let base = StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(Arc::new(mock.clone())),
        on_payload: Some(Arc::new(move |payload: Value, _model: &Model| {
            *seen_for_payload
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(payload.clone());
            let mut params = payload.as_object().cloned().unwrap_or_default();
            params.insert("temperature".into(), json!(0.1));
            Some(Value::Object(params))
        })),
        ..Default::default()
    };
    let (request_model, base) = azure_streams(&model, base);
    let _ = mock_http::run_completions(
        &request_model,
        &context(),
        OpenAICompletionsOptions {
            base,
            ..Default::default()
        },
    )
    .await;
    let seen = seen
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("onPayload called");
    assert_eq!(seen["model"], json!("my-deepseek"));
    let body = mock.last_body();
    assert_eq!(body["model"], json!("my-deepseek"));
    assert_eq!(body["temperature"], json!(0.1));

    unsafe { std::env::remove_var("AZURE_OPENAI_DEPLOYMENT_NAME_MAP") };
    unsafe { std::env::remove_var("AZURE_OPENAI_BASE_URL") };
}

/// 进程级环境变量在同一个测试二进制里是共享状态；涉及 env 的用例用这把锁串行化。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn azure_provider_endpoint_errors_and_api_map() {
    let _guard = ENV_LOCK.lock().await;
    let provider = azure_provider();
    let transcript = context();
    let options = |mock: &mock_http::MockHttp| StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(Arc::new(mock.clone())),
        ..Default::default()
    };

    // 未配置端点：错误以 error 事件返回，而不是从 stream() 同步抛出。
    unsafe {
        std::env::remove_var("AZURE_OPENAI_BASE_URL");
        std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME");
    }
    let mock = mock_http::MockHttp::single(success_sse());
    let model = mock_http::catalog_model("azure", "deepseek-v4-pro");
    let result = provider
        .stream(&model, &transcript, options(&mock))
        .result()
        .await
        .expect("stream result");
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Azure OpenAI base URL is required"),
        "{:?}",
        result.error_message
    );
    assert_eq!(mock.request_count(), 0);

    unsafe {
        std::env::set_var(
            "AZURE_OPENAI_BASE_URL",
            "https://my-resource.services.ai.azure.com",
        )
    };

    // Responses 模型走 responses api。
    let mock = mock_http::MockHttp::single(responses_sse());
    let responses_model = mock_http::catalog_model("azure", "gpt-4o-mini");
    let result = provider
        .stream(&responses_model, &transcript, options(&mock))
        .result()
        .await
        .expect("stream result");
    assert_eq!(
        result.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert!(
        mock.last_request().url.contains("/responses"),
        "{}",
        mock.last_request().url
    );

    // openai-completions 模型走 chat completions。
    let mock = mock_http::MockHttp::single(success_sse());
    let completions_model = mock_http::catalog_model("azure", "deepseek-v4-pro");
    let result = provider
        .stream(&completions_model, &transcript, options(&mock))
        .result()
        .await
        .expect("stream result");
    assert_eq!(
        result.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert!(
        mock.last_request().url.contains("/chat/completions"),
        "{}",
        mock.last_request().url
    );

    unsafe { std::env::remove_var("AZURE_OPENAI_BASE_URL") };
}
