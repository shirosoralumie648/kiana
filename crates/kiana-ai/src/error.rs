//! crate 级错误类型。
//!
//! TS 的 `kiana-ai` 大多用 `Error` + 消息文本；`ModelsError`（`utils/models-error.ts`）是唯一
//! 有结构化字段的错误。Rust 侧：
//! - [`AiError`]：`kiana-ai` 内部与跨 crate 的统一错误，`Display` **逐字**等于 TS 的 `error.message`
//!   （它会被拼进会话文件、UI 错误框与诊断）。因此不使用 `thiserror` 的字段展开格式，
//!   而是显式实现 `Display`。
//! - 具体分类（鉴权、HTTP、解析、中止）用 [`AiErrorKind`]，调用方按 kind 分支。
//!
//! # 错误在系统里的流动
//!
//! 1. 底层产生具体错误：[`crate::http::HttpError`]（网络/HTTP）、`ModelsError`
//!    （模型目录/鉴权）、`AuthError`（OAuth 交互）等。能用 JS 文本保真的路径尽量保留原文。
//! 2. 跨 crate 边界统一转成 [`AiError`]：`kind` 供程序分支，`message` 供用户展示，
//!    `name` 保留 TS 的 `error.name`，`detail` 只放诊断信息。
//! 3. [`ModelsError`] → [`AiError`] 的映射在 `crate::models::ai_error_from_models_error`：
//!    `auth`/`oauth` → [`AiErrorKind::Auth`]，其余 → [`AiErrorKind::Other`]，原始 code 字符串
//!    放进 `detail.code`，因此上层仍能按 TS 错误码分支。
//!
//! # 序列化边界
//!
//! - [`AiError`] 不实现 `Serialize`/`Deserialize`：它不进入持久化格式；进入会话文件的是
//!   `message` 字符串本身。
//! - [`ModelsErrorCode`] 实现 `Serialize`/`Deserialize`，输出/接受 TS 的六个字面量；未知
//!   字面量在反序列化时报错，不会静默降级成某个默认值。
//!
//! # 失败模式
//!
//! `Display` 永远只输出 `message`，即使 `detail` 存在；因此日志若要保留诊断信息，必须显式
//! 读取 `detail`。修改 `Display` 的输出会破坏与 TS 文本的字节级对齐。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiErrorKind {
    /// 缺少或无效的凭据。
    Auth,
    /// 请求构造/参数问题。
    Request,
    /// 网络或 HTTP 失败。
    Transport,
    /// 响应解析失败（SSE、JSON、schema）。
    Parse,
    /// 调用方中止。
    Aborted,
    /// 模型/提供商不支持该操作。
    Unsupported,
    /// 其他。
    Other,
}

/// `kiana-ai` 的统一错误。
///
/// 不变量：`Display` 只输出 `message`（逐字等于 TS `error.message`），因此不要改用
/// `thiserror` 的 `{field:?}` 展开格式；`kind`/`name`/`detail` 都不参与展示。
#[derive(Debug, Clone, PartialEq)]
pub struct AiError {
    /// 程序分支用的分类（不进入展示文本）。
    pub kind: AiErrorKind,
    /// 面向用户展示的错误文本（当前稳定契约，改动需同步测试）。
    pub message: String,
    /// TS 的 `error.name`（`Error`、`AbortError`、`ModelsError`…）。
    pub name: String,
    /// 诊断用细节（不进入面向用户的文本）。
    pub detail: Option<serde_json::Value>,
}

impl AiError {
    /// 构造 `name = "Error"`、`detail = None` 的错误；`message` 原样保存。
    pub fn new(kind: AiErrorKind, message: impl Into<String>) -> Self {
        AiError {
            kind,
            message: message.into(),
            name: "Error".into(),
            detail: None,
        }
    }

    /// 设置 TS 的 `error.name`（如 `AbortError`、`ModelsError`）。
    ///
    /// 只影响名字，不改 `kind`；兼容层/调用方可以按名字匹配 TS 分支。
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// 附加诊断 JSON；不会进入 `Display`，仅供日志/诊断读取。
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// `AbortError`：TS 在 `signal.reason` 缺省时构造的名字。
    pub fn aborted(message: impl Into<String>) -> Self {
        AiError::new(AiErrorKind::Aborted, message).with_name("AbortError")
    }

    /// 是否属于用户/调用方取消。
    ///
    /// 只按 `kind` 判断：仅用 [`AiError::with_name`] 把名字改成 `"AbortError"`、但 `kind`
    /// 不是 [`AiErrorKind::Aborted`] 的错误**不会**被判为中止；反之，`kind == Aborted` 的
    /// 错误无论名字是什么都算中止。
    pub fn is_aborted(&self) -> bool {
        self.kind == AiErrorKind::Aborted
    }
}

// 显式实现而不是 `#[derive(thiserror::Error)]`：保证 `Display` 严格等于 `message`，
// 不附加任何字段名/错误链（见模块文档的字节级保真说明）。
impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AiError {}

// serde_json 的错误文本是 Rust 自己的描述，不保证与 V8 `JSON.parse` 的文本逐字一致；
// 需要 JS 文本保真的解析路径应使用 `kiana_json`（见下一个 `From` 实现）。
impl From<serde_json::Error> for AiError {
    fn from(e: serde_json::Error) -> Self {
        AiError::new(AiErrorKind::Parse, e.to_string())
    }
}

