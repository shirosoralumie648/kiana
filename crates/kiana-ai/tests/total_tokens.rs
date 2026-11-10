//! `total-tokens.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! `totalTokens` 表示 LLM 处理的总 token 数（input + output + cacheRead + cacheWrite，
//! 是该口径的基准）。测试对每个 provider 连续发两次请求（第二次命中缓存），断言
//! `totalTokens == input + output + cacheRead + cacheWrite`；Anthropic 额外要求出现缓存活动。
//!
//! 环境变量缺失时与 vitest 的 `skipIf` 一样直接返回；`retry: 3` 由调用方重试。

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

/// 触发缓存的系统提示（>2KB）。
fn long_system_prompt() -> String {
    let sentence = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.";
    let mut body = String::from(
        "You are a helpful assistant. Be concise in your responses.\n\nHere is some additional context that makes this system prompt long enough to trigger caching:\n\n",
    );
    body.push_str(&vec![sentence; 50].join("\n\n"));
    body.push_str("\n\nRemember: Always be helpful and concise.");
    body
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: common::event_stream::now_ms(),
    })
}

fn assert_total_tokens_equals_components(usage: &Usage) {
    let computed = usage.input + usage.output + usage.cache_read + usage.cache_write;
    assert_eq!(usage.total_tokens, computed);
}

/// `testTotalTokensWithCache`：返回 `(first, second)`。
async fn test_total_tokens_with_cache(llm: &Model, options: &StreamOptions) -> (Usage, Usage) {
    let system_prompt = long_system_prompt();
    let context1 = Context {
        system_prompt: Some(system_prompt.clone()),
        messages: vec![user("What is 2 + 2? Reply with just the number.")],
        tools: None,
    };

    let response1 = complete(llm, &context1, clone_options(options)).await;
    assert_eq!(response1.stop_reason, StopReason::Stop);

    let context2 = Context {
        system_prompt: Some(system_prompt),
        messages: vec![
            context1.messages[0].clone(),
            Message::Assistant(response1.clone()),
            user("What is 3 + 3? Reply with just the number."),
        ],
        tools: None,
    };

    let response2 = complete(llm, &context2, clone_options(options)).await;
    assert_eq!(response2.stop_reason, StopReason::Stop);

    (response1.usage, response2.usage)
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
            let options = opts(env($env), &[]);
            let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
            assert_total_tokens_equals_components(&first);
            assert_total_tokens_equals_components(&second);
        });
    };
}

// TS: `describe.skipIf(!process.env.ANTHROPIC_API_KEY)` 的用例同样断言缓存活动
// （`hasCache = second.cacheRead > 0 || second.cacheWrite > 0 || first.cacheWrite > 0`）。
group!(
    anthropic_api_key,
    "live Anthropic API (ANTHROPIC_API_KEY)",
    {
        if !env_set("ANTHROPIC_API_KEY") {
            return;
        }
        let llm = model("anthropic", "claude-sonnet-4-5");
        let options = opts(env("ANTHROPIC_API_KEY"), &[]);
        let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
        assert_total_tokens_equals_components(&first);
        assert_total_tokens_equals_components(&second);
        // Anthropic 应当有缓存活动。
        assert!(second.cache_read > 0 || second.cache_write > 0 || first.cache_write > 0);
    }
);

