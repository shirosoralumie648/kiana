//! `unicode-surrogate.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 回归场景：tool result 里的 emoji（BMP 之外的字符）不能被序列化成未配对的代理项，
//! 否则 Anthropic 会拒绝请求（`no low surrogate in string`）。
//!
//! Rust 的 `String` 是合法 UTF-8，**不能保存孤立的 UTF-16 代理项**，因此 TS 的
//! `testUnpairedHighSurrogate`（`String.fromCharCode(0xd83d)`）在 Rust 里不可表达；
//! 本文件移植另外两个用例（emoji 与真实 LinkedIn 评论数据），第三个用例在
//! `remainingGaps` 中记录。`retry: 3` 由调用方重试。

mod common;

use kiana_ai::compat::{complete, get_model};
use kiana_ai::types::*;
use serde_json::json;

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

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: common::event_stream::now_ms(),
    })
}

fn empty_tool(name: &str, description: &str) -> Tool {
    Tool {
        name: name.into(),
        description: description.into(),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
    }
}

/// 构造一条带 toolCall 的 assistant 消息（与 TS 的对象字面量一致）。
fn assistant_with_tool_call(llm: &Model, tool_call_id: &str, tool_name: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: tool_call_id.into(),
            name: tool_name.into(),
            arguments: JsonObject::new(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        })],
        api: llm.api.clone(),
        provider: llm.provider.clone(),
        model: llm.id.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: common::event_stream::now_ms(),
        ..AssistantMessage::default()
    }
}

fn tool_result(tool_call_id: &str, tool_name: &str, text: String) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text,
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: common::event_stream::now_ms(),
        duration_ms: None,
    })
}

/// `testEmojiInToolResults`。
async fn test_emoji_in_tool_results(llm: &Model, options: &StreamOptions) {
    let tool_call_id = if llm.provider == "mistral" {
        "testtool1"
    } else {
        "test_1"
    };
    let mut context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: vec![
            user("Use the test tool"),
            Message::Assistant(assistant_with_tool_call(llm, tool_call_id, "test_tool")),
        ],
        tools: Some(vec![empty_tool("test_tool", "A test tool")]),
    };

    context
        .messages
        .push(tool_result(tool_call_id, "test_tool", "Test with emoji 🙈 and other characters:\n- Monkey emoji: 🙈\n- Thumbs up: 👍\n- Heart: ❤️\n- Thinking face: 🤔\n- Rocket: 🚀\n- Mixed text: Mario Zechner wann? Wo? Bin grad äußersr eventuninformiert 🙈\n- Japanese: こんにちは\n- Chinese: 你好\n- Mathematical symbols: ∑∫∂√\n- Special quotes: \"curly\" 'quotes'".into()));
    context
        .messages
        .push(user("Summarize the tool result briefly."));

    let response = complete(llm, &context, clone_options(options)).await;
    assert_ne!(response.stop_reason, StopReason::Error);
    assert!(
        response.error_message.is_none(),
        "{:?}",
        response.error_message
    );
    assert!(!response.content.is_empty());
}

/// `testRealWorldLinkedInData`。
async fn test_real_world_linkedin_data(llm: &Model, options: &StreamOptions) {
    let tool_call_id = if llm.provider == "mistral" {
        "linkedin1"
    } else {
        "linkedin_1"
    };
    let mut context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: vec![
            user("Use the linkedin tool to get comments"),
            Message::Assistant(assistant_with_tool_call(
                llm,
                tool_call_id,
                "linkedin_skill",
            )),
        ],
        tools: Some(vec![empty_tool("linkedin_skill", "Get LinkedIn comments")]),
    };

    context.messages.push(tool_result(
        tool_call_id,
        "linkedin_skill",
        "Post: Hab einen \"Generative KI für Nicht-Techniker\" Workshop gebaut.\nUnanswered Comments: 2\n\n=> {\n  \"comments\": [\n    {\n      \"author\": \"Matthias Neumayer's  graphic link\",\n      \"text\": \"Leider nehmen das viel zu wenige Leute ernst\"\n    },\n    {\n      \"author\": \"Matthias Neumayer's  graphic link\",\n      \"text\": \"Mario Zechner wann? Wo? Bin grad äußersr eventuninformiert 🙈\"\n    }\n  ]\n}".into(),
    ));
    context.messages.push(user("How many comments are there?"));

    let response = complete(llm, &context, clone_options(options)).await;
    assert_ne!(response.stop_reason, StopReason::Error);
    assert!(
        response.error_message.is_none(),
        "{:?}",
        response.error_message
    );
    assert!(
        response
            .content
            .iter()
            .any(|block| matches!(block, AssistantContentBlock::Text(_)))
    );
}

macro_rules! group {
    ($name:ident, $ignore:literal, $body:block) => {
        #[tokio::test]
        #[ignore = $ignore]
        async fn $name() $body
    };
}

