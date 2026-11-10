//! `harness/json.ts`：把一份 JSON 值逐叶写进文档草稿。
//!
//! TS: `packages/durable/src/harness/json.ts`（60 行）。Chord 把一次容器赋值记录为一整个
//! `set`，只有字符串叶子被重新赋成更长的串时才产生 append；因此把流的中间态整体写回去会让每次
//! flush 都存储并发布完整消息。本模块的入口 [`assign_json`] 按叶子写入，`plan` 说明某个字符串
//! 成员自上次赋值以来增长了哪个文本。
//!
//! Rust 的 `DocDraft` 在结算时用 chord 的 `diff_revisions()` 与基线求差（见
//! `crate::types::DocDraft`），因此"逐叶写入"在这里等价于"把最终值写成与 TS 相同的形状"：
//! 字符串增长由 diff 输出 `a` 操作，容器增删由 `s`/`d` 输出。`AssignPlan` 保留三种形状，
//! 因为它决定 `assign_json` 写哪个值（`keep` 不写、`append` 写 `current + text`），
//! 并驱动 `partial_changes` 的调用点。
//!
//! # 执行流程
//!
//! [`assign_json`] 是唯一的写入入口，按当前值的形状与 `plan` 选择分支：
//!
//! 1. `plan = Keep`：立即返回，既不删除也不写入。
//! 2. 双方都是对象：先删掉 `current` 里 `value` 没有的成员，再按 `value` 的成员递归；子计划
//!    从 `Fields` 里按键查找。
//! 3. 双方都是数组且 `current` 不更长：逐项递归，超出部分 push；缩短时整体赋值。
//! 4. `Append` 且目标是对象成员、两边都是字符串、UTF-16 长度吻合：写入 `current + text`，
//!    形状表示“只追加了 `text`”。
//! 5. 其余：结构不相等时整体赋值。
//!
//! 除 `plan = Keep` 直接返回外，对象与数组的逐叶合并由值的形状决定，与 `Append`、`Fields`
//! 和 `None` 无关；`Keep` 正是会阻止容器递归的例外。因此 `harness/tool.rs` 写 `details` 时
//! 传 `None` 也会逐叶合并。
//!
//! # 边界与失败模式
//!
//! - `target` 必须是对象（[`TargetKey::Name`]）或数组（[`TargetKey::Index`]），且下标最多
//!   等于数组长度；类型不符或越界会在内部 `expect`/索引处 panic（见 [`assign_json`] 的
//!   Panics 一节）。现有调用点都传符合形状的草稿值。
//! - 值比较用 [`js_equal`] 而不是 `serde_json::Value` 的 `PartialEq`：后者把 `1` 与 `1.0`
//!   判为不等，而 JS 的 `===` 按数值比较。
//! - 对象键序：工作区给 `serde_json` 启用了 `preserve_order`，所以按 `value` 的插入序递归
//!   与 TS 的 `Object.entries` 一致。

use std::collections::HashMap;

use serde_json::Value;

use crate::harness::live::utf16_len;

/// `assignJson()` 如何写入一个值，而不是逐叶赋值（TS: `AssignPlan`）。
///
/// 它只是“写入策略”：容器是否递归合并由值的形状决定，`AssignPlan` 只指定某个槽位要不要
/// 跳过（`Keep`）、字符串是否按增量追加（`Append`），以及子键各自的策略（`Fields`）。
#[derive(Debug, Clone, PartialEq)]
pub enum AssignPlan {
    /// 字符串自上次赋值以来增长了 `text`；长度吻合时追加它。
    ///
    /// 只在目标是对象成员（不是数组元素）、当前值与新值都是字符串、且
    /// `utf16_len(current) + utf16_len(text) == utf16_len(value)` 时生效；否则退化为按值
    /// 整体比较。长度按 UTF-16 code unit 计算，与 JS 的 `String.length` 一致。
    Append {
        /// 增长出来的文本。
        text: String,
    },
    /// 保持原值；调用者自己写。
    ///
    /// `assign_json` 立即返回：既不删除也不写入。`partial_changes` 用它跳过工具调用的
    /// `arguments`，那部分由 `JsonChange` 序列单独应用。
    Keep,
    /// 容器部分成员的写入计划。
    ///
    /// 键是对象成员名或数组下标；查不到子计划等价于 `None`（按默认规则写入）。
    Fields(PlanFields),
}

