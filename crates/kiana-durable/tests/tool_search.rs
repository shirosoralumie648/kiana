//! `test/tool-search.test.ts` 的移植：延迟工具与 `tool_search`。
//!
//! 延迟工具先不提供给模型，`tool_search` 命中后用 `addTools` 把声明追加到下一次请求；
//! 装载写会话的 `modelTools`，因此按分支持久、重置后进入基线。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::pin::Pin;
use std::sync::Arc;

use kiana_ai::providers::faux::{FauxResponseStep, FauxToolCallOptions, faux_tool_call_with};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonObject, Message, StopReason,
    SystemMessage, TextContent, TextKind, UserContentBlock,
};
use kiana_durable::Session as _;
use kiana_durable::harness::agent::AGENT_DOC;
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationCreateOptions, Extension,
    ModelToolChange, ToolControl, ToolExecuteFn, ToolExecutionResult, ToolRegistration,
};
use kiana_durable::tool_search::tool_search::create_tool_search_extension;
use kiana_durable::types::{
    BoxError, ConversationOwnership, DocTarget, DocumentReader, EntryRecord, JsonValue,
};
use serde_json::json;

use chat_support::{ChatSetup, context, open_chat};
use harness_support::{TestExtension, add_tool, text_input};

fn memory() -> Arc<dyn kiana_durable::types::Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new())
        as Arc<dyn kiana_durable::types::Storage>
}

fn text_output(text: &str) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })]
}

/// `tool(name, description, extra)`：执行返回 `${name} ran`。
fn tool(name: &str, description: &str, deferred: bool) -> ToolRegistration {
    let output_text = format!("{name} ran");
    let execute: ToolExecuteFn = Arc::new(move |_arguments, _api, _context| {
        let output = text_output(&output_text);
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(output),
                ..Default::default()
            })
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    ToolRegistration {
        tool: kiana_ai::Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "number": { "type": "number", "description": "Issue number" } }
            }),
            constrained_sampling: None,
        },
        replay: Some(kiana_durable::harness::types::ReplayPolicy::Unsafe),
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred,
        output_limits: None,
        execute: Some(execute),
    }
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "done".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    }
}

fn call(name: &str, arguments: JsonObject, id: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(faux_tool_call_with(
            name,
            arguments,
            FauxToolCallOptions {
                id: Some(id.to_owned()),
            },
        ))],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

fn search_arguments(query: &str, limit: Option<f64>) -> JsonObject {
    let mut arguments = JsonObject::new();
    arguments.insert("query".to_owned(), JsonValue::String(query.to_owned()));
    if let Some(limit) = limit {
        arguments.insert("limit".to_owned(), json!(limit));
    }
    arguments
}

/// `alpha`、延迟的 `github_get_issue` 与 `github_list_repos`、`omega`，最后是 `tool_search`。
fn setup() -> ChatSetup {
    let chat = ChatSetup::new();
    let _ = add_tool(&chat.registry, tool("alpha", "Alpha tool", false));
    let _ = add_tool(
        &chat.registry,
        tool("github_get_issue", "Get a GitHub issue by number", true),
    );
    let _ = add_tool(
        &chat.registry,
        tool("github_list_repos", "List the repositories of a user", true),
    );
    let _ = add_tool(&chat.registry, tool("omega", "Omega tool", false));
    setup_registry(&chat);
    chat
}

fn setup_registry(chat: &ChatSetup) {
    use kiana_durable::harness::types::Registry as _;
    chat.registry.install(create_tool_search_extension());
}

