//! 会话用量汇总与成本分解。
//!
//! TS: `packages/coding-agent/src/core/usage-totals.ts`。
//!
//! # 两个用途
//!
//! - 累计总量（[`crate::usage_totals::UsageTotals`]）：给 `getSessionStats` 与会话 footer
//!   用，字段是 `u64`/`f64`。
//! - 按模型分组的成本分解（[`crate::usage_totals::get_usage_cost_breakdown`]）：给
//!   `/session` 之类的命令展示“哪个模型/哪类摘要花了多少”，数据来源按分支而定：`Usage`
//!   条目带 `raw_entry` 时读原始 JSON，typed 视图可解析的 `Compaction`/`BranchSummary`/
//!   标准消息读 typed 字段，解析失败退化为 `Unknown` 或宽松解析成 `AgentMessage::Custom`
//!   的条目再回退到原始 JSON（详见下节）。
//!
//! # 为什么分解路径要绕开 typed `Usage`
//!
//! TS 的 usage 字段是 `number`：缺失字段参与算术会得到 `NaN`，因此一份残缺的 usage 仍会
//! 进入统计（例如 `cost` 有值而 token 缺失 → `tokens: NaN`，`JSON.stringify` 写成 `null`）。
//! Rust 的 `Usage` 把 token 字段建模为 `u64`，且没有为缺失字段提供默认值：残缺的 `usage`
//! 条目在解析时会失败并退化为 `SessionEntry::Unknown`；`message` 条目里的消息则通常落进
//! `AgentMessage::Custom`（`untagged` 反序列化的兜底）。为了对齐 TS 的统计口径，分解路径
//! 对这些残缺或宽松解析的条目用 `f64` 直接读原始 JSON 字段（内部类型 `UsageNumbers`）；
//! 其余路径改用 `UsageNumbers::from_usage` 从 typed `Usage` 换算：`raw_entry` 为 `None`
//! 的 `Usage` 条目（不经文件解析，例如运行期 append）、typed 的 `Compaction`/
//! `BranchSummary` usage，以及标准 assistant/toolResult 消息。两条来源最终汇总到同一个
//! `f64` 累加器。
//!
//! # 调用方
//!
//! - `kiana-session`：`getSessionStats`（[`crate::usage_totals::add_usage_to_totals`]）与
//!   嵌套调用用量合并（[`crate::usage_totals::combine_usage`]，见 `crate::nested_tool_calls`）。
//! - `kiana-interactive`：footer 的实时累计与 `/session` 的成本表。
//! - `kiana-compaction` 与 codemode 扩展：多次调用的 usage 合并。
//!
//! # 数值约定
//!
//! - token 累计用 `u64::saturating_add`（TS 的 `number` 没有这个上限；超出 `u64` 时 Rust
//!   饱和而不是回绕）。
//! - 成本是 `f64` 直接相加；分解路径允许 `NaN`，它会在过滤与排序中被特殊处理。
//!
//! # 阅读顺序
//!
//! 先看 [`crate::usage_totals::UsageTotals`] 与
//! [`crate::usage_totals::add_usage_to_totals`]（简单累计），再看
//! [`crate::usage_totals::combine_usage`]（可选字段的合并规则），最后看
//! [`crate::usage_totals::get_usage_cost_breakdown`]（原始 JSON 的分桶与排序）。

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{JsonValue, Message, Usage};

use crate::session_manager::{SessionEntry, js_to_string, js_truthy};

/// 累计用量。
///
/// 每个字段都是累计值（不是单次调用的增量）。`cost` 是各次 `usage.cost.total` 的和，
/// 不从四项费用重算。
/// TS: `usage-totals.ts#UsageTotals`
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    /// 输入 token。
    pub input: u64,
    /// 输出 token。
    pub output: u64,
    /// 缓存读取 token。
    pub cache_read: u64,
    /// 缓存写入 token。
    pub cache_write: u64,
    /// 累计美元成本。
    pub cost: f64,
}

use serde::{Deserialize, Serialize};

/// `createUsageTotals()`：全 0 的累计器。
///
/// TS: `usage-totals.ts#createUsageTotals`
pub fn create_usage_totals() -> UsageTotals {
    UsageTotals::default()
}

