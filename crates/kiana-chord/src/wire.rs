//! `wire.ts`：链路帧（`Frame`）、`PROTOCOL_VERSION` 与错误编解码。
//!
//! 帧是 JSON 对象，`t` 为标签。Rust 用 `#[serde(tag = "t")]` 的枚举；字段名沿用协议既有的
//! `camelCase`（`rootId`/`codeVersion`/`gateDeadline`/`retireDeadline` 等都是 camelCase）。
//!
//! ## 23 个标签与方向（协议稳定契约，改动需两端同步）
//!
//! consumer → provider：`hold` `release` `call` `cancel` `sub` `unsub` `resync`
//! provider → consumer：`shape` `gone` `result` `state`
//! 双向：`hello` `ping` `pong` `bye` `root_end` `ack`
//! parent → child：`invalidate` `prepare` `commit` `abort` `retire`
//! child → parent：`vote`
//!
//! ## 校验
//!
//! TS 的 `isFrame` 用 TypeBox 逐帧校验。Rust 侧 `serde` 反序列化严格复刻 schema 的**类型**
//! 约束：可选字段的显式 `null` 被拒绝（TypeBox 的 optional 只表示"可以缺键"）、
//! `Type.Unknown()`/`{}` 载荷仍保留 `null`、`integer` 接受 `1.0`（`Number.isInteger`）、
//! `token.input`/`token.host`/`token.variants` 按 `DescriptionSchema` 校验。除 `resync.m`
//! 外，数值下限与 `minLength`（`id >= 1`、`codeVersion >= 0/1`、`reload >= 1`、`protocol >= 1`、
//! 非空名字）由 [`Frame::validate`] 补做——`serde_json::from_value::<Frame>()` 后再调用一次。
//!
//! **已知不等价处**：`resync.m` 在 schema 里是 `minLength: 1`，但 [`Frame::validate`] 的
//! `Resync` 分支只检查 `hold`。因此 `from_value` + `validate` 会接受
//! `{"t":"resync","hold":1,"m":""}`，而 [`is_frame`] 拒绝它（测试
//! `frame_schema_rejects_loose_shapes` 的 rejects 列表里有这一条，反序列化测试没有覆盖它）。
//! 接收侧应先用 [`is_frame`]（对应 TS 的 `Value.Check`）判定 malformed frame；只走 serde
//! 路径的调用方需要自行补这项检查。
//!
//! ## 为什么需要这个模块
//!
//! 帧的线上形状在 TS 侧只由 `wire.ts` 的 `FrameSchema` + `isFrame` + 两个错误转换函数定义。
//! 本文件把它们收敛成一份 Rust 对应物，避免连接层、host、reload 各自手写 JSON 判断，导致同一
//! 条消息在不同代码路径上被判得不一样。按 `spec/chord-core.md` §5.1 的模块表，`wire` 的使用者
//! 是 connection / host / reload / remote；在仓库当前快照里这些模块仍是骨架占位
//! （`src/connection/mod.rs`、`src/host/mod.rs` 目前只有 TODO 与子模块声明），所以 [`Frame`]
//! 目前只经 [`crate::index`] 再导出、并在本文件底部的单元测试里使用，真正的收发接线随这些模块
//! 的移植落地。
//!
//! ## 校验分成三层，各管一段
//!
//! 接收一条帧的完整严格性由三处校验叠加而成，但它们的角色不同：[`is_frame`] 直接对应 TS 的
//! `Value.Check`（两者等价）；`from_value` 负责把 JSON 构造为 [`Frame`] 并校验 serde 能表达的
//! 类型；[`Frame::validate`] 补 `serde` 表达不了的 `minimum`/`minLength`（`resync.m` 例外，
//! 见上文“已知不等价处”）。
//!
//! 1. `serde_json::from_value::<Frame>()`：字段类型、缺键与显式 `null` 的区分、`token` 的
//!    `DescriptionSchema` 形状、以及 `kind`/`scope`/`as` 等枚举字面量。类型固定的可选字段用
//!    `deserialize_present`（显式 `null` 判错），`Type.Unknown()` 载荷用 `deserialize_some`
//!    （显式 `null` 保留），`protocol`/`state`/`again` 各有专用函数。
//! 2. [`Frame::validate`]：`serde` 类型系统表达不了的 `minimum` 与 `minLength: 1`
//!    （`id`/`hold`/`reload`/`codeVersion`/`protocol` 的下限、名字类字段非空）。
//! 3. [`is_frame`]：直接对原始 `serde_json::Value` 跑内嵌的 `FrameSchema`，对应 TS 的
//!    `Value.Check(FrameSchema, value)`。按 spec §4.2，连接层先做这一步，失败即
//!    `malformed frame` 并关闭链路；Rust 侧等价流程是 `is_frame(&value)` 通过后再
//!    `serde_json::from_value::<Frame>(value)`。文件底部两个回归测试分别锁定这两条路径的
//!    拒绝/接受集合。
//!
//! ## 设计原因与注意事项
//!
//! - **帧结构不加 `deny_unknown_fields`**：`FrameSchema` 的 23 个分支都没有
//!   `additionalProperties: false`，TS 侧会接受多余字段；serde 默认忽略未知字段，二者一致。
//!   本文件唯一**拒绝未知键**的对象是 [`DescriptionToken::host`] 的 placement——内嵌 schema 里
//!   那两个分支带 `additionalProperties: false`。
//! - **内嵌 schema 是生成数据，不能手改**：`FRAME_SCHEMA_JSON` 逐字节取自 TypeBox 1.3.27 的
//!   `JSON.stringify(FrameSchema)`；要改字段必须回到 TS 源重新生成整串。`frame_schema()` 用
//!   `OnceLock` 只解析一次，schema 文本损坏会在首次调用时 panic（而不是返回 false）。
//! - **错误编解码不对称**：[`to_wire_error`] 把 `ChordError` 的完整 `code[/reason]: message`
//!   写进线上 `message`，[`from_wire_error`] 只剥一层同样的前缀并恢复 `code`/`reason`；
//!   `ChordError.details` 不参与线上传输，过一趟链路后无法恢复。
//! - **`to_json` 走 JS 语义的 `JSON.stringify` 等价物**：键按 JS 对象规则重排（整数索引键升序
//!   在前，其余保持插入序），数字按 JS `Number` 的规则格式化（超过 2^53 的整数会按 f64 舍入）。
//!   帧自身的键都不是整数索引，因此顺序等于 serde 的字段顺序；测试锁定了
//!   `{"t":"ping","n":7}` 这类逐字节输出。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::errors::ChordErrorCode;
use crate::schema::check;

/// 链路协议版本，在 `hello` 中宣告。版本不同的对端不说话：发现方以 `protocol-version <n>` 关闭链路。
///
/// 对端未声明 `protocol` 时按 1 处理（见 [`Frame::Hello`]），因此本常量只描述当前版本；
/// 版本比较与断链动作在连接层，本文件不做判断。测试 `protocol_version_is_four` 固定了它的值。
pub const PROTOCOL_VERSION: i64 = 4;

/// `{ rootId, codeVersion }`：调用发起方所在的 root。
///
/// 目前只出现在 [`Frame::Call`] 的 `root` 字段：消费方把 root 身份随 `call` 带过去，接收方
/// 据此把调用关联到发起者的 root 与 code version（`spec/chord-core.md` §4.2 的 `RootRef`）。
/// `rootId` 非空、`codeVersion >= 1` 这两条下限不在 `serde` 层，由 [`Frame::validate`] 在
/// `call` 分支检查。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootRef {
    /// root 标识；出现在 `call` 时必须非空。
    pub root_id: String,
    /// 该 root 所属的 code version；出现在 `call` 时必须 `>= 1`。
    #[serde(deserialize_with = "js_integer::deserialize")]
    pub code_version: i64,
}

