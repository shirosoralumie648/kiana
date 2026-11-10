//! Prompt cache 浪费统计。
//!
//! TS: `packages/coding-agent/src/core/cache-stats.ts`。
//!
//! Rust 侧与 TS 的一处必要差异：TS 的 `collectCacheMisses` 以 assistant 消息**对象引用**
//! 为键（`Map<AssistantMessage, CacheMiss>`）；Rust 无法按引用跨 `SessionEntry` 切片建键，
//! 因此返回 `entries` 的下标（`(usize, CacheMiss)`），顺序与扫描顺序一致。
//!
//! # 这个模块解决什么问题
//!
//! 会话里的 assistant 消息只带本轮 `usage`（`input` / `cacheRead` / `cacheWrite` 及各自
//! 成本）。单看一条 usage 无法回答“这次请求本来能命中缓存、却按原价重新计费了多少”，
//! 因为判断需要**上一轮请求的 prompt 规模**作为上界。本模块按条目顺序重建这条基线，逐轮
//! 比较：上一轮 prompt 的 token 数与本轮实际从缓存读到的 token 数之差，就是一次 cache
//! miss；再按“实际付费单价 − 缓存读取单价”折算成多付的美元。
//!
//! 本仓库中，输出只用于展示与统计（`/session` 汇总、交互式界面的 cache miss 提示）；
//! 模块自身不写回会话、不改动 `entries`，也不影响后续请求的内容。
//!
//! # 扫描模型（一次线性遍历）
//!
//! 内部函数 `scan` 顺序遍历调用方给出的条目切片，维护一个可选的“上一轮请求”状态：
//!
//! 1. `compaction` / `branch_summary`：上下文合法变化，清空基线——下一轮 prompt 是新
//!    内容，不应算作重复计费；
//! 2. `usage` 且 `kind == "cache_warm"`：视为一次缓存刷新，重建基线（要求 prompt token
//!    数 > 0）；其它 `kind` 的 usage 条目（如 `custom_operation`）**不**参与；
//! 3. `message` 且 role 为 `assistant`：先与基线比较并累计 miss，再用**本轮** assistant
//!    的 usage 更新基线，所以连续多轮 miss 是相对紧邻的上一条 assistant 计算的；
//! 4. 其它条目（`model_change`、`label`、`custom`、非 assistant 的 `message` 等）不改动
//!    基线。模型切换**不**重置基线；若切换后这一轮满足 miss 条件，`CacheMiss` 会带
//!    `model_changed = true` 标记——切换本身不额外产生 miss。
//!
//! # 重要约束
//!
//! - 数值语义是 JS 的：`Math.min` / `Math.max` 在任一操作数为 `NaN` 时返回 `NaN`。
//!   `AgentMessage::Custom`（宽松解析）里的畸形 usage 会产生 `NaN` 的 miss 数值并照常
//!   计数，而不是被丢弃；调用方不要把 `NaN` 当异常处理（见 `tests/cache_stats.rs` 的
//!   `cache_miss_math_propagates_nan_like_javascript`）。
//! - 条目顺序完全由调用方决定：本模块不排序、不去重、不沿 `parentId` 回溯。调用方应传入
//!   它想统计的会话顺序。
//! - 字段类型畸形、无法解析成类型化条目的 `usage` / `compaction` / `branch_summary`
//!   （[`crate::session_manager::SessionEntry::Unknown`]）仍按原始 `type` / `kind` 字段
//!   参与扫描，与 TS 只读 `entry.type` 的行为一致。
//! - `ModelPriceSource` 的返回值单位是“美元 / 百万 token”，`detect_miss` 使用前会除以
//!   `1_000_000`。
//!
//! # 阅读顺序
//!
//! `CACHE_TTL_MS` 与两个数据结构 → `detect_miss`（单轮判定）→ `scan`（跨轮状态）→ 三个
//! 公开入口 `compute_cache_waste` / `collect_cache_misses` / `detect_cache_miss`。

