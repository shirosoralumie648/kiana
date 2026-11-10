//! TS 运行时对象键序的还原（wire / 会话文件共用的写出层）。
//!
//! TS 的 `JSON.stringify` 按对象**插入顺序**输出键，而插入顺序由「对象字面量 + 后续赋值」
//! 决定；Rust 的带类型结构体只能按字段声明序序列化。本模块在写出到 JSON `Value` 之后做一次
//! 递归重排，把 `system` / `assistant` / `toolResult` 消息、`usage` 与模型对象的键序还原成
//! TS 运行时顺序。
//!
//! 调用方：
//! - `kiana-rpc-print::json_event`（print/json/rpc 的 stdout 记录：`to_wire_value` /
//!   `serialize_wire_line` / `serialize_event_line`）；
//! - `kiana-session-core::session_manager`（新追加会话条目的 `JSON.stringify(entry)`；
//!   从文件加载的条目带原始对象，走逐字回放，不经过本模块）。
//!
//! 设计说明与各键序常量见下方注释；完整背景见 `ARCHITECTURE.md` §7.3 M3-verification
//! 与 `spec/ca-rpc-print.md` §4.4.2。
//!
//! # 一次典型调用
//! 1. 调用方把记录序列化成 [`serde_json::Value`]（wire 层用 [`to_wire_value`]；远端模型的
//!    原始 JSON 身份由 `AnyModel` 手写的 `Serialize` 直接写出 `Remote.json` 保留，
//!    `to_wire_value` 本身不额外包装，见该函数文档）；
//! 2. 调用 [`reorder_wire_keys`] 原地重排：对象先做本层重排，再递归子值；数组逐元素递归；
//! 3. 调用方按 `JSON.stringify` 语义逐字节写出（`kiana_json::js_object::stringify`）。
//!
//! # 关键约束
//! - 依赖 `serde_json` 的 `preserve_order` feature（workspace 已开启）：`Map` 才是保持插入序
//!   的 `IndexMap`，键序重排才有意义；删除键必须用 `shift_remove`——`remove` 在
//!   `preserve_order` 下等价于 `swap_remove`，会把末位键换到删除位置，破坏其余键的相对顺序。
//! - 重排只改键序（模型对象还会补回目录专属键，见 `merge_catalog_extra_keys`），不删键、
//!   不改已有键的值：`apply_key_order` 会把未列出的键按原相对顺序追加到末尾。
//! - 重排函数都没有错误通道、不 panic：形状不满足守卫键时静默跳过。漏识别不会报错，
//!   只会让线格式键序偏离 TS，因此新增消息形状时要同步这里的守卫与常量。
//! - 重排除单键远端包装对象外是幂等的：同一对象重复处理不会继续改变键序（顺序只由键集
//!   与模板决定）。单键包装对象例外：首次调用只把它解包成远端原文且不再递归，第二次调用
//!   才会继续处理解包出的对象（可能按模型规则重排/补键，见 `reorder_wire_keys_inner`）。

use serde_json::{Map, Value};

/// assistant 消息字面量（真实 provider）的键序：`usage`/`stopReason` 在 `timestamp` 之前。
const ASSISTANT_BASE_ORDER: &[&str] = &[
    "role",
    "content",
    "api",
    "provider",
    "model",
    "usage",
    "stopReason",
    "deferred",
    "timestamp",
];

/// 真实 provider 在字面量之后按赋值顺序追加的 assistant 字段。
///
/// `providerThinkingLevel` 在请求发出前赋值（`anthropic-messages.ts:652`），早于
/// `responseId`/`responseModel`；`durationMs` 由 `event-stream.ts#128` 在 done/error 时追加，
/// `thinkingLevel` 由 `agent-loop.ts:419` 最后追加。
const ASSISTANT_TRAILING_ORDER: &[&str] = &[
    "providerThinkingLevel",
    "responseId",
    "responseModel",
    "diagnostics",
    "endTurn",
    "rawStopReason",
    "errorMessage",
    "durationMs",
    "thinkingLevel",
];

/// `fauxAssistantMessage` / `createErrorMessage` 的字面量键序：`deferred`、`errorMessage`、
/// `responseId` 都在 `timestamp` **之前**（`providers/faux.ts:79-105,327-338`）。
const FAUX_ASSISTANT_BASE_ORDER: &[&str] = &[
    "role",
    "content",
    "api",
    "provider",
    "model",
    "usage",
    "stopReason",
    "deferred",
    "errorMessage",
    "responseId",
    "timestamp",
];

