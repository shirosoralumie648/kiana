//! core/agent-session-services.ts：cwd 绑定的运行时服务（ModelRuntime / SettingsManager /
//! ResourceLoader）与扩展注册/flag 的落地。
//!
//! TS 源：`packages/coding-agent/src/core/agent-session-services.ts`（233 行）。规范：
//! `spec/ca-session.md` §2.5.1-§2.5.3、§3.1；`spec/ca-extensions.md` §B.2.1-§B.2.3；
//! `spec/ca-cli.md` §5.2。
//!
//! 与 TS 的差异（Rust 映射）：
//! - `DefaultResourceLoaderOptions` 的 `Omit<..., "cwd" | "agentDir" | "settingsManager">` 在
//!   Rust 里无法表达，[`CreateAgentSessionServicesOptions::resource_loader_options`] 直接收完整
//!   选项；本模块用服务自己的 `cwd`/`agent_dir`/`settings_manager` 覆盖这三个字段。
//! - `extensionFlagValues` 的 `Map<string, boolean | string>` 用
//!   [`IndexMap<String, FlagValue>`] 表达（`FlagValue::Bool(true)` 即「无值」）。
//! - `AbortSignal` → [`AbortScope`](kiana_ai::utils::abort::AbortScope)（§4.2）。
//!
//! `kiana_ai::AiError` 是冻结错误类型（>128 字节）；保持 `Result<_, AiError>` 的公开签名。
//!
//! # 执行流程（`create_agent_session_services`）
//!
//! 1. 解析 `cwd`（相对路径按进程 cwd）与 `agent_dir`（未提供时取 `get_agent_dir()`）；
//! 2. 组装 `ModelRuntime`（未提供时从 `<agent_dir>/auth.json`、`<agent_dir>/models.json`
//!    创建，`ModelRuntime::create` 失败直接返回 `Err`）与 `SettingsManager`（未提供时按
//!    cwd/agent_dir 新建）——两者都允许调用方
//!    注入既有实例，`kiana-coding-agent` 与 `kiana-evals` 都用这个口子做隔离/复用；
//! 3. 创建 `DefaultResourceLoader` 并 `reload()`：这一步加载扩展/技能等资源，失败直接
//!    返回 `Err(AiError)`——资源加载不完整时会话无法继续，与 TS 的 `reload()` 抛出一致；
//! 4. 从扩展运行时取走（取走即清空）三组 pending 注册队列，逐条登记到 `ModelRuntime`。
//!    单条注册失败只记入 diagnostics、循环继续：其余扩展不受影响（TS 同为逐条 try/catch）；
//! 5. `model_runtime.refresh(allow_network: false)` 失败同样返回 `Err`；
//! 6. 应用 CLI 的扩展 flag，把产生的 diagnostics 并入返回值。
//!
//! # 调用方与注意事项
//!
//! 下游 `kiana-coding-agent` 的启动编排（其 app.rs）与 `kiana-evals` 的评测装配
//! （harness.rs）先建 services，再经 [`create_agent_session_from_services`] 传入
//! `crate::sdk::create_agent_session`；`AgentSessionServices` 是 `Clone` 的（字段是
//! `Arc`/字符串的廉价克隆），runtime 在会话生命周期内按需克隆整份服务。diagnostics
//! 是"非致命问题"的容器，返回后由调用方决定展示或忽略；`AgentSessionRuntimeDiagnostic`
//! 另实现了 kiana-config 的 `DiagnosticLike`，可与 settings 诊断共用同一去重函数。
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::budget::BudgetConfig;
use kiana_agent_core::loop_guard::{
    DetectorThreshold, LoopDetectorKind, LoopGuard, LoopGuardConfig, LoopGuardHandle,
};
use kiana_ai::models::ModelsRefreshOptions;
use kiana_config::models::composer::ProviderConfigInput;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::diagnostics::DiagnosticKind;
use kiana_ext_api::types::{FlagType, FlagValue};
use kiana_extensions::ProviderConfig;
use kiana_extensions::loader::{
    runtime_set_flag_value, take_runtime_pending_native_provider_registrations,
    take_runtime_pending_provider_registrations, take_runtime_pending_virtual_model_registrations,
};
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
    ResourceLoaderReloadOptions,
};
use kiana_session_core::session_manager::SessionManager;
use serde::{Deserialize, Serialize};

