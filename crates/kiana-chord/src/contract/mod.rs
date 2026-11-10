//! contract 目录：`contract.ts` 的 `method`/`opaque`/`hostNamed`/`hostFamily`/`remote`/`local`/
//! `contractFromDescription`/`resolveHost`/`placedOn`/`placementText`/`LOCAL_VARIANT`/
//! `RESERVED_MEMBERS`/`DescriptionSchema`/`coverage`/`bind`/`takesNoArgs`。
//!
//! 本 crate 内不得出现第二份实现。
//!
//! ## 为什么 `TypeError` 变成 `ChordError`
//!
//! TS 的契约构造函数抛 `TypeError`（不是 `ChordError`）。Rust 的公开错误面是
//! [`ChordError`]（`ARCHITECTURE.md` §4.1：公开 `Display` 必须逐字等于 TS 的 `error.message`），
//! 而 `TypeError` 的 `message` 在 TS 里**不带** `code: ` 前缀、`name` 是 `"TypeError"`。
//!
//! 因此 Rust 侧统一为 `ChordError { code: Invalid, reason: Some("contract"), message: <TS 原文> }`，
//! 并额外提供 [`ContractError`]（`name()` 返回 `"TypeError"`）描述这一分类。调用方判断
//! `error.name === "TypeError"` 的地方（TS 里靠 `instanceof TypeError`）改用
//! `error.reason == Some("contract")`。
//!
//! ## 键顺序
//!
//! `variants`/`members` 在 TS 里是普通对象，`Object.entries` 的顺序是 **JS 对象键序**（数字样式的键
//! 在前、升序；其余按插入序）。构造描述时统一过 [`describe::js_object_reorder`]，因此
//! `description.variants` 的迭代顺序与 TS 逐项一致（`coverage`/`takesNoArgs` 按它遍历；指纹由
//! `canonicalJson` 对键排序后计算，本身与键序无关；描述 JSON 的键序另有测试直接断言）。
//!
//! ## 这个文件负责什么
//!
//! 本文件是 `contract.ts` 的 Rust 骨架：把用户声明的 [`RemoteDef`]/[`LocalDef`] 变成携带
//! [`Description`] 与 fingerprint 的契约值。三块职责分开放置：
//!
//! - `describe`：描述类型、TypeBox schema 的逐字节等价物、成员品牌分派（`describe_member`）；
//! - `coverage`：跨版本绑定判定（`coverage`/`bind`/`takes_no_args`）；
//! - 本文件：常量与类型、`remote`/`local` 的构造流程、从描述重建 token。
//!
//! 本 crate 的 `src` 内，`index.rs` 只做再导出、`types.rs` 把 [`AnyContract`] 用作引用类型；
//! 完整的构造与绑定行为由 `tests/contract.rs` 覆盖，`tests/types.rs` 覆盖契约在类型层的使用。
//!
//! ## 一次 `remote()` 的执行流程
//!
//! 1. `check_header`：空名字、版本 < 1 → `invalid/contract`；
//! 2. `prepare_remote_def`：把 input 与所有成员 schema 按 JS 自有键序重排，并在**可推断
//!    TypeBox 种类**的 schema 节点上打 `~kind` 标记（标量/未知形状的 schema 不带；标记表示
//!    "本进程内构造的活 schema"，见 `schema::mark_live_schema`）；
//! 3. 复制 `def.host` 并按 `PlacementSchema` 校验；variant 不能为空；
//! 4. 逐 variant、先 state 后 methods 调用 `describe_member`（实现在 `describe` 里），同时按
//!    JS 对象的 `in` 语义拒绝同名的 state/method；
//! 5. 组装 [`Description`]（字段顺序与键序照 TS 的对象形状），用
//!    `hash64(canonicalJson(description))` 补出 [`ContractBase::fingerprint`]。
//!
//! [`local()`] 走同一条构造路径，但只有一张 members 表、允许 opaque、描述里没有 `host`，
//! variant 名固定为 [`LOCAL_VARIANT`]。
//!
//! ## 注意事项
//!
//! - 构造会**就地改写**传入的 def（键序，以及可推断种类的节点上的 `~kind` 标记）；
//!   `description` 序列化时由 `describe` 的 serde 钩子剥掉标记，所以标记不上线、也不进指纹。
//! - 指纹只由 `description` 决定；对端用 [`contract_from_description`] 重算同一指纹，两侧必须
//!   逐字节一致，否则同一契约会得到不同指纹（`canonicalJson` 对键排序，数字格式与字符串转义
//!   由 `schema` 侧负责）。
//! - 构造错误统一是 `invalid/contract`（逐字复刻 TS 的 `TypeError` 消息）；token 重建错误是
//!   `invalid/token` `"malformed contract token"`。

pub mod coverage;
pub mod describe;

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{ChordError, ChordErrorCode, invalid};
use crate::schema::{
    canonical_json, check, fits, hash64, mark_live_schema, normalize, reorder_js_keys,
};

pub use coverage::{Accepts, Adapter, bind, coverage, takes_no_args};
pub use describe::{
    Description, DescriptionScope, MemberDescription, MemberKind, MemberMap, array_index,
    description_schema, js_object_order, js_object_reorder, member_description_schema,
    placement_schema, schema_object,
};
// TS 里 `describe_member` 是 contract.ts 的**私有**函数（`contract.ts:192`），不导出。
pub(crate) use describe::{describe_member, malformed_token};

/// 成员品牌（TS: `const MEMBER = Symbol("chord.member")`）。
///
/// Rust 侧不需要运行期 symbol：成员是 [`LocalMember`] / [`MethodSpec`] / [`OpaqueSpec`] 的枚举
/// 变体，品牌由类型承载。这个常量保留给"按品牌字符串判别"的调试/序列化路径。
pub const MEMBER: &str = "chord.member";

/// local 契约的唯一（无名）variant 名。
///
/// TS: `export const LOCAL_VARIANT = ""`。
pub const LOCAL_VARIANT: &str = "";

/// 引用自己使用的名字；成员不得占用它们。
///
/// TS: `RESERVED_MEMBERS`（`contract.ts:179-190`），顺序即 TS 的插入顺序（`Set` 迭代序）。
pub const RESERVED_MEMBERS: [&str; 10] = [
    "variant", "status", "ready", "shape", "error", "onShape", "onStatus", "release", "then",
    "contract",
];

/// `RESERVED_MEMBERS.has(name)`。
///
/// 精确匹配：区分大小写，不做 Unicode 归一化。`describe_member` 对 state 与 method 成员都会先做
/// 这项检查，命中即 `invalid/contract` `"member name is reserved: …"`（`describe.rs`）。
pub fn is_reserved_member(name: &str) -> bool {
    RESERVED_MEMBERS.contains(&name)
}

/// `RESERVED_MEMBERS` 的集合视图（顺序仍是插入序；`indexmap` 的 `IndexSet` 保证）。
pub fn reserved_members() -> indexmap::IndexSet<&'static str> {
    RESERVED_MEMBERS.into_iter().collect()
}

