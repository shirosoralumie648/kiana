//! core/http-dispatcher.ts：进程级 HTTP 客户端（代理、idle 超时、连接族选择）。
//!
//! TS: `packages/coding-agent/src/core/http-dispatcher.ts#configureHttpDispatcher`（113 行）。
//!
//! TS 用 undici 的 `EnvHttpProxyAgent` 替换全局 dispatcher；Rust 侧没有全局 dispatcher，
//! 改为**进程级共享 `reqwest::Client`**（`ARCHITECTURE.md` §8 第 29 条）：
//! 下游（`kiana-config`/`kiana-rpc-print`/本 crate 的 version-check 与 tools-manager）统一通过
//! [`shared_http_client`] 取同一个实例，代理与超时行为一致。
//!
//! ## 逐条映射
//!
//! | undici | reqwest |
//! |---|---|
//! | `bodyTimeout`/`headersTimeout = idle` | [`ClientBuilder::read_timeout`]（`0` → 不设；对应 disabled） |
//! | `allowH2: false` | [`ClientBuilder::http1_only`] |
//! | `connect` 的 undici 默认连接超时 `10e3` | [`ClientBuilder::connect_timeout`]（10s，覆盖 TCP/TLS/代理隧道） |
//! | `EnvHttpProxyAgent`（`http_proxy ?? HTTP_PROXY` + `NO_PROXY`） | [`Proxy::custom`] 动态匹配器，逐字复刻 undici 的 `#parseNoProxy`/`#shouldProxy` |
//! | `ProxyAgent` 对 `socks5:`/`socks:` 用 `Socks5ProxyAgent` | `reqwest` 的 `socks` feature；内部改写成 `socks5h:`（远端 DNS，见下） |
//! | `proxyTunnel: true` | **无对应物**：`reqwest` 对 http 目标走 absolute-form 直连代理，只有 https 目标才 CONNECT 隧道（见“已知差异”） |
//! | `connect.autoSelectFamilyAttemptTimeout: 2000` | **无对应物**：`reqwest 0.13.5` 不暴露 Happy Eyeballs 的每地址预算（`hyper-util` 有，`ClientBuilder` 没有） |
//! | `undici.fetch` 的默认请求头（`user-agent: undici`、`accept: */*`、`accept-language: *`、`sec-fetch-mode: cors`、`accept-encoding: gzip, deflate`） | [`ClientBuilder::default_headers`]（逐条逐字，见 `client_default_headers`） |
//! | `undici.setGlobalDispatcher` / `undici.install()` | 见 [`configure_http_dispatcher`] 替换进程级共享 client |
//!
//! ## 代理环境变量的读取语义（undici `EnvHttpProxyAgent`）
//!
//! - HTTP 代理：`http_proxy ?? HTTP_PROXY`（**小写优先**，空串算“存在”且为假 → 不再看大写，
//!   与 `??` + `if (HTTP_PROXY)` 的组合一致）。
//! - HTTPS 代理：`https_proxy ?? HTTPS_PROXY`；未设置时**回退到 HTTP 代理**
//!   （undici 把 `kHttpsProxyAgent` 指向 `kHttpProxyAgent`）。
//! - `NO_PROXY`/`no_proxy`（`no_proxy ?? NO_PROXY ?? ''`，**小写优先**）由 `NoProxyPolicy`
//!   逐字复刻 undici `EnvHttpProxyAgent#parseNoProxy`/`#shouldProxy`：按 `[,\s]`（JS `\s`）
//!   切分，支持 `host:port`、`[::1]:443`、`*.example.com`/`.example.com` 与子域后缀匹配、
//!   原始值等于 `*` 时永不代理。**不用** `reqwest::NoProxy`（hyper-util 语义不同：不支持
//!   端口、`*.`、空白分隔，却把 CIDR/IP 当作匹配项）。
//! - 没有任何代理变量时显式 [`ClientBuilder::no_proxy`]，关闭 reqwest 的 `system-proxy`
//!   自动探测，避免与显式装配重复。
//!
//! ## 已知差异
//!
//! - `proxyTunnel: true`（HTTP 源也走 `CONNECT` 隧道）无法用 `reqwest` 表达；http 目标会以
//!   absolute-form 请求行发给代理（`GET http://host/path HTTP/1.1`），https 目标仍走 CONNECT。
//!   代理服务器需要支持这种标准形式；行为差异记录在
//!   `crates/kiana-core/src/http_dispatcher.rs` 的文档与测试中。
//! - `autoSelectFamilyAttemptTimeout = 2000` 在 `reqwest 0.13.5` 不可观测（§2.12 要求的
//!   `happy_eyeballs_timeout` API 不存在），不移植；Node 默认值不受影响这一点在 Rust 侧无意义。
//! - 环境变量在**构造 client 时**读取一次（undici 的 `#noProxyChanged` 会在每次 dispatch 时
//!   重新解析 `no_proxy`）；运行期修改环境变量只在下次 [`configure_http_dispatcher`] 生效。
//! - undici 的 `clientFactory`/`factory`（每个 origin 一个 Client/Pool，HTTP/1 源也复用连接）
//!   由 reqwest 的连接池统一承担；`pool_idle_timeout` 保持 reqwest 默认。
//! - `undici.install()` 的“保持调用方自定义 fetch 不被覆盖”逻辑没有对应物：Rust 没有全局
//!   fetch 可替换，`shared_http_client()` 的调用方显式选用本客户端。
//! - SOCKS5：undici 的 `Socks5ProxyAgent` 把**目标主机名**原样发给代理（ATYP=DOMAIN，代理侧
//!   解析）；`reqwest` 用 `socks5h:` 表达同样的远端解析，因此 `socks5:`/`socks:` 在建 client
//!   时改写成 `socks5h:`。`socks4:`/`socks4a:`/`socks5h:` 与 undici 一样在配置期报
//!   `Invalid URL protocol` 错误（`socks5h` 是 undici 不接受的写法）。undici 首次用 SOCKS5 时
//!   会 `process.emitWarning` 一条 ExperimentalWarning；Rust 进程没有对应机制，不输出。
//! - 代理 URL 的解析/校验按 undici 的构造顺序：先 `new URL(uri)`（失败 → `Invalid URL`），
//!   再按协议判断（`http:`/`https:` 通过，`socks5:`/`socks:` 特判，其它 →
//!   `Invalid URL protocol: the URL must start with \`http:\` or \`https:\`.`）。注意 undici
//!   **不会**给缺少协议的代理值补 `http://`（`reqwest::IntoProxy` 会，本模块刻意不用）。
//! - 退化输入 `socks5://`（空主机）：Node 的 `new URL` 接受，`Socks5ProxyAgent` 在请求期才
//!   失败；`url` crate 在解析期就拒绝，因此 Rust 在配置期报 `Invalid URL`。两者最终都不可用，
//!   Rust 选择“失败关闭”（不会像漏配代理那样静默直连）。
//!
//! ## 执行流程与共享状态
//!
//! [`configure_http_dispatcher`] 先校验 `timeout_ms`，再读取一次环境变量构造新的
//! `reqwest::Client`（代理 URL 非法会在这一步返回 `HttpDispatcherError`），最后在写锁内
//! 把进程级槽位替换成新的 `Arc<Client>`。构造失败时槽位保持不变（旧 client 继续服务）；
//! 替换成功后，已经持有旧 `Arc` 的调用方仍用旧配置完成在途请求，之后的新调用才拿到新
//! client。
//!
//! [`shared_http_client`] 是懒加载读取：先持读锁克隆已有 `Arc`；槽位为空时在锁外构造默认
//! client，再用写锁 `get_or_insert` 发布。两个线程同时首次调用可能各构造一次，但只有先
//! 发布者被保留，另一个实例被丢弃（只多一次构造，不会产生两个可见实例）。读写锁中毒时
//! 通过 `into_inner` 恢复：槽位里只有 `Arc`，不存在需要回滚的半更新状态。
//!
//! `apply_http_proxy_settings` 只写**大写** `HTTP_PROXY`/`HTTPS_PROXY`，且只写缺失的键；
//! 而 `apply_proxy_env` **按协议分别读取**、小写优先（`http_proxy ?? HTTP_PROXY` 与
//! `https_proxy ?? HTTPS_PROXY`，后者读取结果为 `None` 时回退到 HTTP 代理）。因此若已有
//! 小写 `http_proxy`，写入的 `HTTP_PROXY` 不会被 http 分支采用；但小写 `https_proxy`
//! 未设置时，写入的 `HTTPS_PROXY` 仍会被 https 分支采用。反向情形同理：只有小写
//! `https_proxy` 时，写入的 `HTTP_PROXY` 对 http 请求仍然生效。这与 undici 的读取优先级
//! 一致（`EnvHttpProxyAgent` 同样是分协议、小写优先）。
//!
//! ## 阅读顺序
//!
//! 常量与 `parse_http_idle_timeout_ms`/`format_http_idle_timeout_ms` →
//! [`apply_http_proxy_settings`]、[`configure_http_dispatcher`]、[`shared_http_client`] →
//! `apply_proxy_env`/`NoProxyPolicy`（代理选择与直连规则）→ `build_client`（undici 默认头、
//! 连接超时）→ 测试（`crates/kiana-core/tests/http_dispatcher.rs`）。