use crate::sdk::{
    CreateAgentSessionOptions, CreateAgentSessionResult, NoTools, create_agent_session,
};

/// 创建服务或会话期间收集的非致命问题。
///
/// 这不是错误通道：注册失败、CLI flag 不认识等问题都进这里，函数照常返回成功，
/// 由调用方（如 kiana-coding-agent 的 `report_diagnostics`）决定如何呈现。`kind`
/// 用 `DiagnosticKind` 枚举而不是裸字符串，序列化时映射回 TS 的 `type` 字面量。
///
/// TS: `agent-session-services.ts#AgentSessionRuntimeDiagnostic`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRuntimeDiagnostic {
    /// `type`（`"info" | "warning" | "error"`）。
    pub kind: DiagnosticKind,
    /// `message`。
    pub message: String,
}

impl Serialize for AgentSessionRuntimeDiagnostic {
    // 手写而不是派生：线上字段名是 `type`（TS 契约），而 `kind` 无法直接 rename 成它——
    // 字段名叫 `type` 会撞 Rust 关键字，`#[serde(rename)]` 写在派生结构上则需要中转结构体，
    // 这里直接借一个 `Wire` 更直白。诊断文案是逐字锁定的用户可见表面，改字段名即破坏
    // RPC/JSON 契约。
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            #[serde(rename = "type")]
            kind: &'a str,
            message: &'a str,
        }
        Wire {
            kind: self.kind.as_str(),
            message: &self.message,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentSessionRuntimeDiagnostic {
    // 反序列化严格化：`type` 不是三个合法字面量之一就报错，而不是容忍未知值。
    // 这是 Rust 侧比 TS 更强的检查——TS 类型在运行时不存在，Rust 的枚举把非法值
    // 挡在数据入口。
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(rename = "type")]
            kind: String,
            message: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let kind = match wire.kind.as_str() {
            "info" => DiagnosticKind::Info,
            "warning" => DiagnosticKind::Warning,
            "error" => DiagnosticKind::Error,
            other => {
                return Err(serde::de::Error::custom(format!(
                    "unknown diagnostic type: {other}"
                )));
            }
        };
        Ok(AgentSessionRuntimeDiagnostic {
            kind,
            message: wire.message,
        })
    }
}

impl kiana_config::settings::diagnostics::DiagnosticLike for AgentSessionRuntimeDiagnostic {
    fn diagnostic_kind(&self) -> DiagnosticKind {
        self.kind
    }

    fn diagnostic_message(&self) -> &str {
        &self.message
    }
}

impl AgentSessionRuntimeDiagnostic {
    /// 便捷构造。
    pub fn new(kind: DiagnosticKind, message: impl Into<String>) -> Self {
        AgentSessionRuntimeDiagnostic {
            kind,
            message: message.into(),
        }
    }
}

/// 创建 cwd 绑定运行时服务的输入。
///
/// TS: `agent-session-services.ts#CreateAgentSessionServicesOptions`
#[derive(Default)]
pub struct CreateAgentSessionServicesOptions {
    /// 工作目录（必填；相对路径按进程 cwd 解析）。
    pub cwd: String,
    /// agent 配置目录；缺省 `get_agent_dir()`。
    pub agent_dir: Option<String>,
    /// 既有设置管理器。
    pub settings_manager: Option<SettingsManager>,
    /// 既有模型/认证运行时。
    pub model_runtime: Option<ModelRuntime>,
    /// `ModelRuntime.create` 的取消信号。
    pub model_runtime_signal: Option<kiana_ai::utils::abort::AbortScope>,
    /// CLI 未知 flag 的取值。
    pub extension_flag_values: Option<IndexMap<String, FlagValue>>,
    /// 资源加载器选项（`cwd`/`agent_dir`/`settings_manager` 会被覆盖）。
    pub resource_loader_options: Option<DefaultResourceLoaderOptions>,
    /// `reload()` 的选项。
    pub resource_loader_reload_options: Option<ResourceLoaderReloadOptions>,
}

