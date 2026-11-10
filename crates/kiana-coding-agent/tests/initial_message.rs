//! `test/initial-message.test.ts` 的逐条移植。

use kiana_ai::types::{ImageContent, ImageKind};
use kiana_coding_agent::cli::args::Args;
use kiana_coding_agent::cli::initial_message::{InitialMessageInput, build_initial_message};

fn create_args(messages: &[&str]) -> Args {
    Args {
        messages: messages.iter().map(|message| message.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn merges_piped_stdin_with_the_first_cli_message_into_one_prompt() {
    let mut parsed = create_args(&["Summarize the text given"]);
    let result = build_initial_message(
        &mut parsed,
        InitialMessageInput {
            stdin_content: Some("README contents\n".to_string()),
            ..Default::default()
        },
    );

    assert_eq!(
        result.initial_message.as_deref(),
        Some("README contents\nSummarize the text given")
    );
    assert_eq!(parsed.messages, Vec::<String>::new());
}

#[test]
fn uses_stdin_as_the_initial_prompt_when_no_cli_message_is_present() {
    let mut parsed = create_args(&[]);
    let result = build_initial_message(
        &mut parsed,
        InitialMessageInput {
            stdin_content: Some("README contents".to_string()),
            ..Default::default()
        },
    );

    assert_eq!(result.initial_message.as_deref(), Some("README contents"));
    assert_eq!(parsed.messages, Vec::<String>::new());
}

#[test]
fn combines_stdin_file_text_and_first_cli_message_in_one_prompt() {
    let mut parsed = create_args(&["Explain it", "Second message"]);
    let result = build_initial_message(
        &mut parsed,
        InitialMessageInput {
            stdin_content: Some("stdin\n".to_string()),
            file_text: Some("file\n".to_string()),
            ..Default::default()
        },
    );

    assert_eq!(
        result.initial_message.as_deref(),
        Some("stdin\nfile\nExplain it")
    );
    assert_eq!(parsed.messages, vec!["Second message".to_string()]);
}

#[test]
fn returns_file_images_only_when_non_empty() {
    let mut parsed = create_args(&[]);
    let empty = build_initial_message(
        &mut parsed,
        InitialMessageInput {
            file_images: Some(Vec::new()),
            ..Default::default()
        },
    );
    assert_eq!(empty.initial_images, None);

    let image = ImageContent {
        kind_image: ImageKind::Image,
        data: "AA==".to_string(),
        mime_type: "image/png".to_string(),
    };
    let mut parsed = create_args(&[]);
    let with_image = build_initial_message(
        &mut parsed,
        InitialMessageInput {
            file_images: Some(vec![image.clone()]),
            ..Default::default()
        },
    );
    assert_eq!(with_image.initial_images, Some(vec![image]));
}
