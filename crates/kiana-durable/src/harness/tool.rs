//! `harness/tool.ts`（976 行）：内建工具任务。
//!
//! 一个工具任务在它的 `call` 阶段里完成解析、校验、`beforeTool`、记录 intent、执行、`afterTool` 与结算，
//! 所以解析与结算之间不会有别的阶段插入。`execute` 只在恢复时到达，按 replay 规则决定是否重跑。
//! 模型发起的调用以追加 `kiana.tool-result` 条目结算；嵌套调用把结果写进调用者的
//! `kiana.tool.nested-call` 家族文档。
//!
//! 与 TS 的差异（交付说明）：
//! - `structuredOutput` 的 schema 校验不做 TS 的编译缓存（`Compile(schema)` 的 `WeakMap`）；
//! - 工具任务的分型 I/S/R 在 Rust 侧擦除为 `JsonValue`（与 compaction 相同），阶段内自行解析。
//!
//! # 为什么需要这个模块
//! 工具调用是模型输出里会触发副作用的入口。generation 任务为本轮已提供且实际启动的每个
//! `ToolCall` 创建一个工具任务（用 [`TOOL_TASK`]）；本轮没有提供的工具当场写
//! `tool_unavailable` 结果、不建任务，顺序轮里还没轮到的调用先记入 `pending`。调度器再按
//! checkpoint 的 `phase` 调用这里的阶段。没有它，模型给出的调用只是一段 JSON：没人校验参数、
//! 没人跑 hook、没有持久 intent，中断恢复时也无从判断一次调用是否已经执行过。
//!
//! # 输入与输出
//! 输入是任务记录里的 [`ToolTaskInput`]：模型调用只存 assistant 条目 ID 与 `call_id`
//! （调用内容以条目为准，不复制）；嵌套调用把 `ToolCall` 随输入携带，因为没有任何条目
//! 持有它。输出写在任务终结状态里（[`ToolTaskResult`]）：模型调用记录 `kiana.tool-result`
//! 条目 ID，必要时带 `control`；嵌套调用的结果写进调用者任务作用域的 [`NESTED_CALL_DOC`]
//! 文档，任务结果本身只标记 `Nested`。
//!
//! # 一次 call 阶段的执行流程
//! 1. 反序列化输入；模型调用再从 assistant 条目里按 `call_id` 找出 `ToolCall`（`read_call`）。
//! 2. 在 `agent.tools`（模型调用）或 `agent.callable`（嵌套调用）里按名字找
//!    [`ToolRegistration`]；找不到时以 `tool_unavailable` 错误结果结算，任务仍是 `completed`。
//! 3. 跑工具的 `prepare_arguments`，再按 `tool.tool.parameters` 校验并取得强制转换后的参数；
//!    任何一步失败都以 `invalid_arguments` 结算。
//! 4. 依次调用 `beforeTool` hook：可替换参数或阻止调用。抛错的 hook 在未被取消时被当成
//!    “阻止”，错误文本进入 `blocked` 诊断；一旦被阻止，后续 handler 不再调用。
//! 5. 对 hook 改过的参数重新校验（hook 可能交出不合 schema 的对象）。
//! 6. 一次提交写 intent：槽位进入 `running`，checkpoint 变为
//!    [`ToolTaskCheckpoint::Execute`]。这一步之后若进程中断，`execute` 阶段就能看到
//!    “可能已产生副作用”。
//! 7. `run` 执行工具（输出经 `api.output`/`api.diagnostic`/`api.details` 收集，并按节流
//!    发布到 live 槽位），跑 `afterTool`，再由 `settle` 追加结果条目/嵌套结果并置终态。
//!
//! # 恢复与中止
//! - `execute` 阶段只在 checkpoint 的 `phase` 为 `execute` 时被调度（调度器按
//!   `checkpoint.phase` 取阶段；停在 `call` 的任务会重新跑 `call`）。重跑要求“存下的
//!   replay 策略”与“当前注册的工具的策略”同时为 [`ReplayPolicy::Safe`]；否则以
//!   `interrupted` 结算失败，并用槽位里已持久化的部分输出构造结果（`SettleBuild::FromSlot`
//!   在结算提交里才读槽位，避免读到进度提交之前的旧快照）。
//! - `abort` 阶段按 `TaskAbortReason::Restart` 与 checkpoint 区分 `aborted` / `interrupted`
//!   / `abandoned`：后两者分别表示“已写 intent，可能跑过”和“工具从未开始”。
//!
//! # 约束与失败模式
//! - 输出有界：上限来自工具的 `output_limits`，缺省 `DEFAULT_MAX_BYTES` 与
//!   `DEFAULT_MAX_LINES`；被丢弃的部分生成 `truncated` 警告诊断，工具输出本身不算错误。
//! - `run` 把借用的 runtime 延长为 `'static`（`unsafe`）。安全前提是：工具发起的 admission
//!   由 `settle_admissions`/`abort_admissions` 等完；`Progress::stop()` 在返回前置 stopped、
//!   解除定时器并等完在飞提交，此后的 `schedule` 见到 stopped 不再发起提交；工具实现不得
//!   把 `api` 带出本次调用。
//! - `settle` 也用 `static_runtime` 延长借用，但只依赖“提交闭包不越过 `commit` 使用该引用”
//!   （见 `live::static_runtime` 的契约）；它不创建 `Progress`，也没有 admission。
//! - 工具任务的 I/S/R 都是 `JsonValue`：checkpoint 与结果的形状没有编译期检查，解析失败
//!   就是阶段错误。输入/checkpoint 的字段名与判别值是持久格式，改动会让旧任务无法恢复。
//! - 同一个 `(父任务, key)` 只能对应一个嵌套调用；重复出现时要求工具名与参数一致，否则
//!   创建提交报错，防止 key 被复用成另一个调用。

use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use kiana_ai::types::{
    AssistantContentBlock, Message, TextContent, TextKind, ToolCall, ToolResultMessage,
    UserContentBlock,
};
use kiana_ai::utils::validation::{
    check_json_schema, collect_json_schema_errors, validate_tool_arguments,
};
use kiana_chord::context::{Context, abort_error, await_with_context};
use kiana_chord::delta::index::{overlap, owned_slice};
use kiana_chord::output::{
    OutputBuffer, OutputChunk, OutputLimits as ChordOutputLimits, OutputSkip, Progress,
    ProgressClock, ProgressFailure, Retain, bound_output, utf8_byte_length,
};
use serde::{Deserialize, Serialize};

use crate::env::{ShellOutputSkip, ShellOutputWindow};
use crate::harness::agent::tool_extension;
use crate::harness::live::{
    LIVE_DOC, PROGRESS_COMMIT, SlotStatus, TOOL_TASK_KIND, TaskCommit, clear_progress, finish_slot,
    hook_api, hook_failure, hook_handlers, nested_tools_mut, remove_nested_slots, state_checkpoint,
    tool_slot,
};
use crate::harness::types::{
    AnyTask, BeforeToolOutput, DiagnosticSeverity, NestedToolExecutionResult, NestedToolOptions,
    OutputRetain, ReplayPolicy, RetainedOutput, ToolControl, ToolDiagnostic, ToolExecutionApi,
    ToolExecutionResult, ToolHookCall, ToolHooks, ToolOutput, ToolRegistration,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::types::documents::{DocDefinition, DocFamilyToken};
use crate::types::{
    AssistantEntry, BoxError, CommitOptions, ConversationId, DocTarget, DocumentFork,
    DocumentObserver, DocumentReader, DocumentSemantics, EntryId, JsonObject, JsonValue,
    ReportFrom, RunningTask, ScopeKind, TaskDefinition, TaskId, TaskOptions, TaskOutcome,
    TaskOwnership, TaskPhase, TaskState, ToolResultEntry, Tx, TypedEntryDraft, define_doc_family,
    id_from_number,
};

/// 工具任务的 hook 表类型。
///
/// 它就是任务定义里的类型参数 `H`。注册表按名字存放擦除后的处理器，`hook_handlers` 用
/// `downcast_ref::<Arc<dyn ToolHooks>>()` 取回；因此注册时值必须正好是 `Arc<dyn ToolHooks>`，
/// 具体实现类型不能被 downcast 命中。
pub type ToolHookTable = Arc<dyn ToolHooks>;

/// 模型发起的调用，或运行中工具通过 `executeTool()` 发起的嵌套调用。
///
/// # 持久格式
/// 任务记录的 `input` 字段就是本枚举的序列化结果（判别键 `kind`，字段 camelCase）；
/// 恢复与中止阶段会重新反序列化它，`codemode` 也按同一形状解析任务输入，改字段名或判别值
/// 会让旧任务无法恢复。
///
/// `Model` 不复制调用内容：调用在 assistant 条目里，按 `(assistant, call_id)` 定位；
/// `Nested` 没有条目可依，所以携带整份 `ToolCall`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum ToolTaskInput {
    /// 从 assistant 条目读的调用。
    #[serde(rename_all = "camelCase")]
    Model {
        /// assistant 条目。
        assistant: EntryId,
        /// 调用 ID。
        call_id: String,
    },
    /// 嵌套调用：调用随输入携带，因为没有任何条目持有它。
    #[serde(rename_all = "camelCase")]
    Nested {
        /// 调用者任务。
        parent: TaskId,
        /// 调用者的调用 ID。
        parent_call_id: String,
        /// 调用者给的键。
        key: String,
        /// 调用。
        call: ToolCall,
        /// `Some(false)`：不把运行输出、details 或诊断提交进槽位。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<bool>,
    },
}

impl ToolTaskInput {
    /// 判别值。
    ///
    /// 返回 `"model"` 或 `"nested"`，与 serde 的 `kind` 标签一致。
    pub fn kind(&self) -> &'static str {
        match self {
            ToolTaskInput::Model { .. } => "model",
            ToolTaskInput::Nested { .. } => "nested",
        }
    }
}

/// 工具任务的 checkpoint。
///
/// `Call` 没有负载：call 阶段没有需要持久化的中间状态，恢复时整个阶段从头重跑。
/// `Execute` 在执行前提交，是恢复时判断“是否已经进入执行”的唯一依据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum ToolTaskCheckpoint {
    /// 解析与结算在 `call` 里。
    Call,
    /// 执行前的持久 intent。
    Execute {
        /// 最终参数。
        arguments: JsonObject,
        /// replay 策略。
        replay: ReplayPolicy,
    },
}

/// 工具任务的结果。
///
/// 两个变体都写进任务的终结 `result`：generation 读取模型调用结果里的 `control` 决定
/// post-tools 行为（`generation::finish_tool_round`）；嵌套调用的结果正文在调用者文档里，
/// 这里只标记种类。
///
/// 版本 1 的任务若在升级时已进入 `completing` 并持有结果，该结果可能是没有 `kind` 的
/// `{ entryId, control? }`；generation 把没有 `kind` 的结果按模型调用读取
/// （`generation::finish_tool_round`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolTaskResult {
    /// 模型发起的调用：结果就是它的转录条目。
    Model {
        /// 条目。
        entry_id: EntryId,
        /// post-tools 控制。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        control: Option<ToolControl>,
    },
    /// 嵌套调用：结果在调用者的 `kiana.tool.nested-call` 里。
    Nested,
}

/// 做过嵌套调用的工具任务的备忘。
///
/// 它是 memo（不是文档）。`admit_nested` 在创建嵌套任务之前写入 `true`；`settle` 读到它就
/// 在结算提交里移除本任务的全部嵌套槽（`remove_nested_slots`），因为父任务结束后这些槽
/// 不再需要展示。
pub const MADE_NESTED_CALLS: &str = "kiana.tool.madeNestedCalls";

/// 嵌套调用文档的家族（每个调用一个，随调用者任务退休）。
///
/// 文档按 `(调用者任务, key)` 寻址（`DocTarget::task_family`）；`scope: Task` 让它随调用者
/// 任务退休；`history`/`fork` 字段对任务作用域不生效（`document_create` 只为会话文档记录
/// `history`/`fork`，见 `types::document_create`），`checkpoint_when` 恒为 false（任务作用域
/// 文档只保留 delta，见 `tests/codemode_checkpoints.rs`）。
///
/// 值形如 [`NestedCallState`]：`taskId` 在创建提交里写入，`result` 由嵌套任务结算时写入。
pub static NESTED_CALL_DOC: LazyLock<DocFamilyToken> = LazyLock::new(|| {
    define_doc_family(
        DocDefinition::family(
            "kiana.tool.nested-call",
            1,
            DocumentSemantics {
                scope: ScopeKind::Task,
                history: None,
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| JsonObject::new())
        .with_checkpoint_when(|_, _, _| false),
    )
});

/// 工具任务如何结束。
///
/// `Completed` 也可能带错误结果（`is_error: true`）：任务成功结算，只是工具返回了错误；
/// `Failed` 表示执行抛错或被中断，任务终态是 `failed`（会取消调用拥有的会话）；
/// `Aborted` 只由 abort 阶段产生。
#[derive(Debug, Clone, PartialEq)]
pub enum Ending {
    /// 完成。
    Completed,
    /// 中止。
    Aborted,
    /// 失败（执行抛错或被中断）；记录取消意图。
    Failed {
        /// 错误消息。
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

/// `call` 阶段。
///
/// 阶段名 `call` 也是 `tool_task()` 的初始 checkpoint；正常情况下解析、执行与结算都在这里
/// 完成，`execute` 只用于恢复。
pub struct CallPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ToolHookTable> for CallPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        call(task, runtime, context).await
    }
}

/// `execute` 阶段（恢复时）。
///
/// 调度器按 checkpoint 的 `phase` 取阶段，所以只有停在上一次 intent 提交之后的任务才会到这里；
/// 它按 replay 判断：checkpoint 里存下的策略与当前注册的工具都声明 Safe 时，用 checkpoint 里的
/// 参数重跑工具（不重跑 `beforeTool`，也不重新校验参数）；否则从槽位构造 `interrupted` 结果结算。
pub struct ExecutePhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ToolHookTable> for ExecutePhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        execute(task, runtime, context).await
    }
}

