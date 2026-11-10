//! `oauth/callback.ts`：回环回调服务器 [`OAuthCallbackServer`]（`spec/telemetry-mcp.md`
//! §18.6、§20.3、§27.4、§30.15、§30.16）。
//!
//! TS 用 `node:http` 的 `createServer` 绑定 `127.0.0.1:0`；Rust 侧用 `tokio::net::TcpListener` +
//! 手写 HTTP/1.1（`spec/telemetry-mcp.md` §21 允许 hyper 或等价实现）。服务语义逐字对齐：
//!
//! | 场景 | 状态码 | 纯文本 body |
//! |---|---|---|
//! | 成功 | 200 | `Authorization complete. You may close this window.` |
//! | 未知路径 | 404 | `Not found` |
//! | `state` 缺失/过期 | 400 | `Invalid or expired OAuth state` |
//! | 回调落在别的路径 | 400 | `Unexpected redirect URI` |
//! | `error` 参数 | 200 | `Authorization failed. You may close this window.` + 空行 + `error_description ?? error` |
//! | 缺 `code` | 400 | `Missing authorization code` |
//!
//! `content-type` 默认 `text/plain; charset=utf-8`；提供 `renderPage` 时是
//! `text/html; charset=utf-8` 并附加 `cache-control: no-store`。
//!
//! # 逐字复刻的边界
//!
//! - 请求行按 llhttp（Node 的 HTTP parser）的规则校验：方法 token 必须精确匹配 Node
//!   `NODE_METHODS` 里的大小写敏感列表；版本 token 必须是 `HTTP/0.9`、`HTTP/1.0`、
//!   `HTTP/1.1`、`HTTP/2.0` 之一（或省略）；目标只能是可打印 ASCII，且必须是 `/`、`*`
//!   开头的 origin-form 或 `字母 scheme://…` 的 absolute-form。任一项不符就回
//!   `400 Bad Request` + 空 body 并关闭连接。`CONNECT` 不调用 request handler，直接关闭连接
//!   （Node 的 server 没有 `connect` 监听器）。合法方法的请求体被忽略（§30.15；HEAD 只按
//!   HTTP 规则省略 body）。
//! - 请求行与请求头按 **latin1** 解码（Node 的 HTTP parser 语义），任意字节的请求都能得到
//!   回复，而不是被当成非法 UTF-8 静默断开。
//! - `redirectUrl = http://{redirectHost 含 ":" 时加 []}:{port}{path}`：已带方括号的
//!   `redirectHost` 会被双层包裹（§30.16，未处理的边界照抄）。
//! - `waitForCallback(state, path)` 的 `path` 只在**回调到达时**比较，不影响 `paths` 白名单
//!   （`extraPaths` 必须包含该 path，否则先 404）。
//! - 同一 state 的第二次回调得到 400（pending 已在第一次回调时删除）。
//! - `error` 参数为空串时按假值处理（等于没有 `error`）；`code` 为空串等于缺 code；
//!   `state` 为空串等于缺 state；`iss` 为空串时省略。
//! - `error_description` 存在但为空串时**仍**作为 description（`??` 只跳过 null），
//!   而纯文本 body 因 `details ?` 为假只输出 message。
//! - `close()` 先以 `OAuth callback server closed` 拒绝所有 pending，再关闭服务器并等待
//!   所有在途连接结束（Node 19+ 的 `server.close()` 语义：关闭一个字节都没收到的空闲连接，
//!   等已经收到首字节的连接把请求处理完——Node 额外的 `headersTimeout` 未复刻）。
//! - `timeoutMs` 按 Node `setTimeout` 的规则钳位：大于 2^31-1 时按 1ms 处理
//!   （`TimeoutOverflowWarning`），0 也至少 1ms。
//!
//! # 与 TS 的已知偏差
//!
//! - 请求 URL 解析失败（Node 会在 request 回调里抛 `TypeError` 崩溃）返回 404 `Not found`。
//! - 请求头只做了分帧需要的解析，没有完整复刻 llhttp 的头语法：缺冒号、obs-fold、非法头名字符
//!   在 Node 是 400 + 空 body，这里忽略该行继续；头块超过 Node 默认 16KB 时 Node 回 431，这里
//!   在 64KB 上限处静默断开。absolute-form 目标的 URL 扫描字符集同理（例如 `http://x\y`
//!   Node 回 400，这里会按 URL 继续解析）。
//! - `timeoutMs` 的负值/NaN/小数无法用 `Option<u64>` 表达（Node 的 `setTimeout` 把它们钳到
//!   1ms 附近）。
//! - 绑定失败的消息复刻 Node `net` 的文本（`listen EADDRINUSE: address already in use host:port`、
//!   `listen EACCES: …`、`getaddrinfo ENOTFOUND host`）；[`OAuthFailure`] 不携带 Node 的
//!   `code`/`errno`/`syscall` 属性。
//! - `waitForCallback` 的定时器用 `tokio::spawn`，因此必须在 Tokio 运行时内调用；
//!   `listen` 是 async，实际调用点都在运行时里。
//! - HTTP 分帧：Node 用 chunked + keep-alive（同一连接上的第二个请求会被继续处理）；
//!   Rust 侧统一发 `content-length` + `connection: close`（HEAD 只写头，不发 `content-length`），
//!   一个连接只服务一个请求。`spec/telemetry-mcp.md` §21 明确不要求 keep-alive；浏览器的
//!   重定向是独立连接，不影响可观察行为。随之而来的差异：Node 在响应后让连接进入
//!   keep-alive 空闲态，`close()` 要等到 `keepAliveTimeout`（默认 5s）才真正返回；
//!   Rust 写完响应就关闭连接，`close()` 立即返回。
//! - 响应没有 Node 自动添加的 `Date`/`Keep-Alive` 头。
//! - `host` 为空串时 Node 绑定通配地址（`::`/`0.0.0.0`），这里主机解析失败并报
//!   `getaddrinfo ENOTFOUND`（coding-agent 的 host 总是取自 URL，不会为空）。
//!
//! # 本文件的结构与调用方
//!
//! 生产调用方是 `kiana-builtin-extensions` 的 MCP 登录流程（`src/mcp/oauth.rs` 的
//! `sign_in_mcp_server`）：它先 `listen`，把 [`OAuthCallbackServer::redirect_url`] 作为
//! `redirect_uri` 交给授权服务器，再用 [`OAuthCallbackServer::wait_for_callback`] 等浏览器回调，
//! 最后无条件调用 [`OAuthCallbackServer::close`]（对应 TS 的 `finally`）。本 crate 的
//! `tests/oauth.rs` 也直接驱动同一个对象。
//!
//! 启动后同一个运行时里有三类任务，它们通过 `Arc<CallbackInner>`（共享状态）、`shutdown`
//! 取消令牌和 oneshot 通道交互。`shutdown` 是 accept 任务与各连接任务的取消信号：accept
//! 循环在它取消后退出，连接任务把它交给 `read_request_head`，据此放弃尚未读到任何字节的
//! 空闲连接（`close()`/`Drop` 负责取消）：
//!
//! 1. **accept 任务**：独占 `TcpListener`，每接受一个连接就 `spawn` 一个连接任务并登记到
//!    `ConnectionTracker`；`shutdown` 令牌被取消后退出。
//! 2. **连接任务**：一个连接一个任务，读请求头 → `handle` → 写响应 → 关闭写半端。
//! 3. **定时器任务**：每次 `wait_for_callback` 注册一个，超时或被取消时结束。
//!
//! # 等待状态机（`pending` 表）
//!
//! `pending: Mutex<HashMap<String, PendingCallback>>` 以 `state` 为键，保存“哪个登录在等哪个
//! state”。锁只在查找/插入/删除时短暂持有，不跨 `.await`；锁中毒时用 `PoisonError::into_inner`
//! 取回内部数据，效果是某个任务 panic 后，其他任务不会在 `lock()` 上继续 panic。
//!
//! 一个 state 的生命周期：
//!
//! 1. `wait_for_callback` 插入条目并启动定时器；同一 state 重复注册立即失败（同步 `Err`）。
//! 2. 匹配的请求到达时 `handle` **先删除条目、再校验其余参数**：错误路径、`error` 参数、缺
//!    `code` 都会消耗 state，之后同一 state 再来只会得到 400 `Invalid or expired OAuth state`。
//! 3. 定时器到期删除条目，并以 `OAuth callback timed out` 结束等待。
//! 4. `close()` 清空整张表，以 `OAuth callback server closed` 结束所有等待。
//!
//! # 关闭与取消
//!
//! `close()` 的顺序是：清空 pending → 取消 `shutdown`（让 accept 退出）→ `await` accept 任务 →
//! `await` 全部连接任务。先清空 pending 保证 drain 之后原先已注册的登录不会再被回调命中；
//! 之后进入 `handle` 的已知路径请求因 state 缺失得到 400，未知路径仍 404，还没读到首字节的
//! 空闲连接则被直接关闭（不写响应）；close 期间新注册的 state 仍可能被已接受的连接命中并成功。
//! `Drop` 是兜底路径：`abort` accept 与连接任务并清空 pending（sender 随之丢弃，等待方得到
//! `OAuth callback server closed`），但不写任何响应、也不等待。
//!
//! 与同时到期的定时器存在竞争：谁先从表中移除条目，谁决定该 state 的失败文案（`OAuth callback
//! timed out` 或 `OAuth callback server closed`）；两种结果都允许，等待方只会收到其中一个。
//!
//! # 注意事项（修改前）
//!
//! - 丢弃 [`OAuthCallbackServer::wait_for_callback`] 返回的 future **不会**注销 state：pending
//!   条目与定时器任务都会留到超时或 `close()`。
//! - `close()` 之后仍可调用 `wait_for_callback`：代码里没有“服务器已关闭”的检查，注册会成功。
//!   若之后不再调用 `close()`/`Drop`，这次等待只能等超时；再次 `close()` 会在返回 `Err`
//!   （accept 任务已被取走）之前先 drain，并以 `OAuth callback server closed` 拒绝该等待；
//!   `Drop` 同样丢弃 sender，让等待方立即得到同一个错误。
//! - `renderPage` 在连接任务里同步执行，不能 `.await`；它返回的字符串原样写进 body（不转义）。
//!   它阻塞多久，`close()` 就要等多久（`close` 会 `await` 连接任务）。
//! - `timeoutMs` 在 `listen` 时固定，之后不可改；每次 `wait_for_callback` 都新建一个定时器，
//!   并按 Node 规则钳位（见 `node_timeout_ms`）。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::oauth::discovery::js_url;
use crate::oauth::errors::OAuthFailure;

