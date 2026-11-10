//! 子代理运行时：独立 tool pool、system prompt、permission context、token budget、
//! transcript 与终态。
//!
//! 生命周期：
//!
//! 1. 父会话构造 [`SubagentParentContext`]（工具快照、权限策略、模型、provider 能力）；
//! 2. [`SubagentRuntime::prepare`] 解析定义、收窄工具、建立子会话与隔离工作区；
//! 3. 调用方把 `system_prompt()` 交给模型，用 `dispatch_tool()` 执行工具；
//! 4. 流式 chunk 通过 [`SubagentRuntime::record_chunk`] 累积 usage/receipt；
//! 5. 结束时 `try_set_terminal(...)` 写入唯一终态。
//!
//! # fork 与 prompt cache
//!
//! fork 子代理（复用父 transcript 前缀）只有在**三个条件同时满足**时才共享 prompt
//! cache：`fork == true`、调用方显式 `share_prompt_cache == true`、provider 声明
//! `prompt_cache == true`。默认关闭——共享前缀会改变请求形状，属于 provider 能力，
//! 不应由子代理定义隐式开启。
//!
//! # 隔离失败
//!
//! `isolation != Shared` 且工作区创建失败时，`prepare` 返回
//! [`SubagentError::Isolation`]，绝不静默回退到共享目录。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_agent_core::types::{AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{JsonValue, Model, Usage};
use kiana_permissions::{PermissionMode, PermissionPolicy, PermissionRequest};
use tokio_util::sync::CancellationToken;

use crate::acceptance::{ExecutionRecord, is_shell_tool};
use crate::agent_definition::AgentDefinition;
use crate::continuity::{receipt_status, tool_result_text};

use super::canonical_tool::{CanonicalToolId, ToolPermissionSet};
use super::isolation::{
    IsolatedWorkspace, IsolationError, IsolationMode, IsolationOptions, adopt_workspace,
    claim_workspace_ownership, create_workspace, workspace_has_ownership_record,
};
use super::permissions::{
    SubagentApprovalMode, SubagentPermissionContext, SubagentPermissionDecision,
};
use super::result::{
    ReceiptAccumulator, ReceiptStatus, SubagentStreamChunk, SubagentTerminal,
    SubagentTerminalState, SubagentUsage, TerminalCell, UsageAccumulator,
};
use super::tool_pool::{ToolAccessError, ToolPool, ToolSchema};
use super::transcript::{SubagentLink, SubagentTranscript, TranscriptError};
use super::yield_tool::YieldContract;

/// provider 能力声明（只放与子代理相关的位）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// provider 是否支持 prompt cache。
    pub prompt_cache: bool,
}

/// 父会话传给子代理的上下文快照。
#[derive(Clone, Debug)]
pub struct SubagentParentContext {
    /// 父会话 id。
    pub parent_session_id: String,
    /// 父会话 cwd。
    pub cwd: PathBuf,
    /// 父会话当前可调用工具的不可变快照。
    pub tools: ToolPool,
    /// 父会话允许的工具集合（`None`/all 表示不额外限制）。
    pub allowed_tools: ToolPermissionSet,
    /// 父会话权限策略；`None` 表示父会话没有启用权限层。
    pub permission_policy: Option<PermissionPolicy>,
    /// 父会话有效权限模式。
    pub permission_mode: PermissionMode,
    /// 父会话模型（用于统计/后续模型选择）。
    pub model: Option<Model>,
    /// provider 能力。
    pub provider_capabilities: ProviderCapabilities,
    /// 父会话里注册的扩展/SDK 工具名（可能遮蔽内建工具名）。
    pub custom_tools: Vec<String>,
    /// 父会话的 `shellCommandPrefix`：`bash` 实际执行 `prefix + "\n" + command`，
    /// 权限检查必须看到同一份文本。
    pub shell_command_prefix: Option<String>,
    /// MCP 工具名 → annotations 快照。子代理权限引擎据此执行
    /// destructive/openWorld 的强制确认与 read-only 模式的 readOnlyHint 放宽。
    pub mcp_annotations:
        std::collections::BTreeMap<String, kiana_ext_api::tool_types::ToolAnnotations>,
}

impl SubagentParentContext {
    /// 用工具快照构造（默认不限制、无权限层、Default 模式）。
    pub fn new(
        parent_session_id: impl Into<String>,
        cwd: impl Into<PathBuf>,
        tools: ToolPool,
    ) -> Self {
        Self {
            parent_session_id: parent_session_id.into(),
            cwd: cwd.into(),
            tools,
            allowed_tools: ToolPermissionSet::all(),
            permission_policy: None,
            permission_mode: PermissionMode::Default,
            model: None,
            provider_capabilities: ProviderCapabilities::default(),
            custom_tools: Vec::new(),
            shell_command_prefix: None,
            mcp_annotations: std::collections::BTreeMap::new(),
        }
    }

    /// 设置父允许集合。
    pub fn with_allowed_tools(mut self, allowed: ToolPermissionSet) -> Self {
        self.allowed_tools = allowed;
        self
    }

    /// 设置权限策略（模式以策略为准）。
    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_mode = policy.mode;
        self.permission_policy = Some(policy);
        self
    }

    /// 设置模型。
    pub fn with_model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    /// 设置 provider 能力。
    pub fn with_provider_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.provider_capabilities = capabilities;
        self
    }

    /// 设置父会话的 `shellCommandPrefix`。
    pub fn with_shell_command_prefix(mut self, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        self.shell_command_prefix = (!prefix.is_empty()).then_some(prefix);
        self
    }
}

/// 创建子代理的选项。
#[derive(Debug, Clone)]
pub struct SubagentRuntimeOptions {
    /// 持久化目录；`None` 表示内存会话。
    pub session_dir: Option<PathBuf>,
    /// 子会话 id；缺省 UUID。
    pub session_id: Option<String>,
    /// sidechain id；缺省 `<child>-sidechain`。
    pub sidechain_id: Option<String>,
    /// 嵌套深度（直接子代理为 1）。
    pub depth: u32,
    /// 审批能力。
    pub approval: SubagentApprovalMode,
    /// token 预算（累计可计费 token）。
    pub token_budget: Option<u64>,
    /// 是否为 fork（复用父 transcript 前缀）。
    pub fork: bool,
    /// 是否显式请求共享 prompt cache（仍需 provider 支持）。
    pub share_prompt_cache: bool,
    /// 覆盖 agent 定义的 isolation。
    pub isolation: Option<IsolationMode>,
    /// 隔离工作区选项。
    pub isolation_options: IsolationOptions,
    /// yield 契约（可选）。
    pub yield_contract: Option<YieldContract>,
    /// 子代理终态时要判定的 acceptance criteria（空列表仍生成 `Unverified` 报告）。
    pub acceptance_criteria: Vec<String>,
}

impl Default for SubagentRuntimeOptions {
    fn default() -> Self {
        Self {
            session_dir: None,
            session_id: None,
            sidechain_id: None,
            depth: 1,
            approval: SubagentApprovalMode::Headless,
            token_budget: None,
            fork: false,
            share_prompt_cache: false,
            isolation: None,
            isolation_options: IsolationOptions::default(),
            yield_contract: None,
            acceptance_criteria: Vec::new(),
        }
    }
}

/// token 预算。
#[derive(Debug)]
pub struct TokenBudget {
    limit: Option<u64>,
    used: AtomicU64,
}

impl TokenBudget {
    /// 新建。
    pub fn new(limit: Option<u64>) -> Self {
        Self {
            limit,
            used: AtomicU64::new(0),
        }
    }

    /// 上限。
    pub fn limit(&self) -> Option<u64> {
        self.limit
    }

    /// 已用。
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    /// 剩余（无上限时为 `None`）。
    pub fn remaining(&self) -> Option<u64> {
        self.limit.map(|limit| limit.saturating_sub(self.used()))
    }

    /// 是否已耗尽。
    pub fn is_exhausted(&self) -> bool {
        self.limit.is_some_and(|limit| self.used() >= limit)
    }

    /// 记录消耗；返回是否仍在预算内。
    pub fn consume(&self, tokens: u64) -> bool {
        let used = self.used.fetch_add(tokens, Ordering::SeqCst) + tokens;
        self.limit.is_none_or(|limit| used <= limit)
    }
}

