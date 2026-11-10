//! `harness/live.ts`：`kiana.live` 文档——运行控制与当前 generation / 工具轮的展示。
//!
//! TS: `packages/durable/src/harness/live.ts`（284 行）。本模块同时给出两套视图：
//!
//! - **读**： [`LiveState`] 及其成员的 serde 类型，用于快照、测试与事件层；
//! - **写**： [`crate::types::DocDraft`] 上的 JSON 级辅助函数。`kiana.live` 的增量测试断言"一次提交只写一个字段"，
//!   所以写路径必须只触碰变化的叶子（替换整个槽对象会把整个窗口记进操作日志）。JSON 键名逐字
//!   对应 TS 的字段名（camelCase）。
//!
//! # 为什么是一份独立、易失的文档
//!
//! `kiana.live` 是每个会话一份的展示层：真正的事实（工具结果、嵌套调用结果、任务终态）在
//! 条目、任务记录与 `NestedCallDoc` 里，这里只保存客户端和事件快照需要的运行控制与进度。
//! 因此定义声明为 conversation 作用域、`history: Latest`、`fork: Initial`：只保留当前状态，
//! fork 出的会话用定义初始值（空对象）初始化，不继承源会话正在运行的 generation 或工具槽。
//!
//! # 在系统中的位置
//!
//! - 写入方：generation / tool / compaction 三类内建任务在各自提交里读改写本文件；
//!   调度器自行写 `faulted`/`orphaned` 终态时通过 [`settle_scheduler_outcome`] 清理。
//! - 读取方：会话视图把本文件作为五个内建文档之一挂载（`harness/view.rs`），事件快照用
//!   [`LiveState::from_document`] 解析它（`harness/events.rs`），解析失败退回默认值。
//!
//! # 数据形状与顺序不变量
//!
//! - 键名全部 camelCase；结构体的字段声明顺序就是序列化键序（测试
//!   `nested_tool_slot_serializes_in_ts_construction_order` 锁定嵌套槽的构造顺序）。
//! - `tools` 按本轮的调用顺序排列；`nestedTools` 按创建顺序排列且任务 ID 递增（[`tool_slot`]
//!   对嵌套槽的二分查找只依赖 `taskId` 递增，不读 `parentTaskId`）；父槽先于子槽是
//!   [`remove_nested_slots`] 一趟扩散删除整棵子树的依赖；`compactions` 按任务 ID 升序。
//! - `run` 只在忙碌期间存在；`generation` 只描述当前这一次尝试。
//!
//! # 写入流程
//!
//! 1. 任务在提交闭包里用 `tx.doc(DocTarget::conversation(&LIVE_DOC, conversation_id))`
//!    打开草稿，只增删改需要的叶子，再让 `TaskRuntime::commit` 结算。
//! 2. 工具进度用 [`PROGRESS_COMMIT`] 延迟持久化，并由 `settings.progress.outputIntervalMs`
//!    节流（`harness/tool.rs` 的 `publish_progress`）。
//! 3. 运行结束时 [`end_run`] 结清输入并清掉 run/generation/tools/nestedTools；调度器写终态
//!    时走 [`settle_scheduler_outcome`]。
//!
//! # 失败模式
//!
//! 槽位写入是"尽力而为"的：`tool_slot` 找不到槽位时，所有写入方都静默跳过（重复结算不会
//! 报错）。[`SlotRef`] 假定底层 JSON 是对象；除 [`SlotRef::value`]、[`SlotRef::value_mut`]
//! 和 [`SlotRef::is_nested`] 外，其它方法对非对象会 panic——槽位由本模块的辅助函数以及
//! generation/tool 的提交按对象形状写入（`harness/generation.rs` 的工具轮提交直接构造 `tools`
//! 槽位，`harness/tool.rs` 创建嵌套调用时直接构造 `NestedToolSlot`），[`SlotRef`] 只应包装
//! 这些写路径产生的槽位。

use std::sync::LazyLock;

use async_trait::async_trait;
use kiana_chord::context::Context;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

use crate::harness::types::{
    AnyTask, CompactionReason, ContextView, ContextViewOptions, ReportFrom, SchedulerOutcome,
    TaskRuntime, TaskRuntimeChange, ToolDiagnostic,
};
use crate::types::{
    BoxError, CommitOptions, CommitPersistence, ConversationId, DocDefinition, DocTarget, DocToken,
    DocumentFork, DocumentHistory, DocumentSemantics, EntryId, JsonObject, JsonValue, SubmissionId,
    SubmissionSettlement, TaskId, TaskOutcome, TaskRecord, TaskState, Tx, define_doc,
    id_from_number,
};

/// 一个运行中工具调用的状态（`SlotProgress.status`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotStatus {
    /// 排队，尚未开始（顺序轮）或即将开始。
    #[default]
    Pending,
    /// 正在执行。
    Running,
    /// 已结束。
    Done,
}

impl SlotStatus {
    /// TS 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            SlotStatus::Pending => "pending",
            SlotStatus::Running => "running",
            SlotStatus::Done => "done",
        }
    }

    /// 从 TS 字面量解析；未知值返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(SlotStatus::Pending),
            "running" => Some(SlotStatus::Running),
            "done" => Some(SlotStatus::Done),
            _ => None,
        }
    }
}

/// 模型发起的调用在当前轮里的展示（TS: `ToolSlot`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSlot {
    /// 调用 ID。
    pub call_id: String,
    /// 工具名。
    pub name: String,
    /// 状态。
    pub status: SlotStatus,
    /// 保留的运行输出；只在 running 期间发布，结算时由 [`finish_slot`] 清掉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 被边界丢弃的字节数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<u64>,
    /// 被边界丢弃的行数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_lines: Option<u64>,
    /// 最后一次 `details()` 的值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// `api.diagnostic()` 记录的诊断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// 尚未开始的顺序调用、以及请求未提供的调用（后者 `done` 且带 `entry`）没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// 完成时写；faulted/orphaned 时没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryId>,
}

/// 嵌套调用的结束摘要（TS: `NestedToolSummary`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolSummary {
    /// 是否是错误结果。
    pub is_error: bool,
    /// 本尝试的耗时（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// 本尝试的 spend。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<JsonValue>,
    /// 最多 500 个 UTF-16 code unit 的错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一个嵌套调用的展示（TS: `NestedToolSlot`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolSlot {
    /// 调用 ID：`<parentCallId>/<key>`。
    pub call_id: String,
    /// 父调用 ID。
    pub parent_call_id: String,
    /// 发起调用的工具任务。
    pub parent_task_id: TaskId,
    /// 工具名。
    pub name: String,
    /// 调用自己的工具任务。
    pub task_id: TaskId,
    /// 执行时使用的参数（修复、`beforeTool` 与强制转换之后）。
    pub arguments: JsonObject,
    /// 状态。
    pub status: SlotStatus,
    /// 保留的运行输出；只在 running 期间发布，结算时由 [`finish_slot`] 清掉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 被边界丢弃的字节数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<u64>,
    /// 被边界丢弃的行数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_lines: Option<u64>,
    /// 最后一次 `details()` 的值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// `api.diagnostic()` 记录的诊断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// 结束后的结局。结果本身在调用者的 `NestedCallDoc` 里，不写进槽位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<NestedToolSummary>,
}

// `TaskId` 没有实现 `Default`，所以这里手写：两个 ID 都用 `id_from_number(0)` 占位。
// 真实槽位由 tool.rs 的 start_nested_call 直接构造，ID 来自创建的子任务与调用方任务。
impl Default for NestedToolSlot {
    fn default() -> Self {
        NestedToolSlot {
            call_id: String::new(),
            parent_call_id: String::new(),
            parent_task_id: id_from_number(0),
            task_id: id_from_number(0),
            name: String::new(),
            arguments: JsonObject::new(),
            status: SlotStatus::Pending,
            output: None,
            dropped_bytes: None,
            dropped_lines: None,
            details: None,
            diagnostics: None,
            summary: None,
        }
    }
}

/// 一次持久退避（TS 的 `retry?: { at, error }`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRetry {
    /// 下次尝试的毫秒时间点（epoch 毫秒）。
    pub at: f64,
    /// 触发退避的错误文本。
    pub error: String,
}

/// 一个运行中生成任务的持久状态（`LiveState.generation`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGeneration {
    /// 尝试序号。
    pub attempt: i64,
    /// 已提交的节流 partial。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<JsonValue>,
    /// 持久退避。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<LiveRetry>,
    /// 正在轮询的 provider 侧延迟响应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<LiveDeferred>,
}

/// `generation.deferred`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveDeferred {
    /// 下次轮询的毫秒时间点。
    pub poll_at: f64,
}

/// `kiana.live.run`：运行控制。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRun {
    /// 结算输入的任务。
    pub task_id: TaskId,
    /// 本次运行的输入。
    pub inputs: Vec<SubmissionId>,
}

