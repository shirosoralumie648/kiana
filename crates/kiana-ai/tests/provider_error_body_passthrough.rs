//! `provider-error-body-passthrough.test.ts` 的 Rust 移植（离线）。
//!
//! 回归场景：网关/代理返回带 body 的非 2xx 时，provider 的 catch 块不能只保留 SDK 的
//! 不透明消息（`"403 status code (no body)"`），必须把状态码与 body 里的原因一起展示。
//!
//! TS 用 `vi.mock("openai")` 造 `APIError`；Rust 的 `openrouter-images` 直接走
//! [`HttpClient`]，因此这里用 wiremock 返回真实的 403 + JSON body，经过同一条
//! `normalizeProviderError` → `formatProviderError` 路径。

mod common;

use kiana_ai::image_models::generate_images;
use kiana_ai::types::{
    ImagesContext, ImagesStopReason, Model, ModelCost, ModelInputModality, ModelType, TextContent,
    TextKind, UserContentBlock,
};
use serde_json::json;

fn image_model(base_url: &str) -> Model {
    Model {
        model_type: Some(ModelType::Image),
        id: "black-forest-labs/flux.2-pro".into(),
        name: "FLUX.2 Pro".into(),
        api: "openrouter-images".into(),
        provider: "openrouter".into(),
        base_url: base_url.to_string(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        output: Some(vec!["image".into()]),
        cost: ModelCost {
            input: 0.015,
            output: 0.03,
            cache_read: 0.0,
            cache_write: 0.0,
            ..ModelCost::default()
        },
        ..Model::default()
    }
}

fn context() -> ImagesContext {
    ImagesContext {
        input: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "Generate a dog".into(),
            text_signature: None,
        })],
    }
}

#[tokio::test]
async fn surfaces_the_http_body_reason_instead_of_the_opaque_sdk_message() {
    let server = common::wiremock_util::start().await;
    common::wiremock_util::mount_body(
        &server,
        "/chat/completions",
        403,
        "application/json",
        json!({ "error": "blocked by gateway WAF" }).to_string(),
    )
    .await;

    let model = image_model(&common::wiremock_util::base_url(&server));
    let options = kiana_ai::types::StreamOptions {
        api_key: Some("test".into()),
        ..kiana_ai::types::StreamOptions::default()
    };

    let output = generate_images(&model, &context(), Some(&options))
        .await
        .expect("the openrouter-images provider is registered");

    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    let error_message = output.error_message.expect("error message");
    // 状态码必须出现。
    assert!(
        error_message.contains("403"),
        "expected status in {error_message:?}"
    );
    // body 里的原因不能被不透明的 SDK 消息吞掉。
    assert!(
        error_message.contains("blocked by gateway WAF"),
        "expected body reason in {error_message:?}"
    );
    assert_ne!(error_message, "403 status code (no body)");
}