/// 从已创建服务构造 `AgentSession` 的输入。
///
/// TS: `agent-session-services.ts#CreateAgentSessionFromServicesOptions`
pub struct CreateAgentSessionFromServicesOptions {
    /// 已创建的服务。
    pub services: AgentSessionServices,
    /// 会话管理器。
    pub session_manager: SessionManager,
    /// `session_start` 元数据。
    pub session_start_event: Option<kiana_ext_api::events::SessionStartEvent>,
    /// 显式模型。
    pub model: Option<kiana_ai::types::Model>,
    /// 显式 thinking level。
    pub thinking_level: Option<kiana_ai::types::ModelThinkingLevel>,
    /// `--models` 作用域模型。
    pub scoped_models: Vec<crate::agent_session::ScopedModel>,
    /// 工具允许列表。
    pub tools: Option<Vec<String>>,
    /// 工具拒绝列表。
    pub exclude_tools: Option<Vec<String>>,
    /// 默认工具抑制模式。
    pub no_tools: Option<NoTools>,
    /// SDK 自定义工具。
    pub custom_tools: Vec<kiana_extensions::ToolDefinition>,
    /// 显式 loop guard；`None` 时从 `settings.loopGuard` 读取（缺省启用保守默认值）。
    ///
    /// 显式传入优先于 settings：CLI 可以据此强制启用/关闭（传一个 `enabled: false`
    /// 的 handle 即关闭）。
    pub loop_guard: Option<LoopGuardHandle>,
    /// 显式预算；`None` 时从 `settings.budget` 读取（缺省不设 cap）。
    ///
    /// 显式传入优先于 settings，供 CLI 的 `--max-turns` 等覆盖使用。
    pub budget_config: Option<BudgetConfig>,
}

/// 一个有效 session cwd 的连贯运行时服务。
///
/// TS: `agent-session-services.ts#AgentSessionServices`
#[derive(Clone)]
pub struct AgentSessionServices {
    /// 工作目录。
    pub cwd: String,
    /// agent 配置目录。
    pub agent_dir: String,
    /// 模型/认证运行时。
    pub model_runtime: ModelRuntime,
    /// 设置管理器。
    pub settings_manager: SettingsManager,
    /// 资源加载器。
    pub resource_loader: Arc<dyn ResourceLoader>,
    /// 诊断。
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

impl std::fmt::Debug for AgentSessionServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentSessionServices")
            .field("cwd", &self.cwd)
            .field("agent_dir", &self.agent_dir)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

/// `applyExtensionFlagValues(resourceLoader, extensionFlagValues)`。
///
/// 把 CLI 收到的未知 flag（`--name` 或 `--name value`）落到对应扩展注册的 flag 上。
/// 规则（对应 `spec/ca-session.md` §2.5.2）：
///
/// - `extension_flag_values` 为 `None` → 直接返回空，不读资源加载器；
/// - 只认**已注册**的 flag 名；未注册的名字累积起来，最后合成一条 error 诊断
///   `Unknown option{s}: --a, --b`（单数无 `s`）；
/// - boolean 型 flag 一律置 `true`（CLI 里 `--flag` 无值即真）；
/// - string 型 flag 要求值是字符串，否则报 `Extension flag "--name" requires a value`。
///
/// 返回的是诊断列表而非 `Result`：flag 应用失败不阻断会话创建，与 TS 的行为一致。
///
/// TS: `agent-session-services.ts#applyExtensionFlagValues`
pub fn apply_extension_flag_values(
    resource_loader: &dyn ResourceLoader,
    extension_flag_values: Option<&IndexMap<String, FlagValue>>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    let extensions_result = resource_loader.get_extensions();
    let mut registered_flags: IndexMap<String, FlagType> = IndexMap::new();
    for extension in &extensions_result.extensions {
        for (name, flag) in &extension.flags {
            registered_flags.insert(name.clone(), flag.r#type);
        }
    }

    let mut unknown_flags: Vec<String> = Vec::new();
    for (name, value) in extension_flag_values {
        let Some(flag_type) = registered_flags.get(name) else {
            unknown_flags.push(name.clone());
            continue;
        };
        match flag_type {
            FlagType::Boolean => {
                runtime_set_flag_value(&extensions_result.runtime, name, FlagValue::Bool(true));
            }
            FlagType::String => match value {
                FlagValue::Str(text) => {
                    runtime_set_flag_value(
                        &extensions_result.runtime,
                        name,
                        FlagValue::Str(text.clone()),
                    );
                }
                FlagValue::Bool(_) => diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                    DiagnosticKind::Error,
                    format!("Extension flag \"--{name}\" requires a value"),
                )),
            },
        }
    }

