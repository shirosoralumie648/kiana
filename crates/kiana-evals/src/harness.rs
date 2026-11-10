//! `evals/src/harness.ts`。
//!
//! 把 Kiana coding agent 包装成评测 harness：解析模型选择、构造隔离的 home/agent 目录、
//! 运行 prompt 序列、把 transcript/用量归一化成 `@vitest-evals/core` 的 `HarnessRun` 形状，
//! 以及文档变体（`without_docs` / `with_docs`）的 system prompt 处理。
//!
//! TS 侧依赖 `vitest-evals/harness` 与 `@vitest-evals/core`；Rust 没有对应 crate，
//! 因此本模块提供了**本地等价物**：
//!
//! | TS（vitest-evals） | 本模块 |
//! |---|---|
//! | `Harness<TInput, TOutput>` | [`Harness`] |
//! | `HarnessContext` | [`HarnessContext`] |
//! | `SimpleHarnessResult<TOutput>` | [`SimpleHarnessResult`] |
//! | `normalizeHarnessRun` | [`normalize_harness_run`] |
//! | `createHarness` 的收尾（artifact 合并、失败 run、trace） | `complete_harness_run` |
//! | `createFailedHarnessRun` / `ensureRunTrace` | `create_failed_harness_run` / `ensure_run_trace` |
//! | `attachHarnessRunToError` / `getHarnessRunFromError` | [`HarnessError::attach_run`] / [`HarnessError::run`] |
//!
//! `createHarness` 会在 run 结束后才读取 `context.artifacts`（所以 `runId` /
//! `kianaSessionJsonl` 这类运行期 artifact 能进入 [`HarnessRun`]），并为每个 run 补一条
//! 根 run span；这些语义都由 `complete_harness_run` 统一实现。
//!
//! 归一化后的 run 形状直接复用 [`crate::report`] 的 wire 类型（`HarnessRun` /
//! `NormalizedSession` / `UsageSummary` / `TimingSummary` / `TranscriptEvent`）。
//!
//! TS 源：`packages/evals/src/harness.ts`（537 行）。
//!
//! # 在系统中的位置
//!
//! 本模块在本 crate 内只依赖 [`crate::plan`] 与 [`crate::report`] 的类型；外部项目 crate
//! 只用到 `kiana-ai`（模型、凭证、中止、转录读取）与 `kiana-coding-agent` 的 SDK 导出面
//! （`AgentSession`、`SessionManager`、`ModelRuntime`、`create_agent_session_*`）。
//! crate 内没有其它模块调用它：`cli` / `docker` 走的是「容器里跑 vitest」的路线。
//! 本模块供「在进程内直接跑一个 Kiana coding agent」的评测使用，目前的实际调用方是集成
//! 测试 `crates/kiana-evals/tests/harness.rs` 与 `tests/support/configured_runtime.rs`
//! （后者只用 [`stop_reason_str`]）；`tests/report.rs` 只用到
//! [`crate::report::HarnessRun`] 类型，不经过本模块。
//!
//! # 一次 run 的执行流程
//!
//! 1. 构造期：[`create_kiana_coding_agent_harness`] 只读取 `options.name`、把选项放进
//!    `Arc`，返回闭包式 [`Harness`]；此时不读环境、不建目录。
//! 2. `run()` 先做「失败也不需要清理」的检查：中止信号、模型选择、沙箱身份
//!    （`KIANA_EVAL_SANDBOX_UID`/`_GID`）、`mkdtemp` 建临时根目录。
//! 3. [`apply_isolated_environment`] 改写进程环境（`HOME`/`USERPROFILE`/
//!    `KIANA_CODING_AGENT_DIR`），agent 的配置与会话因此落在临时目录。
//! 4. 在临时 `root` 下建 `workspace`、`home/.kiana/agent`、`sessions`，读取宿主凭证，
//!    构造 `ModelRuntime` 与 `AgentSession`，再按 step 依次 `prompt` / `reload`。
//! 5. 收集 transcript、用量、system prompt 摘要，做文档变体校验与 `output` 回调。
//! 6. 无论成功失败都走 `finally` 段：把会话 JSONL 快照写入 artifact、`dispose()` 会话、
//!    递归删除临时根目录、恢复进程环境，并把 cleanup 错误与运行错误合并。
//! 7. `complete_harness_run` 归一化成 [`HarnessRun`]（events 为空即失败）并补根 run span；
//!    失败时把 run 挂到 [`HarnessError`] 上（用 `error.run()` 取回）。
//!
//! # 重要约束（改动前先确认）
//!
//! - **进程环境是全局状态**：环境隔离、隐藏凭证、沙箱降权都直接改进程级状态，调用方必须
//!   保证没有并发读者（测试里用互斥锁串行化）。环境变量由 `finally` 恢复；沙箱路径下被
//!   删除的 `auth.json` 不会重建。
//! - **顺序有依赖**：沙箱身份、文档变体等 `KIANA_EVAL_*` 变量必须在
//!   [`apply_isolated_environment`] **之前**读取——它会把所有 `KIANA_EVAL_` 前缀的键删掉。
//! - **沙箱降权是单向的**：设置沙箱身份后，`enter_tool_sandbox` 会对整个进程执行
//!   `setgroups`/`setgid`/`setuid`，代码不再切回原身份。
//! - **错误文案是冻结契约**：本模块自己构造的 [`HarnessError`] 消息逐字对齐 Node 原文；
//!   来自 SDK 的错误（`error.to_string()` / `error.message()`）保持底层文本不变。文件系统
//!   错误还带 `errno`/`code`/`syscall`/`path` 自有属性，序列化时整体替代
//!   `{type, message}`。
//! - **失败要带 run**：vitest-evals 的失败 run 语义由 `complete_harness_run` 统一实现，
//!   新增失败路径时不要绕过它直接返回错误。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, SecondsFormat, Utc};
use futures::future::LocalBoxFuture;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::oauth::js_trim;
use kiana_ai::auth::types::{AuthOperationOptions, CredentialStore};
use kiana_ai::index::{content_text_assistant, content_text_user, content_text_user_content};
use kiana_ai::types::{
    AssistantContentBlock, Message, Model, ModelThinkingLevel, StopReason, UserContentBlock,
};
use kiana_ai::utils::abort::{AbortReason, AbortScope};
use kiana_ai::utils::transcript::get_current_system_prompt;
use kiana_coding_agent::sdk_exports::{
    AgentMessage, AgentSession, BeforeAgentStartEvent, BeforeAgentStartEventResult,
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
    CreateModelRuntimeOptions, DefaultResourceLoaderOptions, ExtensionEvent, ExtensionEventKind,
    ExtensionEventResult, ExtensionHandler, InlineExtension, ModelRuntime, NoTools, PromptOptions,
    SessionManager, ToolDefinition, convert_to_llm, create_agent_session_from_services,
    create_agent_session_services, get_agent_dir, read_stored_credential,
};

use crate::plan::DocumentationVariant;
use crate::report::{
    HarnessRun, JsNumber, KIANA_SESSION_SNAPSHOT_ARTIFACT, NormalizedError, NormalizedSession,
    NormalizedSpan, NormalizedTrace, SpanKind, SpanStatus, TimingSummary, TranscriptEvent,
    TranscriptRole, UsageSummary,
};

// ---------------------------------------------------------------------------
// 泛型 harness（vitest-evals/harness 的本地等价物）
// ---------------------------------------------------------------------------

/// `vitest-evals/harness#JsonValue`：harness 输出/artifact 可接受的 JSON 值。
///
/// Rust 侧直接用 `serde_json::Value`（`preserve_order` 已开启，保持插入键序）。
pub type JsonValue = Value;

/// `HarnessContext.artifacts` 的类型（字符串键 → JSON 值，保持插入序）。
pub type HarnessArtifacts = indexmap::IndexMap<String, JsonValue>;

/// 一次 harness run 的本地结果类型（`Harness["run"]` 的返回 future）。
///
/// TS: `vitest-evals/harness#Harness.run`
pub type HarnessRunFuture = LocalBoxFuture<'static, Result<HarnessRun, HarnessError>>;

/// 错误对象上携带的归一化 run。
///
/// TS: `vitest-evals/harness#attachHarnessRunToError`（把 `vitestEvalsRun` 挂到 Error 上）。
///
/// `Display` 只输出 `message`（把错误转成字符串时只暴露 `message`）；
/// `name` 只能通过 [`HarnessError::name`] 读取，并作为归一化错误里的 `type`。
/// 挂载 run 用 [`HarnessError::attach_run`]：它消费 `self` 返回新值，因为 Rust 不能像 JS
/// 那样就地给 Error 加属性。
#[derive(Clone, Debug)]
pub struct HarnessError {
    name: String,
    message: String,
    run: Option<Box<HarnessRun>>,
    causes: Vec<HarnessError>,
    /// Node `SystemError` 的自有可枚举属性（`errno` / `code` / `syscall` / `path`）。
    ///
    /// vitest-evals 的 `normalizeSimpleErrors` 先对错误跑 `toJsonValue`：普通 `Error` 没有
    /// 可枚举自有属性 → 回退到 `{type, message}`；Node 的 fs 错误则直接以这些属性序列化
    /// （不带 `type`/`message`）。`None` 表示按普通 `Error` 处理。装箱以保持
    /// `HarnessError` 足够小（clippy 的 `result_large_err`）。
    own_properties: Option<Box<Map<String, JsonValue>>>,
}

impl HarnessError {
    /// 普通 `Error`（`name = "Error"`）。
    pub fn new(message: impl Into<String>) -> Self {
        HarnessError {
            name: "Error".to_string(),
            message: message.into(),
            run: None,
            causes: Vec::new(),
            own_properties: None,
        }
    }

    /// `new TypeError(message)`（`name = "TypeError"`）。
    pub fn type_error(message: impl Into<String>) -> Self {
        HarnessError {
            name: "TypeError".to_string(),
            message: message.into(),
            run: None,
            causes: Vec::new(),
            own_properties: None,
        }
    }

    /// 带显式 `name` 的错误（`AbortError` 等）。
    pub fn with_name(name: impl Into<String>, message: impl Into<String>) -> Self {
        HarnessError {
            name: name.into(),
            message: message.into(),
            run: None,
            causes: Vec::new(),
            own_properties: None,
        }
    }

    /// `AggregateError(causes, message)`：TS 的 `error.name === "AggregateError"`，
    /// `error.errors` 对应 [`HarnessError::causes`]。
    pub fn aggregate(causes: Vec<HarnessError>, message: impl Into<String>) -> Self {
        HarnessError {
            name: "AggregateError".to_string(),
            message: message.into(),
            run: None,
            causes,
            own_properties: None,
        }
    }

    /// 附加 Node `SystemError` 的自有可枚举属性（序列化时替代 `{type, message}`）。
    fn with_own_properties(mut self, properties: Map<String, JsonValue>) -> Self {
        self.own_properties = Some(Box::new(properties));
        self
    }

    /// `error.name`。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `getHarnessRunFromError(error)`：错误上挂载的归一化 run。
    pub fn run(&self) -> Option<&HarnessRun> {
        self.run.as_deref()
    }

    /// `AggregateError.errors`（非聚合错误为空）。
    pub fn causes(&self) -> &[HarnessError] {
        &self.causes
    }

    /// `attachHarnessRunToError(error, run)`（返回新的错误对象）。
    pub fn attach_run(mut self, run: HarnessRun) -> Self {
        self.run = Some(Box::new(run));
        self
    }
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HarnessError {}

/// 执行体上下文（`HarnessContext`）。
///
/// TS 的 `artifacts` 是可直接改写的普通对象；Rust 用 [`HarnessContext::set_artifact`]
/// 暴露同一语义，并用 [`HarnessContext::artifacts`] 取快照。
///
/// `Clone` 是浅拷贝：所有 clone 共享同一张 artifact 表（`Arc<Mutex<...>>`），因此把 context
/// 交给执行体之后，`run` 期间写入的 artifact 仍能在 `complete_harness_run` 里看到。
/// 互斥锁中毒时用 `into_inner()` 继续，对应 TS 的单线程对象没有「中毒」状态。
#[derive(Clone, Default)]
pub struct HarnessContext {
    signal: Option<AbortScope>,
    artifacts: Arc<Mutex<HarnessArtifacts>>,
}

impl std::fmt::Debug for HarnessContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarnessContext")
            .field("signal", &self.signal)
            .finish_non_exhaustive()
    }
}

impl HarnessContext {
    /// 新建上下文；`signal` 对应 TS 的 `context.signal`（可缺省）。
    pub fn new(signal: Option<AbortScope>) -> Self {
        HarnessContext {
            signal,
            artifacts: Arc::new(Mutex::new(HarnessArtifacts::new())),
        }
    }

    /// `context.signal`。
    pub fn signal(&self) -> Option<&AbortScope> {
        self.signal.as_ref()
    }

    /// `context.setArtifact(name, value)`。
    pub fn set_artifact(&self, name: impl Into<String>, value: JsonValue) {
        self.artifacts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(name.into(), value);
    }

