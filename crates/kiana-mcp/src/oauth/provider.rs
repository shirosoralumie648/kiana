//! `oauth/provider.ts`：`McpOAuthProvider`、`McpOAuthState` 与状态存储抽象。
//!
//! TS 源：`packages/mcp/src/oauth/provider.ts`（`spec/telemetry-mcp.md` §18.5、§20.4、§30.17、§30.18）。
//!
//! `mcp-auth.json` 的持久化形状就是 [`McpOAuthState`]（camelCase、`undefined` 字段省略）；
//! 嵌入方（coding-agent）把它存成 `Record<string, McpOAuthState>`，本 crate **不**写文件。
//!
//! # 在系统中的位置
//!
//! `oauth/flow.ts` 侧的流程（`authorizeMcp`、`runFlow`、`adaptOAuthProvider` 等）只依赖
//! [`crate::oauth::flow::OAuthClientProvider`] trait，通过 `&dyn`/`Arc<dyn>` 使用任意实现
//! （包括本类型）；`refreshAuthorization`（`flow.rs` 里的 `refresh_authorization`）是纯 token
//! 端点请求，只接收授权服务器 URL 与 `RefreshAuthorizationOptions`，签名里没有 provider，
//! 不经过该 trait。本类型由嵌入方构造（工作区里是 `kiana-builtin-extensions` 的
//! `create_provider`）。需要持久化时由嵌入方提供 [`McpOAuthStateStore`]：本模块自带的
//! [`MemoryOAuthStateStore`] 只在进程内保存一份状态，不做文件 IO，也不负责 `mcp-auth.json`
//! 的并发与加锁；工作区里真正的文件存储由 `kiana-builtin-extensions` 的 `McpOAuthServerStore`
//! 按"命名空间 + 规范化 URL"分键实现。
//!
//! # 逐字复刻的语义（不要"改进"）
//!
//! - **服务器 URL 绑定**：`McpOAuthProvider::own` 只接受 `serverUrl` 与自身**规范化 href 逐字相等**
//!   的状态，否则返回 `{serverUrl}` 一个空状态。凭据绝不跨服务器 URL 泄漏
//!   （`binds persisted credentials to the exact MCP server URL`）。
//! - **配置的客户端不可变**：构造时给了非空 `clientId` 就得到 `configuredClient`，
//!   此时 `saveClientInformation` 是 no-op（`hasSaveClientInformation` 仍为 `true`，因为 TS 的
//!   方法存在）。
//! - **`state()`**：已有非空 `oauthState` 直接返回；否则 32 个随机字节的**小写 hex**（64 字符），
//!   **先**经 `update` 持久化再返回。TS 里两个并发 `state()` 会各自生成、后者覆盖前者（§30.18）；
//!   这里是同样的简单实现（`update` 串行化但不合并），未做去重。
//! - **`saveTokens`**：`tokensExpireAt = Date.now() + expires_in*1000`（JS `ToNumber`：
//!   `null`/`false` → 0、`"3600"` → 3600、`true` → 1、`"abc"` → `NaN` 写成 JSON `null`）；
//!   `expires_in` 属性不存在（`undefined`）时**删除**该键。`Date.now()` 用系统时钟的纪元毫秒。
//! - **`update` 的错误链（§30.17）**：TS 的 `this.writes` 是 promise 链，任何一次 `store.save`
//!   （或链内的 `store.load`）失败都会让链变成 rejected，**后续所有** `update`/`load` 永远以同一个
//!   错误失败（并且更新丢失）。这里用一个 [`tokio::sync::Mutex`] 串行化并保存
//!   `poisoned: Option<OAuthFailure>` 复刻这一"一次失败永久毒化"的行为，而不是"下一次调用又能成功"
//!   的普通互斥。注意 `clientInformation()` 在配置了客户端时**不**读链，因此不受毒化影响（同 TS 的
//!   `configuredClient ?? (await this.load()).clientInformation` 短路）。
//! - **`invalidateCredentials`**：`all` 清除 clientInformation/tokens/tokensExpireAt/codeVerifier/
//!   discovery/oauthState；`tokens` 同时清 tokens 与 tokensExpireAt；`oauthState` **只**在 `all`
//!   时清除。
//! - **`codeVerifier()`**：缺失或空串都报 `Error("No OAuth PKCE code verifier is stored")`。
//! - **`MemoryOAuthStateStore`** 的 `structuredClone` 语义：[`McpOAuthState`] 的 `Clone` 是深拷贝
//!   （字段全是拥有所有权的值），`load`/`save` 各克隆一次。
//! - **持久化 JSON 的 `null` 与未知嵌套键**：TS 的 `{...value}` 浅拷贝会保留已知键的显式
//!   `null`（例如 `"tokensExpireAt": null`，消费者用 `!== undefined` 判断"有值"）以及
//!   `discovery` 里未知的嵌套属性。Rust 用 [`McpOAuthState::null_keys`] 与
//!   [`McpOAuthState::discovery_raw`] 记录这两类信息，并在下一次写入时原样写回。
//!
//! # 与 TS 的已知偏差
//!
//! - `onRedirect` 在 TS 里可以返回 Promise；Rust 侧是返回 [`BoxFuture`] 的闭包，
//!   `redirect_to_authorization` 会 await 它。同步回调写成 `Box::pin(async move { Ok(()) })`。
//! - `new URL(options.serverUrl)` 失败在 TS 里是构造期抛 `TypeError("Invalid URL")`；
//!   Rust 的 [`McpOAuthProvider::new`] 返回 `Err`（同样的 `name`/消息）。
//! - `tokensExpireAt` 写成 JSON 数字时按 JS 的整数样式（`1700000000000`，不是
//!   `1700000000000.0`）；超出 ±2^53 或非整数的值退化为 `serde_json` 的浮点写法
//!   （`Date.now()` + `expires_in` 的现实中不会出现）。
//! - 已知键在 JSON 里类型不符时按缺席读取（TS 会原样保留任意值）；这是 typed struct 的
//!   固有限制，解析器/线上路径不会产生这种状态。`discovery` 是例外：非 null 的任意值都
//!   保存在 [`McpOAuthState::discovery_raw`] 里原样写回，但 [`McpOAuthProvider::discovery_state`]
//!   仍只能返回能解析成 [`OAuthDiscoveryState`] 的对象（TS 会返回任意值）。
//! - 已知键的显式 `null` 在 typed getter 里读作 `None`（TS 返回 `null`）；两者在 truthiness
//!   判断下等价，持久化 JSON 的形状由 [`McpOAuthState::null_keys`] 保留。唯一语义差异是
//!   `tokensExpireAt`（见该字段文档）。
//!
//! # 并发模型与锁范围
//!
//! - provider 自身唯一的共享可变状态是 `McpOAuthProvider.writes`（[`tokio::sync::Mutex`] 包着 `WriteChain`）。
//!   用 tokio 的互斥量而不是 `std::sync::Mutex`，是因为 guard 必须跨 `store.load()` /
//!   `store.save()` 的 `.await` 持有；持锁范围覆盖整段 read-modify-write，所以同一 provider
//!   上的 `update` 严格串行，`load` 也要先取得同一把锁（会等在途写完成）。
//! - 锁只覆盖"状态存储的读改写"：`on_redirect` 在锁外 await；不读取状态存储的 getter
//!   （`server_url()`/`redirect_url()`/`client_metadata()`，以及配置了客户端时的
//!   `client_information()`）不取锁；`tokens()`/`code_verifier()`/`discovery_state()` 与
//!   未配置客户端时的 `client_information()` 都走 `load()`，会取同一把写锁（等在途写完成）。
//!   因此回调执行得慢不会阻塞状态读写。
//! - 写链毒化后不再调用 store：所有 `update`/`load` 直接返回第一次错误的克隆（单元测试
//!   `a_failed_write_poisons_the_chain` 断言 store 的 `save` 只执行了一次）。
//! - `load()` 读路径上的 `store.load()` 失败**不**毒化写链（TS 的 `load` 只 `await this.writes`，
//!   没有把结果 promise 写回链）；只有 `update` 闭包内的 `store.load`/`store.save` 失败才毒化。
//! - **取消与任务生命周期**：provider 不创建任务，`load`/`update` 的进度完全由调用方的 future
//!   驱动。调用方 drop 尚未完成的 future 时，`writes.lock().await` 取得的 guard 是 `load`/
//!   `update` 的局部变量，随 future 一起释放（互斥量不会留下"被占用的锁"）；而 `update` 只在
//!   `store.load`/`store.save` 返回 `Err` 时才写 `poisoned`，中途取消不会走到那一步，因此取消
//!   不会毒化写链，后续调用可以重新开始。若取消恰好发生在 `store.load`/`store.save` 的 await
//!   中，存储侧是否留下半完成的副作用由具体实现决定；本 crate 的 [`MemoryOAuthStateStore`] 与
//!   工作区的 `McpOAuthServerStore` 的 `load`/`save` 内部都没有 await 点（同步完成读写后直接
//!   返回），对它们而言取消不会在半途打断一次存储操作。
//! - 配置了客户端时，`client_information()` 走短路、`save_client_information()` 直接返回
//!   `Ok(())`，两者都不取锁，因此写链毒化后仍然可用（对应 TS 的 `configuredClient ?? …`）。
//! - [`McpOAuthStateStore`] 的方法是在写锁内被 await 的：实现里不得回调同一个 provider
//!   （例如在 `save` 里调用 `provider.tokens()`），否则会自锁。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_core::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::oauth::discovery::js_url;
use crate::oauth::errors::OAuthFailure;
use crate::oauth::flow::{CredentialKind, OAuthClientMetadataDocument, OAuthClientProvider};
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthClientInformationMixed, OAuthClientMetadata,
    OAuthDiscoveryState, OAuthTokens, js_number,
};

/// `clientMetadataDocument(metadata)`：没有存储的客户端信息时，用一个 CIMD URL 充当 `client_id`
/// 而不是动态注册。
///
/// TS 的签名是 `(metadata: AuthorizationServerMetadata | undefined) => OAuthClientMetadataDocument | undefined`，
/// **同步**且可以抛异常。
pub type ClientMetadataDocumentFn = Arc<
    dyn Fn(
            Option<&AuthorizationServerMetadata>,
        ) -> Result<Option<OAuthClientMetadataDocument>, OAuthFailure>
        + Send
        + Sync,
