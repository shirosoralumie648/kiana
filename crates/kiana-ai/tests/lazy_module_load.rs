//! `lazy-module-load.test.ts` 的 Rust 等价物。
//!
//! TS 版本用 `node:module` 的 `registerHooks` 断言「导入根 barrel / 构建全部内置 provider /
//! 导入 compat 入口时都不加载 SDK，只有真正 stream 时才加载对应 SDK」。Rust 是静态链接，
//! 没有运行期模块加载；按 `spec/ai-api-anthropic-openai.md` §2.1.5 的等价约束，这里验证
//! `lazy_stream` / `lazy_api` 的「同步返回、异步 setup」语义：
//!
//! - `lazy_stream` 同步返回：`setup()` 的同步前缀（第一个 await 之前）在返回前执行，
//!   其余转发在后台任务里跑（TS 直接同步调用 `setup()`）；
//! - 事件先 `push` 到 outer，再通知 `onEvent`；
//! - `onResult` 恰好一次，且拿到内层 `result()` 覆盖后的结果；
//! - setup 失败 → `error` 事件 + `stopReason: "error"` 的合成消息（消息文本是稳定契约）；
//! - `lazy_api` 在第一次 `stream` / `streamSimple` 之前不调用 `load()`，每次流都调用一次。

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::api::lazy::{
    LazyApiCapabilities, LazyApiLoad, LazyStreamObserver, lazy_api, lazy_stream,
};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::ProviderStreams;
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, Context, DeferredCancelOptions,
    DeferredFetchOptions, DeferredHandle, DoneReason, ErrorReason, Model, SimpleStreamOptions,
    StopReason, StreamOptions, TranscriptContext, Usage,
};

fn model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "test-api".into(),
        provider: "test-provider".into(),
        base_url: "https://example.invalid".into(),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Model::default()
    }
}

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![],
        ..Context::default()
    })
}

fn final_message(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![kiana_ai::types::AssistantContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: text.into(),
                text_signature: None,
            },
        )],
        api: "test-api".into(),
        provider: "test-provider".into(),
        model: "test-model".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp: 1,
        ..AssistantMessage::default()
    }
}

/// 构造一个“内层流”：先推两个事件，再以 `message` 结束。
fn inner_stream(text: &str) -> AssistantMessageEventStream {
    let message = final_message(text);
    let stream = AssistantMessageEventStream::new();
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(message.clone()),
    });
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: Box::new(message.clone()),
    });
    stream.end(message);
    stream
}

#[tokio::test]
async fn lazy_stream_runs_the_setup_synchronous_prefix_before_returning() {
    let started = Arc::new(AtomicUsize::new(0));
    let started_for_setup = started.clone();
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let stream = lazy_stream(
        &model(),
        Box::pin(async move {
            started_for_setup.fetch_add(1, Ordering::SeqCst);
            let _ = gate_rx.await;
            Ok(inner_stream("hello"))
        }),
        None,
    );

    // TS 在 `lazyStream` 里同步调用 `setup()`：第一个 await 之前的同步前缀已经执行
    // （`started` 在返回时就是 1），但流本身还没结束、事件转发仍是异步的。
    assert_eq!(started.load(Ordering::SeqCst), 1);
    gate_tx.send(()).expect("gate");
    let result = stream.result().await.expect("result");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(matches!(
        result.content.first(),
        Some(kiana_ai::types::AssistantContentBlock::Text(text)) if text.text == "hello"
    ));
}

