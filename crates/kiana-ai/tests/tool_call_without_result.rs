//! `tool-call-without-result.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 场景：assistant 发起了 toolCall，但上下文里没有对应的 toolResult（模拟工具调用被
//! 取消），随后用户又发了一条消息。适配器必须过滤掉孤立的 toolCall，请求不能因此失败。
//!
//! 断言：第一次响应必须含 toolCall；第二次响应 `stopReason !== "error"`、内容非空、
//! 文本或新的 toolCall 至少有一个，且 `stopReason ∈ {stop, toolUse}`。
//! 环境变量缺失时与 `skipIf` 一样跳过；`retry: 3` 由调用方重试。

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

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: common::event_stream::now_ms(),
    })
}

fn calculate_tool() -> Tool {
    Tool {
        name: "calculate".into(),
        description: "Evaluate mathematical expressions".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "The mathematical expression to evaluate" }
            },
            "required": ["expression"]
        }),
        constrained_sampling: None,
    }
}

/// `testToolCallWithoutResult`。
async fn test_tool_call_without_result(llm: &Model, options: &StreamOptions) {
    let mut context = Context {
        system_prompt: Some(
            "You are a helpful assistant. Use the calculate tool when asked to perform calculations."
                .into(),
        ),
        messages: Vec::new(),
        tools: Some(vec![calculate_tool()]),
    };
    context
        .messages
        .push(user("Please calculate 25 * 18 using the calculate tool."));

    let first_response = complete(llm, &context, clone_options(options)).await;
    context
        .messages
        .push(Message::Assistant(first_response.clone()));

    let has_tool_call = first_response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)));
    assert!(has_tool_call, "Expected assistant to make a tool call");

    // 不提供 toolResult，直接再发一条用户消息（模拟被取消的 tool call）。
    context
        .messages
        .push(user("Never mind, just tell me what is 2+2?"));

    let second_response = complete(llm, &context, clone_options(options)).await;
    assert_ne!(
        second_response.stop_reason,
        StopReason::Error,
        "unexpected error: {:?}",
        second_response.error_message
    );
    assert!(!second_response.content.is_empty());

    let text = second_response
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let tool_calls = second_response
        .content
        .iter()
        .filter(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
        .count();
    assert!(tool_calls > 0 || !text.is_empty());
    assert!(
        matches!(
            second_response.stop_reason,
            StopReason::Stop | StopReason::ToolUse
        ),
        "unexpected stop reason: {:?}",
        second_response.stop_reason
    );
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
            test_tool_call_without_result(&llm, &StreamOptions::default()).await;
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

group!(openai_completions, "live OpenAI API (OPENAI_API_KEY)", {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let mut llm = model("openai", "gpt-4o-mini");
    // TS 解构掉目录模型的 `compat`（`const { compat: _compat, ...baseModel }`），
    // 让 openai-completions 用自动检测的 compat（supportsStrictMode=false）。
    llm.compat = None;
    llm.api = "openai-completions".into();
    test_tool_call_without_result(&llm, &StreamOptions::default()).await;
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
            Some(name) => opts(&[("azureDeploymentName", json!(name))]),
            None => StreamOptions::default(),
        };
        test_tool_call_without_result(&llm, &options).await;
    }
);

group!(cloudflare_workers_ai, "live Cloudflare Workers AI", {
    if !common::credentials::has_cloudflare_workers_ai_credentials() {
        return;
    }
    let llm = model("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6");
    test_tool_call_without_result(&llm, &StreamOptions::default()).await;
});

group!(cloudflare_ai_gateway, "live Cloudflare AI Gateway", {
    if !common::credentials::has_cloudflare_ai_gateway_credentials() {
        return;
    }
    let llm = model(
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    );
    test_tool_call_without_result(&llm, &StreamOptions::default()).await;
});

group!(huggingface, "live Hugging Face API (HF_TOKEN)", {
    if !env_set("HF_TOKEN") {
        return;
    }
    let llm = model("huggingface", "moonshotai/Kimi-K2.5");
    test_tool_call_without_result(&llm, &StreamOptions::default()).await;
});

group!(together, "live Together AI API (TOGETHER_API_KEY)", {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    let llm = model("together", "moonshotai/Kimi-K3");
    test_tool_call_without_result(&llm, &opts(&[("reasoningEffort", json!("high"))])).await;
});

group!(baseten, "live Baseten API (BASETEN_API_KEY)", {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    let llm = model("baseten", "zai-org/GLM-5.2");
    test_tool_call_without_result(&llm, &opts(&[("reasoningEffort", json!("high"))])).await;
});

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!($name, "live Xiaomi MiMo API", {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, "mimo-v2.6-pro");
            test_tool_call_without_result(&llm, &StreamOptions::default()).await;
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
            test_tool_call_without_result(&llm, &StreamOptions::default()).await;
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
        test_tool_call_without_result(&llm, &options).await;
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
            test_tool_call_without_result(&llm, &options).await;
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
    test_tool_call_without_result(&llm, &StreamOptions::default()).await;
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
        test_tool_call_without_result(&llm, &options).await;
    }
);
