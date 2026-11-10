//! `harness/usage.ts`：一个会话自己的用量账本。
//!
//! TS: `packages/durable/src/harness/usage.ts`。账本记录条目自身的开销，以及没有模型身份
//! 的 compaction 摘要尝试；键是 `provider/modelId`（models）或工具名（tools）。
//!
//! # 为什么是文档而不是一张表
//! 用量必须和产生它的条目/工具结果放在同一次提交里：`record_usage` 只接受调用方事务里的
//! `Tx`，与 `append_entry`/工具结果的写入共用同一个事务，因此条目与账本一起提交或一起回滚。
//! `Harness::usage()` 之后读取每个会话的这份文档，并用 `add_usage_state` 汇总。
//!
//! # 数据形状
//! [`USAGE_DOC`] 是会话作用域、`history: latest`、`fork: initial` 的文档，初始值
//! `{ "models": {}, "tools": {} }`。两个桶的值都是累计 `Usage` 对象：`models` 的键是
//! `provider/modelId`，`tools` 的键是工具名。累计量只增不减，合并用 [`add_usage`] /
//! [`add_usage_state`]。
//!
//! # 写入路径
//! [`record_usage`] 取文档草稿（不存在时按定义初始值创建），选中桶：
//! - 键不存在：把 `Usage` 序列化成严格 JSON 写入；可选计数器为 `None` 时省略（serde 的
//!   `skip_serializing_if`），不会出现 `undefined`/`null`。
//! - 键已存在：在草稿的 JSON 对象上原地累加（`add_usage_json`），保留已有对象里本模块
//!   不认识的键。
//!
//! # 注意事项
//! - 键是任意字符串：工具可以叫 `toString` 或 `__proto__`。Rust 的 `IndexMap`/`serde_json`
//!   没有原型链，这些键只是普通字符串键，不会触发原型污染，因此无需特殊的“定义而非赋值”处理。
//! - `checkpoint_when` 恒为 true：每次普通变化都写成完整 base，而不是 delta（判定在
//!   `session/transaction.rs` 的组装阶段）。该文档的 `history` 是 `latest`，只保留当前状态。
//! - [`record_usage`] 的错误（桶不是对象、`Usage` 序列化失败）会让调用方事务失败；这里不做
//!   部分写入，也不吞错。

use std::sync::LazyLock;

use indexmap::IndexMap;
use kiana_ai::Usage;

use crate::types::{
    BoxError, ConversationId, DocDefinition, DocTarget, DocToken, DocumentFork, DocumentSemantics,
    JsonObject, JsonValue, Tx, define_doc,
};

pub use crate::harness::types::UsageState;

/// `UsageState` 的两个桶（TS: `keyof UsageState`）。
///
/// 桶名同时是持久文档顶层的两个键（`models`/`tools`），由 [`UsageBucket::as_str`] 给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageBucket {
    /// assistant 条目与摘要尝试，按 `provider/modelId` 键。
    Models,
    /// 工具结果，按工具名键。
    Tools,
}

impl UsageBucket {
    /// TS 的属性名。
    ///
    /// 这个字符串直接用作持久 JSON 的顶层键；已写入的账本按它读取，改名会让旧文档里的
    /// 桶对不上。
    pub fn as_str(self) -> &'static str {
        match self {
            UsageBucket::Models => "models",
            UsageBucket::Tools => "tools",
        }
    }
}

/// 一个会话的用量账本。
///
/// TS: `UsageDoc`
///
/// - `kind: "kiana.usage"`（TS 原值 `pi.usage`，Rust 侧为 `kiana.usage`）、`version: 1`、
///   会话作用域；
/// - `history: latest`（只保留当前状态）、`fork: initial`（fork 出的会话从空账本开始）；
/// - 初始值由 `with_initial` 给出：`{ "models": {}, "tools": {} }`；
/// - `checkpoint_when` 恒为 `true`：普通变化会写成完整 base，而不是 delta。
///
/// 文档按会话隔离，所以不同会话的账本互不影响；汇总由调用方（`Harness::usage()`）
/// 用 [`add_usage_state`] 完成。
pub static USAGE_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.usage",
            1,
            DocumentSemantics::latest(DocumentFork::Initial),
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("models".to_owned(), JsonValue::Object(Default::default()));
            value.insert("tools".to_owned(), JsonValue::Object(Default::default()));
            value
        })
        .with_checkpoint_when(|_, _, _| true),
    )
});

