//! auth/helpers.ts。
//!
//! - [`env_api_key_auth`]：标准 api-key auth（存储的 key 优先，否则第一个有值的 env 变量）。
//! - [`lazy_oauth`]：把动态加载的 [`OAuthAuth`] 包装成 provider 可静态持有的 `Arc`；
//!   首次 `login`/`refresh`/`to_auth` 调用才加载，且**失败不重试**（TS 的 `promise ??=`）。
//!
//! # 为什么需要这两个工厂
//! provider 定义（`crate::providers::*`）只声明「支持哪种鉴权方式」，不应在模块初始化时
//! 依赖具体的 OAuth 流程实现：TS 用动态 `import()` 延迟加载，Rust 用 `load` 闭包 +
//! `OnceCell` 表达同一意图（是否真的延迟到运行时，由调用方提供的闭包决定）。
//! `env_api_key_auth` 则是轻量实现：构造后不再变化，只通过 `AuthContext` 读环境变量。
//!
//! # 与 TS 的对应
//! `EnvApiKeyAuth` 对应 `envApiKeyAuth(name, envVars)` 返回的匿名对象；`LazyOAuth` 对应
//! `lazyOAuth(input)` 的返回对象。两者都返回 `Arc<dyn ...>`，可直接放进 `ProviderAuth`
//! 并被 provider 注册表克隆共享。
//!
//! # 注意事项
//! - `EnvApiKeyAuth::login` 把用户输入**原样**存为 key（空串也接受），校验留给后续解析或
//!   请求阶段（见 spec/ai-auth.md §2.3）。
//! - `lazy_oauth` 的加载结果（含失败）只算一次：失败被缓存后同一实例不会重试；需要重试时
//!   重新构造一个 `LazyOAuthInput`。`OnceCell` 只缓存「实现加载」，不缓存登录/刷新结果。
//! - 两个实现都按 trait 约束实现为 `Send + Sync`，可放进 `Arc` 跨任务共享；`LazyOAuth`
//!   依赖 `tokio::sync::OnceCell` 保证并发首次调用只执行一次加载。
//!
//! TS: `src/auth/helpers.ts`

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::OnceCell;

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthError, AuthInteraction, AuthPrompt,
    AuthPromptRequest, AuthResult, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
    throw_if_aborted,
};

/// Standard api-key auth：a stored credential key wins, otherwise the first set env var resolves.
///
/// 无可变状态：`name` 是展示名（同时逐字拼进登录提示 `Enter <name>`），`env_vars` 是按优先级
/// 排列的环境变量名，两者构造后都不再改变。不实现 `check`，因此 `has_check()` 为 `false`，
/// `Models` 会回退到 `resolve()` 判断该 provider 是否可用。
struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

#[async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn has_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        // 两次检查与 TS 相同：进入登录前先看是否已取消；`prompt` 返回后再看一次——用户在
        // 提示期间取消时，已经输入的内容不能当作成功结果返回。
        throw_if_aborted(interaction.signal())?;
        // 逐字：`Enter ${name}`（无 placeholder）。
        let key = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                message: format!("Enter {}", self.name),
                placeholder: None,
            }))
            .await?;
        throw_if_aborted(interaction.signal())?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
            extra: Default::default(),
        })
    }

    /// 解析顺序：存储的非空 `key` → `env_vars` 中第一个非空值 → `Ok(None)`。
    ///
    /// `Ok(None)` 表示该 provider 当前未配置，不是错误；调用方（`Models`）据此跳过它。
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        throw_if_aborted(input.signal)?;
        // 阶段 1：存储的 key 优先级最高；`credential.env` 原样带出，供 provider 自己使用。
        // 空串视为未设置（TS 的真值判断），继续尝试环境变量。
        if let Some(key) = input
            .credential
            .and_then(|credential| credential.key.as_deref())
            && !key.is_empty()
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key.to_string()),
                    ..Default::default()
                },
                env: input
                    .credential
                    .and_then(|credential| credential.env.clone()),
                source: Some("stored credential".into()),
            }));
        }
        // 阶段 2：按 `env_vars` 顺序逐个询问 `AuthContext`；`ctx.env` 是 trait 方法，自定义
        // 实现可能耗时，所以每次 await 之后都重新检查取消。
        // 只排除空串、不做 trim：纯空白字符串仍算已配置（TS 的字符串真值语义）。
        for env_var in &self.env_vars {
            let value = input.ctx.env(env_var).await;
            throw_if_aborted(input.signal)?;
            if let Some(value) = value
                && !value.is_empty()
            {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(value),
                        ..Default::default()
                    },
                    env: None,
                    source: Some(env_var.clone()),
                }));
            }
        }
        // 阶段 3：全部未命中。
        Ok(None)
    }
}

