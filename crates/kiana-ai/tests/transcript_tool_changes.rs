//! `packages/ai/test/transcript-tool-changes.test.ts`。
//!
//! TS 的这个文件混合了两类用例：
//! 1. 纯函数 `resolveTranscriptTools(messages, support)`（本单元 `utils/transcript.rs`）——
//!    逐条移植并实际运行；
//! 2. 通过 `streamSimple(...)` + `onPayload` 捕获真实请求 payload 的用例——依赖的
//!    provider 与 api 单元均已合入，这里直接真实运行（不再 `#[ignore]`）。

use std::sync::{Arc, Mutex};

use kiana_ai::api::{anthropic_messages, openai_completions, openai_responses};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, Model,
    ModelInputModality, SimpleStreamOptions, StopReason, StreamOptions, SystemContent,
    SystemMessage, TextContent, Tool, Usage, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::{
    TranscriptToolSupport, TranscriptTools, normalize_context, resolve_transcript_tools,
};
use serde_json::{Value, json};

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

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

fn system(
    tools_added: Option<Vec<Tool>>,
    tools_removed: Option<Vec<&str>>,
    timestamp: i64,
) -> Message {
    Message::System(SystemMessage {
        role: Default::default(),
        content: kiana_ai::types::SystemContent::Text(String::new()),
        sections: None,
        tools_added,
        tools_removed: tools_removed.map(|names| {
            names
                .into_iter()
                .map(|name| kiana_ai::types::ToolReference {
                    name: name.to_string(),
                })
                .collect()
        }),
        timestamp,
    })
}

fn expected(request_tools: Vec<Tool>, folded_system_messages: usize) -> TranscriptTools {
    TranscriptTools {
        request_tools,
        folded_system_messages,
    }
}

// #10940: removals and redeclarations move the top-level boundary once; plain additions stay anchored.

#[test]
fn resolves_the_top_level_tool_boundary_per_transport() {
    let messages = vec![
        system(Some(vec![tool("base_tool"), tool("drop_tool")]), None, 0),
        system(None, Some(vec!["drop_tool"]), 1),
        system(Some(vec![tool("drop_tool")]), None, 2),
        system(Some(vec![tool_with_description("late_tool", "x")]), None, 3),
        system(Some(vec![tool_with_description("late_tool", "y")]), None, 4),
    ];

    assert_eq!(
        resolve_transcript_tools(&messages[..2], TranscriptToolSupport::Additions),
        expected(vec![tool("base_tool")], 2)
    );
    // Kimi K3 ignores a removed tool that a later tool message declares again, so the re-added
    // drop_tool moves the boundary; the new late_tool after it stays anchored.
    assert_eq!(
        resolve_transcript_tools(&messages[..4], TranscriptToolSupport::Additions),
        expected(vec![tool("base_tool"), tool("drop_tool")], 3)
    );
    // The redefinition of late_tool moves the boundary to its message.
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::Additions),
        expected(
            vec![
                tool("base_tool"),
                tool("drop_tool"),
                tool_with_description("late_tool", "y")
            ],
            5
        )
    );
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::Changes),
        expected(Vec::new(), 0)
    );
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::None),
        expected(
            vec![
                tool("base_tool"),
                tool("drop_tool"),
                tool_with_description("late_tool", "y")
            ],
            5
        )
    );
}

// ---------------------------------------------------------------------------
// 通过各适配器 + `onPayload` 捕获真实请求 payload 的用例（TS 的 `capturePayload`）。
// ---------------------------------------------------------------------------

fn base_model(id: &str, name: &str, api: &str, provider: &str) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: api.into(),
        provider: provider.into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(100_000),
        max_tokens: Some(1000),
        ..Default::default()
    }
}

fn anthropic_native_model() -> Model {
    Model {
        compat: Some(json!({
            "supportsMidConvoSystemMessages": true,
            "supportsMidConvoToolChanges": true,
        })),
        ..base_model(
            "claude-opus-5",
            "Claude Opus 5",
            "anthropic-messages",
            "anthropic",
        )
    }
}

fn additional_tools_model() -> Model {
    Model {
        compat: Some(json!({
            "supportsMidConvoSystemMessages": true,
            "supportsAdditionalTools": true,
        })),
        ..base_model("gpt-5.4", "GPT-5.4", "openai-responses", "openai")
    }
}

