//! `harness/types.ts`：Harness 的公开契约。
//!
//! TS 里大量“函数形接口 + 泛型重载”在 Rust 中落成 trait 与擦除后的参数：
//! - 泛型回调（`Session.commit<T>`、`TaskRuntime.memo<T>`）→ [`crate::types::SessionChange`]
//!   这类显式 trait + `JsonValue`/`Box<dyn Any>` 擦除；
//! - `ToolRegistration<TParameters, TDetails>` → 具体结构体 [`ToolRegistration`]，JSON schema
//!   与 details 都擦除成 `JsonValue`（运行期本来也没有泛型）；
//! - `WatchHandle`/`Draft` 仍用 `crate::types` 里的契约等价物（chord 的 `watch.ts` 是骨架）。
//!
//! 本文件同时定义 `view.ts`/`task-graph.ts`/`usage.ts`/`scheduler.ts` 的**公开数据形状**
//! （[`ConversationView`]、[`TaskGraph`]、[`UsageState`]、[`BlockedReason`]、
//! [`SchedulerOutcome`]），供这些单元直接 re-export，避免循环依赖。
//!
//! # 为什么集中在这里
//! 沿用 TS `harness/types.ts` 的单一契约文件布局：它被几乎所有 harness 子模块 import，Rust
//! 侧同样把 [`ConversationView`]、[`TaskGraph`]、[`UsageState`] 这些形状集中定义在本文件，
//! 便于与 TS 逐文件对照、统一再导出。实现文件只做 `pub use` 或直接 import（见
//! `harness/task_graph.rs`、`harness/usage.rs`、`harness/scheduler.rs` 的再导出），
//! `crate::index` 再把它们汇总成 TS 的公开面。
//!
//! # 边界与内容
//! 这里只有契约：结构体/枚举/trait/类型别名，加上少量纯转换（`into_draft`、`into_query`、
//! `SchedulerOutcome::into_task_outcome`）和两个构造期断言包装。没有 I/O、锁或后台任务；
//! 持久化在 `crate::session`，调度在 `harness/scheduler.rs`，工具执行在 `harness/tool.rs`。
//!
//! # 序列化约定（线格式）
//! - 结构体普遍 `#[serde(rename_all = "camelCase")]`，可选字段同时写 `default`（容忍旧记录
//!   缺字段）与 `skip_serializing_if = "Option::is_none"`（不写空字段）。
//! - `#[serde(untagged)]` 的联合体按**声明顺序**尝试解析：数组先落到 `List`，对象再落到
//!   `Edit`/`Remove`；`Edit` 的两个方向都可缺省，因此空对象 `{}` 会被解析成一次空编辑。
//! - 少数类型手写 `Serialize` 或依赖字段声明序，因为 TS 用对象字面量构造、`JSON.stringify`
//!   的键序会原样进入持久记录、工具结果与 UI（[`ToolDiagnostic`]、[`ContextView`]）。改动
//!   字段顺序或序列化实现前，先看锁定键序的测试：`ContextView` 见 `harness/context.rs` 的
//!   `a_missing_result_serializes_is_error_before_details`，`ToolDiagnostic` 见
//!   `tests/types.rs` 的 `tool_diagnostic_json_matches_ts_object_literal_order`。
//!
//! # 类型擦除的约定
//! [`AnyTaskDefinition`] 的 `Arc<dyn Any + Send + Sync>` 字段不是任意值：
//! - `harness/scheduler.rs` 的 `erase_task` 存放 `ErasedTaskPhase`（`phases` 与 `abort`）、
//!   `ErasedTaskMigrate`（`migrate`）和 `crate::session` 的 `ErasedTaskInitial`（`initial`）；
//!   `hooks` 直接存任务声明的 `Arc<H>`。调度器按这些类型 downcast，类型不符时返回 `Err`
//!   （`erased_phase`、`erased_abort`、`erased_migrate`），通常不会 panic。
//! - `harness/live.rs` 的 `erase_task_definition` 的 `phases`/`abort`/`initial` 符合上述
//!   约定，但 `migrate` 与 `hooks` 各多包了一层 `Arc`，实际存放
//!   `Arc<Arc<TaskMigrate<JsonValue, JsonValue>>>` 与 `Arc<Arc<H>>`；`erased_migrate` 只接受
//!   `ErasedTaskMigrate` 或裸的 `TaskMigrate<JsonValue, JsonValue>`，对多包一层的值 downcast
//!   会失败。这与该函数自己的契约注释不符，疑似实现缺陷，需另案修复；内建任务（如
//!   `harness/tool.rs` 的 `TOOL_TASK`）经它擦除，而 tool 任务带 `migrate: Some`。
//! - `initial` 的 downcast 发生在任务创建事务（`session/transaction.rs` 的
//!   `create_task_inner` 调用 `call_task_initial`），不在调度器。
//!
//! 手工构造定义必须复用同一批包装。
//!
//! # 阅读顺序
//! `AgentState`/`Agent`/`ToolRegistration`/`Settings` 解释“一个请求由什么组成”；
//! `Submission`/`Conversation`/`Harness` 是宿主 API；`TaskRuntime`/`ToolExecutionApi`/各 Hook
//! trait 是任务与工具代码看到的视图；最后是 `ConversationView`/`TaskGraph`/`UsageState` 这些
//! watch 输出的形状。

use std::any::Any;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::models::Models;
use kiana_ai::{
    AssistantMessage, CacheRetention, ClientIdentity, JsonObject, JsonValue, Message,
    ModelThinkingLevel, Tool, ToolCall, Transport, Usage, UserContent,
};
use kiana_chord::context::{AbortSignal, Context};
use kiana_telemetry::TelemetryContext;

use crate::env::{ExecutionEnv, ShellOutputSkip, ShellOutputWindow};
use crate::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, ConversationRecord, Cursor,
    DocumentObserver, DocumentReader, Entry, EntryData, EntryDraft, EntryId, EntryQuery,
    EntryRecord, JoinPolicy, Page, ReportInfo, SubmissionId, SubmissionRecord, SubmissionStatus,
    TaskAbortReason, TaskId, TaskOptions, TaskOutcome, TaskOutcomeError, TaskRecord, TaskState,
    TaskStatus, Tx, TypedEntry,
};

/// 从 `crate::types` 再导出的宿主回调、计时器与结果状态类型，保持 TS `types.ts` 的统一出口。
pub use crate::types::{
    HooksOf, HostCallback, ReportFrom, ReportTask, SetTimer, TaskOutcomeStatus,
};

// ---------------------------------------------------------------------------
// Model / agent
// ---------------------------------------------------------------------------

/// Provider 与模型 ID（TS: `ModelRef`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// Provider ID。
    pub provider: String,
    /// 模型 ID。
    pub model_id: String,
}

/// `UserMessage["content"]`。
pub type UserInput = UserContent;

/// 输入被放置成的条目；模型消息仍是输入自己的 user message。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputEntry {
    /// 应用条目 kind；不得以 `kiana.` 开头。
    ///
    /// 还必须非空；`admit_submission` 会调用 `check_input_entry` 校验，前缀保留给内建
    /// 文档/条目。
    pub kind: String,
    /// transcript 展示用的数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

/// 宿主提交：可能开始一次运行的输入，或被动的条目写入（TS: `SubmissionDraft`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionDraft {
    /// 宿主去重键。
    ///
    /// 同一会话里用一个 `request_id` 重复提交会返回已有提交；若两次的 kind 不同，
    /// `admit_submission` 会报错而不是复用。`None` 表示不去重。
    pub request_id: Option<String>,
    /// 负载。
    pub kind: SubmissionDraftKind,
}

/// `SubmissionDraft` 的判别部分。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmissionDraftKind {
    /// 用户输入。
    Input {
        /// 输入内容。
        content: UserInput,
        /// 忙碌时的处理。
        when_busy: Option<WhenBusy>,
        /// 放置成的条目。
        entry: Option<InputEntry>,
    },
    /// 被动条目写入。
    Write {
        /// 条目草稿。
        entry: EntryDraft,
    },
}

/// 忙碌时的策略。
///
/// 只对“会话正有一次运行”时的输入提交生效；空闲时三种值都不会触发 `ConversationBusy`，
/// 输入按正常放置流程处理——若 `kiana.inbox` 已有排队项，新输入仍可能保持 queued
/// （`harness/submissions.rs` 的 `admit_submission`）。`None` 与 `FollowUp` 等价（排进
/// follow-up 队列），`Steer` 走 steering 队列，`Reject` 让 `submit()` 返回
/// `ConversationBusy` 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WhenBusy {
    /// 转向当前运行。
    Steer,
    /// 排到本次运行之后。
    FollowUp,
    /// 直接拒绝。
    Reject,
}

/// `ConversationHandle.submit()` 接受的输入提交（TS: `InputSubmissionDraft`）。
#[derive(Debug, Clone, PartialEq)]
pub struct InputSubmissionDraft {
    /// 宿主去重键。
    ///
    /// 去重规则见 [`SubmissionDraft::request_id`]。
    pub request_id: Option<String>,
    /// 输入内容。
    pub content: UserInput,
    /// 忙碌时的处理。
    pub when_busy: Option<WhenBusy>,
    /// 放置成的条目。
    pub entry: Option<InputEntry>,
}

impl InputSubmissionDraft {
    /// 转成普通提交草稿。
    pub fn into_draft(self) -> SubmissionDraft {
        SubmissionDraft {
            request_id: self.request_id,
            kind: SubmissionDraftKind::Input {
                content: self.content,
                when_busy: self.when_busy,
                entry: self.entry,
            },
        }
    }
}

/// 已落定的提交记录（TS: `SettledSubmissionRecord = SubmissionRecord & { status: "done" | "unanswered" }`）。
///
/// Rust 的类型别名无法收窄字段，所以用一个只读包装承载这个不变量：只能由
/// [`SettledSubmissionRecord::new`] 从已落定的记录构造，字段经 [`Deref`](std::ops::Deref) 读取。
#[derive(Debug, Clone, PartialEq)]
pub struct SettledSubmissionRecord(SubmissionRecord);

impl SettledSubmissionRecord {
    /// 包装一条已落定的提交记录。
    ///
    /// # Panics
    /// 记录的 `status` 不是 `done`/`unanswered` 时（TS 的 `wait()` 只返回这两种）。
    pub fn new(record: SubmissionRecord) -> Self {
        assert!(
            matches!(
                record.status,
                SubmissionStatus::Done | SubmissionStatus::Unanswered
            ),
            "SettledSubmissionRecord requires a settled submission"
        );
        Self(record)
    }

    /// 原始记录。
    pub fn record(&self) -> &SubmissionRecord {
        &self.0
    }

    /// 取出记录。
    pub fn into_record(self) -> SubmissionRecord {
        self.0
    }
}

impl std::ops::Deref for SettledSubmissionRecord {
    type Target = SubmissionRecord;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// 已落定的任务记录（TS: `SettledTask<R> = TaskRecord<...> & { state: Extract<TaskState, { status: "terminal" }> }`）。
///
/// 同 [`SettledSubmissionRecord`]：只读包装 + 构造期断言，`state` 必为 `terminal`。
#[derive(Debug, Clone, PartialEq)]
pub struct SettledTask<R = JsonValue>(TaskRecord<JsonValue, JsonValue, R>);

impl<R> SettledTask<R> {
    /// 包装一条已提交的终态任务记录。
    ///
    /// # Panics
    /// `record.state` 不是 `terminal` 时。
    pub fn new(record: TaskRecord<JsonValue, JsonValue, R>) -> Self {
        assert!(
            matches!(record.state, TaskState::Terminal { .. }),
            "SettledTask requires a terminal task"
        );
        Self(record)
    }

