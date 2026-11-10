//! auth/oauth/callback-server.ts。
//!
//! 浏览器登录流程共享的 loopback OAuth 重定向处理器。TS 用 `node:http`；Rust 侧用
//! `tokio::net::TcpListener` + 手写 HTTP/1.1（只需 GET 请求行与响应头，见 spec/ai-auth.md
//! §6.2 #20：测试只断言状态码与 `content-type`，头部集合可以自由扩展）。
//!
//! 状态机顺序（不可交换，spec §4.4）：
//! 1. `method !== GET || pathname !== path` → 404；
//! 2. state 不匹配（仅当 options.state 定义）→ 400；
//! 3. `claimed || settled` → 409；
//! 4. `error` 参数存在 → 400 并 settle(reject)；
//! 5. `code` 缺失 → 400（**不** settle）；
//! 6. `claimed = true` → 跑 `complete(code)` → 200 resolve / 502 reject。
//!
//! `cancel()` 只在 `!claimed` 时生效并让 `wait()` resolve `None`；`close()` 以
//! `"OAuth callback server closed"` reject。
//!
//! # 为什么是一个通用服务器
//! Anthropic / OpenAI Codex / OpenRouter / Radius 的浏览器登录都需要“在本机收一次 GET
//! 回调、校验 state、把 code 交给流程”。HTTP 细节集中在这里后，各流程只提供 provider 名、
//! 路径、期望 state 与 `complete` 回调；`complete` 在返回网页**之前**执行，因此交换失败
//! 会直接显示在浏览器页面上（502），而不是只让终端里的等待失败。
//!
//! # 并发模型与生命周期
//! - 绑定成功后 `tokio::spawn` 一个 accept 循环；每个连接再 `spawn` 一个处理任务。两者都
//!   不保存 `JoinHandle`：停止接受新连接只能靠 [`OAuthCallbackServer::close`] 或句柄 `Drop`
//!   （取消 `shutdown` token）；在途连接没有统一的退出等待，通常写完响应后自行结束。
//!   连接任务也没有读写超时：对端不读取响应时 `write_all` 可以一直挂起。
//! - 所有观察者共享 `CallbackShared`：`result` 由 `Mutex` 保护，`settled`/`claimed` 是原子
//!   标志，`Notify` 唤醒 `wait()`。`finish` 里的 `settled.swap(true)` 保证第一个结果生效
//!   （回调、`cancel`、`close`、超时、abort 之间“先到先赢”）。
//! - `wait()` 先用 `notified().enable()` 注册等待者、再检查 `result`，所以 `finish` 恰好
//!   发生在两步之间也不会丢唤醒；结果 clone 后返回，因此需要 `T: Clone`。
//!
//! # 注意事项
//! - 端口被占用时**不会**自动换端口：调用方（例如
//!   [`crate::auth::oauth::anthropic`] 的浏览器流程）根据返回的错误决定回退策略，这是
//!   “绝不静默换端口”不变量的一部分。
//! - `complete` 的结果会一直保存在共享状态里，直到所有持有它的句柄/任务退出；不要把大
//!   对象作为 `T`。
//! - `cancel()` 让 `wait()` 以 `Ok(None)` 成功返回，abort/超时/`close()` 则让它返回错误；
//!   调用方必须区分“用户改用手动粘贴”和“登录失败”。
//!
//! TS: `src/auth/oauth/callback-server.ts`

use std::ffi::CString;
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::types::{AuthError, AuthInteraction, AuthPrompt, AuthPromptRequest};
use crate::utils::abort::{AbortListener, AbortScope};
use crate::utils::oauth_page::{oauth_error_html, oauth_success_html};

/// 完成登录的异步回调（`complete: (code) => Promise<T>`）。
///
/// 服务器会在写 HTTP 响应之前 await 它，所以实现应尽快返回；不要在这里等待人工输入。
pub type CompleteFuture<T> = Pin<Box<dyn Future<Output = Result<T, AuthError>> + Send + 'static>>;

/// `complete(code)` 回调。
///
/// 用 `Fn` 而不是 `FnOnce`：闭包会被多个连接任务共享；实际只有一个连接能通过 `claimed`
/// 竞争成功并执行交换。
pub type CompleteFn<T> = Arc<dyn Fn(String) -> CompleteFuture<T> + Send + Sync>;