/// 中止消息的基序：`createAbortedMessage` 保留 partial 的键位（`responseId` 若存在仍在
/// `timestamp` 之前），只把新加的 `errorMessage` 追加在 `timestamp` 之后。
const FAUX_ABORTED_BASE_ORDER: &[&str] = &[
    "role",
    "content",
    "api",
    "provider",
    "model",
    "usage",
    "stopReason",
    "deferred",
    "responseId",
    "timestamp",
];

/// `anthropic-messages` / `bedrock-converse-stream` 的 usage 运行时键序：字面量之后追加
/// `cacheWrite1h`，再追加 `reasoning`（都在 `cost` 之后）。
const ANTHROPIC_USAGE_ORDER: &[&str] = &[
    "input",
    "output",
    "cacheRead",
    "cacheWrite",
    "totalTokens",
    "cost",
    "cacheWrite1h",
    "reasoning",
];

/// `provider-composer.ts#modelFromJson` 构造自定义模型时的字面量键序。
const MODEL_FROM_JSON_ORDER: &[&str] = &[
    "id",
    "name",
    "api",
    "provider",
    "baseUrl",
    "reasoning",
    "thinkingLevelMap",
    "input",
    "inputLimits",
    "cost",
    "promptCache",
    "contextWindow",
    "maxTokens",
    "samplingParams",
    "samplingParamsByThinkingLevel",
    "headers",
    "compat",
];