>;

/// `onRedirect(url)`：`(url: URL) => void | Promise<void>`（TS 允许同步或异步）。
pub type OnRedirectFn =
    Arc<dyn Fn(Url) -> BoxFuture<'static, Result<(), OAuthFailure>> + Send + Sync>;

/// `McpOAuthState` 的已知键（TS 接口的书写顺序；也是补齐"还没记录键序"的字段时的顺序）。
const KNOWN_STATE_KEYS: [&str; 7] = [
    "serverUrl",
    "clientInformation",
    "tokens",
    "tokensExpireAt",
    "codeVerifier",
    "oauthState",
    "discovery",
];

/// `McpOAuthState`：一个 MCP 服务器 URL 的持久化状态（`mcp-auth.json` 的值形状）。
///
/// `undefined` 字段省略；`serverUrl` 缺失时反序列化成空串——它不会等于任何 provider 的
/// 规范化 href，于是 `own()` 会把它当作"别的服务器的状态"清空（与 TS 对损坏文件的处理一致）。
///
/// # 键序与未知键（TS 的 spread 语义）
///
/// TS 的 `update` 用 `{...value, key: …}`，所以持久化对象的键序是**操作历史**：已有键保持
/// 原位，新键追加到末尾（`delete` 之后重新添加也追加）。[`key_order`](Self::key_order) 记录
/// 这个顺序：反序列化时取 JSON 的键序，`update` 时按修改前/后的差异维护（见
/// `reconcile_key_order`）。序列化先按 `key_order` 输出，仍未覆盖的已知键按接口顺序、
/// 未知键按原相对顺序补齐。
///
/// 未知键保存在 [`extra`](Self::extra)：TS 的 spread 会把它们原样带进下一次写入，
/// 这里同样在 `load` → `update` → `save` 之间保留并写回。
///
/// 显式 `null` 的已知键记录在 [`null_keys`](Self::null_keys)：TS 的 `{...value}` 保留
/// `"tokensExpireAt": null` 这样的键（消费者用 `!== undefined` 判断"有值"，null 算有值），
/// Rust 的 `Option` 无法区分"缺席"和 `null`，因此单独记录并在序列化时写回 `null`。
/// `discovery` 的原始 JSON 保存在 [`discovery_raw`](Self::discovery_raw)：TS 的浅拷贝会把
/// 未知的嵌套属性（包括 `authorizationServerMetadata` 里的）一起带进下一次写入。
#[derive(Debug, Clone, Default)]
pub struct McpOAuthState {
    /// `serverUrl`：状态所属的服务器 URL（规范化 href）。
    pub server_url: String,
    /// `clientInformation`：动态注册或配置的客户端。
    pub client_information: Option<OAuthClientInformationMixed>,
    /// `tokens`：已保存的令牌。
    pub tokens: Option<OAuthTokens>,
    /// `tokensExpireAt`：保存时按 `expires_in` 算出的过期时刻（纪元毫秒）。
    ///
    /// 持久化 JSON 里是显式 `null` 时这里是 `None` 且 [`null_keys`](Self::null_keys) 含该键——
    /// TS 的消费者（`tokensExpireAt !== undefined`）把 `null` 当作"有值且已过期"，Rust 消费者
    /// 需要同时检查 `null_keys` 才能复刻这一判断（`null - skew <= Date.now()` 恒为真）。
    pub tokens_expire_at: Option<f64>,
    /// `codeVerifier`：进行中的 PKCE verifier。
    pub code_verifier: Option<String>,
    /// `oauthState`：进行中的授权请求的 `state`。
    pub oauth_state: Option<String>,
    /// `discovery`：缓存的发现结果。
    pub discovery: Option<OAuthDiscoveryState>,
    /// 状态对象里**未知**的键（`{...value}` 原样保留，按原相对顺序写回）。
    pub extra: Map<String, Value>,
    /// 键的插入历史（`Object.keys()` 顺序），决定序列化时的键序；不参与相等比较。
    pub key_order: Vec<String>,
    /// 持久化 JSON 里值为显式 `null` 的已知可选键（TS 会原样保留这些键）。
    pub null_keys: Vec<String>,
    /// `discovery` 的原始 JSON 值（保留 TS 浅拷贝带过来的未知嵌套属性）。
    ///
    /// 只在 `discovery` 仍等于该原始值的 typed 投影时用于序列化；`saveDiscoveryState`
    /// 或 `invalidateCredentials` 会清掉它。不参与相等比较（与 `key_order` 一样是序列化细节）。
    pub discovery_raw: Option<Value>,
}

impl PartialEq for McpOAuthState {
    /// 相等按字段值判断（键序/原始 discovery 无语义，和 `JSON.stringify` 的读取方一致）；
    /// 显式 `null` 与缺席的序列化结果不同，因此 `null_keys` 参与比较。
    fn eq(&self, other: &Self) -> bool {
        self.server_url == other.server_url
            && self.client_information == other.client_information
            && self.tokens == other.tokens
            && self.tokens_expire_at == other.tokens_expire_at
            && self.code_verifier == other.code_verifier
            && self.oauth_state == other.oauth_state
            && self.discovery == other.discovery
            && self.extra == other.extra
            && self.null_keys == other.null_keys
    }
}

impl McpOAuthState {
    /// `{ serverUrl }`：只属于某个服务器、还没有任何凭据的状态。
    ///
    /// `key_order` 预先放入 `serverUrl`，因此序列化时它固定是第一个键（与 TS 的
    /// `{ serverUrl }` 对象一致）。
    pub fn new(server_url: impl Into<String>) -> Self {
        McpOAuthState {
            server_url: server_url.into(),
            key_order: vec!["serverUrl".to_owned()],
            ..McpOAuthState::default()
        }
    }

    /// `key in state`（`undefined`/缺席为假；显式 `null` 也算存在）。
    fn has_key(&self, key: &str) -> bool {
        match key {
            // 必填字段：序列化总会写 `serverUrl`（哪怕值是空串），所以它永远算"存在"。
            "serverUrl" => true,
            "clientInformation" => self.client_information.is_some() || self.is_explicit_null(key),
            "tokens" => self.tokens.is_some() || self.is_explicit_null(key),
            "tokensExpireAt" => self.tokens_expire_at.is_some() || self.is_explicit_null(key),
            "codeVerifier" => self.code_verifier.is_some() || self.is_explicit_null(key),
            "oauthState" => self.oauth_state.is_some() || self.is_explicit_null(key),
            "discovery" => {
                self.discovery.is_some()
                    || self.discovery_raw.is_some()
                    || self.is_explicit_null(key)
            }
            other => self.extra.contains_key(other),
        }
    }

    /// 该已知可选键在持久化 JSON 里是显式 `null`。
    fn is_explicit_null(&self, key: &str) -> bool {
        self.null_keys.iter().any(|existing| existing == key)
    }

    /// 把某个键的显式 `null` 标记清掉（`update` 里写入或删除该键时调用）。
    fn clear_explicit_null(&mut self, key: &str) {
        self.null_keys.retain(|existing| existing != key);
    }

    /// 输出顺序：`key_order`（过滤掉已删除的键）→ 已知键的接口顺序 → 未知键的原顺序。
    fn serialized_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for key in &self.key_order {
            self.push_key(key, &mut keys);
        }
        for key in KNOWN_STATE_KEYS {
            self.push_key(key, &mut keys);
        }
        for key in self.extra.keys() {
            self.push_key(key, &mut keys);
        }
        keys
    }

    /// 键存在且尚未收录时追加（保持首次出现的位置）。
    fn push_key(&self, key: &str, keys: &mut Vec<String>) {
        if self.has_key(key) && !keys.iter().any(|existing| existing == key) {
            keys.push(key.to_owned());
        }
    }
}

/// `update` 后的键序维护：修改前存在的键保持原位（被删除的去掉），新键追加到末尾。
///
/// 这正是 `{...value, key: …}` / `delete value.key` 在 JS 对象上的可观察结果。
fn reconcile_key_order(previous: &McpOAuthState, next: &mut McpOAuthState) {
    let mut keys: Vec<String> = Vec::new();
    for key in &previous.key_order {
        next.push_key(key, &mut keys);
    }
    for key in previous.extra.keys() {
        next.push_key(key, &mut keys);
    }
    for key in KNOWN_STATE_KEYS {
        next.push_key(key, &mut keys);
    }
    for key in next.extra.keys() {
        next.push_key(key, &mut keys);
    }
    next.key_order = keys;
}

impl Serialize for McpOAuthState {
    /// `JSON.stringify(state)`：按 TS 对象的键序输出，`undefined` 字段省略。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap as _;

