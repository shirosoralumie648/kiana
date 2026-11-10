//! `harness/context.ts`：已提交上下文的读取与派生。
//!
//! TS: `packages/durable/src/harness/context.ts`。边界在 Session 线上用两次 O(1) 读取捕获；
//! 尾部及以下的条目不可变，因此 `derive_context()` 可以在线下扫描它们。
//!
//! # 为什么需要这个模块
//!
//! 模型每次请求都需要一份「当前上下文」：活动条目、每个条目贡献的消息，以及最终发给
//! provider 的消息数组。转录只追加、条目不可变（见 [`EntryRecord`]），所以整份上下文完全由
//! 一对边界决定：最新的 head 标记与范围末尾的条目。同一对边界必然派生出同一份视图。
//! 本模块把「捕获边界」（很短，由调用方放在会话变更线上）与「扫描并派生」（较长，可以离线做）
//! 拆开，并让连续读取复用上一次扫描的结果。
//!
//! # 与谁交互
//!
//! - `harness/harness.rs`：`ConversationTrait::context` 调 [`read_context`]，每次全量派生。
//! - `harness/scheduler.rs`：任务 runtime 调 [`read_context_from`]，把返回的 [`ContextRange`]
//!   缓存在调度器的 `State.contexts` 里，下一次读取作为 `previous` 传入。
//! - `harness/view.rs`：挂载会话视图时用 [`capture_context_bounds`] + [`active_entries`]，
//!   只要原始条目，不派生模型消息。
//! - `harness/compaction.rs`：用 [`order_tool_results`] 按调用顺序整理工具结果。
//!
//! # 主要流程
//!
//! 1. [`capture_context_bounds`] 取范围末尾（最新条目，或调用方指定的 `at`）与 `tail` 及之前
//!    最新的 head 标记。
//! 2. `scan_range` 从 head 标记的 `head` 字段扫到 `tail`，分页取完后再反转成从旧到新。
//! 3. `derive_range` 依次：收集编辑、选活动条目、计算每条贡献、按最后一个 assistant
//!    切成 settled/open、排序工具结果、把基线 system 消息领到最前。
//! 4. 有可复用的 `previous` 时，[`read_context_from`] 走捷径：tail 不变直接复用，tail 变小
//!    按 id 截断后重派生，tail 变大只扫描新增区间。
//!
//! # 注意事项
//!
//! - TS 用对象身份（`system !== baseline`）判断「第一条 system 消息是不是基线」。Rust 的
//!   [`Message`] 是拥有值，因此派生时额外记录来源 `Origin`（条目 id + model 下标）；
//!   替换编辑产生的消息与合成结果永远不算基线。
//! - [`ContextRange`] 保留 `select_active` 之前的原始条目：更旧 head 标记携带的 edits 也必须
//!   参与派生，否则截断/增量读取会与全新读取不一致（测试
//!   `a_truncated_read_keeps_edits_carried_by_older_head_markers` 锁定）。
//! - `scan_range` 依赖 `EntryQuery` 的默认扫描顺序是 `descending`（见 `crate::types::EntryQuery`），
//!   所以多页拼接之后要整体反转。
//! - [`capture_context_bounds`] 自己不在会话变更线上；[`read_context`] 与
//!   [`read_context_from`] 在内部用 `SessionImpl::read_on_line` 包住它。

use std::collections::HashSet;

use kiana_ai::{
    AssistantContentBlock, Message, StopReason, TextContent, TextKind, ToolCall, ToolResultMessage,
    ToolResultRole, UserContentBlock,
};
use kiana_chord::context::Context;

use crate::harness::types::{ContextView, EntryRangeQuery};
use crate::session::SessionImpl;
use crate::types::{
    BoxError, ContextEdit, ContextEditAction, ConversationId, Cursor, EntryId, EntryQuery,
    EntryRecord, Storage, SystemEntry, id_from_number,
};

const SCAN_PAGE_SIZE: usize = 256;

/// 合成工具结果的正文。
///
/// 范围在「工具调用」与「工具结果」之间被截断时触发——`at` 截断点落在调用条目上或调用与
/// 结果之间（测试 `reads_the_context_as_of_an_earlier_entry`），或者结果条目被 `Omit` 编辑
/// 省略、`Replace` 后不再含对应结果（见 `contribute`）——`order_tool_results` 会为缺失的
/// 调用补一条结果，正文固定用这段文本。`harness/types.rs`
/// 的 `context_message_json` 也按这段文本（加 `details` 标记）识别合成结果并改用 TS 的
/// 键序，所以它不是可以随手改的提示语。
pub(crate) const MISSING_RESULT_TEXT: &str =
    "Tool result unavailable: history ends before this call completed.";

/// 固定一个已提交上下文范围的 head 标记与最新可见条目。
///
/// 这对值就是「已提交上下文」的全部身份：条目不可变且只追加，所以只要 `head` 与 `tail`
/// 相同，派生结果就确定。`head` 是 `Storage::find_latest_head_marker` 在 `tail` 及之前找到的
/// 最新标记（标记条目自己的 `head` 字段指向活动范围起点）；没有标记时整段转录都是活动的。
/// `tail` 是范围末尾：`at` 读取时等于 `at`，否则是最新条目。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBounds {
    /// 最新可见 head 标记（它的 `head` 固定了范围起点）。
    pub head: Option<EntryRecord>,
    /// 范围末尾的条目。
    pub tail: EntryId,
}

