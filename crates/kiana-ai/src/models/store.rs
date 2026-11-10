//! ModelsStore / InMemoryModelsStore。
//!
//! TS: `src/models-store.ts`。持久化的模型目录按 provider id 存储；[`ModelsStoreEntry`] 的
//! 可选字段（`lastModified`、`checkedAt`、`etag`）全部按原样往返（`etag` 含引号）。
//!
//! Rust 侧的差异：
//! - TS 的 `models: readonly AnyModel[]` 在 Rust 是 `Vec<AnyModel>`；[`AnyModel`] 由
//!   `types.rs` 定义为四变体枚举（chat/image/classifier/remote），其中前三个是已知类型的
//!   视图，`Remote` 也只是“已知类型的投影 + 原始 JSON”，因此**没有‘未知类型’变体**。
//!   TS 的 `withKnownModelTypes()` 只会在刷新边界丢弃这些条目；Rust 在反序列化
//!   [`ModelsStoreEntry`] 时把它们挡在 `models` 之外，但原样保留在 [`ModelsStoreEntry::raw_models`]
//!   中：条目未被修改时序列化会逐字写回原始数组（含未知类型与未知字段的模型），因此
//!   `read` -> `write` 往返不丢失更新版本写入的数据（直接读取 `AnyModel` 仍无法表示它们）。
//! - 未知的顶层字段保留在 [`ModelsStoreEntry::extra`]（TS 的 `structuredClone` 会保留）并按
//!   原始顺序写出。
//! - `structuredClone` 的语义由 `Clone` 承担：读时克隆出；写入按值接管所有权，
//!   调用方若还要继续使用该条目需自行 `Clone`。
//!
//! # 这个模块在系统中的位置
//!
//! TS 的 `models-store.ts` 只有接口与内存实现，真正的落盘实现由 `kiana-config` 的
//! `FileModelsStore` 提供。本模块是两者共享的**数据形状 + 读写契约**：
//! `models/mod.rs` 的刷新协调在拉取目录前用 [`ModelsStore::read`] 取回上一条记录
//! （`etag` 作为 `If-None-Match` 回传，`lastModified` 供上层判断远端目录新旧），发布时用
//! [`ModelsStore::write`] / [`ModelsStore::delete`] 落盘；[`InMemoryModelsStore`] 是未注入
//! 存储时的默认实现，进程重启即丢失。`kiana-config` 的 `FileModelsStore` 在反序列化前会
//! 先做宽松清洗（把类型不合法的已知字段挪进 `extra`），再交给本文件的 [`ModelsStoreEntry`] serde 实现。
//!
//! # 序列化契约（为什么手写 serde）
//!
//! TS 用 `structuredClone` 原样保留对象，而 `serde` 的 `derive` 会丢掉未知字段、
//! 无法固定键序，也无法在“未修改”时逐字还原。因此 `ModelsStoreEntry` 的
//! `Deserialize`/`Serialize` 都是手写的，约定如下：
//!
//! - 顶层键序固定为 `models` → `lastModified` → `etag` → `checkedAt` → 未知字段。
//!   测试 `entry_serializes_in_ts_field_order_and_round_trips` 锁定 `checkedAt` 位于最后
//!   （TS 的 `{ ...catalog, checkedAt }` 形状）。
//! - 反序列化时 `models` 只投影本版本认识的三种类型；未知类型的模型与未知字段只留在
//!   [`ModelsStoreEntry::raw_models`]。
//! - 序列化时比较“原始数组的已知模型”与当前 `models`：一致则逐字写回 `raw_models`
//!   （保住未知类型/未知字段），不一致才写出 `models` 的投影（等于 TS 在刷新边界执行
//!   `withKnownModelTypes` 后的结果）。
//! - 未知顶层字段进入 [`ModelsStoreEntry::extra`]；`serde_json` 开启了 `preserve_order`，
//!   因此 `IndexMap` 保持原始键序。
//!
//! # 取消与错误
//!
//! [`ModelsStoreOperationOptions::signal`] 是协作式取消：实现应在每个方法入口检查它
//! （TS 的 `InMemoryModelsStore` 每条方法第一行是 `signal.throwIfAborted()`）。本文件的
//! [`InMemoryModelsStore`] 只做一次入口检查，因为它的临界区里没有 await 点，检查通过后
//! 就会同步完成。
//! 中止被转换为 [`AiError`]：`name` 原样保留 signal 的 reason 名（通常为 `AbortError`，
//! 也可能被 `AbortScope::abort_with` 设成 `TimeoutError` 等）；有 reason 时消息取
//! `reason.message`，没有 reason 时用 `AbortReason::fallback_abort_error` 的
//! `The operation was aborted`。IO/解析错误由持久化实现自行映射为 [`AiError`]。
//!
//! # 并发
//!
//! [`InMemoryModelsStore`] 用一把 `std::sync::Mutex` 保护 `provider id -> entry` 表，
//! 每个方法只在锁内做一次 `HashMap` 操作、不跨 await 持锁。锁中毒时用 `into_inner`
//! 继续工作，因为 `insert`/`remove`/`get` 都是整条表项操作，不会留下半更新的条目。
//! `models/mod.rs` 的发布路径会用 per-provider 异步锁把同一 provider 的发布串行化；
//! 绕过发布路径直接调用本类型时没有额外串行化。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::de::Error as _;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::AiError;
use crate::types::{AnyModel, Model, ModelType};
use crate::utils::abort::AbortScope;