    /// `context.artifacts` 的快照（保持插入序）。
    pub fn artifacts(&self) -> HarnessArtifacts {
        self.artifacts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

/// 轻量结果（`SimpleHarnessResult<TOutput>`）。
///
/// TS 的字段：`output`、`events`、`usage`、`timings`、`artifacts`、`traces`、`metadata`、
/// `errors`。本 harness 只需要前五个中的 `output/events/usage/timings/metadata`
/// （`runKianaCodingAgent` 不产生 traces，artifacts 来自上下文，errors 走抛出路径）。
/// `output` 用 `Option` 表达 TS 的 `undefined`（失败路径上的 partial run 没有 output）。
pub struct SimpleHarnessResult<TOutput> {
    /// harness 输出（写入 `HarnessRun.output`；`None` 表示 `undefined`）。
    pub output: Option<TOutput>,
    /// 有序 transcript 事件。
    pub events: Vec<TranscriptEvent>,
    /// 使用量。
    pub usage: UsageSummary,
    /// 计时。
    pub timings: Option<TimingSummary>,
    /// 会话级元数据（写入 `session.metadata`）。
    pub metadata: Option<Map<String, JsonValue>>,
}

/// 通用 harness（`Harness<TInput, TOutput>`）。
///
/// `run` 的返回值是已归一化的 [`HarnessRun`]；`TOutput` 只用于类型层面的区分
/// （TS 的 `HarnessRun<TOutput>` 里 output 同样在运行时是任意 JSON 值）。
///
/// `run` 是普通闭包，本身不负责隔离与清理：这些在 `create_*` 构造出的执行体里，由
/// `run_kiana_coding_agent` + `complete_harness_run` 完成。
///
/// 返回的 [`HarnessRunFuture`] 是 `LocalBoxFuture`（不实现 `Send`），只能在 current-thread
/// 运行时或 `LocalSet` 上 await；测试用 `LocalSet::new().block_on(...)`。
pub struct Harness<Input, Output> {
    /// harness 名（报告里的 `harness.name`）。
    pub name: String,
    /// 执行体。
    pub run: Box<dyn Fn(Input, HarnessContext) -> HarnessRunFuture>,
    marker: std::marker::PhantomData<fn(Input) -> Output>,
}

impl<Input, Output> Harness<Input, Output> {
    /// 新建 harness。
    pub fn new<F>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn(Input, HarnessContext) -> HarnessRunFuture + 'static,
    {
        Harness {
            name: name.into(),
            run: Box::new(run),
            marker: std::marker::PhantomData,
        }
    }

    /// `harness.run(input, context)`。
    pub fn run(&self, input: Input, context: HarnessContext) -> HarnessRunFuture {
        (self.run)(input, context)
    }
}

/// `normalizeHarnessRun` 的本地等价物。
///
/// TS 在 `events` 为空时抛 `TypeError`；这里返回 [`HarnessError`]（消息逐字）。
/// `artifacts` 是 `context.artifacts` 的快照（非空时并入 run）。
///
/// 字段映射：`session` 的 `events/provider/model/metadata` 取自结果，`usage`、`timings`、
/// `output` 原样带入；`traces` 留空（随后由 `ensure_run_trace` 补根 span），`errors` 初始
/// 为空。`output: None` 序列化为缺省字段，对应 TS 的 `undefined`。
pub fn normalize_harness_run(
    result: SimpleHarnessResult<JsonValue>,
    artifacts: &HarnessArtifacts,
) -> Result<HarnessRun, HarnessError> {
    if result.events.is_empty() {
        return Err(HarnessError::type_error(
            "createHarness results must include at least one transcript event. Return ordered events or message transport inputs that normalize into events.",
        ));
    }
    let session = NormalizedSession {
        events: result.events,
        provider: result.usage.provider.clone(),
        model: result.usage.model.clone(),
        metadata: result.metadata,
    };
    Ok(HarnessRun {
        output: result.output,
        session,
        usage: result.usage,
        timings: result.timings,
        artifacts: artifacts_map(artifacts),
        traces: None,
        errors: Vec::new(),
    })
}

/// `serializeError(error)`：`{type: error.name, message: error.message}`。
fn serialize_error(error: &HarnessError) -> Map<String, JsonValue> {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(error.name().to_string()));
    map.insert(
        "message".to_string(),
        Value::String(error.message().to_string()),
    );
    map
}

/// `normalizeSimpleErrors` 对单个错误的处理：
/// `toJsonValue(error)` 产出非空对象时直接使用，否则回退到 [`serialize_error`]。
///
/// Node 的 fs `SystemError` 有可枚举的 `errno`/`code`/`syscall`/`path`，因此序列化结果
/// **没有** `type`/`message`；普通 `Error`/`TypeError`/`AggregateError` 则是 `{type, message}`。
fn normalize_simple_error(error: &HarnessError) -> Map<String, JsonValue> {
    match &error.own_properties {
        Some(properties) if !properties.is_empty() => (**properties).clone(),
        _ => serialize_error(error),
    }
}

/// 把 artifacts 快照转成 [`HarnessRun::artifacts`] 的 map（空 → `None`）。
fn artifacts_map(artifacts: &HarnessArtifacts) -> Option<Map<String, JsonValue>> {
    if artifacts.is_empty() {
        None
    } else {
        Some(
            artifacts
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }
}

/// `createFailedHarnessRun(input, error, {artifacts})`。
///
/// 早期失败（模型/沙箱/会话创建等，发生在诊断快照之前）没有可挂载的 partial run；
/// vitest-evals 的 `createHarness` 会合成一个只含输入 user message 的失败 run。
///
/// 注意这里用 [`serialize_error`] 写 `errors`（固定 `{type, message}`），而中途失败路径
/// 用的是 [`normalize_simple_error`]；因此带自有属性的文件系统错误在这条路径上会丢掉
/// `errno`/`code`/`syscall`/`path`。
fn create_failed_harness_run(
    input: &KianaCodingAgentInput,
    error: &HarnessError,
    artifacts: &HarnessArtifacts,
) -> HarnessRun {
    HarnessRun {
        output: None,
        session: NormalizedSession {
            events: vec![TranscriptEvent::Message {
                role: TranscriptRole::User,
                content: Some(input_to_json(input)),
                metadata: None,
            }],
            provider: None,
            model: None,
            metadata: None,
        },
        usage: UsageSummary::default(),
        timings: None,
        artifacts: artifacts_map(artifacts),
        traces: None,
        errors: vec![serialize_error(error)],
    }
}

/// `normalizeContent(input)`：harness 输入 → transcript 的 `content` 值。
///
/// `Prompt` 直接变成字符串；`Steps` 变成 `{type:"prompt", content}` / `{type:"reload"}`
/// 组成的数组。失败 run 的合成 transcript 用这个值，因此形状必须与
/// `normalizeContent` 一致。
fn input_to_json(input: &KianaCodingAgentInput) -> JsonValue {
    match input {
        KianaCodingAgentInput::Prompt(content) => Value::String(content.clone()),
        KianaCodingAgentInput::Steps(steps) => Value::Array(
            steps
                .iter()
                .map(|step| match step {
                    KianaCodingAgentStep::Prompt { content } => {
                        let mut map = Map::new();
                        map.insert("type".to_string(), Value::String("prompt".to_string()));
                        map.insert("content".to_string(), Value::String(content.clone()));
                        Value::Object(map)
                    }
                    KianaCodingAgentStep::Reload => {
                        let mut map = Map::new();
                        map.insert("type".to_string(), Value::String("reload".to_string()));
                        Value::Object(map)
                    }
                })
                .collect(),
        ),
    }
}

/// 生成 trace id 的全局计数器（`createGeneratedTraceId`）。
static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(0);

/// `createGeneratedTraceId()`：`trace_1`、`trace_2` ……
fn create_generated_trace_id() -> String {
    format!("trace_{}", NEXT_TRACE_ID.fetch_add(1, Ordering::SeqCst) + 1)
}

/// `Date#toISOString()` 的格式（毫秒固定 3 位、UTC、`Z` 后缀）。
fn format_iso_timestamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// JS `Date` 只有毫秒精度：先把时间截断到毫秒，保证 ISO 文本与 `durationMs` 一致。
fn truncate_to_millis(time: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(time.timestamp_millis()).unwrap_or(time)
}

/// `normalizeSpanError(error)`：run error 是 JSON 对象。
///
/// - `{message: string, type?: string, ...}` → 原样拆出 `message`/`type`，其余键进 `fields`；
/// - 没有字符串 `message` 的对象（例如 fs `SystemError` 序列化后的
///   `{errno, code, syscall, path}`）走 `serializeError` 回退：`String(error)` 是
///   `[object Object]`，`type` 是 `"Error"`（Node 26 实测）。
fn span_error_from_json(error: &Map<String, JsonValue>) -> NormalizedError {
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return NormalizedError {
            message: "[object Object]".to_string(),
            r#type: Some("Error".to_string()),
            fields: Map::new(),
        };
    };
    let r#type = error
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let fields = error
        .iter()
        .filter(|(key, _)| key.as_str() != "message" && key.as_str() != "type")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    NormalizedError {
        message: message.to_string(),
        r#type,
        fields,
    }
}

/// `ensureRunTrace(run, {name, startedAt, finishedAt})`。
///
/// 当 run 还没有任何 span 时，写入一条只含根 run span 的 trace（`run.traces` 会被整体
/// 替换；即使原 traces 里只有空 span 的条目也会被丢弃）。
fn ensure_run_trace(
    run: &mut HarnessRun,
    name: &str,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
) {
    let has_spans = run
        .traces
        .as_ref()
        .is_some_and(|traces| traces.iter().any(|trace| !trace.spans.is_empty()));
    if has_spans {
        return;
    }
    // 先截断到毫秒再算 duration：JS 的 Date 只有毫秒精度，这样 ISO 文本与 durationMs
    // 不会自相矛盾。
    let started_at = truncate_to_millis(started_at);
    let finished_at = truncate_to_millis(finished_at);
    let trace_id = create_generated_trace_id();
    let root_span_id = format!("{trace_id}:run");
    let started_at_text = format_iso_timestamp(started_at);
    let finished_at_text = format_iso_timestamp(finished_at);
    let duration_ms = (finished_at - started_at).num_milliseconds() as f64;
    // 根 span 的错误只取 run.errors 的第一条；没有错误时 status=ok。
    let root_error = run.errors.first().map(span_error_from_json);

    // gen_ai.* 是 vitest-evals 的 run span 约定属性；只写有值的字段（provider/model/token
    // 在失败 run 上可能缺席）。
    let mut attributes = Map::new();
    attributes.insert(
        "gen_ai.operation.name".to_string(),
        Value::String("invoke_workflow".to_string()),
    );
    attributes.insert(
        "gen_ai.workflow.name".to_string(),
        Value::String(name.to_string()),
    );
    if let Some(provider) = &run.usage.provider {
        attributes.insert(
            "gen_ai.provider.name".to_string(),
            Value::String(provider.clone()),
        );
    }
    if let Some(model) = &run.usage.model {
        attributes.insert(
            "gen_ai.request.model".to_string(),
            Value::String(model.clone()),
        );
        attributes.insert(
            "gen_ai.response.model".to_string(),
            Value::String(model.clone()),
        );
    }
    if let Some(input_tokens) = run.usage.input_tokens {
        attributes.insert(
            "gen_ai.usage.input_tokens".to_string(),
            js_number_value(input_tokens.0),
        );
    }
    if let Some(output_tokens) = run.usage.output_tokens {
        attributes.insert(
            "gen_ai.usage.output_tokens".to_string(),
            js_number_value(output_tokens.0),
        );
    }
    if let Some(reasoning_tokens) = run.usage.reasoning_tokens {
        attributes.insert(
            "gen_ai.usage.reasoning.output_tokens".to_string(),
            js_number_value(reasoning_tokens.0),
        );
    }

    let run_span = NormalizedSpan {
        id: Some(root_span_id),
        trace_id: Some(trace_id.clone()),
        parent_id: None,
        name: name.to_string(),
        kind: Some(SpanKind::Run),
        started_at: Some(started_at_text.clone()),
        finished_at: Some(finished_at_text.clone()),
        duration_ms: Some(JsNumber(duration_ms)),
        status: Some(if root_error.is_some() {
            SpanStatus::Error
        } else {
            SpanStatus::Ok
        }),
        error: root_error,
        attributes: Some(attributes),
        events: None,
    };
    run.traces = Some(vec![NormalizedTrace {
        id: Some(trace_id),
        name: Some(name.to_string()),
        started_at: Some(started_at_text),
        finished_at: Some(finished_at_text),
        duration_ms: Some(JsNumber(duration_ms)),
        metadata: None,
        spans: vec![run_span],
    }]);
}

/// `createHarness` 的收尾：归一化结果（或合成失败 run）、合并 live artifacts、补 trace。
///
/// TS 的 `createHarness` 在 `options.run(...)` 返回（或抛出）**之后**才读取
/// `context.artifacts`；因此 `runId` 与 `kianaSessionJsonl` 这类运行期 artifact 能被看到。
///
/// 三种结局的处理不同：
/// - 成功：归一化后补 trace；
/// - 失败且错误上已有 partial run（诊断快照之后才失败）：只补 artifacts（原本为空时）与
///   trace，保留原有的 events/usage；
/// - 失败且没有 run（诊断快照之前失败）：合成只含输入 user message 的失败 run。
/// 归一化本身失败（events 为空）也走最后一条，并把带 run 的错误返回。
fn complete_harness_run(
    name: &str,
    input: &KianaCodingAgentInput,
    context: &HarnessContext,
    started_at: DateTime<Utc>,
    outcome: Result<SimpleHarnessResult<JsonValue>, HarnessError>,
) -> Result<HarnessRun, HarnessError> {
    match outcome {
        Ok(result) => {
            // 先取 artifact 快照再归一化：快照包含 run 期间写入的键。
            let artifacts = context.artifacts();
            match normalize_harness_run(result, &artifacts) {
                Ok(mut run) => {
                    // TS 在归一化之后才取 `finishedAt`。
                    ensure_run_trace(&mut run, name, started_at, Utc::now());
                    Ok(run)
                }
                Err(error) => {
                    // TS 里 `normalizeHarnessRun` 的 TypeError 也会被 createHarness 的 catch
                    // 接住，并合成一个失败 run。
                    let mut run = create_failed_harness_run(input, &error, &artifacts);
                    ensure_run_trace(&mut run, name, started_at, Utc::now());
                    Err(error.attach_run(run))
                }
            }
        }
        Err(mut error) => {
            if let Some(mut run) = error.run.take() {
                // 只在 partial run 还没有 artifacts 时补；已有的保持原样。
                if run.artifacts.is_none() {
                    run.artifacts = artifacts_map(&context.artifacts());
                }
                ensure_run_trace(&mut run, name, started_at, Utc::now());
                error.run = Some(run);
                Err(error)
            } else {
                let mut run = create_failed_harness_run(input, &error, &context.artifacts());
                ensure_run_trace(&mut run, name, started_at, Utc::now());
                Err(error.attach_run(run))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 公开类型（KianaCodingAgent harness）
// ---------------------------------------------------------------------------

/// 模型选择（`{provider, id}`）。
///
/// TS: `harness.ts#KianaCodingAgentModelSelection`（字段名是 `id`，不是 `model`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KianaCodingAgentModelSelection {
    /// provider id。
    pub provider: String,
    /// model id。
    pub id: String,
}

/// `resolveModelSelection` 的 `environment` 参数。
///
/// TS 的默认值是 `process.env`；[`ModelSelectionEnvironment::from_process_env`] 提供同样语义。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelSelectionEnvironment {
    /// `KIANA_PROVIDER`。
    pub kiana_provider: Option<String>,
    /// `KIANA_MODEL`。
    pub kiana_model: Option<String>,
}

impl ModelSelectionEnvironment {
    /// 从进程环境读取 `KIANA_PROVIDER` / `KIANA_MODEL`（未设置或非 UTF-8 → `None`）。
    pub fn from_process_env() -> Self {
        ModelSelectionEnvironment {
            kiana_provider: std::env::var("KIANA_PROVIDER").ok(),
            kiana_model: std::env::var("KIANA_MODEL").ok(),
        }
    }
}

/// harness 的一个输入 step。
///
/// TS: `KianaCodingAgentInput` 数组元素（`{type:"prompt",content}` / `{type:"reload"}`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KianaCodingAgentStep {
    /// `{type:"prompt", content}`。
    Prompt {
        /// 用户提示。
        content: String,
    },
    /// `{type:"reload"}`：重建会话的扩展运行器并重新发现资源，不发送 prompt。
    Reload,
}

/// harness 输入：单个字符串 prompt，或有序 step 数组。
///
/// TS: `KianaCodingAgentInput = string | Array<{type:"prompt";content} | {type:"reload"}>`
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KianaCodingAgentInput {
    /// `string`。
    Prompt(String),
    /// step 数组。
    Steps(Vec<KianaCodingAgentStep>),
}

impl KianaCodingAgentInput {
    /// 展开成 step 数组（字符串 → 单个 prompt step）。
    fn steps(&self) -> Vec<KianaCodingAgentStep> {
        match self {
            KianaCodingAgentInput::Prompt(content) => vec![KianaCodingAgentStep::Prompt {
                content: content.clone(),
            }],
            KianaCodingAgentInput::Steps(steps) => steps.clone(),
        }
    }
}

impl From<&str> for KianaCodingAgentInput {
    fn from(value: &str) -> Self {
        KianaCodingAgentInput::Prompt(value.to_string())
    }
}

impl From<String> for KianaCodingAgentInput {
    fn from(value: String) -> Self {
        KianaCodingAgentInput::Prompt(value)
    }
}

impl From<Vec<KianaCodingAgentStep>> for KianaCodingAgentInput {
    fn from(value: Vec<KianaCodingAgentStep>) -> Self {
        KianaCodingAgentInput::Steps(value)
    }
}

/// `output` 回调的入参。
///
/// TS: `{ response, session, systemPrompt, agentDir }`。
pub struct KianaCodingAgentOutputArgs<'a> {
    /// 最后一次 prompt 的文本输出。
    pub response: &'a str,
    /// 存活的会话（可读取 `modelRuntime` / `resourceLoader` 等）。
    pub session: &'a AgentSession,
    /// 发给 provider 的系统提示。
    pub system_prompt: &'a str,
    /// 隔离的 agent 目录。
    pub agent_dir: &'a str,
}

/// `output` 回调（可异步）。
pub type KianaCodingAgentOutputFn<TOutput> =
    Arc<dyn for<'a> Fn(KianaCodingAgentOutputArgs<'a>) -> LocalBoxFuture<'a, TOutput>>;

/// 归一化输出回调（把 `TOutput` 转成 JSON 值）。
type NormalizedOutputFn = Arc<
    dyn for<'a> Fn(
        KianaCodingAgentOutputArgs<'a>,
    ) -> LocalBoxFuture<'a, Result<JsonValue, HarnessError>>,
>;

/// system prompt 变换回调。
///
/// TS 的 `(defaultPrompt: string) => string` 可以抛异常；Rust 用 `Result` 表达同一失败路径。
/// 返回 `Err` 时扩展处理器把错误交还 SDK，`session.prompt` 随之失败，最终整个 run 以该
/// 错误结束（对应 TS 的异常冒泡）。回调要求 `Send + Sync`（放在 `Arc` 里跨异步边界保存）。
pub type TransformSystemPromptFn = Arc<dyn Fn(&str) -> Result<String, HarnessError> + Send + Sync>;

