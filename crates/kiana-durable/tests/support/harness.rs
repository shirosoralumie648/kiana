//! `test/harness-support.ts` 的 Rust 等价物：构造工具/扩展、打开 Harness，以及消息夹具。
//!
//! TS 侧用 `Type.Object({})` 生成空参数 schema；这里直接用 `{"type":"object"}` 的 JSON。

#![allow(dead_code)]
#![allow(clippy::type_complexity)]

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_ai::models::create_models;
use kiana_ai::providers::faux::{FauxToolCallOptions, faux_tool_call_with};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, SystemContent,
    SystemMessage, SystemRole, TextContent, TextKind, Tool, ToolResultMessage, ToolResultRole,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context, ContextRef};
use kiana_durable::harness::define::{define_extension, define_tool, section};
use kiana_durable::harness::harness::{Harness, HarnessImpl};
use kiana_durable::harness::registry::{RegistryImpl, create_registry_impl};
use kiana_durable::harness::types::{
    AnyTask, Extension, HarnessOptions, HookRegistration, ModelsProvider, PromptSection, Registry,
    RegistryReader, RegistrySnapshot, ToolExecuteFn, ToolRegistration, UserInput,
};
use kiana_durable::types::{BoxError, JsonObject, JsonValue, ReportInfo, Storage};

/// 失败的 registry 读端替身（`Registry lacks built-in tasks` 用例）。
pub struct EmptyRegistry;

impl kiana_durable::harness::types::RegistryReader for EmptyRegistry {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(EmptySnapshot)
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn Fn() + Send + Sync>,
    ) -> kiana_durable::types::Unsubscribe {
        Box::new(|| {})
    }
}

struct EmptySnapshot;

impl RegistrySnapshot for EmptySnapshot {
    fn installed(&self) -> Vec<Arc<dyn Extension>> {
        Vec::new()
    }
    fn extension(&self, _name: &str) -> Option<Arc<dyn Extension>> {
        None
    }
    fn tools(&self) -> Vec<kiana_durable::harness::types::RegistryTool> {
        Vec::new()
    }
    fn sections(&self) -> Vec<kiana_durable::harness::types::RegistrySection> {
        Vec::new()
    }
    fn tasks(&self) -> Vec<AnyTask> {
        Vec::new()
    }
    fn task(&self, _name: &str) -> Option<AnyTask> {
        None
    }
}

