//! `AuthStorage` / `FileAuthStorageBackend` / `ReadOnlyAuthStorage` / `InMemoryAuthStorageBackend`.
//!
//! CredentialStore implementation backed by `auth.json`（TS: `core/auth-storage.ts`）。
//!
//! 结构映射：
//! - [`FileAuthStorageBackend`]：`proper-lockfile` 目录锁 + `auth.json` 读写；同步路径 10×20 ms
//!   重试，异步路径 30 s 预算的指数退避（[`crate::auth::lock`] 提供锁语义）。
//! - [`InMemoryAuthStorageBackend`]：单进程内存 JSON 文本；异步操作以 `tokio::sync::Mutex`
//!   串行化并按 `AbortSignal` 取消排队项。
//! - [`AuthStorage`]：解析后的快照 + 进程内按路径共享的读状态（`getFileRevision` 合并并发读）。
//! - [`ReadOnlyAuthStorage`]：绝不创建文件的校验型只读实现（不执行 `!command`）。
//! - [`read_stored_credential`]：无锁、无创建的一次性同步读。
//!
//! 读合并（TS 的 `readState.reload`）在 Rust 侧用 `tokio::sync::watch` 承载「共享 promise」：
//! 首个读者启动真实重载，其余读者等待同一结果；最后一个读者离开时 abort 该次重载。
//!
//! **凭证投影**：`auth.json` 的条目在 TS 里是任意 JSON（`read`/`modify`/`list` 只读
//! `.type`/`.key`），而冻结的 `Credential` 只能是 `api_key`/`oauth`。读路径把无法表达的
//! 条目投影为 `None`（不抛错，见 `credential_from_value`），写路径仍按原始 JSON 逐字
//! 保留未修改的条目。
//!
//! # 为什么需要这个文件
//!
//! `auth.json` 是模型 provider 凭据的主要持久化来源（MCP OAuth 状态另存于
//! `mcp-auth.json`，复用本文件的后端，见「上下游」），但它是**外部可改**的文件：用户会手工
//! 编辑，旧版本或其它工具可能写入本实现不认识的条目。因此这里把它当作「任意 JSON 文本」而不是固定 schema：
//! - 读路径只投影本实现能表达的 `api_key`/`oauth`，其余条目不报错（见 `credential_from_value`）；
//! - 写路径在锁内重新解析磁盘内容、只改目标 provider，其余键（含未知形状、键序）逐字保留；
//! - 解析失败时抛出 `SyntaxError` 且**不覆盖**原文件（写路径先解析，再写）。
//!
//! 这也是 TS 版本的行为基线：`StorageDocument` 因此按 JS 属性语义实现取值、枚举、删除、
//! 赋值与对象展开，而不是用 `serde` 的严格结构。
//!
//! # 上下游
//!
//! - 上游调用方：`kiana_config::models::runtime` 构造 [`AuthStorage`] 作为 `Models` 的
//!   `CredentialStore`；`kiana-coding-agent` 的 `auth check` 在 `--no-refresh` 时改用
//!   [`ReadOnlyAuthStorage`]；`kiana-builtin-extensions` 的 MCP OAuth 状态存储复用
//!   [`FileAuthStorageBackend`] / [`InMemoryAuthStorageBackend`]。
//! - 下游依赖：[`crate::auth::lock`] 提供跨进程目录锁，`kiana_core::paths::get_file_revision`
//!   提供「文件是否变化」的不透明令牌，`kiana_json` 提供 V8 兼容的 JSON 解析与序列化。
//!
//! # 一次读与一次写
//!
//! 读（[`AuthStorage::read_credential`]）：`read_latest_data` 先比较 revision，命中快照就直接
//! 返回；否则经后端锁重载一次，再按 provider 取值、投影，并对 `api_key` 解析
//! `$ENV` / `!command`。同一路径的并发读者共享这一次重载。
//!
//! 写（[`AuthStorage::modify_credential`] / [`AuthStorage::delete_credential`]）：加后端锁
//! （文件后端是跨进程文件锁）→ 读并解析磁盘内容 → 在锁内调用回调 → 合并或删除 → 序列化
//! 整份文档 → 仍持锁写回 → 释放锁并更新进程内快照。因此共享同一个 `auth.json` 的读写不会
//! 互相覆盖。
//!
//! # 并发与失败模式
//!
//! - 进程内快照由 `std::sync::Mutex` 保护，只在同步临界区持有，不在 `await` 期间跨越。
//! - 共享重载用 `tokio::sync::watch` 传递结果，读者计数归零时 abort 该次重载；单个读者的
//!   取消不会打断其它读者。
//! - 文件后端的带 signal 写依次检查取消（进入回调前、写盘前）与锁破坏（进入回调前、写盘前、
//!   写盘后）；带 signal 的读在入口检查一次，并在等待共享重载时与取消赛跑。无 signal 的读
//!   在重载失败时回退到上一份快照，不会让调用方看到瞬时 IO 错误。
//! - 错误保持 TS 可观察形状：`AuthError::name`（`SyntaxError` / `TypeError` / `AbortError`…）
//!   与 `message` 逐字对齐；`AuthStorage`/`FileAuthStorageBackend` 的原始文件/锁错误另带
//!   `error_code`（`EACCES` / `EISDIR` / `ECOMPROMISED`…），调用方可以据此分支；
//!   `ReadOnlyAuthStorage` 包装后的错误只保留 message，`error_code` 为 `None`（只读路径
//!   没有原始文件/锁错误可映射）。
//!
//! # 阅读顺序
//!
//! 先看 `StorageDocument`（JS 属性语义的地基）与错误映射，再看 `FileAuthStorageBackend` /
//! `InMemoryAuthStorageBackend` 的锁语义，最后看 `AuthStorage` 的读合并与 CRUD 流程；
//! `ReadOnlyAuthStorage` 与 `read_stored_credential` 是两条独立的一次性读取路径。

use std::collections::HashMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    AuthError, AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialModifyFn,
    CredentialStore, throw_if_aborted,
};
use kiana_ai::utils::abort::{AbortScope, operation_signal, race_with_abort_signal};
use kiana_core::config::get_agent_dir;
use kiana_core::paths::{PathInputOptions, get_file_revision, normalize_path};
use kiana_core::text::strip_bom;
use serde_json::Value;
use tokio::sync::watch;

use crate::auth::lock::{self, LockError, LockOptions};
use crate::resolve_config_value::{is_command_config_value, resolve_config_value};

/// `type AuthStorageData = Record<string, Credential>`。
///
/// 原始 JSON 形状（保序）。AuthStorage 本身**不校验**文件内容（只有
/// [`ReadOnlyAuthStorage`] 校验），因此这里存放未解释的 `Value`：外部写入的未知字段、
/// 以及本实现无法反序列化的条目都能逐字写回。
pub type AuthStorageData = IndexMap<String, Value>;

/// `JSON.parse(stripBom(content))` 的顶层文档。
///
/// TS 的 `AuthStorageData = Record<string, Credential>` 只是对 `JSON.parse` 结果的类型断言：
/// `parseStorageData` 不校验顶层形状，数组 / 数字 / 字符串 / 布尔 / `null` 都会原样参与后续的
/// 属性读取、`Object.entries`、`delete` 与对象展开。Rust 侧用本枚举保留这个形状，
/// 并按 JS 的属性语义实现这些操作（[`ReadOnlyAuthStorage`] 仍按 TS 校验顶层必须是对象）。
#[derive(Debug, Clone, PartialEq)]
pub enum StorageDocument {
    /// JSON object（键序即 `Object.keys()` 顺序）。
    Object(AuthStorageData),
    /// JSON array。
    Array(Vec<Value>),
    /// JSON `null` / boolean / number / string。
    Scalar(Value),
}

/// `TypeError`（JS 属性访问 / 赋值失败的形态）。
///
/// 只设置 `name`；`message` 由调用方逐字提供——这些文案是 TS 测试锁定的可观察行为。
fn js_type_error(message: impl Into<String>) -> AuthError {
    AuthError::with_name("TypeError", message)
}

/// JS 数组索引键：`ToString(ToUint32(key)) === key` 且 `key < 2^32 - 1`。
///
/// 只有规范十进制、无前导零、且小于 `2^32 - 1` 的键才算索引（`"01"`、`"-1"`、`"length"`
/// 都不是）。JS 对这类键有独立于普通字符串键的枚举顺序，`StorageDocument` 的键序重排以及
/// 数组/字符串取值都依赖这个判定。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    (value < u64::from(u32::MAX)).then_some(value as u32)
}

/// 字符串的第 `index` 个 UTF-16 code unit（JS 字符串按 code unit 索引）。
///
/// 每次调用都会把整串重新编码成 UTF-16 序列。若索引落在代理对的一半上，
/// `from_utf16_lossy` 只能得到 U+FFFD：Rust 的 `String` 无法表示孤立代理，这是与 JS 的
/// 可观察差异（emoji 等非 BMP 字符按索引读取时会看到替换符）。
fn js_code_unit(text: &str, index: u32) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    units
        .get(index as usize)
        .map(|unit| String::from_utf16_lossy(&[*unit]))
}

/// `Object.keys({...data, [key]: value})` 的键序：整数索引键升序在前，其余保持插入序。
///
/// JS 对象自身属性的枚举顺序就是这条规则。函数重建整张表是为了按「整数键升序 + 其余插入
/// 序」分组，而不是因为 `IndexMap` 缺少排序 API（它提供 `sort_by`/`sort_keys` 等原地重排
/// 方法）。调用方：内部写路径 `set_property`/`set_spread` 在插入后调用，`from_data` 在
/// 构造时调用。`auth.json` 的黄金字节测试实际覆盖的是 `parse`/`to_json_string` 的键序，
/// 不经过本函数。
fn js_order_data(data: AuthStorageData) -> AuthStorageData {
    let mut indices: Vec<(u32, String, Value)> = Vec::new();
    let mut others: AuthStorageData = AuthStorageData::new();
    for (key, value) in data {
        match js_array_index(&key) {
            Some(index) => indices.push((index, key, value)),
            None => {
                others.insert(key, value);
            }
        }
    }
    indices.sort_by_key(|(index, ..)| *index);
    let mut ordered = AuthStorageData::with_capacity(indices.len() + others.len());
    for (_, key, value) in indices {
        ordered.insert(key, value);
    }
    ordered.extend(others);
    ordered
}

impl StorageDocument {
    /// 由普通对象数据构造（键序按 JS 规则重排）。
    ///
    /// 输入里可能含整数键，JS 在输出与枚举时会把它们排到最前，因此这里先统一顺序。`parse`
    /// 与对象展开在内部做同样的排序，但各自直接构造，不经过本函数。
    pub fn from_data(data: AuthStorageData) -> Self {
        Self::Object(js_order_data(data))
    }

    /// `parseStorageData(content)` / `FileModelsStore.parse(content)`：
    /// 空内容 → `{}`；否则 `JSON.parse(stripBom(content))`（**不校验**顶层形状）。
    ///
    /// `content = None` 表示文件不存在，与空串一样按 `{}` 处理；BOM 只在解析前剥离。
    /// 解析失败的错误名是 `SyntaxError`。错误消息是当前稳定契约：与 V8 `JSON.parse`
    /// 的文案逐字一致（[`kiana_json::js_parse`]），改动需同步测试。
    ///
    /// 返回的是未校验的文档：数组/标量会在后续 `get`/`entries`/`delete` 里按对应 JS 语义
    /// 处理（见各方法）。需要“顶层必须是对象”的调用方须自行检查：[`ReadOnlyAuthStorage`]
    /// 直接匹配 `Value::Object`（不走本枚举），[`crate::auth::models_store`] 的
    /// `FileModelsStore` 写路径则用 [`StorageDocument::as_object`]。
    pub fn parse(content: Option<&str>) -> Result<Self, AuthError> {
        let Some(content) = content else {
            return Ok(Self::Object(AuthStorageData::new()));
        };
        if content.is_empty() {
            return Ok(Self::Object(AuthStorageData::new()));
        }
        let parsed = kiana_json::js_parse::parse(&strip_bom(content))
            .map_err(|error| AuthError::with_name("SyntaxError", error.message))?;
        Ok(Self::from_value(parsed))
    }

