//! providers/amazon-bedrock.ts：`amazon_bedrock_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/amazon-bedrock.ts`
//!
//! # 这个 provider 有什么特殊
//!
//! Bedrock 没有单一的「API key」概念：请求可以用 bearer token，也可以走 AWS 默认凭据链
//! （共享 profile、环境里的 AK/SK、ECS task role、web identity 等）。因此本文件自带
//! `BedrockAuth`（一个 [`ApiKeyAuth`] 实现），而不是复用 `env_api_key_auth`：
//!
//! - `login` 让用户三选一：bearer token、AWS profile、或「已自行配置好凭据链」；
//! - `resolve` 按固定顺序探测存储凭证与 ambient 环境，只回答「这个 provider 当前是否
//!   已配置、用哪种凭据」，真正的签名与取凭证由 `bedrock-converse-stream` 适配器完成。
//!
//! # 解析顺序（与 spec/ai-core.md §2.3.5 一致）
//!
//! 1. 存储凭证里的非空 `key` → `ModelAuth.api_key`（Bearer token）；
//! 2. `AWS_BEARER_TOKEN_BEDROCK` 非空 → 已配置，但不复制 key（适配器自己读环境）；
//! 3. stored `AWS_PROFILE`，否则 ambient `AWS_PROFILE` → 已配置，并把凭证里的 `env`
//!    原样传给请求层；
//! 4. `AWS_ACCESS_KEY_ID` 与 `AWS_SECRET_ACCESS_KEY` 同时非空 → "AWS access keys"；
//! 5. `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `..._FULL_URI` → "ECS task role"；
//! 6. `AWS_WEB_IDENTITY_TOKEN_FILE` → "web identity token"；
//! 7. 都没有 → `Ok(None)`，表示未配置（不是错误）。
//!
//! # 约束与失败模式
//!
//! - 每次读取 `AuthContext::env` 前后都检查取消信号（见私有 `env` 函数）：环境实现可能
//!   是异步且耗时的，只查入口会漏掉中途取消。
//! - ambient 凭据不会被写入 kiana 的凭证存储；`source` 只是状态 UI 的展示标签。
//! - 除交互层自身的错误/取消外，`login` 只在用户选择未知方法时返回错误；正常路径返回的
//!   凭证可以只有 `env`、没有 `key`。
//! - provider 不声明 `base_url`：端点由适配器按 region/profile 与模型自身的 `baseUrl` 解析。
//!
//! 调用方：[`crate::providers::all::builtin_providers`] 按固定顺序实例化本工厂。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::api::bedrock_converse_stream::bedrock_converse_stream_api;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthError, AuthEvent, AuthInfoLink,
    AuthInteraction, AuthPrompt, AuthPromptRequest, AuthResult, AuthSelectOption, ModelAuth,
    ProviderAuth, throw_if_aborted,
};
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;
use crate::utils::abort::AbortScope;

/// Bedrock accepts a bearer token or the AWS SDK's default credential chain.
///
/// The login flow can store a token/profile choice; resolve also detects ambient
/// AWS credentials without copying them into kiana's credential store.
///
/// 无状态实现：`resolve` 的结果完全由入参凭证与 `AuthContext` 决定，实例可跨线程共享
/// （`ApiKeyAuth` 要求 `Send + Sync`，`Arc<BedrockAuth>` 被 provider 的 `ProviderAuth`
/// 长期持有）。
struct BedrockAuth;

/// TS 的 `const env = async (name) => { signal.throwIfAborted(); ...; signal.throwIfAborted(); ... }`。
///
/// 返回 `Ok(None)` 表示环境变量不存在（或实现返回了 `None`）；空串由调用方按 JS 真值
/// 语义另行过滤。两次取消检查分别覆盖「进入 await 之前」与「await 返回之后」——后者保证
/// 用户在中途取消时不会把刚读到的值当成成功结果继续使用。
async fn env(
    ctx: &dyn AuthContext,
    name: &str,
    signal: &AbortScope,
) -> Result<Option<String>, AuthError> {
    throw_if_aborted(signal)?;
    let value = ctx.env(name).await;
    throw_if_aborted(signal)?;
    Ok(value)
}

