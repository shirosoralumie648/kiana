//! providers/cloudflare-auth.ts：Cloudflare Workers AI / AI Gateway 的 api-key auth。
//!
//! TS: `packages/ai/src/providers/cloudflare-auth.ts`
//!
//! # 为什么需要单独的 api-key auth
//!
//! 两家 Cloudflare provider 都不止需要一个 secret：
//!
//! - 请求 URL 里要填 `CLOUDFLARE_ACCOUNT_ID`；AI Gateway 还需要 `CLOUDFLARE_GATEWAY_ID`。
//!   这些值必须以 provider env 的形式返回，由
//!   [`crate::providers::cloudflare_stream::resolve_cloudflare_model`] 在发送前替换
//!   base URL 里的 `{...}` 占位符。
//! - AI Gateway 的鉴权头是 `cf-aig-authorization: Bearer <key>`，而不是 OpenAI 兼容的
//!   `Authorization`/`x-api-key`；同时还要**抑制**这两个默认头（`ProviderHeaders` 的
//!   值为 `None` 表示抑制，见 [`crate::types::ProviderHeaders`]）。
//!
//! 标准 `env_api_key_auth` 只会把 key 放进 `ModelAuth.api_key`，表达不了这些，因此这里
//! 自行实现 [`ApiKeyAuth`]。
//!
//! # 两个实现的差别
//!
//! 解析逻辑共用 `resolve_cloudflare_env`，只有三点不同：
//!
//! 1. AI Gateway 额外解析并要求 `CLOUDFLARE_GATEWAY_ID`；
//! 2. AI Gateway 的 `login` 多问一条 gateway id；
//! 3. `resolve` 的鉴权产物不同：Workers AI 返回 `ModelAuth.api_key`，AI Gateway 返回
//!    `ModelAuth.headers`（三个头，见上）。
//!
//! # 解析顺序（逐字段合并）
//!
//! `CLOUDFLARE_API_KEY` 取 `credential.key`，其余字段取 `credential.env[<name>]`。凭证里
//! 只要该字段存在（**空串也算存在**）就不再查环境变量；这复刻 TS 的
//! `value !== undefined` 语义，也让「只存了 key 的凭证」能继续从环境读到 account id。
//! 每次环境读取前后都检查取消信号。
//!
//! 最终 api key 或 account id 为空（AI Gateway 还要求 gateway id 非空）时返回
//! `Ok(None)`：表示 provider 未配置，不是错误；`Models` 会据此跳过该 provider。
//!
//! # 返回值
//!
//! `AuthResult.source`：存在凭证时 `"stored credential"`，否则 `"CLOUDFLARE_API_KEY"`。
//! `AuthResult.env` 只包含解析后的 account/gateway id（供占位符替换使用），不含 secret。

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthError, AuthInteraction,
    AuthPrompt, AuthPromptRequest, AuthResult, ModelAuth, throw_if_aborted,
};
use crate::types::ProviderEnv;
use crate::utils::abort::AbortScope;

const CLOUDFLARE_API_KEY: &str = "CLOUDFLARE_API_KEY";
const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

/// `CloudflareAuthKind`。
///
/// 只影响 `resolve_cloudflare_env` 是否读取/要求 gateway id；两个常量值没有其它语义。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloudflareAuthKind {
    WorkersAi,
    AiGateway,
}

/// `resolveValue(name, ctx, credential, signal)`：逐字段优先取凭证值，回退环境变量。
///
/// 返回 `Ok(Some(""))` 也是合法结果（凭证里存了空串）；调用方统一在
/// `resolve_cloudflare_env` 里按非空过滤。取消检查只在真正访问 `ctx.env` 时发生——
/// 凭证命中时不读环境，也就没有可取消的异步步骤。
async fn resolve_value(
    name: &str,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortScope,
) -> Result<Option<String>, AuthError> {
    // Per-field merge: prefer the credential value, fall back to ambient env.
    // A credential carrying only the API key must still pick up the account /
    // gateway id from the environment.
    //
    // 逐字段合并：凭证值优先，缺失才回退 ambient 环境；只存了 key 的凭证仍能从环境
    // 补齐 account / gateway id。
    let from_credential = match credential {
        Some(credential) if name == CLOUDFLARE_API_KEY => credential.key.clone(),
        Some(credential) => credential
            .env
            .as_ref()
            .and_then(|env| env.get(name))
            .cloned(),
        None => None,
    };
    if from_credential.is_some() {
        return Ok(from_credential);
    }
    throw_if_aborted(signal)?;
    let value = ctx.env(name).await;
    throw_if_aborted(signal)?;
    Ok(value)
}

