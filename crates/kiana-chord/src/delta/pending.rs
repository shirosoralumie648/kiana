//! `delta/pending.ts`：慢消费者的待处理 op 队列与折叠。
//!
//! 状态机：`empty → batch → folding → replace`；`r` 丢弃此前的一切；
//! 叶 op（`a` 与原始值 `s`）按路径合并；结构 op 清空叶索引；超过 `MAX_PENDING_OPS`
//! 后只保留"用当前值替换"。
//!
//! ## 设计原因
//!
//! watch 等消费者如果暂时没有运行，提交产生的 op 会不断堆积。逐批保存会让内存随提交数
//! 线性增长，而消费者最终会把它们依次应用——同一路径上的叶 op 提前合并不会改变结果。
//! `PendingOps` 因此只保留“折叠后仍不可省”的 op；超过 [`MAX_PENDING_OPS`] 时干脆放弃
//! 折叠，交由 owner 用当前值发一次根替换。第一批按原样保存，让跟得上的消费者不付折叠成本
//! （TS 按引用保存该批次；Rust 的 `push` 接收借用切片，必须 `to_vec()` 克隆）。
//!
//! ## 状态机
//!
//! 下表的每一行都对应 `push`/`take` 里的一个分支：
//!
//! | 状态 | 含义 | 允许的转换 |
//! | --- | --- | --- |
//! | `Empty` | 没有 pending | `push` 非空批次（含 `r`）→ `Batch` |
//! | `Batch` | 只收到一个批次，原样保存 | `push` 非 `r` 批次 → `Folding`（未溢出）或 `Replace`；`push` 以 `r` 开头的批次 → `Batch`（丢弃旧批次）；`take` → `Empty` |
//! | `Folding` | 已折叠过至少两批，`leaves` 记录每个路径最后一个叶 op | `push` 折叠成功 → `Folding`，溢出 → `Replace`；`push` 以 `r` 开头的批次 → `Batch`；`take` → `Empty` |
//! | `Replace` | 已溢出，只保留“用当前值替换” | `push` 非 `r` 批次 → 保持 `Replace`；`push` 以 `r` 开头的批次 → `Batch`；`take` → `Empty` |
//!
//! `push` 的 `r` 特判先于状态分支：以 `r` 开头的批次会丢弃此前一切（包括溢出状态）并
//! **原样**保存该批次。这是唯一能把 `Replace` 救回 `Batch` 的路径。
//!
//! ## 折叠规则与不变量
//!
//! - 叶 op 只有两种：`a` 与值不是对象/数组的 `s`。同路径的叶 op 合并：`a`+`a` 拼接文本，
//!   `a` 接在字符串 `s` 后面，`s` 覆盖同路径的先前叶 op（见 `merge`）。
//! - 任何结构 op（`d`/`t`/`p`/`m`，以及值为容器的 `s`）都会清空叶索引：它改变父容器或
//!   位置关系，之后再按旧路径合并会写错位置。
//! - 合并会**替换旧 op 在 `ops` 中的位置**，因此 `take()` 交出的顺序不保证等于提交顺序；
//!   测试 `tests/delta_pending.rs` 用 `apply_immutable` 对比原始批次与折叠批次，锁定
//!   “回放结果相同”。调用方不应依赖 `take()` 结果的排列顺序。
//! - 入参 `&[Op]` 从不被修改；合并产生新的 `Op`。
//! - 容量上限只在**折叠**时检查：第一次 `push` 的单个批次无论多大都原样保存，第二条
//!   批次触发折叠时才可能溢出。
//!
//! ## 消费方式
//!
//! [`PendingOps::take`] 返回 [`PendingTake::Ops`] 或 [`PendingTake::Replace`] 并把队列重置
//! 为空。`Replace` **不携带值**：调用方（例如 kiana-durable 的 session/transaction/watch）
//! 必须用自己持有的当前值构造一条 `Op::Replace`。队列内部没有锁，共享时由调用方加锁
//! （kiana-durable 用 `Mutex<PendingOps>` 串行化 `push`/`take`）。

use std::collections::HashMap;

use serde_json::Value;

use crate::delta::index::path_to_json;
use crate::delta::{Op, Seg};

/// 超过这个数量的 pending op 只保留 replace（TS: `MAX_PENDING_OPS`）。
///
/// 判定是**严格大于**：折叠后恰好 10_000 条仍保留，10_001 条才溢出。该上限只在折叠路径
/// 检查，第一次入队的单个批次无论多大都按原样保存。
pub const MAX_PENDING_OPS: usize = 10_000;

