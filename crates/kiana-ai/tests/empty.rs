//! `empty.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 各 provider 对空消息的容错：空 content 数组、空字符串、纯空白、以及对话里夹带空
//! assistant 消息。断言与 TS 相同：响应必须是 assistant；`stopReason === "error"` 时
//! 必须有 `errorMessage`，否则 content 必须存在（空 assistant 用例还要求非空）。
//!
//! 环境变量缺失时与 vitest 的 `skipIf` 一样直接返回；同一 provider 的 4 个用例合并到
//! 一个测试里顺序执行。`retry: 3` 由调用方重试。

mod common;

use kiana_ai::compat::{complete, get_model};
use kiana_ai::types::*;
use serde_json::json;

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn model(provider: &str, id: &str) -> Model {
    get_model(provider, id).unwrap_or_else(|| panic!("model {provider}/{id} not found"))
}

fn clone_options(options: &StreamOptions) -> StreamOptions {
    StreamOptions {
        api_key: options.api_key.clone(),
        env: options.env.clone(),
        headers: options.headers.clone(),
        identity: options.identity.clone(),
        timeout_ms: options.timeout_ms,
        max_retries: options.max_retries,
        max_retry_delay_ms: options.max_retry_delay_ms,
        temperature: options.temperature,
        sampling_params: options.sampling_params.clone(),
        max_tokens: options.max_tokens,
        transport: options.transport,
        cache_retention: options.cache_retention,
        session_id: options.session_id.clone(),
        websocket_connect_timeout_ms: options.websocket_connect_timeout_ms,
        metadata: options.metadata.clone(),
        extra: options.extra.clone(),
        signal: options.signal.clone(),
        on_payload: options.on_payload.clone(),

        on_payload_async: options.on_payload_async.clone(),
        on_response: options.on_response.clone(),
        on_provider_stream_event: options.on_provider_stream_event.clone(),
        fetch: options.fetch.clone(),
        telemetry: options.telemetry.clone(),
    }
}

fn opts(extras: &[(&str, serde_json::Value)]) -> StreamOptions {
    let mut options = StreamOptions::default();
    for (key, value) in extras {
        options.extra.insert((*key).to_string(), value.clone());
    }
    options
}

fn user_text(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: common::event_stream::now_ms(),
    })
}

/// 断言一个空消息请求的通用后置条件。
fn assert_graceful(response: &AssistantMessage) {
    assert_eq!(response.role, AssistantRole::Assistant);
    if response.stop_reason == StopReason::Error {
        assert!(response.error_message.is_some());
    } else {
        // TS: `expect(response.content).toBeDefined()`；`Vec` 恒存在，无额外断言。
    }
}

/// `testEmptyMessage`。
async fn test_empty_message(llm: &Model, options: &StreamOptions) {
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(Vec::new()),
            timestamp: common::event_stream::now_ms(),
        })],
        tools: None,
    };
    let response = complete(llm, &context, clone_options(options)).await;
    assert_graceful(&response);
}

/// `testEmptyStringMessage`。
async fn test_empty_string_message(llm: &Model, options: &StreamOptions) {
    let context = Context {
        system_prompt: None,
        messages: vec![user_text("")],
        tools: None,
    };
    let response = complete(llm, &context, clone_options(options)).await;
    assert_graceful(&response);
}

/// `testWhitespaceOnlyMessage`。
async fn test_whitespace_only_message(llm: &Model, options: &StreamOptions) {
    let context = Context {
        system_prompt: None,
        messages: vec![user_text("   \n\t  ")],
        tools: None,
    };
    let response = complete(llm, &context, clone_options(options)).await;
    assert_graceful(&response);
}