fn user(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp,
    })
}

fn system_message(
    content: &str,
    sections: Option<indexmap::IndexMap<String, Option<String>>>,
    tools_added: Option<Vec<Tool>>,
    tools_removed: Option<Vec<&str>>,
    timestamp: i64,
) -> Message {
    Message::System(SystemMessage {
        role: Default::default(),
        content: SystemContent::Text(content.into()),
        sections,
        tools_added,
        tools_removed: tools_removed.map(|names| {
            names
                .into_iter()
                .map(|name| kiana_ai::types::ToolReference {
                    name: name.to_string(),
                })
                .collect()
        }),
        timestamp,
    })
}

fn reply(text: &str, provider: &str, api: &str, model: &str) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            text: text.into(),
            ..Default::default()
        })],
        api: api.into(),
        provider: provider.into(),
        model: model.into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    })
}

/// TS 的 `capturePayload`：`streamSimple(...)` + `onPayload` 捕获请求体。
///
/// TS 在 `onPayload` 里抛 `PayloadCaptured` 中断请求；Rust 的 `on_payload` 不能抛错，
/// 因此捕获后返回 `None`，让请求打到返回 500 的假 HTTP 客户端（结果被忽略）。
async fn capture_payload(model: &Model, context: &Context) -> Value {
    let mock = mock_http::MockHttp::new(vec![mock_http::MockResponse::empty(500)]);
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_for_payload = Arc::clone(&captured);
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test-key".into()),
            fetch: Some(Arc::new(mock)),
            on_payload: Some(Arc::new(move |payload: Value, _model: &Model| {
                *captured_for_payload
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(payload);
                None
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    let transcript = normalize_context(context);
    let stream = match model.api.as_str() {
        "anthropic-messages" => anthropic_messages::stream_simple(model, &transcript, options),
        "openai-responses" => openai_responses::stream_simple(model, &transcript, options),
        "openai-completions" => openai_completions::stream_simple(model, &transcript, options),
        other => panic!("unexpected api {other}"),
    }
    .expect("streamSimple returns a stream");
    let _ = stream.result().await;
    captured
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .expect("Expected payload capture")
}

fn tools_list() -> (Tool, Tool, Tool) {
    (tool("base_tool"), tool("late_tool"), tool("drop_tool"))
}

fn system_texts(payload: &Value) -> Vec<String> {
    payload["system"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn tool_names(payload: &Value) -> Vec<String> {
    payload["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn message_roles(payload: &Value) -> Vec<String> {
    payload["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message["role"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn input_items(payload: &Value) -> Vec<Value> {
    payload["input"].as_array().cloned().unwrap_or_default()
}

fn item_types_or_content(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            item.get("type")
                .cloned()
                .unwrap_or_else(|| item["content"].clone())
        })
        .collect()
}

fn function_tool_names(tools: &Value) -> Vec<String> {
    tools
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn expect_growing_prefix(inputs: &[Vec<Value>]) {
    for index in 1..inputs.len() {
        let previous = &inputs[index - 1];
        let current = &inputs[index];
        let prefix_len = previous.len().min(current.len());
        assert_eq!(
            &current[..prefix_len],
            &previous[..],
            "request {index} is not a prefix extension"
        );
    }
}

fn anthropic_context() -> Context {
    Context {
        messages: vec![
            system_message(
                "base prompt",
                Some(indexmap::IndexMap::from([
                    (
                        "rules".to_string(),
                        Some("<rules>\nold rules\n</rules>".to_string()),
                    ),
                    (
                        "docs".to_string(),
                        Some("<docs>\nread docs\n</docs>".to_string()),
                    ),
                ])),
                Some(vec![tool("base_tool")]),
                None,
                0,
            ),
            user("before", 1),
            system_message(
                "updated guidance",
                Some(indexmap::IndexMap::from([
                    (
                        "rules".to_string(),
                        Some("<rules>\nnew rules\n</rules>".to_string()),
                    ),
                    ("docs".to_string(), None),
                ])),
                Some(vec![tool("late_tool")]),
                Some(vec!["base_tool"]),
                2,
            ),
        ],
        ..Context::default()
    }
}

fn addition_context() -> Context {
    Context {
        messages: vec![
            system_message("base prompt", None, Some(vec![tool("base_tool")]), None, 0),
            user("before", 1),
            system_message(
                "updated guidance",
                None,
                Some(vec![tool("late_tool")]),
                None,
                2,
            ),
        ],
        ..Context::default()
    }
}

#[tokio::test]
async fn sends_anthropic_updates_and_tool_changes_in_native_system_messages() {
    let model = anthropic_native_model();
    let context = anthropic_context();
    let payload = capture_payload(&model, &context).await;

    let betas = payload["betas"].as_array().cloned().unwrap_or_default();
    assert!(betas.contains(&json!("inline-tools-2026-09-15")));
    assert!(!betas.contains(&json!("mid-conversation-tool-changes-2026-07-01")));
    assert_eq!(
        system_texts(&payload),
        vec!["base prompt\n\n<rules>\nold rules\n</rules>\n\n<docs>\nread docs\n</docs>"]
    );

    let tools = payload["tools"].as_array().cloned().unwrap_or_default();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], json!("base_tool"));
    assert_eq!(tools[0]["cache_control"], json!({ "type": "ephemeral" }));
    assert!(tools[0].get("defer_loading").is_none());
    assert_eq!(tools[1]["name"], json!("__kiana_deferred_placeholder__"));
    assert_eq!(tools[1]["defer_loading"], json!(true));
    assert!(tools[1].get("cache_control").is_none());

    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    let update = messages.last().expect("update message");
    assert_eq!(update["role"], json!("system"));
    let content = update["content"].as_array().cloned().unwrap_or_default();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"], json!("text"));
    assert_eq!(
        content[1],
        json!({
            "type": "tool_removal",
            "tool": { "type": "tool_reference", "name": "base_tool" },
        })
    );
    // TS 的 `toMatchObject` 是子集匹配：definition 里还有 eager_input_streaming / input_schema。
    assert_eq!(content[2]["type"], json!("tool_addition"));
    assert_eq!(content[2]["tool"]["type"], json!("tool_definition"));
    assert_eq!(content[2]["tool"]["definition"]["name"], json!("late_tool"));
    assert_eq!(
        content[2]["tool"]["definition"]["description"],
        json!("late_tool tool")
    );
    assert_eq!(content[2]["cache_control"], json!({ "type": "ephemeral" }));
    assert!(
        content[2]["tool"]["definition"]
            .get("cache_control")
            .is_none()
    );
    assert!(
        content[2]["tool"]["definition"]
            .get("defer_loading")
            .is_none()
    );
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("updated guidance"), "{text}");
    assert!(text.contains("<rules>\nnew rules\n</rules>"), "{text}");
    assert!(
        text.contains("Removed system prompt section \"docs\""),
        "{text}"
    );

    // placeholder 在第一次请求前就声明，脚手架从 request one 起就被缓存。
    let mut initial_context = context.clone();
    initial_context.messages.truncate(2);
    let initial = capture_payload(&model, &initial_context).await;
    assert_eq!(
        tool_names(&initial),
        vec![
            "base_tool".to_string(),
            "__kiana_deferred_placeholder__".to_string()
        ]
    );
}

#[tokio::test]
async fn redefines_an_anthropic_tool_by_value_under_the_same_name() {
    let mut redefined = tool("base_tool");
    redefined.description = "changed".into();
    let context = Context {
        messages: vec![
            system_message("base prompt", None, Some(vec![tool("base_tool")]), None, 0),
            user("before", 1),
            system_message("", None, Some(vec![redefined]), Some(vec!["base_tool"]), 2),
        ],
        ..Context::default()
    };
    let payload = capture_payload(&anthropic_native_model(), &context).await;

    let betas = payload["betas"].as_array().cloned().unwrap_or_default();
    assert!(betas.contains(&json!("inline-tools-2026-09-15")));
    let tools = payload["tools"].as_array().cloned().unwrap_or_default();
    assert_eq!(tools[0]["name"], json!("base_tool"));
    assert_eq!(tools[0]["description"], json!("base_tool tool"));
    assert_eq!(tools[1]["name"], json!("__kiana_deferred_placeholder__"));

    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    let update = messages.last().expect("update message");
    assert_eq!(update["role"], json!("system"));
    let content = update["content"].as_array().cloned().unwrap_or_default();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], json!("tool_addition"));
    assert_eq!(content[0]["tool"]["type"], json!("tool_definition"));
    assert_eq!(content[0]["tool"]["definition"]["name"], json!("base_tool"));
    assert_eq!(
        content[0]["tool"]["definition"]["description"],
        json!("changed")
    );
}

#[tokio::test]
async fn sends_the_current_anthropic_tool_list_without_an_initial_tool() {
    let context = Context {
        messages: vec![
            system_message("base prompt", None, None, None, 0),
            user("before", 1),
            system_message(
                "updated guidance",
                None,
                Some(vec![tool("late_tool")]),
                None,
                2,
            ),
        ],
        ..Context::default()
    };
    let payload = capture_payload(&anthropic_native_model(), &context).await;

    let betas = payload["betas"].as_array().cloned().unwrap_or_default();
    assert!(!betas.contains(&json!("inline-tools-2026-09-15")));
    let tools = payload["tools"].as_array().cloned().unwrap_or_default();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!("late_tool"));
    assert_eq!(tools[0]["cache_control"], json!({ "type": "ephemeral" }));
    assert!(tools[0].get("defer_loading").is_none());
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    let update = messages.last().expect("update message");
    let content = update["content"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        content
            .iter()
            .map(|block| block["type"].clone())
            .collect::<Vec<_>>(),
        vec![json!("text")]
    );
}

