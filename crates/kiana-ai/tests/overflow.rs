//! `packages/ai/test/overflow.test.ts`。
//!
//! 逐条对应 TS 的 `describe("isContextOverflow")`：`it.each` 的两组错误文本、HTTP 状态匹配
//! 只对 `error` 结束生效、各 provider 的溢出文案、排除项（限流/429/Bedrock），以及 length
//! 截断（Xiaomi/MiMo 风格）与 `isRecoverableLength`。

use kiana_ai::types::{
    Api, AssistantMessage, AssistantRole, ProviderId, StopReason, Usage, UsageCost,
};
use kiana_ai::utils::overflow::{is_context_overflow, is_recoverable_length};

fn create_error_message(error_message: &str, provider: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: Api::from("openai-completions"),
        provider: ProviderId::from(provider),
        model: "qwen3.5:35b".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some(error_message.into()),
        timestamp: 0,
        ..Default::default()
    }
}

fn create_length_stop_message(options: LengthOptions) -> AssistantMessage {
    let cache_write = options.cache_write.unwrap_or(0);
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: Api::from(options.api.as_deref().unwrap_or("openai-completions")),
        provider: ProviderId::from(options.provider.as_deref().unwrap_or("test-provider")),
        model: options.model.as_deref().unwrap_or("test-model").to_string(),
        usage: Usage {
            input: options.input,
            output: options.output,
            cache_read: options.cache_read,
            cache_write,
            cache_write1h: None,
            reasoning: None,
            total_tokens: options.input + options.cache_read + cache_write + options.output,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Length,
        timestamp: 0,
        ..Default::default()
    }
}

struct LengthOptions {
    input: u64,
    cache_read: u64,
    output: u64,
    cache_write: Option<u64>,
    api: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

impl LengthOptions {
    fn new(input: u64, cache_read: u64, output: u64) -> Self {
        LengthOptions {
            input,
            cache_read,
            output,
            cache_write: None,
            api: None,
            provider: None,
            model: None,
        }
    }

    fn cache_write(mut self, cache_write: u64) -> Self {
        self.cache_write = Some(cache_write);
        self
    }

    fn api(mut self, api: &str) -> Self {
        self.api = Some(api.into());
        self
    }

    fn provider(mut self, provider: &str) -> Self {
        self.provider = Some(provider.into());
        self
    }

    fn model(mut self, model: &str) -> Self {
        self.model = Some(model.into());
        self
    }
}

// describe("isContextOverflow")

#[test]
fn detects_provider_independent_request_size_overflow() {
    // The 413 pattern is provider-agnostic, so a single Copilot-prefixed string and a single
    // non-Copilot string cover both branches of the regex; the bare forms cover the raw SDK output.
    let cases = [
        r#"github-copilot API error (413): {"message":"failed to parse request","code":""}"#,
        "custom-provider API error (413): Payload Too Large",
        r#"413 {"error":{"message":"failed to parse request","code":""}}"#,
        "413",
    ];
    for error_message in cases {
        // The request can exceed the byte limit even with zero usage and a large token window.
        assert!(
            is_context_overflow(
                &create_error_message(error_message, "ollama"),
                Some(1_000_000)
            ),
            "should match with window: {error_message}"
        );
        assert!(
            is_context_overflow(&create_error_message(error_message, "ollama"), None),
            "should match without window: {error_message}"
        );
    }
}

#[test]
fn does_not_mistake_unrelated_errors_for_request_size_overflow() {
    let cases = [
        r#"github-copilot API error (400): {"message":"failed to parse request","code":""}"#,
        "custom-provider API error (429): Too many requests",
        r#"github-copilot API error (400): {"message":"failed to parse request: invalid value 413"}"#,
        "custom-provider API error (400): upstream mentioned OpenAI API error (413): invalid input",
        "custom-provider API error (4130): invalid input",
        "4130 failed to parse request",
    ];
    for error_message in cases {
        assert!(
            !is_context_overflow(&create_error_message(error_message, "ollama"), None),
            "should not match: {error_message}"
        );
    }
}

#[test]
fn limits_http_status_matching_to_failed_responses() {
    let message = create_error_message(
        r#"OpenAI API error (413): {"message":"failed to parse request"}"#,
        "ollama",
    );
    let mut aborted = message.clone();
    aborted.stop_reason = StopReason::Aborted;
    assert!(!is_context_overflow(&aborted, None));
    let mut stopped = message;
    stopped.stop_reason = StopReason::Stop;
    assert!(!is_context_overflow(&stopped, None));
}

#[test]
fn detects_explicit_ollama_prompt_too_long_errors() {
    let message = create_error_message(
        "400 `prompt too long; exceeded max context length by 100918 tokens`",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(32768)));
}

