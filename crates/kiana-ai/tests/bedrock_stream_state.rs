//! `test/bedrock-stream-state.test.ts` 的移植（内存线性用例除外：Rust 侧由
//! `ToolCallArgumentsStream` 的线性实现保证，测试环境无法测量分配量）。

use serde_json::{Value, json};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream_with_items};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, Message, Model, ModelInputModality,
    UserContent, UserMessage, UserRole,
};

const BLOCK_KEYS: &[(&str, &[&str])] = &[
    ("text", &["type", "text", "textSignature"]),
    (
        "thinking",
        &["type", "thinking", "thinkingSignature", "redacted"],
    ),
    (
        "toolCall",
        &["type", "id", "name", "arguments", "argumentsError"],
    ),
];

fn model() -> Model {
    Model {
        id: "global.anthropic.claude-test".into(),
        name: "Claude Test".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn content() -> String {
    (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\\\ done\n"))
        .collect()
}

struct WriteCall {
    args: Value,
    text: String,
}

fn write_call(size: usize) -> WriteCall {
    let content = content();
    let args =
        json!({"path": "/src/file.ts", "content": content.chars().take(size).collect::<String>()});
    WriteCall {
        text: serde_json::to_string(&args).unwrap(),
        args,
    }
}

fn events(text: &str) -> Vec<Value> {
    let mut items = vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": [1, 2]}}}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": [3]}}}}),
        json!({"contentBlockStop": {"contentBlockIndex": 0}}),
        json!({"contentBlockStart": {"contentBlockIndex": 1,
            "start": {"toolUse": {"toolUseId": "tool_1", "name": "write"}}}}),
    ];
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let end = (index + 32).min(chars.len());
        let delta: String = chars[index..end].iter().collect();
        items.push(json!({"contentBlockDelta": {"contentBlockIndex": 1,
            "delta": {"toolUse": {"input": delta}}}}));
        index = end;
    }
    items.push(json!({"contentBlockStop": {"contentBlockIndex": 1}}));
    items.push(json!({"messageStop": {"stopReason": "tool_use"}}));
    items
}

fn context() -> kiana_ai::types::TranscriptContext {
    kiana_ai::utils::transcript::normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("write".into()),
            timestamp: 0,
        })],
        ..Context::default()
    })
}

fn expect_no_stream_state(message: &kiana_ai::types::AssistantMessage) {
    for block in &message.content {
        let value = serde_json::to_value(block).unwrap();
        let kind = value["type"].as_str().unwrap();
        let allowed = BLOCK_KEYS
            .iter()
            .find(|(name, _)| *name == kind)
            .map(|(_, keys)| *keys)
            .expect("known block");
        for key in value.as_object().unwrap().keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{kind}.{key} is stream state"
            );
        }
    }
}

#[tokio::test]
async fn keeps_stream_state_off_partial_blocks_and_finishes_the_tool_call() {
    let WriteCall { args, text } = write_call(5_000);
    let model = model();
    let stream = stream_with_items(
        &model,
        &context(),
        BedrockOptions::default(),
        None,
        events(&text).into_iter().map(Ok).collect(),
    );
    let mut shown = 0usize;
    while let Some(event) = stream.recv().await {
        if matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        ) {
            continue;
        }
        let partial = event.partial().expect("partial").clone();
        expect_no_stream_state(&partial);
        let AssistantMessageEvent::ToolCallDelta {
            content_index,
            partial,
            ..
        } = event
        else {
            continue;
        };
        let Some(AssistantContentBlock::ToolCall(call)) = partial.content.get(content_index) else {
            continue;
        };
        let Some(partial_text) = call.arguments.get("content").and_then(Value::as_str) else {
            continue;
        };
        let expected_prefix: String = args["content"]
            .as_str()
            .unwrap()
            .chars()
            .take(partial_text.chars().count())
            .collect();
        assert_eq!(partial_text, expected_prefix);
        assert!(partial_text.chars().count() >= shown);
        shown = partial_text.chars().count();
    }
    let message = stream.result().await.expect("result");
    assert_eq!(
        message.stop_reason,
        kiana_ai::types::StopReason::ToolUse,
        "{:?}",
        message.error_message
    );
    expect_no_stream_state(&message);
    assert_eq!(message.content.len(), 2);
    match &message.content[0] {
        AssistantContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "[Reasoning redacted]");
            assert_eq!(thinking.thinking_signature.as_deref(), Some("AQID"));
            assert_eq!(thinking.redacted, Some(true));
        }
        other => panic!("expected thinking, got {other:?}"),
    }
    match &message.content[1] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "tool_1");
            assert_eq!(call.name, "write");
            assert_eq!(
                Value::Object(call.arguments.clone().into_iter().collect()),
                args
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}
