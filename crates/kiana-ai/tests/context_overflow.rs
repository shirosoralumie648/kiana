//! `packages/ai/test/context-overflow.test.ts` 的 Rust 移植（真实 API，全部 `#[ignore]`）。
//!
//! 每个用例都用超长输入触发上下文溢出，再断言 `is_context_overflow` 能识别；
//! `describe.skipIf(!env)` 在这里是测试开头的环境检查（缺凭据时直接返回）。
//! `isContextOverflow` 本身的分类逻辑另由 `tests/overflow.rs` 用静态样本覆盖。

mod common;

use std::time::Duration;

use kiana_ai::compat::{complete, get_model, get_models};
use kiana_ai::types::{
    AssistantMessage, Context, Message, Model, ModelCost, ModelInputModality, StopReason,
    StreamOptions, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::overflow::is_context_overflow;

const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. ";

/// `generateOverflowContent`：`contextWindow + 10000` tokens，按 chars/4 估算并乘 1.5。
fn generate_overflow_content(context_window: u64) -> String {
    let target_tokens = context_window as f64 + 10_000.0;
    let target_chars = (target_tokens * 4.0 * 1.5) as usize;
    let repetitions = target_chars.div_ceil(LOREM_IPSUM.len());
    LOREM_IPSUM.repeat(repetitions)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn model(provider: &str, id: &str) -> Model {
    get_model(provider, id).unwrap_or_else(|| panic!("model {provider}/{id} not found"))
}

fn local_model(provider: &str, base_url: &str, context_window: u64, max_tokens: u64) -> Model {
    Model {
        id: "local-model".into(),
        name: base_url.into(),
        api: "openai-completions".into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(context_window),
        max_tokens: Some(max_tokens),
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        ..Default::default()
    }
}

fn tcp_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )
    .is_ok()
}

struct OverflowResult {
    model: Model,
    response: AssistantMessage,
    has_usage_data: bool,
}

/// `testContextOverflow`：用超长 user 消息 complete，并记录 usage 是否可用。
async fn test_context_overflow(model: Model, api_key: &str) -> OverflowResult {
    let overflow_content = generate_overflow_content(model.context_window.unwrap_or_default());
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(overflow_content),
            timestamp: 0,
        })],
        tools: None,
    };
    let mut options = StreamOptions::default();
    if !api_key.is_empty() {
        options.api_key = Some(api_key.to_string());
    }
    let response = complete(&model, &context, options).await;
    let has_usage_data = response.usage.input > 0 || response.usage.cache_read > 0;
    OverflowResult {
        model,
        response,
        has_usage_data,
    }
}

/// 错误文本必须包含 `needle`（大小写不敏感，等价 TS 的 `/needle/i`），且
/// `isContextOverflow` 为 true。
fn assert_error_overflow(result: &OverflowResult, needle: &str) {
    assert_eq!(
        result.response.stop_reason,
        StopReason::Error,
        "provider {}: {:?}",
        result.model.provider,
        result.response.error_message
    );
    let message = result
        .response
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains(&needle.to_lowercase()),
        "provider {}: error {:?} does not contain {needle:?}",
        result.model.provider,
        result.response.error_message
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

/// 只要求 error + `isContextOverflow`。
fn assert_plain_overflow(result: &OverflowResult) {
    assert_eq!(
        result.response.stop_reason,
        StopReason::Error,
        "provider {}: {:?}",
        result.model.provider,
        result.response.error_message
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

/// Xiaomi 静默截断：`length` + `output == 0` 才是可检测的溢出信号。
fn assert_length_overflow(result: &OverflowResult) {
    assert_eq!(result.response.stop_reason, StopReason::Length);
    assert_eq!(result.response.usage.output, 0);
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

// ---------------- Anthropic ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn claude_haiku_4_5_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("ANTHROPIC_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("anthropic", "claude-haiku-4-5"), &key).await;
    assert_error_overflow(&result, "prompt is too long");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn claude_sonnet_4_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("ANTHROPIC_OAUTH_TOKEN") else {
        return;
    };
    let result = test_context_overflow(model("anthropic", "claude-sonnet-4-6"), &key).await;
    assert_error_overflow(&result, "prompt is too long");
}

// ---------------- GitHub Copilot ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn google_model_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = common::oauth::resolve_api_key("github-copilot") else {
        return;
    };
    let candidate = get_models("github-copilot")
        .into_iter()
        .find(|candidate| candidate.id.starts_with("gemini-"))
        .expect("No Google models available through GitHub Copilot");
    let result = test_context_overflow(candidate, &key).await;
    assert_error_overflow(&result, "exceeds the limit of ");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn claude_sonnet_4_should_detect_overflow_via_iscontextoverflow_2() {
    let Some(key) = common::oauth::resolve_api_key("github-copilot") else {
        return;
    };
    let result = test_context_overflow(model("github-copilot", "claude-sonnet-4.6"), &key).await;
    assert_eq!(result.response.stop_reason, StopReason::Error);
    let message = result
        .response
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("exceeds the limit of ") || message.contains("input is too long"),
        "unexpected error {:?}",
        result.response.error_message
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

// ---------------- OpenAI ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gpt_4o_mini_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENAI_API_KEY") else {
        return;
    };
    let mut llm = model("openai", "gpt-4o-mini");
    llm.api = "openai-completions".into();
    let result = test_context_overflow(llm, &key).await;
    assert_error_overflow(&result, "maximum context length");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gpt_4o_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENAI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("openai", "gpt-4o"), &key).await;
    assert_error_overflow(&result, "exceeds the context window");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gpt_4o_mini_should_detect_overflow_via_iscontextoverflow_2() {
    if !common::credentials::has_azure_openai_credentials() {
        return;
    }
    let key = env("AZURE_OPENAI_API_KEY").unwrap_or_default();
    let result = test_context_overflow(model("azure", "gpt-4o-mini"), &key).await;
    assert_eq!(result.response.stop_reason, StopReason::Error);
    let message = result
        .response
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("context") || message.contains("maximum"),
        "unexpected error {:?}",
        result.response.error_message
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

// ---------------- Google ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gemini_2_5_flash_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("GEMINI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("google", "gemini-2.5-flash"), &key).await;
    assert_eq!(result.response.stop_reason, StopReason::Error);
    let message = result
        .response
        .error_message
        .clone()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        message.contains("input token count") && message.contains("exceeds the maximum"),
        "unexpected error {:?}",
        result.response.error_message
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

// ---------------- OpenAI Codex ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gpt_5_6_sol_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    let result = test_context_overflow(model("openai-codex", "gpt-5.6-sol"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- Amazon Bedrock ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn claude_sonnet_4_5_should_detect_overflow_via_iscontextoverflow() {
    if !common::credentials::has_bedrock_credentials() {
        return;
    }
    let result = test_context_overflow(
        model(
            "amazon-bedrock",
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        ),
        "",
    )
    .await;
    assert_plain_overflow(&result);
}

// ---------------- xAI ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn grok_4_3_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("XAI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("xai", "grok-4.3"), &key).await;
    assert_error_overflow(&result, "maximum prompt length is ");
}