use std::sync::{Arc, LazyLock, OnceLock, RwLock};
use std::time::Duration;

use reqwest::redirect::Policy;
use reqwest::{Client, ClientBuilder, Proxy, Url};

/// `DEFAULT_HTTP_IDLE_TIMEOUT_MS`（TS 逐字）。
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: i64 = 300_000;

/// `HTTP_IDLE_TIMEOUT_CHOICES` 的一项（TS 的 `{label, timeoutMs}`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpIdleTimeoutChoice {
    /// 菜单展示文案（逐字）。
    pub label: &'static str,
    /// 毫秒；`0` 表示 disabled。
    pub timeout_ms: i64,
}

/// `HTTP_IDLE_TIMEOUT_CHOICES`（5 项，顺序与 label/timeoutMs 逐字）。
pub const HTTP_IDLE_TIMEOUT_CHOICES: [HttpIdleTimeoutChoice; 5] = [
    HttpIdleTimeoutChoice {
        label: "30 sec",
        timeout_ms: 30_000,
    },
    HttpIdleTimeoutChoice {
        label: "1 min",
        timeout_ms: 60_000,
    },
    HttpIdleTimeoutChoice {
        label: "2 min",
        timeout_ms: 120_000,
    },
    HttpIdleTimeoutChoice {
        label: "5 min",
        timeout_ms: 300_000,
    },
    HttpIdleTimeoutChoice {
        label: "disabled",
        timeout_ms: 0,
    },
];

