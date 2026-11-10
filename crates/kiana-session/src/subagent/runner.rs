//! 子代理模型循环（生产入口）。
//!
//! 第 11 周只实现了 [`SubagentRuntime`]（工具池 / 权限 / 预算 / transcript），
//! 但没有驱动模型循环的入口；本模块补上这一层：
//!
//! 1. 用 [`kiana_agent_core::Agent`] 创建子代理 agent：system prompt 来自
//!    [`SubagentRuntime::system_prompt`]，工具来自运行时工具池（执行时统一经过
//!    [`SubagentRuntime::dispatch_tool`]，因此 allow/deny 与权限引擎仍然生效）；
//! 2. 订阅 agent 事件：`turnStart` 计 turn 并检查 `maxTurns`，`messageEnd` 增量写
//!    transcript，`turnEnd` 把 assistant `usage` 计入 token 预算（超限时中止 run）；
//! 3. prompt 结束后按最后的 assistant `stopReason` 写终态（completed / failed /
//!    cancelled）；yield 契约存在时把装配结果作为终态 `result`；
//! 4. 返回终态、完整消息、usage、receipts 与会话文件路径。
//!
//! 生产入口是 [`crate::agent_session::AgentSession::spawn_subagent`] /
//! [`crate::agent_session::AgentSession::spawn_named_subagent`]：SDK/CLI/RPC 前端无需
//! 自己拼 agent loop。headless 子代理的审批语义沿用运行时（`Ask` → 拒绝）；交互式审批
//! 回调仍留给后续接线（运行时的 `NeedsApproval` 会成为 failed tool result）。

use std::sync::Arc;

use kiana_agent_core::agent::{Agent, AgentInitialState, AgentListener, AgentOptions};
use kiana_agent_core::types::{
    AgentEvent, AgentMessage, AgentTool, AgentToolError, AgentToolExecute, AgentToolResult,
    AgentToolUpdateCallback, BoxFuture,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, JsonValue, Model, StopReason, Usage,
};
use tokio_util::sync::CancellationToken;

use super::isolation::workspace_has_changes;
use super::result::{
    ReceiptStatus, SubagentStopReason, SubagentTerminal, SubagentTerminalState, SubagentUsage,
    ToolReceipt,
};
use super::runtime::{SubagentError, SubagentRuntime};
use super::yield_tool::create_yield_tool_with_state;

/// 一次子代理运行的输入。
#[derive(Clone)]
pub struct SubagentRunOptions {
    /// 实际使用的模型（`AgentSession` 负责解析 `inherit` / `model:`）。
    pub model: Model,
    /// 覆盖流函数；`None` 时使用 kiana 的 `streamSimple` 规范实例。
    pub stream_fn: Option<kiana_agent_core::types::StreamFn>,
    /// 用户消息（任务描述）。
    pub prompt: String,
    /// 取消信号：触发后 abort 当前 run，终态为 cancelled。
    pub cancel: Option<CancellationToken>,
    /// 覆盖转发给 provider 的 session id；缺省用子会话 id。
    pub session_id: Option<String>,
}

/// 一次子代理运行的结果。
#[derive(Debug, Clone)]
pub struct SubagentRunOutcome {
    /// 终态（completed / failed / cancelled / timed_out）。
    pub terminal: SubagentTerminalState,
    /// 运行结束时的完整 transcript（含初始消息）。
    pub messages: Vec<AgentMessage>,
    /// 累计 usage。
    pub usage: SubagentUsage,
    /// 工具执行凭证。
    pub receipts: Vec<ToolReceipt>,
    /// yield 契约存在时的装配输出。
    pub output: Option<JsonValue>,
    /// 终态验收报告（criteria 为空时是显式 `Unverified`）。
    pub acceptance: Option<crate::acceptance::AcceptanceReport>,
    /// 持久化会话文件（内存会话为 `None`）。
    pub session_file: Option<String>,
    /// 隔离工作区收尾结果（Shared / 无隔离时为 `None`）。
    pub isolation: Option<SubagentIsolationOutcome>,
}

