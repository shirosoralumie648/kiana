//! `responseid.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 各 provider 的响应必须带非空字符串 `responseId`，且 `stopReason !== "error"`。
//! 环境变量缺失时与 `skipIf` 一样跳过；`timeout: 30000` 由调用方决定。

mod common;

use kiana_ai::compat::{complete, get_model};
use kiana_ai::types::*;

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn model(provider: &str, id: &str) -> Model {
    get_model(provider, id).unwrap_or_else(|| panic!("model {provider}/{id} not found"))
}

fn opts(api_key: Option<String>, extras: &[(&str, serde_json::Value)]) -> StreamOptions {
    let mut options = StreamOptions {
        api_key,
        ..StreamOptions::default()
    };
    for (key, value) in extras {
        options.extra.insert((*key).to_string(), value.clone());
    }
    options
}

/// `expectResponseId`。
async fn expect_response_id(llm: &Model, options: StreamOptions) {
    let context = Context {
        system_prompt: Some("You are a helpful assistant. Be concise.".into()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Reply with exactly: response id test".into()),
            timestamp: common::event_stream::now_ms(),
        })],
        tools: None,
    };
    let response = complete(llm, &context, options).await;
    assert_ne!(
        response.stop_reason,
        StopReason::Error,
        "{:?}",
        response.error_message
    );
    let response_id = response.response_id.as_deref().unwrap_or("");
    assert!(!response_id.is_empty());
}

macro_rules! group {
    ($name:ident, $ignore:literal, $body:block) => {
        #[tokio::test]
        #[ignore = $ignore]
        async fn $name() $body
    };
}

group!(google, "live Google Gemini API (GEMINI_API_KEY)", {
    if !env_set("GEMINI_API_KEY") {
        return;
    }
    let llm = model("google", "gemini-2.5-flash");
    expect_response_id(&llm, StreamOptions::default()).await;
});

group!(google_vertex_adc, "live Google Vertex API (ADC)", {
    let project = env("GOOGLE_CLOUD_PROJECT").or_else(|| env("GCLOUD_PROJECT"));
    let location = env("GOOGLE_CLOUD_LOCATION");
    let (Some(project), Some(location)) = (project, location) else {
        return;
    };
    let llm = model("google-vertex", "gemini-3-flash-preview");
    expect_response_id(
        &llm,
        opts(
            None,
            &[
                ("project", serde_json::json!(project)),
                ("location", serde_json::json!(location)),
            ],
        ),
    )
    .await;
});

group!(
    google_vertex_api_key,
    "live Google Vertex API (GOOGLE_CLOUD_API_KEY)",
    {
        let Some(api_key) = env("GOOGLE_CLOUD_API_KEY") else {
            return;
        };
        let llm = model("google-vertex", "gemini-3-flash-preview");
        expect_response_id(&llm, opts(Some(api_key), &[])).await;
    }
);

group!(openai_completions, "live OpenAI API (OPENAI_API_KEY)", {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let mut llm = model("openai", "gpt-4o-mini");
    // TS 解构掉目录模型的 `compat`（`const { compat: _compat, ...baseModel }`），
    // 让 openai-completions 用自动检测的 compat（supportsStrictMode=false）。
    llm.compat = None;
    llm.api = "openai-completions".into();
    expect_response_id(&llm, StreamOptions::default()).await;
});

group!(openai_responses, "live OpenAI API (OPENAI_API_KEY)", {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let llm = model("openai", "gpt-5-mini");
    expect_response_id(&llm, StreamOptions::default()).await;
});

group!(anthropic, "live Anthropic API (ANTHROPIC_API_KEY)", {
    if !env_set("ANTHROPIC_API_KEY") {
        return;
    }
    let llm = model("anthropic", "claude-sonnet-4-5");
    expect_response_id(&llm, StreamOptions::default()).await;
});

group!(
    azure_openai,
    "live Azure OpenAI (AZURE_OPENAI_* credentials)",
    {
        if !common::credentials::has_azure_openai_credentials() {
            return;
        }
        let llm = model("azure", "gpt-4o-mini");
        let options = match common::credentials::resolve_azure_deployment_name(&llm.id) {
            Some(name) => opts(None, &[("azureDeploymentName", serde_json::json!(name))]),
            None => StreamOptions::default(),
        };
        expect_response_id(&llm, options).await;
    }
);

group!(mistral, "live Mistral API (MISTRAL_API_KEY)", {
    if !env_set("MISTRAL_API_KEY") {
        return;
    }
    let llm = model("mistral", "devstral-medium-latest");
    expect_response_id(&llm, StreamOptions::default()).await;
});

group!(
    github_copilot,
    "live GitHub Copilot OAuth (github-copilot credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
            return;
        };
        // OpenAI path。
        let llm = model("github-copilot", "gpt-5.3-codex");
        expect_response_id(&llm, opts(Some(token.clone()), &[])).await;
        // Anthropic path。
        let llm = model("github-copilot", "claude-sonnet-4.6");
        expect_response_id(&llm, opts(Some(token), &[])).await;
    }
);

group!(
    openai_codex,
    "live OpenAI Codex OAuth (openai-codex credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
            return;
        };
        let llm = model("openai-codex", "gpt-6.1-sol");
        expect_response_id(&llm, opts(Some(token), &[])).await;
    }
);
