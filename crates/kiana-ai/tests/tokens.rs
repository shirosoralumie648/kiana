//! `tokens.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 验证「流式输出中途 abort」后各 provider 上报的 token 统计形状：`stopReason` 必须是
//! `"aborted"`，并按 provider 的上报时机断言 input/output 是否为 0。
//!
//! TS 用 `AbortController`，Rust 用 `StreamOptions.signal` 的
//! [`tokio_util::sync::CancellationToken`]；`for await` 的文本/思考增量累计到 1000 个
//! UTF-16 码元（TS `text.length`）时取消，语义相同。Xiaomi 的 4 个用例在 TS 里是 `it.skip`（上游在 abort 时不报 usage），
//! 这里用 `KIANA_RUN_KNOWN_BROKEN` 环境变量控制是否执行。`retry: 3` 没有对应物（由调用方重试）。

mod common;

use kiana_ai::compat::{get_model, get_models, stream};
use kiana_ai::types::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

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

/// `testTokensOnAbort`。
async fn test_tokens_on_abort(llm: &Model, options: &StreamOptions) {
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(
                "Write a long poem with 20 stanzas about the beauty of nature.".into(),
            ),
            timestamp: common::event_stream::now_ms(),
        })],
        tools: None,
    };

    let token = CancellationToken::new();
    let mut options = clone_options(options);
    options.signal = Some(token.clone());

    let response = stream(llm, &context, options);
    let mut abort_fired = false;
    let mut text = String::new();
    while let Some(event) = response.recv().await {
        let delta = match &event {
            AssistantMessageEvent::TextDelta { delta, .. } => Some(delta),
            AssistantMessageEvent::ThinkingDelta { delta, .. } => Some(delta),
            _ => None,
        };
        if let Some(delta) = delta
            && !abort_fired
        {
            text.push_str(delta);
            // TS: `text.length >= 1000` 是 **UTF-16 码元**数（非 Unicode 标量数）。
            if text.encode_utf16().count() >= 1000 {
                abort_fired = true;
                token.cancel();
            }
        }
    }

    let message = response.result().await.expect("stream result");
    assert_eq!(message.stop_reason, StopReason::Aborted);

    // 这些 api/provider 只在最后一个 chunk 上报 usage，abort 后没有任何统计。
    let final_chunk_usage = matches!(
        llm.api.as_str(),
        "openai-completions"
            | "mistral-conversations"
            | "openai-responses"
            | "azure-openai-responses"
            | "openai-codex-responses"
    ) || llm.provider == "zai"
        || llm.provider == "amazon-bedrock"
        || llm.provider == "vercel-ai-gateway";

    if final_chunk_usage {
        assert_eq!(message.usage.input, 0);
        assert_eq!(message.usage.output, 0);
    } else if llm.provider == "minimax" {
        // MiniMax M2.7 对 aborted 请求不上报 token 统计。
        assert_eq!(message.usage.input, 0);
        assert_eq!(message.usage.output, 0);
    } else if llm.provider == "kimi-coding" {
        // Kimi 提前上报 input，output 只在最终 chunk。
        assert!(message.usage.input > 0);
        assert_eq!(message.usage.output, 0);
    } else {
        assert!(message.usage.input > 0);
        assert!(message.usage.output > 0);
        // 部分 provider（Copilot）的 cost 费率为 0。
        if llm.cost.input > 0.0 {
            assert!(message.usage.cost.input > 0.0);
            assert!(message.usage.cost.total > 0.0);
        }
    }
}

macro_rules! group {
    ($name:ident, $ignore:literal, $body:block) => {
        #[tokio::test]
        #[ignore = $ignore]
        async fn $name() $body
    };
}

group!(
    google_provider,
    "live Google Gemini API (GEMINI_API_KEY)",
    {
        if !env_set("GEMINI_API_KEY") {
            return;
        }
        let llm = model("google", "gemini-2.5-flash");
        test_tokens_on_abort(&llm, &opts(&[("thinking", json!({ "enabled": true }))])).await;
    }
);

