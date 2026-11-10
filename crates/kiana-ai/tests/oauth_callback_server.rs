//! `packages/ai/test/oauth-callback-server.test.ts` 的 Rust 移植。
//!
//! 浏览器侧请求用 `reqwest`（TS 用 `fetch`）；页面断言只涉及状态码、
//! `content-type` 与关键片段（与 TS 测试同一粒度）。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::oauth::callback_server::{
    CallbackManualPrompt, CallbackOrManualInput, CompleteFn, OAuthCallbackServer,
    OAuthCallbackServerOptions, start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use kiana_ai::auth::types::{AuthError, AuthEvent, AuthInteraction, AuthPromptRequest};
use kiana_ai::utils::abort::AbortScope;
use regex::Regex;
use tokio::sync::{Notify, oneshot};
use url::Url;

// --- 与 TS 测试同形的辅助 ---

fn callback_url(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let mut url = Url::parse(redirect_uri).expect("valid redirect uri");
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
    }
    url.to_string()
}

struct Page {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn page(response: reqwest::Response) -> Page {
    Page {
        status: response.status().as_u16(),
        content_type: response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: response.text().await.expect("response body"),
    }
}

fn echo_complete<T: From<String> + Send + 'static>() -> CompleteFn<T> {
    Arc::new(|code: String| Box::pin(async move { Ok(T::from(format!("completed:{code}"))) }))
}

fn options<T: Send + 'static>(complete: CompleteFn<T>) -> OAuthCallbackServerOptions<T> {
    OAuthCallbackServerOptions {
        provider_name: "Example".into(),
        host: "127.0.0.1".into(),
        port: 0,
        path: "/callback".into(),
        redirect_host: None,
        state: Some("expected-state".into()),
        complete,
        signal: None,
        timeout_ms: None,
    }
}

/// 测试结束时 `close()`（对应 afterEach 里的 servers.splice）。
struct ServerGuard<T>(Option<OAuthCallbackServer<T>>);

impl<T> ServerGuard<T> {
    fn new(server: OAuthCallbackServer<T>) -> Self {
        ServerGuard(Some(server))
    }
}

impl<T> std::ops::Deref for ServerGuard<T> {
    type Target = OAuthCallbackServer<T>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("server present")
    }
}

impl<T> Drop for ServerGuard<T> {
    fn drop(&mut self) {
        if let Some(server) = &self.0 {
            server.close();
        }
    }
}

async fn start<T: Clone + Send + 'static>(
    options: OAuthCallbackServerOptions<T>,
) -> Result<ServerGuard<T>, AuthError> {
    Ok(ServerGuard::new(
        start_oauth_callback_server(options).await?,
    ))
}

// --- OAuth callback server ---

