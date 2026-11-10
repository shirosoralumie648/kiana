//! calculateCost 与价格分档。
//!
//! TS: `src/models.ts#calculateCost`。就地更新 `usage.cost`，返回同一个 cost 对象；
//! Rust 侧接收 `&mut Usage` 并把更新后的 [`UsageCost`] 克隆返回（调用方可以忽略返回值）。
//!
//! # 这个模块解决什么问题
//!
//! 各 provider 适配器把服务商返回的 token 用量写进 [`Usage`] 之后调用 [`calculate_cost`]，
//! 用模型目录里的「每百万 token 单价 + 请求级分档」算出这一次请求的美元金额，写进
//! `usage.cost` 供上层展示与累计。它不发请求、不读全局状态，只依赖 `model.cost` 与
//! `usage` 的计数字段；除写 `usage.cost` 外没有副作用。
//!
//! # 计费规则（与 TS 逐条对齐，`spec/ai-core.md` §2.1.3）
//!
//! 1. 分档选择用输入侧总量 `input + cache_read + cache_write`，与每个
//!    [`crate::types::ModelCostTier`] 的 `input_tokens_above` 做**严格大于**比较；阈值不同的
//!    多个档位同时命中时取阈值最高的那个；阈值相同的并列档位由声明顺序决定，先声明者
//!    生效（后声明者的阈值不满足“严格大于已匹配阈值”的循环条件）。
//! 2. 命中档位后四个费率**整组替换**基础费率（不叠加、不按区间分段）；未命中则用
//!    [`crate::types::ModelCost`] 顶层的四个费率。
//! 3. 1 小时 cache write（`usage.cache_write1h`）按 2x **输入费率**计价，其余 cache write
//!    按 cache write 费率；两者合并为一个 `cache_write` 金额。
//! 4. `total` 是四项之和，`total_from_provider` 被置为 `Some(false)`：结果来自目录价而
//!    不是服务商账单，即使调用前该字段是 provider 上报的 `true` 也会被覆盖。
//!
//! # 边界与注意事项
//!
//! - 单价单位是 USD / 百万 token，代码里的 `/ 1_000_000.0` 不能省。
//! - `short_write` 用 f64 相减而不是 `saturating_sub`：当 `cache_write1h > cache_write`
//!   （上游计数不一致）时中间量 `short_write` 为负，因此 `rates.cache_write * short_write`
//!   这一分量为负；最终 `usage.cost.cache_write` 仍可能为正（测试场景为 +0.00001375）。
//!   这是 TS 的既有行为，测试 `allows_a_negative_short_cache_write_for_inconsistent_usage`
//!   锁定的是这种非饱和相减，而不是“费用项为负”。
//! - token 计数是 `u64`，`input + cache_read + cache_write` 用饱和加法：极端值停在
//!   `u64::MAX`，不会在 debug 构建 panic、也不会在 release 构建回绕成小值。计数转 f64
//!   参与阈值比较时，不处理超过 2^53 的精度损失。

use crate::types::{Model, ModelCostRates, Usage, UsageCost};

