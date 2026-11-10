//! `core/messages.ts` 的契约测试（spec `ca-session.md` §2.1/§2.2 与 ARCHITECTURE §4.9）。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, Message, SystemContent, TextContent, TextKind, UserContent,
    UserContentBlock,
};
use kiana_session_core::messages::{
    BRANCH_SUMMARY_PREFIX, BRANCH_SUMMARY_SUFFIX, BashExecutionMessage, COMPACTION_SUMMARY_PREFIX,
    COMPACTION_SUMMARY_SUFFIX, CustomMessage, bash_execution_to_text, convert_to_llm,
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};

fn text_of_blocks(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect()
}

fn user_text(message: &Message) -> String {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => text_of_blocks(blocks),
        },
        other => panic!("expected user message, got {other:?}"),
    }
}

/// 摘要前缀/后缀是当前稳定契约，逐字不可变（注意换行的不对称）。
#[test]
fn summary_constants_match_the_typescript_templates() {
    assert_eq!(
        COMPACTION_SUMMARY_PREFIX,
        "The conversation history before this point was compacted into the following summary:\n\n<summary>\n"
    );
    assert_eq!(COMPACTION_SUMMARY_SUFFIX, "\n</summary>");
    assert_eq!(
        BRANCH_SUMMARY_PREFIX,
        "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n"
    );
    assert_eq!(BRANCH_SUMMARY_SUFFIX, "</summary>");
}

/// `createBranchSummaryMessage`：role/summary/fromId/timestamp。
#[test]
fn create_branch_summary_message_matches_the_session_shape() {
    let message =
        create_branch_summary_message("abandoned work", "entry-9", "2025-01-01T00:00:01Z");
    let value = message_json(&message.to_agent_message());
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("branchSummary")
    );
    assert_eq!(
        value.get("summary").and_then(serde_json::Value::as_str),
        Some("abandoned work")
    );
    assert_eq!(
        value.get("fromId").and_then(serde_json::Value::as_str),
        Some("entry-9")
    );
    assert_eq!(
        value.get("timestamp").and_then(serde_json::Value::as_i64),
        Some(1735689601000)
    );
    // 无效日期 → NaN → JSON null。
    let invalid = create_branch_summary_message("s", "e", "not-a-date");
    let value = message_json(&invalid.to_agent_message());
    assert_eq!(value.get("timestamp"), Some(&serde_json::Value::Null));
}

/// `createCompactionSummaryMessage`：role/summary/tokensBefore/timestamp。
#[test]
fn create_compaction_summary_message_matches_the_session_shape() {
    let message =
        create_compaction_summary_message("summary text", 12_345.0, "2025-01-01T00:00:00Z");
    let value = message_json(&message.to_agent_message());
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("compactionSummary")
    );
    assert_eq!(
        value.get("summary").and_then(serde_json::Value::as_str),
        Some("summary text")
    );
    assert_eq!(
        value
            .get("tokensBefore")
            .and_then(serde_json::Value::as_f64),
        Some(12_345.0)
    );
    assert_eq!(
        value.get("timestamp").and_then(serde_json::Value::as_i64),
        Some(1735689600000)
    );
}

/// `createCustomMessage`：role/customType/content/display/details/timestamp（details 省略）。
#[test]
fn create_custom_message_matches_the_session_shape() {
    let message = create_custom_message(
        "my-ext",
        serde_json::json!("Injected context..."),
        true,
        None,
        "2025-01-01T00:00:00Z",
    );
    let value = message_json(&message.to_agent_message());
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("custom")
    );
    assert_eq!(
        value.get("customType").and_then(serde_json::Value::as_str),
        Some("my-ext")
    );
    assert_eq!(
        value.get("content").and_then(serde_json::Value::as_str),
        Some("Injected context...")
    );
    assert_eq!(
        value.get("display").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(value.get("details").is_none());
    assert_eq!(
        value.get("timestamp").and_then(serde_json::Value::as_i64),
        Some(1735689600000)
    );
}

