//! 守卫函数与 `ExtensionEventKind` 的行为契约。
//!
//! 覆盖 `types.ts` 的 8 个 `is*ToolResult`、`isToolCallEventType`、41 个事件名的
//! `as_str`/`FromStr`/`Display`，以及 `before_agent_start`、`tool_call`、`tool_result`
//! 三个嵌套联合的按字符串分派（未知 `messageType`/`toolName` 的行为）。

use std::collections::HashSet;
use std::str::FromStr;

use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventKind, ParseExtensionEventKindError, ToolCallEvent,
    ToolResultEvent, is_bash_tool_result, is_edit_tool_result, is_find_tool_result,
    is_grep_tool_result, is_ls_tool_result, is_power_shell_tool_result, is_read_tool_result,
    is_tool_call_event_type, is_write_tool_result,
};
use serde_json::json;

const TOOL_NAMES: [&str; 9] = [
    "bash",
    "powershell",
    "read",
    "edit",
    "write",
    "grep",
    "find",
    "ls",
    "custom_tool",
];

fn tool_call(tool_name: &str) -> ToolCallEvent {
    let input = match tool_name {
        "bash" | "powershell" => json!({ "command": "ls" }),
        "read" => json!({ "path": "/f" }),
        "edit" => json!({ "path": "/f", "edits": [] }),
        "write" => json!({ "path": "/f", "content": "c" }),
        "grep" => json!({ "pattern": "p" }),
        "find" => json!({ "pattern": "*.rs" }),
        "ls" => json!({}),
        _ => json!({ "action": "do" }),
    };
    serde_json::from_value(json!({
        "type": "tool_call",
        "toolCallId": "c1",
        "toolName": tool_name,
        "input": input,
    }))
    .expect("tool_call 夹具必须可反序列化")
}

fn tool_result(tool_name: &str) -> ToolResultEvent {
    let mut value = json!({
        "type": "tool_result",
        "toolCallId": "c1",
        "toolName": tool_name,
        "input": {},
        "content": [],
        "isError": false,
    });
    if tool_name == "custom_tool" {
        // 自定义结果的 details 是必填（`unknown`，可为 null）。
        value["details"] = json!(null);
    }
    serde_json::from_value(value).expect("tool_result 夹具必须可反序列化")
}

fn normalized_options() -> serde_json::Value {
    json!({
        "selectedTools": [],
        "hiddenTools": [],
        "toolSnippets": {},
        "toolGuidelines": {},
        "promptGuidelines": [],
        "appendSystemPrompt": "",
        "sections": {},
        "cwd": "",
        "contextFiles": [],
        "skills": [],
    })
}

fn before_agent_start(message_type: &str) -> ExtensionEvent {
    let mut value = json!({
        "type": "before_agent_start",
        "systemPrompt": "sp",
        "systemPromptOptions": normalized_options(),
        "messageType": message_type,
    });
    if message_type == "user" {
        value["prompt"] = json!("hi");
    } else {
        value["message"] = json!({
            "role": "custom",
            "customType": "t",
            "content": "hello",
            "display": true,
            "timestamp": 1,
        });
    }
    serde_json::from_value(value).expect("before_agent_start 夹具必须可反序列化")
}

#[test]
fn result_guards_match_exactly_their_own_tool() {
    type Guard = fn(&ToolResultEvent) -> bool;
    let guards: [(&str, Guard); 8] = [
        ("bash", is_bash_tool_result),
        ("powershell", is_power_shell_tool_result),
        ("read", is_read_tool_result),
        ("edit", is_edit_tool_result),
        ("write", is_write_tool_result),
        ("grep", is_grep_tool_result),
        ("find", is_find_tool_result),
        ("ls", is_ls_tool_result),
    ];

    for (guard_name, guard) in guards {
        for tool_name in TOOL_NAMES {
            let event = tool_result(tool_name);
            assert_eq!(
                guard(&event),
                guard_name == tool_name,
                "守卫 {guard_name} 对 toolName={tool_name} 的判定错误"
            );
        }
    }
}

#[test]
fn custom_result_with_builtin_name_is_matched_by_string() {
    // 自定义事件的 toolName 也可以是 "bash"：守卫按字符串比较，与 TS 的开放联合一致。
    let event: ToolResultEvent = serde_json::from_value(json!({
        "type": "tool_result",
        "toolCallId": "c1",
        "toolName": "bash",
        "input": {},
        "content": [],
        "isError": false,
        "details": null,
    }))
    .unwrap();
    // 已知名字会分派到内建变体（而不是 Custom）。
    assert!(matches!(event, ToolResultEvent::Bash(_)));
    assert!(is_bash_tool_result(&event));
    assert!(!is_read_tool_result(&event));
}

