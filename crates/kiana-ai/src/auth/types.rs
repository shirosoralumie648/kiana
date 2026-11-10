//! auth/types.ts。
//!
//! 凭证、解析结果与登录交互的全部类型（TS 侧本文件**只有类型**；Rust 侧额外提供
//! [`AuthError`]——TS 的 `Error`/`ModelsError` 在 auth 子系统的统一可观察形态）。
//!
//! 设计约束：
//! - `Credential` 的 JSON 形状即 `auth.json` 的条目形状（spec/ai-auth.md §4.1）：
//!   判别式 `type`、`api_key` 的 `key`/`env`、`oauth` 的 `access`/`refresh`/`expires`，
//!   其余字段进 `Credential::extra`（`IndexMap` 保序、原样回写）。
//! - `expires` 是 JS number（毫秒），Rust 用 `f64` 保留 `NaN`/小数的可观察语义。
//! - `AbortSignal` 用 [`AbortScope`]（`ARCHITECTURE.md` §4.2）。
//!
//! # 这个文件负责什么
//!
//! 它是 auth 子系统的**契约层**：只定义类型、trait 与错误的可观察投影，不读文件、不发
//! 网络请求、不实现重试。真正的解析逻辑在 [`crate::auth::resolve`]（`resolveProviderAuth`
//! 与锁内刷新）、默认存储在 [`crate::auth::credential_store`]、默认 api-key 实现在
//! [`crate::auth::helpers`]；`Models`（[`crate::models`]）负责把它们编排成 getAuth/login。
//!
//! 三类职责：
//! 1. **持久化形状**：`ApiKeyCredential` / `OAuthCredential` / `Credential` 对应 `auth.json`
//!    的条目，字段顺序即写出顺序，未知字段经 `extra` 保序回写（spec/ai-auth.md §4.1）；
//! 2. **解析接口**：`ApiKeyAuth` / `OAuthAuth` / `ProviderAuth` / `AuthResult` / `AuthCheck`
//!    是 provider 工厂实现、`Models` 消费的鉴权契约；
//! 3. **登录交互**：`AuthPrompt` / `AuthEvent` / `AuthInteraction` / `LoginOptions` 描述
//!    登录期间流程与宿主（CLI、TUI、扩展）之间的双向协议。
//!
//! `AuthError` 贯穿三类接口，是 TS 中 `Error` / `ModelsError` / `AbortError` 的可观察投影；
//! 其 `Display` 逐字等于 `message`。
//!
//! # 一次典型调用
//!
//! ```text
//! Models.getAuth(provider)
//!   -> CredentialStore::read
//!      -> 有 api_key 凭证 : ApiKeyAuth::resolve（key 优先，否则查 ambient）
//!      -> 有 oauth 凭证   : 必要时在 modify 锁内 OAuthAuth::refresh，再 OAuthAuth::to_auth
//!      -> 无凭证          : ApiKeyAuth::resolve 只查 ambient
//!   -> AuthResult { auth, env, source }
//!
//! Models.login(provider, type, interaction)
//!   -> ApiKeyAuth::login / OAuthAuth::login（与 interaction.signal 竞速）
//!   -> CredentialStore::modify 持久化
//! ```
//!
//! # 注意事项
//!
//! - 这些 trait 都要求 `Send + Sync`，异步方法由 `async_trait` 装箱；实现方的 future
//!   必须 `Send`，不能依赖线程局部状态。
//! - `signal` 的取消语义由调用方与实现方共同维护：实现应在长操作前后检查
//!   [`throw_if_aborted`]，并把 `signal` 传给等待/网络原语；只在入口检查一次会漏掉中途取消。
//! - 本文件的序列化类型直接决定 `auth.json` 的字节形状，字段顺序、`rename`、
//!   `skip_serializing_if` 都属于外部契约，改动需要同步 spec/ai-auth.md §4.1 与 fixture。
//!
//! TS: `src/auth/types.ts`

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiErrorKind, ModelsError, ModelsErrorCode};
use crate::types::{DiagnosticErrorInfo, ProviderEnv, ProviderHeaders};
use crate::utils::abort::{AbortReason, AbortScope};
use crate::utils::diagnostics::{ThrownValue, format_thrown_value};

/// 把 `f64` 写成 JS `JSON.stringify` 的数字文本（`1` 而不是 `1.0`，`NaN` → `null`，
/// `1e20` → `100000000000000000000`，`1e-6` → `0.000001`）。
///
/// TS 的 `expires` 是 JS number；serde 的 `f64` 默认输出 `1767225600000.0`，
/// 与 `auth.json` 的逐字格式不符（spec/ai-auth.md §4.1）。同一规则也被
/// [`crate::compat::extension_oauth_types`] 的可选数字字段复用。
///
/// 实现用 [`serde_json::value::RawValue`] 原样写出 `format_js_number` 的文本：若先解析成
/// `serde_json::Number` 再序列化，serde 会用自己的 ryu 格式化（`0.000001234` → `1.234e-6`、
/// `1e20` → `1e+20`），与 JS 输出不一致。
///
/// 本类型只实现 `Serialize`（读取侧仍由 serde 按 `f64` 解析）。`NaN`/`±Infinity` 按 JS
/// 规则写成 `null`，但 spec/ai-auth.md §4.1 要求 `auth.json` 里的 `expires` 是有限
/// number，因此这类值只有写出意义，不构成可持久化往返。
pub(crate) struct JsNumber(pub(crate) f64);

impl Serialize for JsNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let text = kiana_json::js_num::format_js_number(self.0);
        if text == "null" {
            return serializer.serialize_unit();
        }
        let raw =
            serde_json::value::RawValue::from_string(text).map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