/// harness 选项。
///
/// TS: `harness.ts#KianaCodingAgentHarnessOptions`。字段名映射：
/// `noTools` → `no_tools`、`customTools` → `custom_tools`、`workspaceFiles` →
/// `workspace_files`、`transformSystemPrompt` → `transform_system_prompt`、
/// `expectedKianaDocumentation` → `expected_kiana_documentation`。
#[derive(Clone, Default)]
pub struct KianaCodingAgentHarnessOptions {
    /// harness 名（缺省 `"kiana-coding-agent"`）。
    pub name: Option<String>,
    /// 显式模型选择（缺省读 `KIANA_PROVIDER`/`KIANA_MODEL`）。
    pub model: Option<KianaCodingAgentModelSelection>,
    /// `noTools`（`"all" | "builtin"`）。
    pub no_tools: Option<NoTools>,
    /// 工具允许列表。
    pub tools: Option<Vec<String>>,
    /// 额外注册的自定义工具。
    pub custom_tools: Vec<ToolDefinition>,
    /// workspace 初始文件（插入序决定写入顺序）。
    pub workspace_files: indexmap::IndexMap<String, String>,
    /// system prompt 变换（在 `before_agent_start` 里生效）。
    ///
    /// TS 的回调可以抛异常；Rust 用 `Result` 表达同一失败路径（错误会作为扩展处理器
    /// 错误冒泡，与 TS 的异常一致）。
    pub transform_system_prompt: Option<TransformSystemPromptFn>,
    /// 期望的 Kiana 文档状态；`None` 表示不校验。
    pub expected_kiana_documentation: Option<bool>,
}

impl std::fmt::Debug for KianaCodingAgentHarnessOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KianaCodingAgentHarnessOptions")
            .field("name", &self.name)
            .field("model", &self.model)
            .field("no_tools", &self.no_tools)
            .field("tools", &self.tools)
            .field("custom_tools_len", &self.custom_tools.len())
            .field("workspace_files", &self.workspace_files)
            .field(
                "transform_system_prompt",
                &self.transform_system_prompt.is_some(),
            )
            .field(
                "expected_kiana_documentation",
                &self.expected_kiana_documentation,
            )
            .finish()
    }
}

/// 带 `output` 回调的 harness 选项。
///
/// TS: `harness.ts#KianaCodingAgentHarnessWithOutput<TOutput>`
/// （`KianaCodingAgentHarnessOptions & { output }`）。
pub struct KianaCodingAgentHarnessWithOutput<TOutput> {
    /// 基础选项。
    pub options: KianaCodingAgentHarnessOptions,
    /// 输出回调。
    pub output: KianaCodingAgentOutputFn<TOutput>,
}

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// 解析 harness 的模型选择。
///
/// TS: `harness.ts#resolveModelSelection`
///
/// `provider`/`id` 都用 [`js_trim`] 处理；任一为空（`!provider || !id`）→
/// `Select a harness model explicitly or set both KIANA_PROVIDER and KIANA_MODEL as defaults.`
pub fn resolve_model_selection(
    explicit_model: Option<&KianaCodingAgentModelSelection>,
) -> Result<KianaCodingAgentModelSelection, HarnessError> {
    resolve_model_selection_from(
        explicit_model,
        &ModelSelectionEnvironment::from_process_env(),
    )
}

/// [`resolve_model_selection`] 的显式环境版本（TS 的第二个参数）。
///
/// 解析顺序：显式 `explicit_model` 的字段优先，缺的字段才回退到 `environment`；两边都做
/// trim；最终 provider 或 id 任一为空即失败（`name = "Error"`，消息是 TS 原文）。
/// 显式值里的空白串**不会**继续回退到环境变量——判断依据是字段有没有值（`Option`），
/// 不是 trim 后是否为空，所以 `Some("  ")` 会直接导致失败。
pub fn resolve_model_selection_from(
    explicit_model: Option<&KianaCodingAgentModelSelection>,
    environment: &ModelSelectionEnvironment,
) -> Result<KianaCodingAgentModelSelection, HarnessError> {
    let raw_provider = explicit_model
        .map(|model| model.provider.clone())
        .or_else(|| environment.kiana_provider.clone());
    let raw_id = explicit_model
        .map(|model| model.id.clone())
        .or_else(|| environment.kiana_model.clone());
    let provider = raw_provider
        .map(|value| js_trim(&value).to_string())
        .unwrap_or_default();
    let id = raw_id
        .map(|value| js_trim(&value).to_string())
        .unwrap_or_default();
    if provider.is_empty() || id.is_empty() {
        return Err(HarnessError::new(
            "Select a harness model explicitly or set both KIANA_PROVIDER and KIANA_MODEL as defaults.",
        ));
    }
    Ok(KianaCodingAgentModelSelection { provider, id })
}

/// 覆盖进程环境并返回恢复闭包。
///
/// TS: `harness.ts#applyIsolatedEnvironment(home, agentDir)`
///
/// 删除所有 `KIANA_EVAL_` 前缀的键（记住原值），再设置 `HOME`/`USERPROFILE`=`home`、
/// `KIANA_CODING_AGENT_DIR`=`agentDir`（原值未被记录时先记录）。恢复闭包逐个还原
/// （原值为 `undefined` 时删除）。
///
/// 进程环境是全局状态；调用方必须保证没有并发读者（测试里用互斥锁串行化）。
///
/// # 顺序约束
///
/// 必须在读取 `KIANA_EVAL_SANDBOX_UID`/`KIANA_EVAL_SANDBOX_GID`、`KIANA_EVAL_VARIANT` 等
/// 变量**之后**调用：本函数会把所有 `KIANA_EVAL_` 前缀的键从进程环境删除。
/// `run_kiana_coding_agent` 就是这样排序的（先解析模型选择与沙箱身份，再调用本函数）。
///
/// 返回的恢复闭包是 `FnOnce`：重复调用不会二次恢复；丢弃不调用则环境保持被改写状态。
///
/// 参数是 `&str`：`run_kiana_coding_agent` 先把临时路径 `to_string_lossy` 再传入，因此
/// `TMPDIR` 指向非 UTF-8 路径时，写进环境变量的值会有损（文件系统操作用的是原始 `Path`）。
pub fn apply_isolated_environment(home: &str, agent_dir: &str) -> impl FnOnce() {
    let mut previous: Vec<(OsString, Option<OsString>)> = Vec::new();
    let mut recorded: HashSet<String> = HashSet::new();
    // 先快照所有键再删除：不要一边迭代 `vars_os()` 一边修改进程环境。
    let keys: Vec<OsString> = std::env::vars_os().map(|(key, _)| key).collect();
    for key in keys {
        let Some(name) = key.to_str() else {
            continue;
        };
        if !name.starts_with("KIANA_EVAL_") {
            continue;
        }
        previous.push((key.clone(), std::env::var_os(&key)));
        recorded.insert(name.to_string());
        // SAFETY: 调用方保证没有并发进程环境读写（与 TS 修改 process.env 的前提一致）。
        unsafe { std::env::remove_var(&key) };
    }
    let overrides = [
        ("HOME", home),
        ("USERPROFILE", home),
        ("KIANA_CODING_AGENT_DIR", agent_dir),
    ];
    for (name, value) in overrides {
        // `recorded` 防止同一个键被记录两次；恢复闭包按记录顺序逐个还原（每个键只出现
        // 一次，因此顺序不影响结果）。
        if !recorded.contains(name) {
            previous.push((OsString::from(name), std::env::var_os(name)));
        }
        // SAFETY: 同上；名称是合法的 UTF-8 环境变量名。
        unsafe { std::env::set_var(name, value) };
    }
    move || {
        for (name, value) in previous {
            match value {
                // SAFETY: 恢复调用发生在同一线程的任务收尾阶段，调用方保证独占环境。
                Some(value) => unsafe { std::env::set_var(&name, value) },
                // SAFETY: 删除一个此前不存在或已记录的键。
                None => unsafe { std::env::remove_var(&name) },
            }
        }
    }
}

/// 沙箱身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SandboxIdentity {
    uid: u32,
    gid: u32,
}

/// `Number(value)` 的本地实现（`process.env` 里只会出现十进制小整数，但仍按 JS 规则解析）。
fn parse_js_number(text: &str) -> f64 {
    // 只实现 Number(value) 在本文件用得到的子集：可选符号、Infinity、无符号的
    // 0x/0b/0o 字面量，以及十进制（可带指数）。其余一律 NaN；空白串按 ECMAScript 的
    // StringNumericLiteral 规则得到 0。
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let bytes = text.as_bytes();
    let (sign, rest) = match bytes[0] {
        b'+' => (1.0_f64, &text[1..]),
        b'-' => (-1.0_f64, &text[1..]),
        _ => (1.0_f64, text),
    };
    if rest == "Infinity" {
        return sign * f64::INFINITY;
    }
    // `0x`/`0b`/`0o` 前缀只在没有符号时识别（Number("-0x10") === NaN）。
    if sign > 0.0
        && text.len() > 2
        && bytes[0] == b'0'
        && matches!(bytes[1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    {
        let base = match bytes[1] {
            b'x' | b'X' => 16u32,
            b'b' | b'B' => 2,
            _ => 8,
        };
        let mut value = 0.0_f64;
        let mut any = false;
        for byte in &bytes[2..] {
            let Some(digit) = (*byte as char).to_digit(base) else {
                return f64::NAN;
            };
            value = value * f64::from(base) + f64::from(digit);
            any = true;
        }
        return if any { value } else { f64::NAN };
    }
    if !is_js_decimal_literal(rest) {
        return f64::NAN;
    }
    rest.parse::<f64>().map_or(f64::NAN, |value| sign * value)
}

/// JS 的 `StringNumericLiteral`（十进制部分）：`digits[.digits][e[+-]digits]`。
fn is_js_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut mantissa_digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        mantissa_digits += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            mantissa_digits += 1;
        }
    }
    if mantissa_digits == 0 {
        return false;
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let exponent_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
}

/// `Number.isSafeInteger(value)`。
fn is_safe_integer(value: f64) -> bool {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
}

/// `parseSandboxId(name)`。
///
/// TS: `harness.ts#parseSandboxId`。未设置 → `None`；`Number(value)` 不是安全整数或
/// `< 1` → `{name} must be a positive integer.`
fn parse_sandbox_id(name: &str) -> Result<Option<u32>, HarnessError> {
    let Some(value) = std::env::var(name).ok() else {
        return Ok(None);
    };
    let id = parse_js_number(&value);
    if !is_safe_integer(id) || id < 1.0 {
        return Err(HarnessError::new(format!(
            "{name} must be a positive integer."
        )));
    }
    Ok(Some(id as u32))
}

/// `resolveSandboxIdentity()`。
///
/// TS: `harness.ts#resolveSandboxIdentity`。两者都未设 → `None`；只设一个 →
/// `Set both KIANA_EVAL_SANDBOX_UID and KIANA_EVAL_SANDBOX_GID, or neither.`
fn resolve_sandbox_identity() -> Result<Option<SandboxIdentity>, HarnessError> {
    let uid = parse_sandbox_id("KIANA_EVAL_SANDBOX_UID")?;
    let gid = parse_sandbox_id("KIANA_EVAL_SANDBOX_GID")?;
    match (uid, gid) {
        (None, None) => Ok(None),
        (Some(uid), Some(gid)) => Ok(Some(SandboxIdentity { uid, gid })),
        _ => Err(HarnessError::new(
            "Set both KIANA_EVAL_SANDBOX_UID and KIANA_EVAL_SANDBOX_GID, or neither.",
        )),
    }
}

/// `os.tmpdir()`：`TMPDIR` → `TMP` → `TEMP` → `/tmp`。
fn node_tmpdir() -> PathBuf {
    for name in ["TMPDIR", "TMP", "TEMP"] {
        if let Some(value) = std::env::var_os(name)
            && !value.is_empty()
        {
            return PathBuf::from(value);
        }
    }
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

/// `fs.promises.mkdtemp(join(tmpdir(), prefix))` 的等价物。
///
/// 失败时返回 `(错误, 本次尝试的路径)`：libuv 在调用 `mkdir` 之前就把 `XXXXXX` 展开成
/// 随机的 6 个字符，因此 Node 的 `SystemError.message` 里是生成的目录名（不是模板）。
fn mkdtemp(parent: &Path, prefix: &str) -> Result<PathBuf, (std::io::Error, PathBuf)> {
    // 最多尝试 100 次：EEXIST 视为后缀碰撞、换后缀重试；其它错误（例如父目录不存在）
    // 立即返回。unix 上目录以 0o700 创建（仅属主可访问）。
    let mut last_path = parent.join(format!("{prefix}XXXXXX"));
    for _ in 0..100 {
        let suffix = random_suffix();
        let path = parent.join(format!("{prefix}{suffix}"));
        last_path = path.clone();
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err((error, path)),
        }
    }
    Err((
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "mkdtemp could not create a unique directory",
        ),
        last_path,
    ))
}

/// libuv 的 `mkdtemp` 后缀：6 个 `[0-9A-Za-z]` 字符。
fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes
        .iter()
        .take(6)
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
        .collect()
}

/// `path.isAbsolute`（Node 语义）。
fn js_is_absolute(path: &str) -> bool {
    #[cfg(unix)]
    {
        path.starts_with('/')
    }
    #[cfg(windows)]
    {
        let bytes = path.as_bytes();
        path.starts_with('\\')
            || path.starts_with('/')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/'))
    }
}

/// `path.resolve(base, name)` 的纯词法版本（不访问文件系统）。
fn resolve_lexical(base: &Path, name: &str) -> PathBuf {
    let joined = if js_is_absolute(name) {
        PathBuf::from(name)
    } else {
        base.join(name)
    };
    normalize_lexical(&joined)
}

/// `path.normalize`：折叠 `.` 与 `..`（`..` 在根处保持根）。
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => output.push(prefix.as_os_str()),
            Component::RootDir => output.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            Component::Normal(part) => output.push(part),
        }
    }
    output
}

/// `relative(workspace, path)` 是否为 `..` / `../...` / 绝对路径。
///
/// 判断方式：`strip_prefix` 失败（路径不在 workspace 下，包括绝对路径）即视为逃逸；
/// 成功时检查剩余部分的第一段不是 `..`。`normalize_lexical` 已折叠过 `.` 与 `..`，因此
/// 这等价于「解析后仍位于 workspace 内」。
///
/// 纯词法判断，不访问文件系统，也不识别符号链接造成的真实逃逸。
fn escapes_workspace(workspace: &Path, path: &Path) -> bool {
    match path.strip_prefix(workspace) {
        Ok(rest) => rest
            .components()
            .next()
            .is_some_and(|component| matches!(component, Component::ParentDir)),
        Err(_) => true,
    }
}

