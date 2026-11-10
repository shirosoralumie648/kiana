//! `testing/assertions.ts`：把 Vitest/Jest 形状的 `expect` 适配成 conformance 断言集合。
//!
//! 文档注释逐字："Adapts a Vitest/Jest-compatible `expect` function without importing either runner."
//!
//! Rust 侧没有可调用的 `expect(actual)` 动态对象，因此把 TS 的两层结构拆成两个 trait：
//! [`ExpectLike`]（`expect(actual, message?)` 与 `expect(promise).rejects.toThrow(...)`）和
//! [`ExpectResult`]（`toBe`/`toBeGreaterThan`/`toEqual`/`toMatchObject`/`toBeTruthy`）。
//! [`create_expect_assertions`] 与 TS 版一样只做转发：`strictEqual` 不传 message，`ok` 传。
//!
//! 另外提供 Rust 测试可直接使用的 [`PanicAssertions`] 与 [`PanicExpect`]（TS 侧由 Vitest 提供
//! `expect`；Rust 没有全局 runner，因此给出一个会 panic 的默认实现）。
//!
//! # Rust 侧的两种使用方式
//!
//! - 有测试框架的调用方实现 [`ExpectLike`]（包装一个记录型 expect，或转发到 Vitest 风格的
//!   实现），再用 [`create_expect_assertions`] 适配成套件需要的
//!   [`StorageConformanceAssertions`]；
//! - 没有 runner 的 `#[test]`/`#[tokio::test]` 直接使用 [`PanicAssertions`]。它不需要注入
//!   [`ExpectLike`]，失败时以 actual/expected 的 JSON 触发 panic。
//!
//! # 行为差异与边界
//!
//! `PanicAssertions` 的 `strict_equal` 与 `deep_equal` 都用 `serde_json::Value` 的结构相等：
//! Rust 侧没有“同一个 JSON 值对象”的引用同一性概念，所以两者行为相同。若调用方需要保留
//! `toBe`（Object.is/引用同一性）与 `toEqual`（结构相等）的差异，应注入自己的 [`ExpectLike`]，
//! 走 [`create_expect_assertions`] 路径。
//!
//! `create_expect_assertions` 的 `greater_than` 会把 actual `f64` 先转成 JSON 数字。`NaN` 与
//! 无穷无法表示为 JSON 数字，会退化为 `null`（见 `number_value`），因此不要用它比较非有限值；
//! `PanicAssertions` 不经过这层转换，直接比较 `actual > expected`（`NaN` 比较为 false），
//! `PanicExpect` 则用 `partial_cmp`。
//!
//! # 失败消息
//!
//! 有 message 时，[`PanicExpect`] 的 panic 文本是 `message: detail`；[`PanicAssertions::ok`]
//! 直接使用 message。其余断言只带 actual/expected 的 JSON（`ok` 没有 message 时是固定文本）。
//! `rejects` 只有在 operation 返回 `Err` 且错误消息包含期望子串时才算通过。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::testing::types::{ConformanceFuture, StorageConformanceAssertions};

/// `expect(promise).rejects.toThrow(...)` 返回的 future。
pub type RejectsFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// TS 内部类型 `ExpectResult`：`expect(actual)` 返回的断言对象。
///
/// 每个方法对应一种断言；失败方式是实现细节（[`PanicExpect`] panic，记录型实现可以只记录）。
/// 返回值的生命周期受 `&self`（以及 message）的借用约束，不能在其借用结束后继续使用。
pub trait ExpectResult: Send + Sync {
    /// `toBe(expected)`。
    fn to_be(&self, expected: &Value);
    /// `toBeGreaterThan(expected)`。
    fn to_be_greater_than(&self, expected: f64);
    /// `toEqual(expected)`。
    fn to_equal(&self, expected: &Value);
    /// `toMatchObject(expected)`。
    fn to_match_object(&self, expected: &Value);
    /// `toBeTruthy()`。
    fn to_be_truthy(&self);
}

