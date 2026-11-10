//! auth/resolve.ts。
//!
//! `Models` 集合内所有操作共享的 auth 解析：
//!
//! > A stored credential owns the provider: ambient/env is consulted only when nothing is stored.
//! > No silent env fallback after a failed refresh or for a credential type without a matching handler.
//!
//! 两个关键语义（spec/ai-auth.md §2.5）：
//! 1. [`resolve_provider_auth`] 的内部解析与调用方 signal **竞速**：调用方立即拿到 `AbortError`，
//!    内部继续跑完（刷新会持久化轮换后的 token）。Rust 用 `tokio::spawn` 实现。
//! 2. [`refresh_stored_oauth_credential`] 只在**等锁**期间响应取消；一旦进入 `modify` 回调，
//!    刷新与其持久化忽略调用方 signal，仅受 15s 超时约束（`AbortSignal.timeout`）。
//!
//! # 为什么需要这一层
//! “凭证存在哪里”（`auth.json`、env 变量、AWS profile、ADC 文件）与“provider 用什么方式
//! 鉴权”（api key / OAuth）是两个正交维度。本模块把它们的组合收敛成一个入口：先按固定
//! 优先级选出唯一一种来源，再委托给 provider 自己的 [`ApiKeyAuth`] / [`OAuthAuth`] 实现
//! （`auth/helpers.rs` 与各 provider 的 `oauth/*`）。调用方（`Models::get_auth`、
//! `Models::check_auth`、`models` 的刷新路径、`utils::identity`、CLI 的 bearer-token 导出）
//! 因此不需要知道凭证来自哪里。
//!
//! # 解析优先级（顺序即语义）
//! 1. `overrides.api_key` 为 `Some` 且 provider 有 `api_key` handler → 用该字符串走 api-key
//!    解析，**完全不读存储**（`Some("")` 也算“提供”）。
//! 2. 否则读存储（[`CredentialStore::read`]）：
//!    - 存的是 OAuth 且 provider 有 `oauth` handler → 剩余有效期不足时加锁刷新，再 `to_auth`；
//!    - 存的是 api key 且 provider 有 `api_key` handler → 存储的 key + provider env 参与解析；
//!    - 类型与 handler 对不上 → `Ok(None)`，**不**回退 ambient。
//! 3. 什么都没存 → 仅当 provider 有 `api_key` handler 时才走 ambient（env 变量、AWS
//!    profiles、ADC 文件）；纯 OAuth provider 返回 `Ok(None)`。
//!
//! `Ok(None)` 的意思是“该 provider 当前没有可用鉴权”，不是错误，调用方应据此跳过它。
//!
//! # 执行流程
//! [`resolve_provider_auth`] 把实际工作 `tokio::spawn` 成独立任务后与调用方 signal 竞速；
//! 真正的判定在私有的 `resolve_provider_auth_with_signal` 里；OAuth 需要刷新时进入
//! [`refresh_stored_oauth_credential`] 的双重检查锁。
//!
//! # 失败模式与错误映射
//! - 存储读失败 → `ModelsError`，code `"auth"`，消息前缀 `Credential store read failed for`。
//! - 存储写/加锁失败 → code `"auth"`，前缀 `Credential store modify failed for`。
//! - [`ApiKeyAuth::resolve`] 失败 → code `"auth"`，前缀 `API key auth failed for provider`。
//! - OAuth 刷新失败 → code `"oauth"`，前缀 `OAuth refresh failed for`；派生请求头失败 →
//!   code `"oauth"`，前缀 `OAuth auth derivation failed for`。
//! - 调用方中止 → `AbortError`（`name` 字段，不是 `ModelsError`），并且优先于同时发生的
//!   其它错误。
//!
//! `ModelsError` 的 message 已由 `withCauseDetail` 拼好 cause 细节，调用方不可再二次拼接。
//!
//! # 并发与取消
//! 内部任务在调用方 abort 后仍会跑完，因此“取消了就什么都没发生”并不成立：已经开始的
//! OAuth 刷新仍会把轮换后的 token 写回存储。这是刻意的——provider 在刷新时可能已让旧
//! refresh token 失效，丢弃刷新结果会让本地凭证永久不可用。
//!
//! TS: `src/auth/resolve.ts`

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthContext, AuthError, AuthOperationOptions,
    AuthResult, Credential, CredentialModifyFn, CredentialStore, OAuthAuth, OAuthCredential,
    ProviderAuth, abort_error, throw_if_aborted,
};
use crate::types::ProviderEnv;
use crate::utils::abort::{
    AbortListenerFn, AbortReason, AbortScope, operation_signal, race_with_abort_signal,
};

/// TS 的 `resolve.ts` 重导出 `ModelsError` 与 `ModelsErrorCode`
/// （`simplify`：`pub use` 同时把名字带进本模块作用域）。
///
/// 这两个名字在 crate 根也有再导出，因此 `kiana_ai::ModelsError` 与
/// `kiana_ai::auth::resolve::ModelsError` 指向同一类型，调用方可按 TS 的入口习惯任选其一。
pub use crate::error::{ModelsError, ModelsErrorCode};

/// 解析覆盖项。
///
/// 所有字段都是 `Option`，因此“未提供”与“提供了空值”是两种不同语义（尤其 `api_key`：
/// `Some("")` 仍会走显式覆盖分支）。`Default` 表示全部不覆盖。
///
/// 覆盖只影响**单次**解析：env 覆盖层与凭证 env 的合并都是临时值，不写回存储。
///
/// TS: `src/auth/resolve.ts#AuthResolutionOverrides`
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    /// 显式 api key（`!== undefined` 即算提供，空串也算）。
    ///
    /// 只在 provider 有 [`ApiKeyAuth`] handler 时生效；纯 OAuth provider 会忽略它（不报错，
    /// 继续走 OAuth 解析）。
    pub api_key: Option<String>,
    /// provider 作用域的环境覆盖。
    ///
    /// 同时用于两处：作为 [`AuthContext::env`] 的上层（空串按未设置处理，见
    /// `OverlayEnvAuthContext`），以及合并进已存储 api-key 凭证的 `env`（同 key 取覆盖值）。
    pub env: Option<ProviderEnv>,
    /// 要求 OAuth token 至少还剩这么多有效期；默认五分钟。
    ///
    /// 只提高下限（与默认值取 `max`），负值等于不提高。只有显式给出时才会在刷新后复查：
    /// 刷新结果仍不足则报 code `"oauth"` 的 `ModelsError`（bearer-token 导出依赖这一点）。
    pub min_oauth_validity_ms: Option<i64>,
    /// 取消信号。
    ///
    /// `None` 时内部使用一个永不中止的 scope。它决定调用方何时拿到 `AbortError`，但不会
    /// 打断已经开始的 OAuth 刷新（见 [`resolve_provider_auth`]）。
    pub signal: Option<AbortScope>,
}