use kiana_agent_core::AgentMessage;
use kiana_ai::types::{AssistantMessage, JsonValue, Message, StopReason, Usage};
use serde::{Deserialize, Serialize};

use crate::session_manager::{SessionEntry, js_to_string};

/// Prompt cache TTL：超过该空闲间隔的 miss 值得提示（Anthropic 默认 5 分钟）。
///
/// 本模块的计算不使用该常量；调用方把 `CacheMiss::idle_ms` 与它比较来决定提示文案
/// （例如交互式界面的 “Cache miss after Nm idle”）。
///
/// TS: `cache-stats.ts#CACHE_TTL_MS`
pub const CACHE_TTL_MS: f64 = 5.0 * 60.0 * 1000.0;

/// 每轮 miss 低于等于该值（token）时视为缓存断点粒度噪声，不产生 [`CacheMiss`]。
const NOISE_FLOOR_TOKENS: f64 = 1024.0;

/// 单条 assistant 消息上的一次 cache miss。
///
/// 数值字段可能为 `NaN`：本模块用 `f64` 承载宽松解析出的 usage/时间戳，JS 的算术会让
/// `NaN` 一路传播到这里（见模块文档的“重要约束”）。
///
/// TS: `cache-stats.ts#CacheMiss`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheMiss {
    /// 上一轮 prompt 中本应从缓存读取、但被重新计费的 token 数。
    ///
    /// 计算式为 `min(上一轮 prompt tokens, 本轮 prompt tokens) - 本轮 cacheRead`；
    /// 结果 `<= 1024` 时不构成 miss（不会构造出本结构体）。
    pub missed_tokens: f64,
    /// 相比全命中多付的美元；成本字段缺失或类型不对时按 `NaN` 传播（不会当成 0），
    /// 差价为负时被夹到 0（`js_max(0, …)`）。
    ///
    /// 计算式为 `missed_tokens * max(0, 实际付费单价 - 缓存读取单价)`：
    /// 付费单价取 `(cost.input + cost.cacheWrite) / (input + cacheWrite)`；读单价在本轮有
    /// `cacheRead` 时取 `cost.cacheRead / cacheRead`，否则取 `ModelPriceSource` 的目录价。
    /// 目录价缺失时读单价按 0 代入，此时结果等于按实际付费单价全额计，并不被强制为 0。
    pub missed_cost: f64,
    /// 距上次请求（上次刷新缓存）的毫秒数。
    ///
    /// 计算式为 `max(0, 本轮 timestamp - 上一轮 timestamp)`；任一侧时间戳不可解析时为
    /// `NaN`。
    pub idle_ms: f64,
    /// 相对上次请求是否切换了模型。
    ///
    /// 比较的是 `provider/model` 拼接串。宽松解析时缺失的 provider/model 会按
    /// `undefined` 参与拼接，因此与空串构成的 key 不相等，会报告 `true`（与 TS 的模板
    /// 插值行为一致）。
    pub model_changed: bool,
}

/// 累计 cache 浪费。
///
/// 三个字段都是逐轮累加；任一轮产生 `NaN`，对应的累计字段也会变成 `NaN`（不做有限性
/// 过滤，保持与 JS 一致）。
///
/// TS: `cache-stats.ts#CacheWasteTotals`
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheWasteTotals {
    /// 累计 miss token。
    pub missed_tokens: f64,
    /// 累计多付成本。
    pub missed_cost: f64,
    /// 计数到的 miss 次数（高于噪声阈值的轮次）。
    ///
    /// `NaN` 的 miss 同样计数：判定条件是 `missed_tokens <= 1024`，与 `NaN` 比较恒为
    /// false。
    pub miss_count: u64,
}

/// 最小定价查询（`ModelRuntime` 可实现）。
///
/// 返回 `None` 表示目录中没有该模型，`detect_miss` 会把缓存读取单价按 0 处理。
///
/// TS: `cache-stats.ts#ModelPriceSource`（`getModel(provider, modelId)` 返回 `{cost:{cacheRead}}`）。
pub trait ModelPriceSource: Send + Sync {
    /// 返回模型的 cache-read 单价（美元 / 百万 token）。
    fn get_model(&self, provider: &str, model_id: &str) -> Option<ModelPriceInfo>;
}