#[tokio::test]
async fn holds_an_anthropic_update_past_an_assistant_message_that_converts_to_no_blocks() {
    let model = anthropic_native_model();
    let messages = vec![
        system_message("base prompt", None, None, None, 0),
        user("hi", 1),
        system_message("updated guidance", None, None, None, 2),
        // 另一个模型的空回复：Anthropic 拒绝空 text block，因此它被丢弃。
        reply("", "openai-codex", "openai-codex-responses", "gpt-x"),
        user("again", 4),
    ];
    let at_end = capture_payload(
        &model,
        &Context {
            messages: messages.clone(),
            ..Context::default()
        },
    )
    .await;
    assert_eq!(
        message_roles(&at_end),
        vec!["user".to_string(), "user".to_string(), "system".to_string()]
    );

    let mut before = messages.clone();
    before.push(reply("answer", &model.provider, &model.api, &model.id));
    before.push(user("third", 5));
    let before_assistant = capture_payload(
        &model,
        &Context {
            messages: before,
            ..Context::default()
        },
    )
    .await;
    assert_eq!(
        message_roles(&before_assistant),
        vec![
            "user".to_string(),
            "user".to_string(),
            "system".to_string(),
            "assistant".to_string(),
            "user".to_string(),
        ]
    );
}

#[tokio::test]
async fn folds_anthropic_updates_into_the_system_prompt_without_native_support() {
    let model = base_model(
        "claude-sonnet-4-5",
        "Claude Sonnet 4.5",
        "anthropic-messages",
        "anthropic",
    );
    let payload = capture_payload(&model, &anthropic_context()).await;

    let betas = payload["betas"].as_array().cloned().unwrap_or_default();
    assert!(!betas.contains(&json!("inline-tools-2026-09-15")));
    assert_eq!(
        system_texts(&payload),
        vec!["base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>"]
    );
    assert_eq!(tool_names(&payload), vec!["late_tool".to_string()]);
    assert_eq!(message_roles(&payload), vec!["user".to_string()]);
}

