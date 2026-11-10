//! `cross-provider-handoff.test.ts` 的 Rust 移植（真实 API，`#[ignore]`）。
//!
//! 验证「一个 provider 生成的上下文能被另一个 provider 消费」：先用每个可用的
//! provider/model 生成一段固定上下文（用户消息 → assistant 的 thinking + toolCall →
//! toolResult → 最终 assistant），再把**其它所有**上下文拼接起来发给每个目标模型，要求
//! 请求成功（`stopReason !== "error"`）。
//!
//! TS 的 `beforeAll` 生成 fixtures，两个 `it` 分别断言：`should have at least 2
//! fixtures` 与逐目标的 handoff。Rust 侧保留两个独立的 `#[ignore]` 测试，fixtures 用
//! `tokio::sync::OnceCell` 共享（只生成一次）；`contexts` 用 [`IndexMap`] 复刻 TS 普通
//! 对象的插入序（含重复 label 覆盖值但保留首次位置），保证拼给目标模型的消息顺序稳定。
//! 失败时与 TS 一样把 payload 写到 `/tmp/kiana-handoff-*.json`。
//!
//! 环境变量/凭据缺失的 pair 与 `skipIf` 一样跳过。

mod common;

use indexmap::IndexMap;

use kiana_ai::compat::{complete_simple, get_env_api_key, get_model};
use kiana_ai::types::*;
use serde_json::json;

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[derive(Clone)]
struct ProviderModelPair {
    provider: &'static str,
    model: &'static str,
    label: &'static str,
    api_override: Option<&'static str>,
    upstream_api_key_env: Option<&'static str>,
}

const PROVIDER_MODEL_PAIRS: &[ProviderModelPair] = &[
    ProviderModelPair {
        provider: "anthropic",
        model: "claude-sonnet-4-5",
        label: "anthropic-claude-sonnet-4-5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "google",
        model: "gemini-3-flash-preview",
        label: "google-gemini-3-flash-preview",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "openai",
        model: "gpt-4o-mini",
        label: "openai-completions-gpt-4o-mini",
        api_override: Some("openai-completions"),
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "openai",
        model: "gpt-5-mini",
        label: "openai-responses-gpt-5-mini",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "azure",
        model: "gpt-4o-mini",
        label: "azure-gpt-4o-mini",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "openai-codex",
        model: "gpt-6.1-sol",
        label: "openai-codex-gpt-6.1-sol",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "github-copilot",
        model: "claude-sonnet-4.5",
        label: "copilot-claude-sonnet-4.5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "github-copilot",
        model: "gpt-5.1-codex",
        label: "copilot-gpt-5.1-codex",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "github-copilot",
        model: "gemini-3-flash-preview",
        label: "copilot-gemini-3-flash-preview",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "github-copilot",
        model: "grok-code-fast-1",
        label: "copilot-grok-code-fast-1",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "amazon-bedrock",
        model: "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
        label: "bedrock-claude-sonnet-4-5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "xai",
        model: "grok-4.3",
        label: "xai-grok-4.3",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "cerebras",
        model: "zai-glm-4.7",
        label: "cerebras-zai-glm-4.7",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "cloudflare-workers-ai",
        model: "@cf/moonshotai/kimi-k2.6",
        label: "cloudflare-kimi-k2.6",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "cloudflare-ai-gateway",
        model: "workers-ai/@cf/moonshotai/kimi-k2.6",
        label: "cloudflare-gateway-kimi-k2.6",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "cloudflare-ai-gateway",
        model: "claude-sonnet-4-5",
        label: "cloudflare-gateway-claude-sonnet-4-5",
        api_override: None,
        upstream_api_key_env: Some("ANTHROPIC_API_KEY"),
    },
    ProviderModelPair {
        provider: "cloudflare-ai-gateway",
        model: "gpt-5.1",
        label: "cloudflare-gateway-gpt-5.1",
        api_override: None,
        upstream_api_key_env: Some("OPENAI_API_KEY"),
    },
    ProviderModelPair {
        provider: "groq",
        model: "openai/gpt-oss-120b",
        label: "groq-gpt-oss-120b",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "huggingface",
        model: "moonshotai/Kimi-K2.5",
        label: "huggingface-kimi-k2.5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "together",
        model: "moonshotai/Kimi-K3",
        label: "together-kimi-k3",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "baseten",
        model: "zai-org/GLM-5.2",
        label: "baseten-glm-5.2",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "kimi-coding",
        model: "kimi-for-coding",
        label: "kimi-for-coding",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "meta",
        model: "muse-spark-1.3",
        label: "meta-muse-spark-1.3",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "mistral",
        model: "devstral-medium-latest",
        label: "mistral-devstral-medium",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "minimax",
        model: "MiniMax-M2.7",
        label: "minimax-m2.7",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "minimax-cn",
        model: "MiniMax-M2.7",
        label: "minimax-m2.7",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "big-pickle",
        label: "zen-big-pickle",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "claude-sonnet-4-5",
        label: "zen-claude-sonnet-4-5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "gemini-3-flash",
        label: "zen-gemini-3-flash",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "glm-4.7-free",
        label: "zen-glm-4.7-free",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "gpt-5.2-codex",
        label: "zen-gpt-5.2-codex",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode",
        model: "minimax-m2.1-free",
        label: "zen-minimax-m2.1-free",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode-go",
        model: "kimi-k2.5",
        label: "go-kimi-k2.5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "opencode-go",
        model: "minimax-m2.5",
        label: "go-minimax-m2.5",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "xiaomi",
        model: "mimo-v2.6-pro",
        label: "xiaomi-mimo-v2.6-pro",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "xiaomi-token-plan-cn",
        model: "mimo-v2.6-pro",
        label: "xiaomi-token-plan-cn-mimo-v2.6-pro",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "xiaomi-token-plan-ams",
        model: "mimo-v2.6-pro",
        label: "xiaomi-token-plan-ams-mimo-v2.6-pro",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "xiaomi-token-plan-sgp",
        model: "mimo-v2.6-pro",
        label: "xiaomi-token-plan-sgp-mimo-v2.6-pro",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "qwen-token-plan",
        model: "qwen3.7-max",
        label: "qwen-token-plan-qwen3.7-max",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "qwen-token-plan-cn",
        model: "qwen3.7-max",
        label: "qwen-token-plan-cn-qwen3.7-max",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "qwen-token-plan-individual",
        model: "qwen3.8-max",
        label: "qwen-token-plan-individual-qwen3.8-max",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "qwen-token-plan-individual",
        model: "deepseek-v4-flash-0731",
        label: "qwen-token-plan-individual-deepseek-v4-flash-0731",
        api_override: None,
        upstream_api_key_env: None,
    },
    ProviderModelPair {
        provider: "qwen-token-plan-individual",
        model: "glm-5.2",
        label: "qwen-token-plan-individual-glm-5.2",
        api_override: None,
        upstream_api_key_env: None,
    },
];