/// 一次压缩任务的状态（TS: `CompactionStatus`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStatus {
    /// 压缩任务。
    pub task_id: TaskId,
    /// 原因。
    pub reason: CompactionReason,
    /// 是否阻塞一个 generation。
    pub blocking: bool,
    /// 尝试序号。
    pub attempt: i64,
    /// 下一次摘要尝试前的持久退避。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<LiveRetry>,
}

/// 自上次产出摘要以来最新的一次压缩失败（TS: `CompactionFailure`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionFailure {
    /// 压缩任务。
    pub task_id: TaskId,
    /// 原因。
    pub reason: CompactionReason,
    /// 失败或 fault 的错误文本，或调度器孤立它的原因。
    pub error: String,
}

/// 内建 live 会话状态（TS: `LiveState`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    /// 运行控制：结清运行输入的任务与那些输入；恰好忙碌时存在。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<LiveRun>,
    /// 当前 generation 尝试的展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<LiveGeneration>,
    /// 当前工具轮，按调用顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSlot>>,
    /// 运行中工具的嵌套调用，按创建顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_tools: Option<Vec<NestedToolSlot>>,
    /// 活动的压缩任务，按任务 ID 升序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions: Option<Vec<CompactionStatus>>,
    /// 最新的压缩失败。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_failure: Option<CompactionFailure>,
}

impl LiveState {
    /// 从文档快照解析；形状不符时返回 `None`（TS 的读取点都是宽松的）。
    ///
    /// 所有成员都可缺省（serde `default`）：缺失的成员保持 `None`/默认值，未知成员被忽略。
    /// 但任一已知成员类型不符，或槽位缺少 `callId`/`name`/`status` 这类必填字段，都会让整个
    /// 解析失败并返回 `None`；事件层把 `None` 当作"没有 live 状态"处理（退回
    /// `LiveState::default()`），不会因此中断事件流。
    pub fn from_document(document: &JsonObject) -> Option<LiveState> {
        serde_json::from_value(object_to_value(document.clone())).ok()
    }

    /// 写回文档形状。
    ///
    /// 结构体的字段声明顺序决定序列化键序（camelCase 由 serde 属性给出）。本类型是结构体，
    /// serde 序列化结果恒为对象；`_` 分支只是防御（返回空对象），正常不会触发。
    pub fn into_document(self) -> JsonObject {
        match serde_json::to_value(self) {
            Ok(JsonValue::Object(object)) => object.into_iter().collect(),
            _ => JsonObject::new(),
        }
    }

    /// 是否有正在运行的槽（`checkpointWhen` 的条件之一）。
    ///
    /// `tools` 或 `nestedTools` 中任一槽的 `status == "running"` 即为真；列表缺失视为没有。
    /// 这里只做类型化判断，[`LIVE_DOC`] 的 `checkpointWhen` 在 JSON 层重复同一条件。
    pub fn has_running_slot(&self) -> bool {
        self.tools
            .as_ref()
            .is_some_and(|slots| slots.iter().any(|slot| slot.status == SlotStatus::Running))
            || self
                .nested_tools
                .as_ref()
                .is_some_and(|slots| slots.iter().any(|slot| slot.status == SlotStatus::Running))
    }
}

/// `kiana.live` 文档。
///
/// `checkpointWhen` 的规则（源码注释要求，性能基准会检查）：只要没有 generation 且没有 running
/// 的工具/嵌套槽，就必须存成完整 base。base 最多跨一个 generation 或一轮工具的重叠执行；槽位只在
/// running 时持有 output，所以 base 很小；嵌套结果在调用者的任务文档里，**不放这里**。不要加
/// "delta 数上限"之类的规则。
///
/// 实现细节：
///
/// - 判据只看当前值：`generation` 键存在（即使值是 null）就返回 `false`；`tools`/`nestedTools`
///   中只认 `status == "running"` 字面量，缺失或其它状态视为未运行。`ops`/`info` 参数未使用，
///   所以这不是"每 N 个 delta 强制 base"的规则。
/// - 对 delta 计划，返回 `true` 时被改写成完整 base，否则保持 delta（调用点在
///   `session/transaction.rs`）。
/// - kind 字符串 `"kiana.live"` 也被事件层按字面量查找（`harness/events.rs` 的
///   `view.docs.get("kiana.live")`），改 kind 会同时影响持久化记录与挂载。
pub static LIVE_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.live",
            1,
            DocumentSemantics {
                scope: crate::types::ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_checkpoint_when(|value, _ops, _info| {
            // 一次 generation 尝试正在进行：等它结束（成功或失败）后才允许存 base。
            if value.contains_key("generation") {
                return false;
            }
            // 工具轮里有任一槽在跑就不能存 base；只认 "running" 字面量。
            for member in ["tools", "nestedTools"] {
                if let Some(items) = value.get(member).and_then(JsonValue::as_array)
                    && items.iter().any(|slot| {
                        slot.get("status").and_then(JsonValue::as_str) == Some("running")
                    })
                {
                    return false;
                }
            }
            true
        }),
    )
});

/// 进度提交的持久化策略（TS: `PROGRESS_COMMIT`）：延迟存储，下一次读写 `kiana.live` 的提交把它一并
/// 落盘，或在 `progress.persistWithinMs` 后由定时器落盘。
///
/// `within_ms: None` 表示延迟上限取 `settings.progress.persistWithinMs`（`session/session.rs`
/// 的 `deferral`）；不带 `persistence` 的普通提交仍在 `commit` 返回前写入。
pub const PROGRESS_COMMIT: CommitOptions = CommitOptions {
    persistence: Some(CommitPersistence::Deferred { within_ms: None }),
};

/// 能拥有 `kiana.live.run` 的内建任务种类（TS: `RUN_TASK_KINDS`）。
///
/// 只有 generation 结算运行输入；tool/compaction 任务不拥有 `run`。这些字符串同时是任务
/// 定义名与持久记录里的 `kind`，改名会让已有任务记录无法识别。
pub const RUN_TASK_KINDS: &[&str] = &["kiana.generation"];
/// 工具任务种类（任务定义名与持久记录的 `kind`，不能随意改名）。
pub const TOOL_TASK_KIND: &str = "kiana.tool";
/// 压缩任务种类（任务定义名与持久记录的 `kind`，不能随意改名）。
pub const COMPACTION_TASK_KIND: &str = "kiana.compaction";

// ---------------------------------------------------------------------------
// JSON draft helpers
// ---------------------------------------------------------------------------

const TOOLS: &str = "tools";
const NESTED_TOOLS: &str = "nestedTools";
const COMPACTIONS: &str = "compactions";

/// `serde_json::Map` 的具体类型。
///
/// 工作区的 [`JsonObject`] 是 `IndexMap<String, JsonValue>`（键序规则见 `kiana_ai::types`），而
/// `serde_json::Value::Object` 装的是 `serde_json::Map<String, JsonValue>`；两者在
/// `preserve_order` 下都按插入序，这里只做容器类型的区分。
pub type JsonMap = serde_json::Map<String, JsonValue>;

/// 把 `JsonObject`（`IndexMap`）转成 JSON 值。
///
/// 工作区里 `JsonObject = IndexMap<String, JsonValue>`（见 `kiana_ai::types`），而
/// `serde_json::Value::Object` 装的是 `serde_json::Map`；两者在 `preserve_order` 下都是插入序，
/// 这里只做容器类型的转换。
pub fn object_to_value(object: JsonObject) -> JsonValue {
    JsonValue::Object(object.into_iter().collect())
}

/// 把 JSON 对象值深拷贝成 `JsonObject`。
pub fn value_to_object(value: &JsonValue) -> Option<JsonObject> {
    value.as_object().map(|map| {
        map.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    })
}

/// 对象成员的可变借用。
pub fn member_mut<'a>(object: &'a mut JsonObject, key: &str) -> Option<&'a mut JsonValue> {
    object.get_mut(key)
}

/// 确保 `JsonObject` 成员是对象并返回它（`serde_json::Map` 视图）。
///
/// 成员缺失时插入空对象；成员存在但不是对象时**覆盖**成空对象（旧值丢失）。返回的引用
/// 保证是对象，因此内部的 `expect` 不会失败。
pub fn object_member<'a>(object: &'a mut JsonObject, key: &str) -> &'a mut JsonMap {
    let slot = object
        .entry(key.to_owned())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    if !slot.is_object() {
        *slot = JsonValue::Object(JsonMap::new());
    }
    slot.as_object_mut().expect("set to object above")
}