#[tokio::test]
async fn requires_both_anthropic_capabilities_for_native_tool_changes() {
    let model = Model {
        compat: Some(json!({ "supportsMidConvoToolChanges": true })),
        ..base_model(
            "claude-opus-5",
            "Claude Opus 5",
            "anthropic-messages",
            "anthropic",
        )
    };
    let payload = capture_payload(&model, &anthropic_context()).await;

    let betas = payload["betas"].as_array().cloned().unwrap_or_default();
    assert!(!betas.contains(&json!("inline-tools-2026-09-15")));
    assert_eq!(tool_names(&payload), vec!["late_tool".to_string()]);
    assert_eq!(message_roles(&payload), vec!["user".to_string()]);
}

#[tokio::test]
async fn declares_every_openai_tool_in_additional_tools_at_its_developer_message() {
    let payload = capture_payload(&additional_tools_model(), &addition_context()).await;

    assert!(payload.get("tools").is_none());
    let items = input_items(&payload);
    let declared: Vec<Option<String>> = items
        .iter()
        .filter(|item| item["type"] == json!("additional_tools"))
        .map(|item| item["tools"][0]["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        declared,
        vec![Some("base_tool".to_string()), Some("late_tool".to_string())]
    );
    let kinds = item_types_or_content(&items);
    assert_eq!(kinds.len(), 6);
    assert_eq!(kinds[0], json!("additional_tools"));
    assert_eq!(kinds[1], json!("base prompt"));
    assert!(kinds[2].is_array(), "{:?}", kinds[2]);
    assert!(
        kinds[3]
            .as_str()
            .is_some_and(|text| text.contains("incremental tools update")),
        "{:?}",
        kinds[3]
    );
    assert_eq!(kinds[4], json!("additional_tools"));
    assert_eq!(kinds[5], json!("updated guidance"));
}

#[tokio::test]
async fn maps_system_message_additions_into_synthetic_tool_search() {
    let model = Model {
        compat: Some(json!({
            "supportsMidConvoSystemMessages": true,
            "supportsToolSearch": true,
        })),
        ..base_model("gpt-5.4", "GPT-5.4", "openai-responses", "openai")
    };
    let payload = capture_payload(&model, &addition_context()).await;

    assert_eq!(tool_names(&payload), vec!["base_tool".to_string()]);
    let items = input_items(&payload);
    let types: Vec<&str> = items
        .iter()
        .filter_map(|item| item["type"].as_str())
        .collect();
    assert!(types.contains(&"tool_search_call"), "{types:?}");
    let output = items
        .iter()
        .find(|item| item["type"] == json!("tool_search_output"))
        .expect("tool_search_output");
    let names: Vec<&str> = output["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(names, vec!["late_tool"]);
}

#[tokio::test]
async fn folds_openai_updates_into_the_leading_developer_message_without_native_support() {
    let model = Model {
        compat: Some(json!({ "supportsAdditionalTools": true })),
        ..base_model("gpt-4.1", "GPT-4.1", "openai-responses", "openai")
    };
    let payload = capture_payload(&model, &anthropic_context()).await;

    assert_eq!(tool_names(&payload), vec!["late_tool".to_string()]);
    let items = input_items(&payload);
    let kinds: Vec<Value> = items
        .iter()
        .map(|item| {
            item.get("type")
                .cloned()
                .unwrap_or_else(|| item["role"].clone())
        })
        .collect();
    assert_eq!(kinds, vec![json!("developer"), json!("user")]);
    assert_eq!(
        items[0]["content"],
        json!("base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>")
    );
}

#[tokio::test]
async fn announces_openai_removals_as_text() {
    let payload = capture_payload(&additional_tools_model(), &anthropic_context()).await;

    assert!(payload.get("tools").is_none());
    let items = input_items(&payload);
    let declared: Vec<Option<String>> = items
        .iter()
        .filter(|item| item["type"] == json!("additional_tools"))
        .map(|item| item["tools"][0]["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        declared,
        vec![Some("base_tool".to_string()), Some("late_tool".to_string())]
    );
    let update = items.last().expect("update item")["content"]
        .as_str()
        .unwrap_or_default();
    assert!(update.contains("updated guidance"), "{update}");
    assert!(
        update.contains(
            "The following tools are no longer available. Do not call them:\n- base_tool"
        ),
        "{update}"
    );
}

#[tokio::test]
async fn redefines_an_openai_tool_with_additional_tools_instead_of_announcing_a_removal() {
    let mut redefined = tool("base_tool");
    redefined.description = "changed".into();
    let context = Context {
        messages: vec![
            system_message("base prompt", None, Some(vec![tool("base_tool")]), None, 0),
            user("before", 1),
            system_message("", None, Some(vec![redefined]), Some(vec!["base_tool"]), 2),
        ],
        ..Context::default()
    };
    let payload = capture_payload(&additional_tools_model(), &context).await;

    assert!(payload.get("tools").is_none());
    let items = input_items(&payload);
    assert_eq!(items.len(), 5);
    assert_eq!(items[0]["type"], json!("additional_tools"));
    assert_eq!(items[0]["tools"][0]["name"], json!("base_tool"));
    assert_eq!(items[0]["tools"][0]["description"], json!("base_tool tool"));
    assert_eq!(items[1]["role"], json!("developer"));
    assert_eq!(items[1]["content"], json!("base prompt"));
    assert_eq!(items[2]["role"], json!("user"));
    assert_eq!(items[3]["role"], json!("developer"));
    assert!(
        items[3]["content"]
            .as_str()
            .is_some_and(|text| text.contains("latest definition replaces the earlier one")),
        "{:?}",
        items[3]["content"]
    );
    assert_eq!(items[4]["type"], json!("additional_tools"));
    assert_eq!(items[4]["tools"][0]["name"], json!("base_tool"));
    assert_eq!(items[4]["tools"][0]["description"], json!("changed"));
}

/// TS 的 `captureToolHistory`：先声明工具，再移除 drop_tool，随后两次加载。
async fn capture_tool_history(model: &Model) -> Vec<Value> {
    let (base_tool, late_tool, drop_tool) = tools_list();
    let steps: Vec<Vec<Message>> = vec![
        vec![user("first", 1)],
        vec![
            system_message("", None, None, Some(vec!["drop_tool"]), 2),
            user("after removal", 3),
        ],
        vec![
            system_message("", None, Some(vec![late_tool]), None, 4),
            user("after first load", 5),
        ],
        vec![
            system_message("", None, Some(vec![tool("later_tool")]), None, 6),
            user("after second load", 7),
        ],
    ];
    let mut messages = vec![system_message(
        "base prompt",
        None,
        Some(vec![base_tool, drop_tool]),
        None,
        0,
    )];
    let mut payloads = Vec::new();
    for step in steps {
        messages.extend(step);
        payloads.push(
            capture_payload(
                model,
                &Context {
                    messages: messages.clone(),
                    ..Context::default()
                },
            )
            .await,
        );
    }
    payloads
}

#[tokio::test]
async fn keeps_the_openai_request_prefix_across_a_removal_and_later_loads() {
    let payloads = capture_tool_history(&additional_tools_model()).await;
    for payload in &payloads {
        assert!(payload.get("tools").is_none());
    }
    let inputs: Vec<Vec<Value>> = payloads.iter().map(input_items).collect();
    expect_growing_prefix(&inputs);

    let last = payloads.last().expect("last payload");
    let items = input_items(last);
    let declared: Vec<Vec<String>> = items
        .iter()
        .filter(|item| item["type"] == json!("additional_tools"))
        .map(|item| {
            item["tools"]
                .as_array()
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        declared,
        vec![
            vec!["base_tool".to_string(), "drop_tool".to_string()],
            vec!["late_tool".to_string()],
            vec!["later_tool".to_string()],
        ]
    );
    assert!(
        items
            .iter()
            .any(|item| item["content"].as_str().is_some_and(|text| {
                text.contains("no longer available. Do not call them:\n- drop_tool")
            }))
    );
    let notices: Vec<String> = items
        .iter()
        .filter_map(|item| item["content"].as_str())
        .filter(|text| text.contains("incremental tools update"))
        .map(str::to_string)
        .collect();
    assert_eq!(notices.len(), 2, "{notices:?}");
    for notice in notices {
        assert!(
            notice.contains("\nStill unavailable: drop_tool."),
            "{notice}"
        );
    }
}

#[tokio::test]
async fn stops_listing_a_tool_as_unavailable_once_it_is_declared_again() {
    let (base_tool, _, drop_tool) = tools_list();
    let context = Context {
        messages: vec![
            system_message(
                "base prompt",
                None,
                Some(vec![base_tool.clone(), drop_tool.clone()]),
                None,
                0,
            ),
            system_message("", None, None, Some(vec!["drop_tool"]), 1),
            system_message("", None, Some(vec![drop_tool]), None, 2),
            user("go", 3),
        ],
        ..Context::default()
    };
    let payload = capture_payload(&additional_tools_model(), &context).await;

    let items = input_items(&payload);
    let notice = items
        .iter()
        .find(|item| {
            item["content"]
                .as_str()
                .is_some_and(|text| text.contains("incremental tools update"))
        })
        .expect("incremental tools update notice");
    let notice = notice["content"].as_str().unwrap_or_default();
    assert!(!notice.contains("Still unavailable"), "{notice}");
    let last_tools = &items[items.len() - 2];
    assert_eq!(last_tools["type"], json!("additional_tools"));
    assert_eq!(last_tools["tools"][0]["name"], json!("drop_tool"));
}

#[tokio::test]
async fn does_not_list_a_tool_as_still_unavailable_before_its_removal_is_announced() {
    let (base_tool, late_tool, drop_tool) = tools_list();
    let context = Context {
        messages: vec![
            system_message(
                "base prompt",
                None,
                Some(vec![base_tool, drop_tool]),
                None,
                0,
            ),
            system_message("", None, Some(vec![late_tool]), Some(vec!["drop_tool"]), 1),
            user("go", 2),
        ],
        ..Context::default()
    };
    let payload = capture_payload(&additional_tools_model(), &context).await;

    let items = input_items(&payload);
    // 与 Codex 一致：notice，然后是工具，最后是移除通知。
    let tail_start = items.len() - 4;
    let notice = items[tail_start]["content"].as_str().unwrap_or_default();
    assert!(notice.contains("incremental tools update"), "{notice}");
    assert!(!notice.contains("Still unavailable"), "{notice}");
    let tail = &items[items.len() - 3..];
    assert_eq!(tail[0]["type"], json!("additional_tools"));
    assert_eq!(tail[0]["tools"][0]["name"], json!("late_tool"));
    assert!(
        tail[1]["content"].as_str().is_some_and(
            |text| text.contains("no longer available. Do not call them:\n- drop_tool")
        ),
        "{:?}",
        tail[1]["content"]
    );
    assert_eq!(tail[2]["role"], json!("user"));
}

#[tokio::test]
async fn keeps_the_openai_tool_search_prefix_across_loads_after_a_removal() {
    let model = Model {
        compat: Some(json!({
            "supportsMidConvoSystemMessages": true,
            "supportsToolSearch": true,
        })),
        ..additional_tools_model()
    };
    let payloads = capture_tool_history(&model).await;
    let before_removal = &payloads[0];
    assert_eq!(
        tool_names(before_removal),
        vec!["base_tool".to_string(), "drop_tool".to_string()]
    );
    let after_removal = &payloads[1..];
    for payload in after_removal {
        assert_eq!(tool_names(payload), vec!["base_tool".to_string()]);
    }
    let inputs: Vec<Vec<Value>> = after_removal.iter().map(input_items).collect();
    expect_growing_prefix(&inputs);

    let last = payloads.last().expect("last payload");
    let items = input_items(last);
    let searched: Vec<Option<String>> = items
        .iter()
        .filter(|item| item["type"] == json!("tool_search_output"))
        .map(|item| item["tools"][0]["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        searched,
        vec![
            Some("late_tool".to_string()),
            Some("later_tool".to_string())
        ]
    );
}

#[tokio::test]
async fn anchors_kimi_additions_in_tool_bearing_system_messages() {
    let model = Model {
        compat: Some(json!({
            "supportsMidConvoSystemMessages": true,
            "supportsMidConvoToolAdditions": true,
        })),
        ..base_model("kimi-k3", "Kimi K3", "openai-completions", "moonshotai")
    };
    let payload = capture_payload(&model, &addition_context()).await;

    assert_eq!(
        function_tool_names(&payload["tools"]),
        vec!["base_tool".to_string()]
    );
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    let with_tools = messages
        .iter()
        .find(|message| message.get("tools").is_some())
        .expect("tool-bearing system message");
    assert_eq!(
        function_tool_names(&with_tools["tools"]),
        vec!["late_tool".to_string()]
    );
    let system_contents: Vec<Option<Value>> = messages
        .iter()
        .filter(|message| message["role"] == json!("system"))
        .map(|message| message.get("content").cloned())
        .collect();
    assert_eq!(
        system_contents,
        vec![
            Some(json!("base prompt")),
            None,
            Some(json!("updated guidance"))
        ]
    );
}

#[tokio::test]
async fn keeps_the_kimi_prefix_across_loads_after_a_removal() {
    let model = mock_http::catalog_model("kimi-coding", "k3");
    let payloads = capture_tool_history(&model).await;
    let before_removal = &payloads[0];
    assert_eq!(
        function_tool_names(&before_removal["tools"]),
        vec!["base_tool".to_string(), "drop_tool".to_string()]
    );
    let after_removal = &payloads[1..];
    for payload in after_removal {
        assert_eq!(
            function_tool_names(&payload["tools"]),
            vec!["base_tool".to_string()]
        );
    }
    let message_lists: Vec<Vec<Value>> = after_removal
        .iter()
        .map(|payload| payload["messages"].as_array().cloned().unwrap_or_default())
        .collect();
    expect_growing_prefix(&message_lists);

    let last = payloads.last().expect("last payload");
    let messages = last["messages"].as_array().cloned().unwrap_or_default();
    let anchored: Vec<Option<String>> = messages
        .iter()
        .filter(|message| message.get("tools").is_some())
        .map(|message| {
            message["tools"][0]["function"]["name"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        anchored,
        vec![
            Some("late_tool".to_string()),
            Some("later_tool".to_string())
        ]
    );
}

async fn assert_anchors_additions_for_kimi_coding(model_id: &str) {
    let model = mock_http::catalog_model("kimi-coding", model_id);
    let payload = capture_payload(&model, &addition_context()).await;

    assert_eq!(
        function_tool_names(&payload["tools"]),
        vec!["base_tool".to_string()]
    );
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    let with_tools = messages
        .iter()
        .find(|message| message.get("tools").is_some())
        .expect("tool-bearing system message");
    assert_eq!(
        function_tool_names(&with_tools["tools"]),
        vec!["late_tool".to_string()]
    );
}

#[tokio::test]
async fn anchors_additions_for_kimi_coding_k3() {
    assert_anchors_additions_for_kimi_coding("k3").await;
}

#[tokio::test]
async fn anchors_additions_for_kimi_coding_k3_256k() {
    assert_anchors_additions_for_kimi_coding("k3-256k").await;
}

#[tokio::test]
async fn anchors_additions_for_kimi_coding_kimi_for_coding() {
    assert_anchors_additions_for_kimi_coding("kimi-for-coding").await;
}

#[tokio::test]
async fn keeps_kimi_coding_highspeed_additions_in_top_level_tools() {
    let model = mock_http::catalog_model("kimi-coding", "kimi-for-coding-highspeed");
    let payload = capture_payload(&model, &addition_context()).await;

    assert_eq!(
        function_tool_names(&payload["tools"]),
        vec!["base_tool".to_string(), "late_tool".to_string()]
    );
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    assert!(
        !messages
            .iter()
            .any(|message| message.get("tools").is_some())
    );
}

#[tokio::test]
async fn keeps_kimi_k2_system_text_inline_without_dynamic_tool_messages() {
    let model = Model {
        compat: Some(json!({ "supportsMidConvoSystemMessages": true })),
        ..base_model(
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            "openai-completions",
            "moonshotai",
        )
    };
    let payload = capture_payload(&model, &addition_context()).await;

    assert_eq!(
        function_tool_names(&payload["tools"]),
        vec!["base_tool".to_string(), "late_tool".to_string()]
    );
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    assert!(
        !messages
            .iter()
            .any(|message| message.get("tools").is_some())
    );
    let system_contents: Vec<Option<String>> = messages
        .iter()
        .filter(|message| message["role"] == json!("system"))
        .map(|message| message["content"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        system_contents,
        vec![
            Some("base prompt".to_string()),
            Some("updated guidance".to_string())
        ]
    );
}

#[tokio::test]
async fn folds_openai_compatible_updates_into_the_system_prompt_without_native_support() {
    let model = Model {
        reasoning: Some(false),
        ..base_model(
            "custom-model",
            "Custom model",
            "openai-completions",
            "custom-provider",
        )
    };
    let payload = capture_payload(&model, &anthropic_context()).await;

    assert_eq!(
        function_tool_names(&payload["tools"]),
        vec!["late_tool".to_string()]
    );
    assert_eq!(
        message_roles(&payload),
        vec!["system".to_string(), "user".to_string()]
    );
    let messages = payload["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        messages[0]["content"],
        json!("base prompt\n\nupdated guidance\n\n<rules>\nnew rules\n</rules>")
    );
}