/// 一次上下文读取：扫到的可见条目与由它们派生的视图。
///
/// [`read_context_from`] 把整个结构交给调用方缓存。`bounds` 决定下一次能否复用：head 标记
/// 相同且 tail 只增时只需扫描新增区间，tail 变小则截断重派生。`entries` 故意保存
/// `select_active` 之前的原始范围（含被丢弃的更旧 head 标记），因为它们的 edits 仍要生效；
/// `settled`/`open`/`baseline` 也保留下来，让增量扩展不必重新派生已有消息。
#[derive(Debug, Clone)]
pub struct ContextRange {
    /// 边界。
    pub bounds: ContextBounds,
    /// 扫到的可见条目（`selectActive` 之前），从旧到新。截断与扩展都基于它，因此旧 head
    /// 标记携带的 edits 不会丢失。
    pub entries: Vec<EntryRecord>,
    /// 派生出的视图。
    pub view: ContextView,
    /// `entries` 里编辑的目标。
    pub edited: HashSet<EntryId>,
    /// 最后一个 assistant 消息之前的已定模型消息。
    pub settled: Vec<Message>,
    /// 最后一个 assistant 消息起的贡献消息（工具结果排序之前）。
    pub open: Vec<Message>,
    /// 基线的 system 消息；见 `find_baseline`。
    pub baseline: Baseline,
    /// 内部簿记：消息来源，用来复现 TS 的对象身份比较（`system !== baseline`）。
    origins: RangeOrigins,
}

/// 一条消息在条目里的来源。TS 按对象身份判断「第一条 system 消息就是基线」，Rust 的消息是
/// 拥有值，因此记下来源 `(条目 id, model 下标)` 来得到等价的身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// 来自条目 `model` 数组的第 `usize` 条消息。
    Model(EntryId, usize),
    /// 来自编辑的替换消息：TS 里是另一批对象，永远不是基线对象。
    Edit,
    /// 没有来源标记（公开的 `order_tool_results` 只接收裸消息，测试辅助函数同样）：
    /// 只能回退到值比较。
    Unknown,
}

/// `ContextRange` 的内部来源簿记。
#[derive(Debug, Clone, Default)]
struct RangeOrigins {
    settled: Vec<Origin>,
    open: Vec<Origin>,
    baseline: Option<(EntryId, usize)>,
}

/// 基线的 system 消息：范围内不可能有基线（TS 的 `null`），或后续条目可能还是基线（`undefined`）。
///
/// `Absent` 与 `Undetermined` 的差别只在增量读取时有意义（见 `extend_range`）：`Undetermined`
/// 时新增条目里可能第一次出现基线，需要在新增区间重新查；`Absent` 与 `Message` 一旦确定，
/// 之后追加的条目不会改变结论，可以直接沿用。`Message` 与另两者的差别则每次派生都要用到：
/// 只有当当前 system 消息与它身份匹配（`is_baseline`）时，`lead_with_system_with_origins`
/// 才会在它前面已有非 user 消息的情况下仍把它前移。
#[derive(Debug, Clone, PartialEq)]
pub enum Baseline {
    /// 后续条目可能还是基线（TS: `undefined`）。
    Undetermined,
    /// 范围内不可能有基线（TS: `null`）。
    Absent,
    /// 基线的 system 消息。
    Message(Box<Message>),
}

/// 这些 stop reason 的 assistant 消息不进入模型上下文（`aborted`：调用方取消；
/// `error`：适配器或 provider 报错；`deferred`：结果需稍后轮询）。
///
/// 条目本身仍是活动条目（仍出现在 `ContextView::entries` 里），只是贡献为空
/// （测试 `excludes_aborted_error_and_deferred_assistant_messages_but_keeps_their_entries`）。
fn excluded_stop_reason(reason: StopReason) -> bool {
    matches!(
        reason,
        StopReason::Aborted | StopReason::Error | StopReason::Deferred
    )
}

/// 捕获当前上下文的边界，或在可见条目 `at` 处截断的上下文边界，只需两次 O(1) 读取。
/// 在 Session 线上运行；尾部及以下的条目不可变，因此 `derive_context()` 随后可以离线扫描它们。
///
/// 具体做法：`at` 为 `None` 时用降序扫描取 `limit = 1` 的第一条作为 `tail`（没有任何条目
/// 则返回 `Ok(None)`，调用方视为空上下文）；`at` 为 `Some` 时先用 `Storage::visible_entry`
/// 校验它属于该会话的祖先范围，再以它为 `tail`。最后用 `Storage::find_latest_head_marker`
/// 找 `tail` 及之前最新的 head 标记作为 `head`（可以没有）。
///
/// 失败：`at` 不可见时返回 `Err("Entry {at} is not visible from conversation {id}")`
/// （逐字消息由测试 `reads_the_context_as_of_an_earlier_entry` 锁定）；存储错误原样上传。
/// 本函数自己不进入会话变更线，调用方负责（[`read_context`] 与 [`read_context_from`] 已经这样做了）。
///
/// TS: `captureContextBounds`
pub async fn capture_context_bounds(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &dyn Context,
    at: Option<EntryId>,
) -> Result<Option<ContextBounds>, BoxError> {
    // tail：范围末尾。at 缺省时取最新条目；指定 at 时它本身就是末尾。
    let tail = match at {
        None => {
            // 降序扫描的第一条就是最新条目；空会话没有边界。
            let query = EntryQuery {
                conversation_id,
                min_entry_id: None,
                max_entry_id: None,
                order: None,
            };
            let page = storage.scan_entries(&query, 1, None, context).await?;
            match page.items.first() {
                Some(entry) => entry.id,
                None => return Ok(None),
            }
        }
        Some(at) => {
            // at 不保证属于本会话：先做可见性校验，保持 TS 的错误文案。
            if storage
                .visible_entry(conversation_id, at, context)
                .await?
                .is_none()
            {
                return Err(format!(
                    "Entry {at} is not visible from conversation {conversation_id}"
                )
                .into());
            }
            at
        }
    };
    // head 标记可能不存在（还没发生过 reset/压缩）；此时整个转录都是活动的。
    let head = storage
        .find_latest_head_marker(conversation_id, Some(tail), context)
        .await?;
    Ok(Some(ContextBounds { head, tail }))
}

