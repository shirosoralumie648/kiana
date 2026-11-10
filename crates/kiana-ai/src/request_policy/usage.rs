//! Usage 记账：canonical / 用户声明 / provider 上报的优先级。
//!
//! 一次模型调用可能同时存在三份 usage：
//!
//! - **canonical**：用请求/响应 token 数按模型目录价格重算出的结果（[`crate::models::cost`]；
//!   它会把 `total_from_provider` 置为 `Some(false)`）；
//! - **用户声明**：会话/配置里用户显式写下的 usage 或费用覆盖；
//! - **provider-reported**：provider 返回的 token 与账单金额。
//!
//! 记账优先级固定为 canonical → 用户声明 → provider-reported，并记录
//! [`CostSource`]，让上层能回答「这个 cost 是目录价、用户指定还是 provider 账单」。
//!
//! canonical 只在模型目录确实配置了非零费率时生成：全零费率无法区分「免费」与「没配价」，
//! 此时退回用户声明/provider 上报，避免把未知价格当成 0 记账。

use serde::{Deserialize, Serialize};

use crate::types::{Model, Usage};

/// 费用来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// 按模型目录价格重算（canonical）。
    Canonical,
    /// 用户/会话显式声明。
    UserDeclared,
    /// provider 上报。
    ProviderReported,
}

impl CostSource {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            CostSource::Canonical => "canonical",
            CostSource::UserDeclared => "user_declared",
            CostSource::ProviderReported => "provider_reported",
        }
    }
}

/// 记账后的 usage。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountedUsage {
    /// 选中的 usage（含费用明细）。
    pub usage: Usage,
    /// 费用来源。
    pub cost_source: CostSource,
}

/// 模型目录是否配置了可用的价格。
///
/// 任一分项（输入/输出/cache read/cache write）为有限且非零即视为已知；全零视为未知，
/// 与 `budget` 模块的 `cost_unknown` 口径一致。
pub fn has_known_model_pricing(model: &Model) -> bool {
    let cost = &model.cost;
    [cost.input, cost.output, cost.cache_read, cost.cache_write]
        .iter()
        .any(|rate| rate.is_finite() && *rate != 0.0)
}

/// 用模型目录价格生成 canonical usage；价格未知时返回 `None`。
pub fn canonical_usage(model: &Model, usage: &Usage) -> Option<Usage> {
    if !has_known_model_pricing(model) {
        return None;
    }
    let mut canonical = usage.clone();
    crate::models::cost::calculate_cost(model, &mut canonical);
    Some(canonical)
}

/// 按 canonical → 用户声明 → provider-reported 的优先级选择一份 usage。
///
/// 全部缺失时返回 `None`。
pub fn resolve_usage(
    canonical: Option<Usage>,
    user_declared: Option<Usage>,
    provider_reported: Option<Usage>,
) -> Option<AccountedUsage> {
    if let Some(usage) = canonical {
        return Some(AccountedUsage {
            usage,
            cost_source: CostSource::Canonical,
        });
    }
    if let Some(usage) = user_declared {
        return Some(AccountedUsage {
            usage,
            cost_source: CostSource::UserDeclared,
        });
    }
    provider_reported.map(|usage| AccountedUsage {
        usage,
        cost_source: CostSource::ProviderReported,
    })
}

/// 用 provider 上报的 token 与可选的用户声明记账。
///
/// - 模型价格已知：canonical（目录价）优先；
/// - 价格未知但用户给了声明：用户声明；
/// - 都没有：provider 上报。
pub fn account_provider_usage(
    model: &Model,
    provider_reported: &Usage,
    user_declared: Option<&Usage>,
) -> AccountedUsage {
    resolve_usage(
        canonical_usage(model, provider_reported),
        user_declared.cloned(),
        Some(provider_reported.clone()),
    )
    .expect("provider-reported usage is always present")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCost, UsageCost};

    fn priced_model() -> Model {
        Model {
            id: "m".into(),
            provider: "openai".into(),
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

    fn unpriced_model() -> Model {
        Model {
            id: "m".into(),
            provider: "custom".into(),
            ..Model::default()
        }
    }

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            total_tokens: input + output,
            ..Usage::default()
        }
    }

    #[test]
    fn canonical_wins_over_user_declared_and_provider_reported() {
        let canonical = usage(10, 20);
        let user = usage(30, 40);
        let provider = usage(50, 60);
        let accounted =
            resolve_usage(Some(canonical.clone()), Some(user), Some(provider)).expect("accounted");
        assert_eq!(accounted.usage, canonical);
        assert_eq!(accounted.cost_source, CostSource::Canonical);
    }

    #[test]
    fn user_declared_wins_over_provider_reported() {
        let user = usage(30, 40);
        let provider = usage(50, 60);
        let accounted = resolve_usage(None, Some(user.clone()), Some(provider)).expect("accounted");
        assert_eq!(accounted.usage, user);
        assert_eq!(accounted.cost_source, CostSource::UserDeclared);
    }

    #[test]
    fn provider_reported_is_the_last_resort() {
        let provider = usage(50, 60);
        let accounted = resolve_usage(None, None, Some(provider.clone())).expect("accounted");
        assert_eq!(accounted.usage, provider);
        assert_eq!(accounted.cost_source, CostSource::ProviderReported);
        assert!(resolve_usage(None, None, None).is_none());
    }

    #[test]
    fn catalog_pricing_produces_a_canonical_ledger() {
        let model = priced_model();
        let provider = usage(1_000_000, 1_000_000);
        let accounted = account_provider_usage(&model, &provider, None);
        assert_eq!(accounted.cost_source, CostSource::Canonical);
        assert_eq!(accounted.usage.cost.input, 3.0);
        assert_eq!(accounted.usage.cost.output, 15.0);
        assert_eq!(accounted.usage.cost.total, 18.0);
        assert_eq!(accounted.usage.cost.total_from_provider, Some(false));
    }

    #[test]
    fn unknown_pricing_falls_back_to_user_declared_then_provider() {
        let model = unpriced_model();
        let provider = usage(100, 50);
        let mut declared = usage(100, 50);
        declared.cost = UsageCost {
            total: 0.42,
            ..UsageCost::default()
        };
        let accounted = account_provider_usage(&model, &provider, Some(&declared));
        assert_eq!(accounted.cost_source, CostSource::UserDeclared);
        assert_eq!(accounted.usage.cost.total, 0.42);

        let accounted = account_provider_usage(&model, &provider, None);
        assert_eq!(accounted.cost_source, CostSource::ProviderReported);
    }
}