/// Serialize any value through a forwarding serializer that would tag a remote-model
/// envelope. The tag is an internal marker that reorder unwraps; public AnyModel
/// serialization itself stays the original JSON object.
///
/// 为什么需要这一层：[`reorder_wire_keys`] 会把“看起来像模型对象”的 JSON 按内置目录模板
/// 重排并补键（见 `reorder_model_object`）。远端目录模型的原始 JSON 可能缺字段、带未知
/// 字段，不能套用内置 chat 模板，因此设计上希望序列化时留下“这是远端原文”的标记：
/// 把原始 JSON 包成单键对象 `{"<REMOTE_MODEL_JSON_KEY>": <原始对象>}`（键名见
/// [`kiana_ai::types::REMOTE_MODEL_JSON_KEY`]），由 [`reorder_wire_keys`] 识别并在写出前
/// 解包。该标记不是公共 wire 格式，不能出现在持久化文件或 stdout 里。
///
/// 现状（与上述设计意图不同，读代码时以这里为准）：`AnyModel::Remote` 是结构体变体
/// `Remote { model, json }`（`kiana-ai/src/types.rs`），并且 `AnyModel` 的 `Serialize` 是
/// 手写的——`Remote` 直接调用 `json.serialize(serializer)`，不会触发
/// `serialize_newtype_variant`。因此下面 `name == "AnyModel" && variant == "Remote"` 的
/// 分支在当前仓库不可达，本函数不会产生上述包装对象；远端 JSON 的保真（未知字段与插入序）
/// 由 `AnyModel` 的 `Serialize` 直接写出 `json` 保证。该分支按历史/防御性代码保留，
/// 不能据此认为包装机制仍在生效。
///
/// 实现：`TagRemote` 是转发 [`serde::Serializer`] 的包装器，绝大多数方法直接转给
/// `serde_json::value::Serializer`，只在 `serialize_newtype_variant` 里特判上述变体。
///
/// 返回：[`serde_json::Error`] 只来自底层序列化失败。本函数不做键序调整；
/// 调用方必须把返回值交给 [`reorder_wire_keys`]（wire 层见
/// `kiana-rpc-print::json_event::to_wire_value`）；若将来某条路径真的产生包装对象，
/// 未经 reorder 会原样漏进输出。
pub fn to_wire_value<T: serde::Serialize>(value: &T) -> Result<Value, serde_json::Error> {
    use serde::ser::{Error, Serialize as _};
    // 转发式 Serializer：把绝大多数调用原样交给内部 serializer，只在
    // `serialize_newtype_variant` 里尝试给 AnyModel::Remote 打标记
    // （当前不会走到，见函数文档）。
    struct TagRemote<S>(S);
    impl<S: serde::Serializer> serde::Serializer for TagRemote<S> {
        type Ok = S::Ok;
        type Error = S::Error;
        type SerializeSeq = S::SerializeSeq;
        type SerializeTuple = S::SerializeTuple;
        type SerializeTupleStruct = S::SerializeTupleStruct;
        type SerializeTupleVariant = S::SerializeTupleVariant;
        type SerializeMap = S::SerializeMap;
        type SerializeStruct = S::SerializeStruct;
        type SerializeStructVariant = S::SerializeStructVariant;
        fn serialize_newtype_struct<T: serde::Serialize + ?Sized>(
            self,
            _name: &'static str,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            value.serialize(TagRemote(self.0))
        }
        fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_bool(v)
        }
        fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_i8(v)
        }
        fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_i16(v)
        }
        fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_i32(v)
        }
        fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_i64(v)
        }
        fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_u8(v)
        }
        fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_u16(v)
        }
        fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_u32(v)
        }
        fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_u64(v)
        }
        fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_f32(v)
        }
        fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_f64(v)
        }
        fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_char(v)
        }
        fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_str(v)
        }
        fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_bytes(v)
        }
        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_none()
        }
        fn serialize_some<T: serde::Serialize + ?Sized>(
            self,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            value.serialize(TagRemote(self.0))
        }
        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_unit()
        }
        fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_unit_struct(name)
        }
        fn serialize_unit_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            self.0.serialize_unit_variant(name, index, variant)
        }
        fn serialize_newtype_variant<T: serde::Serialize + ?Sized>(
            self,
            name: &'static str,
            _index: u32,
            variant: &'static str,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            if name == "AnyModel" && variant == "Remote" {
                // 先把原始 JSON 序列化成独立 Value，再包成单键对象交给底层 serializer；
                // 之后的 reorder 递归遇到这个单键对象就解包。
                let raw = serde_json::to_value(value).map_err(S::Error::custom)?;
                let wrapped = Value::Object(Map::from_iter([(
                    kiana_ai::types::REMOTE_MODEL_JSON_KEY.to_owned(),
                    raw,
                )]));
                return wrapped.serialize(self.0);
            }
            // 其余变体直接序列化内层值。
            value.serialize(TagRemote(self.0))
        }
        fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            self.0.serialize_seq(len)
        }
        fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
            self.0.serialize_tuple(len)
        }
        fn serialize_tuple_struct(
            self,
            name: &'static str,
            len: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            self.0.serialize_tuple_struct(name, len)
        }
        fn serialize_tuple_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            self.0.serialize_tuple_variant(name, index, variant, len)
        }
        fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            self.0.serialize_map(len)
        }
        fn serialize_struct(
            self,
            name: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            self.0.serialize_struct(name, len)
        }
        fn serialize_struct_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            self.0.serialize_struct_variant(name, index, variant, len)
        }
        fn collect_str<T: std::fmt::Display + ?Sized>(
            self,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            self.0.collect_str(value)
        }
        fn is_human_readable(&self) -> bool {
            self.0.is_human_readable()
        }
    }
    value.serialize(TagRemote(serde_json::value::Serializer))
}

