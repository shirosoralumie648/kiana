//! `openai-responses-terminal-event.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::process_responses_stream;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{AssistantMessage, AssistantMessageEvent, Model, StopReason};
use serde_json::{Value, json};

fn create_model() -> Model {
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

async fn run(events: Vec<Value>) -> (Result<(), String>, AssistantMessage) {
    let model = create_model();
    let mut output = mock_http::responses_output(&model);
    let stream = AssistantMessageEventStream::new();
    let result = process_responses_stream(
        mock_http::event_stream(events),
        &mut output,
        &stream,
        &model,
        None,
    )
    .await
    .map_err(|error| error.message);
    (result, output)
}

#[tokio::test]
async fn rejects_streams_that_end_before_a_terminal_response_event() {
    let (result, _) = run(vec![
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] },
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "sequence_number": 2,
            "output_index": 0,
            "content_index": 0,
            "item_id": "rs_early_eof",
            "delta": "partial reasoning before the stream ends",
        }),
    ])
    .await;
    assert_eq!(
        result.unwrap_err(),
        "OpenAI Responses stream ended before a terminal response event"
    );
}

#[tokio::test]
async fn rejects_completed_streams_whose_tool_call_never_received_output_item_done() {
    let (result, _) = run(vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": "" },
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "sequence_number": 1,
            "output_index": 0,
            "item_id": "fc_1",
            "delta": "{\"command\":\"rm -rf /tmp/build",
        }),
        json!({ "type": "response.completed", "sequence_number": 2, "response": { "id": "resp_unfinished", "status": "completed" } }),
    ])
    .await;
    assert_eq!(
        result.unwrap_err(),
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_1|fc_1)"
    );
}

fn call_item(n: &str, arguments: &str) -> Value {
    json!({
        "type": "function_call",
        "id": format!("fc_{n}"),
        "call_id": format!("call_{n}"),
        "name": "bash",
        "arguments": arguments,
    })
}

#[tokio::test]
async fn rejects_parallel_tool_calls_without_output_index_instead_of_running_mixed_up_calls() {
    let events = vec![
        json!({ "type": "response.output_item.added", "item": call_item("a", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_a", "delta": "{\"command\":\"echo a\"}" }),
        json!({ "type": "response.output_item.added", "item": call_item("b", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_b", "delta": "{\"command\":\"echo b\"}" }),
        json!({ "type": "response.output_item.done", "item": call_item("a", "{\"command\":\"echo a\"}") }),
        json!({ "type": "response.output_item.done", "item": call_item("b", "{\"command\":\"echo b\"}") }),
        json!({ "type": "response.completed", "response": { "id": "resp_no_output_index", "status": "completed" } }),
    ];
    // 事件里没有 output_index：与 TS 的 `Map<undefined>` 单槽位语义一致。
    let (result, _) = run(events).await;
    assert_eq!(
        result.unwrap_err(),
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_a|fc_a)"
    );
}

#[tokio::test]
async fn forwards_parsed_provider_stream_events_in_order() {
    let model = create_model();
    let context = mock_http::context(vec![]);
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] },
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "sequence_number": 2,
            "output_index": 0,
            "content_index": 0,
            "item_id": "rs_early_eof",
            "delta": "partial reasoning before the wrapper stream ends",
        }),
    ]));
    let provider_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let captured = provider_events.clone();
    let options = kiana_ai::api::openai_responses::OpenAIResponsesOptions {
        base: kiana_ai::types::StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(std::sync::Arc::new(mock)),
            on_provider_stream_event: Some(std::sync::Arc::new(move |event, _model| {
                captured.lock().unwrap().push(event);
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = mock_http::run_responses(&model, &context, options).await;
    let events = provider_events.lock().unwrap();
    assert_eq!(events.len(), 3);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "response.created",
            "response.output_item.added",
            "response.reasoning_text.delta"
        ]
    );
}

#[tokio::test]
async fn emits_an_error_final_result_when_the_wrapper_stream_ends_before_a_terminal_response_event()
{
    let model = create_model();
    let context = mock_http::context(vec![]);
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({
            "type": "response.output_item.added",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] },
        }),
        json!({
            "type": "response.reasoning_text.delta",
            "sequence_number": 2,
            "output_index": 0,
            "content_index": 0,
            "item_id": "rs_early_eof",
            "delta": "partial reasoning",
        }),
    ]));
    let options = kiana_ai::api::openai_responses::OpenAIResponsesOptions {
        base: kiana_ai::types::StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(std::sync::Arc::new(mock)),
            ..Default::default()
        },
        ..Default::default()
    };
    let stream = kiana_ai::api::openai_responses::stream(&model, &context, options);
    let mut events = Vec::new();
    let mut initial_stop_reason = None;
    while let Some(event) = stream.recv().await {
        if let AssistantMessageEvent::Start { partial } = &event {
            initial_stop_reason = Some(partial.stop_reason);
        }
        events.push(event);
    }
    let result = stream.result().await.expect("result");
    assert_eq!(initial_stop_reason, Some(StopReason::Pending));
    let last = events.last().unwrap();
    assert_eq!(last.kind(), "error");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("OpenAI Responses stream ended before a terminal response event")
    );
}

