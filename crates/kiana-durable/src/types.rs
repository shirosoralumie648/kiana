//! `types.ts` / `ids.ts` / `documents.ts` / `entries.ts` / `tasks.ts` / `errors.ts`。
//!
//! # Rust 形状与 TS 的对应
//!
//! - **ID 品牌**：TS 的 `Id<Kind, Type>` 是带唯一 symbol 品牌的 number。Rust 用
//!   `Id<Kind>`（phantom 品牌 + `i64`）表达：不同 kind 的 ID 不能互相赋值，JSON 里仍是数字。
//!   [`id_from_number`] / [`seq_from_number`] 是可信分配/解码边界的类型擦除入口。
//! - **联合体**：TS 的判别联合在 Rust 里有两种写法，取决于 JSON 键序：
//!   - 判别键在 TS 里第一个声明（`TaskState.status`、`TaskOutcome.status`、`StorageWrite.type`、
//!     `DocumentCommitChange.type`）→ 内部 tag 的 enum，serde 先写 tag，从而保持既有落盘键序；
//!   - 判别键不在最前（`DocumentContent.version` 在前、`ContextEdit.target` 在前、
//!     `SubmissionRecord` 的 identity 字段在前）→ 带判别字段的结构体 + 可选负载字段，
//!     键序按 TS 声明顺序，类型层面的互斥由构造函数与 Session 的运行期校验保证。
//! - **可选 JSON 值**：`data?: JsonValue` / `detail?: JsonValue` 里的显式 `null` 与省略不同，
//!   因此这些字段用自定义反序列化把 `null` 收成 `Some(Value::Null)`。
//! - **错误**：TS 的 `throw` 在 trait 方法里是 `Result<_, BoxError>`；`StorageRequestError`
//!   这类具体错误装箱后可用 `downcast_ref` 还原，对应 TS 的 `instanceof`。
//! - **watch/draft**：`@earendil-works/chord` 的 `watch.ts` / `delta/draft.ts` 在本工作区仍是
//!   骨架。这里先给出契约等价的 [`WatchHandle`] / [`DocDraft`]，待 chord 单元落地后由合并方
//!   换成 `kiana_chord::watch::WatchHandle` / `Draft`（见交付说明的 deviation）。
//!
//! # 这个文件负责什么
//! durable 的数据形状与 JSON 编码规则：ID 品牌、持久记录、文档定义与地址解析、任务状态机
//! 形状，以及 `Tx` / `Storage` / `Session` 等 trait 契约。行为实现分别在 `crate::session`、
//! `crate::storage`、`crate::harness`，它们都通过本文件的类型交换数据；因此这里的字段名、
//! serde 属性和键序就是磁盘与线格式本身，重排字段或改属性会直接改变落盘字节。
//!
//! # 记录到 JSON 的两条路径
//! - 普通结构体（[`ConversationRecord`]、[`DocumentRecord`]、[`TaskRecord`] 等）依赖 serde
//!   按**字段声明序**写键；声明序由 TS 对象字面量的插入序固定，不能重排。
//! - [`EntryRecord`]、[`EntryDraft`]、[`SubmissionRecord`] 的键序随 TS 调用点变化，声明序
//!   表达不了，因此手写 `Serialize` / `Deserialize`：反序列化把实际键序存进隐藏的
//!   `key_order` 提示，序列化按提示排序；提示为 `None` 时回落到规范序。该提示不参与
//!   `PartialEq`，只影响编码。
//!
//! # 注意事项
//! - `key` / `detail` 等字段用 `deserialize_present_json` 保留显式 JSON `null`；手写
//!   visitor 里的 `data` / `detail` 也用 `Some(Some(..))` 做同样的事。TS 里“键缺失”与
//!   “键为 null”是两种语义（单例地址与 `["family", null]`），不能换成 serde 的默认
//!   `Option` 行为。
//! - 手写 `Serialize` 里字段的 JSON 编码失败通过 `S::Error::custom` 上报，不做降级。
//! - 磁盘/线格式必须用 [`stringify_json`]（ECMAScript 数字格式）；SQLite 后端的
//!   `encode_json` 是它的包装。
//! - 会直接 panic 的入口（列表不穷尽）：`define_entry`（空 kind）、`resolve_address`
//!   （owner 不是安全整数）、`define_doc` / `define_doc_family`（版本不在 `1..=2^53-1`），
//!   以及 `DocDraft::value_mut` / `DerefMut`（持有结算状态的草稿：已准备时 panic
//!   `Prepared overlays are read-only`，已采纳/放弃时 panic `Cannot use a settled overlay`）。

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::Message;
use kiana_chord::context::Context;
use kiana_chord::delta::Op;
use kiana_telemetry::TelemetryCarrier;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use kiana_ai::{JsonObject, JsonValue};

// ---------------------------------------------------------------------------
// Branded numeric IDs
// ---------------------------------------------------------------------------

/// 记录种类的 phantom 品牌（TS: `Id<Kind>` 的 `Kind`）。
pub trait IdBrand {}

/// 带 `i64` 值、按 kind 区分类型的记录 ID（TS: `Id<Kind, Type>`）。
///
/// 品牌只存在于类型层：序列化与 `Display` 都是裸数字。
///
/// phantom 用 `fn() -> K` 而不是 `K`：`Id<K>` 的 `Send` / `Sync` 不取决于品牌类型，品牌
/// 只参与类型检查（也避免给品牌类型加 `Send + Sync` 约束）。
pub struct Id<K: IdBrand>(i64, PhantomData<fn() -> K>);

/// `ConversationId` 的品牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConversationBrand;
/// `EntryId` 的品牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryBrand;
/// `SubmissionId` 的品牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionBrand;
/// `DocumentId` 的品牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentBrand;
/// `TaskId<Result>` 的品牌；`R` 只用于类型层携带任务结果类型。
pub struct TaskBrand<R = JsonValue>(PhantomData<fn() -> R>);

impl IdBrand for ConversationBrand {}
impl IdBrand for EntryBrand {}
impl IdBrand for SubmissionBrand {}
impl IdBrand for DocumentBrand {}
impl<R> IdBrand for TaskBrand<R> {}

impl<K: IdBrand> Id<K> {
    /// 裸数值。
    pub fn get(&self) -> i64 {
        self.0
    }
}

/// `idFromNumber`：在可信边界上应用 ID 品牌。
pub const fn id_from_number<K: IdBrand>(value: i64) -> Id<K> {
    Id(value, PhantomData)
}

impl<K: IdBrand> Clone for Id<K> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K: IdBrand> Copy for Id<K> {}
impl<K: IdBrand> PartialEq for Id<K> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl<K: IdBrand> Eq for Id<K> {}
impl<K: IdBrand> PartialOrd for Id<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<K: IdBrand> Ord for Id<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}
impl<K: IdBrand> Hash for Id<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}
impl<K: IdBrand> fmt::Display for Id<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl<K: IdBrand> fmt::Debug for Id<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl<K: IdBrand> Serialize for Id<K> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de, K: IdBrand> Deserialize<'de> for Id<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        i64::deserialize(deserializer).map(|value| Id(value, PhantomData))
    }
}

/// 会话 ID。
pub type ConversationId = Id<ConversationBrand>;
/// 条目 ID。
pub type EntryId = Id<EntryBrand>;
/// 提交 ID。
pub type SubmissionId = Id<SubmissionBrand>;
/// 文档化身 ID。
pub type DocumentId = Id<DocumentBrand>;
/// 任务 ID；`R` 是任务的终态结果类型（只在类型层）。
pub type TaskId<R = JsonValue> = Id<TaskBrand<R>>;

/// 一次原子提交的严格递增序号（TS: `Seq`）；允许有空洞。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(i64);

impl Seq {
    /// 裸数值。
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// `seqFromNumber`：在可信存储边界上应用序号品牌。
pub const fn seq_from_number(value: i64) -> Seq {
    Seq(value)
}

/// 根会话永远使用这个保留 ID。
pub const ROOT_CONVERSATION_ID: ConversationId = id_from_number(1);

/// 扫描顺序（TS: `ScanOrder`）：`ascending` 由旧到新，`descending` 由新到旧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanOrder {
    /// 旧的在前。
    Ascending,
    /// 新的在前。
    Descending,
}

impl ScanOrder {
    /// TS 的字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            ScanOrder::Ascending => "ascending",
            ScanOrder::Descending => "descending",
        }
    }
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

/// `errors.ts`：错误消息与四个具体错误类型。
pub mod errors {
    use super::{ConversationId, JsonValue};

    /// trait 方法使用的装箱错误；具体错误可 `downcast_ref`。
    pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

    /// `errorMessage(error)`：`Error` 的 `message`（Rust 的 `Display`）。
    ///
    /// TS 还接受任意值（`String(error)`）；需要那种语义时用
    /// [`error_message_of_value`]。
    pub fn error_message(error: &(dyn std::error::Error + 'static)) -> String {
        error.to_string()
    }

    /// `String(error)` 的 JS 语义（非 `Error` 值，例如 JSON 值）。
    pub fn error_message_of_value(value: Option<&JsonValue>) -> String {
        kiana_json::js_string::js_string(value)
    }

    /// 事务在第一次表写入之后又读表。应先读完所有需要的行再写。
    #[derive(Debug, Clone, thiserror::Error)]
    #[error("Tx.{method}() cannot read tables after the first table write")]
    pub struct ReadAfterWrite {
        /// 被拒绝的方法名。
        pub method: String,
    }

    impl ReadAfterWrite {
        /// `new ReadAfterWrite(method)`。
        pub fn new(method: impl Into<String>) -> Self {
            ReadAfterWrite {
                method: method.into(),
            }
        }
    }

    /// Storage 读取拒绝了非法请求（未知会话、来自另一次扫描的游标等），没有持久效果。
    ///
    /// 与 Storage 抛出的其他错误不同，它只让这次读取失败，而不是让 Session 失败；从
    /// `commit()` 里抛出时和其他错误一样是致命的。
    #[derive(Debug, Clone, thiserror::Error)]
    #[error("{message}")]
    pub struct StorageRequestError {
        /// 完整消息。
        pub message: String,
    }

    impl StorageRequestError {
        /// `new StorageRequestError(message)`。
        pub fn new(message: impl Into<String>) -> Self {
            StorageRequestError {
                message: message.into(),
            }
        }
    }

    /// Session 已失败：不再运行或提交任何东西。重新打开 Session 即可从已提交状态恢复。
    #[derive(Debug, thiserror::Error)]
    #[error("Session failed after a storage error; close and reopen it")]
    pub struct SessionFailed {
        /// 第一个错误。
        #[source]
        pub cause: BoxError,
    }

    impl SessionFailed {
        /// `new SessionFailed(cause)`。
        pub fn new(cause: impl Into<BoxError>) -> Self {
            SessionFailed {
                cause: cause.into(),
            }
        }
    }

    /// Harness 以只读方式打开（`HarnessOptions.readOnly`）：带写入的提交、或会运行任务的调用
    /// 被拒绝。什么都没写，Harness 仍可用于读取。
    #[derive(Debug, Clone, thiserror::Error)]
    #[error("Read-only Harness: {what} refused")]
    pub struct ReadOnlySession {
        /// 被拒绝的操作描述。
        pub what: String,
    }

    impl ReadOnlySession {
        /// `new ReadOnlySession(what)`。
        pub fn new(what: impl Into<String>) -> Self {
            ReadOnlySession { what: what.into() }
        }
    }

    /// 提交遇到忙碌的会话，未被接纳。
    #[derive(Debug, Clone, thiserror::Error)]
    #[error("Conversation {conversation_id} is busy")]
    pub struct ConversationBusy {
        /// 忙碌的会话 ID。
        pub conversation_id: ConversationId,
    }

    impl ConversationBusy {
        /// `new ConversationBusy(conversationId)`。
        pub fn new(conversation_id: ConversationId) -> Self {
            ConversationBusy { conversation_id }
        }
    }
}

pub use errors::{
    BoxError, ConversationBusy, ReadAfterWrite, ReadOnlySession, SessionFailed,
    StorageRequestError, error_message, error_message_of_value,
};

// ---------------------------------------------------------------------------
// Shared JSON helpers
// ---------------------------------------------------------------------------

/// `details?: JsonValue` 一类字段：区分“键不存在”与“键是 null”。
///
/// serde 只在 JSON 对象里出现该键时才调用字段的反序列化器，所以被调用本身就意味着键
/// 存在；显式 `null` 由 [`JsonValue`] 收成 `Value::Null`，同样包成 `Some`。配合
/// `skip_serializing_if = "Option::is_none"` 即可让两种状态完整往返。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// JS `Number.isSafeInteger(value)` 的 JSON 版本：整数、有限、且 |v| ≤ 2^53-1。
///
/// `serde_json` 会把 `1e3` 这类带指数的 JSON 数字解析成 `f64`，所以不能只看
/// [`serde_json::Number::as_i64`]。
pub fn js_safe_integer(value: &JsonValue) -> Option<i64> {
    let JsonValue::Number(number) = value else {
        return None;
    };
    if let Some(integer) = number.as_i64() {
        // 用无符号绝对值比较：i64::MIN 直接取 `abs()` 会溢出，`unsigned_abs` 不会。
        return (integer.unsigned_abs() < (1u64 << 53)).then_some(integer);
    }
    // `serde_json` 对 `1e3`、`1.0` 这类字面量保留为 f64，`as_i64` 返回 None，所以还要
    // 走浮点路径：必须是有限值、没有小数部分、且落在 JS 安全整数范围内。超出 i64 的
    // u64 也会走到这里，并因上界判断被拒绝。
    let float = number.as_f64()?;
    if float.is_finite() && float.fract() == 0.0 && float.abs() <= 9_007_199_254_740_991.0 {
        return Some(float as i64);
    }
    None
}

/// 把一条记录写成与 `JSON.stringify` 逐字节一致的文本。
///
/// durable 的磁盘/线格式（JSONL 每行、SQLite/IndexedDB 的 `record` 列、`details` 等）要求
/// 键序 = 结构体声明序（见 [`EntryRecord`]/[`SubmissionRecord`]/[`TaskRecord`]/
/// [`DocumentRecord`] 的键序说明）且数字按 ECMAScript `Number::toString` 输出。
/// `serde_json::to_string` 的数字格式与 JS 不同（例如 `1e21`），所以 storage 后端与任何
/// 落盘/上线路径必须走这个函数，而不是直接 `serde_json::to_string`。
///
/// # Errors
/// 值无法序列化成 JSON 时（durable 的记录类型不会）。
pub fn stringify_json<T: ?Sized + Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(value)?;
    Ok(kiana_json::js_object::stringify(&value))
}

// ---------------------------------------------------------------------------
// Document semantics / addresses / contents
// ---------------------------------------------------------------------------

/// 文档作用域（TS: `DocumentSemantics.scope` 的判别值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// 整个 Session 一个。
    Session,
    /// 每个会话一个。
    Conversation,
    /// 每个任务一个。
    Task,
}

/// 文档历史的保留策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentHistory {
    /// 只保留当前状态。
    Latest,
    /// 保留 as-of 读取所需的历史。
    Rewindable,
}

/// fork 时子文档的初始值来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFork {
    /// 在 cutoff 处初始化。
    AsOf,
    /// 用当前状态初始化。
    Current,
    /// 用定义的初始值初始化。
    Initial,
}

/// 文档的所有权、寿命与 fork 行为（TS: `DocumentSemantics`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentSemantics {
    /// 作用域。
    pub scope: ScopeKind,
    /// 历史策略；只有 conversation 文档声明。
    pub history: Option<DocumentHistory>,
    /// fork 行为；只有 conversation 文档声明。
    pub fork: Option<DocumentFork>,
}

impl DocumentSemantics {
    /// Session 文档。
    pub const SESSION: DocumentSemantics = DocumentSemantics {
        scope: ScopeKind::Session,
        history: None,
        fork: None,
    };
    /// 任务文档。
    pub const TASK: DocumentSemantics = DocumentSemantics {
        scope: ScopeKind::Task,
        history: None,
        fork: None,
    };