        let keys = self.serialized_keys();
        let mut map = serializer.serialize_map(Some(keys.len()))?;
        for key in &keys {
            match key.as_str() {
                "serverUrl" => map.serialize_entry("serverUrl", &self.server_url)?,
                "clientInformation" => {
                    if let Some(value) = &self.client_information {
                        map.serialize_entry("clientInformation", value)?;
                    } else if self.is_explicit_null("clientInformation") {
                        map.serialize_entry("clientInformation", &Value::Null)?;
                    }
                }
                "tokens" => {
                    if let Some(value) = &self.tokens {
                        map.serialize_entry("tokens", value)?;
                    } else if self.is_explicit_null("tokens") {
                        map.serialize_entry("tokens", &Value::Null)?;
                    }
                }
                "tokensExpireAt" => {
                    if let Some(value) = self.tokens_expire_at {
                        // JS 的 `JSON.stringify` 把 1.7e12 写成 `1700000000000`，`NaN` 写成 `null`。
                        map.serialize_entry("tokensExpireAt", &js_number_value(value))?;
                    } else if self.is_explicit_null("tokensExpireAt") {
                        map.serialize_entry("tokensExpireAt", &Value::Null)?;
                    }
                }
                "codeVerifier" => {
                    if let Some(value) = &self.code_verifier {
                        map.serialize_entry("codeVerifier", value)?;
                    } else if self.is_explicit_null("codeVerifier") {
                        map.serialize_entry("codeVerifier", &Value::Null)?;
                    }
                }
                "oauthState" => {
                    if let Some(value) = &self.oauth_state {
                        map.serialize_entry("oauthState", value)?;
                    } else if self.is_explicit_null("oauthState") {
                        map.serialize_entry("oauthState", &Value::Null)?;
                    }
                }
                "discovery" => {
                    // 原始 JSON 优先：TS 的浅拷贝保留未知的嵌套属性（以及无法解析成 typed 的值）。
                    if let Some(raw) = &self.discovery_raw
                        && discovery_matches_raw(self.discovery.as_ref(), raw)
                    {
                        map.serialize_entry("discovery", raw)?;
                        continue;
                    }
                    if let Some(value) = &self.discovery {
                        map.serialize_entry("discovery", value)?;
                    } else if let Some(raw) = &self.discovery_raw {
                        map.serialize_entry("discovery", raw)?;
                    } else if self.is_explicit_null("discovery") {
                        map.serialize_entry("discovery", &Value::Null)?;
                    }
                }
                // 未知键：`{...value}` 把它们原样带回来。
                other => {
                    if let Some(value) = self.extra.get(other) {
                        map.serialize_entry(other, value)?;
                    }
                }
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for McpOAuthState {
    /// `JSON.parse` 的逆过程：记录键序，未知键原样留下；显式 `null` 的已知键记录在
    /// [`null_keys`](McpOAuthState::null_keys)，`discovery` 的原始值记录在
    /// [`discovery_raw`](McpOAuthState::discovery_raw)；类型不符的已知字段按缺席处理。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let value = Value::deserialize(deserializer)?;
        let Value::Object(fields) = value else {
            return Err(D::Error::custom("McpOAuthState must be a JSON object"));
        };
        let mut state = McpOAuthState {
            key_order: fields.keys().cloned().collect(),
            ..McpOAuthState::default()
        };
        // 逐键分派：已知键解析成 typed 值（类型不符时按缺席），显式 null 只记录进
        // `null_keys`，未知键原样收进 `extra`（保留原相对顺序，写回时照抄）。
        for (key, value) in fields {
            match key.as_str() {
                // 非字符串按空串处理：它不会匹配任何规范化 URL，`own()` 会清空状态。
                "serverUrl" => {
                    if let Value::String(url) = value {
                        state.server_url = url;
                    }
                }
                "clientInformation" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.client_information = serde_json::from_value(value).ok().flatten();
                    }
                }
                "tokens" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.tokens = serde_json::from_value(value).ok().flatten();
                    }
                }
                "tokensExpireAt" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.tokens_expire_at = serde_json::from_value(value).ok().flatten();
                    }
                }
                "codeVerifier" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.code_verifier = serde_json::from_value(value).ok().flatten();
                    }
                }
                "oauthState" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.oauth_state = serde_json::from_value(value).ok().flatten();
                    }
                }
                "discovery" => {
                    if value.is_null() {
                        state.null_keys.push(key);
                    } else {
                        state.discovery = serde_json::from_value(value.clone()).ok();
                        state.discovery_raw = Some(value);
                    }
                }
                _ => {
                    state.extra.insert(key, value);
                }
            }
        }
        Ok(state)
    }
}

/// `discovery_raw` 是否仍然是当前 typed `discovery` 的投影（没有别的路径改过它）。
fn discovery_matches_raw(typed: Option<&OAuthDiscoveryState>, raw: &Value) -> bool {
    let parsed: Option<OAuthDiscoveryState> = serde_json::from_value(raw.clone()).ok();
    parsed.as_ref() == typed
}

/// JS 数字 → JSON 值（整数写在整数字面量里，`-0`/`NaN`/`Infinity` 按 `JSON.stringify` 处理）。
///
/// 与 `oauth::types` 里的同名私有辅助一致；`serde_json::Number` 无法保存 `1e+21` 这种 JS 文本，
/// 因此超出 ±2^53 的整数与浮点交给 `serde_json` 的 f64 写法。
fn js_number_value(value: f64) -> Value {
    if value.is_nan() || value.is_infinite() {
        return Value::Null;
    }
    if value == 0.0 {
        return Value::Number(serde_json::Number::from(0));
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        return Value::Number(serde_json::Number::from(value as i64));
    }
    Value::Number(
        serde_json::Number::from_f64(value).unwrap_or_else(|| serde_json::Number::from(0)),
    )
}

/// `McpOAuthStateStore`：一个服务器 URL 的状态存储。
///
/// TS 允许同步或异步；Rust 侧一律异步，失败按抛异常处理（`OAuthFailure`）。
/// 在 `McpOAuthProvider` 的 `update` 里发生的 `load`/`save` 失败会毒化该 provider 的写链
/// （§30.17）；而读路径 `load()` 上的 `store.load` 失败只返回错误，不毒化。
///
/// # 实现契约
///
/// - 实现应按服务器 URL 隔离状态（构造时绑定一个 URL，或按 URL 分键存放）；provider 仍会用
///   `serverUrl` 逐字比对，不匹配就当作空状态（避免共享/串号的存储把凭据泄漏给别的服务器）。
/// - 方法会在 [`McpOAuthProvider`] 的写锁内被 await：实现里不得回调同一个 provider，
///   否则会自锁。
/// - 在 `update` 内返回的错误会被原样保留并克隆给后续调用，因此错误消息应当稳定、可读。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpOAuthStateStore: Send + Sync {
    /// `load()`：读取状态；没有时 `None`。
    async fn load(&self) -> Result<Option<McpOAuthState>, OAuthFailure>;

    /// `save(state)`：整体替换状态。
    async fn save(&self, state: McpOAuthState) -> Result<(), OAuthFailure>;
}

/// `MemoryOAuthStateStore`：进程内的状态存储，`load`/`save` 都做 `structuredClone` 深拷贝。
#[derive(Debug, Default)]
pub struct MemoryOAuthStateStore {
    value: Mutex<Option<McpOAuthState>>,
}

impl MemoryOAuthStateStore {
    /// 空存储。
    pub fn new() -> Self {
        MemoryOAuthStateStore::default()
    }

    fn lock(&self) -> MutexGuard<'_, Option<McpOAuthState>> {
        // 中毒只可能来自持锁期间 panic；这里唯一的字段是一个可整体替换的 Option，没有
        // 需要跨字段维护的不变量，所以直接取回内部值继续用（JS 的单线程存储没有"中毒"概念）。
        self.value
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl McpOAuthStateStore for MemoryOAuthStateStore {
    async fn load(&self) -> Result<Option<McpOAuthState>, OAuthFailure> {
        // `structuredClone(this.value)`（`Clone` 是深拷贝）。
        Ok(self.lock().clone())
    }

    async fn save(&self, state: McpOAuthState) -> Result<(), OAuthFailure> {
        // `this.value = structuredClone(state)`。
        *self.lock() = Some(state);
        Ok(())
    }
}

/// `McpOAuthProviderOptions`：构造 [`McpOAuthProvider`] 的参数。
///
/// `clientMetadata` 是 `Omit<OAuthClientMetadata, "redirect_uris"> & { redirect_uris?: string[] }`，
/// Rust 侧用同一个 [`OAuthClientMetadata`]（`redirect_uris` 本就可选）。
pub struct McpOAuthProviderOptions {
    /// `serverUrl: string | URL`：会被规范化为 href。
    pub server_url: String,
    /// `redirectUrl: string | URL`：原样保存（`String(options.redirectUrl)`）。
    pub redirect_url: String,
    /// `clientMetadata`：其余字段按 TS 的默认值补齐。
    ///
    /// 补齐发生在 `McpOAuthProvider::new`：缺失或显式为 `null` 的键写入默认值，已有键保持
    /// 原位与原值；传入的对象本身不会被修改（`new` 先克隆 `fields()`）。
    pub client_metadata: OAuthClientMetadata,
    /// `clientMetadataDocument`：见 [`ClientMetadataDocumentFn`]。
    pub client_metadata_document: Option<ClientMetadataDocumentFn>,
    /// `clientId`：非空时构成不可变的配置客户端。
    pub client_id: Option<String>,
    /// `clientSecret`：与 `clientId` 一起构成配置客户端，并决定默认的认证方式。
    pub client_secret: Option<String>,
    /// `store`：缺省是新的 [`MemoryOAuthStateStore`]。
    pub store: Option<Arc<dyn McpOAuthStateStore>>,
    /// `onRedirect`：`redirectToAuthorization` 调它。
    pub on_redirect: OnRedirectFn,
}

impl McpOAuthProviderOptions {
    /// 必填的四个参数；其余字段用结构体字面量或 `with_*` 补。
    pub fn new(
        server_url: impl Into<String>,
        redirect_url: impl Into<String>,
        client_metadata: OAuthClientMetadata,
        on_redirect: OnRedirectFn,
    ) -> Self {
        McpOAuthProviderOptions {
            server_url: server_url.into(),
            redirect_url: redirect_url.into(),
            client_metadata,
            client_metadata_document: None,
            client_id: None,
            client_secret: None,
            store: None,
            on_redirect,
        }
    }

    /// 设置 `clientMetadataDocument`。
    pub fn with_client_metadata_document(mut self, document: ClientMetadataDocumentFn) -> Self {
        self.client_metadata_document = Some(document);
        self
    }

    /// 设置 `clientId`/`clientSecret`。
    ///
    /// 空串的 `clientId` 等于没配置（JS 假值，`new` 里会过滤掉）；空串的 `clientSecret`
    /// 同样被丢弃，于是默认认证方式是 `none` 而不是 `client_secret_post`。
    pub fn with_client(
        mut self,
        client_id: impl Into<String>,
        client_secret: Option<String>,
    ) -> Self {
        self.client_id = Some(client_id.into());
        self.client_secret = client_secret;
        self
    }

