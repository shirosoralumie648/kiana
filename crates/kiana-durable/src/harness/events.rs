//! `harness/events.ts`：实验性的 agent 事件流（会话事件形状，spec §9.4）。
//!
//! TS: `packages/durable/src/harness/events.ts`。每个提交产出一个"批次"（`Vec<AgentEvent>`），
//! 事件顺序是 §9.4 的权威顺序；未投递的批次超过 100 个时替换为一次当前视图快照。
//!
//! # 这个文件解决什么问题
//!
//! Harness 的内部状态（活动转录、`kiana.live` 等内建文档、任务与提交表）按"提交"变化，
//! 宿主 UI 需要的却是一条条可以顺序消费的展示事件。本文件负责这层翻译：输入是视图挂载在
//! 提交前后的两份快照、本次提交的视图 op 与原始 `ObservedCommit`，输出是本次提交对应的
//! `Vec<AgentEvent>`。它不写存储、不改视图；除原地更新跨提交的 `held` 集合外，翻译逻辑只
//! 读取参数。
//!
//! # 边界与调用方
//!
//! 对外入口是 [`watch_events`]（`HarnessImpl::watch_events` 转调），返回的
//! [`AgentEventStream`] 由宿主调用 `start`/`stop`/`closed`。批次通过 `harness/view.rs` 的
//! `NativeWatch` 串行投递：每次发布至多一个批次，批次之间不合并；待投递批次超过 100 个时，
//! 队列被替换成一次"当前视图快照"（[`AgentEvent::Snapshot`]）。
//!
//! # 一次订阅的执行流程
//!
//! 1. [`watch_events`] 通过 `ConversationViews::attach` 在 Session 的变更线上注册观察者，
//!    并在返回前固定 `AgentEventStream::snapshot`；
//! 2. 注册时扫描该会话已处于 `completing` 的 `kiana.generation` 任务，把它们的 ID 放进
//!    `held`，避免对已经结束过的 turn 重复发 `turn_end`；
//! 3. 之后每次发布到达观察者的 `publication`：先更新"最新视图"，再调用 `translate`；
//!    空批次不投递；
//! 4. Session 关闭或失败时观察者的 `end` 让流落定为 `Ended` 或 `Failed`；宿主调用 `stop`
//!    则落定为 `Stopped`。
//!
//! # 重要约束
//!
//! - `translate` 的分段顺序就是对外协议顺序（进度 → 工具结束 → 条目 → 压缩/任务失败 →
//!   turn/run 结束 → 提交与文档 → 压缩开始 → run/turn 开始）。`tests/harness_events.rs`
//!   对整批顺序有逐项断言，改动前先读它的断言语义。
//! - 跨提交去重依赖 `before`/`after` 两份视图的差值，而不是自己保存完整历史；本提交之外的
//!   条目、任务与提交记录不会进入事件。
//! - 序列化保持 JS `JSON.stringify` 的文本格式（当前稳定契约，改动需同步测试）：含 `f64`
//!   的载荷走 `serialize_js_json` 或 `JsNumber`，直接交给 serde 的默认浮点格式会写出不同的
//!   文本（例如 `0.0` 与 `0`）。
//! - `snapshot` 是 `untagged` 变体，`SnapshotEvent` 自带 `"type":"snapshot"`；否则枚举标签
//!   会与结构体字段写出重复键。
//!
//! # 阅读顺序
//!
//! 先看公开的事件形状（[`AgentEvent`]、[`SnapshotEvent`]、[`MessageChange`]），再看
//! [`watch_events`] 的订阅流程，最后按分段注释读 `translate`；`message_changes` 与
//! `tool_update` 是消息与工具输出增量编码的核心。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::IndexMap;
use kiana_ai::types::{AssistantContentBlock, AssistantMessage, Message, Usage};
use kiana_chord::context::ContextRef;
use kiana_chord::delta::{Op, Path, Seg};
use serde::{Deserialize, Serialize};

use crate::harness::agent::AGENT_DOC;
use crate::harness::inbox::{InboxItem, InboxState};
use crate::harness::live::{
    CompactionFailure, CompactionStatus, LiveRetry, LiveState, NestedToolSlot, ToolSlot,
};
use crate::harness::tool::{NESTED_CALL_DOC, NestedCallState};
use crate::harness::types::Harness;
use crate::harness::types::{
    AgentState, CompactionReason, ConversationView, NestedToolExecutionResult, TaskGraphState,
    ToolDiagnostic,
};
use crate::harness::usage::USAGE_DOC;
use crate::harness::util::scan_all;
use crate::harness::view::{NativeWatch, conversation_views};
use crate::session::session::{Failure, ObservedCommit};
use crate::types::{
    BoxError, CommitChange, ConversationId, DocumentCommitChange, EntryId, EntryRecord, JsonObject,
    JsonValue, SubmissionId, SubmissionRecord, TableCommitChange, TaskId, TaskQuery, TaskRecord,
    TaskStatus, WatchEnd, WatchListener,
};

// ---------------------------------------------------------------------------
// 公开类型
// ---------------------------------------------------------------------------

/// 一次正在进行中的 assistant 消息的变化（相对该消息）。
///
/// `contentIndex` 是数组下标（当前稳定契约）；`block` 是 kiana-ai 的内容块。
///
/// 一个批次里的多个变化按出现顺序应用在同一条消息上：`*Start` 在指定下标插入初始块，
/// `*Delta` 追加到已存在的块，`Block`/`Message` 整体替换。测试里的 `apply_changes` 就是
/// 按这个规则重放，并断言重建结果与提交到 `kiana.live` 的 partial 序列一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum MessageChange {
    /// 一个新文本块开始。
    ///
    /// 在 `content_index` 处插入一个文本块；之后同下标的 [`MessageChange::TextDelta`]
    /// 追加到它的 `text`。
    TextStart {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 初始块。
        block: AssistantContentBlock,
    },
    /// 一个新思考块开始。
    ///
    /// 在 `content_index` 处插入一个思考块；之后同下标的
    /// [`MessageChange::ThinkingDelta`] 追加到它的 `thinking`。
    ThinkingStart {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 初始块。
        block: AssistantContentBlock,
    },
    /// 一个新工具调用块开始。
    ///
    /// 在 `content_index` 处插入一个工具调用块；参数增长以
    /// [`MessageChange::ToolcallDelta`] 追加。
    ToolcallStart {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 初始块。
        block: AssistantContentBlock,
    },
    /// 文本增长。
    ///
    /// 把 `delta` 追加到该下标文本块的 `text`；下标必须已存在。
    TextDelta {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 追加的文本。
        delta: String,
    },
    /// 思考增长。
    ///
    /// 把 `delta` 追加到该下标思考块的 `thinking`。
    ThinkingDelta {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 追加的文本。
        delta: String,
    },
    /// 工具调用参数增长。
    ///
    /// `path` 相对该块的 `arguments` 对象，`delta` 追加到路径末端的字符串（与 TS 的
    /// `String.prototype.slice`/`+=` 语义一致，测试的 `append_at` 按同一规则重放）。
    ToolcallDelta {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 参数里的路径。
        path: Vec<ChangePathSegment>,
        /// 追加的文本。
        delta: String,
    },
    /// 整个块被替换。
    ///
    /// 发送后，本批次内同下标的后续 op 被抑制：该 `block` 已包含这些变化的最终结果。
    Block {
        /// 内容下标。
        #[serde(rename = "contentIndex")]
        content_index: usize,
        /// 新块。
        block: AssistantContentBlock,
    },
    /// 整条消息被替换。
    ///
    /// 这是无法用增量表达时的回退；消费方应丢弃之前的块状态，直接采用 `message`。
    Message {
        /// 新消息。
        message: AssistantMessage,
    },
}

/// `path` 的一段：对象键或数组下标。
///
/// 序列化为裸字符串或数字（`untagged`），对应 TS 的 `readonly (string | number)[]`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChangePathSegment {
    /// 对象键。
    Key(String),
    /// 数组下标。
    Index(i64),
}

/// 排队中的一项：只暴露 ID 与 mode，不含内容。
///
/// 内容与条目草稿留在 `kiana.inbox` 文档里；事件消费者只能据此知道"谁在排队、以什么模式"，
/// 要展示输入文本必须另行读取该文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedItem {
    /// 提交 ID。
    pub id: SubmissionId,
    /// 队列模式：`steer`、`followUp` 或 `write`。
    pub mode: QueuedMode,
}

/// 队列项的 mode 字符串。
///
/// 由 `kiana.inbox` 的 `InboxItem` 投影而来：`InputMode::Steer`/`FollowUp` 分别对应
/// `steer`/`followUp`，条目写入（`InboxItem::Write`）对应 `write`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueuedMode {
    /// steering。
    #[serde(rename = "steer")]
    Steer,
    /// follow-up。
    #[serde(rename = "followUp")]
    FollowUp,
    /// 被动写入。
    #[serde(rename = "write")]
    Write,
}

/// `snapshot` 事件里的运行输入。
///
/// `inputs` 是 `kiana.live.run.inputs` 的副本，顺序即该次运行的输入顺序（`run_start` /
/// `run_end` 使用同一列表）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRun {
    /// 输入提交。
    pub inputs: Vec<SubmissionId>,
}

/// 以 JS `JSON.stringify` 的格式写出一个浮点数（`1700000000000` 而非 `1700000000000.0`，
/// `NaN`/`±Infinity` 写成 `null`，见 ARCHITECTURE §4.3 第 4 条）。
struct JsNumber(f64);

impl Serialize for JsNumber {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let raw =
            serde_json::value::RawValue::from_string(kiana_json::js_num::format_js_number(self.0))
                .map_err(S::Error::custom)?;
        raw.serialize(serializer)
    }
}