    if !unknown_flags.is_empty() {
        let plural = if unknown_flags.len() == 1 { "" } else { "s" };
        let flags = unknown_flags
            .iter()
            .map(|name| format!("--{name}"))
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(AgentSessionRuntimeDiagnostic::new(
            DiagnosticKind::Error,
            format!("Unknown option{plural}: {flags}"),
        ));
    }

    diagnostics
}

/// `createAgentSessionServices(options)`。
///
/// 组装 cwd 绑定的运行时服务：解析目录 → 复用或新建 ModelRuntime/SettingsManager →
/// 创建并 reload 资源加载器 → 把扩展的 provider/模型注册登记进 ModelRuntime →
/// 应用 CLI flag。完整阶段划分见文件级文档。失败路径有三处直接返回 `Err`
/// （`ModelRuntime::create`、资源加载 reload、模型刷新 refresh），其余问题进
/// `diagnostics` 字段随成功值返回。
///
/// `allow_network: false` 传给 `refresh`：创建会话阶段不做网络拉取，模型列表只来自
/// 本地 models.json 与扩展注册。
///
/// TS: `agent-session-services.ts#createAgentSessionServices`
pub async fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> Result<AgentSessionServices, kiana_ai::AiError> {
    let process_cwd = process_cwd();
    let cwd = kiana_core::paths::resolve_path(
        &options.cwd,
        &process_cwd,
        &kiana_core::paths::PathInputOptions::default(),
    );
    // TS 的 `options.agentDir ? resolvePath(options.agentDir) : getAgentDir()`：空字符串按未提供处理。
    let agent_dir = match options.agent_dir.as_deref().filter(|dir| !dir.is_empty()) {
        Some(agent_dir) => kiana_core::paths::resolve_path(
            agent_dir,
            &process_cwd,
            &kiana_core::paths::PathInputOptions::default(),
        ),
        None => kiana_core::config::get_agent_dir()
            .to_string_lossy()
            .into_owned(),
    };
    let model_runtime = match options.model_runtime {
        Some(model_runtime) => model_runtime,
        None => {
            ModelRuntime::create(CreateModelRuntimeOptions {
                auth_path: Some(join_path(&agent_dir, "auth.json")),
                models_path: ModelsPathOption::Path(join_path(&agent_dir, "models.json")),
                signal: options.model_runtime_signal,
                ..Default::default()
            })
            .await?
        }
    };
    let settings_manager = options
        .settings_manager
        .unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir));
    let mut loader_options = options.resource_loader_options.unwrap_or_default();
    loader_options.cwd = cwd.clone();
    loader_options.agent_dir = agent_dir.clone();
    loader_options.settings_manager = Some(settings_manager.clone());
    // `ResourceLoader` 与 TS 一样在单线程上运行（trait 不带 Send/Sync）。
    #[allow(clippy::arc_with_non_send_sync)]
    let resource_loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(loader_options));
    if let Err(error) = resource_loader
        .reload(options.resource_loader_reload_options)
        .await
    {
        // TS 的 `reload()` 失败会抛出；Rust 的 ResourceLoaderError 只携带 message。
        return Err(kiana_ai::AiError::new(
            kiana_ai::AiErrorKind::Other,
            error.to_string(),
        ));
    }

    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = Vec::new();
    let extensions_result = resource_loader.get_extensions();
    // 三个 take_* 各自"取走并清空"运行时的 pending 队列：注册责任从扩展运行时移交给
    // 本函数，重复调用不会二次注册（spec §2.5.3 明确要求处理后清空）。三条循环单条
    // 失败只记诊断、继续处理后续条目，使一个坏扩展不拖垮其余注册。
    for registration in take_runtime_pending_provider_registrations(&extensions_result.runtime) {
        match model_runtime.register_provider(
            &registration.name,
            provider_config_input(&registration.config),
        ) {
            Ok(()) => {}
            Err(error) => diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Error,
                format!(
                    "Extension \"{}\" error: {error}",
                    registration.extension_path
                ),
            )),
        }
    }
    for registration in
        take_runtime_pending_native_provider_registrations(&extensions_result.runtime)
    {
        match model_runtime.register_native_provider(Arc::clone(&registration.provider)) {
            Ok(()) => {}
            Err(error) => diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Error,
                format!(
                    "Extension \"{}\" error: {error}",
                    registration.extension_path
                ),
            )),
        }
    }
    for registration in take_runtime_pending_virtual_model_registrations(&extensions_result.runtime)
    {
        match model_runtime.register_virtual_model(registration.definition.clone()) {
            Ok(()) => {}
            Err(error) => diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Error,
                format!(
                    "Extension \"{}\" error: {error}",
                    registration.extension_path
                ),
            )),
        }
    }
    model_runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await?;
    diagnostics.extend(apply_extension_flag_values(
        resource_loader.as_ref(),
        options.extension_flag_values.as_ref(),
    ));

    Ok(AgentSessionServices {
        cwd,
        agent_dir,
        model_runtime,
        settings_manager,
        resource_loader,
        diagnostics,
    })
}