/// 一条持久化的 provider 模型目录。
///
/// TS: `src/models-store.ts#ModelsStoreEntry`
///
/// 这是持久化格式的权威形状：字段顺序、可选字段的省略、未知字段的保留都由本结构的
/// 手写 serde 实现决定（见模块文档的“序列化契约”）。构造时通常只需要填 `models` 与
/// `checked_at`；`raw_models` 由反序列化填充（读取已有存储时），手工构造时一般为 `None`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelsStoreEntry {
    /// 本版本能投影出的模型（chat/image/classifier 三种类型）。
    ///
    /// 顺序与原始数组过滤后一致；未知类型的模型不在这里，只存在于 `raw_models`。
    /// 读取方若改写本字段，再次序列化时会以它为准（见 `raw_models` 的说明）。
    pub models: Vec<AnyModel>,
    /// 反序列化时的原始 `models` 数组（逐字保留，含未知类型/未知字段的模型）。
    ///
    /// 序列化并不总是以 `models` 为准：只有当“原始数组的已知模型投影”与当前 `models`
    /// 逐元素相等时，才写回本字段；一旦调用方增删改或重排过 `models`，就改为写出
    /// `models` 的投影，仅存在于原始数组中的未知类型模型也会被丢弃（与 TS 的
    /// `withKnownModelTypes` 过滤语义一致）。每个远端模型的原始 JSON 同时由
    /// [`AnyModel::Remote`] 的 `json` 字段承载。
    ///
    /// 边界：比较是逐元素的，无法区分“未改动”和“把 `models` 清空”。若原始数组中全部是
    /// 未知类型模型（已知投影本来就是空），调用方把 `models` 置空后序列化仍会写回原数组；
    /// 要真正清空，需要同时把 `raw_models` 置为 `None` 或空 `Vec`。
    pub raw_models: Option<Vec<Value>>,
    /// 远端目录的 `Last-Modified` 头对应的 Unix 时间戳（毫秒）。
    pub last_modified: Option<i64>,
    /// 最近一次完成的远端检查的 Unix 时间戳（毫秒）；provider 默认实现写入 `Date.now()`。
    pub checked_at: Option<i64>,
    /// 远端目录的 `ETag`，**逐字保存（含引号）**，作为 `If-None-Match` 回传。
    pub etag: Option<String>,
    /// 未知的顶层字段（更新版本写入的字段）；按原始顺序写回。
    ///
    /// 反序列化时收集所有不属于 `models`/`lastModified`/`checkedAt`/`etag` 的键。
    /// 手工构造时不要放入这四个已知键，否则序列化会写出重复键。
    pub extra: IndexMap<String, Value>,
}

