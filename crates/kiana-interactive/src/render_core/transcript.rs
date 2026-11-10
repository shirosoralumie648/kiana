//! 不可变事件日志 → 渲染模型的投影。
//!
//! # 规则
//!
//! 1. **Action 被 Observation 原地替换**：工具调用的 action 先建一张 `ToolCard`，
//!    对应 observation 到达时只填它的 observation 字段——界面上始终只有一张卡。
//!    `Thought` 的 observation 被忽略（thought 提升为独立条目）；`Finish` 的
//!    observation 也被忽略（内容在 action 里），避免重复消息。
//! 2. **连续 tool run 折叠**：相邻的 ToolCard 属于同一个 [`RenderGroupKind::ToolRun`]；
//!    任何非工具条目都会断开该组。
//! 3. **thought 提升**：thought 是独立的渲染条目，不会被它的 observation 替换。
//! 4. **group breaker**：artifact / error / finish 各自成组，并且会打断 tool run。
//! 5. **provisional slot**：流式文本只存在于 UI 模型里，id 与最终 durable 事件相同，
//!    用 `anchor_seq` 定位到「第一个 seq 大于 anchor 的 durable 事件之前」。durable 事件
//!    到达时原地退役 slot，内容不闪断。`attempt` 更高的 delta 会重置 slot（重试），
//!    更低或相等的 attempt 不会重复追加。
//! 6. **delta 按帧批量**：`push_delta` 只排队；`flush_due(now)` 按最小帧间隔提交；
//!    任何非 delta 事件先 `flush_pending`，因此控制事件永远不会被迟到的 delta 越过。
//!
//! # 与参考实现的关系
//!
//! 行为取自 OpenHands `src/utils/handle-event-for-ui.ts`（Action/Observation 合并、
//! streaming slot 的 `anchor_seq`/`attempt` 语义）。Kiana 侧的 `AgentSessionEvent` 是
//! 另一套类型，因此这里用独立的 [`DurableEvent`] 建模，由接线层把会话事件翻译进来。
//!
//! # 纯函数性
//!
//! 本模块不读时钟（`now` 由调用方传入）、不渲染终端、不依赖主题；所有状态都在
//! [`TranscriptProjection`] 里，可直接单测。

use std::collections::BTreeMap;

/// durable 事件内容。
#[derive(Clone, Debug, PartialEq)]
pub enum DurableEventKind {
    /// 用户消息。
    UserMessage {
        /// 文本。
        text: String,
    },
    /// assistant 文本（完整消息）。
    AssistantText {
        /// 文本。
        text: String,
    },
    /// thought（thinking / ThinkAction）。
    Thought {
        /// 文本。
        text: String,
    },
    /// 工具调用 action。
    Action {
        /// provider 的 tool_call id（用于卡片关联）。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
        /// 参数。
        args: serde_json::Value,
    },
    /// 工具结果 observation。
    Observation {
        /// 被替换的 action 事件 id。
        action_id: String,
        /// provider 的 tool_call id。
        tool_call_id: String,
        /// 工具名（action 缺失时用于建卡）。
        tool_name: String,
        /// 内容。
        content: String,
        /// 是否错误。
        is_error: bool,
    },
    /// artifact（文件/产物）。
    Artifact {
        /// 路径或标识。
        path: String,
        /// 摘要。
        summary: String,
    },
    /// 错误条目。
    Error {
        /// 错误文本。
        message: String,
    },
    /// turn 结束。
    Finish {
        /// 结束文本。
        message: String,
    },
}

/// 一条不可变 durable 事件。
#[derive(Clone, Debug, PartialEq)]
pub struct DurableEvent {
    /// 稳定事件 id（与流式 slot 的 id 相同）。
    pub id: String,
    /// 服务端/日志序号；`anchor_seq` 比较用。
    pub seq: u64,
    /// 内容。
    pub kind: DurableEventKind,
}

impl DurableEvent {
    /// 构造事件。
    pub fn new(id: impl Into<String>, seq: u64, kind: DurableEventKind) -> Self {
        Self {
            id: id.into(),
            seq,
            kind,
        }
    }
}

/// 工具 observation 的渲染数据。
#[derive(Clone, Debug, PartialEq)]
pub struct ObservationData {
    /// 输出文本。
    pub content: String,
    /// 是否错误。
    pub is_error: bool,
}

/// 工具卡片状态（action + 可选 observation，同一张卡）。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCardState {
    /// provider tool_call id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 参数。
    pub args: serde_json::Value,
    /// observation；`None` 表示仍在运行。
    pub observation: Option<ObservationData>,
}