/// `seedWorkspace(workspace, files)`。
///
/// 按 `Object.entries(files)` 的顺序写入：
/// - 空名/绝对路径 → `Invalid workspace fixture path: {name}`
/// - 解析后逃逸 workspace → `Workspace fixture escapes the workspace: {name}`
/// - `mkdir(dirname, recursive)` + `writeFile(..., {mode: 0o600})`
///
/// 检查与写入逐条进行：某个条目失败会立刻返回，**此前已写入的文件留在磁盘上**（临时目录
/// 随后由 cleanup 删除）。错误类型：路径检查失败是 `TypeError`，文件系统失败是 Node 文案
/// 的普通 `Error`。
///
/// `resolve_lexical` 只做词法解析、不跟随符号链接；workspace 是刚创建的空目录，因此这里
/// 不考虑链接逃逸。
async fn seed_workspace(
    workspace: &Path,
    files: &indexmap::IndexMap<String, String>,
) -> Result<(), HarnessError> {
    for (name, content) in files {
        if name.is_empty() || js_is_absolute(name) {
            return Err(HarnessError::type_error(format!(
                "Invalid workspace fixture path: {name}"
            )));
        }
        let path = resolve_lexical(workspace, name);
        if escapes_workspace(workspace, &path) {
            return Err(HarnessError::type_error(format!(
                "Workspace fixture escapes the workspace: {name}"
            )));
        }
        if let Some(parent) = path.parent() {
            // TS: `mkdir(dirname(path), { recursive: true })`。
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| node_fs_error(&error, "mkdir", Some(parent)))?;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        // TS: `writeFile(path, content, { mode: 0o600 })` 内部先 `open(path, 'w')`。
        let mut file = options
            .open(&path)
            .await
            .map_err(|error| node_fs_error(&error, "open", Some(&path)))?;
        use tokio::io::AsyncWriteExt as _;
        file.write_all(content.as_bytes())
            .await
            .map_err(|error| node_fs_error(&error, "write", None))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Node `fs` / `process` 错误文案
// ---------------------------------------------------------------------------

/// Linux 的 errno → libuv 的 `(code, description)`（Node 26 的 `util.getSystemErrorMessage`
/// 实测；文本与 `uv_strerror` 一致）。
///
/// 与 `kiana-coding-agent` 启动期共用同一套文案；那个辅助函数只在 `cli::setup` 里公开、
/// 不在 coding-agent 的 SDK 导出面上，所以这里保留一份本地副本。
#[cfg(target_os = "linux")]
const UNIX_ERRNO_PARTS: &[(i32, &str, &str)] = &[
    (1, "EPERM", "operation not permitted"),
    (2, "ENOENT", "no such file or directory"),
    (3, "ESRCH", "no such process"),
    (4, "EINTR", "interrupted system call"),
    (5, "EIO", "i/o error"),
    (6, "ENXIO", "no such device or address"),
    (7, "E2BIG", "argument list too long"),
    (8, "ENOEXEC", "exec format error"),
    (9, "EBADF", "bad file descriptor"),
    (11, "EAGAIN", "resource temporarily unavailable"),
    (12, "ENOMEM", "not enough memory"),
    (13, "EACCES", "permission denied"),
    (14, "EFAULT", "bad address in system call argument"),
    (16, "EBUSY", "resource busy or locked"),
    (17, "EEXIST", "file already exists"),
    (18, "EXDEV", "cross-device link not permitted"),
    (19, "ENODEV", "no such device"),
    (20, "ENOTDIR", "not a directory"),
    (21, "EISDIR", "illegal operation on a directory"),
    (22, "EINVAL", "invalid argument"),
    (23, "ENFILE", "file table overflow"),
    (24, "EMFILE", "too many open files"),
    (25, "ENOTTY", "inappropriate ioctl for device"),
    (26, "ETXTBSY", "text file is busy"),
    (27, "EFBIG", "file too large"),
    (28, "ENOSPC", "no space left on device"),
    (29, "ESPIPE", "invalid seek"),
    (30, "EROFS", "read-only file system"),
    (31, "EMLINK", "too many links"),
    (32, "EPIPE", "broken pipe"),
    (34, "ERANGE", "result too large"),
    (36, "ENAMETOOLONG", "name too long"),
    (38, "ENOSYS", "function not implemented"),
    (39, "ENOTEMPTY", "directory not empty"),
    (40, "ELOOP", "too many symbolic links encountered"),
    (49, "EUNATCH", "protocol driver not attached"),
    (61, "ENODATA", "no data available"),
    (64, "ENONET", "machine is not on the network"),
    (71, "EPROTO", "protocol error"),
    (75, "EOVERFLOW", "value too large for defined data type"),
    (84, "EILSEQ", "illegal byte sequence"),
    (88, "ENOTSOCK", "socket operation on non-socket"),
    (89, "EDESTADDRREQ", "destination address required"),
    (90, "EMSGSIZE", "message too long"),
    (91, "EPROTOTYPE", "protocol wrong type for socket"),
    (92, "ENOPROTOOPT", "protocol not available"),
    (93, "EPROTONOSUPPORT", "protocol not supported"),
    (94, "ESOCKTNOSUPPORT", "socket type not supported"),
    (95, "ENOTSUP", "operation not supported on socket"),
    (97, "EAFNOSUPPORT", "address family not supported"),
    (98, "EADDRINUSE", "address already in use"),
    (99, "EADDRNOTAVAIL", "address not available"),
    (100, "ENETDOWN", "network is down"),
    (101, "ENETUNREACH", "network is unreachable"),
    (103, "ECONNABORTED", "software caused connection abort"),
    (104, "ECONNRESET", "connection reset by peer"),
    (105, "ENOBUFS", "no buffer space available"),
    (106, "EISCONN", "socket is already connected"),
    (107, "ENOTCONN", "socket is not connected"),
    (
        108,
        "ESHUTDOWN",
        "cannot send after transport endpoint shutdown",
    ),
    (110, "ETIMEDOUT", "connection timed out"),
    (111, "ECONNREFUSED", "connection refused"),
    (112, "EHOSTDOWN", "host is down"),
    (113, "EHOSTUNREACH", "host is unreachable"),
    (114, "EALREADY", "connection already in progress"),
    (121, "EREMOTEIO", "remote I/O error"),
    (125, "ECANCELED", "operation canceled"),
];

/// 底层 `io::Error` → Node/libuv 的 `(code, description)`。
fn node_error_parts(error: &std::io::Error) -> (&'static str, String) {
    // 优先用真实 errno：Linux 下查本地表；表里没有的 errno 必须输出 libuv 的
    // "UNKNOWN"/"Unknown system error -N"，而不是 Rust 的 strerror 文本。
    #[cfg(target_os = "linux")]
    if let Some(raw) = error.raw_os_error() {
        if let Some((_, code, message)) = UNIX_ERRNO_PARTS.iter().find(|(errno, ..)| *errno == raw)
        {
            return (code, (*message).to_string());
        }
        // libuv 的 `uv_err_name` 对未知 errno 返回 "UNKNOWN"，`uv_strerror` 返回
        // "Unknown system error -N"。
        return ("UNKNOWN", format!("Unknown system error -{raw}"));
    }
    // 走到这里说明没有可用的 errno 表（非 Linux 平台，或 raw_os_error 为 None 的合成
    // 错误）：按 ErrorKind 猜一个 libuv 风格的 code；都不匹配时退回稳定文案，去掉 Rust
    // 追加的 " (os error N)" 并把首字母改成小写以贴近 libuv。
    use std::io::ErrorKind;
    let known = match error.kind() {
        ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        ErrorKind::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        ErrorKind::NotADirectory => Some(("ENOTDIR", "not a directory")),
        ErrorKind::DirectoryNotEmpty => Some(("ENOTEMPTY", "directory not empty")),
        ErrorKind::ReadOnlyFilesystem => Some(("EROFS", "read-only file system")),
        ErrorKind::StorageFull => Some(("ENOSPC", "no space left on device")),
        ErrorKind::CrossesDevices => Some(("EXDEV", "cross-device link not permitted")),
        ErrorKind::InvalidInput => Some(("EINVAL", "invalid argument")),
        _ => None,
    };
    if let Some((code, message)) = known {
        return (code, message.to_string());
    }
    // libuv 文案小写开头；Rust 的 strerror 大写开头且带 " (os error N)"。
    let display = error.to_string();
    let message = display
        .rsplit_once(" (os error ")
        .map_or(display.as_str(), |(message, _)| message);
    let mut characters = message.chars();
    let message = match characters.next() {
        Some(first) => first.to_lowercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    };
    ("UNKNOWN", message)
}

/// Node `fs` 错误文案：`<code>: <description>, <syscall> '<path>'`。
///
/// TS 的所有文件系统调用都走 `fs/promises`，失败时抛出的 `Error.message` 就是这个形状
/// （例如 `EISDIR: illegal operation on a directory, open '/tmp/kiana-eval-…/workspace'`）。
/// Node 的 `SystemError` 还带有可枚举自有属性 `errno`/`code`/`syscall`/`path`，它们会在
/// `toJsonValue` 序列化时替代 `{type, message}`（见 [`normalize_simple_error`]）。
///
/// 只有 `error.raw_os_error()` 存在时才写入这些自有属性（`errno` 取负值，fs 错误的约定）；
/// 没有真实 errno 的合成错误仍按普通 `Error` 序列化成 `{type, message}`。
fn node_fs_error(error: &std::io::Error, syscall: &str, path: Option<&Path>) -> HarnessError {
    let (code, description) = node_error_parts(error);
    let mut message = format!("{code}: {description}, {syscall}");
    if let Some(path) = path {
        message.push_str(&format!(" '{}'", path.display()));
    }
    let mut properties = Map::new();
    if let Some(errno) = error.raw_os_error() {
        properties.insert("errno".to_string(), Value::from(-errno));
        properties.insert("code".to_string(), Value::String(code.to_string()));
        properties.insert("syscall".to_string(), Value::String(syscall.to_string()));
        if let Some(path) = path {
            properties.insert(
                "path".to_string(),
                Value::String(path.to_string_lossy().into_owned()),
            );
        }
    }
    let error = HarnessError::new(message);
    if properties.is_empty() {
        error
    } else {
        error.with_own_properties(properties)
    }
}

/// Node `process.setuid` / `setgid` / `setgroups` 的错误文案：`<code>, <Description>`。
///
/// Node 的非 `fs` `errnoException` 用 "EPERM, Operation not permitted"（没有 syscall 与路径），
/// 但错误对象仍有可枚举的 `errno`/`code`/`syscall`（`errno` 为正数，与 fs 的负 errno 不同）。
fn node_process_error(error: &std::io::Error, syscall: &str) -> HarnessError {
    let (code, description) = node_error_parts(error);
    let mut characters = description.chars();
    let description = match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    };
    let mut properties = Map::new();
    if let Some(errno) = error.raw_os_error() {
        properties.insert("errno".to_string(), Value::from(errno));
        properties.insert("code".to_string(), Value::String(code.to_string()));
        properties.insert("syscall".to_string(), Value::String(syscall.to_string()));
    }
    let error = HarnessError::new(format!("{code}, {description}"));
    if properties.is_empty() {
        error
    } else {
        error.with_own_properties(properties)
    }
}

/// `fs.promises.rm(path, { recursive: true, force: true })` 的等价物。
///
/// 逐层 `lstat` → `scandir` → `unlink`/`rmdir`，错误文案带 Node 的 syscall 与路径；
/// `force: true` 时任何层级的 ENOENT 都忽略（Node 的 rimraf 语义）。
fn node_rm_recursive(path: PathBuf) -> LocalBoxFuture<'static, Result<(), HarnessError>> {
    Box::pin(async move {
        // symlink_metadata（lstat）不跟随链接：符号链接本身按文件删除，不会递归到
        // 被链接的目录（可能位于临时 root 之外）。
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(node_fs_error(&error, "lstat", Some(&path))),
        };
        if metadata.file_type().is_dir() {
            let mut entries = match tokio::fs::read_dir(&path).await {
                Ok(entries) => entries,
                // Node 的 rimraf 在 `force` 下同样忽略 readdir 的 ENOENT。
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(node_fs_error(&error, "scandir", Some(&path))),
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|error| node_fs_error(&error, "scandir", Some(&path)))?
            {
                node_rm_recursive(entry.path()).await?;
            }
            // remove_dir 要求目录已空：先递归删完子项，再删目录本身。
            match tokio::fs::remove_dir(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(node_fs_error(&error, "rmdir", Some(&path))),
            }
        } else {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(node_fs_error(&error, "unlink", Some(&path))),
            }
        }
        Ok(())
    })
}

/// `fs.promises.readFile(path)`：`open` 失败按 `open`、读取阶段失败按 `read` 格式化。
///
/// `tokio::fs::read` 把两个阶段合并成一次调用，目录路径的 `EISDIR` 会被误标为 `open`；
/// Node 的 `readFile` 对 `/tmp` 这类目录报告的是 `read '<path>'`。
async fn node_read_file(path: &Path) -> Result<Vec<u8>, HarnessError> {
    use tokio::io::AsyncReadExt as _;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| node_fs_error(&error, "open", Some(path)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|error| node_fs_error(&error, "read", Some(path)))?;
    Ok(bytes)
}

/// `signal?.throwIfAborted()`。
///
/// 无 signal 或未中止 → `Ok(())`；已中止 → 用 signal 的 reason 构造 [`HarnessError`]
/// （缺省是 DOMException 的 `AbortError`，见下方的文案说明）。`run_kiana_coding_agent`
/// 在开头与每条 prompt 之前调用它。
fn throw_if_aborted(signal: Option<&AbortScope>) -> Result<(), HarnessError> {
    let Some(signal) = signal else {
        return Ok(());
    };
    if !signal.is_aborted() {
        return Ok(());
    }
    // TS 的 `AbortSignal.reason` 在 `abort()` 时默认是 DOMException
    // `AbortError: This operation was aborted`；由 `CancellationToken` 构造的
    // `AbortScope`（`AbortScope::from(token)`，runner 的常规取消路径）没有显式 reason，
    // 必须用同一个 DOMException 文案，而不是 `abortReason()` 的
    // "The operation was aborted" 回退。
    let reason = signal
        .reason()
        .unwrap_or_else(AbortReason::default_abort_error);
    Err(HarnessError::with_name(reason.name, reason.message))
}

/// `StopReason` 的 TS 字面量（`stop` / `toolUse` / `error` …）。
///
/// 这些字符串会进入 `Agent run ended with unexpected stop reason: {…}.` 的错误文案，
/// 也被集成测试用作 provider 响应字段（`tests/support/configured_runtime.rs`）；改字面量
/// 会同时改变这两类输出。
pub fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
        StopReason::TurnCapped => "turnCapped",
        StopReason::TokenCapped => "tokenCapped",
        StopReason::CostCapped => "costCapped",
        StopReason::TimeCapped => "timeCapped",
    }
}

/// `toTranscriptEvents(messages)`。
///
/// TS: `harness.ts#toTranscriptEvents`。只处理标准 user / assistant / toolResult 消息；
/// custom 消息与 system 消息忽略。
///
/// 映射规则（逐项对应 TS）：
/// - user 消息 → `Message{role: user}`，content 是各文本块用 `\n` 拼接的字符串；
/// - assistant 消息 → 文本非空时先出一条 `Message{role: assistant}`，随后**每个**
///   `ToolCall` 块各出一条 `ToolCall`（arguments 转成 JSON 对象，起止时间与 duration
///   留空）；只含工具调用的 assistant 不会产生 Message 事件；
/// - toolResult 消息 → `ToolResult`；内容全是文本块时给字符串，否则给整个内容数组的
///   JSON 值；`is_error` 时填 `error`（文本为空时用 `Tool failed`）；
/// - system 消息、以及 `as_standard()` 返回 `None` 的 custom 消息被跳过。
fn to_transcript_events(messages: &[AgentMessage]) -> Vec<TranscriptEvent> {
    let mut events = Vec::new();
    for message in messages {
        match message.as_standard() {
            Some(Message::User(user)) => {
                events.push(TranscriptEvent::Message {
                    role: TranscriptRole::User,
                    content: Some(Value::String(content_text_user_content(
                        &user.content,
                        "\n",
                    ))),
                    metadata: None,
                });
            }
            Some(Message::Assistant(assistant)) => {
                let text = content_text_assistant(&assistant.content, "\n");
                if !text.is_empty() {
                    events.push(TranscriptEvent::Message {
                        role: TranscriptRole::Assistant,
                        content: Some(Value::String(text)),
                        metadata: None,
                    });
                }
                for block in &assistant.content {
                    let AssistantContentBlock::ToolCall(call) = block else {
                        continue;
                    };
                    events.push(TranscriptEvent::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: Some(
                            call.arguments
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect(),
                        ),
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        metadata: None,
                    });
                }
            }
            Some(Message::ToolResult(result)) => {
                let text = content_text_user(&result.content, "\n");
                let all_text = result
                    .content
                    .iter()
                    .all(|part| matches!(part, UserContentBlock::Text(_)));
                let content = if all_text {
                    Value::String(text.clone())
                } else {
                    serde_json::to_value(&result.content).unwrap_or(Value::Null)
                };
                let error = result.is_error.then(|| NormalizedError {
                    message: if text.is_empty() {
                        "Tool failed".to_string()
                    } else {
                        text.clone()
                    },
                    r#type: None,
                    fields: Map::new(),
                });
                events.push(TranscriptEvent::ToolResult {
                    tool_call_id: result.tool_call_id.clone(),
                    name: Some(result.tool_name.clone()),
                    content: Some(content),
                    error,
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    metadata: None,
                });
            }
            Some(Message::System(_)) | None => {}
        }
    }
    events
}