/// 确保对象成员是数组并返回它。
///
/// 与 [`object_member`] 同一规则：成员缺失或类型不符都会被替换成空数组。
pub fn array_member<'a>(object: &'a mut JsonMap, key: &str) -> &'a mut Vec<JsonValue> {
    let slot = object
        .entry(key.to_owned())
        .or_insert_with(|| JsonValue::Array(Vec::new()));
    if !slot.is_array() {
        *slot = JsonValue::Array(Vec::new());
    }
    slot.as_array_mut().expect("set to array above")
}

/// 确保 `JsonObject` 成员是数组并返回它。
///
/// 它是 [`array_member`] 的 `JsonObject` 入口：缺失或类型不符时创建空数组。
pub fn object_array_member<'a>(object: &'a mut JsonObject, key: &str) -> &'a mut Vec<JsonValue> {
    let slot = object
        .entry(key.to_owned())
        .or_insert_with(|| JsonValue::Array(Vec::new()));
    if !slot.is_array() {
        *slot = JsonValue::Array(Vec::new());
    }
    slot.as_array_mut().expect("set to array above")
}

/// 读取对象成员的整数（ID）。
pub fn int_member(object: &JsonMap, key: &str) -> Option<i64> {
    object.get(key).and_then(JsonValue::as_i64)
}

/// 读取对象成员的字符串。
pub fn str_member<'a>(object: &'a JsonMap, key: &str) -> Option<&'a str> {
    object.get(key).and_then(JsonValue::as_str)
}

/// 用 `&str` 写对象成员。
pub fn set_str(object: &mut JsonMap, key: &str, text: &str) {
    object.insert(key.to_owned(), JsonValue::String(text.to_owned()));
}

/// 用 `i64` 写对象成员。
pub fn set_int(object: &mut JsonMap, key: &str, value: i64) {
    object.insert(key.to_owned(), JsonValue::from(value));
}

/// 用一个 JSON 值写对象成员。
pub fn set_json(object: &mut JsonMap, key: &str, value: JsonValue) {
    object.insert(key.to_owned(), value);
}

/// UTF-16 code unit 计数（TS 的 `string.length`）。
///
/// 不能用 `str::len()`（UTF-8 字节）或 `chars().count()`（Unicode 标量）：JS 的字符串长度
/// 按 UTF-16 code unit 计，非 BMP 字符占 2 个。这里的计数被两处按这一长度语义使用：
/// `harness/json.rs` 的 append 计划长度校验，以及 `harness/compaction.rs` 的
/// `truncate_chars`（按 code unit 计数与定位；截断点落在代理对中间时，`from_utf16_lossy`
/// 会把落单的代理项替换成 U+FFFD，而 JS 的 `slice` 保留孤立代理项，两者并不完全一致，
/// 详见该函数的文档）。
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

// ---------------------------------------------------------------------------
// Slots
// ---------------------------------------------------------------------------

/// 一个槽位（模型发起或嵌套）的可变视图。
///
/// TS 用 `ToolSlot | NestedToolSlot` 的联合与 `"parentTaskId" in slot` 区分；这里用
/// [`SlotRef::is_nested`] 表达同一判别。
///
/// 包装时不校验形状：除 [`SlotRef::value`]、[`SlotRef::value_mut`] 与 [`SlotRef::is_nested`]
/// 外，其它方法都假定它是 JSON 对象并会 `expect`。调用方应只包装 `tool_slot` 返回的槽位。
pub struct SlotRef<'a> {
    value: &'a mut JsonValue,
}

impl<'a> SlotRef<'a> {
    /// 包装一个槽对象。
    pub fn new(value: &'a mut JsonValue) -> Self {
        SlotRef { value }
    }

    /// 底层 JSON。
    pub fn value(&self) -> &JsonValue {
        self.value
    }

    /// 底层 JSON（可变）。
    pub fn value_mut(&mut self) -> &mut JsonValue {
        self.value
    }

    /// 是否是嵌套槽（TS 的 `"parentTaskId" in slot`）。
    pub fn is_nested(&self) -> bool {
        self.value
            .as_object()
            .is_some_and(|object| object.contains_key("parentTaskId"))
    }

    fn object(&self) -> &JsonMap {
        self.value.as_object().expect("a slot is an object")
    }

    fn object_mut(&mut self) -> &mut JsonMap {
        self.value.as_object_mut().expect("a slot is an object")
    }

    /// 状态。
    ///
    /// 成员缺失或字面量未知时返回 [`SlotStatus::Pending`]（`Default`）。
    pub fn status(&self) -> SlotStatus {
        str_member(self.object(), "status")
            .and_then(SlotStatus::parse)
            .unwrap_or_default()
    }

    /// 设置状态。
    pub fn set_status(&mut self, status: SlotStatus) {
        set_str(self.object_mut(), "status", status.as_str());
    }

    /// 任务 ID（可能存在、可能缺席）。
    ///
    /// 成员缺失或不是整数时返回 `None`；顺序轮里尚未开始的调用就是这种状态。
    pub fn task_id(&self) -> Option<TaskId> {
        int_member(self.object(), "taskId").map(id_from_number)
    }

    /// 设置任务 ID。
    pub fn set_task_id(&mut self, task_id: TaskId) {
        set_int(self.object_mut(), "taskId", task_id.get());
    }

    /// 结果条目 ID。
    pub fn entry(&self) -> Option<EntryId> {
        int_member(self.object(), "entry").map(id_from_number)
    }

    /// 设置结果条目 ID。
    pub fn set_entry(&mut self, entry: EntryId) {
        set_int(self.object_mut(), "entry", entry.get());
    }

    /// 嵌套调用的参数。
    ///
    /// 只有嵌套槽写这个成员；缺失或不是对象时返回 `None`。
    pub fn arguments(&self) -> Option<&JsonMap> {
        self.object()
            .get("arguments")
            .and_then(JsonValue::as_object)
    }

    /// 设置嵌套调用的参数。
    pub fn set_arguments(&mut self, arguments: JsonObject) {
        set_json(self.object_mut(), "arguments", object_to_value(arguments));
    }

    /// 设置运行输出（整个字符串字段；Chord 会差分成 append）。
    pub fn set_output(&mut self, text: &str) {
        set_str(self.object_mut(), "output", text);
    }

    /// 运行输出。
    pub fn output(&self) -> Option<&str> {
        str_member(self.object(), "output")
    }

    /// 设置被丢弃的字节数。
    pub fn set_dropped_bytes(&mut self, dropped: u64) {
        set_int(self.object_mut(), "droppedBytes", dropped as i64);
    }

    /// 设置被丢弃的行数。
    pub fn set_dropped_lines(&mut self, dropped: u64) {
        set_int(self.object_mut(), "droppedLines", dropped as i64);
    }

    /// 被丢弃的字节数。
    ///
    /// 成员缺失或为负数时返回 0（负数归零而不是回绕）。
    pub fn dropped_bytes(&self) -> u64 {
        int_member(self.object(), "droppedBytes")
            .unwrap_or(0)
            .max(0) as u64
    }

    /// 被丢弃的行数。
    ///
    /// 成员缺失或为负数时返回 0。
    pub fn dropped_lines(&self) -> u64 {
        int_member(self.object(), "droppedLines")
            .unwrap_or(0)
            .max(0) as u64
    }

    /// details。
    pub fn details(&self) -> Option<&JsonValue> {
        self.object().get("details")
    }

    /// 设置 details。
    pub fn set_details(&mut self, details: JsonValue) {
        set_json(self.object_mut(), "details", details);
    }

