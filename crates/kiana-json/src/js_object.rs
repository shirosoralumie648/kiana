//! JS 对象键序与 `JSON.stringify` 等价输出。
//!
//! 三个必须逐字节一致的地方：
//! 1. **键序**：JS 对象的自有键按「整数索引键（升序）→ 其余键（插入序）」排列。
//!    `serde_json` 的 `preserve_order` 只保插入序，因此任何会经过 JS 序列化的对象
//!    （会话文件、chord 帧、遥测快照、扩展的 `details`）都必须用 [`sort_js_keys`]
//!    归一化后再写出。
//! 2. **数字**：走 [`crate::js_num::format_js_number`]（serde 的默认实现与 JS 不同）。
//! 3. **`undefined`**：JSON 里没有 `undefined`；JS 在对象里省略该键、在数组里写 `null`。
//!    Rust 侧用 `Option<Value>` + `skip_serializing_if` 表达对象省略；数组里的
//!    `undefined` 由调用方显式写成 `Value::Null`。
//!
//! # 递归与栈
//!
//! `JSON.stringify` / `Object.keys` 在 V8 里能处理上百万层嵌套（`JSON.parse` +
//! `JSON.stringify` 实测 5,000,000 层），而固定栈的 Rust 递归在 ~16k 层就 abort 整个进程。
//! 模型可控的 JSON 因此足以打死进程，所以本模块的 [`sort_js_keys`]、[`stringify()`]、
//! [`stringify_pretty`]、[`deep_equal`] 全部用**显式栈**迭代实现，深度只受堆内存限制。
//!
//! 注意 `serde_json::Value` 自身的 `Drop` 仍是递归的（实测 ~100k 层在 8 MB 栈上溢出）；
//! 那是 `Value` 类型的外部行为，不在本 crate 能改的范围内（见 `js_parse` 模块文档）。
//!
//! # 执行流程
//!
//! - [`stringify()`] / [`stringify_pretty`]：显式栈的任务列表（各自的 `Task` 枚举）驱动
//!   写出。容器先输出开括号，再按**逆序**把子节点、逗号、键与冒号压栈，使弹出顺序
//!   等于文本顺序；因此每个分支都不需要知道「我是不是最后一个元素」。
//! - [`sort_js_keys`]：后序遍历——`Visit` 容器时先压入对应的 `Finish*` 再压子节点；
//!   子节点完成后结果落在 `done` 尾部，`FinishObject`/`FinishArray` 从尾部取回并
//!   重建容器。`FinishObject` 会翻转尾部值以恢复原始插入序，再交给 `reorder_pairs`
//!   做键序归一。
//! - [`deep_equal`]：把待比较的 `(左, 右)` 对压栈逐层展开；容器先比长度再展开子项，
//!   发现任一标量或长度不同立即返回 `false`。
//!
//! # 设计原因
//!
//! 为什么不递归：见上节「递归与栈」——模型可控的 JSON 深度可以超过固定栈的容量，
//! 而 `JSON.stringify` 在 V8 里只受堆内存限制。为什么不先克隆再排序：`sort_js_keys`
//! 会产生第二棵同样深的 `Value`，两棵树的递归 `Drop` 都可能栈溢出；`stringify`
//! 因此直接借用原树，只在输出时计算键序。
//!
//! # 注意事项
//!
//! - [`sort_js_keys`] 返回**新树**，不修改入参；若只需要输出文本，优先用
//!   [`stringify()`]，避免为深树再分配一份同规模的内存。
//! - `stringify` 的数字分支用 `as_f64()`：`u64` 超出 2^53 时文本按 JS 的 f64 舍入写，
//!   但原 `Value` 里的整数表示不会被修改。需要在 `Value` 层面归一化时用
//!   [`crate::js_parse::normalize_numbers`]。
//! - 键序规则只作用于对象；数组元素顺序永远保持，遍历只深入元素内部。

use serde_json::Value;

use crate::js_num::format_js_number;
use crate::js_string::js_quote;

/// 是否是「数组索引」形态的键：`0`、`1`、…、`4294967294`（无前导零）。
/// JS 规范里 `ToString(ToUint32(k)) === k && k !== "4294967295"`。
fn is_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: u32 = key.parse().ok()?;
    if v == u32::MAX {
        return None;
    }
    Some(v)
}