/// `getModelType(model) = model.type ?? "chat"`，返回已知类型名；非字符串/未知返回 `None`。
///
/// `type` 缺失或为 `null` 才等价于 chat；其他字符串（如更新版本才认识的 `"embedding"`）
/// 与畸形类型（数字/对象）一律返回 `None`，让调用方把它们留在 `raw_models` 而不是
/// 错误地归类成 chat。
fn known_model_type_name(value: &Value) -> Option<&'static str> {
    match value.get("type") {
        None | Some(Value::Null) => Some("chat"),
        Some(Value::String(name)) => match name.as_str() {
            "chat" => Some("chat"),
            "image" => Some("image"),
            "classifier" => Some("classifier"),
            _ => None,
        },
        Some(_) => None,
    }
}

/// 把一条模型 JSON 解析成 [`AnyModel`]；`type` 缺失或为 `null` 时按 chat 处理；`type` 为
/// 未知字符串，或数字/对象/数组等非 null 非字符串值时返回 `None`。
///
/// 已知类型但字段稀疏/不合法的模型**不会**返回 `None`：先尝试按 [`Model`] 完整反序列化，
/// 失败时退化为只含 `id`/`provider`/`type` 的最小投影（其余字段取默认值）。原因是 TS 的
/// `AnyModel` 允许稀疏对象，且原始 JSON 才是持久化与展示的权威形状；投影只用于请求路径，
/// 因此缺少 `api`/`baseUrl` 的投影可能不足以发请求，调用方需要时应读取 `json`。
/// 未知字段永远不会从 `value` 重建进投影。
fn parse_any_model(value: Value) -> Option<AnyModel> {
    let model_type = known_model_type_name(&value)?;
    let json = value.clone();
    // The store accepts TypeScript's sparse AnyModel objects. This is only a
    // best-effort request projection; the raw JSON remains authoritative for
    // persistence and getAllModels, so unknown fields are never reconstructed.
    let model = serde_json::from_value(value).unwrap_or_else(|_| Model {
        id: json
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        provider: json
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        model_type: match model_type {
            "image" => Some(ModelType::Image),
            "classifier" => Some(ModelType::Classifier),
            _ => None,
        },
        ..Model::default()
    });
    Some(AnyModel::Remote { model, json })
}

/// [`AnyModel`] 的 JSON 形状：chat 模型没有 `type`（除非 `Model.model_type` 显式设置），
/// image/classifier 模型带 `type`。
///
/// `Remote` 直接返回保存的原始 JSON（保留键序与未知字段），因此对 `Remote` 内部
/// `Model` 投影的修改**不会**被持久化；需要修改时请换成对应的类型化变体再写回。
/// `serde_json::to_value` 失败时退回 `Value::Null`，避免持久化路径 panic。非有限浮点数
/// （NaN/Infinity）不会触发这个兜底：serde_json 的 `Value` 序列化器把它们写成 `null`；
/// 兜底只防御自定义 `Serialize` 实现返回错误的极端情况。
fn any_model_to_value(model: &AnyModel) -> Value {
    if let AnyModel::Remote { json, .. } = model {
        return json.clone();
    }
    let mut inner = model.model().clone();
    match model {
        AnyModel::Chat(_) => {}
        AnyModel::Image(_) => inner.model_type = Some(ModelType::Image),
        AnyModel::Classifier(_) => inner.model_type = Some(ModelType::Classifier),
        AnyModel::Remote { .. } => unreachable!("returned above"),
    }
    serde_json::to_value(inner).unwrap_or(Value::Null)
}