/// `OAuthCallback`：授权响应里的 `code`/`state`/`iss`。
///
/// 由 [`OAuthCallbackServer::wait_for_callback`] 在回调成功时返回；字段全部来自回调 URL 的
/// query（`state` 与注册时使用的键相同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCallback {
    /// `code`：授权码。
    pub code: String,
    /// `state`：`waitForCallback` 注册的 state。
    pub state: String,
    /// `iss`：RFC 9207 的签发者（回调没带或为空串时 `None`）。
    pub iss: Option<String>,
}

/// `OAuthCallbackPage`：重定向之后展示在浏览器页上的结果。
///
/// TS 是 `{ok: true} | {ok: false; message; details?}`。
///
/// 只用于渲染页面：由 `handle` 构造后交给 `renderPage` 或 `plain_text`，不参与授权逻辑
/// （等待方拿到的是 [`OAuthCallback`] 或错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthCallbackPage {
    /// `{ ok: true }`。
    Success,
    /// `{ ok: false, message, details? }`。
    Failure {
        /// `message`：页面主文案（逐字）。
        message: String,
        /// `details`：附加说明（例如 `error_description`）。
        ///
        /// 该值为空串时 [`OAuthCallbackPage::details`] 仍返回 `Some("")`，只有纯文本输出
        /// （`plain_text`）会把空串当作“没有 details”而只打印 message。
        details: Option<String>,
    },
}

impl OAuthCallbackPage {
    /// `page.ok`。
    pub fn is_ok(&self) -> bool {
        matches!(self, OAuthCallbackPage::Success)
    }

    /// `page.ok ? undefined : page.message`。
    pub fn message(&self) -> Option<&str> {
        match self {
            OAuthCallbackPage::Success => None,
            OAuthCallbackPage::Failure { message, .. } => Some(message),
        }
    }

    /// `page.ok ? undefined : page.details`。
    ///
    /// 与 `plain_text` 不同，空串仍返回 `Some("")`（这里没有 `details ?` 的假值判断）。
    pub fn details(&self) -> Option<&str> {
        match self {
            OAuthCallbackPage::Success => None,
            OAuthCallbackPage::Failure { details, .. } => details.as_deref(),
        }
    }
}

/// `renderPage(page)`：把结果渲染成 HTML。
///
/// 交给 handler 的每个响应都会经过它（成功页、404 `Not found`、400 的三种失败页），返回值原样
/// 作为 body；状态码与 `content-type: text/html; charset=utf-8`、`cache-control: no-store`
/// 由服务器决定。请求行解析失败（llhttp 那类 400）不经过它，直接回 Node 风格的空 body。
///
/// 回调在连接任务里同步执行，不能 `.await`，也不应长时间阻塞：`close()` 要等连接任务结束。
/// 需要 `Send + Sync` 是因为它放在 `Arc<CallbackInner>` 里被多个任务共享。
pub type RenderPageFn = Arc<dyn Fn(&OAuthCallbackPage) -> String + Send + Sync>;

/// `OAuthCallbackServerOptions`：监听参数。
///
/// 除 `extra_paths`（`Vec<String>`，默认为空）外其余字段都是 `Option`；所有字段都有默认值，
/// 可用 `Default`/`..Default::default()` 省略。用 `Default` 构造后 `listen`，等于监听
/// `127.0.0.1:0`、路径 `/callback`、5 分钟超时、纯文本页面。`listen` 只读取一次，启动后再改
/// 这些值不会影响已运行的服务器。
#[derive(Clone, Default)]
pub struct OAuthCallbackServerOptions {
    /// `host`：监听地址，默认 `127.0.0.1`。
    pub host: Option<String>,
    /// `redirectHost`：`redirectUrl` 里的主机名（例如监听 `127.0.0.1` 但注册的是 `localhost`）。
    /// 默认等于 `host`。
    pub redirect_host: Option<String>,
    /// `port`：默认 0（系统分配）；`Some(0)` 与 `None` 等价。
    pub port: Option<u16>,
    /// `path`：默认 `/callback`。原样拼进 `redirectUrl`，并作为白名单的第一项与
    /// `url.path()` 精确比较（不做前缀或大小写匹配）。
    pub path: Option<String>,
    /// `extraPaths`：额外接收回调的路径（例如服务器专属的 redirect URI）。
    ///
    /// 白名单 = `path` + `extra_paths`，逐项与 `url.path()` 精确比较。
    pub extra_paths: Vec<String>,
    /// `timeoutMs`：`waitForCallback` 的超时，默认 5 分钟。大于 2^31-1 时按 1ms 处理
    /// （Node `setTimeout` 的溢出行为，见 `node_timeout_ms`）。
    pub timeout_ms: Option<u64>,
    /// `renderPage`：把结果渲染成 HTML；缺省输出纯文本。见 [`RenderPageFn`]。
    pub render_page: Option<RenderPageFn>,
}

impl std::fmt::Debug for OAuthCallbackServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCallbackServerOptions")
            .field("host", &self.host)
            .field("redirect_host", &self.redirect_host)
            .field("port", &self.port)
            .field("path", &self.path)
            .field("extra_paths", &self.extra_paths)
            .field("timeout_ms", &self.timeout_ms)
            .field("render_page", &self.render_page.is_some())
            .finish()
    }
}

/// 一个等待中的回调。
///
/// 条目被 `handle`（回调到达）、定时器任务（超时）或 `close()` 移除；**谁移除谁负责发送**
/// 结果，因此 `sender` 至多成功发送一次。
struct PendingCallback {
    /// 把结果交给 `wait_for_callback` 返回的 future（接收端可能已被丢弃，发送失败被忽略）。
    sender: oneshot::Sender<Result<OAuthCallback, OAuthFailure>>,
    /// `waitForCallback` 的期望路径；`None` 表示回调到达时不校验路径。
    path: Option<String>,
    /// 取消后对应的定时器任务立即结束；回调/`close` 移除条目时调用。
    timer: CancellationToken,
}

/// 请求处理需要的共享状态（HTTP 服务器任务与 [`OAuthCallbackServer`] 共用）。
struct CallbackInner {
    /// `redirectUrl` 的原始字符串，供 [`OAuthCallbackServer::redirect_url`] 返回。
    redirect_url: String,
    /// `new URL(rawUrl, redirectUrl)` 的 base。
    base: Url,
    /// 路径白名单：`path` 选项 + `extra_paths`，与 `Url::path()` 精确比较。
    paths: Vec<String>,
    /// 自定义页面渲染；`None` 时用 `plain_text` 输出纯文本。
    render_page: Option<RenderPageFn>,
    /// state → 等待中的回调；加锁方式见模块文档的「等待状态机」。
    pending: Mutex<HashMap<String, PendingCallback>>,
}