/// TS 的 `type AppTool = ToolRegistration & { snippet?: string }`：Rust 用扩展字段表携带。
pub fn tool_with(name: &str, snippet: Option<&str>) -> ToolRegistration {
    // TS 的 `snippet` 只是测试扩展字段，不参与运行。
    let _ = snippet;
    define_tool(ToolRegistration {
        tool: Tool {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(noop_execute()),
    })
}

pub fn tool(name: &str) -> ToolRegistration {
    ToolRegistration {
        tool: Tool {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(Arc::new(|_arguments, _api, _context| {
            Box::pin(async {
                Ok(kiana_durable::harness::types::ToolExecutionResult {
                    output: Some(Vec::new()),
                    ..Default::default()
                })
            })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<
                                    kiana_durable::harness::types::ToolExecutionResult,
                                    BoxError,
                                >,
                            > + Send,
                    >,
                >
        }) as ToolExecuteFn),
    }
}

fn noop_execute() -> ToolExecuteFn {
    Arc::new(|_arguments: JsonValue, _api, _context| {
        Box::pin(async {
            Ok(kiana_durable::harness::types::ToolExecutionResult {
                output: Some(Vec::new()),
                ..Default::default()
            })
        })
            as std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                kiana_durable::harness::types::ToolExecutionResult,
                                BoxError,
                            >,
                        > + Send,
                >,
            >
    })
}

/// 一个测试扩展：工具、分段、hook、包装器与任务都按安装顺序发布。
pub struct TestExtension {
    name: String,
    tools: Vec<ToolRegistration>,
    sections: Vec<Arc<dyn PromptSection>>,
    hooks: Vec<HookRegistration>,
    wraps: Vec<kiana_durable::harness::types::Wrap>,
    tasks: Vec<AnyTask>,
}

impl TestExtension {
    pub fn new(name: impl Into<String>) -> Self {
        TestExtension {
            name: name.into(),
            tools: Vec::new(),
            sections: Vec::new(),
            hooks: Vec::new(),
            wraps: Vec::new(),
            tasks: Vec::new(),
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolRegistration>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_sections(mut self, sections: Vec<Arc<dyn PromptSection>>) -> Self {
        self.sections = sections;
        self
    }

    pub fn with_hooks(mut self, hooks: Vec<HookRegistration>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn with_tasks(mut self, tasks: Vec<AnyTask>) -> Self {
        self.tasks = tasks;
        self
    }
}

impl Extension for TestExtension {
    fn name(&self) -> &str {
        &self.name
    }
    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }
    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
    fn hooks(&self) -> Vec<HookRegistration> {
        self.hooks.clone()
    }
    fn wraps(&self) -> Vec<kiana_durable::harness::types::Wrap> {
        self.wraps.clone()
    }
    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.clone()
    }
}

/// `openHarness()` 的选项。
#[derive(Clone, Default)]
pub struct OpenHarnessOptions {
    pub registry: Option<Arc<RegistryImpl>>,
    pub on_report:
        Option<Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync>>,
}

/// 用新 registry（含 `tool_names`）打开一个 Harness。
pub async fn open_harness(
    storage: Arc<dyn Storage>,
    tool_names: &[&str],
) -> (Arc<HarnessImpl>, Arc<RegistryImpl>) {
    open_harness_with(storage, tool_names, OpenHarnessOptions::default()).await
}

pub async fn open_harness_with(
    storage: Arc<dyn Storage>,
    tool_names: &[&str],
    options: OpenHarnessOptions,
) -> (Arc<HarnessImpl>, Arc<RegistryImpl>) {
    let registry = match options.registry {
        Some(registry) => registry,
        None => create_registry_impl(),
    };
    if !tool_names.is_empty() {
        let tools = tool_names.iter().map(|name| tool(name)).collect();
        registry.install(define_extension(
            TestExtension::new("tools").with_tools(tools),
        ));
    }
    let harness = Harness::open(
        storage,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(create_models(Default::default())),
            identity: None,
            registry: Arc::clone(&registry)
                as Arc<dyn kiana_durable::harness::types::RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: options.on_report,
            read_only: None,
        },
        &**BACKGROUND_CONTEXT,
    )
    .await
    .expect("the harness opens");
    (harness, registry)
}

/// 用给定 registry 打开 Harness（不做内建任务校验以外的检查）。
pub async fn open_harness_with_registry(
    storage: Arc<dyn Storage>,
    registry: Arc<RegistryImpl>,
) -> Result<Arc<HarnessImpl>, BoxError> {
    Harness::open(
        storage,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(create_models(Default::default())),
            identity: None,
            registry: Arc::clone(&registry)
                as Arc<dyn kiana_durable::harness::types::RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        &**BACKGROUND_CONTEXT,
    )
    .await
}

/// 卸载某个 helper 装上的扩展。
pub struct Installed {
    registry: Arc<RegistryImpl>,
    name: String,
}

impl Installed {
    /// TS: `dispose()`。
    pub fn dispose(&self) {
        self.registry.uninstall(&self.name);
    }
}

/// 安装一个以工具名命名的单工具扩展。
pub fn add_tool(registry: &Arc<RegistryImpl>, tool: ToolRegistration) -> Installed {
    add_tool_named(registry, tool, None)
}

pub fn add_tool_named(
    registry: &Arc<RegistryImpl>,
    tool: ToolRegistration,
    name: Option<&str>,
) -> Installed {
    let name = name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("tool:{}", tool.name()));
    registry.install(define_extension(
        TestExtension::new(name.clone()).with_tools(vec![tool]),
    ));
    Installed {
        registry: Arc::clone(registry),
        name,
    }
}

/// 安装一个以任务名命名的单任务扩展。
pub fn add_task(registry: &Arc<RegistryImpl>, task: AnyTask) -> Installed {
    add_task_named(registry, task, None)
}

pub fn add_task_named(
    registry: &Arc<RegistryImpl>,
    task: AnyTask,
    name: Option<&str>,
) -> Installed {
    let name = name
        .map(str::to_owned)
        .unwrap_or_else(|| format!("task:{}", task.definition.name));
    registry.install(define_extension(
        TestExtension::new(name.clone()).with_tasks(vec![task]),
    ));
    Installed {
        registry: Arc::clone(registry),
        name,
    }
}

/// 安装一个只含 hook 注册的扩展。
pub fn add_hooks(
    registry: &Arc<RegistryImpl>,
    hooks: Vec<HookRegistration>,
    name: &str,
) -> Installed {
    registry.install(define_extension(
        TestExtension::new(name.to_owned()).with_hooks(hooks),
    ));
    Installed {
        registry: Arc::clone(registry),
        name: name.to_owned(),
    }
}

/// 安装一个以 section key 命名的单分段扩展。
pub fn add_section(
    registry: &Arc<RegistryImpl>,
    key: &str,
    render: kiana_durable::harness::define::SectionRender,
    tag: Option<bool>,
) -> Installed {
    let name = format!("section:{key}");
    registry.install(define_extension(
        TestExtension::new(name.clone()).with_sections(vec![section(key, render, tag.map(Some))]),
    ));
    Installed {
        registry: Arc::clone(registry),
        name,
    }
}

/// 一个可被测试重用的 registry 快照。
pub fn snapshot_of(registry: &Arc<RegistryImpl>) -> Arc<dyn RegistrySnapshot> {
    registry.snapshot()
}

// ---------------------------------------------------------------------------
// 消息夹具（TS harness-support.ts 的下半部分）
// ---------------------------------------------------------------------------

pub fn user(text: &str) -> UserMessage {
    UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_owned()),
        timestamp: 1,
    }
}

