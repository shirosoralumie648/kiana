//! core/tools/tool-definition-wrapper.ts。
//!
//! 两个方向的适配层：
//!
//! - [`wrap_tool_definition`]：`ToolDefinition` -> `AgentTool`（核心运行时执行的瘦视图）。
//!   只复制 `name`/`label`/`description`/`parameters`/`outputSchema`/`constrainedSampling`/
//!   `prepareArguments`/`executionMode`；prompt 元数据与渲染器**不**进入 `AgentTool`
//!   （TS 里由 `Object.assign` 单独补，见 `bash.ts`）。
//! - [`create_tool_definition_from_agent_tool`]：`AgentTool` -> 最小 `ToolDefinition`，
//!   让 `AgentSession` 的内部注册表始终「definition 优先」；渲染器与 prompt 元数据被丢弃
//!   （这是语义要求，不是实现细节）。
//!
//! # 上下文
//!
//! TS 的 `AgentTool.execute` 没有 `ExtensionContext` 参数，包装器闭包住 `ctxFactory`
//! （`ARCHITECTURE.md` §4.9 的冻结签名）。Rust 的 [`ToolDefinitionExecute::execute`] 需要
//! `Arc<dyn ExtensionToolContextApi>`，因此没有工厂时使用 [`missing_tool_context`] 占位：
//! 内置工具在 TS 里同样拿不到 `ctx`（agent loop 只传 4 个参数），它们的实现不应读取上下文；
//! 需要真实上下文的扩展工具必须提供工厂。占位上下文的所有读取方法都返回「无 UI/无会话」的
//! 空值；`model_registry()` 惰性构造一个不联网、不读 models.json 的进程级注册表（TS 在
//! `ctx === undefined` 时会抛 `TypeError`，而冻结签名 `-> &ModelRegistry` 无法表达错误）。
//!
//! # 执行流程
//!
//! [`wrap_tool_definition`] 产生的 `AgentTool.execute` 是 `WrappedDefinitionExecute`：
//!
//! 1. agent loop 以固定签名调用（`tool_call_id`、参数、取消 token、更新回调，**没有**
//!    上下文参数）；
//! 2. 适配器按需调用 `ctx_factory(tool_call_id, signal)` 现场创建上下文，没有工厂时用
//!    进程级占位单例；
//! 3. 把上下文作为最后一个参数转交原始 `ToolDefinition::execute`，其余参数原样透传。
//!
//! 反方向的 [`create_tool_definition_from_agent_tool`] 用 `AgentToolDefinitionExecute`
//! 适配：它丢弃传入的上下文，因为 `AgentTool::execute` 本身没有上下文概念。
//!
//! # 注意事项
//!
//! - 两个方向都是**有损投影**：`ToolDefinition` 的 prompt 元数据、渲染器、
//!   `exposure`/`namespace` 等字段在 `AgentTool` 里没有对应位置，包装后不可恢复；
//!   `AgentTool` 的 `replay` 也没有来源，固定为 `None`（agent loop 不读取它）。
//! - [`is_missing_tool_context`] 用指针判等，只有 [`missing_tool_context`] 返回的同一个
//!   单例会被识别；其它「读取都返回空值」的实现不算占位上下文。
//! - 占位上下文的 `cwd()` 是**构造时**的进程当前目录，而它的 `session_manager().get_cwd()`
//!   返回空串——两者不是同一个来源；`crate::read::effective_cwd` 这类辅助函数读的是
//!   `cwd()`，并在上下文缺失/为空时回落到工具自己的 cwd。
//! - `ctx_factory` 每次执行调用一次；它返回的 `Arc` 至少要活到本次 execute 结束。
//!
//! [`ToolDefinitionExecute::execute`]: kiana_ext_api::tool_types::ToolDefinitionExecute::execute

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use kiana_agent_core::types::BoxFuture;
use kiana_agent_core::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, ExecuteToolOptions, ExtensionContextApi, ExtensionMode,
    ExtensionToolContextApi,
};
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult,
    WidgetContent,
};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use kiana_theme::theme::Theme;
use tokio_util::sync::CancellationToken;