/// 复用 `previous`（同一会话更早的一次范围）的 `read_context()`：head 标记相同时只扫描它尾部之后的
/// 条目。返回视图与下一次读取要传的范围。
///
/// TS: `readContextFrom`
///
/// 复用判定只看 head 标记的 **id**：条目不可变，id 相同就是同一条标记。三种情况：
/// - `tail` 相同：直接克隆 `previous`；
/// - `tail` 更小（`at` 指向更早的条目）：按 id 过滤 `previous.entries` 后重新派生；
/// - `tail` 更大：只扫描 `(previous.tail, bounds.tail]`，交给 `extend_range` 增量扩展。
///
/// head 标记不同或没有 `previous` 时全量扫描。边界为空（空会话）时返回空视图与 `None`，
/// 表示调用方不应缓存范围。返回的视图是范围的拷贝，调用方可以同时保留两者。
pub async fn read_context_from(
    session: &SessionImpl,
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &dyn Context,
    at: Option<EntryId>,
    previous: Option<&ContextRange>,
) -> Result<(ContextView, Option<ContextRange>), BoxError> {
    let bounds = session
        .read_on_line(capture_context_bounds(
            storage,
            conversation_id,
            context,
            at,
        ))
        .await?;
    // 空会话没有边界，也就没有可缓存的范围。
    let Some(bounds) = bounds else {
        return Ok((empty_view(), None));
    };
    // 只比较 head 标记的 id：标记条目不可变，同一个 id 一定是同一条标记。
    let previous_head =
        previous.and_then(|previous| previous.bounds.head.as_ref().map(|head| head.id));
    let bounds_head = bounds.head.as_ref().map(|head| head.id);
    let range = match previous {
        Some(previous) if previous_head == bounds_head => {
            // tail 没变：同一对边界，视图必然相同。
            if bounds.tail == previous.bounds.tail {
                previous.clone()
            } else if bounds.tail < previous.bounds.tail {
                // 读得更早：previous.entries 覆盖得更远，按 id 截断即可。
                let entries: Vec<EntryRecord> = previous
                    .entries
                    .iter()
                    .filter(|entry| entry.id <= bounds.tail)
                    .cloned()
                    .collect();
                derive_range(&bounds, entries)
            } else {
                // 读得更晚：只扫描 (previous.tail, bounds.tail] 这一段。
                let min_entry_id = id_from_number(previous.bounds.tail.get() + 1);
                let added = scan_range(
                    storage,
                    conversation_id,
                    EntryRangeQuery {
                        min_entry_id: Some(min_entry_id),
                        max_entry_id: Some(bounds.tail),
                        order: None,
                    },
                    context,
                )
                .await?;
                extend_range(previous, &bounds, &added)
            }
        }
        _ => {
            // 换了 head 标记（reset/压缩）或没有缓存：只能全量扫描。
            let entries =
                scan_range(storage, conversation_id, range_query(&bounds), context).await?;
            derive_range(&bounds, entries)
        }
    };
    // 视图按值返回，范围本身继续留给调用方缓存。
    let view = ContextView {
        head: range.view.head.clone(),
        entries: range.view.entries.clone(),
        contributions: range.view.contributions.clone(),
        messages: range.view.messages.clone(),
    };
    Ok((view, Some(range)))
}

/// 一个会话的已提交上下文：边界在 Session 线上捕获，条目在线下派生。
///
/// TS: `readContext`
///
/// 每次调用都重新扫描整个活动范围，不保留状态；需要重复读取同一会话时用 [`read_context_from`]。
/// `at` 为 `Some` 时只派生到该可见条目为止（测试 `reads_the_context_as_of_an_earlier_entry`
/// 逐个截断点锁定了结果）。
pub async fn read_context(
    session: &SessionImpl,
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &dyn Context,
    at: Option<EntryId>,
) -> Result<ContextView, BoxError> {
    let bounds = session
        .read_on_line(capture_context_bounds(
            storage,
            conversation_id,
            context,
            at,
        ))
        .await?;
    derive_context(storage, conversation_id, bounds.as_ref(), context).await
}

/// 在捕获的边界内派生一个会话的活动转录与模型上下文。
///
/// TS: `deriveContext`
///
/// `bounds` 为 `None` 时返回空视图（空会话）。本函数不经过会话变更线，只做存储扫描，因此可以
/// 在边界捕获之后离线运行；扫描被 `bounds.tail` 限制，之后追加的条目不会进入这次派生。
pub async fn derive_context(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<&ContextBounds>,
    context: &dyn Context,
) -> Result<ContextView, BoxError> {
    let Some(bounds) = bounds else {
        return Ok(empty_view());
    };
    let entries = scan_range(storage, conversation_id, range_query(bounds), context).await?;
    Ok(derive_range(bounds, entries).view)
}

fn empty_view() -> ContextView {
    ContextView {
        head: None,
        entries: Vec::new(),
        contributions: Vec::new(),
        messages: Vec::new(),
    }
}