/// 线上错误（`WireError`）：`{ code, message, reason? }`。
///
/// `code` 是 12 个错误码之一。`ChordError` 分支的 `message` 是**带前缀**的完整消息
/// （`code[/reason]: 原始消息`，见 [`to_wire_error`]）；其它错误降级为 `unavailable`，
/// `message` 是原始 Display/Error 文本，可能没有前缀。`reason` 是可选细分原因。
/// `ChordError` 的 `details` 与 `retryable` 不在线上出现。
///
/// 反序列化用 `deserialize_present`：显式 `"reason": null` 会被拒绝（TypeBox 的 optional 只
/// 表示可以缺键）；序列化时 `None` 省略该键。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    /// 错误码（12 个 [`ChordErrorCode`] 之一）。
    pub code: ChordErrorCode,
    /// `ChordError` 时为带 `code[/reason]: ` 前缀的完整消息；其它错误降级为 `unavailable`
    /// 且不额外加前缀（原始 Display 文本照写）。
    pub message: String,
    /// 细分原因；缺省表示没有。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub reason: Option<String>,
}

/// 重载选项（`WireReloadOptions`）：`prepare` 帧里下发给子 host 的选项。
///
/// 三个字段都可缺省，缺省即不出现在 JSON 中。两个 `*Deadline` 是 `number`：JSON 整数与小数
/// 都被 `f64` 接受（因此 `1000` 与 `1000.0` 等价），显式 `null` 被拒绝。单位与默认值由
/// reload/cluster 模块决定（`spec/chord-core.md` 记默认 5000，超时消息按 ms 计），
/// 本类型只负责透传。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireReloadOptions {
    /// 本次重载要移除的插件名；缺省表示不删。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub remove: Option<Vec<String>>,
    /// gate 阶段的等待上限（毫秒；默认值不在本文件）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub gate_deadline: Option<f64>,
    /// 等待旧 code version 退役的期限（毫秒；默认值不在本文件）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present"
    )]
    pub retire_deadline: Option<f64>,
}

/// 子节点随 `vote` 上报的已准备重载（不含 children）。
///
/// 字段全部必填（内嵌 schema 的 `required` 列全了这个对象）；`code_version`/`previous` 是
/// `Type.Integer()`，没有下限，`1.0` 形式也接受。四个列表的内容由 reload 模块填充与解释，
/// 本类型不校验元素语义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireReloadReport {
    /// 子节点报告的新 code version。
    #[serde(deserialize_with = "js_integer::deserialize")]
    pub code_version: i64,
    /// 子节点当前（旧）的 code version。
    #[serde(deserialize_with = "js_integer::deserialize")]
    pub previous: i64,
    /// 有变化的条目。
    pub changed: Vec<String>,
    /// 被移除的条目。
    pub removed: Vec<String>,
    /// 已准备完成的对象地址。
    pub prepared: Vec<String>,
    /// 被 gate 的对象地址。
    pub gated: Vec<String>,
}

/// 契约 token（`DescriptionSchema` 的线上形态）。
///
/// 字段与 `contract::Description` 一致；`input`/`variants` 内部是任意 schema，按 `Value` 保留，
/// 只在 `kiana-chord` 内部校验时才解释。这里定义线上最小面，避免 `wire` 依赖 `contract` 的全部类型。
///
/// 与 `contract::Description` 的差异：本类型只保留 schema 明列出的 6 个字段，未知字段在反序列化
/// 时被忽略；`contract::Description` 及其 `MemberDescription` 用 `#[serde(flatten)]` 保留额外
/// 字段（它们参与契约指纹）。因此若要把 token 原样转成描述，额外字段必须由转换代码另行承载。
///
/// `host` 的 placement 形状与 `variants` 的成员描述在 `serde` 反序列化时检查；`name` 非空只由
/// 内嵌 schema 与 [`Frame::validate`] 检查，单独 `serde_json::from_value::<DescriptionToken>()`
/// 会接受空串。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DescriptionToken {
    /// 类型名；出现在 `hold` 时必须非空（`Frame::validate` 检查）。
    pub name: String,
    /// `remote` 或 `local`。
    pub scope: DescriptionScope,
    /// `Type.Integer()`：`1.0` 也是整数（`Number.isInteger`）。schema 没有给下限，
    /// 0 与负数同样合法。
    #[serde(deserialize_with = "js_integer::deserialize")]
    pub version: i64,
    /// `SchemaObject`：必须是对象（数组/null 都不行）。
    #[serde(deserialize_with = "deserialize_schema_object")]
    pub input: serde_json::Value,
    /// `PlacementSchema`（`{name}` / `{prefix,key}`，`additionalProperties: false`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_placement_opt"
    )]
    pub host: Option<serde_json::Value>,
    /// `Record(String, Record(String, MemberDescription))`。
    #[serde(deserialize_with = "deserialize_member_variants")]
    pub variants: serde_json::Map<String, serde_json::Value>,
}

/// `Description.scope`：契约是远端放置的还是本地提供的。
///
/// 序列化为小写字面量 `"remote"`/`"local"`，与 TS 的字符串联合一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DescriptionScope {
    /// `"remote"`：通过 placement 放在某个 host 上（`host` 字段描述地点）。
    Remote,
    /// `"local"`：在本 host 本地提供（没有 placement）。
    Local,
}

/// `Option<Value>` 的"保留显式 null"反序列化。
///
/// TS 的 `Type.Optional(Type.Unknown())` 只要求**键存在**：`value: null` 与缺键不同，
/// `JSON.stringify` 也必须保留 `null`。serde 默认把 `null` 读成 `None`，这里把任意 JSON 值
/// （包括 `null`）读成 `Some`；缺键仍由 `#[serde(default)]` 给出 `None`。
///
/// 本文件用它承载 `Type.Unknown()` 载荷：`hold.principal`、`call.args`、`result.value`、
/// `state.value`。`serde_json::Value` 能表示任意 JSON，因此这个函数本身不会报错。
fn deserialize_some<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// `Type.Optional(Type.X())`：键存在时必须是 `X`，**显式 `null` 不算**（serde 默认把
/// `Option` 的 `null` 读成 `None`，而 TypeBox 的 optional 只是"可以缺键"）。
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// `state` 是 `Type.Boolean({ const: false })`：键存在时必须是 `false`。
fn deserialize_state<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        Err(serde::de::Error::custom("state must be false"))
    } else {
        Ok(Some(value))
    }
}

/// `again` 是 `Type.Boolean({ const: true })`：键存在时必须是 `true`。
fn deserialize_again<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        Ok(Some(value))
    } else {
        Err(serde::de::Error::custom("again must be true"))
    }
}

/// JSON Schema 的 `integer`（`Number.isInteger`）：`1.0` 也是整数。
///
/// 为什么不能直接用 `i64::deserialize`：TypeBox 的 `Type.Integer()` 判的是数值，
/// `JSON.parse("1.0")` 得到的 `1` 在 TS 侧合法；而 serde 见到 `1.0` 会把它当 f64 并拒绝给
/// `i64`。这里先按 `Value` 读入再自行判定，`1.0` 与 `1` 等价。用于所有 schema 标为
/// `Type.Integer()` 的字段。
mod js_integer {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    fn to_integer<E: serde::de::Error>(value: &Value) -> Result<i64, E> {
        // 先走整数分支：serde_json 用 i64/u64 精确保存整数字面量，避免先转 f64 丢精度。
        if let Some(integer) = value.as_i64() {
            return Ok(integer);
        }
        // 否则必须是"数值上为整数"的 f64（`1.0`、`1e3`），并且落在 i64 范围内。
        // 注意 `i64::MAX as f64` 恰好等于 2^63（f64 无法表示 2^63-1），所以 2^63 能通过上界
        // 检查，再由 Rust `as` 的饱和转换得到 i64::MAX；TS 的 Number 没有这个上限。
        if let Some(number) = value.as_f64()
            && kiana_json::is_js_integer(number)
            && number >= i64::MIN as f64
            && number <= i64::MAX as f64
        {
            return Ok(number as i64);
        }
        Err(E::custom("expected an integer"))
    }

