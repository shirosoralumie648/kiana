//! providers/google-vertex.ts：`google_vertex_provider` provider 工厂。
//!
//! TS: `packages/ai/src/providers/google-vertex.ts`
//!
//! # 特殊之处
//!
//! 多数只支持 API key 的 provider 复用 `env_api_key_auth`；少数（Bedrock、Anthropic、
//! OpenAI Codex、Cloudflare 等）使用自定义鉴权或只有 OAuth。Vertex 的身份有两条来源：
//! 显式 API key，或 Google Application Default Credentials（ADC）。ADC 本身不向请求头填
//! token——真正的 OAuth2 token 由 `api/google_vertex` 在请求时按 ADC 文件自行获取。因此
//! 这里的鉴权只负责「判断哪种方式可用」，并把 project/location/credentials 路径经
//! `AuthResult.env` 传给下游（下游用 `get_provider_env_value` 读取）。
//!
//! # 登录（`login`）流程
//!
//! 1. 选择 `api-key` → 读取 secret，返回只带 `key` 的凭证；
//! 2. 选择 `adc` 或 `service-account` → 依次询问 project、location
//!    （service-account 额外问文件路径），写进 `ApiKeyCredential.env`，`key` 留空；
//! 3. 其他选项返回 `Unknown Google Vertex AI auth method: {method}`。
//!
//! # 解析（`resolve`）优先级
//!
//! 1. 凭证里的 `key` > `GOOGLE_CLOUD_API_KEY`；命中即用 API key，`env` 为 `None`；
//! 2. 否则确定 ADC 文件：凭证 `env.GOOGLE_APPLICATION_CREDENTIALS` > 进程同名变量 >
//!    默认 `VERTEX_ADC_PATH`（`~/.config/gcloud/application_default_credentials.json`）；
//! 3. project：凭证 env > `GOOGLE_CLOUD_PROJECT` > `GCLOUD_PROJECT`；location：凭证 env >
//!    `GOOGLE_CLOUD_LOCATION`；
//! 4. 文件存在且 project/location 非空才算配置完成：返回空 `ModelAuth` 与原样 `env`，
//!    让下游走 ADC；否则返回 `None`（视为未配置，不报错）。
//!
//! `env()` 辅助函数在每个 await 前后都检查取消信号，与 TS 的 `signal.throwIfAborted()`
//! 检查点一一对应；`js_truthy` 复刻 JS 把空字符串视为 falsy 的判定。
//!
//! 调用方：[`crate::providers::all::builtin_providers`]；模型目录来自
//! `providers/data/google-vertex.json`（由 `build.rs` 编成静态表）。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::api::google_vertex::google_vertex_api;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthError, AuthEvent, AuthInfoLink,
    AuthInteraction, AuthPrompt, AuthPromptRequest, AuthResult, AuthSelectOption, ModelAuth,
    ProviderAuth, throw_if_aborted,
};
use crate::models::{CreateProviderOptions, Provider, create_provider};
use crate::providers::catalog_chat_models;
use crate::utils::abort::AbortScope;

/// `~/.config/gcloud/application_default_credentials.json`。
const VERTEX_ADC_PATH: &str = "~/.config/gcloud/application_default_credentials.json";

/// Vertex accepts an explicit API key or Application Default Credentials
/// (`gcloud auth application-default login`). ADC additionally requires
/// project and location env vars, which the implementation reads itself.
///
/// 登录入口只有 `login`（API key 或 ADC/service-account 元数据）；`resolve` 不联网，
/// 只读凭证与 `AuthContext`。`name` 与 `has_login` 供状态 UI/登录菜单使用。
struct VertexAuth;

