//! `cli/auth-check.ts`（73 行）：`checkProviderAuth` / `createAuthCheckModelRuntime`。
//!
//! TS: `packages/coding-agent/src/cli/auth-check.ts`
//!
//! # 职责与边界
//! 本模块只实现 `kiana auth check` 的“查询 + 判定”，不做参数文案与进程退出：
//! `app::run_auth_command` 负责解析与打印，并把 [`AuthCheckStatus`] 映射为退出码
//! （`Ready` 0 / `NotReady` 1 / `Invalid` 2，见 `app.rs::execute_auth_command`）。
//! 打印类子命令（`print-api-key` / `print-bearer-token`）走 `cli/credential_print`，
//! 不经过本模块。调用方通常先用 [`create_auth_check_model_runtime`] 建运行时，再调用
//! [`check_provider_auth`]；[`get_provider_credential`] 只在 `--credentials` 时使用。
//!
//! # 判定流程（[`check_provider_auth`]）
//! 1. `validate_auth_command_args` 做参数校验，失败直接 `Err`；
//! 2. 给了 `--model` 就用 `resolve_cli_model` 解析 provider（`--provider` 作为提示），
//!    解析失败或没有模型 → `Err`；
//! 3. `ModelRuntime::get_error()` 非空 → `invalid/invalid_state`；
//! 4. provider 未注册 → `not_ready/provider_not_found`；
//! 5. `check_auth` 返回 `Err` → `invalid`；返回 `None` → `not_ready/credentials_not_configured`；
//! 6. 有认证结果且 `refresh` 为真时，再调用一次 `get_auth_for_provider` 走带刷新的读取：
//!    返回 `None` → `not_ready`，出错 → `invalid`，否则 `ready`。
//!
//! # 状态与 reason 的产生者
//! `provider_not_found` / `credentials_not_configured` / `invalid_state` 由本模块产生；
//! `credential_not_available` **不由本模块产生**——那是 `app.rs` 在 `--credentials`
//! 取不到（或取到空串）凭据后改写结果时设置的 reason。
//!
//! # 只读约束
//! 模型目录使用进程内实现 `InMemoryCodingAgentModelsStore`，并设置
//! `allow_model_network:false`、`refresh_on_create:false`，创建运行时不会拉取或写入
//! 模型目录。`auth check` 默认允许刷新 OAuth：刷新结果写回调用方提供的 `CredentialStore`
//! （`tests/auth_check.rs` 的 `refreshes_oauth_by_default` 以“存储里出现新 token”为
//! 可观察等价物）；`--no-refresh` 时 `app.rs` 传入 `ReadOnlyAuthStorage`，
//! `does_not_create_an_auth_file_or_its_parent_directory` 用同类只读存储断言不会创建
//! auth.json 及其父目录。

use std::sync::Arc;

use kiana_ai::AiError;
use kiana_ai::auth::types::{
    AuthError, AuthOperationOptions, AuthResult, AuthType, Credential, CredentialStore,
};
use kiana_ai::models::store::ModelsStore;
use kiana_config::auth::models_store::InMemoryCodingAgentModelsStore;
use kiana_config::models::resolver::{ResolveCliModelOptions, resolve_cli_model};
use kiana_config::models::runtime::{
    CreateModelRuntimeOptions, ModelRuntime, ModelRuntimeAuthOverrides, ModelsPathOption,
};

use crate::cli::args::Args;
use crate::cli::auth_command::{
    AuthCommandError, AuthCommandKind, get_auth_credential, validate_auth_command_args,
};

/// `AuthCheckStatus`。
///
/// 三态直接决定 `kiana auth check` 的退出码（映射在 `app.rs::execute_auth_command`）：
/// `Ready` → 0、`NotReady` → 1、`Invalid` → 2；`as_str` 的形态同时是 `--json` 输出的
/// `status` 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCheckStatus {
    /// `"ready"`：找到可用凭据；`AuthCheckResult::auth_type` 会一并给出。
    Ready,
    /// `"not_ready"`：provider 未注册或凭据未配置，可以通过配置修复。
    NotReady,
    /// `"invalid"`：运行时/读取失败，无法判断凭据状态，需要先修复环境。
    Invalid,
}

impl AuthCheckStatus {
    /// TS 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            AuthCheckStatus::Ready => "ready",
            AuthCheckStatus::NotReady => "not_ready",
            AuthCheckStatus::Invalid => "invalid",
        }
    }
}

impl std::fmt::Display for AuthCheckStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `AuthCheckReason`。
///
/// 只在 `NotReady` / `Invalid` 时出现，解释具体原因；`Ready` 时
/// `AuthCheckResult.reason` 为 `None`。字符串形态即 `--json` 输出的 `reason`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCheckReason {
    /// `"provider_not_found"`：运行时的 provider 列表里没有该 id。
    ProviderNotFound,
    /// `"credentials_not_configured"`：provider 存在，但 `check_auth` 没找到可用凭据
    /// （包括凭证里的环境变量引用无法解析、只读存储未配置该 provider）。
    CredentialsNotConfigured,
    /// `"credential_not_available"`：`--credentials` 请求的凭据为空或缺失；
    /// 由 `app.rs` 改写结果时设置，本模块不产生。
    CredentialNotAvailable,
    /// `"invalid_state"`：运行时配置/组合/可用性错误，或认证读取返回错误。
    InvalidState,
}

