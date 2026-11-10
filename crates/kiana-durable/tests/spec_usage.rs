//! `test/spec-usage.test.ts` 的 Rust 编译检查：docs/SPEC.md 的用法示例。
//!
//! TS 用 vitest 的 `expectTypeOf` 编译检查 `examples()`；Rust 没有类型层的测试，替代做法是
//! 把同样的形状写成本文件里的真实构造（`#![allow(dead_code)]` 的编译检查），再用一个冒烟测试
//! 打开 Harness 并走一遍 `tableRules` 序列。
//!
//! 与 TS 的差异：hook 声明（`hook(ToolTask, ...)`/`hook(GenerationTask, ...)`）需要具体的
//! `HooksOf<K>` 类型，本文件不重复构造（hook 的运行期行为见 `harness_conversations` 与
//! `harness_events`）；`section` 的 `input.env` 在 Rust 里是 `ExecutionEnv`，示例改用
//! `input.agent.cwd`。

#![allow(dead_code)]

#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{Models, create_models};
use kiana_ai::types::{TextContent, TextKind, Tool, UserContentBlock};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::configure;
use kiana_durable::harness::define::{
    SectionRender, define_extension, define_tool, section, wrap_tool,
};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, EnvBuilder, Extension, HarnessOptions, HarnessSettings, ModelRef, ModelsProvider,
    PromptSection, Registry, RegistryReader, RootOptions, SettingsProvider, TaskRuntimeChange,
    ToolExecuteFn, ToolExecutionResult, ToolRegistration, Wrap as ToolWrap,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::tools::CodingTools;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, ConversationQuery,
    CreateConversationOptions, DocDefinition, DocTarget, DocumentFork, DocumentHistory,
    DocumentReader, DocumentSemantics, EntryDraft, JsonObject, JsonValue, ScopeKind, Session,
    SessionChange, TaskDefinition, TaskId, TaskOptions, TaskOwnership, TaskPhase, TaskRecord,
    TaskRuntime, TaskState, Tx, define_doc,
};

// ---------------------------------------------------------------------------
// 7.1 扩展与宿主设置
// ---------------------------------------------------------------------------

struct Sections {
    name: String,
    sections: Vec<Arc<dyn PromptSection>>,
}

impl Extension for Sections {
    fn name(&self) -> &str {
        &self.name
    }

    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
}

struct Tools {
    name: String,
    tools: Vec<ToolRegistration>,
    wraps: Vec<ToolWrap>,
}

impl Extension for Tools {
    fn name(&self) -> &str {
        &self.name
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }

    fn wraps(&self) -> Vec<ToolWrap> {
        self.wraps.clone()
    }
}

fn static_section(key: &str, text: &str, tag: Option<bool>) -> Arc<dyn PromptSection> {
    let text = text.to_owned();
    let render: SectionRender = Arc::new(move |_, _| {
        let text = text.clone();
        Box::pin(async move { Ok(Some(text)) })
    });
    section(key, render, tag.map(Some))
}

fn cwd_section() -> Arc<dyn PromptSection> {
    let render: SectionRender =
        Arc::new(|input: &kiana_durable::harness::types::PromptInput, _| {
            let cwd = input.agent.cwd.clone();
            Box::pin(async move { Ok(cwd.map(|cwd| format!("Working directory: {cwd}"))) })
        });
    section("cwd", render, None)
}

fn coding_extension() -> Arc<dyn Extension> {
    define_extension(Sections {
        name: "coding".to_owned(),
        sections: vec![
            static_section(
                "preamble",
                "You are an expert coding assistant.",
                Some(false),
            ),
            cwd_section(),
        ],
    })
}

fn read_tool() -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(|_arguments: JsonValue, _api, _context| {
        Box::pin(async {
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "contents".to_owned(),
                    text_signature: None,
                })]),
                ..Default::default()
            })
        })
    });
    let mut tool = ToolRegistration::new(Tool {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    });
    tool.execute = Some(execute);
    define_tool(tool)
}

/// 包装 bash 的 `timing` 扩展：在 `execute` 外层记一次耗时。
fn timing_extension(bash: &ToolRegistration) -> Arc<dyn Extension> {
    define_extension(Tools {
        name: "timing".to_owned(),
        tools: Vec::new(),
        wraps: vec![wrap_tool(
            bash,
            Arc::new(|mut tool: ToolRegistration| {
                let inner = tool.execute.take();
                tool.execute = Some(Arc::new(move |arguments, api, context| {
                    let inner = inner.clone();
                    Box::pin(async move {
                        let started = std::time::Instant::now();
                        let result = match inner {
                            Some(inner) => inner(arguments, api, context).await,
                            None => Ok(ToolExecutionResult::default()),
                        };
                        let _elapsed = started.elapsed();
                        result
                    })
                }));
                tool
            }),
        )],
    })
}

