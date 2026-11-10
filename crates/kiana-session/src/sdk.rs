//! core/sdk.ts：`createAgentSession`（依赖 AgentSession，故放在此 crate 而非 kiana-resources）。
//!
//! TS 源：`packages/coding-agent/src/core/sdk.ts`（479 行）。规范：
//! `spec/ca-resources.md` §2.12.1-§2.12.6。
//!
//! # 模块级副作用
//!
//! TS 在模块加载时执行 `setDefaultStreamFn(streamSimple)`。Rust 没有 import 期副作用；
//! [`create_agent_session`] 在入口用 [`std::sync::Once`] 安装同一适配器（见
//! [`ensure_default_stream_fn`]）。这是本文件对 §2.12.1 的唯一偏离。
//!
//! # 面向 downstream 的再导出
//!
//! 与 `sdk.ts` 一致：`export * from "./agent-session-runtime.ts"`、扩展 API 类型、
//! prompt/skill/tool 类型与工具工厂。
//!
//! `kiana_ai::AiError` 是冻结的错误类型（>128 字节），与 kiana-config/kiana-ai 的同类入口一样
//! 保留 `Result<_, AiError>` 签名。
//!
//! # 职责与边界
//!
//! 本文件只做"组装"：把 ModelRuntime / SettingsManager / SessionManager /
//! ResourceLoader / CacheWarmer / `Agent` / `AgentSession` 按规范 §2.12.4 的阶段 A-J
//! 逐一接线，业务逻辑本身不在这里——prompt 主循环在 [`crate::agent_session`]，
//! 生命周期在 [`crate::agent_session_runtime`]。下游入口：kiana-coding-agent 的
//! `sdk_exports` 把 [`create_agent_session`] 与本文件的再导出面暴露给 CLI/前端；
//! kiana-interactive 与 kiana-evals 的测试/挂具也直接调用本工厂。
//!
//! # 一次创建的执行顺序（代码内有与阶段编号对应的 A1…J36 注释）
//!
//! 1. A：解析 cwd / agentDir 的三态，缺省时创建 ModelRuntime / SettingsManager /
//!    SessionManager / DefaultResourceLoader（外部提供 loader 时不 reload、不打点）；
//! 2. B：从会话 branch 恢复上次的模型与 thinking level；恢复不了或没有历史会话时走
//!    `find_initial_model`，差异写进 `model_fallback_message`；
//! 3. C-D：确定 thinking level 与工具名单（允许 / 排除 / `+name`-`-name` 修饰符）；
//! 4. E-F：包装 convertToLlm（图片屏蔽）、创建 CacheWarmer 与各钩子闭包；
//! 5. H：构造 `Agent`（`initial_state.tools` 恒为空、`system_prompt` 恒为 `""`，
//!    工具由 AgentSession 注入）；
//! 6. I：把 model / thinking level 变更落盘为会话条目；
//! 7. J：构造 [`AgentSession`]，注入扩展 runner
//!    与 warmer 会话槽，返回。
//!
//! # 线程与引用环约束
//!
//! - 扩展运行时是 `!Send` 的：所有从 `Send` 上下文回调扩展的钩子（on_payload /
//!   on_response / on_provider_stream_event / transform_context / transform_headers /
//!   cache warming decide）都必须经 `run_on_session_thread*` 派发。没有派发表或
//!   handler 出错时：on_payload 返回 `None`，事件类钩子静默丢弃，transform 类回退到
//!   原始输入（各闭包处有对应注释）。
//! - CacheWarmer 需要回调 `AgentSession`，而 `AgentSession` 又持有 warmer，直接持有
//!   会形成引用环：靠线程局部表 + `release_cache_warmer_session` 打断（见
//!   `CacheWarmerSessionSlot`），由 agent_session_runtime 在替换/销毁会话时调用。
//!
//! # 边界与失败模式
//!
//! - 同步参数校验错误只有 `Invalid tools option: …`；ModelRuntime 构造失败经
//!   `From<AiError>` 透传；其余错误折成 `SessionManager` / `Other` 字符串变体。
//! - 模型恢复失败不致命：会话照常创建，提示进 `model_fallback_message`
//!   （三种最终形态见 spec §2.12.5）。
//! - 创建过程中的会话元数据落盘（append_model_change / append_thinking_level_change）
//!   失败会让整个创建失败（`?` 透传 `SessionManagerError`）。
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock};

use kiana_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use kiana_agent_core::budget::{BudgetConfig, budget_handle};
use kiana_agent_core::loop_guard::LoopGuardHandle;
use kiana_agent_core::types::{AgentMessage, StreamFn};
use kiana_ai::models::thinking::clamp_thinking_level;
use kiana_ai::types::{
    Message, Model, ModelThinkingLevel, ProviderHeaders, SimpleStreamOptions, TextContent,
    TextKind, ThinkingBudgets, Transport, UserContent, UserContentBlock,
};
use kiana_config::auth_guidance::format_no_models_available_message;
use kiana_config::cache::warmer::{
    CacheWarmRequest, CacheWarmer, CacheWarmerModels, CacheWarmerOptions, CacheWarmerSession,
    SharedCacheWarmerSession,
};
use kiana_config::client_identity::get_client_identity;
use kiana_config::defaults::DEFAULT_THINKING_LEVEL;
use kiana_config::models::resolver::{FindInitialModelOptions, find_initial_model};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{SteeringMode, TransportSetting};
use kiana_config::settings::tools::{
    DEFAULT_TOOL_NAMES, apply_tool_modifiers, get_tool_list_error, is_tool_modifier,
};
use kiana_core::mcp_servers::create_tool_name_matcher;
use kiana_core::timings::{TimingLabel, time};
use kiana_ext_api::events::{
    AfterProviderResponseEvent, AfterProviderResponseEventType, ExtensionEvent,
    ProviderStreamEvent, ProviderStreamEventType,
};
use kiana_extensions::LoadExtensionsResult;
use kiana_extensions::runner::ExtensionRunner;
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session_core::messages::convert_to_llm;
use kiana_session_core::session_manager::{SessionEntry, SessionManager};
use kiana_session_core::virtual_models::get_branch_selection;

use crate::agent_session::{AgentSession, AgentSessionConfig, ScopedModel};
use crate::continuity::ContinuityOptions;

// ============================================================================
// Re-exports（镜像 sdk.ts 的再导出面）
// ============================================================================

pub use crate::agent_session_runtime::*;

pub use kiana_core::slash_commands::{SlashCommandInfo, SlashCommandSource};
pub use kiana_ext_api::types::{
    ExtensionApi, ExtensionCommandContextApi, ExtensionContextApi, ExtensionFactory,
    InlineExtension,
};
pub use kiana_resources::prompts::PromptTemplate;
pub use kiana_resources::skills::Skill;
pub use kiana_tools::index::Tool;
pub use kiana_tools::index::{create_coding_tools, create_read_only_tools};
pub use kiana_tools::with_file_mutation_queue;
pub use kiana_tools::{
    create_bash_tool, create_edit_tool, create_find_tool, create_grep_tool, create_ls_tool,
    create_power_shell_tool, create_read_tool, create_write_tool,
};

