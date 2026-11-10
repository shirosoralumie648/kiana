//! `emitBoundary`（`runner.ts:1036–1090`、`agent-session-boundaries.test.ts`）。
//!
//! 用例覆盖：共享草稿链（`entries`/`continue`/`context` 跨 handler 可见）、每个 handler 后
//! 重新 `buildContext`、无效预览的 `Invalid boundary entries: <msg>` 与后续修复、
//! handler 抛错后共享修改仍然保留、最终无效时返回 `{entries: [], continue: false,
//! context, valid: false}`。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::AgentMessage;
use kiana_config::models::registry::ModelRegistry;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{
    AgentActivityOutcome, AgentBeforeSettleEvent, AgentBeforeSettleEventType,
    BoundaryContextPreview, BoundaryResult, ExtensionEvent, ExtensionEventResult, ExtensionHandler,
    ExtensionHandlerFn, SessionBoundaryDraft,
};
use kiana_ext_api::types::{Extension, ExtensionError};
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::runner::context;
use kiana_extensions::runner::dispatch;
use kiana_extensions::runner::{BoundaryBaseEvent, BuildBoundaryContext, ExtensionRunner};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use serde_json::{Value as JsonValue, json};

// ===========================================================================
// harness
// ===========================================================================

#[derive(Default)]
struct FakeSessionManager;

