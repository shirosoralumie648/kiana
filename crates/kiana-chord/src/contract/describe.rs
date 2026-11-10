//! `contract/describe.ts`：`Description` 的 serde 形状、`DescriptionSchema` 的等价物、
//! `describe_member` 与指纹（TS 里这些都在 `contract.ts`，骨架按 spec/chord-core.md §8.1 拆到本文件）。
//!
//! TS 的 `DescriptionSchema` 是 TypeBox schema（`contract.ts:122-156`）；`Description` 是
//! `Static<typeof DescriptionSchema>`。Rust 侧同时提供：
//!
//! - 强类型 [`Description`]（serde 结构，字段顺序 == TS 接口声明顺序 == 输出顺序）；
//! - [`description_schema`] / [`member_description_schema`] / [`placement_schema`]：与 TypeBox 产物
//!   **逐字节相同**的 JSON Schema（`isFrame` 的 hold.token 校验、`fromFrame` 的等价物都靠它）。
//!
//! `DescriptionSchema.input` 与成员 schema 都是 `Type.Unsafe<TSchema>(Type.Object({}))`，即
//! `{"type":"object","properties":{}}`——"任意对象"。`TSchema` 是空接口，任何对象都算。
//!
//! ## 与相邻模块的分工
//!
//! - `contract/mod.rs` 负责**构造**：`remote()`/`local()` 调用 `describe_member` 把用户声明变成
//!   [`Description`]；`contract_from_description_json` 用 [`Description::from_json`] 从链路 token
//!   还原。
//! - 本文件负责**形状**：强类型结构、TypeBox 等价的 JSON Schema、序列化与指纹。
//! - `contract/coverage.rs` 在这些描述上做覆盖判定（`coverage`/`bind`）。
//!
//! ## 序列化就是协议
//!
//! 描述序列化后的 JSON 是 `hold` 帧 `token` 字段的协议形状（`DescriptionSchema`），并参与
//! `hash64(canonicalJson(description))`，所以本文件的 `#[serde(...)]` 属性不是风格问题：
//! 字段顺序、缺省键、额外字段都直接影响指纹，是稳定契约（改动需同步测试）。
//!
//! Rust 侧的线上类型是 [`crate::wire::DescriptionToken`]：它只声明 `DescriptionSchema` 列出的
//! 6 个字段，反序列化时忽略未知字段。因此 `Description::extra` 与 `MemberDescription::extra`
//! 只在 [`Description::from_json`]（`contract_from_description_json` 路径）中保留；经
//! `DescriptionToken` 往返会丢掉这些键，指纹也随之改变。
//!
//! - schema 位置在序列化时剥掉内部 `~kind` 标记（`serialize_schema`）；漏剥会让指纹与 TS 不同。
//! - `Method` 的 `args`/`result` 缺省时整个键不出现；`host` 对 local 契约同样缺省。
//! - 反序列化保留未知字段（`extra`，`#[serde(flatten)]`）：在 [`Description::from_json`] 路径上
//!   这保证含扩展字段的描述与发送方指纹一致，但线上 `DescriptionToken` 不承载它们（见上）。
//!
//! ## 键顺序
//!
//! `IndexMap` 保留插入序，而 JS 普通对象会把数组下标样式的键提前并按数值升序。描述中的
//! `variants`/成员表由 `contract` 模块在构造时经 [`js_object_reorder`] 调整为 JS 键序；
//! 覆盖判定（`coverage`/`takes_no_args`）与逐字节输出都依赖它。

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::Placement;
use crate::errors::{ChordError, ChordErrorCode, invalid};
use crate::schema::{canonical_json, check, hash64, strip_live_markers};

/// 序列化 schema 时剥除 `~kind` 标记：`JSON.stringify` 看不见 TypeBox 的非枚举 `~kind`，
/// 线上描述里也绝不能出现 Rust 侧的标记键。
///
/// `strip_live_markers` 返回新值，不修改内存中的 schema；它只进入 schema 位置，不会误删
/// `const`/`default`/`enum` 数据里的同名键。
fn serialize_schema<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    strip_live_markers(value).serialize(serializer)
}

/// [`serialize_schema`] 的 `Option` 版本。
fn serialize_schema_opt<S>(value: &Option<Value>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(value) => serializer.serialize_some(&strip_live_markers(value)),
        None => serializer.serialize_none(),
    }
}