fn test_tool() -> Tool {
    Tool {
        name: "double_number".into(),
        description: "Doubles a number and returns the result".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "number", "description": "A number to double" } },
            "required": ["value"],
        }),
        constrained_sampling: None,
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: common::event_stream::now_ms(),
    })
}

/// `getApiKey`：OAuth 存储优先，其次环境变量。
fn get_api_key(provider: &str) -> Option<String> {
    common::oauth::resolve_api_key(provider).or_else(|| get_env_api_key(provider, None))
}

/// `hasApiKey`（同步、只看环境变量/凭据探测）。
fn has_api_key(pair: &ProviderModelPair) -> bool {
    if pair.provider == "azure" {
        return common::credentials::has_azure_openai_credentials();
    }
    if pair.provider == "cloudflare-workers-ai" {
        return common::credentials::has_cloudflare_workers_ai_credentials();
    }
    if pair.provider == "cloudflare-ai-gateway" {
        if !common::credentials::has_cloudflare_ai_gateway_credentials() {
            return false;
        }
        return pair.upstream_api_key_env.is_none_or(env_set);
    }
    get_env_api_key(pair.provider, None).is_some()
}

/// `getHeaders`。
fn get_headers(pair: &ProviderModelPair) -> Option<ProviderHeaders> {
    let name = pair.upstream_api_key_env?;
    let value = env(name)?;
    let mut headers = ProviderHeaders::new();
    headers.insert("Authorization".into(), Some(format!("Bearer {value}")));
    Some(headers)
}

/// `dumpFailurePayload`。
fn dump_failure_payload(
    label: &str,
    error: &str,
    payload: Option<&serde_json::Value>,
    messages: &[Message],
) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let filename = format!("/tmp/kiana-handoff-{label}-{timestamp}.json");
    let body = json!({
        "label": label,
        "error": error,
        "payload": payload,
        "messages": messages,
    });
    if let Ok(text) = serde_json::to_string_pretty(&body) {
        let _ = std::fs::write(&filename, text);
    }
    eprintln!("Wrote failure payload to {filename}");
}

/// `generateContext` 的结果。
struct GeneratedContext {
    messages: Vec<Message>,
}