    // 解析路径不需要再排序：`kiana_json::js_parse::parse` 在返回前已把每个对象按 JS 键序
    // 重排（`preserve_order` 只用于解析期间保留文本顺序）；序列化时 `stringify` 也按 JS
    // 键序输出。因此这里直接收集 `Value::Object` 的迭代序即可与 TS 字节一致。
    fn from_value(value: Value) -> Self {
        match value {
            Value::Object(map) => Self::Object(map.into_iter().collect()),
            Value::Array(items) => Self::Array(items),
            scalar => Self::Scalar(scalar),
        }
    }

    /// 顶层是对象时返回其键值表。
    pub fn as_object(&self) -> Option<&AuthStorageData> {
        match self {
            Self::Object(data) => Some(data),
            Self::Array(_) | Self::Scalar(_) => None,
        }
    }

    /// `JSON.stringify(document, null, 2)`（JS 键序 + JS 数字格式）。
    ///
    /// 结果是写回磁盘的**完整文件内容**，不带结尾换行。对象分支先把保序表转成
    /// `serde_json::Map`，再交给 JS 兼容的 pretty 打印；数组与标量直接交给同一打印器。
    pub fn to_json_string(&self) -> String {
        match self {
            Self::Object(data) => stringify_json(&entries_to_value(data)),
            Self::Array(items) => {
                kiana_json::js_object::stringify_pretty(&Value::Array(items.clone()), 2)
            }
            Self::Scalar(value) => kiana_json::js_object::stringify_pretty(value, 2),
        }
    }

    /// `Object.entries(document)`。
    ///
    /// 与 JS 一致的边界：`null` 抛 `TypeError`；字符串按 UTF-16 code unit 拆成索引键；
    /// 数字/布尔展开为空表；数组按索引字符串给出。`list` 依赖这里的键序与逐项语义。
    pub(crate) fn entries(&self) -> Result<Vec<(String, Value)>, AuthError> {
        match self {
            Self::Object(data) => Ok(data
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()),
            Self::Array(items) => Ok(items
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone()))
                .collect()),
            Self::Scalar(Value::Null) => {
                Err(js_type_error("Cannot convert undefined or null to object"))
            }
            Self::Scalar(Value::String(text)) => Ok(text
                .encode_utf16()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        index.to_string(),
                        Value::String(String::from_utf16_lossy(&[unit])),
                    )
                })
                .collect()),
            Self::Scalar(_) => Ok(Vec::new()),
        }
    }

    /// `document[key]`。
    ///
    /// - 对象：缺失键 → `None`（JS 的 `undefined`）；
    /// - 数组：`length` 返回元素数，整数索引返回元素，其余键（含命名属性）不可见；
    /// - 字符串：`length` 是 UTF-16 code unit 数，索引按 code unit 取值；
    /// - `null`：任何读取都抛 `TypeError`（文案含键名）；其它标量返回 `None`。
    ///
    /// 返回 `Result` 而不是 `Option`，是因为 `null`（抛错）与「没有该属性」（`undefined`）
    /// 在 TS 里行为不同。
    pub(crate) fn get(&self, key: &str) -> Result<Option<Value>, AuthError> {
        match self {
            Self::Object(data) => Ok(data.get(key).cloned()),
            Self::Array(items) => {
                if key == "length" {
                    return Ok(Some(Value::from(items.len())));
                }
                Ok(js_array_index(key).and_then(|index| items.get(index as usize).cloned()))
            }
            Self::Scalar(Value::Null) => Err(js_type_error(format!(
                "Cannot read properties of null (reading '{key}')"
            ))),
            Self::Scalar(Value::String(text)) => {
                if key == "length" {
                    return Ok(Some(Value::from(text.encode_utf16().count())));
                }
                let Some(index) = js_array_index(key) else {
                    return Ok(None);
                };
                Ok(js_code_unit(text, index).map(Value::String))
            }
            Self::Scalar(_) => Ok(None),
        }
    }

    /// `delete document[key]`（数组的索引删除留下 `null` 空洞，与 `JSON.stringify` 一致）。
    ///
    /// 对象用 `shift_remove` 删除并保持其余键序；数组索引删除把槽位置为 `null`（长度不变）；
    /// 数组/字符串的 `length` 与字符串的索引属性不可删除，抛 `TypeError`；`null` 抛
    /// `TypeError`。删除不存在的键静默成功——与 JS 一致。
    pub(crate) fn delete(&mut self, key: &str) -> Result<(), AuthError> {
        match self {
            Self::Object(data) => {
                data.shift_remove(key);
                Ok(())
            }
            Self::Array(items) => {
                if key == "length" {
                    return Err(js_type_error(
                        "Cannot delete property 'length' of [object Array]",
                    ));
                }
                if let Some(index) = js_array_index(key)
                    && let Some(slot) = items.get_mut(index as usize)
                {
                    *slot = Value::Null;
                }
                Ok(())
            }
            Self::Scalar(Value::Null) => {
                Err(js_type_error("Cannot convert undefined or null to object"))
            }
            Self::Scalar(Value::String(text)) => {
                let length = text.encode_utf16().count() as u32;
                if key == "length" || js_array_index(key).is_some_and(|index| index < length) {
                    return Err(js_type_error(format!(
                        "Cannot delete property '{key}' of [object String]"
                    )));
                }
                Ok(())
            }
            Self::Scalar(_) => Ok(()),
        }
    }

    /// `document[key] = value`（`FileModelsStore.write` 的直接赋值；数组的命名属性会被
    /// `JSON.stringify` 忽略，与 JS 一致）。
    ///
    /// 具体规则：对象赋值后按 JS 键序重排；数组的整数索引会先用 `null` 填补空洞再写入，
    /// 赋 `length` 抛 `RangeError`，命名属性静默丢弃；`null` 抛 `TypeError`；字符串/数字/
    /// 布尔的属性赋值抛 `TypeError`（字符串的已有索引是只读属性，其余键是新增属性，两种
    /// 文案不同）。
    ///
    /// 调用方是 `FileModelsStore`：它把解析出的文档当作 JS 对象直接赋值，因此必须保留这套
    /// 属性语义，否则写出的字节会与 TS 不同。
    pub(crate) fn set_property(&mut self, key: &str, value: Value) -> Result<(), AuthError> {
        match self {
            Self::Object(data) => {
                data.insert(key.to_string(), value);
                *data = js_order_data(std::mem::take(data));
                Ok(())
            }
            Self::Array(items) => {
                if key == "length" {
                    return Err(AuthError::with_name("RangeError", "Invalid array length"));
                }
                if let Some(index) = js_array_index(key) {
                    let index = index as usize;
                    while items.len() <= index {
                        items.push(Value::Null);
                    }
                    items[index] = value;
                }
                Ok(())
            }
            Self::Scalar(Value::Null) => Err(js_type_error(format!(
                "Cannot set properties of null (setting '{key}')"
            ))),
            Self::Scalar(Value::String(text)) => {
                let length = text.encode_utf16().count() as u32;
                if js_array_index(key).is_some_and(|index| index < length) {
                    Err(js_type_error(format!(
                        "Cannot assign to read only property '{key}' of string '{text}'"
                    )))
                } else {
                    Err(js_type_error(format!(
                        "Cannot create property '{key}' on string '{text}'"
                    )))
                }
            }
            Self::Scalar(Value::Number(number)) => Err(js_type_error(format!(
                "Cannot create property '{key}' on number '{}'",
                kiana_json::js_num::format_js_number(number.as_f64().unwrap_or(f64::NAN))
            ))),
            Self::Scalar(Value::Bool(value)) => Err(js_type_error(format!(
                "Cannot create property '{key}' on boolean '{value}'"
            ))),
            Self::Scalar(_) => Ok(()),
        }
    }

    /// `{...document, [key]: value}`（`AuthStorage.modify` 的合并方式；数组/字符串展开为
    /// 以索引为键的普通对象，标量展开为空对象）。
    ///
    /// 与 [`StorageDocument::set_property`] 的差异：展开**永远**产出对象，因此 `modify` 能把
    /// 数组/标量顶层文档修复成对象；键序在合并后按 JS 规则重排。无法展开的输入不会失败。
    fn set_spread(&self, key: &str, value: Value) -> Result<Self, AuthError> {
        let mut merged = match self {
            Self::Object(data) => data.clone(),
            Self::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value.clone()))
                .collect(),
            Self::Scalar(Value::String(text)) => text
                .encode_utf16()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        index.to_string(),
                        Value::String(String::from_utf16_lossy(&[unit])),
                    )
                })
                .collect(),
            Self::Scalar(_) => AuthStorageData::new(),
        };
        merged.insert(key.to_string(), value);
        Ok(Self::Object(js_order_data(merged)))
    }
}

/// `type LockResult<T> = { result: T; next?: string }`。
///
/// 回调用它区分「只读」与「读改写」：
/// - `result`：返回给调用方的值；
/// - `next`：**整份**新的文件内容（不是 patch）。`Some` 时后端在仍持锁的情况下覆盖写入；
///   `None` 时不动文件。
///
/// 两者只在「加锁 → 读 → 回调 → 可选写」全部成功后才会被调用方看到；任一步失败都会丢弃
/// `result` 并返回错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockResult<T> {
    /// 回调返回值。
    pub result: T,
    /// 非 `None` 时在仍持锁时写回文件。
    pub next: Option<String>,
}

impl<T> LockResult<T> {
    /// `{ result }`（不写回）。
    pub fn new(result: T) -> Self {
        Self { result, next: None }
    }

    /// `{ result, next }`（持锁写回）。
    pub fn with_next(result: T, next: String) -> Self {
        Self {
            result,
            next: Some(next),
        }
    }
}

/// 异步锁内回调返回的 boxed future（`Send`，`'static`：可跨 `raceWithAbortSignal` 的 spawn）。
///
/// 固定为 boxed `'static` 是因为回调会离开调用方的栈帧：`InMemoryAuthStorageBackend` 用
/// `race_with_abort_signal` 把整个操作 spawn 到运行时，`FileAuthStorageBackend` 则要在锁的
/// 生命周期内 await 它。错误类型与同步回调一致，便于两个后端互相替换。
pub type LockFuture<T> =
    Pin<Box<dyn Future<Output = Result<LockResult<T>, AuthError>> + Send + 'static>>;

/// 取 `std::sync::Mutex` 的锁；中毒（此前有持有者 panic）时取回内部值。
///
/// 忽略中毒是为了避免一次 panic 让后续所有凭证读写永久失败。被保护的值里，
/// `AuthReadState`/`ReadOnly` 的缓存可以从磁盘重建，但 `InMemoryAuthStorageBackend` 的
/// `value` 是该份 JSON 文本在进程内的唯一副本，没有其它来源可恢复；这里统一选择继续运行
/// 而不是把中毒扩散给调用方。
fn lock_state<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `signal?.throwIfAborted()`。
///
/// `None` 表示该操作不可取消（不是“沿用调用方的信号”），直接返回 `Ok`。
fn throw_if_aborted_opt(signal: Option<&AbortScope>) -> Result<(), AuthError> {
    match signal {
        Some(signal) => throw_if_aborted(signal),
        None => Ok(()),
    }
}