    /// 必需字段的 `Type.Integer()`；缺键由 serde 报 missing field，显式 `null` 在此报错。
    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        to_integer(&value)
    }

    /// 可选字段的 `Type.Integer()`。只在键**存在**时被 serde 调用（缺键由 `#[serde(default)]`
    /// 给 `None`），因此这里的 `Some` 表示"键在且是整数"；显式 `null` 会报错。
    pub fn deserialize_opt<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        to_integer(&value).map(Some)
    }
}

fn is_non_empty_string(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

/// `SchemaObject`（`Type.Unsafe<TSchema>(Type.Object({}))`）：任意**非数组**对象。
fn is_schema_object(value: &serde_json::Value) -> bool {
    value.is_object()
}

/// `PlacementSchema`：`{name}` 或 `{prefix, key}`，两者都是 `additionalProperties: false`。
///
/// 通过键数区分两个分支：恰好 1 个键只能是 `name`，恰好 2 个键必须同时有 `prefix` 与 `key`。
/// 因此 `{name, extra}`、`{name, prefix}` 之类都被拒绝（与 schema 的
/// `additionalProperties: false` 一致），`{}` 与 3 个键的对象也返回 false。
fn is_placement(value: &serde_json::Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    match map.len() {
        1 => map.get("name").is_some_and(is_non_empty_string),
        2 => {
            map.get("prefix").is_some_and(is_non_empty_string)
                && map.get("key").is_some_and(is_non_empty_string)
        }
        _ => false,
    }
}

/// `MemberDescriptionSchema` 的一个分支（三个 `anyOf` 分支之一）。
///
/// 三个分支都没有 `additionalProperties: false`，所以这里只检查必需键与已知键的类型，
/// **未知键一律忽略**（与 schema 一致）。`method` 的 `args`/`result` 可以缺省，但存在时必须是
/// 对象；`state` 必须有对象型的 `schema`；`opaque` 必须有字符串 `id`。
fn is_member_description(value: &serde_json::Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    let Some(kind) = map.get("kind").and_then(serde_json::Value::as_str) else {
        return false;
    };
    match kind {
        "method" => {
            map.get("args").is_none_or(is_schema_object)
                && map.get("result").is_none_or(is_schema_object)
        }
        "state" => map.get("schema").is_some_and(is_schema_object),
        "opaque" => map.get("id").is_some_and(serde_json::Value::is_string),
        _ => false,
    }
}

/// `variants: Record(String, Record(String, MemberDescription))`。
///
/// schema 用 `patternProperties: {"^.*$": …}` 表达"任意键名"，因此变体名/成员名都不限字符、
/// 也不要求非空；外层 `{}`（没有任何变体）合法。
fn is_member_variants(value: &serde_json::Value) -> bool {
    let Some(variants) = value.as_object() else {
        return false;
    };
    variants.values().all(|members| {
        members
            .as_object()
            .is_some_and(|members| members.values().all(is_member_description))
    })
}

fn deserialize_schema_object<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if is_schema_object(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("expected an object"))
    }
}

fn deserialize_placement_opt<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if is_placement(&value) {
        Ok(Some(value))
    } else {
        Err(serde::de::Error::custom("expected a placement"))
    }
}

fn deserialize_member_variants<'de, D>(
    deserializer: D,
) -> Result<serde_json::Map<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if !is_member_variants(&value) {
        return Err(serde::de::Error::custom("expected member variants"));
    }
    // 上面已保证是对象；`unwrap_or_default` 只是为了不写 `expect`，不会真的落到默认值。
    Ok(value.as_object().cloned().unwrap_or_default())
}