group!(
    openai_completions_provider,
    "live OpenAI API (OPENAI_API_KEY)",
    {
        if !env_set("OPENAI_API_KEY") {
            return;
        }
        let mut llm = model("openai", "gpt-4o-mini");
        // TS 解构掉目录模型的 `compat`（`const { compat: _compat, ...baseModel }`），
        // 让 openai-completions 用自动检测的 compat（supportsStrictMode=false）。
        llm.compat = None;
        llm.api = "openai-completions".into();
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(
    openai_responses_provider,
    "live OpenAI API (OPENAI_API_KEY)",
    {
        if !env_set("OPENAI_API_KEY") {
            return;
        }
        let llm = model("openai", "gpt-5.4-mini");
        test_tokens_on_abort(&llm, &opts(&[("reasoningEffort", json!("low"))])).await;
    }
);

group!(
    azure_openai_responses_provider,
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
        test_tokens_on_abort(&llm, &options).await;
    }
);

group!(
    anthropic_provider,
    "live Anthropic API (ANTHROPIC_API_KEY)",
    {
        if !env_set("ANTHROPIC_API_KEY") {
            return;
        }
        let llm = model("anthropic", "claude-sonnet-4-6");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(xai_provider, "live xAI API (XAI_API_KEY)", {
    if !env_set("XAI_API_KEY") {
        return;
    }
    let llm = model("xai", "grok-4.3");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(groq_provider, "live Groq API (GROQ_API_KEY)", {
    if !env_set("GROQ_API_KEY") {
        return;
    }
    let llm = model("groq", "openai/gpt-oss-20b");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(cerebras_provider, "live Cerebras API (CEREBRAS_API_KEY)", {
    if !env_set("CEREBRAS_API_KEY") {
        return;
    }
    let preferred = ["gpt-oss-120b", "zai-glm-4.7", "llama3.1-8b"];
    let models = get_models("cerebras");
    let llm = models
        .iter()
        .find(|model| preferred.contains(&model.id.as_str()))
        .or_else(|| models.first())
        .unwrap_or_else(|| panic!("No Cerebras models available"));
    test_tokens_on_abort(llm, &StreamOptions::default()).await;
});

group!(
    cloudflare_workers_ai_provider,
    "live Cloudflare Workers AI",
    {
        if !common::credentials::has_cloudflare_workers_ai_credentials() {
            return;
        }
        let llm = model("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(
    cloudflare_ai_gateway_provider,
    "live Cloudflare AI Gateway",
    {
        if !common::credentials::has_cloudflare_ai_gateway_credentials() {
            return;
        }
        let llm = model(
            "cloudflare-ai-gateway",
            "workers-ai/@cf/moonshotai/kimi-k2.6",
        );
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(huggingface_provider, "live Hugging Face API (HF_TOKEN)", {
    if !env_set("HF_TOKEN") {
        return;
    }
    let llm = model("huggingface", "moonshotai/Kimi-K2.5");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(
    together_provider,
    "live Together AI API (TOGETHER_API_KEY)",
    {
        if !env_set("TOGETHER_API_KEY") {
            return;
        }
        let llm = model("together", "moonshotai/Kimi-K3");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(baseten_provider, "live Baseten API (BASETEN_API_KEY)", {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    let llm = model("baseten", "zai-org/GLM-5.2");
    test_tokens_on_abort(&llm, &opts(&[("reasoningEffort", json!("high"))])).await;
});

group!(zai_provider, "live zAI API (ZAI_API_KEY)", {
    if !env_set("ZAI_API_KEY") {
        return;
    }
    let llm = model("zai", "glm-5.2");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(mistral_provider, "live Mistral API (MISTRAL_API_KEY)", {
    if !env_set("MISTRAL_API_KEY") {
        return;
    }
    let llm = model("mistral", "devstral-medium-latest");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(minimax_provider, "live MiniMax API (MINIMAX_API_KEY)", {
    if !env_set("MINIMAX_API_KEY") {
        return;
    }
    let llm = model("minimax", "MiniMax-M2.7");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(
    kimi_coding_provider,
    "live Kimi For Coding API (KIMI_API_KEY)",
    {
        if !env_set("KIMI_API_KEY") {
            return;
        }
        let llm = model("kimi-coding", "kimi-for-coding");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(meta_provider, "live Meta API (META_API_KEY)", {
    if !env_set("META_API_KEY") {
        return;
    }
    let llm = model("meta", "muse-spark-1.3");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(
    vercel_ai_gateway_provider,
    "live Vercel AI Gateway (AI_GATEWAY_API_KEY)",
    {
        if !env_set("AI_GATEWAY_API_KEY") {
            return;
        }
        let llm = model("vercel-ai-gateway", "google/gemini-2.5-flash");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!(
            $name,
            "known-broken upstream (Xiaomi aborted usage); set KIANA_RUN_KNOWN_BROKEN=1",
            {
                if !env_set("KIANA_RUN_KNOWN_BROKEN") || !env_set($env) {
                    return;
                }
                let llm = model($provider, "mimo-v2.6-pro");
                test_tokens_on_abort(&llm, &StreamOptions::default()).await;
            }
        );
    };
}

xiaomi_family!(xiaomi_provider, "XIAOMI_API_KEY", "xiaomi");
xiaomi_family!(
    xiaomi_token_plan_cn_provider,
    "XIAOMI_TOKEN_PLAN_CN_API_KEY",
    "xiaomi-token-plan-cn"
);
xiaomi_family!(
    xiaomi_token_plan_ams_provider,
    "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
    "xiaomi-token-plan-ams"
);
xiaomi_family!(
    xiaomi_token_plan_sgp_provider,
    "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
    "xiaomi-token-plan-sgp"
);

group!(qwen_token_plan_provider, "live Qwen Token Plan API", {
    if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
        return;
    }
    let llm = model("qwen-token-plan", "qwen3.7-max");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(
    qwen_token_plan_individual_provider,
    "live Qwen Token Plan API",
    {
        if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
            return;
        }
        let llm = model("qwen-token-plan-individual", "qwen3.8-max");
        test_tokens_on_abort(&llm, &StreamOptions::default()).await;
    }
);

group!(qwen_token_plan_cn_provider, "live Qwen Token Plan API", {
    if !env_set("QWEN_TOKEN_PLAN_CN_API_KEY") {
        return;
    }
    let llm = model("qwen-token-plan-cn", "qwen3.7-max");
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});

group!(
    anthropic_oauth_provider,
    "live Anthropic OAuth (anthropic credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("anthropic") else {
            return;
        };
        let llm = model("anthropic", "claude-sonnet-4-6");
        let options = StreamOptions {
            api_key: Some(token),
            ..StreamOptions::default()
        };
        test_tokens_on_abort(&llm, &options).await;
    }
);

group!(
    github_copilot_haiku_provider,
    "live GitHub Copilot OAuth (github-copilot credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
            return;
        };
        let llm = model("github-copilot", "claude-haiku-4.5");
        let options = StreamOptions {
            api_key: Some(token),
            ..StreamOptions::default()
        };
        test_tokens_on_abort(&llm, &options).await;
    }
);

group!(
    github_copilot_sonnet_provider,
    "live GitHub Copilot OAuth (github-copilot credential)",
    {
        let Some(token) = common::oauth::resolve_api_key("github-copilot") else {
            return;
        };
        let llm = model("github-copilot", "claude-sonnet-4.6");
        let options = StreamOptions {
            api_key: Some(token),
            ..StreamOptions::default()
        };
        test_tokens_on_abort(&llm, &options).await;
    }
);

group!(
    openai_codex_provider,
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
        test_tokens_on_abort(&llm, &options).await;
    }
);

group!(bedrock_provider, "live Amazon Bedrock (AWS credentials)", {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    let llm = model(
        "amazon-bedrock",
        "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
    );
    test_tokens_on_abort(&llm, &StreamOptions::default()).await;
});
