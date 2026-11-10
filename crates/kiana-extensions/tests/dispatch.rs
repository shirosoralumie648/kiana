//! 事件派发语义（`design-extension-runtime.md` §4.6 / §10.2）。
//!
//! 覆盖 13 个 emitter：正常合并、`undefined` 返回、handler 抛错、多扩展顺序，另加
//! `emitBoundary` 的无效/修复、`emitToolResult` 的 `structuredContent` 删除、
//! `emitMessageEnd` 的角色规则与非 Error 抛出、`emitUserBash` 的 XOR 校验与向上抛、
//! `emitContext` 的原地修改与 leading-system 警告、`emitBeforeProviderHeaders` 的原地修改、
//! `emitInput` 的 handled/transform 链，以及 `snapshotEventHandlers` 的嵌套派发快照。
//!
//! 与 TS 用例的对应：`extensions-runner.test.ts` 的 `error handling`、`before_agent_start`、
//! `boundary chaining`、`tool_result chaining`、`event subscriptions`；
//! `9068-user-bash-fail-closed.test.ts` 的 `it.each` 用例；
//! `extensions-input-event.test.ts`；`3982-message-end-cost-override.test.ts` 的角色规则。
//!
//! `E2e` harness 用真实 loader 加载内联 JS 扩展，覆盖只有跨语言桥才能验证的语义：
//! 共享事件对象的原地修改（headers / input / entries / systemPromptOptions / payload）、
//! `user_bash` 的 `{ operations }` exec 桥、project_trust 的抛出/null、context phase-2
//! 晚快照、handler 30 s deadline。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{ProviderHeaders, TextContent, TextKind, UserContentBlock};
use kiana_config::models::registry::ModelRegistry;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{
    CacheWarmingDecisionEvent, ContextEventResult, CustomToolCallEvent, CustomToolResultEvent,
    ExtensionEvent, ExtensionEventResult, ExtensionHandler, ExtensionHandlerFn, InputEventResult,
    InputSource, MessageEndEvent, MessageEndEventResult, OptionalPatch, ProjectTrustContext,
    ProjectTrustEvent, ProjectTrustEventResult, SessionBeforeSwitchResult, StreamingBehavior,
    ToolCallEvent, ToolCallEventResult, ToolCallEventType, ToolExecutionUpdateEvent,
    ToolResultEvent, ToolResultEventResult, ToolResultEventType, UserBashEvent,
    UserBashEventResult,
};
use kiana_ext_api::types::{Extension, ExtensionActions, ExtensionContextActions, ExtensionError};
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::runner::context;
use kiana_extensions::runner::dispatch;
use kiana_extensions::runner::{
    BoundaryBaseEvent, DispatchError, ExtensionRunner, ResourcePathEntry,
};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use serde_json::{Value as JsonValue, json};

// ===========================================================================
// 测试 harness
// ===========================================================================

/// 最小只读会话管理器（runner 只在构造时保存它）。
#[derive(Default)]
struct FakeSessionManager {
    cwd: String,
}