/// `resolveCloudflareEnv(kind, ctx, credential, signal)`。
///
/// 成功时返回 `(api_key, env, source)`：`env` 至少含 `CLOUDFLARE_ACCOUNT_ID`，AI Gateway
/// 还会含 `CLOUDFLARE_GATEWAY_ID`；`source` 是给状态 UI 的展示标签。任何必需字段为空
/// 都返回 `Ok(None)`（provider 未配置），而不是 `Err`。
async fn resolve_cloudflare_env(
    kind: CloudflareAuthKind,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortScope,
) -> Result<Option<(String, ProviderEnv, String)>, AuthError> {
    let api_key = resolve_value(CLOUDFLARE_API_KEY, ctx, credential, signal).await?;
    let account_id = resolve_value(CLOUDFLARE_ACCOUNT_ID, ctx, credential, signal).await?;
    let gateway_id = if kind == CloudflareAuthKind::AiGateway {
        resolve_value(CLOUDFLARE_GATEWAY_ID, ctx, credential, signal).await?
    } else {
        None
    };

    // 必需字段非空才算配置完成：空串在 TS 里是假值，等价于未设置。
    let (Some(api_key), Some(account_id)) = (
        api_key.filter(|v| !v.is_empty()),
        account_id.filter(|v| !v.is_empty()),
    ) else {
        return Ok(None);
    };
    // AI Gateway 的 URL 模板必须有 gateway id；Workers AI 不读它，也不会返回空键。
    if kind == CloudflareAuthKind::AiGateway && !gateway_id.as_ref().is_some_and(|v| !v.is_empty())
    {
        return Ok(None);
    }

    // env 只回传解析后的 id（非 secret）：cloudflare_stream 用它替换 base URL 占位符。
    let mut env = IndexMap::new();
    env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
    if let Some(gateway_id) = gateway_id.filter(|v| !v.is_empty()) {
        env.insert(CLOUDFLARE_GATEWAY_ID.to_string(), gateway_id);
    }
    let source = if credential.is_some() {
        "stored credential"
    } else {
        CLOUDFLARE_API_KEY
    };
    Ok(Some((api_key, env, source.to_string())))
}

/// `cloudflareWorkersAIAuth()`。
///
/// Workers AI 的 api-key 实现：login 依次询问 API key（secret）与 account id（text）；
/// resolve 返回 `ModelAuth.api_key` + account id env。无状态，每次调用新建 `Arc`。
pub struct CloudflareWorkersAiAuth;

#[async_trait]
impl ApiKeyAuth for CloudflareWorkersAiAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        // Workers AI：两个必填值，account id 存进 env 供 URL 占位符替换。
        let key = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                message: "Enter Cloudflare API key".into(),
                placeholder: None,
            }))
            .await?;
        let account_id = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Enter Cloudflare account ID".into(),
                placeholder: None,
            }))
            .await?;
        let mut env = IndexMap::new();
        env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
        Ok(ApiKeyCredential {
            key: Some(key),
            env: Some(env),
            extra: IndexMap::new(),
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        // Workers AI：解析出 key + account env，鉴权走标准的 api_key。
        let resolved = resolve_cloudflare_env(
            CloudflareAuthKind::WorkersAi,
            input.ctx,
            input.credential,
            input.signal,
        )
        .await?;
        let Some((api_key, env, source)) = resolved else {
            return Ok(None);
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(api_key),
                ..Default::default()
            },
            env: Some(env),
            source: Some(source),
        }))
    }
}

/// `cloudflareAIGatewayAuth()`。
///
/// AI Gateway 的 api-key 实现：login 额外询问 gateway id；resolve 把 key 放进
/// `cf-aig-authorization` 头，并显式抑制 `Authorization`/`x-api-key`，避免通用 OpenAI
/// 适配器再带一份默认鉴权头（测试
/// `requires_cloudflare_ai_gateway_account_and_gateway_config_and_returns_scoped_env_headers`
/// 锁定了三个头的内容）。
pub struct CloudflareAiGatewayAuth;

#[async_trait]
impl ApiKeyAuth for CloudflareAiGatewayAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        // AI Gateway：三个必填值，account/gateway id 都进 env。
        let key = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                message: "Enter Cloudflare API key".into(),
                placeholder: None,
            }))
            .await?;
        let account_id = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Enter Cloudflare account ID".into(),
                placeholder: None,
            }))
            .await?;
        let gateway_id = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Enter Cloudflare AI Gateway ID".into(),
                placeholder: None,
            }))
            .await?;
        let mut env = IndexMap::new();
        env.insert(CLOUDFLARE_ACCOUNT_ID.to_string(), account_id);
        env.insert(CLOUDFLARE_GATEWAY_ID.to_string(), gateway_id);
        Ok(ApiKeyCredential {
            key: Some(key),
            env: Some(env),
            extra: IndexMap::new(),
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        // AI Gateway：解析出 key + account/gateway env，鉴权改为自定义头。
        let resolved = resolve_cloudflare_env(
            CloudflareAuthKind::AiGateway,
            input.ctx,
            input.credential,
            input.signal,
        )
        .await?;
        let Some((api_key, env, source)) = resolved else {
            return Ok(None);
        };
        let mut headers = IndexMap::new();
        // 真正携带 secret 的是 cf-aig-authorization；另外两个头用 `None` 抑制，防止
        // OpenAI 适配器把 api_key 再写成 Authorization / x-api-key（null 是「不要发送」，
        // 不是删除已有头，见 `types::ProviderHeaders` 的说明）。
        headers.insert(
            "cf-aig-authorization".to_string(),
            Some(format!("Bearer {api_key}")),
        );
        headers.insert("Authorization".to_string(), None);
        headers.insert("x-api-key".to_string(), None);
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: Some(headers),
                base_url: None,
            },
            env: Some(env),
            source: Some(source),
        }))
    }
}

/// `cloudflareWorkersAIAuth()`。
///
/// 返回 Workers AI 的 [`ApiKeyAuth`] 实例（无状态，可跨线程共享）；provider 工厂把它放进
/// `ProviderAuth.api_key`。
pub fn cloudflare_workers_ai_auth() -> std::sync::Arc<dyn ApiKeyAuth> {
    std::sync::Arc::new(CloudflareWorkersAiAuth)
}

/// `cloudflareAIGatewayAuth()`。
///
/// 返回 AI Gateway 的 [`ApiKeyAuth`] 实例（无状态）；与 Workers AI 版本的区别见模块文档。
pub fn cloudflare_ai_gateway_auth() -> std::sync::Arc<dyn ApiKeyAuth> {
    std::sync::Arc::new(CloudflareAiGatewayAuth)
}
