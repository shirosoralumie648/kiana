//! `harness/partial-changes.ts`：流式 partial 在两次提交之间改变了什么。
//!
//! TS: `packages/durable/src/harness/partial-changes.ts`（184 行）。提交按增量写
//! `kiana.live.generation.message`：新文本用 append，而不是把增长的整串重新赋值（后者每次提交都要比较
//! 整个字符串，代价随长度增长）。工具调用的参数由每个调用自己的 `StreamingJsonParser` 跟踪，提交时
//! 按变更序列应用到已提交的参数上，参数本身在消息赋值里被 `keep` 跳过。
//!
//! # 为什么需要这个收集器
//!
//! 一个流式响应会在每个事件后更新 partial 消息；若每次节流提交都把整条消息重新赋值，写入会
//! 退化成整串替换，已经提交过的内容也要重新比较/写入。`PartialChanges` 只记住“自上次成功
//! 提交以来新增了什么”，`take` 据此生成只描述增量的写入计划，`commit` 把参数变更逐条重放到
//! 已提交参数上。内部跟踪状态（待提交文本与被取走调用的 `changes`）在 `take` 时清空，因此
//! 它只保留未提交的增量，而不是完整消息的副本。
//!
//! # 谁在用它
//!
//! `crate::harness::generation` 的 `stream_response` 在事件到达时调用 [`PartialChanges::observe`]，
//! 节流计时器到点时由 `commit_partial` 依次调用 [`PartialChanges::take`] 与
//! [`PartialChanges::commit`]，提交失败则调用 [`PartialChanges::restore`]。收集器由调用方用
//! `Arc<Mutex<PartialChanges>>` 包住；本类型自身不加锁，也不做 IO。
//!
//! # 一次提交的完整流程
//!
//! 1. 事件阶段：`TextDelta` / `ThinkingDelta` 累积到待提交文本；`ToolCallStart` 为这次调用
//!    新建一个独立的 `StreamingJsonParser`；`ToolCallDelta` 把解析器产出的 `JsonChange`
//!    按顺序追加到该调用的 `changes`；`*End` 事件删除对应的跟踪项。
//! 2. [`PartialChanges::take`]：把待提交文本变成 `AssignPlan::Append` 计划，取走每个被跟踪
//!    调用的参数变更与当前参数快照；同时把 `message` 里该调用的 `arguments` 清空，并在计划
//!    里标 `AssignPlan::Keep`，让 `assign_json` 保留已提交的值。
//! 3. [`PartialChanges::commit`]：先按计划把消息合并进 `generation.message`，再把取走的
//!    参数变更按顺序应用到对应块上。
//! 4. 提交失败：[`PartialChanges::restore`] 把参数变更按原顺序放回，下一次提交重新应用。
//!
//! # 关键约束与失败模式
//!
//! - `take` 与 `commit` 必须成对：`take` 会清空待提交文本，并清空被取走调用的 `changes`
//!   （消息里块缺失或 `type` 不符的调用保持不变，留待下一次 `take`）。一次 `take` 之后既不
//!   `commit` 也不 `restore`，这段参数增量就丢了（下一次提交只应用之后的变更）；文本不受
//!   影响，因为 `message` 始终携带完整文本。
//! - `restore` 只恢复工具参数变更；文本增量不恢复（见该方法的说明）。
//! - 参数增量只对“根值是非数组对象”的调用成立。根不是对象时该调用会被删除跟踪，之后它的
//!   参数完全由消息快照（`message.content[index].arguments`）写入。
//! - `commit` 用提交前 `generation.message.content` 的长度判断某调用是否已有已提交块；若它
//!   早先经过 `commit`、块却不见了（另一个写者重置了 `kiana.live`，或那次提交最终失败），
//!   就退回完整快照并停止跟踪，避免把只含增量的变更重放到一个空参数对象上。
//! - `observe` / `take` / `commit` / `restore` 都不是线程安全的；调用方必须自己串行化。