/// `signal.reason` 缺省时的 `AbortError`（`abortReason()`）。
///
/// reason 的 `name`/`message` 原样保留（例如 `TimeoutError` 不会被改写成 `AbortError`）；
/// 没有 reason 时用 TS `abortReason()` 的回退文案 "The operation was aborted"，而不是
/// DOMException 的 "This operation was aborted"。
fn abort_error_from_signal(signal: Option<&AbortScope>) -> AuthError {
    match signal.and_then(AbortScope::reason) {
        Some(reason) => AuthError::with_name(reason.name, reason.message),
        None => AuthError::with_name("AbortError", "The operation was aborted"),
    }
}

/// libuv `uv_strerror` 的文案（`uv-common.c`；`auth/lock.rs` 为锁错误维护同一张表）。
#[cfg(unix)]
const UNIX_ERRNO_PARTS: &[(i32, &str, &str)] = &[
    (libc::E2BIG, "E2BIG", "argument list too long"),
    (libc::EACCES, "EACCES", "permission denied"),
    (libc::EBUSY, "EBUSY", "resource busy or locked"),
    (libc::EDQUOT, "EDQUOT", "disk quota exceeded"),
    (libc::EEXIST, "EEXIST", "file already exists"),
    (libc::EFAULT, "EFAULT", "bad address"),
    (libc::EIO, "EIO", "i/o error"),
    (libc::EISDIR, "EISDIR", "illegal operation on a directory"),
    (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
    (libc::EMFILE, "EMFILE", "too many open files"),
    (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
    (libc::ENFILE, "ENFILE", "file table overflow"),
    (libc::ENOENT, "ENOENT", "no such file or directory"),
    (libc::ENOMEM, "ENOMEM", "not enough memory"),
    (libc::ENOSPC, "ENOSPC", "no space left on device"),
    (libc::ENOTDIR, "ENOTDIR", "not a directory"),
    (libc::ENOTEMPTY, "ENOTEMPTY", "directory not empty"),
    (libc::EPERM, "EPERM", "operation not permitted"),
    (libc::EROFS, "EROFS", "read-only file system"),
    (libc::ESPIPE, "ESPIPE", "invalid seek"),
    (libc::ETXTBSY, "ETXTBSY", "text file busy"),
    (libc::EXDEV, "EXDEV", "cross-device link not permitted"),
];

/// Windows：`io::Error::raw_os_error()` 是 Win32 错误码；映射到 libuv 的 errno 名称与文案。
#[cfg(windows)]
const WINDOWS_ERROR_PARTS: &[(i32, &str, &str)] = &[
    (2, "ENOENT", "no such file or directory"), // ERROR_FILE_NOT_FOUND
    (3, "ENOENT", "no such file or directory"), // ERROR_PATH_NOT_FOUND
    (5, "EACCES", "permission denied"),         // ERROR_ACCESS_DENIED
    (19, "EROFS", "read-only file system"),     // ERROR_WRITE_PROTECT
    (32, "EBUSY", "resource busy or locked"),   // ERROR_SHARING_VIOLATION
    (33, "EBUSY", "resource busy or locked"),   // ERROR_LOCK_VIOLATION
    (112, "ENOSPC", "no space left on device"), // ERROR_DISK_FULL
    (145, "ENOTEMPTY", "directory not empty"),  // ERROR_DIR_NOT_EMPTY
    (183, "EEXIST", "file already exists"),     // ERROR_ALREADY_EXISTS
    (206, "ENAMETOOLONG", "name too long"),     // ERROR_FILENAME_EXCED_RANGE
    (267, "ENOTDIR", "not a directory"),        // ERROR_DIRECTORY
];

/// 把底层 `io::Error` 转成 Node/libuv 的 `(code, message)`。
///
/// raw 码在 libuv 表里时使用 errno 名（`ErrorKind` 会丢失区分：例如 `EPERM` 与 `EACCES`
/// 在 Rust 里同为 `PermissionDenied`）；raw 码缺失**或不在表中**（如 `EINVAL`/`EAGAIN`）
/// 时退回 `ErrorKind`（本函数的 fallback 只映射 NotFound/PermissionDenied/AlreadyExists/
/// TimedOut；`EROFS`/`ENOTDIR` 的 kind 存在但未被映射），最后兜底 `UNKNOWN`；消息来自
/// libuv 表或 `Display`（去掉 ` (os error N)` 后缀并小写首字母）。
fn node_error_parts(error: &io::Error) -> (&'static str, String) {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, code, message)) = UNIX_ERRNO_PARTS.iter().find(|(errno, ..)| *errno == raw)
    {
        return (code, (*message).to_string());
    }
    #[cfg(windows)]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, code, message)) =
            WINDOWS_ERROR_PARTS.iter().find(|(errno, ..)| *errno == raw)
    {
        return (code, (*message).to_string());
    }

    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        // 无 raw errno 的合成错误（测试/非 OS 来源）：保持 `getRetryableLockErrorCode` 的平台
        // 语义——Windows 上 PermissionDenied 视作 EPERM，其它平台 EACCES。
        io::ErrorKind::PermissionDenied => {
            if cfg!(windows) {
                "EPERM"
            } else {
                "EACCES"
            }
        }
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::TimedOut => "ETIMEDOUT",
        _ => "UNKNOWN",
    };
    let display = error.to_string();
    let message = display
        .rsplit_once(" (os error ")
        .map_or(display.as_str(), |(message, _)| message);
    (code, lowercase_first(message))
}

/// Node 风格的文件错误文案：
/// - `EACCES: permission denied, open '/path'`
/// - `EISDIR: illegal operation on a directory, read`（`read`/`write` 阶段 libuv 不带 path）
pub(crate) fn node_io_error(syscall: &str, path: &Path, error: &io::Error) -> String {
    let (code, message) = node_error_parts(error);
    if syscall == "read" || syscall == "write" {
        format!("{code}: {message}, {syscall}")
    } else {
        format!("{code}: {message}, {syscall} '{}'", path.display())
    }
}

/// `readFileSync` 的失败点：`open` 阶段（带 path）或 `read(2)` 阶段
/// （EISDIR；Node 的 syscall 是 `read` 且不带 path）。
///
/// EISDIR 的判定是 Unix 专属的；其它平台一律返回 `open`（没有等价的 errno 特判）。
pub(crate) fn read_syscall(error: &io::Error) -> &'static str {
    #[cfg(unix)]
    {
        if error.raw_os_error() == Some(libc::EISDIR) {
            return "read";
        }
        let _ = error;
    }
    #[cfg(not(unix))]
    let _ = error;
    "open"
}

/// 把首字符转成小写：libuv 的错误描述（`no such file or directory` 等）全部以小写开头，
/// 而回退用的 `io::Error::to_string()` 大小写不保证一致。
fn lowercase_first(message: &str) -> String {
    let mut message = message.to_string();
    if let Some(first) = message.chars().next() {
        let lowered = first.to_lowercase().to_string();
        message.replace_range(..first.len_utf8(), &lowered);
    }
    message
}

/// 构造 Node 风格的文件错误：`Display` 是 `CODE: message, syscall`（`read`/`write` 不带路径，
/// 其余 syscall 带 `'path'`），`error_code` 保留 `CODE` 供调用方分支（对应 TS 的
/// `error.code`）。
fn io_auth_error(syscall: &str, path: &Path, error: io::Error) -> AuthError {
    // Node 的 fs 错误带 `error.code`（`EACCES`/`EPERM`/`EISDIR`…），调用方可以据此分支。
    let code = node_error_parts(&error).0;
    AuthError::new(node_io_error(syscall, path, &error)).with_error_code(code)
}

/// 把 [`LockError`] 转成 TS 侧可观察的 `Error`（`Display` == `error.message`）。
///
/// - `Aborted`：用**调用方的 signal** 重建 reason（保留 `name`/`message`），而不是固定文案；
/// - `Io`：message 来自 `Display`（`CODE: message, syscall 'path'`），`error_code` 用底层
///   errno 名；errno 名来自 `node_error_parts` 的表，raw 码缺失或不在表中时按 `ErrorKind`
///   fallback 归类（见该函数文档）；
/// - 其余变体：message 是锁的英文文案，`error_code` 分别是
///   `ELOCKED`/`ERELEASED`/`ENOTACQUIRED`/`ECOMPROMISED`。
///
/// `modify`/`delete` 会把这个错误原样返回；`read` 仅在带 signal 时原样返回，无 signal 时
/// 回退旧快照（见 `read_latest_data`）。因此 name/message/code 是公开可观察契约。
fn lock_auth_error(error: LockError, signal: Option<&AbortScope>) -> AuthError {
    match &error {
        LockError::Aborted => abort_error_from_signal(signal),
        LockError::Locked { .. } => AuthError::new(error.to_string()).with_error_code("ELOCKED"),
        LockError::Released { .. } => {
            AuthError::new(error.to_string()).with_error_code("ERELEASED")
        }
        LockError::NotAcquired { .. } => {
            AuthError::new(error.to_string()).with_error_code("ENOTACQUIRED")
        }
        LockError::Compromised { .. } => {
            AuthError::new(error.to_string()).with_error_code("ECOMPROMISED")
        }
        LockError::Io { source, .. } => {
            // Node 的 `error.code` 来自底层 errno（EPERM/EACCES/EISDIR/EROFS/ENOTDIR…），
            // 不能只按 `io::ErrorKind` 归类：例如真实 EPERM（不可变目录）与 EACCES 同属
            // `PermissionDenied`。errno 名与 `ErrorKind` fallback 的划分见 `node_error_parts`。
            let code = node_error_parts(source).0;
            AuthError::new(error.to_string()).with_error_code(code)
        }
    }
}

/// 默认 `auth.json`：`join(getAgentDir(), "auth.json")`。
///
/// `getAgentDir()` 每次调用都会重读 `KIANA_CODING_AGENT_DIR`（未设置时是
/// `<home>/.kiana/agent`），因此运行期改变该环境变量会影响之后构造的存储路径。返回 `String`
/// 是为了直接喂给 [`FileAuthStorageBackend::new`] / [`AuthStorage::create`]。
pub fn default_auth_path() -> String {
    get_agent_dir()
        .join("auth.json")
        .to_string_lossy()
        .into_owned()
}

/// 路径的父目录（`path.dirname`；空串/没有父目录 → `"."`）。
///
/// 用于 `ensureParentDir`。注意 Rust 的 `Path::parent` 对根路径 `"/"` 返回 `None`，于是这里
/// 得到 `"."`，与 JS `path.dirname("/")` 返回 `"/"` 不一致；本模块的测试未覆盖根路径输入。
fn parent_dir(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        Some(_) | None => ".".to_string(),
    }
}

/// `JSON.stringify(value, null, 2)`（JS 键序 + JS 数字格式）。
pub(crate) fn stringify_json(value: &Value) -> String {
    kiana_json::js_object::stringify_pretty(value, 2)
}