/// 隔离子代理工作区的收尾结果。
///
/// 有改动的工作区会被保留（`retained = true` 且 ownership 记录标记 `retained`），
/// 调用方可以审阅、合并或 `resume_subagent`；无改动的已在终态后清理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentIsolationOutcome {
    /// 子代理工作目录（`IsolatedWorkspace::root`）。
    pub path: String,
    /// 实际后端（`git-worktree` / `reflink` / `overlayfs`）。
    pub backend: crate::subagent::isolation::IsolationBackend,
    /// 是否有改动并因此保留。
    pub retained: bool,
    /// 有改动但 ownership 记录写入失败时的错误。
    ///
    /// 此时本进程仍靠 `Drop` 守卫保住工作区，但记录没有标记 `retained`，下一个进程
    /// 的 sweep 可能回收它；宿主必须能区分“无改动”与“保留失败”。
    pub retain_error: Option<String>,
}

/// 把运行时工具替换成经过 `dispatch_tool` 的执行器。
///
/// 直接克隆 `AgentTool` 会让 agent loop 绕过子代理的 allow/deny 与权限引擎；包一层后
/// schema / `prepareArguments` 不变，执行统一走运行时。
struct SubagentToolExecute {
    runtime: Arc<SubagentRuntime>,
    tool_name: String,
}

impl AgentToolExecute for SubagentToolExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            self.runtime
                .dispatch_tool_with(tool_call_id, &self.tool_name, params, signal, on_update)
                .await
                .map_err(|error| AgentToolError {
                    message: error.to_string(),
                    name: Some("SubagentToolError".to_string()),
                    stack: None,
                })
        })
    }
}

fn wrap_tool(tool: &AgentTool, runtime: &Arc<SubagentRuntime>) -> AgentTool {
    let mut wrapped = tool.clone();
    wrapped.execute = Arc::new(SubagentToolExecute {
        runtime: Arc::clone(runtime),
        tool_name: tool.name.clone(),
    });
    wrapped
}