    /// 只保留当前状态的会话文档。
    pub const fn latest(fork: DocumentFork) -> Self {
        DocumentSemantics {
            scope: ScopeKind::Conversation,
            history: Some(DocumentHistory::Latest),
            fork: Some(fork),
        }
    }

    /// 保留可 as-of 读取历史的会话文档。
    pub const fn rewindable(fork: DocumentFork) -> Self {
        DocumentSemantics {
            scope: ScopeKind::Conversation,
            history: Some(DocumentHistory::Rewindable),
            fork: Some(fork),
        }
    }
}

/// 记录的 `scope` 对象（TS: `DocumentRecord["scope"]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DocumentScope {
    /// Session 作用域。
    Session,
    /// 会话作用域。
    Conversation {
        /// 会话 ID。
        #[serde(rename = "conversationId")]
        conversation_id: ConversationId,
    },
    /// 任务作用域。
    Task {
        /// 任务 ID。
        #[serde(rename = "taskId")]
        task_id: TaskId,
    },
}

impl DocumentScope {
    /// 判别值。
    pub fn kind(&self) -> ScopeKind {
        match self {
            DocumentScope::Session => ScopeKind::Session,
            DocumentScope::Conversation { .. } => ScopeKind::Conversation,
            DocumentScope::Task { .. } => ScopeKind::Task,
        }
    }
}

/// 定义里的 `checkpointWhen` 收到的回放信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointInfo {
    /// 最新 base 之后已存的 delta 数（不含正在评估的这次变化）。
    pub deltas_since_base: u64,
}

/// 精确的逻辑地址（TS: `DocumentAddress`）。
///
/// `key` 在 TS 的类型里是 `string`，但 `resolveAddress` 只做 `as string` 断言、不做运行期
/// 校验（spec §2.12：key 不做类型校验），所以这里保持任意 JSON 值：字符串是正常路径，
/// 其他值只可能来自类型违约的调用，仍然进入地址与 `addressId`（不在解析处做类型校验）。
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentAddress {
    /// 稳定的文档 kind。
    pub kind: String,
    /// 精确作用域。
    pub scope: DocumentScope,
    /// 家族成员键；`None` 是单例。
    pub key: Option<JsonValue>,
}

// `JsonValue` 没有 `Eq` 实现（`serde_json::Number` 只保证 `PartialEq`），但 JSON 数字不能是
// NaN、判等是自反的，所以地址仍满足 `Eq` 的契约。
impl Eq for DocumentAddress {}

/// 存储创建的记录（TS: `DocumentCreate` = `DocumentRecord` 去掉 `createdAt`/`retiredAt`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCreate {
    /// 新化身 ID。
    pub id: DocumentId,
    /// 文档 kind。
    pub kind: String,
    /// 家族成员键。
    ///
    /// `key === undefined` 与 `key: null` 在 TS 里是两种地址（`["singleton"]` 与
    /// `["family", null]`），所以反序列化必须保留显式的 JSON `null`（同 `EntryRecord.data`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub key: Option<JsonValue>,
    /// 作用域。
    pub scope: DocumentScope,
    /// 历史策略（conversation 文档）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    /// fork 行为（conversation 文档）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
}

/// 持久化的文档化身记录（TS: `DocumentRecord`）。
///
/// **键序**：TS 的 storage 用 `{ ...create, createdAt, ...(retire ? { retiredAt } : {}) }` 打戳，
/// 所以 `createdAt`/`retiredAt` 在创建记录的 `id, kind, key, scope, history, fork` 之后。
/// 字段声明顺序即此顺序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRecord {
    /// 化身 ID；重建同一逻辑文档时绝不复用。
    pub id: DocumentId,
    /// 稳定的文档 kind。
    pub kind: String,
    /// 家族成员键；单例没有。
    ///
    /// 同 [`DocumentCreate::key`]：显式 JSON `null` 是家族成员 `["family", null]`，不是单例。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub key: Option<JsonValue>,
    /// 作用域。
    pub scope: DocumentScope,
    /// 历史策略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    /// fork 行为。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
    /// 创建它的提交（由 storage 打戳）。
    pub created_at: Seq,
    /// 退役它的提交；当前化身没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Seq>,
}

/// 记录作用域/历史/fork 的只读视图（`DocumentCreate` 与 `DocumentRecord` 共用）。
pub trait RecordSemantics {
    /// 化身 ID。
    fn record_id(&self) -> DocumentId;
    /// 文档 kind。
    fn record_kind(&self) -> &str;
    /// 作用域。
    fn record_scope(&self) -> &DocumentScope;
    /// 历史策略。
    fn record_history(&self) -> Option<DocumentHistory>;
    /// fork 行为。
    fn record_fork(&self) -> Option<DocumentFork>;
}

impl RecordSemantics for DocumentCreate {
    fn record_id(&self) -> DocumentId {
        self.id
    }
    fn record_kind(&self) -> &str {
        &self.kind
    }
    fn record_scope(&self) -> &DocumentScope {
        &self.scope
    }
    fn record_history(&self) -> Option<DocumentHistory> {
        self.history
    }
    fn record_fork(&self) -> Option<DocumentFork> {
        self.fork
    }
}

impl RecordSemantics for DocumentRecord {
    fn record_id(&self) -> DocumentId {
        self.id
    }
    fn record_kind(&self) -> &str {
        &self.kind
    }
    fn record_scope(&self) -> &DocumentScope {
        &self.scope
    }
    fn record_history(&self) -> Option<DocumentHistory> {
        self.history
    }
    fn record_fork(&self) -> Option<DocumentFork> {
        self.fork
    }
}

/// 文档内容判别值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentContentKind {
    /// 完整 base。
    Base,
    /// Chord 操作批次。
    Delta,
}

/// Chord 操作批次的 JSON 形状：wire tuple 数组。
///
/// `kiana_chord::delta::Op` 是强类型枚举而没有 serde 实现；这里用 chord 的官方
/// [`kiana_chord::delta::index::encode`] / [`kiana_chord::delta::index::decode`] 做桥接，
/// 从而在持久化时保持既有 `Op[]` 的 wire 形状。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ops(pub Vec<Op>);

impl Ops {
    /// 空批次。
    pub fn new() -> Self {
        Ops(Vec::new())
    }
}

impl From<Vec<Op>> for Ops {
    fn from(ops: Vec<Op>) -> Self {
        Ops(ops)
    }
}

impl std::ops::Deref for Ops {
    type Target = Vec<Op>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Ops {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Serialize for Ops {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        kiana_chord::delta::index::encode(&self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Ops {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 先按通用 JSON 数组读入，再交给 chord 的 wire 解码；无法识别的 op 结构会变成
        // 这次反序列化的错误，而不是 panic。
        let wire = Vec::<JsonValue>::deserialize(deserializer)?;
        kiana_chord::delta::index::decode(&wire)
            .map(Ops)
            .map_err(serde::de::Error::custom)
    }
}

/// 完整的 checkpoint 或 Chord 操作批次（TS: `DocumentContent`）。
///
/// `value` 与 `ops` 哪个有效由 `kind` 决定，但两者都是 `Option`：互斥性由构造函数
/// [`DocumentContent::base`] / [`DocumentContent::delta`] 建立；反序列化只按字段读取，
/// 不校验另一个负载字段是否被误填。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentContent {
    /// 值的定义版本。
    pub version: i64,
    /// `base` 或 `delta`。
    pub kind: DocumentContentKind,
    /// base 的完整值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<JsonObject>,
    /// delta 的操作。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<Ops>,
}

impl DocumentContent {
    /// 完整 base。
    pub fn base(version: i64, value: JsonObject) -> Self {
        DocumentContent {
            version,
            kind: DocumentContentKind::Base,
            value: Some(value),
            ops: None,
        }
    }

    /// 操作批次。
    pub fn delta(version: i64, ops: Vec<Op>) -> Self {
        DocumentContent {
            version,
            kind: DocumentContentKind::Delta,
            value: None,
            ops: Some(Ops(ops)),
        }
    }
}

/// 固定的 `"base"` 判别值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DocumentBaseKind {
    /// `"base"`。
    #[serde(rename = "base")]
    Base,
}

/// 文档创建时的完整 base（TS: `Extract<DocumentContent, {kind:"base"}>`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentBaseContent {
    /// 值的定义版本。
    pub version: i64,
    /// 固定 `"base"`。
    pub kind: DocumentBaseKind,
    /// 完整值。
    pub value: JsonObject,
}

impl DocumentBaseContent {
    /// 新建。
    pub fn new(version: i64, value: JsonObject) -> Self {
        DocumentBaseContent {
            version,
            kind: DocumentBaseKind::Base,
            value,
        }
    }
}

/// 文档读取的时间点：提交序号，或当前（TS: `DocumentPoint = Seq | "current"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentPoint {
    /// 当前状态。
    Current,
    /// 某个历史提交。
    At(Seq),
}

/// `"current"` 的固定判别值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentPointCurrent {
    /// `"current"`。
    #[serde(rename = "current")]
    Current,
}

impl Serialize for DocumentPoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            DocumentPoint::Current => DocumentPointCurrent::Current.serialize(serializer),
            DocumentPoint::At(seq) => seq.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for DocumentPoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 线上只有两种形状：字符串 `"current"` 或整数提交序号。其他字符串、布尔、对象、
        // 数组，以及不是 i64 的数字（小数或超出 i64 范围）都以 "invalid document point"
        // 拒绝，不会静默退化成 `Current`。
        let value = JsonValue::deserialize(deserializer)?;
        match value {
            JsonValue::String(text) if text == "current" => Ok(DocumentPoint::Current),
            JsonValue::Number(number) => number
                .as_i64()
                .map(|value| DocumentPoint::At(seq_from_number(value)))
                .ok_or_else(|| serde::de::Error::custom("invalid document point")),
            _ => Err(serde::de::Error::custom("invalid document point")),
        }
    }
}

/// 定义无关的文档复制来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCopySource {
    /// 源化身。
    pub id: DocumentId,
    /// 读取点。
    pub at: DocumentPoint,
}

/// 物化后的文档值与存储版本（TS: `StoredDocument`）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDocument {
    /// 化身记录。
    pub record: DocumentRecord,
    /// 值的定义版本。
    pub version: i64,
    /// 物化后的值。
    pub value: JsonObject,
    /// 物化 `value` 时在选中 base 之后回放的 delta 数。
    pub deltas_since_base: u64,
}

/// 一次原子存储提交里的单条记录或文档变更（TS: `StorageWrite`）。
///
/// TS 的联合体各分支大小本就不同；保持一 enum 一比一直翻译。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StorageWrite {
    /// 创建会话。
    #[serde(rename = "conversation")]
    Conversation {
        /// 记录。
        value: ConversationRecord,
    },
    /// 追加条目。
    #[serde(rename = "entry")]
    Entry {
        /// 记录。
        value: EntryRecord,
    },
    /// 任务状态替换。
    #[serde(rename = "task")]
    Task {
        /// 记录。
        value: TaskRecord,
    },
    /// 提交状态写入。
    #[serde(rename = "submission")]
    Submission {
        /// 记录。
        value: SubmissionRecord,
    },
    /// 创建文档化身。
    #[serde(rename = "document.create")]
    DocumentCreate {
        /// 新化身。
        record: DocumentCreate,
        /// 初始 base。
        content: DocumentBaseContent,
    },
    /// 从既有化身复制创建。
    #[serde(rename = "document.copy")]
    DocumentCopy {
        /// 新化身。
        record: DocumentCreate,
        /// 来源。
        source: DocumentCopySource,
    },
    /// 文档变化。
    #[serde(rename = "document.change")]
    DocumentChange {
        /// 化身 ID。
        id: DocumentId,
        /// base 或 delta。
        content: DocumentContent,
    },
    /// 退役化身。
    #[serde(rename = "document.retire")]
    DocumentRetire {
        /// 化身 ID。
        id: DocumentId,
    },
}

/// 只含表记录的子集（TS: `TableCommitChange`）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TableCommitChange {
    /// 会话。
    #[serde(rename = "conversation")]
    Conversation {
        /// 记录。
        value: ConversationRecord,
    },
    /// 条目。
    #[serde(rename = "entry")]
    Entry {
        /// 记录。
        value: EntryRecord,
    },
    /// 任务。
    #[serde(rename = "task")]
    Task {
        /// 记录。
        value: TaskRecord,
    },
    /// 提交。
    #[serde(rename = "submission")]
    Submission {
        /// 记录。
        value: SubmissionRecord,
    },
}

/// 一个文档化身的已提交变化（TS: `DocumentCommitChange`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DocumentCommitChange {
    /// 普通文档变化。
    #[serde(rename = "document")]
    Document {
        /// 化身记录。
        record: DocumentRecord,
        /// 拥有该文档的会话；任务文档由任务记录推出。只有 Session 文档是 `None`。
        #[serde(skip_serializing_if = "Option::is_none")]
        conversation_id: Option<ConversationId>,
        /// `value` 的定义版本；退役时为 `None`。
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<i64>,
        /// 采用的不可变版本；退役时为 `null`。
        value: Option<JsonObject>,
        /// 普通更新的操作；创建与退役为空。
        ops: Ops,
    },
    /// 定义无关的子文档初始化。
    #[serde(rename = "document.copy")]
    DocumentCopy {
        /// 化身记录。
        record: DocumentRecord,
        /// 拥有它的会话。
        conversation_id: ConversationId,
        /// 来源。
        source: DocumentCopySource,
    },
}

/// `CommitChange = TableCommitChange | DocumentCommitChange`。
///
/// 用 `untagged` 是因为 TS 的联合没有外层判别键。两个子枚举各自的 `type` 取值不相交
/// （表记录为 `conversation` / `entry` / `task` / `submission`，文档为 `document` /
/// `document.copy`），所以按声明序尝试不会误判；两者都不匹配时 serde 返回“不匹配任何
/// untagged 变体”的反序列化错误。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommitChange {
    /// 表记录。
    Table(TableCommitChange),
    /// 文档。
    Document(DocumentCommitChange),
}

impl From<TableCommitChange> for CommitChange {
    fn from(change: TableCommitChange) -> Self {
        CommitChange::Table(change)
    }
}

impl From<DocumentCommitChange> for CommitChange {
    fn from(change: DocumentCommitChange) -> Self {
        CommitChange::Document(change)
    }
}

/// 一次提交存储的全部不可变变化（TS: `CommitPublication`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitPublication {
    /// 提交序号。
    pub seq: Seq,
    /// 变化；顺序未指定。
    pub changes: Vec<CommitChange>,
}

// ---------------------------------------------------------------------------
// Conversations, entries, submissions, tasks
// ---------------------------------------------------------------------------

/// 会话的 fork 来源（TS: `ConversationRecord.parent`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationParent {
    /// 父会话。
    pub conversation_id: ConversationId,
    /// 继承历史上的最后一个条目（含）。
    pub at: EntryId,
}

/// 会话的创建者边（TS: `ConversationRecord.owner`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationOwner {
    /// 创建者所在会话。
    pub conversation_id: ConversationId,
    /// 创建者任务。
    pub task_id: TaskId,
}

/// 转录作用域的不可变身份、历史祖先与任务所有权。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    /// 会话 ID。
    pub id: ConversationId,
    /// fork 来源。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ConversationParent>,
    /// 创建者边。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<ConversationOwner>,
}

/// 对某个可见条目在模型上下文中的贡献的不可变覆盖（TS: `ContextEdit`）。
///
/// TS 是判别联合 `{target, action:"omit"} | {target, action:"replace", messages}`；
/// 判别键 `action` 在 `target` 之后声明，所以用结构体保持键序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEdit {
    /// 被省略或替换的条目。
    pub target: EntryId,
    /// `omit` 或 `replace`。
    pub action: ContextEditAction,
    /// 替换用的模型消息；`omit` 时缺省。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
}

/// `ContextEdit.action`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextEditAction {
    /// 省略。
    Omit,
    /// 替换。
    Replace,
}