/// `export type ExpectLike = (actual: unknown, message?: string) => unknown;`
///
/// `rejects` 是 `expect(operation).rejects.toThrow(messageIncludes)` 的显式形式（operation 在
/// TS 里是一个 Promise；Rust 里是尚未轮询的 future）。
///
/// [`create_expect_assertions`] 每执行一次同步断言就调用一次 [`ExpectLike::expect`]，所以实现
/// 不应假设“一个测试只取一个结果对象”；返回的 [`ExpectResult`] 也只是短期借用。
pub trait ExpectLike: Send + Sync {
    /// `expect(actual, message?)`。
    fn expect<'a>(&'a self, actual: &Value, message: Option<&'a str>)
    -> Box<dyn ExpectResult + 'a>;
    /// `await expect(operation).rejects.toThrow(messageIncludes)`。
    fn rejects<'a>(
        &'a self,
        operation: ConformanceFuture<'a>,
        message_includes: &'a str,
    ) -> RejectsFuture<'a>;
}

/// `Export createExpectAssertions(expect)`：逐个方法转发到 `expect(actual)`。
///
/// 返回 `Arc<dyn StorageConformanceAssertions>`，可直接放进
/// [`StorageConformanceOptions::assertions`](crate::testing::types::StorageConformanceOptions::assertions)
/// 或 env 套件的同名字段。`ok` 传 message，其余方法都不传；每次断言都会新建一个
/// [`ExpectResult`]，实现方无需（也不应）缓存它。
pub fn create_expect_assertions(
    expect: Arc<dyn ExpectLike>,
) -> Arc<dyn StorageConformanceAssertions> {
    Arc::new(ExpectAssertions { expect })
}

struct ExpectAssertions {
    expect: Arc<dyn ExpectLike>,
}

fn number_value(value: f64) -> Value {
    match serde_json::Number::from_f64(value) {
        Some(number) => Value::Number(number),
        // `ok`/`greaterThan` 的 actual 不参与 `toBeGreaterThan` 的比较（它只看捕获的值），
        // 非有限数无法表示为 JSON，用 null 占位即可。
        None => Value::Null,
    }
}

#[async_trait]
impl StorageConformanceAssertions for ExpectAssertions {
    fn ok(&self, value: bool, message: Option<&str>) {
        self.expect
            .expect(&Value::Bool(value), message)
            .to_be_truthy();
    }

    fn strict_equal(&self, actual: &Value, expected: &Value) {
        self.expect.expect(actual, None).to_be(expected);
    }

    fn deep_equal(&self, actual: &Value, expected: &Value) {
        self.expect.expect(actual, None).to_equal(expected);
    }

    fn partial_deep_equal(&self, actual: &Value, expected: &Value) {
        self.expect.expect(actual, None).to_match_object(expected);
    }

    fn greater_than(&self, actual: f64, expected: f64) {
        self.expect
            .expect(&number_value(actual), None)
            .to_be_greater_than(expected);
    }

    async fn rejects<'a>(&self, operation: ConformanceFuture<'a>, message_includes: &str) {
        self.expect.rejects(operation, message_includes).await;
    }
}

// ---------------------------------------------------------------------------
// Rust 测试用的默认实现
// ---------------------------------------------------------------------------

/// JavaScript 的真值判断（`toBeTruthy`）。
///
/// 假值：`null`、`false`、数值 0、空字符串；空数组与空对象为真（JS 中对象总是真值）。
/// 数值按 `as_f64` 比较，`-0.0` 与 `0.0` 相等，因此同样是假。
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(boolean) => *boolean,
        Value::Number(number) => match number.as_f64() {
            Some(number) => number != 0.0 && !number.is_nan(),
            None => true,
        },
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Vitest `toMatchObject` 的匹配规则：对象按 key 递归做子集匹配，数组等长逐个递归匹配，
/// 其他值严格相等。
///
/// - 对象只检查 `expected` 里出现的键，`actual` 多出的键被忽略；
/// - 数组要求长度相同且逐项递归匹配，不是前缀匹配（`tests/storage_conformance.rs` 锁定）；
/// - `null` 与“键不存在”不同：期望 `null` 时实际值也必须是 `null`。
pub fn matches_subset(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|found| matches_subset(found, value))
        }),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected.iter())
                    .all(|(actual, expected)| matches_subset(actual, expected))
        }
        _ => actual == expected,
    }
}

fn format_failure(message: &Option<String>, detail: String) -> String {
    match message {
        Some(message) => format!("{message}: {detail}"),
        None => detail,
    }
}

fn fail(message: &Option<String>, detail: String) -> ! {
    panic!("{}", format_failure(message, detail))
}