/// `testEmptyAssistantMessage`。
async fn test_empty_assistant_message(llm: &Model, options: &StreamOptions) {
    let empty_assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: llm.api.clone(),
        provider: llm.provider.clone(),
        model: llm.id.clone(),
        usage: Usage {
            input: 10,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 10,
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: common::event_stream::now_ms(),
        ..AssistantMessage::default()
    };
    let context = Context {
        system_prompt: None,
        messages: vec![
            user_text("Hello, how are you?"),
            Message::Assistant(empty_assistant),
            user_text("Please respond this time."),
        ],
        tools: None,
    };
    let response = complete(llm, &context, clone_options(options)).await;
    assert_eq!(response.role, AssistantRole::Assistant);
    if response.stop_reason == StopReason::Error {
        assert!(response.error_message.is_some());
    } else {
        assert!(!response.content.is_empty());
    }
}

async fn run_all(llm: &Model, options: &StreamOptions) {
    test_empty_message(llm, options).await;
    test_empty_string_message(llm, options).await;
    test_whitespace_only_message(llm, options).await;
    test_empty_assistant_message(llm, options).await;
}

macro_rules! group {
    ($name:ident, $ignore:literal, $body:block) => {
        #[tokio::test]
        #[ignore = $ignore]
        async fn $name() $body
    };
}

macro_rules! basic_group {
    ($name:ident, $env:literal, $provider:literal, $model_id:literal, $ignore:literal) => {
        group!($name, $ignore, {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, $model_id);
            run_all(&llm, &StreamOptions::default()).await;
        });
    };
}

basic_group!(
    google,
    "GEMINI_API_KEY",
    "google",
    "gemini-2.5-flash",
    "live Google Gemini API (GEMINI_API_KEY)"
);
basic_group!(
    openai_completions,
    "OPENAI_API_KEY",
    "openai",
    "gpt-4o-mini",
    "live OpenAI API (OPENAI_API_KEY)"
);
basic_group!(
    openai_responses,
    "OPENAI_API_KEY",
    "openai",
    "gpt-5-mini",
    "live OpenAI API (OPENAI_API_KEY)"
);
basic_group!(
    anthropic,
    "ANTHROPIC_API_KEY",
    "anthropic",
    "claude-haiku-4-5",
    "live Anthropic API (ANTHROPIC_API_KEY)"
);
basic_group!(
    xai,
    "XAI_API_KEY",
    "xai",
    "grok-4.3",
    "live xAI API (XAI_API_KEY)"
);
basic_group!(
    groq,
    "GROQ_API_KEY",
    "groq",
    "openai/gpt-oss-20b",
    "live Groq API (GROQ_API_KEY)"
);
basic_group!(
    cerebras,
    "CEREBRAS_API_KEY",
    "cerebras",
    "gpt-oss-120b",
    "live Cerebras API (CEREBRAS_API_KEY)"
);
basic_group!(
    zai,
    "ZAI_API_KEY",
    "zai",
    "glm-5.2",
    "live zAI API (ZAI_API_KEY)"
);
basic_group!(
    mistral,
    "MISTRAL_API_KEY",
    "mistral",
    "devstral-medium-latest",
    "live Mistral API (MISTRAL_API_KEY)"
);
basic_group!(
    minimax,
    "MINIMAX_API_KEY",
    "minimax",
    "MiniMax-M2.7",
    "live MiniMax API (MINIMAX_API_KEY)"
);
basic_group!(
    kimi_coding,
    "KIMI_API_KEY",
    "kimi-coding",
    "kimi-for-coding",
    "live Kimi For Coding API (KIMI_API_KEY)"
);
basic_group!(
    vercel_ai_gateway,
    "AI_GATEWAY_API_KEY",
    "vercel-ai-gateway",
    "google/gemini-2.5-flash",
    "live Vercel AI Gateway (AI_GATEWAY_API_KEY)"
);

group!(
    azure_openai,
    "live Azure OpenAI (AZURE_OPENAI_* credentials)",
    {
        if !common::credentials::has_azure_openai_credentials() {
            return;
        }
        let llm = model("azure", "gpt-4o-mini");
        let options = match common::credentials::resolve_azure_deployment_name(&llm.id) {
            Some(name) => opts(&[("azureDeploymentName", json!(name))]),
            None => StreamOptions::default(),
        };
        run_all(&llm, &options).await;
    }
);