/// 条目 `model` 里一条消息的持久形状（TS 构造序）。
///
/// 条目是持久化 JSON，键序必须与既有构造序一致（当前稳定契约，改动需同步测试）。
/// `kiana.tool-result` 的消息在 TS 里由
/// `appendToolResult` 的对象字面量构造（`role, toolCallId, toolName, content, details?,
/// usage?, isError, durationMs?, timestamp`，见 `harness/tool.ts`），而 kiana-ai 的
/// `ToolResultMessage` 接口把 `timestamp` 声明在 `durationMs` 之前。助手消息则不同：
/// provider 构造的消息没有 `thinkingLevel`，durable 的 `classify` 用
/// `message.thinkingLevel = thinkingLevel` 赋值，JS 把它插到对象末尾（`timestamp`、
/// `durationMs` 之后）。
pub(crate) fn persisted_message_json(message: &Message) -> Result<JsonValue, serde_json::Error> {
    let value = serde_json::to_value(message)?;
    match message {
        Message::ToolResult(_) => {
            // TS: `appendToolResult` 的字面量顺序；`nestedCalls` 不在 durable 的构造里，若未来出现，
            // 放在 `usage` 之后（kiana-ai 接口顺序）。
            Ok(reorder_object_keys(
                value,
                &[
                    "role",
                    "toolCallId",
                    "toolName",
                    "content",
                    "details",
                    "usage",
                    "nestedCalls",
                    "isError",
                    "durationMs",
                    "timestamp",
                ],
            ))
        }
        // durable `harness/prompt.ts#systemEntry` 的对象字面量顺序：`toolsRemoved` 在
        // `toolsAdded` 之前（kiana-ai 的 `SystemMessage` 按 TS interface 声明序序列化，见 §4.3.3）。
        Message::System(_) => Ok(reorder_object_keys(
            value,
            &[
                "role",
                "content",
                "sections",
                "toolsRemoved",
                "toolsAdded",
                "timestamp",
            ],
        )),
        // JS 给缺失属性赋值会把键插到末尾（`classify` 在 provider 构造之后写 thinkingLevel）。
        Message::Assistant(_) => Ok(move_object_key_last(value, "thinkingLevel")),
        _ => Ok(value),
    }
}

/// 按固定键序重排对象；未列出的键按原顺序附在末尾，保持可往返。
pub(crate) fn reorder_object_keys(value: JsonValue, keys: &[&str]) -> JsonValue {
    let JsonValue::Object(object) = value else {
        return value;
    };
    let mut ordered = serde_json::Map::new();
    for key in keys {
        if let Some(value) = object.get(*key) {
            ordered.insert((*key).to_owned(), value.clone());
        }
    }
    for (key, value) in &object {
        if !ordered.contains_key(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }
    JsonValue::Object(ordered)
}

/// 把 `key` 移到对象末尾（对象缺失该键时原样返回）。
fn move_object_key_last(value: JsonValue, key: &str) -> JsonValue {
    let JsonValue::Object(object) = value else {
        return value;
    };
    let mut ordered = serde_json::Map::new();
    for (candidate, value) in &object {
        if candidate != key {
            ordered.insert(candidate.clone(), value.clone());
        }
    }
    if let Some(value) = object.get(key) {
        ordered.insert(key.to_owned(), value.clone());
    }
    JsonValue::Object(ordered)
}

/// 持久记录的键序提示（TS 对象字面量的插入序）。
///
/// TS 的 `Tx.#appendEntry` 用 `copyJson({ ...rest, id, conversationId, head, byTaskId })`
/// 构造记录，`rest` 是调用点草稿字面量的键序；同一个 TS 类型在不同调用路径上会写出不同
/// 键序（`placeSummary` 的裸草稿以 `kind` 开头，带类型 token 的调用以 `model` 开头）。
/// Rust 的结构体字段序只能表达一种，因此追加路径在这里记录实际构造序；`None` 表示按
/// 结构体声明序。反序列化会记录读到的键序，从而在再次写出（fork/重新落盘）时保持。
#[doc(hidden)]
pub type KeyOrder = Option<Vec<String>>;

/// 按提示的键序排列「存在」的已知键，再按规范序补上提示里没有的键。
///
/// 提示里不属于规范表的键（反序列化时捕获的未知字段）与 `present` 为假的键都会被跳过；
/// 同一键在提示里重复出现时只保留第一次的位置。`order` 为 `None` 时结果就是规范序过滤
/// 掉不存在的键。返回的是 `'static` 规范名，因此调用方可以安全地重新排序后写回。
fn ordered_field_keys(
    order: Option<&[String]>,
    canonical: &[&'static str],
    present: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Vec::new();
    if let Some(order) = order {
        for key in order {
            if let Some(known) = canonical.iter().find(|candidate| **candidate == key)
                && present(known)
                && !keys.contains(known)
            {
                keys.push(known);
            }
        }
    }
    for key in canonical {
        if present(key) && !keys.contains(key) {
            keys.push(key);
        }
    }
    keys
}

/// 不可变转录事件，模型面与应用面的负载分开（TS: `EntryRecord`）。
///
/// **键序**：TS 的 `Tx.#appendEntry` 用 `copyJson({ ...rest, id, conversationId, head, byTaskId })`
/// 构造记录，`rest` 是草稿去掉 `head` 后的键（对象字面量插入序）。带类型 token 的写法
/// （`appendEntry(Token, ...)`，引擎的全部内建条目都走它）先展开 `TypedEntryDraft`
/// （调用点通常写 `model, data, head, edits`）再补 `kind`，所以记录键序是
/// `model, data, edits, kind, id, conversationId, head, byTaskId`（`head` 被解构后重排在
/// `conversationId` 之后）。裸草稿（调用方自建对象字面量）的键序由调用点决定，例如
/// `placeSummary` 的 `{ kind, head, model, data }` 会写成
/// `kind, model, data, id, conversationId, head, byTaskId`。同一个类型无法用字段声明序表达
/// 两种构造序，因此记录携带隐藏的 `key_order` 提示（追加路径写入，反序列化时捕获）。
#[derive(Debug, Clone)]
pub struct EntryRecord {
    /// 贡献给模型上下文的消息；展示或记账条目没有。
    pub model: Option<Vec<Message>>,
    /// 视图、扩展或记账逻辑消费的 JSON 负载。
    pub data: Option<JsonValue>,
    /// 对更早可见条目的上下文覆盖。
    pub edits: Option<Vec<ContextEdit>>,
    /// 应用定义的条目判别值。
    pub kind: String,
    /// 条目 ID。
    pub id: EntryId,
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 本条目选中的活动上下文首条目。
    pub head: Option<EntryId>,
    /// 追加本条目的任务。
    pub by_task_id: Option<TaskId>,
    /// 持久化键序提示；`None` 表示结构体声明序（见 [`KeyOrder`]）。
    #[doc(hidden)]
    pub key_order: KeyOrder,
}

/// `EntryRecord` 的规范字段序（结构体声明序）。
const ENTRY_RECORD_KEYS: [&str; 8] = [
    "model",
    "data",
    "edits",
    "kind",
    "id",
    "conversationId",
    "head",
    "byTaskId",
];

impl EntryRecord {
    fn has_field(&self, key: &str) -> bool {
        match key {
            "model" => self.model.is_some(),
            "data" => self.data.is_some(),
            "edits" => self.edits.is_some(),
            "kind" | "id" | "conversationId" => true,
            "head" => self.head.is_some(),
            "byTaskId" => self.by_task_id.is_some(),
            _ => false,
        }
    }

    // 只在 `has_field` 为真的键上调用；`model` / `data` / `edits` 的兜底分支（空数组或
    // `Null`）因此是防御性的，正常序列化不会为不存在的键写出占位值。
    fn field_json(&self, key: &str) -> Result<JsonValue, serde_json::Error> {
        match key {
            "model" => {
                let messages = self.model.as_deref().unwrap_or_default();
                let mut values = Vec::with_capacity(messages.len());
                for message in messages {
                    values.push(persisted_message_json(message)?);
                }
                Ok(JsonValue::Array(values))
            }
            "data" => Ok(self.data.clone().unwrap_or(JsonValue::Null)),
            "edits" => serde_json::to_value(self.edits.as_ref()),
            "kind" => Ok(JsonValue::String(self.kind.clone())),
            "id" => serde_json::to_value(self.id),
            "conversationId" => serde_json::to_value(self.conversation_id),
            "head" => serde_json::to_value(self.head),
            "byTaskId" => serde_json::to_value(self.by_task_id),
            _ => Ok(JsonValue::Null),
        }
    }
}

impl Serialize for EntryRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        // 两步：先决定键序（`key_order` 提示优先，再按规范序补齐），再逐键编码。反序列化
        // 时捕获但不在规范表里的未知键不会写回，所以跨版本往返会丢弃旧字段。
        let keys = ordered_field_keys(self.key_order.as_deref(), &ENTRY_RECORD_KEYS, |key| {
            self.has_field(key)
        });
        let mut object = serde_json::Map::new();
        for key in keys {
            object.insert(
                key.to_owned(),
                self.field_json(key).map_err(S::Error::custom)?,
            );
        }
        JsonValue::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EntryRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;

        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = EntryRecord;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an entry record")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<EntryRecord, A::Error> {
                let mut model: Option<Option<Vec<Message>>> = None;
                let mut data: Option<Option<JsonValue>> = None;
                let mut edits: Option<Option<Vec<ContextEdit>>> = None;
                let mut kind: Option<String> = None;
                let mut id: Option<EntryId> = None;
                let mut conversation_id: Option<ConversationId> = None;
                let mut head: Option<Option<EntryId>> = None;
                let mut by_task_id: Option<Option<TaskId>> = None;
                // 逐键读取，同时把实际出现顺序原样记录到 `order`（包括未知键与重复键）。
                // 未知键的值用 `IgnoredAny` 跳过；重复键的值后写覆盖先写，但顺序里两次
                // 出现都保留（`ordered_field_keys` 去重时取第一次的位置）。
                let mut order: Vec<String> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "model" => model = Some(map.next_value()?),
                        "data" => data = Some(Some(map.next_value::<JsonValue>()?)),
                        "edits" => edits = Some(map.next_value()?),
                        "kind" => kind = Some(map.next_value()?),
                        "id" => id = Some(map.next_value()?),
                        "conversationId" => conversation_id = Some(map.next_value()?),
                        "head" => head = Some(map.next_value()?),
                        "byTaskId" => by_task_id = Some(map.next_value()?),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                    order.push(key);
                }
                // `kind` / `id` / `conversationId` 是必填项，缺失时报 `missing_field`
                // （错误文本里是 JSON 键名）；其余字段缺省为 `None`。即使输入是空对象，
                // `key_order` 也会是 `Some(vec![])`，序列化效果与 `None` 相同。
                Ok(EntryRecord {
                    model: model.unwrap_or_default(),
                    data: data.unwrap_or_default(),
                    edits: edits.unwrap_or_default(),
                    kind: kind.ok_or_else(|| serde::de::Error::missing_field("kind"))?,
                    id: id.ok_or_else(|| serde::de::Error::missing_field("id"))?,
                    conversation_id: conversation_id
                        .ok_or_else(|| serde::de::Error::missing_field("conversationId"))?,
                    head: head.unwrap_or_default(),
                    by_task_id: by_task_id.unwrap_or_default(),
                    key_order: Some(order),
                })
            }
        }

        deserializer.deserialize_map(EntryVisitor)
    }
}

impl PartialEq for EntryRecord {
    fn eq(&self, other: &Self) -> bool {
        // 键序提示只影响序列化，不影响记录的值语义（TS 的 `toEqual` 与键序无关）。
        self.model == other.model
            && self.data == other.data
            && self.edits == other.edits
            && self.kind == other.kind
            && self.id == other.id
            && self.conversation_id == other.conversation_id
            && self.head == other.head
            && self.by_task_id == other.by_task_id
    }
}

/// `head` 的 `"self"` 判别值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryHeadSelf {
    /// `"self"`。
    #[serde(rename = "self")]
    SelfHead,
}

/// `EntryDraft.head`：条目 ID 或 `"self"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryHead {
    /// 新分配的条目 ID。
    SelfHead,
    /// 指定条目。
    Id(EntryId),
}

impl Serialize for EntryHead {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            EntryHead::SelfHead => EntryHeadSelf::SelfHead.serialize(serializer),
            EntryHead::Id(id) => id.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for EntryHead {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 与 `DocumentPoint` 同样的两种线形状：字符串 `"self"` 或整数条目 ID。其他值
        // （包括其他字符串与小数）报 "invalid entry head"，不会当作 `SelfHead`。
        let value = JsonValue::deserialize(deserializer)?;
        match value {
            JsonValue::String(text) if text == "self" => Ok(EntryHead::SelfHead),
            JsonValue::Number(number) => number
                .as_i64()
                .map(|value| EntryHead::Id(id_from_number::<EntryBrand>(value)))
                .ok_or_else(|| serde::de::Error::custom("invalid entry head")),
            _ => Err(serde::de::Error::custom("invalid entry head")),
        }
    }
}

/// Session 分配身份与任务归属之前的条目内容（TS: `EntryDraft`）。
///
/// TS 的草稿是调用点的对象字面量，键序随调用点而变；隐藏的 `key_order` 记录该字面量的
/// 插入序（`None` = 结构体声明序 `kind, model, data, head, edits`），供 inbox JSON 与最终
/// 记录复用。
#[derive(Debug, Clone)]
pub struct EntryDraft {
    /// 条目 kind。
    pub kind: String,
    /// 模型消息。
    pub model: Option<Vec<Message>>,
    /// JSON 负载。
    pub data: Option<JsonValue>,
    /// 活动上下文首条目。
    pub head: Option<EntryHead>,
    /// 上下文覆盖。
    pub edits: Option<Vec<ContextEdit>>,
    /// 键序提示；`None` 表示结构体声明序（见 [`KeyOrder`]）。
    #[doc(hidden)]
    pub key_order: KeyOrder,
}

/// `EntryDraft` 的规范字段序。
const ENTRY_DRAFT_KEYS: [&str; 5] = ["kind", "model", "data", "head", "edits"];

impl EntryDraft {
    fn has_field(&self, key: &str) -> bool {
        match key {
            "kind" => true,
            "model" => self.model.is_some(),
            "data" => self.data.is_some(),
            "head" => self.head.is_some(),
            "edits" => self.edits.is_some(),
            _ => false,
        }
    }

    // 同 `EntryRecord::field_json`：只在 `key_sequence` 选中的、存在的键上调用。
    fn field_json(&self, key: &str) -> Result<JsonValue, serde_json::Error> {
        match key {
            "kind" => Ok(JsonValue::String(self.kind.clone())),
            "model" => {
                let messages = self.model.as_deref().unwrap_or_default();
                let mut values = Vec::with_capacity(messages.len());
                for message in messages {
                    values.push(persisted_message_json(message)?);
                }
                Ok(JsonValue::Array(values))
            }
            "data" => Ok(self.data.clone().unwrap_or(JsonValue::Null)),
            "head" => serde_json::to_value(self.head),
            "edits" => serde_json::to_value(self.edits.as_ref()),
            _ => Ok(JsonValue::Null),
        }
    }

    /// 草稿当前的实际键序（提示优先，缺省为规范序），只含存在的键。
    pub(crate) fn key_sequence(&self) -> Vec<String> {
        ordered_field_keys(self.key_order.as_deref(), &ENTRY_DRAFT_KEYS, |key| {
            self.has_field(key)
        })
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

impl Serialize for EntryDraft {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut object = serde_json::Map::new();
        for key in self.key_sequence() {
            object.insert(
                key.clone(),
                self.field_json(&key).map_err(S::Error::custom)?,
            );
        }
        JsonValue::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EntryDraft {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DraftVisitor;

        impl<'de> serde::de::Visitor<'de> for DraftVisitor {
            type Value = EntryDraft;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an entry draft")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<EntryDraft, A::Error> {
                let mut kind: Option<String> = None;
                let mut model: Option<Option<Vec<Message>>> = None;
                let mut data: Option<Option<JsonValue>> = None;
                let mut head: Option<Option<EntryHead>> = None;
                let mut edits: Option<Option<Vec<ContextEdit>>> = None;
                // 与 `EntryRecord` 的 visitor 相同：记录实际键序（含未知键）、忽略未知键
                // 的值、重复键后写覆盖先写。
                let mut order: Vec<String> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "kind" => kind = Some(map.next_value()?),
                        "model" => model = Some(map.next_value()?),
                        "data" => data = Some(Some(map.next_value::<JsonValue>()?)),
                        "head" => head = Some(map.next_value()?),
                        "edits" => edits = Some(map.next_value()?),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                    order.push(key);
                }
                // 必填项只有 `kind`；`model` / `data` / `head` / `edits` 缺省为 `None`。
                Ok(EntryDraft {
                    kind: kind.ok_or_else(|| serde::de::Error::missing_field("kind"))?,
                    model: model.unwrap_or_default(),
                    data: data.unwrap_or_default(),
                    head: head.unwrap_or_default(),
                    edits: edits.unwrap_or_default(),
                    key_order: Some(order),
                })
            }
        }

        deserializer.deserialize_map(DraftVisitor)
    }
}