/// `take()` 的结果：折叠后的 op，或"用当前值替换"（owner 自建 `[["r", value]]`）。
///
/// 变体不携带值是有意的：队列只保存 op、不持有状态值；替换值由调用方的当前快照决定。
#[derive(Debug, Clone, PartialEq)]
pub enum PendingTake {
    /// 待投递的 op 批次。
    Ops(Vec<Op>),
    /// 溢出：owner 用当前值构造根替换。
    Replace,
}

/// 折叠中的批次。
///
/// `ops` 与 `leaves` 必须同步维护：`leaves` 里的每个位置都必须指向 `ops` 中仍有效的叶
/// op；清空 `ops`（`Replace`）时必须同时清空 `leaves`，清空 `leaves` 则让旧索引全部作废。
#[derive(Debug, Default)]
struct Folding {
    ops: Vec<Op>,
    /// 路径键 → 该路径上最后一个叶 op 在 `ops` 里的位置。
    leaves: HashMap<String, usize>,
}

// 队列状态。`#[default]` 让 `std::mem::take` 之后的队列自动回到 `Empty`（`take` 依赖这一点）。
#[derive(Debug, Default)]
enum State {
    #[default]
    Empty,
    Batch(Vec<Op>),
    Folding(Folding),
    Replace,
}

/// TS `PendingOps`。
///
/// 每个待投递的消费者一个实例；`push`/`take` 都是 `&mut self`，内部没有锁，共享时由
/// 调用方加锁（kiana-durable 用 `Mutex<PendingOps>`）。
#[derive(Debug, Default)]
pub struct PendingOps {
    state: State,
}

impl PendingOps {
    /// 新建空队列。
    pub fn new() -> Self {
        PendingOps::default()
    }

    /// 入队一个提交的 op。
    ///
    /// - `ops` 为空时不改变状态。
    /// - 以 `Replace` 开头的批次丢弃此前所有 pending（包括溢出状态），并原样保存。
    /// - 其余情况：`Empty` 原样保存；`Batch`/`Folding` 触发折叠，成功则进入/保持
    ///   `Folding`，超过 [`MAX_PENDING_OPS`] 则进入 `Replace`（折叠中间结果被丢弃）；
    ///   `Replace` 不再累积。
    ///
    /// 入参不会被修改；合并产生新的 op。
    pub fn push(&mut self, ops: &[Op]) {
        if ops.is_empty() {
            return;
        }
        // 以 `r` 开头的批次丢弃一切 pending（包括溢出），并原样保留。
        if matches!(ops.first(), Some(Op::Replace(_))) {
            self.state = State::Batch(ops.to_vec());
            return;
        }
        match std::mem::take(&mut self.state) {
            State::Empty => {
                self.state = State::Batch(ops.to_vec());
            }
            State::Batch(previous) => {
                // `&&` 短路：上一批在重新折叠时就溢出的情况下，本批不再处理，直接 Replace。
                let mut folding = Folding::default();
                let folded = fold(&mut folding, &previous) && fold(&mut folding, ops);
                if folded {
                    self.state = State::Folding(folding);
                } else {
                    self.state = State::Replace;
                }
            }
            State::Folding(mut folding) => {
                if fold(&mut folding, ops) {
                    self.state = State::Folding(folding);
                } else {
                    self.state = State::Replace;
                }
            }
            State::Replace => {
                self.state = State::Replace;
            }
        }
    }

    /// 折叠后的 pending 数（溢出时为 1）。
    ///
    /// `Batch`/`Folding` 返回其 `ops.len()`；`Empty` 返回 0；`Replace` 恒为 1（它只代表
    /// 一次根替换）。这是逻辑条数，不是字节数；单个超大但尚未折叠的批次按原样计数。
    pub fn size(&self) -> usize {
        match &self.state {
            State::Empty => 0,
            State::Replace => 1,
            State::Batch(ops) => ops.len(),
            State::Folding(folding) => folding.ops.len(),
        }
    }