/// 按 JS 键序重排一组 `(键, 值)`（输入按插入序）；返回的 `Map` 迭代序即输出序。
fn reorder_pairs(pairs: impl Iterator<Item = (String, Value)>) -> serde_json::Map<String, Value> {
    let mut indices: Vec<(u32, String, Value)> = Vec::new();
    let mut others: Vec<(String, Value)> = Vec::new();
    for (k, v) in pairs {
        match is_array_index(&k) {
            Some(i) => indices.push((i, k, v)),
            None => others.push((k, v)),
        }
    }
    // 稳定排序：索引键唯一（同一 Map 里不可能有重复键）。
    indices.sort_by_key(|(i, _, _)| *i);
    let mut out = serde_json::Map::with_capacity(indices.len() + others.len());
    for (_, k, v) in indices {
        out.insert(k, v);
    }
    for (k, v) in others {
        out.insert(k, v);
    }
    out
}

/// 把对象的键按 JS 顺序重排（整数索引键升序在前，其余保持插入序）。
///
/// 返回新的对象；数组递归处理；标量原样返回。用显式栈，深度无上限（见模块文档）。
///
/// # 算法
/// 1. `Visit` 到的容器先压入对应的 `Finish*`，再压入全部子节点；
/// 2. 子节点完成后，结果依次放入 `done` 尾部；
/// 3. `FinishArray` 取尾部 `len` 个结果按原顺序重建数组；`FinishObject` 取尾部
///    `len` 个结果（此时是逆序）翻转回原始插入序，与键重新配对后交给
///    `reorder_pairs` 做 JS 键序归一。
///
/// # 复杂度与分配
/// 每个值恰好访问一次，索引键排序的代价是 `O(k log k)`（`k` 为该层整数键个数）；
/// 结果树与输入同规模，`done` 与 `tasks` 的合计峰值与节点数成正比。
///
/// # 边界
/// - 空对象/空数组走快速路径，不压 `Finish*`。
/// - `serde_json::Map` 不允许重复键，因此索引键排序不需要处理相等键的稳定性；
///   `sort_by_key` 本身是稳定排序，但这里不依赖这一点。
/// - `Visit` 的兜底分支 `other.clone()` 把标量原样复制；若未来 `Value` 增加新变体，
///   需要重新判断它是否应该被递归处理。
pub fn sort_js_keys(value: &Value) -> Value {
    // 后序重建：`stack` 里是「已在处理」的节点，`done` 里是已完成的结果。
    enum Task<'a> {
        /// 访问一个值：容器展开成子任务，标量直接进 `done`。
        Visit(&'a Value),
        /// 该对象的所有子值已完成，取 `done` 尾部重建并重排键序。
        FinishObject(&'a serde_json::Map<String, Value>),
        /// 该数组的 `len` 个子值已完成，按顺序重建数组。
        FinishArray(usize),
    }
    let mut tasks = vec![Task::Visit(value)];
    let mut done: Vec<Value> = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(Value::Object(map)) => {
                if map.is_empty() {
                    done.push(Value::Object(serde_json::Map::new()));
                    continue;
                }
                tasks.push(Task::FinishObject(map));
                for (_, v) in map {
                    tasks.push(Task::Visit(v));
                }
            }
            Task::Visit(Value::Array(items)) => {
                if items.is_empty() {
                    done.push(Value::Array(Vec::new()));
                    continue;
                }
                tasks.push(Task::FinishArray(items.len()));
                // 逆序压栈 → 正序完成，`FinishArray` 直接按顺序取尾部切片。
                for item in items.iter().rev() {
                    tasks.push(Task::Visit(item));
                }
            }
            Task::Visit(other) => done.push(other.clone()),
            Task::FinishArray(len) => {
                // `done` 尾部恰好是这 `len` 个子节点，且按数组原顺序排列
                // （`Visit` 分支的逆序压栈保证）。
                let start = done.len() - len;
                let items: Vec<Value> = done.split_off(start);
                done.push(Value::Array(items));
            }
            Task::FinishObject(map) => {
                // `Visit` 按 map 迭代序压栈，弹出是逆序完成 → `done` 尾部是**逆序**的；
                // 反过来就是原始插入序（`reorder_pairs` 依赖它来保持「其余键」的顺序）。
                let mut values: Vec<Value> = done.split_off(done.len() - map.len());
                values.reverse();
                let reordered =
                    reorder_pairs(map.iter().zip(values).map(|((k, _), v)| (k.clone(), v)));
                done.push(Value::Object(reordered));
            }
        }
    }
    debug_assert_eq!(done.len(), 1);
    done.pop().unwrap_or(Value::Null)
}