/// 把单个 `f64` 走 [`JsNumber`] 的格式；给 `#[serde(serialize_with = ...)]` 用。
///
/// 目前只用于 `AutoRetryStart.at` 与 `DeferredPoll.poll_at` 这两个 epoch 毫秒字段。
fn serialize_js_number<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    JsNumber(*value).serialize(serializer)
}

/// 以 JS `JSON.stringify` 的文本格式序列化整棵值（数字与整数键序；见 ARCHITECTURE §4.3）。
///
/// 用在含 `f64` 的字段（用量成本、耗时）以及由模型产出的 JSON 载荷上：JS 的 `JSON.stringify`
/// 把 `0` 写成 `0`、`120` 写成 `120`，而 serde 的 f64 默认写 `0.0` / `120.0`。先转成
/// `serde_json::Value` 再经 [`kiana_json::js_object::stringify`] 写出，无论调用方直接
/// `serde_json::to_string` 还是先 `to_value`，都得到同一份文本。
pub(crate) fn serialize_js_json<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize + ?Sized,
    S: serde::Serializer,
{
    use serde::ser::Error as _;
    let value = serde_json::to_value(value).map_err(S::Error::custom)?;
    let text = kiana_json::js_object::stringify(&value);
    serde_json::value::RawValue::from_string(text)
        .map_err(S::Error::custom)?
        .serialize(serializer)
}

/// `SnapshotGeneration.retry`（`LiveRetry`）的序列化：`at` 走 JS 数字格式。
///
/// 手写结构体序列化只为替换这一个浮点字段；键名与顺序保持 `at`、`error`，与 TS 的
/// `{ at, error }` 一致。
fn serialize_retry<S: serde::Serializer>(
    value: &Option<LiveRetry>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let Some(retry) = value else {
        return serializer.serialize_none();
    };
    let mut state = serializer.serialize_struct("LiveRetry", 2)?;
    state.serialize_field("at", &JsNumber(retry.at))?;
    state.serialize_field("error", &retry.error)?;
    state.end()
}

/// `SnapshotGeneration.deferred`（`LiveDeferred`）的序列化：`pollAt` 走 JS 数字格式。
///
/// 同样只为把 `pollAt` 写成 JS 数字文本；字段名由这里显式给出（`pollAt`）。
fn serialize_deferred<S: serde::Serializer>(
    value: &Option<crate::harness::live::LiveDeferred>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let Some(deferred) = value else {
        return serializer.serialize_none();
    };
    let mut state = serializer.serialize_struct("LiveDeferred", 1)?;
    state.serialize_field("pollAt", &JsNumber(deferred.poll_at))?;
    state.end()
}

/// `snapshot` 事件里的当前 generation 尝试。
///
/// 字段原样来自 `kiana.live.generation`；`message` 的 JSON 解析失败时该字段缺席（见
/// `generation_of`），`retry`/`deferred` 不受影响。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotGeneration {
    /// 尝试序号。
    ///
    /// 持久化的计数，原样透传，不由本文件递增。
    pub attempt: i64,
    /// 进行中的 partial。
    ///
    /// 已提交到文档的节流 partial，解码为 `AssistantMessage` 后才放进快照。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<AssistantMessage>,
    /// 重试退避。
    ///
    /// 存在表示 generation 处于持久退避；`at` 是 epoch 毫秒。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_retry"
    )]
    pub retry: Option<LiveRetry>,
    /// provider 侧延迟轮询。
    ///
    /// 存在时 `pollAt` 是下一次轮询的 epoch 毫秒。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_deferred"
    )]
    pub deferred: Option<crate::harness::live::LiveDeferred>,
}

/// `SnapshotEvent` 的判别字段（TS: `type: "snapshot"`）。
///
/// 该字段随结构体一起序列化，因此 `AgentEventStream.snapshot` 单独写出时与 TS 一样带
/// `"type":"snapshot"`；[`AgentEvent::Snapshot`] 变体标记为 untagged，避免枚举标签重复。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotEventType {
    /// 快照。
    #[default]
    #[serde(rename = "snapshot")]
    Snapshot,
}

/// 附件时的快照事件。
///
/// 字段声明顺序即序列化顺序。测试 `a_snapshot_serializes_like_the_typescript_event` 逐键
/// 断言可选字段缺席时的键序 `type, entries, tools, nestedTools, compactions, inbox, agent,
/// usage`，并要求它作为 [`AgentEvent::Snapshot`] 序列化时与单独序列化字节相同。
/// `run`/`generation`/`compactionFailure` 缺席时不写出键。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEvent {
    /// 判别字段。`AgentEvent::Snapshot` 反序列化时枚举标签已被取走，故允许缺省。
    #[serde(rename = "type", default)]
    pub r#type: SnapshotEventType,
    /// 活动条目。
    ///
    /// 挂载的活动转录（按上下文边界与 head 标记筛选），不是全量历史。
    #[serde(serialize_with = "serialize_js_json")]
    pub entries: Vec<EntryRecord>,
    /// 运行输入。
    ///
    /// 仅当 `kiana.live.run` 存在时出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<SnapshotRun>,
    /// 当前 generation 尝试。
    ///
    /// 仅当 `kiana.live.generation` 存在时出现。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_json"
    )]
    pub generation: Option<SnapshotGeneration>,
    /// 工具槽。
    ///
    /// `kiana.live.tools` 缺席时为空列表。
    #[serde(serialize_with = "serialize_js_json")]
    pub tools: Vec<ToolSlot>,
    /// 嵌套调用槽。
    ///
    /// `kiana.live.nestedTools` 缺席时为空列表。
    #[serde(serialize_with = "serialize_js_json")]
    pub nested_tools: Vec<NestedToolSlot>,
    /// 活动压缩。
    ///
    /// `kiana.live.compactions` 缺席时为空列表。
    #[serde(serialize_with = "serialize_js_json")]
    pub compactions: Vec<CompactionStatus>,
    /// 最新压缩失败。
    ///
    /// 仅当 `kiana.live.compactionFailure` 存在时出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_failure: Option<CompactionFailure>,
    /// 排队项。
    ///
    /// 只含 id 与 mode；`kiana.inbox` 缺席时为空列表。
    pub inbox: Vec<QueuedItem>,
    /// `kiana.agent`；缺席时 `{}`。
    ///
    /// 退役或从未创建时取文档定义的初始值（空对象解析出的默认 `AgentState`）。
    pub agent: AgentState,
    /// 用量账本。
    ///
    /// 退役或从未创建时取文档定义的初始值（空账本）。
    #[serde(serialize_with = "serialize_js_json")]
    pub usage: crate::harness::types::UsageState,
}

/// 一个工具事件涉及的调用。
///
/// `toolCallId` 是转录与工具事件里的调用 ID：模型调用是 provider 的 ID，嵌套调用是
/// `<parent call ID>/<key>`；按它匹配事件，不要解析它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolEventCall {
    /// 调用 ID。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 该调用的工具任务；从未获得任务的调用没有。
    ///
    /// 嵌套调用总会带上自己的工具任务（`call_of_nested` 无条件写入）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// 嵌套调用：发起调用的父调用 ID。
    ///
    /// 只有嵌套调用有；模型发起的调用为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    /// 嵌套调用：父调用的工具任务。
    ///
    /// 只有嵌套调用有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
}

/// 工具输出的增量：先 trim 再 append 的窗口，或整体替换。
///
/// 消费方对 `Window` 的处理等价于 `output = output.slice(trimStart ?? 0) + (append ?? "")`，
/// 其中 `trimStart` 以 UTF-16 code unit 计（与 TS 的 `String.prototype.slice` 一致；测试的
/// `utf16_slice` 按同样的单位重放）。`Set` 直接替换保留的整段输出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolOutputUpdate {
    /// 窗口：剪掉开头 `trimStart` 个 UTF-16 单元并追加。
    Window {
        /// 剪掉的字符数。
        ///
        /// 单位是 UTF-16 code unit，不是字节；`None` 表示不裁剪头部。
        #[serde(rename = "trimStart", default, skip_serializing_if = "Option::is_none")]
        trim_start: Option<usize>,
        /// 追加的文本。
        ///
        /// `None`（或空串）表示这次没有追加。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append: Option<String>,
    },
    /// 整体替换。
    Set {
        /// 新值。
        set: String,
    },
}

