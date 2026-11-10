//! `index.ts` 的再导出面。
//!
//! TS 的 `packages/durable/src/index.ts` 逐项列出包公开面；这里保持同样的顺序与分组。
//! 文档 token 在 TS 里是 `AgentDoc` 之类的值，Rust 侧是 [`AGENT_DOC`] 之类的静态量
//! （见 `harness/*.rs` 的 `*_DOC`），因此名字按 Rust 的常量拼写导出。
//!
//! # 这个文件负责什么
//!
//! 它只有 `pub use`（再导出），没有逻辑、类型定义或运行时状态。存在的目的有两个：
//!
//! 1. 让 crate 的公共名字与 TS 包入口一一对应，便于逐行对照移植进度；
//! 2. 让消费者可以 `use kiana_durable::index::TaskId` 这样的“包入口路径”。
//!
//! 来自 `types` 的那批名字（如 `TaskId`）在 crate 根也可以通过 `lib.rs` 的
//! `pub use types::*` 访问；两个路径指向同一个项，不是两套类型。`harness` / `session` /
//! `storage` 的项（如 `Harness`、`MemoryStorage`、`create_session`）没有根级别名，只能经
//! `crate::...` 或本模块访问。`index` 模块本身不引入新的可见性：它转出的每个项在
//! `types` / `harness` / `session` / `storage` 里本来就是公开的。
//!
//! # 改动约束
//!
//! - 增删导出时以 TS 的 `index.ts` 为准，并保持这里的顺序与分组。顺序不参与编译，
//!   但它是人工对照 TS 的唯一线索；重排会让差异审查变困难。
//! - TS 里同一个名字可以既是值又是类型（`Harness` 与 `HarnessType`）；Rust 一名一物，
//!   所以类型面单独以 [`HarnessType`] 导出（见文件末尾）。
//! - [`search`](crate::search)、[`truncate`](crate::truncate)、[`timers`](crate::timers)
//!   等模块是 `pub mod`，但 TS 的 `index.ts` 没有转出它们，因此这里也不列出；它们的
//!   公开项通过 crate 路径直接访问。
//!
//! # 注意
//!
//! 这个文件不参与任何运行时行为，改错导出只会在编译期或下游使用处暴露。不要在 `pub use`
//! 上方添加外层 `///`：目标项的文档由定义处提供，重复描述容易与源码脱节。

// `export type { WatchEnd, WatchHandle } from "@earendil-works/chord"`
pub use crate::types::{WatchEnd, WatchHandle};

// `export { defineDoc, defineDocFamily } from "./documents.ts"`
pub use crate::types::{define_doc, define_doc_family};

// `export { ... } from "./entries.ts"`
pub use crate::types::{
    AssistantEntry, CompactionEntry, ResetEntry, SystemEntry, ToolResultEntry, UserEntry,
    define_entry,
};

// `export { ConversationBusy, ReadAfterWrite, ReadOnlySession, SessionFailed, StorageRequestError } from "./errors.ts"`
pub use crate::types::{
    ConversationBusy, ReadAfterWrite, ReadOnlySession, SessionFailed, StorageRequestError,
};

// `export { AgentDoc, configure, DEFAULT_* } from "./harness/agent.ts"`
pub use crate::harness::agent::{
    AGENT_DOC, DEFAULT_COMPACTION_POLICY, DEFAULT_PROGRESS_POLICY, DEFAULT_RESTART_LIMIT,
    DEFAULT_RETRY_POLICY, configure,
};

// `export { ... } from "./harness/compaction.ts"`
pub use crate::harness::compaction::{
    COMPACTION_TASK, CompactionCheckpoint, CompactionInput, SummaryRequest,
};

// `export { defineExtension, defineTool, hook, section, wrapSection, wrapTool } from "./harness/define.ts"`
pub use crate::harness::define::{
    define_extension, define_tool, hook, section, wrap_section, wrap_tool,
};

// `export { ... , watchEvents } from "./harness/events.ts"`
pub use crate::harness::events::{
    AgentEvent, AgentEventStream, MessageChange, SnapshotEvent, ToolEventCall, watch_events,
};

// `export { ... } from "./harness/generation.ts"`
pub use crate::harness::generation::{
    GENERATION_TASK, GenerationCheckpoint, GenerationInput, GenerationResult,
};

// `export { DEFAULT_HARNESS_IDENTITY, Harness } from "./harness/harness.ts"`
pub use crate::harness::harness::{DEFAULT_HARNESS_IDENTITY, Harness};

// `export { InboxDoc, type InboxItem, type InboxState } from "./harness/inbox.ts"`
pub use crate::harness::inbox::{INBOX_DOC, InboxItem, InboxState};

// `export { ... } from "./harness/live.ts"`
pub use crate::harness::live::{
    CompactionFailure, CompactionStatus, LIVE_DOC, LiveState, NestedToolSlot, NestedToolSummary,
    ToolSlot,
};