/// 渲染条目内容。
#[derive(Clone, Debug, PartialEq)]
pub enum RenderItemKind {
    /// 用户消息。
    UserMessage {
        /// 文本。
        text: String,
    },
    /// assistant 消息（durable 或 provisional）。
    AssistantMessage {
        /// 文本。
        text: String,
    },
    /// thought。
    Thought {
        /// 文本。
        text: String,
    },
    /// 一张工具卡。
    ToolCard(ToolCardState),
    /// artifact。
    Artifact {
        /// 路径。
        path: String,
        /// 摘要。
        summary: String,
    },
    /// 错误。
    Error {
        /// 文本。
        message: String,
    },
    /// turn 结束。
    Finish {
        /// 文本。
        message: String,
    },
}

/// 一个渲染条目。
#[derive(Clone, Debug, PartialEq)]
pub struct RenderItem {
    /// 稳定 id；provisional 条目与最终 durable 条目相同。
    pub id: String,
    /// durable seq；provisional 为 `None`。
    pub seq: Option<u64>,
    /// provisional 的锚点 seq（UI-only 排序用）。
    pub anchor_seq: Option<u64>,
    /// 是否是 UI-only 流式槽位。
    pub provisional: bool,
    /// 当前 attempt（provisional 用）。
    pub attempt: u64,
    /// 流式 reasoning 内容（provisional assistant 用）。
    pub reasoning: String,
    /// 条目内容。
    pub kind: RenderItemKind,
}

impl RenderItem {
    fn durable(id: String, seq: u64, kind: RenderItemKind) -> Self {
        Self {
            id,
            seq: Some(seq),
            anchor_seq: None,
            provisional: false,
            attempt: 0,
            reasoning: String::new(),
            kind,
        }
    }

    fn provisional(id: String, attempt: u64, anchor_seq: Option<u64>) -> Self {
        Self {
            id,
            seq: None,
            anchor_seq,
            provisional: true,
            attempt,
            reasoning: String::new(),
            kind: RenderItemKind::AssistantMessage {
                text: String::new(),
            },
        }
    }
}

/// 渲染组类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderGroupKind {
    /// 单条消息（user / assistant / thought）。
    Message,
    /// 连续工具调用。
    ToolRun,
    /// artifact。
    Artifact,
    /// error。
    Error,
    /// finish。
    Finish,
}

/// 一个渲染组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderGroup {
    /// 组类型。
    pub kind: RenderGroupKind,
    /// 条目在 [`TranscriptProjection::items`] 里的下标（顺序）。
    pub items: Vec<usize>,
}

/// delta 的通道。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaKind {
    /// 可见文本。
    Text,
    /// reasoning / thinking。
    Reasoning,
}

/// 一个流式 delta chunk。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaChunk {
    /// 最终 durable 事件的 id。
    pub item_id: String,
    /// 锚定序号；slot 插在第一个 seq 更大的 durable 事件之前。
    pub anchor_seq: Option<u64>,
    /// 重试 attempt；更高者覆盖旧内容。
    pub attempt: u64,
    /// 通道。
    pub kind: DeltaKind,
    /// 增量文本。
    pub content: String,
}

impl DeltaChunk {
    /// 构造 text delta。
    pub fn text(item_id: &str, anchor_seq: Option<u64>, attempt: u64, content: &str) -> Self {
        Self {
            item_id: item_id.to_string(),
            anchor_seq,
            attempt,
            kind: DeltaKind::Text,
            content: content.to_string(),
        }
    }
}

/// 默认最小帧间隔（约 60 fps）。
pub const DEFAULT_MIN_FRAME_INTERVAL_MS: i64 = 16;

/// 投影状态机。
#[derive(Debug)]
pub struct TranscriptProjection {
    items: Vec<RenderItem>,
    pending_deltas: Vec<DeltaChunk>,
    last_frame_ms: Option<i64>,
    min_frame_interval_ms: i64,
    frame_commits: usize,
    slots_retired: usize,
    deltas_dropped: u64,
}