    /// 包装一条记录而不校验终态（TS 在 `waitForTask()` 里直接把 Storage 读到的记录
    /// `as SettledTask` 返回，非终态也不会抛错）。
    ///
    /// 只供 `TaskScheduler::wait_for_task` 复刻该行为；[`outcome`](Self::outcome) 对非终态记录
    /// 仍会 panic，与 TS 里读到 `undefined` 的差异见该方法的说明。
    pub fn from_record_unchecked(record: TaskRecord<JsonValue, JsonValue, R>) -> Self {
        Self(record)
    }

    /// 原始记录。
    pub fn record(&self) -> &TaskRecord<JsonValue, JsonValue, R> {
        &self.0
    }

    /// 取出记录。
    pub fn into_record(self) -> TaskRecord<JsonValue, JsonValue, R> {
        self.0
    }

    /// 终态结果（`state.status` 为 `terminal` 这一不变量的直接结果）。
    pub fn outcome(&self) -> &TaskOutcome<R> {
        match &self.0.state {
            TaskState::Terminal { outcome } => outcome,
            // `new` 只接受 `terminal`；`Deref` 只给出共享借用，状态无法被改回。
            _ => panic!("SettledTask holds a terminal task"),
        }
    }
}

impl<R> std::ops::Deref for SettledTask<R> {
    type Target = TaskRecord<JsonValue, JsonValue, R>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// registry 里擦除后的可执行任务定义（TS: `AnyTask`）。
///
/// 身份由 `definition` 的 `Arc` 指针决定：调度器的 `same_task` 用 `Arc::ptr_eq` 判断两个
/// `AnyTask` 是否是同一定义（对应 TS 里 `===` 比较同一个定义对象）。因此重新 `erase_task`
/// 同一个类型化定义会得到新的身份。
#[derive(Clone)]
pub struct AnyTask {
    /// 定义。
    pub definition: Arc<AnyTaskDefinition>,
}

impl std::fmt::Debug for AnyTask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnyTask")
            .field("name", &self.definition.name)
            .field("version", &self.definition.version)
            .finish()
    }
}

/// TS `AnyTask["definition"]` 的 Rust 形状：`unknown` 字段保留为类型擦除的 `Arc<dyn Any>`。
///
/// # 擦除契约（消费方按此 downcast）
/// - `initial` 必须是 `crate::session` 的 `ErasedTaskInitial`（闭包输入/输出都是 `JsonValue`）；
///   任务创建事务（`session/transaction.rs` 的 `create_task_inner`）用 `call_task_initial`
///   调用它，类型不符返回
///   `task definition initial is not a kiana-durable ErasedTaskInitial`。
/// - `phases` 与 `abort` 的值必须都是 `ErasedTaskPhase`；阶段缺失或类型不符时该阶段以 io 错误
///   失败（错误文本与 TS 的 TypeError message 对齐，见 `erased_phase`）。
/// - `migrate` 可以是 `ErasedTaskMigrate`，也可以是裸的
///   `TaskMigrate<JsonValue, JsonValue>`；`erased_migrate` 两种都接受，其它类型报错。
/// - `hooks` 存放任务声明的 hook 表 `Arc<H>`（`erase_task` 的约定；`erase_task_definition`
///   实际多包一层 `Arc`，见模块文档）。
///
/// 调度器侧的 `ErasedTaskPhase` 与 `scheduler::ErasedTaskMigrate`（结构体）是 `pub(crate)`；
/// `crate::session::ErasedTaskInitial` 和 `crate::harness::live::ErasedTaskMigrate`（类型别名）
/// 是公开项，但仍属内部约定：外部代码不应手工填充 `Arc<dyn Any>`，而应通过 `erase_task` /
/// `erase_task_definition` 构造定义。
#[derive(Clone)]
pub struct AnyTaskDefinition {
    /// 注册名。
    pub name: String,
    /// 版本。
    pub version: i64,
    /// `initial`。
    ///
    /// 必须存放 `crate::session` 的 `ErasedTaskInitial`；任务创建事务用它把输入转换成
    /// checkpoint（见 `call_task_initial`）。
    pub initial: Arc<dyn Any + Send + Sync>,
    /// phase 处理器表。
    ///
    /// 键是 checkpoint 里的 phase 名；`IndexMap` 保留注册顺序。每个值必须是
    /// `ErasedTaskPhase`，调度器逐阶段按名字取出。
    pub phases: IndexMap<String, Arc<dyn Any + Send + Sync>>,
    /// abort 处理器。
    ///
    /// 与 `phases` 使用同一种 `ErasedTaskPhase` 包装；记录带 abort 标记时调度器以 abort
    /// 调用模式运行它。
    pub abort: Arc<dyn Any + Send + Sync>,
    /// `migrate`。
    ///
    /// 可为 `ErasedTaskMigrate` 或 `TaskMigrate<JsonValue, JsonValue>`，`erased_migrate`
    /// 两种都接受；定义版本与记录不兼容且没有可用迁移时，任务被判定为阻塞
    /// （见 [`BlockedReason`]）。
    pub migrate: Option<Arc<dyn Any + Send + Sync>>,
    /// hook 处理器表。
    ///
    /// 存放任务定义声明的 `Arc<H>`；`H` 是任务自己的 hook 类型，只有任务代码知道如何解释。
    pub hooks: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for AnyTaskDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnyTaskDefinition")
            .field("name", &self.name)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// `Conversation.abort()` 的选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConversationAbortOptions {
    /// 跨 background 边界。
    ///
    /// `None`/`Some(false)` 只中止本会话普通所有权范围内的任务，background 任务不动；
    /// `Some(true)` 会继续向下走 background 子树，并对每个被中止的任务等待其终态后才返回
    /// （`harness/scheduler.rs` 的 `abort_conversation`）。
    pub background: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool and prompt shapes
// ---------------------------------------------------------------------------

/// 工具结果请求的 post-tools 控制。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolControl {
    /// 追加给下一轮的工具名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_tools: Option<Vec<String>>,
    /// 本轮所有结果都为 `true` 时结束运行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    /// 用这段文本作为新上下文的 user message。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
}

/// 诊断等级。
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    /// 信息。
    #[default]
    Info,
    /// 警告。
    Warn,
    /// 错误。
    Error,
}

/// 一次调用的模型/UI 备注（TS: `ToolDiagnostic`）。
///
/// **键序**：TS 的所有构造点都写 `{ severity, code, message }`（`tools/read.ts` 的一条
/// 无 code 诊断写 `{ severity, message }`），`JSON.stringify` 会把这个顺序带进工具结果的
/// `data.diagnostics`（展示给模型与 UI）。字段声明顺序即此顺序。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiagnostic {
    /// 等级。
    #[serde(default)]
    pub severity: DiagnosticSeverity,
    /// 稳定代码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// 消息。
    #[serde(default)]
    pub message: String,
}

/// 工具返回的三个通道（TS: `ToolExecutionResult`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionResult {
    /// 给模型的内容；缺省用 `output()` 保留的文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<kiana_ai::UserContentBlock>>,
    /// 给程序的结构化结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<JsonValue>,
    /// 是否是错误结果。
    #[serde(rename = "isError", default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// 给 UI 的 details。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// 额外诊断，追加在 `api.diagnostic()` 之后。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// 执行本身的 spend。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// post-tools 控制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolControl>,
}

/// 嵌套调用的结果（TS: `NestedToolExecutionResult`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolExecutionResult {
    /// 嵌套调用的工具任务。
    pub task_id: TaskId,
    /// 给程序的值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<JsonValue>,
    /// 是否是错误结果。
    #[serde(rename = "isError")]
    pub is_error: bool,
    /// details。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// 诊断。
    #[serde(default)]
    pub diagnostics: Vec<ToolDiagnostic>,
    /// 本次执行的 spend。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 产生该结果的尝试耗时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

/// 一轮工具是并行执行还是按调用顺序串行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// 并行。
    Parallel,
    /// 串行。
    Sequential,
}

/// 一种模式的边界放置多少排队项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum QueueMode {
    /// 全部。
    #[serde(rename = "all")]
    All,
    /// 一次一个。
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// 工具输出的一块：`string | Uint8Array`。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    /// 文本。
    Text(String),
    /// 原始字节。
    Bytes(Vec<u8>),
}

/// 已保留的输出与其是否被截断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedOutput {
    /// 模型将看到的文本。
    pub text: String,
    /// 是否丢弃了更早的输出。
    pub truncated: bool,
}

/// 工具输出限制。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputLimits {
    /// 最大字节数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// 最大行数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    /// 保留头部还是尾部。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<OutputRetain>,
}

/// `OutputLimits.retain`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputRetain {
    /// 头部。
    Head,
    /// 尾部。
    Tail,
}

/// 谁能调用工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCaller {
    /// 模型。
    Model,
    /// 其他工具。
    Tools,
}

/// 重放策略。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ReplayPolicy {
    /// 中断后可安全重跑。
    Safe,
    /// 默认：不重跑。
    #[default]
    Unsafe,
}