/// 描述里的一个成员（TS: `MemberDescription`，`contract.ts:124-134` 的三个分支）。
///
/// serde 用 `tag = "kind"` 内部标记；序列化时 `kind` 在前、其余按声明顺序，与 TS 的对象字面量
/// `{ kind: "method", ...(args), ...(result) }` 一致。
///
/// # 不变量与线格式
///
/// - `kind` 由 serde 的内部标记生成，始终是第一个键；其余字段按变体声明顺序输出。
/// - 构造器（[`MemberDescription::method`] 等）不剥除 schema 里的 `~kind`；剥除发生在序列化
///   （`serialize_schema` / `serialize_schema_opt`）以及 `fits` 内部，所以内存中的描述可以和
///   `remote()` 的活 schema 共存，指纹不受影响。
/// - `extra`（serde 的 flatten map，不含 schema 内部的关键字）里的额外字段参与序列化、相等比较
///   与指纹，但 `coverage` 不读取它们：两份描述只差 `extra` 时仍判为覆盖；指纹不同只意味着
///   `bind` 不能在“同构”短路处返回，仍需按 version/coverage 继续判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MemberDescription {
    /// `{ kind: "method", args?, result? }`：缺省的 `args` 表示无参数，缺省的 `result` 表示无返回值。
    Method {
        /// 参数 schema；缺省 = 无参数。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_schema_opt"
        )]
        args: Option<Value>,
        /// 结果 schema；缺省 = 无返回值。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_schema_opt"
        )]
        result: Option<Value>,
        /// 线格式上多出来的字段：TS 的 token 是普通对象，`contractFromDescription` 原样保留
        /// 并计入指纹，所以 serde 也必须留住它们（`#[serde(flatten)]`）。
        #[serde(flatten)]
        extra: IndexMap<String, Value>,
    },
    /// `{ kind: "state", schema }`。
    State {
        /// 状态成员的 schema。
        #[serde(serialize_with = "serialize_schema")]
        schema: Value,
        /// 见 [`MemberDescription::Method`] 的 `extra`。
        #[serde(flatten)]
        extra: IndexMap<String, Value>,
    },
    /// `{ kind: "opaque", id }`。
    Opaque {
        /// `name@version`。
        id: String,
        /// 见 [`MemberDescription::Method`] 的 `extra`。
        #[serde(flatten)]
        extra: IndexMap<String, Value>,
    },
}

impl MemberDescription {
    /// `{ kind: "method" }`：无参数、无返回值。
    pub fn method() -> Self {
        MemberDescription::Method {
            args: None,
            result: None,
            extra: IndexMap::new(),
        }
    }

    /// `{ kind: "method", args?, result? }`。
    pub fn method_with(args: Option<Value>, result: Option<Value>) -> Self {
        MemberDescription::Method {
            args,
            result,
            extra: IndexMap::new(),
        }
    }

    /// `{ kind: "state", schema }`。
    pub fn state(schema: Value) -> Self {
        MemberDescription::State {
            schema,
            extra: IndexMap::new(),
        }
    }

    /// `{ kind: "opaque", id }`。
    pub fn opaque(id: impl Into<String>) -> Self {
        MemberDescription::Opaque {
            id: id.into(),
            extra: IndexMap::new(),
        }
    }

    /// TS 的 `member.kind`。
    pub fn kind(&self) -> MemberKind {
        match self {
            MemberDescription::Method { .. } => MemberKind::Method,
            MemberDescription::State { .. } => MemberKind::State,
            MemberDescription::Opaque { .. } => MemberKind::Opaque,
        }
    }

    /// 方法成员的 `args`。注意"无参数"与"不是方法"都返回 `None`，与 TS 里
    /// `member.args === undefined` 的两种情形一致。
    pub fn args(&self) -> Option<&Value> {
        match self {
            MemberDescription::Method { args, .. } => args.as_ref(),
            _ => None,
        }
    }

    /// 方法成员的 `result`。
    pub fn result(&self) -> Option<&Value> {
        match self {
            MemberDescription::Method { result, .. } => result.as_ref(),
            _ => None,
        }
    }

    /// 状态成员的 schema。
    pub fn schema(&self) -> Option<&Value> {
        match self {
            MemberDescription::State { schema, .. } => Some(schema),
            _ => None,
        }
    }