/// `createAgentSessionFromServices(options)`。
///
/// [`create_agent_session_services`] 与会话创建的桥：把已建好的五件套（cwd、agent_dir、
/// model_runtime、settings_manager、resource_loader）经 [`crate::sdk::create_agent_session`]
/// 的 `Option` 注入口透传，sdk 侧见到这些字段非 `Some` 即跳过对应的重建/重新加载，
/// 因此扩展不会在这里被加载两次。错误类型转发 sdk 的
/// [`crate::sdk::CreateAgentSessionError`]。
///
/// TS: `agent-session-services.ts#createAgentSessionFromServices`
pub async fn create_agent_session_from_services(
    options: CreateAgentSessionFromServicesOptions,
) -> Result<CreateAgentSessionResult, crate::sdk::CreateAgentSessionError> {
    // 第 8 周：shipped CLI 走这条桥。settings 是 CLI 的配置入口；显式选项（来自
    // CLI flag）优先。loop guard 默认启用保守阈值（只 Warn，不 Stop），预算只在用户
    // 配置了 cap 时启用。
    let settings = options.services.settings_manager.get_settings();
    let loop_guard = options
        .loop_guard
        .or_else(|| loop_guard_from_settings_or_default(&settings));
    let budget_config = options
        .budget_config
        .or_else(|| budget_config_from_settings(&settings));
    create_agent_session(CreateAgentSessionOptions {
        cwd: Some(options.services.cwd.clone()),
        agent_dir: Some(options.services.agent_dir.clone()),
        model_runtime: Some(options.services.model_runtime.clone()),
        settings_manager: Some(options.services.settings_manager.clone()),
        resource_loader: Some(Arc::clone(&options.services.resource_loader)),
        session_manager: Some(options.session_manager),
        model: options.model,
        thinking_level: options.thinking_level,
        scoped_models: options.scoped_models,
        tools: options.tools,
        exclude_tools: options.exclude_tools,
        no_tools: options.no_tools,
        custom_tools: options.custom_tools,
        session_start_event: options.session_start_event,
        loop_guard,
        budget_config,
        ..Default::default()
    })
    .await
}