/// 工具执行的擦除闭包。
///
/// 参数依次是工具参数（已修复/校验后的 JSON）、本次调用的 `ToolExecutionApi`、chord 的取消
/// `Context`；返回的 future 必须 `Send`。实现返回 `Err` 时，`harness/tool.rs` 把它折算成
/// 工具错误结果并把整个调用标为失败（而不是让进程失败）；被取消时错误以调用自己的 abort
/// 信号为准。
pub type ToolExecuteFn = Arc<
    dyn Fn(
            JsonValue,
            Arc<dyn ToolExecutionApi>,
            Arc<dyn Context>,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
        > + Send
        + Sync,
>;

/// registry 里的工具（TS: `ToolRegistration`）。
///
/// `parameters` 与 details 在运行期本来就是 JSON；泛型 `TParameters`/`TDetails` 只约束 TS 调用点。
///
/// `tool` 是发给 provider 的 kiana-ai 定义（name/description/parameters），其余字段是 Harness
/// 侧的扩展点：`callers`/`deferred` 决定工具进入 [`Agent::tools`] 还是 [`Agent::deferred`]，
/// `execute` 是实际执行体，`prepare_arguments`/`structured_output_schema`/`output_limits`
/// 由 `harness/tool.rs` 在参数校验、结构化输出检查与输出截断时读取。
pub struct ToolRegistration {
    /// kiana-ai 工具字段：name/description/parameters。
    pub tool: Tool,
    /// 中断执行的重放策略；默认 [`ReplayPolicy::Unsafe`]。
    pub replay: Option<ReplayPolicy>,
    /// 执行模式；缺省用 settings 的 `toolExecution`。
    pub execution_mode: Option<ToolExecutionMode>,
    /// 参数修复。
    ///
    /// 在 schema 校验之前调用；修复函数返回 `Err` 或非对象时，调用以
    /// `invalid_arguments` 工具错误结束，不会执行 `execute`。
    pub prepare_arguments:
        Option<Arc<dyn Fn(&JsonValue) -> Result<JsonValue, BoxError> + Send + Sync>>,
    /// `structuredOutput` 的 schema。
    ///
    /// 嵌套调用没有 schema 时，程序拿到的 `structuredOutput` 由工具输出派生（单个文本块是
    /// 字符串、多个块是数组，见 `output_value`）；有 schema 时结果必须通过校验，缺失或不符
    /// 都在结算时报告（`structured_output_error`）。
    pub structured_output_schema: Option<JsonValue>,
    /// 允许的调用者。
    ///
    /// `None` 表示模型与工具两种调用者都允许；`Some` 时按 [`ToolCaller`] 过滤：不允许模型的
    /// 工具不进入 `tools`/`deferred`，不允许工具的不进入 `callable`（`resolve_agent`）。
    pub callers: Option<Vec<ToolCaller>>,
    /// 是否延迟加载。
    ///
    /// 只影响模型侧：延迟工具默认不出现在请求里，需要 [`AgentState::model_tools`] 的 `add`
    /// 或显式列表才会提供（`offered_tools`）；工具侧的 `callable` 不受影响。
    pub deferred: bool,
    /// 输出限制。
    ///
    /// 缺省用 `DEFAULT_MAX_LINES`/`DEFAULT_MAX_BYTES`，`retain` 缺省为头部保留；被截断时
    /// 生成警告诊断，但工具输出本身不算错误。
    pub output_limits: Option<OutputLimits>,
    /// 执行体。
    ///
    /// `None` 表示没有实现：调用会得到 `Tool implementation is missing` 的工具错误结果，并
    /// 把这次调用标为失败（`harness/tool.rs`），不会 panic。
    pub execute: Option<ToolExecuteFn>,
}

impl Clone for ToolRegistration {
    fn clone(&self) -> Self {
        ToolRegistration {
            tool: self.tool.clone(),
            replay: self.replay,
            execution_mode: self.execution_mode,
            prepare_arguments: self.prepare_arguments.clone(),
            structured_output_schema: self.structured_output_schema.clone(),
            callers: self.callers.clone(),
            deferred: self.deferred,
            output_limits: self.output_limits.clone(),
            execute: self.execute.clone(),
        }
    }
}

impl std::fmt::Debug for ToolRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolRegistration")
            .field("name", &self.tool.name)
            .field("replay", &self.replay)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

impl ToolRegistration {
    /// 一个只有 kiana-ai 字段的工具。
    pub fn new(tool: Tool) -> Self {
        ToolRegistration {
            tool,
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            structured_output_schema: None,
            callers: None,
            deferred: false,
            output_limits: None,
            execute: None,
        }
    }

    /// 工具名。
    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

/// 系统提示分段渲染的输入（TS: `PromptInput`）。
pub struct PromptInput {
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 本次请求解析出的 agent。
    pub agent: Arc<Agent>,
    /// 为此请求构建的环境。
    pub env: Option<Arc<dyn ExecutionEnv>>,
    /// 回放活动转录后已生效的分段。
    pub shown: IndexMap<String, String>,
    /// 已提交的文档读取。
    pub read: Arc<dyn DocumentReader>,
}

/// 一段系统提示（TS: `PromptSection`）。
///
/// 分段在 `resolve_agent` 里按 `key` 合并：同名分段后一个扩展覆盖前一个，但保留首次出现的
/// 位置；包装器也以 `key` 选择目标。渲染由 `harness/prompt.rs` 的 `render_sections` 驱动，
/// 结果随后经 `plan_system_entries` 写进 `kiana.system` 条目。
#[async_trait]
pub trait PromptSection: Send + Sync {
    /// 稳定的 key。
    ///
    /// 同一 agent 内应唯一：`resolve_agent` 用它在合并时去重/覆盖，渲染结果也按键保存与
    /// 打补丁；不同分段共用 key 会导致后一个替换前一个。
    fn key(&self) -> &str;
    /// 渲染；`None` 表示不显示。
    ///
    /// 返回 `Err` 不会让整个提示失败：非 abort 的错误由 `render_sections` 报告，并保留上一版
    /// 该 key 的文本（若之前渲染过）；调用方上下文已 abort 时错误向上传播。
    async fn render(
        &self,
        input: &PromptInput,
        context: &dyn Context,
    ) -> Result<Option<String>, BoxError>;
    /// 默认 true：包成 `<key>\n...\n</key>`。
    ///
    /// 只有 `Some(false)` 表示“原样输出、不加标签”；`None` 与 `Some(true)` 都会加标签
    /// （`render_sections` 判断 `tag() == Some(false)`）。
    fn tag(&self) -> Option<bool> {
        None
    }
}

/// registry 里的 hook 声明（TS: `HookRegistration`）。
#[derive(Clone)]
pub struct HookRegistration {
    /// 任务名。
    pub task: String,
    /// 处理器表（类型擦除；任务定义把它 downcast 回自己的 H）。
    pub handlers: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for HookRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookRegistration")
            .field("task", &self.task)
            .finish_non_exhaustive()
    }
}

/// 包装器（TS: `Wrap`）：纯函数，作用于一个工具或一个分段。
///
/// 目标不存在时什么都不做。包装器 panic 或返回改名后的对象时，`resolve_agent` 会移除该
/// 目标并通过 `report` 上报错误，其余工具/分段继续解析（`harness/agent.rs` 的 `apply_wrap`）；
/// 因此包装器不能依赖“失败后目标仍存在”。
#[derive(Clone)]
pub enum Wrap {
    /// 包装一个指定名字的工具。
    Tool {
        /// 工具名。
        tool: String,
        /// 包装函数。
        wrap: Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
    },
    /// 包装一个指定 key 的分段。
    Section {
        /// 分段 key。
        section: String,
        /// 包装函数。
        wrap: Arc<dyn Fn(Arc<dyn PromptSection>) -> Arc<dyn PromptSection> + Send + Sync>,
    },
}

impl std::fmt::Debug for Wrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wrap::Tool { tool, .. } => formatter
                .debug_struct("Wrap::Tool")
                .field("tool", tool)
                .finish(),
            Wrap::Section { section, .. } => formatter
                .debug_struct("Wrap::Section")
                .field("section", section)
                .finish(),
        }
    }
}

/// 扩展：具名的代码包（TS: `Extension`）。
///
/// 扩展名是 registry 里的身份：`install` 用同名扩展原位替换。下面几个集合方法会在构造
/// registry 快照、解析 agent、查找任务归属等时机被直接调用，调用点不缓存返回值。
pub trait Extension: Send + Sync {
    /// 扩展名。
    fn name(&self) -> &str;
    /// 工具。
    fn tools(&self) -> Vec<ToolRegistration> {
        Vec::new()
    }
    /// 系统提示分段。
    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        Vec::new()
    }
    /// hook 声明。
    fn hooks(&self) -> Vec<HookRegistration> {
        Vec::new()
    }
    /// 包装器。
    fn wraps(&self) -> Vec<Wrap> {
        Vec::new()
    }
    /// 任务定义。
    fn tasks(&self) -> Vec<AnyTask> {
        Vec::new()
    }
}

/// registry 快照里的一条工具。
#[derive(Clone)]
pub struct RegistryTool {
    /// 所属扩展。
    pub extension: Arc<dyn Extension>,
    /// 工具。
    pub tool: ToolRegistration,
}

/// registry 快照里的一段提示。
#[derive(Clone)]
pub struct RegistrySection {
    /// 所属扩展。
    pub extension: Arc<dyn Extension>,
    /// 分段。
    pub section: Arc<dyn PromptSection>,
}

/// 一份已发布的不可变 registry 视图（TS: `RegistrySnapshot`）。
pub trait RegistrySnapshot: Send + Sync {
    /// 已安装的扩展。
    fn installed(&self) -> Vec<Arc<dyn Extension>>;
    /// 按名字取扩展。
    fn extension(&self, name: &str) -> Option<Arc<dyn Extension>>;
    /// 每个已安装工具及其扩展，按安装顺序。
    fn tools(&self) -> Vec<RegistryTool>;
    /// 每段提示及其扩展。
    fn sections(&self) -> Vec<RegistrySection>;
    /// 内建与已安装的任务定义。
    fn tasks(&self) -> Vec<AnyTask>;
    /// 按名字取任务。
    fn task(&self, name: &str) -> Option<AnyTask>;
}

/// Harness 消费的 registry 读端（TS: `RegistryReader`）。
pub trait RegistryReader: Send + Sync {
    /// 当前 registry 的不可变视图。
    ///
    /// 返回的是发布时构造的快照；之后 `install`/`uninstall` 只会换掉当前快照，已拿到的
    /// `Arc` 内容不变。调度器在每次解析 agent 时重新取快照。
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot>;
    /// 每次发布后同步调用；唤醒调度器重新考虑 blocked 任务。
    ///
    /// 监听器在持有发布锁之外同步调用（`harness/registry.rs` 的 `publish`），因此不要在
    /// 监听器里做长耗时工作。同一个 `Arc` 监听器重复注册是空操作；返回的 `Unsubscribe`
    /// 负责移除监听，registry 已被释放时是空操作。
    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> crate::types::Unsubscribe;
}

/// 应用拥有的扩展 registry（TS: `Registry`）。
pub trait Registry: RegistryReader {
    /// 安装扩展，或按名字原位替换。
    ///
    /// 同名扩展替换后保持原来的安装位置。校验失败（同一扩展内工具重名、分段 key 非法或重复、
    /// 占用保留的 instructions key、跨扩展任务重名）时 `install` 没有错误通道，而是以
    /// `panic_any` 抛出错误对象（调用方可用 `catch_unwind` 取回），见 `harness/registry.rs`
    /// 的 `validate_extension` 与 `panic_js`。
    fn install(&self, extension: Arc<dyn Extension>);
    /// 按名字卸载。
    ///
    /// 名字不存在是空操作；发布失败与 `install` 一样以 panic 形式抛出。
    fn uninstall(&self, name: &str);
}

/// 一个会话存储的 agent 选择（TS: `AgentState`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    /// 模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// 思考等级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 扩展选择。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionSelection>,
    /// 已启用工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSelection>,
    /// 提供给模型的工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_tools: Option<ModelToolSelection>,
    /// 追加在扩展分段之后的说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// 环境文件系统的工作目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `AgentState.extensions`：精确列表或对宿主默认选择做增量编辑。
///
/// `#[serde(untagged)]` 按声明顺序解析：JSON 数组落到 `List`，对象落到 `Edit`。`Edit` 的
/// `add`/`remove` 都可缺省，所以 `{}` 也是一次合法的空编辑。名字在解析时不去 registry
/// 校验，缺失的扩展在 `resolve_agent` 里被跳过（等它安装后重新解析即可生效）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ExtensionSelection {
    /// 恰好这些扩展，按顺序。
    List(Vec<String>),
    /// 增量编辑。
    Edit {
        /// 追加。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<Vec<String>>,
        /// 移除。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
    },
}

/// `AgentState.tools`：精确列表或移除集合。
///
/// 数组→`List`，对象→`Remove`（`remove` 必填，因此 `{}` 不能解析）。`List` 按存储的名字
/// 顺序输出、重复名只取第一次、未知名字跳过；`Remove` 保持工具原顺序（`filter_tools`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ToolSelection {
    /// 恰好这些。
    List(Vec<String>),
    /// 从默认集合移除。
    Remove {
        /// 移除。
        remove: Vec<String>,
    },
}

/// `AgentState.modelTools`：精确列表或增量编辑。
///
/// 数组→`List`（精确提供，显式列出的延迟工具也会提供）；对象→`Edit`（`add`/`remove`
/// 都可缺省，`{}` 是空编辑）。`Edit` 的 `add` 只加载**延迟**工具，非延迟名字不起作用；
/// `remove` 对默认集合与 `add` 都生效（`offered_tools`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ModelToolSelection {
    /// 恰好这些。
    List(Vec<String>),
    /// 增量编辑。
    Edit {
        /// 移除。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
        /// 追加（仅延迟工具）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<Vec<String>>,
    },
}