/// `generateContext`。
async fn generate_context(pair: &ProviderModelPair, api_key: &str) -> Option<GeneratedContext> {
    let mut model = match get_model(pair.provider, pair.model) {
        Some(model) => model,
        None => {
            eprintln!("  Model not found: {}/{}", pair.provider, pair.model);
            return None;
        }
    };
    if let Some(api_override) = pair.api_override {
        model.api = api_override.into();
    }

    let user_message = user("Please double the number 21 using the double_number tool.");
    let supports_reasoning = model.reasoning == Some(true);
    let headers = get_headers(pair);
    let payload: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let payload_for_callback = std::sync::Arc::clone(&payload);

    let mut options = SimpleStreamOptions {
        base: StreamOptions::default(),
        ..SimpleStreamOptions::default()
    };
    options.base.api_key = Some(api_key.to_string());
    options.base.headers = headers;
    options.reasoning = supports_reasoning.then_some(ThinkingLevel::High);
    options.base.on_payload = Some(std::sync::Arc::new(
        move |value: serde_json::Value, _model: &Model| {
            *payload_for_callback
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(value);
            None
        },
    ));

    let context = Context {
        system_prompt: Some(
            "You are a helpful assistant. Use the provided tool to complete the task.".into(),
        ),
        messages: vec![user_message.clone()],
        tools: Some(vec![test_tool()]),
    };
    let assistant_response = complete_simple(&model, &context, options).await;
    if assistant_response.stop_reason == StopReason::Error {
        let message = assistant_response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".into());
        eprintln!("  Initial request error: {message}");
        dump_failure_payload(
            &format!("{}-initial", pair.label),
            &message,
            payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref(),
            std::slice::from_ref(&user_message),
        );
        return None;
    }

    let tool_call = assistant_response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        });
    let Some(tool_call) = tool_call else {
        eprintln!(
            "  No tool call in response (stopReason: {:?})",
            assistant_response.stop_reason
        );
        return Some(GeneratedContext {
            messages: vec![user_message, Message::Assistant(assistant_response.clone())],
        });
    };
    eprintln!("  Tool call ID: {}", tool_call.id);

    let tool_result = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "42".into(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: common::event_stream::now_ms(),
        duration_ms: None,
    };

    let messages_for_final = vec![
        user_message,
        Message::Assistant(assistant_response),
        Message::ToolResult(tool_result),
    ];
    let mut final_options = SimpleStreamOptions {
        base: StreamOptions::default(),
        ..SimpleStreamOptions::default()
    };
    final_options.base.api_key = Some(api_key.to_string());
    final_options.base.headers = match pair.upstream_api_key_env.and_then(env) {
        Some(value) => {
            let mut headers = ProviderHeaders::new();
            headers.insert("Authorization".into(), Some(format!("Bearer {value}")));
            Some(headers)
        }
        None => None,
    };
    final_options.reasoning = supports_reasoning.then_some(ThinkingLevel::High);
    let payload_for_final = std::sync::Arc::clone(&payload);
    final_options.base.on_payload = Some(std::sync::Arc::new(
        move |value: serde_json::Value, _model: &Model| {
            *payload_for_final
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(value);
            None
        },
    ));
    let final_context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: messages_for_final.clone(),
        tools: Some(vec![test_tool()]),
    };
    let final_response = complete_simple(&model, &final_context, final_options).await;
    if final_response.stop_reason == StopReason::Error {
        let message = final_response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".into());
        eprintln!("  Final request error: {message}");
        dump_failure_payload(
            &format!("{}-final", pair.label),
            &message,
            payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref(),
            &messages_for_final,
        );
        return None;
    }

    let mut messages = messages_for_final;
    messages.push(Message::Assistant(final_response));
    Some(GeneratedContext { messages })
}

fn has_any_api_key() -> bool {
    PROVIDER_MODEL_PAIRS.iter().any(has_api_key)
}

/// `beforeAll` 生成的结果。
///
/// TS 用普通对象 `contexts: Record<string, CachedContext>` 保存：按插入序迭代，重复
/// label（`minimax` 与 `minimax-cn` 都是 `minimax-m2.7`）覆盖值但保留首次位置。因此这里
/// 用 [`IndexMap`] —— `HashMap` 的随机顺序会让拼接给目标模型的消息顺序每次运行都不同。
struct Fixtures {
    contexts: IndexMap<&'static str, GeneratedContext>,
    available_pairs: Vec<&'static ProviderModelPair>,
}

/// `describe` 的 `beforeAll` 只跑一次；两个 `it` 共享同一份 fixtures（`OnceCell` 让两个
/// `#[ignore]` 测试各自启动时也只生成一次）。
static FIXTURES: tokio::sync::OnceCell<Fixtures> = tokio::sync::OnceCell::const_new();

async fn fixtures() -> &'static Fixtures {
    FIXTURES.get_or_init(generate_fixtures).await
}

