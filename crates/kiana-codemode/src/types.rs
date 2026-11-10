//! types.ts：codemode 的公开数据契约。
//!
//! TS 源：`packages/codemode/src/types.ts`（254 行，纯类型，无运行时代码）。
//!
//! 约定（ARCHITECTURE.md §4.3）：
//! - 字段顺序 == TS 接口声明顺序 == JSON 输出顺序（`serde_json` 的 `preserve_order`）；
//! - `field?: T` → `Option<T>` + `skip_serializing_if`（TS 的 `undefined` 在 JSON 里被省略）；
//! - `unknown` → [`serde_json::Value`]；`undefined` → `Option<Value>::None`。
//!
//! # JSON 文本保真（`ARCHITECTURE.md` §4.3 第 3、4 条）
//!
//! 这些类型是 `kiana-durable` 的 journal（`JournalEntry.global.outcome` 等）与 worker/remote
//! 协议的线上形状，写出的字节是稳定契约，必须与 JS `JSON.stringify` 的输出一致：
//!
//! - **浮点**：每个 `f64` 字段都 `serialize_with = "serialize_js_number"`，走
//!   [`kiana_json::js_num::format_js_number`]。serde 的默认实现写 `3.0`/`1e-6`/`-0.0`，
//!   JS 写 `3`/`0.000001`/`0`（`CodemodeCall.durationMs`、`HostResult.now`、
//!   `VmStartData.memoryLimitBytes`、`VmDeterministic.startedAt`/`seed`、
//!   `CodemodeSourceOptions.maxOutputTokens`/`timeoutMs`）。
//! - **键序**：`Record<string, unknown>` 形状的字段（[`CodemodeStoreWrites::set`]、
//!   [`CodemodeOutcomeErr::properties`]、`VmStartData.store`）用 `serialize_js_keyed_map`
//!   写出：JS 对象把整数索引键按升序排在前面，其余按插入序——`IndexMap` 的 serde 实现只保
//!   插入序。反序列化走 `deserialize_js_keyed_map`，把 `JSON.parse` 会给 JS 的枚举序还原出来。
//! - **`null` 与 `undefined`**：TS 里 `unknown` 是三态的。JSON 里 `undefined` 是「键缺失」，
//!   `null` 是一个值。带 `deserialize_optional_value` 的字段（[`CodemodeResultOk::value`]、
//!   [`CodemodeOutcomeOk::value`]、[`CodemodeHostCall::args`]）因此把 `"value": null` 解析成
//!   `Some(Value::Null)`，缺失才解析成 `None`——否则 journal 重放会把脚本返回的 `null`
//!   变成「未返回」（TS 的 `packages/durable/src/codemode/codemode.ts` 直接存执行结果的 JSON）。
//!
//! 嵌在 `unknown` 字段里的 [`serde_json::Value`] 不经过这些函数：它们的数字/键序由**写出方**
//! 决定，按 `ARCHITECTURE.md` §4.3 必须走 `kiana_json::js_object::stringify`（本 crate 的
//! 协议层只在 `store` 里传 JSON 文本，`Value` 形状的值来自 `kiana_json::parse`，其数字已按 JS
//! 的 f64 语义归一化）。
//!
//! 与 TS 的另两处语义映射：
//! - `AbortSignal` → [`tokio_util::sync::CancellationToken`]（ARCHITECTURE.md §4.2）；
//! - `Promise<unknown>` → [`BoxFuture`]，`execute` 抛出的值取 `errorMessage` 文本
//!   （`Error` 取 `message`，否则 `String(v)`），因此 Rust 侧是 `Err(String)`。
//!
//! # 这个文件负责什么（边界）
//!
//! 内容以**数据契约**（`Codemode*` 结构体/枚举与若干回调、类型别名）与 **JSON 适配**
//! （上一节的 `pub(crate)` serde 函数、[`deserialize_optional_value`] 与两个手写
//! `Deserialize`）为主，但并非纯数据：`CodemodeDeclaredTool::script_name` 计算脚本标识符，
//! [`CodemodeResult::ok`] 按变体给出 TS 的 `result.ok` 判别值，[`CodemodeError`] 实现
//! `Display`（逐字等于 TS 的 `error.message`），[`CodemodeResultOk`]/[`CodemodeResultErr`]
//! 提供到 [`CodemodeResult`] 的 `From`。[`CodemodeRun`] 还持有私有的 `finished`/`deliver`
//! 状态，其 `CodemodeRun::finished`/`CodemodeRun::deliver` 只是转发给
//! `runtime::execution` 的实现（校验在那边完成）。本文件不启动 VM、不持锁、不做 IO；
//! 执行顺序、超时与取消都在 [`crate::runtime::execution`]。
//!
//! 本文件的 `pub` 项**不等于** crate 的公开面：根面由 [`crate::index`] 的 `pub use` 决定，
//! `/portable` 面由 [`crate::portable`] 决定（两个模块的文档各有冻结清单）。
//!
//! # 谁在使用这些类型
//!
//! - [`crate::runtime::execution`]：构造 [`CodemodeResult`]、接收 [`CodemodeOutcome`]、
//!   暴露 [`CodemodeRun`]，并在 `execute()` 路径的每次调用前构造 [`CodemodeToolContext`]
//!   （`start()` 路径的调用改走 [`CodemodeHostCall`] + `deliver()`）；
//! - [`crate::runtime::protocol`] 与 [`crate::source`]：复用本文件 `pub(crate)` 的数字与
//!   键序函数（它们不在公开面里）；
//! - [`crate::index`] / [`crate::portable`]：决定哪些类型被再导出。
//!
//! # 一次 JSON 往返
//!
//! 1. 写出：派生 `Serialize` 按字段声明顺序写；带 `serialize_with` 的数字字段走 JS 的
//!    `Number::toString`，带键序函数的 `Record` 字段走 JS 枚举序；`Option` 字段由
//!    `skip_serializing_if` 决定「省略键」。
//! 2. 读入：普通字段由派生实现处理；[`CodemodeResult`] 与 [`CodemodeOutcome`] 先把整个输入
//!    读成 [`serde_json::Value`]，用布尔 `ok` 选分支，再把同一个值反序列化成载荷结构体。
//! 3. 失败：字段类型不符、必填字段缺失都会变成 serde 的错误；两个判别联合在 `ok` 缺失或
//!    不是布尔时返回自定义错误（文本见 `read_ok`）。
//!
//! 反序列化默认忽略未知键（本文件没有 `deny_unknown_fields`）；但新增**必填**字段会让旧
//! 记录解析失败，新增可选字段要按现有模式加 `#[serde(default, skip_serializing_if = ...)]`
//! 才能同时覆盖「读出缺失键」与「写出省略键」。
//!
//! # 修改本文件的影响
//!
//! 字段名、字段顺序、`rename_all`/`rename` 与 `skip_serializing_if` 共同决定 JSON 字节。
//! 模块开头列出的 journal 记录与 worker/remote 协议都按这个形状读写，所以：新增可选字段
//! 要沿用「TS 的 `undefined` → 键缺失」的约定；新增 `f64` 字段必须挂 `serialize_js_number`，
//! 否则 `3.0`、`1e-6` 这类文本与 JS 不一致。
//!
//! # 阅读顺序
//!
//! serde 辅助（`serialize_js_number` … `deserialize_false`）→ 工具与输出
//! （[`CodemodeTool`]、[`CodemodeOutputItem`]、[`CodemodeCall`]）→ 结果
//! （[`CodemodeResult`]、[`CodemodeOutcome`]）→ 确定性 run（[`CodemodeRunOptions`]、
//! [`CodemodeRun`]）→ sandbox 选项（[`CodemodeSandboxBaseOptions`]、
//! [`CodemodeSandboxOptions`]）。

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio_util::sync::CancellationToken;

