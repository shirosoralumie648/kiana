//! 输出预算（max output tokens）计算。
//!
//! # 公式
//!
//! ```text
//! available = context_window - estimated_prompt - 1024 reserve   （窗口已知且 > 0 时）
//! budget    = min(caller_max_tokens?, model.max_output?, available?, product_default 32000)
//! floor     = min(floor(0.3 * model.max_output), available)      （model.max_output 存在时）
//! max_tokens = max(budget, floor)
//! ```
//!
//! 直接后果：
//!
//! - `available < 1` 时返回 typed [`ContextOverflow`]，调用方应在发网络前压缩或换模型；
//! - 最终值永远 `<= available`（窗口已知时），也永远 `>= floor`；
//! - floor 会把「产品默认 32k」抬到模型输出预算的 30%（例如 202800 → 60840），但不会
//!   把低于 32k 的小模型抬高到 32k 以上——`model.max_output` 仍在 caps 里；
//! - 调用方显式传入 `Some(0)` 时保留 0 语义，不参与 floor 抬升。显式 0 是「让模型不产出
//!   任何 token」，不是「未指定」；`None` 才表示未指定。
//!
//! 设计参考 cline `providers/gateway.ts#resolveGatewayRequestMaxTokens` 的
//! `DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS=32000`、`DEFAULT_GATEWAY_MAX_OUTPUT_FRACTION=0.3`
//! 与 `GATEWAY_OUTPUT_RESERVE_TOKENS=1024`；Kiana 用显式输入结构重写，并把
//! `available < 1` 变成 typed error 而不是 `None`。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiErrorKind};
use crate::types::Model;

/// 产品默认输出上限（32k）。
pub const PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_000;
/// 给上下文窗口预留的安全余量（1024）。
pub const OUTPUT_RESERVE_TOKENS: u64 = 1_024;
/// floor 相对模型输出预算的比例（30%）。
pub const OUTPUT_FLOOR_FRACTION: f64 = 0.3;

/// 预算输入。所有字段都是调用方可控的纯数据，便于随机化测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputBudgetInput {
    /// 调用方显式指定的输出上限；`None` 表示未指定，`Some(0)` 表示显式 0。
    pub caller_max_tokens: Option<u64>,
    /// 模型目录声明的最大输出 token。
    pub model_max_output: Option<u64>,
    /// 模型上下文窗口；`None` 或 `Some(0)` 表示未知（不做上下文裁剪）。
    pub context_window: Option<u64>,
    /// 发请求前的 prompt token 估算。
    pub estimated_prompt_tokens: u64,
    /// 产品默认上限；通常用 [`PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS`]。
    pub product_default_max_output: u64,
    /// 上下文预留；通常用 [`OUTPUT_RESERVE_TOKENS`]。
    pub reserve_tokens: u64,
}

impl Default for OutputBudgetInput {
    fn default() -> Self {
        OutputBudgetInput {
            caller_max_tokens: None,
            model_max_output: None,
            context_window: None,
            estimated_prompt_tokens: 0,
            product_default_max_output: PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS,
            reserve_tokens: OUTPUT_RESERVE_TOKENS,
        }
    }
}

impl OutputBudgetInput {
    /// 用模型与估算 prompt 构造（产品默认与预留取常量）。
    pub fn for_model(
        model: &Model,
        estimated_prompt_tokens: u64,
        caller_max_tokens: Option<u64>,
    ) -> Self {
        OutputBudgetInput {
            caller_max_tokens,
            model_max_output: model.max_tokens,
            context_window: model.context_window,
            estimated_prompt_tokens,
            product_default_max_output: PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS,
            reserve_tokens: OUTPUT_RESERVE_TOKENS,
        }
    }
}

/// 命中最小值的 cap 来源（诊断用；并列时按 caller → model → context → default 取先者）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCap {
    /// 调用方显式上限。
    Caller,
    /// 模型输出上限。
    Model,
    /// 上下文剩余空间。
    Context,
    /// 产品默认上限。
    ProductDefault,
}

impl BudgetCap {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetCap::Caller => "caller",
            BudgetCap::Model => "model",
            BudgetCap::Context => "context",
            BudgetCap::ProductDefault => "product_default",
        }
    }
}

/// 计算成功后的预算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputBudget {
    /// 最终输出上限。
    pub max_tokens: u64,
    /// 裁剪前的 min 结果（未应用 floor）。
    pub min_budget: u64,
    /// floor 值；没有模型输出预算时为 0。
    pub floor: u64,
    /// 上下文剩余空间；窗口未知时为 `None`。
    pub available: Option<u64>,
    /// 最小值来自哪个 cap。
    pub capped_by: BudgetCap,
    /// floor 是否真的抬高了结果。
    pub floor_applied: bool,
}

