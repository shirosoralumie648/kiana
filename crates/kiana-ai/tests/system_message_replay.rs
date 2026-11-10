//! `packages/ai/test/system-message-replay.test.ts`。
//!
//! TS 的 `tool(name, description = \`${name} tool\`)` 用 `Type.Object({})` 作参数 schema；
//! Rust 侧直接给同一个 JSON 形状（`{"type":"object","properties":{}}`）。

use indexmap::IndexMap;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, SystemContent,
    SystemMessage, SystemRole, TextContent, TextKind, Tool, ToolReference, TranscriptContext,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::text::{get_system_message_text, render_system_message_update};
#[allow(deprecated)] // TS 仍导出 `hasToolRedefinitions`；本文件测试它的现行语义。
use kiana_ai::utils::transcript::has_tool_redefinitions;
use kiana_ai::utils::transcript::{
    collapse_system_messages, declarations_equal, get_current_system_message,
    get_current_system_prompt, get_tool_state_changes, has_non_additive_tool_changes,
    normalize_context,
};
use serde_json::json;

fn tool(name: &str) -> Tool {
    tool_with_description(name, &format!("{name} tool"))
}

fn tool_with_description(name: &str, description: &str) -> Tool {
    Tool {
        name: name.to_string(),
        description: description.to_string(),
        parameters: json!({"type": "object", "properties": {}}),
        constrained_sampling: None,
    }
}

fn system_message(
    content: &str,
    sections: Option<&[(&str, Option<&str>)]>,
    tools_added: Option<Vec<Tool>>,
    tools_removed: Option<Vec<ToolReference>>,
    timestamp: i64,
) -> Message {
    Message::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(content.to_string()),
        sections: sections.map(|sections| {
            sections
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.map(str::to_string)))
                .collect::<IndexMap<_, _>>()
        }),
        tools_added,
        tools_removed,
        timestamp,
    })
}

fn user_message(content: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.to_string()),
        timestamp,
    })
}

fn assistant_message(text: &str, timestamp: i64) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        timestamp,
        ..Default::default()
    })
}

fn transcript() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            system_message(
                "base",
                Some(&[("a", Some("<a>1</a>")), ("b", Some("<b>1</b>"))]),
                Some(vec![tool("first")]),
                None,
                10,
            ),
            user_message("hello", 11),
            system_message("also do this", None, None, None, 12),
            assistant_message("ok", 13),
            system_message(
                "",
                Some(&[
                    ("a", Some("<a>2</a>")),
                    ("b", None),
                    ("c", Some("<c>1</c>")),
                ]),
                Some(vec![tool("second")]),
                Some(vec![ToolReference {
                    name: "first".to_string(),
                }]),
                14,
            ),
        ],
        tools: None,
    })
}

// describe("system message replay")

#[test]
fn replays_content_sections_and_tools_into_one_leading_message() {
    let transcript = transcript();
    let current = get_current_system_message(transcript.as_context().messages.as_slice())
        .expect("system message");
    let mut expected_sections = IndexMap::new();
    expected_sections.insert("a".to_string(), Some("<a>2</a>".to_string()));
    expected_sections.insert("c".to_string(), Some("<c>1</c>".to_string()));
    assert_eq!(
        Message::System(current),
        system_message(
            "base\n\nalso do this",
            Some(&[("a", Some("<a>2</a>")), ("c", Some("<c>1</c>"))]),
            Some(vec![tool("second")]),
            None,
            10,
        )
    );
    assert_eq!(
        get_current_system_prompt(transcript.as_context().messages.as_slice()),
        "base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
    );
}

#[test]
fn collapse_keeps_only_non_system_messages_after_the_replayed_head() {
    let transcript = transcript();
    let collapsed = collapse_system_messages(&transcript);
    let roles: Vec<&str> = collapsed
        .as_context()
        .messages
        .iter()
        .map(Message::role)
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant"]);
    assert_eq!(collapse_system_messages(&collapsed), collapsed);
}

#[test]
fn replay_of_a_transcript_without_system_messages_is_empty() {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![user_message("hi", 1)],
        tools: None,
    });
    assert_eq!(
        get_current_system_message(context.as_context().messages.as_slice()),
        None
    );
    assert_eq!(
        get_current_system_prompt(context.as_context().messages.as_slice()),
        ""
    );
    assert_eq!(
        collapse_system_messages(&context).as_context().messages,
        context.as_context().messages
    );
}

#[test]
fn a_late_full_patch_on_a_transcript_without_a_leading_message_replays_as_the_prompt() {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            user_message("old session", 1),
            system_message(
                "",
                Some(&[("preamble", Some("You are kiana."))]),
                Some(vec![tool("x")]),
                None,
                2,
            ),
        ],
        tools: None,
    });
    assert_eq!(
        get_current_system_prompt(context.as_context().messages.as_slice()),
        "You are kiana."
    );
    let collapsed = collapse_system_messages(&context);
    match &collapsed.as_context().messages[0] {
        Message::System(message) => {
            assert_eq!(message.role, SystemRole::System);
            assert_eq!(message.tools_added, Some(vec![tool("x")]));
        }
        other => panic!("expected a system message, got {other:?}"),
    }
}