use crate::wasm::CodemodeWasmModule;

/// 异步盒装 future（与 `kiana-agent-core::BoxFuture` 同形；不跨 crate 复用，避免依赖）。
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// JS 语义的 serde 辅助（`ARCHITECTURE.md` §4.3 第 3、4 条；见模块文档）
//
// 这些函数是 `pub(crate)` 的：`runtime::protocol.rs` 的 `HostResult.now`、`VmStartData.memoryLimitBytes`
// 与 `VmDeterministic` 用同一套实现，避免 crate 里出现第二份数字/键序规则。它们不属于
// 公开面（`index.rs`/`portable.rs` 的再导出清单里没有，spec §3.3 也没有）。
// ---------------------------------------------------------------------------

/// 以 ECMAScript `Number::toString` 写一个 `f64`（`JSON.stringify` 对数字的文本）。
///
/// `NaN`/`±Infinity` 写成 `null`（JSON 里没有它们，正是 `JSON.stringify(NaN)` 的结果）。
/// 借助 `serde_json` 的 `raw_value` 把格式化后的文本原样嵌进输出：`RawValue` 在
/// `to_value` 与 `to_string` 两条路径上都保留数字类型（不会变成字符串）。
///
/// TS: 所有 `number` 字段的 `JSON.stringify` 输出。
pub(crate) fn serialize_js_number<S: Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    let text = kiana_json::js_num::format_js_number(*value);
    let raw = serde_json::value::RawValue::from_string(text).map_err(S::Error::custom)?;
    raw.serialize(serializer)
}

/// `Option<f64>` 的写出：`None` 由 `skip_serializing_if` 挡掉，`Some` 走 JS 数字文本。
pub(crate) fn serialize_optional_js_number<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serialize_js_number(value, serializer),
        // 不会走到（`skip_serializing_if` 先挡住），保留一个自洽的分支。
        None => serializer.serialize_none(),
    }
}

/// `Option<f64>` 的读入（接受整数形态的 JSON 数字）。
pub(crate) fn deserialize_optional_js_number<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(deserializer)
}

/// 以 JS 对象的键序写出一个字符串键的映射（整数索引键升序在前，其余插入序）。
///
/// `IndexMap` 的 serde 实现按插入序写；JS 的 `JSON.stringify` 先枚举整数索引键
/// （`0`…`4294967294`，无前导零）再枚举其余键。规则与 [`kiana_json::js_object`] 相同。
pub(crate) fn serialize_js_map<S: Serializer>(
    map: &IndexMap<String, serde_json::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serialize_js_keyed_map(map, serializer)
}

/// [`serialize_js_map`] 的泛型版本：任何字符串键的映射（`Record<string, T>`）。
///
/// 键集合完全来自 `map` 自身，因此按序 `get` 一定命中；若键序函数将来返回外来键，条目会被
/// `if let Some` 静默跳过（不会写错值）。
pub(crate) fn serialize_js_keyed_map<T: Serialize, S: Serializer>(
    map: &IndexMap<String, T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let keys = js_ordered_keys(map.keys().map(String::as_str));
    let mut out = serializer.serialize_map(Some(map.len()))?;
    for key in keys {
        if let Some(value) = map.get(key) {
            out.serialize_entry(key, value)?;
        }
    }
    out.end()
}

/// [`deserialize_js_map`] 的泛型版本：任何字符串键的映射。
pub(crate) fn deserialize_js_keyed_map<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<IndexMap<String, T>, D::Error> {
    let mut raw: IndexMap<String, T> = IndexMap::deserialize(deserializer)?;
    // 先取出顺序（借用），再按序把值移进新映射（`shift_remove` 保持未取键的相对序）。
    let order: Vec<String> = js_ordered_keys(raw.keys().map(String::as_str))
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut out = IndexMap::with_capacity(raw.len());
    for key in order {
        if let Some(value) = raw.shift_remove(&key) {
            out.insert(key, value);
        }
    }
    Ok(out)
}

/// 反序列化一个 `Record<string, unknown>`：与 `JSON.parse` 一样，把键排成 JS 的枚举序。
pub(crate) fn deserialize_js_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<IndexMap<String, serde_json::Value>, D::Error> {
    let raw: IndexMap<String, serde_json::Value> = IndexMap::deserialize(deserializer)?;
    Ok(js_reorder_map(raw))
}

/// `Option<Record<string, unknown>>` 的写出（`None` 由 `skip_serializing_if` 挡掉）。
pub(crate) fn serialize_optional_js_map<S: Serializer>(
    map: &Option<IndexMap<String, serde_json::Value>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match map {
        Some(map) => serialize_js_map(map, serializer),
        // 不会走到（`skip_serializing_if` 先挡住），保留一个自洽的分支。
        None => serializer.serialize_none(),
    }
}

/// `Option<Record<string, unknown>>` 的读入：`null` 与缺失都保持 `None`（TS 里 `properties?`
/// 只在 `!== undefined` 时才被序列化/使用，`null` 会被 TS 的类型守卫拒绝）。
pub(crate) fn deserialize_optional_js_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<IndexMap<String, serde_json::Value>>, D::Error> {
    let raw: Option<IndexMap<String, serde_json::Value>> = Option::deserialize(deserializer)?;
    Ok(raw.map(js_reorder_map))
}

