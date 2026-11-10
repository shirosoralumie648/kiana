//! Work envelope v1：宿主驱动子代理运行的身份与结果契约。
//!
//! 本模块是 `docs/adr/work-orchestration.md` Phase 0 的实现：
//!
//! - [`WorkRequest`] 只按名字引用 [`crate::agent_definition::AgentDefinition`]，不复制
//!   profile 字段；真正的执行仍由 `AgentSession::spawn_subagent_with_work` 完成。
//! - [`WorkResult`] 用穷尽 tagged union [`WorkTerminal`] 表达终态，避免裸字符串状态；
//!   它不替代 `kiana-durable` 的 `TaskRecord`，只是宿主可见的不可变结果信封。
//! - [`WorkEventKind`] 是库内 best-effort 事件流，**不是**权威状态；权威状态由
//!   durable 的 `TaskRecord` 在接线后提供。
//! - 所有 wire 字节都经 `kiana-json`（`normalize_numbers` + `stringify`），
//!   不直接使用 `serde_json::to_string`。
//!
//! 默认路径不受影响：只有显式调用 `spawn_subagent_with_work` 并传入 [`WorkOptions`]
//! 时才会产生 Work 身份、事件与持久化；普通 `spawn_subagent` 的行为保持不变。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde::{Deserialize, Serialize};

use kiana_ai::types::JsonValue;

use crate::acceptance::{AcceptanceReport, AcceptanceStatus};
use crate::agent_definition::AgentDefinition;
use crate::subagent::isolation::{IsolationBackend, IsolationMode};
use crate::subagent::result::{SubagentTerminal, SubagentUsage, ToolReceipt};
use crate::subagent::runner::{SubagentIsolationOutcome, SubagentRunOutcome};
use crate::subagent::runtime::SubagentRuntimeOptions;
use crate::subagent::work_store::{WorkBegin, WorkFinish, WorkRecord, WorkState};

/// Work 协议版本。
pub const WORK_PROTOCOL_VERSION: u32 = 1;

static NEXT_WORK_ID: AtomicI64 = AtomicI64::new(1);

/// Work 的稳定数字身份。
///
/// Phase 0 使用进程内单调计数；Phase 1 接到 durable 后，它等于 `TaskId::get()`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkId(pub i64);

impl WorkId {
    /// 构造。
    pub fn new(value: i64) -> Self {
        Self(value)
    }
}

/// 一次执行的稳定字符串身份。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub String);

impl RunId {
    /// 构造。
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// 计划身份（Phase 3 的 Work Graph 使用；Phase 0 只保留字段位置）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkPlanId(pub String);

impl WorkPlanId {
    /// 构造。
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// 委派模式，语义参考 Google ADK 2.0 的 chat / task / single_turn。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    /// 对话式委派。
    Chat,
    /// 单次任务委派（默认；子代理必须是 leaf）。
    #[default]
    Task,
    /// 单轮、可并行多路委派。
    SingleTurn,
}

/// 审批模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkApproval {
    /// 无交互 UI：`Ask` → `Deny`（默认）。
    #[default]
    Headless,
    /// 交互式审批；Phase 0 在提交阶段拒绝。
    Interactive,
}

/// 恢复策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRecovery {
    /// 失败即终止，不重放有副作用的工具（默认）。
    #[default]
    Fail,
    /// 显式恢复；Phase 1 才接线。
    Resume,
}

/// 验收策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAcceptancePolicy {
    /// 只报告验收结果，不因失败改变 Work 终态（默认）。
    #[default]
    ReportOnly,
    /// 任一可判定 leaf 失败即 Work 失败；要求 criteria 非空。
    RequirePassed,
}

/// 单次 Work 的请求信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRequest {
    /// 协议版本。
    pub protocol: u32,
    /// Work 身份。
    pub work_id: WorkId,
    /// 本次执行身份。
    pub run_id: RunId,
    /// 计划身份（Phase 3 的 Work Graph / 事件关联）；缺省不参与编码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<WorkPlanId>,
    /// `AgentDefinition` 的名字；不复制 profile 字段。
    pub profile: String,
    /// 任务文本。
    pub prompt: String,
    /// 委派模式。
    pub mode: WorkMode,
    /// 审批模式。
    pub approval: WorkApproval,
    /// 恢复策略。
    pub recovery: WorkRecovery,
    /// 验收策略。
    pub acceptance_policy: WorkAcceptancePolicy,
    /// 验收 criteria。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    /// 宿主声明的工作目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// 隔离模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationMode>,
    /// 嵌套深度。
    pub depth: u32,
    /// turn 上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// token 预算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// 超时（秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// 工具调用上限（Phase 2 才执行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<u64>,
    /// 是否只读。
    #[serde(default)]
    pub read_only: bool,
    /// 是否允许恢复。
    #[serde(default)]
    pub resumable: bool,
}

impl WorkRequest {
    /// 校验协议不变量。错误文本不会进入 TS 可见路径。
    pub fn validate(&self) -> Result<(), WorkValidationError> {
        let problem = if self.protocol != WORK_PROTOCOL_VERSION {
            Some(format!(
                "unsupported work protocol {}; expected {WORK_PROTOCOL_VERSION}",
                self.protocol
            ))
        } else if self.work_id.0 <= 0 {
            Some("workId must be positive".to_string())
        } else if self.run_id.0.trim().is_empty() {
            Some("runId cannot be empty".to_string())
        } else if self.profile.trim().is_empty() {
            Some("profile cannot be empty".to_string())
        } else if self.prompt.trim().is_empty() {
            Some("prompt cannot be empty".to_string())
        } else if self.approval == WorkApproval::Interactive {
            Some("interactive approval is not supported in work protocol v1".to_string())
        } else if self.acceptance_policy == WorkAcceptancePolicy::RequirePassed
            && self
                .acceptance_criteria
                .iter()
                .all(|criterion| criterion.trim().is_empty())
        {
            Some("require_passed needs at least one acceptance criterion".to_string())
        } else if self.timeout_seconds == Some(0) {
            Some("timeoutSeconds must be positive".to_string())
        } else if self.max_calls == Some(0) {
            Some("maxCalls must be positive".to_string())
        } else {
            None
        };
        match problem {
            Some(message) => Err(WorkValidationError { message }),
            None => Ok(()),
        }
    }
}

/// Work 协议校验错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkValidationError {
    /// 原因。
    pub message: String,
}