/// 模型被提供的工具名。
async fn offered(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    root.agent(&*context())
        .await
        .expect("agent")
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

async fn deferred(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    root.agent(&*context())
        .await
        .expect("agent")
        .deferred
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

async fn callable(root: &Arc<dyn ConversationTrait>) -> Vec<String> {
    root.agent(&*context())
        .await
        .expect("agent")
        .callable
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

fn system_messages(entries: &[EntryRecord]) -> Vec<SystemMessage> {
    entries
        .iter()
        .filter(|entry| entry.kind == "kiana.system")
        .map(|entry| match &entry.model.as_ref().expect("model")[0] {
            Message::System(message) => message.clone(),
            other => panic!("a system message, got {other:?}"),
        })
        .collect()
}

fn added_names(message: &SystemMessage) -> Vec<String> {
    message
        .tools_added
        .as_ref()
        .expect("toolsAdded")
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn removed_names(message: &SystemMessage) -> Option<Vec<String>> {
    message
        .tools_removed
        .as_ref()
        .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect())
}

fn tool_result_texts(entries: &[EntryRecord]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.kind == "kiana.tool-result")
        .map(|entry| match &entry.model.as_ref().expect("model")[0] {
            Message::ToolResult(result) => result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            other => panic!("a tool result, got {other:?}"),
        })
        .collect()
}

async fn submit(root: &Arc<dyn ConversationTrait>, message: &str) {
    let submission = root
        .submit(text_input(message).into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
}

async fn agent_model_tools(
    harness: &Arc<kiana_durable::harness::harness::HarnessImpl>,
    root: &Arc<dyn ConversationTrait>,
) -> Option<JsonValue> {
    harness
        .snapshot(&DocTarget::conversation(&AGENT_DOC, root.id()), &*context())
        .await
        .expect("snapshot")
        .and_then(|state| state.get("modelTools").cloned())
}

#[tokio::test(flavor = "multi_thread")]
async fn offers_a_deferred_tool_only_once_tool_search_loads_it_by_appending_its_declaration() {
    let chat = setup();
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("github issue", Some(1.0)),
            "c1",
        )),
        FauxResponseStep::Message(call(
            "github_get_issue",
            {
                let mut arguments = JsonObject::new();
                arguments.insert("number".to_owned(), json!(42));
                arguments
            },
            "c2",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    assert_eq!(offered(&root).await, vec!["alpha", "omega", "tool_search"]);
    assert_eq!(
        deferred(&root).await,
        vec!["github_get_issue", "github_list_repos"]
    );
    // 延迟工具仍可被其他工具（例如 code mode 脚本）调用。
    assert!(
        callable(&root)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    submit(&root, "go").await;
    let entries = chat_support::all_entries(&root, &*context()).await;
    let texts = tool_result_texts(&entries);
    assert_eq!(
        texts[0],
        "Loaded 1 tool. They are available from your next call:\n- github_get_issue: Get a GitHub issue by number"
    );
    assert_eq!(texts[1], "github_get_issue ran");
    assert_eq!(
        agent_model_tools(&harness, &root).await,
        Some(json!({"add": ["github_get_issue"]}))
    );
    assert_eq!(
        offered(&root).await,
        vec!["alpha", "omega", "tool_search", "github_get_issue"]
    );
    // 之后的搜索只看到还没装载的工具。
    assert_eq!(deferred(&root).await, vec!["github_list_repos"]);
    // 装载是追加声明：没有工具被移除再声明，缓存的 prefix 保持有效。
    let messages = system_messages(&entries);
    assert_eq!(
        added_names(&messages[0]),
        vec!["alpha", "omega", "tool_search"]
    );
    let load = &messages[1];
    assert_eq!(added_names(load), vec!["github_get_issue"]);
    assert_eq!(removed_names(load), None);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn appends_each_loaded_tool_in_load_order_not_registry_order() {
    let chat = setup();
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("repositories", Some(1.0)),
            "c1",
        )),
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("issue", Some(1.0)),
            "c2",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    submit(&root, "go").await;
    assert_eq!(
        offered(&root).await,
        vec![
            "alpha",
            "omega",
            "tool_search",
            "github_list_repos",
            "github_get_issue"
        ]
    );
    let messages = system_messages(&chat_support::all_entries(&root, &*context()).await);
    let loads: Vec<Vec<String>> = messages[1..].iter().map(added_names).collect();
    assert_eq!(
        loads,
        vec![
            vec!["github_list_repos".to_owned()],
            vec!["github_get_issue".to_owned()]
        ]
    );
    assert!(
        messages[1..]
            .iter()
            .all(|message| removed_names(message).is_none())
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn records_loads_by_name_unknown_and_non_deferred_names_change_nothing_remove_wins_and_a_load_lifts_it()
 {
    let chat = setup();
    let grow: ToolRegistration = {
        let execute: ToolExecuteFn = Arc::new(move |_arguments, _api, _context| {
            Box::pin(async move {
                Ok(ToolExecutionResult {
                    output: Some(Vec::new()),
                    control: Some(ToolControl {
                        add_tools: Some(vec![
                            "github_get_issue".to_owned(),
                            "alpha".to_owned(),
                            "ghost".to_owned(),
                        ]),
                        terminate: None,
                        handoff: None,
                    }),
                    ..Default::default()
                })
            })
                as Pin<
                    Box<
                        dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                            + Send,
                    >,
                >
        });
        let mut grow = harness_support::tool("grow");
        grow.tool.description = "grow".to_owned();
        grow.execute = Some(execute);
        grow
    };
    let _ = add_tool(&chat.registry, grow);
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call("grow", JsonObject::new(), "c1")),
        FauxResponseStep::Message(done()),
        FauxResponseStep::Message(call("grow", JsonObject::new(), "c2")),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    submit(&root, "go").await;
    assert_eq!(
        agent_model_tools(&harness, &root).await,
        Some(json!({"add": ["github_get_issue", "alpha", "ghost"]}))
    );
    assert_eq!(
        offered(&root).await,
        vec!["alpha", "omega", "tool_search", "grow", "github_get_issue"]
    );
    // 同一个名字同时在 remove 与 add 里时不提供。
    let get_issue = root
        .agent(&*context())
        .await
        .expect("agent")
        .callable
        .iter()
        .find(|tool| tool.name() == "github_get_issue")
        .expect("github_get_issue")
        .clone();
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Edit {
                remove: Some(vec![get_issue.clone()]),
                add: Some(vec![get_issue]),
            }),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    assert!(
        !offered(&root)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    // 再装载一次会把它从 remove 里去掉。
    submit(&root, "again").await;
    assert_eq!(
        agent_model_tools(&harness, &root).await,
        Some(json!({"add": ["github_get_issue", "alpha", "ghost"]}))
    );
    assert!(
        offered(&root)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_a_loaded_tool_when_its_extension_is_gone_for_a_while_and_appends_to_an_explicit_array()
 {
    use kiana_durable::harness::types::Registry as _;
    let chat = setup();
    let issue = tool("github_get_issue", "Get a GitHub issue by number", true);
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("github issue", Some(1.0)),
            "c1",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    submit(&root, "go").await;
    // 服务器暂时消失：它的扩展被重新安装成不含该工具。
    chat.registry.install(Arc::new(
        TestExtension::new("tool:github_get_issue").with_tools(Vec::new()),
    ) as Arc<dyn Extension>);
    assert!(
        !offered(&root)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    chat.registry.install(Arc::new(
        TestExtension::new("tool:github_get_issue").with_tools(vec![issue]),
    ) as Arc<dyn Extension>);
    assert!(
        offered(&root)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    // 显式数组时，装载照旧追加到它。
    let agent = root.agent(&*context()).await.expect("agent");
    let alpha = agent
        .tools
        .iter()
        .find(|tool| tool.name() == "alpha")
        .expect("alpha")
        .clone();
    let search = agent
        .tools
        .iter()
        .find(|tool| tool.name() == "tool_search")
        .expect("tool_search")
        .clone();
    drop(agent);
    root.configure(
        AgentChange {
            model_tools: Some(ModelToolChange::Set(vec![alpha, search])),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("repositories", Some(1.0)),
            "c3",
        )),
        FauxResponseStep::Message(done()),
    ]);
    submit(&root, "more").await;
    assert_eq!(
        agent_model_tools(&harness, &root).await,
        Some(json!(["alpha", "tool_search", "github_list_repos"]))
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_loaded_tools_per_branch_and_in_the_baseline_after_a_reset() {
    let chat = setup();
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("github issue", Some(1.0)),
            "c1",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    submit(&root, "go").await;
    let entries = chat_support::all_entries(&root, &*context()).await;
    let before = root
        .fork(
            entries[0].id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context(),
        )
        .await
        .expect("fork before");
    let after = root
        .fork(
            entries.last().expect("an entry").id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context(),
        )
        .await
        .expect("fork after");
    assert!(
        !offered(&before)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    assert!(
        offered(&after)
            .await
            .contains(&"github_get_issue".to_owned())
    );
    root.reset(None, &*context()).await.expect("reset");
    chat.faux
        .set_responses(vec![FauxResponseStep::Message(done())]);
    submit(&root, "fresh").await;
    let messages = system_messages(&chat_support::all_entries(&root, &*context()).await);
    let baseline = messages.last().expect("a baseline");
    assert_eq!(
        added_names(baseline),
        vec!["alpha", "omega", "tool_search", "github_get_issue"]
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_nothing_for_an_unrelated_query_and_loads_nothing() {
    let chat = setup();
    chat.faux.set_responses(vec![
        FauxResponseStep::Message(call(
            "tool_search",
            search_arguments("weather forecast", None),
            "c1",
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat).await;
    submit(&root, "go").await;
    assert_eq!(
        tool_result_texts(&chat_support::all_entries(&root, &*context()).await),
        vec!["No matching tools found.".to_owned()]
    );
    assert_eq!(agent_model_tools(&harness, &root).await, None);
    harness.close(&*context()).await;
}