/// `#[serde(serialize_with = ...)]`：按 JS 规则写数字。
fn serialize_js_number<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    JsNumber(*value).serialize(serializer)
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// auth 子系统的错误形态：TS 的 `Error`（含 `ModelsError`）在 Rust 的可观察投影。
///
/// TS 的 auth 代码直接 throw `Error` / `ModelsError` / `AbortError`（`signal.reason`）；
/// 调用方只观察 `error.name`、`error.message`（显示）与 `instanceof ModelsError`
/// （仅 `resolve.ts` 自己使用）。Rust 侧用一个结构承载这三种信息：
/// - `name`：`"Error"` / `"AbortError"` / `"TimeoutError"` / `"ModelsError"` …
/// - `message`：已按 `withCauseDetail` 拼接好的文本（`Display` 逐字返回它）；
/// - `code`：仅当 `name == "ModelsError"` 时为 `Some`；
/// - `error_code`：TS 错误对象的 `code` 字段（Node 的 `EADDRINUSE` 等；DOMException 的
///   legacy 数值码也以文本形式放这里，如 `TimeoutError` 的 `"23"`）；
/// - `details`：`error.errno`/`error.cause`/`error.stack`（Node 错误的附加字段，供
///   `formatErrorDetails` 使用）。装箱以保持 `Result<_, AuthError>` 的 Err 体积在
///   clippy 阈值以下。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError {
    /// `error.name`。
    pub name: String,
    /// `error.message`（`ModelsError` 已拼接 cause 细节）。
    pub message: String,
    /// `ModelsError.code`（六个 TS 字面量之一）。
    pub code: Option<ModelsErrorCode>,
    /// `error.code`（Node 错误码，例如 `EADDRINUSE`）。
    pub error_code: Option<String>,
    /// `error.errno` / `error.cause` / `error.stack`（全部为 `None` 时不装箱）。
    pub details: Option<Box<AuthErrorDetails>>,
}

/// `AuthError` 的附加可观察字段（TS `Error` 实例上的 `errno`/`cause`/`stack`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthErrorDetails {
    /// `String(error.errno)`（`typeof error.errno !== "undefined"` 时存在）。
    pub errno: Option<String>,
    /// `error.cause`。
    pub cause: Option<Box<AuthError>>,
    /// `error.stack`。
    pub stack: Option<String>,
}

impl AuthError {
    /// `new Error(message)`（`name = "Error"`）。
    pub fn new(message: impl Into<String>) -> Self {
        AuthError {
            name: "Error".into(),
            message: message.into(),
            code: None,
            error_code: None,
            details: None,
        }
    }

    /// `new Error(message)` + 显式 `name`。
    pub fn with_name(name: impl Into<String>, message: impl Into<String>) -> Self {
        AuthError {
            name: name.into(),
            message: message.into(),
            code: None,
            error_code: None,
            details: None,
        }
    }

    /// `new ModelsError(code, message, { cause })`：`message` 按 `withCauseDetail` 拼接。
    ///
    /// `cause` 只参与 message 生成：`format_thrown_value` 取错误文本，`with_cause_detail`
    /// 在文本为空或 message 已包含它时跳过拼接。cause 不会以结构化形式留在
    /// [`AuthErrorDetails`] 里。
    ///
    /// TS: `utils/models-error.ts#ModelsError`、`withCauseDetail`
    pub fn models(
        code: ModelsErrorCode,
        message: impl Into<String>,
        cause: Option<&ThrownValue>,
    ) -> Self {
        let message = message.into();
        let cause_text = cause.map(format_thrown_value);
        let models =
            ModelsError::with_cause_detail(code, message, cause_text.as_deref().unwrap_or(""));
        AuthError {
            name: "ModelsError".into(),
            message: models.message,
            code: Some(code),
            error_code: None,
            details: None,
        }
    }

    /// `ModelsError` 的构造包装。
    ///
    /// `error.message` 已含 cause 细节（[`ModelsError::with_cause_detail`] 拼好），这里原样
    /// 复制；`error_code` 与 [`AuthErrorDetails`] 留空。
    pub fn from_models_error(error: &ModelsError) -> Self {
        AuthError {
            name: "ModelsError".into(),
            message: error.message.clone(),
            code: Some(error.code),
            error_code: None,
            details: None,
        }
    }

    /// `Error` 的构造包装（`AiError`）。
    pub fn from_ai_error(error: &AiError) -> Self {
        AuthError {
            name: error.name.clone(),
            message: error.message.clone(),
            code: None,
            error_code: None,
            details: None,
        }
    }

    /// 中立项（`AbortError`）。
    pub fn aborted(message: impl Into<String>) -> Self {
        AuthError::with_name("AbortError", message)
    }

    /// 设置 `error.code`（`String()` 文本；DOMException 的数值 code 也用它）。
    pub fn with_error_code(mut self, code: impl Into<String>) -> Self {
        self.error_code = Some(code.into());
        self
    }

    /// 设置 `error.errno`（调用方负责 `String(errno)` 的文本）。
    pub fn with_errno(mut self, errno: impl Into<String>) -> Self {
        self.details_mut().errno = Some(errno.into());
        self
    }

    /// 设置 `error.cause`。
    pub fn with_cause(mut self, cause: AuthError) -> Self {
        self.details_mut().cause = Some(Box::new(cause));
        self
    }

    /// 设置 `error.stack`。
    pub fn with_stack(mut self, stack: impl Into<String>) -> Self {
        self.details_mut().stack = Some(stack.into());
        self
    }

    /// 附加字段的可写视图（按需装箱）。
    fn details_mut(&mut self) -> &mut AuthErrorDetails {
        self.details
            .get_or_insert_with(|| Box::new(AuthErrorDetails::default()))
    }

    /// `error.errno`（`String()` 文本）。
    pub fn errno(&self) -> Option<&str> {
        self.details
            .as_deref()
            .and_then(|details| details.errno.as_deref())
    }

    /// `error.cause`。
    pub fn cause(&self) -> Option<&AuthError> {
        self.details
            .as_deref()
            .and_then(|details| details.cause.as_deref())
    }

    /// `error.stack`。
    pub fn stack(&self) -> Option<&str> {
        self.details
            .as_deref()
            .and_then(|details| details.stack.as_deref())
    }

    /// 转回 [`AiError`]（供不使用 `ModelsError` 的调用方展示/传播）。
    ///
    /// 映射口径：`AbortError`/`TimeoutError` → [`AiErrorKind::Aborted`]；`ModelsError` 中
    /// `auth`/`oauth` 两种 code → [`AiErrorKind::Auth`]，其余 code → [`AiErrorKind::Other`]；
    /// 其他 name → `Other`。`name` 原样保留；`error_code` 与 [`AuthErrorDetails`] 不进入
    /// [`AiError`]（它没有对应字段）。
    pub fn to_ai_error(&self) -> AiError {
        let kind = match self.name.as_str() {
            "AbortError" | "TimeoutError" => AiErrorKind::Aborted,
            "ModelsError" => match self.code {
                Some(ModelsErrorCode::Auth | ModelsErrorCode::OAuth) => AiErrorKind::Auth,
                _ => AiErrorKind::Other,
            },
            _ => AiErrorKind::Other,
        };
        AiError::new(kind, self.message.clone()).with_name(self.name.clone())
    }

