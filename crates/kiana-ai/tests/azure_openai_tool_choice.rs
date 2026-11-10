//! `azure-openai-tool-choice.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::azure_openai_responses::{AzureOpenAIResponsesOptions, stream, stream_simple};
use kiana_ai::types::{Context, Model, SimpleStreamOptions, StreamOptions, Tool, ToolChoice};
use serde_json::json;

fn model() -> Model {
    Model {
        id: "test-deployment".into(),
        name: "Test Deployment".into(),
        api: "azure-openai-responses".into(),
        provider: "azure".into(),
        base_url: "http://127.0.0.1:9/openai/v1".into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(10_000),
        max_tokens: Some(1000),
        ..Model::default()
    }
}

fn read_tool() -> Tool {
    Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        constrained_sampling: None,
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    mock_http::context_with(&Context {
        messages: vec![mock_http::user("Summarize this")],
        tools: Some(vec![read_tool()]),
        ..Context::default()
    })
}

#[tokio::test]
async fn forwards_provider_specific_tool_choice_while_preserving_tool_definitions() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "response": { "id": "resp", "status": "completed" },
    })]));
    let options = AzureOpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some("test-key".into()),
            fetch: Some(std::sync::Arc::new(mock.clone())),
            ..Default::default()
        },
        azure_base_url: Some("https://my-resource.openai.azure.com".into()),
        tool_choice: Some(json!("required")),
        ..Default::default()
    };
    let result = stream(&model(), &context(), options)
        .result()
        .await
        .expect("result");
    let payload = mock.last_body();
    assert_eq!(payload["tool_choice"], json!("required"));
    assert_eq!(payload["tools"].as_array().map(Vec::len), Some(1));
    // 请求完成后流正常结束（这里只验证 payload；mock 的响应是合法终止事件）。
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
}

#[tokio::test]
async fn forwards_provider_neutral_tool_choice_from_simple_options() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "response": { "id": "resp", "status": "completed" },
    })]));
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test-key".into()),
            fetch: Some(std::sync::Arc::new(mock.clone())),
            ..Default::default()
        },
        tool_choice: Some(ToolChoice::None),
        ..Default::default()
    };
    // azure 端点通过 env 或 model.baseUrl；这里直接用 model.baseUrl。
    let _ = stream_simple(&model(), &context(), options)
        .expect("stream")
        .result()
        .await
        .expect("result");
    let payload = mock.last_body();
    assert_eq!(payload["tool_choice"], json!("none"));
    assert_eq!(payload["tools"].as_array().map(Vec::len), Some(1));
}