    /// opaque 成员的 id。
    pub fn id(&self) -> Option<&str> {
        match self {
            MemberDescription::Opaque { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// `member.kind`（`"method" | "state" | "opaque"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// `"method"`。
    Method,
    /// `"state"`。
    State,
    /// `"opaque"`。
    Opaque,
}

impl MemberKind {
    /// 与 TS 字符串字面量一致。
    pub fn as_str(self) -> &'static str {
        match self {
            MemberKind::Method => "method",
            MemberKind::State => "state",
            MemberKind::Opaque => "opaque",
        }
    }
}

/// `description.scope`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DescriptionScope {
    /// `"remote"`。
    Remote,
    /// `"local"`。
    Local,
}

impl DescriptionScope {
    /// 与 TS 字符串字面量一致。
    pub fn as_str(self) -> &'static str {
        match self {
            DescriptionScope::Remote => "remote",
            DescriptionScope::Local => "local",
        }
    }
}

/// 变体名 → 成员名 → 成员描述。顺序是 JS 对象键序（见 [`super::js_object_reorder`]）。
pub type MemberMap = IndexMap<String, MemberDescription>;

/// 契约描述（TS: `Description`）。随引用的契约 token 上链路，所以只声明一次、按 schema 校验。
///
/// 字段顺序 == TS 接口声明顺序 `{name, scope, version, input, host?, variants}` == 输出顺序；
/// `host` 只有 remote 契约才有（缺省不出现在 JSON 里）。
///
/// # 不变量
///
/// - `contract_from_description` 要求 remote 描述的 `host` 为 `Some`、local 为 `None`；
///   [`Description::from_json`] 的 schema 本身不强制这一点。
/// - 由 [`Description::local`] 构造的 local 契约，`variants` 只有 [`super::LOCAL_VARIANT`]
///   （空串）一个键；从 token 读回的 local 描述不受此限制（schema 只要求它是对象）。
/// - 字段都是 `pub`，直接构造不会校验。需要 schema 形状校验时用 [`Description::from_json`]；
///   需要 host/scope 配套等契约级检查时用 [`super::contract_from_description`]（JSON 入口
///   [`super::contract_from_description_json`]）；需要 `name`/`version` 与成员规则的构造校验时用
///   `remote()`/`local()`。注意 `remote()`/`local()` 同样不校验 `input` 的 schema（见
///   [`Description::local`]），`from_json` 也不检查 `version` 下限。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    /// 类型名。
    pub name: String,
    /// `remote` 或 `local`。
    pub scope: DescriptionScope,
    /// 版本（正整数）。
    pub version: i64,
    /// input 的 schema。
    #[serde(serialize_with = "serialize_schema")]
    pub input: Value,
    /// remote 契约的地点；local 契约没有这个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Placement>,
    /// 变体与成员。
    pub variants: IndexMap<String, MemberMap>,
    /// 线格式上多出来的字段：TS 的 `contractFromDescription` 只检查四个字段，然后把整个
    /// description 对象计入指纹（`hash64(canonicalJson(description))`），因此额外字段必须
    /// 原样保留，否则同一 token 在 TS/Rust 两侧的指纹不同。
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl Description {
    /// 构造 local 契约的描述（唯一变体名是 [`super::LOCAL_VARIANT`]）。
    ///
    /// 本函数不做任何校验。`super::local` 构造契约时用 `check_header` 校验 `name`/`version`、
    /// 用 `describe_member` 落实成员规则（保留名拒绝、按品牌生成描述，local scope 下 opaque
    /// 合法）；`input` 只经 `prepare_schema` 做键序归一化与 `~kind` 标记，**不做** schema 校验——
    /// `local()` 接受任意 JSON 值的 `input`，产出的描述可能通不过 [`description_schema`]。
    pub fn local(name: impl Into<String>, version: i64, input: Value, members: MemberMap) -> Self {
        let mut variants = IndexMap::new();
        variants.insert(super::LOCAL_VARIANT.to_owned(), members);
        Description {
            name: name.into(),
            scope: DescriptionScope::Local,
            version,
            input,
            host: None,
            variants,
            extra: IndexMap::new(),
        }
    }

    /// 该 variant 的成员表（不存在则为 `None`）。
    pub fn members(&self, variant: &str) -> Option<&MemberMap> {
        self.variants.get(variant)
    }

    /// 单个成员（不存在则为 `None`）。
    pub fn member(&self, variant: &str, name: &str) -> Option<&MemberDescription> {
        self.variants
            .get(variant)
            .and_then(|members| members.get(name))
    }

    /// 按 JS `Object.values(variants)` 顺序遍历成员表。
    ///
    /// 顺序即 `variants` 的插入序；构造路径已按 JS 键序重排（见 [`js_object_reorder`]）。
    pub fn variant_members(&self) -> impl Iterator<Item = &MemberMap> {
        self.variants.values()
    }

    /// 序列化成 JSON（字段顺序即 TS 的对象字面量顺序）。
    ///
    /// 前提：字段都是 `Value`/字符串/整数，序列化不会失败。
    ///
    /// schema 字段在序列化时剥掉 `~kind`；本文件的 `description_round_trips_and_hashes` 锁定了一个
    /// 固定描述的逐字节输出。
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("Description is always JSON-serializable")
    }

    /// 按 [`description_schema`] 校验并反序列化；不合法即 `invalid/token`
    /// `"malformed contract token"`。
    ///
    /// 检查顺序：先 `check(description_schema(), value)` 做 TypeBox 等价校验，再
    /// `serde_json::from_value`；两步失败都归一成同一条 `malformed contract token`，不暴露 serde
    /// 的错误文本。schema 只在**顶层与成员级**接受额外字段（这些位置没有
    /// `additionalProperties: false`），它们被收进 `extra` 并计入指纹；`host` 位置的
    /// [`placement_schema`] 两个分支都带 `additionalProperties: false`，多出的键让 `check` 失败，
    /// 整个 token 被拒（`extra` 也收不到）。schema 也**不**检查 scope 与 host 是否配套、不检查
    /// `version` 下限；host/scope 的配套检查在 `contract_from_description`。
    pub fn from_json(value: &Value) -> Result<Self, ChordError> {
        if !check(&description_schema(), value) {
            return Err(malformed_token());
        }
        serde_json::from_value(value.clone()).map_err(|_| malformed_token())
    }

    /// 契约指纹：`hash64(canonicalJson(description))`。
    ///
    /// 16 个小写十六进制字符（`hash64` 是双路 FNV-1a，非加密散列，只用于相等比较）。先做
    /// `canonical_json`（对象键排序），所以键的插入顺序不影响结果，但任何字段值（含 `extra`）
    /// 都影响。
    pub fn fingerprint(&self) -> String {
        hash64(&canonical_json(&self.to_json()))
    }
}

