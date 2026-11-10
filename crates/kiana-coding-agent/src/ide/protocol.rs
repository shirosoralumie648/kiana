//! IDE 协议：MCP Streamable HTTP 传输、`ide/contextUpdate` 与 `ide/openDiff` 往返。
//!
//! 设计参照：
//! - `reference_project/ClaudeCode/src/utils/ide.ts` 的 `callIdeRpc`（JSON-RPC over HTTP +
//!   `Authorization: Bearer`）；
//! - `reference_project/ClaudeCode/src/hooks/useDiffInIDE.ts` 的 `openDiff`：
//!   `ide/diffAccepted` 携带用户最终内容、`ide/diffRejected` 关闭、readOnly 显式降级；
//! - `reference_project/opencode` / `cline` 的 MCP streamable HTTP 客户端（POST JSON-RPC、
//!   响应可能是 `application/json` 或 `text/event-stream`）。
//!
//! # 上下文上限
//!
//! `ide/contextUpdate` 最多 10 个打开文件、payload JSON ≤ 16 KiB。超限时先截断文件内容
//! （按 UTF-8 边界），仍超限则丢弃尾部文件，并在 payload 上标记 `truncated: true`。
//!
//! # diff 状态机
//!
//! 1. `read_only` 请求立即返回 [`IdeError::ReadOnlyDiff`]，**不发送任何请求**、不写文件；
//! 2. 注册一次性 channel，发送 `ide/openDiff`；
//! 3. 响应直接给出 `{accepted, content}` / `{rejected}` 时立即结算；
//! 4. 否则等待 IDE 的 `ide/diffAccepted`（携带用户手工修改后的最终内容）或
//!    `ide/diffRejected`（关闭），300s 超时；
//! 5. 超时/传输失败会注销 channel，不会留下悬挂条目。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::oneshot;

use super::super::web::http::{WebHttpRequest, WebTransport};

/// `contextUpdate` 最多携带的打开文件数。
pub const MAX_CONTEXT_FILES: usize = 10;
/// `contextUpdate` payload 的字节上限（JSON 编码后）。
pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;

/// IDE 协议错误。错误文本绝不包含 `authToken`。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdeError {
    /// 未连接任何 IDE。
    #[error("no IDE is connected")]
    NotConnected,
    /// 传输层失败。
    #[error("IDE transport failed: {0}")]
    Transport(String),
    /// 响应 shape 不符合 JSON-RPC / IDE 协议。
    #[error("IDE returned an invalid response: {0}")]
    InvalidResponse(String),
    /// 等待用户操作超时。
    #[error("IDE request timed out after {0} ms")]
    Timeout(u64),
    /// readOnly diff：明确降级，不静默写。
    #[error("read-only diff is not supported; refusing to write without user confirmation")]
    ReadOnlyDiff,
}

/// 光标/选区位置（0-based line/character）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    /// 行号。
    pub line: u32,
    /// 列号（UTF-16 code unit，与 LSP 一致）。
    pub character: u32,
}

/// 选区。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    /// 起点。
    pub start: Position,
    /// 终点。
    pub end: Position,
}

/// 一个打开的文件。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenFile {
    /// 绝对路径。
    pub path: String,
    /// 当前内容；`None` 表示只有路径（编辑器可按需读取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 语言 id（如 `rust`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_id: Option<String>,
}

/// 编辑器上下文快照。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdeContext {
    /// 打开的文件（按最近使用排序）。
    pub open_files: Vec<OpenFile>,
    /// 当前文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
    /// 光标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Position>,
    /// 选区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    /// 用户是否信任当前 workspace。
    pub trust: bool,
}

/// 经过上限裁剪的 `ide/contextUpdate` payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUpdatePayload {
    /// 最多 [`MAX_CONTEXT_FILES`] 个文件。
    pub open_files: Vec<OpenFile>,
    /// 当前文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
    /// 光标。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Position>,
    /// 选区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<Selection>,
    /// 信任状态。
    pub trust: bool,
    /// 是否发生了文件数/字节数裁剪。
    pub truncated: bool,
}