impl PartialEq for EntryDraft {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.model == other.model
            && self.data == other.data
            && self.head == other.head
            && self.edits == other.edits
    }
}

/// 带类型的条目值（Rust 侧的便捷视图，不参与序列化）。
#[derive(Debug, Clone, PartialEq)]
pub struct TypedEntry<D> {
    /// 原始记录。
    pub record: EntryRecord,
    /// 类型化的 `data`。
    pub data: D,
}

/// 带类型的条目草稿（TS: `TypedEntryDraft<D>`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TypedEntryDraft<D> {
    /// 模型消息。
    pub model: Option<Vec<Message>>,
    /// 类型化负载。
    pub data: D,
    /// 活动上下文首条目。
    pub head: Option<EntryHead>,
    /// 上下文覆盖。
    pub edits: Option<Vec<ContextEdit>>,
}

/// 条目 `data` 的编解码（Rust 侧的 `D` ↔ 记录里的 JSON `data`）。
///
/// TS 的 `D extends JsonValue` 在运行期不需要转换：`TypedEntry<D>` 只是给同一个 `data` 加静态
/// 类型。Rust 的记录只保存 `JsonValue`，所以类型化读写要在两者之间转换，由该 trait 承担。
/// 内建条目用到的 `data` 类型（[`entries::NoData`]、`entries::ToolResultData`、
/// `entries::CompactionData`）与 [`JsonValue`] 已实现；自定义 kind 的 `D` 需要自行实现。
pub trait EntryData: Send + Sized + 'static {
    /// 从记录里的 `data`（可能缺失）构造 `D`。
    fn from_entry_data(data: Option<&JsonValue>) -> Result<Self, BoxError>;
    /// 转成记录里的 `data`；`None` 表示不写 `data` 键。
    fn into_entry_data(self) -> Result<Option<JsonValue>, BoxError>;
}

impl EntryData for JsonValue {
    fn from_entry_data(data: Option<&JsonValue>) -> Result<Self, BoxError> {
        // TS 里 `TypedEntry<JsonValue>` 的 `data` 在记录缺失时运行期是 `undefined`；Rust 用
        // JSON `null` 表达这个“无值”位置。
        Ok(data.cloned().unwrap_or(JsonValue::Null))
    }

    fn into_entry_data(self) -> Result<Option<JsonValue>, BoxError> {
        Ok(Some(self))
    }
}

/// 提交类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionKind {
    /// 用户输入。
    Input,
    /// 被动条目写入。
    Write,
}

/// 提交状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionStatus {
    /// 已接纳但尚未进入转录。
    Queued,
    /// 已进入转录，由活动运行拥有。
    Placed,
    /// 成功。
    Done,
    /// 终态且无法再被回答。
    Unanswered,
}

/// 一次被接纳的用户输入或被动条目写入的持久生命周期（TS: `SubmissionRecord`）。
///
/// **键序**：TS 的 `Tx.createSubmission` 用 `copyJson({ ...create, id })` 打 ID，`create` 由
/// `harness/submissions.ts` 的 `fields = { conversationId, requestId, trace }` 展开后再补
/// `type, status, entry`，所以直接创建的记录键序是
/// `conversationId, requestId, trace, type, status, entry, id`（`id` 总在创建字段之后）。
/// 此后的结算（`applySubmissionChange` 的 `{ ...current, status, answer/reason/detail }`）
/// 保持已有键的位置、把新键追加到末尾：queued 的 `{...create, id}` 被放置时 `entry` 追加在
/// `id` 之后（`conversationId, ..., status, id, entry`），结算时 `answer` 再追加。同一类型
/// 无法用字段声明序表达这些路径，因此记录携带隐藏的 `key_order` 提示。
#[derive(Debug, Clone)]
pub struct SubmissionRecord {
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 宿主去重键，作用域是会话。
    pub request_id: Option<String>,
    /// 接纳 span 的适配器上下文；没有传播时缺省。
    pub trace: Option<TelemetryCarrier>,
    /// `input` 或 `write`。
    pub kind: SubmissionKind,
    /// 状态。
    pub status: SubmissionStatus,
    /// 相关条目。
    pub entry: Option<EntryId>,
    /// 提交 ID（TS 在 `{ ...create, id }` 里跟随创建字段插入）。
    pub id: SubmissionId,
    /// 回答条目（只有 input 的 `done`）。
    pub answer: Option<EntryId>,
    /// 终态原因。
    pub reason: Option<String>,
    /// 终态的细节负载。
    pub detail: Option<JsonValue>,
    /// 持久化键序提示；`None` 表示结构体声明序（见 [`KeyOrder`]）。
    #[doc(hidden)]
    pub key_order: KeyOrder,
}

/// `SubmissionRecord` 的规范字段序。
const SUBMISSION_RECORD_KEYS: [&str; 10] = [
    "conversationId",
    "requestId",
    "trace",
    "type",
    "status",
    "entry",
    "id",
    "answer",
    "reason",
    "detail",
];

impl SubmissionRecord {
    fn has_field(&self, key: &str) -> bool {
        match key {
            "conversationId" | "type" | "status" | "id" => true,
            "requestId" => self.request_id.is_some(),
            "trace" => self.trace.is_some(),
            "entry" => self.entry.is_some(),
            "answer" => self.answer.is_some(),
            "reason" => self.reason.is_some(),
            "detail" => self.detail.is_some(),
            _ => false,
        }
    }

    fn field_json(&self, key: &str) -> Result<JsonValue, serde_json::Error> {
        match key {
            "conversationId" => serde_json::to_value(self.conversation_id),
            "requestId" => serde_json::to_value(self.request_id.as_ref()),
            "trace" => serde_json::to_value(self.trace.as_ref()),
            "type" => serde_json::to_value(self.kind),
            "status" => serde_json::to_value(self.status),
            "entry" => serde_json::to_value(self.entry),
            "id" => serde_json::to_value(self.id),
            "answer" => serde_json::to_value(self.answer),
            "reason" => serde_json::to_value(self.reason.as_ref()),
            "detail" => Ok(self.detail.clone().unwrap_or(JsonValue::Null)),
            _ => Ok(JsonValue::Null),
        }
    }

    /// 按 TS 的 `{ ...current, ...change }` 语义合并键序：已有键保持位置，新键按 `updates`
    /// 的顺序追加到末尾。
    ///
    /// 起始序是当前提示过滤到已知键（`None` 时用规范序里的存在键）；返回值总是 `Some`。
    /// 提示里出现、但对应值为 `None` 的键仍会留在序里——序列化时 `has_field` 会再过滤
    /// 一次，所以不会写出空值。
    pub(crate) fn merged_key_order(&self, updates: &[&str]) -> KeyOrder {
        let mut order: Vec<String> = match &self.key_order {
            Some(order) => order
                .iter()
                .filter(|key| SUBMISSION_RECORD_KEYS.contains(&key.as_str()))
                .cloned()
                .collect(),
            None => SUBMISSION_RECORD_KEYS
                .iter()
                .filter(|key| self.has_field(key))
                .map(|key| (*key).to_owned())
                .collect(),
        };
        for update in updates {
            if !order.iter().any(|key| key == update) {
                order.push((*update).to_owned());
            }
        }
        Some(order)
    }
}

impl Serialize for SubmissionRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        // 与 `EntryRecord` 相同的两步：先按提示排出现在存在的规范键，再逐键编码。注意
        // JSON 键 `type` 对应 Rust 字段 `kind`（`field_json` 里做映射）。
        let keys = ordered_field_keys(self.key_order.as_deref(), &SUBMISSION_RECORD_KEYS, |key| {
            self.has_field(key)
        });
        let mut object = serde_json::Map::new();
        for key in keys {
            object.insert(
                key.to_owned(),
                self.field_json(key).map_err(S::Error::custom)?,
            );
        }
        JsonValue::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubmissionRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SubmissionVisitor;

        impl<'de> serde::de::Visitor<'de> for SubmissionVisitor {
            type Value = SubmissionRecord;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a submission record")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<SubmissionRecord, A::Error> {
                let mut conversation_id: Option<ConversationId> = None;
                let mut request_id: Option<Option<String>> = None;
                let mut trace: Option<Option<TelemetryCarrier>> = None;
                let mut kind: Option<SubmissionKind> = None;
                let mut status: Option<SubmissionStatus> = None;
                let mut entry: Option<Option<EntryId>> = None;
                let mut id: Option<SubmissionId> = None;
                let mut answer: Option<Option<EntryId>> = None;
                let mut reason: Option<Option<String>> = None;
                let mut detail: Option<Option<JsonValue>> = None;
                // 与 `EntryRecord` 的 visitor 相同：记录实际键序（含未知键）、忽略未知键
                // 的值、重复键后写覆盖先写。
                let mut order: Vec<String> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "conversationId" => conversation_id = Some(map.next_value()?),
                        "requestId" => request_id = Some(map.next_value()?),
                        "trace" => trace = Some(map.next_value()?),
                        "type" => kind = Some(map.next_value()?),
                        "status" => status = Some(map.next_value()?),
                        "entry" => entry = Some(map.next_value()?),
                        "id" => id = Some(map.next_value()?),
                        "answer" => answer = Some(map.next_value()?),
                        "reason" => reason = Some(map.next_value()?),
                        "detail" => detail = Some(Some(map.next_value::<JsonValue>()?)),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                    order.push(key);
                }
                // 必填项是 `conversationId` / `type` / `status` / `id`，缺失时报
                // `missing_field`（JSON 键名）；其余字段可缺省。
                Ok(SubmissionRecord {
                    conversation_id: conversation_id
                        .ok_or_else(|| serde::de::Error::missing_field("conversationId"))?,
                    request_id: request_id.unwrap_or_default(),
                    trace: trace.unwrap_or_default(),
                    kind: kind.ok_or_else(|| serde::de::Error::missing_field("type"))?,
                    status: status.ok_or_else(|| serde::de::Error::missing_field("status"))?,
                    entry: entry.unwrap_or_default(),
                    id: id.ok_or_else(|| serde::de::Error::missing_field("id"))?,
                    answer: answer.unwrap_or_default(),
                    reason: reason.unwrap_or_default(),
                    detail: detail.unwrap_or_default(),
                    key_order: Some(order),
                })
            }
        }

        deserializer.deserialize_map(SubmissionVisitor)
    }
}

impl PartialEq for SubmissionRecord {
    fn eq(&self, other: &Self) -> bool {
        // 键序提示只影响序列化，不影响记录的值语义。
        self.conversation_id == other.conversation_id
            && self.request_id == other.request_id
            && self.trace == other.trace
            && self.kind == other.kind
            && self.status == other.status
            && self.entry == other.entry
            && self.id == other.id
            && self.answer == other.answer
            && self.reason == other.reason
            && self.detail == other.detail
    }
}

impl SubmissionRecord {
    /// 只有 identity 字段、状态为 queued 的输入提交。
    pub fn input_queued(
        id: SubmissionId,
        conversation_id: ConversationId,
        request_id: Option<String>,
    ) -> Self {
        SubmissionRecord {
            trace: None,
            id,
            conversation_id,
            request_id,
            kind: SubmissionKind::Input,
            status: SubmissionStatus::Queued,
            entry: None,
            answer: None,
            reason: None,
            detail: None,
            // 规范序在 `entry` 缺省时就是 TS 的 `{...fields, type, status, id}`。
            key_order: None,
        }
    }

    /// 只有 identity 字段、状态为 queued 的写入提交。
    pub fn write_queued(
        id: SubmissionId,
        conversation_id: ConversationId,
        request_id: Option<String>,
    ) -> Self {
        SubmissionRecord {
            trace: None,
            id,
            conversation_id,
            request_id,
            kind: SubmissionKind::Write,
            status: SubmissionStatus::Queued,
            entry: None,
            answer: None,
            reason: None,
            detail: None,
            key_order: None,
        }
    }
}

/// 由 Session 分配 ID 之前的提交字段（TS: `SubmissionCreate`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionCreate {
    /// 接纳 span 的适配器上下文。
    pub trace: Option<TelemetryCarrier>,
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 宿主去重键。
    pub request_id: Option<String>,
    /// 类型。
    pub kind: SubmissionKind,
    /// 状态。
    pub status: SubmissionStatus,
    /// 相关条目。
    pub entry: Option<EntryId>,
    /// 回答条目。
    pub answer: Option<EntryId>,
    /// 终态原因。
    pub reason: Option<String>,
    /// 终态细节。
    pub detail: Option<JsonValue>,
}

/// 为某个提交落定的终态（TS: `SubmissionSettlement`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SubmissionSettlement {
    /// 成功，并给出回答条目。
    Done {
        /// 回答条目。
        answer: EntryId,
    },
    /// 终态且未回答。
    Unanswered {
        /// 原因。
        reason: String,
        /// 细节。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present_json"
        )]
        detail: Option<JsonValue>,
    },
}

/// 持久化的 JSON 安全错误快照（TS: `TaskOutcomeError`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcomeError {
    /// 消息。
    pub message: String,
    /// 结构化诊断数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub detail: Option<JsonValue>,
}

/// 任务终态的原因与可选结果（TS: `TaskOutcome<R>`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum TaskOutcome<R = JsonValue> {
    /// 完成。
    Completed {
        /// 结果。
        result: R,
    },
    /// 任务实现显式提交的预期失败。
    Failed {
        /// 错误。
        error: TaskOutcomeError,
        /// 可选结果。
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<R>,
    },
    /// 任务的 abort 协议处理了取消。
    Aborted {
        /// 原因。
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// 可选结果。
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<R>,
    },
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

/// 任务状态判别值（TS: `TaskState["status"]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// 可调度。
    Pending,
    /// 被一次内存中的调用保留。
    Running,
    /// 等待一组任务。
    Waiting,
    /// 睡到某个时钟点。
    Sleeping,
    /// 结果已定，等待其下普通工作收尾。
    Completing,
    /// 永久落定。
    Terminal,
}

/// 等待任务如何对待被等待的任务（TS: `JoinPolicy`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JoinPolicy {
    /// 任一失败即失败。
    FailFast,
    /// 全部落定。
    AllSettled,
}

/// `TaskOutcome["status"]`：终态结果的判别值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskOutcomeStatus {
    /// 完成。
    Completed,
    /// 失败。
    Failed,
    /// 中止。
    Aborted,
    /// 无法恢复。
    Orphaned,
    /// 契约失败。
    Faulted,
}

/// 任务的完整持久执行状态（TS: `TaskState<S, R>`）。
///
/// 状态集合就是下面六个变体。本文件只定义形状，不校验迁移顺序；迁移由 harness 的调度器
/// 与任务实现写入（`harness/scheduler.rs`、`harness/generation.rs` 中的构造点）。
///
/// 字段与状态的对应关系是不变量：`checkpoint` 只出现在 `pending` / `running` /
/// `waiting` / `sleeping`，`outcome` 只出现在 `completing` / `terminal`，由
/// [`TaskState::checkpoint`] / [`TaskState::outcome`] 的返回值表达。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum TaskState<S = JsonValue, R = JsonValue> {
    /// 可调度。
    Pending {
        /// 完整恢复状态。
        checkpoint: S,
    },
    /// 被一次内存中的调用保留。
    Running {
        /// 完整恢复状态。
        checkpoint: S,
    },
    /// 没有调用，直到 `on` 全部终态；然后在 `checkpoint` 恢复。
    Waiting {
        /// 完整恢复状态。
        checkpoint: S,
        /// 等待的任务。
        on: Vec<TaskId>,
        /// 等待策略。
        policy: JoinPolicy,
    },
    /// 没有调用，直到 Harness 时钟到达 `until`。
    Sleeping {
        /// 完整恢复状态。
        checkpoint: S,
        /// 毫秒时间点（`Date.now()` 语义，见 ARCHITECTURE §4.3(7)）。
        until: i64,
    },
    /// 结果已定，等待其下普通工作收尾。
    Completing {
        /// 结果。
        outcome: TaskOutcome<R>,
    },
    /// 永久落定的结果收据。
    Terminal {
        /// 结果。
        outcome: TaskOutcome<R>,
    },
}