// `ToolDefinition` 在 TS 里同时是扩展类型与 SDK 自定义工具类型；kiana-extensions 的再导出
// 与 kiana-ext-api 的定义是同一个类型。
pub use kiana_extensions::ToolDefinition;

// ============================================================================
// 默认流函数（§2.12.1）
// ============================================================================

/// 模块级 `setDefaultStreamFn(streamSimple)` 的 Rust 形态：首次入口调用时安装一次。
///
/// 安装的必须是 [`crate::agent_session::stream_simple_stream_fn`] 的**规范实例**：TS 的
/// `sdk.ts` 把 `streamSimple` 本身（而不是一个等价包装）装成默认流函数，
/// `_getSummarizationRequestAuth` 的 `agent.streamFunction === streamSimple` 身份判断依赖
/// 这一点。若这里再包一层 `Arc`，默认构造的 Agent 会被误判成“自定义流函数”。
pub fn ensure_default_stream_fn() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        kiana_agent_core::stream_fn::set_default_stream_fn(
            crate::agent_session::stream_simple_stream_fn(),
        );
    });
}

// ============================================================================
// 参数 / 结果
// ============================================================================

/// `noTools`（`"all" | "builtin"`）。
///
/// TS: `sdk.ts#CreateAgentSessionOptions.noTools`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoTools {
    /// 从没有任何工具开始。
    All,
    /// 禁用内建默认工具，但保留扩展/自定义工具。
    Builtin,
}

/// `createAgentSession` 的输入。
///
/// TS: `sdk.ts#CreateAgentSessionOptions`。`cwd`/`agentDir` 在 TS 是字符串；Rust 侧用
/// `String` 与仓库其它 cwd 传递方式（AgentSessionConfig / ResourceLoader）保持一致。
///
/// 所有字段都可缺省；缺省链是"显式值 → 会话管理器/全局默认 → 进程状态"。
/// 传入的 `session_manager` / `settings_manager` / `resource_loader` 原样接管（不 clone、
/// 不重新发现），用于把多个入口的配置拼到同一套服务上。
#[derive(Default)]
pub struct CreateAgentSessionOptions {
    /// 项目本地发现的工作目录。默认 `sessionManager?.getCwd() ?? process.cwd()`。
    pub cwd: Option<String>,
    /// 全局配置目录。默认 `getAgentDir()`。
    pub agent_dir: Option<String>,
    /// 模型/认证运行时。默认使用 `agentDir/auth.json` 与 `models.json`。
    pub model_runtime: Option<ModelRuntime>,
    /// 模型。默认来自设置或第一个可用模型。
    pub model: Option<Model>,
    /// thinking level。默认来自设置（clamp 到模型能力）。
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 可循环的模型（交互模式 Ctrl+P）。
    pub scoped_models: Vec<ScopedModel>,
    /// 未提供显式允许列表时的默认工具抑制模式。
    pub no_tools: Option<NoTools>,
    /// 工具名/模式允许列表（`*` 匹配任意字符）。
    pub tools: Option<Vec<String>>,
    /// 工具名/模式拒绝列表。
    pub exclude_tools: Option<Vec<String>>,
    /// 额外注册的自定义工具。
    pub custom_tools: Vec<ToolDefinition>,
    /// 资源加载器；缺省新建 `DefaultResourceLoader` 并 reload。
    pub resource_loader: Option<Arc<dyn ResourceLoader>>,
    /// 会话管理器；缺省 `SessionManager.create(cwd, getDefaultSessionDir(cwd, agentDir))`。
    pub session_manager: Option<SessionManager>,
    /// 设置管理器；缺省 `SettingsManager.create(cwd, agentDir)`。
    pub settings_manager: Option<SettingsManager>,
    /// 扩展 `session_start` 元数据。
    pub session_start_event: Option<kiana_ext_api::events::SessionStartEvent>,
    /// 失控保护（loop guard）；`None` 表示不启用（保持旧行为）。
    pub loop_guard: Option<LoopGuardHandle>,
    /// turn / token / cost / wall-clock 预算；`None` 表示不启用。
    ///
    /// 在模型解析完成后用 `budget_handle(config, &model)` 构造成 `BudgetHandle`
    /// 并放进 `AgentOptions`；没有可用模型时忽略该配置。
    pub budget_config: Option<BudgetConfig>,
}

/// `createAgentSession` 的结果。
///
/// TS: `sdk.ts#CreateAgentSessionResult`
///
/// 不变量：`session` 已经携带恢复的历史消息、已落盘 model/thinking 变更条目，并且
/// CacheWarmer 的会话槽已指向它；`model_fallback_message` 存在时表示会话已创建但未
/// 按预期拿到模型（恢复失败，或完全没有可用模型——三种最终形态见 spec §2.12.5），
/// 展示层应把它呈现给用户而不是当作错误处理。
pub struct CreateAgentSessionResult {
    /// 创建的会话。
    pub session: AgentSession,
    /// 加载的扩展（交互模式用于 UI 上下文）。
    pub extensions_result: LoadExtensionsResult,
    /// 会话以不同模型恢复时的警告。
    pub model_fallback_message: Option<String>,
}

/// `createAgentSession` 的错误。`Display` 逐字等于 TS 的 `error.message`（§2.12.5 的
/// 逐字性约束），因此变体只携带已格式化的消息、不再叠加前缀。
#[derive(Debug)]
pub enum CreateAgentSessionError {
    /// `Invalid tools option: {message}`。
    InvalidToolsOption(String),
    /// `ModelRuntime.create` 失败。
    ModelRuntime(kiana_ai::AiError),
    /// 会话管理器/设置管理器构造失败。
    SessionManager(String),
    /// 其它透传错误。
    Other(String),
}

impl std::fmt::Display for CreateAgentSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateAgentSessionError::InvalidToolsOption(message) => {
                write!(formatter, "Invalid tools option: {message}")
            }
            CreateAgentSessionError::ModelRuntime(error) => formatter.write_str(&error.message),
            CreateAgentSessionError::SessionManager(message)
            | CreateAgentSessionError::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CreateAgentSessionError {}

impl From<kiana_ai::AiError> for CreateAgentSessionError {
    fn from(error: kiana_ai::AiError) -> Self {
        CreateAgentSessionError::ModelRuntime(error)
    }
}

impl From<kiana_session_core::session_manager::SessionManagerError> for CreateAgentSessionError {
    fn from(error: kiana_session_core::session_manager::SessionManagerError) -> Self {
        CreateAgentSessionError::SessionManager(error.to_string())
    }
}

// ============================================================================
// createAgentSession（§2.12.4）
// ============================================================================

