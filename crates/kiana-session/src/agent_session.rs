//! core/agent-session.ts（4458 行）：门面、钩子、队列、重试、压缩、bash、模型切换、工具注册表。
//!
//! TS 源：`packages/coding-agent/src/core/agent-session.ts`。实现按 TS 的成员顺序组织：
//! 解析辅助 → 公开类型 → 私有状态 → 构造与钩子安装 → 事件流 → prompt/队列 → 模型/thinking
//! → 压缩 → 扩展 → 重试 → bash → 树导航 → 统计/导出。
//!
//! # 并发形态
//!
//! `AgentSession` 是 `Arc<Core>` + 资源加载器的组合。`Core` 只含 `Send + Sync` 状态
//! （`agent` / `settings_manager` / `model_runtime` / `Mutex<SessionManager>` / 扩展 runner），
//! 所以安装在 [`Agent`] 上的钩子（要求 `Send + Sync`）可以持有它的 `Weak`。资源加载器
//! （`ResourceLoader` trait 没有 `Send + Sync` 约束）只由 `AgentSession` 自身持有，不进入钩子。
//!
//! 会话管理器的方法需要 `&mut self`，因此放在 `Mutex` 后面；所有临界区都是短同步段，
//! 绝不在 `.await` 期间持有。扩展派发（`runner.emit`）之前会先释放会话锁，避免扩展
//! handler 回调 `ctx.sendMessage` 时重入死锁（ARCHITECTURE §4.2）。
//!
//! # 职责边界
//!
//! 本文件把 [`AgentSessionConfig`] 中的 [`Agent`]、`SessionManager`、设置、模型运行时、
//! 工具定义与扩展 runner 组装成 [`AgentSession`]，并在 `Agent` 的各个钩子上安装会话层
//! 逻辑（工具调用扩展、turn 边界、压缩、上下文投影、消息持久化）。不负责的部分：
//! CLI/RPC 的输入输出（`agent_session_runtime` / `sdk` 及其上游）、会话文件格式
//! （`kiana-session-core`）、扩展的 JS 沙箱与事件派发实现（`kiana-extensions`）。
//!
//! # 一次 prompt 的执行流程
//!
//! 1. [`AgentSession::prompt`] 归一化图片、展开扩展命令 / skill 命令 / prompt 模板；
//!    流式期间按 `streamingBehavior` 进入 steering 或 follow-up 队列后直接返回。
//! 2. 非流式时检查模型与认证，进入 `prepare_run`：先对最后一条 assistant 消息做压缩
//!    检查，再派发 `before_agent_start`，组装本轮用户消息与本 turn 的系统提示增量。
//! 3. `start_run` 调 `agent.prompt_messages`，随后在循环里依次检查中止请求与持久化
//!    错误、执行 `handle_post_agent_run`（自动重试、溢出/阈值压缩）、派发
//!    `agent_before_settle` 边界，再调用 `agent.continue_()` 推进下一轮。
//! 4. 循环结束后冲刷待写入的 bash / custom 消息，派发 `agent_settled`，执行在该事件
//!    期间排队的动作，最后把第一个持久化错误作为 run 的错误返回。
//!
//! 循环的每一轮对应一个 agent turn；`AgentSession` 不自己驱动事件循环，而是通过
//! `install_agent_hooks` 安装到 `Agent` 上的回调被调用。
//!
//! # 主要内部结构
//!
//! - `Core`：全部 `Send + Sync` 的会话状态；每个字段的说明见结构体定义。
//! - `SessionSnapshot`：交给 `ExtensionRunner` 的实时只读会话视图；按 id 缓存泄漏的
//!   条目，避免每次读取都克隆整份会话。
//! - 线程本地注册表：`CORE_REGISTRY`（句柄 → `Weak<Core>`）、`RESOURCE_LOADERS`
//!   （`ResourceLoader` 没有 `Send + Sync` 约束，因此只能留在会话线程）与
//!   `BRIDGE_LOCAL`（把 `!Send` 的扩展派发桥回会话线程的常驻 `LocalSet`）。
//!
//! # 注意事项
//!
//! - `AgentListener` 与边界钩子都没有错误通道，持久化/压缩失败只能先记录到
//!   `Core::persistence_error`，再由 `start_run` 在 run 结束点返回；改动返回路径时
//!   必须保留这个约定，否则错误会被静默吞掉。
//! - 扩展的 `message_end` 替换在 TS 里是原地修改对象，Rust 用 `message_replacements`
//!   与 `nested_result_replacements` 两张表补齐可见性。`message_key` 决定消息身份，
//!   改动它会影响持久化、边界事件与重试判定。
//! - 公开事件（`AgentSessionEvent`）的字段顺序即 `JSON.stringify` 的键序，部分被
//!   RPC 契约锁定，不要在枚举里重排字段。
//!
//! # 阅读顺序
//!
//! 公开类型与事件 → [`AgentSessionError`] → `Core` 字段与 `install_agent_hooks` →
//! `handle_agent_event` → `start_run` 及其辅助函数 → 压缩/重试 → 工具注册表 →
//! `AgentSession` 的公开方法。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use crate::continuity::{
    ContinuityOptions, ContinuityRuntime, receipt_status, tool_result_text, written_paths_from_args,
};
use crate::memory_wiring::{
    MemoryModelBridge, MemoryWiringOptions, ProviderMemoryModel, SessionMemory,
};
use indexmap::{IndexMap, IndexSet};
use kiana_agent_core::agent::{Agent, AgentListener, Subscription};
use kiana_agent_core::agent_loop::{RunToolCallOptions, run_tool_call};
use kiana_agent_core::context_report::{
    CondensationConfig, CondensationEvent, CondensationPhase, ContextReport, ContextReportOptions,
    build_context_report_with, observe_condensation,
};
use kiana_agent_core::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopTurnUpdate,
    AgentMessage, AgentTool, AgentToolCall, AgentToolCallOutcome, BeforeToolCallContext,
    BeforeToolCallResult, PrepareNextTurnContext, QueueMode, StreamFn,
};
use kiana_agent_core::{
    AgentRunCollector, ApiCallRecord, CompactionRecord, SessionSpan, SessionTelemetry,
    ToolCallRecord, ToolStatus, TurnTracker,
};
use kiana_ai::request_policy::{
    ProviderErrorClass, TypedProviderError, account_provider_usage,
    classify_provider_error_with_typed, precheck_input_with_defaults,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, ImageContent, Message, Model,
    ModelThinkingLevel, ProviderEnv, ProviderHeaders, StopReason, SystemContent, SystemMessage,
    SystemRole, TextContent, TextKind, ToolResultMessage, TranscriptContext, Usage, UserContent,
    UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::overflow::{is_context_overflow, is_recoverable_length};
use kiana_ai::utils::retry::{
    RetryCallbacks, RetryPolicy, is_retryable_assistant_error, retry_delay_ms_js,
};
use kiana_ai::utils::text::{
    content_text_assistant, content_text_system, content_text_user, content_text_user_content,
};
use kiana_ai::utils::transcript::{
    get_current_system_message, get_current_tools, normalize_context,
};
use kiana_compaction::branch_summarization::{
    GenerateBranchSummaryOptions, collect_entries_for_branch_summary, generate_branch_summary,
};
use kiana_compaction::compaction::{
    CompactionPreparation, CompactionResult, CompactionSettings, calculate_context_tokens,
    estimate_context_tokens, estimate_projected_context_tokens, estimate_tokens, should_compact,
};
use kiana_compaction::fallback::{FallbackConfig, build_fallback_result};
use kiana_compaction::gate::{AttemptOutcome, CompactionGate};
use kiana_compaction::pipeline::{
    CompactModelOptions, HistorySnapshot as CompactionHistorySnapshot,
    compact_with_structured_template, prepare_with_pruning,
};
use kiana_compaction::pruning::PruneConfig;
use kiana_compaction::recovery::{
    PlanRecovery, RecoveryBudget, RecoveryInput, build_recovery_attachment, collect_called_skills,
    collect_context_deltas,
};
use kiana_config::cache::warmer::{CacheWarmer, CacheWarmingStatus};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{ModelRuntime, ModelRuntimeAuthOverrides, ResolveModelOptions};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::compaction::CompactionModelRef;
use kiana_config::settings::layers::{
    ConfigLayer, ConfigLayerSource, ConfigLayers, LayerLoadError, LayerLoadMode, MergedConfig,
};
use kiana_config::settings::schema::SteeringMode;
use kiana_core::artifact_store::ArtifactStore;
use kiana_core::mcp_servers::is_mcp_tool_name;
use kiana_core::slash_commands::{SlashCommandInfo, SlashCommandSource};
use kiana_core::source_info::{
    BUILTIN_PATH_PREFIX, SourceInfo, create_synthetic_source_info, is_synthetic_path,
};
use kiana_ext_api::events::{
    AgentActivityOutcome, AgentBeforeSettleEvent, AgentBeforeSettleEventType, AgentEndEvent,
    AgentEndEventType, AgentSettledEvent, AgentSettledEventType, AgentStartEvent,
    AgentStartEventType, BoundaryContextPreview, CompactionReason, ExtensionEvent,
    ExtensionEventResult, InputEventResult, InputSource, MessageEndEvent, MessageStartEvent,
    MessageStartEventType, MessageUpdateEvent, MessageUpdateEventType, ModelSelectEvent,
    ModelSelectEventType, ModelSelectSource, ResourcesDiscoverReason, SessionBoundaryDraft,
    SessionCompactEvent, SessionCompactEventType, SessionCompactFailedEvent,
    SessionCompactFailedEventType, SessionEvent, SessionInfoChangedEvent,
    SessionInfoChangedEventType, SessionStartEvent, SessionTreeEvent, SessionTreeEventType,
    StreamingBehavior, ThinkingLevelSelectEvent, ThinkingLevelSelectEventType, ToolCallEvent,
    ToolExecutionEndEvent, ToolExecutionEndEventType, ToolExecutionStartEvent,
    ToolExecutionStartEventType, ToolExecutionUpdateEvent, ToolExecutionUpdateEventType,
    ToolResultEvent, TurnEndEvent, TurnEndEventType, TurnStartEvent, TurnStartEventType,
};
use kiana_ext_api::system_prompt::{
    BuildSystemPromptOptions, NormalizedBuildSystemPromptOptions, Skill,
};
use kiana_ext_api::tool_types::ToolAnnotations;
use kiana_ext_api::types::{
    CompactOptions, DeliverAs, ExecuteToolOptions, ExtensionCommandContextActions, ExtensionError,
    ExtensionMode, SendMessageOptions, SendUserMessageDeliverAs, SendUserMessageOptions,
};
use kiana_ext_api::ui::{PermissionCardChoice, PermissionCardRequest, PermissionScopeKind};
use kiana_extensions::RegisteredTool;
use kiana_extensions::runner::{
    BeforeAgentStartTrigger, BoundaryBaseEvent, BuildBoundaryContext, ExtensionRunner,
};
use kiana_extensions::wrapper::wrap_registered_tools;
use kiana_extensions::{
    ExtensionErrorListener, ExtensionUiContext, ShutdownHandler, ToolDefinition, ToolExposure,
    ToolInfo, ToolLoadout,
};
use kiana_hooks::{
    HookAuditRecord, HookAuditSink, HookEvent, HookInput, HookOutcome, HookRegistry, HookRunner,
    HookRunnerConfig, HookSource, HookTurnState, HooksConfig, HttpHookPolicy,
};
use kiana_permissions::{
    ApprovalDecision, ApprovalScope, PermissionDecision, PermissionEngine, PermissionFlags,
    PermissionMode, PermissionPolicy, PermissionRequest, PermissionRule,
};
use kiana_resources::loader::{ResourceExtensionPath, ResourceExtensionPaths, ResourceLoader};
use kiana_resources::mcp_annotations::{allows_persistent_approval, apply_mcp_annotations};
use kiana_resources::prompts::PromptTemplate;
use kiana_resources::skills::activation::now_utc;
use kiana_resources::skills::prompt::SkillReader;
use kiana_resources::skills::{
    SKILL_TOOL_NAME, SkillDefinition, SkillTool, create_skill_tool_definition,
    evaluate_with_activations, format_skill_definitions_for_prompt_with_cwd,
    load_skill_definition_from_file, skill_index_budget_chars,
};
use kiana_resources::system_prompt::{
    build_system_prompt, build_system_prompt_sections, diff_system_prompt_sections,
    normalize_build_system_prompt_options,
};
use kiana_sandbox::{ExecutionPolicy, SandboxDecision, SandboxManager, SandboxPolicy, ToolInput};
use kiana_session_core::compaction_support::{
    CommitTransaction, HistorySnapshot, append_compaction_transactional,
};
use kiana_session_core::messages::{BashExecutionMessage, CustomMessage, convert_to_llm};
use kiana_session_core::nested_tool_calls::{
    LocalFuture, NestedToolCallHost, NestedToolCallRunner, NestedToolExecutionEvent,
};
use kiana_session_core::recovery::{
    RecoveryClass, RecoveryContinuation, RecoveryPlan, plan_manager_recovery, repair_request_view,
};
use kiana_session_core::session_export::export_session_to_jsonl;
use kiana_session_core::session_manager::{
    CompactionEntry, ContextEditReplacement, FileEntry, ReadonlySessionManager, SessionEntry,
    SessionManager, SessionManagerError, SessionProjection,
};
use kiana_session_core::usage_totals::{add_usage_to_totals, combine_usage, create_usage_totals};
use kiana_session_core::virtual_models::{
    ModelRouteReason, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelStateData, find_latest_response,
    get_branch_selection, get_virtual_model_state, is_virtual_model,
};
use kiana_tools::bash::BashOperations;
use kiana_tools::bash_executor::{BashResult, execute_bash_with_operations};
use kiana_tools::definition::create_tool_definition_from_agent_tool;
use kiana_tools::index::{
    ApplyPatchSettings, ApplyPatchToolOptions, BashToolOptions, EditToolOptions,
    OutputBudgetOptions, PowerShellToolOptions, ReadToolOptions, ToolDefinitions, ToolsOptions,
    WriteToolOptions, create_all_tool_definitions_for_model, should_expose_apply_patch,
    wrap_agent_tool_with_output_budget,
};
use kiana_tools::read::ReadStateRegistry;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;

use kiana_config::defaults::THINKING_LEVEL_OPTIONS;

// ============================================================================
// Skill Block Parsing
// ============================================================================

/// Parsed skill block from a user message.
///
/// TS: `agent-session.ts#ParsedSkillBlock`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedSkillBlock {
    /// 技能名。
    pub name: String,
    /// `SKILL.md` 路径。
    pub location: String,
    /// 技能正文。
    pub content: String,
    /// 技能块之后的可选用户消息（`trim()` 后非空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
}

/// Parse a skill block from message text.
///
/// TS: `agent-session.ts#parseSkillBlock`。正则逐字：
/// `^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n<\/skill>(?:\n\n([\s\S]+))?$`
pub fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            r#"(?s)^<skill name="([^"]+)" location="([^"]+)">\n(.*?)\n</skill>(?:\n\n(.+))?$"#,
        )
        .expect("valid skill block pattern")
    });
    let captures = pattern.captures(text)?;
    Some(ParsedSkillBlock {
        name: captures.get(1)?.as_str().to_string(),
        location: captures.get(2)?.as_str().to_string(),
        content: captures.get(3)?.as_str().to_string(),
        user_message: captures
            .get(4)
            .map(|value| js_trim(value.as_str()).to_string())
            .filter(|value| !value.is_empty()),
    })
}

/// `std::io::Error` → Node fs 异常 message（`ENOENT: no such file or directory, open '<path>'`）。
///
/// 调用方只读取 `error.message`，errno 文案属当前稳定契约，改动需同步测试。
fn node_fs_error_message(syscall: &str, path: &str, error: &std::io::Error) -> String {
    // Node/libuv 在 read 阶段的 EISDIR 不带路径（`EISDIR: illegal operation on a directory, read`）。
    if syscall == "read" && error.kind() == std::io::ErrorKind::IsADirectory {
        return "EISDIR: illegal operation on a directory, read".to_string();
    }
    #[cfg(unix)]
    let mapped = error.raw_os_error().and_then(|raw| {
        const UNIX_ERRNO: [(i32, &str, &str); 16] = [
            (1, "EPERM", "operation not permitted"),
            (2, "ENOENT", "no such file or directory"),
            (13, "EACCES", "permission denied"),
            (17, "EEXIST", "file already exists"),
            (20, "ENOTDIR", "not a directory"),
            (21, "EISDIR", "illegal operation on a directory"),
            (24, "EMFILE", "too many open files"),
            (28, "ENOSPC", "no space left on device"),
            (30, "EROFS", "read-only file system"),
            (36, "ENAMETOOLONG", "name too long"),
            (40, "ELOOP", "too many symbolic links encountered"),
            (75, "EOVERFLOW", "value too large for defined data type"),
            (95, "EOPNOTSUPP", "operation not supported on socket"),
            (116, "ESTALE", "stale file handle"),
            (122, "EDQUOT", "disk quota exceeded"),
            (0, "", ""),
        ];
        UNIX_ERRNO
            .iter()
            .find(|entry| entry.0 == raw)
            .map(|entry| (entry.1, entry.2))
    });
    #[cfg(not(unix))]
    let mapped: Option<(&str, &str)> = None;
    let (code, description) = mapped.unwrap_or_else(|| match error.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        _ => ("UNKNOWN", "unknown error"),
    });
    if code == "UNKNOWN" {
        return error.to_string();
    }
    format!("{code}: {description}, {syscall} '{path}'")
}

/// JS `String.prototype.trim` 的空白语义：字符集是 ECMAScript 的 WhiteSpace +
/// LineTerminator（含 U+FEFF，不含 U+0085），与 Rust 的 `char::is_whitespace` 不同，
/// 因此显式列出字符而不是用 `char::is_whitespace`。
fn js_trim(text: &str) -> &str {
    text.trim_matches(|character: char| {
        matches!(
            character,
            '\t' | '\n'
                | '\u{000B}'
                | '\u{000C}'
                | '\r'
                | ' '
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    })
}

/// `Object.fromEntries(Object.entries(headers).filter(([, v]) => v !== null))`。
///
/// TS: `agent-session.ts#withoutDeletedHeaders`。
fn without_deleted_headers(headers: Option<ProviderHeaders>) -> Option<ProviderHeaders> {
    headers.map(|headers| {
        headers
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, Some(value))))
            .collect()
    })
}

/// `Σ estimateTokens(m)`。
///
/// TS: `agent-session.ts#estimateMessagesTokens`。
fn estimate_messages_tokens(messages: &[AgentMessage]) -> u64 {
    messages.iter().map(estimate_tokens).sum()
}

// ============================================================================
// Public types
// ============================================================================

/// Session-specific events that extend the core [`AgentEvent`].
///
/// TS: `agent-session.ts#AgentSessionEvent`。字段顺序与 `JSON.stringify` 的键序一致；
/// `None` 字段按 TS 的 `undefined` 省略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum AgentSessionEvent {
    /// `agent_start`。
    #[serde(rename = "agent_start")]
    AgentStart,
    /// `agent_end`（带 `willRetry`）。
    #[serde(rename = "agent_end")]
    AgentEnd {
        /// 本次 run 的消息。
        messages: Vec<AgentMessage>,
        /// 是否会自动重试。
        will_retry: bool,
    },
    /// `agent_settled`。
    #[serde(rename = "agent_settled")]
    AgentSettled {
        /// 是否因中止而结束。
        aborted: bool,
    },
    /// `queue_update`。
    #[serde(rename = "queue_update")]
    QueueUpdate {
        /// 排队中的 steering 文本。
        steering: Vec<String>,
        /// 排队中的 follow-up 文本。
        follow_up: Vec<String>,
    },
    /// `compaction_start`。
    #[serde(rename = "compaction_start")]
    CompactionStart {
        /// 触发原因。
        reason: CompactionReason,
    },
    /// `compaction_end`。
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        /// 触发原因。
        reason: CompactionReason,
        /// 压缩结果（失败/取消时省略）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<CompactionResult>,
        /// 是否被取消/中止。
        aborted: bool,
        /// 是否会在压缩后重试。
        will_retry: bool,
        /// 失败文本。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    /// `condensation`：手动压缩的收敛观测结果。
    ///
    /// `compact()` 在提交压缩后以 token 下降并稳定为判据发出该事件；超时/失败时
    /// `event.phase` 为 `failed`。UI 与 RPC 都应展示 `saved_tokens`，而不是只报告
    /// “压缩完成”。
    #[serde(rename = "condensation")]
    Condensation {
        /// 观测事件。
        event: CondensationEvent,
    },
    /// `hook_notification`：hook 产生的用户可见消息。
    ///
    /// `level` 为 `"info"`（hook 的 `systemMessage`）或 `"warning"`（非阻塞警告：
    /// exit 1、超时、非法 JSON、HTTP 非 2xx、SSRF/白名单拒绝等）。`hook_event` 是
    /// 触发消息的事件名。`suppressOutput: true` 时 `systemMessage` 不产生 info 消息，
    /// 但 warning 仍会发出——诊断信息不应被 hook 隐藏。
    #[serde(rename = "hook_notification")]
    HookNotification {
        /// 消息文本。
        message: String,
        /// 级别：`info` / `warning`。
        level: String,
        /// 事件名（`PreToolUse` 等）。
        hook_event: String,
    },
    /// `entry_appended`。
    #[serde(rename = "entry_appended")]
    EntryAppended {
        /// 新增条目。
        entry: SessionEntry,
    },
    /// `session_info_changed`。
    #[serde(rename = "session_info_changed")]
    SessionInfoChanged {
        /// 会话名；清除时省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// `thinking_level_changed`。
    #[serde(rename = "thinking_level_changed")]
    ThinkingLevelChanged {
        /// 新的 thinking level。
        level: ModelThinkingLevel,
    },
    /// `auto_retry_start`。
    #[serde(rename = "auto_retry_start")]
    AutoRetryStart {
        /// 第几次尝试。
        attempt: u64,
        /// 最大次数。
        max_attempts: u64,
        /// 退避毫秒。
        delay_ms: u64,
        /// 错误文本。
        error_message: String,
    },
    /// `auto_retry_end`。
    #[serde(rename = "auto_retry_end")]
    AutoRetryEnd {
        /// 是否成功。
        success: bool,
        /// 尝试次数。
        attempt: u64,
        /// 最终错误（失败时）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
    /// `summarization_retry_scheduled`。
    #[serde(rename = "summarization_retry_scheduled")]
    SummarizationRetryScheduled {
        /// 第几次尝试。
        attempt: u64,
        /// 最大次数。
        max_attempts: u64,
        /// 退避毫秒。
        delay_ms: u64,
        /// 错误文本。
        error_message: String,
    },
    /// `summarization_retry_attempt_start`。
    #[serde(rename = "summarization_retry_attempt_start")]
    SummarizationRetryAttemptStart {
        /// 来源（`branchSummary` / `compaction`）。
        source: SummarizationRetrySource,
        /// 压缩原因（仅 `source: "compaction"`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<CompactionReason>,
    },
    /// `summarization_retry_finished`。
    #[serde(rename = "summarization_retry_finished")]
    SummarizationRetryFinished,
    /// `bash_execution_update`。
    #[serde(rename = "bash_execution_update")]
    BashExecutionUpdate {
        /// 可选标识。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// 输出增量。
        delta: String,
    },
    /// `permission_requested`：一次 `Ask` 即将询问用户。
    ///
    /// `summary` 是脱敏后的参数摘要；`target` 是命令前缀/路径/工具名。事件里不出现
    /// secret 明文，完整命令只在审批 UI 中展示。
    #[serde(rename = "permission_requested")]
    PermissionRequested {
        /// 工具名。
        tool_name: String,
        /// 动作类别（`read`/`write`/`execute`/...）。
        action: String,
        /// 脱敏后的参数摘要。
        summary: String,
        /// 目标摘要（命令前缀/路径/工具名）。
        target: String,
        /// 为什么要询问（模式/规则/安全策略）。
        reason: String,
    },
    /// `permission_decided`：用户对一次 `Ask` 做出了选择。
    #[serde(rename = "permission_decided")]
    PermissionDecided {
        /// 工具名。
        tool_name: String,
        /// `allow-once` / `allow-always` / `reject`。
        decision: String,
        /// `allow-always` 的作用域：`session` / `local` / `project`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// 用户给出的拒绝原因。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// 目标摘要。
        target: String,
    },
    /// `permission_denied`：一次调用最终被拒绝（策略或用户）。
    #[serde(rename = "permission_denied")]
    PermissionDenied {
        /// 工具名。
        tool_name: String,
        /// 拒绝原因（会回传给模型）。
        reason: String,
        /// 目标摘要。
        target: String,
    },
    /// `turn_start`。
    #[serde(rename = "turn_start")]
    TurnStart,
    /// `turn_end`。
    #[serde(rename = "turn_end")]
    TurnEnd {
        /// 本 turn 的最终消息。
        message: AgentMessage,
        /// 工具结果消息。
        tool_results: Vec<ToolResultMessage>,
    },
    /// `message_start`。
    #[serde(rename = "message_start")]
    MessageStart {
        /// 消息。
        message: AgentMessage,
    },
    /// `message_update`。
    ///
    /// 字段顺序同 TS 的 `{ type, message, assistantMessageEvent }`（serde 按声明顺序输出）。
    #[serde(rename = "message_update")]
    MessageUpdate {
        /// 消息。
        message: AgentMessage,
        /// provider 风格的 assistant 流事件。
        assistant_message_event: kiana_ai::types::AssistantMessageEvent,
    },
    /// `message_end`。
    #[serde(rename = "message_end")]
    MessageEnd {
        /// 消息。
        message: AgentMessage,
    },
    /// `tool_execution_start`。
    #[serde(rename = "tool_execution_start")]
    ToolExecutionStart {
        /// 调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 参数。
        args: serde_json::Value,
        /// 嵌套调用时指向父调用。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    /// `tool_execution_update`。
    #[serde(rename = "tool_execution_update")]
    ToolExecutionUpdate {
        /// 调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 参数。
        args: serde_json::Value,
        /// 部分结果。
        partial_result: serde_json::Value,
        /// 嵌套调用时指向父调用。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
    /// `tool_execution_end`。
    #[serde(rename = "tool_execution_end")]
    ToolExecutionEnd {
        /// 调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 结果。
        result: serde_json::Value,
        /// 是否错误。
        is_error: bool,
        /// 工具执行耗时（毫秒）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
        /// 嵌套调用时指向父调用。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_call_id: Option<String>,
    },
}

/// `summarization_retry_attempt_start` 的 `source`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SummarizationRetrySource {
    /// `"branchSummary"`。
    #[serde(rename = "branchSummary")]
    BranchSummary,
    /// `"compaction"`。
    #[serde(rename = "compaction")]
    Compaction,
}

/// Listener function for agent session events.
///
/// TS: `agent-session.ts#AgentSessionEventListener`。
pub type AgentSessionEventListener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;

/// 取消订阅句柄（TS 的 `subscribe()` 返回值）。只有显式调用才退订。
pub type AgentSessionUnsubscribe = Box<dyn Fn() + Send + Sync>;

/// `ExtensionBindings`。
///
/// TS: `agent-session.ts#ExtensionBindings`。Rust 侧的所有回调都已带 `Send + Sync`。
#[derive(Clone, Default)]
pub struct ExtensionBindings {
    /// 交互式 UI 上下文。
    pub ui_context: Option<Arc<dyn ExtensionUiContext>>,
    /// 运行模式。
    pub mode: Option<ExtensionMode>,
    /// 命令上下文动作（newSession/fork/...）。
    pub command_context_actions: Option<ExtensionCommandContextActions>,
    /// 中止处理。
    pub abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    /// 优雅关闭处理。
    pub shutdown_handler: Option<ShutdownHandler>,
    /// 扩展错误监听。
    pub on_error: Option<ExtensionErrorListener>,
}

/// `QueuedInputDisposition`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedInputDisposition {
    /// 被扩展处理。
    Handled,
    /// 已排队。
    Queued,
}

/// `PromptDisposition`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDisposition {
    /// 被扩展处理。
    Handled,
    /// 已排队。
    Queued,
    /// 已开始 run。
    Started,
}

/// Options for `AgentSession.prompt()`.
///
/// TS: `agent-session.ts#PromptOptions`。
#[derive(Clone, Default)]
pub struct PromptOptions {
    /// 是否展开扩展命令 / skill 命令 / prompt 模板（默认 true）。
    pub expand_prompt_templates: Option<bool>,
    /// 图片附件。
    pub images: Option<Vec<ImageContent>>,
    /// 流式期间的排队方式。
    pub streaming_behavior: Option<StreamingBehavior>,
    /// 输入来源（默认 `interactive`）。
    pub source: Option<InputSource>,
    /// RPC 的 preflight 回调。
    pub preflight_result: Option<Arc<dyn Fn(PromptDisposition) + Send + Sync>>,
}

/// Options for model/thinking mutations.
///
/// TS: `agent-session.ts#ModelMutationOptions`。
#[derive(Clone, Copy, Default)]
pub struct ModelMutationOptions {
    /// 是否写入全局默认值（默认仅会话内）。
    pub persist: Option<bool>,
}

/// Result from `cycleModel()`.
///
/// TS: `agent-session.ts#ModelCycleResult`。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCycleResult {
    /// 新模型。
    pub model: Model,
    /// 新的 thinking level。
    pub thinking_level: ModelThinkingLevel,
    /// 是否在 scoped 模型间循环。
    pub is_scoped: bool,
}

/// `routedModel` 的返回值（`thinkingLevel` 可缺省）。
///
/// TS: `agent-session.ts#routedModel` 的返回类型。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedModel {
    /// 产生最新响应的物理模型。
    pub model: Model,
    /// 该响应的 thinking level；消息没有记录时为 `None`（TS 的 `undefined`）。
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// 作用域模型（`--models` 的条目）。
///
/// TS: `Array<{ model: Model; thinkingLevel?: ThinkingLevel }>`。
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedModel {
    /// 模型。
    pub model: Model,
    /// 可选的 thinking level。
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// Session statistics for `/session`.
///
/// TS: `agent-session.ts#SessionStats`。键序固定（ca-rpc-print.md §2.5.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    /// 会话文件路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    /// 会话 id。
    pub session_id: String,
    /// 用户消息数。
    pub user_messages: u64,
    /// assistant 消息数。
    pub assistant_messages: u64,
    /// 工具调用数。
    pub tool_calls: u64,
    /// 工具结果数。
    pub tool_results: u64,
    /// 消息总数。
    pub total_messages: u64,
    /// 累计 token。
    pub tokens: SessionStatsTokens,
    /// 累计成本。
    pub cost: f64,
    /// 上下文用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<kiana_ext_api::types::ContextUsage>,
}

/// [`SessionStats`] 的 `tokens`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatsTokens {
    /// 输入 token。
    pub input: f64,
    /// 输出 token。
    pub output: f64,
    /// 缓存读 token。
    pub cache_read: f64,
    /// 缓存写 token。
    pub cache_write: f64,
    /// 总计。
    pub total: f64,
}

/// `navigateTree()` 的结果。
///
/// TS: `{ editorText?: string; cancelled: boolean; aborted?: boolean; summaryEntry?: BranchSummaryEntry }`。
#[derive(Debug, Clone, Default)]
pub struct TreeNavigationResult {
    /// 目标为用户消息时的文本。
    pub editor_text: Option<String>,
    /// 是否取消。
    pub cancelled: bool,
    /// 是否因中止取消。
    pub aborted: Option<bool>,
    /// 新增的分支摘要条目。
    pub summary_entry: Option<SessionEntry>,
}

/// `navigateTree()` 的选项。
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    /// 是否总结被放弃分支。
    pub summarize: Option<bool>,
    /// 自定义摘要指令。
    pub custom_instructions: Option<String>,
    /// 是否用自定义指令替换默认 prompt。
    pub replace_instructions: Option<bool>,
    /// 摘要条目的标签。
    pub label: Option<String>,
}

/// 会话的 plan / plan-mode 状态（第 6 周 pruning 保护与压缩恢复使用）。
///
/// 由 plan-mode 扩展通过 `appendEntry("plan-mode", ...)` 驱动：
/// `{ "enabled": true, "planFilePath": "local://PLAN.md", "approved": false }`。
/// 宿主也可以直接调用 [`AgentSession::set_plan_state`]。`content` 缺省时从 `path`
/// 读取（相对路径按会话 cwd 解析）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionPlanState {
    /// plan 文件路径（支持 `local://` 前缀）。
    pub path: Option<String>,
    /// plan 内容；`None` 时按 `path` 读取。
    pub content: Option<String>,
    /// 是否处于 plan mode。
    pub plan_mode: bool,
    /// plan 是否已获批准。
    pub approved: bool,
}

impl SessionPlanState {
    /// 转成压缩恢复输入。
    pub fn recovery(&self) -> PlanRecovery {
        PlanRecovery {
            path: self.path.clone(),
            content: self.content.clone(),
            plan_mode: self.plan_mode,
            approved: self.approved,
        }
    }

    /// pruning 保护的 active plan 路径列表。
    pub fn prune_paths(&self) -> Vec<String> {
        self.path.iter().cloned().collect()
    }
}

/// `AgentSessionConfig`。
///
/// TS: `agent-session.ts#AgentSessionConfig`。
pub struct AgentSessionConfig {
    /// Agent 实例。
    pub agent: Agent,
    /// 会话管理器。
    pub session_manager: SessionManager,
    /// 设置管理器。
    pub settings_manager: SettingsManager,
    /// 工作目录。
    pub cwd: String,
    /// `--models` 的作用域模型。
    pub scoped_models: Vec<ScopedModel>,
    /// 资源加载器。
    pub resource_loader: Arc<dyn ResourceLoader>,
    /// SDK 自定义工具。
    pub custom_tools: Vec<ToolDefinition>,
    /// 模型/认证运行时。
    pub model_runtime: ModelRuntime,
    /// 缓存预热器。
    pub cache_warmer: Option<CacheWarmer>,
    /// 初始激活的内建工具名（默认 read/bash/edit/write）。
    pub initial_active_tool_names: Option<Vec<String>>,
    /// 初始工具是否来自 `defaultTools` 设置。
    pub uses_default_tools: Option<bool>,
    /// `+name`/`-name` 修正项。
    pub default_tool_modifiers: Vec<String>,
    /// `--tools` 允许列表（名字或 `*` 模式）。
    pub allowed_tool_names: Option<Vec<String>>,
    /// `--exclude-tools` 拒绝列表。
    pub excluded_tool_names: Option<Vec<String>>,
    /// 覆盖基础工具。
    pub base_tools_override: Option<IndexMap<String, AgentTool>>,
    /// 可变 runner 引用（供 Agent 钩子读取当前 runner）。
    pub extension_runner_ref: Option<Arc<Mutex<Option<Arc<ExtensionRunner>>>>>,
    /// 扩展绑定时发出的 `session_start` 元数据。
    pub session_start_event: Option<SessionStartEvent>,
    /// 第 10 周持久记忆接线；`None` 时回退到 `KIANA_MEMORY` 环境变量（默认关闭）。
    pub memory: Option<MemoryWiringOptions>,
    /// 模型回答语言（`zh` / `en` / …）。`None` 时回退到 `KIANA_OUTPUT_LANGUAGE` 设置
    /// （`output_language` 或 `ui.outputLanguage`）；与 UI locale 分离。
    pub output_language: Option<String>,
    /// 第 11 周后半的连续性接线（执行所有权 / checkpoint / ledger / 验收 / 后台任务）。
    /// `None` 时回退到 `KIANA_CONTINUITY` 环境变量（默认关闭）。
    pub continuity: Option<ContinuityOptions>,
    /// settings 显式关闭 continuity（`continuity: false` / `{enabled:false}`）。
    ///
    /// `continuity` 为 `None` 时不能再用环境变量回退：显式关闭必须压过
    /// `KIANA_CONTINUITY=1`。
    pub continuity_disabled: bool,
    /// 第 9 周遥测实例；`None` 时从环境变量构造（`OTEL_EXPORTER_OTLP_*`）。
    ///
    /// 测试/宿主注入后 `flush_telemetry()` 使用同一实例；不设置时行为不变。
    pub telemetry: Option<Arc<SessionTelemetry>>,
}

// ============================================================================
// Errors
// ============================================================================

/// `AgentSession` 的公开错误。`Display` 等于 TS 的 `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionError {
    /// 压缩进行中提交 prompt。
    CompactionInProgress,
    /// 流式期间未指定 `streamingBehavior`。
    StreamingBehaviorRequired,
    /// 排队扩展命令。
    ExtensionCommandCannotBeQueued { command_name: String },
    /// 未选择模型。
    NoModelSelected,
    /// 没有可用 API key。
    NoApiKey { provider: String },
    /// OAuth 凭据失效。
    AuthenticationFailed { provider: String },
    /// 已压缩过。
    AlreadyCompacted,
    /// 没有可压缩内容。
    NothingToCompact,
    /// 压缩被取消。
    CompactionCancelled,
    /// 压缩失败。
    CompactionFailed { message: String },
    /// 没有可用于摘要的模型。
    NoModelForSummarization,
    /// 流式期间导航树。
    TreeWhileStreaming,
    /// 压缩/树导航期间导航树。
    TreeWhileCompacting,
    /// 目标条目不存在。
    EntryNotFound { id: String },
    /// 扩展失败阻止执行。
    ExtensionFailedBlocking { message: String },
    /// 会话头缺失。
    SessionHeaderMissing,
    /// recovery omission 无法找到来源条目。
    RecoveryOmissionUnresolved,
    /// 其它错误：消息原样透传（Display 直接输出，调用方按文本判断）。
    Other(String),
}

impl std::fmt::Display for AgentSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentSessionError::CompactionInProgress => formatter.write_str(
                "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.",
            ),
            AgentSessionError::StreamingBehaviorRequired => formatter.write_str(
                "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
            ),
            AgentSessionError::ExtensionCommandCannotBeQueued { command_name } => write!(
                formatter,
                "Extension command \"/{command_name}\" cannot be queued. Use prompt() or execute the command when not streaming.",
            ),
            AgentSessionError::NoModelSelected => {
                formatter.write_str(&kiana_config::auth_guidance::format_no_model_selected_message())
            }
            AgentSessionError::NoApiKey { provider } => {
                formatter.write_str(&kiana_config::auth_guidance::format_no_api_key_found_message(provider))
            }
            AgentSessionError::AuthenticationFailed { provider } => write!(
                formatter,
                "Authentication failed for \"{provider}\". Credentials may have expired or network is unavailable. Run '/login {provider}' to re-authenticate.",
            ),
            AgentSessionError::AlreadyCompacted => formatter.write_str("Already compacted"),
            AgentSessionError::NothingToCompact => {
                formatter.write_str("Nothing to compact (session too small)")
            }
            AgentSessionError::CompactionCancelled => formatter.write_str("Compaction cancelled"),
            AgentSessionError::CompactionFailed { message } => {
                write!(formatter, "Compaction failed: {message}")
            }
            AgentSessionError::NoModelForSummarization => {
                formatter.write_str("No model available for summarization")
            }
            AgentSessionError::TreeWhileStreaming => formatter.write_str(
                "Wait for the current response to finish before navigating the session tree.",
            ),
            AgentSessionError::TreeWhileCompacting => formatter.write_str(
                "Wait for the current compaction or tree navigation to finish before navigating the session tree.",
            ),
            AgentSessionError::EntryNotFound { id } => write!(formatter, "Entry {id} not found"),
            AgentSessionError::ExtensionFailedBlocking { message } => {
                write!(formatter, "Extension failed, blocking execution: {message}")
            }
            AgentSessionError::SessionHeaderMissing => formatter.write_str("Session header is missing"),
            AgentSessionError::RecoveryOmissionUnresolved => formatter.write_str(
                "Cannot persist recovery omission because a projected message has no source entry",
            ),
            AgentSessionError::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AgentSessionError {}

/// 正在重放的 `InterruptedPrompt` 尾部 user。
struct RecoveryReplaySeed {
    entry_id: String,
    message_key: String,
    message: kiana_agent_core::AgentMessage,
}

impl AgentSessionError {
    /// 包装任意错误文本（TS 的 `Error.message`）。
    pub fn other(message: impl Into<String>) -> Self {
        AgentSessionError::Other(message.into())
    }
}

// ============================================================================
// JS helpers
// ============================================================================

/// `Mutex::lock` 的封装：锁中毒（其他线程在持锁期间 panic）时直接取回数据继续，
/// 不把 poisoning 传播给会话层调用方。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `Date.now()`。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 消息的稳定键（Rust 值语义下替代 TS 的对象身份）。
fn message_key(message: &AgentMessage) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

/// `Array.prototype.indexOf`。
fn index_of(haystack: &[String], needle: &str) -> Option<usize> {
    haystack.iter().position(|value| value == needle)
}

fn remove_at(values: &mut Vec<String>, index: usize) -> String {
    values.remove(index)
}

/// 文本消息内容块。
fn text_content(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

/// `contentText(message.content, "")`，按消息角色分派。
fn agent_message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::User(user)) => content_text_user_content(&user.content, ""),
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            content_text_assistant(&assistant.content, "")
        }
        AgentMessage::Standard(Message::ToolResult(result)) => {
            content_text_user(&result.content, "")
        }
        AgentMessage::Standard(Message::System(system)) => content_text_system(&system.content, ""),
        AgentMessage::Custom(object) => {
            // TS 的 `contentText(event.message.content, "")`：字符串原样返回；数组取 text 块。
            match object.get("content") {
                Some(serde_json::Value::String(text)) => text.clone(),
                Some(serde_json::Value::Array(blocks)) => blocks
                    .iter()
                    .filter(|&block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    })
                    .map(|block| {
                        block
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            }
        }
    }
}

/// 从消息里取 assistant 视图。
fn as_assistant(message: &AgentMessage) -> Option<&AssistantMessage> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => Some(assistant),
        _ => None,
    }
}

/// 从 JSON 值构造 `ToolCallEvent`（内建/自定义变体由 serde 分派）。
fn tool_call_event_from_json(value: serde_json::Value) -> Option<ToolCallEvent> {
    serde_json::from_value(value).ok()
}

/// 从 JSON 值构造 `ToolResultEvent`。
fn tool_result_event_from_json(value: serde_json::Value) -> Option<ToolResultEvent> {
    serde_json::from_value(value).ok()
}

/// `CustomMessage`（typed 视图）→ `AgentMessage`。
fn custom_message_agent_message(message: &CustomMessage) -> AgentMessage {
    message.to_agent_message()
}

// ============================================================================
// Session snapshot（供 ExtensionRunner 的 ReadonlySessionManager）
// ============================================================================

/// `ExtensionRunner::new` 需要 `Arc<dyn ReadonlySessionManager>`，而 `SessionManager` 的
/// 变更方法需要 `&mut self`。这里提供一个**实时只读视图**：读取直接访问 `Core` 里的
/// 管理器（`Weak<Core>` 升级 + 短锁），不复制整份会话。
///
/// `ReadonlySessionManager` 的引用返回方法（`get_entry` / `get_label` / `get_leaf_id` /
/// `get_header`）要求 `&self` 生命周期的稳定地址，而锁 guard 不能逃逸。解法是**按值缓存**：
/// 每条会话条目的不可变克隆只泄漏一次（按 id 索引），叶子 id / 标签 / 头部同理。这样
/// 泄漏量是 O(会话条目数)，而不是每次读取复制一整份管理器（原实现的 O(n²)）。
pub(crate) struct SessionSnapshot {
    cwd: String,
    session_dir: String,
    session_id: String,
    session_file: Option<String>,
    owner: Mutex<Weak<Core>>,
    /// 条目 id → 泄漏的不可变条目（条目一旦写入就不再修改）。
    entries: Mutex<HashMap<String, &'static SessionEntry>>,
    /// 叶子 id 值 → 泄漏的字符串。
    leaf_ids: Mutex<HashMap<String, &'static str>>,
    /// 标签值 → 泄漏的字符串。
    labels: Mutex<HashMap<String, &'static str>>,
    /// 会话头（构造后不再变化）。
    header: Mutex<Option<&'static kiana_session_core::session_manager::SessionHeader>>,
}

impl SessionSnapshot {
    fn new(manager: &SessionManager) -> Self {
        SessionSnapshot {
            cwd: manager.get_cwd().to_string(),
            session_dir: manager.get_session_dir().to_string(),
            session_id: manager.get_session_id().to_string(),
            session_file: manager.get_session_file().map(str::to_string),
            owner: Mutex::new(Weak::new()),
            entries: Mutex::new(HashMap::new()),
            leaf_ids: Mutex::new(HashMap::new()),
            labels: Mutex::new(HashMap::new()),
            header: Mutex::new(None),
        }
    }

    /// 绑定所属会话（`install_agent_hooks` 时调用；`Weak` 不构成循环引用）。
    fn set_owner(&self, core: &Arc<Core>) {
        *lock(&self.owner) = Arc::downgrade(core);
    }

    /// 在会话管理器的短锁内执行读取；会话已释放时返回 `fallback`。
    fn with_manager<R>(&self, f: impl FnOnce(&SessionManager) -> R, fallback: R) -> R {
        let owner = lock(&self.owner).upgrade();
        let Some(core) = owner else {
            return fallback;
        };
        let guard = lock(&core.session_manager);
        f(&guard)
    }

    /// 取得条目的稳定引用（同 id 只泄漏一次）。
    fn cached_entry(&self, entry: &SessionEntry) -> &'static SessionEntry {
        let id = entry.id().to_string();
        let mut cache = lock(&self.entries);
        if let Some(existing) = cache.get(&id) {
            return existing;
        }
        let leaked: &'static SessionEntry = Box::leak(Box::new(entry.clone()));
        cache.insert(id, leaked);
        leaked
    }

    fn cached_str(cache: &Mutex<HashMap<String, &'static str>>, value: &str) -> &'static str {
        let mut cache = lock(cache);
        if let Some(existing) = cache.get(value) {
            return existing;
        }
        let leaked: &'static str = Box::leak(value.to_string().into_boxed_str());
        cache.insert(value.to_string(), leaked);
        leaked
    }
}

impl ReadonlySessionManager for SessionSnapshot {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }
    fn get_session_dir(&self) -> &str {
        &self.session_dir
    }
    fn get_session_id(&self) -> &str {
        &self.session_id
    }
    fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }
    fn get_leaf_id(&self) -> Option<&str> {
        self.with_manager(
            |manager| {
                manager
                    .get_leaf_id()
                    .map(|id| Self::cached_str(&self.leaf_ids, id))
            },
            None,
        )
    }
    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        self.with_manager(
            |manager| {
                manager
                    .get_leaf_entry()
                    .map(|entry| self.cached_entry(entry))
            },
            None,
        )
    }
    fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.with_manager(
            |manager| manager.get_entry(id).map(|entry| self.cached_entry(entry)),
            None,
        )
    }
    fn get_label(&self, id: &str) -> Option<&str> {
        self.with_manager(
            |manager| {
                manager
                    .get_label(id)
                    .map(|label| Self::cached_str(&self.labels, label))
            },
            None,
        )
    }
    fn get_branch(&self, from_id: Option<&str>) -> Vec<SessionEntry> {
        self.with_manager(|manager| manager.get_branch(from_id), Vec::new())
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        self.with_manager(SessionManager::build_context_entries, Vec::new())
    }
    fn build_session_projection(&self) -> SessionProjection {
        self.with_manager(
            SessionManager::build_session_projection,
            SessionProjection {
                entries: Vec::new(),
                messages: Vec::new(),
                thinking_level: String::new(),
                model: None,
            },
        )
    }
    fn get_header(&self) -> Option<&kiana_session_core::session_manager::SessionHeader> {
        let live = self.with_manager(|manager| manager.get_header().cloned(), None)?;
        if let Some(cached) = *lock(&self.header)
            && cached.id == live.id
        {
            return Some(cached);
        }
        let leaked: &'static _ = Box::leak(Box::new(live));
        *lock(&self.header) = Some(leaked);
        Some(leaked)
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        self.with_manager(SessionManager::get_entries, Vec::new())
    }
    fn get_tree(&self) -> Vec<kiana_session_core::session_manager::SessionTreeNode> {
        self.with_manager(SessionManager::get_tree, Vec::new())
    }
    fn get_session_name(&self) -> Option<String> {
        self.with_manager(SessionManager::get_session_name, None)
    }
}

// ============================================================================
// Private state helpers
// ============================================================================

/// 「下一条 user 消息」投递方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliverMode {
    Steer,
    FollowUp,
}

/// `_deferredSettledActions` 的条目（TS 是闭包；Rust 用可枚举的操作避免持有非 Send 的
/// `AgentSession`）。
#[derive(Clone)]
enum DeferredSettledAction {
    Prompt {
        text: String,
        options: PromptOptions,
    },
    CustomMessage {
        message: CustomMessage,
        trigger_turn: Option<bool>,
        deliver_as: Option<DeliverAs>,
    },
}

/// 工具名匹配器（`allowedTools` / `excludedTools`）。
type ToolNameMatcher = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// `ctx.getSystemPromptOptions()` 的动作。
type GetSystemPromptOptionsAction = Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>;
/// `ctx.executeTool()` 的动作。
type ExecuteToolAction = Arc<
    dyn Fn(
            String,
            String,
            serde_json::Value,
            ExecuteToolOptions,
        ) -> kiana_agent_core::types::BoxFuture<'static, AgentToolCallOutcome>
        + Send
        + Sync,
>;
/// `ctx.tools` 的动作。
type GetCallableToolsAction = Arc<dyn Fn() -> Vec<AgentTool> + Send + Sync>;
/// 扩展注册原生 provider 的动作。
type RegisterNativeProviderAction = Arc<dyn Fn(Arc<dyn kiana_ai::models::Provider>) + Send + Sync>;
/// `executeBash` 的流式输出回调。
type BashChunkCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// 工具定义 + 来源。
#[derive(Clone)]
struct ToolDefinitionEntry {
    definition: ToolDefinition,
    source_info: SourceInfo,
}

/// 命令清单快照（扩展 `getCommands` 与 `slashCommands` 用）。
#[derive(Default)]
struct CommandSources {
    prompts: Vec<PromptTemplate>,
    skills: Vec<Skill>,
    context_files: Vec<kiana_ext_api::system_prompt::ContextFile>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
}

/// 嵌套工具调用的宿主（实现 `NestedToolCallHost`）。
///
/// TS 的 `getTools` 是活回调（`() => this._getCallableTools()`），因此这里每次读取都
/// 重新解析；`NestedToolCallHost::get_tools` 返回借用切片，所以刷新后的快照必须是稳定
/// 地址（见 [`SessionNestedHost::get_tools`]）。
struct SessionNestedHost {
    core: Weak<Core>,
    tools: Mutex<NestedToolCache>,
}

/// `get_tools` 的稳定快照缓存。
struct NestedToolCache {
    signature: Vec<(String, Option<kiana_agent_core::types::ToolExecutionMode>)>,
    tools: &'static [AgentTool],
}

/// 把嵌套工具执行事件发给扩展与公开监听者（TS `NestedToolCallRunner.emit` 的宿主实现）。
async fn emit_nested_tool_event(core: &Core, event: NestedToolExecutionEvent) {
    if let Some(runner) = core.runner() {
        let extension_event = match &event {
            NestedToolExecutionEvent::Start {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => Some(ExtensionEvent::ToolExecutionStart(
                ToolExecutionStartEvent {
                    event_type: ToolExecutionStartEventType::ToolExecutionStart,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    parent_tool_call_id: Some(parent_tool_call_id.clone()),
                },
            )),
            NestedToolExecutionEvent::Update {
                tool_call_id,
                tool_name,
                args,
                partial_result,
                parent_tool_call_id,
            } => Some(ExtensionEvent::ToolExecutionUpdate(
                ToolExecutionUpdateEvent {
                    event_type: ToolExecutionUpdateEventType::ToolExecutionUpdate,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    partial_result: serde_json::to_value(partial_result)
                        .unwrap_or(serde_json::Value::Null),
                    parent_tool_call_id: Some(parent_tool_call_id.clone()),
                },
            )),
            NestedToolExecutionEvent::End {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
                parent_tool_call_id,
            } => Some(ExtensionEvent::ToolExecutionEnd(ToolExecutionEndEvent {
                event_type: ToolExecutionEndEventType::ToolExecutionEnd,
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                result: serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                is_error: *is_error,
                duration_ms: duration_ms.map(|value| value as u64),
                parent_tool_call_id: Some(parent_tool_call_id.clone()),
            })),
        };
        if let Some(extension_event) = extension_event {
            let _ = runner.emit(extension_event).await;
        }
    }
    let public_event = match event {
        NestedToolExecutionEvent::Start {
            tool_call_id,
            tool_name,
            args,
            parent_tool_call_id,
        } => Some(AgentSessionEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
            parent_tool_call_id: Some(parent_tool_call_id),
        }),
        NestedToolExecutionEvent::Update {
            tool_call_id,
            tool_name,
            args,
            partial_result,
            parent_tool_call_id,
        } => Some(AgentSessionEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result: serde_json::to_value(partial_result).unwrap_or(serde_json::Value::Null),
            parent_tool_call_id: Some(parent_tool_call_id),
        }),
        NestedToolExecutionEvent::End {
            tool_call_id,
            tool_name,
            result,
            is_error,
            duration_ms,
            parent_tool_call_id,
        } => Some(AgentSessionEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result: serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
            is_error,
            duration_ms,
            parent_tool_call_id: Some(parent_tool_call_id),
        }),
    };
    if let Some(public_event) = public_event {
        core.emit(public_event);
    }
}

/// 嵌套调用开始前会话已释放（或找不到宿主 assistant 消息）时返回的错误结果：
/// 正文固定为 "No assistant message issued this call" 且 `is_error` 为真。
fn nested_no_assistant_outcome(tool_call: AgentToolCall) -> AgentToolCallOutcome {
    AgentToolCallOutcome {
        tool_call,
        result: kiana_agent_core::AgentToolResult {
            content: vec![text_content("No assistant message issued this call")],
            details: serde_json::Value::Null,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        },
        is_error: true,
        duration_ms: None,
    }
}

impl NestedToolCallHost for SessionNestedHost {
    fn get_tools(&self) -> &[AgentTool] {
        // 与 TS 的活回调一致：按调用时的 callable 工具集决定独占执行（`run_tool_call`
        // 本身已经重新解析工具）。返回类型是借用切片，因此快照只在签名变化时替换；
        // 旧快照泄漏是有界的（每次工具集/执行模式变化一次）。
        let Some(core) = self.core.upgrade() else {
            return &[];
        };
        let current = core.get_callable_tools();
        let signature: Vec<(String, Option<kiana_agent_core::types::ToolExecutionMode>)> = current
            .iter()
            .map(|tool| (tool.name.clone(), tool.execution_mode))
            .collect();
        let mut cache = lock(&self.tools);
        if cache.signature != signature {
            cache.tools = Box::leak(current.into_boxed_slice());
            cache.signature = signature;
        }
        cache.tools
    }

    fn is_sequential(&self) -> bool {
        self.core
            .upgrade()
            .map(|core| {
                core.agent.tool_execution() == kiana_agent_core::ToolExecutionMode::Sequential
            })
            .unwrap_or(false)
    }

    fn run_tool_call<'a, F>(
        &'a self,
        tool_call: AgentToolCall,
        parent_tool_call_id: &'a str,
        signal: Option<&'a kiana_ai::utils::abort::AbortScope>,
        on_update: F,
    ) -> LocalFuture<'a, AgentToolCallOutcome>
    where
        F: Fn(kiana_agent_core::AgentToolResult<serde_json::Value>) -> LocalFuture<'a, ()>
            + Send
            + 'a,
    {
        let core = self.core.upgrade();
        let parent = parent_tool_call_id.to_string();
        let signal = signal.map(|signal| signal.token());
        Box::pin(async move {
            let Some(core) = core else {
                return nested_no_assistant_outcome(tool_call);
            };
            let Some(assistant_message) = core.find_last_assistant_message() else {
                return nested_no_assistant_outcome(tool_call);
            };
            let state = core.agent.state();
            // 工具按调用时解析（TS 的 `getTools` 是活回调），不依赖 runner 创建时的快照。
            let tools = core.get_callable_tools();
            let context = AgentContext {
                messages: state.messages,
                tools: Some(state.tools),
            };

            // before/after 钩子与模型发起的调用共享同一实现（带 `parentToolCallId`）。
            let before_core = self.core.clone();
            let before_parent = parent.clone();
            let before_tool_call: Option<kiana_agent_core::types::BeforeToolCallFn> =
                Some(Arc::new(move |context, _signal| {
                    let core = before_core.clone();
                    let parent = before_parent.clone();
                    Box::pin(async move {
                        dispatch_local(async move {
                            match core.upgrade() {
                                Some(core) => core.before_tool_call(context, Some(parent)).await,
                                None => None,
                            }
                        })
                        .await
                        .flatten()
                    })
                }));
            let after_core = self.core.clone();
            let after_parent = parent.clone();
            let after_tool_call: Option<kiana_agent_core::types::AfterToolCallFn> =
                Some(Arc::new(move |context, _signal| {
                    let core = after_core.clone();
                    let parent = after_parent.clone();
                    Box::pin(async move {
                        dispatch_local(async move {
                            match core.upgrade() {
                                Some(core) => core.after_tool_call(context, Some(parent)).await,
                                None => None,
                            }
                        })
                        .await
                        .flatten()
                    })
                }));

            // `on_update` 是带 `'a` 生命周期的局部 future 工厂，而 `ToolUpdateSink` 要求
            // `'static + Send + Sync`。sink 由本次 `run_tool_call` 独占，只在其 poll 期间被
            // 调用（调用栈仍在 `on_update` 的借用期内），因此用裸指针跨过类型系统的 `'a`
            // 是安全的；本地 future 用同线程 LocalSet 桥同步驱动。
            struct UpdatePtr<F>(usize, std::marker::PhantomData<fn() -> F>);
            impl<F> Copy for UpdatePtr<F> {}
            impl<F> Clone for UpdatePtr<F> {
                fn clone(&self) -> Self {
                    *self
                }
            }
            let on_update_ptr =
                UpdatePtr::<F>(&on_update as *const F as usize, std::marker::PhantomData);
            let on_update_sink: kiana_agent_core::agent_loop::ToolUpdateSink =
                Arc::new(move |partial| {
                    let address = on_update_ptr.0;
                    Box::pin(async move {
                        // SAFETY: 见上；run_tool_call 在 `on_update` 仍存活时调用 sink。
                        let on_update = unsafe { &*(address as *const F) };
                        let _ = run_local_blocking(on_update(partial));
                    })
                });

            run_tool_call(
                tool_call,
                RunToolCallOptions {
                    tools,
                    assistant_message,
                    context,
                    before_tool_call,
                    after_tool_call,
                    signal,
                    on_update: Some(on_update_sink),
                },
            )
            .await
        })
    }

    fn emit<'a>(&'a self, event: NestedToolExecutionEvent) -> LocalFuture<'a, ()> {
        let core = self.core.clone();
        Box::pin(async move {
            // 扩展派发需要同线程 LocalSet（`runner.emit` 是 !Send），但 trait 的 future
            // 带 `Send` 约束：用同步桥把 !Send 部分留在当前线程。
            dispatch_local(async move {
                if let Some(core) = core.upgrade() {
                    emit_nested_tool_event(&core, event).await;
                }
            })
            .await;
        })
    }
}

/// 会话核心状态。所有字段都是 `Send + Sync`，因此 [`Agent`] 钩子用 `Weak<Core>` 引用它。
///
/// 字段按功能分组（运行状态原子量 / 队列与待写消息 / 取消令牌 / 消息身份与替换 /
/// 工具表），每组前的注释说明该组的共同约束；单个字段的语义见字段文档。锁的约定：
/// 所有 `Mutex` 临界区都是短同步段，绝不在 `.await` 期间持有（见文件级文档）。
pub(crate) struct Core {
    agent: Agent,
    session_manager: Mutex<SessionManager>,
    /// `newSession({ setup })` 期间的会话管理器覆盖。
    ///
    /// setup 拿到的是 `Arc<Mutex<SessionManager>>`，而 `Core` 持有的是内嵌
    /// `Mutex<SessionManager>`。为了让 `kiana.setSessionName()` 等 Core 路由的 API 与
    /// setup 的直写落到同一份状态，setup 期间把该槽位指向共享管理器；所有
    /// `with_session*` 先检查它。
    session_override: Mutex<Option<Arc<Mutex<SessionManager>>>>,
    snapshot: Arc<SessionSnapshot>,
    settings_manager: SettingsManager,
    model_runtime: ModelRuntime,
    cache_warmer: Option<CacheWarmer>,

    cwd: String,
    scoped_models: Mutex<Vec<ScopedModel>>,
    initial_active_tool_names: Option<Vec<String>>,
    uses_default_tools: bool,
    default_tool_modifiers: Vec<String>,
    allowed_tools: Option<ToolNameMatcher>,
    allowlist_filters_mcp: bool,
    excluded_tools: Option<ToolNameMatcher>,
    base_tools_override: Option<IndexMap<String, AgentTool>>,
    session_start_event: SessionStartEvent,
    extension_runner_ref: Option<Arc<Mutex<Option<Arc<ExtensionRunner>>>>>,
    custom_tools: Vec<ToolDefinition>,

    extension_runner: Mutex<Option<Arc<ExtensionRunner>>>,
    extension_ui_context: Mutex<Option<Arc<dyn ExtensionUiContext>>>,
    /// 权限引擎。`None` 表示未启用（默认；设置 `KIANA_PERMISSIONS` 后在构造时启用）。
    permissions: Mutex<Option<Arc<PermissionEngine>>>,
    /// 第 10 周持久记忆运行时；默认关闭，`AgentSessionConfig::memory`/`KIANA_MEMORY=1`
    /// 显式启用。load-time 快照是 system prompt `memory` section 的唯一来源。
    memory: Option<Arc<SessionMemory>>,
    /// 模型回答语言（system prompt `output_language` section）；`None` 不追加规则。
    output_language: Option<String>,
    /// 第 11 周后半的连续性运行时；默认关闭。
    continuity: Option<Arc<ContinuityRuntime>>,
    /// 本会话准备子代理时见过的自定义隔离状态目录。
    ///
    /// `IsolationOptions.state_dir` 是逐次调用参数，启动 sweep 必须知道它才能回收
    /// 对应目录；这里记录本进程见过的目录，另有 Work 账本/子会话 header 推导出的
    /// 目录由调用方传入。
    isolation_state_dirs: Mutex<std::collections::BTreeSet<std::path::PathBuf>>,
    /// 启动阶段 `recover_work` 的结果（`sdk` 启动时写入）。
    ///
    /// `None` 表示本会话没有 durable Work 账本或尚未执行恢复；`Some(Ok(report))`
    /// 是成功（空报告表示没有遗留 Work）；`Some(Err(_))` 是恢复失败。宿主据此
    /// 区分“无可恢复”和“恢复失败”。
    work_recovery: Mutex<Option<Result<Option<crate::subagent::WorkRecoveryReport>, String>>>,
    /// 会话打开时的恢复计划（只读分类；`InterruptedTurn` 已在构造前应用 canonical 修复）。
    initial_recovery: Mutex<Option<RecoveryPlan>>,
    /// 尚未消费的恢复 continuation（宿主调用 `take_recovery_continuation` 取走）。
    pending_recovery: Mutex<Option<RecoveryContinuation>>,
    /// 正在重放的 InterruptedPrompt 尾部 user（entry id + 消息键 + 原消息）。
    ///
    /// `prepare_run` 用它代替新构造的用户消息；`MessageEnd` 用它跳过重复写
    /// canonical user entry，但保留消息键到原 entry id 的映射。
    replay_seed: Mutex<Option<RecoveryReplaySeed>>,
    /// 执行所有权拒绝原因；`Some` 时本进程不得启动模型 run。
    execution_refusal: Option<String>,
    /// 每个 AgentStart 递增；用于 checkpoint 的 turn 去重键（跨 run 不重复）。
    run_sequence: AtomicU64,
    /// 由召回注入的瞬时 user 消息正文。它们在 `MessageEnd` 时被识别并跳过持久化，
    /// 对外事件也不展示（`<memory-context>` 是参考数据，不是用户真实输入）。
    pending_memory_contexts: Mutex<Vec<String>>,
    /// settings/env 是否显式关闭了权限检查（`permissions.off`）。
    permissions_disabled: bool,
    /// `--add-dir` 追加的可写根。即使权限层关闭也保留：它同时进入沙箱策略。
    additional_writable_roots: Mutex<Vec<PathBuf>>,
    /// Hook 执行器；配置解析失败时为 `None`（hook 初始化失败不阻断会话启动）。
    hook_runner: Mutex<Option<Arc<HookRunner>>>,
    /// 本回合共享的 hook 状态（deny 上限、`KIANA_HOOK_DEPTH` 继承）。
    ///
    /// 同一回合的所有事件必须复用同一个状态；每轮新建会让 deny 上限失效。
    /// 锁只做短同步拷贝，绝不在 hook `.await` 期间持有。
    hook_turn: Mutex<HookTurnState>,
    /// 串行化 hook 事件对 `hook_turn` 的「读快照 → await → 写回」。
    ///
    /// 并行工具批次会同时触发多次 PreToolUse；若各自读快照再写回，`deny_count`
    /// 的增量会互相覆盖，per-turn deny 上限形同虚设。
    hook_turn_lock: tokio::sync::Mutex<()>,
    /// Hook 审计环形缓冲（有界、非阻塞）。
    hook_audit: Arc<SessionHookAudit>,
    /// 会话级读记录表：read 写入，write/edit 的 read-before-write 门禁读取。
    ///
    /// 每个会话独立一份，避免单进程多会话互相授权（`ReadStateRegistry::global()` 是
    /// 进程级共享的缺省值，只适合没有会话隔离需求的调用方）。
    read_state: Arc<ReadStateRegistry>,
    /// 会话级执行策略：权限模式（read-only/bypass）与 addDirs/workspaceRoots 的投影。
    ///
    /// 与 `write`/`edit` 的门禁和 bash 的 `SandboxManager` 共用同一份实例；权限模式或
    /// 目录在运行期变化时重建（见 [`Core::refresh_execution_policy`]）。
    execution_policy: Mutex<Arc<ExecutionPolicy>>,
    /// 与 [`Core::execution_policy`] 对应的沙箱 manager；策略不需要隔离时为 `None`。
    sandbox: Mutex<Option<Arc<SandboxManager>>>,
    /// 配置层（System/Managed/Enterprise/User/Project/Local）合并后的整体指纹。
    config_fingerprint: String,
    /// 配置层逐 key path 的胜出来源（“这个值从哪来”）。
    config_origins: std::collections::BTreeMap<String, ConfigLayerSource>,
    /// 配置层加载/严格诊断（启动诊断用）。
    config_diagnostics: Vec<String>,
    extension_mode: Mutex<ExtensionMode>,
    extension_command_context_actions: Mutex<Option<ExtensionCommandContextActions>>,
    extension_abort_handler: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    extension_shutdown_handler: Mutex<Option<ShutdownHandler>>,
    extension_error_listener: Mutex<Option<ExtensionErrorListener>>,
    extension_error_unsubscriber: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,

    event_listeners: Mutex<Vec<(u64, AgentSessionEventListener)>>,
    next_listener_id: AtomicU64,
    subscription: Mutex<Option<Subscription>>,

    // -- 运行状态标志 ------------------------------------------------------
    //
    // 以下原子量只表达布尔/计数，不保护其它数据；它们用 `SeqCst` 读写，跨线程可见。
    /// 是否有 run（由 `prompt` / `continue_` 驱动）在执行；`is_streaming` 直接读它。
    is_agent_run_active: AtomicBool,
    /// run 期间收到的中止请求；`handle_agent_event` 在 `AgentStart` 时据此调用
    /// `agent.abort()`，`start_run` 的循环检查点也会直接读取。
    agent_run_abort_requested: AtomicBool,
    /// `agent_before_settle` 边界派发中。此时收到的 abort 不能立刻打断边界，
    /// 先记到下一个标志，等边界返回后再让循环退出。
    is_before_settle: AtomicBool,
    /// 边界派发期间收到的 abort，边界结束后由 `start_run` 消费。
    abort_during_before_settle: AtomicBool,
    /// 正在派发 `agent_settled`。此期间新到的 `prompt`/custom 消息会被延后执行，
    /// 避免在 settled 流程内部重入 run。
    is_emitting_agent_settled: AtomicBool,
    /// 上下文溢出/截断恢复是否已尝试过；用于把 compact-and-retry 限制为一次。
    overflow_recovery_attempted: AtomicBool,
    /// 当前自动重试次数（0 表示不在重试中），与 `retry_abort` 配合。
    retry_attempt: AtomicU64,
    /// 当前 run 内的 turn 序号，扩展的 `turn_start`/`turn_end` 事件使用。
    turn_index: AtomicU64,

    // -- 队列与待写消息 ----------------------------------------------------
    /// idle 等待者；会话重新空闲时全部唤醒（对应 TS 的同一个 `_idleWaitPromise`）。
    idle_wait: Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
    /// 排队中的 steering / follow-up **文本**。真正的消息队列在 `Agent` 内部；这两份
    /// 只用于 `queue_update` 事件、`clearQueue` 的返回值与 `pendingMessageCount`；
    /// 扩展上下文的 `has_pending_messages`（`hasPendingMessages()`）也读它们判断是否有
    /// 排队消息。
    steering_messages: Mutex<Vec<String>>,
    follow_up_messages: Mutex<Vec<String>>,
    /// `sendCustomMessage(..., deliverAs: nextTurn)` 排入的下一条消息，`prepare_run`
    /// 时并入本轮输入。
    pending_next_turn_messages: Mutex<Vec<CustomMessage>>,
    /// run 中产生、不触发 turn 的 custom 消息；在 turn 结束或 run 的 flush 点写入。
    pending_custom_messages: Mutex<Vec<CustomMessage>>,
    /// 运行中执行的 bash 结果；run 结束后由 flush 写入会话。
    pending_bash_messages: Mutex<Vec<BashExecutionMessage>>,

    // -- 取消令牌 ----------------------------------------------------------
    //
    // 这些槽位“有值”同时表示对应操作正在进行（`is_compacting` 就依赖这一点）。
    /// 手动 `compact()` 的取消令牌。
    compaction_abort: Mutex<Option<CancellationToken>>,
    /// 自动压缩的取消令牌。用 `Arc` 是为了在 finally 里按指针比较，只清除本次压缩
    /// 自己登记的令牌，不误清并发/后续的另一次压缩。
    auto_compaction_abort: Mutex<Option<Arc<CancellationToken>>>,
    /// 分支摘要（`navigateTree`）的取消令牌。
    branch_summary_abort: Mutex<Option<CancellationToken>>,
    /// 自动重试退避期间的取消令牌；`is_retrying` 读它是否存在。
    retry_abort: Mutex<Option<CancellationToken>>,
    /// 每个执行中的 bash 的 (id, 令牌)；`abortBash`/`dispose` 取消全部。
    bash_abort: Mutex<Vec<(u64, CancellationToken)>>,
    next_bash_id: AtomicU64,
    /// 上一次失败的 assistant 消息，交给下一次 `prepare_request` 作为路由/重试输入。
    failed_response: Mutex<Option<AssistantMessage>>,

    // -- 消息身份与替换 ----------------------------------------------------
    //
    // TS 的 `message_end` 替换是原地改对象；Rust 的消息是值，因此用下面这些表把
    // “消息内容键 → 条目/替换值”的映射补回来。
    /// 消息内容键 → 会话条目 id；`refresh_finalized_context` 会整体重建。
    entry_ids_by_message: Mutex<IndexMap<String, String>>,
    /// 已派发过 `turn_end` 边界的消息键，防止 `finish_turn` 钩子与事件监听重复派发。
    boundary_dispatched_messages: Mutex<HashSet<String>>,
    /// run 内扩展 `message_end` 返回的替换消息（键为替换前消息的内容键）。
    message_replacements: Mutex<IndexMap<String, AgentMessage>>,
    /// 最近一条 assistant 消息：`message_end` 持久化时写入，`handle_post_agent_run`
    /// 取走并据此决定重试/压缩。只消费一次。
    last_assistant_message: Mutex<Option<AssistantMessage>>,
    /// 最近一个 turn 的结束原因（completed/error/aborted），供边界事件使用。
    last_activity_outcome: Mutex<AgentActivityOutcome>,
    /// `agent_settled` 派发期间排队的 prompt/custom 动作，派发结束后依次执行。
    deferred_settled_actions: Mutex<Vec<DeferredSettledAction>>,

    // -- 工具表 ------------------------------------------------------------
    //
    // 注意这里有三个不同的表：
    // - `base_tool_definitions`：内建（或 override）工具的原始定义；
    // - `tool_registry`：包装后的 `AgentTool`（内建 + 扩展），`Agent` 实际调用的对象；
    // - `tool_definitions`：所有允许的工具定义（内建 + SDK + 扩展），带来源信息。
    /// 内建（或 base override）工具的原始定义；`build_runtime` 重建。
    ///
    /// 键仍是 8 个内建 `ToolName`，但 `edit` 槽的 `definition.name` 可能按模型路由为
    /// `apply_patch`（见 week 7）。需要模型可见名字时读 `definition.name`。
    base_tool_definitions: Mutex<IndexMap<String, ToolDefinition>>,
    /// `edit` 槽当前实际的模型可见工具名（`edit` 或 `apply_patch`）。
    ///
    /// `base_tool_definitions` 的键不随模型路由变化，因此激活名单/允许列表必须借助
    /// 这个值把配置里的 `edit` 映射到实际名字，反之亦然。
    edit_tool_name: Mutex<String>,
    /// 当前可调用的 `AgentTool`（内建 + 扩展，已包装），按名字索引。
    tool_registry: Mutex<IndexMap<String, AgentTool>>,
    /// 来自扩展/SDK 的工具名（可能覆盖内建名）。权限层据此判定「不能凭名字自动放行」。
    custom_tool_names: Mutex<HashSet<String>>,
    /// 扩展在派发期间调用 `refreshTools()` 时置位；真正的刷新推迟到安全的读取点，
    /// 避免重入 `kiana-extensions` 正在可变借用的扩展表（regression #5109）。
    pending_tool_refresh: AtomicBool,
    pending_tool_names: Mutex<IndexSet<String>>,
    hidden_declarations: Mutex<HashSet<String>>,
    tool_definitions: Mutex<IndexMap<String, ToolDefinitionEntry>>,
    tool_prompt_snippets: Mutex<IndexMap<String, String>>,
    tool_prompt_guidelines: Mutex<IndexMap<String, Vec<String>>>,
    commands: Mutex<CommandSources>,
    /// 资源加载器的 thread-local 注册 id（`None` = 无加载器）。
    ///
    /// TS 在每次使用时读取 live `ResourceLoader`（getter）；Rust 的 loader 不是 `Send`，
    /// 不能存进 `Core`，因此保存注册 id，并在每次读取命令/技能/上下文文件时按 id 取回
    /// 加载器重新快照（宿主直接调用 `resourceLoader.reload()` 后也能看到新数据）。
    resource_loader_id: Option<usize>,

    /// 扩展后的技能定义（第 5 周）：从 live loader 的技能快照重新读取正文/frontmatter。
    ///
    /// 与 `commands.skills`（冻结契约的 `Skill` 记录）并存：`Skill` 只含索引字段，
    /// `SkillDefinition` 含正文、`allowed-tools`、`paths` 等前端/权限所需信息。
    skill_definitions: Mutex<Vec<SkillDefinition>>,
    /// 会话级 `skill` 工具与激活台账（`allowed-tools` 权限、TTL、撤销）。
    ///
    /// 懒构造：`refresh_tool_registry` 在建立工具表时创建；资源 reload 只更新定义。
    skill_tool: Mutex<Option<Arc<SkillTool>>>,
    /// 本会话已触达的路径（`paths` 技能可见性）。
    ///
    /// 权限请求构造时记录 `paths` 与 `read_paths`（bash 的虚拟读写路径也在其中）。
    touched_paths: Mutex<HashSet<PathBuf>>,
    /// 当前 plan / plan-mode 状态（第 6 周 pruning 保护与压缩恢复）。
    plan_state: Mutex<SessionPlanState>,

    /// 会话级 artifact 存储（工具输出落盘 / JSON outline / 聚合预算）。
    ///
    /// 惰性创建：内存会话（session dir 为空）没有可写目录时保持 `None`，预算层退化为
    /// middle-out 截断。
    artifact_store: OnceLock<Arc<ArtifactStore>>,
    /// 压缩门控：同一历史 no-op 缓存 + 5 分钟冷却（跨调用保存）。
    compaction_gate: Mutex<CompactionGate>,
    /// 压缩后待注入下一次 provider 请求的 hidden context（非 tool result）。
    pending_recovery_attachment: Mutex<Option<String>>,
    /// hook 的 `additionalContext`（`PreToolUse` / `SessionStart` / `PostCompact`），
    /// 待注入下一次 provider 请求；每个元素已 XML 转义并包好 `<hook-context>`。
    pending_hook_contexts: Mutex<Vec<String>>,
    /// 最近一次手动压缩的收敛观测（`compact()` 成功提交后写入）。
    last_condensation: Mutex<Option<CondensationEvent>>,
    /// 第 9 周：会话级 OTel 埋点（无 exporter 时为内存 context，仍可测试）。
    telemetry: Arc<SessionTelemetry>,
    /// 当前 run 的 collector（TUI/评测汇总）；每个 run 新建。
    run_collector: Mutex<Option<Arc<AgentRunCollector>>>,
    /// 当前 run / turn 的 span 句柄。
    agent_run_span: Mutex<Option<SessionSpan>>,
    turn_span: Mutex<Option<SessionSpan>>,
    turn_tracker: Mutex<Option<TurnTracker>>,

    base_system_prompt_options: Mutex<Option<NormalizedBuildSystemPromptOptions>>,
    run_system_prompt_options: Mutex<Option<NormalizedBuildSystemPromptOptions>>,

    nested_tool_calls: Mutex<Option<Arc<NestedToolCallRunner<SessionNestedHost>>>>,
    /// `message_start` 时为 toolResult 算好的替换消息（`nestedCalls` + 合并 usage），
    /// 等 `message_end` 把消息放进 agent state 后再应用；按 `tool_call_id` 索引。
    nested_result_replacements: Mutex<HashMap<String, ToolResultMessage>>,
    self_ref: Mutex<Weak<Core>>,
    registry_id: AtomicU64,
    /// `agent.streamFunction` 的「标准」形态：`AgentSession` 构造时安装的流函数包装。
    /// `_getSummarizationRequestAuth` 用它识别 `agent.streamFunction` 是否仍是我们安装的
    /// 那个包装（被宿主替换过时改看新函数本身）。
    standard_stream_function: StreamFn,
    /// 构造时 `agent.streamFunction` 是否是 [`stream_simple_stream_fn`] 的规范实例
    /// （TS 的 `agent.streamFunction === streamSimple`）。
    builtin_stream_function: bool,
    /// 虚拟模型路由失败时由 `prepareRequest` 写入、由流函数包装取出并让请求失败。
    pending_stream_error: Arc<Mutex<Option<kiana_ai::AiError>>>,
    /// 运行期会话持久化失败（TS 的 `SessionManager.append*` 直接抛出的 fs 异常）。
    ///
    /// TS 的 agent 监听器是 await 的，监听器抛错会 reject `prompt()`；Rust 的
    /// [`AgentListener`] 没有错误通道（冻结契约），因此在监听器里记录、由 `_startRun`
    /// 在 run 结束时 reject（见交付报告 deviations）。
    persistence_error: Mutex<Option<String>>,
}

/// 扩展注册失败时按 TS 的同步抛出语义中断动作。
///
/// `kiana-ext-api` 的 provider 动作签名返回 `()`（冻结契约）；`kiana-extensions` 的 runner 把
/// 注入的 Rust 闭包装进 `catch_item`，panic 的 payload 会经 `PROVIDER_ACTION_ERROR`
/// 变成扩展调用点的同步 JS 异常（见 `runner/mod.rs#bind_core`）。
fn throw_provider_error(error: impl std::fmt::Display) -> ! {
    panic!("{error}");
}

// ============================================================================
// 配置层与执行策略接线
// ============================================================================

/// 把一个作用域的 `Settings` 投影成配置层的 JSON 值。
///
/// `Settings` 的 `Serialize` 只写出显式配置过的键（`None` 与未出现的键不输出），
/// 因此可以直接作为一层参与合并。
fn settings_layer_value(settings: &kiana_config::settings::schema::Settings) -> JsonValue {
    sanitize_config_layer_value(serde_json::to_value(settings).unwrap_or(JsonValue::Null))
}

/// 把一层配置里的 `permissions` 规范成“`null` = 没有值”。
///
/// 旧 `merge_permission_settings` 明确忽略 `null`（`{"mode": null}` 不能关掉低层的
/// `read-only`）；分层合并默认让高层 `null` 覆盖低层，因此必须在合并**之前**逐层清理，
/// 否则 local 的 `{"mode": null}` 会把项目里的 mode 整个抹掉。
fn sanitize_config_layer_value(mut value: JsonValue) -> JsonValue {
    let JsonValue::Object(object) = &mut value else {
        return value;
    };
    match object.get_mut("permissions") {
        Some(JsonValue::Null) => {
            object.remove("permissions");
        }
        Some(JsonValue::Object(permissions)) => {
            permissions.retain(|_, value| !value.is_null());
        }
        _ => {}
    }
    value
}

/// 组装本会话的配置层。
///
/// - System / Managed / Enterprise 从标准位置读文件（`/etc/kiana/...`）；这是 managed
///   policy 的唯一来源，`SettingsManager` 不认识这些层。
/// - User / Project 使用 `SettingsManager` 已加载的值：文件后端与内存后端（测试、SDK）
///   因此走同一条路径；重复读盘会让内存 manager 的设置丢失，也会让测试依赖宿主
///   `~/.kiana/settings.json`。
/// - Local 是 `SettingsManager` 不读的 `<cwd>/.kiana/settings.local.json`；项目未受信时
///   跳过，避免项目通过 local 给自己授予 `bypass`。
fn load_session_config_layers(
    settings_manager: &SettingsManager,
    cwd: &str,
) -> (ConfigLayers, Vec<LayerLoadError>) {
    let mut layers = ConfigLayers::new();
    let mut errors = Vec::new();
    // managed policy 只认管理员层；逐层加载以便在合并前统一 `null` 语义。
    for source in ConfigLayerSource::ALL {
        if !source.is_policy_source() {
            continue;
        }
        let Some(path) = source.standard_path(Path::new(cwd)) else {
            continue;
        };
        match ConfigLayer::load_file(source, &path, LayerLoadMode::default_for(source)) {
            Ok(Some(mut layer)) => {
                layer.value = sanitize_config_layer_value(layer.value);
                layers.push(layer);
            }
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    layers.push(ConfigLayer::new(
        ConfigLayerSource::User,
        settings_layer_value(&settings_manager.get_global_settings()),
    ));
    if settings_manager.is_project_trusted() {
        layers.push(ConfigLayer::new(
            ConfigLayerSource::Project,
            settings_layer_value(&settings_manager.get_project_settings()),
        ));
        let local_path = kiana_config::settings::permissions::local_settings_path(Path::new(cwd));
        match ConfigLayer::load_file(
            ConfigLayerSource::Local,
            &local_path,
            LayerLoadMode::Lenient,
        ) {
            Ok(Some(mut layer)) => {
                layer.value = sanitize_config_layer_value(layer.value);
                layers.push(layer);
            }
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    (layers, errors)
}

/// 去掉 `permissions` 顶层的 `null`。
///
/// 分层合并里显式 `null` 会覆盖低层；旧的 `merge_permission_settings` 把 `null` 当作
/// “没有值”。这里保持旧语义（`{"mode": null}` 不应关掉项目里的 `read-only`），同时
/// 不改变深层结构（规则条目里的 `null` 仍由权限层自行判断）。
fn strip_null_permission_fields(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(mut object) => {
            object.retain(|_, value| !value.is_null());
            JsonValue::Object(object)
        }
        other => other,
    }
}

/// 配置层加载/严格诊断（启动诊断用）。
///
/// 与 [`Core::new`] 使用同一套层组装逻辑；供 `kiana-coding-agent` 在会话创建前把
/// managed 文件错误、未知字段等信息并入启动诊断。
pub fn session_config_diagnostics(settings_manager: &SettingsManager, cwd: &str) -> Vec<String> {
    let (layers, errors) = load_session_config_layers(settings_manager, cwd);
    let effective = layers.resolve();
    errors
        .iter()
        .map(ToString::to_string)
        .chain(effective.diagnostics().iter().map(ToString::to_string))
        .collect()
}

/// 权限模式 → 执行策略 + 沙箱 manager。
///
/// - `read-only`：显式 `SandboxPolicy::read_only()`，不依赖 `KIANA_SANDBOX_POLICY`；
/// - `bypass`：显式 `danger_full_access`（权限层已放行，沙箱不再额外限制）；
/// - 其它：从环境变量构造，并追加权限层的 `addDirs` / `workspaceRoots`（只在
///   `workspace-write` 下生效，`danger-full-access` 忽略）。
///
/// 环境变量非法时 `ExecutionPolicy::from_env` 会拒绝所有执行；对应的 manager 必须
/// 保持 fail-closed，不能因为 `requires_sandbox() == false` 就退回无沙箱路径。
fn execution_context_for(
    permissions: Option<&PermissionEngine>,
    cwd: &str,
    additional_writable_roots: &[PathBuf],
) -> (Arc<ExecutionPolicy>, Option<Arc<SandboxManager>>) {
    match permissions.map(PermissionEngine::mode) {
        Some(PermissionMode::ReadOnly) => {
            let policy = ExecutionPolicy::new(SandboxPolicy::read_only(), cwd);
            let sandbox = sandbox_manager_for(&policy, cwd);
            (Arc::new(policy), sandbox)
        }
        Some(PermissionMode::BypassPermissions) => (
            Arc::new(ExecutionPolicy::new(
                SandboxPolicy::danger_full_access(),
                cwd,
            )),
            None,
        ),
        _ => {
            let mut policy = ExecutionPolicy::from_env(cwd);
            let invalid_env = matches!(
                policy.resolve("bash", &ToolInput::Shell { command: "" }),
                SandboxDecision::Deny(_)
            );
            if let Some(engine) = permissions {
                policy = policy.with_additional_writable_roots(engine.policy().workspace_roots);
            }
            // 权限层关闭（None）时引擎没有 roots；`--add-dir` 仍必须进入沙箱策略。
            policy = policy.with_additional_writable_roots(additional_writable_roots.to_vec());
            let policy = Arc::new(policy);
            let sandbox = if invalid_env {
                // `SandboxManager::from_env` 对非法/非 UTF-8 的策略值返回 fail-closed
                // manager；保留它，让第一条命令以 BackendUnavailable 失败。
                SandboxManager::from_env(cwd).map(Arc::new)
            } else {
                sandbox_manager_for(&policy, cwd)
            };
            (policy, sandbox)
        }
    }
}

/// 策略需要隔离时构造会话级 `SandboxManager`；`DangerFullAccess` 返回 `None`。
///
/// `None` 让 bash / `execute_bash` 保持原有的直接 spawn 路径（没有 relay 开销），同时
/// 文件工具拿到的是同一份 `DangerFullAccess` 策略，两边结论一致。
fn sandbox_manager_for(policy: &ExecutionPolicy, cwd: &str) -> Option<Arc<SandboxManager>> {
    policy
        .policy()
        .requires_sandbox()
        .then(|| Arc::new(SandboxManager::new(policy.policy().clone(), cwd)))
}

/// 会话层 hook 审计 sink。
///
/// `HookAuditSink::record` 在 hook 执行路径上同步调用，因此这里只做有界内存追加，
/// 不做磁盘 IO / 日志格式化。记录里可能包含用户配置的 command 文本或 URL，只保留
/// `HookAuditRecord` 自带的字段，不额外记录 tool input。
struct SessionHookAudit {
    records: Mutex<VecDeque<HookAuditRecord>>,
}

/// 审计环形缓冲的容量；超出后丢弃最旧的记录。
const SESSION_HOOK_AUDIT_CAPACITY: usize = 256;

impl SessionHookAudit {
    fn new() -> Self {
        Self {
            records: Mutex::new(VecDeque::new()),
        }
    }

    fn records(&self) -> Vec<HookAuditRecord> {
        lock(&self.records).iter().cloned().collect()
    }
}

impl HookAuditSink for SessionHookAudit {
    fn record(&self, record: HookAuditRecord) {
        let mut records = lock(&self.records);
        if records.len() >= SESSION_HOOK_AUDIT_CAPACITY {
            records.pop_front();
        }
        records.push_back(record);
    }
}

/// 从分层配置构建 hook runner。
///
/// 逐层 `add_source` 保留 `local > project > user` 的来源优先级；不能从合并后的
/// 有效配置读 `hooks`，因为第 3 周的数组 concat 会丢失每条 hook 的来源。
///
/// `allowManagedHooksOnly` 为真时，`kiana-hooks` 目前没有 `Managed` 来源，无法表达
/// “只允许管理员 hook”；按 handoff §11 的最安全做法整体停用，并把原因写入诊断。
/// 项目未受信时 `load_session_config_layers` 已经跳过 Project/Local，这里仍把真实的
/// trust 结论传给 `add_source`，防止未来调用路径变化时静默加载。
fn build_hook_runner(
    layers: &ConfigLayers,
    effective: &MergedConfig,
    settings_manager: &SettingsManager,
    cwd: &str,
    audit: Arc<SessionHookAudit>,
) -> (Option<Arc<HookRunner>>, Vec<String>) {
    let mut diagnostics = Vec::new();
    if effective.policy().allow_managed_hooks_only {
        diagnostics.push(
            "allowManagedHooksOnly is enabled; user/project/local hooks were not loaded because kiana-hooks has no managed hook source yet"
                .to_string(),
        );
        return (None, diagnostics);
    }

    let trusted = settings_manager.is_project_trusted();
    let mut registry = HookRegistry::new();
    for layer in layers.layers() {
        let source = match layer.source {
            ConfigLayerSource::User => HookSource::User,
            ConfigLayerSource::Project => HookSource::Project,
            ConfigLayerSource::Local => HookSource::Local,
            // System/Managed/Enterprise 没有对应的 HookSource；见函数文档。
            _ => continue,
        };
        let hooks = layer.value.get("hooks").cloned().unwrap_or(JsonValue::Null);
        let load = HooksConfig::from_value(&hooks);
        diagnostics.extend(
            registry
                .add_source(source, load, trusted)
                .into_iter()
                .map(|diagnostic| diagnostic.message),
        );
    }

    let mut config = HookRunnerConfig::new(PathBuf::from(cwd));
    config.audit = audit;
    // HTTP hook 的 URL / 环境变量白名单来自合并后的有效设置。参考实现把两个键放在
    // 顶层；同时接受 `security.*` 下的同名键（部分发行版把它归到 security 命名空间）。
    config.http_policy = http_hook_policy_from_settings(effective.value());
    match HookRunner::new(registry, config) {
        Ok(runner) => (Some(Arc::new(runner)), diagnostics),
        Err(error) => {
            diagnostics.push(format!("failed to initialize hooks: {error}"));
            (None, diagnostics)
        }
    }
}

/// 从合并后的 settings JSON 读取 HTTP hook 白名单。
///
/// - `allowedHttpHookUrls`：glob 数组；缺省表示不限制，`[]` 表示全部拒绝（与
///   `allowedMcpServers` 的语义一致）。
/// - `httpHookAllowedEnvVars`：全局环境变量白名单，与每个 hook 自己的
///   `allowedEnvVars` 取交集。
///
/// 顶层键优先，其次 `security.*`。类型不符时忽略该键（settings 层的严格诊断负责
/// 报告），但 `allowedHttpHookUrls: []` 是有效的“全部拒绝”。
fn http_hook_policy_from_settings(settings: &JsonValue) -> HttpHookPolicy {
    let security = settings.get("security");
    let lookup = |name: &str| {
        settings
            .get(name)
            .or_else(|| security.and_then(|security| security.get(name)))
    };
    let string_list = |value: Option<&JsonValue>| {
        value.and_then(JsonValue::as_array).map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
    };
    let allowed_url_patterns = match string_list(lookup("allowedHttpHookUrls")) {
        // 键缺省：不限制（`HttpHookPolicy` 的既有契约：空列表 = 允许所有）。
        None => Vec::new(),
        // 显式 `[]`：拒绝所有。`HttpHookPolicy` 没有单独的“已配置”布尔，用一个不可能
        // 匹配任何 URL 的空模式表达 deny-all（参考实现同样把 `[]` 视为全部拒绝）。
        Some(patterns) if patterns.is_empty() => vec![String::new()],
        Some(patterns) => patterns,
    };
    let allowed_env_vars = string_list(lookup("httpHookAllowedEnvVars"));
    HttpHookPolicy {
        allowed_url_patterns,
        allowed_env_vars,
    }
}

/// settings 里的模型回答语言：顶层 `output_language` 优先，其次 `ui.outputLanguage`
/// / `ui.output_language`。
fn settings_language(settings_manager: &kiana_config::settings::SettingsManager) -> Option<String> {
    let settings = settings_manager.get_settings();
    let read = |value: Option<serde_json::Value>| {
        value
            .and_then(|value| value.as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
    };
    read(settings.raw("output_language")).or_else(|| {
        let ui = settings.raw("ui")?;
        read(ui.get("outputLanguage").cloned()).or_else(|| read(ui.get("output_language").cloned()))
    })
}

impl Core {
    fn new(config: AgentSessionConfig, resource_loader_id: usize) -> Self {
        let AgentSessionConfig {
            agent,
            mut session_manager,
            settings_manager,
            cwd,
            scoped_models,
            resource_loader: _,
            custom_tools,
            model_runtime,
            cache_warmer,
            initial_active_tool_names,
            uses_default_tools,
            default_tool_modifiers,
            allowed_tool_names,
            excluded_tool_names,
            base_tools_override,
            extension_runner_ref,
            session_start_event,
            memory: memory_options,
            output_language,
            continuity: continuity_options,
            continuity_disabled,
            telemetry: telemetry_override,
        } = config;
        let memory_options = memory_options.unwrap_or_else(MemoryWiringOptions::from_env);
        // 第 12 周：output language 与 UI locale 分离。显式配置 > 环境变量 > settings。
        let output_language = output_language
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("KIANA_OUTPUT_LANGUAGE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| settings_language(&settings_manager));
        // 第 11 周后半：恢复分类先于任何修复。只对 InterruptedTurn 写 canonical
        // （合成 failed tool result + turn 标记）；InterruptedPrompt/DegradedHistory
        // 只影响请求视图。分类结果保存在 Core 上供宿主读取 continuation。
        let initial_recovery = plan_manager_recovery(&session_manager);
        let recovery_diagnostic = match initial_recovery.classification.class {
            RecoveryClass::InterruptedTurn => match initial_recovery.apply(&mut session_manager) {
                Ok(_) => None,
                Err(error) => Some(format!("session recovery repair failed: {error}")),
            },
            _ => None,
        };
        let pending_recovery = initial_recovery.continuation();
        // plan / plan-mode 状态从会话里的 `plan-mode` 自定义条目恢复（最新的生效）。
        let plan_state = plan_state_from_entries(&session_manager.get_branch(None));
        // 连续性接线默认关闭；KIANA_CONTINUITY=1 时才创建状态目录与执行 claim。
        let continuity_options = if continuity_disabled {
            // settings 显式关闭：环境变量只是部署侧的额外开关，不能把它重新打开。
            None
        } else {
            match continuity_options {
                // settings 是生产开关；部署侧环境变量仍可单独关闭各能力（kill-switch）。
                Some(options) => Some(options.with_env_overrides()),
                None => ContinuityOptions::from_env(),
            }
        };
        let continuity = continuity_options.map(ContinuityRuntime::new);
        let execution_refusal = continuity.as_ref().and_then(|runtime| {
            runtime
                .begin_session(session_manager.get_session_id())
                .err()
        });
        let snapshot = Arc::new(SessionSnapshot::new(&session_manager));
        let (allowed_tools, allowlist_filters_mcp) = match allowed_tool_names {
            Some(names) => {
                let filters_mcp =
                    names.is_empty() || names.iter().any(|entry| entry.starts_with("mcp__"));
                let matcher = create_owned_tool_matcher(names.clone());
                (
                    Some(Arc::new(matcher) as Arc<dyn Fn(&str) -> bool + Send + Sync>),
                    filters_mcp,
                )
            }
            None => (None, false),
        };
        let excluded_tools = excluded_tool_names.map(|names| {
            let matcher = create_owned_tool_matcher(names.clone());
            Arc::new(matcher) as Arc<dyn Fn(&str) -> bool + Send + Sync>
        });

        // 虚拟模型路由失败没有钩子错误通道：在流函数入口注入一次失败，复刻 TS 里
        // `prepareRequest` reject 后 `handleRunFailure` 生成的错误 assistant 消息。
        let original_stream = agent.stream_function();
        let builtin_stream_function = Arc::ptr_eq(&original_stream, &stream_simple_stream_fn());
        let pending_stream_error: Arc<Mutex<Option<kiana_ai::AiError>>> =
            Arc::new(Mutex::new(None));
        let error_slot = Arc::clone(&pending_stream_error);
        let standard_stream_function: StreamFn = Arc::new(move |model, context, options| {
            let original = Arc::clone(&original_stream);
            let pending = lock(&error_slot).take();
            Box::pin(async move {
                if let Some(error) = pending {
                    return Err(error);
                }
                original(model, context, options).await
            })
        });
        agent.set_stream_function(Arc::clone(&standard_stream_function));

        // settings 的 `permissions` 对象启用权限引擎；命令行 flags 由调用方在会话创建后
        // 通过 `set_permission_*` 追加。两者都没有配置时回退到环境变量，行为与引入权限层
        // 之前一致。
        //
        // 有效值来自第 3 周的配置分层：
        // - System/Managed/Enterprise 从磁盘读，`ManagedPolicy` 在合并后强制生效；
        // - User/Project 使用 `SettingsManager` 已加载的值（内存后端与文件后端一致）；
        // - Local（`settings.local.json`）在项目受信时叠加，项目未受信时完全跳过
        //   （其中可能有 `mode: "bypass"`，未受信项目不能给自己授权）。
        //
        // 文件损坏不阻止会话启动：该层被跳过并记录诊断；真正的写回路径会单独报错。
        let (config_layers, layer_errors) = load_session_config_layers(&settings_manager, &cwd);
        let effective_config = config_layers.resolve();
        let permission_settings = effective_config
            .value()
            .get("permissions")
            .cloned()
            .map(strip_null_permission_fields);
        let permissions_disabled =
            PermissionEngine::configuration_disables(cwd.as_str(), permission_settings.as_ref());
        let permissions = PermissionEngine::from_settings_and_flags(
            cwd.as_str(),
            permission_settings.as_ref(),
            &PermissionFlags::default(),
        )
        .map(Arc::new);

        // 第 10 周：持久记忆。默认关闭；只有显式配置/环境变量才创建目录与 service。
        // 构造失败（目录不可写、快照损坏）不让会话启动失败：记忆是增强能力，不是
        // 主循环的依赖；失败通过诊断可见，下一次显式 `/memory` 操作会重新报错。
        let mut memory_diagnostic: Option<String> = None;
        let memory = match SessionMemory::new(
            &memory_options,
            &cwd,
            permissions.clone(),
            permissions_disabled,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                memory_diagnostic = Some(format!("memory disabled: {error}"));
                None
            }
        };
        let mut custom_tools = custom_tools;
        if let Some(memory) = &memory {
            custom_tools.push(crate::memory_wiring::memory_tool_definition(Arc::clone(
                memory,
            )));
        }

        // 执行策略与权限模式共用同一个投影：read-only/bypass 显式映射，其它模式由
        // `KIANA_SANDBOX_POLICY` 决定；`workspace-write` 下追加权限层的 addDirs。
        let (execution_policy, sandbox) = execution_context_for(permissions.as_deref(), &cwd, &[]);
        let config_fingerprint = effective_config.fingerprint().to_string();
        let config_origins = effective_config.origins().clone();
        let mut config_diagnostics: Vec<String> = layer_errors
            .iter()
            .map(ToString::to_string)
            .chain(
                effective_config
                    .diagnostics()
                    .iter()
                    .map(ToString::to_string),
            )
            .collect();
        if let Some(diagnostic) = memory_diagnostic {
            config_diagnostics.push(diagnostic);
        }
        if let Some(diagnostic) = recovery_diagnostic {
            config_diagnostics.push(diagnostic);
        }
        if let Some(refusal) = &execution_refusal {
            config_diagnostics.push(format!("execution ownership: {refusal}"));
        }

        // Hooks：逐层注册以保留 local > project > user 的来源优先级。初始化失败只记
        // 诊断，不阻断会话启动。
        let hook_audit = Arc::new(SessionHookAudit::new());
        let (hook_runner, hook_diagnostics) = build_hook_runner(
            &config_layers,
            &effective_config,
            &settings_manager,
            &cwd,
            Arc::clone(&hook_audit),
        );
        config_diagnostics.extend(hook_diagnostics);

        // 第 9 周：OTLP exporter 从环境变量构造；没有 endpoint 时遥测保持内存模式。
        // 宿主/测试可以通过 `AgentSessionConfig::telemetry` 注入实例。
        let telemetry = telemetry_override.unwrap_or_else(|| {
            let exporter = kiana_telemetry::OtlpExporter::from_env()
                .ok()
                .flatten()
                .map(Arc::new);
            Arc::new(match exporter {
                Some(exporter) => SessionTelemetry::new().with_exporter(exporter),
                None => SessionTelemetry::new(),
            })
        });

        Core {
            agent,
            session_manager: Mutex::new(session_manager),
            session_override: Mutex::new(None),
            snapshot,
            settings_manager,
            model_runtime,
            cache_warmer,
            cwd,
            scoped_models: Mutex::new(scoped_models),
            initial_active_tool_names,
            uses_default_tools: uses_default_tools.unwrap_or(false),
            default_tool_modifiers,
            allowed_tools,
            allowlist_filters_mcp,
            excluded_tools,
            base_tools_override,
            session_start_event: session_start_event.unwrap_or(SessionStartEvent {
                event_type: kiana_ext_api::events::SessionStartEventType::SessionStart,
                reason: kiana_ext_api::events::SessionStartReason::Startup,
                previous_session_file: None,
            }),
            extension_runner_ref,
            custom_tools,
            extension_runner: Mutex::new(None),
            extension_ui_context: Mutex::new(None),
            permissions: Mutex::new(permissions),
            permissions_disabled,
            additional_writable_roots: Mutex::new(Vec::new()),
            memory,
            output_language,
            continuity,
            isolation_state_dirs: Mutex::new(std::collections::BTreeSet::new()),
            work_recovery: Mutex::new(None),
            initial_recovery: Mutex::new(Some(initial_recovery)),
            pending_recovery: Mutex::new(pending_recovery),
            replay_seed: Mutex::new(None),
            execution_refusal,
            run_sequence: AtomicU64::new(1),
            pending_memory_contexts: Mutex::new(Vec::new()),
            hook_runner: Mutex::new(hook_runner),
            hook_turn: Mutex::new(HookTurnState::new()),
            hook_turn_lock: tokio::sync::Mutex::new(()),
            hook_audit,
            read_state: Arc::new(ReadStateRegistry::new()),
            execution_policy: Mutex::new(execution_policy),
            sandbox: Mutex::new(sandbox),
            config_fingerprint,
            config_origins,
            config_diagnostics,
            extension_mode: Mutex::new(ExtensionMode::Print),
            extension_command_context_actions: Mutex::new(None),
            extension_abort_handler: Mutex::new(None),
            extension_shutdown_handler: Mutex::new(None),
            extension_error_listener: Mutex::new(None),
            extension_error_unsubscriber: Mutex::new(None),
            event_listeners: Mutex::new(Vec::new()),
            next_listener_id: AtomicU64::new(1),
            subscription: Mutex::new(None),
            is_agent_run_active: AtomicBool::new(false),
            agent_run_abort_requested: AtomicBool::new(false),
            is_before_settle: AtomicBool::new(false),
            abort_during_before_settle: AtomicBool::new(false),
            is_emitting_agent_settled: AtomicBool::new(false),
            overflow_recovery_attempted: AtomicBool::new(false),
            retry_attempt: AtomicU64::new(0),
            turn_index: AtomicU64::new(0),
            idle_wait: Mutex::new(Vec::new()),
            steering_messages: Mutex::new(Vec::new()),
            follow_up_messages: Mutex::new(Vec::new()),
            pending_next_turn_messages: Mutex::new(Vec::new()),
            pending_custom_messages: Mutex::new(Vec::new()),
            pending_bash_messages: Mutex::new(Vec::new()),
            compaction_abort: Mutex::new(None),
            auto_compaction_abort: Mutex::new(None),
            branch_summary_abort: Mutex::new(None),
            retry_abort: Mutex::new(None),
            bash_abort: Mutex::new(Vec::new()),
            next_bash_id: AtomicU64::new(1),
            failed_response: Mutex::new(None),
            entry_ids_by_message: Mutex::new(IndexMap::new()),
            boundary_dispatched_messages: Mutex::new(HashSet::new()),
            message_replacements: Mutex::new(IndexMap::new()),
            last_assistant_message: Mutex::new(None),
            last_activity_outcome: Mutex::new(AgentActivityOutcome::Completed),
            deferred_settled_actions: Mutex::new(Vec::new()),
            base_tool_definitions: Mutex::new(IndexMap::new()),
            edit_tool_name: Mutex::new("edit".to_string()),
            tool_registry: Mutex::new(IndexMap::new()),
            custom_tool_names: Mutex::new(HashSet::new()),
            pending_tool_refresh: AtomicBool::new(false),
            pending_tool_names: Mutex::new(IndexSet::new()),
            hidden_declarations: Mutex::new(HashSet::new()),
            tool_definitions: Mutex::new(IndexMap::new()),
            tool_prompt_snippets: Mutex::new(IndexMap::new()),
            tool_prompt_guidelines: Mutex::new(IndexMap::new()),
            commands: Mutex::new(CommandSources::default()),
            resource_loader_id: Some(resource_loader_id),
            skill_definitions: Mutex::new(Vec::new()),
            skill_tool: Mutex::new(None),
            touched_paths: Mutex::new(HashSet::new()),
            plan_state: Mutex::new(plan_state),
            artifact_store: OnceLock::new(),
            compaction_gate: Mutex::new(CompactionGate::default()),
            pending_recovery_attachment: Mutex::new(None),
            pending_hook_contexts: Mutex::new(Vec::new()),
            last_condensation: Mutex::new(None),
            telemetry,
            run_collector: Mutex::new(None),
            agent_run_span: Mutex::new(None),
            turn_span: Mutex::new(None),
            turn_tracker: Mutex::new(None),
            base_system_prompt_options: Mutex::new(None),
            run_system_prompt_options: Mutex::new(None),
            nested_tool_calls: Mutex::new(None),
            nested_result_replacements: Mutex::new(HashMap::new()),
            self_ref: Mutex::new(Weak::new()),
            registry_id: AtomicU64::new(0),
            standard_stream_function,
            builtin_stream_function,
            pending_stream_error,
            persistence_error: Mutex::new(None),
        }
    }

    /// 记录一次会话持久化失败（保留第一个错误，与 TS 的“异常立即中断”一致）。
    fn record_persistence_error(&self, error: impl std::fmt::Display) {
        let mut slot = lock(&self.persistence_error);
        if slot.is_none() {
            *slot = Some(error.to_string());
        }
    }

    fn take_persistence_error(&self) -> Option<String> {
        lock(&self.persistence_error).take()
    }

    /// 该消息是否是召回注入的 `<memory-context>` 参考消息。
    fn is_pending_memory_context(&self, message: &AgentMessage) -> bool {
        if message.role() != "user" {
            return false;
        }
        let text = agent_message_text(message);
        lock(&self.pending_memory_contexts)
            .iter()
            .any(|content| content == &text)
    }

    /// 取出（消费）一条待注入消息；只用于 MessageEnd 的持久化分支。
    fn take_pending_memory_context(&self, message: &AgentMessage) -> bool {
        if message.role() != "user" {
            return false;
        }
        let text = agent_message_text(message);
        let mut pending = lock(&self.pending_memory_contexts);
        match pending.iter().position(|content| content == &text) {
            Some(index) => {
                pending.remove(index);
                true
            }
            None => false,
        }
    }

    // -- 会话访问 -----------------------------------------------------------

    fn with_session_mut<R>(&self, f: impl FnOnce(&mut SessionManager) -> R) -> R {
        // setup 进行中时，Core 路由的读写必须与 setup 持有的 `Arc<Mutex<...>>` 同步。
        if let Some(shared) = lock(&self.session_override).clone() {
            let mut guard = lock(&shared);
            return f(&mut guard);
        }
        let mut guard = lock(&self.session_manager);
        f(&mut guard)
    }

    fn with_session<R>(&self, f: impl FnOnce(&SessionManager) -> R) -> R {
        if let Some(shared) = lock(&self.session_override).clone() {
            let guard = lock(&shared);
            return f(&guard);
        }
        let guard = lock(&self.session_manager);
        f(&guard)
    }

    fn projection(&self) -> SessionProjection {
        self.with_session(SessionManager::build_session_projection)
    }

    /// `newEntries.find((e) => e.type === "compaction" && e.summary === summary)`。
    ///
    /// TS 的 `session_compact` 事件用**会话里第一条**摘要文本相同的压缩条目（旧条目也有
    /// 相同摘要时不会换成刚追加的那条）。
    fn first_compaction_entry_by_summary(&self, summary: &str) -> Option<CompactionEntry> {
        self.with_session(|manager| {
            manager
                .get_entries()
                .into_iter()
                .find_map(|entry| match entry {
                    SessionEntry::Compaction(entry) if entry.summary == summary => Some(entry),
                    _ => None,
                })
        })
    }

    fn branch(&self) -> Vec<SessionEntry> {
        self.with_session(|manager| manager.get_branch(None))
    }

    fn session_id(&self) -> String {
        self.with_session(|manager| manager.get_session_id().to_string())
    }

    fn session_file(&self) -> Option<String> {
        self.with_session(|manager| manager.get_session_file().map(str::to_string))
    }

    fn session_name(&self) -> Option<String> {
        self.with_session(SessionManager::get_session_name)
    }

    fn session_cwd(&self) -> String {
        self.with_session(|manager| manager.get_cwd().to_string())
    }

    fn runner(&self) -> Option<Arc<ExtensionRunner>> {
        lock(&self.extension_runner).clone()
    }

    fn runner_required(&self) -> Arc<ExtensionRunner> {
        self.runner().expect("extension runner is installed")
    }

    fn emit(&self, event: AgentSessionEvent) {
        let listeners = lock(&self.event_listeners).clone();
        for (_id, listener) in listeners {
            listener(&event);
        }
    }

    fn emit_queue_update(&self) {
        let steering = lock(&self.steering_messages).clone();
        let follow_up = lock(&self.follow_up_messages).clone();
        self.emit(AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        });
    }

    // -- 安装 Agent 钩子 ----------------------------------------------------

    /// `_installAgentHooks`：把会话层逻辑挂到 [`Agent`] 的各个钩子上（工具调用前后、
    /// `finishTurn`、`prepareNextTurnWithContext`、`prepareRequest`、两层
    /// `transformContext`）并订阅 agent 事件。钩子闭包只捕获注册 id（usize），以及需要
    /// 链式调用的前一层钩子（`previous_finish_turn`/`previous_prepare_next`/
    /// `previous_prepare_request`/`previous_transform`，用于会话释放或钩子链式转发）；
    /// 运行时经 `CORE_REGISTRY` 升级回 `Arc<Core>`，再经 [`dispatch_local`] 把 `!Send` 的
    /// 扩展派发桥回本线程；会话已释放时钩子退化为只调前一层钩子。
    fn install_agent_hooks(self: &Arc<Self>) {
        self.snapshot.set_owner(self);
        let weak = Arc::downgrade(self);
        *lock(&self.self_ref) = weak.clone();
        let session_id = register_core(self);
        self.registry_id.store(session_id as u64, Ordering::SeqCst);

        // 工具调用 / 结果钩子（扩展派发经 `spawn_local_task` 桥回当前 LocalSet）。
        let before_id = session_id;
        self.agent
            .set_before_tool_call(Some(Arc::new(move |context, _signal| {
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(before_id) {
                            Some(core) => core.before_tool_call(context, None).await,
                            None => None,
                        }
                    })
                    .await
                    .flatten()
                })
            })));
        let after_id = session_id;
        self.agent
            .set_after_tool_call(Some(Arc::new(move |context, _signal| {
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(after_id) {
                            Some(core) => core.after_tool_call(context, None).await,
                            None => None,
                        }
                    })
                    .await
                    .flatten()
                })
            })));

        // finishTurn：turn_end 边界。
        let previous_finish_turn = self.agent.finish_turn();
        let finish_id = session_id;
        self.agent
            .set_finish_turn(Some(Arc::new(move |turn, signal| {
                let previous = previous_finish_turn.clone();
                Box::pin(async move {
                    let previous_snapshot = previous.clone();
                    dispatch_local(async move {
                        let Some(core) = lookup_core(finish_id) else {
                            return match &previous_snapshot {
                                Some(previous) => previous(turn, signal).await,
                                None => None,
                            };
                        };
                        // `message_end` 扩展替换后的 assistant/toolResult 才是 TS 里被原地修改、
                        // 也被持久化的那个对象。
                        let effective_assistant = match core.effective_message(
                            &AgentMessage::Standard(Message::Assistant(turn.message.clone())),
                        ) {
                            AgentMessage::Standard(Message::Assistant(assistant)) => assistant,
                            _ => turn.message.clone(),
                        };
                        let effective_tool_results: Vec<ToolResultMessage> = turn
                            .tool_results
                            .iter()
                            .map(|result| {
                                match core.effective_message(&AgentMessage::Standard(
                                    Message::ToolResult(result.clone()),
                                )) {
                                    AgentMessage::Standard(Message::ToolResult(updated)) => updated,
                                    _ => result.clone(),
                                }
                            })
                            .collect();
                        lock(&core.boundary_dispatched_messages).insert(message_key(
                            &AgentMessage::Standard(Message::Assistant(
                                effective_assistant.clone(),
                            )),
                        ));
                        let extension_continue = core
                            .dispatch_turn_end_boundary(
                                &effective_assistant,
                                &effective_tool_results,
                            )
                            .await;
                        let previous_decision = match &previous_snapshot {
                            Some(previous) => previous(turn, signal).await,
                            None => None,
                        };
                        if previous_decision
                            == Some(kiana_agent_core::types::AgentTurnDecision::End)
                        {
                            return previous_decision;
                        }
                        if extension_continue
                            || previous_decision
                                == Some(kiana_agent_core::types::AgentTurnDecision::Continue)
                        {
                            return Some(kiana_agent_core::types::AgentTurnDecision::Continue);
                        }
                        None
                    })
                    .await
                    .flatten()
                })
            })));

        // prepareNextTurnWithContext：compaction + loadout 刷新。
        let previous_prepare_next = self.agent.prepare_next_turn_with_context().or_else(|| {
            self.agent.prepare_next_turn().map(|previous| {
                Arc::new(
                    move |_turn: PrepareNextTurnContext, signal: Option<CancellationToken>| {
                        let previous = previous.clone();
                        Box::pin(async move {
                            previous(signal).await.map(|update| AgentLoopTurnUpdate {
                                context: update.context,
                                messages: update.messages,
                                model: update.model,
                                thinking_level: update.thinking_level,
                            })
                        })
                            as kiana_agent_core::types::BoxFuture<
                                'static,
                                Option<AgentLoopTurnUpdate>,
                            >
                    },
                ) as kiana_agent_core::agent::AgentPrepareNextTurnWithContextFn
            })
        });
        let next_id = session_id;
        self.agent
            .set_prepare_next_turn_with_context(Some(Arc::new(move |turn, signal| {
                let previous = previous_prepare_next.clone();
                Box::pin(async move {
                    let previous_snapshot = previous.clone();
                    dispatch_local(async move {
                        match lookup_core(next_id) {
                            Some(core) => {
                                core.prepare_next_turn(turn, signal, previous_snapshot)
                                    .await
                            }
                            None => match &previous_snapshot {
                                Some(previous) => previous(turn, signal).await,
                                None => None,
                            },
                        }
                    })
                    .await
                    .flatten()
                })
            })));

        // prepareRequest：canonical context + 虚拟模型路由 + 阈值压缩。
        let previous_prepare_request = self.agent.prepare_request();
        let request_id = session_id;
        self.agent
            .set_prepare_request(Some(Arc::new(move |request, signal| {
                let previous = previous_prepare_request.clone();
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(request_id) {
                            Some(core) => core.prepare_request(request, signal, previous).await,
                            None => None,
                        }
                    })
                    .await
                    .flatten()
                })
            })));

        // transformContext：隐藏声明投影。
        let previous_transform = self.agent.transform_context();
        let hidden_id = session_id;
        self.agent
            .set_transform_context(Some(Arc::new(move |messages, signal| {
                let previous = previous_transform.clone();
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(hidden_id) {
                            Some(core) => {
                                core.transform_hidden_declarations(messages, signal, previous)
                                    .await
                            }
                            None => messages,
                        }
                    })
                    .await
                    .unwrap_or_default()
                })
            })));

        // transformContext：强制系统提示投影（在隐藏声明投影之后）。
        let previous_transform = self.agent.transform_context();
        let forced_id = session_id;
        self.agent
            .set_transform_context(Some(Arc::new(move |messages, signal| {
                let previous = previous_transform.clone();
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(forced_id) {
                            Some(core) => {
                                core.transform_forced_prompt(messages, signal, previous)
                                    .await
                            }
                            None => messages,
                        }
                    })
                    .await
                    .unwrap_or_default()
                })
            })));

        // 订阅 agent 事件。
        let listener: AgentListener = Arc::new(move |event, _signal| {
            Box::pin(async move {
                dispatch_local(async move {
                    if let Some(core) = lookup_core(session_id) {
                        core.handle_agent_event(event).await;
                    }
                })
                .await;
            })
        });
        let subscription = self.agent.subscribe(listener);
        *lock(&self.subscription) = Some(subscription);
    }

    /// `Agent` 事件的总入口（`install_agent_hooks` 订阅的监听器）。按顺序做六件事：
    /// 1. 应用扩展 `message_end` 替换到 `agent_end`/`turn_end` 的快照消息（TS 是原地改对象）；
    /// 2. 处理 `AgentStart` 期间的待定 abort 与嵌套工具结果的 usage 合并；
    /// 3. 用户消息开始时把文本从 steering/follow-up 队列摘除并发 `queue_update`；
    /// 4. 先派发扩展事件（`message_end` 可能返回替换消息），再发公开事件；
    /// 5. 持久化 `message_end`（custom 或标准角色）——写失败时记录 `persistence_error`
    ///    并 abort 当前 run（监听器没有错误通道），本轮后续逻辑不再执行；
    /// 6. turn 结束时冲刷上下文型 custom 消息（`flush_pending_custom_messages`）。
    ///
    /// 第 9 周：把 AgentEvent 喂给 run collector，并按事件记录 OTel span/指标。
    ///
    /// 遥测失败只影响可观测性，不改变 agent 行为（全部忽略错误）。
    async fn observe_run_telemetry(&self, event: &AgentEvent) {
        if matches!(event, AgentEvent::AgentStart) {
            let collector = Arc::new(AgentRunCollector::new());
            collector
                .note_available_tools(self.agent.tools().iter().map(|tool| tool.name.as_str()));
            *lock(&self.run_collector) = Some(Arc::clone(&collector));
            if let Ok(span) = self.telemetry.begin_agent_run("kiana").await {
                *lock(&self.agent_run_span) = Some(span);
            }
        }
        if let Some(collector) = lock(&self.run_collector).clone() {
            // 第 9 周：summary 的 token/cost 与遥测 API call 同源。turn_end 时把
            // canonical 记账结果交给 collector，避免 provider 上报 cost=0 时两边不一致。
            let accounted_usage = match event {
                AgentEvent::TurnEnd {
                    message: AgentMessage::Standard(Message::Assistant(assistant)),
                    ..
                } => self
                    .model_for_message(assistant)
                    .map(|model| account_provider_usage(&model, &assistant.usage, None).usage),
                _ => None,
            };
            collector.observe_event_with_usage(event, accounted_usage.as_ref());
        }
        match event {
            AgentEvent::TurnStart => {
                *lock(&self.turn_tracker) = Some(TurnTracker::new());
                let turn_index = self.turn_index.load(Ordering::SeqCst);
                let model = self
                    .current_model()
                    .map(|model| model.id)
                    .unwrap_or_default();
                if let Ok(span) = self.telemetry.begin_turn(turn_index, &model).await {
                    *lock(&self.turn_span) = Some(span);
                }
            }
            AgentEvent::TurnEnd { message, .. } => {
                let tracker = lock(&self.turn_tracker).take();
                // 先取出 span，避免 `if let` 的临时 MutexGuard 活到 `finish().await`
                // 期间（同一 LocalSet 上的其它任务可能在 await 点阻塞拿锁）。
                let turn_span = lock(&self.turn_span).take();
                if let Some(span) = turn_span {
                    let mut attributes = kiana_telemetry::SpanAttributes::new();
                    if let Some(tracker) = &tracker {
                        attributes.insert(
                            "kiana.turn.duration_ms".into(),
                            kiana_telemetry::AttributeValue::Number(tracker.elapsed_ms()),
                        );
                    }
                    if let AgentMessage::Standard(Message::Assistant(assistant)) = message {
                        attributes.insert(
                            "kiana.turn.stop_reason".into(),
                            kiana_telemetry::AttributeValue::String(stop_reason_label(
                                assistant.stop_reason,
                            )),
                        );
                    }
                    span.span().set_attributes(attributes);
                    span.finish().await;
                }
                self.telemetry.end_turn();
            }
            AgentEvent::MessageEnd {
                message: AgentMessage::Standard(Message::Assistant(assistant)),
            } => {
                let is_error = assistant.stop_reason == StopReason::Error;
                // 第 9 周成功路径 usage 记账：canonical 价格可用时重算，否则保持
                // provider 上报值；`cost_source` 进入遥测属性。
                let (usage, cost_source) = match self.model_for_message(assistant) {
                    Some(model) => {
                        let accounted = account_provider_usage(&model, &assistant.usage, None);
                        (
                            accounted.usage,
                            Some(accounted.cost_source.as_str().to_string()),
                        )
                    }
                    None => (assistant.usage.clone(), None),
                };
                let _ = self
                    .telemetry
                    .record_api_call(&ApiCallRecord {
                        provider: assistant.provider.clone(),
                        model: assistant.model.clone(),
                        response_model: assistant.response_model.clone(),
                        duration_ms: assistant.duration_ms.unwrap_or(0.0),
                        ttft_ms: None,
                        ttfm_ms: None,
                        usage,
                        cost_source,
                        stop_reason: Some(stop_reason_label(assistant.stop_reason).to_string()),
                        error_type: is_error.then(|| {
                            assistant
                                .error_message
                                .clone()
                                .unwrap_or_else(|| "provider_error".to_string())
                        }),
                    })
                    .await;
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                is_error,
                duration_ms,
                ..
            } => {
                let _ = self
                    .telemetry
                    .record_tool_call(&ToolCallRecord {
                        tool_name: tool_name.clone(),
                        tool_call_id: tool_call_id.clone(),
                        status: if *is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Ok
                        },
                        duration_ms: duration_ms.unwrap_or(0.0),
                        error_type: is_error.then(|| "tool_error".to_string()),
                    })
                    .await;
            }
            AgentEvent::AgentEnd { .. } => {
                let agent_run_span = lock(&self.agent_run_span).take();
                if let Some(span) = agent_run_span {
                    span.finish().await;
                }
                self.telemetry.end_agent_run();
            }
            _ => {}
        }
    }

    async fn handle_agent_event(&self, event: AgentEvent) {
        // TS 里 `message_end` 的替换是原地改对象，因此 `agent_end.messages` 也看到替换后的
        // 消息；Rust 的消息是值，这里用一张 run 内映射把替换应用到 agent_end 的快照。
        let mut event = match event {
            AgentEvent::AgentEnd { messages } => {
                let messages = self.apply_message_replacements(messages);
                AgentEvent::AgentEnd { messages }
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                // TS 的 `message_end` 替换是原地对象修改，`turn_end.message`/`toolResults` 因此
                // 也是替换后的值；Rust 用替换表映射回来。
                let message = self.effective_message(&message);
                let tool_results: Vec<ToolResultMessage> = tool_results
                    .into_iter()
                    .map(|result| {
                        let effective = self.effective_message(&AgentMessage::Standard(
                            Message::ToolResult(result.clone()),
                        ));
                        match effective {
                            AgentMessage::Standard(Message::ToolResult(updated)) => updated,
                            _ => result,
                        }
                    })
                    .collect();
                AgentEvent::TurnEnd {
                    message,
                    tool_results,
                }
            }
            other => other,
        };
        // 第 9 周：collector / OTel 看到替换后的消息（与扩展和监听者一致）。
        self.observe_run_telemetry(&event).await;
        // run 启动期间请求的 abort 在这里生效（before_agent_start 期间）。
        if matches!(event, AgentEvent::AgentStart)
            && self.agent_run_abort_requested.load(Ordering::SeqCst)
        {
            self.agent.abort();
        }

        // 记录 ctx.executeTool() 的嵌套调用与用量。
        //
        // TS 在 `message_start` 里**原地**给同一条 toolResult 消息补上 `nestedCalls` 与
        // 合并后的 usage，agent-core 在 `message_end` 时推入状态的就是同一个对象。Rust 的
        // 消息是值，agent-core 只在 `message_end` 才把消息放进 state，所以这里分两步：
        // `message_start` 造出替换消息并缓存，事件本身也换成替换消息（扩展/监听者看到与
        // TS 相同的对象）；`message_end` 再把缓存应用到此刻已在 state 里的那条消息。
        if lock(&self.nested_tool_calls).is_some() {
            match &event {
                AgentEvent::MessageStart {
                    message: AgentMessage::Standard(Message::ToolResult(result)),
                } => {
                    let summary = lock(&self.nested_tool_calls)
                        .as_ref()
                        .and_then(|runner| runner.take_record(&result.tool_call_id));
                    if let Some(summary) = summary {
                        let mut updated = result.clone();
                        if let Some(calls) = summary.calls {
                            updated.nested_calls = Some(calls);
                        }
                        if let Some(usage) = summary.usage {
                            updated.usage = Some(match updated.usage {
                                Some(existing) => combine_usage(&existing, &usage),
                                None => usage,
                            });
                        }
                        lock(&self.nested_result_replacements)
                            .insert(result.tool_call_id.clone(), updated.clone());
                        event = AgentEvent::MessageStart {
                            message: AgentMessage::Standard(Message::ToolResult(updated)),
                        };
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    if let Some(runner) = lock(&self.nested_tool_calls).as_ref() {
                        runner.clear();
                    }
                    lock(&self.nested_result_replacements).clear();
                }
                _ => {}
            }
        }
        if let AgentEvent::MessageEnd { message } = &event
            && let AgentMessage::Standard(Message::ToolResult(result)) = message
            && let Some(updated) =
                lock(&self.nested_result_replacements).remove(&result.tool_call_id)
        {
            let replacement = AgentMessage::Standard(Message::ToolResult(updated.clone()));
            self.replace_agent_message(message, replacement);
            event = AgentEvent::MessageEnd {
                message: AgentMessage::Standard(Message::ToolResult(updated)),
            };
        }

        // 用户消息开始：先从队列里移除，再发事件（UI 看到更新后的队列）。
        if let AgentEvent::MessageStart { message } = &event
            && message.role() == "user"
        {
            self.overflow_recovery_attempted
                .store(false, Ordering::SeqCst);
            let message_text = agent_message_text(message);
            if !message_text.is_empty() {
                let removed = {
                    let mut steering = lock(&self.steering_messages);
                    match index_of(&steering, &message_text) {
                        Some(index) => {
                            remove_at(&mut steering, index);
                            true
                        }
                        None => false,
                    }
                };
                if removed {
                    self.emit_queue_update();
                } else {
                    let removed = {
                        let mut follow_up = lock(&self.follow_up_messages);
                        match index_of(&follow_up, &message_text) {
                            Some(index) => {
                                remove_at(&mut follow_up, index);
                                true
                            }
                            None => false,
                        }
                    };
                    if removed {
                        self.emit_queue_update();
                    }
                }
            }
        }

        // 先发给扩展，再通知公开监听者。
        let replaced_message = self.emit_extension_event(&event).await;
        if let (AgentEvent::MessageEnd { message }, Some(replacement)) = (&event, &replaced_message)
        {
            // TS `_replaceMessageInPlace`：后续请求、agent_end 与监听者都看到替换后的消息。
            self.replace_agent_message(message, replacement.clone());
            lock(&self.message_replacements).insert(message_key(message), replacement.clone());
        }
        if matches!(event, AgentEvent::AgentEnd { .. }) {
            lock(&self.message_replacements).clear();
        }
        let public_event = match &event {
            AgentEvent::AgentEnd { messages } => {
                // `<memory-context>` 是 per-turn 参考数据，不属于用户 transcript。
                let visible: Vec<AgentMessage> = messages
                    .iter()
                    .filter(|message| !self.is_pending_memory_context(message))
                    .cloned()
                    .collect();
                Some(AgentSessionEvent::AgentEnd {
                    messages: visible,
                    will_retry: self.will_retry_after_agent_end(messages),
                })
            }
            AgentEvent::MessageStart { message } | AgentEvent::MessageEnd { message }
                if self.is_pending_memory_context(message) =>
            {
                None
            }
            _ => match &replaced_message {
                Some(replacement) => session_event_from_agent_event_with_message(
                    &event,
                    ReplacementMessage::Replace(replacement.clone()),
                ),
                None => session_event_from_agent_event(&event),
            },
        };
        if let Some(public_event) = public_event {
            self.emit(public_event);
        }

        // 会话持久化。
        if let AgentEvent::MessageEnd { message } = &event {
            let final_message = replaced_message.clone().unwrap_or_else(|| message.clone());
            let mut entry_id: Option<String> = None;
            // 召回注入的 `<memory-context>` 只参与当前请求，不写会话文件。
            if !self.take_pending_memory_context(&final_message) {
                // InterruptedPrompt 的重放：原 user 消息已经是一条 canonical entry，这里
                // 只建立消息键到原 entry id 的映射，绝不 append 新 user entry。
                let replay_entry_id = if final_message.role() == "user" {
                    let key = message_key(&final_message);
                    let seed = lock(&self.replay_seed);
                    seed.as_ref()
                        .filter(|seed| seed.message_key == key)
                        .map(|seed| seed.entry_id.clone())
                } else {
                    None
                };
                if let Some(replay_entry_id) = replay_entry_id {
                    lock(&self.replay_seed).take();
                    entry_id = Some(replay_entry_id);
                } else if final_message.role() == "custom" {
                    if let Some(custom) = CustomMessage::from_agent_message(&final_message) {
                        match self.with_session_mut(|manager| {
                            manager.append_custom_message_entry(
                                &custom.custom_type,
                                custom.content.clone(),
                                custom.display,
                                custom.details.clone(),
                            )
                        }) {
                            Ok(id) => {
                                // 后台任务终态通知真正落成 canonical 条目后才确认
                                // durable 投递；落盘失败时保持未确认，重启 sweep 会重放。
                                self.ack_background_notification_message(&custom);
                                entry_id = Some(id);
                            }
                            Err(error) => {
                                // TS 的 append 直接抛错：记录并停止当前 run（监听器没有错误通道，
                                // 见 `Core::persistence_error`），后续 message_end 逻辑不再执行。
                                self.record_persistence_error(&error);
                                self.agent.abort();
                                return;
                            }
                        }
                    }
                } else if matches!(
                    final_message.role(),
                    "system" | "user" | "assistant" | "toolResult"
                ) {
                    match self
                        .with_session_mut(|manager| manager.append_message(final_message.clone()))
                    {
                        Ok(id) => entry_id = Some(id),
                        Err(error) => {
                            self.record_persistence_error(&error);
                            self.agent.abort();
                            return;
                        }
                    }
                }
            }
            if let Some(entry_id) = entry_id {
                lock(&self.entry_ids_by_message).insert(message_key(&final_message), entry_id);
            }
            if let Some(assistant) = as_assistant(&final_message) {
                *lock(&self.last_assistant_message) = Some(assistant.clone());
                if assistant.stop_reason != StopReason::Error
                    && assistant.stop_reason != StopReason::Length
                {
                    self.overflow_recovery_attempted
                        .store(false, Ordering::SeqCst);
                }
                let retry_attempt = self.retry_attempt.load(Ordering::SeqCst);
                if assistant.stop_reason != StopReason::Error && retry_attempt > 0 {
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt: retry_attempt,
                        final_error: None,
                    });
                    self.retry_attempt.store(0, Ordering::SeqCst);
                }
            }
        }

        // turn 结束后冲刷上下文型自定义消息。
        if matches!(event, AgentEvent::TurnEnd { .. }) {
            self.flush_pending_custom_messages();
        }
    }

    /// 把 run 内记录的消息替换应用到一组快照消息（对应 TS 原地替换后的可见性）。
    fn apply_message_replacements(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let replacements = lock(&self.message_replacements);
        if replacements.is_empty() {
            return messages;
        }
        messages
            .into_iter()
            .map(|message| {
                replacements
                    .get(&message_key(&message))
                    .cloned()
                    .unwrap_or(message)
            })
            .collect()
    }

    /// 单条消息的有效值：扩展 `message_end` 的替换（TS 的原地改对象在 Rust 里是值替换）。
    ///
    /// agent-core 的循环仍持有替换前的值（`turn_end.message`、`agent_end.messages` 等），
    /// 这里按替换表的键做一次映射，让边界派发与公开事件看到与 TS 相同的对象内容。
    fn effective_message(&self, message: &AgentMessage) -> AgentMessage {
        lock(&self.message_replacements)
            .get(&message_key(message))
            .cloned()
            .unwrap_or_else(|| message.clone())
    }

    /// 用替换消息更新 agent 状态（对应 TS 的 `_replaceMessageInPlace`）。
    fn replace_agent_message(&self, target: &AgentMessage, replacement: AgentMessage) {
        let key = message_key(target);
        let mut messages = self.agent.messages();
        if let Some(index) = messages
            .iter()
            .position(|message| message_key(message) == key)
        {
            messages[index] = replacement;
            self.agent.set_messages(messages);
        }
    }

    /// 发扩展事件；`message_end` 可能返回替换消息。
    async fn emit_extension_event(&self, event: &AgentEvent) -> Option<AgentMessage> {
        let runner = self.runner()?;
        match event {
            AgentEvent::AgentStart => {
                self.turn_index.store(0, Ordering::SeqCst);
                let _ = runner
                    .emit(ExtensionEvent::AgentStart(AgentStartEvent {
                        event_type: AgentStartEventType::AgentStart,
                    }))
                    .await;
                None
            }
            AgentEvent::AgentEnd { messages } => {
                let _ = runner
                    .emit(ExtensionEvent::AgentEnd(AgentEndEvent {
                        event_type: AgentEndEventType::AgentEnd,
                        messages: messages.clone(),
                    }))
                    .await;
                None
            }
            AgentEvent::TurnStart => {
                let turn_index = self.turn_index.load(Ordering::SeqCst);
                let _ = runner
                    .emit(ExtensionEvent::TurnStart(TurnStartEvent {
                        event_type: TurnStartEventType::TurnStart,
                        turn_index,
                        timestamp: now_ms(),
                    }))
                    .await;
                None
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                if let AgentMessage::Standard(Message::Assistant(assistant)) = message {
                    let key = message_key(message);
                    let was_dispatched = lock(&self.boundary_dispatched_messages).remove(&key);
                    if !was_dispatched {
                        let _ = self
                            .dispatch_turn_end_boundary(assistant, tool_results)
                            .await;
                    }
                }
                self.turn_index.fetch_add(1, Ordering::SeqCst);
                None
            }
            AgentEvent::MessageStart { message } => {
                let _ = runner
                    .emit(ExtensionEvent::MessageStart(MessageStartEvent {
                        event_type: MessageStartEventType::MessageStart,
                        message: message.clone(),
                    }))
                    .await;
                None
            }
            AgentEvent::MessageUpdate {
                assistant_message_event,
                message,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::MessageUpdate(MessageUpdateEvent {
                        event_type: MessageUpdateEventType::MessageUpdate,
                        message: message.clone(),
                        assistant_message_event: assistant_message_event.clone(),
                    }))
                    .await;
                None
            }
            AgentEvent::MessageEnd { message } => {
                match runner
                    .emit_message_end(MessageEndEvent {
                        event_type: kiana_ext_api::events::MessageEndEventType::MessageEnd,
                        message: message.clone(),
                    })
                    .await
                {
                    Ok(replacement) => replacement.map(|replacement| {
                        if replacement.role() != "user"
                            && replacement.role() != "assistant"
                            && replacement.role() != "toolResult"
                            && replacement.role() != "custom"
                        {
                            return replacement;
                        }
                        // `content == null` 归一为空数组（未类型化扩展可能返回 null）。
                        normalize_message_content(replacement)
                    }),
                    Err(_) => None,
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionStart(
                        ToolExecutionStartEvent {
                            event_type: ToolExecutionStartEventType::ToolExecutionStart,
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                            parent_tool_call_id: None,
                        },
                    ))
                    .await;
                None
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionUpdate(
                        ToolExecutionUpdateEvent {
                            event_type: ToolExecutionUpdateEventType::ToolExecutionUpdate,
                            tool_call_id: tool_call_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                            partial_result: partial_result.clone(),
                            parent_tool_call_id: None,
                        },
                    ))
                    .await;
                None
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
            } => {
                let _ = runner
                    .emit(ExtensionEvent::ToolExecutionEnd(ToolExecutionEndEvent {
                        event_type: ToolExecutionEndEventType::ToolExecutionEnd,
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        result: result.clone(),
                        is_error: *is_error,
                        duration_ms: duration_ms.map(|value| value as u64),
                        parent_tool_call_id: None,
                    }))
                    .await;
                None
            }
        }
    }

    fn will_retry_after_agent_end(&self, messages: &[AgentMessage]) -> bool {
        if self.agent_run_abort_requested.load(Ordering::SeqCst) {
            return false;
        }
        let settings = self.settings_manager.get_retry_settings();
        let retry_attempt = self.retry_attempt.load(Ordering::SeqCst);
        if !settings.enabled || retry_attempt as f64 >= settings.max_retries {
            return false;
        }
        for message in messages.iter().rev() {
            if let Some(assistant) = as_assistant(message) {
                return self.is_retryable_error(assistant);
            }
        }
        false
    }

    // -- 消息身份 -----------------------------------------------------------

    /// 找到一条消息对应的持久化条目 id（边界事件需要）。三级查找：先查
    /// `entry_ids_by_message` 缓存，再倒序扫描当前分支，最后按 agent state 的消息
    /// 位置对齐到投影条目（第三级命中后写入缓存）。返回 `None` 表示消息还没有落盘。
    fn find_persisted_message_entry_id(&self, message: &AgentMessage) -> Option<String> {
        // 扩展 message_end 替换后的消息才是 agent state / 会话里持久化的那个值。
        let effective = self.effective_message(message);
        let message = &effective;
        let key = message_key(message);
        if let Some(mapped) = lock(&self.entry_ids_by_message).get(&key) {
            return Some(mapped.clone());
        }
        for entry in self.branch().iter().rev() {
            if let SessionEntry::Message(entry_message) = entry
                && message_key(&entry_message.message) == key
            {
                return Some(entry.id().to_string());
            }
        }
        let messages = self.agent.messages();
        let message_index = messages
            .iter()
            .position(|candidate| message_key(candidate) == key)?;
        let projection = self.projection();
        let mut projected_index = 0usize;
        for entry in &projection.entries {
            for _ in 0..entry.messages.len() {
                if projected_index == message_index {
                    let id = entry.source_entry.id().to_string();
                    lock(&self.entry_ids_by_message).insert(key, id.clone());
                    return Some(id);
                }
                projected_index += 1;
            }
        }
        None
    }

    fn find_last_assistant_message(&self) -> Option<AssistantMessage> {
        self.agent
            .messages()
            .iter()
            .rev()
            .find_map(|message| as_assistant(message).cloned())
    }

    // -- 上下文刷新 ---------------------------------------------------------

    /// 按会话文件的持久化投影整体重建 agent 消息与「消息键 → 条目 id」映射。
    /// 任何直接改写会话（压缩、树导航、context_edit）之后都必须调用它，让 agent 状态
    /// 与磁盘状态重新对齐。
    fn refresh_finalized_context(&self) {
        self.refresh_finalized_context_inner(false);
    }

    /// 会话打开时的恢复刷新：在重建消息视图的同时只修请求视图。
    ///
    /// 孤儿/重复 tool result 从本次请求的 messages 中移除，但 canonical transcript
    /// 不变（[`repair_request_view`] 不写回 SessionManager）。
    fn refresh_finalized_context_with_recovery(&self) {
        self.refresh_finalized_context_inner(true);
    }

    fn refresh_finalized_context_inner(&self, _apply_recovery: bool) {
        let projection = self.projection();
        {
            let mut ids = lock(&self.entry_ids_by_message);
            ids.clear();
            for entry in &projection.entries {
                for message in &entry.messages {
                    ids.insert(message_key(message), entry.source_entry.id().to_string());
                }
            }
        }
        // 每次重建 finalized 视图都重新分类并应用请求视图修复。只在会话打开时修一次会
        // 让树导航/压缩之后的刷新把未修复的 canonical 投影重新灌回 agent 历史，孤儿/
        // 重复 tool_result 会重新进入请求视图（wire-E §6.9）。
        let plan = self.with_session(plan_manager_recovery);
        let messages = repair_request_view(&projection.messages, &plan).messages;
        self.agent.set_messages(messages);
    }

    /// 会话打开时的分类是否需要刷新消息视图（Clean 不需要）。
    fn recovery_needs_refresh(&self) -> bool {
        lock(&self.initial_recovery)
            .as_ref()
            .is_some_and(|plan| plan.classification.class != RecoveryClass::Clean)
    }

    /// 该 entry id 是否已经重放过（canonical 里的 `kiana.recovery-replay` 标记）。
    fn recovery_replay_recorded(&self, entry_id: &str) -> bool {
        self.branch().iter().any(|entry| match entry {
            kiana_session_core::session_manager::SessionEntry::Custom(custom) => {
                custom.custom_type == RECOVERY_REPLAY_MARKER
                    && custom
                        .data
                        .as_ref()
                        .and_then(JsonValue::as_str)
                        .is_some_and(|value| value == entry_id)
            }
            _ => false,
        })
    }

    /// 持久化“已重放”标记（custom entry，不进入 LLM 上下文）。
    fn record_recovery_replay(&self, entry_id: &str) -> Result<(), AgentSessionError> {
        let data = JsonValue::String(entry_id.to_string());
        self.with_session_mut(|manager| {
            manager.append_custom_entry(RECOVERY_REPLAY_MARKER, Some(data.clone()))
        })
        .map_err(|error| AgentSessionError::other(error.to_string()))?;
        Ok(())
    }

    /// 用原 user 消息启动一次重放 run：不新增 canonical user entry。
    async fn start_recovery_replay(
        &self,
        message: kiana_agent_core::AgentMessage,
        entry_id: String,
    ) -> Result<(), AgentSessionError> {
        let text = agent_message_text(&message);
        let key = message_key(&message);
        *lock(&self.replay_seed) = Some(RecoveryReplaySeed {
            entry_id,
            message_key: key,
            message,
        });
        self.start_run(
            BeforeAgentStartTrigger::User {
                prompt: text,
                images: None,
            },
            None,
        )
        .await
    }

    /// `_buildBoundaryContext`。
    fn build_boundary_context(
        &self,
        drafts: Vec<SessionBoundaryDraft>,
        boundary: &str,
    ) -> Result<BoundaryContextPreview, AgentSessionError> {
        let manager = self.create_boundary_preview_manager(&drafts)?;
        let projection = manager.build_session_projection();
        let pending_messages = self.get_pending_boundary_messages();
        let llm_messages = convert_to_llm(&projection.messages);
        let final_role = llm_messages.last().map(|message| message.role());
        let has_non_system_context = llm_messages
            .iter()
            .any(|message| message.role() != "system");
        let context_can_continue = has_non_system_context && final_role != Some("assistant");
        let pending_custom_context = !lock(&self.pending_custom_messages).is_empty();
        Ok(BoundaryContextPreview {
            context_entries: projection.entries,
            context_messages: projection.messages,
            llm_messages,
            pending_messages,
            can_continue: context_can_continue
                || pending_custom_context
                || if boundary == "turn_end" {
                    self.agent.has_queued_messages()
                } else {
                    final_role == Some("assistant") && self.agent.has_queued_messages()
                },
        })
    }

    fn get_pending_boundary_messages(&self) -> Vec<AgentMessage> {
        let mut messages = self.agent.peek_queued_messages();
        messages.extend(
            lock(&self.pending_custom_messages)
                .iter()
                .map(custom_message_agent_message),
        );
        messages
    }

    fn create_boundary_preview_manager(
        &self,
        drafts: &[SessionBoundaryDraft],
    ) -> Result<SessionManager, AgentSessionError> {
        let (header, branch) =
            self.with_session(|manager| (manager.get_header().cloned(), manager.get_branch(None)));
        let Some(header) = header else {
            return Err(AgentSessionError::SessionHeaderMissing);
        };
        let mut files = vec![FileEntry::Header(header)];
        files.extend(branch.into_iter().map(FileEntry::Entry));
        let mut manager = SessionManager::in_memory(&self.cwd, None, Some(files))
            .map_err(|error| AgentSessionError::other(error.to_string()))?;
        self.apply_boundary_drafts(&mut manager, drafts)?;
        Ok(manager)
    }

    fn apply_boundary_drafts(
        &self,
        manager: &mut SessionManager,
        drafts: &[SessionBoundaryDraft],
    ) -> Result<Vec<SessionEntry>, AgentSessionError> {
        let mut appended = Vec::new();
        for draft in drafts {
            let entry_id = match draft {
                SessionBoundaryDraft::Custom(draft) => manager
                    .append_custom_entry(&draft.custom_type, draft.data.clone())
                    .map_err(|error| AgentSessionError::other(error.to_string()))?,
                SessionBoundaryDraft::CustomMessage(draft) => manager
                    .append_custom_message_entry(
                        &draft.custom_type,
                        serde_json::to_value(&draft.content).unwrap_or(serde_json::Value::Null),
                        draft.display,
                        draft.details.clone(),
                    )
                    .map_err(|error| AgentSessionError::other(error.to_string()))?,
                SessionBoundaryDraft::ContextEdit(draft) => manager
                    .append_context_edit(&draft.target_id, draft.replacement.clone())
                    .map_err(|error| AgentSessionError::other(error.to_string()))?,
                SessionBoundaryDraft::Compaction(draft) => {
                    let projected = manager.build_session_projection();
                    let branch = manager.get_branch(None);
                    let tokens_before =
                        estimate_projected_context_tokens(&projected, &branch).tokens;
                    manager
                        .append_compaction(
                            &draft.summary,
                            draft.first_kept_entry_id.as_deref(),
                            tokens_before as f64,
                            draft.details.clone(),
                            Some(true),
                            draft.usage.clone(),
                        )
                        .map_err(|error| AgentSessionError::other(error.to_string()))?
                }
            };
            if let Some(entry) = manager.get_entry(&entry_id) {
                appended.push(entry.clone());
            }
        }
        Ok(appended)
    }

    fn commit_boundary_drafts(&self, drafts: Vec<SessionBoundaryDraft>) {
        // 压缩会从 live history 里删掉 read 工具结果；读记录必须同步失效，否则模型可以
        // 用“已经不存在的 read”授权 write/edit（计划第 2 周「读后写」的一部分）。
        // 这里用最保守的整表清空：只影响体验（压缩后第一次写需要重新 read）。
        let compaction_committed = drafts
            .iter()
            .any(|draft| matches!(draft, SessionBoundaryDraft::Compaction(_)));
        let appended =
            self.with_session_mut(|manager| self.apply_boundary_drafts(manager, &drafts));
        match appended {
            Ok(appended) => {
                if compaction_committed {
                    self.read_state.clear();
                }
                self.refresh_finalized_context();
                for entry in appended {
                    self.emit(AgentSessionEvent::EntryAppended { entry });
                }
            }
            // TS 的 `_commitBoundaryDrafts` 没有 catch：写失败会中断这次边界派发并最终
            // reject run。边界钩子没有错误通道，记录后由 `_startRun` 结束时 reject。
            Err(error) => self.record_persistence_error(error),
        }
    }

    fn report_invalid_boundary_continuation(&self, event: &str) {
        if let Some(runner) = self.runner() {
            runner.emit_error(ExtensionError {
                extension_path: "<boundary>".to_string(),
                event: event.to_string(),
                error: format!("{event} requested continuation without runnable model context"),
                stack: None,
            });
        }
    }

    /// `turn_end` 边界派发：先解析 assistant/toolResult 的持久化条目 id（assistant 条目
    /// 解析不到发 `ExtensionError` 并放弃；toolResult 条目解析不到时静默跳过），再构造
    /// context 并派发 `emit_boundary`，提交扩展 drafts，
    /// 最后按 context 的 `can_continue` 校验扩展的 `continue` 请求。返回是否继续。
    async fn dispatch_turn_end_boundary(
        &self,
        message: &AssistantMessage,
        tool_results: &[ToolResultMessage],
    ) -> bool {
        *lock(&self.last_activity_outcome) = match message.stop_reason {
            StopReason::Aborted => AgentActivityOutcome::Aborted,
            StopReason::Error => AgentActivityOutcome::Error,
            _ => AgentActivityOutcome::Completed,
        };
        let assistant_message = AgentMessage::Standard(Message::Assistant(message.clone()));
        let message_entry_id = self.find_persisted_message_entry_id(&assistant_message);
        let Some(runner) = self.runner() else {
            return false;
        };
        if !runner.has_handlers("turn_end") {
            return false;
        }
        let Some(message_entry_id) = message_entry_id else {
            runner.emit_error(ExtensionError {
                extension_path: "<boundary>".to_string(),
                event: "turn_end".to_string(),
                error: "turn_end could not resolve the persisted assistant entry ID".to_string(),
                stack: None,
            });
            return false;
        };
        let tool_result_entry_ids: Vec<String> = tool_results
            .iter()
            .filter_map(|result| {
                let message = AgentMessage::Standard(Message::ToolResult(result.clone()));
                self.find_persisted_message_entry_id(&message)
            })
            .collect();
        let context = match self.build_boundary_context(Vec::new(), "turn_end") {
            Ok(context) => context,
            Err(_) => {
                return false;
            }
        };
        let base_event = BoundaryBaseEvent::TurnEnd(TurnEndEvent {
            entries: Vec::new(),
            r#continue: false,
            context,
            outcome: *lock(&self.last_activity_outcome),
            event_type: TurnEndEventType::TurnEnd,
            turn_index: self.turn_index.load(Ordering::SeqCst),
            message: assistant_message,
            tool_results: tool_results.to_vec(),
            message_entry_id,
            tool_result_entry_ids,
        });
        let core = lock(&self.self_ref).clone();
        let build: BuildBoundaryContext = Arc::new(move |drafts| {
            let core = core.clone();
            Box::pin(async move {
                match core.upgrade() {
                    Some(core) => core
                        .build_boundary_context(drafts, "turn_end")
                        .map_err(|error| error.to_string()),
                    None => Err("session is disposed".to_string()),
                }
            })
        });
        let boundary = runner.emit_boundary(base_event, build).await;
        let boundary = match boundary {
            Ok(boundary) => boundary,
            Err(_) => return false,
        };
        self.commit_boundary_drafts(boundary.entries.clone());
        if boundary.r#continue
            && !self
                .build_boundary_context(Vec::new(), "turn_end")
                .map(|context| context.can_continue)
                .unwrap_or(false)
        {
            self.report_invalid_boundary_continuation("turn_end");
            return false;
        }
        boundary.r#continue
    }

    // -- 队列冲刷 -----------------------------------------------------------

    fn flush_pending_custom_messages(&self) {
        let pending = std::mem::take(&mut *lock(&self.pending_custom_messages));
        for message in pending {
            if let Err(error) = self.append_custom_message(&message) {
                // TS 的 `_flushPendingCustomMessages` 没有 catch：第一个写失败即中断，
                // 剩余消息丢弃，异常向上抛出。
                self.record_persistence_error(&error);
                return;
            }
        }
    }

    fn append_custom_message(&self, message: &CustomMessage) -> Result<(), SessionManagerError> {
        self.with_session_mut(|manager| {
            manager.append_custom_message_entry(
                &message.custom_type,
                message.content.clone(),
                message.display,
                message.details.clone(),
            )
        })?;
        self.refresh_finalized_context();
        let agent_message = custom_message_agent_message(message);
        self.emit(AgentSessionEvent::MessageStart {
            message: agent_message.clone(),
        });
        self.emit(AgentSessionEvent::MessageEnd {
            message: agent_message,
        });
        Ok(())
    }

    fn flush_pending_bash_messages(&self) {
        let pending = std::mem::take(&mut *lock(&self.pending_bash_messages));
        if pending.is_empty() {
            return;
        }
        for message in pending {
            if let Err(error) =
                self.with_session_mut(|manager| manager.append_message(message.to_agent_message()))
            {
                self.record_persistence_error(&error);
                return;
            }
        }
        self.refresh_finalized_context();
    }

    // -- idle 等待 ----------------------------------------------------------

    fn is_idle(&self) -> bool {
        !self.is_agent_run_active.load(Ordering::SeqCst) && !self.is_compacting()
    }

    fn is_compacting(&self) -> bool {
        lock(&self.auto_compaction_abort).is_some()
            || lock(&self.compaction_abort).is_some()
            || lock(&self.branch_summary_abort).is_some()
    }

    fn resolve_idle_wait_if_idle(&self) {
        if !self.is_idle() {
            return;
        }
        // TS 里所有等待者共享同一个 `_idleWaitPromise`；这里唤醒全部等待者。
        let senders = std::mem::take(&mut *lock(&self.idle_wait));
        for sender in senders {
            let _ = sender.send(());
        }
    }

    async fn wait_for_idle(&self) {
        loop {
            if self.is_idle() {
                return;
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // 先登记再复查，避免 `_resolveIdleWaitIfIdle` 在检查与等待之间运行导致丢唤醒。
            lock(&self.idle_wait).push(sender);
            if self.is_idle() {
                self.resolve_idle_wait_if_idle();
            }
            let _ = receiver.await;
        }
    }
}

/// 权限层阻止工具调用时的统一返回。
fn blocked_tool_result(reason: String) -> BeforeToolCallResult {
    BeforeToolCallResult {
        block: Some(true),
        reason: Some(reason),
        terminate: Some(false),
    }
}

/// 把 `shellCommandPrefix` 拼到 bash 工具的 `command` 参数上（仅用于权限检查）。
pub(crate) fn with_shell_command_prefix(args: &JsonValue, prefix: &str) -> JsonValue {
    let JsonValue::Object(map) = args else {
        return args.clone();
    };
    let Some(command) = map.get("command").and_then(JsonValue::as_str) else {
        return args.clone();
    };
    let mut map = map.clone();
    map.insert(
        "command".to_string(),
        JsonValue::String(format!("{prefix}\n{command}")),
    );
    JsonValue::Object(map)
}

/// 审批对话框的固定选项文本（UI 返回同一字符串）。
/// InterruptedPrompt 重放标记（custom entry，不进入 LLM 上下文）。
const RECOVERY_REPLAY_MARKER: &str = "kiana.recovery-replay";
const APPROVAL_ALLOW_ONCE: &str = "Allow once";
const APPROVAL_ALWAYS: &str = "Always allow";
const APPROVAL_REJECT: &str = "Reject";
const APPROVAL_SCOPE_SESSION: &str = "This session";
const APPROVAL_SCOPE_PROJECT: &str = "This project";
const APPROVAL_SCOPE_LOCAL: &str = "This machine (local)";

/// 结构化审批：三次以内的选择（主选择 → 作用域 / 拒绝原因）。
///
/// 使用 `ExtensionUiContext::select` 而不是旧的 Yes/No `confirm`，这样 TUI 与 RPC 都能
/// 展示「只允许一次 / 总是允许 / 拒绝」。选择器关闭或返回未知值时按拒绝处理
/// （fail-closed）。
async fn ask_for_approval(ui_context: &dyn ExtensionUiContext, prompt: &str) -> ApprovalDecision {
    let title = format!("Permission required\n{prompt}");
    let options = vec![
        APPROVAL_ALLOW_ONCE.to_string(),
        APPROVAL_ALWAYS.to_string(),
        APPROVAL_REJECT.to_string(),
    ];
    let Some(choice) = ui_context.select(&title, &options, None).await else {
        return ApprovalDecision::Reject {
            reason: Some("Approval was cancelled.".to_string()),
        };
    };
    match choice.as_str() {
        APPROVAL_ALLOW_ONCE => ApprovalDecision::AllowOnce,
        APPROVAL_ALWAYS => {
            let scopes = vec![
                APPROVAL_SCOPE_SESSION.to_string(),
                APPROVAL_SCOPE_PROJECT.to_string(),
                APPROVAL_SCOPE_LOCAL.to_string(),
            ];
            let Some(scope) = ui_context
                .select("Remember this permission?", &scopes, None)
                .await
            else {
                return ApprovalDecision::Reject {
                    reason: Some("Approval was cancelled.".to_string()),
                };
            };
            let scope = match scope.as_str() {
                APPROVAL_SCOPE_SESSION => ApprovalScope::Session,
                APPROVAL_SCOPE_PROJECT => ApprovalScope::Project,
                APPROVAL_SCOPE_LOCAL => ApprovalScope::Local,
                _ => {
                    return ApprovalDecision::Reject {
                        reason: Some("Unknown permission scope.".to_string()),
                    };
                }
            };
            ApprovalDecision::AllowAlways { scope }
        }
        APPROVAL_REJECT => {
            let reason = ui_context
                .input("Reason for rejecting (optional)", None, None)
                .await
                .and_then(|reason| {
                    let reason = reason.trim().to_string();
                    (!reason.is_empty()).then_some(reason)
                });
            ApprovalDecision::Reject { reason }
        }
        _ => ApprovalDecision::Reject {
            reason: Some("Unknown permission choice.".to_string()),
        },
    }
}

/// 审批卡选择 → 权限层决策。
fn permission_card_choice_to_decision(choice: PermissionCardChoice) -> ApprovalDecision {
    match choice {
        PermissionCardChoice::AllowOnce => ApprovalDecision::AllowOnce,
        PermissionCardChoice::AllowAlways(scope) => ApprovalDecision::AllowAlways {
            scope: match scope {
                PermissionScopeKind::Session => ApprovalScope::Session,
                PermissionScopeKind::Project => ApprovalScope::Project,
                PermissionScopeKind::Local => ApprovalScope::Local,
            },
        },
        PermissionCardChoice::Reject { reason } => ApprovalDecision::Reject { reason },
    }
}

/// 构造 per-tool 审批卡请求。
///
/// `dangerous` 必须来自权限层的命令分析（[`PermissionRequest::is_dangerous`]），不能用
/// 调用方的猜测：危险动作在卡片里默认聚焦 Reject 且 Enter 不批准。
fn build_permission_card_request(
    request: &PermissionRequest,
    reason: String,
    always_scopes: Vec<PermissionScopeKind>,
) -> PermissionCardRequest {
    PermissionCardRequest {
        tool_name: request.tool_name.clone(),
        args: request.args.clone(),
        cwd: request.cwd.display().to_string(),
        decision_reason: reason,
        always_scopes,
        dangerous: request.is_dangerous(),
    }
}

/// PreToolUse hook 对工具调用流程的影响。
enum PreToolUseHookEffect {
    /// 继续执行；`allow` 表示 hook 显式 `permissionDecision: allow`（可跳过权限层的
    /// Ask，但不能覆盖 deny），`additional_context` 是已转义的模型上下文。
    Continue {
        allow: bool,
        additional_context: Option<String>,
    },
    /// 阻断本次调用。
    Block(BeforeToolCallResult),
}

impl Core {
    // -- Hooks --------------------------------------------------------------

    fn hook_runner(&self) -> Option<Arc<HookRunner>> {
        lock(&self.hook_runner).clone()
    }

    /// 构造一个带会话元数据的 hook 输入。
    fn hook_input(&self, event: HookEvent) -> HookInput {
        let mut input = HookInput::new(event, self.session_id(), self.cwd.as_str());
        if let Some(path) = self.session_file() {
            input = input.with_transcript_path(path);
        }
        let mode = lock(&self.permissions).as_ref().map(|engine| engine.mode());
        if let Some(mode) = mode {
            input = input.with_permission_mode(mode.as_str());
        }
        input
    }

    /// 执行一个 hook 事件。
    ///
    /// `HookTurnState` 按回合共享：先短锁拷贝，执行期间不持锁，结束后写回。这样
    /// deny 上限与递归深度在 PreToolUse / Stop / PreCompact 等事件之间连续。
    ///
    /// 成功返回的 `HookOutcome` 在这里统一转成用户可见事件（`systemMessage` 与非阻塞
    /// 警告），因此所有事件类型（包括 SessionStart/SessionEnd/PostCompact）都有同一条
    /// 展示通道，不会因某个事件没有专门消费点而静默丢弃。
    async fn run_hook_event(
        &self,
        input: &HookInput,
    ) -> Result<Option<HookOutcome>, kiana_hooks::HookError> {
        let Some(runner) = self.hook_runner() else {
            return Ok(None);
        };
        // 状态在整个事件期间独占（跨 await 持有 tokio 锁），否则并行批次的
        // `deny_count` 增量会丢失。
        let _turn_guard = self.hook_turn_lock.lock().await;
        let mut turn = *lock(&self.hook_turn);
        let outcome = runner.run_event_with_state(input, &mut turn).await;
        *lock(&self.hook_turn) = turn;
        let outcome = outcome?;
        self.emit_hook_notifications(&outcome);
        Ok(Some(outcome))
    }

    /// 把 hook 的用户可见输出转成 [`AgentSessionEvent::HookNotification`]。
    ///
    /// - `systemMessage`：`suppressOutput: true` 时隐藏（与参考实现一致）；
    /// - `warnings`：始终展示。它们代表 hook 自身失败（exit 1、超时、非法 JSON、
    ///   HTTP 非 2xx、白名单/SSRF 拒绝），隐藏会让用户误以为 hook 生效了；
    /// - `continue: false`：`stopReason` 作为 info 消息展示，调用方仍负责终止 run。
    fn emit_hook_notifications(&self, outcome: &HookOutcome) {
        let event = outcome.event.as_str().to_string();
        if !outcome.suppress_output {
            for message in &outcome.system_messages {
                self.emit(AgentSessionEvent::HookNotification {
                    message: message.clone(),
                    level: "info".to_string(),
                    hook_event: event.clone(),
                });
            }
        }
        for warning in &outcome.warnings {
            self.emit(AgentSessionEvent::HookNotification {
                message: warning.clone(),
                level: "warning".to_string(),
                hook_event: event.clone(),
            });
        }
        if !outcome.continue_execution
            && let Some(reason) = &outcome.stop_reason
        {
            self.emit(AgentSessionEvent::HookNotification {
                message: format!("Hook stopped execution: {reason}"),
                level: "info".to_string(),
                hook_event: event,
            });
        }
    }

    /// 暂存 hook 的 `additionalContext`，等下一次 provider 请求时注入。
    fn queue_hook_context(&self, outcome: &HookOutcome) {
        if let Some(context) = outcome.additional_context_xml() {
            lock(&self.pending_hook_contexts)
                .push(format!("<hook-context>{context}</hook-context>"));
        }
    }

    fn take_hook_contexts(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.pending_hook_contexts))
    }

    /// PreToolUse：hook 改写先于扩展与权限判断。
    ///
    /// 顺序契约（PORTING_PLAN 第 4 周）：hook 改写 → 扩展 `tool_call` → 权限判断 →
    /// 执行。`updatedInput` 只改写 `context.args`，不能绕过权限层的 deny。
    ///
    /// `permissionDecision: allow` 只跳过权限层的 `Ask`（hook 已显式放行），deny 规则
    /// 仍然优先；`continue: false` 阻断本次调用并终止 run；`additionalContext`
    /// 进入下一次请求的 hidden context。
    async fn pre_tool_use_hook(&self, context: &BeforeToolCallContext) -> PreToolUseHookEffect {
        let input = self
            .hook_input(HookEvent::PreToolUse)
            .with_tool_call(context.tool_call.name.clone(), context.args.get());
        let outcome = match self.run_hook_event(&input).await {
            Ok(Some(outcome)) => outcome,
            Ok(None) => {
                return PreToolUseHookEffect::Continue {
                    allow: false,
                    additional_context: None,
                };
            }
            // 递归深度超限/输入非法属于安全边界失败：工具调用 fail-closed。
            Err(error) => {
                return PreToolUseHookEffect::Block(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some(format!("Hook evaluation failed: {error}")),
                    terminate: Some(false),
                });
            }
        };
        // deny 优先，且 `blocking_error()` 只在这里消费一次。
        if let Some(reason) = outcome.blocking_error() {
            return PreToolUseHookEffect::Block(BeforeToolCallResult {
                block: Some(true),
                reason: Some(reason.to_string()),
                terminate: Some(false),
            });
        }
        // `continue: false`：阻断并请求终止 run；stopReason 同时作为模型可见错误。
        if !outcome.continue_execution {
            let reason = outcome
                .stop_reason
                .clone()
                .or_else(|| outcome.reason.clone())
                .unwrap_or_else(|| "Tool execution was stopped by a hook".to_string());
            self.agent_run_abort_requested.store(true, Ordering::SeqCst);
            return PreToolUseHookEffect::Block(BeforeToolCallResult {
                block: Some(true),
                reason: Some(reason),
                terminate: Some(true),
            });
        }
        if outcome.is_asking() && !self.approve_hook_ask(&outcome).await {
            let reason = outcome
                .reason
                .clone()
                .unwrap_or_else(|| "Blocked by hook: approval is required".to_string());
            return PreToolUseHookEffect::Block(BeforeToolCallResult {
                block: Some(true),
                reason: Some(reason),
                terminate: Some(false),
            });
        }
        let additional_context = outcome.additional_context_xml();
        if let Some(updated) = outcome.updated_input {
            // 整体替换，不 merge；权限层随后读取的是改写后的参数。
            context.args.set(updated);
        }
        PreToolUseHookEffect::Continue {
            allow: outcome.decision == kiana_hooks::HookDecision::Allow,
            additional_context,
        }
    }

    /// hook 的 `Ask` 决策复用权限审批 UI；没有 UI 时 fail-closed。
    async fn approve_hook_ask(&self, outcome: &HookOutcome) -> bool {
        let Some(ui_context) = lock(&self.extension_ui_context).clone() else {
            return false;
        };
        let prompt = outcome.reason.clone().unwrap_or_else(|| {
            "A hook requires approval before this tool call can run.".to_string()
        });
        let decision = dispatch_local(async move { ask_for_approval(&*ui_context, &prompt).await })
            .await
            .unwrap_or(ApprovalDecision::Reject {
                reason: Some("Approval was cancelled.".to_string()),
            });
        !matches!(decision, ApprovalDecision::Reject { .. })
    }

    /// PostToolUse / PostToolUseFailure：只做反馈合并，不改变已执行的副作用。
    async fn post_tool_use_hook(&self, context: &AfterToolCallContext) -> Option<HookOutcome> {
        let event = if context.is_error {
            HookEvent::PostToolUseFailure
        } else {
            HookEvent::PostToolUse
        };
        let mut input = self
            .hook_input(event)
            .with_tool_call(context.tool_call.name.clone(), context.args.clone());
        if context.is_error {
            let error_text = context
                .result
                .content
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.as_str()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            input = input.with_error(error_text);
        } else {
            input = input.with_tool_response(serde_json::json!({
                "content": context.result.content,
                "details": context.result.details,
                "isError": false,
            }));
        }
        self.run_hook_event(&input).await.ok().flatten()
    }

    /// PermissionDenied：`retry` 与 feedback 只作为模型提示，绝不改变 deny 结论。
    async fn permission_denied_hook(
        &self,
        tool_name: &str,
        args: &JsonValue,
        reason: &str,
    ) -> Option<HookOutcome> {
        let input = self
            .hook_input(HookEvent::PermissionDenied)
            .with_tool_call(tool_name, args.clone())
            .with_reason(reason);
        self.run_hook_event(&input).await.ok().flatten()
    }

    /// UserPromptSubmit：deny 阻止提交，`additionalContext` 追加到用户消息。
    async fn user_prompt_submit_hook(
        &self,
        prompt: &str,
    ) -> Result<Option<HookOutcome>, AgentSessionError> {
        let input = self
            .hook_input(HookEvent::UserPromptSubmit)
            .with_prompt(prompt);
        self.run_hook_event(&input)
            .await
            .map_err(|error| AgentSessionError::other(format!("Hook evaluation failed: {error}")))
    }

    /// Stop：deny 表示“不要结束回合”。
    async fn stop_hook(&self) -> Option<HookOutcome> {
        let input = self.hook_input(HookEvent::Stop);
        self.run_hook_event(&input).await.ok().flatten()
    }

    /// PreCompact：deny 阻止（手动）或跳过（自动）本次压缩。
    async fn pre_compact_hook(
        &self,
        trigger: &str,
        custom_instructions: Option<&str>,
    ) -> Option<HookOutcome> {
        let mut input = self.hook_input(HookEvent::PreCompact).with_trigger(trigger);
        if let Some(instructions) = custom_instructions {
            input = input.with_custom_instructions(instructions);
        }
        self.run_hook_event(&input).await.ok().flatten()
    }

    /// PostCompact：不阻塞压缩；`additionalContext` 注入下一次请求，
    /// `continue: false` 终止当前 run（压缩已经提交，副作用不可撤销）。
    async fn post_compact_hook(&self, trigger: &str, custom_instructions: Option<&str>) {
        let mut input = self
            .hook_input(HookEvent::PostCompact)
            .with_trigger(trigger);
        if let Some(instructions) = custom_instructions {
            input = input.with_custom_instructions(instructions);
        }
        if let Ok(Some(outcome)) = self.run_hook_event(&input).await {
            self.queue_hook_context(&outcome);
            if !outcome.continue_execution {
                self.agent_run_abort_requested.store(true, Ordering::SeqCst);
            }
        }
    }

    /// SessionStart（best-effort）。会话创建是同步路径，这里通常由
    /// `AgentSession::bind_extensions` 在异步上下文里调用；`additionalContext` 暂存到
    /// 下一次请求的 hidden context（会话创建时尚无稳定注入点）。
    pub(crate) async fn run_session_start_hook(&self, source: &str) {
        let input = self.hook_input(HookEvent::SessionStart).with_source(source);
        if let Ok(Some(outcome)) = self.run_hook_event(&input).await {
            self.queue_hook_context(&outcome);
        }
    }

    /// SessionEnd（best-effort）；决策与附加上下文都忽略，只保留审计。
    pub(crate) async fn run_session_end_hook(&self, reason: &str) {
        let input = self.hook_input(HookEvent::SessionEnd).with_reason(reason);
        let _ = self.run_hook_event(&input).await;
    }

    fn hook_audit_records(&self) -> Vec<HookAuditRecord> {
        self.hook_audit.records()
    }

    // -- 工具钩子 -----------------------------------------------------------

    /// 工具执行前的三层检查：PreToolUse hook → 扩展 `tool_call` 钩子 → 权限层。
    ///
    /// 顺序很重要：hook 与扩展都可以改写 `args`，权限层必须看到最终参数；任何一层的
    /// deny 都优先，hook 的 `Allow` 不能跳过权限判断。
    async fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        parent_tool_call_id: Option<String>,
    ) -> Option<BeforeToolCallResult> {
        let hook_allow = match self.pre_tool_use_hook(&context).await {
            PreToolUseHookEffect::Block(result) => return Some(result),
            PreToolUseHookEffect::Continue {
                allow,
                additional_context,
            } => {
                if let Some(context_text) = additional_context {
                    lock(&self.pending_hook_contexts)
                        .push(format!("<hook-context>{context_text}</hook-context>"));
                }
                allow
            }
        };

        let extension_result = self
            .extension_before_tool_call(&context, parent_tool_call_id)
            .await;
        if extension_result
            .as_ref()
            .is_some_and(|result| result.block == Some(true))
        {
            return extension_result;
        }

        // `paths` 技能可见性：记录 hook/扩展改写后的参数实际触达的路径。即使会话没有
        // 启用权限引擎（`permission_before_tool_call` 直接返回），这里也要记录。
        self.record_touched_for_tool_call(&context.tool_call.name, context.args.get());

        if let Some(result) = self.permission_before_tool_call(&context, hook_allow).await {
            return Some(result);
        }

        // 第 11 周后半：权限层放行后、真正执行前做私有 git checkpoint（每 turn/目录
        // 最多一次）。只读工具与非 mutating 工具由 is_mutating_tool 过滤。
        self.checkpoint_before_mutating_tool(&context);

        extension_result
    }

    /// mutating tool 前的 checkpoint 入口（continuity 关闭时是 no-op）。
    fn checkpoint_before_mutating_tool(&self, context: &BeforeToolCallContext) {
        let Some(continuity) = &self.continuity else {
            return;
        };
        let turn_id = format!(
            "{}:{}",
            self.run_sequence.load(Ordering::SeqCst),
            self.turn_index.load(Ordering::SeqCst)
        );
        continuity.checkpoint_before_tool(Path::new(&self.cwd), &turn_id, &context.tool_call.name);
    }

    /// 工具完成后的账本与验收采集（continuity 关闭时是 no-op）。
    fn record_tool_effects(&self, context: &AfterToolCallContext) {
        let Some(continuity) = &self.continuity else {
            return;
        };
        let output = tool_result_text(&context.result.content);
        continuity.record_execution(
            &context.tool_call.name,
            context.args.clone(),
            output,
            receipt_status(context.is_error),
        );
        if context.is_error {
            return;
        }
        // 只对能枚举路径的文件工具记 ledger；bash 的任意写入无法枚举。
        let workdir = Path::new(&self.cwd);
        for relative in written_paths_from_args(&context.tool_call.name, &context.args) {
            let path = if Path::new(&relative).is_absolute() {
                PathBuf::from(&relative)
            } else {
                workdir.join(&relative)
            };
            continuity.record_agent_write(workdir, &path);
        }
    }

    /// 把待投递的后台任务 terminal 通知转成下一轮请求里的自定义消息。
    ///
    /// 通知来自 durable journal 的进程内投递队列（见 `background::durable`）；这里取走
    /// 后由 agent 的 MessageEnd 持久化为 `kiana.background-task` 自定义条目，因此父会话
    /// 之后的轮次仍能在 canonical 里看到它。
    fn take_background_notification_messages(&self) -> Vec<CustomMessage> {
        let Some(continuity) = &self.continuity else {
            return Vec::new();
        };
        continuity
            .take_background_notifications(&self.session_id())
            .into_iter()
            .map(|notification| {
                let summary = match (&notification.reason, notification.exit_code) {
                    (Some(reason), _) => reason.clone(),
                    (None, Some(code)) => format!("exit code {code}"),
                    (None, None) => "no reason recorded".to_string(),
                };
                // `content` 必须是字符串：custom-role 投影把非 string/array 内容
                // 转成空 user turn，模型只看到一条空消息。结构化字段放进 details，
                // ack 也从 details 读取。
                let status = notification.status.as_str();
                let truncated = if notification.output_truncated {
                    ", truncated"
                } else {
                    ""
                };
                let content = format!(
                    "Background task {} finished: status={status}, summary={summary}, output={} ({} bytes{truncated}).",
                    notification.task_id, notification.output_path, notification.output_bytes,
                );
                CustomMessage {
                    role: kiana_session_core::messages::CustomRole::Custom,
                    custom_type: "kiana.background-task".to_string(),
                    content: serde_json::Value::String(content),
                    display: true,
                    details: Some(serde_json::json!({
                        "taskId": notification.task_id,
                        "terminalSeq": notification.terminal_seq,
                        "status": status,
                        "reason": notification.reason,
                        "exitCode": notification.exit_code,
                        "outputPath": notification.output_path,
                        "outputBytes": notification.output_bytes,
                        "outputTruncated": notification.output_truncated,
                        "summary": summary,
                        "parentSessionId": notification.parent_session_id,
                        "parentTurnId": notification.parent_turn_id,
                        "tool": notification.tool,
                    })),
                    timestamp: now_ms() as f64,
                }
            })
            .collect()
    }

    /// `kiana.background-task` 自定义消息落盘后确认对应的 durable 投递。
    fn ack_background_notification_message(&self, custom: &CustomMessage) {
        if custom.custom_type != "kiana.background-task" {
            return;
        }
        let details = custom.details.as_ref();
        let task_id = details
            .and_then(|details| details.get("taskId"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                custom
                    .content
                    .get("taskId")
                    .and_then(serde_json::Value::as_str)
            });
        let Some(task_id) = task_id else {
            return;
        };
        let terminal_seq = details
            .and_then(|details| details.get("terminalSeq"))
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                custom
                    .content
                    .get("terminalSeq")
                    .and_then(serde_json::Value::as_u64)
            });
        let Some(terminal_seq) = terminal_seq else {
            return;
        };
        // take 已按父会话过滤；这里再防御一次，避免持久化旧格式消息时误 ack 兄弟会话。
        if details
            .and_then(|details| details.get("parentSessionId"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|parent| parent != self.session_id())
        {
            return;
        }
        if let Some(continuity) = self.continuity.as_ref() {
            continuity.ack_background_notification(task_id, terminal_seq);
        }
    }

    /// 记录一次工具调用会触达的路径（供 `paths` 技能下一轮可见性使用）。
    fn record_touched_for_tool_call(&self, tool_name: &str, args: JsonValue) {
        let builtin = !lock(&self.custom_tool_names).contains(tool_name);
        let args = self.permission_args(tool_name, builtin, args);
        let request =
            PermissionRequest::from_tool_call_with(tool_name, &args, self.cwd.as_str(), builtin);
        self.record_touched_paths(&request);
    }

    /// 权限层：`Allow` 放行，`Deny` 阻止，`Ask` 通过交互式 UI 确认（无 UI 时失败关闭）。
    async fn permission_before_tool_call(
        &self,
        context: &BeforeToolCallContext,
        hook_allowed: bool,
    ) -> Option<BeforeToolCallResult> {
        let engine = lock(&self.permissions).clone()?;
        let builtin = !lock(&self.custom_tool_names).contains(&context.tool_call.name);
        let args = self.permission_args(&context.tool_call.name, builtin, context.args.get());
        let request = PermissionRequest::from_tool_call_with(
            &context.tool_call.name,
            &args,
            self.cwd.as_str(),
            builtin,
        );
        self.refusal_for(&engine, &request, hook_allowed)
            .await
            .map(blocked_tool_result)
    }

    /// 扩展**直接**执行工具（`ctx.tools[i].execute` / `create*Tool().execute`）时的权限
    /// 检查。这两条路径不经过 `before_tool_call`，必须由扩展宿主回调到这里。
    async fn tool_permission_refusal(&self, tool_name: String, args: JsonValue) -> Option<String> {
        let engine = lock(&self.permissions).clone()?;
        let builtin = !lock(&self.custom_tool_names).contains(&tool_name);
        let args = self.permission_args(&tool_name, builtin, args);
        let request =
            PermissionRequest::from_tool_call_with(&tool_name, &args, self.cwd.as_str(), builtin);
        self.record_touched_paths(&request);
        // 扩展直接执行的工具没有 PreToolUse hook 的 allow 结论。
        self.refusal_for(&engine, &request, false).await
    }

    /// 运行期切换权限模式。
    ///
    /// 引擎尚未启用时新建一个（默认使用会话 cwd 作为工作区根）；这样 CLI 的
    /// `--permission-mode` 不依赖 settings 必须存在。
    fn set_permission_mode(&self, mode: PermissionMode) {
        // 先取额外根再锁 permissions，避免与 `add_permission_directory` 的锁顺序相反。
        let additional = lock(&self.additional_writable_roots).clone();
        let created = {
            let mut guard = lock(&self.permissions);
            match guard.as_ref() {
                Some(engine) => {
                    engine.set_mode(mode);
                    None
                }
                None => {
                    let mut policy = PermissionPolicy::new(mode, self.cwd.as_str());
                    policy.workspace_roots.extend(additional);
                    let engine = Arc::new(PermissionEngine::new(policy));
                    *guard = Some(Arc::clone(&engine));
                    Some(engine)
                }
            }
        };
        // 记忆写入门禁在 Core::new 时可能因为权限层未启用而拿到 deny-all；CLI 的
        // `--permission-mode` 到此时才创建引擎，必须同步接线。
        if let Some(engine) = created
            && let Some(memory) = self.memory.as_ref()
        {
            memory.enable_permission_gate(engine, &self.cwd);
        }
        // 执行策略必须跟着模式走（read-only/bypass 显式映射；其它模式回到环境变量 +
        // addDirs），工具定义里的写入门禁与 bash 的 SandboxManager 共用这份策略。
        self.refresh_execution_policy_and_tools();
    }

    /// 当前执行策略（会话级，已合并权限模式与 addDirs）。
    fn execution_policy(&self) -> Arc<ExecutionPolicy> {
        lock(&self.execution_policy).clone()
    }

    /// 与 [`Core::execution_policy`] 对应的沙箱 manager。
    fn sandbox_manager(&self) -> Option<Arc<SandboxManager>> {
        lock(&self.sandbox).clone()
    }

    /// 按当前权限引擎重算执行策略与沙箱 manager。
    fn refresh_execution_policy(&self) {
        let engine = lock(&self.permissions).clone();
        let additional = lock(&self.additional_writable_roots).clone();
        let (policy, sandbox) = execution_context_for(engine.as_deref(), &self.cwd, &additional);
        *lock(&self.execution_policy) = policy;
        *lock(&self.sandbox) = sandbox;
    }

    /// 重算执行策略并重建内建工具定义。
    ///
    /// `write`/`edit` 在构造时捕获 `Arc<ExecutionPolicy>`，`bash` 捕获
    /// `SandboxManager`，所以模式/addDirs 变化后必须重建定义，否则工具仍在用旧策略。
    /// 正在跑 run 时只置 `pending_tool_refresh`，等安全读取点统一刷新，避免在扩展派发
    /// 中途改动注册表。
    fn refresh_execution_policy_and_tools(&self) {
        self.refresh_execution_policy();
        self.rebuild_builtin_tool_definitions();
    }

    /// 内建工具的构造选项：read/write/edit 共用会话读记录表，write/edit/bash 共用
    /// 同一份执行策略。
    fn builtin_tool_options(&self) -> ToolsOptions {
        let read_state = Arc::clone(&self.read_state);
        let execution_policy = self.execution_policy();
        let sandbox = self.sandbox_manager();
        let shell_path = self.settings_manager.get_shell_path();
        // `BashToolOptions::sandbox = None` 的含义是“由 KIANA_SANDBOX_POLICY 决定”，
        // 不是“明确不隔离”。会话策略若已经是 DangerFullAccess（例如 bypass 模式），
        // 必须显式给一个不带沙箱的执行器，否则工具会绕过会话策略重新读环境变量。
        let bash_operations = if sandbox.is_none() {
            Some(kiana_tools::bash::create_local_bash_operations(
                shell_path.as_deref(),
            ))
        } else {
            None
        };
        // PowerShell 与会话里的 bash 共用同一个沙箱 manager；策略明确
        // DangerFullAccess（`sandbox = None`）时显式给无沙箱执行器，避免工具自行读
        // `KIANA_SANDBOX_POLICY` 把会话的 bypass 结论覆盖掉。
        let powershell_operations = sandbox
            .is_none()
            .then(kiana_tools::powershell::create_local_power_shell_operations);
        // 当前 run 的 W3C trace context 注入 bash 子进程（`TRACEPARENT`/`TRACESTATE`）。
        let telemetry_for_bash = Arc::clone(&self.telemetry);
        let trace_context_provider: kiana_tools::bash::TraceContextProvider = Arc::new(move || {
            telemetry_for_bash
                .current_trace_context()
                .map(|trace| (trace.traceparent.to_header(), trace.tracestate))
        });
        ToolsOptions {
            read: Some(ReadToolOptions {
                auto_resize_images: self.settings_manager.get_image_auto_resize(),
                read_state: Some(Arc::clone(&read_state)),
                ..Default::default()
            }),
            write: Some(WriteToolOptions {
                read_state: Some(Arc::clone(&read_state)),
                execution_policy: Some(Arc::clone(&execution_policy)),
                ..Default::default()
            }),
            edit: Some(EditToolOptions {
                read_state: Some(Arc::clone(&read_state)),
                execution_policy: Some(Arc::clone(&execution_policy)),
                ..Default::default()
            }),
            bash: Some(BashToolOptions {
                operations: bash_operations,
                command_prefix: self.settings_manager.get_shell_command_prefix(),
                shell_path,
                sandbox: sandbox.clone(),
                trace_context: Some(trace_context_provider),
                ..Default::default()
            }),
            powershell: Some(PowerShellToolOptions {
                operations: powershell_operations,
                sandbox: sandbox.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// `tools.apply_patch.models` 配置。
    ///
    /// week 7 handoff 建议在 `kiana-config` 的 settings schema 增加类型化字段；本波
    /// Wire B 的共享文件所有权不包含 `kiana-config`，因此从合并后 `Settings` 的
    /// `tools` 原始值读取（未知键保存在 `extra`，跨层合并按对象递归）。线格式与
    /// `ApplyPatchSettings::from_config` 完全一致（`tools.applyPatch.models` /
    /// `tools.apply_patch.models`）。
    fn apply_patch_settings(&self) -> ApplyPatchSettings {
        self.settings_manager
            .get_settings()
            .raw("tools")
            .map(|tools| ApplyPatchSettings::from_config(&serde_json::json!({ "tools": tools })))
            .unwrap_or_default()
    }

    /// 按当前模型构造内建工具定义。
    ///
    /// 键序仍是 8 个内建 `ToolName`；命中 `tools.apply_patch.models` 时 `edit` 槽的
    /// `definition.name` 是 `apply_patch`，其余模型继续使用 `edit`（两者不会同时暴露）。
    fn builtin_tool_definitions(&self) -> ToolDefinitions {
        let apply_patch_settings = self.apply_patch_settings();
        let model_id = self.current_model().map(|model| model.id);
        let expose_apply_patch =
            should_expose_apply_patch(model_id.as_deref(), &apply_patch_settings);
        let apply_patch_options = ApplyPatchToolOptions {
            read_state: Some(Arc::clone(&self.read_state)),
            execution_policy: Some(self.execution_policy()),
            ..Default::default()
        };
        let definitions = create_all_tool_definitions_for_model(
            self.cwd.clone(),
            Some(self.builtin_tool_options()),
            model_id.as_deref(),
            &apply_patch_settings,
            Some(apply_patch_options),
        );
        // 激活名单/允许列表按实际模型可见名字工作；缓存 `edit` 槽的名字用于双向映射。
        *lock(&self.edit_tool_name) = if expose_apply_patch {
            kiana_tools::index::APPLY_PATCH_TOOL_NAME.to_string()
        } else {
            "edit".to_string()
        };
        definitions
    }

    /// 用当前选项重建 `base_tool_definitions`。
    ///
    /// 调用方提供了 `base_tools_override` 时不重建（宿主完全接管工具集）。
    fn rebuild_builtin_tool_definitions(&self) {
        if self.base_tools_override.is_some() {
            *lock(&self.edit_tool_name) = "edit".to_string();
            return;
        }
        let definitions = self.builtin_tool_definitions();
        {
            let mut base = lock(&self.base_tool_definitions);
            base.clear();
            for (name, definition) in definitions {
                base.insert(name.as_str().to_string(), definition);
            }
        }
        if self.is_agent_run_active.load(Ordering::SeqCst) {
            self.pending_tool_refresh.store(true, Ordering::SeqCst);
        } else {
            self.refresh_tool_registry(None, false);
        }
    }

    /// 配置层整体指纹（`sha256:<hex>`）。
    fn config_fingerprint(&self) -> &str {
        &self.config_fingerprint
    }

    /// 某个 key path 的配置来源。
    fn config_origin(&self, key_path: &str) -> Option<ConfigLayerSource> {
        if let Some(source) = self.config_origins.get(key_path) {
            return Some(*source);
        }
        let prefix = if key_path.is_empty() {
            String::new()
        } else {
            format!("{key_path}.")
        };
        self.config_origins
            .iter()
            .filter(|(candidate, _)| candidate.starts_with(&prefix))
            .map(|(_, source)| *source)
            .max()
    }

    /// 配置层加载/严格诊断。
    fn config_diagnostics(&self) -> &[String] {
        &self.config_diagnostics
    }

    /// 替换全部权限规则。
    fn set_permission_rules(&self, rules: Vec<PermissionRule>) {
        // settings/env 明确关闭权限层时，只有 `set_permission_mode` 可以重新启用；
        // 规则注入不能顺手把引擎带回来。
        if self.permissions_disabled && lock(&self.permissions).is_none() {
            return;
        }
        let additional = lock(&self.additional_writable_roots).clone();
        let mut created: Option<Arc<PermissionEngine>> = None;
        {
            let mut guard = lock(&self.permissions);
            match guard.as_ref() {
                Some(engine) => engine.set_rules(rules),
                None => {
                    let mut policy =
                        PermissionPolicy::new(PermissionMode::Default, self.cwd.as_str());
                    policy.workspace_roots.extend(additional);
                    policy.rules = rules;
                    let engine = Arc::new(PermissionEngine::new(policy));
                    created = Some(Arc::clone(&engine));
                    *guard = Some(engine);
                }
            }
        }
        // 与 `set_permission_mode` 一致：权限层在 `Core::new` 之后才由规则创建时，
        // 记忆写入门禁仍指向 deny-all，必须重新接线。
        if let Some(engine) = created
            && let Some(memory) = self.memory.as_ref()
        {
            memory.enable_permission_gate(engine, &self.cwd);
        }
    }

    /// 追加权限规则（不覆盖 settings/env 已有的规则）。
    fn add_permission_rules(&self, rules: impl IntoIterator<Item = PermissionRule>) {
        let rules: Vec<PermissionRule> = rules.into_iter().collect();
        if rules.is_empty() {
            return;
        }
        if self.permissions_disabled && lock(&self.permissions).is_none() {
            return;
        }
        let additional = lock(&self.additional_writable_roots).clone();
        let mut created: Option<Arc<PermissionEngine>> = None;
        {
            let mut guard = lock(&self.permissions);
            match guard.as_ref() {
                Some(engine) => engine.add_rules(rules),
                None => {
                    let mut policy =
                        PermissionPolicy::new(PermissionMode::Default, self.cwd.as_str());
                    policy.workspace_roots.extend(additional);
                    policy.rules = rules;
                    let engine = Arc::new(PermissionEngine::new(policy));
                    created = Some(Arc::clone(&engine));
                    *guard = Some(engine);
                }
            }
        }
        if let Some(engine) = created
            && let Some(memory) = self.memory.as_ref()
        {
            memory.enable_permission_gate(engine, &self.cwd);
        }
    }

    /// 追加一个可写工作区根（CLI `--add-dir`）。
    fn add_permission_directory(&self, path: impl Into<PathBuf>) {
        self.add_permission_directories([path.into()]);
    }

    /// 批量追加可写工作区根；只触发一次工具重建。
    fn add_permission_directories(&self, paths: impl IntoIterator<Item = PathBuf>) {
        // 相对路径统一按会话 cwd 解析：权限引擎用会话 cwd，而沙箱的
        // `canonical_or_lexical` 用进程 cwd；不归一化时两层会指向不同目录。
        let cwd = Path::new(&self.cwd);
        let mut normalized: Vec<PathBuf> = Vec::new();
        for path in paths {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            if !normalized.contains(&path) {
                normalized.push(path);
            }
        }
        if normalized.is_empty() {
            return;
        }
        let mut changed = false;
        {
            let mut extra = lock(&self.additional_writable_roots);
            for path in &normalized {
                if !extra.contains(path) {
                    extra.push(path.clone());
                    changed = true;
                }
            }
        }
        {
            let guard = lock(&self.permissions);
            if let Some(engine) = guard.as_ref() {
                for path in &normalized {
                    engine.add_workspace_root(path.clone());
                }
            }
            // 权限层关闭时不创建引擎（那等于重新启用）；额外根由
            // `refresh_execution_policy` 直接并入沙箱策略。
        }
        if !changed {
            return;
        }
        // addDirs 只在 workspace-write 策略下改变可写根；README 的 `--add-dir` 语义
        // 要求工具立即看到新的根，所以同样重建定义。
        self.refresh_execution_policy_and_tools();
    }

    /// 权限检查用的参数：bash 工具实际执行的是 `prefix + "\n" + command`
    /// （`ShellExecute`），权限层必须看到同一份文本，否则前缀里的危险命令不会被检查，
    /// 审批 key 也与实际执行不一致。
    fn permission_args(&self, tool_name: &str, builtin: bool, args: JsonValue) -> JsonValue {
        if builtin
            && tool_name == "bash"
            && let Some(prefix) = self.settings_manager.get_shell_command_prefix()
            && !prefix.is_empty()
        {
            return with_shell_command_prefix(&args, &prefix);
        }
        args
    }

    /// 直接执行 bash（`execute_bash` / RPC `user_bash`）的权限检查。
    ///
    /// 返回拒绝/未确认时的提示文本。
    async fn bash_refusal(&self, command: &str) -> Option<String> {
        let engine = lock(&self.permissions).clone()?;
        let args = JsonValue::Object(
            [(
                "command".to_string(),
                JsonValue::String(command.to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let request = PermissionRequest::from_tool_call("bash", &args, self.cwd.as_str());
        self.refusal_for(&engine, &request, false).await
    }

    /// 权限引擎的共用决策（含 PermissionDenied hook 接线）。
    ///
    /// `Allow` 返回 `None`；`Deny` 返回拒绝原因并发出 `permission_denied`；`Ask` 通过
    /// 结构化审批（[`ApprovalDecision`]）询问用户。没有交互式 UI 时 fail-closed。
    ///
    /// 拒绝原因产生后运行 `PermissionDenied` hook：`retry: true` 只追加“可以重试”的
    /// 提示，feedback 追加为模型可见文本；**绝不**把 deny 改成 allow，也不会自动重放
    /// 工具调用。
    async fn refusal_for(
        &self,
        engine: &PermissionEngine,
        request: &PermissionRequest,
        hook_allowed: bool,
    ) -> Option<String> {
        let reason = self
            .permission_refusal_inner(engine, request, hook_allowed)
            .await?;
        let Some(outcome) = self
            .permission_denied_hook(&request.tool_name, &request.args, &reason)
            .await
        else {
            return Some(reason);
        };
        let mut message = reason;
        if outcome.retry == Some(true) {
            message.push_str(
                "\nA PermissionDenied hook indicates this action may be retried once with different arguments.",
            );
        }
        for feedback in outcome.model_feedback {
            message.push('\n');
            message.push_str(&feedback);
        }
        Some(message)
    }

    async fn permission_refusal_inner(
        &self,
        engine: &PermissionEngine,
        request: &PermissionRequest,
        hook_allowed: bool,
    ) -> Option<String> {
        let target = request.target_summary();
        // 顺序：引擎（deny 永远优先）→ 技能激活只把 `Ask` 放行 → MCP annotations
        // 只对 MCP 工具做额外收紧/read-only 放宽。任何一层都不会把 deny 改成 allow。
        let decision = match lock(&self.skill_tool).as_ref() {
            Some(tool) => {
                let now = now_utc();
                tool.activations().prune_expired(now);
                evaluate_with_activations(engine, tool.activations(), request, now)
            }
            None => engine.evaluate(request),
        };
        let decision = if request.action == kiana_permissions::ActionKind::Mcp {
            let annotations = self.tool_annotations(&request.tool_name);
            let explicit_deny = engine.policy().rules.iter().any(|rule| {
                rule.decision() == kiana_permissions::RuleDecision::Deny && rule.matches(request)
            });
            apply_mcp_annotations(annotations.as_ref(), engine.mode(), decision, explicit_deny)
        } else {
            decision
        };
        // PreToolUse hook 的 `permissionDecision: allow` 只跳过“询问”，不能覆盖 deny：
        // 引擎/规则/annotations 已经算出 Deny 时仍走下面的拒绝分支。
        if hook_allowed && matches!(decision, PermissionDecision::Ask { .. }) {
            return None;
        }
        match decision {
            PermissionDecision::Allow { .. } => None,
            PermissionDecision::Deny { reason } => {
                self.emit(AgentSessionEvent::PermissionDenied {
                    tool_name: request.tool_name.clone(),
                    reason: reason.clone(),
                    target,
                });
                Some(reason)
            }
            PermissionDecision::Ask { prompt, reason } => {
                self.emit(AgentSessionEvent::PermissionRequested {
                    tool_name: request.tool_name.clone(),
                    action: request.action.as_str().to_string(),
                    summary: request.redacted_args_summary(),
                    target: target.clone(),
                    reason: reason.clone(),
                });
                let ui_context = lock(&self.extension_ui_context).clone();
                let Some(ui_context) = ui_context else {
                    let message = format!(
                        "{prompt}\n\nNo interactive UI is available to approve this action."
                    );
                    // 事件里只用决策原因（不含完整命令/参数），完整提示只回传给调用方。
                    self.emit(AgentSessionEvent::PermissionDenied {
                        tool_name: request.tool_name.clone(),
                        reason: format!(
                            "{reason} No interactive UI is available to approve this action."
                        ),
                        target,
                    });
                    return Some(message);
                };
                // 第 12 周：per-tool 审批卡。默认 trait 实现退化为 select；TUI 覆盖后
                // 可以拿到完整 args 渲染 diff/命令/域名。`args` 只存在于这个请求里，
                // 不进入 `PermissionRequested` 事件。
                // destructive/open-world MCP 工具只能“本次批准”，与 `persist_approval` 的
                // 规则一致；其余工具提供三个持久作用域。`.git`/`.kiana` 安全确认属于
                // “必须每次重新询问”的类别（引擎会先于任何批准返回 Ask），因此不提供
                // 「总是允许」——否则用户会以为规则已记住，但重启/下一次仍然询问。
                let annotations = self.tool_annotations(&request.tool_name);
                let always_scopes = if allows_persistent_approval(annotations.as_ref())
                    && engine.approval_can_persist(request)
                {
                    vec![
                        PermissionScopeKind::Session,
                        PermissionScopeKind::Project,
                        PermissionScopeKind::Local,
                    ]
                } else {
                    Vec::new()
                };
                let card_request =
                    build_permission_card_request(request, reason.clone(), always_scopes);
                let decision =
                    dispatch_local(async move { ui_context.permission_card(card_request).await })
                        .await
                        .flatten()
                        .map(permission_card_choice_to_decision)
                        .unwrap_or_else(|| ApprovalDecision::Reject {
                            reason: Some("Approval was cancelled.".to_string()),
                        });
                self.emit(AgentSessionEvent::PermissionDecided {
                    tool_name: request.tool_name.clone(),
                    decision: decision.as_str().to_string(),
                    scope: decision.scope().map(|scope| scope.as_str().to_string()),
                    reason: decision.reject_reason().map(str::to_string),
                    target: target.clone(),
                });
                match decision {
                    ApprovalDecision::AllowOnce => None,
                    ApprovalDecision::AllowAlways { scope } => {
                        match self.persist_approval(engine, request, scope) {
                            Ok(()) => None,
                            Err(error) => {
                                self.emit(AgentSessionEvent::PermissionDenied {
                                    tool_name: request.tool_name.clone(),
                                    reason: error.clone(),
                                    target,
                                });
                                Some(error)
                            }
                        }
                    }
                    ApprovalDecision::Reject { reason } => {
                        let message = match reason.as_deref().map(str::trim) {
                            Some(reason) if !reason.is_empty() => {
                                format!("Permission denied by the user: {reason}")
                            }
                            _ => "Permission denied by the user.".to_string(),
                        };
                        self.emit(AgentSessionEvent::PermissionDenied {
                            tool_name: request.tool_name.clone(),
                            reason: message.clone(),
                            target,
                        });
                        Some(message)
                    }
                }
            }
        }
    }

    /// 把 `AllowAlways` 写入对应作用域，并把规则立即加入当前引擎。
    ///
    /// `Deny` 规则仍优先（引擎的匹配顺序不变），所以这里只生成 `Allow` 规则。
    fn persist_approval(
        &self,
        engine: &PermissionEngine,
        request: &PermissionRequest,
        scope: ApprovalScope,
    ) -> Result<(), String> {
        // MCP annotations 是不可信提示：destructive/open-world 工具只允许“本次批准”，
        // 不能写成 session/local/user 规则。非 MCP 工具没有 annotations，恒为允许。
        let annotations = self.tool_annotations(&request.tool_name);
        if !allows_persistent_approval(annotations.as_ref()) {
            return Err(
                "This MCP tool declares destructive/open-world behavior; approve it once per call."
                    .to_string(),
            );
        }
        // 安全确认（`.git`/`.kiana` 写入）每次都必须重新询问；即使 UI 客户端无视
        // `always_scopes` 直接返回 AllowAlways，也在这里 fail-closed。
        if !engine.approval_can_persist(request) {
            return Err(
                "This action writes to a protected .git/.kiana path; approve it once per call."
                    .to_string(),
            );
        }
        let decision = ApprovalDecision::AllowAlways { scope };
        let rules = decision.rules_for(request);
        if rules.is_empty() {
            return Err("Could not derive a permission rule for this action.".to_string());
        }
        match scope {
            ApprovalScope::Session => {
                engine.record_session_allow(request);
                return Ok(());
            }
            ApprovalScope::Local => {
                // 未受信项目不会加载 Local 层；写进去只会「报告成功、重启后失效」。
                // 与项目设置写入口径一致：直接报错，让调用方 fail-closed。
                if !self.settings_manager.is_project_trusted() {
                    return Err(
                        kiana_config::settings::PROJECT_NOT_TRUSTED_WRITE_MESSAGE.to_string()
                    );
                }
                let path =
                    kiana_config::settings::permissions::local_settings_path(Path::new(&self.cwd));
                for rule in &rules {
                    kiana_config::settings::permissions::append_permission_rule(
                        &path,
                        &rule.to_settings_value(),
                    )
                    .map_err(|error| {
                        format!("Could not save the local permission rule: {error}")
                    })?;
                }
            }
            ApprovalScope::Project => {
                let values = rules
                    .iter()
                    .map(PermissionRule::to_settings_value)
                    .collect::<Vec<_>>();
                let mut append_error = None;
                self.settings_manager
                    .update_project_permissions(|current| {
                        // 已有的 `permissions` 类型不对时不要覆盖用户配置：保留原值并把
                        // 错误交给调用方，让本次调用 fail-closed。
                        if let Some(value) = current
                            && !value.is_object()
                        {
                            append_error =
                                Some("project permissions must be a JSON object".to_string());
                            return value.clone();
                        }
                        let mut permissions = current
                            .cloned()
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                        for value in &values {
                            if let Err(error) =
                                kiana_config::settings::permissions::append_rule_to_permissions(
                                    &mut permissions,
                                    value,
                                )
                            {
                                append_error = Some(error.to_string());
                            }
                        }
                        permissions
                    })
                    .map_err(|error| {
                        format!("Could not save the project permission rule: {error}")
                    })?;
                if let Some(error) = append_error {
                    return Err(format!(
                        "Could not save the project permission rule: {error}"
                    ));
                }
            }
        }
        engine.add_rules(rules);
        Ok(())
    }

    /// 扩展层的 `tool_call` 钩子。
    async fn extension_before_tool_call(
        &self,
        context: &BeforeToolCallContext,
        parent_tool_call_id: Option<String>,
    ) -> Option<BeforeToolCallResult> {
        let runner = self.runner()?;
        if !runner.has_handlers("tool_call") {
            return None;
        }
        let input = context.args.get();
        let input = if input.is_object() {
            input
        } else {
            serde_json::Value::Object(Default::default())
        };
        let mut value = serde_json::json!({
            "type": "tool_call",
            "toolName": &context.tool_call.name,
            "toolCallId": &context.tool_call.id,
            "input": input,
        });
        if let Some(parent) = &parent_tool_call_id
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "parentToolCallId".to_string(),
                serde_json::Value::String(parent.clone()),
            );
        }
        let mut event = tool_call_event_from_json(value)?;
        match runner.emit_tool_call(&mut event).await {
            Ok(result) => {
                // handler 的就地修改写回共享参数（TS 的 event.input 就是同一个对象）。
                if let Ok(serde_json::Value::Object(object)) = serde_json::to_value(&event)
                    && let Some(input) = object.get("input")
                {
                    context.args.set(input.clone());
                }
                result.map(|result| BeforeToolCallResult {
                    block: result.block,
                    reason: result.reason,
                    terminate: result.terminate,
                })
            }
            // TS `_beforeToolCall`：Error 实例原样 rethrow（agent-loop 取 `err.message`），
            // 只有非 Error 的抛出值才加 "Extension failed, blocking execution: " 前缀。
            Err(error) => Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some(match &error {
                    kiana_extensions::runner::DispatchError::Js { message, .. }
                        if js_thrown_value_is_error(&error) =>
                    {
                        message.clone()
                    }
                    other => format!("Extension failed, blocking execution: {}", other.message()),
                }),
                terminate: Some(false),
            }),
        }
    }

    async fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        parent_tool_call_id: Option<String>,
    ) -> Option<AfterToolCallResult> {
        // 第 11 周后半：先按运行时看到的完整结果采集验收证据并记录 agent 写入，
        // 再做 PostToolUse / 扩展钩子（钩子只影响反馈，不能撤销副作用）。
        self.record_tool_effects(&context);
        // PostToolUse / PostToolUseFailure：在扩展 `tool_result` 之前看到执行结果，
        // 其结果只做反馈合并（不能撤销已执行的副作用）。
        let post_hook = self.post_tool_use_hook(&context).await;
        let runner = self.runner();
        let hook_result = match &runner {
            Some(runner) if runner.has_handlers("tool_result") => {
                let mut value = serde_json::json!({
                    "type": "tool_result",
                    "toolName": context.tool_call.name,
                    "toolCallId": context.tool_call.id,
                    "input": context.args,
                    "content": context.result.content,
                    "isError": context.is_error,
                });
                if let Some(object) = value.as_object_mut() {
                    if let Some(parent) = &parent_tool_call_id {
                        object.insert(
                            "parentToolCallId".to_string(),
                            serde_json::Value::String(parent.clone()),
                        );
                    }
                    if !context.result.details.is_null() {
                        object.insert("details".to_string(), context.result.details.clone());
                    }
                    if let Some(structured) = &context.result.structured_content {
                        object.insert("structuredContent".to_string(), structured.clone());
                    }
                    if let Some(usage) = &context.result.usage {
                        object.insert(
                            "usage".to_string(),
                            serde_json::to_value(usage).unwrap_or(serde_json::Value::Null),
                        );
                    }
                }
                match tool_result_event_from_json(value) {
                    Some(event) => runner.emit_tool_result(event).await.ok().flatten(),
                    None => None,
                }
            }
            _ => None,
        };

        let mut content = match &hook_result {
            Some(result) => match &result.content {
                kiana_ext_api::events::OptionalPatch::Value(content) => content.clone(),
                _ => context.result.content.clone(),
            },
            None => context.result.content.clone(),
        };
        let mut post_hook_denied = false;
        let mut post_hook_terminate = false;
        let mut hook_added = false;
        if let Some(outcome) = &post_hook {
            post_hook_denied = outcome.is_denied();
            let mut text = String::new();
            if post_hook_denied && let Some(reason) = outcome.blocking_error() {
                text.push_str(reason);
            }
            for feedback in &outcome.model_feedback {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(feedback);
            }
            if let Some(context) = outcome.additional_context_xml() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("<hook-context>");
                text.push_str(&context);
                text.push_str("</hook-context>");
            }
            // PostToolUse 的 `continue: false`：结果已经落盘，不能撤销副作用；把
            // stopReason 作为模型可见文本追加到结果，并让 agent loop 在批次结束后
            // 停止（`terminate` 由 agent-loop 的批次判定消费）。
            if !outcome.continue_execution {
                let reason = outcome
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "Execution was stopped by a PostToolUse hook".to_string());
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&reason);
                post_hook_terminate = true;
                self.agent_run_abort_requested.store(true, Ordering::SeqCst);
            }
            if !text.is_empty() {
                hook_added = true;
                content.push(text_content(text));
            }
        }
        let resize_options = self
            .limits_model()
            .and_then(|model| model.input_limits)
            .and_then(|limits| limits.images)
            .and_then(|images| images.resize);
        let normalized = kiana_core::image_process::normalize_tool_result_images(
            &content,
            Some(
                kiana_core::image_process::NormalizeToolResultImagesOptions {
                    auto_resize_images: Some(self.settings_manager.get_image_auto_resize()),
                    resize_options,
                },
            ),
        );
        let changed = matches!(normalized, std::borrow::Cow::Owned(_));
        if hook_result.is_none()
            && !changed
            && !hook_added
            && !post_hook_denied
            && !post_hook_terminate
        {
            return None;
        }
        let normalized = normalized.into_owned();
        Some(AfterToolCallResult {
            content: if hook_result.is_some() || changed || hook_added || post_hook_denied {
                Some(normalized)
            } else {
                None
            },
            details: hook_result
                .as_ref()
                .and_then(|result| match &result.details {
                    kiana_ext_api::events::OptionalPatch::Value(value) => Some(value.clone()),
                    _ => None,
                }),
            structured_content: match &hook_result {
                Some(result) => match &result.structured_content {
                    kiana_ext_api::events::OptionalPatch::Value(value) => Some(value.clone()),
                    _ => None,
                },
                None => context.result.structured_content.clone(),
            },
            is_error: Some(
                hook_result
                    .as_ref()
                    .and_then(|result| match &result.is_error {
                        kiana_ext_api::events::OptionalPatch::Value(value) => Some(*value),
                        // TS `hookResult?.isError ?? isError`：null 与缺省都回退到执行结果。
                        _ => None,
                    })
                    .unwrap_or(context.is_error)
                    || post_hook_denied,
            ),
            usage: hook_result.as_ref().and_then(|result| match &result.usage {
                kiana_ext_api::events::OptionalPatch::Value(value) => Some(value.clone()),
                _ => None,
            }),
            terminate: post_hook_terminate.then_some(true),
        })
    }

    // -- 模型辅助 -----------------------------------------------------------

    fn current_model(&self) -> Option<Model> {
        self.agent.state().model
    }

    fn routed_model(&self) -> Option<RoutedModel> {
        let model = self.current_model()?;
        if !is_virtual_model(&model) {
            return None;
        }
        let latest = find_latest_response(&self.agent.messages())?;
        let physical = self
            .model_runtime
            .get_physical_model(&latest.provider, &latest.model)?;
        Some(RoutedModel {
            model: physical,
            thinking_level: latest.thinking_level,
        })
    }

    fn limits_model(&self) -> Option<Model> {
        self.routed_model()
            .map(|result| result.model)
            .or_else(|| self.current_model())
    }

    /// 当前 assistant 消息所属的模型（虚拟选择下是产生它的物理模型）。
    fn model_for_message(&self, message: &AssistantMessage) -> Option<Model> {
        let model = self.current_model()?;
        if is_virtual_model(&model) {
            return self
                .model_runtime
                .get_physical_model(&message.provider, &message.model);
        }
        if model.provider == message.provider && model.id == message.model {
            Some(model)
        } else {
            None
        }
    }

    /// 在分支隐含另一个选择时把当前选择记到分支上。
    ///
    /// TS 的 `_recordSelection()` 同步写会话文件，失败会 reject `_startRun`。
    fn record_selection(&self) -> Result<(), AgentSessionError> {
        let Some(model) = self.current_model() else {
            return Ok(());
        };
        let recorded = {
            let runtime = self.model_runtime.clone();
            get_branch_selection(&self.branch(), &move |provider: &str, model_id: &str| {
                runtime.get_model(provider, model_id)
            })
        };
        let Some(recorded) = recorded else {
            return Ok(());
        };
        if recorded.provider == model.provider && recorded.model_id == model.id {
            return Ok(());
        }
        let recorded_model = self
            .model_runtime
            .get_model(&recorded.provider, &recorded.model_id);
        if !is_virtual_model(&model) && !recorded_model.as_ref().is_some_and(is_virtual_model) {
            return Ok(());
        }
        self.with_session_mut(|manager| manager.append_model_change(&model.provider, &model.id))
            .map_err(|error| AgentSessionError::other(error.to_string()))?;
        Ok(())
    }

    fn exceeds_compaction_threshold(&self, model: &Model, projection: &SessionProjection) -> bool {
        let context_window = model.context_window.unwrap_or(0);
        if context_window == 0 {
            return false;
        }
        let settings = self.compaction_settings(Some(model));
        let branch = self.branch();
        should_compact(
            estimate_projected_context_tokens(projection, &branch).tokens,
            context_window,
            &settings,
        )
    }

    fn compaction_settings(&self, model: Option<&Model>) -> CompactionSettings {
        let resolved = self
            .settings_manager
            .get_compaction_settings(model.map(CompactionModelRef::from_model));
        match resolved {
            Ok(settings) => CompactionSettings {
                enabled: settings.enabled,
                reserve_tokens: settings.reserve_tokens as u64,
                keep_recent_tokens: settings.keep_recent_tokens as u64,
            },
            Err(_) => CompactionSettings::default(),
        }
    }

    fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        let context_window = self
            .model_for_message(message)
            .or_else(|| self.current_model())
            .and_then(|model| model.context_window)
            .unwrap_or(0);
        assistant_error_is_retryable(message, context_window)
    }

    /// 把压缩后的 hidden context 注入本次请求副本。
    ///
    /// 只对请求副本生效（不持久化、不是 tool result）；如果加上附件后估算超过模型窗口，
    /// 直接丢弃附件——恢复信息是增强，不应把本来能发的请求推成 overflow。
    fn attach_recovery_context(&self, context: &mut AgentContext, model: Option<&Model>) {
        let Some(attachment) = lock(&self.pending_recovery_attachment).take() else {
            return;
        };
        // 脚本化测试 provider 的窗口只驱动阈值判定，不执行 provider 侧预算；注入附件
        // 会改变脚本轨迹的 usage/overflow 判定。真实 provider 才注入。
        if model.is_some_and(|model| model.provider == kiana_ai::providers::faux::DEFAULT_PROVIDER)
        {
            return;
        }
        let mut candidate = context.clone();
        candidate
            .messages
            .push(AgentMessage::Standard(Message::System(SystemMessage {
                role: SystemRole::System,
                content: SystemContent::Text(attachment),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: now_ms(),
            })));
        if let Some(model) = model {
            let transcript = transcript_for_context(&candidate);
            if precheck_input_with_defaults(&transcript, model).is_err() {
                return;
            }
        }
        *context = candidate;
    }

    /// 把 hook 的 `additionalContext` 注入本次请求副本。
    ///
    /// 与压缩恢复附件一样只对请求副本生效（不写会话）；每个条目已经由
    /// [`Core::queue_hook_context`] 包好 `<hook-context>` 标签并做过 XML 转义。
    fn attach_hook_contexts(&self, context: &mut AgentContext) {
        for text in self.take_hook_contexts() {
            context
                .messages
                .push(AgentMessage::Standard(Message::System(SystemMessage {
                    role: SystemRole::System,
                    content: SystemContent::Text(text),
                    sections: None,
                    tools_added: None,
                    tools_removed: None,
                    timestamp: now_ms(),
                })));
        }
    }

    /// 请求前输入预检（第 9 周）：估算放不进窗口时先压缩一次，仍放不进则返回 typed
    /// `ContextWindowExceeded`，由流函数入口在下一次 stream 调用时失败（零 provider 请求）。
    async fn ensure_request_fits_context(
        &self,
        model: &Model,
        context: &mut AgentContext,
    ) -> Option<kiana_ai::AiError> {
        // faux provider 的 context_window 只用于驱动会话阈值测试，provider 本身不执行
        // 窗口限制；硬失败会改变脚本化轨迹的语义。非 faux provider（含测试用的独立
        // mock provider）仍然零请求失败。
        if model.provider == kiana_ai::providers::faux::DEFAULT_PROVIDER {
            return None;
        }
        let transcript = transcript_for_context(context);
        let error = match precheck_input_with_defaults(&transcript, model) {
            Ok(_) => return None,
            Err(error) => error,
        };
        let settings = self.compaction_settings(Some(model));
        if !settings.enabled {
            // 不能把已知超窗的请求发出去：压缩关闭时直接返回 typed 错误。
            return Some(error.into());
        }
        // 没有任何可压缩内容时同样不能发：保留原历史并把 typed 错误交给流函数入口。
        let branch = self.branch();
        let (_, preparation) = prepare_with_pruning(&branch, &settings, &self.plan_prune_config());
        if preparation.is_none() {
            return Some(error.into());
        }
        self.run_auto_compaction(CompactionReason::Overflow, false)
            .await;
        context.messages = self.projection().messages;
        let transcript = transcript_for_context(context);
        match precheck_input_with_defaults(&transcript, model) {
            Ok(_) => None,
            Err(error) => Some(error.into()),
        }
    }

    // -- prepareRequest 投影 -------------------------------------------------

    /// `prepareRequest` 钩子：每次 provider 请求前重建 canonical context 并路由模型。
    /// 非虚拟模型直接透传（先跑前一层 prepare 钩子）；虚拟选择先经 `resolve_model`
    /// 路由到物理模型（路由失败没有钩子错误通道，记入 `pending_stream_error` 由流函数
    /// 包装在下次 stream 调用时抛出），状态变化落盘为 `VIRTUAL_MODEL_STATE_ENTRY` 条目，
    /// 超过压缩阈值时先自动压缩再用新投影重建 context。
    async fn prepare_request(
        &self,
        request: kiana_agent_core::types::PrepareRequestContext,
        signal: Option<CancellationToken>,
        previous_prepare: Option<kiana_agent_core::types::PrepareRequestFn>,
    ) -> Option<kiana_agent_core::types::AgentRequestUpdate> {
        // 第 6 周：请求前把过旧的聚合 tool result 清成 artifact 占位符（保留 tool pair）。
        self.enforce_aggregate_tool_result_budget();
        let failed = lock(&self.failed_response).take();
        let prepare = |_context: AgentContext,
                       signal: Option<CancellationToken>,
                       failed: Option<AssistantMessage>| {
            let previous = previous_prepare.clone();
            async move {
                let projection = self.projection();
                let canonical_context = AgentContext {
                    messages: projection.messages.clone(),
                    tools: Some(self.agent.tools()),
                };
                let previous = match &previous {
                    Some(previous) => {
                        previous(
                            kiana_agent_core::types::PrepareRequestContext {
                                context: canonical_context.clone(),
                                model: self.agent.model(),
                                thinking_level: self.agent.thinking_level(),
                            },
                            signal,
                        )
                        .await
                    }
                    None => None,
                };
                let context = previous
                    .as_ref()
                    .and_then(|update| update.context.clone())
                    .unwrap_or(canonical_context);
                let model = previous
                    .as_ref()
                    .and_then(|update| update.model.clone())
                    .or_else(|| self.current_model());
                let thinking_level = previous
                    .as_ref()
                    .and_then(|update| update.thinking_level)
                    .unwrap_or_else(|| self.agent.thinking_level());
                (previous, context, projection, model, thinking_level, failed)
            }
        };
        let (previous, mut context, projection, model, thinking_level, failed) =
            prepare(request.context, signal.clone(), failed).await;
        // 压缩后的 hidden context：作为 system 消息注入本次请求副本，不写入会话、
        // 不伪造 tool result；放不进模型窗口时丢弃（避免恢复附件本身触发 overflow）。
        // 虚拟模型要等路由到物理模型后再按实际窗口判断。
        if model.as_ref().is_none_or(|model| !is_virtual_model(model)) {
            self.attach_hook_contexts(&mut context);
            self.attach_recovery_context(&mut context, model.as_ref());
        }
        let Some(model) = model else {
            return previous;
        };
        if !is_virtual_model(&model) {
            // 第 9 周输入预检：超窗时先压缩，仍超窗则让本次请求在流函数入口失败（零请求）。
            if let Some(error) = self.ensure_request_fits_context(&model, &mut context).await {
                *lock(&self.pending_stream_error) = Some(error);
            }
            return Some(kiana_agent_core::types::AgentRequestUpdate {
                context: Some(context),
                model: Some(model),
                thinking_level: Some(thinking_level),
            });
        }

        // 虚拟选择：只为本次请求路由到物理模型。
        let last_response = context
            .messages
            .iter()
            .rposition(|message| message.role() == "assistant");
        let user_turn = context
            .messages
            .iter()
            .skip(last_response.map(|index| index + 1).unwrap_or(0))
            .any(|message| message.role() == "user");
        let state = get_virtual_model_state(&self.branch(), &model.provider, &model.id);
        let route = match self
            .model_runtime
            .resolve_model(
                &model,
                &convert_to_llm(&context.messages),
                ResolveModelOptions {
                    reason: if failed.is_some() {
                        ModelRouteReason::Retry
                    } else if user_turn {
                        ModelRouteReason::User
                    } else {
                        ModelRouteReason::Continuation
                    },
                    thinking_level,
                    signal: signal.as_ref().map(abort_scope_from_token),
                    failed: failed.clone(),
                    state: state.clone(),
                },
            )
            .await
        {
            Ok(route) => route,
            Err(error) => {
                // TS 里路由失败会 reject 整个请求 → run 以错误 assistant 消息结束。
                // Rust 钩子没有错误通道：由流函数包装在下一次 stream 调用时返回该错误。
                *lock(&self.pending_stream_error) = Some(kiana_ai::AiError::new(
                    kiana_ai::AiErrorKind::Other,
                    error.message(),
                ));
                return Some(kiana_agent_core::types::AgentRequestUpdate {
                    context: Some(context),
                    model: Some(model),
                    thinking_level: Some(thinking_level),
                });
            }
        };
        if route.state.is_some() && route.state != state {
            let data = VirtualModelStateData {
                provider: model.provider.clone(),
                model_id: model.id.clone(),
                state: route.state.clone(),
            };
            let entry_id = self.with_session_mut(|manager| {
                manager.append_custom_entry(
                    VIRTUAL_MODEL_STATE_ENTRY,
                    Some(serde_json::to_value(&data).unwrap_or(serde_json::Value::Null)),
                )
            });
            match entry_id {
                Ok(entry_id) => {
                    if let Some(entry) =
                        self.with_session(|manager| manager.get_entry(&entry_id).cloned())
                    {
                        self.emit(AgentSessionEvent::EntryAppended { entry });
                    }
                }
                // TS 的 `appendCustomEntry` 会抛错并让 prepareRequest 失败；钩子没有错误
                // 通道，记录后由 `_startRun` 在 run 结束时 reject。
                Err(error) => self.record_persistence_error(&error),
            }
        }
        if self.exceeds_compaction_threshold(&route.model, &projection) {
            self.run_auto_compaction(CompactionReason::Threshold, false)
                .await;
            let (_, new_context, _, _, _, _) = prepare(context.clone(), signal.clone(), None).await;
            context = new_context;
        }
        self.attach_hook_contexts(&mut context);
        self.attach_recovery_context(&mut context, Some(&route.model));
        if let Some(error) = self
            .ensure_request_fits_context(&route.model, &mut context)
            .await
        {
            *lock(&self.pending_stream_error) = Some(error);
        }
        Some(kiana_agent_core::types::AgentRequestUpdate {
            context: Some(context),
            model: Some(route.model),
            thinking_level: Some(route.thinking_level),
        })
    }

    async fn compact_before_next_assistant_response(&self, context: AgentContext) -> AgentContext {
        let projection = self.projection();
        let model = self.current_model();
        let Some(model) = model else {
            return AgentContext {
                messages: projection.messages,
                ..context
            };
        };
        if is_virtual_model(&model) || !self.exceeds_compaction_threshold(&model, &projection) {
            return AgentContext {
                messages: projection.messages,
                ..context
            };
        }
        self.run_auto_compaction(CompactionReason::Threshold, false)
            .await;
        AgentContext {
            messages: self.projection().messages,
            ..context
        }
    }

    // -- 压缩 ---------------------------------------------------------------

    fn estimate_messages_tokens(&self) -> u64 {
        estimate_messages_tokens(&self.projection().messages)
    }

    async fn summarization_request_auth(
        &self,
        selected_model: Model,
        signal: Option<CancellationToken>,
    ) -> Result<SummarizationAuth, AgentSessionError> {
        // 虚拟模型先路由：摘要的输入/输出按实际模型计算。
        let (model, thinking_level) = if is_virtual_model(&selected_model) {
            let route = self
                .model_runtime
                .resolve_model(
                    &selected_model,
                    &convert_to_llm(&self.agent.messages()),
                    ResolveModelOptions {
                        reason: ModelRouteReason::Direct,
                        thinking_level: self.agent.thinking_level(),
                        signal: signal.as_ref().map(abort_scope_from_token),
                        failed: None,
                        state: None,
                    },
                )
                .await
                .map_err(|error| AgentSessionError::other(error.message()))?;
            (route.model, route.thinking_level)
        } else {
            (selected_model, self.agent.thinking_level())
        };
        // TS：只有 agent 的 streamFunction 是内建 `streamSimple` 时才强制认证；自定义
        // 流函数自行处理认证，`getAuth` 缺失或失败（且未 abort）时用裸模型继续。
        let current_stream = self.agent.stream_function();
        let builtin_stream = if Arc::ptr_eq(&current_stream, &self.standard_stream_function) {
            // 宿主没有替换流函数：按构造时的原始函数判断。
            self.builtin_stream_function
        } else {
            // 宿主在构造后换过流函数（SDK/嵌入场景）：TS 的 `streamFunction === streamSimple`
            // 此时看的是新函数本身。
            Arc::ptr_eq(&current_stream, &stream_simple_stream_fn())
        };
        let auth = if builtin_stream {
            self.required_request_auth(&model, signal.clone()).await?
        } else {
            match self
                .model_runtime
                .get_auth_for_model(
                    &model,
                    ModelRuntimeAuthOverrides {
                        signal: signal.as_ref().map(abort_scope_from_token),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(Some(result)) => {
                    let request_model = match &result.auth.base_url {
                        Some(base_url) => {
                            let mut request_model = model.clone();
                            request_model.base_url = base_url.clone();
                            request_model
                        }
                        None => model.clone(),
                    };
                    Some(RequestAuth {
                        model: request_model,
                        api_key: result.auth.api_key,
                        headers: without_deleted_headers(result.auth.headers),
                        env: result.env,
                    })
                }
                Ok(None) => None,
                Err(error) => {
                    if signal.as_ref().is_some_and(|token| token.is_cancelled()) {
                        return Err(AgentSessionError::other(error.message.clone()));
                    }
                    None
                }
            }
        };
        match auth {
            Some(auth) => Ok(SummarizationAuth {
                model: auth.model,
                api_key: auth.api_key,
                headers: auth.headers,
                env: auth.env,
                thinking_level,
            }),
            None => Ok(SummarizationAuth {
                model,
                api_key: None,
                headers: None,
                env: None,
                thinking_level,
            }),
        }
    }

    async fn required_request_auth(
        &self,
        model: &Model,
        signal: Option<CancellationToken>,
    ) -> Result<Option<RequestAuth>, AgentSessionError> {
        let result = match self
            .model_runtime
            .get_auth_for_model(
                model,
                ModelRuntimeAuthOverrides {
                    signal: signal.as_ref().map(abort_scope_from_token),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                // TS `_getRequiredRequestAuth` 的 catch：`error.cause.message` 是
                // `authHeader requires a resolved API key` 时改抛
                // `formatNoApiKeyFoundMessage(provider)`（含登录帮助）。
                if let Some(details) = &error.details
                    && let Some(cause) = &details.cause
                    && cause.message == "authHeader requires a resolved API key"
                {
                    return Err(AgentSessionError::NoApiKey {
                        provider: model.provider.clone(),
                    });
                }
                return Err(AgentSessionError::other(error.message.clone()));
            }
        };
        if let Some(result) = result
            && (result.auth.api_key.is_some() || result.auth.headers.is_some())
        {
            let request_model = match &result.auth.base_url {
                Some(base_url) => {
                    let mut request_model = model.clone();
                    request_model.base_url = base_url.clone();
                    request_model
                }
                None => model.clone(),
            };
            return Ok(Some(RequestAuth {
                model: request_model,
                api_key: result.auth.api_key,
                headers: without_deleted_headers(result.auth.headers),
                env: result.env,
            }));
        }
        if self.model_runtime.is_using_oauth(&model.provider) {
            return Err(AgentSessionError::AuthenticationFailed {
                provider: model.provider.clone(),
            });
        }
        Err(AgentSessionError::NoApiKey {
            provider: model.provider.clone(),
        })
    }

    fn summarization_retry_callbacks(
        &self,
        source: SummarizationRetrySource,
        reason: Option<CompactionReason>,
    ) -> RetryCallbacks {
        let weak_self = lock(&self.self_ref).clone();
        let scheduled_core = weak_self.clone();
        let start_core = weak_self.clone();
        let finish_core = weak_self;
        RetryCallbacks {
            on_retry_scheduled: Some(Arc::new(
                move |attempt, max_attempts, delay_ms, error_message| {
                    if let Some(core) = scheduled_core.upgrade() {
                        core.emit(AgentSessionEvent::SummarizationRetryScheduled {
                            attempt,
                            max_attempts,
                            delay_ms,
                            error_message,
                        });
                    }
                    Box::pin(async {})
                },
            )),
            on_retry_attempt_start: Some(Arc::new(move || {
                if let Some(core) = start_core.upgrade() {
                    core.emit(AgentSessionEvent::SummarizationRetryAttemptStart { source, reason });
                }
                Box::pin(async {})
            })),
            on_retry_finished: Some(Arc::new(move |_success, _attempts, _error| {
                if let Some(core) = finish_core.upgrade() {
                    core.emit(AgentSessionEvent::SummarizationRetryFinished);
                }
                Box::pin(async {})
            })),
        }
    }

    async fn run_default_compaction(
        &self,
        preparation: &CompactionPreparation,
        model: &Model,
        custom_instructions: Option<String>,
        signal: CancellationToken,
        reason: CompactionReason,
    ) -> Result<CompactionResult, AgentSessionError> {
        let request = self
            .summarization_request_auth(model.clone(), Some(signal.clone()))
            .await?;
        let callback_source = match reason {
            CompactionReason::Manual => SummarizationRetrySource::Compaction,
            _ => SummarizationRetrySource::Compaction,
        };
        // 第 6 周：默认摘要路径改用结构化模板（Objective/Details/Completed/Active/
        // Blocked/Next/Files）与 prior-summary 合并规则；旧 `compact` 仍保留给 SDK 导出。
        let options = CompactModelOptions {
            model: request.model.clone(),
            api_key: request.api_key.clone(),
            headers: request.headers.clone(),
            custom_instructions,
            signal: Some(abort_scope_from_token(&signal)),
            thinking_level: Some(request.thinking_level),
            stream_fn: Some(self.agent.stream_function()),
            env: request.env.clone(),
            retry: Some(self.retry_policy()),
            callbacks: Some(self.summarization_retry_callbacks(callback_source, Some(reason))),
            session_id: Some(self.session_id()),
        };
        compact_with_structured_template(preparation, &options)
            .await
            .map_err(|error| AgentSessionError::other(error.to_string()))
    }

    fn retry_policy(&self) -> RetryPolicy {
        let settings = self.settings_manager.get_retry_settings();
        RetryPolicy {
            enabled: settings.enabled,
            max_retries: settings.max_retries.max(0.0) as u64,
            base_delay_ms: settings.base_delay_ms.max(0.0) as u64,
            max_agent_delay_ms: Some(settings.max_agent_delay_ms.max(0.0) as u64),
        }
    }

    fn clear_manual_compaction_state(&self) {
        *lock(&self.compaction_abort) = None;
        self.resolve_idle_wait_if_idle();
    }

    async fn emit_session_compact_failed(&self, event: SessionCompactFailedEvent) {
        if let Some(runner) = self.runner()
            && runner.has_handlers("session_compact_failed")
        {
            let _ = runner
                .emit(ExtensionEvent::Session(SessionEvent::CompactFailed(event)))
                .await;
        }
    }

    /// `_checkCompaction(assistantMessage, skipAbortedCheck)`：对一条 assistant 消息做
    /// 压缩判定。返回是否触发了自动压缩（含压缩后要求重试的情形）。
    ///
    /// 判定顺序（与 TS 的 case 结构一致）：
    /// 1. 设置关闭 / abort 消息（且 `skip_aborted_check`）→ 不压缩；
    /// 2. 消息早于最近一次压缩边界 → 不压缩（旧消息不反复触发）；
    /// 3. 上下文溢出（显式 `StopReason::Error` + overflow，或 usage 匹配投影的隐式溢出）
    ///    与可恢复截断（`Length`）：compact-and-retry 只尝试一次
    ///    （`overflow_recovery_attempted`），重试前用 `omit_recovery_attempt` 把失败消息
    ///    从上下文中省略；
    /// 4. 否则走阈值判定：有 context_edit 时用投影估算，usage 缺失/错误时回退到消息
    ///    估算（且晚于压缩时间才估算），否则用 usage 里的直接 token 数。
    async fn check_compaction(
        &self,
        assistant_message: &AssistantMessage,
        skip_aborted_check: bool,
    ) -> Result<bool, AgentSessionError> {
        let settings = self.compaction_settings(self.current_model().as_ref());
        if !settings.enabled {
            return Ok(false);
        }
        if skip_aborted_check && assistant_message.stop_reason == StopReason::Aborted {
            return Ok(false);
        }
        let message_model = self.model_for_message(assistant_message);
        let same_model = message_model.is_some();
        let fallback_model = self.current_model();
        let context_window = message_model
            .as_ref()
            .or(fallback_model.as_ref())
            .and_then(|model| model.context_window)
            .unwrap_or(0);

        // 早于最近压缩边界的 assistant 消息不再触发压缩。
        let early_branch = self.branch();
        let compaction_entry =
            kiana_session_core::session_manager::get_latest_compaction_entry(&early_branch);
        let assistant_is_from_before_compaction = compaction_entry.as_ref().is_some_and(|entry| {
            assistant_message.timestamp
                <= kiana_ai::model_catalog::js_date_parse(&entry.timestamp).unwrap_or(f64::NAN)
                    as i64
        });
        if assistant_is_from_before_compaction {
            return Ok(false);
        }

        let current_projection = self.projection();
        let assistant_agent_message =
            AgentMessage::Standard(Message::Assistant(assistant_message.clone()));
        let assistant_entry_id = self.find_persisted_message_entry_id(&assistant_agent_message);
        let assistant_is_projected = assistant_entry_id.as_ref().is_none_or(|entry_id| {
            current_projection.entries.iter().any(|entry| {
                entry.source_entry.id() == entry_id
                    && entry
                        .messages
                        .iter()
                        .any(|message| message.role() == "assistant")
            })
        });
        let branch = self.branch();
        let assistant_index = assistant_entry_id
            .as_ref()
            .and_then(|entry_id| branch.iter().position(|entry| entry.id() == entry_id));
        let entries_after_assistant: &[SessionEntry] = match assistant_index {
            Some(index) => &branch[index + 1..],
            None => &[],
        };
        let has_post_assistant_context_edit = entries_after_assistant
            .iter()
            .any(|entry| entry.kind() == "context_edit");
        let latest_assistant_edit = entries_after_assistant
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::ContextEdit(edit)
                    if Some(&edit.target_id) == assistant_entry_id.as_ref() =>
                {
                    Some(edit)
                }
                _ => None,
            })
            .next_back();
        let assistant_retained_for_explicit_recovery = assistant_entry_id.is_none()
            || (!entries_after_assistant
                .iter()
                .any(|entry| entry.kind() == "compaction")
                && latest_assistant_edit.is_none_or(|edit| edit.replacement.is_some()));
        let assistant_usage_matches_projection =
            assistant_is_projected && !has_post_assistant_context_edit;
        let explicit_overflow = assistant_message.stop_reason == StopReason::Error
            && is_context_overflow(assistant_message, Some(context_window));
        let context_overflow = same_model
            && ((explicit_overflow && assistant_retained_for_explicit_recovery)
                || (assistant_usage_matches_projection
                    && is_context_overflow(assistant_message, Some(context_window))));
        let recoverable_length = same_model
            && assistant_is_projected
            && message_model.as_ref().is_some_and(|model| {
                is_recoverable_length(assistant_message, model.max_tokens.unwrap_or(0) as i64)
            });
        if context_overflow || recoverable_length {
            let will_retry = assistant_message.stop_reason != StopReason::Stop;
            if !will_retry {
                return Ok(self
                    .run_auto_compaction(CompactionReason::Overflow, false)
                    .await);
            }
            if self.overflow_recovery_attempted.load(Ordering::SeqCst) {
                let error_message = if context_overflow {
                    "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_string()
                } else {
                    "Truncated response recovery failed after one compact-and-retry attempt."
                        .to_string()
                };
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(error_message.clone()),
                });
                self.emit_session_compact_failed(SessionCompactFailedEvent {
                    event_type: SessionCompactFailedEventType::SessionCompactFailed,
                    reason: CompactionReason::Overflow,
                    error_message: Some(error_message),
                    aborted: false,
                    will_retry: false,
                    from_extension: false,
                })
                .await;
                return Ok(false);
            }
            self.overflow_recovery_attempted
                .store(true, Ordering::SeqCst);
            self.omit_recovery_attempt(assistant_message)?;
            let retry = self
                .run_auto_compaction(CompactionReason::Overflow, will_retry)
                .await;
            if retry {
                *lock(&self.failed_response) = Some(assistant_message.clone());
            }
            return Ok(retry);
        }

        // Case 3：阈值压缩，不重试。
        let projection = current_projection;
        let has_context_edits = projection
            .entries
            .iter()
            .any(|entry| entry.source_entry.kind() == "context_edit");
        let direct_context_tokens = calculate_context_tokens(&assistant_message.usage);
        let context_tokens = if has_context_edits {
            estimate_projected_context_tokens(&projection, &branch).tokens
        } else if assistant_message.stop_reason == StopReason::Error || direct_context_tokens == 0 {
            let messages = self.agent.messages();
            let estimate = estimate_context_tokens(&messages);
            if let Some(last_usage_index) = estimate.last_usage_index
                && let Some(compaction_entry) = &compaction_entry
                && let Some(AgentMessage::Standard(Message::Assistant(usage_message))) =
                    messages.get(last_usage_index)
            {
                let compaction_time =
                    kiana_ai::model_catalog::js_date_parse(&compaction_entry.timestamp)
                        .unwrap_or(f64::NAN);
                if usage_message.timestamp as f64 <= compaction_time {
                    return Ok(false);
                }
            }
            estimate.tokens
        } else {
            direct_context_tokens
        };
        if should_compact(context_tokens, context_window, &settings) {
            return Ok(self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await);
        }
        Ok(false)
    }

    /// 自动压缩（阈值/溢出触发）的完整编排：prepare → `session_before_compact` 扩展事件
    /// （可取消或自带结果）→ 默认压缩或扩展压缩 → 落盘 → `session_compact` / `CompactionEnd`
    /// → 失败时 `session_compact_failed`。`auto_compaction_abort` 只登记本次的令牌，
    /// finally 按指针比较清除，避免误清并发的另一次压缩。返回“是否要求重试/继续”。
    async fn run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        let model = self.current_model();
        let settings = self.compaction_settings(model.as_ref());
        let mut abort_token: Option<Arc<CancellationToken>> = None;
        let mut started = false;
        let mut from_extension = false;
        let mut cancelled_by_extension = false;
        let compaction_started = std::time::Instant::now();
        let result: Result<bool, AgentSessionError> = async {
            let Some(model) = model else {
                return Ok(false);
            };
            let path_entries = self.branch();
            // 第 6 周门控：同一历史刚 no-op / 冷却中时不再付摘要成本。
            let history_snapshot = CompactionHistorySnapshot::capture(&path_entries)
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
            let history_hash = history_snapshot.hash();
            let gate_decision = self
                .compaction_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .check(history_hash, std::time::Instant::now());
            if !gate_decision.should_attempt() {
                return Ok(false);
            }
            // 第 6 周：压缩前先做 tool-result pruning（纯函数，未提交前不改变历史）。
            let prune_config = self.plan_prune_config();
            let (pruned, preparation) =
                prepare_with_pruning(&path_entries, &settings, &prune_config);
            let Some(preparation) = preparation else {
                self.compaction_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .record(
                        history_hash,
                        AttemptOutcome::NoOp,
                        std::time::Instant::now(),
                    );
                return Ok(false);
            };
            // 压缩前快照文件读取状态；成功后用同一份快照恢复 read gate。
            let read_state = Arc::clone(&self.read_state);
            let read_snapshots = tokio::task::spawn_blocking(move || read_state.snapshots(5))
                .await
                .unwrap_or_default();
            let summary_start = self.with_session(HistorySnapshot::capture);
            // PreCompact：自动压缩被 deny 时跳过本次（不报错、不改变历史）。
            let mut hook_instructions: Option<String> = None;
            if let Some(outcome) = self.pre_compact_hook("auto", None).await {
                if outcome.is_denied() {
                    return Ok(false);
                }
                // `continue: false`：取消本次自动压缩，并终止当前 run（压缩属于 run 的
                // 一部分时不能只静默跳过）。
                if !outcome.continue_execution {
                    self.agent_run_abort_requested.store(true, Ordering::SeqCst);
                    return Ok(false);
                }
                hook_instructions = outcome.additional_context_xml().map(|context| {
                    format!("<hook-context>{context}</hook-context>")
                });
            }
            let token = Arc::new(CancellationToken::new());
            abort_token = Some(Arc::clone(&token));
            *lock(&self.auto_compaction_abort) = Some(Arc::clone(&token));
            started = true;
            self.emit(AgentSessionEvent::CompactionStart { reason });
            if token.is_cancelled() {
                return Err(AgentSessionError::CompactionCancelled);
            }

            let mut extension_compaction: Option<CompactionResult> = None;
            if let Some(runner) = self.runner()
                && runner.has_handlers("session_before_compact") {
                    let emit_result = runner
                        .emit(ExtensionEvent::Session(SessionEvent::BeforeCompact(
                            kiana_ext_api::events::SessionBeforeCompactEvent {
                                event_type: kiana_ext_api::events::SessionBeforeCompactEventType::SessionBeforeCompact,
                                preparation: preparation.clone(),
                                branch_entries: path_entries.clone(),
                                custom_instructions: hook_instructions.clone(),
                                reason,
                                will_retry,
                                signal: (*token).clone(),
                            },
                        )))
                        .await;
                    if let Ok(Some(ExtensionEventResult::SessionBeforeCompact(result))) = emit_result {
                        if result.cancel == Some(true) {
                            cancelled_by_extension = true;
                            return Err(AgentSessionError::CompactionCancelled);
                        }
                        if let Some(compaction) = result.compaction {
                            extension_compaction = Some(compaction);
                            from_extension = true;
                        }
                    }
                }
            if token.is_cancelled() {
                return Err(AgentSessionError::CompactionCancelled);
            }
            let compaction = match extension_compaction {
                Some(compaction) => compaction,
                None => {
                    // 自动压缩（阈值/溢出）失败时保留既有语义：不提交、不重试，由
                    // `compaction_end` / `session_compact_failed` 通知调用方；确定性
                    // fallback 只用于用户显式发起的紧凑压缩（见 `Core::compact`）。
                    let result = self
                        .run_default_compaction(
                            &preparation,
                            &model,
                            hook_instructions.clone(),
                            (*token).clone(),
                            reason,
                        )
                        .await;
                    if result.is_err() {
                        self.compaction_gate
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .record(
                                history_hash,
                                AttemptOutcome::Failed,
                                std::time::Instant::now(),
                            );
                    }
                    result?
                }
            };
            if token.is_cancelled() {
                return Err(AgentSessionError::CompactionCancelled);
            }
            // all-or-nothing：摘要成功后校验“摘要期间”历史未被并发修改，再把 prune 的
            // context_edit 与 compaction 条目作为一个事务提交；任一步失败回滚内存。
            let commit_result = self.with_session_mut(|manager| -> Result<(), AgentSessionError> {
                if !summary_start.matches(manager) {
                    return Err(AgentSessionError::other(
                        "session history changed while compaction was in progress; refusing to commit",
                    ));
                }
                let mut transaction = CommitTransaction::begin(manager);
                if pruned.applied {
                    for record in &pruned.pruned {
                        match manager.append_context_edit(
                            &record.entry_id,
                            Some(ContextEditReplacement {
                                content: JsonValue::String(record.notice.clone()),
                            }),
                        ) {
                            Ok(edit_id) => transaction.record(&edit_id),
                            Err(error) => {
                                transaction.rollback(manager);
                                return Err(AgentSessionError::other(error.to_string()));
                            }
                        }
                    }
                }
                // 快照必须在 context_edit 之后重新捕获，才能通过提交时的并发校验。
                let after_edits = HistorySnapshot::capture(manager);
                match append_compaction_transactional(
                    manager,
                    &after_edits,
                    &compaction.summary,
                    Some(&compaction.first_kept_entry_id),
                    compaction.tokens_before as f64,
                    compaction.details.clone(),
                    Some(from_extension),
                    compaction.usage.clone(),
                ) {
                    Ok(id) => {
                        transaction.record(&id);
                        Ok(())
                    }
                    Err(error) => {
                        transaction.rollback(manager);
                        Err(AgentSessionError::other(error.to_string()))
                    }
                }
            });
            commit_result?;
            self.refresh_finalized_context();
            self.refresh_memory_after_compaction();
            // 压缩把部分 read 结果移出 live history：先整表失效，再用压缩前快照恢复
            // （指纹不一致/文件缺失只跳过该文件，不阻塞压缩）。
            self.read_state.clear();
            // 快照恢复可能要对大文件做流式指纹校验，放到 blocking 线程，避免阻塞会话。
            let read_state = Arc::clone(&self.read_state);
            let snapshots = read_snapshots.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for snapshot in &snapshots {
                    let _ = read_state.restore_snapshot(snapshot);
                }
            })
            .await;
            // 恢复文件/技能/plan/增量作为 hidden context 注入下一次请求；不是 tool result。
            let attachment = build_recovery_attachment(
                &RecoveryInput {
                    files: read_snapshots.clone(),
                    skills: collect_called_skills(&path_entries, 20),
                    plan: self.plan_recovery(),
                    deltas: collect_context_deltas(&path_entries),
                },
                &RecoveryBudget::default(),
            );
            *lock(&self.pending_recovery_attachment) =
                (!attachment.text.trim().is_empty()).then_some(attachment.text);
            self.compaction_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(
                    history_hash,
                    AttemptOutcome::Succeeded,
                    std::time::Instant::now(),
                );
            self.post_compact_hook("auto", hook_instructions.as_deref())
                .await;
            let estimated_tokens_after = self.estimate_messages_tokens();
            let _ = self
                .telemetry
                .record_compaction(&CompactionRecord {
                    trigger: match reason {
                        CompactionReason::Manual => "manual",
                        CompactionReason::Overflow => "overflow",
                        CompactionReason::Threshold => "auto",
                    }
                    .to_string(),
                    duration_ms: compaction_started.elapsed().as_secs_f64() * 1_000.0,
                    input_tokens: compaction.tokens_before,
                    output_tokens: estimated_tokens_after,
                    outcome: "ok".to_string(),
                })
                .await;
            // TS 重新扫描全部条目，取第一条摘要相同的压缩条目（可能不是刚追加的这条）。
            let saved = self.first_compaction_entry_by_summary(&compaction.summary);
            if let Some(entry) = saved
                && let Some(runner) = self.runner() {
                    let _ = runner
                        .emit(ExtensionEvent::Session(SessionEvent::Compact(SessionCompactEvent {
                            event_type: SessionCompactEventType::SessionCompact,
                            compaction_entry: entry,
                            from_extension,
                            reason,
                            will_retry,
                        })))
                        .await;
                }
            let result = CompactionResult {
                estimated_tokens_after: Some(estimated_tokens_after),
                ..compaction
            };
            self.emit(AgentSessionEvent::CompactionEnd {
                reason,
                result: Some(result),
                aborted: false,
                will_retry,
                error_message: None,
            });
            if will_retry {
                return Ok(true);
            }
            Ok(self.agent.has_queued_messages())
        }
        .await;
        let result_value = match result {
            Ok(value) => value,
            Err(error) => {
                let aborted = abort_token
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled())
                    || cancelled_by_extension;
                if started {
                    let message = if matches!(error, AgentSessionError::CompactionCancelled) {
                        "Compaction cancelled".to_string()
                    } else {
                        error.to_string()
                    };
                    let error_message = if aborted {
                        None
                    } else if reason == CompactionReason::Overflow {
                        Some(format!("Context overflow recovery failed: {message}"))
                    } else {
                        Some(format!("Auto-compaction failed: {message}"))
                    };
                    self.emit(AgentSessionEvent::CompactionEnd {
                        reason,
                        result: None,
                        aborted,
                        will_retry: false,
                        error_message: error_message.clone(),
                    });
                    self.emit_session_compact_failed(SessionCompactFailedEvent {
                        event_type: SessionCompactFailedEventType::SessionCompactFailed,
                        reason,
                        error_message,
                        aborted,
                        will_retry: false,
                        from_extension,
                    })
                    .await;
                }
                false
            }
        };
        // TS 的 `finally`：只清除本次压缩的 controller，并唤醒 idle 等待者。
        {
            let mut slot = lock(&self.auto_compaction_abort);
            let should_clear = match (&*slot, &abort_token) {
                (Some(current), Some(token)) => Arc::ptr_eq(current, token),
                _ => false,
            };
            if should_clear {
                *slot = None;
            }
        }
        self.resolve_idle_wait_if_idle();
        result_value
    }

    /// `_omitRecoveryAttempt`：把溢出/截断的 assistant 消息及其同轮 toolResult 从上下文
    /// 中省略（追加 `target_id → null` 的 context_edit 条目）。消息仍在 agent state 里
    /// 但找不到来源条目时返回 [`AgentSessionError::RecoveryOmissionUnresolved`]
    /// （TS 同名异常）。
    fn omit_recovery_attempt(&self, message: &AssistantMessage) -> Result<(), AgentSessionError> {
        let tool_call_ids: HashSet<String> = message
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::ToolCall(call) => Some(call.id.clone()),
                _ => None,
            })
            .collect();
        let message = AgentMessage::Standard(Message::Assistant(message.clone()));
        let entry_id = self.find_persisted_message_entry_id(&message);
        let messages = self.agent.messages();
        let index = messages.iter().position(|candidate| {
            message_key(candidate) == message_key(&message)
                || entry_id.as_ref().is_some_and(|entry_id| {
                    lock(&self.entry_ids_by_message)
                        .get(&message_key(candidate))
                        .is_some_and(|candidate_id| candidate_id == entry_id)
                })
        });
        let mut tool_results: Vec<AgentMessage> = Vec::new();
        if let Some(index) = index {
            for candidate in messages.iter().skip(index + 1) {
                if candidate.role() == "assistant" {
                    break;
                }
                if let AgentMessage::Standard(Message::ToolResult(result)) = candidate
                    && tool_call_ids.contains(&result.tool_call_id)
                {
                    tool_results.push(candidate.clone());
                }
            }
        }
        let mut targets = vec![message];
        targets.extend(tool_results);
        let target_ids: Vec<Option<String>> = targets
            .iter()
            .map(|target| self.find_persisted_message_entry_id(target))
            .collect();
        let unresolved = targets.iter().zip(target_ids.iter()).any(|(target, id)| {
            id.is_none()
                && messages
                    .iter()
                    .any(|candidate| message_key(candidate) == message_key(target))
        });
        if unresolved {
            return Err(AgentSessionError::RecoveryOmissionUnresolved);
        }
        for target_id in target_ids.into_iter().flatten() {
            // TS 让 `appendContextEdit` 的异常直接冒出 `omitRecoveryAttempt`；这里同样
            // 传播错误，而不是把它当成“无事发生”——持久化失败后继续用超限上下文重试
            // 只会无限重复同一个溢出。
            let edit_id = self
                .with_session_mut(|manager| manager.append_context_edit(&target_id, None))
                .map_err(|error| AgentSessionError::Other(error.to_string()))?;
            if let Some(entry) = self.with_session(|manager| manager.get_entry(&edit_id).cloned()) {
                self.emit(AgentSessionEvent::EntryAppended { entry });
            }
        }
        self.refresh_finalized_context();
        Ok(())
    }

    // -- 自动重试 -----------------------------------------------------------

    fn finish_cancelled_retry(&self) {
        let attempt = self.retry_attempt.swap(0, Ordering::SeqCst);
        if attempt == 0 {
            return;
        }
        self.emit(AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error: Some("Retry cancelled".to_string()),
        });
    }

    /// 自动重试的前置：检查重试设置与上限（超出则回退计数），按 TS 的指数退避公式算
    /// 延迟并发出 `auto_retry_start`，省略失败消息后让出（退避期间可被 `abort_retry`
    /// 取消，取消时发 `auto_retry_end(success=false)`）。返回是否应该重试。
    async fn prepare_retry(&self, message: &AssistantMessage) -> Result<bool, AgentSessionError> {
        let settings = self.settings_manager.get_retry_settings();
        if !settings.enabled {
            return Ok(false);
        }
        let attempt = self.retry_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt as f64 > settings.max_retries {
            self.retry_attempt.fetch_sub(1, Ordering::SeqCst);
            return Ok(false);
        }
        let delay_ms = retry_delay_ms_js(
            settings.base_delay_ms,
            Some(settings.max_agent_delay_ms),
            attempt as f64,
        ) as u64;
        self.emit(AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts: settings.max_retries.max(0.0) as u64,
            delay_ms,
            error_message: message
                .error_message
                .clone()
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| "Unknown error".to_string()),
        });
        // TS 不捕获：无法持久化恢复省略时错误直接冒泡出 prompt()，且不发出取消事件。
        self.omit_recovery_attempt(message)?;
        let token = CancellationToken::new();
        *lock(&self.retry_abort) = Some(token.clone());
        let cancelled = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => false,
            _ = token.cancelled() => true,
        };
        *lock(&self.retry_abort) = None;
        if cancelled {
            self.finish_cancelled_retry();
            return Ok(false);
        }
        Ok(true)
    }

    fn abort_retry(&self) {
        if let Some(token) = lock(&self.retry_abort).as_ref() {
            token.cancel();
        }
    }

    fn is_retrying(&self) -> bool {
        lock(&self.retry_abort).is_some()
    }

    // -- 运行后处理 ---------------------------------------------------------

    /// run 后处理：取走 `last_assistant_message`，按 abort → 可重试（`prepare_retry`，
    /// 退避可被取消）→ 报告 `AutoRetryEnd` 失败 → 压缩检查 → 是否还有排队消息的顺序
    /// 决定返回值；`true` 表示 `start_run` 的循环应该 `continue_()` 推进下一轮。
    async fn handle_post_agent_run(&self) -> Result<bool, AgentSessionError> {
        let message = lock(&self.last_assistant_message).take();
        if self.agent_run_abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return Ok(false);
        }
        let Some(message) = message else {
            return Ok(self.agent.has_queued_messages());
        };
        if self.is_retryable_error(&message) && self.prepare_retry(&message).await? {
            if self.agent_run_abort_requested.load(Ordering::SeqCst) {
                self.finish_cancelled_retry();
            }
            *lock(&self.failed_response) = Some(message);
            return Ok(!self.agent_run_abort_requested.load(Ordering::SeqCst));
        }
        if self.agent_run_abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return Ok(false);
        }
        let retry_attempt = self.retry_attempt.load(Ordering::SeqCst);
        if message.stop_reason == StopReason::Error && retry_attempt > 0 {
            self.emit(AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt: retry_attempt,
                final_error: message.error_message.clone(),
            });
            self.retry_attempt.store(0, Ordering::SeqCst);
        }
        if self.check_compaction(&message, true).await? {
            return Ok(!self.agent_run_abort_requested.load(Ordering::SeqCst));
        }
        Ok(!self.agent_run_abort_requested.load(Ordering::SeqCst)
            && self.agent.has_queued_messages())
    }

    /// `agent_before_settle` 边界派发。返回是否继续下一轮（扩展 `continue` 或 agent 有
    /// 排队消息，且最终 context 判定 `can_continue`）。扩展请求 continue 但 context
    /// 不可续时发 `ExtensionError` 并返回 `false`。派发期间（`is_before_settle`）收到的
    /// abort 记入 `abort_during_before_settle`，等边界结束再让循环退出。
    async fn run_before_settle_boundary(&self) -> bool {
        // Stop：hook deny 的语义是“不要结束回合”。达到每轮 deny 上限后 crate 会把
        // deny 降级为 ask；对 Stop 来说 ask 不能再续命，否则 hook 可以无限拖住回合。
        let stop_outcome = self.stop_hook().await;
        let mut stop_continue = false;
        if let Some(outcome) = &stop_outcome {
            if !outcome.continue_execution {
                // `continue: false`：不要继续回合，也不要再跑 before_settle 扩展。
                // 置 abort 标志让 `start_run_inner` 的循环在本次边界后退出；排队消息
                // 不会把 run 拖回下一轮。
                self.agent_run_abort_requested.store(true, Ordering::SeqCst);
                return false;
            }
            if outcome.is_denied() {
                stop_continue = true;
                let mut message = outcome
                    .blocking_error()
                    .unwrap_or("Continue working before stopping.")
                    .to_string();
                if let Some(context) = outcome.additional_context_xml() {
                    message.push_str("\n\n<hook-context>");
                    message.push_str(&context);
                    message.push_str("</hook-context>");
                }
                self.queue_steer(message, None).await;
            } else if let Some(context) = outcome.additional_context_xml() {
                self.queue_steer(format!("<hook-context>{context}</hook-context>"), None)
                    .await;
            }
        }

        let Some(runner) = self.runner() else {
            return stop_continue || self.agent.has_queued_messages();
        };
        if !runner.has_handlers("agent_before_settle") {
            return stop_continue || self.agent.has_queued_messages();
        }
        self.is_before_settle.store(true, Ordering::SeqCst);
        self.abort_during_before_settle
            .store(false, Ordering::SeqCst);
        let outcome = *lock(&self.last_activity_outcome);
        let weak = lock(&self.self_ref).clone();
        let build: BuildBoundaryContext = Arc::new(move |drafts| {
            let weak = weak.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(core) => core
                        .build_boundary_context(drafts, "agent_before_settle")
                        .map_err(|error| error.to_string()),
                    None => Err("session is disposed".to_string()),
                }
            })
        });
        let context = match self.build_boundary_context(Vec::new(), "agent_before_settle") {
            Ok(context) => context,
            Err(_error) => {
                self.is_before_settle.store(false, Ordering::SeqCst);
                return false;
            }
        };
        let emit_result = runner
            .emit_boundary(
                BoundaryBaseEvent::AgentBeforeSettle(AgentBeforeSettleEvent {
                    entries: Vec::new(),
                    r#continue: false,
                    context,
                    outcome,
                    event_type: AgentBeforeSettleEventType::AgentBeforeSettle,
                }),
                build,
            )
            .await;
        let result = match emit_result {
            Ok(result) => {
                self.is_before_settle.store(false, Ordering::SeqCst);
                result
            }
            Err(_) => {
                self.is_before_settle.store(false, Ordering::SeqCst);
                return false;
            }
        };
        self.commit_boundary_drafts(result.entries.clone());
        self.flush_pending_custom_messages();
        let final_context = self.build_boundary_context(Vec::new(), "agent_before_settle");
        self.is_before_settle.store(false, Ordering::SeqCst);
        if self.abort_during_before_settle.load(Ordering::SeqCst) {
            return false;
        }
        let should_continue =
            result.r#continue || stop_continue || self.agent.has_queued_messages();
        let can_continue = final_context
            .map(|context| context.can_continue)
            .unwrap_or(false);
        if should_continue && !can_continue {
            if result.r#continue {
                self.report_invalid_boundary_continuation("agent_before_settle");
            }
            return false;
        }
        should_continue
    }

    // -- 工具注册表 ---------------------------------------------------------

    fn get_active_tool_names(&self) -> Vec<String> {
        self.flush_pending_tool_refresh();
        self.agent
            .tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    /// `edit` 槽当前实际的模型可见工具名（`edit` 或 `apply_patch`）。
    fn edit_tool_name(&self) -> String {
        lock(&self.edit_tool_name).clone()
    }

    /// 把配置/系统提示里的 `edit` 映射到当前实际的工具名。
    fn canonical_tool_name(&self, name: &str) -> String {
        let edit_name = self.edit_tool_name();
        if edit_name != "edit" && name == "edit" {
            edit_name
        } else {
            name.to_string()
        }
    }

    /// 允许/排除列表里的等价名字。
    ///
    /// 配置可以用 `edit`（内建槽位）或路由后的 `apply_patch` 指代同一个编辑工具；
    /// 两个方向都要接受，否则 `--tools apply_patch` 或 `--tools edit` 会把工具过滤掉。
    fn allowlist_alias(&self, name: &str) -> Option<String> {
        let edit_name = self.edit_tool_name();
        if edit_name == "edit" {
            return None;
        }
        if name == "edit" {
            Some(edit_name)
        } else if name == kiana_tools::index::APPLY_PATCH_TOOL_NAME {
            Some("edit".to_string())
        } else {
            None
        }
    }

    fn is_allowed_tool(&self, name: &str) -> bool {
        let alias = self.allowlist_alias(name);
        if let Some(excluded) = &self.excluded_tools
            && (excluded(name) || alias.as_deref().is_some_and(|alias| excluded(alias)))
        {
            return false;
        }
        match &self.allowed_tools {
            None => true,
            Some(allowed) => {
                allowed(name)
                    || alias.as_deref().is_some_and(|alias| allowed(alias))
                    || (!self.allowlist_filters_mcp && is_mcp_tool_name(name))
            }
        }
    }

    fn is_activatable(&self, name: &str) -> bool {
        match &self.allowed_tools {
            None => true,
            Some(allowed) => {
                allowed(name)
                    || !is_mcp_tool_name(name)
                    || (self.tool_exposure(name) != ToolExposure::Direct
                        && lock(&self.tool_registry).contains_key("tool_search"))
            }
        }
    }

    fn tool_exposure(&self, name: &str) -> ToolExposure {
        lock(&self.tool_definitions)
            .get(name)
            .and_then(|entry| entry.definition.exposure)
            .unwrap_or(ToolExposure::Direct)
    }

    fn get_callable_tools_for(&self, active: &HashSet<String>) -> Vec<AgentTool> {
        lock(&self.tool_registry)
            .values()
            .filter(|tool| match self.tool_exposure(&tool.name) {
                ToolExposure::Codemode | ToolExposure::Deferred => true,
                ToolExposure::Direct => active.contains(&tool.name),
                _ => false,
            })
            .cloned()
            .collect()
    }

    fn get_callable_tools(&self) -> Vec<AgentTool> {
        self.flush_pending_tool_refresh();
        let active: HashSet<String> = self.get_active_tool_names().into_iter().collect();
        self.get_callable_tools_for(&active)
    }

    fn is_declarable(&self, name: &str) -> bool {
        matches!(
            self.tool_exposure(name),
            ToolExposure::Direct | ToolExposure::ModelOnly
        )
    }

    fn is_activated_on_registration(&self, name: &str) -> bool {
        self.is_declarable(name)
            && lock(&self.tool_definitions)
                .get(name)
                .and_then(|entry| entry.definition.default_active)
                .unwrap_or(true)
    }

    fn normalize_prompt_snippet(text: Option<&String>) -> Option<String> {
        let text = text?;
        if text.is_empty() {
            return None;
        }
        static LINE_BREAKS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static WHITESPACE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let line_breaks =
            LINE_BREAKS.get_or_init(|| regex::Regex::new(r"[\r\n]+").expect("valid regex"));
        // TS 用 JS 的 `/\s+/g`：该字符类**包含** U+FEFF 而**不包含** U+0085（NEL）；
        // Rust regex 的 Unicode `\s` 是 White_Space 属性，两者恰好相反，因此显式列出
        // ECMAScript 的 WhiteSpace + LineTerminator 集合。
        let whitespace = WHITESPACE.get_or_init(|| {
            regex::Regex::new(
                r"[\t\n\x{000B}\x{000C}\r \x{00A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]+",
            )
            .expect("valid regex")
        });
        let one_line = line_breaks.replace_all(text, " ");
        let one_line = whitespace.replace_all(&one_line, " ");
        let one_line = js_trim(&one_line).to_string();
        (!one_line.is_empty()).then_some(one_line)
    }

    fn normalize_prompt_guidelines(guidelines: Option<&Vec<String>>) -> Vec<String> {
        let Some(guidelines) = guidelines else {
            return Vec::new();
        };
        let mut unique = HashSet::new();
        let mut result = Vec::new();
        for guideline in guidelines {
            let normalized = js_trim(guideline).to_string();
            if !normalized.is_empty() && unique.insert(normalized.clone()) {
                result.push(normalized);
            }
        }
        result
    }

    /// `_applyToolLoadout`：按激活名单从注册表取工具（过滤 Hidden 与不可激活项），
    /// 对声明了 `prepare_loadout` 的工具派发 loadout hook —— hook 可以改描述或把声明
    /// 标记为隐藏（panic 按扩展错误上报，不中断其它 hook）；隐藏集写入
    /// `hidden_declarations`，最终把声明的工具写回 `agent.set_tools`。
    fn apply_tool_loadout(&self, tool_names: &[String]) -> Vec<AgentTool> {
        let mut seen = HashSet::new();
        let mut tools: Vec<AgentTool> = Vec::new();
        for name in tool_names {
            // 调用方可能仍传配置里的 `edit`；注册表键是当前实际名字。
            let name = self.canonical_tool_name(name);
            if !seen.insert(name.clone()) {
                continue;
            }
            let tool = lock(&self.tool_registry).get(&name).cloned();
            if let Some(tool) = tool
                && self.tool_exposure(&name) != ToolExposure::Hidden
                && self.is_activatable(&name)
            {
                tools.push(tool);
            }
        }
        let hooks: Vec<ToolDefinitionEntry> = tools
            .iter()
            .filter_map(|tool| {
                lock(&self.tool_definitions)
                    .get(&tool.name)
                    .filter(|entry| entry.definition.prepare_loadout.is_some())
                    .cloned()
            })
            .collect();
        let mut hidden: HashSet<String> = HashSet::new();
        let mut declared = tools.clone();
        if !hooks.is_empty() {
            let callable =
                self.get_callable_tools_for(&tools.iter().map(|tool| tool.name.clone()).collect());
            let registered: Vec<AgentTool> = lock(&self.tool_registry).values().cloned().collect();
            // TS 的 `getNamespace`/`getPromptGuidelines` 读 live 表；loadout 只在本次
            // prepareLoadout 派发期间存活，快照与 live 读取等价，且避免为返回引用而泄漏。
            let namespaces: HashMap<String, kiana_ext_api::tool_types::ToolNamespace> =
                lock(&self.tool_definitions)
                    .iter()
                    .filter_map(|(name, entry)| {
                        entry
                            .definition
                            .namespace
                            .clone()
                            .map(|namespace| (name.clone(), namespace))
                    })
                    .collect();
            let prompt_guidelines: HashMap<String, Vec<String>> =
                lock(&self.tool_prompt_guidelines)
                    .clone()
                    .into_iter()
                    .collect();
            let loadout = SessionToolLoadout {
                declared: tools.clone(),
                callable,
                registered,
                namespaces,
                prompt_guidelines,
                core: self,
            };
            let mut descriptions: IndexMap<String, String> = IndexMap::new();
            for entry in &hooks {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    entry
                        .definition
                        .prepare_loadout
                        .as_ref()
                        .and_then(|hook| hook(&loadout))
                }));
                match result {
                    Ok(Some(changes)) => {
                        if let Some(descriptions_changes) = &changes.descriptions {
                            for (name, description) in descriptions_changes {
                                descriptions.insert(name.clone(), description.clone());
                            }
                        }
                        if let Some(hidden_declarations) = &changes.hidden_declarations {
                            hidden.extend(hidden_declarations.iter().cloned());
                        }
                    }
                    Ok(None) => {}
                    Err(payload) => {
                        let message = payload
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        if let Some(runner) = self.runner() {
                            runner.emit_error(ExtensionError {
                                extension_path: entry.source_info.path.clone(),
                                event: "prepare_loadout".to_string(),
                                error: message,
                                stack: None,
                            });
                        }
                    }
                }
            }
            declared = tools
                .iter()
                .map(|tool| match descriptions.get(&tool.name) {
                    Some(description) => {
                        let mut tool = tool.clone();
                        tool.description = description.clone();
                        tool
                    }
                    None => tool.clone(),
                })
                .collect();
        }
        *lock(&self.hidden_declarations) = hidden;
        self.agent.set_tools(declared.clone());
        declared
    }

    fn set_active_tools(&self, tool_names: &[String]) {
        let tools = self.apply_tool_loadout(tool_names);
        {
            let mut pending = lock(&self.pending_tool_names);
            for tool in &tools {
                pending.shift_remove(&tool.name);
            }
        }
        let names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        let _ = self.rebuild_system_prompt(&names);
    }

    fn set_active_tools_by_name(&self, tool_names: &[String]) {
        let previous = self.get_active_tool_names();
        self.set_active_tools(tool_names);
        let active: HashSet<String> = self.get_active_tool_names().into_iter().collect();
        if previous.iter().any(|name| !active.contains(name)) {
            lock(&self.pending_tool_names).clear();
        }
    }

    /// 第 10 周：把当前模型接进记忆 side query，并在 `auto_extract=true` 时启动后台抽取。
    ///
    /// 必须在 `Arc<Core>` 建立后调用（模型 getter 通过 `Weak<Core>` 读取会话当前模型）。
    fn initialize_memory_runtime(self: &Arc<Self>) {
        let Some(memory) = self.memory.clone() else {
            return;
        };
        let weak = Arc::downgrade(self);
        let model: Arc<dyn Fn() -> Option<kiana_ai::types::Model> + Send + Sync> =
            Arc::new(move || weak.upgrade().map(|core| core.agent.model()));
        let bridge: Arc<dyn MemoryModelBridge> =
            Arc::new(ProviderMemoryModel::new(self.model_runtime.clone(), model));
        memory.set_model_bridge(bridge);
        let current_session_id = lock(&self.session_manager).get_session_id().to_string();
        memory.spawn_background_extraction(Some(current_session_id));
    }

    /// 第 11 周：清理上次进程崩溃遗留的隔离工作区 ownership 记录。
    ///
    /// 扫描默认目录（`<cwd>/.kiana/subagents`）、本会话见过的自定义
    /// `IsolationOptions.state_dir`，以及调用方从 Work 账本/子会话 header 推导出的
    /// 目录；损坏记录会被隔离，清理失败（目录仍被占用/权限不足）的记录保留给下一次
    /// 启动重试。
    fn sweep_subagent_workspaces(
        &self,
        keep_pids: &[u32],
        extra_state_dirs: &[std::path::PathBuf],
    ) {
        let mut state_dirs = std::collections::BTreeSet::new();
        state_dirs.insert(
            std::path::Path::new(&self.cwd)
                .join(".kiana")
                .join("subagents"),
        );
        state_dirs.extend(lock(&self.isolation_state_dirs).iter().cloned());
        state_dirs.extend(extra_state_dirs.iter().cloned());
        for state_dir in state_dirs {
            let _ = crate::subagent::sweep_ownership_records_with_keep(state_dir, keep_pids);
        }
    }

    /// 记录一个准备子代理时使用的隔离状态目录。
    fn register_isolation_state_dir(&self, state_dir: Option<&std::path::Path>) {
        let Some(state_dir) = state_dir else {
            return;
        };
        lock(&self.isolation_state_dirs).insert(state_dir.to_path_buf());
    }

    /// 第 10 周：压缩提交成功后刷新 load-time 记忆快照。
    ///
    /// 这是 mid-session 写入进入 system prompt 的唯一时机；快照变化时同时重建 base/run
    /// options，下一次 `prepareNextTurn` 的 `diff_system_prompt_sections` 会把新的
    /// `memory` section 推给模型。压缩失败路径不会调用本方法。
    fn refresh_memory_after_compaction(&self) {
        let Some(memory) = &self.memory else {
            return;
        };
        if let Ok(change) = memory.after_compaction()
            && change.changed
        {
            // 压缩后允许重新召回（已注入集合只对旧上下文有效）。
            memory.reset_recall();
            let names = self.get_active_tool_names();
            if let Some(options) = self.rebuild_system_prompt(&names) {
                *lock(&self.run_system_prompt_options) = Some(options);
            }
        }
    }

    /// `_rebuildSystemPrompt(toolNames)`：需要资源加载器，由 `AgentSession` 注入技能/上下文。
    fn rebuild_system_prompt(
        &self,
        tool_names: &[String],
    ) -> Option<NormalizedBuildSystemPromptOptions> {
        self.flush_pending_tool_refresh();
        // TS `_rebuildSystemPrompt` 每次从 live `ResourceLoader` 读取技能/上下文/系统提示。
        self.refresh_command_sources_from_loader();
        let valid: Vec<String> = tool_names
            .iter()
            .filter(|name| lock(&self.tool_registry).contains_key(*name))
            .cloned()
            .collect();
        let mut tool_snippets: IndexMap<String, String> = IndexMap::new();
        for name in lock(&self.tool_registry).keys() {
            if let Some(snippet) = lock(&self.tool_prompt_snippets).get(name) {
                tool_snippets.insert(name.clone(), snippet.clone());
            }
        }
        let (skills, context_files, system_prompt, append_system_prompt) = {
            let sources = lock(&self.commands);
            (
                sources.skills.clone(),
                sources.context_files.clone(),
                sources.system_prompt.clone(),
                if sources.append_system_prompt.is_empty() {
                    String::new()
                } else {
                    sources.append_system_prompt.join("\n\n")
                },
            )
        };
        // 第 5 周：技能索引改为「扩展定义 + 模型窗口 1% 预算 + paths 可见性」。
        // `format_skill_definitions_for_prompt` 为空时回退到默认 `format_skills_for_prompt`
        // （老路径仍应用 8000 字符预算/250 字符上限）。
        let mut sections = IndexMap::new();
        // 有定义时必须走新索引（空字符串表示“全部被 paths/disable-model-invocation
        // 隐藏”），不能回退到旧的 `format_skills_for_prompt` —— 旧格式会忽略 paths。
        let skills = match self.skill_prompt_section(skill_reader_for_tools(&valid)) {
            Some(text) => {
                if !text.trim().is_empty() {
                    sections.insert("skills".to_string(), text);
                }
                Vec::new()
            }
            None => skills,
        };
        // 第 10 周：load-time 记忆索引。mid-session 写入只改磁盘，不重建本 section；
        // 只有压缩成功后才通过 `refresh_memory_after_compaction` 刷新快照。
        if let Some(memory) = &self.memory
            && let Some(index) = memory.system_prompt_index()
        {
            sections.insert("memory".to_string(), index);
        }
        // 第 12 周：output language 只影响模型回答语言，与 UI locale 无关。
        if let Some(rule) = self
            .output_language
            .as_deref()
            .and_then(kiana_resources::system_prompt::output_language_system_rule)
        {
            sections.insert("output_language".to_string(), rule);
        }
        let options = normalize_build_system_prompt_options(&BuildSystemPromptOptions {
            cwd: self.cwd.clone(),
            skills: Some(skills),
            context_files: Some(context_files),
            custom_prompt: system_prompt,
            append_system_prompt: Some(append_system_prompt),
            selected_tools: Some(valid),
            hidden_tools: Some(lock(&self.hidden_declarations).iter().cloned().collect()),
            tool_snippets: Some(tool_snippets),
            tool_guidelines: Some(lock(&self.tool_prompt_guidelines).clone()),
            sections: Some(sections),
            ..Default::default()
        });
        *lock(&self.base_system_prompt_options) = Some(options.clone());
        Some(options)
    }

    /// `_preparePromptAndToolLoadout(options, messages)`。
    fn prepare_prompt_and_tool_loadout(
        &self,
        options: &NormalizedBuildSystemPromptOptions,
        messages: &[AgentMessage],
    ) -> Option<SystemMessage> {
        let mut options = options.clone();
        options.selected_tools = self
            .apply_tool_loadout(&options.selected_tools)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        options.hidden_tools = lock(&self.hidden_declarations).iter().cloned().collect();
        // 每一轮都按当前已触达路径重建技能索引：`paths` 技能在第一次 read 之后的
        // 下一轮系统提示增量里出现。定义/预算本身不变时输出确定，diff 为空。
        // 有定义时总是清空旧格式列表（即使本轮没有任何可见技能），避免回退路径忽略 paths。
        if let Some(text) =
            self.skill_prompt_section(skill_reader_for_tools(&options.selected_tools))
        {
            options.skills = Vec::new();
            if !text.trim().is_empty() {
                options.sections.insert("skills".to_string(), text);
            }
        }
        let llm: Vec<Message> = convert_to_llm(messages);
        let previous = get_current_system_message(&llm)
            .and_then(|message| message.sections)
            .unwrap_or_default();
        let current = build_system_prompt_sections(&BuildSystemPromptOptions {
            custom_prompt: options.custom_prompt.clone(),
            force_system_prompt: options.force_system_prompt.clone(),
            selected_tools: Some(options.selected_tools.clone()),
            hidden_tools: Some(options.hidden_tools.clone()),
            tool_snippets: Some(options.tool_snippets.clone()),
            tool_guidelines: Some(options.tool_guidelines.clone()),
            prompt_guidelines: Some(options.prompt_guidelines.clone()),
            append_system_prompt: Some(options.append_system_prompt.clone()),
            sections: Some(options.sections.clone()),
            cwd: options.cwd.clone(),
            context_files: Some(options.context_files.clone()),
            skills: Some(options.skills.clone()),
        })
        .ok()?;
        let sections = diff_system_prompt_sections(&previous, &current)?;
        Some(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(String::new()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        })
    }

    fn normalize_build_options(
        &self,
        options: &NormalizedBuildSystemPromptOptions,
        tool_names: Vec<String>,
    ) -> NormalizedBuildSystemPromptOptions {
        let base = lock(&self.base_system_prompt_options).clone();
        let mut tool_snippets = base
            .as_ref()
            .map(|base| base.tool_snippets.clone())
            .unwrap_or_default();
        for (name, snippet) in &options.tool_snippets {
            tool_snippets.insert(name.clone(), snippet.clone());
        }
        let mut tool_guidelines = base
            .as_ref()
            .map(|base| base.tool_guidelines.clone())
            .unwrap_or_default();
        for (name, value) in &options.tool_guidelines {
            tool_guidelines.insert(name.clone(), value.clone());
        }
        normalize_build_system_prompt_options(&BuildSystemPromptOptions {
            custom_prompt: options.custom_prompt.clone(),
            force_system_prompt: options.force_system_prompt.clone(),
            selected_tools: Some(tool_names),
            hidden_tools: Some(options.hidden_tools.clone()),
            tool_snippets: Some(tool_snippets),
            tool_guidelines: Some(tool_guidelines),
            prompt_guidelines: Some(options.prompt_guidelines.clone()),
            append_system_prompt: Some(options.append_system_prompt.clone()),
            sections: Some(options.sections.clone()),
            cwd: options.cwd.clone(),
            context_files: Some(options.context_files.clone()),
            skills: Some(options.skills.clone()),
        })
    }
}

/// `prepareRequest` 用到的认证结果。
struct RequestAuth {
    model: Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
}

/// 摘要请求认证。
struct SummarizationAuth {
    model: Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    thinking_level: ModelThinkingLevel,
}

/// `ToolLoadout` 的会话实现。
struct SessionToolLoadout<'a> {
    declared: Vec<AgentTool>,
    callable: Vec<AgentTool>,
    registered: Vec<AgentTool>,
    /// `_toolDefinitions.get(name)?.definition.namespace` 的 loadout 快照。
    namespaces: HashMap<String, kiana_ext_api::tool_types::ToolNamespace>,
    /// `_toolPromptGuidelines` 的 loadout 快照。
    prompt_guidelines: HashMap<String, Vec<String>>,
    core: &'a Core,
}

impl ToolLoadout for SessionToolLoadout<'_> {
    fn declared(&self) -> &[AgentTool] {
        &self.declared
    }
    fn callable(&self) -> &[AgentTool] {
        &self.callable
    }
    fn registered(&self) -> &[AgentTool] {
        &self.registered
    }
    fn get_exposure(&self, name: &str) -> ToolExposure {
        self.core.tool_exposure(name)
    }
    fn get_namespace(&self, name: &str) -> Option<&kiana_ext_api::tool_types::ToolNamespace> {
        self.namespaces.get(name)
    }
    fn get_prompt_guidelines(&self, name: &str) -> &[String] {
        self.prompt_guidelines
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

impl Core {
    /// `_executeNestedToolCall`。
    async fn execute_nested_tool_call(
        &self,
        parent_tool_call_id: String,
        name: String,
        args: serde_json::Value,
        options: ExecuteToolOptions,
    ) -> AgentToolCallOutcome {
        let runner = {
            let mut slot = lock(&self.nested_tool_calls);
            match slot.as_ref() {
                Some(runner) => Arc::clone(runner),
                None => {
                    let initial = self.get_callable_tools();
                    let signature = initial
                        .iter()
                        .map(|tool| (tool.name.clone(), tool.execution_mode))
                        .collect();
                    let host = SessionNestedHost {
                        core: lock(&self.self_ref).clone(),
                        tools: Mutex::new(NestedToolCache {
                            signature,
                            tools: Box::leak(initial.into_boxed_slice()),
                        }),
                    };
                    let runner = Arc::new(NestedToolCallRunner::new(host));
                    *slot = Some(Arc::clone(&runner));
                    runner
                }
            }
        };
        let options = kiana_session_core::nested_tool_calls::NestedToolCallOptions {
            signal: options.signal.as_ref().map(abort_scope_from_token),
            on_update: options.on_update,
        };
        runner
            .execute(&parent_tool_call_id, &name, args, options)
            .await
    }

    /// `setModel(model, options)`。
    async fn set_model_impl(
        &self,
        model: Model,
        options: ModelMutationOptions,
    ) -> Result<(), AgentSessionError> {
        // TS: `if (!(await checkAuth(provider))) throw ...`；checkAuth 的 rejection
        // （OAuth 刷新/凭证读取失败）原样传播，而不是被当成“没有 API key”。
        let check = self
            .model_runtime
            .check_auth(&model.provider, Default::default())
            .await
            .map_err(|error| AgentSessionError::other(error.message))?;
        if check.is_none() {
            return Err(AgentSessionError::other(format!(
                "No API key for {}/{}",
                model.provider, model.id
            )));
        }
        let previous_model = self.current_model();
        let thinking_level = self.thinking_level_for_model_switch(Some(&model), None);
        self.agent.set_model(model.clone());
        // `tools.applyPatch.models` 按模型路由编辑工具：切换模型后必须重建定义，
        // 否则新模型仍看到上一个模型的 edit/apply_patch 选择。
        self.rebuild_builtin_tool_definitions();
        // TS 的 `sessionManager.appendModelChange` 直接写文件，失败会 reject setModel。
        self.with_session_mut(|manager| manager.append_model_change(&model.provider, &model.id))
            .map_err(|error| AgentSessionError::other(error.to_string()))?;
        if options.persist.unwrap_or(false) {
            self.settings_manager
                .set_default_model_and_provider(model.provider.clone(), model.id.clone());
            self.add_persisted_default_to_non_empty_scope(&model);
        }
        self.set_thinking_level_impl(thinking_level, ModelMutationOptions::default())?;
        self.emit_model_select(model, previous_model, ModelSelectSource::Set)
            .await;
        Ok(())
    }

    fn add_persisted_default_to_non_empty_scope(&self, model: &Model) {
        {
            let mut scoped = lock(&self.scoped_models);
            if scoped.is_empty() {
                return;
            }
            if scoped
                .iter()
                .any(|entry| models_equal(Some(&entry.model), Some(model)))
            {
                return;
            }
            scoped.push(ScopedModel {
                model: model.clone(),
                thinking_level: None,
            });
        }
        let Some(enabled_models) = self.settings_manager.get_enabled_models() else {
            return;
        };
        if enabled_models.is_empty() {
            return;
        }
        let reference = format!("{}/{}", model.provider, model.id);
        if enabled_models
            .iter()
            .any(|pattern| pattern.to_lowercase() == reference.to_lowercase())
        {
            return;
        }
        let mut next = enabled_models;
        next.push(reference);
        self.settings_manager.set_enabled_models(Some(next));
    }

    fn thinking_level_for_model_switch(
        &self,
        target: Option<&Model>,
        explicit: Option<ModelThinkingLevel>,
    ) -> ModelThinkingLevel {
        if let Some(explicit) = explicit {
            return explicit;
        }
        if let Some(target) = target
            && let Some(level) = self
                .settings_manager
                .get_model_thinking_level(&target.provider, &target.id)
        {
            return level;
        }
        self.settings_manager
            .get_default_thinking_level()
            .unwrap_or_else(|| self.agent.thinking_level())
    }

    fn available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        match self.current_model() {
            None => THINKING_LEVEL_OPTIONS.to_vec(),
            Some(model) => kiana_ai::models::thinking::get_supported_thinking_levels(&model),
        }
    }

    fn supports_thinking(&self) -> bool {
        self.current_model()
            .and_then(|model| model.reasoning)
            .unwrap_or(false)
    }

    fn set_thinking_level_impl(
        &self,
        level: ModelThinkingLevel,
        options: ModelMutationOptions,
    ) -> Result<(), AgentSessionError> {
        let available = self.available_thinking_levels();
        let effective = if available.contains(&level) {
            level
        } else if let Some(model) = self.current_model() {
            kiana_ai::models::thinking::clamp_thinking_level(&model, level)
        } else {
            ModelThinkingLevel::Off
        };
        let previous = self.agent.thinking_level();
        let is_changing = previous != effective;
        self.agent.set_thinking_level(effective);
        // TS 先写 settings，再 append（append 抛出时 settings 已经落盘）。
        if options.persist.unwrap_or(false) {
            self.settings_manager.set_default_thinking_level(level);
        }
        if is_changing {
            // TS `appendThinkingLevelChange` 直接写文件，失败会同步抛出。
            self.with_session_mut(|manager| {
                manager.append_thinking_level_change(&thinking_level_text(effective))
            })
            .map_err(|error| AgentSessionError::other(error.to_string()))?;
            self.emit(AgentSessionEvent::ThinkingLevelChanged { level: effective });
            if let Some(runner) = self.runner() {
                spawn_local_event(
                    runner,
                    ExtensionEvent::ThinkingLevelSelect(ThinkingLevelSelectEvent {
                        event_type: ThinkingLevelSelectEventType::ThinkingLevelSelect,
                        level: effective,
                        previous_level: previous,
                    }),
                );
            }
        }
        Ok(())
    }

    async fn emit_model_select(
        &self,
        next: Model,
        previous: Option<Model>,
        source: ModelSelectSource,
    ) {
        if models_equal(previous.as_ref(), Some(&next)) {
            return;
        }
        if let Some(runner) = self.runner() {
            let _ = runner
                .emit(ExtensionEvent::ModelSelect(ModelSelectEvent {
                    event_type: ModelSelectEventType::ModelSelect,
                    model: next,
                    previous_model: previous,
                    source,
                }))
                .await;
        }
    }

    fn sync_queue_modes_from_settings(&self) {
        self.agent.set_steering_mode(queue_mode_from_settings(
            self.settings_manager.get_steering_mode(),
        ));
        self.agent.set_follow_up_mode(queue_mode_from_settings(
            self.settings_manager.get_follow_up_mode(),
        ));
    }

    /// `getContextUsage` 的实现。压缩边界之后没有任何带 usage 的 assistant 消息时，
    /// 无法可靠估算（投影被压缩截断），返回 `tokens`/`percent` 为 `None` 的占位值
    /// （只带 context_window）；其余情况用 `estimate_projected_context_tokens` 估算。
    fn get_context_usage_inner(&self) -> Option<kiana_ext_api::types::ContextUsage> {
        let model = self.limits_model()?;
        let context_window = model.context_window.unwrap_or(0);
        if context_window == 0 {
            return None;
        }
        let projection = self.projection();
        let branch = self.branch();
        let latest_compaction =
            kiana_session_core::session_manager::get_latest_compaction_entry(&branch);
        if let Some(latest_compaction) = latest_compaction {
            let projected_assistants: HashSet<String> = projection
                .entries
                .iter()
                .filter(|entry| {
                    entry.messages.iter().any(|message| {
                        as_assistant(message).is_some_and(|assistant| {
                            assistant.stop_reason != StopReason::Aborted
                                && assistant.stop_reason != StopReason::Error
                                && calculate_context_tokens(&assistant.usage) > 0
                        })
                    })
                })
                .map(|entry| entry.source_entry.id().to_string())
                .collect();
            let compaction_index = branch
                .iter()
                .position(|entry| entry.id() == latest_compaction.id);
            let has_post_compaction_usage = match compaction_index {
                Some(index) => branch[index + 1..]
                    .iter()
                    .any(|entry| projected_assistants.contains(entry.id())),
                None => false,
            };
            if !has_post_compaction_usage {
                return Some(kiana_ext_api::types::ContextUsage {
                    tokens: None,
                    context_window,
                    percent: None,
                });
            }
        }
        let estimate = estimate_projected_context_tokens(&projection, &branch);
        let percent = (estimate.tokens as f64 / context_window as f64) * 100.0;
        Some(kiana_ext_api::types::ContextUsage {
            tokens: Some(estimate.tokens),
            context_window,
            percent: Some(percent),
        })
    }

    fn get_session_stats_inner(&self) -> SessionStats {
        let mut user_messages = 0u64;
        let mut assistant_messages = 0u64;
        let mut tool_results = 0u64;
        let mut total_messages = 0u64;
        let mut tool_calls = 0u64;
        let mut usage_totals = create_usage_totals();
        for entry in self.with_session(SessionManager::get_entries) {
            match &entry {
                SessionEntry::Usage(usage) => add_usage_to_totals(&mut usage_totals, &usage.usage),
                SessionEntry::BranchSummary(summary) => {
                    if let Some(usage) = &summary.usage {
                        add_usage_to_totals(&mut usage_totals, usage);
                    }
                }
                SessionEntry::Compaction(compaction) => {
                    if let Some(usage) = &compaction.usage {
                        add_usage_to_totals(&mut usage_totals, usage);
                    }
                }
                _ => {}
            }
            let SessionEntry::Message(message_entry) = &entry else {
                continue;
            };
            total_messages += 1;
            match &message_entry.message {
                AgentMessage::Standard(Message::User(_)) => user_messages += 1,
                AgentMessage::Standard(Message::ToolResult(result)) => {
                    tool_results += 1;
                    if let Some(usage) = &result.usage {
                        add_usage_to_totals(&mut usage_totals, usage);
                    }
                }
                AgentMessage::Standard(Message::Assistant(assistant)) => {
                    assistant_messages += 1;
                    tool_calls += assistant
                        .content
                        .iter()
                        .filter(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
                        .count() as u64;
                    add_usage_to_totals(&mut usage_totals, &assistant.usage);
                }
                _ => {}
            }
        }
        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            tokens: SessionStatsTokens {
                input: usage_totals.input as f64,
                output: usage_totals.output as f64,
                cache_read: usage_totals.cache_read as f64,
                cache_write: usage_totals.cache_write as f64,
                total: (usage_totals.input
                    + usage_totals.output
                    + usage_totals.cache_read
                    + usage_totals.cache_write) as f64,
            },
            cost: usage_totals.cost,
            context_usage: self.get_context_usage_inner(),
        }
    }

    fn get_last_assistant_text_inner(&self) -> Option<String> {
        let last = self.agent.messages().into_iter().rev().find(|message| {
            as_assistant(message).is_some_and(|assistant| {
                !(assistant.stop_reason == StopReason::Aborted && assistant.content.is_empty())
            })
        })?;
        let assistant = as_assistant(&last)?;
        let text: String = assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();
        let trimmed = js_trim(&text);
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }
}

impl Core {
    // -- prompt / 运行循环 --------------------------------------------------

    fn prompt_templates_snapshot(&self) -> Vec<PromptTemplate> {
        self.refresh_command_sources_from_loader();
        lock(&self.commands).prompts.clone()
    }

    /// `_expandSkillCommand`（技能来自命令快照）：`/skill:<name> [args]` → `<skill>` 块。
    /// 技能正文从 `SKILL.md` 读取并剥掉 frontmatter；读取失败按 Node fs 异常的 message
    /// 文案上报，frontmatter 解析失败按解析错误的 Display 文案上报；两者都发
    /// `skill_expansion` 扩展错误并原样返回命令文本（不中断）。
    fn expand_skill_command(&self, text: &str) -> String {
        if !text.starts_with("/skill:") {
            return text.to_string();
        }
        self.refresh_command_sources_from_loader();
        // 用户显式停用一个技能：`/skill:deactivate <name>` / `/skill:off <name>`。
        for prefix in ["/skill:deactivate", "/skill:off"] {
            if let Some(name) = text.strip_prefix(prefix).map(str::trim)
                && !name.is_empty()
            {
                let deactivated = lock(&self.skill_tool)
                    .as_ref()
                    .is_some_and(|tool| tool.deactivate(name));
                if deactivated {
                    return format!("Skill \"{name}\" deactivated.");
                }
                return text.to_string();
            }
        }
        let (skill_name, args) = match text.find(' ') {
            Some(index) => (&text[7..index], js_trim(&text[index + 1..]).to_string()),
            None => (&text[7..], String::new()),
        };
        let skill = lock(&self.commands)
            .skills
            .iter()
            .find(|skill| skill.name == skill_name)
            .cloned();
        let Some(skill) = skill else {
            return text.to_string();
        };
        // 用户路径必须尊重 `user-invocable`。定义里没有这个技能时保持旧行为（loader 尚未
        // 同步的极端情况），但定义存在时 false 直接拒绝。
        let definition = lock(&self.skill_definitions)
            .iter()
            .find(|definition| definition.name() == skill.name)
            .cloned();
        if definition
            .as_ref()
            .is_some_and(|definition| !definition.metadata.user_invocable)
        {
            self.emit_skill_expansion_error(
                &skill.file_path,
                format!("Skill {} is not user-invocable", skill.name),
            );
            return text.to_string();
        }
        // TS `readFileSync(path, "utf-8")`：按 UTF-8 解码非法字节为 U+FFFD（不是报错），
        // 读取失败则抛出 Node fs 异常（message 形如
        // `ENOENT: no such file or directory, open '<path>'`）。
        let content = match std::fs::read(&skill.file_path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                // Node 的 `readFileSync` 先 open 再 read：open 失败用 `open`，读取目录等
                // read 阶段失败用 `read`。
                let syscall = if error.kind() == std::io::ErrorKind::IsADirectory {
                    "read"
                } else {
                    "open"
                };
                let message = node_fs_error_message(syscall, &skill.file_path, &error);
                self.emit_skill_expansion_error(&skill.file_path, message);
                return text.to_string();
            }
        };
        // TS 把 `readFileSync` 与 `stripFrontmatter` 放在同一个 try 里：frontmatter
        // 解析失败（yaml 抛错）同样上报错误并原样返回命令。
        let body = match kiana_core::frontmatter::strip_frontmatter(&content) {
            Ok(body) => js_trim(&body).to_string(),
            Err(error) => {
                self.emit_skill_expansion_error(&skill.file_path, error.to_string());
                return text.to_string();
            }
        };
        // 用户路径与模型 `skill` 工具共用同一套 activation/参数校验：`allowed-tools` 只在
        // 激活存活期间生效，`arguments` 的必填/未知校验也在这里执行。
        if definition.is_some() {
            let tool = self.ensure_skill_tool();
            if let Err(error) = tool.activate_user(&skill.name, &args) {
                self.emit_skill_expansion_error(&skill.file_path, error);
                return text.to_string();
            }
        }
        let skill_block = format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
            skill.name, skill.file_path, skill.base_dir, body
        );
        if args.is_empty() {
            skill_block
        } else {
            format!("{skill_block}\n\n{args}")
        }
    }

    /// `_extensionRunner.emitError({ extensionPath, event: "skill_expansion", error })`。
    fn emit_skill_expansion_error(&self, extension_path: &str, error: String) {
        if let Some(runner) = self.runner() {
            runner.emit_error(ExtensionError {
                extension_path: extension_path.to_string(),
                event: "skill_expansion".to_string(),
                error,
                stack: None,
            });
        }
    }

    /// `_tryExecuteExtensionCommand`。
    async fn try_execute_extension_command(&self, text: &str) -> bool {
        let (command_name, args) = match text.find(' ') {
            Some(index) => (text[1..index].to_string(), text[index + 1..].to_string()),
            None => (text[1..].to_string(), String::new()),
        };
        let Some(runner) = self.runner() else {
            return false;
        };
        let Some(command) = runner.get_command(&command_name) else {
            return false;
        };
        let context = runner.create_command_context();
        (command.handler)(args, context).await;
        true
    }

    /// `_runInputHandlers`。
    async fn run_input_handlers(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Option<(String, Option<Vec<ImageContent>>)> {
        let runner = self.runner()?;
        if !runner.has_handlers("input") {
            return Some((text, images));
        }
        match runner
            .emit_input(&text, images.clone(), source, streaming_behavior)
            .await
        {
            Ok(InputEventResult::Handled) => None,
            Ok(InputEventResult::Transform {
                text,
                images: transformed,
            }) => Some((text, transformed.or(images))),
            Ok(InputEventResult::Continue) => Some((text, images)),
            Err(_) => Some((text, images)),
        }
    }

    /// `_normalizePromptImages`。
    fn normalize_prompt_images(
        &self,
        images: Option<Vec<ImageContent>>,
    ) -> (Vec<ImageContent>, Vec<String>) {
        let Some(images) = images else {
            return (Vec::new(), Vec::new());
        };
        let resize_options = self
            .limits_model()
            .and_then(|model| model.input_limits)
            .and_then(|limits| limits.images)
            .and_then(|images| images.resize);
        let auto_resize_images = self.settings_manager.get_image_auto_resize();
        let mut normalized = Vec::new();
        let mut hints = Vec::new();
        for image in images {
            let bytes = kiana_tui::terminal_image::base64_decode(&image.data);
            let processed = kiana_core::image_process::process_image(
                &bytes,
                &image.mime_type,
                Some(kiana_core::image_process::ProcessImageOptions {
                    auto_resize_images: Some(auto_resize_images),
                    resize_options: resize_options.as_ref().map(image_resize_options_from_model),
                }),
            );
            match processed {
                kiana_core::image_process::ProcessImageResult::Ok {
                    data,
                    mime_type,
                    hints: processed_hints,
                } => {
                    normalized.push(ImageContent {
                        kind_image: kiana_ai::types::ImageKind::Image,
                        data,
                        mime_type,
                    });
                    hints.extend(processed_hints);
                }
                kiana_core::image_process::ProcessImageResult::Err { message } => {
                    hints.push(message)
                }
            }
        }
        (normalized, hints)
    }

    /// `_queueSteer`。
    async fn queue_steer(&self, text: String, images: Option<Vec<ImageContent>>) {
        lock(&self.steering_messages).push(text.clone());
        self.emit_queue_update();
        let mut content = vec![text_content(text)];
        if let Some(images) = images {
            content.extend(images.into_iter().map(UserContentBlock::Image));
        }
        self.agent
            .steer(AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(content),
                timestamp: now_ms(),
            })));
    }

    /// `_queueFollowUp`。
    async fn queue_follow_up(&self, text: String, images: Option<Vec<ImageContent>>) {
        lock(&self.follow_up_messages).push(text.clone());
        self.emit_queue_update();
        let mut content = vec![text_content(text)];
        if let Some(images) = images {
            content.extend(images.into_iter().map(UserContentBlock::Image));
        }
        self.agent
            .follow_up(AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(content),
                timestamp: now_ms(),
            })));
    }

    fn throw_if_extension_command(&self, text: &str) -> Result<(), AgentSessionError> {
        let command_name = match text.find(' ') {
            Some(index) => &text[1..index],
            None => &text[1..],
        };
        if let Some(runner) = self.runner()
            && runner.get_command(command_name).is_some()
        {
            return Err(AgentSessionError::ExtensionCommandCannotBeQueued {
                command_name: command_name.to_string(),
            });
        }
        Ok(())
    }

    /// `_queueUserInput`：steering/follow-up 的公共入口。流式时才构造
    /// `streaming_behavior`（传给 input 扩展事件），先跑 input 处理器（可能拦截或改写），
    /// 再展开 skill 命令与 prompt 模板，最后入对应队列。
    async fn queue_user_input(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        behavior: DeliverMode,
        source: InputSource,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        if text.starts_with('/') {
            self.throw_if_extension_command(&text)?;
        }
        let streaming_behavior = if self.is_agent_run_active.load(Ordering::SeqCst) {
            Some(match behavior {
                DeliverMode::Steer => StreamingBehavior::Steer,
                DeliverMode::FollowUp => StreamingBehavior::FollowUp,
            })
        } else {
            None
        };
        let Some((text, images)) = self
            .run_input_handlers(text, images, source, streaming_behavior)
            .await
        else {
            return Ok(QueuedInputDisposition::Handled);
        };
        let mut expanded = self.expand_skill_command(&text);
        expanded = kiana_resources::prompts::expand_prompt_template(
            &expanded,
            &self.prompt_templates_snapshot(),
        );
        match behavior {
            DeliverMode::Steer => self.queue_steer(expanded, images).await,
            DeliverMode::FollowUp => self.queue_follow_up(expanded, images).await,
        }
        Ok(QueuedInputDisposition::Queued)
    }

    /// `prompt()`（Core 版本；技能/prompt 模板来自命令快照）。分支顺序：
    /// `agent_settled` 派发期间入 `deferred_settled_actions`；`/` 开头先试扩展命令
    /// （命中即 Handled）；手动压缩进行中报 `CompactionInProgress`；流式时必须带
    /// `streaming_behavior`（入队返回 Queued）；否则检查模型与认证后进入 `start_run`
    /// （Started）。`preflight_result` 只在非错误的返回分支（Handled/Queued/Started）
    /// 回调；延后与出错的分支不回调。
    async fn prompt(&self, text: String, options: PromptOptions) -> Result<(), AgentSessionError> {
        if self.is_emitting_agent_settled.load(Ordering::SeqCst) {
            lock(&self.deferred_settled_actions)
                .push(DeferredSettledAction::Prompt { text, options });
            return Ok(());
        }
        let expand = options.expand_prompt_templates.unwrap_or(true);
        let preflight = options.preflight_result.clone();
        if expand && text.starts_with('/') && self.try_execute_extension_command(&text).await {
            if let Some(preflight) = &preflight {
                preflight(PromptDisposition::Handled);
            }
            return Ok(());
        }
        if lock(&self.compaction_abort).is_some() {
            return Err(AgentSessionError::CompactionInProgress);
        }
        let streaming = self.is_agent_run_active.load(Ordering::SeqCst);
        let Some((current_text, current_images)) = self
            .run_input_handlers(
                text,
                options.images.clone(),
                options.source.unwrap_or(InputSource::Interactive),
                if streaming {
                    options.streaming_behavior
                } else {
                    None
                },
            )
            .await
        else {
            if let Some(preflight) = &preflight {
                preflight(PromptDisposition::Handled);
            }
            return Ok(());
        };
        let mut expanded = current_text;
        if expand {
            expanded = self.expand_skill_command(&expanded);
            expanded = kiana_resources::prompts::expand_prompt_template(
                &expanded,
                &self.prompt_templates_snapshot(),
            );
        }
        // 新回合从 deny 计数归零开始（保留 `KIANA_HOOK_DEPTH`）；流式期间排队的
        // steering/follow-up 属于当前回合，不重置上限。
        if !streaming {
            lock(&self.hook_turn).reset_turn();
        }
        if let Some(outcome) = self.user_prompt_submit_hook(&expanded).await? {
            if let Some(reason) = outcome.blocking_error() {
                return Err(AgentSessionError::other(reason.to_string()));
            }
            if !outcome.continue_execution {
                let reason = outcome
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "Prompt submission was stopped by a hook".to_string());
                return Err(AgentSessionError::other(reason));
            }
            if let Some(context) = outcome.additional_context_xml() {
                // `additional_context_xml` 已做 XML 转义；不要二次转义。
                expanded.push_str("\n\n<hook-context>");
                expanded.push_str(&context);
                expanded.push_str("</hook-context>");
            }
        }
        if streaming {
            let Some(behavior) = options.streaming_behavior else {
                return Err(AgentSessionError::StreamingBehaviorRequired);
            };
            match behavior {
                StreamingBehavior::FollowUp => self.queue_follow_up(expanded, current_images).await,
                StreamingBehavior::Steer => self.queue_steer(expanded, current_images).await,
            }
            if let Some(preflight) = &preflight {
                preflight(PromptDisposition::Queued);
            }
            return Ok(());
        }
        let Some(model) = self.current_model() else {
            return Err(AgentSessionError::NoModelSelected);
        };
        // TS: `hasConfiguredAuth(p) || (await checkAuth(p)) !== undefined` —— 短路求值，
        // 且 checkAuth 的 rejection 会原样 reject prompt()（不能当成“未配置”）。
        let configured = if self.model_runtime.has_configured_auth(&model.provider) {
            true
        } else {
            match self
                .model_runtime
                .check_auth(&model.provider, Default::default())
                .await
            {
                Ok(check) => check.is_some(),
                Err(error) => return Err(AgentSessionError::other(error.message)),
            }
        };
        if !configured {
            if self.model_runtime.is_using_oauth(&model.provider) {
                return Err(AgentSessionError::AuthenticationFailed {
                    provider: model.provider,
                });
            }
            return Err(AgentSessionError::NoApiKey {
                provider: model.provider,
            });
        }
        let on_started: Option<Arc<dyn Fn() + Send + Sync>> = preflight.map(|preflight| {
            Arc::new(move || preflight(PromptDisposition::Started)) as Arc<dyn Fn() + Send + Sync>
        });
        self.start_run(
            BeforeAgentStartTrigger::User {
                prompt: expanded,
                images: current_images,
            },
            on_started,
        )
        .await
    }

    /// `sendCustomMessage`。投递方式按优先级：`deliverAs: nextTurn` 入 `pending_next_turn`
    /// （下一轮 `prepare_run` 并入输入）；run 中且允许触发 → agent 队列（steer/followUp）；
    /// `triggerTurn: true` → 新开一个 run（settled 派发期间则延后）；run 中不触发 →
    /// `pending_custom_messages`（turn 结束冲刷）；空闲 → 直接写会话文件。
    async fn send_custom_message(
        &self,
        mut message: CustomMessage,
        trigger_turn: Option<bool>,
        deliver_as: Option<DeliverAs>,
    ) -> Result<(), AgentSessionError> {
        // TS: `content: message.content ?? []`（未类型化扩展可能传 null/缺省）。
        if message.content.is_null() {
            message.content = JsonValue::Array(Vec::new());
        }
        let agent_message = message.to_agent_message();
        if deliver_as == Some(DeliverAs::NextTurn) {
            lock(&self.pending_next_turn_messages).push(message);
        } else if self.is_agent_run_active.load(Ordering::SeqCst) && trigger_turn != Some(false) {
            match deliver_as {
                Some(DeliverAs::FollowUp) => self.agent.follow_up(agent_message),
                _ => self.agent.steer(agent_message),
            }
        } else if trigger_turn == Some(true) {
            if self.is_emitting_agent_settled.load(Ordering::SeqCst) {
                lock(&self.deferred_settled_actions).push(DeferredSettledAction::CustomMessage {
                    message,
                    trigger_turn,
                    deliver_as,
                });
                return Ok(());
            }
            self.start_run(
                BeforeAgentStartTrigger::Custom {
                    message: message.clone(),
                },
                None,
            )
            .await?;
        } else if self.is_agent_run_active.load(Ordering::SeqCst) {
            lock(&self.pending_custom_messages).push(message);
        } else {
            // TS `_appendCustomMessage` 直接写文件，失败会 reject sendCustomMessage。
            self.append_custom_message(&message)
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
        }
        Ok(())
    }

    /// `sendUserMessage(content, options)`。
    async fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> Result<(), AgentSessionError> {
        let (text, images) = match &content {
            UserContent::Text(text) => (text.clone(), None),
            UserContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut images = Vec::new();
                for block in blocks {
                    match block {
                        UserContentBlock::Text(text) => text_parts.push(text.text.clone()),
                        UserContentBlock::Image(image) => images.push(image.clone()),
                    }
                }
                (
                    text_parts.join("\n"),
                    if images.is_empty() {
                        None
                    } else {
                        Some(images)
                    },
                )
            }
        };
        let options = options.unwrap_or_default();
        self.prompt(
            text,
            PromptOptions {
                expand_prompt_templates: Some(options.expand_prompt_templates.unwrap_or(false)),
                images,
                streaming_behavior: options.deliver_as.map(|deliver| match deliver {
                    SendUserMessageDeliverAs::Steer => StreamingBehavior::Steer,
                    SendUserMessageDeliverAs::FollowUp => StreamingBehavior::FollowUp,
                }),
                source: Some(InputSource::Extension),
                preflight_result: None,
            },
        )
        .await
    }

    /// `_prepareRun`：run 的前置阶段。对最后一条 assistant 消息做压缩检查（可抛错），
    /// 派发 `before_agent_start`（扩展可追加消息、改系统提示选项与工具集），组装本轮
    /// 输入消息（用户文本 + 图片归一化 + `deliverAs: nextTurn` 的 custom 消息 + 扩展
    /// 追加的消息），最后按 run 选项与当前消息算增量 system 消息插到最前。返回完整
    /// 的 `prompt_messages` 输入。
    async fn prepare_run(
        &self,
        trigger: BeforeAgentStartTrigger,
    ) -> Result<Vec<AgentMessage>, AgentSessionError> {
        let last_assistant = self.find_last_assistant_message();
        if let Some(last_assistant) = last_assistant {
            self.check_compaction(&last_assistant, false).await?;
        }
        let selected_before = lock(&self.base_system_prompt_options)
            .clone()
            .map(|options| options.selected_tools)
            .unwrap_or_default();
        let base_options = lock(&self.base_system_prompt_options)
            .clone()
            .as_ref()
            .map(normalized_to_build_options)
            .unwrap_or_else(|| BuildSystemPromptOptions {
                cwd: self.cwd.clone(),
                ..Default::default()
            });
        let runner = self.runner();
        let mut result = match &runner {
            Some(runner) => runner
                .emit_before_agent_start(trigger.clone(), base_options)
                .await
                .map_err(|error| AgentSessionError::other(error.to_string()))?,
            None => kiana_extensions::runner::BeforeAgentStartCombinedResult {
                messages: Vec::new(),
                system_prompt_options: normalize_build_system_prompt_options(&base_options),
            },
        };
        let handler_edited_tools = result.system_prompt_options.selected_tools.len()
            != selected_before.len()
            || result
                .system_prompt_options
                .selected_tools
                .iter()
                .zip(selected_before.iter())
                .any(|(a, b)| a != b);
        if !handler_edited_tools {
            result.system_prompt_options.selected_tools = self.get_active_tool_names();
        }

        let mut messages: Vec<AgentMessage> = Vec::new();
        let mut recall_query: Option<String> = None;
        match trigger {
            BeforeAgentStartTrigger::Custom { message } => {
                messages.push(message.to_agent_message());
            }
            BeforeAgentStartTrigger::User { prompt, images } => {
                // 恢复重放：把原 user 消息原样送进请求，不重新构造、也不写新 user entry。
                if let Some(seed) = lock(&self.replay_seed).as_ref() {
                    messages.push(seed.message.clone());
                } else {
                    let (images, hints) = self.normalize_prompt_images(images);
                    let text = if hints.is_empty() {
                        prompt
                    } else {
                        format!("{prompt}\n\n{}", hints.join("\n"))
                    };
                    recall_query = Some(text.clone());
                    let mut content = vec![text_content(text)];
                    content.extend(images.into_iter().map(UserContentBlock::Image));
                    messages.push(AgentMessage::Standard(Message::User(UserMessage {
                        role: UserRole::User,
                        content: UserContent::Blocks(content),
                        timestamp: now_ms(),
                    })));
                }
            }
        }
        // 第 10 周：每轮最多一次召回，注入 0/1 条 `<memory-context>` user 消息。
        // 消息在 `handle_agent_event` 的 MessageEnd 分支被识别并跳过持久化；它只
        // 参与当前请求，不会变成用户真实输入。
        if let Some(query) = &recall_query
            && let Some(memory) = &self.memory
            && let Some(content) = memory.recall(query).await
        {
            lock(&self.pending_memory_contexts).push(content.clone());
            messages.push(AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![text_content(content)]),
                timestamp: now_ms(),
            })));
        }
        messages.extend(
            std::mem::take(&mut *lock(&self.pending_next_turn_messages))
                .into_iter()
                .map(|message| message.to_agent_message()),
        );
        // 后台任务终态通知：父 turn 结束后任务可能才完成，通知在下一轮 run 开始时作为
        // 自定义消息进入请求与 canonical（按 task id + terminal seq 去重由 journal 保证）。
        messages.extend(
            self.take_background_notification_messages()
                .into_iter()
                .map(|message| message.to_agent_message()),
        );
        for message in result.messages {
            let custom = CustomMessage {
                role: kiana_session_core::messages::CustomRole::Custom,
                custom_type: message.custom_type,
                content: serde_json::to_value(&message.content).unwrap_or(serde_json::Value::Null),
                display: message.display,
                details: message.details,
                timestamp: now_ms() as f64,
            };
            messages.push(custom.to_agent_message());
        }
        let current_messages = self.agent.messages();
        let update_message =
            self.prepare_prompt_and_tool_loadout(&result.system_prompt_options, &current_messages);
        *lock(&self.run_system_prompt_options) = Some(result.system_prompt_options);
        if let Some(update_message) = update_message {
            messages.insert(0, AgentMessage::Standard(Message::System(update_message)));
        }
        Ok(messages)
    }

    /// `_startRun` 的入口：执行所有权拒绝与 run 序号在这里处理，主体委托
    /// [`Self::start_run_inner`]。
    async fn start_run(
        &self,
        trigger: BeforeAgentStartTrigger,
        on_started: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<(), AgentSessionError> {
        if let Some(refusal) = &self.execution_refusal {
            return Err(AgentSessionError::other(format!(
                "Cannot start a run: {refusal}"
            )));
        }
        self.run_sequence.fetch_add(1, Ordering::SeqCst);
        // 第 11 周后半：execution claim 是 per-run 的。会话构造时已经取得首个 claim；
        // 每次 run 结束（成功/失败/取消）都会在下面释放并发布 Terminal 事件，因此后续
        // run 在这里重新获取（另一个存活进程在此期间可以接手）。
        if let Some(continuity) = &self.continuity {
            continuity
                .begin_run(&self.session_id())
                .map_err(AgentSessionError::other)?;
        }
        let result = self.start_run_inner(trigger, on_started).await;
        // 重放 seed 只对一次 run 有效；即使没有收到 MessageEnd（早退/取消）也不能泄漏到
        // 下一轮。
        lock(&self.replay_seed).take();
        if let Some(continuity) = &self.continuity {
            let aborted = self.agent_run_abort_requested.load(Ordering::SeqCst);
            let status = if aborted {
                crate::execution::ExecutionStatus::Cancelled
            } else if result.is_ok() {
                crate::execution::ExecutionStatus::Completed
            } else {
                crate::execution::ExecutionStatus::Failed
            };
            continuity.finish_run(status, result.as_ref().err().map(|error| error.to_string()));
        }
        result
    }

    /// `_startRun`：一次完整 run 的驱动器。阶段划分：
    /// 1. 复位运行标志与错误槽，`prepare_run` 组装本轮输入消息（压缩检查 + before_agent_start）；
    /// 2. 冲刷 bash/custom 待写消息，调用 `on_started`（preflight），记录模型选择；
    /// 3. 循环：`prompt_messages` / `continue_` 推进 agent，`handle_post_agent_run` 决定
    ///    重试/压缩/继续，`run_before_settle_boundary` 决定是否再续一轮；
    /// 4. 收尾：清 failed_response、冲刷待写消息，派发 `agent_settled`（其错误优先于
    ///    持久化错误、持久化错误优先于 run 本身的错误返回，对应 TS finally 的覆盖语义）。
    ///
    /// 错误来源：`prepare_run` 的检查（模型/认证/压缩）、`prompt_messages`/`continue_`
    /// 的 `AgentError`（经 `to_string` 包装为 `AgentSessionError::other`）、
    /// 以及监听器记录的 `persistence_error`；取消（abort）不返回错误，只提前退出循环。
    async fn start_run_inner(
        &self,
        trigger: BeforeAgentStartTrigger,
        on_started: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<(), AgentSessionError> {
        self.is_agent_run_active.store(true, Ordering::SeqCst);
        self.agent_run_abort_requested
            .store(false, Ordering::SeqCst);
        lock(&self.pending_stream_error).take();
        // 每次 run 都是新的错误作用域（TS 的异常按调用传播）。
        self.take_persistence_error();
        // 上一次 run 若在 MessageEnd 前中止，未消费的召回消息不能留到下一轮。
        lock(&self.pending_memory_contexts).clear();
        let messages = match self.prepare_run(trigger).await {
            Ok(messages) => messages,
            Err(error) => {
                self.is_agent_run_active.store(false, Ordering::SeqCst);
                self.flush_pending_bash_messages();
                self.flush_pending_custom_messages();
                // TS 的 catch 分支里 flush 抛出的错误会替换掉原始错误。
                if let Some(message) = self.take_persistence_error() {
                    return Err(AgentSessionError::other(message));
                }
                self.resolve_idle_wait_if_idle();
                return Err(error);
            }
        };
        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        // TS 在 try 之前 flush，写失败会中断 run（这里同样返回错误；额外复位 run 状态，
        // 避免把会话卡在“运行中”，见交付报告 deviations）。
        if let Some(message) = self.take_persistence_error() {
            self.is_agent_run_active.store(false, Ordering::SeqCst);
            self.resolve_idle_wait_if_idle();
            return Err(AgentSessionError::other(message));
        }
        if let Some(on_started) = &on_started {
            on_started();
        }
        lock(&self.failed_response).take();
        if let Err(error) = self.record_selection() {
            // TS 在这里抛错会中断 run（并且漏掉复位，见 deviations；这里复位避免卡死）。
            self.is_agent_run_active.store(false, Ordering::SeqCst);
            self.resolve_idle_wait_if_idle();
            return Err(error);
        }
        lock(&self.pending_tool_names).clear();

        let run = async {
            self.agent
                .prompt_messages(messages)
                .await
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
            loop {
                if self.agent_run_abort_requested.load(Ordering::SeqCst) {
                    break;
                }
                // 监听器记录的持久化失败：TS 里监听器抛错会立刻中断 agent.prompt。
                if lock(&self.persistence_error).is_some() {
                    break;
                }
                if self.handle_post_agent_run().await? {
                    if self.agent_run_abort_requested.load(Ordering::SeqCst) {
                        break;
                    }
                    self.agent
                        .continue_()
                        .await
                        .map_err(|error| AgentSessionError::other(error.to_string()))?;
                    continue;
                }
                if self.agent_run_abort_requested.load(Ordering::SeqCst)
                    || !self.run_before_settle_boundary().await
                {
                    break;
                }
                if self.agent_run_abort_requested.load(Ordering::SeqCst) {
                    break;
                }
                self.agent
                    .continue_()
                    .await
                    .map_err(|error| AgentSessionError::other(error.to_string()))?;
            }
            Ok::<(), AgentSessionError>(())
        }
        .await;

        if self.agent_run_abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
        }
        lock(&self.failed_response).take();
        *lock(&self.run_system_prompt_options) = None;
        self.flush_pending_bash_messages();
        self.flush_pending_custom_messages();
        let persistence_error = self.take_persistence_error();
        // TS 的 finally 里 `await _emitAgentSettled()` 抛错会覆盖原始结果。
        let settled = self.emit_agent_settled().await;
        match settled {
            Err(error) => Err(error),
            Ok(()) => match persistence_error {
                Some(message) => Err(AgentSessionError::other(message)),
                None => run,
            },
        }
    }

    /// `_emitAgentSettled`。
    async fn emit_agent_settled(&self) -> Result<(), AgentSessionError> {
        if let Some(warmer) = &self.cache_warmer {
            warmer.on_agent_settled();
        }
        self.is_agent_run_active.store(false, Ordering::SeqCst);
        self.is_emitting_agent_settled.store(true, Ordering::SeqCst);
        let aborted = self.agent_run_abort_requested.load(Ordering::SeqCst);
        if let Some(runner) = self.runner() {
            let _ = runner
                .emit(ExtensionEvent::AgentSettled(AgentSettledEvent {
                    event_type: AgentSettledEventType::AgentSettled,
                    aborted,
                }))
                .await;
        }
        self.emit(AgentSessionEvent::AgentSettled { aborted });
        self.is_emitting_agent_settled
            .store(false, Ordering::SeqCst);

        // TS: `const deferred = this._deferredSettledActions.splice(0)` —— 一次性快照；
        // 处理期间新入队的动作不会被同一批执行。动作的错误会向外抛出（最后 reject
        // 原始 prompt/startRun），因此这里同样传播。
        let deferred = std::mem::take(&mut *lock(&self.deferred_settled_actions));
        if deferred.is_empty() {
            self.resolve_idle_wait_if_idle();
            return Ok(());
        }
        let result = async {
            for action in deferred {
                match action {
                    DeferredSettledAction::Prompt { text, options } => {
                        Box::pin(self.prompt(text, options)).await?;
                    }
                    DeferredSettledAction::CustomMessage {
                        message,
                        trigger_turn,
                        deliver_as,
                    } => {
                        Box::pin(self.send_custom_message(message, trigger_turn, deliver_as))
                            .await?;
                    }
                }
            }
            Ok(())
        }
        .await;
        self.resolve_idle_wait_if_idle();
        result
    }

    fn is_streaming_active(&self) -> bool {
        self.is_agent_run_active.load(Ordering::SeqCst)
    }

    fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        let steering = std::mem::take(&mut *lock(&self.steering_messages));
        let follow_up = std::mem::take(&mut *lock(&self.follow_up_messages));
        self.agent.clear_all_queues();
        self.emit_queue_update();
        (steering, follow_up)
    }

    fn request_abort(&self) {
        if self.is_agent_run_active.load(Ordering::SeqCst) {
            self.agent_run_abort_requested.store(true, Ordering::SeqCst);
        }
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        if self.is_before_settle.load(Ordering::SeqCst) {
            self.abort_during_before_settle
                .store(true, Ordering::SeqCst);
        }
        self.agent.abort();
    }

    async fn abort(&self) {
        self.request_abort();
        self.wait_for_idle().await;
    }

    fn abort_compaction(&self) {
        if let Some(token) = lock(&self.compaction_abort).as_ref() {
            token.cancel();
        }
        if let Some(token) = lock(&self.auto_compaction_abort).as_ref() {
            token.cancel();
        }
    }

    fn abort_branch_summary(&self) {
        if let Some(token) = lock(&self.branch_summary_abort).as_ref() {
            token.cancel();
        }
    }

    /// `AgentSession.compact`：手动压缩。先 `abort()`（等 idle），登记 `compaction_abort`
    /// 令牌，然后与自动压缩同构：`session_before_compact`（可取消/可替换结果）→ 默认或
    /// 扩展压缩 → 落盘 → 事件。区别是没有 `will_retry`，且失败消息带 `Compaction failed:`
    /// 前缀。`NothingToCompact`/`AlreadyCompacted` 的判定来自 `prepare_compaction`
    /// 失败时分支末尾条目的类型。
    async fn compact(
        &self,
        custom_instructions: Option<String>,
        condensation_config: Option<CondensationConfig>,
    ) -> Result<CompactionResult, AgentSessionError> {
        self.abort().await;
        // 基线必须在提交压缩前采集：压缩会把旧消息移出 projection，之后再取就只剩
        // 压缩后的值。`saved_tokens` 以同一估算器（`estimate_messages_tokens`）的差值
        // 为准，与 `context_tokens()` 暴露给调用方的数字一致。
        let baseline_tokens = self.estimate_messages_tokens();
        let compaction_started = std::time::Instant::now();
        let token = CancellationToken::new();
        *lock(&self.compaction_abort) = Some(token.clone());
        self.emit(AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        });
        let mut from_extension = false;
        let mut cancelled_by_extension = false;
        let outcome: Result<CompactionResult, AgentSessionError> = async {
            let Some(model) = self.current_model() else {
                return Err(AgentSessionError::NoModelSelected);
            };
            let settings = self.compaction_settings(Some(&model));
            let path_entries = self.branch();
            let history_snapshot = CompactionHistorySnapshot::capture(&path_entries)
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
            let history_hash = history_snapshot.hash();
            // 手动压缩忽略 no-op 缓存（用户显式要求），但仍复用 prune/事务/fallback。
            let prune_config = self.plan_prune_config();
            let (pruned, preparation) =
                prepare_with_pruning(&path_entries, &settings, &prune_config);
            let Some(preparation) = preparation else {
                let last = path_entries.last();
                if last.is_some_and(|entry| entry.kind() == "compaction") {
                    return Err(AgentSessionError::AlreadyCompacted);
                }
                return Err(AgentSessionError::NothingToCompact);
            };
            let read_state = Arc::clone(&self.read_state);
            let read_snapshots = tokio::task::spawn_blocking(move || read_state.snapshots(5))
                .await
                .unwrap_or_default();
            let summary_start = self.with_session(HistorySnapshot::capture);
            // PreCompact：在扩展压缩决定与真正压缩之前运行；deny 取消手动压缩，
            // additionalContext 追加到自定义指令（已转义，不再二次转义）。
            let mut custom_instructions = custom_instructions;
            if let Some(outcome) = self
                .pre_compact_hook("manual", custom_instructions.as_deref())
                .await
            {
                if outcome.is_denied() || !outcome.continue_execution {
                    return Err(AgentSessionError::CompactionCancelled);
                }
                if let Some(context) = outcome.additional_context_xml() {
                    let mut instructions = custom_instructions.take().unwrap_or_default();
                    if !instructions.is_empty() {
                        instructions.push_str("\n\n");
                    }
                    instructions.push_str(&context);
                    custom_instructions = Some(instructions);
                }
            }
            let mut extension_compaction: Option<CompactionResult> = None;
            if let Some(runner) = self.runner()
                && runner.has_handlers("session_before_compact") {
                    let result = runner
                        .emit(ExtensionEvent::Session(SessionEvent::BeforeCompact(
                            kiana_ext_api::events::SessionBeforeCompactEvent {
                                event_type: kiana_ext_api::events::SessionBeforeCompactEventType::SessionBeforeCompact,
                                preparation: preparation.clone(),
                                branch_entries: path_entries.clone(),
                                custom_instructions: custom_instructions.clone(),
                                reason: CompactionReason::Manual,
                                will_retry: false,
                                signal: token.clone(),
                            },
                        )))
                        .await;
                    if let Ok(Some(ExtensionEventResult::SessionBeforeCompact(result))) = result {
                        if result.cancel == Some(true) {
                            cancelled_by_extension = true;
                            return Err(AgentSessionError::CompactionCancelled);
                        }
                        if let Some(compaction) = result.compaction {
                            extension_compaction = Some(compaction);
                            from_extension = true;
                        }
                    }
                }
            let compaction = match extension_compaction {
                Some(compaction) => compaction,
                None => match self
                    .run_default_compaction(
                        &preparation,
                        &model,
                        custom_instructions.clone(),
                        token.clone(),
                        CompactionReason::Manual,
                    )
                    .await
                {
                    Ok(compaction) => compaction,
                    Err(error) => match build_fallback_result(&preparation, &FallbackConfig::default())
                    {
                        Some(fallback) => fallback.result,
                        None => return Err(error),
                    },
                },
            };
            if token.is_cancelled() {
                return Err(AgentSessionError::CompactionCancelled);
            }
            // 与自动压缩相同的 all-or-nothing 提交：prune context_edit + compaction。
            self.with_session_mut(|manager| -> Result<(), AgentSessionError> {
                if !summary_start.matches(manager) {
                    return Err(AgentSessionError::other(
                        "session history changed while compaction was in progress; refusing to commit",
                    ));
                }
                let mut transaction = CommitTransaction::begin(manager);
                if pruned.applied {
                    for record in &pruned.pruned {
                        match manager.append_context_edit(
                            &record.entry_id,
                            Some(ContextEditReplacement {
                                content: JsonValue::String(record.notice.clone()),
                            }),
                        ) {
                            Ok(edit_id) => transaction.record(&edit_id),
                            Err(error) => {
                                transaction.rollback(manager);
                                return Err(AgentSessionError::other(error.to_string()));
                            }
                        }
                    }
                }
                let after_edits = HistorySnapshot::capture(manager);
                match append_compaction_transactional(
                    manager,
                    &after_edits,
                    &compaction.summary,
                    Some(&compaction.first_kept_entry_id),
                    compaction.tokens_before as f64,
                    compaction.details.clone(),
                    Some(from_extension),
                    compaction.usage.clone(),
                ) {
                    Ok(id) => {
                        transaction.record(&id);
                        Ok(())
                    }
                    Err(error) => {
                        transaction.rollback(manager);
                        Err(AgentSessionError::other(error.to_string()))
                    }
                }
            })?;
            self.refresh_finalized_context();
            self.refresh_memory_after_compaction();
            // 压缩删除 read 结果后，旧的读记录不能再授权 write/edit；随后按压缩前
            // 快照恢复仍能验证指纹的文件。
            self.read_state.clear();
            let read_state = Arc::clone(&self.read_state);
            let snapshots = read_snapshots.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for snapshot in &snapshots {
                    let _ = read_state.restore_snapshot(snapshot);
                }
            })
            .await;
            let attachment = build_recovery_attachment(
                &RecoveryInput {
                    files: read_snapshots.clone(),
                    skills: collect_called_skills(&path_entries, 20),
                    plan: self.plan_recovery(),
                    deltas: collect_context_deltas(&path_entries),
                },
                &RecoveryBudget::default(),
            );
            *lock(&self.pending_recovery_attachment) =
                (!attachment.text.trim().is_empty()).then_some(attachment.text);
            self.compaction_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(
                    history_hash,
                    AttemptOutcome::Succeeded,
                    std::time::Instant::now(),
                );
            self.post_compact_hook("manual", custom_instructions.as_deref())
                .await;
            let estimated_tokens_after = self.estimate_messages_tokens();
            let _ = self
                .telemetry
                .record_compaction(&CompactionRecord {
                    trigger: "manual".to_string(),
                    duration_ms: compaction_started.elapsed().as_secs_f64() * 1_000.0,
                    input_tokens: compaction.tokens_before,
                    output_tokens: estimated_tokens_after,
                    outcome: "ok".to_string(),
                })
                .await;
            // TS：`newEntries.find(...)` 取第一条摘要相同的压缩条目。
            if let Some(entry) = self.first_compaction_entry_by_summary(&compaction.summary)
                && let Some(runner) = self.runner() {
                    let _ = runner
                        .emit(ExtensionEvent::Session(SessionEvent::Compact(SessionCompactEvent {
                            event_type: SessionCompactEventType::SessionCompact,
                            compaction_entry: entry,
                            from_extension,
                            reason: CompactionReason::Manual,
                            will_retry: false,
                        })))
                        .await;
                }
            let result = CompactionResult {
                estimated_tokens_after: Some(estimated_tokens_after),
                ..compaction
            };
            self.clear_manual_compaction_state();
            self.emit(AgentSessionEvent::CompactionEnd {
                reason: CompactionReason::Manual,
                result: Some(result.clone()),
                aborted: false,
                will_retry: false,
                error_message: None,
            });
            // 提交后的收敛观测：token 已经下降且保持稳定才算成功；超时走 Failed。
            // 事件在 `CompactionEnd` 之后发出，UI 可以先重建压缩后的聊天区，再显示
            // 精确的 saved_tokens。
            //
            // 先做一次同步检查：压缩已提交且历史不会再变，若 token 没有下降（小会话的
            // 摘要可能比原文更长），没有必要空等 90s 超时；直接报 Failed。
            let after_tokens = self.estimate_messages_tokens();
            let condensation = if after_tokens >= baseline_tokens {
                CondensationEvent {
                    phase: CondensationPhase::Failed,
                    before_tokens: baseline_tokens,
                    after_tokens,
                    saved_tokens: 0,
                    elapsed_ms: compaction_started.elapsed().as_millis() as u64,
                    error: Some(format!(
                        "compaction did not reduce context tokens ({baseline_tokens} -> {after_tokens})"
                    )),
                }
            } else {
                observe_condensation(
                    baseline_tokens,
                    || async { Some(self.estimate_messages_tokens()) },
                    condensation_config.unwrap_or_default(),
                )
                .await
            };
            *lock(&self.last_condensation) = Some(condensation.clone());
            self.emit(AgentSessionEvent::Condensation { event: condensation });
            Ok(result)
        }
        .await;
        match outcome {
            Ok(result) => Ok(result),
            Err(error) => {
                let aborted = token.is_cancelled() || cancelled_by_extension;
                let error_message = if aborted {
                    None
                } else {
                    Some(format!("Compaction failed: {}", error))
                };
                // 失败/取消也发 CondensationEvent（Failed），否则调用方只能看到
                // compaction_end 的 errorMessage，无法区分“压缩没发生”和“收敛超时”。
                let after_tokens = self.estimate_messages_tokens();
                let condensation = CondensationEvent {
                    phase: CondensationPhase::Failed,
                    before_tokens: baseline_tokens,
                    after_tokens,
                    saved_tokens: baseline_tokens.saturating_sub(after_tokens),
                    elapsed_ms: compaction_started.elapsed().as_millis() as u64,
                    error: Some(error_message.clone().unwrap_or_else(|| error.to_string())),
                };
                self.clear_manual_compaction_state();
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: error_message.clone(),
                });
                self.emit_session_compact_failed(SessionCompactFailedEvent {
                    event_type: SessionCompactFailedEventType::SessionCompactFailed,
                    reason: CompactionReason::Manual,
                    error_message,
                    aborted,
                    will_retry: false,
                    from_extension,
                })
                .await;
                *lock(&self.last_condensation) = Some(condensation.clone());
                self.emit(AgentSessionEvent::Condensation {
                    event: condensation,
                });
                Err(error)
            }
        }
    }

    /// `_runDefaultCompaction` 之外的 bash 记录。
    fn record_bash_result(
        &self,
        command: String,
        result: &BashResult,
        exclude_from_context: Option<bool>,
    ) -> Result<(), AgentSessionError> {
        let message = BashExecutionMessage {
            role: kiana_session_core::messages::BashExecutionRole::BashExecution,
            command,
            output: result.output.clone(),
            exit_code: result.exit_code.map(|code| code as f64),
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
            timestamp: now_ms() as f64,
            exclude_from_context,
        };
        if self.is_agent_run_active.load(Ordering::SeqCst) {
            lock(&self.pending_bash_messages).push(message);
        } else {
            self.with_session_mut(|manager| manager.append_message(message.to_agent_message()))
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
            self.refresh_finalized_context();
        }
        Ok(())
    }
}

/// `NormalizedBuildSystemPromptOptions` → `BuildSystemPromptOptions`。
fn normalized_to_build_options(
    options: &NormalizedBuildSystemPromptOptions,
) -> BuildSystemPromptOptions {
    BuildSystemPromptOptions {
        custom_prompt: options.custom_prompt.clone(),
        force_system_prompt: options.force_system_prompt.clone(),
        selected_tools: Some(options.selected_tools.clone()),
        hidden_tools: Some(options.hidden_tools.clone()),
        tool_snippets: Some(options.tool_snippets.clone()),
        tool_guidelines: Some(options.tool_guidelines.clone()),
        prompt_guidelines: Some(options.prompt_guidelines.clone()),
        append_system_prompt: Some(options.append_system_prompt.clone()),
        sections: Some(options.sections.clone()),
        cwd: options.cwd.clone(),
        context_files: Some(options.context_files.clone()),
        skills: Some(options.skills.clone()),
    }
}

/// 把扩展事件异步派发（TS 的 `void runner.emit(...)`）。
///
/// 派发到本线程的常驻派发表上：即使调用点位于另一次派发的内部（例如扩展 handler 里
/// `kiana.setSessionName`），事件也不会随外层派发结束被丢弃。
fn spawn_local_event(runner: Arc<ExtensionRunner>, event: ExtensionEvent) {
    spawn_local_dispatch(async move {
        let _ = runner.emit(event).await;
    });
}

/// `kiana-ai` 的 `streamSimple` 在 `Agent` 的 [`StreamFn`] 形态下的规范实例。
///
/// TS: `agent-session.ts#_getSummarizationRequestAuth` 用
/// `agent.streamFunction === streamSimple` 判断请求是否走 kiana 自带的鉴权路径。Rust 的
/// `StreamFn` 是 `Arc<dyn Fn>`，函数身份只能表达为同一个 `Arc` 指针；宿主构造 `Agent`
/// 时使用本函数（而不是自己包一层闭包）即可保持该判断。
pub fn stream_simple_stream_fn() -> StreamFn {
    static STREAM_SIMPLE: std::sync::OnceLock<StreamFn> = std::sync::OnceLock::new();
    Arc::clone(STREAM_SIMPLE.get_or_init(|| {
        let stream: StreamFn = Arc::new(
            |model: Model,
             context: kiana_ai::types::TranscriptContext,
             options: Option<kiana_ai::types::SimpleStreamOptions>| {
                Box::pin(async move {
                    let mut options = options.unwrap_or_default();
                    // 第 9 周：发网络前执行能力门 → 预检 → 输出预算 → cache 语义。
                    // 任何失败都返回 `Err`，由 agent loop 生成 error assistant 消息，
                    // provider 侧零请求。
                    apply_session_request_policy(&model, &context, &mut options)?;
                    Ok(kiana_ai::compat::stream_simple(
                        &model,
                        context.as_context(),
                        options,
                    ))
                })
            },
        );
        stream
    }))
}

/// 在 provider 请求前应用 `request_policy`（第 9 周）。
///
/// - 能力门：根据模型 `input` 声明 manifest/adapter 能力；上下文含图片而模型不支持时
///   返回 `Unsupported`（零 HTTP）；
/// - 输入预检 + 输出预算：`plan.max_tokens` 写回请求选项；超窗返回 typed 错误；
/// - cache 语义：`ExplicitBreakpoints` 时组合 `onPayload`，在最终 wire payload 上注入断点。
// 返回值必须保持 `AiError`：调用方是 `StreamFn` 闭包，错误类型要与 `stream_simple`
// 的签名一致；boxing 会改变错误类型并波及闭包签名。
#[allow(clippy::result_large_err)]
fn apply_session_request_policy(
    model: &Model,
    context: &kiana_ai::types::TranscriptContext,
    options: &mut kiana_ai::types::SimpleStreamOptions,
) -> Result<(), kiana_ai::AiError> {
    use kiana_ai::request_policy::{
        CapabilitySet, RequestPolicyInput, RequestRequirements, policy_ai_error, prepare_request,
    };
    // 脚本化 faux provider 不执行 provider 侧的窗口/能力限制；保持既有测试轨迹。
    if model.provider == kiana_ai::providers::faux::DEFAULT_PROVIDER {
        return Ok(());
    }
    let messages = &context.as_context().messages;
    let vision = messages.iter().any(message_contains_image);
    let supports_vision = model
        .input
        .contains(&kiana_ai::types::ModelInputModality::Image);
    let manifest = CapabilitySet::language_chat().with_vision(supports_vision);
    let adapter = CapabilitySet::language_chat().with_vision(supports_vision);
    let requirements = RequestRequirements {
        tools: !kiana_ai::utils::transcript::get_current_tools(messages).is_empty(),
        vision,
        ..RequestRequirements::default()
    };
    let mut input = RequestPolicyInput::new(model, context, &manifest, &adapter, &requirements);
    input.caller_max_tokens = options.base.max_tokens;
    let plan = prepare_request(&input).map_err(policy_ai_error)?;
    options.base.max_tokens = Some(plan.max_tokens);
    if plan.cache.uses_explicit_breakpoints() {
        wrap_on_payload_with_cache_breakpoints(options, plan.cache);
    }
    Ok(())
}

/// 组合 `onPayload` 与 cache 断点注入：先让调用方回调改写 payload，再在最终载荷上标记。
fn wrap_on_payload_with_cache_breakpoints(
    options: &mut kiana_ai::types::SimpleStreamOptions,
    cache: kiana_ai::request_policy::CacheSemantics,
) {
    let original_async = options.base.on_payload_async.clone();
    let original_sync = options.base.on_payload.clone();
    options.base.on_payload_async = Some(Arc::new(move |mut payload, model| {
        let original_async = original_async.clone();
        let original_sync = original_sync.clone();
        Box::pin(async move {
            if let Some(callback) = &original_async {
                if let Some(replaced) = callback(payload.clone(), model).await {
                    payload = replaced;
                }
            } else if let Some(callback) = &original_sync
                && let Some(replaced) = callback(payload.clone(), model)
            {
                payload = replaced;
            }
            kiana_ai::request_policy::apply_cache_breakpoints(&mut payload, cache);
            Some(payload)
        })
    }));
    options.base.on_payload = None;
}

/// 消息里是否含图片块（user / toolResult）。
fn message_contains_image(message: &kiana_ai::types::Message) -> bool {
    match message {
        kiana_ai::types::Message::User(user) => match &user.content {
            kiana_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, kiana_ai::types::UserContentBlock::Image(_))),
            kiana_ai::types::UserContent::Text(_) => false,
        },
        kiana_ai::types::Message::ToolResult(result) => result
            .content
            .iter()
            .any(|block| matches!(block, kiana_ai::types::UserContentBlock::Image(_))),
        _ => false,
    }
}

/// 判读一个 JS 异常值在 TS 里是否是 `Error` 实例。
///
/// `kiana-extensions` 把抛出的原始值物化成 [`kiana_extensions::runner::DispatchError::Js`]：
/// prelude 的 `__kiana.errorText` 对 `instanceof Error` 给出真实 `stack`，非 Error 值固定
/// `name = "Error"` 且没有 `stack`。TS 的 `_beforeToolCall` 只对非 Error 值加前缀。
fn js_thrown_value_is_error(error: &kiana_extensions::runner::DispatchError) -> bool {
    match error {
        kiana_extensions::runner::DispatchError::Js { name, stack, .. } => {
            stack.is_some() || name != "Error"
        }
        _ => true,
    }
}

/// 模型同一性判定：只比较 provider 与 id，忽略 base_url 等其余字段；任一侧为
/// `None` 即不等。
fn models_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.provider == b.provider && a.id == b.id,
        _ => false,
    }
}

/// 设置项 `SteeringMode`（config crate）↔ agent-core `QueueMode` 的双向映射。
/// 两个枚举变体一一对应，仅命名不同；拆成两个函数是因为两侧类型分属不同 crate。
fn queue_mode_from_settings(mode: SteeringMode) -> QueueMode {
    match mode {
        SteeringMode::All => QueueMode::All,
        SteeringMode::OneAtATime => QueueMode::OneAtATime,
    }
}

fn steering_mode_from_queue(mode: QueueMode) -> SteeringMode {
    match mode {
        QueueMode::All => SteeringMode::All,
        QueueMode::OneAtATime => SteeringMode::OneAtATime,
    }
}

/// thinking level 的线格式（`serde` 的 JSON 表示）。
fn thinking_level_text(level: ModelThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "off".to_string())
}

/// `executeBash` 的选项。
#[derive(Clone, Default)]
pub struct ExecuteBashOptions {
    /// 输出不进入 LLM 上下文（`!!` 前缀）。
    pub exclude_from_context: Option<bool>,
    /// 包含在 bash 更新事件里的标识。
    pub id: Option<String>,
    /// 自定义执行操作。
    pub operations: Option<Arc<dyn BashOperations>>,
}

/// 拥有型工具名匹配器（`create_tool_name_matcher` 的返回值借用入参）。
fn create_owned_tool_matcher(
    entries: Vec<String>,
) -> impl Fn(&str) -> bool + Send + Sync + 'static {
    let mut names: HashSet<String> = HashSet::new();
    let mut patterns: Vec<regex::Regex> = Vec::new();
    for entry in entries {
        if entry.contains('*') {
            let source = entry
                .split('*')
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(".*");
            if let Ok(pattern) = regex::Regex::new(&format!("^{source}$")) {
                patterns.push(pattern);
            }
        } else {
            names.insert(entry.clone());
        }
    }
    move |name: &str| names.contains(name) || patterns.iter().any(|pattern| pattern.is_match(name))
}

/// 线程本地资源加载器注册表的键/计数结构，见 [`RESOURCE_LOADERS`] 的字段文档。
type ResourceLoaderRegistry =
    std::cell::RefCell<std::collections::HashMap<usize, (Arc<dyn ResourceLoader>, usize)>>;

thread_local! {
    /// `ResourceLoader` 的线程本地注册表。`ResourceLoader` 没有 `Send + Sync` 约束，
    /// 因此 `AgentSession` 只保存键；这样会话句柄本身仍然是 `Send + Sync`（监听器要求）。
    ///
    /// 值里的计数是同一加载器的登记次数：多个会话（或被替换的会话）可以共享同一个
    /// `Arc<dyn ResourceLoader>`；`dispose` 一个会话只能移除它自己的登记。
    static RESOURCE_LOADERS: ResourceLoaderRegistry =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 把加载器登记进线程本地注册表，返回它的注册 id（`Arc` 指针地址）。同一加载器
/// 重复登记只递增计数，计数见 [`RESOURCE_LOADERS`] 文档。
fn register_loader(loader: &Arc<dyn ResourceLoader>) -> usize {
    let id = Arc::as_ptr(loader) as *const () as usize;
    RESOURCE_LOADERS.with(|loaders| {
        let mut loaders = loaders.borrow_mut();
        match loaders.get_mut(&id) {
            Some((_, count)) => *count += 1,
            None => {
                loaders.insert(id, (Arc::clone(loader), 1));
            }
        }
    });
    id
}

/// 递减加载器的登记计数；计数归零（或最后一笔）时才真正移除。
fn unregister_loader(id: usize) {
    RESOURCE_LOADERS.with(|loaders| {
        let mut loaders = loaders.borrow_mut();
        let remove = match loaders.get_mut(&id) {
            Some((_, count)) if *count > 1 => {
                *count -= 1;
                false
            }
            Some(_) => true,
            None => false,
        };
        if remove {
            loaders.remove(&id);
        }
    });
}

/// 按注册 id 取回加载器的 `Arc` 副本；`dispose` 之后（计数归零）返回 `None`。
fn lookup_loader(id: usize) -> Option<Arc<dyn ResourceLoader>> {
    RESOURCE_LOADERS.with(|loaders| {
        loaders
            .borrow()
            .get(&id)
            .map(|(loader, _count)| Arc::clone(loader))
    })
}

thread_local! {
    /// `Core` 的线程本地注册表（`Arc` 指针 → `Weak`）。用于让要求 `Send + Sync` 的扩展
    /// 回调（扩展动作、Agent 钩子）只捕获 `usize` 键，而不是非 `Send` 的 `Core`。
    static CORE_REGISTRY: std::cell::RefCell<std::collections::HashMap<usize, Weak<Core>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 把 `Core` 登记进线程本地注册表并返回注册 id（`Arc` 指针地址）。
fn register_core(core: &Arc<Core>) -> usize {
    let id = Arc::as_ptr(core) as usize;
    CORE_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(id, Arc::downgrade(core));
    });
    id
}

/// 从注册表移除会话核心；移除后钩子里的 `lookup_core` 返回 `None`（会话已释放）。
fn unregister_core(id: usize) {
    CORE_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&id);
    });
}

/// 按注册 id 升级出 `Arc<Core>`；会话已 `dispose`（弱引用悬空）时返回 `None`。
fn lookup_core(id: usize) -> Option<Arc<Core>> {
    CORE_REGISTRY.with(|registry| registry.borrow().get(&id).and_then(Weak::upgrade))
}

thread_local! {
    /// 本线程的**常驻**扩展派发 `LocalSet`。
    ///
    /// 扩展运行时（`ExtensionRunner` / JS 引擎）是 `!Send`，只能在会话线程上执行；而
    /// [`Agent`] 的钩子 future 要求 `Send`。桥的做法：把 `!Send` 的派发 future
    /// `spawn_local` 到这张常驻表上，`Send` 侧只 await 一个 `oneshot`。
    ///
    /// 这张表由宿主线程上的驱动任务持续运行（`run_until(pending)`），因此：
    /// - 派发中再 `spawn_local` 的宿主异步（如 `ctx.compact()`）不会随单次派发结束被丢弃；
    /// - 等待期间调用方让出线程，定时器 / IO / abort 监视器可以继续推进；
    /// - 没有超时 panic：与 TS 一样一直等到完成。
    ///
    /// 必须在宿主线程的 `LocalSet` 上下文里安装（[`AgentSession::new`] 会做这件事）；
    /// 安装失败时退化为 [`run_local_blocking`] 的同步轮询。
    static BRIDGE_LOCAL: std::cell::RefCell<Option<Rc<tokio::task::LocalSet>>> =
        const { std::cell::RefCell::new(None) };
    /// 常驻派发 driver 的最近心跳。`start_local` 在写 journal 之前用它确认 executor
    /// 仍在被宿主驱动：宿主丢弃/不再驱动临时 `LocalSet` 时，driver 不会再更新心跳。
    static BRIDGE_HEARTBEAT: std::cell::RefCell<
        Option<Rc<std::cell::Cell<Option<std::time::Instant>>>>,
    > = const { std::cell::RefCell::new(None) };
}

/// 心跳间隔（driver 在 LocalSet 上周期醒来）。
const BRIDGE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// 心跳新鲜度阈值。
const BRIDGE_HEARTBEAT_FRESH: std::time::Duration = std::time::Duration::from_millis(300);
/// `start_local` 等待一次心跳的最长时间；宿主正在驱动时 driver 会在下一个 tick 更新。
const BRIDGE_HEARTBEAT_WAIT: std::time::Duration = std::time::Duration::from_millis(150);

/// 常驻派发 `LocalSet` 的驱动任务被丢弃时，清掉线程本地登记，避免后续会话在
/// 已停止的 `LocalSet` 上排队（同一线程顺序运行多个 `LocalSet` 的场景）。
struct BridgeGuard {
    id: usize,
}

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        let stale = BRIDGE_LOCAL.with(|slot| {
            let stale = slot
                .borrow()
                .as_ref()
                .is_some_and(|local| Rc::as_ptr(local) as *const () as usize == self.id);
            if stale {
                *slot.borrow_mut() = None;
            }
            stale
        });
        if stale {
            BRIDGE_HEARTBEAT.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// 安装（或复用）本线程的常驻派发 `LocalSet`。返回 `None` 表示当前线程不在
/// `LocalSet` 上下文（无法 `spawn_local`）。
fn bridge_local() -> Option<Rc<tokio::task::LocalSet>> {
    BRIDGE_LOCAL.with(|slot| {
        if let Some(local) = slot.borrow().as_ref() {
            return Some(Rc::clone(local));
        }
        let local = Rc::new(tokio::task::LocalSet::new());
        let driver = Rc::clone(&local);
        let guard = BridgeGuard {
            id: Rc::as_ptr(&local) as *const () as usize,
        };
        let heartbeat = Rc::new(std::cell::Cell::new(None));
        let driver_heartbeat = Rc::clone(&heartbeat);
        // `tokio::task::spawn_local` 在 `LocalSet` 之外会 panic；用 `catch_unwind` 探测，
        // 避免把「宿主没建 LocalSet」变成构造期崩溃。
        let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::spawn_local(async move {
                let _guard = guard;
                // 心跳 future 永不完成，同时驱动 inner LocalSet。宿主停止驱动外层
                // executor 后不会再有心跳，`start_local` 据此拒绝创建后台记录。
                driver
                    .run_until(async move {
                        loop {
                            driver_heartbeat.set(Some(std::time::Instant::now()));
                            tokio::time::sleep(BRIDGE_HEARTBEAT_INTERVAL).await;
                        }
                    })
                    .await;
            });
        }));
        if spawned.is_err() {
            return None;
        }
        *slot.borrow_mut() = Some(Rc::clone(&local));
        BRIDGE_HEARTBEAT.with(|heartbeat_slot| {
            *heartbeat_slot.borrow_mut() = Some(heartbeat);
        });
        Some(local)
    })
}

/// 会话线程的常驻 `LocalSet` 句柄（后台 `!Send` Work 的执行器）。
///
/// 在 `LocalSet` 上下文内首次调用时会安装常驻派发表；不在其中时返回 `None`。
/// `BackgroundTaskManager::start_local` 用它把任务钉在会话自己的执行器上，而不是
/// 调用点恰好处于的临时 `LocalSet`。
pub(crate) fn session_local_set() -> Option<Rc<tokio::task::LocalSet>> {
    bridge_local()
}

/// 常驻 driver 是否仍在被宿主驱动。
///
/// 等一个短窗口让 driver 更新心跳：调用发生在会话 turn 内时，宿主正在驱动该
/// `LocalSet`，下一次 tick 会把心跳刷新；若宿主只持有但不再驱动这个 executor
/// （例如 per-turn 临时 `LocalSet`），心跳不会更新，返回 `false`。
pub(crate) async fn session_local_driver_is_live() -> bool {
    let Some(heartbeat) = BRIDGE_HEARTBEAT.with(|slot| slot.borrow().clone()) else {
        return false;
    };
    let deadline = std::time::Instant::now() + BRIDGE_HEARTBEAT_WAIT;
    loop {
        if heartbeat
            .get()
            .is_some_and(|at| at.elapsed() <= BRIDGE_HEARTBEAT_FRESH)
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

/// 在常驻派发表上运行 `!Send` future，`Send` 侧等待结果。
///
/// 返回的 future 只持有 `oneshot::Receiver`（`T: Send` 时为 `Send`），因此满足
/// [`Agent`] 钩子的 `Send` 约束。没有安装派发表时退化为同步轮询。
fn dispatch_local<F, T>(future: F) -> futures::future::BoxFuture<'static, Option<T>>
where
    F: std::future::Future<Output = T> + 'static,
    T: Send + 'static,
{
    match bridge_local() {
        Some(local) => {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            local.spawn_local(async move {
                let _ = sender.send(future.await);
            });
            Box::pin(async move { receiver.await.ok() })
        }
        None => {
            let value = run_local_blocking(future);
            Box::pin(async move { value })
        }
    }
}

/// 把一段需要扩展运行时（`!Send`）的异步工作投到会话线程的常驻派发表上，从 `Send`
/// 上下文安全等待结果。
///
/// 供宿主/测试 harness 转发 `transformContext` 一类由 agent-core 在 `Send` future 里
/// 调用的钩子。没有安装派发表时返回 `None`（同步轮询失败时同样）。
pub fn run_on_session_thread<F, T>(future: F) -> futures::future::BoxFuture<'static, Option<T>>
where
    F: std::future::Future<Output = T> + 'static,
    T: Send + 'static,
{
    dispatch_local(future)
}

/// 把一段需要扩展运行时的异步工作投到会话线程上 **fire-and-forget**（TS 的
/// `void runner.emit(...)`）。
pub fn spawn_on_session_thread<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    spawn_local_dispatch(future);
}

/// [`run_on_session_thread`] 的同步形态：在调用线程上同步驱动（用于 `onPayload` 一类
/// 签名是同步、返回值必须立刻可用的钩子）。没有常驻派发表时同步轮询。
pub fn run_on_session_thread_blocking<F, T>(future: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    run_local_blocking(future)
}

/// 在常驻派发表上派生一个 fire-and-forget future（TS 的 `void fn()`）。
fn spawn_local_dispatch<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    match bridge_local() {
        Some(local) => {
            local.spawn_local(future);
        }
        None => {
            let _ = run_local_blocking(future);
        }
    }
}

/// 在同一个线程的 `LocalSet` 上**同步**驱动一个非 `Send` future（退化路径）。
///
/// 只在没有常驻派发表时使用（调用方不在 `LocalSet` 上下文）。与 TS 一样不设超时：
/// 需要定时器/IO 的 future 在这里无法推进，属于宿主必须建立 `LocalSet` 的约束。
fn run_local_blocking<F, T>(future: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    /// 让 `wake` 真正唤醒被 park 的线程，而不是依赖固定间隔的忙轮询。
    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    // 在临时 LocalSet 上驱动：`RunUntil::poll` 会进入 LocalSet 上下文，因此扩展派发里
    // 的 `spawn_local`（handler 执行、宿主异步）落到这个 LocalSet，并在后续轮询里由
    // `RunUntil` 的 tick 驱动。waker 只负责唤醒当前线程，不改变 future 的 `Send` 约束。
    let local = tokio::task::LocalSet::new();
    let mut future = std::pin::pin!(local.run_until(future));
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return Some(value),
            // 正常由 waker 唤醒（例如权限确认从其它线程回复）；100ms 只是没有 waker
            // 时的兜底，不再每秒空转上万次。
            Poll::Pending => std::thread::park_timeout(std::time::Duration::from_millis(100)),
        }
    }
}

/// `ModelThinkingLevel` → `kiana_ai::types::ThinkingLevel`（`off` 映射为 `None`）。
fn ai_thinking_level(level: ModelThinkingLevel) -> Option<kiana_ai::types::ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(kiana_ai::types::ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(kiana_ai::types::ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(kiana_ai::types::ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(kiana_ai::types::ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(kiana_ai::types::ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(kiana_ai::types::ThinkingLevel::Max),
    }
}

/// `ModelImageResizeOptions` → `ImageResizeOptions`（缺省字段回落到 TS 的默认值）。
fn image_resize_options_from_model(
    options: &kiana_ai::types::ModelImageResizeOptions,
) -> kiana_core::image_process::ImageResizeOptions {
    let defaults = kiana_core::image_process::ImageResizeOptions::default();
    kiana_core::image_process::ImageResizeOptions {
        max_width: options
            .max_width
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_width),
        max_height: options
            .max_height
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_height),
        max_bytes: options.max_bytes.unwrap_or(defaults.max_bytes),
        jpeg_quality: options.jpeg_quality,
    }
}

/// 把取消令牌包装成 `AbortScope`（压缩/摘要路径使用）。
fn abort_scope_from_token(token: &CancellationToken) -> kiana_ai::utils::abort::AbortScope {
    let scope = kiana_ai::utils::abort::AbortScope::new();
    let token = token.clone();
    let watcher = scope.clone();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            token.cancelled().await;
            watcher.abort_with(kiana_ai::utils::abort::AbortReason::default_abort_error());
        });
    }
    scope
}

/// 把 `Message` 的消息内容归一化（`content == null` → 空数组）。
///
/// TS: agent-session.ts#_replaceMessageInPlace。未类型化扩展的 `message_end` 返回值可能把
/// `content` 改成 `null`（或整键缺失），而 `AgentMessage` 的反序列化会因此退回
/// [`AgentMessage::Custom`]，丢掉标准角色的类型信息（`as_assistant()`、重试/压缩都会跳过）。
/// 对 user/assistant/toolResult 先在 JSON 层把 `content` 补成 `[]` 再尝试还原标准消息；
/// `custom` 角色则保留自定义对象，只补 `content`。
fn normalize_message_content(message: AgentMessage) -> AgentMessage {
    let AgentMessage::Custom(mut object) = message else {
        return message;
    };
    let role = object
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    if !matches!(
        role.as_str(),
        "user" | "assistant" | "toolResult" | "custom"
    ) {
        return AgentMessage::Custom(object);
    }
    let content_is_null = object.get("content").is_none_or(JsonValue::is_null);
    if !content_is_null {
        return AgentMessage::Custom(object);
    }
    object.insert("content".to_string(), JsonValue::Array(Vec::new()));
    // TS 的 replacement 仍是原角色的对象；能解析成标准消息时保持标准类型。
    let standard_value = JsonValue::Object(
        object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    match serde_json::from_value::<Message>(standard_value) {
        Ok(standard) => AgentMessage::Standard(standard),
        Err(_) => AgentMessage::Custom(object),
    }
}

/// [`AgentEvent`] → [`AgentSessionEvent`]。
fn session_event_from_agent_event(event: &AgentEvent) -> Option<AgentSessionEvent> {
    session_event_from_agent_event_inner(event, None)
}

/// 事件转换时对 `MessageEnd` 消息的替换包装；单独成类型是为了让调用方显式写出
/// “这个消息已被扩展替换”的意图（见 `session_event_from_agent_event_with_message`）。
enum ReplacementMessage {
    Replace(AgentMessage),
}

/// [`session_event_from_agent_event`] 的带替换版本；`message_replacement` 只用于
/// `MessageEnd`（扩展 `message_end` 的替换消息）。
fn session_event_from_agent_event_with_message(
    event: &AgentEvent,
    replacement: ReplacementMessage,
) -> Option<AgentSessionEvent> {
    match replacement {
        ReplacementMessage::Replace(message) => {
            session_event_from_agent_event_inner(event, Some(message))
        }
    }
}

/// [`AgentEvent`] → [`AgentSessionEvent`] 的逐变体映射；`AgentEnd` 返回 `None`
/// （公开侧由 `handle_agent_event` 附加 `will_retry` 后单独构造）。
fn session_event_from_agent_event_inner(
    event: &AgentEvent,
    message_replacement: Option<AgentMessage>,
) -> Option<AgentSessionEvent> {
    match event {
        AgentEvent::AgentStart => Some(AgentSessionEvent::AgentStart),
        AgentEvent::AgentEnd { .. } => None,
        AgentEvent::TurnStart => Some(AgentSessionEvent::TurnStart),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => Some(AgentSessionEvent::TurnEnd {
            message: message.clone(),
            tool_results: tool_results.clone(),
        }),
        AgentEvent::MessageStart { message } => Some(AgentSessionEvent::MessageStart {
            message: message.clone(),
        }),
        AgentEvent::MessageUpdate {
            assistant_message_event,
            message,
        } => Some(AgentSessionEvent::MessageUpdate {
            assistant_message_event: assistant_message_event.clone(),
            message: message.clone(),
        }),
        AgentEvent::MessageEnd { message } => Some(AgentSessionEvent::MessageEnd {
            message: message_replacement.unwrap_or_else(|| message.clone()),
        }),
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Some(AgentSessionEvent::ToolExecutionStart {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            parent_tool_call_id: None,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => Some(AgentSessionEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            partial_result: partial_result.clone(),
            parent_tool_call_id: None,
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
            duration_ms,
        } => Some(AgentSessionEvent::ToolExecutionEnd {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            result: result.clone(),
            is_error: *is_error,
            duration_ms: *duration_ms,
            parent_tool_call_id: None,
        }),
    }
}

impl Core {
    /// TS 的 `this._resourceLoader.get*()` 读取：每次使用都从 live 加载器重新取快照。
    ///
    /// 加载器不在 `Core` 里（不是 `Send`），按 thread-local 注册 id 取回。
    fn refresh_command_sources_from_loader(&self) {
        let Some(loader) = self.resource_loader_id.and_then(lookup_loader) else {
            return;
        };
        let prompts = loader.get_prompts().prompts;
        let skills = loader.get_skills().skills;
        let context_files = loader.get_agents_files().agents_files;
        let system_prompt = loader.get_system_prompt();
        let append_system_prompt = loader.get_append_system_prompt();
        self.sync_skill_definitions(&skills, false);
        let mut sources = lock(&self.commands);
        sources.prompts = prompts;
        sources.skills = skills;
        sources.context_files = context_files;
        sources.system_prompt = system_prompt;
        sources.append_system_prompt = append_system_prompt;
    }

    /// 用 loader 的技能快照同步扩展定义。
    ///
    /// `force = false` 时只在技能集合（路径/名字）变化时重新读正文：系统提示每轮重建
    /// 都会调用本函数，不能每轮都做 N 次文件读取。显式 reload/build_runtime 用
    /// `force = true` 保证正文变更也会刷新。
    fn sync_skill_definitions(&self, skills: &[Skill], force: bool) {
        let current = lock(&self.skill_definitions);
        if !force && skill_definitions_match(&current, skills) {
            return;
        }
        let definitions = skill_definitions_from_skills(skills);
        drop(current);
        *lock(&self.skill_definitions) = definitions;
    }

    /// 懒构造会话级 `skill` 工具；已存在时用最新定义更新。
    fn ensure_skill_tool(&self) -> Arc<SkillTool> {
        let definitions = lock(&self.skill_definitions).clone();
        let ttl = self.skill_activation_ttl();
        let mut guard = lock(&self.skill_tool);
        match guard.as_ref() {
            Some(tool) => {
                tool.update_definitions(definitions);
                tool.set_ttl(ttl);
                Arc::clone(tool)
            }
            None => {
                let tool = Arc::new(SkillTool::new(definitions));
                tool.set_ttl(ttl);
                *guard = Some(Arc::clone(&tool));
                tool
            }
        }
    }

    /// 技能激活 TTL（settings `skills.activationTtlSeconds` / `skills.activationTtl`）。
    ///
    /// 配置为 0/负数/非数字时视为不自动过期（`None`），保持旧行为。
    fn skill_activation_ttl(&self) -> Option<std::time::Duration> {
        let settings = self.settings_manager.get_settings();
        // 顶层键是迁移后的稳定入口（对象形式的 `skills.activationTtlSeconds` 会被
        // `migrate_skills` 搬到这里）；未走迁移的直接 serde 调用仍兼容嵌套对象。
        let top_level = settings
            .raw("skillActivationTtlSeconds")
            .or_else(|| settings.raw("skillActivationTtl"))
            .and_then(|value| value.as_u64());
        let nested = settings.raw("skills").and_then(|raw| {
            raw.get("activationTtlSeconds")
                .or_else(|| raw.get("activationTtl"))
                .and_then(serde_json::Value::as_u64)
        });
        let seconds = top_level.or(nested).filter(|seconds| *seconds > 0)?;
        Some(std::time::Duration::from_secs(seconds))
    }

    /// 读取工具定义的 annotations（MCP 服务器可能在 `tools/list_changed` 后更新）。
    fn tool_annotations(&self, tool_name: &str) -> Option<ToolAnnotations> {
        self.flush_pending_tool_refresh();
        lock(&self.tool_definitions)
            .get(tool_name)
            .and_then(|entry| entry.definition.annotations.clone())
    }

    /// 记录一次工具调用会触达的路径（`paths` 技能可见性）。
    fn record_touched_paths(&self, request: &PermissionRequest) {
        let mut touched = lock(&self.touched_paths);
        touched.extend(request.paths.iter().cloned());
        touched.extend(request.read_paths.iter().cloned());
    }

    /// 当前 plan 状态；`content` 缺失时按 `path` 读取（读取失败只保留路径）。
    fn plan_state_snapshot(&self) -> SessionPlanState {
        let mut state = lock(&self.plan_state).clone();
        if state.content.is_none()
            && let Some(path) = state.path.clone()
        {
            state.content = read_plan_content(&self.cwd, &path);
        }
        state
    }

    /// 供 pruning 使用的配置：默认保护器 + 当前 active plan 路径。
    fn plan_prune_config(&self) -> PruneConfig {
        let state = lock(&self.plan_state).clone();
        PruneConfig::default().with_plan_paths(state.prune_paths())
    }

    /// 压缩恢复输入里的 plan 段。
    fn plan_recovery(&self) -> PlanRecovery {
        self.plan_state_snapshot().recovery()
    }

    /// 观察扩展追加的 `plan-mode` 自定义条目（生产入口：`appendEntry` 处理器）。
    fn observe_plan_custom_entry(&self, custom_type: &str, data: Option<&serde_json::Value>) {
        if custom_type != "plan-mode" {
            return;
        }
        apply_plan_entry_data(&mut lock(&self.plan_state), data);
    }

    /// 本会话当前模型窗口（token）；未知时 `None`。
    fn current_model_context_window(&self) -> Option<u64> {
        self.current_model()
            .and_then(|model| model.context_window)
            .filter(|window| *window > 0)
    }

    /// 按当前模型窗口与已触达路径生成技能索引段。
    ///
    /// 返回 `None` 表示没有可用定义（调用方保持 `sources.skills` 的默认路径）。
    fn skill_prompt_section(&self, reader: SkillReader) -> Option<String> {
        let definitions = lock(&self.skill_definitions).clone();
        if definitions.is_empty() {
            return None;
        }
        let budget = skill_index_budget_chars(self.current_model_context_window());
        let touched: Vec<PathBuf> = lock(&self.touched_paths).iter().cloned().collect();
        Some(format_skill_definitions_for_prompt_with_cwd(
            &definitions,
            reader,
            budget,
            &touched,
            Some(Path::new(&self.cwd)),
        ))
    }

    /// 会话结束/重置：撤销所有技能激活。
    fn end_skill_session(&self) {
        if let Some(tool) = lock(&self.skill_tool).as_ref() {
            tool.end_session();
        }
    }

    /// 会话级 artifact 存储；目录不可用（内存会话）时返回 `None`。
    fn artifact_store(&self) -> Option<Arc<ArtifactStore>> {
        if let Some(store) = self.artifact_store.get() {
            return Some(Arc::clone(store));
        }
        let (dir, session_id) = self.with_session(|manager| {
            (
                manager.get_session_dir().to_string(),
                manager.get_session_id().to_string(),
            )
        });
        if dir.is_empty() || session_id.is_empty() {
            return None;
        }
        let store = Arc::new(ArtifactStore::new(dir, &session_id).ok()?);
        let _ = self.artifact_store.set(Arc::clone(&store));
        Some(store)
    }

    /// 本分支的压缩边界（`first_kept_entry_id`），供聚合预算跳过已不在上下文里的条目。
    fn compaction_boundary_id(&self) -> Option<String> {
        let branch = self.branch();
        kiana_session_core::session_manager::get_latest_compaction_entry(&branch)
            .map(|entry| entry.first_kept_entry_id.clone())
    }

    /// 聚合 tool-result 预算：超过 50k token 时从最旧结果开始清除，完整内容落 artifact。
    ///
    /// 清除只改 content、不删消息、不拆 tool pair；占位符带 `artifact: <path>`，压缩后
    /// 仍能解析到同一文件。已清除的条目通过分支上的 `context_edit` 识别（原始内容
    /// 不变，只改投影），不会在每次请求前被重复落盘/重复追加编辑。
    fn enforce_aggregate_tool_result_budget(&self) {
        let branch = self.branch();
        let store = self.artifact_store();
        let config = self
            .plan_prune_config()
            .with_boundary(self.compaction_boundary_id());
        // 分支上的 context_edit（同一 target 后者覆盖前者），供预算计划按投影内容计数
        // 并跳过已清除条目。
        let mut replacements = std::collections::HashMap::new();
        for entry in &branch {
            if let kiana_session_core::session_manager::SessionEntry::ContextEdit(edit) = entry {
                let content = edit
                    .replacement
                    .as_ref()
                    .map(|replacement| replacement.content.clone())
                    .unwrap_or(JsonValue::Null);
                replacements.insert(edit.target_id.clone(), content);
            }
        }
        // 热路径只需要“清除哪些条目”，不要复制整段历史（每次 provider 请求前都跑）。
        let plan = kiana_compaction::pruning::plan_aggregate_tool_result_budget_with_replacements(
            &branch,
            &replacements,
            50_000,
            store.as_deref(),
            &config,
        );
        if plan.cleared.is_empty() {
            return;
        }
        let mut changed = false;
        for record in &plan.cleared {
            let edit = self.with_session_mut(|manager| {
                manager.append_context_edit(
                    &record.entry_id,
                    Some(ContextEditReplacement {
                        content: JsonValue::String(record.notice.clone()),
                    }),
                )
            });
            // 目标不在活跃分支/会话正在切换：预算只是优化，不能阻断请求。
            if let Ok(edit_id) = edit {
                changed = true;
                if let Some(entry) =
                    self.with_session(|manager| manager.get_entry(&edit_id).cloned())
                {
                    self.emit(AgentSessionEvent::EntryAppended { entry });
                }
            }
        }
        if changed {
            self.refresh_finalized_context();
        }
    }
}

/// 技能集合是否与定义一致（只比较名字与路径，不读磁盘）。
fn skill_definitions_match(definitions: &[SkillDefinition], skills: &[Skill]) -> bool {
    definitions.len() == skills.len()
        && definitions.iter().zip(skills).all(|(definition, skill)| {
            definition.skill.name == skill.name && definition.skill.file_path == skill.file_path
        })
}

/// 把会话的 `AgentContext` 归一化成请求策略使用的 `TranscriptContext`。
/// 从会话条目恢复 plan / plan-mode 状态：最后一个 `plan-mode` 自定义条目生效。
fn plan_state_from_entries(entries: &[SessionEntry]) -> SessionPlanState {
    let mut state = SessionPlanState::default();
    for entry in entries {
        if let SessionEntry::Custom(custom) = entry
            && custom.custom_type == "plan-mode"
        {
            apply_plan_entry_data(&mut state, custom.data.as_ref());
        }
    }
    state
}

/// 把 plan-mode 扩展的 `appendEntry` 数据合并进会话状态。
///
/// 已知键：`enabled`、`planFilePath`/`planPath`/`path`、`content`、`approved`。未知键忽略，
/// 因此扩展可以继续在同一个条目里保存 todos 等私有状态。
fn apply_plan_entry_data(state: &mut SessionPlanState, data: Option<&serde_json::Value>) {
    let Some(serde_json::Value::Object(data)) = data else {
        return;
    };
    if let Some(enabled) = data.get("enabled").and_then(serde_json::Value::as_bool) {
        state.plan_mode = enabled;
    }
    if let Some(approved) = data.get("approved").and_then(serde_json::Value::as_bool) {
        state.approved = approved;
    }
    if let Some(content) = data.get("content").and_then(serde_json::Value::as_str) {
        state.content = Some(content.to_string());
    }
    for key in ["planFilePath", "planPath", "plan_path", "path"] {
        if let Some(path) = data.get(key).and_then(serde_json::Value::as_str)
            && !path.trim().is_empty()
        {
            state.path = Some(path.to_string());
            break;
        }
    }
}

/// 读取 plan 文件（`local://` 前缀按会话 cwd 解析）；缺失/不可读时返回 `None`。
fn read_plan_content(cwd: &str, path: &str) -> Option<String> {
    let raw = path.trim();
    let raw = raw
        .strip_prefix("local://")
        .or_else(|| raw.strip_prefix("local:/"))
        .unwrap_or(raw);
    let candidate = Path::new(raw);
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        Path::new(cwd).join(candidate)
    };
    std::fs::read_to_string(resolved).ok()
}

fn transcript_for_context(context: &AgentContext) -> TranscriptContext {
    // 工具声明在 provider 请求里同样占用输入 token；只估算 messages 会低估实际请求，
    // 让超窗请求仍然发出。这里把 `context.tools` 也折叠进初始 system 消息。
    let tools = context.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|tool| kiana_ai::types::Tool {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
                constrained_sampling: tool.constrained_sampling.clone(),
            })
            .collect()
    });
    normalize_context(&Context {
        system_prompt: None,
        messages: convert_to_llm(&context.messages),
        tools,
    })
}

/// `StopReason` 的稳定低基数字面量（遥测标签）。
/// 从 provider 错误文本里提取 HTTP 状态码（适配器写成 `API error (429): …` / JSON
/// `"status": 429` 两类）。消息里没有可识别状态时返回 `None`。
///
/// 只接受完整的三位 4xx/5xx 数字边界，避免把 token 计数（`4000 tokens`）误判成状态码。
fn provider_status_from_message(raw: &str) -> Option<u16> {
    fn is_boundary(byte: Option<u8>) -> bool {
        !matches!(byte, Some(byte) if byte.is_ascii_digit())
    }
    let bytes = raw.as_bytes();
    for (index, window) in bytes.windows(3).enumerate() {
        let candidate = &window;
        if !candidate.iter().all(u8::is_ascii_digit) {
            continue;
        }
        if !is_boundary(index.checked_sub(1).map(|previous| bytes[previous]))
            || !is_boundary(bytes.get(index + 3).copied())
        {
            continue;
        }
        let status = (candidate[0] - b'0') as u16 * 100
            + (candidate[1] - b'0') as u16 * 10
            + (candidate[2] - b'0') as u16;
        if (400..=599).contains(&status) {
            return Some(status);
        }
    }
    None
}

/// 失败 assistant 消息是否可重试（第 9 周：状态码 + `request_policy` 单一事实来源）。
///
/// 顺序：
/// 1. 上下文溢出不可重试（压缩路径处理）；
/// 2. 从错误文本提取 HTTP 状态，构造 typed error；`Auth/Quota/ContextWindow/
///    InvalidRequest` 一律不重试；
/// 3. `request_policy::is_retryable_provider_error` 正向判定（含 425/529 与 typed
///    `isRetryable` 的口径）；命中即重试；
/// 4. 文本兜底（旧正则），只在没有结构化结论时使用。
fn assistant_error_is_retryable(message: &AssistantMessage, context_window: u64) -> bool {
    if is_context_overflow(message, Some(context_window)) {
        return false;
    }
    let raw = message.error_message.as_deref();
    let status = raw.and_then(provider_status_from_message);
    let typed = TypedProviderError {
        status,
        message: raw,
        ..TypedProviderError::default()
    };
    let value = serde_json::json!({ "message": raw.unwrap_or_default() });
    let class = classify_provider_error_with_typed(Some(&typed), &value);
    if matches!(
        class,
        ProviderErrorClass::Auth
            | ProviderErrorClass::Quota
            | ProviderErrorClass::ContextWindow
            | ProviderErrorClass::InvalidRequest
    ) {
        return false;
    }
    if kiana_ai::request_policy::is_retryable_provider_error(Some(&typed), &value) {
        return true;
    }
    is_retryable_assistant_error(message)
}

fn stop_reason_label(reason: StopReason) -> String {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "tool_use",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
        StopReason::TurnCapped => "turn_capped",
        StopReason::TokenCapped => "token_capped",
        StopReason::CostCapped => "cost_capped",
        StopReason::TimeCapped => "time_capped",
    }
    .to_string()
}

/// 按当前激活工具选择技能索引里引用的读取工具。
fn skill_reader_for_tools(tools: &[String]) -> SkillReader {
    if tools.iter().any(|tool| tool == "read") {
        SkillReader::Read
    } else if tools.iter().any(|tool| tool == "bash") {
        SkillReader::Bash
    } else {
        SkillReader::Indirect
    }
}

/// 从 loader 的 `Skill` 记录加载扩展定义（正文/frontmatter）。
///
/// 与 `kiana_resources::skills::load_skill_definitions` 的逐技能逻辑一致；这里复用
/// loader 已完成的发现/冲突处理，避免再走一遍目录扫描。
fn skill_definitions_from_skills(skills: &[Skill]) -> Vec<SkillDefinition> {
    skills
        .iter()
        .map(|skill| {
            let source = if skill.source_info.source == "local" {
                match skill.source_info.scope {
                    kiana_core::source_info::SourceScope::User => "user",
                    kiana_core::source_info::SourceScope::Project => "project",
                    kiana_core::source_info::SourceScope::Temporary => "path",
                }
            } else {
                skill.source_info.source.as_str()
            };
            let (definition, _diagnostics) =
                load_skill_definition_from_file(&skill.file_path, source);
            definition.unwrap_or_else(|| SkillDefinition {
                skill: skill.clone(),
                metadata: Default::default(),
                content: String::new(),
            })
        })
        .collect()
}

// ============================================================================
// AgentSession
// ============================================================================

/// `AgentSession`：交互/print/RPC 模式共享的会话门面。
///
/// TS: `agent-session.ts#AgentSession`。持有 `Core`（`Send + Sync` 状态）与资源加载器
/// （trait 没有 `Send + Sync` 约束，不进入 Agent 钩子）。
#[derive(Clone)]
pub struct AgentSession {
    core: Arc<Core>,
    resource_loader_id: usize,
}

/// 第 11 周：子代理运行时入口。
///
/// 这些方法只做**快照**，不启动模型循环：调用方拿到 [`SubagentRuntime`] 后自己驱动
/// 模型与工具。这样交互/TUI、print、RPC 三种前端可以复用同一套子代理状态机，而不会
/// 把模型调用方式绑死在会话门面里。
///
/// 父上下文的关键点：
/// - 工具表用 `get_callable_tools()`（已应用激活名单、exposure 与 exclude 过滤）；
/// - 权限策略取当前引擎的 policy，父会话的 `session_allow` 记录**不**复制；
/// - 模型取 `agent.model()`（无模型时为默认模型），子代理的 `model: inherit` 使用它。
impl AgentSession {
    /// 构造子代理父上下文快照。
    pub fn subagent_parent_context(&self) -> crate::subagent::SubagentParentContext {
        let session_id = lock(&self.core.session_manager)
            .get_session_id()
            .to_string();
        let tools = crate::subagent::ToolPool::new(self.core.get_callable_tools());
        let policy = lock(&self.core.permissions)
            .as_ref()
            .map(|engine| engine.policy());
        let permission_mode = policy
            .as_ref()
            .map(|policy| policy.mode)
            .unwrap_or(PermissionMode::Default);
        let mut context =
            crate::subagent::SubagentParentContext::new(session_id, self.core.cwd.clone(), tools);
        context.allowed_tools = crate::subagent::ToolPermissionSet::all();
        context.permission_policy = policy;
        context.permission_mode = permission_mode;
        context.model = Some(self.core.agent.model());
        context.custom_tools = lock(&self.core.custom_tool_names).iter().cloned().collect();
        context.shell_command_prefix = self.settings_manager().get_shell_command_prefix();
        // MCP annotations 是权限决策的一部分：子代理走自己的权限上下文，必须拿到
        // 同一份 hints，否则 destructive/openWorld 工具会绕过强制确认。
        self.core.flush_pending_tool_refresh();
        context.mcp_annotations = lock(&self.core.tool_definitions)
            .iter()
            .filter_map(|(name, entry)| {
                entry
                    .definition
                    .annotations
                    .clone()
                    .map(|annotations| (name.clone(), annotations))
            })
            .collect();
        context
    }

    /// 准备一个子代理（独立 tool pool / system prompt / permission context /
    /// token budget / transcript）。
    pub fn prepare_subagent(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        options: crate::subagent::SubagentRuntimeOptions,
    ) -> Result<crate::subagent::SubagentRuntime, crate::subagent::SubagentError> {
        // 低层 prepare+run 协议没有 Work 身份，不能执行声明式 stages。
        if definition.stages.is_some() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!(
                    "agent `{}` declares stages; run it with `spawn_subagent_with_work`",
                    definition.name
                ),
            });
        }
        self.core
            .register_isolation_state_dir(options.isolation_options.state_dir.as_deref());
        crate::subagent::SubagentRuntime::prepare(
            definition,
            self.subagent_parent_context(),
            options,
        )
    }

    /// 恢复已有子代理会话；transcript 复用，工具与权限按当前父上下文重建。
    pub fn resume_subagent(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        options: crate::subagent::SubagentRuntimeOptions,
        session_path: impl AsRef<std::path::Path>,
    ) -> Result<crate::subagent::SubagentRuntime, crate::subagent::SubagentError> {
        if definition.stages.is_some() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!(
                    "agent `{}` declares stages; staged work is not resumable",
                    definition.name
                ),
            });
        }
        self.core
            .register_isolation_state_dir(options.isolation_options.state_dir.as_deref());
        crate::subagent::SubagentRuntime::resume(
            definition,
            self.subagent_parent_context(),
            options,
            session_path,
        )
    }

    /// 解析子代理定义里的 `model:`。
    ///
    /// `inherit` 使用父会话当前模型；具名模型在运行时注册表里按 `id`、
    /// `provider/id` 或显示名查找，找不到时显式报错（不静默回退到父模型，避免用户以为
    /// 子代理用了指定模型）。
    pub fn resolve_subagent_model(
        &self,
        definition: &crate::agent_definition::AgentDefinition,
    ) -> Result<Model, crate::subagent::SubagentError> {
        use crate::agent_definition::AgentModel;
        match &definition.model {
            AgentModel::Inherit => Ok(self.model().unwrap_or_else(|| self.core.agent.model())),
            AgentModel::Named(name) => {
                let models = self.model_runtime().get_models(None);
                models
                    .into_iter()
                    .find(|model| {
                        model.id == *name
                            || format!("{}/{}", model.provider, model.id) == *name
                            || model.name == *name
                    })
                    .ok_or_else(|| crate::subagent::SubagentError::Agent {
                        message: format!(
                            "unknown model `{name}` for subagent `{}`",
                            definition.name
                        ),
                    })
            }
        }
    }

    /// 启动并驱动一个子代理直到终态（生产入口）。
    ///
    /// 这是第 11 周运行时缺失的模型循环门面：加载/传入的定义 → `prepare_subagent`
    /// 建运行时 → `run_subagent` 跑 agent loop。终态、usage、receipts、yield 输出和
    /// 会话文件都会返回；调用方不需要自己拼 `Agent` 或处理事件。
    ///
    /// 隔离工作区（`worktree` / `cow-clone`）的收尾与参照实现一致：**有改动就保留**
    /// （ownership 记录标记 `retained`，路径写进 `outcome.isolation`，可继续
    /// `resume_subagent` 或由调用方合并），**无改动才清理**。不把 delta 自动 apply
    /// 回父仓库：父工作区的未提交改动、cow-clone 的脏拷贝与嵌套仓库都无法可靠地
    /// 表示为一条可重放补丁，自动 apply 会在这些场景下失败或重复应用。
    pub async fn spawn_subagent(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError> {
        // 声明式 stage 需要 Work 身份（写前记录、review.json 命名空间与事件）；
        // 普通路径不能静默只跑第一阶段。
        if definition.stages.is_some() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!(
                    "agent `{}` declares stages; run it with `spawn_subagent_with_work`",
                    definition.name
                ),
            });
        }
        let model = self.resolve_subagent_model(&definition)?;
        let runtime = std::sync::Arc::new(self.prepare_subagent(definition, runtime_options)?);
        self.run_prepared_subagent(runtime, model, prompt.into(), cancel)
            .await
    }

    /// 运行一个已准备的子代理。
    ///
    /// 隔离收尾与 acceptance 判定在 `run_subagent` 内部完成（工作区必须先于
    /// `Drop` 清理被读取），这里只负责注入生产环境的 stream 函数。
    async fn run_prepared_subagent(
        &self,
        runtime: std::sync::Arc<crate::subagent::SubagentRuntime>,
        model: Model,
        prompt: String,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError> {
        crate::subagent::run_subagent(
            runtime,
            crate::subagent::SubagentRunOptions {
                model,
                stream_fn: Some(stream_simple_stream_fn()),
                prompt,
                cancel,
                session_id: None,
            },
        )
        .await
    }

    /// 按名字从默认 agent 目录加载定义；名字不存在时列出可用名字。
    fn load_subagent_definition(
        &self,
        name: &str,
    ) -> Result<crate::agent_definition::AgentDefinition, crate::subagent::SubagentError> {
        let loader =
            crate::agent_definition::AgentDefinitionLoader::with_default_roots(&self.core.cwd);
        let report = loader.load();
        report.get(name).cloned().ok_or_else(|| {
            let available = report
                .definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            crate::subagent::SubagentError::Agent {
                message: format!("unknown subagent `{name}`; available: {available}"),
            }
        })
    }

    /// 按名字从默认 agent 目录（用户/项目/策略）加载定义并启动子代理。
    ///
    /// 名字不存在时错误里列出可用名字（内置 `general-purpose` / `explore` 始终存在）。
    pub async fn spawn_named_subagent(
        &self,
        name: &str,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError> {
        let definition = self.load_subagent_definition(name)?;
        self.spawn_subagent(definition, runtime_options, prompt, cancel)
            .await
    }

    /// 启动一个带 Work envelope 的子代理（宿主 opt-in 入口）。
    ///
    /// 默认 `spawn_subagent` 路径完全不变；只有本方法会生成 `WorkId` / `RunId`、发出
    /// 库内 `WorkEvent`、可选地持久化 `WorkResult`。注入 `WorkStore` 时采用写前记录：
    /// `begin` → `attach_session` → 运行 → `finish`，让崩溃后的 sweep 能识别遗留 Work。
    pub async fn spawn_subagent_with_work(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        let prepared = self
            .prepare_work(definition, runtime_options, prompt, work_options)
            .await?;
        self.run_work_lifecycle(
            prepared.request,
            prepared.record,
            prepared.prompt,
            cancel,
            prepared.work_options,
            prepared.launch,
        )
        .await
    }

    /// 后台执行一个带 Work envelope 的子代理；立即返回 task 与 Work 身份。
    ///
    /// 这是 Phase 1c 的后台入口：工作 future 通过
    /// [`BackgroundTaskManager::start_local`](crate::background::BackgroundTaskManager::start_local)
    /// 运行在调用方的单线程 `LocalSet` 上，因此可以安全地驱动 `!Send` 的
    /// `AgentSession` / 运行时。预检（执行所有权、store 选择、id 分配、请求校验、
    /// staged 边界）同步完成：非法请求在创建后台任务之前就返回错误。
    ///
    /// 取消是协作式的：`AgentSession::cancel_background_task(task_id)` 只取消 token，
    /// 不 abort Work 生命周期、也不抢写后台记录终态；Work 账本写出自己的终态后，
    /// 后台记录由该终态映射而来。宿主传入的 `cancel` token 也会转发到后台任务。
    ///
    /// 跨进程恢复由两条记录协作完成：`WorkRecord` 是权威状态，后台 task 记录是本次
    /// attempt 的输出/通知载体。启动时 [`Self::recover_work`] 先落定 Work，再把权威
    /// 终态对账回后台记录。后台 attempt 在 journal 里带有 `workBacked` 标记，通用
    /// background sweep 不替它判定终态（否则会在所属会话对账前误写 `process_lost`）。
    /// `recovery=resume` 的后台 Work 崩溃后不会被自动重放，宿主仍通过显式
    /// [`Self::resume_work`] 恢复（恢复是新的前台 Work）。
    ///
    /// 后台 Work 必须落在 durable Work 账本上：对账的输入就是账本记录，没有账本就
    /// 没有可恢复的权威终态。未显式提供 `store` 时会注入会话的 durable store；
    /// 会话没有 durable work 存储时直接拒绝。
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_subagent_with_work_in_background(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<crate::subagent::BackgroundWorkHandle, crate::subagent::SubagentError> {
        let Some(manager) = self.background_task_manager().cloned() else {
            return Err(crate::subagent::SubagentError::Agent {
                message: "background work requires continuity.background".to_string(),
            });
        };
        let Some(durable_store) = self.durable_work_store() else {
            return Err(crate::subagent::SubagentError::Agent {
                message: "background work requires a durable work store".to_string(),
            });
        };
        let mut work_options = work_options;
        // 恢复只读会话的 durable 账本：begin 写进别的 store 会让 Work 在崩溃后
        // 既不能被 recover_work 终态化，也不能被 reconcile 对账。
        if let Some(injected) = &work_options.store {
            let injected = std::sync::Arc::as_ptr(injected) as *const ();
            let session_store = std::sync::Arc::as_ptr(&durable_store) as *const ();
            if injected != session_store {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "background work requires the session's durable work store; remove the injected WorkOptions.store".to_string(),
                });
            }
        }
        let store: std::sync::Arc<dyn crate::subagent::WorkStore> = durable_store;
        work_options.store = Some(store);
        let prepared = self
            .prepare_work(definition, runtime_options, prompt, work_options)
            .await?;
        let work_id = prepared.request.work_id;
        let run_id = prepared.request.run_id.clone();
        let parent_session_id = prepared.record.parent_session_id.clone();
        // begin 在后台任务里异步执行；常见冲突（同一 WorkId 已被占用）在创建任务
        // 之前同步返回，避免 handle 先宣告一个不会开始的 Work。并发窗口里的冲突仍
        // 由生命周期里的 `begin` 兜底。
        if let Some(store) = &prepared.work_options.store
            && let Some(existing) = store
                .load(&parent_session_id, work_id)
                .await
                .map_err(work_store_subagent_error)?
        {
            return Err(crate::subagent::SubagentError::WorkAlreadyExists {
                work_id: existing.work_id.0,
                status: existing.status().to_string(),
                run_id: existing.run_id.0.clone(),
            });
        }
        let description = prepared
            .request
            .prompt
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let description = if description.is_empty() {
            prepared.request.profile.clone()
        } else {
            description
        };
        let spec = crate::background::BackgroundTaskSpec::new(
            parent_session_id.clone(),
            format!("work:{}", work_id.0),
            "agent",
        )
        .with_description(description)
        // Work 驱动的 attempt：通用 sweep 不得替 Work 账本宣告终态。
        .with_work_backed(true)
        // 崩溃可能发生在 journal Running 之后、begin 之前：对账需要 Work 身份。
        .with_work_identity(work_id.0, run_id.0.clone());

        let session = self.clone();
        let handle = manager
            .start_local(spec, move |context| async move {
                // `begin` 前把 task id 写进 Work 记录：崩溃后启动恢复才能把两条账对齐。
                let mut prepared = prepared;
                prepared.record = prepared
                    .record
                    .with_background_task_id(context.task_id.clone());
                // `store_error` 意味着 Work 终态可能没有落账。映射前重新读一次账本，
                // 只有账本里确实是本 run 的终态时才发布它；否则 task 记录不能宣称
                // Completed（那会让两条账永久分叉，终态 CAS 也无法再被修正）。
                let store = prepared.work_options.store.clone();
                let work_id = prepared.request.work_id;
                let run_id = prepared.request.run_id.clone();
                let parent_session_id = prepared.record.parent_session_id.clone();
                // 宿主 token 可能在启动前就已取消：先同步置位，避免 Work 已经开始跑
                // 第一条消息后才观察到取消。
                if cancel.as_ref().is_some_and(|token| token.is_cancelled()) {
                    context.cancel.cancel();
                }
                let forwarder = cancel.map(|host| {
                    let target = context.cancel.clone();
                    tokio::task::spawn_local(async move {
                        host.cancelled().await;
                        target.cancel();
                    })
                });
                let run = session
                    .run_work_lifecycle(
                        prepared.request,
                        prepared.record,
                        prepared.prompt,
                        Some(context.cancel.clone()),
                        prepared.work_options,
                        prepared.launch,
                    )
                    .await;
                if let Some(forwarder) = forwarder {
                    forwarder.abort();
                }
                match run {
                    Ok(outcome) => match &outcome.store_error {
                        None => Self::background_task_outcome(&outcome.result),
                        Some(store_error) => {
                            let persisted = match &store {
                                Some(store) => store
                                    .load(&parent_session_id, work_id)
                                    .await
                                    .ok()
                                    .flatten()
                                    .filter(|record| record.run_id == run_id)
                                    .and_then(|record| record.terminal_result().cloned()),
                                None => None,
                            };
                            match persisted {
                                Some(result) => Self::background_task_outcome(&result),
                                None => {
                                    session.record_continuity_diagnostic(format!(
                                        "background work terminal was not persisted: {store_error}"
                                    ));
                                    crate::background::TaskOutcome::failed(format!(
                                        "work terminal was not persisted: {store_error}"
                                    ))
                                }
                            }
                        }
                    },
                    Err(error) => crate::background::TaskOutcome::failed(error.to_string()),
                }
            })
            .await
            .map_err(|error| crate::subagent::SubagentError::Agent {
                message: format!("cannot start background work: {error}"),
            })?;
        Ok(crate::subagent::BackgroundWorkHandle {
            task_id: handle.task_id,
            work_id,
            run_id,
            output_path: handle.output_path,
        })
    }

    /// `spawn_subagent_with_work` 与后台入口共用的预检。
    ///
    /// 这里只做“能否开始”的判定与记录构造，不运行任何子代理：执行所有权、store
    /// 回退、id 分配、请求校验、staged 边界都必须在创建后台任务之前同步失败。
    async fn prepare_work(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<PreparedWork, crate::subagent::SubagentError> {
        use crate::subagent::work::build_work_request;

        // 没有执行所有权时不能开新的 Work：`start_run` 会拒绝，durable 目录也可能
        // 属于另一个存活进程，claim 它会把对方正在使用的存储抢过来。
        if let Some(refusal) = self.execution_refusal() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("Cannot start a run: {refusal}"),
            });
        }
        let prompt = prompt.into();
        let parent_session_id = self.session_id();
        let mut work_options = work_options;
        // `recovery=resume` 的 Work 必须记在与 `resume_work` 默认读取的同一本账上：
        // 否则这里用进程内计数器发号、账本里却根本没有这条记录，后续 `resume_work`
        // 会拿着同一个数字去 durable 账本里恢复另一个 Work。显式注入的 store 仍然
        // 优先；默认 `recovery=fail` 路径保持 Phase 0 行为（没有显式 store 就不落账）。
        if work_options.recovery == crate::subagent::WorkRecovery::Resume
            && work_options.store.is_none()
        {
            work_options.store = self
                .durable_work_store()
                .map(|store| store as std::sync::Arc<dyn crate::subagent::WorkStore>);
            if work_options.store.is_none() {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "work resume requires a durable work store".to_string(),
                });
            }
        }
        let mut request_options = work_options.clone();
        if let Some(store) = &work_options.store
            && request_options.work_id.is_none()
        {
            request_options.work_id = Some(
                store
                    .allocate_work_id(&parent_session_id)
                    .await
                    .map_err(work_store_subagent_error)?,
            );
        }
        let request = build_work_request(
            &definition,
            &runtime_options,
            &prompt,
            std::path::Path::new(&self.core.cwd),
            &request_options,
        );
        request
            .validate()
            .map_err(|error| crate::subagent::SubagentError::Agent {
                message: error.to_string(),
            })?;
        if request.resumable && runtime_options.session_dir.is_none() {
            return Err(crate::subagent::SubagentError::Agent {
                message: "resumable work requires a persistent subagent session directory"
                    .to_string(),
            });
        }
        // Phase 2 的边界：staged Work 没有单一 transcript，无法把“恢复到第几个
        // stage”表达成一条记录；worktree 会为每个 stage 各建一份工作区，改动无法
        // 跨 stage 累积。这些约束在启动前拒绝，而不是运行到一半才发现。
        if let Some(stages) = &definition.stages {
            // 程序化构造的 `AgentDefinition` 不经过 `DefinitionBuilder::finish`：非法
            // stage 图（consume 未产出、produces 为空、重名）会带进运行。启动前按
            // 同一套规则校验，错误文本与定义加载路径一致。
            if let Err(message) = crate::agent_definition::validate_stages(stages) {
                return Err(crate::subagent::SubagentError::Agent { message });
            }
            if request.resumable {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "staged work is not resumable; use recovery=fail".to_string(),
                });
            }
            let isolation = runtime_options.isolation.unwrap_or(definition.isolation);
            if isolation != crate::subagent::IsolationMode::Shared {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "staged work requires isolation=shared".to_string(),
                });
            }
            if runtime_options.fork {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "staged work does not support fork".to_string(),
                });
            }
            if runtime_options.session_id.is_some() || runtime_options.sidechain_id.is_some() {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "staged work creates one session per stage; fixed session ids are not supported"
                        .to_string(),
                });
            }
            if runtime_options.yield_contract.is_some() {
                return Err(crate::subagent::SubagentError::Agent {
                    message: "staged work derives yield contracts from stages".to_string(),
                });
            }
        }
        let clock: std::sync::Arc<dyn crate::subagent::WorkClock> = work_options
            .clock
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(crate::subagent::SystemWorkClock));
        // 记录先于生命周期构造：`createdAtMs` 必须与 `begin`、事件共用同一个时钟。
        let mut record = crate::subagent::WorkRecord::new(
            request.clone(),
            parent_session_id,
            None,
            Some(std::process::id()),
            clock.now_ms(),
        );
        // 自定义隔离状态目录要在 `begin` 之前记进账本：子会话 header 要等首条消息
        // 才落盘，崩溃在这个窗口时 sweep 只能靠记录本身找到该目录。
        if let Some(state_dir) = &runtime_options.isolation_options.state_dir {
            record = record.with_isolation_state_dir(
                std::path::absolute(state_dir).unwrap_or_else(|_| state_dir.clone()),
            );
        }
        let launch = if definition.stages.is_some() {
            WorkLaunch::Stages {
                definition,
                runtime_options,
            }
        } else {
            WorkLaunch::Fresh {
                definition,
                runtime_options,
            }
        };
        Ok(PreparedWork {
            request,
            record,
            prompt,
            work_options,
            launch,
        })
    }

    /// 把 Work 账本的权威终态映射成后台 attempt 的 `TaskOutcome`。
    ///
    /// 后台 task 记录不是权威状态：它只承载本次 attempt 的输出与通知。映射必须
    /// 覆盖 `WorkTerminal` 的全部变体，`Suspended`/`Skipped` 没有对应的 task 状态，
    /// 按失败上报并保留原因。
    fn background_task_outcome(
        result: &crate::subagent::WorkResult,
    ) -> crate::background::TaskOutcome {
        use crate::background::{BackgroundTaskStatus, TaskOutcome, TaskUsage};
        use crate::subagent::WorkTerminal;

        let (status, reason, exit_code) = match &result.terminal {
            WorkTerminal::Success { .. } => (BackgroundTaskStatus::Completed, None, Some(0)),
            // 崩溃恢复落定的 Work 与 `recovery=fail` 共用 `Failed` 变体，靠
            // `stopReason=interrupted` 区分。后台 attempt 应报 Interrupted，而不是
            // 普通 Failed——两者在面板与通知里的恢复语义不同。
            WorkTerminal::Failed {
                stop_reason: Some(reason),
                ..
            } if reason == "interrupted" => (
                BackgroundTaskStatus::Interrupted,
                Some("process_lost".to_string()),
                None,
            ),
            WorkTerminal::Failed { error, .. } => {
                (BackgroundTaskStatus::Failed, Some(error.clone()), None)
            }
            WorkTerminal::Cancelled { reason } => (
                BackgroundTaskStatus::Cancelled,
                Some(reason.clone().unwrap_or_else(|| "cancelled".to_string())),
                None,
            ),
            WorkTerminal::TimedOut { reason } => {
                (BackgroundTaskStatus::TimedOut, reason.clone(), None)
            }
            WorkTerminal::Suspended { reason } => {
                (BackgroundTaskStatus::Failed, Some(reason.clone()), None)
            }
            WorkTerminal::Skipped { reason } => {
                (BackgroundTaskStatus::Failed, Some(reason.clone()), None)
            }
        };
        let output = match &result.terminal {
            WorkTerminal::Success {
                output: Some(value),
            } => format!(
                "{}\n",
                kiana_json::stringify(&kiana_json::normalize_numbers(value.clone()))
            ),
            _ => String::new(),
        };
        TaskOutcome {
            status,
            reason,
            exit_code,
            output,
            usage: TaskUsage {
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                total_tokens: result.usage.total_tokens,
            },
            acceptance: result
                .acceptance
                .as_ref()
                .map(|acceptance| acceptance.report.clone()),
        }
    }

    /// 后台 attempt 已随进程丢失、但 Work 仍可能被显式恢复时写入的 task 终态。
    fn background_attempt_interrupted() -> crate::background::TaskOutcome {
        crate::background::TaskOutcome {
            status: crate::background::BackgroundTaskStatus::Interrupted,
            reason: Some("process_lost".to_string()),
            exit_code: None,
            output: String::new(),
            usage: crate::background::TaskUsage::default(),
            acceptance: None,
        }
    }

    /// Work 生命周期：登记 → `begin`（Running）→ 事件 → 准备运行时 →
    /// `attach_session` → 运行 → `finish`（Terminal）。
    ///
    /// 新建与显式恢复只在“如何得到运行时”上不同；写前记录、超时映射、失败落定与
    /// store 错误上报必须完全一致，因此共用这一条路径。声明式 stage 流水线
    /// （`WorkLaunch::Stages`）在同一生命周期里执行，只是“运行”由多个 stage 组成。
    async fn run_work_lifecycle(
        &self,
        request: crate::subagent::WorkRequest,
        record: crate::subagent::WorkRecord,
        prompt: String,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
        launch: WorkLaunch,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        use crate::subagent::work::{SystemWorkClock, WorkClock, WorkEventKind, emit_work_event};
        use crate::subagent::work_store::{WorkBegin, WorkLiveGuard};

        let parent_session_id = record.parent_session_id.clone();
        let started_at_ms = record.created_at_ms;
        let clock: std::sync::Arc<dyn WorkClock> = work_options
            .clock
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(SystemWorkClock));
        let mut seq = 0u64;

        // 先登记活 Work，再写 Running 记录：否则并发的 `recover_work` 可能在 begin
        // 与登记之间看到“pid 属于本进程、登记表却没有”的记录，把它误判为崩溃遗留。
        let _work_live = work_options
            .store
            .as_ref()
            .map(|_| WorkLiveGuard::register(&parent_session_id, request.work_id, &request.run_id));
        let resumed_from = record.resumed_from;
        if let Some(store) = &work_options.store {
            match store
                .begin(record)
                .await
                .map_err(work_store_subagent_error)?
            {
                WorkBegin::Started(_) => {}
                WorkBegin::Existing(existing) => {
                    return Err(crate::subagent::SubagentError::WorkAlreadyExists {
                        work_id: existing.work_id.0,
                        status: existing.status().to_string(),
                        run_id: existing.run_id.0.clone(),
                    });
                }
                WorkBegin::AlreadyResumed(existing) => {
                    return Err(crate::subagent::SubagentError::WorkAlreadyResumed {
                        resumed_from: resumed_from.map(|work_id| work_id.0).unwrap_or_default(),
                        existing_work_id: existing.work_id.0,
                    });
                }
            }
        }

        emit_work_event(
            &work_options.event_sink,
            &clock,
            &request,
            &mut seq,
            WorkEventKind::Created,
        );
        emit_work_event(
            &work_options.event_sink,
            &clock,
            &request,
            &mut seq,
            WorkEventKind::Started {
                profile: request.profile.clone(),
            },
        );

        // 超时只取消子代理的 `CancellationToken`，不 drop future；这样 run 仍能写出
        // 终态与 transcript，WorkResult 可以把 Cancelled 映射为 TimedOut。
        //
        // 计时从进入生命周期开始：`prepare`（worktree / reflink / transcript 打开）
        // 是不可取消的同步调用，无法被打断，但它的耗时会从剩余预算里扣除，不能让
        // `timeoutSeconds` 被准备阶段绕过。
        let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_cancel = cancel.clone().unwrap_or_default().child_token();
        // `timeoutSeconds` 是宿主给的值，可能是任意大的数；`Instant + Duration`
        // 溢出会 panic。溢出时按“没有截止时间”处理并留下诊断，Work 仍会启动。
        let now = std::time::Instant::now();
        let deadline = work_options.timeout_seconds.and_then(|seconds| {
            let deadline = now.checked_add(std::time::Duration::from_secs(seconds));
            if deadline.is_none() {
                self.record_continuity_diagnostic(format!(
                    "work {} timeout {}s exceeds the clock range; running without a deadline",
                    request.work_id.0, seconds
                ));
            }
            deadline
        });

        // 声明式流水线没有单个运行时：每个 stage 自己准备、运行。所有 stage 结束后
        // 统一走 `finish_work_outcome`，终态、事件与 store 错误语义与单运行时一致。
        if let WorkLaunch::Stages {
            definition,
            runtime_options,
        } = launch
        {
            // Work 级超时对 staged 同样有效：它取消 `child_cancel`，stage 的子 token
            // 随之取消。没有这个定时器，未声明 stage 预算的流水线可以无限超期。
            let timer = deadline.map(|deadline| {
                let timed_out = std::sync::Arc::clone(&timed_out);
                let token = child_cancel.clone();
                tokio::spawn(async move {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
                    token.cancel();
                })
            });
            let run = self
                .run_staged_work(
                    &request,
                    definition,
                    runtime_options,
                    prompt,
                    child_cancel.clone(),
                    deadline,
                    &clock,
                    started_at_ms,
                )
                .await;
            if let Some(timer) = timer {
                timer.abort();
            }
            return Self::finish_work_outcome(
                &work_options,
                &request,
                &parent_session_id,
                started_at_ms,
                &clock,
                &mut seq,
                run,
                &timed_out,
                cancel.as_ref(),
                None,
                resumed_from,
            )
            .await;
        }

        let model = match self.resolve_subagent_model(launch.definition()) {
            Ok(model) => model,
            Err(error) => {
                return Self::finish_work_outcome(
                    &work_options,
                    &request,
                    &parent_session_id,
                    started_at_ms,
                    &clock,
                    &mut seq,
                    Err(error),
                    &timed_out,
                    cancel.as_ref(),
                    None,
                    resumed_from,
                )
                .await;
            }
        };
        let runtime = match launch.prepare(self) {
            Ok(runtime) => std::sync::Arc::new(runtime),
            Err(error) => {
                return Self::finish_work_outcome(
                    &work_options,
                    &request,
                    &parent_session_id,
                    started_at_ms,
                    &clock,
                    &mut seq,
                    Err(error),
                    &timed_out,
                    cancel.as_ref(),
                    None,
                    resumed_from,
                )
                .await;
            }
        };
        let timer = deadline.map(|deadline| {
            let timed_out = std::sync::Arc::clone(&timed_out);
            let token = child_cancel.clone();
            tokio::spawn(async move {
                // 截止时间在 `prepare` 之前就已确定；已经过期时立即触发。
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
                token.cancel();
            })
        });
        let session_file = runtime.session_file();
        if let (Some(store), Some(file)) = (&work_options.store, session_file.clone())
            && let Err(error) = store
                .attach_session(&parent_session_id, request.work_id, &request.run_id, &file)
                .await
        {
            if let Some(timer) = timer {
                timer.abort();
            }
            let error = crate::subagent::SubagentError::Agent {
                message: format!("work store error: {error}"),
            };
            return Self::finish_work_outcome(
                &work_options,
                &request,
                &parent_session_id,
                started_at_ms,
                &clock,
                &mut seq,
                Err(error),
                &timed_out,
                cancel.as_ref(),
                Some(file),
                resumed_from,
            )
            .await;
        }

        let run = self
            .run_prepared_subagent(runtime, model, prompt, Some(child_cancel))
            .await;
        if let Some(timer) = timer {
            timer.abort();
        }
        Self::finish_work_outcome(
            &work_options,
            &request,
            &parent_session_id,
            started_at_ms,
            &clock,
            &mut seq,
            run,
            &timed_out,
            cancel.as_ref(),
            session_file,
            resumed_from,
        )
        .await
    }

    /// 顺序执行声明式 stage 流水线。
    ///
    /// 每个 stage 都是一次独立子代理运行（owner 定义、独立会话），共享父 cwd；
    /// artifact 通过提示词传递，`yield` 契约由 `stages` 派生。最后一个 stage 的
    /// transcript 与全部 usage/receipt/执行记录汇总成 Work 的 outcome。
    ///
    /// 所有失败路径都**合成**终态返回而不是抛错：已完成 stage 的 usage、receipt、
    /// 执行记录与 transcript 必须保留，Work 级 acceptance 也必须在每条路径上用
    /// 宿主声明的 criteria 判定（stage 自己的 exit-criteria 报告不能进 WorkResult）。
    #[allow(clippy::too_many_arguments)]
    async fn run_staged_work(
        &self,
        request: &crate::subagent::WorkRequest,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: String,
        cancel: tokio_util::sync::CancellationToken,
        deadline: Option<std::time::Instant>,
        clock: &std::sync::Arc<dyn crate::subagent::WorkClock>,
        started_at_ms: u64,
    ) -> Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError> {
        use crate::agent_definition::StageArtifact;
        use crate::subagent::{SubagentTerminal, SubagentTerminalState, SubagentUsage};

        let Some(stages) = definition
            .stages
            .clone()
            .filter(|stages| !stages.is_empty())
        else {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("agent `{}` has no stages", definition.name),
            });
        };
        let total = stages.len();
        let mut artifacts: std::collections::BTreeMap<StageArtifact, kiana_ai::types::JsonValue> =
            std::collections::BTreeMap::new();
        let mut usage = SubagentUsage::default();
        let mut receipts: Vec<crate::subagent::ToolReceipt> = Vec::new();
        let mut executions: Vec<crate::acceptance::ExecutionRecord> = Vec::new();
        // 每个 stage runtime 的 receipt.seq 都从 1 开始；直接串联会让 `tests_passed`
        // 的“最新一次匹配”选中前一个 stage 的旧执行。跨 stage 重新编号成单调序列。
        let mut next_execution_seq = 1u64;
        // 证据（receipts）与执行记录是两套独立视图，同样必须跨 stage 单调。
        let mut next_receipt_seq = 1u64;
        // Work 级 token 预算跨 stage 共享；记录前面 stage 已消耗的可计费 token。
        let mut work_budget_used = 0u64;
        let mut last_outcome: Option<crate::subagent::SubagentRunOutcome> = None;
        let mut last_session_file: Option<String> = None;

        for (index, stage) in stages.iter().enumerate() {
            // 取消/超时后不再为后续 stage 解析 owner、准备运行时。准备失败会被合成
            // 为 Failed，把已经发生的取消/超时改写成失败终态；这里直接按取消收尾，
            // 由 `finish_work_outcome` 决定最终是 Cancelled 还是 TimedOut。
            if cancel.is_cancelled() {
                let terminal =
                    SubagentTerminalState::with_terminal(SubagentTerminal::Cancelled, None);
                return Ok(self.finalize_staged_outcome(
                    request,
                    last_outcome.take(),
                    Some(terminal),
                    usage,
                    receipts,
                    &executions,
                    last_session_file,
                ));
            }
            // owner 定义不递归执行自己的 `stages`；owner 名不存在时由加载器报错。
            let mut owner = match match stage.owner.as_deref() {
                Some(owner) if owner != definition.name => self.load_subagent_definition(owner),
                _ => Ok(definition.clone()),
            } {
                Ok(owner) => owner,
                Err(error) => {
                    let terminal = SubagentTerminalState::failed(
                        Self::stage_run_error(&stage.name, error).to_string(),
                    );
                    return Ok(self.finalize_staged_outcome(
                        request,
                        last_outcome.take(),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            };
            owner.stages = None;
            // 与启动前预检使用同一个有效隔离值：运行时 override 优先于 owner 声明。
            // staged 流水线无法为单个 stage 建工作区，因此有效值必须是 shared；只看
            // `owner.isolation` 会让 `isolation=worktree` 的定义 + 运行时 shared
            // override 通过预检后在第一个 stage 才失败。
            let effective_isolation = runtime_options.isolation.unwrap_or(owner.isolation);
            if effective_isolation != crate::subagent::IsolationMode::Shared {
                let terminal = SubagentTerminalState::failed(format!(
                    "stage `{}` owner `{}` requires isolation={}; staged workflows require shared isolation",
                    stage.name, owner.name, effective_isolation
                ));
                return Ok(self.finalize_staged_outcome(
                    request,
                    last_outcome.take(),
                    Some(terminal),
                    usage,
                    receipts,
                    &executions,
                    last_session_file,
                ));
            }
            if let Some(max_turns) = stage.budget.max_turns {
                owner.max_turns = Some(max_turns);
            }
            let model = match self.resolve_subagent_model(&owner) {
                Ok(model) => model,
                Err(error) => {
                    let terminal = SubagentTerminalState::failed(
                        Self::stage_run_error(&stage.name, error).to_string(),
                    );
                    return Ok(self.finalize_staged_outcome(
                        request,
                        last_outcome.take(),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            };
            let mut options = runtime_options.clone();
            // 每个 stage 独立会话；调用方固定的 id 会在 stage 之间冲突。
            options.session_id = None;
            options.sidechain_id = None;
            // stage 共享父 cwd（owner 的隔离声明已在上方校验为 shared）。
            options.isolation = Some(crate::subagent::IsolationMode::Shared);
            options.token_budget = Self::staged_stage_token_budget(
                stage.budget.token_budget,
                runtime_options.token_budget,
                work_budget_used,
            );
            options.acceptance_criteria = stage.exit_criteria.clone();
            options.yield_contract = Some(crate::subagent::stages::stage_yield_contract(stage));
            let runtime = match self.prepare_subagent(owner, options) {
                Ok(runtime) => std::sync::Arc::new(runtime),
                Err(error) => {
                    let terminal = SubagentTerminalState::failed(
                        Self::stage_run_error(&stage.name, error).to_string(),
                    );
                    return Ok(self.finalize_staged_outcome(
                        request,
                        last_outcome.take(),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            };
            let execution_runtime = std::sync::Arc::clone(&runtime);
            let stage_cancel = cancel.child_token();
            let stage_timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stage_timer = match stage.budget.timeout_seconds {
                Some(seconds) => {
                    let now = std::time::Instant::now();
                    match now.checked_add(std::time::Duration::from_secs(seconds)) {
                        // 只有严格早于 Work 截止时间的 stage deadline 才单独 arm：
                        // 两个定时器落在同一 Instant 时，谁先被调度谁就把终态改写成
                        // 自己的 reason/预算归属。Work 截止时间不晚于 stage 时，超时
                        // 归 Work 级，由 `finish_work_outcome` 统一映射。
                        Some(stage_deadline)
                            if Self::stage_deadline_precedes_work(stage_deadline, deadline) =>
                        {
                            let timed_out = std::sync::Arc::clone(&stage_timed_out);
                            let token = stage_cancel.clone();
                            Some(tokio::spawn(async move {
                                tokio::time::sleep_until(tokio::time::Instant::from_std(
                                    stage_deadline,
                                ))
                                .await;
                                timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
                                token.cancel();
                            }))
                        }
                        Some(_) => None,
                        None => {
                            self.record_continuity_diagnostic(format!(
                                "stage `{}` timeout {}s exceeds the clock range; \
                                 running without a stage deadline",
                                stage.name, seconds
                            ));
                            None
                        }
                    }
                }
                None => None,
            };
            let stage_prompt = crate::subagent::stages::build_stage_prompt(
                &prompt, stage, index, total, &artifacts,
            );
            let run = self
                .run_prepared_subagent(runtime, model, stage_prompt, Some(stage_cancel))
                .await;
            if let Some(timer) = stage_timer {
                timer.abort();
            }
            let mut outcome = match run {
                Ok(outcome) => outcome,
                Err(error) => {
                    let terminal = SubagentTerminalState::failed(
                        Self::stage_run_error(&stage.name, error).to_string(),
                    );
                    return Ok(self.finalize_staged_outcome(
                        request,
                        last_outcome.take(),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            };
            work_budget_used = work_budget_used.saturating_add(
                outcome
                    .usage
                    .input_tokens
                    .saturating_add(outcome.usage.output_tokens)
                    .saturating_add(outcome.usage.cache_read_tokens)
                    .saturating_add(outcome.usage.cache_write_tokens),
            );
            usage.add_snapshot(&outcome.usage);
            // 每个 stage 的 receipt.seq 也从 1 开始；直接拼接会让 Work evidence 出现
            // 重复序号，审计无法排序/去重。与 execution seq 一样跨 stage 重新编号。
            for receipt in &mut outcome.receipts {
                receipt.seq = next_receipt_seq;
                next_receipt_seq = next_receipt_seq.saturating_add(1);
                receipts.push(receipt.clone());
            }
            let mut stage_executions = execution_runtime.execution_records();
            for execution in &mut stage_executions {
                execution.receipt.seq = next_execution_seq;
                next_execution_seq = next_execution_seq.saturating_add(1);
            }
            executions.extend(stage_executions);
            if let Some(file) = outcome.session_file.clone() {
                last_session_file = Some(file);
            }

            // Review gate：产出即持久化，不等 stage 终态或 exit-criteria 判定。stage
            // 已经 yield 的 findings 不能因为自身随后失败、取消或超时而丢失；写入后
            // 严格回读，缺失/非法/陈旧都按 reject。
            match crate::subagent::stages::review_findings_from_output(
                stage,
                outcome.output.as_ref(),
            ) {
                Ok(None) => {}
                Ok(Some(findings)) => {
                    if let Err(message) = crate::subagent::stages::persist_review(
                        std::path::Path::new(&self.core.cwd),
                        &self.session_id(),
                        request.work_id,
                        &request.run_id,
                        stage,
                        &findings,
                        started_at_ms,
                        clock.now_ms(),
                    ) {
                        outcome.output =
                            Some(crate::subagent::stages::artifacts_to_json(&artifacts));
                        let terminal =
                            SubagentTerminalState::failed(format!("review rejected: {message}"));
                        return Ok(self.finalize_staged_outcome(
                            request,
                            Some(outcome),
                            Some(terminal),
                            usage,
                            receipts,
                            &executions,
                            last_session_file,
                        ));
                    }
                }
                Err(message) => {
                    let terminal = SubagentTerminalState::failed(message);
                    return Ok(self.finalize_staged_outcome(
                        request,
                        Some(outcome),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            }

            // stage 预算超时：把这次运行标成 TimedOut（Work 级取消/超时保持原终态）。
            if Self::stage_deadline_won(
                stage_timed_out.load(std::sync::atomic::Ordering::SeqCst),
                cancel.is_cancelled(),
                outcome.terminal.terminal,
            ) {
                let terminal = SubagentTerminalState::with_terminal(
                    SubagentTerminal::TimedOut,
                    Some(format!(
                        "stage `{}` timed out after {}s",
                        stage.name,
                        stage.budget.timeout_seconds.unwrap_or_default()
                    )),
                );
                return Ok(self.finalize_staged_outcome(
                    request,
                    Some(outcome),
                    Some(terminal),
                    usage,
                    receipts,
                    &executions,
                    last_session_file,
                ));
            }
            match outcome.terminal.terminal {
                SubagentTerminal::Cancelled
                | SubagentTerminal::TimedOut
                | SubagentTerminal::PollingTimedOut => {
                    // Work 级取消或超时：保留原始终态，后续 stage 不再执行。
                    return Ok(self.finalize_staged_outcome(
                        request,
                        Some(outcome),
                        None,
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
                SubagentTerminal::Failed => {
                    let error = outcome
                        .terminal
                        .error
                        .clone()
                        .unwrap_or_else(|| "subagent failed".to_string());
                    outcome.terminal.error = Some(format!("stage `{}`: {error}", stage.name));
                    return Ok(self.finalize_staged_outcome(
                        request,
                        Some(outcome),
                        None,
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
                SubagentTerminal::Completed => {}
            }
            // exit criteria 是硬门禁：声明了 criteria 时 `Passed` 才算通过；`Unverified`
            // 表示无法确定性判定，不能静默放行。
            if !stage.exit_criteria.is_empty() {
                let status = outcome.acceptance.as_ref().map(|report| report.status);
                if status != Some(crate::acceptance::AcceptanceStatus::Passed) {
                    let (label, details) = match (status, outcome.acceptance.as_ref()) {
                        (Some(crate::acceptance::AcceptanceStatus::Failed), Some(report)) => (
                            "failed",
                            report
                                .leaves
                                .iter()
                                .filter(|leaf| leaf.checked && !leaf.holds)
                                .map(|leaf| leaf.criterion.clone())
                                .collect::<Vec<_>>(),
                        ),
                        (Some(_), Some(report)) => ("unverified", report.unchecked.clone()),
                        _ => ("unverified", stage.exit_criteria.clone()),
                    };
                    let terminal = SubagentTerminalState::failed(format!(
                        "stage `{}` exit criteria {label}: {}",
                        stage.name,
                        if details.is_empty() {
                            "no criterion could be evaluated".to_string()
                        } else {
                            details.join("; ")
                        }
                    ));
                    return Ok(self.finalize_staged_outcome(
                        request,
                        Some(outcome),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            }
            let produced = match crate::subagent::stages::collect_stage_artifacts(
                stage,
                outcome.output.as_ref(),
            ) {
                Ok(produced) => produced,
                Err(message) => {
                    let terminal = SubagentTerminalState::failed(message);
                    return Ok(self.finalize_staged_outcome(
                        request,
                        Some(outcome),
                        Some(terminal),
                        usage,
                        receipts,
                        &executions,
                        last_session_file,
                    ));
                }
            };
            for (artifact, value) in produced {
                artifacts.insert(artifact, value);
            }
            // review.json 已在 stage 收尾时持久化（见上面的产出即持久化）。
            last_outcome = Some(outcome);
        }

        let Some(outcome) = last_outcome else {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("agent `{}` has no stages", definition.name),
            });
        };
        let mut outcome = self.finalize_staged_outcome(
            request,
            Some(outcome),
            None,
            usage,
            receipts,
            &executions,
            last_session_file,
        );
        outcome.output = Some(crate::subagent::stages::artifacts_to_json(&artifacts));
        Ok(outcome)
    }

    /// stage 循环的早退收尾：保留已累积的 usage / receipt / 执行记录与最后一个
    /// stage 的 transcript，并用 Work 级 criteria 重新判定 acceptance。
    ///
    /// `terminal` 为 `Some` 时替换终态；为 `None` 时保留 outcome 原终态（取消/超时/
    /// stage 自身失败）。stage 自己的 exit-criteria 报告不能进 WorkResult：宿主没有
    /// 声明那些 criteria。
    #[allow(clippy::too_many_arguments)]
    fn finalize_staged_outcome(
        &self,
        request: &crate::subagent::WorkRequest,
        outcome: Option<crate::subagent::SubagentRunOutcome>,
        terminal: Option<crate::subagent::SubagentTerminalState>,
        usage: crate::subagent::SubagentUsage,
        receipts: Vec<crate::subagent::ToolReceipt>,
        executions: &[crate::acceptance::ExecutionRecord],
        session_file: Option<String>,
    ) -> crate::subagent::SubagentRunOutcome {
        let acceptance = crate::acceptance::evaluate_acceptance(
            &request.acceptance_criteria,
            &crate::acceptance::AcceptanceContext::new(
                std::path::Path::new(&self.core.cwd),
                executions,
            ),
        );
        let mut outcome = outcome.unwrap_or_else(|| crate::subagent::SubagentRunOutcome {
            terminal: crate::subagent::SubagentTerminalState::failed("staged workflow failed"),
            messages: Vec::new(),
            usage,
            receipts: Vec::new(),
            output: None,
            acceptance: None,
            session_file: None,
            isolation: None,
        });
        match terminal {
            Some(terminal) => outcome.terminal = terminal.with_usage(usage),
            None => outcome.terminal.usage = usage,
        }
        outcome.usage = usage;
        outcome.receipts = receipts;
        outcome.acceptance = Some(acceptance);
        outcome.session_file = session_file;
        outcome
    }

    /// Work 运行的统一收尾：成功映射为 `WorkResult` 并落定；失败先归还恢复锚点，
    /// 再落一条失败记录。新建、恢复与 staged 流水线共用这条路径。
    #[allow(clippy::too_many_arguments)]
    async fn finish_work_outcome(
        work_options: &crate::subagent::WorkOptions,
        request: &crate::subagent::WorkRequest,
        parent_session_id: &str,
        started_at_ms: u64,
        clock: &std::sync::Arc<dyn crate::subagent::WorkClock>,
        seq: &mut u64,
        run: Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError>,
        timed_out: &std::sync::atomic::AtomicBool,
        cancel: Option<&tokio_util::sync::CancellationToken>,
        session_file: Option<String>,
        resumed_from: Option<crate::subagent::WorkId>,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        use crate::subagent::work::{WorkEventKind, WorkResult, emit_work_event};

        let outcome = match run {
            Ok(outcome) => outcome,
            Err(error) => {
                // 先落定终态、再归还锚点。反过来会出现“Running 且已经没有锚点”
                // 的记录：它在 recovery=resume 下与原记录同时可恢复，两个 runtime
                // 会追加同一个 child JSONL。落定失败时保留锚点（交给启动恢复），
                // 终态写成功后才释放；释放失败会留下一个非 Success 终态，
                // `recover_work` 的修复会按同一策略再次归还。
                let finish_error = Self::finish_failed_work(
                    work_options,
                    request,
                    parent_session_id,
                    started_at_ms,
                    clock,
                    seq,
                    error.to_string(),
                    session_file,
                )
                .await;
                if finish_error.is_some() {
                    return Err(attach_work_store_error(error, finish_error));
                }
                let release_error = Self::release_resume_claim(
                    work_options,
                    request,
                    parent_session_id,
                    resumed_from,
                )
                .await;
                return Err(attach_work_store_error(error, release_error));
            }
        };
        let finished_at_ms = clock.now_ms();
        let mut result = WorkResult::from_outcome(request, &outcome, started_at_ms, finished_at_ms);
        // 外部取消优先于超时：两者同时触发时保留 Cancelled，不能把调用方
        // 主动取消伪装成超时。
        let parent_cancelled = cancel.is_some_and(|token| token.is_cancelled());
        result.terminal = apply_work_timeout(
            result.terminal,
            timed_out.load(std::sync::atomic::Ordering::SeqCst),
            parent_cancelled,
        );
        // 事件是提交的投影（Phase 3）：先做终态 CAS，只有真的写入（或没有账本）
        // 才投影 Acceptance/Terminal。CAS 失败时 Work 仍是 Running，投影 Terminal
        // 会让事件流与权威记录分叉。
        let (finish_error, release_error) = match &work_options.store {
            Some(store) => {
                let finish_error =
                    work_finish_store_error(store.finish(parent_session_id, &result).await);
                // Success 与“仍可恢复的 interrupted tip”保留锚点；其余终态归还，
                // 否则 `recovery=resume` 的后继失败/取消/超时后原记录永久不可重试。
                let release_error = if finish_error.is_none()
                    && resumed_from.is_some()
                    && !terminal_keeps_resume_anchor(
                        &result.terminal,
                        request.resumable,
                        result.session_file.as_deref(),
                    ) {
                    store
                        .release_resume(parent_session_id, request.work_id, &request.run_id)
                        .await
                        .err()
                } else {
                    None
                };
                (finish_error, release_error)
            }
            None => (None, None),
        };
        if finish_error.is_none() {
            emit_work_event(
                &work_options.event_sink,
                clock,
                request,
                seq,
                WorkEventKind::AcceptanceEvaluated {
                    status: result
                        .acceptance
                        .as_ref()
                        .map(|acceptance| acceptance.report.status.as_str().to_string())
                        .unwrap_or_else(|| "unverified".to_string()),
                },
            );
            emit_work_event(
                &work_options.event_sink,
                clock,
                request,
                seq,
                WorkEventKind::Terminal {
                    status: result.terminal.status().to_string(),
                },
            );
        }
        let store_error = match (finish_error.map(|error| error.to_string()), release_error) {
            (None, None) => None,
            (Some(finish), None) => Some(finish),
            (None, Some(release)) => Some(release.to_string()),
            (Some(finish), Some(release)) => Some(format!("{finish}; {release}")),
        };
        Ok(crate::subagent::WorkRunOutcome {
            request: request.clone(),
            result,
            outcome,
            store_error,
        })
    }

    /// 给准备阶段的错误加上 stage 前缀（成功运行的失败走合成终态，不经过这里）。
    fn stage_run_error(
        stage: &str,
        error: crate::subagent::SubagentError,
    ) -> crate::subagent::SubagentError {
        crate::subagent::SubagentError::Agent {
            message: format!("stage `{stage}`: {error}"),
        }
    }

    /// stage 实际可用的 token 预算。
    ///
    /// 调用方声明的 Work 级预算跨 stage 共享：每个 stage 只能拿到扣除前面 stage
    /// 已消耗后的余额；stage 自己的预算只能进一步收紧，不能放大。
    fn staged_stage_token_budget(
        stage_budget: Option<u64>,
        work_budget: Option<u64>,
        work_budget_used: u64,
    ) -> Option<u64> {
        let remaining = work_budget.map(|limit| limit.saturating_sub(work_budget_used));
        match (stage_budget, remaining) {
            (Some(stage), Some(remaining)) => Some(stage.min(remaining)),
            (Some(stage), None) => Some(stage),
            (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        }
    }

    /// stage 预算定时器是否真的决定了终态。
    ///
    /// stage 定时器只为**严格早于** Work 截止时间的预算 arm（见 `run_staged_work`），
    /// 因此两个定时器落在同一 Instant 时不会互相改写终态归属。定时器取消与 stage
    /// 收尾仍会竞争：已经 `Completed` 的运行（取消生效前已收尾）与 Work 级取消
    /// （host cancel / Work deadline）都优先于 stage 超时，否则会把成功结果或宿主
    /// 主动取消改写成 `TimedOut`，并丢掉已产出的 artifact。
    fn stage_deadline_won(
        stage_timed_out: bool,
        work_cancelled: bool,
        terminal: crate::subagent::SubagentTerminal,
    ) -> bool {
        stage_timed_out
            && !work_cancelled
            && terminal != crate::subagent::SubagentTerminal::Completed
    }

    /// stage 预算是否严格早于 Work 截止时间（没有 Work 截止时间时总是由 stage 决定）。
    ///
    /// 相等也不行：两个定时器在同一 Instant 到期时谁先被调度取决于运行时，stage
    /// 先跑会把 Work 级超时改写成 stage 的 reason 与预算归属。
    fn stage_deadline_precedes_work(
        stage_deadline: std::time::Instant,
        work_deadline: Option<std::time::Instant>,
    ) -> bool {
        work_deadline.is_none_or(|work| stage_deadline < work)
    }

    /// 后继在真正开始运行前失败时，把 `begin` 占用的恢复锚点还回去。
    ///
    /// `begin` 先于 `prepare` 是为了在账本事务里原子占用锚点（并发恢复只有一个
    /// 赢家）；但打开 transcript / 准备隔离工作区失败时这次后继从未执行，锚点若继续
    /// 被它占着，宿主就永远无法重试同一个原记录。
    async fn release_resume_claim(
        work_options: &crate::subagent::WorkOptions,
        request: &crate::subagent::WorkRequest,
        parent_session_id: &str,
        resumed_from: Option<crate::subagent::WorkId>,
    ) -> Option<crate::subagent::WorkStoreError> {
        resumed_from?;
        work_options
            .store
            .as_ref()?
            .release_resume(parent_session_id, request.work_id, &request.run_id)
            .await
            .err()
    }

    /// 在运行前失败时落定 Work 记录并发 Terminal 事件。
    #[allow(clippy::too_many_arguments)]
    async fn finish_failed_work(
        work_options: &crate::subagent::WorkOptions,
        request: &crate::subagent::WorkRequest,
        parent_session_id: &str,
        started_at_ms: u64,
        clock: &std::sync::Arc<dyn crate::subagent::WorkClock>,
        seq: &mut u64,
        message: String,
        session_file: Option<String>,
    ) -> Option<crate::subagent::WorkStoreError> {
        let mut result =
            crate::subagent::WorkResult::failure(request, message, started_at_ms, clock.now_ms());
        result.session_file = session_file;
        let finish_error = match &work_options.store {
            Some(store) => work_finish_store_error(store.finish(parent_session_id, &result).await),
            None => None,
        };
        // 与成功路径一致：只有终态 CAS 成功（或没有账本）才投影 Terminal。
        if finish_error.is_none() {
            crate::subagent::work::emit_work_event(
                &work_options.event_sink,
                clock,
                request,
                seq,
                crate::subagent::WorkEventKind::Terminal {
                    status: result.terminal.status().to_string(),
                },
            );
        }
        finish_error
    }

    /// 按名字加载定义并启动带 Work envelope 的子代理。
    pub async fn spawn_named_subagent_with_work(
        &self,
        name: &str,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        let definition = self.load_subagent_definition(name)?;
        self.spawn_subagent_with_work(definition, runtime_options, prompt, cancel, work_options)
            .await
    }

    /// 按名字加载定义并在后台启动带 Work envelope 的子代理。
    ///
    /// 与 [`Self::spawn_named_subagent_with_work`] 相同，只是立即返回 task/Work 身份；
    /// 预检（定义加载、执行所有权、请求校验、staged 边界）仍在创建后台任务之前完成。
    pub async fn spawn_named_subagent_with_work_in_background(
        &self,
        name: &str,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<crate::subagent::BackgroundWorkHandle, crate::subagent::SubagentError> {
        let definition = self.load_subagent_definition(name)?;
        self.spawn_subagent_with_work_in_background(
            definition,
            runtime_options,
            prompt,
            cancel,
            work_options,
        )
        .await
    }

    /// 显式恢复一个中断的 Work。
    ///
    /// 不自动恢复：工具调用可能有副作用，必须由宿主确认。要求原记录
    /// `request.resumable == true` 且有 `sessionFile`，并且没有仍存活的执行者。
    /// 崩溃遗留的 Running 记录先以 CAS 落定为 `interrupted`（并发恢复的输家会在这里
    /// 失败），然后创建一条新记录（新 `workId` / `runId`，`resumedFrom` 指向原记录，
    /// `attempts + 1`），复用原 transcript 继续运行。
    pub async fn resume_work(
        &self,
        work_id: crate::subagent::WorkId,
        continuation_prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        self.resume_work_with_options(
            work_id,
            continuation_prompt,
            cancel,
            crate::subagent::WorkOptions::default(),
        )
        .await
    }

    /// 与 [`Self::resume_work`] 相同，但允许宿主提供事件接收器与时钟。
    ///
    /// 恢复会分配新的 `WorkId`；没有 `event_sink` 时宿主收不到任何 `WorkEvent`，
    /// 因此需要事件流的宿主应使用本方法。请求快照、`timeoutSeconds` 与审批语义以
    /// 原记录为准，`work_options` 的同名字段被忽略。
    pub async fn resume_work_with_options(
        &self,
        work_id: crate::subagent::WorkId,
        continuation_prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
        work_options: crate::subagent::WorkOptions,
    ) -> Result<crate::subagent::WorkRunOutcome, crate::subagent::SubagentError> {
        use crate::subagent::work::{SystemWorkClock, WorkClock, WorkResult, new_run_id};
        use crate::subagent::work_store::{WorkFinish, WorkRecord, WorkState};

        if let Some(refusal) = self.execution_refusal() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("Cannot start a run: {refusal}"),
            });
        }
        // 与 `spawn_subagent_with_work` 一致：宿主显式注入的 store 优先，没有时才回退
        // 到会话的 durable 账本。读取锚点、占用与整条生命周期必须用同一个 store。
        let store: std::sync::Arc<dyn crate::subagent::WorkStore> = match work_options.store.clone()
        {
            Some(store) => store,
            None => match self.durable_work_store() {
                Some(store) => store as std::sync::Arc<dyn crate::subagent::WorkStore>,
                None => {
                    return Err(crate::subagent::SubagentError::Agent {
                        message: "work resume requires a durable work store".to_string(),
                    });
                }
            },
        };
        let parent_session_id = self.session_id();
        let original = store
            .load(&parent_session_id, work_id)
            .await
            .map_err(work_store_subagent_error)?
            .ok_or_else(|| crate::subagent::SubagentError::Agent {
                message: format!("work {} does not exist", work_id.0),
            })?;
        if !original.request.resumable {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("work {} is not resumable", work_id.0),
            });
        }
        let Some(session_file) = original.session_file().map(str::to_string) else {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("work {} has no session file to resume", work_id.0),
            });
        };
        // 仍在运行的 Work 不能恢复：两个执行者会并发写同一 transcript。能否打开本会话
        // 的 durable 目录已经证明“另一个进程没有持有目录租约”，所以只有本进程的登记表
        // 命中才算活的。
        let alive = original.pid == Some(std::process::id())
            && crate::subagent::work_store::work_is_live(
                &parent_session_id,
                original.work_id,
                &original.run_id,
            );
        if alive {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!("work {} is still running", work_id.0),
            });
        }
        // 定义在恢复时重新加载；如果它现在声明了 stages，旧 transcript 无法表达
        // “从第几个 stage 继续”，在改动账本之前拒绝。
        let definition = self.load_subagent_definition(&original.request.profile)?;
        if definition.stages.is_some() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!(
                    "work {} cannot be resumed: agent `{}` declares stages",
                    work_id.0, original.request.profile
                ),
            });
        }

        // 先校验宿主给出的续跑输入，再改动账本：空 prompt 不该让原记录先被落定。
        let mut request = original.request.clone();
        request.prompt = continuation_prompt.into();
        request
            .validate()
            .map_err(|error| crate::subagent::SubagentError::Agent {
                message: error.to_string(),
            })?;

        let clock: std::sync::Arc<dyn WorkClock> = work_options
            .clock
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(SystemWorkClock));
        let now = clock.now_ms();
        match &original.state {
            WorkState::Running { .. } => {
                // 宿主已确认恢复：先把崩溃遗留的 Running 落定为 interrupted，闭合原记录
                // 的生命周期，也把“谁有权恢复”交给 CAS 的赢家。
                let interrupted = WorkResult::interrupted(
                    &original.request,
                    "work interrupted: resumed by host",
                    Some(session_file.clone()),
                    original.created_at_ms,
                    now,
                );
                match store
                    .finish(&parent_session_id, &interrupted)
                    .await
                    .map_err(work_store_subagent_error)?
                {
                    WorkFinish::Finished(_) => {}
                    WorkFinish::NotApplied(existing) => {
                        return Err(crate::subagent::SubagentError::Agent {
                            message: format!(
                                "work {} is no longer resumable (status {})",
                                work_id.0,
                                existing.status()
                            ),
                        });
                    }
                    WorkFinish::Missing => {
                        return Err(crate::subagent::SubagentError::Agent {
                            message: format!("work {} disappeared during resume", work_id.0),
                        });
                    }
                }
            }
            WorkState::Terminal { result, .. } => {
                let interrupted = matches!(
                    &result.terminal,
                    crate::subagent::WorkTerminal::Failed { stop_reason, .. }
                        if stop_reason.as_deref() == Some("interrupted")
                );
                if !interrupted {
                    return Err(crate::subagent::SubagentError::Agent {
                        message: format!(
                            "work {} is not interrupted (status {}); refusing to resume",
                            work_id.0,
                            result.terminal.status()
                        ),
                    });
                }
            }
        }

        request.work_id = store
            .allocate_work_id(&parent_session_id)
            .await
            .map_err(work_store_subagent_error)?;
        request.run_id = new_run_id();
        let mut record = WorkRecord::new(
            request.clone(),
            parent_session_id,
            original.parent_turn_id.clone(),
            Some(std::process::id()),
            now,
        )
        .with_resumed_from(original.work_id);
        record.attempts = original.attempts.saturating_add(1);

        let session_path = std::path::PathBuf::from(&session_file);
        let runtime_options = crate::subagent::SubagentRuntimeOptions {
            // resume 只按 `session_path` 打开 transcript；`session_dir` 保持与记录一致，
            // 供运行时的会话文件链接使用。
            session_dir: session_path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .map(std::path::Path::to_path_buf),
            depth: request.depth,
            token_budget: request.token_budget,
            isolation: request.isolation,
            acceptance_criteria: request.acceptance_criteria.clone(),
            ..Default::default()
        };
        // `attach_session` 在 `prepare` 之后立刻把路径写进账本，但子会话文件要等首条
        // 消息落盘才真正创建。崩溃在这个窗口里时记录指向一个不存在的文件：没有历史
        // 可复用，按新建运行时继续（`resume` 打开空文件会因缺少 link 条目失败）。
        let launch = if session_path.exists() {
            WorkLaunch::Resume {
                definition,
                runtime_options,
                session_path,
            }
        } else {
            self.record_continuity_diagnostic(format!(
                "work {} resume: session file {} is missing; starting a fresh session",
                work_id.0, session_file
            ));
            WorkLaunch::Fresh {
                definition,
                runtime_options,
            }
        };
        let prompt = request.prompt.clone();
        // 保留宿主传入的事件接收器：恢复出来的新 WorkId 也要发 WorkEvent。
        let mut lifecycle_options = work_options;
        lifecycle_options.work_id = Some(request.work_id);
        lifecycle_options.run_id = Some(request.run_id.clone());
        lifecycle_options.timeout_seconds = request.timeout_seconds;
        lifecycle_options.store = Some(store);
        lifecycle_options.clock = Some(clock);
        self.run_work_lifecycle(request, record, prompt, cancel, lifecycle_options, launch)
            .await
    }

    /// 恢复一个子代理会话并继续运行（transcript 复用，不重复初始 prompt）。
    pub async fn resume_and_run_subagent(
        &self,
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        session_path: impl AsRef<std::path::Path>,
        prompt: impl Into<String>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<crate::subagent::SubagentRunOutcome, crate::subagent::SubagentError> {
        if definition.stages.is_some() {
            return Err(crate::subagent::SubagentError::Agent {
                message: format!(
                    "agent `{}` declares stages; staged work is not resumable",
                    definition.name
                ),
            });
        }
        let model = self.resolve_subagent_model(&definition)?;
        let runtime =
            std::sync::Arc::new(self.resume_subagent(definition, runtime_options, session_path)?);
        self.run_prepared_subagent(runtime, model, prompt.into(), cancel)
            .await
    }
}

/// 一次 Work 运行如何得到运行时：全新准备，或复用已有 transcript。
/// `spawn_subagent_with_work` 与后台入口共用的预检结果。
struct PreparedWork {
    request: crate::subagent::WorkRequest,
    record: crate::subagent::WorkRecord,
    prompt: String,
    work_options: crate::subagent::WorkOptions,
    launch: WorkLaunch,
}

enum WorkLaunch {
    /// 新建子代理会话。
    Fresh {
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
    },
    /// 复用已有子代理 transcript（显式恢复）。
    Resume {
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
        session_path: std::path::PathBuf,
    },
    /// 声明式 stage 流水线（Phase 2）。
    Stages {
        definition: crate::agent_definition::AgentDefinition,
        runtime_options: crate::subagent::SubagentRuntimeOptions,
    },
}

impl WorkLaunch {
    fn definition(&self) -> &crate::agent_definition::AgentDefinition {
        match self {
            WorkLaunch::Fresh { definition, .. } => definition,
            WorkLaunch::Resume { definition, .. } => definition,
            WorkLaunch::Stages { definition, .. } => definition,
        }
    }

    fn prepare(
        self,
        session: &AgentSession,
    ) -> Result<crate::subagent::SubagentRuntime, crate::subagent::SubagentError> {
        match self {
            WorkLaunch::Fresh {
                definition,
                runtime_options,
            } => session.prepare_subagent(definition, runtime_options),
            WorkLaunch::Resume {
                definition,
                runtime_options,
                session_path,
            } => session.resume_subagent(definition, runtime_options, session_path),
            // staged 启动在生命周期里直接进入 `run_staged_work`，不会走到这里。
            WorkLaunch::Stages { .. } => Err(crate::subagent::SubagentError::Agent {
                message: "staged work cannot prepare a single runtime".to_string(),
            }),
        }
    }
}

fn work_store_subagent_error(
    error: crate::subagent::WorkStoreError,
) -> crate::subagent::SubagentError {
    crate::subagent::SubagentError::Agent {
        message: format!("work store error: {error}"),
    }
}

/// 把 `finish` 的结果映射成需要上报的 store 错误。
///
/// 只有真正写入终态（`Finished`）才算成功：`NotApplied`（记录已终态或 run 不匹配）
/// 与 `Missing` 都意味着本次终态没有落账，必须像 `Err` 一样上报，否则失败路径会
/// 把“没有写进去”当成成功。
fn work_finish_store_error(
    outcome: Result<crate::subagent::WorkFinish, crate::subagent::WorkStoreError>,
) -> Option<crate::subagent::WorkStoreError> {
    use crate::subagent::WorkFinish;
    match outcome {
        Ok(WorkFinish::Finished(_)) => None,
        Ok(WorkFinish::NotApplied(existing)) => {
            Some(crate::subagent::WorkStoreError::new(format!(
                "work finish was not applied; record is {}",
                existing.status()
            )))
        }
        Ok(WorkFinish::Missing) => Some(crate::subagent::WorkStoreError::new(
            "work record missing during finish",
        )),
        Err(error) => Some(error),
    }
}

/// 终态后继是否继续占用恢复锚点。
///
/// 只有两类记录保留占用：
/// - `Success`：一个锚点只成功恢复一次；
/// - 仍可被 `resume_work` 接受的 interrupted 记录（`resumable` 且有 `sessionFile`）：
///   它是 lineage 的 tip。保留占用才能保证同一时刻只有一个记录可恢复；否则原记录
///   与后继会同时可恢复，两个 runtime 会追加同一个 child JSONL。
///
/// 其它终态（`Cancelled` / `TimedOut` / 非 interrupted 的 `Failed` / 不可恢复的
/// interrupted）都不能作为下一次恢复目标，必须归还锚点，否则 lineage 永久不可重试。
fn terminal_keeps_resume_anchor(
    terminal: &crate::subagent::WorkTerminal,
    resumable: bool,
    session_file: Option<&str>,
) -> bool {
    use crate::subagent::WorkTerminal;

    match terminal {
        WorkTerminal::Success { .. } => true,
        WorkTerminal::Failed { stop_reason, .. }
            if stop_reason.as_deref() == Some("interrupted") =>
        {
            resumable && session_file.is_some()
        }
        _ => false,
    }
}

/// 记录是否应保留它的锚点占用；非终态记录不参与归还。
fn record_keeps_resume_anchor(record: &crate::subagent::WorkRecord) -> bool {
    let Some(result) = record.terminal_result() else {
        return true;
    };
    terminal_keeps_resume_anchor(
        &result.terminal,
        record.request.resumable,
        record.session_file(),
    )
}

/// 把失败落定时的 store 错误并入返回给宿主的原始错误，而不是静默丢弃。
fn attach_work_store_error(
    error: crate::subagent::SubagentError,
    store_error: Option<crate::subagent::WorkStoreError>,
) -> crate::subagent::SubagentError {
    match store_error {
        Some(store_error) => crate::subagent::SubagentError::Agent {
            message: format!("{error}; work store error: {store_error}"),
        },
        None => error,
    }
}

/// 超时只在没有外部取消时改写 Cancelled；外部取消优先。
fn apply_work_timeout(
    terminal: crate::subagent::WorkTerminal,
    timed_out: bool,
    parent_cancelled: bool,
) -> crate::subagent::WorkTerminal {
    if timed_out
        && !parent_cancelled
        && matches!(terminal, crate::subagent::WorkTerminal::Cancelled { .. })
    {
        crate::subagent::WorkTerminal::TimedOut {
            reason: Some("work timeout".to_string()),
        }
    } else {
        terminal
    }
}

/// `execute_bash` 的 `bash_abort` 条目守卫：future 在任意 await 点被取消/丢弃时
/// 也要注销，否则 `is_bash_running()` 永远为真、`abort_bash` 一直取消死 token。
struct BashAbortRegistration {
    core: Arc<Core>,
    bash_id: u64,
}

impl Drop for BashAbortRegistration {
    fn drop(&mut self) {
        lock(&self.core.bash_abort).retain(|(id, _)| *id != self.bash_id);
    }
}

impl AgentSession {
    /// `new AgentSession(config)`。
    pub fn new(config: AgentSessionConfig) -> Self {
        // 在宿主线程的 `LocalSet` 上下文里安装扩展派发桥（ARCHITECTURE §4.2）。构造期
        // 安装失败（调用方没有 `LocalSet`）时桥退化为同步轮询。
        let _ = bridge_local();
        let resource_loader = Arc::clone(&config.resource_loader);
        let resource_loader_id = register_loader(&resource_loader);
        let core = Arc::new(Core::new(config, resource_loader_id));
        core.install_agent_hooks();
        core.initialize_memory_runtime();
        // 子代理工作区回收统一延迟到 `recover_work` 之后由 sdk 带着完整 keep-set
        // 扫描（`AgentSession::sweep_subagent_workspaces`）。没有 Work 账本时不能用
        // 空 keep-set 扫描共享目录：别的会话里崩溃遗留的可恢复 worktree 会被误删。
        // 恢复的 canonical 修复（InterruptedTurn 的合成结果）写进了 session manager；
        // Agent 的历史是构造前由 SDK 从旧投影装入的，这里重建一次消息视图。其余分类
        // 只修请求视图（孤儿 tool result 从本次请求移除，但会话文件保留）。
        if core.recovery_needs_refresh() {
            core.refresh_finalized_context_with_recovery();
        }
        let session = AgentSession {
            core: Arc::clone(&core),
            resource_loader_id,
        };
        if let Some(warmer) = &core.cache_warmer {
            let weak = Arc::downgrade(&core);
            warmer.set_on_warmed(Some(Arc::new(move |entry| {
                if let Some(core) = weak.upgrade() {
                    core.emit(AgentSessionEvent::EntryAppended {
                        entry: SessionEntry::Usage(entry),
                    });
                }
            })));
        }
        session.refresh_command_sources();
        session.build_runtime(core.initial_active_tool_names.clone(), None, true);
        if core.uses_default_tools {
            session.restore_tools_from_transcript(&core.default_tool_modifiers);
        }
        session
    }

    fn loader(&self) -> Arc<dyn ResourceLoader> {
        lookup_loader(self.resource_loader_id).expect("resource loader is registered")
    }

    /// 采集资源加载器里扩展命令/prompt/技能所需的数据（转调 [`Core`]）。
    fn refresh_command_sources(&self) {
        self.core.refresh_command_sources_from_loader();
    }

    /// `_buildRuntime`：组装一轮扩展运行时 —— 重建内建（或 override）工具定义，用快照
    /// 创建 [`ExtensionRunner`]，应用绑定时已保存的 UI 上下文/模式/错误监听，绑定会话
    /// 动作，最后以初始激活名单刷新工具注册表。`reload` 也会走这里（复用旧 flag 值）。
    fn build_runtime(
        &self,
        active_tool_names: Option<Vec<String>>,
        flag_values: Option<IndexMap<String, kiana_ext_api::types::FlagValue>>,
        include_all_extension_tools: bool,
    ) {
        // 资源 reload 后强制重新读取技能正文（`refresh_command_sources` 的按需缓存
        // 只比较路径/名字，正文变更需要显式刷新）。
        self.core
            .sync_skill_definitions(&self.loader().get_skills().skills, true);
        let base_tool_definitions: Vec<(String, ToolDefinition)> =
            match &self.core.base_tools_override {
                Some(overrides) => overrides
                    .iter()
                    .map(|(name, tool)| {
                        (
                            name.clone(),
                            create_tool_definition_from_agent_tool(tool.clone()),
                        )
                    })
                    .collect(),
                // 会话级读记录表与执行策略在这里注入：read/write/edit 共用同一份
                // `ReadStateRegistry`，write/edit/bash 共用同一份 `ExecutionPolicy`；
                // 权限模式/addDirs 变化后由 `Core::rebuild_builtin_tool_definitions`
                // 走同一条构造路径。
                None => self
                    .core
                    .builtin_tool_definitions()
                    .into_iter()
                    .map(|(name, definition)| (name.as_str().to_string(), definition))
                    .collect(),
            };
        {
            let mut definitions = lock(&self.core.base_tool_definitions);
            definitions.clear();
            for (name, definition) in base_tool_definitions {
                definitions.insert(name, definition);
            }
        }

        let extensions = self.loader().get_extensions();
        let model_registry = Arc::new(ModelRegistry::new(Arc::new(
            self.core.model_runtime.clone(),
        )));
        let runner = Arc::new(ExtensionRunner::new(
            extensions.extensions,
            extensions.runtime,
            std::path::PathBuf::from(&self.core.cwd),
            self.core.snapshot.clone(),
            model_registry,
        ));
        if let Some(flag_values) = flag_values {
            for (name, value) in flag_values {
                runner.set_flag_value(&name, value);
            }
        }
        *lock(&self.core.extension_runner) = Some(Arc::clone(&runner));
        if let Some(runner_ref) = &self.core.extension_runner_ref {
            *lock(runner_ref) = Some(Arc::clone(&runner));
        }
        // 扩展直接执行工具（`ctx.tools[i].execute` / `create*Tool().execute`）绕过
        // `before_tool_call`；把权限检查装到扩展 JS 所在的线程上。闭包只持有 `Weak`，
        // 会话销毁后自然失效（此时扩展也不应再执行）。
        let permission_core = Arc::downgrade(&self.core);
        kiana_extensions::runner::set_tool_permission_check(Some(Rc::new(move |name, args| {
            let core = permission_core.clone();
            Box::pin(async move {
                match core.upgrade() {
                    Some(core) => core.tool_permission_refusal(name, args).await,
                    None => Some("permission context is no longer available".to_string()),
                }
            })
        })));
        self.bind_extension_core(&runner);
        self.apply_extension_bindings(&runner);

        let default_active_tool_names: Vec<String> = match &self.core.base_tools_override {
            Some(overrides) => overrides.keys().cloned().collect(),
            None => kiana_config::settings::tools::DEFAULT_TOOL_NAMES
                .iter()
                .map(|name| name.to_string())
                .collect(),
        };
        let base_active = active_tool_names.unwrap_or(default_active_tool_names);
        self.core
            .refresh_tool_registry(Some(base_active), include_all_extension_tools);
    }

    fn apply_extension_bindings(&self, runner: &Arc<ExtensionRunner>) {
        runner.set_ui_context(
            lock(&self.core.extension_ui_context).clone(),
            *lock(&self.core.extension_mode),
        );
        runner.bind_command_context(lock(&self.core.extension_command_context_actions).clone());
        if let Some(unsubscribe) = lock(&self.core.extension_error_unsubscriber).take() {
            unsubscribe();
        }
        if let Some(listener) = lock(&self.core.extension_error_listener).clone() {
            let unsubscribe = runner.on_error(listener);
            *lock(&self.core.extension_error_unsubscriber) = Some(unsubscribe);
        }
    }

    /// `_bindExtensionCore`：把会话的各个动作（`ExtensionActions` /
    /// `ExtensionContextActions` / `ProviderActions`）绑定到 runner。所有动作闭包只捕获
    /// 注册 id，运行时经 `lookup_core` 升级；会话已释放时静默返回缺省值。与 TS 的语义
    /// 差异点都以行内注释标出（错误通道缺失的动作记录到 `persistence_error`，注册
    /// provider 失败用 panic 复刻同步抛出）。
    fn bind_extension_core(&self, runner: &Arc<ExtensionRunner>) {
        let session_id = self.core.registry_id.load(Ordering::SeqCst) as usize;

        // -- ExtensionActions --
        let send_id = session_id;
        let send_message: kiana_ext_api::types::SendMessageHandler = Arc::new(move |options| {
            let Some(core) = lookup_core(send_id) else {
                return;
            };
            let message = CustomMessage {
                role: kiana_session_core::messages::CustomRole::Custom,
                custom_type: options.message.custom_type,
                content: serde_json::to_value(&options.message.content)
                    .unwrap_or(serde_json::Value::Null),
                display: options.message.display,
                details: options.message.details,
                timestamp: now_ms() as f64,
            };
            // TS: `this.sendCustomMessage(...).catch(err => runner.emitError(...))`：不阻塞
            // 调用方，但失败必须走扩展错误通道。
            let error_runner = core.runner();
            spawn_local_dispatch(async move {
                if let Err(error) = core
                    .send_custom_message(message, options.trigger_turn, options.deliver_as)
                    .await
                    && let Some(runner) = error_runner
                {
                    runner.emit_error(ExtensionError {
                        extension_path: "<runtime>".to_string(),
                        event: "send_message".to_string(),
                        error: error.to_string(),
                        stack: None,
                    });
                }
            });
        });
        let send_user_id = session_id;
        let send_user_message: kiana_ext_api::types::SendUserMessageHandler =
            Arc::new(move |content, options| {
                let Some(core) = lookup_core(send_user_id) else {
                    return;
                };
                let error_runner = core.runner();
                spawn_local_dispatch(async move {
                    if let Err(error) = core.send_user_message(content, options).await
                        && let Some(runner) = error_runner
                    {
                        runner.emit_error(ExtensionError {
                            extension_path: "<runtime>".to_string(),
                            event: "send_user_message".to_string(),
                            error: error.to_string(),
                            stack: None,
                        });
                    }
                });
            });
        let append_id = session_id;
        let append_entry: kiana_ext_api::types::AppendEntryHandler =
            Arc::new(move |custom_type, data| {
                let Some(core) = lookup_core(append_id) else {
                    return;
                };
                // plan-mode 扩展通过自定义条目持久化状态；同步进会话级 plan 状态，
                // pruning 保护与压缩恢复都读取它。
                core.observe_plan_custom_entry(&custom_type, data.as_ref());
                let entry_id = core
                    .with_session_mut(|manager| manager.append_custom_entry(&custom_type, data));
                match entry_id {
                    Ok(entry_id) => {
                        if let Some(entry) =
                            core.with_session(|manager| manager.get_entry(&entry_id).cloned())
                        {
                            core.emit(AgentSessionEvent::EntryAppended { entry });
                        }
                    }
                    // TS 的 `appendEntry` 同步抛出到扩展；动作签名返回 ()（冻结契约），
                    // 记录后由下一次 run 结束点暴露。
                    Err(error) => core.record_persistence_error(error),
                }
            });
        let name_id = session_id;
        let set_session_name: kiana_ext_api::types::SetSessionNameHandler = Arc::new(move |name| {
            if let Some(core) = lookup_core(name_id) {
                core.set_session_name_inner(&name);
            }
        });
        let get_name_id = session_id;
        let get_session_name: kiana_ext_api::types::GetSessionNameHandler =
            Arc::new(move || lookup_core(get_name_id).and_then(|core| core.session_name()));
        let label_id = session_id;
        let set_label: kiana_ext_api::types::SetLabelHandler = Arc::new(move |entry_id, label| {
            let Some(core) = lookup_core(label_id) else {
                return;
            };
            if let Err(error) = core.with_session_mut(|manager| {
                manager.append_label_change(&entry_id, label.as_deref())
            }) {
                core.record_persistence_error(error);
            }
        });
        let tools_id = session_id;
        let get_active_tools: kiana_ext_api::types::GetActiveToolsHandler = Arc::new(move || {
            lookup_core(tools_id)
                .map(|core| core.get_active_tool_names())
                .unwrap_or_default()
        });
        let all_tools_id = session_id;
        let get_all_tools: kiana_ext_api::types::GetAllToolsHandler = Arc::new(move || {
            lookup_core(all_tools_id)
                .map(|core| core.get_all_tools())
                .unwrap_or_default()
        });
        let settings_id = session_id;
        let get_settings: kiana_ext_api::types::GetSettingsHandler = Arc::new(move || {
            lookup_core(settings_id)
                .map(|core| core.settings_manager.get_settings())
                .unwrap_or_default()
        });
        let set_active_id = session_id;
        let set_active_tools: kiana_ext_api::types::SetActiveToolsHandler =
            Arc::new(move |names| {
                if let Some(core) = lookup_core(set_active_id) {
                    core.set_active_tools_by_name(&names);
                }
            });
        let refresh_id = session_id;
        let refresh_tools: kiana_ext_api::types::RefreshToolsHandler = Arc::new(move || {
            if let Some(core) = lookup_core(refresh_id) {
                core.pending_tool_refresh.store(true, Ordering::SeqCst);
            }
        });
        let commands_id = session_id;
        let runner_commands = Arc::clone(runner);
        let get_commands: kiana_ext_api::types::GetCommandsHandler = Arc::new(move || {
            let Some(core) = lookup_core(commands_id) else {
                return Vec::new();
            };
            let mut commands: Vec<SlashCommandInfo> = runner_commands
                .get_registered_commands()
                .into_iter()
                .map(|command| SlashCommandInfo {
                    name: command.invocation_name,
                    description: command.description,
                    source: SlashCommandSource::Extension,
                    source_info: command.source_info,
                })
                .collect();
            for template in core.prompt_templates_snapshot() {
                commands.push(SlashCommandInfo {
                    name: template.name,
                    description: Some(template.description),
                    source: SlashCommandSource::Prompt,
                    source_info: template.source_info,
                });
            }
            // TS 的 `getCommands` 每次读 live loader 的技能列表。
            core.refresh_command_sources_from_loader();
            for skill in lock(&core.commands).skills.iter() {
                commands.push(SlashCommandInfo {
                    name: format!("skill:{}", skill.name),
                    description: Some(skill.description.clone()),
                    source: SlashCommandSource::Skill,
                    source_info: skill.source_info.clone(),
                });
            }
            commands
        });
        let set_model_id = session_id;
        let set_model: kiana_ext_api::types::SetModelHandler = Arc::new(move |model| {
            let core = lookup_core(set_model_id);
            Box::pin(async move {
                let Some(core) = core else {
                    return false;
                };
                if !core.model_runtime.has_configured_auth(&model.provider) {
                    return false;
                }
                core.set_model_impl(model, ModelMutationOptions::default())
                    .await
                    .is_ok()
            })
        });
        let get_thinking_id = session_id;
        let get_thinking_level: kiana_ext_api::types::GetThinkingLevelHandler =
            Arc::new(move || {
                lookup_core(get_thinking_id)
                    .map(|core| core.agent.thinking_level())
                    .unwrap_or(ModelThinkingLevel::Off)
            });
        let set_thinking_id = session_id;
        let set_thinking_level: kiana_ext_api::types::SetThinkingLevelHandler =
            Arc::new(move |level| {
                if let Some(core) = lookup_core(set_thinking_id) {
                    // TS 的 setThinkingLevel 在 appendThinkingLevelChange 失败时同步抛出；
                    // `kiana-ext-api` 的这个动作签名没有错误通道（冻结契约），因此只能忽略。
                    let _ = core.set_thinking_level_impl(level, ModelMutationOptions::default());
                }
            });

        let actions = kiana_ext_api::types::ExtensionActions {
            send_message,
            send_user_message,
            append_entry,
            set_session_name,
            get_session_name,
            set_label,
            get_active_tools,
            get_all_tools,
            get_settings,
            set_active_tools,
            refresh_tools,
            get_commands,
            set_model,
            get_thinking_level,
            set_thinking_level,
        };

        // -- ExtensionContextActions --
        let get_model_id = session_id;
        let context_get_model: Arc<dyn Fn() -> Option<Model> + Send + Sync> =
            Arc::new(move || lookup_core(get_model_id).and_then(|core| core.current_model()));
        let scoped_id = session_id;
        let get_scoped_models: Arc<
            dyn Fn() -> Vec<kiana_config::models::resolver::ScopedModel> + Send + Sync,
        > = Arc::new(move || {
            lookup_core(scoped_id)
                .map(|core| {
                    lock(&core.scoped_models)
                        .iter()
                        .map(|entry| kiana_config::models::resolver::ScopedModel {
                            model: entry.model.clone(),
                            thinking_level: entry.thinking_level,
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
        let idle_id = session_id;
        let is_idle: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            lookup_core(idle_id)
                .map(|core| core.is_idle())
                .unwrap_or(true)
        });
        let trust_id = session_id;
        let is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            lookup_core(trust_id)
                .map(|core| core.settings_manager.is_project_trusted())
                .unwrap_or(false)
        });
        let signal_id = session_id;
        let get_signal: Arc<dyn Fn() -> Option<CancellationToken> + Send + Sync> =
            Arc::new(move || lookup_core(signal_id).and_then(|core| core.agent.signal()));
        let abort_id = session_id;
        let abort: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Some(core) = lookup_core(abort_id) else {
                return;
            };
            // TS：绑定 abortHandler 时由它接管（交互模式会先恢复队列到编辑器）；否则 abort()。
            if let Some(handler) = lock(&core.extension_abort_handler).clone() {
                handler();
                return;
            }
            core.request_abort();
        });
        let pending_id = session_id;
        let has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            lookup_core(pending_id)
                .map(|core| {
                    lock(&core.steering_messages).len() + lock(&core.follow_up_messages).len() > 0
                })
                .unwrap_or(false)
        });
        let shutdown_id = session_id;
        let shutdown: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(core) = lookup_core(shutdown_id)
                && let Some(handler) = lock(&core.extension_shutdown_handler).clone()
            {
                handler();
            }
        });
        let usage_id = session_id;
        let get_context_usage: Arc<
            dyn Fn() -> Option<kiana_ext_api::types::ContextUsage> + Send + Sync,
        > = Arc::new(move || lookup_core(usage_id).and_then(|core| core.get_context_usage_inner()));
        let compact_id = session_id;
        let compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync> = Arc::new(move |options| {
            let Some(core) = lookup_core(compact_id) else {
                return;
            };
            let options = options.unwrap_or_default();
            // TS: `void this.compact(...).then(onComplete, onError)`。派发到常驻派发表，
            // 这样即使调用点在另一次扩展派发内部也不会被丢弃。
            spawn_local_dispatch(async move {
                match core.compact(options.custom_instructions, None).await {
                    Ok(result) => {
                        if let Some(on_complete) = options.on_complete {
                            on_complete(result);
                        }
                    }
                    Err(error) => {
                        if let Some(on_error) = options.on_error {
                            on_error(Box::new(error));
                        }
                    }
                }
            });
        });
        let prompt_id = session_id;
        let get_system_prompt: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || {
            lookup_core(prompt_id)
                .map(|core| core.system_prompt_inner())
                .unwrap_or_default()
        });
        let system_options_id = session_id;
        let get_system_prompt_options: Option<GetSystemPromptOptionsAction> =
            Some(Arc::new(move || {
                // TS: `getSystemPromptOptions: () => this._baseSystemPromptOptions` —— 始终是
                // 会话的基础选项对象，即使当前有 run 进行中（不使用 `_runSystemPromptOptions`）。
                lookup_core(system_options_id)
                    .and_then(|core| lock(&core.base_system_prompt_options).clone())
                    .map(|options| normalized_to_build_options(&options))
                    .unwrap_or_else(|| BuildSystemPromptOptions {
                        cwd: lookup_core(system_options_id)
                            .map(|core| core.cwd.clone())
                            .unwrap_or_default(),
                        ..Default::default()
                    })
            }));
        let execute_id = session_id;
        let execute_tool: Option<ExecuteToolAction> =
            Some(Arc::new(move |caller_id, name, args, options| {
                Box::pin(async move {
                    dispatch_local(async move {
                        match lookup_core(execute_id) {
                            Some(core) => {
                                core.execute_nested_tool_call(caller_id, name, args, options)
                                    .await
                            }
                            None => AgentToolCallOutcome {
                                tool_call: AgentToolCall {
                                    kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                                    id: caller_id,
                                    name,
                                    arguments: Default::default(),
                                    thought_signature: None,
                                    namespace: None,
                                    arguments_error: None,
                                },
                                result: kiana_agent_core::AgentToolResult {
                                    content: vec![text_content("Session is disposed")],
                                    details: serde_json::Value::Null,
                                    structured_content: None,
                                    usage: None,
                                    is_error: None,
                                    terminate: None,
                                },
                                is_error: true,
                                duration_ms: None,
                            },
                        }
                    })
                    .await
                    .unwrap_or_else(|| AgentToolCallOutcome {
                        tool_call: AgentToolCall {
                            kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                            id: String::new(),
                            name: String::new(),
                            arguments: Default::default(),
                            thought_signature: None,
                            namespace: None,
                            arguments_error: None,
                        },
                        result: kiana_agent_core::AgentToolResult {
                            content: vec![text_content("Session is disposed")],
                            details: serde_json::Value::Null,
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                        },
                        is_error: true,
                        duration_ms: None,
                    })
                })
            }));
        let callable_id = session_id;
        let get_callable_tools: Option<GetCallableToolsAction> = Some(Arc::new(move || {
            lookup_core(callable_id)
                .map(|core| core.get_callable_tools())
                .unwrap_or_default()
        }));

        let context_actions = kiana_ext_api::types::ExtensionContextActions {
            get_model: context_get_model,
            get_scoped_models,
            is_idle,
            is_project_trusted,
            get_signal,
            abort,
            has_pending_messages,
            shutdown,
            get_context_usage,
            compact,
            get_system_prompt,
            get_system_prompt_options,
            execute_tool,
            get_callable_tools,
        };

        // -- ProviderActions --
        let register_id = session_id;
        let register_provider: Option<
            Arc<dyn Fn(String, kiana_ext_api::types::ProviderConfig) + Send + Sync>,
        > = Some(Arc::new(move |name, config| {
            if let Some(core) = lookup_core(register_id) {
                match core
                    .model_runtime
                    .register_provider(&name, provider_config_to_input(config))
                {
                    Ok(()) => core.refresh_current_model_from_registry(),
                    // TS: `this._modelRuntime.registerProvider(...)` 同步抛出到扩展的
                    // `kiana.registerProvider(...)` 调用点。注入的 Rust 闭包用 panic 形态
                    // “抛出”，由 kiana-extensions runner 的 `catch_item` 转成 JS 异常。
                    Err(error) => throw_provider_error(error),
                }
            }
        }));
        let register_native_id = session_id;
        let register_native_provider: Option<RegisterNativeProviderAction> =
            Some(Arc::new(move |provider| {
                if let Some(core) = lookup_core(register_native_id) {
                    match core.model_runtime.register_native_provider(provider) {
                        Ok(()) => core.refresh_current_model_from_registry(),
                        Err(error) => throw_provider_error(error),
                    }
                }
            }));
        let unregister_id = session_id;
        let unregister_provider: Option<Arc<dyn Fn(String) + Send + Sync>> =
            Some(Arc::new(move |name| {
                if let Some(core) = lookup_core(unregister_id) {
                    core.model_runtime.unregister_provider(&name);
                    core.refresh_current_model_from_registry();
                }
            }));
        let register_virtual_id = session_id;
        let register_virtual_model: Option<
            Arc<dyn Fn(kiana_session_core::virtual_models::VirtualModelDefinition) + Send + Sync>,
        > = Some(Arc::new(move |definition| {
            if let Some(core) = lookup_core(register_virtual_id) {
                match core.model_runtime.register_virtual_model(definition) {
                    Ok(()) => core.refresh_current_model_from_registry(),
                    Err(error) => throw_provider_error(error),
                }
            }
        }));
        let unregister_virtual_id = session_id;
        let unregister_virtual_model: Option<Arc<dyn Fn(String, String) + Send + Sync>> =
            Some(Arc::new(move |provider, id| {
                if let Some(core) = lookup_core(unregister_virtual_id) {
                    core.model_runtime.unregister_virtual_model(&provider, &id);
                    core.refresh_current_model_from_registry();
                }
            }));
        let provider_actions = kiana_extensions::runner::ProviderActions {
            register_provider,
            register_native_provider,
            unregister_provider,
            register_virtual_model,
            unregister_virtual_model,
        };

        runner.bind_core(actions, context_actions, Some(provider_actions));
    }

    /// `_restoreToolsFromTranscript`：会话从持久化恢复时重建激活工具集 —— 收集全部
    /// system 消息提到过的工具名，从注册表补上「注册即激活」且未被提到的新工具，再按
    /// modifiers 与 allowlist 过滤后激活。
    fn restore_tools_from_transcript(&self, modifiers: &[String]) {
        lock(&self.core.pending_tool_names).clear();
        let system_messages: Vec<AgentMessage> = self
            .core
            .branch()
            .into_iter()
            .filter_map(|entry| match entry {
                SessionEntry::Message(entry) if entry.message.role() == "system" => {
                    Some(entry.message)
                }
                _ => None,
            })
            .collect();
        if system_messages.is_empty() {
            return;
        }
        let mut mentioned: HashSet<String> = HashSet::new();
        for message in &system_messages {
            if let AgentMessage::Standard(Message::System(system)) = message {
                if let Some(tools) = &system.tools_added {
                    mentioned.extend(tools.iter().map(|tool| tool.name.clone()));
                }
                if let Some(tools) = &system.tools_removed {
                    mentioned.extend(tools.iter().map(|tool| tool.name.clone()));
                }
            }
        }
        let new_tools: Vec<String> = lock(&self.core.tool_registry)
            .keys()
            .filter(|name| {
                !mentioned.contains(*name)
                    && !lock(&self.core.base_tool_definitions).contains_key(*name)
                    && self.core.is_activated_on_registration(name)
            })
            .cloned()
            .collect();
        let llm: Vec<Message> = convert_to_llm(&system_messages);
        let mut base: Vec<String> = get_current_tools(&llm)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        base.extend(new_tools);
        let names = kiana_config::settings::tools::apply_tool_modifiers(&base, modifiers);
        *lock(&self.core.pending_tool_names) = names
            .iter()
            .filter(|name| self.core.is_allowed_tool(name))
            .cloned()
            .collect();
        self.core.set_active_tools(&names);
    }

    // -- 公开访问器 ---------------------------------------------------------

    /// `modelRuntime`。
    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.core.model_runtime
    }

    /// TS 的公开 `agent` 字段（`session.agent`）。
    ///
    /// `modes/rpc/rpc-mode.ts` 与 `modes/print-mode.ts` 额外订阅 `session.agent`，在每个
    /// agent 事件后 `await waitForRawStdoutBackpressure()`（stdout 背压）；没有这个访问器
    /// 时 `kiana-rpc-print` 无法复刻该订阅。
    pub fn agent(&self) -> Agent {
        self.core.agent.clone()
    }

    /// TS 的公开 `settingsManager` 字段（交互模式的 `/bug`、`/share` 等读取设置）。
    pub fn settings_manager(&self) -> SettingsManager {
        self.core.settings_manager.clone()
    }

    /// `subscribe(listener)`。
    pub fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        let id = self.core.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.core.event_listeners).push((id, Arc::clone(&listener)));
        let listeners = Arc::downgrade(&self.core);
        Box::new(move || {
            if let Some(core) = listeners.upgrade() {
                let mut entries = lock(&core.event_listeners);
                if let Some(index) = entries.iter().position(|(candidate_id, candidate)| {
                    *candidate_id == id && Arc::ptr_eq(candidate, &listener)
                }) {
                    entries.remove(index);
                }
            }
        })
    }

    /// `dispose()`。
    /// 第 9 周：把内存遥测交给 OTLP exporter；没有 exporter 时 no-op。
    ///
    /// 应在 tokio runtime 退出前调用（`AgentSessionRuntime::dispose` 已接线）。
    pub async fn flush_telemetry(&self) {
        let _ = self.core.telemetry.flush().await;
    }

    /// 当前（或最近一次）run 的汇总；TUI 状态栏/评测使用。
    pub fn run_summary(&self) -> Option<kiana_agent_core::AgentRunSummary> {
        lock(&self.core.run_collector)
            .as_ref()
            .map(|collector| collector.summary())
    }

    /// 当前（或最近一次）run 的覆盖率（注册工具 vs 实际调用）。
    pub fn run_coverage(&self) -> Option<kiana_agent_core::AgentRunCoverage> {
        lock(&self.core.run_collector)
            .as_ref()
            .map(|collector| collector.coverage())
    }

    pub fn dispose(&self) {
        // 技能激活只在会话内有效；销毁时全部撤销（`allowed-tools` 不会泄漏到后续会话）。
        self.core.end_skill_session();
        self.core.abort_retry();
        self.core.abort_compaction();
        self.core.abort_branch_summary();
        for (_id, token) in lock(&self.core.bash_abort).iter() {
            token.cancel();
        }
        self.core.agent.abort();
        // 第 11 周后半：正常销毁时标记 clean shutdown（保留 claim、不消耗 attempt）。
        if let Some(continuity) = &self.core.continuity {
            continuity.shutdown();
        }
        if let Some(runner) = self.core.runner() {
            runner.invalidate(Some(
                "This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().".to_string(),
            ));
        }
        if let Some(subscription) = lock(&self.core.subscription).take() {
            subscription.unsubscribe();
        }
        lock(&self.core.event_listeners).clear();
        if let Some(warmer) = &self.core.cache_warmer {
            warmer.set_on_warmed(None);
            warmer.cancel();
        }
        let session_id = self.core.session_id();
        let _ = kiana_ai::session_resources::cleanup_session_resources(Some(session_id.as_str()));
        let registry_id = self.core.registry_id.load(Ordering::SeqCst) as usize;
        if registry_id != 0 {
            unregister_core(registry_id);
        }
        unregister_loader(self.resource_loader_id);
    }

    /// `refreshContext()`。
    pub fn refresh_context(&self) {
        self.core.refresh_finalized_context();
    }

    /// `get state()`。
    pub fn state(&self) -> kiana_agent_core::types::AgentState {
        self.core.agent.state()
    }

    /// `cacheWarmingStatus`。
    pub fn cache_warming_status(&self) -> Option<CacheWarmingStatus> {
        self.core.cache_warmer.as_ref().map(CacheWarmer::status)
    }

    /// `setCacheWarmingMode(mode)`。
    pub fn set_cache_warming_mode(&self, mode: kiana_config::settings::schema::CacheWarmingMode) {
        self.core.settings_manager.set_cache_warming_mode(mode);
        if let Some(warmer) = &self.core.cache_warmer {
            warmer.on_mode_changed();
        }
    }

    /// `get model()`。
    pub fn model(&self) -> Option<Model> {
        self.core.current_model()
    }

    /// `get thinkingLevel()`。
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.core.agent.thinking_level()
    }

    /// `get routedModel()`。
    pub fn routed_model(&self) -> Option<RoutedModel> {
        self.core.routed_model()
    }

    /// `get isStreaming()`。
    pub fn is_streaming(&self) -> bool {
        self.core.is_streaming_active()
    }

    /// `get isIdle()`。
    pub fn is_idle(&self) -> bool {
        self.core.is_idle()
    }

    /// `get systemPrompt()`。
    pub fn system_prompt(&self) -> String {
        self.core.system_prompt_inner()
    }

    /// `get retryAttempt()`。
    pub fn retry_attempt(&self) -> u64 {
        self.core.retry_attempt.load(Ordering::SeqCst)
    }

    /// `getActiveToolNames()`。
    pub fn get_active_tool_names(&self) -> Vec<String> {
        self.core.get_active_tool_names()
    }

    /// 设置 plan / plan-mode 状态（宿主或 plan-mode 扩展的显式入口）。
    ///
    /// 下一次 pruning / 压缩恢复读取新值；不写会话文件（扩展用 `appendEntry("plan-mode")`
    /// 持久化，会话启动时会自动恢复）。
    pub fn set_plan_state(&self, state: SessionPlanState) {
        *lock(&self.core.plan_state) = state;
    }

    /// 当前 plan 状态（`content` 缺失时按 `path` 读取）。
    pub fn plan_state(&self) -> SessionPlanState {
        self.core.plan_state_snapshot()
    }

    /// 激活一个技能（宿主入口，例如 cron turn 的 `skills` 列表）。
    ///
    /// 等价于用户 `/skill:<name>`：校验 `user-invocable` 并登记激活，`allowed-tools`
    /// 立即生效。没有加载技能或名字未知时返回可操作错误。
    pub fn activate_skill(&self, name: &str) -> Result<(), String> {
        let guard = lock(&self.core.skill_tool);
        let Some(tool) = guard.as_ref() else {
            return Err("no skills are loaded for this session".to_string());
        };
        tool.activate_user(name, "")
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// 撤销一个技能的激活（`allowed-tools` 立即失效，同一请求重新回到 `Ask`）。
    ///
    /// 返回是否找到并撤销。用户入口是 `/skill:deactivate <name>`；宿主也可以直接调用。
    pub fn deactivate_skill(&self, name: &str) -> bool {
        lock(&self.core.skill_tool)
            .as_ref()
            .is_some_and(|tool| tool.deactivate(name))
    }

    /// 设置技能激活 TTL（`None` 表示直到显式撤销/会话结束）。
    ///
    /// 生产配置入口是 settings 的 `skills.activationTtlSeconds`（构造/刷新时应用）；
    /// 这里供宿主运行期覆盖。
    pub fn set_skill_activation_ttl(&self, ttl: Option<std::time::Duration>) {
        if let Some(tool) = lock(&self.core.skill_tool).as_ref() {
            tool.set_ttl(ttl);
        }
    }

    /// 当前有效技能激活（诊断/UI）。
    pub fn active_skill_activations(
        &self,
    ) -> Vec<kiana_resources::skills::activation::SkillActivation> {
        lock(&self.core.skill_tool)
            .as_ref()
            .map(|tool| tool.active_activations(kiana_resources::skills::activation::now_utc()))
            .unwrap_or_default()
    }

    /// 当前 run 的 W3C `traceparent`（bash 子进程注入/诊断；没有活动 run 时为 `None`）。
    pub fn current_traceparent(&self) -> Option<String> {
        self.core
            .telemetry
            .current_trace_context()
            .map(|trace| trace.traceparent.to_header())
    }

    /// `getCallableToolNames()`。
    pub fn get_callable_tool_names(&self) -> Vec<String> {
        self.core
            .get_callable_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    /// `getAllTools()`。
    pub fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.core.get_all_tools()
    }

    /// `getToolDefinition(name)`。
    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        // TS 的 `refreshTools()` 立即重建注册表；Rust 的刷新推迟到读取点（避开扩展派发期
        // 的重入），因此这里与 `getAllTools`/`getActiveToolNames` 一样先冲刷。
        self.core.flush_pending_tool_refresh();
        lock(&self.core.tool_definitions)
            .get(name)
            .map(|entry| entry.definition.clone())
    }

    /// `setActiveToolsByName(toolNames)`。
    pub fn set_active_tools_by_name(&self, tool_names: Vec<String>) {
        self.core.set_active_tools_by_name(&tool_names);
    }

    /// `get isCompacting()`。
    pub fn is_compacting(&self) -> bool {
        self.core.is_compacting()
    }

    /// `get messages()`。
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.core.agent.messages()
    }

    /// `messages.length`（O(1)，不克隆 transcript）。
    pub fn message_count(&self) -> usize {
        self.core.agent.message_count()
    }

    /// `get steeringMode()`。
    pub fn steering_mode(&self) -> QueueMode {
        self.core.agent.steering_mode()
    }

    /// `get followUpMode()`。
    pub fn follow_up_mode(&self) -> QueueMode {
        self.core.agent.follow_up_mode()
    }

    /// `get sessionFile()`。
    pub fn session_file(&self) -> Option<String> {
        self.core.session_file()
    }

    /// `get sessionId()`。
    pub fn session_id(&self) -> String {
        self.core.session_id()
    }

    /// `get sessionName()`。
    pub fn session_name(&self) -> Option<String> {
        self.core.session_name()
    }

    /// `get scopedModels()`。
    pub fn scoped_models(&self) -> Vec<ScopedModel> {
        lock(&self.core.scoped_models).clone()
    }

    /// `setScopedModels(scopedModels)`。
    pub fn set_scoped_models(&self, scoped_models: Vec<ScopedModel>) {
        *lock(&self.core.scoped_models) = scoped_models;
    }

    /// `get promptTemplates()`。
    pub fn prompt_templates(&self) -> Vec<PromptTemplate> {
        self.core.prompt_templates_snapshot()
    }

    /// `get resourceLoader()`。
    pub fn resource_loader(&self) -> Arc<dyn ResourceLoader> {
        self.loader()
    }

    /// `get extensionRunner()`。
    pub fn extension_runner(&self) -> Arc<ExtensionRunner> {
        self.core.runner_required()
    }

    /// `get sessionManager()`（返回当前状态的快照管理器）。
    pub fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        Arc::clone(&self.core.snapshot) as Arc<dyn ReadonlySessionManager>
    }

    /// TS 的 `sessionManager` 是公开的可变字段（测试/嵌入方直接调用 `appendMessage` 等）。
    /// Rust 的变更方法需要 `&mut self`，因此用闭包暴露同一对象；快照视图在调用后刷新。
    pub fn with_session_manager_mut<R>(&self, f: impl FnOnce(&mut SessionManager) -> R) -> R {
        self.core.with_session_mut(f)
    }

    /// `newSession({ setup })` 期间把 Core 的会话读写重定向到 setup 持有的共享管理器。
    pub(crate) fn set_session_override(&self, manager: Option<Arc<Mutex<SessionManager>>>) {
        *lock(&self.core.session_override) = manager;
    }

    /// `hasExtensionHandlers(eventType)`。
    pub fn has_extension_handlers(&self, event_type: &str) -> bool {
        self.core
            .runner()
            .is_some_and(|runner| runner.has_handlers(event_type))
    }

    // -- 队列 ---------------------------------------------------------------

    /// `steer(text, images?, options?)`。
    pub async fn steer(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        self.core
            .queue_user_input(
                text,
                images,
                DeliverMode::Steer,
                source.unwrap_or(InputSource::Interactive),
            )
            .await
    }

    /// `followUp(text, images?, options?)`。
    pub async fn follow_up(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        self.core
            .queue_user_input(
                text,
                images,
                DeliverMode::FollowUp,
                source.unwrap_or(InputSource::Interactive),
            )
            .await
    }

    /// `sendCustomMessage(message, options?)`。
    pub async fn send_custom_message(
        &self,
        message: CustomMessage,
        trigger_turn: Option<bool>,
        deliver_as: Option<DeliverAs>,
    ) -> Result<(), AgentSessionError> {
        self.core
            .send_custom_message(message, trigger_turn, deliver_as)
            .await
    }

    /// `sendUserMessage(content, options?)`。
    pub async fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> Result<(), AgentSessionError> {
        self.core.send_user_message(content, options).await
    }

    /// `clearQueue()`。
    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        self.core.clear_queue()
    }

    /// `get pendingMessageCount()`。
    pub fn pending_message_count(&self) -> usize {
        lock(&self.core.steering_messages).len() + lock(&self.core.follow_up_messages).len()
    }

    /// `getSteeringMessages()`。
    pub fn get_steering_messages(&self) -> Vec<String> {
        lock(&self.core.steering_messages).clone()
    }

    /// `getFollowUpMessages()`。
    pub fn get_follow_up_messages(&self) -> Vec<String> {
        lock(&self.core.follow_up_messages).clone()
    }

    /// `prompt(text, options?)`。
    pub async fn prompt(
        &self,
        text: String,
        options: PromptOptions,
    ) -> Result<(), AgentSessionError> {
        self.core.prompt(text, options).await
    }

    // -- 第 11 周后半：恢复 / 连续性 ----------------------------------------

    /// 会话打开时的恢复分类（`None` 表示没有可用的分类结果）。
    pub fn recovery_classification(&self) -> Option<RecoveryClass> {
        lock(&self.core.initial_recovery)
            .as_ref()
            .map(|plan| plan.classification.class)
    }

    /// 会话打开时的完整恢复计划（只读快照）。
    pub fn recovery_plan(&self) -> Option<RecoveryPlan> {
        lock(&self.core.initial_recovery).clone()
    }

    /// 取走恢复 continuation（`InterruptedPrompt` 的尾部 user 重放 / `InterruptedTurn`
    /// 的合成结果）。
    ///
    /// `InterruptedTurn` 的合成结果已经写进 canonical transcript；`InterruptedPrompt`
    /// 的 `ReplayUser` 由宿主负责用 `entry_id` 去重后发送一次。本方法只交付一次。
    pub fn take_recovery_continuation(&self) -> Option<RecoveryContinuation> {
        lock(&self.core.pending_recovery).take()
    }

    /// 消费恢复 continuation 并在会话内执行一次（SDK/CLI 启动路径调用）。
    ///
    /// - `InterruptedPrompt::ReplayUser`：按 `entry_id` 去重（canonical 里的
    ///   `kiana.recovery-replay` 标记），用原 user 消息启动一次 run，不新增 user entry；
    /// - `InterruptedTurn::SyntheticToolResults`：合成结果已经由构造期修复写进
    ///   canonical，下一次正常 prompt 会带上它们，因此这里返回 `Ok(false)`（不额外发请求）。
    ///
    /// 返回 `true` 表示确实重放了一次。continuation 只交付一次：调用后
    /// [`Self::take_recovery_continuation`] 为 `None`。
    pub async fn resume_recovery_continuation(&self) -> Result<bool, AgentSessionError> {
        let Some(continuation) = self.take_recovery_continuation() else {
            return Ok(false);
        };
        match continuation {
            RecoveryContinuation::ReplayUser { entry_id, message } => {
                if self.core.recovery_replay_recorded(&entry_id) {
                    return Ok(false);
                }
                // 先持久化标记再发请求：即使进程在响应前被杀，重启也不会再次重放
                // （at-most-once；标记本身不进入模型上下文）。
                self.core.record_recovery_replay(&entry_id)?;
                self.core.start_recovery_replay(message, entry_id).await?;
                Ok(true)
            }
            RecoveryContinuation::SyntheticToolResults { .. } => Ok(false),
        }
    }

    /// 连续性接线是否启用。
    pub fn continuity_enabled(&self) -> bool {
        self.core.continuity.is_some()
    }

    /// 连续性接线的诊断（执行所有权拒绝、checkpoint/ledger 失败）。
    pub fn continuity_diagnostics(&self) -> Vec<String> {
        self.core
            .continuity
            .as_ref()
            .map(|continuity| {
                let mut diagnostics = self.core.config_diagnostics.clone();
                diagnostics.extend(continuity.diagnostics());
                diagnostics
            })
            .unwrap_or_default()
    }

    /// 当前采集的验收执行记录。
    pub fn acceptance_records(&self) -> Vec<crate::acceptance::ExecutionRecord> {
        self.core
            .continuity
            .as_ref()
            .map(|continuity| continuity.execution_records())
            .unwrap_or_default()
    }

    /// 用当前采集的执行记录判定 acceptance criteria。
    ///
    /// 未启用连续性接线时返回一个空上下文：所有 criteria 都是 `Unverified`（绝不静默
    /// 通过）。
    pub fn evaluate_acceptance(&self, criteria: &[String]) -> crate::acceptance::AcceptanceReport {
        let workdir = PathBuf::from(&self.core.cwd);
        match &self.core.continuity {
            Some(continuity) => continuity.evaluate(criteria, &workdir),
            None => crate::acceptance::evaluate_acceptance(
                criteria,
                &crate::acceptance::AcceptanceContext::new(&workdir, &[]),
            ),
        }
    }

    /// checkpoint manager（restore / prune 入口）；连续性关闭或 checkpoint 未启用时
    /// 返回 `None`。
    pub fn checkpoint_manager(&self) -> Option<&crate::checkpoint::CheckpointManager> {
        self.core
            .continuity
            .as_ref()
            .and_then(|continuity| continuity.checkpoint_manager())
    }

    /// 恢复最新 checkpoint。只还原“当前内容 == agent 最后一次写入”的文件；用户手改
    /// 过的文件进入 `skipped_user_edits`（调用方必须展示）。
    pub fn restore_checkpoint(
        &self,
    ) -> Result<Option<crate::checkpoint::RestoreReport>, crate::checkpoint::CheckpointError> {
        let Some(continuity) = &self.core.continuity else {
            return Ok(None);
        };
        let Some(manager) = continuity.checkpoint_manager() else {
            return Ok(None);
        };
        let workdir = Path::new(&self.core.cwd);
        let ledger = manager.ledger(workdir)?;
        manager.restore(workdir, None, &ledger).map(Some)
    }

    /// 后台任务管理器；连续性关闭或后台任务未启用时返回 `None`。
    pub fn background_task_manager(
        &self,
    ) -> Option<&Arc<crate::background::BackgroundTaskManager>> {
        self.core
            .continuity
            .as_ref()
            .and_then(|continuity| continuity.background_manager())
    }

    /// 启动一次后台任务；立即返回 task id，任务在 manager 拥有的 tokio task 中继续。
    ///
    /// 工作闭包必须是 `Send + 'static`（tokio::spawn 的约束）；会话本身是 `!Send`，
    /// 因此这个入口面向外部命令/网络工作，而不是递归驱动同一个 AgentSession。
    pub async fn start_background_task<F, Fut>(
        &self,
        spec: crate::background::BackgroundTaskSpec,
        work: F,
    ) -> Result<crate::background::BackgroundTaskHandle, crate::background::BackgroundTaskError>
    where
        F: FnOnce(crate::background::BackgroundTaskContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = crate::background::TaskOutcome> + Send + 'static,
    {
        let Some(manager) = self.background_task_manager() else {
            return Err(crate::background::BackgroundTaskError::NoRuntime);
        };
        manager.start(spec, work).await
    }

    /// 当前待投递的后台任务 terminal 通知数量（下一轮 run 开始时自动注入）。
    pub fn pending_background_notifications(&self) -> usize {
        self.core
            .continuity
            .as_ref()
            .map(|continuity| continuity.pending_background_notifications())
            .unwrap_or(0)
    }

    /// 启动时 sweep 后台任务（恢复或 terminalize 崩溃遗留记录）。
    pub async fn sweep_background_tasks(
        &self,
    ) -> Result<Option<crate::background::SweepReport>, crate::background::BackgroundTaskError>
    {
        match self.background_task_manager() {
            Some(manager) => manager
                .sweep_for_session(Some(self.session_id().as_str()))
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    /// 执行所有权拒绝原因（另一个存活进程持有本会话的执行 claim）。
    ///
    /// `Some` 时本会话不能执行 run，也不应该接管 durable 目录：另一个进程可能仍在
    /// 使用这些存储，`sweep`/`recover` 连目录租约都不该拿。
    pub fn execution_refusal(&self) -> Option<&str> {
        self.core.execution_refusal.as_deref()
    }

    /// 启动阶段 `recover_work` 的结果（由 `sdk::create_agent_session` 写入）。
    ///
    /// `None` 表示没有 durable Work 账本或尚未执行恢复；`Some(Ok(report))` 是成功
    /// （空报告表示没有遗留 Work）；`Some(Err(_))` 是恢复失败。
    pub fn work_recovery(
        &self,
    ) -> Option<Result<Option<crate::subagent::WorkRecoveryReport>, String>> {
        lock(&self.core.work_recovery).clone()
    }

    /// 记录启动恢复结果（`sdk` 启动阶段使用）。
    pub(crate) fn record_work_recovery(
        &self,
        outcome: Result<Option<crate::subagent::WorkRecoveryReport>, String>,
    ) {
        *lock(&self.core.work_recovery) = Some(outcome);
    }

    /// 追加一条连续性诊断（启动 sweep 失败等），与 `continuity_diagnostics()` 同源。
    pub(crate) fn record_continuity_diagnostic(&self, message: String) {
        if let Some(continuity) = &self.core.continuity {
            continuity.push_diagnostic(message);
        }
    }

    /// 会话级 Work 账本；连续性或 `continuity.work` 关闭时返回 `None`。
    pub fn durable_work_store(&self) -> Option<Arc<crate::subagent::DurableWorkStore>> {
        let store = self
            .core
            .continuity
            .as_ref()
            .and_then(|continuity| continuity.work_store().cloned())?;
        // 编排层 telemetry：每次取账本时把会话的 context 注入 store。span 是诊断，
        // 失败不影响账本提交；重复设置只是覆盖同一个 Arc。
        store.set_telemetry(self.core.telemetry.context());
        Some(store)
    }

    /// 当前会话的 Work 图视图；没有 durable 账本时返回 `None`。
    ///
    /// 视图不缓存状态：`refresh()` 从权威账本重建，事件流只是 best-effort 投影。
    pub fn work_graph_view(&self) -> Option<crate::subagent::WorkGraphView> {
        self.durable_work_store()
            .map(|store| crate::subagent::WorkGraphView::for_session(store, self.session_id()))
    }

    /// 所有可读会话的 Work 图集合视图；`skipped_sessions` 非零表示图不完整。
    pub fn work_graph_set_view(&self) -> Option<crate::subagent::WorkGraphView> {
        self.durable_work_store()
            .map(crate::subagent::WorkGraphView::all_sessions)
    }

    /// 当前会话 Work 图的快照。
    pub async fn work_graph(
        &self,
    ) -> Result<Option<crate::subagent::WorkGraph>, crate::subagent::WorkStoreError> {
        let Some(view) = self.work_graph_view() else {
            return Ok(None);
        };
        match view.refresh().await? {
            crate::subagent::GraphRefresh::Graph(graph) => Ok(Some(graph)),
            crate::subagent::GraphRefresh::Set(_) => Ok(None),
        }
    }

    /// 落定崩溃遗留的 Work；若它是某个恢复锚点的后继，同时把锚点还回去。
    ///
    /// 后继在 `begin` 时原子占用了锚点。进程在它运行期间崩溃、`recover_work` 把它
    /// 落定为终态后，这个后继从未完成，锚点必须释放，否则宿主永远无法重试原记录。
    /// 正常完成的后继保留占用（“一个锚点只成功恢复一次”），这里只处理恢复路径。
    async fn terminalize_recovered_work(
        store: &crate::subagent::DurableWorkStore,
        parent_session_id: &str,
        record: &crate::subagent::WorkRecord,
        result: crate::subagent::WorkResult,
    ) -> Result<bool, crate::subagent::WorkStoreError> {
        use crate::subagent::work::WorkStore as _;
        use crate::subagent::work_store::WorkFinish;

        if !matches!(
            store.finish(parent_session_id, &result).await?,
            WorkFinish::Finished(_)
        ) {
            return Ok(false);
        }
        if record.resumed_from.is_some() {
            store
                .release_resume(parent_session_id, record.work_id, &record.run_id)
                .await?;
        }
        Ok(true)
    }

    /// 启动阶段的子代理工作区回收（在 `recover_work` 之后调用）。
    ///
    /// 先从 Work 账本收集仍可能被恢复引用的进程 pid，交给 sweep 当作“仍在使用”：
    /// 崩溃遗留的可恢复 Work 的 worktree / 分支是 `resume_work` 继续运行的 cwd，
    /// 不能按“死 pid”回收。
    ///
    /// `.kiana/subagents` 工作区按仓库共享，因此扫描**所有会话**的记录，并且只在
    /// 账本完整可读时回收：没有 Work 账本，或某个会话目录被另一个存活进程持有/
    /// 打不开时，keep-set 可能漏掉其它会话仍在引用的工作区，宁可整个跳过。
    pub async fn sweep_subagent_workspaces(&self) {
        let Some(store) = self.durable_work_store() else {
            // 没有账本就无法区分“别的会话仍可恢复的工作区”和垃圾。
            return;
        };
        let (records, skipped) = match store.list_all_sessions().await {
            Ok(result) => result,
            // 账本损坏时不做回收：宁可留着工作区，也不能删掉可恢复的 worktree。
            Err(_) => return,
        };
        if skipped > 0 {
            // 部分扫描不等于完整账本：被别的进程持有的会话里可能仍有可恢复 Work。
            return;
        }
        let keep_pids = Self::sweep_keep_pids(&records);
        let state_dirs = Self::sweep_state_dirs(&records);
        self.core.sweep_subagent_workspaces(&keep_pids, &state_dirs);
    }

    /// 所有已经有过后继的 `(父会话, WorkId)`。出现后继即表示该锚点不再可恢复
    /// （“一个锚点只成功恢复一次”），除非后继又把 `resumedFrom` 还了回去。
    ///
    /// WorkId 由每个父会话的账本计数器独立分配：另一个会话里同一个数字是**另一个
    /// Work**。sweep 扫描所有会话，锚点判定必须按父会话隔离，否则别的会话的终态
    /// 后继会把本会话仍可恢复的记录误判成“已恢复”，工作区随即被当垃圾删掉。
    fn anchored_work_ids(
        records: &[crate::subagent::WorkRecord],
    ) -> std::collections::HashSet<(String, crate::subagent::WorkId)> {
        records
            .iter()
            .filter_map(|record| {
                record
                    .resumed_from
                    .map(|origin| (record.parent_session_id.clone(), origin))
            })
            .collect()
    }

    /// 记录是否仍可能被 `resume_work` 接受。
    ///
    /// Running 记录可以；崩溃落定的 `interrupted` 记录可以（前提是 `resumable`、
    /// 有 `sessionFile`，且还没有后继占用锚点）。这个条件同时给 keep-set 设了上界：
    /// 第一次恢复之后锚点不再被保留，keep 集合不会随会话寿命无限增长。
    fn work_may_be_resumed(
        record: &crate::subagent::WorkRecord,
        anchored: &std::collections::HashSet<(String, crate::subagent::WorkId)>,
    ) -> bool {
        use crate::subagent::WorkTerminal;

        if !record.is_terminal() {
            return true;
        }
        record.request.resumable
            && record.session_file().is_some()
            && !anchored.contains(&(record.parent_session_id.clone(), record.work_id))
            && matches!(
                record.terminal_result().map(|result| &result.terminal),
                Some(WorkTerminal::Failed { stop_reason, .. })
                    if stop_reason.as_deref() == Some("interrupted")
            )
    }

    /// 只读取子会话 JSONL 第一行（session header）里的 cwd。
    fn session_header_cwd(path: &std::path::Path) -> Option<String> {
        use std::io::{BufRead, BufReader, Read};

        const HEADER_LIMIT_BYTES: u64 = 64 * 1024;
        let file = std::fs::File::open(path).ok()?;
        let mut first_line = String::new();
        BufReader::new(file)
            .take(HEADER_LIMIT_BYTES)
            .read_line(&mut first_line)
            .ok()?;
        kiana_session_core::parse_session_entries(&first_line)
            .first()
            .and_then(|entry| entry.as_header())
            .and_then(|header| header.cwd.clone())
    }

    /// 记录关联的子会话若在隔离工作区里，返回其 ownership（含当前 pid/state_dir）。
    fn record_workspace_ownership(
        record: &crate::subagent::WorkRecord,
    ) -> Option<crate::subagent::IsolatedWorkspace> {
        let session_file = record.session_file()?;
        let cwd = Self::session_header_cwd(std::path::Path::new(session_file))?;
        crate::subagent::isolation::adopt_workspace(std::path::Path::new(&cwd))
    }

    /// 启动 sweep 的 keep-set：工作区仍可能被运行的或可恢复的 Work 引用。
    fn sweep_keep_pids(records: &[crate::subagent::WorkRecord]) -> Vec<u32> {
        let anchored = Self::anchored_work_ids(records);
        // 锚点只在仍有未终态后继时被引用；后继已终态的原记录不会再被恢复，
        // 排除它们，keep 集合才不会随会话寿命无界增长。
        let live_anchors: std::collections::HashSet<(&str, crate::subagent::WorkId)> = records
            .iter()
            .filter(|record| !record.is_terminal())
            .filter_map(|record| {
                record
                    .resumed_from
                    .map(|origin| (record.parent_session_id.as_str(), origin))
            })
            .collect();
        let mut keep = records
            .iter()
            .filter(|record| {
                !record.is_terminal()
                    || live_anchors.contains(&(record.parent_session_id.as_str(), record.work_id))
            })
            .filter_map(|record| record.pid)
            .collect::<Vec<_>>();
        for record in records
            .iter()
            .filter(|record| Self::work_may_be_resumed(record, &anchored))
        {
            if let Some(pid) = record.pid {
                keep.push(pid);
            }
            // 失败的 resume 已把 ownership pid 刷新成另一个（可能已死的）进程；只按
            // Work 记录里的 pid 会漏掉这个工作区。按子会话 header 的 cwd 找到实际
            // ownership 记录，保留它当前的 pid。
            if let Some(pid) = Self::record_workspace_ownership(record)
                .and_then(|workspace| workspace.ownership().pid)
            {
                keep.push(pid);
            }
        }
        keep.sort_unstable();
        keep.dedup();
        keep
    }

    /// 隔离状态目录：记录里声明的自定义 `state_dir`，加上从子会话 header 推导出的
    /// ownership 目录（两者都要 sweep）。
    ///
    /// header 要等首条消息落盘才存在；`begin`/`attach_session` 之后、首条消息之前
    /// 崩溃的 Work 只能靠记录里的 `isolationStateDir` 找到它的自定义目录。
    fn sweep_state_dirs(records: &[crate::subagent::WorkRecord]) -> Vec<std::path::PathBuf> {
        records
            .iter()
            .flat_map(|record| {
                record
                    .isolation_state_dir
                    .iter()
                    .cloned()
                    .chain(
                        Self::record_workspace_ownership(record)
                            .map(|workspace| workspace.ownership().state_dir.clone()),
                    )
                    .chain(Self::default_isolation_state_dir(record))
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// 隔离工作区默认落在 `<workdir>/.kiana/subagents`。ownership 记录丢失、或崩溃
    /// 发生在子会话 header 落盘前时，`record_workspace_ownership` 找不到 cwd，sweep
    /// 必须还能按记录里声明的工作目录扫到默认状态目录，否则 worktree/clone 会一直
    /// 留在仓库里。Shared 不创建任何资源，不加入扫描集。
    fn default_isolation_state_dir(
        record: &crate::subagent::WorkRecord,
    ) -> Option<std::path::PathBuf> {
        use crate::subagent::IsolationMode;

        if !matches!(
            record.request.isolation,
            Some(IsolationMode::Worktree | IsolationMode::CowClone)
        ) {
            return None;
        }
        record.request.workdir.as_deref().map(|workdir| {
            std::path::Path::new(workdir)
                .join(".kiana")
                .join("subagents")
        })
    }

    /// 启动时 sweep 本会话遗留的 Work。
    ///
    /// - 默认 `recovery=fail`：把崩溃遗留的 Running 记录落定为
    ///   `Failed { stopReason: "interrupted" }`，保留 `sessionFile` 供审计。
    /// - `recovery=resume`：不自动重放，返回 `resumable` 列表并保持 Running；
    ///   由宿主显式决定是否继续（Phase 1b 的 `resume_work`）。
    /// - pid 仍存活或进程内活任务登记表命中的记录跳过。
    pub async fn recover_work(
        &self,
    ) -> Result<Option<crate::subagent::WorkRecoveryReport>, crate::subagent::WorkStoreError> {
        use crate::subagent::work::{SystemWorkClock, WorkClock, WorkResult, WorkStore as _};
        use crate::subagent::work_store::work_is_live;

        let Some(store) = self.durable_work_store() else {
            return Ok(None);
        };
        let parent_session_id = self.session_id();
        let records = store.list_recoverable(&parent_session_id).await?;
        let now = SystemWorkClock.now_ms();
        let mut report = crate::subagent::WorkRecoveryReport::default();
        for record in records {
            // 能读到账本就证明本进程持有该会话的目录租约，另一个存活进程没有在写；
            // 因此只有本进程的活登记才算“仍在运行”。用 `kill(pid, 0)` 会因 pid 复用
            // 把崩溃的 Work 永久当成活的。
            let alive = record.pid == Some(std::process::id())
                && work_is_live(&parent_session_id, record.work_id, &record.run_id);
            if alive {
                report.skipped_live.push(record.work_id);
                continue;
            }
            match record.request.recovery {
                crate::subagent::WorkRecovery::Fail => {
                    let result = WorkResult::interrupted(
                        &record.request,
                        "work interrupted: process lost",
                        record.session_file().map(str::to_string),
                        record.created_at_ms,
                        now,
                    );
                    if Self::terminalize_recovered_work(&store, &parent_session_id, &record, result)
                        .await?
                    {
                        report.terminalized.push(record.work_id);
                    }
                }
                crate::subagent::WorkRecovery::Resume => {
                    // Phase 1b 的显式恢复要求 `sessionFile` 存在。崩溃发生在
                    // `attach_session` 之前的记录没有 transcript，永远无法 resume；
                    // 留在账本里只会让宿主反复拿到不可执行的 id，按中断落定。
                    if record.request.resumable && record.session_file().is_some() {
                        report.resumable.push(record.work_id);
                    } else {
                        let result = WorkResult::interrupted(
                            &record.request,
                            "work interrupted: process lost and no resumable session file",
                            record.session_file().map(str::to_string),
                            record.created_at_ms,
                            now,
                        );
                        if Self::terminalize_recovered_work(
                            &store,
                            &parent_session_id,
                            &record,
                            result,
                        )
                        .await?
                        {
                            report.terminalized.push(record.work_id);
                        }
                    }
                }
            }
        }
        // 修补“finish 已提交、release_resume 尚未提交”之间崩溃留下的后继。释放放在
        // 落定 Running 记录之后：本轮刚被落定的 tip 也要能在同一轮里逐层归还整条
        // 死链，而不是等下一次启动。每一轮重新读取账本，直到没有记录可再释放。
        // `release_resume` 还会在事务内复查该记录是否仍被后继占用，并发 `begin`
        // 无法从两轮之间插入新的占用。
        //
        // 只有 `Success` 与“仍可恢复的 interrupted tip”保留锚点（见
        // `record_keeps_resume_anchor`）：前者是“一个锚点只成功恢复一次”，后者保证
        // 同一 lineage 只有一个可恢复记录。其余终态都必须归还，否则原记录永久不可
        // 重试。
        loop {
            let all_records = store.list_for_session(&parent_session_id).await?;
            let mut released_any = false;
            for record in &all_records {
                if !record.is_terminal()
                    || record.resumed_from.is_none()
                    || record_keeps_resume_anchor(record)
                {
                    continue;
                }
                if store
                    .release_resume(&parent_session_id, record.work_id, &record.run_id)
                    .await?
                {
                    released_any = true;
                }
            }
            if !released_any {
                break;
            }
        }
        // 后台 Work 的两条账在这里对齐：Work 记录是权威状态，task 记录只是本次
        // attempt 的输出/通知载体。对账必须在 background sweep 之前，否则 sweep 会
        // 把“Work 已成功、task 记录尚未 finish”的崩溃窗口误报成 interrupted。
        self.reconcile_background_work(&store, &parent_session_id)
            .await;
        Ok(Some(report))
    }

    /// 启动恢复的后台对账：把 Work 账本的权威终态写回对应的后台 attempt。
    ///
    /// 正常路径在 Work terminal 提交之后才调用 `finish_task`；进程在两次提交之间
    /// 崩溃时 task 记录会停在 Running，而 Work 记录已经是终态。这里按 Work 结果补齐
    /// （`finish_task` 幂等，task 已终态时是 no-op）。
    ///
    /// 仍在运行的记录（本进程活登记命中）不碰。`recovery=resume` 留下的可恢复
    /// Running 记录没有执行者：本次 attempt 以 `process_lost` 关闭，宿主之后仍可用
    /// `resume_work` 开一个新的前台 Work。
    async fn reconcile_background_work(
        &self,
        store: &crate::subagent::DurableWorkStore,
        parent_session_id: &str,
    ) {
        use crate::subagent::work::WorkStore as _;

        let Some(manager) = self.background_task_manager() else {
            return;
        };
        let records = match store.list_for_session(parent_session_id).await {
            Ok(records) => records,
            Err(error) => {
                self.record_continuity_diagnostic(format!(
                    "work background reconciliation failed: {error}"
                ));
                return;
            }
        };
        for record in records {
            let Some(task_id) = record.background_task_id.as_deref() else {
                continue;
            };
            let alive = record.pid == Some(std::process::id())
                && crate::subagent::work_store::work_is_live(
                    parent_session_id,
                    record.work_id,
                    &record.run_id,
                );
            if alive {
                continue;
            }
            let outcome = match record.terminal_result() {
                Some(result) => Self::background_task_outcome(result),
                None => Self::background_attempt_interrupted(),
            };
            if let Err(error) = manager.finish_task(task_id, outcome).await {
                self.record_continuity_diagnostic(format!(
                    "background task `{task_id}` reconciliation failed: {error}"
                ));
            }
        }

        // 反向对账：journal 已有 workBacked attempt，但 Work 账本没有对应的
        // `backgroundTaskId`（崩溃发生在 journal Running 之后、begin/attach 之前）。
        // 只处理 Running 且已无执行者的记录。
        let attempts = match manager.list(parent_session_id).await {
            Ok(records) => records,
            Err(error) => {
                self.record_continuity_diagnostic(format!(
                    "background attempt reconciliation failed: {error}"
                ));
                return;
            }
        };
        for attempt in attempts {
            if attempt.is_terminal() || !attempt.work_backed {
                continue;
            }
            if crate::background::record_is_live(&attempt) {
                continue;
            }
            // 旧记录没有 workId 字段：从 `work:<id>` 的 turn id 兜底解析；runId
            // 缺失时按 workId 配对（升级前写入的 attempt 只能这样恢复）。
            let parsed_work_id = attempt
                .work_id
                .or_else(|| attempt.parent_turn_id.strip_prefix("work:")?.parse().ok());
            let Some(work_id) = parsed_work_id else {
                continue;
            };
            let run_id = attempt.run_id.as_deref();
            let work = match store
                .load(parent_session_id, crate::subagent::WorkId(work_id))
                .await
            {
                Ok(work) => work,
                Err(error) => {
                    self.record_continuity_diagnostic(format!(
                        "background attempt `{}` reconciliation failed: {error}",
                        attempt.task_id
                    ));
                    continue;
                }
            };
            let outcome = match work {
                Some(work) if run_id.is_none_or(|run_id| work.run_id.0 == run_id) => {
                    if work.background_task_id.as_deref() == Some(attempt.task_id.as_str()) {
                        // 已由 Work→task 方向处理。
                        continue;
                    }
                    match work.terminal_result() {
                        Some(result) => Self::background_task_outcome(result),
                        None => {
                            let alive = work.pid == Some(std::process::id())
                                && crate::subagent::work_store::work_is_live(
                                    parent_session_id,
                                    work.work_id,
                                    &work.run_id,
                                );
                            if alive {
                                continue;
                            }
                            Self::background_attempt_interrupted()
                        }
                    }
                }
                // Work 从未 begin，或该 id 已被另一个 run 占用：这个 attempt 不会
                // 再有权威终态。
                _ => Self::background_attempt_interrupted(),
            };
            if let Err(error) = manager.finish_task(&attempt.task_id, outcome).await {
                self.record_continuity_diagnostic(format!(
                    "background attempt `{}` reconciliation failed: {error}",
                    attempt.task_id
                ));
            }
        }
    }

    /// 取消后台任务。
    pub async fn cancel_background_task(
        &self,
        task_id: &str,
    ) -> Result<bool, crate::background::BackgroundTaskError> {
        match self.background_task_manager() {
            Some(manager) => manager.cancel(task_id).await,
            None => Err(crate::background::BackgroundTaskError::TaskNotFound(
                task_id.to_string(),
            )),
        }
    }

    // -- 生命周期 -----------------------------------------------------------

    /// `abort()` 的同步前缀（设置取消标志、中止 agent，不等 idle）。
    pub fn request_abort(&self) {
        self.core.request_abort();
    }

    /// `abort()`。
    pub async fn abort(&self) {
        self.core.abort().await;
    }

    /// `waitForIdle()`。
    pub async fn wait_for_idle(&self) {
        self.core.wait_for_idle().await;
    }

    // -- 模型管理 -----------------------------------------------------------

    /// `setModel(model, options?)`。
    pub async fn set_model(
        &self,
        model: Model,
        options: ModelMutationOptions,
    ) -> Result<(), AgentSessionError> {
        self.core.set_model_impl(model, options).await
    }

    /// `cycleModel(direction?, options?)`：在 scoped 模型（`--models`，需过滤出当前可用
    /// 的）或全部可用模型中循环切换。切换动作与 `set_model_impl` 一致（写 `model_change`
    /// 条目、可选持久化默认值、按目标模型解析 thinking level、发 `model_select` 事件）；
    /// 候选不足两个时返回 `None` 表示没有切换。
    pub async fn cycle_model(
        &self,
        direction: &str,
        options: ModelMutationOptions,
    ) -> Result<Option<ModelCycleResult>, AgentSessionError> {
        let scoped = lock(&self.core.scoped_models).clone();
        if !scoped.is_empty() {
            let available: HashSet<(String, String)> = self
                .core
                .model_runtime
                .get_available_snapshot()
                .into_iter()
                .map(|model| (model.provider.clone(), model.id.clone()))
                .collect();
            let scoped: Vec<ScopedModel> = scoped
                .into_iter()
                .filter(|entry| {
                    available.contains(&(entry.model.provider.clone(), entry.model.id.clone()))
                })
                .collect();
            if scoped.len() <= 1 {
                return Ok(None);
            }
            let current = self.core.current_model();
            let mut index = scoped
                .iter()
                .position(|entry| models_equal(Some(&entry.model), current.as_ref()))
                .unwrap_or(0);
            let len = scoped.len();
            index = if direction == "forward" {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            };
            let next = scoped[index].clone();
            let thinking = self
                .core
                .thinking_level_for_model_switch(Some(&next.model), next.thinking_level);
            self.core.agent.set_model(next.model.clone());
            // 模型路由的编辑工具随模型切换（见 set_model_impl）。
            self.core.rebuild_builtin_tool_definitions();
            self.core
                .with_session_mut(|manager| {
                    manager.append_model_change(&next.model.provider, &next.model.id)
                })
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
            if options.persist.unwrap_or(false) {
                self.core.settings_manager.set_default_model_and_provider(
                    next.model.provider.clone(),
                    next.model.id.clone(),
                );
                self.core
                    .add_persisted_default_to_non_empty_scope(&next.model);
            }
            self.core
                .set_thinking_level_impl(thinking, ModelMutationOptions::default())?;
            self.core
                .emit_model_select(next.model.clone(), current, ModelSelectSource::Cycle)
                .await;
            return Ok(Some(ModelCycleResult {
                model: next.model,
                thinking_level: self.core.agent.thinking_level(),
                is_scoped: true,
            }));
        }
        let available = self.core.model_runtime.get_available_snapshot();
        if available.len() <= 1 {
            return Ok(None);
        }
        let current = self.core.current_model();
        let mut index = available
            .iter()
            .position(|model| models_equal(Some(model), current.as_ref()))
            .unwrap_or(0);
        let len = available.len();
        index = if direction == "forward" {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        let next = available[index].clone();
        let thinking = self.core.thinking_level_for_model_switch(Some(&next), None);
        self.core.agent.set_model(next.clone());
        self.core
            .with_session_mut(|manager| manager.append_model_change(&next.provider, &next.id))
            .map_err(|error| AgentSessionError::other(error.to_string()))?;
        if options.persist.unwrap_or(false) {
            self.core
                .settings_manager
                .set_default_model_and_provider(next.provider.clone(), next.id.clone());
            self.core.add_persisted_default_to_non_empty_scope(&next);
        }
        self.core
            .set_thinking_level_impl(thinking, ModelMutationOptions::default())?;
        self.core
            .emit_model_select(next.clone(), current, ModelSelectSource::Cycle)
            .await;
        Ok(Some(ModelCycleResult {
            model: next,
            thinking_level: self.core.agent.thinking_level(),
            is_scoped: false,
        }))
    }

    // -- thinking 管理 ------------------------------------------------------

    /// `setThinkingLevel(level, options?)`。
    pub fn set_thinking_level(&self, level: ModelThinkingLevel, options: ModelMutationOptions) {
        // TS 的同步方法在写会话文件失败时抛异常；Rust 这个公开签名返回 ()（跨 crate 调用方
        // 依赖它），因此持久化错误在这里不可见（见交付报告 deviations）。
        let _ = self.core.set_thinking_level_impl(level, options);
    }

    /// `cycleThinkingLevel(options?)`。
    pub fn cycle_thinking_level(
        &self,
        options: ModelMutationOptions,
    ) -> Option<ModelThinkingLevel> {
        if !self.core.supports_thinking() {
            return None;
        }
        let levels = self.core.available_thinking_levels();
        if levels.is_empty() {
            return None;
        }
        let current = self.core.agent.thinking_level();
        let index = levels
            .iter()
            .position(|level| *level == current)
            .map(|index| (index + 1) % levels.len())
            .unwrap_or(0);
        let next = levels[index];
        let _ = self.core.set_thinking_level_impl(next, options);
        Some(next)
    }

    /// `getAvailableThinkingLevels()`。
    pub fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        self.core.available_thinking_levels()
    }

    /// `supportsThinking()`。
    pub fn supports_thinking(&self) -> bool {
        self.core.supports_thinking()
    }

    // -- 队列模式 -----------------------------------------------------------

    /// `setSteeringMode(mode)`。
    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.core.agent.set_steering_mode(mode);
        self.core
            .settings_manager
            .set_steering_mode(steering_mode_from_queue(mode));
    }

    /// `setFollowUpMode(mode)`。
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.core.agent.set_follow_up_mode(mode);
        self.core
            .settings_manager
            .set_follow_up_mode(steering_mode_from_queue(mode));
    }

    // -- 压缩 ---------------------------------------------------------------

    /// 测试接缝：跑一次压缩检查（TS 测试直接访问私有 `_checkCompaction`）。
    #[doc(hidden)]
    pub async fn check_compaction_for_test(&self, assistant: &AssistantMessage) -> bool {
        self.core
            .check_compaction(assistant, true)
            .await
            .unwrap_or(false)
    }

    /// 测试接缝：触发一次自动压缩（TS 测试直接访问私有 `_runAutoCompaction`）。
    #[doc(hidden)]
    pub async fn run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        self.core.run_auto_compaction(reason, will_retry).await
    }

    /// `compact(customInstructions?)`。
    ///
    /// 成功后按默认收敛配置（2.5s settle、90s timeout）等待 token 下降并稳定，发出
    /// [`AgentSessionEvent::Condensation`]；失败/取消也发出 `phase: failed` 的观测事件。
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionResult, AgentSessionError> {
        self.core.compact(custom_instructions, None).await
    }

    /// 与 [`Self::compact`] 相同，但允许调用方覆盖收敛观测参数。
    ///
    /// 测试和交互式命令可以传更短的 settle/timeout；传 `None` 等价于默认配置。
    pub async fn compact_with_condensation_options(
        &self,
        custom_instructions: Option<String>,
        condensation: Option<CondensationConfig>,
    ) -> Result<CompactionResult, AgentSessionError> {
        self.core.compact(custom_instructions, condensation).await
    }

    /// 当前会话（压缩后的 projection）的 token 估算。
    ///
    /// 与手动压缩基线使用同一个估算器；`CondensationEvent.before_tokens/after_tokens`
    /// 就是压缩前后两次调用本方法的值。
    pub fn context_tokens(&self) -> u64 {
        self.core.estimate_messages_tokens()
    }

    /// 最近一次手动压缩的收敛观测（没有手动压缩时为 `None`）。
    pub fn last_condensation(&self) -> Option<CondensationEvent> {
        lock(&self.core.last_condensation).clone()
    }

    /// 用当前上下文与模型构建下一次请求的上下文报告。
    ///
    /// stand-in prompt 模拟用户的下一条输入；报告按 tool/prompt/turn-context/segment
    /// 分类统计 token 与预览。没有选中模型时返回
    /// [`AgentSessionError::NoModelSelected`]。
    pub fn context_report(
        &self,
        options: ContextReportOptions,
    ) -> Result<ContextReport, AgentSessionError> {
        let model = self
            .core
            .current_model()
            .ok_or(AgentSessionError::NoModelSelected)?;
        let context = AgentContext {
            messages: self.core.projection().messages,
            tools: Some(self.core.agent.tools()),
        };
        // 用 agent 的真实 `convert_to_llm`：会话会把 compactionSummary /
        // bashExecution 等自定义消息投影成 user 文本，缺省投影会漏算它们。
        let convert = self.core.agent.convert_to_llm();
        Ok(build_context_report_with(
            &context,
            &model,
            &options,
            |message| convert(vec![message.clone()]).into_iter().next(),
        ))
    }

    /// 捕获一次只读 Context Shadow（Phase 4）。
    ///
    /// 复用 [`Self::context_report`]；不修改消息、工具或压缩状态。Enforce 未实现：
    /// 影子只是预览，`ContextShadow::is_enforced()` 恒为 `false`。
    pub fn context_shadow(
        &self,
        options: ContextReportOptions,
    ) -> Result<crate::context_shadow::ContextShadow, AgentSessionError> {
        self.context_report(options)
            .map(crate::context_shadow::ContextShadow::capture)
    }

    /// `abortCompaction()`。
    pub fn abort_compaction(&self) {
        self.core.abort_compaction();
    }

    /// `abortBranchSummary()`。
    pub fn abort_branch_summary(&self) {
        self.core.abort_branch_summary();
    }

    /// `setAutoCompactionEnabled(enabled)`。
    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        let _ = self.core.settings_manager.set_compaction_enabled(enabled);
    }

    /// `get autoCompactionEnabled()`。
    pub fn auto_compaction_enabled(&self) -> bool {
        self.core.settings_manager.get_compaction_enabled()
    }

    // -- 扩展绑定 -----------------------------------------------------------

    /// 执行 `SessionStart` hooks（best-effort）。`additionalContext` 暂存到下一次
    /// provider 请求的 hidden context；其他输出经 `hook_notification` 呈现。
    #[doc(hidden)]
    pub async fn run_session_start_hook(&self, source: &str) {
        self.core.run_session_start_hook(source).await;
    }

    /// 执行 `SessionEnd` hooks（best-effort，忽略决策）。
    #[doc(hidden)]
    pub async fn run_session_end_hook(&self, reason: &str) {
        self.core.run_session_end_hook(reason).await;
    }

    /// 最近的 hook 审计记录（有界环形缓冲，最旧的在前）。
    pub fn hook_audit_records(&self) -> Vec<HookAuditRecord> {
        self.core.hook_audit_records()
    }

    /// `bindExtensions(bindings)`。
    pub async fn bind_extensions(&self, bindings: ExtensionBindings) {
        if let Some(ui) = bindings.ui_context {
            *lock(&self.core.extension_ui_context) = Some(ui);
        }
        if let Some(mode) = bindings.mode {
            *lock(&self.core.extension_mode) = mode;
        }
        if let Some(actions) = bindings.command_context_actions {
            *lock(&self.core.extension_command_context_actions) = Some(actions);
        }
        if let Some(handler) = bindings.abort_handler {
            *lock(&self.core.extension_abort_handler) = Some(handler);
        }
        if let Some(handler) = bindings.shutdown_handler {
            *lock(&self.core.extension_shutdown_handler) = Some(handler);
        }
        if let Some(listener) = bindings.on_error {
            *lock(&self.core.extension_error_listener) = Some(listener);
        }
        if let Some(runner) = self.core.runner() {
            self.apply_extension_bindings(&runner);
            let _ = runner
                .emit(ExtensionEvent::Session(SessionEvent::Start(
                    self.core.session_start_event.clone(),
                )))
                .await;
            runner.report_unhandled_mcp_servers();
        }
        let hook_source = match self.core.session_start_event.reason {
            kiana_ext_api::events::SessionStartReason::Startup => "startup",
            kiana_ext_api::events::SessionStartReason::Reload => "reload",
            kiana_ext_api::events::SessionStartReason::New => "new",
            kiana_ext_api::events::SessionStartReason::Resume => "resume",
            kiana_ext_api::events::SessionStartReason::Fork => "fork",
        };
        self.core.run_session_start_hook(hook_source).await;
        let reason = match self.core.session_start_event.reason {
            kiana_ext_api::events::SessionStartReason::Reload => ResourcesDiscoverReason::Reload,
            _ => ResourcesDiscoverReason::Startup,
        };
        self.extend_resources_from_extensions(reason).await;
    }

    async fn extend_resources_from_extensions(&self, reason: ResourcesDiscoverReason) {
        let Some(runner) = self.core.runner() else {
            return;
        };
        if !runner.has_handlers("resources_discover") {
            return;
        }
        let paths = match runner.emit_resources_discover(&self.core.cwd, reason).await {
            Ok(paths) => paths,
            Err(_) => return,
        };
        if paths.skill_paths.is_empty()
            && paths.prompt_paths.is_empty()
            && paths.theme_paths.is_empty()
        {
            return;
        }
        let extension_paths = ResourceExtensionPaths {
            skill_paths: build_extension_resource_paths(&paths.skill_paths),
            prompt_paths: build_extension_resource_paths(&paths.prompt_paths),
            theme_paths: build_extension_resource_paths(&paths.theme_paths),
        };
        if self.loader().extend_resources(extension_paths).is_ok() {
            self.refresh_command_sources();
            let names = self.core.get_active_tool_names();
            let _ = self.core.rebuild_system_prompt(&names);
        }
    }

    /// `reload(options?)`：重建扩展运行时。阶段：向旧 runner 发 `session_shutdown` 并
    /// 失效 → 记录默认工具集（重载前后对比用于补激活）→ 重载设置与 API provider 注册
    /// 表、重载资源加载器 → 以「旧激活工具 + 新增默认工具 + 旧 flag 值」重建运行时 →
    /// 有绑定时回调 `before_session_start` 并重发 `session_start`（reason = reload）与
    /// `resources_discover`。
    pub async fn reload(
        &self,
        before_session_start: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<(), AgentSessionError> {
        let Some(old_runner) = self.core.runner() else {
            return Ok(());
        };
        let previous_flag_values = old_runner.get_flag_values();
        kiana_extensions::runner::emit_session_shutdown_event(
            &old_runner,
            kiana_ext_api::events::SessionShutdownEvent {
                event_type: kiana_ext_api::events::SessionShutdownEventType::SessionShutdown,
                reason: kiana_ext_api::events::SessionShutdownReason::Reload,
                target_session_file: None,
            },
        )
        .await;
        old_runner.invalidate(None);
        let uses_default = self.core.uses_default_tools;
        let modifiers = self.core.default_tool_modifiers.clone();
        let get_default_tools = |core: &Core| -> Vec<String> {
            if uses_default {
                let base = core
                    .settings_manager
                    .get_default_tools()
                    .unwrap_or_else(|| {
                        kiana_config::settings::tools::DEFAULT_TOOL_NAMES
                            .iter()
                            .map(|name| name.to_string())
                            .collect()
                    });
                kiana_config::settings::tools::apply_tool_modifiers(&base, &modifiers)
            } else {
                Vec::new()
            }
        };
        let previous_default_tools: HashSet<String> =
            get_default_tools(&self.core).into_iter().collect();
        self.core.settings_manager.reload().await;
        self.core.sync_queue_modes_from_settings();
        kiana_ai::compat::reset_api_providers();
        let _ = self.loader().reload(None).await;
        let added_default_tools: Vec<String> = get_default_tools(&self.core)
            .into_iter()
            .filter(|name| !previous_default_tools.contains(name))
            .collect();
        {
            let active = self.core.get_active_tool_names();
            let mut pending = lock(&self.core.pending_tool_names);
            for name in active {
                pending.insert(name);
            }
        }
        self.refresh_command_sources();
        let mut active = self.core.get_active_tool_names();
        active.extend(added_default_tools);
        self.build_runtime(Some(active), Some(previous_flag_values), true);
        let has_bindings = lock(&self.core.extension_ui_context).is_some()
            || lock(&self.core.extension_command_context_actions).is_some()
            || lock(&self.core.extension_shutdown_handler).is_some()
            || lock(&self.core.extension_error_listener).is_some();
        if has_bindings {
            if let Some(callback) = before_session_start {
                callback();
            }
            if let Some(runner) = self.core.runner() {
                let _ = runner
                    .emit(ExtensionEvent::Session(SessionEvent::Start(
                        SessionStartEvent {
                            event_type: kiana_ext_api::events::SessionStartEventType::SessionStart,
                            reason: kiana_ext_api::events::SessionStartReason::Reload,
                            previous_session_file: None,
                        },
                    )))
                    .await;
                runner.report_unhandled_mcp_servers();
            }
            self.extend_resources_from_extensions(ResourcesDiscoverReason::Reload)
                .await;
        }
        Ok(())
    }

    // -- 自动重试 -----------------------------------------------------------

    /// `abortRetry()`。
    pub fn abort_retry(&self) {
        self.core.abort_retry();
    }

    /// `get isRetrying()`。
    pub fn is_retrying(&self) -> bool {
        self.core.is_retrying()
    }

    /// `get autoRetryEnabled()`。
    pub fn auto_retry_enabled(&self) -> bool {
        self.core.settings_manager.get_retry_enabled()
    }

    /// `setAutoRetryEnabled(enabled)`。
    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        let _ = self.core.settings_manager.set_retry_enabled(enabled);
    }

    // -- permissions --------------------------------------------------------

    /// settings/env 是否显式关闭了权限检查。
    ///
    /// 为真时 CLI 的 `--allow`/`--deny`/`--add-dir` 不应创建权限引擎；
    /// 只有 `--permission-mode` 能重新启用。
    pub fn permissions_disabled(&self) -> bool {
        self.core.permissions_disabled
    }

    /// 运行期切换权限模式。
    ///
    /// 模式在下一个工具调用立即生效；切换会清空本会话的「总是允许」记录。引擎尚未启用
    /// 时（settings/env 都没有配置）会创建一个默认策略的引擎。
    pub fn set_permission_mode(&self, mode: PermissionMode) {
        self.core.set_permission_mode(mode);
    }

    /// 替换全部权限规则。
    pub fn set_permission_rules(&self, rules: Vec<PermissionRule>) {
        self.core.set_permission_rules(rules);
    }

    /// 追加权限规则（保留 settings/env 已有的规则）。
    pub fn add_permission_rules(&self, rules: impl IntoIterator<Item = PermissionRule>) {
        self.core.add_permission_rules(rules);
    }

    /// 追加一个可写工作区根（CLI `--add-dir`）。
    pub fn add_permission_directory(&self, path: impl Into<PathBuf>) {
        self.core.add_permission_directory(path);
    }

    /// 批量追加可写工作区根；只重建一次工具定义。
    pub fn add_permission_directories(&self, paths: impl IntoIterator<Item = PathBuf>) {
        self.core.add_permission_directories(paths);
    }

    /// 当前权限模式；权限引擎未启用时返回 `None`。
    pub fn permission_mode(&self) -> Option<PermissionMode> {
        lock(&self.core.permissions)
            .as_ref()
            .map(|engine| engine.mode())
    }

    /// 当前有效权限策略的克隆；权限引擎未启用时返回 `None`。
    ///
    /// 返回的是引擎合并后的结果（settings 分层、CLI flags、managed policy 都已应用），
    /// 供 `/permissions` 面板与 headless 文本输出组装只读快照；不要在 UI 里自行读取
    /// settings 文件重做合并。
    pub fn permission_policy(&self) -> Option<PermissionPolicy> {
        lock(&self.core.permissions)
            .as_ref()
            .map(|engine| engine.policy())
    }

    /// managed policy 是否禁用了 `bypass` 模式；权限引擎未启用时为 `false`。
    pub fn bypass_permissions_disabled(&self) -> bool {
        lock(&self.core.permissions)
            .as_ref()
            .is_some_and(|engine| engine.bypass_permissions_disabled())
    }

    /// 本会话「总是允许」记录数；权限引擎未启用时为 0。
    pub fn permission_session_allow_count(&self) -> usize {
        lock(&self.core.permissions)
            .as_ref()
            .map(|engine| engine.session_allow_count())
            .unwrap_or(0)
    }

    /// 按会话列出后台任务记录（运行中与终态都包含，按 `created_at_ms` 升序）。
    ///
    /// manager 未启用（continuity 关闭）时返回空列表；调用方若需要区分“未启用”与
    /// “没有任务”，先用 [`Self::background_task_manager`] 判断。
    pub async fn list_background_tasks(
        &self,
    ) -> Result<Vec<crate::background::BackgroundTaskRecord>, crate::background::BackgroundTaskError>
    {
        match self.background_task_manager() {
            Some(manager) => manager.list(self.session_id().as_str()).await,
            None => Ok(Vec::new()),
        }
    }

    // -- memory -------------------------------------------------------------

    /// 第 10 周：持久记忆是否启用。
    pub fn memory_enabled(&self) -> bool {
        self.core.memory.is_some()
    }

    /// 当前 load-time 记忆索引（诊断/测试；不含 topic 正文）。
    pub fn memory_prompt_index(&self) -> Option<String> {
        self.core
            .memory
            .as_ref()
            .and_then(|memory| memory.system_prompt_index())
    }

    /// 配置层合并后的整体指纹（`sha256:<hex>`）；可用于会话/缓存版本判断。
    pub fn settings_config_fingerprint(&self) -> String {
        self.core.config_fingerprint().to_string()
    }

    /// 某个 key path 的胜出来源（例如 `permissions.mode`）；容器 key 返回其后代里
    /// 优先级最高的来源。
    pub fn settings_config_origin(&self, key_path: &str) -> Option<ConfigLayerSource> {
        self.core.config_origin(key_path)
    }

    /// 配置层加载/严格诊断（文件、行列、key path）。
    pub fn settings_config_diagnostics(&self) -> Vec<String> {
        self.core.config_diagnostics().to_vec()
    }

    // -- bash ---------------------------------------------------------------

    /// `executeBash(command, onChunk?, options?)`。
    ///
    /// 与模型发起的 `bash` 工具一样经过权限引擎与 `KIANA_SANDBOX_POLICY` 沙箱：
    /// `read-only` 等模式会拒绝，`Ask` 在交互式会话里弹确认、无 UI 时 fail-closed。
    pub async fn execute_bash(
        &self,
        command: String,
        on_chunk: Option<BashChunkCallback>,
        options: Option<ExecuteBashOptions>,
    ) -> Result<BashResult, AgentSessionError> {
        let token = CancellationToken::new();
        let bash_id = self.core.next_bash_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.core.bash_abort).push((bash_id, token.clone()));
        // 任何返回路径（包括 future 被丢弃）都由 Drop 注销。
        let _registration = BashAbortRegistration {
            core: Arc::clone(&self.core),
            bash_id,
        };
        let prefix = self.core.settings_manager.get_shell_command_prefix();
        let shell_path = self.core.settings_manager.get_shell_path();
        let resolved_command = match prefix {
            Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{command}"),
            _ => command.clone(),
        };
        // 权限层对这条「直接执行」的路径同样生效（read-only 等模式下必须拒绝），
        // 与模型发起的 bash 工具调用共用同一套规则与会话批准。检查的是**实际执行**的
        // 文本：`shellCommandPrefix` 里的内容同样会被运行。
        if let Some(refusal) = self.core.bash_refusal(&resolved_command).await {
            return Err(AgentSessionError::other(refusal));
        }
        let options = options.unwrap_or_default();
        let operations = match options.operations.clone() {
            Some(operations) => operations,
            None => {
                // 与工具表里的 bash 使用同一份沙箱策略：这条路径供交互式 `!cmd` 与 RPC
                // 直接执行，不能因为不走工具调用就失去隔离。会话级策略已经合并了
                // 权限模式（read-only/bypass）与 addDirs，不能在这里重新读
                // `KIANA_SANDBOX_POLICY`，否则同一个会话会出现两套结论。
                //
                // `SandboxManager::new` 可能触发 bubblewrap 探针（同步、最多 500ms），
                // 放到 blocking 线程，避免卡住 current-thread 运行时。
                let sandbox = self.core.sandbox_manager();
                let shell_path = shell_path.clone();
                let operations = tokio::task::spawn_blocking(move || match sandbox {
                    Some(sandbox) => kiana_tools::bash::create_local_shell_operations_with_sandbox(
                        "bash",
                        Arc::new(move || {
                            kiana_core::shell::get_shell_config(shell_path.as_deref())
                        }),
                        Some(sandbox),
                    ),
                    None => kiana_tools::bash::create_local_bash_operations(shell_path.as_deref()),
                })
                .await;
                match operations {
                    Ok(operations) => operations,
                    Err(error) => {
                        // 条目由 `_registration` 的 Drop 注销。
                        return Err(AgentSessionError::other(error.to_string()));
                    }
                }
            }
        };
        let core = Arc::downgrade(&self.core);
        let id = options.id.clone();
        let on_chunk = on_chunk;
        let executor_on_chunk: kiana_tools::bash_executor::BashChunkCallback =
            Arc::new(move |delta: &str| {
                if let Some(on_chunk) = &on_chunk {
                    on_chunk(delta);
                }
                if let Some(core) = core.upgrade() {
                    core.emit(AgentSessionEvent::BashExecutionUpdate {
                        id: id.clone(),
                        delta: delta.to_string(),
                    });
                }
            });
        let result = execute_bash_with_operations(
            &resolved_command,
            &self.core.session_cwd(),
            &*operations,
            Some(kiana_tools::bash_executor::BashExecutorOptions {
                on_chunk: Some(executor_on_chunk),
                signal: Some(token.clone()),
            }),
        )
        .await;
        match result {
            Ok(result) => {
                self.core
                    .record_bash_result(command, &result, options.exclude_from_context)?;
                Ok(result)
            }
            Err(error) => Err(AgentSessionError::other(error.to_string())),
        }
    }

    /// `recordBashResult(command, result, options?)`。
    pub fn record_bash_result(
        &self,
        command: String,
        result: &BashResult,
        exclude_from_context: Option<bool>,
    ) {
        // TS 的同步方法写文件失败会抛异常；这个公开签名返回 ()，错误不可见（见 deviations）。
        let _ = self
            .core
            .record_bash_result(command, result, exclude_from_context);
    }

    /// `abortBash()`。
    pub fn abort_bash(&self) {
        for (_id, token) in lock(&self.core.bash_abort).iter() {
            token.cancel();
        }
    }

    /// `get isBashRunning()`。
    pub fn is_bash_running(&self) -> bool {
        !lock(&self.core.bash_abort).is_empty()
    }

    /// `get hasPendingBashMessages()`。
    pub fn has_pending_bash_messages(&self) -> bool {
        !lock(&self.core.pending_bash_messages).is_empty()
    }

    // -- 会话管理 -----------------------------------------------------------

    /// `setSessionName(name)`。
    pub fn set_session_name(&self, name: &str) {
        // TS 的同步方法写文件失败会抛异常（事件不会发出）；这个公开签名返回 ()，
        // 因此记录错误，由下一次 run 的结束点暴露（见交付报告 deviations）。
        if let Err(error) = self
            .core
            .with_session_mut(|manager| manager.append_session_info(name))
        {
            self.core.record_persistence_error(error);
            return;
        }
        let event = AgentSessionEvent::SessionInfoChanged {
            name: self.core.session_name(),
        };
        self.core.emit(event.clone());
        if let Some(runner) = self.core.runner()
            && let AgentSessionEvent::SessionInfoChanged { name } = &event
        {
            spawn_local_event(
                runner,
                ExtensionEvent::Session(SessionEvent::InfoChanged(SessionInfoChangedEvent {
                    event_type: SessionInfoChangedEventType::SessionInfoChanged,
                    name: name.clone(),
                })),
            );
        }
    }

    // -- 树导航 -------------------------------------------------------------

    /// `navigateTree(targetId, options?)`：把会话 leaf 移到目标条目（新建分支），可选地
    /// 对被放弃的分支生成摘要。流程：状态检查（流式/压缩中拒绝）→ 目标存在性 → 收集
    /// 待摘要条目 → `session_before_tree` 扩展事件（可取消、可提供摘要/指令/标签）→
    /// 默认摘要（`generate_branch_summary`，可中止/失败）→ `branch_with_summary` 或
    /// `branch`/`resetLeaf` 落盘 → 重建上下文与工具 → 发 `session_tree`。摘要结果为空
    /// 字符串时按 TS 的 truthiness 处理（视为没有摘要）。所有 `SessionManager` 写入失败
    /// 都会作为错误返回（TS 同样没有 catch）。
    pub async fn navigate_tree(
        &self,
        target_id: String,
        options: NavigateTreeOptions,
    ) -> Result<TreeNavigationResult, AgentSessionError> {
        if self.core.is_streaming_active() {
            return Err(AgentSessionError::TreeWhileStreaming);
        }
        if self.core.is_compacting() {
            return Err(AgentSessionError::TreeWhileCompacting);
        }
        let old_leaf_id = self
            .core
            .with_session(|manager| manager.get_leaf_id().map(str::to_string));
        if Some(target_id.as_str()) == old_leaf_id.as_deref() {
            return Ok(TreeNavigationResult {
                cancelled: false,
                ..Default::default()
            });
        }
        if options.summarize == Some(true) && self.core.current_model().is_none() {
            return Err(AgentSessionError::NoModelForSummarization);
        }
        let target_entry = self
            .core
            .with_session(|manager| manager.get_entry(&target_id).cloned());
        let Some(target_entry) = target_entry else {
            return Err(AgentSessionError::EntryNotFound { id: target_id });
        };

        let collected = {
            let manager = lock(&self.core.session_manager);
            collect_entries_for_branch_summary(&*manager, old_leaf_id.as_deref(), &target_id)
        };
        let mut custom_instructions = options.custom_instructions.clone();
        let mut replace_instructions = options.replace_instructions;
        let mut label = options.label.clone();
        let preparation = kiana_ext_api::events::TreePreparation {
            target_id: target_id.clone(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id: collected.common_ancestor_id.clone(),
            entries_to_summarize: collected.entries.clone(),
            user_wants_summary: options.summarize.unwrap_or(false),
            custom_instructions: custom_instructions.clone(),
            replace_instructions,
            label: label.clone(),
        };
        let token = CancellationToken::new();
        *lock(&self.core.branch_summary_abort) = Some(token.clone());
        let outcome: Result<TreeNavigationResult, AgentSessionError> = async {
            let mut extension_summary: Option<kiana_ext_api::events::TreeSummary> = None;
            let mut from_extension = false;
            if let Some(runner) = self.core.runner()
                && runner.has_handlers("session_before_tree")
            {
                let result = runner
                    .emit(ExtensionEvent::Session(SessionEvent::BeforeTree(
                        kiana_ext_api::events::SessionBeforeTreeEvent {
                            event_type:
                                kiana_ext_api::events::SessionBeforeTreeEventType::SessionBeforeTree,
                            preparation: preparation.clone(),
                            signal: token.clone(),
                        },
                    )))
                    .await;
                if let Ok(Some(ExtensionEventResult::SessionBeforeTree(result))) = result {
                    if result.cancel == Some(true) {
                        return Ok(TreeNavigationResult {
                            cancelled: true,
                            ..Default::default()
                        });
                    }
                    if result.summary.is_some() && options.summarize == Some(true) {
                        extension_summary = result.summary;
                        from_extension = true;
                    }
                    if result.custom_instructions.is_some() {
                        custom_instructions = result.custom_instructions;
                    }
                    if result.replace_instructions.is_some() {
                        replace_instructions = result.replace_instructions;
                    }
                    if result.label.is_some() {
                        label = result.label;
                    }
                }
            }

            let mut summary_text: Option<String> = None;
            let mut summary_details: Option<serde_json::Value> = None;
            let mut summary_usage: Option<Usage> = None;
            if options.summarize == Some(true)
                && !collected.entries.is_empty()
                && extension_summary.is_none()
            {
                let Some(model) = self.core.current_model() else {
                    return Err(AgentSessionError::NoModelForSummarization);
                };
                let auth = self
                    .core
                    .summarization_request_auth(model, Some(token.clone()))
                    .await?;
                let branch_settings = self.core.settings_manager.get_branch_summary_settings();
                let result = generate_branch_summary(
                    &collected.entries,
                    &GenerateBranchSummaryOptions {
                        model: auth.model.clone(),
                        api_key: auth.api_key.clone(),
                        headers: auth.headers.clone(),
                        env: auth.env.clone(),
                        signal: abort_scope_from_token(&token),
                        custom_instructions: custom_instructions.clone(),
                        replace_instructions,
                        reserve_tokens: Some(branch_settings.reserve_tokens as u64),
                        stream_fn: Some(self.core.agent.stream_function()),
                        retry: Some(self.core.retry_policy()),
                        callbacks: Some(self.core.summarization_retry_callbacks(
                            SummarizationRetrySource::BranchSummary,
                            None,
                        )),
                    },
                )
                .await
                .map_err(|error| AgentSessionError::other(error.to_string()))?;
                if result.aborted == Some(true) {
                    return Ok(TreeNavigationResult {
                        cancelled: true,
                        aborted: Some(true),
                        ..Default::default()
                    });
                }
                if let Some(error) = result.error {
                    return Err(AgentSessionError::other(error));
                }
                summary_text = result.summary;
                summary_usage = result.usage;
                summary_details = Some(serde_json::json!({
                    "readFiles": result.read_files.unwrap_or_default(),
                    "modifiedFiles": result.modified_files.unwrap_or_default(),
                }));
            } else if let Some(extension_summary) = extension_summary {
                summary_text = Some(extension_summary.summary);
                summary_details = extension_summary.details;
                summary_usage = extension_summary.usage;
            }

            let (new_leaf_id, editor_text) = match &target_entry {
                SessionEntry::Message(entry) if entry.message.role() == "user" => (
                    entry.parent_id.clone(),
                    Some(agent_message_text(&entry.message)),
                ),
                SessionEntry::CustomMessage(entry) => (
                    entry.parent_id.clone(),
                    Some({
                        // TS: `contentText(targetEntry.content, "")`。
                        let mut object = IndexMap::new();
                        object.insert("content".to_string(), entry.content.clone());
                        agent_message_text(&AgentMessage::Custom(Box::new(object)))
                    }),
                ),
                _ => (Some(target_id.clone()), None),
            };

            let mut summary_entry = None;
            // TS 用 truthiness 判断：空字符串等同于没有摘要（不建 branch_summary 条目，
            // 标签留在目标条目上，session_tree 的 fromExtension 为 undefined）。
            if let Some(summary_text) = summary_text.filter(|text| !text.is_empty()) {
                // TS 的这些 SessionManager 调用没有 catch：写失败会 reject navigateTree。
                let summary_id = self
                    .core
                    .with_session_mut(|manager| {
                        manager.branch_with_summary(
                            new_leaf_id.as_deref(),
                            &summary_text,
                            summary_details.clone(),
                            Some(from_extension),
                            summary_usage.clone(),
                        )
                    })
                    .map_err(|error| AgentSessionError::other(error.to_string()))?;
                summary_entry = self
                    .core
                    .with_session(|manager| manager.get_entry(&summary_id).cloned());
                if let Some(label) = &label {
                    self.core
                        .with_session_mut(|manager| {
                            manager.append_label_change(&summary_id, Some(label))
                        })
                        .map_err(|error| AgentSessionError::other(error.to_string()))?;
                }
            } else if new_leaf_id.is_none() {
                // `resetLeaf()` 只改内存 leaf（不落盘）。
                self.core.with_session_mut(|manager| manager.reset_leaf());
            } else if let Some(new_leaf_id) = &new_leaf_id {
                self.core
                    .with_session_mut(|manager| manager.branch(new_leaf_id))
                    .map_err(|error| AgentSessionError::other(error.to_string()))?;
            }
            if let (Some(label), None) = (&label, &summary_entry) {
                self.core
                    .with_session_mut(|manager| {
                        manager.append_label_change(&target_id, Some(label))
                    })
                    .map_err(|error| AgentSessionError::other(error.to_string()))?;
            }
            self.core.refresh_finalized_context();
            self.restore_tools_from_transcript(&[]);
            if let Some(runner) = self.core.runner() {
                let new_leaf = self
                    .core
                    .with_session(|manager| manager.get_leaf_id().map(str::to_string));
                let _ = runner
                    .emit(ExtensionEvent::Session(SessionEvent::Tree(
                        SessionTreeEvent {
                            event_type: SessionTreeEventType::SessionTree,
                            new_leaf_id: new_leaf,
                            old_leaf_id: old_leaf_id.clone(),
                            summary_entry: summary_entry.as_ref().and_then(|entry| match entry {
                                SessionEntry::BranchSummary(summary) => Some(summary.clone()),
                                _ => None,
                            }),
                            from_extension: summary_entry.as_ref().map(|_| from_extension),
                        },
                    )))
                    .await;
            }
            Ok(TreeNavigationResult {
                editor_text,
                cancelled: false,
                aborted: None,
                summary_entry,
            })
        }
        .await;
        *lock(&self.core.branch_summary_abort) = None;
        self.core.resolve_idle_wait_if_idle();
        outcome
    }

    /// `getUserMessagesForForking()`。
    pub fn get_user_messages_for_forking(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        for entry in self.core.with_session(SessionManager::get_entries) {
            if let SessionEntry::Message(entry) = entry {
                if entry.message.role() != "user" {
                    continue;
                }
                let text = agent_message_text(&entry.message);
                if !text.is_empty() {
                    result.push((entry.id, text));
                }
            }
        }
        result
    }

    // -- 统计 / 上下文 ------------------------------------------------------

    /// `getSessionStats()`。
    pub fn get_session_stats(&self) -> SessionStats {
        self.core.get_session_stats_inner()
    }

    /// `getContextUsage()`。
    pub fn get_context_usage(&self) -> Option<kiana_ext_api::types::ContextUsage> {
        self.core.get_context_usage_inner()
    }

    /// `exportToHtml(outputPath?, options?)`。
    pub fn export_to_html(
        &self,
        output_path: Option<String>,
        options: Option<ExportThemeOptions>,
    ) -> Result<String, AgentSessionError> {
        let theme_name = options
            .as_ref()
            .and_then(|options| options.theme_name.clone())
            .filter(|name| kiana_theme::theme::get_theme_by_name(name).is_some())
            .or_else(|| {
                self.core
                    .settings_manager
                    .get_theme()
                    .filter(|name| kiana_theme::theme::get_theme_by_name(name).is_some())
            });
        let state = self.core.agent.state();
        let tools: Vec<kiana_export::ExportToolInfo> = state
            .tools
            .iter()
            .map(|tool| kiana_export::ExportToolInfo {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            })
            .collect();
        let export_state = kiana_export::ExportSessionState {
            system_prompt: Some(state.system_prompt),
            tools: Some(tools),
        };
        // TS 直接把**实时** SessionManager 交给导出：会话文件、名称、leaf 与持久化状态
        // 都来自它（内存会话则按 TS 报错）。同时构造扩展工具渲染器。
        let core = Arc::downgrade(&self.core);
        let runner = self.core.runner();
        let get_tool_renderers: kiana_export::GetToolRenderersFn = Arc::new(move |name: &str| {
            let base = || -> Option<kiana_ext_api::tool_types::ToolRenderers> {
                let core = core.upgrade()?;
                let definition = lock(&core.tool_definitions)
                    .get(name)
                    .map(|entry| entry.definition.clone())?;
                Some(kiana_ext_api::tool_types::ToolRenderers {
                    render_shell: definition.render_shell,
                    render_call: definition.render_call.clone(),
                    render_result: definition.render_result.clone(),
                })
            };
            match &runner {
                Some(runner) => runner.resolve_tool_renderers(name, &base),
                None => base(),
            }
        });
        let theme = kiana_theme::theme::current_theme()
            .or_else(|| kiana_theme::theme::get_theme_by_name("dark"))
            .ok_or_else(|| AgentSessionError::other("No theme available"))?;
        let mut renderer =
            kiana_export::create_tool_html_renderer(kiana_export::ToolHtmlRendererDeps {
                get_tool_renderers,
                theme,
                cwd: self.core.session_cwd(),
                width: None,
            });
        // 导出器会在同步渲染扩展工具时回调会话 API（`kiana.getSessionName()` 等）。
        // 如果此时仍持有 `core.session_manager` 的锁，回调会在同一线程再次加锁并死锁。
        // 导出是只读操作，先克隆一份会话快照、释放锁，再把快照交给导出器。
        let manager = {
            let guard = lock(&self.core.session_manager);
            guard.clone()
        };
        kiana_export::export_session_to_html(
            &manager,
            Some(&export_state),
            Some(kiana_export::ExportOptions {
                output_path,
                theme_name,
                tool_renderer: Some(&mut renderer),
            }),
        )
        .map_err(|error| AgentSessionError::other(error.to_string()))
    }

    /// `exportToJsonl(outputPath?)`。
    pub fn export_to_jsonl(
        &self,
        output_path: Option<String>,
    ) -> Result<String, AgentSessionError> {
        let guard = lock(&self.core.session_manager);
        export_session_to_jsonl(&guard, output_path.as_deref(), None)
            .map_err(|error| AgentSessionError::other(error.to_string()))
    }

    /// `summarizeForBugReport(options)`。
    pub async fn summarize_for_bug_report(
        &self,
        hint: Option<String>,
        signal: CancellationToken,
    ) -> Result<String, AgentSessionError> {
        let Some(model) = self.core.current_model() else {
            return Err(AgentSessionError::other("No model selected"));
        };
        let auth = self
            .core
            .summarization_request_auth(model, Some(signal.clone()))
            .await?;
        let stream_fn = self.core.agent.stream_function();
        kiana_resources::bug_report::generate_bug_report_summary(
            kiana_resources::bug_report::GenerateBugReportSummaryOptions {
                messages: &self.core.agent.messages(),
                hint,
                model: &auth.model,
                api_key: auth.api_key,
                headers: auth.headers,
                env: auth.env,
                signal: Some(signal),
                thinking_level: ai_thinking_level(auth.thinking_level),
                stream_fn: Some(stream_fn),
                retry: Some(self.core.retry_policy()),
                session_id: Some(self.core.session_id()),
            },
        )
        .await
        .map_err(|error| AgentSessionError::other(error.to_string()))
    }

    /// `getLastAssistantText()`。
    pub fn get_last_assistant_text(&self) -> Option<String> {
        self.core.get_last_assistant_text_inner()
    }

    /// `createReplacedSessionContext()`。
    pub fn create_replaced_session_context(
        &self,
    ) -> Arc<dyn kiana_ext_api::types::ReplacedSessionContextApi> {
        let inner = match self.core.runner() {
            Some(runner) => runner.create_command_context(),
            None => unreachable!("extension runner is installed"),
        };
        Arc::new(SessionReplacedContext {
            inner,
            core: Arc::downgrade(&self.core),
        })
    }
}

/// `exportToHtml` 的演示选项。
#[derive(Debug, Clone, Default)]
pub struct ExportThemeOptions {
    /// 主题名。
    pub theme_name: Option<String>,
}

/// 把 `ProviderConfig` 转成 kiana-config 的 `ProviderConfigInput`（尽力而为字段映射）。
fn provider_config_to_input(
    config: kiana_ext_api::types::ProviderConfig,
) -> kiana_config::models::composer::ProviderConfigInput {
    // 与 kiana-extensions 的 `provider_config_input` 等价：TS 原样转发整个 config，
    // 因此 models / streamSimple / refreshModels / images / classifiers 都必须保留。
    kiana_config::models::composer::ProviderConfigInput {
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
            .map(|models| models.iter().map(provider_model_config).collect::<Vec<_>>()),
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

/// 扩展 `ProviderModelConfig`（线格式）→ kiana-config 的组合器模型配置。TS 侧是
/// 直接转发同一个对象；Rust 两边是不同的具体类型，因此逐字段物化（三种变体各自映射）。
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

/// 把 `resources_discover` 事件返回的路径包装成资源加载器的扩展路径：来源标签取
/// 扩展名（合成路径去掉尖括号），非合成路径再带上父目录作为 `base_dir`；scope 固定
/// `Temporary`、origin 固定 `TopLevel`。
fn build_extension_resource_paths(
    entries: &[kiana_extensions::runner::ResourcePathEntry],
) -> Vec<ResourceExtensionPath> {
    entries
        .iter()
        .map(|entry| {
            let source = get_extension_source_label(&entry.extension_path);
            let base_dir = if is_synthetic_path(&entry.extension_path) {
                None
            } else {
                std::path::Path::new(&entry.extension_path)
                    .parent()
                    .map(|parent| parent.to_string_lossy().into_owned())
            };
            ResourceExtensionPath {
                path: entry.path.clone(),
                metadata: kiana_core::source_info::PathMetadata {
                    source,
                    scope: kiana_core::source_info::SourceScope::Temporary,
                    origin: kiana_core::source_info::SourceOrigin::TopLevel,
                    base_dir,
                    package_root: None,
                },
            }
        })
        .collect()
}

/// 扩展资源的来源标签（`SourceInfo.source`）：合成路径输出 `extension:<name>`（去尖括号），
/// 否则同样输出 `extension:<去后缀文件名>`（取路径文件名并剥掉 `.ts`/`.js` 后缀）——
/// 两个分支都以 `extension:` 为前缀。
fn get_extension_source_label(extension_path: &str) -> String {
    if is_synthetic_path(extension_path) {
        return format!("extension:{}", extension_path.replace(['<', '>'], ""));
    }
    let name = std::path::Path::new(extension_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name
        .strip_suffix(".ts")
        .or_else(|| name.strip_suffix(".js"))
        .unwrap_or(&name);
    format!("extension:{name}")
}

/// 会话替换后的命令上下文（`createReplacedSessionContext`）。
struct SessionReplacedContext {
    inner: Arc<dyn kiana_ext_api::types::ExtensionCommandContextApi>,
    core: Weak<Core>,
}

impl kiana_ext_api::types::ExtensionContextApi for SessionReplacedContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        self.inner.ui()
    }
    fn mode(&self) -> ExtensionMode {
        self.inner.mode()
    }
    fn has_ui(&self) -> bool {
        self.inner.has_ui()
    }
    fn cwd(&self) -> &std::path::Path {
        self.inner.cwd()
    }
    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        self.inner.session_manager()
    }
    fn model_registry(&self) -> &ModelRegistry {
        self.inner.model_registry()
    }
    fn model(&self) -> Option<Model> {
        self.inner.model()
    }
    fn scoped_models(&self) -> Vec<kiana_config::models::resolver::ScopedModel> {
        self.inner.scoped_models()
    }
    fn thinking_level(&self) -> Option<ModelThinkingLevel> {
        self.inner.thinking_level()
    }
    fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }
    fn is_project_trusted(&self) -> bool {
        self.inner.is_project_trusted()
    }
    fn signal(&self) -> Option<CancellationToken> {
        self.inner.signal()
    }
    fn abort(&self) {
        self.inner.abort();
    }
    fn has_pending_messages(&self) -> bool {
        self.inner.has_pending_messages()
    }
    fn shutdown(&self) {
        self.inner.shutdown();
    }
    fn get_context_usage(&self) -> Option<kiana_ext_api::types::ContextUsage> {
        self.inner.get_context_usage()
    }
    fn compact(&self, options: Option<CompactOptions>) {
        self.inner.compact(options);
    }
    fn get_system_prompt(&self) -> String {
        self.inner.get_system_prompt()
    }
}

impl kiana_ext_api::types::ExtensionCommandContextApi for SessionReplacedContext {
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.inner.get_system_prompt_options()
    }
    fn wait_for_idle(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        self.inner.wait_for_idle()
    }
    fn new_session(
        &self,
        options: Option<kiana_ext_api::types::NewSessionOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult> {
        self.inner.new_session(options)
    }
    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: Option<kiana_ext_api::types::ForkOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
        self.inner.fork(entry_id, options)
    }
    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: Option<kiana_ext_api::types::NavigateTreeOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
        self.inner.navigate_tree(target_id, options)
    }
    fn switch_session<'a>(
        &'a self,
        session_path: &'a str,
        options: Option<kiana_ext_api::types::SwitchSessionOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
        self.inner.switch_session(session_path, options)
    }
    fn reload(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        self.inner.reload()
    }
}

impl kiana_ext_api::types::ReplacedSessionContextApi for SessionReplacedContext {
    fn send_message(
        &self,
        message: SendMessageOptions,
    ) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        let future = self.send_message_checked(message);
        Box::pin(async move {
            let _ = future.await;
        })
    }
    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        let future = self.send_user_message_checked(content, options);
        Box::pin(async move {
            let _ = future.await;
        })
    }
    /// TS `createReplacedSessionContext` 直接挂会话方法：`await ctx.sendMessage()` 在失败时
    /// 必须 reject（例如压缩中、"Agent is already processing"、无 API key）。
    fn send_message_checked(
        &self,
        message: SendMessageOptions,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        let core = self.core.clone();
        Box::pin(async move {
            let Some(core) = core.upgrade() else {
                return Ok(());
            };
            let app_message = CustomMessage {
                role: kiana_session_core::messages::CustomRole::Custom,
                custom_type: message.message.custom_type,
                content: serde_json::to_value(&message.message.content)
                    .unwrap_or(serde_json::Value::Null),
                display: message.message.display,
                details: message.message.details,
                timestamp: now_ms() as f64,
            };
            core.send_custom_message(app_message, message.trigger_turn, message.deliver_as)
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn send_user_message_checked(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        let core = self.core.clone();
        Box::pin(async move {
            let Some(core) = core.upgrade() else {
                return Ok(());
            };
            core.send_user_message(content, options)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

impl Core {
    /// `getAllTools()`。
    fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.flush_pending_tool_refresh();
        // 先复制，避免在持有 `tool_definitions` 锁时调用 `tool_exposure`（同一把锁）。
        let definitions: Vec<(String, ToolDefinitionEntry)> = lock(&self.tool_definitions)
            .iter()
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect();
        definitions
            .into_iter()
            .map(|(name, entry)| ToolInfo {
                exposure: self.tool_exposure(&name),
                name: entry.definition.name.clone(),
                description: entry.definition.description.clone(),
                parameters: entry.definition.parameters.clone(),
                prompt_guidelines: entry.definition.prompt_guidelines.clone(),
                reaches_tools: entry.definition.reaches_tools.clone(),
                namespace: entry.definition.namespace.clone(),
                annotations: entry.definition.annotations.clone(),
                source_info: entry.source_info.clone(),
            })
            .collect()
    }

    /// `_rebuildSystemPrompt` 的 Core 版本（技能/上下文来自命令快照）。
    fn system_prompt_inner(&self) -> String {
        let options = lock(&self.run_system_prompt_options)
            .clone()
            .or_else(|| lock(&self.base_system_prompt_options).clone());
        match options {
            Some(options) => {
                build_system_prompt(&normalized_to_build_options(&options)).unwrap_or_default()
            }
            None => String::new(),
        }
    }

    /// `_refreshToolRegistry`。
    fn flush_pending_tool_refresh(&self) {
        if self.pending_tool_refresh.swap(false, Ordering::SeqCst) {
            self.refresh_tool_registry(None, false);
        }
    }

    /// 把工具注册表的变更落到 agent：重算定义表（内建 + SDK + 扩展，先过 allow/exclude
    /// 过滤）、归一化 snippet/guidelines、包装内建与扩展工具进 `tool_registry`，再按
    /// `active_tool_names` / allowlist / `include_all_extension_tools` / 待激活名单算出
    /// 下一个激活集合并调用 `set_active_tools`（其内部还会重建系统提示）。
    /// 注意与 `flush_pending_tool_refresh` 的关系：扩展派发期间的 `refreshTools()` 只置
    /// 标志（regression #5109），真正的重建推迟到这里的读取点。
    fn refresh_tool_registry(
        &self,
        active_tool_names: Option<Vec<String>>,
        include_all_extension_tools: bool,
    ) {
        self.pending_tool_refresh.store(false, Ordering::SeqCst);
        let previous_activated_on_registration: HashSet<String> = lock(&self.tool_registry)
            .keys()
            .filter(|name| self.is_activated_on_registration(name))
            .cloned()
            .collect();
        let previous_active_tool_names = self.get_active_tool_names();

        let registered_tools = self
            .runner()
            .map(|runner| runner.get_all_registered_tools())
            .unwrap_or_default();
        let mut all_custom_tools: Vec<RegisteredTool> = registered_tools
            .into_iter()
            .filter(|tool| self.is_allowed_tool(&tool.definition.name))
            .collect();
        for definition in &self.custom_tools {
            if self.is_allowed_tool(&definition.name) {
                all_custom_tools.push(RegisteredTool {
                    definition: definition.clone(),
                    source_info: create_synthetic_source_info(
                        &format!("<sdk:{}>", definition.name),
                        kiana_core::source_info::SyntheticSourceInfoOptions {
                            source: "sdk".to_string(),
                            ..Default::default()
                        },
                    ),
                });
            }
        }
        // 第 5 周：注册会话级 `skill` 工具。`SkillTool::invoke` 内部登记激活，
        // `permission_refusal_inner` 读取同一份台账应用 `allowed-tools`。
        // 扩展/SDK 已注册同名工具时不覆盖（扩展优先）。
        // 没有技能定义时不注册 `skill` 工具：避免给每个无技能会话的系统提示/tools
        // schema 增加无用的 token（也保持既有会话快照稳定）。
        if !lock(&self.skill_definitions).is_empty()
            && self.is_allowed_tool(SKILL_TOOL_NAME)
            && !all_custom_tools
                .iter()
                .any(|tool| tool.definition.name == SKILL_TOOL_NAME)
        {
            let skill_tool = self.ensure_skill_tool();
            all_custom_tools.push(RegisteredTool {
                definition: create_skill_tool_definition(skill_tool),
                source_info: create_synthetic_source_info(
                    &format!("{BUILTIN_PATH_PREFIX}{SKILL_TOOL_NAME}"),
                    kiana_core::source_info::SyntheticSourceInfoOptions {
                        source: "builtin".to_string(),
                        ..Default::default()
                    },
                ),
            });
        }

        let mut definition_registry: IndexMap<String, ToolDefinitionEntry> = IndexMap::new();
        for (name, definition) in lock(&self.base_tool_definitions).iter() {
            if self.is_allowed_tool(name) {
                // 表的键用模型可见名字（apply_patch 替换 edit 时不能再用键 `edit`），
                // 否则 `tool_exposure` / `is_declarable` 等按名字查找会落空。
                let visible_name = definition.name.clone();
                definition_registry.insert(
                    visible_name.clone(),
                    ToolDefinitionEntry {
                        definition: definition.clone(),
                        source_info: create_synthetic_source_info(
                            &format!("{BUILTIN_PATH_PREFIX}{visible_name}"),
                            kiana_core::source_info::SyntheticSourceInfoOptions {
                                source: "builtin".to_string(),
                                ..Default::default()
                            },
                        ),
                    },
                );
            }
        }
        for tool in &all_custom_tools {
            definition_registry.insert(
                tool.definition.name.clone(),
                ToolDefinitionEntry {
                    definition: tool.definition.clone(),
                    source_info: tool.source_info.clone(),
                },
            );
        }
        *lock(&self.tool_definitions) = definition_registry.clone();
        *lock(&self.tool_prompt_snippets) = definition_registry
            .values()
            .filter_map(|entry| {
                Self::normalize_prompt_snippet(entry.definition.prompt_snippet.as_ref())
                    .map(|snippet| (entry.definition.name.clone(), snippet))
            })
            .collect();
        *lock(&self.tool_prompt_guidelines) = definition_registry
            .values()
            .filter_map(|entry| {
                let guidelines =
                    Self::normalize_prompt_guidelines(entry.definition.prompt_guidelines.as_ref());
                (!guidelines.is_empty()).then(|| (entry.definition.name.clone(), guidelines))
            })
            .collect();

        let Some(runner) = self.runner() else {
            return;
        };
        let budget = self
            .artifact_store()
            .map(|store| OutputBudgetOptions { store });
        let apply_output_budget = |tool: AgentTool| match &budget {
            Some(budget) => wrap_agent_tool_with_output_budget(tool, budget.clone()),
            None => tool,
        };
        // 必须在 `wrap_registered_tools` 之后套：扩展包装负责工具上下文与注册，
        // 输出预算层是执行后的纯文本适配器，重复套会重复落盘。
        let wrapped_extension_tools: Vec<AgentTool> =
            wrap_registered_tools(&all_custom_tools, &runner)
                .into_iter()
                .map(apply_output_budget)
                .collect();
        let builtin_registered: Vec<RegisteredTool> = lock(&self.base_tool_definitions)
            .iter()
            .filter(|(name, _)| self.is_allowed_tool(name))
            .map(|(_, definition)| RegisteredTool {
                definition: definition.clone(),
                source_info: create_synthetic_source_info(
                    &format!("{BUILTIN_PATH_PREFIX}{}", definition.name),
                    kiana_core::source_info::SyntheticSourceInfoOptions {
                        source: "builtin".to_string(),
                        ..Default::default()
                    },
                ),
            })
            .collect();
        let mut wrapped_builtin_tools: Vec<AgentTool> =
            wrap_registered_tools(&builtin_registered, &runner)
                .into_iter()
                .map(apply_output_budget)
                .collect();
        // `bash background=true`：工具层不依赖会话，由这里注入后台任务能力。continuity
        // 关闭时仍包装，让 `background=true` 得到明确错误而不是静默前台执行。
        let background_manager = self
            .continuity
            .as_ref()
            .and_then(|continuity| continuity.background_manager())
            .cloned();
        let parent_session_id = self.session_id();
        for tool in &mut wrapped_builtin_tools {
            if tool.name == "bash" {
                let wrapped = crate::background::wrap_bash_background(
                    tool.clone(),
                    background_manager.clone(),
                    parent_session_id.clone(),
                );
                *tool = wrapped;
            }
        }
        *lock(&self.custom_tool_names) = wrapped_extension_tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let mut registry: IndexMap<String, AgentTool> = wrapped_builtin_tools
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect();
        for tool in &wrapped_extension_tools {
            registry.insert(tool.name.clone(), tool.clone());
        }
        *lock(&self.tool_registry) = registry;

        let had_active_tool_names = active_tool_names.is_some();
        let mut next_active_tool_names: Vec<String> = active_tool_names
            .unwrap_or_else(|| previous_active_tool_names.clone())
            .into_iter()
            .map(|name| self.canonical_tool_name(&name))
            .filter(|name| self.is_allowed_tool(name))
            .collect();
        if let Some(allowed) = &self.allowed_tools {
            let names: Vec<String> = lock(&self.tool_registry).keys().cloned().collect();
            for name in names {
                if allowed(&name) && self.is_declarable(&name) {
                    next_active_tool_names.push(name);
                }
            }
        } else if include_all_extension_tools {
            for tool in &wrapped_extension_tools {
                if self.is_activated_on_registration(&tool.name) {
                    next_active_tool_names.push(tool.name.clone());
                }
            }
        } else if !had_active_tool_names {
            let names: Vec<String> = lock(&self.tool_registry).keys().cloned().collect();
            for name in names {
                if !previous_activated_on_registration.contains(&name)
                    && self.is_activated_on_registration(&name)
                {
                    next_active_tool_names.push(name);
                }
            }
        }
        next_active_tool_names.extend(
            lock(&self.pending_tool_names)
                .iter()
                .map(|name| self.canonical_tool_name(name)),
        );
        let mut seen = HashSet::new();
        next_active_tool_names.retain(|name| seen.insert(name.clone()));
        self.set_active_tools(&next_active_tool_names);
    }

    /// `prepareNextTurn` 钩子：turn 之间先做阈值压缩检查，再跑前一层 prepare 钩子，
    /// 然后按当前激活工具重建 run 级系统提示选项并生成增量 system 消息（追加到
    /// `messages` 返回，供 agent-core 插入下一轮上下文）。
    async fn prepare_next_turn(
        &self,
        turn: PrepareNextTurnContext,
        signal: Option<CancellationToken>,
        previous: Option<kiana_agent_core::agent::AgentPrepareNextTurnWithContextFn>,
    ) -> Option<AgentLoopTurnUpdate> {
        let context = self
            .compact_before_next_assistant_response(turn.context)
            .await;
        let previous_snapshot = match &previous {
            Some(previous) => {
                previous(
                    PrepareNextTurnContext {
                        context: context.clone(),
                        ..turn
                    },
                    signal,
                )
                .await
            }
            None => None,
        };
        let next_context = previous_snapshot
            .as_ref()
            .and_then(|update| update.context.clone())
            .unwrap_or(context);
        let run_options = lock(&self.run_system_prompt_options)
            .clone()
            .or_else(|| lock(&self.base_system_prompt_options).clone());
        let Some(run_options) = run_options else {
            return previous_snapshot;
        };
        let options = self.normalize_build_options(&run_options, self.get_active_tool_names());
        let update_message = self.prepare_prompt_and_tool_loadout(&options, &next_context.messages);
        *lock(&self.run_system_prompt_options) = Some(options);
        let state = self.agent.state();
        Some(AgentLoopTurnUpdate {
            context: Some(AgentContext {
                messages: next_context.messages,
                tools: Some(state.tools),
            }),
            messages: match update_message {
                Some(message) => {
                    let mut messages = previous_snapshot
                        .as_ref()
                        .and_then(|update| update.messages.clone())
                        .unwrap_or_default();
                    messages.push(AgentMessage::Standard(Message::System(message)));
                    Some(messages)
                }
                None => previous_snapshot
                    .as_ref()
                    .and_then(|update| update.messages.clone()),
            },
            model: state.model,
            thinking_level: Some(state.thinking_level),
        })
    }

    /// `transformContext` 投影一：把 `hidden_declarations`（loadout hook 标记为隐藏的工具）
    /// 从 system 消息的 `tools_added`/`tools_removed` 里剔除，列表变空时键整体移除。
    async fn transform_hidden_declarations(
        &self,
        messages: Vec<AgentMessage>,
        signal: Option<CancellationToken>,
        previous: Option<kiana_agent_core::types::TransformContextFn>,
    ) -> Vec<AgentMessage> {
        let transformed = match &previous {
            Some(previous) => previous(messages, signal).await,
            None => messages,
        };
        let hidden = lock(&self.hidden_declarations).clone();
        if hidden.is_empty() {
            return transformed;
        }
        transformed
            .into_iter()
            .map(|message| match message {
                AgentMessage::Standard(Message::System(mut system))
                    if system.tools_added.is_some() || system.tools_removed.is_some() =>
                {
                    if let Some(added) = system.tools_added.as_mut() {
                        added.retain(|tool| !hidden.contains(&tool.name));
                    }
                    if let Some(removed) = system.tools_removed.as_mut() {
                        removed.retain(|tool| !hidden.contains(&tool.name));
                    }
                    if system.tools_added.as_ref().is_some_and(Vec::is_empty) {
                        system.tools_added = None;
                    }
                    if system.tools_removed.as_ref().is_some_and(Vec::is_empty) {
                        system.tools_removed = None;
                    }
                    AgentMessage::Standard(Message::System(system))
                }
                other => other,
            })
            .collect()
    }

    /// `transformContext` 投影二（在隐藏声明投影之后安装）：run 选项带 `force_system_prompt`
    /// 时，把强制提示作为首条 system 消息（沿用现有 system 的 `tools_added`/时间戳），
    /// 其余 system 消息全部丢弃。
    async fn transform_forced_prompt(
        &self,
        messages: Vec<AgentMessage>,
        signal: Option<CancellationToken>,
        previous: Option<kiana_agent_core::types::TransformContextFn>,
    ) -> Vec<AgentMessage> {
        let transformed = match &previous {
            Some(previous) => previous(messages, signal).await,
            None => messages,
        };
        let forced = lock(&self.run_system_prompt_options)
            .as_ref()
            .and_then(|options| options.force_system_prompt.clone());
        let Some(forced) = forced else {
            return transformed;
        };
        let llm: Vec<Message> = convert_to_llm(&transformed);
        let current = get_current_system_message(&llm);
        let head = SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(forced),
            sections: None,
            tools_added: current
                .as_ref()
                .and_then(|current| current.tools_added.clone()),
            tools_removed: None,
            timestamp: current
                .as_ref()
                .map(|current| current.timestamp)
                .unwrap_or_else(now_ms),
        };
        let mut result = vec![AgentMessage::Standard(Message::System(head))];
        result.extend(
            transformed
                .into_iter()
                .filter(|message| message.role() != "system"),
        );
        result
    }

    fn set_session_name_inner(&self, name: &str) {
        if let Err(error) = self.with_session_mut(|manager| manager.append_session_info(name)) {
            self.record_persistence_error(error);
            return;
        }
        let event = AgentSessionEvent::SessionInfoChanged {
            name: self.session_name(),
        };
        self.emit(event.clone());
        if let AgentSessionEvent::SessionInfoChanged { name } = event
            && let Some(runner) = self.runner()
        {
            spawn_local_event(
                runner,
                ExtensionEvent::Session(SessionEvent::InfoChanged(SessionInfoChangedEvent {
                    event_type: SessionInfoChangedEventType::SessionInfoChanged,
                    name,
                })),
            );
        }
    }

    fn refresh_current_model_from_registry(&self) {
        let Some(current) = self.current_model() else {
            return;
        };
        let refreshed = self.model_runtime.get_model(&current.provider, &current.id);
        if let Some(refreshed) = refreshed
            && refreshed != current
        {
            self.agent.set_model(refreshed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_does_not_override_parent_cancellation() {
        let cancelled = crate::subagent::WorkTerminal::Cancelled {
            reason: Some("user cancelled".to_string()),
        };
        assert!(matches!(
            apply_work_timeout(cancelled.clone(), true, true),
            crate::subagent::WorkTerminal::Cancelled { .. }
        ));
        assert!(matches!(
            apply_work_timeout(cancelled, true, false),
            crate::subagent::WorkTerminal::TimedOut { .. }
        ));
    }

    #[test]
    fn work_store_errors_are_not_dropped_from_failures() {
        let error = crate::subagent::SubagentError::Agent {
            message: "boom".to_string(),
        };
        let combined = attach_work_store_error(
            error,
            Some(crate::subagent::WorkStoreError::new("disk full")),
        );
        let text = combined.to_string();
        assert!(text.contains("boom"), "{text}");
        assert!(text.contains("disk full"), "{text}");
    }

    /// round-18 finding #2：失败路径必须先落定终态、再归还锚点。释放失败时记录
    /// 已经是一条非 Success 终态；`recover_work` 的锚点修复会按同一策略再次归还，
    /// 不会出现“Running 但没有锚点”的双可恢复窗口。
    #[tokio::test]
    async fn a_failed_release_after_finish_is_repaired_by_recovery() {
        use crate::subagent::WorkStore as _;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct ReleaseFailingStore {
            inner: crate::subagent::MemoryWorkStore,
            finished: AtomicBool,
        }

        #[async_trait::async_trait]
        impl crate::subagent::WorkStore for ReleaseFailingStore {
            async fn allocate_work_id(
                &self,
                parent_session_id: &str,
            ) -> Result<crate::subagent::WorkId, crate::subagent::WorkStoreError> {
                self.inner.allocate_work_id(parent_session_id).await
            }

            async fn begin(
                &self,
                record: crate::subagent::WorkRecord,
            ) -> Result<crate::subagent::WorkBegin, crate::subagent::WorkStoreError> {
                self.inner.begin(record).await
            }

            async fn attach_session(
                &self,
                parent_session_id: &str,
                work_id: crate::subagent::WorkId,
                run_id: &crate::subagent::RunId,
                session_file: &str,
            ) -> Result<(), crate::subagent::WorkStoreError> {
                self.inner
                    .attach_session(parent_session_id, work_id, run_id, session_file)
                    .await
            }

            async fn release_resume(
                &self,
                _parent_session_id: &str,
                _work_id: crate::subagent::WorkId,
                _run_id: &crate::subagent::RunId,
            ) -> Result<bool, crate::subagent::WorkStoreError> {
                Err(crate::subagent::WorkStoreError::new("release failed"))
            }

            async fn finish(
                &self,
                parent_session_id: &str,
                result: &crate::subagent::WorkResult,
            ) -> Result<crate::subagent::WorkFinish, crate::subagent::WorkStoreError> {
                self.finished.store(true, Ordering::SeqCst);
                self.inner.finish(parent_session_id, result).await
            }

            async fn load(
                &self,
                parent_session_id: &str,
                work_id: crate::subagent::WorkId,
            ) -> Result<Option<crate::subagent::WorkRecord>, crate::subagent::WorkStoreError>
            {
                self.inner.load(parent_session_id, work_id).await
            }

            async fn list_recoverable(
                &self,
                parent_session_id: &str,
            ) -> Result<Vec<crate::subagent::WorkRecord>, crate::subagent::WorkStoreError>
            {
                self.inner.list_recoverable(parent_session_id).await
            }

            async fn list_for_session(
                &self,
                parent_session_id: &str,
            ) -> Result<Vec<crate::subagent::WorkRecord>, crate::subagent::WorkStoreError>
            {
                self.inner.list_for_session(parent_session_id).await
            }
        }

        let store = std::sync::Arc::new(ReleaseFailingStore {
            inner: crate::subagent::MemoryWorkStore::new(),
            finished: AtomicBool::new(false),
        });
        let definition = crate::agent_definition::builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("builtin definition");
        let options = crate::subagent::WorkOptions {
            work_id: Some(crate::subagent::WorkId(1)),
            run_id: Some(crate::subagent::RunId::new("run-1")),
            recovery: crate::subagent::WorkRecovery::Resume,
            store: Some(store.clone() as std::sync::Arc<dyn crate::subagent::WorkStore>),
            ..Default::default()
        };
        let request = crate::subagent::build_work_request(
            &definition,
            &crate::subagent::SubagentRuntimeOptions::default(),
            "prompt",
            std::path::Path::new("/tmp"),
            &options,
        );
        // 账本里必须已有这条 Running 记录，`finish` 才能走到 release 分支。
        store
            .begin(
                crate::subagent::WorkRecord::new(request.clone(), "session", None, None, 1)
                    .with_resumed_from(crate::subagent::WorkId(99)),
            )
            .await
            .expect("begin");
        let clock: std::sync::Arc<dyn crate::subagent::WorkClock> =
            std::sync::Arc::new(crate::subagent::SystemWorkClock);
        let timed_out = AtomicBool::new(false);
        let mut seq = 0u64;
        let error = AgentSession::finish_work_outcome(
            &options,
            &request,
            "session",
            1,
            &clock,
            &mut seq,
            Err(crate::subagent::SubagentError::Agent {
                message: "boom".to_string(),
            }),
            &timed_out,
            None,
            None,
            Some(crate::subagent::WorkId(99)),
        )
        .await
        .expect_err("the original error must be returned");
        let text = error.to_string();
        assert!(text.contains("boom"), "{text}");
        assert!(text.contains("release failed"), "{text}");
        assert!(
            store.finished.load(Ordering::SeqCst),
            "the terminal must be committed before the anchor is released"
        );
        let repaired = store
            .load("session", crate::subagent::WorkId(1))
            .await
            .expect("load")
            .expect("record");
        assert!(repaired.is_terminal());
        assert_eq!(
            repaired.resumed_from,
            Some(crate::subagent::WorkId(99)),
            "the failed release leaves a non-success terminal, which recovery repairs"
        );
    }

    /// 回归（第六轮评审 finding #6）：失败路径的 `finish` 返回 `NotApplied`/`Missing`
    /// 时也必须上报，不能当成已经落账。
    #[test]
    fn finish_without_writing_a_terminal_is_reported_as_a_store_error() {
        assert!(work_finish_store_error(Ok(crate::subagent::WorkFinish::Missing)).is_some());

        let definition = crate::agent_definition::builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("builtin general-purpose");
        let request = crate::subagent::build_work_request(
            &definition,
            &crate::subagent::SubagentRuntimeOptions::default(),
            "prompt",
            std::path::Path::new("/tmp"),
            &crate::subagent::WorkOptions::default(),
        );
        let record = crate::subagent::WorkRecord::new(request, "session", None, None, 0);
        let error = work_finish_store_error(Ok(crate::subagent::WorkFinish::NotApplied(record)))
            .expect("not applied must be reported");
        assert!(error.to_string().contains("not applied"), "{error}");
    }

    #[test]
    fn staged_stage_budget_shares_the_work_level_limit() {
        assert_eq!(
            AgentSession::staged_stage_token_budget(None, Some(100), 40),
            Some(60)
        );
        assert_eq!(
            AgentSession::staged_stage_token_budget(Some(20), Some(100), 40),
            Some(20)
        );
        assert_eq!(
            AgentSession::staged_stage_token_budget(Some(80), Some(100), 40),
            Some(60)
        );
        assert_eq!(
            AgentSession::staged_stage_token_budget(None, Some(100), 100),
            Some(0)
        );
        assert_eq!(
            AgentSession::staged_stage_token_budget(Some(5), None, 40),
            Some(5)
        );
        assert_eq!(
            AgentSession::staged_stage_token_budget(None, None, 40),
            None
        );
    }

    #[test]
    fn equal_stage_and_work_deadlines_are_owned_by_the_work_timer() {
        let now = std::time::Instant::now();
        assert!(
            !AgentSession::stage_deadline_precedes_work(now, Some(now)),
            "an equal deadline must not give the stage timer ownership"
        );
        assert!(!AgentSession::stage_deadline_precedes_work(
            now,
            Some(now - std::time::Duration::from_secs(1))
        ));
        assert!(AgentSession::stage_deadline_precedes_work(
            now,
            Some(now + std::time::Duration::from_secs(1))
        ));
        assert!(AgentSession::stage_deadline_precedes_work(now, None));
    }

    #[test]
    fn stage_deadline_never_overrides_completed_or_work_cancellation() {
        use crate::subagent::SubagentTerminal;

        assert!(!AgentSession::stage_deadline_won(
            true,
            false,
            SubagentTerminal::Completed
        ));
        assert!(!AgentSession::stage_deadline_won(
            true,
            true,
            SubagentTerminal::Cancelled
        ));
        assert!(AgentSession::stage_deadline_won(
            true,
            false,
            SubagentTerminal::Cancelled
        ));
        assert!(AgentSession::stage_deadline_won(
            true,
            false,
            SubagentTerminal::Failed
        ));
        assert!(!AgentSession::stage_deadline_won(
            false,
            false,
            SubagentTerminal::Cancelled
        ));
    }

    fn sweep_test_record(
        work_id: i64,
        pid: Option<u32>,
        recovery: crate::subagent::WorkRecovery,
    ) -> crate::subagent::WorkRecord {
        let definition = crate::agent_definition::builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("builtin general-purpose");
        let options = crate::subagent::WorkOptions {
            work_id: Some(crate::subagent::WorkId(work_id)),
            run_id: Some(crate::subagent::RunId::new(format!("run-{work_id}"))),
            recovery,
            ..Default::default()
        };
        let request = crate::subagent::build_work_request(
            &definition,
            &crate::subagent::SubagentRuntimeOptions::default(),
            "prompt",
            std::path::Path::new("/tmp"),
            &options,
        );
        crate::subagent::WorkRecord::new(request, "session", None, pid, 100)
    }

    #[test]
    fn sweep_keep_set_is_bounded_after_a_successor_exists() {
        use crate::subagent::{WorkId, WorkRecovery, WorkResult};

        let mut anchor = sweep_test_record(1, Some(11), WorkRecovery::Resume);
        let result = WorkResult::interrupted(
            &anchor.request,
            "work interrupted: process lost",
            Some("child.jsonl".to_string()),
            anchor.created_at_ms,
            200,
        );
        terminalize(&mut anchor, result);

        let mut successor = sweep_test_record(2, Some(22), WorkRecovery::Fail);
        successor = successor.with_resumed_from(WorkId(1));
        let result = WorkResult::failure(&successor.request, "done", 100, 200);
        terminalize(&mut successor, result);

        let keep = AgentSession::sweep_keep_pids(&[anchor, successor]);
        assert!(
            keep.is_empty(),
            "a resumed anchor must not pin its workspace forever: {keep:?}"
        );
    }

    /// 回归（round-16 finding #1）：WorkId 是 per-parent-session 的。另一个会话里
    /// 同号的终态后继不能把本会话仍可恢复的 interrupted 记录判成“已恢复”。
    #[test]
    fn sweep_keep_set_scopes_anchors_to_the_parent_session() {
        use crate::subagent::{WorkId, WorkRecovery, WorkResult};

        let mut resumable = sweep_test_record(1, Some(11), WorkRecovery::Resume);
        let result = WorkResult::interrupted(
            &resumable.request,
            "work interrupted: process lost",
            Some("child.jsonl".to_string()),
            resumable.created_at_ms,
            200,
        );
        terminalize(&mut resumable, result);

        let mut foreign = sweep_test_record(2, None, WorkRecovery::Fail);
        foreign.parent_session_id = "other-session".to_string();
        foreign = foreign.with_resumed_from(WorkId(1));
        let result = WorkResult::failure(&foreign.request, "done", 100, 200);
        terminalize(&mut foreign, result);

        let keep = AgentSession::sweep_keep_pids(&[resumable, foreign]);
        assert!(
            keep.contains(&11),
            "another session's WorkId(1) must not anchor this session's record: {keep:?}"
        );
    }

    /// 回归（round-16 finding #10）：自定义隔离状态目录要记在 Work 记录里。崩溃发生在
    /// 首条消息落盘前时没有 session header，sweep 仍必须扫描该目录。
    #[test]
    fn sweep_state_dirs_include_a_declared_custom_state_dir_without_a_header() {
        let record = sweep_test_record(1, None, crate::subagent::WorkRecovery::Fail)
            .with_isolation_state_dir(std::path::PathBuf::from("/custom/isolation-state"));
        assert_eq!(
            AgentSession::sweep_state_dirs(&[record]),
            vec![std::path::PathBuf::from("/custom/isolation-state")]
        );
    }

    /// 回归（round-18 finding #9）：隔离 Work 的默认状态目录由声明的 workdir 推导。
    /// ownership 记录丢失或子会话 header 尚未落盘时，sweep 仍要扫到
    /// `<workdir>/.kiana/subagents`；Shared 不产生资源，不进扫描集。
    #[test]
    fn sweep_state_dirs_include_the_default_state_dir_of_a_declared_workdir() {
        use crate::subagent::{IsolationMode, WorkRecovery};

        let mut record = sweep_test_record(1, None, WorkRecovery::Fail);
        record.request.workdir = Some("/repo".to_string());
        record.request.isolation = Some(IsolationMode::Worktree);
        assert_eq!(
            AgentSession::sweep_state_dirs(&[record]),
            vec![std::path::PathBuf::from("/repo/.kiana/subagents")]
        );

        let shared = sweep_test_record(2, None, WorkRecovery::Fail);
        assert_eq!(shared.request.isolation, Some(IsolationMode::Shared));
        assert!(
            AgentSession::sweep_state_dirs(&[shared]).is_empty(),
            "Shared isolation creates no state dir to sweep"
        );
    }

    #[test]
    fn sweep_keep_set_keeps_the_current_workspace_owner_pid() {
        use crate::subagent::{
            IsolationBackend, IsolationMode, WorkId, WorkRecovery, WorkResult, WorkspaceOwnership,
            ownership_record_path,
        };

        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let workspace = state.join("clones").join("id-1");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(state.join("ownership")).expect("ownership dir");
        let ownership = WorkspaceOwnership {
            id: "id-1".to_string(),
            mode: IsolationMode::CowClone,
            backend: IsolationBackend::Reflink,
            original_root: temp.path().join("repo"),
            workspace_root: workspace.clone(),
            worktree_path: None,
            state_dir: state.clone(),
            git_root: None,
            branch: None,
            head: None,
            nested_repos: Vec::new(),
            created_at_ms: 1,
            // 一个不可能存活的 pid：模拟“resume 刷新了 ownership pid 后进程崩溃”。
            pid: Some(u32::MAX),
            retained: false,
        };
        let text = kiana_json::stringify(&kiana_json::normalize_numbers(
            serde_json::to_value(&ownership).expect("ownership json"),
        ));
        std::fs::write(ownership_record_path(&state, "id-1"), text).expect("ownership record");

        let session_file = temp.path().join("child.jsonl");
        std::fs::write(
            &session_file,
            serde_json::json!({
                "type": "session",
                "id": "child",
                "cwd": workspace.to_string_lossy(),
            })
            .to_string(),
        )
        .expect("session header");

        let mut anchor = sweep_test_record(1, Some(4242), WorkRecovery::Resume);
        let result = WorkResult::interrupted(
            &anchor.request,
            "work interrupted: process lost",
            Some(session_file.to_string_lossy().into_owned()),
            anchor.created_at_ms,
            200,
        );
        terminalize(&mut anchor, result);

        let keep = AgentSession::sweep_keep_pids(std::slice::from_ref(&anchor));
        assert!(keep.contains(&4242), "{keep:?}");
        assert!(
            keep.contains(&u32::MAX),
            "the ownership record's current pid must be kept even if the Work pid differs: {keep:?}"
        );
        assert_eq!(
            AgentSession::sweep_state_dirs(std::slice::from_ref(&anchor)),
            vec![state],
            "custom isolation state dirs must be swept"
        );

        // 有后继的 anchor 不再保留（finding 5 的上界）。
        let mut successor =
            sweep_test_record(2, None, WorkRecovery::Fail).with_resumed_from(WorkId(1));
        let result = WorkResult::failure(&successor.request, "done", 100, 200);
        terminalize(&mut successor, result);
        let keep = AgentSession::sweep_keep_pids(&[anchor, successor]);
        assert!(!keep.contains(&u32::MAX), "{keep:?}");
    }

    fn terminalize(record: &mut crate::subagent::WorkRecord, result: crate::subagent::WorkResult) {
        record.state = crate::subagent::work_store::WorkState::Terminal {
            result: Box::new(result),
            terminal_seq: 1,
        };
    }

    #[test]
    fn sweep_keep_set_includes_resumable_interrupted_records() {
        use crate::subagent::{WorkId, WorkRecovery, WorkResult};

        let running = sweep_test_record(1, Some(11), WorkRecovery::Resume);

        let mut resumable = sweep_test_record(2, Some(22), WorkRecovery::Resume);
        let result = WorkResult::interrupted(
            &resumable.request,
            "work interrupted: process lost",
            Some("child.jsonl".to_string()),
            resumable.created_at_ms,
            200,
        );
        terminalize(&mut resumable, result);

        // 不可恢复的 interrupted（recovery=fail，resumable=false）不保留工作区。
        let mut dead = sweep_test_record(3, Some(33), WorkRecovery::Fail);
        let result = WorkResult::interrupted(
            &dead.request,
            "work interrupted: process lost",
            Some("child.jsonl".to_string()),
            dead.created_at_ms,
            200,
        );
        terminalize(&mut dead, result);

        // 正常终态记录不保留。
        let mut finished = sweep_test_record(4, Some(44), WorkRecovery::Resume);
        let result = WorkResult::failure(&finished.request, "done", 100, 200);
        terminalize(&mut finished, result);

        // 仍被未终态后继引用的原记录要保留。
        let mut anchor = sweep_test_record(5, Some(55), WorkRecovery::Resume);
        let result = WorkResult::failure(&anchor.request, "done", 100, 200);
        terminalize(&mut anchor, result);
        let successor =
            sweep_test_record(6, None, WorkRecovery::Resume).with_resumed_from(WorkId(5));

        let keep =
            AgentSession::sweep_keep_pids(&[running, resumable, dead, finished, anchor, successor]);
        assert_eq!(keep, vec![11, 22, 55]);
    }

    #[test]
    fn permission_card_request_marks_dangerous_commands() {
        let dangerous = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "rm -rf ~/Documents"}),
            "/workspace",
        );
        let card =
            build_permission_card_request(&dangerous, "default mode asks".to_string(), Vec::new());
        assert!(
            card.dangerous,
            "destructive command must be marked dangerous"
        );

        let safe = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "git status"}),
            "/workspace",
        );
        let card =
            build_permission_card_request(&safe, "default mode asks".to_string(), Vec::new());
        assert!(!card.dangerous);
    }

    /// 回归（评审 finding #10）：`message_update` 的序列化键序必须是 TS 的
    /// `{ type, message, assistantMessageEvent }`。
    #[test]
    fn message_update_serializes_in_ts_key_order() {
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hi".to_string()),
                timestamp: 0,
            })),
            assistant_message_event: kiana_ai::types::AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "x".to_string(),
                partial: Box::new(AssistantMessage::default()),
            },
        };
        let value = serde_json::to_value(&event).expect("serialize");
        let keys: Vec<&str> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["type", "message", "assistantMessageEvent"]);
    }

    /// 回归（评审 finding #9）：`_normalizePromptSnippet` 用 JS 的 `/\s+/g` —— 该字符类
    /// 包含 U+FEFF 而**不包含** U+0085（NEL）；不能直接用 Rust regex 的 Unicode `\s`。
    #[test]
    fn normalize_prompt_snippet_uses_js_whitespace_class() {
        assert_eq!(
            Core::normalize_prompt_snippet(Some(&"a\u{0085}b\u{FEFF}c".to_string())),
            Some("a\u{0085}b c".to_string())
        );
        assert_eq!(
            Core::normalize_prompt_snippet(Some(&"a\r\nb\tc".to_string())),
            Some("a b c".to_string())
        );
        assert_eq!(
            Core::normalize_prompt_snippet(Some(&"   ".to_string())),
            None
        );
    }

    /// 回归（评审 finding #2）：命令期的 `registerProvider` 失败必须像 TS 一样“抛出”
    /// （Rust 侧为 panic，由 kiana-extensions runner 的 catch_item 转成同步 JS 异常），
    /// 而不是静默成功。
    #[tokio::test]
    async fn provider_registration_error_throws_like_ts() {
        let runtime = ModelRuntime::create(
            kiana_config::models::runtime::CreateModelRuntimeOptions::default(),
        )
        .await
        .expect("model runtime");
        let config = kiana_config::models::composer::ProviderConfigInput {
            stream_simple: Some(Arc::new(
                |_model: &Model,
                 _context: &kiana_ai::types::TranscriptContext,
                 _options: kiana_ai::types::SimpleStreamOptions| {
                    kiana_ai::event_stream::create_assistant_message_event_stream()
                },
            )),
            ..Default::default()
        };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match runtime.register_provider("test-provider", config) {
                Ok(()) => {}
                Err(error) => throw_provider_error(error),
            }
        }))
        .expect_err("registration error must throw");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .expect("string panic payload");
        assert_eq!(
            message,
            "Provider test-provider: \"api\" is required when registering streamSimple."
        );
    }

    fn failure(message: &str) -> AssistantMessage {
        AssistantMessage {
            stop_reason: StopReason::Error,
            error_message: Some(message.to_string()),
            ..AssistantMessage::default()
        }
    }

    #[test]
    fn provider_status_participates_in_retry_decisions() {
        assert_eq!(
            provider_status_from_message("Anthropic API error (429): slow down"),
            Some(429)
        );
        assert_eq!(
            provider_status_from_message(r#"{"status": 529, "message": "overloaded"}"#),
            Some(529)
        );
        assert_eq!(provider_status_from_message("HTTP 4000 tokens used"), None);
        assert_eq!(provider_status_from_message("no status here"), None);

        // 425/529 在 RETRYABLE_STATUS_CODES 里：即使文本正则不认识也要重试。
        assert!(assistant_error_is_retryable(
            &failure("API error (425): too early"),
            200_000
        ));
        assert!(assistant_error_is_retryable(
            &failure("API error (529): overloaded"),
            200_000
        ));
        // 501 / 401 不可重试。
        assert!(!assistant_error_is_retryable(
            &failure("API error (501): not implemented"),
            200_000
        ));
        assert!(!assistant_error_is_retryable(
            &failure("API error (401): unauthorized"),
            200_000
        ));
        // 上下文溢出永远不重试（走压缩路径）。
        assert!(!assistant_error_is_retryable(
            &failure("input exceeds the context length"),
            1_000
        ));
    }

    fn policy_model() -> Model {
        Model {
            id: "claude-test".to_string(),
            provider: "anthropic".to_string(),
            api: "anthropic-messages".to_string(),
            input: vec![kiana_ai::types::ModelInputModality::Text],
            context_window: Some(200_000),
            max_tokens: Some(8_192),
            ..Model::default()
        }
    }

    fn text_context(text: &str) -> kiana_ai::types::TranscriptContext {
        let context = kiana_ai::types::Context {
            system_prompt: None,
            messages: vec![kiana_ai::types::Message::User(
                kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text(text.to_string()),
                    timestamp: 0,
                },
            )],
            tools: None,
        };
        kiana_ai::utils::transcript::normalize_context(&context)
    }

    /// 第 9 周 finding #10：请求策略把输出预算写进 `max_tokens`，并为 Anthropic 方言
    /// 安装 cache 断点回调（在调用方 `onPayload` 之后生效）。
    #[tokio::test]
    async fn request_policy_writes_budget_and_cache_breakpoints() {
        let model = policy_model();
        let context = text_context("hello");
        let mut options = kiana_ai::types::SimpleStreamOptions::default();
        options.base.max_tokens = Some(4_096);
        options.base.on_payload = Some(Arc::new(|mut payload: serde_json::Value, _model| {
            payload["messages"][0]["content"][0]["text"] = serde_json::json!("rewritten-by-caller");
            Some(payload)
        }));

        apply_session_request_policy(&model, &context, &mut options).expect("policy passes");
        // caller 上限 4096 小于模型默认 8192，预算取 caller。
        assert_eq!(options.base.max_tokens, Some(4_096));
        assert!(
            options.base.on_payload_async.is_some(),
            "explicit cache semantics must install an async payload callback"
        );

        let payload = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": "hi" }]
            }]
        });
        let rewritten = options
            .base
            .apply_on_payload(payload, &model)
            .await
            .expect("payload");
        assert_eq!(
            rewritten["messages"][0]["content"][0]["text"],
            serde_json::json!("rewritten-by-caller"),
            "the caller callback must run first"
        );
        assert_eq!(
            rewritten["messages"][0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "cache breakpoints must be applied to the final wire payload"
        );
    }

    /// 能力门：上下文含图片而模型只声明文本时必须在发网络前返回 Unsupported。
    #[test]
    fn request_policy_rejects_vision_for_a_text_model() {
        let model = policy_model();
        let context =
            kiana_ai::types::TranscriptContext::from_normalized(kiana_ai::types::Context {
                system_prompt: None,
                messages: vec![kiana_ai::types::Message::User(
                    kiana_ai::types::UserMessage {
                        role: kiana_ai::types::UserRole::User,
                        content: kiana_ai::types::UserContent::Blocks(vec![
                            kiana_ai::types::UserContentBlock::Image(
                                kiana_ai::types::ImageContent {
                                    kind_image: kiana_ai::types::ImageKind::Image,
                                    data: "AAAA".to_string(),
                                    mime_type: "image/png".to_string(),
                                },
                            ),
                        ]),
                        timestamp: 0,
                    },
                )],
                tools: None,
            });
        let mut options = kiana_ai::types::SimpleStreamOptions::default();
        let error = apply_session_request_policy(&model, &context, &mut options)
            .expect_err("vision must be rejected");
        assert_eq!(error.kind, kiana_ai::AiErrorKind::Unsupported, "{error:?}");
        assert_eq!(options.base.max_tokens, None, "policy failed before budget");
    }
}