/// 把保序条目表转成 `serde_json::Value::Object`。
///
/// workspace 的 `serde_json` 启用了 `preserve_order`，因此 `Map` 保留 `IndexMap` 的插入
/// 顺序；整数索引键的最终排序由序列化器（`kiana_json`）负责。
pub(crate) fn entries_to_value(data: &AuthStorageData) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in data {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

/// 把一条存储的 JSON 值投影到冻结的 [`Credential`] 枚举（**不抛错**）。
///
/// TS 的 `read`/`list`/`modify`/`readStoredCredential` 只读取 `.type`/`.key`，其余字段原样
/// 传递，因此 `auth.json` 里未知 `type`（`"bearer"`）、形状不符（`expires: "123"`、
/// `{}`、标量）的条目在 TS 里都能被读出并修复。Rust 的冻结返回类型是
/// `Option<Credential>`，无法表达这些形状：本函数把它们投影为 `None`（等价于“没有可表示的
/// 凭证”），而不是像严格反序列化那样报错——后者会让 `modify` 在回调运行前失败、堵死
/// `Models.login()` / OAuth 刷新对旧版本的修复路径。
///
/// 投影规则（尽量保留可表达的部分）：
/// - `api_key`：`key` 为字符串或缺省 → [`ApiKeyCredential`]；`env` 只保留字符串项
///   （`ProviderEnv` 无法表达其它值）；其它字段进 `extra`。`key` 为 `null`/非字符串时
///   TS 的 `read` 会在 `resolveConfigValue` 里抛 `TypeError`（见
///   [`AuthStorage::read_credential`]），这里仍投影为 `key: None`
///   （`readStoredCredential`/`modify` 不解析 key，不会抛错）。
/// - `oauth`：`access`/`refresh` 为字符串且 `expires` 是数字 → [`OAuthCredential`]；
///   否则 `None`。
/// - 未知 `type`、非对象、JS 假值 → `None`。
///
/// 前置的 `is_truthy` 让 `0`/`false`/`""`/`null` 这些 JS 假值直接投影为 `None`；对象即使
/// 为空（`{}`）也会继续走 `type` 判别。
fn credential_from_value(value: &Value) -> Option<Credential> {
    if !kiana_json::js_string::is_truthy(Some(value)) {
        return None;
    }
    let Value::Object(object) = value else {
        return None;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("api_key") => Some(Credential::ApiKey(api_key_from_object(object))),
        Some("oauth") => serde_json::from_value::<Credential>(value.clone()).ok(),
        _ => None,
    }
}

/// 无校验地构造 [`ApiKeyCredential`]（字段类型不符时丢弃该字段而不是整条失败）。
///
/// `extra` 捕获除 `type`/`key`/`env` 外的所有字段（保序），序列化时由 serde 的 flatten
/// 平铺回条目；`env` 中值不是字符串的项被丢弃——`ProviderEnv` 的值类型是 `String`，无法
/// 表达 TS 允许的其它值。
fn api_key_from_object(
    object: &serde_json::Map<String, Value>,
) -> kiana_ai::auth::types::ApiKeyCredential {
    let key = object
        .get("key")
        .and_then(Value::as_str)
        .map(str::to_string);
    let env = match object.get("env") {
        // `ProviderEnv` 只能表达字符串值；非字符串项无法保留（TS 原样保留）。
        Some(Value::Object(env)) => Some(
            env.iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| (name.clone(), value.to_string()))
                })
                .collect(),
        ),
        _ => None,
    };
    let extra = object
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "type" | "key" | "env"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    kiana_ai::auth::types::ApiKeyCredential { key, env, extra }
}

/// 只读取条目的 `.type` 判别式，不校验对应字段是否齐全。
///
/// `list` 用它决定是否产出一条元数据：未知类型无法用 [`AuthType`] 表达，只能跳过（TS 会
/// 给出 `type: undefined`）。
fn credential_type(value: &Value) -> Option<AuthType> {
    match value.get("type").and_then(Value::as_str) {
        Some("api_key") => Some(AuthType::ApiKey),
        Some("oauth") => Some(AuthType::OAuth),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// FileAuthStorageBackend
// ---------------------------------------------------------------------------

/// `auth.json` 的加锁后端（复用给 models-store）。
///
/// TS: `auth-storage.ts#FileAuthStorageBackend`
///
/// 实例本身没有内存锁状态：`Clone` 只复制路径与测试钩子，两个克隆之间的互斥完全依赖文件
/// 锁（同进程内的两次加锁同样是真实的目录锁互斥，不共享额外的内存锁）。路径在构造时
/// 规范化一次，之后不再改变。
#[derive(Debug, Clone)]
pub struct FileAuthStorageBackend {
    auth_path: String,
    /// 测试钩子：异步锁的 `update_ms`（proper-lockfile 的 `update`）。
    async_update_ms: Option<u64>,
}

impl FileAuthStorageBackend {
    /// `new FileAuthStorageBackend(authPath)`（路径经 `normalizePath`）。
    ///
    /// 只做路径规范化（默认 `PathInputOptions`：展开 `~`、不做 trim），不访问文件系统；
    /// 父目录与文件在第一次 `with_lock*` 时才创建。
    pub fn new(auth_path: &str) -> Self {
        Self {
            auth_path: normalize_path(auth_path, &PathInputOptions::default()),
            async_update_ms: None,
        }
    }

    /// 默认路径 `join(getAgentDir(), "auth.json")`。
    pub fn default_path() -> Self {
        Self::new(&default_auth_path())
    }

    /// 规范化后的文件路径。
    ///
    /// 返回值在构造时确定；`AuthStorage::create` 用同一个规范化结果作为共享读状态的键。
    pub fn path(&self) -> &str {
        &self.auth_path
    }

    /// **test hook, not public API**：缩短异步锁的心跳间隔（compromise 测试用）。
    ///
    /// 值最终经 `LockOptions::effective_update_ms` 钳制到 `[1000, stale/2]`，所以传 1 实际
    /// 等价于 1 s 心跳——compromise 测试依赖这个下限让心跳可观察。
    #[doc(hidden)]
    pub fn with_async_update_ms(mut self, update_ms: u64) -> Self {
        self.async_update_ms = Some(update_ms);
        self
    }

    /// `ensureParentDir()`：不存在 → `mkdirSync(dir, { recursive: true, mode: 0o700 })`。
    ///
    /// 只在目录不存在时创建，`recursive(true)` 会连同缺失的祖先一起建。Unix 上的
    /// `mode(0o700)` 只作用于本次新建的目录，已存在的目录权限保持不变；失败按 `mkdir`
    /// syscall 映射成带 `error_code` 的 Node 错误。
    fn ensure_parent_dir(&self) -> Result<(), AuthError> {
        let dir = parent_dir(&self.auth_path);
        let dir = Path::new(&dir);
        if dir.exists() {
            return Ok(());
        }
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(dir)
            .map_err(|error| io_auth_error("mkdir", dir, error))
    }

    /// `ensureFileExists()`：不存在 → 写入 `{}`（创建时 mode 0600）。
    ///
    /// 正常路径下，已存在的文件会在 `path.exists()` 处直接返回，内容与权限都不变（测试锁定
    /// 0o660 的保留）；0600 只对新建文件生效（`OpenOptions::truncate(true)` 本身对已存在
    /// 文件同样会截断，这里只是靠先检查存在性避开它）。`exists()` 与 `open()` 不是原子的：
    /// 若其它进程在这个窗口创建了 `auth.json`，`create + truncate` 会把该文件截断为 `{}`。
    /// 写入内容是两个字节 `{}`，没有结尾换行。这一步发生在拿文件锁**之前**（与 TS 的顺序
    /// 一致）。
    fn ensure_file_exists(&self) -> Result<(), AuthError> {
        let path = Path::new(&self.auth_path);
        if path.exists() {
            return Ok(());
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|error| io_auth_error("open", path, error))?;
        file.write_all(b"{}")
            .map_err(|error| io_auth_error("write", path, error))
    }

    /// `existsSync ? readFileSync(utf-8) : undefined`。
    ///
    /// 用一次 `fs::read` + `NotFound` 特判实现，不先做存在性检查；其它错误按
    /// [`read_syscall`] 选择 syscall 文案（Unix 上 EISDIR 是 `read` 且不带路径，其余按
    /// `open` 带路径）。UTF-8 解码是 lossy 的，与 Node `readFileSync(path, "utf-8")` 对非法
    /// 字节的处理一致。
    fn read_current(&self) -> Result<Option<String>, AuthError> {
        let path = Path::new(&self.auth_path);
        match fs::read(path) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_auth_error(read_syscall(&error), path, error)),
        }
    }

    /// `writeFileSync(path, next, { encoding: "utf-8", mode: 0o600 })`。
    ///
    /// 加锁后调用；`create + truncate` 覆盖整份内容，0600 只在新建文件时生效。没有临时文件
    /// 或 rename：持锁的参与者之间互斥，非持锁的外部读者可能观察到部分内容。函数不
    /// `fsync`，成功返回表示内容已写入内核缓冲，而非已落盘。
    fn write_current(&self, next: &str) -> Result<(), AuthError> {
        let path = Path::new(&self.auth_path);
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|error| io_auth_error("open", path, error))?;
        file.write_all(next.as_bytes())
            .map_err(|error| io_auth_error("write", path, error))
    }

    /// `withLock(fn)`：同步加锁执行。
    ///
    /// 阶段：`ensureParentDir` → `ensureFileExists` → 同步文件锁（`LockOptions::sync_default`，
    /// 10 次 × 20 ms 重试）→ 读取当前文本 → 回调 → 若返回 `next` 则写回 → 释放锁。
    ///
    /// 回调在**持锁**期间执行，因此其中的读改写对其它进程也是原子的。第一次调用会创建目录
    /// 与文件（即使回调只读）。回调返回 `Err` 时不写盘，锁随 guard 的 `Drop` 释放。
    pub fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>,
    {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        let guard = lock::lock_sync(Path::new(&self.auth_path), &LockOptions::sync_default())
            .map_err(|error| lock_auth_error(error, None))?;
        let current = self.read_current()?;
        let outcome = f(current.as_deref())?;
        if let Some(next) = &outcome.next {
            self.write_current(next)?;
        }
        drop(guard);
        Ok(outcome.result)
    }

    /// `withLockAsync(fn, options)`：异步加锁执行（30 s 预算，`onCompromised` → 错误）。
    ///
    /// 时序：
    /// 1. 先检查 signal（`throwIfAborted`），因此已取消的调用不会创建目录/文件；
    /// 2. `ensureParentDir` + `ensureFileExists`（此时仍未持锁）；
    /// 3. 以 `LockOptions::async_default()`（stale 30 s、心跳 15 s）等待文件锁；等待可被
    ///    signal 取消，`on_compromised` 只把错误记进共享槽，由后续检查抛出；
    /// 4. 持锁后依次检查锁破坏与取消，读取当前文本，再 await 回调；
    /// 5. 回调之后再次检查破坏/取消，然后（有 `next` 时）写盘，写完再检查一次破坏；
    /// 6. guard 的 `Drop` 停止心跳并删除锁目录，等价于忽略错误的 `release()`。
    ///
    /// 取消语义：等待锁阶段被取消 → 立即返回 `AbortError`，回调不执行；回调**已开始**后
    /// 取消不会打断它（future 继续跑完），但其 `next` 不会提交，调用方仍得到 `AbortError`
    /// （测试 `holds_the_file_lock_until_a_cancelled_active_callback_settles_without_committing_it`
    /// 锁定该行为）。
    ///
    /// 错误：任一步失败都会先 `drop(guard)` 再返回，避免把锁留给后续调用；写盘时的打开/
    /// 写入失败分别按 `open`/`write` syscall 包装。最后一次破坏检查在写盘之后——若锁恰在
    /// 写入后才被观察到破坏，调用方得到 `ECOMPROMISED`，但文件已经写入。
    pub async fn with_lock_async<T, F>(
        &self,
        f: F,
        options: &AuthOperationOptions,
    ) -> Result<T, AuthError>
    where
        T: Send + 'static,
        F: FnOnce(Option<String>) -> LockFuture<T> + Send + 'static,
    {
        let signal = options.signal.clone();
        // 先于任何文件系统操作检查：已取消的调用不应创建 auth.json 或锁目录。
        throw_if_aborted_opt(signal.as_ref())?;
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        // `on_compromised` 在心跳线程上同步调用，不能在那里直接返回错误；回调只把错误记进
        // 共享槽，本任务在进入回调前、写盘前、写盘后各检查一次。
        let compromised: Arc<Mutex<Option<AuthError>>> = Arc::new(Mutex::new(None));
        let callback_compromised = Arc::clone(&compromised);
        let mut lock_options = LockOptions::async_default();
        if let Some(update_ms) = self.async_update_ms {
            lock_options = lock_options.with_update_ms(update_ms);
        }
        lock_options = lock_options.with_on_compromised(Arc::new(move |error: LockError| {
            let mut slot = lock_state(&callback_compromised);
            *slot = Some(AuthError::new(error.to_string()).with_error_code("ECOMPROMISED"));
        }));

        let token = signal.as_ref().map(AbortScope::token);
        let guard = lock::lock_async(Path::new(&self.auth_path), &lock_options, token.as_ref())
            .await
            .map_err(|error| lock_auth_error(error, signal.as_ref()))?;

        let throw_if_compromised = || -> Result<(), AuthError> {
            match lock_state(&compromised).as_ref() {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        };

        let outcome = async {
            throw_if_compromised()?;
            throw_if_aborted_opt(signal.as_ref())?;
            let current = self.read_current()?;
            let outcome = f(current).await?;
            Ok(outcome)
        }
        .await;

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                drop(guard);
                return Err(error);
            }
        };

        if let Err(error) = throw_if_compromised() {
            drop(guard);
            return Err(error);
        }
        if let Err(error) = throw_if_aborted_opt(signal.as_ref()) {
            drop(guard);
            return Err(error);
        }
        if let Some(next) = &outcome.next
            && let Err(error) = self.write_current(next)
        {
            drop(guard);
            return Err(error);
        }
        if let Err(error) = throw_if_compromised() {
            drop(guard);
            return Err(error);
        }
        // TS 的 `await release()`（错误被忽略）；Drop 同样停止心跳并删除锁目录。
        drop(guard);
        Ok(outcome.result)
    }
}