/// 由 `bounds` 内扫到的条目派生上下文视图。
///
/// 输入 `entries` 是从旧到新的原始扫描结果（`select_active` 之前）。步骤与顺序都有含义：
/// 1. 先收集范围内**所有**条目的编辑（包括马上会被丢弃的旧 head 标记），同一 target 后写覆盖先写；
/// 2. `select_active` 选出活动条目；
/// 3. 每条目按编辑算出贡献消息，展平后按最后一个 assistant 切成 settled/open；
/// 4. 找基线，把消息数组组装成「settled + 排序后的 open」，基线 system 消息可能被领到最前；
/// 5. 视图的 `entries` 是活动条目，而 [`ContextRange`] 的 `entries` 仍是原始扫描结果。
fn derive_range(bounds: &ContextBounds, entries: Vec<EntryRecord>) -> ContextRange {
    let mut edits: indexmap::IndexMap<EntryId, ContextEdit> = indexmap::IndexMap::new();
    // 范围内每个条目的编辑都算，包括 `select_active()` 丢掉的更旧 head 标记。
    // 后写覆盖先写：同一 target 被多次编辑时以最新的为准
    // （测试 `applies_the_newest_edit_per_target_within_the_active_range`）。
    for entry in &entries {
        for edit in entry.edits.iter().flatten() {
            edits.insert(edit.target, edit.clone());
        }
    }
    // 活动条目：head 标记本身 + 范围内非标记条目。
    let active = select_active(bounds.head.as_ref(), entries.clone());
    let contributions: Vec<Vec<(Message, Origin)>> = active
        .iter()
        .map(|entry| contribute(entry, edits.get(&entry.id)))
        .collect();
    // 展平时还看不出该在哪里切开，交给 settle 按最后一个 assistant 决定。
    let flat: Vec<(Message, Origin)> = contributions.iter().flatten().cloned().collect();
    let (settled, open) = settle_with_origins(&[], &flat);
    let (baseline, baseline_origin) = find_baseline(bounds.head.as_ref(), &active);
    // 最终消息 = 已定稿部分 + 排序后的开放部分。
    let mut messages: Vec<(Message, Origin)> = settled.clone();
    messages.extend(order_tool_results_with_origins(&open));
    let messages = lead_with_system_with_origins(messages, &baseline, baseline_origin);
    // 视图只暴露活动条目与贡献；原始 entries 留在 ContextRange 里供增量读取。
    let view = ContextView {
        head: bounds.head.clone(),
        entries: active,
        contributions: contributions
            .iter()
            .map(|messages| {
                messages
                    .iter()
                    .map(|(message, _)| message.clone())
                    .collect()
            })
            .collect(),
        messages,
    };
    // 来源数组与 settled/open 一一对应；后续增量读取依赖这个不变式。
    let origins = RangeOrigins {
        settled: settled.iter().map(|(_, origin)| *origin).collect(),
        open: open.iter().map(|(_, origin)| *origin).collect(),
        baseline: baseline_origin,
    };
    ContextRange {
        bounds: bounds.clone(),
        entries,
        view,
        edited: edits.keys().copied().collect(),
        settled: settled.into_iter().map(|(message, _)| message).collect(),
        open: open.into_iter().map(|(message, _)| message).collect(),
        baseline,
        origins,
    }
}

/// 同一 head 标记下用尾部的 `added` 扩展示 `previous`：只派生它们与 open 消息。新增的编辑可能改变
/// 更早的条目，所以那时整段重新派生。
///
/// 能走增量的前提：head 标记没变，且新增区间里没有会回头影响已有条目的东西。以下任一情况
/// 会退化为整段重派生（`derive_range`）：
/// - 新增条目带 `edits`：编辑可以指向更早的条目；
/// - 新增条目本身是 head 标记：活动范围可能改变；
/// - 新增条目是已知编辑的 target：它此前不在范围里，现在必须按编辑重新贡献。
///
/// 增量路径只重新派生 `added` 的贡献：`previous.settled` 作为前缀保留，`previous.open` 与
/// 新增贡献拼接后重新切分（旧的 open 消息可能因此被移入 settled），`origins` 随重新切分后的
/// settled/open 重建；复用的只是消息值与来源，分区本身并不原样保留。切分前两处 assert 检查
/// 来源数组与消息数组没有脱节（失配属于内部 bug，直接 panic 而不是返回错误）。
fn extend_range(
    previous: &ContextRange,
    bounds: &ContextBounds,
    added: &[EntryRecord],
) -> ContextRange {
    // 原始范围也要扩展：ContextRange.entries 必须包含新条目。
    let mut entries = previous.entries.clone();
    entries.extend_from_slice(added);
    // 新增条目可能回头改变已有派生结果的情况，全部退化为全量重派生。
    let changed = added.iter().any(|entry| {
        entry.edits.is_some() || entry.head.is_some() || previous.edited.contains(&entry.id)
    });
    if changed {
        return derive_range(bounds, entries);
    }
    // 到这里可以确定新条目不受任何编辑影响，直接按无编辑计算贡献。
    let contributions: Vec<Vec<(Message, Origin)>> =
        added.iter().map(|entry| contribute(entry, None)).collect();
    assert_eq!(
        previous.settled.len(),
        previous.origins.settled.len(),
        "settled origins stay in step with settled messages"
    );
    assert_eq!(
        previous.open.len(),
        previous.origins.open.len(),
        "open origins stay in step with open messages"
    );
    let settled_previous: Vec<(Message, Origin)> = previous
        .settled
        .iter()
        .cloned()
        .zip(previous.origins.settled.iter().copied())
        .collect();
    let mut open: Vec<(Message, Origin)> = previous
        .open
        .iter()
        .cloned()
        .zip(previous.origins.open.iter().copied())
        .collect();
    open.extend(contributions.iter().flatten().cloned());
    // 把旧的 settled/open 与新增贡献重新切分：最后一个 assistant 可能变了。
    let (settled, open) = settle_with_origins(&settled_previous, &open);
    // 基线一旦确定就不会被后续条目改变；只有 Undetermined 才需要在新增区间里再找。
    let (baseline, baseline_origin) = if previous.baseline != Baseline::Undetermined {
        (previous.baseline.clone(), previous.origins.baseline)
    } else {
        find_baseline(bounds.head.as_ref(), added)
    };
    // 视图同样按增量拼接：新增条目都不是 head 标记（否则上面已经全量重派生）。
    let mut view_entries = previous.view.entries.clone();
    view_entries.extend_from_slice(added);
    let mut view_contributions = previous.view.contributions.clone();
    view_contributions.extend(contributions.iter().map(|messages| {
        messages
            .iter()
            .map(|(message, _)| message.clone())
            .collect()
    }));
    let mut messages: Vec<(Message, Origin)> = settled.clone();
    messages.extend(order_tool_results_with_origins(&open));
    let messages = lead_with_system_with_origins(messages, &baseline, baseline_origin);
    let view = ContextView {
        head: bounds.head.clone(),
        entries: view_entries,
        contributions: view_contributions,
        messages,
    };
    let origins = RangeOrigins {
        settled: settled.iter().map(|(_, origin)| *origin).collect(),
        open: open.iter().map(|(_, origin)| *origin).collect(),
        baseline: baseline_origin,
    };
    ContextRange {
        bounds: bounds.clone(),
        entries,
        view,
        edited: previous.edited.clone(),
        settled: settled.into_iter().map(|(message, _)| message).collect(),
        open: open.into_iter().map(|(message, _)| message).collect(),
        baseline,
        origins,
    }
}