/// 把一个映射的键就地重排为 JS 枚举序。
fn js_reorder_map(
    mut map: IndexMap<String, serde_json::Value>,
) -> IndexMap<String, serde_json::Value> {
    let order: Vec<String> = js_ordered_keys(map.keys().map(String::as_str))
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut out = IndexMap::with_capacity(map.len());
    for key in order {
        if let Some(value) = map.shift_remove(&key) {
            out.insert(key, value);
        }
    }
    out
}

/// `Object.keys` 顺序：整数索引键（`0..=4294967294`，无前导零）升序，其余保持插入序。
///
/// 与 `kiana_json::js_object::sort_js_keys` 的 `is_array_index` 判定一致；`kiana-json` 没有导出
/// 这个判定，而本 crate 只依赖它的公开 API，所以在这里保留同一份「JS 数组索引」规则
/// （规则本身来自 ECMA-262，不是实现细节）。
///
/// 例：`["b", "2", "1", "01"]` 排成 `["1", "2", "b", "01"]`；`"4294967295"`（`u32::MAX`）
/// 不是数组索引，因此留在普通键里。
fn js_ordered_keys<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut indices: Vec<(u32, &str)> = Vec::new();
    let mut others: Vec<&str> = Vec::new();
    for key in keys {
        match js_array_index(key) {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    // 只有整数索引键参与排序；普通键按遍历顺序（即 `IndexMap` 的插入序）原样接在后面。
    indices.sort_by_key(|(index, _)| *index);
    let mut out: Vec<&str> = Vec::with_capacity(indices.len() + others.len());
    out.extend(indices.into_iter().map(|(_, key)| key));
    out.extend(others);
    out
}

/// 是否是 JS 的「数组索引」键：`ToString(ToUint32(k)) === k && k !== "4294967295"`。
///
/// 先按长度与 ASCII 数字做快速排除（10 位以内的十进制数才可能落进 `u32`），再由
/// `parse::<u32>` 与 `u32::MAX` 比较完成判定；前导零（`"01"`）与空串都不是索引。
fn js_array_index(key: &str) -> Option<u32> {
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
    if value == u32::MAX {
        return None;
    }
    Some(value)
}

/// 反序列化 `unknown` + `undefined` 的字段：`null` 保留为 `Some(Value::Null)`，键缺失才是 `None`。
///
/// TS 的 `unknown` 三态（有值 / `null` / `undefined`）在 JSON 里是「值 / `null` / 键缺失」。
/// serde 的 `Option<Value>` 会把 `null` 也当成 `None`，于是 journal 重放时脚本返回的 `null`
/// 与「没有返回值」不可区分（TS 的 `JSON.parse` 不会丢 `null`）。
///
/// 使用它的字段必须同时带 `#[serde(default)]`：带 `deserialize_with` 的字段在键缺失时不会
/// 调用本函数，而是直接取字段类型的 `Default`（`Option` 即 `None`）；只有键存在时本函数
/// 才把 `null` 保留成 `Some(Value::Null)`。
pub fn deserialize_optional_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// 「键存在且恒为 `true`」的判别式字段反序列化（`{ ok: true, … }` 一支）。
///
/// 用于 `#[serde(untagged)]` 的两支枚举（`runtime/protocol.rs` 的 `WorkerDoneOutcome`）：
/// untagged 本身不识别判别式，`bool` 会同时接受两个值；校验字面量才能让「`ok` 与所在分支
/// 一致」成为解析条件。
pub(crate) fn deserialize_true<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    match bool::deserialize(deserializer)? {
        true => Ok(true),
        false => Err(serde::de::Error::custom("expected `ok` to be true")),
    }
}

/// 「键存在且恒为 `false`」的判别式字段反序列化（`{ ok: false, … }` 一支）。
///
/// 与 [`deserialize_true`] 对称：只接受 `false`，供 untagged 的失败分支使用。
pub(crate) fn deserialize_false<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    match bool::deserialize(deserializer)? {
        false => Ok(false),
        true => Err(serde::de::Error::custom("expected `ok` to be false")),
    }
}

/// `CodemodeJsonSchema = { [key: string]: unknown } | boolean`。
///
/// 只用于渲染声明（`declarations.rs`），**不校验值**。TS 的联合是对象或布尔（draft-04 的
/// `true`/`false` schema），JSON 值恰好能表示两者。
pub type CodemodeJsonSchema = serde_json::Value;

/// `CodemodeToolContext { signal: AbortSignal }`。
///
/// 脚本结束（含未 await 的调用）、执行超时、调用方 abort 或 sandbox 关闭时取消。
///
/// `runtime::execution` 只在 `execute()`（含 `execute_with_abort_reason`）路径的
/// `handle_call` 里为每次调用构造一个 context（工具与 globals 共用这套执行器），`signal`
/// 是该次调用自己的 `CancellationToken` 的 clone；执行结束时 `finish()` 会取消所有在途
/// 调用的令牌（`runtime/execution.rs`）。确定性 `start()` 路径的调用走 `forward_call`，
/// 不会构造本类型：调用方拿到的是 [`CodemodeHostCall`]，由 `deliver()` 回答。工具实现应把
/// 它传给可取消的下游操作。
#[derive(Debug, Clone)]
pub struct CodemodeToolContext {
    /// 取消令牌（TS: `context.signal`）。
    pub signal: CancellationToken,
}