/// `bashExecutionToText`：命令、输出、退出码、截断与取消提示。
#[test]
fn bash_execution_to_text_covers_output_exit_code_and_truncation() {
    let mut message = BashExecutionMessage::new("ls", "", 0.0);
    assert_eq!(bash_execution_to_text(&message), "Ran `ls`\n(no output)");

    message.output = "a.txt\nb.txt".to_string();
    message.exit_code = Some(2.0);
    assert_eq!(
        bash_execution_to_text(&message),
        "Ran `ls`\n```\na.txt\nb.txt\n```\n\nCommand exited with code 2"
    );

    message.exit_code = Some(0.0);
    message.truncated = true;
    message.full_output_path = Some("/tmp/full.log".to_string());
    assert_eq!(
        bash_execution_to_text(&message),
        "Ran `ls`\n```\na.txt\nb.txt\n```\n\n[Output truncated. Full output: /tmp/full.log]"
    );

    message.cancelled = true;
    message.truncated = false;
    assert_eq!(
        bash_execution_to_text(&message),
        "Ran `ls`\n```\na.txt\nb.txt\n```\n\n(command cancelled)"
    );
}

/// `convertToLlm`：标准消息原样透传。
#[test]
fn convert_to_llm_passes_standard_messages_through() {
    let messages = vec![
        user_message("hello", 1),
        assistant_message("hi", 2),
        tool_result_message("call-1", "read", "result", 3),
        system_message("prompt", 4),
    ];
    let converted = convert_to_llm(&messages);
    assert_eq!(converted.len(), 4);
    assert_eq!(converted[0].role(), "user");
    assert_eq!(converted[1].role(), "assistant");
    assert_eq!(converted[2].role(), "toolResult");
    assert_eq!(converted[3].role(), "system");
    match &converted[3] {
        Message::System(system) => {
            assert_eq!(system.content, SystemContent::Text("prompt".to_string()))
        }
        other => panic!("expected system, got {other:?}"),
    }
}

/// `convertToLlm`：bashExecution / custom / 两种摘要都变成 user 文本。
#[test]
fn convert_to_llm_maps_custom_roles_to_user_text() {
    let mut bash = BashExecutionMessage::new("pwd", "/project", 1000.0);
    bash.exit_code = Some(0.0);
    let custom = create_custom_message(
        "ext",
        serde_json::json!([{"type": "text", "text": "custom text"}]),
        false,
        None,
        "2025-01-01T00:00:00Z",
    );
    let branch = create_branch_summary_message("branch summary", "from", "2025-01-01T00:00:00Z");
    let compaction =
        create_compaction_summary_message("compaction summary", 10.0, "2025-01-01T00:00:00Z");

    let converted = convert_to_llm(&[
        bash.to_agent_message(),
        custom.to_agent_message(),
        branch.to_agent_message(),
        compaction.to_agent_message(),
    ]);

    assert_eq!(converted.len(), 4);
    assert_eq!(user_text(&converted[0]), "Ran `pwd`\n```\n/project\n```");
    assert_eq!(user_text(&converted[1]), "custom text");
    assert_eq!(
        user_text(&converted[2]),
        format!("{BRANCH_SUMMARY_PREFIX}branch summary{BRANCH_SUMMARY_SUFFIX}")
    );
    assert_eq!(
        user_text(&converted[3]),
        format!("{COMPACTION_SUMMARY_PREFIX}compaction summary{COMPACTION_SUMMARY_SUFFIX}")
    );
    assert_eq!(converted[0].role(), "user");
}