// ---------------- Groq ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn llama_3_3_70b_versatile_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("GROQ_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("groq", "llama-3.3-70b-versatile"), &key).await;
    assert_error_overflow(&result, "reduce the length of the messages");
}

// ---------------- Cerebras ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn available_model_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("CEREBRAS_API_KEY") else {
        return;
    };
    let preferred = ["gpt-oss-120b", "zai-glm-4.7", "llama3.1-8b"];
    let models = get_models("cerebras");
    let candidate = models
        .iter()
        .find(|candidate| preferred.contains(&candidate.id.as_str()))
        .cloned()
        .or_else(|| models.first().cloned())
        .expect("No Cerebras models available");
    let result = test_context_overflow(candidate, &key).await;
    assert_eq!(result.response.stop_reason, StopReason::Error);
    let message = result.response.error_message.clone().unwrap_or_default();
    let lower = message.to_lowercase();
    assert!(
        (lower.contains("400") || lower.contains("413") || lower.contains("429"))
            && lower.contains("(no body)"),
        "unexpected error {message:?}"
    );
    assert!(is_context_overflow(
        &result.response,
        result.model.context_window
    ));
}

// ---------------- Hugging Face ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn kimi_k2_5_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("HF_TOKEN") else {
        return;
    };
    let result = test_context_overflow(model("huggingface", "moonshotai/Kimi-K2.5"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- Together AI ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn kimi_k3_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("TOGETHER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("together", "moonshotai/Kimi-K3"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- z.ai ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn glm_5_2_should_detect_overflow_via_iscontextoverflow_when_z_ai_reports_it() {
    let Some(key) = env("ZAI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("zai", "glm-5.2"), &key).await;
    // z.ai 行为不稳定：显式溢出错误 / 静默接受（usage.input > contextWindow）/ 限流。
    if result.response.stop_reason == StopReason::Error {
        let message = result.response.error_message.clone().unwrap_or_default();
        if message
            .to_lowercase()
            .contains("model_context_window_exceeded")
        {
            assert!(is_context_overflow(
                &result.response,
                result.model.context_window
            ));
        }
    } else if result.response.stop_reason == StopReason::Stop
        && result.has_usage_data
        && result.response.usage.input > result.model.context_window.unwrap_or(0)
    {
        assert!(is_context_overflow(
            &result.response,
            result.model.context_window
        ));
    }
}

// ---------------- Mistral ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn devstral_medium_latest_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("MISTRAL_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("mistral", "devstral-medium-latest"), &key).await;
    assert_error_overflow(&result, "too large for model with ");
}

// ---------------- MiniMax ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn minimax_m2_7_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("MINIMAX_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("minimax", "MiniMax-M2.7"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- Xiaomi（静默截断 -> length + output 0） ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mimo_v2_6_pro_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("XIAOMI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("xiaomi", "mimo-v2.6-pro"), &key).await;
    assert_length_overflow(&result);
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mimo_v2_6_pro_should_detect_overflow_via_iscontextoverflow_2() {
    let Some(key) = env("XIAOMI_TOKEN_PLAN_CN_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("xiaomi-token-plan-cn", "mimo-v2.6-pro"), &key).await;
    assert_length_overflow(&result);
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mimo_v2_6_pro_should_detect_overflow_via_iscontextoverflow_3() {
    let Some(key) = env("XIAOMI_TOKEN_PLAN_AMS_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("xiaomi-token-plan-ams", "mimo-v2.6-pro"), &key).await;
    assert_length_overflow(&result);
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mimo_v2_6_pro_should_detect_overflow_via_iscontextoverflow_4() {
    let Some(key) = env("XIAOMI_TOKEN_PLAN_SGP_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("xiaomi-token-plan-sgp", "mimo-v2.6-pro"), &key).await;
    assert_length_overflow(&result);
}

// ---------------- Qwen Token Plan ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen3_7_max_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("QWEN_TOKEN_PLAN_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("qwen-token-plan", "qwen3.7-max"), &key).await;
    assert_error_overflow(&result, "input length");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen3_8_max_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("QWEN_TOKEN_PLAN_API_KEY") else {
        return;
    };
    let result =
        test_context_overflow(model("qwen-token-plan-individual", "qwen3.8-max"), &key).await;
    assert_error_overflow(&result, "input length");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn qwen3_7_max_should_detect_overflow_via_iscontextoverflow_2() {
    let Some(key) = env("QWEN_TOKEN_PLAN_CN_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("qwen-token-plan-cn", "qwen3.7-max"), &key).await;
    assert_error_overflow(&result, "input length");
}

// ---------------- Kimi For Coding ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn kimi_for_coding_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("KIMI_API_KEY") else {
        return;
    };
    let result = test_context_overflow(model("kimi-coding", "kimi-for-coding"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- Vercel AI Gateway ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn google_gemini_2_5_flash_via_ai_gateway_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("AI_GATEWAY_API_KEY") else {
        return;
    };
    let result =
        test_context_overflow(model("vercel-ai-gateway", "google/gemini-2.5-flash"), &key).await;
    assert_plain_overflow(&result);
}

// ---------------- OpenRouter ----------------

fn openrouter_model(id: &str) -> Model {
    model("openrouter", id)
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn anthropic_claude_sonnet_4_5_via_openrouter_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENROUTER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(openrouter_model("anthropic/claude-sonnet-4.5"), &key).await;
    assert_error_overflow(&result, "maximum context length is ");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn deepseek_deepseek_v3_2_via_openrouter_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENROUTER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(openrouter_model("deepseek/deepseek-v3.2"), &key).await;
    assert_error_overflow(&result, "maximum context length is ");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn mistralai_mistral_large_2512_via_openrouter_should_detect_overflow_via_iscontextoverflow()
{
    let Some(key) = env("OPENROUTER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(openrouter_model("mistralai/mistral-large"), &key).await;
    assert_error_overflow(&result, "maximum context length is ");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn google_gemini_2_5_flash_via_openrouter_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENROUTER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(openrouter_model("google/gemini-2.5-flash"), &key).await;
    assert_error_overflow(&result, "maximum context length is ");
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn meta_llama_llama_4_scout_via_openrouter_should_detect_overflow_via_iscontextoverflow() {
    let Some(key) = env("OPENROUTER_API_KEY") else {
        return;
    };
    let result = test_context_overflow(openrouter_model("meta-llama/llama-4-scout"), &key).await;
    assert_error_overflow(&result, "maximum context length is ");
}

// ---------------- 本地 provider ----------------

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn gpt_oss_20b_should_detect_overflow_via_iscontextoverflow_ollama_silently_truncates() {
    if env("KIANA_NO_LOCAL_LLM").is_some() || !tcp_open(11434) {
        return;
    }
    let mut llm = local_model("ollama", "http://localhost:11434/v1", 128_000, 16_000);
    llm.id = "gpt-oss:20b".into();
    llm.name = "Ollama GPT-OSS 20B".into();
    llm.reasoning = Some(true);
    let result = test_context_overflow(llm, "ollama").await;
    // Ollama 静默截断：stop + usage 表示接受；error 时才断言溢出。
    if result.response.stop_reason == StopReason::Error {
        assert!(is_context_overflow(
            &result.response,
            result.model.context_window
        ));
    }
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn should_detect_overflow_via_iscontextoverflow() {
    if env("KIANA_NO_LOCAL_LLM").is_some() || !tcp_open(1234) {
        return;
    }
    let llm = local_model("lm-studio", "http://localhost:1234/v1", 8192, 2048);
    let result = test_context_overflow(llm, "lm-studio").await;
    assert_plain_overflow(&result);
}

#[tokio::test]
#[ignore = "needs live kiana-ai provider"]
async fn should_detect_overflow_via_iscontextoverflow_2() {
    if env("KIANA_NO_LOCAL_LLM").is_some() || !tcp_open(8081) {
        return;
    }
    let llm = local_model("llama.cpp", "http://localhost:8081/v1", 4096, 2048);
    let result = test_context_overflow(llm, "llama.cpp").await;
    assert_plain_overflow(&result);
}