/// `promptAgent(session, input, signal)`。
///
/// 发送一条 prompt，返回本次新增消息里最后一条 assistant 的文本。
///
/// 步骤：先 `throwIfAborted` → 记录调用前的消息数 → `session.prompt` → 只在新消息里
/// 倒序找最后一条 assistant（工具循环可能产生多条）。
///
/// 失败路径：
/// - 没有任何 assistant 消息 → `Agent run completed without an assistant message.`；
/// - stop reason 既不是 `Stop` 也不是 `ToolUse` → 优先用 assistant 的 `error_message`，
///   否则拼 `Agent run ended with unexpected stop reason: {…}.`；
/// - stop reason 是 `Stop` 却没有 assistant 文本 → `Agent run produced no assistant text.`
///   （`ToolUse` 且无文本是合法的，返回空串）。
async fn prompt_agent(
    session: &AgentSession,
    input: &str,
    signal: Option<&AbortScope>,
) -> Result<String, HarnessError> {
    throw_if_aborted(signal)?;
    // 只统计本次 prompt 新增的消息，避免把历史 transcript 里的 assistant 当成结果。
    let previous_message_count = session.messages().len();
    session
        .prompt(input.to_string(), PromptOptions::default())
        .await
        .map_err(|error| HarnessError::new(error.to_string()))?;
    let messages = session.messages();
    // 倒序取最后一条 assistant：一次工具循环会追加多条 assistant 消息。
    let assistant = messages
        .iter()
        .skip(previous_message_count)
        .rev()
        .find_map(AgentMessage::as_assistant)
        .ok_or_else(|| HarnessError::new("Agent run completed without an assistant message."))?;
    if assistant.stop_reason != StopReason::Stop && assistant.stop_reason != StopReason::ToolUse {
        if let Some(message) = &assistant.error_message {
            return Err(HarnessError::new(message.clone()));
        }
        return Err(HarnessError::new(format!(
            "Agent run ended with unexpected stop reason: {}.",
            stop_reason_str(assistant.stop_reason)
        )));
    }
    let output = session.get_last_assistant_text();
    if output.is_none() && assistant.stop_reason == StopReason::Stop {
        return Err(HarnessError::new("Agent run produced no assistant text."));
    }
    Ok(output.unwrap_or_default())
}

/// `verifySystemPrompt(systemPrompt, options)`。
///
/// TS: `harness.ts#verifySystemPrompt`。`expectedKianaDocumentation === undefined` 时原样返回。
///
/// 开启校验（`expected_kiana_documentation: Some(_)`）时按顺序检查两件事：
/// 1. prompt 里必须有 `\n<rules>\n`，否则报
///    `Kiana system prompt lost its rules in the {name} eval variant.`；
/// 2. `<docs>` 段（识别标记是 `\n<docs>\nKiana documentation (read only`）的存在性必须
///    等于期望值，否则报 `Kiana system prompt does not match the {name} eval variant.`。
///
/// `options.name` 缺失时错误文案里写 `undefined`（TS 模板字符串对 `undefined` 的输出）。
/// 校验通过时返回原字符串的拷贝，不做任何改写。
pub fn verify_system_prompt(
    system_prompt: &str,
    options: &KianaCodingAgentHarnessOptions,
) -> Result<String, HarnessError> {
    let Some(expected) = options.expected_kiana_documentation else {
        return Ok(system_prompt.to_string());
    };
    let name = options.name.as_deref().unwrap_or("undefined");
    if !system_prompt.contains("\n<rules>\n") {
        return Err(HarnessError::new(format!(
            "Kiana system prompt lost its rules in the {name} eval variant."
        )));
    }
    let has_documentation = system_prompt.contains("\n<docs>\nKiana documentation (read only");
    if has_documentation != expected {
        return Err(HarnessError::new(format!(
            "Kiana system prompt does not match the {name} eval variant."
        )));
    }
    Ok(system_prompt.to_string())
}

/// `resolveDocumentationVariant(value = process.env.KIANA_EVAL_VARIANT)`。
///
/// 合法值原样返回；否则 `KIANA_EVAL_VARIANT must be "without_docs" or "with_docs".`
///
/// 只有 `value` 为 `None` 时才读环境变量；未设置、非 UTF-8、空串或其它值都算非法
/// （错误对象 `name = "TypeError"`，对应 TS 抛 `TypeError`）。比较是逐字节精确匹配，
/// 不做 trim，因此 `" with_docs"` 会被拒绝。
pub fn resolve_documentation_variant(
    value: Option<&str>,
) -> Result<DocumentationVariant, HarnessError> {
    let value = match value {
        Some(value) => Some(value.to_string()),
        None => std::env::var("KIANA_EVAL_VARIANT").ok(),
    };
    match value.as_deref() {
        Some("without_docs") => Ok(DocumentationVariant::WithoutDocs),
        Some("with_docs") => Ok(DocumentationVariant::WithDocs),
        _ => Err(HarnessError::type_error(
            "KIANA_EVAL_VARIANT must be \"without_docs\" or \"with_docs\".",
        )),
    }
}

/// 文档评测刻意排除 shell 与无限制网络工具。
///
/// TS: `harness.ts#DOCUMENTATION_EVAL_TOOLS`
///
/// 这是工具**允许列表**（`options.tools`）的默认值：`documentation_options` 只在调用方
/// 没有显式给 `tools` 时使用它；显式传入空数组会保持为空。顺序与 TS 数组一致。
pub const DOCUMENTATION_EVAL_TOOLS: [&str; 6] = ["read", "write", "edit", "grep", "find", "ls"];

/// `excludeKianaDocumentation(defaultPrompt)`。
///
/// TS: `harness.ts#excludeKianaDocumentation`：只删除 `<docs>` 段（保留前后一切）。
/// 三个错误文本是当前稳定契约，保持不变。
///
/// 算法（纯字符串索引，不解析 HTML）：
/// 1. 找第一个 `\n<docs>\n` 作为删除起点（找不到 → 第一条错误）；
/// 2. 从起点之后找第一个 `\n</docs>` 作为删除终点（找不到 → 第二条错误）；
/// 3. 要求 `\n<cwd>\n` 的最后一次出现不早于删除终点（找不到或更早 → 第三条错误），
///    用来确认拿到的确实是「文档路由段」而不是别的 `<docs>` 块；
/// 4. 输出 = 起点之前 + 终点标记之后，两侧文本原样保留（标记周围的换行包含在删除范围）。
///
/// 错误顺序固定：缺 `<docs>` 时报第一条，即使同时缺 `<cwd>` 也不会走到第三条。
pub fn exclude_kiana_documentation(default_prompt: &str) -> Result<String, HarnessError> {
    const DOCUMENTATION_START_MARKER: &str = "\n<docs>\n";
    const DOCUMENTATION_END_MARKER: &str = "\n</docs>";
    let documentation_start = default_prompt
        .find(DOCUMENTATION_START_MARKER)
        .ok_or_else(|| {
            HarnessError::new("Default Kiana system prompt has no Kiana documentation section.")
        })?;
    let documentation_end = default_prompt[documentation_start..]
        .find(DOCUMENTATION_END_MARKER)
        .map(|offset| documentation_start + offset)
        .ok_or_else(|| {
            HarnessError::new(
                "Default Kiana system prompt has no complete Kiana documentation section.",
            )
        })?;
    let cwd_start = default_prompt
        .rfind("\n<cwd>\n")
        .map_or(-1_i64, |index| index as i64);
    // 文档段必须先于工作目录段结束；借助 `<cwd>` 的存在确认删的是正确的那一段。
    if cwd_start < documentation_end as i64 {
        return Err(HarnessError::new(
            "Default Kiana system prompt has no working-directory section.",
        ));
    }
    Ok(format!(
        "{}{}",
        &default_prompt[..documentation_start],
        &default_prompt[documentation_end + DOCUMENTATION_END_MARKER.len()..]
    ))
}

// ---------------------------------------------------------------------------
// 沙箱（仅 POSIX）
// ---------------------------------------------------------------------------

/// `chownTree(path, identity)`：`lstat`，递归真实目录（不跟随符号链接），最后 chown。
///
/// 遍历用显式栈做深度优先，把访问过的路径按「父在前、子在后」收集进 `post_order`，再逆序
/// （先子后父）逐个 chown。目录判断用 `symlink_metadata`（lstat）：符号链接不会被当成
/// 目录展开，因此遍历不会进入临时 root 之外；但真正执行的是 `libc::chown`，它会跟随
/// 链接（与 Node 的 `fs.chown` 一致，见下方 SAFETY 注释），所以 root 内若有指向外部的
/// 符号链接，chown 会作用到链接目标。
///
/// 任何 `lstat`/`scandir`/`chown` 失败都以 Node 的 syscall 文案返回；调用方（降权流程）
/// 在失败时中止，不会继续 `setuid`。
#[cfg(unix)]
async fn chown_tree(root: &Path, identity: &SandboxIdentity) -> Result<(), HarnessError> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut stack = vec![root.to_path_buf()];
    let mut post_order = Vec::new();
    // 第一遍只遍历并记录路径（此时仍是 root 身份，读权限不受影响）。
    while let Some(path) = stack.pop() {
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|error| node_fs_error(&error, "lstat", Some(&path)))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            let mut entries = tokio::fs::read_dir(&path)
                .await
                .map_err(|error| node_fs_error(&error, "scandir", Some(&path)))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|error| node_fs_error(&error, "scandir", Some(&path)))?
            {
                stack.push(entry.path());
            }
        }
        post_order.push(path);
    }
    for path in post_order.into_iter().rev() {
        // 逆序处理（先子后父）。chown 只改属主/属组，遍历顺序不影响最终结果。
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| HarnessError::new("EINVAL: invalid argument, chown"))?;
        // SAFETY: `c_path` 是合法的 NUL 结尾路径。Node 的 `fs.promises.chown` 走 `chown(2)`
        // （跟随符号链接；`lchown` 才是 no-follow），这里按 TS 参考实现保持跟随。
        let result = unsafe { libc::chown(c_path.as_ptr(), identity.uid, identity.gid) };
        if result != 0 {
            return Err(node_fs_error(
                &std::io::Error::last_os_error(),
                "chown",
                Some(&path),
            ));
        }
    }
    Ok(())
}

/// `chmod(path, mode)`。
#[cfg(unix)]
async fn chmod(path: &Path, mode: u32) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|error| node_fs_error(&error, "chmod", Some(path)))
}

/// `protectTransformedModules()`：把 tmpdir 下每个目录的 `ssr` 缓存设为不可读。
///
/// 「transformed module」是 eval 运行时转换后的模块缓存（目录名 `ssr`）。目的是让降权后
/// 的工具进程读不到 eval 自身的 transformed module，否则降权后的复查会报错（见
/// `enter_tool_sandbox_unix`）。做法：在仍然持有 root 权限时，把 `os.tmpdir()` 下每个目录
/// 的 `<dir>/ssr` 权限收紧：目录 0o755、其中的普通文件 0o600。文件属主是 root 时，降权
/// 后即不可读；属主恰好是沙箱用户的文件不会被这一步挡住，由降权后的复查发现。
/// 返回被收紧的文件路径，供降权后逐个尝试打开来验证。
///
/// 边界：只扫描 `os.tmpdir()` 的直接子目录，且只处理名为 `ssr` 的子目录；不存在的跳过。
/// 它修改的是 tmpdir 下**所有**匹配目录（不只本次评测创建的），属于进程级副作用。
#[cfg(unix)]
async fn protect_transformed_modules() -> Result<Vec<PathBuf>, HarnessError> {
    let tmpdir = node_tmpdir();
    let mut protected_paths = Vec::new();
    let mut directories = tokio::fs::read_dir(&tmpdir)
        .await
        .map_err(|error| node_fs_error(&error, "scandir", Some(&tmpdir)))?;
    while let Some(directory) = directories
        .next_entry()
        .await
        .map_err(|error| node_fs_error(&error, "scandir", Some(&tmpdir)))?
    {
        let cache_root = directory.path();
        let is_directory = directory
            .file_type()
            .await
            .map_err(|error| node_fs_error(&error, "lstat", Some(&cache_root)))?
            .is_dir();
        if !is_directory {
            continue;
        }
        let ssr_root = cache_root.join("ssr");
        let mut files = match tokio::fs::read_dir(&ssr_root).await {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(node_fs_error(&error, "scandir", Some(&ssr_root))),
        };
        chmod(&cache_root, 0o755).await?;
        chmod(&ssr_root, 0o755).await?;
        while let Some(file) = files
            .next_entry()
            .await
            .map_err(|error| node_fs_error(&error, "scandir", Some(&ssr_root)))?
        {
            let path = file.path();
            let is_file = file
                .file_type()
                .await
                .map_err(|error| node_fs_error(&error, "lstat", Some(&path)))?
                .is_file();
            if !is_file {
                continue;
            }
            chmod(&path, 0o600).await?;
            protected_paths.push(path);
        }
    }
    Ok(protected_paths)
}

/// `enterToolSandbox(root, identity)`。
///
/// 只有设置了沙箱身份（`KIANA_EVAL_SANDBOX_UID` + `GID`）时才动作；未设置直接返回
/// `Ok(())`。非 unix 平台在设置了身份时返回
/// `The eval filesystem sandbox requires POSIX user APIs.`。成功返回后，**整个进程**
/// 已切换到沙箱 uid/gid（见 `enter_tool_sandbox_unix`）。
async fn enter_tool_sandbox(
    root: &Path,
    identity: Option<&SandboxIdentity>,
) -> Result<(), HarnessError> {
    let Some(identity) = identity else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        enter_tool_sandbox_unix(root, identity).await
    }
    #[cfg(not(unix))]
    {
        let _ = (root, identity);
        Err(HarnessError::new(
            "The eval filesystem sandbox requires POSIX user APIs.",
        ))
    }
}