/// `convertToLlm`：`excludeFromContext` 的 bash 消息与未知 role 都被丢弃。
#[test]
fn convert_to_llm_drops_excluded_bash_and_unknown_roles() {
    let mut bash = BashExecutionMessage::new("secret", "output", 0.0);
    bash.exclude_from_context = Some(true);
    let unknown: AgentMessage =
        serde_json::from_value(serde_json::json!({"role": "mystery", "content": "x"})).unwrap();
    let converted = convert_to_llm(&[bash.to_agent_message(), unknown, user_message("kept", 0)]);
    assert_eq!(converted.len(), 1);
    assert_eq!(user_text(&converted[0]), "kept");
}

/// `toAgentMessage` / `fromAgentMessage` 往返。
#[test]
fn custom_message_conversions_round_trip() {
    let custom = CustomMessage {
        role: kiana_session_core::messages::CustomRole::Custom,
        custom_type: "ext".to_string(),
        content: serde_json::json!("body"),
        display: true,
        details: Some(serde_json::json!({"a": 1})),
        timestamp: 42.0,
    };
    let agent = custom.to_agent_message();
    let parsed = CustomMessage::from_agent_message(&agent).expect("round trip");
    assert_eq!(parsed, custom);

    let bash = BashExecutionMessage::new("echo hi", "hi", 7.0);
    let parsed =
        BashExecutionMessage::from_agent_message(&bash.to_agent_message()).expect("round trip");
    assert_eq!(parsed, bash);

    // role 不匹配时返回 None。
    assert!(CustomMessage::from_agent_message(&user_message("hi", 0)).is_none());
}

/// 文本块构造使用 `type: "text"` 判别式（保持 `TextContent` 形状）。
#[test]
fn text_blocks_keep_the_typescript_shape() {
    let block = TextContent {
        kind_text: TextKind::Text,
        text: "x".to_string(),
        text_signature: None,
    };
    let value = serde_json::to_value(AssistantContentBlock::Text(block)).unwrap();
    assert_eq!(value, serde_json::json!({"type": "text", "text": "x"}));
}

/// 回归（评审 #9）：TS 的 `convertToLlm` 直接读取原始字段（不做类型校验）；
/// 字段类型错误（字符串 `exitCode`、`output: null`、`content: null`、未知内容块、
/// 缺失 `summary`）不会丢弃消息。
#[test]
fn convert_to_llm_keeps_messages_with_malformed_fields() {
    let bash: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "bashExecution",
        "command": "true",
        "output": null,
        "exitCode": "1",
        "cancelled": false,
        "truncated": false,
        "timestamp": 1,
    }))
    .unwrap();
    let custom_null: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "custom",
        "customType": "x",
        "content": null,
        "display": false,
        "timestamp": 2,
    }))
    .unwrap();
    let custom_unknown_blocks: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "custom",
        "content": [{"type": "text", "text": "ok"}, {"type": "weird", "x": 1}],
        "timestamp": 3,
    }))
    .unwrap();
    let summary: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "branchSummary",
        "timestamp": 4,
    }))
    .unwrap();

    let converted = convert_to_llm(&[bash, custom_null, custom_unknown_blocks, summary]);
    assert_eq!(converted.len(), 4, "no message may be dropped");
    // `output: null` → 假值 → "(no output)"；`exitCode: "1"` → 模板插值 "1"。
    assert_eq!(
        user_text(&converted[0]),
        "Ran `true`\n(no output)\n\nCommand exited with code 1"
    );
    // `content: null` → 消息保留（Rust 用空内容块表示 TS 的 null）。
    assert_eq!(user_text(&converted[1]), "");
    // 未知内容块被跳过，合法文本块保留。
    assert_eq!(user_text(&converted[2]), "ok");
    // 缺失 summary → JS 模板插值 "undefined"。
    assert_eq!(
        user_text(&converted[3]),
        format!("{BRANCH_SUMMARY_PREFIX}undefined{BRANCH_SUMMARY_SUFFIX}")
    );

    // 标准 role 的宽松视图：content: null 归一化为空内容后仍原样返回。
    let system_null: AgentMessage = serde_json::from_value(serde_json::json!({
        "role": "system",
        "content": null,
        "timestamp": 0,
    }))
    .unwrap();
    let converted = convert_to_llm(&[system_null]);
    assert_eq!(converted.len(), 1);
    assert!(matches!(
        &converted[0],
        Message::System(system) if system.content == SystemContent::Text(String::new())
    ));
}