/// Standard api-key auth：存储的 key 优先，否则第一个有值的 env 变量。
///
/// 返回的对象带 `login`（`secret` prompt `"Enter " + name`）与 `resolve`；
/// 非标准解析（provider env、ambient 文件、IAM）的 provider 自己实现 [`ApiKeyAuth`]。
///
/// `env_vars` 的顺序即优先级，第一个非空值胜出；空切片表示只认存储的 key。每次调用都新建
/// 一个 `Arc`，实例之间没有共享状态。`login` 只产出 key（`env` 为 `None`）：需要收集
/// provider 作用域 env 的 provider 要自己实现 `login`。
///
/// TS: `src/auth/helpers.ts#envApiKeyAuth`
pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> Arc<dyn ApiKeyAuth> {
    Arc::new(EnvApiKeyAuth {
        name: name.into(),
        env_vars: env_vars.iter().map(|value| (*value).to_string()).collect(),
    })
}

/// `lazyOAuth` 的加载结果。
///
/// 装箱是为了让 `LazyOAuthInput.load` 能以 `dyn Fn` 形式存放；`Send + 'static` 让返回的
/// future 能在 `OnceCell` 的初始化流程里跨 await 持有、在任意工作线程上轮询。
pub type LazyOAuthFuture =
    Pin<Box<dyn Future<Output = Result<Arc<dyn OAuthAuth>, AuthError>> + Send + 'static>>;

/// Wraps a dynamically imported [`OAuthAuth`]，provider 定义无需 import 实现。
///
/// 所有字段在首次加载前就固定下来；`load` 只被调用一次（成功或失败都一样）。
/// `is_subscription` 为 `None` 等价于 `false`，`login_label` 为 `None` 表示没有专属标签。
///
/// TS: `src/auth/helpers.ts#lazyOAuth` 的入参对象
pub struct LazyOAuthInput {
    /// 展示名。
    pub name: String,
    /// 是否由 provider 订阅支撑。
    pub is_subscription: Option<bool>,
    /// OAuth 登录选项的标签。
    pub login_label: Option<String>,
    /// 首次使用时加载（失败的结果会被 memoize，不会重试）。
    pub load: Arc<dyn Fn() -> LazyOAuthFuture + Send + Sync>,
}

/// `lazyOAuth` 的运行时形态：元数据 + 一次性的加载闭包 + 加载结果缓存。
///
/// 缓存类型是 `Result`，因此失败也会被记住；`Debug` 对 `loaded` 只输出 `initialized()`
/// 布尔值（其余元数据照常打印），不会触发加载。
struct LazyOAuth {
    name: String,
    is_subscription: bool,
    login_label: Option<String>,
    load: Arc<dyn Fn() -> LazyOAuthFuture + Send + Sync>,
    /// TS 的 `let promise; promise ??= input.load();`：成功/失败都只加载一次。
    loaded: OnceCell<Result<Arc<dyn OAuthAuth>, AuthError>>,
}

impl fmt::Debug for LazyOAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyOAuth")
            .field("name", &self.name)
            .field("is_subscription", &self.is_subscription)
            .field("login_label", &self.login_label)
            .field("loaded", &self.loaded.initialized())
            .finish()
    }
}