// ---------------------------------------------------------------------------
// InMemoryAuthStorageBackend
// ---------------------------------------------------------------------------

/// 进程内 JSON 文本后端（`{ result, next }` 语义与文件后端一致）。
///
/// TS: `auth-storage.ts#InMemoryAuthStorageBackend`
///
/// `value` 保存整份 JSON 文本（`None` = 尚未写入），异步路径另有一把 `tokio::sync::Mutex`
/// 作为串行门。同步 [`InMemoryAuthStorageBackend::with_lock`] **不**经过串行门（TS 的同步
/// 路径同样不经过 `asyncChain`），因此它只与其它同步调用通过 `value` 的互斥串行，可与
/// 进行中的异步回调交错。`Clone` 共享同一个 `value` 与串行门。
#[derive(Debug, Clone, Default)]
pub struct InMemoryAuthStorageBackend {
    value: Arc<Mutex<Option<String>>>,
    /// 异步操作的串行门（TS 的 `asyncChain`）。
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl InMemoryAuthStorageBackend {
    /// 新建空后端。
    pub fn new() -> Self {
        Self::default()
    }

    /// `withLock(fn)`（同步路径直接读写；不经过异步串行门，只与其它同步调用通过 `value`
    /// 互斥）。
    ///
    /// 回调在 `value` 的 std 锁内执行；`next` 覆盖整份文本。回调返回错误时既不写入也不改变
    /// 现有值。与文件后端不同，这里没有取消与锁破坏的概念。
    pub fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>,
    {
        let mut value = lock_state(&self.value);
        let outcome = f(value.as_deref())?;
        if let Some(next) = outcome.next {
            *value = Some(next);
        }
        Ok(outcome.result)
    }

    /// `withLockAsync(fn, options)`：以异步串行门排队；排队期间可被 signal 取消。
    ///
    /// 门是 `tokio::sync::Mutex`：异步回调逐个执行，但同步 `with_lock` 不受它约束。等待门
    /// 期间可被 signal 取消，被取消的排队项不会运行回调；拿到门后、回调前、写回前各检查一次
    /// 取消。
    ///
    /// 整个 operation 交给 `race_with_abort_signal`：它被 spawn 到运行时，取消只让**调用方**
    /// 立即返回 `AbortError`，operation 自身继续运行。因为回调后还有一次取消检查，已取消的
    /// 回调结果通常不会写入；若取消与写入/完成几乎同时发生，调用方可能收到 `AbortError` 而
    /// 值已经更新（`select!` 两个分支同时就绪时随机选择）。
    pub async fn with_lock_async<T, F>(
        &self,
        f: F,
        options: &AuthOperationOptions,
    ) -> Result<T, AuthError>
    where
        T: Send + 'static,
        F: FnOnce(Option<String>) -> LockFuture<T> + Send + 'static,
    {
        let signal = operation_signal(options.signal.clone());
        let value = Arc::clone(&self.value);
        let gate = Arc::clone(&self.gate);
        let queued_signal = signal.clone();
        let token = queued_signal.token();

        let operation = async move {
            let _guard = tokio::select! {
                guard = gate.lock() => guard,
                _ = token.cancelled() => return Err(abort_error_from_signal(Some(&queued_signal))),
            };
            throw_if_aborted(&queued_signal)?;
            let current = lock_state(&value).clone();
            let outcome = f(current).await?;
            throw_if_aborted(&queued_signal)?;
            if let Some(next) = outcome.next {
                *lock_state(&value) = Some(next);
            }
            Ok(outcome.result)
        };

        match race_with_abort_signal(operation, &signal).await {
            Ok(result) => result,
            Err(error) => Err(AuthError::from(error)),
        }
    }
}

// ---------------------------------------------------------------------------
// AuthStorageBackend
// ---------------------------------------------------------------------------

/// `interface AuthStorageBackend` 的 Rust 形态：两种具体后端的枚举。
///
/// TS 的结构化接口允许任意实现；workspace 里只有 File/InMemory 两个实现
/// （`extensions/mcp/oauth.ts` 也只接收这两个），因此用枚举获得泛型回调方法
/// （trait 的泛型方法不能作为 `dyn` 使用）。
///
/// 两个变体都支持 `.into()`（`From` 实现）。转发方法保持后端差异：文件锁跨进程生效，内存
/// 串行门只跨任务生效；`path()` 只对文件后端有值。
#[derive(Debug, Clone)]
pub enum AuthStorageBackend {
    /// `auth.json` 文件后端。
    File(FileAuthStorageBackend),
    /// 进程内后端。
    InMemory(InMemoryAuthStorageBackend),
}

impl From<FileAuthStorageBackend> for AuthStorageBackend {
    fn from(backend: FileAuthStorageBackend) -> Self {
        Self::File(backend)
    }
}

impl From<InMemoryAuthStorageBackend> for AuthStorageBackend {
    fn from(backend: InMemoryAuthStorageBackend) -> Self {
        Self::InMemory(backend)
    }
}

impl AuthStorageBackend {
    /// 同步加锁执行（`withLock`）。
    ///
    /// 文件后端会创建目录/文件并做真实文件锁；内存后端只锁 `value`。回调签名相同，调用方
    /// 无需知道自己持有哪种后端。
    pub fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>,
    {
        match self {
            Self::File(backend) => backend.with_lock(f),
            Self::InMemory(backend) => backend.with_lock(f),
        }
    }

    /// 异步加锁执行（`withLockAsync`）。
    ///
    /// 取消与错误语义由具体后端决定：文件后端有跨进程锁、取消等待与锁破坏检测，内存后端
    /// 只有串行门与取消。
    pub async fn with_lock_async<T, F>(
        &self,
        f: F,
        options: &AuthOperationOptions,
    ) -> Result<T, AuthError>
    where
        T: Send + 'static,
        F: FnOnce(Option<String>) -> LockFuture<T> + Send + 'static,
    {
        match self {
            Self::File(backend) => backend.with_lock_async(f, options).await,
            Self::InMemory(backend) => backend.with_lock_async(f, options).await,
        }
    }

    /// 文件路径（仅 File 后端）。
    ///
    /// 内存后端没有路径，返回 `None`；需要路径的调用方必须先判空。
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::File(backend) => Some(backend.path()),
            Self::InMemory(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 共享读状态
// ---------------------------------------------------------------------------

/// 一次进行中的共享重载。
///
/// - `controller`：最后一个读者离开时 abort，用来结束仍在等锁/等待的重载；
/// - `result`：`watch` 通道，`None` 表示尚未完成，完成后写入一次 `Some(Result)`；
/// - `readers`：参与本次重载的读者计数（读者自行 `fetch_add`/`fetch_sub`），归零者负责
///   清状态并 abort。
struct AuthFileReload {
    controller: AbortScope,
    result: watch::Receiver<Option<Result<StorageDocument, AuthError>>>,
    readers: AtomicUsize,
}

/// 单个路径的进程内读状态。
///
/// `data`/`revision` 是最近一次成功解析的快照与对应的文件修订号；`reload` 是当前进行中的
/// 共享重载（没有则为 `None`）；`reloads` 只给测试观察尝试次数。
#[derive(Default)]
struct AuthReadState {
    data: StorageDocument,
    revision: Option<String>,
    reload: Option<Arc<AuthFileReload>>,
    /// 真实存储读取次数（test hook，见 [`AuthStorage::reload_count`]）。
    reloads: usize,
}

// 初始快照等价于「文件不存在/为空」：`StorageDocument::parse(None)` 也返回空对象。
impl Default for StorageDocument {
    fn default() -> Self {
        Self::Object(AuthStorageData::new())
    }
}

/// 进程内按规范化路径共享的读状态（TS 的 `sharedAuthFileReadState`，按路径索引后不再受
/// 「只保留首个路径」限制——可观察行为相同）。
///
/// 值为 `Weak`：不再有 `AuthStorage` 引用时状态自动释放；`shared_read_state` 在未命中时
/// 顺便清理已死亡的条目，避免路径表随进程生命周期无限增长。所有访问都在同一把 std 锁内，
/// 不跨 `await`。
static SHARED_AUTH_READ_STATES: LazyLock<Mutex<HashMap<String, Weak<Mutex<AuthReadState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 取路径对应的共享状态：命中存活条目就复用，否则新建并登记 `Weak`。
///
/// 未命中时先清理一遍已死亡的条目。`path` 必须是调用方已经规范化过的键（`AuthStorage`
/// 传的是 `create` 里 `normalize_path` 的结果）；否则同一个文件会分裂成多个互不相干的
/// 读状态，revision 复用与读合并都会失效。
fn shared_read_state(path: &str) -> Arc<Mutex<AuthReadState>> {
    let mut states = lock_state(&SHARED_AUTH_READ_STATES);
    if let Some(existing) = states.get(path).and_then(Weak::upgrade) {
        return existing;
    }
    states.retain(|_, weak| weak.strong_count() > 0);
    let state = Arc::new(Mutex::new(AuthReadState::default()));
    states.insert(path.to_string(), Arc::downgrade(&state));
    state
}

// ---------------------------------------------------------------------------
// AuthStorage
// ---------------------------------------------------------------------------

/// `AuthStorage` 的共享内核；`Clone` 只增加引用计数。
///
/// `auth_path` 为 `None` 表示后端不是按路径构造的（[`AuthStorage::from_storage`]）：此时没有
/// revision 快路径，也不参与进程内共享读状态，每次异步读都直接走后端锁。
struct AuthStorageInner {
    storage: AuthStorageBackend,
    auth_path: Option<String>,
    read_state: Arc<Mutex<AuthReadState>>,
}

/// Credential storage backed by a JSON file。
///
/// TS: `auth-storage.ts#AuthStorage`
///
/// 实例内部只有一份 `Arc`：克隆共享同一个后端、快照与进行中的重载。快照缓存的是解析后的
/// JSON 文档（`StorageDocument`），不是 `Credential`——每次 `read` 都重新投影并解析 key。
#[derive(Clone)]
pub struct AuthStorage {
    inner: Arc<AuthStorageInner>,
}

impl std::fmt::Debug for AuthStorage {
    // 只暴露路径；快照内容（含凭证文本）不进入 Debug 输出。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthStorage")
            .field("auth_path", &self.inner.auth_path)
            .finish_non_exhaustive()
    }
}

impl AuthStorage {
    /// 构造并装载初始快照；这是 `create`/`from_storage` 的共同实现。
    ///
    /// 有路径时先读一次 revision：若与共享状态的 `revision` 相同且已有快照，直接复用（不发
    /// 锁）；否则同步调用 [`AuthStorage::reload`]。无路径时同样会 `reload` 一次填充私有快照
    /// （`in_memory` 会先向后端写入种子 JSON）。
    ///
    /// 该函数不是 async：需要重载时会就在当前线程上做阻塞的文件 IO 与文件锁，且 `reload`
    /// 的文件后端会在文件缺失时创建目录与 `auth.json`。
    fn new(storage: AuthStorageBackend, auth_path: Option<String>) -> Self {
        let read_state = match &auth_path {
            Some(path) => shared_read_state(path),
            None => Arc::new(Mutex::new(AuthReadState::default())),
        };
        let storage = AuthStorage {
            inner: Arc::new(AuthStorageInner {
                storage,
                auth_path,
                read_state,
            }),
        };
        if let Some(path) = &storage.inner.auth_path {
            let revision = get_file_revision(path);
            let same = match (&revision, &lock_state(&storage.inner.read_state).revision) {
                (Some(revision), Some(current)) => revision == current,
                _ => false,
            };
            if same {
                return storage;
            }
        }
        storage.reload();
        storage
    }