/// `DEFAULT_OAUTH_MINIMUM_VALIDITY_MS = 5 * 60 * 1000`。
///
/// 刷新触发窗口：剩余有效期不超过 5 分钟（恰好 5 分钟也会刷新，因为比较是
/// `now + 5min >= expires`）的 OAuth 凭证会被刷新。注意它是**触发**阈值，
/// 不是使用前的有效期保证；只有调用方显式给出 `min_oauth_validity_ms` 时才在刷新后强校验。
const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;

/// `DEFAULT_OAUTH_REFRESH_TIMEOUT_MS = 15_000`。
///
/// 超时 scope 只传给 `oauth.refresh`，不参与存储的 `modify`：拿到锁之后的持久化不受
/// 这个 15s 限制。
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

/// `Date.now()`：墙钟毫秒（f64，与 JS number 一致）。
///
/// 用浮点是因为 [`OAuthCredential`] 的 `expires` 字段是 JS number（可以是小数，甚至 `NaN`），
/// 过期比较必须按浮点规则做。`duration_since` 失败（系统时钟早于 Unix 纪元）时返回 `0.0`，
/// 调用点因此无需处理错误。
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `overlayEnvAuthContext(base, env)`：override 中的真值优先，否则回落 base。
///
/// `||` 语义：空串也回落。
struct OverlayEnvAuthContext {
    base: Arc<dyn AuthContext>,
    env: ProviderEnv,
}

#[async_trait]
impl AuthContext for OverlayEnvAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        // JS 的 `env[name] || base.env(name)`：覆盖值非空才采用；空串与未设置等价，都会回落。
        if let Some(value) = self.env.get(name)
            && !value.is_empty()
        {
            return Some(value.clone());
        }
        self.base.env(name).await
    }

    // 文件存在性不参与 env 覆盖，始终由 base 决定。
    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

/// Auth resolution shared by all operations in a `Models` collection。
///
/// 存有凭证时由它接管 provider；只有什么都没存时才查 ambient/env。
/// 刷新失败或凭证类型无匹配 handler 时**不**静默回落到 env。
///
/// # 参数
/// - `provider_id`：`Provider.id`，同时是存储键与错误消息里的名字。
/// - `auth`：provider 声明的鉴权 handler；`api_key` / `oauth` 至少有一个。
/// - `credentials`：凭证存储；读走 `read`，OAuth 刷新走 `modify`（唯一写路径）。
/// - `auth_context`：ambient 环境访问（env 变量、文件是否存在）。
/// - `overrides`：单次覆盖项，见 [`AuthResolutionOverrides`]。
///
/// # 返回值
/// `Ok(None)` 表示此刻解析不出鉴权（未登录/未配置），调用方应按“跳过该 provider”处理，
/// 不要当成错误。成功值的 `source` 是给状态 UI 的标签（如 `"OAuth"`、env 变量名、
/// `"stored credential"`），由各 handler 与 OAuth 分支自行填写。
///
/// # 错误
/// - `AbortError`：调用方 signal 已中止，优先于内部结果与内部错误。
/// - `ModelsError`：存储、api-key 或 OAuth 解析失败，code 见模块文档。
///
/// # 取消与生命周期
/// 内部解析被 `tokio::spawn` 成独立任务再与 signal 竞速，因此调用方拿到 `AbortError`
/// 后内部仍会跑完（可能继续写存储）。`provider_id`、`auth` 与各个 `Arc` 被 move 进闭包，
/// 是为了满足 `tokio::spawn` 对 `Send + 'static` 的要求。`signal` 另 clone 一份
/// （`inner_signal`）给内部任务使用，原 `signal` 留给 `race_with_abort_signal` 竞速。
///
/// TS: `src/auth/resolve.ts#resolveProviderAuth`
pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
    overrides: AuthResolutionOverrides,
) -> Result<Option<AuthResult>, AuthError> {
    // `operation_signal`：`None` 时新建一个永不中止的 scope，后面的 `is_aborted` /
    // `throw_if_aborted` 因此都不需要判空。
    let signal = operation_signal(overrides.signal.clone());
    let provider_id = provider_id.to_string();
    let auth = auth.clone();
    let inner_signal = signal.clone();
    // 内部任务必须 `'static`：name/auth/Arc 全部 move 进来；`signal` clone 一份给内部任务
    // （`inner_signal`），原 `signal` 用于 `race_with_abort_signal` 竞速。
    let operation = async move {
        resolve_provider_auth_with_signal(
            provider_id,
            auth,
            credentials,
            auth_context,
            overrides,
            inner_signal,
        )
        .await
    };
    match race_with_abort_signal(operation, &signal).await {
        // TS 的 abort 事件同步派发：若中止发生在解析期间（即使解析随后正常完成或报错），
        // 调用方拿到的是 `AbortError`。`tokio::select!` 同时就绪时是随机的，这里用中止
        // 状态补回确定语义。内部解析的错误包在 `Ok(Err(...))` 里，由第一个分支覆盖；
        // `Err` 分支只可能是中止结果或 JoinError（任务被取消），这里同样统一。
        // 代价是跨线程下比 TS 更保守：内部任务已返回、但中止在结果被检查前置位时，
        // 调用方仍会看到 `AbortError`。
        Ok(_) if signal.is_aborted() => Err(abort_error(&signal)),
        Ok(result) => result,
        Err(_) if signal.is_aborted() => Err(abort_error(&signal)),
        Err(error) => Err(AuthError::from_ai_error(&error)),
    }
}