/// 同步（非 tokio）调用方：setup 的前缀同样在 `lazy_stream` 返回前执行，事件转发由专用线程
/// 的 current-thread runtime 驱动完成。
#[test]
fn lazy_stream_runs_setup_prefix_without_an_ambient_runtime() {
    let started = Arc::new(AtomicUsize::new(0));
    let started_for_setup = started.clone();
    let stream = lazy_stream(
        &model(),
        Box::pin(async move {
            started_for_setup.fetch_add(1, Ordering::SeqCst);
            Ok(inner_stream("no-runtime"))
        }),
        None,
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let result = futures::executor::block_on(stream.result()).expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(matches!(
        result.content.first(),
        Some(kiana_ai::types::AssistantContentBlock::Text(text)) if text.text == "no-runtime"
    ));
}

#[tokio::test]
async fn lazy_stream_forwards_events_then_observes_result_once() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let results = Arc::new(Mutex::new(Vec::<Option<AssistantMessage>>::new()));

    let events_for_observer = events.clone();
    let results_for_observer = results.clone();
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |event: &AssistantMessageEvent| {
            events_for_observer
                .lock()
                .unwrap()
                .push(event.kind().to_string());
        })),
        on_result: Some(Box::new(move |result: Option<&AssistantMessage>| {
            results_for_observer.lock().unwrap().push(result.cloned());
        })),
    };

    let stream = lazy_stream(
        &model(),
        Box::pin(async move { Ok(inner_stream("hi")) }),
        Some(observer),
    );

    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event.kind().to_string());
    }
    let result = stream.result().await.expect("result");

    assert_eq!(seen, vec!["start", "done"]);
    assert_eq!(*events.lock().unwrap(), vec!["start", "done"]);
    let results = results.lock().unwrap();
    assert_eq!(results.len(), 1, "onResult is called exactly once");
    assert_eq!(
        results[0].as_ref().map(|message| message.stop_reason),
        Some(StopReason::Stop)
    );
    assert_eq!(result.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn lazy_stream_setup_failure_emits_error_event() {
    let events = Arc::new(Mutex::new(Vec::<AssistantMessageEvent>::new()));
    let results = Arc::new(Mutex::new(Vec::<Option<AssistantMessage>>::new()));
    let events_for_observer = events.clone();
    let results_for_observer = results.clone();
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |event: &AssistantMessageEvent| {
            events_for_observer.lock().unwrap().push(event.clone());
        })),
        on_result: Some(Box::new(move |result: Option<&AssistantMessage>| {
            results_for_observer.lock().unwrap().push(result.cloned());
        })),
    };

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let stream = lazy_stream(
        &model(),
        Box::pin(async move {
            Err(AiError::new(
                AiErrorKind::Auth,
                "No API key for provider: test-provider",
            ))
        }),
        Some(observer),
    );

    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event.kind().to_string());
    }
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    assert_eq!(seen, vec!["error"]);
    {
        let observer_events = events.lock().unwrap();
        assert_eq!(observer_events.len(), 1);
        let AssistantMessageEvent::Error { reason, error } = &observer_events[0] else {
            panic!("observer saw an error event");
        };
        assert_eq!(*reason, ErrorReason::Error);
        assert_eq!(error.role, AssistantRole::Assistant);
        assert_eq!(error.api, "test-api");
        assert_eq!(error.provider, "test-provider");
        assert_eq!(error.model, "test-model");
        assert_eq!(error.stop_reason, StopReason::Error);
        assert_eq!(
            error.error_message.as_deref(),
            Some("No API key for provider: test-provider")
        );
        assert!(error.content.is_empty());
        assert_eq!(error.usage, Usage::default());
        // 允许时钟小幅回拨/跳变（`Date.now()` 语义，不依赖严格单调）。
        assert!(
            (error.timestamp - before).abs() <= 60_000 && (error.timestamp - after).abs() <= 60_000
        );
    }

    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(results.lock().unwrap().len(), 1);
}