/// POSIX 实现的降权流程。
///
/// 阶段与顺序（每一步失败都会立刻返回 Node 文案的错误，已生效的切换不回滚）：
/// 1. 前置检查：真实 uid 与有效 uid 都必须是 0，否则报
///    `The eval runner must start as root before entering the unprivileged tool sandbox.`；
/// 2. 仍以 root 身份准备文件系统：收紧 tmpdir 下的 transformed module 缓存，把临时 root
///    整棵树 chown 给沙箱身份（之后降权仍能读写自己的工作区）；
/// 3. 切换身份：`setgroups(0, NULL)` 清空附加组 → `setgid` → `setuid`；
/// 4. 复查：uid/euid 都必须等于目标 uid，并逐个尝试打开被保护的缓存文件——能读到就说明
///    隔离失败。
#[cfg(unix)]
async fn enter_tool_sandbox_unix(
    root: &Path,
    identity: &SandboxIdentity,
) -> Result<(), HarnessError> {
    // SAFETY: `getuid`/`geteuid` 无参数、无副作用。
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid != 0 || euid != 0 {
        return Err(HarnessError::new(
            "The eval runner must start as root before entering the unprivileged tool sandbox.",
        ));
    }

    // 阶段 2：文件系统准备必须在 setuid 之前完成（chown 需要 root 权限）。
    let protected_paths = protect_transformed_modules().await?;
    chown_tree(root, identity).await?;
    // 阶段 3：先清空附加组，再切 gid、最后切 uid；切换之后不再尝试改动组身份。
    // SAFETY: `setgroups(0, NULL)` 清空附加组；参数与 Linux/BSD 语义一致。
    let groups_result = unsafe { libc::setgroups(0, std::ptr::null()) };
    if groups_result != 0 {
        return Err(node_process_error(
            &std::io::Error::last_os_error(),
            "setgroups",
        ));
    }
    // SAFETY: 在已确认以 root 运行后切换 gid/uid。
    if unsafe { libc::setgid(identity.gid) } != 0 {
        return Err(node_process_error(
            &std::io::Error::last_os_error(),
            "setgid",
        ));
    }
    // SAFETY: 同上。
    if unsafe { libc::setuid(identity.uid) } != 0 {
        return Err(node_process_error(
            &std::io::Error::last_os_error(),
            "setuid",
        ));
    }
    // 阶段 4：降权后的自我检查。euid/uid 不符说明切换没完全生效；被保护的缓存能被读出
    // 内容说明隔离失败（PERMISSION_DENIED / NOT_FOUND 都算正常）。
    // SAFETY: `getuid`/`geteuid` 无参数、无副作用。
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid != identity.uid || euid != identity.uid {
        return Err(HarnessError::new(
            "Failed to enter the unprivileged eval tool sandbox.",
        ));
    }
    for path in protected_paths {
        use tokio::io::AsyncReadExt as _;
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(node_fs_error(&error, "open", Some(&path))),
        };
        let mut contents = Vec::new();
        match file.read_to_end(&mut contents).await {
            Ok(_) => {
                return Err(HarnessError::new(format!(
                    "The unprivileged eval process can read a transformed eval module: {}",
                    path.display()
                )));
            }
            Err(error) => return Err(node_fs_error(&error, "read", Some(&path))),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// runKianaCodingAgent
// ---------------------------------------------------------------------------

/// `estimatedCostUsd` 是否可观测：基础费率或任意 tier 的任一费率为正。
///
/// 全 0（或没有 tier）时 `run_kiana_coding_agent` 不写 `estimatedCostUsd`，因此没有价格
/// 信息的模型不会产生费用字段。
fn has_pricing(model: &Model) -> bool {
    let positive = |input: f64, output: f64, cache_read: f64, cache_write: f64| {
        input > 0.0 || output > 0.0 || cache_read > 0.0 || cache_write > 0.0
    };
    let cost = &model.cost;
    positive(cost.input, cost.output, cost.cache_read, cost.cache_write)
        || cost
            .tiers
            .iter()
            .flatten()
            .any(|tier| positive(tier.input, tier.output, tier.cache_read, tier.cache_write))
}

/// `target` 是否为 `^[A-Z][A-Z0-9_]*$`。
fn is_environment_variable_name(target: &str) -> bool {
    let mut bytes = target.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// JS 数字 → JSON 值。
///
/// 走 [`crate::report::js_number::format_js_number`]（ECMA-262 `Number::toString`），再解析回
/// `serde_json::Number`：整数写成 `0`/`1234`，而不是 `serde_json` 的 `0.0`/`1234.0`。
/// `NaN`/`±Infinity` 保持 `JSON.stringify` 的 `null`。
///
/// `serde_json::Number` 不保留原始文本，因此反序列化后再序列化会走 ryu；承载这些数值的
/// metadata 字段在 `report.rs` 里用自定义序列化器重新按 ECMAScript 格式写出
/// （`serialize_optional_js_metadata`），所以 `5e-6` 仍写成 `0.000005`。
fn js_number_value(value: f64) -> Value {
    serde_json::from_str(&crate::report::js_number::format_js_number(value)).unwrap_or(Value::Null)
}

/// 隐藏的凭证环境变量（cleanup 时恢复）。
struct HiddenCredentialEnvironment {
    name: String,
    value: OsString,
}

/// 执行一次 Kiana coding agent 评测。
///
/// TS: `harness.ts#runKianaCodingAgent`（逐行移植，见规范 §2.5.5）。
///
/// 输入：`input`（单个 prompt 或 step 数组）、`context`（中止信号 + artifacts）、`options`
/// 与可选的 `output` 回调。输出：`artifacts` 与 `traces` 尚未填充（前者由
/// `complete_harness_run` 从 context 快照合并，后者由 `ensure_run_trace` 补），`timings`
/// 已在本函数末尾补上。函数保证「成功才返回 Ok」：任何一步失败都会先走完下面
/// `finally` 段的清理，再把错误（可能聚合了 cleanup 错误）返回；此前已经收集到的诊断
/// 快照会以 partial run 的形式挂到错误上，供 `complete_harness_run` 复用。
async fn run_kiana_coding_agent(
    input: &KianaCodingAgentInput,
    context: &HarnessContext,
    options: &KianaCodingAgentHarnessOptions,
    output: Option<&NormalizedOutputFn>,
) -> Result<SimpleHarnessResult<JsonValue>, HarnessError> {
    // 阶段 1：失败也无需清理的准备工作——中止检查、模型选择、沙箱身份、临时根目录。
    // 顺序不能随意调整：沙箱身份读 KIANA_EVAL_SANDBOX_*，apply_isolated_environment
    // 会删除所有 KIANA_EVAL_ 前缀键，必须在它之前读取；host_agent_dir 也必须先取
    // （apply 会把 KIANA_CODING_AGENT_DIR 改写成隔离目录，而 get_agent_dir 正是读它）。
    // 模型选择读的 KIANA_PROVIDER/KIANA_MODEL 不受 apply 影响，保持在前只是为了
    // 与 TS 的读取顺序一致。
    let started_at = Instant::now();
    let signal = context.signal().cloned();
    throw_if_aborted(signal.as_ref())?;
    let selection = resolve_model_selection_from(
        options.model.as_ref(),
        &ModelSelectionEnvironment::from_process_env(),
    )?;
    let host_agent_dir = get_agent_dir().to_string_lossy().into_owned();
    let sandbox_identity = resolve_sandbox_identity()?;
    // Node 的 `mkdtemp(join(tmpdir(), "kiana-eval-"))` 在 libuv 里先生成带随机后缀的目录名
    // 再调用 `mkdir`；失败文案里的路径就是那个生成的名字。
    let temp_parent = node_tmpdir();
    let root = mkdtemp(&temp_parent, "kiana-eval-")
        .map_err(|(error, path)| node_fs_error(&error, "mkdtemp", Some(&path)))?;
    let workspace = root.join("workspace");
    let isolated_home = root.join("home");
    let agent_dir = isolated_home.join(".kiana").join("agent");

    let mut extension_factories: Vec<InlineExtension> = Vec::new();
    // 阶段 2：若配置了 system prompt 变换，注册一个隐藏的内联扩展。它在
    // BeforeAgentStart 事件里改写 system_prompt，并把结果存进 forced_system_prompt，
    // 供最终的 verify / sha256 使用（不依赖事后能否从会话消息里重新推导）。
    let forced_system_prompt: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    if let Some(transform) = options.transform_system_prompt.clone() {
        let slot = Arc::clone(&forced_system_prompt);
        extension_factories.push(InlineExtension::Named {
            name: "eval-system-prompt-transform".to_string(),
            hidden: Some(true),
            factory: Arc::new(move |kiana| {
                let transform = Arc::clone(&transform);
                let slot = Arc::clone(&slot);
                let handler = ExtensionHandler::Standard(Arc::new(
                    move |event,
                          _context|
                          -> LocalBoxFuture<
                        'static,
                        Result<
                            Option<ExtensionEventResult>,
                            Box<dyn std::error::Error + Send + Sync>,
                        >,
                    > {
                        let system_prompt = match &event {
                            ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent::User(
                                event,
                            )) => Some(event.system_prompt.clone()),
                            ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent::Custom(
                                event,
                            )) => Some(event.system_prompt.clone()),
                            _ => None,
                        };
                        let Some(default_prompt) = system_prompt else {
                            return Box::pin(async {
                                Ok::<
                                    Option<ExtensionEventResult>,
                                    Box<dyn std::error::Error + Send + Sync>,
                                >(None)
                            });
                        };
                        let forced = match transform(&default_prompt) {
                            Ok(forced) => forced,
                            Err(error) => {
                                return Box::pin(async move {
                                    Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
                                });
                            }
                        };
                        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
                            Some(forced.clone());
                        Box::pin(async move {
                            Ok(Some(ExtensionEventResult::BeforeAgentStart(
                                BeforeAgentStartEventResult {
                                    message: None,
                                    system_prompt: Some(forced),
                                },
                            )))
                        })
                    },
                ));
                let _unsubscribe = kiana.on(ExtensionEventKind::BeforeAgentStart, handler);
                Box::pin(async {})
            }),
            replaceable: None,
            builtin: None,
        });
    }

    // 阶段 3：跨 finally 段共享的状态。这些变量在 run 块结束后还要用于清理与错误组装，
    // 因此声明在 async 块之外；失败时由它们判断「哪些资源真的创建过」。
    let mut session_manager_created = false;
    let mut session: Option<AgentSession> = None;
    let mut result: Option<SimpleHarnessResult<JsonValue>> = None;
    let mut run_diagnostics: Option<SimpleHarnessResult<JsonValue>> = None;
    let mut run_error: Option<HarnessError> = None;
    let mut cleanup_errors: Vec<HarnessError> = Vec::new();
    let mut hidden_credential_environment: Option<HiddenCredentialEnvironment> = None;
    let isolated_home_string = isolated_home.to_string_lossy().into_owned();
    let agent_dir_string = agent_dir.to_string_lossy().into_owned();
    let restore_environment = apply_isolated_environment(&isolated_home_string, &agent_dir_string);
    let mut session_file: Option<String> = None;
    let expected_inline_paths: HashSet<String> = extension_factories
        .iter()
        .map(|extension| match extension {
            InlineExtension::Named { name, .. } => format!("<inline:{name}>"),
            InlineExtension::Factory(_) => String::new(),
        })
        .collect();

    // 主流程包在 async 块里：块内的 `?` 只会让它求值为 Err，下面的 finally 段仍然执行；
    // 这是 Rust 里模拟 `try/finally` 的方式。
    let run = async {
        // 阶段 4：凭证与模型运行时。宿主 auth.json 里的凭证被读进内存存储，后续即使磁盘
        // 文件在沙箱路径下被删除，本次 run 仍能鉴权。
        let auth_path = Path::new(&host_agent_dir).join("auth.json");
        let auth_path_string = auth_path.to_string_lossy().into_owned();
        let credentials = InMemoryCredentialStore::new();
        let stored_credential = read_stored_credential(&selection.provider, &auth_path_string);
        if let Some(credential) = stored_credential.clone() {
            credentials
                .modify(
                    &selection.provider,
                    Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })),
                    AuthOperationOptions::default(),
                )
                .await
                .map_err(|error| HarnessError::new(error.message))?;
        }
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(credentials) as Arc<dyn CredentialStore>),
            ..Default::default()
        })
        .await
        .map_err(|error| HarnessError::new(error.message))?;
        // 阶段 5：临时目录布局。workspace 是 agent 的 cwd；agent_dir 用 create_dir_all
        // 预建（HOME 已指向 isolated_home，SDK 会在这里找配置）；fixture 按插入序写入。
        tokio::fs::create_dir(&workspace)
            .await
            .map_err(|error| node_fs_error(&error, "mkdir", Some(&workspace)))?;
        tokio::fs::create_dir_all(&agent_dir)
            .await
            .map_err(|error| node_fs_error(&error, "mkdir", Some(&agent_dir)))?;
        seed_workspace(&workspace, &options.workspace_files).await?;
        // 阶段 6：模型与鉴权。模型不在 catalog 或拿不到鉴权都直接失败；只有凭证不是从
        // 宿主 auth.json 读到的时候，才把 api_key 写回运行时。
        let model = model_runtime
            .get_model(&selection.provider, &selection.id)
            .ok_or_else(|| {
                HarnessError::new(format!(
                    "Eval model not found: {}/{}",
                    selection.provider, selection.id
                ))
            })?;
        let auth = model_runtime
            .get_auth_for_model(&model, Default::default())
            .await
            .map_err(|error| HarnessError::new(error.message))?
            .ok_or_else(|| {
                HarnessError::new(format!(
                    "Eval model has no configured authentication: {}/{}",
                    selection.provider, selection.id
                ))
            })?;
        if stored_credential.is_none()
            && let Some(api_key) = auth.auth.api_key.as_deref()
        {
            model_runtime
                .set_runtime_api_key(
                    &selection.provider,
                    api_key,
                    AuthOperationOptions::default(),
                )
                .await
                .map_err(|error| HarnessError::new(error.message()))?;
        }
        // 阶段 7：沙箱路径下抹掉磁盘与环境中的凭证，只保留内存副本。
        // `host_agent_dir` 是在 apply_isolated_environment 之前取的，因此通常指向宿主机
        // 的 agent 目录；这里删除的 auth.json 在 cleanup 阶段不会重建——在宿主机上启用
        // 沙箱身份会删掉真实的 auth.json。
        if sandbox_identity.is_some() {
            match tokio::fs::remove_file(&auth_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(node_fs_error(&error, "unlink", Some(auth_path.as_path())));
                }
            }
            if let Some(source) = auth.source.as_deref()
                && is_environment_variable_name(source)
                && let Some(value) = std::env::var_os(source)
                && !value.is_empty()
            {
                hidden_credential_environment = Some(HiddenCredentialEnvironment {
                    name: source.to_string(),
                    value: value.clone(),
                });
                // SAFETY: runKianaCodingAgent 独占进程环境（与 TS 删除 process.env 的前提一致）。
                unsafe { std::env::remove_var(source) };
            }
        }

        // 阶段 8：创建 services 与会话。services 以 workspace 为 cwd，并注入上面登记的
        // 扩展工厂；会话文件放在临时 root 的 sessions 目录，避免污染宿主。
        let workspace_string = workspace.to_string_lossy().into_owned();
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: workspace_string.clone(),
            model_runtime: Some(model_runtime),
            resource_loader_options: Some(DefaultResourceLoaderOptions {
                extension_factories,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .map_err(|error| HarnessError::new(error.message))?;
        throw_if_aborted(signal.as_ref())?;
        let sessions_dir = root.join("sessions").to_string_lossy().into_owned();
        let session_manager = SessionManager::create(&workspace_string, Some(&sessions_dir), None)
            .map_err(|error| HarnessError::new(error.to_string()))?;
        session_manager_created = true;
        context.set_artifact(
            "runId",
            Value::String(session_manager.get_session_id().to_string()),
        );
        session_file = session_manager.get_session_file().map(str::to_string);
        let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
            services,
            session_manager,
            session_start_event: None,
            model: Some(model.clone()),
            thinking_level: Some(ModelThinkingLevel::Off),
            scoped_models: Vec::new(),
            tools: options.tools.clone(),
            exclude_tools: None,
            no_tools: options.no_tools,
            custom_tools: options.custom_tools.clone(),

            loop_guard: None,
            budget_config: None,
        })
        .await
        .map_err(|error| HarnessError::new(error.to_string()))?;
        session = Some(created.session);
        let session = session
            .as_ref()
            .expect("session was installed immediately above");

        // 阶段 9：确认隔离生效——只允许本模块注册的内联扩展；出现其它扩展即判定为隔离
        // 失败（错误里列出这些路径）。
        let unexpected_extensions: Vec<String> = session
            .extension_runner()
            .get_extension_paths()
            .into_iter()
            .filter(|path| !expected_inline_paths.contains(path))
            .collect();
        if !unexpected_extensions.is_empty() {
            return Err(HarnessError::new(format!(
                "Isolated eval loaded unexpected extensions: {}",
                unexpected_extensions.join(", ")
            )));
        }

        // 阶段 10：降权（仅在设置了沙箱身份时生效）。这一步不可逆，之后所有文件操作都以
        // 沙箱 uid/gid 执行，因此必须放在 workspace/agent_dir 建好、凭证读完之后。
        enter_tool_sandbox(&root, sandbox_identity.as_ref()).await?;

        let mut response: Option<String> = None;
        let steps = input.steps();
        let aborted = Arc::new(AtomicBool::new(false));
        // 阶段 11：注册中止监听。TS 的监听器只调用一次 `session.abort()` 并把 promise 存进
        // `abortPromise`；Rust 用同步的 `request_abort()` 触发取消，再在循环结束后用
        // `wait_for_idle()` 等待同一个 abort 完成（`abort()` = request_abort() + wait_for_idle()）。
        let abort_listener = signal.as_ref().map(|signal| {
            let session = session.clone();
            let aborted = Arc::clone(&aborted);
            signal.add_abort_listener(Arc::new(move || {
                aborted.store(true, Ordering::SeqCst);
                session.request_abort();
            }))
        });
        // 阶段 12：按顺序执行 step。Reload 重建会话的扩展运行器（不发送 prompt）；每个
        // Prompt 覆盖 `response`，因此最终输出取最后一次 prompt 的文本。
        let loop_result: Result<(), HarnessError> = async {
            for step in steps {
                match step {
                    KianaCodingAgentStep::Reload => {
                        session
                            .reload(None)
                            .await
                            .map_err(|error| HarnessError::new(error.to_string()))?;
                    }
                    KianaCodingAgentStep::Prompt { content } => {
                        response = Some(prompt_agent(session, &content, signal.as_ref()).await?);
                    }
                }
            }
            Ok(())
        }
        .await;
        // 监听器用完立即摘除；若中止确实触发过，等会话空闲后再让 loop_result 里的错误
        // 继续向上传播（避免请求已发出、内部任务还在收尾）。
        if let Some(listener) = abort_listener {
            listener.remove();
        }
        if aborted.load(Ordering::SeqCst) {
            session.wait_for_idle().await;
        }
        loop_result?;
        let response = response.ok_or_else(|| {
            HarnessError::new("Kiana eval input must include at least one prompt step.")
        })?;

        // 阶段 13：收集诊断。system prompt 优先用扩展记录的值；没有变换时从会话消息重新
        // 推导（convert_to_llm + get_current_system_prompt）。
        let forced_prompt = forced_system_prompt
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let messages = session.messages();
        let system_prompt =
            forced_prompt.unwrap_or_else(|| get_current_system_prompt(&convert_to_llm(&messages)));
        let stats = session.get_session_stats();
        let pricing = has_pricing(&model);
        let mut usage_metadata = Map::new();
        usage_metadata.insert(
            "cacheReadTokens".to_string(),
            js_number_value(stats.tokens.cache_read),
        );
        usage_metadata.insert(
            "cacheWriteTokens".to_string(),
            js_number_value(stats.tokens.cache_write),
        );
        if pricing {
            usage_metadata.insert("estimatedCostUsd".to_string(), js_number_value(stats.cost));
        }
        let usage = UsageSummary {
            provider: Some(model.provider.clone()),
            model: Some(model.id.clone()),
            input_tokens: Some(JsNumber(stats.tokens.input)),
            output_tokens: Some(JsNumber(stats.tokens.output)),
            reasoning_tokens: None,
            total_tokens: Some(JsNumber(stats.tokens.total)),
            tool_calls: Some(JsNumber(stats.tool_calls as f64)),
            retries: None,
            metadata: Some(usage_metadata),
        };
        let events = to_transcript_events(&messages);
        let mut metadata = Map::new();
        let mut hasher = Sha256::new();
        hasher.update(system_prompt.as_bytes());
        metadata.insert(
            "systemPromptSha256".to_string(),
            Value::String(hex::encode(hasher.finalize())),
        );
        // 先留一份不含 output 的诊断快照：后续 verify / output 回调失败时，失败 run 仍能
        // 带上 transcript、usage 与 systemPromptSha256。
        run_diagnostics = Some(SimpleHarnessResult {
            output: None,
            events: events.clone(),
            usage: usage.clone(),
            timings: None,
            metadata: Some(metadata.clone()),
        });

        // 文档变体校验放在 output 回调之前：prompt 不符合预期时不应产生 output。
        verify_system_prompt(&system_prompt, options)?;
        let output_value = match output {
            Some(callback) => {
                callback(KianaCodingAgentOutputArgs {
                    response: &response,
                    session,
                    system_prompt: &system_prompt,
                    agent_dir: &agent_dir_string,
                })
                .await?
            }
            None => Value::String(response.clone()),
        };
        // 到这里才认为本次 run 成功；timings 在本函数末尾（cleanup 之后）补。
        result = Some(SimpleHarnessResult {
            output: Some(output_value),
            events,
            usage,
            timings: None,
            metadata: Some(metadata),
        });
        Ok::<(), HarnessError>(())
    }
    .await;

    if let Err(error) = run {
        run_error = Some(error);
    }

    // finally
    // 清理顺序有依赖：先读会话快照，再 dispose，再删临时目录，最后恢复进程环境。
    // 1) 会话快照：必须在 dispose 与删除临时目录之前读。
    if session_manager_created {
        // TS 在 cleanup 时才调用 `sessionManager.getSessionFile()`；会话可能在运行期间被
        // 扩展命令替换（`new_session` / `switch_session`），因此优先取存活会话的当前路径。
        let current_session_file = match session.as_ref() {
            Some(session) => session.session_file(),
            // 会话创建前失败时管理器仍存在（持有初始路径），与 TS 的局部变量一致。
            None => session_file.clone(),
        };
        match current_session_file.as_deref() {
            Some(path) if Path::new(path).exists() => match node_read_file(Path::new(path)).await {
                Ok(bytes) => context.set_artifact(
                    KIANA_SESSION_SNAPSHOT_ARTIFACT,
                    Value::String(String::from_utf8_lossy(&bytes).into_owned()),
                ),
                Err(error) => cleanup_errors.push(error),
            },
            _ => cleanup_errors.push(HarnessError::new("Kiana eval produced no session file.")),
        }
    }
    // 2) 释放会话（停止后台任务），然后才删目录。
    if let Some(session) = session.as_ref() {
        session.dispose();
    }
    // 3) 删除临时 root（recursive + force，缺失不算错）。
    // TS: `rm(root, { recursive: true, force: true })`。
    if let Err(error) = node_rm_recursive(root.clone()).await {
        cleanup_errors.push(error);
    }
    // 4) 恢复进程环境与临时隐藏的凭证变量。注意 auth.json 文件本身不会恢复（见阶段 7）。
    restore_environment();
    if let Some(hidden) = hidden_credential_environment.take() {
        // SAFETY: 恢复此前隐藏的凭证环境变量（独占环境的同一前提）。
        unsafe { std::env::set_var(&hidden.name, &hidden.value) };
    }

    // 5) 合并运行错误与 cleanup 错误（规则是当前稳定契约）：
    //    - 运行失败 + 清理失败 → AggregateError，第一个 cause 是运行错误；
    //    - 只有清理失败：1 条用原错误，多条用 AggregateError；
    //    - 两者都没有 → failure 为 None，走成功路径。
    let mut failure = run_error;
    if let Some(run_failure) = failure.take() {
        if cleanup_errors.is_empty() {
            failure = Some(run_failure);
        } else {
            let mut causes = Vec::with_capacity(1 + cleanup_errors.len());
            causes.push(run_failure);
            causes.extend(cleanup_errors);
            failure = Some(HarnessError::aggregate(
                causes,
                "Agent run failed and cleanup also failed.",
            ));
        }
    } else if cleanup_errors.len() == 1 {
        failure = cleanup_errors.pop();
    } else if cleanup_errors.len() > 1 {
        failure = Some(HarnessError::aggregate(
            cleanup_errors,
            "Agent cleanup failed.",
        ));
    }

    // 失败时尽量把「失败前已收集的诊断」归一化成 partial run 挂到错误上；归一化只有
    // events 为空才会失败（有诊断时不可能），那时退回不带 run 的原始错误。
    if let Some(error) = failure {
        if let Some(diagnostics) = run_diagnostics {
            let partial = SimpleHarnessResult {
                output: None,
                events: diagnostics.events,
                usage: diagnostics.usage,
                timings: Some(TimingSummary {
                    total_ms: Some(JsNumber(elapsed_ms(started_at))),
                    metadata: None,
                }),
                metadata: diagnostics.metadata,
            };
            // TS 会在此处 `normalizeHarnessRun` 并 `attachHarnessRunToError`；只有当转录为空时
            // 归一化才会失败（诊断存在时不可能），失败则退回原始错误。
            if let Ok(mut run) = normalize_harness_run(partial, &HarnessArtifacts::new()) {
                // TS: `normalizeSimpleErrors`（`toJsonValue` 优先，Node SystemError 因此丢掉
                // type/message，只保留 errno/code/syscall/path）。
                run.errors.push(normalize_simple_error(&error));
                return Err(error.attach_run(run));
            }
        }
        return Err(error);
    }
    let mut result =
        result.ok_or_else(|| HarnessError::new("Kiana eval completed without a result."))?;
    // 成功路径的 timings 只在这里补：total_ms 覆盖整个 run（含 cleanup 与删除临时目录）。
    result.timings = Some(TimingSummary {
        total_ms: Some(JsNumber(elapsed_ms(started_at))),
        metadata: None,
    });
    Ok(result)
}

