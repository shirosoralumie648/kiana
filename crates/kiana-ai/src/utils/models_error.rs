//! `utils/models-error.ts`——带 code 的 `ModelsError` 与 cause 拼接。
//!
//! `ModelsError` 本体是冻结的 [`crate::error::ModelsError`]（`code` + `message` +
//! `cause_detail`）。本模块只移植 TS 的两段逻辑：
//! - `withCauseDetail(message, cause)`：把 cause 的文本拼进 message（调用方只展示
//!   `error.message`），空细节或已包含时去重；
//! - 各 code 的字符串名（TS 的 `ModelsErrorCode` 六值）。
//!
//! **与骨架的差异（已随 `error.rs` 更新）**：Rust 的
//! [`ModelsErrorCode`] 现在与 TS 的
//! `"model_source" | "model_validation" | "provider" | "stream" | "auth" | "oauth"`
//! 一一对应，因此 [`TsModelsErrorCode`] 到 `ModelsErrorCode` 的转换是恒等映射
//! （见 [`TsModelsErrorCode::frozen_variant`]）。
//!
//! TS: `utils/models-error.ts`
//!
//! # 为什么还需要 `TsModelsErrorCode`
//!
//! [`crate::error::ModelsErrorCode`] 已经是与 TS 六值一一对应的冻结枚举；本模块保留一份
//! 同形枚举，调用点按 TS 的类型写法传值（如 `TsModelsErrorCode::Provider`），再由
//! [`TsModelsErrorCode::frozen_variant`] 恒等转换成冻结枚举。两者取值必须同步：
//! `tests/models_error.rs` 对 `error.rs` 的六个字面量做双向映射断言，新增或改名只改
//! 一边会让这里的分派与 TS 漂移。
//!
//! # 失败与边界
//!
//! 本模块不返回 `Result`：`with_cause_detail` 与 `models_error` 都是纯函数，输入不合法
//! （空 cause、已被 message 包含的细节）表现为“不追加”，而不是错误。`models_error`
//! 会把追加过的细节另存到 `cause_detail`，调用方可据此区分“原始 message”。

use crate::error::{ModelsError, ModelsErrorCode};
use crate::utils::diagnostics::{ThrownValue, format_thrown_value};

/// TS 的 `ModelsErrorCode`（六个字面量，逐字）。
///
/// 这是为逐行移植保留的 TS 同形枚举；错误结构本身使用
/// [`crate::error::ModelsErrorCode`]，两者通过 [`TsModelsErrorCode::frozen_variant`]
/// 恒等转换。
///
/// # 不变量
///
/// [`TsModelsErrorCode::as_str`] 与 [`TsModelsErrorCode::parse`] 必须互逆，且六个字面量
/// 与 `error.rs` 的冻结枚举一致；本文件的 `code_literals_round_trip` 单测锁定本枚举，
/// `tests/models_error.rs` 锁定冻结枚举与序列化结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TsModelsErrorCode {
    /// `"model_source"`
    ModelSource,
    /// `"model_validation"`
    ModelValidation,
    /// `"provider"`
    Provider,
    /// `"stream"`
    Stream,
    /// `"auth"`
    Auth,
    /// `"oauth"`
    OAuth,
}

impl TsModelsErrorCode {
    /// TS 的字面量。
    ///
    /// 返回值是写进会话文件与 CLI 输出的字符串常量（当前稳定契约，改动需同步测试）。
    /// 它经 [`TsModelsErrorCode::frozen_variant`] 进入 `ModelsError.code`，最终由
    /// [`crate::error::ModelsErrorCode`] 的序列化写进会话
    /// 文件与 CLI 输出，因此不要改成 Rust 的枚举名（如 `"ModelSource"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            TsModelsErrorCode::ModelSource => "model_source",
            TsModelsErrorCode::ModelValidation => "model_validation",
            TsModelsErrorCode::Provider => "provider",
            TsModelsErrorCode::Stream => "stream",
            TsModelsErrorCode::Auth => "auth",
            TsModelsErrorCode::OAuth => "oauth",
        }
    }

    /// 解析 TS 字面量。
    ///
    /// 精确匹配（区分大小写），未知值返回 `None`；没有默认 code，调用方无法把它误当作
    /// 某个已知类别。
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "model_source" => TsModelsErrorCode::ModelSource,
            "model_validation" => TsModelsErrorCode::ModelValidation,
            "provider" => TsModelsErrorCode::Provider,
            "stream" => TsModelsErrorCode::Stream,
            "auth" => TsModelsErrorCode::Auth,
            "oauth" => TsModelsErrorCode::OAuth,
            _ => return None,
        })
    }

    /// 对应的 [`ModelsErrorCode`]（与 TS 六值一一对应）。
    ///
    /// 当前是恒等映射，没有失败分支；保留显式转换是为了让调用点只依赖本文件的 TS 同形
    /// 枚举，而错误结构继续使用冻结枚举。两个枚举的取值集合必须同时维护。
    pub fn frozen_variant(self) -> ModelsErrorCode {
        match self {
            TsModelsErrorCode::ModelSource => ModelsErrorCode::ModelSource,
            TsModelsErrorCode::ModelValidation => ModelsErrorCode::ModelValidation,
            TsModelsErrorCode::Provider => ModelsErrorCode::Provider,
            TsModelsErrorCode::Stream => ModelsErrorCode::Stream,
            TsModelsErrorCode::Auth => ModelsErrorCode::Auth,
            TsModelsErrorCode::OAuth => ModelsErrorCode::OAuth,
        }
    }
}