/// 为每次工具调用创建上下文。
///
/// TS: `tool-definition-wrapper.ts#ToolContextFactory`
/// （`(toolCallId, signal) => ExtensionToolContext`）。Rust 用 trait 对象
/// （`ExtensionToolContext` 在 `kiana-ext-api` 里是 [`ExtensionToolContextApi`]）。
#[allow(clippy::type_complexity)]
pub type ToolContextFactory =
    Arc<dyn Fn(&str, Option<&CancellationToken>) -> Arc<dyn ExtensionToolContextApi> + Send + Sync>;

/// 把 `ToolDefinition` 包装成 `AgentTool`。
///
/// TS: `wrapToolDefinition(definition, ctxFactory?)`。字段投影是当前稳定契约（改动需同步测试）；
/// 提供工厂时每次执行都调用它创建上下文（工具定义自身的 `ctx` 优先语义在 Rust 里不可表达，
/// 因为 `AgentToolExecute` 没有 ctx 参数——这是冻结签名的必然结果）。
///
/// `definition` 按值消费：未被 `AgentTool` 承载的字段在这里丢弃（见模块文档）。没有工厂时
/// 每次执行拿到同一个占位单例，可用 [`is_missing_tool_context`] 识别。
pub fn wrap_tool_definition(
    definition: ToolDefinition,
    ctx_factory: Option<ToolContextFactory>,
) -> AgentTool {
    AgentTool {
        name: definition.name,
        label: definition.label,
        description: definition.description,
        parameters: definition.parameters,
        constrained_sampling: definition.constrained_sampling,
        output_schema: definition.output_schema,
        prepare_arguments: definition.prepare_arguments,
        execution_mode: definition.execution_mode,
        replay: None,
        execute: Arc::new(WrappedDefinitionExecute {
            execute: definition.execute,
            ctx_factory,
        }),
    }
}

/// 批量版本。TS: `wrapToolDefinitions(definitions, ctxFactory?)`。
///
/// 工厂（若提供）被 `clone` 后逐个传入；返回与输入等长、同序的 `Vec`。
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    ctx_factory: Option<ToolContextFactory>,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|definition| wrap_tool_definition(definition, ctx_factory.clone()))
        .collect()
}

/// 从 `AgentTool` 合成最小 `ToolDefinition`。
///
/// TS: `createToolDefinitionFromAgentTool(tool)`。复制
/// `name/label/description/parameters/outputSchema/constrainedSampling/prepareArguments/executionMode`，
/// 其余字段（prompt 元数据、渲染器、exposure/namespace 等）保持缺省——`AgentTool` 里也没有它们。
///
/// `tool` 按值消费；返回的定义执行时**不接收也不转发上下文**（适配器直接忽略第 5 个参数）。
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    ToolDefinition {
        name: tool.name,
        label: tool.label,
        description: tool.description,
        parameters: tool.parameters,
        output_schema: tool.output_schema,
        constrained_sampling: tool.constrained_sampling,
        prepare_arguments: tool.prepare_arguments,
        execution_mode: tool.execution_mode,
        prompt_snippet: None,
        prompt_guidelines: None,
        render_shell: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execute: Arc::new(AgentToolDefinitionExecute {
            execute: tool.execute,
        }),
        render_call: None,
        render_result: None,
    }
}

/// 包装后的 `AgentTool::execute`：每次调用先解析上下文，再委托给定义。
///
/// `execute` 是原始定义的回调，本适配器不改变参数与返回值；`ctx_factory` 的差异只体现在
/// 第 5 个参数（上下文）的来源上。
struct WrappedDefinitionExecute {
    execute: Arc<dyn ToolDefinitionExecute>,
    ctx_factory: Option<ToolContextFactory>,
}

impl AgentToolExecute for WrappedDefinitionExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            // 每次调用重新取上下文：工厂可能依赖 tool_call_id / signal（TS 的闭包语义）。
            // 没有工厂时用占位单例——内置工具在 TS 里同样拿不到 ctx。
            let ctx = match &self.ctx_factory {
                Some(factory) => factory(tool_call_id, signal),
                None => missing_tool_context(),
            };
            self.execute
                .execute(tool_call_id, params, signal, on_update, ctx)
                .await
        })
    }
}