use kiana_ai::types::{AssistantContentBlock, AssistantMessageEvent};
use kiana_ai::utils::streaming_json::{JsonChange, JsonPath, PathSegment, StreamingJsonParser};
use serde_json::Value;

use crate::harness::json::{AssignPlan, PlanFields, PlanKey, TargetKey, assign_json, is_record};

/// `kiana.live.generation.message` 里保存的 partial 消息（TS: `PartialMessage`）。
///
/// 形状是 `AssistantMessage` 序列化出的 JSON：`content` 是块数组，块用 `type` 字段区分
/// `"text"` / `"thinking"` / `"toolCall"`。本模块只依赖 `content` 下标、块的 `type`，以及
/// 块内的 `text` / `thinking` / `arguments` 字段；消息的其余键不解析。生产调用方用
/// `serde_json::to_value(partial)` 得到这个值。
pub type PartialMessage = Value;

/// 一个被跟踪的工具调用自上次提交以来的参数变更与快照。
///
/// 两个字段来自同一次 [`PartialChanges::take`]：`changes` 用于把已提交参数推进到当前值，
/// `snapshot` 是这次提交若发现“已提交块丢失”时用来整体恢复的参数值。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallCommit {
    /// 自上次提交以来的变更，按顺序。
    ///
    /// 应用时逐条生效：`Set` 整体赋值，`Append` 拼接字符串。
    pub changes: Vec<JsonChange>,
    /// 调用参数的快照（提交时消息里的 `arguments`）。
    ///
    /// 块没有 `arguments` 键时是 `Value::Null`。
    pub snapshot: Value,
}

/// 一次 partial 提交除消息本身之外要写的东西（TS: `PartialCommit`）。
///
/// 由 [`PartialChanges::take`] 产生，交给 [`PartialChanges::commit`] 或
/// [`PartialChanges::restore`] 消费。`Clone` 让调用方可以在提交闭包里持有一份副本，同时把
/// 原始值留给失败时的 `restore`（`generation.rs` 的 `commit_partial` 就是这样用的）。
#[derive(Debug, Clone, PartialEq)]
pub struct PartialCommit {
    /// `assign_json` 写消息时用的计划：文本追加，工具调用参数留给 `changes`。
    pub plan: AssignPlan,
    /// 每个被跟踪工具调用（按 content 下标）自上次提交以来的变更与快照。
    ///
    /// 顺序是跟踪顺序（通常是 `ToolCallStart` 事件的到达顺序），不是排序后的下标。
    pub tool_calls: Vec<(usize, ToolCallCommit)>,
}

/// 一个被跟踪工具调用自上次提交以来改变了什么，以及是否有提交拿走过它的变更。
#[derive(Debug)]
struct ToolCallState {
    /// 该调用私有的参数解析器；片段边界由它自己维护，与其它调用互不影响。
    parser: StreamingJsonParser,
    /// 尚未被 `take` 取走的变更；`take` 取走后清空，提交失败由 `restore` 放回。
    changes: Vec<JsonChange>,
    /// `commit` 是否已经处理过该调用（把 `take` 取走的变更排进过应用列表）。
    ///
    /// 新调用第一次提交时是 `false`（`changes` 从根对象的 `Set` 开始，可以直接重放）；
    /// 处理过之后只持有增量，一旦已提交块被别的写者重置，就必须退回快照而不是重放增量。
    /// 事务最终失败时这个标记也不会复位（`restore` 只放回变更），因此它偏保守：最坏情况是
    /// 后续参数改走完整快照，值仍然正确。
    committed: bool,
}

/// 文本/思考字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    Text,
    Thinking,
}

impl TextField {
    fn as_str(self) -> &'static str {
        match self {
            TextField::Text => "text",
            TextField::Thinking => "thinking",
        }
    }
}

/// 一段待提交的文本增量。
#[derive(Debug)]
struct PendingText {
    /// 这段文本属于哪个 JSON 字段（`text` 或 `thinking`）。
    field: TextField,
    /// 自上次 `take` 以来累积的增量文本。
    text: String,
}

