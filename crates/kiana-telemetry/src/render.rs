//! render.ts：`renderTrace` —— 把已结算 span 渲染成按时间顺序的森林。
//!
//! 输出必须逐字节稳定（spec §6）：
//!
//! ```text
//! root request="a b" attempt=2 cached=false tags=["a","b"] sizes=[1,2] flags=[true,false] (20ms)
//!   · checkpoint phase="tools" (+2ms)
//!   · tick (+3.5ms)
//!   → link trace remote-trace span remote reason="steer"
//!   → link trace other-trace span other
//!   first (4.5ms) error Error: failed call
//!     nested (1ms)
//!   second (2ms) error
//! ```
//!
//! 算法要点（与 TS 的 `Map` / `Array#sort` / `Array#reverse` 一一对应）：
//! 1. 按 id 去重：后来的重复 id **覆盖内容**但保留**首次出现的位置**（`IndexMap::insert` 语义）；
//! 2. 按 `startedMs` 排序，用 `timsort`（V8 TimSort 的移植）：比较器结果是 `NaN` 时
//!    V8 归一化为「相等」，因此既不能 panic（`slice::sort_by` 会），也不能用 `total_cmp`；
//! 3. 根 = 无 `parentId` 或父级不在 `byId` 中；兄弟保持排序后的顺序；
//! 4. 迭代 DFS（无递归；2000 层链必须可渲染）：根先 `reverse()` 再 `pop()`，子级逆序压栈；
//! 5. 空输入 → `""`；不修改入参。
//!
//! 数字与字符串走 JS 语义：属性值走 `JSON.stringify`（`crate::jsval::attribute_json_text`，
//! 非有限数字 → `null`），模板字面量里的时长/事件时间走 `Number::toString`
//! （`crate::jsval::js_number_text`，`NaN`/`Infinity`/`-Infinity` 原样输出）。
//!
//! TS: `packages/telemetry/src/render.ts`
//!
//! # 为什么需要这个文件
//!
//! `renderTrace` 是给人（以及模型）看的纯文本视图，属于 `@earendil-works/kiana-telemetry/render`
//! 子路径；它不是 store 查询的一部分，也不产生持久状态。输出是当前稳定契约，必须逐字节
//! 稳定，因为黄金文件直接比较整段文本；改动格式需同步更新 `tests/fixtures/render_traces.json`。
//!
//! # 执行阶段
//!
//! 1. 去重：`IndexMap<&str, &StoredSpan>`，重复 id 覆盖内容、保留首次出现的位置；
//! 2. 排序：`timsort::sort_by_js` 复刻 V8 `Array#sort`（NaN 归一化为相等，不 panic）；
//! 3. 建森林：无父或父 id 不在输入中者为根，其余挂到 `children[parentId]`；
//! 4. 迭代 DFS：根逆序入栈，`pop` 顺序即渲染顺序；子级同样逆序入栈；
//! 5. 逐行拼装：span 行、事件行、link 行；最后 `join("\n")`。
//!
//! # 边界与注意事项
//!
//! - 只渲染从根可达的 span：不可达的自环/环会被静默丢弃；即使某个环从根可达，每个保留的
//!   id 也最多渲染一次，所以没有环检测也不会死循环（与 TS 的 `Map` 行为一致）。
//! - 保留的每个 id 最多渲染一次；重复输入不会产生重复行。
//! - 不使用递归：2000 层链由 `tests/render.rs` 锁定。
//! - 输出行数 = 被渲染的 span 数 + 事件数 + link 数；空输入返回 `""`。
//! - `by_id_has` 每次调用线性扫描输入（`O(n)`），span 很多时建森林阶段最坏 `O(n²)`；
//!   这是为了保持“父 id 只要在任意输入中出现过就算存在”的判定。
//! - 函数只借用 `spans`，不修改入参（排序与反向遍历都作用于局部 `Vec`）。

use indexmap::IndexMap;

use crate::index::{SpanAttributes, SpanStatus, StoredSpan};
use crate::jsval::{attribute_json_text, js_entries, js_number_text};

/// `formatAttributes(attributes = {})`：`Object.entries` 顺序（整数样键升序在前），每个条目渲染为
/// ` key=<JSON.stringify(value)>`。
///
/// 空属性 map 返回空串（调用方直接拼接，因此不会多出空格）；每个条目恰好一个前导空格。
/// 键序走 `crate::jsval::js_entries`（JS `Object.entries` 语义），值走 `JSON.stringify`
/// 语义（非有限数字 → `null`），两者共同决定黄金文本。
fn format_attributes(attributes: &SpanAttributes) -> String {
    let mut out = String::new();
    for (key, value) in js_entries(attributes) {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        out.push_str(&attribute_json_text(value));
    }
    out
}

