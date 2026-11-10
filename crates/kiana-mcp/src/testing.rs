//! testing/index.ts：`/testing` 子路径的公开面。
//!
//! TS 源只有一行再导出：
//!
//! ```text
//! export { createInMemoryTransportPair, InMemoryTransport } from "../transports/in-memory.ts";
//! ```
//!
//! 因此这里同样只是再导出，名字恰好两个（`spec/telemetry-mcp.md` §19.2 / §30.24）：
//!
//! - [`create_in_memory_transport_pair`]（TS `createInMemoryTransportPair`）——测试夹具的入口；
//! - [`InMemoryTransport`]——下游测试用它做类型标注，也用它的 `emit_error` 钩子模拟传输层
//!   错误（§30.24：README 只承诺 `createInMemoryTransportPair()`，`emitError` 是 public 的
//!   override 方法，实际可从 `/testing` 子路径访问；Rust 侧保持同样粒度——在模块里 public，
//!   但不进 crate 根）。
//!
//! TS: `packages/mcp/src/testing/index.ts`
//!
//! # 为什么单独成模块
//!
//! 本模块**不**在 `#[cfg(test)]` 下：它编译进普通库，供其他 crate 的集成测试使用
//! （例如 `crates/kiana-builtin-extensions/tests/mcp_connections.rs` 用一对 in-memory 传输
//! 搭假 MCP 服务器）。它只做再导出，类型与实现都在 [`crate::transports::in_memory`]；
//! 与 TS 一样，两个名字都**不**进 crate 根（`src/index.ts` 不含 `/testing` 子路径）。

pub use crate::transports::in_memory::InMemoryTransport;

/// `createInMemoryTransportPair()`：新建一对互相连接的传输（在
/// [`crate::transports::in_memory`] 里定义，这里按 TS 的 `testing/index.ts` 再导出）。
///
/// 返回 [`crate::transports::in_memory::InMemoryTransportPair`]（TS 的 `{ client, server }`，
/// 字段名相同），两侧互相
/// `connect_peer`，但都还**没有** `start`——在 `start` 之前 `send` 会得到
/// `McpConnectionClosedError`。
///
/// ```
/// use kiana_mcp::testing::{InMemoryTransport, create_in_memory_transport_pair};
///
/// let pair = create_in_memory_transport_pair();
/// // 两侧都已连好；再连第三个 peer 会报 TS 的逐字错误。
/// let other = InMemoryTransport::new();
/// assert_eq!(
///     pair.client.connect_peer(&other).unwrap_err().to_string(),
///     "In-memory MCP transport already has a peer",
/// );
/// drop(pair.server);
/// ```
pub use crate::transports::in_memory::create_in_memory_transport_pair;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transports::McpTransport;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn the_pair_round_trips_in_both_directions() {
        let (client, server) = create_in_memory_transport_pair().into_parts();
        let client_seen = Arc::new(AtomicUsize::new(0));
        let server_seen = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&client_seen);
        let _client_subscription = client.on_message(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let counter = Arc::clone(&server_seen);
        let _server_subscription = server.on_message(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        client.start().await.unwrap();
        server.start().await.unwrap();
        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "clientRequest"}))
            .await
            .unwrap();
        server
            .send(json!({"jsonrpc": "2.0", "id": 1, "result": {}}))
            .await
            .unwrap();

        for _ in 0..1_000 {
            if server_seen.load(Ordering::SeqCst) == 1 && client_seen.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(client_seen.load(Ordering::SeqCst), 1);
        assert_eq!(server_seen.load(Ordering::SeqCst), 1);
    }

    /// 下游测试的典型用法：一对传输、服务端同步应答（`client.test.ts` 的 `TestServer`）。
    #[tokio::test]
    async fn a_server_can_answer_a_request_over_the_pair() {
        let (client, server) = create_in_memory_transport_pair().into_parts();
        let replies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&replies);
        let _client_subscription = client.on_message(move |message: &serde_json::Value| {
            sink.lock().unwrap().push(message.clone());
            Ok(())
        });

        server.start().await.unwrap();
        client.start().await.unwrap();
        client
            .send(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
            .await
            .unwrap();
        server
            .send(json!({"jsonrpc": "2.0", "id": 1, "result": {}}))
            .await
            .unwrap();

        for _ in 0..1_000 {
            if !replies.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            *replies.lock().unwrap(),
            vec![json!({"jsonrpc": "2.0", "id": 1, "result": {}})]
        );
    }

    /// `emit_error` 可从 `/testing` 子路径使用（§30.24 的测试钩子）。
    #[tokio::test]
    async fn emit_error_is_reachable_from_the_testing_surface() {
        let (client, _server) = create_in_memory_transport_pair().into_parts();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let _subscription = client.on_error(move |error| {
            sink.lock().unwrap().push(error.to_string());
            Ok(())
        });
        client
            .emit_error(&crate::auth_provider::McpTransportError::new("boom"))
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["boom".to_owned()]);
    }

    /// `InMemoryTransport` 也实现了 `McpTransport`，可直接包成 `Arc<dyn McpTransport>`。
    #[tokio::test]
    async fn the_transport_can_be_used_as_a_trait_object() {
        let (client, server) = create_in_memory_transport_pair().into_parts();
        let client: Arc<dyn McpTransport> = Arc::new(client);
        let server: Arc<dyn McpTransport> = Arc::new(server);
        server.start().await.unwrap();
        client.start().await.unwrap();
        client
            .send(
                json!({"jsonrpc": "2.0", "id": 1, "method": "x"}),
                Default::default(),
            )
            .await
            .unwrap();
        client.close().await.unwrap();
    }
}