/// 读取 `settings.loopGuard`；键缺省时返回保守默认 guard。
///
/// 支持字段（camelCase）：`enabled`、`stopOnDetection`、`softToolCallsPerTurn`、
/// `hardToolCallMultiplier`、`globalWindow`、`thresholds`（
/// `{ consecutiveToolCalls: { reminderAt, stopAt }, ... }`）。未知字段忽略，类型不符
/// 时回退到默认值——loop guard 是保护层，不因配置畸形而拒绝启动会话。
fn loop_guard_from_settings_or_default(
    settings: &kiana_config::settings::schema::Settings,
) -> Option<LoopGuardHandle> {
    let default_guard = || {
        Some(Arc::new(Mutex::new(LoopGuard::new(
            LoopGuardConfig::default(),
        )))) as Option<LoopGuardHandle>
    };
    let Some(value) = settings.extra.get("loopGuard") else {
        return default_guard();
    };
    // 类型不符（例如字符串/null）按“未配置”处理：loop guard 是保护层，配置畸形时
    // 退回保守默认值而不是静默关闭。
    let Some(object) = value.as_object() else {
        return default_guard();
    };
    let mut config = LoopGuardConfig::default();
    if let Some(enabled) = object.get("enabled").and_then(serde_json::Value::as_bool) {
        config.enabled = enabled;
    }
    if !config.enabled {
        return None;
    }
    if let Some(stop) = object
        .get("stopOnDetection")
        .and_then(serde_json::Value::as_bool)
    {
        config.stop_on_detection = stop;
    }
    if let Some(value) = object
        .get("softToolCallsPerTurn")
        .and_then(serde_json::Value::as_u64)
    {
        config.soft_tool_calls_per_turn = value as u32;
    }
    if let Some(value) = object
        .get("hardToolCallMultiplier")
        .and_then(serde_json::Value::as_u64)
    {
        config.hard_tool_call_multiplier = value as u32;
    }
    if let Some(value) = object
        .get("globalWindow")
        .and_then(serde_json::Value::as_u64)
    {
        config.global_window = value as usize;
    }
    if let Some(thresholds) = object
        .get("thresholds")
        .and_then(serde_json::Value::as_object)
    {
        for (name, threshold) in thresholds {
            let Some(kind) = loop_detector_kind_from_str(name) else {
                continue;
            };
            let reminder = threshold
                .get("reminderAt")
                .and_then(serde_json::Value::as_u64);
            let stop = threshold.get("stopAt").and_then(serde_json::Value::as_u64);
            if let (Some(reminder), Some(stop)) = (reminder, stop) {
                config
                    .thresholds
                    .insert(kind, DetectorThreshold::new(reminder as u32, stop as u32));
            }
        }
    }
    Some(Arc::new(Mutex::new(LoopGuard::new(config))))
}

/// settings 的检测器名（camelCase）→ 枚举。未知名字忽略。
fn loop_detector_kind_from_str(name: &str) -> Option<LoopDetectorKind> {
    use LoopDetectorKind::*;
    Some(match name {
        "consecutiveToolCalls" => ConsecutiveToolCalls,
        "globalRepeatedToolCalls" => GlobalRepeatedToolCalls,
        "alternatingToolCalls" => AlternatingToolCalls,
        "readFileLoop" => ReadFileLoop,
        "shellStagnation" => ShellStagnation,
        "actionStagnation" => ActionStagnation,
        "invalidArgumentRepeat" => InvalidArgumentRepeat,
        "executionFailureRepeat" => ExecutionFailureRepeat,
        "repeatedThought" => RepeatedThought,
        "contentChanting" => ContentChanting,
        "longCycleRepetition" => LongCycleRepetition,
        "turnToolCallLimit" => TurnToolCallLimit,
        _ => return None,
    })
}

