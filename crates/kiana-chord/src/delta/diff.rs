//! `delta/diff.ts`：两版不可变 JSON 之间的紧凑操作批次。
//!
//! 逐函数对应 TS 实现（`emitString`/`diffArray`/`diffArrayRegion`/`identitySubsequence`/
//! `identityAnchors`/`lcsMatches`/`jsonCost`/…），包括 4096 条 op 的溢出退化与 64 KiB 的
//! 成本比较。
//!
//! ## 身份语义（引用身份）
//!
//! TS 的 `Map<JsonValue, …>`、`===` 对对象/数组比较的是**引用身份**：只有两版实际共享的
//! 子树才算"同一个值"；两次独立解析（或 `structuredClone`）得到的等值对象互不相同。
//! `serde_json::Value` 是树形所有权类型，不能表达共享子树，因此 Rust 侧用**指针地址**
//! 复刻引用身份：基本值按 JS 的 `===` / SameValueZero 比较（`1` 与 `1.0` 相同），容器只有
//! 地址相同才相同。跨版本的两个容器（必然来自不同的 `Value` 树）因此永不相等——这正是
//! TS 面对"独立解析的两棵对象图"（JSON 往返、线上解码、`structuredClone`）时的行为。
//! Rust 的调用方无法构造共享子树，所以这是该类型下能给出的最接近逐字的行为。
//!
//! 注意 `sameValue`（trim、逐位匹配、`processArrayMatches`）仍是 `=== || equalJson`：
//! 引用不同但结构相等的值在这里相等，与 TS 相同。
//!
//! ## 这个文件负责什么
//!
//! 输入是两棵 JSON 值树（通常来自同一文档的两个版本），输出一个 [`Op`] 批次。本模块
//! **只求差**：它不校验、也不应用操作。批次的消费方是 `delta::index` 的应用器（金标准
//! 测试用 `apply_immutable` 回放批次并断言结果等于 `after`），以及 `kiana-durable` 的
//! 事务准备（`session/transaction.rs` 的 `Change::prepare` 直接调用 [`diff_revisions`]）。
//!
//! ## 主要流程
//!
//! 1. [`diff_revisions`] 新建 `Diff` 累加器，从根调用 `diff_value` 递归求差。
//! 2. `diff_value` 先做结构深比较，再按类型分派：字符串交给 `emit_string` 做
//!    append/truncate 优化；数组走 `diff_array`；对象走 `diff_object`；其余值用
//!    `emit_set`（根上会变成 [`Op::Replace`]）。
//! 3. 数组是最复杂的部分：`diff_array` 先整体深比较；等长、至少两个元素且首尾都
//!    不同时尝试纯重排快路径（`permutation` 成双射就只发一条 `Permute`）；否则进入
//!    `diff_array_region`，先 trim 公共前后缀，再依次尝试「逐位匹配 → 保序子序列 →
//!    身份锚点（LIS）→ 语义对齐 LCS」。四策略都不命中时，单个对单个递归
//!    `diff_value`，其余情况才发整段 `Splice`。先命中的策略决定 op 形状。
//! 4. 收尾时若 op 数溢出，或批次估算成本达到 64 KiB 且不低于整快照
//!    （`deltaCost >= snapshotCost`，相等也替换），就把整个批次换成一条根替换。
//!
//! ## 上限与退化
//!
//! 下列常量与 TS 的同名常量一一对应：
//!
//! - `MAX_DELTA_OPERATIONS`（4096）：批次最多保留 4096 条 op；再来的操作被丢弃，
//!   并把批次标记为溢出，最终返回 `[Replace(after)]`。
//! - 64 KiB：批次估算成本达到 65536 后，只有严格小于快照成本才保留细粒度批次。
//! - `MAX_IDENTITY_CANDIDATES`（200000）：身份锚点候选对超过它时退化为贪心匹配。
//! - `MAX_SEMANTIC_CELLS`（65536）：语义 LCS 的 DP 单元格上限，超限跳过该策略。
//!
//! ## 注意事项
//!
//! - 数组 op 的下标是**应用后**（`after` 侧）的下标，不是 `before` 的下标；见
//!   `process_array_matches` 对 `output_start` 的推进。
//! - `Set`/`Delete`/`Append`/`Truncate` 的路径必须非空；根上的整体替换只产生
//!   [`Op::Replace`]。
//! - 对象键的遍历顺序是 `Object.keys` 语义（整数索引键升序在前，其余键按插入序），
//!   依赖 workspace 为 `serde_json` 启用的 `preserve_order` 特性；op 顺序依赖它。

use std::collections::HashMap;

use serde_json::Value;

use crate::delta::index::{RESERVED_SEGMENTS, overlap, owned_slice, utf16_len, utf16_slice};
use crate::delta::{Op, Seg};

/// `emit_string` 的重叠扫描范围：只看 `before` 末尾这么多 UTF-16 码元。
const DEFAULT_OVERLAP_SCAN: usize = 65_536;
/// 单批次保留的 op 上限；达到后继续产生 op 会把整批退化为一条根替换。
const MAX_DELTA_OPERATIONS: usize = 4_096;
/// `identity_anchors` 的候选对上限；超过后退化为贪心匹配。
const MAX_IDENTITY_CANDIDATES: usize = 200_000;
/// 语义 LCS 的 DP 单元格上限；超限直接跳过该策略。
const MAX_SEMANTIC_CELLS: usize = 65_536;

/// 一个批次内的 diff 状态：op 列表 + 溢出标记（TS 的 `WeakSet<Op[]>`）。
///
/// 溢出标记只属于当前批次：一旦置位，`emit` 丢弃后续所有操作，[`diff_revisions`]
/// 看到它就把半成品换成一条根替换。因此每次调用都必须新建一个 `Diff`，不能跨批次复用。
struct Diff<'a> {
    operations: &'a mut Vec<Op>,
    overflowed: bool,
}