macro_rules! emoji_group {
    ($name:ident, $env:literal, $provider:literal, $model_id:literal, $ignore:literal) => {
        group!($name, $ignore, {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, $model_id);
            let options = opts(env($env), &[]);
            test_emoji_in_tool_results(&llm, &options).await;
            test_real_world_linkedin_data(&llm, &options).await;
        });
    };
}

emoji_group!(
    google,
    "GEMINI_API_KEY",
    "google",
    "gemini-2.5-flash",
    "live Google Gemini API (GEMINI_API_KEY)"
);
emoji_group!(
    openai_completions,
    "OPENAI_API_KEY",
    "openai",
    "gpt-4o-mini",
    "live OpenAI API (OPENAI_API_KEY)"
);
emoji_group!(
    openai_responses,
    "OPENAI_API_KEY",
    "openai",
    "gpt-5-mini",
    "live OpenAI API (OPENAI_API_KEY)"
);
emoji_group!(
    anthropic,
    "ANTHROPIC_API_KEY",
    "anthropic",
    "claude-haiku-4-5",
    "live Anthropic API (ANTHROPIC_API_KEY)"
);
emoji_group!(
    xai,
    "XAI_API_KEY",
    "xai",
    "grok-4.3",
    "live xAI API (XAI_API_KEY)"
);
emoji_group!(
    groq,
    "GROQ_API_KEY",
    "groq",
    "openai/gpt-oss-20b",
    "live Groq API (GROQ_API_KEY)"
);
emoji_group!(
    cerebras,
    "CEREBRAS_API_KEY",
    "cerebras",
    "gpt-oss-120b",
    "live Cerebras API (CEREBRAS_API_KEY)"
);
emoji_group!(
    zai,
    "ZAI_API_KEY",
    "zai",
    "glm-5.2",
    "live zAI API (ZAI_API_KEY)"
);
emoji_group!(
    mistral,
    "MISTRAL_API_KEY",
    "mistral",
    "devstral-medium-latest",
    "live Mistral API (MISTRAL_API_KEY)"
);
emoji_group!(
    minimax,
    "MINIMAX_API_KEY",
    "minimax",
    "MiniMax-M2.7",
    "live MiniMax API (MINIMAX_API_KEY)"
);
emoji_group!(
    kimi_coding,
    "KIMI_API_KEY",
    "kimi-coding",
    "kimi-for-coding",
    "live Kimi For Coding API (KIMI_API_KEY)"
);
emoji_group!(
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
            Some(name) => opts(None, &[("azureDeploymentName", json!(name))]),
            None => StreamOptions::default(),
        };
        test_emoji_in_tool_results(&llm, &options).await;
        test_real_world_linkedin_data(&llm, &options).await;
    }
);

group!(cloudflare_workers_ai, "live Cloudflare Workers AI", {
    if !common::credentials::has_cloudflare_workers_ai_credentials() {
        return;
    }
    let llm = model("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6");
    test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
    test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
});

group!(cloudflare_ai_gateway, "live Cloudflare AI Gateway", {
    if !common::credentials::has_cloudflare_ai_gateway_credentials() {
        return;
    }
    let llm = model(
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    );
    test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
    test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
});

group!(huggingface, "live Hugging Face API (HF_TOKEN)", {
    if !env_set("HF_TOKEN") {
        return;
    }
    let llm = model("huggingface", "moonshotai/Kimi-K2.5");
    test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
    test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
});

group!(together, "live Together AI API (TOGETHER_API_KEY)", {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    let llm = model("together", "moonshotai/Kimi-K3");
    let options = opts(None, &[("reasoningEffort", json!("high"))]);
    test_emoji_in_tool_results(&llm, &options).await;
    test_real_world_linkedin_data(&llm, &options).await;
});

group!(baseten, "live Baseten API (BASETEN_API_KEY)", {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    let llm = model("baseten", "zai-org/GLM-5.2");
    let options = opts(None, &[("reasoningEffort", json!("high"))]);
    test_emoji_in_tool_results(&llm, &options).await;
    test_real_world_linkedin_data(&llm, &options).await;
});

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!($name, "live Xiaomi MiMo API", {
            if !env_set($env) {
                return;
            }
            let llm = model($provider, "mimo-v2.6-pro");
            test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
            test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
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
            test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
            test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
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
        let options = opts(Some(token), &[]);
        test_emoji_in_tool_results(&llm, &options).await;
        test_real_world_linkedin_data(&llm, &options).await;
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
            let options = opts(Some(token.clone()), &[]);
            test_emoji_in_tool_results(&llm, &options).await;
            test_real_world_linkedin_data(&llm, &options).await;
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
    test_emoji_in_tool_results(&llm, &StreamOptions::default()).await;
    test_real_world_linkedin_data(&llm, &StreamOptions::default()).await;
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
        test_emoji_in_tool_results(&llm, &options).await;
        test_real_world_linkedin_data(&llm, &options).await;
    }
);