/// 按 TS 运行时构造序重排 wire 对象里的易错键组（递归处理数组与对象）。
///
/// - `role == "system"` 且带 `toolsAdded`/`toolsRemoved`（同时要求 `content` 与 `timestamp`
///   存在）：把两者移到对象末尾，顺序 `toolsAdded`、`toolsRemoved`
///   （TS: `withToolChanges` 的解构 + 追加）。
/// - 完整 assistant 消息（`role`/`content`/`api`/`provider`/`model`/`usage`/`stopReason`/
///   `timestamp` 齐备）：按 `ASSISTANT_BASE_ORDER` + `ASSISTANT_TRAILING_ORDER` 重建；
///   `api == "faux"` 走 faux 字面量序（中止消息把 `errorMessage` 放到末尾）；
///   `api == "anthropic-messages"`/`"bedrock-converse-stream"` 时同步重排 `usage`。
///   只带 `thinkingLevel` 的 assistant 形状对象仍按旧规则移到最末
///   （TS: `Object.assign(..., { thinkingLevel })`）。
/// - `role == "toolResult"`：把 `durationMs` 移到 `timestamp` 之前、`nestedCalls` 移到最末
///   （TS: `createToolResultMessage` + `agent-session.ts:1112` 的追加）。
/// - 模型对象（`id`/`name`/`api`/`provider`/`baseUrl` 均为字符串且带 `input` 数组与 `cost`
///   对象）：按 kiana-ai 内嵌的模型目录（与 TS `providers/data/*.json` 同一份数据）的**原始键序**
///   重排。TS 的模型运行时对象就是目录 JSON 里的对象（`flattenModelCatalog` 用
///   `Object.fromEntries` 直接收集，`applyModelOverride` 的 spread 保留已有键的位置），
///   而 Rust 的 [`kiana_ai::types::Model`] 只能按结构体声明序序列化；这一步把两者对齐。
///   不带 `type` 的模型是 models.json 自定义模型（`modelFromJson` 不写 `type`），按
///   `MODEL_FROM_JSON_ORDER` 重排；带 `type` 但不在内嵌目录里的（远端刷新目录）保持原样，
///   其 TS 键序来自远端 JSON，Rust 侧无法还原。
///
/// 守卫键用于避免误伤任意带同名字段的对象。
///
/// 递归覆盖数组元素与嵌套对象；本层的重排与补键不改变子值内容，因此与随后递归子值的
/// 顺序互不影响。`type == "compaction"` 的对象里，只对 `systemMessage` 子对象跳过 system
/// 的 tool 变更搬移（标记参数的传递见 `reorder_wire_keys_inner`）。
///
/// 无返回值、无错误：不认识的形状原样保留。需要“序列化 + 重排”一步到位时用
/// `kiana-rpc-print::json_event::to_wire_value`。
pub fn reorder_wire_keys(value: &mut Value) {
    reorder_wire_keys_inner(value, false);
}

/// Compaction captures the already-computed system message using
/// `{ ...systemMessage, timestamp }` (session-manager.ts#appendCompaction).
/// Unlike the live `withToolChanges` path, that spread retains toolsAdded and
/// toolsRemoved in their existing slots, before the replacement timestamp.
fn reorder_wire_keys_inner(value: &mut Value, compaction_system_message: bool) {
    match value {
        Value::Object(map) if map.len() == 1 => {
            // 只认“恰好一个键”的包装对象：设计上这是 to_wire_value 给远端模型原始 JSON
            // 加的内部标记。但当前 `AnyModel::Remote` 的 `Serialize` 直接输出 `json`，
            // 不会产生这种包装对象（见 `to_wire_value` 文档），因此这里实际只处理调用方
            // 手工构造的单键对象；单键但不是该键的对象按普通对象处理。
            let Some(raw) = map.shift_remove(kiana_ai::types::REMOTE_MODEL_JSON_KEY) else {
                return reorder_wire_object(map, compaction_system_message);
            };
            *value = raw;
            // The original remote object is already in TypeScript insertion
            // order; no model or other structural reordering applies to it.
            // 解包后不再递归：远端原文的嵌套对象同样是远端 JSON 的键序。
            // 这是一次性转换：再次调用时包装键已不存在，解包出的对象会按普通模型对象
            // 规则继续处理，因此带包装对象的输入连续重排两次可能得到不同键序。
        }
        Value::Object(map) => reorder_wire_object(map, compaction_system_message),
        Value::Array(items) => {
            // 数组元素独立判断；compaction 标记只描述“某个对象的 systemMessage 子键”，
            // 不跨元素传播。
            for item in items {
                reorder_wire_keys_inner(item, false);
            }
        }
        _ => {}
    }
}

fn reorder_wire_object(map: &mut Map<String, Value>, compaction_system_message: bool) {
    // 阶段 1：本层重排。下面的重排函数各自只在形状匹配时动手，互不依赖。
    // 先跑“旧规则”（system 搬 tool 变更、assistant 搬 thinkingLevel），再跑完整消息的
    // 整序：完整 assistant 消息会在后者里被整体重建，覆盖前者的结果。
    if !compaction_system_message {
        // compaction 的 systemMessage 跳过：TS 用 `{ ...systemMessage, timestamp }` 播种，
        // toolsAdded/toolsRemoved 留在原槽位，与 withToolChanges 的“移到末尾”不同。
        reorder_object(map);
    }
    reorder_assistant_object(map);
    reorder_tool_result_object(map);
    reorder_model_object(map);
    // 阶段 2：递归子值。只有 compaction 对象自己的 `systemMessage` 继承标记，
    // 其余子键（summary/details/usage 等）按普通规则处理。
    let is_compaction = map.get("type").and_then(Value::as_str) == Some("compaction");
    for (key, child) in map.iter_mut() {
        reorder_wire_keys_inner(child, is_compaction && key == "systemMessage");
    }
}