/// 就地按 JS 键序重排所有对象层级（`crate::js_parse::parse` 用它让解析结果的枚举序
/// 与 `Object.keys()` 一致）。显式栈，无递归深度限制。
///
/// # 为什么用 `std::mem::take`
/// 不能一边持有 `&mut Map` 一边重建它；`take` 把旧 `Map` 移出（原处留下空 `Map`），
/// 重建后再逐个子节点压栈。这样不需要克隆整个对象，也不会在遍历中使引用失效。
///
/// # 与 `sort_js_keys` 的关系
/// 两者共用 `reorder_pairs`，键序规则不会漂移。区别是这里直接修改调用方的树
/// （`parse` 的返回值），避免为解析结果再复制一份整树。
pub(crate) fn reorder_in_place(root: &mut Value) {
    let mut stack: Vec<&mut Value> = vec![root];
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(map) => {
                let old = std::mem::take(map);
                *map = reorder_pairs(old.into_iter());
                for (_, child) in map.iter_mut() {
                    stack.push(child);
                }
            }
            Value::Array(items) => {
                for child in items.iter_mut() {
                    stack.push(child);
                }
            }
            _ => {}
        }
    }
}

/// `JSON.stringify(value)` 的等价文本（无缩进）。
///
/// 与 `serde_json::to_string` 的差异：
/// - 数字走 JS 格式化；
/// - 对象键按 JS 顺序输出（整数索引键升序在前，其余插入序）；
/// - `NaN`/`Infinity` 写成 `null`；
/// - 不含 `undefined`（JSON 值里本就不存在）。
///
/// 不先 `sort_js_keys` 克隆整棵树：深输入（模型可控）下克隆会产生第二棵同样深的值，
/// 而 `Value` 的 `Drop` 是递归的，`stringify` 返回时就会在析构里栈溢出。这里直接按 JS
/// 键序**借用**原值写出，深度由显式栈承担。
///
/// # 输入输出
/// - `value`：任意深度/形状的 JSON 值，只读借用，不会被修改。
/// - 返回：紧凑 JSON 文本（无空格、无换行），可直接写入磁盘或网络。
///
/// # 键与字符串
/// 对象键与字符串值都经 [`crate::js_string::js_quote`] 转义（只转义 `"`、`\` 与
/// U+0000–U+001F，与 `JSON.stringify` 一致）。键的输出顺序由 `ordered_keys` 计算，
/// 不修改原 `Map`。
///
/// # 失败模式
/// 无 `Result`：所有 `Value` 都能被写出。JS 里无法用 JSON 表达的值（`undefined`、
/// 函数、`Symbol`）在 Rust 侧本就不存在；调用方需按模块文档的约定用
/// `Option`/`Value::Null` 建模。
pub fn stringify(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

/// `JSON.stringify(value, null, indent)` 的等价文本；`indent` 是每级空格数。
///
/// JS 的 `space` 语义是 `ToInteger` 后 clamp 到 0..=10：`<= 0` 时退化为**紧凑输出**
/// （`JSON.stringify(v, null, 0) === JSON.stringify(v)`），`> 10` 时按 10 处理。
///
/// 冻结签名只有 `usize`，因此无法表达 JS 的另外两种 `space` 形态（字符串缩进如 `"\t"`，
/// 以及 0.5 这类 `ToInteger` 为 0 但仍是 pretty 的浮点）；若上游需要走 §7.0 规则 1 新开函数。
///
/// # 与 JS 的对应
/// 等价于 `JSON.stringify(value, null, indent)`：每级缩进 `indent` 个空格，`indent`
/// 上限为 10；空容器输出 `[]`/`{}` 且不换行（与 JS 相同，`tests/js_object.rs` 的
/// fixture 锁定）。整个写出仍走显式栈，深度不受调用栈限制。
///
/// # 边界
/// `indent == 0` 直接转调 [`stringify()`]，不进入 pretty 任务栈。
pub fn stringify_pretty(value: &Value, indent: usize) -> String {
    if indent == 0 {
        return stringify(value);
    }
    let pad = " ".repeat(indent.min(10));
    let mut out = String::new();
    write_value_pretty(value, &mut out, &pad);
    out
}

/// `map` 的键按 JS 输出顺序排列（整数索引键升序在前，其余保持插入序）。
///
/// 只读取键，不触碰值；整数索引键需要排序，复杂度为 `O(k log k)`（`k` 为该类键的
/// 个数），其余键按 `Map` 的插入序直接追加。
fn ordered_keys(map: &serde_json::Map<String, Value>) -> Vec<&String> {
    let mut indices: Vec<(u32, &String)> = Vec::new();
    let mut others: Vec<&String> = Vec::new();
    for k in map.keys() {
        match is_array_index(k) {
            Some(i) => indices.push((i, k)),
            None => others.push(k),
        }
    }
    indices.sort_by_key(|(i, _)| *i);
    let mut out: Vec<&String> = Vec::with_capacity(map.len());
    out.extend(indices.into_iter().map(|(_, k)| k));
    out.extend(others);
    out
}

/// 紧凑写出（显式栈；见模块文档）。
///
/// 任务栈模型：栈顶先执行。容器的子节点与分隔符都按**逆序**入栈，因此弹出顺序等于
/// 文本顺序；`Comma` 表示「在下一次写出之前补一个逗号」，这样空容器与单元素容器都
/// 不需要特判。
fn write_value(value: &Value, out: &mut String) {
    enum Task<'a> {
        /// 待写出的值（子容器会继续展开成任务）。
        Value(&'a Value),
        /// 分隔符（在下一项**之前**写）。
        Comma,
        /// 直接追加的静态文本（括号、冒号）。
        Text(&'static str),
        /// 对象键，写出时由 `js_quote` 加上引号。
        Key(&'a String),
    }
    let mut tasks = vec![Task::Value(value)];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Text(text) => out.push_str(text),
            Task::Comma => out.push(','),
            Task::Key(k) => out.push_str(&js_quote(k)),
            Task::Value(Value::Null) => out.push_str("null"),
            Task::Value(Value::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
            Task::Value(Value::Number(n)) => {
                out.push_str(&format_js_number(n.as_f64().unwrap_or(f64::NAN)));
            }
            Task::Value(Value::String(s)) => out.push_str(&js_quote(s)),
            Task::Value(Value::Array(items)) => {
                out.push('[');
                tasks.push(Task::Text("]"));
                for (i, item) in items.iter().enumerate().rev() {
                    tasks.push(Task::Value(item));
                    if i > 0 {
                        tasks.push(Task::Comma);
                    }
                }
            }
            Task::Value(Value::Object(map)) => {
                out.push('{');
                tasks.push(Task::Text("}"));
                let keys = ordered_keys(map);
                for (i, k) in keys.iter().enumerate().rev() {
                    if let Some(v) = map.get(*k) {
                        tasks.push(Task::Value(v));
                    }
                    tasks.push(Task::Text(":"));
                    tasks.push(Task::Key(k));
                    if i > 0 {
                        tasks.push(Task::Comma);
                    }
                }
            }
        }
    }
}

fn newline(out: &mut String, pad: &str, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str(pad);
    }
}

/// pretty 写出（显式栈）。空容器永远内联（`[]` / `{}`），与 JS 一致。
///
/// 与紧凑版的差别：换行与缩进也作为任务入栈（`Task::Newline`），因此不需要在每个
/// 容器分支里手写缩进状态；标量与空容器直接复用 `write_value`，保证两种输出在叶子
/// 的键/字符串转义上不会分叉。
fn write_value_pretty(value: &Value, out: &mut String, pad: &str) {
    enum Task<'a> {
        /// 待写出的值，附带当前容器深度（用于子项缩进）。
        Value(&'a Value, usize),
        /// 分隔符（在下一项之前写）。
        Comma,
        /// 换行 + `depth` 级缩进。
        Newline(usize),
        /// 直接追加的静态文本（括号、`: `）。
        Text(&'static str),
        /// 对象键，写出时由 `js_quote` 加上引号。
        Key(&'a String),
    }
    let mut tasks = vec![Task::Value(value, 0)];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Text(text) => out.push_str(text),
            Task::Comma => out.push(','),
            Task::Newline(depth) => newline(out, pad, depth),
            Task::Key(k) => out.push_str(&js_quote(k)),
            Task::Value(Value::Array(items), depth) if !items.is_empty() => {
                out.push('[');
                tasks.push(Task::Text("]"));
                tasks.push(Task::Newline(depth));
                for (i, item) in items.iter().enumerate().rev() {
                    tasks.push(Task::Value(item, depth + 1));
                    tasks.push(Task::Newline(depth + 1));
                    if i > 0 {
                        tasks.push(Task::Comma);
                    }
                }
            }
            Task::Value(Value::Object(map), depth) if !map.is_empty() => {
                out.push('{');
                tasks.push(Task::Text("}"));
                tasks.push(Task::Newline(depth));
                let keys = ordered_keys(map);
                for (i, k) in keys.iter().enumerate().rev() {
                    if let Some(v) = map.get(*k) {
                        tasks.push(Task::Value(v, depth + 1));
                    }
                    tasks.push(Task::Text(": "));
                    tasks.push(Task::Key(k));
                    tasks.push(Task::Newline(depth + 1));
                    if i > 0 {
                        tasks.push(Task::Comma);
                    }
                }
            }
            Task::Value(other, _) => {
                // 空容器与标量：复用紧凑写出。
                write_value(other, out);
            }
        }
    }
}

/// 深比较：忽略对象键的插入序（按 key 匹配），数字按 f64 值比较。
///
/// 不能直接用 `sort_js_keys(a) == sort_js_keys(b)`：`serde_json::Number` 的 `==` 会区分
/// `PosInt`/`NegInt`/`Float`，于是 `1 != 1.0`、`-0 != 0`，而 JS 的标量相等是 `===` 语义
/// （数字只有一个 f64 类型，`-0 === 0`，`NaN !== NaN`）。
///
/// 显式栈实现（深度无上限，见模块文档）；键按名字匹配，与插入序无关。
///
/// # 语义细节
/// - 对象：键集合必须相同（先比长度，再逐键在对方中查找），与插入序无关；
/// - 数组：长度必须相同，元素逐项比较；
/// - 数字：统一转成 `f64` 后比较，因此 `1` 与 `1.0` 相等、`-0` 与 `0` 相等。这符合
///   JS 的 `===`，但也意味着两个不同的超大整数（都超过 2^53、舍入到同一 f64）会被
///   判为相等——在 JS 里它们本来就是同一个 `Number`。
/// - `NaN`：`serde_json::Number` 通常无法保存 NaN（`Number::from_f64(NaN)` 返回
///   `None`），但 `as_f64()` 的兜底值仍是 NaN，因此这里显式判 `is_nan()`，保持
///   `NaN !== NaN`。
///
/// # 失败模式
/// 无错误路径；遇到第一处不同立即返回 `false`。显式栈的峰值与树的节点数成正比。
/// 本函数只做结构/数值比较，不比较对象键的插入顺序。
///
/// # 与 `js_strict_equal` 的分工
/// 本函数是「按内容深比较」，对应测试断言与 `JSON.stringify` 后的语义相等；
/// `js_strict_equal` 对应 JS 的 `===`，对容器一律返回 `false`。
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((a, b)) = stack.pop() {
        match (a, b) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(x), Value::Bool(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Number(x), Value::Number(y)) => {
                let fx = x.as_f64().unwrap_or(f64::NAN);
                let fy = y.as_f64().unwrap_or(f64::NAN);
                // NaN !== NaN；-0.0 == 0.0 在 f64 的 `==` 下成立，正是 JS 的语义。
                if !(fx == fy) || fx.is_nan() {
                    return false;
                }
            }
            (Value::String(x), Value::String(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                for (l, r) in x.iter().zip(y.iter()) {
                    stack.push((l, r));
                }
            }
            (Value::Object(x), Value::Object(y)) => {
                // 先比长度可快速排除「键集合不同」；随后逐键在 `y` 中查找，
                // 因此 `Map` 的迭代序在这里无关紧要。
                if x.len() != y.len() {
                    return false;
                }
                for (k, v) in x {
                    match y.get(k) {
                        Some(other) => stack.push((v, other)),
                        None => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_keys_come_first_in_ascending_order() {
        let v = json!({"b": 1, "10": 2, "2": 3, "a": 4, "01": 5});
        assert_eq!(stringify(&v), r#"{"2":3,"10":2,"b":1,"a":4,"01":5}"#);
    }

    #[test]
    fn numbers_use_javascript_formatting() {
        let v = json!({"a": 1e21, "b": 1e-7, "c": -0.0});
        assert_eq!(stringify(&v), r#"{"a":1e+21,"b":1e-7,"c":0}"#);
    }

    #[test]
    fn pretty_matches_two_space_indent() {
        let v = json!({"a": [1, {"b": null}], "c": []});
        assert_eq!(
            stringify_pretty(&v, 2),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": null\n    }\n  ],\n  \"c\": []\n}"
        );
    }
}