impl ReadonlySessionManager for FakeSessionManager {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }
    fn get_session_dir(&self) -> &str {
        ""
    }
    fn get_session_id(&self) -> &str {
        "fake-session"
    }
    fn get_session_file(&self) -> Option<&str> {
        None
    }
    fn get_leaf_id(&self) -> Option<&str> {
        None
    }
    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        None
    }
    fn get_entry(&self, _id: &str) -> Option<&SessionEntry> {
        None
    }
    fn get_label(&self, _id: &str) -> Option<&str> {
        None
    }
    fn get_branch(&self, _from_id: Option<&str>) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_session_projection(&self) -> SessionProjection {
        SessionProjection {
            entries: Vec::new(),
            messages: Vec::new(),
            thinking_level: String::new(),
            model: None,
        }
    }
    fn get_header(&self) -> Option<&SessionHeader> {
        None
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

/// 测试 runner：引擎 + `ExtensionRunner` + 错误收集。
struct TestRunner {
    // 先声明 runner，保证先于引擎析构（引擎 drop 后不再有 JS 句柄清理）。
    runner: Arc<ExtensionRunner>,
    /// 保持引擎存活（同线程 drop）。
    _engine: Rc<JsEngine>,
    errors: Arc<Mutex<Vec<ExtensionError>>>,
}

impl TestRunner {
    fn take_errors(&self) -> Vec<ExtensionError> {
        std::mem::take(
            &mut *self
                .errors
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

async fn make_runner(extensions: Vec<Extension>) -> TestRunner {
    let engine = Rc::new(JsEngine::new().expect("engine"));
    engine::install(&engine).expect("install prelude");
    context::install(&engine).expect("install context.js");
    dispatch::install(Rc::clone(&engine)).expect("install dispatch.js");

    use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
        .await
        .expect("model runtime");
    let model_registry = Arc::new(ModelRegistry::new(Arc::new(model_runtime)));
    let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
    let runtime = loader::create_extension_runtime();
    let runner = Arc::new(ExtensionRunner::new(
        extensions,
        runtime,
        PathBuf::from("/tmp/runner-test"),
        session,
        model_registry,
    ));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let _ = runner.on_error(Arc::new(move |error| {
        sink.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(error);
    }));
    TestRunner {
        runner,
        _engine: engine,
        errors,
    }
}

fn source_info(path: &str) -> SourceInfo {
    SourceInfo {
        path: path.to_owned(),
        source: "local".to_owned(),
        scope: SourceScope::Project,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    }
}

fn extension(path: &str, handlers: Vec<(&str, ExtensionHandler)>) -> Extension {
    let mut map: IndexMap<String, Vec<ExtensionHandler>> = IndexMap::new();
    for (event, handler) in handlers {
        map.entry(event.to_owned()).or_default().push(handler);
    }
    Extension {
        path: path.to_owned(),
        resolved_path: path.to_owned(),
        hidden: None,
        replaceable: None,
        source_info: source_info(path),
        handlers: map,
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

type HandlerResult = Result<Option<ExtensionEventResult>, Box<dyn std::error::Error + Send + Sync>>;

fn standard<F>(handler: F) -> ExtensionHandler
where
    F: Fn(
            ExtensionEvent,
            Arc<dyn kiana_ext_api::types::ExtensionContextApi>,
        ) -> LocalBoxFuture<'static, HandlerResult>
        + 'static,
{
    ExtensionHandler::Standard(Arc::new(handler) as ExtensionHandlerFn)
}

fn empty_result(
    _event: ExtensionEvent,
    _ctx: Arc<dyn kiana_ext_api::types::ExtensionContextApi>,
) -> LocalBoxFuture<'static, HandlerResult> {
    Box::pin(async { Ok(None) })
}

fn user_message(text: &str) -> AgentMessage {
    let mut object: IndexMap<String, JsonValue> = IndexMap::new();
    object.insert("role".to_owned(), json!("user"));
    object.insert("content".to_owned(), json!(text));
    object.insert("timestamp".to_owned(), json!(0));
    AgentMessage::Custom(Box::new(object))
}

fn message_end_event(message: AgentMessage) -> MessageEndEvent {
    MessageEndEvent {
        event_type: kiana_ext_api::events::MessageEndEventType::MessageEnd,
        message,
    }
}

fn user_bash_event() -> UserBashEvent {
    UserBashEvent {
        event_type: kiana_ext_api::events::UserBashEventType::UserBash,
        command: "pwd".to_owned(),
        exclude_from_context: false,
        cwd: "/tmp".to_owned(),
    }
}

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

// ===========================================================================
// 泛型 emit
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn generic_emit_takes_the_last_session_before_result_and_respects_order() {
    run_local(async {
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let first = {
            let order = Arc::clone(&order);
            extension(
                "/ext/a.ts",
                vec![(
                    "session_before_switch",
                    standard(move |_event, _ctx| {
                        let order = Arc::clone(&order);
                        Box::pin(async move {
                            order.lock().unwrap().push("a".to_owned());
                            Ok(Some(ExtensionEventResult::SessionBeforeSwitch(
                                SessionBeforeSwitchResult {
                                    cancel: Some(false),
                                },
                            )))
                        })
                    }),
                )],
            )
        };
        let second = {
            let order = Arc::clone(&order);
            extension(
                "/ext/b.ts",
                vec![(
                    "session_before_switch",
                    standard(move |_event, _ctx| {
                        let order = Arc::clone(&order);
                        Box::pin(async move {
                            order.lock().unwrap().push("b".to_owned());
                            Ok(Some(ExtensionEventResult::SessionBeforeSwitch(
                                SessionBeforeSwitchResult { cancel: Some(true) },
                            )))
                        })
                    }),
                )],
            )
        };
        let test = make_runner(vec![first, second]).await;
        let event = ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::BeforeSwitch(
            kiana_ext_api::events::SessionBeforeSwitchEvent {
                event_type:
                    kiana_ext_api::events::SessionBeforeSwitchEventType::SessionBeforeSwitch,
                reason: kiana_ext_api::events::SessionBeforeSwitchReason::New,
                target_session_file: None,
            },
        ));
        let result = test.runner.emit(event).await.expect("emit");
        assert_eq!(*order.lock().unwrap(), vec!["a", "b"]);
        match result {
            Some(ExtensionEventResult::SessionBeforeSwitch(result)) => {
                assert_eq!(result.cancel, Some(true));
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert!(test.take_errors().is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn generic_emit_ignores_undefined_results_and_isolates_throwing_handlers() {
    run_local(async {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let throwing = {
            let calls = Arc::clone(&calls);
            extension(
                "/ext/throws.ts",
                vec![(
                    "agent_end",
                    standard(move |_event, _ctx| {
                        let calls = Arc::clone(&calls);
                        Box::pin(async move {
                            calls.lock().unwrap().push("throws".to_owned());
                            Err(Box::new(std::io::Error::other("Handler error!"))
                                as Box<dyn std::error::Error + Send + Sync>)
                        })
                    }),
                )],
            )
        };
        let silent = {
            let calls = Arc::clone(&calls);
            extension(
                "/ext/silent.ts",
                vec![(
                    "agent_end",
                    standard(move |_event, _ctx| {
                        let calls = Arc::clone(&calls);
                        Box::pin(async move {
                            calls.lock().unwrap().push("silent".to_owned());
                            Ok(None)
                        })
                    }),
                )],
            )
        };
        let test = make_runner(vec![throwing, silent]).await;
        let result = test
            .runner
            .emit(ExtensionEvent::AgentEnd(
                kiana_ext_api::events::AgentEndEvent {
                    event_type: kiana_ext_api::events::AgentEndEventType::AgentEnd,
                    messages: Vec::new(),
                },
            ))
            .await
            .expect("emit");
        assert!(result.is_none());
        assert_eq!(*calls.lock().unwrap(), vec!["throws", "silent"]);
        let errors = test.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].event, "agent_end");
        assert_eq!(errors[0].extension_path, "/ext/throws.ts");
        assert!(errors[0].error.contains("Handler error!"));
    })
    .await;
}

// ===========================================================================
// cache_warming_decision
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn cache_warming_starts_from_event_action_and_last_override_wins() {
    run_local(async {
        let first = extension(
            "/ext/a.ts",
            vec![(
                "cache_warming_decision",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::CacheWarming(
                            kiana_ext_api::CacheWarmingDecisionEventResult {
                                action: Some(kiana_config::cache::warmer::CacheWarmingAction::Warm),
                            },
                        )))
                    })
                }),
            )],
        );
        let second = extension(
            "/ext/b.ts",
            vec![(
                "cache_warming_decision",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::CacheWarming(
                            kiana_ext_api::CacheWarmingDecisionEventResult {
                                action: Some(kiana_config::cache::warmer::CacheWarmingAction::Stop),
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![first, second]).await;
        let event = CacheWarmingDecisionEvent {
            warm_cost: 1.0,
            miss_cost: 2.0,
            continuation_probability: 0.5,
            action: kiana_config::cache::warmer::CacheWarmingAction::Warm,
        };
        let action = test
            .runner
            .emit_cache_warming_decision(event)
            .await
            .expect("decision");
        assert_eq!(
            action,
            kiana_config::cache::warmer::CacheWarmingAction::Stop
        );
    })
    .await;
}

// ===========================================================================
// JS 层 harness：fake host（合并语义的逐行验证；含原地修改）
// ===========================================================================

/// 只装 prelude + `dispatch.js` 的引擎；用 JS 写的 fake host 直接驱动 13 个 emitter。
struct JsHarness {
    engine: Rc<JsEngine>,
}

impl JsHarness {
    fn new() -> Self {
        let engine = Rc::new(JsEngine::new().expect("engine"));
        engine::install(&engine).expect("install prelude");
        dispatch::install(Rc::clone(&engine)).expect("install dispatch.js");
        let harness = Self { engine };
        harness.eval::<()>(FAKE_HOST);
        harness
    }

    fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        match self.engine.eval_script::<T>(source, "<test>") {
            Ok(value) => value,
            Err(error) => panic!("eval failed: {error:?}"),
        }
    }

    /// 求值一段返回 Promise 的脚本并驱动到 settle，返回 JSON。
    fn eval_json(&self, source: &str) -> JsonValue {
        self.engine.with_js(|ctx| {
            // 顶层 await：`eval_promise` 不会替我们 await 脚本返回的 Promise。
            let wrapped = format!("await ({source});");
            let promise: rquickjs::Promise<'_> = match ctx.eval_promise(wrapped) {
                Ok(promise) => promise,
                Err(error) => panic!("eval failed: {error:?}"),
            };
            loop {
                if let Some(result) = promise.result::<rquickjs::Value<'_>>() {
                    match result {
                        Ok(value) => {
                            let text = ctx
                                .json_stringify(value)
                                .expect("stringify")
                                .map(|text| text.to_string().expect("utf8"))
                                .unwrap_or_else(|| "null".to_owned());
                            let value: JsonValue =
                                serde_json::from_str(&text).unwrap_or(JsonValue::Null);
                            // QuickJS 的顶层 await 求值把完成值包在 `{ value }` 里。
                            return match value {
                                JsonValue::Object(mut object)
                                    if object.len() == 1 && object.contains_key("value") =>
                                {
                                    object.shift_remove("value").unwrap_or(JsonValue::Null)
                                }
                                other => other,
                            };
                        }
                        Err(_) => {
                            let value = ctx.catch();
                            let text = ctx
                                .json_stringify(value)
                                .ok()
                                .flatten()
                                .map(|text| text.to_string().unwrap_or_default())
                                .unwrap_or_default();
                            panic!("promise rejected: {text}");
                        }
                    }
                }
                if !ctx.execute_pending_job() {
                    panic!("promise did not settle");
                }
            }
        })
    }
}

const FAKE_HOST: &str = r#"
(function () {
  globalThis.__reports = [];
  globalThis.__handlers = [];
  globalThis.__builds = [];
  globalThis.__buildContext = null;
  globalThis.__kiana_dispatch_host = {
    call(token, hid, event) {
      const entry = globalThis.__handlers[hid];
      if (typeof entry !== "function") return Promise.resolve({ hasValue: false, value: null });
      return Promise.resolve(entry(event)).then(
        (value) => (value === undefined ? { hasValue: false, value: null } : { hasValue: true, value: value }),
      );
    },
    buildContext(token, entries) {
      globalThis.__builds.push(entries);
      const build = globalThis.__buildContext;
      if (typeof build === "function") return Promise.resolve(build(entries));
      return Promise.resolve({
        contextEntries: [],
        contextMessages: [],
        llmMessages: [],
        pendingMessages: [],
        canContinue: false,
      });
    },
    reportError(payload) {
      globalThis.__reports.push(payload);
    },
    setSystemPrompt(token, prompt) {
      globalThis.__systemPrompt = prompt;
    },
  };
})();
"#;

fn state(event_type: &str, event: &str, handler_count: usize) -> String {
    let snapshot = (0..handler_count)
        .map(|id| format!("[{id}]"))
        .collect::<Vec<_>>()
        .join(",");
    let paths = (0..handler_count)
        .map(|id| format!("\"/ext/{id}.ts\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{ token: 1, type: \"{event_type}\", paths: [{paths}], snapshot: [{snapshot}], event: {event} }}"
    )
}

// ===========================================================================
// message_end
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn message_end_chains_messages_and_reports_role_mismatches() {
    run_local(async {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let first = extension(
            "/ext/a.ts",
            vec![(
                "message_end",
                standard(move |event, _ctx| {
                    let calls = Arc::clone(&first_calls);
                    Box::pin(async move {
                        let ExtensionEvent::MessageEnd(event) = event else {
                            panic!("expected message_end");
                        };
                        calls.lock().unwrap().push(event.message.role().to_owned());
                        Ok(Some(ExtensionEventResult::MessageEnd(
                            MessageEndEventResult {
                                message: Some(user_message("changed by a")),
                            },
                        )))
                    })
                }),
            )],
        );
        let second = extension(
            "/ext/b.ts",
            vec![(
                "message_end",
                standard(move |event, _ctx| {
                    let calls = Arc::clone(&second_calls);
                    Box::pin(async move {
                        let ExtensionEvent::MessageEnd(event) = event else {
                            panic!("expected message_end");
                        };
                        calls.lock().unwrap().push(event.message.role().to_owned());
                        // 角色不同：报错并继续；这条修改被忽略。
                        let mut object: IndexMap<String, JsonValue> = IndexMap::new();
                        object.insert("role".to_owned(), json!("assistant"));
                        object.insert("content".to_owned(), json!("wrong role"));
                        Ok(Some(ExtensionEventResult::MessageEnd(
                            MessageEndEventResult {
                                message: Some(AgentMessage::Custom(Box::new(object))),
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![first, second]).await;
        let result = test
            .runner
            .emit_message_end(message_end_event(user_message("original")))
            .await
            .expect("message_end");
        assert_eq!(*calls.lock().unwrap(), vec!["user", "user"]);
        let message = result.expect("modified");
        assert_eq!(message.role(), "user");
        let errors = test.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].error,
            "message_end handlers must return a message with the same role"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn message_end_returns_none_when_no_handler_modifies_the_message() {
    run_local(async {
        let ext = extension(
            "/ext/noop.ts",
            vec![("message_end", standard(empty_result))],
        );
        let test = make_runner(vec![ext]).await;
        let result = test
            .runner
            .emit_message_end(message_end_event(user_message("same")))
            .await
            .expect("message_end");
        assert!(result.is_none());
    })
    .await;
}

// ===========================================================================
// tool_result
// ===========================================================================

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })
}

fn custom_tool_result_event(content: Vec<UserContentBlock>) -> ToolResultEvent {
    ToolResultEvent::Custom(CustomToolResultEvent {
        event_type: ToolResultEventType::ToolResult,
        tool_call_id: "call-1".to_owned(),
        parent_tool_call_id: None,
        input: IndexMap::new(),
        content,
        structured_content: Some(json!({ "initial": true })),
        is_error: false,
        usage: None,
        tool_name: "my_tool".to_owned(),
        details: Some(json!({ "initial": true })),
    })
}

#[tokio::test(flavor = "current_thread")]
async fn tool_result_chains_content_and_deletes_unmatched_structured_content() {
    run_local(async {
        let observed = Arc::new(Mutex::new(Vec::<usize>::new()));
        let append = |text: &'static str, observed: Arc<Mutex<Vec<usize>>>| {
            extension(
                "/ext/tool-result.ts",
                vec![(
                    "tool_result",
                    standard(move |event, _ctx| {
                        let observed = Arc::clone(&observed);
                        Box::pin(async move {
                            let ExtensionEvent::ToolResult(ToolResultEvent::Custom(event)) = event
                            else {
                                panic!("expected custom tool_result");
                            };
                            observed.lock().unwrap().push(event.content.len());
                            let mut content = event.content.clone();
                            content.push(text_block(text));
                            Ok(Some(ExtensionEventResult::ToolResult(
                                ToolResultEventResult {
                                    content: OptionalPatch::Value(content),
                                    ..ToolResultEventResult::default()
                                },
                            )))
                        })
                    }),
                )],
            )
        };
        let first = append("ext1", Arc::clone(&observed));
        let second = append("ext2", Arc::clone(&observed));
        let test = make_runner(vec![first, second]).await;
        let result = test
            .runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("base")]))
            .await
            .expect("tool_result")
            .expect("modified");
        assert_eq!(*observed.lock().unwrap(), vec![1, 2]);
        let content = result.content.value().expect("content patch");
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], text_block("base"));
        assert_eq!(content[2], text_block("ext2"));
        // details 未被补丁覆盖：保留初始值；structuredContent 因内容替换被删除。
        assert_eq!(result.details.value(), Some(&json!({ "initial": true })));
        assert!(result.structured_content.is_absent());
        // TS 的返回对象总是带全部 5 个键（值为当前事件的值）。
        assert_eq!(result.is_error.value(), Some(&false));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_result_keeps_previous_patches_under_partial_updates() {
    run_local(async {
        let details = extension(
            "/ext/details.ts",
            vec![(
                "tool_result",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ToolResult(
                            ToolResultEventResult {
                                details: OptionalPatch::Value(json!({ "patched": true })),
                                ..ToolResultEventResult::default()
                            },
                        )))
                    })
                }),
            )],
        );
        let error = extension(
            "/ext/error.ts",
            vec![(
                "tool_result",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ToolResult(
                            ToolResultEventResult {
                                is_error: OptionalPatch::Value(true),
                                ..ToolResultEventResult::default()
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![details, error]).await;
        let result = test
            .runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("base")]))
            .await
            .expect("tool_result")
            .expect("modified");
        assert_eq!(result.details.value(), Some(&json!({ "patched": true })));
        assert_eq!(result.is_error.value(), Some(&true));
        assert_eq!(result.content.value().map(Vec::len), Some(1));
        // content 未被替换：structuredContent 保持原值。
        assert_eq!(
            result.structured_content.value(),
            Some(&json!({ "initial": true }))
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_result_isolates_throwing_handlers() {
    run_local(async {
        let throws = extension(
            "/ext/throws.ts",
            vec![(
                "tool_result",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("tool result failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        );
        let applies = extension(
            "/ext/applies.ts",
            vec![(
                "tool_result",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ToolResult(
                            ToolResultEventResult {
                                details: OptionalPatch::Value(json!(null)),
                                ..ToolResultEventResult::default()
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![throws, applies]).await;
        let result = test
            .runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("base")]))
            .await
            .expect("tool_result")
            .expect("modified");
        assert_eq!(result.details.value(), Some(&JsonValue::Null));
        let errors = test.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "tool result failed");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_result_returns_none_without_modifications() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/noop.ts",
            vec![("tool_result", standard(empty_result))],
        )])
        .await;
        let result = test
            .runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("base")]))
            .await
            .expect("tool_result");
        assert!(result.is_none());
    })
    .await;
}

// ===========================================================================
// tool_call
// ===========================================================================

fn custom_tool_call_event() -> ToolCallEvent {
    ToolCallEvent::Custom(CustomToolCallEvent {
        event_type: ToolCallEventType::ToolCall,
        tool_call_id: "call-1".to_owned(),
        parent_tool_call_id: None,
        tool_name: "my_tool".to_owned(),
        input: IndexMap::new(),
    })
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_blocks_short_circuit_and_last_result_wins() {
    run_local(async {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let first = extension(
            "/ext/a.ts",
            vec![(
                "tool_call",
                standard(move |_event, _ctx| {
                    let calls = Arc::clone(&first_calls);
                    Box::pin(async move {
                        calls.lock().unwrap().push("a".to_owned());
                        Ok(Some(ExtensionEventResult::ToolCall(ToolCallEventResult {
                            reason: Some("first".to_owned()),
                            ..ToolCallEventResult::default()
                        })))
                    })
                }),
            )],
        );
        let second = extension(
            "/ext/b.ts",
            vec![(
                "tool_call",
                standard(move |_event, _ctx| {
                    let calls = Arc::clone(&second_calls);
                    Box::pin(async move {
                        calls.lock().unwrap().push("b".to_owned());
                        Ok(Some(ExtensionEventResult::ToolCall(ToolCallEventResult {
                            block: Some(true),
                            reason: Some("blocked".to_owned()),
                            ..ToolCallEventResult::default()
                        })))
                    })
                }),
            )],
        );
        let test = make_runner(vec![first, second.clone()]).await;
        let mut event = custom_tool_call_event();
        let result = test
            .runner
            .emit_tool_call(&mut event)
            .await
            .expect("tool_call")
            .expect("result");
        assert_eq!(result.block, Some(true));
        assert_eq!(result.reason.as_deref(), Some("blocked"));
        assert_eq!(*calls.lock().unwrap(), vec!["a", "b"]);

        // 没有 block 时记录最后一次结果。
        let test = make_runner(vec![second]).await;
        let mut event = custom_tool_call_event();
        let result = test
            .runner
            .emit_tool_call(&mut event)
            .await
            .expect("tool_call")
            .expect("result");
        assert_eq!(result.block, Some(true));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_errors_propagate_without_being_reported() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/throws.ts",
            vec![(
                "tool_call",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("tool call failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        )])
        .await;
        let mut event = custom_tool_call_event();
        let error = test
            .runner
            .emit_tool_call(&mut event)
            .await
            .expect_err("must propagate");
        assert_eq!(error.message(), "tool call failed");
        // emitToolCall 没有 try/catch：错误不会进 onError。
        assert!(test.take_errors().is_empty());
    })
    .await;
}
// ===========================================================================
// user_bash
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn user_bash_fails_closed_when_a_handler_throws() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/throws.ts",
            vec![(
                "user_bash",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("Routing failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        )])
        .await;
        let error = test
            .runner
            .emit_user_bash(user_bash_event())
            .await
            .expect_err("must fail closed");
        assert_eq!(error.message(), "Routing failed");
        let errors = test.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].event, "user_bash");
        assert_eq!(errors[0].error, "Routing failed");
    })
    .await;
}

/// 测试用 `BashOperations`（只验证类型穿过派发表）。
struct TestBashOperations;

impl kiana_ext_api::tool_result::BashOperations for TestBashOperations {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        _options: kiana_ext_api::tool_result::BashExecOptions<'a>,
    ) -> kiana_ext_api::BoxFuture<
        'a,
        Result<
            kiana_ext_api::tool_result::BashExecResult,
            Box<dyn std::error::Error + Send + Sync>,
        >,
    > {
        Box::pin(async {
            Ok(kiana_ext_api::tool_result::BashExecResult {
                exit_code: Some(0),
                execution_status: None,
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_accepts_result_and_operations_overrides() {
    run_local(async {
        let result_handler = extension(
            "/ext/result.ts",
            vec![(
                "user_bash",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::UserBash(
                            UserBashEventResult::Result(kiana_ext_api::tool_result::BashResult {
                                output: "handled".to_owned(),
                                exit_code: Some(0),
                                cancelled: false,
                                truncated: false,
                                full_output_path: None,
                                execution_status: None,
                            }),
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![result_handler]).await;
        let result = test
            .runner
            .emit_user_bash(user_bash_event())
            .await
            .expect("user_bash")
            .expect("result");
        match result {
            UserBashEventResult::Result(result) => {
                assert_eq!(result.output, "handled");
                assert_eq!(result.exit_code, Some(0));
            }
            UserBashEventResult::Operations(_) => panic!("expected result"),
        }

        let operations_handler = extension(
            "/ext/operations.ts",
            vec![(
                "user_bash",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::UserBash(
                            UserBashEventResult::Operations(Arc::new(TestBashOperations)),
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![operations_handler]).await;
        let result = test
            .runner
            .emit_user_bash(user_bash_event())
            .await
            .expect("user_bash")
            .expect("result");
        assert!(matches!(result, UserBashEventResult::Operations(_)));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_undefined_handler_result_means_local_execution() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/noop.ts",
            vec![("user_bash", standard(empty_result))],
        )])
        .await;
        let result = test
            .runner
            .emit_user_bash(user_bash_event())
            .await
            .expect("user_bash");
        assert!(result.is_none());
    })
    .await;
}

/// 9068 的 6 个 `it.each` 用例：非法 handler 结果必须 fail closed（JS 侧 XOR 校验）。
#[test]
fn user_bash_rejects_invalid_handler_results() {
    let harness = JsHarness::new();
    let cases = [
        ("an empty object", "{}"),
        ("null operations", "{ operations: null }"),
        ("operations without exec", "{ operations: {} }"),
        ("a null result", "{ result: null }"),
        (
            "an incomplete result",
            "{ result: { output: \"handled\" } }",
        ),
        (
            "operations and a result",
            "{ operations: { exec: async () => ({ exitCode: 0 }) }, result: { output: \"handled\", exitCode: 0, cancelled: false, truncated: false } }",
        ),
    ];
    for (description, handler_result) in cases {
        let source = format!(
            r#"(async () => {{
                __handlers[0] = async () => ({handler_result});
                try {{
                  await __kiana_dispatch.emitUserBash({});
                  return "resolved";
                }} catch (err) {{
                  return err.message;
                }}
            }})()"#,
            state(
                "user_bash",
                r#"{ type: "user_bash", command: "pwd", excludeFromContext: false, cwd: "/tmp" }"#,
                1,
            )
        );
        let message = harness.eval_json(&source);
        let message = message.as_str().unwrap_or_default();
        assert_eq!(
            message,
            "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object",
            "case: {description}"
        );
        let reports: JsonValue =
            serde_json::from_str(&harness.eval::<String>("JSON.stringify(__reports)"))
                .unwrap_or(JsonValue::Null);
        assert!(
            reports
                .as_array()
                .is_some_and(|reports| reports.iter().any(|report| report["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("Invalid user_bash handler result")))),
            "case {description} must report an extension error"
        );
        harness.eval::<()>("__reports.length = 0;");
    }
}

// ===========================================================================
// input
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn input_handled_short_circuits_and_transforms_chain() {
    run_local(async {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let first = extension(
            "/ext/a.ts",
            vec![(
                "input",
                standard(move |event, _ctx| {
                    let calls = Arc::clone(&first_calls);
                    Box::pin(async move {
                        let ExtensionEvent::Input(event) = event else {
                            panic!("expected input");
                        };
                        calls.lock().unwrap().push(event.text.clone());
                        Ok(Some(ExtensionEventResult::Input(
                            InputEventResult::Transform {
                                text: format!("{}:a", event.text),
                                images: None,
                            },
                        )))
                    })
                }),
            )],
        );
        let second = extension(
            "/ext/b.ts",
            vec![(
                "input",
                standard(move |event, _ctx| {
                    let calls = Arc::clone(&second_calls);
                    Box::pin(async move {
                        let ExtensionEvent::Input(event) = event else {
                            panic!("expected input");
                        };
                        calls.lock().unwrap().push(event.text.clone());
                        Ok(Some(ExtensionEventResult::Input(InputEventResult::Handled)))
                    })
                }),
            )],
        );
        let third = extension(
            "/ext/c.ts",
            vec![(
                "input",
                standard(|_event, _ctx| Box::pin(async { panic!("must not run after handled") })),
            )],
        );
        let test = make_runner(vec![first, second, third]).await;
        let result = test
            .runner
            .emit_input("hello", None, InputSource::Interactive, None)
            .await
            .expect("input");
        assert_eq!(result, InputEventResult::Handled);
        assert_eq!(*calls.lock().unwrap(), vec!["hello", "hello:a"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_returns_transform_only_when_text_or_images_changed() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/noop.ts",
            vec![(
                "input",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::Input(
                            InputEventResult::Continue,
                        )))
                    })
                }),
            )],
        )])
        .await;
        let result = test
            .runner
            .emit_input(
                "hello",
                None,
                InputSource::Rpc,
                Some(StreamingBehavior::Steer),
            )
            .await
            .expect("input");
        assert_eq!(result, InputEventResult::Continue);
    })
    .await;
}

// ===========================================================================
// before_provider_request / before_provider_headers
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn before_provider_request_keeps_payload_on_undefined_and_replaces_otherwise() {
    run_local(async {
        let test = make_runner(vec![
            extension(
                "/ext/noop.ts",
                vec![(
                    "before_provider_request",
                    standard(|_event, _ctx| Box::pin(async { Ok(None) })),
                )],
            ),
            extension(
                "/ext/replace.ts",
                vec![(
                    "before_provider_request",
                    standard(|event, _ctx| {
                        Box::pin(async move {
                            let ExtensionEvent::BeforeProviderRequest(event) = event else {
                                panic!("expected before_provider_request");
                            };
                            assert_eq!(event.payload, json!({ "original": true }));
                            Ok(Some(ExtensionEventResult::BeforeProviderRequest(
                                json!({ "replaced": true }),
                            )))
                        })
                    }),
                )],
            ),
        ])
        .await;
        let payload = test
            .runner
            .emit_before_provider_request(json!({ "original": true }))
            .await
            .expect("request");
        assert_eq!(payload, json!({ "replaced": true }));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn before_provider_request_keeps_the_input_payload_when_no_handler_returns_a_value() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/noop.ts",
            vec![(
                "before_provider_request",
                standard(|_event, _ctx| Box::pin(async { Ok(None) })),
            )],
        )])
        .await;
        let payload = test
            .runner
            .emit_before_provider_request(json!({ "original": true }))
            .await
            .expect("request");
        assert_eq!(payload, json!({ "original": true }));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn before_provider_headers_keeps_headers_and_isolates_throwing_handlers() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/throws.ts",
            vec![(
                "before_provider_headers",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("headers failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        )])
        .await;
        let mut headers = ProviderHeaders::new();
        headers.insert("authorization".to_owned(), Some("Bearer x".to_owned()));
        let result = test
            .runner
            .emit_before_provider_headers(headers.clone())
            .await
            .expect("headers");
        assert_eq!(result, headers);
        assert_eq!(test.take_errors()[0].error, "headers failed");
    })
    .await;
}
// ===========================================================================
// context / context_with_system
// ===========================================================================

fn system_message(text: &str) -> AgentMessage {
    let mut object: IndexMap<String, JsonValue> = IndexMap::new();
    object.insert("role".to_owned(), json!("system"));
    object.insert("content".to_owned(), json!(text));
    object.insert("timestamp".to_owned(), json!(0));
    AgentMessage::Custom(Box::new(object))
}

fn role_of(message: &AgentMessage) -> &str {
    message.role()
}

#[tokio::test(flavor = "current_thread")]
async fn context_prepends_the_leading_system_message_after_a_change() {
    run_local(async {
        let ext = extension(
            "/ext/context.ts",
            vec![(
                "context",
                standard(|event, _ctx| {
                    Box::pin(async move {
                        let ExtensionEvent::Context(event) = event else {
                            panic!("expected context");
                        };
                        // handler 只看到非 system 消息。
                        assert!(
                            event
                                .messages
                                .iter()
                                .all(|message| role_of(message) != "system")
                        );
                        let mut messages = event.messages.clone();
                        messages.push(user_message("extra"));
                        Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                            messages: Some(messages),
                        })))
                    })
                }),
            )],
        );
        let test = make_runner(vec![ext]).await;
        let result = test
            .runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        assert_eq!(result.len(), 3);
        assert_eq!(role_of(&result[0]), "system");
        assert_eq!(role_of(&result[2]), "user");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_keeps_the_system_message_when_the_handler_returns_nothing() {
    run_local(async {
        let ext = extension("/ext/noop.ts", vec![("context", standard(empty_result))]);
        let test = make_runner(vec![ext]).await;
        let result = test
            .runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        assert_eq!(result.len(), 2);
        assert_eq!(role_of(&result[0]), "system");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_with_system_warns_but_honors_a_dropped_system_message() {
    run_local(async {
        let ext = extension(
            "/ext/context.ts",
            vec![(
                "context_with_system",
                standard(|event, _ctx| {
                    Box::pin(async move {
                        let ExtensionEvent::ContextWithSystem(event) = event else {
                            panic!("expected context_with_system");
                        };
                        // 第二阶段看到全量（含 system）。
                        assert_eq!(role_of(&event.messages[0]), "system");
                        Ok(Some(ExtensionEventResult::Context(ContextEventResult {
                            messages: Some(vec![user_message("only user")]),
                        })))
                    })
                }),
            )],
        );
        let test = make_runner(vec![ext]).await;
        let result = test
            .runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        assert_eq!(result.len(), 1);
        assert_eq!(role_of(&result[0]), "user");
        let errors = test.take_errors();
        assert_eq!(
            errors[0].error,
            "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage()."
        );
    })
    .await;
}

/// 原地修改：fake host 把**同一个**事件对象交给 handler，`sameMessages` 用引用相等判定。
#[test]
fn context_detects_in_place_mutation_of_the_visible_messages() {
    let harness = JsHarness::new();
    let result = harness.eval_json(&format!(
        r#"(async () => {{
            __handlers[0] = (event) => {{
              event.messages.push({{ role: "user", content: "pushed", timestamp: 0 }});
              return undefined;
            }};
            return await __kiana_dispatch.emitContext({});
        }})()"#,
        r#"{ token: 1, type: "context", paths: ["/ext/a.ts"], snapshot: [[0]],
             messages: [
               { role: "system", content: "prompt", timestamp: 0 },
               { role: "user", content: "hello", timestamp: 0 }
             ],
             paths2: [], snapshot2: [] }"#
    ));
    let messages = result.as_array().expect("messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[2]["content"], "pushed");
}

/// 返回新数组同样被接受；未改变时保持原消息。
#[test]
fn context_accepts_a_returned_array_and_ignores_unchanged_messages() {
    let harness = JsHarness::new();
    let result = harness.eval_json(&format!(
        r#"(async () => {{
            __handlers[0] = (event) => ({{ messages: event.messages.slice(0, 1) }});
            __handlers[1] = () => ({{}});
            return await __kiana_dispatch.emitContext({});
        }})()"#,
        r#"{ token: 1, type: "context", paths: ["/ext/a.ts", "/ext/b.ts"], snapshot: [[0], [1]],
             messages: [
               { role: "system", content: "prompt", timestamp: 0 },
               { role: "user", content: "hello", timestamp: 0 }
             ],
             paths2: [], snapshot2: [] }"#
    ));
    let messages = result.as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["content"], "hello");
}

// ===========================================================================
// before_agent_start
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_chains_force_system_prompt_and_ctx_get_system_prompt() {
    run_local(async {
        let second_saw = Arc::new(Mutex::new(None::<String>));
        let first = extension(
            "/ext/first.ts",
            vec![(
                "before_agent_start",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::BeforeAgentStart(
                            kiana_ext_api::events::BeforeAgentStartEventResult {
                                message: Some(kiana_ext_api::types::CustomMessagePayload {
                                    custom_type: "note".to_owned(),
                                    content: kiana_ai::types::UserContent::Text("hello".to_owned()),
                                    display: false,
                                    details: None,
                                }),
                                system_prompt: Some("first".to_owned()),
                            },
                        )))
                    })
                }),
            )],
        );
        let second = {
            let second_saw = Arc::clone(&second_saw);
            extension(
                "/ext/second.ts",
                vec![(
                    "before_agent_start",
                    standard(move |event, ctx| {
                        let second_saw = Arc::clone(&second_saw);
                        Box::pin(async move {
                            let seen = ctx.get_system_prompt();
                            *second_saw.lock().unwrap() = Some(seen.clone());
                            let ExtensionEvent::BeforeAgentStart(_event) = event else {
                                panic!("expected before_agent_start");
                            };
                            Ok(Some(ExtensionEventResult::BeforeAgentStart(
                                kiana_ext_api::events::BeforeAgentStartEventResult {
                                    message: None,
                                    system_prompt: Some(format!("{seen}\nsecond")),
                                },
                            )))
                        })
                    }),
                )],
            )
        };
        let test = make_runner(vec![first, second]).await;
        let combined = test
            .runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: "/tmp/runner-test".to_owned(),
                    custom_prompt: Some("base".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        assert_eq!(*second_saw.lock().unwrap(), Some("first".to_owned()));
        assert_eq!(
            combined
                .system_prompt_options
                .force_system_prompt
                .as_deref(),
            Some("first\nsecond")
        );
        assert_eq!(combined.messages.len(), 1);
        assert_eq!(combined.messages[0].custom_type, "note");
        assert!(test.take_errors().is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_isolates_throwing_handlers() {
    run_local(async {
        let test = make_runner(vec![extension(
            "/ext/throws.ts",
            vec![(
                "before_agent_start",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("prompt failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        )])
        .await;
        let combined = test
            .runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: "/tmp".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        assert!(combined.messages.is_empty());
        assert!(combined.system_prompt_options.force_system_prompt.is_none());
        assert_eq!(test.take_errors()[0].error, "prompt failed");
    })
    .await;
}

// ===========================================================================
// resources_discover
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn resources_discover_tags_each_path_with_its_extension() {
    run_local(async {
        let first = extension(
            "/ext/a.ts",
            vec![(
                "resources_discover",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ResourcesDiscover(
                            kiana_ext_api::events::ResourcesDiscoverResult {
                                skill_paths: Some(vec!["/skills/a".to_owned()]),
                                prompt_paths: Some(vec!["/prompts/a".to_owned()]),
                                theme_paths: None,
                            },
                        )))
                    })
                }),
            )],
        );
        let second = extension(
            "/ext/b.ts",
            vec![(
                "resources_discover",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ResourcesDiscover(
                            kiana_ext_api::events::ResourcesDiscoverResult {
                                skill_paths: Some(vec!["/skills/b".to_owned()]),
                                prompt_paths: None,
                                theme_paths: Some(vec!["/themes/b".to_owned()]),
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![first, second]).await;
        let paths = test
            .runner
            .emit_resources_discover(
                "/tmp/project",
                kiana_ext_api::events::ResourcesDiscoverReason::Startup,
            )
            .await
            .expect("resources");
        assert_eq!(paths.skill_paths.len(), 2);
        assert_eq!(paths.skill_paths[0].path, "/skills/a");
        assert_eq!(paths.skill_paths[0].extension_path, "/ext/a.ts");
        assert_eq!(paths.skill_paths[1].extension_path, "/ext/b.ts");
        assert_eq!(paths.prompt_paths.len(), 1);
        assert_eq!(paths.theme_paths.len(), 1);
        assert_eq!(paths.theme_paths[0].extension_path, "/ext/b.ts");
    })
    .await;
}

// ===========================================================================
// before_provider_headers 的原地修改（JS 循环语义）
// ===========================================================================

#[test]
fn before_provider_headers_lets_a_handler_mutate_headers_in_place() {
    let harness = JsHarness::new();
    let result = harness.eval_json(&format!(
        r#"(async () => {{
            __handlers[0] = (event) => {{
              event.headers["x-extra"] = "1";
              event.headers["x-remove"] = null;
              return "ignored";
            }};
            return await __kiana_dispatch.emitBeforeProviderHeaders({});
        }})()"#,
        r#"{ token: 1, type: "before_provider_headers", paths: ["/ext/a.ts"], snapshot: [[0]],
             event: { type: "before_provider_headers", headers: { "x-keep": "yes", "x-remove": "gone" } } }"#
    ));
    assert_eq!(result["headers"]["x-keep"], "yes");
    assert_eq!(result["headers"]["x-extra"], "1");
    assert!(result["headers"]["x-remove"].is_null());
}

// ===========================================================================
// message_end 的非 Error 抛出（`String(err)`）
// ===========================================================================

#[test]
fn message_end_reports_string_thrown_values_verbatim() {
    let harness = JsHarness::new();
    let result = harness.eval_json(&format!(
        r#"(async () => {{
            __handlers[0] = () => {{ throw "plain string"; }};
            const message = await __kiana_dispatch.emitMessageEnd({});
            return {{ message: message, reports: __reports }};
        }})()"#,
        r#"{ token: 1, type: "message_end", paths: ["/ext/a.ts"], snapshot: [[0]],
             event: { type: "message_end", message: { role: "user", content: "hi", timestamp: 0 } } }"#
    ));
    assert!(result["message"].is_null());
    assert_eq!(result["reports"][0]["error"], "plain string");
}

