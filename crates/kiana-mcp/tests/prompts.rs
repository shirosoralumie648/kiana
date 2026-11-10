//! `prompts/list` / `prompts/get` 客户端契约。
//!
//! 用一个回环传输（`send` 在后台任务里把响应 emit 回同一侧）驱动真实 `McpClient`，
//! 覆盖分页、参数原样下发、消息角色校验与错误文案。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_mcp::auth_provider::McpTransportError;
use kiana_mcp::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription, TransportEvents,
};
use kiana_mcp::{McpClient, McpClientOptions, McpRequestOptions, PromptRole};
use serde_json::{Map, Value, json};

type Responder = Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

struct EchoTransport {
    events: Arc<TransportEvents>,
    responder: Responder,
}

#[async_trait]
impl McpTransport for EchoTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        Ok(())
    }

    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        // 只有请求需要响应；通知（initialize 后的 initialized）忽略。
        if message.get("id").is_none() || message.get("method").is_none() {
            return Ok(());
        }
        let Some(result) = (self.responder)(&message) else {
            return Ok(());
        };
        let id = message["id"].clone();
        let mut reply = Map::new();
        reply.insert("jsonrpc".to_string(), json!("2.0"));
        reply.insert("id".to_string(), id);
        match result {
            Value::Object(mut object) if object.contains_key("error") => {
                reply.insert(
                    "error".to_string(),
                    object.remove("error").unwrap_or(Value::Null),
                );
            }
            other => {
                reply.insert("result".to_string(), other);
            }
        }
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            let _ = events.emit_message(&Value::Object(reply));
        });
        Ok(())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        let _ = self.events.emit_close();
        Ok(())
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.events.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.events.on_close(move || listener())
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": kiana_mcp::LATEST_PROTOCOL_VERSION,
        "capabilities": { "prompts": { "listChanged": true } },
        "serverInfo": { "name": "prompt-server", "version": "1.0.0" }
    })
}

async fn connect_client(responder: Responder) -> McpClient {
    let events = Arc::new(TransportEvents::new());
    let transport = Arc::new(EchoTransport { events, responder });
    let client = McpClient::new(McpClientOptions::default());
    client.connect(transport).await.expect("connect");
    client
}

#[tokio::test]
async fn lists_prompts_across_pages_and_sends_required_arguments_verbatim() {
    let responder: Responder = Arc::new(|request| {
        let method = request["method"].as_str().unwrap_or_default();
        match method {
            "initialize" => Some(initialize_result()),
            "prompts/list" => {
                let cursor = request["params"]["cursor"].as_str();
                match cursor {
                    None => Some(json!({
                        "prompts": [{
                            "name": "summarize",
                            "description": "Summarize a topic",
                            "arguments": [
                                { "name": "topic", "required": true },
                                { "name": "style", "required": false }
                            ]
                        }],
                        "nextCursor": "page-2"
                    })),
                    Some("page-2") => Some(json!({
                        "prompts": [{ "name": "review", "title": "Review" }]
                    })),
                    Some(other) => panic!("unexpected cursor {other}"),
                }
            }
            "prompts/get" => {
                assert_eq!(request["params"]["name"], "summarize");
                assert_eq!(request["params"]["arguments"]["topic"], "MCP");
                assert_eq!(request["params"]["arguments"]["style"], "brief");
                Some(json!({
                    "description": "rendered",
                    "messages": [
                        { "role": "user", "content": { "type": "text", "text": "Summarize MCP briefly" } },
                        { "role": "assistant", "content": { "type": "text", "text": "ok" } }
                    ]
                }))
            }
            other => panic!("unexpected method {other}"),
        }
    });
    let client = connect_client(responder).await;
    let prompts = client
        .list_prompts(&McpRequestOptions::new())
        .await
        .expect("list prompts");
    assert_eq!(
        prompts
            .iter()
            .map(|prompt| prompt.name.as_str())
            .collect::<Vec<_>>(),
        vec!["summarize", "review"]
    );
    assert_eq!(prompts[0].arguments.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        prompts[0].arguments.as_ref().unwrap()[0].required,
        Some(true)
    );
    assert_eq!(prompts[1].title.as_deref(), Some("Review"));

    let mut args = Map::new();
    args.insert("topic".to_string(), json!("MCP"));
    args.insert("style".to_string(), json!("brief"));
    let result = client
        .get_prompt("summarize", Some(args), &McpRequestOptions::new())
        .await
        .expect("get prompt");
    assert_eq!(result.description.as_deref(), Some("rendered"));
    assert_eq!(result.messages.len(), 2);
    assert_eq!(result.messages[0].role, PromptRole::User);
    assert_eq!(result.messages[1].role, PromptRole::Assistant);
    assert_eq!(
        result.messages[0].content.as_value()["text"],
        "Summarize MCP briefly"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn rejects_malformed_prompt_messages_with_a_typed_error() {
    let responder: Responder =
        Arc::new(
            |request| match request["method"].as_str().unwrap_or_default() {
                "initialize" => Some(initialize_result()),
                "prompts/get" => Some(json!({
                    "messages": [
                        { "role": "system", "content": { "type": "text", "text": "nope" } }
                    ]
                })),
                _ => Some(json!({})),
            },
        );
    let client = connect_client(responder).await;
    let error = client
        .get_prompt("broken", None, &McpRequestOptions::new())
        .await
        .expect_err("role system must be rejected");
    assert_eq!(error.name(), "McpError");
    assert_eq!(
        error.to_string(),
        "Invalid message in MCP prompts/get result"
    );

    client.close().await.expect("close");
}