/// `performance.now() - startedAt` 的毫秒值。
fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// 构造入口
// ---------------------------------------------------------------------------

/// 创建 Kiana coding agent harness（无 `output` 回调）。
///
/// TS: `harness.ts#createKianaCodingAgentHarness`（无 output 的重载）
///
/// 缺省名 `kiana-coding-agent`；默认 `output` 是最后一次 prompt 的 assistant 文本
/// （`Value::String`）。模型/参数错误不会在构造期暴露，而是在 `run()` 时以「挂载了失败
/// run 的错误」返回。
pub fn create_kiana_coding_agent_harness(
    options: KianaCodingAgentHarnessOptions,
) -> Harness<KianaCodingAgentInput, String> {
    let name = options
        .name
        .clone()
        .unwrap_or_else(|| "kiana-coding-agent".to_string());
    let harness_name = name.clone();
    let options = Arc::new(options);
    Harness::new(
        name,
        move |input: KianaCodingAgentInput, context: HarnessContext| {
            let options = Arc::clone(&options);
            let harness_name = harness_name.clone();
            Box::pin(async move {
                // TS 的 `createHarness` 在 `options.run` 之前记录 startedAt，运行结束后才归一化
                // （因此能读到 `runId` / `kianaSessionJsonl` 等运行期 artifact）。
                let started_at = Utc::now();
                let outcome = run_kiana_coding_agent(&input, &context, &options, None).await;
                complete_harness_run(&harness_name, &input, &context, started_at, outcome)
            })
        },
    )
}

/// 创建 Kiana coding agent harness（带 `output` 回调）。
///
/// TS: `harness.ts#createKianaCodingAgentHarness`（有 output 的重载）
///
/// 回调返回值经 `serde_json::to_value` 变成 [`JsonValue`]；序列化失败会变成
/// `HarnessError`（`name = "Error"`，消息是 serde 的错误文本），沿普通失败路径返回。
/// `TOutput` 只需要 `Serialize + 'static`，不需要 `Send`（内部 future 是 `LocalBoxFuture`）。
pub fn create_kiana_coding_agent_harness_with_output<TOutput>(
    options: KianaCodingAgentHarnessWithOutput<TOutput>,
) -> Harness<KianaCodingAgentInput, TOutput>
where
    TOutput: Serialize + 'static,
{
    let name = options
        .options
        .name
        .clone()
        .unwrap_or_else(|| "kiana-coding-agent".to_string());
    let harness_name = name.clone();
    let base = Arc::new(options.options);
    let typed = options.output;
    let output: NormalizedOutputFn = Arc::new(move |args| {
        let future = typed(args);
        Box::pin(async move {
            let output = future.await;
            serde_json::to_value(output).map_err(|error| HarnessError::new(error.to_string()))
        })
    });
    Harness::new(
        name,
        move |input: KianaCodingAgentInput, context: HarnessContext| {
            let base = Arc::clone(&base);
            let output = Arc::clone(&output);
            let harness_name = harness_name.clone();
            Box::pin(async move {
                let started_at = Utc::now();
                let outcome = run_kiana_coding_agent(&input, &context, &base, Some(&output)).await;
                complete_harness_run(&harness_name, &input, &context, started_at, outcome)
            })
        },
    )
}

/// 创建文档评测 harness（无 `output` 回调）。
///
/// TS: `harness.ts#createKianaDocumentationEvalHarness`（无 output 的重载）。
///
/// `options.name` / `options.transform_system_prompt` /
/// `options.expected_kiana_documentation` 会被文档变体覆盖（TS 的类型 `Omit` 保证调用方
/// 不设置它们；Rust 用同结构体表达，因此在这里强制覆盖）。
///
/// 构造期先做容器沙箱校验（`KIANA_EVAL_CONTAINER=1` 且设置了
/// `KIANA_EVAL_SANDBOX_UID`/`GID`），不满足时直接返回错误，不会产生 harness。
pub fn create_kiana_documentation_eval_harness(
    options: KianaCodingAgentHarnessOptions,
) -> Result<Harness<KianaCodingAgentInput, String>, HarnessError> {
    documentation_options(options).map(create_kiana_coding_agent_harness)
}

/// 创建文档评测 harness（带 `output` 回调）。
///
/// TS: `harness.ts#createKianaDocumentationEvalHarness`（有 output 的重载）。
pub fn create_kiana_documentation_eval_harness_with_output<TOutput>(
    options: KianaCodingAgentHarnessWithOutput<TOutput>,
) -> Result<Harness<KianaCodingAgentInput, TOutput>, HarnessError>
where
    TOutput: Serialize + 'static,
{
    let KianaCodingAgentHarnessWithOutput { options, output } = options;
    let options = documentation_options(options)?;
    Ok(create_kiana_coding_agent_harness_with_output(
        KianaCodingAgentHarnessWithOutput { options, output },
    ))
}

/// 校验容器沙箱并应用文档变体选项。
///
/// 规则与顺序（当前稳定契约，改动需同步测试）：
/// 1. `KIANA_EVAL_CONTAINER` 必须等于 `"1"`，且沙箱身份必须已设置（两个环境变量都在），
///    否则返回 `Documentation evals must run in the isolated container sandbox.`；
/// 2. 变体取自 `KIANA_EVAL_VARIANT`；
/// 3. 强制覆盖四项：`name`（变体名）、`tools`（缺省为 [`DOCUMENTATION_EVAL_TOOLS`]，
///    显式空数组保持为空）、`transform_system_prompt`（`without_docs` 用
///    [`exclude_kiana_documentation`]，`with_docs` 直接清空）、
///    `expected_kiana_documentation`（是否 `with_docs`）。
fn documentation_options(
    options: KianaCodingAgentHarnessOptions,
) -> Result<KianaCodingAgentHarnessOptions, HarnessError> {
    // 容器标志与沙箱身份缺一不可：没有容器隔离就不允许跑文档评测。
    if std::env::var("KIANA_EVAL_CONTAINER").as_deref() != Ok("1")
        || resolve_sandbox_identity()?.is_none()
    {
        return Err(HarnessError::new(
            "Documentation evals must run in the isolated container sandbox.",
        ));
    }
    let variant = resolve_documentation_variant(None)?;
    let mut options = options;
    options.name = Some(variant.as_str().to_string());
    options.tools = Some(options.tools.take().unwrap_or_else(|| {
        DOCUMENTATION_EVAL_TOOLS
            .iter()
            .map(|tool| (*tool).to_string())
            .collect()
    }));
    options.transform_system_prompt = if variant == DocumentationVariant::WithoutDocs {
        // 对照组：在 BeforeAgentStart 时删掉 <docs> 段；with_docs 组清空变换，保持原样。
        Some(Arc::new(|prompt: &str| exclude_kiana_documentation(prompt)))
    } else {
        None
    };
    options.expected_kiana_documentation = Some(variant == DocumentationVariant::WithDocs);
    Ok(options)
}

// ---------------------------------------------------------------------------
// camelCase 别名（保持既有导出名，方便迁移调用方）
// ---------------------------------------------------------------------------

// 供从 TS 迁移过来的调用方使用：这些别名不是新 API，rustdoc 会直接展示目标项的文档，
// 维护时只改目标函数即可，别名不必重复注释。
pub use self::apply_isolated_environment as applyIsolatedEnvironment;
pub use self::create_kiana_coding_agent_harness as createKianaCodingAgentHarness;
pub use self::create_kiana_coding_agent_harness_with_output as createKianaCodingAgentHarnessWithOutput;
pub use self::create_kiana_documentation_eval_harness as createKianaDocumentationEvalHarness;
pub use self::create_kiana_documentation_eval_harness_with_output as createKianaDocumentationEvalHarnessWithOutput;
pub use self::exclude_kiana_documentation as excludeKianaDocumentation;
pub use self::resolve_documentation_variant as resolveDocumentationVariant;
pub use self::resolve_model_selection as resolveModelSelection;
pub use self::resolve_model_selection_from as resolveModelSelectionFrom;
pub use self::verify_system_prompt as verifySystemPrompt;