/// `createAgentSession(options = {})`。
///
/// TS: `sdk.ts#createAgentSession`。一步到位的 SDK 工厂：输入一个选项结构（所有字段
/// 可缺省，缺省值见 [`CreateAgentSessionOptions`] 各字段），返回可直接 `prompt` 的
/// [`AgentSession`]。
///
/// 执行按规范 §2.12.4 的阶段 A-J 顺序进行（源码内以 A1…J36 标注），成功时
/// [`CreateAgentSessionResult`] 携带会话、已加载扩展与可选的模型回退提示；
/// 失败时机与形态见 [`CreateAgentSessionError`]。入口做的第一件事是
/// [`ensure_default_stream_fn`]（TS 模块加载副作用的等价物）。
///
/// 注意：外部提供的 `resource_loader` 不会被 `reload()`，也不会打
/// `resourceLoader.reload` 计时点（spec §2.12.5）；需要刷新时由调用方自行处理。
pub async fn create_agent_session(
    options: CreateAgentSessionOptions,
) -> Result<CreateAgentSessionResult, CreateAgentSessionError> {
    ensure_default_stream_fn();

    let CreateAgentSessionOptions {
        cwd: cwd_option,
        agent_dir: agent_dir_option,
        model_runtime: model_runtime_option,
        model: explicit_model,
        thinking_level: explicit_thinking_level,
        scoped_models,
        no_tools,
        tools,
        exclude_tools,
        custom_tools,
        resource_loader: resource_loader_option,
        session_manager: session_manager_option,
        settings_manager: settings_manager_option,
        session_start_event,
        loop_guard,
        budget_config,
    } = options;

    // A1-A6：基础对象。cwd / agentDir 的三态决定 auth.json 与 models.json 是否交给
    // ModelRuntime 自定义解析（spec：只有显式传入 agentDir 才传路径）。
    let process_cwd = process_cwd();
    let cwd = match cwd_option {
        Some(cwd) => resolve_path(&cwd, &process_cwd),
        None => match &session_manager_option {
            Some(session_manager) => session_manager.get_cwd().to_string(),
            None => resolve_path(&process_cwd, &process_cwd),
        },
    };
    // TS 的 `options.agentDir ? ... : getDefaultAgentDir()`：空字符串是 falsy，按未提供处理。
    let agent_dir_option = agent_dir_option.filter(|agent_dir| !agent_dir.is_empty());
    let agent_dir = match &agent_dir_option {
        Some(agent_dir) => resolve_path(agent_dir, &process_cwd),
        None => kiana_core::config::get_agent_dir()
            .to_string_lossy()
            .into_owned(),
    };
    let auth_path = agent_dir_option
        .as_ref()
        .map(|_| join_path(&agent_dir, "auth.json"));
    // ModelsPathOption::Default 让 ModelRuntime 用自己的默认发现逻辑；这与 auth_path
    // 的"显式才传"是同一条规则的两半。
    let models_path = if agent_dir_option.is_some() {
        ModelsPathOption::Path(join_path(&agent_dir, "models.json"))
    } else {
        ModelsPathOption::Default
    };
    let model_runtime = match model_runtime_option {
        Some(model_runtime) => model_runtime,
        None => {
            ModelRuntime::create(CreateModelRuntimeOptions {
                auth_path,
                models_path,
                ..Default::default()
            })
            .await?
        }
    };
    let settings_manager =
        settings_manager_option.unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir));
    let mut session_manager = match session_manager_option {
        Some(session_manager) => session_manager,
        None => {
            let session_dir = kiana_session_core::session_manager::get_default_session_dir(
                &cwd,
                Some(&agent_dir),
            )?;
            SessionManager::create(&cwd, Some(&session_dir), None)?
        }
    };
    let resource_loader: Arc<dyn ResourceLoader> = match resource_loader_option {
        Some(resource_loader) => resource_loader,
        None => {
            // `ResourceLoader` 与 TS 一样在单线程上运行（trait 不带 Send/Sync）。
            #[allow(clippy::arc_with_non_send_sync)]
            let loader: Arc<dyn ResourceLoader> =
                Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
                    cwd: cwd.clone(),
                    agent_dir: agent_dir.clone(),
                    settings_manager: Some(settings_manager.clone()),
                    ..Default::default()
                }));
            if let Err(error) = loader.reload(None).await {
                return Err(CreateAgentSessionError::Other(error.to_string()));
            }
            time("resourceLoader.reload", TimingLabel::Main);
            loader
        }
    };

    // B8-B11：会话恢复。顺序敏感：先试"上次会话用的模型"（还要有配置好的认证），
    // 再试 find_initial_model 的默认发现；两段失败都只产生提示，不中断创建。
    let existing_session = session_manager.build_session_context();
    let has_existing_session = !existing_session.messages.is_empty();
    let branch = session_manager.get_branch(None);
    let has_thinking_entry = branch
        .iter()
        .any(|entry| matches!(entry, SessionEntry::ThinkingLevelChange(_)));

    let mut model = explicit_model;
    let mut model_fallback_message: Option<String> = None;

    // assistant 消息命名实际回答的物理模型；虚拟选择只出现在 model_change 条目里。
    let session_model = get_branch_selection(&branch, &|provider: &str, model_id: &str| {
        model_runtime.get_model(provider, model_id)
    });

    if model.is_none()
        && has_existing_session
        && let Some(session_model) = &session_model
    {
        let restored_model =
            model_runtime.get_model(&session_model.provider, &session_model.model_id);
        if let Some(restored_model) = restored_model
            && model_runtime.has_configured_auth(&restored_model.provider)
        {
            model = Some(restored_model);
        }
        if model.is_none() {
            model_fallback_message = Some(format!(
                "Could not restore model {}/{}",
                session_model.provider, session_model.model_id
            ));
        }
    }

    if model.is_none() {
        // find_initial_model 只在"没有可恢复模型"时兜底；scoped_models 传空是因为
        // 交互模式的 Ctrl+P 循环列表不应影响首模型选择（spec 阶段 B11）。
        let default_provider = settings_manager.get_default_provider();
        let default_model_id = settings_manager.get_default_model();
        let default_thinking_level = settings_manager.get_default_thinking_level();
        let model_thinking_levels = settings_manager.get_all_model_thinking_levels();
        let result = find_initial_model(&FindInitialModelOptions {
            scoped_models: &[],
            is_continuing: has_existing_session,
            default_provider: default_provider.as_deref(),
            default_model_id: default_model_id.as_deref(),
            default_thinking_level,
            model_thinking_levels: Some(&model_thinking_levels),
            model_runtime: &model_runtime,
        })
        .await;
        model = result.model;
        match &model {
            None => model_fallback_message = Some(format_no_models_available_message()),
            Some(found) => {
                if let Some(existing) = &mut model_fallback_message {
                    existing.push_str(&format!(". Using {}/{}", found.provider, found.id));
                }
            }
        }
    }

    // C12-C16：thinking level。优先级：显式传入 → 会话条目（有历史时）→ 按模型的
    // 设置 → 全局默认；最后 clamp 到所选模型的能力上限。无模型时强制 Off。
    let mut thinking_level = explicit_thinking_level;
    if thinking_level.is_none() && has_existing_session {
        thinking_level = Some(if has_thinking_entry {
            parse_thinking_level(&existing_session.thinking_level)
        } else {
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(default_thinking_level)
        });
    }
    if thinking_level.is_none()
        && let Some(found) = &model
        && let Some(per_model) =
            settings_manager.get_model_thinking_level(&found.provider, &found.id)
    {
        thinking_level = Some(per_model);
    }
    if thinking_level.is_none() {
        thinking_level = Some(
            settings_manager
                .get_default_thinking_level()
                .unwrap_or_else(default_thinking_level),
        );
    }
    let thinking_level = match &model {
        None => ModelThinkingLevel::Off,
        Some(found) => clamp_thinking_level(found, thinking_level.unwrap_or_default()),
    };

    // D17-D23：工具选择。三份名单语义不同：selected 决定"初始激活哪些"，
    // allowed 决定"会话期间允许切到哪些"，excluded 是额外黑名单。`+name`/`-name`
    // 修饰符只作用于默认名单（settings 的 defaultTools，否则内建 DEFAULT_TOOL_NAMES；
    // 经 apply_tool_modifiers），不会把修饰符本身当工具名。
    if let Some(tools) = &tools
        && let Some(tool_list_error) = get_tool_list_error(tools)
    {
        return Err(CreateAgentSessionError::InvalidToolsOption(tool_list_error));
    }
    let default_tool_names: Vec<String> = if no_tools.is_some() {
        Vec::new()
    } else {
        settings_manager.get_default_tools().unwrap_or_else(|| {
            DEFAULT_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        })
    };
    let tool_modifiers: Option<Vec<String>> = tools
        .as_ref()
        .filter(|entries| entries.iter().any(|entry| is_tool_modifier(entry)))
        .cloned();
    let selected_tool_names: Option<Vec<String>> = match &tool_modifiers {
        Some(modifiers) => Some(apply_tool_modifiers(&default_tool_names, modifiers)),
        None => tools.clone(),
    };
    let allowed_tool_names: Option<Vec<String>> = match &tool_modifiers {
        Some(_) => {
            if no_tools == Some(NoTools::All) {
                selected_tool_names.clone()
            } else {
                None
            }
        }
        None => tools.clone().or_else(|| {
            if no_tools == Some(NoTools::All) {
                Some(Vec::new())
            } else {
                None
            }
        }),
    };
    let excluded_tool_names = exclude_tools;
    let is_excluded_tool = excluded_tool_names
        .as_ref()
        .map(|names| create_tool_name_matcher(names));
    let initial_active_tool_names: Vec<String> = selected_tool_names
        .as_ref()
        .unwrap_or(&default_tool_names)
        .iter()
        .filter(|name| {
            is_excluded_tool
                .as_ref()
                .is_none_or(|is_excluded| !is_excluded(name))
        })
        .cloned()
        .collect();
    // `create_tool_name_matcher` 的 RPIT 捕获输入生命周期；显式释放借用后再移动列表。
    drop(is_excluded_tool);

    // E24：convertToLlm 的图片屏蔽包装。闭包持有 SettingsManager 的克隆，每次调用
    // 都动态读 get_block_images()，因此会话中改设置立即生效（spec 阶段 E）。
    let settings_for_convert = settings_manager.clone();
    let convert_to_llm_with_block_images = Arc::new(move |messages: Vec<AgentMessage>| {
        let converted = convert_to_llm(&messages);
        if !settings_for_convert.get_block_images() {
            return converted;
        }
        converted.into_iter().map(filter_blocked_images).collect()
    });

    // F25-F26：扩展 runner 引用与 CacheWarmer。runner 此刻还不存在（AgentSession
    // 构造时才装入 extension_runner_ref），所有钩子闭包都必须经引用槽延迟读取；
    // warmer 的会话槽同理（见 CacheWarmerSessionSlot）。
    let extension_runner_ref: Arc<Mutex<Option<Arc<ExtensionRunner>>>> = Arc::new(Mutex::new(None));
    let warmer_session_slot = Arc::new(Mutex::new(CacheWarmerSessionSlot::default()));
    let cache_warmer = {
        let shared_session: SharedCacheWarmerSession = warmer_session_slot.clone();
        let get_mode_settings = settings_manager.clone();
        let decide_runner_ref = Arc::clone(&extension_runner_ref);
        CacheWarmer::new(CacheWarmerOptions {
            models: Arc::new(model_runtime.clone()) as Arc<dyn CacheWarmerModels>,
            session: shared_session,
            get_mode: Arc::new(move || get_mode_settings.get_cache_warming_mode()),
            decide: Some(Arc::new(move |event| {
                let default_action = event.action;
                let runner = lock(&decide_runner_ref).clone();
                Box::pin(async move {
                    let Some(runner) = runner else {
                        return default_action;
                    };
                    crate::agent_session::run_on_session_thread(async move {
                        runner.emit_cache_warming_decision(event).await
                    })
                    .await
                    .and_then(Result::ok)
                    .unwrap_or(default_action)
                })
            })),
            timer: None,
        })
    };

    // G28-G31：Agent 钩子（runner 在 AgentSession 构造后才注入）。四个闭包的共同
    // 形状：先查 runner 是否存在、是否有对应 handler（无 handler 时短路，避免一次
    // 会话线程派发），再经 run_on_session_thread* 转发到 !Send 的扩展运行时。
    let agent_ref: Arc<OnceLock<Agent>> = Arc::new(OnceLock::new());

    let payload_runner_ref = Arc::clone(&extension_runner_ref);
    let on_payload: kiana_ai::types::OnPayloadAsyncFn = Arc::new(move |payload, _model| {
        let runner = lock(&payload_runner_ref).clone();
        Box::pin(async move {
            let runner = runner?;
            if !runner.has_handlers("before_provider_request") {
                return None;
            }
            // 异步桥：把 !Send 的 JS handler 投到会话线程的常驻 LocalSet，再由本 future
            // await 一个 Send 的 oneshot。不能在 provider 线程上同步忙轮询——那会阻塞
            // runtime driver，handler 里的 setTimeout/IO 永远不会推进。
            crate::agent_session::run_on_session_thread(async move {
                runner.emit_before_provider_request(payload).await
            })
            .await
            .and_then(Result::ok)
        })
    });
    // 扩展 provider 的 `options.onPayload` 契约是同步的；保留同步版本，供
    // `add_provider_callbacks` 注入。Kiana 自己的适配器优先走 `on_payload_async`。
    let payload_runner_sync = Arc::clone(&extension_runner_ref);
    let on_payload_sync: kiana_ai::types::OnPayloadFn = Arc::new(move |payload, _model| {
        let runner = lock(&payload_runner_sync).clone()?;
        if !runner.has_handlers("before_provider_request") {
            return None;
        }
        crate::agent_session::run_on_session_thread_blocking(async move {
            runner.emit_before_provider_request(payload).await
        })
        .and_then(Result::ok)
    });
    let response_runner_ref = Arc::clone(&extension_runner_ref);
    let on_response: kiana_ai::types::OnResponseFn = Arc::new(move |response, _model: &Model| {
        let Some(runner) = lock(&response_runner_ref).clone() else {
            return;
        };
        if !runner.has_handlers("after_provider_response") {
            return;
        }
        let event = ExtensionEvent::AfterProviderResponse(AfterProviderResponseEvent {
            event_type: AfterProviderResponseEventType::AfterProviderResponse,
            status: response.status,
            headers: response.headers,
        });
        // TS 的 `handleProviderResponse` 是 async，provider 会 `await` 它。冻结的
        // `OnResponseFn` 是同步签名，不能在这里 await；改为投到会话线程的常驻 LocalSet
        // 上 fire-and-forget，避免同步忙轮询阻塞 runtime driver（handler 里的 timer 会
        // 永远不推进）。handler 错误仍无法像 TS 的 rejected promise 那样传播给 provider。
        crate::agent_session::spawn_on_session_thread(async move {
            let _ = runner.emit(event).await;
        });
    });
    let stream_event_runner_ref = Arc::clone(&extension_runner_ref);
    let on_provider_stream_event: kiana_ai::types::OnProviderStreamEventFn =
        Arc::new(move |data, model: &Model| {
            let Some(runner) = lock(&stream_event_runner_ref).clone() else {
                return;
            };
            if !runner.has_handlers("provider_stream_event") {
                return;
            }
            let event = ExtensionEvent::ProviderStreamEvent(ProviderStreamEvent {
                event_type: ProviderStreamEventType::ProviderStreamEvent,
                provider: model.provider.clone(),
                api: model.api.clone(),
                model: model.id.clone(),
                data,
            });
            // 同 `on_response`：同步签名不能 await，投到常驻 LocalSet 后台执行。
            crate::agent_session::spawn_on_session_thread(async move {
                let _ = runner.emit(event).await;
            });
        });
    let transform_runner_ref = Arc::clone(&extension_runner_ref);
    let transform_context: kiana_agent_core::types::TransformContextFn =
        Arc::new(move |messages, _signal| {
            let runner = lock(&transform_runner_ref).clone();
            let fallback = messages.clone();
            Box::pin(async move {
                let Some(runner) = runner else {
                    return messages;
                };
                crate::agent_session::run_on_session_thread(async move {
                    runner.emit_context(messages).await
                })
                .await
                .and_then(Result::ok)
                .unwrap_or(fallback)
            })
        });

    // H32：Agent 构造（initialState.tools 恒为空；systemPrompt 恒为 ""）。
    // H32 的 stream_fn：Agent 的每次请求都会走到这里。两条支路——
    // 1. 请求的 session_id 等于本会话 id 时，向 CacheWarmer 提交一次预热请求；
    //    is_current 闭包在这里构造、捕获请求瞬间的 messages 快照，但 start 只存储
    //    不求值——真正的检查发生在 warmer 后续的校验点（status / validate_run）。
    //    判定语义见 cache_context_is_current；
    // 2. 无论是否预热，最终都交给 model_runtime.stream_simple（TS 的 streamSimple）。
    let session_id = session_manager.get_session_id().to_string();
    let stream_runner_ref = Arc::clone(&extension_runner_ref);
    let stream_settings = settings_manager.clone();
    let stream_model_runtime = model_runtime.clone();
    let stream_cache_warmer = cache_warmer.clone();
    let stream_agent_ref = Arc::clone(&agent_ref);
    let stream_session_id = session_id.clone();
    let stream_fn: StreamFn = Arc::new(move |model, context, options| {
        let request_session_id = options
            .as_ref()
            .and_then(|options| options.base.session_id.as_deref());
        let session_matches = request_session_id == Some(stream_session_id.as_str());
        let runner_ref = Arc::clone(&stream_runner_ref);
        let settings_manager = stream_settings.clone();
        let model_runtime = stream_model_runtime.clone();
        let cache_warmer = stream_cache_warmer.clone();
        let agent_ref = Arc::clone(&stream_agent_ref);
        Box::pin(async move {
            let request_options = build_request_options(&settings_manager, &runner_ref, options)?;
            if session_matches {
                let agent = agent_ref
                    .get()
                    .cloned()
                    .expect("Agent is set before its stream function runs");
                let messages = agent.state().messages;
                let request_model = model.clone();
                let is_current: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                    // 在 state 锁内做前缀比较，避免每次查询都深拷贝整条 transcript
                    // （`agent.state()` 还会重算 system prompt）。
                    agent.with_state(|state| {
                        cache_context_is_current(
                            &request_model,
                            &messages,
                            state.model.as_ref(),
                            &state.messages,
                        )
                    })
                });
                cache_warmer.start(
                    CacheWarmRequest {
                        model: model.clone(),
                        context: context.as_context().clone(),
                        options: request_options.duplicate(),
                    },
                    is_current,
                );
            }
            Ok(model_runtime.stream_simple(&model, context.as_context(), request_options))
        })
    });

    // H32：Agent 构造。initial_state.tools 恒为空、system_prompt 恒为 ""（spec 阶段 H
    // 的显式注意项）：工具由 AgentSession 在初始化时按名单注入，实际系统提示词来自
    // 资源加载器的 get_system_prompt（agent_session.rs 的 rebuild_system_prompt 消费）。
    // 快照 messages 让 Agent 从上次会话的 transcript 继续。
    // agent_ref 在构造成功后立即填充，供 stream_fn 的预热支路读取 agent 状态；正常
    // 流程中 stream_fn 只会在会话开始 run 之后被调用，`expect` 不会触发。J35 的
    // AgentSession::new 会 build_runtime 把真正的 ExtensionRunner 装进
    // extension_runner_ref（见 agent_session.rs），此前注册的钩子闭包由此"上线"。
    // 第 8 周：loop guard / budget 在模型解析之后接线。预算需要最终模型的价格表判断
    // `cost_unknown`；没有模型时忽略预算配置（与 `budget_handle` 的签名一致）。
    let budget =
        budget_config.and_then(|config| model.as_ref().map(|model| budget_handle(config, model)));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some(String::new()),
            model: model.clone(),
            thinking_level: Some(thinking_level),
            tools: Some(Vec::new()),
            messages: Some(existing_session.messages.clone()),
        }),
        convert_to_llm: Some(convert_to_llm_with_block_images),
        stream_fn: Some(stream_fn),
        on_payload: Some(on_payload_sync),
        on_payload_async: Some(on_payload),
        on_response: Some(on_response),
        on_provider_stream_event: Some(on_provider_stream_event),
        session_id: Some(session_id.clone()),
        transform_context: Some(transform_context),
        steering_mode: Some(queue_mode(settings_manager.get_steering_mode())),
        follow_up_mode: Some(queue_mode(settings_manager.get_follow_up_mode())),
        transport: Some(transport(settings_manager.get_transport())),
        thinking_budgets: thinking_budgets(settings_manager.get_thinking_budgets()),
        max_retry_delay_ms: Some(js_millis(
            settings_manager
                .get_provider_retry_settings()
                .max_retry_delay_ms,
        )),
        loop_guard,
        budget,
        ..Default::default()
    })
    .map_err(|error| CreateAgentSessionError::Other(error.to_string()))?;
    let _ = agent_ref.set(agent.clone());

    // I33-I34：会话元数据落盘。两条规则：恢复的会话只在缺 thinking 条目时补一条
    // （不重复记录）；全新会话则先记 model 再记 thinking level，顺序即 spec 的
    // 条目顺序。这里的 append_* 失败会让整个创建失败（`?` 透传）。
    let thinking_level_name = thinking_level_name(thinking_level);
    if has_existing_session {
        if !has_thinking_entry {
            session_manager.append_thinking_level_change(thinking_level_name)?;
        }
    } else {
        if let Some(found) = &model {
            session_manager.append_model_change(&found.provider, &found.id)?;
        }
        session_manager.append_thinking_level_change(thinking_level_name)?;
    }

    // J35：AgentSession。到此为止所有零件都已就绪：Agent（带钩子）、两个管理器、
    // 资源加载器、CacheWarmer。构造内部会装入扩展 runner（G 组闭包因此可用）并按
    // initial_active_tool_names 注入工具——这解释了 H32 里 Agent 的 tools 为何为空。
    // 第 11 周后半：settings `continuity` 键是生产开关（环境变量保留为部署侧覆盖）。
    // 两者都缺省时关闭；显式 `continuity: false` 也会关闭。
    let continuity_setting = settings_manager.get_settings().raw("continuity");
    let continuity_disabled =
        ContinuityOptions::settings_disable_continuity(continuity_setting.as_ref());
    let continuity = ContinuityOptions::from_settings(continuity_setting.as_ref());
    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager: settings_manager.clone(),
        cwd: cwd.clone(),
        scoped_models,
        resource_loader: Arc::clone(&resource_loader),
        custom_tools,
        model_runtime,
        cache_warmer: Some(cache_warmer),
        initial_active_tool_names: Some(initial_active_tool_names),
        uses_default_tools: Some(
            (tools.is_none() || tool_modifiers.is_some()) && no_tools.is_none(),
        ),
        default_tool_modifiers: tool_modifiers.unwrap_or_default(),
        allowed_tool_names,
        excluded_tool_names,
        base_tools_override: None,
        extension_runner_ref: Some(Arc::clone(&extension_runner_ref)),
        session_start_event,
        memory: None,
        output_language: None,
        continuity,
        continuity_disabled,
        telemetry: None,
    });
    // 第 11 周后半：连续性启用时在启动阶段 sweep 后台任务（默认关闭时是 no-op）。
    // Phase 1a：接管本会话崩溃遗留的 Work Running 记录（默认 Fail 落定为 interrupted）。
    //
    // 没有执行所有权时（另一个存活进程持有本会话的执行 claim）两者都跳过：本进程
    // 明确不该执行，也不该去 claim `background/` 与 `work/` 目录——那些目录可能
    // 仍由对方使用。
    if session.execution_refusal().is_none() {
        // `recover_work` 先于 background sweep：它会把后台 Work 的权威终态对账回
        // task 记录（见 `reconcile_background_work`），sweep 随后只处理“journal
        // 已写入、Work 尚未 begin”的崩溃窗口，不会把已完成的后台 Work 误报成
        // interrupted。恢复失败时仍执行 background sweep（普通后台任务不依赖 Work
        // 账本），但 workspace sweep 必须跳过。
        let recovered = session.recover_work().await;
        let recovered_ok = match recovered {
            Ok(outcome) => {
                session.record_work_recovery(Ok(outcome));
                true
            }
            Err(error) => {
                session.record_continuity_diagnostic(format!("work recovery failed: {error}"));
                session.record_continuity_diagnostic(
                    "subagent workspace sweep skipped: work recovery failed".to_string(),
                );
                session.record_work_recovery(Err(error.to_string()));
                false
            }
        };
        if let Err(error) = session.sweep_background_tasks().await {
            session.record_continuity_diagnostic(format!("background sweep failed: {error}"));
        }
        if recovered_ok {
            // 工作区回收必须在 `recover_work` 成功之后：可恢复 Work 的 worktree
            // 作为 keep-set 传给 sweep，否则崩溃遗留的隔离工作区会被当成垃圾
            // 删除。恢复失败时账本可能只读到一部分，keep-set 不完整，宁可跳过
            // 本轮回收也不能误删仍可恢复的工作区。
            session.sweep_subagent_workspaces().await;
        }
    }
    // CacheWarmer 的会话写入口在 AgentSession 构造后注入（见 CacheWarmerSessionSlot）。
    lock(&warmer_session_slot).set_session(session.clone());
    register_cache_warmer_session(&session_id, Arc::clone(&warmer_session_slot));

    // J36：返回。
    let extensions_result = resource_loader.get_extensions();
    Ok(CreateAgentSessionResult {
        session,
        extensions_result,
        model_fallback_message,
    })
}

