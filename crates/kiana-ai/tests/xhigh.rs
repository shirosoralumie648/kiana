//! `xhigh.test.ts` 的 Rust 移植 — 真实 OpenAI API 测试。
//!
//! TS 侧用 `describe.skipIf(!process.env.OPENAI_API_KEY)` 跳过；Rust 侧对应 `#[ignore]`
//! （与 TS 相同的断言，运行时设置 `OPENAI_API_KEY` 并加 `--ignored`）。

fn make_context() -> kiana_ai::types::Context {
    kiana_ai::types::Context {
        messages: vec![kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text(
                    "What is 7 + 3? Think step by step.".into(),
                ),
                timestamp: 0,
            },
        )],
        ..Default::default()
    }
}

fn xhigh_options() -> kiana_ai::types::StreamOptions {
    let mut options = kiana_ai::types::StreamOptions::default();
    options
        .extra
        .insert("reasoningEffort".into(), serde_json::json!("xhigh"));
    options
}

async fn drain(
    stream: kiana_ai::event_stream::AssistantMessageEventStream,
) -> (bool, kiana_ai::types::AssistantMessage) {
    use kiana_ai::types::AssistantMessageEvent;
    let mut has_thinking = false;
    while let Some(event) = stream.recv().await {
        if matches!(
            event,
            AssistantMessageEvent::ThinkingStart { .. }
                | AssistantMessageEvent::ThinkingDelta { .. }
        ) {
            has_thinking = true;
        }
    }
    let message = stream.result().await.expect("result");
    (has_thinking, message)
}

fn text_blocks(message: &kiana_ai::types::AssistantMessage) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, kiana_ai::types::AssistantContentBlock::Text(_)))
}

fn thinking_blocks(message: &kiana_ai::types::AssistantMessage) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, kiana_ai::types::AssistantContentBlock::Thinking(_)))
}

#[tokio::test]
#[ignore = "live OpenAI API test; needs OPENAI_API_KEY"]
async fn gpt_5_5_should_work_with_openai_responses_at_xhigh() {
    let model = kiana_ai::compat::get_model("openai", "gpt-5.5").expect("gpt-5.5");
    let stream = kiana_ai::compat::stream(&model, &make_context(), xhigh_options());
    let (has_thinking, response) = drain(stream).await;
    assert_eq!(
        response.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "Error: {}",
        response.error_message.clone().unwrap_or_default()
    );
    assert!(text_blocks(&response));
    assert!(has_thinking || thinking_blocks(&response));
}

#[tokio::test]
#[ignore = "live OpenAI API test; needs OPENAI_API_KEY"]
async fn gpt_5_mini_should_error_with_openai_responses_when_using_xhigh() {
    let model = kiana_ai::compat::get_model("openai", "gpt-5-mini").expect("gpt-5-mini");
    let stream = kiana_ai::compat::stream(&model, &make_context(), xhigh_options());
    let (_, response) = drain(stream).await;
    assert_eq!(response.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        response
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("xhigh"))
    );
}

#[tokio::test]
#[ignore = "live OpenAI API test; needs OPENAI_API_KEY"]
async fn gpt_5_mini_should_error_with_openai_completions_when_using_xhigh() {
    let mut model = kiana_ai::compat::get_model("openai", "gpt-5-mini").expect("gpt-5-mini");
    model.compat = None;
    model.api = "openai-completions".into();
    let stream = kiana_ai::compat::stream(&model, &make_context(), xhigh_options());
    let (_, response) = drain(stream).await;
    assert_eq!(response.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        response
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("xhigh"))
    );
}

#[test]
fn gpt_5_5_and_gpt_5_mini_exist_in_the_catalog_with_expected_xhigh_support() {
    // 不需要网络的目录断言：gpt-5.5 支持 xhigh；gpt-5-mini 不支持（xhigh 会被钳制到 high）。
    let gpt_5_5: kiana_ai::types::Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openai", "gpt-5.5")
            .expect("gpt-5.5")
            .clone(),
    )
    .unwrap();
    assert!(
        kiana_ai::models::get_supported_thinking_levels(&gpt_5_5)
            .contains(&kiana_ai::types::ModelThinkingLevel::Xhigh)
    );
    let gpt_5_mini: kiana_ai::types::Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openai", "gpt-5-mini")
            .expect("gpt-5-mini")
            .clone(),
    )
    .unwrap();
    assert_eq!(
        kiana_ai::models::clamp_thinking_level(
            &gpt_5_mini,
            kiana_ai::types::ModelThinkingLevel::Xhigh
        ),
        kiana_ai::types::ModelThinkingLevel::High
    );
}