/// 构造并裁剪 contextUpdate payload。
pub fn build_context_update(context: &IdeContext) -> ContextUpdatePayload {
    let truncated_files = context.open_files.len() > MAX_CONTEXT_FILES;
    let mut payload = ContextUpdatePayload {
        open_files: context
            .open_files
            .iter()
            .take(MAX_CONTEXT_FILES)
            .cloned()
            .collect(),
        current_file: context.current_file.clone(),
        cursor: context.cursor,
        selection: context.selection,
        trust: context.trust,
        truncated: truncated_files,
    };
    if let Ok(bytes) = serde_json::to_vec(&payload)
        && bytes.len() <= MAX_CONTEXT_BYTES
    {
        return payload;
    }

    payload.truncated = true;
    // 先截内容，再丢文件。cap 从 8 KiB 起对半递减；path 等开销仍在时继续丢尾部文件。
    let mut cap = MAX_CONTEXT_BYTES / 2;
    loop {
        for file in &mut payload.open_files {
            if let Some(content) = file.content.as_mut() {
                *content = truncate_to_bytes(content, cap);
            }
        }
        let fits = serde_json::to_vec(&payload)
            .map(|bytes| bytes.len() <= MAX_CONTEXT_BYTES)
            .unwrap_or(false);
        if fits {
            break;
        }
        if cap > 0 {
            cap /= 2;
            continue;
        }
        if payload.open_files.pop().is_none() {
            break;
        }
    }
    if serde_json::to_vec(&payload)
        .map(|bytes| bytes.len() > MAX_CONTEXT_BYTES)
        .unwrap_or(false)
    {
        // 极端情况下（current_file 本身超长）只保留最小骨架。
        payload.open_files.clear();
        if let Some(current) = payload.current_file.take() {
            payload.current_file = Some(truncate_to_bytes(&current, 1024));
        }
    }
    payload
}

fn truncate_to_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// IDE JSON-RPC 传输契约。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait IdeTransport: Send + Sync {
    /// 发送请求并等待 result；`timeout` 覆盖整次调用。
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, IdeError>;

    /// 发送通知（无 id，不等待 result）。
    async fn notify(&self, method: &str, params: Value) -> Result<(), IdeError>;
}

/// SSE 通知接收端（`ide/diffAccepted` / `ide/diffRejected` 等）。
pub type NotificationSink = std::sync::Arc<dyn Fn(&str, &Value) + Send + Sync>;

/// MCP Streamable HTTP 传输：`POST http://127.0.0.1:<port>/mcp` + bearer。
///
/// 响应支持 `application/json` 与 `text/event-stream`（取第一条 `data:` 行）。
pub struct HttpIdeTransport {
    web: std::sync::Arc<dyn WebTransport>,
    endpoint: String,
    auth_token: Option<String>,
    next_id: AtomicU64,
    /// SSE 响应里夹带的服务器通知（如 `ide/diffAccepted`）的接收端。
    notifications: Option<NotificationSink>,
}

impl HttpIdeTransport {
    /// 用 lockfile 的端口与 token 构造，端点固定为 loopback `/mcp`。
    pub fn new(
        web: std::sync::Arc<dyn WebTransport>,
        port: u16,
        auth_token: Option<String>,
    ) -> Self {
        Self::with_endpoint(web, format!("http://127.0.0.1:{port}/mcp"), auth_token)
    }