/// Work 终态：穷尽 tagged union。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkTerminal {
    /// 正常完成。
    Success {
        /// 结构化输出（yield 契约存在时）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<JsonValue>,
    },
    /// 失败。
    Failed {
        /// 错误文本。
        error: String,
        /// 提前结束原因（token/turn/loop 等）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "stop_reason"
        )]
        stop_reason: Option<String>,
    },
    /// 挂起（Phase 2+ 的审批/暂停使用）。
    Suspended {
        /// 原因。
        reason: String,
    },
    /// 被取消。
    Cancelled {
        /// 原因。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// 超时。
    TimedOut {
        /// 原因。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// 因依赖失败而未执行。
    Skipped {
        /// 原因。
        reason: String,
    },
}

impl WorkTerminal {
    /// 稳定状态字符串。
    pub fn status(&self) -> &'static str {
        match self {
            Self::Success { .. } => "success",
            Self::Failed { .. } => "failed",
            Self::Suspended { .. } => "suspended",
            Self::Cancelled { .. } => "cancelled",
            Self::TimedOut { .. } => "timed_out",
            Self::Skipped { .. } => "skipped",
        }
    }
}

/// Work 的 usage 记账。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkUsage {
    /// 净输入 token。
    pub input_tokens: u64,
    /// 输出 token。
    pub output_tokens: u64,
    /// 缓存命中 token。
    pub cache_read_tokens: u64,
    /// 缓存写入 token。
    pub cache_write_tokens: u64,
    /// 总 token。
    pub total_tokens: u64,
    /// 费用（USD）。
    ///
    /// 非有限值（provider 的浮点加法可溢出成 `inf`）在落盘前被钳成 0；读取侧也把
    /// JSON `null`（`serde_json` 对非有限 f64 的默认编码）还原为 0，避免一条坏
    /// usage 让整个账本的 `list`/`recover` 永久失败。
    #[serde(deserialize_with = "deserialize_finite_cost")]
    pub cost: f64,
    /// 工具调用次数。
    pub tool_calls: u64,
    /// turn 数（Phase 0 暂不可得，保持 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
}

/// 把非有限费用钳成 0：账本必须可解码，`inf`/`NaN` 在 JSON 里没有表示。
fn finite_cost(cost: f64) -> f64 {
    if cost.is_finite() { cost } else { 0.0 }
}

fn deserialize_finite_cost<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // JSON `null` 是旧记录里非有限 f64 的落盘形态：按 0 读取以自愈，而不是让整条
    // 记录解码失败、把整个账本卡在损坏状态。
    let value = Option::<f64>::deserialize(deserializer)?;
    Ok(value.map(finite_cost).unwrap_or(0.0))
}

impl WorkUsage {
    /// 从子代理 usage 构造。
    pub fn from_subagent(usage: &SubagentUsage, tool_calls: u64) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            total_tokens: usage.total_tokens,
            cost: finite_cost(usage.cost),
            tool_calls,
            turns: None,
        }
    }
}

/// 一条可审计的工具执行证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkEvidence {
    /// 稳定序号。
    pub seq: u64,
    /// 工具名。
    pub tool: String,
    /// 参数 JSON 的 sha256。
    pub args_sha256: String,
    /// 输出 JSON 的 sha256。
    pub output_sha256: String,
    /// 执行状态。
    pub status: String,
}

impl From<&ToolReceipt> for WorkEvidence {
    fn from(receipt: &ToolReceipt) -> Self {
        Self {
            seq: receipt.seq,
            tool: receipt.tool.clone(),
            args_sha256: receipt.args_sha256.clone(),
            output_sha256: receipt.output_sha256.clone(),
            status: receipt.status.as_str().to_string(),
        }
    }
}

/// 验收结果与策略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkAcceptance {
    /// 宿主声明的策略。
    pub policy: WorkAcceptancePolicy,
    /// 确定性验收报告。
    pub report: AcceptanceReport,
}

/// 隔离工作区结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkIsolation {
    /// 工作目录。
    pub path: String,
    /// 实际后端。
    pub backend: IsolationBackend,
    /// 是否保留。
    pub retained: bool,
    /// 有改动但 ownership 记录写入失败时的错误。
    ///
    /// `retained=false` 既可能是“没有改动”，也可能是“有改动但保留记录写失败”；
    /// 丢掉这个字段会让宿主把后者当成可以安全丢弃的无改动工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_error: Option<String>,
}

impl From<&SubagentIsolationOutcome> for WorkIsolation {
    fn from(outcome: &SubagentIsolationOutcome) -> Self {
        Self {
            path: outcome.path.clone(),
            backend: outcome.backend,
            retained: outcome.retained,
            retain_error: outcome.retain_error.clone(),
        }
    }
}

/// 一次 Work 的不可变结果信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkResult {
    /// 协议版本。
    pub protocol: u32,
    /// Work 身份。
    pub work_id: WorkId,
    /// 本次执行身份；`Skipped`/依赖失败路径可以为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// 终态。
    pub terminal: WorkTerminal,
    /// usage。
    pub usage: WorkUsage,
    /// 验收结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<WorkAcceptance>,
    /// 工具证据。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<WorkEvidence>,
    /// 持久化会话文件（内存会话为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    /// 隔离工作区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<WorkIsolation>,
    /// 开始时间（毫秒）。
    pub started_at_ms: u64,
    /// 结束时间（毫秒）。
    pub finished_at_ms: u64,
}

impl WorkResult {
    /// 从子代理运行结果构造。
    pub fn from_outcome(
        request: &WorkRequest,
        outcome: &SubagentRunOutcome,
        started_at_ms: u64,
        finished_at_ms: u64,
    ) -> Self {
        let acceptance = outcome.acceptance.clone();
        let mut terminal = terminal_from_outcome(outcome);
        // `RequirePassed` 是宿主声明的硬约束，但只覆盖“本来成功”的 Work：取消/超时/
        // 失败是更高优先级的事实，不能被验收失败改写成 Failed（否则
        // `spawn_subagent_with_work` 就再也无法把超时映射成 TimedOut）。
        if request.acceptance_policy == WorkAcceptancePolicy::RequirePassed
            && matches!(terminal, WorkTerminal::Success { .. })
            && acceptance
                .as_ref()
                .is_some_and(|report| report.status == AcceptanceStatus::Failed)
        {
            terminal = WorkTerminal::Failed {
                error: "acceptance criteria failed".to_string(),
                stop_reason: outcome
                    .terminal
                    .stop_reason
                    .map(|reason| reason.as_str().to_string()),
            };
        }
        Self {
            protocol: request.protocol,
            work_id: request.work_id,
            run_id: Some(request.run_id.clone()),
            terminal,
            usage: WorkUsage::from_subagent(&outcome.usage, outcome.receipts.len() as u64),
            acceptance: acceptance.map(|report| WorkAcceptance {
                policy: request.acceptance_policy,
                report,
            }),
            evidence: outcome.receipts.iter().map(WorkEvidence::from).collect(),
            session_file: outcome.session_file.clone(),
            isolation: outcome.isolation.as_ref().map(WorkIsolation::from),
            started_at_ms,
            finished_at_ms,
        }
    }

