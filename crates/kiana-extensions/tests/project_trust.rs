//! `emit_project_trust_event`（`runner.ts:302–330`、`8261-subagent-project-trust.test.ts`）。
//!
//! 用例覆盖：`undecided` 继续、第一个 yes/no 获胜、后续 handler 跳过（含同一扩展内的多个
//! handler）、handler 抛错收进 `errors` 且继续、没有 handler 时返回空结果。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_config::models::registry::ModelRegistry;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::events::{
    ExtensionEvent, ProjectTrustContext, ProjectTrustEvent, ProjectTrustEventDecision,
    ProjectTrustEventResult, ProjectTrustEventType, ProjectTrustHandler, ProjectTrustUi,
};
use kiana_ext_api::types::{Extension, ExtensionMode, LoadExtensionsResult};
use kiana_ext_api::{ExtensionHandler, LocalBoxFuture};
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::project_trust::{ProjectTrustOutcome, emit_project_trust_event};
use kiana_extensions::runner::dispatch;
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};

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

fn install_engine() -> Rc<JsEngine> {
    let engine = Rc::new(JsEngine::new().expect("engine"));
    engine::install(&engine).expect("prelude");
    dispatch::install(Rc::clone(&engine)).expect("dispatch");
    engine
}

fn project_trust_handler<F>(handler: F) -> ExtensionHandler
where
    F: Fn(
            ProjectTrustEvent,
            ProjectTrustContext,
        ) -> LocalBoxFuture<'static, ProjectTrustEventResult>
        + 'static,
{
    ExtensionHandler::ProjectTrust(Arc::new(handler) as ProjectTrustHandler)
}