/// 工具输出预算配置。
///
/// 第 6 周：把超过落盘阈值的工具输出保存为会话 artifact，inline 只保留
/// middle-out 预览。预算数值见 [`kiana_core::output_budget`]。
#[derive(Clone)]
pub struct OutputBudgetOptions {
    /// 会话级 artifact 存储；同一会话应复用同一个 `Arc`。
    pub store: Arc<kiana_core::artifact_store::ArtifactStore>,
}

impl std::fmt::Debug for OutputBudgetOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputBudgetOptions")
            .field("store", &self.store.root())
            .finish()
    }
}

/// 执行后应用输出预算的适配器。
///
/// 只处理**纯文本**结果：含图片的结果原样返回（图片由 provider 视觉预算管理，不做
/// 文本 middle-out）。落盘成功后把 handle 写入 `details.artifact`，方便 UI/恢复逻辑
/// 复用；`details` 不是对象时保持原样。
struct BudgetedExecute {
    inner: Arc<dyn AgentToolExecute>,
    tool_name: String,
    store: Arc<kiana_core::artifact_store::ArtifactStore>,
}

impl AgentToolExecute for BudgetedExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            let mut result = self
                .inner
                .execute(tool_call_id, params, signal, on_update)
                .await?;
            if result.content.is_empty()
                || result
                    .content
                    .iter()
                    .any(|block| matches!(block, UserContentBlock::Image(_)))
            {
                return Ok(result);
            }
            let joined: String = result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect();
            let budgeted = kiana_core::output_budget::apply_output_budget(
                &self.tool_name,
                &joined,
                Some(&self.store),
            );
            if !budgeted.truncated {
                return Ok(result);
            }
            result.content = vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: budgeted.content,
                text_signature: None,
            })];
            if budgeted.spilled
                && let Some(handle) = &budgeted.artifact
                && let JsonValue::Object(details) = &mut result.details
            {
                details.insert(
                    "artifact".to_string(),
                    serde_json::json!({
                        "id": handle.id,
                        "uri": handle.uri(),
                        "path": handle.path.to_string_lossy(),
                        "bytes": handle.bytes,
                        "redacted": handle.redacted,
                    }),
                );
            }
            Ok(result)
        })
    }
}

/// 包装工具定义并在每次执行后应用输出预算。
///
/// 与 [`wrap_tool_definition`] 的区别只有执行后的预算层；prompt 元数据与渲染器
/// 仍按同一规则丢弃。`definition.name` 是预算策略的 key（与模型看到的工具名一致）。
pub fn wrap_tool_definition_with_output_budget(
    definition: ToolDefinition,
    ctx_factory: Option<ToolContextFactory>,
    budget: OutputBudgetOptions,
) -> AgentTool {
    let tool = wrap_tool_definition(definition, ctx_factory);
    wrap_agent_tool_with_output_budget(tool, budget)
}

/// 在已经包装好的 [`AgentTool`] 上追加输出预算层。
///
/// 供 `AgentSession::build_runtime` 在 `wrap_registered_tools` 之后套用（扩展工具与
/// 内置工具都适用）；对同一工具重复调用会重复套层，调用方应只套一次。
pub fn wrap_agent_tool_with_output_budget(
    mut tool: AgentTool,
    budget: OutputBudgetOptions,
) -> AgentTool {
    let tool_name = tool.name.clone();
    let inner = tool.execute;
    tool.execute = Arc::new(BudgetedExecute {
        inner,
        tool_name,
        store: budget.store,
    });
    tool
}

/// `ToolDefinitionExecute` 适配器：忽略上下文，直接执行 `AgentTool`。
///
/// `AgentTool::execute` 没有上下文参数，所以传入的 `_ctx` 只能丢弃；这是两个 trait 形状
/// 不同导致的必然结果，不是遗漏。
struct AgentToolDefinitionExecute {
    execute: Arc<dyn AgentToolExecute>,
}

impl ToolDefinitionExecute for AgentToolDefinitionExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        self.execute
            .execute(tool_call_id, params, signal, on_update)
    }
}

