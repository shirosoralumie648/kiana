//! `modes/json-event.ts` 的移植测试：消息投影、usage 保留、toolcall_start 元数据与 M3 键序修复。
//!
//! - 回归套件 #7290 / #7911 / #7925 的标题逐字保留。
//! - `tests/fixtures/json_event/cases.json` 由 Node v26.12.1 对同一批事件调用 `toJsonEvent`
//!   生成（TS 运行时键序）；Rust 侧反序列化事件后重新投影并逐字节比较。
//! - `tests/fixtures/json_event/models.json` 由 Node v26.12.1 的
//!   `providers/all.ts#getBuiltin*Model` + `JSON.stringify` 生成（模型对象键序）。

use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, Message,
    StopReason, TextContent, TextKind, ToolCall, ToolCallKind, Usage, UsageCost,
};
use kiana_rpc_print::json_event::{
    WireError, reorder_wire_keys, serialize_event_line, serialize_wire_line, to_json_event,
    to_wire_value,
};
use kiana_session::AgentSessionEvent;
use serde_json::{Value, json};

const EVENT_FIXTURE: &str = include_str!("fixtures/json_event/cases.json");
const MODEL_FIXTURE: &str = include_str!("fixtures/json_event/models.json");
const RUNTIME_ORDER_FIXTURE: &str = include_str!("fixtures/json_event/runtime_order_cases.json");

fn usage() -> Usage {
    Usage {
        input: 12,
        output: 34,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: Some(5),
        reasoning: Some(3),
        total_tokens: 46,
        cost: UsageCost {
            input: 0.003,
            output: 0.3,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.303,
            total_from_provider: Some(true),
        },
    }
}

fn assistant_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux".to_string(),
        usage: usage(),
        stop_reason: StopReason::Stop,
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }
}

fn assistant_event(
    assistant_message_event: AssistantMessageEvent,
    usage: Usage,
) -> AgentSessionEvent {
    let message = AssistantMessage {
        usage,
        ..assistant_message("")
    };
    AgentSessionEvent::MessageUpdate {
        message: AgentMessage::Standard(Message::Assistant(message)),
        assistant_message_event,
    }
}