/// 把 `usage` 加进会话 `kiana.usage` 的一个桶，就在记录响应的那次提交里。
///
/// TS: `recordUsage`
///
/// 参数：`tx` 是调用方的事务（决定原子性）；`conversation_id` 定位账本文档；
/// `bucket` 选 `models`/`tools`；`key` 是桶内键（`provider/modelId` 或工具名）；
/// `usage` 是本次调用的计费量。
///
/// 行为：
/// - 文档不存在时由 `Tx::doc` 按 [`USAGE_DOC`] 的初始值创建；
/// - 键不存在时写入 `usage` 的严格 JSON 形态（可选计数器缺省）；
/// - 键已存在时在现有 JSON 对象上累加，并保留对象里未知的键；
/// - 桶的值不是对象时返回错误，让整个事务提交失败（不会静默跳过计费）。
///
/// 成功只表示变化已排入事务；真正的持久化/可见性由调用方的提交决定。
pub async fn record_usage(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    bucket: UsageBucket,
    key: &str,
    usage: &Usage,
) -> Result<(), BoxError> {
    let target = DocTarget::conversation(&USAGE_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    let doc = draft.value_mut();
    // 桶缺失时补成空对象；非对象值会被下面的 as_object_mut 拒绝。
    let bucket_value = doc
        .entry(bucket.as_str().to_owned())
        .or_insert_with(|| JsonValue::Object(Default::default()));
    let totals = bucket_value
        .as_object_mut()
        .ok_or("kiana.usage bucket is not an object")?;
    // 只看自有键：工具可以叫 `toString`。
    if totals.contains_key(key) {
        let total = totals.get_mut(key).expect("checked above");
        add_usage_json(total, usage);
    } else {
        // provider 可能把可选计数器留成 `undefined`；草稿只接受严格 JSON。
        totals.insert(key.to_owned(), serde_json::to_value(usage)?);
    }
    Ok(())
}

/// 把 `usage` 的每个计数器加进 `total`；可选计数器只要任一侧报告过就相加。
///
/// TS: `addUsage`
///
/// `input`/`output`/`cache_*`/`total_tokens` 与 `cost` 的五个金额字段无条件相加；
/// `cache_write1h`/`reasoning` 是可选计数：只有 `usage` 这一侧报告了 `Some` 才累加，
/// 若 `total` 之前没有就按 0 起算（测试 `add_usage_adds_every_counter_and_only_reported_optionals`
/// 锁定该行为）。
///
/// 计数用饱和加法：provider 报告的 token 数不受本仓库控制，接近 `u64::MAX` 时普通 `+=`
/// 会在 debug 构建 panic、release 构建回绕，把累计用量改成一个很小的数（JSON 路径
/// `add_usage_json` 已经是饱和语义）。
pub fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input = total.input.saturating_add(usage.input);
    total.output = total.output.saturating_add(usage.output);
    total.cache_read = total.cache_read.saturating_add(usage.cache_read);
    total.cache_write = total.cache_write.saturating_add(usage.cache_write);
    total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
    if let Some(cache_write1h) = usage.cache_write1h {
        total.cache_write1h = Some(
            total
                .cache_write1h
                .unwrap_or(0)
                .saturating_add(cache_write1h),
        );
    }
    if let Some(reasoning) = usage.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or(0).saturating_add(reasoning));
    }
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}

/// 把 `state` 的每个桶加进 `sum`。
///
/// TS: `addUsageState`
///
/// 对每个桶按键合并：`sum` 里已有的键用 [`add_usage`] 累加；没有的键直接克隆插入，
/// 插入顺序跟随 `state` 的迭代顺序（`tests` 里有顺序断言）。
pub fn add_usage_state(sum: &mut UsageState, state: &UsageState) {
    add_usage_map(&mut sum.models, &state.models);
    add_usage_map(&mut sum.tools, &state.tools);
}