    /// 自定义端点（测试或非标准 IDE）。
    pub fn with_endpoint(
        web: std::sync::Arc<dyn WebTransport>,
        endpoint: impl Into<String>,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            web,
            endpoint: endpoint.into(),
            auth_token,
            next_id: AtomicU64::new(1),
            notifications: None,
        }
    }

    /// 安装 SSE 通知接收端。
    ///
    /// MCP Streamable HTTP 允许服务器在响应流里插入通知；没有接收端时这些通知会被
    /// 丢弃（只取与请求 id 匹配的响应）。接线层用它在同一连接上处理
    /// `ide/diffAccepted` / `ide/diffRejected`。
    pub fn with_notification_sink(mut self, sink: NotificationSink) -> Self {
        self.notifications = Some(sink);
        self
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
        ];
        if let Some(token) = &self.auth_token {
            headers.push(("authorization".to_string(), format!("Bearer {token}")));
        }
        headers
    }

    async fn post(
        &self,
        payload: Value,
        timeout: Duration,
    ) -> Result<(u16, String, Vec<u8>), IdeError> {
        let body = serde_json::to_vec(&payload)
            .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
        let request = WebHttpRequest {
            method: "POST".to_string(),
            url: self.endpoint.clone(),
            headers: self.headers(),
            body: Some(body),
            timeout_ms: Some(timeout.as_millis().min(u64::MAX as u128) as u64),
            max_response_bytes: 4 * 1024 * 1024,
        };
        let response = self
            .web
            .execute(request)
            .await
            .map_err(|error| IdeError::Transport(error.to_string()))?;
        let content_type = response.content_type().unwrap_or_default();
        Ok((response.status, content_type, response.body))
    }
}

#[async_trait]
impl IdeTransport for HttpIdeTransport {
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, IdeError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (status, content_type, body) = self.post(payload, timeout).await?;
        if status == 401 || status == 403 {
            return Err(IdeError::Transport(format!(
                "IDE authorization failed (HTTP {status})"
            )));
        }
        if !(200..=299).contains(&status) {
            return Err(IdeError::Transport(format!("IDE returned HTTP {status}")));
        }
        let (value, notifications) = parse_rpc_body(&content_type, &body, id)?;
        if let Some(sink) = &self.notifications {
            for (method, params) in &notifications {
                sink(method, params);
            }
        }
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown JSON-RPC error");
            return Err(IdeError::Transport(message.to_string()));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), IdeError> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let (status, _content_type, _body) = self.post(payload, Duration::from_secs(30)).await?;
        if (200..=299).contains(&status) || status == 202 {
            return Ok(());
        }
        if status == 401 || status == 403 {
            return Err(IdeError::Transport(format!(
                "IDE authorization failed (HTTP {status})"
            )));
        }
        Err(IdeError::Transport(format!("IDE returned HTTP {status}")))
    }
}

/// 解析 JSON 或 SSE 形式的 JSON-RPC 响应，并只接受与请求 `id` 匹配的响应。
///
/// MCP Streamable HTTP 的 SSE 流可能在此之前插入服务器通知（没有 `id`）或其它请求的
/// 响应；这里逐条跳过，直到找到本请求的 `id`。
fn parse_rpc_body(
    content_type: &str,
    body: &[u8],
    id: u64,
) -> Result<(Value, Vec<(String, Value)>), IdeError> {
    if content_type.eq_ignore_ascii_case("text/event-stream") {
        let text = String::from_utf8_lossy(body);
        let mut response = None;
        let mut notifications = Vec::new();
        for line in text.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                response = Some(value);
                continue;
            }
            // 没有 id 的 JSON-RPC 消息是服务器通知；不能因为先读到响应就丢掉后面的通知。
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                notifications.push((
                    method.to_string(),
                    value.get("params").cloned().unwrap_or(Value::Null),
                ));
            }
        }
        return match response {
            Some(value) => Ok((value, notifications)),
            None => Err(IdeError::InvalidResponse(format!(
                "SSE response has no data line for request id {id}"
            ))),
        };
    }
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(IdeError::InvalidResponse(format!(
            "JSON-RPC response id does not match request id {id}"
        )));
    }
    Ok((value, Vec::new()))
}

/// 已初始化的 MCP 客户端。
pub struct McpIdeClient {
    transport: std::sync::Arc<dyn IdeTransport>,
    initialized: AtomicBool,
}

impl McpIdeClient {
    /// 包装一个传输实现。
    pub fn new(transport: std::sync::Arc<dyn IdeTransport>) -> Self {
        Self {
            transport,
            initialized: AtomicBool::new(false),
        }
    }