/// 定价查询结果（只用到 `cost.cacheRead`）。
///
/// TS: `cache-stats.ts#ModelPriceSource` 的返回值。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModelPriceInfo {
    /// 每百万 token 的 cache-read 价格（美元）。
    pub cache_read: f64,
}

/// 上一次请求（其 prompt 应全部命中缓存）。
///
/// 这是跨条目滚动的扫描状态：由 `cache_warm` usage 条目或上一条 assistant 消息产生，
/// 遇到 compaction / branch_summary 时被清空。
#[derive(Debug, Clone)]
struct PreviousRequest {
    /// 该轮 prompt 的总 token 数（input + cacheRead + cacheWrite），用作下一轮 miss 的上界。
    prompt_tokens: f64,
    /// `${provider}/${model}`，用于判断是否切换模型。
    model_key: String,
    /// Unix 毫秒（可能为 `NaN`）。
    timestamp: f64,
    /// 粘性：本段扫描中更早的请求报告过缓存活动。
    ///
    /// 一旦为 true，后续即使某轮 cacheRead/cacheWrite 都是 0，也仍然认为该 provider 会
    /// 报告缓存活动，从而允许计 miss（见 `detect_miss` 提前返回条件中的第 3 条）。
    reported_cache: bool,
}

/// assistant 消息的 usage 数值视图。
///
/// TS 直接对 `entry.message.usage` 的数字做算术；`AgentMessage::Custom`（宽松解析）的
/// 字段从原始对象读取，缺失字段按 `undefined` 参与运算（结果为 NaN）。
#[derive(Debug, Clone, Copy)]
struct UsageNumbers {
    input: f64,
    cache_read: f64,
    cache_write: f64,
    cost_input: f64,
    cost_cache_read: f64,
    cost_cache_write: f64,
}

impl UsageNumbers {
    fn from_usage(usage: &Usage) -> Self {
        UsageNumbers {
            input: usage.input as f64,
            cache_read: usage.cache_read as f64,
            cache_write: usage.cache_write as f64,
            cost_input: usage.cost.input,
            cost_cache_read: usage.cost.cache_read,
            cost_cache_write: usage.cost.cache_write,
        }
    }

