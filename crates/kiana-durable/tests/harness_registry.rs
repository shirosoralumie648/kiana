//! `test/harness-registry.test.ts` 的 Rust 端口。
//!
//! 注册表的发布/快照语义、安装校验、内建任务表，以及 `resolveAgent` 的选择、包装、
//! section 渲染与 hook 顺序。

#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::ModelThinkingLevel;
use kiana_chord::context::Context;
use kiana_durable::harness::agent::{agent_hooks, resolve_agent, resolve_settings};
use kiana_durable::harness::define::{define_extension, section, wrap_section, wrap_tool};
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::RegistryReader;
use kiana_durable::harness::types::{
    Agent, AgentState, AnyTask, Extension, ExtensionSelection, HarnessSettings, HookRegistration,
    ModelRef, PromptInput, PromptSection, Registry, RegistrySnapshot, ToolRegistration,
    ToolSelection,
};
use kiana_durable::types::{
    BoxError, JsonValue, TaskDefinition, TaskPhase, TaskRecord, TaskRuntime,
};
use serde_json::json;

use harness_support::{EmptyReader, TestExtension, tool, tool_described};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn names(items: &[Arc<dyn Extension>]) -> Vec<String> {
    items.iter().map(|item| item.name().to_owned()).collect()
}

fn task_names(tasks: &[AnyTask]) -> Vec<String> {
    tasks
        .iter()
        .map(|task| task.definition.name.clone())
        .collect()
}

