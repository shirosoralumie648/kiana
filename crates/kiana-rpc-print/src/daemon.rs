//! daemon / 多客户端事件服务：把 [`EventFeed`] 暴露成 SSE / WebSocket / REST。
//!
//! 这是 [`event_feed`] 的传输接线层：
//!
//! - `GET /events`（`Accept: text/event-stream` 或普通 GET）→ SSE；
//! - `GET /events` + `Upgrade: websocket` → WebSocket（每帧一条预编码 JSON）；
//! - `GET /events/latest?limit=N` → REST 最新一页 durable 事件，`since` 可直接作为
//!   WebSocket 的 `?since=` 续传游标；
//! - `GET /healthz` → `ok`。
//!
//! # 语义
//!
//! - 每条事件只编码一次（[`FeedEvent`] 已持有 JSON），SSE/WS 共用同一份字节。
//! - 每个连接一个 [`Subscription`]；队列满时 [`EventFeed::publish`] 只移除该订阅，
//!   不会阻塞其它连接或无限增长内存。
//! - `since` 早于 replay log 起点时，SSE 仍然只补可用事件；REST 响应里的
//!   `has_more`/`since` 让客户端可以重新拉取最新页。`SubscribeOutcome::gap` 的
//!   精确序号不在当前帧格式里暴露（见 `event_feed` 的测试），需要时调用方应直接使用
//!   [`EventFeed::subscribe`]。
//! - 新连接第一条事件固定是 `server.connected`（由 `subscribe` 入队）；心跳由
//!   [`DaemonConfig::heartbeat_interval`] 控制，默认 15s。
//!
//! # 默认只监听 loopback
//!
//! [`DaemonConfig::bind_addr`] 默认 `127.0.0.1:0`。daemon 没有内建鉴权，跨主机
//! 暴露必须由调用方在前面加认证/反代；本模块不读取任何凭据。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, UPGRADE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::event_feed::{EventChannel, EventFeed, HEARTBEAT_INTERVAL_SECS, Subscription};

/// daemon 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    /// REST 最新页默认条数。
    pub latest_page_limit: usize,
    /// 心跳间隔。
    pub heartbeat_interval: Duration,
    /// 订阅队列轮询间隔（SSE/WS 写出节奏）。
    pub poll_interval: Duration,
    /// `bind_daemon` 使用的监听地址。
    pub bind_addr: SocketAddr,
    /// 可选的 bearer token。
    ///
    /// `Some` 时 `/events` 与 `/events/latest` 必须携带
    /// `Authorization: Bearer <token>`；浏览器 `EventSource`/`WebSocket` 无法设置
    /// 请求头，也可以用 `?token=<token>`。`/healthz` 保持免鉴权（供进程健康检查）。
    /// `None` 时不校验——调用方仍应只绑定 loopback。
    pub auth_token: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            latest_page_limit: 200,
            heartbeat_interval: Duration::from_secs(HEARTBEAT_INTERVAL_SECS),
            poll_interval: Duration::from_millis(50),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            auth_token: None,
        }
    }
}

type ServerBody = UnsyncBoxBody<Bytes, Infallible>;

fn full_body(bytes: impl Into<Bytes>) -> ServerBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn text_response(status: StatusCode, body: impl Into<Bytes>) -> Response<ServerBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(full_body(body))
        .expect("static response")
}

fn json_response(value: &serde_json::Value) -> Response<ServerBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(full_body(
            serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
        ))
        .expect("json response")
}

/// daemon 服务器。
#[derive(Clone)]
pub struct DaemonServer {
    feed: EventFeed,
    config: Arc<DaemonConfig>,
}

impl DaemonServer {
    /// 新建。
    pub fn new(feed: EventFeed, config: DaemonConfig) -> Self {
        Self {
            feed,
            config: Arc::new(config),
        }
    }

