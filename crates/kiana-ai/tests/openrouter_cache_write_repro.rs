//! `openrouter-cache-write-repro.test.ts`（真实 OpenRouter API）。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::types::{Context, StreamOptions};
use serde_json::{Value, json};

fn long_system_prompt() -> String {
    let nonce = format!(
        "{}-{}",
        kiana_ai::model_catalog::now_epoch_millis(),
        std::process::id()
    );
    let body = (0..80)
        .map(|_| "Prompt-caching probe content. Keep this exact text stable across requests so the provider can reuse prefix tokens and report cache read and cache write usage.")
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("You are a concise assistant.\nCache nonce: {nonce}\n\n{body}")
}

fn add_cache_control(payload: Value) -> Option<Value> {
    let mut payload = payload;
    let messages = payload.get_mut("messages")?.as_array_mut()?;
    for message in messages.iter_mut().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            let content = content.to_string();
            message["content"] = json!([{ "type": "text", "text": content, "cache_control": { "type": "ephemeral" } }]);
            break;
        }
        if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part["cache_control"] = json!({ "type": "ephemeral" });
                    break;
                }
            }
            break;
        }
    }
    Some(payload)
}

fn options(api_key: String) -> OpenAICompletionsOptions {
    OpenAICompletionsOptions {
        base: StreamOptions {
            api_key: Some(api_key),
            max_tokens: Some(32),
            temperature: Some(0.0),
            on_payload: Some(std::sync::Arc::new(|payload, _model| {
                add_cache_control(payload)
            })),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "needs live OpenRouter API (OPENROUTER_API_KEY)"]
async fn preserves_cache_write_tokens_on_the_openai_completions_stream_path() {
    let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") else {
        return;
    };
    if api_key.is_empty() {
        return;
    }
    let model = mock_http::catalog_model("openrouter", "google/gemini-2.5-flash");
    let context = Context {
        system_prompt: Some(long_system_prompt()),
        messages: vec![mock_http::user("Reply with exactly: OK")],
        tools: None,
    };
    let first = mock_http::run_completions(
        &model,
        &mock_http::context_with(&context),
        options(api_key.clone()),
    )
    .await;
    let second =
        mock_http::run_completions(&model, &mock_http::context_with(&context), options(api_key))
            .await;
    assert_eq!(
        first.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        first.error_message
    );
    assert_eq!(
        second.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        second.error_message
    );
    assert!(first.usage.cache_write > 0 || second.usage.cache_write > 0);
}