/// TS 的 `beforeAll`：为每个有凭据的 pair 生成固定上下文。
async fn generate_fixtures() -> Fixtures {
    // === Generating Fixtures ===
    let mut contexts: IndexMap<&'static str, GeneratedContext> = IndexMap::new();
    let mut available_pairs: Vec<&ProviderModelPair> = Vec::new();
    for pair in PROVIDER_MODEL_PAIRS {
        let Some(api_key) = get_api_key(pair.provider) else {
            eprintln!("[{}] Skipping - no auth for {}", pair.label, pair.provider);
            continue;
        };
        if !has_api_key(pair) {
            eprintln!("[{}] Skipping - no auth for {}", pair.label, pair.provider);
            continue;
        }
        eprintln!("[{}] Generating fixture...", pair.label);
        let Some(result) = generate_context(pair, &api_key).await else {
            eprintln!("[{}] Failed to generate fixture, skipping", pair.label);
            continue;
        };
        if result.messages.len() < 4 {
            eprintln!("[{}] Failed to generate fixture, skipping", pair.label);
            continue;
        }
        eprintln!(
            "[{}] Generated {} messages",
            pair.label,
            result.messages.len()
        );
        contexts.insert(pair.label, result);
        available_pairs.push(pair);
    }

    eprintln!(
        "=== {}/{} contexts available ===",
        available_pairs.len(),
        PROVIDER_MODEL_PAIRS.len()
    );
    Fixtures {
        contexts,
        available_pairs,
    }
}

/// TS `it("should have at least 2 fixtures to test handoffs")`。
#[tokio::test]
#[ignore = "live cross-provider APIs (requires provider credentials)"]
async fn should_have_at_least_2_fixtures_to_test_handoffs() {
    if !has_any_api_key() {
        return;
    }
    let fixtures = fixtures().await;
    assert!(
        fixtures.contexts.len() >= 2,
        "should have at least 2 fixtures"
    );
}

/// TS `it("should handle cross-provider handoffs for each target")`。
#[tokio::test]
#[ignore = "live cross-provider APIs (requires provider credentials)"]
async fn should_handle_cross_provider_handoffs_for_each_target() {
    if !has_any_api_key() {
        return;
    }
    let fixtures = fixtures().await;
    let contexts = &fixtures.contexts;
    let available_pairs = &fixtures.available_pairs;

    // TS: `if (contextLabels.length < 2) { console.log(...); return; }`
    if contexts.len() < 2 {
        eprintln!("Not enough fixtures for handoff test, skipping");
        return;
    }

    // === Testing Cross-Provider Handoffs ===
    let mut failures: Vec<(String, Option<String>)> = Vec::new();
    for target_pair in available_pairs {
        let Some(api_key) = get_api_key(target_pair.provider) else {
            continue;
        };
        if !has_api_key(target_pair) {
            continue;
        }

        let mut other_messages: Vec<Message> = Vec::new();
        // `Object.entries(contexts)`：插入序（IndexMap 与 JS 对象一致，含重复 label 的
        // 首次位置语义）。
        for (label, context) in contexts {
            if *label == target_pair.label {
                continue;
            }
            other_messages.extend(context.messages.iter().cloned());
        }
        if other_messages.is_empty() {
            continue;
        }

        let mut all_messages = other_messages;
        all_messages.push(user(
            "Great, thanks for all that help! Now just say 'Hello, handoff successful!' to confirm you received everything.",
        ));

        let Some(mut model) = get_model(target_pair.provider, target_pair.model) else {
            continue;
        };
        if let Some(api_override) = target_pair.api_override {
            model.api = api_override.into();
        }
        let supports_reasoning = model.reasoning == Some(true);
        let headers = get_headers(target_pair);
        let payload: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let payload_for_callback = std::sync::Arc::clone(&payload);

        let mut options = SimpleStreamOptions {
            base: StreamOptions::default(),
            ..SimpleStreamOptions::default()
        };
        options.base.api_key = Some(api_key);
        options.base.headers = headers;
        options.reasoning = supports_reasoning.then_some(ThinkingLevel::High);
        options.base.on_payload = Some(std::sync::Arc::new(
            move |value: serde_json::Value, _model: &Model| {
                *payload_for_callback
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(value);
                None
            },
        ));

        let context = Context {
            system_prompt: Some("You are a helpful assistant.".into()),
            messages: all_messages.clone(),
            tools: Some(vec![test_tool()]),
        };
        let response = complete_simple(&model, &context, options).await;
        if response.stop_reason == StopReason::Error {
            let message = response.error_message.clone();
            dump_failure_payload(
                target_pair.label,
                message.as_deref().unwrap_or("Unknown error"),
                payload
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref(),
                &all_messages,
            );
            failures.push((target_pair.label.to_string(), message));
        }
    }

    assert!(failures.is_empty(), "handoff failures: {failures:?}");
}