#[test]
fn renders_complete_prompts_and_framed_updates() {
    let transcript = transcript();
    let leading = transcript.as_context().messages[0].clone();
    let update = transcript.as_context().messages[4].clone();
    let (Message::System(leading), Message::System(update)) = (leading, update) else {
        panic!("expected system messages");
    };
    assert_eq!(
        get_system_message_text(&leading),
        "base\n\n<a>1</a>\n\n<b>1</b>"
    );
    assert_eq!(
        render_system_message_update(&update),
        [
            "Updated system prompt section \"a\":\n\n<a>2</a>",
            "Removed system prompt section \"b\".",
            "Updated system prompt section \"c\":\n\n<c>1</c>",
        ]
        .join("\n\n")
    );
}

#[test]
fn normalizes_the_legacy_prompt_and_tool_fields_into_a_leading_system_message() {
    let messages = vec![user_message("hi", 1)];
    // 无 prompt、无工具：上下文原样返回。
    let context = Context {
        system_prompt: None,
        messages: messages.clone(),
        tools: None,
    };
    assert_eq!(normalize_context(&context).as_context().messages, messages);
    // 空字符串 prompt + 空工具数组：等价于都没有。
    let context = Context {
        system_prompt: Some(String::new()),
        messages: messages.clone(),
        tools: Some(Vec::new()),
    };
    assert_eq!(normalize_context(&context).as_context().messages, messages);
    // 两者都有：插到最前面，timestamp 为 0。
    let context = Context {
        system_prompt: Some("be brief".into()),
        messages: messages.clone(),
        tools: Some(vec![tool("a")]),
    };
    let mut expected = vec![system_message(
        "be brief",
        None,
        Some(vec![tool("a")]),
        None,
        0,
    )];
    expected.extend(messages);
    assert_eq!(normalize_context(&context).as_context().messages, expected);
}

#[test]
fn compares_tool_declarations_without_executable_or_undefined_fields() {
    // TS 的 `constrainedSampling: undefined` 与 `execute` 函数在序列化后都不出现；
    // Rust 侧对应 `None`。
    let mut executable = tool("a");
    executable.constrained_sampling = None;
    assert!(declarations_equal(&executable, &tool("a")));
    assert!(!declarations_equal(
        &tool("a"),
        &tool_with_description("a", "changed")
    ));
    let mut constrained = tool("a");
    constrained.constrained_sampling = Some(kiana_ai::types::ConstrainedSampling::Disabled(false));
    assert!(!declarations_equal(&tool("a"), &constrained));
}

#[test]
fn tool_state_changes_treat_changed_definitions_as_removal_plus_addition() {
    let changes = get_tool_state_changes(
        &[tool("a"), tool("b")],
        &[tool_with_description("b", "changed"), tool("c")],
    );
    assert_eq!(
        changes,
        kiana_ai::utils::transcript::ToolStateChanges {
            tools_added: vec![tool_with_description("b", "changed"), tool("c")],
            tools_removed: vec![
                ToolReference {
                    name: "a".to_string()
                },
                ToolReference {
                    name: "b".to_string()
                },
            ],
        }
    );
    assert_eq!(
        get_tool_state_changes(&[tool("a")], &[tool("a")]),
        kiana_ai::utils::transcript::ToolStateChanges::default()
    );
}

#[test]
#[allow(deprecated)] // `hasToolRedefinitions` 在 TS 里仍被导出，这里验证它的现行语义。
fn detects_non_additive_tool_history_and_redefinitions() {
    let transcript = transcript();
    assert!(has_non_additive_tool_changes(
        transcript.as_context().messages.as_slice()
    ));
    assert!(!has_tool_redefinitions(
        transcript.as_context().messages.as_slice()
    ));

    let additive = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            system_message("", None, Some(vec![tool("a")]), None, 1),
            system_message("", None, Some(vec![tool("b")]), None, 2),
        ],
        tools: None,
    });
    assert!(!has_non_additive_tool_changes(
        additive.as_context().messages.as_slice()
    ));

    let redeclared = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            system_message("", None, Some(vec![tool("a")]), None, 1),
            system_message(
                "",
                None,
                Some(vec![tool_with_description("a", "changed")]),
                None,
                2,
            ),
        ],
        tools: None,
    });
    assert!(has_non_additive_tool_changes(
        redeclared.as_context().messages.as_slice()
    ));
    assert!(has_tool_redefinitions(
        redeclared.as_context().messages.as_slice()
    ));
}