/// `execute(args, context): Promise<unknown> | unknown` 的 Rust 形态。
///
/// `args` 是脚本传入值经过 JSON 往返后的结果；返回值必须是 JSON 可序列化值；
/// `Err(message)` 在脚本里表现为同 `message` 的 `Error`（TS 的 `errorMessage(thrown)`）。
///
/// # 失败模式
///
/// `runtime::execution` 在工具调用任务里同时接住同步阶段与 future 轮询中的 panic，并把它
/// 转成与 `Err` 相同的工具错误（`runtime/execution.rs` 的调用分派）。因此 panic 不会传播
/// 到执行任务之外；一次工具失败只影响这次调用，脚本可以选择捕获后继续。
///
/// # `undefined` 与 `null`（已冻结签名的已知差异）
///
/// TS 的 `unknown` 有三个状态：有值、`null`、`undefined`。脚本以无参形式调用
/// （`await tools.x()`）时 prelude 的 `serialize(args[0])` 得到 `undefined`，宿主把
/// `args` 原样交给 `execute`；`execute` 返回 `undefined` 时宿主送 `payload: undefined`，
/// 脚本再 `settle()` 成 `undefined`。冻结的 Rust 签名
/// `Fn(serde_json::Value, …) -> Result<serde_json::Value, String>` 只有两个状态，
/// `serde_json::Value` 无法表示 `undefined`，因此：
///
/// - 无参调用 → `Value::Null`（TS 是 `undefined`）；
/// - 返回 `Value::Null` → 脚本看到 `null`（TS 返回 `undefined` 时脚本看到 `undefined`）。
///
/// 可观察差异：脚本里 `(await tools.noop()) === undefined` 在 TS 为 `true`，Rust 侧为
/// `false`；`text(await tools.noop())` 在 TS 打印 `undefined`，Rust 侧打印 `null`。
/// 恢复它需要把签名换成 `Option<serde_json::Value>`（args 与返回值都改），这属于
/// `ARCHITECTURE.md` §4.9 冻结契约的变更，不在本单元权限内；记录在此供合并者决策。
/// 确定性 run 的 [`CodemodeHostCall::args`] 用 `Option`，不受影响。
pub type CodemodeToolExecute = Arc<
    dyn Fn(
            serde_json::Value,
            CodemodeToolContext,
        ) -> BoxFuture<'static, Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

/// `CodemodeTool`：脚本可见的一个工具（`tools.<id>` / `tools["<name>"]`）。
///
/// globals 与 tools 共用此类型：`spread`/`signature` 只对 globals 有意义。
#[derive(Clone)]
pub struct CodemodeTool {
    /// 工具名；脚本以 `tools.<jsName>`（见 `to_codemode_identifier`）与 `tools["<name>"]` 调用。
    pub name: String,
    /// `renderDeclarations` 的 doc comment，并出现在 `ALL_TOOLS`。
    pub description: Option<String>,
    /// 单参数 schema；缺省渲染为 `unknown`。
    pub input_schema: Option<CodemodeJsonSchema>,
    /// 解析值 schema；缺省渲染为 `unknown`。
    pub output_schema: Option<CodemodeJsonSchema>,
    /// 仅 globals：`execute` 收到全部调用参数的数组而非第一个。
    pub spread: Option<bool>,
    /// 仅 globals：声明用的 TS 参数表与返回类型，替代 schema 渲染。
    pub signature: Option<String>,
    /// 执行函数。
    pub execute: CodemodeToolExecute,
}

impl fmt::Debug for CodemodeTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodemodeTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("output_schema", &self.output_schema)
            .field("spread", &self.spread)
            .field("signature", &self.signature)
            .field("execute", &"<fn>")
            .finish()
    }
}

impl CodemodeTool {
    /// 只带名字与执行函数的最小构造；其余字段为 `None`。
    pub fn new(name: impl Into<String>, execute: CodemodeToolExecute) -> Self {
        CodemodeTool {
            name: name.into(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: None,
            signature: None,
            execute,
        }
    }
}

/// `CodemodeOutputItem`：脚本输出的一项，按产生顺序。
///
/// TS：`{ type: "text"; text: string; console?: true } | { type: "image"; data: string; mimeType: string }`。
/// `console` 只在 `console.*` 产生时为 `true`，其余省略（JSON 里无该键）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CodemodeOutputItem {
    /// `text()` 或 `console.*` 产生的一项。
    Text {
        /// 文本。
        text: String,
        /// `console.*` 产生的项为 `Some(true)`；其余省略。
        ///
        /// 字段类型是 `Option<bool>`：手工构造 `Some(false)` 会写出 `"console":false`，
        /// 这是 TS 的 `console?: true` 表达不了的状态（构造函数只产生 `Some(true)`/`None`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        console: Option<bool>,
    },
    /// `image()` 产生的一项；`data` 为 base64。
    Image {
        /// base64 数据。
        data: String,
        /// MIME 类型（由数据签名判定）。
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl CodemodeOutputItem {
    /// `text(value)` 的项。
    pub fn text(text: impl Into<String>) -> Self {
        CodemodeOutputItem::Text {
            text: text.into(),
            console: None,
        }
    }

    /// `console.*` 的项。
    pub fn console(text: impl Into<String>) -> Self {
        CodemodeOutputItem::Text {
            text: text.into(),
            console: Some(true),
        }
    }

    /// `image()` 的项。
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        CodemodeOutputItem::Image {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

/// `CodemodeCallStatus = "ok" | "error" | "cancelled"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeCallStatus {
    /// 工具正常返回。
    Ok,
    /// 工具抛错。
    Error,
    /// 执行结束（超时、abort、close）时仍未完成。
    Cancelled,
}

/// `CodemodeCall { name; status; durationMs }`。
///
/// `duration_ms` 用 `performance.now()` 差值，**不取整**（TS 侧只在渲染时 `Math.round`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeCall {
    /// 工具名。
    pub name: String,
    /// 结果状态。
    pub status: CodemodeCallStatus,
    /// 毫秒，浮点；JSON 文本按 JS 的 `Number::toString`（`3` 而不是 `3.0`）。
    #[serde(serialize_with = "serialize_js_number")]
    pub duration_ms: f64,
}

/// `CodemodeErrorKind = "script" | "timeout" | "aborted" | "sandbox"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeErrorKind {
    /// 脚本抛错或语法错误；`name`/`stack` 来自脚本的错误。
    Script,
    /// 总 deadline 到期，或调用线程上的 VM 用尽中断预算。
    Timeout,
    /// 调用方 signal 触发或 sandbox 被关闭。
    Aborted,
    /// VM 或其传输在脚本控制之外失败（wasm trap、worker 缺失、远端失败）。
    Sandbox,
}

/// `CodemodeError { kind; name?; message; stack? }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeError {
    /// 错误类别。
    pub kind: CodemodeErrorKind,
    /// 脚本错误的 `name`（`kind == "script"` 时通常存在）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 错误消息；对非脚本错误即 `Display` 文本。
    pub message: String,
    /// 脚本错误的栈（`kind == "script"` 时通常存在）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

impl std::fmt::Display for CodemodeError {
    /// 逐字等于 TS 的 `error.message`（ARCHITECTURE.md §4.1：公开错误的 `Display` == TS message）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodemodeError {}

impl CodemodeError {
    /// 非脚本类错误（`timeout`/`aborted`/`sandbox`）的构造：只有 `message`。
    pub fn new(kind: CodemodeErrorKind, message: impl Into<String>) -> Self {
        CodemodeError {
            kind,
            name: None,
            message: message.into(),
            stack: None,
        }
    }
}

/// `CodemodeStoreWrites { set; delete }`：脚本用 `store()` 改动的键。
///
/// 只有成功执行才报告写集合；`delete` 是以 `undefined` 存储的 key。
/// `set` 是 JS 的 `Record<string, unknown>`，因此 JSON 里整数索引键按升序排在前面
/// （`JSON.stringify` 的枚举序，而不是 `IndexMap` 的插入序）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodemodeStoreWrites {
    /// 新值（JSON 值）。
    #[serde(
        serialize_with = "serialize_js_map",
        deserialize_with = "deserialize_js_map"
    )]
    pub set: IndexMap<String, serde_json::Value>,
    /// 被删除的键。
    pub delete: Vec<String>,
}