/// 配置失败。`Display` 逐字等于 TS 的 `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpDispatcherError {
    message: String,
}

impl HttpDispatcherError {
    /// 用消息构造（`Display` 原样输出）。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for HttpDispatcherError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HttpDispatcherError {}

/// Node `fetch` 的最大重定向次数（WHATWG Fetch：第 20 次重定向后报网络错误）。
const MAX_REDIRECTS: usize = 20;

/// `parseHttpIdleTimeoutMs(value)`。
///
/// 复刻 TS 的递归结构：字符串先 `trim()`，`disabled`（大小写不敏感）→ `0`，空串 → `None`，
/// 其余按 ECMAScript `Number(string)` 再走数字分支；数字要求有限且 `>= 0`，然后 `floor`；
/// 其它类型（bool/null/数组/对象）→ `None`。
///
/// 返回值是 JS `number`（`f64`）：`Math.floor` 对任意有限值都原样返回，`1e19` 之类的
/// 大值不会截断（早先返回 `i64` 时会在 2^63 处饱和，导致 `format_http_idle_timeout_ms`
/// 打印出与 TS 不同的秒数）。
pub fn parse_http_idle_timeout_ms(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::String(text) => {
            // `String.prototype.trim()` 的字符集（含 U+FEFF，不含 U+0085）。
            let trimmed = kiana_json::js_trim(text);
            if trimmed.to_lowercase() == "disabled" {
                return Some(0.0);
            }
            if trimmed.is_empty() {
                return None;
            }
            number_to_timeout(kiana_json::parse_js_number(trimmed))
        }
        serde_json::Value::Number(number) => number_to_timeout(number.as_f64()?),
        _ => None,
    }
}

/// `Math.floor` + `Number.isFinite` + `>= 0` 的 `number` 分支。
fn number_to_timeout(value: f64) -> Option<f64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.floor())
}

/// `formatHttpIdleTimeoutMs(ms)`。
///
/// 严格相等命中 [`HTTP_IDLE_TIMEOUT_CHOICES`] 时返回 label；否则 `` `${ms / 1000} sec` ``，
/// 数字按 JS `Number::toString` 格式化（`kiana_json::format_js_number`：整数不带 `.0`，
/// `1500` → `"1.5 sec"`）。入参是 JS `number`（`f64`），除法与格式化与 TS 同精度。
pub fn format_http_idle_timeout_ms(timeout_ms: f64) -> String {
    if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES
        .iter()
        .find(|choice| choice.timeout_ms as f64 == timeout_ms)
    {
        return choice.label.to_string();
    }
    let seconds = timeout_ms / 1000.0;
    format!("{} sec", kiana_json::format_js_number(seconds))
}

/// `applyHttpProxySettings(httpProxy)`：把 `httpProxy` 写进 `HTTP_PROXY`/`HTTPS_PROXY`，
/// **只在不存在的环境变量上写**（`??=`；空串算存在，不覆盖）。
///
/// `httpProxy` 先 `trim()`，空则直接返回。`None` 与全空白字符串都不修改任何环境变量。
///
/// 只写大写键。`apply_proxy_env` 按协议分别读取、小写优先：已有小写 `http_proxy` 时，写入的
/// `HTTP_PROXY` 不会被 http 分支采用；但小写 `https_proxy` 未设置时，写入的 `HTTPS_PROXY` 仍会
/// 被 https 分支采用（反之，只有小写 `https_proxy` 时，写入的 `HTTP_PROXY` 对 http 请求生效）。
/// 即小写键只遮蔽同协议的大写键（与 undici `EnvHttpProxyAgent` 的读取顺序一致）。
/// 写环境变量是进程级全局副作用，调用方必须保证没有并发的 `getenv`（见函数体的 SAFETY
/// 注释）。
pub fn apply_http_proxy_settings(http_proxy: Option<&str>) {
    let Some(http_proxy) = http_proxy.map(kiana_json::js_trim) else {
        return;
    };
    if http_proxy.is_empty() {
        return;
    }
    if std::env::var_os("HTTP_PROXY").is_none() {
        // SAFETY: edition 2024 把 `set_var` 标成 unsafe（并发 getenv/setenv 在 C 层是数据竞争）。
        // 本函数与 TS 的 `process.env.X ??= ...` 一样只在启动期调用一次（`kiana-coding-agent` 的
        // `main` 在创建任何网络任务之前），不存在并发访问；测试里由测试自身的锁串行化。
        unsafe { std::env::set_var("HTTP_PROXY", http_proxy) };
    }
    if std::env::var_os("HTTPS_PROXY").is_none() {
        // SAFETY: 同上。
        unsafe { std::env::set_var("HTTPS_PROXY", http_proxy) };
    }
}