/// abort 处理器。
///
/// 只结算，不执行工具；诊断码由中止原因与 checkpoint 共同决定（见 `abort`）。
pub struct AbortPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ToolHookTable> for AbortPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        abort(task, runtime, context).await
    }
}

fn parse_input(input: &JsonValue) -> Result<ToolTaskInput, BoxError> {
    Ok(serde_json::from_value(input.clone())?)
}

fn parse_checkpoint(state: &TaskState<JsonValue>) -> Result<ToolTaskCheckpoint, BoxError> {
    let checkpoint = state_checkpoint(state).ok_or("Tool task has no checkpoint")?;
    Ok(serde_json::from_value(checkpoint.clone())?)
}

async fn call(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let input = parse_input(&task.input)?;
    let call = read_call(runtime, &input, context).await?;
    // 阶段 2：解析工具。缺失不是任务失败：模型看到一条 tool_unavailable 结果，本轮照常收尾。
    let call_name = call.call.name.clone();
    let Some(tool) = resolve_tool(runtime, &input, &call_name, context).await? else {
        let error = harness_error(
            "tool_unavailable",
            &format!("Tool {call_name} is not available"),
        );
        return settle(
            runtime,
            &input,
            &call,
            Ending::Completed,
            SettleBuild::Ready(Box::new(error)),
            context,
            None,
        )
        .await;
    };
    // 阶段 3：参数修复 + schema 校验。修复抛错与校验失败都折算成 invalid_arguments。
    let prepared = prepare(&tool, call.call.arguments.clone());
    let checked = match prepared {
        Ok(args) => validate(&tool, &call.call, &args),
        Err(message) => Err(message),
    };
    let mut args = match checked {
        Ok(args) => args,
        Err(message) => {
            let result = invalid(&message);
            return settle(
                runtime,
                &input,
                &call,
                Ending::Completed,
                SettleBuild::Ready(Box::new(result)),
                context,
                None,
            )
            .await;
        }
    };
    // 阶段 4：beforeTool hook，按注册顺序执行。`block` 一旦置位，后面的 handler 跳过（用
    // `continue` 而不是 `break`，只是不再发起调用，行为与测试要求的“后续 handler 不运行”一致）。
    // block 优先于 arguments：同一 handler 同时返回两者时以阻止为准。
    let mut block: Option<String> = None;
    let api: Arc<dyn crate::harness::types::HookApi> = hook_api(runtime);
    for hook in hook_handlers::<dyn ToolHooks, _, _, _, _>(runtime, "beforeTool", context).await? {
        if block.is_some() {
            continue;
        }
        match hook
            .handler
            .before_tool(
                ToolHookCall {
                    call: ToolCall {
                        arguments: args.clone(),
                        ..call.call.clone()
                    },
                    parent: call.parent.clone(),
                },
                Arc::clone(&api),
                context,
            )
            .await
        {
            Ok(Some(BeforeToolOutput {
                arguments: replaced,
                block: blocked,
            })) => {
                if let Some(blocked) = blocked {
                    block = Some(blocked);
                } else if let Some(replaced) = replaced {
                    args = replaced;
                }
            }
            Ok(None) => {}
            Err(error) => {
                if runtime.signal().is_aborted() {
                    return Err(error);
                }
                block = Some(error.to_string());
            }
        }
    }
    if let Some(block) = block {
        let blocked = harness_error("blocked", &format!("Tool call blocked: {block}"));
        return settle(
            runtime,
            &input,
            &call,
            Ending::Completed,
            SettleBuild::Ready(Box::new(blocked)),
            context,
            None,
        )
        .await;
    }
    // 阶段 5：hook 可以替换参数，所以对最终参数重新跑一遍完整校验（prepare 不会重跑）。
    let validated = match validate(&tool, &call.call, &args) {
        Ok(args) => args,
        Err(message) => {
            let result = invalid(&message);
            return settle(
                runtime,
                &input,
                &call,
                Ending::Completed,
                SettleBuild::Ready(Box::new(result)),
                context,
                None,
            )
            .await;
        }
    };
    // 严格 JSON：repair 可能把可选属性设成 undefined，checkpoint 存不下。
    let final_args = validated;
    let task_id = id_from_number(runtime.task_id().get());
    let replay = tool.replay.unwrap_or(ReplayPolicy::Unsafe);
    let input_for_commit = input.clone();
    let final_for_commit = final_args.clone();
    let conversation_id = runtime.conversation_id();
    // 阶段 6：持久 intent。提交失败会以 `?` 让阶段失败，工具不会执行——checkpoint 承诺
    // “执行前的参数与 replay 策略”必须落盘，恢复时才能据此判断是否重跑。
    let change = TaskCommit::new(move |tx, _current| {
        let input = input_for_commit.clone();
        let final_args = final_for_commit.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            if let Some(mut slot) = tool_slot(live.value_mut(), task_id) {
                slot.set_status(SlotStatus::Running);
                // 嵌套槽位展示调用实际运行的参数；与输入比较（输入在接纳时写进槽位）。
                let changed = match &input {
                    ToolTaskInput::Nested { call, .. } => {
                        !json_equal(&json_value(&call.arguments), &json_value(&final_args))
                    }
                    ToolTaskInput::Model { .. } => false,
                };
                if changed && slot.is_nested() {
                    slot.set_arguments(final_args.clone());
                }
            }
            let checkpoint = serde_json::to_value(ToolTaskCheckpoint::Execute {
                arguments: final_args,
                replay,
            })?;
            Ok(Some(TaskState::Running { checkpoint }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await?;
    // 阶段 7：执行与结算。intent 已提交：若此后进程中断，恢复时的 execute 阶段能看到这条
    // intent（Safe 重跑，否则按可能已运行结算）。
    run(runtime, &input, &call, &tool, &final_args, context).await
}

async fn execute(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let checkpoint = parse_checkpoint(&task.state)?;
    let ToolTaskCheckpoint::Execute { arguments, replay } = checkpoint else {
        return Err("Tool task is not executing".into());
    };
    let input = parse_input(&task.input)?;
    let call = read_call(runtime, &input, context).await?;
    let tool = resolve_tool(runtime, &input, &call.call.name, context).await?;
    // 重跑的两个条件都要满足：checkpoint 里存下的策略与当前注册的工具都声明 Safe。任一为
    // Unsafe（或工具已不在注册表里）就按“可能已产生副作用”结算，绝不重跑。
    if replay == ReplayPolicy::Safe
        && tool.as_ref().and_then(|tool| tool.replay) == Some(ReplayPolicy::Safe)
    {
        // 重跑从头报告：先清掉被中断那次发布的 output/details/诊断，避免重跑期间槽位仍显示
        // 旧进度，也避免再次中断时 from_slot 把旧输出当作本次的部分输出。
        let task_id = id_from_number(runtime.task_id().get());
        let conversation_id = runtime.conversation_id();
        let change = TaskCommit::new(move |tx, _current| {
            Box::pin(async move {
                let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                let mut live = tx.doc(&target).await?;
                if let Some(mut slot) = tool_slot(live.value_mut(), task_id) {
                    clear_progress(&mut slot);
                }
                Ok(None)
            })
        });
        runtime
            .commit(&change, context, CommitOptions::default())
            .await?;
        // 条件已经要求 `tool` 存在且策略为 Safe（`None` 时 `and_then` 得到 `None`），
        // 所以这里的 expect 不会触发。
        let tool = tool.expect("safe replay requires a resolved tool");
        return run(runtime, &input, &call, &tool, &arguments, context).await;
    }
    // 结果从槽位构造（FromSlot），而不是这里读：结算提交之前可能还有进度提交落地，
    // 现在读会拿到旧的部分输出。without-replay 的工具只能报告“可能已部分运行”。
    let message = format!(
        "Tool {} was interrupted and may have partially run",
        call.call.name
    );
    // `failed` 记录取消意图，所以调用拥有的会话会被 abort。
    let ending = Ending::Failed {
        message: message.clone(),
    };
    let build = SettleBuild::FromSlot {
        code: "interrupted".to_owned(),
        message: message.clone(),
    };
    settle(runtime, &input, &call, ending, build, context, None).await
}

async fn abort(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let input = parse_input(&task.input)?;
    let call = read_call(runtime, &input, context).await?;
    // 中止原因只有两类：普通 abort（`aborted`）与重启后的放弃（`TaskAbortReason::Restart`）。
    // 重启时再按 checkpoint 区分：已写 Execute intent 的调用可能跑过（`interrupted`），
    // 仍停在 Call 的调用从未开始（`abandoned`，消息里写作 caller ended with a restart）。
    let restart = matches!(
        &task.abort_reason,
        Some(crate::types::TaskAbortReason::Restart)
    );
    let (code, message) = if !restart {
        ("aborted", format!("Tool {} was aborted", call.call.name))
    } else {
        match state_checkpoint(&task.state) {
            Some(checkpoint)
                if serde_json::from_value::<ToolTaskCheckpoint>(checkpoint.clone())
                    .map(|checkpoint| matches!(checkpoint, ToolTaskCheckpoint::Execute { .. }))
                    .unwrap_or(false) =>
            {
                (
                    "interrupted",
                    format!(
                        "Tool {} was interrupted by a restart and may have partially run",
                        call.call.name
                    ),
                )
            }
            _ => (
                "abandoned",
                format!(
                    "Tool {} was not started: its caller ended with a restart",
                    call.call.name
                ),
            ),
        }
    };
    let build = SettleBuild::FromSlot {
        code: code.to_owned(),
        message,
    };
    settle(
        runtime,
        &input,
        &call,
        Ending::Aborted,
        build,
        context,
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// Resolution and validation
// ---------------------------------------------------------------------------

async fn resolve_tool(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    input: &ToolTaskInput,
    name: &str,
    context: &dyn Context,
) -> Result<Option<ToolRegistration>, BoxError> {
    // 两套可见工具不同：模型调用查 `agent.tools`（请求提供给模型的工具）；嵌套调用查
    // `agent.callable`（嵌套调用可解析的工具）。按名字取第一个匹配项并克隆注册。
    let agent = runtime.agent(context).await?;
    let tools = match input {
        ToolTaskInput::Model { .. } => &agent.tools,
        ToolTaskInput::Nested { .. } => &agent.callable,
    };
    Ok(tools.iter().find(|tool| tool.name() == name).cloned())
}

/// 调用：嵌套调用取自输入，否则取自 assistant 条目。
///
/// `Model` 输入要求 `assistant` 条目存在、是 [`AssistantEntry`]（`kiana.assistant`），且其
/// `model[0]` 的 content 里含有 `call_id` 对应的 `ToolCall`；任何一步不满足都返回错误，
/// 让阶段失败（这不是一次工具错误结果）。找到调用后把 `tool.name`/`tool.call_id` 写进当前
/// span 的属性，供后续 telemetry 使用。
pub async fn read_call(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    input: &ToolTaskInput,
    context: &dyn Context,
) -> Result<ToolHookCall, BoxError> {
    // 嵌套调用的 ToolCall 只存在于输入里；parent 让 hook 能区分调用层级。
    if let ToolTaskInput::Nested {
        call,
        parent,
        parent_call_id,
        ..
    } = input
    {
        set_tool_attributes(context, call);
        return Ok(ToolHookCall {
            call: call.clone(),
            parent: Some(crate::harness::types::NestedParent {
                task_id: *parent,
                call_id: parent_call_id.clone(),
            }),
        });
    }
    let ToolTaskInput::Model { assistant, call_id } = input else {
        unreachable!("matched above");
    };
    // 条目不存在/不可见时 `entry` 返回 `None`；它与“条目不是 assistant”在下面得到同一个错误。
    let entry = runtime.entry(*assistant, context).await?;
    let message = entry
        .as_ref()
        .filter(|record| AssistantEntry.is(record))
        .and_then(|record| record.model.as_ref()?.first());
    // 只在 assistant 消息的 content 里找 `ToolCall`；同一 ID 出现多次时取第一个。
    let call = match message {
        Some(Message::Assistant(message)) => {
            message.content.iter().find_map(|content| match content {
                AssistantContentBlock::ToolCall(call) if &call.id == call_id => Some(call.clone()),
                _ => None,
            })
        }
        _ => None,
    };
    let Some(call) = call else {
        return Err(format!("Entry {assistant} has no tool call {call_id}").into());
    };
    set_tool_attributes(context, &call);
    Ok(ToolHookCall { call, parent: None })
}

// 把调用身份写进当前 span：之后执行、hook 与嵌套调用产生的 telemetry 都带同一组属性。
fn set_tool_attributes(context: &dyn Context, call: &ToolCall) {
    let mut attributes = kiana_telemetry::SpanAttributes::new();
    attributes.insert(
        "tool.name".to_owned(),
        kiana_telemetry::AttributeValue::String(call.name.clone()),
    );
    attributes.insert(
        "tool.call_id".to_owned(),
        kiana_telemetry::AttributeValue::String(call.id.clone()),
    );
    context.telemetry().set_attributes(attributes);
}

/// 参数，或为什么无效。
type Checked = Result<JsonObject, String>;

/// 经过工具 repair 的参数；报错的 repair 让参数无效。
///
/// repair 的入参是 JSON 视图（`JsonObject` 转成 `serde_json::Value`），返回非对象或返回错误
/// 都折算成 `invalid_arguments` 的文本；这个字符串会原样进诊断。
fn prepare(tool: &ToolRegistration, args: JsonObject) -> Checked {
    let Some(prepare_arguments) = &tool.prepare_arguments else {
        return Ok(args);
    };
    match prepare_arguments(&json_value(&args)) {
        Ok(value) => serde_json::from_value::<JsonObject>(value)
            .map_err(|_| "Arguments must be an object".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// 参数按实现 schema 校验并强制。
///
/// `validate_tool_arguments` 返回强制转换后的独立副本（应用默认值/类型转换）；这里再把顶层
/// 约束为对象——工具参数协议是对象，数组或标量即使通过 schema 也不能执行。
fn validate(tool: &ToolRegistration, call: &ToolCall, args: &JsonObject) -> Checked {
    let call = ToolCall {
        arguments: args.clone(),
        ..call.clone()
    };
    match validate_tool_arguments(&tool.tool, &call) {
        Ok(value) => serde_json::from_value::<JsonObject>(value)
            .map_err(|_| "Arguments must be an object".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

fn invalid(message: &str) -> ToolExecutionResult {
    harness_error("invalid_arguments", message)
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// 一个运行中工具通过 api 报告的东西（TS: `Reported`）。
struct Reported {
    output: OutputBuffer,
    diagnostics: Vec<ToolDiagnostic>,
    details: Option<JsonValue>,
}

/// 用解析出的实现执行，然后结算。
async fn run(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    input: &ToolTaskInput,
    call: &ToolHookCall,
    tool: &ToolRegistration,
    args: &JsonObject,
    context: &dyn Context,
) -> Result<(), BoxError> {
    // 输出上限在调用开始时定型：工具注册里的 output_limits，缺省用 truncate 的默认值。
    let limits = output_limits(tool);
    // SAFETY: 与 `hook_api` 相同。`ToolApi` 只向工具实现转发调用；进度提交的定时器由下面的
    // `progress.stop()` 解除、在飞提交由它等完，且 stop 之后不再安排新提交；工具发起的
    // admission 由 `settle_admissions`/`abort_admissions` 等完，工具实现也不得保留 `api`，
    // 所以借用在函数返回后不会被使用。
    let runtime: &'static dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable> = unsafe {
        std::mem::transmute::<
            &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
            &'static dyn crate::harness::types::TaskRuntime<
                JsonValue,
                JsonValue,
                JsonValue,
                ToolHookTable,
            >,
        >(runtime)
    };
    // 工具通过 api 报告的东西先落进这个共享缓冲；进度提交与最终结果都从它取快照，
    // 因此工具可以继续写、提交只读其中的一致快照。
    let reported = Arc::new(Mutex::new(Reported {
        output: OutputBuffer::new(limits),
        diagnostics: Vec::new(),
        details: None,
    }));
    // `progress: false` 的嵌套调用不发布运行进度；结果仍然有输出、details 与诊断。
    let streams = match input {
        ToolTaskInput::Model { .. } => true,
        ToolTaskInput::Nested { progress, .. } => *progress != Some(false),
    };
    let progress = Arc::new(publish_progress(
        runtime,
        Arc::clone(&reported),
        streams,
        context,
    )?);
    let api = Arc::new(ToolApi {
        runtime,
        call: call.call.clone(),
        reported: Arc::clone(&reported),
        limits,
        ended: Mutex::new(false),
        env: Mutex::new(None),
        sequence: Mutex::new(0),
        resumes: tool.replay == Some(ReplayPolicy::Safe),
        progress: Arc::clone(&progress),
        violation: Mutex::new(None),
        admissions: Mutex::new(Vec::new()),
    });

    let mut result = ToolExecutionResult::default();
    let mut ending = Ending::Completed;
    // 本尝试的执行时间；恢复后的重跑只计它自己。
    let mut duration_ms: Option<f64> = None;

    // 环境构建失败与被取消走与 execute() 相同的两条路径：取消向上传播，普通错误记为
    // `failed` 并生成一条 tool_error 诊断。
    // TS 把 `env = await runtime.env(context)` 放在 try 里：构建它抛错与 execute() 抛错同样结算。
    match runtime.env(context).await {
        Ok(env) => *lock(&api.env) = env,
        Err(error) => {
            if runtime.signal().is_aborted() {
                *lock(&api.ended) = true;
                reject_waiters(progress.stop().await, &error);
                return Err(Box::new(abort_error(&runtime.signal())));
            }
            result = tool_error_result(&error);
            ending = Ending::Failed {
                message: format!("Tool {} threw", call.call.name),
            };
        }
    }
    // 只有环境构建成功才执行实现；`execute` 为 None 与“executor 抛错”走同一分支。
    if matches!(ending, Ending::Completed) {
        // 计时只覆盖 execute 本身（不含 env 构建、hook 与结算）；恢复后的重跑从头计时。
        let started = std::time::Instant::now();
        let executed = match &tool.execute {
            Some(execute) => {
                execute(
                    json_value(args),
                    Arc::clone(&api) as Arc<dyn ToolExecutionApi>,
                    crate::session::context_handle(context),
                )
                .await
            }
            None => Err(Box::new(ToolExecutionError(
                "Tool implementation is missing".to_owned(),
            )) as BoxError),
        };
        // TS `Math.round(performance.now() - startedAt)`：四舍五入到整毫秒（不是向下取整）。
        duration_ms = Some((started.elapsed().as_secs_f64() * 1000.0).round());
        match executed {
            Ok(value) => result = value,
            Err(error) => {
                if runtime.signal().is_aborted() {
                    *lock(&api.ended) = true;
                    // TS 在 abort 路径不等待 admissions（随调用一起放弃）；先取消再等，避免
                    // 被取消的任务在本函数返回之后还使用借来的 runtime。
                    api.abort_admissions().await;
                    reject_waiters(progress.stop().await, &error);
                    return Err(Box::new(abort_error(&runtime.signal())));
                }
                result = tool_error_result(&error);
                // 抛错让任务以 `failed` 结束，取消调用拥有的东西。
                ending = Ending::Failed {
                    message: format!("Tool {} threw", call.call.name),
                };
            }
        }
    }
    // 从这里起 `api.output()`/`diagnostic` 等同步入口会把违规记进 `violation`，
    // `executeTool()`/`details()` 等异步入口直接返回错误（已结算的调用不能再报告）。
    *lock(&api.ended) = true;
    // TS: `ended = true; await Promise.allSettled(admissions);`——工具启动过但没 await 的嵌套调用
    // 也在这里被等到（随后由 settle 中止它们拥有的任务）。
    api.settle_admissions().await;
    // TS 在 `output()` 里同步抛错（head 保留收到 `skipped`）：与 execute() 抛错同样结算。
    if matches!(ending, Ending::Completed)
        && let Some(message) = lock(&api.violation).take()
    {
        result = ToolExecutionResult {
            output: None,
            structured_output: None,
            is_error: Some(true),
            details: None,
            diagnostics: Some(vec![tool_diagnostic("tool_error", &message)]),
            usage: None,
            control: None,
        };
        ending = Ending::Failed {
            message: format!("Tool {} threw", call.call.name),
        };
    }
    lock(&reported).output.end();
    // 仍在等进度提交的 `details()` 由终态提交（最后一次 flush）结算。
    let pending = progress.stop().await;
    // 结果定形（afterTool、输出截断、structuredOutput 校验）与终态提交分成两步：
    // final_result 失败时不写终态，直接把错误交给阶段；settle 成功才结算 pending waiter。
    let settled = match final_result(runtime, input, call, tool, &result, &api, context).await {
        Ok(settled) => settled,
        Err(error) => {
            reject_waiters(pending, &error);
            return Err(error);
        }
    };
    match settle(
        runtime,
        input,
        call,
        ending,
        SettleBuild::Ready(Box::new(settled)),
        context,
        duration_ms,
    )
    .await
    {
        Ok(()) => {
            for waiter in pending {
                let _ = waiter.send(Ok(()));
            }
            Ok(())
        }
        Err(error) => {
            reject_waiters(pending, &error);
            Err(error)
        }
    }
}

/// `execute()`（或环境构建）抛出的错误结果。
fn tool_error_result(error: &BoxError) -> ToolExecutionResult {
    ToolExecutionResult {
        output: None,
        structured_output: None,
        is_error: Some(true),
        details: None,
        diagnostics: Some(vec![tool_diagnostic("tool_error", &error.to_string())]),
        usage: None,
        control: None,
    }
}

/// 把进度 waiter 以 `error` 拒绝（TS: `waiter.reject(error)`）。
fn reject_waiters(
    waiters: Vec<tokio::sync::oneshot::Sender<Result<(), ProgressFailure>>>,
    error: &BoxError,
) {
    let failure: ProgressFailure = Arc::new(ProgressError(error.to_string()));
    for waiter in waiters {
        let _ = waiter.send(Err(Arc::clone(&failure)));
    }
}

/// 包装一个进度失败，供 `BoxError` 传播。
struct ProgressError(String);

impl std::fmt::Display for ProgressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::fmt::Debug for ProgressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ProgressError")
            .field(&self.0)
            .finish()
    }
}

impl std::error::Error for ProgressError {}

// 工具未配置的字段回落到 `DEFAULT_MAX_BYTES`/`DEFAULT_MAX_LINES`；`retain` 只识别 Tail，
// 未配置或 Head 都按 Head 处理。
fn output_limits(tool: &ToolRegistration) -> ChordOutputLimits {
    ChordOutputLimits {
        max_bytes: tool
            .output_limits
            .as_ref()
            .and_then(|limits| limits.max_bytes)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_BYTES),
        max_lines: tool
            .output_limits
            .as_ref()
            .and_then(|limits| limits.max_lines)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_LINES),
        retain: match tool.output_limits.as_ref().and_then(|limits| limits.retain) {
            Some(OutputRetain::Tail) => Retain::Tail,
            _ => Retain::Head,
        },
    }
}

/// 已提交的进度（TS 的 `written`）。
#[derive(Clone, Default)]
struct Written {
    text: String,
    details: Option<JsonValue>,
    diagnostics: usize,
}

/// Harness 时钟与定时器，用于进度节流（TS: `progressClock`）。
struct RuntimeClock {
    runtime: &'static dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
}

impl ProgressClock for RuntimeClock {
    fn now(&self) -> f64 {
        self.runtime.now() as f64
    }

    fn set_timer(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn FnOnce() + Send> {
        let slot = Mutex::new(Some(callback));
        let callback: crate::timers::TimerCallback = Arc::new(move || {
            let taken = lock(&slot).take();
            if let Some(callback) = taken {
                callback();
            }
        });
        let disarm = self.runtime.set_timer(callback, delay_ms);
        Box::new(disarm)
    }
}

/// 把工具报告的东西按节流提交进槽位（`kiana.live.tools` 或 `kiana.live.nestedTools`），每次只写自上次
/// 以来变化的部分，延迟存储（`PROGRESS_COMMIT`）；`enabled` 为假时不发布（TS: `publishProgress`）。
fn publish_progress(
    runtime: &'static dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    reported: Arc<Mutex<Reported>>,
    enabled: bool,
    context: &dyn Context,
) -> Result<Progress, BoxError> {
    let context = crate::session::context_handle(context);
    let written = Arc::new(Mutex::new(Written::default()));
    let conversation_id = runtime.conversation_id();
    let task_id = id_from_number(runtime.task_id().get());
    let clock: Arc<dyn ProgressClock> = Arc::new(RuntimeClock { runtime });
    let write = move || -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<usize, ProgressFailure>> + Send>,
    > {
        let reported = Arc::clone(&reported);
        let written = Arc::clone(&written);
        let context = Arc::clone(&context);
        Box::pin(async move {
            if !enabled {
                return Ok(0);
            }
            // 同步捕获：工具还在继续报告。`snapshot` 是当前窗口（tail 保留会在这里压缩成
            // 一个后缀块），后续只比较快照，不再长时间持锁。
            let (snapshot, current_details, current_diagnostics) = {
                let mut reported = lock(&reported);
                let snapshot = reported.output.snapshot();
                (
                    snapshot,
                    reported.details.clone(),
                    reported.diagnostics.clone(),
                )
            };
            let current_count = current_diagnostics.len();
            let (written_text, written_details, written_count) = {
                let written = lock(&written);
                (
                    written.text.clone(),
                    written.details.clone(),
                    written.diagnostics,
                )
            };
            // 诊断只追加：已提交条数之后的就是这次的新增。`min` 只是防御（正常情况下
            // 当前列表不会比已提交的短）。
            let added: Vec<ToolDiagnostic> = current_diagnostics
                [written_count.min(current_count)..]
                .to_vec();
            let details_changed = current_details != written_details;
            // 这次提交写入的体积，决定下一段暂停。
            // Progress 按 100 KiB/s 与最小间隔推算下一次提交时间。
            let mut bytes = 0usize;
            if snapshot.text != written_text {
                // 快照是已提交文本的严格扩展时，只需追加后缀；否则是 tail 窗口滑动/替换，
                // 用 overlap 在最后 65536 个 UTF-16 码元里找最长“旧文本后缀 = 新文本前缀”。
                // overlap 返回 0 表示放弃（重复字符太多或没有重叠），此时按整段写入，仍然正确。
                let shared = if snapshot.text.starts_with(&written_text) {
                    written_text.encode_utf16().count()
                } else {
                    overlap(&written_text, &snapshot.text, 65_536)
                };
                bytes += utf8_byte_length(utf16_suffix(&snapshot.text, shared));
            }
            if details_changed {
                bytes += utf8_byte_length(&kiana_json::js_object::stringify(
                    &current_details.clone().unwrap_or(JsonValue::Null),
                ));
            }
            if !added.is_empty() {
                bytes += utf8_byte_length(&kiana_json::js_object::stringify(
                    &serde_json::to_value(&added).unwrap_or(JsonValue::Null),
                ));
            }
            let text = snapshot.text.clone();
            let dropped_bytes = snapshot.dropped_bytes as u64;
            let dropped_lines = snapshot.dropped_lines as u64;
            let text_for_commit = text.clone();
            let details_for_commit = current_details.clone();
            let detailed = current_details.clone();
            let added_for_commit = added.clone();
            let change = TaskCommit::new(move |tx, _current| {
                let text = text_for_commit.clone();
                let details = details_for_commit.clone();
                let added = added_for_commit.clone();
                Box::pin(async move {
                    let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                    let mut live = tx.doc(&target).await?;
                    let Some(mut slot) = tool_slot(live.value_mut(), task_id) else {
                        return Ok(None);
                    };
                    // 把 `output` 作为一个字符串字段写：Chord 会把它差分成 append，或滑动的 tail
                    // 窗口的 trim+append；替换整个槽对象会把整个窗口记进每次提交。
                    if slot.output().unwrap_or("") != text {
                        slot.set_output(&text);
                    }
                    if dropped_bytes > 0 {
                        slot.set_dropped_bytes(dropped_bytes);
                    }
                    if dropped_lines > 0 {
                        slot.set_dropped_lines(dropped_lines);
                    }
                    // details 逐叶写，诊断追加，所以每次提交只写变化的部分。
                    if details_changed
                        && let Some(details) = details
                    {
                        crate::harness::json::assign_json(
                            slot.value_mut(),
                            crate::harness::json::TargetKey::Name("details"),
                            &details,
                            None,
                        );
                    }
                    for diagnostic in &added {
                        slot.push_diagnostic(diagnostic);
                    }
                    Ok(None)
                })
            });
            runtime
                .commit(&change, &*context, PROGRESS_COMMIT)
                .await
                .map_err(|error| -> ProgressFailure { Arc::from(error) })?;
            // 提交成功才推进 `written`：失败时下一次提交会重发同样的增量，不会丢内容。
            let mut written = lock(&written);
            written.text = text;
            written.details = detailed;
            written.diagnostics = current_count;
            Ok(bytes)
        })
    };
    let on_error = move |failure: ProgressFailure| {
        // 中止标记或关闭之后的拒绝是预期的；已提交状态保持一致。
        if !runtime.signal().is_aborted() {
            runtime.report(&*failure, None);
        }
    };
    // TS 在 `run()` 的 try 之前读 `runtime.settings.progress.outputIntervalMs`：宿主抛错直接
    // 传进 phase（faults the task）。
    Ok(Progress::with_clock(
        write,
        on_error,
        runtime.settings()?.progress.output_interval_ms,
        clock,
    ))
}

/// `text` 从 UTF-16 下标 `start_units` 起的后缀。
fn utf16_suffix(text: &str, start_units: usize) -> &str {
    if start_units == 0 {
        return text;
    }
    let mut units = 0usize;
    for (byte, character) in text.char_indices() {
        if units >= start_units {
            return &text[byte..];
        }
        units += character.len_utf16();
        if units > start_units {
            // 落在代理对中间：TS 的 slice 会切出半个代理，Rust 字符串只能取整字符。
            return &text[byte + character.len_utf8()..];
        }
    }
    ""
}

struct ToolExecutionError(String);

impl std::fmt::Display for ToolExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::fmt::Debug for ToolExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ToolExecutionError")
            .field(&self.0)
            .finish()
    }
}

impl std::error::Error for ToolExecutionError {}

// 本文件所有内部 Mutex 共用：中毒（持锁线程 panic）时取回内部值而不是继续传播 panic。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Result shaping
// ---------------------------------------------------------------------------

async fn final_result(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    input: &ToolTaskInput,
    call: &ToolHookCall,
    tool: &ToolRegistration,
    result: &ToolExecutionResult,
    api: &ToolApi<'_>,
    context: &dyn Context,
) -> Result<ToolExecutionResult, BoxError> {
    let mut harness: Vec<ToolDiagnostic> = Vec::new();
    let limits = api.limits;
    // 阶段 1：决定模型看到的内容。工具显式给了 output 就用它；否则用 `api.output()` 保留的
    // 窗口文本（已按输出上限取好 head/tail）。
    // TS 只在 result 没有显式 output 时取样保留输出。
    let retained = if result.output.is_none() {
        Some(lock(&api.reported).output.snapshot())
    } else {
        None
    };
    let (reported_details, reported_diagnostics) = {
        let reported = lock(&api.reported);
        (reported.details.clone(), reported.diagnostics.clone())
    };
    let output: Vec<UserContentBlock> = match &result.output {
        Some(output) => output.clone(),
        None => match retained.as_ref() {
            Some(retained) if retained.text.is_empty() => Vec::new(),
            Some(retained) => vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: retained.text.clone(),
                text_signature: None,
            })],
            None => Vec::new(),
        },
    };
    let mut final_result = ToolExecutionResult {
        output: Some(output.clone()),
        details: result.details.clone().or(reported_details),
        diagnostics: Some(
            reported_diagnostics
                .into_iter()
                .chain(result.diagnostics.clone().unwrap_or_default())
                .collect(),
        ),
        ..result.clone()
    };
    // 阶段 2：afterTool hook，按注册顺序执行，每个都可以整体替换结果。
    // TS 用引用同一性判断 `final.output === output`：只有 afterTool 原样带回那份数组时，保留
    // 输出的截断才报告。Rust 里用“Vec 的分配是否被原样带回来”近似：结构更新（`..result`）
    // 会移动同一份 Vec，显式复制则不会。
    let mut output_is_original = true;
    let api: Arc<dyn crate::harness::types::HookApi> = hook_api(runtime);
    for hook in hook_handlers::<dyn ToolHooks, _, _, _, _>(runtime, "afterTool", context).await? {
        let passed = final_result.clone();
        let passed_output = passed
            .output
            .as_ref()
            .map(|content| (content.as_ptr() as usize, content.len(), content.capacity()));
        match hook
            .handler
            .after_tool(call.clone(), passed, Arc::clone(&api), context)
            .await
        {
            Ok(Some(replaced)) => {
                let kept = match (replaced.output.as_ref(), passed_output) {
                    (Some(content), Some((pointer, len, capacity))) => {
                        content.as_ptr() as usize == pointer
                            && content.len() == len
                            && content.capacity() == capacity
                    }
                    (None, None) => true,
                    _ => false,
                };
                if !kept {
                    output_is_original = false;
                }
                final_result = replaced;
            }
            Ok(None) => {}
            // TS `hooks.each`：抛错的处理器上报后跳过，工具结果照常结算。
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    // 保留输出的截断只在 afterTool 保留了那份输出时适用。
    if let Some(retained) = &retained
        && retained.dropped_bytes > 0
        && output_is_original
    {
        harness.push(truncated(
            retained.dropped_bytes as u64,
            retained.dropped_lines as u64,
            Some(limits.retain),
        ));
    }
    // 阶段 3：最终内容再按输出上限裁一次（afterTool 可能加长内容）。
    let bounded = bound_content(&final_result.output.clone().unwrap_or_default(), &limits);
    if bounded.dropped_bytes > 0 {
        harness.push(truncated(
            bounded.dropped_bytes,
            bounded.dropped_lines,
            Some(limits.retain),
        ));
    }
    // 阶段 4：structuredOutput 契约。模型调用把违规作为 extension_error 上报（结果照常成立）；
    // 嵌套调用没有 UI，违规转成错误结果 + `invalid_structured_output` 诊断交给调用者。
    if let Some(broken) = structured_output_error(tool, &final_result) {
        final_result.structured_output = None;
        if matches!(input, ToolTaskInput::Nested { .. }) {
            final_result.is_error = Some(true);
            harness.push(tool_diagnostic("invalid_structured_output", &broken));
        } else {
            let extension = runtime
                .agent(context)
                .await
                .ok()
                .and_then(|agent| tool_extension(&agent, tool.name()));
            runtime.report(
                &ReportExtError(broken),
                Some(ReportFrom::Extension { extension }),
            );
        }
    }
    // 程序拿到工具自己的值：没有 schema 时就是它的 output。
    if matches!(input, ToolTaskInput::Nested { .. }) && tool.structured_output_schema.is_none() {
        final_result.structured_output = Some(output_value(&bounded.content));
    }
    // 阶段 5：错误结果的可诊断性。嵌套调用声明了 schema、结果又是错误、却没有值也没有任何
    // error 诊断时，把内容正文补成一条 tool_error，避免调用者只看到 isError 而不知道原因。
    let existing_errors = final_result
        .diagnostics
        .clone()
        .unwrap_or_default()
        .iter()
        .chain(harness.iter())
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error);
    if matches!(input, ToolTaskInput::Nested { .. })
        && tool.structured_output_schema.is_some()
        && final_result.is_error == Some(true)
        && final_result.structured_output.is_none()
        && !existing_errors
    {
        let text = bounded
            .content
            .iter()
            .filter_map(|item| match item {
                UserContentBlock::Text(text) => Some(text.text.as_str()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            harness.push(tool_diagnostic("tool_error", &text));
        }
    }
    // 诊断顺序：工具/hook 提供的诊断在前，harness 追加的在后；输出用裁剪后的内容。
    let mut diagnostics = final_result.diagnostics.clone().unwrap_or_default();
    diagnostics.extend(harness);
    Ok(ToolExecutionResult {
        output: Some(bounded.content),
        diagnostics: Some(diagnostics),
        ..final_result
    })
}

struct ReportExtError(String);

impl std::fmt::Display for ReportExtError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::fmt::Debug for ReportExtError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ReportExtError")
            .field(&self.0)
            .finish()
    }
}

impl std::error::Error for ReportExtError {}

/// 结果的 `structuredOutput` 为什么破坏工具的契约。
///
/// `Some(message)` 表示违反契约，共有三种情况：
/// - 工具没有声明 schema 却返回了 `structuredOutput`；
/// - 声明了 schema，但结果既没有 `structuredOutput` 也不是 `isError: true`
///   （错误结果允许省略 `structuredOutput`）；
/// - 有值但 `check_json_schema` 不通过，消息带第一处错误的实例路径与原因：路径取自
///   `instance_path`，去掉前导 `/` 后把 `/` 换成 `.`，空路径写成 `root`。
///
/// 消息文本被测试锁定（模块内单测断言完整文本，`tests/harness_structured_output.rs` 断言
/// 前缀），改措辞会同时改测试。
pub fn structured_output_error(
    tool: &ToolRegistration,
    result: &ToolExecutionResult,
) -> Option<String> {
    let Some(schema) = tool.structured_output_schema.as_ref() else {
        return result.structured_output.as_ref().map(|_| {
            format!(
                "Tool {} returned structuredOutput but declares no structuredOutputSchema",
                tool.name()
            )
        });
    };
    let Some(value) = result.structured_output.as_ref() else {
        return if result.is_error == Some(true) {
            None
        } else {
            Some(format!("Tool {} returned no structuredOutput", tool.name()))
        };
    };
    if check_json_schema(schema, value) {
        return None;
    }
    let first = collect_json_schema_errors(schema, value).into_iter().next();
    let path = first
        .as_ref()
        .map(|error| {
            let path = error
                .instance_path
                .trim_start_matches('/')
                .replace('/', ".");
            if path.is_empty() {
                "root".to_owned()
            } else {
                path
            }
        })
        .unwrap_or_else(|| "root".to_owned());
    let message = first
        .map(|error| error.message)
        .unwrap_or_else(|| "invalid".to_owned());
    Some(format!(
        "Tool {} returned structuredOutput that does not match its schema: {path}: {message}",
        tool.name()
    ))
}

/// 结算要用的结果来源（TS `settle` 的 `build(slot)`）。
///
/// `FromSlot` 在结算提交里才从槽位构造结果：进度提交可能落在“读快照”和“结算提交”之间，
/// 提前读会把中断/中止结果里的持久部分输出读旧。
#[derive(Clone)]
enum SettleBuild {
    /// 已经算好的结果。
    Ready(Box<ToolExecutionResult>),
    /// 从槽位的持久部分输出构造错误结果。
    FromSlot {
        /// 诊断码。
        code: String,
        /// 诊断文本。
        message: String,
    },
}

impl SettleBuild {
    fn build(&self, slot: Option<&crate::harness::live::SlotRef<'_>>) -> ToolExecutionResult {
        match self {
            SettleBuild::Ready(result) => (**result).clone(),
            SettleBuild::FromSlot { code, message } => match slot {
                Some(slot) => from_slot(slot, code, message),
                None => harness_error(code, message),
            },
        }
    }
}

/// 提交工具的终态并让槽位进入 `done`。
///
/// 一次提交里完成：构造结果、写结果条目或嵌套结果文档、结束槽位、移除嵌套槽、把任务状态置为
/// `Terminal`。提交之前先中止本任务直接拥有的嵌套调用任务，防止父任务结束后它们继续运行。
async fn settle(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    input: &ToolTaskInput,
    call: &ToolHookCall,
    ending: Ending,
    build: SettleBuild,
    context: &dyn Context,
    duration_ms: Option<f64>,
) -> Result<(), BoxError> {
    // 结算提交里要读时钟（TS 的 `timing`）；闭包只活到 `commit` 返回。
    let runtime = crate::harness::live::static_runtime(runtime);
    // 读取必须发生在结算提交之前：`ended` 之后不再有接纳。
    // 只取“直接拥有”的任务，且按任务记录里的 input 判断它是不是嵌套调用（kind 单独不足以
    // 区分模型调用）。
    let owned = runtime.owned_tasks(context).await?;
    let nested_ids: Vec<TaskId> = owned
        .iter()
        .filter(|record| record.kind == TOOL_TASK_KIND)
        .filter(|record| {
            serde_json::from_value::<ToolTaskInput>(record.input.clone())
                .map(|input| matches!(input, ToolTaskInput::Nested { .. }))
                .unwrap_or(false)
        })
        .map(|record| record.id)
        .collect();
    let made_nested = runtime.memo(MADE_NESTED_CALLS, context).await?.is_some();
    // 先全部标记、再等任何一个：一个嵌套调用可能跑到兄弟被取消为止（TS 的 `Promise.all`）。
    let aborts = nested_ids
        .iter()
        .map(|id| runtime.abort_owned(id_from_number(id.get()), context));
    let aborted: Vec<Result<(), BoxError>> = futures::future::join_all(aborts).await;
    for outcome in aborted {
        outcome?;
    }
    let task_id = id_from_number(runtime.task_id().get());
    let input = input.clone();
    let call = call.clone();
    let build = build.clone();
    let conversation_id = runtime.conversation_id();
    let change = TaskCommit::new(move |tx, _current| {
        let input = input.clone();
        let call = call.clone();
        let build = build.clone();
        let ending = ending.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // TS 在结算提交里读槽位并构造 `build(slot)`。
            let mut slot = tool_slot(live.value_mut(), task_id);
            let result = build.build(slot.as_ref());
            let settled = match &input {
                ToolTaskInput::Nested { parent, key, .. } => {
                    // 嵌套调用：结果写进调用者的家族文档；usage 记进会话的 tools 用量桶；
                    // 槽位（若还在）标 done 并写摘要。
                    let nested = nested_result(task_id, &result, duration_ms);
                    if let Some(usage) = &nested.usage {
                        record_usage(
                            tx,
                            conversation_id,
                            UsageBucket::Tools,
                            &call.call.name,
                            usage,
                        )
                        .await?;
                    }
                    let family = DocTarget::task_family(
                        &NESTED_CALL_DOC,
                        *parent,
                        key.clone(),
                        JsonValue::Null,
                    );
                    let mut state = tx.doc(&family).await?;
                    // 文档里的 taskId 必须指回自己：若不同，说明同一个 (父任务, key) 已被
                    // 另一个任务占用，继续写会覆盖别人的结果。
                    let current: crate::harness::tool::NestedCallState =
                        serde_json::from_value(serde_json::to_value(state.value())?)
                            .unwrap_or_default();
                    if current.task_id != Some(task_id) {
                        return Err(format!(
                            "Nested call {} is not in its caller's index",
                            call.call.id
                        )
                        .into());
                    }
                    state
                        .value_mut()
                        .insert("result".to_owned(), serde_json::to_value(&nested)?);
                    if let Some(slot) = slot.as_mut()
                        && slot.is_nested()
                    {
                        finish_slot(slot);
                        slot.set_summary(&summary_of(
                            &nested,
                            &result.output.clone().unwrap_or_default(),
                        ));
                    }
                    ToolTaskResult::Nested
                }
                ToolTaskInput::Model { .. } => {
                    // 模型调用：追加 `kiana.tool-result` 条目并记录 entry id；`control` 只从
                    // “完成且结果自带 control”的调用带出，失败/中止的调用不触发 post-tools 控制。
                    // TS 在结算提交里读时钟（`const timing = { timestamp: runtime.now(), ... }`）。
                    let now = runtime.now();
                    let entry = append_tool_result(
                        tx,
                        conversation_id,
                        &call.call,
                        &result,
                        now,
                        duration_ms,
                    )
                    .await?;
                    if let Some(slot) = slot.as_mut()
                        && !slot.is_nested()
                    {
                        finish_slot(slot);
                        slot.set_entry(entry.id);
                    }
                    let control = match (&result.control, &ending) {
                        (Some(control), Ending::Completed) => Some(control.clone()),
                        _ => None,
                    };
                    ToolTaskResult::Model {
                        entry_id: entry.id,
                        control,
                    }
                }
            };
            // 本任务发起过嵌套调用：移除它的嵌套槽（包括传递的子槽），父任务结束后不再展示。
            if made_nested {
                remove_nested_slots(live.value_mut(), task_id);
            }
            let result_value = serde_json::to_value(&settled)?;
            // 终态映射：Ending 决定 outcome 的种类，`settled` 一律作为 `result` 持久化。
            Ok(Some(TaskState::Terminal {
                outcome: match ending {
                    Ending::Aborted => TaskOutcome::Aborted {
                        reason: None,
                        result: Some(result_value),
                    },
                    Ending::Failed { message } => TaskOutcome::Failed {
                        error: crate::types::TaskOutcomeError {
                            message,
                            detail: None,
                        },
                        result: Some(result_value),
                    },
                    Ending::Completed => TaskOutcome::Completed {
                        result: result_value,
                    },
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// ---------------------------------------------------------------------------
// Nested calls
// ---------------------------------------------------------------------------

/// 一个嵌套调用的持久状态（`kiana.tool.nested-call` 的值）。
///
/// 文档键是调用者的 `(parent task, key)`。`task_id` 在创建提交里写入；`result` 是
/// [`NestedToolExecutionResult`] 的序列化值，`finish_nested_call` 靠它把结果交还给调用者，
/// 缺失时（任务 faulted/orphaned，从未走到结算）退回按终态构造的错误结果。
///
/// `task_id` 缺席只可能出现在创建提交尚未落地的窗口；结算提交要求它等于自己（见 `settle`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedCallState {
    /// 嵌套调用任务；只在创建它的那次提交里缺席。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// `executeTool()` 的返回值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonValue>,
}

/// 嵌套调用的结果：严格 JSON，没有 `output` 与 `control`。
///
/// 嵌套调用没有转录条目，调用者只通过返回值拿结构化结果；`is_error` 缺省为 false，
/// `diagnostics` 缺省为空列表。`duration_ms` 由调用方传入（只计本次尝试，不含恢复前的时间）。
pub fn nested_result(
    task_id: TaskId,
    result: &ToolExecutionResult,
    duration_ms: Option<f64>,
) -> NestedToolExecutionResult {
    NestedToolExecutionResult {
        task_id,
        structured_output: result.structured_output.clone(),
        is_error: result.is_error.unwrap_or(false),
        details: result.details.clone(),
        diagnostics: result.diagnostics.clone().unwrap_or_default(),
        usage: result.usage.clone(),
        duration_ms,
    }
}

/// 从槽位的持久部分输出构造错误结果（TS: `fromSlot`）。
///
/// 只使用已经提交进槽位的内容（文本、details、诊断、丢弃计数），不读 `api` 的内存状态，
/// 因此可以在结算提交里安全调用。`retain` 传 `None`：槽位不记录保留方向，截断消息不带
/// “保留头/尾”后缀。
fn from_slot(
    slot: &crate::harness::live::SlotRef<'_>,
    code: &str,
    message: &str,
) -> ToolExecutionResult {
    let mut diagnostics = slot.diagnostics();
    let dropped_bytes = slot.dropped_bytes();
    let dropped_lines = slot.dropped_lines();
    if dropped_bytes > 0 {
        diagnostics.push(truncated(dropped_bytes, dropped_lines, None));
    }
    diagnostics.push(tool_diagnostic(code, message));
    let output = slot
        .output()
        .filter(|output| !output.is_empty())
        .map(|output| {
            vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: output.to_owned(),
                text_signature: None,
            })]
        })
        .unwrap_or_default();
    let details = slot.details().cloned();
    ToolExecutionResult {
        output: Some(output),
        is_error: Some(true),
        details,
        diagnostics: Some(diagnostics),
        structured_output: None,
        usage: None,
        control: None,
    }
}

/// Harness 自己写的错误结果：没有内容，一条带 `code` 的 `error` 诊断。
///
/// 形状固定：`output` 是 `Some(vec![])`（空内容，不是 `None`）、`is_error: Some(true)`、
/// 一条 [`DiagnosticSeverity::Error`] 诊断，其余通道为 `None`。`tool_unavailable`、
/// `invalid_arguments`、`blocked` 等“没有执行工具”的错误结果都由它构造；generation 在
/// 工具不可用/轮次中止时也用它。
pub fn harness_error(code: &str, message: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        output: Some(Vec::new()),
        is_error: Some(true),
        diagnostics: Some(vec![tool_diagnostic(code, message)]),
        structured_output: None,
        details: None,
        usage: None,
        control: None,
    }
}

// 本文件用 `tool_diagnostic` 构造的都是 error 级诊断；唯一的 warn 是 `truncated`。
fn tool_diagnostic(code: &str, message: &str) -> ToolDiagnostic {
    ToolDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: Some(code.to_owned()),
        message: message.to_owned(),
    }
}

// `retain` 为 `None` 时不写方向后缀；消息文本被 `tests/harness_tools.rs` 断言前缀。
fn truncated(dropped_bytes: u64, dropped_lines: u64, retain: Option<Retain>) -> ToolDiagnostic {
    let kept = match retain {
        None => String::new(),
        Some(Retain::Head) => " to its beginning".to_owned(),
        Some(Retain::Tail) => " to its end".to_owned(),
    };
    ToolDiagnostic {
        severity: DiagnosticSeverity::Warn,
        code: Some("truncated".to_owned()),
        message: format!(
            "Output truncated{kept}: {dropped_lines} lines, {dropped_bytes} bytes dropped"
        ),
    }
}

/// 嵌套调用摘要保留的错误文本上限。
///
/// 单位是 UTF-16 码元（`owned_slice` 的计数单位，与 TS `string.length` 一致），不是字节数。
pub const MAX_SUMMARY_ERROR_CHARS: usize = 500;

// 摘要只记录“调用者需要知道的最小信息”：错误文本只在 is_error 时收集（诊断的 error 消息
// 优先，没有诊断才用输出正文），截到 MAX_SUMMARY_ERROR_CHARS；空串写成 None。
fn summary_of(
    result: &NestedToolExecutionResult,
    output: &[UserContentBlock],
) -> crate::harness::live::NestedToolSummary {
    let error = if result.is_error {
        owned_slice(
            &error_text_of(result, output),
            0,
            Some(MAX_SUMMARY_ERROR_CHARS),
        )
    } else {
        String::new()
    };
    crate::harness::live::NestedToolSummary {
        is_error: result.is_error,
        duration_ms: result.duration_ms,
        usage: result
            .usage
            .as_ref()
            .and_then(|usage| serde_json::to_value(usage).ok()),
        error: if error.is_empty() { None } else { Some(error) },
    }
}

// 优先用诊断里的 error 消息；没有 error 诊断时才用输出正文。各段用换行连接。
fn error_text_of(result: &NestedToolExecutionResult, output: &[UserContentBlock]) -> String {
    let errors: Vec<&str> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    if !errors.is_empty() {
        return errors.join("\n");
    }
    output
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 追加一条 `kiana.tool-result` 条目。
///
/// 条目内容 = 工具输出 +（若有诊断）一段 `<harness>…</harness>` 文本，每个诊断一行
/// `[severity] message`；同一批诊断还以结构化形式写进条目 data 的 `ToolResultData.diagnostics`，
/// 让 UI 不必解析文本。`usage` 会先记进会话的 tools 用量桶；`timestamp` 与 `duration_ms`
/// 由调用方在提交回调里读时钟/计时后传入（本函数自己不做计时）。
pub async fn append_tool_result(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    call: &ToolCall,
    result: &ToolExecutionResult,
    timestamp: i64,
    duration_ms: Option<f64>,
) -> Result<crate::types::EntryRecord, BoxError> {
    let diagnostics = result.diagnostics.clone().unwrap_or_default();
    let mut content: Vec<UserContentBlock> = result.output.clone().unwrap_or_default();
    // 诊断文本块追加在内容末尾；结构化副本在下面写进条目 data，两处内容一致。
    if !diagnostics.is_empty() {
        content.push(UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: render_diagnostics(&diagnostics),
            text_signature: None,
        }));
    }
    if let Some(usage) = &result.usage {
        record_usage(tx, conversation_id, UsageBucket::Tools, &call.name, usage).await?;
    }
    let message = Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content,
        details: result.details.clone(),
        usage: result.usage.clone(),
        nested_calls: None,
        is_error: result.is_error.unwrap_or(false),
        timestamp,
        duration_ms,
    });
    let draft = ToolResultEntry.typed_draft(TypedEntryDraft {
        model: Some(vec![message]),
        data: crate::types::entries::ToolResultData {
            diagnostics: diagnostics.clone(),
        },
        head: None,
        edits: None,
    })?;
    tx.append_entry(conversation_id, draft).await
}

fn render_diagnostics(diagnostics: &[ToolDiagnostic]) -> String {
    format!(
        "<harness>\n{}\n</harness>",
        diagnostics
            .iter()
            .map(|diagnostic| format!(
                "[{}] {}",
                severity_str(diagnostic.severity),
                diagnostic.message
            ))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn severity_str(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Info => "info",
        DiagnosticSeverity::Warn => "warn",
        DiagnosticSeverity::Error => "error",
    }
}

struct BoundedContent {
    content: Vec<UserContentBlock>,
    dropped_bytes: u64,
    dropped_lines: u64,
}

/// 界限结果内容的文本。
///
/// 把所有文本块按顺序拼成一个字符串（不带分隔符）后调用 `bound_output`，用它的丢字节/行数
/// 作为诊断依据。没有丢弃时原样返回内容；发生丢弃时只保留保留端的一块文本（head 取第一块、
/// tail 取最后一块），图片块全部保留——文本通道之外的块不参与截断。
fn bound_content(content: &[UserContentBlock], limits: &ChordOutputLimits) -> BoundedContent {
    let texts: Vec<&TextContent> = content
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(text) => Some(text),
            UserContentBlock::Image(_) => None,
        })
        .collect();
    let joined = texts
        .iter()
        .map(|text| text.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    let bounded = bound_output(&joined, limits);
    if bounded.dropped_bytes == 0 {
        return BoundedContent {
            content: content.to_vec(),
            dropped_bytes: 0,
            dropped_lines: 0,
        };
    }
    let keep = match limits.retain {
        Retain::Head => texts.first().copied(),
        Retain::Tail => texts.last().copied(),
    };
    let result: Vec<UserContentBlock> = content
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Image(_) => Some(item.clone()),
            UserContentBlock::Text(text) => {
                if keep.is_some_and(|keep| std::ptr::eq(keep, text)) {
                    Some(UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: bounded.text.clone(),
                        text_signature: None,
                    }))
                } else {
                    None
                }
            }
        })
        .collect();
    BoundedContent {
        content: result,
        dropped_bytes: bounded.dropped_bytes as u64,
        dropped_lines: bounded.dropped_lines as u64,
    }
}