impl AuthCheckReason {
    /// TS 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            AuthCheckReason::ProviderNotFound => "provider_not_found",
            AuthCheckReason::CredentialsNotConfigured => "credentials_not_configured",
            AuthCheckReason::CredentialNotAvailable => "credential_not_available",
            AuthCheckReason::InvalidState => "invalid_state",
        }
    }
}

impl std::fmt::Display for AuthCheckReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `AuthCheckResult`。
///
/// `provider` 始终是最终目标 id（`--model` 解析成功后会被替换为模型所属 provider）。
/// `status` 决定其余字段的形态：`Ready` 时 `auth_type` 为 `Some`、`reason` 为 `None`；
/// `NotReady` / `Invalid` 时 `reason` 为 `Some`、`auth_type` 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheckResult {
    /// 状态。
    pub status: AuthCheckStatus,
    /// provider id。
    pub provider: String,
    /// 失败原因。
    pub reason: Option<AuthCheckReason>,
    /// 凭证类型（`ready` 时）。
    pub auth_type: Option<AuthType>,
}

/// `checkProviderAuth` 的选项（`{ refresh }`）。
///
/// 默认 `refresh: false`。为 `true` 时，在 `check_auth` 已找到认证结果后额外调用
/// `get_auth_for_provider` 走一次带刷新的读取；这次读取为空或出错会把结果降级为
/// `not_ready` / `invalid`。`auth_type` 始终来自第一次 `check_auth`，不受刷新影响。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuthCheckOptions {
    /// 是否刷新（默认 `false`）。
    pub refresh: bool,
}

