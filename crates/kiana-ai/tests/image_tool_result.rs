//! `image-tool-result.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 验证 tool result 只含图片、以及文本+图片时的多模态传递：
//! 第一次请求必须得到 `stopReason: "toolUse"` 且工具名正确；把工具结果（图片）追加进
//! 上下文后，第二次请求必须 `stopReason: "stop"` 且模型能描述出图片里的红色圆形
//! （文本+图片用例还要求引用文本里的 diameter/100/pixel）。
//!
//! 不支持图片输入的模型与 TS 一样直接返回（console.log 跳过）；环境变量缺失时与
//! `skipIf` 一样跳过。图片取自 `tests/fixtures/compat-entrypoints/red-circle.png`。

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

fn base64_image() -> String {
    use base64::Engine as _;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/compat-entrypoints/red-circle.png"
    );
    let bytes = std::fs::read(path).expect("red-circle.png fixture");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn empty_tool(name: &str, description: &str) -> Tool {
    Tool {
        name: name.into(),
        description: description.into(),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
    }
}

fn first_tool_call(response: &AssistantMessage) -> &ToolCall {
    response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .expect("Expected tool call")
}

fn first_text(response: &AssistantMessage) -> String {
    response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .expect("text content")
}

/// `handleToolWithImageResult`。
async fn handle_tool_with_image_result(llm: &Model, options: &StreamOptions) {
    if !llm.input.contains(&ModelInputModality::Image) {
        return;
    }
    let base64_image = base64_image();

    let tool = empty_tool("get_circle", "Returns a circle image for visualization");
    let mut context = Context {
        system_prompt: Some("You are a helpful assistant that uses tools when asked.".into()),
        messages: vec![user_text(
            "Call the get_circle tool to get an image, and describe what you see, shapes, colors, etc.",
        )],
        tools: Some(vec![tool]),
    };

    let first_response = complete(llm, &context, clone_options(options)).await;
    assert_eq!(first_response.stop_reason, StopReason::ToolUse);
    let tool_call = first_tool_call(&first_response);
    assert_eq!(tool_call.name, "get_circle");
    let (tool_call_id, tool_name) = (tool_call.id.clone(), tool_call.name.clone());
    context.messages.push(Message::Assistant(first_response));

    // 只含图片（没有文本）的 tool result。
    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id,
        tool_name,
        content: vec![UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: base64_image,
            mime_type: "image/png".into(),
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: common::event_stream::now_ms(),
        duration_ms: None,
    };
    context.messages.push(Message::ToolResult(tool_result));

    let second_response = complete(llm, &context, clone_options(options)).await;
    assert_eq!(second_response.stop_reason, StopReason::Stop);
    assert!(
        second_response.error_message.is_none(),
        "{:?}",
        second_response.error_message
    );
    let text = first_text(&second_response).to_lowercase();
    assert!(text.contains("red"), "unexpected text: {text}");
    assert!(text.contains("circle"), "unexpected text: {text}");
}

/// `handleToolWithTextAndImageResult`。
async fn handle_tool_with_text_and_image_result(llm: &Model, options: &StreamOptions) {
    if !llm.input.contains(&ModelInputModality::Image) {
        return;
    }
    let base64_image = base64_image();

    let tool = empty_tool(
        "get_circle_with_description",
        "Returns a circle image with a text description",
    );
    let mut context = Context {
        system_prompt: Some("You are a helpful assistant that uses tools when asked.".into()),
        messages: vec![user_text(
            "Use the get_circle_with_description tool and tell me what you learned. Also say what color the shape is.",
        )],
        tools: Some(vec![tool]),
    };

    let first_response = complete(llm, &context, clone_options(options)).await;
    assert_eq!(first_response.stop_reason, StopReason::ToolUse);
    let tool_call = first_tool_call(&first_response);
    assert_eq!(tool_call.name, "get_circle_with_description");
    let (tool_call_id, tool_name) = (tool_call.id.clone(), tool_call.name.clone());
    context.messages.push(Message::Assistant(first_response));

    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id,
        tool_name,
        content: vec![
            UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "This is a geometric shape with specific properties: it has a diameter of 100 pixels.".into(),
                text_signature: None,
            }),
            UserContentBlock::Image(ImageContent {
                kind_image: ImageKind::Image,
                data: base64_image,
                mime_type: "image/png".into(),
            }),
        ],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: common::event_stream::now_ms(),
        duration_ms: None,
    };
    context.messages.push(Message::ToolResult(tool_result));

    let second_response = complete(llm, &context, clone_options(options)).await;
    assert_eq!(second_response.stop_reason, StopReason::Stop);
    assert!(
        second_response.error_message.is_none(),
        "{:?}",
        second_response.error_message
    );
    let text = first_text(&second_response).to_lowercase();
    assert!(
        text.contains("diameter") || text.contains("100") || text.contains("pixel"),
        "unexpected text: {text}"
    );
    assert!(text.contains("red"), "unexpected text: {text}");
    assert!(text.contains("circle"), "unexpected text: {text}");
}

async fn run_all(llm: &Model, options: &StreamOptions) {
    handle_tool_with_image_result(llm, options).await;
    handle_tool_with_text_and_image_result(llm, options).await;
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
group!(openai_completions, "live OpenAI API (OPENAI_API_KEY)", {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let mut llm = model("openai", "gpt-4o-mini");
    // TS 解构掉目录模型的 `compat`（`const { compat: _compat, ...baseModel }`），
    // 让 openai-completions 用自动检测的 compat（supportsStrictMode=false）。
    llm.compat = None;
    // TS 把该用例切到 `openai-completions` api。
    llm.api = "openai-completions".into();
    run_all(&llm, &StreamOptions::default()).await;
});
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
    openrouter,
    "OPENROUTER_API_KEY",
    "openrouter",
    "z-ai/glm-4.5v",
    "live OpenRouter API (OPENROUTER_API_KEY)"
);
basic_group!(
    mistral,
    "MISTRAL_API_KEY",
    "mistral",
    "pixtral-12b",
    "live Mistral API (MISTRAL_API_KEY)"
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

group!(together, "live Together AI API (TOGETHER_API_KEY)", {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    let llm = model("together", "moonshotai/Kimi-K3");
    let options = opts(&[("reasoningEffort", json!("high"))]);
    run_all(&llm, &options).await;
});

macro_rules! xiaomi_family {
    ($name:ident, $env:literal, $provider:literal) => {
        group!(
            $name,
            "live Xiaomi MiMo API (text+image case is it.skip upstream)",
            {
                if !env_set($env) {
                    return;
                }
                let llm = model($provider, "mimo-v2.6-pro");
                handle_tool_with_image_result(&llm, &StreamOptions::default()).await;
                // TS 把「文本+图片」用例标记为 `it.skip`（多模态融合的上游缺陷）。
                if env_set("KIANA_RUN_KNOWN_BROKEN") {
                    handle_tool_with_text_and_image_result(&llm, &StreamOptions::default()).await;
                }
            }
        );
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
        let llm = model("anthropic", "claude-sonnet-4-5");
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