/// 对 `kiana.agent` 的一次变更：缺省不动，`Clear` 清空，`Set` 替换。
///
/// 标量字段用双层 `Option` 表达三种意图：`None` 不动、`Some(None)` 删除（`null`）、
/// `Some(Some(v))` 替换。扩展与工具变更只把**名字**写进文档，不校验是否已安装/存在；
/// 解析留到 `resolve_agent`（`harness/agent.rs` 的 `apply_change`）。
#[derive(Clone, Default)]
pub struct AgentChange {
    /// 模型。
    pub model: Option<Option<ModelRef>>,
    /// 思考等级。
    pub thinking_level: Option<Option<ModelThinkingLevel>>,
    /// 扩展。
    pub extensions: Option<ExtensionChange>,
    /// 工具。
    pub tools: Option<ToolChange>,
    /// 提供给模型的工具。
    pub model_tools: Option<ModelToolChange>,
    /// 说明。
    pub instructions: Option<Option<String>>,
    /// 工作目录。
    pub cwd: Option<Option<String>>,
}

/// `AgentChange.extensions`。
#[derive(Clone)]
pub enum ExtensionChange {
    /// 清空（`null`）。
    Clear,
    /// 替换成这些扩展。
    Set(Vec<Arc<dyn Extension>>),
    /// 增量编辑。
    Edit {
        /// 追加。
        add: Option<Vec<Arc<dyn Extension>>>,
        /// 移除。
        remove: Option<Vec<Arc<dyn Extension>>>,
    },
}

/// `AgentChange.tools`。
#[derive(Clone)]
pub enum ToolChange {
    /// 清空。
    Clear,
    /// 替换。
    Set(Vec<ToolRegistration>),
    /// 移除。
    Remove { remove: Vec<ToolRegistration> },
}

/// `AgentChange.modelTools`。
#[derive(Clone)]
pub enum ModelToolChange {
    /// 清空。
    Clear,
    /// 替换。
    Set(Vec<ToolRegistration>),
    /// 增量编辑。
    Edit {
        /// 移除。
        remove: Option<Vec<ToolRegistration>>,
        /// 追加。
        add: Option<Vec<ToolRegistration>>,
    },
}

/// 会话解析后的 agent（TS: `Agent`）。
///
/// 由 `resolve_agent` 从 `kiana.agent` 文档、registry 快照与解析后的设置算出；结果是快照，
/// registry 或文档变化后必须重新解析。顺序保证：扩展顺序决定工具/分段/包装器的合并顺序
/// （同名后一个覆盖前一个但保留首次位置），`tools` 的数组形式按存储名字顺序，延迟工具按
/// `modelTools.add` 的顺序追加，`instructions` 永远排在扩展分段之后。
#[derive(Clone)]
pub struct Agent {
    /// 模型。
    pub model: Option<ModelRef>,
    /// 思考等级。
    pub thinking_level: ModelThinkingLevel,
    /// 选中的扩展。
    pub extensions: Vec<Arc<dyn Extension>>,
    /// 请求提供给模型的工具。
    pub tools: Vec<ToolRegistration>,
    /// 嵌套调用可解析的工具。
    ///
    /// 受存储的 `AgentState.tools` 选择与 `callers` 过滤，不受 `modelTools` 影响；工具任务用
    /// 它解析 `executeTool()` 的目标。
    pub callable: Vec<ToolRegistration>,
    /// 已启用但未提供的延迟工具。
    ///
    /// 判定条件：`callers` 允许模型、`deferred` 为真、且最终没有被 `modelTools` 提供
    /// （既不在默认集合也不在 `add` 里）。供上层知道还有哪些延迟工具可加载。
    pub deferred: Vec<ToolRegistration>,
    /// 扩展分段，最后是 `instructions`。
    ///
    /// `instructions` 在包装器阶段之后才追加，因此不会被扩展的 `Wrap::Section` 处理。
    pub sections: Vec<Arc<dyn PromptSection>>,
    /// 说明。
    pub instructions: Option<String>,
    /// 工作目录。
    pub cwd: Option<String>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Agent")
            .field("model", &self.model)
            .field("thinking_level", &self.thinking_level)
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Conversations / submissions (host-facing)
// ---------------------------------------------------------------------------

/// 一次运行中的一次创建事务里执行的初始化。
///
/// 参数是创建事务与已写入的会话 ID。它运行在创建会话记录的同一事务里，位于内建文档、
/// `conversation_created` 回调与 `AgentChange` 之后；返回 `Err` 会使整个创建提交失败，
/// 会话记录不会落盘（`harness/harness.rs` 的 `create`）。
pub type ConversationInit = Arc<
    dyn for<'a> Fn(
            &'a dyn Tx,
            ConversationId,
        )
            -> Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + 'a>>
        + Send
        + Sync,
>;

/// 创建/分叉会话的选项（TS: `ConversationCreateOptions`）。
///
/// 没有 `Default`：TS 里 `ownership` 必填，Rust 构造时也必须显式给出。
#[derive(Clone)]
pub struct ConversationCreateOptions {
    /// 显式选择的拥有权（TS 里必填）。
    pub ownership: ConversationOwnership,
    /// 在创建 hook 的复制之后、`init` 之前应用的 agent 变更。
    pub agent: Option<AgentChange>,
    /// 创建事务里的初始化。
    pub init: Option<ConversationInit>,
}

/// 时间窗口形式的延迟参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DeferredWindow {
    /// 15 分钟。
    #[serde(rename = "15m")]
    FifteenMinutes,
    /// 1 小时。
    #[serde(rename = "1h")]
    OneHour,
    /// 24 小时。
    #[serde(rename = "24h")]
    OneDay,
}

/// `ConversationStreamOptions.deferred`：bool 或带窗口的对象。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Deferred {
    /// 纯 bool。
    Enabled(bool),
    /// 窗口。
    Window {
        /// 窗口。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<DeferredWindow>,
    },
}

/// 精选的 kiana-ai 请求选项（TS: `ConversationStreamOptions`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStreamOptions {
    /// 传输。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// 超时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<f64>,
    /// 重试次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    /// 最大重试延迟。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<f64>,
    /// 额外请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    /// 请求元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
    /// 缓存策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    /// 延迟请求。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<Deferred>,
}

/// 持久的 generation 重试策略（TS: `ConversationRetryPolicy`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRetryPolicy {
    /// 是否启用。
    pub enabled: bool,
    /// 最大重试次数。
    pub max_retries: i64,
    /// 基础延迟。
    pub base_delay_ms: f64,
    /// agent 延迟上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<f64>,
}

/// `HarnessSettings.retry`：TS 的 `Partial<ConversationRetryPolicy>`。
///
/// 每个字段缺省时沿用 [`crate::harness::agent::DEFAULT_RETRY_POLICY`] 的对应值
/// （TS: `{ ...DEFAULT_RETRY_POLICY, ...settings?.retry }`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RetrySettings {
    /// 是否启用。
    pub enabled: Option<bool>,
    /// 最大重试次数。
    pub max_retries: Option<i64>,
    /// 基础延迟。
    pub base_delay_ms: Option<f64>,
    /// agent 延迟上限。
    pub max_agent_delay_ms: Option<f64>,
}

/// 自动压缩阈值（TS: `CompactionPolicy`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPolicy {
    /// 阈值与溢出压缩。
    pub enabled: bool,
    /// 为回答保留的空间。
    pub reserve_tokens: f64,
    /// 摘要保留的近期上下文大小。
    pub keep_recent_tokens: f64,
    /// 后台压缩在阻塞阈值下方多少开始；`0` 关闭。
    pub background_tokens: f64,
}

/// `CompactionPolicy` 的每模型覆盖（Partial）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPolicyOverride {
    /// 是否启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 保留空间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<f64>,
    /// 保留的近期上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<f64>,
    /// 后台阈值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tokens: Option<f64>,
}

/// Harness 级压缩设置（TS: `CompactionSettings`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    /// 是否启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 保留空间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<f64>,
    /// 保留的近期上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<f64>,
    /// 后台阈值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tokens: Option<f64>,
    /// 按 `"provider/modelId"` 的覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, CompactionPolicyOverride>>,
}

/// 解析后的压缩策略（TS: `Settings.compaction`，即
/// `CompactionPolicy & Pick<CompactionSettings, "modelOverrides">`）。
///
/// 与 [`CompactionSettings`] 的区别：四个策略字段都已解析成具体值（不再 `Option`），只有按模型的
/// 覆盖仍可选，因此消费者不需要猜默认值。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCompactionSettings {
    /// 是否启用。
    pub enabled: bool,
    /// 保留空间。
    pub reserve_tokens: f64,
    /// 保留的近期上下文。
    pub keep_recent_tokens: f64,
    /// 后台阈值。
    pub background_tokens: f64,
    /// 按 `"provider/modelId"` 的覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, CompactionPolicyOverride>>,
}

/// 运行进度的提交节奏（TS: `ProgressPolicy`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPolicy {
    /// 回答提交的最小间隔。
    pub partial_interval_ms: f64,
    /// 工具输出提交的最小间隔。
    pub output_interval_ms: f64,
    /// 延迟提交在内存里的最长时间。
    pub persist_within_ms: f64,
}

/// `HarnessSettings.progress`：TS 的 `Partial<ProgressPolicy>`。
///
/// 每个字段缺省时沿用 [`crate::harness::agent::DEFAULT_PROGRESS_POLICY`] 的对应值；
/// `persist_within_ms` 不是非负数时同样回落到默认值（TS: `persistWithin()`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressSettings {
    /// 回答提交的最小间隔。
    pub partial_interval_ms: Option<f64>,
    /// 工具输出提交的最小间隔。
    pub output_interval_ms: Option<f64>,
    /// 延迟提交在内存里的最长时间。
    pub persist_within_ms: Option<f64>,
}

/// 压缩原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    /// `compact()`。
    Manual,
    /// 阈值。
    Threshold,
    /// 上下文溢出。
    Overflow,
}

/// 压缩的结果 ID（TS: `CompactionResult`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// 阻塞压缩摘要的条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    /// 会话拥有的压缩摘要写入的提交。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<SubmissionId>,
}

/// 失败运行如何处理排在后面的输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AfterFailedRun {
    /// 保持排队。
    Keep,
    /// 继续下一次运行。
    Continue,
    /// 以 `unanswered`/`run_failed` 结清。
    Withdraw,
}

/// Harness 级运行策略（TS: `HarnessSettings`）。
///
/// 这是宿主给出的**部分**设置：每个字段 `None` 都表示“用默认值”。解析由
/// `harness/agent.rs` 的 `resolve_settings` 完成（`retry`/`progress` 逐字段回落到
/// `DEFAULT_RETRY_POLICY`/`DEFAULT_PROGRESS_POLICY`，另有部分数值范围规则），解析结果是
/// [`Settings`]。
#[derive(Clone, Default)]
pub struct HarnessSettings {
    /// 默认扩展选择。
    pub extensions: Option<Vec<Arc<dyn Extension>>>,
    /// 请求选项。
    pub stream: Option<ConversationStreamOptions>,
    /// 重试策略（部分字段，逐字段覆盖默认值）。
    pub retry: Option<RetrySettings>,
    /// 压缩。
    pub compaction: Option<CompactionSettings>,
    /// 进度（部分字段，逐字段覆盖默认值）。
    pub progress: Option<ProgressSettings>,
    /// 工具执行模式。
    pub tool_execution: Option<ToolExecutionMode>,
    /// steering 队列模式。
    pub steering_mode: Option<QueueMode>,
    /// follow-up 队列模式。
    pub follow_up_mode: Option<QueueMode>,
    /// 空闲会话保留上下文读取的时间。
    pub context_retention_ms: Option<f64>,
    /// 未观察文档在内存里保留的时间。
    pub document_retention_ms: Option<f64>,
    /// 失败运行后的处理。
    pub after_failed_run: Option<AfterFailedRun>,
    /// restart 上限。
    pub restart_limit: Option<f64>,
}