    /// 诊断列表。
    ///
    /// 每次都克隆并重新解析；成员缺失或形状不符时返回空列表，不报错。
    pub fn diagnostics(&self) -> Vec<ToolDiagnostic> {
        self.object()
            .get("diagnostics")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    /// 追加一条诊断（槽位没有列表时创建）。
    ///
    /// `diagnostics` 成员不是数组时会被替换成空数组再追加（见 `array_member`），所以损坏的
    /// 旧值不会保留。
    pub fn push_diagnostic(&mut self, diagnostic: &ToolDiagnostic) {
        let value = serde_json::to_value(diagnostic).expect("diagnostic serializes");
        array_member(self.object_mut(), "diagnostics").push(value);
    }

    /// 设置摘要。
    ///
    /// 已存在的 `summary` 被整个替换；字段顺序由 [`NestedToolSummary`] 的声明顺序决定。
    pub fn set_summary(&mut self, summary: &NestedToolSummary) {
        let value = serde_json::to_value(summary).expect("summary serializes");
        set_json(self.object_mut(), "summary", value);
    }
}

/// 结束一个槽：标记 `done` 并清掉运行时进度。
///
/// 调用者随后设置模型调用的结果 `entry` 或嵌套调用的 `summary`；任务 faulted/orphaned 时都不设。
/// 只改 `status` 与进度字段，`callId`/`taskId`/`arguments` 等标识不动。
pub fn finish_slot(slot: &mut SlotRef<'_>) {
    slot.set_status(SlotStatus::Done);
    clear_progress(slot);
}

/// 清掉一个工具在运行时发布的进度；它的结果条目或重跑会替换。
///
/// 只删除 `output`、`droppedBytes`、`droppedLines`、`details`、`diagnostics` 五个键；
/// `status`、`entry`、`summary` 等结算字段不受影响。
pub fn clear_progress(slot: &mut SlotRef<'_>) {
    let object = slot.object_mut();
    object.remove("output");
    object.remove("droppedBytes");
    object.remove("droppedLines");
    object.remove("details");
    object.remove("diagnostics");
}

/// 槽位 `taskId`（模型发起或嵌套）的可变视图，若 `kiana.live` 仍列出它。
///
/// 嵌套槽用二分查找：一个工具可能发起上千个嵌套调用，每次提交多次查槽位，而每个被读的草稿元素
/// 都会在 Chord change tracker 里产生节点。它们的任务 ID 递增（每个槽在创建任务的提交里追加，
/// 任务 ID 随提交顺序递增，删除保持顺序）。
///
/// 查找顺序是先在 `tools` 上线性扫描，再在 `nestedTools` 上二分。`tools` 缺失或不是数组时
/// 直接跳到嵌套列表；`nestedTools` 缺失/类型不符，或二分途中遇到缺少 `taskId` 的嵌套槽，
/// 都返回 `None`（列表不变量已破坏，不做猜测）。
pub fn tool_slot<'a>(live: &'a mut JsonObject, task_id: TaskId) -> Option<SlotRef<'a>> {
    let target = task_id.get();
    // 模型发起的槽只覆盖"当前工具轮"，数量小；按调用顺序线性扫描。
    let model_index = live
        .get(TOOLS)
        .and_then(JsonValue::as_array)
        .and_then(|items| {
            items
                .iter()
                .position(|slot| slot.get("taskId").and_then(JsonValue::as_i64) == Some(target))
        });
    if let Some(index) = model_index {
        let items = live.get_mut(TOOLS)?.as_array_mut()?;
        return Some(SlotRef::new(&mut items[index]));
    }
    // 嵌套槽按 taskId 升序；二分只读 O(log n) 个元素，避免为查找在 change tracker 里生成
    // 大量节点（见函数文档）。
    let nested_index = {
        let nested = live.get(NESTED_TOOLS)?.as_array()?;
        let mut low = 0isize;
        let mut high = nested.len() as isize - 1;
        let mut found = None;
        while low <= high {
            let middle = (low + high) >> 1;
            match nested[middle as usize]
                .get("taskId")
                .and_then(JsonValue::as_i64)
            {
                Some(value) if value == target => {
                    found = Some(middle as usize);
                    break;
                }
                Some(value) if value < target => low = middle + 1,
                Some(_) => high = middle - 1,
                // 缺少 `taskId`：列表不变量已破坏，按未找到处理。
                None => break,
            }
        }
        found
    };
    let index = nested_index?;
    let nested = live.get_mut(NESTED_TOOLS)?.as_array_mut()?;
    Some(SlotRef::new(&mut nested[index]))
}

/// 当前工具轮的槽列表（写路径）。
///
/// 只取不建：`tools` 缺失或不是数组时返回 `None`（`tools` 键由 generation 的工具轮提交创建）。
pub fn tools_mut(live: &mut JsonObject) -> Option<&mut Vec<JsonValue>> {
    live.get_mut(TOOLS).and_then(JsonValue::as_array_mut)
}

/// 确保嵌套槽列表存在并返回它。
///
/// 缺失或不是数组时创建空数组。键缺失时新键追加到 `kiana.live` 的键序末尾；键已存在但
/// 值不是数组时由 [`object_array_member`] 原位替换该值，键序不变。
pub fn nested_tools_mut(live: &mut JsonObject) -> &mut Vec<JsonValue> {
    object_array_member(live, NESTED_TOOLS)
}

/// 按调用 ID 找当前轮里还没有任务的槽（TS: `slot.callId === next && slot.taskId === undefined`）。
///
/// 返回槽本身的裸 JSON 引用（不是 [`SlotRef`]）；匹配"没有任务"时同时接受成员缺失与 `null`。
pub fn pending_tool_slot<'a>(live: &'a mut JsonObject, call_id: &str) -> Option<&'a mut JsonValue> {
    let items = live.get_mut(TOOLS)?.as_array_mut()?;
    items.iter_mut().find(|slot| {
        slot.get("callId").and_then(JsonValue::as_str) == Some(call_id)
            && slot.get("taskId").is_none_or(JsonValue::is_null)
    })
}

/// 移除工具任务 `task_id` 之下（传递地）的嵌套槽；列表里父先于子。
///
/// 依赖"父槽先于子槽"的创建序：第一遍按顺序扩散后代，若顺序被破坏，孙槽可能残留。
/// 列表清空后删掉 `nestedTools` 键，而不是留一个空数组。
pub fn remove_nested_slots(live: &mut JsonObject, task_id: TaskId) {
    let Some(nested) = live.get_mut(NESTED_TOOLS).and_then(JsonValue::as_array_mut) else {
        return;
    };
    let mut removed = std::collections::HashSet::new();
    removed.insert(task_id.get());
    // 第一遍：按列表顺序扩散。父先于子，所以一趟就能把整棵子树标进 removed。
    for slot in nested.iter() {
        let parent = slot.get("parentTaskId").and_then(JsonValue::as_i64);
        if let Some(parent) = parent
            && removed.contains(&parent)
            && let Some(id) = slot.get("taskId").and_then(JsonValue::as_i64)
        {
            removed.insert(id);
        }
    }
    // 第二遍：从后往前删；已检查过的后缀不会因为前面的删除而再次移动。
    let mut index = nested.len();
    while index > 0 {
        index -= 1;
        let parent = nested[index]
            .get("parentTaskId")
            .and_then(JsonValue::as_i64);
        if parent.is_some_and(|parent| removed.contains(&parent)) {
            nested.remove(index);
        }
    }
    if nested.is_empty() {
        live.shift_remove(NESTED_TOOLS);
    }
}

// ---------------------------------------------------------------------------
// Run / compaction status
// ---------------------------------------------------------------------------

/// 结束 `task_id` 拥有的运行：结清它的每个输入并移除 `run`。
///
/// 总是移除 `generation`、`tools` 与 `nestedTools`，它们的展示属于正在结束的运行。
///
/// `run` 的归属只决定是否结清输入与删除 `run`；`generation`/`tools`/`nestedTools` 无论如何
/// 都删除（TS 相同）。本 crate 的调用点只有正在结束的那次 generation 的提交，以及
/// [`settle_scheduler_outcome`] 里已确认拥有 `run` 的路径，所以不会误删别的运行。
///
/// 每个输入单独克隆 `settlement` 结清；`inputs` 中不是整数的元素被忽略。删除键用
/// `shift_remove` 而非 `swap_remove`，其余键的插入顺序保持不变。
pub fn end_run(
    tx: &dyn Tx,
    live: &mut JsonObject,
    task_id: TaskId,
    settlement: &SubmissionSettlement,
) {
    let owned = live
        .get("run")
        .and_then(|run| run.get("taskId"))
        .and_then(JsonValue::as_i64)
        == Some(task_id.get());
    if owned
        && let Some(run) = live.get("run").and_then(JsonValue::as_object)
        && let Some(inputs) = run.get("inputs").and_then(JsonValue::as_array)
    {
        for input in inputs {
            if let Some(id) = input.as_i64() {
                tx.settle_submission(id_from_number(id), settlement.clone());
            }
        }
    }
    if owned {
        live.shift_remove("run");
    }
    // 展示属于刚刚结束的运行；即使本任务不拥有 run 也与 TS 一样清掉。
    live.shift_remove("generation");
    live.shift_remove(TOOLS);
    live.shift_remove(NESTED_TOOLS);
}

/// 追加一个本次提交里创建的压缩状态；状态按任务 ID 升序。
///
/// 只做 `push`：顺序由调用方保证——在创建压缩任务的同一提交里按任务创建顺序调用
/// （任务 ID 随创建顺序递增）。
pub fn add_compaction_status(live: &mut JsonObject, status: &CompactionStatus) {
    let value = serde_json::to_value(status).expect("compaction status serializes");
    object_array_member(live, COMPACTIONS).push(value);
}

/// 列出压缩任务 `task_id` 的状态（可变）。
///
/// 线性查找；返回裸 JSON（调用方按需读写 `attempt`/`retry` 等字段）。列表缺失或没有该任务
/// 时返回 `None`。
pub fn compaction_status(live: &mut JsonObject, task_id: TaskId) -> Option<&mut JsonValue> {
    let statuses = live.get_mut(COMPACTIONS)?.as_array_mut()?;
    statuses
        .iter_mut()
        .find(|status| status.get("taskId").and_then(JsonValue::as_i64) == Some(task_id.get()))
}