/// 程序拿到工具输出的什么。
///
/// 空输出是空字符串；多块输出序列化为内容块数组；单块文本是字符串；单块图片是图片对象。
/// 只在嵌套调用且工具没有 `structured_output_schema` 时使用（见 `final_result`）。
fn output_value(output: &[UserContentBlock]) -> JsonValue {
    if output.is_empty() {
        return JsonValue::String(String::new());
    }
    if output.len() > 1 {
        return serde_json::to_value(output).unwrap_or(JsonValue::Null);
    }
    match &output[0] {
        UserContentBlock::Text(text) => JsonValue::String(text.text.clone()),
        UserContentBlock::Image(image) => serde_json::to_value(image).unwrap_or(JsonValue::Null),
    }
}

/// 显式嵌套调用键是调用 ID 的路径段。
///
/// 允许任意非空字符串，但拒绝三类：
/// - 含 `/`：key 会拼进调用 ID `<parentCallId>/<key>`，也会作为文档家族键的路径段；
/// - `__proto__`：与 TS 的同名规则一致；
/// - 正整数（不以 `0` 开头且全为 ASCII 数字）：自动 key 就是 `sequence += 1` 的十进制形式，
///   两者共用同一命名空间。因此 `"0"`、`"00"`、`"-1"`、`" 1"`、`"0x1"` 都是合法 key
///   （见本文件单测 `nested_call_keys_follow_the_ts_rules`）。
///
/// 错误消息里的 key 用 `JSON.stringify` 的形式加引号（`kiana_json::js_object::stringify`）；
/// key 可能含引号/反斜杠，直接拼接会产生歧义，测试也断言了这段文本。
pub fn check_key(key: &str) -> Result<(), BoxError> {
    let bytes = key.as_bytes();
    let positive_integer = !bytes.is_empty()
        && bytes[0] != b'0'
        && bytes[0].is_ascii_digit()
        && bytes.iter().all(u8::is_ascii_digit);
    if key.is_empty() || key.contains('/') || key == "__proto__" || positive_integer {
        return Err(format!(
            "Nested call key {} must be non-empty, without \"/\", not \"__proto__\", and not a positive integer",
            kiana_json::js_object::stringify(&JsonValue::String(key.to_owned()))
        )
        .into());
    }
    Ok(())
}

