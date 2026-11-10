//! `core/auth-guidance.ts`（spec §2.6）的逐字文案测试。
//!
//! TS 源没有独立单测；这里按规范里【逐字】的完整展开断言四条消息。文档目录由
//! `get_docs_path()` 决定（`<package-dir>/docs`），因此期望值用同一路径拼出，其余文本
//! 一字不差。

use kiana_config::auth_guidance::{
    UNKNOWN_PROVIDER, format_no_api_key_found_message, format_no_model_selected_message,
    format_no_models_available_message, get_provider_login_help,
};
use kiana_core::config::get_docs_path;

fn docs(name: &str) -> String {
    get_docs_path().join(name).to_string_lossy().into_owned()
}

fn login_help() -> String {
    format!(
        "Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        docs("providers.md"),
        docs("models.md")
    )
}

#[test]
fn unknown_provider_sentinel_is_the_ts_literal() {
    assert_eq!(UNKNOWN_PROVIDER, "unknown");
}

#[test]
fn login_help_is_three_verbatim_lines() {
    assert_eq!(get_provider_login_help(), login_help());
    // docs 目录来自 `getDocsPath()`，不是相对路径。
    assert!(get_provider_login_help().contains(&docs("providers.md")));
    assert!(get_provider_login_help().ends_with(&docs("models.md")));
}

#[test]
fn no_models_available_message_is_verbatim() {
    let expected = format!(
        "No models available. Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        docs("providers.md"),
        docs("models.md")
    );
    assert_eq!(format_no_models_available_message(), expected);
}

#[test]
fn no_model_selected_message_is_verbatim() {
    let expected = format!(
        "No model selected.\n\nUse /login to log into a provider via OAuth or API key. See:\n  {}\n  {}\n\nThen use /model to select a model.",
        docs("providers.md"),
        docs("models.md")
    );
    assert_eq!(format_no_model_selected_message(), expected);
}

#[test]
fn no_api_key_found_message_is_verbatim() {
    let expected = format!(
        "No API key found for anthropic.\n\nUse /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        docs("providers.md"),
        docs("models.md")
    );
    assert_eq!(format_no_api_key_found_message("anthropic"), expected);
}

#[test]
fn unknown_provider_renders_as_the_selected_model() {
    let expected = format!(
        "No API key found for the selected model.\n\nUse /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        docs("providers.md"),
        docs("models.md")
    );
    assert_eq!(format_no_api_key_found_message("unknown"), expected);
}

#[test]
fn provider_name_is_interpolated_verbatim() {
    // TS 只对 `"unknown"` 做特判：大小写、空白都原样输出。
    for provider in [
        "Unknown",
        "openrouter",
        "",
        "anthropic ",
        "a\nb",
        "unknownish",
    ] {
        assert_eq!(
            format_no_api_key_found_message(provider),
            format!("No API key found for {provider}.\n\n{}", login_help())
        );
    }
}

#[test]
fn messages_share_the_same_help_block() {
    let help = login_help();
    assert_eq!(
        format_no_models_available_message(),
        format!("No models available. {help}")
    );
    assert_eq!(
        format_no_model_selected_message(),
        format!("No model selected.\n\n{help}\n\nThen use /model to select a model.")
    );
    assert_eq!(
        format_no_api_key_found_message("unknown"),
        format!("No API key found for the selected model.\n\n{help}")
    );
}
