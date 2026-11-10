//! `harness/inbox.ts`：一个会话等待边界的提交队列。
//!
//! TS: `packages/durable/src/harness/inbox.ts`。`kiana.inbox` 是 latest/initial 文档，队列按 ID 顺序。
//!
//! # 这个文件负责什么
//!
//! 会话忙碌、或空闲但队列已有项时，不能立刻放置的提交（用户输入与条目写入）先以 JSON 项
//! 排进 `kiana.inbox`；等运行
//! 走到工具轮次之后（[`BoundaryAt::PostTools`]）或运行结束（[`BoundaryAt::Final`]）时，由
//! `harness/submissions.rs` 的 `admit_submission`（接纳提交时）与 `harness/generation.rs`
//! （回答提交后、工具轮次后、失败运行结清时）调用 [`prepare_boundary`] / [`apply_boundary`]
//! 把它们放置成真正的条目。撤回（`Conversation.abort()`）走 [`withdraw_queued_inputs`]，
//! 单个提交的撤回走 [`remove_inbox_item`]。
//!
//! # 一次边界的完整流程
//!
//! 1. [`prepare_boundary`] 在事务里读最新 head 标记与 inbox 草稿。它必须在本次提交的第一次
//!    表写入之前调用（读表晚于写表会违反 Session 的读后写约束；`admit_submission` 就是这样
//!    排的）。
//! 2. 接纳提交的 `admit_submission` 把新提交 push 进草稿：空闲且已有排队项时是
//!    `boundary.inbox`，忙碌时是另外 `tx.doc` 取到的同一份文档（见 `harness/submissions.rs`
//!    的 `push_inbox_item_into`）。
//! 3. [`apply_boundary`] 先放置所有 write 项（stale 的落定为 `unanswered`），再按位置升序放置
//!    选中的 steer/follow-up 输入；随后把这些项从草稿里删除。
//! 4. 调用方按返回的 `users` 启动后续运行，并按 `reset` 决定是否先结束当前运行
//!    （`harness/generation.rs` 在 reset 处结束运行并结算 `unanswered`/`reset`）。
//!
//! # 重要约束
//!
//! - **保留原始 JSON**：inbox 的 `items` 始终按 `JsonValue` 数组读写（`read_raw_items`），不做
//!   类型化往返；未类型化调用方多带的字段、未知 `mode`、`data: null` 都必须原样留下，否则
//!   删除选中项时会把它们悄悄丢掉。类型化的 [`InboxItem`] 只用于序列化新项与调用方检查。
//! - **顺序**：write 先于用户输入；用户项按 inbox 位置升序放置；删除按下标降序，避免前面的
//!   删除让后面的下标位移。
//! - **reset**：队列里任意位置出现 `head: "self"` 的 write，就把 `PostTools` 边界升级为
//!   `Final`，让 follow-up 也在新上下文里运行。
//! - **stale**：目标 head 早于当前活动范围的 write 不写条目，落定 `unanswered`/`stale`；
//!   它仍然会从队列移除。
//! - **错误**：`items` 不是数组、项的 `id` 无法解析、用户项缺 `content`、写入项缺 `entry`，
//!   或 JSON 形状不符，都会返回 `BoxError`；函数内部不捕获，由事务调用方处理。

use std::sync::LazyLock;

use kiana_ai::{Message, UserContent, UserMessage, UserRole};
use serde::de::Error as DeError;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::harness::types::{InputEntry, QueueMode, Settings};
use crate::types::{
    BoxError, ConversationId, DocDefinition, DocDraft, DocTarget, DocToken, DocumentFork,
    DocumentSemantics, EntryDraft, EntryHead, EntryRecord, JsonObject, JsonValue, NoData,
    SubmissionId, SubmissionSettlement, Tx, TypedEntryDraft, TypedEntryTx, UserEntry, define_doc,
};

/// 一条排队中的提交：一次运行的用户输入（带它不是 `kiana.user` 时被放置成的条目），或一次条目写入。
///
/// TS: `InboxItem`。TS 里是判别联合，键序为 `{id, mode, content?, entry?}`；内部标记的 serde 枚举
/// 会把 `mode` 排到前面，因此手写这两个实现保留键序。
///
/// 线上形状：
///
/// - `Input`：`mode` 是 `steer`/`followUp`，`content` 必有，`entry` 可选（缺省即 `kiana.user`）；
/// - `Write`：`mode` 固定为 `"write"`，`entry` 是一个 [`EntryDraft`] 对象。
///
/// 反序列化按这两个形状严格校验（未知 `mode`、缺 `content`/`entry` 都报错）。需要保留无法
/// 识别的排队项时，本模块内部改读原始 JSON，而不是走这个类型。
#[derive(Debug, Clone, PartialEq)]
pub enum InboxItem {
    /// 用户输入：`steer` 或 `followUp`。
    Input {
        /// 提交 ID。
        id: SubmissionId,
        /// 队列模式。
        mode: InputMode,
        /// 用户输入。
        content: UserContent,
        /// 输入被放置成的条目；缺省是 `kiana.user`。
        entry: Option<InputEntry>,
    },
    /// 条目写入（`entry` 是 `EntryDraft`，按普通 JSON 存储）。
    Write {
        /// 提交 ID。
        id: SubmissionId,
        /// 条目草稿。
        entry: EntryDraft,
    },
}