/// `CodemodeResult`：一次执行的结果。
///
/// TS 是两个对象字面量联合，判别式 `ok`。序列化按变体形状；反序列化手写（serde 的
/// `untagged` 无法把 `ok` 当判别式）——先读 `ok` 布尔值再落到对应结构体，与 TS 的
/// `result.ok` 判别语义一致。
///
/// # 反序列化的两步
///
/// 输入先被完整读成 [`serde_json::Value`]，再用 `read_ok` 选择载荷，最后把同一个值
/// 反序列化成 [`CodemodeResultOk`] 或 [`CodemodeResultErr`]。这要求反序列化器支持
/// 自描述值（本 crate 只通过 `serde_json` 使用它）。
///
/// [`CodemodeResultOk::ok`]/[`CodemodeResultErr::ok`] 是普通 `bool`：直接反序列化载荷
/// 结构体时不会校验其取值，只有经过本枚举才会用 `ok` 选分支。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CodemodeResult {
    /// `{ ok: true, value, output, calls, storeWrites, during? }`。
    Ok(CodemodeResultOk),
    /// `{ ok: false, error, output, calls, during? }`。
    Err(CodemodeResultErr),
}

/// [`CodemodeResult::Ok`] 的载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeResultOk {
    /// 恒为 `true`。
    pub ok: bool,
    /// 脚本的返回值；`exit()` 与未返回值时为 `None`（TS 的 `undefined`，JSON 中省略）。
    ///
    /// 与 `"value": null` 不同：`null` 是脚本返回的 JSON 值，反序列化后是 `Some(Value::Null)`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_value"
    )]
    pub value: Option<serde_json::Value>,
    /// 到结束为止的输出项。
    pub output: Vec<CodemodeOutputItem>,
    /// 到结束为止的工具调用记录。
    pub calls: Vec<CodemodeCall>,
    /// `store()` 的写集合。
    pub store_writes: CodemodeStoreWrites,
    /// 确定性 run 中脚本结束时正在处理的结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<CodemodeDuring>,
}

/// [`CodemodeResult::Err`] 的载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeResultErr {
    /// 恒为 `false`。
    pub ok: bool,
    /// 失败原因。
    pub error: CodemodeError,
    /// 到失败点为止的输出项。
    pub output: Vec<CodemodeOutputItem>,
    /// 到失败点为止的工具调用记录。
    pub calls: Vec<CodemodeCall>,
    /// 确定性 run 中脚本结束时正在处理的结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<CodemodeDuring>,
}

impl CodemodeResultOk {
    /// 成功结果的载荷。
    pub fn new(
        value: Option<serde_json::Value>,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        store_writes: CodemodeStoreWrites,
        during: Option<CodemodeDuring>,
    ) -> Self {
        CodemodeResultOk {
            ok: true,
            value,
            output,
            calls,
            store_writes,
            during,
        }
    }
}

impl CodemodeResultErr {
    /// 失败结果的载荷。
    pub fn new(
        error: CodemodeError,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        during: Option<CodemodeDuring>,
    ) -> Self {
        CodemodeResultErr {
            ok: false,
            error,
            output,
            calls,
            during,
        }
    }
}

impl From<CodemodeResultOk> for CodemodeResult {
    fn from(value: CodemodeResultOk) -> Self {
        CodemodeResult::Ok(value)
    }
}

impl From<CodemodeResultErr> for CodemodeResult {
    fn from(value: CodemodeResultErr) -> Self {
        CodemodeResult::Err(value)
    }
}

impl CodemodeResult {
    /// 成功结果。
    pub fn success(
        value: Option<serde_json::Value>,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        store_writes: CodemodeStoreWrites,
        during: Option<CodemodeDuring>,
    ) -> Self {
        CodemodeResult::Ok(CodemodeResultOk {
            ok: true,
            value,
            output,
            calls,
            store_writes,
            during,
        })
    }

    /// 失败结果。
    pub fn failure(
        error: CodemodeError,
        output: Vec<CodemodeOutputItem>,
        calls: Vec<CodemodeCall>,
        during: Option<CodemodeDuring>,
    ) -> Self {
        CodemodeResult::Err(CodemodeResultErr {
            ok: false,
            error,
            output,
            calls,
            during,
        })
    }

    /// TS 的 `result.ok` 判别式。
    pub fn ok(&self) -> bool {
        matches!(self, CodemodeResult::Ok(_))
    }
}

/// `CodemodeOutcome`：`CodemodeRun.deliver()` 的回答。
///
/// 拒绝在脚本里变成带 `message` 的 `Error`，并附上 `properties` 的自有属性
/// （JSON 值；`name`/`message`/`stack` 被忽略）。反序列化按 `ok` 判别（同 [`CodemodeResult`]）。
///
/// 载荷里的 `ok` 也是普通 `bool`：直接反序列化 [`CodemodeOutcomeOk`] / [`CodemodeOutcomeErr`]
/// 不会校验它，经本枚举反序列化时才由 `read_ok` 选分支。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CodemodeOutcome {
    /// `{ ok: true; value }`（`value` 可为 `undefined`）。
    Ok(CodemodeOutcomeOk),
    /// `{ ok: false; message; properties? }`。
    Err(CodemodeOutcomeErr),
}

/// 判别式 `ok` 的取值：缺失或非布尔值都返回同一条自定义错误（文本固定为
/// `` missing field `ok` ``；`Value::as_bool` 对两者都给出 `None`，所以不区分「键缺失」
/// 与「类型不对」）。
fn read_ok(data: &serde_json::Value) -> Result<bool, String> {
    data.get("ok")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "missing field `ok`".to_string())
}

