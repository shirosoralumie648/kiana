//! 请求策略的组合入口：能力门 → 输入预检 → 输出预算 → cache 语义。
//!
//! 顺序是刻意的：能力门在最前（能力不满足时连估算都不做，更不会发网络），输入预检第二
//! （超窗直接 typed error，零请求），输出预算第三（预算只在请求确实要发时才需要），最后
//! 给出 cache 语义供适配器决定是否注入断点/安排 warmer。
//!
//! 这是纯函数层：不读环境、不发 IO、没有全局状态。调用方（agent loop / session / provider
//! 适配器）在真正构造 HTTP 请求前调用 [`prepare_request`]，把 [`RequestPlan::max_tokens`]
//! 写进请求参数，并按 [`RequestPlan::cache`] 决定断点。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiErrorKind};
use crate::request_policy::budget::{
    OUTPUT_RESERVE_TOKENS, OutputBudgetError, PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS,
    compute_output_budget,
};
use crate::request_policy::cache::{CacheSemantics, cache_warmer_eligible};
use crate::request_policy::capabilities::{
    CapabilityGate, CapabilitySet, RequestRequirements, UnsupportedCapability,
};
use crate::request_policy::precheck::{ContextWindowExceeded, InputPrecheck, precheck_input};
use crate::types::{Model, TranscriptContext};

/// 策略输入。
#[derive(Debug, Clone, Copy)]
pub struct RequestPolicyInput<'a> {
    /// 目标模型。
    pub model: &'a Model,
    /// 已归一化上下文。
    pub context: &'a TranscriptContext,
    /// provider manifest 声明的能力。
    pub manifest: &'a CapabilitySet,
    /// 已选 adapter 声明的能力。
    pub adapter: &'a CapabilitySet,
    /// 请求自身的能力需求。
    pub requirements: &'a RequestRequirements,
    /// 调用方显式输出上限。
    pub caller_max_tokens: Option<u64>,
    /// 上下文预留 token。
    pub reserve_tokens: u64,
    /// 产品默认输出上限。
    pub product_default_max_output: u64,
}

impl<'a> RequestPolicyInput<'a> {
    /// 用默认预留/产品默认构造。
    pub fn new(
        model: &'a Model,
        context: &'a TranscriptContext,
        manifest: &'a CapabilitySet,
        adapter: &'a CapabilitySet,
        requirements: &'a RequestRequirements,
    ) -> Self {
        RequestPolicyInput {
            model,
            context,
            manifest,
            adapter,
            requirements,
            caller_max_tokens: None,
            reserve_tokens: OUTPUT_RESERVE_TOKENS,
            product_default_max_output: PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }
}

/// 通过策略后的请求参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPlan {
    /// 最终输出上限。
    pub max_tokens: u64,
    /// 输入预检结果。
    pub precheck: InputPrecheck,
    /// 该模型的 prompt cache 语义。
    pub cache: CacheSemantics,
    /// 是否有 cache-warmer 资格。
    pub cache_warmer_eligible: bool,
}

/// 策略错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestPolicyError {
    /// 能力门拒绝（manifest 或 adapter 缺能力）。
    Unsupported(UnsupportedCapability),
    /// 输入放不进上下文窗口。
    ContextWindow(ContextWindowExceeded),
    /// 输出预算不足。
    Budget(OutputBudgetError),
}