/// `invalid/token` `"malformed contract token"`。
pub(crate) fn malformed_token() -> ChordError {
    invalid("token", "malformed contract token")
}

/// `Type.Unsafe<TSchema>(Type.Object({}))` 的 JSON：任意对象。
///
/// `remote_from_json` 在缺省 `input` 时用它占位；线格式里 `input`/`args`/`result`/state schema
/// 的“任意对象”位置也是同一形状。
pub fn schema_object() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// `MemberDescriptionSchema`（`contract.ts:124-133`）的逐字节等价物。
///
/// 三个 `anyOf` 分支都允许额外字段（没有 `additionalProperties: false`）；`args`/`result`/
/// `schema` 的位置是“任意 JSON 对象”（`null`/数组/标量不通过）。
pub fn member_description_schema() -> Value {
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "kind": {"type": "string", "const": "method"},
                    "args": {"type": "object", "properties": {}},
                    "result": {"type": "object", "properties": {}}
                }
            },
            {
                "type": "object",
                "required": ["kind", "schema"],
                "properties": {
                    "kind": {"type": "string", "const": "state"},
                    "schema": {"type": "object", "properties": {}}
                }
            },
            {
                "type": "object",
                "required": ["kind", "id"],
                "properties": {
                    "kind": {"type": "string", "const": "opaque"},
                    "id": {"type": "string"}
                }
            }
        ]
    })
}

/// `PlacementSchema`（`contract.ts:139-145`）的逐字节等价物。
///
/// 两个分支都 `additionalProperties: false` 且要求字段非空——这是描述里唯一会拒绝额外字段的
/// 位置；[`super::Placement::is_valid_json`] 直接用它校验。
pub fn placement_schema() -> Value {
    serde_json::json!({
        "anyOf": [
            {
                "type": "object",
                "required": ["name"],
                "properties": {"name": {"type": "string", "minLength": 1}},
                "additionalProperties": false
            },
            {
                "type": "object",
                "required": ["prefix", "key"],
                "properties": {
                    "prefix": {"type": "string", "minLength": 1},
                    "key": {"type": "string", "minLength": 1}
                },
                "additionalProperties": false
            }
        ]
    })
}