/// TS: `assistant(text, { calls, stopReason })`。
pub fn assistant(text: &str, calls: &[&str], stop_reason: Option<StopReason>) -> AssistantMessage {
    let mut content = vec![AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_owned(),
        text_signature: None,
    })];
    for id in calls {
        content.push(AssistantContentBlock::ToolCall(faux_tool_call_with(
            format!("tool-{id}"),
            JsonObject::new(),
            FauxToolCallOptions {
                id: Some((*id).to_owned()),
            },
        )));
    }
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: stop_reason.unwrap_or(if calls.is_empty() {
            StopReason::Stop
        } else {
            StopReason::ToolUse
        }),
        timestamp: 2,
        ..Default::default()
    }
}

pub fn assistant_simple(text: &str) -> AssistantMessage {
    assistant(text, &[], None)
}

pub fn tool_result(id: &str) -> ToolResultMessage {
    tool_result_text(id, &format!("result {id}"))
}

pub fn tool_result_text(id: &str, text: &str) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: id.to_owned(),
        tool_name: format!("tool-{id}"),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_owned(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    }
}

pub fn system(sections: IndexMap<String, Option<String>>) -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(sections),
        tools_removed: None,
        tools_added: None,
        timestamp: 4,
    }
}

/// 紧凑的消息渲染（TS: `describeMessage`）。
pub fn describe_message(message: &Message) -> String {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => format!("user:{text}"),
            UserContent::Blocks(blocks) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text(text) => Some(text.text.clone()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect();
                format!("user:{text}")
            }
        },
        Message::Assistant(assistant) => {
            let text = assistant
                .content
                .iter()
                .find_map(|block| match block {
                    AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            format!("assistant:{text}")
        }
        Message::ToolResult(result) => {
            let text = result
                .content
                .iter()
                .find_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            format!(
                "result:{}:{}",
                result.tool_call_id,
                if result.is_error {
                    "error".to_string()
                } else {
                    text
                }
            )
        }
        Message::System(system) => {
            let keys: Vec<String> = system
                .sections
                .as_ref()
                .map(|sections| sections.keys().cloned().collect())
                .unwrap_or_default();
            format!("system:{}", keys.join(","))
        }
    }
}

/// TS 的 `InputSubmissionDraft` 便捷构造。
pub fn input(content: UserInput) -> kiana_durable::harness::types::InputSubmissionDraft {
    kiana_durable::harness::types::InputSubmissionDraft {
        request_id: None,
        content,
        when_busy: None,
        entry: None,
    }
}

/// 文本输入。
pub fn text_input(text: &str) -> kiana_durable::harness::types::InputSubmissionDraft {
    input(UserInput::Text(text.to_owned()))
}

/// 常用上下文句柄。
pub fn context() -> ContextRef {
    Arc::clone(&BACKGROUND_CONTEXT)
}

/// 供 `open_harness` 的调用方保留的类型擦除上下文。
pub fn context_ref(context: &dyn Context) -> ContextRef {
    kiana_durable::session::ContextSource::context_ref(&context)
}

/// 指定描述的单个工具（TS 的 `tool(name, { description })`）。
pub fn tool_described(name: &str, description: &str) -> ToolRegistration {
    let mut registration = tool(name);
    registration.tool.description = description.to_owned();
    registration
}

/// 什么都不提供的文档读取端（section 渲染夹具用）。
pub struct EmptyReader;

#[async_trait::async_trait]
impl kiana_durable::types::DocumentReader for EmptyReader {
    async fn snapshot(
        &self,
        _target: &kiana_durable::types::DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        Ok(None)
    }

    async fn snapshot_as_of(
        &self,
        _target: &kiana_durable::types::DocTarget,
        _at: kiana_durable::types::EntryId,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        Ok(None)
    }
}

impl TestExtension {
    /// 包装器（`wrapTool`/`wrapSection`）。
    pub fn with_wraps(mut self, wraps: Vec<kiana_durable::harness::types::Wrap>) -> Self {
        self.wraps = wraps;
        self
    }
}