impl<S, R> TaskState<S, R> {
    /// 状态判别值。
    pub fn status(&self) -> TaskStatus {
        match self {
            TaskState::Pending { .. } => TaskStatus::Pending,
            TaskState::Running { .. } => TaskStatus::Running,
            TaskState::Waiting { .. } => TaskStatus::Waiting,
            TaskState::Sleeping { .. } => TaskStatus::Sleeping,
            TaskState::Completing { .. } => TaskStatus::Completing,
            TaskState::Terminal { .. } => TaskStatus::Terminal,
        }
    }

    /// 恢复 checkpoint（只有活动状态有）。
    pub fn checkpoint(&self) -> Option<&S> {
        match self {
            TaskState::Pending { checkpoint }
            | TaskState::Running { checkpoint }
            | TaskState::Waiting { checkpoint, .. }
            | TaskState::Sleeping { checkpoint, .. } => Some(checkpoint),
            TaskState::Completing { .. } | TaskState::Terminal { .. } => None,
        }
    }

    /// 终态结果（只有 `completing`/`terminal` 有）。
    pub fn outcome(&self) -> Option<&TaskOutcome<R>> {
        match self {
            TaskState::Completing { outcome } | TaskState::Terminal { outcome } => Some(outcome),
            _ => None,
        }
    }
}

/// 任务状态机记录共享的 identity / 定义 / 调度字段（TS: `TaskRecordBase`）。
///
/// **键序**：TS 的 `Tx.#createTask` 用 `copyJson({ id, conversationId, kind, version, input,
/// trace, links, owner, background, abortRequested, abandonOnRestart, state })` 构造记录
/// （`copyJson` 省略值为 `undefined` 的键）。之后 `setTask`/`#stampTimes`/调度器只在末尾
/// 追加 `startedAt`、`endedAt`、`restarts`、`abortReason`、`memos`。字段声明顺序即
/// 创建期顺序加这些后续追加字段；`serde` 反序列化按名匹配，与顺序无关。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord<I = JsonValue, S = JsonValue, R = JsonValue> {
    /// 任务 ID。
    pub id: TaskId<R>,
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 注册的任务定义名。
    pub kind: String,
    /// 定义版本。
    pub version: i64,
    /// 原始输入（活动或终态都保留）。
    pub input: I,
    /// 创建该任务的适配器上下文。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<TelemetryCarrier>,
    /// 首次调用的不可变创建期链接。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<TelemetryCarrier>>,
    /// 子任务的拥有者任务；会话拥有的任务没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    /// 会话拥有的任务是否排除在普通 idle 等待、会话 abort 与级联之外。
    pub background: bool,
    /// 在提交运行态进展前检查的持久 abort 标记。
    pub abort_requested: bool,
    /// 来自 `TaskOptions.abandonOnRestart`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandon_on_restart: Option<bool>,
    /// 完整状态。
    pub state: TaskState<S, R>,
    /// 第一次变为 `running` 的墙钟毫秒（`Date.now()` 语义，见 ARCHITECTURE §4.3(7)）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// 变为 `terminal` 的墙钟毫秒（`Date.now()` 语义，见 ARCHITECTURE §4.3(7)）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    /// 连续被崩溃/驱逐/重置打断的次数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u64>,
    /// 标记来源：`restart` 表示重启后被放弃的任务。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<TaskAbortReason>,
    /// 小而先写先赢的备忘值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memos: Option<IndexMap<String, JsonValue>>,
}

/// `abortReason` 的固定值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskAbortReason {
    /// 重启后放弃。
    #[serde(rename = "restart")]
    Restart,
}

/// 活动任务记录（TS: `RunningTask`）。
pub type RunningTask<I = JsonValue, S = JsonValue, R = JsonValue> = TaskRecord<I, S, R>;

/// 任务下一次为自己提交的状态（TS: `NextTaskState`）。
pub type NextTaskState<S = JsonValue, R = JsonValue> = TaskState<S, R>;

/// 任务拥有权的持久形状（TS: `TaskOwnership`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TaskOwnership {
    /// 由会话拥有（顶层任务）。
    Conversation,
    /// 由另一个任务拥有（子任务）。
    Task {
        /// 拥有者任务。
        #[serde(rename = "taskId")]
        task_id: TaskId,
    },
}

/// 显式选择的会话拥有权（TS: `ConversationOwnership`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConversationOwnership {
    /// 无拥有者。
    Ownerless,
    /// 由任务拥有。
    Task {
        /// 拥有者任务。
        #[serde(rename = "taskId")]
        task_id: TaskId,
    },
}

/// `trace` 的特殊值 `"root"` 或一个 carrier（TS: `TelemetryCarrier | "root"`）。
#[derive(Debug, Clone, PartialEq)]
pub enum TaskTrace {
    /// 从适配器根开始，不携带存储的 carrier。
    Root,
    /// 携带的上下文。
    Carrier(TelemetryCarrier),
}

/// 持久任务的创建选项（TS: `TaskOptions`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskOptions {
    /// 跟踪上下文：缺省继承创建者的 carrier；`Root` 从根开始。
    pub trace: Option<TaskTrace>,
    /// 不可变创建期链接，附加到首次调用的 span。
    pub links: Option<Vec<TelemetryCarrier>>,
    /// 拥有权（必填）。
    pub ownership: TaskOwnership,
    /// 默认是拥有者任务的会话，或事务绑定的会话。
    pub conversation_id: Option<ConversationId>,
    /// 只对会话拥有的任务有效：排除在普通 idle 等待、会话 abort 与级联之外。
    pub background: Option<bool>,
    /// 创建者只在内存里等待，重启后不再恢复。
    pub abandon_on_restart: Option<bool>,
}

impl TaskOptions {
    /// 基本选项。
    pub fn new(ownership: TaskOwnership) -> Self {
        TaskOptions {
            trace: None,
            links: None,
            ownership,
            conversation_id: None,
            background: None,
            abandon_on_restart: None,
        }
    }
}

/// 只读游标（TS: `Cursor`）：后端拥有的 JSON 续页状态，调用方只原样回传。
pub type Cursor = JsonObject;

/// 一次有序扫描的结果与可选续页状态（TS: `Page<T, C>`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T, C = Cursor> {
    /// 这一页的条目。
    pub items: Vec<T>,
    /// 续页状态。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<C>,
}

/// 一个全局条目与持久化它的提交序号（TS `Storage.entry` 的内联返回形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEntry {
    /// 条目记录。
    pub entry: EntryRecord,
    /// 持久化该条目的提交序号。
    pub commit_seq: Seq,
}

/// 有序会话扫描的可选过滤（TS: `ConversationQuery`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationQuery {
    /// 创建者会话。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_conversation_id: Option<ConversationId>,
    /// 创建者任务。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_task_id: Option<TaskId>,
    /// 默认 `ascending`；有游标时以游标为准。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
}

/// 单个会话 fork-aware 历史的闭区间过滤（TS: `EntryQuery`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryQuery {
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// 可能返回的最旧条目。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_entry_id: Option<EntryId>,
    /// 可能返回的最新条目。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_entry_id: Option<EntryId>,
    /// 默认 `descending`；有游标时以游标为准。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
}

/// 任务记录扫描的可选过滤（TS: `TaskQuery`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskQuery {
    /// 会话 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// 任务定义名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// 状态。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    /// abort 标记。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_requested: Option<bool>,
    /// background 标志。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// 默认 `ascending`；有游标时以游标为准。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
}

/// 提交记录扫描的可选过滤（TS: `SubmissionQuery`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionQuery {
    /// 会话 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// 状态。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SubmissionStatus>,
    /// 默认 `ascending`；有游标时以游标为准。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
}

/// 文档化身扫描的查询（TS: `DocumentQuery`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentQuery {
    /// 精确作用域。
    pub scope: DocumentScope,
    /// 读取点。
    pub at: DocumentPoint,
    /// 文档 kind。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

// ---------------------------------------------------------------------------
// IDs of a record kind, for `Storage.mint_id`
// ---------------------------------------------------------------------------

/// `mintId` 的运行时种类擦除（TS 的品牌在运行期不存在）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdKind {
    /// 会话。
    Conversation,
    /// 条目。
    Entry,
    /// 任务。
    Task,
    /// 提交。
    Submission,
    /// 文档。
    Document,
}

// ---------------------------------------------------------------------------
// Task definitions (tasks.ts)
// ---------------------------------------------------------------------------

/// 一个 checkpoint 阶段的处理器（TS: `PhaseHandler`）。
#[async_trait]
pub trait TaskPhase<I = JsonValue, S = JsonValue, R = JsonValue, H = ()>: Send + Sync {
    /// 运行一个阶段。
    async fn run(
        &self,
        task: &RunningTask<I, S, R>,
        runtime: &dyn crate::harness::types::TaskRuntime<I, S, R, H>,
        context: &dyn Context,
    ) -> Result<(), BoxError>;
}

/// 迁移函数：`(input, checkpoint, fromVersion) -> (input, checkpoint)`。
pub type TaskMigrate<I, S> =
    Arc<dyn Fn(JsonValue, JsonValue, i64) -> Result<(I, S), BoxError> + Send + Sync>;

/// 可执行的持久状态机定义（TS: `TaskDefinition<I, S, R, H>`）。
pub struct TaskDefinition<I = JsonValue, S = JsonValue, R = JsonValue, H = ()> {
    /// 注册名。
    pub name: String,
    /// 定义版本。
    pub version: i64,
    /// 新任务的第一个持久 checkpoint（`initial(input)`）。
    pub initial: Arc<dyn Fn(I) -> S + Send + Sync>,
    /// 穷尽的阶段表。
    ///
    /// 键是 checkpoint 里的 `phase` 字符串；调度器按名查找，缺失时该阶段失败
    /// （`erased(...).phases[checkpoint.phase] is not a function`）。
    pub phases: IndexMap<String, Arc<dyn TaskPhase<I, S, R, H>>>,
    /// abort 协议的处理器。
    pub abort: Arc<dyn TaskPhase<I, S, R, H>>,
    /// 旧版本记录的迁移。
    pub migrate: Option<TaskMigrate<I, S>>,
    /// 任务声明的 hook 处理器表。
    pub hooks: Option<Arc<H>>,
}

impl<I, S, R, H> Clone for TaskDefinition<I, S, R, H> {
    fn clone(&self) -> Self {
        TaskDefinition {
            name: self.name.clone(),
            version: self.version,
            initial: Arc::clone(&self.initial),
            phases: self.phases.clone(),
            abort: Arc::clone(&self.abort),
            migrate: self.migrate.clone(),
            hooks: self.hooks.clone(),
        }
    }
}

impl<I, S, R, H> fmt::Debug for TaskDefinition<I, S, R, H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskDefinition")
            .field("name", &self.name)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// 带类型的可执行任务定义（TS: `Task<I, S, R, H>`）。
pub struct Task<I = JsonValue, S = JsonValue, R = JsonValue, H = ()> {
    /// 定义。
    pub definition: Arc<TaskDefinition<I, S, R, H>>,
}

impl<I, S, R, H> Clone for Task<I, S, R, H> {
    fn clone(&self) -> Self {
        Task {
            definition: Arc::clone(&self.definition),
        }
    }
}

impl<I, S, R, H> fmt::Debug for Task<I, S, R, H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Task")
            .field("definition", &self.definition)
            .finish()
    }
}

/// 提取任务定义的 hook 类型（TS: `HooksOf<K>`）。
pub trait TaskLike {
    /// 定义声明的 hook 处理器表类型。
    type Hooks;
    /// 注册名。
    fn name(&self) -> &str;
}

impl<I, S, R, H> TaskLike for Task<I, S, R, H> {
    type Hooks = H;
    fn name(&self) -> &str {
        &self.definition.name
    }
}

/// TS: `HooksOf<K>`。
pub type HooksOf<K> = <K as TaskLike>::Hooks;

/// `tasks.ts`：`defineTask`。
pub mod tasks {
    use std::sync::Arc;

    use super::{JsonValue, Task, TaskDefinition};

    /// 定义一个可执行任务。注册它，Harness 才能运行该种类的任务。
    pub fn define_task<I, S, R, H>(definition: TaskDefinition<I, S, R, H>) -> Task<I, S, R, H> {
        Task {
            definition: Arc::new(definition),
        }
    }

    /// 定义 `JsonValue` 擦除形态的便捷入口。
    pub fn define_json_task(
        definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()>,
    ) -> Task<JsonValue, JsonValue, JsonValue, ()> {
        define_task(definition)
    }
}

// ---------------------------------------------------------------------------
// entries.ts
// ---------------------------------------------------------------------------

/// `entries.ts`：带类型的条目 kind 与六个内建种类。
pub mod entries {
    #![allow(non_upper_case_globals)]
    use std::marker::PhantomData;

    use super::{
        BoxError, EntryData, EntryDraft, EntryRecord, JsonValue, TypedEntry, TypedEntryDraft,
    };

    use crate::harness::types::{CompactionReason, ToolDiagnostic};

    /// 无数据的条目（TS 的 `never`）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoData;

    impl EntryData for NoData {
        fn from_entry_data(_data: Option<&JsonValue>) -> Result<Self, BoxError> {
            Ok(NoData)
        }

        fn into_entry_data(self) -> Result<Option<JsonValue>, BoxError> {
            // TS 的 `TypedEntryDraft<never>` 没有 `data` 键。
            Ok(None)
        }
    }

    /// 带类型的条目 kind，带 `is()` 收窄守卫（TS: `Entry<D>`）。
    ///
    /// `kind` 用 [`std::borrow::Cow`] 保存：内建种类是 `'static` 字面量，[`define_entry`] 也接受运行期
    /// 拼出的 `String`（TS 的 `defineEntry(kind: string)` 只拒绝空串）。
    pub struct Entry<D = NoData> {
        /// 条目判别值。
        pub kind: std::borrow::Cow<'static, str>,
        marker: PhantomData<fn() -> D>,
    }

    impl<D> Entry<D> {
        /// 判断记录是否属于该 kind（TS: `is(entry)`）。
        pub fn is(&self, entry: &EntryRecord) -> bool {
            entry.kind == self.kind.as_ref()
        }

        /// 类型化的视图。
        pub fn typed(&self, entry: EntryRecord, data: D) -> TypedEntry<D> {
            TypedEntry {
                record: entry,
                data,
            }
        }
    }

    impl<D: EntryData> Entry<D> {
        /// 把一条记录收窄成类型化视图；kind 与 token 不符时返回 `None`。
        ///
        /// 对应 TS 的 `entry(token, id)` 重载在 `TypeScript` 里由 `Entry.is()` 完成的收窄。
        ///
        /// # Errors
        /// 记录里的 `data` 无法解码成 `D` 时。
        pub fn typed_entry(&self, record: EntryRecord) -> Result<Option<TypedEntry<D>>, BoxError> {
            if record.kind != self.kind.as_ref() {
                return Ok(None);
            }
            let data = D::from_entry_data(record.data.as_ref())?;
            Ok(Some(TypedEntry { record, data }))
        }

        /// 把类型化草稿补上 token 的 `kind`（TS: `appendEntry(token, ...)` 重载）。
        ///
        /// # Errors
        /// `data` 无法编码成 JSON 时。
        pub fn typed_draft(&self, value: TypedEntryDraft<D>) -> Result<EntryDraft, BoxError> {
            let TypedEntryDraft {
                model,
                data,
                head,
                edits,
            } = value;
            Ok(EntryDraft {
                kind: self.kind.to_string(),
                model,
                data: data.into_entry_data()?,
                head,
                edits,
                // TS: `{ ...third, kind }`——草稿字面量的键在前，`kind` 追加在末尾。
                key_order: Some(
                    ["model", "data", "head", "edits"]
                        .iter()
                        .map(|key| (*key).to_owned())
                        .collect(),
                ),
            })
        }
    }

