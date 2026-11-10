//! `utils/openrouter-cost.ts`——用 OpenRouter 上报的计费替换目录价。
//!
//! `applyOpenRouterReportedCost` 只在响应带**完整且有效**的 accounting 时改写
//! `usage.cost.total` 并把 `totalFromProvider` 置真。BYOK 请求要把上游推理成本加到
//! OpenRouter 的收费上（`total = cost + upstream`）。
//!
//! TS: `utils/openrouter-cost.ts`
//!
//! # 为什么需要覆盖目录价
//!
//! `calculate_cost` 按模型目录的单价在本地计算费用，但请求经 OpenRouter 转发时实际账单
//! 由 OpenRouter 结算：响应 `usage` 里的 `cost` 是记到用户 OpenRouter 账户的金额，
//! `cost_details` 则单列走上游 provider key（BYOK）的推理成本。两者与目录价可能不同，
//! 因此本模块在字段齐全时用 provider 上报值替换 `usage.cost.total`；字段缺失或非法时
//! **保持**本地计算值，不报错、不把 `total_from_provider` 置 `false`。
//!
//! # 数据结构与 TS 的对应
//!
//! [`OpenRouterUsageAccounting`] / [`OpenRouterCostDetails`] 对应 TS 的两个内联类型：
//! `number | null` → `Option<f64>`，`is_byok: boolean` → `bool`，
//! `cost_details: {...} | null` → `Option<...>`。`server_tool_cost` 不参与计算：它已经
//! 包含在 OpenRouter 的 `cost` 里（TS 字段注释）。
//!
//! # 不变量
//!
//! - 只有 `total_from_provider == Some(true)` 时 `usage.cost.total` 才是账单值；此时
//!   `input`/`output`/`cache_read`/`cache_write` 仍是目录价算出的分项，四项之和**不一定**
//!   等于 `total`（BYOK 时 `total = cost + upstream`）。
//! - 失败路径不修改 `usage`：调用方可能在本次之前已经设置过 `total_from_provider`，保持
//!   原值才能表达“本次没有 provider 确认”。
//!
//! # 调用方
//!
//! `api::openai_completions`（流式 SSE chunk 的顶层 `usage` 或 `choice.usage`；该适配器
//! 的请求固定写 `stream: true`，没有非流式入口）与 `api::anthropic_messages`（OpenRouter
//! 的 Anthropic 兼容端点）在 [`is_open_router_model`] 为真时调用。两者都先把原始 JSON
//! 过滤成类型化结构，无法表达的 `undefined` 在构造前就被丢弃（见
//! `openai_completions::apply_open_router_accounting` 与
//! `anthropic_messages::open_router_usage_accounting`）。

use crate::types::Usage;

/// `OpenRouterUsageAccounting.cost_details`。
///
/// 字段含义见 TS 内联类型；`upstream_inference_prompt_cost` /
/// `upstream_inference_completions_cost` 在 TS 中是必填 `number`，因此这里是 `f64`
/// 而不是 `Option<f64>`：缺失/非数字在适配器解析 JSON 时就被过滤掉，不会构造出本结构。
/// 本结构不校验金额是否为有限非负数，校验统一在 [`apply_open_router_reported_cost`]。
///
/// # `upstream_inference_cost` 的 `None` 只表示 `null`
///
/// 类型化结构无法区分 TS 的 `undefined`（键缺失）与 `null`：`None` 在这里按 TS 的
/// **显式 `null`** 处理，会触发“prompt + completions 求和”的回退分支。直接构造本结构
/// 的调用方必须先在缺失键时放弃调用 [`apply_open_router_reported_cost`]（仓库内两个
/// 适配器都这样做）；否则会在 TS 本应“什么都不做”的场景下改写 `usage`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenRouterCostDetails {
    /// 通过用户上游 key 计费的总推理成本（不可得时 `None`，对应 TS 的 `null`）。
    pub upstream_inference_cost: Option<f64>,
    /// prompt 处理的上游成本（含 provider 侧缓存定价）。
    pub upstream_inference_prompt_cost: f64,
    /// 生成完成的上游成本。
    pub upstream_inference_completions_cost: f64,
    /// 已包含在 OpenRouter 收费 `cost` 内的服务端工具成本（本函数不使用）。
    pub server_tool_cost: Option<f64>,
}