/// 解析后的设置（TS: `Settings`）。
///
/// 由 `harness/agent.rs` 的 `resolve_settings` 从可选的 [`HarnessSettings`] 补齐默认值；
/// 这里的字段都是具体值，消费者不需要再猜缺省，只有 `extensions` 仍可表示“未指定”。
pub struct Settings {
    /// 扩展；`None` 表示全部按安装顺序。
    pub extensions: Option<Vec<Arc<dyn Extension>>>,
    /// 请求选项。
    pub stream: ConversationStreamOptions,
    /// 重试策略。
    pub retry: ConversationRetryPolicy,
    /// 压缩（解析后的策略 + 可选的每模型覆盖）。
    pub compaction: ResolvedCompactionSettings,
    /// 进度。
    pub progress: ProgressPolicy,
    /// 工具执行模式。
    pub tool_execution: ToolExecutionMode,
    /// steering 队列模式。
    pub steering_mode: QueueMode,
    /// follow-up 队列模式。
    pub follow_up_mode: QueueMode,
    /// 空闲上下文保留。
    pub context_retention_ms: f64,
    /// 文档保留。
    pub document_retention_ms: f64,
    /// 失败运行处理。
    pub after_failed_run: AfterFailedRun,
    /// restart 上限。
    pub restart_limit: f64,
}

/// `HarnessOptions.env` 的目标。
pub struct EnvTarget {
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 会话 agent 的 cwd。
    pub cwd: Option<String>,
    /// 已提交的文档读取。
    pub read: Arc<dyn DocumentReader>,
}

/// 环境构建回调。
///
/// 收到会话 ID、当前 `kiana.agent.cwd` 与已提交文档的读取器；返回 `None` 表示该会话不提供
/// 环境（`HarnessImpl::build_env` 在未配置构建器时也返回 `None`）。返回的错误按调用点原样
/// 传播，不转换成报告。
pub type EnvBuilder = Arc<
    dyn Fn(
            EnvTarget,
            Arc<dyn Context>,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<Arc<dyn ExecutionEnv>>, BoxError>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// `HarnessOptions.conversationCreated`。
///
/// 参数是创建事务与刚写入的会话记录。它在四个内建文档与复制出的 `kiana.agent` 之后、
/// `ConversationCreateOptions::agent` 与 `init` 之前运行，因此能看到完整初始文档并覆盖
/// agent 值；返回 `Err` 使创建失败，会话记录不会落盘（`HarnessHooks::conversation_created`）。
pub type ConversationCreatedHandler = Arc<
    dyn for<'a> Fn(
            &'a dyn Tx,
            &'a ConversationRecord,
        )
            -> Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + 'a>>
        + Send
        + Sync,
>;

/// 模型访问：`Models` 或每次取当前 `Models` 的函数。
#[derive(Clone)]
pub enum ModelsProvider {
    /// 直接持有。
    Direct(Arc<dyn Models>),
    /// 每次调用。
    Dynamic(Arc<dyn Fn() -> Arc<dyn Models> + Send + Sync>),
}

impl ModelsProvider {
    /// 当前 `Models`。
    pub fn get(&self) -> Arc<dyn Models> {
        match self {
            ModelsProvider::Direct(models) => Arc::clone(models),
            ModelsProvider::Dynamic(provider) => provider(),
        }
    }
}

/// 设置来源。
///
/// 变体大小差异来自 TS 的联合体形状（固定设置 vs 回调）；枚举本身很少在线路上传输。
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum SettingsProvider {
    /// 固定设置。
    Static(HarnessSettings),
    /// 每次读取。
    Dynamic(Arc<dyn Fn() -> Option<HarnessSettings> + Send + Sync>),
}

/// `HarnessOptions`（TS 同名类型）。
///
/// 构造 Harness 的依赖注入点：`models` 与 `registry` 必填，其余可选。缺省行为：
/// `settings` 为 `None` 时 `resolve_settings(None)` 给出全默认值；`now` 为 `None` 时使用
/// 系统时钟（`SystemTime::now`，epoch 毫秒），宿主时钟 panic 会回落到它并上报一次；
/// `read_only` 为 `None` 时等同 `false`。`on_report` 接收带 [`ReportInfo`] 的非致命错误。
pub struct HarnessOptions {
    /// 无内容的接纳/阶段遥测适配器。
    pub telemetry: Option<Arc<dyn TelemetryContext>>,
    /// 模型访问。
    pub models: ModelsProvider,
    /// 请求身份。
    ///
    /// 与 `DEFAULT_HARNESS_IDENTITY` 合并（后者在前、本字段逐字段覆盖）后包进模型客户端；
    /// 单次请求的 `StreamOptions.identity` 优先级仍最高（`current_models`）。
    pub identity: Option<ClientIdentity>,
    /// registry 读端。
    pub registry: Arc<dyn RegistryReader>,
    /// 运行设置。
    pub settings: Option<SettingsProvider>,
    /// 环境构建器。
    pub env: Option<EnvBuilder>,
    /// 创建/分叉会话的提交里运行。
    pub conversation_created: Option<ConversationCreatedHandler>,
    /// Harness 时钟（epoch 毫秒；默认 `Date.now`，见 ARCHITECTURE §4.3(7)）。
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// 计时器。
    pub set_timer: Option<SetTimer>,
    /// 睡眠任务的唤醒回调。
    pub on_wake: Option<Arc<dyn Fn(&Wake) + Send + Sync>>,
    /// 非致命失败报告。
    pub on_report:
        Option<Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync>>,
    /// 只读打开。
    ///
    /// 缺省 `false`。为真时调度器拒绝 `resume()`（返回 `ReadOnlySession` 错误）、`wake()`
    /// 恒报不活跃，也不安排 reconcile/wake 计时器（`harness/scheduler.rs` 的对应检查）。
    pub read_only: Option<bool>,
}

/// `Harness.wake()`：Session 何时再需要一个活进程。
///
/// 由调度器计算：`active` 为真表示现在就有工作；`at` 是当前记录中最早一个未到期、且未被
/// abort 的睡眠时间点（可与 `active` 同时出现），到点后需要再次唤醒进程。只读调度器恒返回
/// `{ active: false, at: None }`（`TaskScheduler::wake`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wake {
    /// 现在就有可运行/正在运行的任务，或有 reconcile 尚未落地。
    pub active: bool,
    /// 最早一个未到期的睡眠（epoch 毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

/// 一个活动任务的调度视角状态。
///
/// 这是 `inspect()` 时由调度器**派生**的视图，不持久化。判定优先级（`inspect_task`）：
/// `Running`（当前有 invocation）→ `Completing`（持久状态已是 completing）→ `Waiting`
/// （被等待的任务仍活着）→ `Sleeping`（未到期）→ `Blocked`（restart 上限，或定义/迁移
/// 不可用）→ `Ready`。因此同一条记录在持久状态之外还可能显示 `Running`/`Blocked`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TaskInspectionState {
    /// 有一次调用在跑。
    Running,
    /// 下一次调度会保留它。
    Ready {
        /// 定义更新且带 `migrate`。
        migrates: bool,
    },
    /// 等待这些活动任务。
    Waiting {
        /// 等待的活动任务。
        on: Vec<TaskId>,
    },
    /// 睡到某个时钟点。
    Sleeping {
        /// 毫秒时间点（epoch 毫秒）。
        until: i64,
    },
    /// 结果已定，等待普通工作收尾。
    Completing,
    /// 不能在这里运行。
    Blocked {
        /// 原因。
        reason: BlockedReason,
        /// 相关错误。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<JsonValue>,
    },
}

/// 活动任务与其调度视角（TS: `TaskInspection`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskInspection {
    /// 记录。
    pub record: TaskRecord,
    /// 调度视角。
    pub state: TaskInspectionState,
}

/// 调度状态。
///
/// 由调度器的关闭标志与启用标志决定：`Closing` 优先，其次 `enabled` 为真时 `Running`，
/// 否则 `Paused`（`TaskScheduler::inspect`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchedulingState {
    /// 暂停。
    Paused,
    /// 运行中。
    Running,
    /// 正在关闭。
    Closing,
}

/// 活动工作的时点视图（TS: `HarnessInspection`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInspection {
    /// 调度状态。
    pub scheduling: SchedulingState,
    /// 任务。
    pub tasks: Vec<TaskInspection>,
    /// queued 与 placed 的提交，按 ID 顺序。
    ///
    /// 两组分别从 Storage 扫描后合并，再按 `SubmissionId` 升序排序（`HarnessImpl::inspect`）。
    pub submissions: Vec<SubmissionRecord>,
}

/// 原始活动转录与派生模型上下文（TS: `ContextView`）。
///
/// 序列化必须复现 TS 的键序：条目走 [`EntryRecord`] 的提示序；`contributions` 是条目
/// `model` 数组里的同一批消息对象，按条目内的持久序写出；`messages` 绝大多数也是这些消息，
/// 但合成的「工具结果缺失」消息是 context.ts 的对象字面量
/// （`role, toolCallId, toolName, content, isError, details, timestamp`），比 kiana-ai 的结构体序
/// 早一个 `isError`（TS: `missingResult`）。
///
/// 构造来源是 `harness/context.rs` 的 `read_context`/`derive_range`：`entries` 是活动条目，
/// `contributions` 与之一一对应（被编辑后的模型消息），`messages` 是下一次 provider 请求的
/// 实际数组（已定稿部分 + 排序后的开放部分，基线 system 可能被领到最前）。`head` 为 `None`
/// 表示范围内没有 head 标记。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ContextView {
    /// 最新适用的 head 标记。
    #[serde(default)]
    pub head: Option<EntryRecord>,
    /// 原始活动条目。
    pub entries: Vec<EntryRecord>,
    /// 每个条目的模型消息（编辑之后、排除 stop reason 之前）。
    pub contributions: Vec<Vec<Message>>,
    /// 下一次 provider 请求的模型上下文。
    pub messages: Vec<Message>,
}

/// `ContextView` 的序列化（见类型的键序说明）。
impl serde::Serialize for ContextView {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 手写 map 而不是 derive：键序必须是 TS 对象字面量的 head?, entries, contributions,
        // messages，且两个数组的元素还要逐条重排键（见下面的两个包装类型）。
        use serde::ser::SerializeMap;
        let length = 3 + usize::from(self.head.is_some());
        let mut map = serializer.serialize_map(Some(length))?;
        if let Some(head) = &self.head {
            map.serialize_entry("head", head)?;
        }
        map.serialize_entry("entries", &self.entries)?;
        map.serialize_entry("contributions", &ContributionList(&self.contributions))?;
        map.serialize_entry("messages", &ContextMessageList(&self.messages))?;
        map.end()
    }
}

/// `messages` 的逐条序列化：合成结果换成 TS 字面量键序，其余走 `persisted_message_json`。
struct ContextMessageList<'a>(&'a [Message]);

impl serde::Serialize for ContextMessageList<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut values = Vec::with_capacity(self.0.len());
        for message in self.0 {
            values.push(context_message_json(message).map_err(S::Error::custom)?);
        }
        values.serialize(serializer)
    }
}

/// `contributions` 的逐条序列化：每个条目的消息按持久序写出，不识别合成结果。
struct ContributionList<'a>(&'a [Vec<Message>]);