// ===========================================================================
// 快照语义：嵌套派发重新取快照；移除在当前 pass 仍运行，新增等下一 pass
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn nested_dispatch_resnapshots_and_mutations_wait_for_the_next_pass() {
    run_local(async {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let runner_cell: Arc<Mutex<Option<Arc<ExtensionRunner>>>> = Arc::new(Mutex::new(None));

        let nested = {
            let calls = Arc::clone(&calls);
            standard(move |_event, _ctx| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.lock().unwrap().push("nested".to_owned());
                    Ok(None)
                })
            })
        };
        let second = {
            let calls = Arc::clone(&calls);
            standard(move |_event, _ctx| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.lock().unwrap().push("second".to_owned());
                    Ok(None)
                })
            })
        };
        let first = {
            let calls = Arc::clone(&calls);
            let runner_cell = Arc::clone(&runner_cell);
            let nested = nested.clone();
            standard(move |_event, _ctx| {
                let calls = Arc::clone(&calls);
                let runner_cell = Arc::clone(&runner_cell);
                let nested = nested.clone();
                Box::pin(async move {
                    calls.lock().unwrap().push("first".to_owned());
                    let runner = runner_cell
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("runner installed");
                    // 移除自己与后面的 handler，并注册一个新 handler。
                    runner.with_extensions(|extensions| {
                        let handlers = extensions[0]
                            .handlers
                            .get_mut("agent_end")
                            .expect("agent_end handlers");
                        handlers.clear();
                        handlers.push(nested);
                    });
                    let _ = runner
                        .emit(ExtensionEvent::AgentEnd(
                            kiana_ext_api::events::AgentEndEvent {
                                event_type: kiana_ext_api::events::AgentEndEventType::AgentEnd,
                                messages: Vec::new(),
                            },
                        ))
                        .await;
                    calls.lock().unwrap().push("first-done".to_owned());
                    Ok(None)
                })
            })
        };
        let ext = extension(
            "/ext/a.ts",
            vec![("agent_end", first), ("agent_end", second)],
        );
        let test = make_runner(vec![ext]).await;
        *runner_cell.lock().unwrap() = Some(Arc::clone(&test.runner));

        let event = || {
            ExtensionEvent::AgentEnd(kiana_ext_api::events::AgentEndEvent {
                event_type: kiana_ext_api::events::AgentEndEventType::AgentEnd,
                messages: Vec::new(),
            })
        };
        test.runner.emit(event()).await.expect("first emit");
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["first", "nested", "first-done", "second"]
        );

        test.runner.emit(event()).await.expect("second emit");
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["first", "nested", "first-done", "second", "nested"]
        );
    })
    .await;
}