// ============================================================================
// CacheWarmer 的会话槽
// ============================================================================

/// `CacheWarmer` 的 `Pick<SessionManager, "appendUsage" | "getBranch">` 视图。
///
/// `create_agent_session` 在 `AgentSession` 构造前就必须创建 warmer，而 `SessionManager`
/// 的所有权随后转交给会话。槽位在构造后注入 `AgentSession` 的克隆（`Arc<Core>` 共享），
/// `append_usage` 经 `with_session_manager_mut` 写入同一管理器。
///
/// 已知限制：槽位持有 `AgentSession`（`Arc<Core>`），而 `Core` 持有 warmer，直接持有会
/// 形成引用环。`AgentSessionRuntime` 在替换/销毁旧会话时调用
/// [`release_cache_warmer_session`] 清空槽位并移除表项，因此 runtime 管理的会话可以正常
/// 释放；直接使用 `create_agent_session` 且不经过 runtime 的宿主（没有 `dispose` 回调入口）
/// 仍会保留该 `Core` 到线程结束。`dispose()` 会取消 warmer，因此不会有关闭后的写入。
#[derive(Default)]
struct CacheWarmerSessionSlot {
    session: Mutex<Option<AgentSession>>,
}

impl CacheWarmerSessionSlot {
    fn set_session(&self, session: AgentSession) {
        *lock(&self.session) = Some(session);
    }