impl<'de> Deserialize<'de> for CodemodeResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let data = serde_json::Value::deserialize(deserializer)?;
        if read_ok(&data).map_err(serde::de::Error::custom)? {
            serde_json::from_value(data)
                .map(CodemodeResult::Ok)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(data)
                .map(CodemodeResult::Err)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl<'de> Deserialize<'de> for CodemodeOutcome {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let data = serde_json::Value::deserialize(deserializer)?;
        if read_ok(&data).map_err(serde::de::Error::custom)? {
            serde_json::from_value(data)
                .map(CodemodeOutcome::Ok)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(data)
                .map(CodemodeOutcome::Err)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// `CodemodeOutcomeOk` 的载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodemodeOutcomeOk {
    /// 恒为 `true`。
    pub ok: bool,
    /// 解析值；`None` 即 TS 的 `undefined`；`Some(Value::Null)` 是脚本得到的 `null`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_value"
    )]
    pub value: Option<serde_json::Value>,
}

/// [`CodemodeOutcome::Err`] 的载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodemodeOutcomeErr {
    /// 恒为 `false`。
    pub ok: bool,
    /// 脚本里 `Error` 的 `message`。
    pub message: String,
    /// 附加到 `Error` 上的自有属性（JS 对象的键序见 [`CodemodeStoreWrites::set`]）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_map",
        deserialize_with = "deserialize_optional_js_map"
    )]
    pub properties: Option<IndexMap<String, serde_json::Value>>,
}

/// `CodemodeDuring { seq; issued }`。
///
/// 确定性 run 中脚本结束时 VM 正在处理的结果号（首次运行前为 0）与到那时为止发出的调用数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodemodeDuring {
    /// 正在处理的结果号；0 表示脚本的首次运行。
    pub seq: u64,
    /// 累计发出的调用数（tools + globals）。
    pub issued: u64,
}

/// 调用目标：`"tool" | "global"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeCallTarget {
    /// `tools.*` 调用。
    Tool,
    /// 全局函数调用。
    Global,
}

/// `CodemodeHostCall`：确定性 run 中脚本发起的一次调用，由 `deliver()` 回答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodemodeHostCall {
    /// VM 自己的调用编号，按脚本发出顺序 1, 2, …（tools 与 globals 共用一个序列）。
    pub id: u64,
    /// 调用目标类别。
    pub target: CodemodeCallTarget,
    /// 工具名或全局名（原样，不是 jsName）。
    pub name: String,
    /// 脚本传入值经过 JSON 往返后的结果；spread global 是参数数组；无参数时 `None`。
    ///
    /// `"args": null` 是脚本显式传入的 `null`（`Some(Value::Null)`），与「没有参数」
    /// （`None`，TS 的 `undefined`）不同：TS 的 `JSON.stringify(call.args)` 给出 `"null"`
    /// 而不是 `undefined`（`packages/durable/src/codemode/codemode.ts` 的预览文本依赖它）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_value"
    )]
    pub args: Option<serde_json::Value>,
}

impl CodemodeDeclaredTool {
    /// 脚本使用的标识符：显式 `js_name`，否则 `to_codemode_identifier(name)`
    /// （TS: `t.jsName ?? toCodemodeIdentifier(t.name)`）。
    ///
    /// 确定性重放必须沿用 run 启动时算出的映射（spec §2.2.1）：重放时若按改名后的 `name`
    /// 重新计算，脚本看到的标识符会与首次运行不同。
    pub fn script_name(&self) -> String {
        match &self.js_name {
            Some(name) => name.clone(),
            None => crate::identifier::to_codemode_identifier(&self.name),
        }
    }
}

/// `CodemodeDeclaredTool`：`start()` run 的脚本看到的工具。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeDeclaredTool {
    /// 工具名。
    pub name: String,
    /// 脚本使用的标识符，默认 `to_codemode_identifier(name)`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js_name: Option<String>,
    /// `ALL_TOOLS` 里的描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `CodemodeDeclaredGlobal`：`start()` run 的脚本看到的全局函数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodemodeDeclaredGlobal {
    /// 全局名；可以带一个 `.` 形成命名空间成员。
    pub name: String,
    /// `execute` 是否收到参数数组（仅 TS 语义的记录；Rust 侧由宿主分派）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<bool>,
}

/// `CodemodeSandboxBaseOptions`：所有 sandbox 都接受的选项。
///
/// `Default` 全部为 `None`；默认值与校验在构造时补：`timeout_ms` 缺省取 300000
/// （`f64::INFINITY` 关闭 deadline），tools 重名与 globals 的保留名会让构造失败
/// （`CodemodeSandboxBase::new`，见 `runtime/execution.rs`）。
#[derive(Clone, Default)]
pub struct CodemodeSandboxBaseOptions {
    /// 以 `tools.<id>` 暴露的工具。
    pub tools: Option<Vec<CodemodeTool>>,
    /// 以顶层标识符暴露的全局函数；名字不得遮蔽内建（`tools`、`ALL_TOOLS`、`console`、
    /// `text`、`image`、`exit`、`store`、`load`，以及 `globalThis`）。
    pub globals: Option<Vec<CodemodeTool>>,
    /// 每次执行的总 deadline（含工具耗时）；默认 300000，`f64::INFINITY` 关闭。
    pub timeout_ms: Option<f64>,
    /// QuickJS VM 的内存上限；缺省为 wasm32 的 4 GiB 地址空间。
    pub memory_limit_bytes: Option<f64>,
}

impl fmt::Debug for CodemodeSandboxBaseOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodemodeSandboxBaseOptions")
            .field("tools", &self.tools)
            .field("globals", &self.globals)
            .field("timeout_ms", &self.timeout_ms)
            .field("memory_limit_bytes", &self.memory_limit_bytes)
            .finish()
    }
}

/// `acquireWorker()` 的返回：release 回调（worker 构造失败或真正退出时调用一次）。
///
/// 「一次」由 `runtime/host.rs` 的 `ReleaseOnce` 用 `Option::take` 保证；回调自身的 panic
/// 被 `catch_unwind` 吞掉，不会影响 worker 或执行结果。
pub type CodemodeWorkerRelease = Box<dyn Fn() + Send + Sync>;

/// `acquireWorker?(): () => void`：创建 worker 前同步预留容量。
///
/// # 不可失败的已知差异（冻结签名）
///
/// TS 的 `acquireWorker` 可以 `throw` 来拒绝准入：`host.ts` 让它同步抛出，launcher
/// 因此拒绝，整次执行以 `kind: "sandbox"` + 抛出文本结束。冻结的 Rust 签名返回
/// [`CodemodeWorkerRelease`] 而不是 `Result`，因此“拒绝准入”用 panic 表达：
/// `runtime::host` 用 `catch_unwind` 接住，效果与 TS 相同（不启动 worker、执行以
/// `kind: "sandbox"` + panic 文本失败），panic 不会展开到调用方。若要改成显式的
/// `Fn() -> Result<CodemodeWorkerRelease, String>`，属于签名变更，不在本单元权限内，
/// 记录在此供合并者决策。
pub type CodemodeAcquireWorker = Arc<dyn Fn() -> CodemodeWorkerRelease + Send + Sync>;