/// 黄金数据：投影结果（含 done/error 的嵌套消息与 message_start/end/entry_appended 的键序）
/// 与黄金数据逐字节一致。
#[test]
fn golden_wire_events_match_typescript() {
    let fixture: Value = serde_json::from_str(EVENT_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["projection"].as_array().expect("projection") {
        let name = case["name"].as_str().expect("name");
        let event: AgentSessionEvent = serde_json::from_value(case["event"].clone())
            .unwrap_or_else(|error| {
                panic!("{name}: fixture event must deserialize: {error}");
            });
        let expected = case["line"].as_str().expect("line");
        match serialize_event_line(&event) {
            Ok(line) if line == expected => {}
            Ok(line) => failures.push(format!("{name}:\n  got:  {line:?}\n  want: {expected:?}")),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// describe: "regression #7290: JSON event streams stay linear"
///
/// TS: "emits delta-only message updates whose size scales linearly"
#[test]
fn emits_delta_only_message_updates_whose_size_scales_linear() {
    fn measure_update_bytes(text: &str) -> usize {
        let message = assistant_message("");
        let partial = assistant_message(text);
        let chunk_len = text.len() / 8;
        let mut total = 0;
        for index in 0..8 {
            let start = index * chunk_len;
            let end = if index == 7 {
                text.len()
            } else {
                start + chunk_len
            };
            let event = AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Standard(Message::Assistant(message.clone())),
                assistant_message_event: AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: text[start..end].to_string(),
                    partial: Box::new(partial.clone()),
                },
            };
            let projected = to_json_event(&event).expect("project");
            assert!(projected.get("message").is_none());
            assert!(projected["assistantMessageEvent"].get("partial").is_none());
            total += serialize_event_line(&event).expect("serialize").len();
        }
        total
    }

    let small_bytes = measure_update_bytes(&"x".repeat(2_000));
    let large_bytes = measure_update_bytes(&"x".repeat(4_000));

    assert!(large_bytes > small_bytes);
    assert!(
        (large_bytes as f64) / (small_bytes as f64) < 2.2,
        "ratio {} (small {small_bytes}, large {large_bytes})",
        (large_bytes as f64) / (small_bytes as f64)
    );
}

/// describe: "regression #7911: JSON message updates retain usage"
///
/// TS: "includes cumulative usage without cumulative message snapshots"
#[test]
fn includes_cumulative_usage_without_cumulative_message_snapshots() {
    let populated = Usage {
        input: 7,
        output: 11,
        total_tokens: 18,
        ..Usage::default()
    };
    let event = assistant_event(
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hello".to_string(),
            partial: Box::new(assistant_message("hello")),
        },
        populated.clone(),
    );

    let wire_update = to_json_event(&event).expect("project");
    assert_eq!(
        wire_update["usage"],
        serde_json::to_value(&populated).unwrap()
    );
    assert!(wire_update.get("message").is_none());
    assert!(
        wire_update["assistantMessageEvent"]
            .get("partial")
            .is_none()
    );
}

/// describe: "regression #7925: tool-call metadata is available when streaming starts"
///
/// TS: "includes the tool call id and name without cumulative snapshots"
#[test]
fn includes_the_tool_call_id_and_name_without_cumulative_snapshots() {
    let mut partial = assistant_message("");
    partial.content = vec![AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "call_7925".to_string(),
        name: "write".to_string(),
        arguments: [
            ("path".to_string(), json!("output.txt")),
            ("content".to_string(), json!("x".repeat(100))),
        ]
        .into_iter()
        .collect(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })];
    let event = assistant_event(
        AssistantMessageEvent::ToolCallStart {
            content_index: 0,
            partial: Box::new(partial),
        },
        usage(),
    );

    assert_eq!(
        to_json_event(&event).expect("project"),
        json!({
            "type": "message_update",
            "usage": serde_json::to_value(usage()).unwrap(),
            "assistantMessageEvent": {
                "type": "toolcall_start",
                "contentIndex": 0,
                "id": "call_7925",
                "toolName": "write",
            },
        })
    );

    // 键序：type、contentIndex、id、toolName（partial 被删除，metadata 追加在后）。
    let line = serialize_event_line(&event).expect("serialize");
    assert!(
        line.ends_with(
            "\"assistantMessageEvent\":{\"type\":\"toolcall_start\",\"contentIndex\":0,\"id\":\"call_7925\",\"toolName\":\"write\"}}\n"
        ),
        "unexpected line: {line}"
    );
}