struct Noop;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Noop {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

/// TS: `task(name, version)`：一个只跑一次的 `run` 阶段任务。
fn task(name: &str, version: i64) -> AnyTask {
    let definition: TaskDefinition = TaskDefinition {
        name: name.to_owned(),
        version,
        initial: Arc::new(|_| json!({ "phase": "run" })),
        phases: IndexMap::from([(
            "run".to_owned(),
            Arc::new(Noop) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(Noop) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

/// 收集安装校验 panic 里的错误文本。
fn install_error(registry: &Arc<impl Registry>, extension: Arc<dyn Extension>) -> String {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.install(extension);
    }))
    .expect_err("the install is rejected");
    match payload.downcast::<BoxError>() {
        Ok(error) => error.to_string(),
        Err(_) => "non-error panic".to_owned(),
    }
}

/// TS: `resolve()`：用当前快照与设置解析，收集报告。
fn resolve(
    state: Option<&AgentState>,
    snapshot: &dyn RegistrySnapshot,
    settings: Option<&HarnessSettings>,
) -> (Agent, Vec<String>) {
    let mut reports: Vec<String> = Vec::new();
    let agent = resolve_agent(
        state,
        snapshot,
        &resolve_settings(settings),
        &mut |error, _extension| reports.push(error.to_string()),
    );
    (agent, reports)
}

/// TS: `rendered(agent)`。
async fn rendered(agent: &Agent) -> Vec<(String, Option<String>)> {
    let input = PromptInput {
        conversation_id: kiana_durable::types::id_from_number::<kiana_durable::ConversationBrand>(
            1,
        ),
        agent: Arc::new(agent.clone()),
        env: None,
        shown: IndexMap::new(),
        read: Arc::new(EmptyReader),
    };
    let mut rendered = Vec::new();
    for item in &agent.sections {
        let value = (item.render(&input, &*harness_support::context()))
            .await
            .expect("the section renders");
        rendered.push((item.key().to_owned(), value));
    }
    rendered
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

#[test]
fn installs_replaces_in_place_and_uninstalls_extensions_by_name() {
    let registry = create_registry_impl();
    let listener: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&listener);
    let snapshot_reader: Arc<dyn RegistryReader> = Arc::clone(&registry) as Arc<dyn RegistryReader>;
    let listener_reader = Arc::clone(&snapshot_reader);
    let _unsubscribe = snapshot_reader.subscribe(Arc::new(move || {
        sink.lock()
            .unwrap()
            .push(names(&listener_reader.snapshot().installed()).join(","));
    }));

    let a = define_extension(
        TestExtension::new("a").with_tools(vec![tool_described("read", "Read files")]),
    );
    let b = define_extension(TestExtension::new("b").with_tools(vec![tool("read"), tool("bash")]));
    registry.install(Arc::clone(&a));
    registry.install(Arc::clone(&b));
    let before = registry.snapshot();
    assert_eq!(names(&before.installed()), vec!["a", "b"]);

    // 同名安装按位置替换。
    let a2 = define_extension(TestExtension::new("a").with_tools(vec![tool("grep")]));
    registry.install(Arc::clone(&a2));
    assert_eq!(names(&registry.snapshot().installed()), vec!["a", "b"]);
    assert!(Arc::ptr_eq(
        &registry.snapshot().extension("a").expect("a"),
        &a2
    ));
    let tools: Vec<String> = registry
        .snapshot()
        .tools()
        .iter()
        .map(|entry| format!("{}@{}", entry.tool.name(), entry.extension.name()))
        .collect();
    assert_eq!(tools, vec!["grep@a", "read@b", "bash@b"]);
    // 旧快照不变。
    assert!(Arc::ptr_eq(&before.extension("a").expect("a"), &a));
    assert_eq!(before.tools()[0].tool.tool.description, "Read files");

    // uninstall 只看名字；之后再安装追加到末尾。
    registry.uninstall("a");
    registry.uninstall("a");
    assert_eq!(names(&registry.snapshot().installed()), vec!["b"]);
    registry.install(Arc::clone(&a));
    assert_eq!(names(&registry.snapshot().installed()), vec!["b", "a"]);
    assert_eq!(
        *listener.lock().unwrap(),
        vec!["a", "a,b", "a,b", "b", "b,a"]
    );
}

#[test]
fn validates_the_registry_as_it_would_be_after_an_install() {
    let registry = create_registry_impl();
    registry.install(define_extension(
        TestExtension::new("tasks").with_tasks(vec![task("app.index", 1)]),
    ));
    let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&published);
    let reader: Arc<dyn RegistryReader> = Arc::clone(&registry) as Arc<dyn RegistryReader>;
    let _unsubscribe = reader.subscribe(Arc::new(move || {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let before = registry.snapshot();

    let errors = [
        (
            define_extension(TestExtension::new("x").with_tools(vec![tool("read"), tool("read")])),
            "two tools named read",
        ),
        (
            define_extension(TestExtension::new("x").with_sections(vec![
                section("a", render_const("1"), None),
                section("a", render_const("2"), None),
            ])),
            "two sections",
        ),
        (
            define_extension(TestExtension::new("x").with_sections(vec![section(
                "Bad Key",
                render_const(""),
                None,
            )])),
            "must match",
        ),
        (
            define_extension(TestExtension::new("x").with_sections(vec![section(
                "instructions",
                render_const(""),
                None,
            )])),
            "reserved",
        ),
        (
            define_extension(TestExtension::new("x").with_tasks(vec![task("kiana.generation", 1)])),
            "already installed",
        ),
        (
            define_extension(TestExtension::new("x").with_tasks(vec![task("app.index", 1)])),
            "already installed",
        ),
    ];
    for (extension, message) in errors {
        let error = install_error(&registry, extension);
        assert!(error.contains(message), "{error} lacks {message}");
    }
    assert!(Arc::ptr_eq(&registry.snapshot(), &before));
    assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 0);

    // 替换持有任务名的扩展是合法的：检查针对替换后的状态。
    registry.install(define_extension(
        TestExtension::new("tasks").with_tasks(vec![task("app.index", 2)]),
    ));
    assert_eq!(
        registry
            .snapshot()
            .task("app.index")
            .expect("app.index")
            .definition
            .version,
        2
    );
}

#[test]
fn always_holds_the_built_in_tasks_which_are_not_an_extension() {
    let registry = create_registry_impl();
    assert!(registry.snapshot().installed().is_empty());
    assert_eq!(
        task_names(&registry.snapshot().tasks()),
        vec!["kiana.generation", "kiana.tool", "kiana.compaction"]
    );
    let custom = task("app.custom", 1);
    registry.install(define_extension(
        TestExtension::new("custom").with_tasks(vec![custom.clone()]),
    ));
    assert_eq!(
        task_names(&registry.snapshot().tasks()),
        vec![
            "kiana.generation",
            "kiana.tool",
            "kiana.compaction",
            "app.custom"
        ]
    );
    registry.uninstall("custom");
    assert!(registry.snapshot().task("app.custom").is_none());
}

#[test]
fn keeps_the_default_of_a_progress_interval_given_as_undefined() {
    let settings = HarnessSettings {
        progress: Some(kiana_durable::harness::types::ProgressSettings {
            partial_interval_ms: None,
            output_interval_ms: Some(250.0),
            persist_within_ms: None,
        }),
        ..Default::default()
    };
    let resolved = resolve_settings(Some(&settings));
    let progress = resolved.progress;
    assert_eq!(progress.partial_interval_ms, 100.0);
    assert_eq!(progress.output_interval_ms, 250.0);
    assert_eq!(progress.persist_within_ms, 1_000.0);
}

// ---------------------------------------------------------------------------
// agent resolution
// ---------------------------------------------------------------------------

fn render_const(text: &'static str) -> kiana_durable::harness::define::SectionRender {
    Arc::new(move |_input, _context| Box::pin(async move { Ok(Some(text.to_owned())) }))
}

struct Fixture {
    registry: Arc<kiana_durable::harness::registry::RegistryImpl>,
    coding: Arc<dyn Extension>,
    skills: Arc<dyn Extension>,
}

fn agent_fixture() -> Fixture {
    let read = tool("read");
    let bash = tool("bash");
    let edit = tool("edit");
    let coding = define_extension(
        TestExtension::new("coding")
            .with_tools(vec![read, bash, edit])
            .with_sections(vec![
                section("preamble", render_const("You code."), Some(Some(false))),
                section("cwd", render_const("/repo"), None),
            ]),
    );
    let skills = define_extension(TestExtension::new("skills").with_sections(vec![section(
        "skills",
        render_const("S"),
        None,
    )]));
    let reviewer = define_extension(TestExtension::new("reviewer").with_sections(vec![section(
        "role",
        render_const("Review."),
        None,
    )]));
    let registry = create_registry_impl();
    for extension in [&coding, &skills, &reviewer] {
        registry.install(Arc::clone(extension));
    }
    let _ = reviewer;
    Fixture {
        registry,
        coding,
        skills,
    }
}

#[test]
fn selects_the_default_an_array_or_the_default_edited_by_add_and_remove() {
    let fixture = agent_fixture();
    let snapshot = fixture.registry.snapshot();
    let (agent, _) = resolve(None, &*snapshot, None);
    assert_eq!(
        names(&agent.extensions),
        vec!["coding", "skills", "reviewer"]
    );

    let settings = HarnessSettings {
        extensions: Some(vec![
            Arc::clone(&fixture.coding),
            Arc::clone(&fixture.skills),
        ]),
        ..Default::default()
    };
    let (agent, _) = resolve(Some(&empty_agent_state()), &*snapshot, Some(&settings));
    assert_eq!(names(&agent.extensions), vec!["coding", "skills"]);

    let (agent, _) = resolve(
        Some(&AgentState {
            extensions: Some(ExtensionSelection::List(vec![
                "reviewer".to_owned(),
                "coding".to_owned(),
            ])),
            ..empty_agent_state()
        }),
        &*snapshot,
        Some(&settings),
    );
    assert_eq!(names(&agent.extensions), vec!["reviewer", "coding"]);

    // add 追加，remove 删除，重复保留第一个位置，未安装的名字被跳过。
    let (agent, _) = resolve(
        Some(&AgentState {
            extensions: Some(ExtensionSelection::Edit {
                add: Some(vec![
                    "reviewer".to_owned(),
                    "coding".to_owned(),
                    "gone".to_owned(),
                ]),
                remove: Some(vec!["skills".to_owned()]),
            }),
            ..empty_agent_state()
        }),
        &*snapshot,
        Some(&settings),
    );
    assert_eq!(names(&agent.extensions), vec!["coding", "reviewer"]);

    // 旧对象按名字代表：解析到已安装的那个扩展。
    let stale = HarnessSettings {
        extensions: Some(vec![
            define_extension(TestExtension::new("skills")),
            Arc::clone(&fixture.skills),
        ]),
        ..Default::default()
    };
    let (agent, _) = resolve(Some(&empty_agent_state()), &*snapshot, Some(&stale));
    assert!(Arc::ptr_eq(&agent.extensions[0], &fixture.skills));
}

fn empty_agent_state() -> AgentState {
    AgentState {
        model: None,
        thinking_level: None,
        extensions: None,
        tools: None,
        model_tools: None,
        instructions: None,
        cwd: None,
    }
}

#[test]
fn skips_uninstalled_names_and_resolves_them_again_once_installed() {
    let read = tool("read");
    let coding = define_extension(TestExtension::new("coding").with_tools(vec![read]));
    let skills = define_extension(TestExtension::new("skills"));
    let local = create_registry_impl();
    local.install(coding);
    let state = AgentState {
        extensions: Some(ExtensionSelection::List(vec![
            "coding".to_owned(),
            "skills".to_owned(),
        ])),
        ..empty_agent_state()
    };
    let (agent, _) = resolve(Some(&state), &*local.snapshot(), None);
    assert_eq!(names(&agent.extensions), vec!["coding"]);
    local.install(skills);
    let (agent, _) = resolve(Some(&state), &*local.snapshot(), None);
    assert_eq!(names(&agent.extensions), vec!["coding", "skills"]);
}

#[test]
fn replaces_same_name_tools_in_place_wraps_the_winner_then_applies_the_filter() {
    let fixture = agent_fixture();
    let local = create_registry_impl();
    let venv_bash = tool_described("bash", "venv bash");
    let venv = define_extension(TestExtension::new("venv").with_tools(vec![venv_bash]));
    let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let timing = define_extension(TestExtension::new("timing").with_wraps(vec![
        wrap_tool(
            &tool("bash"),
            Arc::new(|inner: ToolRegistration| {
                let mut next = inner;
                next.tool.description = format!("{} (timed)", next.tool.description);
                next
            }),
        ),
        wrap_tool(
            &tool("bash"),
            Arc::new(|inner: ToolRegistration| {
                let mut next = inner;
                next.tool.description = format!("{} [2]", next.tool.description);
                next
            }),
        ),
        wrap_tool(&tool("grep"), {
            let calls = Arc::clone(&calls);
            Arc::new(move |inner: ToolRegistration| {
                calls.lock().unwrap().push("grep".to_owned());
                inner
            })
        }),
    ]));
    local.install(Arc::clone(&fixture.coding));
    local.install(venv);
    local.install(timing);

    let (agent, reports) = resolve(None, &*local.snapshot(), None);
    let described: Vec<(String, String)> = agent
        .tools
        .iter()
        .map(|tool| (tool.name().to_owned(), tool.tool.description.clone()))
        .collect();
    assert_eq!(
        described,
        vec![
            ("read".to_owned(), "read tool".to_owned()),
            ("bash".to_owned(), "venv bash (timed) [2]".to_owned()),
            ("edit".to_owned(), "edit tool".to_owned()),
        ]
    );
    assert!(reports.is_empty());
    assert!(calls.lock().unwrap().is_empty());

    // 数组按顺序精确保留这些名字；重复名字保留第一个位置。
    let (filtered, _) = resolve(
        Some(&AgentState {
            tools: Some(ToolSelection::List(vec![
                "edit".to_owned(),
                "missing".to_owned(),
                "read".to_owned(),
                "edit".to_owned(),
            ])),
            ..empty_agent_state()
        }),
        &*local.snapshot(),
        None,
    );
    let names: Vec<String> = filtered
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, vec!["edit", "read"]);

    let (removed, _) = resolve(
        Some(&AgentState {
            tools: Some(ToolSelection::Remove {
                remove: vec!["bash".to_owned()],
            }),
            ..empty_agent_state()
        }),
        &*local.snapshot(),
        None,
    );
    let names: Vec<String> = removed
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, vec!["read", "edit"]);
}

#[test]
fn drops_a_tool_or_section_whose_wrapper_throws_or_renames_it_and_reports_the_failure() {
    let read = tool("read");
    let edit = tool("edit");
    let coding = define_extension(
        TestExtension::new("coding")
            .with_tools(vec![read.clone(), tool("bash"), edit.clone()])
            .with_sections(vec![
                section("preamble", render_const("You code."), Some(Some(false))),
                section("cwd", render_const("/repo"), None),
            ]),
    );
    let broken = define_extension(TestExtension::new("broken").with_wraps(vec![
        wrap_tool(
            &read,
            Arc::new(|_inner: ToolRegistration| -> ToolRegistration { panic!("wrapper failed") }),
        ),
        wrap_tool(
            &edit,
            Arc::new(|inner: ToolRegistration| {
                let mut next = inner;
                next.tool.name = "renamed".to_owned();
                next
            }),
        ),
        wrap_section(
            "cwd",
            Arc::new(|_inner: Arc<dyn PromptSection>| -> Arc<dyn PromptSection> {
                panic!("section wrapper failed")
            }),
        ),
    ]));
    let local = create_registry_impl();
    local.install(coding);
    local.install(broken);
    let (agent, reports) = resolve(None, &*local.snapshot(), None);
    let tool_names: Vec<String> = agent
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(tool_names, vec!["bash"]);
    let keys: Vec<String> = agent
        .sections
        .iter()
        .map(|item| item.key().to_owned())
        .collect();
    assert_eq!(keys, vec!["preamble"]);
    assert_eq!(
        reports,
        vec![
            "wrapper failed",
            "Wrapper renamed edit to renamed",
            "section wrapper failed",
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn orders_sections_by_extension_replaces_same_keys_and_renders_instructions_last_unwrapped() {
    let fixture = agent_fixture();
    let local = create_registry_impl();
    let override_extension = define_extension(
        TestExtension::new("override")
            .with_sections(vec![section(
                "preamble",
                render_const("You review."),
                Some(Some(false)),
            )])
            .with_wraps(vec![
                wrap_section(
                    "cwd",
                    Arc::new(|inner: Arc<dyn PromptSection>| -> Arc<dyn PromptSection> {
                        Arc::new(WrappedSection { inner })
                    }),
                ),
                wrap_section(
                    "instructions",
                    Arc::new(|_inner: Arc<dyn PromptSection>| -> Arc<dyn PromptSection> {
                        panic!("never")
                    }),
                ),
            ]),
    );
    local.install(Arc::clone(&fixture.coding));
    local.install(Arc::clone(&fixture.skills));
    local.install(override_extension);
    let state = AgentState {
        instructions: Some("Be terse.".to_owned()),
        ..empty_agent_state()
    };
    let (agent, reports) = resolve(Some(&state), &*local.snapshot(), None);
    let rendered = rendered(&agent).await;
    assert_eq!(
        rendered,
        vec![
            ("preamble".to_owned(), Some("You review.".to_owned())),
            ("cwd".to_owned(), Some("/repo!".to_owned())),
            ("skills".to_owned(), Some("S".to_owned())),
            ("instructions".to_owned(), Some("Be terse.".to_owned())),
        ]
    );
    assert_eq!(agent.sections.last().unwrap().tag(), None);
    assert!(reports.is_empty());
}

/// 把内部分段的渲染结果加一个 `!`。
struct WrappedSection {
    inner: Arc<dyn PromptSection>,
}

#[async_trait]
impl PromptSection for WrappedSection {
    fn key(&self) -> &str {
        self.inner.key()
    }
    fn tag(&self) -> Option<bool> {
        self.inner.tag()
    }
    async fn render(
        &self,
        input: &PromptInput,
        context: &dyn Context,
    ) -> Result<Option<String>, BoxError> {
        Ok(self
            .inner
            .render(input, context)
            .await?
            .map(|value| format!("{value}!")))
    }
}

#[derive(Clone)]
struct ToolHookMarker(&'static str);

#[test]
fn collects_hooks_of_the_selected_extensions_in_extension_order_and_applies_field_defaults() {
    let local = create_registry_impl();
    local.install(define_extension(TestExtension::new("a").with_hooks(vec![
        HookRegistration {
            task: "kiana.tool".to_owned(),
            handlers: Arc::new(ToolHookMarker("a-tool")),
        },
        HookRegistration {
            task: "kiana.generation".to_owned(),
            handlers: Arc::new(ToolHookMarker("a-generation")),
        },
    ])));
    local.install(define_extension(TestExtension::new("b").with_hooks(vec![
        HookRegistration {
            task: "kiana.tool".to_owned(),
            handlers: Arc::new(ToolHookMarker("b-tool")),
        },
    ])));

    let (agent, _) = resolve(None, &*local.snapshot(), None);
    let hooks = agent_hooks(&agent, "kiana.tool");
    let described: Vec<(String, &'static str)> = hooks
        .iter()
        .map(|hook| {
            (
                hook.extension.clone(),
                hook.handlers
                    .downcast_ref::<ToolHookMarker>()
                    .expect("tool hook")
                    .0,
            )
        })
        .collect();
    assert_eq!(
        described,
        vec![("a".to_owned(), "a-tool"), ("b".to_owned(), "b-tool"),]
    );

    let (agent, _) = resolve(
        Some(&AgentState {
            extensions: Some(ExtensionSelection::List(vec![
                "b".to_owned(),
                "a".to_owned(),
            ])),
            ..empty_agent_state()
        }),
        &*local.snapshot(),
        None,
    );
    let extensions: Vec<String> = agent_hooks(&agent, "kiana.tool")
        .iter()
        .map(|hook| hook.extension.clone())
        .collect();
    assert_eq!(extensions, vec!["b", "a"]);
    let (agent, _) = resolve(
        Some(&AgentState {
            extensions: Some(ExtensionSelection::List(vec!["b".to_owned()])),
            ..empty_agent_state()
        }),
        &*local.snapshot(),
        None,
    );
    assert!(agent_hooks(&agent, "kiana.generation").is_empty());

    let (defaults, _) = resolve(None, &*local.snapshot(), None);
    assert!(defaults.model.is_none());
    assert_eq!(defaults.thinking_level, ModelThinkingLevel::Off);
    assert!(defaults.cwd.is_none());

    let (set, _) = resolve(
        Some(&AgentState {
            model: Some(ModelRef {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
            }),
            thinking_level: Some(ModelThinkingLevel::High),
            cwd: Some("/w".to_owned()),
            ..empty_agent_state()
        }),
        &*local.snapshot(),
        None,
    );
    assert_eq!(set.model.expect("model").provider, "p");
    assert_eq!(set.thinking_level, ModelThinkingLevel::High);
    assert_eq!(set.cwd.as_deref(), Some("/w"));
}