/// `onWorkerStarted?(exited: Promise<void>)`：worker 启动回调，`exited` 在真正退出时 resolve。
///
/// # 不可失败的已知差异（冻结签名）
///
/// TS 里该回调抛错会让执行失败并请求终止 worker（`host.ts`）。冻结的 Rust 签名返回
/// `()`，同样用 panic 表达失败：`runtime::host` 接住后请求终止 worker，并让执行以
/// `kind: "sandbox"` + panic 文本失败（panic 不会展开到调用方）。恢复一个
/// `Result<(), String>` 返回类型属于签名变更；记录在此供合并者决策。
pub type CodemodeWorkerStarted = Arc<dyn Fn(BoxFuture<'static, ()>) + Send + Sync>;

/// `CodemodeSandboxOptions`：worker 后端（`CodemodeSandbox`）的选项。
#[derive(Clone, Default)]
pub struct CodemodeSandboxOptions {
    /// 基类选项。
    pub base: CodemodeSandboxBaseOptions,
    /// 编译好的 `quickjs.wasm`；默认 `load_quickjs_wasm(None)`（嵌入的同一份）。
    pub wasm: Option<CodemodeWasmModule>,
    /// worker 入口说明符；`None` 表示本包自带的 worker。
    pub worker_url: Option<String>,
    /// 每次创建 worker 前**同步**调用；返回的 release 在 worker 构造失败或真正退出时调用一次。
    pub acquire_worker: Option<CodemodeAcquireWorker>,
    /// 每个由 `execute()`/`start()` 启动的 worker 调用一次；参数在该 worker 真正退出时 resolve。
    pub on_worker_started: Option<CodemodeWorkerStarted>,
}

impl fmt::Debug for CodemodeSandboxOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodemodeSandboxOptions")
            .field("base", &self.base)
            .field("wasm", &self.wasm.is_some())
            .field("worker_url", &self.worker_url)
            .field("acquire_worker", &self.acquire_worker.is_some())
            .field("on_worker_started", &self.on_worker_started.is_some())
            .finish()
    }
}

/// `CodemodeExecuteOptions`：`execute()` 的每次执行选项。
#[derive(Debug, Clone, Default)]
pub struct CodemodeExecuteOptions {
    /// 调用方取消信号。
    pub signal: Option<CancellationToken>,
    /// 覆盖 sandbox 的 deadline。
    ///
    /// `None` 时取 sandbox 的 `timeout_ms`（构造时的默认 300000；`f64::INFINITY` 关闭）。
    pub timeout_ms: Option<f64>,
    /// `load(key)` 的初值快照；必须 JSON 可序列化。
    ///
    /// 每个值在进入 VM 前先按 JS 的 `JSON.stringify` 规则写成文本（`serialize_store`）；
    /// `serialize_store` 只是按输入顺序构造 `VmStartData.store`，并不重排。按 JS 枚举序
    /// 写出 JSON 键序发生在序列化 `VmStartData` 时（`runtime/protocol.rs` 的
    /// `serialize_js_keyed_map`），`VmStartData.store` 这个 Rust 字段本身仍是插入序。
    pub store: Option<IndexMap<String, serde_json::Value>>,
}

/// `CodemodeRunOptions`：`start()`（可重放 run）的选项。
///
/// `tools`/`globals` 是本次 run 自己的声明：`start()` 不使用 sandbox 注册的工具与全局，
/// 所有调用都交给 `on_call`（见 `runtime/execution.rs` 的 `start`）。
///
/// # 必填字段的已知差异（冻结签名）
///
/// TS 的 `CodemodeRunOptions` 要求 `startedAt`、`seed`、`onCall` 三个字段
/// （漏掉 `onCall` 是类型错误），且 `execution.ts#start()` 会同步抛出：
/// `startedAt` 必须是有限数且非负，`seed` 必须是 `0..=0xffffffff` 内的整数。
/// 本结构 `derive(Default)` 且把回调都建模为 `Option`，因此 Rust 侧能构造出 TS
/// 无法表达的 run（`started_at = 0`、`seed = 0`、无调用 sink）。
/// 这些校验属于执行单元的 `start()` 路径（`runtime/execution.rs`），不在本单元；
/// 若要把必填性前移到构造器，同样涉及签名变更。记录在此供合并者决策。
#[derive(Clone, Default)]
pub struct CodemodeRunOptions {
    /// VM 时钟起点（ms since epoch）。
    pub started_at: f64,
    /// 32 位无符号整数，决定 `Math.random()` 与其它随机。
    pub seed: f64,
    /// 脚本看到的工具。
    pub tools: Option<Vec<CodemodeDeclaredTool>>,
    /// 脚本看到的全局函数。
    pub globals: Option<Vec<CodemodeDeclaredGlobal>>,
    /// `load()` 的初值快照。
    pub store: Option<IndexMap<String, serde_json::Value>>,
    /// 调用方取消信号。
    pub signal: Option<CancellationToken>,
    /// 本次 run 的 deadline（默认无，不是 sandbox 的默认）。
    pub timeout_ms: Option<f64>,
    /// 脚本运行前、任何调用之前收到指纹。
    pub on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// 脚本发起调用；用 [`CodemodeRun::deliver`] 回答。
    ///
    /// `None` 时，脚本一旦发起调用，执行就以 `kind: "sandbox"`、消息
    /// `Sandbox host failed: control.onCall is not a function` 结束
    /// （`runtime/execution.rs` 的 `forward_call`）；脚本从不发起调用时不会走到这条
    /// 失败路径，`start()` 可以正常完成。
    pub on_call: Option<Arc<dyn Fn(CodemodeHostCall) + Send + Sync>>,
    /// VM 处理完结果 `seq` 并再次等待时调用。
    pub on_idle: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
}

impl fmt::Debug for CodemodeRunOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodemodeRunOptions")
            .field("started_at", &self.started_at)
            .field("seed", &self.seed)
            .field("tools", &self.tools)
            .field("globals", &self.globals)
            .field("store", &self.store)
            .field("signal", &self.signal)
            .field("timeout_ms", &self.timeout_ms)
            .field("on_started", &self.on_started.is_some())
            .field("on_call", &self.on_call.is_some())
            .field("on_idle", &self.on_idle.is_some())
            .finish()
    }
}