    impl<D> Clone for Entry<D> {
        fn clone(&self) -> Self {
            Entry {
                kind: self.kind.clone(),
                marker: PhantomData,
            }
        }
    }
    impl<D> std::fmt::Debug for Entry<D> {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("Entry")
                .field("kind", &self.kind)
                .finish()
        }
    }

    /// `defineEntry`：定义带类型的条目 kind。
    ///
    /// TS 的签名是 `defineEntry(kind: string)`：任何非空运行期字符串都合法（只有空串抛
    /// `TypeError("Entry kind must be a non-empty string")`）。Rust 侧接受 `&'static str`
    /// 或运行期 `String`；空串同样 `panic!`，消息逐字相同。
    pub fn define_entry<D>(kind: impl Into<std::borrow::Cow<'static, str>>) -> Entry<D> {
        let kind = kind.into();
        if kind.is_empty() {
            panic!("Entry kind must be a non-empty string");
        }
        Entry {
            kind,
            marker: PhantomData,
        }
    }

    /// 工具结果的 `data` 形状。
    #[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct ToolResultData {
        /// 结构化诊断。
        pub diagnostics: Vec<ToolDiagnostic>,
    }

    impl EntryData for ToolResultData {
        fn from_entry_data(data: Option<&JsonValue>) -> Result<Self, BoxError> {
            Ok(serde_json::from_value(
                data.cloned().unwrap_or(JsonValue::Null),
            )?)
        }

        fn into_entry_data(self) -> Result<Option<JsonValue>, BoxError> {
            Ok(Some(serde_json::to_value(self)?))
        }
    }

    /// 压缩条目的 `data` 形状。
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct CompactionData {
        /// 压缩原因。
        pub reason: CompactionReason,
    }

    impl EntryData for CompactionData {
        fn from_entry_data(data: Option<&JsonValue>) -> Result<Self, BoxError> {
            Ok(serde_json::from_value(
                data.cloned().unwrap_or(JsonValue::Null),
            )?)
        }

        fn into_entry_data(self) -> Result<Option<JsonValue>, BoxError> {
            Ok(Some(serde_json::to_value(self)?))
        }
    }

    /// 用户输入：`model` 是 `[UserMessage]`。由提交写入。
    pub const UserEntry: Entry<NoData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.user"),
        marker: PhantomData,
    };
    /// Provider 结果：`model` 是 `[AssistantMessage]`。由 generation 写入。
    pub const AssistantEntry: Entry<NoData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.assistant"),
        marker: PhantomData,
    };
    /// 位置性提示与工具变化：`model` 是内容为空的 `[SystemMessage]`。
    pub const SystemEntry: Entry<NoData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.system"),
        marker: PhantomData,
    };
    /// 工具结果：`model` 是 `[ToolResultMessage]`，`data` 保存结构化诊断。
    pub const ToolResultEntry: Entry<ToolResultData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.tool-result"),
        marker: PhantomData,
    };
    /// 新上下文的起点：总是 `head: "self"`。
    pub const ResetEntry: Entry<NoData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.reset"),
        marker: PhantomData,
    };
    /// 压缩摘要。
    pub const CompactionEntry: Entry<CompactionData> = Entry {
        kind: std::borrow::Cow::Borrowed("kiana.compaction"),
        marker: PhantomData,
    };

    /// 只为让 `JsonValue` 与 `TypedEntryDraft` 在本模块文档中可见。
    const _: fn() -> Option<JsonValue> = || None;
}

pub use entries::{
    AssistantEntry, CompactionData, CompactionEntry, Entry, NoData, ResetEntry, SystemEntry,
    ToolResultData, ToolResultEntry, UserEntry, define_entry,
};

// ---------------------------------------------------------------------------
// documents.ts
// ---------------------------------------------------------------------------

/// `documents.ts`：定义、地址解析、创建记录与物化。
pub mod documents {
    use std::sync::Arc;

    use super::{
        CheckpointInfo, DocumentAddress, DocumentCreate, DocumentScope, DocumentSemantics,
        JsonObject, JsonValue, Op, RecordSemantics, ScopeKind, StoredDocument, id_from_number,
        js_safe_integer,
    };

    /// 定义里的不可变字段（TS: `CommonDocDefinition`）。
    ///
    /// 字段全部公开，构造本身不校验版本；交给 [`define_doc`] / [`define_doc_family`] 时
    /// 才会检查版本落在 `1..=2^53-1`，越界 panic。
    pub struct DocDefinition {
        /// 稳定的持久 kind。
        pub kind: String,
        /// 正值版本。
        pub version: i64,
        /// 是否是 keyed 文档家族。
        pub family: bool,
        /// 所有权与寿命。
        pub semantics: DocumentSemantics,
        /// 初始值；家族成员收到 seed。
        pub initial: Arc<dyn Fn(Option<&JsonValue>) -> JsonObject + Send + Sync>,
        /// 旧版本迁移：`(value, fromVersion) -> value`。
        pub migrate: Option<Arc<dyn Fn(JsonObject, i64) -> JsonObject + Send + Sync>>,
        /// 某个普通变化是否应存成完整 base：返回 `true` 时把这次 delta 改写成 base。
        ///
        /// 入参是当前值、本次 op 批次与 [`CheckpointInfo`]。闭包是用户代码，调用方用
        /// `catch_unwind` 把 panic 转成提交错误。
        pub checkpoint_when: Arc<dyn Fn(&JsonObject, &[Op], CheckpointInfo) -> bool + Send + Sync>,
    }

    impl DocDefinition {
        /// 单例定义。
        pub fn new(kind: impl Into<String>, version: i64, semantics: DocumentSemantics) -> Self {
            DocDefinition {
                kind: kind.into(),
                version,
                family: false,
                semantics,
                initial: Arc::new(|_| JsonObject::new()),
                migrate: None,
                checkpoint_when: Arc::new(|_, _, _| false),
            }
        }

        /// 家族定义。
        pub fn family(kind: impl Into<String>, version: i64, semantics: DocumentSemantics) -> Self {
            let mut definition = DocDefinition::new(kind, version, semantics);
            definition.family = true;
            definition
        }

        /// 设置初始值。
        pub fn with_initial(
            mut self,
            initial: impl Fn(Option<&JsonValue>) -> JsonObject + Send + Sync + 'static,
        ) -> Self {
            self.initial = Arc::new(initial);
            self
        }

        /// 设置迁移。
        pub fn with_migrate(
            mut self,
            migrate: impl Fn(JsonObject, i64) -> JsonObject + Send + Sync + 'static,
        ) -> Self {
            self.migrate = Some(Arc::new(migrate));
            self
        }

        /// 设置 checkpoint 判据。
        pub fn with_checkpoint_when(
            mut self,
            checkpoint_when: impl Fn(&JsonObject, &[Op], CheckpointInfo) -> bool + Send + Sync + 'static,
        ) -> Self {
            self.checkpoint_when = Arc::new(checkpoint_when);
            self
        }
    }

    impl std::fmt::Debug for DocDefinition {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("DocDefinition")
                .field("kind", &self.kind)
                .field("version", &self.version)
                .field("family", &self.family)
                .field("semantics", &self.semantics)
                .finish_non_exhaustive()
        }
    }

    /// 擦除的文档定义（TS: `AnyDocDefinition`）。
    pub type AnyDocDefinition = DocDefinition;

    /// 擦除的单例或家族 token（TS: `AnyDocToken`）。
    #[derive(Clone, Debug)]
    pub struct AnyDocToken {
        /// 定义。
        pub definition: Arc<DocDefinition>,
    }

    /// 单例文档 token（TS: `DocToken`）。
    #[derive(Clone, Debug)]
    pub struct DocToken {
        /// 定义。
        pub definition: Arc<DocDefinition>,
    }

    /// 文档家族 token（TS: `DocFamilyToken`）。
    #[derive(Clone, Debug)]
    pub struct DocFamilyToken {
        /// 定义。
        pub definition: Arc<DocDefinition>,
    }

    /// `Number.MAX_SAFE_INTEGER`（JS 安全整数的上界）。
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

    fn validate_definition(definition: &DocDefinition) {
        // TS 的 `!Number.isSafeInteger(version) || version < 1`：`i64` 自身不保证落在 JS 的
        // 安全整数范围内，所以上界也要检查。
        if definition.version < 1 || definition.version > MAX_SAFE_INTEGER {
            panic!(
                "Document {} version must be a positive integer",
                definition.kind
            );
        }
    }

    /// `defineDoc`：定义一个单例文档。
    pub fn define_doc(definition: DocDefinition) -> DocToken {
        validate_definition(&definition);
        DocToken {
            definition: Arc::new(definition),
        }
    }

    /// `defineDocFamily`：定义 keyed 文档家族。
    pub fn define_doc_family(definition: DocDefinition) -> DocFamilyToken {
        validate_definition(&definition);
        DocFamilyToken {
            definition: Arc::new(definition),
        }
    }

    /// 逻辑地址加字符串身份（TS: `ResolvedAddress`）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ResolvedAddress {
        /// 地址。
        pub address: DocumentAddress,
        /// 稳定字符串身份。
        pub id: String,
        /// 参数列表里下一个参数的下标。
        pub next_argument: usize,
    }

    fn owner_id(definition: &DocDefinition, value: Option<&JsonValue>) -> i64 {
        value.and_then(js_safe_integer).unwrap_or_else(|| {
            panic!(
                "Document {} requires a {} ID",
                definition.kind,
                match definition.semantics.scope {
                    ScopeKind::Conversation => "conversation",
                    ScopeKind::Task => "task",
                    ScopeKind::Session => "session",
                }
            )
        })
    }

    /// 解析重载参数列表，返回 owner 与家族 key 之后的参数下标（TS: `resolveAddress`）。
    ///
    /// `args` 按 TS 重载的位置取值：conversation 作用域先吃一个会话 ID，task 作用域先吃
    /// 一个任务 ID，`family` 定义再吃一个 key。owner 必须是 JS 安全整数；key 只做
    /// `as string` 断言，非字符串原样保留（见 [`DocumentAddress::key`]），缺参数才是
    /// “没有 key”。`next_argument` 供调用方继续解析后续参数。
    ///
    /// # Panics
    /// 需要 owner ID 但对应位置的参数缺失或不是 JS 安全整数时 panic，消息包含文档 kind
    /// 与所需的作用域名。
    pub fn resolve_address(definition: &DocDefinition, args: &[JsonValue]) -> ResolvedAddress {
        let mut index = 0usize;
        let scope = match definition.semantics.scope {
            ScopeKind::Session => DocumentScope::Session,
            ScopeKind::Conversation => {
                let value = owner_id(definition, args.get(index));
                index += 1;
                DocumentScope::Conversation {
                    conversation_id: id_from_number(value),
                }
            }
            ScopeKind::Task => {
                let value = owner_id(definition, args.get(index));
                index += 1;
                DocumentScope::Task {
                    task_id: id_from_number(value),
                }
            }
        };
        // 对应 `const key = definition.family === true ? (args[index++] as string) : undefined`：
        // 只有 `undefined`（参数缺失）算“没有 key”，其余值原样进入地址（不做类型校验）。
        let key = if definition.family {
            let value = args.get(index).cloned();
            index += 1;
            value
        } else {
            None
        };
        let address = DocumentAddress {
            kind: definition.kind.clone(),
            scope,
            key,
        };
        ResolvedAddress {
            id: address_id(&address),
            address,
            next_argument: index,
        }
    }

    /// 一个逻辑地址的稳定字符串身份（TS: `addressId`）：
    /// `JSON.stringify([kind, scope.kind, owner, key ?? null])`。
    ///
    /// 用 `kiana_json::js_object::stringify` 而非 `serde_json::to_string`，数字格式才与
    /// TS 的 `JSON.stringify` 一致（session 作用域的 owner 是 `null`，ID 是裸整数）。session
    /// 层用它做文档地址的索引键（`latest_document_by_address`），因此同一个逻辑地址必须
    /// 得到逐字相同的字符串。
    pub fn address_id(address: &DocumentAddress) -> String {
        let owner = match &address.scope {
            DocumentScope::Session => JsonValue::Null,
            DocumentScope::Conversation { conversation_id } => {
                JsonValue::from(conversation_id.get())
            }
            DocumentScope::Task { task_id } => JsonValue::from(task_id.get()),
        };
        let value = JsonValue::Array(vec![
            JsonValue::String(address.kind.clone()),
            JsonValue::String(address.scope.kind().as_str().to_owned()),
            owner,
            match &address.key {
                Some(key) => key.clone(),
                None => JsonValue::Null,
            },
        ]);
        kiana_json::js_object::stringify(&value)
    }

    /// 为一个地址构造存储创建记录（TS: `documentCreate`）。
    ///
    /// 只有 conversation 文档带 `history`/`fork`（TS 的判别联合保证）。
    pub fn document_create(
        definition: &DocDefinition,
        address: &DocumentAddress,
        id: super::DocumentId,
    ) -> DocumentCreate {
        let conversation = matches!(address.scope, DocumentScope::Conversation { .. });
        DocumentCreate {
            id,
            kind: address.kind.clone(),
            key: address.key.clone(),
            scope: address.scope,
            history: conversation
                .then_some(definition.semantics.history)
                .flatten(),
            fork: conversation.then_some(definition.semantics.fork).flatten(),
        }
    }

    /// 文档语义不匹配（TS 的 `TypeError`/`Error`）。
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[error("{message}")]
    pub struct DocumentSemanticsError {
        /// 完整消息。
        pub message: String,
    }

    /// 拒绝与持久作用域/历史/fork 语义不一致的带类型访问（TS: `checkRecordScope`）。
    pub fn check_record_scope<R: RecordSemantics>(
        definition: &DocDefinition,
        record: &R,
    ) -> Result<(), DocumentSemanticsError> {
        let scope = record.record_scope();
        let mismatch = scope.kind() != definition.semantics.scope
            || (scope.kind() == ScopeKind::Conversation
                && (record.record_history() != definition.semantics.history
                    || record.record_fork() != definition.semantics.fork));
        if mismatch {
            return Err(DocumentSemanticsError {
                message: format!(
                    "Document {} ({}) does not match the supplied definition semantics",
                    record.record_id(),
                    record.record_kind()
                ),
            });
        }
        Ok(())
    }

    /// 拒绝带类型访问无法使用的存储版本（TS: `checkRecordVersion`）。
    ///
    /// 只比较存储版本与定义版本，不检查 scope / history / fork（那是
    /// [`check_record_scope`] 的职责）。版本相等通过；存储版本更新时拒绝；更旧时要求
    /// 定义提供 `migrate`，否则拒绝。
    pub fn check_record_version<R: RecordSemantics>(
        definition: &DocDefinition,
        record: &R,
        version: i64,
    ) -> Result<(), DocumentSemanticsError> {
        if version > definition.version {
            return Err(DocumentSemanticsError {
                message: format!(
                    "Document {} ({}) has newer version {} than {}",
                    record.record_id(),
                    record.record_kind(),
                    version,
                    definition.version
                ),
            });
        }
        if version < definition.version && definition.migrate.is_none() {
            return Err(DocumentSemanticsError {
                message: format!(
                    "Document {} ({}) requires migration from version {}",
                    record.record_id(),
                    record.record_kind(),
                    version
                ),
            });
        }
        Ok(())
    }

    /// 校验并物化一份分离的存储值（TS: `materializeDocument`）。
    pub fn materialize_document(
        definition: &DocDefinition,
        stored: &StoredDocument,
    ) -> Result<JsonObject, DocumentSemanticsError> {
        materialize_document_value(
            definition,
            &stored.record,
            stored.version,
            stored.value.clone(),
        )
    }

    /// 在第一次持久化身之前校验并物化一份分离值（TS: `materializeDocumentValue`）。
    pub fn materialize_document_value<R: RecordSemantics>(
        definition: &DocDefinition,
        record: &R,
        version: i64,
        value: JsonObject,
    ) -> Result<JsonObject, DocumentSemanticsError> {
        check_record_scope(definition, record)?;
        check_record_version(definition, record, version)?;
        if version == definition.version {
            return Ok(value);
        }
        // 上面的 `check_record_version` 已保证走到这里时 `migrate` 存在：版本相等会在
        // 上一个分支直接返回，版本更新或“更旧且没有 migrate”都已经报错。
        let migrate = definition
            .migrate
            .as_ref()
            .expect("check_record_version guarantees migrate");
        Ok(migrate(value, version))
    }
}