impl serde::Serialize for ContributionList<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut values = Vec::with_capacity(self.0.len());
        for messages in self.0 {
            let mut items = Vec::with_capacity(messages.len());
            for message in messages {
                items
                    .push(crate::types::persisted_message_json(message).map_err(S::Error::custom)?);
            }
            values.push(items);
        }
        values.serialize(serializer)
    }
}

/// 一条上下文消息的持久键序；合成的缺失工具结果用 context.ts 的字面量序。
pub(crate) fn context_message_json(message: &Message) -> Result<JsonValue, serde_json::Error> {
    // 识别 context.rs `missing_result()` 合成的占位结果：字段必须全部吻合——is_error、
    // 没有 duration/usage/nested、details 恰为 {"reason":"missing_result"}、正文恰为单个
    // MISSING_RESULT_TEXT 文本块。判定只看字段值、不追踪来源，因此真实工具产生同样形状的
    // 结果时也会走这个键序（内容相同，只是键序不同）。
    let missing_result = match message {
        Message::ToolResult(result) => {
            let missing_details = serde_json::json!({"reason": "missing_result"});
            result.is_error
                && result.duration_ms.is_none()
                && result.usage.is_none()
                && result.nested_calls.is_none()
                && result.details.as_ref() == Some(&missing_details)
                && matches!(
                    result.content.as_slice(),
                    [kiana_ai::UserContentBlock::Text(text)]
                        if text.text == crate::harness::context::MISSING_RESULT_TEXT
                )
        }
        _ => false,
    };
    // 先按 kiana-ai 结构体序得到 JSON，再对合成结果重排成 TS 的 `missingResult` 字面量序
    // （role, toolCallId, toolName, content, isError, details, timestamp）。
    let value = crate::types::persisted_message_json(message)?;
    if missing_result {
        Ok(crate::types::reorder_object_keys(
            value,
            &[
                "role",
                "toolCallId",
                "toolName",
                "content",
                "isError",
                "details",
                "timestamp",
            ],
        ))
    } else {
        Ok(value)
    }
}

/// `Conversation.context()` 的选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextViewOptions {
    /// 在某个可见条目处截断。
    pub at: Option<EntryId>,
}

/// `Conversation.entries()` 的过滤（`Omit<EntryQuery, "conversationId">`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRangeQuery {
    /// 可能返回的最旧条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_entry_id: Option<EntryId>,
    /// 可能返回的最新条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entry_id: Option<EntryId>,
    /// 顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<crate::types::ScanOrder>,
}

impl EntryRangeQuery {
    /// 补上会话 ID。
    pub fn into_query(self, conversation_id: ConversationId) -> EntryQuery {
        EntryQuery {
            conversation_id,
            min_entry_id: self.min_entry_id,
            max_entry_id: self.max_entry_id,
            order: self.order,
        }
    }
}

/// 提交中止的结果。
///
/// 来自 [`Submission::abort`]：对象已经存在，所以没有 `NotFound` 变体——未知 ID 由该方法返回
/// 错误。`Aborted` 只对应仍处于 queued 的提交（撤回并移除 inbox 项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionAbortOutcome {
    /// 已中止。
    Aborted,
    /// 已经进入转录。
    AlreadyPlaced,
    /// 已经落定。
    Settled,
}

/// 任务的提交中止结果。
///
/// 与 [`SubmissionAbortOutcome`] 相同，另加 `NotFound`：Harness 级入口按 ID 查找提交，且
/// `conversation_id` 不匹配也按未知处理（`harness/submissions.rs` 的 `abort`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortSubmissionOutcome {
    /// 已中止。
    Aborted,
    /// 已经进入转录。
    AlreadyPlaced,
    /// 已经落定。
    Settled,
    /// 未知或属于别的会话。
    NotFound,
}

/// 中止任务的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AbortTaskOutcome {
    /// 已写下 abort 标记。
    Marked,
    /// 已经终态。
    Terminal,
}

/// 一个已接纳提交的宿主对象（TS: `Submission`）。
///
/// 常规实现是 `harness/submissions.rs` 的 `SubmissionHandle`：只持有 ID 与 `Submissions`
/// 句柄，记录从 Storage 读取或经提交事务修改。任务绑定的会话
/// （`bound_conversation`/`BoundConversation::submit`，`harness/harness.rs`）返回的是
/// `BoundSubmission`：它持有 ID、`InvocationBinding` 与内层 `Arc<dyn Submission>`，先检查
/// 调用绑定，再在带 abort 信号的上下文里把操作转发给内层 `Submission`。会话关闭后 `wait`
/// 以错误结束。
#[async_trait]
pub trait Submission: Send + Sync {
    /// 提交 ID。
    fn id(&self) -> SubmissionId;
    /// 当前记录。
    ///
    /// 记录不存在时返回错误，而不是 `None`。
    async fn status(&self, context: &dyn Context) -> Result<SubmissionRecord, BoxError>;
    /// 等到终态。
    ///
    /// 返回时状态必是 `done` 或 `unanswered`；已结算的提交立即返回，否则登记 waiter。
    /// 会话关闭会让等待以错误结束。检查与登记在同一条变更线上，中间不会插入结算提交。
    async fn wait(&self, context: &dyn Context) -> Result<SettledSubmissionRecord, BoxError>;
    /// 中止。
    ///
    /// queued 提交被撤回并结算为 `unanswered`（`aborted`）；已放置/已结算如实返回对应变体。
    async fn abort(&self, context: &dyn Context) -> Result<SubmissionAbortOutcome, BoxError>;
}

/// 任务/工具的调用绑定会话操作（TS: `ConversationHandle`）。
#[async_trait]
pub trait ConversationHandle: Send + Sync {
    /// 会话 ID。
    fn id(&self) -> ConversationId;
    /// 提交输入。
    async fn submit(
        &self,
        submission: InputSubmissionDraft,
        context: &dyn Context,
    ) -> Result<Arc<dyn Submission>, BoxError>;
    /// 中止并等待空闲。
    async fn abort(
        &self,
        context: &dyn Context,
        options: ConversationAbortOptions,
    ) -> Result<(), BoxError>;
    /// 等待普通所有权作用域空闲。
    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError>;
}