impl<'a> Diff<'a> {
    fn new(operations: &'a mut Vec<Op>) -> Self {
        Diff {
            operations,
            overflowed: false,
        }
    }

    /// 记录一条 op；已有 4096 条时置溢出并拒绝写入，之后的所有 emit 都是空操作。
    fn emit(&mut self, operation: Op) {
        if self.overflowed {
            return;
        }
        if self.operations.len() >= MAX_DELTA_OPERATIONS {
            self.overflowed = true;
            return;
        }
        self.operations.push(operation);
    }

    /// 把 `value` 写回路径；空路径表示根，只能发 [`Op::Replace`]（`Set` 的路径必须
    /// 非空，见 `delta::index` 的 `assert_valid_op`）。
    fn emit_set(&mut self, path: &[Seg], value: &Value) {
        if path.is_empty() {
            self.emit(Op::Replace(value.clone()));
        } else {
            self.emit(Op::Set(path.to_vec(), value.clone()));
        }
    }
}

fn is_container(value: &Value) -> bool {
    value.is_object() || value.is_array()
}

/// 结构深相等（TS 的 `equalJson`）：数值按 JS 的 f64 语义比较，对象按键集合与值递归
/// 比较，不要求引用身份。
///
/// TS 的 `sameValue` 是 `=== || equalJson`；对无环 JSON 值，`===` 成立时 `equalJson`
/// 必然也成立，所以这里只调用 `deep_equal` 就覆盖了两种情况。
fn same_value(left: &Value, right: &Value) -> bool {
    kiana_json::js_object::deep_equal(left, right)
}

/// `Object.keys` 顺序：整数索引键升序在前，其余键保持插入序。
///
/// `serde_json` 在 workspace 中启用了 `preserve_order`，所以 `map.keys()` 给出的就是
/// 插入序；本函数只把形如 JS 数组索引的键抽出来按数值重排。不能对全部键排序：那会把
/// `"10"` 排到 `"2"` 前面，与 JS 的遍历顺序不同。`diff_object` 按这个顺序发射 op，
/// 因此顺序是稳定的输出契约（改动需同步测试）。
fn js_keys(map: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut indices: Vec<(u32, String)> = Vec::new();
    let mut others: Vec<String> = Vec::new();
    for key in map.keys() {
        match array_index(key) {
            Some(index) => indices.push((index, key.clone())),
            None => others.push(key.clone()),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    let mut out: Vec<String> = indices.into_iter().map(|(_, key)| key).collect();
    out.extend(others);
    out
}

/// JS 的数组索引键：`0`…`4294967294`，无前导零。
///
/// `4294967295`（`u32::MAX`）与超过 10 位的数字都不是数组索引，在 JS 里按普通字符串
/// 键排在非索引键区。这个判定决定 `js_keys` 的分组，不能放宽。
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    if value == u32::MAX { None } else { Some(value) }
}

/// 身份键：数字按 JS 值（`1 === 1.0`），容器按**指针地址**（JS 的引用身份）。
///
/// 两个 `Value` 来自不同的树时地址必然不同，因此容器永不匹配；这与 TS 对两棵独立
/// 解析的对象图运行 `permutation`/`identityAnchors` 的结果一致。同一 `Value` 节点
/// （只可能出现在 `before` 与 `after` 是同一棵树的情况）地址相同，因此按引用身份匹配。
///
/// 各分支的前缀（`null`、`bool:`、`num:`、`str:`、`ptr:`）互不重叠，字符串内容不会
/// 与其它类型撞键。数字统一经 `format_js_number` 格式化，`1` 与 `1.0`、`-0` 与 `0`
/// 都得到同一个键（与 JS 的数字语义一致）。
fn identity_key(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => format!("bool:{value}"),
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            format!("num:{}", kiana_json::js_num::format_js_number(number))
        }
        Value::String(text) => format!("str:{text}"),
        Value::Array(_) | Value::Object(_) => format!("ptr:{:p}", std::ptr::from_ref(value)),
    }
}

/// JS 严格相等（`===` / SameValueZero）：基本值按值，容器按引用身份。
///
/// 供 `identity_subsequence` 做双指针配对：基本值可以跨树匹配，容器必须地址相同。
fn identical(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Array(_) | Value::Object(_), Value::Array(_) | Value::Object(_)) => {
            std::ptr::eq(left, right)
        }
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => false,
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        // JS 里所有数字都是 f64：`1 === 1.0`，超出 2^53 的整数按最近的可表示值比较。
        (Value::Number(left), Value::Number(right)) => {
            left.as_f64().unwrap_or(f64::NAN) == right.as_f64().unwrap_or(f64::NAN)
        }
        (Value::String(left), Value::String(right)) => left == right,
        _ => false,
    }
}

/// `permutation(before, after)`：存在双射则返回 `new[i] = old[permutation[i]]`。
///
/// 身份相同的元素按出现顺序一一配对：`positions` 保存 before 中每个身份键的全部
/// 下标，`used` 游标保证重复元素不会被重复使用。长度不等、或某个 after 元素找不到
/// 未用候选时返回 `None`，调用方据此决定是否发 `Permute`。
fn permutation(before: &[Value], after: &[Value]) -> Option<Vec<usize>> {
    if before.len() != after.len() {
        return None;
    }
    let mut positions: HashMap<String, (Vec<usize>, usize)> = HashMap::new();
    for (index, value) in before.iter().enumerate() {
        positions
            .entry(identity_key(value))
            .or_insert_with(|| (Vec::new(), 0))
            .0
            .push(index);
    }
    let mut result = vec![0usize; after.len()];
    for (index, value) in after.iter().enumerate() {
        let entry = positions.get_mut(&identity_key(value))?;
        if entry.1 == entry.0.len() {
            return None;
        }
        result[index] = entry.0[entry.1];
        entry.1 += 1;
    }
    Some(result)
}