    /// `error.name === "AbortError"`（`Models.getAuth` 取消时调用方按名字判断）。
    ///
    /// 只看名字：`TimeoutError`（OAuth 刷新超时）不算“调用方取消”，与 TS 的判断一致；
    /// 注意 [`AuthError::to_ai_error`] 会把 `TimeoutError` 也归入
    /// [`AiErrorKind::Aborted`]，两处口径不同。
    pub fn is_aborted(&self) -> bool {
        self.name == "AbortError"
    }

    /// 是否为 `ModelsError`（TS 的 `instanceof ModelsError`）。
    pub fn is_models_error(&self) -> bool {
        self.name == "ModelsError"
    }

    /// 作为 `Unknown` 抛出值参与 `withCauseDetail` 计算。
    ///
    /// 只投影 `name`/`message`：`format_thrown_value` 只读取这两者（`stack`/`code` 不参与
    /// 文本拼接），因此用于 `ModelsError` 的 cause 已经足够。要完整诊断信息应直接读
    /// [`AuthErrorDetails`]。
    pub fn as_thrown_value(&self) -> ThrownValue {
        ThrownValue::Error(DiagnosticErrorInfo {
            name: Some(self.name.clone()),
            message: self.message.clone(),
            stack: None,
            code: None,
        })
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AuthError {}

impl From<AiError> for AuthError {
    fn from(error: AiError) -> Self {
        AuthError::from_ai_error(&error)
    }
}

impl From<&AiError> for AuthError {
    fn from(error: &AiError) -> Self {
        AuthError::from_ai_error(error)
    }
}

impl From<ModelsError> for AuthError {
    fn from(error: ModelsError) -> Self {
        AuthError::from_models_error(&error)
    }
}

/// `signal.throwIfAborted()`：已中止时返回 `signal.reason`（缺省 `AbortError`）。
///
/// 无 reason 的 signal 用 [`AbortReason::fallback_abort_error`]（message 为
/// "The operation was aborted"）；`AbortScope::abort()` 产生的 reason 则是 DOMException
/// 文案 "This operation was aborted"（spec/ai-auth.md §4.7）。本函数只读取状态，
/// 不清除取消。
///
/// TS: `AbortSignal.prototype.throwIfAborted`
pub fn throw_if_aborted(signal: &AbortScope) -> Result<(), AuthError> {
    if !signal.is_aborted() {
        return Ok(());
    }
    let reason = signal
        .reason()
        .unwrap_or_else(AbortReason::fallback_abort_error);
    Err(AuthError::with_name(reason.name, reason.message))
}

/// 把中止 scope 的 reason 渲染为 [`AuthError`]（`raceWithAbortSignal` 的拒绝值）。
///
/// 与 [`throw_if_aborted`] 的分工：本函数不检查状态、总是构造一个错误值（由调用方用
/// `Err(...)` 包装），用于 `select!` 的取消分支（此时中止已经发生）；有 reason 时保留其
/// `name`/`message`。
pub(crate) fn abort_error(signal: &AbortScope) -> AuthError {
    match signal.reason() {
        Some(reason) => AuthError::with_name(reason.name, reason.message),
        None => AuthError::with_name("AbortError", "The operation was aborted"),
    }
}

// ---------------------------------------------------------------------------
// 凭证
// ---------------------------------------------------------------------------

/// `AuthType = "api_key" | "oauth"`。
///
/// serde 只接受这两个字面量；未知值会让 [`Credential`] 的反序列化直接失败，不会回退到
/// 默认类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthType {
    /// `"api_key"`。
    #[serde(rename = "api_key")]
    ApiKey,
    /// `"oauth"`。
    #[serde(rename = "oauth")]
    OAuth,
}

impl AuthType {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            AuthType::ApiKey => "api_key",
            AuthType::OAuth => "oauth",
        }
    }
}

impl fmt::Display for AuthType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stored api-key credential。
///
/// `env` 持有 provider 作用域的 environment/config 值（例如 Cloudflare account/gateway id）；
/// `key` 允许缺省（完全靠 env/ambient 解析，见 §4.1 的 `empty-key-provider`）。
///
/// `env` 在 Rust 侧是 `ProviderEnv`（`IndexMap<String, String>`）：TS 的 `undefined` 值
/// 会被 `JSON.stringify` 丢弃，因此磁盘语义一致。反序列化 [`Credential`] 时 `type` 已被
/// 判别式消费，不会出现在 `extra` 里。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    /// `key?: string`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `env?: Record<string, string | undefined>`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
    /// 未知字段（保序、原样回写）。
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

/// OAuth token data（TS 的 `OAuthCredentials`，无 `type` 判别式）。
///
/// 字段声明顺序是 TS 接口顺序（`refresh`、`access`、`expires`），序列化文本因此以
/// `refresh` 开头；[`OAuthCredential`] 为对齐 `auth.json` 示例把 `access` 放前面，两者
/// 互转时 `extra` 原样搬运。`Default`（空串、`expires = 0.0`）只作占位。
///
/// TS: `src/auth/types.ts#OAuthCredentials`（供 `src/oauth.ts` 兼容 barrel）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentials {
    /// `refresh`。
    pub refresh: String,
    /// `access`。
    pub access: String,
    /// 绝对过期时间（毫秒，JS `Date.now()` 时基）；按 JS `JSON.stringify` 规则写出。
    #[serde(serialize_with = "serialize_js_number")]
    pub expires: f64,
    /// 索引签名：任何额外字段（`accountId`、`clientId`、`scopes`…）。
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

