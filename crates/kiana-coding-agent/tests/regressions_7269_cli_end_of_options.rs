//! `test/suite/regressions/7269-cli-end-of-options.test.ts` 的解析部分。
//!
//! TS 用例的后半段用 session harness 断言会话消息；那部分属于 agent/session 单元，
//! 这里只保留 `parseArgs` 的断言，解析出的选项需保持稳定。

use kiana_coding_agent::cli::args::parse_args;

fn parse(args: &[&str]) -> kiana_coding_agent::cli::args::Args {
    parse_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
}

#[test]
fn passes_dash_summarize_prompt_after_end_of_options() {
    let prompt = "- summarize the following points for me";
    let parsed = parse(&["-ne", "--no-session", "-p", "--", prompt]);
    assert_eq!(parsed.messages, vec![prompt.to_string()]);
    assert_eq!(parsed.unknown_flags.len(), 0);
    assert_eq!(parsed.diagnostics, Vec::new());
}

#[test]
fn passes_double_dash_answer_prompt_after_end_of_options() {
    let prompt = "--answer my question briefly";
    let parsed = parse(&["-ne", "--no-session", "-p", "--", prompt]);
    assert_eq!(parsed.messages, vec![prompt.to_string()]);
    assert_eq!(parsed.unknown_flags.len(), 0);
    assert_eq!(parsed.diagnostics, Vec::new());
}

#[test]
fn stops_parsing_options_while_retaining_file_handling() {
    let parsed = parse(&[
        "--unknown-flag",
        "value",
        "--",
        "--provider",
        "openai",
        "-c",
        "@prompt.md",
    ]);

    assert_eq!(
        parsed.unknown_flags.get("unknown-flag"),
        Some(&kiana_ext_api::types::FlagValue::Str("value".to_string()))
    );
    assert_eq!(parsed.provider, None);
    assert!(!parsed.r#continue);
    assert_eq!(
        parsed.messages,
        vec![
            "--provider".to_string(),
            "openai".to_string(),
            "-c".to_string()
        ]
    );
    assert_eq!(parsed.file_args, vec!["prompt.md".to_string()]);
    assert_eq!(parsed.diagnostics, Vec::new());
}