/// 按模型价格（含请求级分档）计算一次请求的费用。
///
/// `inputTokens = input + cacheRead + cacheWrite`；tier 只在
/// `inputTokens > inputTokensAbove` 且阈值严格大于已匹配阈值时生效（取最高的匹配阈值）。
/// 1 小时 cache write 按 2x input 计价。
///
/// # 参数与副作用
///
/// - `model`：只读取 `model.cost`（基础费率与可选 `tiers`），不会被修改。
/// - `usage`：读取 `input`/`output`/`cache_read`/`cache_write`/`cache_write1h`；
///   写回 `usage.cost` 的四个分项金额、`total` 与 `total_from_provider`。
/// - 返回值：写回后的 [`UsageCost`] 克隆（与 `usage.cost` 内容相同）；调用方可以忽略。
///
/// 函数没有 `Result`、也没有校验分支：计数字段异常时按饱和语义参与计算（见模块文档）。
///
/// TS: `src/models.ts#calculateCost`
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    // 分档判断用“请求级输入总量”：缓存读/写的 token 也计入（它们同属本次请求的输入）。
    // 饱和相加：provider 报告的值不受本仓库控制，普通 `+` 会在 debug 构建 panic、
    // release 构建回绕成小数，进而选错计价档位。
    let input_tokens = usage
        .input
        .saturating_add(usage.cache_read)
        .saturating_add(usage.cache_write);
    let mut rates = ModelCostRates {
        input: model.cost.input,
        output: model.cost.output,
        cache_read: model.cost.cache_read,
        cache_write: model.cost.cache_write,
    };
    let mut matched_threshold = -1.0_f64;
    // -1.0 是哨兵：目录阈值均为正数，等价于“尚未命中任何档”。
    // 严格大于保证恰好等于阈值时不升档；循环不假设 tiers 有序，靠阈值比较取最高档。
    if let Some(tiers) = &model.cost.tiers {
        for tier in tiers {
            if input_tokens as f64 > tier.input_tokens_above
                && tier.input_tokens_above > matched_threshold
            {
                // 命中档位后四个费率整组替换基础费率（不是与基础价叠加）。
                rates = ModelCostRates {
                    input: tier.input,
                    output: tier.output,
                    cache_read: tier.cache_read,
                    cache_write: tier.cache_write,
                };
                matched_threshold = tier.input_tokens_above;
            }
        }
    }

    // Anthropic charges 2x base input for 1h cache writes.
    // 这里的 2x 用 rates.input：若命中分档，取的是该档的输入费率而不是目录基础价。
    let long_write = usage.cache_write1h.unwrap_or(0);
    // TS: `shortWrite = usage.cacheWrite - longWrite`（无符号数用 f64 相减，保留畸形 usage 的负值）。
    let short_write = usage.cache_write as f64 - long_write as f64;
    let input = (rates.input / 1_000_000.0) * usage.input as f64;
    let output = (rates.output / 1_000_000.0) * usage.output as f64;
    let cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    let cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write as f64) / 1_000_000.0;
    let total = input + output + cache_read + cache_write;

    usage.cost.input = input;
    usage.cost.output = output;
    usage.cost.cache_read = cache_read;
    usage.cost.cache_write = cache_write;
    usage.cost.total = total;
    // 金额来自目录价而非服务商账单；调用前即使标了 Some(true) 也要改回 false，
    // 否则上层会把重算值当成 provider 上报的账单总额。
    usage.cost.total_from_provider = Some(false);
    // TS 返回同一个对象引用；Rust 返回副本，让调用方无需再借一次 usage。
    usage.cost.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCost, ModelCostTier};

    fn model(cost: ModelCost) -> Model {
        Model {
            cost,
            provider: "openai".into(),
            id: "gpt-5.6-sol".into(),
            ..Model::default()
        }
    }

    fn usage(cache_write: u64, cache_write1h: Option<u64>) -> Usage {
        Usage {
            input: 200_000,
            output: 100_000,
            cache_read: 72_000,
            cache_write,
            cache_write1h,
            ..Usage::default()
        }
    }

    fn tiered_cost() -> ModelCost {
        ModelCost {
            input: 5.0,
            output: 30.0,
            cache_read: 0.5,
            cache_write: 6.25,
            tiers: Some(vec![ModelCostTier {
                input_tokens_above: 272_000.0,
                input: 10.0,
                output: 45.0,
                cache_read: 1.0,
                cache_write: 12.5,
            }]),
        }
    }

    #[test]
    fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
        let model = model(tiered_cost());
        let mut short = usage(0, None);
        let cost = calculate_cost(&model, &mut short);
        assert_eq!(cost.input, 1.0);
        assert_eq!(cost.output, 3.0);
        assert_eq!(cost.cache_read, 0.036);
        assert_eq!(cost.cache_write, 0.0);

        let mut long = usage(1, None);
        let cost = calculate_cost(&model, &mut long);
        assert_eq!(cost.input, 2.0);
        assert_eq!(cost.output, 4.5);
        assert_eq!(cost.cache_read, 0.072);
        assert_eq!(cost.cache_write, 0.0000125);
        assert!((cost.total - (2.0 + 4.5 + 0.072 + 0.0000125)).abs() < 1e-12);
    }

    #[test]
    fn marks_catalog_derived_totals_as_not_provider_reported() {
        let mut usage = Usage {
            input: 1_000_000,
            cost: UsageCost {
                total: 9.0,
                total_from_provider: Some(true),
                ..UsageCost::default()
            },
            ..Usage::default()
        };
        calculate_cost(
            &model(ModelCost {
                input: 5.0,
                output: 30.0,
                cache_read: 0.5,
                cache_write: 6.25,
                tiers: None,
            }),
            &mut usage,
        );
        assert_eq!(usage.cost.total_from_provider, Some(false));
    }

    #[test]
    fn allows_a_negative_short_cache_write_for_inconsistent_usage() {
        // TS: `shortWrite = usage.cacheWrite - longWrite`；cacheWrite1h > cacheWrite 时
        // shortWrite 为负，费用里对应项必须为负（不能像 saturating_sub 那样截到 0）。
        // 参考实现（node 探针）：cacheWrite=1, cacheWrite1h=2, rates 6.25/5 ->
        // (6.25 * -1 + 5 * 2 * 2) / 1e6 = 0.00001375。
        let model = model(ModelCost {
            input: 5.0,
            output: 30.0,
            cache_read: 0.5,
            cache_write: 6.25,
            tiers: None,
        });
        let mut usage = Usage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_write: 1,
            cache_write1h: Some(2),
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert_eq!(cost.cache_write, 0.00001375);
        assert_eq!(cost.total, 5.00001375);
    }

    #[test]
    fn saturates_the_input_token_sum_for_tier_selection() {
        // 回归：`input + cache_read + cache_write` 是 provider 可控的 u64 加法。旧实现
        // 在 debug 测试构建里会 panic（release 里回绕成小值，选错计价档位）。
        let model = model(ModelCost {
            input: 5.0,
            output: 30.0,
            cache_read: 0.5,
            cache_write: 6.25,
            tiers: Some(vec![ModelCostTier {
                input_tokens_above: 1.0,
                input: 10.0,
                output: 45.0,
                cache_read: 1.0,
                cache_write: 12.5,
            }]),
        });
        let mut usage = Usage {
            input: u64::MAX - 1,
            output: 0,
            cache_read: 10,
            cache_write: 10,
            cache_write1h: None,
            ..Usage::default()
        };
        let cost = calculate_cost(&model, &mut usage);
        assert!(cost.total.is_finite());
        // 饱和后的总和仍然高于阈值，因此必须命中分档费率（10 USD / 1M input）。
        assert!(cost.input > 0.0);
    }
}