/// `map` 现有键是否全部出现在 `keys` 中（用于完整消息签名守卫）。
fn contains_all(map: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().all(|key| map.contains_key(*key))
}

/// 按给定键序重建 `map`：`order` 里存在的键依次插入，其余键保持相对顺序追加到末尾
/// （防止未知字段丢失）。
fn apply_key_order(map: &mut Map<String, Value>, order: &[String]) {
    // `Map` 的插入顺序就是输出顺序，无法原地移动键；先 `take` 走旧表（避免克隆值），
    // 再按目标顺序逐个 `shift_remove` + `insert` 重建。
    let mut previous = std::mem::take(map);
    for key in order {
        if let Some(value) = previous.shift_remove(key) {
            map.insert(key.clone(), value);
        }
    }
    map.extend(previous);
}

/// 把 `keys` 中存在的键按序推入 `order`。
fn push_present(map: &Map<String, Value>, keys: &[&str], order: &mut Vec<String>) {
    for key in keys {
        if map.contains_key(*key) {
            order.push((*key).to_string());
        }
    }
}

/// 把 `map` 中尚未进入 `order` 的键按原相对顺序追加。
fn push_remaining(map: &Map<String, Value>, order: &mut Vec<String>) {
    for key in map.keys() {
        if !order.iter().any(|ordered| ordered == key) {
            order.push(key.clone());
        }
    }
}

/// 完整 assistant 消息 -> TS 运行时键序。
fn reorder_assistant_object(map: &mut Map<String, Value>) {
    if map.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    // 完整 AssistantMessage 签名才重建整序；只带 thinkingLevel 的形状留给
    // `reorder_object` 的旧规则，避免误伤扩展构造的自定义消息。
    if !contains_all(
        map,
        &[
            "content",
            "api",
            "provider",
            "model",
            "usage",
            "stopReason",
            "timestamp",
        ],
    ) {
        return;
    }
    // 复制成 String：结束对 map 的不可变借用，随后才能 `get_mut("usage")`。
    let api = map
        .get("api")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // 先重排嵌套的 usage（它有自己的形状守卫），再重建本层键序。
    if let Some(Value::Object(usage)) = map.get_mut("usage") {
        reorder_usage_object(usage, &api);
    }
    let aborted = map.get("stopReason").and_then(Value::as_str) == Some("aborted");
    let mut order = Vec::new();
    if api == "faux" {
        // faux provider 用不同的字面量基序（见上面的 FAUX_* 常量）：
        // 普通消息把 durationMs/thinkingLevel 追加在 timestamp 之后；
        // 中止消息在 timestamp 之后先放 errorMessage，再排 durationMs/thinkingLevel。
        let base = if aborted {
            FAUX_ABORTED_BASE_ORDER
        } else {
            FAUX_ASSISTANT_BASE_ORDER
        };
        push_present(map, base, &mut order);
        if aborted {
            push_present(
                map,
                &["errorMessage", "durationMs", "thinkingLevel"],
                &mut order,
            );
        } else {
            push_present(map, &["durationMs", "thinkingLevel"], &mut order);
        }
    } else {
        push_present(map, ASSISTANT_BASE_ORDER, &mut order);
        push_present(map, ASSISTANT_TRAILING_ORDER, &mut order);
    }
    // 未识别的键（扩展字段等）按原相对顺序追加到末尾，保证不丢字段。
    push_remaining(map, &mut order);
    apply_key_order(map, &order);
}