    /// `AuthStorage.create(authPath = join(getAgentDir(), "auth.json"))`。
    ///
    /// 路径先经 `normalize_path` 再交给文件后端。构造会装载初始快照：需要真实读取时通过
    /// `reload` 加锁；文件缺失则由 `ensureFileExists` 创建为 `{}`，新建文件权限是 0600。
    pub fn create(auth_path: &str) -> Self {
        let normalized = normalize_path(auth_path, &PathInputOptions::default());
        let backend = AuthStorageBackend::File(FileAuthStorageBackend::new(&normalized));
        Self::new(backend, Some(normalized))
    }

    /// 默认路径版本（`create()`）。
    ///
    /// 路径来自 [`default_auth_path`]，因此每次调用都会重读 `KIANA_CODING_AGENT_DIR`。
    pub fn create_default() -> Self {
        Self::create(&default_auth_path())
    }

    /// `AuthStorage.fromStorage(storage)`。
    ///
    /// 不关联路径：后续读操作不会检查 revision，也不复用跨实例的快照；写回与并发语义完全由
    /// 传入后端决定（内存后端是进程内串行门，文件后端仍是跨进程文件锁）。
    pub fn from_storage(storage: AuthStorageBackend) -> Self {
        Self::new(storage, None)
    }

    /// `AuthStorage.inMemory(data = {})`。
    ///
    /// 先把初始数据序列化成整份 JSON 文本写进内存后端，再走 `from_storage`（其构造会同步
    /// reload 一次，因此初始快照立即可读）。
    pub fn in_memory(data: AuthStorageData) -> Self {
        let storage = InMemoryAuthStorageBackend::new();
        let json = stringify_json(&entries_to_value(&data));
        // 内存后端不可能失败（回调永不报错）。
        let _ = storage.with_lock(|_| Ok(LockResult::with_next((), json)));
        Self::from_storage(AuthStorageBackend::InMemory(storage))
    }

    fn read_state(&self) -> MutexGuard<'_, AuthReadState> {
        lock_state(&self.inner.read_state)
    }

    fn update_read_state(&self, data: StorageDocument, revision: Option<String>) {
        let mut state = self.read_state();
        state.data = data;
        state.revision = revision;
    }

    fn current_data(&self) -> StorageDocument {
        self.read_state().data.clone()
    }

    /// **test hook, not public API**：该路径已执行的存储读取次数（测试用它观察加锁/读取
    /// 调用次数）。
    ///
    /// 计数是**尝试次数**：同步与异步重载都在开始时加一，因此加锁/读取/解析失败也会被计入。
    #[doc(hidden)]
    pub fn reload_count(&self) -> usize {
        self.read_state().reloads
    }

    /// 重新从存储读取凭证（同步；任何异常都保留上一份有效快照）。
    ///
    /// TS: `auth-storage.ts#AuthStorage.reload`
    ///
    /// 通过后端的 `with_lock` 读取内容与 revision（revision 在持锁时读取，保证与内容对应），
    /// 释放锁后再解析。`with_lock` 失败或解析失败时直接返回，`data`/`revision` 都不更新，
    /// 后续无 signal 的读继续使用旧快照。计数在尝试前递增，失败也能被 `reload_count` 看到。
    pub fn reload(&self) {
        let mut content: Option<String> = None;
        let mut revision: Option<String> = None;
        self.read_state().reloads += 1;
        let result = self.inner.storage.with_lock(|current| {
            content = current.map(str::to_string);
            revision = self.inner.auth_path.as_deref().and_then(get_file_revision);
            Ok(LockResult::new(()))
        });
        if result.is_err() {
            return;
        }
        if let Ok(data) = StorageDocument::parse(content.as_deref()) {
            self.update_read_state(data, revision);
        }
    }

    /// `reloadFromStorageAsync(options)`。
    ///
    /// 一次真实的后端读取：在锁内解析内容、读取 revision 并更新进程内快照，然后把解析结果
    /// 返回给共享读者。解析失败以 `SyntaxError` 返回且不改快照；revision 与内容在同一临界区
    /// 更新。该函数不负责合并并发读者——那是 [`AuthStorage::ensure_reload`] 的职责。
    async fn reload_from_storage_async(
        inner: Arc<AuthStorageInner>,
        signal: Option<AbortScope>,
    ) -> Result<StorageDocument, AuthError> {
        let read_state = Arc::clone(&inner.read_state);
        let auth_path = inner.auth_path.clone();
        lock_state(&read_state).reloads += 1;
        inner
            .storage
            .with_lock_async(
                move |content| {
                    let read_state = Arc::clone(&read_state);
                    let auth_path = auth_path.clone();
                    Box::pin(async move {
                        let data = StorageDocument::parse(content.as_deref())?;
                        let revision = auth_path.as_deref().and_then(get_file_revision);
                        {
                            let mut state = lock_state(&read_state);
                            state.data = data.clone();
                            state.revision = revision;
                        }
                        Ok(LockResult::new(data))
                    })
                },
                &AuthOperationOptions { signal },
            )
            .await
    }

    /// 取得（或启动）共享的异步重载。调用方负责增减 `readers`。
    ///
    /// 同一路径同一时间只有一个真实重载：已有 `state.reload` 时直接复用；否则建 `watch`
    /// 通道、起后台任务、再把 `Arc` 发布到状态里。发布在 `read_state` 锁内完成，后台任务
    /// 结束清理时也要拿同一把锁，因此不会在发布前被清掉；任务完成后发送一次结果，并仅在
    /// `state.reload` 仍指向自己时清空（避免误清后续重载）。
    fn ensure_reload(&self) -> Arc<AuthFileReload> {
        let mut state = self.read_state();
        if let Some(reload) = &state.reload {
            return Arc::clone(reload);
        }

        let controller = AbortScope::new();
        let (sender, receiver) = watch::channel(None);
        let reload = Arc::new(AuthFileReload {
            controller: controller.clone(),
            result: receiver,
            readers: AtomicUsize::new(0),
        });

        let inner = Arc::clone(&self.inner);
        let read_state = Arc::clone(&self.inner.read_state);
        let task_reload = Arc::clone(&reload);
        tokio::spawn(async move {
            let result = Self::reload_from_storage_async(inner, Some(controller)).await;
            let _ = sender.send(Some(result));
            let mut state = lock_state(&read_state);
            if state
                .reload
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &task_reload))
            {
                state.reload = None;
            }
        });