    fn clear_session(&self) {
        *lock(&self.session) = None;
    }
}

thread_local! {
    /// 会话 id → warmer 会话槽（仅用于在 runtime 替换/销毁时打断器引用环）。
    ///
    /// `CacheWarmer` 由 `Core` 持有，槽位又需要回调 `AgentSession`；直接持有会形成
    /// `Core → CacheWarmer → slot → AgentSession → Core` 的循环。表里的强引用由
    /// [`release_cache_warmer_session`]（runtime 在 `teardownCurrent` / `dispose`）释放，
    /// 之后槽位只剩弱语义（`Option::None`），`Core` 可以正常 drop。
    ///
    /// 直接使用 `create_agent_session` 而不经过 `AgentSessionRuntime` 的宿主仍需在会话
    /// 结束后调用释放；`AgentSession::dispose` 在另一单元，无法回调本模块。
    static CACHE_WARMER_SESSIONS: RefCell<HashMap<String, Arc<Mutex<CacheWarmerSessionSlot>>>> =
        RefCell::new(HashMap::new());
}

fn register_cache_warmer_session(session_id: &str, slot: Arc<Mutex<CacheWarmerSessionSlot>>) {
    // 只注册、不清理：清理统一由 release_cache_warmer_session 负责（同一线程上
    // 调用，thread_local 表不会跨线程泄漏）。
    CACHE_WARMER_SESSIONS.with(|sessions| {
        sessions.borrow_mut().insert(session_id.to_string(), slot);
    });
}