impl CallbackInner {
    /// 取 pending 表的锁；忽略锁中毒（某个任务 panic 后其他任务仍能继续服务）。
    fn lock_pending(&self) -> MutexGuard<'_, HashMap<String, PendingCallback>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 正在处理的连接（`close()` 要等它们结束，Node 19+ 的 `server.close()` 语义）。
///
/// 句柄只在 `spawn` 时顺手清理已结束的，所以表里可能留着早已完成的任务；`close()` 对它们
/// `await` 会立即返回。`Drop` 对全部句柄 `abort`（对已结束的无害）。
#[derive(Default)]
struct ConnectionTracker {
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ConnectionTracker {
    fn lock(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 启动一个连接任务并登记；顺手清理已结束的句柄。
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(future);
        let mut tasks = self.lock();
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
    }
}

/// 回环 OAuth 回调服务器（`oauth/callback.ts#OAuthCallbackServer`）。
///
/// 生命周期：`listen` 绑定并启动 accept 任务 → [`OAuthCallbackServer::wait_for_callback`] 注册
/// 期待的 state → 授权完成后 [`OAuthCallbackServer::close`]（或让对象 `Drop`）。所有方法都取
/// `&self`，可变状态放在 `Mutex` 与取消令牌里，共享状态用 `Arc` 交给后台任务，因此可以从多个
/// 任务同时调用。
///
/// `close()` 归还的是“监听任务”而不是对象本身：对象仍然可用，第二次 `close()` 返回 `Err`。
pub struct OAuthCallbackServer {
    /// 与 accept / 连接 / 定时器任务共享的配置与 pending 表。
    inner: Arc<CallbackInner>,
    /// `waitForCallback` 的超时原值；每次注册时再按 Node 规则钳位。
    timeout_ms: u64,
    /// 取消后 accept 循环退出、空闲连接被关闭；`close` 与 `Drop` 都会取消它。
    shutdown: CancellationToken,
    /// accept 任务句柄：`close` 取走并等待，`Drop` 取走并 `abort`。
    accept_task: Mutex<Option<JoinHandle<()>>>,
    /// 已登记连接任务的句柄，`close` 逐个等待、`Drop` 逐个 `abort`。
    connections: Arc<ConnectionTracker>,
}

impl OAuthCallbackServer {
    /// `OAuthCallbackServer.listen(options)`：绑定 `host:port` 并开始接收回调。
    ///
    /// `port` 为 `None`/`0` 时由系统分配，真实端口只在 [`Self::redirect_url`] 里可见。
    /// 返回前监听 socket 已绑定、accept 任务已 `spawn`，可以立刻向 `redirectUrl` 发请求。
    ///
    /// # 错误
    ///
    /// - 主机名解析失败：`getaddrinfo <EAI_AGAIN|EAI_FAIL|ENOTFOUND> {host}`（按错误文本归类）；
    ///   没有任何解析结果时固定报 `getaddrinfo ENOTFOUND {host}`。只取第一个解析结果，不在多个
    ///   地址之间回退（与 Node/libuv 一致）；
    /// - 绑定失败：按 Node `net` 的文案报 `listen EADDRINUSE: …`、`listen EACCES: …`、
    ///   `listen EADDRNOTAVAIL: …`；消息里的主机是**解析后**的地址（例如 `localhost` → `::1`），
    ///   映射逻辑在私有的 `bind_failure` 里；
    /// - 绑定成功但 `local_addr()` 失败：`OAuth callback server did not bind to TCP`。
    ///
    /// 这些都是 [`OAuthFailure::Error`]（`name == "Error"`），不携带 Node 的
    /// `code`/`errno`/`syscall`。
    pub async fn listen(options: OAuthCallbackServerOptions) -> Result<Self, OAuthFailure> {
        let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
        let redirect_host = options.redirect_host.unwrap_or_else(|| host.clone());
        let path = options.path.unwrap_or_else(|| "/callback".to_owned());
        let port = options.port.unwrap_or(0);
        // Node 的 `server.listen(port, host)` 先 `dns.lookup` 取**一个**地址，再只绑定它
        // （libuv 不会在多个 getaddrinfo 结果之间回退）；失败消息里的地址也是解析后的地址
        // （`localhost` → `::1`）。这里用同样的顺序：取第一个解析结果，只尝试一次。
        let mut addresses = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| bind_failure(&error, &host, port))?;
        let Some(address) = addresses.next() else {
            return Err(OAuthFailure::error(format!("getaddrinfo ENOTFOUND {host}")));
        };
        let listener = TcpListener::bind(address)
            .await
            .map_err(|error| bind_failure(&error, &address.ip().to_string(), address.port()))?;
        let address = listener
            .local_addr()
            .map_err(|_| OAuthFailure::error("OAuth callback server did not bind to TCP"))?;
        // TS: `${redirectHost.includes(":") ? `[${redirectHost}]` : redirectHost}` ——
        // 已带方括号的主机会被双层包裹（§30.16，照抄）。
        let host_part = if redirect_host.contains(':') {
            format!("[{redirect_host}]")
        } else {
            redirect_host
        };
        let redirect_url = format!("http://{host_part}:{}{path}", address.port());
        // `new URL(relative, this.redirectUrl)`：redirectUrl 总是能解析的正常 URL；万一
        // 调用方给了奇怪的主机名（Node 不校验字符串），退回一个可用的 base 保住路径匹配。
        let base = js_url::parse(&redirect_url)
            .unwrap_or_else(|()| Url::parse("http://127.0.0.1/").expect("static base URL"));
        let mut paths = vec![path];
        paths.extend(options.extra_paths);
        let inner = Arc::new(CallbackInner {
            redirect_url,
            base,
            paths,
            render_page: options.render_page,
            pending: Mutex::new(HashMap::new()),
        });
        let shutdown = CancellationToken::new();
        let connections = Arc::new(ConnectionTracker::default());
        let accept = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&inner),
            shutdown.clone(),
            Arc::clone(&connections),
        ));
        Ok(OAuthCallbackServer {
            inner,
            timeout_ms: options.timeout_ms.unwrap_or(5 * 60_000),
            shutdown,
            accept_task: Mutex::new(Some(accept)),
            connections,
        })
    }

    /// `readonly redirectUrl`：`http://{redirectHost}:{实际端口}{path}`。
    ///
    /// 端口是绑定后的真实端口（`port: 0` 时由系统分配）；主机取自 `redirectHost`（缺省等于
    /// `host`），含 `:` 时用方括号包裹，已带方括号的会再包一层（见模块文档的「逐字复刻的边界」）。
    pub fn redirect_url(&self) -> &str {
        &self.inner.redirect_url
    }

    /// `waitForCallback(state, path?)`：等待带 `state` 的授权响应。
    ///
    /// 重复的 `state` 立即报 `OAuth state is already pending`（TS 同步 throw）。`path` 给定时，
    /// 落在另一个路径的响应会以 `The authorization response arrived on another redirect URI` 失败。
    /// 超时 `OAuth callback timed out`；[`Self::close`] 以 `OAuth callback server closed` 拒绝。
    ///
    /// 外层 `Result` 只反映**同步注册**是否成功；等待本身在返回的 future 上。丢弃 future 不会
    /// 注销 `state`——pending 条目与定时器任务会留到超时或 `close()`。
    ///
    /// `path` 只在回调到达时比较，不参与路径白名单（白名单是 `path` 选项 + `extra_paths`）。
    /// 注册会 `spawn` 一个定时器任务，因此必须在 Tokio 运行时内调用。
    pub fn wait_for_callback(
        &self,
        state: impl Into<String>,
        path: Option<String>,
    ) -> Result<impl Future<Output = Result<OAuthCallback, OAuthFailure>>, OAuthFailure> {
        let state = state.into();
        let (sender, receiver) = oneshot::channel();
        let timer = CancellationToken::new();
        {
            let mut pending = self.inner.lock_pending();
            if pending.contains_key(&state) {
                return Err(OAuthFailure::error("OAuth state is already pending"));
            }
            pending.insert(
                state.clone(),
                PendingCallback {
                    sender,
                    path,
                    timer: timer.clone(),
                },
            );
        }
        // `setTimeout(..., this.timeoutMs)`：定时器在注册时启动，到期删除 pending 并拒绝。
        let inner = Arc::clone(&self.inner);
        let timeout_ms = node_timeout_ms(self.timeout_ms);
        let timer_state = state.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                    let entry = inner.lock_pending().remove(&timer_state);
                    if let Some(entry) = entry {
                        let _ = entry
                            .sender
                            .send(Err(OAuthFailure::error("OAuth callback timed out")));
                    }
                }
                () = timer.cancelled() => {}
            }
        });
        Ok(async move {
            match receiver.await {
                Ok(result) => result,
                // 服务器对象被丢弃时 sender 消失：按 TS 的关闭文案处理。
                Err(_) => Err(OAuthFailure::error("OAuth callback server closed")),
            }
        })
    }

    /// `close()`：以 `OAuth callback server closed` 拒绝所有 pending，然后停止服务器并等待
    /// 所有在途连接结束（Node 的 `server.close(cb)` 在全部连接结束后才回调）。
    ///
    /// 第二次调用返回 `Err("Server is not running.")`（Node 的 `server.close()` 以
    /// `ERR_SERVER_NOT_RUNNING` / `"Server is not running."` 拒绝）。
    ///
    /// 步骤：① 清空 pending，逐个取消定时器并以该文案拒绝等待方；② 取消 `shutdown`，让 accept
    /// 循环退出并等待它；③ 等待所有已登记的连接任务结束（写完响应，或因连接已关闭直接退出）。
    ///
    /// ① 先于 ②：drain 之后原先已注册的登录不会再被回调命中；之后进入 `handle` 的已知路径
    /// 请求因 state 缺失得到 400，未知路径仍 404（close 期间新注册的 state 仍可能被已接受的
    /// 连接命中）。② 取消后，一个字节都没收到的空闲连接被直接关闭、不写响应；已经收到首字节
    /// 的连接会把本次请求处理完，因此 `close()` 可能被一个慢客户端拖住。第二次调用时 accept
    /// 任务已被取走，所以返回 `Err`；在此之前它仍会（幂等地）再清空一次 pending、再取消一次
    /// `shutdown`。
    pub async fn close(&self) -> Result<(), OAuthFailure> {
        let drained: Vec<PendingCallback> = {
            let mut pending = self.inner.lock_pending();
            pending.drain().map(|(_, entry)| entry).collect()
        };
        for entry in drained {
            entry.timer.cancel();
            let _ = entry
                .sender
                .send(Err(OAuthFailure::error("OAuth callback server closed")));
        }
        self.shutdown.cancel();
        let task = {
            let mut slot = self
                .accept_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slot.take()
        };
        match task {
            Some(task) => {
                let _ = task.await;
                // 监听循环已退出，不会再登记新连接；等所有在途连接写完响应。
                let connections: Vec<JoinHandle<()>> = self.connections.lock().drain(..).collect();
                for connection in connections {
                    let _ = connection.await;
                }
                Ok(())
            }
            None => Err(OAuthFailure::error("Server is not running.")),
        }
    }
}