    /// 构造一个失败的 WorkResult（运行在准备阶段就失败时使用）。
    pub fn failure(
        request: &WorkRequest,
        error: impl Into<String>,
        started_at_ms: u64,
        finished_at_ms: u64,
    ) -> Self {
        Self {
            protocol: request.protocol,
            work_id: request.work_id,
            run_id: Some(request.run_id.clone()),
            terminal: WorkTerminal::Failed {
                error: error.into(),
                stop_reason: None,
            },
            usage: WorkUsage::default(),
            acceptance: None,
            evidence: Vec::new(),
            session_file: None,
            isolation: None,
            started_at_ms,
            finished_at_ms,
        }
    }

    /// 构造一个“进程丢失”中断的 WorkResult（启动 sweep 落定遗留 Running 记录）。
    ///
    /// 保留子代理持久会话文件，供宿主审计或后续显式恢复；不假装有 acceptance 或
    /// tool evidence。
    pub fn interrupted(
        request: &WorkRequest,
        error: impl Into<String>,
        session_file: Option<String>,
        started_at_ms: u64,
        finished_at_ms: u64,
    ) -> Self {
        Self {
            protocol: request.protocol,
            work_id: request.work_id,
            run_id: Some(request.run_id.clone()),
            terminal: WorkTerminal::Failed {
                error: error.into(),
                stop_reason: Some("interrupted".to_string()),
            },
            usage: WorkUsage::default(),
            acceptance: None,
            evidence: Vec::new(),
            session_file,
            isolation: None,
            started_at_ms,
            finished_at_ms,
        }
    }
}

fn terminal_from_outcome(outcome: &SubagentRunOutcome) -> WorkTerminal {
    match outcome.terminal.terminal {
        SubagentTerminal::Completed => WorkTerminal::Success {
            output: outcome
                .output
                .clone()
                .or_else(|| outcome.terminal.result.clone().map(JsonValue::String)),
        },
        SubagentTerminal::Failed => WorkTerminal::Failed {
            error: outcome
                .terminal
                .error
                .clone()
                .unwrap_or_else(|| "subagent failed".to_string()),
            stop_reason: outcome
                .terminal
                .stop_reason
                .map(|reason| reason.as_str().to_string()),
        },
        SubagentTerminal::Cancelled => WorkTerminal::Cancelled {
            reason: outcome.terminal.error.clone(),
        },
        SubagentTerminal::TimedOut | SubagentTerminal::PollingTimedOut => WorkTerminal::TimedOut {
            reason: outcome.terminal.error.clone(),
        },
    }
}

/// Work 事件种类。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkEventKind {
    /// 已创建。
    Created,
    /// 已开始。
    Started {
        /// profile 名。
        profile: String,
    },
    /// 验收已判定。
    AcceptanceEvaluated {
        /// 状态字符串。
        status: String,
    },
    /// 已到达终态。
    Terminal {
        /// 状态字符串。
        status: String,
    },
}

/// 库内 Work 事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkEvent {
    /// 协议版本。
    pub protocol: u32,
    /// 事件序号（1-based）。
    pub seq: u64,
    /// 时间（毫秒）。
    pub at_ms: u64,
    /// Work 身份。
    pub work_id: WorkId,
    /// 执行身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// 计划身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<WorkPlanId>,
    /// 事件体。
    pub event: WorkEventKind,
}

/// 事件接收器。实现必须是非阻塞的；慢消费者不应阻塞子代理运行。
pub trait WorkEventSink: Send + Sync {
    /// 接收一个事件。
    fn emit(&self, event: &WorkEvent);
}

/// Work 生命周期的持久化接口。
///
/// Phase 1 起它是 Work 的写权威：`begin` 在运行前写入 Running 记录，`finish` 用一次
/// CAS 落定终态。Phase 0 的终态归档语义由 [`MemoryWorkStore`] 的同一接口保留；
/// durable 实现见 [`crate::subagent::work_store::DurableWorkStore`]。
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait WorkStore: Send + Sync {
    /// 分配一个会话内唯一的 WorkId（durable 实现用文档内计数器）。
    async fn allocate_work_id(&self, parent_session_id: &str) -> Result<WorkId, WorkStoreError>;

    /// 写入 Running 记录；同 WorkId 已存在时返回 `Existing`，不覆盖。
    async fn begin(&self, record: WorkRecord) -> Result<WorkBegin, WorkStoreError>;

    /// 在运行前补齐子代理持久会话文件。
    async fn attach_session(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
        session_file: &str,
    ) -> Result<(), WorkStoreError>;

    /// 归还 `begin` 占用的恢复锚点。
    ///
    /// 恢复的后继在真正运行前就 `begin`，以便在账本事务里原子占用锚点；若随后
    /// 打开 transcript / 准备隔离工作区失败，这次后继从未执行，锚点必须还回去，
    /// 否则宿主永远无法重试同一个原记录。返回是否真的清除了占用。
    async fn release_resume(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
    ) -> Result<bool, WorkStoreError>;

    /// Running → Terminal 的 CAS；只有赢家写入结果。
    async fn finish(
        &self,
        parent_session_id: &str,
        result: &WorkResult,
    ) -> Result<WorkFinish, WorkStoreError>;

    /// 读取一条记录。
    async fn load(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
    ) -> Result<Option<WorkRecord>, WorkStoreError>;

    /// 读取本会话所有 Running 记录（崩溃恢复用）。
    async fn list_recoverable(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError>;

    /// 读取本会话全部记录（诊断/面板用，按创建时间升序）。
    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError>;
}

/// Work 存储错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkStoreError {
    /// 原因。
    pub message: String,
}

impl WorkStoreError {
    /// 构造。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Default)]
struct MemoryWorkStoreState {
    /// 每个父会话独立的“最后分配到的 id”，与 durable 的 per-session 计数器一致。
    last_work_id: HashMap<String, i64>,
    records: Vec<WorkRecord>,
}

/// 进程内 WorkStore（测试与宿主可选实现）。
pub struct MemoryWorkStore {
    state: tokio::sync::Mutex<MemoryWorkStoreState>,
    /// 与 `DurableWorkStore` 一致：`updatedAtMs` 使用注入时钟，测试里不会出现
    /// 系统时钟跳变。
    clock: Arc<dyn WorkClock>,
}

impl std::fmt::Debug for MemoryWorkStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryWorkStore")
            .finish_non_exhaustive()
    }
}

impl Default for MemoryWorkStore {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(MemoryWorkStoreState::default()),
            clock: Arc::new(SystemWorkClock),
        }
    }
}

impl MemoryWorkStore {
    /// 新建（系统时钟）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用指定时钟（测试可注入固定值）。
    pub fn with_clock(mut self, clock: Arc<dyn WorkClock>) -> Self {
        self.clock = clock;
        self
    }

    /// 读取已终态的记录（按保存顺序），供 Phase 0 兼容测试使用。
    pub async fn records(&self) -> Vec<(WorkRequest, WorkResult)> {
        self.state
            .lock()
            .await
            .records
            .iter()
            .filter_map(|record| {
                record
                    .terminal_result()
                    .map(|result| (record.request.clone(), result.clone()))
            })
            .collect()
    }

