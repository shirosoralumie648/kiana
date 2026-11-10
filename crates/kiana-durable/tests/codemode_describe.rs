//! `test/codemode-describe.test.ts` 的移植：code mode 的工具描述与 `codemode` 提示段。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::sync::Arc;

use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, TextContent,
    TextKind,
};
use kiana_codemode::source::CODEMODE_SOURCE_GRAMMAR;
use kiana_durable::Session as _;
use kiana_durable::codemode::codemode::{CodemodeToolOptions, create_codemode_extension};
use kiana_durable::codemode::describe::{
    CodemodeNamespace, codemode_description, codemode_section,
};
use kiana_durable::harness::types::{
    Registry as _, ToolCaller, ToolExecuteFn, ToolExecutionResult, ToolRegistration,
};
use kiana_durable::types::{BoxError, Storage};
use serde_json::json;

use chat_support::{ChatSetup, context, open_chat};
use harness_support::{add_tool, text_input};

fn tool(name: &str, description: &str) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |_arguments, _api, _context| {
        Box::pin(async { Ok(ToolExecutionResult::default()) })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    ToolRegistration {
        tool: kiana_ai::Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string", "description": "File path" } }
            }),
            constrained_sampling: None,
        },
        replay: Some(kiana_durable::harness::types::ReplayPolicy::Unsafe),
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(execute),
    }
}

fn github() -> CodemodeNamespace {
    CodemodeNamespace {
        name: "github".to_owned(),
        description: Some("GitHub MCP server".to_owned()),
        instructions: None,
        prefix: "mcp__github__".to_owned(),
    }
}

#[test]
fn keeps_the_tool_description_the_same_for_every_conversation() {
    let text = codemode_description(false);
    assert!(text.contains("`await tools.<name>({ ...args })`"));
    assert!(text.contains("searchTools(query"));
    assert!(
        text.contains(
            "The `codemode` section of the system prompt lists the tools scripts can call"
        )
    );
    assert!(!text.contains("`models`"));
    assert!(codemode_description(true).contains("`models`"));
}

#[test]
fn lists_callable_tools_as_declarations_grouped_by_namespace_with_descriptions() {
    let section = codemode_section(
        &[
            tool("read", "Read a file."),
            tool("mcp__github__list_issues", "List issues."),
        ],
        &[github()],
        3000.0,
    )
    .expect("a section");
    assert!(section.contains("### `read`\nRead a file."));
    assert!(section.contains("read(args: {"));
    assert!(section.contains("## github\nGitHub MCP server"));
    assert!(
        section.find("### `read`").unwrap() < section.find("## github").unwrap(),
        "{section}"
    );
    assert!(
        section.find("## github").unwrap()
            < section.find("### `mcp__github__list_issues`").unwrap(),
        "{section}"
    );
    assert!(section.contains("Promise<string>"));
}

#[test]
fn lists_what_fits_the_budget_every_namespace_before_any_is_complete_and_says_what_is_left_out() {
    let mut tools = vec![tool("read", "Read a file.")];
    for index in 0..30 {
        tools.push(tool(&format!("mcp__github__tool{index}"), &"x".repeat(200)));
    }
    let section = codemode_section(&tools, &[github()], 400.0).expect("a section");
    assert!(section.contains("### `read`"));
    assert!(section.contains("## github (some tools not listed)"));
    assert!(!section.contains("tool29"), "{section}");
    let github_only: Vec<ToolRegistration> = (0..30)
        .map(|index| tool(&format!("mcp__github__tool{index}"), &"x".repeat(200)))
        .collect();
    assert!(
        codemode_section(&github_only, &[github()], 10.0)
            .expect("a section")
            .contains("## github (tools not listed)")
    );
    assert!(codemode_section(&[], &[github()], 3000.0).is_none());
}

#[test]
fn uses_the_longest_matching_namespace_prefix() {
    let section = codemode_section(
        &[tool("mcp__github__x", "x")],
        &[
            CodemodeNamespace {
                name: "git".to_owned(),
                description: None,
                instructions: None,
                prefix: "mcp__git".to_owned(),
            },
            github(),
        ],
        3000.0,
    )
    .expect("a section");
    assert!(section.contains("## github"));
    assert!(!section.contains("## git\n"));
}

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "hi".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 1,
        ..Default::default()
    }
}

fn sandbox() -> Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox> {
    let sandbox =
        kiana_codemode::CodemodeSandbox::new(kiana_codemode::types::CodemodeSandboxOptions {
            base: kiana_codemode::types::CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(64.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("the sandbox");
    Arc::new(sandbox) as Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox>
}

#[tokio::test(flavor = "multi_thread")]
async fn renders_the_section_from_the_conversations_callable_tools_into_the_system_prompt() {
    let setup = ChatSetup::new();
    setup
        .registry
        .install(create_codemode_extension(CodemodeToolOptions {
            sandbox: sandbox(),
            namespaces: Some(vec![github()]),
            inline_budget: None,
            models: None,
            globals: None,
        }));
    let _ = add_tool(&setup.registry, tool("read", "Read a file."));
    let mut list_issues = tool("mcp__github__list_issues", "List issues.");
    list_issues.callers = Some(vec![ToolCaller::Tools]);
    let _ = add_tool(&setup.registry, list_issues);
    let mut get_issue = tool("mcp__github__get_issue", "Get an issue.");
    get_issue.deferred = true;
    let _ = add_tool(&setup.registry, get_issue);
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(done())]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    let entries = chat_support::all_entries(&root, &*context()).await;
    let system = entries
        .iter()
        .find(|entry| entry.kind == "kiana.system")
        .expect("the system entry");
    let message = match system
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message")
    {
        Message::System(message) => message.clone(),
        _ => panic!("a system message"),
    };
    let sections = message.sections.clone().expect("sections");
    let codemode = sections
        .get("codemode")
        .and_then(|section| section.clone())
        .expect("the codemode section");
    assert!(codemode.contains("### `read`"), "{codemode}");
    assert!(
        codemode.contains("### `mcp__github__list_issues`"),
        "{codemode}"
    );
    // 延迟工具用 searchTools() 找，不列出。
    assert!(!codemode.contains("mcp__github__get_issue"));
    // code mode 工具本身不能被脚本调用。
    assert!(!codemode.contains("### `codemode`"));
    // 工具声明带上脚本语法；kiana-ai 只对支持的模型应用。
    let tools = message.tools_added.clone().expect("toolsAdded");
    let codemode_tool = tools
        .iter()
        .find(|tool| tool.name == "codemode")
        .expect("the codemode tool");
    assert_eq!(
        codemode_tool.constrained_sampling,
        Some(kiana_ai::types::ConstrainedSampling::Config(
            kiana_ai::types::ConstrainedSamplingConfig::Grammar {
                variants: kiana_ai::types::GrammarVariants {
                    openai_lark: Some(CODEMODE_SOURCE_GRAMMAR.to_owned()),
                    openai_regex: None,
                },
            },
        ))
    );
    harness.close(&*context()).await;
}