/// 丢弃时的兜底清理：不写响应、不等待，直接 `abort` accept 与连接任务。
///
/// 与 [`OAuthCallbackServer::close`] 的差别：`close` 尽力让在途请求写完响应并返回结果，`Drop`
/// 只保证后台任务不泄漏。pending 条目被清空后 sender 随之丢弃，等待方在 `receiver.await` 处
/// 得到 `Err`，被映射成 `OAuth callback server closed`（见 `wait_for_callback` 返回的 future）。
impl Drop for OAuthCallbackServer {
    fn drop(&mut self) {
        // 没有显式 close 时不留下监听任务与定时器。
        self.shutdown.cancel();
        let task = {
            let mut slot = self
                .accept_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slot.take()
        };
        if let Some(task) = task {
            task.abort();
        }
        // 没有显式 close 时也不能留下连接任务。
        for connection in self.connections.lock().drain(..) {
            connection.abort();
        }
        let drained: Vec<PendingCallback> = {
            let mut pending = self.inner.lock_pending();
            pending.drain().map(|(_, entry)| entry).collect()
        };
        for entry in drained {
            entry.timer.cancel();
        }
    }
}

/// 接受连接；`close()` 取消令牌后退出。
///
/// 每个连接 `spawn` 一个独立任务并登记到 `ConnectionTracker`；连接之间除 `pending` 表外没有
/// 共享可变状态。`accept()` 返回的错误被忽略（不记录、不退出循环）。
async fn accept_loop(
    listener: TcpListener,
    inner: Arc<CallbackInner>,
    shutdown: CancellationToken,
    connections: Arc<ConnectionTracker>,
) {
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                if let Ok((socket, _peer)) = accepted {
                    let inner = Arc::clone(&inner);
                    let shutdown = shutdown.clone();
                    connections.spawn(serve_connection(socket, inner, shutdown));
                }
            }
        }
    }
}

/// 一个请求的解析结果（只保留处理需要的部分）。
struct HttpRequestHead {
    method: String,
    target: String,
}

/// Node（llhttp）接受的 HTTP 方法 token，大小写敏感。
///
/// 列表由 Node 26 实测得出：其余 token（小写 `get`、`FOO`、`PRI`、RTSP 的 `DESCRIBE` 等）都会
/// 让 llhttp 报解析错误，Node 回 `400 Bad Request` + 空 body 且**不**调用 request handler。
/// `CONNECT` 在列表里，但 Node 的 server 没有 `connect` 监听器时直接关闭连接，也不调用 handler。
const NODE_METHODS: [&str; 35] = [
    "GET",
    "HEAD",
    "POST",
    "PUT",
    "DELETE",
    "CONNECT",
    "OPTIONS",
    "TRACE",
    "COPY",
    "LOCK",
    "MKCOL",
    "MOVE",
    "PROPFIND",
    "PROPPATCH",
    "SEARCH",
    "UNLOCK",
    "BIND",
    "REBIND",
    "UNBIND",
    "ACL",
    "REPORT",
    "MKACTIVITY",
    "CHECKOUT",
    "MERGE",
    "M-SEARCH",
    "NOTIFY",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "PATCH",
    "PURGE",
    "MKCALENDAR",
    "LINK",
    "UNLINK",
    "SOURCE",
    "QUERY",
];

/// llhttp 接受的 HTTP 版本 token（大小写敏感）：只有这四个，`HTTP/2`、`HTTP/3.0`、
/// `HTTP/1.11`、`http/1.1` 都是解析错误。
const NODE_HTTP_VERSIONS: [&str; 4] = ["HTTP/0.9", "HTTP/1.0", "HTTP/1.1", "HTTP/2.0"];

/// 请求行的解析结果。
enum RequestLine {
    /// 交给 handler（方法与目标）。
    Request { method: String, target: String },
    /// `CONNECT`：Node 不调用 handler，直接关闭连接。
    Connect,
    /// llhttp 会拒绝：`400 Bad Request` + 空 body。
    Malformed,
}

/// 按 llhttp 的宽容度解析请求行。
///
/// 实测的 Node 26 行为：方法后允许多个空格；版本 token 可省略（HTTP/0.9 的简单请求），否则
/// 必须是 [`NODE_HTTP_VERSIONS`] 之一；目标必须是 `/`、`*` 开头的 origin-form 或
/// `字母 scheme://…` 的 absolute-form；行首空格、tab 分隔、版本后再跟一个 token 都是 400。
fn parse_request_line(line: &str) -> RequestLine {
    let Some((method, rest)) = line.split_once(' ') else {
        return RequestLine::Malformed;
    };
    // 空方法（行首空格）与 tab 分隔都会落成不匹配的 token。
    if !NODE_METHODS.contains(&method) {
        return RequestLine::Malformed;
    }
    let rest = rest.trim_start_matches(' ');
    let (target, version) = match rest.split_once(' ') {
        Some((target, version)) => (target, Some(version)),
        None => (rest, None),
    };
    if target.is_empty() {
        return RequestLine::Malformed;
    }
    // llhttp 的 `normal_url_char`：URL 只接受可打印 ASCII（0x21–0x7E）。实测 Node 26 对 URL 里
    // 的 0x01/0x7F/0x80 都回 400（`<`、`"`、`{}`、`|`、`^`、反引号、反斜杠则是合法字符）。
    if !target.chars().all(|ch| ('!'..='~').contains(&ch)) {
        return RequestLine::Malformed;
    }
    if let Some(version) = version {
        let version = version.trim_start_matches(' ');
        if !NODE_HTTP_VERSIONS.contains(&version) {
            return RequestLine::Malformed;
        }
    }
    if method == "CONNECT" {
        return RequestLine::Connect;
    }
    // llhttp 的 `s_req_spaces_before_url`：非 CONNECT 的目标只能以 `/`、`*` 或字母开头；
    // 字母开头时必须是 absolute-form，scheme 只接受字母且后面必须紧跟 `://`。Node 26 实测：
    // `hello`、`1abc`、`?x`、`%2f`、`a:b`、`http:/x`、`a+b://x`、`a1://x` 都是 400 解析错误，
    // 而 `/x`、`*`、`http://x/y`、`httpx://x/y`、`//x/y` 都能走到 handler。
    match target.as_bytes().first() {
        Some(b'/' | b'*') => {}
        Some(first) if first.is_ascii_alphabetic() => {
            let scheme_len = target.bytes().take_while(u8::is_ascii_alphabetic).count();
            if !target[scheme_len..].starts_with("://") {
                return RequestLine::Malformed;
            }
        }
        _ => return RequestLine::Malformed,
    }
    RequestLine::Request {
        method: method.to_owned(),
        target: target.to_owned(),
    }
}

