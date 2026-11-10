//! `openai-responses-namespace.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, convert_responses_messages, process_responses_stream,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{AssistantContentBlock, Model, ToolCall};
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        id: "gpt-5.4".into(),
        name: "GPT-5.4".into(),
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

async fn run_events(
    events: Vec<Value>,
    grammar: Option<&indexmap::IndexMap<String, String>>,
) -> kiana_ai::types::AssistantMessage {
    let model = model();
    let mut output = mock_http::responses_output(&model);
    let stream = AssistantMessageEventStream::new();
    let options = kiana_ai::api::openai_responses_shared::OpenAIResponsesStreamOptions {
        grammar_tool_input_properties: grammar.cloned(),
        ..Default::default()
    };
    process_responses_stream(
        mock_http::event_stream(events),
        &mut output,
        &stream,
        &model,
        Some(&options),
    )
    .await
    .expect("process");
    output
}

fn function_call_events() -> Vec<Value> {
    vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": "" },
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": {
                "type": "function_call",
                "id": "fc_test",
                "call_id": "call_test",
                "name": "lookup",
                "arguments": "{\"value\":\"hello\"}",
                "namespace": "dynamic_tools",
            },
        }),
        json!({ "type": "response.completed", "sequence_number": 2, "response": { "id": "resp_test", "status": "completed" } }),
    ]
}

fn custom_tool_call_events() -> Vec<Value> {
    vec![
        json!({
            "type": "response.output_item.added",
            "sequence_number": 0,
            "output_index": 0,
            "item": { "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "" },
        }),
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": { "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "hello", "namespace": "dynamic_tools" },
        }),
        json!({ "type": "response.completed", "sequence_number": 2, "response": { "id": "resp_test", "status": "completed" } }),
    ]
}

fn tool_call(output: &kiana_ai::types::AssistantMessage) -> &ToolCall {
    match &output.content[0] {
        AssistantContentBlock::ToolCall(tool_call) => tool_call,
        _ => panic!("expected toolCall block"),
    }
}

#[test]
fn omits_an_absent_error_message() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let output = runtime.block_on(run_events(function_call_events(), None));
    assert!(output.error_message.is_none());
}

#[test]
fn round_trips_a_function_namespace_received_only_on_output_item_done() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let output = runtime.block_on(run_events(function_call_events(), None));
    let call = tool_call(&output);
    assert_eq!(call.id, "call_test|fc_test");
    assert_eq!(call.name, "lookup");
    assert_eq!(call.namespace.as_deref(), Some("dynamic_tools"));

    let replayed = convert_responses_messages(
        &model(),
        &mock_http::context(vec![kiana_ai::types::Message::Assistant(output.clone())]),
        &["openai"],
        None,
    )
    .expect("convert");
    let function_call = replayed
        .iter()
        .find(|item| item["type"] == json!("function_call"))
        .expect("function_call");
    assert_eq!(function_call["id"], json!("fc_test"));
    assert_eq!(function_call["call_id"], json!("call_test"));
    assert_eq!(function_call["name"], json!("lookup"));
    assert_eq!(function_call["arguments"], json!("{\"value\":\"hello\"}"));
    assert_eq!(function_call["namespace"], json!("dynamic_tools"));
}

#[test]
fn round_trips_a_custom_tool_namespace_received_only_on_output_item_done() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let grammar: indexmap::IndexMap<String, String> = [("query".to_string(), "input".to_string())]
        .into_iter()
        .collect();
    let output = runtime.block_on(run_events(custom_tool_call_events(), Some(&grammar)));
    let call = tool_call(&output);
    assert_eq!(call.id, "call_test|ctc_test");
    assert_eq!(call.name, "query");
    assert_eq!(call.namespace.as_deref(), Some("dynamic_tools"));
    assert_eq!(
        call.arguments.get("input").and_then(Value::as_str),
        Some("hello")
    );

    let options = ConvertResponsesMessagesOptions {
        grammar_tool_input_properties: Some(grammar),
        ..Default::default()
    };
    let replayed = convert_responses_messages(
        &model(),
        &mock_http::context(vec![kiana_ai::types::Message::Assistant(output.clone())]),
        &["openai"],
        Some(&options),
    )
    .expect("convert");
    let custom = replayed
        .iter()
        .find(|item| item["type"] == json!("custom_tool_call"))
        .expect("custom_tool_call");
    assert_eq!(custom["id"], json!("ctc_test"));
    assert_eq!(custom["call_id"], json!("call_test"));
    assert_eq!(custom["name"], json!("query"));
    assert_eq!(custom["input"], json!("hello"));
    assert_eq!(custom["namespace"], json!("dynamic_tools"));
}