/// 回调服务器选项。
///
/// TS: `src/auth/oauth/callback-server.ts#OAuthCallbackServerOptions`
pub struct OAuthCallbackServerOptions<T> {
    /// 浏览器页面上的 provider 名，例如 `OpenAI`；也用于错误文案
    /// （`${provider_name} authorization failed.` 等）。
    pub provider_name: String,
    /// 监听地址。只解析一次并绑定首个地址（libuv 语义）；`127.0.0.1` 只对本机可见。
    pub host: String,
    /// 监听端口；`0` 选空闲端口。被占用时返回 Node 形状的 `EADDRINUSE`，不会改绑其他端口。
    pub port: u16,
    /// 回调路径。与请求路径**完全相等**比较（含前导 `/`）；查询串不参与比较。
    pub path: String,
    /// `redirectUri` 中与 `host` 不同的主机，例如 `localhost`。
    ///
    /// 只影响返回的 `redirectUri`，不参与绑定。典型用法：绑定 `127.0.0.1`，让浏览器访问
    /// `localhost`。
    pub redirect_host: Option<String>,
    /// 期望的 `state` 参数；provider 不发 state 时省略。
    ///
    /// `Some` 时按字符串精确比较（`URLSearchParams.get`，重复键取第一个）；`None` 时跳过
    /// state 检查。
    pub state: Option<String>,
    /// 在返回网页之前完成登录（能显示交换失败）；`|code| async { Ok(code) }` 表示稍后交换。
    ///
    /// 成功值成为 `wait()` 的 `Some(value)`；返回错误会让页面 502、`wait()` 得到同一错误。
    pub complete: CompleteFn<T>,
    /// 取消信号。
    ///
    /// 启动前已中止 → [`start_oauth_callback_server`] 直接返回 `"Login cancelled"`；
    /// 运行中中止 → `wait()` 以同一文案失败。
    pub signal: Option<AbortScope>,
    /// 超时（毫秒）。
    ///
    /// 按 Node `setTimeout` 规则钳制（0 或超过 `2^31-1` → 1ms）；触发后 `wait()` 以
    /// `${provider_name} sign-in timed out` 失败。
    pub timeout_ms: Option<u64>,
}

/// 回调服务器与所有等待者共享的状态机。
///
/// - `result`：唯一结果槽；一旦写入不再改变（`finish` 只生效一次）。
/// - `settled`：已有结果（成功或失败）；用于拒绝迟到的回调（409）。
/// - `claimed`：某个请求已取走 code、正在执行 `complete`；此时 `cancel()` 不生效。
/// - `timer_cancel`：让超时任务在提前 settle 后退出。
/// - `abort_listener`：abort 监听句柄，`finish` 时移除（对应 TS 的 `removeEventListener`）。
struct CallbackShared<T> {
    result: Mutex<Option<Result<Option<T>, AuthError>>>,
    settled: AtomicBool,
    claimed: AtomicBool,
    notify: Notify,
    abort_listener: Mutex<Option<AbortListener>>,
    timer_cancel: CancellationToken,
}

impl<T> CallbackShared<T> {
    fn new() -> Self {
        CallbackShared {
            result: Mutex::new(None),
            settled: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
            notify: Notify::new(),
            abort_listener: Mutex::new(None),
            timer_cancel: CancellationToken::new(),
        }
    }

    /// `finish(result)`：只生效一次；清定时器、移除 abort 监听并唤醒 waiter。
    ///
    /// 先抢占 `settled` 再改共享状态，竞争失败的调用直接返回。锁中毒时取回内层值，
    /// 与仓库其他全局表一致：一个 panic 的测试不应永久毒化服务器。
    fn finish(&self, result: Result<Option<T>, AuthError>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        self.timer_cancel.cancel();
        if let Some(listener) = self
            .abort_listener
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            listener.remove();
        }
        *self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(result);
        self.notify.notify_waiters();
    }
}

/// 回调服务器句柄。
///
/// 持有 `redirect_uri`、共享状态与停止 accept 循环的 token。句柄被丢弃时只停止监听，
/// 不会把已 settle 的结果改成错误。
///
/// TS: `src/auth/oauth/callback-server.ts#OAuthCallbackServer`
pub struct OAuthCallbackServer<T> {
    redirect_uri: String,
    shared: Arc<CallbackShared<T>>,
    shutdown: CancellationToken,
}