/// 单桶合并：已有键累加，新键克隆插入。
fn add_usage_map(sum: &mut IndexMap<String, Usage>, state: &IndexMap<String, Usage>) {
    for (key, usage) in state {
        match sum.get_mut(key) {
            Some(total) => add_usage(total, usage),
            // 定义而不是赋值：名叫 `__proto__` 的键也只是普通键。
            None => {
                sum.insert(key.clone(), usage.clone());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 草稿上的 JSON 形态（TS 的 `Draft<Usage>` 原地修改）
// ---------------------------------------------------------------------------

/// 在草稿里原地相加：保留对象上未知的键（TS 也是原地改属性）。
///
/// 这是 [`add_usage`] 的 JSON 形态：草稿里存的是持久 JSON，不能反序列化成 `Usage` 再写回，
/// 否则会丢掉本模块不认识的键。计数器缺失或不是数字时按 0 起算；`cost` 缺失时补一个空对象。
fn add_usage_json(total: &mut JsonValue, usage: &Usage) {
    let Some(object) = total.as_object_mut() else {
        // TS 会对非对象属性赋值时抛 TypeError；这里以替换成一份新账目收尾（不可达）。
        *total = serde_json::to_value(usage).unwrap_or(JsonValue::Null);
        return;
    };
    add_counter(object, "input", usage.input);
    add_counter(object, "output", usage.output);
    add_counter(object, "cacheRead", usage.cache_read);
    add_counter(object, "cacheWrite", usage.cache_write);
    add_counter(object, "totalTokens", usage.total_tokens);
    if let Some(cache_write1h) = usage.cache_write1h {
        add_counter(object, "cacheWrite1h", cache_write1h);
    }
    if let Some(reasoning) = usage.reasoning {
        add_counter(object, "reasoning", reasoning);
    }
    let cost = object
        .entry("cost".to_owned())
        .or_insert_with(|| JsonValue::Object(Default::default()));
    if let Some(cost) = cost.as_object_mut() {
        add_cost(cost, "input", usage.cost.input);
        add_cost(cost, "output", usage.cost.output);
        add_cost(cost, "cacheRead", usage.cost.cache_read);
        add_cost(cost, "cacheWrite", usage.cost.cache_write);
        add_cost(cost, "total", usage.cost.total);
    }
}

/// 把一个 u64 计数器加到 JSON 对象上。
///
/// 按 JSON 数字的表示分别处理：非负整数（`is_u64()` 为真）走 u64 分支，用
/// `saturating_add`（溢出时停在 `u64::MAX`，不回绕）；负整数（`is_i64()`，来自旧账本或
/// 人工修复）按整数相加——TS 的 `total.x += usage.x` 同样是在原值上累加，而不是把旧值
/// 当成 0；浮点（`is_f64()`）走 f64 分支，把增量转成 `f64` 相加并保持浮点形态。
/// 只有缺失、null、字符串等非数字值才按 0 起算。
fn add_counter(object: &mut serde_json::Map<String, JsonValue>, key: &str, delta: u64) {
    match object.get(key) {
        Some(JsonValue::Number(number)) if number.is_u64() => {
            let sum = number.as_u64().unwrap_or(0).saturating_add(delta);
            object.insert(key.to_owned(), JsonValue::from(sum));
        }
        Some(JsonValue::Number(number)) if number.is_i64() => {
            // 负整数：i128 不会溢出（i64 + u64 落在 i128 内）。超出 i64 时退回浮点，
            // 与 JS 的 Number 语义一致。
            let sum = number.as_i64().unwrap_or(0) as i128 + delta as i128;
            let value = match i64::try_from(sum) {
                Ok(sum) => JsonValue::from(sum),
                Err(_) => number_from_f64(sum as f64),
            };
            object.insert(key.to_owned(), value);
        }
        Some(JsonValue::Number(number)) if number.is_f64() => {
            let sum = number.as_f64().unwrap_or(0.0) + delta as f64;
            object.insert(key.to_owned(), number_from_f64(sum));
        }
        // 缺省（或非数字）按 0 起算；JS 的 `undefined + n` 是 NaN，但计数器总是由本模块写出。
        _ => {
            object.insert(key.to_owned(), JsonValue::from(delta));
        }
    }
}

/// 把一个 `f64` 金额加到 JSON 对象上；非数字按 `0.0` 起算。
///
/// 结果经 `number_from_f64` 落盘，因此 NaN/∞ 会变成 JSON `null`（见该函数）。
fn add_cost(object: &mut serde_json::Map<String, JsonValue>, key: &str, delta: f64) {
    let current = object.get(key).and_then(JsonValue::as_f64).unwrap_or(0.0);
    object.insert(key.to_owned(), number_from_f64(current + delta));
}

/// `JSON.stringify` 会把非有限数字写成 `null`；草稿里保持同样的可持久形态。
///
/// `serde_json::Number::from_f64` 对 NaN/±∞ 返回 `None`，这里统一落成 `Value::Null`；
/// 有限值保留浮点表示。
fn number_from_f64(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

// 锁定：账本初值、可选计数器只在报告侧出现时求和、typed 与 JSON 两种累加形态一致、
// 未知键保留、`toString`/`__proto__` 这类名字按普通键处理。
#[cfg(test)]
mod tests {
    use super::*;
    use kiana_ai::{Usage, UsageCost};

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read: 1,
            cache_write: 2,
            cache_write1h: None,
            reasoning: None,
            total_tokens: input + output + 3,
            cost: UsageCost {
                input: 0.5,
                output: 1.5,
                cache_read: 0.25,
                cache_write: 0.75,
                total: 3.0,
                total_from_provider: None,
            },
        }
    }

    #[test]
    fn the_doc_starts_with_two_empty_buckets() {
        let definition = &USAGE_DOC.definition;
        assert_eq!(definition.kind, "kiana.usage");
        assert_eq!(definition.version, 1);
        assert_eq!(
            definition.semantics,
            DocumentSemantics::latest(DocumentFork::Initial)
        );
        let initial = (definition.initial)(None);
        let names: Vec<&str> = initial.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["models", "tools"]);
        assert_eq!(initial["models"], serde_json::json!({}));
        assert_eq!(initial["tools"], serde_json::json!({}));
        assert!((definition.checkpoint_when)(
            &initial,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
    }

    #[test]
    fn add_usage_adds_every_counter_and_only_reported_optionals() {
        let mut total = usage(10, 20);
        let mut delta = usage(1, 2);
        delta.cache_write1h = Some(4);
        add_usage(&mut total, &delta);
        assert_eq!(total.input, 11);
        assert_eq!(total.output, 22);
        assert_eq!(total.cache_read, 2);
        assert_eq!(total.cache_write, 4);
        assert_eq!(total.total_tokens, 39);
        assert_eq!(total.cache_write1h, Some(4));
        assert_eq!(total.reasoning, None);
        assert_eq!(total.cost.total, 6.0);

        let mut reasoning = usage(1, 1);
        reasoning.reasoning = Some(7);
        add_usage(&mut total, &reasoning);
        assert_eq!(total.reasoning, Some(7));
        assert_eq!(total.cache_write1h, Some(4));
    }

    #[test]
    fn add_usage_saturates_counter_overflow() {
        // 回归：provider 可控的 token 数接近 `u64::MAX` 时，普通 `+=` 在 debug 构建
        // panic、release 构建回绕；这里必须停在 `u64::MAX`。
        let mut total = usage(10, 20);
        total.cache_write = u64::MAX - 1;
        total.cache_write1h = Some(u64::MAX - 1);
        let mut delta = usage(1, 2);
        delta.cache_write = 10;
        delta.cache_write1h = Some(10);
        add_usage(&mut total, &delta);
        assert_eq!(total.cache_write, u64::MAX);
        assert_eq!(total.cache_write1h, Some(u64::MAX));
    }

    #[test]
    fn add_usage_state_inserts_then_adds_per_bucket() {
        let mut sum = UsageState::default();
        let mut state = UsageState::default();
        state.models.insert("p/m".to_owned(), usage(1, 2));
        state.tools.insert("toString".to_owned(), usage(3, 4));
        state.tools.insert("__proto__".to_owned(), usage(5, 6));

        add_usage_state(&mut sum, &state);
        assert_eq!(
            sum.models.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["p/m"]
        );
        assert_eq!(
            sum.tools.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["toString", "__proto__"]
        );
        assert_eq!(sum.tools["toString"].input, 3);

        let mut more = UsageState::default();
        more.models.insert("p/m".to_owned(), usage(10, 20));
        more.tools.insert("toString".to_owned(), usage(1, 1));
        add_usage_state(&mut sum, &more);
        assert_eq!(sum.models["p/m"].input, 11);
        assert_eq!(sum.tools["toString"].input, 4);
        assert_eq!(sum.tools["__proto__"].input, 5);
    }

    #[test]
    fn json_form_matches_the_typed_form() {
        let mut typed = usage(0, 0);
        typed.input = 0;
        typed.output = 0;
        let mut json = serde_json::to_value(&typed).unwrap();
        let delta = {
            let mut value = usage(2, 3);
            value.cache_write1h = Some(9);
            value.reasoning = Some(1);
            value
        };
        add_usage(&mut typed, &delta);
        add_usage_json(&mut json, &delta);
        assert_eq!(json, serde_json::to_value(&typed).unwrap());
        assert_eq!(json["cacheWrite1h"], serde_json::json!(9));
        assert_eq!(json["cost"]["total"], serde_json::json!(6.0));
    }

    #[test]
    fn json_form_keeps_unknown_keys() {
        let usage = usage(1, 1);
        let mut json =
            serde_json::json!({"input": 1, "future": true, "cost": {"total": 1.0, "extra": 2}});
        add_usage_json(&mut json, &usage);
        assert_eq!(json["future"], serde_json::json!(true));
        assert_eq!(json["cost"]["extra"], serde_json::json!(2));
        assert_eq!(json["input"], serde_json::json!(2));
        assert_eq!(json["cost"]["total"], serde_json::json!(4.0));
    }

    #[test]
    fn counter_keys_are_ordinary_keys() {
        let usage = usage(1, 1);
        let mut json = serde_json::json!({});
        add_usage_json(&mut json, &usage);
        assert_eq!(json["input"], serde_json::json!(1));
        assert_eq!(json["cacheRead"], serde_json::json!(1));
    }

    #[test]
    fn json_form_adds_to_a_negative_stored_counter() {
        // 回归：负整数旧值（`is_i64()`）曾落入兜底分支被替换成增量，丢掉已有欠额。
        let usage = usage(3, 0);
        let mut json = serde_json::json!({"input": -5});
        add_usage_json(&mut json, &usage);
        assert_eq!(json["input"], serde_json::json!(-2));
    }
}
