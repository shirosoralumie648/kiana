//! `packages/coding-agent/test/mcp-extension.test.ts` 的 `MCP servers section` 三组测试，
//! 外加 `/mcp` 命令的注册与参数补全（`mcp/index.ts` 的 `registerCommand` 面）。
//!
//! 该文件的其余 describe 属于 `mcp-core`（config/tools）与 `mcp-runtime-cli`
//! （connections）；本单元只移植直接依赖 `renderServersSection` 的这一组。

use std::sync::Arc;

use kiana_builtin_extensions::mcp::config::{
    McpExposure, McpServerConfig, McpServerEntry, McpStdioServerConfig,
};
use kiana_builtin_extensions::mcp::index::{
    MAX_SERVERS_SECTION_CHARS, MCP_SERVERS_SECTION, McpExtensionOptions, McpServerConnectionInfo,
    McpServerListing, create_mcp_extension, render_servers_section,
};
use kiana_core::mcp_servers::McpServerConfigBase;
use kiana_ext_api::types::ExtensionApi;

#[path = "mcp_extension_support/mod.rs"]
mod mcp_extension_support;

use mcp_extension_support::FakeExtensionApi;

/// TS 测试里的 `server(name, description?, exposure?)`，可附 instructions。
fn server(
    name: &str,
    description: Option<&str>,
    exposure: Option<McpExposure>,
    instructions: Option<&str>,
) -> McpServerListing {
    let config = McpStdioServerConfig {
        base: McpServerConfigBase {
            description: description.map(str::to_string),
            exposure,
            ..McpServerConfigBase::default()
        },
        command: "x".to_string(),
        ..McpStdioServerConfig::default()
    };
    McpServerListing {
        entry: McpServerEntry {
            name: name.to_string(),
            config: McpServerConfig::Stdio(config),
            source: "test".to_string(),
            scope: None,
            override_: None,
        },
        connection: instructions.map(|instructions| McpServerConnectionInfo {
            instructions: Some(instructions.to_string()),
        }),
    }
}

#[test]
fn lists_servers_with_how_their_tools_are_reached_and_the_first_line_of_their_description() {
    let section = render_servers_section(&[
        server("docs", Some("Docs search.\nMore."), None, None),
        server("later", None, Some(McpExposure::Deferred), None),
        server("direct", Some("Declared."), Some(McpExposure::Direct), None),
        server("plain", None, None, Some("From instructions.")),
    ])
    .expect("section");
    let lines: Vec<&str> = section.split('\n').skip(1).collect();
    assert_eq!(
        lines,
        vec![
            "- mcp__docs (codemode): Docs search.",
            "- mcp__later (tool_search)",
            "- mcp__plain (codemode): From instructions.",
        ]
    );
    assert_eq!(
        section.split('\n').next(),
        Some(
            "MCP servers whose tools are not declared to you. Call the tools of `codemode` servers from codemode scripts. Load the tools of `tool_search` servers with `tool_search`."
        )
    );
    assert_eq!(MCP_SERVERS_SECTION, "mcp_servers");
    assert!(
        render_servers_section(&[server(
            "direct",
            Some("Declared."),
            Some(McpExposure::Direct),
            None
        )])
        .is_none()
    );
}

#[test]
fn shortens_descriptions_to_fit_the_size_limit() {
    let servers: Vec<McpServerListing> = (0..40)
        .map(|index| {
            server(
                &format!("server{index}"),
                Some(&"x".repeat(400)),
                None,
                None,
            )
        })
        .collect();
    let section = render_servers_section(&servers).expect("section");
    assert!(section.encode_utf16().count() <= MAX_SERVERS_SECTION_CHARS);
    assert_eq!(section.split('\n').count(), 41);
    assert!(
        section.contains("- mcp__server39 (codemode): x"),
        "{section}"
    );
}

#[test]
fn leaves_out_the_last_servers_when_their_names_alone_do_not_fit() {
    let servers: Vec<McpServerListing> = (0..200)
        .map(|index| {
            server(
                &format!("server-with-a-long-name-{index}"),
                Some("desc"),
                None,
                None,
            )
        })
        .collect();
    let section = render_servers_section(&servers).expect("section");
    assert!(section.encode_utf16().count() <= MAX_SERVERS_SECTION_CHARS);
    let lines: Vec<&str> = section.split('\n').collect();
    let last = *lines.last().expect("last line");
    let prefix = "- … ";
    let suffix = " more servers; find their tools with searchTools()";
    assert!(
        last.starts_with(prefix) && last.ends_with(suffix),
        "unexpected last line: {last}"
    );
    let omitted: usize = last[prefix.len()..last.len() - suffix.len()]
        .parse()
        .expect("omitted count");
    assert_eq!(lines.len() - 2 + omitted, 200);
}

/// `/mcp` 命令注册面：描述逐字、`getArgumentCompletions` 的三种前缀分支（动作、带服务器、
/// 多余参数）。
#[test]
#[allow(clippy::arc_with_non_send_sync)] // 假 ExtensionApi 故意是单线程的（ExtensionApi 本身 !Send）。
fn registers_the_mcp_command_with_argument_completions() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async {
        let api = Arc::new(FakeExtensionApi::new());
        create_mcp_extension(McpExtensionOptions::default())(api.clone() as Arc<dyn ExtensionApi>)
            .await;
        let (name, description, completions) = {
            let commands = api.commands.borrow();
            let (name, options) = commands.first().expect("mcp command registered");
            (
                name.clone(),
                options.description.clone(),
                options
                    .get_argument_completions
                    .clone()
                    .expect("getArgumentCompletions"),
            )
        };
        assert_eq!(name, "mcp");
        assert_eq!(
            description.as_deref(),
            Some("Manage MCP servers: sign in, reconnect, enable or disable, and change exposure")
        );

        let actions = completions("lo".to_string())
            .await
            .expect("action completions");
        assert_eq!(
            actions
                .iter()
                .map(|item| (item.value.as_str(), item.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("login ", "login"), ("logout ", "logout")]
        );

        // 没有服务器时，带服务器名的前缀没有可补全项。
        assert!(completions("login d".to_string()).await.is_none());
        // 两个以上的位置参数不属于补全范围。
        assert!(completions("login a b".to_string()).await.is_none());
    }));
}
