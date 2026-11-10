//! `errors.ts`：`CHORD_ERROR_CODES`、[`ChordError`] 与构造函数。
//!
//! TS 的 `ChordError extends Error`，`message` 是 `` `${code}${reason ? "/" + reason : ""}: ${message}` ``，
//! 另有 `code`/`reason`/`retryable`/`details`/`cause`。Rust 侧用 [`ChordError`] 保存 `code`/`message`/
//! `reason`/`details`；`retryable` 由 [`ChordError::retryable`] 从 `code` 计算，`cause` 没有对应字段
//! （[`as_chord_error`] 包装外部错误时不保留错误链）。`Display` 输出**完整**消息
//! （稳定契约，改动需同步测试），原始消息在 [`ChordError::message`]。
//!
//! TS 的 `error.name` 在 Rust 侧没有实例字段；[`ChordError::NAME`] 只是保留 `"ChordError"` 这个
//! 字符串的常量，不参与 `Display`/serde，调用方需要时自行读取。
//!
//! # 错误如何流经本 crate
//!
//! - [`ChordError::new`] 只填 `code`/`message`；`reason`/`details` 用 builder 方法追加。
//! - 错误以值类型 `Result<T, ChordError>` 传播。`lib.rs` 只为 `clippy::result_large_err` 开了
//!   allow：错误结构体的体积超过该 lint 的阈值，而 TS 的错误是引用类型、没有这个成本；装箱成
//!   `Box<ChordError>` 则会改变本 crate 已冻结的公开签名。
//! - 与外部错误互操作时，[`as_chord_error`] 保留 `ChordError` 原文，其它错误统一变成
//!   `unavailable` 并拼上上下文；[`is_chord_error`] 只判断类型与（可选的）错误码。
//!
//! # 错误码的顺序
//!
//! [`ChordErrorCode::ALL`] 按 TS 的 `CHORD_ERROR_CODES` 顺序列出；每个码的 `serde` 序列化
//! 结果必须等于 [`ChordErrorCode::as_str`]，`errors.rs` 的单元测试 `codes_serialize_snake_case`
//! 逐项校验这一点（`#[serde(rename_all = "snake_case")]` 与手写字符串因此不能分叉）。

use serde::{Deserialize, Serialize};

/// 调用者可见的错误码（顺序与 TS 的 `CHORD_ERROR_CODES` 一致；`Type.Enum` 的校验顺序也依赖它）。
///
/// 每个码的完整语义表见 `spec/chord-core.md` §3.2（取自 TS 的 guide 表格）；这里只给一行摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChordErrorCode {
    /// 调用方的 `ctx` 被取消。
    Aborted,
    /// provider 契约不覆盖 consumer 契约，且没有 adapter 可用。
    ContractMismatch,
    /// 工厂或方法拒绝该 principal。
    Forbidden,
    /// 对象在仍被引用时被移除；调用方应重新 `get`。
    Gone,
    /// 输入、参数或结果未通过 schema 校验。
    Invalid,
    /// 请求的成员不在当前 variant 里。
    NotInVariant,
    /// 在工厂里使用了尚未就绪的资源/引用，或在首次 hydration 前读取状态。
    NotReady,
    /// 引用已经被释放。
    Released,
    /// 该 root 的 code version 在宿主上已不可用；可重试。
    Superseded,
    /// 目标宿主无法连接或启动；`reason` 为 `ready_timeout` 时表示等待引用就绪超时。
    Unavailable,
    /// 旧 root 访问了期间已迁移的对象；应以新 root 重试。
    UpgradeConflict,
    /// 浏览器侧的契约已过期，需要重新加载它的 web facets。
    UpgradeRequired,
}

