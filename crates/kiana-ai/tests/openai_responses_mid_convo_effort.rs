//! `openai-responses-mid-convo-effort.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{
    AssistantMessage, Message, Model, StreamOptions, ThinkingLevel, ToolResultMessage,
};
use serde_json::{Value, json};

fn thinking_level_map() -> indexmap::IndexMap<kiana_ai::types::ModelThinkingLevel, Option<String>> {
    serde_json::from_value(json!({
        "off": "none",
        "minimal": null,
        "low": "low",
        "medium": "medium",
        "high": "high",
        "xhigh": "xhigh",
        "max": "max",
    }))
    .unwrap()
}

fn openai_model(compat: Value) -> Model {
    Model {
        id: "gpt-6-luna".into(),
        name: "GPT-6 Luna".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: Some(true),
        thinking_level_map: Some(thinking_level_map()),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(272_000),
        max_tokens: Some(128_000),
        compat: Some(compat),
        ..Model::default()
    }
}

fn user(text: &str) -> Message {
    mock_http::user(text)
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: call_id.into(),
        tool_name: "lookup".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: "result".into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    })
}

fn update(effort: &str) -> Value {
    json!({ "type": "configuration_update", "reasoning": { "effort": effort } })
}

fn response_events(response_id: &str, with_tool_call: bool) -> Vec<Value> {
    let item = if with_tool_call {
        json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": "{}", "status": "completed" })
    } else {
        json!({
            "type": "message",
            "id": format!("msg_{response_id}"),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": "answer", "annotations": [] }],
        })
    };
    vec![
        json!({ "type": "response.created", "response": { "id": response_id } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": item }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        json!({ "type": "response.completed", "response": { "id": response_id, "status": "completed" } }),
    ]
}

async fn run_openai(
    messages: Vec<Message>,
    reasoning_effort: Option<ThinkingLevel>,
    api_key: &str,
    model: Model,
    with_tool_call: bool,
) -> (Value, AssistantMessage) {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&response_events(
        "resp_1",
        with_tool_call,
    )));
    let options = OpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some(api_key.into()),
            fetch: Some(Arc::new(mock.clone())),
            ..Default::default()
        },
        reasoning_effort,
        ..Default::default()
    };
    let message = mock_http::run_responses(&model, &mock_http::context(messages), options).await;
    assert_ne!(
        message.stop_reason,
        kiana_ai::types::StopReason::Error,
        "{:?}",
        message.error_message
    );
    (mock.last_body(), message)
}

#[tokio::test]
async fn keeps_reasoning_effort_constant_and_only_appends_input_when_the_level_changes() {
    let model = openai_model(json!({ "supportsMidConvoEffort": true }));
    let mut history = vec![user("one")];
    let (first_payload, first_message) = run_openai(
        history.clone(),
        Some(ThinkingLevel::High),
        "sk-test",
        model.clone(),
        false,
    )
    .await;
    history.push(Message::Assistant(first_message.clone()));
    history.push(user("two"));
    let (second_payload, second_message) = run_openai(
        history.clone(),
        Some(ThinkingLevel::High),
        "sk-test",
        model.clone(),
        false,
    )
    .await;
    history.push(Message::Assistant(second_message.clone()));
    history.push(user("three"));
    let (third_payload, third_message) = run_openai(
        history.clone(),
        Some(ThinkingLevel::Low),
        "sk-test",
        model.clone(),
        false,
    )
    .await;

    assert_eq!(
        first_payload["input"],
        json!([{ "role": "user", "content": [{ "type": "input_text", "text": "one" }] }, update("high")])
    );
    for payload in [&first_payload, &second_payload, &third_payload] {
        assert_eq!(payload["reasoning"]["effort"], json!("medium"));
    }
    let first_input = first_payload["input"].as_array().unwrap();
    let second_input = second_payload["input"].as_array().unwrap();
    let third_input = third_payload["input"].as_array().unwrap();
    assert_eq!(&second_input[..first_input.len()], first_input.as_slice());
    assert_eq!(&third_input[..second_input.len()], second_input.as_slice());
    let updates: Vec<&Value> = third_input
        .iter()
        .filter(|item| item["type"] == json!("configuration_update"))
        .collect();
    assert_eq!(
        updates,
        vec![&update("high"), &update("high"), &update("low")]
    );
    assert_eq!(third_input.last(), Some(&update("low")));
    assert_eq!(
        [first_message, second_message, third_message]
            .iter()
            .map(|message| message.provider_thinking_level.clone())
            .collect::<Vec<_>>(),
        vec![Some("high".into()), Some("high".into()), Some("low".into())]
    );
}

