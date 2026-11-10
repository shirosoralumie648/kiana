//! `core/tools/index.ts` 的移植测试。
//!
//! TS 参考：`test/tool-system-prompt-contributions.test.ts`（每个内置工具的定义与
//! contribution 对齐、session 环境提示条件化）、`test/builtin-tool-strict-mode.test.ts`
//! （严格采样集合与显式 opt-out 在 wrap 后保留）、以及工具集合的成员/键序。

mod search_shell_test_support;

use kiana_ai::types::ConstrainedSampling;
use kiana_tools::definition::wrap_tool_definition;
use kiana_tools::index::{
    ALL_TOOL_NAMES, ToolName, ToolNameError, ToolsOptions, create_all_tool_definitions,
    create_all_tools, create_coding_tool_definitions, create_coding_tools,
    create_read_only_tool_definitions, create_read_only_tools, create_tool, create_tool_definition,
};
use serde_json::json;

use search_shell_test_support::test_model;

fn contribution_guidelines(guidelines: &[&str]) -> Vec<String> {
    guidelines
        .iter()
        .map(|guideline| (*guideline).to_string())
        .collect()
}

#[test]
fn keeps_each_tool_definition_aligned_with_its_contribution() {
    let cases: Vec<(
        &str,
        kiana_ext_api::tool_types::ToolDefinition,
        String,
        Vec<String>,
    )> = vec![
        {
            let contribution = kiana_tools::read::read_tool_system_prompt_contribution();
            (
                "read",
                kiana_tools::read::create_read_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::bash::bash_tool_system_prompt_contribution();
            (
                "bash",
                kiana_tools::bash::create_bash_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution =
                kiana_tools::powershell::powershell_tool_system_prompt_contribution();
            (
                "powershell",
                kiana_tools::powershell::create_power_shell_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::edit::edit_tool_system_prompt_contribution();
            (
                "edit",
                kiana_tools::edit::create_edit_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::write::write_tool_system_prompt_contribution();
            (
                "write",
                kiana_tools::write::create_write_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::grep::grep_tool_system_prompt_contribution();
            (
                "grep",
                kiana_tools::grep::create_grep_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::find::find_tool_system_prompt_contribution();
            (
                "find",
                kiana_tools::find::create_find_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
        {
            let contribution = kiana_tools::ls::ls_tool_system_prompt_contribution();
            (
                "ls",
                kiana_tools::ls::create_ls_tool_definition("/workspace", None),
                contribution.snippet.to_string(),
                contribution_guidelines(contribution.guidelines),
            )
        },
    ];
    for (name, definition, snippet, guidelines) in cases {
        assert_eq!(definition.name, name);
        assert_eq!(definition.label, name);
        assert_eq!(
            definition.prompt_snippet.as_deref(),
            Some(snippet.as_str()),
            "{name}"
        );
        assert_eq!(
            // TS 测试是 `definition.promptGuidelines ?? []`：grep/find/ls 的定义里该属性是
            // `undefined`，与空数组比较相等。
            definition.prompt_guidelines.clone().unwrap_or_default(),
            guidelines,
            "{name}"
        );
    }
}

#[test]
fn keeps_session_environment_guidance_conditional() {
    for definition in [
        kiana_tools::bash::create_bash_tool_definition(
            "/workspace",
            Some(kiana_tools::bash::BashToolOptions {
                expose_session_environment: false,
                ..Default::default()
            }),
        ),
        kiana_tools::powershell::create_power_shell_tool_definition(
            "/workspace",
            Some(kiana_tools::powershell::PowerShellToolOptions {
                expose_session_environment: false,
                ..Default::default()
            }),
        ),
    ] {
        assert_eq!(definition.prompt_guidelines, None);
        assert!(definition.prompt_snippet.is_some());
    }
}

#[test]
fn prefers_strict_sampling_for_schema_eligible_tools() {
    let definitions = create_all_tool_definitions("/workspace", None);
    for name in [
        ToolName::Read,
        ToolName::Bash,
        ToolName::PowerShell,
        ToolName::Edit,
        ToolName::Write,
    ] {
        let definition = &definitions[&name];
        assert_eq!(
            serde_json::to_value(definition.constrained_sampling.as_ref()).expect("json"),
            json!({ "type": "json_schema", "strict": "prefer" }),
            "{}",
            name
        );
    }
    for name in [ToolName::Grep, ToolName::Find, ToolName::Ls] {
        assert_eq!(definitions[&name].constrained_sampling, None, "{name}");
    }
    // 严格性是 provider 侧转换，不改变执行 schema。
    assert_eq!(
        definitions[&ToolName::Read].parameters["required"],
        json!(["path"])
    );
    assert_eq!(
        definitions[&ToolName::Bash].parameters["required"],
        json!(["command"])
    );
}

#[test]
fn preserves_explicit_opt_outs_when_wrapping_definitions_for_execution() {
    let definitions = create_all_tool_definitions("/workspace", None);
    for name in [
        ToolName::Read,
        ToolName::Bash,
        ToolName::PowerShell,
        ToolName::Edit,
        ToolName::Write,
    ] {
        let definition = &definitions[&name];
        let mut override_definition = definition.clone();
        override_definition.constrained_sampling = Some(ConstrainedSampling::Disabled(false));
        let tool = wrap_tool_definition(override_definition, None);
        assert_eq!(
            tool.constrained_sampling,
            Some(ConstrainedSampling::Disabled(false)),
            "{name}"
        );
        assert!(definition.constrained_sampling.is_some());
    }
}

#[test]
fn tool_sets_and_key_order_match_ts() {
    let all = create_all_tool_definitions("/workspace", None);
    let names: Vec<&str> = all.keys().map(|name| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "read",
            "bash",
            "powershell",
            "edit",
            "write",
            "grep",
            "find",
            "ls"
        ]
    );

    let coding: Vec<String> = create_coding_tool_definitions("/workspace", None)
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert_eq!(
        coding,
        ["read", "bash", "edit", "write"].map(str::to_string)
    );

    let read_only: Vec<String> = create_read_only_tool_definitions("/workspace", None)
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert_eq!(
        read_only,
        ["read", "grep", "find", "ls"].map(str::to_string)
    );

    // coding / read-only 故意不含 powershell（TS 的既有不一致）。
    assert!(!coding.iter().any(|name| name == "powershell"));
    assert!(!read_only.iter().any(|name| name == "powershell"));

    let all_tools = create_all_tools("/workspace", None);
    assert_eq!(
        all_tools
            .keys()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        names
    );
    assert_eq!(
        create_coding_tools("/workspace", None)
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["read", "bash", "edit", "write"]
    );
    assert_eq!(
        create_read_only_tools("/workspace", None)
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["read", "grep", "find", "ls"]
    );
}

#[test]
fn create_tool_definition_and_create_tool_accept_every_builtin_name() {
    for name in ALL_TOOL_NAMES {
        let definition = create_tool_definition(name.as_str(), "/workspace", None).expect("known");
        assert_eq!(definition.name, name.as_str());
        let tool = create_tool(name.as_str(), "/workspace", None).expect("known");
        assert_eq!(tool.name, name.as_str());
        assert_eq!(name.to_string(), name.as_str());
        assert_eq!(name.as_str().parse::<ToolName>(), Ok(name));
    }
}

#[test]
fn unknown_tool_names_are_runtime_errors_with_the_ts_message() {
    for name in ["nope", "", "Bash", "read "] {
        let error = create_tool_definition(name, "/workspace", None).expect_err("unknown");
        assert_eq!(
            error,
            ToolNameError {
                name: name.to_string()
            }
        );
        assert_eq!(error.to_string(), format!("Unknown tool name: {name}"));
        let error = create_tool(name, "/workspace", None).expect_err("unknown");
        assert_eq!(error.to_string(), format!("Unknown tool name: {name}"));
    }
}

#[test]
fn options_are_threaded_to_the_matching_tool_only() {
    let options = ToolsOptions {
        bash: Some(kiana_tools::bash::BashToolOptions {
            command_prefix: Some("echo prefixed".to_string()),
            expose_session_environment: false,
            ..Default::default()
        }),
        ..Default::default()
    };
    let definition =
        create_tool_definition("bash", "/workspace", Some(options.clone())).expect("bash");
    assert_eq!(
        definition.description,
        kiana_tools::bash::create_bash_tool_definition(
            "/workspace",
            Some(kiana_tools::bash::BashToolOptions {
                command_prefix: Some("echo prefixed".to_string()),
                expose_session_environment: false,
                ..Default::default()
            }),
        )
        .description
    );
    let grep = create_tool_definition("grep", "/workspace", Some(options)).expect("grep");
    assert_eq!(grep.name, "grep");
}

#[test]
fn model_helper_is_available_for_shared_support() {
    // 共享测试支持的 smoke：index 测试不需要执行，只确认模型构造可用。
    let model = test_model("anthropic", "claude-sonnet-4-5");
    assert_eq!(model.provider, "anthropic");
}

#[test]
fn crate_root_exports_bash_operations_like_index_ts() {
    // TS 的 `core/tools/index.ts` 显式 `export { type BashOperations } from "./bash.ts"`；
    // crate 根（`pub use index::*`）必须同样可用，否则按 index.ts 写代码的下游 crate 编译失败。
    fn assert_trait<T: ?Sized>() {}
    assert_trait::<dyn kiana_tools::BashOperations>();
    let creator: fn(Option<&str>) -> std::sync::Arc<dyn kiana_tools::BashOperations> =
        kiana_tools::create_local_bash_operations;
    let _ = creator;
}