/// 结构 JSON 相等；对象键序无关。
///
/// 与 JS 的 `===` 一致：数字按数值比较（`1 === 1.0`），其它标量按值，数组逐项、对象按键集合逐项。
/// `serde_json` 的结构相等会把 `1` 与 `1.0` 判为不等，TS 不会。
///
/// 实现委托给 `crate::harness::json::js_equal`。调用点用它判断“嵌套槽展示的参数是否在
/// repair/hook/校验强制转换后与输入不同”与“同一 key 的旧调用是否同名同参”。
pub fn json_equal(a: &JsonValue, b: &JsonValue) -> bool {
    crate::harness::json::js_equal(a, b)
}

/// `JsonObject`（`IndexMap`）转成 `serde_json::Value`。
fn json_value(object: &JsonObject) -> JsonValue {
    serde_json::to_value(object).unwrap_or(JsonValue::Null)
}

// ---------------------------------------------------------------------------
// ToolExecutionApi
// ---------------------------------------------------------------------------

/// 工具实现的运行期门面：报告输出/诊断/details、发起嵌套调用、读写会话与任务。
///
/// `ended` 在 `run` 收尾时置 true；此后 `output`/`diagnostic` 拒绝新内容并把错误记进
/// `violation`，`execute_tool`/`details` 直接返回错误，`retained_output` 仍返回快照但会记
/// 违规。异步的 `commit`/`memo`/`get_task` 等不做这个检查。
struct ToolApi<'a> {
    runtime: &'a dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    call: ToolCall,
    reported: Arc<Mutex<Reported>>,
    limits: ChordOutputLimits,
    ended: Mutex<bool>,
    env: Mutex<Option<Arc<dyn crate::env::ExecutionEnv>>>,
    sequence: Mutex<u64>,
    resumes: bool,
    progress: Arc<Progress>,
    /// `output()` 里被同步吞掉的契约错误（head 保留收到 `skipped`）；下次结算时当作抛出。
    violation: Mutex<Option<String>>,
    /// 本次调用发起的嵌套调用 admission（TS: `run()` 的 `admissions` 数组）。
    admissions: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// 一次嵌套调用的 admission：TS 的 `executeTool()` 在调用时同步登记 promise，`run()` 在结算前