#[tokio::test]
async fn applies_a_level_change_made_during_a_tool_loop_to_the_next_reply() {
    let model = openai_model(json!({ "supportsMidConvoEffort": true }));
    let mut history = vec![user("one")];
    let (tool_payload, tool_message) = run_openai(
        history.clone(),
        Some(ThinkingLevel::High),
        "sk-test",
        model.clone(),
        true,
    )
    .await;
    history.push(Message::Assistant(tool_message));
    history.push(tool_result("call_1|fc_1"));
    let (after_tool_payload, after_tool_message) =
        run_openai(history, Some(ThinkingLevel::Low), "sk-test", model, false).await;

    let tool_input = tool_payload["input"].as_array().unwrap();
    let after_input = after_tool_payload["input"].as_array().unwrap();
    assert_eq!(&after_input[..tool_input.len()], tool_input.as_slice());
    let tail = &after_input[after_input.len() - 2..];
    assert_eq!(tail[0]["type"], json!("function_call_output"));
    assert_eq!(tail[0]["call_id"], json!("call_1"));
    assert_eq!(tail[1], update("low"));
    assert_eq!(
        after_tool_message.provider_thinking_level.as_deref(),
        Some("low")
    );
}

#[tokio::test]
async fn adds_items_only_before_replies_recorded_by_this_model() {
    let model = openai_model(json!({ "supportsMidConvoEffort": true }));
    let unstamped = run_openai(
        vec![user("one")],
        Some(ThinkingLevel::High),
        "sk-test",
        openai_model(json!({})),
        false,
    )
    .await
    .1;
    let other_provider = AssistantMessage {
        provider: "opencode".into(),
        provider_thinking_level: Some("high".into()),
        ..unstamped.clone()
    };
    let other_model = AssistantMessage {
        model: "gpt-6-sol".into(),
        provider_thinking_level: Some("high".into()),
        ..unstamped.clone()
    };
    let recorded = AssistantMessage {
        provider_thinking_level: Some("high".into()),
        ..unstamped.clone()
    };

    let messages = vec![
        user("one"),
        Message::Assistant(unstamped),
        user("two"),
        Message::Assistant(other_provider),
        user("three"),
        Message::Assistant(other_model),
        user("four"),
        Message::Assistant(recorded),
        user("five"),
    ];
    let (payload, _) =
        run_openai(messages, Some(ThinkingLevel::Low), "sk-test", model, false).await;
    let kinds: Vec<&str> = payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .or_else(|| item.get("role").and_then(Value::as_str))
                .unwrap()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "message",
            "user",
            "message",
            "user",
            "message",
            "user",
            "configuration_update",
            "message",
            "user",
            "configuration_update"
        ]
    );
    let updates: Vec<&Value> = payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == json!("configuration_update"))
        .collect();
    assert_eq!(updates, vec![&update("high"), &update("low")]);
}

#[tokio::test]
async fn sends_the_level_as_reasoning_effort_without_mid_convo_updates_when_disabled() {
    let cases: Vec<(&str, &str, Model, Option<ThinkingLevel>, &str)> = vec![
        (
            "Sign in with ChatGPT",
            "chatgpt-access-token",
            openai_model(json!({ "supportsMidConvoEffort": true })),
            Some(ThinkingLevel::Low),
            "low",
        ),
        (
            "the compat flag turned off",
            "sk-test",
            openai_model(json!({ "supportsMidConvoEffort": false })),
            Some(ThinkingLevel::Low),
            "low",
        ),
        (
            "thinking off",
            "sk-test",
            openai_model(json!({ "supportsMidConvoEffort": true })),
            None,
            "none",
        ),
    ];
    for (name, api_key, model, level, effort) in cases {
        let (_, first_message) = run_openai(
            vec![user("one")],
            Some(ThinkingLevel::High),
            api_key,
            model.clone(),
            false,
        )
        .await;
        let (payload, message) = run_openai(
            vec![user("one"), Message::Assistant(first_message), user("two")],
            level,
            api_key,
            model,
            false,
        )
        .await;
        assert_eq!(payload["reasoning"]["effort"], json!(effort), "{name}");
        assert!(
            payload["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != json!("configuration_update")),
            "{name}"
        );
        assert_eq!(message.provider_thinking_level, None, "{name}");
    }
}