// ===========================================================================
// #8967 事件订阅快照（TS `extensions-runner.test.ts` 的 "event subscriptions"）
// ===========================================================================

fn agent_end_event() -> ExtensionEvent {
    ExtensionEvent::AgentEnd(kiana_ext_api::events::AgentEndEvent {
        event_type: kiana_ext_api::events::AgentEndEventType::AgentEnd,
        messages: Vec::new(),
    })
}

fn js_calls(e2e: &E2e) -> Vec<String> {
    let text = e2e
        ._engine
        .eval_script::<String>("JSON.stringify(globalThis.__calls)", "<calls>")
        .expect("read __calls");
    serde_json::from_str(&text).expect("__calls json")
}

/// `allows self-removal without skipping neighboring handlers`：handler 在被调用期间
/// 退订自己，同一次派发里的邻居仍要执行。
#[tokio::test(flavor = "current_thread")]
async fn event_subscriptions_allow_self_removal_without_skipping_neighbors() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "self-removal",
                r#"
globalThis.__calls = [];
export default function (kiana) {
  const calls = globalThis.__calls;
  const unsubscribe = kiana.on("agent_end", () => {
    calls.push("A");
    unsubscribe();
  });
  kiana.on("agent_end", () => {
    calls.push("B");
  });
}
"#,
            )])
            .await;

        runner.emit(agent_end_event()).await.expect("first emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B"]);
        let first_errors = take_collected(&errors);
        assert!(first_errors.is_empty(), "first: {first_errors:?}");
        assert!(
            runner.has_handlers("agent_end"),
            "B must remain after A unsubscribes"
        );
        runner.emit(agent_end_event()).await.expect("second emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B", "B"]);
        let collected = take_collected(&errors);
        assert!(collected.is_empty(), "{collected:?}");
    })
    .await;
}

/// `removes duplicate registrations independently and cleans up the last handler`：
/// 同一个函数注册两次得到两个独立条目；退订是幂等的，只删自己那一条。
#[tokio::test(flavor = "current_thread")]
async fn event_subscriptions_remove_duplicate_registrations_independently() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "duplicates",
                r#"
globalThis.__calls = [];
export default function (kiana) {
  const calls = globalThis.__calls;
  const shared = () => {
    calls.push("shared");
  };
  const unsubscribers = [];
  unsubscribers.push(kiana.on("agent_end", shared));
  unsubscribers.push(kiana.on("agent_end", () => {
    calls.push("B");
  }));
  unsubscribers.push(kiana.on("agent_end", shared));
  globalThis.__stopFirst = unsubscribers[0];
  globalThis.__stopB = unsubscribers[1];
  globalThis.__stopSecond = unsubscribers[2];
}
"#,
            )])
            .await;

        let stop = |name: &str| {
            e2e._engine
                .eval_script::<()>(&format!("globalThis.{name}()"), "<unsubscribe>")
                .expect("unsubscribe runs");
        };

        stop("__stopSecond");
        stop("__stopSecond");
        runner.emit(agent_end_event()).await.expect("first emit");
        assert_eq!(js_calls(&e2e), vec!["shared", "B"]);

        stop("__stopFirst");
        runner.emit(agent_end_event()).await.expect("second emit");
        assert_eq!(js_calls(&e2e), vec!["shared", "B", "B"]);

        stop("__stopB");
        assert!(!runner.has_handlers("agent_end"));
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// `keeps removed pending handlers in the current dispatch`：派发开始后取快照；A 在本次
/// 派发里退订 B，B 仍参加**本次**派发，下一次不再运行。
#[tokio::test(flavor = "current_thread")]
async fn event_subscriptions_keep_removed_pending_handlers_in_the_current_dispatch() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "removed-pending",
                r#"
globalThis.__calls = [];
export default function (kiana) {
  const calls = globalThis.__calls;
  let stopB;
  kiana.on("agent_end", () => {
    calls.push("A");
    stopB();
  });
  stopB = kiana.on("agent_end", () => {
    calls.push("B");
  });
  kiana.on("agent_end", () => {
    calls.push("C");
  });
}
"#,
            )])
            .await;

        runner.emit(agent_end_event()).await.expect("first emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B", "C"]);
        runner.emit(agent_end_event()).await.expect("second emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B", "C", "A", "C"]);
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// `defers registrations made during dispatch until the next dispatch`。
#[tokio::test(flavor = "current_thread")]
async fn event_subscriptions_defer_registrations_until_the_next_dispatch() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "deferred",
                r#"
globalThis.__calls = [];
export default function (kiana) {
  const calls = globalThis.__calls;
  kiana.on("agent_end", () => {
    calls.push("A");
    kiana.on("agent_end", () => {
      calls.push("C");
    });
  });
  kiana.on("agent_end", () => {
    calls.push("B");
  });
}
"#,
            )])
            .await;

        runner.emit(agent_end_event()).await.expect("first emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B"]);
        runner.emit(agent_end_event()).await.expect("second emit");
        assert_eq!(js_calls(&e2e), vec!["A", "B", "A", "B", "C"]);
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

// ===========================================================================
// Send + Sync（`kiana-session` 以 `Arc<ExtensionRunner>` 共享）
// ===========================================================================

#[test]
fn extension_runner_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ExtensionRunner>();
    assert_send_sync::<Arc<ExtensionRunner>>();
}

// ===========================================================================
// 端到端：真实 loader + JS 扩展（共享事件对象的就地修改、user_bash 桥、晚快照）
// ===========================================================================

#[path = "fixtures/extensions/support.rs"]
mod support;

use support::{LoaderHarness, temp_root, write_file};

fn stub_actions() -> ExtensionActions {
    ExtensionActions {
        send_message: Arc::new(|_| {}),
        send_user_message: Arc::new(|_, _| {}),
        append_entry: Arc::new(|_, _| {}),
        set_session_name: Arc::new(|_| {}),
        get_session_name: Arc::new(|| None),
        set_label: Arc::new(|_, _| {}),
        get_active_tools: Arc::new(Vec::new),
        get_all_tools: Arc::new(Vec::new),
        get_settings: Arc::new(Default::default),
        set_active_tools: Arc::new(|_| {}),
        refresh_tools: Arc::new(|| {}),
        get_commands: Arc::new(Vec::new),
        set_model: Arc::new(|_| Box::pin(async { false })),
        get_thinking_level: Arc::new(|| kiana_agent_core::ThinkingLevel::Off),
        set_thinking_level: Arc::new(|_| {}),
    }
}

fn stub_context_actions() -> ExtensionContextActions {
    ExtensionContextActions {
        get_model: Arc::new(|| None),
        get_scoped_models: Arc::new(Vec::new),
        is_idle: Arc::new(|| true),
        is_project_trusted: Arc::new(|| true),
        get_signal: Arc::new(|| None),
        abort: Arc::new(|| {}),
        has_pending_messages: Arc::new(|| false),
        shutdown: Arc::new(|| {}),
        get_context_usage: Arc::new(|| None),
        compact: Arc::new(|_| {}),
        get_system_prompt: Arc::new(String::new),
        get_system_prompt_options: None,
        execute_tool: None,
        get_callable_tools: None,
    }
}

fn empty_preview(context_entries: usize) -> kiana_ext_api::events::BoundaryContextPreview {
    kiana_ext_api::events::BoundaryContextPreview {
        context_entries: Vec::new(),
        context_messages: (0..context_entries)
            .map(|_| user_message("preview"))
            .collect(),
        llm_messages: Vec::new(),
        pending_messages: Vec::new(),
        can_continue: false,
    }
}

/// 端到端 runner：真实 loader 加载 JS 扩展（`kiana.on`），`ExtensionRunner` 用同引擎派发。
struct E2e {
    /// 保持引擎存活（同线程 drop；runner 的上下文/派发都依赖它）。
    _engine: Rc<JsEngine>,
    root: tempfile::TempDir,
}