    /// 在已绑定的 listener 上服务，直到 listener 被关闭/任务被取消。
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        loop {
            let (stream, _peer) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                let _ = server.serve_connection(stream).await;
            });
        }
    }

    async fn serve_connection(&self, stream: TcpStream) -> Result<(), hyper::Error> {
        let server = self.clone();
        let service = service_fn(move |request| {
            let server = server.clone();
            async move { Ok::<_, Infallible>(server.handle(request).await) }
        });
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades()
            .await
    }

    async fn handle(&self, request: Request<Incoming>) -> Response<ServerBody> {
        if request.method() != Method::GET {
            return text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        }
        let path = request.uri().path();
        // `/healthz` 有意免鉴权：进程管理器只需要知道进程是否还在。
        if path != "/healthz" && !self.authorized(&request) {
            return text_response(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        match path {
            "/healthz" => text_response(StatusCode::OK, "ok"),
            "/events/latest" => {
                let limit = query_param(request.uri().query(), "limit")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(self.config.latest_page_limit)
                    .clamp(1, 10_000);
                let page = self.feed.latest_page(limit);
                let events: Vec<serde_json::Value> = page
                    .events
                    .iter()
                    .filter_map(|event| serde_json::from_str(event.json()).ok())
                    .collect();
                json_response(&serde_json::json!({
                    "events": events,
                    "since": page.since,
                    "has_more": page.has_more,
                }))
            }
            "/events" => {
                if is_websocket_upgrade(&request) {
                    self.handle_websocket(request).await
                } else {
                    self.handle_sse(request)
                }
            }
            _ => text_response(StatusCode::NOT_FOUND, "not found"),
        }
    }

    /// 校验 bearer token（请求头优先，其次 `?token=`）。
    fn authorized(&self, request: &Request<Incoming>) -> bool {
        let Some(expected) = self.config.auth_token.as_deref() else {
            return true;
        };
        if let Some(header) = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            let header = header.trim();
            if let Some(token) = header
                .strip_prefix("Bearer ")
                .or_else(|| header.strip_prefix("bearer "))
            {
                return constant_time_eq(token.trim(), expected);
            }
        }
        query_param(request.uri().query(), "token")
            .is_some_and(|token| constant_time_eq(&token, expected))
    }

    fn handle_sse(&self, request: Request<Incoming>) -> Response<ServerBody> {
        let since =
            query_param(request.uri().query(), "since").and_then(|value| value.parse().ok());
        let outcome = self.feed.subscribe(since);
        let subscription = outcome.subscription;
        let feed = self.feed.clone();
        let poll_interval = self.config.poll_interval;
        let heartbeat_interval = self.config.heartbeat_interval;

        let stream = futures::stream::unfold(
            (
                subscription,
                feed,
                std::time::Instant::now(),
                poll_interval,
                heartbeat_interval,
            ),
            |(subscription, feed, mut last_heartbeat, poll_interval, heartbeat_interval)| async move {
                loop {
                    if let Some(event) = subscription.try_recv() {
                        let frame = Frame::data(Bytes::from(event.to_sse_frame()));
                        return Some((
                            Ok::<_, Infallible>(frame),
                            (
                                subscription,
                                feed,
                                last_heartbeat,
                                poll_interval,
                                heartbeat_interval,
                            ),
                        ));
                    }
                    if subscription.is_failed() {
                        return None;
                    }
                    tokio::time::sleep(poll_interval).await;
                    let now = std::time::Instant::now();
                    if now.duration_since(last_heartbeat) >= heartbeat_interval {
                        let _ = feed.publish_heartbeat(now_unix_millis());
                        last_heartbeat = now;
                    }
                }
            },
        );

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(StreamBody::new(stream).boxed_unsync())
            .expect("sse response")
    }

    async fn handle_websocket(&self, mut request: Request<Incoming>) -> Response<ServerBody> {
        let key = request
            .headers()
            .get(SEC_WEBSOCKET_KEY)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let Some(key) = key else {
            return text_response(StatusCode::BAD_REQUEST, "missing Sec-WebSocket-Key");
        };
        let accept = derive_accept_key(key.as_bytes());
        let since =
            query_param(request.uri().query(), "since").and_then(|value| value.parse().ok());
        let outcome = self.feed.subscribe(since);
        let subscription = outcome.subscription;
        let poll_interval = self.config.poll_interval;
        // 在返回 101 之前注册 upgrade；hyper 在响应写出后把它解析成 Upgraded。
        let upgrade = hyper::upgrade::on(&mut request);

        tokio::spawn(async move {
            let Ok(upgraded) = upgrade.await else {
                return;
            };
            let websocket =
                WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
            forward_websocket(websocket, subscription, poll_interval).await;
        });

        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(CONNECTION, "upgrade")
            .header(UPGRADE, "websocket")
            .header(SEC_WEBSOCKET_ACCEPT, accept)
            .body(full_body(Bytes::new()))
            .expect("upgrade response")
    }
}