/// `steer` 或 `followUp`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    /// 在工具轮次边界插入。
    Steer,
    /// 在运行结束时插入。
    FollowUp,
}

impl InputMode {
    /// TS 的判别值。
    pub fn as_str(self) -> &'static str {
        match self {
            InputMode::Steer => "steer",
            InputMode::FollowUp => "followUp",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "steer" => Some(InputMode::Steer),
            "followUp" => Some(InputMode::FollowUp),
            _ => None,
        }
    }
}

impl InboxItem {
    /// 提交 ID。
    pub fn id(&self) -> SubmissionId {
        match self {
            InboxItem::Input { id, .. } | InboxItem::Write { id, .. } => *id,
        }
    }

    /// 是不是一次写入。
    pub fn is_write(&self) -> bool {
        matches!(self, InboxItem::Write { .. })
    }
}

// 手写 Serialize 只为键序：`id, mode, content, entry`；`Write` 的 `mode` 是字面量 "write"，
// 与 TS 的判别值一致。`Input` 的 `entry = None` 时整条键省略（TS 的 undefined 属性不出现）。
impl Serialize for InboxItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            InboxItem::Input {
                id,
                mode,
                content,
                entry,
            } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("mode", mode.as_str())?;
                map.serialize_entry("content", content)?;
                if let Some(entry) = entry {
                    map.serialize_entry("entry", entry)?;
                }
                map.end()
            }
            InboxItem::Write { id, entry } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("mode", "write")?;
                map.serialize_entry("entry", entry)?;
                map.end()
            }
        }
    }
}

// 先收成宽松的 RawItem（未知字段忽略，content/entry 可缺），再按 mode 分派：
// - "write"：entry 必填，交给 EntryDraft 的严格解析；
// - "steer"/"followUp"：content 必填，entry 缺失或 null 表示用默认的 kiana.user；
// - 其它 mode：报 unknown_variant（错误里列出三个合法值）。
impl<'de> Deserialize<'de> for InboxItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawItem {
            id: SubmissionId,
            mode: String,
            #[serde(default)]
            content: Option<UserContent>,
            #[serde(default)]
            entry: Option<JsonValue>,
        }

        let raw = RawItem::deserialize(deserializer)?;
        if raw.mode == "write" {
            let entry = raw.entry.ok_or_else(|| DeError::missing_field("entry"))?;
            let entry: EntryDraft = serde_json::from_value(entry).map_err(DeError::custom)?;
            return Ok(InboxItem::Write { id: raw.id, entry });
        }
        let mode = InputMode::from_str(&raw.mode)
            .ok_or_else(|| DeError::unknown_variant(&raw.mode, &["steer", "followUp", "write"]))?;
        let content = raw
            .content
            .ok_or_else(|| DeError::missing_field("content"))?;
        let entry = match raw.entry {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(serde_json::from_value(value).map_err(DeError::custom)?),
        };
        Ok(InboxItem::Input {
            id: raw.id,
            mode,
            content,
            entry,
        })
    }
}

/// 一个会话等待边界的提交队列，按 ID 顺序。
///
/// 这是文档值的完整形状；本模块的读写函数不经过它，而是直接操作 `items` 的原始 JSON
/// （见模块文档）。用它做类型化往返会丢掉未知字段与无法识别的项；`submissions.rs` 只在
/// 判断队列长度与“是否排着 reset”时把项解析成 [`InboxItem`]。
///
/// TS: `InboxState`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxState {
    /// 队列项。
    pub items: Vec<InboxItem>,
}