impl<T> std::fmt::Debug for OAuthCallbackServer<T> {
    /// 手写实现避免要求 `T: Debug`：只报告 redirect_uri 与是否已 settle。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCallbackServer")
            .field("redirect_uri", &self.redirect_uri)
            .field("settled", &self.shared.settled.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl<T: Clone> OAuthCallbackServer<T> {
    /// `redirectUri`。
    ///
    /// 端口是内核**实际分配**的端口（`port: 0` 时尤其重要），主机来自 `redirect_host`
    /// （缺省为 `host`），IPv6 字面量会加方括号。
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// 等待浏览器回调：`complete` 的结果，或 `cancel()` 后的 `None`。
    ///
    /// provider 带 error 重定向、`complete` 失败、signal abort 或超时时 reject。
    ///
    /// 可以多次调用：每次都 clone 同一个已 settle 的结果；结果产生前一直挂起。
    pub async fn wait(&self) -> Result<Option<T>, AuthError> {
        loop {
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self
                .shared
                .result
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}

impl<T> OAuthCallbackServer<T> {
    /// 停止等待浏览器，除非回调已在完成中（`claimed`）。
    ///
    /// 已 claimed 时调用不生效：正在执行的 `complete` 会继续完成并 resolve。
    pub fn cancel(&self) {
        if !self.shared.claimed.load(Ordering::SeqCst) {
            self.shared.finish(Ok(None));
        }
    }

    /// `finish` 尚未 settle 时以 `"OAuth callback server closed"` reject，并停止监听。
    ///
    /// 幂等：已 settle 时只停监听，不会覆盖已有结果。
    pub fn close(&self) {
        self.shared
            .finish(Err(AuthError::new("OAuth callback server closed")));
        self.shutdown.cancel();
    }
}

impl<T> Drop for OAuthCallbackServer<T> {
    fn drop(&mut self) {
        // TS 的 server 生命周期由调用方管理；Rust 侧在句柄销毁时停止接受新连接，
        // 避免测试/流程泄漏监听端口（不改变 `wait()` 的已 settle 结果）。
        self.shutdown.cancel();
    }
}

/// 回调服务器内部选项（与 `OAuthCallbackServerOptions` 分开以便在任务间共享）。
///
/// 只保留路由所需字段与 `complete`；signal/timeout 不进入 `ServerOptions`：它们通过
/// `shared.settled` 影响后续请求（409），路由本身不需要这两个字段。
struct ServerOptions<T> {
    provider_name: String,
    path: String,
    state: Option<String>,
    complete: CompleteFn<T>,
}

/// `sendPage(response, status, html)` 的迭代：`content-type` 小写、`cache-control: no-store`。
///
/// 发送 spec/ai-auth.md §4.4 要求的 `content-type` 与 `cache-control: no-store`，并附加
/// `content-length`、`connection: close`、`date`；测试只断言状态码与 `content-type`
/// （spec/ai-auth.md §6.2 #20），头部集合没有额外约束。`content-length` 是
/// body 的 UTF-8 字节数；`date` 用当前 UTC 时间。
fn http_response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        502 => "Bad Gateway",
        _ => "Unknown",
    };
    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT");
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\ndate: {date}\r\n\r\n",
        body.len()
    );
    let mut bytes = Vec::with_capacity(head.len() + body.len());
    bytes.extend_from_slice(head.as_bytes());
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// 一次请求的回复；`finish` 在响应写出后应用。
///
/// `page()` 构造“不 settle”的回复（404、state 错误、409、缺 code），只有 provider error
/// 与 `complete` 的结果才携带 `finish`。
struct Reply<T> {
    status: u16,
    body: String,
    finish: Option<Result<Option<T>, AuthError>>,
}

impl<T> Reply<T> {
    fn page(status: u16, body: String) -> Self {
        Reply {
            status,
            body,
            finish: None,
        }
    }
}

/// `new URL(request.url ?? "/", "http://localhost")`。
///
/// 以 `/` 开头的 target 用 `http://localhost` 作为 base（对应 `new URL(request.url, base)`）；
/// 否则按绝对 URL 解析。失败返回 `None`，路由层转 404。
fn parse_url(target: &str) -> Option<Url> {
    if target.starts_with('/') {
        Url::parse("http://localhost").ok()?.join(target).ok()
    } else {
        Url::parse(target).ok()
    }
}

/// `url.searchParams.get(name)`：重复键取第一个；键存在但值为空时返回 `Some("")`。
fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// 读取请求头，直到出现 `\r\n\r\n`。
///
/// 不读 body（路由只接受 GET）；EOF、读错误、非 UTF-8，或在找到头部结束标记前缓冲超过
/// 64KiB，都返回 `None`——调用方直接断开连接，不回复、也不 settle。
async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
    }
    String::from_utf8(buffer).ok()
}

/// 取请求行（第一行）按空白切成 method 与 target；格式不完整返回 `None` → 404。
fn request_line(raw: &str) -> Option<(String, String)> {
    let line = raw.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    Some((method, target))
}