/// 移除压缩任务 `task_id` 的状态；列表空了就删掉列表。
///
/// 状态列表为空时删除整个 `compactions` 键（缺席即"没有活动压缩"），而不是留下空数组。
pub fn remove_compaction_status(live: &mut JsonObject, task_id: TaskId) {
    let Some(statuses) = live.get_mut(COMPACTIONS).and_then(JsonValue::as_array_mut) else {
        return;
    };
    if let Some(index) = statuses
        .iter()
        .position(|status| status.get("taskId").and_then(JsonValue::as_i64) == Some(task_id.get()))
    {
        statuses.remove(index);
    }
    if statuses.is_empty() {
        live.shift_remove(COMPACTIONS);
    }
}

/// 移除压缩任务 `task_id` 的状态并记录它失败于 `error`。
///
/// 只有状态还在列表里时才写 `compactionFailure`（从状态复制 `taskId` 与 `reason`）；找不到
/// 状态则不写失败记录。记录字段顺序为 `taskId`、`reason`、`error`。
pub fn fail_compaction_status(live: &mut JsonObject, task_id: TaskId, error: &str) {
    let failure = compaction_status(live, task_id).map(|status| {
        (
            status
                .get("taskId")
                .and_then(JsonValue::as_i64)
                .unwrap_or(0),
            status.get("reason").cloned().unwrap_or(JsonValue::Null),
        )
    });
    if let Some((id, reason)) = failure {
        // 手动构造而不用 serde：CompactionFailure 的字段顺序要与 TS 的 `{ taskId, reason, error }`
        // 一致，这里显式按该顺序插入。
        let mut record = JsonObject::new();
        record.insert("taskId".to_owned(), JsonValue::from(id));
        record.insert("reason".to_owned(), reason);
        record.insert("error".to_owned(), JsonValue::String(error.to_owned()));
        live.insert("compactionFailure".to_owned(), object_to_value(record));
    }
    remove_compaction_status(live, task_id);
}

// ---------------------------------------------------------------------------
// Scheduler terminal outcomes
// ---------------------------------------------------------------------------

/// 调度器自己写终态（`faulted`/`orphaned`）时的 Harness 清理。
///
/// 运行任务结束它的运行；工具任务的槽标成 `done` 但不带 `entry`（上下文派生会合成缺失的结果）；
/// 压缩任务的状态被移除并记录失败。其它 kind 直接忽略，绝不在别的会话创建 `kiana.live`。
/// 调度器不知道任务 kind，由 Harness 注入（spec §5.4）。
///
/// REMINDER：这里提交的 aborted assistant 条目**没有** `byTaskId` 归属（调度器的提交没有任务
/// scope）。
pub async fn settle_scheduler_outcome(
    tx: &dyn Tx,
    record: &TaskRecord,
    outcome: &SchedulerOutcome,
) -> Result<(), BoxError> {
    let conversation_id: ConversationId = record.conversation_id;
    let task_id: TaskId = record.id;
    if record.kind == TOOL_TASK_KIND {
        let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
        let mut draft = tx.doc(&target).await?;
        let live = draft.value_mut();
        // 槽位可能已被正常结算移除；找不到就跳过，不报错。
        if let Some(mut slot) = tool_slot(live, task_id) {
            finish_slot(&mut slot);
        }
        // 父任务终态后一并移除它名下的嵌套槽。
        remove_nested_slots(live, task_id);
        return Ok(());
    }
    if record.kind == COMPACTION_TASK_KIND {
        // 错误文本来源：fault 用 error.message，orphan 用调度器给的原因。
        let error = match outcome {
            SchedulerOutcome::Faulted { error } => error.message.clone(),
            SchedulerOutcome::Orphaned { reason } => reason.clone(),
        };
        let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
        let mut draft = tx.doc(&target).await?;
        fail_compaction_status(draft.value_mut(), task_id, &error);
        return Ok(());
    }
    if !RUN_TASK_KINDS.contains(&record.kind.as_str()) {
        // 其它 kind 直接返回：不打开、更不创建 `kiana.live`。
        return Ok(());
    }
    let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    let live = draft.value_mut();
    let owns_run = live
        .get("run")
        .and_then(|run| run.get("taskId"))
        .and_then(JsonValue::as_i64)
        == Some(task_id.get());
    if !owns_run {
        // run 已不存在或属于别的任务：不改它的展示。
        return Ok(());
    }
    // 已提交的 partial 变成 stopReason 为 aborted 的 assistant 条目（并记 usage）；调度器的
    // 提交没有任务 scope，所以该条目没有 `byTaskId` 归属（见上方 REMINDER）。
    crate::harness::generation::convert_partial(tx, &mut draft, conversation_id).await?;
    let settlement = match outcome {
        SchedulerOutcome::Faulted { error } => SubmissionSettlement::Unanswered {
            reason: "faulted".to_owned(),
            detail: Some(JsonValue::String(error.message.clone())),
        },
        SchedulerOutcome::Orphaned { reason } => SubmissionSettlement::Unanswered {
            reason: reason.clone(),
            detail: None,
        },
    };
    end_run(tx, draft.value_mut(), task_id, &settlement);
    Ok(())
}

/// `TaskOutcome` 的 `status` 字面量（TS 的判别值；诊断 code 也直接用它）。
pub fn outcome_status(outcome: &TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Completed { .. } => "completed",
        TaskOutcome::Failed { .. } => "failed",
        TaskOutcome::Aborted { .. } => "aborted",
        TaskOutcome::Orphaned { .. } => "orphaned",
        TaskOutcome::Faulted { .. } => "faulted",
    }
}

// ---------------------------------------------------------------------------
// Task commits
// ---------------------------------------------------------------------------

/// 一次任务提交返回的 future（借用事务与当前记录）。
pub type CommitFuture<'t> =
    Pin<Box<dyn Future<Output = Result<Option<TaskState>, BoxError>> + Send + 't>>;

/// 把 [`TaskRuntimeChange`] 的 `async` 方法适配成闭包。
///
/// TS 的 `runtime.commit(async (tx) => ..., context)` 直接写内联异步函数；Rust 的 trait
/// 方法不能是闭包，这里把任意 `for<'t> Fn(&'t dyn Tx, &'t TaskRecord) -> future` 包成一个
/// 变更对象。闭包捕获的值按 `Send + Sync` 要求传入（通常是 `Arc`/标量/`Mutex`）。
///
/// 闭包在 `TaskRuntime::commit` 内部被调用，拿到一次事务和当前任务记录；返回 `Some(state)`
/// 表示替换任务状态，`None` 表示只改文档、任务状态不变。
pub struct TaskCommit<'a> {
    run: Box<dyn for<'t> Fn(&'t dyn Tx, &'t TaskRecord) -> CommitFuture<'t> + Send + Sync + 'a>,
}

impl<'a> TaskCommit<'a> {
    /// 包装一个提交闭包；不在构造时执行，等 `TaskRuntime::commit` 调用它。
    pub fn new<F>(run: F) -> Self
    where
        F: for<'t> Fn(&'t dyn Tx, &'t TaskRecord) -> CommitFuture<'t> + Send + Sync + 'a,
    {
        TaskCommit { run: Box::new(run) }
    }
}

#[async_trait]
impl TaskRuntimeChange for TaskCommit<'_> {
    async fn run(&self, tx: &dyn Tx, current: &TaskRecord) -> Result<Option<TaskState>, BoxError> {
        (self.run)(tx, current).await
    }
}

/// `runtime.commit(change, context, options)` 的便捷形态（TS 的内联 commit 回调）。
///
/// 把闭包包成 [`TaskCommit`] 后立即提交；提交错误原样以 `BoxError` 返回，本函数不重试、
/// 不吞错。
pub async fn commit_with<I, S, R, H, F>(
    runtime: &dyn TaskRuntime<I, S, R, H>,
    run: F,
    context: &dyn Context,
    options: CommitOptions,
) -> Result<(), BoxError>
where
    F: for<'t> Fn(&'t dyn Tx, &'t TaskRecord) -> CommitFuture<'t> + Send + Sync,
{
    let change = TaskCommit::new(run);
    runtime.commit(&change, context, options).await
}

/// 读一条已提交条目的 `model[0]`（TS: `entry.model?.[0]`）。
pub fn entry_message(record: &crate::types::EntryRecord) -> Option<&kiana_ai::types::Message> {
    record.model.as_ref().and_then(|model| model.first())
}

// ---------------------------------------------------------------------------
// Task erasure
// ---------------------------------------------------------------------------