fn extension(path: &str, handlers: Vec<ExtensionHandler>) -> Extension {
    let mut map: IndexMap<String, Vec<ExtensionHandler>> = IndexMap::new();
    if !handlers.is_empty() {
        map.insert("project_trust".to_owned(), handlers);
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

fn result(trusted: ProjectTrustEventDecision) -> ProjectTrustEventResult {
    ProjectTrustEventResult {
        trusted,
        remember: Some(true),
    }
}

fn event() -> ProjectTrustEvent {
    ProjectTrustEvent {
        event_type: ProjectTrustEventType::ProjectTrust,
        cwd: "/tmp/project".to_owned(),
    }
}

fn context() -> ProjectTrustContext {
    ProjectTrustContext {
        cwd: "/tmp/project".to_owned(),
        mode: ExtensionMode::Tui,
        has_ui: false,
        ui: ProjectTrustUi {
            select: Arc::new(|_title, _options, _opts| Box::pin(async { None })),
            confirm: Arc::new(|_title, _message, _opts| Box::pin(async { false })),
            input: Arc::new(|_title, _placeholder, _opts| Box::pin(async { None })),
            notify: Arc::new(|_message, _kind| {}),
        },
    }
}

fn load_result(extensions: Vec<Extension>) -> LoadExtensionsResult {
    let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager);
    let _ = session;
    LoadExtensionsResult {
        extensions,
        errors: Vec::new(),
        warnings: None,
        runtime: loader::create_extension_runtime(),
    }
}

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

#[tokio::test(flavor = "current_thread")]
async fn continues_past_undecided_handlers_and_returns_the_first_yes_no_decision() {
    run_local(async {
        let _engine = install_engine();
        let undecided = extension(
            "/ext/undecided.ts",
            vec![project_trust_handler(|_event, _ctx| {
                Box::pin(async { result(ProjectTrustEventDecision::Undecided) })
            })],
        );
        let decided = extension(
            "/ext/decided.ts",
            vec![project_trust_handler(|_event, _ctx| {
                Box::pin(async { result(ProjectTrustEventDecision::No) })
            })],
        );
        let extensions = load_result(vec![undecided, decided]);
        let ProjectTrustOutcome {
            result: decided,
            errors,
        } = emit_project_trust_event(&extensions, event(), context()).await;
        assert_eq!(decided, Some(result(ProjectTrustEventDecision::No)));
        assert!(errors.is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn skips_later_handlers_after_the_first_decision() {
    run_local(async {
        let _engine = install_engine();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let extensions = load_result(vec![
            extension(
                "/ext/a.ts",
                vec![project_trust_handler(move |_event, _ctx| {
                    let calls = Arc::clone(&first_calls);
                    Box::pin(async move {
                        calls.lock().unwrap().push("a".to_owned());
                        result(ProjectTrustEventDecision::Yes)
                    })
                })],
            ),
            extension(
                "/ext/b.ts",
                vec![project_trust_handler(move |_event, _ctx| {
                    let calls = Arc::clone(&second_calls);
                    Box::pin(async move {
                        calls.lock().unwrap().push("b".to_owned());
                        result(ProjectTrustEventDecision::No)
                    })
                })],
            ),
        ]);
        let outcome = emit_project_trust_event(&extensions, event(), context()).await;
        assert_eq!(outcome.result, Some(result(ProjectTrustEventDecision::Yes)));
        assert_eq!(*calls.lock().unwrap(), vec!["a"]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_handlers_in_one_extension_are_all_consulted() {
    run_local(async {
        let _engine = install_engine();
        let extensions = load_result(vec![extension(
            "/ext/multi.ts",
            vec![
                project_trust_handler(|_event, _ctx| {
                    Box::pin(async { result(ProjectTrustEventDecision::Undecided) })
                }),
                project_trust_handler(|_event, _ctx| {
                    Box::pin(async { result(ProjectTrustEventDecision::Yes) })
                }),
            ],
        )]);
        let outcome = emit_project_trust_event(&extensions, event(), context()).await;
        assert_eq!(outcome.result, Some(result(ProjectTrustEventDecision::Yes)));
        assert!(outcome.errors.is_empty());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn handler_failures_are_collected_and_later_handlers_still_run() {
    run_local(async {
        let _engine = install_engine();
        // `kiana-ext-api` 的 `ProjectTrustHandler` 没有错误返回通道（TS 的 handler 可以抛错）；
        // 这里用一个挂在 `project_trust` 下的标准 handler 触发桥接错误，验证错误收集与
        // “继续下一个 handler”的语义。
        let mut broken = extension("/ext/broken.ts", Vec::new());
        broken.handlers.insert(
            "project_trust".to_owned(),
            vec![ExtensionHandler::Standard(Arc::new(
                |_event: ExtensionEvent,
                 _ctx|
                 -> LocalBoxFuture<
                    'static,
                    Result<
                        Option<kiana_ext_api::events::ExtensionEventResult>,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > { Box::pin(async { Ok(None) }) },
            )
                as kiana_ext_api::events::ExtensionHandlerFn)],
        );
        let decided = extension(
            "/ext/decided.ts",
            vec![project_trust_handler(|_event, _ctx| {
                Box::pin(async { result(ProjectTrustEventDecision::No) })
            })],
        );
        let extensions = load_result(vec![broken, decided]);
        let outcome = emit_project_trust_event(&extensions, event(), context()).await;
        assert_eq!(outcome.result, Some(result(ProjectTrustEventDecision::No)));
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].extension_path, "/ext/broken.ts");
        assert_eq!(outcome.errors[0].event, "project_trust");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_no_result_without_handlers() {
    run_local(async {
        let _engine = install_engine();
        let extensions = load_result(vec![extension("/ext/empty.ts", Vec::new())]);
        let outcome = emit_project_trust_event(&extensions, event(), context()).await;
        assert_eq!(outcome.result, None);
        assert!(outcome.errors.is_empty());
    })
    .await;
}

// 保留一个显式的 `PathBuf` 引用，避免未使用导入（测试构造里 cwd 用字符串）。
#[allow(dead_code)]
fn _unused(_: PathBuf) {}

#[allow(dead_code)]
fn _assert_model_registry(_: ModelRegistry) {}