#[test]
fn tool_call_event_type_compares_tool_name_string() {
    for own in TOOL_NAMES {
        let event = tool_call(own);
        for other in TOOL_NAMES {
            assert_eq!(
                is_tool_call_event_type(other, &event),
                own == other,
                "is_tool_call_event_type({other}) 对 toolName={own} 的判定错误"
            );
        }
        assert_eq!(event.tool_name(), own);
        assert_eq!(event.kind(), ExtensionEventKind::ToolCall);
    }
}

#[test]
fn tool_call_dispatch_selects_variant() {
    assert!(matches!(tool_call("bash"), ToolCallEvent::Bash(_)));
    assert!(matches!(
        tool_call("powershell"),
        ToolCallEvent::PowerShell(_)
    ));
    assert!(matches!(tool_call("read"), ToolCallEvent::Read(_)));
    assert!(matches!(tool_call("edit"), ToolCallEvent::Edit(_)));
    assert!(matches!(tool_call("write"), ToolCallEvent::Write(_)));
    assert!(matches!(tool_call("grep"), ToolCallEvent::Grep(_)));
    assert!(matches!(tool_call("find"), ToolCallEvent::Find(_)));
    assert!(matches!(tool_call("ls"), ToolCallEvent::Ls(_)));
    match tool_call("custom_tool") {
        ToolCallEvent::Custom(event) => {
            assert_eq!(event.tool_name, "custom_tool");
            assert_eq!(event.input["action"], json!("do"));
        }
        other => panic!("未知 toolName 必须落到 Custom，实际 {other:?}"),
    }
}

#[test]
fn tool_result_dispatch_selects_variant() {
    assert!(matches!(tool_result("bash"), ToolResultEvent::Bash(_)));
    assert!(matches!(
        tool_result("powershell"),
        ToolResultEvent::PowerShell(_)
    ));
    assert!(matches!(tool_result("read"), ToolResultEvent::Read(_)));
    assert!(matches!(tool_result("edit"), ToolResultEvent::Edit(_)));
    assert!(matches!(tool_result("write"), ToolResultEvent::Write(_)));
    assert!(matches!(tool_result("grep"), ToolResultEvent::Grep(_)));
    assert!(matches!(tool_result("find"), ToolResultEvent::Find(_)));
    assert!(matches!(tool_result("ls"), ToolResultEvent::Ls(_)));
    assert!(matches!(
        tool_result("custom_tool"),
        ToolResultEvent::Custom(_)
    ));
    assert_eq!(tool_result("ls").kind(), ExtensionEventKind::ToolResult);
}

#[test]
fn event_kind_all_names_are_unique_and_round_trip() {
    assert_eq!(ExtensionEventKind::ALL.len(), 41);
    let unique: HashSet<&'static str> = ExtensionEventKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(unique.len(), 41, "41 个事件名不得重复");

    for kind in ExtensionEventKind::ALL {
        let name = kind.as_str();
        assert_eq!(kind.to_string(), name, "Display 必须等于 as_str");
        let parsed = ExtensionEventKind::from_str(name).expect("本 crate 自己的名字必须能解析");
        assert_eq!(parsed, kind, "as_str → FromStr 必须回到同一变体");
        assert_eq!(parsed.as_str(), name);
    }
}