/// 回归（评审 round 2 #3）：`convertToLlm` 对标准 role 的宽松消息（无法解析为 typed
/// `Message` 的 `AgentMessage::Custom`）必须**保留**，而不是整条丢弃。
///
/// TS 原样返回消息对象；Rust 的封闭联合无法表示的部分按可表示形态归一化
/// （缺失 timestamp → 0、字符串 assistant content → 单个 text 块、未知内容块过滤、
/// `content: null` → 空内容块）。
#[test]
fn convert_to_llm_retains_lenient_standard_role_messages() {
    let values = [
        serde_json::json!({"role": "user", "content": "hi"}),
        serde_json::json!({
            "role": "assistant", "content": "str", "api": "a", "provider": "p", "model": "m",
            "usage": {"input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "totalTokens": 10,
                      "cost": {"input": 0.1, "output": 0.2, "cacheRead": 0.3, "cacheWrite": 0.4, "total": 0.3}},
            "stopReason": "stop", "timestamp": 2,
        }),
        serde_json::json!({"role": "user", "content": [{"type": "weird", "v": 1}, {"type": "text", "text": "b"}], "timestamp": 11}),
        serde_json::json!({"role": "user", "content": 7, "timestamp": 5}),
        serde_json::json!({"role": "toolResult", "toolCallId": "c", "toolName": "t", "content": null, "isError": false, "timestamp": 3}),
        serde_json::json!({"role": "system", "content": null, "timestamp": 4}),
    ];
    let messages: Vec<AgentMessage> = values
        .iter()
        .map(|value| serde_json::from_value(value.clone()).expect("lenient message"))
        .collect();
    let converted = convert_to_llm(&messages);
    assert_eq!(
        converted.len(),
        6,
        "no standard-role message may be dropped"
    );
    let json: Vec<serde_json::Value> = converted
        .iter()
        .map(|message| serde_json::to_value(message).expect("message json"))
        .collect();

    // user 无 timestamp：TS 原样返回（没有 timestamp 键）；Rust 用 0 承载。
    assert_eq!(
        json[0],
        serde_json::json!({"role": "user", "content": "hi", "timestamp": 0})
    );
    // assistant content 字符串：TS 原样透传；Rust 映射为单个 text 块并保留其余字段。
    assert_eq!(json[1]["role"], "assistant");
    assert_eq!(
        json[1]["content"],
        serde_json::json!([{"type": "text", "text": "str"}])
    );
    assert_eq!(json[1]["api"], "a");
    assert_eq!(json[1]["provider"], "p");
    assert_eq!(json[1]["model"], "m");
    assert_eq!(json[1]["stopReason"], "stop");
    assert_eq!(json[1]["timestamp"], 2);
    // user 未知内容块：合法的 text 块保留。
    assert_eq!(
        json[2]["content"],
        serde_json::json!([{"type": "text", "text": "b"}])
    );
    assert_eq!(json[2]["timestamp"], 11);
    // user content 为非字符串/数组的值：消息保留、内容为空块。
    assert_eq!(json[3]["content"], serde_json::json!([]));
    assert_eq!(json[3]["timestamp"], 5);
    // toolResult content: null → 空内容块。
    assert_eq!(json[4]["role"], "toolResult");
    assert_eq!(json[4]["toolCallId"], "c");
    assert_eq!(json[4]["content"], serde_json::json!([]));
    // system content: null → ""。
    assert_eq!(
        json[5],
        serde_json::json!({"role": "system", "content": "", "timestamp": 4})
    );
}