/// `role == "toolResult"`：`durationMs` 在 `timestamp` 之前，`nestedCalls` 最末。
fn reorder_tool_result_object(map: &mut Map<String, Value>) {
    if map.get("role").and_then(Value::as_str) != Some("toolResult") {
        return;
    }
    // 守卫键是启发式最小集合（不含标准 `ToolResultMessage` 必填的 `toolName`，见
    // `kiana-ai/src/types.rs`）：带齐 `toolCallId`/`content`/`isError`/`timestamp` 的
    // 扩展构造 `role: "toolResult"` 对象同样会被重排，无法与标准消息区分。
    if !contains_all(map, &["toolCallId", "content", "isError", "timestamp"]) {
        return;
    }
    // 先取出除两个目标键之外的既有键序，再把 durationMs 插到 timestamp 之前、
    // nestedCalls 放到最末（TS: createToolResultMessage + agent-session.ts:1112 的追加）。
    let mut order: Vec<String> = map
        .keys()
        .filter(|key| key.as_str() != "durationMs" && key.as_str() != "nestedCalls")
        .cloned()
        .collect();
    if map.contains_key("durationMs") {
        if let Some(index) = order.iter().position(|key| key == "timestamp") {
            order.insert(index, "durationMs".to_string());
        } else {
            // 守卫已保证 timestamp 存在，这里只是防御性分支。
            order.push("durationMs".to_string());
        }
    }
    if map.contains_key("nestedCalls") {
        order.push("nestedCalls".to_string());
    }
    apply_key_order(map, &order);
}

/// `usage` 对象 -> TS 运行时键序（仅 `anthropic-messages` / `bedrock-converse-stream`）。
///
/// `api` 是所属 assistant 消息的 `api` 字段：只有这两个 provider 会在字面量之后追加
/// `cacheWrite1h`、`reasoning`（都在 `cost` 之后）；其余 API 直接返回。多数内置 provider
/// 的 TS usage 字面量与 Rust 结构体声明序一致，早退不影响结果；但 `kiana-messages` 的
/// usage 是从服务端 JSON 解析的（`kiana-ai/src/api/kiana_messages.rs`），键序由服务端
/// 给出，属于已知无法还原的情况（见 `kiana-rpc-print` 的 `json_event` 模块文档）。
/// 只有 `input`/`output`/`cacheRead`/`cacheWrite`/`totalTokens`/`cost` 六个基础键
/// 齐备时才重排，稀疏或扩展构造的 usage 保持原样。
///
/// 由 `reorder_assistant_object` 在消息内部调用；`message_update` 的顶层 `usage` 不在消息
/// 对象里，由 `kiana-rpc-print::json_event` 的 `message_usage` 单独调用本函数。
pub fn reorder_usage_object(map: &mut Map<String, Value>, api: &str) {
    if api != "anthropic-messages" && api != "bedrock-converse-stream" {
        return;
    }
    if !contains_all(
        map,
        &[
            "input",
            "output",
            "cacheRead",
            "cacheWrite",
            "totalTokens",
            "cost",
        ],
    ) {
        return;
    }
    let mut order = Vec::new();
    push_present(map, ANTHROPIC_USAGE_ORDER, &mut order);
    push_remaining(map, &mut order);
    apply_key_order(map, &order);
}

/// 把目录里存在、但 [`kiana_ai::types::Model`] 没有字段承载的键补回模型对象。
///
/// radius 的 32 个目录条目带 `lab` / `enabled` / `providers`（其中一个还带 `free`）；
/// TS 的模型运行时对象就是目录 JSON 本身，`JSON.stringify` 会输出这些键。`serde` 在
/// 反序列化时丢弃未知字段，[`reorder_from_template`] 只能重排已有键，因此识别出目录
/// 模板后必须先把缺失的键按模板位置插回，再按模板键序输出。
///
/// 值直接取模板原文：TS 的 `applyModelOverride` 是 `{...model, ...override}`，覆盖层
/// 不会删除或改写这些目录专属键。只在“运行时对象确实来自该目录条目”的分支调用
/// （models.json 自定义模型由 `modelFromJson` 重新构造，不含这些键）。
fn merge_catalog_extra_keys(map: &mut Map<String, Value>, template: &Map<String, Value>) {
    if template.keys().all(|key| map.contains_key(key)) {
        return;
    }
    // 按模板顺序合并：已有键保留运行时值（models.json 覆盖层可能已改写它），
    // 缺失键取模板原文——这些键 serde 反序列化时被丢弃，运行时值本就无从产生。
    let mut merged = Map::new();
    for (key, template_value) in template {
        match map.get(key) {
            Some(value) => {
                merged.insert(key.clone(), value.clone());
            }
            None => {
                merged.insert(key.clone(), template_value.clone());
            }
        }
    }
    for (key, value) in map.iter() {
        if !template.contains_key(key) {
            merged.insert(key.clone(), value.clone());
        }
    }
    *map = merged;
}