impl E2e {
    fn new() -> Self {
        let harness = LoaderHarness::new();
        context::install(&harness.engine).expect("install context.js");
        dispatch::install(Rc::clone(&harness.engine)).expect("install dispatch.js");
        Self {
            _engine: harness.engine,
            root: temp_root(),
        }
    }

    /// 写 `sources`（name → JS 源码）并加载。
    async fn load(&self, sources: &[(&str, &str)]) -> kiana_ext_api::types::LoadExtensionsResult {
        let mut paths = Vec::new();
        for (name, source) in sources {
            paths.push(
                write_file(self.root.path(), &format!("extensions/{name}.ts"), source)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let cwd = self.root.path().to_string_lossy().into_owned();
        let result = loader::load_extensions(&paths, &cwd, None, None).await;
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        result
    }

    /// 写 `sources`（name → JS 源码）并加载，返回 runner + 错误收集。
    async fn runner(
        &self,
        sources: &[(&str, &str)],
    ) -> (Arc<ExtensionRunner>, Arc<Mutex<Vec<ExtensionError>>>) {
        let result = self.load(sources).await;
        let cwd = self.root.path().to_string_lossy().into_owned();
        use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
            .await
            .expect("model runtime");
        let model_registry = Arc::new(ModelRegistry::new(Arc::new(model_runtime)));
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
        let runner = Arc::new(ExtensionRunner::new(
            result.extensions,
            result.runtime,
            PathBuf::from(cwd),
            session,
            model_registry,
        ));
        runner.bind_core(stub_actions(), stub_context_actions(), None);
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _ = runner.on_error(Arc::new(move |error| {
            sink.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(error);
        }));
        (runner, errors)
    }
}

fn take_collected(errors: &Arc<Mutex<Vec<ExtensionError>>>) -> Vec<ExtensionError> {
    std::mem::take(
        &mut *errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_handlers_patch_input_in_place() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "patch",
                r#"
export default function(kiana) {
  kiana.on("tool_call", (event) => {
    event.input.command = "patched";
  });
  kiana.on("tool_call", (event) => ({ block: true, reason: event.input.command }));
}
"#,
            )])
            .await;
        let mut event = custom_tool_call_event();
        let result = runner
            .emit_tool_call(&mut event)
            .await
            .expect("tool_call")
            .expect("result");
        assert_eq!(result.reason.as_deref(), Some("patched"));
        let ToolCallEvent::Custom(event) = event else {
            panic!("expected custom tool call");
        };
        assert_eq!(event.input["command"], json!("patched"));
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn before_provider_headers_handlers_mutate_headers_in_place() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "headers",
                r#"
export default function(kiana) {
  kiana.on("before_provider_headers", (event) => {
    event.headers["x-added"] = "1";
    delete event.headers["x-drop"];
  });
  kiana.on("before_provider_headers", (event) => {
    event.headers["x-seen"] = event.headers["x-added"];
  });
}
"#,
            )])
            .await;
        let mut original = ProviderHeaders::new();
        original.insert("x-drop".to_owned(), Some("gone".to_owned()));
        let headers = runner
            .emit_before_provider_headers(original)
            .await
            .expect("headers");
        let errors = take_collected(&errors);
        assert_eq!(
            headers.get("x-added"),
            Some(&Some("1".to_owned())),
            "headers={headers:?} errors={errors:?}"
        );
        assert_eq!(
            headers.get("x-seen"),
            Some(&Some("1".to_owned())),
            "headers={headers:?} errors={errors:?}"
        );
        assert!(!headers.contains_key("x-drop"), "headers={headers:?}");
        assert!(errors.is_empty(), "errors={errors:?}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn boundary_handlers_share_entries_and_push_in_place() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[
                (
                    "boundary-first",
                    r#"
export default function(kiana) {
  kiana.on("agent_before_settle", (event) => {
    event.entries.push({ type: "custom", customType: "first", data: 1 });
    return { continue: true };
  });
}
"#,
                ),
                (
                    "boundary-second",
                    r#"
export default function(kiana) {
  kiana.on("agent_before_settle", (event) => ({
    continue:
      event.entries.length === 1 &&
      event.context.contextMessages.length === 1 &&
      event.context.canContinue === false,
  }));
}
"#,
                ),
            ])
            .await;
        let base =
            BoundaryBaseEvent::AgentBeforeSettle(kiana_ext_api::events::AgentBeforeSettleEvent {
                entries: Vec::new(),
                r#continue: false,
                context: empty_preview(0),
                outcome: kiana_ext_api::events::AgentActivityOutcome::Completed,
                event_type: kiana_ext_api::events::AgentBeforeSettleEventType::AgentBeforeSettle,
            });
        let result = runner
            .emit_boundary(
                base,
                Arc::new(|entries| Box::pin(async move { Ok(empty_preview(entries.len())) })),
            )
            .await
            .expect("boundary");
        let expected: JsonValue =
            serde_json::from_str(include_str!("fixtures/boundary/shared_draft_expected.json"))
                .expect("fixture");
        assert_eq!(result.valid, expected["valid"].as_bool().expect("valid"));
        assert_eq!(
            result.r#continue,
            expected["continue"].as_bool().expect("continue")
        );
        assert_eq!(
            result.entries.len() as u64,
            expected["entries"].as_u64().expect("entries")
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_with_system_snapshot_happens_after_phase_one() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "late",
                r#"
export default function(kiana) {
  let added = false;
  kiana.on("context", (event) => {
    if (added) return undefined;
    added = true;
    kiana.on("context_with_system", () => ({
      messages: [{ role: "user", content: "from late handler", timestamp: 0 }],
    }));
  });
}
"#,
            )])
            .await;
        let messages = runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), "user");
        let message = serde_json::to_value(&messages[0]).expect("message json");
        assert_eq!(message["content"], json!("from late handler"));
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1, "errors={errors:?}");
        assert_eq!(errors[0].event, "context_with_system");
        assert_eq!(
            errors[0].error,
            "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage()."
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_renders_current_options_and_reports_mutations() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "prompt",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event, ctx) => {
    event.systemPromptOptions.selectedTools.push("grep");
    return { systemPrompt: ctx.getSystemPrompt() + "\nfirst" };
  });
  kiana.on("before_agent_start", (event, ctx) => {
    return { systemPrompt: ctx.getSystemPrompt() + "\nsecond" };
  });
}
"#,
            )])
            .await;
        // 测试渲染器：forceSystemPrompt 优先，否则 customPrompt + 工具列表。
        let renderer: kiana_extensions::runner::SystemPromptRenderer = Arc::new(
            |options: &kiana_ext_api::system_prompt::NormalizedBuildSystemPromptOptions| {
                if let Some(force) = &options.force_system_prompt {
                    return force.clone();
                }
                format!(
                    "{} [{}]",
                    options.custom_prompt.clone().unwrap_or_default(),
                    options.selected_tools.join(",")
                )
            },
        );
        runner.set_system_prompt_renderer(Some(renderer));
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    custom_prompt: Some("base".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        let errors = take_collected(&errors);
        let force = combined
            .system_prompt_options
            .force_system_prompt
            .clone()
            .unwrap_or_else(|| {
                panic!(
                    "force prompt missing: options={:?} errors={:?}",
                    combined.system_prompt_options, errors
                )
            });
        assert!(force.contains("base"), "{force}");
        assert!(force.ends_with("\nfirst\nsecond"), "{force}");
        assert!(
            combined
                .system_prompt_options
                .selected_tools
                .iter()
                .any(|tool| tool == "grep"),
            "in-place options mutation must persist"
        );
        assert!(errors.is_empty(), "errors={errors:?}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// user_bash：JS 结果的逐字校验 + `{ operations }` 桥（9068）
// ---------------------------------------------------------------------------

async fn emit_user_bash_with(source: &str) -> Result<Option<UserBashEventResult>, DispatchError> {
    let e2e = E2e::new();
    let (runner, errors) = e2e.runner(&[("bash", source)]).await;
    let result = runner.emit_user_bash(user_bash_event()).await;
    // 错误由 dispatch.js 上报（每条 handler 错误一条 ExtensionError）。
    let _ = take_collected(&errors);
    result
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_js_result_validation_matches_ts() {
    run_local(async {
        let invalid = "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object";
        // 夹具即 9068 的非法结果矩阵（`tests/fixtures/dispatch/user_bash_invalid_cases.json`）。
        let cases: JsonValue = serde_json::from_str(include_str!(
            "fixtures/dispatch/user_bash_invalid_cases.json"
        ))
        .expect("fixture");
        for case in cases.as_array().expect("cases") {
            let description = case["name"].as_str().expect("name");
            let body = case["body"].as_str().expect("body");
            let source = format!("export default function(kiana) {{ {body} }}");
            let error = emit_user_bash_with(&source).await.expect_err(description);
            assert_eq!(error.message(), invalid, "case: {description}");
        }

        // undefined → 本地执行。
        let result = emit_user_bash_with("export default function(kiana) { kiana.on(\"user_bash\", () => undefined); }")
            .await
            .expect("undefined");
        assert!(result.is_none());

        // 合法 { result } → Result；exitCode 数字保留。
        let result = emit_user_bash_with(
            "export default function(kiana) { kiana.on(\"user_bash\", () => ({ result: { output: \"handled\", exitCode: 3, cancelled: true, truncated: false } })); }",
        )
        .await
        .expect("result")
        .expect("override");
        let UserBashEventResult::Result(result) = result else {
            panic!("expected result override");
        };
        assert_eq!(result.output, "handled");
        assert_eq!(result.exit_code, Some(3));
        assert!(result.cancelled);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_operations_execute_through_the_js_exec_bridge() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "ops",
                r#"
export default function(kiana) {
  kiana.on("user_bash", () => ({
    operations: {
      async exec(command, cwd, options) {
        options.onData(new TextEncoder().encode("chunk:" + command + "@" + cwd));
        return { exitCode: 7 };
      },
    },
  }));
}
"#,
            )])
            .await;
        let result = runner
            .emit_user_bash(user_bash_event())
            .await
            .expect("user_bash")
            .expect("operations");
        let UserBashEventResult::Operations(operations) = result else {
            panic!("expected operations");
        };
        let chunks = Mutex::new(Vec::<u8>::new());
        let on_data = |bytes: &[u8]| {
            chunks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(bytes);
        };
        let exec = operations
            .exec(
                "pwd",
                "/tmp",
                kiana_ext_api::tool_result::BashExecOptions {
                    on_data: &on_data,
                    signal: None,
                    timeout: None,
                    env: None,
                },
            )
            .await
            .expect("exec");
        assert_eq!(exec.exit_code, Some(7));
        assert_eq!(
            String::from_utf8_lossy(&chunks.lock().unwrap()),
            "chunk:pwd@/tmp"
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// project_trust：JS handler 的抛出 / null / 缺 trusted（TS 语义）
// ---------------------------------------------------------------------------

fn trust_event() -> ProjectTrustEvent {
    ProjectTrustEvent {
        event_type: kiana_ext_api::events::ProjectTrustEventType::ProjectTrust,
        cwd: "/tmp".to_owned(),
    }
}

fn trust_context() -> ProjectTrustContext {
    use kiana_ext_api::ui::NotifyKind;
    ProjectTrustContext {
        cwd: "/tmp".to_owned(),
        mode: kiana_ext_api::types::ExtensionMode::Tui,
        has_ui: true,
        ui: kiana_ext_api::events::ProjectTrustUi {
            select: Arc::new(|_title, _options, _dialog| Box::pin(async { None })),
            confirm: Arc::new(|_title, _message, _dialog| Box::pin(async { false })),
            input: Arc::new(|_title, _placeholder, _dialog| Box::pin(async { None })),
            notify: Arc::new(|_message: String, _kind: Option<NotifyKind>| {}),
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn project_trust_js_errors_and_null_results_are_collected() {
    run_local(async {
        let e2e = E2e::new();
        let result = e2e
            .load(&[(
                "trust",
                r#"
export default function(kiana) {
  kiana.on("project_trust", () => { throw new Error("boom"); });
}
"#,
            )])
            .await;
        let outcome = kiana_extensions::project_trust::emit_project_trust_event(
            &result,
            trust_event(),
            trust_context(),
        )
        .await;
        assert_eq!(outcome.result, None);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].error, "boom");
        assert_eq!(outcome.errors[0].event, "project_trust");
        assert!(outcome.errors[0].extension_path.ends_with("trust.ts"));

        let e2e = E2e::new();
        let result = e2e
            .load(&[(
                "null",
                "export default function(kiana) { kiana.on(\"project_trust\", () => null); }",
            )])
            .await;
        let outcome = kiana_extensions::project_trust::emit_project_trust_event(
            &result,
            trust_event(),
            trust_context(),
        )
        .await;
        assert_eq!(outcome.result, None);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(
            outcome.errors[0].error,
            "Cannot read properties of null (reading 'trusted')"
        );

        let e2e = E2e::new();
        let result = e2e
            .load(&[(
                "empty",
                "export default function(kiana) { kiana.on(\"project_trust\", () => ({})); }",
            )])
            .await;
        let outcome = kiana_extensions::project_trust::emit_project_trust_event(
            &result,
            trust_event(),
            trust_context(),
        )
        .await;
        // TS: `{}` 不是 "undecided"，作为决定返回；调用方只认 "yes" → deny。
        assert_eq!(
            outcome.result,
            Some(ProjectTrustEventResult {
                trusted: kiana_ext_api::events::ProjectTrustEventDecision::No,
                remember: None,
            })
        );
        assert!(outcome.errors.is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// 其余回归：input.images、cache_warming 未知动作、handler deadline
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn input_js_handler_sees_undefined_images_when_none_were_passed() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "input",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({
    action: "transform",
    text: event.images === undefined ? "undefined" : "defined",
  }));
}
"#,
            )])
            .await;
        let result = runner
            .emit_input("hi", None, InputSource::Interactive, None)
            .await
            .expect("input");
        let InputEventResult::Transform { text, .. } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "undefined");
        let images = vec![kiana_ai::types::ImageContent {
            kind_image: kiana_ai::types::ImageKind::Image,
            data: "aGk=".to_owned(),
            mime_type: "image/png".to_owned(),
        }];
        let result = runner
            .emit_input("hi", Some(images), InputSource::Interactive, None)
            .await
            .expect("input");
        let InputEventResult::Transform { text, .. } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "defined");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cache_warming_unknown_js_action_is_treated_as_warm() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "warm",
                r#"
export default function(kiana) {
  kiana.on("cache_warming_decision", () => ({ action: "something-else" }));
}
"#,
            )])
            .await;
        let action = runner
            .emit_cache_warming_decision(CacheWarmingDecisionEvent {
                warm_cost: 1.0,
                miss_cost: 2.0,
                continuation_probability: 0.5,
                action: kiana_config::cache::warmer::CacheWarmingAction::Stop,
            })
            .await
            .expect("decision");
        assert_eq!(
            action,
            kiana_config::cache::warmer::CacheWarmingAction::Warm
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_js_handlers_run_under_the_handler_deadline() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "slow",
                r#"
export default function(kiana) {
  kiana.on("message_end", () => {
    const start = Date.now();
    while (Date.now() - start < 150) {}
    return undefined;
  });
}
"#,
            )])
            .await;
        let result = runner
            .emit_message_end(message_end_event(user_message("hello")))
            .await
            .expect("message_end");
        assert!(result.is_none());
        assert!(
            take_collected(&errors).is_empty(),
            "a 150 ms synchronous handler must not hit the 100 ms default watchdog"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// input：extensions-input-event.test.ts 的其余用例
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn input_returns_continue_without_a_result_or_with_an_explicit_continue() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, _errors) = e2e.runner(&[]).await;
        assert_eq!(
            runner
                .emit_input("x", None, InputSource::Interactive, None)
                .await
                .expect("input"),
            InputEventResult::Continue
        );
        assert!(!runner.has_handlers("input"));

        let e2e = E2e::new();
        let (runner, _errors) = e2e
            .runner(&[(
                "input",
                r#"
export default function(kiana) {
  kiana.on("input", async () => {});
  kiana.on("input", async () => ({ action: "continue" }));
}
"#,
            )])
            .await;
        assert!(runner.has_handlers("input"));
        assert_eq!(
            runner
                .emit_input("x", None, InputSource::Interactive, None)
                .await
                .expect("input"),
            InputEventResult::Continue
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_passes_source_and_streaming_behavior_and_chains_transforms() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "input",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({
    action: "transform",
    text: event.source + ":" + String(event.streamingBehavior) + ":" + event.text,
  }));
  kiana.on("input", (event) => ({ action: "transform", text: event.text + "[2]" }));
}
"#,
            )])
            .await;
        let result = runner
            .emit_input("X", None, InputSource::Rpc, Some(StreamingBehavior::Steer))
            .await
            .expect("input");
        let InputEventResult::Transform { text, images } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "rpc:steer:X[2]");
        assert!(images.is_none());

        let result = runner
            .emit_input("X", None, InputSource::Extension, None)
            .await
            .expect("input");
        let InputEventResult::Transform { text, .. } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "extension:undefined:X[2]");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_transforms_preserve_or_replace_images() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "input",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({ action: "transform", text: "T:" + event.text }));
}
"#,
            )])
            .await;
        let original = vec![kiana_ai::types::ImageContent {
            kind_image: kiana_ai::types::ImageKind::Image,
            data: "orig".to_owned(),
            mime_type: "image/png".to_owned(),
        }];
        let result = runner
            .emit_input("hi", Some(original.clone()), InputSource::Interactive, None)
            .await
            .expect("input");
        let InputEventResult::Transform { text, images } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "T:hi");
        assert_eq!(images, Some(original.clone()));

        // 另一个 handler 显式替换图片。
        let e2e = E2e::new();
        let (runner, _errors) = e2e
            .runner(&[(
                "replace",
                r#"
export default function(kiana) {
  kiana.on("input", () => ({
    action: "transform",
    text: "X",
    images: [{ type: "image", data: "new", mimeType: "image/jpeg" }],
  }));
}
"#,
            )])
            .await;
        let result = runner
            .emit_input("hi", Some(original), InputSource::Interactive, None)
            .await
            .expect("input");
        let InputEventResult::Transform { text, images } = result else {
            panic!("expected transform");
        };
        assert_eq!(text, "X");
        let images = images.expect("replaced images");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, "new");
        assert_eq!(images[0].mime_type, "image/jpeg");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_catches_handler_errors_and_continues() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "throws",
                r#"
