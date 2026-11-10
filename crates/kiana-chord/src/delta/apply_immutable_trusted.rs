//! `delta/apply-immutable-trusted.ts`：**不校验**操作、调用方自产的不可变应用。
//!
//! TS 版本用 copy-on-write 让未触碰的容器与入参共享身份；Rust 的 `serde_json::Value`
//! 没有可观察的指针身份，且所有权已保证入参不会被调用方观察到修改，因此这里复用
//! [`crate::delta::index::apply`] 的应用逻辑，但**跳过** `assertValidOp`：与 TS 的 trusted
//! 应用器一样，`__proto__` 这样的保留段是一条普通的数据路径，索引也不做范围预检。
//! 违反调用契约（畸形 op、非容器路径等）会直接失败：TS 抛 `TypeError`，Rust 侧 panic
//! （签名无法返回 `Result`）。
//!
//! ## 为什么可以跳过校验
//!
//! trusted 应用器的输入必须是**调用方自己刚生成**的 op；对这些 op 再跑一遍
//! `assert_valid_op` 只是重复检查，所以两边都刻意省掉。代价是：传入手写或来自网络的 op 时，
//! 没有任何一层会在应用之前预检它——trusted 版连每条 op 应用前的 `assert_valid_op` 都没有，
//! 畸形 op 要等应用到它时才失败；校验版 `apply` 也只在应用该条之前才调用 `assert_valid_op`，
//! 不会在调用入口一次性拒绝整批 op。
//!
//! ## 与 `index::apply` 的关系
//!
//! 本函数直接复用 `index::apply_unvalidated`（与校验版共用同一份 `apply_inner`），因此除
//! “不调用 `assert_valid_op`”外，应用期的检查与原版相同：父容器必须存在且是容器、
//! `s`/`d`/`a`/`t` 的数组下标不能越过末尾一格、`a`/`t` 的目标必须是字符串、`p`/`m` 的
//! 目标必须是数组。失败时把 `OpError::message()` 的 Rust 错误文案拼进 panic 消息
//! （`PathError` 的 `unresolvable path: ...` 或 `UnsafePathError` 的
//! `unsafe path segment: ...`）。TS 的 `applyImmutableTrusted` 并不复用校验版 apply 的
//! 错误：例如父级缺失时 Rust 报 `unresolvable path: ["missing"]`，TS 抛的是
//! `Trusted operation path is not a container` 这类 `TypeError`，两边文案不同。
//!
//! 注意“跳过校验”只指入口的 `assert_valid_op`：写入时的边界检查仍在，所以“索引不做
//! 范围预检”并不等于接受越界下标。
//!
//! Rust 的所有权保证调用方无法观察到入参被修改；替换 payload 也会在被采用前克隆，后续
//! op 改不到它（测试 `replacement_payloads_are_protected` 锁定这一点）。
//!
//! ## 调用方
//!
//! TS 侧只有 `tracker.ts` 的物化路径调用它（含 `p`/`m` 的批次改走 `applyImmutable`，见
//! `tracker.ts` 的 `materializeOperations`）。kiana-chord 的 delta tracker 尚未移植，当前
//! 工作区内只有单元测试直接调用本函数；值级的 `Tracker`/`Change` 在 `kiana-durable`，
//! 见 `tracker` 模块说明。调用方必须保证 op 来自可信生成器——本函数没有第二道校验。

use serde_json::Value;

use crate::delta::Op;
use crate::delta::index::apply_unvalidated;

