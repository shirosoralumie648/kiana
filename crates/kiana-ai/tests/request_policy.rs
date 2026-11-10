//! Provider 请求策略的公开 API 测试（第 9 周 DoD 的端到端形状）。
//!
//! 这些用例只通过 `kiana_ai::request_policy` 的公开面断言行为：
//! 能力门在发网络前失败、预检零请求、预算不变量、429 不误判上下文、可重试集合、
//! usage 优先级与 cache 注入。

use kiana_ai::request_policy::{
    CacheSemantics, CapabilitySet, CostSource, OutputBudgetInput, ProviderErrorClass,
    RequestPolicyError, RequestPolicyInput, RequestRequirements, TypedProviderError,
    account_provider_usage, apply_cache_breakpoints, classify_provider_error,
    compute_output_budget, is_retryable_provider_error, prepare_request,
};
use kiana_ai::types::{
    Message, Model, ModelCost, TranscriptContext, Usage, UserContent, UserMessage, UserRole,
};
use serde_json::json;

fn context_with(chars: usize) -> TranscriptContext {
    let mut context = TranscriptContext::default();
    context.0.messages.push(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text("x".repeat(chars)),
        timestamp: 0,
    }));
    context
}

fn model() -> Model {
    Model {
        id: "text-model".into(),
        provider: "anthropic".into(),
        context_window: Some(200_000),
        max_tokens: Some(8_192),
        cost: ModelCost {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
            tiers: None,
        },
        ..Model::default()
    }
}

#[test]
fn a_text_only_provider_rejects_image_generation_before_the_network() {
    let model = model();
    let context = context_with(100);
    let manifest = CapabilitySet::language_chat();
    let adapter = CapabilitySet::language_chat();
    let requirements = RequestRequirements::image_generation();
    let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
    let mut dispatch_count = 0;
    let result = prepare_request(&input).map(|_| dispatch_count += 1);
    let error = result.expect_err("unsupported");
    assert!(matches!(error, RequestPolicyError::Unsupported(_)));
    assert_eq!(dispatch_count, 0);
    let message = error.to_string();
    assert!(message.contains("image-generation"));
    assert!(message.contains("manifest"));
    assert!(message.contains("adapter"));
}

#[test]
fn an_oversized_prompt_is_rejected_with_zero_requests() {
    let model = model();
    let context = context_with(700_000);
    let manifest = CapabilitySet::language_chat();
    let adapter = CapabilitySet::language_chat();
    let requirements = RequestRequirements::chat_with_tools();
    let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
    let mut dispatch_count = 0;
    let result = prepare_request(&input).map(|_| dispatch_count += 1);
    assert!(matches!(
        result.expect_err("context window"),
        RequestPolicyError::ContextWindow(_)
    ));
    assert_eq!(dispatch_count, 0);
}

#[test]
fn budget_never_exceeds_available_space_and_keeps_the_floor() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };
    for _ in 0..5_000 {
        let context_window = next() % 65_536 + 1;
        let estimated_prompt = next() % 45_000;
        let model_max = next() % 300_000 + 1;
        let caller = match next() % 3 {
            0 => None,
            1 => Some(next() % 100_000),
            _ => Some(0),
        };
        let input = OutputBudgetInput {
            caller_max_tokens: caller,
            model_max_output: Some(model_max),
            context_window: Some(context_window),
            estimated_prompt_tokens: estimated_prompt,
            ..OutputBudgetInput::default()
        };
        match compute_output_budget(&input) {
            Ok(budget) => {
                let available = budget.available.expect("known window");
                assert!(budget.max_tokens <= available);
                if caller != Some(0) {
                    assert!(budget.max_tokens >= budget.floor);
                } else {
                    assert_eq!(budget.max_tokens, 0);
                }
            }
            Err(_) => {
                assert!(
                    context_window as i64 - estimated_prompt as i64 - 1_024 < 1,
                    "overflow only when available < 1"
                );
            }
        }
    }
}

#[test]
fn classification_and_retryability_follow_the_contract() {
    // 429 + "context length" → RateLimit，不触发压缩。
    let rate_limited = json!({
        "status": 429,
        "message": "rate limit exceeded: context length too large"
    });
    assert_eq!(
        classify_provider_error(&rate_limited),
        ProviderErrorClass::RateLimit
    );
    assert!(is_retryable_provider_error(None, &rate_limited));

    // 401 + invalid_api_key → Auth，不重试。
    let auth = json!({"status": 401, "code": "invalid_api_key", "message": "bad key"});
    assert_eq!(classify_provider_error(&auth), ProviderErrorClass::Auth);
    assert!(!is_retryable_provider_error(None, &auth));

    // 400 + malformed tool schema → InvalidRequest。
    let invalid = json!({"status": 400, "message": "malformed tool schema"});
    assert_eq!(
        classify_provider_error(&invalid),
        ProviderErrorClass::InvalidRequest
    );
    assert!(!is_retryable_provider_error(None, &invalid));

    // 529/503 可重试；typed isRetryable=false 覆盖状态码集合。
    assert!(is_retryable_provider_error(None, &json!({"status": 529})));
    assert!(is_retryable_provider_error(None, &json!({"status": 503})));
    let typed = TypedProviderError {
        status: Some(503),
        is_retryable: Some(false),
        ..TypedProviderError::default()
    };
    assert!(!is_retryable_provider_error(Some(&typed), &json!({})));
}

#[test]
fn usage_priority_is_canonical_then_user_declared_then_provider_reported() {
    let model = model();
    let provider = Usage {
        input: 1_000,
        output: 100,
        total_tokens: 1_100,
        ..Usage::default()
    };
    let canonical = account_provider_usage(&model, &provider, None);
    assert_eq!(canonical.cost_source, CostSource::Canonical);

    let unpriced = Model {
        cost: ModelCost::default(),
        ..model.clone()
    };
    let declared = Usage {
        cost: kiana_ai::types::UsageCost {
            total: 1.5,
            ..Default::default()
        },
        ..provider.clone()
    };
    let accounted = account_provider_usage(&unpriced, &provider, Some(&declared));
    assert_eq!(accounted.cost_source, CostSource::UserDeclared);
    let accounted = account_provider_usage(&unpriced, &provider, None);
    assert_eq!(accounted.cost_source, CostSource::ProviderReported);
}

#[test]
fn explicit_breakpoints_inject_and_implicit_tolerant_does_not() {
    let payload = json!({
        "messages": [{"role": "system", "content": "be careful"}],
        "tools": [{"type": "function", "function": {"name": "bash"}}]
    });
    let mut explicit = payload.clone();
    let injected = apply_cache_breakpoints(
        &mut explicit,
        CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 },
    );
    assert!(injected > 0);
    assert!(explicit.to_string().contains("cache_control"));

    let mut tolerant = payload.clone();
    assert_eq!(
        apply_cache_breakpoints(&mut tolerant, CacheSemantics::ImplicitTolerant),
        0
    );
    assert_eq!(tolerant, payload);
}