fn is_system(message: &Message) -> bool {
    message.role() == "system"
}

/// `entries` 中（从旧到新）的基线：head 标记之后第一个 `kiana.system` 条目的 system 消息，或没有 head
/// 标记的转录里第一个 `kiana.system` 条目；generation 在写到它时让每个分段与工具都已生效。
///
/// 返回值同时带上基线消息的来源 `(条目 id, model 下标)`，用来复现 TS 的对象身份比较。
///
/// 判定顺序与三种结果：
/// - 跳过 `id <= head.id` 的条目：head 标记与更早的历史不参与；
/// - 遇到第一个 `kiana.system` 条目就下结论——含 system 消息则是基线；`model` 缺失或不含
///   system 消息则返回 [`Baseline::Absent`]，不再往后找（之后的 `kiana.system` 条目通常只带
///   增量补丁，见 `harness/prompt.rs` 的 `plan_system_entries`）；
/// - 没有 head 标记时，在第一个 `kiana.system` 条目之前，任何携带 system 消息的条目
///   （自己的 `model`，或 `Replace` 编辑里的消息）都会让结果为 `Absent`：视图里的第一条
///   system 消息可能来自任意 kind 的条目，无法确认它就是基线；
/// - 扫完仍没有结论则返回 `Undetermined`，表示后续追加的条目里可能还会出现基线。
///
/// TS: `findBaseline`
fn find_baseline(
    head: Option<&EntryRecord>,
    entries: &[EntryRecord],
) -> (Baseline, Option<(EntryId, usize)>) {
    for entry in entries {
        // head 标记之前（含标记自身）的条目与基线无关。
        if let Some(head) = head
            && entry.id <= head.id
        {
            continue;
        }
        // 第一个 kiana.system 条目就是结论：没有 system 消息说明范围内没有基线，
        // 后面的同类条目只是补丁。
        if SystemEntry.is(entry) {
            let Some(messages) = entry.model.as_ref() else {
                return (Baseline::Absent, None);
            };
            let Some(index) = messages.iter().position(is_system) else {
                return (Baseline::Absent, None);
            };
            return (
                Baseline::Message(Box::new(messages[index].clone())),
                Some((entry.id, index)),
            );
        }
        // 没有 head 标记时，前面出现过 system 消息就无法确认“第一条 system 就是基线”。
        if head.is_none() && carries_system_message(entry) {
            return (Baseline::Absent, None);
        }
    }
    (Baseline::Undetermined, None)
}

/// 条目自己的消息、或它编辑的替换消息里是否有 system 消息。
///
/// 只看 `Replace` 编辑（`Omit` 不携带消息）。用于没有 head 标记时的保守判定：即使这条
/// system 消息后来被编辑省略，它的存在仍会让基线判定为 `Absent`
/// （测试 `does_not_lead_with_a_system_entry_written_after_another_system_message`）。
fn carries_system_message(entry: &EntryRecord) -> bool {
    entry
        .model
        .as_ref()
        .is_some_and(|messages| messages.iter().any(is_system))
        || entry.edits.as_ref().is_some_and(|edits| {
            edits.iter().any(|edit| {
                edit.action == ContextEditAction::Replace
                    && edit
                        .messages
                        .as_ref()
                        .is_some_and(|messages| messages.iter().any(is_system))
            })
        })
}

/// 当它前面只有 user 消息、或它就是基线时，把第一条 system 消息移到最前。
///
/// TS: `leadWithSystem`。TS 用对象身份比较 `system !== baseline`；这里用 `Origin` 复现：
/// 来源相同（同一条目的同一 model 下标）才算基线，编辑替换产生的消息永远不是；来源未知
/// （`Unknown`，只来自无来源的包装函数）时回退到值比较。
///
/// 只看**第一条** system 消息，是否前移由身份判定决定：它是基线（[`Baseline::Message`] 身份
/// 命中）时无条件前移，即使前面已有 assistant（测试
/// `a_baseline_written_after_the_first_answers_leads_when_nothing_earlier_was_a_system_message`）；
/// 不是基线时，只有前面全是 user 消息才前移，前面已有非 user 消息则保持原位。`Edit` 来源的
/// system 消息只保证不会被认作基线（即使值与基线相同），当前面只有 user 消息时仍会前移。
fn lead_with_system_with_origins(
    messages: Vec<(Message, Origin)>,
    baseline: &Baseline,
    baseline_origin: Option<(EntryId, usize)>,
) -> Vec<Message> {
    let Some(index) = messages.iter().position(|(message, _)| is_system(message)) else {
        return messages.into_iter().map(|(message, _)| message).collect();
    };
    if index == 0 {
        return messages.into_iter().map(|(message, _)| message).collect();
    }
    let (system, origin) = messages[index].clone();
    // 身份判定：编辑替换（Edit）与来源未知的降级比较各有规则，前者永远不是基线。
    let is_baseline = match baseline {
        Baseline::Message(baseline_message) => match origin {
            Origin::Model(entry_id, model_index) => {
                baseline_origin == Some((entry_id, model_index))
            }
            Origin::Edit => false,
            Origin::Unknown => baseline_message.as_ref() == &system,
        },
        Baseline::Absent | Baseline::Undetermined => false,
    };
    // 不是基线、且前面已有非 user 消息：保持原位。
    if !is_baseline
        && messages[..index]
            .iter()
            .any(|(message, _)| message.role() != "user")
    {
        return messages.into_iter().map(|(message, _)| message).collect();
    }
    // 前移只改变这一条的位置，其余消息保持相对顺序。
    let mut result = Vec::with_capacity(messages.len());
    result.push(system);
    result.extend(messages[..index].iter().map(|(message, _)| message.clone()));
    result.extend(
        messages[index + 1..]
            .iter()
            .map(|(message, _)| message.clone()),
    );
    result
}