/// `emitString`：append 前缀优先，否则滚动窗口（truncate + append）或 set。
///
/// 只在路径非空时调用（`diff_value` 的分派条件），因为 `Append`/`Truncate` 不能指向
/// 根。长度与切片都按 UTF-16 码元计算（JS 的 `string.length`）；重叠扫描最多看
/// `before` 末尾 65536 个码元。append 前缀命中时只发一条 `Append`；走重叠路径时最多
/// 发两条 op（`Truncate` 加可选的 `Append`）；`overlap` 返回 0（没有可用重叠，或
/// 候选数超过上限而放弃）时只发一条 `Set`。
fn emit_string(before: &str, after: &str, path: &[Seg], diff: &mut Diff<'_>) {
    if before == after {
        return;
    }
    let before_len = utf16_len(before);
    let after_len = utf16_len(after);
    if after_len > before_len && utf16_slice(after, 0, Some(before_len)) == before {
        diff.emit(Op::Append(
            path.to_vec(),
            owned_slice(after, before_len, None),
        ));
        return;
    }
    let shared = overlap(before, after, DEFAULT_OVERLAP_SCAN);
    if shared == 0 {
        diff.emit(Op::Set(path.to_vec(), Value::String(after.to_owned())));
        return;
    }
    diff.emit(Op::Truncate(path.to_vec(), before_len - shared));
    if after_len > shared {
        diff.emit(Op::Append(path.to_vec(), owned_slice(after, shared, None)));
    }
}