/// 内建队列文档。
///
/// `kind = "kiana.inbox"`、版本 1、会话作用域、`latest`/`initial`（fork 时子会话以空队列
/// 开始）。`initial` 是 `{ "items": [] }`；`checkpoint_when` 只在 `items` 为空时返回 true，
/// 让空队列存成完整 base，非空时继续沿用 delta 链（spec §2.7.1）。修改 kind/version/semantics
/// 会改变落盘与 fork 行为，是当前稳定契约，改动需同步测试。
///
/// TS: `InboxDoc`
pub static INBOX_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.inbox",
            1,
            DocumentSemantics::latest(DocumentFork::Initial),
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("items".to_owned(), JsonValue::Array(Vec::new()));
            value
        })
        // 空队列缩成 base（否则一次清空后仍要携带一串 delta）。
        .with_checkpoint_when(|value, _, _| {
            value
                .get("items")
                .and_then(JsonValue::as_array)
                .is_some_and(|items| items.is_empty())
        }),
    )
});

/// 一次边界读取的设置。
///
/// 两个模式在提交开始时从解析后的 [`Settings`] 复制进来，本次边界后续不再重新读取。
///
/// TS: `QueueModes`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueModes {
    /// steering 队列模式。
    pub steering_mode: QueueMode,
    /// follow-up 队列模式。
    pub follow_up_mode: QueueMode,
}

impl QueueModes {
    /// 从解析后的设置取出两个模式。
    pub fn from_settings(settings: &Settings) -> Self {
        QueueModes {
            steering_mode: settings.steering_mode,
            follow_up_mode: settings.follow_up_mode,
        }
    }
}

/// 一次边界在提交的第一次表写入之前读到的东西，以及它到目前为止见到的最新 head。
///
/// `inbox` 是事务草稿的活值：调用方在 prepare 之后继续 push 新项，[`apply_boundary`] 直接
/// 修改它。`head` 由 [`prepare_boundary`] 初始化；调用方可以在 `apply_boundary` 之前为 handoff
/// reset 直接推进它（`harness/generation.rs` 的 `finish_tool_round` 追加 reset 条目后写入
/// `boundary.head = Some(entry.id)`），[`apply_boundary`] 放置带 head 的写入时再继续推进。
/// 它用来判断后面的 write 是否 stale。
///
/// TS: `Boundary`
pub struct Boundary {
    /// 会话 ID。
    pub conversation_id: ConversationId,
    /// inbox 草稿。
    pub inbox: DocDraft,
    /// steering 队列模式。
    pub steering_mode: QueueMode,
    /// follow-up 队列模式。
    pub follow_up_mode: QueueMode,
    /// 活动范围的起点（最新 head 标记的 `head`）；被本次提交写入的 head 推进。
    pub head: Option<crate::types::EntryId>,
}

/// 被选中的用户项（按 inbox 位置升序），以及是否放置了一次 `head: "self"` 写入（一次 reset）。
///
/// `users` 的顺序就是 [`apply_boundary`] 内 `place_submission` 以及调用方随后 `start_run`
/// 的顺序；调用方只拿它启动运行，不会再次放置这些提交。实现按 inbox 下标排序；正常入队时 ID
/// 递增且追加在队尾，因此位置序与提交 ID 升序一致，但手工构造的队列可能不同。
///
/// `reset` 来自本次调用前的队列扫描：只要队列里有 `head: "self"` 的 write 就为 true，表示
/// 这个边界开启了新上下文。
///
/// TS: `BoundaryResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryResult {
    /// 被放置的用户提交。
    pub users: Vec<SubmissionId>,
    /// 是否放置了 reset。
    pub reset: bool,
}

/// 边界的种类。
///
/// `PostTools` 只放 steer；`Final` 还会放 follow-up。队列里存在 reset 写入时，`PostTools`
/// 也按 `Final` 处理（见 [`apply_boundary`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryAt {
    /// 工具轮次之后。
    PostTools,
    /// 运行结束。
    Final,
}

/// 读取一次边界需要的东西。表读取必须在提交的第一次表写入之前，因此调用方在提交开始时准备边界。
///
/// `head` 取 `latest_head_marker(conversation_id)` 记录的 `head` 字段（没有带 head 的可见
/// 条目时为 `None`），表示当前活动范围的起点；`inbox` 取得该会话的 `kiana.inbox` 草稿
/// （文档不存在时按定义创建）。两个读取的错误原样作为 `BoxError` 返回。
///
/// TS: `prepareBoundary`
pub async fn prepare_boundary(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    modes: QueueModes,
) -> Result<Boundary, BoxError> {
    let head = tx
        .latest_head_marker(conversation_id)
        .await?
        .and_then(|record| record.head);
    let inbox = tx
        .doc(&DocTarget::conversation(&INBOX_DOC, conversation_id))
        .await?;
    Ok(Boundary {
        conversation_id,
        inbox,
        steering_mode: modes.steering_mode,
        follow_up_mode: modes.follow_up_mode,
        head,
    })
}