/// `DescriptionSchema`（`contract.ts:147-155`）的逐字节等价物。
///
/// `Type.Record(Type.String(), …)` 编译成 `{"type":"object","patternProperties":{"^.*$": …}}`；
/// 注意它**没有** `additionalProperties: false`（多余字段被接受）。
///
/// 用法：[`Description::from_json`] 的入口校验；`host` 位置内嵌 [`placement_schema`]，成员位置
/// 内嵌 [`member_description_schema`]。`version` 只要求整数、没有下限——`>= 1` 由构造侧的
/// `check_header` 保证，token 校验不重复检查（TS 的 `DescriptionSchema` 同样只有
/// `Type.Integer()`）。
pub fn description_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "scope", "version", "input", "variants"],
        "properties": {
            "name": {"type": "string", "minLength": 1},
            "scope": {"anyOf": [
                {"type": "string", "const": "remote"},
                {"type": "string", "const": "local"}
            ]},
            "version": {"type": "integer"},
            "input": {"type": "object", "properties": {}},
            "host": placement_schema(),
            "variants": {
                "type": "object",
                "patternProperties": {
                    "^.*$": {
                        "type": "object",
                        "patternProperties": {
                            "^.*$": member_description_schema()
                        }
                    }
                }
            }
        }
    })
}

/// `describe_member(name, member, scope)`（`contract.ts:192-208`，TS 里是私有函数）。
///
/// - 保留名 → `TypeError("member name is reserved: …")`；
/// - brand `method` → `{kind:"method", ...(args===undefined?{}:{args}), ...(result===undefined?{}:{result})}`；
/// - brand `opaque`：remote scope → `TypeError("remote contracts cannot expose opaque members: …")`，
///   否则 `{kind:"opaque", id}`；
/// - 其余（TSchema）→ `{kind:"state", schema: member}`（**不检查它是否真是 schema**）。
///
/// TS 抛 `TypeError`；Rust 侧统一为 `ChordError { code: Invalid, reason: Some("contract") }`
/// （本文件的私有 `type_error` 构造）。调用方可用 [`super::ContractError`] 还原 TS 的
/// `name == "TypeError"` 分类。
///
/// # 边界
///
/// - 检查顺序：先保留名，再按变体分派；两条错误都是 `invalid/contract`。
/// - method 的 `args`/`result` 与 state 的 `schema` 都**不做** schema 校验：state 分支把任意
///   `Value` 原样放进 `{kind:"state",schema}`。这意味着这样造出的描述可能通不过
///   [`description_schema`] 的入口校验（例如 schema 不是对象时）。
pub(crate) fn describe_member(
    name: &str,
    member: &super::LocalMember,
    scope: DescriptionScope,
) -> Result<MemberDescription, ChordError> {
    if super::is_reserved_member(name) {
        return Err(type_error(format!("member name is reserved: {name}")));
    }
    match member {
        super::LocalMember::Method(spec) => Ok(MemberDescription::method_with(
            spec.args.clone(),
            spec.result.clone(),
        )),
        super::LocalMember::Opaque(spec) => {
            if scope == DescriptionScope::Remote {
                return Err(type_error(format!(
                    "remote contracts cannot expose opaque members: {name}"
                )));
            }
            Ok(MemberDescription::opaque(spec.id.clone()))
        }
        super::LocalMember::State(schema) => Ok(MemberDescription::state(schema.clone())),
    }
}

/// TS 的 `TypeError`：`errors.ts` 没有把它映射成 `ChordError`；Rust 侧用 `invalid/contract` 承载
/// （`ChordError::code` 只有 12 个公开码，`TypeError` 不在其中——调用方按 `reason == "contract"`
/// 分辨，见 spec §3.4 的 contract 段）。
pub(crate) fn type_error(message: String) -> ChordError {
    ChordError::new(ChordErrorCode::Invalid, message).with_reason("contract")
}

/// JS 对象键序：数字样式的键（数组下标）在前、按数值升序；其余按插入序。
///
/// ECMAScript `OrdinaryOwnPropertyKeys`：先按升序列出整数索引，再按创建顺序列出字符串键。
/// `indexmap` 只保留插入序，所以凡是会被 `Object.entries`/`Object.keys` 观察的地方都要经过它
/// （spec/chord-rest.md §4 的键序条目）。
///
/// 构造描述时由 [`js_object_reorder`] 调用；返回新的 `Vec<String>`，不修改入参。
pub fn js_object_order<V>(map: &IndexMap<String, V>) -> Vec<String> {
    let mut indices: Vec<(u32, &String)> = Vec::new();
    let mut strings: Vec<&String> = Vec::new();
    for key in map.keys() {
        match array_index(key) {
            Some(index) => indices.push((index, key)),
            None => strings.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, key)| key.clone())
        .chain(strings.into_iter().cloned())
        .collect()
}