/// 读取 `settings.budget`。没有配置任何 cap 时返回 `None`（预算不启用）。
fn budget_config_from_settings(
    settings: &kiana_config::settings::schema::Settings,
) -> Option<BudgetConfig> {
    let object = settings.extra.get("budget")?.as_object()?;
    let u64_key = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| object.get(*name).and_then(serde_json::Value::as_u64))
    };
    let mut config = BudgetConfig::new();
    config.max_turns = u64_key(&["maxTurns", "max_turns"]);
    config.max_tokens = u64_key(&["maxTokens", "max_tokens"]);
    config.max_wall_clock_ms = u64_key(&["maxWallClockMs", "max_wall_clock_ms"]);
    config.max_cost_usd = object
        .get("maxCostUsd")
        .or_else(|| object.get("max_cost_usd"))
        .and_then(serde_json::Value::as_f64);
    if let Some(ratios) = object
        .get("warnRatios")
        .or_else(|| object.get("warn_ratios"))
        .and_then(serde_json::Value::as_array)
    {
        let ratios: Vec<f64> = ratios
            .iter()
            .filter_map(serde_json::Value::as_f64)
            .collect();
        if !ratios.is_empty() {
            config.warn_ratios = ratios;
        }
    }
    config.is_enabled().then_some(config)
}