pub use documents::{
    AnyDocDefinition, AnyDocToken, DocDefinition, DocFamilyToken, DocToken, DocumentSemanticsError,
    ResolvedAddress, address_id, check_record_scope, check_record_version, define_doc,
    define_doc_family, document_create, materialize_document, materialize_document_value,
    resolve_address,
};

// ---------------------------------------------------------------------------
// TS 类型别名的同名端口（spec §2.1 的 type-only 导出清单）
// ---------------------------------------------------------------------------

/// 把 TS 的泛型定义/token 别名投影到 Rust 的擦除类型。
///
/// Rust 的类型别名不允许未使用的类型参数（E0091），而 durable 的定义与 token 在 Rust 侧是
/// 类型擦除的（文档值一律是 [`JsonObject`]）。这个投影 trait 让别名“使用”参数，同时对每个
/// `T` 都归一化为同一个擦除类型，因此 `SessionDocToken<T>` 与 [`DocToken`] 完全等同。
#[doc(hidden)]
pub trait DocAliasFamily<I: ?Sized = JsonValue> {
    /// `CommonDocDefinition<T>` / `DocFamilyDefinition<T, I>` 的擦除形态。
    type Definition;
    /// `DocToken<T, D>` 及其具体别名的擦除形态。
    type Token;
    /// `DocFamilyToken<T, I, D>` 及其具体别名的擦除形态。
    type FamilyToken;
}

impl<T: ?Sized, I: ?Sized> DocAliasFamily<I> for T {
    type Definition = documents::DocDefinition;
    type Token = documents::DocToken;
    type FamilyToken = documents::DocFamilyToken;
}

/// 单例与家族定义共用的字段（TS: `CommonDocDefinition<T>`）。
pub type CommonDocDefinition<T = JsonObject> = <T as DocAliasFamily>::Definition;

/// keyed 文档家族定义（TS: `DocFamilyDefinition<T, I>`）。
pub type DocFamilyDefinition<T = JsonObject, I = JsonValue> = <T as DocAliasFamily<I>>::Definition;

/// Session 单例文档 token（TS: `SessionDocToken<T>`）。
pub type SessionDocToken<T = JsonObject> = <T as DocAliasFamily>::Token;
/// conversation 单例文档 token（TS: `ConversationDocToken<T>`）。
pub type ConversationDocToken<T = JsonObject> = <T as DocAliasFamily>::Token;
/// rewindable conversation 单例文档 token（TS: `RewindableConversationDocToken<T>`）。
pub type RewindableConversationDocToken<T = JsonObject> = <T as DocAliasFamily>::Token;
/// task 单例文档 token（TS: `TaskDocToken<T>`）。
pub type TaskDocToken<T = JsonObject> = <T as DocAliasFamily>::Token;

/// Session 家族文档 token（TS: `SessionDocFamilyToken<T, I>`）。
pub type SessionDocFamilyToken<T = JsonObject, I = JsonValue> =
    <T as DocAliasFamily<I>>::FamilyToken;
/// conversation 家族文档 token（TS: `ConversationDocFamilyToken<T, I>`）。
pub type ConversationDocFamilyToken<T = JsonObject, I = JsonValue> =
    <T as DocAliasFamily<I>>::FamilyToken;
/// rewindable conversation 家族文档 token（TS: `RewindableConversationDocFamilyToken<T, I>`）。
pub type RewindableConversationDocFamilyToken<T = JsonObject, I = JsonValue> =
    <T as DocAliasFamily<I>>::FamilyToken;
/// task 家族文档 token（TS: `TaskDocFamilyToken<T, I>`）。
pub type TaskDocFamilyToken<T = JsonObject, I = JsonValue> = <T as DocAliasFamily<I>>::FamilyToken;

/// 只保留当前状态的 conversation 文档语义（TS: `LatestConversationSemantics`）。
///
/// Rust 侧把 TS 的判别联合擦除为 [`DocumentSemantics`]；用
/// [`DocumentSemantics::latest`] 构造并与 [`DocumentSemantics::history`] 校验。
pub type LatestConversationSemantics = DocumentSemantics;

/// 保留 as-of 历史的 conversation 文档语义（TS: `RewindableConversationSemantics`）。
///
/// 同 [`LatestConversationSemantics`]：用 [`DocumentSemantics::rewindable`] 构造。
pub type RewindableConversationSemantics = DocumentSemantics;

/// 一个 checkpoint 阶段的处理器（TS: `PhaseHandler`）。
///
/// TS 的函数类型在 Rust 侧是 [`TaskPhase`] trait 对象（`Arc<dyn TaskPhase<...>>`）。
pub type PhaseHandler<I = JsonValue, S = JsonValue, R = JsonValue, H = ()> =
    dyn TaskPhase<I, S, R, H>;

impl ScopeKind {
    /// TS 的字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Session => "session",
            ScopeKind::Conversation => "conversation",
            ScopeKind::Task => "task",
        }
    }
}

/// 文档操作的解析目标：定义 + owner + 可选的家族 key/seed。
#[derive(Clone, Debug)]
pub struct DocTarget {
    /// 定义。
    pub definition: Arc<DocDefinition>,
    /// owner。
    pub owner: DocOwner,
    /// 家族键。
    ///
    /// 带类型的入口（`DocTarget` 的构造函数）只接受字符串；未类型化的
    /// [`documents::resolve_address`] 允许任意 JSON 值，那种地址不会变成 `DocTarget`。
    pub key: Option<String>,
    /// 家族初始 seed。
    pub seed: Option<JsonValue>,
}

/// 文档的拥有者。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocOwner {
    /// 整个 Session。
    Session,
    /// 一个会话。
    Conversation(ConversationId),
    /// 一个任务。
    Task(TaskId),
}

impl DocTarget {
    fn from_token(definition: Arc<DocDefinition>, owner: DocOwner) -> Self {
        DocTarget {
            definition,
            owner,
            key: None,
            seed: None,
        }
    }

    /// Session 单例。
    pub fn session(token: &DocToken) -> Self {
        Self::from_token(Arc::clone(&token.definition), DocOwner::Session)
    }

    /// 会话单例。
    pub fn conversation(token: &DocToken, conversation_id: ConversationId) -> Self {
        Self::from_token(
            Arc::clone(&token.definition),
            DocOwner::Conversation(conversation_id),
        )
    }

    /// 任务单例。
    pub fn task(token: &DocToken, task_id: TaskId) -> Self {
        Self::from_token(Arc::clone(&token.definition), DocOwner::Task(task_id))
    }

    /// Session 家族成员。
    pub fn session_family(token: &DocFamilyToken, key: impl Into<String>, seed: JsonValue) -> Self {
        let mut target = Self::from_token(Arc::clone(&token.definition), DocOwner::Session);
        target.key = Some(key.into());
        target.seed = Some(seed);
        target
    }

    /// 会话家族成员。
    pub fn conversation_family(
        token: &DocFamilyToken,
        conversation_id: ConversationId,
        key: impl Into<String>,
        seed: JsonValue,
    ) -> Self {
        let mut target = Self::from_token(
            Arc::clone(&token.definition),
            DocOwner::Conversation(conversation_id),
        );
        target.key = Some(key.into());
        target.seed = Some(seed);
        target
    }

    /// 任务家族成员。
    pub fn task_family(
        token: &DocFamilyToken,
        task_id: TaskId,
        key: impl Into<String>,
        seed: JsonValue,
    ) -> Self {
        let mut target = Self::from_token(Arc::clone(&token.definition), DocOwner::Task(task_id));
        target.key = Some(key.into());
        target.seed = Some(seed);
        target
    }

    /// 精确作用域。
    pub fn scope(&self) -> DocumentScope {
        match self.owner {
            DocOwner::Session => DocumentScope::Session,
            DocOwner::Conversation(conversation_id) => {
                DocumentScope::Conversation { conversation_id }
            }
            DocOwner::Task(task_id) => DocumentScope::Task { task_id },
        }
    }

    /// 逻辑地址。
    pub fn address(&self) -> DocumentAddress {
        DocumentAddress {
            kind: self.definition.kind.clone(),
            scope: self.scope(),
            key: self.key.clone().map(JsonValue::String),
        }
    }

    /// 稳定地址身份。
    pub fn address_id(&self) -> String {
        documents::address_id(&self.address())
    }
}

// ---------------------------------------------------------------------------
// Documents: drafts and watches
// ---------------------------------------------------------------------------

/// 草稿的结算状态（TS: chord overlay 的 `status`）。
#[doc(hidden)]
pub const DRAFT_OPEN: u8 = 0;
/// 已准备（`prepare()` 之后）：可读，写入被拒绝（TS: `Prepared overlays are read-only`）。
#[doc(hidden)]
pub const DRAFT_PREPARED: u8 = 1;
/// 已采纳（`adopt()` 之后）：任何访问都视为已结算（TS: `Cannot use a settled overlay`）。
#[doc(hidden)]
pub const DRAFT_CONSUMED: u8 = 2;
/// 已放弃（`abort()` 之后）：任何访问都视为已结算。
#[doc(hidden)]
pub const DRAFT_ABORTED: u8 = 3;

/// 文档的可变事务视图（TS: `Draft<T>` / chord overlay 的根代理）。
///
/// TS 的 `Draft` 是指向 chord overlay 的可变代理：赋值立即记录在 overlay 上，`prepare()` 读出
/// 最终值与操作日志。Rust 没有属性代理，这里用一个**共享活值单元**表达同样的可见性：
///
/// - 事务创建的草稿持有事务 [`crate::session::transaction::Change`] 的同一份
///   `Arc<Mutex<JsonObject>>`，因此每次 `tx.doc()` 获取到的草稿看到同一份活值；通过任何一份草稿
///   做的修改在 `prepare()` 时都可见（即使草稿逃逸出回调）。
/// - 草稿同时持有共享的结算状态：`prepare()` 之后写入被拒绝，采纳/放弃之后读取返回空对象、
///   写入 panic（TS 在读取时抛 `Cannot use a settled overlay`、写入时抛 `TypeError`；冻结的
///   `Deref` 没有错误通道，只能以空对象表示吊销）。
/// - `DocDraft::new()` 创建的独立草稿没有共享单元（测试与工具用）。
///
/// SAFETY：共享单元的读写只由持有它的草稿完成，而一份事务的所有草稿都在同一个任务里使用
/// （与 TS 的单线程 overlay 一致）；`Arc<Mutex<_>>` 分配地址稳定，读取在取到地址后立即释放锁，
/// 不会跨 `await` 持锁。
#[derive(Debug, Clone, Default)]
pub struct DocDraft {
    value: JsonObject,
    shared: Option<Arc<std::sync::Mutex<JsonObject>>>,
    status: Option<Arc<AtomicU8>>,
}

fn draft_lock(mutex: &std::sync::Mutex<JsonObject>) -> std::sync::MutexGuard<'_, JsonObject> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn empty_object() -> &'static JsonObject {
    static EMPTY: std::sync::LazyLock<JsonObject> = std::sync::LazyLock::new(JsonObject::new);
    &EMPTY
}

impl DocDraft {
    /// 从已载入的值创建。
    pub fn new(value: JsonObject) -> Self {
        DocDraft {
            value,
            shared: None,
            status: None,
        }
    }

    /// 事务内部：与 `Change` 共享活值（TS 返回同一个代理）。
    #[doc(hidden)]
    pub fn shared(value: JsonObject, shared: Arc<std::sync::Mutex<JsonObject>>) -> Self {
        DocDraft {
            value,
            shared: Some(shared),
            status: Some(Arc::new(AtomicU8::new(DRAFT_OPEN))),
        }
    }

    /// 事务内部：共享活值与结算状态。
    #[doc(hidden)]
    pub fn transaction(shared: Arc<std::sync::Mutex<JsonObject>>, status: Arc<AtomicU8>) -> Self {
        DocDraft {
            value: JsonObject::new(),
            shared: Some(shared),
            status: Some(status),
        }
    }

    fn status_code(&self) -> u8 {
        self.status
            .as_ref()
            .map(|status| status.load(Ordering::SeqCst))
            .unwrap_or(DRAFT_OPEN)
    }

    fn settled(&self) -> bool {
        matches!(self.status_code(), DRAFT_CONSUMED | DRAFT_ABORTED)
    }

    /// TS `assertWritable()`：已准备时抛 `Prepared overlays are read-only`，已结算时抛
    /// `Cannot use a settled overlay`。
    fn assert_writable(&self) {
        match self.status_code() {
            DRAFT_OPEN => {}
            DRAFT_PREPARED => panic!("Prepared overlays are read-only"),
            _ => panic!("Cannot use a settled overlay"),
        }
    }

    /// 取出底层对象（已结算的草稿返回它最后的值，供内部读取）。
    pub fn into_value(mut self) -> JsonObject {
        match &self.shared {
            Some(shared) => draft_lock(shared).clone(),
            None => std::mem::take(&mut self.value),
        }
    }

    /// 只读视图。已结算的草稿返回空对象：TS 的 `get` 陷阱对任何属性抛出
    /// `Cannot use a settled overlay`，但冻结契约 `Deref<Target = JsonObject>` 无法返回错误，
    /// 因此这里用"读不到任何字段"表达吊销（写入仍以 TS 的文案 panic）。
    pub fn value(&self) -> &JsonObject {
        match &self.shared {
            Some(shared) => {
                if self.settled() {
                    return empty_object();
                }
                let guard = draft_lock(shared);
                let pointer = &*guard as *const JsonObject;
                drop(guard);
                // SAFETY: 见类型说明；地址来自 `Arc` 内的 `Mutex`，在草稿存活期间稳定。
                // 返回的引用与 `&self` 同生命周期；前提是同一共享单元上没有并发的
                // `value_mut`（类型说明里的单任务使用约束）。
                unsafe { &*pointer }
            }
            None => &self.value,
        }
    }

    /// 可变视图。已准备/已结算的草稿 panic（TS 抛 `TypeError`）。
    pub fn value_mut(&mut self) -> &mut JsonObject {
        match &self.shared {
            Some(shared) => {
                self.assert_writable();
                let mut guard = draft_lock(shared);
                let pointer = &mut *guard as *mut JsonObject;
                drop(guard);
                // SAFETY: 见类型说明；事务的草稿只在同一个任务里读写。
                unsafe { &mut *pointer }
            }
            None => &mut self.value,
        }
    }
}

impl PartialEq for DocDraft {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}

impl std::ops::Deref for DocDraft {
    type Target = JsonObject;
    fn deref(&self) -> &Self::Target {
        self.value()
    }
}

impl std::ops::DerefMut for DocDraft {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value_mut()
    }
}

/// watch 的终态（TS: `WatchEnd`）。
#[derive(Debug)]
pub enum WatchEnd {
    /// 调用了 `stop()`。
    Stopped,
    /// 获取 watch 的上下文被取消。
    Cancelled,
    /// 源被退役（最后一帧是 `[["r", null]]`）。
    Retired,
    /// 源关闭。
    Ended,
    /// 监听器抛错。
    ListenerError(String),
    /// 源失败。
    Failed(BoxError),
}

/// watch 监听器：值、ops 与上下文。
#[async_trait]
pub trait WatchListener<T: Send + Sync>: Send + Sync {
    /// 送达一帧。
    async fn call(&self, value: T, ops: Vec<Op>, context: Arc<dyn Context>);
}