/// 方法的参数与结果；`None`：不接收参数，或什么都不返回（void）。
///
/// TS: `interface MethodSpec<A, R> { readonly [MEMBER]: "method"; readonly args: A; readonly result: R }`。
/// `method()` = 无参数无返回值；`method(args)` = 有参数无返回值；`method(args, result)`；
/// `method(undefined, result)` = 无参数有返回值。
///
/// `None` 与 `Some(Value::Null)` 不等价：`None` 让描述里**缺省** `args`/`result` 键，
/// 而 `Some(Value::Null)` 会真的写出 `null`（`member_description_schema` 要求它是对象，
/// `null` 不合法）。"无参数/无返回值"一律用 `None` 表达。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MethodSpec {
    /// 参数 schema（`None` = 无参数）。
    pub args: Option<Value>,
    /// 结果 schema（`None` = 无返回值）。
    pub result: Option<Value>,
}

impl MethodSpec {
    /// `method()`。
    pub fn new() -> Self {
        MethodSpec::default()
    }

    /// `method(args)`。
    pub fn args(args: Value) -> Self {
        MethodSpec {
            args: Some(args),
            result: None,
        }
    }

    /// `method(args, result)` / `method(undefined, result)`。
    pub fn with(args: Option<Value>, result: Option<Value>) -> Self {
        MethodSpec { args, result }
    }
}

/// `method()`：无参数（调用形式 `ref.name(ctx)`，实现形式 `(ctx) => …`）且无返回值。
pub fn method() -> MethodSpec {
    MethodSpec::new()
}

/// `method(args)`：有参数、无返回值。
pub fn method_args(args: Value) -> MethodSpec {
    MethodSpec::args(args)
}

/// `method(args, result)`：有参数、有返回值。
pub fn method_with(args: Value, result: Value) -> MethodSpec {
    MethodSpec {
        args: Some(args),
        result: Some(result),
    }
}

/// `method(undefined, result)`：无参数、有返回值。
pub fn method_result(result: Value) -> MethodSpec {
    MethodSpec {
        args: None,
        result: Some(result),
    }
}

/// 由 `name@version` 标识的原生进程内对象（只允许出现在 local 契约里）。
///
/// TS: `interface OpaqueSpec<T> { readonly [MEMBER]: "opaque"; readonly id: string; readonly type?: T }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueSpec {
    /// `name@version`。
    pub id: String,
}

impl OpaqueSpec {
    /// `opaque(id)`：`id` 必须匹配 `^[^@\s]+@\d+$`。
    ///
    /// 不匹配 → `invalid/contract` `opaque id must be name@version: {id}`（见 [`is_opaque_id`]）。
    pub fn new(id: impl Into<String>) -> Result<Self, ChordError> {
        let id = id.into();
        if !is_opaque_id(&id) {
            return Err(type_error(format!("opaque id must be name@version: {id}")));
        }
        Ok(OpaqueSpec { id })
    }

    /// 已校验过的 id（调用方保证合法）。
    ///
    /// 构造时不再跑正则，`local()`/描述序列化也不会重验：非法 id 会原样进入 description
    /// 与指纹。只在 id 来自 [`OpaqueSpec::new`] 或已知合法常量时使用。
    pub fn unchecked(id: impl Into<String>) -> Self {
        OpaqueSpec { id: id.into() }
    }
}

/// `opaque(id)`（TS 的函数形式）。
///
/// 校验与错误消息见 [`OpaqueSpec::new`]。
pub fn opaque(id: impl Into<String>) -> Result<OpaqueSpec, ChordError> {
    OpaqueSpec::new(id)
}

/// `/^[^@\s]+@\d+$/`。
///
/// JS 的 `\s` 集是 `[\t\n\v\f\r    -     　﻿]`；
/// `\d` 是 ASCII 数字。Rust 用 `[^@\s]` 需要与 JS 的 `\s` 一致，因此在下面手写字符类。
pub fn is_opaque_id(id: &str) -> bool {
    let mut chars = id.chars().peekable();
    let mut name_len = 0usize;
    while let Some(&ch) = chars.peek() {
        if ch == '@' {
            break;
        }
        if is_js_whitespace(ch) {
            return false;
        }
        chars.next();
        name_len += 1;
    }
    // `@` 必须存在，名字非空，且其余全是 ASCII 数字且至少一位。
    if name_len == 0 || chars.next() != Some('@') {
        return false;
    }
    let mut digits = 0usize;
    for ch in chars {
        if !ch.is_ascii_digit() {
            return false;
        }
        digits += 1;
    }
    digits > 0
}

/// JS 正则的 `\s`（WhiteSpace + LineTerminator）。
pub fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// 契约成员（TS: `type LocalMember = MethodSpec | OpaqueSpec | TSchema`）。
///
/// 判别顺序与 TS 的 `describe_member` 相同：先品牌 `"method"`，再 `"opaque"`，其余是 state schema。
#[derive(Debug, Clone, PartialEq)]
pub enum LocalMember {
    /// `method(…)`。
    Method(MethodSpec),
    /// `opaque("name@version")`。
    Opaque(OpaqueSpec),
    /// 状态成员的 schema（TS 的 `TSchema`，不做"它真是 schema"的检查）。
    State(Value),
}

impl LocalMember {
    /// 枚举变体名（对应 TS 的 `MEMBER` 品牌串）；state 没有品牌。
    pub fn brand(&self) -> Option<&'static str> {
        match self {
            LocalMember::Method(_) => Some("method"),
            LocalMember::Opaque(_) => Some("opaque"),
            LocalMember::State(_) => None,
        }
    }

    /// 方法成员。
    pub fn as_method(&self) -> Option<&MethodSpec> {
        match self {
            LocalMember::Method(spec) => Some(spec),
            _ => None,
        }
    }

    /// opaque 成员。
    pub fn as_opaque(&self) -> Option<&OpaqueSpec> {
        match self {
            LocalMember::Opaque(spec) => Some(spec),
            _ => None,
        }
    }

    /// 状态成员 schema。
    pub fn as_state(&self) -> Option<&Value> {
        match self {
            LocalMember::State(schema) => Some(schema),
            _ => None,
        }
    }
}

impl From<MethodSpec> for LocalMember {
    fn from(spec: MethodSpec) -> Self {
        LocalMember::Method(spec)
    }
}

impl From<OpaqueSpec> for LocalMember {
    fn from(spec: OpaqueSpec) -> Self {
        LocalMember::Opaque(spec)
    }
}

impl From<Value> for LocalMember {
    fn from(schema: Value) -> Self {
        LocalMember::State(schema)
    }
}

/// remote 类型的地点：一个具名 host。
///
/// TS: `interface HostNamed { readonly name: string }`；`Placement = HostNamed | HostFamily`。
/// 序列化是**数据**（进描述与指纹），因此用 untagged 枚举 + 字段顺序 `{name}` / `{prefix,key}`。
///
/// 线上 `PlacementSchema` 的两个分支都写了 `additionalProperties: false`（协议里唯一需要
/// `deny_unknown_fields` 的地方，见 spec chord-core §A4）：带多余字段的 placement 在 TS 侧会让整个
/// `hold` 帧 `isFrame` 失败。这里的 [`Placement::from_json`] 复刻该检查。
///
/// ## 不变量与构造途径
///
/// 两个变体的字段都是公开的，直接写字面量可以绕过 [`host_named`]/[`host_family`] 的非空检查；
/// [`remote`] 在构造描述前会用 `PlacementSchema` 再验一次，[`remote_from_json`] 也从
/// [`Placement::from_json`] 进入同一检查。空 `name`/`prefix`/`key` 因此不会经由这两个构造入口
/// 进入描述（手工构造 [`Description`] 再走 [`contract_from_description`] 不在这条路径上）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Placement {
    /// `hostNamed("server")`：`{ name }`。
    Named {
        /// host 名。
        name: String,
    },
    /// `hostFamily("session:", "session")`：`{ prefix, key }`。
    Family {
        /// host 名前缀。
        prefix: String,
        /// input 里的字符串字段名。
        key: String,
    },
}

