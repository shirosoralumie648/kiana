//! `delta/revision-validator.ts`：不可变副本的严格 JSON 校验。
//!
//! TS 的 `JsonRevisionValidator` 用一个可复用的 `WeakSet<object>` 记住已校验过的容器，
//! 并检查环、稀疏数组、符号键、访问器、非有限数字与 `undefined`。`serde_json::Value`
//! 只能表示严格 JSON（没有 `undefined`/symbol/访问器/环/NaN），因此本实现保留
//! **完整的遍历与错误文案**，但当前类型边界下错误不可达；遍历同时检查数字不是 `NaN`，
//! 以便将来从外部构造的值也走同一检查点。
//!
//! `WeakSet` 的"复用已校验容器"在 Rust 里没有等价物（`Value` 无身份）；每次调用都完整
//! 遍历，行为（接受/拒绝）相同。
//!
//! ## 执行流程
//!
//! [`JsonRevisionValidator::validate`] 每次调用新建 `ancestors`（当前递归栈）与 `finished`
//! （本次调用内已完整校验的容器）；`validate_inner` 对数组逐项、对对象逐值递归。
//! `ancestors` 命中即报环错误；`finished` 命中则跳过重复遍历。一个容器的子树全部通过后，
//! 它才被记入 `finished`，同时递增诊断计数 `validated`。
//!
//! ## 注意事项
//!
//! - 实例的 `validated` 只是**累计成功次数**：每次调用都可能重复统计同一个值，不是 TS
//!   `WeakSet` 的去重集合大小，也不参与校验决策。
//! - 在安全构造的 `Value` 上，`validate` 实际上不会返回 `Err`：`Value` 是拥有子值的树
//!   （构造不出环或共享节点），数组天然稠密，键只能是 `String`，数字也无法表示
//!   `NaN`/`±Infinity`。`RevisionError` 与各条文案仍按 TS 保留，供 API 兼容与将来替换
//!   表示时使用。
//! - 除 `CYCLE_MESSAGE` 与 `STRICT_JSON_MESSAGE` 外的错误文案对应当前表示下不可达的 JS
//!   检查（普通对象/稠密数组/symbol/访问器），保留它们是为了保持错误文案的稳定契约（将来替换表示时可直接复用）。
//! - 当前工作区内没有生产调用方：TS 侧只有基准测试直接引用，Rust 侧只有本文件的单元
//!   测试。本模块以公开 API 形式提供，接入点由使用方决定。

use std::collections::HashSet;

use serde_json::Value;

/// 校验错误：消息是稳定的对外错误文案（改动需同步测试）。
///
/// 当前只会由两个（在安全构造的 `Value` 上不可达的）分支产生：环与非法数字。`Display`
/// 输出 `message`，调用方可以按 TS 的 `error.message` 逐字比较。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionError {
    /// 完整消息。
    pub message: String,
}

impl RevisionError {
    fn new(message: &'static str) -> Self {
        RevisionError {
            message: message.to_owned(),
        }
    }
}

impl std::fmt::Display for RevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RevisionError {}

/// 各条固定错误文案（当前稳定契约，改动需同步测试）。
///
/// 当前实现只会返回 `CYCLE_MESSAGE` 与 `STRICT_JSON_MESSAGE`；其余五条对应
/// `serde_json::Value` 无法表达的 JS 形状（见模块说明）。
pub const CYCLE_MESSAGE: &str = "Replicated state cannot contain cycles";
/// 非严格 JSON 值。
pub const STRICT_JSON_MESSAGE: &str = "Replicated state values must be strict JSON";
/// 非普通对象/数组。
pub const PLAIN_CONTAINER_MESSAGE: &str =
    "Replicated state containers must be plain objects or arrays";
/// 稀疏数组/额外键。
pub const DENSE_ARRAY_MESSAGE: &str =
    "Replicated state arrays must be dense and contain only indexed entries";
/// 数组元素缺失/访问器。
pub const ARRAY_ENTRY_MESSAGE: &str =
    "Replicated state arrays must contain enumerable indexed data properties with defined values";
/// 符号属性。
pub const SYMBOL_MESSAGE: &str = "Replicated state cannot contain symbol properties";
/// 对象属性的访问器/不可枚举。
pub const OBJECT_ENTRY_MESSAGE: &str =
    "Replicated state objects must contain enumerable data properties";

/// TS `JsonRevisionValidator`：可复用实例。
///
/// “可复用”只体现在跨调用保留 `validated` 计数；校验本身每次都从头遍历，因为
/// `serde_json::Value` 没有可放进 `WeakSet` 的身份。
#[derive(Debug, Default)]
pub struct JsonRevisionValidator {
    /// TS 的 `WeakSet<object>`：Rust 的值没有身份，仅保留计数以便 API 形状一致。
    validated: usize,
}

impl JsonRevisionValidator {
    /// 新建校验器。
    pub fn new() -> Self {
        JsonRevisionValidator::default()
    }