    fn prompt_tokens(&self) -> f64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// `usage.input + usage.cacheRead + usage.cacheWrite`（typed usage）。
fn usage_prompt_tokens(usage: &Usage) -> f64 {
    usage.input as f64 + usage.cache_read as f64 + usage.cache_write as f64
}

/// `entry.message.role === "assistant"` 时的字段视图（Standard 与 Custom 统一）。
struct AssistantView {
    /// 宽松对象里缺失/非字符串的 provider 经 `js_to_string` 变成 `"undefined"` /
    /// `"[object Object]"` 等字符串（缺失不会变成空串），与 TS 模板插值一致。
    provider: String,
    /// 同 `provider`。
    model: String,
    /// Unix 毫秒；缺失或非数字时为 `NaN`。
    timestamp: f64,
    usage: UsageNumbers,
}

/// 读取宽松 usage 对象里的数值字段。
///
/// 缺失、JSON `null` 或非数字都返回 `NaN`：本实现只接受 JSON number，其余一律按 `NaN`。
/// 这样畸形数据会以 `NaN` 一路传播并显性暴露，而不是被 `null` 当 0、字符串被迫转换后
/// 悄悄污染统计。不要把它当异常处理，见模块文档。
fn usage_number(usage: Option<&serde_json::Map<String, JsonValue>>, key: &str) -> f64 {
    usage
        .and_then(|usage| usage.get(key))
        .and_then(JsonValue::as_f64)
        .unwrap_or(f64::NAN)
}

/// 把一条 [`AgentMessage`] 投影成 assistant 字段视图；非 assistant 返回 `None`。
///
/// TS 直接读 `entry.message.usage` 的字段，不校验消息形状，所以 `AgentMessage::Custom`
/// （无法解析成标准 `Message::Assistant` 的宽松消息，例如 `content: null`）同样要参与
/// 统计。这里刻意不检查消息类型，只检查 `role` 字符串。
fn assistant_view(message: &AgentMessage) -> Option<AssistantView> {
    if message.role() != "assistant" {
        return None;
    }
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => Some(AssistantView {
            provider: assistant.provider.clone(),
            model: assistant.model.clone(),
            timestamp: assistant.timestamp as f64,
            usage: UsageNumbers::from_usage(&assistant.usage),
        }),
        AgentMessage::Custom(object) => {
            let usage = object.get("usage").and_then(JsonValue::as_object);
            let cost = usage
                .and_then(|usage| usage.get("cost"))
                .and_then(JsonValue::as_object);
            Some(AssistantView {
                provider: js_to_string(object.get("provider")),
                model: js_to_string(object.get("model")),
                timestamp: object
                    .get("timestamp")
                    .and_then(JsonValue::as_f64)
                    .unwrap_or(f64::NAN),
                usage: UsageNumbers {
                    input: usage_number(usage, "input"),
                    cache_read: usage_number(usage, "cacheRead"),
                    cache_write: usage_number(usage, "cacheWrite"),
                    cost_input: cost
                        .and_then(|cost| cost.get("input"))
                        .and_then(JsonValue::as_f64)
                        .unwrap_or(f64::NAN),
                    cost_cache_read: cost
                        .and_then(|cost| cost.get("cacheRead"))
                        .and_then(JsonValue::as_f64)
                        .unwrap_or(f64::NAN),
                    cost_cache_write: cost
                        .and_then(|cost| cost.get("cacheWrite"))
                        .and_then(JsonValue::as_f64)
                        .unwrap_or(f64::NAN),
                },
            })
        }
        _ => None,
    }
}

/// `Math.min(a, b)`：任一操作数为 `NaN` 时结果为 `NaN`（Rust 的 `f64::min` 会忽略 `NaN`）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// `Math.max(a, b)`：任一操作数为 `NaN` 时结果为 `NaN`（Rust 的 `f64::max` 会忽略 `NaN`）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// `detectMiss(prev, message, models)`：把一条 assistant 的 usage 与上一轮基线比较。
///
/// 返回 `None` 的情况（任一条件成立即提前返回；顺序固定，先排除无基线/无 token 的情形，
/// 再判断缓存活动与噪声阈值）：
///
/// 1. 没有上一轮基线（会话第一轮，或刚被 compaction / branch_summary 清空）；
/// 2. 本轮 prompt token 数 `<= 0`；
/// 3. 本轮 `cacheRead + cacheWrite == 0` 且基线从未报告过缓存活动（provider 不上报缓存
///    用量时不应误报）；
/// 4. `missed_tokens <= 1024`（噪声）。
///
/// 注意 `NaN` 不会命中上述数值比较（`NaN <= 0`、`NaN == 0`、`NaN <= 1024` 都是 false），
/// 因此畸形 usage 会一路走到末尾，产生 `NaN` 的 `CacheMiss`——这是有意保留的 JS 语义。
///
/// `models` 只在“本轮 cacheRead 为 0”时被查询：此时没有实测的读单价，需要目录价来估算
/// 本应支付的缓存读取成本。
fn detect_miss(
    prev: Option<&PreviousRequest>,
    view: &AssistantView,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let usage = &view.usage;
    let prompt_tokens = usage.prompt_tokens();
    let previously_reported = prev.is_some_and(|prev| prev.reported_cache);
    let prev = prev?;
    if prompt_tokens <= 0.0 || (usage.cache_read + usage.cache_write == 0.0 && !previously_reported)
    {
        return None;
    }

    // 上一轮 prompt 与本轮 prompt 的重叠部分本应全部命中缓存；扣掉真正从缓存读到的
    // cacheRead，剩下的就是被重新计费的 token。取 min 表示只比较两轮 prompt 的重叠长度：
    // 本轮比上一轮短时，上一轮多出的部分不算本轮 miss。
    let missed_tokens = js_min(prev.prompt_tokens, prompt_tokens) - usage.cache_read;
    if missed_tokens <= NOISE_FLOOR_TOKENS {
        return None;
    }

    // 额外成本 = miss token 按实际支付费率（input/cacheWrite，含写入溢价）计费，
    // 而不是 cache-read 费率。miss token 只会落在 input 或 cacheWrite 桶里。
    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0.0 {
        (usage.cost_input + usage.cost_cache_write) / paid_tokens
    } else {
        0.0
    };
    // 缓存读取的真实单价优先用本轮 usage 自带的成本反推（provider 实际计费）；本轮没有
    // 读缓存时，用目录价（美元/百万 token）折算成每 token 价格。两处都拿不到就按 0 计：
    // 这会把差值放大（高估浪费），而不是把结果归零。
    let read_per_token = if usage.cache_read > 0.0 {
        usage.cost_cache_read / usage.cache_read
    } else {
        models
            .get_model(&view.provider, &view.model)
            .map(|info| info.cache_read)
            .unwrap_or(0.0)
            / 1_000_000.0
    };

    Some(CacheMiss {
        missed_tokens,
        // 差价可能为负（例如实际读单价比付费单价还高），JS 的 Math.max 把它夹到 0；
        // 任意一侧为 NaN 时结果为 NaN（js_max 的语义）。
        missed_cost: missed_tokens * js_max(0.0, paid_per_token - read_per_token),
        // 时间戳来自两条不同的条目，时钟回拨时夹到 0；NaN 继续传播。
        idle_ms: js_max(0.0, view.timestamp - prev.timestamp),
        model_changed: format!("{}/{}", view.provider, view.model) != prev.model_key,
    })
}