/// `semanticallyAligned`：深相等，或某一侧每个键下的**同一个容器**（引用身份）在另一侧。
/// 跨版本的两棵树没有共享容器，因此该函数在 Rust 里等价于 `same_value`。
fn semantically_aligned(left: &Value, right: &Value) -> bool {
    if same_value(left, right) {
        return true;
    }
    if !is_container(left) || !is_container(right) || left.is_array() != right.is_array() {
        return false;
    }
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return false;
            }
            left.iter()
                .zip(right.iter())
                .any(|(value, other)| is_container(value) && identical(value, other))
        }
        (Value::Object(left), Value::Object(right)) => {
            for key in js_keys(left) {
                let value = &left[&key];
                if is_container(value)
                    && right.get(&key).is_some_and(|other| identical(value, other))
                {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// 返回升序切片中第一个 `>= value` 的下标（找不到时为 `values.len()`）。
fn lower_bound(values: &[usize], value: usize) -> usize {
    let mut low = 0usize;
    let mut high = values.len();
    while low < high {
        let middle = (low + high) / 2;
        if values[middle] < value {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// `identitySubsequence`：一侧更短时，按引用身份（`identical`）找保序子序列。
///
/// 用双指针在较长一侧扫描较短一侧的每个元素：只要求匹配下标的相对顺序不变，不要求
/// 连续。任一元素找不到匹配就返回 `None`，调用方继续尝试后续策略。两侧等长时直接
/// 返回 `None`（等长重排交给 `identity_anchors`）。返回的是两侧原数组的绝对下标。
// 下标算术逐行对应 TS；用 enumerate 会打乱双指针的推进顺序。
#[allow(clippy::needless_range_loop)]
fn identity_subsequence(
    before: &[Value],
    before_start: usize,
    before_end: usize,
    after: &[Value],
    after_start: usize,
    after_end: usize,
) -> Option<Vec<(usize, usize)>> {
    let before_count = before_end - before_start;
    let after_count = after_end - after_start;
    let mut matches = Vec::new();
    if after_count < before_count {
        let mut before_index = before_start;
        for after_index in after_start..after_end {
            while before_index < before_end
                && !identical(&before[before_index], &after[after_index])
            {
                before_index += 1;
            }
            if before_index == before_end {
                return None;
            }
            matches.push((before_index, after_index));
            before_index += 1;
        }
        return Some(matches);
    }
    if before_count < after_count {
        let mut after_index = after_start;
        for before_index in before_start..before_end {
            while after_index < after_end && !identical(&before[before_index], &after[after_index])
            {
                after_index += 1;
            }
            if after_index == after_end {
                return None;
            }
            matches.push((before_index, after_index));
            after_index += 1;
        }
        return Some(matches);
    }
    None
}

/// `identity_anchors` 的贪心退化版：候选对超过 `MAX_IDENTITY_CANDIDATES` 时使用。
///
/// 按 `after` 下标递增扫描，每个元素在 `before` 中取「第一个大于上一个匹配下标」的
/// 候选；找不到就跳过。结果保序，但不是最长递增子序列，可能比 LIS 少匹配一些锚点。
#[allow(clippy::needless_range_loop)]
fn greedy_identity_anchors(
    positions: &HashMap<String, Vec<usize>>,
    after: &[Value],
    after_start: usize,
    after_end: usize,
) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut previous: Option<usize> = None;
    for after_index in after_start..after_end {
        let Some(candidates) = positions.get(&identity_key(&after[after_index])) else {
            continue;
        };
        let at = lower_bound(candidates, previous.map_or(0, |index| index + 1));
        let Some(&before_index) = candidates.get(at) else {
            continue;
        };
        matches.push((before_index, after_index));
        previous = Some(before_index);
    }
    matches
}

/// `identityAnchors`：按身份匹配的 LIS；候选超过 200k 时退化为贪心。
///
/// 先用 `positions` 把 before 中每个身份键映射到它的全部下标；再统计 after 各元素
/// 能形成的候选对总数，超过 `MAX_IDENTITY_CANDIDATES`（200000）就改用
/// `greedy_identity_anchors`，避免构造过大的候选表。否则用 patience sorting 求最长
/// 递增子序列：`tails[j]`/`tail_values[j]` 记录长度为 j+1 的递增链末尾候选；候选按
/// after 下标递增、before 候选下标递减的顺序枚举，`previous` 记录链上前驱，最后从
/// `tails` 末端回溯。返回的匹配对是绝对下标，按 before 顺序排列。
#[allow(clippy::needless_range_loop)]
fn identity_anchors(
    before: &[Value],
    before_start: usize,
    before_end: usize,
    after: &[Value],
    after_start: usize,
    after_end: usize,
) -> Vec<(usize, usize)> {
    // 第一步：把 before 侧每个身份键映射到它的全部下标。
    let mut positions: HashMap<String, Vec<usize>> = HashMap::new();
    for index in before_start..before_end {
        positions
            .entry(identity_key(&before[index]))
            .or_default()
            .push(index);
    }
    // 先累计候选数，超限就直接走贪心，避免构造 O(候选数) 的候选表。
    let mut candidate_count = 0usize;
    for index in after_start..after_end {
        candidate_count += positions
            .get(&identity_key(&after[index]))
            .map_or(0, Vec::len);
        if candidate_count > MAX_IDENTITY_CANDIDATES {
            return greedy_identity_anchors(&positions, after, after_start, after_end);
        }
    }
    if candidate_count == 0 {
        return Vec::new();
    }
    // patience sorting：after 递增扫描，before 候选从大到小尝试；lower_bound 找到
    // 该候选能接在哪个长度之后，previous 记录前驱，最后从 tails 末端回溯。
    struct Candidate {
        before: usize,
        after: usize,
        previous: Option<usize>,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut tails: Vec<usize> = Vec::new();
    let mut tail_values: Vec<usize> = Vec::new();
    for after_index in after_start..after_end {
        let Some(before_positions) = positions.get(&identity_key(&after[after_index])) else {
            continue;
        };
        for &before_index in before_positions.iter().rev() {
            let at = lower_bound(&tail_values, before_index);
            let candidate_index = candidates.len();
            candidates.push(Candidate {
                before: before_index,
                after: after_index,
                previous: if at == 0 { None } else { Some(tails[at - 1]) },
            });
            if at == tails.len() {
                tails.push(candidate_index);
                tail_values.push(before_index);
            } else {
                tails[at] = candidate_index;
                tail_values[at] = before_index;
            }
        }
    }
    let mut matches = Vec::new();
    let mut candidate_index = tails.last().copied();
    while let Some(index) = candidate_index {
        let candidate = &candidates[index];
        matches.push((candidate.before, candidate.after));
        candidate_index = candidate.previous;
    }
    matches.reverse();
    matches
}

/// `lcsMatches`：`maxCells` 内做 DP；超限返回 `None`。
///
/// 表大小是 `(before.len()+1) * (after.len()+1)`，单元格存 `u32` 长度；`checked_mul`
/// 让乘积溢出时也返回 `None`（不可能分配的表）。空输入返回 `Some(空)`；超限返回
/// `None`，调用方据此跳过 LCS 策略。回溯阶段的平局规则是「长度相等时先跳过 before
/// 元素」，与 TS 的 `>=` 分支一致，决定等长 LCS 中具体保留哪些匹配。
fn lcs_matches(
    before: &[Value],
    after: &[Value],
    equal: fn(&Value, &Value) -> bool,
    max_cells: usize,
) -> Option<Vec<(usize, usize)>> {
    if before.is_empty() || after.is_empty() {
        return Some(Vec::new());
    }
    if before
        .len()
        .checked_mul(after.len())
        .is_none_or(|cells| cells > max_cells)
    {
        return None;
    }
    // 从右下往左上填表：lengths[left][right] 是 before[left..] 与 after[right..] 的
    // LCS 长度。
    let width = after.len() + 1;
    let mut lengths = vec![0u32; (before.len() + 1) * width];
    for left in (0..before.len()).rev() {
        for right in (0..after.len()).rev() {
            let at = left * width + right;
            lengths[at] = if equal(&before[left], &after[right]) {
                lengths[(left + 1) * width + right + 1] + 1
            } else {
                lengths[(left + 1) * width + right].max(lengths[left * width + right + 1])
            };
        }
    }
    // 正向回溯：相等且长度恰好下降一格就采纳该对，否则走向较长的一侧。
    let mut matches = Vec::new();
    let mut left = 0usize;
    let mut right = 0usize;
    while left < before.len() && right < after.len() {
        if equal(&before[left], &after[right])
            && lengths[left * width + right] == lengths[(left + 1) * width + right + 1] + 1
        {
            matches.push((left, right));
            left += 1;
            right += 1;
        } else if lengths[(left + 1) * width + right] >= lengths[left * width + right + 1] {
            left += 1;
        } else {
            right += 1;
        }
    }
    Some(matches)
}

/// 用一组锚点把两个区间切开：锚点之间递归 `diff_array_region`，锚点对本身若不是深
/// 相等则继续 `diff_value`，因此叶子修改会细化到具体路径。
///
/// `output_start` 是区间首元素在 `after` 中的下标，也是后续 op 路径使用的下标：每处
/// 理完一个间隙，`output_at` 先加上间隙在 after 侧占用的元素数，再为锚点本身加一。
/// 所有路径都以「应用后」的数组为准，而不是 `before` 的下标。溢出后不再发射任何 op。
#[allow(clippy::too_many_arguments)]
fn process_array_matches(
    before: &[Value],
    after: &[Value],
    path: &[Seg],
    diff: &mut Diff<'_>,
    before_start: usize,
    before_end: usize,
    after_start: usize,
    after_end: usize,
    output_start: usize,
    matches: &[(usize, usize)],
) {
    let mut before_at = before_start;
    let mut after_at = after_start;
    let mut output_at = output_start;
    for &(before_match, after_match) in matches {
        if diff.overflowed {
            return;
        }
        diff_array_region(
            before,
            after,
            path,
            diff,
            before_at,
            before_match,
            after_at,
            after_match,
            output_at,
        );
        output_at += after_match - after_at;
        if !same_value(&before[before_match], &after[after_match]) {
            let mut child = path.to_vec();
            child.push(Seg::Index(output_at as i64));
            diff_value(&before[before_match], &after[after_match], &child, diff);
        }
        output_at += 1;
        before_at = before_match + 1;
        after_at = after_match + 1;
    }
    if !diff.overflowed {
        diff_array_region(
            before, after, path, diff, before_at, before_end, after_at, after_end, output_at,
        );
    }
}

/// 比较两个同路径数组区间，按固定优先级依次尝试各策略，先命中的策略决定 op 形状。
///
/// 阶段（先命中者决定 op 形状，顺序不能随意调换，TS 的输出依赖同一顺序）：
///
/// 1. 用深相等 trim 公共前缀/后缀；前缀每吃掉一个元素，`output_start` 就前进一格
///    （结果数组里的位置），后缀不影响输出下标。
/// 2. 一侧为空：发一条 `Splice`（纯删除或纯插入）。
/// 3. 等长：把逐位相等的元素当锚点递归（允许多段）。
/// 4. 一侧更短：`identity_subsequence` 求保序子序列。
/// 5. `identity_anchors` 求按身份键（基本值按值、容器按地址）的最长递增匹配。
/// 6. 剩余部分在 65536 单元格内做 `semantically_aligned` 的 LCS。
/// 7. 单个对单个：递归到 `diff_value`，让叶子修改保持在窄路径上。
/// 8. 其他：整段 `Splice`。
///
/// `before_*`/`after_*` 是当前区间的绝对下标；`output_start` 是区间首元素在 `after`
/// 中的下标。
#[allow(clippy::too_many_arguments)]
fn diff_array_region(
    before: &[Value],
    after: &[Value],
    path: &[Seg],
    diff: &mut Diff<'_>,
    mut before_start: usize,
    mut before_end: usize,
    mut after_start: usize,
    mut after_end: usize,
    mut output_start: usize,
) {
    if diff.overflowed {
        return;
    }
    // 1a. 公共前缀：只有它会让结果数组的输出下标前进。
    while before_start < before_end
        && after_start < after_end
        && same_value(&before[before_start], &after[after_start])
    {
        before_start += 1;
        after_start += 1;
        output_start += 1;
    }
    // 1b. 公共后缀：只缩小待处理区间，不改变输出下标。
    while before_start < before_end
        && after_start < after_end
        && same_value(&before[before_end - 1], &after[after_end - 1])
    {
        before_end -= 1;
        after_end -= 1;
    }
    let before_count = before_end - before_start;
    let after_count = after_end - after_start;
    if before_count == 0 && after_count == 0 {
        return;
    }
    // 2. 一侧为空：一次 splice 完成剩余全部插入或删除。
    if before_count == 0 || after_count == 0 {
        diff.emit(Op::Splice {
            path: path.to_vec(),
            index: output_start,
            remove: before_count,
            items: after[after_start..after_end].to_vec(),
        });
        return;
    }
    // 3. 等长时先尝试逐位匹配：位置相同的元素可直接当锚点。
    if before_count == after_count {
        let mut positional = Vec::new();
        for offset in 0..before_count {
            if same_value(&before[before_start + offset], &after[after_start + offset]) {
                positional.push((before_start + offset, after_start + offset));
            }
        }
        if !positional.is_empty() {
            process_array_matches(
                before,
                after,
                path,
                diff,
                before_start,
                before_end,
                after_start,
                after_end,
                output_start,
                &positional,
            );
            return;
        }
    }
    // 4. 一侧更短：较短一侧整体作为保序子序列匹配。
    if let Some(subsequence) = identity_subsequence(
        before,
        before_start,
        before_end,
        after,
        after_start,
        after_end,
    ) && !subsequence.is_empty()
    {
        process_array_matches(
            before,
            after,
            path,
            diff,
            before_start,
            before_end,
            after_start,
            after_end,
            output_start,
            &subsequence,
        );
        return;
    }
    // 5. 引用身份锚点（LIS）。
    let identity = identity_anchors(
        before,
        before_start,
        before_end,
        after,
        after_start,
        after_end,
    );
    if !identity.is_empty() {
        process_array_matches(
            before,
            after,
            path,
            diff,
            before_start,
            before_end,
            after_start,
            after_end,
            output_start,
            &identity,
        );
        return;
    }
    // 6. 语义 LCS：在 65536 单元格内找结构上对齐的元素。
    let semantic = lcs_matches(
        &before[before_start..before_end],
        &after[after_start..after_end],
        semantically_aligned,
        MAX_SEMANTIC_CELLS,
    );
    if let Some(semantic) = semantic
        && !semantic.is_empty()
    {
        let absolute: Vec<(usize, usize)> = semantic
            .into_iter()
            .map(|(before_index, after_index)| {
                (before_start + before_index, after_start + after_index)
            })
            .collect();
        process_array_matches(
            before,
            after,
            path,
            diff,
            before_start,
            before_end,
            after_start,
            after_end,
            output_start,
            &absolute,
        );
        return;
    }
    // 7. 只剩一个对一个是叶子修改：递归到 diff_value，避免整段 splice。
    if before_count == 1 && after_count == 1 {
        let mut child = path.to_vec();
        child.push(Seg::Index(output_start as i64));
        diff_value(&before[before_start], &after[after_start], &child, diff);
        return;
    }
    // 8. 兜底：整段替换。
    diff.emit(Op::Splice {
        path: path.to_vec(),
        index: output_start,
        remove: before_count,
        items: after[after_start..after_end].to_vec(),
    });
}

fn equal_arrays(before: &[Value], after: &[Value]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after.iter())
            .all(|(left, right)| same_value(left, right))
}

/// 数组求差的入口：先整体深比较，再尝试「纯重排」快路径，否则交给
/// `diff_array_region`。
///
/// 纯重排要求长度相等、至少两个元素、首尾元素都变了，且 `permutation` 能给出双射；
/// 满足才发一条 `Permute`。首尾条件与 TS 相同。
///
/// `Permute` 的容器匹配按引用身份（基本值按值），因此独立解析的对象数组不会命中；
/// 之后仍走常规区域算法，未必整段退化为 `Splice`（区间内有位置相同的深相等元素时
/// 会走逐位锚点并发多条 `Set`）。测试
/// `does_not_match_containers_from_independent_json_trees` 的输入最终产生 `Splice`。
fn diff_array(before: &[Value], after: &[Value], path: &[Seg], diff: &mut Diff<'_>) {
    if equal_arrays(before, after) {
        return;
    }
    if before.len() == after.len()
        && before.len() > 1
        && !same_value(&before[0], &after[0])
        && !same_value(&before[before.len() - 1], &after[after.len() - 1])
        && let Some(order) = permutation(before, after)
    {
        diff.emit(Op::Permute(path.to_vec(), order));
        return;
    }
    diff_array_region(
        before,
        after,
        path,
        diff,
        0,
        before.len(),
        0,
        after.len(),
        0,
    );
}

/// 对象求差：按 `Object.keys` 顺序遍历 after 键（新增或递归），再遍历 before 键发
/// `Delete`。
///
/// 只要任一侧出现保留段（`__proto__`/`constructor`/`prototype`），整个对象改成一条
/// `Set`（根上为 `Replace`）：逐键 diff 会生成指向原型链的路径，而应用端会以
/// `UnsafePathError` 拒绝这类段（见 `delta::index` 的 `RESERVED_SEGMENTS` 与
/// `assert_safe_path`）。这是保守退化：宁可整段替换，也不生成应用端必然拒绝的路径。
///
/// 键顺序影响 op 顺序：after 侧的新增/修改先发，删除后发；`js_keys` 保持 JS 的
/// 「整数键升序 + 插入序」。溢出后在两个循环的每次迭代检查并停止。
fn diff_object(
    before: &serde_json::Map<String, Value>,
    after: &serde_json::Map<String, Value>,
    path: &[Seg],
    diff: &mut Diff<'_>,
) {
    let before_keys = js_keys(before);
    let after_keys = js_keys(after);
    if before_keys
        .iter()
        .chain(after_keys.iter())
        .any(|key| RESERVED_SEGMENTS.contains(&key.as_str()))
    {
        if !same_value(
            &Value::Object(before.clone()),
            &Value::Object(after.clone()),
        ) {
            diff.emit_set(path, &Value::Object(after.clone()));
        }
        return;
    }
    for key in &after_keys {
        if diff.overflowed {
            return;
        }
        let mut child = path.to_vec();
        child.push(Seg::Key(key.clone()));
        if let Some(before_value) = before.get(key) {
            diff_value(before_value, &after[key], &child, diff);
        } else {
            diff.emit_set(&child, &after[key]);
        }
    }
    for key in &before_keys {
        if diff.overflowed {
            return;
        }
        if !after.contains_key(key) {
            let mut child = path.to_vec();
            child.push(Seg::Key(key.clone()));
            diff.emit(Op::Delete(child));
        }
    }
}

/// 递归分派：深相等或已溢出时直接返回；字符串（且路径非空）走 `emit_string`；
/// 数组/对象递归；其余用 `emit_set`（根上退化为 `Replace`）。
///
/// 路径为空时字符串也走 `emit_set`：`Append`/`Truncate` 要求非空路径。这里用
/// `same_value`（结构深相等）做提前返回，因此两棵独立树中结构相等的子树不会产生
/// 任何 op。
fn diff_value(before: &Value, after: &Value, path: &[Seg], diff: &mut Diff<'_>) {
    if same_value(before, after) || diff.overflowed {
        return;
    }
    if let (Value::String(before), Value::String(after)) = (before, after)
        && !path.is_empty()
    {
        emit_string(before, after, path, diff);
        return;
    }
    if let (Value::Array(before), Value::Array(after)) = (before, after) {
        diff_array(before, after, path, diff);
        return;
    }
    if let (Value::Object(before), Value::Object(after)) = (before, after) {
        diff_object(before, after, path, diff);
        return;
    }
    diff.emit_set(path, after);
}

/// 估算一个值序列化成 JSON 文本后的 UTF-16 长度（`JSON.stringify(value).length` 的
/// 近似）：`null` 4、字符串加引号、数字按其 JS 文本、布尔 4/5、容器加括号与逗号。
///
/// 只用于 64 KiB 的退化判断，因此是成本模型而非精确序列化（例如不计字符串里需要
/// 转义的字符）。比较对象是 `diff_revisions` 的 `snapshotCost = json_cost(after) + 6`。
/// 数字经 `format_js_number` 格式化，长度按 UTF-16 码元计。
fn json_cost(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::String(text) => utf16_len(text) + 2,
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            utf16_len(&kiana_json::js_num::format_js_number(number))
        }
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Array(items) => {
            let mut cost = 2usize;
            for (index, item) in items.iter().enumerate() {
                cost += json_cost(item) + usize::from(index != 0);
            }
            cost
        }
        Value::Object(map) => {
            let mut cost = 2usize;
            for (index, key) in js_keys(map).into_iter().enumerate() {
                cost += utf16_len(&key) + 3 + json_cost(&map[&key]) + usize::from(index != 0);
            }
            cost
        }
    }
}

/// 估算路径的序列化成本：2（方括号）+ 每段（键按 `len + 2` 计引号；数字段按其 JS
/// 文本长度）+ 段间逗号。与 TS `pathCost` 一致，仅用于成本比较。
fn path_cost(path: &[Seg]) -> usize {
    let mut cost = 2usize;
    for (index, segment) in path.iter().enumerate() {
        let segment_cost = match segment {
            Seg::Key(key) => utf16_len(key) + 2,
            // TS: `String(segment).length`——数字段按 JS 的数字格式化。
            Seg::Index(index) => utf16_len(&kiana_json::js_num::format_js_number(*index as f64)),
        };
        cost += segment_cost + usize::from(index != 0);
    }
    cost
}

/// 估算一条 op 的 wire 成本：verb 与各字段的固定开销 + 路径/负载成本。常量与 TS
/// `operationCost` 逐项对应；只参与 `diff_revisions` 的 64 KiB 判断，不影响 op 生成。
fn operation_cost(operation: &Op) -> usize {
    match operation {
        Op::Replace(value) => 6 + json_cost(value),
        Op::Set(path, value) => 7 + path_cost(path) + json_cost(value),
        Op::Delete(path) => 6 + path_cost(path),
        Op::Append(path, text) => 7 + path_cost(path) + utf16_len(text) + 2,
        Op::Truncate(path, count) => 7 + path_cost(path) + count.to_string().len(),
        Op::Splice {
            path,
            index,
            remove,
            items,
        } => {
            10 + path_cost(path)
                + index.to_string().len()
                + remove.to_string().len()
                + json_cost(&Value::Array(items.clone()))
        }
        Op::Permute(path, permutation) => {
            7 + path_cost(path)
                + json_cost(&Value::Array(
                    permutation
                        .iter()
                        .map(|index| Value::Number((*index as u64).into()))
                        .collect(),
                ))
        }
    }
}

/// TS `diffRevisions(before, after)`：计算把 `before` 变成 `after` 的紧凑 op 批次。
///
/// # 参数与返回
///
/// - `before`：旧版本的 JSON 值；`after`：新版本。两者只读，不会被修改（测试
///   `diff_revisions_does_not_mutate_its_inputs` 锁定）。
/// - 返回 `Vec<Op>`：按顺序应用即可得到 `after`（`tests/delta_diff.rs` 用
///   `apply_immutable` 回放并断言结果等于 `after`）；两值深相等时返回空批次。
///
/// 函数返回 `Vec<Op>` 而非 `Result`，调用方不需要处理错误分支。
///
/// # 退化规则
///
/// 以下情况整批退化为一条 [`Op::Replace`]（对应 TS 的 `[["r", after]]`）：
///
/// - 中间 op 数达到 `MAX_DELTA_OPERATIONS`（4096）后仍有新 op；
/// - 估算成本 `deltaCost >= 65536` 且不低于快照成本 `json_cost(after) + 6`。
///
/// 退化后的批次只含 `Replace(after.clone())`。
///
/// # 不变量
///
/// - `Set`/`Delete`/`Append`/`Truncate` 的路径非空；根上的整体替换只产生 `Replace`。
/// - 数组相关 op 的下标是应用后（`after` 侧）的下标。
/// - 不含保留段路径：遇到含 `__proto__`/`constructor`/`prototype` 的对象时，该对象
///   整体用 `Set` 写回（根上则退化为 `Replace`）。
///
/// 函数无全局状态：TS 的 `WeakSet` 标记在这里是每次调用新建的 `Diff` 字段。
pub fn diff_revisions(before: &Value, after: &Value) -> Vec<Op> {
    let mut operations: Vec<Op> = Vec::new();
    // 内层作用域让 `diff` 对 operations 的可变借用在这里结束。
    {
        let mut diff = Diff::new(&mut operations);
        diff_value(before, after, &[], &mut diff);
        // 溢出：半成品没有意义，直接用完整快照替换。
        if diff.overflowed {
            return vec![Op::Replace(after.clone())];
        }
    }
    // 没有 op，或第一条就是根替换：不需要再做成本比较。
    if operations.is_empty() || matches!(operations.first(), Some(Op::Replace(_))) {
        return operations;
    }
    // 批次成本模型：2（外层数组）+ 每条 op 的成本 + 1 个逗号。
    let mut delta_cost = 2usize;
    for operation in &operations {
        delta_cost = delta_cost.saturating_add(operation_cost(operation) + 1);
    }
    // 估算成本没到 64 KiB 就不值得和整快照比较。
    if delta_cost < 65_536 {
        return operations;
    }
    // 只有严格小于快照成本才保留细粒度批次，否则一条根替换更省。
    let snapshot_cost = json_cost(after) + 6;
    if delta_cost >= snapshot_cost {
        vec![Op::Replace(after.clone())]
    } else {
        operations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emits_sets_and_deletes() {
        let before = json!({"keep": 1, "change": 1, "remove": true});
        let after = json!({"keep": 1, "change": 2, "add": 3});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![
                Op::Set(vec![Seg::Key("change".into())], json!(2)),
                Op::Set(vec![Seg::Key("add".into())], json!(3)),
                Op::Delete(vec![Seg::Key("remove".into())]),
            ]
        );
    }

    #[test]
    fn emits_string_append_and_front_truncation() {
        assert_eq!(
            diff_revisions(&json!({"text": "hello"}), &json!({"text": "hello world"})),
            vec![Op::Append(vec![Seg::Key("text".into())], " world".into())]
        );
        assert_eq!(
            diff_revisions(&json!({"text": "hello world"}), &json!({"text": "world"})),
            vec![Op::Truncate(vec![Seg::Key("text".into())], 6)]
        );
        assert_eq!(
            diff_revisions(&json!({"text": "abcdefgh"}), &json!({"text": "defghxyz"})),
            vec![
                Op::Truncate(vec![Seg::Key("text".into())], 3),
                Op::Append(vec![Seg::Key("text".into())], "xyz".into()),
            ]
        );
    }

    #[test]
    fn emits_permutation_for_a_pure_reorder_of_primitives() {
        // 数字/字符串在 JS 里按值比较，独立解析也不影响 `permutation`。
        let before = json!({"values": [1, 2, 3]});
        let after = json!({"values": [3, 1, 2]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![Op::Permute(vec![Seg::Key("values".into())], vec![2, 0, 1])]
        );
    }

    #[test]
    fn does_not_match_containers_from_independent_json_trees() {
        // TS 的 `permutation`/`identityAnchors` 用引用身份；两次 JSON 解析得到的
        // 等值对象互不相同，按引用身份匹配不会配对，只能整段 splice。
        let before = json!({"values": [{"value": 0}, {"value": 1}, {"value": 2}]});
        let after = json!({"values": [{"value": 2}, {"value": 0}, {"value": 1}]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 0,
                    remove: 0,
                    items: vec![json!({"value": 2})],
                },
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 3,
                    remove: 1,
                    items: vec![],
                },
            ]
        );
        // 对象数组重排也一样（TS test/delta-diff.test.ts 的 "emits a permutation" 输入
        // 在 Rust 里只能表达为独立解析，结果与 TS 的独立解析模式一致）。
        let before = json!({"values": [{"id": "a"}, {"id": "b"}, {"id": "c"}]});
        let after = json!({"values": [{"id": "c"}, {"id": "a"}, {"id": "b"}]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 0,
                    remove: 0,
                    items: vec![json!({"id": "c"})],
                },
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 3,
                    remove: 1,
                    items: vec![],
                },
            ]
        );
    }

    #[test]
    fn combines_removals_append_and_a_survivor_edit_like_independent_json() {
        // TS test/delta-diff.test.ts:149 的共享引用输入。`serde_json::Value` 无法表达
        // 共享子树，Rust 侧只能对应 TS 对独立解析值的批次（也已用 TS 参考实现核对）。
        let before = json!({"values": [
            {"id": "a", "stable": {}, "detail": {"text": "a"}},
            {"id": "b", "stable": {}, "detail": {"text": "b"}},
            {"id": "c", "stable": {}, "detail": {"text": "c"}},
            {"id": "d", "stable": {}, "detail": {"text": "d"}},
        ]});
        let after = json!({"values": [
            {"id": "a", "stable": {}, "detail": {"text": "a"}},
            {"id": "c", "stable": {}, "detail": {"text": "changed"}},
            {"id": "d", "stable": {}, "detail": {"text": "d"}},
            {"id": "e", "stable": {}, "detail": {"text": "e"}},
        ]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 1,
                    remove: 2,
                    items: vec![json!({"id": "c", "stable": {}, "detail": {"text": "changed"}})],
                },
                Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 3,
                    remove: 0,
                    items: vec![json!({"id": "e", "stable": {}, "detail": {"text": "e"}})],
                },
            ]
        );
    }

    #[test]
    fn normalizes_reordered_distinct_deeply_equal_objects_to_a_noop() {
        let first = json!({"nested": {"value": 1}});
        let second = json!({"nested": {"value": 1}});
        assert!(
            diff_revisions(
                &json!({"values": [first, second]}),
                &json!({"values": [second, first]})
            )
            .is_empty()
        );
    }

    #[test]
    fn emits_nothing_for_deeply_equal_values() {
        assert!(
            diff_revisions(
                &json!({"value": {"nested": [1, 2]}}),
                &json!({"value": {"nested": [1, 2]}})
            )
            .is_empty()
        );
    }

    #[test]
    fn keeps_a_leaf_edit_inside_a_reconstructed_array_narrow() {
        let before = json!({"values": [{"id": 1, "label": "one"}, {"id": 2, "label": "two"}]});
        let after = json!({"values": [{"id": 1, "label": "one"}, {"id": 2, "label": "changed"}]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![Op::Set(
                vec![
                    Seg::Key("values".into()),
                    Seg::Index(1),
                    Seg::Key("label".into())
                ],
                json!("changed")
            )]
        );
    }

    #[test]
    fn encodes_removals_canonically() {
        let cases: [(Vec<Value>, Vec<Value>, Vec<Op>); 4] = [
            (
                vec![json!(1), json!(2), json!(3), json!(4)],
                vec![json!(2), json!(3), json!(4)],
                vec![Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 0,
                    remove: 1,
                    items: vec![],
                }],
            ),
            (
                vec![json!(1), json!(2), json!(3), json!(4)],
                vec![json!(1), json!(2), json!(3)],
                vec![Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 3,
                    remove: 1,
                    items: vec![],
                }],
            ),
            (
                vec![json!(1), json!(2), json!(3), json!(4)],
                vec![json!(1), json!(3), json!(4)],
                vec![Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 1,
                    remove: 1,
                    items: vec![],
                }],
            ),
            (
                vec![json!(1), json!(2), json!(3), json!(4)],
                vec![],
                vec![Op::Splice {
                    path: vec![Seg::Key("values".into())],
                    index: 0,
                    remove: 4,
                    items: vec![],
                }],
            ),
        ];
        for (before, after, expected) in cases {
            assert_eq!(
                diff_revisions(&json!({ "values": before }), &json!({ "values": after })),
                expected
            );
        }
    }

    #[test]
    fn same_value_matches_structurally_equal_containers() {
        // `sameValue`（trim / 逐位匹配）仍是 `=== || equalJson`：结构相等的容器在
        // 这些位置相等（与 TS 相同）。只有 identity 系列函数用引用身份。
        let before = json!({"values": [{"value": 0}, {"value": 1}, {"value": 2}]});
        let after = json!({"values": [{"value": 0}, {"value": 1}, {"value": 2}]});
        assert!(diff_revisions(&before, &after).is_empty());
        // 逐位相同的容器不会触发 splice：前缀 trim 吃掉了整段。
        let before = json!({"values": [{"value": 0}, {"value": 1}, {"value": 2}]});
        let after = json!({"values": [{"value": 0}, {"value": 1}, {"value": 3}]});
        assert_eq!(
            diff_revisions(&before, &after),
            vec![Op::Set(
                vec![
                    Seg::Key("values".into()),
                    Seg::Index(2),
                    Seg::Key("value".into())
                ],
                json!(3)
            )]
        );
    }

    #[test]
    fn bounds_a_wide_object_with_a_root_replacement() {
        let mut before = serde_json::Map::new();
        let mut after = serde_json::Map::new();
        for index in 0..20_000 {
            before.insert(format!("field{index}"), json!(0));
            after.insert(format!("field{index}"), json!(1));
        }
        let before = Value::Object(before);
        let after = Value::Object(after);
        assert_eq!(
            diff_revisions(&before, &after),
            vec![Op::Replace(after.clone())]
        );
    }
}