/// 一个活动条目在它的编辑与排除的 stop reason 之后的模型消息，工具结果排序之前，带来源标记。
///
/// - `Omit`：贡献为空；
/// - `Replace`：用编辑里的消息，来源一律 `Edit`（永远不会被当作基线）；`messages` 缺省时为空；
/// - 其他：用条目自己的 `model`，来源为 `Model(条目 id, 下标)`。
///
/// `model` 为 `None` 的条目（展示/记账用）贡献为空，但仍留在活动条目列表里。
fn contribute(entry: &EntryRecord, edit: Option<&ContextEdit>) -> Vec<(Message, Origin)> {
    if let Some(edit) = edit
        && edit.action == ContextEditAction::Omit
    {
        return Vec::new();
    }
    // 编辑缺省 messages 时按空贡献处理，不 panic。
    let empty: Vec<Message> = Vec::new();
    let (contributed, from_edit): (&[Message], bool) = match edit {
        Some(edit) if edit.action == ContextEditAction::Replace => {
            (edit.messages.as_ref().unwrap_or(&empty), true)
        }
        _ => (entry.model.as_ref().unwrap_or(&empty), false),
    };
    // enumerate 在 filter 之前：来源里的下标始终是 model（或替换消息）里的原始下标，
    // 与 find_baseline 的 position 结果可比。
    contributed
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message.role() != "assistant"
                || match message {
                    Message::Assistant(assistant) => !excluded_stop_reason(assistant.stop_reason),
                    _ => true,
                }
        })
        .map(|(index, message)| {
            let origin = if from_edit {
                Origin::Edit
            } else {
                Origin::Model(entry.id, index)
            };
            (message.clone(), origin)
        })
        .collect()
}

/// 把 `open` 最后一个 assistant 消息之前的已排序消息移到 `settled`。
///
/// 切分点取**最后一个** assistant：它之前的消息在后续追加中不会再变，可以先定稿；从它
/// 开始（含）的消息留在 open，增量读取时与新增条目一起重新切分。
/// 没有 assistant、或最后一个 assistant 就是第一条时，open 原样保留。
fn settle_with_origins(
    settled: &[(Message, Origin)],
    open: &[(Message, Origin)],
) -> (Vec<(Message, Origin)>, Vec<(Message, Origin)>) {
    let last = open
        .iter()
        .rposition(|(message, _)| message.role() == "assistant");
    match last {
        // 没有 assistant 或它就在开头：还没有可定稿的前缀。
        None | Some(0) => (settled.to_vec(), open.to_vec()),
        Some(last) => {
            let mut next_settled = settled.to_vec();
            next_settled.extend(order_tool_results_with_origins(&open[..last]));
            (next_settled, open[last..].to_vec())
        }
    }
}

/// 单元测试用的无来源版本。[`settle_with_origins`] 的前身，只为测试保持原签名。
#[cfg(test)]
fn settle(settled: &[Message], open: &[Message]) -> (Vec<Message>, Vec<Message>) {
    let settled: Vec<(Message, Origin)> = settled
        .iter()
        .cloned()
        .map(|message| (message, Origin::Unknown))
        .collect();
    let open: Vec<(Message, Origin)> = open
        .iter()
        .cloned()
        .map(|message| (message, Origin::Unknown))
        .collect();
    let (settled, open) = settle_with_origins(&settled, &open);
    (
        settled.into_iter().map(|(message, _)| message).collect(),
        open.into_iter().map(|(message, _)| message).collect(),
    )
}

/// 单元测试用的无来源版本：`Unknown` 来源回退到值比较，与迁移前的行为一致。
#[cfg(test)]
fn lead_with_system(messages: Vec<Message>, baseline: &Baseline) -> Vec<Message> {
    let messages = messages
        .into_iter()
        .map(|message| (message, Origin::Unknown))
        .collect();
    lead_with_system_with_origins(messages, baseline, None)
}

/// 捕获边界内的原始活动条目，不派生模型上下文。
///
/// TS: `activeEntries`
///
/// 返回 `select_active` 的结果：有 head 标记时标记本身排在最前，随后是非标记条目。它与
/// [`derive_context`] 的 `view.entries` 一致（测试
/// `active_entries_and_derive_context_agree_on_the_entries` 锁定），但不计算消息。
/// `bounds` 为 `None` 时返回空列表。
pub async fn active_entries(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    bounds: Option<&ContextBounds>,
    context: &dyn Context,
) -> Result<Vec<EntryRecord>, BoxError> {
    let Some(bounds) = bounds else {
        return Ok(Vec::new());
    };
    let range = scan_range(storage, conversation_id, range_query(bounds), context).await?;
    Ok(select_active(bounds.head.as_ref(), range))
}

/// `bounds` 的可见范围：从 head 标记的 head（或转录起点）到尾部。
///
/// `min_entry_id` 用的是标记条目的 `head` 字段（活动范围起点），不是标记条目自己的 id；
/// head 为 `None` 时从转录开头扫描。`order` 留空，走存储的默认降序。
fn range_query(bounds: &ContextBounds) -> EntryRangeQuery {
    EntryRangeQuery {
        min_entry_id: bounds.head.as_ref().and_then(|head| head.head),
        max_entry_id: Some(bounds.tail),
        order: None,
    }
}

/// `range` 内的可见条目，从旧到新。
///
/// 按 `SCAN_PAGE_SIZE` 分页，用上一页返回的 `Cursor` 续扫（游标会保持创建时的顺序）。
/// `EntryQuery.order` 为 `None` 时存储默认按降序（新 → 旧）返回，所以每页都是倒序；
/// 所有页拼接后再整体 `reverse`，得到从旧到新的顺序。`min/max` 是闭区间。
async fn scan_range(
    storage: &dyn Storage,
    conversation_id: ConversationId,
    range: EntryRangeQuery,
    context: &dyn Context,
) -> Result<Vec<EntryRecord>, BoxError> {
    let mut entries: Vec<EntryRecord> = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let query = EntryQuery {
            conversation_id,
            min_entry_id: range.min_entry_id,
            max_entry_id: range.max_entry_id,
            order: range.order,
        };
        let page = storage
            .scan_entries(&query, SCAN_PAGE_SIZE, cursor.as_ref(), context)
            .await?;
        entries.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    // 多页按降序累积，反转后才是从旧到新；编辑覆盖与基线查找都依赖这个顺序。
    entries.reverse();
    Ok(entries)
}