/// 链路帧。`t` 为判别标签。
///
/// 23 个变体与 TS 的 `Frame` 联合类型一一对应；`#[serde(tag = "t")]` 让 `t` 与其余字段同级
/// （不是嵌套对象），`rename_all = "snake_case"` 产生 `root_end` 这类标签。
///
/// - 未知的 `t`、缺少必填字段或字段类型不符都会让反序列化失败——对应 TS 侧所有 `anyOf`
///   分支都不匹配（`isFrame` 返回 false）。
/// - 帧对象自身的未知**额外字段**被忽略（见模块文档“设计原因”第一条）；唯一例外是
///   `token.host` 的 placement（带 `additionalProperties: false`，未知键会被拒绝）。
/// - 方向不由类型系统强制：同一个 [`Frame`] 既能被消费方构造也能被提供方构造，方向由连接层
///   按变体检查（`spec/chord-core.md` §4.2 的方向表；控制帧另有 role 限制，见同节）。
/// - 线上发送用 [`Frame::to_json`]；直接 `serde_json::to_string` 在键序与数字格式上可能与 TS
///   的 `JSON.stringify` 不同。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// `hello`：链路建立后双方互发的第一帧，声明对端身份。
    ///
    /// 协议语义上 `protocol` 缺省视为 1，但本类型保留 `None`、不做默认填充——比较版本时必须
    /// 用 `unwrap_or(1)` 之类的等价逻辑（由连接层负责）。与本端 [`PROTOCOL_VERSION`] 不等的
    /// 处置（上报并断链）也在连接层，本文件不比较版本。`protocol` 的下限是 1，而
    /// `codeVersion` 的下限是 0（与 `prepare`/`retire`/`call.root` 的 1 不同）。
    Hello {
        /// 缺省视为 1。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "js_integer::deserialize_opt"
        )]
        protocol: Option<i64>,
        /// 对端名字（`minLength: 1`）。
        name: String,
        /// 对端自称的类型。
        kind: HelloKind,
        /// 发送方当前的 code version；`hello` 的 schema 下限是 0。
        #[serde(rename = "codeVersion", deserialize_with = "js_integer::deserialize")]
        code_version: i64,
        /// 是否启用 ack 流控；缺省视为未声明（TS 侧两端都 announce 才启用，见 spec §4.2）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        acks: Option<bool>,
    },
    /// 到目前为止（自链路开始）已处理的、除 `ack` 外的帧数。
    Ack {
        /// 计数值（`minimum: 1`）。
        #[serde(deserialize_with = "js_integer::deserialize")]
        n: i64,
    },
    /// `hold`：消费方请求提供方为一个对象建立引用（消费方 → 提供方）。
    Hold {
        /// 消费方选定的 hold id（`>= 1`）；同一链路上重复由连接层判违规。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
        /// 目标 host 名；中继在核对消费者与提供者之后据此路由。
        at: String,
        /// 契约 token；见 [`DescriptionToken`]。
        token: Box<DescriptionToken>,
        /// 建引用时的输入（`Type.Unknown()`：显式 `null` 保留）。
        input: serde_json::Value,
        /// 只在来自父链路的帧上被采纳；下游链路使用本条链路自己的 principal（spec §4.2）。
        /// 显式 `null` 会被保留。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_some"
        )]
        principal: Option<serde_json::Value>,
        /// 源标识；下游链路用本连接自己的 id，父链路用帧中转发值（spec §4.2）。空串被
        /// [`Frame::validate`] 拒绝。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        origin: Option<String>,
        /// `false`：在 `sub` 之前没有状态（仅方法绑定）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_state"
        )]
        state: Option<bool>,
        /// 重新 hold（链路丢失后）：消费者已有值，单元不紧急。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_again"
        )]
        again: Option<bool>,
    },
    /// `release`：释放一个 hold（消费方 → 提供方）；对未知 id 是 no-op（spec §4.2）。
    Release {
        /// 要释放的 hold id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
    },
    /// `shape`：提供方返回 hold 的成员面（成员名、契约指纹、随后要发送的状态成员）。
    Shape {
        /// 对应 hold 的 id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
        /// 可选变体名。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        variant: Option<String>,
        /// 成员名列表；schema 只要求是字符串数组，没有逐项非空约束。
        members: Vec<String>,
        /// 契约指纹。
        contract: String,
        /// 紧随本帧之后发送快照的状态成员；到齐后消费者才应用 shape。
        state: Vec<String>,
    },
    /// `gone`：hold 失败/终止（提供方 → 消费方）。既可能是建立 hold 失败，也可能是已建立的
    /// 引用或 relay 进入 gone 状态：TS 的 `ref.goneListeners` 回调（`connection.ts`）与
    /// `RelayRef.close` 在 `notify: true` 时都会发送本帧（后者见 spec §2.4.3）。
    Gone {
        /// 对应的 hold id（建立失败或终止的那个 hold）。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
        /// 失败原因，按 [`WireError`] 编码。
        error: WireError,
    },
    /// `call`：在已有 hold 上调用成员（消费方 → 提供方）。
    Call {
        /// 调用 id：由消费方分配（`>= 1`）。spec §4.2 的方向约定：每种帧只有一个方向，
        /// 因此 call id 不必与其它方向的 id 全局唯一。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
        /// 目标 hold 的 id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        hold: i64,
        /// 成员名（`minLength: 1`）。
        member: String,
        /// 参数（`Type.Unknown()`：显式 `null` 保留；TS 侧这是可被压缩打包的两个载荷之一）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_some"
        )]
        args: Option<serde_json::Value>,
        /// 缺省：接收方 host 启动一个 root（浏览器不携带 root）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        root: Option<RootRef>,
    },
    /// `result`：`call` 的应答（提供方 → 消费方）；成功时可带 `value`（无返回值/`undefined`
    /// 时省略该键），失败带 `error`。
    Result {
        /// 对应 call 的 id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
        /// 返回值（`Type.Unknown()`：显式 `null` 保留；可被压缩打包的载荷之一）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_some"
        )]
        value: Option<serde_json::Value>,
        /// 失败原因；schema 不强制它与 `value` 互斥。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        error: Option<WireError>,
    },
    /// `cancel`：取消一个尚未应答的 `call`（消费方 → 提供方）；对未知 id 是 no-op。
    Cancel {
        /// 要取消的 call id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        id: i64,
    },
    /// 为以 `state: false` 打开的 hold 启动状态（以 unit 应答）。
    Sub {
        /// 要启动状态的 hold id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        hold: i64,
    },
    /// 停止 hold 的状态。
    Unsub {
        /// 要停止状态的 hold id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        hold: i64,
    },
    /// 重新发送成员 `m` 作为 reset（中继不得不丢弃等待它的内容时）。
    Resync {
        /// 目标 hold 的 id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        hold: i64,
        /// 要重发的状态成员名（非空）。非空约束只由内嵌 schema（[`is_frame`]）检查，
        /// [`Frame::validate`] 未覆盖。
        m: String,
    },
    /// `state`：状态成员的一次推送（提供方 → 消费方）；`as` 决定载荷如何解释。
    State {
        /// 对应 hold 的 id。
        #[serde(deserialize_with = "js_integer::deserialize")]
        hold: i64,
        /// 状态成员名（`minLength: 1`）。
        m: String,
        #[serde(rename = "as")]
        state_as: StateKind,
        /// 整值载荷（用于 `snap`/`reset`）；缺省表示未提供，显式 `null` 保留为 JSON null。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_some"
        )]
        value: Option<serde_json::Value>,
        /// 用流的 delta 编码器编码；解码器逐 op 校验。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        ops: Option<Vec<Vec<serde_json::Value>>>,
        /// `as: "error"` 时的失败原因。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        error: Option<WireError>,
    },
    /// `ping`：心跳请求；`n` 是发送方自增的序号。
    Ping {
        #[serde(deserialize_with = "js_integer::deserialize")]
        n: i64,
    },
    /// `pong`：心跳应答；`n` 原样回显收到的 ping 序号（spec 的帧分发表）。
    Pong {
        #[serde(deserialize_with = "js_integer::deserialize")]
        n: i64,
    },
    /// 结束链路（由链路本身消费）。`reason: "protocol-version <n>"`：发送方说的是版本 n。
    Bye {
        /// 关闭原因；缺省表示没有说明。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        reason: Option<String>,
    },
    /// root 在源头结束：其对象租约终止（在该 root 调用用过的每条链路上发送）。
    RootEnd {
        /// 结束的 root 标识（`minLength: 1`）。
        #[serde(rename = "rootId")]
        root_id: String,
    },
    /// `invalidate`：父节点要求子节点作废 principal 匹配该模式的对象（父 → 子）。
    Invalidate {
        /// principal 匹配模板的线上形态；函数形式的 match 不会上线（spec 的
        /// `invalidatePrincipal`）。
        pattern: serde_json::Map<String, serde_json::Value>,
    },
    /// `prepare`：reload 第一阶段，父节点要求子 host 预加载新 code version（父 → 子）。
    Prepare {
        /// reload 序号（`>= 1`），`vote`/`commit`/`abort` 用它对应同一轮。
        #[serde(deserialize_with = "js_integer::deserialize")]
        reload: i64,
        /// 新 code version（`>= 1`）。
        #[serde(rename = "codeVersion", deserialize_with = "js_integer::deserialize")]
        code_version: i64,
        /// 要加载的插件列表。
        plugins: Vec<String>,
        /// 重载选项，见 [`WireReloadOptions`]。
        options: WireReloadOptions,
    },
    /// `vote`：子 host 对 `prepare` 的应答（子 → 父）；成功带 `report`，失败带 `error`。
    Vote {
        /// 对应的 reload 序号。
        #[serde(deserialize_with = "js_integer::deserialize")]
        reload: i64,
        /// 失败原因。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        error: Option<WireError>,
        /// 成功时上报的已准备重载，见 [`WireReloadReport`]。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present"
        )]
        report: Option<WireReloadReport>,
    },
    /// `commit`：提交本轮 reload（父 → 子）。
    Commit {
        /// 对应的 reload 序号。
        #[serde(deserialize_with = "js_integer::deserialize")]
        reload: i64,
    },
    /// 父节点丢弃了 `codeVersion`：来自那里的 root 不再可能。
    Retire {
        /// 被丢弃的 code version（`>= 1`）。
        #[serde(rename = "codeVersion", deserialize_with = "js_integer::deserialize")]
        code_version: i64,
    },
    /// `abort`：中止本轮 reload（父 → 子）。
    Abort {
        /// 对应的 reload 序号。
        #[serde(deserialize_with = "js_integer::deserialize")]
        reload: i64,
    },
}

/// `Hello.kind`：对端在 `hello` 里自称的类型。
///
/// 连接层对 upstream（父）链路要求 `host`，否则判 `parent is not a host`（spec 的 `#onHello`
/// 规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HelloKind {
    /// `"host"`：对端是一个 chord host。
    Host,
    /// `"client"`：对端是客户端（不作为父节点）。
    Client,
}

/// `State.as`：`state` 帧载荷的解释方式。
///
/// 序列化为小写字面量。`snap`/`reset` 的载荷是整值 `value`（可以缺省，缺省等价于
/// `undefined`）；`ops` 的载荷在 `ops` 字段（delta 编码）；`error` 的载荷在 `error` 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    /// `"snap"`：完整快照。
    Snap,
    /// `"reset"`：整值重置（spec 里发送侧 `#feedEvent` 与接收侧 state 处理都会重置 delta
    /// 编解码器，后续 ops 相对这个 `value` 开始）。
    Reset,
    /// `"ops"`：相对上一状态的 delta 编码增量。
    Ops,
    /// `"error"`：该状态成员失败。
    Error,
}