/// TS: `observer.onEvent` 抛错会让 `forwardStream` 的 Promise 拒绝，`lazyStream` 的
/// `.catch` 用 `createSetupErrorMessage(model, thrown, startedAt)` 结束外层流。
///
/// node 探针（同一输入）得到：`types: ["start","error"]`、`onEvent: ["start","error"]`、
/// `onResult: ["observer exploded"]`、`errorMessage: "observer exploded"`。
#[tokio::test]
async fn lazy_stream_observer_panic_becomes_the_catch_branch() {
    let observed_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_results = Arc::new(Mutex::new(Vec::<Option<AssistantMessage>>::new()));
    let events_for_observer = observed_events.clone();
    let results_for_observer = observed_results.clone();
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |event: &AssistantMessageEvent| {
            events_for_observer
                .lock()
                .unwrap()
                .push(event.kind().to_string());
            if event.kind() == "start" {
                panic!("observer exploded");
            }
        })),
        on_result: Some(Box::new(move |result: Option<&AssistantMessage>| {
            results_for_observer.lock().unwrap().push(result.cloned());
        })),
    };

    let stream = lazy_stream(
        &model(),
        Box::pin(async move { Ok(inner_stream("hi")) }),
        Some(observer),
    );

    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event.kind().to_string());
    }
    let result = stream.result().await.expect("result");

    // 内层先 push 了 `start`，之后 observer 抛错 → catch 分支补一个 `error` 事件。
    assert_eq!(seen, vec!["start", "error"]);
    assert_eq!(*observed_events.lock().unwrap(), vec!["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("observer exploded"));
    let results = observed_results.lock().unwrap();
    assert_eq!(results.len(), 1, "onResult is called exactly once");
    assert_eq!(
        results[0]
            .as_ref()
            .and_then(|message| message.error_message.as_deref()),
        Some("observer exploded")
    );
}

/// TS: `observer.onResult` 抛错同样让 `forwardStream` 的 Promise 拒绝，进入 `lazyStream`
/// 的 `.catch` 分支。
///
/// node 探针（同一输入）得到：`types: ["start","done"]`（`target.end()` 已把 outer 置为
/// done，catch 里补的 error 事件被 `push` 静默丢弃）、
/// `onEvent: ["start","done","error"]`（catch 直接调用 observer）、
/// `onResult` 只调用一次（第一次即抛错，catch 里的第二次被 `observedResult` 守卫拦下）、
/// 流结果仍是正常的 done 消息。
#[tokio::test]
async fn lazy_stream_on_result_panic_becomes_the_catch_branch() {
    let observed_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed_results = Arc::new(Mutex::new(Vec::<Option<AssistantMessage>>::new()));
    let events_for_observer = observed_events.clone();
    let results_for_observer = observed_results.clone();
    let observer = LazyStreamObserver {
        on_event: Some(Box::new(move |event: &AssistantMessageEvent| {
            events_for_observer
                .lock()
                .unwrap()
                .push(event.kind().to_string());
        })),
        on_result: Some(Box::new(move |result: Option<&AssistantMessage>| {
            results_for_observer.lock().unwrap().push(result.cloned());
            panic!("onResult exploded");
        })),
    };

    let stream = lazy_stream(
        &model(),
        Box::pin(async move { Ok(inner_stream("hi")) }),
        Some(observer),
    );

    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event.kind().to_string());
    }
    let result = stream.result().await.expect("result");

    // 流里看不到 catch 分支补的 error 事件：`done` 之后 `push` 被忽略。
    assert_eq!(seen, vec!["start", "done"]);
    // observer 直接收到该事件。
    assert_eq!(
        *observed_events.lock().unwrap(),
        vec!["start", "done", "error"]
    );
    // 已结束的流保留正常结果，不被合成错误消息覆盖。
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.error_message, None);
    assert!(matches!(
        result.content.first(),
        Some(kiana_ai::types::AssistantContentBlock::Text(text)) if text.text == "hi"
    ));
    // `observedResult` 守卫：onResult 只调用一次（即使它抛错）。
    assert_eq!(observed_results.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// lazy_api
// ---------------------------------------------------------------------------

/// 记录调用次数的假实现。
struct FakeStreams {
    supports_deferred: LazyApiCapabilities,
    cancelled: Arc<AtomicUsize>,
}

#[async_trait]
impl ProviderStreams for FakeStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        inner_stream("stream")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        inner_stream("streamSimple")
    }

    fn has_fetch_deferred(&self) -> bool {
        self.supports_deferred.fetch_deferred
    }

    fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.supports_deferred
            .fetch_deferred
            .then(|| inner_stream("deferred"))
    }

    fn has_cancel_deferred(&self) -> bool {
        self.supports_deferred.cancel_deferred
    }

    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: DeferredCancelOptions,
    ) -> Option<Result<(), AiError>> {
        if !self.supports_deferred.cancel_deferred {
            return None;
        }
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        Some(Ok(()))
    }
}

