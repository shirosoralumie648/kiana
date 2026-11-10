//! 回归：真实 AWS SDK（`@smithy/core` 的 `getMessageUnmarshaller`）对 event-stream
//! 消息的解码语义。
//!
//! 旧实现把 `:message-type=exception` 当成普通事件项 yield 给状态机，于是错误消息变成
//! payload 的 JSON 字符串、诊断里也没有 errorCode/requestId。真实 SDK 抛的是服务异常
//! 实例（`.name` 为 schema 名）。

use kiana_ai::api::bedrock_converse_stream::{
    BedrockFailure, DecodedEventStreamItem, EventStreamMessage, eventstream_message_to_item,
    format_bedrock_error,
};
use serde_json::json;

fn message(headers: &[(&str, &str)], payload: &str) -> EventStreamMessage {
    EventStreamMessage {
        headers: headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        payload: payload.as_bytes().to_vec(),
    }
}

fn exception_message(exception_type: &str, payload: &str) -> EventStreamMessage {
    message(
        &[
            (":message-type", "exception"),
            (":exception-type", exception_type),
        ],
        payload,
    )
}

#[test]
fn known_event_types_are_items() {
    let decoded = eventstream_message_to_item(&message(
        &[
            (":message-type", "event"),
            (":event-type", "messageStart"),
            (":content-type", "application/json"),
        ],
        r#"{"role":"assistant"}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Item(item) => {
            assert_eq!(item, json!({"messageStart": {"role": "assistant"}}));
        }
        _ => panic!("expected item"),
    }
}

#[test]
fn unknown_event_types_are_skipped_like_the_sdk_unknown_member() {
    let decoded = eventstream_message_to_item(&message(
        &[
            (":message-type", "event"),
            (":event-type", "someFutureEvent"),
        ],
        r#"{"foo":"bar"}"#,
    ))
    .expect("decoded");
    assert!(matches!(decoded, DecodedEventStreamItem::Skip));
}

#[test]
fn modeled_exception_uses_the_pascal_case_schema_name() {
    let decoded = eventstream_message_to_item(&exception_message(
        "serviceUnavailableException",
        r#"{"message":"try later"}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Exception(failure) => {
            assert!(matches!(
                &failure,
                BedrockFailure::Service { name, .. } if name == "ServiceUnavailableException"
            ));
            assert_eq!(
                format_bedrock_error(&failure),
                "Service unavailable: try later"
            );
        }
        _ => panic!("expected modeled exception"),
    }
}

#[test]
fn modeled_exception_without_message_is_unknown() {
    let decoded = eventstream_message_to_item(&exception_message("throttlingException", "{}"))
        .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Exception(failure) => {
            assert_eq!(format_bedrock_error(&failure), "Throttling error: Unknown");
        }
        _ => panic!("expected modeled exception"),
    }
}

#[test]
fn modeled_exception_stringifies_a_numeric_message() {
    let decoded = eventstream_message_to_item(&exception_message(
        "throttlingException",
        r#"{"message":42}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Exception(failure) => {
            assert_eq!(format_bedrock_error(&failure), "Throttling error: 42");
        }
        _ => panic!("expected modeled exception"),
    }
}

#[test]
fn unknown_exception_type_throws_a_plain_error_with_the_raw_payload() {
    let decoded = eventstream_message_to_item(&exception_message(
        "someWeirdException",
        r#"{"message":"weird"}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Error(BedrockFailure::Named { name, message, .. }) => {
            assert_eq!(name, "someWeirdException");
            assert_eq!(message, r#"{"message":"weird"}"#);
        }
        _ => panic!("expected plain error"),
    }
}

#[test]
fn error_frames_use_the_error_code_and_message_headers() {
    let decoded = eventstream_message_to_item(&message(
        &[
            (":message-type", "error"),
            (":error-code", "internalServerException"),
            (":error-message", "boom"),
        ],
        "{}",
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Error(BedrockFailure::Named { name, message, .. }) => {
            assert_eq!(name, "internalServerException");
            assert_eq!(message, "boom");
        }
        _ => panic!("expected plain error"),
    }
}

#[test]
fn error_frames_without_an_error_message_fall_back_to_unknown_error() {
    let decoded = eventstream_message_to_item(&message(
        &[
            (":message-type", "error"),
            (":error-code", "internalServerException"),
        ],
        "",
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Error(BedrockFailure::Named { message, .. }) => {
            assert_eq!(message, "UnknownError");
        }
        _ => panic!("expected plain error"),
    }
}

#[test]
fn invalid_exception_payload_reports_the_v8_syntax_error_text() {
    let decoded = eventstream_message_to_item(&exception_message("throttlingException", "{oops"))
        .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Error(BedrockFailure::Named { name, message, .. }) => {
            assert_eq!(name, "SyntaxError");
            assert_eq!(
                message,
                "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
            );
        }
        _ => panic!("expected SyntaxError"),
    }
}

#[test]
fn lone_surrogate_escapes_are_accepted_like_json_parse() {
    // serde_json 会拒绝 `\ud800`（lone leading surrogate in hex escape）；JSON.parse 接受它。
    let decoded = eventstream_message_to_item(&exception_message(
        "throttlingException",
        r#"{"message":"\ud800"}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Exception(failure) => {
            assert_eq!(format_bedrock_error(&failure), "Throttling error: \u{fffd}");
        }
        _ => panic!("expected modeled exception"),
    }
}

#[test]
fn out_of_range_numbers_are_accepted_like_json_parse() {
    let decoded = eventstream_message_to_item(&exception_message(
        "throttlingException",
        r#"{"message":1e400}"#,
    ))
    .expect("decoded");
    match decoded {
        DecodedEventStreamItem::Exception(failure) => {
            // kiana_json 把 Infinity 落成 null（ARCHITECTURE §8 第 32 条），String(null) = "null"。
            assert_eq!(format_bedrock_error(&failure), "Throttling error: null");
        }
        _ => panic!("expected modeled exception"),
    }
}

#[test]
fn invalid_event_payload_is_a_syntax_error_too() {
    let decoded = eventstream_message_to_item(&message(
        &[
            (":message-type", "event"),
            (":event-type", "contentBlockDelta"),
        ],
        "{oops",
    ))
    .expect("decoded");
    assert!(matches!(
        decoded,
        DecodedEventStreamItem::Error(BedrockFailure::Named { ref name, .. }) if name == "SyntaxError"
    ));
}