/// `checkProviderAuth(args, modelRuntime, options)`。
///
/// # 错误
/// 只有“参数或目标 provider 无法确定”才返回 `Err(AuthCommandError)`：参数校验失败、
/// `--model` 解析失败（透传 `resolve_cli_model` 的 error）、解析结果没有模型、最终没有
/// provider。provider 的健康状态一律通过 `Ok(AuthCheckResult)` 表达，调用方据此决定
/// 退出码。
///
/// # 顺序与副作用
/// provider 解析先于任何运行时查询；`resolve_cli_model` 只读运行时已有的模型列表。
/// 查询阶段可能触发 OAuth 刷新并写回凭证存储（仅当 `options.refresh` 且认证已存在）。
/// 本函数不设置取消信号或超时（`AuthOperationOptions::default()`）；
/// [`create_auth_check_model_runtime`] 创建运行时同样没有传入 signal。
pub async fn check_provider_auth(
    args: &Args,
    model_runtime: &ModelRuntime,
    options: AuthCheckOptions,
) -> Result<AuthCheckResult, AuthCommandError> {
    let target = validate_auth_command_args(args, AuthCommandKind::Check)?;
    let mut provider = target.provider.clone();
    if let Some(cli_model) = target.model.as_deref() {
        let resolved = resolve_cli_model(&ResolveCliModelOptions {
            cli_provider: target.provider.clone(),
            cli_model: Some(cli_model.to_string()),
            cli_thinking: None,
            inline_api_key: false,
            model_runtime,
        });
        if let Some(error) = resolved.error {
            return Err(AuthCommandError::new(error));
        }
        let Some(model) = resolved.model else {
            return Err(AuthCommandError::new(format!(
                "Unable to resolve model \"{cli_model}\""
            )));
        };
        provider = Some(model.provider);
    }
    let Some(provider) = provider else {
        return Err(AuthCommandError::new("Unable to resolve an auth provider"));
    };

    if model_runtime.get_error().is_some() {
        return Ok(invalid_state(provider));
    }
    if model_runtime.get_provider(&provider).is_none() {
        return Ok(AuthCheckResult {
            status: AuthCheckStatus::NotReady,
            provider,
            reason: Some(AuthCheckReason::ProviderNotFound),
            auth_type: None,
        });
    }

    match model_runtime
        .check_auth(&provider, AuthOperationOptions::default())
        .await
    {
        Err(_) => Ok(invalid_state(provider)),
        Ok(None) => Ok(not_ready(provider)),
        Ok(Some(auth)) => {
            if options.refresh {
                match model_runtime
                    .get_auth_for_provider(&provider, ModelRuntimeAuthOverrides::default())
                    .await
                {
                    Ok(None) => return Ok(not_ready(provider)),
                    Err(_) => return Ok(invalid_state(provider)),
                    Ok(Some(_)) => {}
                }
            }
            Ok(AuthCheckResult {
                status: AuthCheckStatus::Ready,
                provider,
                reason: None,
                auth_type: Some(auth.r#type),
            })
        }
    }
}

/// 构造 `invalid/invalid_state` 结果，保留调用方给出的目标 provider id。
fn invalid_state(provider: String) -> AuthCheckResult {
    AuthCheckResult {
        status: AuthCheckStatus::Invalid,
        provider,
        reason: Some(AuthCheckReason::InvalidState),
        auth_type: None,
    }
}

/// 构造 `not_ready/credentials_not_configured` 结果（provider 本身存在，缺凭据）。
fn not_ready(provider: String) -> AuthCheckResult {
    AuthCheckResult {
        status: AuthCheckStatus::NotReady,
        provider,
        reason: Some(AuthCheckReason::CredentialsNotConfigured),
        auth_type: None,
    }
}

/// `getProviderCredential(providerId, modelRuntime, credentials, options)`。
///
/// 先直接用 `credentials.read(...)` 读存储：
/// - `options.refresh == false` 且读到的是 OAuth 凭证 → 直接返回 `access`，不经过
///   `ModelRuntime`，因此不会刷新、不会发起网络请求；`tests/auth_check.rs` 的
///   `reads_credentials_without_refreshing_oauth_when_requested` 锁定旧 token 原样返回；
/// - 其它情况委托 `modelRuntime.get_auth_for_provider`（该路径可能刷新并持久化 OAuth），
///   再用 [`get_auth_credential`] 从结果里提取 API key 或 Bearer token。
///
/// # 错误
/// `credentials.read` 与 `get_auth_for_provider` 的 `AuthError` 原样返回；调用方
/// （`app.rs`）把任何 `Err` 视为凭据不可用并改写为 `invalid/invalid_state`。
pub async fn get_provider_credential(
    provider_id: &str,
    model_runtime: &ModelRuntime,
    credentials: &dyn CredentialStore,
    options: AuthCheckOptions,
) -> Result<Option<String>, AuthError> {
    let credential = credentials
        .read(provider_id, AuthOperationOptions::default())
        .await?;
    if !options.refresh
        && let Some(Credential::OAuth(oauth)) = credential.as_ref()
    {
        return Ok(Some(oauth.access.clone()));
    }
    let auth: Option<AuthResult> = model_runtime
        .get_auth_for_provider(provider_id, ModelRuntimeAuthOverrides::default())
        .await?;
    Ok(get_auth_credential(auth.as_ref()))
}

/// `createAuthCheckModelRuntime(credentials)`。
///
/// `modelsPath` 未提供 → [`ModelsPathOption::Default`]（读 `$KIANA_CODING_AGENT_DIR/models.json`，
/// 该目录名由 `getAgentDir()` 决定），与原版一致；catalog 存储使用进程内实现，不落盘。
///
/// 其余固定选项：`allow_model_network:false`、`refresh_on_create:false`；凭证存储由
/// 调用方注入（`auth check` 用默认 `AuthStorage`，`--no-refresh` 时用只读实现）。
///
/// # 错误
/// 透传 `ModelRuntime::create` 的 `AiError`；调用方（`app.rs`）把它折叠为
/// `invalid/invalid_state`（退出码 2），而不是 `AuthCommandError`。
// 返回类型与 `ModelRuntime::create` 一致；`AiError` 体积较大，但这是 TS 异常面的等价形状，
// 调用方（app.rs）只在启动期调用一次，不做额外的 Box 包装。
#[allow(clippy::result_large_err)]
pub async fn create_auth_check_model_runtime(
    credentials: Arc<dyn CredentialStore>,
) -> Result<ModelRuntime, AiError> {
    let models_store: Arc<dyn ModelsStore> = Arc::new(InMemoryCodingAgentModelsStore::new());
    ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: ModelsPathOption::Default,
        models_store: Some(models_store),
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
}

/// `auth check --json` 的输出对象。
///
/// 字段顺序与 TS 的对象展开顺序一致：`status`, `provider`, `reason?`, `authType?`,
/// `credentials?`（见 spec/ca-cli.md §3.3）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AuthCheckJson<'a> {
    /// `status`。
    pub status: &'a str,
    /// `provider`。
    pub provider: &'a str,
    /// `reason`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
    /// `authType`。
    #[serde(rename = "authType", skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<&'a str>,
    /// `credentials`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<&'a str>,
}

impl<'a> AuthCheckJson<'a> {
    /// 由 [`AuthCheckResult`] 与可选凭据构造。
    pub fn from_result(result: &'a AuthCheckResult, credentials: Option<&'a str>) -> Self {
        Self {
            status: result.status.as_str(),
            provider: &result.provider,
            reason: result.reason.map(AuthCheckReason::as_str),
            auth_type: result.auth_type.map(AuthType::as_str),
            credentials,
        }
    }

    /// `JSON.stringify(value)`（字段序即声明序）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("AuthCheckJson is always serializable")
    }
}