/// 识别模型对象并按其目录原型重排键序（含嵌套的 `inputLimits`/`compat` 等对象）。
///
/// 目录里查不到、或查到了但因 `type` 判别被跳过（见下），且没有 `type` 判别键的对象
/// 按 models.json 自定义模型处理（`modelFromJson` 不写 `type`，内置/远端目录条目都有），
/// 按 [`MODEL_FROM_JSON_ORDER`] 重排；`models` 数组里与内置同名 id 的覆盖条目同样走这里
/// （`provider-composer.ts:336-342` 用 `modelFromJson` 替换内置条目）。
fn reorder_model_object(map: &mut Map<String, Value>) {
    // 签名不满足（例如稀疏的远端条目缺 name/baseUrl）就不是模型对象，直接跳过。
    let Some((provider, id)) = model_identity(map) else {
        return;
    };
    let has_type = map.contains_key("type");
    if let Some(template) = lookup_catalog_model(map, provider, id) {
        let template_has_type = template.get("type").is_some();
        // 条件 `has_type || !template_has_type` 只排除一种组合：运行时对象省略了 `type`
        // （查表按 `type` 选，缺失时只落在 chat 表），却命中了带 `type` 的 chat 目录条目
        // （models.json 用 modelFromJson 覆盖内置 chat id 的场景）；此时不套该模板。
        // image/classifier 模板也带 `type`，但查表要求运行时对象自带对应 `type`
        // （此时 `has_type == true`，条件为真），所以不会走到这个排除分支。
        if let Some(template) = template.as_object()
            && (has_type || !template_has_type)
        {
            merge_catalog_extra_keys(map, template);
            reorder_from_template(map, template);
            return;
        }
    }
    // 两种情况会落到这里：目录里查不到，或查到了但因上面的 `type` 判别被跳过
    // （例如 models.json 覆盖内置 chat id：运行时对象不带 `type`、模板带 `"type":"chat"`）。
    // 没有 `type` 的对象按 modelFromJson 处理：models.json 自定义模型不写 `type`；
    // 扩展模型由 `extensionModelFromDefinition` 按 definition 展开构造
    // （`spec/ca-config-models.md` §2.19.5），definition 未写 `type` 时同样没有 `type`，
    // 也会走到这里。有 `type` 但不在目录里的（远端刷新目录）Rust 侧没有它的键序来源，
    // 保持原样。
    if !has_type {
        reorder_model_from_json(map);
    }
}

/// models.json 自定义模型 -> `modelFromJson` 字面量键序（只重排顶层；嵌套对象保留 Rust
/// 结构体序，TS 的嵌套键序来自用户 JSON，无法还原）。
fn reorder_model_from_json(map: &mut Map<String, Value>) {
    let mut order = Vec::new();
    push_present(map, MODEL_FROM_JSON_ORDER, &mut order);
    push_remaining(map, &mut order);
    apply_key_order(map, &order);
}

/// 模型对象的签名守卫：五个字符串字段 + `input` 数组 + `cost` 对象。
fn model_identity(map: &Map<String, Value>) -> Option<(&str, &str)> {
    for key in ["id", "name", "api", "provider", "baseUrl"] {
        map.get(key).and_then(Value::as_str)?;
    }
    if !map.get("input").is_some_and(Value::is_array)
        || !map.get("cost").is_some_and(Value::is_object)
    {
        return None;
    }
    Some((
        map.get("provider").and_then(Value::as_str)?,
        map.get("id").and_then(Value::as_str)?,
    ))
}

/// 在 kiana-ai 的内嵌目录表里按 `provider` + `id` 查找模型原文。
///
/// `id` 在不同表里可能重复（chat / image / classifier 各自独立），TS 用 `type` 判别：
/// 缺失或 `"chat"` 走 chat 表（chat 是默认类型），`"image"` / `"classifier"` 各走各表。
fn lookup_catalog_model(
    map: &Map<String, Value>,
    provider: &str,
    id: &str,
) -> Option<&'static Value> {
    use kiana_ai::providers::catalog;
    match map.get("type").and_then(Value::as_str) {
        Some("image") => catalog::get_image_model(provider, id),
        Some("classifier") => catalog::get_classifier_model(provider, id),
        _ => catalog::get_model(provider, id),
    }
}