/// 释放会话的 warmer 会话槽（打断引用环）。runtime 在替换/销毁旧会话时调用。
pub(crate) fn release_cache_warmer_session(session_id: &str) {
    let slot = CACHE_WARMER_SESSIONS.with(|sessions| sessions.borrow_mut().remove(session_id));
    if let Some(slot) = slot {
        lock(&slot).clear_session();
    }
}

impl CacheWarmerSession for CacheWarmerSessionSlot {
    fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: &kiana_ai::types::Usage,
        note: Option<&str>,
    ) -> Option<kiana_session_core::session_manager::UsageEntry> {
        let guard = lock(&self.session);
        let session = guard.as_ref()?;
        session.with_session_manager_mut(|manager| {
            manager
                .append_usage(kind, provider, model, usage.clone(), note)
                .ok()
        })
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        let guard = lock(&self.session);
        let Some(session) = guard.as_ref() else {
            return Vec::new();
        };
        session.session_manager().get_branch(None)
    }

    fn last_prompt_tokens(&self) -> u64 {
        let guard = lock(&self.session);
        let Some(session) = guard.as_ref() else {
            return 0;
        };
        // O(1) 叶子回溯：`status()` 每次都会调用，不能克隆整条分支。
        session.with_session_manager_mut(|manager| manager.last_prompt_tokens())
    }
}

