//! `packages/ai/test/message-types.test.ts` 的 Rust 移植。
//!
//! TS 的这条测试主要是**类型级**断言：
//!
//! - `ToolResultMessage<Details>` 的 `details` 可以是任意 JSON 兼容值（对象 / 数组 / 只读元组 /
//!   字符串 / `null`），且 `Message`（联合类型）接受其中任意一种；
//! - `ToolResultMessage<OptionalAny>` 等含有 `any`/`undefined`/函数/`Date`/`unknown` 的类型参数
//!   **不是** `ToolResultMessage`（`toEqualTypeOf<never>()`）。
//!
//! Rust 侧 `ToolResultMessage::details` 的类型是 `Option<serde_json::Value>`（`types.rs` 已冻结），
//! 因此“JSON 兼容值”集合天然成立；“非 JSON 类型不可表达”由类型系统保证（函数 / `Date` 无法变成
//! `Value`）。下面把每条**运行期**断言逐条移植，并把类型级断言写成等价的编译期/运行期检查。

mod common;

use common::messages::{minimal_tool_result, tool_result_base};
use kiana_ai::types::{Message, ToolResultMessage, Usage};
use serde_json::{Value, json};

/// 把 `test/message-types.test.ts` 的 `{...base, details}` 构造成 JSON。
fn with_details(details: Value) -> Value {
    let mut base = tool_result_base();
    base.insert("details".into(), details);
    Value::Object(base)
}

/// TS: "preserves JSON-compatible detail types without requiring index signatures"。
#[test]
fn preserves_json_compatible_detail_types_without_requiring_index_signatures() {
    // `details: { path: "a.ts", items: ["first"], summary: { lines: 3 } }`。
    let object_message = with_details(json!({
        "path": "a.ts",
        "items": ["first"],
        "summary": { "lines": 3 },
    }));
    // `details: ["a", "b"]`。
    let array_message = with_details(json!(["a", "b"]));
    // `details: ["a", { count: 1 }]`（只读元组在运行期就是数组）。
    let readonly_array_message = with_details(json!(["a", { "count": 1 }]));
    // `details: "diagnostic"`。
    let primitive_message = with_details(json!("diagnostic"));
    // `details: null`（TS 的 `ToolResultMessage<null>`；注意 `details?: T`，显式 null 与原样保留）。
    let null_message = with_details(Value::Null);

    // `expect(objectMessage.details?.summary?.lines).toBe(3)`。
    assert_eq!(object_message["details"]["summary"]["lines"], json!(3));
    // `expect(arrayMessage.details).toEqual(["a", "b"])`。
    assert_eq!(array_message["details"], json!(["a", "b"]));
    // `expect(readonlyArrayMessage.details).toEqual(["a", { count: 1 }])`。
    assert_eq!(
        readonly_array_message["details"],
        json!(["a", { "count": 1 }])
    );
    // `expect(primitiveMessage.details).toBe("diagnostic")`。
    assert_eq!(primitive_message["details"], json!("diagnostic"));
    // `expect(nullMessage.details).toBeNull()`。
    assert!(null_message["details"].is_null());

    // `const transcriptMessage: Message = objectMessage` —— 联合类型接受它（Rust：`Message` 的
    // `ToolResult` 变体持有同一个 `ToolResultMessage`）。
    let tool_result: ToolResultMessage =
        serde_json::from_value(object_message.clone()).expect("tool result deserializes");
    let transcript = Message::ToolResult(tool_result.clone());
    assert_eq!(transcript.role(), "toolResult");
    let readonly_transcript = Message::ToolResult(
        serde_json::from_value(readonly_array_message.clone())
            .expect("readonly tuple deserializes"),
    );
    assert_eq!(readonly_transcript.role(), "toolResult");

    // `const context: Context = { messages: [objectMessage, readonlyArrayMessage] }`。
    let context_messages = [object_message, readonly_array_message];
    assert_eq!(context_messages.len(), 2);
    assert_eq!(context_messages[0]["details"]["items"], json!(["first"]));
    assert_eq!(context_messages[1]["details"], json!(["a", { "count": 1 }]));
}