/// `configureHttpDispatcher(timeoutMs)`：按 `timeoutMs` 重建进程级共享 client。
///
/// 失败（`parseHttpIdleTimeoutMs` 为 `undefined`）时 `Display` 逐字
/// `Invalid HTTP idle timeout: {value}`；代理 URL 非法时按 undici 构造期的两种文案报错
/// （`Invalid URL` / `Invalid URL protocol: the URL must start with \`http:\` or \`https:\`.`）。
///
/// 新 client 完全构造成功后才替换槽位：任何错误返回时旧 client 保持不变，已经持有
/// [`shared_http_client`] 返回的 `Arc` 的调用方不受影响。`0` 表示禁用 idle 超时
/// （不设 `read_timeout`），与 [`DEFAULT_HTTP_IDLE_TIMEOUT_MS`] 的 300s 不同。
pub fn configure_http_dispatcher(timeout_ms: i64) -> Result<(), HttpDispatcherError> {
    let normalized =
        parse_http_idle_timeout_ms(&serde_json::Value::from(timeout_ms)).ok_or_else(|| {
            HttpDispatcherError::new(format!("Invalid HTTP idle timeout: {timeout_ms}"))
        })?;
    // 0 = disabled：bodyTimeout/headersTimeout 为 0 时 undici 关闭超时。
    let read_timeout = (normalized > 0.0).then(|| Duration::from_millis(normalized as u64));
    let client = build_client(read_timeout, Policy::limited(MAX_REDIRECTS), true)?;
    let slot = shared_slot();
    let mut guard = slot
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(Arc::new(client));
    Ok(())
}

/// `shared_http_client()`：进程级共享 client（首次调用时惰性构造默认配置）。
///
/// 默认配置 = `DEFAULT_HTTP_IDLE_TIMEOUT_MS` 的 `read_timeout` + 环境代理 + HTTP/1。
/// [`configure_http_dispatcher`] 之后返回新实例；同一时刻只有一个实例，所有调用方拿到
/// 同一个 `Arc<reqwest::Client>`（§8 第 29 条）。
///
/// 槽位为空时在锁外构造默认 client；并发首次调用可能各自构造一次，`get_or_insert` 只保留
/// 先发布的实例，另一个被丢弃。
pub fn shared_http_client() -> Arc<Client> {
    let slot = shared_slot();
    {
        let guard = slot.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(client) = guard.as_ref() {
            return Arc::clone(client);
        }
    }
    let client = Arc::new(default_http_client());
    let mut guard = slot
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 构造在锁外完成；`get_or_insert` 保证“先发布者胜出”，并返回槽位里最终的实例。
    Arc::clone(guard.get_or_insert(client))
}

/// `redirect: "manual"` 请求（GitHub release 页面）专用的 client。
///
/// `reqwest` 的重定向策略只能在 `Client` 级配置，无法在 `RequestBuilder` 上覆盖；`tools_manager`
/// 因此持有一个**同代理/同 HTTP/1 配置**、但 `Policy::none()` 的 client（不跟随重定向，
/// 由调用方读取 `Location`）。它不设 `read_timeout`：`fetchWithRetry` 的 10s 总预算已覆盖
/// 该请求，而 undici 的 idle 超时默认 300s 远大于它。
///
/// 首次调用时构造并缓存；构造失败记录 warning 后返回 `fallback_client`（与默认 client
/// 相同的降级路径）。
pub fn redirect_manual_http_client() -> Client {
    static SLOT: LazyLock<Client> = LazyLock::new(|| {
        build_client(None, Policy::none(), true).unwrap_or_else(|error| {
            tracing::warn!("Failed to configure HTTP client: {error}");
            fallback_client()
        })
    });
    SLOT.clone()
}

/// 槽位为空时构造的默认 client：`DEFAULT_HTTP_IDLE_TIMEOUT_MS` 的 `read_timeout`、跟随
/// 最多 `MAX_REDIRECTS` 次重定向、HTTP/1、环境代理。构造失败时记录 warning 并退回
/// `fallback_client`。
fn default_http_client() -> Client {
    build_client(
        Some(Duration::from_millis(DEFAULT_HTTP_IDLE_TIMEOUT_MS as u64)),
        Policy::limited(MAX_REDIRECTS),
        true,
    )
    .unwrap_or_else(|error| {
        tracing::warn!("Failed to configure HTTP client: {error}");
        fallback_client()
    })
}