/// `await Promise.allSettled(admissions)`。Rust 的 `async fn` 要等首次 poll 才跑前缀，因此
/// `execute_tool()` 是普通方法（`self: Arc<Self>`）：它在方法体里同步完成 `assertLive()`、key
/// 分配与登记，并把工作 `tokio::spawn` 成独立任务；返回的 future 只等结果。工具不 await、
/// 直接丢弃 future 时任务照常完成，[`run`] 在结算前等全部 admission 结束。
#[derive(Clone)]
struct AdmissionJob {
    name: String,
    args: JsonObject,
    key: String,
    progress: Option<bool>,
    context: kiana_chord::context::ContextRef,
}

/// admission 的工作体（TS 的 `admission` promise：前置 memo + `startNestedCall()`）。
///
/// TS 里 `admissions` 数组只装到这个 promise——它解析出新建的嵌套任务 ID；随后的
/// `waitForTask`/结果读取属于 `executeTool()` 自己的返回 promise，不计入父调用的
/// `Promise.allSettled(admissions)`。这样才能在父调用返回时中止一个仍挂着的嵌套任务。
async fn admit_nested(api: &ToolApi<'static>, job: &AdmissionJob) -> Result<TaskId, BoxError> {
    // 先写备忘（memo_put 只在不存在的时写），再创建嵌套任务：即使创建失败/进程重启，
    // 结算时也能从备忘得知本任务尝试过嵌套调用。
    api.runtime
        .memo_put(MADE_NESTED_CALLS, JsonValue::Bool(true), &*job.context)
        .await?;
    start_nested_call(
        api.runtime,
        &api.call.id,
        &job.name,
        &job.args,
        &job.key,
        job.progress,
        !api.resumes,
        &*job.context,
    )
    .await
}