async fn forward_websocket(
    mut websocket: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
    subscription: Subscription,
    poll_interval: Duration,
) {
    loop {
        while let Some(event) = subscription.try_recv() {
            let message = Message::Text(event.to_ws_frame().to_string().into());
            if websocket.send(message).await.is_err() {
                subscription.close();
                return;
            }
        }
        if subscription.is_failed() {
            let _ = websocket.close(None).await;
            break;
        }
        tokio::select! {
            incoming = websocket.next() => {
                match incoming {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = websocket.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                }
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
    subscription.close();
}

fn is_websocket_upgrade(request: &Request<Incoming>) -> bool {
    request
        .headers()
        .get(UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        && request.headers().contains_key(SEC_WEBSOCKET_KEY)
}

/// 与长度无关的逐字节比较，避免 token 校验通过提前返回泄漏前缀。
fn constant_time_eq(provided: &str, expected: &str) -> bool {
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    let mut diff = provided.len() ^ expected.len();
    for index in 0..provided.len().max(expected.len()) {
        let left = provided.get(index).copied().unwrap_or(0);
        let right = expected.get(index).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == name {
            let value = parts.next().unwrap_or("");
            return Some(percent_decode(value));
        }
    }
    None
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn now_unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 绑定 `config.bind_addr` 并启动服务；返回实际地址与任务句柄。
pub async fn bind_daemon(
    feed: EventFeed,
    config: DaemonConfig,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let listener = TcpListener::bind(config.bind_addr).await?;
    let address = listener.local_addr()?;
    let server = DaemonServer::new(feed, config);
    let handle = tokio::spawn(async move { server.serve(listener).await });
    Ok((address, handle))
}

/// 在已绑定的 listener 上启动 daemon。
pub async fn serve_daemon(
    feed: EventFeed,
    listener: TcpListener,
    config: DaemonConfig,
) -> std::io::Result<()> {
    DaemonServer::new(feed, config).serve(listener).await
}

impl DaemonServer {
    /// 供测试/嵌入方直接驱动一个连接（不 accept）。
    #[doc(hidden)]
    pub async fn serve_test_connection(&self, stream: TcpStream) -> Result<(), hyper::Error> {
        self.serve_connection(stream).await
    }
}

/// 把 `AgentSessionEvent` 映射为 daemon 事件时的通道建议：
/// 流式增量用 volatile，最终消息/工具终态/错误用 durable。
pub fn channel_for_event_type(event_type: &str) -> EventChannel {
    if event_type.ends_with(".delta") || event_type.ends_with(".update") {
        EventChannel::Volatile
    } else {
        EventChannel::Durable
    }
}

/// 发布一个已经序列化为 JSON 的事件（避免调用方重复实现编码）。
pub fn publish_json(
    feed: &EventFeed,
    channel: EventChannel,
    event_type: &str,
    data: serde_json::Value,
) -> Result<Arc<crate::event_feed::FeedEvent>, crate::event_feed::FeedError> {
    feed.publish(channel, event_type, &data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn http_get(address: SocketAddr, path: &str, extra: &str) -> String {
        let mut stream = TcpStream::connect(address).await.expect("connect");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).into_owned()
    }

    #[tokio::test]
    async fn health_and_latest_page_are_served_over_real_http() {
        let feed = EventFeed::new();
        let first = feed
            .publish(
                EventChannel::Durable,
                "message.final",
                &json!({"text": "hello"}),
            )
            .expect("publish");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let handle = tokio::spawn(serve_daemon(feed, listener, DaemonConfig::default()));

        let health = http_get(address, "/healthz", "").await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(health.ends_with("ok"));

        let latest = http_get(address, "/events/latest?limit=10", "").await;
        assert!(latest.starts_with("HTTP/1.1 200"), "{latest}");
        let body = latest.split("\r\n\r\n").nth(1).expect("body");
        let value: serde_json::Value = serde_json::from_str(body).expect("json");
        assert_eq!(value["events"].as_array().expect("events").len(), 1);
        assert_eq!(value["events"][0]["type"], "message.final");
        assert_eq!(value["events"][0]["data"]["text"], "hello");
        assert_eq!(value["since"], first.seq());
        handle.abort();
    }

    #[tokio::test]
    async fn auth_token_is_required_and_accepts_header_or_query() {
        let feed = EventFeed::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let handle = tokio::spawn(serve_daemon(
            feed,
            listener,
            DaemonConfig {
                auth_token: Some("s3cret".to_string()),
                ..Default::default()
            },
        ));

        let missing = http_get(address, "/events/latest", "").await;
        assert!(missing.starts_with("HTTP/1.1 401"), "{missing}");

        let wrong = http_get(address, "/events/latest", "Authorization: Bearer nope\r\n").await;
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");

        let header = http_get(
            address,
            "/events/latest",
            "Authorization: Bearer s3cret\r\n",
        )
        .await;
        assert!(header.starts_with("HTTP/1.1 200"), "{header}");

        // EventSource / WebSocket 无法设置请求头，允许 `?token=`。
        let query = http_get(address, "/events/latest?token=s3cret", "").await;
        assert!(query.starts_with("HTTP/1.1 200"), "{query}");

        // 健康检查保持免鉴权。
        let health = http_get(address, "/healthz", "").await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        handle.abort();
    }

    #[tokio::test]
    async fn sse_replays_durable_events_and_emits_connected() {
        let feed = EventFeed::new();
        feed.publish(EventChannel::Durable, "message.final", &json!({"n": 1}))
            .expect("publish");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let handle = tokio::spawn(serve_daemon(
            feed,
            listener,
            DaemonConfig {
                poll_interval: Duration::from_millis(5),
                ..Default::default()
            },
        ));

        let mut stream = TcpStream::connect(address).await.expect("connect");
        stream
            .write_all(b"GET /events?since=0 HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n")
            .await
            .expect("write");
        let mut buffer = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let mut chunk = [0u8; 4096];
            match tokio::time::timeout(Duration::from_millis(200), stream.read(&mut chunk)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(read)) => buffer.extend_from_slice(&chunk[..read]),
                Ok(Err(_)) => break,
                Err(_) => {}
            }
            let text = String::from_utf8_lossy(&buffer);
            if text.contains("event: message.final") && text.contains("event: server.connected") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buffer);
        assert!(text.contains("HTTP/1.1 200"), "{text}");
        assert!(text.contains("content-type: text/event-stream"), "{text}");
        assert!(text.contains("event: server.connected"), "{text}");
        assert!(text.contains("event: message.final"), "{text}");
        assert!(text.contains("\"n\":1"), "{text}");
        handle.abort();
    }

    #[tokio::test]
    async fn websocket_handshake_and_since_replay_work() {
        let feed = EventFeed::new();
        feed.publish(EventChannel::Durable, "message.final", &json!({"n": 7}))
            .expect("publish");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let handle = tokio::spawn(serve_daemon(
            feed,
            listener,
            DaemonConfig {
                poll_interval: Duration::from_millis(5),
                ..Default::default()
            },
        ));

        let (mut websocket, response) =
            tokio_tungstenite::connect_async(format!("ws://{address}/events?since=0"))
                .await
                .expect("ws connect");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        let mut saw_connected = false;
        let mut saw_message = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline && !(saw_connected && saw_message) {
            match tokio::time::timeout(Duration::from_millis(500), websocket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if text.contains("server.connected") {
                        saw_connected = true;
                    }
                    if text.contains("message.final") && text.contains("\"n\":7") {
                        saw_message = true;
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => break,
                Ok(Some(Ok(_))) => {}
                Err(_) => {}
            }
        }
        assert!(saw_connected, "server.connected missing");
        assert!(saw_message, "replayed durable event missing");
        let _ = websocket.close(None).await;
        handle.abort();
    }

    #[test]
    fn query_and_event_type_helpers() {
        assert_eq!(
            query_param(Some("since=4&x=a%20b"), "since"),
            Some("4".into())
        );
        assert_eq!(query_param(Some("x=a+b"), "x"), Some("a b".into()));
        assert_eq!(query_param(None, "since"), None);
        assert_eq!(
            channel_for_event_type("session.delta"),
            EventChannel::Volatile
        );
        assert_eq!(
            channel_for_event_type("tool.completed"),
            EventChannel::Durable
        );
    }
}