/// Stored canonical OAuth credential。
///
/// 字段顺序即 `auth.json` 的写出顺序（spec/ai-auth.md §4.1 示例、§8.2 类型草图）：`type` 由
/// `Credential` 的判别式提供，随后是 `access`、`refresh`、`expires`，未知字段（`accountId`、
/// `clientId`、`scopes`…）按原顺序平铺在末尾。
///
/// TS 的凭证是普通对象，`JSON.stringify` 保留运行时插入顺序：9 条内置流程里 openai-codex /
/// openai-chatgpt / openrouter / kimi-coding / xai / radius 都以 `access` 开头（anthropic /
/// meta / github-copilot 以 `refresh` 开头）。Rust 侧无法按实例保留顺序，固定为多数流程的
/// `access` 开头（同时与 §4.1 的 `auth.json` 示例一致）。
///
/// `extra` 保留 `accountId`/`clientId`/`scopes`/`enterpriseUrl`/`scope`/`deviceId` 等
/// 未知字段（保序、原样回写）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// 请求凭证（api key 或 bearer token）。
    pub access: String,
    /// 刷新令牌。
    pub refresh: String,
    /// 绝对过期时间（毫秒）；按 JS `JSON.stringify` 规则写出（整数不带 `.0`）。
    #[serde(serialize_with = "serialize_js_number")]
    pub expires: f64,
    /// 未知字段（保序、原样回写）。
    #[serde(flatten)]
    pub extra: IndexMap<String, serde_json::Value>,
}

impl OAuthCredential {
    /// 构造一个只含必填字段的凭证。
    pub fn new(access: impl Into<String>, refresh: impl Into<String>, expires: f64) -> Self {
        OAuthCredential {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            extra: IndexMap::new(),
        }
    }

    /// 读取额外字段（`[key: string]: unknown`）。
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.extra.get(key)
    }

    /// 读取字符串额外字段（TS 的 `credential.someId` 用法）。
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(serde_json::Value::as_str)
    }

    /// `credential.enterpriseUrl`（github-copilot 使用）。
    pub fn enterprise_url(&self) -> Option<&str> {
        self.get_str("enterpriseUrl")
    }

    /// 转成兼容层的 [`OAuthCredentials`]（丢弃 `type`）。
    pub fn credentials(&self) -> OAuthCredentials {
        OAuthCredentials {
            refresh: self.refresh.clone(),
            access: self.access.clone(),
            expires: self.expires,
            extra: self.extra.clone(),
        }
    }
}

impl From<OAuthCredentials> for OAuthCredential {
    fn from(value: OAuthCredentials) -> Self {
        OAuthCredential {
            access: value.access,
            refresh: value.refresh,
            expires: value.expires,
            extra: value.extra,
        }
    }
}

/// One type-tagged credential per provider——`auth.json` 的条目形状。
///
/// 序列化：`{"type": "api_key" | "oauth", ...}`；未知字段保序回写。
///
/// 这是 serde 的 internally tagged 枚举（`#[serde(tag = "type")]`）：序列化时 `type`
/// 恒在首位；反序列化要求顶层是对象，缺失或未知 `type` 会报错；变体内部的未知键由各
/// `extra` 捕获而不是静默丢弃。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    /// `type: "api_key"`。
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    /// `type: "oauth"`。
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    /// 判别式。
    pub fn type_(&self) -> AuthType {
        match self {
            Credential::ApiKey(_) => AuthType::ApiKey,
            Credential::OAuth(_) => AuthType::OAuth,
        }
    }

    /// `credential.type === "api_key"` 分支的取值。
    pub fn as_api_key(&self) -> Option<&ApiKeyCredential> {
        match self {
            Credential::ApiKey(credential) => Some(credential),
            Credential::OAuth(_) => None,
        }
    }

    /// `credential.type === "oauth"` 分支的取值。
    pub fn as_oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Credential::OAuth(credential) => Some(credential),
            Credential::ApiKey(_) => None,
        }
    }
}

/// Non-secret credential metadata for account/status enumeration。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    /// `Provider.id`。
    pub provider_id: String,
    /// 凭证类型。
    pub r#type: AuthType,
}

/// Optional cancellation for public auth and credential operations。
///
/// `signal: None` 表示该操作不可取消（不是“沿用调用方的 signal”）；需要传播取消时调用方
/// 显式 clone 一个 [`AbortScope`] 放进这里。
#[derive(Debug, Clone, Default)]
pub struct AuthOperationOptions {
    /// 取消信号。
    pub signal: Option<AbortScope>,
}

// ---------------------------------------------------------------------------
// 解析结果
// ---------------------------------------------------------------------------

/// Request auth for a single model request。
///
/// 若某个值无法表达为 `apiKey`/`headers`/`baseUrl`，它是 provider config，不是 auth。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    /// `apiKey?: string`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// `headers?: ProviderHeaders`。
    ///
    /// 值为 `None` 表示抑制同名头；与模型自身的 headers 合并时按小写名称覆盖并移到末尾。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    /// `baseUrl?: string`。
    ///
    /// 非空时覆盖该次请求使用的模型 `baseUrl`（空串等价于未提供，见
    /// `crates/kiana-ai/src/models/mod.rs` 的 `apply_auth`，约 1624 行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Result of resolving auth for a model。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResult {
    /// 请求 auth。
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials and ambient context.
    ///
    /// OAuth 分支恒为 `None`；api_key 分支来自凭证的 `env` 或 ambient 解析结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI: `"ANTHROPIC_API_KEY"`, `"OAuth"`, `"~/.aws/credentials"`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl AuthResult {
    /// 只带 `auth` 的结果（`source`/`env` 缺省）。
    pub fn new(auth: ModelAuth) -> Self {
        AuthResult {
            auth,
            env: None,
            source: None,
        }
    }
}

/// `AuthCheck`。
///
/// `type` 表示命中的 handler 类别，`source` 仅用于显示（如 `"OAuth"`、
/// `"ANTHROPIC_API_KEY"`）；两者都不含 secret。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheck {
    /// 状态的展示标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 凭证类型。
    pub r#type: AuthType,
}

// ---------------------------------------------------------------------------
// 登录交互
// ---------------------------------------------------------------------------

/// `select` 提示的选项（`id` 是 `prompt()` 的返回值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSelectOption {
    /// 选项 id。
    pub id: String,
    /// 展示文本。
    pub label: String,
    /// 可选描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Prompt shown to the user during login。