/// `OpenRouterUsageAccounting`。
///
/// Rust 侧用 `Option` 表达 TS 的 `null`，但**无法表达 `undefined`**：TS 的事件对象是
/// 运行时强转的，字段可能缺失；各适配器在构造本结构前会先按 TS 的严格分支
/// （`is_byok === false` / `=== true`）过滤，缺失或类型不符时根本不调用
/// [`apply_open_router_reported_cost`]。因此本结构的 `is_byok` 一定是确定的布尔值。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenRouterUsageAccounting {
    /// 计入用户 OpenRouter 账户的金额（`null` 表示不可得）。
    pub cost: Option<f64>,
    /// OpenRouter 是否使用了用户的上游 provider key。
    pub is_byok: bool,
    /// 明细（`null` 表示不可得）。
    pub cost_details: Option<OpenRouterCostDetails>,
}

/// `isValidCost(v)`：数字、有限、且 `>= 0`。
///
/// `None`（TS 的 `null`/`undefined`）、`NaN`、正负无穷与负数都返回 `false`；`0.0` 与
/// `-0.0` 返回 `true`（`-0.0 >= 0.0` 成立，与 JS 的 `>=` 一致）。作为纯判定函数，它不
/// 记录日志也不报错。
pub fn is_valid_cost(value: Option<f64>) -> bool {
    matches!(value, Some(value) if value.is_finite() && value >= 0.0)
}

/// `isOpenRouterModel(model)`：`provider === "openrouter"` 或 `baseUrl` 含 `openrouter.ai`。
///
/// 两个判定都是原样字符串比较：`provider` 精确等于 `"openrouter"`（`"openrouter-work"`
/// 这类自定义 provider 不算，除非 URL 命中）；`base_url` 是区分大小写的**子串**检查，
/// 不解析 host，因此路径/查询串里出现该子串也会命中（与 TS 的 `.includes` 一致）。
///
/// TS: `utils/openrouter-cost.ts#isOpenRouterModel`
pub fn is_open_router_model(provider: &str, base_url: &str) -> bool {
    provider == "openrouter" || base_url.contains("openrouter.ai")
}