/// 路由并执行 `complete`：根据 method/target/query 与共享状态生成 [`Reply`]，不写 socket、
/// 不直接 settle（`finish` 由 `handle_connection` 在写完响应后应用）。
///
/// 顺序是先校验、再 `claimed.swap(true, Ordering::SeqCst)`，最后执行一次
/// `options.complete(code)`。`complete` 由各 provider 提供，可能包含网络请求（例如 token
/// 交换）：它的等待时长会推迟响应写出，返回 `Err` 时响应为 502，并通过 `finish` 让
/// `wait()` 以同一错误失败；`claimed` 的置位则决定后续回调是否得到 409。
async fn route<T: Send + 'static>(
    raw: &str,
    options: &ServerOptions<T>,
    shared: &CallbackShared<T>,
) -> Reply<T> {
    let Some((method, target)) = request_line(raw) else {
        return Reply::page(404, oauth_error_html("Callback route not found.", None));
    };
    let url = match parse_url(&target) {
        Some(url) => url,
        None => return Reply::page(404, oauth_error_html("Callback route not found.", None)),
    };
    // 判定顺序不可交换：方法/路径（404）→ state（400）→ 重复请求（409）→ provider error
    // （400 并 settle）→ 缺 code（400 不 settle）。spec/ai-auth.md §4.4 的表格与此一一对应。
    if method != "GET" || url.path() != options.path {
        return Reply::page(404, oauth_error_html("Callback route not found.", None));
    }
    if let Some(expected) = &options.state
        && query_param(&url, "state").as_deref() != Some(expected.as_str())
    {
        return Reply::page(400, oauth_error_html("State mismatch.", None));
    }
    if shared.claimed.load(Ordering::SeqCst) || shared.settled.load(Ordering::SeqCst) {
        return Reply::page(
            409,
            oauth_error_html("This sign-in has already been handled.", None),
        );
    }
    // provider 的 error 重定向会结束等待（reject）；description 缺省时回退为 error 本身。
    if let Some(error) = query_param(&url, "error")
        && !error.is_empty()
    {
        let description = query_param(&url, "error_description").unwrap_or_else(|| error.clone());
        return Reply {
            status: 400,
            body: oauth_error_html(
                &format!("{} authorization failed.", options.provider_name),
                Some(&description),
            ),
            finish: Some(Err(AuthError::new(format!(
                "{} authorization failed: {}",
                options.provider_name, description
            )))),
        };
    }
    // 缺 code 只回 400，不 claim、不 settle：浏览器刷新/用户重试仍可继续等待。
    let Some(code) = query_param(&url, "code").filter(|code| !code.is_empty()) else {
        return Reply::page(400, oauth_error_html("Missing authorization code.", None));
    };
    // TS 在同步段内检查 `claimed || settled` 后立刻置 `claimed`；这里用 swap 复现同样的
    // 原子性（缺 code / error 的请求不会 claim）。
    if shared.claimed.swap(true, Ordering::SeqCst) || shared.settled.load(Ordering::SeqCst) {
        return Reply::page(
            409,
            oauth_error_html("This sign-in has already been handled.", None),
        );
    }
    match (options.complete)(code).await {
        Ok(value) => Reply {
            status: 200,
            body: oauth_success_html(&format!(
                "Signed in to {}. You may now close this page.",
                options.provider_name
            )),
            finish: Some(Ok(Some(value))),
        },
        Err(error) => Reply {
            status: 502,
            body: oauth_error_html(
                &format!("{} sign-in failed.", options.provider_name),
                Some(&error.message),
            ),
            finish: Some(Err(error)),
        },
    }
}

/// 每个连接一个任务：读请求 → 路由 → 写响应 → 可选 settle。
///
/// 写失败被忽略（浏览器可能提前断开），settle 不受影响；先写完响应再 `finish`，与 TS 的
/// `response.end(...)` 后再 settle 的顺序一致。
async fn handle_connection<T: Send + 'static>(
    mut stream: TcpStream,
    options: Arc<ServerOptions<T>>,
    shared: Arc<CallbackShared<T>>,
) {
    let Some(raw) = read_request(&mut stream).await else {
        return;
    };
    let reply = route(&raw, &options, &shared).await;
    let bytes = http_response(reply.status, &reply.body);
    let _ = stream.write_all(&bytes).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
    if let Some(finish) = reply.finish {
        shared.finish(finish);
    }
}

/// Node `TIMEOUT_MAX`（`2^31 - 1`）：`setTimeout` 的延时不在 `[1, TIMEOUT_MAX]` 内时取 1ms。
const TIMEOUT_MAX_MS: u64 = 2_147_483_647;

/// `setTimeout` 的延时钳制（Node 对超出 32 位有符号整数范围的延时取 1ms，并对 0 取 1ms）。
fn set_timeout_duration(ms: u64) -> Duration {
    if ms == 0 || ms > TIMEOUT_MAX_MS {
        Duration::from_millis(1)
    } else {
        Duration::from_millis(ms)
    }
}