/// 从对象中取出一个可选的时间戳：缺失与 `null` 都映射为 `None`。
///
/// 目标类型是 `i64`，所以数值必须是 JSON 整数；浮点（即使值是 `2.0`）与字符串都会
/// 返回错误。用 `shift_remove` 而不是 `remove`，是为了让剩余未知键保持原始顺序。
/// 持久化实现可以在这之前做宽松清洗（`kiana-config` 的 `FileModelsStore` 会把类型
/// 不合法的已知字段挪进 `extra`），再交给本反序列化器。
fn take_optional_i64<E: serde::de::Error>(
    map: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<i64>, E> {
    match map.shift_remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| E::custom(format!("{key} must be an integer"))),
        Some(_) => Err(E::custom(format!("{key} must be a number"))),
    }
}

// 手写反序列化的原因：必须保留未知顶层字段及其原始键序，并把 `models` 数组原样留在
// `raw_models`（derive 会直接丢弃未知字段）。错误统一映射为 serde 的 custom error，
// 由上层持久化实现决定是报错还是宽松清洗。
impl<'de> Deserialize<'de> for ModelsStoreEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // 先整体读成 Value 再逐键拆：只有这样才能把不认识的键按原顺序留给 `extra`。
        let value = Value::deserialize(deserializer)?;
        let Value::Object(mut map) = value else {
            return Err(D::Error::custom("models store entry must be an object"));
        };
        // `models` 缺失/null 等价于空数组；其他类型视为损坏数据。
        // 原数组先完整保留，投影失败的单条模型不会让整个条目失败。
        let raw_models = match map.shift_remove("models") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(models)) => models,
            Some(_) => return Err(D::Error::custom("models must be an array")),
        };
        let last_modified = take_optional_i64(&mut map, "lastModified")?;
        let checked_at = take_optional_i64(&mut map, "checkedAt")?;
        // etag 只接受字符串或 null；原样保存（含引号），不做去引号/规范化。
        let etag = match map.shift_remove("etag") {
            None | Some(Value::Null) => None,
            Some(Value::String(etag)) => Some(etag),
            Some(_) => return Err(D::Error::custom("etag must be a string")),
        };
        // `AnyModel` 无法表示未知类型；TS 只在刷新边界（`withKnownModelTypes`）过滤，
        // 这里把已知类型的模型放进 `models`，类型未知的模型只保留在 `raw_models` 供无损写回。
        // 已知类型但字段稀疏的模型会以最小投影进入 `models`（见 `parse_any_model`）。
        let models = raw_models
            .iter()
            .filter_map(|value| parse_any_model(value.clone()))
            .collect();
        Ok(ModelsStoreEntry {
            models,
            raw_models: Some(raw_models),
            last_modified,
            checked_at,
            etag,
            // 剩下没被消费的键都是本版本不认识的顶层字段；`preserve_order` 保持原顺序。
            extra: map.into_iter().collect(),
        })
    }
}