// `export { ProviderDoc, type ProviderState } from "./harness/provider.ts"`
pub use crate::harness::provider::{PROVIDER_DOC, ProviderState};

// `export { createRegistry } from "./harness/registry.ts"`
pub use crate::harness::registry::create_registry;

// `export type { BlockedReason } from "./harness/scheduler.ts"`
pub use crate::harness::scheduler::BlockedReason;

// `export type { TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch } from "./harness/task-graph.ts"`
pub use crate::harness::task_graph::{TaskGraph, TaskGraphNode, TaskGraphState, TaskGraphWatch};

// `export { DURABLE_TELEMETRY_SCHEMA } from "./harness/telemetry.ts"`
pub use crate::harness::telemetry::DURABLE_TELEMETRY_SCHEMA;

// `export { NestedCallDoc, ..., ToolTask, ... } from "./harness/tool.ts"`
pub use crate::harness::tool::{
    NESTED_CALL_DOC, NestedCallState, TOOL_TASK, ToolTaskCheckpoint, ToolTaskInput, ToolTaskResult,
};

// harness/types.ts 的类型面（TS 的 type-only 导出）。
pub use crate::harness::types::{
    AfterFailedRun, Agent, AgentChange, AgentState, AnyTask, CompactionHooks, CompactionPolicy,
    CompactionReason, CompactionResult, CompactionSettings, ContextView, Conversation,
    ConversationAbortOptions, ConversationCreateOptions, ConversationHandle, ConversationInit,
    ConversationRetryPolicy, ConversationStreamOptions, ConversationWatch, EnvTarget, Extension,
    GenerationHooks, HarnessInspection, HarnessOptions, HarnessSettings, HookApi, HookRegistration,
    HookResult, HooksOf, InputEntry, InputSubmissionDraft, ModelRef, NestedToolExecutionResult,
    ProgressPolicy, PromptInput, PromptSection, QueueMode, Registry, RegistryReader,
    RegistrySnapshot, Settings, SettledSubmissionRecord, SettledTask, Submission, SubmissionDraft,
    TaskInspection, ToolControl, ToolDiagnostic, ToolExecutionApi, ToolExecutionMode,
    ToolExecutionResult, ToolHookCall, ToolHooks, ToolRegistration, UserInput, Wake, Wrap,
};

// `export { UsageDoc, type UsageState } from "./harness/usage.ts"`
pub use crate::harness::usage::{USAGE_DOC, UsageState};

// `export type { ConversationView } from "./harness/view.ts"`
pub use crate::harness::types::ConversationView;

// `export { createSession } from "./session/session.ts"`
pub use crate::session::session::create_session;

// `export { MemoryStorage } from "./storage/memory.ts"`
pub use crate::storage::memory::MemoryStorage;

// `export { defineTask } from "./tasks.ts"`
pub use crate::types::tasks::define_task;

// `export type { ... } from "./types.ts"`（顺序同 TS）。
pub use crate::types::{
    CheckpointInfo, CommitChange, CommitOptions, CommitPersistence, CommitPublication,
    CommonDocDefinition, ContextEdit, ConversationDocFamilyToken, ConversationDocToken,
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor,
    DocDefinition, DocFamilyDefinition, DocFamilyToken, DocToken, DocumentAddress,
    DocumentCommitChange, DocumentContent, DocumentCopySource, DocumentCreate, DocumentId,
    DocumentObserver, DocumentPoint, DocumentQuery, DocumentReader, DocumentRecord,
    DocumentSemantics, DocumentWatch, Entry, EntryDraft, EntryId, EntryQuery, EntryRecord,
    HookRunner, HostCallback, Id, JoinPolicy, JsonObject, LatestConversationSemantics,
    NextTaskState, Page, PhaseHandler, ReportFrom, ReportInfo, ReportTask,
    RewindableConversationDocFamilyToken, RewindableConversationDocToken,
    RewindableConversationSemantics, RunningTask, ScanOrder, Seq, Session, SessionDocFamilyToken,
    SessionDocToken, SessionEnd, SetTimer, Storage, StorageWrite, StoredDocument, SubmissionCreate,
    SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionSettlement, TableCommitChange, Task,
    TaskDefinition, TaskDocFamilyToken, TaskDocToken, TaskId, TaskOptions, TaskOutcome,
    TaskOutcomeError, TaskOwnership, TaskQuery, TaskRecord, TaskRuntime, TaskState, Tx, TypedEntry,
    TypedEntryDraft,
};

// `export { ROOT_CONVERSATION_ID } from "./types.ts"`
pub use crate::types::ROOT_CONVERSATION_ID;

// TS 的 `Harness` 既是值（`{ open }`）也是类型（`HarnessType`）。Rust 一名一物：值面是
// [`Harness`]，类型面（trait）以 [`HarnessType`] 导出。
pub use crate::harness::types::Harness as HarnessType;