/// 子代理运行时错误。
#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    /// transcript 错误。
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    /// 隔离错误。
    #[error(transparent)]
    Isolation(#[from] IsolationError),
    /// 工具不可用。
    #[error("tool `{tool}` is unavailable: {source}")]
    ToolUnavailable {
        /// 工具名。
        tool: String,
        /// 原因。
        #[source]
        source: ToolAccessError,
    },
    /// 权限拒绝。
    #[error("permission denied for `{tool}`: {reason}")]
    PermissionDenied {
        /// 工具名。
        tool: String,
        /// 原因。
        reason: String,
    },
    /// 需要父会话审批（交互模式）。
    #[error("subagent requires approval for `{tool}`: {prompt}")]
    NeedsApproval {
        /// 工具名。
        tool: String,
        /// 审批提示。
        prompt: String,
        /// 决策原因。
        reason: String,
    },
    /// 工具执行失败（agent loop 通常会把它转成 failed tool result）。
    #[error("tool `{tool}` failed: {message}")]
    ToolFailed {
        /// 工具名。
        tool: String,
        /// 错误消息。
        message: String,
    },
    /// resume 的会话不属于当前 agent/父会话。
    #[error("cannot resume subagent session: {message}")]
    ResumeMismatch {
        /// 原因。
        message: String,
    },
    /// Work 幂等键 `(parentSessionId, workId)` 已存在；宿主可用
    /// [`crate::subagent::WorkStore::load`] 读取已有记录。
    #[error("work {work_id} already exists (status {status}, run {run_id})")]
    WorkAlreadyExists {
        /// 已存在的 WorkId。
        work_id: i64,
        /// 已有记录的状态字符串。
        status: String,
        /// 已有记录的 RunId。
        run_id: String,
    },
    /// 恢复锚点已经有后继 Work（并发 `resume_work` 的输家）。
    #[error("work {resumed_from} has already been resumed as work {existing_work_id}")]
    WorkAlreadyResumed {
        /// 被恢复的原 WorkId。
        resumed_from: i64,
        /// 已占用锚点的后继 WorkId。
        existing_work_id: i64,
    },
    /// agent loop 构造/运行、模型解析等门面错误。
    #[error("subagent run failed: {message}")]
    Agent {
        /// 原因。
        message: String,
    },
}

/// 子代理运行时。
pub struct SubagentRuntime {
    definition: AgentDefinition,
    parent: SubagentParentContext,
    tool_pool: ToolPool,
    permissions: SubagentPermissionContext,
    transcript: Mutex<SubagentTranscript>,
    terminal: TerminalCell,
    usage: Mutex<UsageAccumulator>,
    receipts: Mutex<ReceiptAccumulator>,
    /// 供 acceptance 判定的运行时执行记录（只保留 shell 类工具）。
    executions: Mutex<Vec<ExecutionRecord>>,
    next_execution_seq: AtomicU64,
    budget: TokenBudget,
    turns: AtomicU64,
    options: SubagentRuntimeOptions,
    isolation: Option<IsolatedWorkspace>,
    /// 终态决定保留隔离工作区（有改动）时置位：Drop 不再清理，ownership 记录也已
    /// 标记 `retained`，启动 sweep 不会回收。
    retain_isolation: std::sync::atomic::AtomicBool,
    /// `resume` 接管了已有的隔离工作区：它不属于本次运行，Drop 不得清理。
    adopted_isolation: bool,
    prompt_cache_sharing: bool,
}

/// 低层运行时 API 同样拒绝声明式 stages：prepare+run/resume+run 没有 Work 身份，
/// 静默只跑一次普通 loop 会让宿主误以为整条流水线执行过。
fn reject_staged_definition(definition: &AgentDefinition) -> Result<(), SubagentError> {
    if definition.stages.is_some() {
        return Err(SubagentError::Agent {
            message: format!(
                "agent `{}` declares stages; run it with `spawn_subagent_with_work`",
                definition.name
            ),
        });
    }
    Ok(())
}

impl SubagentRuntime {
    /// 准备一个全新子代理。
    pub fn prepare(
        definition: AgentDefinition,
        mut parent: SubagentParentContext,
        options: SubagentRuntimeOptions,
    ) -> Result<Self, SubagentError> {
        reject_staged_definition(&definition)?;
        // 先建立隔离工作区，再创建子会话：session header 必须记录子代理真实 cwd，
        // 否则 resume 会把 cwd 恢复成父目录。创建 transcript 失败时清理刚建好的
        // worktree/clone，避免留下无人认领的资源。
        let isolation_mode = options.isolation.unwrap_or(definition.isolation);
        let isolation = if isolation_mode == IsolationMode::Shared {
            None
        } else {
            let workspace =
                create_workspace(isolation_mode, &parent.cwd, &options.isolation_options)?;
            parent.cwd = workspace.root().to_path_buf();
            Some(workspace)
        };
        let child_session_id = options
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let sidechain_id = options
            .sidechain_id
            .clone()
            .unwrap_or_else(|| format!("{child_session_id}-sidechain"));
        let link = SubagentLink::new(
            parent.parent_session_id.clone(),
            child_session_id,
            sidechain_id,
            definition.name.clone(),
            options.depth,
        );
        let cwd = parent.cwd.to_string_lossy().to_string();
        let transcript = match &options.session_dir {
            Some(dir) => SubagentTranscript::create_persistent(&cwd, &dir.to_string_lossy(), link),
            None => SubagentTranscript::create_in_memory(&cwd, link),
        };
        let transcript = match transcript {
            Ok(transcript) => transcript,
            Err(error) => {
                if let Some(workspace) = &isolation {
                    let _ = workspace.cleanup();
                }
                return Err(error.into());
            }
        };
        Self::build(definition, parent, options, transcript, isolation, false)
    }