/// `errno` → Node 的 `{ code, reason }`（libuv `uv_err_name` / `uv_strerror` 的常用条目）。
///
/// 只覆盖常见的 socket/地址错误；未列出的统一映射为 `("EUNKNOWN", "unknown error")`。
/// 文本与 code 都参与测试断言（`EADDRINUSE`、`EADDRNOTAVAIL`）。
fn errno_details(errno: i32) -> (&'static str, &'static str) {
    match errno {
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EADDRINUSE => ("EADDRINUSE", "address already in use"),
        libc::EADDRNOTAVAIL => ("EADDRNOTAVAIL", "address not available"),
        libc::EAFNOSUPPORT => ("EAFNOSUPPORT", "address family not supported"),
        libc::EAGAIN => ("EAGAIN", "resource temporarily unavailable"),
        libc::EBADF => ("EBADF", "bad file descriptor"),
        libc::ECONNABORTED => ("ECONNABORTED", "software caused connection abort"),
        libc::ECONNREFUSED => ("ECONNREFUSED", "connection refused"),
        libc::EFAULT => ("EFAULT", "bad address"),
        libc::EINTR => ("EINTR", "interrupted system call"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::EIO => ("EIO", "i/o error"),
        libc::EISCONN => ("EISCONN", "socket is already connected"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::EMSGSIZE => ("EMSGSIZE", "message too long"),
        libc::ENETDOWN => ("ENETDOWN", "network is down"),
        libc::ENETUNREACH => ("ENETUNREACH", "network is unreachable"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::ENOBUFS => ("ENOBUFS", "no buffer space available"),
        libc::ENOMEM => ("ENOMEM", "out of memory"),
        libc::ENOTCONN => ("ENOTCONN", "socket is not connected"),
        libc::ENOTSOCK => ("ENOTSOCK", "socket operation on non-socket"),
        libc::EOPNOTSUPP => ("EOPNOTSUPP", "operation not supported on socket"),
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::EPIPE => ("EPIPE", "broken pipe"),
        libc::EPROTONOSUPPORT => ("EPROTONOSUPPORT", "protocol not supported"),
        libc::EPROTOTYPE => ("EPROTOTYPE", "protocol wrong type for socket"),
        libc::ETIMEDOUT => ("ETIMEDOUT", "connection timed out"),
        _ => ("EUNKNOWN", "unknown error"),
    }
}

/// Node 形状的 socket 错误：`<syscall> <CODE>: <reason>`，并带上 `error.code`。
///
/// 没有 OS errno 时退化为 `<syscall> <io::Error 文本>`，且不设置 `error_code`（调用方只
/// 关心失败本身，测试关心有 errno 时的文案与 code）。
fn node_socket_error(syscall: &str, error: &std::io::Error) -> AuthError {
    match error.raw_os_error() {
        Some(errno) => {
            let (code, reason) = errno_details(errno);
            let mut error = AuthError::new(format!("{syscall} {code}: {reason}"));
            error.error_code = Some(code.to_string());
            error
        }
        None => AuthError::new(format!("{syscall} {error}")),
    }
}

/// `listen` 失败：`listen <CODE>: <reason> <addr>[:<port>]`（Node `UVExceptionWithHostPort`
/// 在端口为 0 时省略 `:port`；地址是**解析后**的 IP，而不是传入的 host）。
///
/// 错误形状与 Node 对齐（测试断言 `error_code == "EADDRINUSE"`）；是否回退由调用方决定。
fn listen_error(error: &std::io::Error, address: &SocketAddr) -> AuthError {
    let mut error = node_socket_error("listen", error);
    error.message.push_str(&format!(" {}", address.ip()));
    if address.port() != 0 {
        error.message.push_str(&format!(":{}", address.port()));
    }
    error
}

/// `sockaddr*` → [`SocketAddr`]。
///
/// 只处理 `AF_INET`/`AF_INET6`；其他地址族或空指针返回 `None`（上游翻译成
/// “did not bind to TCP”）。端口需要从网络字节序转换，IPv4 地址按大端整数解释。
fn sockaddr_to_socket_addr(address: *const libc::sockaddr) -> Option<SocketAddr> {
    if address.is_null() {
        return None;
    }
    // SAFETY: 调用方保证 `address` 指向 `getaddrinfo` 返回链表中的有效 `sockaddr`，
    // 且 `sa_family` 决定后续 cast 的目标类型。
    unsafe {
        match (*address).sa_family as i32 {
            libc::AF_INET => {
                let address = &*address.cast::<libc::sockaddr_in>();
                Some(SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)),
                    u16::from_be(address.sin_port),
                )))
            }
            libc::AF_INET6 => {
                let address = &*address.cast::<libc::sockaddr_in6>();
                Some(SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::from(address.sin6_addr.s6_addr),
                    u16::from_be(address.sin6_port),
                    address.sin6_flowinfo,
                    address.sin6_scope_id,
                )))
            }
            _ => None,
        }
    }
}