impl Placement {
    /// `hostNamed(name)`。
    ///
    /// 不做非空校验（与 TS 的 `hostNamed` 不同）：空串会在 [`remote`]/[`remote_from_json`]
    /// 按 `PlacementSchema` 复验时被拒。需要在构造点就报错时用 [`host_named`]。
    pub fn named(name: impl Into<String>) -> Placement {
        Placement::Named { name: name.into() }
    }

    /// `hostFamily(prefix, key)`。
    ///
    /// 同样不做非空校验：空 `prefix`/`key` 要到 [`remote`]/[`remote_from_json`] 复验时才报错；
    /// 需要立即校验用 [`host_family`]。
    pub fn family(prefix: impl Into<String>, key: impl Into<String>) -> Placement {
        Placement::Family {
            prefix: prefix.into(),
            key: key.into(),
        }
    }

    /// TS 的 `"name" in placement`。
    pub fn is_named(&self) -> bool {
        matches!(self, Placement::Named { .. })
    }

    /// 具名 host 的名字。
    pub fn name(&self) -> Option<&str> {
        match self {
            Placement::Named { name } => Some(name),
            Placement::Family { .. } => None,
        }
    }

    /// family 的 `(prefix, key)`。
    pub fn family_parts(&self) -> Option<(&str, &str)> {
        match self {
            Placement::Family { prefix, key } => Some((prefix, key)),
            Placement::Named { .. } => None,
        }
    }

    /// `placementText(placement)`：named → `name`；family → `` `${prefix}<${key}>` ``。
    pub fn text(&self) -> String {
        match self {
            Placement::Named { name } => name.clone(),
            Placement::Family { prefix, key } => format!("{prefix}<{key}>"),
        }
    }

    /// `placedOn(placement, hostName)`：named → 相等；family → 以 prefix 开头且**严格更长**
    /// （`session:` 不能 serve 在 `session:` 本身）。
    pub fn placed_on(&self, host_name: &str) -> bool {
        match self {
            Placement::Named { name } => name == host_name,
            Placement::Family { prefix, .. } => {
                host_name.starts_with(prefix.as_str()) && host_name.len() > prefix.len()
            }
        }
    }

    /// JSON 副本：`"name" in placement ? {name} : {prefix, key}`（多余字段被丢弃）。
    ///
    /// Rust 的 [`Placement`] 是枚举，本来就不可能带多余字段，因此这里等价于逐字段拷贝；
    /// 保留独立方法是为了对应 TS `remote()` 里的"复制 placement"步骤。
    pub fn json_copy(&self) -> Placement {
        match self {
            Placement::Named { name } => Placement::Named { name: name.clone() },
            Placement::Family { prefix, key } => Placement::Family {
                prefix: prefix.clone(),
                key: key.clone(),
            },
        }
    }

    /// 是否通过 `PlacementSchema`（`{name}` 或 `{prefix,key}`，两者都
    /// `additionalProperties: false`，两个字段都 `minLength: 1`）。
    ///
    /// 这是 [`remote`] 对已类型化 placement 的复验入口，也是 [`Placement::from_json`] 的第一道
    /// 检查；schema 的逐字节定义在 `describe::placement_schema`。
    pub fn is_valid_json(value: &Value) -> bool {
        check(&placement_schema(), value)
    }

