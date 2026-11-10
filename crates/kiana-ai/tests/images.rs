//! `images.test.ts` 的 Rust 移植（真实 OpenRouter 图片 API，`#[ignore]`）。
//!
//! TS 的两个用例都只在 `OPENROUTER_API_KEY` 存在时运行（`describe.skipIf`），模型是
//! `getImageModel("openrouter", "google/gemini-2.5-flash-image")`。注意原测试**没有**把
//! key 传进 options（`generateImages` 的注释要求显式 `options.apiKey`），因此这里同样
//! 不传——本用例固定省略 `options`，断言按该调用形状编写。
//!
//! 图片输入用例在模型不支持 image 输入时与 TS 一样直接返回。

mod common;

use kiana_ai::image_models::{generate_images, get_image_model};
use kiana_ai::types::{
    ImageContent, ImageKind, ImagesContext, ImagesStopReason, ModelInputModality, TextContent,
    TextKind, UserContentBlock,
};

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn image_model() -> kiana_ai::types::Model {
    get_image_model("openrouter", "google/gemini-2.5-flash-image")
        .expect("openrouter image model is in the catalog")
}

fn assert_generated(response: &kiana_ai::types::AssistantImages) {
    assert_eq!(
        response.stop_reason,
        ImagesStopReason::Stop,
        "Error: {:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
    assert!(
        response
            .output
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    );
    assert!(response.timestamp > 0);
}

/// `basicImageGeneration`。
async fn basic_image_generation(model: &kiana_ai::types::Model) {
    let context = ImagesContext {
        input: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "Generate a simple red circle on a plain white background. No text.".into(),
            text_signature: None,
        })],
    };
    let response = generate_images(model, &context, None)
        .await
        .expect("openrouter-images provider is registered");
    assert_generated(&response);
}

/// `handleImageInput`。
async fn handle_image_input(model: &kiana_ai::types::Model) {
    if !model.input.contains(&ModelInputModality::Image) {
        return;
    }
    use base64::Engine as _;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/compat-entrypoints/red-circle.png"
    );
    let bytes = std::fs::read(path).expect("red-circle.png fixture");
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);

    let context = ImagesContext {
        input: vec![
            UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "Create a variation of this image with a blue background.".into(),
                text_signature: None,
            }),
            UserContentBlock::Image(ImageContent {
                kind_image: ImageKind::Image,
                data,
                mime_type: "image/png".into(),
            }),
        ],
    };
    let response = generate_images(model, &context, None)
        .await
        .expect("openrouter-images provider is registered");
    assert_eq!(
        response.stop_reason,
        ImagesStopReason::Stop,
        "Error: {:?}",
        response.error_message
    );
    assert!(
        response
            .output
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    );
}

#[tokio::test]
#[ignore = "live OpenRouter images API (OPENROUTER_API_KEY)"]
async fn should_generate_a_basic_image() {
    if !env_set("OPENROUTER_API_KEY") {
        return;
    }
    let model = image_model();
    basic_image_generation(&model).await;
}

#[tokio::test]
#[ignore = "live OpenRouter images API (OPENROUTER_API_KEY)"]
async fn should_handle_image_input() {
    if !env_set("OPENROUTER_API_KEY") {
        return;
    }
    let model = image_model();
    handle_image_input(&model).await;
}