/// head 标记后跟范围里非 head 的条目；没有标记时是整个范围。
///
/// 标记条目即使出现在 `range` 里也会先被过滤掉、再显式放在最前，避免重复；落在范围里的
/// 更旧 head 标记（`head` 非空的条目）同样被丢弃。丢弃只影响活动条目列表，它们的 edits
/// 已由 `derive_range` 先收集。
fn select_active(head: Option<&EntryRecord>, range: Vec<EntryRecord>) -> Vec<EntryRecord> {
    match head {
        None => range,
        Some(head) => {
            let mut active = Vec::with_capacity(range.len() + 1);
            active.push(head.clone());
            active.extend(range.into_iter().filter(|entry| entry.head.is_none()));
            active
        }
    }
}

/// 把每个 assistant 的工具结果按调用顺序放到它后面。结果取自下一条 assistant 之前的消息；缺失的
/// 结果会被合成，未匹配的结果被丢弃。
///
/// TS: `orderToolResults`
///
/// 纯函数，不访问存储；除重排工具结果与合成缺失结果之外，其余消息的顺序与内容保持不变。
/// 幂等：排好序的消息再排一次结果不变（测试 `order_tool_results_is_idempotent` 锁定）。
pub fn order_tool_results(messages: &[Message]) -> Vec<Message> {
    let pairs: Vec<(Message, Origin)> = messages
        .iter()
        .cloned()
        .map(|message| (message, Origin::Unknown))
        .collect();
    order_tool_results_with_origins(&pairs)
        .into_iter()
        .map(|(message, _)| message)
        .collect()
}

/// [`order_tool_results`] 的来源标记版本：重排只移动消息，合成结果永远不是基线对象。
///
/// 算法：顺序遍历。`toolResult` 消息在主循环里一律跳过，只有被某个 assistant 的工具调用
/// 匹配到才会在调用之后输出。遇到带工具调用的 assistant 时，向后扫到下一个 assistant
/// 为止，按 `tool_call_id` 记录每个调用的第一条结果（重复的后续结果被忽略），再按调用
/// 顺序输出；没有结果就合成一条。之后主循环从下一条消息继续，因此被跳过的结果不会重复
/// 输出，夹在中间的非结果消息仍按原顺序出现，但排在前面那个 assistant 的全部结果之后
/// （测试 `keeps_positional_system_messages_and_orders_tool_results_by_call_order` 锁定）。
fn order_tool_results_with_origins(messages: &[(Message, Origin)]) -> Vec<(Message, Origin)> {
    let mut ordered: Vec<(Message, Origin)> = Vec::with_capacity(messages.len());
    let mut index = 0;
    // 主循环保持非结果消息的相对顺序；工具结果只通过匹配输出。
    while index < messages.len() {
        let (message, origin) = &messages[index];
        // 裸工具结果先跳过：匹配不到调用就丢弃，匹配到的会在 assistant 之后输出。
        if message.role() == "toolResult" {
            index += 1;
            continue;
        }
        ordered.push((message.clone(), *origin));
        if message.role() != "assistant" {
            index += 1;
            continue;
        }
        // 只有带工具调用的 assistant 才需要整理结果。
        let (calls, timestamp) = match message {
            Message::Assistant(assistant) => {
                let calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(call) => Some(call.clone()),
                        _ => None,
                    })
                    .collect();
                (calls, assistant.timestamp)
            }
            _ => (Vec::new(), 0),
        };
        if calls.is_empty() {
            index += 1;
            continue;
        }
        // 结果窗口到下一条 assistant 为止；同一个 tool_call_id 只认第一条。
        let mut results: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
        let mut next = index + 1;
        while next < messages.len() && messages[next].0.role() != "assistant" {
            if let Message::ToolResult(result) = &messages[next].0
                && !results.contains_key(&result.tool_call_id)
            {
                results.insert(result.tool_call_id.clone(), next);
            }
            next += 1;
        }
        for call in &calls {
            match results.get(&call.id) {
                Some(result_index) => ordered.push(messages[*result_index].clone()),
                // 合成的结果不是任何条目的消息对象；`Edit` 表示「永不等于基线」。
                None => ordered.push((missing_result(call, timestamp), Origin::Edit)),
            }
        }
        index += 1;
    }
    ordered
}