///
/// TS 是 `{ signal?: AbortSignal } & (union)`；Rust 的 signal 放在
/// [`AuthPromptRequest`] 上（`ARCHITECTURE` 的 Rust 草图约定）。
///
/// 这些变体是进程内表示，没有 serde 派生；跨进程/扩展的线格式在
/// [`crate::compat::extension_oauth_types`]（`OAuthPrompt` 等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPrompt {
    /// `type: "text"`。
    Text {
        /// 提示文本。
        message: String,
        /// 占位符。
        placeholder: Option<String>,
    },
    /// `type: "secret"`。
    Secret {
        /// 提示文本。
        message: String,
        /// 占位符。
        placeholder: Option<String>,
    },
    /// `type: "select"`。
    Select {
        /// 提示文本。
        message: String,
        /// 选项。
        options: Vec<AuthSelectOption>,
    },
    /// `type: "manual_code"`。
    ManualCode {
        /// 提示文本。
        message: String,
        /// 占位符。
        placeholder: Option<String>,
    },
}

impl AuthPrompt {
    /// TS 的判别式字面量。
    pub fn type_name(&self) -> &'static str {
        match self {
            AuthPrompt::Text { .. } => "text",
            AuthPrompt::Secret { .. } => "secret",
            AuthPrompt::Select { .. } => "select",
            AuthPrompt::ManualCode { .. } => "manual_code",
        }
    }

    /// 提示文本（所有变体都有）。
    pub fn message(&self) -> &str {
        match self {
            AuthPrompt::Text { message, .. }
            | AuthPrompt::Secret { message, .. }
            | AuthPrompt::Select { message, .. }
            | AuthPrompt::ManualCode { message, .. } => message,
        }
    }

    /// 占位符（`select` 没有）。
    pub fn placeholder(&self) -> Option<&str> {
        match self {
            AuthPrompt::Text { placeholder, .. }
            | AuthPrompt::Secret { placeholder, .. }
            | AuthPrompt::ManualCode { placeholder, .. } => placeholder.as_deref(),
            AuthPrompt::Select { .. } => None,
        }
    }
}

/// 一次提示请求：`AuthPrompt` + 该提示自己的取消信号（TS 的 `AuthPrompt.signal`）。
///
/// 该 `signal` 只取消**这一次提示**：外部事件先到（例如回调服务器已收到 code）时 abort
/// 它，让宿主收起提示；流程本身不等待 `prompt` 返回（竞速胜出后 prompt future 可能直接被
/// 丢弃）。宿主通常以普通 `Error("Login cancelled")` 结束这次提示（不是 `AbortError`）；
/// 整个登录流程的取消用 [`AuthInteraction::signal`]（见 spec/ai-auth.md §2.1）。
#[derive(Debug, Clone)]
pub struct AuthPromptRequest {
    /// 提示内容。
    pub prompt: AuthPrompt,
    /// 单次提示的取消信号（外部事件胜出时 abort）。
    pub signal: Option<AbortScope>,
}

impl AuthPromptRequest {
    /// 无取消信号的提示。
    pub fn new(prompt: AuthPrompt) -> Self {
        AuthPromptRequest {
            prompt,
            signal: None,
        }
    }

    /// 带取消信号的提示。
    pub fn with_signal(prompt: AuthPrompt, signal: AbortScope) -> Self {
        AuthPromptRequest {
            prompt,
            signal: Some(signal),
        }
    }
}

/// `AuthInfoLink`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfoLink {
    /// URL。
    pub url: String,
    /// 链接文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `AuthEvent`：登录期间由流程发给宿主的事件。
///
/// 事件只在本进程内传递（无 serde 派生），由宿主负责渲染；`device_code` 的两个
/// `*_on_confirm` 字段为 `None` 时按默认 `true` 处理。
#[derive(Debug, Clone, PartialEq)]
pub enum AuthEvent {
    /// `{ type: "info", message, links? }`。
    Info {
        /// 文本。
        message: String,
        /// 链接。
        links: Option<Vec<AuthInfoLink>>,
    },
    /// `{ type: "auth_url", url, instructions? }`。
    AuthUrl {
        /// 授权 URL。
        url: String,
        /// 操作说明。
        instructions: Option<String>,
    },
    /// `{ type: "device_code", ... }`。
    DeviceCode {
        /// 用户码。
        user_code: String,
        /// 验证地址。
        verification_uri: String,
        /// 是否提供浏览器打开（默认 true；宿主可忽略）。
        open_browser_on_confirm: Option<bool>,
        /// 是否提供复制用户码（默认 true；宿主可忽略）。
        copy_code_on_confirm: Option<bool>,
        /// 轮询间隔（秒）。
        interval_seconds: Option<f64>,
        /// 有效期（秒）。
        expires_in_seconds: Option<f64>,
    },
    /// `{ type: "progress", message }`。
    Progress {
        /// 文本。
        message: String,
    },
}

impl AuthEvent {
    /// TS 的判别式字面量。
    pub fn type_name(&self) -> &'static str {
        match self {
            AuthEvent::Info { .. } => "info",
            AuthEvent::AuthUrl { .. } => "auth_url",
            AuthEvent::DeviceCode { .. } => "device_code",
            AuthEvent::Progress { .. } => "progress",
        }
    }
}

/// Login interaction callbacks serving both api-key and OAuth flows。
///
/// `prompt()` 返回输入/选中的字符串（`select` 返回 option id）；取消/中止时 reject。
/// `signal` abort 整个登录流程；单次提示的取消用 [`AuthPromptRequest::signal`]。
/// `notify` 同步、无返回。
///
/// trait 要求 `Send + Sync`：交互对象以 `&dyn` 借用传入 OAuth 流程，必须能在多线程
/// 运行时中共享，实现不能依赖线程局部状态。`prompt` 被取消时返回 [`AuthError`]；仓库内
/// 宿主实现（TUI）使用普通 `Error`（name `"Error"`、message `"Login cancelled"`），CLI
/// 实现则忽略 per-prompt signal（只响应 [`AuthInteraction::signal`]）；`notify` 中不要
/// 阻塞等待网络或用户输入。
///
/// TS: `src/auth/types.ts#AuthInteraction`
// `async_trait` 展开出的 `Pin<Box<dyn Future>>` 方法自带 `#[must_use]`；clippy 的
// `double_must_use` 无法看到宏内属性，故在 trait 上显式关闭（仅影响这 5 个 trait）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AuthInteraction: Send + Sync {
    /// 整个登录流程的取消信号。
    fn signal(&self) -> &AbortScope;

    /// 显示提示并等待输入。
    async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError>;

    /// 发送事件（同步；实现不得让异常影响流程）。
    fn notify(&self, event: &AuthEvent);
}

