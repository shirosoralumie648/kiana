//! `packages/ai/test/abort.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! TS 的每个用例都要真实调用 provider（Google / OpenAI / Azure / Anthropic / Mistral /
//! Together / Baseten / MiniMax / Xiaomi / Qwen / Kimi / Vercel / Codex / Bedrock）并在流中途
//! abort；`describe.skipIf(!env)` 在这里由测试开头的环境检查复刻（缺凭据时直接返回），
//! `retry: 3` 在 Rust 侧没有对应物。
//!
//! 中止语义本身（`AbortScope`、`operation_signal`、`race_with_abort_signal`）另由
//! `utils/abort.rs`、`utils/abort_signals.rs` 的模块内单元测试覆盖。

mod common;

use kiana_ai::compat::{complete, get_model, stream};
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, Context, Message, Model, StopReason, StreamOptions,
    UserContent, UserMessage, UserRole,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn model(provider: &str, id: &str) -> Model {
    get_model(provider, id).unwrap_or_else(|| panic!("model {provider}/{id} not found"))
}

fn opts(extras: &[(&str, Value)]) -> StreamOptions {
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
        timestamp: 0,
    })
}

fn abort_context() -> Context {
    Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: vec![user(
            "What is 15 + 27? Think step by step. Then list 50 first names.",
        )],
        tools: None,
    }
}

/// `testAbortSignal`：流到 50 个文本码元后中止，断言 aborted 且内容非空，再用同一上下文
/// 完成一次后续请求（`stop`、内容非空）。
async fn test_abort_signal(llm: &Model, extras: &[(&str, Value)], api_key: Option<&str>) {
    let controller = CancellationToken::new();
    let mut options = opts(extras);
    options.signal = Some(controller.clone());
    if let Some(api_key) = api_key {
        options.api_key = Some(api_key.to_string());
    }
    let context = abort_context();
    let response = stream(llm, &context, options);
    let mut text = 0usize;
    let mut abort_fired = false;
    while let Some(event) = response.recv().await {
        if abort_fired {
            break;
        }
        if let AssistantMessageEvent::TextDelta { delta, .. }
        | AssistantMessageEvent::ThinkingDelta { delta, .. } = &event
        {
            text += delta.encode_utf16().count();
        }
        if text >= 50 {
            controller.cancel();
            abort_fired = true;
        }
    }
    let msg = response.result().await.expect("aborted message");

    assert_eq!(msg.stop_reason, StopReason::Aborted);
    assert!(!msg.content.is_empty());

    let mut follow_up_context = context;
    follow_up_context.messages.push(Message::Assistant(msg));
    follow_up_context
        .messages
        .push(user("Please continue, but only generate 5 names."));

    let mut follow_up_options = opts(extras);
    if let Some(api_key) = api_key {
        follow_up_options.api_key = Some(api_key.to_string());
    }
    let follow_up = complete(llm, &follow_up_context, follow_up_options).await;
    assert_eq!(follow_up.stop_reason, StopReason::Stop);
    assert!(!follow_up.content.is_empty());
}

/// `testImmediateAbort`：流开始前就中止。
async fn test_immediate_abort(llm: &Model, extras: &[(&str, Value)], api_key: Option<&str>) {
    let controller = CancellationToken::new();
    controller.cancel();
    let mut options = opts(extras);
    options.signal = Some(controller);
    if let Some(api_key) = api_key {
        options.api_key = Some(api_key.to_string());
    }
    let context = Context {
        system_prompt: None,
        messages: vec![user("Hello")],
        tools: None,
    };
    let response: AssistantMessage = complete(llm, &context, options).await;
    assert_eq!(response.stop_reason, StopReason::Aborted);
}