/// 读一个任务状态里的 checkpoint（TS: `task.state.checkpoint`）。
///
/// `Pending`/`Running`/`Waiting`/`Sleeping` 携带恢复所需的状态；`Completing` 已定结果、
/// `Terminal` 永久落定，两者没有可恢复的 checkpoint，返回 `None`。调用方通常把 `None`
/// 当作"阶段数据缺失"报错（例如压缩任务）。
pub fn state_checkpoint<S>(state: &TaskState<S>) -> Option<&S> {
    match state {
        TaskState::Pending { checkpoint }
        | TaskState::Running { checkpoint }
        | TaskState::Waiting { checkpoint, .. }
        | TaskState::Sleeping { checkpoint, .. } => Some(checkpoint),
        TaskState::Completing { .. } | TaskState::Terminal { .. } => None,
    }
}

/// 擦除后的任务迁移函数：`(input, checkpoint, fromVersion) -> (input, checkpoint)`。
///
/// 契约：[`crate::harness::types::AnyTaskDefinition::migrate`] 存放的值是
/// `Arc<dyn Fn(JsonValue, JsonValue, i64) -> Result<(JsonValue, JsonValue), BoxError> + Send + Sync>`
/// （即 `TaskMigrate<JsonValue, JsonValue>`），调度器按这个类型 downcast。
pub type ErasedTaskMigrate = crate::types::TaskMigrate<JsonValue, JsonValue>;

/// 把类型化的 `TaskRuntime` 变成 `H = ()` 的视图，供擦除后的阶段调用。
///
/// 除 `task_id` 的结果品牌（`TaskId<R>` 与 `TaskId` 是同一种数值，`R` 只在类型层）与
/// `hook_type_marker` 外，trait 的每个成员都与 `H` 无关，逐个委托即可。
///
/// `hook_type_marker` 用 trait 的默认实现（返回 `PhantomData`），不需要转发；本视图不拥有、
/// 不复制内层状态，生命周期与内层借用相同。
pub struct HooksRuntime<'a, H> {
    inner: &'a (dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()> + 'a),
    marker: std::marker::PhantomData<fn() -> H>,
}

impl<'a, H> HooksRuntime<'a, H> {
    /// 包装一个擦除运行端。
    pub fn new(inner: &'a dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>) -> Self {
        HooksRuntime {
            inner,
            marker: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<H: Send + Sync + 'static> TaskRuntime<JsonValue, JsonValue, JsonValue, H>
    for HooksRuntime<'_, H>
{
    fn task_id(&self) -> TaskId<JsonValue> {
        id_from_number(self.inner.task_id().get())
    }

    fn conversation_id(&self) -> ConversationId {
        self.inner.conversation_id()
    }

    fn signal(&self) -> kiana_chord::context::AbortSignal {
        self.inner.signal()
    }

    fn registry(&self) -> std::sync::Arc<dyn crate::harness::types::RegistrySnapshot> {
        self.inner.registry()
    }

    async fn agent(
        &self,
        context: &dyn Context,
    ) -> Result<std::sync::Arc<crate::harness::types::Agent>, BoxError> {
        self.inner.agent(context).await
    }

    fn settings(&self) -> Result<std::sync::Arc<crate::harness::types::Settings>, BoxError> {
        self.inner.settings()
    }

    fn models(&self) -> std::sync::Arc<dyn kiana_ai::models::Models> {
        self.inner.models()
    }

    async fn env(
        &self,
        context: &dyn Context,
    ) -> Result<Option<std::sync::Arc<dyn crate::env::ExecutionEnv>>, BoxError> {
        self.inner.env(context).await
    }

    fn hooks(&self) -> std::sync::Arc<dyn crate::harness::types::HookRunner> {
        self.inner.hooks()
    }

    async fn commit(
        &self,
        change: &dyn TaskRuntimeChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<(), BoxError> {
        self.inner.commit(change, context, options).await
    }

    async fn memo(&self, name: &str, context: &dyn Context) -> Result<Option<JsonValue>, BoxError> {
        self.inner.memo(name, context).await
    }

    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        self.inner.memo_put(name, candidate, context).await
    }

    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.inner.get_task(id, context).await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<crate::harness::types::SettledTask, BoxError> {
        self.inner.wait_for_task(id, context).await
    }

    async fn abort_owned(&self, id: TaskId, context: &dyn Context) -> Result<(), BoxError> {
        self.inner.abort_owned(id, context).await
    }

    async fn owned_tasks(&self, context: &dyn Context) -> Result<Vec<TaskRecord>, BoxError> {
        self.inner.owned_tasks(context).await
    }

    async fn outcomes(
        &self,
        ids: &[TaskId],
        context: &dyn Context,
    ) -> Result<Vec<TaskOutcome>, BoxError> {
        self.inner.outcomes(ids, context).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<std::sync::Arc<dyn crate::harness::types::ConversationHandle>>, BoxError>
    {
        self.inner.conversation(id, context).await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<crate::types::EntryRecord>, BoxError> {
        self.inner.entry(id, context).await
    }

    async fn context(
        &self,
        conversation_id: ConversationId,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError> {
        self.inner.context(conversation_id, context, options).await
    }

    fn now(&self) -> i64 {
        self.inner.now()
    }

    fn set_timer(
        &self,
        callback: crate::timers::TimerCallback,
        delay_ms: f64,
    ) -> crate::timers::Disarm {
        self.inner.set_timer(callback, delay_ms)
    }

    fn report(
        &self,
        error: &(dyn std::error::Error + Send + Sync),
        from: Option<crate::harness::types::ReportFrom>,
    ) {
        self.inner.report(error, from);
    }

    async fn sleep_until(
        &self,
        until: i64,
        checkpoint: JsonValue,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.inner.sleep_until(until, checkpoint, context).await
    }
}

#[async_trait]
impl<H: Send + Sync + 'static> crate::types::DocumentReader for HooksRuntime<'_, H> {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        crate::types::DocumentReader::snapshot(self.inner, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        crate::types::DocumentReader::snapshot_as_of(self.inner, target, at, context).await
    }
}

#[async_trait]
impl<H: Send + Sync + 'static> crate::types::DocumentObserver for HooksRuntime<'_, H> {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<crate::types::DocumentWatch>, BoxError> {
        crate::types::DocumentObserver::watch_doc(self.inner, target, context).await
    }
}

/// 把带类型 hook 的阶段包装成 `H = ()` 的擦除阶段。
///
/// `ErasedHooksPhase::run` 在调用内层阶段前构造 [`HooksRuntime`]，把擦除后的运行端适配回
/// 该阶段声明的 `H`；调度器因此只需认识 `H = ()` 的接口。
pub struct ErasedHooksPhase<H> {
    inner: std::sync::Arc<dyn crate::types::TaskPhase<JsonValue, JsonValue, JsonValue, H>>,
}

impl<H> ErasedHooksPhase<H> {
    /// 包装；只保存阶段的 `Arc`，适配发生在 `run` 里。
    pub fn new(
        inner: std::sync::Arc<dyn crate::types::TaskPhase<JsonValue, JsonValue, JsonValue, H>>,
    ) -> Self {
        ErasedHooksPhase { inner }
    }
}

#[async_trait]
impl<H: Send + Sync + 'static> crate::types::TaskPhase<JsonValue, JsonValue, JsonValue, ()>
    for ErasedHooksPhase<H>
{
    async fn run(
        &self,
        task: &crate::types::RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let view = HooksRuntime::<H>::new(runtime);
        self.inner.run(task, &view, context).await
    }
}

/// 把 `TaskDefinition<JsonValue, JsonValue, JsonValue, H>` 擦除成注册表里的 [`AnyTask`]。
///
/// 约定（调度器按此 downcast）：
/// - `phases`/`abort` 的值是调度器私有的 `ErasedTaskPhase`（内部持有 `Arc<dyn ErasedPhaseHandler>`），
///   不是裸的 `Arc<dyn TaskPhase<...>>`；
/// - `initial` 是 [`crate::session::transaction::ErasedTaskInitial`]；
/// - `migrate` 是 [`ErasedTaskMigrate`]；
/// - `hooks` 是 `Arc<H>`（`H` 为任务声明的 hook 表）。
///
/// `phases` 用 `IndexMap` 保留定义顺序；每个阶段与 `abort` 都包成调度器可 downcast 的
/// 擦除处理器。注册表里的值与调度器期望的类型不符时，调度阶段会返回错误（见
/// `harness/scheduler.rs` 的 `erased_phase`/`erased_abort`），而不是未定义行为。
pub fn erase_task_definition<H: Send + Sync + 'static>(
    definition: &crate::types::TaskDefinition<JsonValue, JsonValue, JsonValue, H>,
) -> AnyTask {
    let mut phases: indexmap::IndexMap<String, std::sync::Arc<dyn std::any::Any + Send + Sync>> =
        indexmap::IndexMap::new();
    for (name, phase) in &definition.phases {
        let erased: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(
            crate::harness::scheduler::ErasedTaskPhase::new(std::sync::Arc::clone(phase)),
        );
        phases.insert(name.clone(), erased);
    }
    let abort: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(
        crate::harness::scheduler::ErasedTaskPhase::new(std::sync::Arc::clone(&definition.abort)),
    );
    // 契约：`AnyTaskDefinition.initial` 必须存放 `ErasedTaskInitial`；创建任务时
    // `call_task_initial`（`session/transaction.rs`）按该类型 downcast，失败返回错误。
    // 这里用一层转发闭包把 `definition.initial` 包成它；该闭包层是否必要待核实。
    let initial: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(
        crate::session::transaction::ErasedTaskInitial(std::sync::Arc::new({
            let initial = std::sync::Arc::clone(&definition.initial);
            move |input: JsonValue| initial(input)
        })),
    );
    // `migrate`/`hooks` 是可选成员：Some 时把 Arc 原样擦除，None 保持 None；调度器按
    // `ErasedTaskMigrate` / `Arc<H>` 契约 downcast。
    let migrate: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> =
        definition.migrate.as_ref().map(|migrate| {
            std::sync::Arc::new(std::sync::Arc::clone(migrate))
                as std::sync::Arc<dyn std::any::Any + Send + Sync>
        });
    let hooks: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> =
        definition.hooks.as_ref().map(|hooks| {
            std::sync::Arc::new(std::sync::Arc::clone(hooks))
                as std::sync::Arc<dyn std::any::Any + Send + Sync>
        });
    AnyTask {
        definition: std::sync::Arc::new(crate::harness::types::AnyTaskDefinition {
            name: definition.name.clone(),
            version: definition.version,
            initial,
            phases,
            abort,
            migrate,
            hooks,
        }),
    }
}

/// 把一个类型化阶段转成 `Arc<dyn Any + Send + Sync>`（自定义注册表代码用）。
///
/// 返回的 `Any` 里存放的是 [`ErasedHooksPhase`]。调度器从 `AnyTaskDefinition.phases` 取处理器
/// 时按它自己的擦除类型 downcast（`harness/scheduler.rs` 的 `erased_phase`）；把本函数的
/// 返回值直接放进阶段表会因类型不符而报 `io_error`——内建任务走的是
/// [`erase_task_definition`]。
pub fn erase_phase<H: Send + Sync + 'static>(
    phase: std::sync::Arc<dyn crate::types::TaskPhase<JsonValue, JsonValue, JsonValue, H>>,
) -> std::sync::Arc<dyn std::any::Any + Send + Sync> {
    let erased: std::sync::Arc<dyn std::any::Any + Send + Sync> =
        std::sync::Arc::new(ErasedHooksPhase::new(phase));
    erased
}

// ---------------------------------------------------------------------------
// Hook API adapter
// ---------------------------------------------------------------------------

/// 把任务运行端包装成 hook 可用的 [`crate::harness::types::HookApi`]（TS: `runtime` 直接传给 hook）。
///
/// `task_id`/`conversation_id` 在构造时捕获，其余成员（`models`、`memo`/`memo_put` 与
/// `DocumentReader` 的读取方法）原样转发给运行端。`HookApi` 不暴露 settings/env/commit。
pub struct RuntimeHookApi<'a, H> {
    runtime: &'a dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H>,
    task_id: TaskId,
    conversation_id: ConversationId,
}

impl<'a, H> RuntimeHookApi<'a, H> {
    /// 包装。
    ///
    /// 构造时读取一次 `task_id` 与 `conversation_id` 并保存；其余成员仍实时转发给 `runtime`。
    pub fn new(runtime: &'a dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H>) -> Self {
        RuntimeHookApi {
            runtime,
            task_id: id_from_number(runtime.task_id().get()),
            conversation_id: runtime.conversation_id(),
        }
    }
}

/// 把运行端以 `'static` 借用捕获进提交闭包。
///
/// [`TaskCommit`] 的闭包必须对任意 `'t` 返回 `CommitFuture<'t>`（实际要求 future 为 `'static`），
/// 而提交回调需要在提交真正执行时读 `runtime.now()`/`runtime.settings()`（TS 是在 commit 回调
/// 里求值的）。闭包只在 `runtime.commit(...).await` 期间被执行，运行端在 `commit` 返回前一直
/// 存活，因此延长借用与 [`hook_api`] 同理是安全的；闭包不得把该引用带出 `commit`。
///
/// 误用：把返回值保存在 `commit` 之外，或让提交 future 在 `commit` 返回后继续使用它，都会
/// 留下悬垂引用（未定义行为）。本函数是安全函数，这条契约只能由调用方维护；本 crate 的
/// 调用点（`harness/tool.rs`、`harness/generation.rs`、`harness/compaction.rs`）都在同一个
/// 函数作用域内使用返回值，不会越过原借用的存活期。
pub fn static_runtime<I, S, R, H>(
    runtime: &dyn TaskRuntime<I, S, R, H>,
) -> &'static dyn TaskRuntime<I, S, R, H>
where
    H: 'static,
{
    // SAFETY: 见上；返回值只在调用方同一函数作用域内使用（直接调用或捕获进 commit 闭包），
    // 原借用在此之前一直存活。
    unsafe {
        std::mem::transmute::<&dyn TaskRuntime<I, S, R, H>, &'static dyn TaskRuntime<I, S, R, H>>(
            runtime,
        )
    }
}

/// 为一次 hook 调用构造 `Arc<dyn HookApi>`。
///
/// hook trait 的签名要求 `Arc<dyn HookApi>`（没有生命周期参数），而运行端只以借用形式传给阶段，
/// 所以这里把借用延长到 `'static`，**只允许在紧随其后的 hook 调用期间使用**：hook 实现必须把
/// 传入的 `api` 当作回调参数，不能在调用返回后继续持有。
///
/// 若 hook 实现把 `Arc` 留存到阶段结束后再使用，同样会留下悬垂引用（未定义行为）；本 crate
/// 的调用点都在阶段函数内完成分发。
pub fn hook_api<H: Send + Sync + 'static>(
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H>,
) -> std::sync::Arc<dyn crate::harness::types::HookApi> {
    // SAFETY: 见上。`RuntimeHookApi` 只转发调用，不拥有运行端；这个 `Arc` 在 hook 调用结束后
    // 就应被丢弃（[`hook_api`] 的每个调用点都在拿到返回值后立刻用完）。
    let runtime: &'static dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H> = unsafe {
        std::mem::transmute::<
            &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H>,
            &'static dyn TaskRuntime<JsonValue, JsonValue, JsonValue, H>,
        >(runtime)
    };
    std::sync::Arc::new(RuntimeHookApi::new(runtime))
}