export default function(kiana) {
  kiana.on("input", () => { throw new Error("boom"); });
}
"#,
            )])
            .await;
        let result = runner
            .emit_input("x", None, InputSource::Interactive, None)
            .await
            .expect("input");
        assert_eq!(result, InputEventResult::Continue);
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "boom");
        assert_eq!(errors[0].event, "input");
    })
    .await;
}

// ---------------------------------------------------------------------------
// 其余 emitter 的补充分支（design §10.2 的矩阵）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn before_provider_request_payload_is_a_shared_object() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "payload",
                r#"
export default function(kiana) {
  kiana.on("before_provider_request", (event) => {
    event.payload.model = "patched";
  });
  kiana.on("before_provider_request", (event) => {
    event.payload.seen = event.payload.model;
  });
}
"#,
            )])
            .await;
        let payload = runner
            .emit_before_provider_request(json!({ "model": "original" }))
            .await
            .expect("payload");
        assert_eq!(payload["model"], json!("patched"));
        assert_eq!(payload["seen"], json!("patched"));
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cache_warming_ignores_undefined_results_and_isolates_throwing_handlers() {
    run_local(async {
        let unchanged = extension(
            "/ext/unchanged.ts",
            vec![(
                "cache_warming_decision",
                standard(|_event, _ctx| Box::pin(async { Ok(None) })),
            )],
        );
        let throws = extension(
            "/ext/throws.ts",
            vec![(
                "cache_warming_decision",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("warm failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        );
        let test = make_runner(vec![unchanged, throws]).await;
        let action = test
            .runner
            .emit_cache_warming_decision(CacheWarmingDecisionEvent {
                warm_cost: 1.0,
                miss_cost: 2.0,
                continuation_probability: 0.5,
                action: kiana_config::cache::warmer::CacheWarmingAction::Warm,
            })
            .await
            .expect("decision");
        assert_eq!(
            action,
            kiana_config::cache::warmer::CacheWarmingAction::Warm
        );
        assert_eq!(test.take_errors()[0].error, "warm failed");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resources_discover_isolates_throwing_handlers_and_keeps_order() {
    run_local(async {
        let first = extension(
            "/ext/first.ts",
            vec![(
                "resources_discover",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ResourcesDiscover(
                            kiana_ext_api::events::ResourcesDiscoverResult {
                                skill_paths: Some(vec!["/skills/1".to_owned()]),
                                prompt_paths: None,
                                theme_paths: None,
                            },
                        )))
                    })
                }),
            )],
        );
        let throws = extension(
            "/ext/throws.ts",
            vec![(
                "resources_discover",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Err(Box::new(std::io::Error::other("discover failed"))
                            as Box<dyn std::error::Error + Send + Sync>)
                    })
                }),
            )],
        );
        let last = extension(
            "/ext/last.ts",
            vec![(
                "resources_discover",
                standard(|_event, _ctx| {
                    Box::pin(async {
                        Ok(Some(ExtensionEventResult::ResourcesDiscover(
                            kiana_ext_api::events::ResourcesDiscoverResult {
                                skill_paths: Some(vec!["/skills/2".to_owned()]),
                                prompt_paths: None,
                                theme_paths: None,
                            },
                        )))
                    })
                }),
            )],
        );
        let test = make_runner(vec![first, throws, last]).await;
        let paths = test
            .runner
            .emit_resources_discover(
                "/tmp",
                kiana_ext_api::events::ResourcesDiscoverReason::Startup,
            )
            .await
            .expect("resources");
        assert_eq!(
            paths.skill_paths,
            vec![
                ResourcePathEntry {
                    path: "/skills/1".to_owned(),
                    extension_path: "/ext/first.ts".to_owned(),
                },
                ResourcePathEntry {
                    path: "/skills/2".to_owned(),
                    extension_path: "/ext/last.ts".to_owned(),
                },
            ]
        );
        assert_eq!(test.take_errors()[0].error, "discover failed");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn context_no_op_js_handler_keeps_every_message_and_reference_semantics() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "noop-context",
                "export default function(kiana) { kiana.on(\"context\", () => undefined); }",
            )])
            .await;
        let original = vec![
            system_message("prompt"),
            user_message("hello"),
            system_message("mid-conversation system message"),
        ];
        let result = runner
            .emit_context(original.clone())
            .await
            .expect("context");
        // 未修改时 TS 原样返回 `currentMessages`（包括中间位置的 system 消息）。
        assert_eq!(result.len(), 3);
        let expected = original
            .iter()
            .map(|message| serde_json::to_value(message).expect("message"))
            .collect::<Vec<_>>();
        let actual = result
            .iter()
            .map(|message| serde_json::to_value(message).expect("message"))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn user_bash_js_handler_throw_fails_closed_and_is_reported() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "throws",
                "export default function(kiana) { kiana.on(\"user_bash\", () => { throw new Error(\"Routing failed\"); }); }",
            )])
            .await;
        let error = runner
            .emit_user_bash(user_bash_event())
            .await
            .expect_err("must fail closed");
        assert_eq!(error.message(), "Routing failed");
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].event, "user_bash");
        assert_eq!(errors[0].error, "Routing failed");
    })
    .await;
}

// ===========================================================================
// 回归：独立审查发现的偏离（kiana-extensions:runner-core）
// ===========================================================================

/// 审查 #1（critical）：`context` handler 返回 `{ messages: event.messages }` 时，
/// TS 的 `sameMessages` 引用比较为真 → `currentMessages` 原样返回（保留中间位置的
/// system 消息与 prompt cache 前缀）。跨 JSON 桥后必须按内容比较复现同一结果。
///
/// TS: `9789-context-handler-system-messages.test.ts` 的 “keeps mid-conversation system
/// messages in place when a handler leaves the conversation unchanged”。
#[tokio::test(flavor = "current_thread")]
async fn context_returning_the_visible_array_keeps_mid_conversation_system_messages() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "identity-context",
                "export default function(kiana) { kiana.on(\"context\", (event) => ({ messages: event.messages })); }",
            )])
            .await;
        let original = vec![
            system_message("prompt"),
            user_message("hello"),
            system_message("mid-conversation"),
        ];
        let result = runner
            .emit_context(original.clone())
            .await
            .expect("context");
        assert_eq!(result.len(), 3, "result={result:?}");
        assert_eq!(role_of(&result[0]), "system");
        assert_eq!(role_of(&result[1]), "user");
        assert_eq!(role_of(&result[2]), "system");
        // 未改动时不得重建首部 system 消息（否则中间 system 补丁会被合并进首部）。
        let head = serde_json::to_value(&result[0]).expect("json");
        assert_eq!(head["content"], json!("prompt"), "head={head}");
        let mid = serde_json::to_value(&result[2]).expect("json");
        assert_eq!(mid["content"], json!("mid-conversation"), "mid={mid}");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #1（critical）第二段：handler 只就地修改消息**内容**（数组引用不变）时，
/// TS 不重建首部 system 消息，但内容修改通过共享对象生效。
#[tokio::test(flavor = "current_thread")]
async fn context_in_place_content_edit_keeps_mid_conversation_system_messages() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "edit-context",
                r#"
export default function(kiana) {
  kiana.on("context", (event) => {
    const target = event.messages.find((message) => message.role === "user");
    target.content = "edited";
  });
}
"#,
            )])
            .await;
        let original = vec![
            system_message("prompt"),
            user_message("hello"),
            system_message("mid-conversation"),
        ];
        let result = runner
            .emit_context(original.clone())
            .await
            .expect("context");
        assert_eq!(result.len(), 3, "result={result:?}");
        assert_eq!(role_of(&result[0]), "system");
        assert_eq!(role_of(&result[1]), "user");
        assert_eq!(role_of(&result[2]), "system");
        let head = serde_json::to_value(&result[0]).expect("json");
        assert_eq!(head["content"], json!("prompt"), "head={head}");
        let user = serde_json::to_value(&result[1]).expect("json");
        assert_eq!(user["content"], json!("edited"), "user={user}");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #2（major）：handler 抛错前对共享事件对象的就地修改不回滚。
///
/// TS: `extensions-runner.test.ts` 的 “keeps shared mutations made before a handler throws”。
#[tokio::test(flavor = "current_thread")]
async fn boundary_keeps_shared_mutations_made_before_a_handler_throws() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "throw-boundary",
                r#"
export default function(kiana) {
  kiana.on("agent_before_settle", (event) => {
    event.entries.push({ type: "custom", customType: "kept" });
    throw new Error("boundary failed");
  });
}
"#,
            )])
            .await;
        let base =
            BoundaryBaseEvent::AgentBeforeSettle(kiana_ext_api::events::AgentBeforeSettleEvent {
                entries: Vec::new(),
                r#continue: false,
                context: empty_preview(0),
                outcome: kiana_ext_api::events::AgentActivityOutcome::Completed,
                event_type: kiana_ext_api::events::AgentBeforeSettleEventType::AgentBeforeSettle,
            });
        let result = runner
            .emit_boundary(
                base,
                Arc::new(|entries| Box::pin(async move { Ok(empty_preview(entries.len())) })),
            )
            .await
            .expect("boundary");
        assert!(result.valid, "valid={}", result.valid);
        assert_eq!(result.entries.len(), 1, "entries={:?}", result.entries);
        let entry = serde_json::to_value(&result.entries[0]).expect("json");
        assert_eq!(entry["type"], json!("custom"));
        assert_eq!(entry["customType"], json!("kept"));
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1, "errors={errors:?}");
        assert_eq!(errors[0].error, "boundary failed");
    })
    .await;
}