/// TS `applyModelOverride` 字面量里的键序：目录里没有、但被 models.json 覆盖层补上的键
/// 会按这个顺序追加在已有键之后。
const MODEL_OVERRIDE_APPEND_ORDER: &[&str] = &[
    "name",
    "reasoning",
    "thinkingLevelMap",
    "input",
    "inputLimits",
    "cost",
    "promptCache",
    "contextWindow",
    "maxTokens",
    "samplingParams",
    "samplingParamsByThinkingLevel",
    "compat",
];

/// 按 `template` 的键序重排 `map`：模板里存在的键按模板序在前，其余键按 TS 覆盖层的追加序
/// 在后（TS 的 `{...base, ...override}` 语义：已有键原位覆盖，新键追加）。递归处理两边都是
/// 对象的字段。
fn reorder_from_template(map: &mut Map<String, Value>, template: &Map<String, Value>) {
    let mut ordered: Vec<String> = template
        .keys()
        .filter(|key| map.contains_key(*key))
        .cloned()
        .collect();
    let mut extras: Vec<String> = map
        .keys()
        .filter(|key| !template.contains_key(*key))
        .cloned()
        .collect();
    // 稳定排序：覆盖层字面量里的键按字面量顺序，其余保持原相对顺序。
    extras.sort_by_key(|key| {
        MODEL_OVERRIDE_APPEND_ORDER
            .iter()
            .position(|known| known == key)
            .unwrap_or(usize::MAX)
    });
    ordered.extend(extras);
    // 已经是目标顺序时跳过重建，避免无谓的搬移。
    if ordered.iter().ne(map.keys()) {
        let mut previous = std::mem::take(map);
        for key in ordered {
            if let Some(value) = previous.shift_remove(&key) {
                map.insert(key, value);
            }
        }
        // `ordered` 已覆盖所有键；防御性保留（正常为空）。
        map.extend(previous);
    }
    // 两边同为对象的字段继续按模板递归（cost/inputLimits/compat 等）；
    // 模板里没有的字段保持 Rust 结构体序。
    for (key, value) in map.iter_mut() {
        if let (Value::Object(child), Some(Value::Object(template_child))) =
            (value, template.get(key))
        {
            reorder_from_template(child, template_child);
        }
    }
}

fn reorder_object(map: &mut Map<String, Value>) {
    match map.get("role").and_then(Value::as_str) {
        Some("system") => {
            // 守卫 = TS withToolChanges 产物的最小签名：有 tool 变更，且 content 与
            // timestamp 都在；否则保持原序。缺 timestamp 的情形由测试锁定
            // （`kiana-rpc-print/tests/json_event_parity.rs`）；缺 content（timestamp
            // 存在）的分支目前没有测试覆盖。
            let has_tool_changes =
                map.contains_key("toolsAdded") || map.contains_key("toolsRemoved");
            if has_tool_changes && map.contains_key("content") && map.contains_key("timestamp") {
                // 必须用 `shift_remove`：`Map::remove` 在 preserve_order 下是 `swap_remove`，
                // 会把末位键换到删除位置，破坏其余键的相对顺序。
                let added = map.shift_remove("toolsAdded");
                let removed = map.shift_remove("toolsRemoved");
                if let Some(added) = added {
                    map.insert("toolsAdded".to_string(), added);
                }
                if let Some(removed) = removed {
                    map.insert("toolsRemoved".to_string(), removed);
                }
            }
        }
        Some("assistant") => {
            // 只处理“带 stopReason 的 assistant 形状”（测试锁定：缺 stopReason 时保持
            // 原序）。完整消息随后会被 reorder_assistant_object 整体重建；这里覆盖的是
            // 不满足完整签名、但仍带 thinkingLevel 的消息。
            if map.contains_key("thinkingLevel")
                && map.contains_key("stopReason")
                && let Some(level) = map.shift_remove("thinkingLevel")
            {
                map.insert("thinkingLevel".to_string(), level);
            }
        }
        _ => {}
    }
}