/// `getaddrinfo` 失败 → Node 形状的错误：`getaddrinfo <CODE> <host>`（系统错误带 reason）。
///
/// `EAI_SYSTEM` 表示错误码在 `errno` 里（用 `last_os_error()` 取）；`EAI_NONAME` 映射成
/// Node/libuv 的 `ENOTFOUND`，其余未知状态统一 `EAI_FAIL`。
fn gai_error(host: &str, status: i32) -> AuthError {
    if status == libc::EAI_SYSTEM {
        let mut error = node_socket_error("getaddrinfo", &std::io::Error::last_os_error());
        error.message.push_str(&format!(" {host}"));
        return error;
    }
    let code = match status {
        libc::EAI_AGAIN => "EAI_AGAIN",
        libc::EAI_BADFLAGS => "EAI_BADFLAGS",
        libc::EAI_FAIL => "EAI_FAIL",
        libc::EAI_FAMILY => "EAI_FAMILY",
        libc::EAI_MEMORY => "EAI_MEMORY",
        libc::EAI_NODATA => "EAI_NODATA",
        // libuv 把 `UV_EAI_NONAME` 映射为 Node 的 `ENOTFOUND`。
        libc::EAI_NONAME => "ENOTFOUND",
        libc::EAI_SERVICE => "EAI_SERVICE",
        libc::EAI_SOCKTYPE => "EAI_SOCKTYPE",
        _ => "EAI_FAIL",
    };
    let mut error = AuthError::new(format!("getaddrinfo {code} {host}"));
    error.error_code = Some(code.to_string());
    error
}

/// `dns.lookup(host)` 的**首个**地址。
///
/// Node 的 `server.listen(port, host)` 只解析一次并 bind 第一个地址；tokio 的
/// `TcpListener::bind((host, port))` 会遍历全部解析结果，端口占用时静默换地址，
/// 既不符合 TS 的 `EADDRINUSE` 回退判断，也破坏「绝不换端口」的不变量。这里用与
/// libuv 相同的 hints（`AF_UNSPEC` + `SOCK_STREAM`）调用 `getaddrinfo`，只取首个结果。
///
/// 调用本身是阻塞的，`start_oauth_callback_server` 用 `spawn_blocking` 包起来；返回链表
/// 在读完首个 `ai_addr` 后立即 `freeaddrinfo`。
fn resolve_first_address(host: &str, port: u16) -> Result<SocketAddr, AuthError> {
    let Ok(host_c) = CString::new(host) else {
        let mut error = AuthError::new(format!("getaddrinfo ENOTFOUND {host}"));
        error.error_code = Some("ENOTFOUND".into());
        return Err(error);
    };
    let service = CString::new(port.to_string()).expect("port digits contain no NUL");
    // SAFETY: `libc::addrinfo` 是 POD；全零表示各字段取默认值，随后显式写入 family/socktype。
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut result: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: `host_c`/`service` 是有效的 NUL 结尾字符串，`hints` 已清零并填好字段；
    // 成功时 `result` 由 getaddrinfo 分配，下面用 freeaddrinfo 释放。
    let status =
        unsafe { libc::getaddrinfo(host_c.as_ptr(), service.as_ptr(), &hints, &mut result) };
    if status != 0 {
        return Err(gai_error(host, status));
    }
    // SAFETY: `result` 由成功的 `getaddrinfo` 返回，`ai_addr`/`ai_addrlen` 有效。
    let address = unsafe {
        result
            .as_ref()
            .and_then(|first| sockaddr_to_socket_addr(first.ai_addr))
    };
    // SAFETY: `result` 来自成功的 `getaddrinfo`（可能为 null，freeaddrinfo(null) 无害）。
    unsafe { libc::freeaddrinfo(result) };
    address.ok_or_else(|| AuthError::new("OAuth callback server did not bind to TCP"))
}

