//! providers/anthropic.ts：`anthropic_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/anthropic.ts`
//!
//! # 为什么需要自定义 api-key auth
//!
//! 除了普通 API key，Anthropic 还接受两类非标准凭据：网关式的 bearer token
//! （`ANTHROPIC_AUTH_TOKEN`，请求头为 `Authorization: Bearer ...`，不是 `x-api-key`），
//! 以及 workload identity federation（SDK 自己拿 identity token 换短期 access token）。
//! 标准 [`crate::auth::helpers::env_api_key_auth`] 只会产出一个 `api_key`，因此本文件实现
//! `AnthropicApiKeyAuth`。
//!
//! # `resolve` 的优先级（从高到低）
//!
//! 1. 存储凭证的非空 `key` → `ModelAuth.api_key`，source `"stored credential"`；
//! 2. `ANTHROPIC_AUTH_TOKEN` → 请求头 `Authorization: Bearer <token>`，source 为该变量名；
//! 3. `ANTHROPIC_OAUTH_TOKEN` → `ModelAuth.api_key`，source 为该变量名；
//! 4. `ANTHROPIC_API_KEY` → `ModelAuth.api_key`，source 为该变量名；
//! 5. workload identity federation：`ANTHROPIC_FEDERATION_RULE_ID`、
//!    `ANTHROPIC_ORGANIZATION_ID`、`ANTHROPIC_IDENTITY_TOKEN_FILE` 三者必须同时非空，
//!    否则整体放弃并返回 `Ok(None)`；可选的 `ANTHROPIC_SERVICE_ACCOUNT_ID` /
//!    `ANTHROPIC_WORKSPACE_ID` 存在时一并放进 `env`。结果为 `ModelAuth::default()`
//!    + `env`，source `"workload identity federation"`；
//! 6. 全部未命中 → `Ok(None)`（provider 未配置，不是错误）。
//!
//! federation 放在最后是为了让显式 key / auth token 始终优先，与 Anthropic SDK 一致；
//! 那几个 id 属于 provider 配置而不是鉴权秘密，所以经 `env` 传给适配器，而不是塞进
//! `ModelAuth`。
//!
//! # 注意事项
//!
//! - 每次 `AuthContext::env` 读取前后都检查取消信号；空串按 JS 假值处理，继续向下回退。
//! - OAuth（Claude Pro/Max）走另一条路径：`ProviderAuth.oauth` 由 [`lazy_oauth`] 延迟
//!   加载 `load_anthropic_oauth`，`is_subscription = true`，不影响上面的 api-key 解析。
//! - provider 的 `base_url` 固定为 `https://api.anthropic.com`，模型可自带 `baseUrl` 覆盖。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::auth::helpers::{LazyOAuthInput, lazy_oauth};
use crate::auth::oauth::load::load_anthropic_oauth;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthError, AuthInteraction, AuthPrompt,
    AuthPromptRequest, AuthResult, ModelAuth, ProviderAuth, throw_if_aborted,
};
use crate::env_api_keys::{
    ANTHROPIC_API_KEY_ENV, ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_FEDERATION_RULE_ID_ENV,
    ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_ORGANIZATION_ID_ENV,
    ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV,
};
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;
use crate::types::ProviderEnv;