/// `AssignPlan.fields` 的键：对象成员名或数组下标（TS: `ReadonlyMap<string | number, AssignPlan>`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlanKey {
    /// 对象成员名。
    Name(String),
    /// 数组下标。
    Index(usize),
}

/// `AssignPlan.fields`：键 → 子计划的查找表。
///
/// TS 是 `ReadonlyMap`（保持插入序）；这里用 `HashMap`，遍历顺序不固定。当前调用点
/// （`partial_changes::take`、`assign_json` 与测试）都只做按键查找，不依赖顺序，因此不要把
/// 它当作有序结构使用。
pub type PlanFields = HashMap<PlanKey, AssignPlan>;

/// 目标槽位：对象成员（`target[key]`）或数组元素（`target[index]`）。
///
/// 成员名用 `&str` 借用，因为递归时键来自 `value` 的成员名，不需要复制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKey<'a> {
    /// 对象成员名。
    Name(&'a str),
    /// 数组下标。
    Index(usize),
}

/// 在 `target[key]` 处逐叶赋值（TS: `assignJson`）。
///
/// `target` 必须是对象（[`TargetKey::Name`]）或数组（[`TargetKey::Index`]）；`plan` 说明该值的
/// 写入方式。语义逐条对应 TS：
///
/// - `Keep` 立即返回（不删、不写）；
/// - 双方都是对象：先删掉 `current` 里 `value` 没有的键，再按 `value` 的键递归（键序 = `value` 的
///   插入序）；
/// - 双方都是数组且 `current.len() <= value.len()`：逐项递归，多出来的 push；
/// - `Append` 且目标不是数组、两侧都是字符串、`current` 的 UTF-16 长度加 `text` 的长度等于 `value`
///   的 UTF-16 长度时，写 `current + text`（结果与 `value` 相同，但形状描述了增量）；
/// - 其余情况：`current` 与 `value` 不相等时整体写入。
///
/// `plan` 为 `None` 时按默认规则写入：对象/数组仍逐叶合并，标量不相等才整体赋值。
///
/// # Panics
///
/// 违反“`target` 的形状与 `key` 匹配”的调用会 panic：
///
/// - [`TargetKey::Name`] 而 `target` 不是对象：`set_slot` 的 `expect("object target")`；
/// - [`TargetKey::Index`] 而 `target` 不是数组：`expect("array target")`；
/// - 下标大于数组长度：`items[index] = value` 越界（等于长度时是 push）。
///
/// 槽位已存在时不会走到 `set_slot`，所以上述 panic 只在写入缺失槽位时发生。
///
/// # 前提
///
/// `Append` 分支只校验 UTF-16 长度，不校验 `text` 的内容确实是 `current` 的后缀；调用方
/// 必须保证这个前提（`partial_changes::take` 记录的是实际增量）。
pub fn assign_json(
    target: &mut Value,
    key: TargetKey<'_>,
    value: &Value,
    plan: Option<&AssignPlan>,
) {
    if matches!(plan, Some(AssignPlan::Keep)) {
        return;
    }
    // plan 只提供 Keep 判断与子键计划；容器是否递归由值的形状单独决定。
    let fields = match plan {
        Some(AssignPlan::Fields(fields)) => Some(fields),
        _ => None,
    };
    let current = slot_mut(target, key);
    match current {
        Some(current) if current.is_object() && value.is_object() => {
            // 先删后写：value 里没有的成员必须消失，否则旧键会残留。递归只按 value 的键进行，
            // 所以子计划也只用 value 的键去查。
            let current_map = current.as_object_mut().expect("checked is_object above");
            let value_map = value.as_object().expect("checked is_object above");
            current_map.retain(|name, _| value_map.contains_key(name));
            for (name, child) in value_map {
                let child_plan = fields.and_then(|fields| fields.get(&PlanKey::Name(name.clone())));
                assign_json(current, TargetKey::Name(name), child, child_plan);
            }
        }
        Some(current) if current.is_array() && value.is_array() => {
            // 只在 current 不更长时逐项合并；缩短只能整体赋值（TS 语义）。
            let value_items = value.as_array().expect("checked is_array above");
            let current_len = current.as_array().expect("checked is_array above").len();
            if current_len <= value_items.len() {
                for (index, child) in value_items.iter().enumerate() {
                    let child_plan = fields.and_then(|fields| fields.get(&PlanKey::Index(index)));
                    if index < current_len {
                        assign_json(current, TargetKey::Index(index), child, child_plan);
                    } else {
                        current
                            .as_array_mut()
                            .expect("checked is_array above")
                            .push(child.clone());
                    }
                }
            } else if !js_equal(current, value) {
                // 数组缩短：整体赋值（TS 的 `slots[key] = value`）。
                *current = value.clone();
            }
        }
        Some(current)
            if matches!(plan, Some(AssignPlan::Append { .. }))
                && matches!(key, TargetKey::Name(_))
                && current.is_string()
                && value.is_string() =>
        {
            // 目标是对象成员时才允许追加；数组元素里的字符串必须整体重赋值（TS 的
            // `appendText` 只接受对象成员）。
            let AssignPlan::Append { text } = plan.expect("matched above") else {
                unreachable!("matched above");
            };
            let current_text = current.as_str().expect("checked is_string above");
            let value_text = value.as_str().expect("checked is_string above");
            // 长度按 UTF-16 code unit 计算（emoji 等代理对占 2）。长度对不上说明 plan 与值
            // 不吻合，不能安全追加，退回按值比较。
            if utf16_len(current_text) + utf16_len(text) == utf16_len(value_text) {
                let mut appended = String::with_capacity(current_text.len() + text.len());
                appended.push_str(current_text);
                appended.push_str(text);
                *current = Value::String(appended);
            } else if !js_equal(current, value) {
                *current = value.clone();
            }
        }
        Some(current) => {
            // TS 这里是 `current !== value`：对象/数组按引用身份几乎总是不等，primitive 按值。
            // Rust 没有引用身份，改用结构相等，相等时跳过无变化的写入；本文件的
            // `equal_values_are_not_rewritten` 测试锁定这一行为。
            if !js_equal(current, value) {
                *current = value.clone();
            }
        }
        None => {
            // 槽位不存在：TS 的 `slots[key]` 是 undefined，走最后的整体赋值。
            set_slot(target, key, value.clone());
        }
    }
}