/// assistant 消息里的可见文本。
fn assistant_text(message: Option<&AssistantMessage>) -> Option<String> {
    let message = message?;
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// 把 yield 装配器的当前值导出（没有 label 时为 `None`）。
///
/// 走装配器自己的投影（含单 `result` label 的 schema 感知拆包），保证取消/失败
/// 路径与 `finish()` 成功路径给出同一种输出形状。
fn assembler_output(
    assembler: &Option<Arc<std::sync::Mutex<super::yield_tool::YieldAssembler>>>,
) -> Option<JsonValue> {
    let assembler = assembler.as_ref()?;
    let assembler = assembler
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if assembler.values().is_empty() {
        return None;
    }
    Some(assembler.output())
}

/// JSON 文本输出统一走 `kiana-json`：数字格式与键序必须复刻 `JSON.stringify`。
fn encode_json(value: &JsonValue) -> String {
    kiana_json::stringify(&kiana_json::normalize_numbers(value.clone()))
}

/// 驱动一个子代理直到终态。
///
/// 运行前会写入 definition 的 `initialPrompt`（transcript 为空时）；运行期间 transcript
/// 增量落盘，因此进程内错误也不会丢失已产生的消息。
pub async fn run_subagent(
    runtime: Arc<SubagentRuntime>,
    options: SubagentRunOptions,
) -> Result<SubagentRunOutcome, SubagentError> {
    // 子会话初始 prompt 落盘失败必须中止：继续跑会让 Work 在缺 transcript 的
    // 情况下报成功，后续 resume/审计不可用（主会话路径同样把落盘错误当致命）。
    runtime.seed_initial_prompt()?;

    let yield_contract = runtime.yield_contract().cloned();
    // agent loop 按第一个同名工具解析调用；契约存在时同名的池工具（父会话/扩展注册的
    // `yield`）必须让位，否则模型的 yield 永远不会到达装配器。
    let mut tools: Vec<AgentTool> = runtime
        .tool_pool()
        .iter()
        .filter(|tool| yield_contract.is_none() || tool.name != "yield")
        .map(|tool| wrap_tool(tool, &runtime))
        .collect();
    let yield_assembler = yield_contract.map(|contract| {
        let (tool, assembler) = create_yield_tool_with_state(contract);
        tools.insert(0, tool);
        assembler
    });

    let state = AgentInitialState {
        system_prompt: Some(runtime.system_prompt()),
        model: Some(options.model.clone()),
        thinking_level: None,
        tools: Some(tools),
        messages: Some(runtime.messages()),
    };
    let agent = Agent::new(AgentOptions {
        initial_state: Some(state),
        stream_fn: options.stream_fn.clone(),
        session_id: Some(
            options
                .session_id
                .clone()
                .unwrap_or_else(|| runtime.transcript_link().child_session_id.clone()),
        ),
        ..Default::default()
    })
    .map_err(|error| SubagentError::Agent {
        message: error.to_string(),
    })?;

    let weak = agent.downgrade();
    let listener_runtime = Arc::clone(&runtime);
    let listener: AgentListener = Arc::new(move |event, _signal| {
        let runtime = Arc::clone(&listener_runtime);
        let agent = weak.clone();
        Box::pin(async move {
            match event {
                AgentEvent::TurnStart => {
                    // 宿主可能在事件到达前已经 timeout()/fail()（例如外部取消）；
                    // 终态已定时继续跑只会产生归属不明的消息。
                    if runtime.record_turn() || runtime.terminal().is_some() {
                        agent.abort();
                    }
                }
                AgentEvent::MessageEnd { message } => {
                    // 落盘失败不能静默：in-memory transcript 仍有消息，但磁盘上的
                    // 子会话会缺条目；标记失败并中止，交给宿主重试/诊断。
                    if let Err(error) = runtime.append_message_if_new(message) {
                        runtime.fail(format!("failed to persist subagent transcript: {error}"));
                        agent.abort();
                    }
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let Some(assistant) = message.as_assistant()
                        && runtime.record_usage(&assistant.usage)
                    {
                        agent.abort();
                    }
                }
                _ => {}
            }
        })
    });
    let subscription = agent.subscribe(listener);

    let prompt = agent.prompt_text(options.prompt.clone());
    tokio::pin!(prompt);
    let mut cancelled = false;
    let prompt_result = if let Some(cancel) = options.cancel.clone() {
        tokio::select! {
            result = &mut prompt => result,
            _ = cancel.cancelled() => {
                cancelled = true;
                agent.abort();
                let _ = prompt.await;
                Ok(())
            }
        }
    } else {
        prompt.await
    };
    subscription.unsubscribe();

    if let Err(error) = prompt_result {
        runtime.fail(error.to_string());
    }

    let messages = runtime.messages();
    let last_assistant = messages
        .iter()
        .rev()
        .find_map(|message| message.as_assistant().cloned());
    let mut output = assembler_output(&yield_assembler);

    if cancelled {
        runtime.cancel();
    } else if runtime.terminal().is_none() {
        match last_assistant.as_ref().map(|message| message.stop_reason) {
            Some(StopReason::Error) => {
                let error = last_assistant
                    .as_ref()
                    .and_then(|message| message.error_message.clone())
                    .unwrap_or_else(|| "model error".to_string());
                runtime.fail(error);
            }
            Some(StopReason::Aborted) => {
                runtime.cancel();
            }
            Some(StopReason::Length) => {
                // 输出被 max_tokens 截断：这不是一个可用的完整结果，不能按 completed
                // 交给调用方（否则宿主要么静默采信半截答案，要么完全不知道发生了截断）。
                let state =
                    SubagentTerminalState::failed("model output was truncated at the token limit")
                        .with_stop_reason(SubagentStopReason::ModelLengthCapped)
                        .with_usage(runtime.usage_snapshot());
                runtime.try_set_terminal(state);
            }
            _ => {
                // 模型已经停下但没有写过终态：对 yield 契约做最终校验。`submit` 在
                // 半成品阶段故意放过缺失的 required 字段，只有这里能判定“yield 不完整”。
                let yield_error = match &yield_assembler {
                    Some(assembler) => {
                        let mut assembler = assembler
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        // 从未调用 yield、或调用都是畸形 payload 且还有重试机会时，
                        // 保留 assistant 文本回退；调用被接受但装配为空（例如
                        // `yield {}`）必须让 `finish()` 判定 required 缺失。已经用尽
                        // 重试的 yield 必须直接判失败：模型明确尝试过结构化输出，
                        // 静默改用正文会让宿主误以为契约被满足；而且对没有 required
                        // 字段的契约，空对象能让 `finish()` 成功，从而丢掉正文。
                        if assembler.values().is_empty()
                            && assembler.attempted()
                            && !assembler.accepted()
                            && assembler.attempts_exhausted()
                        {
                            Some(
                                "yield schema validation failed: no submission was accepted before retries were exhausted"
                                    .to_string(),
                            )
                        } else if assembler.values().is_empty()
                            && (!assembler.attempted() || !assembler.accepted())
                        {
                            None
                        } else {
                            match assembler.finish() {
                                Ok(finished) => {
                                    output = Some(finished.output);
                                    None
                                }
                                Err(error) => Some(error.to_string()),
                            }
                        }
                    }
                    None => None,
                };
                if let Some(message) = yield_error {
                    runtime.fail(message);
                } else {
                    let result = output
                        .as_ref()
                        .map(encode_json)
                        .or_else(|| assistant_text(last_assistant.as_ref()))
                        .unwrap_or_else(|| "completed".to_string());
                    runtime.complete(result);
                }
            }
        }
    }

    let terminal = runtime
        .terminal()
        .unwrap_or_else(|| SubagentTerminalState::failed("subagent finished without a terminal"));
    // 被 abort 掉的在途工具调用不会留下 `ToolCallFinished`；按终态补一条凭证，
    // 否则 Work 证据会丢掉一个可能已经产生副作用的调用。
    let pending_status = match terminal.terminal {
        SubagentTerminal::Cancelled => ReceiptStatus::Cancelled,
        SubagentTerminal::TimedOut | SubagentTerminal::PollingTimedOut => ReceiptStatus::TimedOut,
        _ => ReceiptStatus::Unknown,
    };
    let receipts = runtime.finalize_pending_receipts(pending_status);
    let usage = runtime.usage_snapshot();
    // 验收必须在隔离工作区仍存在时完成：`file:` 类 criterion 直接读子代理的工作目录，
    // 而最后一个 Arc 释放时 `Drop` 会清理“没有改动”的工作区。
    let acceptance = crate::acceptance::evaluate_acceptance(
        runtime.acceptance_criteria(),
        &crate::acceptance::AcceptanceContext::new(
            &runtime.parent().cwd,
            &runtime.execution_records(),
        ),
    );
    // 隔离收尾：有改动就保留（ownership 记录标记 `retained`），无改动由 `Drop` 清理。
    let isolation = runtime.isolated_workspace().map(|workspace| {
        let changed = workspace_has_changes(workspace);
        let retain_error = if changed {
            runtime
                .retain_isolation()
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        SubagentIsolationOutcome {
            path: workspace.root().to_string_lossy().into_owned(),
            backend: workspace.backend(),
            retained: changed && retain_error.is_none(),
            retain_error,
        }
    });
    Ok(SubagentRunOutcome {
        terminal,
        messages,
        usage,
        receipts,
        output,
        acceptance: Some(acceptance),
        session_file: runtime.session_file(),
        isolation,
    })
}

/// 从累计 usage 估算（测试/诊断用；运行路径直接使用 agent 上报的 usage）。
pub fn usage_from(messages: &[AgentMessage]) -> Usage {
    let mut usage = Usage::default();
    for message in messages {
        if let Some(assistant) = message.as_assistant() {
            usage.input = usage.input.saturating_add(assistant.usage.input);
            usage.output = usage.output.saturating_add(assistant.usage.output);
            usage.cache_read = usage.cache_read.saturating_add(assistant.usage.cache_read);
            usage.cache_write = usage
                .cache_write
                .saturating_add(assistant.usage.cache_write);
            usage.total_tokens = usage
                .total_tokens
                .saturating_add(assistant.usage.total_tokens);
        }
    }
    usage
}