// `ParseError.message` 与 V8 `SyntaxError.message`（或宽松解析的错误文本）逐字一致
// （见 `kiana-json` 的 `ParseError` 文档），因此直接取 `message`；`position` 不进入消息。
impl From<kiana_json::ParseError> for AiError {
    fn from(e: kiana_json::ParseError) -> Self {
        AiError::new(AiErrorKind::Parse, e.message)
    }
}

// `HttpError.message` 已经等于 TS `error.message`；`status` 在这里被丢弃（`AiError` 没有
// 对应字段，调用方若需要状态码应直接使用 `HttpError`）。
impl From<crate::http::HttpError> for AiError {
    fn from(e: crate::http::HttpError) -> Self {
        AiError::new(AiErrorKind::Transport, e.message)
    }
}

/// `ModelsError`（`utils/models-error.ts`）。
///
/// TS: `class ModelsError extends Error { readonly code: ModelsErrorCode }`，`name = "ModelsError"`，
/// 构造函数把 `cause` 的文本拼进 `message`（[`ModelsError::with_cause_detail`]）。
/// `message` 字段就是 TS 的 `error.message`（调用方只展示它），因此 [`fmt::Display`] 逐字输出。
/// `cause_detail` 只保存被追加的那段细节，便于调用方区分“原始 message”；它不会被再次拼接。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelsError {
    /// 结构化错误码；会话文件与上层分支依赖它。
    pub code: ModelsErrorCode,
    /// 面向用户展示的错误文本（可能已含 `cause_detail`；当前稳定契约，改动需同步测试）。
    pub message: String,
    /// `withCauseDetail` 附加的细节文本（已拼进 `message` 的尾部）。
    pub cause_detail: Option<String>,
}

/// `ModelsErrorCode = "model_source" | "model_validation" | "provider" | "stream" | "auth" | "oauth"`。
///
/// TS: `utils/models-error.ts#ModelsErrorCode`（`spec/ai-auth.md` §2.5、`spec/ai-core.md` §2.2.31）。
/// 六个值全部保留；`Display` 输出 TS 的字面量（会话文件与 CLI 会看到它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelsErrorCode {
    /// `"model_source"`。
    ModelSource,
    /// `"model_validation"`。
    ModelValidation,
    /// `"provider"`。
    Provider,
    /// `"stream"`。
    Stream,
    /// `"auth"`。
    Auth,
    /// `"oauth"`。
    OAuth,
}

impl ModelsErrorCode {
    /// TS 的字符串字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ModelsErrorCode::ModelSource => "model_source",
            ModelsErrorCode::ModelValidation => "model_validation",
            ModelsErrorCode::Provider => "provider",
            ModelsErrorCode::Stream => "stream",
            ModelsErrorCode::Auth => "auth",
            ModelsErrorCode::OAuth => "oauth",
        }
    }

    /// 从 TS 字面量解析（未知值返回 `None`）。
    ///
    /// 保留 `from_str` 这个名字：它与 TS 的字符串字面量联合一一对应，且其他单元按此名调用。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "model_source" => Some(ModelsErrorCode::ModelSource),
            "model_validation" => Some(ModelsErrorCode::ModelValidation),
            "provider" => Some(ModelsErrorCode::Provider),
            "stream" => Some(ModelsErrorCode::Stream),
            "auth" => Some(ModelsErrorCode::Auth),
            "oauth" => Some(ModelsErrorCode::OAuth),
            _ => None,
        }
    }
}

impl fmt::Display for ModelsErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// 序列化为 TS 字面量：会话文件与 CLI 输出看到的就是 `"auth"` 这类字符串，而不是枚举名。
// `tests/models_error.rs` 锁定六个值的双向映射。
impl Serialize for ModelsErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

// 未知字面量报错而不是回落到默认值：静默降级会让上层把未知错误当成某个已知类别处理。
impl<'de> Deserialize<'de> for ModelsErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        ModelsErrorCode::from_str(&value)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown ModelsErrorCode: {value}")))
    }
}

impl ModelsError {
    /// `new ModelsError(code, message, options)`：`message` 原样使用（无 cause）。
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        ModelsError {
            code,
            message: message.into(),
            cause_detail: None,
        }
    }

    /// `new ModelsError(code, message, { cause })`：`withCauseDetail` 的拼接。
    ///
    /// TS:
    /// ```text
    /// if (cause === undefined || cause === null) return message;
    /// const detail = formatThrownValue(cause).trim();
    /// if (!detail || message.includes(detail)) return message;
    /// return `${message}: ${detail}`;
    /// ```
    /// `cause_text` 是调用方已经格式化好的 `formatThrownValue(cause)`
    /// （`utils/diagnostics.rs` 的 `format_thrown_value`）。
    pub fn with_cause_detail(
        code: ModelsErrorCode,
        message: impl Into<String>,
        cause_text: &str,
    ) -> Self {
        let message = message.into();
        let detail = cause_text.trim();
        if detail.is_empty() || message.contains(detail) {
            return ModelsError {
                code,
                message,
                cause_detail: None,
            };
        }
        let message = format!("{message}: {detail}");
        ModelsError {
            code,
            message,
            cause_detail: Some(detail.to_owned()),
        }
    }

    /// `ModelsError` 的 `name`（TS 构造里 `this.name = "ModelsError"`）。
    pub fn name(&self) -> &'static str {
        "ModelsError"
    }
}

// `Display` 只输出 `message`，`code` 不进入用户可见文本（它通过字段与序列化
// 单独可见）。
impl fmt::Display for ModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelsError {}