#[async_trait]
impl<H: Send + Sync + 'static> crate::harness::types::HookApi for RuntimeHookApi<'_, H> {
    fn task_id(&self) -> TaskId {
        self.task_id
    }

    fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    fn models(&self) -> std::sync::Arc<dyn kiana_ai::models::Models> {
        self.runtime.models()
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
}

#[async_trait]
impl<H: Send + Sync + 'static> crate::types::DocumentReader for RuntimeHookApi<'_, H> {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        crate::types::DocumentReader::snapshot(self.runtime, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        crate::types::DocumentReader::snapshot_as_of(self.runtime, target, at, context).await
    }
}

/// 一个 hook 处理器及其所属扩展（TS: `agentHooks()` 的 `{ extension, handlers }` 项）。
pub struct HookHandler<H: ?Sized + Send + Sync + 'static> {
    /// 提供该处理器的扩展名；未知时为 `None`。
    pub extension: Option<String>,
    /// 处理器。
    pub handler: std::sync::Arc<H>,
}

/// 取出某个 hook 名的全部处理器，并 downcast 成 `Arc<H>`。
///
/// `H` 允许是 trait object（`dyn ToolHooks` 等）：注册表里存的处理器就是 `Arc<H>`，
/// 而 `Arc<H>` 本身是一个具体（Sized）类型。
/// TS 的 `hooks.each` 在取处理器之前无条件 `await agent()`：即使本阶段还没有别的地方解析过
/// agent，hook 分发也必须经过它（解析失败同样让阶段失败）；调度器的处理器表也只在 agent
/// 解析之后才可见（见 `SchedulerRuntime::hook_tables`）。
///
/// downcast 失败的处理器被静默跳过（注册表里存的不是 `Arc<H>`），不当作错误；`agent()` 的
/// 解析错误则通过 `?` 向上传播，让阶段失败。
pub async fn hook_handlers<H, I, S, R, HH>(
    runtime: &dyn TaskRuntime<I, S, R, HH>,
    name: &str,
    context: &dyn Context,
) -> Result<Vec<HookHandler<H>>, BoxError>
where
    H: ?Sized + Send + Sync + 'static,
{
    // 先解析 agent：hook 处理器表只有在 agent 解析之后才可见（见函数文档）。
    runtime.agent(context).await?;
    Ok(runtime
        .hooks()
        .handlers_with_extension(name)
        .into_iter()
        .filter_map(|(extension, handler)| {
            handler
                .downcast_ref::<std::sync::Arc<H>>()
                .map(|handler| HookHandler {
                    extension,
                    handler: std::sync::Arc::clone(handler),
                })
        })
        .collect())
}