/// `applyOpenRouterReportedCost(usage, reported)`。
///
/// # 执行顺序
///
/// 1. `reported.cost` 必须是有效金额（有限且 `>= 0`），否则原样返回；
/// 2. 非 BYOK：`total = cost`，`total_from_provider = Some(true)`；
/// 3. BYOK：必须有 `cost_details`；`upstream_inference_cost` 有值就直接使用，为 `None`
///    （对应 TS 的 `null`）才回落到 prompt + completions 两项之和，且两项都必须有效；
/// 4. 上游成本通过有效性检查后，`total = cost + upstream`，并标记来自 provider。
///
/// # 失败模式
///
/// 任一步不满足都静默保留调用前的 `total`/`total_from_provider`（不返回错误）。因此
/// “provider 上报了但字段不完整”与“provider 没上报”在本函数内不可区分；需要区分的
/// 调用方应自行检查 `total_from_provider`，不要用 `total` 是否变化来推断。
///
/// TS: `utils/openrouter-cost.ts#applyOpenRouterReportedCost`
pub fn apply_open_router_reported_cost(usage: &mut Usage, reported: &OpenRouterUsageAccounting) {
    let Some(cost) = reported.cost.filter(|value| is_valid_cost(Some(*value))) else {
        return;
    };
    // `is_byok` 既非 true 也非 false（这里是 `false` 之外的默认值）时什么都不做。
    // TS 的 `is_byok` 是必填布尔，因此 Rust 侧用同一个字段表达两种分支。
    // `bool` 只有两个取值，`!is_byok` 精确对应 TS 的 `=== false`；TS 运行期可能出现的
    // `undefined`/非布尔值在构造本结构前就被适配器过滤（见模块文档的调用方列表）。
    if !reported.is_byok {
        usage.cost.total = cost;
        usage.cost.total_from_provider = Some(true);
        return;
    }
    let Some(details) = &reported.cost_details else {
        return;
    };
    // 优先使用上游总成本；只有它的值为 `None`（TS 的显式 `null`）时才回落到两个分项。
    // 「键缺失」在 TS 中会让 isValidCost 失败并直接返回，适配器已在构造前区分这两种情况。
    let upstream = match details.upstream_inference_cost {
        Some(value) => value,
        None => {
            if !is_valid_cost(Some(details.upstream_inference_prompt_cost))
                || !is_valid_cost(Some(details.upstream_inference_completions_cost))
            {
                return;
            }
            details.upstream_inference_prompt_cost + details.upstream_inference_completions_cost
        }
    };
    if !is_valid_cost(Some(upstream)) {
        return;
    }
    // 与 TS 一样只校验两个加数，不校验和：两个接近 f64::MAX 的合法值相加会得到 inf；
    // 本函数不为该溢出做额外处理，以保持与 JS `Number` 加法一致。
    usage.cost.total = cost + upstream;
    usage.cost.total_from_provider = Some(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(total: f64) -> Usage {
        let mut usage = Usage {
            input: 1,
            output: 1,
            total_tokens: 2,
            ..Usage::default()
        };
        usage.cost.input = 0.25;
        usage.cost.output = 0.75;
        usage.cost.total = total;
        usage
    }

    #[test]
    fn recognizes_openrouter_models() {
        assert!(is_open_router_model("openrouter", "https://example.com"));
        assert!(is_open_router_model(
            "openrouter-work",
            "https://openrouter.ai/api/v1"
        ));
        assert!(!is_open_router_model("custom", "https://example.com/v1"));
    }

    #[test]
    fn uses_reported_cost_for_non_byok_requests() {
        let mut value = usage(1.0);
        apply_open_router_reported_cost(
            &mut value,
            &OpenRouterUsageAccounting {
                cost: Some(0.0),
                is_byok: false,
                cost_details: Some(OpenRouterCostDetails::default()),
            },
        );
        assert_eq!(value.cost.total, 0.0);
        assert_eq!(value.cost.total_from_provider, Some(true));
    }

    #[test]
    fn adds_upstream_cost_for_byok_requests() {
        let mut value = usage(1.0);
        apply_open_router_reported_cost(
            &mut value,
            &OpenRouterUsageAccounting {
                cost: Some(0.1),
                is_byok: true,
                cost_details: Some(OpenRouterCostDetails {
                    upstream_inference_cost: Some(0.9),
                    upstream_inference_prompt_cost: 0.6,
                    upstream_inference_completions_cost: 0.3,
                    server_tool_cost: None,
                }),
            },
        );
        assert!((value.cost.total - 1.0).abs() < 1e-12);
        assert_eq!(value.cost.total_from_provider, Some(true));
    }

    #[test]
    fn uses_upstream_components_when_total_is_omitted() {
        let mut value = usage(1.0);
        apply_open_router_reported_cost(
            &mut value,
            &OpenRouterUsageAccounting {
                cost: Some(0.1),
                is_byok: true,
                cost_details: Some(OpenRouterCostDetails {
                    upstream_inference_cost: None,
                    upstream_inference_prompt_cost: 0.6,
                    upstream_inference_completions_cost: 0.3,
                    server_tool_cost: None,
                }),
            },
        );
        assert!((value.cost.total - 1.0).abs() < 1e-12);
        assert_eq!(value.cost.total_from_provider, Some(true));
    }

    #[test]
    fn keeps_catalog_total_when_accounting_is_incomplete() {
        let mut value = usage(1.0);
        value.cost.total_from_provider = Some(false);
        apply_open_router_reported_cost(
            &mut value,
            &OpenRouterUsageAccounting {
                cost: None,
                is_byok: false,
                cost_details: None,
            },
        );
        apply_open_router_reported_cost(
            &mut value,
            &OpenRouterUsageAccounting {
                cost: Some(0.1),
                is_byok: true,
                cost_details: None,
            },
        );
        assert_eq!(value.cost.total, 1.0);
        assert_eq!(value.cost.total_from_provider, Some(false));
    }

    #[test]
    fn rejects_negative_or_non_finite_costs() {
        assert!(!is_valid_cost(Some(-1.0)));
        assert!(!is_valid_cost(Some(f64::NAN)));
        assert!(!is_valid_cost(Some(f64::INFINITY)));
        assert!(!is_valid_cost(None));
        assert!(is_valid_cost(Some(0.0)));
    }
}