#[test]
fn detects_z_ai_prompt_too_long_errors() {
    // Regression for #9805.
    let message = create_error_message(r#"400 {"code":"1261","message":"Prompt too long"}"#, "zai");
    assert!(is_context_overflow(&message, Some(1_048_576)));
}

#[test]
fn detects_z_ai_cn_endpoint_prompt_exceeds_max_length_errors() {
    // Regression for #10208.
    let message = create_error_message(
        r#"400 {"code":"1261","message":"Prompt exceeds max length"}"#,
        "zai",
    );
    assert!(is_context_overflow(&message, Some(1_048_576)));
}

#[test]
fn detects_together_ai_context_length_errors() {
    let message = create_error_message(
        "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(262_144)));
}

#[test]
fn detects_litellm_wrapped_openai_maximum_context_length_errors() {
    let message = create_error_message(
        "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(131_072)));
}

#[test]
fn detects_openai_compatible_parenthesized_maximum_context_length_errors() {
    let message = create_error_message(
        "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(262_144)));
}

#[test]
fn detects_openrouter_poolside_maximum_allowed_input_length_errors() {
    let message = create_error_message(
        "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(131_072)));
}

#[test]
fn detects_ds4_configured_context_size_errors() {
    let message = create_error_message(
        "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
        "ollama",
    );
    assert!(is_context_overflow(&message, Some(256_000)));

    let comma_message = create_error_message(
        "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
        "ollama",
    );
    assert!(is_context_overflow(&comma_message, Some(256_000)));
}

#[test]
fn does_not_treat_generic_non_overflow_ollama_errors_as_overflow() {
    let message = create_error_message("500 `model runner crashed unexpectedly`", "ollama");
    assert!(!is_context_overflow(&message, Some(32768)));
}

#[test]
fn only_treats_bodyless_400_errors_as_overflow_for_cerebras() {
    // Regression for #9482. HTTP 413 is a request-size error for every provider,
    // but a bodyless HTTP 400 is only known to indicate overflow for Cerebras.
    let error_message = "400 status code (no body)";
    assert!(is_context_overflow(
        &create_error_message(error_message, "cerebras"),
        Some(131_072)
    ));
    assert!(!is_context_overflow(
        &create_error_message(error_message, "opencode-go"),
        Some(1_000_000)
    ));
}

#[test]
fn does_not_treat_bedrock_throttling_too_many_tokens_as_overflow() {
    // Bedrock returns this for HTTP 429 rate limiting, NOT context overflow.
    let message = create_error_message(
        "Throttling error: Too many tokens, please wait before trying again.",
        "ollama",
    );
    assert!(!is_context_overflow(&message, Some(200_000)));
}

#[test]
fn does_not_treat_bedrock_service_unavailable_as_overflow() {
    let message = create_error_message(
        "Service unavailable: The service is temporarily unavailable.",
        "ollama",
    );
    assert!(!is_context_overflow(&message, Some(200_000)));
}

#[test]
fn does_not_treat_generic_rate_limit_errors_as_overflow() {
    let message = create_error_message(
        "Rate limit exceeded, please retry after 30 seconds.",
        "ollama",
    );
    assert!(!is_context_overflow(&message, Some(200_000)));
}

#[test]
fn does_not_treat_http_429_style_errors_as_overflow() {
    let message = create_error_message("Too many requests. Please slow down.", "ollama");
    assert!(!is_context_overflow(&message, Some(200_000)));
}

#[test]
fn detects_xiaomi_style_overflow_length_stop_with_zero_output_and_filled_context() {
    let message = create_length_stop_message(
        LengthOptions::new(58, 1_048_512, 0)
            .provider("xiaomi")
            .model("mimo-v2.6-pro"),
    );
    assert!(is_context_overflow(&message, Some(1_048_576)));
}

#[test]
fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
    let message = create_length_stop_message(
        LengthOptions::new(3, 253_584, 16)
            .cache_write(25_554)
            .api("openai-responses")
            .provider("openai")
            .model("gpt-5.6-sol"),
    );
    assert!(is_recoverable_length(&message, 128_000));
}

#[test]
fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
    let message = create_length_stop_message(LengthOptions::new(4_062, 0, 1_024));
    assert!(!is_recoverable_length(&message, 1_024));
}

#[test]
fn treats_zero_output_length_stops_as_recoverable_without_context_metadata() {
    let message = create_length_stop_message(LengthOptions::new(100, 0, 0));
    assert!(is_recoverable_length(&message, 128_000));
}

#[test]
fn does_not_treat_normal_length_stops_with_output_as_context_overflow() {
    let message = create_length_stop_message(LengthOptions::new(1_000, 0, 4_096));
    assert!(!is_context_overflow(&message, Some(200_000)));
}

#[test]
fn does_not_treat_zero_output_length_stops_far_below_context_as_context_overflow() {
    let message = create_length_stop_message(LengthOptions::new(100, 0, 0));
    assert!(!is_context_overflow(&message, Some(200_000)));
}