/// 帧校验失败（`isFrame` 返回 false 的原因）。
///
/// 这些 `Display` 文本只用于 Rust 侧诊断：TS 的 `isFrame` 只返回 boolean，没有对应文案，
/// 因此它们不参与线上协议。`Frame::validate` 只覆盖 `serde` 类型表达不了的那部分限制；
/// 字段类型/`null`/`token` 形状的错误来自 `serde_json::from_value`，不在这里。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// 某个 `minimum` 约束不满足。
    #[error("{field} must be at least {min}")]
    Minimum { field: &'static str, min: i64 },
    /// `hello.protocol` 存在且小于 1。
    #[error("hello.protocol must be at least 1")]
    ProtocolVersion,
    /// 某个要求非空的字符串为空。字段名是调用点硬编码的字符串；注意 `hold.at`、`call.member`
    /// 与 `state.m` 都走同一个 `name` 辅助闭包，因此这几种情况报出的字段名是 `name` 而不是
    /// 各自的名字（`origin`/`rootId` 则各自硬编码，能区分）。
    #[error("{0} must not be empty")]
    Empty(&'static str),
    /// `shape.state` 中出现空成员名。
    #[error("shape.state entries must be non-empty names")]
    EmptyName,
    /// `hold.token.name` 为空。
    #[error("description name must not be empty")]
    EmptyDescriptionName,
}

impl Frame {
    /// 数值下限与空串校验。本函数只补 `serde` 类型表达不了的 `minimum`/`minLength`；
    /// 与 TS `Value.Check` 等价的是 [`is_frame`]。`serde_json::from_value::<Frame>()` + 本函数
    /// 仅在 `resync.m` 一处不等价（见下）。
    ///
    /// 注意本函数**没有**覆盖 `FrameSchema` 的全部 `minLength`：`resync.m` 的非空约束只在
    /// [`is_frame`] 一侧检查（见模块文档的“已知不等价处”）。
    pub fn validate(&self) -> Result<(), FrameError> {
        let min = |field: &'static str, value: i64, m: i64| -> Result<(), FrameError> {
            if value < m {
                Err(FrameError::Minimum { field, min: m })
            } else {
                Ok(())
            }
        };
        let name = |value: &str| -> Result<(), FrameError> {
            if value.is_empty() {
                Err(FrameError::Empty("name"))
            } else {
                Ok(())
            }
        };
        // 逐变体补 schema 里 serde 类型表达不了的限制（minimum / minLength）。
        // 每个分支遇到第一个不满足的限制就返回，因此一帧有多个问题时只报告第一个。
        match self {
            Frame::Hello {
                protocol,
                name: n,
                code_version,
                ..
            } => {
                // protocol 用专门的错误变体，便于诊断时与普通 minimum 失败区分。
                if protocol.is_some_and(|p| p < 1) {
                    return Err(FrameError::ProtocolVersion);
                }
                name(n)?;
                min("codeVersion", *code_version, 0)
            }
            Frame::Ack { n } => min("n", *n, 1),
            Frame::Hold {
                id,
                at,
                token,
                origin,
                ..
            } => {
                min("id", *id, 1)?;
                // at 的 minLength: 1；token.name 有专门的错误变体（诊断时能区分是哪个名字）。
                name(at)?;
                if token.name.is_empty() {
                    return Err(FrameError::EmptyDescriptionName);
                }
                if origin.as_deref() == Some("") {
                    return Err(FrameError::Empty("origin"));
                }
                Ok(())
            }
            Frame::Release { id } | Frame::Cancel { id } | Frame::Gone { id, .. } => {
                min("id", *id, 1)
            }
            Frame::Shape {
                id, members, state, ..
            } => {
                min("id", *id, 1)?;
                if state.iter().any(|m| m.is_empty()) {
                    return Err(FrameError::EmptyName);
                }
                // `members` 的元素在 schema 里只要求是字符串（items 没有 minLength），
                // 没有可补的约束；这里绑定它只是为说明该字段确实不参与校验。
                let _ = members;
                Ok(())
            }
            Frame::Call {
                id,
                hold,
                member,
                root,
                ..
            } => {
                min("id", *id, 1)?;
                min("hold", *hold, 1)?;
                name(member)?;
                // root 可缺省；出现时它带着自己的两条下限（`RootRef` 的定义见 schema）。
                if let Some(root) = root {
                    if root.root_id.is_empty() {
                        return Err(FrameError::Empty("rootId"));
                    }
                    min("codeVersion", root.code_version, 1)?;
                }
                Ok(())
            }
            Frame::Result { id, .. } => min("id", *id, 1),
            Frame::Sub { hold } | Frame::Unsub { hold } | Frame::Resync { hold, .. } => {
                min("hold", *hold, 1)
            }
            Frame::State { hold, m, .. } => {
                min("hold", *hold, 1)?;
                name(m)
            }
            Frame::Prepare {
                reload,
                code_version,
                ..
            } => {
                min("reload", *reload, 1)?;
                min("codeVersion", *code_version, 1)
            }
            Frame::Vote { reload, .. } | Frame::Commit { reload } | Frame::Abort { reload } => {
                min("reload", *reload, 1)
            }
            Frame::Retire { code_version } => min("codeVersion", *code_version, 1),
            Frame::RootEnd { root_id } => {
                if root_id.is_empty() {
                    Err(FrameError::Empty("rootId"))
                } else {
                    Ok(())
                }
            }
            // 这些帧的 schema 没有 minimum/minLength，serde 层已经是全部约束。
            Frame::Ping { .. }
            | Frame::Pong { .. }
            | Frame::Bye { .. }
            | Frame::Invalidate { .. } => Ok(()),
        }
    }

    /// 线路上的字节（紧凑 JSON，键序保持）。
    ///
    /// 先用 `serde_json::to_value` 得到 [`serde_json::Value`]，再交给
    /// `kiana_json::js_object::stringify`（JS `JSON.stringify` 的等价物）：键按 JS 对象规则
    /// 排序，数字按 JS `Number` 的格式写出（超过 2^53 的整数会被 f64 舍入）。帧自身的键都是
    /// 非整数索引，因此顶层顺序等于 serde 的字段顺序；`args`/`value` 等载荷内部的整数索引键
    /// 仍会按 JS 规则被重排。`Frame` 的字段都可序列化，`expect` 不应触发；唯一可能非有限浮点的
    /// 位置是 `WireReloadOptions` 的两个 deadline，`serde_json` 会把 `NaN`/`±Infinity` 写成
    /// `null`（与 JS 的 `JSON.stringify` 一致，不报错）。
    pub fn to_json(&self) -> String {
        kiana_json::js_object::stringify(
            &serde_json::to_value(self).expect("Frame is serializable"),
        )
    }
}