group!(cloudflare_workers_ai, "live Cloudflare Workers AI", {
    if !common::credentials::has_cloudflare_workers_ai_credentials() {
        return;
    }
    let llm = model("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6");
    run_all(&llm, &StreamOptions::default()).await;
});

group!(cloudflare_ai_gateway, "live Cloudflare AI Gateway", {
    if !common::credentials::has_cloudflare_ai_gateway_credentials() {
        return;
    }
    let llm = model(
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    );
    run_all(&llm, &StreamOptions::default()).await;
});

group!(huggingface, "live Hugging Face API (HF_TOKEN)", {
    if !env_set("HF_TOKEN") {
        return;
    }
    let llm = model("huggingface", "moonshotai/Kimi-K2.5");
    run_all(&llm, &StreamOptions::default()).await;
});

group!(together, "live Together AI API (TOGETHER_API_KEY)", {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    let llm = model("together", "moonshotai/Kimi-K3");
    let options = opts(&[("reasoningEffort", json!("high"))]);
    run_all(&llm, &options).await;
});

group!(baseten, "live Baseten API (BASETEN_API_KEY)", {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    let llm = model("baseten", "zai-org/GLM-5.2");
    let options = opts(&[("reasoningEffort", json!("high"))]);
    run_all(&llm, &options).await;
});

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!($name, "live Xiaomi MiMo API", {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, "mimo-v2.6-pro");
            run_all(&llm, &StreamOptions::default()).await;
        });
    };
}

xiaomi_family!(xiaomi, "XIAOMI_API_KEY", "xiaomi");
xiaomi_family!(
    xiaomi_token_plan_cn,
    "XIAOMI_TOKEN_PLAN_CN_API_KEY",
    "xiaomi-token-plan-cn"
);
xiaomi_family!(
    xiaomi_token_plan_ams,
    "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
    "xiaomi-token-plan-ams"
);
xiaomi_family!(
    xiaomi_token_plan_sgp,
    "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
    "xiaomi-token-plan-sgp"
);

macro_rules! qwen_family {
    ($name:ident, $env:literal, $provider:literal, $model_id:literal) => {
        group!($name, "live Qwen Token Plan API", {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, $model_id);
            run_all(&llm, &StreamOptions::default()).await;
        });
    };
}

qwen_family!(
    qwen_token_plan,
    "QWEN_TOKEN_PLAN_API_KEY",
    "qwen-token-plan",
    "qwen3.7-max"
);
qwen_family!(
    qwen_token_plan_individual,
    "QWEN_TOKEN_PLAN_API_KEY",
    "qwen-token-plan-individual",
    "qwen3.8-max"
);
qwen_family!(
    qwen_token_plan_cn,
    "QWEN_TOKEN_PLAN_CN_API_KEY",
    "qwen-token-plan-cn",
    "qwen3.7-max"
);

group!(
    anthropic_oauth,
    "live Anthropic OAuth (anthropic credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("anthropic") else {
            return;
        };
        let llm = model("anthropic", "claude-haiku-4-5");
        let options = StreamOptions {
            api_key: Some(token),
            ..StreamOptions::default()
        };
        run_all(&llm, &options).await;
    }
);

group!(
    github_copilot,
    "live GitHub Copilot OAuth (github-copilot credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
            return;
        };
        for model_id in ["claude-haiku-4.5", "claude-sonnet-4.6"] {
            let llm = model("github-copilot", model_id);
            let options = StreamOptions {
                api_key: Some(token.clone()),
                ..StreamOptions::default()
            };
            run_all(&llm, &options).await;
        }
    }
);

group!(bedrock, "live Amazon Bedrock (AWS credentials)", {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    let llm = model(
        "amazon-bedrock",
        "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
    );
    run_all(&llm, &StreamOptions::default()).await;
});

group!(
    openai_codex,
    "live OpenAI Codex OAuth (openai-codex credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
            return;
        };
        let llm = model("openai-codex", "gpt-6.1-sol");
        let options = StreamOptions {
            api_key: Some(token),
            ..StreamOptions::default()
        };
        run_all(&llm, &options).await;
    }
);