/// 无工厂时使用的进程级占位上下文。
///
/// 每次调用返回同一个单例的 `Arc` 克隆（`LazyLock`），因此指针稳定，可用
/// [`is_missing_tool_context`] 判定。它没有错误通道：读取方法不返回错误，只给固定的
/// 无宿主值（空串/`None`/空集合，另有 `cwd()` 返回构造时的进程当前目录、`mode()` 固定
/// 返回 `Tui`、`theme()` 返回 dark 主题等回退值），写方法 no-op；判断是否占位上下文应使用
/// [`is_missing_tool_context`]，不要靠「读到空值」推断。
pub fn missing_tool_context() -> Arc<dyn ExtensionToolContextApi> {
    static MISSING: LazyLock<Arc<MissingToolContext>> =
        LazyLock::new(|| Arc::new(MissingToolContext::default()));
    MISSING.clone()
}

/// 判断上下文是否是 [`missing_tool_context`] 的占位实例。
///
/// 内置工具可以用它判断「TS 里 `ctx === undefined`」的语义（例如 `bash` 的
/// `exposeSessionEnvironment && ctx`）。判等用 `Arc::as_ptr` + `std::ptr::eq`，即只认
/// 那个单例；其它内容等价的实现（例如测试里自建的空上下文）不会被识别。
pub fn is_missing_tool_context(ctx: &Arc<dyn ExtensionToolContextApi>) -> bool {
    let missing = missing_tool_context();
    std::ptr::eq(Arc::as_ptr(ctx), Arc::as_ptr(&missing))
}

/// 占位上下文的模型注册表。
///
/// TS 在 `ctx === undefined` 时读取 `modelRegistry` 会抛 `TypeError`；Rust 的
/// [`ExtensionContextApi::model_registry`] 返回 `&ModelRegistry`，没有错误通道。这里惰性构造
/// 一个内存凭证、禁用 models.json、关闭网络与初始刷新的进程级注册表：内置工具（以及所有提供
/// 工厂的扩展工具）都不会读取它，因此不改变任何生产行为；无工厂的扩展工具读到时得到一个
/// 只读的内置目录注册表，而不是让整个 agent 任务 panic。
fn placeholder_model_registry() -> &'static kiana_config::models::registry::ModelRegistry {
    static REGISTRY: std::sync::OnceLock<kiana_config::models::registry::ModelRegistry> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| {
        let credentials =
            std::sync::Arc::new(kiana_ai::auth::credential_store::InMemoryCredentialStore::new());
        let mut options =
            kiana_config::models::runtime::CreateModelRuntimeOptions::with_credentials(credentials);
        options.refresh_on_create = Some(false);
        options.allow_model_network = false;
        // 上述选项下 create() 的剩余步骤都是同步构造（无 IO / 无网络 / 无刷新），
        // 首次 poll 即完成；futures 的 block_on 在 tokio 运行时内部调用也安全。
        let runtime = futures::executor::block_on(
            kiana_config::models::runtime::ModelRuntime::create(options),
        )
        .expect("an in-memory model runtime with models.json disabled and no refresh cannot fail");
        kiana_config::models::registry::ModelRegistry::new(std::sync::Arc::new(runtime))
    })
}

/// 占位上下文：所有读取方法返回空值（等价 TS 的 `ctx === undefined` 分支）。
///
/// 唯一的真实数据是构造时的进程 cwd（`cwd()` 返回它）；`session_manager()` 返回的空实现
/// 与它无关。任何方法都不应 panic —— 冻结签名没有错误通道，panic 只会沿调用栈展开，
/// 调用方无法把它转成工具错误。
struct MissingToolContext {
    ui: UnavailableUi,
    session_manager: UnavailableSessionManager,
    cwd: PathBuf,
}