// 只读当前槽位；目标形状与 key 不符或下标越界时返回 `None`，由调用方走 `set_slot`。
fn slot_mut<'a>(target: &'a mut Value, key: TargetKey<'_>) -> Option<&'a mut Value> {
    match key {
        TargetKey::Name(name) => target.get_mut(name),
        TargetKey::Index(index) => target.get_mut(index),
    }
}

// 写入一个缺失的槽位；`target` 的形状必须与 key 匹配（见 `assign_json` 的 Panics）。
fn set_slot(target: &mut Value, key: TargetKey<'_>, value: Value) {
    match key {
        TargetKey::Name(name) => {
            let map = target.as_object_mut().expect("object target");
            map.insert(name.to_owned(), value);
        }
        TargetKey::Index(index) => {
            let items = target.as_array_mut().expect("array target");
            if index == items.len() {
                items.push(value);
            } else {
                items[index] = value;
            }
        }
    }
}

/// `isRecord()`：非 null、非数组的对象。
///
/// `None` 对应 JS 的 `undefined`，与 `null` 一样返回 `false`。`partial_changes` 用它拒绝
/// 根不是对象的流式工具参数（那样参数没有可追踪的成员）。
pub fn is_record(value: Option<&Value>) -> bool {
    matches!(value, Some(value) if value.is_object())
}