impl ChordErrorCode {
    /// 与 TS 数组同序。
    pub const ALL: [ChordErrorCode; 12] = [
        ChordErrorCode::Aborted,
        ChordErrorCode::ContractMismatch,
        ChordErrorCode::Forbidden,
        ChordErrorCode::Gone,
        ChordErrorCode::Invalid,
        ChordErrorCode::NotInVariant,
        ChordErrorCode::NotReady,
        ChordErrorCode::Released,
        ChordErrorCode::Superseded,
        ChordErrorCode::Unavailable,
        ChordErrorCode::UpgradeConflict,
        ChordErrorCode::UpgradeRequired,
    ];

    /// 线上用的字符串（`serde` 的 `rename_all` 结果必须与它一致，测试会校验）。
    pub fn as_str(self) -> &'static str {
        match self {
            ChordErrorCode::Aborted => "aborted",
            ChordErrorCode::ContractMismatch => "contract_mismatch",
            ChordErrorCode::Forbidden => "forbidden",
            ChordErrorCode::Gone => "gone",
            ChordErrorCode::Invalid => "invalid",
            ChordErrorCode::NotInVariant => "not_in_variant",
            ChordErrorCode::NotReady => "not_ready",
            ChordErrorCode::Released => "released",
            ChordErrorCode::Superseded => "superseded",
            ChordErrorCode::Unavailable => "unavailable",
            ChordErrorCode::UpgradeConflict => "upgrade_conflict",
            ChordErrorCode::UpgradeRequired => "upgrade_required",
        }
    }

    /// `RETRYABLE`：只有 `superseded` 与 `upgrade_conflict`。
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ChordErrorCode::Superseded | ChordErrorCode::UpgradeConflict
        )
    }
}

/// 错误码内更细的原因（`invalid/args`、`unavailable/unknown_type` 等）；自由字符串。
pub type ChordErrorReason = String;

/// `ChordError`。
#[derive(Debug, Clone, PartialEq)]
pub struct ChordError {
    /// 错误码；决定 [`ChordError::retryable`]，也是调用方分支恢复的主要依据。
    pub code: ChordErrorCode,
    /// 未加前缀的原始消息（TS 的 `super(message)` 参数）。
    pub message: String,
    /// 错误码内更细的原因（`invalid/args`、`unavailable/unknown_type` 等）；自由字符串。
    pub reason: Option<ChordErrorReason>,
    /// 机器可读的附加信息；例如 schema 校验失败时保存前几条错误（`schema::assert_valid`）。
    pub details: Option<serde_json::Value>,
}

impl ChordError {
    /// TS 的 `error.name`。
    pub const NAME: &'static str = "ChordError";

    /// `new ChordError(code, message)`。
    pub fn new(code: ChordErrorCode, message: impl Into<String>) -> Self {
        ChordError {
            code,
            message: message.into(),
            reason: None,
            details: None,
        }
    }