/// Normalized interaction passed to provider login implementations。
///
/// TS 的 `ProviderAuthInteraction = AuthInteraction & { signal: AbortSignal }`；
/// Rust 的 [`AuthInteraction`] 已把 `signal` 作为必需方法，故二者等价。
pub type ProviderAuthInteraction = dyn AuthInteraction;

/// App-supplied context for `Models.login`。
///
/// `get_device_id` 是回调而非值：[`OAuthAuth::login`] 只在需要的流程里调用它（如 OpenAI 的
/// agent host ID），实现应返回稳定 ID。`Debug` 只报告是否提供该回调，不打印闭包。
#[derive(Clone, Default)]
pub struct LoginOptions {
    /// 返回本安装的稳定 ID（如 OpenAI 的 agent host ID）；仅在需要的流程里调用。
    pub get_device_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// app 自报名称（OpenAI agent name hint、Codex originator），默认 kiana 自己的名字。
    pub agent_name: Option<String>,
}

impl fmt::Debug for LoginOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginOptions")
            .field("get_device_id", &self.get_device_id.is_some())
            .field("agent_name", &self.agent_name)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Provider auth
// ---------------------------------------------------------------------------

/// `ApiKeyAuth` 的 `check`/`resolve` 入参。
///
/// 三个字段都按 `'a` 借用；`async_trait` 生成的 future 会捕获这些引用，因此调用方必须让
/// 它们活到 `check`/`resolve` 返回的 future 完成（被 await 或 drop）。
#[derive(Clone, Copy)]
pub struct ApiKeyAuthInput<'a> {
    /// 环境访问（可注入）。
    pub ctx: &'a dyn AuthContext,
    /// 已存储的凭证（若有）。
    pub credential: Option<&'a ApiKeyCredential>,
    /// 取消信号。
    pub signal: &'a AbortScope,
}

/// Api-key auth：存储的 key/provider env 加上 ambient 来源（env 变量、AWS profile、ADC 文件）。
///
/// 仅 ambient 的 provider 不实现 `login`（[`ApiKeyAuth::has_login`] 返回 `false`）。
///
/// `has_login`/`has_check` 对应 TS 的可选成员：缺省 `login` 返回“不支持登录”的
/// [`AuthError`]，缺省 `check` 返回 `Ok(None)`（`Models` 会回退到 `resolve` 判断可用性）。
/// 因此实现至少要实现 `name` 与 `resolve`。
///
/// TS: `src/auth/types.ts#ApiKeyAuth`
// `async_trait` 展开出的 `Pin<Box<dyn Future>>` 方法自带 `#[must_use]`；clippy 的
// `double_must_use` 无法看到宏内属性，故在 trait 上显式关闭（仅影响这 5 个 trait）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ApiKeyAuth: Send + Sync {
    /// Display name，例如 `"Anthropic API key"`。
    fn name(&self) -> &str;

    /// 是否实现了 `login`（TS 的 `login !== undefined`）。
    fn has_login(&self) -> bool {
        false
    }

    /// 交互式设置（prompt for key/provider env）。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        let _ = interaction;
        Err(AuthError::new(format!(
            "{} does not support login",
            self.name()
        )))
    }

    /// 是否实现了 `check`（TS 的 `check !== undefined`；缺省时 `Models` 通过 `resolve` 判断）。
    fn has_check(&self) -> bool {
        false
    }

    /// 可选的无副作用可用性检查。
    async fn check(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        let _ = input;
        Ok(None)
    }

    /// 从存储凭证和/或 ambient 来源解析 auth（按字段合并）。
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError>;
}

/// OAuth auth。
///
/// `refresh`/`toAuth` 的拆分让 `Models` 拥有加锁刷新模式：`refresh` 产出凭证，
/// `toAuth` 从最终存储的凭证推导请求 auth。
///
/// 并发/重入约束：`Models` 在 [`CredentialStore::modify`] 的回调内调用 `refresh`，即持有
/// 该 provider 的串行门（见 [`crate::auth::resolve`]）。实现不得在这个回调里再次对同一
/// provider 调用该 store 的 `modify`/`delete`，否则默认内存实现会等待自己持有的门；
/// 刷新结果由 store 持久化，实现本身不写存储。
///
/// `to_auth` 必须无副作用且只依赖传入凭证：每次从存储解析 OAuth auth 都会调用它，它不应
/// 触发网络或修改存储。
///
/// TS: `src/auth/types.ts#OAuthAuth`
// `async_trait` 展开出的 `Pin<Box<dyn Future>>` 方法自带 `#[must_use]`；clippy 的
// `double_must_use` 无法看到宏内属性，故在 trait 上显式关闭（仅影响这 5 个 trait）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait OAuthAuth: Send + Sync {
    /// Display name，例如 `"Anthropic (Claude Pro/Max)"`。
    fn name(&self) -> &str;

    /// 该 auth 方式是否由 provider 订阅支撑。
    fn is_subscription(&self) -> bool {
        false
    }

    /// OAuth 登录选项的标签（例如 `"Sign in with ChatGPT"`）。
    fn login_label(&self) -> Option<&str> {
        None
    }

    /// 交互式登录。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError>;

    /// 交换 refresh token（网络调用；失败即抛；`Models` 在 store 锁内运行它）。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError>;

    /// 从有效凭证无副作用地推导请求 auth。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError>;
}

/// Provider auth。`apiKey`/`oauth` 至少存在一个。
///
/// 即便 ambient 凭证的 provider 与无 key 的本地服务也提供 `apiKey` auth，
/// 其 `resolve()` 报告 provider 是否已配置。
///
/// 两个字段都是 `Arc<dyn ...>`，可被 provider 注册表克隆共享；`Debug` 只打印各自的
/// `name()`。本结构不做“至少一个存在”的运行期校验，缺失时由 `Models.login` 报
/// `does not support ... login`。
///
/// TS: `src/auth/types.ts#ProviderAuth`
#[derive(Clone, Default)]
pub struct ProviderAuth {
    /// api-key auth。
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    /// OAuth auth。
    pub oauth: Option<Arc<dyn OAuthAuth>>,
}