        state.reload = Some(Arc::clone(&reload));
        reload
    }

    /// 等待共享重载的结果；已有结果时立即返回。
    ///
    /// `watch` 的初始值是 `None`，任务完成写入一次 `Some(..)`。若发送端随任务一起被丢弃
    /// （运行时关闭、任务未执行到发送），`changed()` 返回 `Err`，这里转成普通 `AuthError`；
    /// 下游无 signal 的读会用「回退到旧快照」吸收它。
    async fn wait_reload(reload: Arc<AuthFileReload>) -> Result<StorageDocument, AuthError> {
        let mut receiver = reload.result.clone();
        loop {
            {
                let borrowed = receiver.borrow();
                if let Some(result) = borrowed.as_ref() {
                    return result.clone();
                }
            }
            if receiver.changed().await.is_err() {
                return Err(AuthError::new("Auth storage reload channel closed"));
            }
        }
    }

    /// `readLatestData(options)`：revision 相同则复用快照，否则共享一次真实重载。
    ///
    /// 三条路径：
    /// 1. 没有关联路径（`from_storage`）：每次直接读后端；无 signal 时把后端错误吞掉并回退
    ///    到当前快照。
    /// 2. revision 命中：直接克隆快照，不碰文件锁（热路径）。`get_file_revision` 读不到
    ///    元数据（文件不存在/不可读）时返回 `None`，此时跳过快路径。
    /// 3. 未命中：`ensure_reload` 取得共享重载，`readers += 1`，等待结果（有 signal 时与取消
    ///    赛跑），`readers -= 1`；最后一个读者负责清状态并 abort 重载。
    ///
    /// 失败语义：有 signal 时返回错误（包括重载里的 `SyntaxError`/`AbortError`）；无 signal
    /// 时返回上一份快照，避免把瞬时 IO 失败暴露给 `Models` 的常规读取。
    async fn read_latest_data(
        &self,
        options: &AuthOperationOptions,
    ) -> Result<StorageDocument, AuthError> {
        throw_if_aborted_opt(options.signal.as_ref())?;

        let Some(auth_path) = self.inner.auth_path.clone() else {
            let reload =
                Self::reload_from_storage_async(Arc::clone(&self.inner), options.signal.clone());
            return match options.signal {
                Some(_) => reload.await,
                None => match reload.await {
                    Ok(data) => Ok(data),
                    Err(_) => Ok(self.current_data()),
                },
            };
        };

        let revision = get_file_revision(&auth_path);
        if let Some(revision) = &revision {
            let state = self.read_state();
            if state.revision.as_deref() == Some(revision.as_str()) {
                return Ok(state.data.clone());
            }
        }

        let reload = self.ensure_reload();
        reload.readers.fetch_add(1, Ordering::SeqCst);
        let awaited = match options.signal.as_ref() {
            Some(signal) => {
                let waited = Self::wait_reload(Arc::clone(&reload));
                match race_with_abort_signal(waited, signal).await {
                    Ok(result) => result,
                    Err(error) => Err(AuthError::from(error)),
                }
            }
            None => Self::wait_reload(Arc::clone(&reload)).await,
        };

        let remaining = reload.readers.fetch_sub(1, Ordering::SeqCst) - 1;
        if remaining == 0 {
            let mut state = self.read_state();
            if state
                .reload
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &reload))
            {
                state.reload = None;
                reload.controller.abort();
            }
        }

        match awaited {
            Ok(data) => Ok(data),
            Err(error) => {
                if options.signal.is_some() {
                    Err(error)
                } else {
                    Ok(self.current_data())
                }
            }
        }
    }

    /// 读取一条凭证（已存储的 `api_key` 会解析 `$ENV`/`!command`）。
    ///
    /// 与 TS 的差异只在返回类型：TS 返回存储的任意值，Rust 无法表达的形状投影为 `None`。
    /// `api_key` 的 `key` 是非字符串时，TS 在 `resolveConfigValue` 内对 `key.startsWith`
    /// 抛 `TypeError`（`null` 的文案不同），这里逐字复刻——这是本方法唯一会因条目内容而
    /// 失败的路径。`oauth` 原样返回，不解析 `access`。
    ///
    /// provider 不存在时返回 `Ok(None)`；存储/重载错误按 `read_latest_data` 的规则处理。
    pub async fn read_credential(
        &self,
        provider: &str,
        options: &AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        let data = self.read_latest_data(options).await?;
        // TS: `const credential = data[provider]; signal?.throwIfAborted();`
        let Some(value) = data.get(provider)? else {
            throw_if_aborted_opt(options.signal.as_ref())?;
            return Ok(None);
        };
        throw_if_aborted_opt(options.signal.as_ref())?;
        // TS: `if (credential?.type !== "api_key") return credential;`
        // 无法投影的条目按“不可表达的凭证”返回 `None`（见 `credential_from_value`）。
        if value.get("type").and_then(Value::as_str) == Some("api_key")
            && let Value::Object(object) = &value
            && let Some(key) = object.get("key")
            && !key.is_string()
        {
            // TS 在 `resolveConfigValue(credential.key, ...)` 里对非字符串 key 抛 TypeError
            // （`config.startsWith` / `null.startsWith`），这里逐字复刻。
            return Err(if key.is_null() {
                js_type_error("Cannot read properties of null (reading 'startsWith')")
            } else {
                js_type_error("config.startsWith is not a function")
            });
        }
        let Some(credential) = credential_from_value(&value) else {
            return Ok(None);
        };
        let Credential::ApiKey(api_key) = &credential else {
            return Ok(Some(credential));
        };
        let Some(key) = &api_key.key else {
            return Ok(Some(credential));
        };
        let resolved = resolve_config_value(key, api_key.env.as_ref());
        let mut api_key = api_key.clone();
        api_key.key = resolved;
        Ok(Some(Credential::ApiKey(api_key)))
    }

    /// `modify(provider, fn, options)`：唯一写路径；`f` 返回 `None` 时不写盘。
    ///
    /// 流程：后端 `with_lock_async` 内在锁里重新解析磁盘内容，把当前条目投影后交给 `f`；
    /// `f` 返回 `Some(next)` 时用 JS 对象展开（`StorageDocument::set_spread`）替换该 provider
    /// 并把整份文档序列化，由后端在释放锁前写回。返回 `None` 时不写文件，但仍把磁盘上当前
    /// 条目的投影（`Option<Credential>`）作为返回值。
    ///
    /// 成功返回后，进程内快照被替换为锁内看到的文档；`f` 返回 `None` 的分支还会记录锁内
    /// 读到的 revision，后续读可以走快路径。`Some(next)` 写盘分支的 revision 保持 `None`
    /// （写盘后磁盘内容可能与快照不同步），因此下一次带路径读会重新比对并重载。任何错误（解析、回调、加锁、
    /// 写盘）都会保留旧快照。返回值：`f` 返回 `Some(next)` 时返回该值；返回 `None` 时返回
    /// 当前条目的投影；两种情况都**不**做 `$ENV`/`!command` 解析。
    pub async fn modify_credential(
        &self,
        provider: &str,
        f: CredentialModifyFn,
        options: &AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        let latest: Arc<Mutex<StorageDocument>> = Arc::new(Mutex::new(self.current_data()));
        let revision: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let auth_path = self.inner.auth_path.clone();
        let provider_id = provider.to_string();

        let latest_written = Arc::clone(&latest);
        let revision_written = Arc::clone(&revision);
        let result = self
            .inner
            .storage
            .with_lock_async(
                move |content| {
                    let latest = Arc::clone(&latest_written);
                    let revision = Arc::clone(&revision_written);
                    let auth_path = auth_path.clone();
                    let provider_id = provider_id.clone();
                    Box::pin(async move {
                        // 必须在锁内重新解析磁盘内容：进程内快照可能落后于外部写入，直接用它
                        // 合并会覆盖别人的修改（测试用“外部新增 openai 条目”锁定这一点）。
                        let current = StorageDocument::parse(content.as_deref())?;
                        // TS 把原始条目直接交给回调（`fn(currentData[provider])`）。无法投影
                        // 到冻结 `Credential` 的形状传 `None`：回调（如 login 的
                        // `async () => credential`）仍能运行并修复该条目。
                        let current_credential = match current.get(&provider_id)? {
                            Some(value) => credential_from_value(&value),
                            None => None,
                        };
                        let next = f(current_credential.clone()).await?;

                        let (result, next_content) = match next {
                            None => {
                                *lock_state(&latest) = current;
                                *lock_state(&revision) =
                                    auth_path.as_deref().and_then(get_file_revision);
                                (current_credential, None)
                            }
                            Some(next) => {
                                let merged = current.set_spread(
                                    &provider_id,
                                    serde_json::to_value(&next)
                                        .map_err(|error| AuthError::new(error.to_string()))?,
                                )?;
                                *lock_state(&latest) = merged.clone();
                                let content = merged.to_json_string();
                                (Some(next), Some(content))
                            }
                        };
                        Ok(match next_content {
                            Some(next) => LockResult::with_next(result, next),
                            None => LockResult::new(result),
                        })
                    })
                },
                options,
            )
            .await?;

        let latest = lock_state(&latest).clone();
        let revision = lock_state(&revision).clone();
        self.update_read_state(latest, revision);
        Ok(result)
    }

    /// `delete(provider, options)`。
    ///
    /// 在锁内重新解析磁盘内容、按 JS `delete` 语义移除该 provider、序列化整份文档并写回。
    /// 即使 provider 不存在也会重写文件（内容按 JS 格式化，黄金字节测试利用这一点）。
    /// 失败时快照不变；成功后快照的 revision 被清空（`None`），因此下一次带路径读取必然
    /// 重新检查并重载文件。
    pub async fn delete_credential(
        &self,
        provider: &str,
        options: &AuthOperationOptions,
    ) -> Result<(), AuthError> {
        let latest: Arc<Mutex<StorageDocument>> = Arc::new(Mutex::new(self.current_data()));
        let provider_id = provider.to_string();
        let latest_written = Arc::clone(&latest);
        self.inner
            .storage
            .with_lock_async(
                move |content| {
                    let latest = Arc::clone(&latest_written);
                    let provider_id = provider_id.clone();
                    Box::pin(async move {
                        let mut current = StorageDocument::parse(content.as_deref())?;
                        current.delete(&provider_id)?;
                        let next = current.to_json_string();
                        *lock_state(&latest) = current;
                        Ok(LockResult::with_next((), next))
                    })
                },
                options,
            )
            .await?;
        let latest = lock_state(&latest).clone();
        self.update_read_state(latest, None);
        Ok(())
    }

    /// `list(options)`：只列元数据，不解析 key。
    ///
    /// `entries()` 给出 JS `Object.entries` 的顺序；值为 `null` 的条目在 TS 读 `.type` 时抛
    /// `TypeError`，这里复刻。未知 `type` 的条目被跳过（TS 会返回 `type: undefined`，冻结的
    /// [`CredentialInfo`] 无法表达）。
    pub async fn list_credentials(
        &self,
        options: &AuthOperationOptions,
    ) -> Result<Vec<CredentialInfo>, AuthError> {
        let data = self.read_latest_data(options).await?;
        throw_if_aborted_opt(options.signal.as_ref())?;
        let mut credentials = Vec::new();
        for (provider_id, value) in data.entries()? {
            // TS: `credential.type`；`null` 条目会抛 TypeError。
            if value.is_null() {
                return Err(js_type_error(
                    "Cannot read properties of null (reading 'type')",
                ));
            }
            // TS: `{ providerId, type: credential.type }`。冻结的 `CredentialInfo.type` 是
            // `AuthType`（只有 api_key/oauth），未知 type 的条目无法表达，只能省略（TS 会给出
            // `type: undefined`）——见交付说明的剩余缺口。
            if let Some(type_) = credential_type(&value) {
                credentials.push(CredentialInfo {
                    provider_id,
                    r#type: type_,
                });
            }
        }
        Ok(credentials)
    }
}

// 对外 trait 只做转发；取消、错误与返回值的语义见上面的固有方法。
#[async_trait]
impl CredentialStore for AuthStorage {
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        self.read_credential(provider_id, &options).await
    }

    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        self.list_credentials(&options).await
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        self.modify_credential(provider_id, f, &options).await
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        self.delete_credential(provider_id, &options).await
    }
}

// ---------------------------------------------------------------------------
// ReadOnlyAuthStorage
// ---------------------------------------------------------------------------

/// 只读凭证存储：绝不创建 `auth.json`，也不执行 `!command`。
///
/// TS: `auth-storage.ts#ReadOnlyAuthStorage`
///
/// 与 [`AuthStorage`] 的差别：不建目录/文件、不加锁、不做 revision 缓存；第一次 `read`/`list`
/// 时同步读入并**校验整份文件**，结果（包括“文件不存在”= 空表）缓存在实例内。因此实例
/// 生命周期内看不到后续的文件创建或修改；需要刷新请新建实例。典型用途是
/// `auth check --no-refresh`：只报告当前存储，不触发刷新写盘。
pub struct ReadOnlyAuthStorage {
    auth_path: String,
    data: Mutex<Option<AuthStorageData>>,
}

impl std::fmt::Debug for ReadOnlyAuthStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadOnlyAuthStorage")
            .field("auth_path", &self.auth_path)
            .finish_non_exhaustive()
    }
}

impl ReadOnlyAuthStorage {
    /// `new ReadOnlyAuthStorage(authPath = join(getAgentDir(), "auth.json"))`。
    ///
    /// 只规范化路径，不读文件；创建实例不会创建 `auth.json` 或锁目录。
    pub fn new(auth_path: &str) -> Self {
        Self {
            auth_path: normalize_path(auth_path, &PathInputOptions::default()),
            data: Mutex::new(None),
        }
    }

    /// 默认路径版本。
    ///
    /// 路径来自 [`default_auth_path`]（每次调用重读 `KIANA_CODING_AGENT_DIR`）。
    pub fn with_default_path() -> Self {
        Self::new(&default_auth_path())
    }