/// TS `applyImmutableTrusted(target, operations)`。
///
/// # 参数
///
/// - `root`：被修改的值，按所有权传入；调用方保留的副本不会被改动。
/// - `operations`：调用方自产的 op，按切片顺序应用。
///
/// # 返回
///
/// 应用全部 op 后的新根。`r` 之后的 op 继续作用在替换值上。
///
/// # Panics
///
/// 应用期的检查失败时 panic（TS 的 trusted 应用器在同类情形抛 `TypeError`），消息为
/// `trusted operation contract violated: <OpError::message()>`。触发条件包括：路径上的
/// 父级不存在或不是容器、`s`/`d`/`a`/`t` 的数组下标越界、`a`/`t` 的目标不是字符串、
/// `p`/`m` 的目标不是数组、`m` 的排列长度与数组长度不一致。`p` 的 `index`/`remove` 按
/// JS `splice` 语义钳制，不会因越界失败。
///
/// `m` 的排列**项**越界（`source >= items.len()`）是另一种失败：读取 `previous[*source]`
/// 时触发 Rust 的索引越界 panic，而不是上面的带前缀消息。项都在范围内但不是双射
/// （例如 `[0, 0]`）不会被检测——既不报错也不 panic，而是静默地产生重复或丢失元素的
/// 结果。trusted 调用方必须自行保证排列是双射（或在调用前显式校验）。
pub fn apply_immutable_trusted(root: Value, operations: &[Op]) -> Value {
    match apply_unvalidated(root, operations) {
        Ok(value) => value,
        // TS 在这里抛 TypeError；Rust 签名没有 Result，只能 panic。前缀标明错误来自
        // trusted 契约（而非调用方传入的普通无效输入）。
        Err(error) => panic!("trusted operation contract violated: {}", error.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::index::parse_op;
    use serde_json::json;

    fn op(value: serde_json::Value) -> Op {
        parse_op(&value).expect("op")
    }

    #[test]
    fn applies_operations_without_copying_untouched_branches() {
        let base = json!({"text": "abcdef", "meta": {"count": 0}, "values": [1, 2, 3]});
        let operations = vec![
            op(json!(["t", ["text"], 2])),
            op(json!(["a", ["text"], "!"])),
            op(json!(["s", ["meta", "count"], 1])),
            op(json!(["p", ["values"], 1, 1, [4, 5]])),
            op(json!(["m", ["values"], [3, 0, 1, 2]])),
        ];
        let result = apply_immutable_trusted(base.clone(), &operations);
        assert_eq!(result["text"], json!("cdef!"));
        assert_eq!(result["meta"]["count"], json!(1));
        assert_eq!(result["values"], json!([3, 1, 4, 5]));
        // Rust 所有权语义：入参未被修改。
        assert_eq!(base["meta"]["count"], json!(0));
    }

    #[test]
    fn replacement_payloads_are_protected() {
        let replacement = json!({"nested": {"value": 1}});
        let result = apply_immutable_trusted(
            Value::Null,
            &[
                op(json!(["r", replacement.clone()])),
                op(json!(["s", ["nested", "value"], 2])),
            ],
        );
        assert_eq!(replacement["nested"]["value"], json!(1));
        assert_eq!(result["nested"]["value"], json!(2));
    }

    #[test]
    fn applies_reserved_segments_as_plain_data_without_validation() {
        // TS 的 trusted 应用器不做 assertValidOp：`__proto__` 是一条普通的数据路径。
        // （`parse_op` 会拒绝保留段，因此这里直接构造 `Op`，与自产 op 的调用方一致。）
        use crate::delta::Seg;

        let result = apply_immutable_trusted(
            json!({}),
            &[Op::Set(vec![Seg::Key("__proto__".into())], json!(1))],
        );
        assert_eq!(result.get("__proto__"), Some(&json!(1)));
        let result = apply_immutable_trusted(
            json!({"__proto__": {"value": 1}}),
            &[Op::Set(
                vec![Seg::Key("__proto__".into()), Seg::Key("value".into())],
                json!(2),
            )],
        );
        assert_eq!(result["__proto__"]["value"], json!(2));
        // 校验版应用器仍然拒绝保留段。
        assert!(
            crate::delta::index::apply(
                json!({}),
                &[Op::Set(vec![Seg::Key("__proto__".into())], json!(1))]
            )
            .is_err()
        );
    }
}