/// 请求头的读取结果。
enum HeadRead {
    /// 一个可交给 handler 的请求。
    Head(HttpRequestHead),
    /// llhttp 解析失败：回 `400 Bad Request` + 空 body。
    Malformed,
    /// `CONNECT`：不回复，直接关闭连接（Node 无 `connect` 监听器）。
    Connect,
    /// 连接在请求行之前关闭，或 `close()` 关闭了空闲连接。
    ///
    /// 也用于请求头读到一半 EOF、以及头块超过 `MAX_HEAD_BYTES` 的情况；这些情况都不写响应。
    Closed,
}

/// 读一行（去掉行尾 `\r\n`/`\n`）并按 **latin1** 解码。
///
/// Node 的 HTTP parser 把请求行与请求头当作 latin1 字节序列；按字节读再用 latin1 解码意味着
/// 非 UTF-8 的请求也能得到回复（`read_line(String)` 会因非法 UTF-8 直接断开）。
async fn read_line_latin1<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    if reader.read_until(b'\n', &mut bytes).await? == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(bytes.iter().map(|byte| char::from(*byte)).collect()))
}

/// 读请求行与头，丢弃请求体（§30.15：body 被忽略）。
///
/// 返回 `Closed` 覆盖三种情况：连接在请求头读完之前断开（请求行或任一头行读到 EOF）、头块超过
/// `MAX_HEAD_BYTES`、以及还没收到任何字节的空闲连接被 `close()` 取消。请求行与头先按 latin1
/// 解码再校验，所以任意字节的请求都能得到确定的结果（不会因非法 UTF-8 静默断开）。
///
/// 头块读完后再把 body 读掉才返回：未读数据留在 socket 里时，内核可能用 RST 吞掉响应。
async fn read_request_head(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    shutdown: &CancellationToken,
) -> HeadRead {
    // 头块上限（Node 的默认上限是 16KB，超出时回 431；这里放宽到 64KB 并直接断开，见模块文档）。
    const MAX_HEAD_BYTES: usize = 64 * 1024;
    // body 最多读这么多字节；超过后不再读，剩余数据留在 socket 里。
    const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

    // Node 19+ 的 `server.close()` 只关闭"一个字节都没收到"的真正空闲连接；一旦收到请求的
    // 首字节，连接就算在处理请求，close() 要等它结束（Node 26 实测：只发 `G` 也会等）。
    // 因此 shutdown 只在缓冲区为空时参与竞争，已经有字节在途就继续读完请求。
    if reader.buffer().is_empty() {
        let filled = tokio::select! {
            biased;
            () = shutdown.cancelled() => return HeadRead::Closed,
            filled = reader.fill_buf() => filled,
        };
        match filled {
            Ok(bytes) if !bytes.is_empty() => {}
            _ => return HeadRead::Closed,
        }
    }
    let Ok(Some(request_line)) = read_line_latin1(reader).await else {
        return HeadRead::Closed;
    };
    let (method, target) = match parse_request_line(&request_line) {
        RequestLine::Request { method, target } => (method, target),
        RequestLine::Connect => return HeadRead::Connect,
        RequestLine::Malformed => return HeadRead::Malformed,
    };
    // 头块大小按解码后字符串的字节数估算：每行补回被 read_line_latin1 去掉的 CRLF（+2）。
    // latin1 的 0x80–0xFF 解码后会占两个 UTF-8 字节，所以对非 ASCII 头这是偏大的估计。
    let mut head_bytes = request_line.len() + 2;
    let mut content_length = 0u64;
    let mut chunked = false;
    loop {
        let Ok(Some(line)) = read_line_latin1(reader).await else {
            return HeadRead::Closed;
        };
        head_bytes += line.len() + 2;
        if head_bytes > MAX_HEAD_BYTES {
            // 超过上限直接断开：不写响应（Node 在这一步回 431，见模块文档的已知偏差）。
            return HeadRead::Closed;
        }
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            // 没有冒号的头行被忽略而不是报 400（llhttp 会拒绝；见模块文档的已知偏差）。
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            // 头名大小写不敏感；解析失败按 0 处理——这里只关心“要不要读 body”，不影响响应。
            content_length = value.trim().parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    // 把 body 读掉再回复，避免未读数据让内核发 RST 吞掉响应。
    // chunked 与 content-length 同时出现时按 chunked 读（分支顺序）；读 body 的错误一律忽略，
    // body 内容不参与处理，连接随后就会关闭。
    if chunked {
        let _ = drain_chunked_body(reader, MAX_BODY_BYTES).await;
    } else if content_length > 0 && content_length <= MAX_BODY_BYTES {
        let mut sink = Vec::new();
        let _ = reader.take(content_length).read_to_end(&mut sink).await;
    }
    HeadRead::Head(HttpRequestHead { method, target })
}

/// 读掉 chunked 请求体（`size\r\n data \r\n` … `0\r\n\r\n`）。
///
/// 纯尽力而为：格式错误、超过 `max_bytes`、读到 EOF 都以 `Ok(())` 提前返回——body 内容本来
/// 就不参与处理，剩余字节不再读取。只有底层读取的 IO 错误会向外传播（调用方仍然忽略它）。
async fn drain_chunked_body(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    max_bytes: u64,
) -> std::io::Result<()> {
    let mut total = 0u64;
    loop {
        let Some(size_line) = read_line_latin1(reader).await? else {
            return Ok(());
        };
        let size_text = size_line.trim().split(';').next().unwrap_or("").trim();
        let Ok(size) = u64::from_str_radix(size_text, 16) else {
            return Ok(());
        };
        if size == 0 {
            // 末尾的 trailers + 空行。
            loop {
                match read_line_latin1(reader).await? {
                    None => return Ok(()),
                    Some(line) if line.is_empty() => return Ok(()),
                    Some(_) => {}
                }
            }
        }
        total += size;
        if total > max_bytes {
            return Ok(());
        }
        let mut sink = Vec::new();
        reader.take(size).read_to_end(&mut sink).await?;
        let mut crlf = [0u8; 2];
        let _ = reader.read_exact(&mut crlf).await;
    }
}

/// 一个连接任务：读请求头 → 调 `handle` → 写响应 → 关闭写半端。
///
/// IO 错误一律忽略：对端可能已经断开，没有可通知的对象。请求体只被读掉
/// （`read_request_head`），内容不参与处理；写完响应就关闭连接，不支持 keep-alive。
/// `CONNECT` 与 `Closed` 不写任何字节，`Malformed` 只写 Node 风格的空 body 400。
async fn serve_connection(
    socket: TcpStream,
    inner: Arc<CallbackInner>,
    shutdown: CancellationToken,
) {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let request = match read_request_head(&mut reader, &shutdown).await {
        HeadRead::Head(request) => request,
        // Node 的 server 没有 `connect` 监听器时直接关闭 socket，不写任何字节。
        HeadRead::Connect | HeadRead::Closed => return,
        HeadRead::Malformed => {
            // llhttp 解析错误的响应：Node 只写这一行 + `Connection: close`。
            let _ = write_half
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            let _ = write_half.flush().await;
            let _ = write_half.shutdown().await;
            return;
        }
    };
    let reply = inner.handle(&request.target);
    let _ = write_reply(&mut write_half, &reply, request.method == "HEAD").await;
    let _ = write_half.shutdown().await;
}

/// Node `setTimeout` 的延迟规范化：大于 2^31-1 时溢出并按 1ms 处理
/// （`TimeoutOverflowWarning`），0 也至少 1ms。负值/NaN 无法用 `u64` 表达。
fn node_timeout_ms(timeout_ms: u64) -> u64 {
    if timeout_ms > i32::MAX as u64 {
        1
    } else {
        timeout_ms.max(1)
    }
}

/// 写响应；HEAD 只写头（HTTP/1.1 规则）。
///
/// Node 对 HEAD 既不写 `content-length` 也不写 `transfer-encoding`（`response.end(body)` 的
/// body 被丢弃，且没有显式设置 `content-length`），这里同样省略 `content-length`。非 HEAD 的
/// `content-length` + `connection: close` 是本实现的分帧选择（§21 不要求 keep-alive/chunked）。
async fn write_reply(
    write: &mut OwnedWriteHalf,
    reply: &Reply,
    head_only: bool,
) -> std::io::Result<()> {
    let mut head = String::new();
    let _ = write!(
        head,
        "HTTP/1.1 {} {}\r\n",
        reply.status,
        reason_phrase(reply.status)
    );
    let _ = write!(head, "content-type: {}\r\n", reply.content_type);
    if let Some(cache_control) = reply.cache_control {
        let _ = write!(head, "cache-control: {cache_control}\r\n");
    }
    if !head_only {
        let _ = write!(head, "content-length: {}\r\n", reply.body.len());
    }
    head.push_str("connection: close\r\n\r\n");
    write.write_all(head.as_bytes()).await?;
    if !head_only {
        write.write_all(reply.body.as_bytes()).await?;
    }
    write.flush().await
}

/// 状态行里的原因短语：本模块只产生 200/400/404，其他值回退成 `Status`。
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Status",
    }
}