/// `types.ts#ExtensionAPI.on` 的 41 个重载按源码顺序逐字排列。
///
/// `event_kind_all_names_are_unique_and_round_trip` 只保证自洽；本测试把名字集合与**顺序**
/// 钉到 TS 源码，防止 `as_str`/`FromStr` 同时被改成另一个（同样自洽的）名字。
#[test]
fn event_kind_all_matches_ts_on_overload_order() {
    let expected = [
        "project_trust",
        "resources_discover",
        "session_start",
        "session_info_changed",
        "session_before_switch",
        "session_before_fork",
        "session_before_compact",
        "session_compact",
        "session_compact_failed",
        "session_shutdown",
        "mcp_servers_change",
        "session_before_tree",
        "session_tree",
        "context",
        "context_with_system",
        "cache_warming_decision",
        "before_provider_request",
        "before_provider_headers",
        "after_provider_response",
        "provider_stream_event",
        "before_agent_start",
        "agent_start",
        "agent_end",
        "agent_before_settle",
        "agent_settled",
        "ui_prompt_start",
        "ui_prompt_end",
        "turn_start",
        "turn_end",
        "message_start",
        "message_update",
        "message_end",
        "tool_execution_start",
        "tool_execution_update",
        "tool_execution_end",
        "model_select",
        "thinking_level_select",
        "tool_call",
        "tool_result",
        "user_bash",
        "input",
    ];
    let actual: Vec<&str> = ExtensionEventKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn event_kind_parse_unknown_reports_exact_message() {
    let error = ExtensionEventKind::from_str("nope").expect_err("未知名字必须报错");
    assert_eq!(error, ParseExtensionEventKindError("nope".to_string()));
    assert_eq!(error.to_string(), "unknown extension event kind: nope");
}

#[test]
fn before_agent_start_dispatches_on_message_type() {
    match before_agent_start("user") {
        ExtensionEvent::BeforeAgentStart(kiana_ext_api::events::BeforeAgentStartEvent::User(
            event,
        )) => {
            assert_eq!(event.prompt, "hi");
        }
        other => panic!("messageType=user 必须分派到 User，实际 {other:?}"),
    }
    match before_agent_start("custom") {
        ExtensionEvent::BeforeAgentStart(kiana_ext_api::events::BeforeAgentStartEvent::Custom(
            event,
        )) => {
            assert_eq!(event.message.timestamp, 1.0);
        }
        other => panic!("messageType=custom 必须分派到 Custom，实际 {other:?}"),
    }
    assert_eq!(
        before_agent_start("user").kind(),
        ExtensionEventKind::BeforeAgentStart
    );
}

#[test]
fn extension_event_kind_matches_serialized_type() {
    // 从 JSON 构造的简单事件：kind().as_str() 必须等于线上的 type 字面量。
    let cases = [
        (json!({"type":"context","messages":[]}), "context"),
        (
            json!({"type":"input","text":"hi","source":"interactive"}),
            "input",
        ),
        (json!({"type":"agent_start"}), "agent_start"),
        (json!({"type":"agent_end","messages":[]}), "agent_end"),
        (
            json!({"type":"agent_settled","aborted":false}),
            "agent_settled",
        ),
        (
            json!({"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{}}),
            "tool_execution_start",
        ),
        (
            json!({"type":"tool_execution_update","toolCallId":"c1","toolName":"bash","args":{},"partialResult":{}}),
            "tool_execution_update",
        ),
        (
            json!({"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{},"isError":false}),
            "tool_execution_end",
        ),
        (
            json!({"type":"session_start","reason":"startup"}),
            "session_start",
        ),
        (
            json!({"type":"session_info_changed","name":"n"}),
            "session_info_changed",
        ),
        (
            json!({"type":"user_bash","command":"ls","excludeFromContext":false,"cwd":"/w"}),
            "user_bash",
        ),
        (
            json!({"type":"ui_prompt_start","reason":"ui_prompt","kind":"select"}),
            "ui_prompt_start",
        ),
    ];
    for (value, expected) in cases {
        let event: ExtensionEvent = serde_json::from_value(value.clone()).unwrap_or_else(|error| {
            panic!("夹具必须可反序列化: {error}; {value}");
        });
        assert_eq!(event.kind().as_str(), expected);
        assert_eq!(
            serde_json::to_value(&event).unwrap()["type"],
            json!(expected)
        );
    }

    // tool_call / tool_result / before_agent_start 的 kind 也走同一枚举。
    assert_eq!(
        ExtensionEvent::ToolCall(tool_call("custom_tool")).kind(),
        ExtensionEventKind::ToolCall
    );
    assert_eq!(
        ExtensionEvent::ToolResult(tool_result("bash")).kind(),
        ExtensionEventKind::ToolResult
    );
}

#[test]
fn before_agent_start_unknown_message_type_is_error() {
    let mut value = json!({
        "type": "before_agent_start",
        "systemPrompt": "sp",
        "systemPromptOptions": normalized_options(),
        "messageType": "nope",
    });
    value["prompt"] = json!("hi");
    let error =
        serde_json::from_value::<ExtensionEvent>(value).expect_err("未知 messageType 必须报错");
    assert!(error.to_string().contains("messageType"), "{error}");
}