#[test]
fn drops_namespaces_when_the_target_cannot_replay_their_load_items() {
    let base = model();
    let output = kiana_ai::types::AssistantMessage {
        role: Default::default(),
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_function|fc_test".into(),
                name: "lookup".into(),
                arguments: [("value".to_string(), json!("hello"))]
                    .into_iter()
                    .collect(),
                namespace: Some("dynamic_tools".into()),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_custom|ctc_test".into(),
                name: "query".into(),
                arguments: [("input".to_string(), json!("hello"))]
                    .into_iter()
                    .collect(),
                namespace: Some("dynamic_tools".into()),
                ..Default::default()
            }),
        ],
        api: base.api.clone(),
        provider: base.provider.clone(),
        model: base.id.clone(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    };
    let grammar: indexmap::IndexMap<String, String> = [("query".to_string(), "input".to_string())]
        .into_iter()
        .collect();
    let options = ConvertResponsesMessagesOptions {
        grammar_tool_input_properties: Some(grammar),
        ..Default::default()
    };
    let targets = vec![
        Model {
            id: "gpt-5.2".into(),
            name: "GPT-5.2".into(),
            ..base.clone()
        },
        Model {
            provider: "azure".into(),
            ..base.clone()
        },
        Model {
            api: "openai-codex-responses".into(),
            provider: "openai-codex".into(),
            id: "gpt-5.3-codex-spark".into(),
            name: "GPT-5.3 Codex Spark".into(),
            ..base.clone()
        },
    ];
    for target in targets {
        let replayed = convert_responses_messages(
            &target,
            &mock_http::context(vec![kiana_ai::types::Message::Assistant(output.clone())]),
            &["openai"],
            Some(&options),
        )
        .expect("convert");
        let function_call = replayed
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .unwrap();
        assert!(function_call.get("namespace").is_none(), "{target:?}");
        let custom = replayed
            .iter()
            .find(|item| item["type"] == json!("custom_tool_call"))
            .unwrap();
        assert!(custom.get("namespace").is_none(), "{target:?}");
    }
}

#[test]
fn does_not_add_a_namespace_to_ordinary_function_calls() {
    let output = kiana_ai::types::AssistantMessage {
        role: Default::default(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_test|fc_test".into(),
            name: "lookup".into(),
            arguments: [("value".to_string(), json!("hello"))]
                .into_iter()
                .collect(),
            ..Default::default()
        })],
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        stop_reason: kiana_ai::types::StopReason::ToolUse,
        timestamp: 1,
        ..Default::default()
    };
    let replayed = convert_responses_messages(
        &model(),
        &mock_http::context(vec![kiana_ai::types::Message::Assistant(output)]),
        &["openai"],
        None,
    )
    .expect("convert");
    let function_call = replayed
        .iter()
        .find(|item| item["type"] == json!("function_call"))
        .unwrap();
    assert!(function_call.get("namespace").is_none());
}

#[tokio::test]
async fn streams_custom_tool_input_as_json_arguments() {
    let grammar: indexmap::IndexMap<String, String> = [("query".to_string(), "input".to_string())]
        .into_iter()
        .collect();
    let model = model();
    let mut output = mock_http::responses_output(&model);
    let stream = AssistantMessageEventStream::new();
    let options = kiana_ai::api::openai_responses_shared::OpenAIResponsesStreamOptions {
        grammar_tool_input_properties: Some(grammar),
        ..Default::default()
    };
    process_responses_stream(
        mock_http::event_stream(vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "query", "input": "" },
            }),
            json!({ "type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": "ab" }),
            json!({ "type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": "cd" }),
            json!({ "type": "response.custom_tool_call_input.done", "output_index": 0, "input": "abcd" }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": { "type": "custom_tool_call", "id": "ctc_1", "call_id": "call_1", "name": "query", "input": "abcd" },
            }),
            json!({ "type": "response.completed", "response": { "id": "resp", "status": "completed" } }),
        ]),
        &mut output,
        &stream,
        &model,
        Some(&options),
    )
    .await
    .expect("process");
    let AssistantContentBlock::ToolCall(tool_call) = &output.content[0] else {
        panic!()
    };
    assert_eq!(tool_call.id, "call_1|ctc_1");
    assert_eq!(
        tool_call.arguments.get("input").and_then(Value::as_str),
        Some("abcd")
    );
    let emitted = mock_http::drain_queued(&stream).await;
    let deltas: Vec<String> = emitted
        .iter()
        .filter_map(|event| match event {
            kiana_ai::types::AssistantMessageEvent::ToolCallDelta { delta, .. } => {
                Some(delta.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![
            "{\"input\":\"ab".to_string(),
            "cd".to_string(),
            "\"}".to_string()
        ]
    );
}