    /// 从 JSON 读 placement（按 `PlacementSchema` 校验）。
    pub fn from_json(value: &Value) -> Option<Placement> {
        if !Self::is_valid_json(value) {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

/// `hostNamed(name)`：`name` 必须是非空字符串。
///
/// 空串 → `invalid/contract` `"hostNamed needs a host name"`。
pub fn host_named(name: impl Into<String>) -> Result<Placement, ChordError> {
    let name = name.into();
    if name.is_empty() {
        return Err(type_error("hostNamed needs a host name".to_owned()));
    }
    Ok(Placement::Named { name })
}

/// `hostFamily(prefix, key)`：两者都必须是非空字符串。
///
/// 空 `prefix` → `invalid/contract` `"hostFamily needs a host-name prefix"`；空 `key` →
/// `"hostFamily needs an input field name"`。
pub fn host_family(
    prefix: impl Into<String>,
    key: impl Into<String>,
) -> Result<Placement, ChordError> {
    let prefix = prefix.into();
    if prefix.is_empty() {
        return Err(type_error("hostFamily needs a host-name prefix".to_owned()));
    }
    let key = key.into();
    if key.is_empty() {
        return Err(type_error(
            "hostFamily needs an input field name".to_owned(),
        ));
    }
    Ok(Placement::Family { prefix, key })
}

/// `resolveHost(typeName, placement, input)`：对象在这个 input 下活在哪台 host。
///
/// named → `placement.name`；family → 读 `input[placement.key]`（仅当 `input` 是非 null 对象），
/// 若不是非空字符串 → `invalid/placement` `` `${typeName}: input field ${placement.key} names no host` ``；
/// 否则 `prefix + value`。
///
/// Rust 侧只把 `Value::Object` 当作对象：input 是 JSON 数组或 `null` 时一律视为没有该字段。
/// TS 的守卫是 `input !== null && typeof input === "object"`（`contract.ts:87-88`）：`null` 在
/// 两边都视为没有该字段；真正的差异只在数组——TS 会把数组当对象做属性读取。
pub fn resolve_host(
    type_name: &str,
    placement: &Placement,
    input: &Value,
) -> Result<String, ChordError> {
    match placement {
        Placement::Named { name } => Ok(name.clone()),
        Placement::Family { prefix, key } => {
            let value = match input {
                Value::Object(map) => map.get(key),
                _ => None,
            };
            match value.and_then(Value::as_str) {
                Some(text) if !text.is_empty() => Ok(format!("{prefix}{text}")),
                _ => Err(invalid(
                    "placement",
                    format!("{type_name}: input field {key} names no host"),
                )),
            }
        }
    }
}

/// `placedOn(placement, hostName)`。
pub fn placed_on(placement: &Placement, host_name: &str) -> bool {
    placement.placed_on(host_name)
}

/// `placementText(placement)`。
pub fn placement_text(placement: &Placement) -> String {
    placement.text()
}

/// remote 类型的定义（TS: `RemoteDef`）。
///
/// [`remote`] 接收后会把 schema 归一化再放回返回值的 `def`：input/成员 schema 的键序变成 JS
/// 自有键序；可推断 TypeBox 种类的节点还会带 `~kind` 标记（标量/未知形状的 schema 不带；
/// `prepare_schema` 的 `live = true` 路径）。
#[derive(Debug, Clone)]
pub struct RemoteDef {
    /// 版本（正整数）。
    pub version: i64,
    /// input 的 schema。
    pub input: Value,
    /// 对象活在哪：`hostNamed(…)` 或 `hostFamily(…)`。属于契约（与指纹）。
    pub host: Placement,
    /// 变体 → 成员。
    pub variants: IndexMap<String, VariantDef>,
}

/// 一个 variant 的定义（TS: `VariantDef`）。
///
/// TS 的 `state`/`methods` 在类型上是 `Record<string, TSchema>` / `Record<string, MethodSpec>`，
/// 但运行期都不校验：`describeMember` 按 `MEMBER` 品牌分派，`methods.x` 若不是 `method()` 则报
/// `` `methods.x is not a method()` ``（见 `remote()`）。Rust 侧两个表都用 [`LocalMember`] 承载以
/// 保留这些运行期路径；`Value: Into<LocalMember>`、`MethodSpec: Into<LocalMember>`，所以常规用法
/// 仍然是 `.state("view", schema)` / `.method("send", method(…))`。
#[derive(Debug, Clone, Default)]
pub struct VariantDef {
    /// 状态成员（名字 → 成员），顺序即插入序。
    pub state: IndexMap<String, LocalMember>,
    /// 方法成员（名字 → 成员），顺序即插入序。
    pub methods: IndexMap<String, LocalMember>,
}

impl VariantDef {
    /// 空变体。
    pub fn new() -> Self {
        VariantDef::default()
    }

    /// 加一个状态成员（普通 schema 或带品牌的成员）。
    pub fn state(mut self, name: impl Into<String>, member: impl Into<LocalMember>) -> Self {
        self.state.insert(name.into(), member.into());
        self
    }

    /// 加一个方法成员（常规是 `MethodSpec`）。
    pub fn method(mut self, name: impl Into<String>, member: impl Into<LocalMember>) -> Self {
        self.methods.insert(name.into(), member.into());
        self
    }
}

/// local 类型的定义（TS: `LocalDef`）。
///
/// 与 [`RemoteDef`] 一样，[`local`] 返回的 `def` 里 schema 已被归一化（键序 + 可推断种类的
/// 节点上的 `~kind`）。
#[derive(Debug, Clone)]
pub struct LocalDef {
    /// 版本（正整数）。
    pub version: i64,
    /// input 的 schema。
    pub input: Value,
    /// 成员（名字 → `LocalMember`），顺序即插入序。
    pub members: IndexMap<String, LocalMember>,
}

/// 契约的公共部分（TS: `interface ContractBase`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ContractBase {
    /// 类型名。
    pub name: String,
    /// 版本。
    pub version: i64,
    /// input 的 schema。
    pub input: Value,
    /// 描述（随 token 上链路）。
    pub description: Description,
    /// `hash64(canonicalJson(description))`。
    pub fingerprint: String,
}

/// 远程契约（TS: `RemoteContract<D>`）。
#[derive(Debug, Clone)]
pub struct RemoteContract {
    /// 公共部分。
    pub base: ContractBase,
    /// 定义（`contractFromDescription` 造的 token 只有 `version`/`input`/`host`，`variants` 为空）。
    pub def: RemoteDef,
}

/// 本地契约（TS: `LocalContract<D>`）。
#[derive(Debug, Clone)]
pub struct LocalContract {
    /// 公共部分。
    pub base: ContractBase,
    /// 定义（token 只有 `version`/`input`）。
    pub def: LocalDef,
}

/// 任意契约（TS: `AnyContract = RemoteContract | LocalContract`）。
#[derive(Debug, Clone)]
pub enum AnyContract {
    /// `remote(…)`。
    Remote(RemoteContract),
    /// `local(…)`。
    Local(LocalContract),
}

impl AnyContract {
    /// `contract.name`。
    pub fn name(&self) -> &str {
        match self {
            AnyContract::Remote(c) => &c.base.name,
            AnyContract::Local(c) => &c.base.name,
        }
    }

    /// `contract.version`。
    pub fn version(&self) -> i64 {
        match self {
            AnyContract::Remote(c) => c.base.version,
            AnyContract::Local(c) => c.base.version,
        }
    }

    /// `contract.input`。
    pub fn input(&self) -> &Value {
        match self {
            AnyContract::Remote(c) => &c.base.input,
            AnyContract::Local(c) => &c.base.input,
        }
    }

    /// `contract.description`。
    pub fn description(&self) -> &Description {
        match self {
            AnyContract::Remote(c) => &c.base.description,
            AnyContract::Local(c) => &c.base.description,
        }
    }

    /// `contract.fingerprint`。
    pub fn fingerprint(&self) -> &str {
        match self {
            AnyContract::Remote(c) => &c.base.fingerprint,
            AnyContract::Local(c) => &c.base.fingerprint,
        }
    }

    /// `contract.scope`。
    pub fn scope(&self) -> DescriptionScope {
        match self {
            AnyContract::Remote(_) => DescriptionScope::Remote,
            AnyContract::Local(_) => DescriptionScope::Local,
        }
    }

    /// `contract.scope === "remote"`。
    pub fn is_remote(&self) -> bool {
        matches!(self, AnyContract::Remote(_))
    }

    /// `contract.scope === "local"`。
    pub fn is_local(&self) -> bool {
        matches!(self, AnyContract::Local(_))
    }

    /// 公共部分。
    pub fn base(&self) -> &ContractBase {
        match self {
            AnyContract::Remote(c) => &c.base,
            AnyContract::Local(c) => &c.base,
        }
    }

    /// remote 定义（local 为 `None`）。
    pub fn remote_def(&self) -> Option<&RemoteDef> {
        match self {
            AnyContract::Remote(c) => Some(&c.def),
            AnyContract::Local(_) => None,
        }
    }

    /// local 定义（remote 为 `None`）。
    pub fn local_def(&self) -> Option<&LocalDef> {
        match self {
            AnyContract::Local(c) => Some(&c.def),
            AnyContract::Remote(_) => None,
        }
    }
}

impl ContractBase {
    /// `contract.description`。
    pub fn description(&self) -> &Description {
        &self.description
    }

    /// `contract.fingerprint`。
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

impl From<RemoteContract> for AnyContract {
    fn from(contract: RemoteContract) -> Self {
        AnyContract::Remote(contract)
    }
}

impl From<LocalContract> for AnyContract {
    fn from(contract: LocalContract) -> Self {
        AnyContract::Local(contract)
    }
}

/// `finish(contract)`：补上 `fingerprint = hash64(canonicalJson(description))`。
///
/// 指纹只依赖 `description`，不读 `def` 的其余内容；因此构造端与 [`contract_from_description`]
/// 消费端能算出同一个值。`description.to_json()` 会剥掉活 schema 的 `~kind` 标记（见 `describe`）。
fn finish(base: ContractBase) -> ContractBase {
    let fingerprint = hash64(&canonical_json(&base.description.to_json()));
    ContractBase {
        fingerprint,
        ..base
    }
}

/// `checkHeader(name, version)`：`!name` → `"contract name is empty"`；
/// `!Number.isInteger(version) || version < 1` → `"contract version must be a positive integer"`。
///
/// `Number.isInteger` 这一半在 Rust 是类型层面的（`version: i64`），这里只剩 `< 1`；
/// [`remote_from_json`] 把缺失/非整数的 JSON version 归为 0，因此落到同一条消息。
fn check_header(name: &str, version: i64) -> Result<(), ChordError> {
    if name.is_empty() {
        return Err(type_error("contract name is empty".to_owned()));
    }
    if version < 1 {
        return Err(type_error(
            "contract version must be a positive integer".to_owned(),
        ));
    }
    Ok(())
}

/// `Object.prototype` 上（`in` 能看见）的全部自有键。
///
/// 注意这里**不是** `Guard.HasPropertyKey` 的安全性分类：`contract.ts` 的 `key in members`
/// 是普通 `in`，因此 `constructor`/`__proto__` 与 `toString` 等一样恒为真。
const OBJECT_PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "toLocaleString",
    "__proto__",
];

/// 成员描述对象上的自有键（`members[key]` 赋值给 `__proto__` 后，它成为 `members` 的原型，
/// 这些键会让 `key in members` 为真）。
fn member_has_own_key(member: &MemberDescription, key: &str) -> bool {
    match member {
        MemberDescription::Method {
            args,
            result,
            extra,
        } => {
            key == "kind"
                || (key == "args" && args.is_some())
                || (key == "result" && result.is_some())
                || extra.contains_key(key)
        }
        MemberDescription::State { extra, .. } => {
            key == "kind" || key == "schema" || extra.contains_key(key)
        }
        MemberDescription::Opaque { extra, .. } => {
            key == "kind" || key == "id" || extra.contains_key(key)
        }
    }
}

/// `key in members`（成员表是普通 JS 对象：自有键 + 原型链）。
///
/// 三个来源：`members` 的自有键、`__proto__` state 赋值后形成的原型对象上的自有键
/// （`member_has_own_key`）、以及 `Object.prototype` 上的名字（`OBJECT_PROTOTYPE_KEYS`）。
fn members_have_key(members: &MemberMap, proto: Option<&MemberDescription>, key: &str) -> bool {
    if members.contains_key(key) {
        return true;
    }
    if proto.is_some_and(|member| member_has_own_key(member, key)) {
        return true;
    }
    OBJECT_PROTOTYPE_KEYS.contains(&key)
}

/// `members[key] = member`：`__proto__` 触发 setter（替换原型，不建自有键）。
///
/// 因此名称为 `__proto__` 的成员不会出现在描述里（`Object.entries` 看不到它）；
/// `tests/contract.rs` 的 `local_members_keep_prototype_names_but_drop_proto` 锁定该行为。
fn members_set(
    members: &mut MemberMap,
    proto: &mut Option<MemberDescription>,
    key: String,
    member: MemberDescription,
) {
    if key == "__proto__" {
        *proto = Some(member);
    } else {
        members.insert(key, member);
    }
}

/// 契约声明里的 schema 准备：按 JS 自有键序重排；`live` 时打上 `~kind`。
///
/// `remote()`/`local()` 的 schema 来自本进程内构造的 TypeBox 对象（活 schema，带不可枚举的
/// `~kind`）；`remote_from_json()` 的 schema 来自 JSON（JSON.parse 后没有 `~kind`），
/// `Default`/`Clean` 在 TS 里对它们是近空操作。见 `schema::mark_live_schema`。
fn prepare_schema(schema: &mut Value, live: bool) {
    let ordered = reorder_js_keys(schema);
    *schema = if live {
        mark_live_schema(&ordered)
    } else {
        ordered
    };
}

/// 对单个成员里的全部 schema 做 [`prepare_schema`]；opaque 成员没有 schema，跳过。
///
/// `MethodSpec` 的 `args`/`result` 分别处理（两者都可为 `None`）。
fn prepare_member(member: &mut LocalMember, live: bool) {
    match member {
        LocalMember::Method(spec) => {
            if let Some(args) = spec.args.as_mut() {
                prepare_schema(args, live);
            }
            if let Some(result) = spec.result.as_mut() {
                prepare_schema(result, live);
            }
        }
        LocalMember::State(schema) => prepare_schema(schema, live),
        LocalMember::Opaque(_) => {}
    }
}

/// 归一化整个 remote def：`input` 以及每个 variant 的 state/methods 成员。
///
/// 只改 schema 内部（键序与 `~kind`），不改变 `variants`/成员表本身的 `IndexMap` 插入序。
fn prepare_remote_def(def: &mut RemoteDef, live: bool) {
    prepare_schema(&mut def.input, live);
    for variant in def.variants.values_mut() {
        for member in variant.state.values_mut() {
            prepare_member(member, live);
        }
        for member in variant.methods.values_mut() {
            prepare_member(member, live);
        }
    }
}

/// 归一化整个 local def：`input` 与全部成员。
fn prepare_local_def(def: &mut LocalDef, live: bool) {
    prepare_schema(&mut def.input, live);
    for member in def.members.values_mut() {
        prepare_member(member, live);
    }
}

/// `remote(name, def)`（`contract.ts:219-252`）。
///
/// 把 remote 契约声明构造成 [`RemoteContract`]：校验头部/`host`/`variants`，逐成员生成
/// [`MemberDescription`]，组装 [`Description`] 并计算指纹。
///
/// 校验顺序与消息（逐字，全部映射为 `invalid/contract`）：
/// 1. `check_header`：名字为空 / 版本 < 1；
/// 2. `def.host` 必须通过 `PlacementSchema`。`hostNamed`/`hostFamily` 已保证非空，但
///    [`Placement`] 的字段是公开的，空 `name`/`prefix`/`key` 在这里被拒；
/// 3. `def.variants` 为空 → `` `remote contract ${name} has no variants` ``；
/// 4. 每个 variant（`def.variants` 的插入序）：先 state、后 methods；
///    - state 成员走 `describe::describe_member`：保留名 → `member name is reserved: …`；
///      remote scope 下的 opaque → `remote contracts cannot expose opaque members: …`
///      （methods 里的 opaque 也先触发这条，而不是 `is not a method()`）；
///    - method 名与已有成员冲突（JS 的 `key in members`，包含 `Object.prototype` 的键与
///      `__proto__` 赋值的效果）→ `` `member ${key} is both state and method in ${name}.${kind}` ``；
///    - methods 里不是 `method()` 的值 → `` `methods.${key} is not a method()` ``。
///
/// 构造会就地归一化 `def`（见 `prepare_remote_def`）：schema 按 JS 自有键序排列，可推断 TypeBox
/// 种类的节点上还打 `~kind`（标量/未知形状的 schema 不带）；`description` 从归一化后的 `def`
/// 派生，序列化时再剥掉标记。指纹只由 `description` 决定
/// （`hash64(canonicalJson(...))`），见 `finish`。
///
/// # 错误
///
/// 上述校验全部返回 `ChordError { code: Invalid, reason: Some("contract") }`；成功时返回的
/// 契约同时带 `base`（description + fingerprint）与归一化后的 `def`。
pub fn remote(name: impl Into<String>, def: RemoteDef) -> Result<RemoteContract, ChordError> {
    build_remote(name.into(), def, true)
}

/// [`remote()`] 的共同实现；`live` 见 [`prepare_schema`]。
fn build_remote(
    name: String,
    mut def: RemoteDef,
    live: bool,
) -> Result<RemoteContract, ChordError> {
    check_header(&name, def.version)?;
    // description 必须由归一化后的 def 派生：`tests/contract.rs` 既把 `description.to_json()`
    // 与 TS 生成的 golden 逐值比对，也用 `contract_input_keys_follow_javascript_own_key_order`
    // 直接断言 schema 内部键序。
    prepare_remote_def(&mut def, live);
    let host = def.host.json_copy();
    // host 虽已是类型化的 Placement，但字段是公开的、可能为空串；这里按 PlacementSchema 复验。
    if !Placement::is_valid_json(&serde_json::to_value(&host).expect("placement is JSON")) {
        return Err(type_error(format!(
            "remote contract {name} needs `host`: hostNamed(…) or hostFamily(…)"
        )));
    }
    if def.variants.is_empty() {
        return Err(type_error(format!(
            "remote contract {name} has no variants"
        )));
    }
    let mut variants: IndexMap<String, MemberMap> = IndexMap::new();
    // 每个 variant 的成员先按"state 全部、再 methods"的顺序处理：这既是 TS 的赋值顺序
    // （决定 `in` 检查看到什么），也决定描述里的成员迭代顺序；最后再按 JS 键序重排。
    for (kind, variant) in &def.variants {
        let mut members: MemberMap = IndexMap::new();
        let mut proto: Option<MemberDescription> = None;
        for (key, state_member) in &variant.state {
            let member = describe_member(key, state_member, DescriptionScope::Remote)?;
            members_set(&mut members, &mut proto, key.clone(), member);
        }
        for (key, spec) in &variant.methods {
            // TS: `if (key in members)` —— 普通 `in`，原型链上的名字也算。
            if members_have_key(&members, proto.as_ref(), key) {
                return Err(type_error(format!(
                    "member {key} is both state and method in {name}.{kind}"
                )));
            }
            let member = describe_member(key, spec, DescriptionScope::Remote)?;
            if member.kind() != MemberKind::Method {
                return Err(type_error(format!("methods.{key} is not a method()")));
            }
            members_set(&mut members, &mut proto, key.clone(), member);
        }
        // `variants[kind] = members`：kind 为 `__proto__` 时只替换原型，不产生自有键
        // （`Object.entries(description.variants)` 看不见它）。
        if kind == "__proto__" {
            continue;
        }
        // 成员名同样按 JS 对象键序排列（数字样式的名字在前）。
        variants.insert(kind.clone(), js_object_reorder(&members));
    }
    let description = Description {
        name: name.clone(),
        scope: DescriptionScope::Remote,
        version: def.version,
        input: def.input.clone(),
        host: Some(host),
        variants: js_object_reorder(&variants),
        extra: IndexMap::new(),
    };
    let base = finish(ContractBase {
        name,
        version: def.version,
        input: def.input.clone(),
        description,
        fingerprint: String::new(),
    });
    Ok(RemoteContract { base, def })
}

/// `remote(name, def)` 的原始 JSON 入口：`host` 是未校验的 JSON。
///
/// TS 的 `remote` 接收任意对象，`def.host` 若不是 `hostNamed(…)`/`hostFamily(…)` 的合法形状
/// （包括带多余字段）就抛
/// `` `remote contract ${name} needs \`host\`: hostNamed(…) or hostFamily(…)` ``。
/// `RemoteDef.host` 是强类型 [`Placement`]，那种非法值无法构造，因此需要这种输入的调用方
/// （例如从帧里读回定义）用本函数。
///
/// # 解析规则（Rust 特有入口）
///
/// - `version` 用 `Value::as_i64` 读取；缺失或不是 JSON 整数（例如写成 `1.0`）按 0 处理，
///   随后由 `check_header` 报 `contract version must be a positive integer`；
/// - `host` 必须通过 `PlacementSchema`（`additionalProperties: false`，字段非空）；
/// - `input` 缺省为 `{"type":"object","properties":{}}`（`describe::schema_object`）；
/// - `variants` 缺省或不是对象 → 空表，随后报 `remote contract ${name} has no variants`；
/// - 每个 variant 只读 `state`/`methods` 两个对象：`state` 的值原样作为 state schema；
///   `methods` 的每个值按 `{args?, result?}` 读成 [`MethodSpec`]，**不检查**它是否真是方法形状
///   （缺字段即 `method()`，非对象值同样成了 `method()`）；
/// - 其余未知字段忽略。
///
/// schema 来自 JSON、没有 `~kind`，因此以 `live = false` 调 `build_remote`；得到的 def 在
/// `Default`/`Clean` 下与 TS 的 `JSON.parse` 产物一致（`tests/contract.rs` 的
/// `live_contracts_default_but_json_contracts_do_not` 锁定该差异）。
pub fn remote_from_json(
    name: impl Into<String>,
    def: &Value,
) -> Result<RemoteContract, ChordError> {
    let name = name.into();
    let version = def.get("version").and_then(Value::as_i64).unwrap_or(0);
    check_header(&name, version)?;
    let host = match def.get("host").and_then(Placement::from_json) {
        Some(host) => host,
        None => {
            return Err(type_error(format!(
                "remote contract {name} needs `host`: hostNamed(…) or hostFamily(…)"
            )));
        }
    };
    let input = def.get("input").cloned().unwrap_or_else(schema_object);
    // JSON 路径不做品牌分派：state 的值原样当 schema；methods 的值无论形状都读成
    // `{args?, result?}`（这与 `remote()` 里会报 "is not a method()" 的路径不同）。
    let mut variants: IndexMap<String, VariantDef> = IndexMap::new();
    if let Some(map) = def.get("variants").and_then(Value::as_object) {
        for (kind, variant) in map {
            let mut parsed = VariantDef::new();
            if let Some(state) = variant.get("state").and_then(Value::as_object) {
                for (key, schema) in state {
                    parsed
                        .state
                        .insert(key.clone(), LocalMember::State(schema.clone()));
                }
            }
            if let Some(methods) = variant.get("methods").and_then(Value::as_object) {
                for (key, spec) in methods {
                    let args = spec.get("args").cloned();
                    let result = spec.get("result").cloned();
                    parsed.methods.insert(
                        key.clone(),
                        LocalMember::Method(MethodSpec { args, result }),
                    );
                }
            }
            variants.insert(kind.clone(), parsed);
        }
    }
    build_remote(
        name,
        RemoteDef {
            version,
            input,
            host,
            variants,
        },
        // JSON 来的 schema 没有 `~kind`（TS 里 `JSON.parse` 也拿不到不可枚举属性）。
        false,
    )
}

/// `local(name, def)`（`contract.ts:254-266`）。
///
/// 成员按 `def.members` 顺序 `describe_member(scope "local")`（允许 opaque）；
/// `description = {name, scope:"local", version, input, variants: {"": members}}`（无 `host`）。
///
/// 与 [`remote`] 共用 `check_header`；没有 host/variants 检查，members 只有一张表。
/// 构造期间同样会就地归一化 `def`（键序 + 可推断种类的节点上的 `~kind`），指纹算法相同
/// （`finish`）。
/// 失败一律 `invalid/contract`（保留名与 opaque 规则来自 `describe::describe_member`，
/// local scope 下 opaque 合法）。
pub fn local(name: impl Into<String>, def: LocalDef) -> Result<LocalContract, ChordError> {
    let name = name.into();
    let mut def = def;
    check_header(&name, def.version)?;
    prepare_local_def(&mut def, true);
    let mut members: MemberMap = IndexMap::new();
    let mut proto: Option<MemberDescription> = None;
    for (key, member) in &def.members {
        members_set(
            &mut members,
            &mut proto,
            key.clone(),
            describe_member(key, member, DescriptionScope::Local)?,
        );
    }
    let description = Description::local(
        name.clone(),
        def.version,
        def.input.clone(),
        js_object_reorder(&members),
    );
    let base = finish(ContractBase {
        name,
        version: def.version,
        input: def.input.clone(),
        description,
        fingerprint: String::new(),
    });
    Ok(LocalContract { base, def })
}

/// 从链路收到的描述造消费者 token（只有描述旅行；没有 `def`，因此只能用于绑定与校验）。
///
/// TS: `contractFromDescription(description)`（`contract.ts:272-286`）。
///
/// 合法性：`name` 是字符串、`scope` 是 `remote`/`local`、`version` 是整数、
/// remote 必须有 host 且 local 必须没有；否则 `invalid/token` `"malformed contract token"`。
/// `def.variants` 为空（描述才是真源）；指纹对收到的描述重新计算。
///
/// 本函数接收已反序列化的 [`Description`]，因此 name/scope/version 的形状由 Rust 类型保证，
/// 它自身只执行一条检查：`(scope == Remote) == host.is_some()`。需要 JSON 级别的完整形状校验
/// 时用 [`contract_from_description_json`]（先过 `DescriptionSchema`：name 非空、input 是对象、
/// host 与成员形状等）。
///
/// 注意空名字：本函数接受空 `name`（TS 只检查 `typeof name !== "string"`），而 JSON 入口的
/// `DescriptionSchema` 有 `minLength: 1`，会在那里被拒。
///
/// 指纹用 `description.to_json()` 重算，`extra` 里的未知字段也原样参与（见 [`Description`]）；
/// 丢弃额外字段会得到与发送方不同的指纹。
pub fn contract_from_description(description: Description) -> Result<AnyContract, ChordError> {
    let remote = description.scope == DescriptionScope::Remote;
    // TS 的检查是 `typeof name !== "string"`（**不是**空串检查：空名字在这里被接受）。Rust 侧
    // `name` 已经是 `String`，因此这一条恒成立，只剩 host 的一致性检查。
    if remote != description.host.is_some() {
        return Err(malformed_token());
    }
    let fingerprint = hash64(&canonical_json(&description.to_json()));
    let base = ContractBase {
        name: description.name.clone(),
        version: description.version,
        input: description.input.clone(),
        description,
        fingerprint,
    };
    Ok(if remote {
        AnyContract::Remote(RemoteContract {
            def: RemoteDef {
                version: base.version,
                input: base.input.clone(),
                // 上面的一致性检查保证 remote 描述一定带 host，expect 不会触发。
                host: base
                    .description
                    .host
                    .clone()
                    .expect("remote descriptions carry a host"),
                variants: IndexMap::new(),
            },
            base,
        })
    } else {
        AnyContract::Local(LocalContract {
            def: LocalDef {
                version: base.version,
                input: base.input.clone(),
                members: IndexMap::new(),
            },
            base,
        })
    })
}

/// 从 JSON 描述造 token（`fromWireError` 风格的便捷入口）。
pub fn contract_from_description_json(value: &Value) -> Result<AnyContract, ChordError> {
    contract_from_description(Description::from_json(value)?)
}

/// TS 的 `TypeError`：Rust 侧统一成 `invalid/contract`（见模块文档）。
fn type_error(message: String) -> ChordError {
    ChordError::new(ChordErrorCode::Invalid, message).with_reason("contract")
}

/// 调用方可以用来区分"TS 会抛 `TypeError`"的错误。
///
/// TS: `TypeError`；`name` 是 `"TypeError"`，`message` **不带** `code: ` 前缀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    /// TS 的 `error.message`（逐字）。
    pub message: String,
}

impl ContractError {
    /// TS 的 `error.name`。
    pub const NAME: &'static str = "TypeError";