    /// MCP `initialize` + `notifications/initialized`，幂等。
    pub async fn initialize(&self) -> Result<Value, IdeError> {
        if self.initialized.load(Ordering::Relaxed) {
            return Ok(Value::Null);
        }
        let result = self
            .transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "kiana", "version": env!("CARGO_PKG_VERSION")},
                }),
                Duration::from_secs(10),
            )
            .await?;
        self.transport
            .notify("notifications/initialized", json!({}))
            .await?;
        self.initialized.store(true, Ordering::Relaxed);
        Ok(result)
    }

    /// 推送上下文；返回实际发送的裁剪后 payload。
    pub async fn send_context_update(
        &self,
        context: &IdeContext,
    ) -> Result<ContextUpdatePayload, IdeError> {
        let payload = build_context_update(context);
        let value = serde_json::to_value(&payload)
            .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
        self.transport.notify("ide/contextUpdate", value).await?;
        Ok(payload)
    }
}

/// 直接通过传输发送上下文更新（不要求先 `initialize`）。
pub async fn send_context_update(
    transport: &dyn IdeTransport,
    context: &IdeContext,
) -> Result<ContextUpdatePayload, IdeError> {
    let payload = build_context_update(context);
    let value = serde_json::to_value(&payload)
        .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
    transport.notify("ide/contextUpdate", value).await?;
    Ok(payload)
}

/// `ide/openDiff` 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRequest {
    /// 调用方生成的 id；IDE 的 accepted/rejected 通知用它回传。
    pub id: String,
    /// 文件绝对路径。
    pub path: String,
    /// 修改前内容。
    pub old_content: String,
    /// 期望的新内容。
    pub new_content: String,
    /// 只读模式：明确降级，不写文件。
    pub read_only: bool,
}

/// diff 结果；`Accepted` 携带用户最终内容（可能被手工修改过）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOutcome {
    /// 用户接受；内容以 IDE 返回为准。
    Accepted {
        /// 最终内容。
        content: String,
    },
    /// 用户拒绝/关闭。
    Rejected,
}

/// diff 往返 broker：登记一次性 channel，供通知解析。
#[derive(Default)]
pub struct DiffBroker {
    pending: Mutex<HashMap<String, oneshot::Sender<DiffOutcome>>>,
}

impl DiffBroker {
    /// 创建空 broker。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记 id 并返回接收端；重复 id 会覆盖旧条目（旧 receiver 得到 `RecvError`）。
    pub fn register(&self, id: &str) -> oneshot::Receiver<DiffOutcome> {
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("diff broker")
            .insert(id.to_string(), sender);
        receiver
    }

    /// 处理 `ide/diffAccepted`；没有对应 id 时返回 `false`。
    pub fn resolve_accepted(&self, id: &str, content: impl Into<String>) -> bool {
        let sender = self.pending.lock().expect("diff broker").remove(id);
        match sender {
            Some(sender) => sender
                .send(DiffOutcome::Accepted {
                    content: content.into(),
                })
                .is_ok(),
            None => false,
        }
    }

    /// 处理 `ide/diffRejected`；没有对应 id 时返回 `false`。
    pub fn resolve_rejected(&self, id: &str) -> bool {
        let sender = self.pending.lock().expect("diff broker").remove(id);
        match sender {
            Some(sender) => sender.send(DiffOutcome::Rejected).is_ok(),
            None => false,
        }
    }

    fn cancel(&self, id: &str) {
        self.pending.lock().expect("diff broker").remove(id);
    }

