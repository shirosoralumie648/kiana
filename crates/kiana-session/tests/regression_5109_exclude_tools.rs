//! `regressions/5109-exclude-tools.test.ts` 的 Rust 移植（regression #5109）。

mod common;

use std::sync::Arc;

use common::*;
use kiana_ext_api::events::ExtensionEventKind;

fn tools_extension() -> kiana_ext_api::types::ExtensionFactory {
    factory(|kiana| {
        let definition = Arc::clone(&kiana);
        let _ = kiana.on(
            ExtensionEventKind::SessionStart,
            standard(move |_event, _ctx| {
                let kiana = Arc::clone(&definition);
                Box::pin(async move {
                    kiana.register_tool(simple_tool_definition(
                        "ask_question",
                        "Ask a question",
                        Some("Ask a question"),
                    ));
                    kiana.register_tool(simple_tool_definition(
                        "dynamic_tool",
                        "Dynamic test tool",
                        Some("Run dynamic test behavior"),
                    ));
                    Ok(None)
                })
            }),
        );
    })
}

fn tool_names(tools: &[kiana_ext_api::types::ToolInfo]) -> Vec<String> {
    let mut names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    names.sort();
    names
}

/// TS: "filters built-in and extension tools from available and active tools"
///
/// 忽略原因：`session_start` handler 里 `registerTool` 会同步回调 `refreshTools`，
/// 而 kiana-extensions 在派发期间持有扩展表的可变借用；重入 `get_all_registered_tools`
#[tokio::test]
async fn filters_builtin_and_extension_tools_from_available_and_active_tools() {
    run(async {
        let harness = create_harness(HarnessOptions {
            excluded_tool_names: Some(vec!["read".to_string(), "ask_question".to_string()]),
            extension_factories: vec![tools_extension()],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;
        let all = tool_names(&harness.session.get_all_tools());
        assert!(!all.contains(&"read".to_string()));
        assert!(!all.contains(&"ask_question".to_string()));
        assert!(all.contains(&"bash".to_string()));
        assert!(all.contains(&"dynamic_tool".to_string()));
        let mut active = harness.session.get_active_tool_names();
        active.sort();
        assert_eq!(active, vec!["bash", "dynamic_tool", "edit", "write"]);
        assert!(!harness.session.system_prompt().contains("- read:"));
        assert!(!harness.session.system_prompt().contains("ask_question"));
        assert!(
            harness
                .session
                .system_prompt()
                .contains("- dynamic_tool: Run dynamic test behavior")
        );
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn lets_excluded_tools_override_the_allowlist() {
    run(async {
        let harness = create_harness(HarnessOptions {
            allowed_tool_names: Some(vec![
                "read".to_string(),
                "bash".to_string(),
                "ask_question".to_string(),
            ]),
            excluded_tool_names: Some(vec!["read".to_string(), "ask_question".to_string()]),
            initial_active_tool_names: Some(vec![
                "read".to_string(),
                "bash".to_string(),
                "ask_question".to_string(),
            ]),
            extension_factories: vec![tools_extension()],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;
        assert_eq!(
            tool_names(&harness.session.get_all_tools()),
            vec!["bash".to_string()]
        );
        assert_eq!(
            harness.session.get_active_tool_names(),
            vec!["bash".to_string()]
        );
        assert!(harness.session.system_prompt().contains("- bash:"));
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn matches_allowlist_and_denylist_patterns() {
    run(async {
        let harness = create_harness(HarnessOptions {
            allowed_tool_names: Some(vec![
                "*_tool".to_string(),
                "ask_*".to_string(),
                "re*".to_string(),
            ]),
            excluded_tool_names: Some(vec!["ask*".to_string()]),
            initial_active_tool_names: Some(vec![
                "*_tool".to_string(),
                "ask_*".to_string(),
                "re*".to_string(),
            ]),
            extension_factories: vec![tools_extension()],
            ..Default::default()
        })
        .await;
        harness.session.bind_extensions(Default::default()).await;
        assert_eq!(
            tool_names(&harness.session.get_all_tools()),
            vec!["dynamic_tool".to_string(), "read".to_string()]
        );
        let mut active = harness.session.get_active_tool_names();
        active.sort();
        assert_eq!(active, vec!["dynamic_tool", "read"]);
        harness.cleanup();
    })
    .await;
}