/// `resolveProviderAuthWithSignal`：解析主体，按“显式 override > 存储凭证 > ambient”的
/// 优先级选出唯一路径（完整顺序见模块文档与 spec/ai-auth.md §2.5）。
///
/// 存储凭证类型与 handler 匹配时，本函数不进入 ambient-only 回退路径（类型不匹配则
/// `Ok(None)`）；但 `ApiKeyAuth` handler 自身在存储 key 为空或缺失时仍会读取 ambient
/// （`helpers.rs` 的 `env_api_key_resolve_ignores_empty_stored_keys` 锁定该行为，
/// `types.rs` 模块文档也写明“有 api_key 凭证：key 优先，否则查 ambient”）。
/// `Ok(None)` 统一表示“未配置”。
async fn resolve_provider_auth_with_signal(
    provider_id: String,
    auth: ProviderAuth,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
    overrides: AuthResolutionOverrides,
    signal: AbortScope,
) -> Result<Option<AuthResult>, AuthError> {
    // 已中止就不必读存储或发起刷新；竞速分支也会返回 AbortError，但那样内部任务要白跑一遍。
    throw_if_aborted(&signal)?;
    // env 覆盖只在本次请求内生效：包一层覆盖 base，不改动调用方的 context。
    let request_auth_context: Arc<dyn AuthContext> = match overrides.env.clone() {
        Some(env) => Arc::new(OverlayEnvAuthContext {
            base: auth_context,
            env,
        }),
        None => auth_context,
    };

    // 显式 apiKey 优先且**跳过存储读取**；`env` 覆盖同时作为该凭证的 provider env 传下去。
    if let (Some(api_key), Some(api_key_auth)) = (overrides.api_key.as_ref(), auth.api_key.as_ref())
    {
        return resolve_api_key(
            request_auth_context.as_ref(),
            api_key_auth,
            &provider_id,
            Some(&ApiKeyCredential {
                key: Some(api_key.clone()),
                env: overrides.env.clone(),
                extra: Default::default(),
            }),
            &signal,
        )
        .await;
    }

    // 只有“显式 apiKey 且 provider 有 api_key handler”才跳过存储读取；否则仍先读存储，
    // 包括纯 OAuth provider 收到显式 override 的情况（该 override 会被忽略，不报错）。
    // 读失败由 `read_credential` 包装成 code `"auth"`。
    let stored = read_credential(credentials.as_ref(), &provider_id, &signal).await?;
    if let Some(stored) = stored {
        // 按“存储类型 × provider handler”配对，只有配对成功才解析。
        match (&stored, auth.oauth.as_ref(), auth.api_key.as_ref()) {
            (Credential::OAuth(credential), Some(oauth), _) => {
                resolve_stored_oauth(
                    credentials,
                    &provider_id,
                    Arc::clone(oauth),
                    credential.clone(),
                    &signal,
                    overrides.min_oauth_validity_ms,
                )
                .await
            }
            (Credential::ApiKey(credential), _, Some(api_key_auth)) => {
                // `{ ...stored, env: { ...stored.env, ...overrides.env } }`：以存储 env 为底、
                // 覆盖项后写，因此同 key 取覆盖值；key 与未知字段保持不变。
                let credential = match overrides.env.as_ref() {
                    Some(env) => {
                        let mut merged = credential.env.clone().unwrap_or_default();
                        merged.extend(
                            env.iter()
                                .map(|(name, value)| (name.clone(), value.clone())),
                        );
                        ApiKeyCredential {
                            key: credential.key.clone(),
                            env: Some(merged),
                            extra: credential.extra.clone(),
                        }
                    }
                    None => credential.clone(),
                };
                resolve_api_key(
                    request_auth_context.as_ref(),
                    api_key_auth,
                    &provider_id,
                    Some(&credential),
                    &signal,
                )
                .await
            }
            // 凭证类型无匹配 handler：绝不回退 env。
            _ => Ok(None),
        }
    } else {
        // Ambient（env vars、AWS profiles、ADC files）。
        // 纯 OAuth provider 没有 api_key handler 可消费 ambient，因此在这里得到 None。
        match auth.api_key.as_ref() {
            Some(api_key_auth) => {
                resolve_api_key(
                    request_auth_context.as_ref(),
                    api_key_auth,
                    &provider_id,
                    None,
                    &signal,
                )
                .await
            }
            None => Ok(None),
        }
    }
}

/// OAuth resolution with double-checked locking：剩余不足五分钟的 token 通过
/// [`refresh_stored_oauth_credential`] 刷新。
///
/// 两层检查：
/// 1. 锁外的乐观检查（`expires_soon`）：未临近过期的请求直接跳过加锁（快路径）；
/// 2. 锁内的权威检查：同一个 `expires_soon` 作为 `needs_refresh` 传进刷新函数，只有锁内
///    看到仍然过期才真正调用 provider。并发请求因此最多刷新一次。
///
/// `minimum_validity_ms` 取默认 5 分钟与调用方要求中的较大者；比较用 `>=`，恰好等于
/// “now + 下限”也算过期。显式给出 `min_oauth_validity_ms` 时刷新后还要再查一次：
/// provider 并不保证刷新结果的有效期，而 bearer-token 导出等调用方需要这个强约束。
///
/// 返回 `Ok(None)` 只在刷新结束时存储里已不是 OAuth（期间 logout 或被改成 api key）；
/// 否则把（可能是刷新后的）凭证交给 [`OAuthAuth::to_auth`] 派生请求头。
async fn resolve_stored_oauth(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn OAuthAuth>,
    stored: OAuthCredential,
    signal: &AbortScope,
    min_oauth_validity_ms: Option<i64>,
) -> Result<Option<AuthResult>, AuthError> {
    // `Math.max(默认 5 分钟, 调用方要求 ?? 0)`：调用方只能把窗口调大，不能调小。
    let minimum_validity_ms =
        DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0));
    // 同一个闭包既做锁外乐观检查、又作为锁内 `needs_refresh`：两处判定必须一致。
    let expires_soon: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync> =
        Arc::new(move |credential: &OAuthCredential| {
            now_ms() + minimum_validity_ms as f64 >= credential.expires
        });
    let mut credential = stored;

    if expires_soon(&credential) {
        // 乐观检查说已过期；权威检查在锁内跑。
        let post = refresh_stored_oauth_credential(
            Arc::clone(&credentials),
            provider_id,
            Arc::clone(&oauth),
            Arc::clone(&expires_soon),
            signal,
        )
        .await?;
        let Some(post) = post else {
            return Ok(None); // logged out meanwhile
        };
        credential = post;
        // 常规五分钟窗口只触发刷新，不构成 provider 契约；显式调用方（如 bearer-token 导出）
        // 要求刷新后仍满足所请求的下限。
        if min_oauth_validity_ms.is_some() && expires_soon(&credential) {
            return Err(AuthError::models(
                ModelsErrorCode::OAuth,
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
                None,
            ));
        }
    }

    // `to_auth` 是纯推导（不联网）；失败说明凭证形状不被 provider 接受。
    // OAuth 分支不产出 provider env（`env: None`），`source` 恒为字面量 `"OAuth"`。
    match oauth.to_auth(&credential).await {
        Ok(auth) => Ok(Some(AuthResult {
            auth,
            env: None,
            source: Some("OAuth".into()),
        })),
        Err(error) => Err(AuthError::models(
            ModelsErrorCode::OAuth,
            format!("OAuth auth derivation failed for {provider_id}"),
            Some(&error.as_thrown_value()),
        )),
    }
}