    /// 设置状态存储。
    pub fn with_store(mut self, store: Arc<dyn McpOAuthStateStore>) -> Self {
        self.store = Some(store);
        self
    }
}

/// 写链状态：`None` = 正常；`Some(error)` = 已毒化（§30.17）。
#[derive(Default)]
struct WriteChain {
    poisoned: Option<OAuthFailure>,
}

/// 某个 MCP 服务器 URL 的有状态 provider（`OAuthClientProvider` 的默认实现）。
///
/// 本类型只负责凭据状态的存取与服务器 URL 绑定，不做网络 IO；授权/刷新流程在
/// `oauth::flow` 里，通过 `OAuthClientProvider` trait 使用它。
///
/// TS: `oauth/provider.ts#McpOAuthProvider`。
pub struct McpOAuthProvider {
    redirect_url: String,
    // 构造时已补齐 TS 默认值；getter 原样返回，不再计算。
    client_metadata: OAuthClientMetadata,
    client_metadata_document: Option<ClientMetadataDocumentFn>,
    // 规范化 href：与持久化状态里的 `serverUrl` 逐字比较，是凭据隔离的依据。
    server_url: String,
    // 有值表示 `clientId` 非空：客户端不可变，`save_client_information` 是 no-op。
    configured_client: Option<OAuthClientInformationMixed>,
    // 永不为 `None`：缺省时在 `new` 里换成 `MemoryOAuthStateStore`。
    store: Arc<dyn McpOAuthStateStore>,
    on_redirect: OnRedirectFn,
    // 串行化 store 的读改写，并记录第一个错误实现"一次失败永久毒化"（§30.17）。
    writes: tokio::sync::Mutex<WriteChain>,
}

impl McpOAuthProvider {
    /// `new McpOAuthProvider(options)`。
    ///
    /// 构造过程不碰 store，只做三件纯计算：规范化 `serverUrl`、补齐 `clientMetadata` 的默认值、
    /// 按 `clientId`/`clientSecret` 生成 `configured_client`（空串按 JS 假值丢弃）。
    /// 失败只可能来自 `new URL(options.serverUrl)`（TS 抛 `TypeError("Invalid URL")`）。
    pub fn new(options: McpOAuthProviderOptions) -> Result<Self, OAuthFailure> {
        // `String(new URL(options.serverUrl))`：规范化 href。
        let server_url = js_url::parse(&options.server_url)
            .map_err(|()| OAuthFailure::type_error("Invalid URL"))?
            .to_string();
        // `String(options.redirectUrl)`：字符串原样。
        let redirect_url = options.redirect_url;
        let client_secret = options.client_secret.filter(|secret| !secret.is_empty());
        let client_metadata = normalize_client_metadata(
            &options.client_metadata,
            &redirect_url,
            client_secret.is_some(),
        );
        // `options.clientId ? { client_id, ...(clientSecret ? { client_secret } : {}) } : undefined`
        // —— 空串是 JS 假值，等同于没有配置客户端。
        let configured_client = options
            .client_id
            .filter(|client_id| !client_id.is_empty())
            .map(|client_id| {
                let mut fields = Map::new();
                fields.insert("client_id".to_owned(), Value::String(client_id));
                if let Some(client_secret) = client_secret {
                    fields.insert("client_secret".to_owned(), Value::String(client_secret));
                }
                OAuthClientInformationMixed::from(fields)
            });
        Ok(McpOAuthProvider {
            redirect_url,
            client_metadata,
            client_metadata_document: options.client_metadata_document,
            server_url,
            configured_client,
            store: options
                .store
                .unwrap_or_else(|| Arc::new(MemoryOAuthStateStore::new())),
            on_redirect: options.on_redirect,
            writes: tokio::sync::Mutex::new(WriteChain::default()),
        })
    }

    /// 本 provider 绑定的服务器 URL（规范化 href）。
    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    /// `readonly redirectUrl`。
    pub fn redirect_url(&self) -> &str {
        &self.redirect_url
    }

    /// `readonly clientMetadata`（默认值已补齐）。
    pub fn client_metadata(&self) -> &OAuthClientMetadata {
        &self.client_metadata
    }

    /// `state()`：已有 `oauthState` 就复用，否则生成 32 随机字节的 hex 并**先保存**。
    ///
    /// 并发调用没有去重（§30.18）：两次调用可能各自生成并写入，后写者覆盖先写者，先返回的
    /// 调用方手里就是一个已经失效的 state（TS 同样如此）。这是 [`OAuthClientProvider`] 的
    /// 原生版本，总是返回 `String`；trait 实现把它包成 `Some`。
    pub async fn state(&self) -> Result<String, OAuthFailure> {
        if let Some(existing) = self
            .load()
            .await?
            .oauth_state
            .filter(|state| !state.is_empty())
        {
            return Ok(existing);
        }
        let state = random_state()?;
        let saved = state.clone();
        self.update(move |mut value| {
            value.oauth_state = Some(saved);
            value.clear_explicit_null("oauthState");
            value
        })
        .await?;
        Ok(state)
    }

    /// `clientInformation()`：配置的客户端优先，不读存储。
    ///
    /// 配置了客户端时返回它的克隆：不取写锁、也不受写链毒化影响；未配置时才走 `load()`
    /// （因此受 URL 绑定与毒化约束）。
    pub async fn client_information(
        &self,
    ) -> Result<Option<OAuthClientInformationMixed>, OAuthFailure> {
        if let Some(client) = &self.configured_client {
            return Ok(Some(client.clone()));
        }
        Ok(self.load().await?.client_information)
    }

    /// `saveClientInformation(information)`：配置了客户端时 no-op。
    ///
    /// no-op 分支直接返回 `Ok(())`，不取锁也不碰存储，所以写链毒化后仍然成功；未配置时走
    /// `update`，失败会毒化写链。
    pub async fn save_client_information(
        &self,
        information: &OAuthClientInformationMixed,
    ) -> Result<(), OAuthFailure> {
        if self.configured_client.is_some() {
            return Ok(());
        }
        let information = information.clone();
        self.update(move |mut value| {
            value.client_information = Some(information);
            value.clear_explicit_null("clientInformation");
            value
        })
        .await
    }

    /// `tokens()`：读取已保存的令牌。
    ///
    /// 走 `load()`，因此受 URL 绑定与写链毒化约束。
    pub async fn tokens(&self) -> Result<Option<OAuthTokens>, OAuthFailure> {
        Ok(self.load().await?.tokens)
    }

    /// `saveTokens(tokens)`：同时写入/删除 `tokensExpireAt`。
    ///
    /// - `expires_in` 属性存在（包括显式 `null`）→ 写 `Date.now() + ToNumber(expires_in) * 1000`；
    ///   无法做数值转换时得到 `NaN`，序列化成 JSON `null`。
    /// - 属性缺失（`undefined`）→ 从状态里删除 `tokensExpireAt`（原有的显式 `null` 也一并删掉）。
    ///
    /// `tokens` 按值整体替换；本方法走 `update`，失败会毒化写链。
    pub async fn save_tokens(&self, tokens: &OAuthTokens) -> Result<(), OAuthFailure> {
        // TS 直接读属性并做 `tokens.expires_in * 1000`（JS `ToNumber`）：
        // `null`/`false` → 0、数字字符串/布尔按 `Number()`、无法转换 → `NaN`（JSON 写成 `null`）。
        // 只有属性**不存在**（`undefined`）才删除 `tokensExpireAt`。
        let expires_at = tokens
            .fields()
            .get("expires_in")
            .map(|expires_in| now_ms() + js_number(expires_in) * 1000.0);
        let tokens = tokens.clone();
        self.update(move |mut value| {
            value.tokens = Some(tokens);
            value.clear_explicit_null("tokens");
            // `delete next.tokensExpireAt` / `next.tokensExpireAt = expiresAt`：两种情况都清掉
            // 显式 null 标记（写入的 Some 本来就优先于标记）。
            value.tokens_expire_at = expires_at;
            value.clear_explicit_null("tokensExpireAt");
            value
        })
        .await
    }

    /// `redirectToAuthorization(url)`：交给 `onRedirect`。
    ///
    /// 回调在写锁之外 await，允许它执行任意异步工作（打开浏览器、记录 URL）；它的错误原样
    /// 返回，不改变持久化状态，也不会毒化写链。
    pub async fn redirect_to_authorization(&self, url: &Url) -> Result<(), OAuthFailure> {
        (self.on_redirect)(url.clone()).await
    }

    /// `saveCodeVerifier(verifier)`。
    ///
    /// 空串会原样写入存储，但随后的 `code_verifier()` 会把它当作缺失并报错（TS 的真值判断
    /// 同样把 `""` 视为没有 verifier）。走 `update`，失败会毒化写链。
    pub async fn save_code_verifier(&self, verifier: &str) -> Result<(), OAuthFailure> {
        let verifier = verifier.to_owned();
        self.update(move |mut value| {
            value.code_verifier = Some(verifier);
            value.clear_explicit_null("codeVerifier");
            value
        })
        .await
    }

    /// `codeVerifier()`：缺失或空串报 `No OAuth PKCE code verifier is stored`。
    pub async fn code_verifier(&self) -> Result<String, OAuthFailure> {
        self.load()
            .await?
            .code_verifier
            .filter(|verifier| !verifier.is_empty())
            .ok_or_else(|| OAuthFailure::error("No OAuth PKCE code verifier is stored"))
    }

    /// `invalidateCredentials(kind)`：按 [`CredentialKind`] 清除持久化字段。
    ///
    /// - `All`：clientInformation、tokens、tokensExpireAt、codeVerifier、discovery、oauthState；
    /// - `Client`：只清 clientInformation；
    /// - `Tokens`：清 tokens 与 tokensExpireAt；
    /// - `Verifier`：只清 codeVerifier；
    /// - `Discovery`：清 discovery（连同原始 JSON 一起丢弃）。
    ///
    /// `oauthState` 只在 `All` 时清除，`serverUrl` 永不删除。走 `update`，失败会毒化写链。
    pub async fn invalidate_credentials(&self, kind: CredentialKind) -> Result<(), OAuthFailure> {
        self.update(move |mut value| {
            if matches!(kind, CredentialKind::All | CredentialKind::Client) {
                value.client_information = None;
                value.clear_explicit_null("clientInformation");
            }
            if matches!(kind, CredentialKind::All | CredentialKind::Tokens) {
                value.tokens = None;
                value.tokens_expire_at = None;
                value.clear_explicit_null("tokens");
                value.clear_explicit_null("tokensExpireAt");
            }
            if matches!(kind, CredentialKind::All | CredentialKind::Verifier) {
                value.code_verifier = None;
                value.clear_explicit_null("codeVerifier");
            }
            if matches!(kind, CredentialKind::All | CredentialKind::Discovery) {
                value.discovery = None;
                value.discovery_raw = None;
                value.clear_explicit_null("discovery");
            }
            if matches!(kind, CredentialKind::All) {
                value.oauth_state = None;
                value.clear_explicit_null("oauthState");
            }
            value
        })
        .await
    }