group!(
    anthropic_oauth,
    "live Anthropic OAuth (anthropic credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("anthropic") else {
            return;
        };
        let llm = model("anthropic", "claude-sonnet-4-6");
        let options = opts(Some(token), &[]);
        let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
        assert_total_tokens_equals_components(&first);
        assert_total_tokens_equals_components(&second);
        // Anthropic 应当有缓存活动。
        assert!(second.cache_read > 0 || second.cache_write > 0 || first.cache_write > 0);
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
    let (first, second) = test_total_tokens_with_cache(&llm, &StreamOptions::default()).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(openai_responses, "live OpenAI API (OPENAI_API_KEY)", {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let llm = model("openai", "gpt-4o");
    let (first, second) = test_total_tokens_with_cache(&llm, &StreamOptions::default()).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(
    azure_openai_responses,
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
        let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
        assert_total_tokens_equals_components(&first);
        assert_total_tokens_equals_components(&second);
    }
);

basic_group!(
    google,
    "GEMINI_API_KEY",
    "google",
    "gemini-2.5-flash",
    "live Google Gemini API (GEMINI_API_KEY)"
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
    "openai/gpt-oss-120b",
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

group!(cloudflare_workers_ai, "live Cloudflare Workers AI", {
    if !common::credentials::has_cloudflare_workers_ai_credentials() {
        return;
    }
    let llm = model("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6");
    let options = opts(env("CLOUDFLARE_API_KEY"), &[]);
    let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(cloudflare_ai_gateway, "live Cloudflare AI Gateway", {
    if !common::credentials::has_cloudflare_ai_gateway_credentials() {
        return;
    }
    let llm = model(
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    );
    let options = opts(env("CLOUDFLARE_API_KEY"), &[]);
    let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(huggingface, "live Hugging Face API (HF_TOKEN)", {
    if !env_set("HF_TOKEN") {
        return;
    }
    let llm = model("huggingface", "moonshotai/Kimi-K2.5");
    let options = opts(env("HF_TOKEN"), &[]);
    let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(together, "live Together AI API (TOGETHER_API_KEY)", {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    let llm = model("together", "moonshotai/Kimi-K3");
    let options = opts(
        env("TOGETHER_API_KEY"),
        &[("reasoningEffort", serde_json::json!("high"))],
    );
    let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(baseten, "live Baseten API (BASETEN_API_KEY)", {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    let llm = model("baseten", "zai-org/GLM-5.2");
    let options = opts(
        env("BASETEN_API_KEY"),
        &[("reasoningEffort", serde_json::json!("high"))],
    );
    let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!($name, "live Xiaomi MiMo API", {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, "mimo-v2.6-pro");
            let options = opts(env($env), &[]);
            let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
            assert_total_tokens_equals_components(&first);
            assert_total_tokens_equals_components(&second);
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
            let options = opts(env($env), &[]);
            let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
            assert_total_tokens_equals_components(&first);
            assert_total_tokens_equals_components(&second);
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

group!(openrouter, "live OpenRouter API (OPENROUTER_API_KEY)", {
    if !env_set("OPENROUTER_API_KEY") {
        return;
    }
    // TS 用 5 个后端子模型（deepseek/deepseek-chat 重复一次）。
    for model_id in [
        "anthropic/claude-sonnet-4.5",
        "deepseek/deepseek-chat",
        "mistralai/mistral-small-3.2-24b-instruct",
        "google/gemini-2.5-flash",
        "deepseek/deepseek-chat",
    ] {
        let llm = model("openrouter", model_id);
        let options = opts(env("OPENROUTER_API_KEY"), &[]);
        let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
        assert_total_tokens_equals_components(&first);
        assert_total_tokens_equals_components(&second);
    }
});

group!(
    github_copilot,
    "live GitHub Copilot OAuth (github-copilot credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
            return;
        };
        for model_id in ["claude-haiku-4.5", "claude-sonnet-4.6"] {
            let llm = model("github-copilot", model_id);
            let options = opts(Some(token.clone()), &[]);
            let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
            assert_total_tokens_equals_components(&first);
            assert_total_tokens_equals_components(&second);
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
    let (first, second) = test_total_tokens_with_cache(&llm, &StreamOptions::default()).await;
    assert_total_tokens_equals_components(&first);
    assert_total_tokens_equals_components(&second);
});

group!(
    openai_codex,
    "live OpenAI Codex OAuth (openai-codex credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
            return;
        };
        let llm = model("openai-codex", "gpt-6.1-sol");
        let options = opts(Some(token), &[]);
        let (first, second) = test_total_tokens_with_cache(&llm, &options).await;
        assert_total_tokens_equals_components(&first);
        assert_total_tokens_equals_components(&second);
    }
);