fn fake_load(
    loads: Arc<AtomicUsize>,
    capabilities: LazyApiCapabilities,
    cancelled: Arc<AtomicUsize>,
) -> LazyApiLoad {
    Arc::new(move || {
        let loads = loads.clone();
        let cancelled = cancelled.clone();
        Box::pin(async move {
            loads.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FakeStreams {
                supports_deferred: capabilities,
                cancelled,
            }) as Arc<dyn ProviderStreams>)
        })
    })
}

#[tokio::test]
async fn lazy_api_does_not_load_until_the_first_stream() {
    let loads = Arc::new(AtomicUsize::new(0));
    let api = lazy_api(
        fake_load(
            loads.clone(),
            LazyApiCapabilities::default(),
            Arc::new(AtomicUsize::new(0)),
        ),
        None,
    );
    // 构造实现本身不加载任何东西。
    assert_eq!(loads.load(Ordering::SeqCst), 0);
    assert!(!api.has_fetch_deferred());
    assert!(!api.has_cancel_deferred());

    let result = api
        .stream_simple(&model(), &context(), SimpleStreamOptions::default())
        .result()
        .await
        .expect("result");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(matches!(
        result.content.first(),
        Some(kiana_ai::types::AssistantContentBlock::Text(text)) if text.text == "streamSimple"
    ));

    let _ = api
        .stream(&model(), &context(), StreamOptions::default())
        .result()
        .await
        .expect("result");
    // 每个流都会调用一次 `load()`（去重由加载器/宿主模块缓存负责），因此计数随流数量增长。
    assert_eq!(loads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn lazy_api_fetch_deferred_reports_missing_implementation() {
    // capabilities 声明了 fetchDeferred，但实现没有 → 流以错误事件结束，消息文本是稳定契约。
    let loads = Arc::new(AtomicUsize::new(0));
    let api = lazy_api(
        fake_load(
            loads.clone(),
            LazyApiCapabilities {
                fetch_deferred: false,
                cancel_deferred: false,
            },
            Arc::new(AtomicUsize::new(0)),
        ),
        Some(LazyApiCapabilities {
            fetch_deferred: true,
            cancel_deferred: false,
        }),
    );
    assert!(api.has_fetch_deferred());
    let handle = DeferredHandle::default();
    let stream = api
        .fetch_deferred(&model(), &handle, DeferredFetchOptions::default())
        .expect("declared");
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("API does not support deferred responses")
    );
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lazy_api_cancel_deferred_reports_missing_implementation() {
    let loads = Arc::new(AtomicUsize::new(0));
    let api = lazy_api(
        fake_load(
            loads.clone(),
            LazyApiCapabilities {
                fetch_deferred: false,
                cancel_deferred: false,
            },
            Arc::new(AtomicUsize::new(0)),
        ),
        Some(LazyApiCapabilities {
            fetch_deferred: false,
            cancel_deferred: true,
        }),
    );
    let handle = DeferredHandle::default();
    let error = api
        .cancel_deferred(&model(), &handle, DeferredCancelOptions::default())
        .await
        .expect("declared")
        .expect_err("not supported by the implementation");
    assert_eq!(error.message, "API cannot cancel deferred responses");
}

#[tokio::test]
async fn lazy_api_declared_deferred_capabilities_are_forwarded() {
    let loads = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let capabilities = LazyApiCapabilities {
        fetch_deferred: true,
        cancel_deferred: true,
    };
    let api = lazy_api(
        fake_load(loads.clone(), capabilities, cancelled.clone()),
        Some(capabilities),
    );
    let handle = DeferredHandle::default();
    let stream = api
        .fetch_deferred(&model(), &handle, DeferredFetchOptions::default())
        .expect("declared");
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Stop);

    api.cancel_deferred(&model(), &handle, DeferredCancelOptions::default())
        .await
        .expect("declared")
        .expect("supported");
    assert_eq!(cancelled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lazy_stream_error_helper_matches_setup_failure() {
    let stream = kiana_ai::api::lazy::lazy_stream_error(
        &model(),
        AiError::new(AiErrorKind::Unsupported, "boom"),
    );
    let result = stream.result().await.expect("result");
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("boom"));
}

// 让 `common` 模块至少被引用一次（与其它测试文件的约定一致）。
#[allow(dead_code)]
fn _uses_common() {
    let _ = common::messages::minimal_tool_result();
}