/// `resolveApiKey`：把解析委托给 provider 的 [`ApiKeyAuth`] 实现。
///
/// `credential` 为 `None` 表示只从 ambient 解析；为 `Some` 时 handler 通常优先使用其中的
/// key 与 env。handler 的 `Ok(None)` 表示该 provider 未配置，不算失败；`Err` 统一包装成
/// code `"auth"` 的 `ModelsError`（带 provider id 与 cause）。
async fn resolve_api_key(
    auth_context: &dyn AuthContext,
    api_key: &Arc<dyn ApiKeyAuth>,
    provider_id: &str,
    credential: Option<&ApiKeyCredential>,
    signal: &AbortScope,
) -> Result<Option<AuthResult>, AuthError> {
    match api_key
        .resolve(ApiKeyAuthInput {
            ctx: auth_context,
            credential,
            signal,
        })
        .await
    {
        Ok(result) => Ok(result),
        Err(error) => Err(AuthError::models(
            ModelsErrorCode::Auth,
            format!("API key auth failed for provider {provider_id}"),
            Some(&error.as_thrown_value()),
        )),
    }
}

/// `readCredential`：读存储，把任何失败包装成 code `"auth"` 的 `ModelsError`。
///
/// 条目不存在时存储返回 `Ok(None)`（不是错误）；`signal` 原样透传，实现可以在读取前检查中止。
async fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    signal: &AbortScope,
) -> Result<Option<Credential>, AuthError> {
    match credentials
        .read(
            provider_id,
            AuthOperationOptions {
                signal: Some(signal.clone()),
            },
        )
        .await
    {
        Ok(credential) => Ok(credential),
        Err(error) => Err(AuthError::models(
            ModelsErrorCode::Auth,
            format!("Credential store read failed for {provider_id}"),
            Some(&error.as_thrown_value()),
        )),
    }
}