/// `addUsageToTotals(totals, usage)`：把一次调用的用量累加进 `totals`。
///
/// token 四项相加，成本只累加 `usage.cost.total`（明细四项不参与）。`u64` 用饱和加法，
/// 溢出时停在 `u64::MAX` 而不是回绕；成本是普通 `f64` 加法。
///
/// 没有返回值也不会失败：`usage` 由调用方保证是合法的 typed 视图。调用方按会话条目顺序
/// 逐条调用（见 `kiana-session` 的 `getSessionStats`）。
///
/// TS: `usage-totals.ts#addUsageToTotals`
pub fn add_usage_to_totals(totals: &mut UsageTotals, usage: &Usage) {
    totals.input = totals.input.saturating_add(usage.input);
    totals.output = totals.output.saturating_add(usage.output);
    totals.cache_read = totals.cache_read.saturating_add(usage.cache_read);
    totals.cache_write = totals.cache_write.saturating_add(usage.cache_write);
    totals.cost += usage.cost.total;
}

/// `combineUsage(first, second)`：保留任一侧报告的可选 token 拆分。
///
/// # 合并规则
///
/// - 四个基础 token 字段与 `total_tokens` 逐项相加（`saturating_add`），`total_tokens`
///   不按四项重算。
/// - `cache_write1h` / `reasoning` 是可选拆分：**两侧都是 `None` 时结果才是 `None`**；
///   否则把 `None` 当 0 相加。这与 TS 的“任一侧 `!== undefined` 才写出键”对应。
/// - `cost` 的五个数值逐项相加（`total` 相加，不重算）。
/// - `total_from_provider` 恒为 `Some(bool)`：只有两侧都是 `Some(true)` 才为 `true`；
///   `None` 视为 false（TS 的 `a === true && b === true`）。
///
/// 因此合并结果不是“重新计价”，而是两个已定价结果的算术和。
///
/// TS: `usage-totals.ts#combineUsage`
pub fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input.saturating_add(second.input),
        output: first.output.saturating_add(second.output),
        cache_read: first.cache_read.saturating_add(second.cache_read),
        cache_write: first.cache_write.saturating_add(second.cache_write),
        // 任一侧报告过 1h 拆分就输出和（缺省按 0）；两侧都缺省时保持 `None`，序列化省略该键。
        cache_write1h: match (first.cache_write1h, second.cache_write1h) {
            (None, None) => None,
            (first, second) => Some(first.unwrap_or(0).saturating_add(second.unwrap_or(0))),
        },
        reasoning: match (first.reasoning, second.reasoning) {
            (None, None) => None,
            (first, second) => Some(first.unwrap_or(0).saturating_add(second.unwrap_or(0))),
        },
        total_tokens: first.total_tokens.saturating_add(second.total_tokens),
        cost: kiana_ai::types::UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
            // 只有两侧都是 provider 报告的总额时，和才是 provider 报告。
            total_from_provider: Some(
                first.cost.total_from_provider == Some(true)
                    && second.cost.total_from_provider == Some(true),
            ),
        },
    }
}

/// 成本分解的一行。
///
/// - `key`：`"<provider>/<model>"`，或摘要类用量的固定桶 `"Tools/summaries"`；
/// - `cost`：该桶内 `usage.cost.total` 之和；
/// - `tokens`：该桶内 `input + output + cacheRead + cacheWrite` 之和（**不是**
///   `totalTokens` 之和，与 TS 的 `getUsageCostBreakdown` 一致）。
///
/// `tokens` 是 TS 的 `number`：畸形 usage（缺字段）会得到 `NaN`，因此用 `f64` 承载
/// （`JSON.stringify(NaN)` 与 serde 都写成 `null`）。
///
/// TS: `usage-totals.ts#UsageCostBreakdownEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageCostBreakdownEntry {
    /// 分组键（`provider/model` 或 `Tools/summaries`）。
    pub key: String,
    /// 美元成本。
    pub cost: f64,
    /// token 总数。
    pub tokens: f64,
}