/// 渲染一个已结算 span 的森林。
///
/// # 参数与返回
/// - `spans`：任意顺序的 `StoredSpan` 切片，函数只读，不修改入参。
/// - 返回：以 `\n` 连接的多行文本；空输入返回空串（不是 `"\n"`）。
///
/// # 失败模式
/// 没有 `Result`，也没有 `unwrap`/`expect`/下标访问；无效父 id、环、`NaN` 时间都按 TS 的
/// 规则静默处理（父级缺失当根、不可达环丢弃、NaN 在排序时视为相等）。正常情况下除内存
/// 分配失败外不会 panic。
///
/// TS: `packages/telemetry/src/render.ts#renderTrace`
pub fn render_trace(spans: &[StoredSpan]) -> String {
    // `new Map(spans.map(...))`：后值覆盖前值，位置保持首次出现。
    let mut by_id: IndexMap<&str, &StoredSpan> = IndexMap::with_capacity(spans.len());
    for span in spans {
        by_id.insert(span.id.as_str(), span);
    }

    // `[...byId.values()].sort((l, r) => l.startedMs - r.startedMs)`：V8 TimSort，NaN 视为相等。
    let mut ordered: Vec<&StoredSpan> = by_id.into_values().collect();
    crate::timsort::sort_by_js(&mut ordered, |left, right| {
        left.started_ms - right.started_ms
    });

    let mut roots: Vec<&StoredSpan> = Vec::new();
    let mut children: IndexMap<&str, Vec<&StoredSpan>> = IndexMap::new();
    // 建森林：父 id 缺失或不在本次输入中 → 根；否则挂到对应父级的子列表。
    // 子列表沿用排序结果，所以兄弟之间按 startedMs（平局按原顺序）渲染。
    for span in ordered {
        match span.parent_id.as_deref() {
            None => roots.push(span),
            Some(parent) if !by_id_has(spans, parent) => roots.push(span),
            Some(parent) => children.entry(parent).or_default().push(span),
        }
    }

    let mut lines: Vec<String> = Vec::new();
    let mut pending: Vec<(&StoredSpan, usize)> =
        roots.iter().rev().map(|span| (*span, 0usize)).collect();
    // 根按 `reverse()` 后的顺序压栈、从尾部 pop ⇒ 起点升序渲染。
    while let Some((span, depth)) = pending.pop() {
        let indent = "  ".repeat(depth);
        let mut error = String::new();
        // 错误详情只在 `Error` 且带 `StatusError` 时追加；裸 `Error { error: None }` 只输出
        // " error"，与 TS 中“抛出的不是 Error 实例”对应。
        if let SpanStatus::Error { error: detail } = &span.status {
            error.push_str(" error");
            if let Some(detail) = detail {
                error.push(' ');
                error.push_str(&detail.name);
                error.push_str(": ");
                error.push_str(&detail.message);
            }
        }
        // 模板字面量插值 = ECMAScript `Number::toString`（不是 JSON）：非有限值写成
        // `NaN` / `Infinity` / `-Infinity`（spec §6 第 6/7 步）。
        let duration = js_number_text(span.ended_ms - span.started_ms);
        lines.push(format!(
            "{indent}{}{} ({duration}ms){error}",
            span.name,
            format_attributes(&span.attributes)
        ));
        // 事件与 link 都按存储顺序逐行输出，缩进比所属 span 多一级。
        for event in &span.events {
            let time = js_number_text(event.time_ms);
            lines.push(format!(
                "{indent}  · {}{} (+{time}ms)",
                event.name,
                format_attributes(&event.attributes)
            ));
        }
        for link in &span.links {
            let attributes = match &link.attributes {
                Some(attributes) => format_attributes(attributes),
                None => String::new(),
            };
            lines.push(format!(
                "{indent}  → link trace {} span {}{attributes}",
                link.trace_id, link.span_id
            ));
        }
        if let Some(descendants) = children.get(span.id.as_str()) {
            // 逆序压栈，配合栈顶 `pop()` 得到按排序结果的升序渲染。
            for descendant in descendants.iter().rev() {
                pending.push((descendant, depth + 1));
            }
        }
    }
    lines.join("\n")
}

/// `byId.has(parentId)`：父级必须是被去重后**保留**的 id 集合成员（等于任意输入的 id 即可，
/// 因为保留的位置来自首次出现，而存在性只取决于是否出现过）。
///
/// 这里扫描的是**原始输入**而不是去重后的 map：只要某个 id 出现过，它一定在去重结果中
/// （`IndexMap::insert` 不会删除已有键），所以两种判定等价。代价是每次检查 `O(n)`；
/// 输入规模由调用方决定，当前实现没有额外维护键集合。
fn by_id_has(spans: &[StoredSpan], id: &str) -> bool {
    spans.iter().any(|span| span.id == id)
}