/// `withCauseDetail(message, cause)`：把 cause 的细节拼进 message。
///
/// cause 为 `None` 时返回 message；`detail = formatThrownValue(cause).trim()`；detail 为空或
/// `message.contains(detail)` 时返回 message；否则 `` `${message}: ${detail}` ``。
///
/// # 参数与边界
///
/// - `cause` 为 `None` 同时表示 TS 的 `undefined` 与 `null`（两者在 TS 中都不追加）。
/// - 细节先经 [`format_thrown_value`]（错误取 `message || name`、字符串原样、其它值走
///   JS 的 `String(v)`），再按 JS 的 `trim` 去掉首尾空白；错误对象的 `message` 为空时
///   会回退到 `name`，这与本 crate 的 `model_operations::thrown_message` 不同（后者保留
///   空 `message`）。
/// - 去重是**区分大小写的子串**判断而不是相等：message 已含 `"boom"` 时不再追加，
///   含 `"BOOM"` 时仍会追加。
/// - 纯函数：不修改入参；message 为空串且 detail 非空时返回 `": detail"`（与 TS 相同）。
///
/// TS: `utils/models-error.ts#withCauseDetail`
pub fn with_cause_detail(message: &str, cause: Option<&ThrownValue>) -> String {
    let Some(cause) = cause else {
        return message.to_string();
    };
    let detail = kiana_json::js_string::js_trim(&format_thrown_value(cause)).to_string();
    if detail.is_empty() || message.contains(&detail) {
        return message.to_string();
    }
    format!("{message}: {detail}")
}

/// 构造 `ModelsError`（TS 的 `new ModelsError(code, message, { cause })`）。
///
/// `message` 已经拼接了 cause 细节；`cause_detail` 保留同一份细节文本供诊断读取。
///
/// # 行为
///
/// 与 TS 构造函数一致：`message` 走 [`with_cause_detail`]；`cause_detail` 仅在细节确实
/// 被追加时才是 `Some(detail)`，否则为 `None`。本函数总是返回一个值（不校验 code 与
/// message 的语义），调用方负责让 `code` 与场景（provider/auth/oauth/stream…）保持一致。
///
/// TS: `utils/models-error.ts#ModelsError`
pub fn models_error(
    code: TsModelsErrorCode,
    message: &str,
    cause: Option<&ThrownValue>,
) -> ModelsError {
    // 细节在这里被格式化两次的原因：一次用于决定是否追加进 message，一次用于记录
    // cause_detail；两次使用同一套 trim 与“是否已包含”判据，保证 message 与
    // cause_detail 描述的是同一段文本。
    let detail = cause
        .map(|cause| kiana_json::js_string::js_trim(&format_thrown_value(cause)).to_string())
        .filter(|detail| !detail.is_empty() && !message.contains(detail));
    ModelsError {
        code: code.frozen_variant(),
        message: with_cause_detail(message, cause),
        cause_detail: detail,
    }
}

/// `ModelsError` 的名字（`error.name`）。
///
/// 与 [`crate::error::ModelsError::name`] 返回同一字面量；这里是常量形态，便于无法持有
/// 实例的比较/诊断代码使用。当前仓库内只有本文件的测试读取它。
pub const MODELS_ERROR_NAME: &str = "ModelsError";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DiagnosticErrorInfo;

    fn error(message: &str) -> ThrownValue {
        ThrownValue::Error(DiagnosticErrorInfo {
            name: Some("Error".into()),
            message: message.into(),
            stack: None,
            code: None,
        })
    }

    #[test]
    fn code_literals_round_trip() {
        for code in [
            TsModelsErrorCode::ModelSource,
            TsModelsErrorCode::ModelValidation,
            TsModelsErrorCode::Provider,
            TsModelsErrorCode::Stream,
            TsModelsErrorCode::Auth,
            TsModelsErrorCode::OAuth,
        ] {
            assert_eq!(TsModelsErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(TsModelsErrorCode::parse("nope"), None);
    }

    #[test]
    fn appends_cause_detail_once() {
        assert_eq!(with_cause_detail("failed", None), "failed");
        assert_eq!(
            with_cause_detail("failed", Some(&error("boom"))),
            "failed: boom"
        );
        // 已包含细节时不重复。
        assert_eq!(
            with_cause_detail("failed: boom", Some(&error("boom"))),
            "failed: boom"
        );
        // 空细节（空白）不拼接。
        assert_eq!(with_cause_detail("failed", Some(&error("   "))), "failed");
        // 字符串 cause 原样拼接。
        assert_eq!(
            with_cause_detail("failed", Some(&ThrownValue::Text("boom".into()))),
            "failed: boom"
        );
    }

    #[test]
    fn builds_models_error_with_detail() {
        let error = models_error(
            TsModelsErrorCode::OAuth,
            "OAuth refresh failed for anthropic",
            Some(&error("401")),
        );
        assert_eq!(error.message, "OAuth refresh failed for anthropic: 401");
        assert_eq!(error.cause_detail.as_deref(), Some("401"));
        assert_eq!(error.code, ModelsErrorCode::OAuth);
        assert_eq!(MODELS_ERROR_NAME, "ModelsError");
    }
}