// ============================================================================
// 辅助
// ============================================================================

/// `Mutex::lock` 的统一封装：锁中毒（持锁线程 panic）时不向上传播，而是照常取回
/// 数据继续用（与 agent_session.rs 的同名 helper 一致）。本文件的槽里没有需要
/// "作废"的不一致中间态（槽存的是 Option/克隆值），因此沿用旧值比让创建流程
/// 突然 panic 更接近 TS 引用槽的行为。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 进程 cwd（TS 的 `process.cwd()`）。取不到时回退 `"."`，让后续解析在相对路径上
/// 失败而不是直接 panic。
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

fn resolve_path(input: &str, base_dir: &str) -> String {
    kiana_core::paths::resolve_path(
        input,
        base_dir,
        &kiana_core::paths::PathInputOptions::default(),
    )
}

fn join_path(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().into_owned()
}

/// `DEFAULT_THINKING_LEVEL`（kiana-config 的默认值是 `ModelThinkingLevel::Medium`）。
fn default_thinking_level() -> ModelThinkingLevel {
    DEFAULT_THINKING_LEVEL
}

/// 会话条目里的 thinking level 字符串 → 枚举（非法值按 `off`，随后由 clamp 兜底）。
fn parse_thinking_level(value: &str) -> ModelThinkingLevel {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .unwrap_or(ModelThinkingLevel::Off)
}

/// `ThinkingLevel` 的 TS 字面量。
fn thinking_level_name(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

/// 设置枚举 → agent 队列模式。
fn queue_mode(mode: SteeringMode) -> kiana_agent_core::types::QueueMode {
    match mode {
        SteeringMode::All => kiana_agent_core::types::QueueMode::All,
        SteeringMode::OneAtATime => kiana_agent_core::types::QueueMode::OneAtATime,
    }
}

/// 设置枚举 → provider transport。
fn transport(setting: TransportSetting) -> Transport {
    match setting {
        TransportSetting::Auto => Transport::Auto,
        TransportSetting::Sse => Transport::Sse,
        TransportSetting::Websocket => Transport::Websocket,
        TransportSetting::WebsocketCached => Transport::WebsocketCached,
    }
}

/// 设置的 thinking budgets → `ThinkingBudgets`（毫秒/令牌数与 TS 一样是数字）。
fn thinking_budgets(
    budgets: Option<kiana_config::settings::schema::ThinkingBudgetsSettings>,
) -> Option<ThinkingBudgets> {
    budgets.map(|budgets| ThinkingBudgets {
        minimal: budgets.minimal.map(js_millis),
        low: budgets.low.map(js_millis),
        medium: budgets.medium.map(js_millis),
        high: budgets.high.map(js_millis),
    })
}

/// JS 的 number 毫秒 → Rust 的整数毫秒（截断；非有限/负数按 0）。
fn js_millis(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.trunc() as u64
}

/// 图片屏蔽：把 user / toolResult 内容里的 image 块换成文本占位，并去掉相邻重复。
///
/// TS: `sdk.ts#convertToLlmWithBlockImages` 的内层 map/filter。
fn filter_blocked_images(message: Message) -> Message {
    const PLACEHOLDER: &str = "Image reading is disabled.";
    fn placeholder() -> UserContentBlock {
        UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: PLACEHOLDER.to_string(),
            text_signature: None,
        })
    }
    fn is_placeholder(block: &UserContentBlock) -> bool {
        matches!(block, UserContentBlock::Text(text) if text.text == PLACEHOLDER)
    }
    fn filter_blocks(blocks: &[UserContentBlock]) -> Option<Vec<UserContentBlock>> {
        if !blocks
            .iter()
            .any(|block| matches!(block, UserContentBlock::Image(_)))
        {
            return None;
        }
        let mapped: Vec<UserContentBlock> = blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Image(_) => placeholder(),
                other => other.clone(),
            })
            .collect();
        let mut filtered = Vec::with_capacity(mapped.len());
        for (index, block) in mapped.iter().enumerate() {
            let duplicate =
                index > 0 && is_placeholder(block) && is_placeholder(&mapped[index - 1]);
            if !duplicate {
                filtered.push(block.clone());
            }
        }
        Some(filtered)
    }
    match message {
        Message::User(mut user) => {
            if let UserContent::Blocks(blocks) = &user.content
                && let Some(filtered) = filter_blocks(blocks)
            {
                user.content = UserContent::Blocks(filtered);
            }
            Message::User(user)
        }
        Message::ToolResult(mut result) => {
            if let Some(filtered) = filter_blocks(&result.content) {
                result.content = filtered;
            }
            Message::ToolResult(result)
        }
        other => other,
    }
}

/// `cacheContextIsCurrent(requestModel)` 返回的判定函数本体。
///
/// TS 逐项比较对象身份（`currentMessages[i] === message`）。Rust 的
/// `AgentState::messages` 是 `Vec<AgentMessage>` 的**值**快照（冻结的 agent-core 契约），
/// 无法取得 `Arc::ptr_eq`；这里用「请求前缀仍是当前 transcript 的前缀」的值等价语义。
/// 浅拷贝数组/刷新模型对象仍然判为 current（与 TS 注释的意图一致）；仅“用等值新对象
/// 替换前缀消息”不可区分（见交付报告的 deviations）。
fn cache_context_is_current(
    request_model: &Model,
    request_messages: &[AgentMessage],
    current_model: Option<&Model>,
    current_messages: &[AgentMessage],
) -> bool {
    let Some(current_model) = current_model else {
        return false;
    };
    current_model.provider == request_model.provider
        && current_model.id == request_model.id
        && request_messages.len() <= current_messages.len()
        && request_messages
            .iter()
            .enumerate()
            .all(|(index, message)| current_messages.get(index) == Some(message))
}