/// 实验性的 agent 事件（22 种 `type`）。
///
/// 每个批次由 `translate` 针对一次发布生成，同一批内的顺序是协议的一部分。序列化为内部
/// 标签枚举：`{"type":"<snake_case>", ...字段}`；唯一的例外是 [`AgentEvent::Snapshot`]
/// （`untagged`），由 [`SnapshotEvent`] 自己写 `"type":"snapshot"`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum AgentEvent {
    /// 一次运行开始。
    ///
    /// 在 `kiana.live.run` 出现且首个输入与上一版不同时发出；`inputs` 是这次运行的输入
    /// 提交，位置在批次末尾（`submission`/文档事件之后，`turn_start` 之前）。
    RunStart {
        /// 输入提交。
        inputs: Vec<SubmissionId>,
    },
    /// 一次运行结束。
    ///
    /// 在 `kiana.live.run` 消失或首个输入变化时发出（只用 `inputs.first()` 判定是否换 run）；
    /// `inputs` 是结束那次运行的输入。它先于同一批的 `submission` 事件。
    RunEnd {
        /// 输入提交。
        inputs: Vec<SubmissionId>,
    },
    /// 一轮开始。
    ///
    /// 当 `kiana.live.run.task_id` 变化、且新任务在本提交里是 `kiana.generation` 时发出，
    /// 位于批次末尾。
    TurnStart,
    /// 一轮结束。
    ///
    /// generation 的 outcome 被提交（状态变为 `completing`），或任务进入 `terminal` 且此前
    /// 未结束过时发出；打开流时已经 `completing` 的任务由 `held` 记录，不会重复发。先于
    /// `run_end`。
    TurnEnd,
    /// 一条消息开始。
    ///
    /// 两种来源：in-flight assistant partial 首次出现（带 partial），或本提交追加的、未经过
    /// 流式 partial 的条目消息。流式回答结束时不会再发一次（条目循环里的 `streamed` 判定）。
    MessageStart {
        /// 消息。
        #[serde(serialize_with = "serialize_js_json")]
        message: Message,
    },
    /// 进行中的 partial 变化。
    ///
    /// `usage` 是变化后 partial 的完整用量；`changes` 可从上一版重建出同一内容。partial 内容
    /// 未变但视图节点被本次提交重建时也会发出（TS 的对象身份语义）。
    MessageUpdate {
        /// partial 当前的用量。
        #[serde(serialize_with = "serialize_js_json")]
        usage: Usage,
        /// 变化。
        #[serde(serialize_with = "serialize_js_json")]
        changes: Vec<MessageChange>,
    },
    /// 一条消息结束。
    ///
    /// `entry` 是已持久化的条目；工具结果条目也走这里，此时对应的
    /// `tool_execution_end` 紧邻它之前。
    MessageEnd {
        /// 条目。
        #[serde(serialize_with = "serialize_js_json")]
        entry: EntryRecord,
    },
    /// 工具调用开始。
    ///
    /// 仅当槽从非 `running` 变为 `running` 时发出（本提交新建即 `running` 的槽也算）。模型
    /// 调用的 `args` 来自该工具任务 checkpoint 里的 `arguments`；嵌套调用直接取槽上的
    /// `arguments`。
    ToolExecutionStart {
        /// 调用。
        #[serde(flatten)]
        call: ToolEventCall,
        /// 参数。
        #[serde(serialize_with = "serialize_js_json")]
        args: JsonObject,
    },
    /// 工具调用进度。
    ///
    /// 仅在槽于提交前后都处于 `running` 时发出；三个字段各自可缺省，缺省表示该通道本批次
    /// 没有变化。输出用窗口增量表达滚动保留区，`details`/`diagnostics` 的删除分别写
    /// `null`/`[]`（安全重放会清空 running 槽的进度）。
    ToolExecutionUpdate {
        /// 调用。
        #[serde(flatten)]
        call: ToolEventCall,
        /// 输出增量。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ToolOutputUpdate>,
        /// details；删除时为 `null`。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_js_json"
        )]
        details: Option<JsonValue>,
        /// 诊断；删除时为 `[]`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostics: Option<Vec<ToolDiagnostic>>,
    },
    /// 工具调用结束。
    ///
    /// 三种来源：槽变为 `done`、槽创建即 `done`（例如请求未提供该工具）、或未完成的槽从
    /// 视图消失。模型调用带 `entry`（结果条目必须能在本提交的条目里找到）；嵌套调用只在
    /// 结果文档随本提交写入时才带 `result`（`end_nested` 只查本提交收集的 `nested_results`，
    /// 结果文档缺席或由更早提交写入时为 `None`）。没有 `entry` 的模型调用结束（结果条目
    /// 不在本提交里，如 faulted/孤立的工具）排在所有条目事件之后；嵌套调用的结束——
    /// 包括 `result` 缺失时——在工具结束收集阶段直接加入批次，位于条目事件之前。
    ToolExecutionEnd {
        /// 调用。
        #[serde(flatten)]
        call: ToolEventCall,
        /// 模型调用：结果条目。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_js_json"
        )]
        entry: Option<EntryRecord>,
        /// 嵌套调用：结果。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_js_json"
        )]
        result: Option<NestedToolExecutionResult>,
    },
    /// 队列变化。
    ///
    /// 在 `kiana.inbox` 的值变化或文档节点被本提交重建时发出；只含 id 与 mode。
    InboxUpdate {
        /// 排队项。
        items: Vec<QueuedItem>,
    },
    /// 一次提交记录。
    ///
    /// 本提交写入的提交记录按 ID 升序逐个发出（同一批可能包含多条）。
    Submission {
        /// 记录。
        record: SubmissionRecord,
    },
    /// 自动重试开始。
    ///
    /// `generation.retry` 从无到有时发出；`at` 是下一次尝试的 epoch 毫秒，走 JS 数字格式。
    /// `attempt` 原样来自 `kiana.live.generation.attempt`，本文件不递增它。
    AutoRetryStart {
        /// 尝试序号。
        attempt: i64,
        /// 下次尝试的时间点。
        #[serde(serialize_with = "serialize_js_number")]
        at: f64,
        /// 错误消息。
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    /// 自动重试结束。
    ///
    /// `generation.retry` 从有到无时发出；`attempt` 取变化前（处于退避那次）的序号。
    AutoRetryEnd {
        /// 尝试序号。
        attempt: i64,
    },
    /// 延迟轮询。
    ///
    /// 仅当本版 `generation.deferred` 存在、且其 `pollAt` 与上一版不同（含 deferred 从无
    /// 到有）时发出；deferred 从有到无不发事件。`poll_at` 是 epoch 毫秒，走 JS 数字格式。
    DeferredPoll {
        /// 下次轮询的时间点。
        #[serde(rename = "pollAt", serialize_with = "serialize_js_number")]
        poll_at: f64,
    },
    /// 追加了一个条目。
    ///
    /// 只在条目没有 `model` 消息（纯展示条目，例如宿主追加的 note）时发出；有 model 消息的
    /// 条目改发 `message_start`/`message_end`。
    EntryAppended {
        /// 条目。
        #[serde(serialize_with = "serialize_js_json")]
        entry: EntryRecord,
    },
    /// agent 文档变化。
    ///
    /// 在 `kiana.agent` 值变化或文档节点被本提交重建时发出；退役的文档读作初始值（空对象
    /// 解析出的默认 `AgentState`），与快照一致。
    AgentChanged {
        /// 状态。
        agent: AgentState,
    },
    /// 用量变化。
    ///
    /// 在 `kiana.usage` 值变化或文档节点被本提交重建时发出；退役的文档读作初始值（空账本）。
    UsageChanged {
        /// 账本。
        #[serde(serialize_with = "serialize_js_json")]
        usage: crate::harness::types::UsageState,
    },
    /// 任务失败或孤立。
    ///
    /// 只针对本提交里处于 `terminal` 且 outcome 为 `faulted` 或 `orphaned` 的任务；`message`
    /// 分别是错误消息或孤立原因。
    TaskFailed {
        /// 任务。
        #[serde(rename = "taskId")]
        task_id: TaskId,
        /// 任务种类。
        kind: String,
        /// 错误文本。
        message: String,
    },
    /// 压缩开始。
    ///
    /// 本提交的 `kiana.live.compactions` 里出现、上一版没有的压缩任务，按当前列表顺序发出；
    /// `blocking` 表示它是否阻塞一个 generation。
    CompactionStart {
        /// 任务。
        #[serde(rename = "taskId")]
        task_id: TaskId,
        /// 原因。
        reason: CompactionReason,
        /// 是否阻塞一个 generation。
        blocking: bool,
    },
    /// 压缩结束。
    ///
    /// 上一版存在、本提交消失的压缩任务；若最新 `compactionFailure` 的 `task_id` 匹配，
    /// `error` 带上失败/孤立文本，否则省略该键。
    CompactionEnd {
        /// 任务。
        #[serde(rename = "taskId")]
        task_id: TaskId,
        /// 原因。
        reason: CompactionReason,
        /// 错误文本（失败、fault 或孤立）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// 附件时的快照。
    ///
    /// 标记为 untagged：`SnapshotEvent` 自带 `"type":"snapshot"`，由枚举标签再写一次会产生
    /// 重复键；反序列化仍按 `type` 判别（serde 在读取内部标签后把它从内容里移除，结构体的
    /// `type` 字段因此带 `default`）。
    #[serde(untagged)]
    Snapshot(SnapshotEvent),
}

/// 一个会话事件批次的串行流（TS: `AgentEventStream`）。
///
/// `snapshot` 在 [`watch_events`] 返回时固定，不随后续提交更新；之后的状态变化只通过监听器
/// 收到的批次送出。一个流只接受一个监听器。
pub struct AgentEventStream {
    /// 附件时的快照。
    ///
    /// 与 TS 一样是"附加那一刻"的视图，不是最新视图；溢出批次会另行发送最新快照。
    pub snapshot: SnapshotEvent,
    watch: Arc<NativeWatch<Vec<AgentEvent>>>,
}

impl AgentEventStream {
    /// 安装唯一的异步监听器；绝不同步调用。
    ///
    /// 批次的投递由 `NativeWatch` 异步串行驱动，`start` 本身只登记监听器并唤醒投递任务。
    ///
    /// # Panics
    /// 已经调用过 `start`，或流已终止（`stop`/关闭/失败）时 panic。
    pub fn start(&self, listener: Arc<dyn WatchListener<Vec<AgentEvent>>>) {
        self.watch.start(listener);
    }

    /// 幂等停止后续回调，并返回终态。
    ///
    /// 停止后 `closed` 立即返回同一个终态；待投递的批次被丢弃。
    pub async fn stop(&self) -> WatchEnd {
        self.watch.stop().await
    }

    /// 终止时落定。
    ///
    /// 流尚未终止时等待。可能的终态：`Stopped`（调用方停止）、`Cancelled`（获取上下文被
    /// 取消）、`Ended`（Session 关闭）、`ListenerError`（监听器 panic）、`Failed`（Session
    /// 失败）；本流不产生 `Retired`。
    pub async fn closed(&self) -> WatchEnd {
        self.watch.closed().await
    }

    /// 异步投递队列是否已排空。
    ///
    /// `watch_events` 的批次由 `NativeWatch` 的投递任务串行送出；提交落定与回调执行之间
    /// 存在调度间隙。方法返回 `true` 时，所有已 `advance` 的批次都已交给监听器。
    pub fn is_idle(&self) -> bool {
        self.watch.is_idle()
    }

    /// 等待投递队列排空（最多 10 秒）。
    ///
    /// 宿主与测试用它替代固定 sleep：慢机器上固定 3ms 可能不够，而这里只在“监听器已
    /// 看完所有批次”后返回。
    pub async fn wait_until_idle(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.is_idle() {
                return;
            }
            if std::time::Instant::now() > deadline {
                panic!("event stream did not drain within 10s");
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// 视图读取
// ---------------------------------------------------------------------------

// 锁中毒时取回内部值而不是 panic；与 `harness/view.rs`、`session/session.rs` 的 `lock` 同
// 策略。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 从挂载读取的类型化部分（TS: `Parts`）。
///
/// `live` 在文档缺席或解析失败时退回 `LiveState::default()`；其余三个保留 `Option`，所以
/// "文档不存在"与"存在但形状非法"在这里都成为 `None`，下游的 `now.inbox != was.inbox` 等
/// 比较会把后者当成删除/重建处理。
struct Parts {
    live: LiveState,
    inbox: Option<InboxState>,
    agent: Option<AgentState>,
    usage: Option<crate::harness::types::UsageState>,
}

// 把 `JsonObject`（`IndexMap<String, JsonValue>`）转成 serde_json 的对象，供 `from_value`
// 按字段名解析文档；这里的键序不参与解析。
fn json_object(value: &JsonObject) -> JsonValue {
    JsonValue::Object(value.clone().into_iter().collect())
}

/// 把挂载里的内建文档解析成 [`Parts`]。
///
/// 只读 live/inbox/agent/usage 四个文档；provider 文档不参与事件。文档缺席、解析失败都走
/// 上面的宽容策略。`translate` 用前后两份 `Parts` 的相等比较判定 inbox/agent/usage 是否变化。
fn parts(view: &ConversationView) -> Parts {
    Parts {
        live: view
            .docs
            .get("kiana.live")
            .and_then(LiveState::from_document)
            .unwrap_or_default(),
        inbox: view
            .docs
            .get("kiana.inbox")
            .and_then(|doc| serde_json::from_value(json_object(doc)).ok()),
        agent: view
            .docs
            .get("kiana.agent")
            .and_then(|doc| serde_json::from_value(json_object(doc)).ok()),
        usage: view
            .docs
            .get("kiana.usage")
            .and_then(|doc| serde_json::from_value(json_object(doc)).ok()),
    }
}

/// 文档初始值（TS: `AgentDoc.definition.initial()` / `UsageDoc.definition.initial()`）。
///
/// 直接复用文档定义的 `initial` 闭包，保证"退役/缺席的文档"与新建文档的初始形状一致；
/// 解析失败时退回类型的 `Default`，事件流不会因此中断。
fn initial_agent() -> AgentState {
    serde_json::from_value(json_object(&(AGENT_DOC.definition.initial)(None))).unwrap_or_default()
}

/// 用量文档的初始值；规则同 [`initial_agent`]。
fn initial_usage() -> crate::harness::types::UsageState {
    serde_json::from_value(json_object(&(USAGE_DOC.definition.initial)(None))).unwrap_or_default()
}

/// 把队列状态投影成事件里的 [`QueuedItem`]：只保留 id 与 mode，内容与条目草稿不进入事件。
///
/// `None`（文档缺席/解析失败）与空队列都得到空列表。
fn queued(inbox: Option<&InboxState>) -> Vec<QueuedItem> {
    inbox
        .map(|inbox| {
            inbox
                .items
                .iter()
                .map(|item| match item {
                    InboxItem::Input { id, mode, .. } => QueuedItem {
                        id: *id,
                        mode: match mode {
                            crate::harness::inbox::InputMode::Steer => QueuedMode::Steer,
                            crate::harness::inbox::InputMode::FollowUp => QueuedMode::FollowUp,
                        },
                    },
                    InboxItem::Write { id, .. } => QueuedItem {
                        id: *id,
                        mode: QueuedMode::Write,
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 把 `kiana.live.generation` 投影成快照里的 generation。
///
/// `message` 是持久化的 JSON，解码成 `AssistantMessage` 失败时只丢该字段，`retry`/`deferred`
/// 仍然保留；`attempt` 原样透传。
fn generation_of(live: &LiveState) -> Option<SnapshotGeneration> {
    let generation = live.generation.as_ref()?;
    Some(SnapshotGeneration {
        attempt: generation.attempt,
        message: generation
            .message
            .as_ref()
            .and_then(|message| serde_json::from_value(message.clone()).ok()),
        retry: generation.retry.clone(),
        deferred: generation.deferred.clone(),
    })
}

/// 由挂载构造一次完整快照。
///
/// 缺失的 `run`/`generation`/`compactionFailure` 在序列化时是缺席键；槽位与压缩列表缺失时
/// 用空列表；`agent`/`usage` 缺失时回退到文档初始值。这个函数既用于 attach 时的固定快照，
/// 也用于 `NativeWatch` 溢出时替换未投递批次的"当前快照"。
fn snapshot_of(view: &ConversationView) -> SnapshotEvent {
    let parts = parts(view);
    let live = &parts.live;
    SnapshotEvent {
        r#type: SnapshotEventType::Snapshot,
        entries: view.entries.clone(),
        run: live.run.as_ref().map(|run| SnapshotRun {
            inputs: run.inputs.clone(),
        }),
        generation: generation_of(live),
        tools: live.tools.clone().unwrap_or_default(),
        nested_tools: live.nested_tools.clone().unwrap_or_default(),
        compactions: live.compactions.clone().unwrap_or_default(),
        compaction_failure: live.compaction_failure.clone(),
        inbox: queued(parts.inbox.as_ref()),
        agent: parts.agent.unwrap_or_else(initial_agent),
        usage: parts.usage.unwrap_or_else(initial_usage),
    }
}

// ---------------------------------------------------------------------------
// watchEvents
// ---------------------------------------------------------------------------

/// 一次运行的观察者：保存最新视图、`held` 集合与批次 watch。
///
/// `current` 是 `NativeWatch` 溢出回调构造快照时的输入，所以每次发布都要先更新它；`held`
/// 跨提交保留，记录 turn 已经结束过的 generation 任务（见 `translate` 的任务循环）。
/// `watch` 的 detach 回调把观察者从挂载上摘除，Session 结束时不会泄漏。
struct EventObserver {
    conversation_id: ConversationId,
    watch: Arc<NativeWatch<Vec<AgentEvent>>>,
    current: Arc<Mutex<ConversationView>>,
    held: Arc<Mutex<HashSet<TaskId>>>,
}

impl crate::harness::view::ViewObserver for EventObserver {
    fn publication(
        &self,
        before: &ConversationView,
        after: &ConversationView,
        ops: &[Op],
        publication: &ObservedCommit,
        context: &ContextRef,
    ) {
        // 先记下最新视图：`NativeWatch::advance` 在队列溢出时同步调用 `replace` 回调构造
        // 替换帧，而该回调读取 `current`，所以必须在调用 `advance` 之前更新它。
        *lock(&self.current) = after.clone();
        let events = {
            // held 同时是 translate 的输入与输出，所以锁覆盖整个翻译过程。
            let mut held = lock(&self.held);
            translate(
                self.conversation_id,
                before,
                after,
                ops,
                publication,
                &mut held,
            )
        };
        // 空批次不投递：NativeWatch 每帧都会触发一次监听器调用，提交里与本会话无关的变化
        // 不应制造空回调。
        if !events.is_empty() {
            self.watch.advance(events, Arc::clone(context));
        }
    }

    // Session 关闭（failure 为 None）或失败：让流以 Ended/Failed 落定，并清空待投递批次。
    fn end(&self, failure: Option<&Failure>) {
        self.watch.end(failure.cloned());
    }
}

/// 附加到某个会话的 agent 事件流（spec §9.4）。快照与后续提交的注册在 Session 线上原子完成；
/// 溢出时用一个快照替换未投递的批次。
///
/// # 参数
///
/// - `harness`：用于找回它的 `ConversationViews`；不是 Harness 实现时报逐字错误
///   `Not a Harness`。
/// - `conversation_id`：订阅的会话；其他会话的提交不会进入批次（`translate` 按会话过滤）。
/// - `context`：决定流的生命周期，并在注册期间提供取消与 Storage 读取的上下文。
///
/// # 返回与错误
///
/// 成功时快照已在返回前固定。挂载构建（会话不存在、扫描任务或读文档失败）的错误直接返回；
/// `attach` 完成后若 `context` 已被取消，会先取消 watch 再返回取消错误。内部交接缺失的两个
/// `ReportedError`（`watchEvents attach failed` / `watchEvents snapshot missing`）是防御分支。
///
/// # 生命周期
///
/// 返回的流不会自行启动；宿主调用 `start` 安装监听器。Session 关闭时流以 `Ended`（无失败）
/// 或 `Failed` 落定，宿主 `stop` 则落定为 `Stopped`。注册上下文被取消会让流以 `Cancelled`
/// 终止。
pub async fn watch_events(
    harness: &dyn Harness,
    conversation_id: ConversationId,
    context: &dyn kiana_chord::context::Context,
) -> Result<AgentEventStream, BoxError> {
    let views = conversation_views(harness)?;
    // attach 的 create 回调是 'static 的 boxed future，快照与 watch 只能先写进共享单元再交回
    // 本函数；这是注册期间的一次性交接，不是长期共享状态。
    let snapshot_cell: Arc<Mutex<Option<SnapshotEvent>>> = Arc::new(Mutex::new(None));
    let watch_cell: Arc<Mutex<Option<Arc<NativeWatch<Vec<AgentEvent>>>>>> =
        Arc::new(Mutex::new(None));
    let context_ref = {
        use crate::session::session::ContextSource;
        context.context_ref()
    };
    let scan_context = context_ref.clone();

    let create_snapshot = Arc::clone(&snapshot_cell);
    let create_watch = Arc::clone(&watch_cell);
    let create_views = Arc::clone(&views);
    let (observer, _detach) = views
        .attach::<EventObserver, _>(
            conversation_id,
            move |initial, release, storage| {
                Box::pin(async move {
                    // 打开时已处于 `completing` 的 generation：其 outcome 已经结束过 turn。
                    let query = TaskQuery {
                        conversation_id: Some(conversation_id),
                        kind: Some("kiana.generation".to_owned()),
                        status: Some(TaskStatus::Completing),
                        ..TaskQuery::default()
                    };
                    let scan_context = Arc::clone(&scan_context);
                    // scan_tasks 是分页的（每页 100），scan_all 会一直读到 next 为空，返回
                    // 全部已处于 completing 的 generation；它们的 turn 已经结束过。
                    let completing = scan_all(|cursor: Option<crate::types::Cursor>| {
                        let query = query.clone();
                        let storage = Arc::clone(&storage);
                        let scan_context = Arc::clone(&scan_context);
                        async move {
                            storage
                                .scan_tasks(&query, 100, cursor.as_ref(), &*scan_context)
                                .await
                        }
                    })
                    .await?;
                    let mut held: HashSet<TaskId> = HashSet::new();
                    for record in completing {
                        held.insert(record.id);
                    }
                    let current = Arc::new(Mutex::new(initial.clone()));
                    *lock(&create_snapshot) = Some(snapshot_of(&initial));
                    let replace_current = Arc::clone(&current);
                    let report_views = Arc::clone(&create_views);
                    let release = release;
                    // NativeWatch 的四个参数：初始值（空批次）、终止时的 detach（注销观察者）、
                    // 监听器错误的报告通道（经 ConversationViews 转给 Harness）、以及队列溢出时
                    // 的替换值——用最新视图构造的单个 Snapshot 批次。
                    let watch = NativeWatch::new(
                        Vec::new(),
                        Box::new(move || release()),
                        Some(Arc::new(move |error: BoxError| report_views.report(error))),
                        Some(Arc::new(move || {
                            vec![AgentEvent::Snapshot(snapshot_of(&lock(&replace_current)))]
                        })),
                    );
                    *lock(&create_watch) = Some(Arc::clone(&watch));
                    Ok(EventObserver {
                        conversation_id,
                        watch,
                        current,
                        held: Arc::new(Mutex::new(held)),
                    })
                })
            },
            context,
        )
        .await?;

    // 与普通 watch 一样，获取时的 context 决定流的生命周期。
    // attach 成功时 create 已经把 watch 写进 watch_cell，因此这里的错误分支只是防御。
    let watch = watch_cell
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| -> BoxError {
            Box::new(crate::session::ReportedError(
                "watchEvents attach failed".to_owned(),
            ))
        })?;
    if let Some(signal) = context.abort_signal()
        && signal.is_aborted()
    {
        watch.cancel();
        return Err(Box::new(kiana_chord::context::abort_error(&signal)));
    }
    if let Some(signal) = context.abort_signal() {
        watch.observe_cancellation(&signal);
    }
    let snapshot = snapshot_cell
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| -> BoxError {
            Box::new(crate::session::ReportedError(
                "watchEvents snapshot missing".to_owned(),
            ))
        })?;
    let _ = observer;
    Ok(AgentEventStream { snapshot, watch })
}

// ---------------------------------------------------------------------------
// translate
// ---------------------------------------------------------------------------

/// 在条目里找 `callId` 的工具结果（TS: `resultOf`）。
///
/// 只看每个条目 `model` 的第一条消息。`translate` 传入的是本提交新增的条目，所以它不会
/// 在历史条目里重新发现旧结果。
fn result_of(entries: &[EntryRecord], call_id: &str) -> Option<EntryRecord> {
    entries.iter().find_map(|entry| {
        let message = entry.model.as_ref()?.first()?;
        match message {
            Message::ToolResult(result) if result.tool_call_id == call_id => Some(entry.clone()),
            _ => None,
        }
    })
}

/// 一次发布造成的全部事件，按 spec §9.4 的顺序。
///
/// # 输入
///
/// - `before`/`after`：同一挂载在本次发布前后的视图；所有"值是否变化"的判定都来自两者之差。
/// - `view_ops`：本次发布的视图 op，用于恢复增量（消息、工具输出）以及 TS 的对象身份语义。
/// - `publication`：原始提交，提供本提交写入的条目、任务、提交记录与嵌套调用结果文档。
/// - `held`：跨提交保留的"turn 已结束"集合；本函数会原地更新它。
///
/// # 顺序
///
/// 顺序是协议的一部分，`spec/durable-harness.md` §2.13.5 的 TS 实现与本函数逐段对应，
/// `tests/harness_events.rs` 对整批事件序列有断言。大致为：进度（工具开始 → 消息 → 工具
/// 更新 → 重试/轮询）→ 收集工具结束 → 条目（结束事件紧邻结果条目之前）→ 无条目的工具
/// 结束 → 压缩结束/任务失败 → `turn_end` → `run_end` → 提交与文档 → 压缩开始 →
/// `run_start` → `turn_start`。
///
/// # 约束
///
/// 函数只读输入、不访问存储。`entries`/`tasks`/`submissions` 只来自本次提交，因此不会重复
/// 发送历史事件；`held` 是唯一的跨提交状态。任务按本提交首次出现的顺序遍历（`IndexMap`
/// 保持首次插入位置，同一任务多次出现只保留最后一次的值）。
#[allow(clippy::too_many_lines)]
fn translate(
    conversation_id: ConversationId,
    before: &ConversationView,
    after: &ConversationView,
    view_ops: &[Op],
    publication: &ObservedCommit,
    held: &mut HashSet<TaskId>,
) -> Vec<AgentEvent> {
    let mut entries: Vec<EntryRecord> = Vec::new();
    let mut tasks: IndexMap<TaskId, TaskRecord> = IndexMap::new();
    let mut submissions: Vec<SubmissionRecord> = Vec::new();
    // 本提交存储的嵌套结果，按嵌套任务 ID；在这里读，因为调用方的文档可能随即退役。
    let mut nested_results: HashMap<TaskId, NestedToolExecutionResult> = HashMap::new();
    let nested_kind = &NESTED_CALL_DOC.definition.kind;
    // 只处理四类对事件有意义的写入：嵌套结果文档、条目、任务、提交；会话记录、其他 kind
    // 的文档以及别的会话的写入全部忽略。
    for change in publication.changes() {
        match change {
            CommitChange::Document(DocumentCommitChange::Document {
                record,
                conversation_id: Some(owner),
                value: Some(value),
                ..
            }) => {
                if record.kind == *nested_kind
                    && owner == conversation_id
                    && let Ok(state) =
                        serde_json::from_value::<NestedCallState>(json_object(&value))
                    && let (Some(task_id), Some(result)) = (state.task_id, state.result)
                    && let Ok(result) = serde_json::from_value::<NestedToolExecutionResult>(result)
                {
                    nested_results.insert(task_id, result);
                }
            }
            CommitChange::Table(TableCommitChange::Entry { value })
                if value.conversation_id == conversation_id =>
            {
                entries.push(value);
            }
            CommitChange::Table(TableCommitChange::Task { value })
                if value.conversation_id == conversation_id =>
            {
                tasks.insert(value.id, value);
            }
            CommitChange::Table(TableCommitChange::Submission { value })
                if value.conversation_id == conversation_id =>
            {
                submissions.push(value);
            }
            _ => {}
        }
    }
    // 视图 op 为空说明挂载内容没有变化；若本提交也没有本会话的条目/任务/提交，就不可能
    // 产生任何事件，直接跳过后续解析。
    if view_ops.is_empty() && entries.is_empty() && tasks.is_empty() && submissions.is_empty() {
        return Vec::new();
    }
    // 同一批里的提交按 ID 升序，而不是按本提交写入的顺序；测试对 placed/queued/done 的
    // 序列有断言。
    submissions.sort_by_key(|record| record.id);

    let was = parts(before);
    let now = parts(after);
    let mut events: Vec<AgentEvent> = Vec::new();

    // 进度：工具开始、进行中的消息、工具更新、重试与延迟状态。
    // 用 call_id / task_id 建索引以避免线性查找；列表本身保留原顺序，后面 tool_update 的
    // 路径下标依赖这个位置。
    let slots_before: IndexMap<String, ToolSlot> = was
        .live
        .tools
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|slot| (slot.call_id.clone(), slot))
        .collect();
    let slots = now.live.tools.clone().unwrap_or_default();
    let nested_before: IndexMap<TaskId, NestedToolSlot> = was
        .live
        .nested_tools
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|slot| (slot.task_id, slot))
        .collect();
    let nested = now.live.nested_tools.clone().unwrap_or_default();
    // 模型调用槽：只有当前 running、且提交前不在 running 的槽发 start（本提交新建即
    // running 的槽也算）；提交前后都在 running 的槽不重复发。
    for slot in &slots {
        let previous = slots_before.get(&slot.call_id);
        if slot.status != crate::harness::live::SlotStatus::Running
            || previous.is_some_and(|previous| {
                previous.status == crate::harness::live::SlotStatus::Running
            })
        {
            continue;
        }
        // 模型调用的参数在它的 intent checkpoint 里。
        let checkpoint: JsonObject = slot
            .task_id
            .and_then(|id| tasks.get(&id))
            .and_then(|record| record.state.checkpoint())
            .and_then(|checkpoint| checkpoint.get("arguments"))
            .and_then(JsonValue::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        // 任务记录不在本提交里（或没有 checkpoint / `arguments`）时参数退化为空对象；
        // start 事件仍会发出，消费者看到的是空参数。
        events.push(AgentEvent::ToolExecutionStart {
            call: call_of_tool(slot),
            args: checkpoint,
        });
    }
    // 嵌套槽同样只在进入 running 时发 start；参数直接取槽上执行用的最终参数。
    for slot in &nested {
        let previous = nested_before.get(&slot.task_id);
        if slot.status != crate::harness::live::SlotStatus::Running
            || previous.is_some_and(|previous| {
                previous.status == crate::harness::live::SlotStatus::Running
            })
        {
            continue;
        }
        events.push(AgentEvent::ToolExecutionStart {
            call: call_of_nested(slot),
            args: slot.arguments.clone(),
        });
    }
    // partial 每次从 JSON 解码：文档层只存 JSON，解码失败在这一版里等同于没有 partial
    // （既不会发 message_start，也不会发 message_update）。
    let partial_before = was
        .live
        .generation
        .as_ref()
        .and_then(|generation| generation.message.as_ref())
        .and_then(|message| serde_json::from_value::<AssistantMessage>(message.clone()).ok());
    let partial = now
        .live
        .generation
        .as_ref()
        .and_then(|generation| generation.message.as_ref())
        .and_then(|message| serde_json::from_value::<AssistantMessage>(message.clone()).ok());
    // TS 比较的是对象身份：本提交重建过消息节点（哪怕内容相同）也要发 `message_update`。
    let partial_rebuilt = node_rebuilt(view_ops, partial_path());
    if let Some(partial) = &partial {
        match &partial_before {
            None => events.push(AgentEvent::MessageStart {
                message: Message::Assistant(partial.clone()),
            }),
            Some(before) if before != partial || partial_rebuilt => {
                events.push(AgentEvent::MessageUpdate {
                    usage: partial.usage.clone(),
                    changes: message_changes(view_ops, partial),
                })
            }
            Some(_) => {}
        }
    }
    // 只在提交前后都处于 running 的槽上发 update：进入 running 的那次已经发过 start，
    // 离开 running 的那次改发 end。路径里的 index 是槽在当前列表中的位置。
    for (index, slot) in slots.iter().enumerate() {
        let previous = slots_before.get(&slot.call_id);
        if slot.status != crate::harness::live::SlotStatus::Running
            || !previous.is_some_and(|previous| {
                previous.status == crate::harness::live::SlotStatus::Running
            })
        {
            continue;
        }
        if let Some(update) = tool_update(
            view_ops,
            "tools",
            index,
            slot_ref_tool(slot),
            slot_ref_tool(previous.unwrap()),
        ) {
            events.push(tool_update_event(call_of_tool(slot), update));
        }
    }
    // 嵌套槽同理，只换路径前缀 `nestedTools`。
    for (index, slot) in nested.iter().enumerate() {
        let previous = nested_before.get(&slot.task_id);
        if slot.status != crate::harness::live::SlotStatus::Running
            || !previous.is_some_and(|previous| {
                previous.status == crate::harness::live::SlotStatus::Running
            })
        {
            continue;
        }
        if let Some(update) = tool_update(
            view_ops,
            "nestedTools",
            index,
            slot_ref_nested(slot),
            slot_ref_nested(previous.unwrap()),
        ) {
            events.push(tool_update_event(call_of_nested(slot), update));
        }
    }
    // 重试与轮询只在状态跨边界时发事件：retry 从无到有发 start、从有到无发 end；
    // deferred 仅在本版仍存在且 pollAt 与上一版不同（含从无到有）时发一次 poll，
    // deferred 消失时不发事件。
    let generation = now.live.generation.as_ref();
    let generation_before = was.live.generation.as_ref();
    if let Some(retry) = generation.and_then(|generation| generation.retry.as_ref())
        && generation_before
            .and_then(|generation| generation.retry.as_ref())
            .is_none()
    {
        events.push(AgentEvent::AutoRetryStart {
            attempt: generation
                .map(|generation| generation.attempt)
                .unwrap_or_default(),
            at: retry.at,
            error_message: retry.error.clone(),
        });
    }
    if let Some(retry) = generation_before.and_then(|generation| generation.retry.as_ref())
        && generation
            .and_then(|generation| generation.retry.as_ref())
            .is_none()
    {
        let _ = retry;
        events.push(AgentEvent::AutoRetryEnd {
            attempt: generation_before
                .map(|generation| generation.attempt)
                .unwrap_or_default(),
        });
    }
    if let Some(deferred) = generation.and_then(|generation| generation.deferred.as_ref())
        && generation_before
            .and_then(|generation| generation.deferred.as_ref())
            .map(|deferred| deferred.poll_at)
            != Some(deferred.poll_at)
    {
        events.push(AgentEvent::DeferredPoll {
            poll_at: deferred.poll_at,
        });
    }

    // 本提交结束的工具：变为 done 的槽、创建即 done 的槽（未提供的调用），或因其运行结束而消失
    // 的未完成槽。一个已经 done 的槽消失意味着它更早就结束了。
    let mut tool_ends: Vec<AgentEvent> = Vec::new();
    let end_tool = |slot: SlotRef<'_>, entry: Option<EntryId>, events: &mut Vec<AgentEvent>| {
        // 结果条目只从本提交的 `entries` 里找；找不到就不带 entry，但结束事件仍要发出。
        let entry =
            entry.and_then(|id| entries.iter().find(|candidate| candidate.id == id).cloned());
        let call = slot.call_of();
        events.push(match entry {
            Some(entry) => AgentEvent::ToolExecutionEnd {
                call,
                entry: Some(entry),
                result: None,
            },
            None => AgentEvent::ToolExecutionEnd {
                call,
                entry: None,
                result: None,
            },
        });
    };
    for previous in slots_before.values() {
        if previous.status == crate::harness::live::SlotStatus::Done {
            continue;
        }
        match slots.iter().find(|slot| slot.call_id == previous.call_id) {
            Some(slot) if slot.status == crate::harness::live::SlotStatus::Done => {
                end_tool(slot_ref_tool(slot), slot.entry, &mut tool_ends);
            }
            Some(_) => {}
            None => {
                let entry = result_of(&entries, &previous.call_id).map(|entry| entry.id);
                end_tool(slot_ref_tool(previous), entry, &mut tool_ends);
            }
        }
    }
    for slot in &slots {
        if slot.status == crate::harness::live::SlotStatus::Done
            && !slots_before.contains_key(&slot.call_id)
        {
            end_tool(slot_ref_tool(slot), slot.entry, &mut tool_ends);
        }
    }
    // 嵌套调用以同样的方式结束，但子先于父：列表父在前，故倒序。结果来自收集阶段读到的
    // `nested_results`（按嵌套任务 ID 索引），不在条目里。
    let end_nested = |slot: &NestedToolSlot,
                      nested_results: &HashMap<TaskId, NestedToolExecutionResult>,
                      events: &mut Vec<AgentEvent>| {
        events.push(AgentEvent::ToolExecutionEnd {
            call: call_of_nested(slot),
            entry: None,
            result: nested_results.get(&slot.task_id).cloned(),
        });
    };
    let nested_now: IndexMap<TaskId, &NestedToolSlot> =
        nested.iter().map(|slot| (slot.task_id, slot)).collect();
    for previous in nested_before.values().rev() {
        if previous.status == crate::harness::live::SlotStatus::Done {
            continue;
        }
        match nested_now.get(&previous.task_id) {
            Some(slot) if slot.status == crate::harness::live::SlotStatus::Done => {
                end_nested(slot, &nested_results, &mut events);
            }
            Some(_) => {}
            None => end_nested(previous, &nested_results, &mut events),
        }
    }
    for slot in nested.iter().rev() {
        if slot.status == crate::harness::live::SlotStatus::Done
            && !nested_before.contains_key(&slot.task_id)
        {
            end_nested(slot, &nested_results, &mut events);
        }
    }

    // 条目按追加顺序；工具结束紧邻其结果消息之前。
    // 顺序来自 `publication.changes()` 里条目写入的出现顺序，无需排序。
    let mut assistant_appended = false;
    for entry in &entries {
        for end in &tool_ends {
            if let AgentEvent::ToolExecutionEnd {
                entry: Some(ended), ..
            } = end
                && ended.id == entry.id
            {
                events.push(end.clone());
            }
        }
        let message = entry.model.as_ref().and_then(|model| model.first());
        let Some(message) = message else {
            events.push(AgentEvent::EntryAppended {
                entry: entry.clone(),
            });
            continue;
        };
        // 流式回答已由它的第一个 partial 开始。
        let streamed =
            message.role() == "assistant" && partial_before.is_some() && !assistant_appended;
        if message.role() == "assistant" {
            assistant_appended = true;
        }
        if !streamed {
            events.push(AgentEvent::MessageStart {
                message: message.clone(),
            });
        }
        events.push(AgentEvent::MessageEnd {
            entry: entry.clone(),
        });
    }
    // 没有结果条目的结束：faulted/孤立的工具，或其运行结束的工具。
    for end in &tool_ends {
        if let AgentEvent::ToolExecutionEnd { entry: None, .. } = end {
            events.push(end.clone());
        }
    }

    // 压缩结束、任务失败，然后 turn 与 run 结束。
    let compactions_before = was.live.compactions.clone().unwrap_or_default();
    let compactions = now.live.compactions.clone().unwrap_or_default();
    let failure = now.live.compaction_failure.clone();
    for status in &compactions_before {
        if !compactions
            .iter()
            .any(|candidate| candidate.task_id == status.task_id)
        {
            let error = failure
                .as_ref()
                .filter(|failure| failure.task_id == status.task_id)
                .map(|failure| failure.error.clone());
            events.push(AgentEvent::CompactionEnd {
                task_id: status.task_id,
                reason: status.reason,
                error,
            });
        }
    }
    // 一个 generation 的 turn 在它的 outcome 被提交时结束：`completing` 持有或终态，先到者。
    // 具体规则：状态首次变为 `completing`（不在 held 里）时结束并记入 held；若提交时直接到
    // `terminal` 而此前没在 `completing` 见过，也算结束。打开流时已经 `completing` 的任务
    // 预先放进了 held，因为它们的 turn 在流建立之前就结束过。非 generation 任务只可能产生
    // `task_failed`，不影响 turn。
    let mut turn_ended = false;
    for task in tasks.values() {
        let status = task.state.status();
        if task.kind == "kiana.generation"
            && status == TaskStatus::Completing
            && !held.contains(&task.id)
        {
            held.insert(task.id);
            turn_ended = true;
        }
        if status != TaskStatus::Terminal {
            continue;
        }
        if task.kind == "kiana.generation" && !held.remove(&task.id) {
            turn_ended = true;
        }
        if let Some(outcome) = task.state.outcome() {
            match outcome {
                crate::types::TaskOutcome::Faulted { error } => {
                    events.push(AgentEvent::TaskFailed {
                        task_id: task.id,
                        kind: task.kind.clone(),
                        message: error.message.clone(),
                    })
                }
                crate::types::TaskOutcome::Orphaned { reason } => {
                    events.push(AgentEvent::TaskFailed {
                        task_id: task.id,
                        kind: task.kind.clone(),
                        message: reason.clone(),
                    })
                }
                _ => {}
            }
        }
    }
    if turn_ended {
        events.push(AgentEvent::TurnEnd);
    }
    let run = now.live.run.as_ref();
    let run_before = was.live.run.as_ref();
    // 是否换 run 只看首个输入：同一 run 追加输入不算换 run。换 run 时先结束
    // 旧的 run，新的 `run_start` 在批次末尾发出。
    let run_changed = run.map(|run| run.inputs.first()) != run_before.map(|run| run.inputs.first());
    if run_changed && let Some(before) = run_before {
        events.push(AgentEvent::RunEnd {
            inputs: before.inputs.clone(),
        });
    }

    // 提交、文档状态，然后是"开始"的事件。
    for record in &submissions {
        events.push(AgentEvent::Submission {
            record: record.clone(),
        });
    }
    // TS 比较文档对象身份：本提交重建过的文档（哪怕内容相同）也算变化。
    if now.inbox != was.inbox || (now.inbox.is_some() && document_rebuilt(view_ops, "kiana.inbox"))
    {
        events.push(AgentEvent::InboxUpdate {
            items: queued(now.inbox.as_ref()),
        });
    }
    // 退役的文档读作它的初始值，与快照一致。
    if now.agent != was.agent || (now.agent.is_some() && document_rebuilt(view_ops, "kiana.agent"))
    {
        events.push(AgentEvent::AgentChanged {
            agent: now.agent.clone().unwrap_or_else(initial_agent),
        });
    }
    if now.usage != was.usage || (now.usage.is_some() && document_rebuilt(view_ops, "kiana.usage"))
    {
        events.push(AgentEvent::UsageChanged {
            usage: now.usage.clone().unwrap_or_else(initial_usage),
        });
    }
    for status in &compactions {
        if !compactions_before
            .iter()
            .any(|candidate| candidate.task_id == status.task_id)
        {
            events.push(AgentEvent::CompactionStart {
                task_id: status.task_id,
                reason: status.reason,
                blocking: status.blocking,
            });
        }
    }
    // `run_start`/`turn_start` 放在批次末尾，确保本提交的结果事件（消息、工具、提交）先于
    // 下一次运行开始。
    if let Some(run) = run
        && run_changed
    {
        events.push(AgentEvent::RunStart {
            inputs: run.inputs.clone(),
        });
    }
    if let Some(run) = run
        && run_before.map(|before| before.task_id) != Some(run.task_id)
        && tasks
            .get(&run.task_id)
            .is_some_and(|task| task.kind == "kiana.generation")
    {
        events.push(AgentEvent::TurnStart);
    }
    events
}

// ---------------------------------------------------------------------------
// messageChanges / callOf / toolUpdate / startsWith
// ---------------------------------------------------------------------------

const PARTIAL_PATH: [&str; 4] = ["docs", "kiana.live", "generation", "message"];

/// `kiana.live.generation.message` 的段路径（TS: `PARTIAL_PATH`）。
///
/// 只构造一次并复用；返回的静态切片供 `starts_with`/`message_changes` 做前缀匹配。
fn partial_path() -> &'static [Seg] {
    static PATH: LazyLock<Vec<Seg>> = LazyLock::new(|| {
        PARTIAL_PATH
            .iter()
            .map(|segment| Seg::Key((*segment).to_owned()))
            .collect()
    });
    &PATH
}

/// `prefix` 是否是 `path` 的段前缀（逐段相等，且前缀不能比路径长）。
fn starts_with(path: &[Seg], prefix: &[Seg]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path.iter()).all(|(a, b)| a == b)
}

/// 本批次是否重建了 `path` 处的 JSON 节点。
///
/// TS 的视图是不可变树：只有落在路径上（或把某个祖先整体 set）的 op 才会让该节点换成新对象，
/// 身份比较因此等价于"本提交碰过这个节点"。结构相同的重写（幂等 commit）在 TS 里仍是新对象。
///
/// 只看 op 的路径与种类，不比较值。`Op::Replace` 没有路径，被显式当作"重建一切"。
fn node_rebuilt(view_ops: &[Op], path: &[Seg]) -> bool {
    view_ops.iter().any(|op| {
        if matches!(op, Op::Replace(_)) {
            // 根替换（视图 op 实际上不会这样用）会重建一切。
            return true;
        }
        let Some(changed) = op_path(op) else {
            return false;
        };
        if starts_with(changed, path) {
            return true;
        }
        // 祖先被整体 set：整棵子树都是新的。
        starts_with(path, changed) && matches!(op, Op::Set(..))
    })
}

/// `view_ops` 是否重建了内建文档 `kind` 的值（TS: `now.inbox !== was.inbox` 的等价物）。
fn document_rebuilt(view_ops: &[Op], kind: &str) -> bool {
    let path = [Seg::Key("docs".to_owned()), Seg::Key(kind.to_owned())];
    node_rebuilt(view_ops, &path)
}

/// 取出 op 的路径；根替换 [`Op::Replace`] 没有路径，返回 `None`（由调用方单独处理）。
fn op_path(op: &Op) -> Option<&Path> {
    match op {
        Op::Replace(_) => None,
        Op::Set(path, _)
        | Op::Delete(path)
        | Op::Append(path, _)
        | Op::Truncate(path, _)
        | Op::Splice { path, .. }
        | Op::Permute(path, _) => Some(path),
    }
}

/// 把下标段转成 `usize`；负数或键段返回 `None`。
fn seg_as_index(seg: &Seg) -> Option<usize> {
    match seg {
        Seg::Index(value) if *value >= 0 => Some(*value as usize),
        _ => None,
    }
}

/// 只有键段返回 `Some`；下标段返回 `None`。
fn seg_as_key(seg: &Seg) -> Option<&str> {
    match seg {
        Seg::Key(key) => Some(key),
        Seg::Index(_) => None,
    }
}

/// 把 chord 的段原样映射成事件里的路径段（键保持键，数字下标保持数字）。
fn change_path(path: &[Seg]) -> Vec<ChangePathSegment> {
    path.iter()
        .map(|segment| match segment {
            Seg::Key(key) => ChangePathSegment::Key(key.clone()),
            Seg::Index(index) => ChangePathSegment::Index(*index),
        })
        .collect()
}

/// 把 in-flight 消息上的视图 op 翻译成 message changes（spec §9.4）。
///
/// 只处理落在 `docs/kiana.live/generation/message` 之下、且能被增量表达的形状：无法落到
/// content 增量的 op（祖先 set、content 数组的删除/替换、未知字段）退回一条
/// [`MessageChange::Message`]；单个块上的其他形状退回 [`MessageChange::Block`]。`usage`
/// 子树被跳过：用量由 `MessageUpdate.usage` 单独携带。
///
/// `message` 必须是变化后的消息（`translate` 传入 `after` 里解码出的 partial）；退回整条
/// 消息时直接克隆它。
fn message_changes(view_ops: &[Op], message: &AssistantMessage) -> Vec<MessageChange> {
    let mut changes: Vec<MessageChange> = Vec::new();
    // 整块发送的块已经含有本批次之后对它的全部变化。
    let mut whole: HashSet<usize> = HashSet::new();
    let partial = partial_path();
    for op in view_ops {
        let Some(path) = op_path(op) else {
            // 根替换没有路径：无法对应到消息内部，直接忽略（视图 op 实际不会这样用）。
            continue;
        };
        if !starts_with(path, partial) {
            // 整条消息或整个 generation 被替换。
            if starts_with(partial, path) {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            }
            // 与 partial 无关的 op（其他文档/字段）。
            continue;
        }
        let rest = &path[partial.len()..];
        // 用量单独随 message_update 发送，不重复进 changes。
        if rest.first().and_then(seg_as_key) == Some("usage") {
            continue;
        }
        // 消息上 content 之外的字段变化（usage 已在上面跳过）无法增量表达。
        if rest.first().and_then(seg_as_key) != Some("content") {
            return vec![MessageChange::Message {
                message: message.clone(),
            }];
        }
        // content 顶层的一次 splice：插入的块各自发一个 Start。删除或非 splice 都整条替换。
        if rest.len() == 1 {
            let Op::Splice {
                index,
                remove,
                items,
                ..
            } = op
            else {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            };
            if *remove != 0 {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            }
            for (offset, item) in items.iter().enumerate() {
                let Ok(block) = serde_json::from_value::<AssistantContentBlock>(item.clone())
                else {
                    // 解不出的块跳过，不产生 Start 事件。
                    continue;
                };
                let content_index = index + offset;
                changes.push(match &block {
                    AssistantContentBlock::Text(_) => MessageChange::TextStart {
                        content_index,
                        block,
                    },
                    AssistantContentBlock::Thinking(_) => MessageChange::ThinkingStart {
                        content_index,
                        block,
                    },
                    AssistantContentBlock::ToolCall(_) => MessageChange::ToolcallStart {
                        content_index,
                        block,
                    },
                });
            }
            continue;
        }
        // 更深路径必须是 content[数字]；否则整条替换。
        let Some(content_index) = rest.get(1).and_then(seg_as_index) else {
            return vec![MessageChange::Message {
                message: message.clone(),
            }];
        };
        let field = rest.get(2).and_then(seg_as_key);
        // 这个块已经整体发送过，后续 op 的结果已包含在块快照里。
        if whole.contains(&content_index) {
            continue;
        }
        // 只有恰好 3 段且是 Append 才是文本/思考增量。
        if let Op::Append(_, delta) = op
            && rest.len() == 3
            && matches!(field, Some("text") | Some("thinking"))
        {
            changes.push(match field {
                Some("text") => MessageChange::TextDelta {
                    content_index,
                    delta: delta.clone(),
                },
                _ => MessageChange::ThinkingDelta {
                    content_index,
                    delta: delta.clone(),
                },
            });
        } else if let Op::Append(_, delta) = op
            && field == Some("arguments")
        {
            // arguments 下任意深度的 Append：路径尾部就是参数里的位置。
            changes.push(MessageChange::ToolcallDelta {
                content_index,
                path: change_path(&rest[3..]),
                delta: delta.clone(),
            });
        } else {
            // 其余形状（Set/Delete/Splice/Permute…）按整块替换处理，并记住该下标。
            whole.insert(content_index);
            let Some(block) = message.content.get(content_index) else {
                // 越界下标没有可发送的块，静默跳过。
                continue;
            };
            changes.push(MessageChange::Block {
                content_index,
                block: block.clone(),
            });
        }
    }
    changes
}

/// 模型发起的调用的 [`ToolEventCall`]：只带自身字段，没有 parent。
fn call_of_tool(slot: &ToolSlot) -> ToolEventCall {
    ToolEventCall {
        tool_call_id: slot.call_id.clone(),
        tool_name: slot.name.clone(),
        task_id: slot.task_id,
        parent_tool_call_id: None,
        parent_task_id: None,
    }
}

/// 嵌套调用的 [`ToolEventCall`]：`toolCallId` 是 `<parentCallId>/<key>`，并带上父调用的
/// ID 与父任务（`call_of_nested` 无条件写 `task_id`）。
fn call_of_nested(slot: &NestedToolSlot) -> ToolEventCall {
    ToolEventCall {
        tool_call_id: slot.call_id.clone(),
        tool_name: slot.name.clone(),
        task_id: Some(slot.task_id),
        parent_tool_call_id: Some(slot.parent_call_id.clone()),
        parent_task_id: Some(slot.parent_task_id),
    }
}

/// 槽的只读视图，让 `tool_update` 同时接受模型调用与嵌套调用。
///
/// 两种槽的 `output`/`details`/`diagnostics` 字段同名，通过这里统一读取；调用身份则分别由
/// [`call_of_tool`] 与 [`call_of_nested`] 构造。
enum SlotRef<'a> {
    Tool(&'a ToolSlot),
    Nested(&'a NestedToolSlot),
}

impl<'a> SlotRef<'a> {
    fn output(&self) -> Option<&String> {
        match self {
            SlotRef::Tool(slot) => slot.output.as_ref(),
            SlotRef::Nested(slot) => slot.output.as_ref(),
        }
    }

    fn details(&self) -> Option<&JsonValue> {
        match self {
            SlotRef::Tool(slot) => slot.details.as_ref(),
            SlotRef::Nested(slot) => slot.details.as_ref(),
        }
    }

    fn diagnostics(&self) -> Option<&Vec<ToolDiagnostic>> {
        match self {
            SlotRef::Tool(slot) => slot.diagnostics.as_ref(),
            SlotRef::Nested(slot) => slot.diagnostics.as_ref(),
        }
    }

    fn call_of(&self) -> ToolEventCall {
        match self {
            SlotRef::Tool(slot) => call_of_tool(slot),
            SlotRef::Nested(slot) => call_of_nested(slot),
        }
    }
}

fn slot_ref_tool(slot: &ToolSlot) -> SlotRef<'_> {
    SlotRef::Tool(slot)
}

fn slot_ref_nested(slot: &NestedToolSlot) -> SlotRef<'_> {
    SlotRef::Nested(slot)
}

/// 一次工具更新的三个可选通道。
///
/// `None` 表示该字段在事件里省略（本批次没有变化），不是"删除"；删除分别用 `null` 与空
/// 数组表达。
struct ToolUpdate {
    output: Option<ToolOutputUpdate>,
    details: Option<JsonValue>,
    diagnostics: Option<Vec<ToolDiagnostic>>,
}

fn tool_update_event(call: ToolEventCall, update: ToolUpdate) -> AgentEvent {
    AgentEvent::ToolExecutionUpdate {
        call,
        output: update.output,
        details: update.details,
        diagnostics: update.diagnostics,
    }
}

/// 一个运行中槽的输出、details 与诊断变化（TS: `toolUpdate`）。
///
/// `at` 是槽列表在 `kiana.live` 下的键（`tools` 或 `nestedTools`），`index` 是它在当前列表
/// 中的位置；`slot` 是变化后的槽，`previous` 是变化前同一调用（按 call id / task id 匹配）
/// 的槽。返回 `None` 表示三个通道都没有变化，调用方不发射事件。
///
/// 输出优先用本提交的 op 重放（`Truncate` 累加、`Append` 拼接）；op 形状无法重放或值在
/// 没有对应 op 的情况下变化时退回 `Set`。details/diagnostics 在值变化或节点被重建时发送，
/// 删除分别写 `null`/`[]`。
fn tool_update(
    view_ops: &[Op],
    at: &str,
    index: usize,
    slot: SlotRef<'_>,
    previous: SlotRef<'_>,
) -> Option<ToolUpdate> {
    let output_path: Path = vec![
        Seg::Key("docs".to_owned()),
        Seg::Key("kiana.live".to_owned()),
        Seg::Key(at.to_owned()),
        Seg::Index(index as i64),
        Seg::Key("output".to_owned()),
    ];
    let mut trim_start: usize = 0;
    let mut append = String::new();
    let mut set = false;
    // 只观察该槽 output 叶子下的 op：truncate 累加裁剪量，append 拼接，其他任何 op
    // （set/delete/splice/permute）都按"整体替换"处理。
    for op in view_ops {
        let Some(path) = op_path(op) else {
            continue;
        };
        if !starts_with(path, &output_path) {
            continue;
        }
        match op {
            Op::Truncate(_, count) => trim_start += *count,
            Op::Append(_, text) => append.push_str(text),
            _ => set = true,
        }
    }
    // 没有可重放的 trim/append，但值确实变了（或 op 形状无法重放）：发 Set 兜底。
    // trim_start/append 只由 op 得出，不由值差反推。
    let output =
        if set || (slot.output() != previous.output() && trim_start == 0 && append.is_empty()) {
            Some(ToolOutputUpdate::Set {
                set: slot.output().cloned().unwrap_or_default(),
            })
        } else if trim_start > 0 || !append.is_empty() {
            Some(ToolOutputUpdate::Window {
                trim_start: if trim_start > 0 {
                    Some(trim_start)
                } else {
                    None
                },
                append: if append.is_empty() {
                    None
                } else {
                    Some(append)
                },
            })
        } else {
            None
        };
    // 安全重放会清空 running 槽的进度：删除的 details 发 `null`，删除的 diagnostics 发 `[]`。
    // TS 比较对象身份：本提交重建过该节点（内容可能相同）也要发。
    // 只有当前值为 Some 时"重建"才单独触发变化，避免把删除后的空值误报成变化。
    let slot_path = |field: &str| -> Vec<Seg> {
        vec![
            Seg::Key("docs".to_owned()),
            Seg::Key("kiana.live".to_owned()),
            Seg::Key(at.to_owned()),
            Seg::Index(index as i64),
            Seg::Key(field.to_owned()),
        ]
    };
    let details_rebuilt = node_rebuilt(view_ops, &slot_path("details"));
    let diagnostics_rebuilt = node_rebuilt(view_ops, &slot_path("diagnostics"));
    let details_changed =
        slot.details() != previous.details() || (slot.details().is_some() && details_rebuilt);
    let diagnostics_changed = slot.diagnostics() != previous.diagnostics()
        || (slot.diagnostics().is_some() && diagnostics_rebuilt);
    if output.is_none() && !details_changed && !diagnostics_changed {
        return None;
    }
    Some(ToolUpdate {
        output,
        details: if details_changed {
            Some(slot.details().cloned().unwrap_or(JsonValue::Null))
        } else {
            None
        },
        diagnostics: if diagnostics_changed {
            Some(slot.diagnostics().cloned().unwrap_or_default())
        } else {
            None
        },
    })
}

// 保持 `TaskGraphState`/`TableCommitChange` 等导入为文档可见性的一部分（本模块不直接使用
// 前者的字段，但 `SnapshotEvent` 与事件的类型面依赖同一批模块）。
const _: fn() = || {
    let _ = std::marker::PhantomData::<TaskGraphState>;
};