/// 读取 inbox 草稿里**原始**的 `items` 数组。
///
/// TS 的 `items` 是文档里的普通 JSON 数组，`splice` 只移除选中的项，其余项（包括未类型化
/// 调用方多带的字段、未知 `mode`、`entry`/`data` 为 `null` 的项）原样保留。类型化往返会
/// 丢掉这些，所以这里直接操作 JSON。
///
/// 返回副本：调用方在副本上删除，再整体写回。缺少 `items` 当作空队列；`items` 不是数组时
/// 返回错误。
fn read_raw_items(draft: &DocDraft) -> Result<Vec<JsonValue>, BoxError> {
    match draft.value().get("items") {
        None => Ok(Vec::new()),
        Some(JsonValue::Array(items)) => Ok(items.clone()),
        Some(_) => Err("kiana.inbox items must be an array".into()),
    }
}

/// 用 `items` 整体覆盖草稿的 `items` 数组。
fn write_raw_items(draft: &mut DocDraft, items: &[JsonValue]) {
    draft
        .value_mut()
        .insert("items".to_owned(), JsonValue::Array(items.to_vec()));
}

/// `item.mode`（非对象或缺失时为 `None`，等价于 JS 的 `undefined`）。
///
/// 不是字符串的 `mode` 也返回 `None`，因此不会被当成 write/steer/followUp 选中。
fn item_mode(item: &JsonValue) -> Option<&str> {
    item.get("mode").and_then(JsonValue::as_str)
}

/// `item.id` 作为 [`SubmissionId`]。
///
/// 缺 `id` 返回 `"Inbox item has no id"`；JSON 形状不对时由 `SubmissionId` 的反序列化报错。
fn item_id(item: &JsonValue) -> Result<SubmissionId, BoxError> {
    let id = item.get("id").ok_or("Inbox item has no id")?;
    Ok(serde_json::from_value(id.clone())?)
}

/// 写入项的 `entry`（`EntryDraft`）。TS 直接把它交给 `appendEntry()`；`null` 会在读取
/// `entry.head` 时抛 TypeError。
///
/// 缺 `entry` 时返回 `"Inbox write has no entry"`。
fn write_entry_draft(item: &JsonValue) -> Result<EntryDraft, BoxError> {
    match item.get("entry") {
        None => Err("Inbox write has no entry".into()),
        Some(JsonValue::Null) => Err("Cannot read properties of null (reading 'head')".into()),
        Some(value) => Ok(serde_json::from_value(value.clone())?),
    }
}

/// 用户项的 `content`（`UserInput`）。
///
/// 缺 `content` 时返回 `"Inbox input has no content"`；形状不对由 `UserContent` 的反序列化
/// 报错。
fn input_content(item: &JsonValue) -> Result<UserContent, BoxError> {
    let content = item.get("content").ok_or("Inbox input has no content")?;
    Ok(serde_json::from_value(content.clone())?)
}

/// 用户项的 `entry`（`InputEntry`）：缺省表示 `kiana.user`。
///
/// TS 的 `pickInputEntry()` 只取 `kind` 与 `data`；`data: null` 会原样保留（`undefined`
/// 才省略），所以这里手工从对象取两个字段，而不是走会把 `null` 归一成 `None` 的
/// `Deserialize`。`entry: null` 在 TS 里会抛解构 TypeError。
///
/// `entry` 是非 null 的非对象、或对象里没有字符串 `kind` 时返回
/// `"Value contains a non-JSON undefined; expected strict JSON"`；`entry: null` 走单独分支，
/// 返回解构 TypeError 对应的文本
/// `"Cannot destructure property 'kind' of 'null' as it is null."`。
fn input_entry(item: &JsonValue) -> Result<Option<InputEntry>, BoxError> {
    match item.get("entry") {
        None => Ok(None),
        Some(JsonValue::Null) => {
            Err("Cannot destructure property 'kind' of 'null' as it is null.".into())
        }
        Some(JsonValue::Object(entry)) => {
            let Some(kind) = entry.get("kind").and_then(JsonValue::as_str) else {
                return Err("Value contains a non-JSON undefined; expected strict JSON".into());
            };
            Ok(Some(InputEntry {
                kind: kind.to_owned(),
                data: entry.get("data").cloned(),
            }))
        }
        Some(_) => Err("Value contains a non-JSON undefined; expected strict JSON".into()),
    }
}