/// 理论上不可达的兜底（TLS 后端初始化失败时 `reqwest::Client::new()` 也会 panic；
/// 这里只是避免把构造错误升级成 `unwrap`）。
fn fallback_client() -> Client {
    reqwest::Client::new()
}

fn shared_slot() -> &'static RwLock<Option<Arc<Client>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<Client>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// undici `connect.js#buildConnector` 的默认连接超时（`timeout == null ? 10e3 : timeout`）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `new URL(uri)` 失败时 Node 的文案（undici `ProxyAgent` 构造期解析代理 URI）。
const INVALID_URL_MESSAGE: &str = "Invalid URL";

/// undici `parseURL`（`Client` 构造期）对非 http(s) 协议抛出的 `InvalidArgumentError` 文案。
const INVALID_URL_PROTOCOL_MESSAGE: &str =
    "Invalid URL protocol: the URL must start with `http:` or `https:`.";

/// `socks5:`/`socks:` 代理在 URL 未写端口时的默认端口（undici `Socks5ProxyAgent`：
/// `parseInt(port) || 1080`）。
const SOCKS_DEFAULT_PORT: u16 = 1080;

/// `undici.fetch` 的默认请求头（实测 undici 8.10.2 / Node 26；`configureHttpDispatcher` 调用
/// `undici.install()` 后 kiana 的全局 fetch 就是这一套）。
///
/// `getLatestVersion`/`getLatestKianaRelease` 会用自己的 `User-Agent` 覆盖；`downloadFile` 等
/// 不设头的请求则带着 `user-agent: undici`（HTTP 代理的 CONNECT 隧道不带这些头，与 undici 相同）。
fn client_default_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static("undici"),
    );
    headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("cors"),
    );
    // undici 只声明 gzip/deflate；`tower-http` 的 Decompression 只在请求头缺失时补
    // `gzip,deflate,br`，因此显式设置可固定请求头内容，解压仍按响应头工作。
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate"),
    );
    headers
}

/// 构造 client：`read_timeout`（`None` = 不设）、重定向策略、HTTP/1、环境代理。
///
/// 错误路径：`apply_proxy_env` 的代理 URL 校验失败原样返回；`reqwest` 的 `build()` 失败
/// （如 TLS 后端初始化）转换成 [`HttpDispatcherError`]，`Display` 为 `reqwest` 的错误文本。
fn build_client(
    read_timeout: Option<Duration>,
    redirect: Policy,
    http1_only: bool,
) -> Result<Client, HttpDispatcherError> {
    let mut builder = Client::builder()
        .redirect(redirect)
        // undici fetch 的默认请求头（UA/accept/accept-language/sec-fetch-mode/accept-encoding）。
        .default_headers(client_default_headers())
        // undici 的 `buildConnector` 默认每个连接 10s 超时（含 DNS 后的 TCP/TLS 与代理隧道）。
        // `read_timeout` 只约束已建立连接上的读写，不能替代它。
        .connect_timeout(CONNECT_TIMEOUT);
    if http1_only {
        builder = builder.http1_only();
    }
    if let Some(read_timeout) = read_timeout {
        builder = builder.read_timeout(read_timeout);
    }
    builder = apply_proxy_env(builder)?;
    builder
        .build()
        .map_err(|error| HttpDispatcherError::new(error.to_string()))
}

/// 按 undici `EnvHttpProxyAgent` 的优先级装配环境代理。
///
/// 只装**一个** [`Proxy::custom`] 匹配器：它按请求协议选择 HTTP/HTTPS 代理，并用
/// [`NoProxyPolicy`]（undici `#shouldProxy`）决定是否直连。`ClientBuilder::proxy` 会关闭
/// `system-proxy` 自动探测，因此不会出现 reqwest 自己再按环境变量装一次代理的情况。
///
/// 匹配器在每个请求上执行，但闭包捕获的是构造时解析好的代理 URL 与 `NO_PROXY` 快照，
/// 不会重新读取环境变量。
fn apply_proxy_env(builder: ClientBuilder) -> Result<ClientBuilder, HttpDispatcherError> {
    let http_proxy = env_proxy("http_proxy", "HTTP_PROXY");
    let https_proxy = env_proxy("https_proxy", "HTTPS_PROXY").or_else(|| http_proxy.clone());
    if http_proxy.is_none() && https_proxy.is_none() {
        // 显式关闭 system-proxy 自动探测，避免 `proxy()`/环境探测各装一次。
        return Ok(builder.no_proxy());
    }
    let http_proxy_url = http_proxy.as_deref().map(parse_proxy_url).transpose()?;
    let https_proxy_url = https_proxy.as_deref().map(parse_proxy_url).transpose()?;
    let no_proxy = NoProxyPolicy::from_env();
    Ok(builder.proxy(Proxy::custom(move |url| {
        // undici：协议是 https 时用 kHttpsProxyAgent，否则用 kHttpProxyAgent
        // （kHttpsProxyAgent 在 HTTPS_PROXY 缺失时回退到 HTTP 代理）。
        let proxy = match url.scheme() {
            "https" => https_proxy_url.as_ref(),
            "http" => http_proxy_url.as_ref(),
            _ => None,
        }?;
        let hostname = normalize_request_hostname(url.host_str()?);
        let port = url.port_or_known_default().unwrap_or(0);
        no_proxy
            .should_proxy(&hostname, port)
            .then(|| proxy.clone())
    })))
}