    /// `saveDiscoveryState(discovery)`。
    ///
    /// 用新对象整体替换旧 discovery，同时丢弃旧 JSON 里的未知嵌套属性（TS 的
    /// `{...value, discovery}` 也是整体替换）。走 `update`，失败会毒化写链。
    pub async fn save_discovery_state(
        &self,
        discovery: &OAuthDiscoveryState,
    ) -> Result<(), OAuthFailure> {
        let discovery = discovery.clone();
        self.update(move |mut value| {
            value.discovery = Some(discovery);
            // `{...value, discovery}` 整体替换：原始 JSON 里未知的嵌套属性随旧对象消失。
            value.discovery_raw = None;
            value.clear_explicit_null("discovery");
            value
        })
        .await
    }

    /// `discoveryState()`：读取 typed 发现结果。
    ///
    /// 只返回能解析成 [`OAuthDiscoveryState`] 的部分；`discovery_raw` 里保留的未知嵌套属性
    /// 是序列化细节，从本方法看不到。走 `load()`，受 URL 绑定与毒化约束。
    pub async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>, OAuthFailure> {
        Ok(self.load().await?.discovery)
    }

    /// `load()`：等待写链（毒化则报同一个错误），返回属于本服务器的状态。
    ///
    /// `store.load()` 自身的失败原样返回但**不**写入毒化标记：TS 的 `load` 只是
    /// `await this.writes` 后调用 store，没有把结果 promise 写回链，下一次调用仍会重试 store。
    /// 锁 guard 在整个 `store.load()` 期间保持，因此同一 provider 的读也串行。
    async fn load(&self) -> Result<McpOAuthState, OAuthFailure> {
        let chain = self.writes.lock().await;
        if let Some(error) = &chain.poisoned {
            return Err(error.clone());
        }
        let state = self.store.load().await?;
        Ok(self.own(state))
    }

    /// `update(fn)`：串行化的 read-modify-write；任何错误都毒化写链（§30.17）。
    ///
    /// 顺序：取锁 → 检查毒化 → `store.load()` → `own()`（URL 不符就换成空状态）→ 应用 `fn`
    /// → `reconcile_key_order` 维护键序 → `store.save()`。`store.load`/`store.save` 任一失败
    /// 都会把错误存进 `poisoned`；此后所有 `update` 与 `load` 直接返回它的克隆，不再碰 store。
    async fn update<F>(&self, update: F) -> Result<(), OAuthFailure>
    where
        F: FnOnce(McpOAuthState) -> McpOAuthState,
    {
        let mut chain = self.writes.lock().await;
        if let Some(error) = &chain.poisoned {
            return Err(error.clone());
        }
        let result = async {
            // `{...value, key: …}`：先取修改前的状态（含键序），再按差异维护键序。
            let previous = self.own(self.store.load().await?);
            let mut next = update(previous.clone());
            reconcile_key_order(&previous, &mut next);
            self.store.save(next).await
        }
        .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                chain.poisoned = Some(error.clone());
                Err(error)
            }
        }
    }

    /// 只保留 `serverUrl` 与自身逐字相等的状态，否则返回 `{serverUrl}`。
    ///
    /// provider 侧的凭据隔离防线：store 若返回别的服务器的状态（共享或串号的存储），读路径
    /// 表现为"没有凭据"；写路径保存的是本服务器的状态（单槽存储会覆盖掉别的服务器的记录，
    /// 按 URL 分键的存储则不会）。
    ///
    /// TS: `state?.serverUrl === this.serverUrl ? state : { serverUrl: this.serverUrl }`。
    fn own(&self, state: Option<McpOAuthState>) -> McpOAuthState {
        match state {
            Some(state) if state.server_url == self.server_url => state,
            _ => McpOAuthState::new(self.server_url.clone()),
        }
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl OAuthClientProvider for McpOAuthProvider {
    fn redirect_url(&self) -> &str {
        &self.redirect_url
    }

    fn client_metadata(&self) -> &OAuthClientMetadata {
        &self.client_metadata
    }

    fn client_metadata_document(
        &self,
        metadata: Option<&AuthorizationServerMetadata>,
    ) -> Result<Option<OAuthClientMetadataDocument>, OAuthFailure> {
        match &self.client_metadata_document {
            Some(document) => document(metadata),
            None => Ok(None),
        }
    }

    async fn state(&self) -> Result<Option<String>, OAuthFailure> {
        McpOAuthProvider::state(self).await.map(Some)
    }

    async fn client_information(
        &self,
    ) -> Result<Option<OAuthClientInformationMixed>, OAuthFailure> {
        McpOAuthProvider::client_information(self).await
    }

    async fn save_client_information(
        &self,
        information: &OAuthClientInformationMixed,
    ) -> Result<(), OAuthFailure> {
        McpOAuthProvider::save_client_information(self, information).await
    }

    fn has_save_client_information(&self) -> bool {
        true
    }

    async fn tokens(&self) -> Result<Option<OAuthTokens>, OAuthFailure> {
        McpOAuthProvider::tokens(self).await
    }

    async fn save_tokens(&self, tokens: &OAuthTokens) -> Result<(), OAuthFailure> {
        McpOAuthProvider::save_tokens(self, tokens).await
    }

    async fn redirect_to_authorization(&self, url: &Url) -> Result<(), OAuthFailure> {
        McpOAuthProvider::redirect_to_authorization(self, url).await
    }

    async fn save_code_verifier(&self, verifier: &str) -> Result<(), OAuthFailure> {
        McpOAuthProvider::save_code_verifier(self, verifier).await
    }

    async fn code_verifier(&self) -> Result<String, OAuthFailure> {
        McpOAuthProvider::code_verifier(self).await
    }

    async fn invalidate_credentials(&self, kind: CredentialKind) -> Result<(), OAuthFailure> {
        McpOAuthProvider::invalidate_credentials(self, kind).await
    }

    async fn save_discovery_state(&self, state: &OAuthDiscoveryState) -> Result<(), OAuthFailure> {
        McpOAuthProvider::save_discovery_state(self, state).await
    }

    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>, OAuthFailure> {
        McpOAuthProvider::discovery_state(self).await
    }
}

impl std::fmt::Debug for McpOAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpOAuthProvider")
            .field("server_url", &self.server_url)
            .field("redirect_url", &self.redirect_url)
            .field("client_metadata", &self.client_metadata)
            .finish_non_exhaustive()
    }
}

/// `clientMetadata = {...options.clientMetadata, <默认值>}`：已存在的键保持原位，缺失的键追加。
fn normalize_client_metadata(
    metadata: &OAuthClientMetadata,
    redirect_url: &str,
    has_client_secret: bool,
) -> OAuthClientMetadata {
    let mut fields = metadata.fields().clone();
    apply_default(&mut fields, "redirect_uris", || {
        Value::Array(vec![Value::String(redirect_url.to_owned())])
    });
    apply_default(&mut fields, "grant_types", || {
        Value::Array(vec![
            Value::String("authorization_code".to_owned()),
            Value::String("refresh_token".to_owned()),
        ])
    });
    apply_default(&mut fields, "response_types", || {
        Value::Array(vec![Value::String("code".to_owned())])
    });
    // `?? (options.clientSecret ? "client_secret_post" : "none")`
    apply_default(&mut fields, "token_endpoint_auth_method", || {
        Value::String(
            if has_client_secret {
                "client_secret_post"
            } else {
                "none"
            }
            .to_owned(),
        )
    });
    OAuthClientMetadata::from_fields(fields)
}

/// `key in value && value[key] !== undefined && value[key] !== null` 时保留，否则写入默认值。
fn apply_default(fields: &mut Map<String, Value>, key: &str, make: impl FnOnce() -> Value) {
    match fields.get(key) {
        Some(value) if !value.is_null() => {}
        _ => {
            fields.insert(key.to_owned(), make());
        }
    }
}