/// Start the loopback callback server。
///
/// 端口被占用时 **不** 自动换端口（调用方决定回退）。
///
/// 启动顺序（任何一步失败都不会留下监听 socket）：
/// 1. `signal` 已中止 → `"Login cancelled"`（在创建 socket 之前）；
/// 2. 只解析 host 的首个地址并 bind；失败返回 Node 形状错误；
/// 3. 用 `local_addr` 取实际端口（`port: 0` 时由内核分配）；
/// 4. 启动 accept 循环；
/// 5. 注册 abort 监听与超时任务；
/// 6. 用实际端口与 `redirect_host` 拼出 `redirectUri`。
///
/// TS: `src/auth/oauth/callback-server.ts#startOAuthCallbackServer`
pub async fn start_oauth_callback_server<T: Send + 'static>(
    options: OAuthCallbackServerOptions<T>,
) -> Result<OAuthCallbackServer<T>, AuthError> {
    // 预取消在任何 socket 创建之前就失败（spec/ai-auth.md §2.8 步骤 1；
    // openrouter 的测试断言此时不 emit 任何事件）。
    if let Some(signal) = &options.signal
        && signal.is_aborted()
    {
        return Err(AuthError::new("Login cancelled"));
    }

    // TS 的 `server.listen(port, host)` 只解析一次并 bind 第一个地址（见
    // `resolve_first_address`）；绑定失败时绝不换端口，调用方据此回退。
    // spawn_blocking：getaddrinfo 是阻塞调用，不能占住异步运行时的工作线程。
    let host = options.host.clone();
    let port = options.port;
    let requested = tokio::task::spawn_blocking(move || resolve_first_address(&host, port))
        .await
        .map_err(|error| AuthError::new(error.to_string()))??;
    let listener = match TcpListener::bind(requested).await {
        Ok(listener) => listener,
        Err(error) => return Err(listen_error(&error, &requested)),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(_) => {
            return Err(AuthError::new("OAuth callback server did not bind to TCP"));
        }
    };

    let shared = Arc::new(CallbackShared::new());
    let server_options = Arc::new(ServerOptions {
        provider_name: options.provider_name.clone(),
        path: options.path.clone(),
        state: options.state.clone(),
        complete: options.complete,
    });
    let shutdown = CancellationToken::new();
    // accept 循环独占 listener；shutdown 取消后立即返回。每个连接独立 spawn，互不阻塞。
    let accept_shared = Arc::clone(&shared);
    let accept_options = Arc::clone(&server_options);
    let accept_shutdown = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = accept_shutdown.cancelled() => return,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _peer)) => {
                        tokio::spawn(handle_connection(
                            stream,
                            Arc::clone(&accept_options),
                            Arc::clone(&accept_shared),
                        ));
                    }
                    // 绑定成功后的 server 级错误在 Node 里是 `server.on("error")` →
                    // `finish({ error })`：等待方必须被拒绝，而不是无限重试。
                    Err(error) => {
                        accept_shared.finish(Err(node_socket_error("accept", &error)));
                        // 与 libuv 一样保持监听，但避免持续性错误（如 EMFILE）忙等。
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    });

    // 运行中 abort → 让 wait() 以 "Login cancelled" 失败；finish 会移除这个监听。
    // 注意 AbortScope 不向“已中止”的 scope 补发事件：入口预检查在 bind 之前，若恰好在
    // bind 期间 abort，注册时 scope 已中止、监听器永远不会被调用，wait() 不会因此 settle
    // （极窄窗口）。
    if let Some(signal) = &options.signal {
        let abort_shared = Arc::clone(&shared);
        let listener = signal.add_abort_listener(Arc::new(move || {
            abort_shared.finish(Err(AuthError::new("Login cancelled")));
        }));
        *shared
            .abort_listener
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(listener);
    }

    // 超时任务与 finish 竞速：提前 settle 会 cancel `timer_cancel`，任务随之退出；
    // 延时按 Node setTimeout 语义钳制。
    if let Some(timeout_ms) = options.timeout_ms {
        let timeout_shared = Arc::clone(&shared);
        let provider_name = options.provider_name.clone();
        let timer_cancel = shared.timer_cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = timer_cancel.cancelled() => {}
                _ = tokio::time::sleep(set_timeout_duration(timeout_ms)) => {
                    timeout_shared.finish(Err(AuthError::new(format!("{provider_name} sign-in timed out"))));
                }
            }
        });
    }

    // redirectUri 用**实际绑定**的端口；主机优先 redirect_host（例如 localhost），
    // 含 `:` 的主机按 IPv6 字面量加方括号。
    let redirect_host = options
        .redirect_host
        .clone()
        .unwrap_or_else(|| options.host.clone());
    let host_part = if redirect_host.contains(':') {
        format!("[{redirect_host}]")
    } else {
        redirect_host
    };
    let redirect_uri = format!("http://{host_part}:{}{}", address.port(), options.path);

    Ok(OAuthCallbackServer {
        redirect_uri,
        shared,
        shutdown,
    })
}

/// 手动粘贴提示（`waitForCallbackOrManualInput` 的第三个参数）。
///
/// 两个字段原样透传给 `AuthPrompt::ManualCode`；调用方通常把 `redirectUri` 作为
/// placeholder（复制代码流程则用 `code#state` 形状）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackManualPrompt {
    /// 提示文本。
    pub message: String,
    /// 占位符。
    pub placeholder: String,
}

/// `waitForCallbackOrManualInput` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOrManualInput<T> {
    /// 浏览器回调完成，值为 `complete` 的返回值。
    Callback(T),
    /// 用户粘贴的输入（未解析；调用方按自己的 `parseAuthorizationInput` 处理）。
    Manual(String),
}