impl ReadonlySessionManager for FakeSessionManager {
    fn get_cwd(&self) -> &str {
        "/tmp"
    }
    fn get_session_dir(&self) -> &str {
        ""
    }
    fn get_session_id(&self) -> &str {
        "fake"
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

struct TestRunner {
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
    engine::install(&engine).expect("prelude");
    context::install(&engine).expect("context.js");
    dispatch::install(Rc::clone(&engine)).expect("dispatch.js");
    use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
        .await
        .expect("model runtime");
    let model_registry = Arc::new(ModelRegistry::new(Arc::new(model_runtime)));
    let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager);
    let runner = Arc::new(ExtensionRunner::new(
        extensions,
        loader::create_extension_runtime(),
        PathBuf::from("/tmp/boundary"),
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

fn extension(path: &str, handlers: Vec<ExtensionHandler>) -> Extension {
    let mut map: IndexMap<String, Vec<ExtensionHandler>> = IndexMap::new();
    if !handlers.is_empty() {
        map.insert("agent_before_settle".to_owned(), handlers);
    }
    Extension {
        path: path.to_owned(),
        resolved_path: path.to_owned(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: path.to_owned(),
            source: "local".to_owned(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
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

fn base_event() -> BoundaryBaseEvent {
    BoundaryBaseEvent::AgentBeforeSettle(AgentBeforeSettleEvent {
        entries: Vec::new(),
        r#continue: false,
        context: preview(0),
        outcome: AgentActivityOutcome::Completed,
        event_type: AgentBeforeSettleEventType::AgentBeforeSettle,
    })
}

fn preview(context_entries: usize) -> BoundaryContextPreview {
    BoundaryContextPreview {
        context_entries: Vec::new(),
        context_messages: (0..context_entries)
            .map(|_| custom_message("preview"))
            .collect(),
        llm_messages: Vec::new(),
        pending_messages: Vec::new(),
        can_continue: false,
    }
}

fn custom_message(text: &str) -> AgentMessage {
    let mut object: IndexMap<String, JsonValue> = IndexMap::new();
    object.insert("role".to_owned(), json!("user"));
    object.insert("content".to_owned(), json!(text));
    object.insert("timestamp".to_owned(), json!(0));
    AgentMessage::Custom(Box::new(object))
}

fn custom_draft(custom_type: &str) -> SessionBoundaryDraft {
    SessionBoundaryDraft::Custom(kiana_ext_api::events::CustomEntryDraft {
        event_type: kiana_ext_api::events::CustomEntryDraftType::Custom,
        custom_type: custom_type.to_owned(),
        data: None,
    })
}

fn build_context(log: Arc<Mutex<Vec<usize>>>) -> BuildBoundaryContext {
    Arc::new(move |entries| {
        let log = Arc::clone(&log);
        let count = entries.len();
        Box::pin(async move {
            log.lock().unwrap().push(count);
            Ok(preview(count))
        })
    })
}

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

// ===========================================================================
// 用例
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn chains_shared_draft_proposals_and_preserves_omitted_result_fields() {
    run_local(async {
        let observations = Arc::new(Mutex::new(Vec::<(usize, bool, usize)>::new()));
        let first_observations = Arc::clone(&observations);
        let second_observations = Arc::clone(&observations);
        let first = extension(
            "/ext/first.ts",
            vec![standard(move |event, _ctx| {
                let observations = Arc::clone(&first_observations);
                Box::pin(async move {
                    let ExtensionEvent::AgentBeforeSettle(event) = event else {
                        panic!("expected agent_before_settle");
                    };
                    observations.lock().unwrap().push((
                        event.entries.len(),
                        event.r#continue,
                        event.context.context_messages.len(),
                    ));
                    Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                        BoundaryResult {
                            entries: Some(vec![custom_draft("first")]),
                            r#continue: Some(true),
                        },
                    )))
                })
            })],
        );
        let second = extension(
            "/ext/second.ts",
            vec![standard(move |event, _ctx| {
                let observations = Arc::clone(&second_observations);
                Box::pin(async move {
                    let ExtensionEvent::AgentBeforeSettle(event) = event else {
                        panic!("expected agent_before_settle");
                    };
                    observations.lock().unwrap().push((
                        event.entries.len(),
                        event.r#continue,
                        event.context.context_messages.len(),
                    ));
                    Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                        BoundaryResult {
                            entries: Some(Vec::new()),
                            r#continue: None,
                        },
                    )))
                })
            })],
        );
        let test = make_runner(vec![first, second]).await;
        let log = Arc::new(Mutex::new(Vec::new()));
        let result = test
            .runner
            .emit_boundary(base_event(), build_context(Arc::clone(&log)))
            .await
            .expect("boundary");
        assert_eq!(
            *observations.lock().unwrap(),
            vec![(0, false, 0), (1, true, 1)]
        );
        assert!(result.entries.is_empty());
        assert!(result.r#continue);
        assert!(result.valid);
        // 每个 handler 之后都重新构建（2 个 handler → 2 次）+ 初始一次。
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 0]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reports_invalid_previews_and_lets_later_handlers_repair_the_proposal() {
    run_local(async {
        let second_ran = Arc::new(Mutex::new(false));
        let first = extension(
            "/ext/invalid.ts",
            vec![standard(|_event, _ctx| {
                Box::pin(async {
                    Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                        BoundaryResult {
                            entries: Some(vec![SessionBoundaryDraft::ContextEdit(
                                kiana_ext_api::events::ContextEditEntryDraft {
                                    event_type:
                                        kiana_ext_api::events::ContextEditEntryDraftType::ContextEdit,
                                    target_id: "missing".to_owned(),
                                    replacement: None,
                                },
                            )]),
                            r#continue: None,
                        },
                    )))
                })
            })],
        );
        let second = {
            let second_ran = Arc::clone(&second_ran);
            extension(
                "/ext/repair.ts",
                vec![standard(move |event, _ctx| {
                    let second_ran = Arc::clone(&second_ran);
                    Box::pin(async move {
                        *second_ran.lock().unwrap() = true;
                        let ExtensionEvent::AgentBeforeSettle(event) = event else {
                            panic!("expected agent_before_settle");
                        };
                        assert_eq!(event.entries.len(), 1);
                        Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                            BoundaryResult {
                                entries: Some(Vec::new()),
                                r#continue: None,
                            },
                        )))
                    })
                })],
            )
        };
        let test = make_runner(vec![first, second]).await;
        let build: BuildBoundaryContext = Arc::new(|entries| {
            let invalid = entries.iter().any(|entry| {
                matches!(
                    entry,
                    SessionBoundaryDraft::ContextEdit(draft) if draft.target_id == "missing"
                )
            });
            Box::pin(async move {
                if invalid {
                    Err("Entry missing not found".to_owned())
                } else {
                    Ok(preview(0))
                }
            })
        });
        let result = test
            .runner
            .emit_boundary(base_event(), build)
            .await
            .expect("boundary");
        assert!(*second_ran.lock().unwrap());
        let errors = test.take_errors();
        assert!(
            errors
                .iter()
                .any(|error| error.error == "Invalid boundary entries: Entry missing not found"),
            "errors: {errors:?}"
        );
        assert!(result.entries.is_empty());
        assert!(result.valid);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn final_invalid_build_returns_empty_entries_and_not_valid() {
    run_local(async {
        let ext = extension(
            "/ext/invalid.ts",
            vec![standard(|_event, _ctx| {
                Box::pin(async {
                    Ok(Some(ExtensionEventResult::AgentBeforeSettle(
                        BoundaryResult {
                            entries: Some(vec![custom_draft("kept")]),
                            r#continue: Some(true),
                        },
                    )))
                })
            })],
        );
        let test = make_runner(vec![ext]).await;
        let build: BuildBoundaryContext = Arc::new(|entries| {
            let invalid = !entries.is_empty();
            Box::pin(async move {
                if invalid {
                    Err("nope".to_owned())
                } else {
                    Ok(preview(0))
                }
            })
        });
        let result = test
            .runner
            .emit_boundary(base_event(), build)
            .await
            .expect("boundary");
        assert!(result.entries.is_empty());
        assert!(!result.r#continue);
        assert!(!result.valid);
        let errors = test.take_errors();
        assert_eq!(errors[0].error, "Invalid boundary entries: nope");
    })
    .await;
}

/// 共享修改在 handler 抛错后仍然保留（JS 事件对象的 `entries` 就是共享草稿）。
#[test]
fn keeps_shared_mutations_made_before_a_handler_throws() {
    let engine = Rc::new(JsEngine::new().expect("engine"));
    engine::install(&engine).expect("prelude");
    dispatch::install(Rc::clone(&engine)).expect("dispatch");
    // fake host：handler 原地 push 后抛错；buildContext 记录收到的条目数。
    let script = r#"
      (async () => {
        globalThis.__seen = [];
        globalThis.__reports = [];
        globalThis.__kiana_dispatch_host = {
          call(token, hid, event) {
            if (hid === 0) {
              event.entries.push({ type: "custom", customType: "kept" });
              return Promise.reject(new Error("boundary failed"));
            }
            return Promise.resolve({ hasValue: false, value: null });
          },
          buildContext(token, entries) {
            globalThis.__seen.push(entries.length);
            return Promise.resolve({
              contextEntries: [], contextMessages: [], llmMessages: [],
              pendingMessages: [], canContinue: false,
            });
          },
          reportError(payload) { globalThis.__reports.push(payload); },
          setSystemPrompt() {},
        };
        return await __kiana_dispatch.emitBoundary({
          token: 1,
          type: "agent_before_settle",
          paths: ["/ext/throws.ts"],
          snapshot: [[0]],
          base: { type: "agent_before_settle", outcome: "completed" },
          entries: [],
          context: null,
        });
      })()
    "#;
    let engine_ref = &engine;
    let result: JsonValue = engine_ref.with_js(|ctx| {
        let wrapped = format!("await ({script});");
        let promise = ctx.eval_promise(wrapped).expect("eval");
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
                        return match value {
                            JsonValue::Object(mut object)
                                if object.len() == 1 && object.contains_key("value") =>
                            {
                                object.shift_remove("value").unwrap_or(JsonValue::Null)
                            }
                            other => other,
                        };
                    }
                    Err(_) => panic!("promise rejected"),
                }
            }
            if !ctx.execute_pending_job() {
                panic!("promise did not settle");
            }
        }
    });
    assert_eq!(result["entries"][0]["customType"], "kept");
    assert_eq!(result["valid"], true);
    let seen: String = engine
        .eval_script("JSON.stringify(__seen)", "<t>")
        .expect("seen");
    // 初始构建一次 + handler 抛错后（共享草稿仍是 1 条）再构建一次。
    assert_eq!(seen, "[0,1]");
    let reports: String = engine
        .eval_script("JSON.stringify(__reports)", "<t>")
        .expect("reports");
    let reports: JsonValue = serde_json::from_str(&reports).unwrap();
    assert_eq!(reports[0]["error"], "boundary failed");
}
