//! `packages/coding-agent/test/args.test.ts` 的逐条移植。
//!
//! 标题保持原文（snake_case 形式），describe 分块用注释保留。

use kiana_coding_agent::cli::args::{
    Args, Diagnostic, DiagnosticKind, ListModelsArg, Mode, VALID_THINKING_LEVELS,
    help_text_with_style, is_valid_thinking_level, normalize_session_name, parse_args,
    print_help_with_style,
};
use kiana_coding_agent::cli::setup::{Output, Style};
use kiana_config::settings::TuiMode;
use kiana_ext_api::types::{ExtensionFlag, FlagType, FlagValue};
use kiana_permissions::PermissionMode;

fn parse(args: &[&str]) -> Args {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    parse_args(&args)
}

fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::Error,
        message: message.into(),
    }
}

fn warning(message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::Warning,
        message: message.into(),
    }
}

// describe("--version flag")
#[test]
fn parses_version_flag() {
    assert!(parse(&["--version"]).version);
}

#[test]
fn parses_v_shorthand() {
    assert!(parse(&["-v"]).version);
}

#[test]
fn version_takes_precedence_over_other_args() {
    let result = parse(&["--version", "--help", "some message"]);
    assert!(result.version);
    assert!(result.help);
    assert!(result.messages.contains(&"some message".to_string()));
}

// describe("--help flag")
#[test]
fn parses_help_flag() {
    assert!(parse(&["--help"]).help);
}

#[test]
fn parses_h_shorthand() {
    assert!(parse(&["-h"]).help);
}

// describe("--print flag")
#[test]
fn parses_print_flag() {
    assert!(parse(&["--print"]).print);
}

#[test]
fn parses_p_shorthand() {
    assert!(parse(&["-p"]).print);
}

#[test]
fn parses_prompt_after_p_even_when_it_starts_with_yaml_frontmatter() {
    let prompt = "---\ntitle: hello\n---\nSay hi.";
    let result = parse(&["-p", prompt]);
    assert!(result.print);
    assert_eq!(result.messages, vec![prompt.to_string()]);
    assert_eq!(result.unknown_flags.len(), 0);
}

#[test]
fn does_not_consume_options_after_p_as_prompts() {
    let result = parse(&["-p", "--provider", "openai", "Say hi."]);
    assert!(result.print);
    assert_eq!(result.provider.as_deref(), Some("openai"));
    assert_eq!(result.messages, vec!["Say hi.".to_string()]);
}

