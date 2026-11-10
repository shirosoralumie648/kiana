//! `cli/credential-print.ts`（90 行）：`resolveCredentialForPrint`。
//!
//! # 为什么需要
//! `kiana auth print-api-key` / `print-bearer-token` 要把“真实请求会使用的凭证”原样打到
//! stdout，供脚本或其它进程消费。因此它不能只读存储里的原始条目，而要走与请求相同的解析
//! 路径（环境变量、凭证存储、OAuth 刷新等）；否则打印值与实际请求可能不一致。
//!
//! # 执行流程
//! 1. [`validate_auth_command_args`] 规范化 `--provider`/`--model`，并拒绝该子命令不接受的
//!    其它参数；
//! 2. `list_credentials` 枚举已存储凭证，建立 “provider → [`AuthType`]” 映射；
//! 3. 构造候选 provider：显式 `--provider` 时只有一个；只有 `--model` 时在映射中列出的
//!    provider 里逐个做模糊匹配；
//! 4. 按子命令过滤凭证类型（API key 打印跳过 OAuth，bearer token 打印跳过非 OAuth），
//!    调 `get_auth_for_model`/`get_auth_for_provider` 解析凭证，再用
//!    [`get_auth_credential`] 提取文本；
//! 5. 恰好一个结果才返回；零个或多个都转成面向用户的 [`AuthCommandError`]。
//!
//! # 失败模式
//! - 任何 [`AuthError`]（凭证存储读取失败、OAuth 刷新/网络错误等）都被折叠成固定
//!   文案 `"Failed to resolve credential"`，对应 TS 只对 `AuthCommandError` 打印原文的行为
//!   （见 `auth_error_to_print_error`）。
//! - 本函数不写任何输出，也不决定退出码；打印凭证与映射退出码是调用方的事
//!   （`crate::app` 的 `execute_auth_command` 在非 `check` 分支调用它）。
//!
//! TS: `packages/coding-agent/src/cli/credential-print.ts`

use kiana_ai::auth::types::{AuthError, AuthOperationOptions, AuthType, CredentialInfo};
use kiana_ai::types::Model;
use kiana_ai::utils::abort::AbortScope;
use kiana_config::models::resolver::{
    ModelReferenceRequest, ResolveCliModelOptions, ThinkingSuffixMode, resolve_cli_model,
    resolve_model_reference,
};
use kiana_config::models::runtime::{ModelRuntime, ModelRuntimeAuthOverrides};

use crate::cli::args::Args;
use crate::cli::auth_command::{
    AuthCommandError, AuthCommandKind, get_auth_credential, validate_auth_command_args,
};

/// `DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS = 30 * 60_000`。
///
/// `print-bearer-token` 未显式给出 `--min-expiry` 时的默认有效期下限（毫秒）。只有
/// `BearerToken` 路径会把它写进 `min_oauth_validity_ms`；`ApiKey` 路径不设置该字段。
pub const DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS: i64 = 30 * 60_000;

/// 打印类子命令（`check` 之外）。
pub type CredentialPrintKind = AuthCommandKind;