/// `deliver(seq, now, id, outcome)` 的实现函数。
///
/// `Err(message)` 对应 TS 同步 `throw new Error(message)` 的四种校验错误
/// （见 spec §3.4「确定性交付校验」）。
///
/// `Ok(())` 表示回答已通过校验并交给投递路径（`runtime/execution.rs` 的 `post`），不代表
/// VM 已收到或应用它。`properties` 必须是对象的那条校验在 Rust 侧不可达
/// （`CodemodeOutcomeErr::properties` 只能是映射），实际可返回错误的只有 `now` 非有限、
/// `seq` 不是 1 起且只用一次、`id` 不在等待三种。
pub type CodemodeDeliver =
    Arc<dyn Fn(f64, f64, u64, CodemodeOutcome) -> Result<(), String> + Send + Sync>;

/// `CodemodeRun`：一个运行中的 `start()` 执行。
///
/// TS：`readonly result: Promise<CodemodeResult>; readonly finished: boolean;
/// deliver(seq, now, id, outcome): void`。`finished` 在结果决定时置位（早于 `result` settle），
/// 之后不再接受回答。
pub struct CodemodeRun {
    /// 结果 future。
    pub result: BoxFuture<'static, CodemodeResult>,
    finished: Arc<AtomicBool>,
    deliver: CodemodeDeliver,
}

impl CodemodeRun {
    /// 由 execution 层构造。
    ///
    /// `finished` 与 `deliver` 必须指向同一次执行（`runtime/execution.rs` 把同一个
    /// `Execution` 的标志与投递函数传进来）：本类型只做转发，不额外校验。
    pub fn new(
        result: BoxFuture<'static, CodemodeResult>,
        finished: Arc<AtomicBool>,
        deliver: CodemodeDeliver,
    ) -> Self {
        CodemodeRun {
            result,
            finished,
            deliver,
        }
    }

    /// TS 的 `readonly finished`：是否已有结局（脚本结束、超时、失败或 abort）。
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// TS 的 `deliver(seq, now, id, outcome)`：回答调用 `id`（结果号 `seq`，VM 时钟读数 `now`）。
    ///
    /// 校验由 `runtime::execution` 的实现完成：`now` 非有限、`seq` 不是从 1 起且未用过的
    /// 整数、`id` 不在等待时返回对应的 `Err(String)`；执行已经结束时返回 `Ok(())` 但不做
    /// 任何事（与 TS 的静默返回一致）。`Err` 是给调用方的，不会进入脚本。
    pub fn deliver(
        &self,
        seq: f64,
        now: f64,
        id: u64,
        outcome: CodemodeOutcome,
    ) -> Result<(), String> {
        (self.deliver)(seq, now, id, outcome)
    }
}

// 锁定本文件写出的 JSON 字节形状：字段顺序、camelCase/小写枚举与 `skip_serializing_if`
// 的省略规则，以及两个 `ok` 判别联合的往返。改任何 `#[serde(...)]` 属性前先跑这里；
// `tests/protocol.rs` 还有 Node 26 生成的差分语料。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_item_json_shape_matches_typescript() {
        assert_eq!(
            serde_json::to_string(&CodemodeOutputItem::text("hi")).unwrap(),
            r#"{"type":"text","text":"hi"}"#
        );
        assert_eq!(
            serde_json::to_string(&CodemodeOutputItem::console("hi")).unwrap(),
            r#"{"type":"text","text":"hi","console":true}"#
        );
        assert_eq!(
            serde_json::to_string(&CodemodeOutputItem::image("AA==", "image/png")).unwrap(),
            r#"{"type":"image","data":"AA==","mimeType":"image/png"}"#
        );
    }

    #[test]
    fn result_key_order_matches_typescript() {
        let result = CodemodeResult::success(
            Some(serde_json::json!({"a": 1})),
            vec![],
            vec![CodemodeCall {
                name: "t".into(),
                status: CodemodeCallStatus::Ok,
                duration_ms: 1.5,
            }],
            CodemodeStoreWrites::default(),
            None,
        );
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            r#"{"ok":true,"value":{"a":1},"output":[],"calls":[{"name":"t","status":"ok","durationMs":1.5}],"storeWrites":{"set":{},"delete":[]}}"#
        );

        let failure = CodemodeResult::failure(
            CodemodeError {
                kind: CodemodeErrorKind::Script,
                name: Some("TypeError".into()),
                message: "boom".into(),
                stack: Some("TypeError: boom".into()),
            },
            vec![],
            vec![],
            Some(CodemodeDuring { seq: 2, issued: 3 }),
        );
        assert_eq!(
            serde_json::to_string(&failure).unwrap(),
            r#"{"ok":false,"error":{"kind":"script","name":"TypeError","message":"boom","stack":"TypeError: boom"},"output":[],"calls":[],"during":{"seq":2,"issued":3}}"#
        );
    }

    #[test]
    fn result_round_trips_through_untagged() {
        let ok =
            CodemodeResult::success(None, vec![], vec![], CodemodeStoreWrites::default(), None);
        let text = serde_json::to_string(&ok).unwrap();
        assert_eq!(serde_json::from_str::<CodemodeResult>(&text).unwrap(), ok);

        let err = CodemodeResult::failure(
            CodemodeError::new(CodemodeErrorKind::Timeout, "Execution timed out after 1 ms"),
            vec![],
            vec![],
            None,
        );
        let text = serde_json::to_string(&err).unwrap();
        assert_eq!(serde_json::from_str::<CodemodeResult>(&text).unwrap(), err);
    }

    #[test]
    fn outcome_round_trips() {
        let ok = CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: Some(serde_json::json!([1])),
        });
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"ok":true,"value":[1]}"#
        );
        let err = CodemodeOutcome::Err(CodemodeOutcomeErr {
            ok: false,
            message: "nope".into(),
            properties: Some(IndexMap::from([(
                "structuredOutput".to_string(),
                serde_json::json!({"x": 1}),
            )])),
        });
        let text = serde_json::to_string(&err).unwrap();
        assert_eq!(
            text,
            r#"{"ok":false,"message":"nope","properties":{"structuredOutput":{"x":1}}}"#
        );
        assert_eq!(serde_json::from_str::<CodemodeOutcome>(&text).unwrap(), err);
    }
}