    /// 交出全部 pending 并回到空状态。
    ///
    /// `Empty` 返回空 `Vec`；`Batch`/`Folding` 返回折叠后的 op；`Replace` 返回
    /// [`PendingTake::Replace`]，**不带值**——调用方用自己持有的当前值构造根替换。
    /// 返回后队列可继续使用。
    pub fn take(&mut self) -> PendingTake {
        match std::mem::take(&mut self.state) {
            State::Empty => PendingTake::Ops(Vec::new()),
            State::Batch(ops) | State::Folding(Folding { ops, .. }) => PendingTake::Ops(ops),
            State::Replace => PendingTake::Replace,
        }
    }
}

/// 把 `ops` 折叠进 `folding`；返回 false 表示溢出。
///
/// 逐条处理（顺序不能变）：
/// 1. `Replace`：清空 `ops` 与叶索引，但仍把这条 `r` push 进去（`r` 之后的 op 继续累积）；
/// 2. `Append`：尝试合并进同路径叶 op，成功则不 push；
/// 3. 原始值 `Set`：同样尝试合并；容器值 `Set` 清空叶索引；
/// 4. 其它结构 op（`d`/`t`/`p`/`m`）清空叶索引。
///
/// 每次 push 后检查 `> MAX_PENDING_OPS`，超限立即返回 false；调用方随后丢弃整个
/// `folding` 并进入 `Replace`，部分折叠结果不会泄漏出去。
fn fold(folding: &mut Folding, ops: &[Op]) -> bool {
    for op in ops {
        match op {
            Op::Replace(_) => {
                folding.ops.clear();
                folding.leaves.clear();
            }
            Op::Append(..) => {
                if merge_leaf(folding, op) {
                    continue;
                }
            }
            Op::Set(_, value) => {
                if is_primitive(value) {
                    if merge_leaf(folding, op) {
                        continue;
                    }
                } else if !folding.leaves.is_empty() {
                    folding.leaves.clear();
                }
            }
            _ => {
                if !folding.leaves.is_empty() {
                    folding.leaves.clear();
                }
            }
        }
        folding.ops.push(op.clone());
        if folding.ops.len() > MAX_PENDING_OPS {
            return false;
        }
    }
    true
}

fn is_primitive(value: &Value) -> bool {
    !value.is_object() && !value.is_array()
}

/// 把叶 op 合并到同路径的上一个叶 op；成功则返回 true（调用方随后不再 push）。
///
/// 命中索引但 `merge` 拒绝合并时，索引仍会更新为新 op 即将 push 到的位置——新 op 成为
/// 该路径当前的叶 op。
fn merge_leaf(folding: &mut Folding, op: &Op) -> bool {
    let key = leaf_path_key(op);
    if let Some(&at) = folding.leaves.get(&key)
        && let Some(merged) = merge(&folding.ops[at], op)
    {
        folding.ops[at] = merged;
        return true;
    }
    folding.leaves.insert(key, folding.ops.len());
    false
}

/// 叶索引的键：路径转 JSON 后再序列化。
///
/// 对象键与数组下标不会混淆（`["a.b"]` 与 `["a", "b"]`、`["1"]` 与 `[1]` 都是不同的键），
/// 所以合并不会把两条不同路径的写吞掉。只在 `Set`/`Append` 上调用。
fn leaf_path_key(op: &Op) -> String {
    let path: &[Seg] = match op {
        Op::Set(path, _) | Op::Append(path, _) => path.as_slice(),
        _ => &[],
    };
    serde_json::to_string(&path_to_json(path)).unwrap_or_default()
}

/// `next` 折进 `previous`（同路径），或返回 `None` 表示不能合并。
///
/// - 新的原始值 `Set` 直接覆盖任何先前的叶 op；
/// - `Append` 可接在 `Append` 或字符串 `Set` 后面，合并成一条拼接后的文本；
/// - 其它组合（例如向数字 `Set` 追加）返回 `None`，退化为两条独立 op。
///
/// TS 的孤立代理项用例在 Rust 的 `String` 里无法表示（见 `tests/delta_pending.rs` 的注释），
/// 因此拼接结果是普通 UTF-8 字符串。
fn merge(previous: &Op, next: &Op) -> Option<Op> {
    if matches!(next, Op::Set(..)) {
        return Some(next.clone());
    }
    let Op::Append(_, next_text) = next else {
        return None;
    };
    match previous {
        Op::Append(path, text) => Some(Op::Append(path.clone(), format!("{text}{next_text}"))),
        Op::Set(path, Value::String(text)) => Some(Op::Set(
            path.clone(),
            Value::String(format!("{text}{next_text}")),
        )),
        _ => None,
    }
}