/// JS 的 truthiness 判定（对字符串只把空串当 falsy）。
///
/// TS 里 `if (project && location)` 用的是原始字符串；Rust 的 `Option<String>` 没有同等
/// 语义，必须显式排除空串，否则 `GOOGLE_CLOUD_PROJECT=""` 会被误判为已配置。
fn js_truthy(value: Option<&String>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// TS 的 `const env = async (name) => { signal.throwIfAborted(); ...; signal.throwIfAborted(); ... }`。
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
impl ApiKeyAuth for VertexAuth {
    fn name(&self) -> &str {
        "Google Cloud credentials"
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
        let method = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: "Select Google Vertex AI authentication method:".into(),
                options: vec![
                    AuthSelectOption {
                        id: "api-key".into(),
                        label: "Google Cloud API key".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "adc".into(),
                        label: "Application Default Credentials".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: "service-account".into(),
                        label: "Service account credentials file".into(),
                        description: None,
                    },
                ],
            }))
            .await?;
        throw_if_aborted(signal)?;
        // API key 分支只保存 key；下面的 ADC 分支则只保存 env，两者互斥。
        if method == "api-key" {
            let key = interaction
                .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                    message: "Enter Google Cloud API key".into(),
                    placeholder: None,
                }))
                .await?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                ..Default::default()
            });
        }
        if method != "adc" && method != "service-account" {
            return Err(AuthError::new(format!(
                "Unknown Google Vertex AI auth method: {method}"
            )));
        }
        interaction.notify(&AuthEvent::Info {
            message: if method == "adc" {
                "Run `gcloud auth application-default login`, then provide the project and location."
            } else {
                "Provide a service account credentials file, project, and location."
            }
            .into(),
            links: Some(vec![AuthInfoLink {
                label: Some("Application Default Credentials".into()),
                url: "https://cloud.google.com/docs/authentication/provide-credentials-adc".into(),
            }]),
        });
        let project = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Enter Google Cloud project ID".into(),
                placeholder: None,
            }))
            .await?;
        let location = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "Enter Google Cloud location".into(),
                placeholder: None,
            }))
            .await?;
        let credentials_path = if method == "service-account" {
            Some(
                interaction
                    .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                        message: "Enter service account credentials file path".into(),
                        placeholder: None,
                    }))
                    .await?,
            )
        } else {
            None
        };
        let mut provider_env = IndexMap::new();
        provider_env.insert("GOOGLE_CLOUD_PROJECT".into(), project);
        provider_env.insert("GOOGLE_CLOUD_LOCATION".into(), location);
        // service-account 的空路径不写进 env：TS 用 `credentialsPath ? {...} : {}` 条件
        // 展开，空串不产生该键；Rust 因而显式 filter 掉空路径。
        if let Some(credentials_path) = credentials_path.filter(|path| !path.is_empty()) {
            provider_env.insert("GOOGLE_APPLICATION_CREDENTIALS".into(), credentials_path);
        }
        Ok(ApiKeyCredential {
            key: None,
            env: Some(provider_env),
            extra: IndexMap::new(),
        })
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let ApiKeyAuthInput {
            ctx,
            credential,
            signal,
        } = input;
        let stored_env = credential.and_then(|credential| credential.env.as_ref());
        let stored_key = credential.and_then(|credential| credential.key.clone());
        // 第 1 优先级：存储的 key，其次 GOOGLE_CLOUD_API_KEY。空串在随后的 filter 里
        // 按未提供处理（与 TS 的 `if (key)` 一致）。
        let key = match &stored_key {
            Some(key) => Some(key.clone()),
            None => env(ctx, "GOOGLE_CLOUD_API_KEY", signal).await?,
        };
        if let Some(key) = key.filter(|value| !value.is_empty()) {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    ..Default::default()
                },
                env: None,
                source: Some(
                    if stored_key.is_some() {
                        "stored credential"
                    } else {
                        "GOOGLE_CLOUD_API_KEY"
                    }
                    .into(),
                ),
            }));
        }

        // ADC 文件：凭证里记录的路径优先，其次进程环境；未指定时检查默认路径。
        let adc_path = match stored_env.and_then(|env| env.get("GOOGLE_APPLICATION_CREDENTIALS")) {
            Some(path) => Some(path.clone()),
            None => env(ctx, "GOOGLE_APPLICATION_CREDENTIALS", signal).await?,
        };
        throw_if_aborted(signal)?;
        let has_credentials = ctx
            .file_exists(adc_path.as_deref().unwrap_or(VERTEX_ADC_PATH))
            .await;
        throw_if_aborted(signal)?;
        // project 允许 GCLOUD_PROJECT 作为次选；location 没有对应的旧变量。
        let project = match stored_env.and_then(|env| env.get("GOOGLE_CLOUD_PROJECT")) {
            Some(project) => Some(project.clone()),
            None => match env(ctx, "GOOGLE_CLOUD_PROJECT", signal).await? {
                Some(project) => Some(project),
                None => env(ctx, "GCLOUD_PROJECT", signal).await?,
            },
        };
        let location = match stored_env.and_then(|env| env.get("GOOGLE_CLOUD_LOCATION")) {
            Some(location) => Some(location.clone()),
            None => env(ctx, "GOOGLE_CLOUD_LOCATION", signal).await?,
        };
        // 三个条件缺一不可；不满足时返回 Ok(None)（未配置），而不是错误。
        if has_credentials && js_truthy(project.as_ref()) && js_truthy(location.as_ref()) {
            return Ok(Some(AuthResult {
                auth: ModelAuth::default(),
                env: credential.and_then(|credential| credential.env.clone()),
                source: Some(
                    if credential.is_some() {
                        "stored credential"
                    } else {
                        "gcloud application default credentials"
                    }
                    .into(),
                ),
            }));
        }
        Ok(None)
    }
}

/// TS: `providers/google-vertex.ts#googleVertexProvider`
///
/// 构造并返回 `google-vertex` provider：不设 provider 级 base URL，模型目录里的
/// `baseUrl` 是 `https://{location}-aiplatform.googleapis.com` 模板，由 `api/google_vertex`
/// 按 location 选主机；鉴权实现是私有的 `VertexAuth`。
pub fn google_vertex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google-vertex".into(),
        name: Some("Google Vertex AI".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(VertexAuth)),
            oauth: None,
        },
        models: catalog_chat_models("google-vertex"),
        api: Some(google_vertex_api().into()),
        ..Default::default()
    })
    .expect("google-vertex provider declares an api implementation")
}