impl LazyOAuth {
    async fn loaded(&self) -> Result<Arc<dyn OAuthAuth>, AuthError> {
        // `get_or_init` 保证并发首次调用只执行一次 `load`，其余调用等待同一次初始化。
        // 缓存的是 `Result`，成功和失败都只发生一次（TS 的 `promise ??= input.load()`）；
        // `AuthError` 是 `Clone`，失败时给每个调用方一个副本。
        match self.loaded.get_or_init(|| (self.load)()).await {
            Ok(auth) => Ok(Arc::clone(auth)),
            Err(error) => Err(error.clone()),
        }
    }
}

#[async_trait]
impl OAuthAuth for LazyOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_subscription(&self) -> bool {
        self.is_subscription
    }

    fn login_label(&self) -> Option<&str> {
        self.login_label.as_deref()
    }

    // 三个方法都先 `loaded()`；OnceCell 只缓存「实现」的加载结果，登录/刷新/推导的结果
    // 不进入缓存，每次调用仍走真实实现。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.loaded().await?.login(interaction, options).await
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &crate::utils::abort::AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        self.loaded().await?.refresh(credential, signal).await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        self.loaded().await?.to_auth(credential).await
    }
}

/// Wraps a dynamically imported [`OAuthAuth`] so provider definitions can advertise OAuth
/// without importing the implementation.
///
/// 流程在首次 `login`/`refresh`/`to_auth` 调用时才加载（并发首次调用共享同一次加载）。
///
/// 返回值是独立实例：每次调用都有自己的 `OnceCell`，互不共享加载结果。`input.load` 应返回
/// `Err` 而不是 panic——只有 `Err` 会被缓存成可重复返回的失败结果。
///
/// TS: `src/auth/helpers.ts#lazyOAuth`
pub fn lazy_oauth(input: LazyOAuthInput) -> Arc<dyn OAuthAuth> {
    Arc::new(LazyOAuth {
        name: input.name,
        is_subscription: input.is_subscription.unwrap_or(false),
        login_label: input.login_label,
        load: input.load,
        loaded: OnceCell::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::auth::types::AuthContext;
    use crate::utils::abort::AbortScope;
    use indexmap::IndexMap;

    struct TestInteraction {
        signal: AbortScope,
        response: String,
        prompts: std::sync::Mutex<Vec<AuthPromptRequest>>,
    }

    impl TestInteraction {
        fn new(response: &str) -> Self {
            TestInteraction {
                signal: AbortScope::new(),
                response: response.to_string(),
                prompts: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AuthInteraction for TestInteraction {
        fn signal(&self) -> &AbortScope {
            &self.signal
        }

        async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
            self.prompts.lock().unwrap().push(request);
            Ok(self.response.clone())
        }

        fn notify(&self, _event: &crate::auth::types::AuthEvent) {}
    }

    struct TestContext {
        env: IndexMap<String, String>,
    }

    #[async_trait]
    impl AuthContext for TestContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    fn context(values: &[(&str, &str)]) -> TestContext {
        TestContext {
            env: values
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    #[tokio::test]
    async fn env_api_key_login_prompts_for_the_key() {
        let auth = env_api_key_auth("Anthropic API key", &["ANTHROPIC_API_KEY"]);
        assert!(auth.has_login());
        assert!(!auth.has_check());
        assert_eq!(auth.name(), "Anthropic API key");

        let interaction = TestInteraction::new("sk-entered");
        let credential = auth.login(&interaction).await.expect("login");
        assert_eq!(credential.key.as_deref(), Some("sk-entered"));
        assert!(credential.env.is_none());

        let prompts = interaction.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompts[0].prompt,
            AuthPrompt::Secret {
                message: "Enter Anthropic API key".into(),
                placeholder: None,
            }
        );
    }

    #[tokio::test]
    async fn env_api_key_login_rejects_an_aborted_flow_before_prompting() {
        let auth = env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"]);
        let interaction = TestInteraction::new("unused");
        interaction.signal.abort();
        let error = auth.login(&interaction).await.unwrap_err();
        assert_eq!(error.name, "AbortError");
        assert!(interaction.prompts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn env_api_key_resolve_prefers_stored_credential_then_env_vars() {
        let auth = env_api_key_auth("OpenAI API key", &["FIRST", "SECOND"]);
        let ctx = context(&[("SECOND", "from-env")]);
        let signal = AbortScope::new();

        let stored = ApiKeyCredential {
            key: Some("stored".into()),
            env: Some(ProviderEnv::from([("EXTRA".to_string(), "x".to_string())])),
            extra: Default::default(),
        };
        let result = auth
            .resolve(ApiKeyAuthInput {
                ctx: &ctx,
                credential: Some(&stored),
                signal: &signal,
            })
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
        assert_eq!(
            result.env,
            Some(ProviderEnv::from([("EXTRA".to_string(), "x".to_string())]))
        );

        let result = auth
            .resolve(ApiKeyAuthInput {
                ctx: &ctx,
                credential: None,
                signal: &signal,
            })
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("from-env"));
        assert_eq!(result.source.as_deref(), Some("SECOND"));
        assert!(result.env.is_none());

        let empty = context(&[]);
        assert!(
            auth.resolve(ApiKeyAuthInput {
                ctx: &empty,
                credential: None,
                signal: &signal,
            })
            .await
            .expect("resolve")
            .is_none()
        );
    }

    #[tokio::test]
    async fn env_api_key_resolve_ignores_empty_stored_keys() {
        let auth = env_api_key_auth("OpenAI API key", &["OPENAI_API_KEY"]);
        let ctx = context(&[("OPENAI_API_KEY", "env-key")]);
        let signal = AbortScope::new();
        let stored = ApiKeyCredential {
            key: Some(String::new()),
            env: None,
            extra: Default::default(),
        };
        let result = auth
            .resolve(ApiKeyAuthInput {
                ctx: &ctx,
                credential: Some(&stored),
                signal: &signal,
            })
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("env-key"));
    }

    struct FakeOAuth {
        name: String,
    }

    #[async_trait]
    impl OAuthAuth for FakeOAuth {
        fn name(&self) -> &str {
            &self.name
        }

        async fn login(
            &self,
            _interaction: &dyn AuthInteraction,
            _options: &LoginOptions,
        ) -> Result<OAuthCredential, AuthError> {
            Ok(OAuthCredential::new("access", "refresh", 1.0))
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            _signal: &AbortScope,
        ) -> Result<OAuthCredential, AuthError> {
            Ok(OAuthCredential::new(
                format!("{}2", credential.access),
                credential.refresh.clone(),
                credential.expires,
            ))
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn lazy_oauth_loads_once_and_exposes_the_input_metadata() {
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&loads);
        let auth = lazy_oauth(LazyOAuthInput {
            name: "Test OAuth".into(),
            is_subscription: Some(true),
            login_label: Some("Sign in with Test".into()),
            load: Arc::new(move || {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(Arc::new(FakeOAuth {
                        name: "Test OAuth".into(),
                    }) as Arc<dyn OAuthAuth>)
                })
            }),
        });
        assert_eq!(auth.name(), "Test OAuth");
        assert!(auth.is_subscription());
        assert_eq!(auth.login_label(), Some("Sign in with Test"));
        assert_eq!(loads.load(Ordering::SeqCst), 0);

        let credential = OAuthCredential::new("a", "r", 1.0);
        let refreshed = auth
            .refresh(&credential, &AbortScope::new())
            .await
            .expect("refresh");
        assert_eq!(refreshed.access, "a2");
        let derived = auth.to_auth(&credential).await.expect("to_auth");
        assert_eq!(derived.api_key.as_deref(), Some("a"));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn lazy_oauth_memoizes_failures_without_retrying() {
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&loads);
        let auth = lazy_oauth(LazyOAuthInput {
            name: "Broken OAuth".into(),
            is_subscription: None,
            login_label: None,
            load: Arc::new(move || {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err(AuthError::new("load failed"))
                })
            }),
        });
        assert!(!auth.is_subscription());
        let first = auth
            .to_auth(&OAuthCredential::new("a", "r", 1.0))
            .await
            .unwrap_err();
        let second = auth
            .to_auth(&OAuthCredential::new("a", "r", 1.0))
            .await
            .unwrap_err();
        assert_eq!(first.message, "load failed");
        assert_eq!(second.message, "load failed");
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    use crate::types::ProviderEnv;
}