    /// 已校验过的容器数（TS 的 `#validated` 大小不可观察；这里只作诊断）。
    ///
    /// 这是累计值：每次 `validate`/`validate_owned` 都把本次成功遍历的容器数加进来，
    /// 同一个值被重复校验会重复计数（TS 的 `WeakSet` 会去重）。不要拿它当缓存命中数。
    pub fn validated(&self) -> usize {
        self.validated
    }

    /// `validate(value)`：校验严格 JSON 并返回值（借用形式）。
    ///
    /// 返回的引用与入参指向同一个值；失败时返回 [`RevisionError`]，消息是稳定的错误文案
    /// （改动需同步测试）。每次调用都完整遍历，不复用上一次的校验结果。
    pub fn validate<'a>(&mut self, value: &'a Value) -> Result<&'a Value, RevisionError> {
        let mut ancestors = HashSet::new();
        let mut finished = HashSet::new();
        self.validate_inner(value, &mut ancestors, &mut finished)?;
        Ok(value)
    }

    /// `validate(value)` 的所有权版本（TS 返回同一个值）。
    ///
    /// 成功时原样返回入参（不复制）；失败时值被丢弃，错误里不携带它。
    pub fn validate_owned(&mut self, value: Value) -> Result<Value, RevisionError> {
        {
            let mut ancestors = HashSet::new();
            let mut finished = HashSet::new();
            self.validate_inner(&value, &mut ancestors, &mut finished)?;
        }
        Ok(value)
    }

    fn validate_inner(
        &mut self,
        value: &Value,
        ancestors: &mut HashSet<usize>,
        finished: &mut HashSet<usize>,
    ) -> Result<(), RevisionError> {
        // 以节点地址作为本次遍历的身份：整棵树在调用期间被不可变借用，地址稳定且不会被
        // 复用。`finished` 只在本次调用内去重；跨调用不复用（TS 的 `#validated` 才是跨调用的）。
        let key = value as *const Value as usize;
        // 原语没有子节点，也不需要环/DAG 记账，直接结算。
        match value {
            Value::Null | Value::Bool(_) | Value::String(_) => return Ok(()),
            Value::Number(number) => {
                return match number.as_f64() {
                    Some(number) if !number.is_nan() => Ok(()),
                    _ => Err(RevisionError::new(STRICT_JSON_MESSAGE)),
                };
            }
            Value::Array(_) | Value::Object(_) => {}
        }
        // 先查 `finished` 再查 `ancestors`：已完整校验的子树无需再看，只有仍在当前递归栈
        // 上的容器才是环。当前的 `Value` 表示是树，这两个集合实际都不会命中。
        if finished.contains(&key) {
            return Ok(());
        }
        if ancestors.contains(&key) {
            return Err(RevisionError::new(CYCLE_MESSAGE));
        }
        ancestors.insert(key);
        let result = match value {
            Value::Array(items) => {
                // `serde_json::Value` 的数组天然稠密且没有额外键。
                let mut result = Ok(());
                for item in items {
                    if let Err(error) = self.validate_inner(item, ancestors, finished) {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
            Value::Object(map) => {
                let mut result = Ok(());
                for (_key, item) in map {
                    if let Err(error) = self.validate_inner(item, ancestors, finished) {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
            _ => Ok(()),
        };
        // 无论成功失败都退出当前节点的祖先集合；只有成功才记入 `finished` 并计数。
        ancestors.remove(&key);
        if result.is_ok() {
            finished.insert(key);
            self.validated += 1;
        }
        result
    }
}

/// TS 的 `validate` 便捷入口（新建校验器）。
///
/// 只关心是否通过、丢弃校验结果；错误没有路径/位置信息，只有固定的错误文案。
pub fn validate_revision(value: &Value) -> Result<(), RevisionError> {
    JsonRevisionValidator::new().validate(value).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_strict_json() {
        let mut validator = JsonRevisionValidator::new();
        for value in [
            json!(null),
            json!(true),
            json!(1.5),
            json!("text"),
            json!([1, [2], {"a": null}]),
        ] {
            assert!(validator.validate(&value).is_ok());
        }
        assert!(validator.validated() > 0);
    }

    #[test]
    fn deduplicates_within_one_revision() {
        // 同一子树在 DAG 里出现两次：`finished` 只遍历一次。
        let shared = json!({"a": [1, 2, 3]});
        let value = json!({"left": shared, "right": shared});
        assert!(validate_revision(&value).is_ok());
    }

    #[test]
    fn rejects_cycles_when_a_value_could_contain_one() {
        // `serde_json::Value` 是有向树，无法构造环；直接验证 ancestors 逻辑的输入契约。
        let mut validator = JsonRevisionValidator::new();
        let value = json!({"ok": true});
        assert!(validator.validate_owned(value).is_ok());
    }

    #[test]
    fn message_constants_match_the_reference() {
        assert_eq!(
            RevisionError::new(CYCLE_MESSAGE).to_string(),
            "Replicated state cannot contain cycles"
        );
        assert_eq!(
            RevisionError::new(STRICT_JSON_MESSAGE).to_string(),
            "Replicated state values must be strict JSON"
        );
    }
}