/// `process.env.<lower> ?? process.env.<upper>`：小写存在（含空串）即采用，空串随后被视为
/// “没有代理”（与 undici 的 `??` + `if (HTTP_PROXY)` 组合一致）。
fn env_proxy(lower: &str, upper: &str) -> Option<String> {
    std::env::var_os(lower)
        .or_else(|| std::env::var_os(upper))
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
}

/// `new URL(value)` + undici `ProxyAgent` 的协议校验。
///
/// undici 的构造顺序（`EnvHttpProxyAgent` → `ProxyAgent` → `clientFactory`）：
/// 1. `new URL(uri)`：URL 语法非法（如 `127.0.0.1:7890`、`http://`、端口越界）→
///    `TypeError("Invalid URL")`；
/// 2. `socks5:`/`socks:` → `Socks5ProxyAgent`（合法；本实现在内部改写成 `socks5h:`，见下）；
/// 3. 其余协议交给 `new Client(uri)` → `parseURL`：只有 `http:`/`https:` 通过，否则
///    `InvalidArgumentError("Invalid URL protocol: the URL must start with \`http:\` or \`https:\`.")`。
///
/// 与 `reqwest` 的差异：`reqwest::Proxy` 的 `IntoProxy` 会给缺协议的值补 `http://`；undici 不会，
/// 因此这里直接 `Url::parse`，不补协议。
///
/// SOCKS5 改写：undici 把目标主机名交给代理解析（ATYP=DOMAIN）；`reqwest` 的 `socks5:`
/// 是本地解析、`socks5h:` 才是代理端解析，故内部用 `socks5h:` 并在缺端口时补
/// [`SOCKS_DEFAULT_PORT`]（`reqwest` 不会像 `IntoProxy` 那样补默认端口）。
fn parse_proxy_url(value: &str) -> Result<Url, HttpDispatcherError> {
    let mut url = Url::parse(value).map_err(|_| HttpDispatcherError::new(INVALID_URL_MESSAGE))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        "socks5" | "socks" => {
            url.set_scheme("socks5h")
                .map_err(|_| HttpDispatcherError::new(INVALID_URL_MESSAGE))?;
            if url.port().is_none() {
                url.set_port(Some(SOCKS_DEFAULT_PORT))
                    .map_err(|_| HttpDispatcherError::new(INVALID_URL_MESSAGE))?;
            }
            Ok(url)
        }
        _ => Err(HttpDispatcherError::new(INVALID_URL_PROTOCOL_MESSAGE)),
    }
}

/// undici `EnvHttpProxyAgent` 的 `NO_PROXY` 解析结果（`#parseNoProxy`）。
struct NoProxyPolicy {
    /// `process.env.no_proxy ?? process.env.NO_PROXY ?? ''` 的原始值（`*` 判定用）。
    raw: String,
    entries: Vec<NoProxyEntry>,
}

/// `#parseNoProxy` 产生的一条 `{hostname, port}`（`port == 0` 表示未指定）。
struct NoProxyEntry {
    hostname: String,
    port: u64,
}

impl NoProxyPolicy {
    /// `process.env.no_proxy ?? process.env.NO_PROXY ?? ''`：**小写优先**（与 undici 一致）。
    fn from_env() -> Self {
        let raw = std::env::var_os("no_proxy")
            .or_else(|| std::env::var_os("NO_PROXY"))
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::parse(&raw)
    }

    /// 复刻 `noProxyValue.split(/[,\s]/)` 与每条目的解析/规范化。
    fn parse(raw: &str) -> Self {
        let entries = raw
            .split(|character: char| character == ',' || is_js_regex_whitespace(character))
            .filter(|entry| !entry.is_empty())
            .map(parse_no_proxy_entry)
            .collect();
        Self {
            raw: raw.to_string(),
            entries,
        }
    }