/// `asPreviousRequest(message, reportedCache)`：用一条 assistant 消息生成下一轮的基线。
///
/// `reported_cache` 是旧基线的粘性标志，与“本轮 cacheRead/cacheWrite > 0”做或运算后继续
/// 传递。prompt token 数 `<= 0` 时返回 `None`，调用方会保留旧基线不变；`NaN` 不满足该
/// 比较，会生成一个带 `NaN` 的基线，让后续轮次继续得到 `NaN`（JS 语义）。
fn as_previous_request(view: &AssistantView, reported_cache: bool) -> Option<PreviousRequest> {
    let usage = &view.usage;
    let prompt_tokens = usage.prompt_tokens();
    if prompt_tokens <= 0.0 {
        return None;
    }
    Some(PreviousRequest {
        prompt_tokens,
        model_key: format!("{}/{}", view.provider, view.model),
        timestamp: view.timestamp,
        reported_cache: reported_cache || usage.cache_read + usage.cache_write > 0.0,
    })
}

/// 扫描结果。
struct ScanResult {
    /// 扫描结束时的基线（供 `detect_cache_miss` 继续判断“下一条”消息）。
    prev: Option<PreviousRequest>,
    /// 累计浪费。
    totals: CacheWasteTotals,
    /// `(entries 下标, miss)`，按扫描顺序递增。
    misses: Vec<(usize, CacheMiss)>,
}