/// 一份已提交文档化身的 watch（TS: `WatchHandle<T>` 的契约等价物）。
///
/// `watch.ts` 在 chord 单元落地前是骨架；这里保留 TS 的公开方法形状。
#[async_trait]
pub trait WatchHandle<T: Send + Sync>: Send + Sync {
    /// 获取时（start 之前）或最近送达的值。
    fn value(&self) -> &T;
    /// 安装唯一的异步监听器；绝不同步调用。
    fn start(&self, listener: Arc<dyn WatchListener<T>>);
    /// 幂等停止后续回调，并返回终态。
    async fn stop(&self) -> WatchEnd;
    /// watch 终止时落定。
    async fn closed(&self) -> WatchEnd;
    /// 异步投递队列是否已排空（没有待投递帧或运行中的投递任务）。
    ///
    /// 宿主/测试用它在提交落定之后等待回调真正赶上，替代固定 sleep。默认返回 `true`，
    /// 只有拥有内部投递队列的实现需要覆写（本 crate 的全部实现都已覆写）。
    fn is_idle(&self) -> bool {
        true
    }
}

/// 一份已提交文档化身的 watch：值可以是 `null`（退役）。
pub type DocumentWatch<T = JsonObject> = Arc<dyn WatchHandle<Option<T>>>;

// ---------------------------------------------------------------------------
// Report types (types.ts)
// ---------------------------------------------------------------------------

/// 报告涉及的任务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportTask {
    /// 任务 ID。
    pub id: TaskId,
    /// 任务注册名。
    pub kind: String,
    /// 任务所在会话。
    pub conversation_id: ConversationId,
}

/// 宿主回调名（TS: `HostCallback`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostCallback {
    /// `now`。
    Now,
    /// `settings`。
    Settings,
    /// `registry`。
    Registry,
    /// `env`。
    Env,
    /// `models`。
    Models,
    /// `setTimer`。
    SetTimer,
    /// `onWake`。
    OnWake,
}

/// 报告给 `HarnessOptions.onReport` 的内容（TS: `ReportInfo`）。
#[derive(Debug, Clone, PartialEq)]
pub enum ReportInfo {
    /// 错误让 Session 失败，随后关闭。
    SessionFailed,
    /// 任务以 `faulted` 结束。
    TaskFaulted {
        /// 任务。
        task: ReportTask,
        /// 相关扩展。
        extension: Option<String>,
        /// 原因。
        reason: TaskFaultedReason,
    },
    /// 重启上限把任务停泊。
    TaskParked {
        /// 任务。
        task: ReportTask,
        /// 相关扩展。
        extension: Option<String>,
    },
    /// 任务存储的状态无法迁移。
    MigrationFailed {
        /// 任务。
        task: ReportTask,
        /// 相关扩展。
        extension: Option<String>,
    },
    /// 运行中的任务继续用旧定义运行。
    TaskOutdated {
        /// 任务。
        task: ReportTask,
        /// 相关扩展。
        extension: Option<String>,
        /// 原因。
        reason: TaskOutdatedReason,
    },
    /// 任务自己的 `runtime.report()` 或运行它的失败。
    TaskError {
        /// 任务。
        task: ReportTask,
        /// 相关扩展。
        extension: Option<String>,
    },
    /// hook、section、wrapper 或工具结构化输出错误。
    ExtensionError {
        /// 相关扩展。
        extension: Option<String>,
        /// 相关任务。
        task: Option<ReportTask>,
        /// 相关会话。
        conversation_id: Option<ConversationId>,
    },
    /// 宿主回调抛错。
    HostError {
        /// 回调名。
        callback: HostCallback,
        /// 相关任务。
        task: Option<ReportTask>,
    },
    /// 提交、关闭、watch、视图或文档监听器抛错。
    ListenerError,
}

/// `task_faulted` 的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFaultedReason {
    /// 阶段抛错。
    Threw,
    /// restart 上限切断。
    RestartLimit,
}

/// `task_outdated` 的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutdatedReason {
    /// 定义被卸载。
    MissingTask,
    /// 定义被替换且无法接管其状态。
    IncompatibleTask,
}

/// 任务 `runtime.report()` 归因于谁（TS: `ReportFrom`）。
#[derive(Debug, Clone, PartialEq)]
pub enum ReportFrom {
    /// 扩展代码。
    Extension {
        /// 扩展名。
        extension: Option<String>,
    },
    /// 宿主回调。
    Host {
        /// 回调名。
        callback: HostCallback,
    },
}

/// 宿主计时器（TS: `SetTimer`）。
pub type SetTimer = crate::timers::SetTimer;

// TS 的 `types.ts` 定义 `HookRunner` 与 `TaskRuntime`；Rust 侧它们位于
// `harness/types.rs`（引用大量 harness 类型），这里在 crate 根再导出，保持 spec §2.1 的
// type-only 导出面。
pub use crate::harness::types::{HookRunner, TaskRuntime};

// ---------------------------------------------------------------------------
// Transactions, readers, observers, sessions, storage
// ---------------------------------------------------------------------------

/// 一个 Session 提交回调的事务面（TS: `Tx`）。
///
/// TS 的 `Tx` 方法没有 `Context` 参数；这里保持一致。`Session::commit()` 接收上下文。
#[async_trait]
pub trait Tx: Send + Sync {
    /// 读一个会话。
    async fn conversation(
        &self,
        id: ConversationId,
    ) -> Result<Option<ConversationRecord>, BoxError>;
    /// 读一个条目。
    async fn entry(&self, id: EntryId) -> Result<Option<EntryRecord>, BoxError>;
    /// 读一个任务。
    async fn task(&self, id: TaskId) -> Result<Option<TaskRecord>, BoxError>;
    /// 扫描会话。
    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<ConversationRecord>, BoxError>;
    /// 扫描条目。
    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<EntryRecord>, BoxError>;
    /// 会话里带 `head` 的最新可见条目。
    async fn latest_head_marker(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<EntryRecord>, BoxError>;
    /// 扫描任务。
    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page<TaskRecord>, BoxError>;
    /// 按会话作用域的 requestId 查已提交的提交。
    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
    ) -> Result<Option<SubmissionRecord>, BoxError>;

    /// 创建会话。
    async fn create_conversation(
        &self,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError>;
    /// 在某个具体可见条目处创建历史 fork。
    async fn fork_conversation(
        &self,
        parent_conversation_id: ConversationId,
        at: EntryId,
        options: &CreateConversationOptions,
    ) -> Result<ConversationRecord, BoxError>;
    /// 追加条目。
    async fn append_entry(
        &self,
        conversation_id: ConversationId,
        value: EntryDraft,
    ) -> Result<EntryRecord, BoxError>;
    /// 创建任务；`task` 是擦除后的定义。
    async fn create_task(
        &self,
        task: &crate::harness::types::AnyTask,
        input: JsonValue,
        options: TaskOptions,
    ) -> Result<TaskId, BoxError>;
    /// 用内部创建选项创建任务（TS `Transaction.createTask` 的 `submissionIds` 解析）。
    ///
    /// 默认实现退回 [`Tx::create_task`]，即不解析提交载体；session 的事务实现覆盖它：任务
    /// `trace` 取自首个提交（`submission?.trace ?? "root"`），其余提交各追加一条 link。
    async fn create_task_with(
        &self,
        task: &crate::harness::types::AnyTask,
        input: JsonValue,
        options: crate::session::transaction::TaskCreationOptions,
    ) -> Result<TaskId, BoxError> {
        self.create_task(task, input, options.task_options()).await
    }
    /// 用新 ID 创建原始提交记录。
    async fn create_submission(
        &self,
        create: SubmissionCreate,
    ) -> Result<SubmissionRecord, BoxError>;
    /// 落定一个 queued/placed 提交。
    ///
    /// 没有返回值：它把变化排进本事务的队列并置位表写标志（此后同一事务里的表读会以
    /// [`ReadAfterWrite`] 失败）。目标提交的状态校验延迟到提交组装时：`done`/`unanswered`
    /// 是吸收态，变化被直接忽略；`SubmissionSettlement::Done` 要求提交已处于 `placed`，
    /// 否则整个提交失败；`SubmissionSettlement::Unanswered` 把任意非终态提交置为
    /// `unanswered`。`Transaction` 的对应实现会在事务已关闭等即时错误上 panic；它另有
    /// 返回 `Result` 的 `try_settle_submission` 变体。
    fn settle_submission(&self, id: SubmissionId, settlement: SubmissionSettlement);
    /// 把一个 queued 提交放到 `entry`。
    ///
    /// 与 [`Tx::settle_submission`] 一样只排队、无返回值，并同样置位表写标志（此后同一
    /// 事务里的表读会以 [`ReadAfterWrite`] 失败）。放置的状态检查在提交组装时进行：
    /// `queued` 且 kind 为 `Input` 的提交变为 `placed`，其他 `queued` 提交直接变为
    /// `done`；对 `placed` 提交放置会让整个提交报 `Submission {id} is not queued`，
    /// `done`/`unanswered` 是吸收态、放置被忽略。
    fn place_submission(&self, id: SubmissionId, entry: EntryId);

    /// 可变的文档草稿（不存在时创建）。
    async fn doc(&self, target: &DocTarget) -> Result<DocDraft, BoxError>;
    /// 退役一个文档化身。
    async fn retire_doc(&self, target: &DocTarget) -> Result<(), BoxError>;
}

/// `Tx` 的类型化条目读写（TS: `Tx.entry<D>(token, id)` / `appendEntry<D>(token, ...)` 重载）。
///
/// Rust 没有方法重载，而且 `Tx` 必须保持对象安全（内部以 `&dyn Tx` 传递），因此带类型参数的
/// 重载放在这个扩展 trait 上；它对每个 `Tx` 实现（包括 `dyn Tx`）自动可用。
#[async_trait]
pub trait TypedEntryTx: Tx {
    /// 读一个条目；不存在或 kind 与 token 不符时返回 `None`。
    async fn entry_typed<D: EntryData>(
        &self,
        token: &entries::Entry<D>,
        id: EntryId,
    ) -> Result<Option<TypedEntry<D>>, BoxError> {
        let Some(record) = self.entry(id).await? else {
            return Ok(None);
        };
        token.typed_entry(record)
    }

    /// 追加条目；`kind` 由 token 注入（`TypedEntryDraft` 没有 `kind`）。
    async fn append_entry_typed<D: EntryData>(
        &self,
        token: &entries::Entry<D>,
        conversation_id: ConversationId,
        value: TypedEntryDraft<D>,
    ) -> Result<TypedEntry<D>, BoxError> {
        let draft = token.typed_draft(value)?;
        let record = self.append_entry(conversation_id, draft).await?;
        let data = D::from_entry_data(record.data.as_ref())?;
        Ok(TypedEntry { record, data })
    }
}

impl<T: Tx + ?Sized> TypedEntryTx for T {}

/// `Tx.createConversation` / `forkConversation` 的 options。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConversationOptions {
    /// 显式选择的拥有权。
    pub ownership: ConversationOwnership,
}

/// 已提交文档的读取（TS: `DocumentReader = Pick<Session, "snapshot" | "snapshotAsOf">`）。
#[async_trait]
pub trait DocumentReader: Send + Sync {
    /// 当前值；文档不存在时返回 `None`。
    ///
    /// 会校验持久记录的 scope / history / fork 与版本（见 [`check_record_scope`] /
    /// [`check_record_version`]），不匹配时返回错误。
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError>;
    /// 某个可见条目处的值。
    ///
    /// 只支持 conversation 作用域的文档，其他作用域返回错误。`at` 是会话里可见条目的
    /// ID：实现把它换成该条目的提交序号后按 as-of 读取。条目不可见、或历史化身存在但内容
    /// 读不出时返回错误；该时间点没有这份文档时返回 `None`。
    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError>;
}

/// 非创建的文档 watch 获取（TS: `DocumentObserver`）。
#[async_trait]
pub trait DocumentObserver: Send + Sync {
    /// 观察一份已提交文档化身；不存在时返回 `None`。
    ///
    /// 加载期间上下文被取消时返回错误而不是 watch（此时的取消不会被静默吞掉）。
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError>;
}

/// Session 结束的原因（TS: `SessionEnd`）。
#[derive(Debug)]
pub enum SessionEnd {
    /// `close()`。
    Closed,
    /// 一次 Storage 调用失败，Session 自行关闭。
    Failed {
        /// 那个错误。
        error: BoxError,
    },
}

/// 一次提交的持久化时机（TS: `CommitPersistence`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CommitPersistence {
    /// 默认：`commit()` 返回前写入。
    Immediate,
    /// 只在特定文档上延迟写入。
    Deferred {
        /// 延迟上限；缺省用 `settings.progress.persistWithinMs`。
        #[serde(rename = "withinMs", skip_serializing_if = "Option::is_none")]
        within_ms: Option<f64>,
    },
}

/// 一次 Session 提交的选项（TS: `CommitOptions`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitOptions {
    /// 持久化时机。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<CommitPersistence>,
}

/// 提交变化的监听器。
pub type CommitListener = Arc<dyn Fn(&CommitPublication, &dyn Context) + Send + Sync>;
/// 关闭监听器。
pub type CloseListener = Arc<dyn Fn() + Send + Sync>;
/// 注销函数。
pub type Unsubscribe = Box<dyn FnOnce() + Send + Sync>;

/// 一个 Session 提交回调（TS: `(tx: Tx) => T | Promise<T>`）。
///
/// Rust 没有匿名异步闭包 trait，因此把回调显式实现为一个 trait；返回擦除后的值，由调用方
/// downcast 回具体类型。
#[async_trait]
pub trait SessionChange: Send + Sync {
    /// 在事务上运行。
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError>;
}

/// 持有一次变更线、其记录与文档的拥有者（TS: `Session`）。
#[async_trait]
pub trait Session: DocumentObserver + DocumentReader + Send + Sync {
    /// 在 Session 变更线上运行一个原子事务。
    async fn commit(
        &self,
        change: &dyn SessionChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError>;
    /// 封闭接纳、结清已接纳的提交，然后关闭 storage。
    async fn close(&self, context: &dyn Context);
    /// Session 关闭（含 storage）后落定。
    async fn closed(&self) -> SessionEnd;
    /// 同步观察已存提交。
    fn subscribe_commits(&self, listener: CommitListener) -> Unsubscribe;
    /// 同步观察关闭开始。
    fn subscribe_close(&self, listener: CloseListener) -> Unsubscribe;
}

/// 已存提交的 adopted 回调（TS: `Storage.commit` 的 `adopt`）。
pub type AdoptFn = Box<dyn Fn(Seq) + Send + Sync>;

/// Session 记录的原子持久边界（TS: `Storage`）。
///
/// 方法抛出的错误是最终错误：它让 Session 失败。两个读错误只让那次读取失败：无效请求
/// （[`StorageRequestError`]）与调用方上下文被取消。
#[async_trait]
pub trait Storage: Send + Sync {
    /// 原子持久化一批写入并返回序号。批次持久后、下一次提交开始前同步调用一次 `adopt`。
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError>;

    /// 从全局数字 ID 命名空间返回一个新的候选 ID。
    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError>;

    /// 按精确 ID 查会话。
    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError>;

    /// 按 ID 顺序扫描会话。
    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError>;

    /// 查全局条目及其提交序号。
    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError>;

    /// 只在请求会话的祖先范围内查条目。
    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError>;

    /// 返回可选含 cutoff 处及之前最新的带 `head` 的可见条目。
    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError>;

    /// 扫描闭区间可见范围。
    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError>;

    /// 查一个任务的最新完整记录。
    async fn task(&self, id: TaskId, context: &dyn Context)
    -> Result<Option<TaskRecord>, BoxError>;

    /// 扫描匹配所有过滤条件的任务记录。
    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError>;

    /// 查一个提交的最新完整记录。
    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError>;

    /// 扫描匹配所有过滤条件的提交。
    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError>;

    /// 按会话作用域的宿主去重键查提交。
    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError>;

    /// 解析某个精确逻辑地址在选中时间点的化身。
    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError>;

    /// 按 ID 物化某个具体化身。
    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError>;

    /// 扫描某个精确作用域在选中时间点存活的化身。
    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError>;

    /// 释放后端资源；之后所有操作都必须报错。
    async fn close(&self, context: &dyn Context);
}