/// `server.listen` 的失败：复刻 Node `net` 的错误文本（coding-agent 会把用户配置端口的
/// 监听失败原样显示出来）。
///
/// Node 的 `EADDRINUSE` 等错误由 `net` 生成：`listen EADDRINUSE: address already in use {host}:{port}`
/// （`error.name === "Error"`，另有 `code`/`errno`/`syscall` 属性；[`OAuthFailure`] 只保留消息）。
/// `getaddrinfo` 失败（主机名解析）是 `getaddrinfo ENOTFOUND {host}`。
fn bind_failure(error: &std::io::Error, host: &str, port: u16) -> OAuthFailure {
    use std::io::ErrorKind;

    match error.kind() {
        ErrorKind::AddrInUse => {
            return OAuthFailure::error(format!(
                "listen EADDRINUSE: address already in use {host}:{port}"
            ));
        }
        ErrorKind::PermissionDenied => {
            return OAuthFailure::error(format!("listen EACCES: permission denied {host}:{port}"));
        }
        ErrorKind::AddrNotAvailable => {
            return OAuthFailure::error(format!(
                "listen EADDRNOTAVAIL: address not available {host}:{port}"
            ));
        }
        _ => {}
    }
    // Rust 把 `getaddrinfo` 失败包成 "failed to lookup address information: <gai_strerror>"。
    let text = error.to_string();
    if let Some(reason) = text.strip_prefix("failed to lookup address information: ") {
        let code = if reason.contains("Temporary failure") {
            "EAI_AGAIN"
        } else if reason.contains("Non-recoverable failure") {
            "EAI_FAIL"
        } else {
            "ENOTFOUND"
        };
        return OAuthFailure::error(format!("getaddrinfo {code} {host}"));
    }
    OAuthFailure::error(text)
}

/// 一个响应（`reply` 的输出）。
struct Reply {
    status: u16,
    content_type: &'static str,
    cache_control: Option<&'static str>,
    body: String,
}

impl CallbackInner {
    /// `handle(rawUrl, response)`：回调的核心状态机。
    ///
    /// 处理顺序（任一失败都直接返回页面，并可能结束等待方）：
    ///
    /// 1. 把请求目标按 `new URL(rawUrl, redirectUrl)` 解析；失败 → 404 `Not found`。
    /// 2. 路径必须在白名单里（与 `Url::path()` 精确比较）；否则 404，且**不**消耗 state。
    /// 3. `state` 非空且存在于 pending；否则 400 `Invalid or expired OAuth state`。
    ///    命中即从表中删除：后续任何失败都不会把 state 放回去。
    /// 4. 取消该条目的定时器（此后超时不会再触发）。
    /// 5. 注册时给过 `path` 且与本次路径不同 → 以 `The authorization response arrived on another
    ///    redirect URI` 结束等待方，页面回 400 `Unexpected redirect URI`。
    /// 6. `error` 非空 → 以 `error_description ?? error` 结束等待方，页面回 200（失败页）。
    /// 7. `code` 缺失或空串 → 以 `OAuth callback did not include an authorization code` 结束
    ///    等待方，页面回 400 `Missing authorization code`。
    /// 8. 成功：发送 `OAuthCallback { code, state, iss }`（`iss` 空串时为 `None`），回 200。
    ///
    /// `sender.send` 的失败一律忽略：等待方可能已被丢弃，但页面仍要正常渲染。
    fn handle(&self, target: &str) -> Reply {
        // `new URL(rawUrl, this.redirectUrl)`：相对 URL 也能解析；hash 不参与 pathname。
        let Ok(url) = js_url::parse_relative(&self.base, target) else {
            return self.reply(
                404,
                OAuthCallbackPage::Failure {
                    message: "Not found".to_owned(),
                    details: None,
                },
            );
        };
        // 白名单在 state 查找之前：未知路径的请求不会消耗 state，也不会结束等待方。
        if !self.paths.iter().any(|path| path == url.path()) {
            return self.reply(
                404,
                OAuthCallbackPage::Failure {
                    message: "Not found".to_owned(),
                    details: None,
                },
            );
        }
        // `const state = url.searchParams.get("state")`：空串是假值，等于没有 state。
        let state = query_param(&url, "state").filter(|state| !state.is_empty());
        // 命中即删除：后面 path/error/code 的校验失败也不会把 state 放回表中。
        let entry = state
            .as_ref()
            .and_then(|state| self.lock_pending().remove(state));
        let (Some(state), Some(entry)) = (state, entry) else {
            return self.reply(
                400,
                OAuthCallbackPage::Failure {
                    message: "Invalid or expired OAuth state".to_owned(),
                    details: None,
                },
            );
        };
        entry.timer.cancel();
        let wrong_path = entry
            .path
            .as_deref()
            .is_some_and(|expected| url.path() != expected);
        if wrong_path {
            let _ = entry.sender.send(Err(OAuthFailure::error(
                "The authorization response arrived on another redirect URI",
            )));
            return self.reply(
                400,
                OAuthCallbackPage::Failure {
                    message: "Unexpected redirect URI".to_owned(),
                    details: None,
                },
            );
        }
        // `if (error)`：空串按假值处理（等于没有 error）。
        if let Some(error) = query_param(&url, "error").filter(|error| !error.is_empty()) {
            // `url.searchParams.get("error_description") ?? error`：`??` 只跳过 null。
            let description =
                query_param(&url, "error_description").unwrap_or_else(|| error.clone());
            let _ = entry
                .sender
                .send(Err(OAuthFailure::error(description.clone())));
            return self.reply(
                200,
                OAuthCallbackPage::Failure {
                    message: "Authorization failed. You may close this window.".to_owned(),
                    details: Some(description),
                },
            );
        }
        let code = match query_param(&url, "code").filter(|code| !code.is_empty()) {
            Some(code) => code,
            None => {
                let _ = entry.sender.send(Err(OAuthFailure::error(
                    "OAuth callback did not include an authorization code",
                )));
                return self.reply(
                    400,
                    OAuthCallbackPage::Failure {
                        message: "Missing authorization code".to_owned(),
                        details: None,
                    },
                );
            }
        };
        let iss = query_param(&url, "iss").filter(|iss| !iss.is_empty());
        let _ = entry.sender.send(Ok(OAuthCallback { code, state, iss }));
        self.reply(200, OAuthCallbackPage::Success)
    }

    /// `reply(response, status, page)`：`renderPage` 决定 HTML 还是纯文本。
    ///
    /// 有 `renderPage` 时：`text/html; charset=utf-8` + `cache-control: no-store`，body 完全由
    /// 回调生成（不转义）；没有时：`text/plain; charset=utf-8`、不带 `cache-control`，body 由
    /// `plain_text` 生成。状态码原样使用（200/400/404）。
    fn reply(&self, status: u16, page: OAuthCallbackPage) -> Reply {
        match &self.render_page {
            Some(render_page) => Reply {
                status,
                content_type: "text/html; charset=utf-8",
                cache_control: Some("no-store"),
                body: render_page(&page),
            },
            None => Reply {
                status,
                content_type: "text/plain; charset=utf-8",
                cache_control: None,
                body: plain_text(&page),
            },
        }
    }
}

/// `plainText(page)`。
///
/// 失败页的 `details` 为空串时只输出 message（对应 `page.details ? … : page.message` 的假值
/// 语义）；非空时用空行分隔 message 与 details。
fn plain_text(page: &OAuthCallbackPage) -> String {
    match page {
        OAuthCallbackPage::Success => {
            "Authorization complete. You may close this window.".to_owned()
        }
        OAuthCallbackPage::Failure { message, details } => {
            match details.as_deref().filter(|details| !details.is_empty()) {
                Some(details) => format!("{message}\n\n{details}"),
                None => message.clone(),
            }
        }
    }
}