    /// 从 [`ChordError`] 还原（仅当 `code == Invalid` 且 `reason == "contract"`）。
    ///
    /// 匹配只按字段进行：本模块与 `describe` 的 `type_error` 都产生这一组合，无法再区分具体
    /// 来自哪个构造函数。
    pub fn from_chord_error(error: &ChordError) -> Option<ContractError> {
        if error.code == ChordErrorCode::Invalid && error.reason.as_deref() == Some("contract") {
            Some(ContractError {
                message: error.message.clone(),
            })
        } else {
            None
        }
    }
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ContractError {}

/// 契约实现的工厂签名（TS: `Factory<C, P>`）。实现者由 `host`/`object` 单元使用；这里只固定形状。
pub type Factory = Arc<
    dyn Fn(
            Value,
            Value,
            Box<dyn std::any::Any + Send + Sync>,
        ) -> Result<Box<dyn std::any::Any + Send + Sync>, ChordError>
        + Send
        + Sync,
>;

/// 校验一个 input 并返回规范化值（`normalize(schema, input, what)` 的便利封装）。
///
/// TS: `host.#locate` / `object` 方法参数都用 `normalize`；这里只是避免每个调用点都写
/// `crate::schema::normalize(&contract.input(), …)`。
///
/// `input` 为 `None` 或 `Some(Value::Null)` 都等价于 `{}`（`normalize` 的 `value ?? {}`），
/// 随后补默认值、清洗并校验；只有校验失败时错误 reason 才是 `input`，消息前缀是
/// `` `<契约名> input` ``。若 schema 内含非法正则，`normalize` 会在补默认值/清洗/校验**之前**
/// 先返回 `unavailable`（消息形如
/// `SyntaxError: Invalid regular expression: /…/u: invalid regular expression`，没有 reason）。
pub fn normalize_input(contract: &AnyContract, input: Option<&Value>) -> Result<Value, ChordError> {
    normalize(
        contract.input(),
        input,
        &format!("{} input", contract.name()),
    )
}

/// `fits(consumer.input, provider.input)` 的便利封装（`coverage` 的 `"input"` 分支）。
///
/// 方向：consumer 的 input 必须落在 provider 的 input 内，与 `coverage` 里
/// `fits(&consumer.input, &provider.input)` 的参数顺序一致。
pub fn input_fits(provider: &AnyContract, consumer: &AnyContract) -> bool {
    fits(consumer.input(), provider.input())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view() -> Value {
        json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})
    }