/// 从一次流式响应的事件里收集 partial 消息自上次提交以来的变化。
///
/// TS: `PartialChanges`。
///
/// 生命周期：事件到达时 `observe`，节流点 `take` + `commit`（失败时 `restore`）。它不做 IO、
/// 不加锁，也不校验消息最终形状——最终参数以 `ToolCallEnd` 之后 `message` 里的解析值为准。
///
/// 两个 `Vec` 都以 content 下标为键、线性查找；同一时刻每个下标至多一条记录
/// （`ToolCallStart` 会先删旧项再插入）。
#[derive(Debug, Default)]
pub struct PartialChanges {
    /// 各 content 下标待追加的文本。
    text: Vec<(usize, PendingText)>,
    /// 各 content 下标正在跟踪的工具调用。
    tool_calls: Vec<(usize, ToolCallState)>,
}

impl PartialChanges {
    /// 新的收集器。
    pub fn new() -> Self {
        PartialChanges::default()
    }

    fn text_slot(&mut self, index: usize) -> Option<&mut PendingText> {
        self.text
            .iter_mut()
            .find(|(slot, _)| *slot == index)
            .map(|(_, pending)| pending)
    }

    fn tool_call(&mut self, index: usize) -> Option<&mut ToolCallState> {
        self.tool_calls
            .iter_mut()
            .find(|(slot, _)| *slot == index)
            .map(|(_, call)| call)
    }