    /// 带上 `reason`。
    pub fn with_reason(mut self, reason: impl Into<ChordErrorReason>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// 带上 `details`。
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// TS 的 `error.retryable`：`true` 当且仅当错误码是 `superseded` 或 `upgrade_conflict`。
    pub fn retryable(&self) -> bool {
        self.code.retryable()
    }

    /// TS 的 `error.message`：`code[/reason]: message`。
    pub fn full_message(&self) -> String {
        match &self.reason {
            Some(reason) => format!("{}/{reason}: {}", self.code.as_str(), self.message),
            None => format!("{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.full_message())
    }
}

impl std::error::Error for ChordError {}

/// `isChordError(error, code?)` 的 Rust 等价物。
///
/// TS 接受任意 `unknown`；Rust 这里只接受 `&dyn Error`，因此调用方要先把错误当成 trait object
/// 传入（如 `(&err as &dyn std::error::Error)`）。非 [`ChordError`] 返回 `false`；`code` 为 `None`
/// 时只判断类型。
pub fn is_chord_error(
    value: &(dyn std::error::Error + 'static),
    code: Option<ChordErrorCode>,
) -> bool {
    match value.downcast_ref::<ChordError>() {
        Some(e) => code.is_none_or(|c| c == e.code),
        None => false,
    }
}

/// `forbidden(message = "forbidden")`。
///
/// Rust 版本没有缺省参数：调用方要传 `"forbidden"` 才能得到缺省文案。
pub fn forbidden(message: impl Into<String>) -> ChordError {
    ChordError::new(ChordErrorCode::Forbidden, message)
}

/// `invalid(reason, message)`。
///
/// TS 还有第三个参数 `details`；Rust 版本没有该参数，需要时链式调用
/// [`ChordError::with_details`]（`schema::assert_valid` 就是这么构造错误的）。
pub fn invalid(reason: impl Into<ChordErrorReason>, message: impl Into<String>) -> ChordError {
    ChordError::new(ChordErrorCode::Invalid, message).with_reason(reason)
}

/// `asChordError(error, context)`：保留 `ChordError` 的 code，其余包成 `unavailable`。
///
/// 非 ChordError 时消息为 `"{context}: {error}"`（`{error}` 是该错误的 `Display`）。TS 会把原
/// 错误挂到新错误的 `cause`；Rust 的 [`ChordError`] 没有 `cause` 字段，错误链在这里断开，
/// 需要保留原始错误类型的调用方应自行记录。
pub fn as_chord_error(error: &(dyn std::error::Error + 'static), context: &str) -> ChordError {
    match error.downcast_ref::<ChordError>() {
        Some(e) => e.clone(),
        None => ChordError::new(ChordErrorCode::Unavailable, format!("{context}: {error}")),
    }
}

/// `runAll(steps, message)` 的聚合结果：多个步骤失败时使用。
///
/// TS 抛 `AggregateError`；Rust 用这个类型（`Display` = `message`，各步错误保存在 `errors`
/// 里，调用方可逐个 downcast）。恰好一个步骤失败时不会用它，原错误直接返回。
#[derive(Debug)]
pub struct ChordAggregateError {
    /// 聚合错误的整体消息（TS `AggregateError` 的 `message`），`Display` 只输出它。
    pub message: String,
    /// 按步骤执行顺序收集的失败；保持原始错误的 trait object，可 downcast。
    pub errors: Vec<Box<dyn std::error::Error + Send + Sync>>,
}

impl std::fmt::Display for ChordAggregateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ChordAggregateError {}

/// 同步版本的 `runAll`：按顺序执行每一步，**不因失败而停下**，最后统一汇报。
///
/// - 全部成功 → `Ok(())`；
/// - 恰好一个失败 → 原样返回该错误（不包 `ChordAggregateError`），错误顺序即步骤顺序；
/// - 多个失败 → 返回 [`ChordAggregateError`]，`message` 是传入的 `message`，`errors` 按步骤顺序。
///
/// TS 的 `runAll` 是 `async` 的；当前 crate 只有这个同步版本，异步调用方需自行 `await` 各步骤后
/// 按同一规则聚合。`steps` 以 `IntoIterator` 接收，元素是 `FnOnce`，所以每个步骤最多执行一次。
pub fn run_all_sync(
    steps: impl IntoIterator<
        Item = Box<dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    >,
    message: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut errors = Vec::new();
    for step in steps {
        if let Err(error) = step() {
            errors.push(error);
        }
    }
    match errors.len() {
        0 => Ok(()),
        1 => Err(errors.pop().unwrap()),
        _ => Err(Box::new(ChordAggregateError {
            message: message.to_owned(),
            errors,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_matches_typescript() {
        let e = ChordError::new(ChordErrorCode::Invalid, "args").with_reason("args");
        assert_eq!(e.to_string(), "invalid/args: args");
        let e = ChordError::new(ChordErrorCode::Gone, "gone");
        assert_eq!(e.to_string(), "gone: gone");
    }

    #[test]
    fn codes_serialize_snake_case() {
        for code in ChordErrorCode::ALL {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
        }
        assert!(ChordErrorCode::Superseded.retryable());
        assert!(!ChordErrorCode::Invalid.retryable());
    }
}