/// 审查 #4（major）：`ExtensionError.stack` 记录扩展原始抛出的栈（不是宿主桥新建
/// `Error` 的栈）。
#[tokio::test(flavor = "current_thread")]
async fn handler_errors_carry_the_original_extension_stack() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "stack",
                r#"
export default function(kiana) {
  kiana.on("agent_start", () => { throw new Error("stack-boom"); });
  kiana.on("agent_start", () => { throw "plain-string"; });
}
"#,
            )])
            .await;
        let event = ExtensionEvent::AgentStart(kiana_ext_api::events::AgentStartEvent {
            event_type: kiana_ext_api::events::AgentStartEventType::AgentStart,
        });
        let _ = runner.emit(event).await.expect("emit");
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 2, "errors={errors:?}");
        assert_eq!(errors[0].error, "stack-boom");
        let stack = errors[0].stack.as_deref().expect("stack");
        assert!(stack.contains("at "), "stack={stack}");
        // 非 Error 抛出：TS 的 `err instanceof Error ? err.stack : undefined` → 无 stack。
        assert_eq!(errors[1].error, "plain-string");
        assert_eq!(errors[1].stack, None, "stack={:?}", errors[1].stack);
    })
    .await;
}

/// 审查 #9（minor）：handler 把 `event.images` 原样返回时，TS 的最终动作是
/// `continue`（`result.images ?? currentImages` 引用不变）。
#[tokio::test(flavor = "current_thread")]
async fn input_transform_returning_the_event_images_keeps_continue() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "images",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({ action: "transform", text: event.text, images: event.images }));
}
"#,
            )])
            .await;
        let images = vec![kiana_ai::types::ImageContent {
            kind_image: kiana_ai::types::ImageKind::Image,
            data: "aGk=".to_owned(),
            mime_type: "image/png".to_owned(),
        }];
        let result = runner
            .emit_input("hi", Some(images), InputSource::Interactive, None)
            .await
            .expect("input");
        assert_eq!(result, InputEventResult::Continue);
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #10（minor）：事件对象的键序与 TS 的对象字面量构造顺序一致
/// （`agent-session.ts` 的 `tool_call` / `turn_end` / `agent_before_settle`）。
#[tokio::test(flavor = "current_thread")]
async fn event_key_order_matches_typescript_construction_order() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "order",
                r#"
export default function(kiana) {
  kiana.on("tool_call", (event) => ({ reason: Object.keys(event).join(",") }));
  kiana.on("tool_result", (event) => ({ details: { order: Object.keys(event).join(",") } }));
  kiana.on("before_agent_start", (event) => ({ systemPrompt: Object.keys(event).join(",") }));
  kiana.on("agent_before_settle", (event) => {
    event.entries.push({ type: "custom", customType: "order", data: Object.keys(event).join(",") });
    return { continue: false };
  });
}
"#,
            )])
            .await;
        let mut event = custom_tool_call_event();
        let result = runner
            .emit_tool_call(&mut event)
            .await
            .expect("tool_call")
            .expect("result");
        assert_eq!(
            result.reason.as_deref(),
            Some("type,toolName,toolCallId,input")
        );

        let tool_result = runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("original")]))
            .await
            .expect("tool_result")
            .expect("result");
        let kiana_ext_api::events::OptionalPatch::Value(details) = tool_result.details else {
            panic!("expected details patch");
        };
        assert_eq!(
            details["order"],
            json!("type,toolName,toolCallId,input,content,details,structuredContent,isError")
        );

        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        assert_eq!(
            combined
                .system_prompt_options
                .force_system_prompt
                .as_deref(),
            Some("messageType,prompt,images,type,systemPromptOptions,systemPrompt")
        );

        let base =
            BoundaryBaseEvent::AgentBeforeSettle(kiana_ext_api::events::AgentBeforeSettleEvent {
                entries: Vec::new(),
                r#continue: false,
                context: empty_preview(0),
                outcome: kiana_ext_api::events::AgentActivityOutcome::Completed,
                event_type: kiana_ext_api::events::AgentBeforeSettleEventType::AgentBeforeSettle,
            });
        let boundary = runner
            .emit_boundary(
                base,
                Arc::new(|entries| Box::pin(async move { Ok(empty_preview(entries.len())) })),
            )
            .await
            .expect("boundary");
        let entry = serde_json::to_value(&boundary.entries[0]).expect("json");
        assert_eq!(
            entry["data"],
            json!("type,outcome,entries,continue,context")
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #5（major）：`bindCore` 之后 provider / 虚拟模型注册失败同步抛回扩展代码
/// （由所在事件 handler 的 catch 上报到该扩展/事件），而不是发成一条
/// `extensionPath: ""` 的合成错误。
#[tokio::test(flavor = "current_thread")]
async fn provider_registration_failures_throw_into_the_extension() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "register",
                r#"
export default function(kiana) {
  kiana.on("session_start", () => {
    kiana.registerVirtualModel({ provider: "", id: "m", name: "M" });
  });
}
"#,
            )])
            .await;
        let event = ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::Start(
            kiana_ext_api::events::SessionStartEvent {
                event_type: kiana_ext_api::events::SessionStartEventType::SessionStart,
                reason: kiana_ext_api::events::SessionStartReason::Startup,
                previous_session_file: None,
            },
        ));
        let _ = runner.emit(event).await.expect("emit");
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1, "errors={errors:?}");
        assert_eq!(errors[0].event, "session_start");
        assert!(
            errors[0]
                .error
                .contains("Virtual model provider and id must not be empty."),
            "error={}",
            errors[0].error
        );
        assert!(
            errors[0].extension_path.ends_with("register.ts"),
            "path={}",
            errors[0].extension_path
        );
    })
    .await;
}

/// 审查 #2（major）：共享事件字段（headers / payload / options）在 handler 抛错前的
/// 就地修改同样保留（TS 的 handler 与 runner 共用同一个对象）。
#[tokio::test(flavor = "current_thread")]
async fn shared_event_mutations_before_a_throw_survive_for_other_shared_fields() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "shared-throw",
                r#"
export default function(kiana) {
  kiana.on("before_provider_headers", (event) => {
    event.headers["x-before-throw"] = "1";
    throw new Error("headers failed");
  });
  kiana.on("before_provider_request", (event) => {
    event.payload.nested.mutated = true;
    throw new Error("payload failed");
  });
}
"#,
            )])
            .await;
        let mut headers = ProviderHeaders::default();
        headers.insert("x-keep".to_owned(), Some("kept".to_owned()));
        let headers = runner
            .emit_before_provider_headers(headers)
            .await
            .expect("headers");
        assert_eq!(
            headers.get("x-before-throw"),
            Some(&Some("1".to_owned())),
            "headers={headers:?}"
        );
        assert_eq!(headers.get("x-keep"), Some(&Some("kept".to_owned())));

        let payload = json!({ "nested": { "mutated": false } });
        let result = runner
            .emit_before_provider_request(payload)
            .await
            .expect("payload");
        assert_eq!(result["nested"]["mutated"], json!(true), "payload={result}");

        let errors = take_collected(&errors);
        let messages = errors
            .iter()
            .map(|error| error.error.as_str())
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["headers failed", "payload failed"]);
    })
    .await;
}

/// 审查 #8（minor）：TS 把同一个事件对象交给所有 handler，任何事件上的就地修改都对
/// 后续 handler 可见（这里用泛型 `emit` 的 `message_start`）。
#[tokio::test(flavor = "current_thread")]
async fn generic_emit_shares_in_place_event_mutations_between_handlers() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "generic-shared",
                r#"
export default function(kiana) {
  kiana.on("message_start", (event) => {
    event.message.content = "mutated-by-a";
  });
  kiana.on("message_start", (event) => {
    throw new Error("b-saw:" + JSON.stringify(event.message.content));
  });
}
"#,
            )])
            .await;
        let event = ExtensionEvent::MessageStart(kiana_ext_api::events::MessageStartEvent {
            event_type: kiana_ext_api::events::MessageStartEventType::MessageStart,
            message: user_message("original"),
        });
        let _ = runner.emit(event).await.expect("emit");
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1, "errors={errors:?}");
        assert_eq!(errors[0].error, "b-saw:\"mutated-by-a\"");
    })
    .await;
}

/// 回归（minor）：高频流式事件（`tool_execution_update`）与其它事件一样共享同一个事件
/// 对象——handler A 的就地修改对 handler B 可见。
///
/// 修复前 `event_requires_readback` 把 `message_update` / `tool_execution_update` /
/// `provider_stream_event` 排除在读回路径外，每个 handler 拿到独立的 JSON 副本。
#[tokio::test(flavor = "current_thread")]
async fn hot_stream_events_share_in_place_event_mutations_between_handlers() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "hot-shared",
                r#"
export default function(kiana) {
  kiana.on("tool_execution_update", (event) => {
    event.partialResult.mutated = "in-place";
  });
  kiana.on("tool_execution_update", (event) => {
    throw new Error("b-saw:" + JSON.stringify(event.partialResult));
  });
}
"#,
            )])
            .await;
        let event = ExtensionEvent::ToolExecutionUpdate(ToolExecutionUpdateEvent {
            event_type: kiana_ext_api::events::ToolExecutionUpdateEventType::ToolExecutionUpdate,
            tool_call_id: "call-1".to_owned(),
            tool_name: "tool".to_owned(),
            args: json!({}),
            partial_result: json!({ "value": 1 }),
            parent_tool_call_id: None,
        });
        let _ = runner.emit(event).await.expect("emit");
        let errors = take_collected(&errors);
        assert_eq!(errors.len(), 1, "errors={errors:?}");
        assert_eq!(
            errors[0].error,
            "b-saw:{\"value\":1,\"mutated\":\"in-place\"}"
        );
    })
    .await;
}

/// 审查 #3（major，部分修复）：未注入渲染器时，`buildSystemPrompt` 的
/// `forceSystemPrompt` 分支仍然生效——handler 就地设置 `systemPromptOptions.forceSystemPrompt`
/// 后，后续 handler 的 `ctx.getSystemPrompt()` 看到该值。
#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_without_renderer_tracks_force_system_prompt() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "force",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event) => {
    event.systemPromptOptions.forceSystemPrompt = "forced";
  });
  kiana.on("before_agent_start", (event, ctx) => ({ systemPrompt: ctx.getSystemPrompt() + "!" }));
}
"#,
            )])
            .await;
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        assert_eq!(
            combined
                .system_prompt_options
                .force_system_prompt
                .as_deref(),
            Some("forced!")
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #8（minor）：`tool_result` 的共享事件对象在 handler 之间可见（A 就地 push
/// `event.content`，B 返回 `event.content`）。
#[tokio::test(flavor = "current_thread")]
async fn tool_result_shares_in_place_content_mutations_between_handlers() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "tool-result-shared",
                r#"
export default function(kiana) {
  kiana.on("tool_result", (event) => {
    event.content.push({ type: "text", text: "in-place" });
  });
  kiana.on("tool_result", (event) => ({ content: event.content }));
}
"#,
            )])
            .await;
        let result = runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("original")]))
            .await
            .expect("tool_result")
            .expect("result");
        let kiana_ext_api::events::OptionalPatch::Value(content) = result.content else {
            panic!("expected content patch, got {:?}", result.content);
        };
        assert_eq!(content.len(), 2, "content={content:?}");
        let last = serde_json::to_value(&content[1]).expect("json");
        assert_eq!(last["text"], json!("in-place"));
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #7（minor）：TS 的 handler 结果是无类型的，调用方只读需要的字段。Rust 侧按
/// JS 语义（truthiness / `String()`）归一化，而不是让整条结果中断整次派发。
#[tokio::test(flavor = "current_thread")]
async fn lenient_handler_results_follow_javascript_coercions() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "lenient",
                r#"
export default function(kiana) {
  kiana.on("session_before_switch", () => ({ cancel: "yes" }));
  kiana.on("session_before_tree", () => ({ summary: { summary: "s", usage: { input: 1 } } }));
  kiana.on("input", () => ({ action: "transform", text: 5 }));
  kiana.on("tool_result", () => ({ content: "not-an-array" }));
}
"#,
            )])
            .await;
        let event = ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::BeforeSwitch(
            kiana_ext_api::events::SessionBeforeSwitchEvent {
                event_type:
                    kiana_ext_api::events::SessionBeforeSwitchEventType::SessionBeforeSwitch,
                reason: kiana_ext_api::events::SessionBeforeSwitchReason::New,
                target_session_file: None,
            },
        ));
        let result = runner
            .emit(event)
            .await
            .expect("session_before_switch")
            .expect("result");
        let ExtensionEventResult::SessionBeforeSwitch(result) = result else {
            panic!("expected session_before_switch result");
        };
        assert_eq!(result.cancel, Some(true));

        let event = ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::BeforeTree(
            kiana_ext_api::events::SessionBeforeTreeEvent::default(),
        ));
        let result = runner
            .emit(event)
            .await
            .expect("session_before_tree")
            .expect("result");
        let ExtensionEventResult::SessionBeforeTree(result) = result else {
            panic!("expected session_before_tree result");
        };
        let summary = result.summary.expect("summary");
        assert_eq!(summary.summary, "s");
        assert_eq!(summary.usage, None);

        let result = runner
            .emit_input("hi", None, InputSource::Interactive, None)
            .await
            .expect("input");
        assert_eq!(
            result,
            InputEventResult::Transform {
                text: "5".to_owned(),
                images: None
            }
        );

        // 非数组 `content` 被忽略（调用方保留原 content），但不会中断派发。
        let _ = runner
            .emit_tool_result(custom_tool_result_event(vec![text_block("original")]))
            .await
            .expect("tool_result");
        assert!(take_collected(&errors).is_empty(), "errors must be empty");
    })
    .await;
}