    /// 打开已有子会话（按链接恢复）。工具与权限按当前父上下文重建。
    pub fn resume(
        definition: AgentDefinition,
        mut parent: SubagentParentContext,
        options: SubagentRuntimeOptions,
        session_path: impl AsRef<Path>,
    ) -> Result<Self, SubagentError> {
        reject_staged_definition(&definition)?;
        let path = session_path.as_ref().to_string_lossy().to_string();
        // 先不带 override 打开：`SessionManager::open` 在给了 override 时会跳过会话头，
        // 直接采用 override。传父 cwd 会让下面永远读不到上次记录的隔离工作区。
        let mut transcript = SubagentTranscript::resume(&path, None)?;
        let link = transcript.link();
        if link.agent != definition.name {
            return Err(SubagentError::ResumeMismatch {
                message: format!(
                    "session `{path}` belongs to agent `{}`, not `{}`",
                    link.agent, definition.name
                ),
            });
        }
        if link.parent_session_id != parent.parent_session_id {
            return Err(SubagentError::ResumeMismatch {
                message: format!(
                    "session `{path}` belongs to parent `{}`, not `{}`",
                    link.parent_session_id, parent.parent_session_id
                ),
            });
        }
        // 恢复时复用子会话记录的工作目录（可能是上次创建的隔离工作区），不再新建
        // 一份隔离资源；隔离工作区的生命周期由 ownership 记录与显式 cleanup 管理。
        //
        // 工作区可能已经清理（无改动的隔离运行会在终态后删除）：把 parent.cwd 设成
        // 不存在的目录会让后续工具全部失败，并且权限检查与执行目录分叉。记录里的
        // 路径不存在时沿用父上下文 cwd。
        let recorded_cwd = transcript
            .manager()
            .get_header_json()
            .get("cwd")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        let mut resume_cwd_accepted = false;
        let mut isolation = None;
        if !recorded_cwd.is_empty() && Path::new(&recorded_cwd).is_dir() {
            let recorded = PathBuf::from(&recorded_cwd);
            // 子会话记录的是上次的隔离工作区（与父 cwd 不同）：权限根必须跟着它走，
            // 否则相对写入会按父 checkout 的根解析，隔离保证被反转。
            //
            // 会话头可以被编辑：只有带有效 ownership 记录的工作区才允许成为新的
            // cwd/唯一权限根；否则一个指向 `/home/.../.ssh` 的 cwd 会让恢复出来的
            // 子代理在父工作区之外写入。
            if recorded != parent.cwd {
                if let Some(workspace) = adopt_workspace(&recorded) {
                    // 认领记录（刷新 pid）失败必须让恢复失败：否则旧 pid 会被其它
                    // 进程的启动 sweep 当成死进程，删掉仍在使用的隔离工作区。
                    claim_workspace_ownership(&recorded)?;
                    parent.cwd = recorded;
                    isolation = Some(workspace);
                    resume_cwd_accepted = true;
                } else if workspace_has_ownership_record(&recorded) {
                    // 路径挂着 ownership 记录却无法接管（另一个存活进程持有，或记录
                    // 损坏）：绝不能退回“父 cwd 内共享子目录”的宽松路径，否则两个
                    // 运行实例会同时编辑同一个 worktree，且一方的清理会删掉另一方。
                    return Err(SubagentError::ResumeMismatch {
                        message: format!(
                            "session `{path}` records isolated workspace `{}`, but its ownership record cannot be adopted (owned by another live process or invalid)",
                            recorded.display()
                        ),
                    });
                } else if Self::resume_cwd_in_scope_path(&recorded, &parent) {
                    // 共享会话恢复：记录目录仍在父 cwd 或既有权限根内，恢复 cwd 是
                    // 安全的；权限根保持父策略（不映射成这个子目录）。
                    parent.cwd = recorded;
                    resume_cwd_accepted = true;
                }
                // 其它情况沿用父 cwd，并让下面的 transcript 重新以父 cwd 打开。
            } else {
                resume_cwd_accepted = true;
            }
            if !resume_cwd_accepted {
                let cwd = parent.cwd.to_string_lossy().to_string();
                transcript = SubagentTranscript::resume(&path, Some(cwd.as_str()))?;
            }
        } else {
            // 会话头没有记录 cwd，或隔离工作区已经被清理（无改动的运行会在终态后
            // 删除）：重新以父 cwd 打开，保证 transcript 与后续工具执行目录一致。
            let cwd = parent.cwd.to_string_lossy().to_string();
            transcript = SubagentTranscript::resume(&path, Some(cwd.as_str()))?;
        }
        // 已接管的隔离工作区在恢复时必须挂回句柄：没有句柄时 runner 无法判定
        // “有改动”并 retain，恢复运行产生的编辑会在进程退出后被启动 sweep 当成
        // 遗留资源删除。
        let adopted_isolation = isolation.is_some();
        let map_workspace_roots = adopted_isolation;
        let options = SubagentRuntimeOptions {
            isolation: Some(IsolationMode::Shared),
            ..options
        };
        let mut runtime = Self::build(
            definition,
            parent,
            options,
            transcript,
            isolation,
            map_workspace_roots,
        )?;
        // 接管的工作区不由本次运行创建：终态收尾由生产入口决定，路径上任何提前
        // 返回（prepare 后失败）都不能让 Drop 删掉上次运行留下的改动。
        runtime.adopted_isolation = adopted_isolation;
        Ok(runtime)
    }

    /// 记录 cwd 是否位于父 cwd 或父权限根之内（无 ownership 记录时的安全恢复）。
    fn resume_cwd_in_scope_path(recorded: &Path, parent: &SubagentParentContext) -> bool {
        let recorded = std::fs::canonicalize(recorded).unwrap_or_else(|_| recorded.to_path_buf());
        let mut roots: Vec<PathBuf> = vec![parent.cwd.clone()];
        if let Some(policy) = &parent.permission_policy {
            roots.extend(policy.workspace_roots.iter().cloned());
        }
        roots.iter().any(|root| {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            recorded == root || recorded.starts_with(&root)
        })
    }

    fn build(
        definition: AgentDefinition,
        parent: SubagentParentContext,
        options: SubagentRuntimeOptions,
        transcript: SubagentTranscript,
        isolation: Option<IsolatedWorkspace>,
        map_workspace_roots: bool,
    ) -> Result<Self, SubagentError> {
        let requested = definition.tools.as_ref();
        let effective = ToolPermissionSet::resolve(
            &parent.allowed_tools,
            parent.tools.canonical_ids(),
            requested,
        );
        let denied = definition.disallowed_tools.clone();
        let tool_pool = parent.tools.filter(&effective, &denied);
        // 隔离工作区的 cwd 已在 prepare 中写入 parent；隔离时（新建或恢复上次的
        // 工作区）把权限根映射到子代理工作区。真正的 Shared 必须保留父策略的
        // addDirs/workspaceRoots，否则相对写入会按父 checkout 的根解析。
        let mut policy = parent.permission_policy.clone();
        if (isolation.is_some() || map_workspace_roots)
            && let Some(policy) = &mut policy
        {
            policy.workspace_roots = vec![parent.cwd.clone()];
        }
        let permissions = SubagentPermissionContext::new(
            policy.as_ref(),
            parent.permission_mode,
            definition.permission_mode,
            parent.cwd.clone(),
            effective,
            denied,
            options.approval,
        )
        .with_mcp_annotations(parent.mcp_annotations.clone());
        let prompt_cache_sharing =
            options.fork && options.share_prompt_cache && parent.provider_capabilities.prompt_cache;
        let budget = TokenBudget::new(options.token_budget);
        Ok(Self {
            definition,
            parent,
            tool_pool,
            permissions,
            transcript: Mutex::new(transcript),
            terminal: TerminalCell::new(),
            usage: Mutex::new(UsageAccumulator::new()),
            receipts: Mutex::new(ReceiptAccumulator::new()),
            executions: Mutex::new(Vec::new()),
            next_execution_seq: AtomicU64::new(1),
            budget,
            turns: AtomicU64::new(0),
            options,
            isolation,
            retain_isolation: std::sync::atomic::AtomicBool::new(false),
            adopted_isolation: false,
            prompt_cache_sharing,
        })
    }

    /// 定义。
    pub fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    /// 父上下文（cwd 可能是隔离工作区）。
    pub fn parent(&self) -> &SubagentParentContext {
        &self.parent
    }

    /// 有效工具表。
    pub fn tool_pool(&self) -> &ToolPool {
        &self.tool_pool
    }

    /// 权限上下文。
    pub fn permission_context(&self) -> &SubagentPermissionContext {
        &self.permissions
    }