/// Refresh a stored OAuth credential under the credential-store lock and persist the result
/// before the lock is released。
///
/// `needs_refresh` 在锁内二次检查，因此并发调用方与进程只刷新一次。
///
/// `signal` 只取消**等锁**。刷新一旦开始，provider 可能已经轮换了 refresh token，
/// 因此刷新与其持久化忽略 `signal`，只受超时约束；否则被取消的调用方会丢弃唯一有效的
/// refresh token。需要立即返回的调用方应与自己的 signal 竞速本 promise。
///
/// # 参数
/// - `credentials`：凭证存储；`modify` 按 provider 串行化，跨进程互斥取决于实现。
/// - `oauth`：provider 的 OAuth handler，`refresh` 负责兑换新 token（网络调用）。
/// - `needs_refresh`：锁内二次检查，返回 `true` 才真正刷新；返回 `false` 说明别的请求或
///   进程已经刷新过。
/// - `signal`：只用于等锁与进入回调前的检查，语义见上。
///
/// # 返回
/// `Ok(Some(credential))` 是 `modify` 返回的写后 OAuth 凭证，**不一定**是本次新刷出来的
/// （锁内检查发现无需刷新时，返回的可能是别人刚写好的新凭证）。`Ok(None)` 表示期间被
/// logout 或凭证被替换成了 api key。
///
/// # 失败与状态
/// - 刷新失败 → 回调返回 `Err`，按 [`CredentialStore::modify`] 的契约不写入，存储保持原样，
///   下一次解析会再次尝试刷新。
/// - 等锁阶段中止 → `AbortError`；存储自身的失败 → code `"auth"` 的 `ModelsError`。
/// - 15s 超时是协作式的：`oauth.refresh` 必须观察传入的 signal 才会提前返回；超时以
///   `TimeoutError` 中止，再按刷新失败包装成 `ModelsError`。
///
/// TS: `src/auth/resolve.ts#refreshStoredOAuthCredential`
pub async fn refresh_stored_oauth_credential(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn OAuthAuth>,
    needs_refresh: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: &AbortScope,
) -> Result<Option<OAuthCredential>, AuthError> {
    // 只用于“等锁”的派生 scope：中止它只影响 `modify` 的排队，不影响已经开始的回调。
    let lock_wait = AbortScope::new();
    let signal_for_lock = signal.clone();
    let lock_wait_for_listener = lock_wait.clone();
    // 调用方中止 → 用同一个 reason（缺省 AbortError）中止锁等待，与 DOM 的 abort 传播一致。
    let cancel_lock_wait: AbortListenerFn = Arc::new(move || {
        let reason = signal_for_lock
            .reason()
            .unwrap_or_else(AbortReason::default_abort_error);
        lock_wait_for_listener.abort_with(reason);
    });
    let registration = signal.add_abort_listener(Arc::clone(&cancel_lock_wait));
    // 监听注册后再补一次检查：已经中止的 scope 不会再派发事件，必须主动补发。
    if signal.is_aborted() {
        cancel_lock_wait();
    }

    // 闭包必须是 `'static` 且 `FnOnce`：provider id、oauth、判定函数与 signal 都 clone 进去。
    let provider = provider_id.to_string();
    let outer_signal = signal.clone();
    let registration_in_fn = registration.clone();
    let modify: CredentialModifyFn = Box::new(move |current| {
        let provider = provider.clone();
        let oauth = Arc::clone(&oauth);
        let needs_refresh = Arc::clone(&needs_refresh);
        let outer_signal = outer_signal.clone();
        let registration = registration_in_fn.clone();
        Box::pin(async move {
            // 进入刷新后移除监听：调用方的 abort 不再影响刷新与持久化。
            registration.remove();
            // 监听虽已移除，这个检查仍必要：中止可能发生在拿到锁之前、回调尚未开始的窗口里。
            // 错误经 `modify` 传出后，由外层按 signal 状态还原成 `AbortError`。
            throw_if_aborted(&outer_signal)?;
            let Some(Credential::OAuth(current)) = current else {
                return Ok(None); // logged out meanwhile
            };
            // 锁内二次检查：并发调用方在这里收敛，只有第一个看到“仍过期”的会真正刷新。
            if !needs_refresh(&current) {
                return Ok(None); // another process/request refreshed
            }
            // 独立超时 scope（`AbortSignal.timeout`）：只交给 `oauth.refresh`；存储锁不参与
            // 这个超时，因此持久化不会被 15s 打断。
            let timeout = AbortScope::new();
            let timeout_for_timer = timeout.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS)).await;
                timeout_for_timer.abort_with(AbortReason {
                    name: "TimeoutError".into(),
                    message: "The operation was aborted due to timeout".into(),
                });
            });
            let result = oauth.refresh(&current, &timeout).await;
            // 刷新已返回（成功或失败）：停掉计时任务，避免它稍后再中止一个不再使用的 scope。
            timer.abort();
            match result {
                Ok(refreshed) => Ok(Some(Credential::OAuth(refreshed))),
                // 刷新失败按 code `"oauth"` 包装；cause 的文本由 `AuthError::models` 拼进 message。
                Err(error) => Err(AuthError::models(
                    ModelsErrorCode::OAuth,
                    format!("OAuth refresh failed for {provider}"),
                    Some(&error.as_thrown_value()),
                )),
            }
        })
    });

    let result = credentials
        .modify(
            provider_id,
            modify,
            AuthOperationOptions {
                signal: Some(lock_wait),
            },
        )
        .await;
    // `finally` 语义：成功、失败、提前返回都注销监听；`remove` 可重复调用。
    registration.remove();
    let post = match result {
        Ok(post) => post,
        Err(error) => {
            // 回调抛出的刷新错误与存储实现自带的 ModelsError 都原样上抛，不再二次包装。
            if error.is_models_error() {
                return Err(error);
            }
            // 等锁阶段被中止时，存储返回的不是 ModelsError；这一步保证“中止优先于包装错误”。
            throw_if_aborted(signal)?;
            // 其余存储失败（含加锁/写入错误）统一 code `"auth"`。
            return Err(AuthError::models(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}"),
                Some(&error.as_thrown_value()),
            ));
        }
    };
    // 只有 OAuth 结果可用；api_key 表示期间被替换（如已 logout 后登录了别的类型）。
    Ok(post.and_then(|credential| match credential {
        Credential::OAuth(credential) => Some(credential),
        Credential::ApiKey(_) => None,
    }))
}