/// 审查 #1/#8：`context` handler 就地 `splice`（不返回值）时，改动按引用可见并触发
/// 首部 system 消息重建（TS `9789` 的 “applies in-place edits to event.messages without a
/// return value”）。
#[tokio::test(flavor = "current_thread")]
async fn context_applies_in_place_splices_without_a_return_value() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "splice-context",
                r#"
export default function(kiana) {
  kiana.on("context", (event) => {
    event.messages.splice(0, 0, { role: "user", content: [{ type: "text", text: "injected" }], timestamp: 0 });
  });
}
"#,
            )])
            .await;
        let result = runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        let roles = result.iter().map(role_of).collect::<Vec<_>>();
        assert_eq!(roles, vec!["system", "user", "user"], "result={result:?}");
        let injected = serde_json::to_value(&result[1]).expect("json");
        assert_eq!(injected["content"][0]["text"], json!("injected"));
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查 #1（critical）：handler 既就地改内容、又返回 `{ messages: event.messages }` 时，
/// TS 的 `sameMessages` 仍为真（元素引用没变）→ 不重建首部 system 消息。
#[tokio::test(flavor = "current_thread")]
async fn context_in_place_edit_returning_the_same_array_keeps_system_messages() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "edit-and-return",
                r#"
export default function(kiana) {
  kiana.on("context", (event) => {
    const target = event.messages.find((message) => message.role === "user");
    target.content = "edited";
    return { messages: event.messages };
  });
}
"#,
            )])
            .await;
        let result = runner
            .emit_context(vec![
                system_message("prompt"),
                user_message("hello"),
                system_message("mid-conversation"),
            ])
            .await
            .expect("context");
        assert_eq!(result.len(), 3, "result={result:?}");
        assert_eq!(role_of(&result[2]), "system");
        let head = serde_json::to_value(&result[0]).expect("json");
        assert_eq!(head["content"], json!("prompt"), "head={head}");
        let user = serde_json::to_value(&result[1]).expect("json");
        assert_eq!(user["content"], json!("edited"), "user={user}");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// 审查回归：逐 handler 重建的事件对象只采纳嵌套共享对象的原地修改
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn reassigning_per_handler_event_properties_is_ignored() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "reassign",
                r#"
export default function(kiana) {
  // before_provider_headers：`event.headers = {...}` 不被采纳；后一个 handler 仍看到共享 headers。
  kiana.on("before_provider_headers", (event) => {
    event.headers = { replaced: "yes" };
  });
  kiana.on("before_provider_headers", (event) => {
    event.headers["x-seen"] = "1";
  });
  // before_provider_request：`event.payload = {...}` 不被采纳；共享对象上的原地修改生效。
  kiana.on("before_provider_request", (event) => {
    event.payload = { replaced: true };
  });
  kiana.on("before_provider_request", (event) => {
    event.payload.mutated = true;
  });
  // message_end：`event.message = {...}` 不被采纳；后一个 handler 看到共享 message。
  kiana.on("message_end", (event) => {
    event.message = { role: "user", content: "REPLACED", timestamp: 0 };
  });
  kiana.on("message_end", (event) => ({
    message: {
      role: event.message.role,
      content: event.message.content,
      timestamp: event.message.timestamp,
    },
  }));
  // context：`event.messages = []` 不被采纳。
  kiana.on("context", (event) => {
    event.messages = [];
  });
  // boundary：`event.entries = [...]` 不被采纳。
  kiana.on("agent_before_settle", (event) => {
    event.entries = [{ type: "custom", customType: "replaced", data: 1 }];
  });
  // before_agent_start：`event.systemPromptOptions = {...}` 不被采纳；共享对象上的原地修改生效。
  kiana.on("before_agent_start", (event) => {
    event.systemPromptOptions = { customPrompt: "REPLACED" };
  });
  kiana.on("before_agent_start", (event) => {
    event.systemPromptOptions.selectedTools.push("grep");
  });
}
"#,
            )])
            .await;

        let mut original = ProviderHeaders::new();
        original.insert("x-base".to_owned(), Some("1".to_owned()));
        let headers = runner
            .emit_before_provider_headers(original)
            .await
            .expect("headers");
        assert!(
            !headers.contains_key("replaced") && !headers.contains_key("yes"),
            "reassignment leaked into headers: {headers:?}"
        );
        assert_eq!(
            headers.get("x-seen"),
            Some(&Some("1".to_owned())),
            "headers={headers:?}"
        );

        let payload = runner
            .emit_before_provider_request(json!({ "original": true }))
            .await
            .expect("payload");
        assert_eq!(payload, json!({ "original": true, "mutated": true }));

        let message = runner
            .emit_message_end(message_end_event(user_message("original")))
            .await
            .expect("message_end")
            .expect("modified");
        assert_eq!(
            serde_json::to_value(&message).expect("json")["content"],
            json!("original")
        );

        let messages = runner
            .emit_context(vec![system_message("prompt"), user_message("hello")])
            .await
            .expect("context");
        assert_eq!(messages.len(), 2, "messages={messages:?}");

        let base =
            BoundaryBaseEvent::AgentBeforeSettle(kiana_ext_api::events::AgentBeforeSettleEvent {
                entries: Vec::new(),
                r#continue: false,
                context: empty_preview(0),
                outcome: kiana_ext_api::events::AgentActivityOutcome::Completed,
                event_type: kiana_ext_api::events::AgentBeforeSettleEventType::AgentBeforeSettle,
            });
        let result = runner
            .emit_boundary(
                base,
                Arc::new(|entries| Box::pin(async move { Ok(empty_preview(entries.len())) })),
            )
            .await
            .expect("boundary");
        assert!(
            result.entries.is_empty(),
            "reassignment leaked into entries: {:?}",
            result.entries
        );
        assert!(
            take_collected(&errors).is_empty(),
            "errors={:?}",
            take_collected(&errors)
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 审查回归：before_agent_start 的 systemPrompt getter / ctx.getSystemPrompt 按当前选项重渲染
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_reads_rerender_after_in_handler_options_mutation() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "prompt",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event, ctx) => {
    event.systemPromptOptions.selectedTools.push("grep");
    return { systemPrompt: ctx.getSystemPrompt() };
  });
}
"#,
            )])
            .await;
        let renderer: kiana_extensions::runner::SystemPromptRenderer = Arc::new(
            |options: &kiana_ext_api::system_prompt::NormalizedBuildSystemPromptOptions| {
                if let Some(force) = &options.force_system_prompt {
                    return force.clone();
                }
                format!(
                    "{} [{}]",
                    options.custom_prompt.clone().unwrap_or_default(),
                    options.selected_tools.join(",")
                )
            },
        );
        runner.set_system_prompt_renderer(Some(renderer));
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    custom_prompt: Some("base".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        let force = combined
            .system_prompt_options
            .force_system_prompt
            .as_deref()
            .expect("force prompt");
        assert!(
            force.contains("grep"),
            "ctx.getSystemPrompt() must re-render with the handler's live options: {force}"
        );
        assert!(take_collected(&errors).is_empty());

        // `event.systemPrompt` getter 同样按读取时的选项渲染。
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "getter",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event) => {
    event.systemPromptOptions.selectedTools.push("grep");
    return { systemPrompt: event.systemPrompt };
  });
}
"#,
            )])
            .await;
        let renderer: kiana_extensions::runner::SystemPromptRenderer = Arc::new(
            |options: &kiana_ext_api::system_prompt::NormalizedBuildSystemPromptOptions| {
                format!(
                    "{} [{}]",
                    options.custom_prompt.clone().unwrap_or_default(),
                    options.selected_tools.join(",")
                )
            },
        );
        runner.set_system_prompt_renderer(Some(renderer));
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    custom_prompt: Some("base".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        let force = combined
            .system_prompt_options
            .force_system_prompt
            .as_deref()
            .expect("force prompt");
        assert!(
            force.contains("grep"),
            "event.systemPrompt must re-render on every read: {force}"
        );
        assert!(take_collected(&errors).is_empty());

        // 重新赋值 `event.systemPromptOptions` 不影响 getter：TS 的闭包持有原 `currentOptions`；
        // 对原对象的原地修改仍然可见。
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "replaced-options",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event, ctx) => {
    event.systemPromptOptions.selectedTools.push("grep");
    event.systemPromptOptions = { customPrompt: "REPLACED", selectedTools: [] };
    return { systemPrompt: ctx.getSystemPrompt() + "|" + event.systemPrompt };
  });
}
"#,
            )])
            .await;
        let renderer: kiana_extensions::runner::SystemPromptRenderer = Arc::new(
            |options: &kiana_ext_api::system_prompt::NormalizedBuildSystemPromptOptions| {
                format!(
                    "{} [{}]",
                    options.custom_prompt.clone().unwrap_or_default(),
                    options.selected_tools.join(",")
                )
            },
        );
        runner.set_system_prompt_renderer(Some(renderer));
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    custom_prompt: Some("base".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        let force = combined
            .system_prompt_options
            .force_system_prompt
            .as_deref()
            .expect("force prompt");
        assert!(
            force.contains("grep"),
            "live options must stay captured: {force}"
        );
        assert!(!force.contains("REPLACED"), "reassignment leaked: {force}");
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// 审查回归：input.images 按引用语义（返回新数组 = transform；返回原数组 = continue；
// 原地修改的图片随 transform 交回调用方）
// ---------------------------------------------------------------------------

fn input_image(data: &str) -> kiana_ai::types::ImageContent {
    kiana_ai::types::ImageContent {
        kind_image: kiana_ai::types::ImageKind::Image,
        data: data.to_owned(),
        mime_type: "image/png".to_owned(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn input_images_follow_reference_semantics() {
    run_local(async {
        // (a) 返回内容相同但**新**的数组：TS 采纳新引用 → transform。
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "copy",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({
    action: "transform",
    text: event.text,
    images: event.images.map((image) => ({ ...image })),
  }));
}
"#,
            )])
            .await;
        let images = vec![input_image("aGk=")];
        match runner
            .emit_input("hi", Some(images.clone()), InputSource::Interactive, None)
            .await
            .expect("input")
        {
            InputEventResult::Transform { text, images: out } => {
                assert_eq!(text, "hi");
                assert_eq!(out.as_deref(), Some(images.as_slice()));
            }
            other => panic!("expected transform, got {other:?}"),
        }
        assert!(take_collected(&errors).is_empty());

        // (b) 返回的正是事件里的原数组（同一引用）：TS 的 `currentImages !== images` 为 false
        //     → continue。
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "same",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => ({ action: "transform", text: event.text, images: event.images }));
}
"#,
            )])
            .await;
        assert_eq!(
            runner
                .emit_input(
                    "hi",
                    Some(vec![input_image("aGk=")]),
                    InputSource::Interactive,
                    None
                )
                .await
                .expect("input"),
            InputEventResult::Continue
        );
        assert!(take_collected(&errors).is_empty());

        // (c) 原地修改共享图片（不返回值）：TS 里调用方的数组被改；Rust 必须用 transform 把
        //     修改交回调用方（`Continue` 不携带图像）。
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "mutate",
                r#"
export default function(kiana) {
  kiana.on("input", (event) => {
    event.images[0].data = "bW9kaWZpZWQ=";
  });
}
"#,
            )])
            .await;
        match runner
            .emit_input(
                "hi",
                Some(vec![input_image("aGk=")]),
                InputSource::Interactive,
                None,
            )
            .await
            .expect("input")
        {
            InputEventResult::Transform { text, images: out } => {
                assert_eq!(text, "hi");
                let out = out.expect("images");
                assert_eq!(out[0].data, "bW9kaWZpZWQ=");
            }
            other => panic!("expected transform, got {other:?}"),
        }
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}

/// 审查回归（minor）：`before_agent_start` 事件对象与 TS 的自有属性形状一致——
/// user 触发器始终有 `images` 键；`systemPrompt` 是无 setter 的访问器（严格模式下赋值抛
/// TypeError）。
#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_event_shape_matches_typescript() {
    run_local(async {
        let e2e = E2e::new();
        let (runner, errors) = e2e
            .runner(&[(
                "shape",
                r#"
export default function(kiana) {
  kiana.on("before_agent_start", (event) => {
    const hasImages = "images" in event;
    let assigned = "no-throw";
    try {
      event.systemPrompt = "nope";
    } catch (err) {
      assigned = err instanceof TypeError ? "type-error" : "other";
    }
    return { systemPrompt: `${assigned}:${hasImages}:${event.images === undefined}` };
  });
}
"#,
            )])
            .await;
        let combined = runner
            .emit_before_agent_start(
                kiana_extensions::runner::BeforeAgentStartTrigger::User {
                    prompt: "hello".to_owned(),
                    images: None,
                },
                kiana_ext_api::system_prompt::BuildSystemPromptOptions {
                    cwd: e2e.root.path().to_string_lossy().into_owned(),
                    ..Default::default()
                },
            )
            .await
            .expect("before_agent_start");
        assert_eq!(
            combined
                .system_prompt_options
                .force_system_prompt
                .as_deref(),
            Some("type-error:true:true")
        );
        assert!(take_collected(&errors).is_empty());
    })
    .await;
}