// 手写序列化的原因：需要固定顶层键序、在已知字段之后续写未知字段，并在调用方未改动
// `models` 时逐字写回 `raw_models`（无损往返）。这三件事 `derive` 都表达不了。
impl Serialize for ModelsStoreEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // 类型化视图：Remote 直接使用原始 JSON，其余变体序列化内部的 Model。
        let current = self
            .models
            .iter()
            .map(any_model_to_value)
            .collect::<Vec<_>>();
        let models = match &self.raw_models {
            // 空数组没有可保留的内容，等价于没有原始形状。
            Some(raw_models) if !raw_models.is_empty() => {
                let known_raw = raw_models
                    .iter()
                    .filter_map(|value| parse_any_model(value.clone()))
                    .map(|model| any_model_to_value(&model))
                    .collect::<Vec<_>>();
                // 只有“原始数组的已知模型投影”与当前 `models` 逐元素相等时才写原始数组：
                // 等价于对象未被改写时的深拷贝往返。未知类型模型不参与
                // 比较，所以“保留它们且不改动已知模型”会原样写回；一旦已知模型被增删改
                // 或重排，就写出 `models` 的投影，只在原始数组里的未知类型模型随之丢弃。
                if known_raw == current {
                    raw_models.clone()
                } else {
                    current
                }
            }
            _ => current,
        };
        let mut map = serializer.serialize_map(None)?;
        // 键序与 TS 的写入顺序一致：models 必须第一，checkedAt 在 etag 之后（测试锁定）。
        map.serialize_entry("models", &models)?;
        if let Some(last_modified) = self.last_modified {
            map.serialize_entry("lastModified", &last_modified)?;
        }
        if let Some(etag) = &self.etag {
            map.serialize_entry("etag", etag)?;
        }
        if let Some(checked_at) = self.checked_at {
            map.serialize_entry("checkedAt", &checked_at)?;
        }
        for (key, value) in &self.extra {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// 存储操作的可选中止信号。
///
/// TS: `src/models-store.ts#ModelsStoreOperationOptions`
///
/// 取消是协作式的：实现只在检查点观察它，不会强杀已经开始的读写。默认值（`signal: None`）
/// 表示不可取消，所有方法都会正常执行。
#[derive(Debug, Clone, Default)]
pub struct ModelsStoreOperationOptions {
    /// 取消信号；`Some` 且已中止时，方法应立即返回 [`AiError`]。`name` 原样保留 reason
    /// 的名字（通常为 `AbortError`）：有 reason 时消息取 `reason.message`，没有 reason 时
    /// 用 `AbortReason::fallback_abort_error` 的 `The operation was aborted`。
    pub signal: Option<AbortScope>,
}

/// 按 provider id 持久化模型目录。
///
/// TS: `src/models-store.ts#ModelsStore`。TS 的 Promise 拒绝在这个接口里用 [`AiError`]
/// 表达：中止错误的 `name` 原样保留 reason 的名字（通常为 `AbortError`），消息有 reason
/// 时取 `reason.message`，没有 reason 时为 `The operation was aborted`。
///
/// # 实现者契约
///
/// - 每个方法都应先检查 `options.signal`（TS 的内存实现每条方法第一行是
///   `signal.throwIfAborted()`）；已中止时不得产生读写副作用，应返回 [`AiError`]：
///   `name` 原样保留 reason 的名字（通常为 `AbortError`），消息取 `reason.message`，
///   无 reason 时为 `The operation was aborted`。
/// - [`ModelsStore::read`]：条目不存在返回 `Ok(None)`，不是错误。
/// - [`ModelsStore::write`]：整条替换该 provider 的目录，不做字段级合并。
/// - [`ModelsStore::delete`]：条目不存在时静默成功（幂等）。
/// - 读出的条目应与内部状态相互独立（TS 的 structuredClone 读出），调用方可能改写
///   `models` 后再写回。
/// - 实现必须是 `Send + Sync` 并可被 `Arc` 共享；上层发布路径会用 per-provider 锁串行化
///   同一 provider 的发布，但实现自身仍应保证单次调用的原子性。
#[allow(clippy::double_must_use, clippy::result_large_err)]
#[async_trait]
pub trait ModelsStore: Send + Sync {
    /// 读取一条目录；不存在时返回 `Ok(None)`。
    ///
    /// 错误：已中止（`AbortError`）或底层存储错误（由实现映射为 [`AiError`]）。
    async fn read(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError>;

    /// 写入一条目录（整体替换，不做合并）。
    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError>;

    /// 删除一条目录（不存在时静默成功）。
    async fn delete(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError>;
}

/// `signal.throwIfAborted()`（`utils/models-store.ts` 每条方法的第一行）。
///
/// 已中止时把 `signal.reason` 转成 [`AiError`]：`name` 原样保留（通常是 `"AbortError"`），
/// 消息就是 `reason.message`；没有 reason 的 signal 用
/// `AbortReason::fallback_abort_error`（消息 "The operation was aborted"）。
/// 这是本模块唯一的中止检查点：检查通过后，内存操作会同步完成，即使随后被 abort 也不回滚。
#[allow(clippy::result_large_err)]
fn throw_if_aborted(options: &ModelsStoreOperationOptions) -> Result<(), AiError> {
    let Some(signal) = &options.signal else {
        return Ok(());
    };
    if signal.is_aborted() {
        return Err(signal
            .reason()
            .unwrap_or_else(crate::utils::abort::AbortReason::fallback_abort_error)
            .to_error());
    }
    Ok(())
}

/// 进程内模型目录存储。
///
/// TS: `src/models-store.ts#InMemoryModelsStore`。每个方法先 `throwIfAborted`；读时克隆出，
/// 写入按值接管（调用方需要保留时自行 `Clone`，对应 TS 的写时 `structuredClone`）。
///
/// 内部是一张 `provider id -> entry` 的 `HashMap`，由一把 `std::sync::Mutex` 保护。
/// 方法虽然声明为 `async`（trait 的形状），但函数体内没有 await 点，锁不会跨 await 持有。
/// 锁中毒时用 `into_inner` 继续：锁内只有整条表项的 `insert`/`remove`/`get`，
/// 不会留下半更新状态。每个方法只在入口检查一次中止信号，之后同步完成本次操作。
#[derive(Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModelsStore for InMemoryModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        throw_if_aborted(&options)?;
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // 克隆出整条 entry（structuredClone 读出）：调用方之后改写返回值不会影响存储。
        Ok(entries.get(provider_id).cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        throw_if_aborted(&options)?;
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // entry 按值传入，整体替换该 provider 的记录（TS 的写时 structuredClone）。
        entries.insert(provider_id.to_owned(), entry);
        Ok(())
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        throw_if_aborted(&options)?;
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(provider_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AnyModel 的持久化 JSON 形状由本文件的 `any_model_to_value` / `parse_any_model` 承担。
//
// 本文件**不**为 `AnyModel` 实现/派生 `Serialize`/`Deserialize`：`AnyModel` 由（冻结的）
// `types.rs` 定义，同一 trait 在 crate 内不能有两个实现（`types.rs` 已提供 `Serialize`）。
// 但那个实现不会给 image/classifier 变体补 `type` 字段，而存储格式要求带 `type`；
// 因此这里保留自己的投影函数，不复用 `types.rs` 的序列化。`Deserialize` 也只有本路径
// 需要（类型判别 + 稀疏模型兜底）。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelCost, ModelInputModality};

    fn chat(id: &str) -> AnyModel {
        AnyModel::Chat(Model {
            id: id.into(),
            name: id.into(),
            api: "test".into(),
            provider: "p".into(),
            base_url: "https://example.test".into(),
            input: vec![ModelInputModality::Text],
            cost: ModelCost::default(),
            ..Model::default()
        })
    }

    #[tokio::test]
    async fn read_write_delete_round_trip() {
        let store = InMemoryModelsStore::new();
        assert_eq!(store.read("p", Default::default()).await.unwrap(), None);
        let entry = ModelsStoreEntry {
            models: vec![chat("a")],
            last_modified: Some(1),
            checked_at: Some(2),
            etag: Some("\"abc\"".into()),
            ..Default::default()
        };
        store
            .write("p", entry.clone(), Default::default())
            .await
            .unwrap();
        assert_eq!(
            store.read("p", Default::default()).await.unwrap(),
            Some(entry)
        );
        store.delete("p", Default::default()).await.unwrap();
        assert_eq!(store.read("p", Default::default()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn aborted_operations_do_not_run() {
        let store = InMemoryModelsStore::new();
        let signal = AbortScope::new();
        signal.abort();
        let options = ModelsStoreOperationOptions {
            signal: Some(signal),
        };
        let error = store
            .write(
                "p",
                ModelsStoreEntry {
                    models: vec![chat("a")],
                    ..Default::default()
                },
                options.clone(),
            )
            .await
            .unwrap_err();
        assert!(error.is_aborted());
        assert!(store.read("p", Default::default()).await.unwrap().is_none());
    }

    #[test]
    fn entry_serializes_in_ts_field_order_and_round_trips() {
        let entry = ModelsStoreEntry {
            models: vec![chat("a")],
            last_modified: Some(1),
            checked_at: Some(2),
            etag: Some("\"abc\"".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&entry).unwrap();
        let expected = serde_json::json!({
            "models": [{
                "id": "a", "name": "a", "api": "test", "provider": "p",
                "baseUrl": "https://example.test", "input": ["text"],
                "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0}
            }],
            "lastModified": 1,
            "checkedAt": 2,
            "etag": "\"abc\""
        });
        assert_eq!(json, expected);
        // Fresh refresh uses `{ ...catalog, checkedAt }`; catalog itself is
        // `{ models, lastModified, etag }`, so checkedAt is the final slot.
        let text = serde_json::to_string(&entry).unwrap();
        assert!(
            text.ends_with(r#","etag":"\"abc\"","checkedAt":2}"#),
            "{text}"
        );
        let parsed: ModelsStoreEntry = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.models.len(), entry.models.len());
        assert_eq!(parsed.models[0].model(), entry.models[0].model());
        assert_eq!(parsed.last_modified, entry.last_modified);
        assert_eq!(parsed.checked_at, entry.checked_at);
        assert_eq!(parsed.etag, entry.etag);
        // 读回后原样写出的字节形状不变（raw_models 保留原始数组）。
        assert_eq!(serde_json::to_value(&parsed).unwrap(), expected);
    }

    #[test]
    fn unknown_top_level_fields_survive_a_read_write_round_trip() {
        // TS 的 FileModelsStore 解析后 `structuredClone` 原对象再写回：更新版本写入的
        // 未知字段必须保留。
        let json = serde_json::json!({
            "models": [],
            "lastModified": 1,
            "futureField": {"nested": [1, 2]},
            "etag": "\"v1\""
        });
        let parsed: ModelsStoreEntry = serde_json::from_value(json).unwrap();
        assert_eq!(
            parsed.extra.get("futureField"),
            Some(&serde_json::json!({"nested": [1, 2]}))
        );
        let written = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            written["futureField"],
            serde_json::json!({"nested": [1, 2]})
        );
        assert_eq!(written["etag"], "\"v1\"");
    }

    #[test]
    fn unknown_model_types_and_fields_survive_an_unchanged_round_trip() {
        // `AnyModel` 无法表示未知类型，但它们必须随原始数组无损写回（TS 的 structuredClone
        // 保留整个模型对象，包括未知字段）。
        let json = serde_json::json!({
            "models": [
                {
                    "id": "known", "name": "known", "api": "test", "provider": "p",
                    "baseUrl": "https://example.test", "input": ["text"],
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
                    "futureModelField": true
                },
                {
                    "id": "future", "name": "future", "api": "test", "provider": "p",
                    "baseUrl": "https://example.test", "input": ["text"], "type": "embedding",
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0}
                }
            ]
        });
        let parsed: ModelsStoreEntry = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(parsed.models.len(), 1);
        assert_eq!(parsed.models[0].model().id, "known");
        let written = serde_json::to_value(&parsed).unwrap();
        assert_eq!(written, json);

        // Removing all typed models removes unknown raw-only models too. Replacing
        // an item with a typed model changes its projection as TS would.
        let mut changed = parsed;
        changed.models.clear();
        let written = serde_json::to_value(&changed).unwrap();
        assert_eq!(written["models"], serde_json::json!([]));
    }
}