/// 等嵌套任务结束并读出结果（TS `executeTool()` 里 `await admission` 之后的部分）。
///
/// 正常路径读调用者文档里的 `result`（嵌套任务结算时写入）；任务 faulted/orphaned 时没有
/// 结果，退回 [`fallback_result`]，用终态 status 作为诊断码。
async fn finish_nested_call(
    api: &ToolApi<'static>,
    job: &AdmissionJob,
    id: TaskId,
) -> Result<NestedToolExecutionResult, BoxError> {
    let settled = api.runtime.wait_for_task(id, &*job.context).await?;
    let parent = id_from_number(api.runtime.task_id().get());
    let family = DocTarget::task_family(&NESTED_CALL_DOC, parent, job.key.clone(), JsonValue::Null);
    let stored = api
        .runtime
        .snapshot(&family, &*job.context)
        .await?
        .and_then(|value| value.get("result").cloned());
    match stored {
        Some(stored) => Ok(serde_json::from_value(stored)?),
        None => Ok(fallback_result(id, &job.name, settled.outcome())),
    }
}

impl ToolApi<'_> {
    // 工具在结算之后继续报告属于契约违规：同步入口把错误记进 violation，异步入口直接返回。
    fn assert_live(&self) -> Result<(), BoxError> {
        if *lock(&self.ended) {
            return Err(format!("Tool call {} has settled", self.call.id).into());
        }
        Ok(())
    }

    /// 同步方法（`output`/`diagnostic`/`retainedOutput`）无法返回错误：记下 TS 会抛出的错误，
    /// 由 [`run`] 在结算前当作 `execute()` 抛错处理。
    fn record_violation(&self, error: BoxError) {
        let mut violation = lock(&self.violation);
        if violation.is_none() {
            *violation = Some(error.to_string());
        }
    }

    /// 等本次调用的全部 admission 结束（TS: `await Promise.allSettled(admissions)`）。
    async fn settle_admissions(&self) {
        let handles: Vec<tokio::task::JoinHandle<()>> = {
            let mut admissions = lock(&self.admissions);
            std::mem::take(&mut *admissions)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// abort 路径：TS 不等待 admission（随调用一起放弃）；这里先取消再等它们结束，避免被
    /// 取消的任务在被借用（transmute 成 `'static`）的 runtime 生命周期之后继续运行。
    async fn abort_admissions(&self) {
        let handles: Vec<tokio::task::JoinHandle<()>> = {
            let mut admissions = lock(&self.admissions);
            std::mem::take(&mut *admissions)
        };
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
    }
}

#[async_trait]
impl DocumentReader for ToolApi<'_> {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot(self.runtime, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot_as_of(self.runtime, target, at, context).await
    }
}

