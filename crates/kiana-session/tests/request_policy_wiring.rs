//! 第 9 周：`stream_simple_stream_fn` 在发网络前执行请求策略（能力门/预检）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_agent_core::types::BoxFuture;
use kiana_ai::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, ImageContent, ImageKind, Message, Model, ModelInputModality, SimpleStreamOptions,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use kiana_session::agent_session::stream_simple_stream_fn;
use tokio_util::sync::CancellationToken;

/// 计数 mock client：任何真实网络尝试都会让断言失败。
struct CountingClient {
    calls: Arc<AtomicUsize>,
}

impl HttpClient for CountingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HttpError::new("network must not be reached")) })
    }
}

fn text_model() -> Model {
    Model {
        id: "claude-test".to_string(),
        provider: "anthropic".to_string(),
        api: "anthropic-messages".to_string(),
        base_url: "http://127.0.0.1:9".to_string(),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(1_024),
        ..Model::default()
    }
}

fn user_message(content: UserContent) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content,
        timestamp: 0,
    })
}

fn normalized(messages: Vec<Message>) -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("system".to_string()),
        messages,
        tools: None,
    })
}

/// 文本模型收到图片：能力门在 HTTP 前返回 `Unsupported`，请求数为 0。
#[tokio::test]
async fn unsupported_vision_fails_before_any_http_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let context = normalized(vec![user_message(UserContent::Blocks(vec![
        UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
        }),
    ]))]);
    let mut options = SimpleStreamOptions::default();
    options.base.fetch = Some(Arc::new(CountingClient {
        calls: Arc::clone(&calls),
    }));
    options.base.api_key = Some("fake".to_string());
    let error = match stream_simple_stream_fn()(text_model(), context, Some(options)).await {
        Ok(_) => panic!("vision must be rejected before dispatch"),
        Err(error) => error,
    };
    assert_eq!(error.kind, kiana_ai::AiErrorKind::Unsupported, "{error:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// 超窗上下文：预检在 HTTP 前返回 typed `ContextWindowExceeded`，请求数为 0。
#[tokio::test]
async fn context_window_precheck_fails_before_any_http_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut model = text_model();
    model.context_window = Some(100);
    let context = normalized(vec![user_message(UserContent::Text("x".repeat(4_000)))]);
    let mut options = SimpleStreamOptions::default();
    options.base.fetch = Some(Arc::new(CountingClient {
        calls: Arc::clone(&calls),
    }));
    options.base.api_key = Some("fake".to_string());
    let error = match stream_simple_stream_fn()(model, context, Some(options)).await {
        Ok(_) => panic!("oversized input must be rejected before dispatch"),
        Err(error) => error,
    };
    assert_eq!(error.name, "ContextWindowExceeded", "{error:?}");
    assert!(
        error.detail.is_some(),
        "typed error must carry ContextWindowExceeded detail"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
