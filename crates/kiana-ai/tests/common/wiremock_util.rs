//! `wiremock` 服务器的启动/收尾辅助（TS 测试里由 `vi.stubGlobal("fetch", ...)` 顶替的那一层）。
//!
//! TS 测试大多直接 mock `fetch`；Rust 侧用真实的本地 HTTP 服务器 + `ReqwestHttpClient`
//! （`spec/ai-core.md` §4.8 第 6 条要求默认实现有 e2e 覆盖），因此需要一个统一入口。

use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 启动一个空服务器（默认无匹配即 404，方便断言“没发请求”）。
pub async fn start() -> MockServer {
    MockServer::start().await
}

/// `GET <path>` → 给定状态码与 body（`content-type: application/json`）。
pub async fn mount_json(server: &MockServer, route: &str, status: u16, body: impl Into<String>) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("content-type", super::sse::JSON_CONTENT_TYPE)
                .set_body_string(body.into()),
        )
        .mount(server)
        .await;
}

/// `POST <path>` → 给定状态码、body 与 `content-type`（SSE 用）。
pub async fn mount_body(
    server: &MockServer,
    route: &str,
    status: u16,
    content_type: &str,
    body: impl Into<String>,
) {
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(status)
                .insert_header("content-type", content_type)
                .set_body_string(body.into()),
        )
        .mount(server)
        .await;
}

/// 延迟 `delay` 后才给响应头（测两段式超时）。
pub async fn mount_delayed(
    server: &MockServer,
    route: &str,
    delay: Duration,
    body: impl Into<String>,
) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(delay)
                .insert_header("content-type", super::sse::JSON_CONTENT_TYPE)
                .set_body_string(body.into()),
        )
        .mount(server)
        .await;
}

/// 服务器根 URL（TS 的 `baseUrl`）。
pub fn base_url(server: &MockServer) -> String {
    server.uri()
}

/// `<baseUrl><route>`。
pub fn url(server: &MockServer, route: &str) -> String {
    format!("{}{route}", server.uri())
}
