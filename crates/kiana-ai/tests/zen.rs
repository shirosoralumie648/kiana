//! `packages/ai/test/zen.test.ts` 的 Rust 移植：需要真实 `OPENCODE_API_KEY` 的 live 冒烟测试。
//!
//! TS 用 `describe.skipIf(!process.env.OPENCODE_API_KEY)` 在无 key 时跳过并逐个模型生成用例；
//! Rust 侧统一标记 `#[ignore]`（运行方式：设置 key 后 `cargo test -- --ignored`）。

use kiana_ai::models::{CreateModelsOptions, ModelsSimpleStreamOptions, create_models};
use kiana_ai::providers::all::{builtin_providers, get_builtin_models};
use kiana_ai::types::{
    Context, Message, SimpleStreamOptions, StreamOptions, UserContent, UserMessage, UserRole,
};

#[tokio::test]
#[ignore = "requires OPENCODE_API_KEY (live API)"]
async fn opencode_models_smoke_test() {
    let api_key = std::env::var("OPENCODE_API_KEY")
        .expect("OPENCODE_API_KEY is required for the live smoke test");
    let models = create_models(CreateModelsOptions::default());
    for provider in builtin_providers() {
        if provider.id() != "opencode" && provider.id() != "opencode-go" {
            continue;
        }
        models.set_provider(provider.clone());
        let label = if provider.id() == "opencode" {
            "OpenCode Zen"
        } else {
            "OpenCode Go"
        };
        for model in get_builtin_models(provider.id()) {
            let context = Context {
                system_prompt: None,
                messages: vec![Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text("Say hello.".into()),
                    timestamp: 0,
                })],
                tools: None,
            };
            let options = SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some(api_key.clone()),
                    ..Default::default()
                },
                ..Default::default()
            };
            let response = models
                .complete_simple(&model, &context, ModelsSimpleStreamOptions::from(options))
                .await;
            assert!(
                !response.content.is_empty(),
                "{label}: {} returned empty content",
                model.id
            );
            assert_eq!(
                response.stop_reason,
                kiana_ai::types::StopReason::Stop,
                "{label}: {}",
                model.id
            );
        }
    }
}