    /// undici `#shouldProxy(hostname, port)`：`true` 表示走代理。
    ///
    /// `hostname` 必须是 [`normalize_request_hostname`] 处理后的形式（去尾点、小写）。
    fn should_proxy(&self, hostname: &str, port: u16) -> bool {
        if self.entries.is_empty() {
            // NO_PROXY 未设置或为空 → 总是代理。
            return true;
        }
        if self.raw == "*" {
            // 原始值（未 trim）严格等于 `*` 时永不代理。
            return false;
        }
        for entry in &self.entries {
            if entry.port != 0 && entry.port != u64::from(port) {
                continue;
            }
            if hostname == entry.hostname {
                return false;
            }
            // `hostname.slice(-(entry.hostname.length + 1)) === '.' + entry.hostname`
            if hostname.len() > entry.hostname.len()
                && hostname.ends_with(&format!(".{}", entry.hostname))
            {
                return false;
            }
        }
        true
    }
}

/// JS 正则 `\s` 的字符集合（与 `char::is_whitespace` 不同：含 U+FEFF、不含 U+0085）。
fn is_js_regex_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | ' '
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// 解析 `#parseNoProxy` 的一条条目。
///
/// 依次尝试 `[v6]:port` 与 `host:port`（仅当恰好一个冒号），都失败时把整条（去掉成对的
/// 方括号）当作无端口主机名；因此裸 IPv6（多个冒号）不会被误拆成 `host:port`。
fn parse_no_proxy_entry(entry: &str) -> NoProxyEntry {
    let parsed = parse_bracketed_ipv6_with_port(entry).or_else(|| {
        let unbracketed = strip_brackets(entry);
        if unbracketed.matches(':').count() == 1 {
            parse_host_port(unbracketed)
        } else {
            None
        }
    });
    let (hostname, port) = match parsed {
        Some((hostname, port)) => (hostname, port),
        None => {
            let unbracketed = strip_brackets(entry);
            (unbracketed.to_string(), 0)
        }
    };
    NoProxyEntry {
        hostname: normalize_no_proxy_hostname(&hostname),
        port,
    }
}

/// `^\[(.+)\]:(\d+)$`（`(.+)` 贪婪：取最后一个 `]:数字` 结尾）。
fn parse_bracketed_ipv6_with_port(entry: &str) -> Option<(String, u64)> {
    if !entry.starts_with('[') {
        return None;
    }
    let close = entry.rfind("]:")?;
    let hostname = &entry[1..close];
    let port = &entry[close + 2..];
    if hostname.is_empty() || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((hostname.to_string(), parse_no_proxy_port(port)))
}

/// `replace(/^\[(.+)\]$/, '$1')`（内层至少 1 个字符）。
fn strip_brackets(entry: &str) -> &str {
    if entry.len() >= 2 && entry.starts_with('[') && entry.ends_with(']') {
        let inner = &entry[1..entry.len() - 1];
        if !inner.is_empty() {
            return inner;
        }
    }
    entry
}

/// `^(.+):(\d+)$`（调用方已确认只有一个 `:`）。
fn parse_host_port(value: &str) -> Option<(String, u64)> {
    let (hostname, port) = value.split_once(':')?;
    if hostname.is_empty() || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((hostname.to_string(), parse_no_proxy_port(port)))
}

/// `Number.parseInt(digits, 10)`：超出 `u64` 的值永远不可能等于端口（`u16`）。
fn parse_no_proxy_port(digits: &str) -> u64 {
    digits.parse::<u64>().unwrap_or(u64::MAX)
}

/// `hostname.replace(/^\*?\./, '').replace(/^(.+)\.$/, '$1').toLowerCase()`。
fn normalize_no_proxy_hostname(hostname: &str) -> String {
    let hostname = hostname
        .strip_prefix("*.")
        .or_else(|| hostname.strip_prefix('.'))
        .unwrap_or(hostname);
    let hostname = if hostname.len() > 1 && hostname.ends_with('.') {
        &hostname[..hostname.len() - 1]
    } else {
        hostname
    };
    hostname.to_lowercase()
}