/// `getUsageCostBreakdown` 的内部累加器（`UsageTotals` 的字段是 `u64`，无法承载 TS 的 `NaN`）。
///
/// 用 `f64` 保存每个字段，让缺失字段的 `NaN` 能传播到最终结果（与 TS 的算术一致），
/// 而不是像 typed `Usage` 那样丢弃条目。
#[derive(Debug, Clone, Copy, Default)]
struct BreakdownTotals {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    cost: f64,
}

impl BreakdownTotals {
    /// 该桶的 token 总数：四项相加（与 TS 的 `input+output+cacheRead+cacheWrite` 一致）。
    fn tokens(self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// 一份 usage 的数值视图（缺失字段为 `NaN`，与 TS 的 `undefined` 算术一致）。
///
/// 两个构造入口对应两种来源：typed [`Usage`]（运行期 append 的条目）与原始 JSON
/// （从文件加载的条目）；后者保留 `undefined` → `NaN` 的语义。
#[derive(Debug, Clone, Copy)]
struct UsageNumbers {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    cost_total: f64,
}

impl UsageNumbers {
    /// 从 typed `Usage` 转换（字段齐全，不会产生 `NaN`）。
    fn from_usage(usage: &Usage) -> Self {
        UsageNumbers {
            input: usage.input as f64,
            output: usage.output as f64,
            cache_read: usage.cache_read as f64,
            cache_write: usage.cache_write as f64,
            cost_total: usage.cost.total,
        }
    }

    /// 原始 `usage` 对象：缺失/非数字字段按 `undefined` → `NaN`（`usage.cost.total` 缺失在
    /// TS 里会抛 TypeError；Rust 无法表达抛错，用 `NaN` 承载）。
    fn from_json(value: &JsonValue) -> Self {
        let object = value.as_object();
        let number = |key: &str| {
            object
                .and_then(|object| object.get(key))
                .and_then(JsonValue::as_f64)
                .unwrap_or(f64::NAN)
        };
        let cost = object
            .and_then(|object| object.get("cost"))
            .and_then(JsonValue::as_object);
        UsageNumbers {
            input: number("input"),
            output: number("output"),
            cache_read: number("cacheRead"),
            cache_write: number("cacheWrite"),
            cost_total: cost
                .and_then(|cost| cost.get("total"))
                .and_then(JsonValue::as_f64)
                .unwrap_or(f64::NAN),
        }
    }
}

/// TS 的 `if (usage)` 真值判断（缺失/`null`/`false`/`0`/`""` 为假）。
fn truthy_usage(value: Option<&JsonValue>) -> Option<&JsonValue> {
    value.filter(|value| js_truthy(value))
}

/// `totals.input += usage.input; …; totals.cost += usage.cost.total`。
///
/// 与 TS 的 `number` 算术一致：任何一项是 `NaN`，累加后的字段仍是 `NaN`，不会中断扫描。
fn add_usage_numbers(totals: &mut BreakdownTotals, usage: &UsageNumbers) {
    totals.input += usage.input;
    totals.output += usage.output;
    totals.cache_read += usage.cache_read;
    totals.cache_write += usage.cache_write;
    totals.cost += usage.cost_total;
}

/// 按模型归组用量；其余用量归入单独的桶。
///
/// 与 TS 一样直接读取原始 usage 字段：缺失字段得到 `NaN`（`UsageCostBreakdownEntry.tokens`
/// 为 `f64`）。TS 在 `usage.cost` 缺失时抛 TypeError；Rust 的签名没有错误通道，用 `cost: NaN`
/// 承载（该桶只有在 token 为正时才会出现）。
///
/// # 分桶规则（与 TS 逐条对应，见 `spec/ca-config-models.md` §2.30.2）
///
/// - `message` 且 role 为 `assistant` → `{provider}/{responseModel ?? model}`；
/// - `message` 且 role 为 `toolResult` 且有 usage → `"Tools/summaries"`；
/// - `usage` 条目 → `{provider}/{model}`：从文件加载时读原始对象（缺失字段按 JS 模板插值为
///   `"undefined"`），运行期 append 的条目用 typed 字段；
/// - `branch_summary` / `compaction` 且有 usage → `"Tools/summaries"`；
/// - typed 解析失败、退化为 `SessionEntry::Unknown` 的条目按原始 `type` 走同样规则；
/// - 拿不到 key 或 usage 的条目跳过。
///
/// # 累加与输出
///
/// - 同一 key 的数值累加，`IndexMap` 保持 key 首次出现的顺序。
/// - 过滤掉不满足 `cost > 0 || tokens > 0` 的桶；`NaN` 与任何数比较都是 false，因此一项为
///   `NaN` 时只有另一项为正才能保留。
/// - 按 cost 降序排序，且是**稳定排序**：cost 相同或差值为 `NaN` 时保持首次出现顺序，
///   对应 V8 `Array.prototype.sort` 的稳定语义。
///
/// # 输入
///
/// `entries` 通常是会话的全部条目；函数只读，不修改输入，也不会因未知类型提前返回。
///
/// TS: `usage-totals.ts#getUsageCostBreakdown`
pub fn get_usage_cost_breakdown(entries: &[SessionEntry]) -> Vec<UsageCostBreakdownEntry> {
    let mut totals_by_key: IndexMap<String, BreakdownTotals> = IndexMap::new();

    // 第 1 步：逐条确定 (key, usage)，两个都拿不到就跳过；类型化视图无法承载的数值在
    // `UsageNumbers::from_json` 里转成 NaN。
    for entry in entries {
        let mut key: Option<String> = None;
        let mut usage: Option<UsageNumbers> = None;
        match entry {
            SessionEntry::Message(message_entry) => {
                let message = &message_entry.message;
                // TS 读取 `entry.message.role`；宽松解析成 `AgentMessage::Custom` 的
                // assistant 消息同样计入（字段从原始对象读取）。
                if message.role() == "assistant" {
                    key = Some(assistant_breakdown_key(message));
                    usage = assistant_usage(message);
                } else if message.role() == "toolResult" {
                    usage = tool_result_usage(message);
                    if usage.is_some() {
                        key = Some("Tools/summaries".to_string());
                    }
                }
            }
            SessionEntry::Usage(usage_entry) => match &usage_entry.raw_entry {
                // TS 读取 `JSON.parse` 得到的原始对象：`${entry.provider}/${entry.model}`
                // 里缺省字段插值为 `"undefined"`，`entry.usage` 按真值判断。
                Some(raw) => {
                    key = Some(format!(
                        "{}/{}",
                        js_to_string(raw.get("provider")),
                        js_to_string(raw.get("model"))
                    ));
                    usage = truthy_usage(raw.get("usage")).map(UsageNumbers::from_json);
                }
                None => {
                    key = Some(format!("{}/{}", usage_entry.provider, usage_entry.model));
                    usage = Some(UsageNumbers::from_usage(&usage_entry.usage));
                }
            },
            SessionEntry::BranchSummary(branch_summary) => {
                usage = branch_summary.usage.as_ref().map(UsageNumbers::from_usage);
                if usage.is_some() {
                    key = Some("Tools/summaries".to_string());
                }
            }
            SessionEntry::Compaction(compaction) => {
                usage = compaction.usage.as_ref().map(UsageNumbers::from_usage);
                if usage.is_some() {
                    key = Some("Tools/summaries".to_string());
                }
            }
            // 类型化视图无法解析（如 usage 字段残缺）的条目仍按 TS 的条目类型参与统计：
            // `usage` / `branch_summary` / `compaction` / `message` 直接读原始字段。
            SessionEntry::Unknown(object) => match entry.kind() {
                "usage" => {
                    usage = truthy_usage(object.get("usage")).map(UsageNumbers::from_json);
                    if usage.is_some() {
                        key = Some(format!(
                            "{}/{}",
                            js_to_string(object.get("provider")),
                            js_to_string(object.get("model"))
                        ));
                    }
                }
                "branch_summary" | "compaction" => {
                    usage = truthy_usage(object.get("usage")).map(UsageNumbers::from_json);
                    if usage.is_some() {
                        key = Some("Tools/summaries".to_string());
                    }
                }
                // TS 直接读 `entry.message.role` / `entry.message.usage`；message 为 null/缺失
                // 时抛 TypeError（Rust 的签名没有错误通道，按无贡献处理）。
                "message" => {
                    let Some(message) = object.get("message").and_then(JsonValue::as_object) else {
                        continue;
                    };
                    match message.get("role").and_then(JsonValue::as_str) {
                        Some("assistant") => {
                            key = Some(format!(
                                "{}/{}",
                                js_to_string(message.get("provider")),
                                js_to_string(raw_response_model(message))
                            ));
                            usage = truthy_usage(message.get("usage")).map(UsageNumbers::from_json);
                        }
                        Some("toolResult") => {
                            usage = truthy_usage(message.get("usage")).map(UsageNumbers::from_json);
                            if usage.is_some() {
                                key = Some("Tools/summaries".to_string());
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            _ => {}
        }
        // 两个都齐了才计入：没有 usage 的条目不会创建空桶。
        let (Some(key), Some(usage)) = (key, usage) else {
            continue;
        };
        add_usage_numbers(totals_by_key.entry(key).or_default(), &usage);
    }

    // 第 2 步：物化成条目并丢掉“成本与 token 都为 0”的空桶。
    let mut breakdown: Vec<UsageCostBreakdownEntry> = totals_by_key
        .into_iter()
        .map(|(key, totals)| UsageCostBreakdownEntry {
            key,
            cost: totals.cost,
            tokens: totals.tokens(),
        })
        .filter(|entry| entry.cost > 0.0 || entry.tokens > 0.0)
        .collect();
    // 第 3 步：按成本降序。`sort_by` 是稳定排序，同额时保持插入顺序（V8 sort 的语义）。
    breakdown.sort_by(|a, b| {
        let delta = b.cost - a.cost;
        // 任一侧是 NaN 时差值也是 NaN，视为相等，交给稳定排序保持原顺序。
        if delta.is_nan() || delta == 0.0 {
            std::cmp::Ordering::Equal
        } else if delta < 0.0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    breakdown
}

/// `message.responseModel ?? message.model`（原始对象版本）。
fn raw_response_model(message: &serde_json::Map<String, JsonValue>) -> Option<&JsonValue> {
    match message.get("responseModel") {
        None | Some(JsonValue::Null) => message.get("model"),
        other => other,
    }
}

/// assistant 消息的分组键：`{provider}/{responseModel ?? model}`。
///
/// TS 直接读取原始消息对象；`AgentMessage::Custom`（宽松解析）的字段从 JSON 读取。
fn assistant_breakdown_key(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            let model = assistant
                .response_model
                .as_deref()
                .unwrap_or(&assistant.model);
            format!("{}/{}", assistant.provider, model)
        }
        AgentMessage::Custom(object) => {
            // `${provider}/${responseModel ?? model}`（缺失字段按 JS 插值为 "undefined"）。
            let model = match object.get("responseModel") {
                None | Some(JsonValue::Null) => object.get("model"),
                other => other,
            };
            format!(
                "{}/{}",
                js_to_string(object.get("provider")),
                js_to_string(model)
            )
        }
        _ => String::new(),
    }
}

/// assistant 消息的 usage（Custom 视图从原始 `usage` 字段读取；`undefined`/假值 → `None`）。
fn assistant_usage(message: &AgentMessage) -> Option<UsageNumbers> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            Some(UsageNumbers::from_usage(&assistant.usage))
        }
        AgentMessage::Custom(object) => {
            truthy_usage(object.get("usage")).map(UsageNumbers::from_json)
        }
        _ => None,
    }
}

/// toolResult 消息的 usage（Custom 视图从原始 `usage` 字段读取；`undefined`/假值 → `None`）。
fn tool_result_usage(message: &AgentMessage) -> Option<UsageNumbers> {
    match message {
        AgentMessage::Standard(Message::ToolResult(tool_result)) => {
            tool_result.usage.as_ref().map(UsageNumbers::from_usage)
        }
        AgentMessage::Custom(object) => {
            truthy_usage(object.get("usage")).map(UsageNumbers::from_json)
        }
        _ => None,
    }
}