/// Rust 测试用的 `expect`：失败时 panic，消息包含 actual/expected 的 JSON。
///
/// `expect()` 会把 actual 克隆进结果对象、把 message 转成 `String`，因此结果可以活到借用结束；
/// `rejects` 会先 await operation：`Err` 时检查错误消息包含子串，`Ok` 或子串不匹配都 panic
/// （因为用例期望的是 reject）。
#[derive(Debug, Default, Clone, Copy)]
pub struct PanicExpect;

struct PanicExpectResult {
    actual: Value,
    message: Option<String>,
}

impl ExpectResult for PanicExpectResult {
    fn to_be(&self, expected: &Value) {
        if &self.actual != expected {
            fail(
                &self.message,
                format!("expected {} to be {}", self.actual, expected),
            );
        }
    }

    fn to_be_greater_than(&self, expected: f64) {
        let actual = self.actual.as_f64().unwrap_or(f64::NAN);
        let greater = actual
            .partial_cmp(&expected)
            .is_some_and(|ordering| ordering == std::cmp::Ordering::Greater);
        if !greater {
            fail(
                &self.message,
                format!("expected {} to be greater than {expected}", self.actual),
            );
        }
    }

    fn to_equal(&self, expected: &Value) {
        if &self.actual != expected {
            fail(
                &self.message,
                format!("expected {} to equal {}", self.actual, expected),
            );
        }
    }

    fn to_match_object(&self, expected: &Value) {
        if !matches_subset(&self.actual, expected) {
            fail(
                &self.message,
                format!("expected {} to match object {}", self.actual, expected),
            );
        }
    }

    fn to_be_truthy(&self) {
        if !is_truthy(&self.actual) {
            fail(
                &self.message,
                format!("expected {} to be truthy", self.actual),
            );
        }
    }
}

impl ExpectLike for PanicExpect {
    fn expect<'a>(
        &'a self,
        actual: &Value,
        message: Option<&'a str>,
    ) -> Box<dyn ExpectResult + 'a> {
        Box::new(PanicExpectResult {
            actual: actual.clone(),
            message: message.map(str::to_owned),
        })
    }

    fn rejects<'a>(
        &'a self,
        operation: ConformanceFuture<'a>,
        message_includes: &'a str,
    ) -> RejectsFuture<'a> {
        Box::pin(async move {
            match operation.await {
                Ok(value) => panic!(
                    "expected operation to reject with a message containing {message_includes:?}, but it resolved to {value}"
                ),
                Err(error) => {
                    let message = error.to_string();
                    assert!(
                        message.contains(message_includes),
                        "expected error message {message:?} to contain {message_includes:?}"
                    );
                }
            }
        })
    }
}

/// Rust 测试用的默认 conformance 断言集合：失败时以 actual/expected 的 JSON panic。
///
/// 等价于 `createExpectAssertions(new VitestExpect())` 的行为；TS 侧由 Vitest 提供 `expect`，
/// Rust 没有全局 runner，因此这里给出一个可直接使用的实现。它不依赖外部注入的
/// [`ExpectLike`]（`rejects` 复用 [`PanicExpect`] 的实现）：`strict_equal` 与 `deep_equal`
/// 都做 `serde_json::Value` 的结构比较（见模块文档的“行为差异”）。
#[derive(Debug, Default, Clone, Copy)]
pub struct PanicAssertions;

#[async_trait]
impl StorageConformanceAssertions for PanicAssertions {
    fn ok(&self, value: bool, message: Option<&str>) {
        if !value {
            panic!("{}", message.unwrap_or("expected condition to hold"));
        }
    }

    fn strict_equal(&self, actual: &Value, expected: &Value) {
        if actual != expected {
            panic!("expected {actual} to be {expected}");
        }
    }

    fn deep_equal(&self, actual: &Value, expected: &Value) {
        if actual != expected {
            panic!("expected {actual} to equal {expected}");
        }
    }

    fn partial_deep_equal(&self, actual: &Value, expected: &Value) {
        if !matches_subset(actual, expected) {
            panic!("expected {actual} to match object {expected}");
        }
    }

    fn greater_than(&self, actual: f64, expected: f64) {
        assert!(
            actual > expected,
            "expected {actual} to be greater than {expected}"
        );
    }

    async fn rejects<'a>(&self, operation: ConformanceFuture<'a>, message_includes: &str) {
        PanicExpect.rejects(operation, message_includes).await;
    }
}