/// Wait for the browser callback, or for the user to paste the code or redirect URL when the
/// browser cannot reach the loopback server（例如 SSH）。
///
/// 同时启动手动 prompt 与回调等待；手动输入成功 → `callback.cancel()` 并返回 `Manual`；
/// 回调先到 → 返回 `Callback`。
///
/// 错误优先级与 TS 的 microtask 时序一致：`callback.wait()` 自身失败时该错误直接抛出；手动
/// prompt 在回调就绪之前已失败 → 抛出该错误；若回调已就绪且有值，则回调胜出，即使手动 prompt
/// 在同一时刻失败（TS 的 `.then/.catch` 反应比 `await callback.wait()` 续体晚一个 microtask）。
/// 无论如何都 abort 手动 prompt 的 signal（`AuthPrompt.signal`），TUI 侧据此收起输入框。
///
/// 补充：手动 prompt 无论成功还是失败都会 `callback.cancel()`（TS 的 `.then` 与 `.catch`
/// 都调用它），因此 prompt 失败后不会再继续等浏览器。
///
/// TS: `src/auth/oauth/callback-server.ts#waitForCallbackOrManualInput`
pub async fn wait_for_callback_or_manual_input<T: Clone + Send + 'static>(
    interaction: &dyn AuthInteraction,
    callback: Option<&OAuthCallbackServer<T>>,
    prompt: CallbackManualPrompt,
) -> Result<CallbackOrManualInput<T>, AuthError> {
    // 单次 prompt 的取消信号：外部事件胜出后用它收起输入框，不会影响整个登录的 signal。
    let manual_abort = AbortScope::new();
    let request = AuthPromptRequest::with_signal(
        AuthPrompt::ManualCode {
            message: prompt.message,
            placeholder: Some(prompt.placeholder),
        },
        manual_abort.clone(),
    );
    // prompt 结束后（成功或失败）都取消回调等待；失败时错误仍会随 `result` 向上传播。
    let manual = async {
        let result = interaction.prompt(request).await;
        if let Some(callback) = callback {
            callback.cancel();
        }
        result
    };
    tokio::pin!(manual);

    // 竞速细节在 inner；外层只负责创建 prompt，并保证返回前中止它的 signal。
    let result = wait_for_callback_or_manual_input_inner(callback, &mut manual).await;
    manual_abort.abort();
    result
}

/// 竞速的核心：回调与手动 prompt 同时推进。
///
/// 三个槽位的区别：
/// - `manual_result`：回调 settle **之前**手动 prompt 已完成的结果；其中的错误优先于回调
///   值（TS 在 `await callback.wait()` 返回后先检查 `manualError`）；
/// - `callback_result`：回调的原始结果，错误直接向上抛；
/// - `eager_manual_result`：回调已就绪后，为补上“prompt 早已同步启动”而立即 poll 到的
///   结果；它只在回调没有值（被 cancel）时生效，避免把“回调同时就绪时的 prompt 失败”
///   误判成赢家。
async fn wait_for_callback_or_manual_input_inner<T: Clone + Send + 'static>(
    callback: Option<&OAuthCallbackServer<T>>,
    manual: &mut Pin<&mut impl Future<Output = Result<String, AuthError>>>,
) -> Result<CallbackOrManualInput<T>, AuthError> {
    let mut manual_result: Option<Result<String, AuthError>> = None;
    let mut callback_result: Option<Result<Option<T>, AuthError>> = None;
    // 回调就绪后为启动 prompt 而立即观察到的结果；只用于没有回调值时的 `await manual`，
    // 不算作回调之前的 manualError（TS 的 `.then/.catch` 晚一个 microtask）。
    let mut eager_manual_result: Option<Result<String, AuthError>> = None;

    if let Some(callback) = callback {
        loop {
            // 回调分支优先：两者同一轮就绪时，TS 的 `await callback?.wait()` 续体先于手动
            // prompt 的 `.then/.catch` 反应执行，因而回调结果胜出（`biased` 固定轮询顺序）。
            tokio::select! {
                biased;
                value = callback.wait() => {
                    callback_result = Some(value);
                    break;
                }
                result = &mut *manual, if manual_result.is_none() => {
                    manual_result = Some(result);
                }
            }
        }
        // 启动手动 prompt（TS 在 `await callback?.wait()` 之前同步调用 prompt）。立即失败不会
        // 被当作回调之前的 manualError，理由见上方注释。
        // 零时长 `timeout` 只 poll 一次 manual：Ready → 结果已排队；Pending → timeout 返回
        // Err，继续走后面的 `manual.await`。
        if manual_result.is_none()
            && let Ok(result) = tokio::time::timeout(Duration::ZERO, &mut *manual).await
        {
            eager_manual_result = Some(result);
        }
    }

    // `const value = await callback?.wait()`：wait 自身的错误优先于手动 prompt 的错误。
    let callback_value: Option<T> = match callback_result {
        Some(result) => result?,
        None => None,
    };
    // 回调 settle 之前手动 prompt 已失败 → 该错误优先（TS 的 manualError 检查在返回值
    // 判断之前）。
    if let Some(Err(error)) = &manual_result {
        return Err(error.clone());
    }
    if let Some(value) = callback_value {
        return Ok(CallbackOrManualInput::Callback(value));
    }
    // 没有回调值时 TS 会 `await manual`，排队期间就绪的手动结果（包括错误）在这里生效。
    if let Some(result) = eager_manual_result {
        return result.map(CallbackOrManualInput::Manual);
    }
    // 兜底：已排队的 manual_result，或仍在挂起的手动 prompt。
    match manual_result {
        Some(result) => result.map(CallbackOrManualInput::Manual),
        None => manual.await.map(CallbackOrManualInput::Manual),
    }
}