impl Default for MissingToolContext {
    fn default() -> Self {
        Self {
            ui: UnavailableUi,
            session_manager: UnavailableSessionManager,
            // 占位上下文只能提供进程当前目录；取不到时退化为 "."。
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

// 读取方法返回固定的无宿主值（含 `cwd()`/`mode()` 等回退值，不都是空），写方法 no-op；
// 内置工具用 `is_missing_tool_context()` 的指针判等识别占位实例，不靠读到的值。
impl ExtensionContextApi for MissingToolContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        &self.ui
    }

    fn mode(&self) -> ExtensionMode {
        // 占位上下文没有真实宿主模式；这里是无条件固定值 `Tui`。判断「有没有 UI」
        // 应看 `has_ui()` 或 `is_missing_tool_context()`。
        ExtensionMode::Tui
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        &self.session_manager
    }

    fn model_registry(&self) -> &kiana_config::models::registry::ModelRegistry {
        placeholder_model_registry()
    }

    fn model(&self) -> Option<kiana_ai::types::Model> {
        None
    }

    fn scoped_models(&self) -> Vec<kiana_config::models::resolver::ScopedModel> {
        Vec::new()
    }

    fn thinking_level(&self) -> Option<kiana_agent_core::ThinkingLevel> {
        None
    }

    fn is_idle(&self) -> bool {
        true
    }

    fn is_project_trusted(&self) -> bool {
        false
    }

    fn signal(&self) -> Option<CancellationToken> {
        None
    }

    fn abort(&self) {}

    fn has_pending_messages(&self) -> bool {
        false
    }

    fn shutdown(&self) {}

    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }

    fn compact(&self, _options: Option<CompactOptions>) {}

    fn get_system_prompt(&self) -> String {
        String::new()
    }
}

impl ExtensionToolContextApi for MissingToolContext {
    fn tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }

    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _args: JsonValue,
        _options: Option<ExecuteToolOptions>,
    ) -> BoxFuture<'a, kiana_agent_core::AgentToolCallOutcome> {
        Box::pin(async move {
            // 占位上下文不持有工具表，无法真正执行：返回一个 `is_error: true` 的空结果，
            // `tool_call.name` 保留调用方传入的名字，便于上层记录/展示。
            kiana_agent_core::AgentToolCallOutcome {
                tool_call: kiana_ai::types::ToolCall {
                    kind_tool_call: Default::default(),
                    id: String::new(),
                    name: name.to_string(),
                    arguments: Default::default(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
                result: AgentToolResult {
                    content: Vec::new(),
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: Some(true),
                    terminate: None,
                },
                is_error: true,
                duration_ms: None,
            }
        })
    }
}

/// 占位 UI：等价「无 UI」的默认返回值。
///
/// 交互类方法立即返回空结果（`select`/`input`/`editor` → `None`，`confirm` → `false`），
/// 状态类方法 no-op；`custom()` 立即返回 `Box<()>`；`theme()` 返回内置 dark 主题，
/// `set_theme()` 明确报告失败（`success: false`）。
struct UnavailableUi;

impl ExtensionUiContext for UnavailableUi {
    fn select(
        &self,
        _title: &str,
        _options: &[String],
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn notify(&self, _message: &str, _kind: Option<NotifyKind>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Box::new(|| {})
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<kiana_ext_api::ui::WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget(
        &self,
        _key: &str,
        _content: Option<WidgetContent>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    fn custom(
        &self,
        _factory: CustomFactory,
        _options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>> {
        Box::pin(async { Box::new(()) as Box<dyn Any + Send> })
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(&self, _title: &str, _prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        Box::pin(async { None })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> &Theme {
        // 内置 dark 主题总是可用；`current_theme()` 是注册表的兜底。
        static THEME: LazyLock<Arc<Theme>> = LazyLock::new(|| {
            kiana_theme::theme::get_theme_by_name("dark")
                .or_else(kiana_theme::theme::current_theme)
                .expect("no theme is available for the placeholder context")
        });
        &THEME
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme(&self, _theme: ThemeOrName) -> ThemeResult {
        ThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

/// 占位会话管理器：所有读取返回空值。
///
/// 空字符串 / `None` / 空 `Vec` 是各方法唯一的返回值，包括 `get_cwd()`（**不是**进程 cwd）
/// 与 `build_session_projection()`（全空投影）。
struct UnavailableSessionManager;

impl ReadonlySessionManager for UnavailableSessionManager {
    fn get_cwd(&self) -> &str {
        ""
    }

    fn get_session_dir(&self) -> &str {
        ""
    }

    fn get_session_id(&self) -> &str {
        ""
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