impl Default for TranscriptProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl TranscriptProjection {
    /// 新建投影。
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            pending_deltas: Vec::new(),
            last_frame_ms: None,
            min_frame_interval_ms: DEFAULT_MIN_FRAME_INTERVAL_MS,
            frame_commits: 0,
            slots_retired: 0,
            deltas_dropped: 0,
        }
    }

    /// 设置最小 delta 提交间隔（0 表示每次 `flush_due` 都提交）。
    pub fn with_min_frame_interval_ms(mut self, interval: i64) -> Self {
        self.min_frame_interval_ms = interval.max(0);
        self
    }

    /// 当前条目（含 provisional）。
    pub fn items(&self) -> &[RenderItem] {
        &self.items
    }

    /// 未提交的 delta 数。
    pub fn pending_delta_count(&self) -> usize {
        self.pending_deltas.len()
    }

    /// 已提交的帧数（一次 `flush_pending` 算一帧）。
    pub fn frame_commits(&self) -> usize {
        self.frame_commits
    }

    /// 因 durable 事件退役的 slot 数。
    pub fn slots_retired(&self) -> usize {
        self.slots_retired
    }

    /// 被 attempt 规则丢弃的 delta 数。
    pub fn deltas_dropped(&self) -> u64 {
        self.deltas_dropped
    }

    /// provisional 条目数。
    pub fn provisional_count(&self) -> usize {
        self.items.iter().filter(|item| item.provisional).count()
    }

    /// 按当前条目顺序重建组。
    pub fn groups(&self) -> Vec<RenderGroup> {
        let mut groups: Vec<RenderGroup> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let kind = match &item.kind {
                RenderItemKind::ToolCard(_) => RenderGroupKind::ToolRun,
                RenderItemKind::Artifact { .. } => RenderGroupKind::Artifact,
                RenderItemKind::Error { .. } => RenderGroupKind::Error,
                RenderItemKind::Finish { .. } => RenderGroupKind::Finish,
                RenderItemKind::UserMessage { .. }
                | RenderItemKind::AssistantMessage { .. }
                | RenderItemKind::Thought { .. } => RenderGroupKind::Message,
            };
            if let Some(last) = groups.last_mut()
                && last.kind == RenderGroupKind::ToolRun
                && kind == RenderGroupKind::ToolRun
            {
                last.items.push(index);
                continue;
            }
            groups.push(RenderGroup {
                kind,
                items: vec![index],
            });
        }
        groups
    }

    /// 应用一条 durable 事件；先 flush 掉尚未提交的 delta（控制事件不能乱序）。
    pub fn apply_durable(&mut self, event: DurableEvent) -> &RenderItem {
        self.flush_pending();

        // 1. 流式 slot 的 id 与最终事件相同：原地退役，保持位置不闪断。
        if let Some(index) = self.items.iter().position(|item| item.id == event.id)
            && self.items[index].provisional
        {
            self.items[index] =
                RenderItem::durable(event.id, event.seq, event_kind_to_render(event.kind));
            self.slots_retired += 1;
            return &self.items[index];
        }

        match event.kind {
            DurableEventKind::Observation {
                action_id,
                tool_call_id,
                tool_name,
                content,
                is_error,
            } => {
                if let Some(index) = self.items.iter().position(|item| item.id == action_id) {
                    match &mut self.items[index].kind {
                        RenderItemKind::ToolCard(card) => {
                            card.observation = Some(ObservationData { content, is_error });
                        }
                        // thought / finish 的 observation 不替换 action（内容在 action 里）。
                        RenderItemKind::Thought { .. } | RenderItemKind::Finish { .. } => {}
                        _ => {
                            self.items.insert(
                                index + 1,
                                RenderItem::durable(
                                    event.id,
                                    event.seq,
                                    RenderItemKind::ToolCard(ToolCardState {
                                        tool_call_id,
                                        tool_name,
                                        args: serde_json::Value::Null,
                                        observation: Some(ObservationData { content, is_error }),
                                    }),
                                ),
                            );
                            return &self.items[index + 1];
                        }
                    }
                    return &self.items[index];
                }
                self.items.push(RenderItem::durable(
                    event.id,
                    event.seq,
                    RenderItemKind::ToolCard(ToolCardState {
                        tool_call_id,
                        tool_name,
                        args: serde_json::Value::Null,
                        observation: Some(ObservationData { content, is_error }),
                    }),
                ));
            }
            other => {
                self.items.push(RenderItem::durable(
                    event.id,
                    event.seq,
                    event_kind_to_render(other),
                ));
            }
        }
        self.items.last().expect("just pushed")
    }

    /// 排队一个 delta；提交由 [`Self::flush_due`] / [`Self::flush_pending`] 控制。
    pub fn push_delta(&mut self, delta: DeltaChunk) {
        self.pending_deltas.push(delta);
    }

    /// 设置 slot 的锚点（`item_started` 帧到达但还没有 delta 时）。
    pub fn open_slot(&mut self, item_id: &str, anchor_seq: Option<u64>, attempt: u64) {
        if self.items.iter().any(|item| item.id == item_id) {
            return;
        }
        let index = self.slot_insert_index(anchor_seq);
        self.items.insert(
            index,
            RenderItem::provisional(item_id.to_string(), attempt, anchor_seq),
        );
    }

    /// 到点则提交待处理 delta；返回是否提交了一帧。
    pub fn flush_due(&mut self, now_ms: i64) -> bool {
        if self.pending_deltas.is_empty() {
            return false;
        }
        let due = match self.last_frame_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= self.min_frame_interval_ms,
        };
        if due {
            self.flush_pending_at(now_ms);
        }
        due
    }

    /// 立即提交所有待处理 delta（非 delta 事件之前必须调用）。
    pub fn flush_pending(&mut self) {
        if self.pending_deltas.is_empty() {
            return;
        }
        let now = self.last_frame_ms.unwrap_or(0);
        self.flush_pending_at(now);
    }

    fn flush_pending_at(&mut self, now_ms: i64) {
        let deltas = std::mem::take(&mut self.pending_deltas);
        for delta in deltas {
            self.apply_delta(delta);
        }
        self.frame_commits += 1;
        self.last_frame_ms = Some(now_ms);
    }

    fn apply_delta(&mut self, delta: DeltaChunk) {
        let mut index = self
            .items
            .iter()
            .position(|item| item.id == delta.item_id && item.provisional);
        if index.is_none() {
            // durable 已经到达：迟到的 delta 直接丢弃（不复活 slot）。
            if self.items.iter().any(|item| item.id == delta.item_id) {
                self.deltas_dropped += 1;
                return;
            }
            let insert = self.slot_insert_index(delta.anchor_seq);
            self.items.insert(
                insert,
                RenderItem::provisional(delta.item_id.clone(), delta.attempt, delta.anchor_seq),
            );
            index = Some(insert);
        }
        let index = index.expect("just inserted");
        let item = &mut self.items[index];
        if delta.attempt < item.attempt {
            self.deltas_dropped += 1;
            return;
        }
        if delta.attempt > item.attempt {
            // 更高 attempt 重放该项：旧尾部被覆盖。
            item.attempt = delta.attempt;
            item.reasoning.clear();
            if let RenderItemKind::AssistantMessage { text } = &mut item.kind {
                text.clear();
            }
        }
        match delta.kind {
            DeltaKind::Text => {
                if let RenderItemKind::AssistantMessage { text } = &mut item.kind {
                    text.push_str(&delta.content);
                }
            }
            DeltaKind::Reasoning => item.reasoning.push_str(&delta.content),
        }
    }

    fn slot_insert_index(&self, anchor_seq: Option<u64>) -> usize {
        let Some(anchor_seq) = anchor_seq else {
            return self.items.len();
        };
        self.items
            .iter()
            .position(|item| {
                item.seq.is_some_and(|seq| seq > anchor_seq)
                    || (item.provisional
                        && item.anchor_seq.is_some_and(|anchor| anchor > anchor_seq))
            })
            .unwrap_or(self.items.len())
    }

    /// 供接线层构建稳定渲染序列：durable 条目按原顺序，provisional 已按锚点插入。
    pub fn render_sequence(&self) -> Vec<(String, bool)> {
        self.items
            .iter()
            .map(|item| (item.id.clone(), item.provisional))
            .collect()
    }

    /// 工具卡按 tool_call_id 的索引（调试/测试用）。
    pub fn tool_cards(&self) -> BTreeMap<String, &ToolCardState> {
        let mut cards = BTreeMap::new();
        for item in &self.items {
            if let RenderItemKind::ToolCard(card) = &item.kind {
                cards.insert(card.tool_call_id.clone(), card);
            }
        }
        cards
    }
}

fn event_kind_to_render(kind: DurableEventKind) -> RenderItemKind {
    match kind {
        DurableEventKind::UserMessage { text } => RenderItemKind::UserMessage { text },
        DurableEventKind::AssistantText { text } => RenderItemKind::AssistantMessage { text },
        DurableEventKind::Thought { text } => RenderItemKind::Thought { text },
        DurableEventKind::Action {
            tool_call_id,
            tool_name,
            args,
        } => RenderItemKind::ToolCard(ToolCardState {
            tool_call_id,
            tool_name,
            args,
            observation: None,
        }),
        DurableEventKind::Artifact { path, summary } => RenderItemKind::Artifact { path, summary },
        DurableEventKind::Error { message } => RenderItemKind::Error { message },
        DurableEventKind::Finish { message } => RenderItemKind::Finish { message },
        DurableEventKind::Observation {
            tool_call_id,
            tool_name,
            content,
            is_error,
            ..
        } => RenderItemKind::ToolCard(ToolCardState {
            tool_call_id,
            tool_name,
            args: serde_json::Value::Null,
            observation: Some(ObservationData { content, is_error }),
        }),
    }
}