    /// 发起 diff 并等待用户结果。
    ///
    /// - `read_only = true`：立即返回 [`IdeError::ReadOnlyDiff`]，不发送请求；
    /// - 响应直接结算，或等待 `ide/diffAccepted`/`ide/diffRejected`；
    /// - 300s 超时返回 [`IdeError::Timeout`]。
    pub async fn open_diff(
        &self,
        transport: &dyn IdeTransport,
        request: DiffRequest,
    ) -> Result<DiffOutcome, IdeError> {
        if request.read_only {
            return Err(IdeError::ReadOnlyDiff);
        }
        let receiver = self.register(&request.id);
        let params = serde_json::to_value(&request)
            .map_err(|error| IdeError::InvalidResponse(error.to_string()))?;
        let response = transport
            .request("ide/openDiff", params, Duration::from_secs(300))
            .await;
        let value = match response {
            Ok(value) => value,
            Err(error) => {
                self.cancel(&request.id);
                return Err(error);
            }
        };
        match interpret_diff_response(&value, &request) {
            Some(outcome) => {
                self.cancel(&request.id);
                Ok(outcome)
            }
            None => match tokio::time::timeout(Duration::from_secs(300), receiver).await {
                Ok(Ok(outcome)) => Ok(outcome),
                Ok(Err(_)) => {
                    self.cancel(&request.id);
                    Err(IdeError::InvalidResponse(
                        "diff notification channel closed".to_string(),
                    ))
                }
                Err(_) => {
                    self.cancel(&request.id);
                    Err(IdeError::Timeout(300_000))
                }
            },
        }
    }
}

