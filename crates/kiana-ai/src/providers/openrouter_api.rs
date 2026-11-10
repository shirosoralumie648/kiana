//! providers/openrouter-api.ts：OpenRouter 的 per-token 价格 → kiana 的 $/1M tokens。
//!
//! TS: `packages/ai/src/providers/openrouter-api.ts`
//!
//! # 负责的边界
//!
//! OpenRouter 的 `/models`、`/models/user` 把价格表示为「每 token 的美元」（JSON 里通常
//! 是字符串，如 `"0.000003"`），而 kiana 内部 [`ModelCost`] 统一用「每 1M token 的
//! 美元」。本模块只做单位换算与形状转换：不发请求、不读目录、不做凭证处理。
//!
//! 输入是模型条目里的 `pricing` 对象（`prompt`/`completion`/`input_cache_read`/
//! `input_cache_write`/`overrides`）；输出是可直接赋给 `Model::cost` 的结构。
//!
//! # 与目录生成器的关系
//!
//! `model_catalog::openrouter_catalog` 里有一份**独立实现**（`pub mod` 下的
//! `pub fn open_router_cost`，返回 JSON `Value`），用于目录生成阶段把同样的换算写进
//! `data/*.json`。其模块注释说明：定价转换属于 provider 单元，目录生成侧保留副本是为了
//! 避免跨单元重复实现。`build.rs` 在编译期只读取已 vendored 的数据并摊平成静态表，不做
//! 定价换算。两处换算规则必须同步修改。
//!
//! # 与 TS 的偏移
//!
//! [`open_router_cost`] 的返回类型是 `ModelCost`，没有错误通道；而 TS 在 `overrides`
//! 不是数组、或条目是 `null` 时会抛 `TypeError`。这里选择跳过畸形形状；正常上游数据
//! （数组 + 对象条目）按正常语义解析（测试
//! `tolerates_malformed_overrides_instead_of_throwing` 锁定该行为）。

use serde_json::Value;

use crate::model_catalog::{js_parse_float, round_cost};
use crate::types::{ModelCost, ModelCostTier};

/// `perMillion(value, fallback)`：`$ / token` → `$ / 1M tokens`；0 表示免费，缺失/非法/负数用 fallback。
///
/// TS 的类型是 `string | undefined`，但运行时若上游给了数字，`Number.parseFloat` 会先做
/// `String(value)`（与 `generate-models.ts` 的 `perMillion` 一致）。
///
/// 判定用的是 `per_token >= 0`：`parseFloat` 得到 `NaN`（空串、纯空白、非数字文本）时
/// 比较为假，因此落入 fallback；负数（OpenRouter 的 router 模型用 `"-1"` 表示“价格取决
/// 于被选中的模型”）同样用 fallback。结果是 `round_cost`（等价于
/// `Number((x).toFixed(6))`）后的 6 位小数值。
fn per_million(value: Option<&Value>, fallback: f64) -> f64 {
    let per_token = match value {
        Some(Value::String(text)) => js_parse_float(text),
        Some(Value::Null) | None => js_parse_float(""),
        Some(other) => js_parse_float(&kiana_json::js_string::js_string(Some(other))),
    };
    if per_token >= 0.0 {
        round_cost(per_token * 1_000_000.0)
    } else {
        fallback
    }
}

/// `override.min_prompt_tokens !== undefined` 等：键存在即视为「已定义」（`null` 也算）。
///
/// 这里刻意不用 `Value::is_null` 过滤：TS 只检查 `!== undefined`，`null` 是已定义值，
/// 后续会按 JS `ToNumber` 参与阈值比较（见 `js_to_number`）。
fn defined(overrides: &serde_json::Map<String, Value>, key: &str) -> bool {
    overrides.contains_key(key)
}