/// 把 map 按 JS 键序重排（构造描述时用）。
///
/// 返回新 map（值 `Clone`），不修改入参；每个键恰好出现一次，长度不变。
pub fn js_object_reorder<V: Clone>(map: &IndexMap<String, V>) -> IndexMap<String, V> {
    let mut out = IndexMap::with_capacity(map.len());
    for key in js_object_order(map) {
        if let Some(value) = map.get(&key) {
            out.insert(key, value.clone());
        }
    }
    out
}

/// 该键是否是 JS 的数组下标（`ToString(ToUint32(k)) === k && ToUint32(k) !== 2^32-1`）。
///
/// `"0"` 是；`"01"`/`"-1"`/`"1.5"`/`"4294967295"`/`""` 不是。
///
/// 返回 `None` 表示该键按普通字符串键处理（保持插入序）。可接受的最大索引是 `4294967294`
/// （`2^32 - 2`）；`4294967295`（`2^32 - 1`）不是数组的合法下标。
pub fn array_index(key: &str) -> Option<u32> {
    // u32 的最大值只有 10 位十进制，更长的键不可能是数组下标。
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    let bytes = key.as_bytes();
    if bytes[0] == b'0' {
        return if bytes.len() == 1 { Some(0) } else { None };
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value >= u32::MAX as u64 {
        return None;
    }
    Some(value as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_index_matches_js() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("12"), Some(12));
        assert_eq!(array_index("01"), None);
        assert_eq!(array_index("-1"), None);
        assert_eq!(array_index("1.5"), None);
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index(""), None);
        assert_eq!(array_index("a"), None);
    }

    #[test]
    fn js_object_order_puts_indices_first() {
        let mut map: IndexMap<String, i32> = IndexMap::new();
        map.insert("b".into(), 1);
        map.insert("2".into(), 2);
        map.insert("a".into(), 3);
        map.insert("1".into(), 4);
        assert_eq!(js_object_order(&map), vec!["1", "2", "b", "a"]);
    }

    #[test]
    fn member_description_serializes_like_typescript() {
        assert_eq!(
            serde_json::to_string(&MemberDescription::method()).unwrap(),
            "{\"kind\":\"method\"}"
        );
        assert_eq!(
            serde_json::to_string(&MemberDescription::method_with(
                Some(serde_json::json!({"type": "string"})),
                None
            ))
            .unwrap(),
            "{\"kind\":\"method\",\"args\":{\"type\":\"string\"}}"
        );
        assert_eq!(
            serde_json::to_string(&MemberDescription::state(
                serde_json::json!({"type": "number"})
            ))
            .unwrap(),
            "{\"kind\":\"state\",\"schema\":{\"type\":\"number\"}}"
        );
        assert_eq!(
            serde_json::to_string(&MemberDescription::opaque("h@1")).unwrap(),
            "{\"kind\":\"opaque\",\"id\":\"h@1\"}"
        );
    }

    #[test]
    fn description_round_trips_and_hashes() {
        let mut members: MemberMap = IndexMap::new();
        members.insert(
            "view".into(),
            MemberDescription::state(serde_json::json!({"type": "object"})),
        );
        let mut variants = IndexMap::new();
        variants.insert("viewer".to_owned(), members);
        let description = Description {
            name: "t.chat".into(),
            scope: DescriptionScope::Remote,
            version: 1,
            input: serde_json::json!({"type": "object"}),
            host: Some(Placement::named("session")),
            variants,
            extra: IndexMap::new(),
        };
        let json = description.to_json();
        assert_eq!(
            serde_json::to_string(&json).unwrap(),
            "{\"name\":\"t.chat\",\"scope\":\"remote\",\"version\":1,\"input\":{\"type\":\"object\"},\"host\":{\"name\":\"session\"},\"variants\":{\"viewer\":{\"view\":{\"kind\":\"state\",\"schema\":{\"type\":\"object\"}}}}}"
        );
        assert!(check(&description_schema(), &json));
        let back = Description::from_json(&json).unwrap();
        assert_eq!(back, description);
        assert_eq!(description.fingerprint(), back.fingerprint());
        assert_eq!(description.fingerprint().len(), 16);
    }
}