/// `item.mode === "write" && item.entry.head === "self"` 的原始版本。
///
/// 在反序列化之前检查，因此容忍未知 `mode`、额外字段与不完整的项；只认字面量字符串
/// `"self"`，数字或 `null` 的 head 都不算 reset。
fn has_queued_reset_raw(items: &[JsonValue]) -> bool {
    items.iter().any(|item| {
        item_mode(item) == Some("write")
            && item
                .get("entry")
                .and_then(|entry| entry.get("head"))
                .and_then(JsonValue::as_str)
                == Some("self")
    })
}

/// 放置边界选中的排队项（spec §6）：每个写入、第一个或全部 steer，以及在 `final` 处的第一个或全部
/// follow-up。被选中的 reset 把 `postTools` 边界变成 `final`。写入先放置，用户项在它们之后，各自按
/// ID 顺序，因此在 reset 之前排队的用户项在新上下文里运行。head 指向活动范围之前（包括本次提交里
/// 更早开启的范围）的写入是 stale。被选中与 stale 的项都按位置移除。
///
/// # 执行顺序
///
/// 1. 把 inbox 草稿的 `items` 读成原始 JSON 数组（副本），计算 `reset` 与 `final_boundary`。
/// 2. 按 `mode` 扫描下标：steer 总是候选，follow-up 只在 `final_boundary` 时候选；
///    `OneAtATime` 对每种模式各取位置最靠前的一个。两组下标最后整体升序排序。
/// 3. 先处理全部 write（按 inbox 位置）：stale 的落定 `unanswered`/`stale` 并跳过；其余
///    `append_entry` 后按 `draft.head` 推进 `boundary.head`（`"self"` 用新条目 ID，数字用
///    该 ID），再 `place_submission`。
/// 4. 再按升序处理用户项：`append_input` 追加条目并 `place_submission`，ID 收进返回值。
/// 5. 把 write 与用户下标合并、降序删除（避免前面的删除让后面的下标位移），写回草稿。
///
/// 实现按 inbox 位置排序用户项；正常入队时 ID 递增且追加在队尾，因此位置序与提交 ID 升序
/// 一致，但手工构造的队列可能不同。
///
/// # 约束与失败
///
/// - 必须在 [`prepare_boundary`] 之后、同一事务里调用；对 `boundary.inbox` 的写入随事务提交。
/// - 任一项的 `id` 解析失败、用户项缺 `content`、写入项缺 `entry`，或 JSON 形状不符，都会
///   返回 `BoxError`；函数自身不做部分回滚，这次提交是否落盘由事务边界决定。
/// - `reset` 只看队列里是否存在 `head: "self"` 的 write（即使它排在 follow-up 之后），因此
///   返回的 `reset` 与“本次是否开启了新上下文”一致。
///
/// TS: `applyBoundary`
pub async fn apply_boundary(
    tx: &dyn Tx,
    boundary: &mut Boundary,
    at: BoundaryAt,
    now: i64,
) -> Result<BoundaryResult, BoxError> {
    // 阶段 1：读原始项并决定本次边界的种类。reset 可以把 PostTools 升级成 Final，
    // 让 follow-up 落进 reset 之后的新上下文。
    let mut items = read_raw_items(&boundary.inbox)?;
    let reset = has_queued_reset_raw(&items);
    let final_boundary = at == BoundaryAt::Final || reset;
    // 阶段 2：按 mode 收集候选下标；OneAtATime 每种模式只取位置最靠前的一个。
    let pick = |mode: &str, queue_mode: QueueMode| -> Vec<usize> {
        let indexes: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item_mode(item) == Some(mode))
            .map(|(index, _)| index)
            .collect();
        match queue_mode {
            QueueMode::All => indexes,
            QueueMode::OneAtATime => indexes.into_iter().take(1).collect(),
        }
    };
    let writes: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item_mode(item) == Some("write"))
        .map(|(index, _)| index)
        .collect();
    let mut users = pick("steer", boundary.steering_mode);
    if final_boundary {
        users.extend(pick("followUp", boundary.follow_up_mode));
    }
    users.sort_unstable();

    // 阶段 3：write 先放置。stale 的只落定不写条目；带 head 的写入推进活动范围。
    // `append_entry()` 复制草稿的值；条目只在这之后才被移除。
    for index in &writes {
        let item = &items[*index];
        let id = item_id(item)?;
        let draft = write_entry_draft(item)?;
        if is_stale(boundary, &draft) {
            tx.settle_submission(
                id,
                SubmissionSettlement::Unanswered {
                    reason: "stale".to_owned(),
                    detail: None,
                },
            );
            continue;
        }
        let entry = tx
            .append_entry(boundary.conversation_id, draft.clone())
            .await?;
        // 只有带 head 的写入推进活动范围：self 指向刚写入的条目，数字指向它自己。
        // stale 检查逐项针对当时的 `boundary.head`（可能已由调用方的 handoff reset 或本次更早
        // 的写入推进），它保证通过检查的数字 head 不会小于当前 head，所以这里只可能前进或保持
        // 不变。
        if let Some(head) = draft.head {
            boundary.head = Some(match head {
                EntryHead::SelfHead => entry.id,
                EntryHead::Id(head) => head,
            });
        }
        tx.place_submission(id, entry.id);
    }
    // 阶段 4：用户项按位置升序追加并放置。
    let mut placed: Vec<SubmissionId> = Vec::new();
    for index in &users {
        let item = &items[*index];
        let id = item_id(item)?;
        let content = input_content(item)?;
        let entry = input_entry(item)?;
        let record =
            append_input(tx, boundary.conversation_id, &content, entry.as_ref(), now).await?;
        tx.place_submission(id, record.id);
        placed.push(id);
    }
    // 阶段 5：降序删除（先删下标大的），避免前面的删除让后面的下标失效。
    let mut removed: Vec<usize> = writes.iter().chain(users.iter()).copied().collect();
    removed.sort_unstable_by(|left, right| right.cmp(left));
    for index in removed {
        items.remove(index);
    }
    write_raw_items(&mut boundary.inbox, &items);
    Ok(BoundaryResult {
        users: placed,
        reset,
    })
}

