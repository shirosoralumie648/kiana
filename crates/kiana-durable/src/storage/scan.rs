//! `storage/scan.ts`：内建存储扫描的起点解析与续页游标。
//!
//! 游标承载扫描顺序：给出游标时，查询重复或省略 `order` 都继续游标创建时的顺序；查询给出
//! **不同**的 `order` 抛出 [`StorageRequestError`]。没有 `order` 的旧游标按扫描的默认顺序继续。
//!
//! # 为什么单独成模块
//!
//! 会话/条目/任务/提交的扫描在多个后端上有同一份契约：游标如何续页、顺序冲突如何报错、
//! 错误消息逐字是什么。把起点解析收在一处后，`memory`（参考语义）与 `sqlite` 只需实现
//! 自己的取数与排序；`jsonl` 的读取路径直接转发给 `MemoryStorage`，因此自动继承这里的语义。
//! 本文件的语义与错误文案对应 `packages/durable/src/storage/scan.ts`。
//!
//! # 数据流
//!
//! 1. 调用方把上一页 `Page::next`（一个 [`Cursor`]，即 JSON 对象）原样传回后端；
//! 2. 后端调用 [`scan_start`]（从 JSON 边界接收查询时用 [`scan_start_json`]）得到
//!    [`ScanStart`]：扫描方向 `order` 与排他位置 `after`；
//! 3. 后端按 `order` 取排好序的 ID，从严格大于（升序）或严格小于（降序）`after` 的位置开始；
//! 4. 生成下一页时用 [`next_cursor`] 把本页最后一个 ID 与本页顺序写回游标。
//!
//! # 关键约束
//!
//! - `after` 是**排他**位置：游标指向的 ID 不会在下一页重复（`memory` 的实现用
//!   `upper_bound`/`lower_bound`；`sqlite` 的会话/任务/提交扫描用 `id > ?` / `id < ?`，
//!   条目扫描用等价的 `id >= after+1` / `id <= after-1`）。
//! - 游标里的 `after` 必须是 JS 安全整数（`|x| <= 2^53-1`）。校验显式复现
//!   `Number.isSafeInteger`，因为游标来自 JSON，可能是浮点、字符串或超出安全范围的整数。
//! - 游标存在但**缺少** `after` 是错误，不会退化为“从头开始”；文档扫描内部使用的
//!   `cursor_id` 语义不同（缺键表示从头），不要混用。
//! - 游标里的 `order` 可以缺失（旧游标），或恰为 `"ascending"` / `"descending"`；显式 `null`
//!   与其它任何值都报 `Invalid storage cursor`。
//! - 错误文案是当前稳定契约（改动需同步测试）：`Invalid storage cursor`、`Invalid scan order: ...` 与
//!   `The cursor continues a {order} scan; the query asks for {requested}`；一致性测试会按
//!   子串 `cursor` 断言顺序冲突。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::StorageRequestError;
use crate::types::{Cursor, JsonObject, JsonValue, ScanOrder};

/// 一次扫描的起点：顺序与上一页返回的最后一个 ID。
///
/// `after` 是排他位置：实现从严格大于（升序）或严格小于（降序）它的 ID 开始；`None` 表示
/// 从表的边界开始。ID 本身来自上一页，本次扫描不应再次返回它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStart {
    /// 扫描顺序。
    pub order: ScanOrder,
    /// 从哪个 ID 之后开始；`None` 表示从头。
    pub after: Option<i64>,
}