/// `resolveCredentialForPrint(args, modelRuntime, kind, minExpiryMs?, signal?)`。
///
/// 会调用 `ModelRuntime.getAuth()`：OAuth 凭证在常规请求路径上会被刷新并持久化。
///
/// # 参数
/// - `args`：原始 CLI 参数。只有 `--provider`/`--model` 参与解析，其余字段由
///   [`validate_auth_command_args`] 检查；
/// - `kind`：打印类子命令（[`AuthCommandKind::ApiKey`] 或
///   [`AuthCommandKind::BearerToken`]）；它既决定过滤哪种凭证，也决定错误文案；
/// - `min_expiry_ms`：仅 `BearerToken` 使用，缺省取
///   [`DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS`]；`ApiKey` 忽略该参数；
/// - `signal`：同时交给凭证枚举与 `get_auth`，用于取消。
///
/// # 返回
/// 恰好一个匹配 provider 的凭证文本。API key 直接返回 `apiKey`；bearer token 从
/// `Authorization` 头中提取，规则见 [`get_auth_credential`]。
///
/// # 错误
/// 参数非法、provider 不存在、模型找不到、没有匹配凭证或多个 provider 同时匹配时返回
/// [`AuthCommandError`]；底层 [`AuthError`] 一律折叠成固定文案（见模块文档）。
pub async fn resolve_credential_for_print(
    args: &Args,
    model_runtime: &ModelRuntime,
    kind: CredentialPrintKind,
    min_expiry_ms: Option<i64>,
    signal: Option<AbortScope>,
) -> Result<String, AuthCommandError> {
    let target = validate_auth_command_args(args, kind)?;
    let credential_infos: Vec<CredentialInfo> = model_runtime
        .list_credentials(AuthOperationOptions {
            signal: signal.clone(),
        })
        .await
        .map_err(auth_error_to_print_error)?;
    let credential_types: std::collections::HashMap<String, AuthType> = credential_infos
        .into_iter()
        .map(|credential| (credential.provider_id, credential.r#type))
        .collect();

    // 候选 provider 与（可选的）已解析模型：有 model 时按模型解析 auth，否则按 provider。
    struct Candidate {
        id: String,
        model: Option<Model>,
    }
    let mut providers: Vec<Candidate> = Vec::new();

    if let Some(cli_provider) = target.provider.as_deref() {
        // 显式 --provider：必须能在运行时里找到（provider 列表包含合成/虚拟 provider）。
        let Some(provider) = model_runtime.get_provider(cli_provider) else {
            return Err(AuthCommandError::new(format!(
                "Unknown provider \"{cli_provider}\". Use --list-models to see available providers."
            )));
        };
        if let Some(cli_model) = target.model.as_deref() {
            // 同时给出 --model：先解析成具体模型；解析失败按 TS 的错误文案原样上报。
            let resolved = resolve_cli_model(&ResolveCliModelOptions {
                cli_provider: Some(provider.id().to_string()),
                cli_model: Some(cli_model.to_string()),
                cli_thinking: None,
                inline_api_key: false,
                model_runtime,
            });
            if let Some(error) = resolved.error {
                return Err(AuthCommandError::new(error));
            }
            let Some(model) = resolved.model else {
                return Err(AuthCommandError::new(
                    "Unable to resolve the requested provider/model",
                ));
            };
            providers.push(Candidate {
                id: provider.id().to_string(),
                model: Some(model),
            });
        } else {
            providers.push(Candidate {
                id: provider.id().to_string(),
                model: None,
            });
        }
    } else {
        // validate 已保证 cliModel 存在。
        // 仅 --model：只遍历 `list_credentials` 返回的 provider；不在映射里的 provider
        // 无法确定凭证类型，直接跳过。候选模型来自运行时的全部模型。
        let cli_model = target.model.as_deref().unwrap_or_default();
        let models = model_runtime.get_models(None);
        for provider in model_runtime.get_providers() {
            if !credential_types.contains_key(provider.id()) {
                continue;
            }
            let resolved = resolve_model_reference(
                &ModelReferenceRequest {
                    text: cli_model.to_string(),
                    provider: Some(provider.id().to_string()),
                    thinking_suffix: ThinkingSuffixMode::Strict,
                    fuzzy: true,
                },
                &models,
                &|_| true,
            );
            if resolved.status() == "resolved"
                && let Some(model) = resolved.resolved_model()
            {
                providers.push(Candidate {
                    id: provider.id().to_string(),
                    model: Some(model.clone()),
                });
            }
        }
        if providers.is_empty() {
            return Err(AuthCommandError::new(format!(
                "Model \"{cli_model}\" not found. Use --list-models to see available models."
            )));
        }
    }

    let mut credentials: Vec<(String, String)> = Vec::new();
    for provider in &providers {
        // 凭证类型过滤：类型未知（未存储凭证）时，API key 路径仍会尝试解析，而
        // bearer token 路径直接跳过。
        let credential_type = credential_types.get(&provider.id).copied();
        if kind == AuthCommandKind::ApiKey && credential_type == Some(AuthType::OAuth) {
            continue;
        }
        if kind == AuthCommandKind::BearerToken && credential_type != Some(AuthType::OAuth) {
            continue;
        }
        // bearer token 导出要求最低剩余有效期；API key 不设置该下限。
        let overrides = ModelRuntimeAuthOverrides {
            min_oauth_validity_ms: (kind == AuthCommandKind::BearerToken)
                .then_some(min_expiry_ms.unwrap_or(DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS)),
            signal: signal.clone(),
            ..Default::default()
        };
        let auth = match &provider.model {
            Some(model) => model_runtime.get_auth_for_model(model, overrides).await,
            None => {
                model_runtime
                    .get_auth_for_provider(&provider.id, overrides)
                    .await
            }
        }
        .map_err(auth_error_to_print_error)?;
        if let Some(value) = get_auth_credential(auth.as_ref()) {
            credentials.push((provider.id.clone(), value));
        }
    }

    if credentials.len() == 1 {
        return Ok(credentials[0].1.clone());
    }
    if credentials.is_empty() {
        // 显式指定 provider 时，把“类型不匹配”解释成比“没有可用凭证”更具体的错误。
        let provider_id = providers.first().map(|provider| provider.id.clone());
        let credential_type = provider_id
            .as_ref()
            .and_then(|provider_id| credential_types.get(provider_id))
            .copied();
        if target.provider.is_some()
            && kind == AuthCommandKind::ApiKey
            && credential_type == Some(AuthType::OAuth)
        {
            return Err(AuthCommandError::new(format!(
                "Provider \"{}\" is configured with OAuth, not an API key",
                provider_id.unwrap_or_default()
            )));
        }
        if target.provider.is_some()
            && kind == AuthCommandKind::BearerToken
            && credential_type != Some(AuthType::OAuth)
        {
            return Err(AuthCommandError::new(format!(
                "Provider \"{}\" is not configured with an OAuth bearer token",
                provider_id.unwrap_or_default()
            )));
        }
        return Err(AuthCommandError::new(format!(
            "No usable {} is configured",
            if kind == AuthCommandKind::ApiKey {
                "API key"
            } else {
                "OAuth bearer token"
            }
        )));
    }
    // 多个 provider 都命中：拒绝猜测，要求用户用 --provider 收窄；列表保持 push 顺序。
    Err(AuthCommandError::new(format!(
        "Multiple configured providers matched ({}). Specify --provider.",
        credentials
            .iter()
            .map(|(provider_id, _)| provider_id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// TS 的打印路径把任何非 `AuthCommandError` 的异常折叠成固定消息
/// （main.ts:204：`error instanceof AuthCommandError ? error.message : "Failed to resolve credential"`）。
/// 该判断只影响**展示文本**（退出码取决于子命令种类），因此这里直接把非
/// `AuthCommandError` 的 `AuthError` 映射成同一条消息，行为字节一致。
const FAILED_TO_RESOLVE_CREDENTIAL: &str = "Failed to resolve credential";

/// kiana-ai 的 [`AuthError`]（凭证存储读取失败、OAuth 刷新/网络错误等）在 TS 里都不是
/// `AuthCommandError`，因此展示 `"Failed to resolve credential"`。
fn auth_error_to_print_error(_error: AuthError) -> AuthCommandError {
    AuthCommandError::new(FAILED_TO_RESOLVE_CREDENTIAL)
}