/// 上下文溢出（typed）。`available` 用 `i64` 保留负值，便于调用方展示差多少。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextOverflow {
    /// 模型上下文窗口。
    pub context_window: u64,
    /// prompt token 估算。
    pub estimated_prompt_tokens: u64,
    /// 预留 token。
    pub reserve_tokens: u64,
    /// `context_window - estimated_prompt_tokens - reserve_tokens`（可为负）。
    pub available: i64,
}

impl fmt::Display for ContextOverflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "context window overflow: context_window={}, estimated_prompt={}, reserve={}, \
             available={}",
            self.context_window, self.estimated_prompt_tokens, self.reserve_tokens, self.available
        )
    }
}

impl std::error::Error for ContextOverflow {}

impl From<ContextOverflow> for AiError {
    fn from(error: ContextOverflow) -> Self {
        AiError::new(AiErrorKind::Request, error.to_string())
            .with_name("ContextOverflowError")
            .with_detail(serde_json::to_value(error).unwrap_or(serde_json::Value::Null))
    }
}

/// 预算错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputBudgetError {
    /// 上下文剩余空间不足 1 token。
    ContextOverflow(ContextOverflow),
}

impl fmt::Display for OutputBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputBudgetError::ContextOverflow(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for OutputBudgetError {}

impl From<OutputBudgetError> for AiError {
    fn from(error: OutputBudgetError) -> Self {
        match error {
            OutputBudgetError::ContextOverflow(error) => error.into(),
        }
    }
}

/// `floor(0.3 * model_max_output)`。
///
/// 用 `f64` 是因为公式本身是比例；对超过 2^53 的 `u64` 会有精度损失，但模型输出预算
/// 远小于该范围。负值/非有限值不可达（入参是 `u64`）。
fn output_floor_for(model_max_output: u64) -> u64 {
    (model_max_output as f64 * OUTPUT_FLOOR_FRACTION).floor() as u64
}

/// 计算输出预算。
///
/// # 边界
///
/// - `context_window = None` 或 `Some(0)`：不做上下文裁剪，`available = None`；
/// - `context_window` 已知且 `available < 1`：返回 [`OutputBudgetError::ContextOverflow`]；
///   **先于**显式 0 判定，因为请求本身已经放不进窗口；
/// - `caller_max_tokens = Some(0)` 且窗口仍有余量：最终值保留 0（floor 不抬升）；
/// - `product_default_max_output = 0` 是调用方错误配置，函数按普通 cap 0 处理（结果 0）。
pub fn compute_output_budget(input: &OutputBudgetInput) -> Result<OutputBudget, OutputBudgetError> {
    let available = match input.context_window {
        Some(window) if window > 0 => {
            let available = window as i128
                - input.estimated_prompt_tokens as i128
                - input.reserve_tokens as i128;
            if available < 1 {
                return Err(OutputBudgetError::ContextOverflow(ContextOverflow {
                    context_window: window,
                    estimated_prompt_tokens: input.estimated_prompt_tokens,
                    reserve_tokens: input.reserve_tokens,
                    available: available.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
                }));
            }
            Some(available as u64)
        }
        _ => None,
    };

    // caps 的顺序决定并列时 `capped_by` 的取值。
    let mut caps: Vec<(u64, BudgetCap)> = Vec::with_capacity(4);
    if let Some(caller) = input.caller_max_tokens {
        caps.push((caller, BudgetCap::Caller));
    }
    if let Some(model_max_output) = input.model_max_output {
        caps.push((model_max_output, BudgetCap::Model));
    }
    if let Some(available) = available {
        caps.push((available, BudgetCap::Context));
    }
    caps.push((input.product_default_max_output, BudgetCap::ProductDefault));

    let (min_budget, capped_by) = *caps
        .iter()
        .min_by_key(|(value, _)| *value)
        .expect("caps is never empty");

    let floor = match input.model_max_output {
        Some(model_max_output) => {
            let floor = output_floor_for(model_max_output);
            match available {
                Some(available) => floor.min(available),
                None => floor,
            }
        }
        None => 0,
    };

    // 显式 0 是「不产出」的语义，floor 不得把它抬起来。
    let explicit_zero = input.caller_max_tokens == Some(0);
    let max_tokens = if explicit_zero {
        0
    } else {
        min_budget.max(floor)
    };

    Ok(OutputBudget {
        max_tokens,
        min_budget,
        floor,
        available,
        capped_by,
        floor_applied: max_tokens > min_budget,
    })
}

/// 用模型与 prompt 估算直接计算。
pub fn output_budget_for_model(
    model: &Model,
    estimated_prompt_tokens: u64,
    caller_max_tokens: Option<u64>,
) -> Result<OutputBudget, OutputBudgetError> {
    compute_output_budget(&OutputBudgetInput::for_model(
        model,
        estimated_prompt_tokens,
        caller_max_tokens,
    ))
}

/// `0.3 * model.max_output` 的整数 floor（供诊断/测试使用）。
pub fn output_floor(model_max_output: u64) -> u64 {
    output_floor_for(model_max_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        caller: Option<u64>,
        model_max: Option<u64>,
        window: Option<u64>,
        prompt: u64,
    ) -> OutputBudgetInput {
        OutputBudgetInput {
            caller_max_tokens: caller,
            model_max_output: model_max,
            context_window: window,
            estimated_prompt_tokens: prompt,
            ..OutputBudgetInput::default()
        }
    }

    #[test]
    fn applies_product_default_when_nothing_is_known() {
        let budget = compute_output_budget(&input(None, None, None, 0)).expect("budget");
        assert_eq!(budget.max_tokens, PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS);
        assert_eq!(budget.floor, 0);
        assert_eq!(budget.capped_by, BudgetCap::ProductDefault);
        assert!(!budget.floor_applied);
    }

    #[test]
    fn raises_the_default_to_thirty_percent_of_a_large_model_budget() {
        let budget = compute_output_budget(&input(None, Some(202_800), Some(202_800), 1_000))
            .expect("budget");
        assert_eq!(budget.max_tokens, 60_840);
        assert_eq!(budget.floor, 60_840);
        assert!(budget.floor_applied);
    }

    #[test]
    fn a_small_model_output_budget_is_never_raised_above_its_own_cap() {
        let budget =
            compute_output_budget(&input(None, Some(8_000), Some(200_000), 1_000)).expect("budget");
        assert_eq!(budget.max_tokens, 8_000);
        assert_eq!(budget.floor, 2_400);
        assert!(!budget.floor_applied);
        assert_eq!(budget.capped_by, BudgetCap::Model);
    }

    #[test]
    fn context_remaining_space_is_a_hard_ceiling() {
        let budget =
            compute_output_budget(&input(Some(202_800), Some(202_800), Some(202_800), 201_500))
                .expect("budget");
        assert_eq!(budget.available, Some(276));
        assert_eq!(budget.max_tokens, 276);
        assert_eq!(budget.capped_by, BudgetCap::Context);
    }

    #[test]
    fn available_below_one_is_a_typed_overflow() {
        let error = compute_output_budget(&input(Some(8_192), Some(202_800), Some(10_000), 9_500))
            .expect_err("overflow");
        match error {
            OutputBudgetError::ContextOverflow(overflow) => {
                assert_eq!(overflow.context_window, 10_000);
                assert_eq!(overflow.estimated_prompt_tokens, 9_500);
                assert_eq!(overflow.reserve_tokens, OUTPUT_RESERVE_TOKENS);
                assert_eq!(overflow.available, -524);
            }
        }
    }

    #[test]
    fn explicit_zero_is_preserved_even_when_a_floor_would_apply() {
        let budget = compute_output_budget(&input(Some(0), Some(202_800), Some(202_800), 1_000))
            .expect("budget");
        assert_eq!(budget.max_tokens, 0);
        assert_eq!(budget.min_budget, 0);
        assert!(!budget.floor_applied);
    }

    #[test]
    fn unknown_context_window_skips_the_context_cap() {
        for window in [None, Some(0)] {
            let budget = compute_output_budget(&input(Some(8_192), Some(202_800), window, 999_999))
                .expect("budget");
            assert_eq!(budget.available, None);
            assert_eq!(budget.max_tokens, 60_840);
        }
    }

    #[test]
    fn available_exactly_zero_is_typed_overflow() {
        // 2_000 - 976 - 1024 == 0 → < 1 → typed overflow。
        let error = compute_output_budget(&input(None, Some(202_800), Some(2_000), 976))
            .expect_err("overflow");
        match error {
            OutputBudgetError::ContextOverflow(overflow) => assert_eq!(overflow.available, 0),
        }
    }

    #[test]
    fn invariant_holds_for_pseudo_random_inputs() {
        // 确定性 LCG：0..64k 窗口、0..300k 模型预算、0..40k prompt、随机 caller。
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        };
        for _ in 0..20_000 {
            let window = Some(next() % 65_536 + 1);
            let model_max = Some(next() % 300_000 + 1);
            let prompt = next() % 45_000;
            let caller = match next() % 3 {
                0 => None,
                1 => Some(next() % 200_000),
                _ => Some(0),
            };
            match compute_output_budget(&input(caller, model_max, window, prompt)) {
                Ok(budget) => {
                    let available = budget.available.expect("window known");
                    assert!(budget.max_tokens <= available, "{budget:?}");
                    if caller != Some(0) {
                        assert!(budget.max_tokens >= budget.floor, "{budget:?}");
                    } else {
                        assert_eq!(budget.max_tokens, 0);
                    }
                }
                Err(OutputBudgetError::ContextOverflow(overflow)) => {
                    assert_eq!(
                        overflow.available,
                        window.unwrap() as i64 - prompt as i64 - OUTPUT_RESERVE_TOKENS as i64
                    );
                    assert!(overflow.available < 1);
                }
            }
        }
    }
}
