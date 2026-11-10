//! `openai-responses-partial-json-cleanup.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::process_responses_stream;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{AssistantContentBlock, AssistantMessageEvent, Model, StopReason};
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        id: "gpt-5-mini".into(),
        name: "GPT-5 Mini".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

fn events(arguments_json: &str) -> Vec<Value> {
    vec![
        json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\"" }),
        json!({ "type": "response.function_call_arguments.delta", "delta": ",\"content\":\"updated\"}" }),
        json!({ "type": "response.function_call_arguments.done", "arguments": arguments_json }),
        json!({ "type": "response.output_item.done", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": arguments_json } }),
        json!({ "type": "response.completed", "sequence_number": 5, "response": { "id": "resp_test", "status": "completed" } }),
    ]
}

async fn run(
    arguments_json: &str,
) -> (
    kiana_ai::types::AssistantMessage,
    Vec<AssistantMessageEvent>,
) {
    let model = model();
    let mut output = mock_http::responses_output(&model);
    let stream = AssistantMessageEventStream::new();
    process_responses_stream(
        mock_http::event_stream(events(arguments_json)),
        &mut output,
        &stream,
        &model,
        None,
    )
    .await
    .expect("process");
    let emitted = mock_http::drain_queued(&stream).await;
    (output, emitted)
}

#[tokio::test]
async fn rejects_function_call_arguments_done_without_a_string_arguments_field() {
    // TS 无条件执行 `replace(event.arguments)`，非字符串会在 `startsWith` 处抛 TypeError，
    // 由外层 catch 转成 stopReason "error"。
    async fn run_done_arguments(value: Option<Value>) -> Result<(), String> {
        let model = model();
        let mut done = json!({ "type": "response.function_call_arguments.done" });
        if let Some(arguments) = value {
            done["arguments"] = arguments;
        }
        let events = vec![
            json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" } }),
            json!({ "type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\"}" }),
            done,
            json!({ "type": "response.completed", "sequence_number": 4, "response": { "id": "resp_test", "status": "completed" } }),
        ];
        let mut output = mock_http::responses_output(&model);
        let stream = AssistantMessageEventStream::new();
        process_responses_stream(
            mock_http::event_stream(events),
            &mut output,
            &stream,
            &model,
            None,
        )
        .await
        .map_err(|error| error.message)
    }

    assert_eq!(
        run_done_arguments(None).await.unwrap_err(),
        "Cannot read properties of undefined (reading 'startsWith')"
    );
    assert_eq!(
        run_done_arguments(Some(Value::Null)).await.unwrap_err(),
        "Cannot read properties of null (reading 'startsWith')"
    );
    assert_eq!(
        run_done_arguments(Some(json!(42))).await.unwrap_err(),
        "text.startsWith is not a function"
    );
}

#[tokio::test]
async fn reports_the_type_error_when_a_stream_omits_arguments_on_the_done_event() {
    use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
    use kiana_ai::types::StreamOptions;

    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\"" }),
        json!({ "type": "response.function_call_arguments.done" }),
    ]));
    let message = mock_http::run_responses(
        &model(),
        &mock_http::context(vec![]),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(std::sync::Arc::new(mock)),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Cannot read properties of undefined (reading 'startsWith')")
    );
}

#[tokio::test]
async fn removes_partial_json_from_persisted_tool_call_blocks_at_output_item_done() {
    let arguments_json = "{\"path\":\"README.md\",\"content\":\"updated\"}";
    let (output, emitted) = run(arguments_json).await;
    assert_eq!(output.content.len(), 1);
    let AssistantContentBlock::ToolCall(tool_call) = &output.content[0] else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(&tool_call.arguments).unwrap(),
        json!({ "path": "README.md", "content": "updated" })
    );
    assert!(tool_call.arguments_error.is_none());

    let toolcall_end = emitted
        .iter()
        .find(|event| event.kind() == "toolcall_end")
        .expect("toolcall_end");
    let AssistantMessageEvent::ToolCallEnd {
        tool_call: emitted_call,
        ..
    } = toolcall_end
    else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(&emitted_call.arguments).unwrap(),
        json!({ "path": "README.md", "content": "updated" })
    );
    assert!(emitted_call.arguments_error.is_none());
}

#[tokio::test]
async fn marks_malformed_final_arguments_instead_of_keeping_the_parsed_prefix() {
    let arguments_json = "{\"path\":\"README.md\",\"content\":\"updated\",\"mode\":.5}";
    let (output, _) = run(arguments_json).await;
    let AssistantContentBlock::ToolCall(tool_call) = &output.content[0] else {
        panic!()
    };
    assert!(tool_call.arguments.is_empty());
    assert!(
        tool_call
            .arguments_error
            .as_deref()
            .is_some_and(|error| error.ends_with(&format!("Received arguments:\n{arguments_json}")))
    );
    assert_eq!(output.stop_reason, StopReason::ToolUse);
}