    /// 惰性加载 + 校验。
    ///
    /// 第一次调用时读整个文件：
    /// - 不存在 → 缓存空表并返回（**不创建**文件）；
    /// - 其它 IO 错误 → `Failed to read auth.json: <Node 文案>`；
    /// - JSON 解析失败 → `Failed to read auth.json: <V8 文案>`；
    /// - 顶层不是对象 → `Invalid auth.json: expected an object`；
    /// - 任一条目不合法 → 整份失败（见 `validate_credential`）。
    ///
    /// 成功或“文件不存在”都会把结果缓存到 `self.data`，后续调用不再访问文件系统；UTF-8
    /// 解码是 lossy 的（与 Node 以 `utf-8` 读取一致）。
    fn load(&self) -> Result<AuthStorageData, AuthError> {
        if let Some(data) = lock_state(&self.data).as_ref() {
            return Ok(data.clone());
        }
        let path = Path::new(&self.auth_path);
        let content = match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // 文件不存在 → 空表；结果同样缓存，后续调用不再访问文件系统（只读存储不会
                // 创建文件，因此这个“文件以后出现”的变化本实例看不到）。
                let data = AuthStorageData::new();
                *lock_state(&self.data) = Some(data.clone());
                return Ok(data);
            }
            Err(error) => {
                return Err(AuthError::new(format!(
                    "Failed to read auth.json: {}",
                    node_io_error(read_syscall(&error), path, &error)
                )));
            }
        };

        let parsed = kiana_json::js_parse::parse(&strip_bom(&content)).map_err(|error| {
            AuthError::new(format!("Failed to read auth.json: {}", error.message))
        })?;
        let Value::Object(map) = parsed else {
            return Err(AuthError::new("Invalid auth.json: expected an object"));
        };
        // 先校验全部 provider 再缓存：任一条不合法就整次失败，不留下部分数据。
        for (provider_id, credential) in &map {
            validate_credential(provider_id, credential)?;
        }
        let data: AuthStorageData = map.into_iter().collect();
        *lock_state(&self.data) = Some(data.clone());
        Ok(data)
    }
}

/// `load()` 的逐 provider 校验（错误文案逐字）。
///
/// 规则与 TS 的手写校验一致（比 [`AuthStorage`] 严格得多，后者完全不校验）：
/// - `type` 必须是 `"api_key"` 或 `"oauth"`；
/// - `api_key`：`key` 缺省或字符串（显式 `null` 不合法）；`env` 缺省或“值全为字符串的
///   对象”（`null`/数组不合法）；
/// - `oauth`：`access`/`refresh` 必须是字符串，`expires` 必须存在且是有限数字（JSON 的
///   `1e400` 解析成 Infinity，判为不合法）。
///
/// 任一规则失败都返回同一句 `Invalid auth.json credential for provider "<id>"`，不附带原因。
fn validate_credential(provider_id: &str, credential: &Value) -> Result<(), AuthError> {
    let invalid = || {
        AuthError::new(format!(
            "Invalid auth.json credential for provider \"{provider_id}\""
        ))
    };
    let Value::Object(value) = credential else {
        return Err(invalid());
    };
    match value.get("type").and_then(Value::as_str) {
        Some("api_key") => {
            // TS: `key === undefined || typeof key === "string"`；`null` 不合法。
            let valid_key = match value.get("key") {
                None | Some(Value::String(_)) => true,
                Some(_) => false,
            };
            // TS: `env === undefined || (对象 && 非 null && 非数组 && 所有值 string)`。
            let valid_env = match value.get("env") {
                None => true,
                Some(Value::Object(env)) => env.values().all(|entry| entry.is_string()),
                Some(_) => false,
            };
            if valid_key && valid_env {
                Ok(())
            } else {
                Err(invalid())
            }
        }
        Some("oauth") => {
            let valid = value.get("access").is_some_and(Value::is_string)
                && value.get("refresh").is_some_and(Value::is_string)
                && value
                    .get("expires")
                    .and_then(Value::as_f64)
                    .is_some_and(f64::is_finite);
            if valid { Ok(()) } else { Err(invalid()) }
        }
        _ => Err(invalid()),
    }
}

#[async_trait]
impl CredentialStore for ReadOnlyAuthStorage {
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        // 取消检查放在加载之前与之后：加载是同步阻塞 IO，无法在读取中途被打断。
        throw_if_aborted_opt(options.signal.as_ref())?;
        let data = self.load()?;
        throw_if_aborted_opt(options.signal.as_ref())?;
        let Some(credential) = data.get(provider_id) else {
            return Ok(None);
        };
        // `load()` 已逐条校验，投影必然成功；无法投影时按缺失处理（防御式）。
        let Some(credential) = credential_from_value(credential) else {
            return Ok(None);
        };
        // `!credential.key || isCommandConfigValue(credential.key)`：不解析（避免执行命令）。
        let skip = match &credential {
            Credential::ApiKey(api_key) => match &api_key.key {
                Some(key) => key.is_empty() || is_command_config_value(key),
                None => true,
            },
            Credential::OAuth(_) => true,
        };
        if skip {
            return Ok(Some(credential));
        }
        match credential {
            Credential::ApiKey(mut api_key) => {
                let key = api_key.key.take().unwrap_or_default();
                api_key.key = resolve_config_value(&key, api_key.env.as_ref());
                Ok(Some(Credential::ApiKey(api_key)))
            }
            // OAuth 恒 skip，不会到达。
            Credential::OAuth(oauth) => Ok(Some(Credential::OAuth(oauth))),
        }
    }

    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        throw_if_aborted_opt(options.signal.as_ref())?;
        let data = self.load()?;
        throw_if_aborted_opt(options.signal.as_ref())?;
        // 只投影 type；未知/缺失 type 的条目被跳过（TS 会给出 `type: undefined`，冻结的
        // CredentialInfo 无法表达）。
        Ok(data
            .iter()
            .filter_map(|(provider_id, value)| {
                credential_type(value).map(|type_| CredentialInfo {
                    provider_id: provider_id.clone(),
                    r#type: type_,
                })
            })
            .collect())
    }

    async fn modify(
        &self,
        _provider_id: &str,
        _f: CredentialModifyFn,
        _options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        // 只读存储永远拒绝写操作；错误文案被移植测试逐字锁定。
        Err(AuthError::new(
            "Read-only credential storage cannot modify auth.json",
        ))
    }

    async fn delete(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        // 与 modify 使用同一句文案（测试分别断言两者）。
        Err(AuthError::new(
            "Read-only credential storage cannot modify auth.json",
        ))
    }
}

// ---------------------------------------------------------------------------
// readStoredCredential
// ---------------------------------------------------------------------------

/// 一次性同步读取已存储的凭证：不加锁、不创建文件、不解析 key 值。
///
/// TS 返回 `JSON.parse(...)[providerId]` 的原始值；Rust 的冻结返回类型是
/// `Option<Credential>`，因此无法投影为 [`Credential`] 枚举的形状（未知 `type` 等）返回
/// `None`（见交付说明的剩余缺口）。与 `AuthStorage.read` 不同，这里**不解析** key，
/// 所以 `key` 类型不符也不会抛错。UTF-8 按 Node `readFileSync("utf-8")` 的方式 lossy 解码。
///
/// 所有失败都折叠成 `None`：文件不存在/不可读、JSON 不合法（V8 文案被丢弃）、条目无法投影。
/// 顶层是数组时按索引取值，元素是对象则仍可能命中；顶层是字符串时按 code unit 取出的是字符串
/// 而不是对象，最终仍为 `None`；`null`/布尔/数字没有该属性。
///
/// TS: `auth-storage.ts#readStoredCredential`
pub fn read_stored_credential(provider_id: &str, auth_path: &str) -> Option<Credential> {
    let path = normalize_path(auth_path, &PathInputOptions::default());
    let bytes = fs::read(path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let parsed = kiana_json::js_parse::parse(&strip_bom(&content)).ok()?;
    match parsed {
        Value::Object(map) => map.get(provider_id).and_then(credential_from_value),
        // `data[providerId]` 对数组/标量：数组按索引、字符串按字符；非对象没有该属性。
        Value::Array(items) => js_array_index(provider_id)
            .and_then(|index| items.get(index as usize))
            .and_then(credential_from_value),
        Value::String(text) => js_array_index(provider_id)
            .and_then(|index| js_code_unit(&text, index))
            .and_then(|text| credential_from_value(&Value::String(text))),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

/// `readStoredCredential(providerId)`（默认 `join(getAgentDir(), "auth.json")`）。
///
/// 路径每次重新计算（[`default_auth_path`]），因此受 `KIANA_CODING_AGENT_DIR` 影响。
pub fn read_stored_credential_default(provider_id: &str) -> Option<Credential> {
    read_stored_credential(provider_id, &default_auth_path())
}

/// 供测试/下游构造 `AuthStorageData` 的便捷函数（`Record<string, Credential>` 字面量）。
///
/// 非对象输入返回空表（不报错），键序保持输入顺序。
pub fn auth_data_from_value(value: Value) -> AuthStorageData {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        _ => AuthStorageData::new(),
    }
}

// 测试移植自 TS `auth-storage.test.ts`，fixture 在 `tests/fixtures/auth/`。断言锁定本模块的
// 可观察行为（错误文案、字节级 JSON、取消语义、文件权限）；修改实现时不要为了通过而放宽它们。
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// `getRetryableLockErrorCode` 的平台条件：`PermissionDenied` 在 Windows 上是 EPERM
    /// （可重试），在其它平台是 EACCES（立即失败）。
    #[test]
    fn permission_denied_lock_error_code_is_platform_specific() {
        let error = LockError::Io {
            path: PathBuf::from("/tmp/auth.json.lock"),
            syscall: "mkdir",
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        let mapped = lock_auth_error(error, None);
        let expected = if cfg!(windows) { "EPERM" } else { "EACCES" };
        assert_eq!(mapped.error_code.as_deref(), Some(expected));
    }

    /// 真实 errno 必须映射到 Node 的 `error.code`（不再按 `ErrorKind` 猜：EPERM 与 EACCES
    /// 在 Rust 里同为 `PermissionDenied`，EROFS/ENOTDIR 没有专属 kind）。
    #[cfg(unix)]
    #[test]
    fn lock_and_file_errors_carry_the_node_errno_code() {
        for (errno, expected) in [
            (libc::EPERM, "EPERM"),
            (libc::EACCES, "EACCES"),
            (libc::EROFS, "EROFS"),
            (libc::ENOTDIR, "ENOTDIR"),
            (libc::EISDIR, "EISDIR"),
            (libc::ENOSPC, "ENOSPC"),
        ] {
            let error = LockError::Io {
                path: PathBuf::from("/tmp/auth.json.lock"),
                syscall: "mkdir",
                source: io::Error::from_raw_os_error(errno),
            };
            let mapped = lock_auth_error(error, None);
            assert_eq!(mapped.error_code.as_deref(), Some(expected));

            let mapped = io_auth_error(
                "mkdir",
                Path::new("/tmp/auth.json"),
                io::Error::from_raw_os_error(errno),
            );
            assert_eq!(mapped.error_code.as_deref(), Some(expected));
        }
    }

    /// Node 的 `read`/`write` syscall 错误不带 path，`open`/`mkdir` 带 path。
    #[cfg(unix)]
    #[test]
    fn node_io_error_format_matches_node() {
        let path = Path::new("/tmp/x");
        assert_eq!(
            node_io_error("read", path, &io::Error::from_raw_os_error(libc::EISDIR)),
            "EISDIR: illegal operation on a directory, read"
        );
        assert_eq!(
            node_io_error("write", path, &io::Error::from_raw_os_error(libc::ENOSPC)),
            "ENOSPC: no space left on device, write"
        );
        assert_eq!(
            node_io_error("open", path, &io::Error::from_raw_os_error(libc::ENOENT)),
            "ENOENT: no such file or directory, open '/tmp/x'"
        );
        assert_eq!(
            node_io_error("mkdir", path, &io::Error::from_raw_os_error(libc::ENOTDIR)),
            "ENOTDIR: not a directory, mkdir '/tmp/x'"
        );
    }
}