    /// 读取全部记录（运行中与终态）。
    pub async fn all_records(&self) -> Vec<WorkRecord> {
        self.state.lock().await.records.clone()
    }
}

#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
impl WorkStore for MemoryWorkStore {
    async fn allocate_work_id(&self, parent_session_id: &str) -> Result<WorkId, WorkStoreError> {
        let mut state = self.state.lock().await;
        let last = state
            .last_work_id
            .get(parent_session_id)
            .copied()
            .unwrap_or(0)
            .max(0);
        let next = last
            .checked_add(1)
            .ok_or_else(|| WorkStoreError::new("work id exhausted"))?;
        state
            .last_work_id
            .insert(parent_session_id.to_owned(), next);
        Ok(WorkId(next))
    }

    async fn begin(&self, record: WorkRecord) -> Result<WorkBegin, WorkStoreError> {
        // 与 durable 实现相同的入参校验：记录身份必须与请求一致、只能从 running
        // 开始，否则替换存储实现会改变可观测行为。
        if record.work_id != record.request.work_id || record.run_id != record.request.run_id {
            return Err(WorkStoreError::new(
                "work record identity does not match request",
            ));
        }
        if !matches!(record.state, WorkState::Running { .. }) {
            return Err(WorkStoreError::new(
                "work record must start in running state",
            ));
        }
        let mut state = self.state.lock().await;
        if let Some(existing) = state.records.iter().find(|existing| {
            existing.work_id == record.work_id
                && existing.parent_session_id == record.parent_session_id
        }) {
            return Ok(WorkBegin::Existing(existing.clone()));
        }
        // 与 durable 实现一致：`i64::MAX` 没有可推进的计数器，显式 begin 在写入前
        // 就报告耗尽，否则下一次 allocate 会永久失败。
        if record.work_id.0.checked_add(1).is_none() {
            return Err(WorkStoreError::new("work id exhausted"));
        }
        // 与 durable 实现一致：一个恢复锚点只能有一个后继。锚点按父会话隔离：
        // 另一个会话里的同一个 WorkId 与本会话无关，不能阻断本次 begin。
        if let Some(origin) = record.resumed_from
            && let Some(existing) = state.records.iter().find(|existing| {
                existing.parent_session_id == record.parent_session_id
                    && existing.resumed_from == Some(origin)
            })
        {
            return Ok(WorkBegin::AlreadyResumed(existing.clone()));
        }
        // 与 durable 实现一致：显式 workId 之后的下一次分配必须跳过它。
        let last = state
            .last_work_id
            .get(&record.parent_session_id)
            .copied()
            .unwrap_or(0);
        if record.work_id.0 > last {
            state
                .last_work_id
                .insert(record.parent_session_id.clone(), record.work_id.0);
        }
        state.records.push(record.clone());
        Ok(WorkBegin::Started(record))
    }

    async fn attach_session(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
        session_file: &str,
    ) -> Result<(), WorkStoreError> {
        let mut state = self.state.lock().await;
        let Some(record) = state.records.iter_mut().find(|record| {
            record.work_id == work_id
                && record.run_id == *run_id
                && record.parent_session_id == parent_session_id
        }) else {
            return Err(WorkStoreError::new(format!(
                "work {} run {} does not exist",
                work_id.0, run_id.0
            )));
        };
        let WorkState::Running {
            session_file: current,
        } = &mut record.state
        else {
            return Err(WorkStoreError::new(format!(
                "work {} run {} is already terminal",
                work_id.0, run_id.0
            )));
        };
        *current = Some(session_file.to_owned());
        record.updated_at_ms = self.clock.now_ms();
        Ok(())
    }

    async fn finish(
        &self,
        parent_session_id: &str,
        result: &WorkResult,
    ) -> Result<WorkFinish, WorkStoreError> {
        let mut state = self.state.lock().await;
        let Some(record) = state.records.iter_mut().find(|record| {
            record.work_id == result.work_id && record.parent_session_id == parent_session_id
        }) else {
            return Ok(WorkFinish::Missing);
        };
        let matches_run = result
            .run_id
            .as_ref()
            .is_none_or(|run_id| *run_id == record.run_id);
        if !matches_run || record.is_terminal() {
            return Ok(WorkFinish::NotApplied(record.clone()));
        }
        record.state = WorkState::Terminal {
            result: Box::new(result.clone()),
            terminal_seq: record.terminal_seq().saturating_add(1),
        };
        record.updated_at_ms = self.clock.now_ms();
        Ok(WorkFinish::Finished(record.clone()))
    }

    async fn load(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
    ) -> Result<Option<WorkRecord>, WorkStoreError> {
        Ok(self
            .state
            .lock()
            .await
            .records
            .iter()
            .find(|record| {
                record.work_id == work_id && record.parent_session_id == parent_session_id
            })
            .cloned())
    }

    async fn release_resume(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
    ) -> Result<bool, WorkStoreError> {
        let mut state = self.state.lock().await;
        // 与 durable 实现一致：记录本身还有后继时不能归还锚点，否则后继（或它的
        // 后继）可能仍在写同一 transcript/工作区，而上游记录已被允许并发恢复。
        if state.records.iter().any(|existing| {
            existing.parent_session_id == parent_session_id
                && existing.resumed_from == Some(work_id)
        }) {
            return Ok(false);
        }
        let Some(record) = state.records.iter_mut().find(|record| {
            record.parent_session_id == parent_session_id
                && record.work_id == work_id
                && record.run_id == *run_id
        }) else {
            return Ok(false);
        };
        let released = record.resumed_from.take().is_some();
        if released {
            record.updated_at_ms = self.clock.now_ms();
        }
        Ok(released)
    }

    async fn list_recoverable(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError> {
        Ok(self
            .state
            .lock()
            .await
            .records
            .iter()
            .filter(|record| record.parent_session_id == parent_session_id && !record.is_terminal())
            .cloned()
            .collect())
    }

    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError> {
        // 与 durable 实现相同的排序：宿主看到的顺序不能随存储实现分叉。
        let mut records = self
            .state
            .lock()
            .await
            .records
            .iter()
            .filter(|record| record.parent_session_id == parent_session_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.work_id.0.cmp(&right.work_id.0))
                .then_with(|| left.run_id.0.cmp(&right.run_id.0))
        });
        Ok(records)
    }
}

/// 时间源；测试可注入固定值以获得确定的 wire。
pub trait WorkClock: Send + Sync {
    /// 当前毫秒时间。
    fn now_ms(&self) -> u64;
}

/// 系统时钟。
#[derive(Debug, Default)]
pub struct SystemWorkClock;