/// 合成一条「工具结果缺失」的 `ToolResult`：`tool_call_id` / `tool_name` 取自调用，正文固定为
/// `MISSING_RESULT_TEXT`，`details` 标记 `{"reason": "missing_result"}`，`is_error = true`，
/// 时间戳沿用发起调用的 assistant。
///
/// 用途：`order_tool_results` 在调用之后的结果窗口里找不到匹配结果时合成一条，让模型仍能看到
/// 每个工具调用都有对应结果。常见情形：`at` 截断点落在调用条目上或调用与结果之间（测试
/// `reads_the_context_as_of_an_earlier_entry`），或结果条目被 `Omit` 编辑省略 / `Replace` 后
/// 不再含对应结果（见 `contribute`）。reset 的 `SelfHead` 会把调用一并切掉，压缩的
/// `head = first_kept` 保留连续后缀，两者都不会留下「调用在、结果不在」的缺口。
/// `harness/types.rs` 的 `context_message_json` 按这组字段识别
/// 合成结果并改用 TS 对象字面量的键序，因此改动任一字段都会影响序列化
/// （测试 `a_missing_result_serializes_is_error_before_details` 锁定键序）。
fn missing_result(call: &ToolCall, timestamp: i64) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::default(),
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::default(),
            text: MISSING_RESULT_TEXT.to_owned(),
            text_signature: None,
        })],
        details: Some(serde_json::json!({"reason": "missing_result"})),
        usage: None,
        nested_calls: None,
        is_error: true,
        timestamp,
        duration_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{JsonValue, UserEntry};
    use kiana_ai::{
        Api, AssistantMessage, AssistantRole, ProviderId, Usage, UserContent, UserMessage, UserRole,
    };

    fn assistant(_id: &str, calls: &[&str], stop_reason: StopReason) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::default(),
            content: calls
                .iter()
                .map(|name| {
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: Default::default(),
                        id: (*name).to_owned(),
                        name: (*name).to_owned(),
                        arguments: Default::default(),
                        thought_signature: None,
                        namespace: None,
                        arguments_error: None,
                    })
                })
                .collect(),
            api: Api::default(),
            provider: ProviderId::default(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 42,
            duration_ms: None,
        })
    }

    fn tool_result(id: &str) -> Message {
        missing_result(
            &ToolCall {
                kind_tool_call: Default::default(),
                id: id.to_owned(),
                name: id.to_owned(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            },
            0,
        )
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            role: UserRole::default(),
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })
    }

    #[test]
    fn tool_results_follow_calls_in_call_order() {
        let messages = vec![
            user("hi"),
            assistant("a1", &["one", "two"], StopReason::ToolUse),
            tool_result("two"),
            tool_result("one"),
            tool_result("stray"),
        ];
        let ordered = order_tool_results(&messages);
        let roles: Vec<&str> = ordered.iter().map(Message::role).collect();
        assert_eq!(roles, vec!["user", "assistant", "toolResult", "toolResult"]);
        let ids: Vec<&str> = ordered
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["one", "two"]);
    }

    #[test]
    fn a_missing_result_is_synthesized_with_the_call_identity() {
        let messages = vec![assistant("a1", &["one"], StopReason::ToolUse)];
        let ordered = order_tool_results(&messages);
        let Message::ToolResult(result) = &ordered[1] else {
            panic!("expected a tool result");
        };
        assert_eq!(result.tool_call_id, "one");
        assert_eq!(result.tool_name, "one");
        assert!(result.is_error);
        assert_eq!(result.timestamp, 42);
        assert_eq!(
            result.details,
            Some(serde_json::json!({"reason": "missing_result"}))
        );
        let UserContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert_eq!(text.text, MISSING_RESULT_TEXT);
    }

    #[test]
    fn a_missing_result_serializes_is_error_before_details() {
        // TS 的 `missingResult` 字面量：`role, toolCallId, toolName, content, isError, details,
        // timestamp`（kiana-ai 的结构体序会先写 details）。
        let messages = vec![assistant("a1", &["one"], StopReason::ToolUse)];
        let ordered = order_tool_results(&messages);
        let json = crate::harness::types::context_message_json(&ordered[1])
            .expect("the synthesized result serializes");
        let keys: Vec<&str> = json
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "role",
                "toolCallId",
                "toolName",
                "content",
                "isError",
                "details",
                "timestamp"
            ]
        );
    }

    #[test]
    fn results_belong_to_the_nearest_assistant() {
        let messages = vec![
            assistant("a", &["one"], StopReason::ToolUse),
            tool_result("one"),
            assistant("b", &["two"], StopReason::ToolUse),
            tool_result("two"),
        ];
        let ordered = order_tool_results(&messages);
        let ids: Vec<&str> = ordered
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult(result) => Some(result.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["one", "two"]);
    }

    #[test]
    fn settle_moves_everything_before_the_last_assistant() {
        let open = vec![user("a"), assistant("a", &[], StopReason::Stop), user("b")];
        let (settled, rest) = settle(&[], &open);
        assert_eq!(settled.len(), 1);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].role(), "assistant");
        assert_eq!(rest[1].role(), "user");
    }

    #[test]
    fn settle_keeps_open_when_the_last_assistant_is_first() {
        let open = vec![assistant("a", &[], StopReason::Stop), user("b")];
        let (settled, rest) = settle(&[], &open);
        assert!(settled.is_empty());
        assert_eq!(rest, open);
    }

    #[test]
    fn the_baseline_system_message_leads_when_only_users_precede_it() {
        let system = Message::System(kiana_ai::SystemMessage {
            role: Default::default(),
            content: kiana_ai::SystemContent::Text("base".to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        });
        let messages = vec![user("a"), user("b"), system.clone()];
        let led = lead_with_system(messages, &Baseline::Message(Box::new(system.clone())));
        assert_eq!(led[0].role(), "system");
        assert_eq!(led.len(), 3);
    }

    #[test]
    fn a_system_message_after_an_assistant_stays_in_place() {
        let system = Message::System(kiana_ai::SystemMessage {
            role: Default::default(),
            content: kiana_ai::SystemContent::Text("base".to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        });
        let messages = vec![assistant("a", &[], StopReason::Stop), system.clone()];
        let led = lead_with_system(messages.clone(), &Baseline::Undetermined);
        assert_eq!(led, messages);
    }

    #[test]
    fn a_leading_system_message_is_never_moved() {
        let system = Message::System(kiana_ai::SystemMessage {
            role: Default::default(),
            content: kiana_ai::SystemContent::Text("base".to_owned()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 0,
        });
        let messages = vec![system.clone(), user("a")];
        let led = lead_with_system(messages.clone(), &Baseline::Absent);
        assert_eq!(led, messages);
    }

    #[test]
    fn entries_are_typed_by_their_kind_tokens() {
        let entry = EntryRecord {
            key_order: None,
            model: Some(vec![user("hi")]),
            data: None,
            edits: None,
            kind: UserEntry.kind.to_string(),
            id: id_from_number(1),
            conversation_id: id_from_number(1),
            head: None,
            by_task_id: None,
        };
        assert!(UserEntry.is(&entry));
        assert!(!SystemEntry.is(&entry));
        let system_entry = EntryRecord {
            key_order: None,
            kind: SystemEntry.kind.to_string(),
            model: None,
            data: Some(JsonValue::Null),
            edits: None,
            id: id_from_number(2),
            conversation_id: id_from_number(1),
            head: None,
            by_task_id: None,
        };
        assert!(SystemEntry.is(&system_entry));
    }
}