// describe("--continue flag")
#[test]
fn parses_continue_flag() {
    assert!(parse(&["--continue"]).r#continue);
}

#[test]
fn parses_c_shorthand() {
    assert!(parse(&["-c"]).r#continue);
}

// describe("--resume flag")
#[test]
fn parses_resume_flag() {
    assert!(parse(&["--resume"]).resume);
}

#[test]
fn parses_r_shorthand() {
    assert!(parse(&["-r"]).resume);
}

// describe("flags with values")
#[test]
fn parses_provider() {
    assert_eq!(
        parse(&["--provider", "openai"]).provider.as_deref(),
        Some("openai")
    );
}

#[test]
fn parses_model() {
    assert_eq!(
        parse(&["--model", "gpt-4o"]).model.as_deref(),
        Some("gpt-4o")
    );
}

#[test]
fn parses_api_key() {
    assert_eq!(
        parse(&["--api-key", "sk-test-key"]).api_key.as_deref(),
        Some("sk-test-key")
    );
}

#[test]
fn parses_system_prompt() {
    assert_eq!(
        parse(&["--system-prompt", "You are a helpful assistant"])
            .system_prompt
            .as_deref(),
        Some("You are a helpful assistant")
    );
}

#[test]
fn parses_append_system_prompt() {
    assert_eq!(
        parse(&["--append-system-prompt", "Additional context"]).append_system_prompt,
        Some(vec!["Additional context".to_string()])
    );
}

#[test]
fn parses_multiple_append_system_prompt_flags() {
    assert_eq!(
        parse(&[
            "--append-system-prompt",
            "Context A",
            "--append-system-prompt",
            "Context B"
        ])
        .append_system_prompt,
        Some(vec!["Context A".to_string(), "Context B".to_string()])
    );
}

#[test]
fn parses_session() {
    assert_eq!(
        parse(&["--session", "/path/to/session.jsonl"])
            .session
            .as_deref(),
        Some("/path/to/session.jsonl")
    );
}

#[test]
fn parses_session_id() {
    assert_eq!(
        parse(&["--session-id", "orchestrated-session"])
            .session_id
            .as_deref(),
        Some("orchestrated-session")
    );
}

#[test]
fn parses_fork() {
    let result = parse(&["--fork", "1234abcd"]);
    assert_eq!(result.fork.as_deref(), Some("1234abcd"));
    assert_eq!(result.messages, Vec::<String>::new());
}

#[test]
fn parses_export() {
    assert_eq!(
        parse(&["--export", "session.jsonl"]).export.as_deref(),
        Some("session.jsonl")
    );
}

#[test]
fn parses_thinking() {
    assert_eq!(
        parse(&["--thinking", "high"]).thinking,
        Some(kiana_agent_core::types::ThinkingLevel::High)
    );
}

#[test]
fn reports_an_invalid_thinking_level_as_a_warning() {
    let result = parse(&["--thinking", "extreme"]);
    assert_eq!(result.thinking, None);
    assert_eq!(
        result.diagnostics,
        vec![warning(
            "Invalid thinking level \"extreme\". Valid values: off, minimal, low, medium, high, xhigh, max"
        )]
    );
}

#[test]
fn parses_models_as_comma_separated_list() {
    assert_eq!(
        parse(&["--models", "gpt-4o,claude-sonnet,gemini-pro"]).models,
        Some(vec![
            "gpt-4o".to_string(),
            "claude-sonnet".to_string(),
            "gemini-pro".to_string()
        ])
    );
}

// Issue #10334
#[test]
fn ignores_empty_entries_in_models() {
    assert_eq!(
        parse(&["--models", "gpt-4o, ,claude-sonnet,"]).models,
        Some(vec!["gpt-4o".to_string(), "claude-sonnet".to_string()])
    );
}

// Issue #9045 — describe("--mode flag")
#[test]
fn parses_mode_text() {
    let result = parse(&["--mode", "text"]);
    assert_eq!(result.mode, Some(Mode::Text));
    assert_eq!(result.diagnostics, Vec::<Diagnostic>::new());
}

#[test]
fn parses_mode_json() {
    let result = parse(&["--mode", "json"]);
    assert_eq!(result.mode, Some(Mode::Json));
    assert_eq!(result.diagnostics, Vec::<Diagnostic>::new());
}

#[test]
fn parses_mode_rpc() {
    let result = parse(&["--mode", "rpc"]);
    assert_eq!(result.mode, Some(Mode::Rpc));
    assert_eq!(result.diagnostics, Vec::<Diagnostic>::new());
}

#[test]
fn rejects_invalid_mode_value_yaml() {
    let result = parse(&["--mode", "yaml", "--version"]);
    assert_eq!(result.mode, None);
    assert!(result.version);
    assert_eq!(result.messages, Vec::<String>::new());
    assert_eq!(result.unknown_flags.len(), 0);
    assert_eq!(
        result.diagnostics,
        vec![error(
            "Invalid mode \"yaml\". Valid values: text, json, rpc"
        )]
    );
}

#[test]
fn rejects_invalid_mode_value_empty() {
    let result = parse(&["--mode", "", "--version"]);
    assert_eq!(result.mode, None);
    assert!(result.version);
    assert_eq!(
        result.diagnostics,
        vec![error("Invalid mode \"\". Valid values: text, json, rpc")]
    );
}

#[test]
fn reports_a_missing_mode_value() {
    let result = parse(&["--mode"]);
    assert_eq!(result.mode, None);
    assert_eq!(result.unknown_flags.len(), 0);
    assert_eq!(
        result.diagnostics,
        vec![error("--mode requires text, json, or rpc")]
    );
}

#[test]
fn does_not_consume_another_option_as_a_mode_value() {
    let result = parse(&["--mode", "--version"]);
    assert_eq!(result.mode, None);
    assert!(result.version);
    assert_eq!(result.unknown_flags.len(), 0);
    assert_eq!(
        result.diagnostics,
        vec![error("--mode requires text, json, or rpc")]
    );
}

#[test]
fn reports_an_invalid_mode_value_after_a_valid_one() {
    let result = parse(&["--mode", "json", "--mode", "yaml"]);
    assert_eq!(
        result.diagnostics,
        vec![error(
            "Invalid mode \"yaml\". Valid values: text, json, rpc"
        )]
    );
}

// describe("--name flag")
#[test]
fn parses_name_flag_with_value() {
    assert_eq!(
        parse(&["--name", "my-session"]).name.as_deref(),
        Some("my-session")
    );
}

#[test]
fn parses_n_shorthand() {
    assert_eq!(
        parse(&["-n", "quick-session"]).name.as_deref(),
        Some("quick-session")
    );
}

#[test]
fn preserves_empty_values_for_main_validation() {
    assert_eq!(parse(&["--name", ""]).name.as_deref(), Some(""));
}

#[test]
fn normalizes_display_names_and_rejects_whitespace_only_values() {
    assert_eq!(
        normalize_session_name("  named session  ").as_deref(),
        Some("named session")
    );
    assert_eq!(normalize_session_name("   "), None);
}

#[test]
fn reports_missing_value() {
    assert_eq!(
        parse(&["--name"]).diagnostics,
        vec![error("--name requires a value")]
    );
}

#[test]
fn name_works_alongside_other_flags() {
    let result = parse(&[
        "--name",
        "named-run",
        "--print",
        "--model",
        "gpt-4o",
        "hello",
    ]);
    assert_eq!(result.name.as_deref(), Some("named-run"));
    assert!(result.print);
    assert_eq!(result.model.as_deref(), Some("gpt-4o"));
    assert_eq!(result.messages, vec!["hello".to_string()]);
}

// describe("--no-session flag")
#[test]
fn parses_no_session_flag() {
    assert!(parse(&["--no-session"]).no_session);
}

#[test]
fn preserves_custom_session_ids_for_non_persisting_commands() {
    let help = parse(&["--session-id", "ephemeral-id", "--help"]);
    assert_eq!(help.session_id.as_deref(), Some("ephemeral-id"));
    assert!(help.help);
    let list = parse(&["--session-id", "ephemeral-id", "--list-models"]);
    assert_eq!(list.session_id.as_deref(), Some("ephemeral-id"));
    assert_eq!(list.list_models, Some(ListModelsArg::True));
    let no_session = parse(&["--session-id", "ephemeral-id", "--no-session"]);
    assert_eq!(no_session.session_id.as_deref(), Some("ephemeral-id"));
    assert!(no_session.no_session);
}

// describe("--extension flag")
#[test]
fn parses_single_extension() {
    assert_eq!(
        parse(&["--extension", "./my-extension.ts"]).extensions,
        Some(vec!["./my-extension.ts".to_string()])
    );
}

#[test]
fn parses_e_shorthand() {
    assert_eq!(
        parse(&["-e", "./my-extension.ts"]).extensions,
        Some(vec!["./my-extension.ts".to_string()])
    );
}

#[test]
fn parses_multiple_extension_flags() {
    assert_eq!(
        parse(&["--extension", "./ext1.ts", "-e", "./ext2.ts"]).extensions,
        Some(vec!["./ext1.ts".to_string(), "./ext2.ts".to_string()])
    );
}

// describe("--no-extensions flag")
#[test]
fn parses_no_extensions_flag() {
    assert!(parse(&["--no-extensions"]).no_extensions);
}

#[test]
fn parses_no_extensions_with_explicit_e_flags() {
    let result = parse(&["--no-extensions", "-e", "foo.ts", "-e", "bar.ts"]);
    assert!(result.no_extensions);
    assert_eq!(
        result.extensions,
        Some(vec!["foo.ts".to_string(), "bar.ts".to_string()])
    );
}

// describe("--no-mcp flag")
#[test]
fn parses_no_mcp_flag() {
    let result = parse(&["--no-mcp"]);
    assert!(result.no_mcp);
    assert_eq!(result.unknown_flags.len(), 0);
}

// describe("--skill flag")
#[test]
fn parses_single_skill() {
    assert_eq!(
        parse(&["--skill", "./skill-dir"]).skills,
        Some(vec!["./skill-dir".to_string()])
    );
}

#[test]
fn parses_multiple_skill_flags() {
    assert_eq!(
        parse(&["--skill", "./skill-a", "--skill", "./skill-b"]).skills,
        Some(vec!["./skill-a".to_string(), "./skill-b".to_string()])
    );
}

// describe("--prompt-template flag")
#[test]
fn parses_single_prompt_template() {
    assert_eq!(
        parse(&["--prompt-template", "./prompts"]).prompt_templates,
        Some(vec!["./prompts".to_string()])
    );
}

#[test]
fn parses_multiple_prompt_template_flags() {
    assert_eq!(
        parse(&["--prompt-template", "./one", "--prompt-template", "./two"]).prompt_templates,
        Some(vec!["./one".to_string(), "./two".to_string()])
    );
}

// describe("--theme flag")
#[test]
fn parses_single_theme() {
    assert_eq!(
        parse(&["--theme", "./theme.json"]).themes,
        Some(vec!["./theme.json".to_string()])
    );
}

#[test]
fn parses_multiple_theme_flags() {
    assert_eq!(
        parse(&["--theme", "./dark.json", "--theme", "./light.json"]).themes,
        Some(vec!["./dark.json".to_string(), "./light.json".to_string()])
    );
}

// describe("--use-theme flag")
#[test]
fn parses_use_theme() {
    assert_eq!(
        parse(&["--use-theme", "light"]).use_theme.as_deref(),
        Some("light")
    );
}

#[test]
fn reports_when_the_theme_name_value_is_missing() {
    let result = parse(&["--use-theme", "--print"]);
    assert_eq!(result.use_theme, None);
    assert!(result.print);
    assert_eq!(
        result.diagnostics,
        vec![error("--use-theme requires a theme name")]
    );
}

// describe("--no-skills flag")
#[test]
fn parses_no_skills_flag() {
    assert!(parse(&["--no-skills"]).no_skills);
}

// describe("--no-prompt-templates flag")
#[test]
fn parses_no_prompt_templates_flag() {
    assert!(parse(&["--no-prompt-templates"]).no_prompt_templates);
}

// describe("--no-themes flag")
#[test]
fn parses_no_themes_flag() {
    assert!(parse(&["--no-themes"]).no_themes);
}

// describe("--no-context-files flag")
#[test]
fn parses_no_context_files_flag() {
    assert!(parse(&["--no-context-files"]).no_context_files);
}

#[test]
fn parses_nc_shorthand() {
    assert!(parse(&["-nc"]).no_context_files);
}

// describe("project approval flags")
#[test]
fn parses_approve() {
    assert_eq!(parse(&["--approve"]).project_trust_override, Some(true));
}

#[test]
fn parses_a_shorthand() {
    assert_eq!(parse(&["-a"]).project_trust_override, Some(true));
}

#[test]
fn parses_no_approve() {
    assert_eq!(parse(&["--no-approve"]).project_trust_override, Some(false));
}

#[test]
fn parses_na_shorthand() {
    assert_eq!(parse(&["-na"]).project_trust_override, Some(false));
}

// describe("--verbose flag")
#[test]
fn parses_verbose_flag() {
    assert!(parse(&["--verbose"]).verbose);
}

// describe("--offline flag")
#[test]
fn parses_offline_flag() {
    assert!(parse(&["--offline"]).offline);
}

// describe("--tui-mode flag")
#[test]
fn parses_regular_mode() {
    assert_eq!(
        parse(&["--tui-mode", "regular"]).tui_mode,
        Some(TuiMode::Regular)
    );
}

#[test]
fn parses_fullscreen_mode() {
    assert_eq!(
        parse(&["--tui-mode", "fullscreen"]).tui_mode,
        Some(TuiMode::Fullscreen)
    );
}

#[test]
fn rejects_invalid_modes() {
    assert_eq!(
        parse(&["--tui-mode", "other"]).diagnostics,
        vec![error(
            "Invalid TUI mode \"other\". Valid values: regular, fullscreen"
        )]
    );
}

#[test]
fn requires_a_mode() {
    assert_eq!(
        parse(&["--tui-mode"]).diagnostics,
        vec![error("--tui-mode requires regular or fullscreen")]
    );
}

#[test]
fn does_not_recognize_the_old_ui_mode_flag() {
    let result = parse(&["--ui-mode", "fullscreen"]);
    assert_eq!(result.tui_mode, None);
    assert_eq!(
        result.unknown_flags.get("ui-mode"),
        Some(&FlagValue::Str("fullscreen".to_string()))
    );
}

// describe("tool flags")
#[test]
fn parses_no_tools_flag() {
    assert!(parse(&["--no-tools"]).no_tools);
}

#[test]
fn parses_nt_shorthand() {
    assert!(parse(&["-nt"]).no_tools);
}

#[test]
fn parses_no_builtin_tools_flag() {
    assert!(parse(&["--no-builtin-tools"]).no_builtin_tools);
}

#[test]
fn parses_nbt_shorthand() {
    assert!(parse(&["-nbt"]).no_builtin_tools);
}

#[test]
fn parses_tools_flag() {
    assert_eq!(
        parse(&["--tools", "read,bash"]).tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

#[test]
fn parses_t_shorthand() {
    assert_eq!(
        parse(&["-t", "read,bash"]).tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

#[test]
fn parses_plus_name_and_minus_name_tool_modifiers() {
    let result = parse(&["-t", "+codemode,-write"]);
    assert_eq!(
        result.tools,
        Some(vec!["+codemode".to_string(), "-write".to_string()])
    );
    assert_eq!(result.diagnostics, Vec::<Diagnostic>::new());
}

#[test]
fn rejects_tool_names_mixed_with_modifiers() {
    let result = parse(&["--tools", "read,+codemode"]);
    assert_eq!(result.tools, None);
    assert_eq!(
        result.diagnostics,
        vec![error(
            "--tools: tool names cannot be mixed with +name or -name entries"
        )]
    );
}

#[test]
fn rejects_patterns_in_tool_modifiers() {
    let result = parse(&["-t", "+mcp__radius__*"]);
    assert_eq!(result.tools, None);
    assert_eq!(
        result.diagnostics,
        vec![error(
            "-t: +name and -name entries take exact tool names, not patterns: +mcp__radius__*"
        )]
    );
}

#[test]
fn parses_exclude_tools_flag() {
    assert_eq!(
        parse(&["--exclude-tools", "read,bash"]).exclude_tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

#[test]
fn parses_xt_shorthand() {
    assert_eq!(
        parse(&["-xt", "read,bash"]).exclude_tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

#[test]
fn parses_no_tools_with_explicit_tools_flags() {
    let result = parse(&["--no-tools", "--tools", "read,bash"]);
    assert!(result.no_tools);
    assert_eq!(
        result.tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

#[test]
fn parses_no_builtin_tools_with_explicit_tools_flags() {
    let result = parse(&["--no-builtin-tools", "--tools", "read,bash"]);
    assert!(result.no_builtin_tools);
    assert_eq!(
        result.tools,
        Some(vec!["read".to_string(), "bash".to_string()])
    );
}

// describe("messages and file args")
#[test]
fn parses_plain_text_messages() {
    assert_eq!(
        parse(&["hello", "world"]).messages,
        vec!["hello".to_string(), "world".to_string()]
    );
}

#[test]
fn parses_file_arguments() {
    assert_eq!(
        parse(&["@README.md", "@src/main.ts"]).file_args,
        vec!["README.md".to_string(), "src/main.ts".to_string()]
    );
}

#[test]
fn parses_mixed_messages_and_file_args() {
    let result = parse(&["@file.txt", "explain this", "@image.png"]);
    assert_eq!(
        result.file_args,
        vec!["file.txt".to_string(), "image.png".to_string()]
    );
    assert_eq!(result.messages, vec!["explain this".to_string()]);
}

#[test]
fn captures_unknown_long_flags_with_string_values() {
    let result = parse(&["--unknown-flag", "message"]);
    assert_eq!(result.messages, Vec::<String>::new());
    assert_eq!(
        result.unknown_flags.get("unknown-flag"),
        Some(&FlagValue::Str("message".to_string()))
    );
}

#[test]
fn captures_unknown_boolean_long_flags() {
    assert_eq!(
        parse(&["--unknown-flag"]).unknown_flags.get("unknown-flag"),
        Some(&FlagValue::Bool(true))
    );
}

#[test]
fn captures_unknown_long_flags_with_equals_syntax() {
    assert_eq!(
        parse(&["--unknown-flag=value"])
            .unknown_flags
            .get("unknown-flag"),
        Some(&FlagValue::Str("value".to_string()))
    );
}

// describe("complex combinations")
#[test]
fn parses_multiple_flags_together() {
    let result = parse(&[
        "--provider",
        "anthropic",
        "--model",
        "claude-sonnet",
        "--print",
        "--thinking",
        "high",
        "@prompt.md",
        "Do the task",
    ]);
    assert_eq!(result.provider.as_deref(), Some("anthropic"));
    assert_eq!(result.model.as_deref(), Some("claude-sonnet"));
    assert!(result.print);
    assert_eq!(
        result.thinking,
        Some(kiana_agent_core::types::ThinkingLevel::High)
    );
    assert_eq!(result.file_args, vec!["prompt.md".to_string()]);
    assert_eq!(result.messages, vec!["Do the task".to_string()]);
}

// ---------------------------------------------------------------------------
// Audit §A.1：parseArgs 边角行为
// ---------------------------------------------------------------------------

#[test]
fn audit_unknown_flag_with_empty_equals_value() {
    assert_eq!(
        parse(&["--unknown-flag="])
            .unknown_flags
            .get("unknown-flag"),
        Some(&FlagValue::Str(String::new()))
    );
}

#[test]
fn audit_unknown_equals_split_applies_to_any_long_option() {
    assert_eq!(
        parse(&["--=x"]).unknown_flags.get(""),
        Some(&FlagValue::Str("x".to_string()))
    );
}

#[test]
fn audit_single_dash_and_multi_char_short_options_are_whole_token_errors() {
    assert_eq!(parse(&["-"]).diagnostics, vec![error("Unknown option: -")]);
    assert_eq!(
        parse(&["-abc"]).diagnostics,
        vec![error("Unknown option: -abc")]
    );
}

#[test]
fn audit_unknown_long_option_does_not_consume_at_or_dash_values() {
    let at = parse(&["--foo", "@bar"]);
    assert_eq!(at.unknown_flags.get("foo"), Some(&FlagValue::Bool(true)));
    assert_eq!(at.file_args, vec!["bar".to_string()]);

    let dash = parse(&["--foo", "-1"]);
    assert_eq!(dash.unknown_flags.get("foo"), Some(&FlagValue::Bool(true)));
    assert_eq!(dash.diagnostics, vec![error("Unknown option: -1")]);
}

#[test]
fn audit_name_consumes_a_leading_dash_value() {
    let result = parse(&["-n", "--foo"]);
    assert_eq!(result.name.as_deref(), Some("--foo"));
    assert_eq!(result.unknown_flags.len(), 0);
}

#[test]
fn audit_print_treats_triple_dash_as_message() {
    let result = parse(&["-p", "---x"]);
    assert!(result.print);
    assert_eq!(result.messages, vec!["---x".to_string()]);
}

#[test]
fn audit_invalid_tui_mode_consumes_the_value() {
    let result = parse(&["--tui-mode", "other", "--version"]);
    assert!(result.version);
    assert_eq!(
        result.diagnostics,
        vec![error(
            "Invalid TUI mode \"other\". Valid values: regular, fullscreen"
        )]
    );
}

#[test]
fn audit_trailing_value_options_fall_into_unknown_long_branch() {
    let model = parse(&["--model"]);
    assert_eq!(model.model, None);
    assert_eq!(
        model.unknown_flags.get("model"),
        Some(&FlagValue::Bool(true))
    );

    let provider = parse(&["--provider"]);
    assert_eq!(
        provider.unknown_flags.get("provider"),
        Some(&FlagValue::Bool(true))
    );

    let tools = parse(&["--tools"]);
    assert_eq!(tools.tools, None);
    assert_eq!(
        tools.unknown_flags.get("tools"),
        Some(&FlagValue::Bool(true))
    );

    let thinking = parse(&["--thinking"]);
    assert_eq!(thinking.thinking, None);
    assert_eq!(
        thinking.unknown_flags.get("thinking"),
        Some(&FlagValue::Bool(true))
    );

    let short_tools = parse(&["-t"]);
    assert_eq!(short_tools.diagnostics, vec![error("Unknown option: -t")]);
}

#[test]
fn audit_tools_error_leaves_tools_unset() {
    let result = parse(&["--tools", "+a,+b,*"]);
    assert_eq!(result.tools, None);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, DiagnosticKind::Error);
}

// ---------------------------------------------------------------------------
// 帮助文本（tests/golden/*.txt 由 TS 参考实现生成）
// ---------------------------------------------------------------------------

#[test]
fn help_matches_ts_cli_golden() {
    assert_eq!(
        help_text_with_style(None, Style::plain()),
        include_str!("golden/help.txt")
    );
}

#[test]
fn empty_extension_flags_and_none_are_identical() {
    assert_eq!(
        help_text_with_style(Some(&[]), Style::plain()),
        help_text_with_style(None, Style::plain())
    );
}

#[test]
fn help_with_extensions_matches_ts_golden() {
    let flags = vec![
        ExtensionFlag {
            name: "plan".to_string(),
            description: Some("Enter plan mode".to_string()),
            r#type: FlagType::Boolean,
            default: None,
            extension_path: "/home/user/.kiana/agent/extensions/plan.ts".to_string(),
        },
        ExtensionFlag {
            name: "team".to_string(),
            description: None,
            r#type: FlagType::String,
            default: None,
            extension_path: "builtin:teams".to_string(),
        },
    ];
    assert_eq!(
        help_text_with_style(Some(&flags), Style::plain()),
        include_str!("golden/help_with_extensions.txt")
    );
}

#[test]
fn print_help_writes_the_same_bytes() {
    // golden 是纯文本；测试必须固定样式，否则进程在 TTY / FORCE_COLOR=1 下会输出 ANSI
    // 序列（产品行为正确，但测试会随环境失败）。
    let output = Output::capture();
    print_help_with_style(None, Style::plain(), &output);
    assert_eq!(output.captured_stdout(), include_str!("golden/help.txt"));
}

// ---------------------------------------------------------------------------
// thinking / 会话名
// ---------------------------------------------------------------------------

#[test]
fn thinking_levels_cover_all_seven_values() {
    for level in VALID_THINKING_LEVELS {
        assert!(is_valid_thinking_level(level), "{level}");
    }
    assert!(!is_valid_thinking_level(""));
    assert!(!is_valid_thinking_level("MAX"));
    assert!(!is_valid_thinking_level("none"));
}

// ---------------------------------------------------------------------------
// 权限 flags（PORTING_PLAN 第 0 周）
// ---------------------------------------------------------------------------

#[test]
fn parses_permission_mode_and_rules() {
    let parsed = parse(&[
        "--permission-mode",
        "read-only",
        "--allow",
        "read",
        "--allow",
        "command:git status",
        "--deny",
        "bash",
        "--add-dir",
        "/tmp/extra",
    ]);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.permission_mode, Some(PermissionMode::ReadOnly));
    assert_eq!(
        parsed.permission_allow,
        vec!["read".to_string(), "command:git status".to_string()]
    );
    assert_eq!(parsed.permission_deny, vec!["bash".to_string()]);
    assert_eq!(parsed.add_dirs, vec!["/tmp/extra".to_string()]);
}

// ---------------------------------------------------------------------------
// daemon flags（第 12 周后半）
// ---------------------------------------------------------------------------

#[test]
fn parses_serve_flags() {
    let parsed = parse(&[
        "--serve",
        "--serve-addr",
        "127.0.0.1:8787",
        "--serve-token",
        "secret",
    ]);
    assert!(parsed.diagnostics.is_empty());
    assert!(parsed.serve);
    assert_eq!(parsed.serve_addr, Some("127.0.0.1:8787".to_string()));
    assert_eq!(parsed.serve_token, Some("secret".to_string()));
}

#[test]
fn serve_defaults_are_off() {
    let parsed = parse(&[]);
    assert!(!parsed.serve);
    assert_eq!(parsed.serve_addr, None);
    assert_eq!(parsed.serve_token, None);
}

#[test]
fn invalid_permission_mode_is_an_error() {
    let parsed = parse(&["--permission-mode", "nonsense"]);
    assert_eq!(parsed.permission_mode, None);
    assert_eq!(
        parsed.diagnostics,
        vec![error(
            "Invalid permission mode \"nonsense\". Valid values: read-only, default, accept-edits, bypass"
        )]
    );
}

#[test]
fn invalid_permission_rule_is_an_error() {
    let parsed = parse(&["--allow", "   "]);
    assert_eq!(
        parsed.diagnostics,
        vec![error("Invalid --allow rule \"   \"")]
    );
}

/// 权限 flag 不能把后面的选项当成自己的值：`--deny --permission-mode read-only`
/// 必须保留 read-only 模式，并报 `--deny` 缺值（否则会生成一条 `--permission-mode`
/// 拒绝规则、丢掉模式，还把 `read-only` 变成位置消息）。
#[test]
fn permission_flags_reject_a_following_option_as_value() {
    let parsed = parse(&["--deny", "--permission-mode", "read-only"]);
    assert_eq!(parsed.permission_mode, Some(PermissionMode::ReadOnly));
    assert!(parsed.permission_deny.is_empty());
    assert!(parsed.messages.is_empty(), "{:?}", parsed.messages);
    assert_eq!(
        parsed.diagnostics,
        vec![error("--deny requires a rule value")]
    );

    let parsed = parse(&["--allow", "--add-dir", "/tmp/extra"]);
    assert!(parsed.permission_allow.is_empty());
    assert_eq!(parsed.add_dirs, vec!["/tmp/extra".to_string()]);
    assert_eq!(
        parsed.diagnostics,
        vec![error("--allow requires a rule value")]
    );

    let parsed = parse(&["--add-dir", "--model", "m"]);
    assert!(parsed.add_dirs.is_empty());
    assert_eq!(parsed.model, Some("m".to_string()));
    assert_eq!(
        parsed.diagnostics,
        vec![error("--add-dir requires a directory")]
    );

    let parsed = parse(&["--permission-mode", "--allow", "read"]);
    assert_eq!(parsed.permission_mode, None);
    assert_eq!(parsed.permission_allow, vec!["read".to_string()]);
    assert_eq!(
        parsed.diagnostics,
        vec![error(
            "--permission-mode requires read-only, default, accept-edits, or bypass"
        )]
    );
}

/// 每个权限 flag 位于末尾时都必须报缺值，而不是落入未知长选项分支。
#[test]
fn permission_flags_report_missing_values_at_end_of_args() {
    for flag in ["--permission-mode", "--allow", "--deny", "--add-dir"] {
        let parsed = parse(&[flag]);
        assert_eq!(
            parsed.diagnostics.len(),
            1,
            "{flag} must report a missing value: {:?}",
            parsed.diagnostics
        );
        assert!(
            parsed.unknown_flags.is_empty(),
            "{flag} must not silently become an extension flag"
        );
    }
}

/// `--flag=value` 形式仍然可用（值本身可以以 `-` 开头）。
#[test]
fn permission_flags_accept_equals_values() {
    let parsed = parse(&[
        "--permission-mode=accept-edits",
        "--allow=tool:read",
        "--deny=tool:mcp__*",
        "--add-dir=/tmp/extra",
    ]);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.permission_mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(parsed.permission_allow, vec!["tool:read".to_string()]);
    assert_eq!(parsed.permission_deny, vec!["tool:mcp__*".to_string()]);
    assert_eq!(parsed.add_dirs, vec!["/tmp/extra".to_string()]);
}

// ---------------------------------------------------------------------------
// 第 8 周深度修复：预算 / loop guard 的 CLI 覆盖
// ---------------------------------------------------------------------------

#[test]
fn parses_budget_and_loop_guard_flags() {
    let result = parse(&[
        "--max-turns",
        "3",
        "--max-tokens",
        "5000",
        "--max-cost-usd",
        "0.25",
        "--max-wall-clock-ms",
        "60000",
        "--loop-guard-stop",
    ]);
    assert_eq!(result.max_turns, Some(3));
    assert_eq!(result.max_tokens, Some(5000));
    assert_eq!(result.max_cost_usd, Some(0.25));
    assert_eq!(result.max_wall_clock_ms, Some(60_000));
    assert!(result.loop_guard_stop);
    assert!(!result.no_loop_guard);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn rejects_invalid_budget_values() {
    let zero = parse(&["--max-turns", "0"]);
    assert_eq!(zero.max_turns, None);
    assert_eq!(
        zero.diagnostics,
        vec![error("--max-turns requires a positive integer")]
    );

    let nan = parse(&["--max-cost-usd", "NaN"]);
    assert_eq!(nan.max_cost_usd, None);
    assert_eq!(
        nan.diagnostics,
        vec![error("--max-cost-usd requires a positive number")]
    );
}

#[test]
fn parses_no_loop_guard() {
    let result = parse(&["--no-loop-guard"]);
    assert!(result.no_loop_guard);
    assert!(!result.loop_guard_stop);
}
