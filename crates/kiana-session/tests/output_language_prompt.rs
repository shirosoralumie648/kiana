//! 第 12 周：`output_language` 进入 system prompt。
//!
//! 覆盖 DoD：`output_language=zh` 时系统提示包含中文回答规则；`en` 不追加规则；
//! settings 的 `output_language` 与 `ui.outputLanguage` 都能生效。

mod common;

use common::*;

#[tokio::test]
async fn chinese_output_language_is_in_the_system_prompt() {
    run(async {
        let harness = create_harness(HarnessOptions {
            output_language: Some("zh".to_string()),
            ..Default::default()
        })
        .await;
        let prompt = harness.session.system_prompt();
        assert!(
            prompt.contains("Always respond in Chinese (简体中文)"),
            "prompt: {prompt}"
        );
        assert!(prompt.contains("<output_language>"), "prompt: {prompt}");
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn english_output_language_adds_no_rule() {
    run(async {
        let harness = create_harness(HarnessOptions {
            output_language: Some("en".to_string()),
            ..Default::default()
        })
        .await;
        let prompt = harness.session.system_prompt();
        assert!(!prompt.contains("Always respond in"), "prompt: {prompt}");
        assert!(!prompt.contains("<output_language>"), "prompt: {prompt}");
        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn output_language_setting_is_used() {
    run(async {
        let settings: kiana_config::settings::Settings =
            serde_json::from_value(serde_json::json!({"ui": {"outputLanguage": "zh"}}))
                .expect("settings");
        let harness = create_harness(HarnessOptions {
            settings: Some(settings),
            ..Default::default()
        })
        .await;
        let prompt = harness.session.system_prompt();
        assert!(
            prompt.contains("Always respond in Chinese (简体中文)"),
            "prompt: {prompt}"
        );
        harness.cleanup();
    })
    .await;
}