    /// 记录 `event` 改变了什么（TS: `observe`）。
    ///
    /// 各类事件的处理：
    /// - `TextDelta` / `ThinkingDelta`：同一 content 下标、字段相同就拼接；字段不同（该下标
    ///   换成了另一种块）则以本次 delta 重新开始，之前未提交的文本被丢弃——partial 的该块
    ///   已经不再属于旧字段。
    /// - `ToolCallStart`：先删掉该下标可能存在的旧跟踪项，再插入一个全新的状态（新解析器、
    ///   空变更、`committed = false`）。
    /// - `ToolCallDelta`：没有跟踪项时忽略。解析器给出空路径（根值）变更时，只接受“值是
    ///   对象”的 `Set`；数组 / 标量根或根上的 `Append` 都会删除跟踪项并丢弃本次已解析的
    ///   变更——这类参数无法用增量表达，之后只能提交消息里的完整快照。
    /// - `TextEnd` / `ThinkingEnd`：删除该下标的待提交文本；块结束后的完整文本由 `message`
    ///   携带，下一个 `take` 会按整体值写入，不需要把最后一段补进待提交区。
    /// - `ToolCallEnd`：删除该下标的跟踪项，最终参数同样以 `message` 里的解析值为准。
    /// - 其它事件（`Start`、`TextStart` / `ThinkingStart`、`Done`、`Error`）不改变状态。
    ///
    /// 没有返回值，也不会失败：解析器的输出总是被接受或按上面的规则丢弃。
    pub fn observe(&mut self, event: &AssistantMessageEvent) {
        match event {
            AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                ..
            }
            | AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                ..
            } => {
                let field = if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
                    TextField::Text
                } else {
                    TextField::Thinking
                };
                let index = *content_index;
                match self.text_slot(index) {
                    Some(pending) if pending.field == field => pending.text.push_str(delta),
                    Some(pending) => {
                        pending.field = field;
                        pending.text = delta.clone();
                    }
                    None => self.text.push((
                        index,
                        PendingText {
                            field,
                            text: delta.clone(),
                        },
                    )),
                }
            }
            AssistantMessageEvent::ToolCallStart { content_index, .. } => {
                let index = *content_index;
                self.tool_calls.retain(|(slot, _)| *slot != index);
                self.tool_calls.push((
                    index,
                    ToolCallState {
                        parser: StreamingJsonParser::new(),
                        changes: Vec::new(),
                        committed: false,
                    },
                ));
            }
            AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                ..
            } => {
                let index = *content_index;
                let Some(call) = self.tool_call(index) else {
                    return;
                };
                let mut drop_call = false;
                for change in call.parser.append_changes(delta) {
                    // 参数必须是对象：其它根值展示为“没有参数”，改为提交快照。
                    if path_is_empty(&change) {
                        let root = match &change {
                            JsonChange::Set { value, .. } => Some(value),
                            JsonChange::Append { .. } => None,
                        };
                        if !is_record(root) {
                            drop_call = true;
                            break;
                        }
                    }
                    call.changes.push(change);
                }
                if drop_call {
                    self.tool_calls.retain(|(slot, _)| *slot != index);
                }
            }
            AssistantMessageEvent::TextEnd { content_index, .. }
            | AssistantMessageEvent::ThinkingEnd { content_index, .. } => {
                let index = *content_index;
                self.text.retain(|(slot, _)| *slot != index);
            }
            AssistantMessageEvent::ToolCallEnd { content_index, .. } => {
                let index = *content_index;
                self.tool_calls.retain(|(slot, _)| *slot != index);
            }
            _ => {}
        }
    }

    /// 取走自上次调用以来的变化，用于提交 `message`（partial 的副本）。
    ///
    /// 被跟踪调用的参数在 `message` 里被清空：提交按变更写入它们。提交失败时调用
    /// [`Self::restore`]。
    ///
    /// 产出物由三部分组成：
    /// 1. 文本计划：每个待提交文本下标生成
    ///    `Fields { <field>: Append { text } }`，随后清空待提交区。
    /// 2. 工具计划：对每个被跟踪调用，若 `message.content[index]` 是对象且 `type` 为
    ///    `"toolCall"`，取走 `changes` 快照、把消息里的 `arguments` 置为 `{}`，再在计划里给
    ///    `arguments` 标 `AssignPlan::Keep`（`assign_json` 因此不会用 `{}` 覆盖已提交值）。
    ///    块缺失或类型不符时跳过该调用：跟踪项与变更原样保留，等下一次 `take`。
    /// 3. 整体计划：`Fields { "content": Fields(blocks) }`。`generation.message` 里尚不存在
    ///    的 content 下标会被 `assign_json` 整块 push（此时参数是 `{}`，随后由 `commit` 的
    ///    变更重放填充）。
    ///
    /// `take` 之后必须 `commit`；提交失败必须 `restore`。只 `take` 会让这次取走的工具参数
    /// 增量丢失（文本仍会随下一次完整消息写入）。
    ///
    /// TS: `take`。
    pub fn take(&mut self, message: &mut Value) -> PartialCommit {
        // 文本：每个 content 下标一条「字段 → Append」计划。
        let mut blocks: PlanFields = PlanFields::new();
        for (index, pending) in &self.text {
            let mut fields: PlanFields = PlanFields::new();
            fields.insert(
                PlanKey::Name(pending.field.as_str().to_owned()),
                AssignPlan::Append {
                    text: pending.text.clone(),
                },
            );
            blocks.insert(PlanKey::Index(*index), AssignPlan::Fields(fields));
        }
        self.text.clear();
        // 工具：只在消息里仍是 `toolCall` 块时取出变更；参数被清空并在计划里标 `Keep`，
        // 由 `commit` 按变更重放，避免用 `{}` 覆盖已提交值。
        let mut tool_calls: Vec<(usize, ToolCallCommit)> = Vec::new();
        for (index, call) in &mut self.tool_calls {
            let Some(block) = message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .and_then(|content| content.get_mut(*index))
            else {
                continue;
            };
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            let snapshot = block.get("arguments").cloned().unwrap_or(Value::Null);
            tool_calls.push((
                *index,
                ToolCallCommit {
                    changes: call.changes.clone(),
                    snapshot,
                },
            ));
            block.insert("arguments".to_owned(), Value::Object(Default::default()));
            let mut fields: PlanFields = PlanFields::new();
            fields.insert(PlanKey::Name("arguments".to_owned()), AssignPlan::Keep);
            blocks.insert(PlanKey::Index(*index), AssignPlan::Fields(fields));
            call.changes.clear();
        }
        let mut content_fields: PlanFields = PlanFields::new();
        content_fields.insert(
            PlanKey::Name("content".to_owned()),
            AssignPlan::Fields(blocks),
        );
        PartialCommit {
            plan: AssignPlan::Fields(content_fields),
            tool_calls,
        }
    }

    /// 把 `commit` 取走的变化写进 `generation.message`：先按计划逐叶写消息，再应用参数变更。
    ///
    /// 这里新出现的被跟踪调用从它的变更得到参数（变更从第一段开始）；当已提交的消息丢掉了某个
    /// 变更已被提交的调用（另一个写者重置了它），该调用从今往后回落到快照。
    ///
    /// 执行顺序：
    /// 1. 读提交前 `generation.message.content` 的长度 `committed_blocks`（缺失按 0）；它表示
    ///    本次提交前消息里已经存在的块数。
    /// 2. 逐个登记要应用的参数变更。跟踪项已被删除的调用跳过；当
    ///    `index >= committed_blocks && call.committed` 且本地消息该块仍是 `toolCall` 时，
    ///    说明这个调用早先经过 `commit`、但已提交消息里的块不见了（被别的写者重置，或那次
    ///    提交最终失败）：把 `taken.snapshot` 写回本地 `message` 并停止跟踪，不再重放增量
    ///    （该下标没有已提交块，第 3 步会把这个块整块 push 进去）。其余情况标记
    ///    `committed = true` 并排队应用。
    /// 3. `assign_json` 按 `commit.plan` 合并消息，然后把排队的变更逐条应用到
    ///    `generation.message` 里对应的 `toolCall` 块；块缺失或类型不符时跳过。
    ///
    /// 为什么第 2 步要区分“新调用”和“重置”：新调用第一次提交时 `committed == false`，它的
    /// `changes` 从解析器的根对象 `Set` 开始，可以直接重放；处理过的调用只持有增量，
    /// 重放到被重置的空块上会得到残缺参数，所以退回完整快照。
    ///
    /// TS: `commit`。
    pub fn commit(&mut self, generation: &mut Value, message: &mut Value, commit: &PartialCommit) {
        let committed_blocks = generation
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        let mut apply: Vec<(usize, Vec<JsonChange>)> = Vec::new();
        for (index, taken) in &commit.tool_calls {
            let Some(call) = self.tool_call(*index) else {
                continue;
            };
            let block_is_call = message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.get(*index))
                .and_then(|block| block.get("type"))
                .and_then(Value::as_str)
                == Some("toolCall");
            if *index >= committed_blocks && call.committed && block_is_call {
                // 早先的变更不在已提交消息里：提交快照，并停止跟踪该调用。
                self.tool_calls.retain(|(slot, _)| *slot != *index);
                if let Some(block) = message
                    .get_mut("content")
                    .and_then(Value::as_array_mut)
                    .and_then(|content| content.get_mut(*index))
                    .and_then(Value::as_object_mut)
                {
                    block.insert("arguments".to_owned(), taken.snapshot.clone());
                }
                continue;
            }
            call.committed = true;
            apply.push((*index, taken.changes.clone()));
        }
        // 先按计划合并消息：文本走 `Append`，已存在块的参数保持原值（`Keep`）。
        assign_json(
            generation,
            TargetKey::Name("message"),
            message,
            Some(&commit.plan),
        );
        // 再把排队的增量逐条重放到目标块；顺序与解析器产出顺序一致。
        for (index, changes) in apply {
            let Some(block) = generation
                .get_mut("message")
                .and_then(|message| message.get_mut("content"))
                .and_then(Value::as_array_mut)
                .and_then(|content| content.get_mut(index))
            else {
                continue;
            };
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            for change in &changes {
                apply_change(block, change);
            }
        }
    }

    /// 放回一次失败提交取走的东西，让下次提交先应用它（TS: `restore`）。
    ///
    /// 只恢复工具调用参数变更（`commit.tool_calls`）：取走的变更按原顺序插到该调用当前已积累
    /// 的变更之前，即使 `take` 之后又观察到了新 delta，全局顺序仍然正确。对已被删除的跟踪项
    /// （例如期间收到了 `ToolCallEnd`）是空操作。同一个 `commit` 不要重复恢复，否则变更会被
    /// 应用两次。
    ///
    /// 文本增量不在恢复范围内：`take` 已清空待提交文本，但 `message` 始终携带完整文本，
    /// 下一次 `assign_json` 会退回整体写入，值不会丢（只是那一次不再呈现 append 形状）。
    ///
    /// TS: `restore`。
    pub fn restore(&mut self, commit: &PartialCommit) {
        for (index, taken) in &commit.tool_calls {
            if let Some(call) = self.tool_call(*index) {
                let mut changes = taken.changes.clone();
                changes.append(&mut call.changes);
                call.changes = changes;
            }
        }
    }
}