#[tokio::test]
async fn ignores_stray_requests_and_resolves_with_the_completed_code() {
    let server = start(options(echo_complete::<String>())).await.unwrap();
    let redirect_uri = server.redirect_uri().to_string();
    assert!(
        Regex::new(r"^http://127\.0\.0\.1:\d+/callback$")
            .unwrap()
            .is_match(&redirect_uri)
    );
    let client = reqwest::Client::new();

    let wrong_path = page(
        client
            .get(Url::parse(&redirect_uri).unwrap().join("/other").unwrap())
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(wrong_path.status, 404);

    let wrong_state = page(
        client
            .get(callback_url(
                &redirect_uri,
                &[("code", "c"), ("state", "other")],
            ))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(wrong_state.status, 400);
    assert_eq!(
        wrong_state.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert!(wrong_state.body.contains("State mismatch."));

    let post = client
        .post(callback_url(
            &redirect_uri,
            &[("code", "c"), ("state", "expected-state")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status().as_u16(), 404);

    let missing_code = page(
        client
            .get(callback_url(&redirect_uri, &[("state", "expected-state")]))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(missing_code.status, 400);

    let success = page(
        client
            .get(callback_url(
                &redirect_uri,
                &[("code", "the-code"), ("state", "expected-state")],
            ))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(success.status, 200);
    assert_eq!(
        success.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert!(success.body.contains("Authentication successful"));
    assert!(success.body.contains("Signed in to Example."));
    assert!(success.body.contains("fill=\"#F09082\""));
    assert!(success.body.contains("fill=\"#4D9ABF\""));
    assert!(success.body.contains("fill=\"#F1BE58\""));

    assert_eq!(
        server.wait().await.unwrap(),
        Some("completed:the-code".to_string())
    );
}

#[tokio::test]
async fn uses_the_redirect_host_and_skips_the_state_check_when_none_is_expected() {
    let mut opts = options(echo_complete::<String>());
    opts.redirect_host = Some("localhost".into());
    opts.state = None;
    let server = start(opts).await.unwrap();
    let redirect_uri = server.redirect_uri().to_string();
    assert!(
        Regex::new(r"^http://localhost:\d+/callback$")
            .unwrap()
            .is_match(&redirect_uri)
    );
    let response = reqwest::Client::new()
        .get(callback_url(&redirect_uri, &[("code", "no-state")]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        server.wait().await.unwrap(),
        Some("completed:no-state".to_string())
    );
}

#[tokio::test]
async fn shows_completion_failures_on_the_page_and_rejects_the_wait() {
    let mut opts = options::<String>(Arc::new(|_code: String| {
        Box::pin(async move { Err(AuthError::new("token exchange failed")) })
    }));
    opts.state = Some("expected-state".into());
    let server = start(opts).await.unwrap();
    let redirect_uri = server.redirect_uri().to_string();
    let failure = page(
        reqwest::Client::new()
            .get(callback_url(
                &redirect_uri,
                &[("code", "c"), ("state", "expected-state")],
            ))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(failure.status, 502);
    assert!(failure.body.contains("Example sign-in failed."));
    assert!(failure.body.contains("token exchange failed"));
    let error = server.wait().await.unwrap_err();
    assert_eq!(error.message, "token exchange failed");
}

#[tokio::test]
async fn rejects_the_wait_when_the_provider_redirects_with_an_error() {
    let server = start(options(echo_complete::<String>())).await.unwrap();
    let redirect_uri = server.redirect_uri().to_string();
    let failure = page(
        reqwest::Client::new()
            .get(callback_url(
                &redirect_uri,
                &[
                    ("error", "access_denied"),
                    ("error_description", "User denied access"),
                    ("state", "expected-state"),
                ],
            ))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(failure.status, 400);
    assert!(failure.body.contains("User denied access"));
    let error = server.wait().await.unwrap_err();
    assert_eq!(
        error.message,
        "Example authorization failed: User denied access"
    );
}

#[tokio::test]
async fn completes_only_the_first_callback() {
    struct Gate {
        started: Notify,
        sender: Mutex<Option<oneshot::Sender<String>>>,
    }
    let gate = Arc::new(Gate {
        started: Notify::new(),
        sender: Mutex::new(None),
    });
    let complete_gate = Arc::clone(&gate);
    let complete: CompleteFn<String> = Arc::new(move |_code: String| {
        let gate = Arc::clone(&complete_gate);
        Box::pin(async move {
            let (sender, receiver) = oneshot::channel();
            *gate.sender.lock().unwrap() = Some(sender);
            gate.started.notify_one();
            receiver
                .await
                .map_err(|_| AuthError::new("exchange cancelled"))
        })
    });
    let server = start(options(complete)).await.unwrap();
    let url = callback_url(
        server.redirect_uri(),
        &[("code", "c"), ("state", "expected-state")],
    );
    let client = reqwest::Client::new();
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.get(url).send().await.unwrap().status().as_u16() }
    });
    gate.started.notified().await;
    let second = client
        .get(callback_url(
            server.redirect_uri(),
            &[("code", "c"), ("state", "expected-state")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status().as_u16(), 409);
    // A claimed callback keeps completing even when the caller switches to manual input.
    server.cancel();
    gate.sender
        .lock()
        .unwrap()
        .take()
        .expect("exchange pending")
        .send("done".to_string())
        .unwrap();
    assert_eq!(first.await.unwrap(), 200);
    assert_eq!(server.wait().await.unwrap(), Some("done".to_string()));
}

#[tokio::test]
async fn resolves_with_undefined_after_cancel() {
    let server = start(options(echo_complete::<String>())).await.unwrap();
    server.cancel();
    assert_eq!(server.wait().await.unwrap(), None);
    let late = reqwest::Client::new()
        .get(callback_url(
            server.redirect_uri(),
            &[("code", "c"), ("state", "expected-state")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status().as_u16(), 409);
}

#[tokio::test]
async fn rejects_the_wait_on_abort_and_on_timeout() {
    let controller = AbortScope::new();
    let mut opts = options(echo_complete::<String>());
    opts.signal = Some(controller.clone());
    let aborted = start(opts).await.unwrap();
    controller.abort();
    assert_eq!(aborted.wait().await.unwrap_err().message, "Login cancelled");

    let mut opts = options(echo_complete::<String>());
    opts.timeout_ms = Some(10);
    let timed_out = start(opts).await.unwrap();
    assert_eq!(
        timed_out.wait().await.unwrap_err().message,
        "Example sign-in timed out"
    );

    let already_aborted = AbortScope::new();
    already_aborted.abort();
    let mut opts = options(echo_complete::<String>());
    opts.signal = Some(already_aborted);
    let error = match start(opts).await {
        Ok(_) => panic!("start should reject"),
        Err(error) => error,
    };
    assert_eq!(error.message, "Login cancelled");
}

#[tokio::test]
async fn fails_instead_of_picking_another_port_when_the_requested_port_is_taken() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = blocker.local_addr().unwrap().port();
    let mut opts = options(echo_complete::<String>());
    opts.port = port;
    let error = match start(opts).await {
        Ok(_) => panic!("start should reject"),
        Err(error) => error,
    };
    assert_eq!(error.error_code.as_deref(), Some("EADDRINUSE"));
    assert_eq!(
        error.message,
        format!("listen EADDRINUSE: address already in use 127.0.0.1:{port}")
    );
    drop(blocker);
}

#[tokio::test]
async fn binds_only_the_first_resolved_address_for_a_hostname() {
    // Node 的 `dns.lookup("localhost")` 只取首个地址并 bind 它；旧实现用 tokio 的
    // `ToSocketAddrs` 会遍历全部地址，端口在首个地址上被占用时静默换到另一个地址，
    // 既丢掉 `EADDRINUSE` 回退，也可能把浏览器引向占位进程。
    use std::net::ToSocketAddrs;

    let addresses: Vec<std::net::SocketAddr> = ("localhost", 0)
        .to_socket_addrs()
        .expect("resolve localhost")
        .collect();
    let first = addresses[0];
    let blocker = std::net::TcpListener::bind(first).expect("bind the first resolved address");
    let port = blocker.local_addr().expect("blocker address").port();
    let mut opts = options(echo_complete::<String>());
    opts.host = "localhost".into();
    opts.port = port;
    let error = match start(opts).await {
        Ok(_) => panic!("start should reject instead of binding another address"),
        Err(error) => error,
    };
    assert_eq!(error.error_code.as_deref(), Some("EADDRINUSE"));
    assert_eq!(
        error.message,
        format!(
            "listen EADDRINUSE: address already in use {}:{}",
            first.ip(),
            port
        )
    );
    drop(blocker);
}

#[tokio::test]
async fn reports_non_eaddrinuse_bind_failures_with_node_error_shape() {
    // Node: `listen EADDRNOTAVAIL: address not available 192.0.2.1`（端口为 0 时省略 `:port`）。
    let mut opts = options(echo_complete::<String>());
    opts.host = "192.0.2.1".into();
    let error = match start(opts).await {
        Ok(_) => panic!("start should reject"),
        Err(error) => error,
    };
    assert_eq!(error.error_code.as_deref(), Some("EADDRNOTAVAIL"));
    assert_eq!(
        error.message,
        "listen EADDRNOTAVAIL: address not available 192.0.2.1"
    );
}

#[tokio::test]
async fn clamps_huge_timeouts_like_set_timeout() {
    // Node 的 setTimeout 把 > 2^31-1 的延时钳到 1ms（并发出 TimeoutOverflowWarning）。
    let mut opts = options(echo_complete::<String>());
    opts.timeout_ms = Some(u64::MAX);
    let server = start(opts).await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), server.wait())
        .await
        .expect("wait must not sleep for the unclamped timeout")
        .unwrap_err();
    assert_eq!(error.message, "Example sign-in timed out");
}

// --- waitForCallbackOrManualInput ---

type PromptHandler = Box<
    dyn Fn(AuthPromptRequest) -> Pin<Box<dyn Future<Output = Result<String, AuthError>> + Send>>
        + Send
        + Sync,
>;

struct TestInteraction {
    signal: AbortScope,
    handler: PromptHandler,
}

impl TestInteraction {
    fn new(handler: PromptHandler) -> Self {
        TestInteraction {
            signal: AbortScope::new(),
            handler,
        }
    }
}

#[async_trait]
impl AuthInteraction for TestInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
        (self.handler)(request).await
    }

    fn notify(&self, _event: &AuthEvent) {}
}

/// A manual prompt that stays open until its signal aborts.
fn pending_prompt(seen: Arc<Mutex<Option<AbortScope>>>, started: Arc<Notify>) -> PromptHandler {
    Box::new(move |request: AuthPromptRequest| {
        let signal = request
            .signal
            .clone()
            .expect("manual prompt carries a signal");
        *seen.lock().unwrap() = Some(signal.clone());
        started.notify_one();
        Box::pin(async move {
            signal.cancelled().await;
            Err(AuthError::new("prompt aborted"))
        })
    })
}

#[tokio::test]
async fn returns_the_browser_callback_and_aborts_the_manual_prompt() {
    let mut opts = options(echo_complete::<String>());
    opts.state = None;
    let server = start(opts).await.unwrap();
    let seen: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let started = Arc::new(Notify::new());
    let interaction = TestInteraction::new(pending_prompt(Arc::clone(&seen), Arc::clone(&started)));
    let url = callback_url(server.redirect_uri(), &[("code", "from-browser")]);
    let client = reqwest::Client::new();
    let prompt = CallbackManualPrompt {
        message: "paste".into(),
        placeholder: server.redirect_uri().into(),
    };
    let (result, status) = tokio::join!(
        wait_for_callback_or_manual_input(&interaction, Some(&server), prompt),
        async {
            started.notified().await;
            client.get(url).send().await.unwrap().status().as_u16()
        }
    );
    assert_eq!(status, 200);
    assert_eq!(
        result.unwrap(),
        CallbackOrManualInput::Callback("completed:from-browser".to_string())
    );
    assert!(
        seen.lock()
            .unwrap()
            .clone()
            .expect("prompt seen")
            .is_aborted()
    );
}

#[tokio::test]
async fn returns_pasted_input_and_stops_waiting_for_the_browser() {
    let mut opts = options(echo_complete::<String>());
    opts.state = None;
    let server = start(opts).await.unwrap();
    let interaction = TestInteraction::new(Box::new(|_request: AuthPromptRequest| {
        Box::pin(async { Ok("pasted".to_string()) })
    }));
    let result = wait_for_callback_or_manual_input(
        &interaction,
        Some(&server),
        CallbackManualPrompt {
            message: "paste".into(),
            placeholder: server.redirect_uri().into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(result, CallbackOrManualInput::Manual("pasted".to_string()));
}

#[tokio::test]
async fn uses_only_the_manual_prompt_without_a_callback_server() {
    let interaction = TestInteraction::new(Box::new(|_request: AuthPromptRequest| {
        Box::pin(async { Ok("pasted".to_string()) })
    }));
    let result = wait_for_callback_or_manual_input::<String>(
        &interaction,
        None,
        CallbackManualPrompt {
            message: "paste".into(),
            placeholder: "http://localhost/callback".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(result, CallbackOrManualInput::Manual("pasted".to_string()));
}

#[tokio::test]
async fn propagates_manual_prompt_failures() {
    let mut opts = options(echo_complete::<String>());
    opts.state = None;
    let server = start(opts).await.unwrap();
    let interaction = TestInteraction::new(Box::new(|_request: AuthPromptRequest| {
        Box::pin(async { Err(AuthError::new("prompt cancelled")) })
    }));
    let error = wait_for_callback_or_manual_input(
        &interaction,
        Some(&server),
        CallbackManualPrompt {
            message: "paste".into(),
            placeholder: server.redirect_uri().into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "prompt cancelled");
}

#[tokio::test]
async fn returns_the_callback_value_even_when_the_manual_prompt_fails_immediately() {
    // TS 的 `await callback?.wait()` 续体先于手动 prompt 的 `.then/.catch` 反应执行，因此回调
    // 已就绪时其值胜出，即使 prompt 在同一时刻立刻失败（callback-server.ts L174-177）。
    let mut opts = options(echo_complete::<String>());
    opts.state = None;
    let server = start(opts).await.unwrap();
    let client = reqwest::Client::new();
    let url = callback_url(server.redirect_uri(), &[("code", "from-browser")]);
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    // 响应在 `finish()` 之前写出，再等一次 settlement 以保证 `wait()` 已经就绪。
    assert_eq!(
        server.wait().await.unwrap(),
        Some("completed:from-browser".to_string())
    );

    let interaction = TestInteraction::new(Box::new(|_request: AuthPromptRequest| {
        Box::pin(async { Err(AuthError::new("prompt cancelled")) })
    }));
    let result = wait_for_callback_or_manual_input(
        &interaction,
        Some(&server),
        CallbackManualPrompt {
            message: "paste".into(),
            placeholder: server.redirect_uri().into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        CallbackOrManualInput::Callback("completed:from-browser".to_string())
    );
}

#[tokio::test]
async fn propagates_the_manual_prompt_error_when_the_callback_was_cancelled() {
    // 没有回调值时 TS 会 `await manual`，手动 prompt 的错误在该处抛出（即使 wait() 已就绪）。
    let mut opts = options(echo_complete::<String>());
    opts.state = None;
    let server = start(opts).await.unwrap();
    server.cancel();
    let interaction = TestInteraction::new(Box::new(|_request: AuthPromptRequest| {
        Box::pin(async { Err(AuthError::new("prompt cancelled")) })
    }));
    let error = wait_for_callback_or_manual_input(
        &interaction,
        Some(&server),
        CallbackManualPrompt {
            message: "paste".into(),
            placeholder: server.redirect_uri().into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "prompt cancelled");
}