/// `url.searchParams.get(name)`：同名参数取第一个；`+` 按空格解码（form-urlencoded）。
fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_pages_match_the_browser_text() {
        assert_eq!(
            plain_text(&OAuthCallbackPage::Success),
            "Authorization complete. You may close this window."
        );
        assert_eq!(
            plain_text(&OAuthCallbackPage::Failure {
                message: "Not found".to_owned(),
                details: None,
            }),
            "Not found"
        );
        assert_eq!(
            plain_text(&OAuthCallbackPage::Failure {
                message: "Authorization failed. You may close this window.".to_owned(),
                details: Some("Denied".to_owned()),
            }),
            "Authorization failed. You may close this window.\n\nDenied"
        );
        // 空 details 是假值：只输出 message（`page.details ? … : page.message`）。
        assert_eq!(
            plain_text(&OAuthCallbackPage::Failure {
                message: "Authorization failed. You may close this window.".to_owned(),
                details: Some(String::new()),
            }),
            "Authorization failed. You may close this window."
        );
    }

    #[tokio::test]
    async fn redirect_url_defaults_to_127_0_0_1_and_callback() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        assert!(server.redirect_url().starts_with("http://127.0.0.1:"));
        assert!(server.redirect_url().ends_with("/callback"));
        server.close().await.expect("close");
    }

    #[tokio::test]
    async fn redirect_host_is_bracketed_when_it_contains_a_colon() {
        // `redirectHost: "::1"` → `[::1]`；监听 127.0.0.1 以免依赖 IPv6。
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions {
            redirect_host: Some("::1".to_owned()),
            ..OAuthCallbackServerOptions::default()
        })
        .await
        .expect("listen");
        assert!(server.redirect_url().starts_with("http://[::1]:"));
        server.close().await.expect("close");

        // 已带方括号时双层包裹（§30.16，未处理的边界照抄）。
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions {
            redirect_host: Some("[::1]".to_owned()),
            ..OAuthCallbackServerOptions::default()
        })
        .await
        .expect("listen");
        assert!(server.redirect_url().starts_with("http://[[::1]]:"));
        server.close().await.expect("close");
    }

    #[tokio::test]
    async fn a_duplicate_state_is_rejected_synchronously() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let _first = server.wait_for_callback("state", None).expect("first wait");
        let error = match server.wait_for_callback("state", None) {
            Ok(_) => panic!("duplicate state must fail"),
            Err(error) => error,
        };
        assert_eq!(error.message(), "OAuth state is already pending");
        server.close().await.expect("close");
    }

    #[tokio::test]
    async fn close_rejects_pending_callbacks() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let pending = server.wait_for_callback("state", None).expect("wait");
        server.close().await.expect("close");
        let error = pending.await.expect_err("closed");
        assert_eq!(error.message(), "OAuth callback server closed");
    }

    #[tokio::test]
    async fn the_timeout_rejects_the_callback() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions {
            timeout_ms: Some(20),
            ..OAuthCallbackServerOptions::default()
        })
        .await
        .expect("listen");
        let pending = server.wait_for_callback("state", None).expect("wait");
        let error = pending.await.expect_err("timed out");
        assert_eq!(error.message(), "OAuth callback timed out");
        // 超时后 pending 已删除：同一 state 可以重新等待。
        let again = server.wait_for_callback("state", None).expect("wait again");
        drop(again);
        server.close().await.expect("close");
    }

    #[tokio::test]
    async fn binding_an_occupied_port_reports_the_node_error() {
        // Node: `Error: listen EADDRINUSE: address already in use 127.0.0.1:<port>`。
        let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("blocker");
        let port = blocker.local_addr().expect("address").port();
        let error = match OAuthCallbackServer::listen(OAuthCallbackServerOptions {
            port: Some(port),
            ..OAuthCallbackServerOptions::default()
        })
        .await
        {
            Ok(_) => panic!("binding an occupied port must fail"),
            Err(error) => error,
        };
        assert_eq!(error.name(), "Error");
        assert_eq!(
            error.message(),
            format!("listen EADDRINUSE: address already in use 127.0.0.1:{port}")
        );
        drop(blocker);
    }

    #[test]
    fn bind_failure_matches_the_node_messages() {
        use std::io::{Error, ErrorKind};

        let error = bind_failure(&Error::from(ErrorKind::AddrInUse), "127.0.0.1", 1234);
        assert_eq!(error.name(), "Error");
        assert_eq!(
            error.message(),
            "listen EADDRINUSE: address already in use 127.0.0.1:1234"
        );
        let error = bind_failure(&Error::from(ErrorKind::PermissionDenied), "127.0.0.1", 80);
        assert_eq!(
            error.message(),
            "listen EACCES: permission denied 127.0.0.1:80"
        );
        let error = bind_failure(&Error::from(ErrorKind::AddrNotAvailable), "1.2.3.4", 80);
        assert_eq!(
            error.message(),
            "listen EADDRNOTAVAIL: address not available 1.2.3.4:80"
        );
        // getaddrinfo 失败：Rust 的文本 → Node 的 `getaddrinfo <code> <host>`。
        let error = bind_failure(
            &Error::other("failed to lookup address information: Name or service not known"),
            "no-such-host.invalid",
            0,
        );
        assert_eq!(
            error.message(),
            "getaddrinfo ENOTFOUND no-such-host.invalid"
        );
        let error = bind_failure(
            &Error::other(
                "failed to lookup address information: Temporary failure in name resolution",
            ),
            "flaky.invalid",
            0,
        );
        assert_eq!(error.message(), "getaddrinfo EAI_AGAIN flaky.invalid");
    }

    #[tokio::test]
    async fn the_second_close_reports_server_not_running() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        server.close().await.expect("first close");
        let error = server.close().await.expect_err("second close");
        assert_eq!(error.name(), "Error");
        assert_eq!(error.message(), "Server is not running.");
    }

    #[tokio::test]
    async fn head_responses_have_no_content_length() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let pending = server.wait_for_callback("s1", None).expect("wait");
        let redirect = Url::parse(server.redirect_url()).expect("redirect URL");
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().expect("port")))
            .await
            .expect("connect");
        let request = format!(
            "HEAD {}?code=abc&state=s1 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            redirect.path()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n").expect("headers");
        assert!(head.starts_with("HTTP/1.1 200 OK"), "head: {head}");
        let lower = head.to_ascii_lowercase();
        // Node 的 HEAD 响应既没有 content-length 也没有 transfer-encoding。
        assert!(!lower.contains("content-length"), "head: {head}");
        assert!(!lower.contains("transfer-encoding"), "head: {head}");
        assert!(body.is_empty(), "body: {body}");
        assert_eq!(pending.await.expect("callback").code, "abc");
        server.close().await.expect("close");
    }

    /// 原始 TCP 请求 → 完整响应文本（连接读到头）。
    async fn raw_exchange(server: &OAuthCallbackServer, request: impl AsRef<[u8]>) -> String {
        let redirect = Url::parse(server.redirect_url()).expect("redirect URL");
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().expect("port")))
            .await
            .expect("connect");
        stream
            .write_all(request.as_ref())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        String::from_utf8_lossy(&response).into_owned()
    }

    fn path_of(server: &OAuthCallbackServer) -> String {
        Url::parse(server.redirect_url())
            .expect("redirect URL")
            .path()
            .to_owned()
    }

    /// 请求行/头里的非 UTF-8 字节按 latin1 解码（Node 的 HTTP parser 语义），连接照常回复。
    #[tokio::test]
    async fn non_utf8_request_bytes_are_decoded_as_latin1() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);
        let pending = server
            .wait_for_callback("latin1", None)
            .expect("wait for callback");
        let mut request =
            format!("GET {path}?code=abc&state=latin1 HTTP/1.1\r\nHost: x\r\nX-Raw: ").into_bytes();
        request.extend_from_slice(&[0xff, 0xfe]);
        request.extend_from_slice(b"\r\n\r\n");
        let response = raw_exchange(&server, request).await;
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "response: {response}"
        );
        assert_eq!(pending.await.expect("callback").code, "abc");
        server.close().await.expect("close");
    }

    /// Node 26 实测：llhttp 不认识的方法 token → `400 Bad Request` + 空 body，handler 不执行。
    #[tokio::test]
    async fn unknown_method_tokens_get_the_node_parse_error() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);
        for method in ["FOO", "get", "PRI", "DESCRIBE", "MYMETHOD", "CHECKIN"] {
            let state = format!("method-{method}");
            let pending = server
                .wait_for_callback(state.clone(), None)
                .expect("wait for callback");
            let response = raw_exchange(
                &server,
                &format!("{method} {path}?code=abc&state={state} HTTP/1.1\r\nHost: x\r\n\r\n"),
            )
            .await;
            assert_eq!(
                response, "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                "method {method} must be rejected by the parser"
            );
            // handler 没有被调用：pending 仍未完成。
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pending)
                    .await
                    .is_err(),
                "method {method} must not reach the handler"
            );
        }
        server.close().await.expect("close");
    }

    /// Node 的 server 没有 `connect` 监听器：`CONNECT` 直接关闭连接，不写响应、不调 handler。
    #[tokio::test]
    async fn connect_gets_no_response_and_never_reaches_the_handler() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);
        let pending = server
            .wait_for_callback("connect-state", None)
            .expect("wait for callback");
        let response = raw_exchange(
            &server,
            &format!("CONNECT {path}?code=abc&state=connect-state HTTP/1.1\r\nHost: x\r\n\r\n"),
        )
        .await;
        assert_eq!(response, "");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), pending)
                .await
                .is_err(),
            "CONNECT must not reach the handler"
        );
        server.close().await.expect("close");
    }

    /// 请求行形状的宽容度与 Node 26 实测一致。
    #[tokio::test]
    async fn request_line_shapes_match_llhttp() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);

        // 合法：无版本号、方法后双空格、llhttp 认识的四个版本 token。
        for (index, line) in [
            format!("GET {path}?code=abc&state=shape-0"),
            format!("GET  {path}?code=abc&state=shape-1 HTTP/1.1"),
            format!("GET {path}?code=abc&state=shape-2 HTTP/2.0"),
            format!("PATCH {path}?code=abc&state=shape-3 HTTP/1.0"),
            format!("GET {path}?code=abc&state=shape-4 HTTP/0.9"),
            format!("QUERY {path}?code=abc&state=shape-5 HTTP/1.1"),
        ]
        .into_iter()
        .enumerate()
        {
            let state = format!("shape-{index}");
            let pending = server
                .wait_for_callback(state.clone(), None)
                .expect("wait for callback");
            let response = raw_exchange(&server, &format!("{line}\r\nHost: x\r\n\r\n")).await;
            assert!(
                response.starts_with("HTTP/1.1 200 OK"),
                "request {line:?} must reach the handler: {response}"
            );
            assert_eq!(pending.await.expect("callback").code, "abc");
        }

        // 非法：行首空格、tab 分隔、版本后的多余 token、llhttp 不认识的版本 token。
        for (index, line) in [
            format!(" GET {path}?code=abc&state=bad-0 HTTP/1.1"),
            format!("GET\t{path}?code=abc&state=bad-1 HTTP/1.1"),
            format!("GET {path}?code=abc&state=bad-2 HTTP/1.1 extra"),
            format!("GET {path}?code=abc&state=bad-3 HTTP/3.0"),
            format!("GET {path}?code=abc&state=bad-4 HTTP/2"),
            format!("GET {path}?code=abc&state=bad-5 http/1.1"),
            format!("GET {path}?code=abc&state=bad-6&x=\u{80} HTTP/1.1"),
            format!("GET {path}?code=abc&state=bad-7&x=\u{7f} HTTP/1.1"),
        ]
        .into_iter()
        .enumerate()
        {
            let response = raw_exchange(&server, &format!("{line}\r\nHost: x\r\n\r\n")).await;
            assert_eq!(
                response, "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                "request {index} must be rejected"
            );
        }
        server.close().await.expect("close");
    }

    /// Node 26 实测：非 CONNECT 的请求目标只能是 `/`、`*` 开头的 origin-form 或
    /// `字母 scheme://…` 的 absolute-form；其余形式（`hello`、`1abc`、`a:b`、`http:/x`）是
    /// llhttp 的解析错误（400 + 空 body），handler 不会被调用。
    #[tokio::test]
    async fn request_target_forms_match_llhttp() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");

        // 合法的目标都走到 handler：路径不在白名单 → 404 `Not found`。
        for (index, form) in [
            "/x",
            "*",
            "*x",
            "//x/y",
            "http://x/y",
            "httpx://x/y",
            "a://x",
        ]
        .into_iter()
        .enumerate()
        {
            let response =
                raw_exchange(&server, format!("GET {form} HTTP/1.1\r\nHost: x\r\n\r\n")).await;
            assert!(
                response.starts_with("HTTP/1.1 404 Not Found"),
                "target {index} ({form:?}) must reach the handler: {response}"
            );
            assert!(
                response.contains("Not found"),
                "target {index} ({form:?}) must be answered by the handler: {response}"
            );
        }

        // 非法目标：llhttp 解析错误。
        for (index, form) in [
            "hello", "1abc", "?x", "-x", "%2f", "a:b", "http:/x", "a+b://x", "a.b://x", "a1://x",
            "1a://x",
        ]
        .into_iter()
        .enumerate()
        {
            for version in ["HTTP/1.1", ""] {
                let line = format!("GET {form} {version}");
                let response =
                    raw_exchange(&server, format!("{}\r\nHost: x\r\n\r\n", line.trim_end())).await;
                assert_eq!(
                    response, "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                    "target {index} ({form:?}) must be rejected"
                );
            }
        }
        server.close().await.expect("close");
    }

    /// Node `setTimeout` 的钳位：> 2^31-1 → 1ms，0 → 1ms。
    #[test]
    fn node_timeout_ms_clamps_like_set_timeout() {
        assert_eq!(node_timeout_ms(0), 1);
        assert_eq!(node_timeout_ms(1), 1);
        assert_eq!(node_timeout_ms(20), 20);
        assert_eq!(node_timeout_ms(300_000), 300_000);
        assert_eq!(node_timeout_ms(i32::MAX as u64), i32::MAX as u64);
        assert_eq!(node_timeout_ms(i32::MAX as u64 + 1), 1);
        assert_eq!(node_timeout_ms(u64::MAX), 1);
    }

    /// Node 26 实测：只要收到请求的第一个字节，连接就算"在处理请求"，`close()` 要等它结束；
    /// 一个字节都没收到的空闲连接才被直接关闭。
    #[tokio::test]
    async fn close_waits_for_a_connection_that_only_started_its_request_line() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);
        let port = Url::parse(server.redirect_url())
            .expect("redirect URL")
            .port()
            .expect("port");

        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        // 只发请求行的前半段（还没有换行）。
        let (head, tail) = path.split_at(path.len() / 2);
        stream
            .write_all(format!("GET {head}").as_bytes())
            .await
            .expect("partial request line");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let close = tokio::spawn(async move { server.close().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !close.is_finished(),
            "close must wait for a started request line"
        );

        // 补完请求：pending 已被 close() 清空，得到 400；随后 close() 返回。
        stream
            .write_all(format!("{tail}?code=abc&state=late HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .expect("finish request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.starts_with("HTTP/1.1 400 Bad Request"),
            "response: {text}"
        );
        assert!(
            text.contains("Invalid or expired OAuth state"),
            "response: {text}"
        );
        close.await.expect("close task").expect("close");
    }

    /// Node 19+ 的 `server.close()`：关闭空闲连接，等已经收到部分请求的连接把请求处理完。
    #[tokio::test]
    async fn close_closes_idle_connections_and_waits_for_in_flight_ones() {
        let server = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
            .await
            .expect("listen");
        let path = path_of(&server);
        let port = Url::parse(server.redirect_url())
            .expect("redirect URL")
            .port()
            .expect("port");

        let mut idle = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("idle connect");
        let mut in_flight = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("in-flight connect");
        // 只发请求行 + 一部分头：连接已经"在处理请求"，close() 要等它结束。
        in_flight
            .write_all(
                format!("GET {path}?code=abc&state=closed HTTP/1.1\r\nHost: x\r\n").as_bytes(),
            )
            .await
            .expect("partial request");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let close = tokio::spawn(async move { server.close().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !close.is_finished(),
            "close must wait for the in-flight request"
        );

        // 空闲连接被 close() 关掉（读到 EOF 或连接错误，而不是超时）。
        let mut byte = [0u8; 1];
        let idle_read = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut byte)).await;
        match idle_read {
            Ok(Ok(0)) | Ok(Err(_)) => {}
            other => panic!("idle connection must be closed by close(): {other:?}"),
        }

        // 完成在途请求：pending 已被 close() 清空，得到 400；随后 close() 返回。
        in_flight.write_all(b"\r\n").await.expect("finish request");
        let mut response = Vec::new();
        in_flight
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.starts_with("HTTP/1.1 400 Bad Request"),
            "response: {text}"
        );
        assert!(
            text.contains("Invalid or expired OAuth state"),
            "response: {text}"
        );
        close.await.expect("close task").expect("close");
    }
}