/// TS `FrameSchema`（`wire.ts:60-151`）的 JSON 形态，逐字节取自 TypeBox 1.3.27 的
/// `JSON.stringify(FrameSchema)`。`isFrame` 就是 `Value.Check(FrameSchema, value)`：
/// 标量取值（`state: false`、`again: true`、`minLength 1`、`additionalProperties: false`
/// 的 placement、`Type.Unknown()` 的 `{}`……）都由它表达，不能靠 `serde` 的宽松形态。
///
/// **这是生成数据，不要手改任何片段**：TS 侧改了 `FrameSchema` 就整串重新生成。可读性差是有意
/// 的——它唯一的用途是喂给 `schema::check`，逐字节保真比可读性重要。结构上，顶层 `anyOf` 的
/// 每个分支是 `{"type":"object","required":[…],"properties":{…}}`，靠 `t` 的 `const` 区分
/// 23 种帧；`WireError` 的子 schema 在用到它的分支里各自重复展开，文本中没有 `$ref`。
const FRAME_SCHEMA_JSON: &str = r#"{"anyOf":[{"type":"object","required":["t","name","kind","codeVersion"],"properties":{"t":{"type":"string","const":"hello"},"protocol":{"type":"integer","minimum":1},"name":{"type":"string","minLength":1},"kind":{"anyOf":[{"type":"string","const":"host"},{"type":"string","const":"client"}]},"codeVersion":{"type":"integer","minimum":0},"acks":{"type":"boolean"}}},{"type":"object","required":["t","n"],"properties":{"t":{"type":"string","const":"ack"},"n":{"type":"integer","minimum":1}}},{"type":"object","required":["t","id","at","token","input"],"properties":{"t":{"type":"string","const":"hold"},"id":{"type":"integer","minimum":1},"at":{"type":"string","minLength":1},"token":{"type":"object","required":["name","scope","version","input","variants"],"properties":{"name":{"type":"string","minLength":1},"scope":{"anyOf":[{"type":"string","const":"remote"},{"type":"string","const":"local"}]},"version":{"type":"integer"},"input":{"type":"object","properties":{}},"host":{"anyOf":[{"type":"object","required":["name"],"properties":{"name":{"type":"string","minLength":1}},"additionalProperties":false},{"type":"object","required":["prefix","key"],"properties":{"prefix":{"type":"string","minLength":1},"key":{"type":"string","minLength":1}},"additionalProperties":false}]},"variants":{"type":"object","patternProperties":{"^.*$":{"type":"object","patternProperties":{"^.*$":{"anyOf":[{"type":"object","required":["kind"],"properties":{"kind":{"type":"string","const":"method"},"args":{"type":"object","properties":{}},"result":{"type":"object","properties":{}}}},{"type":"object","required":["kind","schema"],"properties":{"kind":{"type":"string","const":"state"},"schema":{"type":"object","properties":{}}}},{"type":"object","required":["kind","id"],"properties":{"kind":{"type":"string","const":"opaque"},"id":{"type":"string"}}}]}}}}}}},"input":{},"principal":{},"origin":{"type":"string","minLength":1},"state":{"type":"boolean","const":false},"again":{"type":"boolean","const":true}}},{"type":"object","required":["t","id"],"properties":{"t":{"type":"string","const":"release"},"id":{"type":"integer","minimum":1}}},{"type":"object","required":["t","id","members","contract","state"],"properties":{"t":{"type":"string","const":"shape"},"id":{"type":"integer","minimum":1},"variant":{"type":"string"},"members":{"type":"array","items":{"type":"string"}},"contract":{"type":"string"},"state":{"type":"array","items":{"type":"string","minLength":1}}}},{"type":"object","required":["t","id","error"],"properties":{"t":{"type":"string","const":"gone"},"id":{"type":"integer","minimum":1},"error":{"type":"object","required":["code","message"],"properties":{"code":{"enum":["aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant","not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required"]},"message":{"type":"string"},"reason":{"type":"string"}}}}},{"type":"object","required":["t","id","hold","member"],"properties":{"t":{"type":"string","const":"call"},"id":{"type":"integer","minimum":1},"hold":{"type":"integer","minimum":1},"member":{"type":"string","minLength":1},"args":{},"root":{"type":"object","required":["rootId","codeVersion"],"properties":{"rootId":{"type":"string","minLength":1},"codeVersion":{"type":"integer","minimum":1}}}}},{"type":"object","required":["t","id"],"properties":{"t":{"type":"string","const":"result"},"id":{"type":"integer","minimum":1},"value":{},"error":{"type":"object","required":["code","message"],"properties":{"code":{"enum":["aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant","not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required"]},"message":{"type":"string"},"reason":{"type":"string"}}}}},{"type":"object","required":["t","id"],"properties":{"t":{"type":"string","const":"cancel"},"id":{"type":"integer","minimum":1}}},{"type":"object","required":["t","hold"],"properties":{"t":{"type":"string","const":"sub"},"hold":{"type":"integer","minimum":1}}},{"type":"object","required":["t","hold"],"properties":{"t":{"type":"string","const":"unsub"},"hold":{"type":"integer","minimum":1}}},{"type":"object","required":["t","hold","m"],"properties":{"t":{"type":"string","const":"resync"},"hold":{"type":"integer","minimum":1},"m":{"type":"string","minLength":1}}},{"type":"object","required":["t","hold","m","as"],"properties":{"t":{"type":"string","const":"state"},"hold":{"type":"integer","minimum":1},"m":{"type":"string","minLength":1},"as":{"anyOf":[{"type":"string","const":"snap"},{"type":"string","const":"reset"},{"type":"string","const":"ops"},{"type":"string","const":"error"}]},"value":{},"ops":{"type":"array","items":{"type":"array","items":{}}},"error":{"type":"object","required":["code","message"],"properties":{"code":{"enum":["aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant","not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required"]},"message":{"type":"string"},"reason":{"type":"string"}}}}},{"type":"object","required":["t","n"],"properties":{"t":{"type":"string","const":"ping"},"n":{"type":"integer"}}},{"type":"object","required":["t","n"],"properties":{"t":{"type":"string","const":"pong"},"n":{"type":"integer"}}},{"type":"object","required":["t"],"properties":{"t":{"type":"string","const":"bye"},"reason":{"type":"string"}}},{"type":"object","required":["t","rootId"],"properties":{"t":{"type":"string","const":"root_end"},"rootId":{"type":"string","minLength":1}}},{"type":"object","required":["t","pattern"],"properties":{"t":{"type":"string","const":"invalidate"},"pattern":{"type":"object","patternProperties":{"^.*$":{}}}}},{"type":"object","required":["t","reload","codeVersion","plugins","options"],"properties":{"t":{"type":"string","const":"prepare"},"reload":{"type":"integer","minimum":1},"codeVersion":{"type":"integer","minimum":1},"plugins":{"type":"array","items":{"type":"string"}},"options":{"type":"object","properties":{"remove":{"type":"array","items":{"type":"string"}},"gateDeadline":{"type":"number"},"retireDeadline":{"type":"number"}}}}},{"type":"object","required":["t","reload"],"properties":{"t":{"type":"string","const":"vote"},"reload":{"type":"integer","minimum":1},"error":{"type":"object","required":["code","message"],"properties":{"code":{"enum":["aborted","contract_mismatch","forbidden","gone","invalid","not_in_variant","not_ready","released","superseded","unavailable","upgrade_conflict","upgrade_required"]},"message":{"type":"string"},"reason":{"type":"string"}}},"report":{"type":"object","required":["codeVersion","previous","changed","removed","prepared","gated"],"properties":{"codeVersion":{"type":"integer"},"previous":{"type":"integer"},"changed":{"type":"array","items":{"type":"string"}},"removed":{"type":"array","items":{"type":"string"}},"prepared":{"type":"array","items":{"type":"string"}},"gated":{"type":"array","items":{"type":"string"}}}}}},{"type":"object","required":["t","reload"],"properties":{"t":{"type":"string","const":"commit"},"reload":{"type":"integer","minimum":1}}},{"type":"object","required":["t","codeVersion"],"properties":{"t":{"type":"string","const":"retire"},"codeVersion":{"type":"integer","minimum":1}}},{"type":"object","required":["t","reload"],"properties":{"t":{"type":"string","const":"abort"},"reload":{"type":"integer","minimum":1}}}]}"#;

/// `FrameSchema` 的 [`serde_json::Value`]（解析一次）。
///
/// 用 `OnceLock` 缓存，避免每条帧都把这段长 JSON 重新解析一遍；解析失败会在首次调用时
/// panic——内嵌文本是常量，坏掉属于本文件自身的缺陷，静默返回 false 只会让所有帧都被判
/// malformed。
fn frame_schema() -> &'static serde_json::Value {
    static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(FRAME_SCHEMA_JSON).expect("embedded FrameSchema is valid JSON")
    })
}