// ---------------------------------------------------------------------------
// 7.2 扩展状态文档
// ---------------------------------------------------------------------------

fn plan_mode_doc() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "app.plan-mode",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Current),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("enabled".to_owned(), JsonValue::Bool(false));
            value
        }),
    )
}

fn container_doc() -> kiana_durable::types::DocToken {
    define_doc(
        DocDefinition::new(
            "app.container",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Current),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("image".to_owned(), JsonValue::from("node:22"));
            value
        }),
    )
}

// ---------------------------------------------------------------------------
// 5.1 一个有意图、效果与结果的任务
// ---------------------------------------------------------------------------

struct SetChargeState;

#[async_trait]
impl TaskRuntimeChange for SetChargeState {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "charge", "key": "key-1" }),
        }))
    }
}

struct ChargePhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for ChargePhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&SetChargeState, context, CommitOptions::default())
            .await
    }
}

struct CompletePayment;

#[async_trait]
impl TaskRuntimeChange for CompletePayment {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: kiana_durable::types::TaskOutcome::Completed {
                result: serde_json::json!({ "entryId": 1 }),
            },
        }))
    }
}

struct SettlePhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SettlePhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&CompletePayment, context, CommitOptions::default())
            .await
    }
}

fn payment_task() -> kiana_durable::harness::types::AnyTask {
    let definition = TaskDefinition {
        name: "app.payment".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({ "phase": "prepare" })),
        phases: IndexMap::from([
            (
                "prepare".to_owned(),
                Arc::new(ChargePhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            ),
            (
                "charge".to_owned(),
                Arc::new(SettlePhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            ),
        ]),
        abort: Arc::new(SettlePhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

// ---------------------------------------------------------------------------
// 序列
// ---------------------------------------------------------------------------

/// 2.2：工具提交里创建一个配置好的子会话。
async fn child_in_tool_commit(tx: &dyn Tx, task_id: TaskId) -> Result<ConversationId, BoxError> {
    let child = tx
        .create_conversation(&CreateConversationOptions {
            ownership: ConversationOwnership::Task { task_id },
        })
        .await?;
    configure(
        tx,
        child.id,
        &AgentChange {
            model: Some(Some(ModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude-haiku".to_owned(),
            })),
            tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![
                read_tool(),
            ])),
            cwd: Some(Some("/worktrees/child".to_owned())),
            ..Default::default()
        },
    )
    .await?;
    Ok(child.id)
}

/// 4：表读取先于第一次表写入；文档仍然可用。
async fn table_rules(
    session: &dyn Session,
    conversation_id: ConversationId,
) -> Result<(), BoxError> {
    struct TableRules {
        conversation: ConversationId,
    }

    #[async_trait]
    impl SessionChange for TableRules {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let _conversation = tx.conversation(self.conversation).await?;
            let mut live = tx
                .doc(&DocTarget::conversation(
                    &kiana_durable::harness::live::LIVE_DOC,
                    self.conversation,
                ))
                .await?;
            tx.append_entry(
                self.conversation,
                EntryDraft {
                    key_order: None,
                    kind: "app.message".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
            live.value_mut().shift_remove("generation");
            let mut options = TaskOptions::new(TaskOwnership::Conversation);
            options.conversation_id = Some(self.conversation);
            tx.create_task(&payment_task(), JsonValue::Null, options)
                .await?;
            Ok(Box::new(()))
        }
    }

    session
        .commit(
            &TableRules {
                conversation: conversation_id,
            },
            &**BACKGROUND_CONTEXT,
            CommitOptions::default(),
        )
        .await?;
    Ok(())
}

/// 2.2：每个会话一个环境。
fn container_env_builder() -> EnvBuilder {
    Arc::new(|target, context| {
        Box::pin(async move {
            let container = target
                .read
                .snapshot(
                    &DocTarget::conversation(&container_doc(), target.conversation_id),
                    &*context,
                )
                .await?;
            let _image = container
                .as_ref()
                .and_then(|value| value.get("image"))
                .and_then(JsonValue::as_str);
            // 真实宿主会返回 containers.env(image, cwd, context)；这里没有容器后端。
            Ok(None)
        })
    })
}

fn host_options(registry: Arc<kiana_durable::harness::registry::RegistryImpl>) -> HarnessOptions {
    let models: Arc<dyn Models> = create_models(Default::default());
    HarnessOptions {
        telemetry: None,
        models: ModelsProvider::Direct(models),
        identity: None,
        registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
        settings: Some(SettingsProvider::Static(HarnessSettings {
            // Reviewer 已安装但默认不选中：只有选中它的会话才拿到它的角色与 hook。
            extensions: Some(vec![Arc::clone(&*CodingTools), coding_extension()]),
            compaction: Some(Default::default()),
            ..Default::default()
        })),
        env: Some(container_env_builder()),
        conversation_created: None,
        now: None,
        set_timer: None,
        on_wake: None,
        on_report: None,
        read_only: None,
    }
}

/// 7.3：具名子 agent 的创建提交。
async fn spawn(tx: &dyn Tx, parent: ConversationId) -> Result<ConversationId, BoxError> {
    let mut options = TaskOptions::new(TaskOwnership::Conversation);
    options.background = Some(true);
    options.conversation_id = Some(parent);
    let anchor = tx
        .create_task(&payment_task(), JsonValue::Null, options)
        .await?;
    let child = tx
        .create_conversation(&CreateConversationOptions {
            ownership: ConversationOwnership::Task { task_id: anchor },
        })
        .await?;
    configure(
        tx,
        child.id,
        &AgentChange {
            instructions: Some(Some(
                "You are the subagent \"helper\". Answer the main agent's requests.".to_owned(),
            )),
            ..Default::default()
        },
    )
    .await?;
    let _ = parent;
    Ok(child.id)
}

fn scan_owned(conversation_id: ConversationId) -> ConversationQuery {
    ConversationQuery {
        owner_conversation_id: Some(conversation_id),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 冒烟：把上面的形状真正跑一遍
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn compiles_and_runs_the_spec_usage_sequences() {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let registry = create_registry_impl();
    registry.install(Arc::clone(&*CodingTools));
    registry.install(coding_extension());
    registry.install(timing_extension(&read_tool()));
    let harness: Arc<kiana_durable::harness::harness::HarnessImpl> = Harness::open(
        Arc::new(MemoryStorage::new()),
        host_options(Arc::clone(&registry)),
        context,
    )
    .await
    .expect("the harness opens");
    let root = harness
        .root(
            context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    cwd: Some(Some("/workspace".to_owned())),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await
        .expect("the root");
    let _query = scan_owned(root.id());
    harness
        .commit(
            &PlanModeOn {
                conversation: root.id(),
            },
            context,
            CommitOptions::default(),
        )
        .await
        .expect("plan mode on");
    table_rules(&*harness, root.id())
        .await
        .expect("table rules");
    let child = harness
        .commit(
            &SpawnChild { parent: root.id() },
            context,
            CommitOptions::default(),
        )
        .await
        .expect("spawn");
    let child = *child.downcast::<ConversationId>().expect("child id");
    assert_ne!(child, root.id());
    let snapshot = harness
        .snapshot(
            &DocTarget::conversation(&plan_mode_doc(), root.id()),
            context,
        )
        .await
        .expect("snapshot")
        .expect("the plan-mode document exists");
    assert_eq!(snapshot.get("enabled"), Some(&JsonValue::Bool(true)));
    harness.close(context).await;
}

struct PlanModeOn {
    conversation: ConversationId,
}

#[async_trait]
impl SessionChange for PlanModeOn {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut draft = tx
            .doc(&DocTarget::conversation(
                &plan_mode_doc(),
                self.conversation,
            ))
            .await?;
        draft
            .value_mut()
            .insert("enabled".to_owned(), JsonValue::Bool(true));
        Ok(Box::new(()))
    }
}

struct SpawnChild {
    parent: ConversationId,
}

#[async_trait]
impl SessionChange for SpawnChild {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        Ok(Box::new(spawn(tx, self.parent).await?))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn defines_the_specs_extension_and_task_shapes() {
    let registry = create_registry_impl();
    let coding = coding_extension();
    let timing = timing_extension(&read_tool());
    registry.install(Arc::clone(&*CodingTools));
    registry.install(Arc::clone(&coding));
    registry.install(Arc::clone(&timing));
    assert_eq!(
        registry
            .snapshot()
            .installed()
            .iter()
            .map(|extension| extension.name().to_owned())
            .collect::<Vec<_>>(),
        vec!["coding-tools", "coding", "timing"]
    );
    assert_eq!(
        registry
            .snapshot()
            .task("app.payment")
            .map(|task| task.definition.version),
        None,
        "a task is only known once installed"
    );
    let _ = payment_task();
    let _ = (plan_mode_doc(), container_doc(), ModelThinkingLevel::Low);
    let _ = &harness_support::tool("read");
}
