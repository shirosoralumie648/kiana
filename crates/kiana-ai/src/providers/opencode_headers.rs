//! providers/opencode-headers.ts：OpenCode 的 per-conversation 路由头包装。
//!
//! TS: `packages/ai/src/providers/opencode-headers.ts`
//!
//! # 为什么需要这一层
//!
//! OpenCode 网关用 `x-opencode-session` 识别同一会话并做会话亲和路由；这个值来自请求
//! 选项 `StreamOptions.session_id`（上层会话 id），不是 provider 配置。包装器只做一件
//! 事：在真正分派前把 `session_id` 映射成请求头，其余选项原样透传。它不关心
//! `cache_retention`——即使调用方关闭了缓存保留，头也照常注入（测试
//! `maps_session_id_for_stream_requests_even_without_cache_retention` 锁定该行为）。
//!
//! # 为什么必须显式转发每个方法
//!
//! TS 用 `{...streams, stream, streamSimple}` 保留被包装对象上的其他成员；Rust 的
//! trait 没有“对象展开”，[`ProviderStreams`] 的每个方法都得手写转发。漏掉
//! `fetch_deferred`/`cancel_deferred` 的方法转发会让能力**静默消失**：内层实现的延迟
//! 响应方法再也不会被调用。若连 `has_*` 谓词也不转发，`create_provider` 构造时快照到的
//! 能力为 false，错误文案才会从带 `for "api"` 的
//! `CreatedProvider::unsupported_fetch_deferred` 路径退化成通用错误；只漏方法转发时
//! 快照仍为 true，错误文案保持带 api 名的形式。
//!
//! # 不变量
//!
//! - 调用方已显式提供同名头（大小写不敏感）时不改写，保留调用方的值；值为 `None`
//!   的条目是“抑制默认头”标记，同样原样保留（见 `ProviderHeaders` 的文档）；
//! - `session_id` 缺失或为空串时不生成该头；不做 trim、不做编码；
//! - 包装器本身无可变状态，`Send + Sync` 由内层实现提供。

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::AiError;
use crate::event_stream::AssistantMessageEventStream;
use crate::models::ProviderStreams;
use crate::types::{
    DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, Model, ProviderHeaders,
    SimpleStreamOptions, StreamOptions, TranscriptContext,
};

/// `x-opencode-session`。
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// `hasHeader(headers, name)`：大小写不敏感地检查键是否存在。
///
/// 只比较键，不关心值（`None` 的抑制标记也算“存在”）。逐项做大小写不敏感比较，
/// 不假设键的书写形式。
fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let expected = name.to_lowercase();
    headers.is_some_and(|headers| headers.keys().any(|key| key.to_lowercase() == expected))
}

/// `withSessionHeader(options)`。
///
/// 三种情况下不改动选项、直接返回原值：没有 `session_id`、`session_id` 为空串、调用方
/// 已经提供了同名头（大小写不敏感）。否则在 `headers` 末尾插入
/// `x-opencode-session: <session_id>`。
///
/// 输入按值接收：调用方传入的是本次请求的选项副本，函数要么原样返回，要么重建一个
/// 只改了 `headers` 的结构体，因此不会影响其他请求。
fn with_session_header(options: StreamOptions) -> StreamOptions {
    let session_id = match options.session_id.as_deref() {
        Some(session_id) if !session_id.is_empty() => session_id.to_string(),
        _ => return options,
    };
    if has_header(options.headers.as_ref(), OPENCODE_SESSION_HEADER) {
        return options;
    }
    let mut headers = options.headers.unwrap_or_default();
    headers.insert(OPENCODE_SESSION_HEADER.to_string(), Some(session_id));
    StreamOptions {
        headers: Some(headers),
        ..options
    }
}

/// `withSessionHeader(options)` 的 simple 变体。
///
/// 只改写 `base`（[`SimpleStreamOptions::base`] 才是承载请求头的那一层）；`tool_choice`、
/// `reasoning` 等便捷字段原样保留。
fn with_session_header_simple(options: SimpleStreamOptions) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: with_session_header(options.base),
        ..options
    }
}

/// `withOpenCodeSessionHeader(streams)`：加 OpenCode 必需的 per-conversation 路由头。
///
/// TS 用对象展开保留被包装实现的 `fetchDeferred`/`cancelDeferred`，因此这里转发全部
/// 可选能力方法。
///
/// 这是一个“装饰器”对象：`inner` 是真正发请求的适配器，构造后不再替换；包装器自身
/// 没有可变状态和锁，并发调用只是把各自的选项加工后转交内层，互不影响。
pub struct OpenCodeStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl OpenCodeStreams {
    /// 包装一个实现。
    ///
    /// 不做能力探测；内层的 `has_*` 谓词在调用时原样转发。
    pub fn new(inner: Arc<dyn ProviderStreams>) -> Self {
        OpenCodeStreams { inner }
    }
}

// 转发契约：`stream`/`stream_simple` 先注入会话头；`fetch_deferred`/`cancel_deferred`
// 在 TS 里也没有被包装覆盖（包装对象只改这两个入口），因此这里只是原样转发，不要在
// 延迟请求上“顺手”注入头，否则与 TS 行为不一致。
#[async_trait]
impl ProviderStreams for OpenCodeStreams {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner
            .stream(model, context, with_session_header(options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner
            .stream_simple(model, context, with_session_header_simple(options))
    }

    fn has_fetch_deferred(&self) -> bool {
        self.inner.has_fetch_deferred()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn has_cancel_deferred(&self) -> bool {
        self.inner.has_cancel_deferred()
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Option<Result<(), AiError>> {
        self.inner.cancel_deferred(model, handle, options).await
    }
}

/// `withOpenCodeSessionHeader(streams)`：返回 `Arc<dyn ProviderStreams>`。
///
/// 调用方是 `opencode` / `opencode-go` 的 provider 工厂，它们对每个 `api` 适配器各包
/// 一次。必须先包装再交给 `create_provider`：原 `streams` 不会被修改（`Arc` 共享），
/// 直接使用原对象发请求就不会带会话头。
pub fn with_open_code_session_header(
    streams: Arc<dyn ProviderStreams>,
) -> Arc<dyn ProviderStreams> {
    Arc::new(OpenCodeStreams::new(streams))
}