impl fmt::Display for RequestPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestPolicyError::Unsupported(error) => error.fmt(formatter),
            RequestPolicyError::ContextWindow(error) => error.fmt(formatter),
            RequestPolicyError::Budget(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RequestPolicyError {}

impl From<RequestPolicyError> for AiError {
    fn from(error: RequestPolicyError) -> Self {
        match error {
            RequestPolicyError::Unsupported(error) => error.into(),
            RequestPolicyError::ContextWindow(error) => error.into(),
            RequestPolicyError::Budget(error) => error.into(),
        }
    }
}

/// 执行完整请求策略。
///
/// 失败时保证没有发生任何网络调用（本函数不持有任何 IO 能力，失败路径自然为零请求）。
pub fn prepare_request(input: &RequestPolicyInput<'_>) -> Result<RequestPlan, RequestPolicyError> {
    let gate = CapabilityGate::new(
        input.model.id.clone(),
        input.manifest.clone(),
        input.adapter.clone(),
    );
    gate.authorize(input.requirements)
        .map_err(RequestPolicyError::Unsupported)?;

    let precheck = precheck_input(input.context, input.model, input.reserve_tokens)
        .map_err(RequestPolicyError::ContextWindow)?;

    let budget = compute_output_budget(&crate::request_policy::budget::OutputBudgetInput {
        caller_max_tokens: input.caller_max_tokens,
        model_max_output: input.model.max_tokens,
        context_window: input.model.context_window,
        estimated_prompt_tokens: precheck.estimated_input_tokens,
        product_default_max_output: input.product_default_max_output,
        reserve_tokens: input.reserve_tokens,
    })
    .map_err(RequestPolicyError::Budget)?;

    let cache = CacheSemantics::for_model(&input.model.provider, &input.model.id);
    Ok(RequestPlan {
        max_tokens: budget.max_tokens,
        precheck,
        cache,
        cache_warmer_eligible: cache_warmer_eligible(input.model, cache),
    })
}

/// 把策略错误映射成 `AiError`（便捷函数，避免调用方写 `From`）。
pub fn policy_ai_error(error: RequestPolicyError) -> AiError {
    error.into()
}

/// 便捷断言：错误是否为 `Unsupported`（能力门）。
pub fn is_unsupported(error: &RequestPolicyError) -> bool {
    matches!(error, RequestPolicyError::Unsupported(_))
}

/// 便捷断言：错误是否为 `ContextWindow`。
pub fn is_context_window(error: &RequestPolicyError) -> bool {
    matches!(error, RequestPolicyError::ContextWindow(_))
}

/// `AiErrorKind` 对策略错误的稳定映射（供诊断/测试）。
pub fn error_kind(error: &RequestPolicyError) -> AiErrorKind {
    match error {
        RequestPolicyError::Unsupported(_) => AiErrorKind::Unsupported,
        RequestPolicyError::ContextWindow(_) => AiErrorKind::Request,
        RequestPolicyError::Budget(_) => AiErrorKind::Request,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_policy::capabilities::ModelOperation;
    use crate::types::{Message, TranscriptContext, UserContent, UserMessage, UserRole};

    fn text_model() -> Model {
        Model {
            id: "text-model".into(),
            provider: "anthropic".into(),
            context_window: Some(200_000),
            max_tokens: Some(8_192),
            ..Model::default()
        }
    }

    fn context_with_chars(chars: usize) -> TranscriptContext {
        let mut context = TranscriptContext::default();
        context.0.messages.push(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("x".repeat(chars)),
            timestamp: 0,
        }));
        context
    }

    #[test]
    fn image_request_on_a_text_model_is_rejected_with_zero_dispatch() {
        let model = text_model();
        let context = context_with_chars(100);
        let manifest = CapabilitySet::language_chat();
        let adapter = CapabilitySet::language_chat();
        let requirements = RequestRequirements::image_generation();
        let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);

        let mut dispatched = 0;
        let result = prepare_request(&input).map(|_| {
            dispatched += 1;
        });
        let error = result.expect_err("unsupported");
        assert!(matches!(error, RequestPolicyError::Unsupported(_)));
        assert!(error.to_string().contains("image-generation"));
        assert_eq!(dispatched, 0);
        assert_eq!(error_kind(&error), AiErrorKind::Unsupported);
    }

    #[test]
    fn oversized_input_is_rejected_before_dispatch() {
        let model = text_model();
        let context = context_with_chars(700_000); // ~200k tokens
        let manifest = CapabilitySet::language_chat();
        let adapter = CapabilitySet::language_chat();
        let requirements = RequestRequirements::default();
        let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
        let error = prepare_request(&input).expect_err("context window");
        assert!(matches!(error, RequestPolicyError::ContextWindow(_)));
        assert_eq!(error_kind(&error), AiErrorKind::Request);
    }

    #[test]
    fn plan_combines_budget_and_cache_semantics() {
        let model = text_model();
        let context = context_with_chars(7_000); // ~2000 tokens
        let manifest = CapabilitySet::language_chat().with_vision(true);
        let adapter = CapabilitySet::language_chat().with_vision(true);
        let requirements = RequestRequirements::chat_with_tools();
        let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
        let plan = prepare_request(&input).expect("plan");
        assert_eq!(plan.precheck.estimated_input_tokens, 2_000);
        // 8192 模型上限 < 32000 产品默认；floor = 2457（30% of 8192），模型 cap 胜出。
        assert_eq!(plan.max_tokens, 8_192);
        assert_eq!(
            plan.cache,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 }
        );
        assert!(!plan.cache_warmer_eligible);
    }

    #[test]
    fn explicit_caller_budget_is_applied() {
        let model = text_model();
        let context = context_with_chars(700);
        let manifest = CapabilitySet::language_chat();
        let adapter = CapabilitySet::language_chat();
        let requirements = RequestRequirements::default();
        let mut input =
            RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
        input.caller_max_tokens = Some(1_024);
        let plan = prepare_request(&input).expect("plan");
        // 1024 < floor 2457，公式要求结果不小于 floor；这里锁定的是「显式值参与 min 后
        // 仍被 floor 抬升」这一行为。
        assert_eq!(plan.max_tokens, 2_457);
    }

    #[test]
    fn image_model_accepts_image_generation() {
        let model = Model {
            id: "image-model".into(),
            provider: "openai".into(),
            context_window: Some(10_000),
            max_tokens: Some(4_000),
            ..Model::default()
        };
        let context = context_with_chars(100);
        let manifest = CapabilitySet::image_generation();
        let adapter = CapabilitySet::image_generation();
        let requirements = RequestRequirements {
            operation: ModelOperation::ImageGeneration,
            ..RequestRequirements::default()
        };
        let input = RequestPolicyInput::new(&model, &context, &manifest, &adapter, &requirements);
        let plan = prepare_request(&input).expect("plan");
        assert_eq!(plan.max_tokens, 4_000);
    }
}