/// 一个会话的无状态句柄（TS: `Conversation`）。
#[async_trait]
pub trait Conversation: Send + Sync {
    /// 会话 ID。
    fn id(&self) -> ConversationId;
    /// 用当前 registry 快照与设置解析的 agent。
    async fn agent(&self, context: &dyn Context) -> Result<Arc<Agent>, BoxError>;
    /// 在独立提交里 `configure()`。
    async fn configure(&self, change: AgentChange, context: &dyn Context) -> Result<(), BoxError>;
    /// 持久接纳用户输入或被动条目写入。
    ///
    /// 返回的 [`Submission`] 在落定前可 `wait`/`abort`；只含 kind/data 的展示型写入可能被
    /// 立即追加并结算为 `done`，忙碌时的输入则按 [`WhenBusy`] 排队或拒绝
    /// （`harness/submissions.rs` 的 `admit_submission`）。
    async fn submit(
        &self,
        submission: SubmissionDraft,
        context: &dyn Context,
    ) -> Result<Arc<dyn Submission>, BoxError>;
    /// 接纳一个 `kiana.reset` 写入。
    async fn reset(&self, handoff: Option<String>, context: &dyn Context) -> Result<(), BoxError>;
    /// 接纳一次手动压缩。
    async fn compact(
        &self,
        instructions: Option<String>,
        context: &dyn Context,
    ) -> Result<TaskId<CompactionResult>, BoxError>;
    /// 提交，`tx.createTask()` 默认到本会话。
    ///
    /// 返回值是这次 `change` 自己的类型擦除输出（即 `SessionChange::run` 的
    /// `Box<dyn Any + Send>`），调用方按与 change 的约定 downcast；它不是会话记录。
    async fn commit(
        &self,
        change: &dyn crate::types::SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError>;
    /// 已提交的活动转录与模型上下文。
    ///
    /// `options.at` 为 `Some` 时只派生到该可见条目为止（更晚的条目与结果不参与），与
    /// `harness/context.rs` 的 `read_context` 一致。
    async fn context(
        &self,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError>;
    /// fork-aware 历史。
    ///
    /// 把 `query` 补上本会话 ID 后调用 Storage 的 `scan_entries`；`limit` 与 `cursor` 原样
    /// 透传。分页边界由 Storage 决定，调用方用返回的 [`Page`] 继续翻页。
    async fn entries(
        &self,
        query: EntryRangeQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError>;
    /// 在某个可见条目处分叉。
    ///
    /// 新会话记录带 `parent { conversation_id, at }`，文档复制由创建事务按 fork 来源登记；
    /// `options.ownership`、`agent` 与 `init` 的语义与 `create_conversation` 相同
    /// （`harness/harness.rs` 的 `ConversationImpl::fork`）。
    async fn fork(
        &self,
        at: EntryId,
        options: ConversationCreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn Conversation>, BoxError>;
    /// 中止普通所有权作用域。
    async fn abort(
        &self,
        context: &dyn Context,
        options: ConversationAbortOptions,
    ) -> Result<(), BoxError>;
    /// 等待普通所有权作用域空闲。
    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError>;
    /// 结构视图的 watch。
    async fn watch(&self, context: &dyn Context) -> Result<ConversationWatch, BoxError>;
}

/// 一次 Durable Harness（TS: `Harness`，继承 `Session`）。
#[async_trait]
pub trait Harness: crate::types::Session {
    /// 启用调度。
    ///
    /// 只读 Harness 返回 `ReadOnlySession` 错误；重复调用只会重新置位并再次把到期睡眠
    /// 变为可运行（`TaskScheduler::resume`）。
    fn resume(&self) -> Result<(), BoxError>;
    /// 下一次需要活进程的时点。
    ///
    /// 只读 Harness 恒报不活跃；Session 失败/关闭后再调用会 panic 并携带错误对象
    /// （`HarnessImpl::wake`），需要可观察错误时用 `HarnessImpl::try_wake`。
    fn wake(&self) -> Wake;
    /// 根会话。
    async fn root(
        &self,
        context: &dyn Context,
        options: RootOptions,
    ) -> Result<Arc<dyn Conversation>, BoxError>;
    /// 按 ID 取会话。
    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn Conversation>>, BoxError>;
    /// 创建会话。
    async fn create_conversation(
        &self,
        options: ConversationCreateOptions,
        context: &dyn Context,
    ) -> Result<Arc<dyn Conversation>, BoxError>;
    /// 取任务。
    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError>;
    /// 活动任务与未落定提交。
    async fn inspect(&self, context: &dyn Context) -> Result<HarnessInspection, BoxError>;
    /// 重新获取一个提交。
    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn Submission>>, BoxError>;
    /// 中止提交。
    ///
    /// 给出 `conversation_id` 时，属于别的会话的提交按 `NotFound` 处理（不会误中止）。
    async fn abort_submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
        conversation_id: Option<ConversationId>,
    ) -> Result<AbortSubmissionOutcome, BoxError>;
    /// 中止任务。
    async fn abort_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<AbortTaskOutcome, BoxError>;
    /// 让被 restart 上限停泊的任务再跑。
    async fn retry_task(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError>;
    /// 等待任务终态。
    ///
    /// 任务仍在本进程的 live 集合里时等内存 waiter；否则直接读 Storage 并用
    /// [`SettledTask::from_record_unchecked`] 包装返回——这条路径**可能返回非终态记录**
    /// （复刻 TS `waitForTask()` 的行为）。任务不存在时返回错误。
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError>;
    /// 等待所有 ownerless 会话空闲。
    async fn wait_for_idle(&self, context: &dyn Context) -> Result<(), BoxError>;
    /// Session 总计用量。
    ///
    /// 扫描所有会话的 `kiana.usage` 文档，按桶逐键相加（可选计数器只要任一侧报告过就累加，
    /// 见 `harness/usage.rs` 的 `add_usage_state`）；无法解析的文档被跳过。
    async fn usage(&self, context: &dyn Context) -> Result<UsageState, BoxError>;
    /// 任务图 watch。
    async fn watch_task_graph(&self, context: &dyn Context) -> Result<TaskGraphWatch, BoxError>;
}

/// `Harness.root()` 的选项。
#[derive(Clone, Default)]
pub struct RootOptions {
    /// agent 变更。
    pub agent: Option<AgentChange>,
    /// 初始化。
    pub init: Option<ConversationInit>,
}

// ---------------------------------------------------------------------------
// Task runtime / tool API / hooks
// ---------------------------------------------------------------------------

/// 任务对自己状态的提交回调（TS `TaskRuntime.commit` 的 change）。
///
/// `run` 在 Session 变更线上执行并拿到当前记录：`Ok(None)` 表示不改状态，`Ok(Some(next))`
/// 用 `next` 替换。任务已结束、会话关闭、记录不再是 `running`，或运行调用遇到 abort 标记时，
/// 提交会失败（`harness/scheduler.rs` 的 `GatedChange`），回调不会执行。
#[async_trait]
pub trait TaskRuntimeChange: Send + Sync {
    /// 运行；返回新状态或不变。
    async fn run(&self, tx: &dyn Tx, current: &TaskRecord) -> Result<Option<TaskState>, BoxError>;
}

/// 把 hook 分发给匹配的处理器（TS: `HookRunner`）。
///
/// TS 的 `each<K extends keyof H>(name, invoke)` 是泛型回调；Rust 里以“按名字取处理器”
/// 的擦除形式表达，回调由调用方在取到的值上执行。
pub trait HookRunner: Send + Sync {
    /// 所有处理器名。
    fn names(&self) -> Vec<String>;
    /// 名字匹配的处理器。
    fn handlers(&self, name: &str) -> Vec<Arc<dyn Any + Send + Sync>>;
    /// 名字匹配的处理器及其所属扩展（TS: `agentHooks()` 的 `{ extension, handlers }` 项）。
    ///
    /// TS 的 `hooks.each` 在处理器抛错时把 `extension` 放进 `extension_error` 报告；默认实现
    /// 只有擦除的处理器表、拿不到扩展名，scheduler 的实现应覆盖它。
    fn handlers_with_extension(
        &self,
        name: &str,
    ) -> Vec<(Option<String>, Arc<dyn Any + Send + Sync>)> {
        self.handlers(name)
            .into_iter()
            .map(|handler| (None, handler))
            .collect()
    }
}

/// 一次任务调用的操作（TS: `TaskRuntime<I, S, R, H>`）。
///
/// 类型参数 `I`/`S`/`R` 是任务的输入/状态/结果，`H` 是任务定义的 hook 处理器类型；调度器以
/// `TaskRuntime<JsonValue, JsonValue, JsonValue, H>` 实现它（`SchedulerRuntime`）。除
/// [`TaskRuntime::hook_type_marker`] 外所有方法都与 `H` 无关，`H` 只用于保证同一个 trait
/// object 与任务定义的类型参数一致。
#[async_trait]
pub trait TaskRuntime<I = JsonValue, S = JsonValue, R = JsonValue, H = ()>:
    DocumentObserver + DocumentReader + Send + Sync
{
    /// 任务 ID。
    fn task_id(&self) -> TaskId<R>;
    /// 会话 ID。
    fn conversation_id(&self) -> ConversationId;
    /// 调用被中止时触发。
    fn signal(&self) -> AbortSignal;
    /// 当前阶段的 registry 快照。
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    /// 本阶段最多解析一次的 agent。
    async fn agent(&self, context: &dyn Context) -> Result<Arc<Agent>, BoxError>;
    /// 当前设置。宿主的 settings getter 抛错时把错误传进 phase（TS: `get settings() { return
    /// settings(); }`）。
    fn settings(&self) -> Result<Arc<Settings>, BoxError>;
    /// 当前模型访问。
    fn models(&self) -> Arc<dyn Models>;
    /// 为任务的会话构建环境。
    async fn env(&self, context: &dyn Context) -> Result<Option<Arc<dyn ExecutionEnv>>, BoxError>;
    /// 本任务名的 hook 处理器。
    fn hooks(&self) -> Arc<dyn HookRunner>;

    /// 在 Session 线上提交；返回的状态替换任务状态。
    async fn commit(
        &self,
        change: &dyn TaskRuntimeChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<(), BoxError>;
    /// 读一个持久备忘。
    async fn memo(&self, name: &str, context: &dyn Context) -> Result<Option<JsonValue>, BoxError>;
    /// 若备忘不存在则写 `candidate`，返回持久赢家。
    ///
    /// 已存在时不覆盖（先写者获胜）；写入在任务的提交事务里执行，并发调用最终会拿到同一个
    /// 持久值。
    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError>;
    /// 已提交任务记录。
    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError>;
    /// 等到任务终态收据。
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError>;
    /// 中止一个自己拥有的任务并等它终态。
    async fn abort_owned(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError>;
    /// 直接拥有的活动任务，按 ID 顺序。
    async fn owned_tasks(&self, context: &dyn Context) -> Result<Vec<TaskRecord>, BoxError>;
    /// 终态任务的结果。
    ///
    /// 按输入顺序返回；任一 ID 不存在或不是终态时整个调用返回错误，不会跳过或返回 `None`。
    async fn outcomes(
        &self,
        ids: &[TaskId],
        context: &dyn Context,
    ) -> Result<Vec<TaskOutcome>, BoxError>;
    /// 已有会话的调用绑定句柄。
    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError>;
    /// 从本会话可见的已提交条目。
    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError>;
    /// 已提交的原始活动转录与模型上下文。
    async fn context(
        &self,
        conversation_id: ConversationId,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError>;
    /// Harness 时钟（epoch 毫秒）。
    fn now(&self) -> i64;
    /// 在 Harness 计时器上安排一次回调。
    fn set_timer(
        &self,
        callback: crate::timers::TimerCallback,
        delay_ms: f64,
    ) -> crate::timers::Disarm;
    /// 转发非致命失败。
    fn report(&self, error: &(dyn std::error::Error + Send + Sync), from: Option<ReportFrom>);
    /// 提交一次睡眠。
    ///
    /// `until` 是 epoch 毫秒且必须非负；abort 处理器里调用会失败（`validate_sleep`）。
    /// 睡眠期间任务状态是 `Sleeping`，到期后调度器把它重新变为可运行。
    async fn sleep_until(
        &self,
        until: i64,
        checkpoint: S,
        context: &dyn Context,
    ) -> Result<(), BoxError>;

    /// 让类型参数 `H` 出现在对象签名里（hook 处理器类型）。
    fn hook_type_marker(&self) -> std::marker::PhantomData<fn() -> H> {
        std::marker::PhantomData
    }
}

/// `TaskRuntime` 的类型化条目读（TS: `TaskRuntime.entry<D>(token, id, context)` 重载）。
///
/// 与 [`crate::types::TypedEntryTx`] 同理：`TaskRuntime` 以 trait object 使用，带类型参数的
/// 重载放在扩展 trait 上，对每个 `dyn TaskRuntime<...>` 自动可用。
#[async_trait]
pub trait TypedEntryRuntime {
    /// 读一个条目；不存在或 kind 与 token 不符时返回 `None`。
    async fn entry_typed<D: EntryData>(
        &self,
        token: &Entry<D>,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<TypedEntry<D>>, BoxError>;
}

#[async_trait]
impl<I, S, R, H> TypedEntryRuntime for dyn TaskRuntime<I, S, R, H> {
    async fn entry_typed<D: EntryData>(
        &self,
        token: &Entry<D>,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<TypedEntry<D>>, BoxError> {
        let Some(record) = self.entry(id, context).await? else {
            return Ok(None);
        };
        token.typed_entry(record)
    }
}

/// 一次工具调用的操作（TS: `ToolExecutionApi`）。
///
/// 实现是 `harness/tool.rs` 的 `ToolApi`。生命周期约束：`Arc<dyn ToolExecutionApi>` 只在本次
/// 工具调用内有效（内部把任务 runtime 的借用延长为 `'static`），工具实现不得把它或它的克隆
/// 保存到调用返回之后；进度提交与在飞提交都由 `tool.rs` 在返回前等完。
#[async_trait]
pub trait ToolExecutionApi: DocumentObserver + DocumentReader + Send + Sync {
    /// 工具任务。
    fn task_id(&self) -> TaskId;
    /// 会话。
    fn conversation_id(&self) -> ConversationId;
    /// 调用 ID。
    fn call_id(&self) -> &str;
    /// 工具任务的阶段快照。
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    /// 调用会话的 agent。
    async fn agent(&self, context: &dyn Context) -> Result<Arc<Agent>, BoxError>;
    /// 当前模型访问。
    fn models(&self) -> Arc<dyn Models>;
    /// 本次调用构建的环境。
    fn env(&self) -> Option<Arc<dyn ExecutionEnv>>;
    /// Harness 时钟（epoch 毫秒）。
    fn now(&self) -> i64;
    /// 追加运行输出。
    fn output(&self, chunk: ToolOutput, skipped: Option<ShellOutputSkip>);
    /// 保留尾部的窗口；`None` 表示保留头部、不接受 skip。
    fn output_window(&self) -> Option<ShellOutputWindow>;
    /// 目前保留的输出。
    fn retained_output(&self) -> RetainedOutput;
    /// 记录一条诊断。
    fn diagnostic(&self, diagnostic: ToolDiagnostic);
    /// 替换运行中的 details。
    async fn details(&self, value: JsonValue, context: &dyn Context) -> Result<(), BoxError>;
    /// 本调用任务的提交。
    async fn commit(
        &self,
        change: &dyn crate::types::SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError>;
    /// 读备忘。
    async fn memo(&self, name: &str, context: &dyn Context) -> Result<Option<JsonValue>, BoxError>;
    /// 写备忘。
    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError>;
    /// 在一次提交里创建任务。
    async fn create_task(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskOptions,
        context: &dyn Context,
    ) -> Result<TaskId, BoxError>;
    /// 取任务。
    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError>;
    /// 等到任务终态。
    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError>;
    /// 已有会话的句柄。
    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError>;
    /// 运行一个嵌套工具调用。
    ///
    /// **不是 `async fn`**：TS 的 `executeTool()` 是 async 方法，调用时同步跑完前缀
    /// （`assertLive()`、分配 key、把 admission 登记进本次调用的清单），之后才在
    /// microtask 上继续。Rust 的 `async fn` 要等第一次 poll 才执行前缀，`void
    /// api.executeTool(...)` 的等价写法（创建 future 后丢弃/并发）会错过结算的
    /// `await Promise.allSettled(admissions)`。因此这里返回显式 boxed future，由实现
    /// 在**方法体内同步**完成登记；`run()` 会与工具执行并发驱动所有登记的 admission，
    /// 并在结算前等它们全部结束。
    fn execute_tool(
        self: Arc<Self>,
        name: &str,
        args: JsonObject,
        context: &dyn Context,
        options: NestedToolOptions,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<NestedToolExecutionResult, BoxError>>
                + Send
                + 'static,
        >,
    >;
}

/// `executeTool()` 的选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestedToolOptions {
    /// 显式 key。
    pub key: Option<String>,
    /// 是否提交运行输出。
    pub progress: Option<bool>,
}

/// hook 可用的操作（TS: `HookApi`）。
///
/// 实现是 `harness/live.rs` 的 `RuntimeHookApi`：它把 `models`、`memo`/`memo_put` 与
/// `DocumentReader` 的读取方法转发给任务 runtime，**不暴露** settings/env/commit。
/// hook 方法拿到的 `Arc<dyn HookApi>` 只在当前 hook 调用期间有效（`hook_api` 用延长为
/// `'static` 的借用构造），hook 不得保存它并在调用返回后使用。
#[async_trait]
pub trait HookApi: DocumentReader + Send + Sync {
    /// 任务 ID。
    fn task_id(&self) -> TaskId;
    /// 会话 ID。
    fn conversation_id(&self) -> ConversationId;
    /// 当前模型访问。
    fn models(&self) -> Arc<dyn Models>;
    /// 读备忘。
    async fn memo(&self, name: &str, context: &dyn Context) -> Result<Option<JsonValue>, BoxError>;
    /// 写备忘。
    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError>;
}

/// TS `HookResult<T>`：hook 可以返回 `T`、`undefined`，或二者的 Promise。
///
/// Rust 的 hook trait 把三种情况统一成 `async fn ... -> Result<Option<T>, BoxError>`；保留
/// 这个名字供端口代码在签名里引用。
pub type HookResult<T> = Result<Option<T>, BoxError>;

/// 一次调用的 hook 视图（TS: `ToolHookCall`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolHookCall {
    /// 调用。
    #[serde(flatten)]
    pub call: ToolCall,
    /// 嵌套调用的父调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NestedParent>,
}

/// 嵌套调用的父调用。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedParent {
    /// 父任务。
    pub task_id: TaskId,
    /// 父调用 ID。
    pub call_id: String,
}

/// `beforeRequest` 的请求。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BeforeRequestInput {
    /// 消息。
    pub messages: Vec<Message>,
}

/// `onYield` 的继续请求。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OnYieldOutput {
    /// 追加的 user message 内容。
    #[serde(rename = "continue")]
    pub continue_run: UserInput,
}

/// `beforeTool` 的结果。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BeforeToolOutput {
    /// 替换参数。
    ///
    /// 仅在 `block` 为 `None` 时生效；同时给出两者时 `block` 优先。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    /// 阻止调用。
    ///
    /// 非 `None` 即阻止：文本进入 `Tool call blocked: {block}` 的工具错误结果，工具实现
    /// 不会执行（`harness/tool.rs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
}

/// `beforeCompact` 的输入。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeCompactInput {
    /// 原因。
    pub reason: CompactionReason,
    /// 摘要替换的活动条目。
    pub entries: Vec<EntryRecord>,
    /// 它们的模型上下文。
    pub messages: Vec<Message>,
    /// 第一个原样保留的条目。
    pub first_kept: EntryId,
    /// 说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// `beforeCompact` 的决定。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum BeforeCompactDecision {
    /// 放弃压缩。
    Decline(DeclineCompaction),
    /// 使用这段摘要。
    Summary(CompactionSummary),
}

/// `decline: true`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeclineCompaction {
    /// 固定 `true`。
    pub decline: bool,
}

/// `summary`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompactionSummary {
    /// 摘要文本。
    pub summary: String,
}

/// 内建 generation 任务的 hook（TS: `GenerationHooks`）。
#[async_trait]
pub trait GenerationHooks: Send + Sync {
    /// 每次请求尝试之前。
    async fn before_request(
        &self,
        request: BeforeRequestInput,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<Option<BeforeRequestInput>, BoxError>;
    /// 每个终态 provider 消息之后。
    async fn after_response(
        &self,
        message: AssistantMessage,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<(), BoxError>;
    /// 一个最终回答。
    async fn on_yield(
        &self,
        answer: AssistantMessage,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<Option<OnYieldOutput>, BoxError>;
    /// 本轮每个工具终态之后。
    async fn after_tools(
        &self,
        assistant: EntryId,
        results: Vec<EntryId>,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<(), BoxError>;
}

/// 内建 tool 任务的 hook（TS: `ToolHooks`）。
#[async_trait]
pub trait ToolHooks: Send + Sync {
    /// intent 之前。
    async fn before_tool(
        &self,
        call: ToolHookCall,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<Option<BeforeToolOutput>, BoxError>;
    /// 执行之后、结果提交之前。
    async fn after_tool(
        &self,
        call: ToolHookCall,
        result: ToolExecutionResult,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError>;
}

/// 内建 compaction 任务的 hook（TS: `CompactionHooks`）。
#[async_trait]
pub trait CompactionHooks: Send + Sync {
    /// 选择范围之后、摘要之前。
    async fn before_compact(
        &self,
        compaction: BeforeCompactInput,
        api: Arc<dyn HookApi>,
        context: &dyn Context,
    ) -> Result<Option<BeforeCompactDecision>, BoxError>;
}

// ---------------------------------------------------------------------------
// Cycle-breaking public shapes from view.ts / task-graph.ts / usage.ts / scheduler.ts
// ---------------------------------------------------------------------------

/// 一个会话的活动转录与内建文档的结构化挂载（TS: `ConversationView`，定义在 `view.ts`）。
///
/// 由 `harness/view.rs` 的 `build_mount` 构造：`conversation` 是会话记录，`entries` 是活动
/// 条目，`docs` 只包含 `mounted_tokens()` 中实际存在的文档（按 kind 键，缺席的不出现）。
/// 之后的 `watch()` 用增量 op 更新同一形状，而不是重新构造。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    /// 会话记录。
    pub conversation: ConversationRecord,
    /// 原始活动条目。
    pub entries: Vec<EntryRecord>,
    /// 内建会话文档，按 kind；缺席的文档不出现。
    pub docs: IndexMap<String, JsonObject>,
}

/// `Conversation.watch()` 的句柄。
pub type ConversationWatch = Arc<dyn crate::types::WatchHandle<ConversationView>>;

/// 活动任务的持久状态，去掉 checkpoint 与 outcome 负载（TS: `TaskGraphState`）。
///
/// 由 `harness/task_graph.rs` 的 `state_of` 从 `TaskState` 映射。终态任务会离开任务图
/// （`build` 只扫描活动状态），但映射函数遇到 `Terminal` 也会给出 `Completing` 形状。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum TaskGraphState {
    /// 可调度。
    Pending {
        /// 阶段。
        phase: String,
    },
    /// 正在运行。
    Running {
        /// 阶段。
        phase: String,
    },
    /// 等待任务。
    Waiting {
        /// 阶段。
        phase: String,
        /// 等待的任务。
        on: Vec<TaskId>,
        /// 策略。
        policy: JoinPolicy,
    },
    /// 睡眠。
    Sleeping {
        /// 阶段。
        phase: String,
        /// 毫秒时间点（epoch 毫秒）。
        until: i64,
    },
    /// 结果已定，等待普通工作收尾。
    Completing {
        /// 结果状态。
        outcome: TaskOutcomeStatus,
    },
}

/// 任务图里的一个节点（TS: `TaskGraphNode`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphNode {
    /// 任务 ID。
    pub id: TaskId,
    /// 任务 kind。
    pub kind: String,
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 拥有者任务；会话拥有的任务没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    /// background 标志。
    pub background: bool,
    /// abort 标记。
    pub abort_requested: bool,
    /// 状态。
    pub state: TaskGraphState,
    /// 它拥有的会话，按 ID 顺序。字段顺序即序列化顺序，与 TS `nodeOf()` 一致：
    /// `conversations` 在 `blocked` 之前。
    pub conversations: Vec<ConversationId>,
    /// 不能在这里运行的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockedReason>,
}

/// Session 的全部活动任务（TS: `TaskGraph`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskGraph {
    /// 每个活动任务，按十进制 ID 为键。
    ///
    /// `build` 先按状态扫描活任务，再按数值 `TaskId` 升序插入，因此迭代顺序是递增的任务 ID；
    /// 键是十进制字符串，但不按字典序排列（`10` 排在 `2` 之后）。
    pub tasks: IndexMap<String, TaskGraphNode>,
}

/// `Harness.watchTaskGraph()` 的句柄。
pub type TaskGraphWatch = Arc<dyn crate::types::WatchHandle<TaskGraph>>;

/// 任务不能运行的原因（TS `scheduler.ts`：`BlockedReason`）。
///
/// 由调度器的 `fit_record`/`blocked_reason()` 计算：同一原因既出现在任务图的
/// [`TaskGraphNode::blocked`]，也出现在 `inspect()` 的 `TaskInspectionState::Blocked`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    /// 没有注册的定义能接管它。
    MissingTask,
    /// 定义太旧且无法迁移。
    TaskTooOld,
    /// 迁移失败。
    MigrationFailed,
    /// restart 上限把它停泊。
    RestartLimit,
}

/// `blockedReason()` 读取的记录片段（TS: `BlockedSubject`）。
///
/// 它是任务记录的只读子集，由 `blocked_subject(&record)` 生成，交给 registry/调度器的
/// blocked 回调；`restarts` 达到 `restart_limit` 且没有 abort 标记时对应
/// [`BlockedReason::RestartLimit`]。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedSubject {
    /// 任务 ID。
    pub id: TaskId,
    /// kind。
    pub kind: String,
    /// 版本。
    pub version: i64,
    /// abort 标记。
    pub abort_requested: bool,
    /// 标记来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<TaskAbortReason>,
    /// 连续被打断次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u64>,
    /// 状态。
    pub status: TaskStatus,
}

/// 调度器不运行任务代码就直接写入的终态（TS: `SchedulerOutcome`，即
/// `Extract<TaskOutcome<JsonValue>, { status: "faulted" | "orphaned" }>`）。
///
/// 只保留调度器会写的两个变体：`completed`/`failed`/`aborted` 只能由任务代码提交。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SchedulerOutcome {
    /// 定义或迁移不可用，无法恢复。
    Orphaned {
        /// 原因。
        reason: String,
    },
    /// 运行期检测到的契约失败。
    Faulted {
        /// 错误。
        error: TaskOutcomeError,
    },
}

impl SchedulerOutcome {
    /// 转成任务记录里的终态（TS 的 `settleOutcome` 把它放进 `TaskState`）。
    pub fn into_task_outcome(self) -> TaskOutcome<JsonValue> {
        match self {
            SchedulerOutcome::Orphaned { reason } => TaskOutcome::Orphaned { reason },
            SchedulerOutcome::Faulted { error } => TaskOutcome::Faulted { error },
        }
    }
}

impl From<SchedulerOutcome> for TaskOutcome<JsonValue> {
    fn from(outcome: SchedulerOutcome) -> Self {
        outcome.into_task_outcome()
    }
}

/// 一个会话自己的用量账本（TS: `UsageState`，定义在 `usage.ts`）。
///
/// 值是累计量：`record_usage` 把每次 assistant 条目/摘要尝试或工具结果的 `Usage` 加到对应
/// 键上；`add_usage_state` 合并多个账本时按桶逐键相加（可选计数器只要任一侧报告过就求和）。
/// `models` 的键是 `provider/modelId`，`tools` 的键是工具名。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageState {
    /// assistant 条目与摘要尝试，按 `provider/modelId` 键。
    pub models: IndexMap<String, Usage>,
    /// 工具结果，按工具名键。
    pub tools: IndexMap<String, Usage>,
}