/// `scan(entries, models)`：单次遍历同时产出累计值、miss 列表与末尾基线。
///
/// 这是三个公开入口的共同实现：`compute_cache_waste` / `collect_cache_misses` 丢弃不需要
/// 的部分，`detect_cache_miss` 取 `prev` 再对新消息判定一次。扫描不修改 `entries`，也没有
/// 内部缓存；每次调用都是完整遍历。
fn scan(entries: &[SessionEntry], models: &dyn ModelPriceSource) -> ScanResult {
    let mut prev: Option<PreviousRequest> = None;
    let mut totals = CacheWasteTotals::default();
    let mut misses: Vec<(usize, CacheMiss)> = Vec::new();

    // 逐条推进：基线只被 reset / cache_warm / assistant 三类条目改变，其余条目相当于
    // 透明（TS 的 if/else 链同样如此）。
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            // 上下文合法地变化了；下一轮 prompt 是新内容，不应重复计费。
            // 模型切换**不**豁免：基线保留，若本轮满足 miss 条件会以 `model_changed` 标记计入。
            SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_) => {
                prev = None;
            }
            // TS 只看 `entry.type`：字段类型畸形（导致条目退化为 [`SessionEntry::Unknown`]）
            // 的 compaction/branch_summary 同样重置扫描。
            SessionEntry::Unknown(_) if matches!(entry.kind(), "compaction" | "branch_summary") => {
                prev = None;
            }
            SessionEntry::Usage(usage_entry) if usage_entry.kind_name == "cache_warm" => {
                // 从文件加载的条目按 TS 读取原始字段：`entry.usage.input` 等未校验数值，
                // `${entry.provider}/${entry.model}` 里缺省字段插值为 `"undefined"`。
                let (prompt_tokens, model_key, timestamp) = match &usage_entry.raw_entry {
                    Some(raw) => {
                        let usage = raw.get("usage");
                        let number = |key: &str| {
                            usage
                                .and_then(|usage| usage.get(key))
                                .and_then(JsonValue::as_f64)
                                .unwrap_or(f64::NAN)
                        };
                        let timestamp = match raw.get("timestamp") {
                            Some(value) => {
                                kiana_ai::model_catalog::js_date_parse(&js_to_string(Some(value)))
                                    .unwrap_or(f64::NAN)
                            }
                            None => f64::NAN,
                        };
                        (
                            number("input") + number("cacheRead") + number("cacheWrite"),
                            format!(
                                "{}/{}",
                                js_to_string(raw.get("provider")),
                                js_to_string(raw.get("model"))
                            ),
                            timestamp,
                        )
                    }
                    None => {
                        let usage = &usage_entry.usage;
                        (
                            usage_prompt_tokens(usage),
                            format!("{}/{}", usage_entry.provider, usage_entry.model),
                            kiana_ai::model_catalog::js_date_parse(&usage_entry.timestamp)
                                .unwrap_or(f64::NAN),
                        )
                    }
                };
                // prompt 为 0 或解析失败（NaN）时不建立基线：`NaN > 0.0` 为 false，
                // 与 TS 的 `if (promptTokens > 0)` 一致。
                if prompt_tokens > 0.0 {
                    prev = Some(PreviousRequest {
                        prompt_tokens,
                        model_key,
                        timestamp,
                        reported_cache: true,
                    });
                }
            }
            // usage 对象缺字段/类型不匹配时 typed 解析失败，但 TS 直接读 `entry.usage` 的原始
            // 字段（`input`/`cacheRead`/`cacheWrite`），部分 usage 仍会建立基线。
            SessionEntry::Unknown(object) if entry.kind() == "usage" => {
                // 与上面的类型化分支等价，只是所有字段（含 provider/model）都从原始对象
                // 读取；缺失字段按 `undefined` → `js_to_string` / `NaN` 参与。
                if object.get("kind").and_then(JsonValue::as_str) == Some("cache_warm") {
                    let usage = object.get("usage");
                    let number = |key: &str| {
                        usage
                            .and_then(|usage| usage.get(key))
                            .and_then(JsonValue::as_f64)
                            .unwrap_or(f64::NAN)
                    };
                    let prompt_tokens =
                        number("input") + number("cacheRead") + number("cacheWrite");
                    if prompt_tokens > 0.0 {
                        let timestamp = match object.get("timestamp") {
                            Some(value) => {
                                kiana_ai::model_catalog::js_date_parse(&js_to_string(Some(value)))
                                    .unwrap_or(f64::NAN)
                            }
                            None => f64::NAN,
                        };
                        prev = Some(PreviousRequest {
                            prompt_tokens,
                            model_key: format!(
                                "{}/{}",
                                js_to_string(object.get("provider")),
                                js_to_string(object.get("model"))
                            ),
                            timestamp,
                            reported_cache: true,
                        });
                    }
                }
            }
            SessionEntry::Message(message_entry) => {
                // TS 判断 `entry.message.role === "assistant"`；Custom 视图同样参与。
                let Some(view) = assistant_view(&message_entry.message) else {
                    continue;
                };
                if let Some(miss) = detect_miss(prev.as_ref(), &view, models) {
                    // 累加顺序与扫描顺序一致（浮点加法不满足结合律，顺序会影响末位）。
                    // `index` 是条目在传入切片中的位置，调用方据此映射回条目。
                    totals.missed_tokens += miss.missed_tokens;
                    totals.missed_cost += miss.missed_cost;
                    totals.miss_count += 1;
                    misses.push((index, miss));
                }
                // 用本轮 assistant 更新基线；prompt `<= 0` 时 `as_previous_request` 返回
                // None，`or(prev)` 保留旧基线继续扫描。`NaN` 会作为新基线继续传播。
                let reported_cache = prev.as_ref().is_some_and(|prev| prev.reported_cache);
                prev = as_previous_request(&view, reported_cache).or(prev);
            }
            _ => {}
        }
    }
    ScanResult {
        prev,
        totals,
        misses,
    }
}

