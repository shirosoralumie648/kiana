//! 子代理运行时（第 11 周前半）。
//!
//! 模块拆分：
//!
//! - [`canonical_tool`]：工具命名空间、通配模式、`ToolPermissionSet` 交集；
//! - [`tool_pool`]：子代理不可变工具表、schema 与 dispatch 二次校验；
//! - [`permissions`]：父/子权限模式收窄、headless Ask→Deny；
//! - [`isolation`]：`IsolationMode`、baseline/delta patch、ownership、worktree/reflink/
//!   overlayfs；
//! - [`transcript`]：parent/sidechain 链接、resume 消息去重；
//! - [`result`]：terminal + stop_reason、`try_set_terminal`、usage/receipt 累积；
//! - [`yield_tool`]：yield JSON Schema 校验与 label 增量装配；
//! - [`runtime`]：把上述能力组装成可调用的 [`runtime::SubagentRuntime`]。
//!
//! 上层入口：`AgentSession::subagent_parent_context()` /
//! `AgentSession::prepare_subagent()` / `AgentSession::resume_subagent()`。

pub mod canonical_tool;
pub mod isolation;
pub mod permissions;
pub mod result;
pub mod runner;
pub mod runtime;
pub mod stages;
pub mod tool_pool;
pub mod transcript;
pub mod work;
pub mod work_graph;
pub mod work_store;
pub mod yield_tool;

pub use canonical_tool::{
    CanonicalToolId, ToolNamespace, ToolPattern, ToolPermissionSet, ToolSelection,
};
pub use isolation::{
    BaselineFile, BaselineSkip, IsolatedWorkspace, IsolationBackend, IsolationCapabilities,
    IsolationError, IsolationMode, IsolationOptions, NestedBaseline, NestedDelta, RepoBaseline,
    SweepReport, WorkspaceBaseline, WorkspaceDelta, WorkspaceOwnership, apply_delta,
    capture_baseline, capture_delta, create_workspace, list_ownership_records,
    ownership_record_path, repository_root, sweep_ownership_records,
    sweep_ownership_records_with_keep, workspace_has_changes, workspace_has_ownership_record,
};
pub use permissions::{
    SubagentApprovalMode, SubagentPermissionContext, SubagentPermissionDecision, stricter_mode,
};
pub use result::{
    ReceiptAccumulator, ReceiptStatus, SubagentStopReason, SubagentStreamChunk, SubagentTerminal,
    SubagentTerminalState, SubagentUsage, TerminalCell, ToolReceipt, UsageAccumulator,
};
pub use runner::{
    SubagentIsolationOutcome, SubagentRunOptions, SubagentRunOutcome, run_subagent, usage_from,
};
pub use runtime::{
    ProviderCapabilities, SubagentError, SubagentParentContext, SubagentRuntime,
    SubagentRuntimeOptions, TokenBudget,
};
pub use tool_pool::{ToolAccessError, ToolPool, ToolSchema};
pub use transcript::{
    SUBAGENT_LINK_CUSTOM_TYPE, SUBAGENT_SPAWNED_CUSTOM_TYPE, SubagentLink, SubagentTranscript,
    TranscriptError, record_spawn_in_parent,
};
pub use work::{
    BackgroundWorkHandle, MemoryWorkStore, RunId, SystemWorkClock, WORK_PROTOCOL_VERSION,
    WorkAcceptance, WorkAcceptancePolicy, WorkApproval, WorkClock, WorkEvent, WorkEventKind,
    WorkEventSink, WorkEvidence, WorkId, WorkIsolation, WorkMode, WorkOptions, WorkPlanId,
    WorkRecovery, WorkRequest, WorkResult, WorkRunOutcome, WorkStore, WorkStoreError, WorkTerminal,
    WorkUsage, WorkValidationError, build_work_request, encode_work_event, encode_work_request,
    encode_work_result, new_run_id, next_work_id,
};
pub use work_graph::{
    GraphRefresh, WorkGraph, WorkGraphEdge, WorkGraphNode, WorkGraphSet, WorkGraphState,
    WorkGraphView,
};
pub use work_store::{
    DurableWorkStore, WORK_RECORD_VERSION, WorkBegin, WorkFinish, WorkRecord, WorkRecoveryReport,
    WorkState,
};
pub use yield_tool::{
    YieldAssembler, YieldContract, YieldError, YieldOutcome, YieldValidationMode,
    create_yield_tool, create_yield_tool_with_state,
};