#[async_trait]
impl DocumentObserver for ToolApi<'_> {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<crate::types::DocumentWatch>, BoxError> {
        DocumentObserver::watch_doc(self.runtime, target, context).await
    }
}

#[async_trait]
impl ToolExecutionApi for ToolApi<'static> {
    fn task_id(&self) -> TaskId {
        id_from_number(self.runtime.task_id().get())
    }

    fn conversation_id(&self) -> ConversationId {
        self.runtime.conversation_id()
    }

    fn call_id(&self) -> &str {
        &self.call.id
    }

    fn registry(&self) -> Arc<dyn crate::harness::types::RegistrySnapshot> {
        self.runtime.registry()
    }

    async fn agent(
        &self,
        context: &dyn Context,
    ) -> Result<Arc<crate::harness::types::Agent>, BoxError> {
        self.runtime.agent(context).await
    }

    fn models(&self) -> Arc<dyn kiana_ai::models::Models> {
        self.runtime.models()
    }

    fn env(&self) -> Option<Arc<dyn crate::env::ExecutionEnv>> {
        lock(&self.env).clone()
    }

    fn now(&self) -> i64 {
        self.runtime.now()
    }

    fn output(&self, chunk: ToolOutput, skipped: Option<ShellOutputSkip>) {
        if let Err(error) = self.assert_live() {
            self.record_violation(error);
            return;
        }
        // 被省略的输出按解码后的文本计量；只有 tail 保留接受它。
        // `push` 返回是否计入了内容：不带 skip 的空块返回 false，不触发进度提交；head 保留
        // 收到 skip 会返回错误，按契约违规记录（结算时表现为 execute() 抛错）。
        let skipped = skipped.map(|skipped| OutputSkip {
            bytes: skipped.bytes,
            newlines: skipped.newlines,
            ends_with_newline: skipped.ends_with_newline,
        });
        let pushed = {
            let mut reported = lock(&self.reported);
            match &chunk {
                ToolOutput::Text(text) => reported.output.push(OutputChunk::Text(text), skipped),
                ToolOutput::Bytes(bytes) => {
                    reported.output.push(OutputChunk::Bytes(bytes), skipped)
                }
            }
        };
        match pushed {
            Ok(true) => self.progress.mark(),
            Ok(false) => {}
            Err(error) => {
                self.record_violation(Box::new(error));
            }
        }
    }

    fn output_window(&self) -> Option<ShellOutputWindow> {
        if self.limits.retain != Retain::Tail {
            return None;
        }
        // TS 的 `outputWindow` getter 读 `runtime.settings`：宿主抛错在工具代码里表现为 throw。
        // 同步方法没有错误通道，沿用 [`ToolApi::record_violation`]（结算时当作 `execute()` 抛错）。
        let settings = match self.runtime.settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.record_violation(error);
                return None;
            }
        };
        Some(ShellOutputWindow {
            max_bytes: self.limits.max_bytes,
            max_lines: self.limits.max_lines,
            min_interval_ms: settings.progress.output_interval_ms,
            bytes_per_second: kiana_chord::output::PROGRESS_BYTES_PER_SECOND,
        })
    }

    // 读当前窗口（tail 保留会触发压缩），不提交、不推进进度；`truncated` 表示已有输出被
    // 丢弃（Retain::Tail 时是前部，Retain::Head 时是后部）。
    fn retained_output(&self) -> RetainedOutput {
        if let Err(error) = self.assert_live() {
            self.record_violation(error);
        }
        let bounded = lock(&self.reported).output.snapshot();
        RetainedOutput {
            truncated: bounded.dropped_bytes > 0,
            text: bounded.text,
        }
    }

    fn diagnostic(&self, diagnostic: ToolDiagnostic) {
        if let Err(error) = self.assert_live() {
            self.record_violation(error);
            return;
        }
        lock(&self.reported).diagnostics.push(diagnostic);
        self.progress.mark();
    }

    async fn details(&self, value: JsonValue, context: &dyn Context) -> Result<(), BoxError> {
        self.assert_live()?;
        // 写之前先检查取消：已取消的调用不再推进持久状态。
        if let Some(signal) = context.abort_signal()
            && signal.is_aborted()
        {
            return Err(Box::new(abort_error(&signal)));
        }
        lock(&self.reported).details = Some(value);
        // 等待包含这次变化的进度提交；取消等待留下更新本身，提交自己的结局仍被观察。
        let committed = self.progress.mark_and_wait();
        match await_with_context(committed, context).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(failure)) => Err(Box::new(ProgressError(failure.to_string()))),
            Err(error) => Err(Box::new(error)),
        }
    }

    async fn commit(
        &self,
        change: &dyn crate::types::SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let wrapper = ApiCommit {
            change,
            result: Mutex::new(None),
        };
        // `TaskRuntime::commit` 返回 `()`，change 的返回值只能由包装对象在回调里存下、
        // 提交结束后再取出；调用方拿到 `Box::new(())` 表示 change 没有返回值。
        self.runtime.commit(&wrapper, context, options).await?;
        let result = lock(&wrapper.result).take();
        Ok(result.unwrap_or_else(|| Box::new(())))
    }

    async fn memo(&self, name: &str, context: &dyn Context) -> Result<Option<JsonValue>, BoxError> {
        self.runtime.memo(name, context).await
    }

    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        self.runtime.memo_put(name, candidate, context).await
    }

    async fn create_task(
        &self,
        task: &AnyTask,
        input: JsonValue,
        options: TaskOptions,
        context: &dyn Context,
    ) -> Result<TaskId, BoxError> {
        let mut options = options;
        // TS: `{ abandonOnRestart: true, ...options }`——调用方显式给的值优先。
        // `resumes` 在本文件里就是 `tool.replay == Some(ReplayPolicy::Safe)`（见 `run` 的构造）；
        // 非 Safe 工具创建 Task 所有权的子任务时，若调用方没显式指定，默认在重启时放弃。
        if !self.resumes
            && matches!(options.ownership, TaskOwnership::Task { .. })
            && options.abandon_on_restart.is_none()
        {
            options.abandon_on_restart = Some(true);
        }
        let change = ApiCreateTask {
            task: task.clone(),
            input,
            options,
            created: Mutex::new(None),
        };
        self.runtime
            .commit(&change, context, CommitOptions::default())
            .await?;
        (*lock(&change.created)).ok_or_else(|| "createTask() did not commit".into())
    }

    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::TaskRecord>, BoxError> {
        self.runtime.get_task(id, context).await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<crate::harness::types::SettledTask, BoxError> {
        self.runtime.wait_for_task(id, context).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn crate::harness::types::ConversationHandle>>, BoxError> {
        self.runtime.conversation(id, context).await
    }

    fn execute_tool(
        self: Arc<Self>,
        name: &str,
        args: JsonObject,
        context: &dyn Context,
        options: NestedToolOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<NestedToolExecutionResult, BoxError>>
                + Send
                + 'static,
        >,
    > {
        // TS 的 `executeTool()` 前缀在调用时同步执行：`assertLive()`、key 分配、admission 登记。
        let admission = self.begin_admission(name, args, context, options);
        let api = Arc::clone(&self);
        Box::pin(async move {
            let (receiver, job) = match admission {
                Ok(admission) => admission,
                Err(error) => return Err(error),
            };
            // oneshot 关闭（发送端被丢弃）说明 admission 任务没能送出自己的结果；正常路径
            // 总能拿到 `Result<TaskId, _>`。拿到 ID 后再等嵌套任务终态并读结果。
            let id = match receiver.await {
                Ok(Ok(id)) => id,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(format!(
                        "Nested call {}/{} admission ended without a result",
                        api.call.id, job.key
                    )
                    .into());
                }
            };
            finish_nested_call(&api, &job, id).await
        })
    }
}

impl ToolApi<'static> {
    /// `execute_tool()` 的同步前缀：校验、分配 key、spawn admission。
    ///
    /// 显式 key 先过 `check_key`；自动 key 是 `sequence += 1` 的十进制字符串，从 `"1"` 开始。
    /// spawn 出的任务体是 `admit_nested`，结果经 oneshot 交回 `execute_tool` 返回的 future；
    /// 句柄同时登记进 `admissions`，供结算等待或 abort 取消。
    fn begin_admission(
        self: &Arc<Self>,
        name: &str,
        args: JsonObject,
        context: &dyn Context,
        options: NestedToolOptions,
    ) -> Result<
        (
            tokio::sync::oneshot::Receiver<Result<TaskId, BoxError>>,
            AdmissionJob,
        ),
        BoxError,
    > {
        self.assert_live()?;
        let key = match &options.key {
            Some(key) => {
                check_key(key)?;
                key.clone()
            }
            None => {
                let mut sequence = lock(&self.sequence);
                *sequence += 1;
                sequence.to_string()
            }
        };
        let job = AdmissionJob {
            name: name.to_owned(),
            args,
            key,
            progress: options.progress,
            context: crate::session::context_handle(context),
        };
        let api = Arc::clone(self);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let job_for_admission = job.clone();
        let handle = tokio::spawn(async move {
            let result = admit_nested(&api, &job_for_admission).await;
            let _ = sender.send(result);
        });
        lock(&self.admissions).push(handle);
        Ok((receiver, job))
    }
}

// 把 `api.commit()` 的 change 返回值经由提交回调带回来的包装器：`TaskRuntimeChange`
// 只能返回新状态，返回值先存进 `result`，`commit` 结束后取出。
struct ApiCommit<'a> {
    change: &'a dyn crate::types::SessionChange,
    result: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

#[async_trait]
impl crate::harness::types::TaskRuntimeChange for ApiCommit<'_> {
    async fn run(
        &self,
        tx: &dyn Tx,
        _current: &crate::types::TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        // TS 的 `api.commit()` 返回 change 的返回值；这里把它从提交里带回来。
        let result = self.change.run(tx).await?;
        *lock(&self.result) = Some(result);
        Ok(None)
    }
}

// 把 `Tx::create_task` 放进一次提交执行，并把新任务 ID 带回来给 `api.create_task`。
struct ApiCreateTask {
    task: AnyTask,
    input: JsonValue,
    options: TaskOptions,
    created: Mutex<Option<TaskId>>,
}

#[async_trait]
impl crate::harness::types::TaskRuntimeChange for ApiCreateTask {
    async fn run(
        &self,
        tx: &dyn Tx,
        _current: &crate::types::TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        let id = tx
            .create_task(&self.task, self.input.clone(), self.options.clone())
            .await?;
        *lock(&self.created) = Some(id);
        Ok(None)
    }
}

/// 任务终态没有留下结果时的兜底结果。
///
/// 诊断码直接取终态 status 字面量（`outcome_status`），消息按 faulted / orphaned / 其它
/// 三种情形区分。只会在嵌套任务没走到结算时用到；结构化输出、usage、耗时都没有可信值。
fn fallback_result(
    task_id: TaskId,
    name: &str,
    outcome: &TaskOutcome<JsonValue>,
) -> NestedToolExecutionResult {
    // TS: 诊断 code 就是终结状态的 `status`（completed/failed/aborted/orphaned/faulted）。
    let message = match outcome {
        TaskOutcome::Faulted { error } => format!("Tool {name} failed: {}", error.message),
        TaskOutcome::Orphaned { reason } => format!("Tool {name} could not resume: {reason}"),
        _ => format!("Tool {name} ended without a result"),
    };
    let code = crate::harness::live::outcome_status(outcome);
    let error = harness_error(code, &message);
    NestedToolExecutionResult {
        task_id,
        structured_output: None,
        is_error: true,
        details: None,
        diagnostics: error.diagnostics.unwrap_or_default(),
        usage: None,
        duration_ms: None,
    }
}