#[async_trait]
impl ApiKeyAuth for BedrockAuth {
    fn name(&self) -> &str {
        "AWS credentials or bearer token"
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        let signal = interaction.signal();
        throw_if_aborted(signal)?;
        // 第一步：选择鉴权方式。`prompt` 返回后再次检查取消，避免把取消期间输入的答案
        // 当作有效选择继续走。
        let method = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: "Select Amazon Bedrock authentication method:".into(),
                options: vec![
                    AuthSelectOption {
                        id: "bearer-token".into(),
                        label: "Bearer token".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "aws-profile".into(),
                        label: "AWS profile".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "credential-chain".into(),
                        label: "Existing AWS credential chain".into(),
                        description: None,
                    },
                ],
            }))
            .await?;
        throw_if_aborted(signal)?;
        // bearer-token：只把 token 存进 key；请求层会把它当作 Bedrock bearer token。
        if method == "bearer-token" {
            let key = interaction
                .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                    message: "Enter Amazon Bedrock bearer token".into(),
                    placeholder: None,
                }))
                .await?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                ..Default::default()
            });
        }
        // 其余两种方法都依赖 AWS 自己的凭证链；先发一条 info 事件（带官方文档链接），
        // 让用户知道 kiana 不会收集 AK/SK，也不会代替 SDK 的角色扮演流程。
        interaction.notify(&AuthEvent::Info {
            message:
                "Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials."
                    .into(),
            links: Some(vec![AuthInfoLink {
                label: Some("AWS credential provider chain".into()),
                url:
                    "https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html"
                        .into(),
            }]),
        });
        // aws-profile：只存 profile 名。真正的 AK/SK 留在 ~/.aws/credentials（或
        // AWS_SHARED_CREDENTIALS_FILE 指向的文件）里，由适配器按 profile 读取。
        if method == "aws-profile" {
            let profile = interaction
                .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                    message: "Enter AWS profile name".into(),
                    placeholder: None,
                }))
                .await?;
            let mut provider_env = IndexMap::new();
            provider_env.insert("AWS_PROFILE".into(), profile);
            return Ok(ApiKeyCredential {
                key: None,
                env: Some(provider_env),
                extra: IndexMap::new(),
            });
        }
        // 方法名不在上面两个分支里时只接受 credential-chain；其他字符串说明交互层传了
        // 非法选项，按错误返回而不是静默降级。
        if method != "credential-chain" {
            return Err(AuthError::new(format!(
                "Unknown Amazon Bedrock auth method: {method}"
            )));
        }
        // credential-chain：用户确认已在外部（环境变量、SSO、实例角色等）配置好凭据。
        // 只等一次回车，返回空凭证；后续 resolve 会重新探测环境，判断是否真的可用。
        interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Configure AWS credentials, then press Enter to continue".into(),
                placeholder: None,
            }))
            .await?;
        Ok(ApiKeyCredential::default())
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let ApiKeyAuthInput {
            ctx,
            credential,
            signal,
        } = input;
        // 阶段 1：存储的 bearer token。空串按「未设置」处理，继续向下探测 ambient。
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
        // 阶段 2：ambient bearer token。这里只回答「已配置」，不把 token 复制进
        // ModelAuth.api_key——适配器自己会从作用域 env / 进程 env 读取
        // AWS_BEARER_TOKEN_BEDROCK 并改用 Bearer 鉴权。
        if env(ctx, "AWS_BEARER_TOKEN_BEDROCK", signal)
            .await?
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("AWS_BEARER_TOKEN_BEDROCK".into()),
            }));
        }
        // 阶段 3：profile。存储凭证里的 AWS_PROFILE 优先于环境变量；注意这里用
        // `Option` 区分「键存在」与「值可用」，空串仍会阻止继续查环境（与 TS 的
        // `credential?.env?.AWS_PROFILE ?? env("AWS_PROFILE")` 一致），随后被下面的
        // 非空判断丢弃，落到静态 AK/SK 分支。
        let stored_profile = credential
            .and_then(|credential| credential.env.as_ref())
            .and_then(|env| env.get("AWS_PROFILE"))
            .cloned();
        let profile = match stored_profile.as_deref() {
            Some(value) => Some(value.to_string()),
            None => env(ctx, "AWS_PROFILE", signal).await?,
        };
        // 阶段 3 的结果判定：非空 profile 即视为已配置。`env` 原样带出（适配器会读
        // `AWS_PROFILE` 定位共享凭证文件）；source 用来区分用户存过 profile 还是环境碰巧有。
        if profile.is_some_and(|value| !value.is_empty()) {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: credential.and_then(|credential| credential.env.clone()),
                source: Some(
                    if stored_profile.is_some_and(|value| !value.is_empty()) {
                        "stored credential"
                    } else {
                        "AWS_PROFILE"
                    }
                    .into(),
                ),
            }));
        }
        // 阶段 4：环境里的显式访问密钥。两个变量必须同时非空才算配置完整。
        if env(ctx, "AWS_ACCESS_KEY_ID", signal)
            .await?
            .is_some_and(|value| !value.is_empty())
            && env(ctx, "AWS_SECRET_ACCESS_KEY", signal)
                .await?
                .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("AWS access keys".into()),
            }));
        }
        // 阶段 5：ECS task role。只要相对/完整 URI 之一存在，就认为凭据由容器运行时提供；
        // 真正取凭据的 HTTP 调用由适配器的凭据链负责。
        for name in [
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        ] {
            if env(ctx, name, signal)
                .await?
                .is_some_and(|value| !value.is_empty())
            {
                return Ok(Some(AuthResult {
                    auth: ModelAuth::default(),
                    env: None,
                    source: Some("ECS task role".into()),
                }));
            }
        }
        // 阶段 6：web identity（EKS IRSA 等）。文件路径存在即视为已配置；token 的交换
        // 与刷新不在本函数内做。
        if env(ctx, "AWS_WEB_IDENTITY_TOKEN_FILE", signal)
            .await?
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: None,
                source: Some("web identity token".into()),
            }));
        }
        // 阶段 7：所有来源都为空 → 未配置。`Models` 会据此跳过该 provider。
        Ok(None)
    }
}

/// 构造 `amazon-bedrock` provider（展示名 `Amazon Bedrock`）。
///
/// 目录来自编译期生成的 `amazon-bedrock` chat 表；所有模型走 `bedrock-converse-stream`
/// 适配器；鉴权用上面的 `BedrockAuth`。provider 不设置 `base_url`，端点留给适配器按
/// region/profile 解析。
///
/// `expect` 分支不可达：`create_provider` 只在 api/images/classifiers 全为空时返回 `Err`，
/// 而这里显式声明了 `api`。
///
/// TS: `providers/amazon-bedrock.ts#amazonBedrockProvider`
pub fn amazon_bedrock_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "amazon-bedrock".into(),
        name: Some("Amazon Bedrock".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(BedrockAuth)),
            oauth: None,
        },
        models: catalog_chat_models("amazon-bedrock"),
        api: Some(bedrock_converse_stream_api().into()),
        ..Default::default()
    })
    .expect("amazon-bedrock provider declares an api implementation")
}