/// `testAbortThenNewMessage`：立即中止且内容为空，把中止消息放回上下文后新请求仍可完成。
async fn test_abort_then_new_message(llm: &Model, extras: &[(&str, Value)]) {
    let controller = CancellationToken::new();
    controller.cancel();
    let mut options = opts(extras);
    options.signal = Some(controller);
    let context = Context {
        system_prompt: None,
        messages: vec![user("Hello, how are you?")],
        tools: None,
    };
    let aborted = complete(llm, &context, options).await;
    assert_eq!(aborted.stop_reason, StopReason::Aborted);
    assert!(aborted.content.is_empty());

    let mut next = context;
    next.messages.push(Message::Assistant(aborted));
    next.messages.push(user("What is 2 + 2?"));
    let follow_up = complete(llm, &next, opts(extras)).await;
    assert_eq!(follow_up.stop_reason, StopReason::Stop);
    assert!(!follow_up.content.is_empty());
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn google_should_abort_mid_stream() {
    if !env_set("GEMINI_API_KEY") {
        return;
    }
    let llm = model("google", "gemini-2.5-flash");
    test_abort_signal(&llm, &[("thinking", json!({ "enabled": true }))], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn google_should_handle_immediate_abort() {
    if !env_set("GEMINI_API_KEY") {
        return;
    }
    let llm = model("google", "gemini-2.5-flash");
    test_immediate_abort(&llm, &[("thinking", json!({ "enabled": true }))], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_completions_should_abort_mid_stream() {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let mut llm = model("openai", "gpt-4o-mini");
    llm.compat = None;
    llm.api = "openai-completions".into();
    test_abort_signal(&llm, &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_completions_should_handle_immediate_abort() {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    let mut llm = model("openai", "gpt-4o-mini");
    llm.compat = None;
    llm.api = "openai-completions".into();
    test_immediate_abort(&llm, &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_responses_should_abort_mid_stream() {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    test_abort_signal(&model("openai", "gpt-5-mini"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_responses_should_handle_immediate_abort() {
    if !env_set("OPENAI_API_KEY") {
        return;
    }
    test_immediate_abort(&model("openai", "gpt-5-mini"), &[], None).await;
}

fn azure_extras(llm: &Model) -> Vec<(&'static str, Value)> {
    common::credentials::resolve_azure_deployment_name(&llm.id)
        .map(|name| vec![("azureDeploymentName", json!(name))])
        .unwrap_or_default()
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn azure_openai_responses_should_abort_mid_stream() {
    if !common::credentials::has_azure_openai_credentials() {
        return;
    }
    let llm = model("azure", "gpt-4o-mini");
    let extras = azure_extras(&llm);
    test_abort_signal(&llm, &extras, None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn azure_openai_responses_should_handle_immediate_abort() {
    if !common::credentials::has_azure_openai_credentials() {
        return;
    }
    let llm = model("azure", "gpt-4o-mini");
    let extras = azure_extras(&llm);
    test_immediate_abort(&llm, &extras, None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn anthropic_should_abort_mid_stream() {
    if !env_set("ANTHROPIC_OAUTH_TOKEN") {
        return;
    }
    let llm = model("anthropic", "claude-sonnet-4-6");
    test_abort_signal(
        &llm,
        &[
            ("thinkingEnabled", json!(true)),
            ("thinkingBudgetTokens", json!(2048)),
        ],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn anthropic_should_handle_immediate_abort() {
    if !env_set("ANTHROPIC_OAUTH_TOKEN") {
        return;
    }
    let llm = model("anthropic", "claude-sonnet-4-6");
    test_immediate_abort(
        &llm,
        &[
            ("thinkingEnabled", json!(true)),
            ("thinkingBudgetTokens", json!(2048)),
        ],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mistral_should_abort_mid_stream() {
    if !env_set("MISTRAL_API_KEY") {
        return;
    }
    test_abort_signal(&model("mistral", "devstral-medium-latest"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mistral_should_handle_immediate_abort() {
    if !env_set("MISTRAL_API_KEY") {
        return;
    }
    test_immediate_abort(&model("mistral", "devstral-medium-latest"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn together_should_abort_mid_stream() {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    test_abort_signal(
        &model("together", "moonshotai/Kimi-K3"),
        &[("reasoningEffort", json!("high"))],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn together_should_handle_immediate_abort() {
    if !env_set("TOGETHER_API_KEY") {
        return;
    }
    test_immediate_abort(
        &model("together", "moonshotai/Kimi-K3"),
        &[("reasoningEffort", json!("high"))],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn baseten_should_abort_mid_stream() {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    test_abort_signal(
        &model("baseten", "zai-org/GLM-5.2"),
        &[("reasoningEffort", json!("high"))],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn baseten_should_handle_immediate_abort() {
    if !env_set("BASETEN_API_KEY") {
        return;
    }
    test_immediate_abort(
        &model("baseten", "zai-org/GLM-5.2"),
        &[("reasoningEffort", json!("high"))],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn minimax_should_abort_mid_stream() {
    if !env_set("MINIMAX_API_KEY") {
        return;
    }
    test_abort_signal(&model("minimax", "MiniMax-M2.7"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn minimax_should_handle_immediate_abort() {
    if !env_set("MINIMAX_API_KEY") {
        return;
    }
    test_immediate_abort(&model("minimax", "MiniMax-M2.7"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_should_abort_mid_stream() {
    if !env_set("XIAOMI_API_KEY") {
        return;
    }
    test_abort_signal(&model("xiaomi", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_should_handle_immediate_abort() {
    if !env_set("XIAOMI_API_KEY") {
        return;
    }
    test_immediate_abort(&model("xiaomi", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_cn_should_abort_mid_stream() {
    if !env_set("XIAOMI_TOKEN_PLAN_CN_API_KEY") {
        return;
    }
    test_abort_signal(&model("xiaomi-token-plan-cn", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_cn_should_handle_immediate_abort() {
    if !env_set("XIAOMI_TOKEN_PLAN_CN_API_KEY") {
        return;
    }
    test_immediate_abort(&model("xiaomi-token-plan-cn", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_ams_should_abort_mid_stream() {
    if !env_set("XIAOMI_TOKEN_PLAN_AMS_API_KEY") {
        return;
    }
    test_abort_signal(&model("xiaomi-token-plan-ams", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_ams_should_handle_immediate_abort() {
    if !env_set("XIAOMI_TOKEN_PLAN_AMS_API_KEY") {
        return;
    }
    test_immediate_abort(&model("xiaomi-token-plan-ams", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_sgp_should_abort_mid_stream() {
    if !env_set("XIAOMI_TOKEN_PLAN_SGP_API_KEY") {
        return;
    }
    test_abort_signal(&model("xiaomi-token-plan-sgp", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn xiaomi_token_plan_sgp_should_handle_immediate_abort() {
    if !env_set("XIAOMI_TOKEN_PLAN_SGP_API_KEY") {
        return;
    }
    test_immediate_abort(&model("xiaomi-token-plan-sgp", "mimo-v2.6-pro"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_should_abort_mid_stream() {
    if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
        return;
    }
    test_abort_signal(&model("qwen-token-plan", "qwen3.7-max"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_should_handle_immediate_abort() {
    if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
        return;
    }
    test_immediate_abort(&model("qwen-token-plan", "qwen3.7-max"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_individual_should_abort_mid_stream() {
    if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
        return;
    }
    test_abort_signal(
        &model("qwen-token-plan-individual", "qwen3.8-max"),
        &[],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_individual_should_handle_immediate_abort() {
    if !env_set("QWEN_TOKEN_PLAN_API_KEY") {
        return;
    }
    test_immediate_abort(
        &model("qwen-token-plan-individual", "qwen3.8-max"),
        &[],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_cn_should_abort_mid_stream() {
    if !env_set("QWEN_TOKEN_PLAN_CN_API_KEY") {
        return;
    }
    test_abort_signal(&model("qwen-token-plan-cn", "qwen3.7-max"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen_token_plan_cn_should_handle_immediate_abort() {
    if !env_set("QWEN_TOKEN_PLAN_CN_API_KEY") {
        return;
    }
    test_immediate_abort(&model("qwen-token-plan-cn", "qwen3.7-max"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn kimi_coding_should_abort_mid_stream() {
    if !env_set("KIMI_API_KEY") {
        return;
    }
    test_abort_signal(&model("kimi-coding", "kimi-for-coding"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn kimi_coding_should_handle_immediate_abort() {
    if !env_set("KIMI_API_KEY") {
        return;
    }
    test_immediate_abort(&model("kimi-coding", "kimi-for-coding"), &[], None).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn vercel_ai_gateway_should_abort_mid_stream() {
    if !env_set("AI_GATEWAY_API_KEY") {
        return;
    }
    test_abort_signal(
        &model("vercel-ai-gateway", "google/gemini-2.5-flash"),
        &[],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn vercel_ai_gateway_should_handle_immediate_abort() {
    if !env_set("AI_GATEWAY_API_KEY") {
        return;
    }
    test_immediate_abort(
        &model("vercel-ai-gateway", "google/gemini-2.5-flash"),
        &[],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_codex_should_abort_mid_stream() {
    let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    test_abort_signal(&model("openai-codex", "gpt-6.1-sol"), &[], Some(&token)).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn openai_codex_should_handle_immediate_abort() {
    let Some(token) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    test_immediate_abort(&model("openai-codex", "gpt-6.1-sol"), &[], Some(&token)).await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn amazon_bedrock_should_abort_mid_stream() {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    test_abort_signal(
        &model(
            "amazon-bedrock",
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ),
        &[("reasoning", json!("medium"))],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn amazon_bedrock_should_handle_immediate_abort() {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    test_immediate_abort(
        &model(
            "amazon-bedrock",
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ),
        &[],
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn amazon_bedrock_should_handle_abort_then_new_message() {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    test_abort_then_new_message(
        &model(
            "amazon-bedrock",
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ),
        &[],
    )
    .await;
}