/// 会话累计 cache 浪费。
///
/// 对 `entries` 做一次完整扫描后返回累计值；不修改、不排序 `entries`，也不缓存结果。
/// 输入顺序决定结果（见模块文档），畸形输入可能得到 `NaN` 而不是 `0`。这是 `/session`
/// 汇总使用的入口。
///
/// TS: `cache-stats.ts#computeCacheWaste`
pub fn compute_cache_waste(
    entries: &[SessionEntry],
    models: &dyn ModelPriceSource,
) -> CacheWasteTotals {
    scan(entries, models).totals
}

/// 所有计数到的 cache miss（按 `entries` 下标映射到 assistant 消息）。
///
/// TS 按消息对象引用建键；Rust 返回 `(entries 下标, miss)`。下标是传入切片中的位置，按
/// 扫描顺序递增，调用方用同一个切片按下标取条目即可（交互式界面据此把 miss 映射回条目
/// id）。过滤条件与 [`crate::cache_stats::compute_cache_waste`] 相同，因此两者统计的轮次
/// 一致；同一条 assistant 消息最多出现一次。
///
/// TS: `cache-stats.ts#collectCacheMisses`
pub fn collect_cache_misses(
    entries: &[SessionEntry],
    models: &dyn ModelPriceSource,
) -> Vec<(usize, CacheMiss)> {
    scan(entries, models).misses
}

/// 检测刚完成的 assistant 消息上的 cache miss（`entries` 尚不包含该消息）。
///
/// 先对 `entries` 完整扫描一次建立基线，再对 `message` 单独判定。会话第一轮（没有基线）、
/// 健康命中、prompt token 数 `<= 0`、本轮没有缓存活动且基线从未报告过、miss 不高于噪声
/// 阈值时返回 `None`。调用时序由调用方保证：应在该消息写入会话之前调用（TS 侧在
/// `message_end` 时调用，早于持久化）。若消息已在 `entries` 中，扫描会先把它自己写成
/// 基线，随后 `detect_miss` 拿它与自身比较，结果不可信。
///
/// TS: `cache-stats.ts#detectCacheMiss`
pub fn detect_cache_miss(
    entries: &[SessionEntry],
    message: &AssistantMessage,
    models: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let scan = scan(entries, models);
    let view = AssistantView {
        provider: message.provider.clone(),
        model: message.model.clone(),
        timestamp: message.timestamp as f64,
        usage: UsageNumbers::from_usage(&message.usage),
    };
    detect_miss(scan.prev.as_ref(), &view, models)
}

/// `AssistantMessage` 是否是可计数的成功响应（用于测试与上层过滤）。
///
/// 仅当 `stop_reason` 是 `Error` 或 `Aborted` 时返回 `false`；`Pending`、`Stop`、
/// `Length`、`ToolUse`、`Deferred` 都算成功。
///
/// TS: `cache-stats.ts#isSuccessfulResponse`
pub fn is_successful_response(message: &AssistantMessage) -> bool {
    !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
}