/// `isFrame(value)`：`Value.Check(FrameSchema, value)`。
///
/// 对任意 JSON 值都只返回 bool：标量、数组、`null`、未知 `t` 以及缺**必填**字段的对象全部
/// 为 false（没有 `anyOf` 分支能匹配）；帧对象自身的多余字段不影响结果，唯一例外是
/// `token.host` 的 placement（带 `additionalProperties: false`，见模块文档）。正常输入下本函数
/// 不 panic；会 panic 的只有 `frame_schema()` 首次解析内嵌文本失败这一种情况。按 spec §4.2，
/// 连接层在反序列化**之前**用它判定 `malformed frame`。
pub fn is_frame(value: &serde_json::Value) -> bool {
    check(frame_schema(), value)
}

/// `toWireError(error)`：`ChordError` 保留 code/reason，其余变 `unavailable`。
///
/// TS 的 `ChordError` 把前缀写进 `Error.message`（`super(\`${code}${reason ? \`/${reason}\` : ""}: ${message}\`)`），
/// 所以线上 `message` 是**带前缀的完整消息**，`fromWireError` 再把它剥掉。这里因此用
/// [`crate::errors::ChordError::full_message`] 而不是裸 `message`。
///
/// 两个分支的差异：`ChordError` 会带上自己的 `code`/`reason`/完整消息；其它
/// [`std::error::Error`] 一律降级为 [`ChordErrorCode::Unavailable`]，`reason` 为 `None`，
/// 消息取 `Display` 文本（可能已经带某种前缀，`from_wire_error` 会按同一条规则尝试剥一层）。
/// `details` 不会出现在 [`WireError`] 里，因此过链路后丢失。
pub fn to_wire_error(error: &(dyn std::error::Error + 'static)) -> WireError {
    match error.downcast_ref::<crate::errors::ChordError>() {
        Some(e) => WireError {
            code: e.code,
            message: e.full_message(),
            reason: e.reason.clone(),
        },
        None => WireError {
            code: ChordErrorCode::Unavailable,
            message: error.to_string(),
            reason: None,
        },
    }
}

/// `fromWireError(error)`：剥掉 `code/reason: ` 前缀后作为原始消息。
///
/// `code` 与 `reason` 直接取自线上字段，`message` 则把前缀剥掉（只剥一层，见
/// `strip_code_prefix`），得到的错误重新 `full_message()` 时会拼回同样的文本（测试
/// `wire_errors_carry_full_messages` 锁定往返）。`details` 线上没有，无法恢复。
///
/// 注意这条规则与 TS 相同：如果对端发来的 `message` **本身**以形如 `abc: ` 的小写前缀开头
/// （不是 ChordError 生成的前缀），它也会被当成前缀剥掉。这是协议既有行为，测试
/// `error_prefix_is_stripped_like_typescript` 固定了这些边界。
pub fn from_wire_error(error: &WireError) -> crate::errors::ChordError {
    let stripped = strip_code_prefix(&error.message);
    let mut out = crate::errors::ChordError::new(error.code, stripped);
    out.reason = error.reason.clone();
    out
}

/// 剥除形如 `/^[a-z_]+(\/[a-z_]+)?: /` 的码前缀（当前稳定契约，改动需同步测试）。
///
/// 手写扫描而非正则：只接受 ASCII 小写字母与 `_` 组成的码段，可选的 `/码段` 必须非空，
/// 之后必须紧跟 `": "`（冒号 + 空格）。任一段不满足就原样返回——注意这意味着
/// `"Invalid: x"`（大写）、`"invalid/: x"`（斜杠后为空）、`"invalid:x"`（缺空格）都不会被剥。
/// 只剥一次，剥完的结果即使再次形如 `code: ` 也不再处理（与 TS 的 `replace` 只匹配行首一致）。
fn strip_code_prefix(message: &str) -> String {
    // 第一段：`[a-z_]+`。空匹配（i == 0）直接放弃。
    let bytes = message.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_lowercase() || bytes[i] == b'_') {
        i += 1;
    }
    if i == 0 {
        return message.to_owned();
    }
    // 可选段：`/` 后必须还有至少一个 `[a-z_]`，否则整个前缀不算数（`invalid/: x` 保持原样）。
    if i < bytes.len() && bytes[i] == b'/' {
        let mut j = i + 1;
        while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j] == b'_') {
            j += 1;
        }
        if j == i + 1 {
            return message.to_owned();
        }
        i = j;
    }
    // 必须以 `": "` 收尾；`i` 一定落在字符边界上（前面只扫过 ASCII）。
    if message[i..].starts_with(": ") {
        return message[i + 2..].to_owned();
    }
    message.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_version_is_four() {
        assert_eq!(PROTOCOL_VERSION, 4);
    }

    #[test]
    fn frames_use_the_t_tag() {
        let frame = Frame::Ping { n: 7 };
        assert_eq!(frame.to_json(), r#"{"t":"ping","n":7}"#);
        let frame = Frame::RootEnd {
            root_id: "r1".into(),
        };
        assert_eq!(frame.to_json(), r#"{"t":"root_end","rootId":"r1"}"#);
        let frame = Frame::Retire { code_version: 3 };
        assert_eq!(frame.to_json(), r#"{"t":"retire","codeVersion":3}"#);
    }

    #[test]
    fn validation_rejects_out_of_range_ids() {
        assert!(!is_frame(&json!({"t": "release", "id": 0})));
        assert!(is_frame(&json!({"t": "release", "id": 1})));
        assert!(!is_frame(
            &json!({"t": "prepare", "reload": 1, "codeVersion": 0, "plugins": [], "options": {}})
        ));
        assert!(is_frame(
            &json!({"t": "prepare", "reload": 1, "codeVersion": 1, "plugins": [], "options": {}})
        ));
    }

    fn local_token() -> serde_json::Value {
        json!({"name": "t", "scope": "local", "version": 1, "input": {}, "variants": {}})
    }

    /// 关键回归：`isFrame` 是 TypeBox `Value.Check(FrameSchema, value)`，不是 serde 的宽松形态。
    /// 每个用例都是审查指出的假阳性（`serde` 会接受，TypeBox 拒绝）。
    #[test]
    fn frame_schema_rejects_loose_shapes() {
        let hold = |extra: serde_json::Value| {
            let mut frame =
                json!({"t": "hold", "id": 1, "at": "h", "token": local_token(), "input": {}});
            for (key, value) in extra.as_object().unwrap() {
                frame[key] = value.clone();
            }
            frame
        };
        let token = |mutation: &dyn Fn(&mut serde_json::Value)| {
            let mut token = local_token();
            mutation(&mut token);
            token
        };
        let rejects = vec![
            hold(json!({"state": true})),
            hold(json!({"again": false})),
            json!({"t": "resync", "hold": 1, "m": ""}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| t["input"] = json!(5)), "input": {}}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| t["host"] = json!({"name": "h", "x": 1})), "input": {}}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| { t["scope"] = json!("remote"); t["host"] = json!({"name": ""}); }), "input": {}}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| { t["scope"] = json!("remote"); t["host"] = json!("h"); }), "input": {}}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| t["variants"] = json!({"v": {"m": {"kind": "nope"}}})), "input": {}}),
            json!({"t": "hold", "id": 1, "at": "h", "token": token(&|t| t["variants"] = json!({"v": {"m": 5}})), "input": {}}),
            json!({"t": "state", "hold": 1, "m": "x", "as": "snap", "ops": null}),
            json!({"t": "bye", "reason": null}),
            json!({"t": "shape", "id": 1, "members": [], "contract": "c", "state": [], "variant": null}),
            hold(json!({"origin": ""})),
            json!({"t": "root_end", "rootId": ""}),
            json!({"t": "result", "id": 1, "error": null}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "root": null}),
            json!({"t": "hello", "name": "n", "kind": "host", "codeVersion": 0, "acks": null}),
            json!({"t": "hold", "id": 1, "at": "", "token": local_token(), "input": {}}),
            json!({"t": "unknown"}),
            json!({}),
            json!([]),
            json!(null),
            json!(5),
            json!("x"),
        ];
        for frame in rejects {
            assert!(!is_frame(&frame), "must reject {frame}");
        }

        let accepts = vec![
            json!({"t": "hello", "name": "n", "kind": "host", "codeVersion": 0}),
            json!({"t": "hello", "name": "n", "kind": "client", "codeVersion": 1, "protocol": 4, "acks": true}),
            json!({"t": "ack", "n": 1}),
            hold(json!({"state": false, "again": true, "origin": "o", "principal": {"p": 1}})),
            json!({"t": "shape", "id": 2, "variant": "v", "members": ["a"], "contract": "c", "state": ["a"]}),
            json!({"t": "state", "hold": 1, "m": "x", "as": "ops", "ops": [[1, "a"]], "value": null}),
            json!({"t": "result", "id": 1, "value": null, "error": {"code": "invalid", "message": "m"}}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "args": null, "root": {"rootId": "r", "codeVersion": 1}}),
            json!({"t": "invalidate", "pattern": {"a": 1}}),
            json!({"t": "prepare", "reload": 1, "codeVersion": 1, "plugins": ["p"], "options": {"remove": [], "gateDeadline": 1.5}}),
            json!({"t": "vote", "reload": 2, "report": {"codeVersion": 1, "previous": 0, "changed": [], "removed": [], "prepared": [], "gated": []}}),
            json!({"t": "retire", "codeVersion": 1}),
        ];
        for frame in accepts {
            assert!(is_frame(&frame), "must accept {frame}");
        }
    }

    /// 关键回归（reviewer #13）：`serde_json::from_value::<Frame>()` 必须与 `isFrame` 一样严格——
    /// 可选字段的显式 `null` 被拒绝、`token` 的形状被校验、`1.0` 形式的整数被接受。
    #[test]
    fn frame_deserialization_is_as_strict_as_is_frame() {
        let hold = |extra: serde_json::Value| {
            let mut frame =
                json!({"t": "hold", "id": 1, "at": "h", "token": local_token(), "input": {}});
            for (key, value) in extra.as_object().unwrap() {
                frame[key] = value.clone();
            }
            frame
        };
        let rejects = vec![
            // `state`/`again` 是 const 布尔，`null` 与错误取值都非法。
            hold(json!({"state": true})),
            hold(json!({"state": null})),
            hold(json!({"again": false})),
            hold(json!({"again": null})),
            hold(json!({"origin": null})),
            // `token` 的 `input`/`host`/`variants` 只能按 DescriptionSchema 的形状来。
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": 5, "variants": {}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": [], "variants": {}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1.5, "input": {}, "variants": {}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "host": null, "variants": {}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "host": {"name": "h", "x": 1}, "variants": {}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "variants": []}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "variants": {"v": {"m": {"kind": "zzz"}}}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "variants": {"v": {"m": 5}}}}),
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "token": {"name": "t", "scope": "local", "version": 1, "input": {}, "variants": {"v": {"m": {"kind": "state"}}}}}),
            // 其它可选字段的 `null`。
            json!({"t": "shape", "id": 1, "members": [], "contract": "c", "state": [], "variant": null}),
            json!({"t": "bye", "reason": null}),
            json!({"t": "result", "id": 1, "error": null}),
            json!({"t": "result", "id": 1, "error": {"code": "invalid", "message": "m", "reason": null}}),
            json!({"t": "state", "hold": 1, "m": "x", "as": "ops", "ops": null}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "root": null}),
            json!({"t": "hello", "name": "n", "kind": "host", "codeVersion": 0, "acks": null}),
            json!({"t": "hello", "name": "n", "kind": "host", "codeVersion": 0, "protocol": null}),
            json!({"t": "prepare", "reload": 1, "codeVersion": 1, "plugins": [], "options": {"remove": null}}),
            json!({"t": "ack", "n": null}),
        ];
        for frame in rejects {
            assert!(
                serde_json::from_value::<Frame>(frame.clone()).is_err(),
                "must reject {frame}"
            );
        }

        // `1.0` 是 JSON Schema 的整数（`Number.isInteger(1.0)`）。
        let frame: Frame =
            serde_json::from_value(json!({"t": "ack", "n": 1.0})).expect("1.0 is an integer");
        assert_eq!(frame, Frame::Ack { n: 1 });
        let frame: Frame = serde_json::from_value(json!({
            "t": "hold", "id": 1.0, "at": "h", "input": {},
            "token": {"name": "t", "scope": "local", "version": 1.0, "input": {}, "variants": {}}
        }))
        .expect("1.0 is an integer");
        frame.validate().expect("valid");
        // `Type.Unknown()` 载荷（`{}`）接受 `null` 并保留它。
        let frame: Frame = serde_json::from_value(json!({"t": "result", "id": 1, "value": null}))
            .expect("value: null");
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({"t": "result", "id": 1, "value": null})
        );

        // `validate()` 补做 serde 类型表达不了的限制（reviewer #13(c)）。
        for frame in [
            json!({"t": "hold", "id": 1, "at": "h", "input": {}, "origin": "", "token": local_token()}),
            json!({"t": "root_end", "rootId": ""}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "root": {"rootId": "", "codeVersion": 1}}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "root": {"rootId": "r", "codeVersion": 0}}),
        ] {
            let parsed: Frame =
                serde_json::from_value(frame.clone()).expect("typed shape is valid");
            assert!(parsed.validate().is_err(), "validate must reject {frame}");
        }
    }

    /// 关键回归：`Option<Value>` 的可选载荷保留**显式** `null`（与缺键不同）。
    #[test]
    fn explicit_null_payloads_survive_a_round_trip() {
        let frames = [
            json!({"t": "result", "id": 1, "value": null}),
            json!({"t": "state", "hold": 1, "m": "x", "as": "snap", "value": null}),
            json!({"t": "call", "id": 1, "hold": 1, "member": "m", "args": null}),
            json!({"t": "hold", "id": 1, "at": "h", "token": local_token(), "input": {}, "principal": null}),
        ];
        for expected in frames {
            let frame: Frame = serde_json::from_value(expected.clone()).expect("parse");
            assert_eq!(serde_json::to_value(&frame).unwrap(), expected);
        }
        // 缺键仍是缺键（序列化时省略）。
        let frame: Frame = serde_json::from_value(json!({"t": "result", "id": 1})).unwrap();
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({"t": "result", "id": 1})
        );
    }

    /// 关键回归：`toWireError` 的 `message` 是 ChordError 的**完整**消息（含 `code/reason: ` 前缀）。
    #[test]
    fn wire_errors_carry_full_messages() {
        let error = crate::errors::ChordError::new(crate::errors::ChordErrorCode::Invalid, "bad")
            .with_reason("args");
        assert_eq!(
            serde_json::to_value(to_wire_error(&error)).unwrap(),
            json!({"code": "invalid", "message": "invalid/args: bad", "reason": "args"})
        );
        let gone = crate::errors::ChordError::new(crate::errors::ChordErrorCode::Gone, "gone");
        assert_eq!(
            serde_json::to_value(to_wire_error(&gone)).unwrap(),
            json!({"code": "gone", "message": "gone: gone"})
        );
        let back = from_wire_error(&to_wire_error(&error));
        assert_eq!(back.full_message(), "invalid/args: bad");
        assert_eq!(back.reason.as_deref(), Some("args"));
    }

    #[test]
    fn error_prefix_is_stripped_like_typescript() {
        assert_eq!(strip_code_prefix("invalid/args: bad"), "bad");
        assert_eq!(strip_code_prefix("gone: gone"), "gone");
        assert_eq!(strip_code_prefix("unavailable: x"), "x");
        assert_eq!(strip_code_prefix("Invalid: x"), "Invalid: x");
        assert_eq!(strip_code_prefix("invalid/: x"), "invalid/: x");
    }
}