/// undici `#getProxyAgentForUrl` 的 host 规范化：
/// `hostname.replace(/:\d*$/, '').replace(/^\[(.+)\]$/, '$1').toLowerCase()`，长度 > 1 时
/// 再去掉尾点。
///
/// `url::Url::host_str()` 对 IPv6 返回**带方括号**的形式（`[::1]`），必须像 undici 一样先剥掉，
/// 否则 `NO_PROXY=::1`/`[::1]` 永远匹配不上（端口由 `Url` 单独给出，无需正则去端口）。
fn normalize_request_hostname(hostname: &str) -> String {
    let hostname = hostname
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(hostname);
    let hostname = if hostname.len() > 1 && hostname.ends_with('.') {
        &hostname[..hostname.len() - 1]
    } else {
        hostname
    };
    hostname.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn should_proxy(raw: &str, hostname: &str, port: u16) -> bool {
        NoProxyPolicy::parse(raw).should_proxy(&normalize_request_hostname(hostname), port)
    }

    #[test]
    fn parses_no_proxy_entries_like_undici() {
        // 未设置/空串 → 总是代理。
        assert!(should_proxy("", "example.com", 443));
        assert!(should_proxy("   ", "example.com", 443));

        // 原始值严格等于 `*` → 永不代理；带空白的 `*` 只是普通条目。
        assert!(!should_proxy("*", "example.com", 443));
        assert!(should_proxy(" * ", "example.com", 443));

        // host:port：端口不匹配时继续代理；不写端口则匹配任意端口。
        assert!(!should_proxy("localhost:8080", "localhost", 8080));
        assert!(should_proxy("localhost:8080", "localhost", 8081));
        assert!(!should_proxy("localhost", "localhost", 1234));

        // 逗号/空白分隔（JS 的 `\s` 含 U+FEFF，不含 U+0085）。
        assert!(!should_proxy(
            "a.example b.example,\tc.example",
            "b.example",
            80
        ));
        assert!(!should_proxy("a.example\u{FEFF}b.example", "b.example", 80));
        // U+0085 不在 JS 的 `\s` 里：不会切分，只能作为一整个主机名匹配。
        assert!(should_proxy("a\u{0085}b", "a", 80));
        assert!(!should_proxy("a\u{0085}b", "a\u{0085}b", 80));

        // `*.domain` 与 `.domain` 都剥离前导通配，匹配自身与子域（后缀必须是点边界）。
        assert!(!should_proxy("*.example.com", "api.example.com", 443));
        assert!(!should_proxy("*.example.com", "example.com", 443));
        assert!(!should_proxy(".example.com", "deep.api.example.com", 443));
        assert!(should_proxy("example.com", "notexample.com", 443));
        assert!(should_proxy("example.com", "badexample.com", 443));

        // 尾点规范化（条目与请求 host 都去尾点）。
        assert!(!should_proxy("example.com.", "example.com", 443));
        assert!(!should_proxy("example.com", "example.com.", 443));

        // `[::1]:443` 与裸 IPv6（请求侧 `host_str()` 给带方括号的形式）。
        assert!(!should_proxy("[::1]:443", "::1", 443));
        assert!(should_proxy("[::1]:443", "::1", 80));
        assert!(!should_proxy("::1", "::1", 80));
        assert!(!should_proxy("::1", "[::1]", 80));
        assert!(!should_proxy("[::1]", "[::1]", 80));

        // undici 不支持 CIDR：`10.0.0.0/8` 只是主机名，不匹配 IP。
        assert!(should_proxy("10.0.0.0/8", "10.0.0.1", 80));
    }

    #[test]
    fn parses_no_proxy_entries_into_hostnames_and_ports() {
        let policy =
            NoProxyPolicy::parse("Example.COM:8080, [::1]:443, *.Foo.Example., 10.0.0.0/8");
        let entries: Vec<(&str, u64)> = policy
            .entries
            .iter()
            .map(|entry| (entry.hostname.as_str(), entry.port))
            .collect();
        assert_eq!(
            entries,
            vec![
                ("example.com", 8080),
                ("::1", 443),
                ("foo.example", 0),
                ("10.0.0.0/8", 0),
            ]
        );
    }

    #[test]
    fn proxy_urls_parse_and_validate_like_undici() {
        // http(s)：原样接受。
        assert_eq!(
            parse_proxy_url("http://127.0.0.1:7890")
                .expect("absolute")
                .as_str(),
            "http://127.0.0.1:7890/"
        );
        assert_eq!(
            parse_proxy_url("https://proxy.example:8443")
                .expect("https proxy")
                .as_str(),
            "https://proxy.example:8443/"
        );

        // `new URL()` 失败 → `Invalid URL`（undici 不给缺协议的值补 http://）。
        for value in [
            "127.0.0.1:7890",
            "http://",
            "://bad",
            "http://example.com:99999",
        ] {
            assert_eq!(
                parse_proxy_url(value).expect_err(value).to_string(),
                "Invalid URL",
                "value: {value}"
            );
        }

        // 语法合法但不是 http(s)/socks5 → undici `parseURL` 的协议错误。
        for value in [
            "localhost:3128",
            "proxy:8080",
            "ftp://example.com:21",
            "socks4://127.0.0.1:1080",
            "socks4a://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            assert_eq!(
                parse_proxy_url(value).expect_err(value).to_string(),
                "Invalid URL protocol: the URL must start with `http:` or `https:`.",
                "value: {value}"
            );
        }

        // socks5:/socks: → reqwest 的 socks5h（代理端 DNS），缺端口补 1080。
        assert_eq!(
            parse_proxy_url("socks5://127.0.0.1:1080")
                .expect("socks5")
                .as_str(),
            "socks5h://127.0.0.1:1080"
        );
        assert_eq!(
            parse_proxy_url("socks://proxy.example")
                .expect("socks")
                .as_str(),
            "socks5h://proxy.example:1080"
        );
        // 用户信息保留（SOCKS5 用户名/密码认证）。
        assert_eq!(
            parse_proxy_url("socks5://user:pass@127.0.0.1:1080")
                .expect("socks5 auth")
                .as_str(),
            "socks5h://user:pass@127.0.0.1:1080"
        );
    }
}