/// `Date.now()`：纪元毫秒。
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `crypto.getRandomValues(new Uint8Array(32))` → 小写 hex（64 字符）。
///
/// 熵来自 `getrandom`（操作系统的 CSPRNG）；失败被包成 [`OAuthFailure::Error`]（消息以
/// `Failed to generate random bytes:` 开头），而不是 `TypeError`。
fn random_state() -> Result<String, OAuthFailure> {
    use std::fmt::Write as _;

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        OAuthFailure::error(format!("Failed to generate random bytes: {error}"))
    })?;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_redirect() -> OnRedirectFn {
        Arc::new(|_url| Box::pin(async { Ok(()) }))
    }

    fn options(
        server_url: &str,
        store: Option<Arc<dyn McpOAuthStateStore>>,
    ) -> McpOAuthProviderOptions {
        McpOAuthProviderOptions {
            server_url: server_url.to_owned(),
            redirect_url: "http://127.0.0.1/callback".to_owned(),
            client_metadata: OAuthClientMetadata::from_fields(
                json!({ "client_name": "test" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
            client_metadata_document: None,
            client_id: None,
            client_secret: None,
            store,
            on_redirect: no_redirect(),
        }
    }

    fn tokens(pairs: Value) -> OAuthTokens {
        OAuthTokens::from_fields(pairs.as_object().expect("object").clone())
    }

    #[test]
    fn normalizes_the_server_url_and_client_metadata() {
        let provider = McpOAuthProvider::new(options("https://EXAMPLE.example:443/mcp", None))
            .expect("provider");
        assert_eq!(provider.server_url(), "https://example.example/mcp");
        let metadata = provider.client_metadata();
        assert_eq!(
            metadata.to_value(),
            json!({
                "client_name": "test",
                "redirect_uris": ["http://127.0.0.1/callback"],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })
        );
        // 键序：原有键在前，默认值按书写顺序追加。
        assert_eq!(
            metadata.fields().keys().cloned().collect::<Vec<_>>(),
            vec![
                "client_name",
                "redirect_uris",
                "grant_types",
                "response_types",
                "token_endpoint_auth_method"
            ]
        );
    }

    #[test]
    fn client_secret_post_is_the_default_auth_method_with_a_secret() {
        let mut options = options("https://example.example/mcp", None);
        options.client_secret = Some("secret".to_owned());
        let provider = McpOAuthProvider::new(options).expect("provider");
        assert_eq!(
            provider.client_metadata().token_endpoint_auth_method(),
            Some("client_secret_post")
        );
    }

    #[test]
    fn an_invalid_server_url_is_a_type_error() {
        let error = McpOAuthProvider::new(options("not a url", None)).expect_err("invalid URL");
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.message(), "Invalid URL");
    }

    #[tokio::test]
    async fn explicit_client_metadata_fields_are_kept() {
        let mut options = options("https://example.example/mcp", None);
        options.client_metadata = OAuthClientMetadata::from_fields(
            json!({
                "redirect_uris": ["https://app.example/cb"],
                "grant_types": ["authorization_code"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "client_secret_basic",
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        let provider = McpOAuthProvider::new(options).expect("provider");
        assert_eq!(
            provider.client_metadata().to_value(),
            json!({
                "redirect_uris": ["https://app.example/cb"],
                "grant_types": ["authorization_code"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "client_secret_basic",
            })
        );
    }

    #[tokio::test]
    async fn the_state_is_generated_once_and_persisted() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let provider =
            McpOAuthProvider::new(options("https://example.example/mcp", Some(store.clone())))
                .expect("provider");
        let first = provider.state().await.expect("state");
        assert_eq!(first.len(), 64);
        assert!(
            first
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
        assert_eq!(provider.state().await.expect("state"), first);
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(stored.oauth_state.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn configured_clients_are_immutable() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let mut options = options("https://example.example/mcp", Some(store.clone()));
        options.client_id = Some("configured".to_owned());
        options.client_secret = Some("secret".to_owned());
        let provider = McpOAuthProvider::new(options).expect("provider");
        assert_eq!(
            provider
                .client_information()
                .await
                .expect("client")
                .map(|client| client.to_value()),
            Some(json!({ "client_id": "configured", "client_secret": "secret" }))
        );
        provider
            .save_client_information(&OAuthClientInformationMixed::from_fields(
                json!({ "client_id": "saved" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ))
            .await
            .expect("save");
        assert_eq!(
            provider
                .client_information()
                .await
                .expect("client")
                .map(|client| client.to_value()),
            Some(json!({ "client_id": "configured", "client_secret": "secret" }))
        );
        assert!(store.load().await.expect("load").is_none());
    }

    #[tokio::test]
    async fn credentials_are_bound_to_the_server_url() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let first = McpOAuthProvider::new(options("https://one.example/mcp", Some(store.clone())))
            .expect("provider");
        first
            .save_tokens(&tokens(
                json!({ "access_token": "secret", "token_type": "Bearer" }),
            ))
            .await
            .expect("save");
        assert_eq!(
            first
                .tokens()
                .await
                .expect("tokens")
                .and_then(|tokens| tokens.access_token().map(str::to_owned)),
            Some("secret".to_owned())
        );
        let second = McpOAuthProvider::new(options("https://two.example/mcp", Some(store.clone())))
            .expect("provider");
        assert!(second.tokens().await.expect("tokens").is_none());
        // 第二个 provider 的写不会把第一个的凭据带过去。
        assert_eq!(
            store.load().await.expect("load").expect("state").server_url,
            "https://one.example/mcp"
        );
    }

    /// 保存一次令牌并取回持久化的 `tokensExpireAt`。
    async fn save_and_read_expire_at(
        provider: &McpOAuthProvider,
        store: &MemoryOAuthStateStore,
        expires_in: Value,
    ) -> Option<f64> {
        provider
            .save_tokens(&tokens(json!({
                "access_token": "a",
                "token_type": "Bearer",
                "expires_in": expires_in,
            })))
            .await
            .expect("save");
        store
            .load()
            .await
            .expect("load")
            .expect("state")
            .tokens_expire_at
    }

    #[tokio::test]
    async fn tokens_expire_at_follows_js_expires_in_coercion() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let provider =
            McpOAuthProvider::new(options("https://example.example/mcp", Some(store.clone())))
                .expect("provider");

        // 数字：`Date.now() + expires_in * 1000`。
        let before = now_ms();
        let expire_at = save_and_read_expire_at(&provider, &store, json!(3600))
            .await
            .expect("tokensExpireAt");
        assert!(expire_at >= before + 3_600_000.0);
        assert!(expire_at <= now_ms() + 3_600_000.0);

        // `null` 走 `Number(null) === 0`，得到 `Date.now()`（TS 同样写数字，不删除键）。
        let before = now_ms();
        let expire_at = save_and_read_expire_at(&provider, &store, Value::Null)
            .await
            .expect("tokensExpireAt");
        assert!(expire_at >= before);
        assert!(expire_at <= now_ms());

        // 数字字符串 `Number("3600") === 3600`。
        let before = now_ms();
        let expire_at = save_and_read_expire_at(&provider, &store, json!("3600"))
            .await
            .expect("tokensExpireAt");
        assert!(expire_at >= before + 3_600_000.0);
        assert!(expire_at <= now_ms() + 3_600_000.0);

        // 布尔：`true` → 1 秒，`false` → 0。
        let before = now_ms();
        let expire_at = save_and_read_expire_at(&provider, &store, json!(true))
            .await
            .expect("tokensExpireAt");
        assert!(expire_at >= before + 1_000.0);
        assert!(expire_at <= now_ms() + 1_000.0);
        let before = now_ms();
        let expire_at = save_and_read_expire_at(&provider, &store, json!(false))
            .await
            .expect("tokensExpireAt");
        assert!(expire_at >= before);
        assert!(expire_at <= now_ms());

        // 无法转换 → `NaN`；`JSON.stringify` 把它写成 `null`。
        let expire_at = save_and_read_expire_at(&provider, &store, json!("abc"))
            .await
            .expect("tokensExpireAt");
        assert!(expire_at.is_nan());
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(
            serde_json::to_value(&stored).expect("serialize")["tokensExpireAt"],
            Value::Null
        );

        // 只有属性缺省（`undefined`）才删除 `tokensExpireAt`。
        provider
            .save_tokens(&tokens(
                json!({ "access_token": "b", "token_type": "Bearer" }),
            ))
            .await
            .expect("save");
        assert!(
            store
                .load()
                .await
                .expect("load")
                .expect("state")
                .tokens_expire_at
                .is_none()
        );
    }

    #[tokio::test]
    async fn invalidate_credentials_clears_the_expected_parts() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let provider =
            McpOAuthProvider::new(options("https://example.example/mcp", Some(store.clone())))
                .expect("provider");
        provider
            .save_client_information(&OAuthClientInformationMixed::from_fields(
                json!({ "client_id": "client" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ))
            .await
            .expect("save");
        provider
            .save_tokens(&tokens(
                json!({ "access_token": "a", "token_type": "Bearer", "expires_in": 3600 }),
            ))
            .await
            .expect("save");
        provider.save_code_verifier("verifier").await.expect("save");
        provider.state().await.expect("state");
        provider
            .save_discovery_state(&OAuthDiscoveryState {
                authorization_server_url: "https://example.example".to_owned(),
                ..OAuthDiscoveryState::default()
            })
            .await
            .expect("save");

        provider
            .invalidate_credentials(CredentialKind::Tokens)
            .await
            .expect("invalidate");
        let state = store.load().await.expect("load").expect("state");
        assert!(state.tokens.is_none());
        assert!(state.tokens_expire_at.is_none());
        assert!(state.client_information.is_some());
        assert!(state.code_verifier.is_some());
        assert!(state.discovery.is_some());
        assert!(state.oauth_state.is_some());

        provider
            .invalidate_credentials(CredentialKind::All)
            .await
            .expect("invalidate");
        let state = store.load().await.expect("load").expect("state");
        assert!(state.client_information.is_none());
        assert!(state.tokens.is_none());
        assert!(state.tokens_expire_at.is_none());
        assert!(state.code_verifier.is_none());
        assert!(state.discovery.is_none());
        assert!(state.oauth_state.is_none());
        assert_eq!(state.server_url, "https://example.example/mcp");
    }

    #[tokio::test]
    async fn a_missing_code_verifier_is_an_error() {
        let provider =
            McpOAuthProvider::new(options("https://example.example/mcp", None)).expect("provider");
        let error = provider
            .code_verifier()
            .await
            .expect_err("missing verifier");
        assert_eq!(error.name(), "Error");
        assert_eq!(error.message(), "No OAuth PKCE code verifier is stored");
    }

    #[tokio::test]
    async fn a_failed_write_poisons_the_chain() {
        /// 第一次 `save` 失败，之后本可成功——但链已毒化。
        struct FailingStore {
            saves: Mutex<u32>,
        }

        #[async_trait]
        impl McpOAuthStateStore for FailingStore {
            async fn load(&self) -> Result<Option<McpOAuthState>, OAuthFailure> {
                Ok(None)
            }

            async fn save(&self, _state: McpOAuthState) -> Result<(), OAuthFailure> {
                let mut saves = self.saves.lock().expect("lock");
                *saves += 1;
                if *saves == 1 {
                    return Err(OAuthFailure::error("disk full"));
                }
                Ok(())
            }
        }

        let store = Arc::new(FailingStore {
            saves: Mutex::new(0),
        });
        let provider = McpOAuthProvider::new(options(
            "https://example.example/mcp",
            Some(store.clone() as Arc<dyn McpOAuthStateStore>),
        ))
        .expect("provider");
        let error = provider
            .save_code_verifier("first")
            .await
            .expect_err("first save fails");
        assert_eq!(error.message(), "disk full");
        // 后续 update 与 load 都以同一个错误失败（TS 的 rejected promise 链）。
        let error = provider
            .save_code_verifier("second")
            .await
            .expect_err("chain poisoned");
        assert_eq!(error.message(), "disk full");
        let error = provider.tokens().await.expect_err("chain poisoned");
        assert_eq!(error.message(), "disk full");
        assert_eq!(*store.saves.lock().expect("lock"), 1);
    }

    #[tokio::test]
    async fn memory_store_deep_clones_on_load_and_save() {
        let store = MemoryOAuthStateStore::new();
        let mut state = McpOAuthState::new("https://example.example/mcp");
        state.oauth_state = Some("first".to_owned());
        store.save(state).await.expect("save");

        let mut loaded = store.load().await.expect("load").expect("state");
        loaded.oauth_state = Some("mutated".to_owned());
        // 修改返回值不影响存储（structuredClone）。
        assert_eq!(
            store
                .load()
                .await
                .expect("load")
                .expect("state")
                .oauth_state
                .as_deref(),
            Some("first")
        );
    }

    #[test]
    fn state_serializes_with_camel_case_and_omits_missing_fields() {
        let mut state = McpOAuthState::new("https://example.example/mcp");
        state.tokens_expire_at = Some(1_700_000_000_000.0);
        let value = serde_json::to_value(&state).expect("serialize");
        assert_eq!(
            value,
            json!({
                "serverUrl": "https://example.example/mcp",
                "tokensExpireAt": 1_700_000_000_000i64,
            })
        );
        // 整数样式：文本里不能出现 `.0`。
        let text = kiana_json::js_object::stringify(&value);
        assert_eq!(
            text,
            r#"{"serverUrl":"https://example.example/mcp","tokensExpireAt":1700000000000}"#
        );
        let round_trip: McpOAuthState = serde_json::from_value(value).expect("deserialize");
        assert_eq!(round_trip, state);
    }

    #[tokio::test]
    async fn unknown_state_fields_survive_read_modify_write() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        // TS 的 `{...value}` 会把 mcp-auth.json 里的未知键原样带进下一次写入。
        store
            .save(
                serde_json::from_value(json!({
                    "serverUrl": "https://example.example/mcp",
                    "extraField": { "a": 1 },
                    "note": "hi",
                }))
                .expect("state"),
            )
            .await
            .expect("save");
        let provider = McpOAuthProvider::new(options(
            "https://example.example/mcp",
            Some(store.clone() as Arc<dyn McpOAuthStateStore>),
        ))
        .expect("provider");

        provider.save_code_verifier("v").await.expect("save");
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(
            stored.extra,
            json!({ "extraField": { "a": 1 }, "note": "hi" })
                .as_object()
                .expect("object")
                .clone()
        );
        let value = serde_json::to_value(&stored).expect("serialize");
        assert_eq!(
            value,
            json!({
                "serverUrl": "https://example.example/mcp",
                "extraField": { "a": 1 },
                "note": "hi",
                "codeVerifier": "v",
            })
        );
        // 未知键保持原位，新键追加到末尾。
        assert_eq!(
            object_keys(&value),
            vec!["serverUrl", "extraField", "note", "codeVerifier"]
        );

        provider
            .save_tokens(&tokens(
                json!({ "access_token": "a", "token_type": "Bearer" }),
            ))
            .await
            .expect("save");
        let value = serde_json::to_value(store.load().await.expect("load").expect("state"))
            .expect("serialize");
        assert_eq!(
            object_keys(&value),
            vec!["serverUrl", "extraField", "note", "codeVerifier", "tokens"]
        );
    }

    #[tokio::test]
    async fn deleted_keys_are_appended_again_at_the_end() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        let provider = McpOAuthProvider::new(options(
            "https://example.example/mcp",
            Some(store.clone() as Arc<dyn McpOAuthStateStore>),
        ))
        .expect("provider");
        provider
            .save_client_information(&client(json!({ "client_id": "c" })))
            .await
            .expect("save");
        provider
            .save_tokens(&tokens(
                json!({ "access_token": "a", "token_type": "Bearer", "expires_in": 60 }),
            ))
            .await
            .expect("save");
        assert_eq!(
            object_keys(
                &serde_json::to_value(store.load().await.expect("load").expect("state"))
                    .expect("serialize")
            ),
            vec!["serverUrl", "clientInformation", "tokens", "tokensExpireAt"]
        );
        provider
            .invalidate_credentials(CredentialKind::Tokens)
            .await
            .expect("invalidate");
        provider.save_code_verifier("v").await.expect("save");
        provider
            .save_tokens(&tokens(
                json!({ "access_token": "b", "token_type": "Bearer", "expires_in": 60 }),
            ))
            .await
            .expect("save");
        // `delete` 之后再添加的键追加到末尾（TS 的对象插入历史）。
        assert_eq!(
            object_keys(
                &serde_json::to_value(store.load().await.expect("load").expect("state"))
                    .expect("serialize")
            ),
            vec![
                "serverUrl",
                "clientInformation",
                "codeVerifier",
                "tokens",
                "tokensExpireAt"
            ]
        );
    }

    /// `tests/fixtures/oauth_provider_state_golden.json`：TS 参照实现（Node 26）在同一操作序列
    /// 下的输出快照；随机 `oauthState` 与时刻相关的 `tokensExpireAt` 已归一化为占位符。
    const GOLDEN: &str = include_str!("../../tests/fixtures/oauth_provider_state_golden.json");

    /// 把持久化状态序列化成 JS 对象形状，随机/时刻值替换为占位符。
    fn normalized(state: &McpOAuthState) -> Value {
        let mut value = serde_json::to_value(state).expect("serialize");
        let fields = value.as_object_mut().expect("object");
        if fields.contains_key("oauthState") {
            fields.insert("oauthState".to_owned(), Value::String("<state>".to_owned()));
        }
        if fields.contains_key("tokensExpireAt") {
            fields.insert(
                "tokensExpireAt".to_owned(),
                Value::String("<tokensExpireAt>".to_owned()),
            );
        }
        value
    }

    /// 序列化后的键序（`Object.keys()` 顺序，TS 的插入历史）。
    fn object_keys(value: &Value) -> Vec<String> {
        value.as_object().expect("object").keys().cloned().collect()
    }

    /// 快照里的 `*Keys` 数组（生成时按 TS 的插入历史）。
    fn golden_keys(value: &Value) -> Vec<String> {
        value
            .as_array()
            .expect("keys")
            .iter()
            .map(|key| key.as_str().expect("key").to_owned())
            .collect()
    }

    fn client(pairs: Value) -> OAuthClientInformationMixed {
        OAuthClientInformationMixed::from_fields(pairs.as_object().expect("object").clone())
    }

    #[tokio::test]
    async fn matches_the_typescript_provider_golden() {
        let golden: Value = serde_json::from_str(GOLDEN).expect("golden fixture");

        let store = Arc::new(MemoryOAuthStateStore::new());
        let provider = McpOAuthProvider::new(McpOAuthProviderOptions {
            server_url: "https://EXAMPLE.example:443/mcp".to_owned(),
            redirect_url: "http://127.0.0.1:1234/callback".to_owned(),
            client_metadata: OAuthClientMetadata::from_fields(
                json!({ "client_name": "test" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
            client_metadata_document: None,
            client_id: None,
            client_secret: None,
            store: Some(store.clone() as Arc<dyn McpOAuthStateStore>),
            on_redirect: no_redirect(),
        })
        .expect("provider");

        assert_eq!(
            provider.server_url(),
            golden["serverUrl"].as_str().expect("serverUrl")
        );
        assert_eq!(
            provider.redirect_url(),
            golden["redirectUrl"].as_str().expect("redirectUrl")
        );
        assert_eq!(
            provider.client_metadata().to_value(),
            golden["clientMetadata"]
        );
        assert_eq!(
            json!(
                provider
                    .client_metadata()
                    .fields()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            ),
            golden["clientMetadataKeys"]
        );
        let error = provider
            .code_verifier()
            .await
            .expect_err("missing verifier");
        assert_eq!(
            error.message(),
            golden["codeVerifierError"].as_str().expect("message")
        );

        provider
            .save_client_information(&client(json!({ "client_id": "c", "client_secret": "s" })))
            .await
            .expect("save client");
        provider
            .save_code_verifier("verifier")
            .await
            .expect("save verifier");
        let state = provider.state().await.expect("state");
        assert_eq!(
            state.len() as u64,
            golden["stateLength"].as_u64().expect("stateLength")
        );
        assert_eq!(
            state
                .chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch)),
            golden["stateIsLowerHex"]
                .as_bool()
                .expect("stateIsLowerHex")
        );
        assert_eq!(provider.state().await.expect("state again"), state);
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(normalized(&stored), golden["afterClientAndVerifier"]);
        // 键序 = 插入历史（saveClientInformation → saveCodeVerifier → state）。
        assert_eq!(
            object_keys(&normalized(&stored)),
            object_keys(&golden["afterClientAndVerifier"])
        );

        provider
            .save_tokens(&tokens(json!({
                "access_token": "a",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .await
            .expect("save tokens");
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(
            object_keys(&serde_json::to_value(&stored).expect("serialize")),
            golden_keys(&golden["afterTokensKeys"])
        );
        assert!(stored.tokens_expire_at.expect("tokensExpireAt").fract() == 0.0);
        assert_eq!(normalized(&stored), golden["afterTokens"]);
        assert_eq!(
            provider
                .tokens()
                .await
                .expect("tokens")
                .map(|value| value.to_value()),
            Some(golden["afterTokens"]["tokens"].clone())
        );
        assert_eq!(
            provider
                .client_information()
                .await
                .expect("client")
                .map(|value| value.to_value()),
            Some(golden["afterTokens"]["clientInformation"].clone())
        );

        provider
            .save_discovery_state(&OAuthDiscoveryState {
                authorization_server_url: "https://example.example".to_owned(),
                ..OAuthDiscoveryState::default()
            })
            .await
            .expect("save discovery");
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(
            object_keys(&serde_json::to_value(&stored).expect("serialize")),
            golden_keys(&golden["afterDiscoveryKeys"])
        );

        provider
            .invalidate_credentials(CredentialKind::Tokens)
            .await
            .expect("invalidate tokens");
        let stored = store.load().await.expect("load").expect("state");
        assert_eq!(
            object_keys(&serde_json::to_value(&stored).expect("serialize")),
            golden_keys(&golden["afterInvalidateTokensKeys"])
        );
        assert_eq!(normalized(&stored), golden["afterInvalidateTokens"]);

        provider
            .invalidate_credentials(CredentialKind::All)
            .await
            .expect("invalidate all");
        let cleared = store.load().await.expect("load").expect("state");
        assert_eq!(normalized(&cleared), golden["afterInvalidateAll"]);
        // `all` 之后只剩 `serverUrl`（键序也只剩它）。
        assert_eq!(
            object_keys(&normalized(&cleared)),
            object_keys(&golden["afterInvalidateAll"])
        );

        // 配置的客户端不可被 store 覆盖。
        let configured = McpOAuthProvider::new(McpOAuthProviderOptions {
            server_url: "https://EXAMPLE.example:443/mcp".to_owned(),
            redirect_url: "http://127.0.0.1:1234/callback".to_owned(),
            client_metadata: OAuthClientMetadata::from_fields(
                json!({ "client_name": "test" })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
            client_metadata_document: None,
            client_id: Some("client".to_owned()),
            client_secret: Some("secret".to_owned()),
            store: Some(Arc::new(MemoryOAuthStateStore::new()) as Arc<dyn McpOAuthStateStore>),
            on_redirect: no_redirect(),
        })
        .expect("provider");
        assert_eq!(
            configured
                .client_information()
                .await
                .expect("client")
                .map(|value| value.to_value()),
            Some(golden["configuredClient"].clone())
        );
        configured
            .save_client_information(&client(json!({ "client_id": "other" })))
            .await
            .expect("save client");
        assert_eq!(
            configured
                .client_information()
                .await
                .expect("client")
                .map(|value| value.to_value()),
            Some(golden["configuredClientAfterSave"].clone())
        );

        // 只有 secret：默认 token_endpoint_auth_method 为 client_secret_post。
        let mut secret_only_options = options("https://example.example/mcp", None);
        secret_only_options.client_metadata = OAuthClientMetadata::from_fields(Map::new());
        secret_only_options.client_secret = Some("secret".to_owned());
        let secret_only = McpOAuthProvider::new(secret_only_options).expect("provider");
        assert_eq!(
            secret_only.client_metadata().to_value(),
            golden["secretOnlyMetadata"]
        );
        assert_eq!(
            json!(
                secret_only
                    .client_metadata()
                    .fields()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            ),
            golden["secretOnlyMetadataKeys"]
        );

        // 显式元数据原样保留。
        let mut explicit_options = options("https://example.example/mcp", None);
        explicit_options.client_metadata = OAuthClientMetadata::from_fields(
            json!({
                "redirect_uris": ["https://app.example/cb"],
                "grant_types": ["authorization_code"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "client_secret_basic",
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        let explicit = McpOAuthProvider::new(explicit_options).expect("provider");
        assert_eq!(
            explicit.client_metadata().to_value(),
            golden["explicitMetadata"]
        );
        assert_eq!(
            json!(
                explicit
                    .client_metadata()
                    .fields()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            ),
            golden["explicitMetadataKeys"]
        );

        // 非法 URL 抛 TypeError("Invalid URL")。
        let mut invalid_options = options("not a url", None);
        invalid_options.redirect_url = "x".to_owned();
        let error = McpOAuthProvider::new(invalid_options).expect_err("invalid URL");
        assert_eq!(
            error.name(),
            golden["invalidUrl"]["name"].as_str().expect("name")
        );
        assert_eq!(
            error.message(),
            golden["invalidUrl"]["message"].as_str().expect("message")
        );

        // 写链毒化：第一次 save 失败后，后续写入与读取都以同一错误失败。
        struct FailingStore {
            saves: Mutex<u32>,
        }

        #[async_trait]
        impl McpOAuthStateStore for FailingStore {
            async fn load(&self) -> Result<Option<McpOAuthState>, OAuthFailure> {
                Ok(None)
            }

            async fn save(&self, _state: McpOAuthState) -> Result<(), OAuthFailure> {
                let mut saves = self.saves.lock().expect("lock");
                *saves += 1;
                if *saves == 1 {
                    return Err(OAuthFailure::error("disk full"));
                }
                Ok(())
            }
        }

        let failing = Arc::new(FailingStore {
            saves: Mutex::new(0),
        });
        let mut poison_options = options(
            "https://example.example/mcp",
            Some(failing.clone() as Arc<dyn McpOAuthStateStore>),
        );
        poison_options.client_metadata = OAuthClientMetadata::from_fields(Map::new());
        let poisoned = McpOAuthProvider::new(poison_options).expect("provider");
        let mut messages = Vec::new();
        for index in 0..3 {
            messages.push(
                poisoned
                    .save_code_verifier(&format!("v{index}"))
                    .await
                    .expect_err("poisoned")
                    .message()
                    .to_owned(),
            );
        }
        messages.push(
            poisoned
                .tokens()
                .await
                .expect_err("poisoned")
                .message()
                .to_owned(),
        );
        assert_eq!(json!(messages), golden["poison"]);
        assert_eq!(
            Some(u64::from(*failing.saves.lock().expect("lock"))),
            golden["poisonSaves"].as_u64()
        );
    }

    /// TS 的 `{...value}` 保留已知键的显式 `null`（消费者用 `!== undefined` 判断"有值"）。
    #[tokio::test]
    async fn explicit_nulls_survive_read_modify_write() {
        let store = Arc::new(MemoryOAuthStateStore::new());
        store
            .save(
                serde_json::from_value(json!({
                    "serverUrl": "https://example.example/mcp",
                    "clientInformation": null,
                    "tokens": null,
                    "tokensExpireAt": null,
                    "oauthState": null,
                    "discovery": null,
                }))
                .expect("state"),
            )
            .await
            .expect("save");
        let provider = McpOAuthProvider::new(options(
            "https://example.example/mcp",
            Some(store.clone() as Arc<dyn McpOAuthStateStore>),
        ))
        .expect("provider");

        provider.save_code_verifier("v").await.expect("save");
        let stored = store.load().await.expect("load").expect("state");
        // `tokensExpireAt: null` 不会被当成 `undefined`：它是"有值且已过期"。
        assert!(stored.tokens_expire_at.is_none());
        assert!(stored.is_explicit_null("tokensExpireAt"));
        let value = serde_json::to_value(&stored).expect("serialize");
        assert_eq!(
            value,
            json!({
                "serverUrl": "https://example.example/mcp",
                "clientInformation": null,
                "tokens": null,
                "tokensExpireAt": null,
                "oauthState": null,
                "discovery": null,
                "codeVerifier": "v",
            })
        );
        // codeVerifier 覆盖原来的 null，键序不变。
        assert_eq!(
            object_keys(&value),
            vec![
                "serverUrl",
                "clientInformation",
                "tokens",
                "tokensExpireAt",
                "oauthState",
                "discovery",
                "codeVerifier",
            ]
        );

        // `saveTokens` 没有 `expires_in` → `delete next.tokensExpireAt`（显式 null 也删掉）。
        provider
            .save_tokens(&tokens(
                json!({ "access_token": "a", "token_type": "Bearer" }),
            ))
            .await
            .expect("save");
        let value = serde_json::to_value(store.load().await.expect("load").expect("state"))
            .expect("serialize");
        assert_eq!(value.get("tokensExpireAt"), None);
        assert_eq!(
            value["tokens"],
            json!({ "access_token": "a", "token_type": "Bearer" })
        );

        // `invalidateCredentials(All)` 删掉所有键（显式 null 也一并清掉）。
        provider
            .invalidate_credentials(CredentialKind::All)
            .await
            .expect("invalidate");
        let value = serde_json::to_value(store.load().await.expect("load").expect("state"))
            .expect("serialize");
        assert_eq!(value, json!({ "serverUrl": "https://example.example/mcp" }));
    }

    /// TS 的浅拷贝保留 `discovery` 里未知的嵌套属性；`update` 不能把它们丢掉。
    #[tokio::test]
    async fn unknown_nested_discovery_fields_survive_read_modify_write() {
        let discovery = json!({
            "authorizationServerUrl": "https://auth.example",
            "extraTop": 1,
            "resourceMetadataUrl": "https://auth.example/rm",
            "authorizationServerMetadata": {
                "issuer": "https://auth.example",
                "extraNested": true,
            },
        });
        let store = Arc::new(MemoryOAuthStateStore::new());
        store
            .save(
                serde_json::from_value(json!({
                    "serverUrl": "https://example.example/mcp",
                    "discovery": discovery.clone(),
                }))
                .expect("state"),
            )
            .await
            .expect("save");
        let provider = McpOAuthProvider::new(options(
            "https://example.example/mcp",
            Some(store.clone() as Arc<dyn McpOAuthStateStore>),
        ))
        .expect("provider");

        provider.save_code_verifier("v").await.expect("save");
        let stored = store.load().await.expect("load").expect("state");
        // typed 视图只认识已知字段……
        assert_eq!(
            stored
                .discovery
                .as_ref()
                .map(|discovery| discovery.authorization_server_url.as_str()),
            Some("https://auth.example")
        );
        // ……但写回的 JSON 与读入时逐字一致（含未知的顶层/嵌套键与键序）。
        let value = serde_json::to_value(&stored).expect("serialize");
        assert_eq!(value["discovery"], discovery);
        assert_eq!(
            object_keys(&value["discovery"]),
            vec![
                "authorizationServerUrl",
                "extraTop",
                "resourceMetadataUrl",
                "authorizationServerMetadata",
            ]
        );
        assert_eq!(
            value["discovery"]["authorizationServerMetadata"]["extraNested"],
            json!(true)
        );

        // `saveDiscoveryState` 在 TS 里用新对象整体替换：旧对象的未知属性消失。
        provider
            .save_discovery_state(&OAuthDiscoveryState {
                authorization_server_url: "https://auth.example".to_owned(),
                ..OAuthDiscoveryState::default()
            })
            .await
            .expect("save discovery");
        let value = serde_json::to_value(store.load().await.expect("load").expect("state"))
            .expect("serialize");
        assert_eq!(
            value["discovery"],
            json!({ "authorizationServerUrl": "https://auth.example" })
        );
    }
}