// 空路径表示变更作用于参数根值：根容器开始时上报一条 `Set { value: {} }`；根值不是对象
// （数组 / 标量 / null）或根上是 `Append` 时，调用方放弃增量跟踪（见 `observe`）。
fn path_is_empty(change: &JsonChange) -> bool {
    match change {
        JsonChange::Set { path, .. } | JsonChange::Append { path, .. } => path.is_empty(),
    }
}

fn change_path(change: &JsonChange) -> &JsonPath {
    match change {
        JsonChange::Set { path, .. } | JsonChange::Append { path, .. } => path,
    }
}

/// 把一个参数变更应用到已提交调用的参数上（TS: `applyChange`）。
///
/// `path` 的最后一段是目标槽位，前面的段是容器路径。变更由同一个解析器按发生顺序产出，
/// 回放时路径上的容器应当已经存在；任一段缺失就直接返回（防御性跳过，避免 panic）。
///
/// 目标容器是数组时：下标在范围内替换、等于长度时 push，大于长度时忽略；对象按键写入。
/// `Append` 只对字符串有意义（对象成员或数组元素取当前字符串拼接）；父值不是容器时只接受
/// 整体赋值的 `Set`。
///
/// 数组父容器上的 `Key` / 对象父容器上的 `Index` 是防御分支：解析器的路径段与容器类型一一
/// 对应（对象成员给 `Key`、数组元素给 `Index`），正常输入不会走到。
fn apply_change(block: &mut Value, change: &JsonChange) {
    let Some(block) = block.as_object_mut() else {
        return;
    };
    let path = change_path(change);
    let Some((key, parents)) = path.split_last() else {
        // 只有对象根会被跟踪（`observe()` 丢弃其它根），且只有 `set` 会开始跟踪。
        if let JsonChange::Set { value, .. } = change
            && is_record(Some(value))
        {
            block.insert("arguments".to_owned(), value.clone());
        }
        return;
    };
    // 被跟踪调用的 `arguments` 由 `take` 保证存在（消息块里被置为 `{}`）；缺失时跳过。
    let Some(arguments) = block.get_mut("arguments") else {
        return;
    };
    // 逐段下钻到目标槽位的父容器；变更按顺序回放，正常每一步都已存在。
    let mut parent = arguments;
    for segment in parents {
        let next = match segment {
            PathSegment::Key(name) => parent.get_mut(name),
            PathSegment::Index(index) => parent.get_mut(*index),
        };
        let Some(next) = next else {
            return;
        };
        parent = next;
    }
    // 非容器目标：整体赋值（`Set`）或忽略（`Append` 只接受对象成员）。
    if !matches!(parent, Value::Array(_) | Value::Object(_)) {
        if let JsonChange::Set { value, .. } = change {
            *parent = value.clone();
        }
        return;
    }
    // 目标槽位按父容器类型写入：`Set` 是整体赋值，`Append` 只拼接已有字符串。
    match (parent, change) {
        (Value::Array(items), JsonChange::Set { .. }) => {
            let target = match change {
                JsonChange::Set { value, .. } => value,
                _ => unreachable!("matched Set above"),
            };
            match key {
                PathSegment::Index(index) => set_array_item(items, *index, target.clone()),
                PathSegment::Key(name) => set_object_member(items, name, target.clone()),
            }
        }
        (Value::Array(items), JsonChange::Append { text, .. }) => {
            // 数组里的字符串是重新赋值（整串比较）；`appendText()` 只接受对象成员。
            match key {
                PathSegment::Index(index) => {
                    let current = items.get(*index).and_then(Value::as_str).unwrap_or("");
                    let appended = format!("{current}{text}");
                    set_array_item(items, *index, Value::String(appended));
                }
                PathSegment::Key(name) => {
                    let current = items
                        .iter()
                        .find_map(|item| item.get(name))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let appended = format!("{current}{text}");
                    set_object_member(items, name, Value::String(appended));
                }
            }
        }
        (Value::Object(map), JsonChange::Set { value, .. }) => match key {
            PathSegment::Key(name) => {
                map.insert(name.clone(), value.clone());
            }
            PathSegment::Index(index) => {
                map.insert(index.to_string(), value.clone());
            }
        },
        (Value::Object(map), JsonChange::Append { text, .. }) => {
            let name = match key {
                PathSegment::Key(name) => name.clone(),
                PathSegment::Index(index) => index.to_string(),
            };
            let appended = format!(
                "{}{text}",
                map.get(&name).and_then(Value::as_str).unwrap_or("")
            );
            map.insert(name, Value::String(appended));
        }
        (_, JsonChange::Append { .. }) => {}
        _ => {}
    }
}