    fn chat_input() -> Value {
        json!({
            "type": "object",
            "required": ["session", "conversation"],
            "properties": {
                "session": {"type": "string"},
                "conversation": {"type": "number", "default": 0}
            }
        })
    }

    fn chat_v1() -> RemoteContract {
        let mut variants = IndexMap::new();
        variants.insert("viewer".to_owned(), VariantDef::new().state("view", view()));
        variants.insert(
            "participant".to_owned(),
            VariantDef::new()
                .state("view", view())
                .method(
                    "send",
                    MethodSpec::with(
                        Some(json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})),
                        Some(json!({"type": "object", "required": ["id"], "properties": {"id": {"type": "number"}}})),
                    ),
                ),
        );
        remote(
            "t.chat",
            RemoteDef {
                version: 1,
                input: chat_input(),
                host: Placement::family("session:", "session"),
                variants,
            },
        )
        .unwrap()
    }

    #[test]
    fn opaque_id_regex() {
        assert!(is_opaque_id("h@1"));
        assert!(is_opaque_id("a.b/c@42"));
        assert!(!is_opaque_id("@1"));
        assert!(!is_opaque_id("h@"));
        assert!(!is_opaque_id("h@x"));
        assert!(!is_opaque_id("h@1@2"));
        assert!(!is_opaque_id("h h@1"));
        assert!(!is_opaque_id("h@1 "));
        assert_eq!(
            opaque("x").unwrap_err().message,
            "opaque id must be name@version: x"
        );
    }

    #[test]
    fn placement_helpers() {
        let named = host_named("server").unwrap();
        assert!(placed_on(&named, "server"));
        assert!(!placed_on(&named, "server2"));
        assert_eq!(placement_text(&named), "server");

        let family = host_family("session:", "session").unwrap();
        assert!(placed_on(&family, "session:S1"));
        assert!(!placed_on(&family, "session:"), "must be strictly longer");
        assert!(!placed_on(&family, "other:S1"));
        assert_eq!(placement_text(&family), "session:<session>");

        assert_eq!(
            host_named("").unwrap_err().message,
            "hostNamed needs a host name"
        );
        assert_eq!(
            host_family("", "k").unwrap_err().message,
            "hostFamily needs a host-name prefix"
        );
        assert_eq!(
            host_family("p", "").unwrap_err().message,
            "hostFamily needs an input field name"
        );
    }

    #[test]
    fn resolve_host_family_reads_input() {
        let family = host_family("session:", "session").unwrap();
        let input = json!({"session": "S1"});
        assert_eq!(resolve_host("t", &family, &input).unwrap(), "session:S1");
        let error = resolve_host("t", &family, &json!({"session": ""})).unwrap_err();
        assert_eq!(error.code, ChordErrorCode::Invalid);
        assert_eq!(error.reason.as_deref(), Some("placement"));
        assert_eq!(error.message, "t: input field session names no host");
        let error = resolve_host("t", &family, &json!({})).unwrap_err();
        assert_eq!(error.message, "t: input field session names no host");
        let error = resolve_host("t", &family, &Value::Null).unwrap_err();
        assert_eq!(error.message, "t: input field session names no host");
        // named 不读 input。
        let named = host_named("server").unwrap();
        assert_eq!(resolve_host("t", &named, &Value::Null).unwrap(), "server");
    }

    #[test]
    fn remote_fingerprint_is_description_based() {
        let a = chat_v1();
        let b = chat_v1();
        assert_eq!(a.base.fingerprint, b.base.fingerprint);
        assert_eq!(a.base.fingerprint.len(), 16);
        assert_eq!(a.base.fingerprint, a.base.description.fingerprint());
    }

    #[test]
    fn remote_rejects_bad_headers_and_members() {
        let variants = || {
            let mut variants = IndexMap::new();
            variants.insert("a".to_owned(), VariantDef::new().state("h", view()));
            variants
        };
        let error = remote(
            "",
            RemoteDef {
                version: 1,
                input: json!({}),
                host: Placement::named("s"),
                variants: variants(),
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "contract name is empty");

        let error = remote(
            "t",
            RemoteDef {
                version: 0,
                input: json!({}),
                host: Placement::named("s"),
                variants: variants(),
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "contract version must be a positive integer");

        let error = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({}),
                host: Placement::named(""),
                variants: variants(),
            },
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "remote contract t needs `host`: hostNamed(…) or hostFamily(…)"
        );

        let error = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({}),
                host: Placement::named("s"),
                variants: IndexMap::new(),
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "remote contract t has no variants");

        // state + method 同名。
        let mut variants = IndexMap::new();
        variants.insert(
            "a".to_owned(),
            VariantDef::new()
                .state("x", view())
                .method("x", MethodSpec::new()),
        );
        let error = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({}),
                host: Placement::named("s"),
                variants,
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "member x is both state and method in t.a");

        // 保留名。
        let mut variants = IndexMap::new();
        variants.insert("a".to_owned(), VariantDef::new().state("status", view()));
        let error = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({}),
                host: Placement::named("s"),
                variants,
            },
        )
        .unwrap_err();
        assert_eq!(error.message, "member name is reserved: status");
    }

    #[test]
    fn local_allows_opaque_and_remote_does_not() {
        let mut members = IndexMap::new();
        members.insert(
            "harness".to_owned(),
            LocalMember::Opaque(OpaqueSpec::unchecked("h@1")),
        );
        let contract = local(
            "t.idx",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members,
            },
        )
        .unwrap();
        assert_eq!(
            contract.base.description.variants[LOCAL_VARIANT]["harness"],
            MemberDescription::opaque("h@1")
        );

        // remote 的 state 成员放 opaque 会走 describe_member 的 remote 分支。
        let mut variants = IndexMap::new();
        variants.insert(
            "a".to_owned(),
            VariantDef {
                state: IndexMap::new(),
                methods: IndexMap::new(),
            }
            .state("h", json!({"type": "string"})),
        );
        let contract = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                host: Placement::named("s"),
                variants,
            },
        )
        .unwrap();
        assert_eq!(
            contract.base.description.variants["a"]["h"].kind(),
            MemberKind::State
        );
    }

    #[test]
    fn contract_from_description_round_trips() {
        let chat = chat_v1();
        let json = chat.base.description.to_json();
        let token = contract_from_description_json(&json).unwrap();
        assert_eq!(token.name(), "t.chat");
        assert_eq!(token.version(), 1);
        assert_eq!(token.fingerprint(), chat.base.fingerprint);
        assert_eq!(token.scope(), DescriptionScope::Remote);
        // token 的 def 只有 version/input/host。
        let def = token.remote_def().unwrap();
        assert!(def.variants.is_empty());
        assert_eq!(def.host, Placement::family("session:", "session"));

        // remote 必须带 host，local 必须没有。
        let mut remote_without_host = json.clone();
        remote_without_host.as_object_mut().unwrap().remove("host");
        let error = contract_from_description_json(&remote_without_host).unwrap_err();
        assert_eq!(error.code, ChordErrorCode::Invalid);
        assert_eq!(error.reason.as_deref(), Some("token"));
        assert_eq!(error.message, "malformed contract token");

        let mut local_with_host = json.clone();
        local_with_host["scope"] = json!("local");
        let error = contract_from_description_json(&local_with_host).unwrap_err();
        assert_eq!(error.message, "malformed contract token");

        // 校验失败（version 不是整数）。
        let mut bad_version = json.clone();
        bad_version["version"] = json!(1.5);
        let error = contract_from_description_json(&bad_version).unwrap_err();
        assert_eq!(error.message, "malformed contract token");
    }

    #[test]
    fn placement_schema_rejects_extra_fields() {
        assert!(Placement::from_json(&json!({"name": "s"})).is_some());
        assert!(Placement::from_json(&json!({"name": "s", "extra": 1})).is_none());
        assert!(Placement::from_json(&json!({"name": ""})).is_none());
        assert!(Placement::from_json(&json!({"prefix": "p:", "key": "k"})).is_some());
        assert!(Placement::from_json(&json!({"prefix": "p:", "key": "k", "x": 1})).is_none());
        assert!(Placement::from_json(&json!({"prefix": "", "key": "k"})).is_none());
        assert!(Placement::from_json(&json!({})).is_none());
    }
}