/// 追加输入被放置成的条目：`entry` 的 kind 与 data，或 `kiana.user`，带 `content` 的用户消息。
///
/// 两条路径都只写一条 `Message::User`，`timestamp` 取 `now`：
///
/// - `entry = None`：用 [`UserEntry`] 的类型化草稿追加，条目 kind 是 `kiana.user`，没有 data；
/// - `entry = Some`：先 [`pick_input_entry`] 丢掉多余字段，再手工拼 [`EntryDraft`]，键序设为
///   `kind, data, model`（TS 的 `{ ...pickInputEntry(entry), model }` 展开顺序）。`data` 为
///   `null` 时保留 `null`。
///
/// 返回追加好的记录；[`apply_boundary`] 用 `record.id` 调 `Tx::place_submission`，而空闲提交
/// 路径（`harness/submissions.rs`）把 `record.id` 填进提交记录的 `entry` 后直接 `start_run`，
/// 不调用 `place_submission`。错误来自底层的 `Tx::append_entry`（类型化路径还会经过 `UserEntry`
/// 的草稿转换）。
///
/// TS: `appendInput`
pub async fn append_input(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    content: &UserContent,
    entry: Option<&InputEntry>,
    now: i64,
) -> Result<EntryRecord, BoxError> {
    let message = Message::User(UserMessage {
        role: UserRole::default(),
        content: content.clone(),
        timestamp: now,
    });
    match entry {
        None => Ok(tx
            .append_entry_typed(
                &UserEntry,
                conversation_id,
                TypedEntryDraft::<NoData> {
                    model: Some(vec![message]),
                    data: NoData,
                    head: None,
                    edits: None,
                },
            )
            .await?
            .record),
        Some(entry) => {
            let picked = pick_input_entry(entry);
            tx.append_entry(
                conversation_id,
                EntryDraft {
                    kind: picked.kind,
                    model: Some(vec![message]),
                    data: picked.data,
                    head: None,
                    edits: None,
                    // TS: `{ ...pickInputEntry(entry), model }`——kind、data 在前，model 追加在后。
                    key_order: Some(
                        ["kind", "data", "model"]
                            .iter()
                            .map(|key| (*key).to_owned())
                            .collect(),
                    ),
                },
            )
            .await
        }
    }
}

/// 输入的条目 kind 与 data 的严格 JSON，不含未类型化调用方可能多传的字段。
///
/// `data: None` 表示 TS 的 `undefined`（序列化时省略键），`Some(JsonValue::Null)` 表示显式
/// null，两者不能合并。入队（`submissions.rs` 构造 [`InboxItem::Input`]）与放置
/// （[`append_input`]）都经过这里，保证两处写出的字段集合一致。
///
/// TS: `pickInputEntry`
pub fn pick_input_entry(entry: &InputEntry) -> InputEntry {
    InputEntry {
        kind: entry.kind.clone(),
        data: entry.data.clone(),
    }
}

/// 写入是否只用于展示：没有 `model`、`head` 或 `edits`（出现即算，即使是空值），所以它不改变模型
/// 看到的东西，也不需要边界。
///
/// `admit_submission` 用它走“立即追加并结算 done”的快捷路径——但如果队列里已经排着 reset，
/// 该写入必须等 reset 之后的上下文，不能走快捷路径。
///
/// TS: `isDisplayOnly`
pub fn is_display_only(entry: &EntryDraft) -> bool {
    entry.model.is_none() && entry.head.is_none() && entry.edits.is_none()
}