/// 把游标里的 `after` 解析成 `i64`，只接受 JS 的“安全整数”。
///
/// 游标来自 JSON，因此同一个数字可能是 `serde_json` 的整数或浮点表示；TS 侧只做
/// `typeof after === "number" && Number.isSafeInteger(after)`，这里必须得到同样的接受集合：
/// 有限、无小数部分、且落在 `[-(2^53-1), 2^53-1]`。其它类型（`null`、字符串、对象）返回
/// `None`，由调用方统一映射成 `Invalid storage cursor`。
fn is_safe_integer_number(value: &Value) -> Option<i64> {
    let Value::Number(number) = value else {
        return None;
    };
    if let Some(integer) = number.as_i64() {
        // JS 的 Number.isSafeInteger 上限是 2^53-1。
        // 用 |x| < 2^53 表达“<= 2^53-1”；unsigned_abs 对 i64::MIN 也不会溢出。
        return (integer.unsigned_abs() < (1u64 << 53)).then_some(integer);
    }
    // `5.0`、`1e3` 这类字面量在 serde_json 里是 f64，`as_i64` 返回 None；但它们在 JS 里
    // 仍是安全整数。先验证有限且无小数部分，再按 2^53-1 的闭区间做范围检查。
    let float = number.as_f64()?;
    if float.is_finite() && float.fract() == 0.0 && float.abs() <= 9_007_199_254_740_991.0 {
        return Some(float as i64);
    }
    None
}

/// 游标里的 `order` 字段（TS 里是 `unknown`，可以缺失或非法）。
///
/// 注意：显式 `null` 也算非法（TS 只把 `undefined` 当作缺失）。只接受两个精确的小写字符串；
/// 大小写不同或缩写都按非法游标处理。
fn parse_stored_order(value: Option<&JsonValue>) -> Result<Option<ScanOrder>, StorageRequestError> {
    match value {
        None => Ok(None),
        Some(Value::String(order)) if order == "ascending" => Ok(Some(ScanOrder::Ascending)),
        Some(Value::String(order)) if order == "descending" => Ok(Some(ScanOrder::Descending)),
        Some(_) => Err(StorageRequestError::new("Invalid storage cursor")),
    }
}

/// 解析请求里的 `order`（TS 的运行期校验；类型化的 Rust 调用方通常用 [`scan_start`]）。
///
/// # 参数与返回
///
/// - `None` 表示请求没有给出顺序，返回 `Ok(None)`，由 [`scan_start`] 用 `fallback` 补默认值；
/// - `Some(Value::String("ascending" | "descending"))` 返回对应枚举；
/// - 其它任何 JSON 值（数字、布尔、对象，以及显式 `null`）返回
///   `StorageRequestError("Invalid scan order: ...")`。
///
/// # 错误消息
///
/// 错误里的 `...` 用 `kiana_json` 的 JS `String(v)` 语义渲染：字符串原样（不带引号）、
/// `null` 写成 `null`、对象写成 `[object Object]`，这是当前稳定契约（改动需同步测试）。
pub fn parse_requested_order(
    requested: Option<&JsonValue>,
) -> Result<Option<ScanOrder>, StorageRequestError> {
    match requested {
        None => Ok(None),
        Some(Value::String(order)) if order == "ascending" => Ok(Some(ScanOrder::Ascending)),
        Some(Value::String(order)) if order == "descending" => Ok(Some(ScanOrder::Descending)),
        Some(other) => Err(StorageRequestError::new(format!(
            "Invalid scan order: {}",
            kiana_json::js_string::js_string(Some(other))
        ))),
    }
}