/// JS `ToNumber(value)`（JSON 可达值）。
///
/// TS 只拒绝 `undefined`，`inputTokensAbove` 会原样保存 `null`/字符串/布尔值；后续
/// `inputTokens > inputTokensAbove` 按 JS 关系比较（两侧 `ToNumber`）。Rust 的
/// [`ModelCostTier::input_tokens_above`] 是 `f64`，无法表达这些 JSON 形状，因此直接用
/// `ToNumber` 的结果承载比较语义（`null` → 0，`true` → 1，`"1000"` → 1000）；只有把
/// tier 序列化成 JSON 时才会看到 0/1/1000 而不是原值。
///
/// 数组先按 `String(array)`（元素用逗号连接）再解析；对象恒为 `NaN`。这两种形状只会
/// 出现在畸形的上游数据里，正常价格配置不会遇到。
fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::parse_js_number(text),
        Value::Array(_) => {
            kiana_json::parse_js_number(&kiana_json::js_string::js_string(Some(value)))
        }
        Value::Object(_) => f64::NAN,
    }
}

/// Convert OpenRouter's $/token prices to kiana's $/1M tokens.
///
/// Missing, invalid and negative base prices are 0. Prompt-length overrides become
/// request-wide tiers; a rate a tier does not list keeps the base price. Time-of-day
/// overrides are skipped because ModelCost cannot express them.
///
/// # 结构映射
///
/// - `prompt` → `input`，`completion` → `output`，`input_cache_read` → `cache_read`，
///   `input_cache_write` → `cache_write`；都先乘 1e6 再按 6 位小数取整；
/// - 每个合格的 override 变成一个 [`ModelCostTier`]：`min_prompt_tokens` 经
///   `js_to_number` 变成 `input_tokens_above`，四个费率各自回落到 base 的同名字段；
/// - 没有任何合格 override 时 `tiers = None`（不是空数组）。
///
/// # override 的筛选
///
/// 只保留「`min_prompt_tokens` 键存在」且「`utc_start`/`utc_end`/`utc_days` 键都不
/// 存在」的条目：ModelCost 只有请求级分档，没有按一天中时段定价的能力，带 UTC 时间
/// 条件的条目只能跳过，而不是近似成全天档位。
///
/// 数组顺序被保留，但结论不受顺序影响：多个档位同时命中时由 `models::cost` 取阈值最高
/// 的那个（见 [`ModelCostTier`] 文档）。
///
/// # 容错
///
/// `overrides` 不是数组、条目不是对象等畸形形状会被跳过（TS 会抛 `TypeError`，见模块
/// 文档）；`pricing` 本身不是对象时视作全部缺失，返回四个 0 费率。
///
/// TS: `providers/openrouter-api.ts#openRouterCost`
pub fn open_router_cost(pricing: Option<&Value>) -> ModelCost {
    let pricing = pricing.and_then(Value::as_object);
    let base = ModelCost {
        input: per_million(pricing.and_then(|pricing| pricing.get("prompt")), 0.0),
        output: per_million(pricing.and_then(|pricing| pricing.get("completion")), 0.0),
        cache_read: per_million(
            pricing.and_then(|pricing| pricing.get("input_cache_read")),
            0.0,
        ),
        cache_write: per_million(
            pricing.and_then(|pricing| pricing.get("input_cache_write")),
            0.0,
        ),
        tiers: None,
    };
    let mut tiers: Vec<ModelCostTier> = Vec::new();
    // TS 的 `(pricing?.overrides ?? []).flatMap(...)` 在 `overrides` 不是数组、或条目是
    // `null` 时抛 TypeError。Rust 的返回类型是 `ModelCost`（没有错误通道），因此跳过这类
    // 形状；正常的上游数据（数组 + 对象条目）按正常语义解析，只有 type-invalid 的输入
    // 才走到这里。
    let overrides = pricing
        .and_then(|pricing| pricing.get("overrides"))
        .and_then(Value::as_array);
    for override_value in overrides.into_iter().flatten() {
        // 非对象条目（null/数字/数组）无法读取字段；TS 在这里会抛 TypeError，Rust 跳过。
        let Some(override_value) = override_value.as_object() else {
            continue;
        };
        // 只收“按 prompt 长度分档”的条目；任何 UTC 时间条件的键出现都意味着这是按时段
        // 定价，ModelCost 表达不了。
        if !defined(override_value, "min_prompt_tokens")
            || defined(override_value, "utc_start")
            || defined(override_value, "utc_end")
            || defined(override_value, "utc_days")
        {
            continue;
        }
        let input_tokens_above = override_value
            .get("min_prompt_tokens")
            .map(js_to_number)
            .unwrap_or(0.0);
        // 单个档位没列出的费率沿用 base 的同名值（不是 0）。
        tiers.push(ModelCostTier {
            input_tokens_above,
            input: per_million(override_value.get("prompt"), base.input),
            output: per_million(override_value.get("completion"), base.output),
            cache_read: per_million(override_value.get("input_cache_read"), base.cache_read),
            cache_write: per_million(override_value.get("input_cache_write"), base.cache_write),
        });
    }
    if tiers.is_empty() {
        base
    } else {
        ModelCost {
            tiers: Some(tiers),
            ..base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_per_token_prices_and_keeps_fallbacks() {
        let cost = open_router_cost(Some(&json!({
            "prompt": "0.000003",
            "completion": "0.000015",
            "input_cache_read": "-1",
            "input_cache_write": "not-a-number",
        })));
        assert_eq!(cost.input, 3.0);
        assert_eq!(cost.output, 15.0);
        assert_eq!(cost.cache_read, 0.0);
        assert_eq!(cost.cache_write, 0.0);
        assert!(cost.tiers.is_none());
    }

    #[test]
    fn turns_prompt_length_overrides_into_tiers() {
        let cost = open_router_cost(Some(&json!({
            "prompt": "0.000001",
            "completion": "0.000002",
            "overrides": [
                { "min_prompt_tokens": 200_000, "prompt": "0.000003" },
                { "min_prompt_tokens": 100, "utc_start": 0 },
                {},
            ],
        })));
        let tiers = cost.tiers.expect("tiers");
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].input_tokens_above, 200_000.0);
        assert_eq!(tiers[0].input, 3.0);
        // 未列出的费率沿用 base 价格。
        assert_eq!(tiers[0].output, 2.0);
        assert_eq!(tiers[0].cache_read, 0.0);
        assert_eq!(tiers[0].cache_write, 0.0);
    }

    /// TS 在 `overrides` 非数组、或条目为 `null` 时抛 TypeError（`flatMap`/属性访问）。
    /// Rust 的 `ModelCost` 没有错误通道，因此跳过这些形状（见函数注释）。
    #[test]
    fn tolerates_malformed_overrides_instead_of_throwing() {
        let cost = open_router_cost(Some(&json!({ "prompt": "0.000001", "overrides": 5 })));
        assert_eq!(cost.input, 1.0);
        assert!(cost.tiers.is_none());

        let cost = open_router_cost(Some(&json!({
            "overrides": [null, { "min_prompt_tokens": 10, "prompt": "0.000002" }, 7, []],
        })));
        let tiers = cost.tiers.expect("tiers");
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].input_tokens_above, 10.0);
        assert_eq!(tiers[0].input, 2.0);
    }

    /// TS 只拒绝 `undefined`：`null`/字符串仍生成 tier，后续 `inputTokens > inputTokensAbove`
    /// 按 JS 关系比较（两侧 `ToNumber`），因此 Rust 用 `ToNumber` 的结果承载阈值。
    #[test]
    fn uses_js_to_number_for_non_numeric_min_prompt_tokens() {
        let cost = open_router_cost(Some(&json!({
            "overrides": [
                { "min_prompt_tokens": null },
                { "min_prompt_tokens": "1000" },
            ],
        })));
        let tiers = cost.tiers.expect("tiers");
        assert_eq!(tiers[0].input_tokens_above, 0.0);
        assert_eq!(tiers[1].input_tokens_above, 1000.0);
    }
}