fn phased_events(phases: [&str; 2], terminal_status: &str) -> Vec<Value> {
    let mut events = vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_phase",
                "role": "assistant",
                "status": "in_progress",
                "content": [],
                "phase": phases[0],
            },
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": "msg_phase",
                "role": "assistant",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "answer", "annotations": [] }],
                "phase": phases[1],
            },
        }),
    ];
    if terminal_status == "incomplete" {
        events.push(json!({
            "type": "response.incomplete",
            "sequence_number": 2,
            "response": {
                "id": "resp_phase",
                "status": "incomplete",
                "incomplete_details": { "reason": "max_output_tokens" },
            },
        }));
    } else {
        events.push(json!({
            "type": "response.completed",
            "sequence_number": 2,
            "response": { "id": "resp_phase", "status": "completed" },
        }));
    }
    events
}

async fn observed_stop_reasons(events: Vec<Value>) -> (Vec<StopReason>, AssistantMessage) {
    let model = create_model();
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
    .expect("process");
    let observed = mock_http::drain_queued(&stream)
        .await
        .iter()
        .filter_map(|event| event.partial().map(|partial| partial.stop_reason))
        .collect();
    (observed, output)
}

#[tokio::test]
async fn tracks_message_phases() {
    for (phases, expected) in [
        (
            ["commentary", "commentary"],
            vec![StopReason::Pending, StopReason::Pending],
        ),
        (
            ["final_answer", "final_answer"],
            vec![StopReason::Stop, StopReason::Stop],
        ),
        (
            ["commentary", "final_answer"],
            vec![StopReason::Pending, StopReason::Stop],
        ),
    ] {
        let (observed, output) = observed_stop_reasons(phased_events(phases, "completed")).await;
        assert_eq!(observed, expected, "{phases:?}");
        assert_eq!(output.stop_reason, StopReason::Stop);
    }
}

#[tokio::test]
async fn replaces_a_provisional_final_answer_stop_with_an_incomplete_terminal_reason() {
    let (observed, output) = observed_stop_reasons(phased_events(
        ["final_answer", "final_answer"],
        "incomplete",
    ))
    .await;
    assert_eq!(observed, vec![StopReason::Stop, StopReason::Stop]);
    assert_eq!(output.stop_reason, StopReason::Length);
}

#[tokio::test]
async fn finalizes_completed_terminal_events_as_stop() {
    let (result, output) = run(vec![json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_completed",
            "status": "completed",
            "usage": {
                "input_tokens": 20,
                "output_tokens": 7,
                "total_tokens": 27,
                "input_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 },
            },
        },
    })])
    .await;
    result.expect("process");
    assert_eq!(output.response_id.as_deref(), Some("resp_completed"));
    assert_eq!(output.stop_reason, StopReason::Stop);
    assert_eq!(output.raw_stop_reason.as_deref(), Some("completed"));
    assert_eq!(output.usage.input, 15);
    assert_eq!(output.usage.output, 7);
    assert_eq!(output.usage.cache_read, 2);
    assert_eq!(output.usage.cache_write, 3);
    assert_eq!(output.usage.total_tokens, 27);
}

fn incomplete_response(reason: &str) -> Vec<Value> {
    vec![json!({
        "type": "response.incomplete",
        "sequence_number": 0,
        "response": {
            "id": "resp_incomplete",
            "status": "incomplete",
            "incomplete_details": { "reason": reason },
            "usage": {
                "input_tokens": 30,
                "output_tokens": 12,
                "total_tokens": 42,
                "input_tokens_details": { "cached_tokens": 5 },
            },
        },
    })]
}

#[tokio::test]
async fn finalizes_incomplete_terminal_events_as_length_stops() {
    let (result, output) = run(incomplete_response("max_output_tokens")).await;
    result.expect("process");
    assert_eq!(output.response_id.as_deref(), Some("resp_incomplete"));
    assert_eq!(output.stop_reason, StopReason::Length);
    assert_eq!(
        output.raw_stop_reason.as_deref(),
        Some("incomplete.max_output_tokens")
    );
    assert_eq!(output.usage.input, 25);
    assert_eq!(output.usage.output, 12);
    assert_eq!(output.usage.cache_read, 5);
    assert_eq!(output.usage.cache_write, 0);
    assert_eq!(output.usage.total_tokens, 42);
}

#[tokio::test]
async fn finalizes_content_filtered_and_unknown_incomplete_reasons_as_errors() {
    let (result, output) = run(incomplete_response("content_filter")).await;
    result.expect("process");
    assert_eq!(output.stop_reason, StopReason::Error);
    assert_eq!(
        output.raw_stop_reason.as_deref(),
        Some("incomplete.content_filter")
    );
    assert_eq!(
        output.error_message.as_deref(),
        Some("Response incomplete: content_filter")
    );

    let (result, output) = run(incomplete_response("max_time_limit")).await;
    result.expect("process");
    assert_eq!(output.stop_reason, StopReason::Error);
    assert_eq!(
        output.raw_stop_reason.as_deref(),
        Some("incomplete.max_time_limit")
    );
    assert_eq!(
        output.error_message.as_deref(),
        Some("Response incomplete: max_time_limit")
    );
}

#[tokio::test]
async fn rejects_failed_terminal_events_with_the_provider_error() {
    let (result, output) = run(vec![json!({
        "type": "response.failed",
        "sequence_number": 0,
        "response": {
            "id": "resp_failed",
            "status": "failed",
            "error": { "code": "server_error", "message": "boom" },
        },
    })])
    .await;
    assert_eq!(result.unwrap_err(), "server_error: boom");
    assert_eq!(output.raw_stop_reason.as_deref(), Some("failed"));
}