/// TS `hooks.each` 对单个处理器失败的处理：调用已中止时向上传播，否则按
/// `extension_error` 报告并继续后续处理器。
///
/// `report` 是 `runtime.report` 的注入形式，便于在无运行端的单元测试里断言报告内容。
///
/// `aborted` 由调用方从 `runtime.signal().is_aborted()` 取得：为真时把错误原样返回（阶段按
/// 取消路径结束），为假时只上报并返回 `Ok`，剩余处理器继续执行。
pub fn hook_failure(
    aborted: bool,
    extension: Option<&str>,
    error: BoxError,
    report: impl FnOnce(&(dyn std::error::Error + Send + Sync), Option<ReportFrom>),
) -> Result<(), BoxError> {
    if aborted {
        return Err(error);
    }
    report(
        error.as_ref(),
        Some(ReportFrom::Extension {
            extension: extension.map(str::to_owned),
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn live() -> JsonObject {
        JsonObject::new()
    }

    fn task(value: i64) -> TaskId {
        id_from_number(value)
    }

    #[test]
    fn tool_slot_finds_model_and_nested_slots() {
        let mut live = live();
        live.insert(
            "tools".to_owned(),
            json!([{ "callId": "a", "name": "one", "status": "running", "taskId": 3 }]),
        );
        live.insert(
            "nestedTools".to_owned(),
            json!([
                { "callId": "a/1", "parentTaskId": 3, "taskId": 7, "name": "x", "arguments": {}, "status": "pending" },
                { "callId": "a/2", "parentTaskId": 3, "taskId": 9, "name": "y", "arguments": {}, "status": "running" },
            ]),
        );
        assert!(tool_slot(&mut live, task(3)).is_some());
        assert!(tool_slot(&mut live, task(7)).is_some());
        assert!(tool_slot(&mut live, task(7)).unwrap().is_nested());
        assert!(tool_slot(&mut live, task(9)).unwrap().is_nested());
        assert!(tool_slot(&mut live, task(11)).is_none());
    }

    #[test]
    fn nested_slots_are_binary_searched_in_task_order() {
        let mut live = live();
        let mut slots = Vec::new();
        for id in [10, 20, 30, 40, 50] {
            slots.push(json!({
                "callId": format!("a/{id}"), "parentTaskId": 1, "taskId": id,
                "name": "x", "arguments": {}, "status": "pending"
            }));
        }
        live.insert("nestedTools".to_owned(), serde_json::Value::Array(slots));
        assert!(tool_slot(&mut live, task(40)).is_some());
        assert!(tool_slot(&mut live, task(45)).is_none());
    }

    #[test]
    fn finish_slot_keeps_the_status_and_drops_progress() {
        let mut live = live();
        live.insert(
            "tools".to_owned(),
            json!([{
                "callId": "a", "name": "one", "status": "running", "taskId": 3,
                "output": "line", "droppedBytes": 2, "droppedLines": 1,
                "details": { "x": 1 }, "diagnostics": []
            }]),
        );
        let mut slot = tool_slot(&mut live, task(3)).unwrap();
        finish_slot(&mut slot);
        assert_eq!(
            live.get("tools").unwrap(),
            &json!([{ "callId": "a", "name": "one", "status": "done", "taskId": 3 }])
        );
    }

    #[test]
    fn remove_nested_slots_removes_children_transitively() {
        let mut live = live();
        live.insert(
            "nestedTools".to_owned(),
            json!([
                { "parentTaskId": 1, "taskId": 2 },
                { "parentTaskId": 2, "taskId": 3 },
                { "parentTaskId": 9, "taskId": 4 },
            ]),
        );
        remove_nested_slots(&mut live, task(1));
        assert_eq!(
            live.get("nestedTools").unwrap(),
            &json!([{ "parentTaskId": 9, "taskId": 4 }])
        );
        remove_nested_slots(&mut live, task(9));
        assert!(!live.contains_key("nestedTools"));
    }

    #[test]
    fn compaction_statuses_stay_in_order_and_leave_a_failure() {
        let mut live = live();
        let status = CompactionStatus {
            task_id: task(4),
            reason: CompactionReason::Threshold,
            blocking: true,
            attempt: 1,
            retry: None,
        };
        add_compaction_status(&mut live, &status);
        assert!(compaction_status(&mut live, task(4)).is_some());
        fail_compaction_status(&mut live, task(4), "boom");
        assert!(!live.contains_key("compactions"));
        assert_eq!(
            live.get("compactionFailure").unwrap(),
            &json!({ "taskId": 4, "reason": "threshold", "error": "boom" })
        );
    }

    #[test]
    fn live_doc_checkpoints_exactly_while_nothing_runs() {
        let definition = &LIVE_DOC.definition;
        let mut value = JsonObject::new();
        assert!((definition.checkpoint_when)(
            &value,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
        value.insert("generation".to_owned(), json!({ "attempt": 1 }));
        assert!(!(definition.checkpoint_when)(
            &value,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
        let mut value = JsonObject::new();
        value.insert(
            "tools".to_owned(),
            json!([{ "callId": "a", "name": "x", "status": "running" }]),
        );
        assert!(!(definition.checkpoint_when)(
            &value,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
        let mut value = JsonObject::new();
        value.insert(
            "tools".to_owned(),
            json!([{ "callId": "a", "name": "x", "status": "done" }]),
        );
        assert!((definition.checkpoint_when)(
            &value,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
    }

    #[test]
    fn live_state_round_trips_its_document_shape() {
        let state = LiveState {
            run: Some(LiveRun {
                task_id: task(1),
                inputs: vec![id_from_number(2)],
            }),
            generation: Some(LiveGeneration {
                attempt: 2,
                message: Some(json!({ "role": "assistant" })),
                retry: Some(LiveRetry {
                    at: 10.0,
                    error: "x".to_owned(),
                }),
                deferred: Some(LiveDeferred { poll_at: 5.0 }),
            }),
            tools: Some(vec![ToolSlot {
                call_id: "a".to_owned(),
                name: "one".to_owned(),
                status: SlotStatus::Running,
                output: Some("out".to_owned()),
                ..ToolSlot::default()
            }]),
            ..LiveState::default()
        };
        let document = state.clone().into_document();
        assert_eq!(
            document.get("run").unwrap(),
            &json!({ "taskId": 1, "inputs": [2] })
        );
        assert_eq!(
            document.get("generation").unwrap(),
            &json!({ "attempt": 2, "message": { "role": "assistant" }, "retry": { "at": 10.0, "error": "x" }, "deferred": { "pollAt": 5.0 } })
        );
        assert_eq!(
            document.get("tools").unwrap(),
            &json!([{ "callId": "a", "name": "one", "status": "running", "output": "out" }])
        );
        assert_eq!(LiveState::from_document(&document).unwrap(), state);
    }

    #[test]
    fn nested_slot_summary_serializes_its_fields() {
        let summary = NestedToolSummary {
            is_error: true,
            duration_ms: Some(3.0),
            usage: None,
            error: Some("bad".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(summary).unwrap(),
            json!({ "isError": true, "durationMs": 3.0, "error": "bad" })
        );
    }

    #[test]
    fn nested_tool_slot_serializes_in_ts_construction_order() {
        // TS（tool.ts 的 executeTool 提交）按这个顺序构造槽位；文档键序要一致。
        let slot = NestedToolSlot {
            call_id: "a/1".to_owned(),
            parent_call_id: "a".to_owned(),
            parent_task_id: task(3),
            name: "x".to_owned(),
            task_id: task(7),
            arguments: JsonObject::new(),
            status: SlotStatus::Pending,
            output: Some("out".to_owned()),
            dropped_bytes: Some(1),
            dropped_lines: Some(2),
            details: Some(json!({ "k": 1 })),
            diagnostics: Some(vec![ToolDiagnostic {
                severity: crate::harness::types::DiagnosticSeverity::Error,
                code: Some("boom".to_owned()),
                message: "bad".to_owned(),
            }]),
            summary: Some(NestedToolSummary {
                is_error: true,
                duration_ms: Some(3.0),
                usage: None,
                error: Some("bad".to_owned()),
            }),
        };
        let value = serde_json::to_value(&slot).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "callId",
                "parentCallId",
                "parentTaskId",
                "name",
                "taskId",
                "arguments",
                "status",
                "output",
                "droppedBytes",
                "droppedLines",
                "details",
                "diagnostics",
                "summary",
            ]
        );
    }
}