impl WorkClock for SystemWorkClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// 创建 Work 的选项（只影响 opt-in 的 Work 路径）。
#[derive(Clone)]
pub struct WorkOptions {
    /// 覆盖 WorkId；缺省使用进程内计数。
    pub work_id: Option<WorkId>,
    /// 覆盖 RunId；缺省使用 UUID。
    pub run_id: Option<RunId>,
    /// 委派模式。
    pub mode: WorkMode,
    /// 审批模式。
    pub approval: WorkApproval,
    /// 恢复策略。
    pub recovery: WorkRecovery,
    /// 验收策略。
    pub acceptance_policy: WorkAcceptancePolicy,
    /// 超时（秒）；到点取消子代理并把终态映射为 `TimedOut`。
    pub timeout_seconds: Option<u64>,
    /// 计划身份；写入 `WorkRequest` 并随 `WorkEvent` 一起投影。
    pub plan_id: Option<WorkPlanId>,
    /// 事件接收器。
    pub event_sink: Option<Arc<dyn WorkEventSink>>,
    /// 结果存储。
    pub store: Option<Arc<dyn WorkStore>>,
    /// 时间源。
    pub clock: Option<Arc<dyn WorkClock>>,
}

impl Default for WorkOptions {
    fn default() -> Self {
        Self {
            work_id: None,
            run_id: None,
            mode: WorkMode::Task,
            approval: WorkApproval::Headless,
            recovery: WorkRecovery::Fail,
            acceptance_policy: WorkAcceptancePolicy::ReportOnly,
            timeout_seconds: None,
            plan_id: None,
            event_sink: None,
            store: None,
            clock: None,
        }
    }
}

impl std::fmt::Debug for WorkOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkOptions")
            .field("work_id", &self.work_id)
            .field("run_id", &self.run_id)
            .field("mode", &self.mode)
            .field("approval", &self.approval)
            .field("recovery", &self.recovery)
            .field("acceptance_policy", &self.acceptance_policy)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("plan_id", &self.plan_id)
            .field("has_event_sink", &self.event_sink.is_some())
            .field("has_store", &self.store.is_some())
            .field("has_clock", &self.clock.is_some())
            .finish()
    }
}

impl WorkOptions {
    /// 设置固定 WorkId。
    pub fn with_work_id(mut self, work_id: WorkId) -> Self {
        self.work_id = Some(work_id);
        self
    }

    /// 设置固定 RunId。
    pub fn with_run_id(mut self, run_id: RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// 设置超时。
    pub fn with_timeout_seconds(mut self, seconds: u64) -> Self {
        self.timeout_seconds = Some(seconds);
        self
    }

    /// 设置计划身份。
    pub fn with_plan_id(mut self, plan_id: WorkPlanId) -> Self {
        self.plan_id = Some(plan_id);
        self
    }

    /// 设置事件接收器。
    pub fn with_event_sink(mut self, sink: Arc<dyn WorkEventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// 设置结果存储。
    pub fn with_store(mut self, store: Arc<dyn WorkStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// 设置时间源。
    pub fn with_clock(mut self, clock: Arc<dyn WorkClock>) -> Self {
        self.clock = Some(clock);
        self
    }
}

/// 后台 Work 的句柄：后台入口立即返回 task 与 Work 身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundWorkHandle {
    /// 后台 task id（输出文件与取消键）。
    pub task_id: String,
    /// Work 账本 id。
    pub work_id: WorkId,
    /// 本次运行 id。
    pub run_id: RunId,
    /// 后台输出文件路径（终态结构化输出写入这里）。
    pub output_path: std::path::PathBuf,
}

/// 一次 Work 运行的返回值。
#[derive(Debug, Clone)]
pub struct WorkRunOutcome {
    /// 提交时的请求快照。
    pub request: WorkRequest,
    /// 不可变结果信封。
    pub result: WorkResult,
    /// 底层子代理运行结果（宿主仍可读取 messages/receipts）。
    pub outcome: SubagentRunOutcome,
    /// 存储失败信息；存储失败不影响运行结果。
    pub store_error: Option<String>,
}

/// 从子代理定义与运行选项构造请求。
pub fn build_work_request(
    definition: &AgentDefinition,
    runtime_options: &SubagentRuntimeOptions,
    prompt: &str,
    workdir: &Path,
    options: &WorkOptions,
) -> WorkRequest {
    let isolation = runtime_options.isolation.unwrap_or(definition.isolation);
    WorkRequest {
        protocol: WORK_PROTOCOL_VERSION,
        work_id: options.work_id.unwrap_or_else(next_work_id),
        run_id: options.run_id.clone().unwrap_or_else(new_run_id),
        plan_id: options.plan_id.clone(),
        profile: definition.name.clone(),
        prompt: prompt.to_string(),
        mode: options.mode,
        approval: options.approval,
        recovery: options.recovery,
        acceptance_policy: options.acceptance_policy,
        acceptance_criteria: runtime_options.acceptance_criteria.clone(),
        workdir: Some(workdir.to_string_lossy().into_owned()),
        isolation: Some(isolation),
        depth: runtime_options.depth,
        max_turns: definition.max_turns,
        token_budget: runtime_options.token_budget,
        timeout_seconds: options.timeout_seconds,
        max_calls: None,
        read_only: definition.is_read_only(),
        resumable: matches!(options.recovery, WorkRecovery::Resume),
    }
}

/// 分配一个进程内 WorkId。
pub fn next_work_id() -> WorkId {
    WorkId(NEXT_WORK_ID.fetch_add(1, Ordering::SeqCst))
}

/// 分配一个 RunId。
pub fn new_run_id() -> RunId {
    RunId(format!("run-{}", uuid::Uuid::new_v4()))
}

/// 发出一个 Work 事件；没有 sink 时是 no-op。
pub(crate) fn emit_work_event(
    sink: &Option<Arc<dyn WorkEventSink>>,
    clock: &Arc<dyn WorkClock>,
    request: &WorkRequest,
    seq: &mut u64,
    event: WorkEventKind,
) {
    let Some(sink) = sink else {
        return;
    };
    *seq = seq.saturating_add(1);
    sink.emit(&WorkEvent {
        protocol: request.protocol,
        seq: *seq,
        at_ms: clock.now_ms(),
        work_id: request.work_id,
        run_id: Some(request.run_id.clone()),
        plan_id: request.plan_id.clone(),
        event,
    });
}

/// 把请求编码成 kiana-json 字节。
pub fn encode_work_request(request: &WorkRequest) -> String {
    encode_value(request)
}

/// 把结果编码成 kiana-json 字节。
pub fn encode_work_result(result: &WorkResult) -> String {
    encode_value(result)
}

/// 把事件编码成 kiana-json 字节。
pub fn encode_work_event(event: &WorkEvent) -> String {
    encode_value(event)
}

fn encode_value<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).expect("work envelope is serializable");
    kiana_json::stringify(&kiana_json::normalize_numbers(value))
}