/// TS: "rejects non-JSON detail types"。
///
/// 类型级断言：`ToolResultMessage<OptionalAny>` / `<undefined[]>` / `<{callback}>` / `<{value:
/// unknown}>` / `<{value: Date}>` 都 `toEqualTypeOf<never>()`。Rust 侧 `details` 是
/// `Option<serde_json::Value>`，上述类型根本无法构造；能编译就说明它们不会进入 `Message`。
/// 这里验证**边界**：只有 JSON 值可以放进 `details`，且 `Option` 的 `None` 对应 TS 的
/// `details?:`（省略字段）。
#[test]
fn rejects_non_json_detail_types() {
    let with_none = ToolResultMessage {
        details: None,
        usage: Option::<Usage>::None,
        ..serde_json::from_value::<ToolResultMessage>(minimal_tool_result())
            .expect("base deserializes")
    };
    // `details?: T` 省略 → JSON 里没有该键（`skip_serializing_if = "Option::is_none"`）。
    let serialized = serde_json::to_value(&with_none).expect("tool result serializes");
    assert!(
        serialized.get("details").is_none(),
        "omitted details is absent"
    );

    // `details: undefined` 在 TS 里同样等于省略；Rust 用 `None` 表达。
    // 非 JSON 值（函数、Date、unknown）在 Rust 侧没有对应类型，无法构造 —— 即类型级 `never`。
    let all_json_values = [
        json!(null),
        json!(true),
        json!(1),
        json!("x"),
        json!([]),
        json!({}),
    ];
    for value in all_json_values {
        let message = ToolResultMessage {
            details: Some(value.clone()),
            ..with_none.clone()
        };
        assert_eq!(
            serde_json::to_value(&message).expect("serializes")["details"],
            value,
            "details round-trips {value}"
        );
    }
}

/// 补充：`Message` 的 JSON 形状是稳定契约（同序同键，按 JS `JSON.stringify` 语义）。
///
/// 这条测试锁定 `types.rs` 的 `Message` 必须是 `#[serde(untagged)]`：internally tagged
/// (`tag = "role"`) 形式对“newtype 变体 + 自带 `role` 字段”会输出两份 `role` 键、且无法反序列化。
#[test]
fn message_json_matches_the_typescript_union_shape() {
    let golden = [
        r#"{"role":"system","content":"base","timestamp":1}"#,
        r#"{"role":"user","content":"hi","timestamp":2}"#,
        r#"{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":3}"#,
        r#"{"role":"assistant","content":[],"api":"openai-responses","provider":"openai","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":4}"#,
        r#"{"role":"toolResult","toolCallId":"c","toolName":"read","content":[],"isError":false,"timestamp":5}"#,
    ];
    for expected in golden {
        let message: Message = serde_json::from_str(expected).unwrap_or_else(|error| {
            panic!("{expected} parses: {error}");
        });
        // 序列化逐字节一致（键序 == 结构体字段声明序）。浮点必须先过 `kiana_json` 的 JS 数字格式化
        // （ARCHITECTURE §4.3 第 4 条）：`Usage.cost` 的 `0.0` 在 JS 里写成 `0`。
        let value = serde_json::to_value(&message).expect("serializes");
        assert_eq!(kiana_json::js_object::stringify(&value), expected);
        // 再解析一次（往返稳定）。
        let reparsed: Message = serde_json::from_str(expected).expect("round-trips");
        assert_eq!(reparsed, message);
    }
}

/// 回归（review finding 14）：显式 `details: null` 与省略 `details` 在 JSON 里必须不同，
/// 往返后仍是 `null`（TS `message-types.test.ts` 的 `expect(details).toBeNull()`）。
#[test]
fn explicit_null_details_round_trip_as_null() {
    let explicit_null = with_details(Value::Null);
    let message: ToolResultMessage =
        serde_json::from_value(explicit_null.clone()).expect("details: null parses");
    assert_eq!(message.details, Some(Value::Null), "null is not absence");
    let serialized = serde_json::to_value(&message).expect("serializes");
    assert!(
        serialized.get("details").is_some_and(Value::is_null),
        "details stays null: {serialized}"
    );
    // 再往返一次仍稳定（不会被当成缺省丢掉）。
    let reparsed: ToolResultMessage =
        serde_json::from_value(serialized.clone()).expect("round-trips");
    assert_eq!(reparsed.details, Some(Value::Null));
    // 键序 == TS 接口声明序（`details` 在 `content` 之后、`isError` 之前）。
    // 注意：TS 测试里的 `{...base, details}` 会把键追加到最后，那只是构造方式；
    // `ToolResultMessage` 的序列化形状由接口声明序决定（ARCHITECTURE §4.3 第 3 条）。
    let expected = json!({
        "role": "toolResult",
        "toolCallId": "call-1",
        "toolName": "read",
        "content": [],
        "details": null,
        "isError": false,
        "timestamp": 1,
    });
    assert_eq!(
        kiana_json::js_object::stringify(&serialized),
        kiana_json::js_object::stringify(&expected)
    );
}