/// JS `===` 对 JSON 值的等价：数字按数值比较（`1 === 1.0`），其余标量按值，数组逐项、对象按
/// 键集合逐项。`serde_json` 的结构相等会把 `1` 与 `1.0` 判为不等，TS 的 `!==` 不会。
///
/// 与真正的 JS `===` 有一处刻意差异：JS 对对象/数组比较引用身份（两个内容相同的 JSON 对象
/// 也不相等），这里按**结构**相等。`assign_json` 依赖这一点跳过无变化的写入（见
/// `equal_values_are_not_rewritten` 测试），`tool.rs` 也用它判断参数是否真的被改过。
pub fn js_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| js_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| right.get(key).is_some_and(|other| js_equal(value, other)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fields(entries: Vec<(PlanKey, AssignPlan)>) -> AssignPlan {
        AssignPlan::Fields(entries.into_iter().collect())
    }

    #[test]
    fn keep_writes_nothing() {
        let mut target = json!({ "a": 1 });
        assign_json(
            &mut target,
            TargetKey::Name("a"),
            &json!(2),
            Some(&AssignPlan::Keep),
        );
        assert_eq!(target, json!({ "a": 1 }));
    }

    #[test]
    fn records_merge_leaf_by_leaf_and_drop_removed_keys() {
        let mut target = json!({ "message": { "text": "he", "gone": 1 } });
        let value = json!({ "text": "hello", "keep": [1] });
        assign_json(
            &mut target,
            TargetKey::Name("message"),
            &value,
            Some(&fields(vec![(
                PlanKey::Name("text".to_owned()),
                AssignPlan::Append {
                    text: "llo".to_owned(),
                },
            )])),
        );
        assert_eq!(
            target,
            json!({ "message": { "text": "hello", "keep": [1] } })
        );
    }

    #[test]
    fn arrays_grow_item_by_item_and_shrink_whole() {
        let mut target = json!({ "items": [1, 2] });
        assign_json(
            &mut target,
            TargetKey::Name("items"),
            &json!([1, 3, 4]),
            None,
        );
        assert_eq!(target, json!({ "items": [1, 3, 4] }));

        let mut target = json!({ "items": [1, 2, 3] });
        assign_json(&mut target, TargetKey::Name("items"), &json!([1]), None);
        assert_eq!(target, json!({ "items": [1] }));
    }

    #[test]
    fn appends_text_when_lengths_agree() {
        let mut target = json!({ "text": "he" });
        assign_json(
            &mut target,
            TargetKey::Name("text"),
            &json!("hello"),
            Some(&AssignPlan::Append {
                text: "llo".to_owned(),
            }),
        );
        assert_eq!(target, json!({ "text": "hello" }));
    }

    #[test]
    fn append_falls_back_to_the_value_when_lengths_disagree() {
        let mut target = json!({ "text": "he" });
        assign_json(
            &mut target,
            TargetKey::Name("text"),
            &json!("goodbye"),
            Some(&AssignPlan::Append {
                text: "llo".to_owned(),
            }),
        );
        assert_eq!(target, json!({ "text": "goodbye" }));
    }

    #[test]
    fn array_strings_are_reassigned_not_appended() {
        let mut target = json!({ "items": ["he"] });
        assign_json(
            &mut target,
            TargetKey::Name("items"),
            &json!(["hello"]),
            Some(&fields(vec![(
                PlanKey::Index(0),
                AssignPlan::Append {
                    text: "llo".to_owned(),
                },
            )])),
        );
        assert_eq!(target, json!({ "items": ["hello"] }));
    }

    #[test]
    fn utf16_lengths_guard_the_append() {
        // 一个 emoji 是 2 个 UTF-16 code unit；`text` 的长度按 code unit 计算。
        let mut target = json!({ "text": "a" });
        assign_json(
            &mut target,
            TargetKey::Name("text"),
            &json!("a😀"),
            Some(&AssignPlan::Append {
                text: "😀".to_owned(),
            }),
        );
        assert_eq!(target, json!({ "text": "a😀" }));
    }

    #[test]
    fn missing_members_are_created() {
        let mut target = json!({});
        assign_json(&mut target, TargetKey::Name("a"), &json!({ "b": 1 }), None);
        assert_eq!(target, json!({ "a": { "b": 1 } }));
    }

    #[test]
    fn equal_values_are_not_rewritten() {
        let mut target = json!({ "a": { "b": [1, { "c": "d" }] } });
        let before = target.clone();
        assign_json(
            &mut target,
            TargetKey::Name("a"),
            &json!({ "b": [1, { "c": "d" }] }),
            None,
        );
        assert_eq!(target, before);
    }

    #[test]
    fn js_equality_compares_numbers_by_value() {
        // TS 的 `1 === 1.0` 为真；serde_json 的结构相等会判不等。
        assert!(js_equal(&json!(1), &json!(1.0)));
        assert!(!js_equal(&json!(1), &json!(2)));
        assert!(js_equal(
            &json!({ "a": [1, { "b": [2.0] }] }),
            &json!({ "a": [1.0, { "b": [2] }] })
        ));
        assert!(!js_equal(&json!({ "a": 1 }), &json!({ "a": 1, "b": 2 })));
        assert!(!js_equal(&json!([1]), &json!([1, 2])));
        assert!(js_equal(&json!(null), &json!(null)));
        assert!(!js_equal(&json!("1"), &json!(1)));
    }
}