#[cfg(test)]
mod tests {
    /// 回归（round-16 finding #5）：宿主必须能区分“无改动”与“有改动但保留记录
    /// 写失败”。后者说明工作区仍在本进程里受 `Drop` 保护，但下一个进程可能回收它。
    #[test]
    fn work_isolation_keeps_the_retain_error() {
        use crate::subagent::isolation::IsolationBackend;

        let outcome = crate::subagent::SubagentIsolationOutcome {
            path: "/tmp/workspace".to_string(),
            backend: IsolationBackend::Reflink,
            retained: false,
            retain_error: Some("ownership record write failed".to_string()),
        };
        let isolation = WorkIsolation::from(&outcome);
        assert_eq!(
            isolation.retain_error.as_deref(),
            Some("ownership record write failed")
        );
    }
    use super::*;
    use crate::subagent::result::{
        ReceiptStatus, SubagentStopReason, SubagentTerminalState, ToolReceipt,
    };
    use crate::subagent::runner::SubagentRunOutcome;
    use serde_json::json;

    fn request() -> WorkRequest {
        WorkRequest {
            protocol: WORK_PROTOCOL_VERSION,
            work_id: WorkId(42),
            run_id: RunId::new("run-1"),
            plan_id: None,
            profile: "general-purpose".to_string(),
            prompt: "run the tests".to_string(),
            mode: WorkMode::Task,
            approval: WorkApproval::Headless,
            recovery: WorkRecovery::Fail,
            acceptance_policy: WorkAcceptancePolicy::ReportOnly,
            acceptance_criteria: vec!["tests_passed:cargo test".to_string()],
            workdir: Some("/tmp/work".to_string()),
            isolation: Some(IsolationMode::Shared),
            depth: 1,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            max_calls: None,
            read_only: false,
            resumable: false,
        }
    }