impl fmt::Debug for ProviderAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderAuth")
            .field("api_key", &self.api_key.as_ref().map(|auth| auth.name()))
            .field("oauth", &self.oauth.as_ref().map(|auth| auth.name()))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 环境访问
// ---------------------------------------------------------------------------

/// Environment access for auth resolution。Injectable for tests and browsers.
///
/// 默认实现见 [`crate::auth::context::default_provider_auth_context`]：`env` 只返回 trim
/// 后非空的原值（不去空格），`file_exists` 展开前导 `~` 且任何错误都视为不存在。
///
/// TS: `src/auth/types.ts#AuthContext`
// `async_trait` 展开出的 `Pin<Box<dyn Future>>` 方法自带 `#[must_use]`；clippy 的
// `double_must_use` 无法看到宏内属性，故在 trait 上显式关闭（仅影响这 5 个 trait）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AuthContext: Send + Sync {
    /// 读取环境变量（未设置/空值语义由实现决定，默认实现见 [`crate::auth::context`]）。
    async fn env(&self, name: &str) -> Option<String>;

    /// 检查文件是否存在。支持前导 `~`；浏览器环境恒 `false`。
    async fn file_exists(&self, path: &str) -> bool;
}

// ---------------------------------------------------------------------------
// CredentialStore
// ---------------------------------------------------------------------------

/// `modify` 的异步回调：看到当前凭证，返回新凭证或 `None`（保持不变）。
///
/// 必须 `Send`：store 实现可能把 future 移入异步临界区、在其他线程上 await；
/// `'static` 因为回调可能在调用栈返回后才被轮到执行（排在该 provider 的串行门后）。
///
/// TS: `(current: Credential | undefined) => Promise<Credential | undefined>`
pub type CredentialModifyFuture =
    Pin<Box<dyn Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static>>;

/// 装箱后的 `modify` 回调（`modify` 是唯一写路径，需要 object safety）。
///
/// `FnOnce + 'static`：每次 `modify` 只执行一次，且可能在调用方返回后才运行，因此必须
/// 自行 move 需要的数据（不能借用栈上的临时值）。
pub type CredentialModifyFn =
    Box<dyn FnOnce(Option<Credential>) -> CredentialModifyFuture + Send + 'static>;

/// App-owned credential storage，按 `Provider.id` 一条凭证。
///
/// `modify` 是唯一写路径，因此每次变更都是串行化的 read-modify-write；
/// `Models.getAuth()` 在 `modify` 内做 OAuth 刷新，避免并发请求重复刷新已轮换的 token。
/// App 在 login 后通过 `modify(provider.id, async () => credential)` 持久化。
///
/// 错误语义：`read` 对缺失条目返回 `Ok(None)`；方法仅在存储失败时 `Err`；
/// `Models` 把这类错误包成 code `"auth"` 的 `ModelsError`。
///
/// TS: `src/auth/types.ts#CredentialStore`
// `async_trait` 展开出的 `Pin<Box<dyn Future>>` 方法自带 `#[must_use]`；clippy 的
// `double_must_use` 无法看到宏内属性，故在 trait 上显式关闭（仅影响这 5 个 trait）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// 读取已存储凭证（可能已过期）。显示/状态用；请求用 auth 来自 `Models.getAuth()`。
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError>;

    /// 列出凭证元数据而不暴露密钥；实现**不得**执行配置的 API-key 命令。
    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError>;

    /// 串行化写入——唯一的写路径。按 provider id 互斥（底层支持时跨进程亦然），
    /// 返回写后凭证；`f` 的错误原样传播。
    ///
    /// 被 signal 取消的排队调用不得在稍后执行 `f`：实现应在拿到串行门后、调用 `f` 前再
    /// 检查一次中止（[`crate::auth::credential_store::InMemoryCredentialStore`] 即如此）。
    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError>;

    /// 删除凭证（logout）。必须与 `modify` 串行化。
    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError>;
}

#[cfg(test)]
mod tests {
    // 本模块锁定 `types.rs` 的对外可观察行为：
    // - 序列化字节形状（字段顺序、`type` 判别式、未知字段保序、JS 数字文本）；
    // - `tests/fixtures/auth-core/credential-json.json` 是 node 用 TS 形状对象字面量生成的
    //   黄金样本（spec/ai-auth.md §7.15 第 4 条），`matches_js_stringify_golden_fixture` 逐例比对；
    // - 错误/中止语义（`ModelsError` 的 cause 拼接、`throwIfAborted` 文案）；
    // - 提示词辅助方法与 `AuthType` 的字面量。
    // 修改本文件序列化字段时，这些断言会直接失败，属于预期的回归保护。
    use super::*;

    fn credential_json(credential: &Credential) -> String {
        serde_json::to_string(credential).expect("serialize")
    }

    #[test]
    fn serializes_api_key_credentials_in_ts_field_order() {
        let credential = Credential::ApiKey(ApiKeyCredential {
            key: Some("sk-test".into()),
            env: Some(ProviderEnv::from([(
                "CLOUDFLARE_ACCOUNT_ID".to_string(),
                "acct".to_string(),
            )])),
            extra: IndexMap::new(),
        });
        assert_eq!(
            credential_json(&credential),
            r#"{"type":"api_key","key":"sk-test","env":{"CLOUDFLARE_ACCOUNT_ID":"acct"}}"#
        );
    }