/// 是否排着一次 reset（`head: "self"` 写入）；在它落地之前追加的条目会离开活动范围。
///
/// 只看 [`InboxItem::Write`] 且 `head == EntryHead::SelfHead`；带数字 head 的写入不算。
/// `admit_submission` 的展示型写入快捷路径用它决定是否必须先排队。
///
/// TS: `hasQueuedReset`
pub fn has_queued_reset(items: &[InboxItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            InboxItem::Write {
                entry: EntryDraft {
                    head: Some(EntryHead::SelfHead),
                    ..
                },
                ..
            }
        )
    })
}

/// head 写入是否指向活动范围之前的条目，放置它会带回已经切掉的历史。
///
/// 只有 `head = Some(EntryHead::Id(h))`、边界已记录 head、且 `h < boundary.head` 时为真。
/// `SelfHead`、`head = None` 与 `boundary.head = None` 都不 stale；判断是严格小于，所以相等
/// 也不 stale。
///
/// TS: `isStale`
pub fn is_stale(boundary: &Boundary, entry: &EntryDraft) -> bool {
    match entry.head {
        Some(EntryHead::Id(id)) => boundary.head.is_some_and(|head| id < head),
        _ => false,
    }
}

/// 移除一个已撤回提交的项；调用方负责落定该提交。
///
/// 找不到匹配项（包括 `id` 无法解析成 [`SubmissionId`] 的项）时什么都不改，也不报错；
/// `items` 不是数组时才返回错误。只有确实删除了项才会写回草稿。
///
/// TS: `removeInboxItem`
pub async fn remove_inbox_item(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    id: SubmissionId,
) -> Result<(), BoxError> {
    let target = DocTarget::conversation(&INBOX_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    let mut items = read_raw_items(&draft)?;
    // TS: `items.findIndex(item => item.id === id)`；无法解析成 ID 的项不匹配。
    if let Some(index) = items
        .iter()
        .position(|item| item_id(item).is_ok_and(|item| item == id))
    {
        items.remove(index);
        write_raw_items(&mut draft, &items);
    }
    Ok(())
}

/// 撤回一个会话所有排队的输入，正如 `Conversation.abort()` 与 abort 级联所做的：每个都以 `reason`
/// （默认 `aborted`）落定为 `unanswered` 并离开 inbox；排队的写入留下等待稍后放置。
///
/// 倒序遍历（先处理下标大的项，删除不会影响尚未访问的下标），只跳过 `mode === "write"`；
/// 未知 mode 的项也会被落定并移除，缺 `id` 会让整个调用返回错误。循环结束后无论是否删过都
/// 调用 `write_raw_items` 把当前数组写回草稿。
///
/// TS: `withdrawQueuedInputs`
pub async fn withdraw_queued_inputs(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    reason: Option<&str>,
) -> Result<(), BoxError> {
    let reason = reason.unwrap_or("aborted");
    let target = DocTarget::conversation(&INBOX_DOC, conversation_id);
    let mut draft = tx.doc(&target).await?;
    let mut items = read_raw_items(&draft)?;
    // TS 只跳过 `mode === "write"`：未知 mode 的项也会被落定并移除。
    let mut index = items.len();
    while index > 0 {
        index -= 1;
        if item_mode(&items[index]) == Some("write") {
            continue;
        }
        let id = item_id(&items[index])?;
        tx.settle_submission(
            id,
            SubmissionSettlement::Unanswered {
                reason: reason.to_owned(),
                detail: None,
            },
        );
        items.remove(index);
    }
    write_raw_items(&mut draft, &items);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextEdit, id_from_number};

    fn input_item(id: i64) -> InboxItem {
        InboxItem::Input {
            id: id_from_number(id),
            mode: InputMode::Steer,
            content: UserContent::Text("hello".to_owned()),
            entry: None,
        }
    }

    fn write_item(id: i64, head: EntryHead) -> InboxItem {
        InboxItem::Write {
            id: id_from_number(id),
            entry: EntryDraft {
                key_order: None,
                kind: "custom.kind".to_owned(),
                model: None,
                data: Some(serde_json::json!({"a": 1})),
                head: Some(head),
                edits: Some(Vec::<ContextEdit>::new()),
            },
        }
    }

    #[test]
    fn the_doc_starts_with_an_empty_queue_and_checkpoints_when_empty() {
        let definition = &INBOX_DOC.definition;
        assert_eq!(definition.kind, "kiana.inbox");
        assert_eq!(definition.version, 1);
        assert_eq!(
            definition.semantics,
            DocumentSemantics::latest(DocumentFork::Initial)
        );
        let initial = (definition.initial)(None);
        assert_eq!(initial["items"], serde_json::json!([]));
        assert!((definition.checkpoint_when)(
            &initial,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
        let mut non_empty = JsonObject::new();
        non_empty.insert("items".to_owned(), serde_json::json!([{"id": 1}]));
        assert!(!(definition.checkpoint_when)(
            &non_empty,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
    }

    #[test]
    fn items_serialize_in_ts_key_order() {
        let item = InboxItem::Input {
            id: id_from_number(3),
            mode: InputMode::FollowUp,
            content: UserContent::Text("hi".to_owned()),
            entry: Some(InputEntry {
                kind: "app.note".to_owned(),
                data: None,
            }),
        };
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "id": 3,
                "mode": "followUp",
                "content": "hi",
                "entry": {"kind": "app.note"}
            })
        );
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(
            json,
            r#"{"id":3,"mode":"followUp","content":"hi","entry":{"kind":"app.note"}}"#
        );
    }

    #[test]
    fn writes_serialize_with_the_entry() {
        let item = write_item(4, EntryHead::SelfHead);
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(
            json,
            r#"{"id":4,"mode":"write","entry":{"kind":"custom.kind","data":{"a":1},"head":"self","edits":[]}}"#
        );
    }

    #[test]
    fn items_round_trip() {
        for item in [
            input_item(1),
            InboxItem::Input {
                id: id_from_number(2),
                mode: InputMode::FollowUp,
                content: UserContent::Text("x".to_owned()),
                entry: Some(InputEntry {
                    kind: "app.kind".to_owned(),
                    data: Some(serde_json::json!({"b": 2})),
                }),
            },
            write_item(3, EntryHead::Id(id_from_number(7))),
        ] {
            let value = serde_json::to_value(&item).unwrap();
            let parsed: InboxItem = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, item);
        }
    }

    #[test]
    fn unknown_modes_are_rejected() {
        let value = serde_json::json!({"id": 1, "mode": "sideways", "content": "x"});
        assert!(serde_json::from_value::<InboxItem>(value).is_err());
        let value = serde_json::json!({"id": 1, "mode": "write"});
        assert!(serde_json::from_value::<InboxItem>(value).is_err());
    }

    #[test]
    fn a_queued_reset_is_only_a_self_head_write() {
        assert!(!has_queued_reset(&[input_item(1)]));
        assert!(!has_queued_reset(&[write_item(
            1,
            EntryHead::Id(id_from_number(2))
        )]));
        assert!(has_queued_reset(&[write_item(1, EntryHead::SelfHead)]));
    }

    #[test]
    fn display_only_needs_absent_model_head_and_edits() {
        let mut entry = EntryDraft {
            key_order: None,
            kind: "k".to_owned(),
            model: None,
            data: Some(JsonValue::Null),
            head: None,
            edits: None,
        };
        assert!(is_display_only(&entry));
        entry.head = Some(EntryHead::SelfHead);
        assert!(!is_display_only(&entry));
        entry.head = None;
        entry.edits = Some(Vec::new());
        assert!(!is_display_only(&entry));
        entry.edits = None;
        entry.model = Some(Vec::new());
        assert!(!is_display_only(&entry));
    }

    #[test]
    fn a_head_before_the_active_range_is_stale() {
        let mut boundary = Boundary {
            conversation_id: id_from_number(1),
            inbox: DocDraft::new(JsonObject::new()),
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            head: Some(id_from_number(10)),
        };
        let mut entry = EntryDraft {
            key_order: None,
            kind: "k".to_owned(),
            model: None,
            data: None,
            head: Some(EntryHead::Id(id_from_number(9))),
            edits: None,
        };
        assert!(is_stale(&boundary, &entry));
        entry.head = Some(EntryHead::Id(id_from_number(10)));
        assert!(!is_stale(&boundary, &entry));
        entry.head = Some(EntryHead::SelfHead);
        assert!(!is_stale(&boundary, &entry));
        boundary.head = None;
        entry.head = Some(EntryHead::Id(id_from_number(1)));
        assert!(!is_stale(&boundary, &entry));
    }

    #[test]
    fn pick_input_entry_drops_outer_fields() {
        let entry = InputEntry {
            kind: "app.kind".to_owned(),
            data: Some(serde_json::json!({"x": true})),
        };
        let picked = pick_input_entry(&entry);
        assert_eq!(picked, entry);
    }
}