// 本模块是 resolve.rs 语义的可执行规格：每个测试对应 spec/ai-auth.md §2.5 的一条不变量。
// 依赖三个手工 fake：TestContext（内存 env）、FixedApiKey（可计数的 api-key handler）、
// FakeOAuth（可脚本化 refresh 结果，并可注入 refresh 执行期间的回调）。
// 测试分四组：
// - 优先级：存储 > ambient；类型无匹配 handler 不回退；显式 apiKey（含空串）跳过存储。
// - OAuth 刷新：窗口内不刷新、过期后在锁内刷新并持久化、并发只刷新一次、刷新后强校验。
// - 取消：不再等待非协作回调（顶层竞速放弃）、进入刷新后不可取消且结果仍持久化、
//   取消优先于并发错误。credential-store 等锁阶段的排队取消不在本模块覆盖，见
//   `credential_store.rs` 与 `tests/models_runtime.rs` 的
//   `cancels_queued_credential_mutations_without_running_them_later`。
// - 错误：存储读/写失败与刷新失败都被包装成带 cause 的 ModelsError。
// 断言里的错误消息文本是 TS 的逐字契约，改动它等同于改动对外行为。
#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::types::{
        ApiKeyAuth, AuthCheck, AuthType, CredentialModifyFuture, ModelAuth, OAuthAuth,
    };
    use crate::error::ModelsErrorCode;
    use indexmap::IndexMap;

    struct TestContext {
        env: IndexMap<String, String>,
    }

    impl TestContext {
        fn from_pairs(values: &[(&str, &str)]) -> Arc<dyn AuthContext> {
            Arc::new(TestContext {
                env: values
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect(),
            })
        }
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

    struct FixedApiKey {
        result: Mutex<Option<AuthResult>>,
        calls: AtomicUsize,
    }

    impl FixedApiKey {
        fn new(result: Option<AuthResult>) -> Arc<FixedApiKey> {
            Arc::new(FixedApiKey {
                result: Mutex::new(result),
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ApiKeyAuth for FixedApiKey {
        fn name(&self) -> &str {
            "Fixed"
        }

        async fn resolve(
            &self,
            input: ApiKeyAuthInput<'_>,
        ) -> Result<Option<AuthResult>, AuthError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            throw_if_aborted(input.signal)?;
            Ok(self.result.lock().unwrap().clone())
        }
    }

    struct FakeOAuth {
        refresh_results: Mutex<Vec<Result<OAuthCredential, AuthError>>>,
        refresh_calls: AtomicUsize,
        to_auth_calls: AtomicUsize,
        on_refresh: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl FakeOAuth {
        fn new() -> Arc<FakeOAuth> {
            Arc::new(FakeOAuth {
                refresh_results: Mutex::new(Vec::new()),
                refresh_calls: AtomicUsize::new(0),
                to_auth_calls: AtomicUsize::new(0),
                on_refresh: Mutex::new(None),
            })
        }

        fn push_refresh(&self, result: Result<OAuthCredential, AuthError>) {
            self.refresh_results.lock().unwrap().push(result);
        }
    }

    #[async_trait]
    impl OAuthAuth for FakeOAuth {
        fn name(&self) -> &str {
            "Fake OAuth"
        }

        async fn login(
            &self,
            _interaction: &dyn crate::auth::types::AuthInteraction,
            _options: &crate::auth::types::LoginOptions,
        ) -> Result<OAuthCredential, AuthError> {
            Err(AuthError::new("unused"))
        }

        async fn refresh(
            &self,
            _credential: &OAuthCredential,
            _signal: &AbortScope,
        ) -> Result<OAuthCredential, AuthError> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = self.on_refresh.lock().unwrap().clone() {
                hook();
            }
            let mut results = self.refresh_results.lock().unwrap();
            if results.is_empty() {
                Err(AuthError::new("no refresh result"))
            } else {
                results.remove(0)
            }
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            self.to_auth_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelAuth {
                api_key: Some(credential.access.clone()),
                ..Default::default()
            })
        }
    }

    fn store() -> Arc<dyn CredentialStore> {
        Arc::new(InMemoryCredentialStore::new())
    }

    async fn put(store: &Arc<dyn CredentialStore>, provider: &str, credential: Credential) {
        store
            .modify(
                provider,
                Box::new(move |_| {
                    let credential = credential.clone();
                    Box::pin(async move { Ok(Some(credential)) }) as CredentialModifyFuture
                }),
                AuthOperationOptions::default(),
            )
            .await
            .expect("modify");
    }

    fn api_key_credential(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.into()),
            env: None,
            extra: Default::default(),
        })
    }

    fn oauth_credential(access: &str, expires: f64) -> Credential {
        Credential::OAuth(OAuthCredential::new(access, "refresh", expires))
    }

    /// `Date.now()` 的等价物（测试用）。
    fn now() -> f64 {
        now_ms()
    }

    #[tokio::test]
    async fn ambient_key_resolves_only_when_nothing_is_stored() {
        let store = store();
        let ctx = TestContext::from_pairs(&[("OPENAI_API_KEY", "env-key")]);
        let auth = ProviderAuth {
            api_key: Some(crate::auth::helpers::env_api_key_auth(
                "OpenAI API key",
                &["OPENAI_API_KEY"],
            )),
            oauth: None,
        };
        let result = resolve_provider_auth(
            "openai",
            &auth,
            Arc::clone(&store),
            ctx,
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve")
        .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("env-key"));
        assert_eq!(result.source.as_deref(), Some("OPENAI_API_KEY"));

        put(&store, "openai", api_key_credential("stored-key")).await;
        let result = resolve_provider_auth(
            "openai",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[("OPENAI_API_KEY", "env-key")]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve")
        .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
    }

    #[tokio::test]
    async fn explicit_api_key_override_requires_an_api_key_handler() {
        let store = store();
        let api_key_auth = FixedApiKey::new(Some(AuthResult::new(ModelAuth {
            api_key: Some("override".into()),
            ..Default::default()
        })));
        let auth = ProviderAuth {
            api_key: Some(Arc::clone(&api_key_auth) as Arc<dyn ApiKeyAuth>),
            oauth: None,
        };
        let result = resolve_provider_auth(
            "p",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides {
                api_key: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("resolve");
        // 空串也算"提供"：apiKey.resolve 被调用并自行决定。
        assert!(result.is_some());

        let oauth_only = ProviderAuth {
            api_key: None,
            oauth: Some(FakeOAuth::new()),
        };
        let result = resolve_provider_auth(
            "p",
            &oauth_only,
            store,
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides {
                api_key: Some("override".into()),
                ..Default::default()
            },
        )
        .await
        .expect("resolve");
        assert!(
            result.is_none(),
            "pure OAuth provider ignores apiKey overrides"
        );
    }

    #[tokio::test]
    async fn stored_oauth_resolves_without_refreshing_inside_the_window() {
        let store = store();
        put(
            &store,
            "anthropic",
            oauth_credential("access", now() + 10.0 * 60_000.0),
        )
        .await;
        let oauth = FakeOAuth::new();
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let result = resolve_provider_auth(
            "anthropic",
            &auth,
            store,
            TestContext::from_pairs(&[("ANTHROPIC_API_KEY", "env")]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve")
        .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("access"));
        assert_eq!(result.source.as_deref(), Some("OAuth"));
        assert!(result.env.is_none());
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn expired_oauth_refreshes_under_the_lock_and_persists() {
        let store = store();
        put(&store, "anthropic", oauth_credential("old", now() - 1.0)).await;
        let oauth = FakeOAuth::new();
        oauth.push_refresh(Ok(OAuthCredential::new(
            "new",
            "new-refresh",
            now() + 60.0 * 60_000.0,
        )));
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let result = resolve_provider_auth(
            "anthropic",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve")
        .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("new"));
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 1);
        let stored = store
            .read("anthropic", AuthOperationOptions::default())
            .await
            .unwrap()
            .expect("stored");
        assert_eq!(stored.as_oauth().unwrap().refresh, "new-refresh");
    }

    #[tokio::test]
    async fn concurrent_resolutions_refresh_only_once() {
        let store = store();
        put(&store, "p", oauth_credential("old", now() - 1.0)).await;
        let oauth = FakeOAuth::new();
        oauth.push_refresh(Ok(OAuthCredential::new(
            "new",
            "new-refresh",
            now() + 60.0 * 60_000.0,
        )));
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let (first, second) = tokio::join!(
            resolve_provider_auth(
                "p",
                &auth,
                Arc::clone(&store),
                TestContext::from_pairs(&[]),
                AuthResolutionOverrides::default()
            ),
            resolve_provider_auth(
                "p",
                &auth,
                Arc::clone(&store),
                TestContext::from_pairs(&[]),
                AuthResolutionOverrides::default()
            )
        );
        assert_eq!(first.unwrap().unwrap().auth.api_key.as_deref(), Some("new"));
        assert_eq!(
            second.unwrap().unwrap().auth.api_key.as_deref(),
            Some("new")
        );
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn refresh_failures_wrap_as_models_errors_without_env_fallback() {
        let store = store();
        put(&store, "p", oauth_credential("old", now() - 1.0)).await;
        let oauth = FakeOAuth::new();
        oauth.push_refresh(Err(AuthError::new("invalid_grant")));
        let auth = ProviderAuth {
            api_key: Some(crate::auth::helpers::env_api_key_auth(
                "P API key",
                &["P_API_KEY"],
            )),
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let error = resolve_provider_auth(
            "p",
            &auth,
            store,
            TestContext::from_pairs(&[("P_API_KEY", "env-key")]),
            AuthResolutionOverrides::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.name, "ModelsError");
        assert_eq!(error.code, Some(ModelsErrorCode::OAuth));
        assert_eq!(error.message, "OAuth refresh failed for p: invalid_grant");
    }

    #[tokio::test]
    async fn stored_credentials_without_a_matching_handler_do_not_fall_back_to_env() {
        let store = store();
        put(
            &store,
            "p",
            oauth_credential("access", now() + 10.0 * 60_000.0),
        )
        .await;
        let api_key = FixedApiKey::new(Some(AuthResult::new(ModelAuth {
            api_key: Some("env".into()),
            ..Default::default()
        })));
        let auth = ProviderAuth {
            api_key: Some(Arc::clone(&api_key) as Arc<dyn ApiKeyAuth>),
            oauth: None,
        };
        let result = resolve_provider_auth(
            "p",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve");
        assert!(result.is_none());
        assert_eq!(api_key.calls.load(Ordering::SeqCst), 0);

        put(&store, "q", api_key_credential("stored")).await;
        let oauth_only = ProviderAuth {
            api_key: None,
            oauth: Some(FakeOAuth::new()),
        };
        let result = resolve_provider_auth(
            "q",
            &oauth_only,
            store,
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn env_overrides_merge_into_stored_api_key_credentials_and_the_context() {
        let store = store();
        put(&store, "p", api_key_credential("stored-key")).await;
        let api_key = FixedApiKey::new(Some(AuthResult::new(ModelAuth {
            api_key: Some("stored-key".into()),
            ..Default::default()
        })));
        let auth = ProviderAuth {
            api_key: Some(Arc::clone(&api_key) as Arc<dyn ApiKeyAuth>),
            oauth: None,
        };
        let mut env = ProviderEnv::new();
        env.insert("CLOUDFLARE_ACCOUNT_ID".into(), "acct".into());
        let result = resolve_provider_auth(
            "p",
            &auth,
            store,
            TestContext::from_pairs(&[("CLOUDFLARE_ACCOUNT_ID", "base")]),
            AuthResolutionOverrides {
                env: Some(env),
                ..Default::default()
            },
        )
        .await
        .expect("resolve")
        .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
    }

    #[tokio::test]
    async fn min_oauth_validity_triggers_and_post_checks_the_refresh() {
        let store = store();
        // 剩余 10 分钟：默认窗口不刷新，但显式要求 30 分钟时刷新。
        put(
            &store,
            "p",
            oauth_credential("old", now() + 10.0 * 60_000.0),
        )
        .await;
        let oauth = FakeOAuth::new();
        oauth.push_refresh(Ok(OAuthCredential::new(
            "new",
            "r",
            now() + 20.0 * 60_000.0,
        )));
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let error = resolve_provider_auth(
            "p",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides {
                min_oauth_validity_ms: Some(30 * 60 * 1000),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name, "ModelsError");
        assert_eq!(error.code, Some(ModelsErrorCode::OAuth));
        assert_eq!(
            error.message,
            "OAuth refresh returned a token that expires too soon for p"
        );
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_stops_waiting_for_non_cooperative_callbacks() {
        struct Blocked {
            started: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        }
        #[async_trait]
        impl ApiKeyAuth for Blocked {
            fn name(&self) -> &str {
                "Blocked"
            }

            async fn resolve(
                &self,
                _input: ApiKeyAuthInput<'_>,
            ) -> Result<Option<AuthResult>, AuthError> {
                self.started.notify_one();
                self.release.notified().await;
                Ok(Some(AuthResult::new(ModelAuth {
                    api_key: Some("key".into()),
                    ..Default::default()
                })))
            }
        }
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let auth = ProviderAuth {
            api_key: Some(Arc::new(Blocked {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            })),
            oauth: None,
        };
        let controller = AbortScope::new();
        let resolve = resolve_provider_auth(
            "p",
            &auth,
            store(),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides {
                signal: Some(controller.clone()),
                ..Default::default()
            },
        );
        let waiter = Arc::clone(&started);
        let aborter = controller.clone();
        tokio::spawn(async move {
            waiter.notified().await;
            aborter.abort();
        });
        let error = resolve.await.unwrap_err();
        assert_eq!(error.name, "AbortError");
        assert_eq!(error.message, "This operation was aborted");
        release.notify_one();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_takes_precedence_over_a_concurrent_operation_error() {
        struct FailsOnAbort;
        #[async_trait]
        impl ApiKeyAuth for FailsOnAbort {
            fn name(&self) -> &str {
                "FailsOnAbort"
            }

            async fn resolve(
                &self,
                input: ApiKeyAuthInput<'_>,
            ) -> Result<Option<AuthResult>, AuthError> {
                // 中止之后才报错（与取消网络请求的实现一致）。
                input.signal.cancelled().await;
                Err(AuthError::new("boom"))
            }
        }
        let auth = ProviderAuth {
            api_key: Some(Arc::new(FailsOnAbort)),
            oauth: None,
        };
        // 中止与失败的 resolve 同时就绪时，`tokio::select!` 的选择是随机的；TS 的 abort
        // 事件同步派发，调用方必须总是看到 `AbortError`（而不是 `ModelsError`）。
        // 多线程 runtime 让 abort 与 resolve 真正并发，覆盖两个分支同时就绪的窗口。
        for iteration in 0..2000 {
            let controller = AbortScope::new();
            let aborter = controller.clone();
            tokio::spawn(async move {
                aborter.abort();
            });
            let resolve = resolve_provider_auth(
                "p",
                &auth,
                store(),
                TestContext::from_pairs(&[]),
                AuthResolutionOverrides {
                    signal: Some(controller),
                    ..Default::default()
                },
            );
            let error = resolve.await.unwrap_err();
            assert_eq!(
                error.name, "AbortError",
                "iteration {iteration} returned {error:?}"
            );
            assert_eq!(error.message, "This operation was aborted");
        }
    }

    #[tokio::test]
    async fn an_oauth_refresh_that_started_before_cancellation_is_persisted() {
        let store = store();
        put(&store, "p", oauth_credential("old", now() - 1.0)).await;
        let oauth = FakeOAuth::new();
        let controller = AbortScope::new();
        let cancel = controller.clone();
        *oauth.on_refresh.lock().unwrap() = Some(Arc::new(move || cancel.abort()));
        oauth.push_refresh(Ok(OAuthCredential::new(
            "new",
            "new-refresh",
            now() + 60.0 * 60_000.0,
        )));
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let error = resolve_provider_auth(
            "p",
            &auth,
            Arc::clone(&store),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides {
                signal: Some(controller),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name, "AbortError");

        // 刷新继续跑完并持久化（等它落地）。
        for _ in 0..100 {
            let stored = store
                .read("p", AuthOperationOptions::default())
                .await
                .unwrap()
                .expect("stored");
            if stored.as_oauth().unwrap().refresh == "new-refresh" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("refreshed credential was not persisted");
    }

    #[tokio::test]
    async fn store_read_failures_wrap_as_auth_models_errors() {
        struct BrokenStore;
        #[async_trait]
        impl CredentialStore for BrokenStore {
            async fn read(
                &self,
                _provider_id: &str,
                _options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                Err(AuthError::new("disk on fire"))
            }
            async fn list(
                &self,
                _options: AuthOperationOptions,
            ) -> Result<Vec<crate::auth::types::CredentialInfo>, AuthError> {
                Ok(Vec::new())
            }
            async fn modify(
                &self,
                _provider_id: &str,
                _f: CredentialModifyFn,
                _options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                Ok(None)
            }
            async fn delete(
                &self,
                _provider_id: &str,
                _options: AuthOperationOptions,
            ) -> Result<(), AuthError> {
                Ok(())
            }
        }
        let auth = ProviderAuth {
            api_key: Some(FixedApiKey::new(None) as Arc<dyn ApiKeyAuth>),
            oauth: None,
        };
        let error = resolve_provider_auth(
            "p",
            &auth,
            Arc::new(BrokenStore),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.name, "ModelsError");
        assert_eq!(error.code, Some(ModelsErrorCode::Auth));
        assert_eq!(
            error.message,
            "Credential store read failed for p: disk on fire"
        );
    }

    #[tokio::test]
    async fn store_modify_failures_wrap_as_auth_models_errors() {
        struct BrokenModifyStore;
        #[async_trait]
        impl CredentialStore for BrokenModifyStore {
            async fn read(
                &self,
                _provider_id: &str,
                _options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                Ok(Some(oauth_credential("old", 0.0)))
            }
            async fn list(
                &self,
                _options: AuthOperationOptions,
            ) -> Result<Vec<crate::auth::types::CredentialInfo>, AuthError> {
                Ok(Vec::new())
            }
            async fn modify(
                &self,
                _provider_id: &str,
                _f: CredentialModifyFn,
                _options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                Err(AuthError::new("lock failed"))
            }
            async fn delete(
                &self,
                _provider_id: &str,
                _options: AuthOperationOptions,
            ) -> Result<(), AuthError> {
                Ok(())
            }
        }
        let oauth = FakeOAuth::new();
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let error = resolve_provider_auth(
            "p",
            &auth,
            Arc::new(BrokenModifyStore),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.name, "ModelsError");
        assert_eq!(error.code, Some(ModelsErrorCode::Auth));
        assert_eq!(
            error.message,
            "Credential store modify failed for p: lock failed"
        );
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn refresh_resolves_to_none_when_the_provider_is_logged_out_meanwhile() {
        // 乐观检查看到 oauth 凭证，但进入锁内时存储已被替换为 api_key（另一个进程 logout/login）。
        struct RacingStore {
            credentials: Arc<dyn CredentialStore>,
            replaced: std::sync::atomic::AtomicBool,
        }
        #[async_trait]
        impl CredentialStore for RacingStore {
            async fn read(
                &self,
                provider_id: &str,
                options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                self.credentials.read(provider_id, options).await
            }
            async fn list(
                &self,
                options: AuthOperationOptions,
            ) -> Result<Vec<crate::auth::types::CredentialInfo>, AuthError> {
                self.credentials.list(options).await
            }
            async fn modify(
                &self,
                provider_id: &str,
                f: CredentialModifyFn,
                options: AuthOperationOptions,
            ) -> Result<Option<Credential>, AuthError> {
                if !self.replaced.swap(true, Ordering::SeqCst) {
                    self.credentials
                        .modify(
                            provider_id,
                            Box::new(|_| Box::pin(async { Ok(Some(api_key_credential("other"))) })),
                            AuthOperationOptions::default(),
                        )
                        .await?;
                }
                self.credentials.modify(provider_id, f, options).await
            }
            async fn delete(
                &self,
                provider_id: &str,
                options: AuthOperationOptions,
            ) -> Result<(), AuthError> {
                self.credentials.delete(provider_id, options).await
            }
        }
        let inner = store();
        put(&inner, "p", oauth_credential("old", 0.0)).await;
        let oauth = FakeOAuth::new();
        let auth = ProviderAuth {
            api_key: None,
            oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
        };
        let result = resolve_provider_auth(
            "p",
            &auth,
            Arc::new(RacingStore {
                credentials: inner,
                replaced: std::sync::atomic::AtomicBool::new(false),
            }),
            TestContext::from_pairs(&[]),
            AuthResolutionOverrides::default(),
        )
        .await
        .expect("resolve");
        assert!(result.is_none());
        assert_eq!(oauth.refresh_calls.load(Ordering::SeqCst), 0);
    }

    #[allow(dead_code)]
    fn assert_auth_type_is_public() -> AuthType {
        AuthType::OAuth
    }

    #[allow(dead_code)]
    fn assert_auth_check_is_public(check: AuthCheck) -> AuthCheck {
        check
    }
}