    fn outcome(terminal: SubagentTerminalState) -> SubagentRunOutcome {
        SubagentRunOutcome {
            terminal,
            messages: Vec::new(),
            usage: SubagentUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 2,
                cache_write_tokens: 1,
                total_tokens: 18,
                cost: 0.01,
            },
            receipts: vec![ToolReceipt {
                tool: "bash".to_string(),
                args_sha256: "a".repeat(64),
                output_sha256: "b".repeat(64),
                status: ReceiptStatus::Succeeded,
                seq: 1,
            }],
            output: Some(json!({"answer": 42})),
            acceptance: None,
            session_file: None,
            isolation: None,
        }
    }

    #[test]
    fn work_id_is_a_bare_number() {
        assert_eq!(serde_json::to_value(WorkId(42)).expect("value"), json!(42));
        assert_eq!(
            serde_json::to_value(RunId::new("run-1")).expect("value"),
            json!("run-1")
        );
    }

    #[test]
    fn request_wire_uses_camel_case_and_omits_none() {
        let wire = encode_work_request(&request());
        assert!(wire.contains("\"workId\":42"), "{wire}");
        assert!(wire.contains("\"runId\":\"run-1\""), "{wire}");
        assert!(
            wire.contains("\"acceptancePolicy\":\"report_only\""),
            "{wire}"
        );
        assert!(wire.contains("\"isolation\":\"shared\""), "{wire}");
        assert!(!wire.contains("planId"), "{wire}");
        assert!(!wire.contains("timeoutSeconds"), "{wire}");
    }

    #[test]
    fn validation_rejects_interactive_and_empty_require_passed() {
        let mut value = request();
        value.approval = WorkApproval::Interactive;
        assert!(value.validate().is_err());

        let mut value = request();
        value.acceptance_policy = WorkAcceptancePolicy::RequirePassed;
        value.acceptance_criteria = vec!["   ".to_string()];
        assert!(value.validate().is_err());

        let mut value = request();
        value.timeout_seconds = Some(0);
        assert!(value.validate().is_err());
    }

    #[test]
    fn terminal_is_a_tagged_union() {
        let result = WorkResult::from_outcome(
            &request(),
            &outcome(SubagentTerminalState::completed("ok")),
            1,
            2,
        );
        let wire = encode_work_result(&result);
        assert!(wire.contains("\"status\":\"success\""), "{wire}");
        assert!(wire.contains("\"output\":{\"answer\":42}"), "{wire}");
        assert!(wire.contains("\"toolCalls\":1"), "{wire}");
    }

    #[test]
    fn terminal_failure_fields_use_camel_case() {
        let terminal = WorkTerminal::Failed {
            error: "boom".to_string(),
            stop_reason: Some("token_capped".to_string()),
        };
        let value = serde_json::to_value(&terminal).expect("value");
        assert_eq!(value["status"], "failed");
        assert_eq!(value["stopReason"], "token_capped");
        assert!(value.get("stop_reason").is_none());
    }

    #[test]
    fn require_passed_leaves_non_success_terminals_untouched() {
        let mut request = request();
        request.acceptance_policy = WorkAcceptancePolicy::RequirePassed;
        request.acceptance_criteria = vec!["file:a.txt exists".to_string()];
        let cases = [
            (
                SubagentTerminalState::with_terminal(
                    SubagentTerminal::Cancelled,
                    Some("cancelled".to_string()),
                ),
                "cancelled",
            ),
            (
                SubagentTerminalState::with_terminal(
                    SubagentTerminal::TimedOut,
                    Some("timed out".to_string()),
                ),
                "timed_out",
            ),
        ];
        for (terminal, expected) in cases {
            let mut outcome = outcome(terminal);
            outcome.acceptance = Some(AcceptanceReport {
                leaves: Vec::new(),
                status: AcceptanceStatus::Failed,
                unchecked: Vec::new(),
            });
            let result = WorkResult::from_outcome(&request, &outcome, 1, 2);
            assert_eq!(
                result.terminal.status(),
                expected,
                "acceptance failure must not override a non-success terminal"
            );
        }
    }

    #[test]
    fn require_passed_failure_turns_the_work_terminal_into_failure() {
        let mut request = request();
        request.acceptance_policy = WorkAcceptancePolicy::RequirePassed;
        request.acceptance_criteria = vec!["file:a.txt exists".to_string()];
        let mut outcome = outcome(SubagentTerminalState::completed("ok"));
        outcome.acceptance = Some(AcceptanceReport {
            leaves: Vec::new(),
            status: AcceptanceStatus::Failed,
            unchecked: Vec::new(),
        });

        let result = WorkResult::from_outcome(&request, &outcome, 1, 2);
        assert!(
            matches!(result.terminal, WorkTerminal::Failed { .. }),
            "RequirePassed must fail the Work: {:?}",
            result.terminal
        );

        // ReportOnly 保持子代理自己的终态。
        request.acceptance_policy = WorkAcceptancePolicy::ReportOnly;
        let result = WorkResult::from_outcome(&request, &outcome, 1, 2);
        assert_eq!(result.terminal.status(), "success");

        // Unverified（没有可判定 leaf）不算失败断言：策略只约束可判定失败。
        outcome.acceptance.as_mut().expect("acceptance").status = AcceptanceStatus::Unverified;
        request.acceptance_policy = WorkAcceptancePolicy::RequirePassed;
        let result = WorkResult::from_outcome(&request, &outcome, 1, 2);
        assert_eq!(result.terminal.status(), "success");
    }

    #[test]
    fn interrupted_result_is_failed_with_stop_reason() {
        let request = request();
        let result = WorkResult::interrupted(
            &request,
            "work interrupted by process loss",
            Some("/tmp/child.jsonl".to_string()),
            10,
            20,
        );
        assert_eq!(result.terminal.status(), "failed");
        match &result.terminal {
            WorkTerminal::Failed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("interrupted"));
            }
            other => panic!("unexpected terminal: {other:?}"),
        }
        assert_eq!(result.session_file.as_deref(), Some("/tmp/child.jsonl"));
        assert!(result.acceptance.is_none());
        assert!(result.evidence.is_empty());
    }

    #[test]
    fn skipped_result_has_no_run_id() {
        let mut value = request();
        value.run_id = RunId::new("run-1");
        let result = WorkResult {
            protocol: WORK_PROTOCOL_VERSION,
            work_id: value.work_id,
            run_id: None,
            terminal: WorkTerminal::Skipped {
                reason: "dependency failed".to_string(),
            },
            usage: WorkUsage::default(),
            acceptance: None,
            evidence: Vec::new(),
            session_file: None,
            isolation: None,
            started_at_ms: 1,
            finished_at_ms: 2,
        };
        let wire = encode_work_result(&result);
        assert!(!wire.contains("runId"), "{wire}");
        assert!(wire.contains("\"status\":\"skipped\""), "{wire}");
    }

    #[test]
    fn terminal_mapping_covers_all_subagent_states() {
        let completed = terminal_from_outcome(&outcome(SubagentTerminalState::completed("ok")));
        assert_eq!(completed.status(), "success");
        let failed = terminal_from_outcome(&outcome(SubagentTerminalState::failed("boom")));
        assert_eq!(failed.status(), "failed");
        let cancelled = terminal_from_outcome(&outcome(SubagentTerminalState::with_terminal(
            SubagentTerminal::Cancelled,
            Some("stop".to_string()),
        )));
        assert_eq!(cancelled.status(), "cancelled");
        let timed_out = terminal_from_outcome(&outcome(SubagentTerminalState::with_terminal(
            SubagentTerminal::TimedOut,
            Some("slow".to_string()),
        )));
        assert_eq!(timed_out.status(), "timed_out");
        let capped = terminal_from_outcome(&outcome(
            SubagentTerminalState::failed("budget")
                .with_stop_reason(SubagentStopReason::TokenCapped),
        ));
        match capped {
            WorkTerminal::Failed { stop_reason, .. } => {
                assert_eq!(stop_reason.as_deref(), Some("token_capped"));
            }
            other => panic!("unexpected terminal: {other:?}"),
        }
    }

    #[test]
    fn event_wire_has_stable_envelope() {
        let event = WorkEvent {
            protocol: WORK_PROTOCOL_VERSION,
            seq: 1,
            at_ms: 1_700_000_000_000,
            work_id: WorkId(7),
            run_id: Some(RunId::new("run-7")),
            plan_id: None,
            event: WorkEventKind::Created,
        };
        let wire = encode_work_event(&event);
        assert_eq!(
            wire,
            "{\"protocol\":1,\"seq\":1,\"atMs\":1700000000000,\"workId\":7,\"runId\":\"run-7\",\"event\":{\"type\":\"created\"}}"
        );
    }

    #[tokio::test]
    async fn memory_store_scopes_records_and_counters_by_session() {
        let store = MemoryWorkStore::new();
        let work_a = store.allocate_work_id("session-a").await.expect("a");
        let work_b = store.allocate_work_id("session-b").await.expect("b");
        assert_eq!(work_a, WorkId(1));
        assert_eq!(work_b, WorkId(1), "counters are per session");

        let mut request_a = request();
        request_a.work_id = work_a;
        request_a.run_id = RunId::new("run-a");
        let mut request_b = request();
        request_b.work_id = work_b;
        request_b.run_id = RunId::new("run-b");
        let record_a = WorkRecord::new(request_a, "session-a", None, None, 1);
        let record_b = WorkRecord::new(request_b.clone(), "session-b", None, None, 2);

        assert!(matches!(
            store.begin(record_a).await.expect("begin a"),
            WorkBegin::Started(_)
        ));
        assert!(
            matches!(
                store.begin(record_b).await.expect("begin b"),
                WorkBegin::Started(_)
            ),
            "the same work id in another session must not collide"
        );

        let result_b = WorkResult::failure(&request_b, "boom", 1, 2);
        store
            .finish("session-b", &result_b)
            .await
            .expect("finish b");
        assert!(
            !store
                .load("session-a", work_a)
                .await
                .expect("load a")
                .expect("record a")
                .is_terminal(),
            "finishing session-b must not terminalize session-a"
        );
        assert!(
            store
                .load("session-b", work_b)
                .await
                .expect("load b")
                .expect("record b")
                .is_terminal()
        );
    }

    #[tokio::test]
    async fn memory_store_anchor_is_scoped_to_its_session_and_can_be_released() {
        let store = MemoryWorkStore::new();
        let mut anchor_request = request();
        anchor_request.work_id = WorkId(1);
        anchor_request.run_id = RunId::new("run-anchor");
        store
            .begin(WorkRecord::new(anchor_request, "session-a", None, None, 1))
            .await
            .expect("anchor");

        // 另一个会话里的同一 WorkId 不是本会话的锚点，不能阻断 begin。
        let mut other_request = request();
        other_request.work_id = WorkId(9);
        other_request.run_id = RunId::new("run-other");
        let other =
            WorkRecord::new(other_request, "session-b", None, None, 2).with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(other).await.expect("other session"),
            WorkBegin::Started(_)
        ));

        let mut successor_request = request();
        successor_request.work_id = WorkId(2);
        successor_request.run_id = RunId::new("run-2");
        let successor = WorkRecord::new(successor_request, "session-a", None, None, 3)
            .with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(successor).await.expect("successor"),
            WorkBegin::Started(_)
        ));

        let mut retry_request = request();
        retry_request.work_id = WorkId(3);
        retry_request.run_id = RunId::new("run-3");
        let retry =
            WorkRecord::new(retry_request, "session-a", None, None, 4).with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(retry.clone()).await.expect("blocked"),
            WorkBegin::AlreadyResumed(_)
        ));
        assert!(
            store
                .release_resume("session-a", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release")
        );
        assert!(matches!(
            store.begin(retry).await.expect("retry"),
            WorkBegin::Started(_)
        ));
    }

    /// 与 durable 实现一致（round-17 finding #1）：记录仍被后继占用时拒绝归还
    /// 锚点，否则上游记录会在后继运行时被并发恢复。
    #[tokio::test]
    async fn memory_store_release_is_refused_while_a_successor_claims_the_record() {
        let store = MemoryWorkStore::new();
        for (work_id, run_id, resumed_from) in [
            (1_i64, "run-1", None),
            (2_i64, "run-2", Some(WorkId(1))),
            (3_i64, "run-3", Some(WorkId(2))),
        ] {
            let mut request = request();
            request.work_id = WorkId(work_id);
            request.run_id = RunId::new(run_id);
            let mut record = WorkRecord::new(request, "session-a", None, None, work_id as u64);
            if let Some(origin) = resumed_from {
                record = record.with_resumed_from(origin);
            }
            store.begin(record).await.expect("begin");
        }

        assert!(
            !store
                .release_resume("session-a", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release blocked"),
            "W3 still claims W2"
        );
        assert!(
            store
                .release_resume("session-a", WorkId(3), &RunId::new("run-3"))
                .await
                .expect("release 3")
        );
        assert!(
            store
                .release_resume("session-a", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release 2")
        );
    }

    struct FixedWorkClock(u64);

    impl WorkClock for FixedWorkClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    /// round-18 finding #12：MemoryWorkStore 与 DurableWorkStore 一样使用注入时钟。
    #[tokio::test]
    async fn memory_store_uses_the_injected_clock() {
        let store = MemoryWorkStore::new().with_clock(Arc::new(FixedWorkClock(12_345)));
        let mut request = request();
        request.work_id = WorkId(1);
        request.run_id = RunId::new("run-clock");
        store
            .begin(WorkRecord::new(
                request.clone(),
                "session-clock",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");
        store
            .attach_session(
                "session-clock",
                WorkId(1),
                &RunId::new("run-clock"),
                "child.jsonl",
            )
            .await
            .expect("attach");
        let attached = store
            .load("session-clock", WorkId(1))
            .await
            .expect("load")
            .expect("record");
        assert_eq!(attached.updated_at_ms, 12_345);

        store
            .finish(
                "session-clock",
                &WorkResult::failure(&request, "boom", 1, 2),
            )
            .await
            .expect("finish");
        let finished = store
            .load("session-clock", WorkId(1))
            .await
            .expect("load")
            .expect("record");
        assert_eq!(finished.updated_at_ms, 12_345);
    }

    /// round-18 finding #7：显式 `i64::MAX` 与 durable 实现一样在写入前拒绝。
    #[tokio::test]
    async fn memory_store_rejects_the_unrepresentable_next_id() {
        let store = MemoryWorkStore::new();
        let mut request = request();
        request.work_id = WorkId(i64::MAX);
        request.run_id = RunId::new("run-max");
        let error = store
            .begin(WorkRecord::new(request, "session-max", None, None, 1))
            .await
            .expect_err("i64::MAX must be rejected");
        assert!(error.to_string().contains("work id exhausted"), "{error}");
    }

    #[tokio::test]
    async fn memory_store_lists_records_in_durable_order() {
        let store = MemoryWorkStore::new();
        for (work_id, created_at_ms) in [(12, 2_000), (11, 1_000)] {
            let mut request = request();
            request.work_id = WorkId(work_id);
            request.run_id = RunId::new(format!("run-{work_id}"));
            store
                .begin(WorkRecord::new(
                    request,
                    "session-order",
                    None,
                    None,
                    created_at_ms,
                ))
                .await
                .expect("begin");
        }
        let records = store.list_for_session("session-order").await.expect("list");
        assert_eq!(
            records
                .iter()
                .map(|record| record.work_id.0)
                .collect::<Vec<_>>(),
            vec![11, 12],
            "lists must be sorted by createdAtMs, not insertion order"
        );
    }

    #[tokio::test]
    async fn memory_store_allocation_skips_explicit_work_ids() {
        let store = MemoryWorkStore::new();
        let mut request = request();
        request.work_id = WorkId(100);
        request.run_id = RunId::new("run-100");
        store
            .begin(WorkRecord::new(request, "session-explicit", None, None, 1))
            .await
            .expect("begin");
        assert_eq!(
            store
                .allocate_work_id("session-explicit")
                .await
                .expect("allocate"),
            WorkId(101)
        );
    }

    #[tokio::test]
    async fn memory_store_persists_request_and_result() {
        let store = MemoryWorkStore::new();
        let request = request();
        let result = WorkResult::from_outcome(
            &request,
            &outcome(SubagentTerminalState::completed("ok")),
            1,
            2,
        );
        let record = crate::subagent::work_store::WorkRecord::new(
            request.clone(),
            "session-1",
            None,
            Some(std::process::id()),
            1,
        );
        store.begin(record).await.expect("begin");
        store.finish("session-1", &result).await.expect("finish");
        let records = store.records().await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0.work_id, request.work_id);
        assert_eq!(records[0].1.terminal.status(), "success");
    }

    #[tokio::test]
    async fn memory_store_rejects_invalid_begin_records() {
        let store = MemoryWorkStore::new();
        let request = request();

        let mut mismatched = WorkRecord::new(request.clone(), "session-1", None, None, 1);
        mismatched.run_id = RunId::new("run-other");
        assert!(
            store.begin(mismatched).await.is_err(),
            "record identity must match the request"
        );

        let mut terminal = WorkRecord::new(request.clone(), "session-1", None, None, 1);
        terminal.state = WorkState::Terminal {
            result: Box::new(WorkResult::failure(&request, "boom", 1, 2)),
            terminal_seq: 1,
        };
        assert!(
            store.begin(terminal).await.is_err(),
            "begin must only write running records"
        );
    }

    #[tokio::test]
    async fn memory_store_advances_updated_at_on_transitions() {
        let store = MemoryWorkStore::new();
        let mut request = request();
        request.resumable = true;
        let mut record = WorkRecord::new(request.clone(), "session-1", None, None, 1)
            .with_resumed_from(WorkId(7));
        record.updated_at_ms = 1;
        store.begin(record).await.expect("begin");
        std::thread::sleep(std::time::Duration::from_millis(2));
        store
            .attach_session("session-1", request.work_id, &request.run_id, "child.jsonl")
            .await
            .expect("attach");
        let attached = store
            .load("session-1", request.work_id)
            .await
            .expect("load")
            .expect("record");
        assert!(
            attached.updated_at_ms > 1,
            "attach must advance updatedAtMs"
        );

        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(
            store
                .release_resume("session-1", request.work_id, &request.run_id)
                .await
                .expect("release")
        );
        let released = store
            .load("session-1", request.work_id)
            .await
            .expect("load")
            .expect("record");
        assert!(
            released.updated_at_ms > attached.updated_at_ms,
            "release must advance updatedAtMs"
        );

        std::thread::sleep(std::time::Duration::from_millis(2));
        let result = WorkResult::failure(&request, "boom", 1, 2);
        store.finish("session-1", &result).await.expect("finish");
        let finished = store
            .load("session-1", request.work_id)
            .await
            .expect("load")
            .expect("record");
        assert!(
            finished.updated_at_ms > released.updated_at_ms,
            "finish must advance updatedAtMs"
        );
    }
}
