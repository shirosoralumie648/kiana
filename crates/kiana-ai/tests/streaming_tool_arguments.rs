//! `packages/ai/test/streaming-tool-arguments.test.ts` 的 Rust 移植（行为部分）。
//!
//! 每条 provider 路径（anthropic-messages / openai-completions / openai-responses /
//! mistral-conversations / kiana-messages / faux）搭一条 SSE 假流，断言：
//! 1. 流式参数只增不减、最终参数与完整 JSON 完全一致，且流内部状态（raw 文本快照）不会泄漏到
//!    partial 消息里；
//! 2. 流在错误中断时同样不泄漏状态。
//!
//! 内存量级与「丢弃后释放」的用例（TS 的 `allocatedBytes` / `WeakRef` + GC）在
//! `streaming_tool_arguments_memory.rs`：Rust 没有 V8 堆统计/强制 GC，改用测试专用全局
//! 分配器统计（见 `tests/common/memory.rs`）。

#[path = "support/streaming_tool_arguments.rs"]
mod harness;

use harness::{
    Path, anthropic_body, base_model, context, deltas_of, mock_http, stream_options, write_call,
};
use kiana_ai::api::anthropic_messages::AnthropicOptions;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{AssistantContentBlock, AssistantMessage, AssistantMessageEvent, StopReason};
use serde_json::{Value, json};

fn event_partial(event: &AssistantMessageEvent) -> Option<&AssistantMessage> {
    match event {
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolCallStart { partial, .. }
        | AssistantMessageEvent::ToolCallDelta { partial, .. }
        | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
        AssistantMessageEvent::Done { message, .. } => Some(message),
        AssistantMessageEvent::Error { error, .. } => Some(error),
    }
}

/// 内容块只允许携带自有字段；其它键意味着流内部状态泄漏进 partial 消息。
fn expect_no_stream_state(message: &AssistantMessage, label: &str) {
    for block in &message.content {
        let value = serde_json::to_value(block).expect("serialize block");
        let object = value.as_object().expect("block object");
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let allowed: &[&str] = match kind {
            "text" => &["type", "text", "textSignature"],
            "thinking" => &["type", "thinking", "thinkingSignature", "redacted"],
            "toolCall" => &[
                "type",
                "id",
                "name",
                "arguments",
                "argumentsError",
                "thoughtSignature",
                "namespace",
            ],
            other => panic!("{label}: unexpected block type {other}"),
        };
        for key in object.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{label}: {kind}.{key} leaked stream state"
            );
        }
    }
}

async fn consume(
    stream: &AssistantMessageEventStream,
    mut on_event: impl FnMut(&AssistantMessageEvent),
) -> AssistantMessage {
    while let Some(event) = stream.recv().await {
        on_event(&event);
    }
    stream.result().await.expect("result")
}

async fn check_growing_arguments(path: Path) {
    let (args, text) = write_call(5_000);
    let deltas = deltas_of(&text, 24);
    let stream = path.stream(&deltas, &args, &text);
    let mut shown = 0usize;
    let mut partials = 0usize;
    let message = consume(&stream, |event| {
        let partial = event_partial(event).expect("partial");
        expect_no_stream_state(partial, event.kind());
        let AssistantMessageEvent::ToolCallDelta { content_index, .. } = event else {
            return;
        };
        let Some(AssistantContentBlock::ToolCall(call)) = partial.content.get(*content_index)
        else {
            return;
        };
        let Some(shown_content) = call.arguments.get("content").and_then(Value::as_str) else {
            return;
        };
        partials += 1;
        let expected = args["content"].as_str().unwrap_or_default();
        assert!(
            expected.starts_with(shown_content),
            "{}: shown content is not a prefix",
            path.name()
        );
        assert!(
            shown_content.len() >= shown,
            "{}: shown content shrank",
            path.name()
        );
        shown = shown_content.len();
    })
    .await;

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(partials > 100, "{}: only {partials} deltas", path.name());
    assert_eq!(message.content.len(), 1);
    let AssistantContentBlock::ToolCall(call) = &message.content[0] else {
        panic!("{}: expected a tool call", path.name());
    };
    assert!(!call.id.is_empty());
    assert_eq!(call.name, "write");
    assert_eq!(call.arguments, args, "{}: exact arguments", path.name());
}

#[tokio::test]
async fn anthropic_messages_shows_growing_partial_arguments() {
    check_growing_arguments(Path::Anthropic).await;
}

#[tokio::test]
async fn openai_completions_shows_growing_partial_arguments() {
    check_growing_arguments(Path::Completions).await;
}

#[tokio::test]
async fn openai_responses_shows_growing_partial_arguments() {
    check_growing_arguments(Path::Responses).await;
}

#[tokio::test]
async fn mistral_conversations_shows_growing_partial_arguments() {
    check_growing_arguments(Path::Mistral).await;
}

#[tokio::test]
async fn kiana_messages_shows_growing_partial_arguments() {
    check_growing_arguments(Path::KianaMessages).await;
}

#[tokio::test]
async fn faux_shows_growing_partial_arguments() {
    check_growing_arguments(Path::Faux).await;
}

#[tokio::test]
async fn leaves_no_stream_state_on_the_error_message_of_a_stream_cut_off_mid_call() {
    let (_, text) = write_call(2_000);
    let cut: Vec<String> = deltas_of(&text, 16).into_iter().take(20).collect();
    // 丢掉 content_block_stop / message_delta / message_stop，模拟连接中断。
    let body = {
        let mut events_json = anthropic_body(&cut);
        let marker = "event: content_block_stop";
        if let Some(index) = events_json.find(marker) {
            events_json.truncate(index);
        }
        events_json
    };
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse_raw(body));
    let model = base_model(
        "claude-test",
        "c",
        "anthropic-messages",
        "anthropic",
        "https://api.anthropic.com",
    );
    let stream = kiana_ai::api::anthropic_messages::stream(
        &model,
        &context(),
        AnthropicOptions {
            base: stream_options(&mock, "sk-ant-test"),
            ..Default::default()
        },
    );
    let message = consume(&stream, |event| {
        if let Some(partial) = event_partial(event) {
            expect_no_stream_state(partial, event.kind());
        }
    })
    .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    expect_no_stream_state(&message, "error");
    let AssistantContentBlock::ToolCall(call) = &message.content[0] else {
        panic!("expected a tool call");
    };
    assert_eq!(call.arguments["path"], json!("/src/file.ts"));
}