/// 解析扫描的顺序与位置。
///
/// 给出游标时继续游标创建时的顺序，查询给出不同 `order` 时抛错；没有顺序的旧游标按
/// `fallback` 继续。
///
/// # 参数
///
/// - `requested`：本次查询显式要求的顺序；`None` 表示查询没写 `order`。它只用来和游标的
///   有效顺序**核对**，不会覆盖游标里的顺序；游标缺少 `order` 时，有效顺序是 `fallback`。
/// - `cursor`：上一页返回的原始 JSON 游标；`None` 表示第一页。
/// - `fallback`：默认顺序，由各扫描 API 决定（例如 `MemoryStorage` 的会话扫描用升序、
///   条目扫描用降序）。没有游标时它就是本次扫描的顺序；有游标但游标缺少 `order` 时，
///   它作为该游标的有效顺序参与核对。
///
/// # 返回
///
/// 扫描方向与排他位置：`after: Some(id)` 表示从该 ID 之后开始，`None` 表示从头。
///
/// # 错误
///
/// 仅当给出游标时可能失败，消息逐字：
///
/// - 缺少 `after`，或它不是 JS 安全整数 → `Invalid storage cursor`；
/// - `order` 存在但不是两个合法字符串（显式 `null` 也算）→ `Invalid storage cursor`；
/// - `requested` 与游标的有效顺序不同（游标缺少 `order` 时有效顺序是 `fallback`）→
///   `The cursor continues a {order} scan; the query asks for {requested}`。
pub fn scan_start(
    requested: Option<ScanOrder>,
    cursor: Option<&Cursor>,
    fallback: ScanOrder,
) -> Result<ScanStart, StorageRequestError> {
    if let Some(cursor) = cursor {
        let after = cursor
            .get("after")
            .and_then(is_safe_integer_number)
            .ok_or_else(|| StorageRequestError::new("Invalid storage cursor"))?;
        let stored = parse_stored_order(cursor.get("order"))?;
        let order = stored.unwrap_or(fallback);
        if let Some(requested) = requested
            && requested != order
        {
            return Err(StorageRequestError::new(format!(
                "The cursor continues a {} scan; the query asks for {}",
                order.as_str(),
                requested.as_str()
            )));
        }
        return Ok(ScanStart {
            order,
            after: Some(after),
        });
    }
    Ok(ScanStart {
        order: requested.unwrap_or(fallback),
        after: None,
    })
}

/// 与 [`scan_start`] 相同，但 `requested` 是原始 JSON：非法值产生逐字的
/// `Invalid scan order: ${String(requested)}`。
///
/// 供从 JSON 边界接收查询的存储后端使用（Rust 的强类型调用方直接用 [`scan_start`]）。
///
/// 校验顺序固定：先校验 `requested`，再解析游标。因此“非法 `order` + 非法游标”同时
/// 出现时报 `Invalid scan order: ...`，而不是游标错误。本仓库当前没有其它调用点。
pub fn scan_start_json(
    requested: Option<&JsonValue>,
    cursor: Option<&Cursor>,
    fallback: ScanOrder,
) -> Result<ScanStart, StorageRequestError> {
    let requested = parse_requested_order(requested)?;
    scan_start(requested, cursor, fallback)
}

/// 最后一个返回项 ID 为 `id` 时，下一次扫描的游标。
///
/// 结果对象固定包含两个键，插入顺序为 `after`、`order`；`order` 写成 TS 的字符串值
/// （`"ascending"` / `"descending"`）。这里**不校验** `id` 的范围：若调用方给出
/// `|id| > 2^53-1`，把它回传给 [`scan_start`] 会被判为 `Invalid storage cursor`。
pub fn next_cursor(id: i64, order: ScanOrder) -> Cursor {
    let mut cursor = JsonObject::new();
    cursor.insert("after".to_owned(), Value::from(id));
    cursor.insert("order".to_owned(), Value::String(order.as_str().to_owned()));
    cursor
}

/// `nextCursor` 的返回形状（强类型版）：JSON 里就是 `{ after, order }`。
///
/// 序列化结果与 [`next_cursor`] 生成的 [`Cursor`] 同形：`after` 是数字，`order` 经
/// [`ScanOrder`] 的 serde 表示写成 `"ascending"` / `"descending"`。与 [`next_cursor`] 一样，
/// 本类型不校验 `after` 是否在安全整数范围内。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextCursor {
    /// 最后一个返回项的 ID。
    pub after: i64,
    /// 扫描顺序。
    pub order: ScanOrder,
}

/// [`next_cursor`] 的强类型版本。
///
/// 直接构造 [`NextCursor`]，不做任何校验（包括 `id` 的安全整数范围），语义与
/// [`next_cursor`] 相同。
pub fn next_cursor_value(id: i64, order: ScanOrder) -> NextCursor {
    NextCursor { after: id, order }
}