// ---------------------------------------------------------------------------
// 单元测试（本地等价物的细节；TS 的 vitest-evals 行为无法直接跑 Rust）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_result() -> SimpleHarnessResult<JsonValue> {
        SimpleHarnessResult {
            output: Some(Value::String("ok".to_string())),
            events: vec![TranscriptEvent::Message {
                role: TranscriptRole::User,
                content: Some(Value::String("hello".to_string())),
                metadata: None,
            }],
            usage: UsageSummary::default(),
            timings: None,
            metadata: None,
        }
    }

    /// `JSON.stringify` 里 token 计数与费用必须是 JS 数字文本（`0` 而不是 `0.0`）。
    #[test]
    fn js_number_values_use_javascript_number_formatting() {
        assert_eq!(serde_json::to_string(&js_number_value(0.0)).unwrap(), "0");
        assert_eq!(
            serde_json::to_string(&js_number_value(1234.0)).unwrap(),
            "1234"
        );
        assert_eq!(serde_json::to_string(&js_number_value(2.0)).unwrap(), "2");
        assert_eq!(serde_json::to_string(&js_number_value(0.5)).unwrap(), "0.5");
        assert_eq!(
            serde_json::to_string(&js_number_value(f64::NAN)).unwrap(),
            "null"
        );
        assert_eq!(
            serde_json::to_string(&js_number_value(f64::INFINITY)).unwrap(),
            "null"
        );
    }

    /// 回归：usage.metadata 的数值必须按 JS 数字文本序列化（`0` 而不是 `0.0`）。
    #[test]
    fn usage_metadata_numbers_serialize_like_javascript() {
        let mut metadata = Map::new();
        metadata.insert("cacheReadTokens".to_string(), js_number_value(0.0));
        metadata.insert("cacheWriteTokens".to_string(), js_number_value(2.0));
        metadata.insert("estimatedCostUsd".to_string(), js_number_value(0.0));
        let usage = UsageSummary {
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-5".to_string()),
            input_tokens: Some(JsNumber(1.0)),
            output_tokens: Some(JsNumber(2.0)),
            total_tokens: Some(JsNumber(3.0)),
            tool_calls: Some(JsNumber(0.0)),
            metadata: Some(metadata),
            ..Default::default()
        };
        let text = serde_json::to_string(&usage).expect("serialize usage");
        assert!(text.contains("\"cacheReadTokens\":0,"), "{text}");
        assert!(text.contains("\"cacheWriteTokens\":2,"), "{text}");
        assert!(text.contains("\"estimatedCostUsd\":0}"), "{text}");
        assert!(text.contains("\"inputTokens\":1,"), "{text}");
        assert!(text.contains("\"toolCalls\":0,"), "{text}");
    }

    /// 回归：harness run 的 artifacts 必须在 run 结束后读取（`runId` / `kianaSessionJsonl`
    /// 是在 runKianaCodingAgent 运行期间写入的）。
    #[test]
    fn complete_harness_run_merges_artifacts_recorded_during_the_run() {
        let input = KianaCodingAgentInput::Prompt("hello".to_string());
        let context = HarnessContext::new(None);
        context.set_artifact("runId", Value::String("session-1".to_string()));
        context.set_artifact(
            KIANA_SESSION_SNAPSHOT_ARTIFACT,
            Value::String("{}\n".to_string()),
        );
        let run = complete_harness_run(
            "kiana-coding-agent",
            &input,
            &context,
            Utc::now(),
            Ok(simple_result()),
        )
        .expect("successful harness run");
        let artifacts = run.artifacts.expect("live artifacts are merged");
        assert_eq!(
            artifacts.get("runId"),
            Some(&Value::String("session-1".to_string()))
        );
        assert_eq!(
            artifacts.get(KIANA_SESSION_SNAPSHOT_ARTIFACT),
            Some(&Value::String("{}\n".to_string()))
        );
        // vitest-evals 的 ensureRunTrace 会为每个 run 补一条 trace。
        let traces = run.traces.expect("ensureRunTrace populates traces");
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].name.as_deref(), Some("kiana-coding-agent"));
        assert_eq!(traces[0].spans.len(), 1);
        assert_eq!(traces[0].spans[0].kind, Some(SpanKind::Run));
        assert_eq!(traces[0].spans[0].status, Some(SpanStatus::Ok));
        assert!(traces[0].spans[0].error.is_none());
    }

    /// 回归：诊断快照之前的失败也要挂载合成 run（user message + errors + artifacts）。
    #[test]
    fn early_failures_attach_a_synthetic_failed_run() {
        let input = KianaCodingAgentInput::Prompt("hello".to_string());
        let context = HarnessContext::new(None);
        context.set_artifact("preset", Value::String("value".to_string()));
        let error = complete_harness_run(
            "kiana-coding-agent",
            &input,
            &context,
            Utc::now(),
            Err(HarnessError::new("boom")),
        )
        .expect_err("failed harness run");
        let run = error.run().expect("failed run is attached");
        assert_eq!(run.session.events.len(), 1);
        match &run.session.events[0] {
            TranscriptEvent::Message { role, content, .. } => {
                assert_eq!(*role, TranscriptRole::User);
                assert_eq!(content, &Some(Value::String("hello".to_string())));
            }
            other => panic!("expected a user message event, got {other:?}"),
        }
        assert_eq!(run.errors.len(), 1);
        assert_eq!(
            run.errors[0].get("type").and_then(Value::as_str),
            Some("Error")
        );
        assert_eq!(
            run.errors[0].get("message").and_then(Value::as_str),
            Some("boom")
        );
        assert_eq!(
            run.artifacts
                .as_ref()
                .and_then(|artifacts| artifacts.get("preset")),
            Some(&Value::String("value".to_string()))
        );
        let traces = run.traces.as_ref().expect("failed run has a trace");
        assert_eq!(traces[0].spans[0].status, Some(SpanStatus::Error));
        assert_eq!(
            traces[0].spans[0]
                .error
                .as_ref()
                .and_then(|error| error.r#type.as_deref()),
            Some("Error")
        );
    }

    /// TS 的 `seedWorkspace` 对非法 fixture 路径抛 `TypeError`。
    #[tokio::test]
    async fn workspace_fixture_path_errors_are_type_errors() {
        let directory = tempfile::tempdir().expect("workspace");
        let mut files = indexmap::IndexMap::new();
        files.insert(String::new(), "x".to_string());
        let error = seed_workspace(directory.path(), &files)
            .await
            .expect_err("empty fixture name must fail");
        assert_eq!(error.name(), "TypeError");

        let mut files = indexmap::IndexMap::new();
        files.insert("../escape.txt".to_string(), "x".to_string());
        let error = seed_workspace(directory.path(), &files)
            .await
            .expect_err("escaping fixture path must fail");
        assert_eq!(error.name(), "TypeError");
    }

    /// 文档段缺结束标记 → `Default Kiana system prompt has no complete Kiana documentation section.`
    #[test]
    fn incomplete_documentation_sections_are_rejected() {
        let error = exclude_kiana_documentation(
            "\n<rules>\n</rules>\n\n<docs>\nKiana documentation (read only",
        )
        .expect_err("missing </docs> must fail");
        assert_eq!(
            error.message(),
            "Default Kiana system prompt has no complete Kiana documentation section."
        );
    }

    /// TS 的 `resolveDocumentationVariant` 抛 `TypeError`。
    #[test]
    fn invalid_documentation_variant_is_a_type_error() {
        let error =
            resolve_documentation_variant(Some("other")).expect_err("unknown variant must fail");
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.message(),
            "KIANA_EVAL_VARIANT must be \"without_docs\" or \"with_docs\"."
        );
    }

    // -----------------------------------------------------------------------
    // 中止与 Node 错误文案的回归
    // -----------------------------------------------------------------------

    /// 回归：由 `CancellationToken` 构造的 `AbortScope` 没有显式 reason，但 TS 的
    /// `signal.throwIfAborted()` 抛的是 DOMException `AbortError: This operation was aborted`。
    #[test]
    fn aborted_token_scope_uses_the_node_abort_error_message() {
        let token = tokio_util::sync::CancellationToken::new();
        let scope = AbortScope::from(token.clone());
        token.cancel();
        let error = throw_if_aborted(Some(&scope)).expect_err("cancelled token aborts");
        assert_eq!(error.name(), "AbortError");
        assert_eq!(error.message(), "This operation was aborted");
    }

    /// 显式 reason（例如超时）保持 `name`/`message` 原样。
    #[test]
    fn explicit_abort_reasons_are_preserved() {
        let scope = AbortScope::new();
        scope.abort_with(AbortReason {
            name: "TimeoutError".to_string(),
            message: "timed out".to_string(),
        });
        let error = throw_if_aborted(Some(&scope)).expect_err("aborted scope fails");
        assert_eq!(error.name(), "TimeoutError");
        assert_eq!(error.message(), "timed out");
    }

    /// 回归：`seedWorkspace` 的文件系统错误使用 Node/libuv 文案（不是 Rust 的
    /// "Is a directory (os error 21)"）。对应 TS 里 `mkdir`/`writeFile` 的失败。
    #[tokio::test]
    async fn workspace_fixture_conflicts_use_node_fs_error_text() {
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = directory.path();

        // TS: entries "a"（文件）随后 "a/b" -> mkdir(dirname) 得到 EEXIST。
        let mut files = indexmap::IndexMap::new();
        files.insert("a".to_string(), "file".to_string());
        files.insert("a/b".to_string(), "nested".to_string());
        let error = seed_workspace(workspace, &files)
            .await
            .expect_err("file/child fixture conflict must fail");
        assert_eq!(
            error.message(),
            format!(
                "EEXIST: file already exists, mkdir '{}'",
                workspace.join("a").display()
            )
        );

        // TS: name "." 解析到 workspace 本身 -> writeFile 的 open 得到 EISDIR。
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = directory.path();
        let mut files = indexmap::IndexMap::new();
        files.insert(".".to_string(), "content".to_string());
        let error = seed_workspace(workspace, &files)
            .await
            .expect_err("directory fixture target must fail");
        assert_eq!(
            error.message(),
            format!(
                "EISDIR: illegal operation on a directory, open '{}'",
                workspace.display()
            )
        );
    }

    /// 回归：errno → Node/libuv `(code, description)` 的映射（EPERM 与 EACCES 都是
    /// `PermissionDenied`，必须按 errno 区分）。
    #[test]
    fn node_fs_error_text_matches_node_libuv() {
        let path = Path::new("/workspace/file");
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(21), "open", Some(path)).message(),
            "EISDIR: illegal operation on a directory, open '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(13), "open", Some(path)).message(),
            "EACCES: permission denied, open '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(1), "chown", Some(path)).message(),
            "EPERM: operation not permitted, chown '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(17), "mkdir", Some(path)).message(),
            "EEXIST: file already exists, mkdir '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(2), "open", Some(path)).message(),
            "ENOENT: no such file or directory, open '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(
                &std::io::Error::from_raw_os_error(20),
                "scandir",
                Some(path)
            )
            .message(),
            "ENOTDIR: not a directory, scandir '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(28), "write", None).message(),
            "ENOSPC: no space left on device, write"
        );
        // libuv 的 ENFILE 文案是 "file table overflow"（不是内核 strerror 的
        // "too many open files in system"）。
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(23), "open", Some(path)).message(),
            "ENFILE: file table overflow, open '/workspace/file'"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(11), "read", None).message(),
            "EAGAIN: resource temporarily unavailable, read"
        );
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(14), "open", Some(path)).message(),
            "EFAULT: bad address in system call argument, open '/workspace/file'"
        );
        // 未知 errno：uv_err_name → "UNKNOWN"，uv_strerror → "Unknown system error -N"。
        assert_eq!(
            node_fs_error(&std::io::Error::from_raw_os_error(10), "open", Some(path)).message(),
            "UNKNOWN: Unknown system error -10, open '/workspace/file'"
        );
        // Node 的 process.setuid/setgid/setgroups 文案没有 syscall 与路径。
        let process = node_process_error(&std::io::Error::from_raw_os_error(1), "setuid");
        assert_eq!(process.message(), "EPERM, Operation not permitted");
        assert_eq!(
            serde_json::to_string(process.own_properties.as_ref().unwrap()).unwrap(),
            r#"{"errno":1,"code":"EPERM","syscall":"setuid"}"#
        );
        let fs_error = node_fs_error(&std::io::Error::from_raw_os_error(2), "open", Some(path));
        assert_eq!(
            serde_json::to_string(fs_error.own_properties.as_ref().unwrap()).unwrap(),
            r#"{"errno":-2,"code":"ENOENT","syscall":"open","path":"/workspace/file"}"#
        );
    }

    /// 回归：`mkdtemp` 的失败文案包含 libuv 生成的 6 字符后缀，而不是模板 `XXXXXX`。
    #[test]
    fn mkdtemp_errors_include_the_generated_suffix() {
        let directory = tempfile::tempdir().expect("root");
        let missing_parent = directory.path().join("missing");
        let (error, path) = mkdtemp(&missing_parent, "kiana-eval-").expect_err("missing parent");
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let suffix = name.strip_prefix("kiana-eval-").expect("prefix");
        assert_eq!(suffix.len(), 6);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        assert!(!path.to_string_lossy().contains("XXXXXX"));
        assert_eq!(
            node_fs_error(&error, "mkdtemp", Some(&path)).message(),
            format!(
                "ENOENT: no such file or directory, mkdtemp '{}'",
                path.display()
            )
        );
    }

    /// 回归：`readFile` 的读取阶段错误标 `read`（`tokio::fs::read` 会把目录的 EISDIR
    /// 错标成 `open`）。
    #[cfg(unix)]
    #[tokio::test]
    async fn node_read_file_labels_read_phase_errors_like_node() {
        let directory = tempfile::tempdir().expect("root");
        let error = node_read_file(directory.path())
            .await
            .expect_err("reading a directory must fail");
        assert_eq!(
            error.message(),
            format!(
                "EISDIR: illegal operation on a directory, read '{}'",
                directory.path().display()
            )
        );
    }

    /// `normalizeSimpleErrors` / `normalizeSpanError`：SystemError 用自有可枚举属性序列化，
    /// 普通 Error 与 AggregateError 仍是 `{type, message}`。
    #[test]
    fn system_error_serialization_matches_vitest_evals() {
        let path = Path::new("/workspace/file");
        let error = node_fs_error(&std::io::Error::from_raw_os_error(2), "open", Some(path));
        let normalized = normalize_simple_error(&error);
        assert_eq!(
            serde_json::to_string(&normalized).unwrap(),
            r#"{"errno":-2,"code":"ENOENT","syscall":"open","path":"/workspace/file"}"#
        );
        // 合成 trace 的根 span：没有字符串 message 的对象走 serializeError 回退。
        let span_error = span_error_from_json(&normalized);
        assert_eq!(span_error.message, "[object Object]");
        assert_eq!(span_error.r#type.as_deref(), Some("Error"));
        assert!(span_error.fields.is_empty());

        let plain = normalize_simple_error(&HarnessError::new("boom"));
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"type":"Error","message":"boom"}"#
        );
        let aggregate = normalize_simple_error(&HarnessError::aggregate(
            vec![HarnessError::new("inner")],
            "Agent cleanup failed.",
        ));
        assert_eq!(
            serde_json::to_string(&aggregate).unwrap(),
            r#"{"type":"AggregateError","message":"Agent cleanup failed."}"#
        );
        let span_error = span_error_from_json(&plain);
        assert_eq!(span_error.message, "boom");
        assert_eq!(span_error.r#type.as_deref(), Some("Error"));
    }

    /// 回归：`usage.metadata` 的数字按 ECMAScript `Number::toString` 序列化
    /// （`JSON.stringify(5e-6) === "0.000005"`、`JSON.stringify(1e21) === "1e+21"`）。
    #[test]
    fn usage_metadata_numbers_use_ecmascript_formatting() {
        let mut metadata = Map::new();
        metadata.insert(
            "estimatedCostUsd".to_string(),
            serde_json::from_str::<Value>("5e-6").unwrap(),
        );
        metadata.insert(
            "cacheReadTokens".to_string(),
            serde_json::from_str::<Value>("1e21").unwrap(),
        );
        let usage = UsageSummary {
            metadata: Some(metadata),
            ..Default::default()
        };
        let text = serde_json::to_string(&usage).expect("serialize usage");
        assert!(text.contains(r#""estimatedCostUsd":0.000005"#), "{text}");
        assert!(text.contains(r#""cacheReadTokens":1e+21"#), "{text}");
    }

    /// `node_rm_recursive` 等价于 `rm(path, { recursive: true, force: true })`：
    /// 删除整棵树、忽略缺失路径，并且不跟随符号链接。
    #[cfg(unix)]
    #[tokio::test]
    async fn node_rm_recursive_matches_force_recursive_remove() {
        let directory = tempfile::tempdir().expect("root");
        let root = directory.path().join("root");
        tokio::fs::create_dir_all(root.join("nested"))
            .await
            .unwrap();
        tokio::fs::write(root.join("nested").join("file"), "x")
            .await
            .unwrap();
        let outside = directory.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("keep"), "x").await.unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        node_rm_recursive(root.clone()).await.expect("remove tree");
        assert!(!root.exists());
        assert!(outside.join("keep").exists(), "symlink target is untouched");

        // force: true → 不存在的路径不是错误。
        node_rm_recursive(directory.path().join("missing"))
            .await
            .expect("missing path is ignored");
    }
}