    #[test]
    fn serializes_empty_key_provider_without_extra_fields() {
        let credential = Credential::ApiKey(ApiKeyCredential::default());
        assert_eq!(credential_json(&credential), r#"{"type":"api_key"}"#);
    }

    #[test]
    fn serializes_oauth_credentials_with_extras_in_order() {
        let mut extra = IndexMap::new();
        extra.insert("accountId".to_string(), serde_json::json!("acct_1"));
        extra.insert(
            "scopes".to_string(),
            serde_json::json!(["openid", "profile"]),
        );
        let credential = Credential::OAuth(OAuthCredential {
            access: "access".into(),
            refresh: "refresh".into(),
            expires: 1767225600000.0,
            extra,
        });
        assert_eq!(
            credential_json(&credential),
            r#"{"type":"oauth","access":"access","refresh":"refresh","expires":1767225600000,"accountId":"acct_1","scopes":["openid","profile"]}"#
        );
    }

    #[test]
    fn round_trips_unknown_fields() {
        let json = r#"{"type":"oauth","access":"a","refresh":"r","expires":1,"enterpriseUrl":"company.ghe.com","availableModelIds":["gpt-5.4"]}"#;
        let credential: Credential = serde_json::from_str(json).expect("deserialize");
        let oauth = credential.as_oauth().expect("oauth");
        assert_eq!(oauth.enterprise_url(), Some("company.ghe.com"));
        // 已知字段按 auth.json 的运行时顺序写出（`access`、`refresh`、`expires`）；未知字段保序。
        assert_eq!(
            credential_json(&credential),
            r#"{"type":"oauth","access":"a","refresh":"r","expires":1,"enterpriseUrl":"company.ghe.com","availableModelIds":["gpt-5.4"]}"#
        );
    }

    #[test]
    fn expires_accepts_nan_like_js_numbers() {
        let credential = Credential::OAuth(OAuthCredential::new("a", "r", f64::NAN));
        // `JSON.stringify(NaN)` → `null`.
        assert_eq!(
            credential_json(&credential),
            r#"{"type":"oauth","access":"a","refresh":"r","expires":null}"#
        );
    }

    #[test]
    fn matches_js_stringify_golden_fixture() {
        // `tests/fixtures/auth-core/credential-json.json` 由 node 用 TS 形状的对象字面量生成
        // （`json` 字段即 JS `JSON.stringify` 的输出，spec/ai-auth.md §7.15 第 4 条）。
        let fixture: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/auth-core/credential-json.json"
        ))
        .expect("fixture json");

        let mut extras = IndexMap::new();
        extras.insert("clientId".to_string(), serde_json::json!("oaiapp_issued"));
        extras.insert(
            "scopes".to_string(),
            serde_json::json!(["openid", "profile", "email", "offline_access"]),
        );

        let cases: Vec<(&str, Credential)> = vec![
            (
                "codex_access_first",
                Credential::OAuth(OAuthCredential::new(
                    "access-token",
                    "refresh-token",
                    1767225600000.0,
                )),
            ),
            (
                "chatgpt_extras_after_known_fields",
                Credential::OAuth(OAuthCredential {
                    access: "access-token".into(),
                    refresh: "refresh-token".into(),
                    expires: 1767225600000.0,
                    extra: extras,
                }),
            ),
            (
                "api_key_with_env",
                Credential::ApiKey(ApiKeyCredential {
                    key: Some("sk-test".into()),
                    env: Some(ProviderEnv::from([(
                        "CLOUDFLARE_ACCOUNT_ID".to_string(),
                        "acct".to_string(),
                    )])),
                    extra: IndexMap::new(),
                }),
            ),
            (
                "api_key_without_key",
                Credential::ApiKey(ApiKeyCredential::default()),
            ),
            (
                "expires_js_number_text",
                Credential::OAuth(OAuthCredential::new("a", "r", 0.000001234)),
            ),
            (
                "expires_1e-6",
                Credential::OAuth(OAuthCredential::new("a", "r", 1e-6)),
            ),
            (
                "expires_1e20",
                Credential::OAuth(OAuthCredential::new("a", "r", 1e20)),
            ),
            (
                "expires_1e21",
                Credential::OAuth(OAuthCredential::new("a", "r", 1e21)),
            ),
            (
                "expires_max_safe_integer",
                Credential::OAuth(OAuthCredential::new("a", "r", 9_007_199_254_740_991.0)),
            ),
            (
                "expires_nan_is_null",
                Credential::OAuth(OAuthCredential::new("a", "r", f64::NAN)),
            ),
        ];
        assert_eq!(cases.len(), fixture.len());
        for (name, credential) in cases {
            let entry = fixture
                .iter()
                .find(|entry| entry["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("missing fixture case {name}"));
            assert_eq!(
                credential_json(&credential),
                entry["json"].as_str().expect("json string"),
                "case {name}"
            );
        }
    }

    #[test]
    fn maps_models_errors_with_cause_detail() {
        let cause = ThrownValue::from_text("invalid_grant");
        let error = AuthError::models(
            ModelsErrorCode::OAuth,
            "OAuth refresh failed for anthropic",
            Some(&cause),
        );
        assert_eq!(error.name, "ModelsError");
        assert_eq!(error.code, Some(ModelsErrorCode::OAuth));
        assert_eq!(
            error.message,
            "OAuth refresh failed for anthropic: invalid_grant"
        );
        assert!(error.is_models_error());
        // Cause 已在 message 中时不再重复拼接。
        let error = AuthError::models(
            ModelsErrorCode::OAuth,
            "OAuth refresh failed for anthropic: invalid_grant",
            Some(&cause),
        );
        assert_eq!(
            error.message,
            "OAuth refresh failed for anthropic: invalid_grant"
        );
    }

    #[test]
    fn throw_if_aborted_matches_dom_semantics() {
        let signal = AbortScope::new();
        assert!(throw_if_aborted(&signal).is_ok());
        signal.abort();
        let error = throw_if_aborted(&signal).unwrap_err();
        assert_eq!(error.name, "AbortError");
        assert_eq!(error.message, "This operation was aborted");
    }

    #[test]
    fn prompt_helpers_expose_the_ts_fields() {
        let prompt = AuthPrompt::Secret {
            message: "Enter Anthropic API key".into(),
            placeholder: None,
        };
        assert_eq!(prompt.type_name(), "secret");
        assert_eq!(prompt.message(), "Enter Anthropic API key");
        assert_eq!(prompt.placeholder(), None);
    }

    #[test]
    fn auth_type_serializes_the_ts_literals() {
        assert_eq!(
            serde_json::to_string(&AuthType::ApiKey).unwrap(),
            "\"api_key\""
        );
        assert_eq!(
            serde_json::to_string(&AuthType::OAuth).unwrap(),
            "\"oauth\""
        );
        assert_eq!(
            serde_json::to_string(&CredentialInfo {
                provider_id: "p".into(),
                r#type: AuthType::ApiKey,
            })
            .unwrap(),
            r#"{"providerId":"p","type":"api_key"}"#
        );
    }
}