fn interpret_diff_response(value: &Value, request: &DiffRequest) -> Option<DiffOutcome> {
    if let Some(accepted) = value.get("accepted").and_then(Value::as_bool) {
        if accepted {
            let content = value
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| request.new_content.clone());
            return Some(DiffOutcome::Accepted { content });
        }
        return Some(DiffOutcome::Rejected);
    }
    if value.get("rejected").and_then(Value::as_bool) == Some(true) {
        return Some(DiffOutcome::Rejected);
    }
    // `pending: true`、`{}`、`null` 都表示“等 IDE 通知”，不是错误。
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::super::web::http::{WebHttpResponse, WebTransportError};
    use super::*;

    #[test]
    fn context_update_caps_files_and_bytes() {
        let files: Vec<OpenFile> = (0..15)
            .map(|index| OpenFile {
                path: format!("/repo/file{index}.rs"),
                content: Some("x".repeat(8_000)),
                language_id: Some("rust".to_string()),
            })
            .collect();
        let context = IdeContext {
            open_files: files,
            current_file: Some("/repo/file0.rs".to_string()),
            cursor: Some(Position {
                line: 1,
                character: 2,
            }),
            selection: Some(Selection {
                start: Position {
                    line: 1,
                    character: 2,
                },
                end: Position {
                    line: 1,
                    character: 5,
                },
            }),
            trust: true,
        };
        let payload = build_context_update(&context);
        assert_eq!(payload.open_files.len(), MAX_CONTEXT_FILES);
        assert!(payload.truncated);
        let bytes = serde_json::to_vec(&payload).expect("json");
        assert!(bytes.len() <= MAX_CONTEXT_BYTES, "{} bytes", bytes.len());
        assert!(payload.open_files.iter().all(|file| {
            file.content
                .as_ref()
                .map(|content| content.len() < 8_000)
                .unwrap_or(true)
        }));
    }

    #[test]
    fn small_context_is_not_truncated() {
        let context = IdeContext {
            open_files: vec![OpenFile {
                path: "/repo/a.rs".to_string(),
                content: Some("fn main() {}".to_string()),
                language_id: None,
            }],
            ..Default::default()
        };
        let payload = build_context_update(&context);
        assert!(!payload.truncated);
        assert_eq!(payload.open_files.len(), 1);
    }

    struct RecordingTransport {
        calls: AtomicUsize,
        response: Value,
        last_request: Mutex<Option<(String, Value)>>,
    }

    impl RecordingTransport {
        fn new(response: Value) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                response,
                last_request: Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl IdeTransport for RecordingTransport {
        async fn request(
            &self,
            method: &str,
            params: Value,
            _timeout: Duration,
        ) -> Result<Value, IdeError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.last_request.lock().expect("lock") = Some((method.to_string(), params));
            Ok(self.response.clone())
        }

        async fn notify(&self, method: &str, params: Value) -> Result<(), IdeError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.last_request.lock().expect("lock") = Some((method.to_string(), params));
            Ok(())
        }
    }

    fn diff_request() -> DiffRequest {
        DiffRequest {
            id: "diff-1".to_string(),
            path: "/repo/a.rs".to_string(),
            old_content: "old".to_string(),
            new_content: "new".to_string(),
            read_only: false,
        }
    }

    #[tokio::test]
    async fn read_only_diff_degrades_explicitly_without_any_request() {
        let transport = RecordingTransport::new(Value::Null);
        let broker = DiffBroker::new();
        let mut request = diff_request();
        request.read_only = true;
        let error = broker
            .open_diff(transport.as_ref(), request)
            .await
            .expect_err("read-only");
        assert!(matches!(error, IdeError::ReadOnlyDiff));
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn accepted_response_returns_user_final_content() {
        let transport = RecordingTransport::new(json!({
            "accepted": true,
            "content": "user edited content"
        }));
        let broker = DiffBroker::new();
        let outcome = broker
            .open_diff(transport.as_ref(), diff_request())
            .await
            .expect("accepted");
        assert_eq!(
            outcome,
            DiffOutcome::Accepted {
                content: "user edited content".to_string()
            }
        );
        let (method, params) = transport
            .last_request
            .lock()
            .expect("lock")
            .clone()
            .expect("request");
        assert_eq!(method, "ide/openDiff");
        assert_eq!(params["id"], "diff-1");
    }

    #[tokio::test]
    async fn accepted_without_content_falls_back_to_proposed_content() {
        let transport = RecordingTransport::new(json!({"accepted": true}));
        let broker = DiffBroker::new();
        let outcome = broker
            .open_diff(transport.as_ref(), diff_request())
            .await
            .expect("accepted");
        assert_eq!(
            outcome,
            DiffOutcome::Accepted {
                content: "new".to_string()
            }
        );
    }

    #[tokio::test]
    async fn rejected_response_is_not_an_error() {
        let transport = RecordingTransport::new(json!({"rejected": true}));
        let broker = DiffBroker::new();
        let outcome = broker
            .open_diff(transport.as_ref(), diff_request())
            .await
            .expect("rejected");
        assert_eq!(outcome, DiffOutcome::Rejected);
    }

    #[tokio::test]
    async fn pending_diff_is_resolved_by_diff_accepted_notification() {
        let transport = RecordingTransport::new(json!({"pending": true}));
        let broker = Arc::new(DiffBroker::new());
        let spawned = {
            let broker = Arc::clone(&broker);
            let transport = Arc::clone(&transport);
            tokio::spawn(async move { broker.open_diff(transport.as_ref(), diff_request()).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(broker.resolve_accepted("diff-1", "final text"));
        let outcome = spawned.await.expect("join").expect("outcome");
        assert_eq!(
            outcome,
            DiffOutcome::Accepted {
                content: "final text".to_string()
            }
        );
        // 重复通知不生效。
        assert!(!broker.resolve_accepted("diff-1", "again"));
    }

    #[tokio::test]
    async fn diff_rejected_notification_resolves_pending() {
        let transport = RecordingTransport::new(json!({}));
        let broker = Arc::new(DiffBroker::new());
        let spawned = {
            let broker = Arc::clone(&broker);
            let transport = Arc::clone(&transport);
            tokio::spawn(async move { broker.open_diff(transport.as_ref(), diff_request()).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(broker.resolve_rejected("diff-1"));
        assert_eq!(
            spawned.await.expect("join").expect("outcome"),
            DiffOutcome::Rejected
        );
    }

    #[tokio::test]
    async fn transport_failure_cancels_pending_entry() {
        struct Failing;
        #[async_trait]
        impl IdeTransport for Failing {
            async fn request(
                &self,
                _method: &str,
                _params: Value,
                _timeout: Duration,
            ) -> Result<Value, IdeError> {
                Err(IdeError::Transport("boom".to_string()))
            }
            async fn notify(&self, _method: &str, _params: Value) -> Result<(), IdeError> {
                Ok(())
            }
        }
        let broker = DiffBroker::new();
        let error = broker
            .open_diff(&Failing, diff_request())
            .await
            .expect_err("transport");
        assert!(matches!(error, IdeError::Transport(_)));
        assert!(
            !broker.resolve_rejected("diff-1"),
            "pending entry must be gone"
        );
    }

    #[derive(Default)]
    struct HttpMock {
        responses: Mutex<Vec<WebHttpResponse>>,
        requests: Mutex<Vec<WebHttpRequest>>,
    }

    impl HttpMock {
        fn new(responses: Vec<WebHttpResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl WebTransport for HttpMock {
        async fn execute(
            &self,
            request: WebHttpRequest,
        ) -> Result<WebHttpResponse, WebTransportError> {
            self.requests.lock().expect("lock").push(request);
            Ok(self.responses.lock().expect("lock").remove(0))
        }
    }

    #[tokio::test]
    async fn http_transport_sends_bearer_and_parses_json_rpc() {
        let mock = HttpMock::new(vec![WebHttpResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#.to_vec(),
            final_url: String::new(),
        }]);
        let transport =
            HttpIdeTransport::new(mock.clone(), 45123, Some("secret-token".to_string()));
        let result = transport
            .request("ide/test", json!({"a": 1}), Duration::from_secs(1))
            .await
            .expect("result");
        assert_eq!(result["ok"], true);
        let requests = mock.requests.lock().expect("lock");
        assert!(
            requests[0].url.ends_with(":45123/mcp"),
            "{}",
            requests[0].url
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer secret-token")
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "accept" && value.contains("text/event-stream"))
        );
    }

    #[tokio::test]
    async fn http_transport_parses_sse_response() {
        let mock = HttpMock::new(vec![WebHttpResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "text/event-stream".to_string())],
            body: b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"ide/contextUpdate\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{\"wrong\":true}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"value\":7}}\n\n"
                .to_vec(),
            final_url: String::new(),
        }]);
        let transport = HttpIdeTransport::new(mock, 45123, None);
        let result = transport
            .request("ide/test", json!({}), Duration::from_secs(1))
            .await
            .expect("result");
        assert_eq!(result["value"], 7);
    }

    #[tokio::test]
    async fn json_rpc_id_mismatch_is_rejected() {
        let mock = HttpMock::new(vec![WebHttpResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: br#"{"jsonrpc":"2.0","id":99,"result":{"wrong":true}}"#.to_vec(),
            final_url: String::new(),
        }]);
        let transport = HttpIdeTransport::new(mock, 45123, None);
        let error = transport
            .request("ide/test", json!({}), Duration::from_secs(1))
            .await
            .expect_err("id mismatch");
        assert!(matches!(error, IdeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn auth_failure_does_not_leak_token() {
        let mock = HttpMock::new(vec![WebHttpResponse {
            status: 401,
            headers: vec![],
            body: b"denied".to_vec(),
            final_url: String::new(),
        }]);
        let transport = HttpIdeTransport::new(mock, 45123, Some("super-secret".to_string()));
        let error = transport
            .request("ide/test", json!({}), Duration::from_secs(1))
            .await
            .expect_err("401");
        assert!(!error.to_string().contains("super-secret"), "{error}");
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[tokio::test]
    async fn context_update_is_sent_as_notification_with_capped_payload() {
        let transport = RecordingTransport::new(Value::Null);
        let context = IdeContext {
            open_files: (0..12)
                .map(|index| OpenFile {
                    path: format!("/repo/{index}.rs"),
                    content: Some("y".repeat(4_000)),
                    language_id: None,
                })
                .collect(),
            trust: true,
            ..Default::default()
        };
        let payload = send_context_update(transport.as_ref(), &context)
            .await
            .expect("sent");
        assert_eq!(payload.open_files.len(), MAX_CONTEXT_FILES);
        assert!(serde_json::to_vec(&payload).expect("json").len() <= MAX_CONTEXT_BYTES);
        let (method, params) = transport
            .last_request
            .lock()
            .expect("lock")
            .clone()
            .expect("request");
        assert_eq!(method, "ide/contextUpdate");
        assert_eq!(
            params["openFiles"].as_array().expect("array").len(),
            MAX_CONTEXT_FILES
        );
    }
}