// 下标等于长度时追加；严格大于长度时忽略——解析器按顺序产出元素，正常只会等于当前长度，
// 这里不插入占位值来模拟 JS 数组的空洞。
fn set_array_item(items: &mut Vec<Value>, index: usize, value: Value) {
    if index < items.len() {
        items[index] = value;
    } else if index == items.len() {
        items.push(value);
    }
}

// 在数组元素里找第一个含该键的对象再写入；没有命中就什么都不做（防御路径）。
fn set_object_member(items: &mut [Value], name: &str, value: Value) {
    for item in items.iter_mut() {
        if let Some(map) = item.as_object_mut()
            && map.contains_key(name)
        {
            map.insert(name.to_owned(), value);
            return;
        }
    }
}

/// 只为让 [`AssistantContentBlock`] 在本模块文档中可见（消息 JSON 的 `type` 判别来自它）。
const _: fn() -> Option<AssistantContentBlock> = || None;

#[cfg(test)]
mod tests {
    use kiana_ai::types::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, TextContent,
        TextKind, ToolCall, ToolCallKind,
    };
    use serde_json::json;

    use super::*;

    fn partial(content: Vec<AssistantContentBlock>) -> Box<AssistantMessage> {
        Box::new(AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            ..Default::default()
        })
    }

    fn text_start(index: usize, content: Vec<AssistantContentBlock>) -> AssistantMessageEvent {
        AssistantMessageEvent::TextStart {
            content_index: index,
            partial: partial(content),
        }
    }

    fn tool_start(index: usize, content: Vec<AssistantContentBlock>) -> AssistantMessageEvent {
        AssistantMessageEvent::ToolCallStart {
            content_index: index,
            partial: partial(content),
        }
    }

    #[test]
    fn collects_text_deltas_and_plans_an_append() {
        let mut changes = PartialChanges::new();
        changes.observe(&text_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "he".to_owned(),
            partial: partial(vec![]),
        });
        changes.observe(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "llo".to_owned(),
            partial: partial(vec![]),
        });
        let mut message = json!({
            "content": [{ "type": "text", "text": "hello" }]
        });
        let commit = changes.take(&mut message);
        let mut generation = json!({});
        changes.commit(&mut generation, &mut message, &commit);
        assert_eq!(generation["message"]["content"][0]["text"], json!("hello"));
    }

    #[test]
    fn text_end_drops_pending_text() {
        let mut changes = PartialChanges::new();
        changes.observe(&text_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "he".to_owned(),
            partial: partial(vec![]),
        });
        changes.observe(&AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "hey".to_owned(),
            partial: partial(vec![]),
        });
        let mut message = json!({ "content": [{ "type": "text", "text": "hey" }] });
        let commit = changes.take(&mut message);
        // 文本不再有 append 计划，但仍按整体值写入。
        let mut generation = json!({});
        changes.commit(&mut generation, &mut message, &commit);
        assert_eq!(generation["message"]["content"][0]["text"], json!("hey"));
    }

    #[test]
    fn tool_call_arguments_follow_the_parser_changes() {
        let mut call = ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: "c1".to_owned(),
            name: "read".to_owned(),
            arguments: Default::default(),
            ..Default::default()
        };
        let mut changes = PartialChanges::new();
        changes.observe(&tool_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: "{\"path\":\"/tmp\"}".to_owned(),
            partial: partial(vec![]),
        });
        call.arguments.insert("path".to_owned(), json!("/tmp"));
        let mut message = json!({
            "content": [{ "type": "toolCall", "id": "c1", "name": "read", "arguments": { "path": "/tmp" } }]
        });
        let commit = changes.take(&mut message);
        assert_eq!(message["content"][0]["arguments"], json!({}));
        let mut generation = json!({});
        changes.commit(&mut generation, &mut message, &commit);
        assert_eq!(
            generation["message"]["content"][0]["arguments"],
            json!({ "path": "/tmp" })
        );
    }

    #[test]
    fn a_non_object_argument_root_stops_tracking() {
        let mut changes = PartialChanges::new();
        changes.observe(&tool_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: "[1,".to_owned(),
            partial: partial(vec![]),
        });
        let mut message = json!({
            "content": [{ "type": "toolCall", "id": "c1", "name": "read", "arguments": { "x": 1 } }]
        });
        let commit = changes.take(&mut message);
        assert!(commit.tool_calls.is_empty());
    }

    #[test]
    fn a_text_block_is_not_tracked_as_a_tool_call() {
        let mut changes = PartialChanges::new();
        changes.observe(&tool_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: "{\"a\":".to_owned(),
            partial: partial(vec![]),
        });
        let mut message = json!({
            "content": [{ "type": "text", "text": "x" }]
        });
        let commit = changes.take(&mut message);
        assert!(commit.tool_calls.is_empty());
    }

    #[test]
    fn restore_puts_taken_changes_back_in_front() {
        let mut changes = PartialChanges::new();
        changes.observe(&tool_start(0, vec![]));
        changes.observe(&AssistantMessageEvent::ToolCallDelta {
            content_index: 0,
            delta: "{\"a\":1".to_owned(),
            partial: partial(vec![]),
        });
        let mut message = json!({
            "content": [{ "type": "toolCall", "id": "c1", "name": "read", "arguments": { "a": 1 } }]
        });
        let commit = changes.take(&mut message);
        assert!(!commit.tool_calls.is_empty());
        changes.restore(&commit);
        let call = changes.tool_call(0).expect("still tracked");
        assert!(!call.changes.is_empty());
    }

    #[test]
    fn text_content_block_type_is_stable() {
        // 消息 JSON 的 `type` 判别来自 `AssistantContentBlock` 的 untagged 序列化。
        let block = AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "hi".to_owned(),
            text_signature: None,
        });
        let value = serde_json::to_value(block).expect("serializes");
        assert_eq!(value["type"], json!("text"));
    }
}