/// 在一次提交里创建嵌套调用 `key`：它的工具任务、槽位与 `NestedCallDoc`。
#[allow(clippy::too_many_arguments)]
async fn start_nested_call(
    runtime: &dyn crate::harness::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ToolHookTable>,
    parent_call_id: &str,
    name: &str,
    args: &JsonObject,
    key: &str,
    progress: Option<bool>,
    abandon_on_restart: bool,
    context: &dyn Context,
) -> Result<TaskId, BoxError> {
    // 调用 ID 是 `<父调用 ID>/<key>`；它同时用于槽位、文档与子任务输入，所以 key 不能含 `/`。
    let call = ToolCall {
        kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
        id: format!("{parent_call_id}/{key}"),
        name: name.to_owned(),
        arguments: args.clone(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    };
    let task_id = id_from_number(runtime.task_id().get());
    let conversation_id = runtime.conversation_id();
    let input = ToolTaskInput::Nested {
        parent: task_id,
        parent_call_id: parent_call_id.to_owned(),
        key: key.to_owned(),
        call: call.clone(),
        progress,
    };
    let key = key.to_owned();
    let call_for_commit = call.clone();
    let created: Arc<Mutex<Option<TaskId>>> = Arc::new(Mutex::new(None));
    let created_for_commit = Arc::clone(&created);
    let replayed = Arc::new(Mutex::new(false));
    let replayed_for_commit = Arc::clone(&replayed);
    let parent_call_id = parent_call_id.to_owned();
    let change = TaskCommit::new(move |tx, _current| {
        let created = Arc::clone(&created_for_commit);
        let replayed = Arc::clone(&replayed_for_commit);
        let call = call_for_commit.clone();
        let input = input.clone();
        let key = key.clone();
        let parent_call_id = parent_call_id.clone();
        Box::pin(async move {
            let family =
                DocTarget::task_family(&NESTED_CALL_DOC, task_id, key.clone(), JsonValue::Null);
            let mut state = tx.doc(&family).await?;
            // 文档里已经有 taskId：这个 key 之前用过。只有同名同参的调用才允许复用那个
            // 任务（重放）；否则说明调用者在用同一个 key 表达另一个调用，直接报错。
            if let Some(existing) = state.value().get("taskId").and_then(JsonValue::as_i64) {
                let existing: TaskId = id_from_number(existing);
                let record = tx.task(existing).await?;
                let same = record
                    .and_then(|record| serde_json::from_value::<ToolTaskInput>(record.input).ok())
                    .map(|input| match input {
                        ToolTaskInput::Nested { call: prior, .. } => {
                            prior.name == call.name
                                && json_equal(
                                    &json_value(&prior.arguments),
                                    &json_value(&call.arguments),
                                )
                        }
                        ToolTaskInput::Model { .. } => false,
                    })
                    .unwrap_or(false);
                if !same {
                    return Err(format!(
                        "Nested call {} was already made with another tool or other arguments",
                        call.id
                    )
                    .into());
                }
                *lock(&created) = Some(existing);
                // TS: `replayed = state.result !== undefined`——这次重放已存结果，不重跑。
                *lock(&replayed) = state.value().get("result").is_some();
                return Ok(None);
            }
            let mut options = TaskOptions::new(TaskOwnership::Task { task_id });
            options.abandon_on_restart = Some(abandon_on_restart);
            // 三件事必须在同一个提交里：创建子任务、把 taskId 写进嵌套调用文档、把槽位追加
            // 进 live.nestedTools。任何一步失败，另外两步也不会留下（提交整体回滚）。
            let child = tx
                .create_task(&TOOL_TASK, serde_json::to_value(&input)?, options)
                .await?;
            state
                .value_mut()
                .insert("taskId".to_owned(), JsonValue::from(child.get()));
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            let slot = crate::harness::live::NestedToolSlot {
                call_id: call.id.clone(),
                parent_call_id: parent_call_id.clone(),
                parent_task_id: task_id,
                task_id: child,
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                status: SlotStatus::Pending,
                output: None,
                dropped_bytes: None,
                dropped_lines: None,
                details: None,
                diagnostics: None,
                summary: None,
            };
            nested_tools_mut(live.value_mut()).push(serde_json::to_value(slot)?);
            *lock(&created) = Some(child);
            Ok(None)
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await?;
    let created = (*lock(&created)).ok_or("Nested call was not created")?;
    if *lock(&replayed) {
        let mut attributes = kiana_telemetry::SpanAttributes::new();
        attributes.insert(
            "tool.name".to_owned(),
            kiana_telemetry::AttributeValue::String(name.to_owned()),
        );
        context
            .telemetry()
            .add_event("kiana.durable.replay", Some(attributes));
    }
    Ok(created)
}

// ---------------------------------------------------------------------------
// Task definition
// ---------------------------------------------------------------------------

/// 内建工具任务定义（TS: `ToolTask`）。
///
/// 阶段表按 `IndexMap` 插入顺序声明：`call` 与 `execute`；调度只看 checkpoint 里的
/// `phase` 名字，所以两个阶段都必须用这两个名字。`abort` 是 [`AbortPhase`]。
///
/// 版本 2 的迁移把旧输入当模型调用：先放 `kind: "model"`，再展开旧输入（旧字段可覆盖判别值），
/// 与 TS 的 `{ kind: "model", ...input }` 一致；checkpoint 原样保留。
pub fn tool_task() -> crate::types::Task<JsonValue, JsonValue, JsonValue, ToolHookTable> {
    let mut phases: indexmap::IndexMap<
        String,
        Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ToolHookTable>>,
    > = indexmap::IndexMap::new();
    phases.insert("call".to_owned(), Arc::new(CallPhase));
    phases.insert("execute".to_owned(), Arc::new(ExecutePhase));
    // 迁移在调度器发现记录版本小于当前版本时调用；`_from` 是旧版本号，这里不需要区分。
    let migrate: crate::types::TaskMigrate<JsonValue, JsonValue> =
        Arc::new(|input: JsonValue, checkpoint: JsonValue, _from: i64| {
            // TS: `{ kind: "model", ...input }`——kind 在前，输入里的同名字段覆盖它。
            let mut object = serde_json::Map::new();
            object.insert("kind".to_owned(), JsonValue::String("model".to_owned()));
            if let JsonValue::Object(map) = input {
                object.extend(map);
            }
            Ok((JsonValue::Object(object), checkpoint))
        });
    // 初始 checkpoint 是 `{ phase: "call" }`：新任务从 call 阶段开始。
    crate::types::tasks::define_task(TaskDefinition {
        name: TOOL_TASK_KIND.to_owned(),
        version: 2,
        initial: Arc::new(|_input| serde_json::json!({ "phase": "call" })),
        phases,
        abort: Arc::new(AbortPhase),
        migrate: Some(migrate),
        hooks: None,
    })
}

/// 擦除后的内建工具任务（注册表用）。
///
/// `tool_task()` 的类型化定义经 `erase_task_definition` 擦除成 `AnyTask`，由
/// `harness::registry` 注册；generation 创建工具任务时用的也是它。`LazyLock` 只构造一次。
pub static TOOL_TASK: LazyLock<AnyTask> =
    LazyLock::new(|| crate::harness::live::erase_task_definition(&tool_task().definition));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskOutcomeError;

    #[test]
    fn json_equal_matches_js_number_semantics() {
        assert!(json_equal(&serde_json::json!(1), &serde_json::json!(1.0)));
        assert!(json_equal(
            &serde_json::json!({ "a": [1, { "b": 2.0 }] }),
            &serde_json::json!({ "a": [1.0, { "b": 2 }] })
        ));
        assert!(!json_equal(
            &serde_json::json!([1]),
            &serde_json::json!([2])
        ));
        assert!(!json_equal(
            &serde_json::json!({ "a": 1 }),
            &serde_json::json!({ "a": 1, "b": 2 })
        ));
    }

    #[test]
    fn utf16_suffix_slices_by_code_units() {
        assert_eq!(utf16_suffix("hello", 0), "hello");
        assert_eq!(utf16_suffix("hello", 2), "llo");
        assert_eq!(utf16_suffix("hello", 5), "");
        assert_eq!(utf16_suffix("hello", 9), "");
        // 一个 emoji 占两个 UTF-16 code unit：从 2 起是它之后的文本。
        assert_eq!(utf16_suffix("😀a", 2), "a");
        // 落在代理对中间时取整字符（见函数注释）。
        assert_eq!(utf16_suffix("😀a", 1), "a");
    }

    #[test]
    fn nested_call_keys_follow_the_ts_rules() {
        // TS `checkKey`。
        for allowed in ["0", "00", "a", "a.b", "call-1", "0x1", "-1", " 1"] {
            assert!(check_key(allowed).is_ok(), "{allowed} is a valid key");
        }
        for rejected in ["", "/", "a/b", "__proto__", "1", "42", "9007199254740993"] {
            let error = check_key(rejected).expect_err(rejected);
            assert_eq!(
                error.to_string(),
                format!(
                    "Nested call key {} must be non-empty, without \"/\", not \"__proto__\", and not a positive integer",
                    kiana_json::js_object::stringify(&JsonValue::String(rejected.to_owned()))
                ),
                "{rejected}"
            );
        }
        // JS 的 `JSON.stringify` 字符串形式（引号与转义）。
        let message = check_key("a\"/b")
            .expect_err("quotes are serialized")
            .to_string();
        assert!(message.contains("\"a\\\"/b\""), "{message}");
    }

    fn schema_tool(schema: Option<JsonValue>) -> ToolRegistration {
        let mut registration = ToolRegistration::new(kiana_ai::types::Tool {
            name: "echo".to_owned(),
            description: String::new(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        });
        registration.structured_output_schema = schema;
        registration
    }

    fn result_with(structured: Option<JsonValue>, is_error: Option<bool>) -> ToolExecutionResult {
        ToolExecutionResult {
            output: Some(Vec::new()),
            structured_output: structured,
            is_error,
            details: None,
            diagnostics: None,
            usage: None,
            control: None,
        }
    }

    #[test]
    fn structured_output_errors_match_the_ts_text() {
        // 没有 schema 却返回 structuredOutput。
        let error = structured_output_error(
            &schema_tool(None),
            &result_with(Some(serde_json::json!(1)), None),
        );
        assert_eq!(
            error.as_deref(),
            Some("Tool echo returned structuredOutput but declares no structuredOutputSchema")
        );
        assert_eq!(
            structured_output_error(&schema_tool(None), &result_with(None, None)),
            None
        );

        // 有 schema 却没有 structuredOutput。
        let schema = Some(serde_json::json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "required": ["a"],
            "additionalProperties": false
        }));
        assert_eq!(
            structured_output_error(&schema_tool(schema.clone()), &result_with(None, None))
                .as_deref(),
            Some("Tool echo returned no structuredOutput")
        );
        // 错误结果可以没有 structuredOutput。
        assert_eq!(
            structured_output_error(&schema_tool(schema.clone()), &result_with(None, Some(true))),
            None
        );
        // 匹配的值没有错误。
        assert_eq!(
            structured_output_error(
                &schema_tool(schema.clone()),
                &result_with(Some(serde_json::json!({ "a": "x" })), None)
            ),
            None
        );
        // 不匹配：路径是 `instancePath` 去掉前导 `/` 后把 `/` 换成 `.`；根路径是 `root`。
        let error = structured_output_error(
            &schema_tool(schema.clone()),
            &result_with(Some(serde_json::json!({ "a": 1 })), None),
        )
        .expect("a mismatch is reported");
        assert!(
            error.starts_with(
                "Tool echo returned structuredOutput that does not match its schema: a: "
            ),
            "{error}"
        );
        let root = serde_json::json!({ "type": "string" });
        let error = structured_output_error(
            &schema_tool(Some(root)),
            &result_with(Some(serde_json::json!(1)), None),
        )
        .expect("a root mismatch is reported");
        assert!(
            error.starts_with(
                "Tool echo returned structuredOutput that does not match its schema: root: "
            ),
            "{error}"
        );
    }

    #[test]
    fn fallback_result_names_every_terminal_status() {
        let faulted = TaskOutcome::Faulted {
            error: TaskOutcomeError {
                message: "boom".to_owned(),
                detail: None,
            },
        };
        let result = fallback_result(id_from_number(1), "read", &faulted);
        assert!(result.is_error);
        assert_eq!(result.task_id, id_from_number(1));
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code.as_deref(), Some("faulted"));
        assert_eq!(result.diagnostics[0].message, "Tool read failed: boom");

        let orphaned = TaskOutcome::Orphaned {
            reason: "gone".to_owned(),
        };
        let result = fallback_result(id_from_number(2), "read", &orphaned);
        assert_eq!(result.diagnostics[0].code.as_deref(), Some("orphaned"));
        assert_eq!(
            result.diagnostics[0].message,
            "Tool read could not resume: gone"
        );

        let aborted: TaskOutcome = TaskOutcome::Aborted {
            reason: None,
            result: None,
        };
        let result = fallback_result(id_from_number(3), "read", &aborted);
        assert_eq!(result.diagnostics[0].code.as_deref(), Some("aborted"));
        assert_eq!(
            result.diagnostics[0].message,
            "Tool read ended without a result"
        );
        assert!(result.structured_output.is_none());
        assert!(result.usage.is_none());
        assert!(result.duration_ms.is_none());
    }
}