/// `anthropicApiKeyAuth()`。
///
/// 无状态：`resolve` 的结果只取决于入参凭证与 `AuthContext`，实例经 `Arc` 共享。
/// `name` 用于状态 UI；`login` 不走 `Enter <name>` 模板，提示固定为
/// `"Enter Anthropic API key"`（当前恰好与模板同文）。
struct AnthropicApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for AnthropicApiKeyAuth {
    fn name(&self) -> &str {
        "Anthropic API key"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        throw_if_aborted(interaction.signal())?;
        let key = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                message: "Enter Anthropic API key".into(),
                placeholder: None,
            }))
            .await?;
        throw_if_aborted(interaction.signal())?;
        Ok(ApiKeyCredential {
            key: Some(key),
            ..Default::default()
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let ApiKeyAuthInput {
            ctx,
            credential,
            signal,
        } = input;
        throw_if_aborted(signal)?;
        // `if (credential?.key)`：空字符串在 JS 里为假，落到 auth-token / env key / federation 分支。
        // 命中时 `credential.env` 原样带出，供适配器读取 provider 级配置。
        if let Some(key) = credential
            .and_then(|credential| credential.key.clone())
            .filter(|key| !key.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    ..Default::default()
                },
                env: credential.and_then(|credential| credential.env.clone()),
                source: Some("stored credential".into()),
            }));
        }

        // 阶段 2：ANTHROPIC_AUTH_TOKEN 是「网关 bearer」形态，必须走 Authorization 头，
        // 不能写成 api_key（否则适配器会发 x-api-key）。显式置 api_key=None 表示不使用
        // API key 鉴权。
        let auth_token = ctx.env(ANTHROPIC_AUTH_TOKEN_ENV).await;
        throw_if_aborted(signal)?;
        if let Some(auth_token) = auth_token.filter(|value| !value.is_empty()) {
            let mut headers = IndexMap::new();
            headers.insert("Authorization".into(), Some(format!("Bearer {auth_token}")));
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: None,
                    headers: Some(headers),
                    base_url: None,
                },
                env: None,
                source: Some(ANTHROPIC_AUTH_TOKEN_ENV.into()),
            }));
        }

        // 阶段 3/4：OAuth token 与 API key 都只作为 api_key 返回；Anthropic 适配器在 token
        // 包含 `sk-ant-oat` 时会切换成 OAuth 请求形态（Bearer + oauth beta 头）。
        for env_var in [ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV] {
            let api_key = ctx.env(env_var).await;
            throw_if_aborted(signal)?;
            if let Some(api_key) = api_key.filter(|value| !value.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(api_key),
                        ..Default::default()
                    },
                    env: None,
                    source: Some(env_var.into()),
                }));
            }
        }

        // Workload identity federation: the Anthropic SDK exchanges the identity
        // token for a short-lived access token and refreshes it itself. Last in
        // line so keys and ANTHROPIC_AUTH_TOKEN keep winning, as in the SDK. The
        // ids are provider config rather than auth, so they travel in `env`.
        // 阶段 5：federation。三个必填变量按固定顺序读取，缺一就整体放弃（返回
        // Ok(None)），不会返回部分 env；source 也不会出现。
        let mut federation = ProviderEnv::new();
        for env_var in [
            ANTHROPIC_FEDERATION_RULE_ID_ENV,
            ANTHROPIC_ORGANIZATION_ID_ENV,
            ANTHROPIC_IDENTITY_TOKEN_FILE_ENV,
        ] {
            let value = ctx.env(env_var).await;
            throw_if_aborted(signal)?;
            let Some(value) = value.filter(|value| !value.is_empty()) else {
                return Ok(None);
            };
            federation.insert(env_var.to_string(), value);
        }
        // service account / workspace 是可选补充：只有非空才写入 env，不影响 federation
        // 是否成立（测试 `passes_workspace_id_through_and_treats_service_account_as_optional`
        // 锁定了该行为）。
        for env_var in [ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV] {
            let value = ctx.env(env_var).await;
            throw_if_aborted(signal)?;
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                federation.insert(env_var.to_string(), value);
            }
        }
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: Some(federation),
            source: Some("workload identity federation".into()),
        }))
    }
}

/// 构造 `anthropic` provider（展示名 `Anthropic`，默认端点 `https://api.anthropic.com`）。
///
/// 同时登记两种鉴权：api-key（上面的 `AnthropicApiKeyAuth`）与 OAuth（`lazy_oauth`，
/// 展示名 `Anthropic (Claude Pro/Max)`，标记为订阅制）。chat 模型统一走
/// `anthropic-messages` 适配器；目录来自编译期生成的 `anthropic` chat 表。
///
/// `expect` 分支不可达：provider 声明了 `api`。
///
/// TS: `providers/anthropic.ts#anthropicProvider`
pub fn anthropic_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "anthropic".into(),
        name: Some("Anthropic".into()),
        base_url: Some("https://api.anthropic.com".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(AnthropicApiKeyAuth)),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "Anthropic (Claude Pro/Max)".into(),
                is_subscription: Some(true),
                login_label: None,
                load: Arc::new(load_anthropic_oauth),
            })),
        },
        models: catalog_chat_models("anthropic"),
        api: Some(anthropic_messages_api().into()),
        ..Default::default()
    })
    .expect("anthropic provider declares an api implementation")
}