/// `buildRequestOptions(options = {})`。
///
/// TS: `sdk.ts#buildRequestOptions`。`getClientIdentity` **每次请求**读取（telemetry 立即生效）。
fn build_request_options(
    settings_manager: &SettingsManager,
    extension_runner_ref: &Arc<Mutex<Option<Arc<ExtensionRunner>>>>,
    options: Option<SimpleStreamOptions>,
) -> Result<kiana_ai::models::ModelsSimpleStreamOptions, kiana_ai::AiError> {
    let mut base = options.unwrap_or_default();
    let provider_retry_settings = settings_manager.get_provider_retry_settings();
    let http_idle_timeout_ms = settings_manager
        .get_http_idle_timeout_ms()
        .map_err(|error| kiana_ai::AiError::new(kiana_ai::AiErrorKind::Other, error.to_string()))?;
    let effective_timeout_ms = if http_idle_timeout_ms == 0.0 {
        // 禁用空闲超时的哨兵值：2147483647（INT32_MAX，约 24.8 天，等于 JS setTimeout
        // 上限）。必须原样写进 provider 请求配置，不能换成 Duration::MAX 之类的等价物
        // （spec §2.12.6）。
        2147483647
    } else {
        js_millis(http_idle_timeout_ms)
    };
    // 四个 `if ... is_none()` 都是"请求显式值优先于设置"：请求里带了 timeout_ms /
    // websocket_connect_timeout_ms / max_retries / max_retry_delay_ms 就不再用设置
    // 覆盖（单元测试 request_timeout_overrides_* 锁定该分支）。
    if base.base.timeout_ms.is_none() {
        base.base.timeout_ms = Some(
            provider_retry_settings
                .timeout_ms
                .map(js_millis)
                .unwrap_or(effective_timeout_ms),
        );
    }
    if base.base.websocket_connect_timeout_ms.is_none() {
        base.base.websocket_connect_timeout_ms = settings_manager
            .get_websocket_connect_timeout_ms()
            .map_err(|error| {
                kiana_ai::AiError::new(kiana_ai::AiErrorKind::Other, error.to_string())
            })?
            .map(js_millis);
    }
    if base.base.max_retries.is_none() {
        base.base.max_retries = provider_retry_settings
            .max_retries
            .map(|value| js_millis(value) as u32);
    }
    if base.base.max_retry_delay_ms.is_none() {
        base.base.max_retry_delay_ms = Some(js_millis(provider_retry_settings.max_retry_delay_ms));
    }
    base.base.identity = Some(get_client_identity(settings_manager));
    let runner_ref = Arc::clone(extension_runner_ref);
    let transform_headers: kiana_ai::models::TransformHeadersFn =
        Arc::new(move |headers: ProviderHeaders| {
            let runner = lock(&runner_ref).clone();
            Box::pin(async move {
                let Some(runner) = runner else {
                    return headers;
                };
                if !runner.has_handlers("before_provider_headers") {
                    return headers;
                }
                let fallback = headers.clone();
                crate::agent_session::run_on_session_thread(async move {
                    runner.emit_before_provider_headers(headers).await
                })
                .await
                .and_then(Result::ok)
                .unwrap_or(fallback)
            })
        });
    Ok(kiana_ai::models::ModelsSimpleStreamOptions {
        base,
        transforms: kiana_ai::models::ModelsRequestTransforms {
            transform_headers: Some(transform_headers),
        },
        signal_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_settings(value: serde_json::Value) -> SettingsManager {
        SettingsManager::in_memory(serde_json::from_value(value).expect("settings"))
    }

    fn no_runner() -> Arc<Mutex<Option<Arc<ExtensionRunner>>>> {
        Arc::new(Mutex::new(None))
    }

    fn user_message(text: &str) -> AgentMessage {
        AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: UserContent::Text(text.to_string()),
            timestamp: 0,
        }))
    }

    fn model(provider: &str, id: &str) -> Model {
        Model {
            provider: provider.to_string(),
            id: id.to_string(),
            ..Default::default()
        }
    }

    /// TS `sdk-stream-options.test.ts` 的 cache warming 断言：messages 数组/模型对象的
    /// 浅拷贝仍视为 current；移除请求前缀则视为 "conversation context changed"。
    #[test]
    fn cache_context_is_current_accepts_shallow_copies_and_extensions() {
        let request_model = model("p", "m");
        let request_messages = vec![user_message("a"), user_message("b")];
        // 浅拷贝（值相等的新 Vec/Model 对象）。
        assert!(cache_context_is_current(
            &request_model,
            &request_messages,
            Some(&model("p", "m")),
            &request_messages.clone(),
        ));
        // transcript 继续增长（前缀不变）。
        let mut extended = request_messages.clone();
        extended.push(user_message("c"));
        assert!(cache_context_is_current(
            &request_model,
            &request_messages,
            Some(&model("p", "m")),
            &extended,
        ));
    }

    #[test]
    fn cache_context_is_current_rejects_prefix_removal_and_model_changes() {
        let request_model = model("p", "m");
        let request_messages = vec![user_message("a"), user_message("b")];
        let extended = {
            let mut messages = request_messages.clone();
            messages.push(user_message("c"));
            messages
        };
        // `messages.slice(1)`：请求前缀不再是当前 transcript 的前缀。
        assert!(!cache_context_is_current(
            &request_model,
            &request_messages,
            Some(&model("p", "m")),
            &extended[1..],
        ));
        // provider/id 改变。
        assert!(!cache_context_is_current(
            &request_model,
            &request_messages,
            Some(&model("other", "m")),
            &extended,
        ));
        assert!(!cache_context_is_current(
            &request_model,
            &request_messages,
            Some(&model("p", "other")),
            &extended,
        ));
        // Agent 没有模型时 TS 的 `currentModel.provider` 会抛错；这里保守返回 false。
        assert!(!cache_context_is_current(
            &request_model,
            &request_messages,
            None,
            &extended,
        ));
    }

    /// TS `sdk-stream-options.test.ts`："lets request timeoutMs override httpIdleTimeoutMs
    /// for OpenAI Codex"。集成测试无法注入 `agent.streamFunction` 的请求选项（Rust 的
    /// `AgentSession` 不暴露 agent），因此直接验证 `buildRequestOptions` 的覆盖分支。
    #[test]
    fn request_timeout_overrides_http_idle_timeout() {
        let settings_manager = in_memory_settings(serde_json::json!({ "httpIdleTimeoutMs": 1234 }));
        let mut request = SimpleStreamOptions::default();
        request.base.timeout_ms = Some(0);
        let built = build_request_options(&settings_manager, &no_runner(), Some(request))
            .expect("request options");
        assert_eq!(built.base.base.timeout_ms, Some(0));
    }

    /// TS `sdk-stream-options.test.ts`："lets request websocketConnectTimeoutMs override
    /// settings"。
    #[test]
    fn request_websocket_connect_timeout_overrides_settings() {
        let settings_manager =
            in_memory_settings(serde_json::json!({ "websocketConnectTimeoutMs": 1234 }));
        let mut request = SimpleStreamOptions::default();
        request.base.websocket_connect_timeout_ms = Some(0);
        let built = build_request_options(&settings_manager, &no_runner(), Some(request))
            .expect("request options");
        assert_eq!(built.base.base.websocket_connect_timeout_ms, Some(0));
    }
}
