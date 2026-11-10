//! `test/classifier-images-live.test.ts` 的 Rust 移植（真实凭据，`#[ignore]`）。
//!
//! 每个 provider 的用例只在对应环境变量存在时才有意义（TS 的 `describe.skipIf`），Rust 侧
//! 统一标成 `#[ignore = "live API: needs ..."]`；用 `--ignored` 跑时若凭据缺失就直接返回。
//!
//! TS 通过 `createModels()` + provider 工厂取目录里的模型；Rust 侧 provider 工厂由 providers
//! 单元交付（stub），这里直接用 `providers/catalog.rs` 的目录数据并调用适配器。
//!
//! 每个用例分别发送纯红与纯蓝两张 PNG，服务若丢图就无法蒙混过关。

#[path = "common/credentials.rs"]
#[allow(dead_code)]
mod credentials;

use kiana_ai::models::ClassifierOptions;
use kiana_ai::types::{
    ClassifierAnswer, ClassifierContext, ClassifierQuestion, ClassifierResult,
    ClassifierStopReason, Model, ModelInputModality, StreamOptions,
};
use serde_json::json;

// 512x512 solid PNGs from the TypeSafe SDK's live image tests.
const RED: &str = "iVBORw0KGgoAAAANSUhEUgAAAgAAAAIAAQMAAADOtka5AAAABlBMVEX/AAAAAABBoxIDAAAANklEQVR42u3BAQEAAACCIP+vbkhAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB8G4IAAAFjdVCkAAAAAElFTkSuQmCC";
const BLUE: &str = "iVBORw0KGgoAAAANSUhEUgAAAgAAAAIAAQMAAADOtka5AAAABlBMVEUAAP8AAAB7Yr8+AAAANklEQVR42u3BAQEAAACCIP+vbkhAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB8G4IAAAFjdVCkAAAAAElFTkSuQmCC";

fn color_context(data: &str) -> ClassifierContext {
    let mut questions = indexmap::IndexMap::new();
    questions.insert(
        "color".to_string(),
        ClassifierQuestion::Choice {
            instructions: "What color is the attached image?".into(),
            criteria: [
                ("red".to_string(), "Red".to_string()),
                ("blue".to_string(), "Blue".to_string()),
                ("green".to_string(), "Green".to_string()),
            ]
            .into_iter()
            .collect(),
        },
    );
    let mut state = indexmap::IndexMap::new();
    state.insert(
        "task".to_string(),
        json!("Name the color of the attached image"),
    );
    ClassifierContext {
        state,
        images: Some(vec![kiana_ai::types::ImageContent {
            kind_image: kiana_ai::types::ImageKind::Image,
            data: data.to_string(),
            mime_type: "image/png".to_string(),
        }]),
        questions,
    }
}

/// 目录模型（cloudflare 的 account 占位符按 `cloudflareClassifier` 的规则替换）。
fn catalog_classifier_model(provider: &str, id: &str) -> Model {
    let entry = kiana_ai::providers::catalog::get_classifier_model(provider, id)
        .unwrap_or_else(|| panic!("missing {provider}/{id}"));
    let mut model: Model = serde_json::from_value(entry.clone()).expect("catalog model");
    if let Some(account_id) = std::env::var("CLOUDFLARE_ACCOUNT_ID")
        .ok()
        .filter(|value| !value.is_empty())
    {
        model.base_url = model
            .base_url
            .replace("{CLOUDFLARE_ACCOUNT_ID}", &account_id);
    }
    model
}

async fn classify_with(
    model: &Model,
    context: &ClassifierContext,
    options: &ClassifierOptions,
) -> ClassifierResult {
    match model.api.as_str() {
        "typesafe-system-one" => {
            kiana_ai::api::typesafe_system_one::classify(model, context, Some(options)).await
        }
        "cloudflare-workers-ai-system-one" => {
            kiana_ai::api::cloudflare_workers_ai_system_one::classify(model, context, Some(options))
                .await
        }
        other => panic!("unexpected classifier api {other}"),
    }
}

async fn expect_colors(provider: &str, id: &str, options: ClassifierOptions) {
    let model = catalog_classifier_model(provider, id);
    assert!(model.input.contains(&ModelInputModality::Image));
    for (data, color) in [(RED, "red"), (BLUE, "blue")] {
        let result = classify_with(&model, &color_context(data), &options).await;
        assert!(result.error_message.is_none(), "{:?}", result.error_message);
        assert_eq!(result.stop_reason, ClassifierStopReason::Stop);
        match &result.answers["color"] {
            ClassifierAnswer::Choice { choice, .. } => assert_eq!(choice, color),
            other => panic!("unexpected answer: {other:?}"),
        }
    }
}

fn options_with_key(api_key: &str) -> ClassifierOptions {
    StreamOptions {
        api_key: Some(api_key.to_string()),
        ..StreamOptions::default()
    }
}

#[tokio::test]
#[ignore = "live API: needs TYPESAFE_API_KEY"]
async fn classifies_images_with_jev_latest() {
    let Some(api_key) = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    expect_colors("typesafe", "jev-latest", options_with_key(&api_key)).await;
}

#[tokio::test]
#[ignore = "live API: needs CLOUDFLARE_API_KEY and CLOUDFLARE_ACCOUNT_ID"]
async fn classifies_images_with_cloudflare_workers_ai_models() {
    if !credentials::has_cloudflare_workers_ai_credentials() {
        return;
    }
    let Some(api_key) = std::env::var("CLOUDFLARE_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    for id in [
        "@cf/cloudflare/clef",
        "@cf/cloudflare/clef-flash",
        "@cf/cloudflare/clef-omni",
        "typesafe/jev",
        "typesafe/jev-1.14.0",
    ] {
        expect_colors("cloudflare-workers-ai", id, options_with_key(&api_key)).await;
    }
}

#[tokio::test]
#[ignore = "live API: needs OPENROUTER_API_KEY"]
async fn classifies_images_with_openrouter_models() {
    let Some(api_key) = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    for id in ["typesafe/jev-1.14", "cloudflare/clef-flash"] {
        expect_colors("openrouter", id, options_with_key(&api_key)).await;
    }
}