    /// 模型可见 schema。
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.tool_pool.schemas()
    }

    /// 有效工具名。
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_pool.names()
    }

    /// 是否 headless。
    pub fn is_headless(&self) -> bool {
        self.options.approval.is_headless()
    }

    /// system prompt：定义 prompt + 预加载技能声明。
    pub fn system_prompt(&self) -> String {
        let mut prompt = self.definition.prompt.clone();
        if !self.definition.skills.is_empty() {
            prompt.push_str("\n\nPreloaded skills:\n");
            for skill in &self.definition.skills {
                prompt.push_str("- ");
                prompt.push_str(skill);
                prompt.push('\n');
            }
        }
        prompt
    }

    /// 是否应省略父会话的 CLAUDE.md 上下文。
    pub fn omit_claude_md(&self) -> bool {
        self.definition.omit_claude_md
    }

    /// prompt cache 共享是否真正启用（fork + 显式请求 + provider 支持）。
    pub fn prompt_cache_sharing_enabled(&self) -> bool {
        self.prompt_cache_sharing
    }

    /// token 预算。
    pub fn budget(&self) -> &TokenBudget {
        &self.budget
    }

    /// `maxTurns` 上限。
    pub fn max_turns(&self) -> Option<u32> {
        self.definition.max_turns
    }

    /// 已开始的 turn 数。
    pub fn turns_used(&self) -> u64 {
        self.turns.load(Ordering::SeqCst)
    }

    /// 在每轮开始时调用；超过 `maxTurns` 时写入 `failed + turn_capped` 并返回 `true`。
    pub fn record_turn(&self) -> bool {
        let turns = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        let exceeded = self
            .definition
            .max_turns
            .is_some_and(|max_turns| turns > u64::from(max_turns));
        if exceeded {
            let state = SubagentTerminalState::with_terminal(
                SubagentTerminal::Failed,
                Some(format!(
                    "maxTurns limit ({}) exceeded",
                    self.definition.max_turns.unwrap_or_default()
                )),
            )
            .with_stop_reason(super::result::SubagentStopReason::TurnCapped)
            .with_usage(self.usage_snapshot());
            return self.terminal.try_set_terminal(state);
        }
        false
    }

    /// yield 契约。
    pub fn yield_contract(&self) -> Option<&YieldContract> {
        self.options.yield_contract.as_ref()
    }

    /// 按 yield 契约构造可注册的 `yield` 工具；没有契约时返回 `None`。
    pub fn build_yield_tool(&self) -> Option<kiana_agent_core::types::AgentTool> {
        self.options
            .yield_contract
            .clone()
            .map(super::yield_tool::create_yield_tool)
    }

    /// 子会话链接。
    pub fn transcript_link(&self) -> SubagentLink {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .link()
            .clone()
    }

    /// 会话文件（内存会话为 `None`）。
    pub fn session_file(&self) -> Option<String> {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .session_file()
    }

    /// 当前 transcript 消息。
    pub fn messages(&self) -> Vec<kiana_agent_core::types::AgentMessage> {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages()
    }

    /// 当前消息数。
    pub fn message_count(&self) -> usize {
        self.messages().len()
    }

    /// 追加消息（无条件）。
    pub fn append_message(
        &self,
        message: kiana_agent_core::types::AgentMessage,
    ) -> Result<usize, SubagentError> {
        let mut transcript = self
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(transcript.append_message(message)?)
    }

    /// 追加消息；与 transcript 最后一条相同（忽略时间戳）时跳过。
    pub fn append_message_if_new(
        &self,
        message: kiana_agent_core::types::AgentMessage,
    ) -> Result<usize, SubagentError> {
        let mut transcript = self
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(transcript.append_message_if_new(message)?)
    }

    /// 首次进入时写入初始 prompt；transcript 非空时不做任何事。
    pub fn seed_initial_prompt(&self) -> Result<bool, SubagentError> {
        let Some(prompt) = self.definition.initial_prompt.clone() else {
            return Ok(false);
        };
        let mut transcript = self
            .transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(transcript.seed_initial_prompt(prompt)?)
    }

    /// 累积一个流式 chunk；返回是否因预算耗尽而写入了终态。
    pub fn record_chunk(&self, chunk: &SubagentStreamChunk) -> bool {
        match chunk {
            SubagentStreamChunk::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                cost,
            } => {
                self.usage
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .add(
                        *input_tokens,
                        *output_tokens,
                        *cache_read_tokens,
                        *cache_write_tokens,
                        *cost,
                    );
            }
            SubagentStreamChunk::UsageObject(usage) => {
                self.usage
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .add_usage(usage);
            }
            _ => {}
        }
        self.receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(chunk);
        self.sync_budget()
    }

    /// 直接累加 usage（流适配层没有 chunk 时使用）；返回是否因预算耗尽写入终态。
    pub fn record_usage(&self, usage: &Usage) -> bool {
        self.usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .add_usage(usage);
        self.sync_budget()
    }

    /// 把累计 usage 同步到预算；超限时写 `failed + token_capped`。
    fn sync_budget(&self) -> bool {
        let total = self
            .usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .billable_tokens();
        let current = self.budget.used();
        if total > current {
            self.budget.consume(total - current);
        }
        if self.budget.is_exhausted() {
            let state = SubagentTerminalState::with_terminal(
                SubagentTerminal::Failed,
                Some("token budget exhausted".to_string()),
            )
            .with_stop_reason(super::result::SubagentStopReason::TokenCapped)
            .with_usage(self.usage_snapshot());
            return self.terminal.try_set_terminal(state);
        }
        false
    }

    /// 当前 usage 快照。
    pub fn usage_snapshot(&self) -> SubagentUsage {
        self.usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot()
    }

    /// 已累积的 receipts。
    pub fn receipts(&self) -> Vec<super::result::ToolReceipt> {
        self.receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .receipts()
            .to_vec()
    }

    /// 收尾仍在执行的工具调用（取消/超时/截断路径），返回完整 receipts。
    ///
    /// 被中断的调用不会走到 [`Self::dispatch_tool_with`] 的完成分支；不在这里补
    /// 一条凭证，它会从 Work 证据里消失，尽管它可能已经产生了副作用。
    pub fn finalize_pending_receipts(
        &self,
        status: ReceiptStatus,
    ) -> Vec<super::result::ToolReceipt> {
        let mut receipts = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receipts.finalize_pending(status);
        receipts.receipts().to_vec()
    }

    /// 供 acceptance 判定的运行时执行记录（按完成顺序）。
    ///
    /// 只保留 shell 类工具：`tests_passed` 是唯一需要完整输出重建的 criterion，
    /// 文件类 criterion 直接读工作区。输出文本与主会话
    /// `record_tool_effects` 的口径一致。
    /// 终态要判定的 acceptance criteria（见 [`SubagentRuntimeOptions::acceptance_criteria`]）。
    pub fn acceptance_criteria(&self) -> &[String] {
        &self.options.acceptance_criteria
    }

    pub fn execution_records(&self) -> Vec<ExecutionRecord> {
        self.executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record_execution(
        &self,
        builtin: bool,
        tool: &str,
        args: &JsonValue,
        output: String,
        status: ReceiptStatus,
    ) {
        // 只看名字不够：扩展/SDK 工具可以遮蔽内建 `bash`，它的输出不是 shell 证据
        // （与 `dispatch_tool_with` 的权限判定使用同一个 `builtin`）。
        if !builtin || !is_shell_tool(tool) {
            return;
        }
        let seq = self.next_execution_seq.fetch_add(1, Ordering::SeqCst);
        let record = ExecutionRecord::record(tool, args.clone(), output, status, seq);
        self.executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(record);
    }

    /// 尝试写入终态（超时/完成竞争只有一个赢家）。
    pub fn try_set_terminal(&self, state: SubagentTerminalState) -> bool {
        let mut state = state;
        if state.usage == SubagentUsage::default() {
            state.usage = self.usage_snapshot();
        }
        self.terminal.try_set_terminal(state)
    }

    /// 当前终态。
    pub fn terminal(&self) -> Option<SubagentTerminalState> {
        self.terminal.get()
    }

    /// 是否结束。
    pub fn is_terminal(&self) -> bool {
        self.terminal.is_terminal()
    }

    /// 完成。
    pub fn complete(&self, result: impl Into<String>) -> bool {
        self.try_set_terminal(SubagentTerminalState::completed(result))
    }

    /// 失败。
    pub fn fail(&self, error: impl Into<String>) -> bool {
        self.try_set_terminal(SubagentTerminalState::failed(error))
    }

    /// 取消。
    pub fn cancel(&self) -> bool {
        self.try_set_terminal(SubagentTerminalState::with_terminal(
            SubagentTerminal::Cancelled,
            Some("cancelled".to_string()),
        ))
    }

    /// 超时。
    pub fn timeout(&self) -> bool {
        self.try_set_terminal(SubagentTerminalState::with_terminal(
            SubagentTerminal::TimedOut,
            Some("timed out".to_string()),
        ))
    }

    /// 隔离工作区（Shared 时为 `None`）。
    pub fn isolated_workspace(&self) -> Option<&IsolatedWorkspace> {
        self.isolation.as_ref()
    }

    /// 保留隔离工作区：Drop 不再清理，ownership 记录标记为 `retained`（启动 sweep
    /// 也不会回收）。有改动的终态由生产入口调用。
    ///
    /// 记录写入失败时仍然不清理（返回错误由调用方决定如何报告）；工作区不会因为
    /// 磁盘错误被静默删除。
    pub fn retain_isolation(&self) -> Result<(), IsolationError> {
        // 先置位 Drop 守卫：即便 ownership 记录写失败，本次进程退出前也不会丢掉改动。
        // 返回值仍要传播——调用方据此告诉宿主“下一个进程的 sweep 可能回收”。
        self.retain_isolation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        match self.isolation.as_ref() {
            Some(workspace) => workspace.retain(),
            None => Ok(()),
        }
    }

    /// 是否已保留隔离工作区。
    pub fn isolation_retained(&self) -> bool {
        self.retain_isolation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 清理隔离工作区（不会改变终态）；重复调用是 no-op。
    pub fn cleanup_isolation(&mut self) -> Result<(), IsolationError> {
        match self.isolation.take() {
            Some(workspace) => workspace.cleanup(),
            None => Ok(()),
        }
    }
    /// dispatch 一次工具调用。
    ///
    /// 顺序：pool 注册表 → allow/deny 二次检查 → 权限引擎 → 工具执行。
    /// headless 下 `Ask` 在这里变成 `PermissionDenied`，不会触及工具实现。
    pub async fn dispatch_tool(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        args: JsonValue,
    ) -> Result<AgentToolResult<JsonValue>, SubagentError> {
        self.dispatch_tool_with(tool_call_id, tool_name, args, None, None)
            .await
    }

    /// [`Self::dispatch_tool`] 的完整版本：把 agent loop 的取消 token 与更新回调透传给
    /// 底层工具。子代理的 `bash` 等长任务只有收到同一个信号才能被取消；丢掉它会让
    /// `cancel` 一直等到命令自己结束。
    pub async fn dispatch_tool_with(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        args: JsonValue,
        signal: Option<&CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult<JsonValue>, SubagentError> {
        let tool = self
            .tool_pool
            .get_checked(
                tool_name,
                self.permissions.tools(),
                &self.definition.disallowed_tools,
            )
            .map_err(|source| SubagentError::ToolUnavailable {
                tool: tool_name.to_string(),
                source,
            })?
            .clone();
        let canonical = CanonicalToolId::parse(tool_name);
        // 扩展/SDK 工具可以遮蔽内建名：父会话用 `custom_tool_names` 区分，这里必须同样
        // 处理，否则同名扩展工具会按内建工具的 ActionKind（例如 read）被只读模式放行。
        let builtin = canonical.is_builtin()
            && !self
                .parent
                .custom_tools
                .iter()
                .any(|name| name == tool_name);
        // `bash` 实际执行的是 `prefix + "\n" + command`（由工具自己在执行时拼接）；
        // 权限层必须看到同一份文本，否则前缀里的命令不会被检查，审批 key 也与父会话
        // 不一致。**只**给权限请求用带前缀的副本——把拼接过前缀的参数再交给工具执行
        // 会让前缀出现两次。
        let permission_args = if builtin && tool_name == "bash" {
            match self.parent.shell_command_prefix.as_deref() {
                Some(prefix) if !prefix.is_empty() => {
                    crate::agent_session::with_shell_command_prefix(&args, prefix)
                }
                _ => args.clone(),
            }
        } else {
            args.clone()
        };
        let request = PermissionRequest::from_tool_call_with(
            tool_name,
            &permission_args,
            self.parent.cwd.clone(),
            builtin,
        );
        match self.permissions.evaluate(&request) {
            SubagentPermissionDecision::Allow { .. } => {}
            SubagentPermissionDecision::Deny { reason } => {
                return Err(SubagentError::PermissionDenied {
                    tool: tool_name.to_string(),
                    reason,
                });
            }
            SubagentPermissionDecision::NeedsApproval { prompt, reason } => {
                return Err(SubagentError::NeedsApproval {
                    tool: tool_name.to_string(),
                    prompt,
                    reason,
                });
            }
        }

        self.receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(&SubagentStreamChunk::ToolCallStarted {
                tool_call_id: tool_call_id.to_string(),
                tool: tool_name.to_string(),
                arguments: args.clone(),
            });

        let execute = tool.execute.clone();
        // 工具上下文由父会话的 ExtensionRunner 创建（cwd 固定为父目录）；子代理必须让
        // 文件/命令工具按自己的 cwd 解析相对路径，否则权限检查（parent.cwd）与实际
        // 写入目录会分叉。任务局部覆盖只影响本次工具执行。
        //
        // 覆盖条件不能只看 `isolation`：`resume` 会把 parent.cwd 换成会话里记录的
        // 工作目录（可能是上次的隔离工作区），而它与扩展运行时的 cwd 不同。
        let recorded_args = args.clone();
        let execution = execute.execute(tool_call_id, args, signal, on_update);
        let result = kiana_extensions::with_cwd_override(self.parent.cwd.clone(), execution).await;
        match result {
            Ok(result) => {
                let status = receipt_status(result.is_error == Some(true));
                self.record_execution(
                    builtin,
                    tool_name,
                    &recorded_args,
                    tool_result_text(&result.content),
                    status,
                );
                let output = serde_json::to_value(&result).unwrap_or(JsonValue::Null);
                self.receipts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .observe(&SubagentStreamChunk::ToolCallFinished {
                        tool_call_id: tool_call_id.to_string(),
                        output,
                        status,
                    });
                Ok(result)
            }
            Err(error) => {
                self.record_execution(
                    builtin,
                    tool_name,
                    &recorded_args,
                    error.message.clone(),
                    ReceiptStatus::Failed,
                );
                self.receipts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .observe(&SubagentStreamChunk::ToolCallFinished {
                        tool_call_id: tool_call_id.to_string(),
                        output: JsonValue::String(error.message.clone()),
                        status: ReceiptStatus::Failed,
                    });
                Err(SubagentError::ToolFailed {
                    tool: tool_name.to_string(),
                    message: error.message,
                })
            }
        }
    }
}

/// 运行时被丢弃时回收**没有改动**的隔离工作区。
///
/// 生产入口在终态后调用 [`SubagentRuntime::retain_isolation`] 保留有改动的工作区
/// （调用方需要审阅/合并）；没有这个 Drop，长会话里每个无改动的隔离子代理都会在
/// 进程生命周期内持续占用一份仓库 checkout。
///
/// 需要自行处理 delta 的调用方走 `prepare_subagent` + 显式
/// [`SubagentRuntime::cleanup_isolation`] 即可，不受这里影响。
impl Drop for SubagentRuntime {
    fn drop(&mut self) {
        if self
            .retain_isolation
            .load(std::sync::atomic::Ordering::SeqCst)
            || self.adopted_isolation
        {
            return;
        }
        if let Some(workspace) = self.isolation.take() {
            let _ = workspace.cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kiana_agent_core::types::{
        AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture,
    };
    use kiana_ai::types::{TextContent, TextKind, UserContentBlock};
    use serde_json::json;

    use super::*;
    use crate::agent_definition::AgentDefinitionSource;
    use crate::agent_definition::parse_markdown_definition;
    use crate::subagent::canonical_tool::ToolPattern;
    use crate::subagent::result::SubagentStopReason;

    struct EchoExecute;

    impl AgentToolExecute for EchoExecute {
        fn execute<'a>(
            &'a self,
            tool_call_id: &'a str,
            params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: format!("{tool_call_id}: {params}"),
                        text_signature: None,
                    })],
                    details: params,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    fn tool(name: &str) -> AgentTool {
        AgentTool {
            name: name.to_string(),
            label: name.to_string(),
            description: format!("{name} tool"),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(EchoExecute),
        }
    }

    fn definition(tools: Option<&str>) -> AgentDefinition {
        let tools_line = tools.unwrap_or("*");
        parse_markdown_definition(
            &format!(
                "---\ndescription: test agent\ntools: {tools_line}\n---\nYou are a test agent.\n"
            ),
            "test",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("definition")
    }

    fn parent(tools: Vec<AgentTool>) -> SubagentParentContext {
        SubagentParentContext::new(
            "parent-session",
            "/tmp/kiana-runtime-work",
            ToolPool::new(tools),
        )
    }

    #[test]
    fn effective_pool_is_parent_intersect_requested() {
        let parent = parent(vec![tool("read"), tool("bash")]);
        let parent =
            parent.with_allowed_tools(ToolPermissionSet::from_ids([CanonicalToolId::parse(
                "read",
            )]));
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        assert_eq!(runtime.tool_names(), vec!["read"]);
        assert_eq!(runtime.tool_schemas().len(), 1);
        assert!(runtime.tool_pool().get("bash").is_none());
    }

    #[tokio::test]
    async fn direct_dispatch_of_denied_tool_fails() {
        let parent = parent(vec![tool("read"), tool("bash")]);
        let mut definition = definition(Some("*"));
        definition.disallowed_tools = vec![ToolPattern::new("bash")];
        let runtime = SubagentRuntime::prepare(
            definition,
            parent,
            SubagentRuntimeOptions {
                approval: SubagentApprovalMode::Interactive,
                ..Default::default()
            },
        )
        .expect("prepare");
        let error = runtime
            .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, SubagentError::ToolUnavailable { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn headless_ask_becomes_deny() {
        let parent = parent(vec![tool("read"), tool("bash")])
            .with_permission_policy(PermissionPolicy::new(PermissionMode::Default, "/tmp"));
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        assert!(runtime.is_headless());
        let error = runtime
            .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
            .await
            .unwrap_err();
        match error {
            SubagentError::PermissionDenied { reason, .. } => assert!(reason.contains("headless")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_command_prefix_is_checked_but_applied_once() {
        // `bash` 执行时会自己拼 `prefix + "\n" + command`。权限层必须看到拼好的文本
        // （否则前缀里的命令绕过了规则，审批 key 也和父会话不一致），但执行器只能
        // 收到原始参数，否则前缀会被执行两遍。
        let prefixed = "source ~/.env\necho hi";
        let denied_parent = parent(vec![tool("bash")])
            .with_permission_policy(
                PermissionPolicy::new(PermissionMode::BypassPermissions, "/tmp").with_rule(
                    kiana_permissions::PermissionRule::exact_command(
                        prefixed,
                        kiana_permissions::RuleDecision::Deny,
                    ),
                ),
            )
            .with_shell_command_prefix("source ~/.env");
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            denied_parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        let error = runtime
            .dispatch_tool("call-1", "bash", json!({"command": "echo hi"}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, SubagentError::PermissionDenied { .. }),
            "权限层必须按带前缀的命令匹配：{error:?}"
        );

        // 同一前缀、没有拒绝规则：工具确实执行，且收到的仍是原始命令。
        let allowed_parent = parent(vec![tool("bash")])
            .with_permission_policy(PermissionPolicy::new(
                PermissionMode::BypassPermissions,
                "/tmp",
            ))
            .with_shell_command_prefix("source ~/.env");
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            allowed_parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        let result = runtime
            .dispatch_tool("call-2", "bash", json!({"command": "echo hi"}))
            .await
            .expect("dispatch");
        assert_eq!(result.details, json!({"command": "echo hi"}));
    }

    #[tokio::test]
    async fn shared_mode_preserves_parent_workspace_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd = temp.path().join("cwd");
        let extra = temp.path().join("extra");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&extra).expect("extra");
        let policy = PermissionPolicy {
            mode: PermissionMode::AcceptEdits,
            workspace_roots: vec![cwd.clone(), extra.clone()],
            rules: Vec::new(),
        };
        let parent =
            SubagentParentContext::new("parent-session", &cwd, ToolPool::new(vec![tool("write")]))
                .with_permission_policy(policy);
        let runtime = SubagentRuntime::prepare(
            definition(Some("write")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        let path = extra.join("allowed.txt");
        runtime
            .dispatch_tool("call-1", "write", json!({"path": path}))
            .await
            .expect("write outside cwd but inside addDirs is allowed");
    }

    #[tokio::test]
    async fn allowed_tool_executes_and_records_receipt() {
        let parent = parent(vec![tool("read")]);
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        let result = runtime
            .dispatch_tool("call-1", "read", json!({"path": "a.txt"}))
            .await
            .expect("dispatch");
        assert_eq!(result.details, json!({"path": "a.txt"}));
        let receipts = runtime.receipts();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].tool, "read");
        assert_eq!(receipts[0].status, ReceiptStatus::Succeeded);
    }

    #[test]
    fn finalize_pending_receipts_records_an_in_flight_call_once() {
        let parent = parent(vec![tool("read")]);
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        // 模拟取消：ToolCallStarted 已记录，但完成分支因为 future 被 drop 没走到。
        runtime.record_chunk(&SubagentStreamChunk::ToolCallStarted {
            tool_call_id: "call-1".to_string(),
            tool: "read".to_string(),
            arguments: json!({"path": "a.txt"}),
        });
        let receipts = runtime.finalize_pending_receipts(ReceiptStatus::Cancelled);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].tool, "read");
        assert_eq!(receipts[0].status, ReceiptStatus::Cancelled);
        // 再收尾一次不会重复记账。
        assert_eq!(
            runtime
                .finalize_pending_receipts(ReceiptStatus::Cancelled)
                .len(),
            1
        );
    }

    #[test]
    fn budget_exhaustion_sets_token_capped_terminal_once() {
        let parent = parent(vec![tool("read")]);
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                token_budget: Some(10),
                ..Default::default()
            },
        )
        .expect("prepare");
        let first = runtime.record_chunk(&SubagentStreamChunk::Usage {
            input_tokens: 20,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost: 0.0,
        });
        assert!(first, "budget writer wins");
        let state = runtime.terminal().expect("terminal");
        assert_eq!(state.terminal, SubagentTerminal::Failed);
        assert_eq!(state.stop_reason, Some(SubagentStopReason::TokenCapped));
        let late = runtime.try_set_terminal(SubagentTerminalState::completed("late"));
        assert!(!late);
        assert_eq!(
            runtime.terminal().expect("terminal").terminal,
            SubagentTerminal::Failed
        );
    }

    #[test]
    fn max_turns_sets_turn_capped_terminal_once() {
        let definition = parse_markdown_definition(
            "---\ndescription: test agent\ntools: read\nmaxTurns: 1\n---\nYou are a test agent.\n",
            "test",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("definition");
        let runtime = SubagentRuntime::prepare(
            definition,
            parent(vec![tool("read")]),
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        assert_eq!(runtime.max_turns(), Some(1));
        assert!(!runtime.record_turn(), "first turn is allowed");
        assert!(runtime.record_turn(), "second turn exceeds maxTurns");
        let state = runtime.terminal().expect("terminal");
        assert_eq!(state.terminal, SubagentTerminal::Failed);
        assert_eq!(state.stop_reason, Some(SubagentStopReason::TurnCapped));
        assert!(!runtime.record_turn(), "terminal stays sticky");
    }

    #[test]
    fn yield_tool_is_built_only_when_contract_is_present() {
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent(vec![tool("read")]),
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        assert!(runtime.build_yield_tool().is_none());

        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent(vec![tool("read")]),
            SubagentRuntimeOptions {
                yield_contract: Some(YieldContract::new(json!({"type": "object"}))),
                ..Default::default()
            },
        )
        .expect("prepare");
        let tool = runtime.build_yield_tool().expect("yield tool");
        assert_eq!(tool.name, "yield");
    }

    #[test]
    fn prompt_cache_sharing_is_off_by_default_and_requires_capability() {
        let agent_definition = definition(Some("read"));
        let parent = parent(vec![tool("read")]);
        let runtime = SubagentRuntime::prepare(
            agent_definition.clone(),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        assert!(!runtime.prompt_cache_sharing_enabled(), "default off");

        let parent = parent_with_capability(vec![tool("read")], true);
        let runtime = SubagentRuntime::prepare(
            agent_definition.clone(),
            parent,
            SubagentRuntimeOptions {
                fork: true,
                share_prompt_cache: false,
                ..Default::default()
            },
        )
        .expect("prepare");
        assert!(
            !runtime.prompt_cache_sharing_enabled(),
            "explicit opt-in required"
        );

        let parent = parent_with_capability(vec![tool("read")], true);
        let runtime = SubagentRuntime::prepare(
            agent_definition.clone(),
            parent,
            SubagentRuntimeOptions {
                fork: true,
                share_prompt_cache: true,
                ..Default::default()
            },
        )
        .expect("prepare");
        assert!(runtime.prompt_cache_sharing_enabled());

        let parent = parent_with_capability(vec![tool("read")], false);
        let runtime = SubagentRuntime::prepare(
            agent_definition,
            parent,
            SubagentRuntimeOptions {
                fork: true,
                share_prompt_cache: true,
                ..Default::default()
            },
        )
        .expect("prepare");
        assert!(
            !runtime.prompt_cache_sharing_enabled(),
            "provider must support it"
        );
    }

    fn parent_with_capability(tools: Vec<AgentTool>, prompt_cache: bool) -> SubagentParentContext {
        parent(tools).with_provider_capabilities(ProviderCapabilities { prompt_cache })
    }

    #[test]
    fn resume_reuses_transcript_without_duplicating_messages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("sessions");
        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(dir),
                session_id: Some("child-runtime".to_string()),
                ..Default::default()
            },
        )
        .expect("prepare");
        runtime.append_message(user_text("hello")).expect("append");
        runtime
            .append_message_if_new(user_text("hello"))
            .expect("dedup");
        assert_eq!(runtime.message_count(), 1);
        let path = runtime.session_file().expect("session file");
        drop(runtime);

        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let resumed = SubagentRuntime::resume(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .expect("resume");
        assert_eq!(resumed.message_count(), 1);
        resumed
            .append_message_if_new(user_text("hello"))
            .expect("replay");
        assert_eq!(resumed.message_count(), 1);
        resumed.append_message(user_text("new")).expect("new");
        assert_eq!(resumed.message_count(), 2);
    }

    #[test]
    fn resume_restores_the_recorded_working_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let dir = temp.path().join("sessions");
        let parent = SubagentParentContext::new(
            "parent-session",
            worktree.clone(),
            ToolPool::new(vec![tool("read")]),
        );
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(dir),
                session_id: Some("child-cwd".to_string()),
                ..Default::default()
            },
        )
        .expect("prepare");
        runtime.append_message(user_text("hello")).expect("append");
        let path = runtime.session_file().expect("session file");
        drop(runtime);

        // 父上下文从父仓库恢复；子会话头里记录的隔离工作区仍然存在时必须沿用，
        // 否则恢复出来的子代理会直接编辑父仓库。
        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let resumed = SubagentRuntime::resume(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .expect("resume");
        assert_eq!(resumed.parent().cwd, worktree);

        // 工作区被清理后回退到父 cwd，不能把工具指到不存在的目录。
        std::fs::remove_dir_all(&worktree).expect("remove worktree");
        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let resumed = SubagentRuntime::resume(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .expect("resume");
        assert_eq!(resumed.parent().cwd, temp.path());
    }

    #[test]
    fn staged_definitions_are_rejected_by_the_exported_runtime_api() {
        let mut staged = definition(Some("read"));
        staged.stages = Some(Vec::new());
        let parent = SubagentParentContext::new(
            "parent-session",
            "/tmp/workspace",
            ToolPool::new(Vec::new()),
        );
        let error = SubagentRuntime::prepare(staged.clone(), parent, Default::default())
            .err()
            .expect("prepare must reject a staged definition");
        assert!(error.to_string().contains("declares stages"), "{error}");

        let parent = SubagentParentContext::new(
            "parent-session",
            "/tmp/workspace",
            ToolPool::new(Vec::new()),
        );
        let error = SubagentRuntime::resume(
            staged,
            parent,
            Default::default(),
            "/tmp/does-not-exist.jsonl",
        )
        .err()
        .expect("resume must reject a staged definition before opening the transcript");
        assert!(error.to_string().contains("declares stages"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn resume_refuses_a_workspace_owned_by_another_live_process() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent_repo = temp.path().join("repo");
        std::fs::create_dir_all(&parent_repo).expect("repo");
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&parent_repo)
                .args(args)
                .status()
        };
        let Ok(status) = git(&["init", "-q"]) else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        git(&["config", "user.email", "test@example.com"]).expect("config");
        git(&["config", "user.name", "Test"]).expect("config");
        std::fs::write(parent_repo.join("tracked.txt"), b"base\n").expect("write");
        assert!(git(&["add", "."]).expect("add").success());
        assert!(
            git(&["commit", "-q", "-m", "init"])
                .expect("commit")
                .success()
        );
        let dir = temp.path().join("sessions");
        let parent = SubagentParentContext::new(
            "parent-session",
            &parent_repo,
            ToolPool::new(vec![tool("read")]),
        );
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(dir),
                session_id: Some("child-live-owner".to_string()),
                isolation: Some(IsolationMode::Worktree),
                isolation_options: IsolationOptions {
                    state_dir: Some(temp.path().join("state")),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("prepare");
        let record_path = runtime
            .isolated_workspace()
            .expect("isolated")
            .ownership()
            .record_path();
        runtime.append_message(user_text("hello")).expect("append");
        let path = runtime.session_file().expect("session file");
        runtime.retain_isolation().expect("retain");
        drop(runtime);

        // 模拟另一个仍存活的进程持有该工作区（pid 1 在本测试环境一定存在）。
        let mut record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).expect("record"))
                .expect("record json");
        record["pid"] = serde_json::json!(1);
        std::fs::write(
            &record_path,
            serde_json::to_string(&record).expect("encode"),
        )
        .expect("rewrite record");

        let parent = SubagentParentContext::new(
            "parent-session",
            &parent_repo,
            ToolPool::new(vec![tool("read")]),
        );
        let error = SubagentRuntime::resume(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .err()
        .expect("resume must not fall back to sharing a live-owned worktree");
        assert!(error.to_string().contains("cannot be adopted"), "{error}");
    }

    #[tokio::test]
    async fn resume_maps_permission_roots_to_the_recorded_isolated_workspace() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent_repo = temp.path().join("repo");
        std::fs::create_dir_all(&parent_repo).expect("repo");
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&parent_repo)
                .args(args)
                .status()
        };
        let Ok(status) = git(&["init", "-q"]) else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        git(&["config", "user.email", "test@example.com"]).expect("config");
        git(&["config", "user.name", "Test"]).expect("config");
        std::fs::write(parent_repo.join("tracked.txt"), b"base\n").expect("write");
        assert!(git(&["add", "."]).expect("add").success());
        assert!(
            git(&["commit", "-q", "-m", "init"])
                .expect("commit")
                .success()
        );
        let dir = temp.path().join("sessions");
        let policy = PermissionPolicy {
            mode: PermissionMode::AcceptEdits,
            workspace_roots: vec![parent_repo.clone()],
            rules: Vec::new(),
        };
        let parent = SubagentParentContext::new(
            "parent-session",
            &parent_repo,
            ToolPool::new(vec![tool("write")]),
        )
        .with_permission_policy(policy.clone());
        let runtime = SubagentRuntime::prepare(
            definition(Some("write")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(dir),
                session_id: Some("child-isolated-cwd".to_string()),
                isolation: Some(IsolationMode::Worktree),
                isolation_options: IsolationOptions {
                    state_dir: Some(temp.path().join("state")),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("prepare");
        let worktree = runtime
            .isolated_workspace()
            .expect("isolated")
            .root()
            .to_path_buf();
        runtime.append_message(user_text("hello")).expect("append");
        let path = runtime.session_file().expect("session file");
        runtime.retain_isolation().expect("retain");
        drop(runtime);

        // 从父仓库恢复：记录里的隔离工作区仍存在，权限根必须跟着它走。
        let parent = SubagentParentContext::new(
            "parent-session",
            &parent_repo,
            ToolPool::new(vec![tool("write")]),
        )
        .with_permission_policy(policy);
        let mut resumed = SubagentRuntime::resume(
            definition(Some("write")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .expect("resume");
        assert_eq!(resumed.parent().cwd, worktree);
        resumed
            .dispatch_tool("call-1", "write", json!({"path": worktree.join("out.txt")}))
            .await
            .expect("write inside the resumed workspace");
        // 父仓库不在映射后的权限根里：隔离保证不能被恢复反转。
        let error = resumed
            .dispatch_tool(
                "call-2",
                "write",
                json!({"path": parent_repo.join("leak.txt")}),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, SubagentError::PermissionDenied { .. }),
            "{error:?}"
        );
        resumed.cleanup_isolation().expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn retain_isolation_reports_a_record_write_failure_but_keeps_the_workspace() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(args).status();
        let Ok(status) = git(&["init", "-q"]) else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        git(&["config", "user.email", "test@example.com"]).expect("config");
        git(&["config", "user.name", "Test"]).expect("config");
        std::fs::write(repo.join("tracked.txt"), b"base\n").expect("write");
        assert!(git(&["add", "."]).expect("add").success());
        assert!(
            git(&["commit", "-q", "-m", "init"])
                .expect("commit")
                .success()
        );
        let parent =
            SubagentParentContext::new("parent-session", &repo, ToolPool::new(vec![tool("read")]));
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            parent,
            SubagentRuntimeOptions {
                isolation: Some(IsolationMode::Worktree),
                isolation_options: IsolationOptions {
                    state_dir: Some(temp.path().join("state")),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("prepare");
        let record = runtime
            .isolated_workspace()
            .expect("isolated")
            .ownership()
            .record_path();
        let record_dir = record.parent().expect("record parent").to_path_buf();
        let original = std::fs::metadata(&record_dir)
            .expect("record dir")
            .permissions();
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let result = runtime.retain_isolation();
        std::fs::set_permissions(&record_dir, original).expect("restore");
        assert!(result.is_err(), "record write must surface as an error");
        // 记录没写成功，但本次进程内仍保住工作区（Drop 不清理）。
        assert!(runtime.isolation_retained());
        // 恢复权限后显式清理，避免 tempdir 与 worktree 的 Drop 顺序告警。
        let workspace = runtime
            .isolated_workspace()
            .expect("isolated")
            .root()
            .to_path_buf();
        assert!(workspace.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resume_adopts_the_recorded_isolated_workspace() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(args).status();
        let Ok(status) = git(&["init", "-q"]) else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        git(&["config", "user.email", "test@example.com"]).expect("config");
        git(&["config", "user.name", "Test"]).expect("config");
        std::fs::write(repo.join("tracked.txt"), b"base\n").expect("write");
        assert!(git(&["add", "."]).expect("add").success());
        assert!(
            git(&["commit", "-q", "-m", "init"])
                .expect("commit")
                .success()
        );
        let state = temp.path().join("state");
        let parent =
            SubagentParentContext::new("parent-session", &repo, ToolPool::new(vec![tool("read")]));
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(temp.path().join("sessions")),
                session_id: Some("child-adopt".to_string()),
                isolation: Some(IsolationMode::Worktree),
                isolation_options: IsolationOptions {
                    state_dir: Some(state.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("prepare");
        let workspace_root = runtime
            .isolated_workspace()
            .expect("isolated")
            .root()
            .to_path_buf();
        runtime.append_message(user_text("hello")).expect("append");
        let session_file = runtime.session_file().expect("session file");
        // 标记保留（有改动的运行在终态会这样做），避免 Drop 清理工作区。
        runtime.retain_isolation().expect("retain");
        drop(runtime);

        let parent =
            SubagentParentContext::new("parent-session", &repo, ToolPool::new(vec![tool("read")]));
        let mut resumed = SubagentRuntime::resume(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions::default(),
            &session_file,
        )
        .expect("resume");
        assert_eq!(resumed.parent().cwd, workspace_root);
        let adopted = resumed
            .isolated_workspace()
            .expect("resume must adopt the recorded workspace");
        assert_eq!(adopted.root(), workspace_root);
        // 认领后记录属于当前进程，sweep 不会把它当成死进程遗留。
        let records = crate::subagent::isolation::list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].as_ref().expect("record").pid,
            Some(std::process::id())
        );
        // 接管的工作区在 Drop 时不清理：恢复前的编辑仍可能在后续重试中使用。
        resumed.cleanup_isolation().expect("cleanup");
    }

    #[tokio::test]
    async fn resume_ignores_a_recorded_cwd_outside_the_parent_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = temp.path().join("outside");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::create_dir_all(&repo).expect("repo");
        let dir = temp.path().join("sessions");
        let parent = SubagentParentContext::new(
            "parent-session",
            &outside,
            ToolPool::new(vec![tool("write")]),
        );
        let runtime = SubagentRuntime::prepare(
            definition(Some("write")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(dir),
                session_id: Some("child-untrusted-cwd".to_string()),
                ..Default::default()
            },
        )
        .expect("prepare");
        runtime.append_message(user_text("hello")).expect("append");
        let path = runtime.session_file().expect("session file");
        drop(runtime);

        // 会话头里的 cwd 没有 ownership 记录、也不在父 cwd / 权限根内：恢复必须
        // 忽略它，继续用父 cwd（否则被编辑的 session file 可以把子代理放到任意目录）。
        let policy = PermissionPolicy {
            mode: PermissionMode::AcceptEdits,
            workspace_roots: vec![repo.clone()],
            rules: Vec::new(),
        };
        let parent =
            SubagentParentContext::new("parent-session", &repo, ToolPool::new(vec![tool("write")]))
                .with_permission_policy(policy);
        let resumed = SubagentRuntime::resume(
            definition(Some("write")),
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        )
        .expect("resume");
        assert_eq!(resumed.parent().cwd, repo);
        let error = resumed
            .dispatch_tool("call-1", "write", json!({"path": outside.join("leak.txt")}))
            .await
            .unwrap_err();
        assert!(
            matches!(error, SubagentError::PermissionDenied { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn resume_rejects_a_different_agent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let runtime = SubagentRuntime::prepare(
            definition(Some("read")),
            parent,
            SubagentRuntimeOptions {
                session_dir: Some(temp.path().join("sessions")),
                session_id: Some("child-mismatch".to_string()),
                ..Default::default()
            },
        )
        .expect("prepare");
        runtime.append_message(user_text("hello")).expect("append");
        let path = runtime.session_file().expect("session file");
        drop(runtime);

        let other = parse_markdown_definition(
            "---\ndescription: other agent\ntools: read\n---\nOther.\n",
            "other",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("definition");
        let parent = SubagentParentContext::new(
            "parent-session",
            temp.path(),
            ToolPool::new(vec![tool("read")]),
        );
        let error = match SubagentRuntime::resume(
            other,
            parent,
            SubagentRuntimeOptions::default(),
            &path,
        ) {
            Ok(_) => panic!("resuming a different agent must fail"),
            Err(error) => error,
        };
        assert!(
            matches!(error, SubagentError::ResumeMismatch { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn dropping_an_isolated_runtime_cleans_the_worktree() {
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(args).status();
        let Ok(status) = git(&["init", "-q"]) else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        git(&["config", "user.email", "test@example.com"]).expect("config");
        git(&["config", "user.name", "Test"]).expect("config");
        std::fs::write(repo.join("tracked.txt"), b"base\n").expect("write");
        assert!(git(&["add", "."]).expect("add").success());
        assert!(
            git(&["commit", "-q", "-m", "init"])
                .expect("commit")
                .success()
        );

        let parent =
            SubagentParentContext::new("parent-session", &repo, ToolPool::new(vec![tool("read")]));
        let options = SubagentRuntimeOptions {
            isolation: Some(IsolationMode::Worktree),
            isolation_options: IsolationOptions {
                state_dir: Some(temp.path().join("state")),
                ..Default::default()
            },
            ..Default::default()
        };
        let runtime =
            SubagentRuntime::prepare(definition(Some("*")), parent, options).expect("prepare");
        let workspace = runtime
            .isolated_workspace()
            .expect("isolated")
            .root()
            .to_path_buf();
        let record = runtime
            .isolated_workspace()
            .expect("isolated")
            .ownership()
            .record_path();
        assert!(workspace.exists());

        drop(runtime);
        assert!(!workspace.exists(), "Drop must remove the worktree");
        assert!(!record.exists(), "Drop must remove the ownership record");
    }

    fn user_text(text: &str) -> kiana_agent_core::types::AgentMessage {
        kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text(text.to_string()),
                timestamp: 0,
            },
        ))
    }

    struct PassingShellExecute;

    impl AgentToolExecute for PassingShellExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: "test result: ok. 1 passed; 0 failed; 0 ignored".to_string(),
                        text_signature: None,
                    })],
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    fn shell_tool() -> AgentTool {
        AgentTool {
            name: "bash".to_string(),
            label: "Bash".to_string(),
            description: "run a shell command".to_string(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(PassingShellExecute),
        }
    }

    #[tokio::test]
    async fn shell_execution_record_supports_tests_passed_acceptance() {
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            parent(vec![shell_tool()]),
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        runtime
            .dispatch_tool_with(
                "call-1",
                "bash",
                json!({"command": "cargo test"}),
                None,
                None,
            )
            .await
            .expect("dispatch");
        let records = runtime.execution_records();
        assert_eq!(records.len(), 1);
        assert!(records[0].verify(), "runtime-stamped receipt must verify");
        let report = crate::acceptance::evaluate_acceptance(
            &["tests_passed:cargo test".to_string()],
            &crate::acceptance::AcceptanceContext::new(std::env::temp_dir(), &records),
        );
        assert_eq!(
            report.status,
            crate::acceptance::AcceptanceStatus::Passed,
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn a_custom_tool_shadowing_bash_is_not_shell_evidence() {
        // 扩展/SDK 工具可以遮蔽内建 `bash`：它执行的不是 shell 命令，输出不能被
        // 当成 tests_passed 的证据（与权限判定共用同一个 `builtin`）。
        let mut parent = parent(vec![shell_tool()]);
        parent.custom_tools.push("bash".to_string());
        let runtime = SubagentRuntime::prepare(
            definition(Some("*")),
            parent,
            SubagentRuntimeOptions::default(),
        )
        .expect("prepare");
        runtime
            .dispatch_tool_with(
                "call-1",
                "bash",
                json!({"command": "cargo test"}),
                None,
                None,
            )
            .await
            .expect("dispatch");
        assert!(
            runtime.execution_records().is_empty(),
            "a custom tool named bash must not be treated as shell evidence"
        );
    }
}