/// 模型对象键序：Rust `Model` 结构体序必须被还原成 TS 运行时（= 模型目录 JSON）的键序。
///
/// 黄金数据由 Node v26.12.1 的 `providers/all.ts#getBuiltin*Model` +
/// `JSON.stringify` 生成（`get_state` / `set_model` / `cycle_model` /
/// `get_available_models` 的 `data` 就是这些运行时对象）。
#[test]
fn model_objects_keep_the_typescript_catalog_key_order() {
    let fixture: Value = serde_json::from_str(MODEL_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let provider = case["provider"].as_str().expect("provider");
        let id = case["id"].as_str().expect("id");
        let raw = match case["kind"].as_str().expect("kind") {
            "image" => kiana_ai::providers::catalog::get_image_model(provider, id),
            "classifier" => kiana_ai::providers::catalog::get_classifier_model(provider, id),
            _ => kiana_ai::providers::catalog::get_model(provider, id),
        }
        .unwrap_or_else(|| panic!("missing catalog entry {provider}/{id}"));
        let model: kiana_ai::types::Model =
            serde_json::from_value(raw.clone()).expect("catalog model deserializes");
        let expected = case["line"].as_str().expect("line");
        match serialize_wire_line(&model) {
            Ok(line) if line == expected => {}
            Ok(line) => failures.push(format!(
                "{provider}/{id}:\n  got:  {line:?}\n  want: {expected:?}"
            )),
            Err(error) => failures.push(format!("{provider}/{id}: {error}")),
        }
        // 嵌套（`get_state` 的 `data.model`）同样生效。
        let wrapped = json!({ "model": model });
        let expected_wrapped = case["wrapped"].as_str().expect("wrapped");
        match serialize_wire_line(&wrapped) {
            Ok(line) if line == expected_wrapped => {}
            Ok(line) => failures.push(format!(
                "{provider}/{id} (nested):\n  got:  {line:?}\n  want: {expected_wrapped:?}"
            )),
            Err(error) => failures.push(format!("{provider}/{id} (nested): {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 回归：工具结果消息、assistant 运行时字段、usage 与 faux 消息的 TS 运行时键序。
///
/// 黄金数据由 Node v26.12.1 按 TS 源码的构造/赋值顺序生成
/// （`agent-loop.ts#createToolResultMessage`、`anthropic-messages.ts` 的运行时赋值、
/// `event-stream.ts#time`、`agent-loop.ts:419`、`providers/faux.ts`）。
/// `message_start`/`message_end`/`turn_end`/`agent_end` 都必须逐字节一致。
#[test]
fn wire_objects_keep_the_typescript_runtime_key_order() {
    let fixture: Value = serde_json::from_str(RUNTIME_ORDER_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["events"].as_array().expect("events") {
        let name = case["name"].as_str().expect("name");
        let event: AgentSessionEvent = serde_json::from_value(case["event"].clone())
            .unwrap_or_else(|error| panic!("{name}: fixture event must deserialize: {error}"));
        // 黄金数据的消息必须反序列化成标准消息；如果退化成 `AgentMessage::Custom`，
        // wire 序列化会原样透传 fixture 键序，测试就失去意义。
        assert!(
            !matches!(
                &event,
                AgentSessionEvent::MessageStart {
                    message: AgentMessage::Custom(_),
                    ..
                } | AgentSessionEvent::MessageEnd {
                    message: AgentMessage::Custom(_),
                    ..
                } | AgentSessionEvent::MessageUpdate {
                    message: AgentMessage::Custom(_),
                    ..
                } | AgentSessionEvent::TurnEnd {
                    message: AgentMessage::Custom(_),
                    ..
                }
            ),
            "{name}: fixture message must deserialize as a standard message"
        );
        if let AgentSessionEvent::AgentEnd { messages, .. } = &event {
            assert!(
                messages
                    .iter()
                    .all(|message| matches!(message, AgentMessage::Standard(_))),
                "{name}: agent_end messages must deserialize as standard messages"
            );
        }
        let expected = case["line"].as_str().expect("line");
        match serialize_event_line(&event) {
            Ok(line) if line == expected => {}
            Ok(line) => failures.push(format!("{name}:\n  got:  {line:?}\n  want: {expected:?}")),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 回归：models.json 自定义模型（`modelFromJson`）的键序，包括替换内置 id 的覆盖条目。
///
/// 黄金数据由 Node v26.12.1 按 `provider-composer.ts#modelFromJson` 的字面量顺序生成；
/// 顶层与嵌套载荷（`get_state` 的 `data.model`）都必须一致。
#[test]
fn custom_models_keep_the_model_from_json_key_order() {
    let fixture: Value = serde_json::from_str(RUNTIME_ORDER_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["models"].as_array().expect("models") {
        let name = case["name"].as_str().expect("name");
        let model: kiana_ai::types::Model = serde_json::from_value(case["model"].clone())
            .unwrap_or_else(|error| panic!("{name}: fixture model must deserialize: {error}"));
        let expected = case["line"].as_str().expect("line");
        match serialize_wire_line(&model) {
            Ok(line) if line == expected => {}
            Ok(line) => failures.push(format!("{name}:\n  got:  {line:?}\n  want: {expected:?}")),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
        let wrapped = json!({ "type": "response", "success": true, "data": { "model": model } });
        let expected_wrapped = format!(
            "{{\"type\":\"response\",\"success\":true,\"data\":{{\"model\":{}}}}}\n",
            expected.trim_end()
        );
        match serialize_wire_line(&wrapped) {
            Ok(line) if line == expected_wrapped => {}
            Ok(line) => failures.push(format!(
                "{name} (nested):\n  got:  {line:?}\n  want: {expected_wrapped:?}"
            )),
            Err(error) => failures.push(format!("{name} (nested): {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 两条错误文本逐字对齐 TS。
#[test]
fn wire_error_messages_are_verbatim() {
    assert_eq!(
        WireError::NotAssistantMessage.to_string(),
        "message_update message is not an assistant message"
    );
    assert_eq!(
        WireError::ToolCallStartNotToolCall { index: 2 }.to_string(),
        "toolcall_start content at index 2 is not a tool call"
    );

    let user = AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
        role: kiana_ai::types::UserRole::User,
        content: kiana_ai::types::UserContent::Text("hi".to_string()),
        timestamp: 0,
    }));
    let event = AgentSessionEvent::MessageUpdate {
        message: user,
        assistant_message_event: AssistantMessageEvent::Start {
            partial: Box::new(assistant_message("")),
        },
    };
    let error = to_json_event(&event).expect_err("non-assistant must fail");
    assert_eq!(
        error.to_string(),
        "message_update message is not an assistant message"
    );

    let event = assistant_event(
        AssistantMessageEvent::ToolCallStart {
            content_index: 4,
            partial: Box::new(assistant_message("")),
        },
        usage(),
    );
    let error = to_json_event(&event).expect_err("missing tool call must fail");
    assert_eq!(
        error.to_string(),
        "toolcall_start content at index 4 is not a tool call"
    );
}

/// M3 §7.3 键序：system 消息的 toolsAdded/toolsRemoved 位于 timestamp 之后；
/// assistant 消息的 thinkingLevel 位于最末。守卫键缺失时不重排。
#[test]
fn reorder_moves_tool_changes_and_thinking_level_to_their_runtime_positions() {
    let mut value = json!({
        "role": "system",
        "content": "base",
        "toolsAdded": [{ "name": "bash" }],
        "toolsRemoved": [{ "name": "legacy" }],
        "timestamp": 1
    });
    reorder_wire_keys(&mut value);
    assert_eq!(
        kiana_json::js_object::stringify(&value),
        "{\"role\":\"system\",\"content\":\"base\",\"timestamp\":1,\"toolsAdded\":[{\"name\":\"bash\"}],\"toolsRemoved\":[{\"name\":\"legacy\"}]}"
    );

    let mut value = json!({
        "role": "assistant",
        "content": [],
        "thinkingLevel": "high",
        "usage": {},
        "stopReason": "stop",
        "timestamp": 1
    });
    reorder_wire_keys(&mut value);
    assert_eq!(
        kiana_json::js_object::stringify(&value),
        "{\"role\":\"assistant\",\"content\":[],\"usage\":{},\"stopReason\":\"stop\",\"timestamp\":1,\"thinkingLevel\":\"high\"}"
    );

    // 守卫：没有 timestamp 的 system 对象、没有 stopReason 的 assistant 对象都保持原序。
    let mut guarded = json!({ "role": "system", "toolsAdded": [], "content": "x" });
    reorder_wire_keys(&mut guarded);
    assert_eq!(
        kiana_json::js_object::stringify(&guarded),
        "{\"role\":\"system\",\"toolsAdded\":[],\"content\":\"x\"}"
    );
    let mut guarded = json!({ "role": "assistant", "thinkingLevel": "high", "usage": {} });
    reorder_wire_keys(&mut guarded);
    assert_eq!(
        kiana_json::js_object::stringify(&guarded),
        "{\"role\":\"assistant\",\"thinkingLevel\":\"high\",\"usage\":{}}"
    );

    // 递归：数组里的嵌套对象同样重排。
    let mut nested =
        json!([{ "role": "assistant", "thinkingLevel": "low", "stopReason": "stop", "x": 1 }]);
    reorder_wire_keys(&mut nested);
    assert_eq!(
        kiana_json::js_object::stringify(&nested),
        "[{\"role\":\"assistant\",\"stopReason\":\"stop\",\"x\":1,\"thinkingLevel\":\"low\"}]"
    );
}

/// `to_wire_value` / `serialize_wire_line` 对所有 `Serialize` 记录生效。
#[test]
fn wire_value_applies_the_reorder_pass() {
    let value = json!({
        "role": "assistant",
        "thinkingLevel": "max",
        "stopReason": "stop",
        "usage": {}
    });
    let reordered = to_wire_value(&value).expect("to wire value");
    assert_eq!(
        kiana_json::js_object::stringify(&reordered),
        "{\"role\":\"assistant\",\"stopReason\":\"stop\",\"usage\":{},\"thinkingLevel\":\"max\"}"
    );
}

/// 目录级性质测试：每个能反序列化成 Rust `Model` 的目录条目（~1715 条），
/// `serialize_wire_line` 必须逐字节等于目录 JSON（= TS 运行时对象的 `JSON.stringify`），
/// 包括嵌套的 `cost` / `inputLimits` 键序，以及 serde 丢弃后由 wire 层补回的
/// `lab`/`enabled`/`providers`/`free`。
#[test]
fn catalog_models_serialize_in_catalog_order_with_all_keys() {
    use kiana_ai::providers::catalog;

    let mut checked = 0usize;
    let mut check = |raw: &Value| {
        let Ok(model) = serde_json::from_value::<kiana_ai::types::Model>(raw.clone()) else {
            return;
        };
        let mut value = serde_json::to_value(&model).expect("model serializes");
        reorder_wire_keys(&mut value);
        let Some(got) = value.as_object() else {
            return;
        };
        assert!(raw.is_object(), "catalog entries are objects");
        assert_eq!(
            kiana_json::js_object::stringify(&value),
            kiana_json::js_object::stringify(raw),
            "catalog key order/content mismatch for {}/{}",
            got.get("provider").and_then(Value::as_str).unwrap_or("?"),
            got.get("id").and_then(Value::as_str).unwrap_or("?")
        );
        checked += 1;
    };
    for provider in catalog::provider_ids() {
        for raw in catalog::get_models(provider) {
            check(raw);
        }
        for raw in catalog::get_image_models(provider) {
            check(raw);
        }
        for raw in catalog::get_classifier_models(provider) {
            check(raw);
        }
    }
    assert!(checked > 1_500, "checked only {checked} catalog entries");
}

/// 回归（review finding 3）：radius 目录模型带 `Model` 结构体之外的键
/// （`lab`/`enabled`/`providers`，`chat:recursive/flint` 还有 `free`）。
/// TS 的 `get_state` / `set_model` / `cycle_model` / `get_available_models` 直接返回
/// 目录对象，`JSON.stringify` 保留这些键；wire 层必须按目录顺序补回。
#[test]
fn radius_catalog_models_keep_model_external_keys() {
    use kiana_ai::providers::catalog;

    let mut checked = 0usize;
    for raw in catalog::get_models("radius") {
        let model: kiana_ai::types::Model =
            serde_json::from_value(raw.clone()).expect("radius model deserializes");
        let value = to_wire_value(&model).expect("model serializes");
        assert_eq!(
            kiana_json::js_object::stringify(&value),
            kiana_json::js_object::stringify(raw),
            "radius model {} lost catalog-only keys or order",
            raw.get("id").and_then(Value::as_str).unwrap_or("?")
        );
        checked += 1;
    }
    assert_eq!(checked, 32, "radius ships 32 chat models");
    let flint: kiana_ai::types::Model = serde_json::from_value(
        catalog::get_model("radius", "recursive/flint")
            .unwrap()
            .clone(),
    )
    .expect("flint deserializes");
    let value = to_wire_value(&flint).expect("flint serializes");
    assert_eq!(value.get("free"), Some(&Value::Bool(true)));
    assert!(value.get("lab").is_some());
    assert!(value.get("enabled").is_some());
    assert!(value.get("providers").is_some());
}