/// 进程 cwd（`process.cwd()`）。
///
/// `current_dir()` 失败时退回 `"."` 而不是报错（Rust 侧的等价函数不能失败）：路径解析
/// 仍能进行，错误推迟到后续真实文件操作时暴露。crate 内 `sdk.rs` 与
/// `agent_session_runtime.rs` 有同名同实现的私有副本。
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `path.join(dir, name)`（平台语义）。
fn join_path(dir: &str, name: &str) -> String {
    std::path::Path::new(dir)
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// `kiana_ext_api::ProviderConfig` → `kiana_config::ProviderConfigInput`。
///
/// 与 `kiana-session` 的 `agent_session::provider_config_to_input` 同一映射（后者私有，
/// 本 crate 内不能跨模块引用）。OAuth 使用 kiana-extensions 的引擎线程调度适配。
///
/// 闭包字段必须逐个改写而不能整包转发：扩展 API 与 kiana-config 的函数签名不同
/// （错误类型、future 包装），但语义一一对应——`stream_simple` 只是把 `Arc` 复制一份
/// 再包一层签名适配；`refresh_models` 把扩展侧 `Box<dyn Error>` 的错误转成 `AiError`
/// （`AiErrorKind::Other`），并让模型列表也经过同一 `provider_model_config` 映射。
fn provider_config_input(config: &ProviderConfig) -> ProviderConfigInput {
    ProviderConfigInput {
        name: config.name.clone(),
        base_url: config.base_url.clone(),
        api_key: config.api_key.clone(),
        api: config.api.as_ref().map(|api| api.to_string()),
        stream_simple: config.stream_simple.as_ref().map(|stream| {
            let stream = Arc::clone(stream);
            Arc::new(
                move |model: &kiana_ai::types::Model,
                      context: &kiana_ai::types::TranscriptContext,
                      options: kiana_ai::types::SimpleStreamOptions|
                      -> kiana_ai::event_stream::AssistantMessageEventStream {
                    stream(model, context, options)
                },
            ) as Arc<kiana_config::models::composer::ExtensionStreamSimpleFn>
        }),
        images: config.images.clone().unwrap_or_default(),
        images_defined: config.images.is_some(),
        classifiers: config.classifiers.clone().unwrap_or_default(),
        classifiers_defined: config.classifiers.is_some(),
        headers: config.headers.clone(),
        auth_header: config.auth_header,
        oauth: config
            .oauth
            .as_ref()
            .map(kiana_extensions::runner::extension_oauth_config),
        models: config
            .models
            .as_ref()
            .map(|models| models.iter().map(provider_model_config).collect()),
        refresh_models: config.refresh_models.as_ref().map(|refresh| {
            let refresh = Arc::clone(refresh);
            let future_fn = move |context: kiana_ai::models::RefreshModelsContext| {
                let refresh = Arc::clone(&refresh);
                let future: kiana_ext_api::BoxFuture<
                    'static,
                    Result<
                        Vec<kiana_config::models::composer::ProviderModelConfig>,
                        kiana_ai::AiError,
                    >,
                > = Box::pin(async move {
                    refresh(context)
                        .await
                        .map(|models| models.iter().map(provider_model_config).collect())
                        .map_err(|error| {
                            kiana_ai::AiError::new(kiana_ai::AiErrorKind::Other, error.to_string())
                        })
                });
                future
            };
            Arc::new(future_fn) as Arc<kiana_config::models::composer::ExtensionRefreshModelsFn>
        }),
    }
}

/// 扩展模型定义 → kiana-config 的模型配置。
///
/// 三个变体共享的字段走 `base`，差异字段各写各的。两处**有意的丢字段**：
/// chat 的 `sampling_params_by_thinking_level` 与 classifier 的 `compat` 在扩展 API 里
/// 没有对应字段，固定为 `None`（kiana-extensions 的同名映射有同样注释）；`declared_type`
/// 由分支写死（`Chat`/`Image`/`Classifier`），不透传扩展侧的 `model_type`。
/// `context_window`/`max_tokens` 从 `u64` 转 `f64`（kiana-config 用 f64 装 JSON number）。
fn provider_model_config(
    model: &kiana_ext_api::ProviderModelConfig,
) -> kiana_config::models::composer::ProviderModelConfig {
    use kiana_config::models::composer::{
        ProviderChatModelConfig, ProviderClassifierModelConfig, ProviderImageModelConfig,
        ProviderModelConfigBase,
    };
    #[allow(clippy::too_many_arguments)]
    fn base(
        id: &str,
        name: &str,
        api: &Option<kiana_ai::types::Api>,
        base_url: &Option<String>,
        input: &[kiana_ai::types::ModelInputModality],
        input_limits: &Option<kiana_ai::types::ModelInputLimits>,
        cost: &kiana_ai::types::ModelCost,
        headers: &Option<IndexMap<String, String>>,
        model_type: kiana_ai::types::ModelType,
    ) -> ProviderModelConfigBase {
        ProviderModelConfigBase {
            id: id.to_owned(),
            name: name.to_owned(),
            api: api.as_ref().map(|api| api.to_string()),
            base_url: base_url.clone(),
            input: input.to_vec(),
            input_limits: input_limits.clone(),
            cost: cost.clone(),
            headers: headers.clone(),
            declared_type: Some(model_type),
        }
    }
    match model {
        kiana_ext_api::ProviderModelConfig::Chat(chat) => {
            kiana_config::models::composer::ProviderModelConfig::Chat(ProviderChatModelConfig {
                base: base(
                    &chat.id,
                    &chat.name,
                    &chat.api,
                    &chat.base_url,
                    &chat.input,
                    &chat.input_limits,
                    &chat.cost,
                    &chat.headers,
                    kiana_ai::types::ModelType::Chat,
                ),
                reasoning: chat.reasoning,
                thinking_level_map: chat.thinking_level_map.clone(),
                prompt_cache: chat.prompt_cache.clone(),
                context_window: chat.context_window as f64,
                max_tokens: chat.max_tokens as f64,
                sampling_params: chat.sampling_params.clone(),
                sampling_params_by_thinking_level: None,
                compat: chat.compat.clone(),
            })
        }
        kiana_ext_api::ProviderModelConfig::Image(image) => {
            kiana_config::models::composer::ProviderModelConfig::Image(ProviderImageModelConfig {
                base: base(
                    &image.id,
                    &image.name,
                    &image.api,
                    &image.base_url,
                    &image.input,
                    &image.input_limits,
                    &image.cost,
                    &image.headers,
                    kiana_ai::types::ModelType::Image,
                ),
                output: image.output.clone(),
            })
        }
        kiana_ext_api::ProviderModelConfig::Classifier(classifier) => {
            kiana_config::models::composer::ProviderModelConfig::Classifier(
                ProviderClassifierModelConfig {
                    base: base(
                        &classifier.id,
                        &classifier.name,
                        &classifier.api,
                        &classifier.base_url,
                        &classifier.input,
                        &classifier.input_limits,
                        &classifier.cost,
                        &classifier.headers,
                        kiana_ai::types::ModelType::Classifier,
                    ),
                    context_window: classifier.context_window as f64,
                    compat: None,
                },
            )
        }
    }
}
