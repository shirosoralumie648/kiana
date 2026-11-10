//! evals/src/report.ts。
//!
//! 读取 Vitest JSON 报告（`@vitest-evals/core/node` 的 `readReportWorkspace` /
//! `readVitestJsonReportFile` 的等价实现），把每个任务的原始报告归一成一个
//! [`EvalObservation`]，再把成对观测汇总成可以写入 `report.json` / `report.txt` 的比较报告。
//!
//! 当前设计约束（改动需同步测试）：
//! - 观测/报告结构体的字段顺序就是 TS 的声明顺序（`serde` 按字段顺序写出）；
//! - 所有会写出的浮点走 [`JsNumber`]（JS `JSON.stringify` 的数字格式：`150` 而非 `150.0`、
//!   `1e+21`、`1e-7`、`-0` → `0`、`NaN`/`±Infinity` → `null`）；
//! - 所有 `toFixed` 走 [`kiana_ai::model_catalog::js_to_fixed`]（V8 的半数远离零），
//!   `toPrecision(15)` 用精确十进制展开实现（`round_significant`）；
//! - `localeCompare` 走 [`kiana_ai::model_catalog::locale_compare`]（固定 `en-US` collation）；
//!   含 ASCII 控制字符的字符串改用本模块的 `eval_locale_compare`（ICU 的 ignorable /
//!   空白权重语义，见该函数）。
//!
//! # 这个模块解决什么问题
//! `kiana-evals` 的 CLI（[`crate::cli`]）在容器里逐个跑评测任务，每个任务只留下一份 Vitest
//! JSON 报告。本模块把「一份报告」翻译成与具体任务无关的统一观测 [`EvalObservation`]，再把
//! 设计阶段的任务矩阵（`plan::create_task_plan` 的输出）与实际观测配对，产出 `report.json`
//! （机器读）与 `report.txt`（人读）。没有它，CLI 手里只有一堆互不相关的报告文件，无法回答
//! 「给模型加文档到底有没有提升」。
//!
//! # 执行流程
//! 1. [`read_task_observation`]：读报告 → 断言唯一且 `fullName` 匹配 → `skipped`/`pending`
//!    提前返回 → 取 workspace case 与 harness run → 写出会话快照 → 校验模型 → 收集 metrics
//!    → 处理 run errors → 校验分数，得到一条观测。
//! 2. [`summarize_eval_observations`]：按 `(eval_set, case_id, model, run_number)` 分组；
//!    每组要求两个变体各恰好一条 `scored` 观测，否则整组进入 `blocked_pairs`。
//! 3. [`format_eval_comparison_report`]：把汇总渲染成固定列宽的文本；JSON 侧由 `serde`
//!    直接序列化，字段顺序就是 TS 的声明顺序。
//!
//! # 重要约束（改代码前先读）
//! - **字节级稳定**：`tests/comparison.rs` 逐字比较 `report.txt`（黄金文件
//!   `tests/golden/report_format.txt`）与 `report.json`（内联字面量），`tests/report.rs`
//!   用字面量比较观测 JSON。字段顺序、键名、数字与百分比格式都不能动，新增字段会直接
//!   破坏这些断言。
//! - **宽松读取、严格校验**：Vitest 报告先经 V8 `JSON.parse` 再交给 zod，所以读取端必须接受
//!   重复键（取最后一个）、孤立代理项转义与超出 f64 的数字（`1e999`）；但 schema 本身是严格的
//!   （`.strict()` 对应 `deny_unknown_fields`，`.optional()` 对应拒绝 `null`），任何形状不符
//!   都会让该断言的 meta 整体失效，任务记成 `errored`。
//! - **失败折叠为数据**：除会话快照写盘失败（[`ReportError`]）外，所有读取/解析/校验失败都
//!   返回 `errored` 观测而不是 `Err`，这样一个任务失败不会中断整轮评测。
//! - **排序不是字节序**：分组与 eval set 排序按 ICU `en-US` 的 `localeCompare` 语义（含控制
//!   字符的权重见 `eval_locale_compare`），直接比较 `str` 会得到不同顺序。

use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::path::Path;

use indexmap::IndexMap;
use kiana_ai::model_catalog::{js_to_fixed, locale_compare};
use num_bigint::BigUint;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::docker::node_system_error;
use crate::plan::{DOCUMENTATION_VARIANTS, DocumentationVariant, EvalTask};

/// `kianaSessionJsonl`：harness run 的 artifacts 里存放完整会话 JSONL 的键名。
///
/// TS: `report.ts#KIANA_SESSION_SNAPSHOT_ARTIFACT`
///
/// 读取方是 `persist_session`（由 [`read_task_observation`] 调用）：值必须是字符串才会写出
/// `<artifact>/<variant>/sessions/<sha256>/session.jsonl`；缺失或不是字符串时静默跳过持久化，
/// 不影响该任务的评分（见 `tests/report.rs` 的 `ignores_a_non_string_session_artifact`）。
pub const KIANA_SESSION_SNAPSHOT_ARTIFACT: &str = "kianaSessionJsonl";

// ---------------------------------------------------------------------------
// JS 数字格式化（kiana-json/js_num.rs 的本地移植）
// ---------------------------------------------------------------------------

/// ECMAScript 数字 → JSON 文本。
///
/// 移植自 `crates/kiana-json/src/js_num.rs`（`kiana-json` 不在 `kiana-evals` 的依赖白名单里）。
/// `serde_json` 自带的 f64 writer 与 JS 不同：`150.0`、`1e21`、`-0.0`、`NaN` 都会写错。
pub(crate) mod js_number {
    /// 把 `f64` 写成与 JS `JSON.stringify` / `String(number)` 一致的文本（不含引号）。
    ///
    /// `NaN` 与 `±Infinity` 在 JSON 里写作 `null`——这就是 TS 里 `JSON.stringify(NaN)` 的结果。
    pub fn format_js_number(value: f64) -> String {
        if value.is_nan() || value.is_infinite() {
            return "null".to_string();
        }
        if value == 0.0 {
            // -0 与 +0 都是 "0"。
            return "0".to_string();
        }

        // ryu 给出最短往返表示；再按 ECMA-262 Number::toString 的规则重排。
        let mut buffer = ryu::Buffer::new();
        let ryu_text = buffer.format_finite(value);
        let (digits, n) = decompose(ryu_text);

        let negative = value.is_sign_negative();
        let k = digits.len() as i32;
        // ECMA-262 Number::toString 的四个区间（k = 有效数字位数，n = 小数点位置）：
        // 1) 纯整数且无需补零；2) 小数点在有效数字中间；3) `0.000…` 形式；4) 科学计数法。
        let body = if k <= n && n <= 21 {
            let mut text = digits;
            for _ in 0..(n - k) {
                text.push('0');
            }
            text
        } else if 0 < n && n <= 21 {
            let split = n as usize;
            let mut text = String::with_capacity(digits.len() + 1);
            text.push_str(&digits[..split]);
            text.push('.');
            text.push_str(&digits[split..]);
            text
        } else if -6 < n && n <= 0 {
            let mut text = String::with_capacity(digits.len() + 2 + (-n) as usize);
            text.push_str("0.");
            for _ in 0..(-n) {
                text.push('0');
            }
            text.push_str(&digits);
            text
        } else {
            let mut text = String::with_capacity(digits.len() + 6);
            text.push_str(&digits[..1]);
            if digits.len() > 1 {
                text.push('.');
                text.push_str(&digits[1..]);
            }
            text.push('e');
            let exponent = n - 1;
            if exponent > 0 {
                text.push('+');
            }
            text.push_str(&exponent.to_string());
            text
        };

        if negative {
            let mut text = String::with_capacity(body.len() + 1);
            text.push('-');
            text.push_str(&body);
            text
        } else {
            body
        }
    }

    /// ECMAScript `String(number)`（`Number::toString`，即模板字符串里的 `${number}`）。
    ///
    /// 与 [`format_js_number`] 只在非有限值上不同：`String(Infinity)` 是 `"Infinity"`，而
    /// `JSON.stringify(Infinity)` 是 `"null"`。有限值（包括 `-0`）两者一致。
    pub fn format_js_string(value: f64) -> String {
        if value.is_nan() {
            "NaN".to_string()
        } else if value == f64::INFINITY {
            "Infinity".to_string()
        } else if value == f64::NEG_INFINITY {
            "-Infinity".to_string()
        } else {
            format_js_number(value)
        }
    }

    /// 把 ryu 的文本拆成 (有效数字串, 小数点位置 n)。
    ///
    /// 例：`"1.5e-7"` → `("15", -6)`。去掉前导/尾随零后，`n` 直接对应 ECMA-262 里
    /// `Number::toString` 的 n（小数点相对于有效数字串起点的位置）。
    fn decompose(text: &str) -> (String, i32) {
        let text = text.strip_prefix('-').unwrap_or(text);

        let (mantissa, exponent) = match text.find(['e', 'E']) {
            Some(index) => {
                let (mantissa, exponent) = text.split_at(index);
                (mantissa, exponent[1..].parse::<i32>().unwrap_or(0))
            }
            None => (text, 0),
        };

        let (integer, fraction) = match mantissa.find('.') {
            Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
            None => (mantissa, ""),
        };

        let mut digits = String::with_capacity(integer.len() + fraction.len());
        digits.push_str(integer);
        digits.push_str(fraction);

        let mut n = integer.len() as i32 + exponent;

        let leading = digits.chars().take_while(|c| *c == '0').count() as i32;
        if leading > 0 {
            digits.drain(..leading as usize);
            n -= leading;
        }
        let trailing = digits.chars().rev().take_while(|c| *c == '0').count();
        if trailing > 0 && trailing < digits.len() {
            digits.truncate(digits.len() - trailing);
        }
        if digits.is_empty() {
            digits.push('0');
            n = 1;
        }
        (digits, n)
    }
}

/// JS 数字（`number`）的 JSON 序列化包装。
///
/// `Serialize` 通过 [`serde_json::value::RawValue`] 写出 `js_number::format_js_number` 的文本，
/// 输出与 `JSON.stringify` 的数字格式一致（当前稳定契约）；`Deserialize` 按 `f64` 读取（不做有限性/范围校验）。
///
/// 注意：RawValue 的写出依赖 serde_json 的特殊令牌，若把 `JsNumber` 嵌进 serde 的 `Content`
/// （例如 internally tagged enum 的 derive）原始文本会丢失或序列化失败——这正是
/// [`TranscriptEvent`] 手写 `Serialize` 的原因。
///
/// TS: 所有 `report.ts` 里会进入 `JSON.stringify` 的 `number` 字段。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JsNumber(pub f64);

impl Serialize for JsNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let text = js_number::format_js_number(self.0);
        let raw =
            serde_json::value::RawValue::from_string(text).map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(JsNumber(f64::deserialize(deserializer)?))
    }
}

/// `serde_json::Value` 里的数字按 ECMAScript `Number::toString` 写出（`JSON.stringify` 语义）。
///
/// `serde_json` 用 ryu 写非整数：`5e-6`、`1e21` 与 JS 的 `0.000005`、`1e+21` 不同。
/// `usage.metadata` 里的 token 数/费用是 JS number，必须与 `JSON.stringify` 的数字格式一致（下游按该格式比较）。
struct JsJsonValue<'a>(&'a Value);

impl Serialize for JsJsonValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Number(number) => {
                let Some(value) = number.as_f64() else {
                    return number.serialize(serializer);
                };
                let text = js_number::format_js_number(value);
                serde_json::value::RawValue::from_string(text)
                    .map_err(serde::ser::Error::custom)?
                    .serialize(serializer)
            }
            Value::Array(items) => {
                use serde::ser::SerializeSeq as _;
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(&JsJsonValue(item))?;
                }
                sequence.end()
            }
            Value::Object(map) => serialize_js_value_map(map, serializer),
            other => other.serialize(serializer),
        }
    }
}

/// [`JsJsonValue`] 的对象分支（键序保持不变）。
fn serialize_js_value_map<S>(
    map: &serde_json::Map<String, Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeMap as _;
    let mut object = serializer.serialize_map(Some(map.len()))?;
    for (key, value) in map {
        object.serialize_entry(key, &JsJsonValue(value))?;
    }
    object.end()
}

/// `Option<metadata map>` 的 `serialize_with`：`None` 仍由 `skip_serializing_if` 跳过。
fn serialize_optional_js_metadata<S>(
    metadata: &Option<serde_json::Map<String, Value>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match metadata {
        Some(map) => serialize_js_value_map(map, serializer),
        None => serializer.serialize_none(),
    }
}

/// `serde` 的 `Option<T>` 默认把 `null` 当成 `None`；zod 的 `.optional()` **拒绝** `null`
/// （只有字段缺失才是 `undefined`）。该反序列化器让 `null` 报错，配合 `#[serde(default)]`
/// 保留「缺失 → `None`」。
mod serde_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }
}

// ---------------------------------------------------------------------------
// @vitest-evals/core 的 JSON 形状（zod schema 的 Rust 对应物）
// ---------------------------------------------------------------------------

/// Vitest 用例状态。
///
/// TS: `@vitest-evals/core` 的 `VitestJsonStatusSchema`（`ReportCase["status"]`）
///
/// `Failed` 在观测层表示「基础设施/评测失败」，会被 [`classify_case_status`] 映射成
/// `errored` 而不是「分数为 0」；分数只来自 eval meta 的 `avgScore`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VitestStatus {
    /// 通过。
    Passed,
    /// 失败。
    Failed,
    /// 跳过。
    Skipped,
    /// 待定。
    Pending,
    /// TODO。
    Todo,
    /// 禁用。
    Disabled,
}

impl VitestStatus {
    /// TS 字符串字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            VitestStatus::Passed => "passed",
            VitestStatus::Failed => "failed",
            VitestStatus::Skipped => "skipped",
            VitestStatus::Pending => "pending",
            VitestStatus::Todo => "todo",
            VitestStatus::Disabled => "disabled",
        }
    }
}

/// 报告里的源位置（`{line, column}`，passthrough）。
///
/// TS: `@vitest-evals/core` 的 `VitestJsonLocationSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VitestLocation {
    /// 行号（1-based）。
    pub line: JsNumber,
    /// 列号（1-based）。
    pub column: JsNumber,
}

/// `NormalizedErrorSchema`：至少含 `message`，其余键以 JSON 值透传。
///
/// TS: `@vitest-evals/core` 的 `NormalizedErrorSchema`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizedError {
    /// 错误消息。
    pub message: String,
    /// 可选的错误类型名。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub r#type: Option<String>,
    /// `catchall(JsonValueSchema)` 的其余键。
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}

/// transcript 消息的角色。
///
/// TS: `TranscriptMessageEventSchema` 的 `role` 枚举
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    /// system。
    System,
    /// user。
    User,
    /// assistant。
    Assistant,
}

/// 归一化后的 transcript 事件（`discriminatedUnion("type", ...)`）。
///
/// TS: `@vitest-evals/core` 的 `TranscriptEventSchema`
///
/// `Serialize` 是手写的：internally tagged 的 derive 会先把 variant 内容缓冲成 `Content`，
/// 而 [`JsNumber`] 依赖 `RawValue` 的 serde_json 特殊令牌，无法穿过 `ContentSerializer`。
/// 手写实现直接按 TS 的键序写 map（`type` 在前），因此 `durationMs` 也能保持 JS 的数字格式。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TranscriptEvent {
    /// `{type:"message", role, content?, metadata?}`
    Message {
        /// 消息角色。
        role: TranscriptRole,
        /// 消息内容（任意 JSON 值）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        /// 附加元数据。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        metadata: Option<serde_json::Map<String, Value>>,
    },
    /// `{type:"tool_call", id, name, arguments?, ...}`
    ToolCall {
        /// 工具调用 id。
        id: String,
        /// 工具名。
        name: String,
        /// 工具参数对象。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        arguments: Option<serde_json::Map<String, Value>>,
        /// 开始时间（ISO 字符串）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        started_at: Option<String>,
        /// 结束时间（ISO 字符串）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        finished_at: Option<String>,
        /// 持续毫秒数。
        #[serde(default, deserialize_with = "serde_option::deserialize")]
        duration_ms: Option<JsNumber>,
        /// 附加元数据。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        metadata: Option<serde_json::Map<String, Value>>,
    },
    /// `{type:"tool_result", toolCallId, name?, content?, error?, ...}`
    ToolResult {
        /// 对应的工具调用 id。
        tool_call_id: String,
        /// 工具名。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        name: Option<String>,
        /// 结果内容（任意 JSON 值）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        /// 错误（仅当失败）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        error: Option<NormalizedError>,
        /// 开始时间（ISO 字符串）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        started_at: Option<String>,
        /// 结束时间（ISO 字符串）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        finished_at: Option<String>,
        /// 持续毫秒数。
        #[serde(default, deserialize_with = "serde_option::deserialize")]
        duration_ms: Option<JsNumber>,
        /// 附加元数据。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        metadata: Option<serde_json::Map<String, Value>>,
    },
}

impl Serialize for TranscriptEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap as _;

        match self {
            TranscriptEvent::Message {
                role,
                content,
                metadata,
            } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "message")?;
                map.serialize_entry("role", role)?;
                if let Some(content) = content {
                    map.serialize_entry("content", content)?;
                }
                if let Some(metadata) = metadata {
                    map.serialize_entry("metadata", metadata)?;
                }
                map.end()
            }
            TranscriptEvent::ToolCall {
                id,
                name,
                arguments,
                started_at,
                finished_at,
                duration_ms,
                metadata,
            } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "tool_call")?;
                map.serialize_entry("id", id)?;
                map.serialize_entry("name", name)?;
                if let Some(arguments) = arguments {
                    map.serialize_entry("arguments", arguments)?;
                }
                if let Some(started_at) = started_at {
                    map.serialize_entry("startedAt", started_at)?;
                }
                if let Some(finished_at) = finished_at {
                    map.serialize_entry("finishedAt", finished_at)?;
                }
                if let Some(duration_ms) = duration_ms {
                    map.serialize_entry("durationMs", duration_ms)?;
                }
                if let Some(metadata) = metadata {
                    map.serialize_entry("metadata", metadata)?;
                }
                map.end()
            }
            TranscriptEvent::ToolResult {
                tool_call_id,
                name,
                content,
                error,
                started_at,
                finished_at,
                duration_ms,
                metadata,
            } => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "tool_result")?;
                map.serialize_entry("toolCallId", tool_call_id)?;
                if let Some(name) = name {
                    map.serialize_entry("name", name)?;
                }
                if let Some(content) = content {
                    map.serialize_entry("content", content)?;
                }
                if let Some(error) = error {
                    map.serialize_entry("error", error)?;
                }
                if let Some(started_at) = started_at {
                    map.serialize_entry("startedAt", started_at)?;
                }
                if let Some(finished_at) = finished_at {
                    map.serialize_entry("finishedAt", finished_at)?;
                }
                if let Some(duration_ms) = duration_ms {
                    map.serialize_entry("durationMs", duration_ms)?;
                }
                if let Some(metadata) = metadata {
                    map.serialize_entry("metadata", metadata)?;
                }
                map.end()
            }
        }
    }
}

/// 归一化会话（`{events, provider?, model?, metadata?}`）。
///
/// TS: `@vitest-evals/core` 的 `NormalizedSessionSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSession {
    /// 事件序列。
    pub events: Vec<TranscriptEvent>,
    /// provider id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub provider: Option<String>,
    /// model id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub model: Option<String>,
    /// 附加元数据（数字按 `JSON.stringify` 的 ECMAScript 格式写出）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize",
        serialize_with = "serialize_optional_js_metadata"
    )]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// 使用量汇总（`UsageSummarySchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `UsageSummarySchema`
///
/// 所有字段都可缺失；已知字段的类型必须正确（`deny_unknown_fields` 让未知键直接失败）。
/// `metadata` 里的数字在序列化时也按 ECMAScript 格式输出（内部用 `JsJsonValue` 实现）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSummary {
    /// provider id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub provider: Option<String>,
    /// model id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub model: Option<String>,
    /// 输入 token。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub input_tokens: Option<JsNumber>,
    /// 输出 token。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub output_tokens: Option<JsNumber>,
    /// reasoning token。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub reasoning_tokens: Option<JsNumber>,
    /// 总 token。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub total_tokens: Option<JsNumber>,
    /// 工具调用次数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub tool_calls: Option<JsNumber>,
    /// 重试次数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub retries: Option<JsNumber>,
    /// 附加元数据（数字按 `JSON.stringify` 的 ECMAScript 格式写出）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize",
        serialize_with = "serialize_optional_js_metadata"
    )]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// 计时汇总（`TimingSummarySchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `TimingSummarySchema`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimingSummary {
    /// 总毫秒数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub total_ms: Option<JsNumber>,
    /// 附加元数据（数字按 `JSON.stringify` 的 ECMAScript 格式写出）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize",
        serialize_with = "serialize_optional_js_metadata"
    )]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// span 种类（`NormalizedSpanSchema.kind`）。
///
/// TS: `@vitest-evals/core` 的 `NormalizedSpanSchema`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    /// run。
    Run,
    /// agent。
    Agent,
    /// model。
    Model,
    /// tool。
    Tool,
    /// guardrail。
    Guardrail,
    /// handoff。
    Handoff,
    /// custom。
    Custom,
}

/// span 状态（`ok` / `error`）。
///
/// TS: `@vitest-evals/core` 的 `NormalizedSpanSchema.status`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    /// 成功。
    Ok,
    /// 失败。
    Error,
}

/// 归一化 span 事件。
///
/// TS: `@vitest-evals/core` 的 `NormalizedSpanEventSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedSpanEvent {
    /// 事件名。
    pub name: String,
    /// 时间戳（ISO 字符串）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub timestamp: Option<String>,
    /// 属性。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub attributes: Option<serde_json::Map<String, Value>>,
}

/// 归一化 span。
///
/// TS: `@vitest-evals/core` 的 `NormalizedSpanSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedSpan {
    /// span id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub id: Option<String>,
    /// trace id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub trace_id: Option<String>,
    /// 父 span id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub parent_id: Option<String>,
    /// span 名。
    pub name: String,
    /// span 种类。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub kind: Option<SpanKind>,
    /// 开始时间。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub started_at: Option<String>,
    /// 结束时间。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub finished_at: Option<String>,
    /// 持续毫秒数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub duration_ms: Option<JsNumber>,
    /// 状态。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub status: Option<SpanStatus>,
    /// 错误。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub error: Option<NormalizedError>,
    /// 属性。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub attributes: Option<serde_json::Map<String, Value>>,
    /// span 事件。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub events: Option<Vec<NormalizedSpanEvent>>,
}

/// 归一化 trace。
///
/// TS: `@vitest-evals/core` 的 `NormalizedTraceSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedTrace {
    /// trace id。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub id: Option<String>,
    /// trace 名。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub name: Option<String>,
    /// 开始时间。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub started_at: Option<String>,
    /// 结束时间。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub finished_at: Option<String>,
    /// 持续毫秒数。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub duration_ms: Option<JsNumber>,
    /// 附加元数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub metadata: Option<serde_json::Map<String, Value>>,
    /// span 列表。
    pub spans: Vec<NormalizedSpan>,
}

/// 一次 harness run 的完整快照（`HarnessRunSchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `HarnessRunSchema`
///
/// `session`、`usage`、`errors` 是必填键（TS 的 schema 没有把它们标成 optional），其余可选。
/// 因为是严格对象，任何未知键都会让整个 harness meta 校验失败，该任务最终变成 `errored`
/// （`tests/report.rs` 的 `rejects_unknown_harness_run_fields`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRun {
    /// harness 输出（任意 JSON 值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// 归一化会话。
    pub session: NormalizedSession,
    /// 使用量。
    pub usage: UsageSummary,
    /// 计时。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub timings: Option<TimingSummary>,
    /// artifacts（字符串键 → 任意 JSON 值）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub artifacts: Option<serde_json::Map<String, Value>>,
    /// traces。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub traces: Option<Vec<NormalizedTrace>>,
    /// run 级错误（每个都是 JSON 对象）。
    pub errors: Vec<serde_json::Map<String, Value>>,
}

/// 单项评分（`EvalScoreSchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `EvalScoreSchema`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalScore {
    /// 评分器名。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub name: Option<String>,
    /// 分数（可为 `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<JsNumber>,
    /// 附加元数据。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// 工具调用记录（`ToolCallSchema`，按 `status` 判别）。
///
/// TS: `@vitest-evals/core` 的 `ToolCallSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ToolCall {
    /// 未完成。
    Pending {
        /// 工具名。
        name: String,
        /// 参数。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        arguments: Option<serde_json::Map<String, Value>>,
    },
    /// 成功。
    Ok {
        /// 工具名。
        name: String,
        /// 参数。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        arguments: Option<serde_json::Map<String, Value>>,
        /// 结果（任意 JSON 值）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
    },
    /// 失败。
    Error {
        /// 工具名。
        name: String,
        /// 参数。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "serde_option::deserialize"
        )]
        arguments: Option<serde_json::Map<String, Value>>,
        /// 错误。
        error: NormalizedError,
    },
}

/// 评测元数据（`EvalMetaSchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `EvalMetaSchema`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvalMeta {
    /// 逐评分器结果。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub scores: Option<Vec<EvalScore>>,
    /// 平均分（可为 `null`，也可缺失）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_score: Option<JsNumber>,
    /// 结构化输出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// 阈值是否失败。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub threshold_failed: Option<bool>,
    /// 工具调用记录。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// harness 元数据（`HarnessMetaSchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `HarnessMetaSchema`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessMeta {
    /// harness 名（文档变体）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub name: Option<String>,
    /// harness run。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "serde_option::deserialize"
    )]
    pub run: Option<HarnessRun>,
}

/// 工作区里的单个用例（`ReportCaseSchema`，`.strict()`）。
///
/// TS: `@vitest-evals/core` 的 `ReportCaseSchema`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportCase {
    /// 用例 id（`runId:file:line:fullName`）。
    pub id: String,
    /// 所属 run id。
    pub run_id: String,
    /// run 的来源（通常是报告文件路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 绝对文件名。
    pub file: String,
    /// 展示用文件名。
    pub display_file: String,
    /// 用例标题。
    pub title: String,
    /// 完整用例名。
    pub full_name: String,
    /// 祖先标题。
    pub ancestor_titles: Vec<String>,
    /// 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// 展示名（`ancestorTitles > title`）。
    pub display_name: String,
    /// 状态。
    pub status: VitestStatus,
    /// 持续毫秒数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<JsNumber>,
    /// 源位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<VitestLocation>,
    /// 失败消息。
    #[serde(default)]
    pub failure_messages: Vec<String>,
    /// 评测元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval: Option<EvalMeta>,
    /// harness 元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessMeta>,
}

// ---------------------------------------------------------------------------
// Vitest JSON 报告的校验（zod schema 的等价物）
// ---------------------------------------------------------------------------

/// `report.ts` 的 `readReportWorkspace` / `readVitestJsonReportFile`（`@vitest-evals/core/node`）
/// 的等价读取器。
///
/// 语义要点（与 zod 实现逐条对应）：
/// - 报告 schema 是 `.passthrough()`：未知键允许出现，但必填键缺失/类型错误会让整份报告无效；
/// - `readEvalTaskMeta` 只保留 `meta` 对象里的 `eval` / `harness` 两键，两者都不存在 → 该断言
///   **不产生任何 workspace case**；`eval` / `harness` 本身用严格 schema 校验，校验失败同样丢弃；
/// - harness 存在而 eval 缺失时，`collectReportWorkspace` 合成
///   `{avgScore: passed ? 1 : failed ? 0 : null, scores: [], thresholdFailed: false}`；
/// - `readTaskObservation` 里两次读取被一个 `Promise.all().catch(() => undefined)` 包住，
///   任何读取/解析/校验失败都等价于「没有 workspace」。
mod reader {
    use super::*;

    // `num*` / `startTime` / `success` 只用于复刻 zod 的「必填键/类型」校验：报告逻辑不需要
    // 它们的值，但缺失或类型错误必须让整份报告无效（`readTaskObservation` 因此得到 errored）。
    #[allow(dead_code)]
    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct VitestJsonReport {
        pub num_failed_tests: f64,
        pub num_passed_tests: f64,
        pub num_pending_tests: f64,
        pub num_todo_tests: f64,
        pub num_total_tests: f64,
        pub start_time: f64,
        pub success: bool,
        #[serde(default)]
        pub test_results: Vec<VitestJsonFile>,
    }

    // 同上：`message` / `status` / `startTime` / `endTime` 是 zod 的必填/可空键，值不使用。
    #[allow(dead_code)]
    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct VitestJsonFile {
        pub message: String,
        pub name: String,
        pub status: VitestFileStatus,
        #[serde(default, deserialize_with = "deserialize_optional_finite")]
        pub start_time: Option<f64>,
        #[serde(default, deserialize_with = "deserialize_optional_finite")]
        pub end_time: Option<f64>,
        #[serde(default)]
        pub assertion_results: Vec<VitestJsonAssertion>,
    }

    #[derive(Clone, Copy, Debug, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum VitestFileStatus {
        Failed,
        Passed,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct VitestJsonAssertion {
        #[serde(default)]
        pub ancestor_titles: Vec<String>,
        pub full_name: String,
        pub status: VitestStatus,
        pub title: String,
        #[serde(default)]
        pub meta: Option<Value>,
        #[serde(default)]
        pub duration: Option<f64>,
        #[serde(default)]
        pub failure_messages: Option<Vec<String>>,
        #[serde(default)]
        pub location: Option<VitestJsonLocation>,
        #[serde(default, deserialize_with = "serde_option::deserialize")]
        pub tags: Option<Vec<String>>,
    }

    #[derive(Clone, Debug, Deserialize)]
    pub struct VitestJsonLocation {
        pub line: f64,
        pub column: f64,
    }

    /// 超出 f64 范围的数字字面量（`1e999`）在容错重写后使用的哨兵对象键。
    ///
    /// V8 的 `JSON.parse` 把这类字面量解析成 `±Infinity`，`serde_json` 却直接报
    /// "number out of range"。重写阶段把它们换成 `{ "<key>": true }` 哨兵：zod 里被
    /// `preprocess` 过的字段（`avgScore`、`scores[].score`、文件 `startTime`/`endTime`）
    /// 把它当 `null`/缺失，其余位置按 zod 的 `finite` 校验失败处理。
    const NONFINITE_SENTINEL_KEY: &str = "\u{0}kiana-evals:non-finite-number\u{0}";

    fn nonfinite_sentinel() -> Value {
        let mut map = serde_json::Map::new();
        map.insert(NONFINITE_SENTINEL_KEY.to_string(), Value::Bool(true));
        Value::Object(map)
    }

    pub(super) fn is_nonfinite_sentinel(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                map.len() == 1 && map.get(NONFINITE_SENTINEL_KEY) == Some(&Value::Bool(true))
            }
            _ => false,
        }
    }

    fn contains_nonfinite(value: &Value) -> bool {
        match value {
            Value::Array(items) => items.iter().any(contains_nonfinite),
            Value::Object(map) => {
                is_nonfinite_sentinel(value) || map.values().any(contains_nonfinite)
            }
            _ => false,
        }
    }

    /// zod `OptionalFiniteNumberSchema` 的预处理器：`null` 与非有限数字 → 缺失。
    fn deserialize_optional_finite<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Null => Ok(None),
            Value::Number(number) => number
                .as_f64()
                .filter(|number| number.is_finite())
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("expected a finite number")),
            value if is_nonfinite_sentinel(&value) => Ok(None),
            _ => Err(serde::de::Error::custom("expected a finite number")),
        }
    }

    /// 解析 Vitest JSON 报告：先常规解析；失败时按 V8 `JSON.parse` 的宽松语义重写字面量后重试。
    ///
    /// 第二次尝试先落到 [`Value`] 再转结构体：`serde` 的 derive 对重复键报 `duplicate field`，
    /// 而 `JSON.parse`（zod 拿到的就是它解析出的普通对象）保留最后一个值。`Value` 的 map
    /// 解析天然是「已存在键原位替换」，与 V8 一致。
    ///
    /// 三次尝试的顺序是刻意的：绝大多数报告第一次就成功；重复键只有经 [`Value`] 中转才会
    /// 按「后者覆盖」处理；V8-only 字面量（`1e999`、孤立代理项）只在最后一次重写后才可解析。
    pub fn parse_report(text: &str) -> Option<VitestJsonReport> {
        // 1) 直接反序列化到结构体：最快，但 derive 对重复键报错。
        if let Ok(report) = serde_json::from_str(text) {
            return Some(report);
        }
        // 2) 经 Value 中转：重复键取最后一个（与 JSON.parse 一致）。
        if let Some(report) = parse_report_value(text) {
            return Some(report);
        }
        // 3) 重写 V8 接受、serde_json 拒绝的字面量，再走 2)。
        let rewritten = rewrite_v8_json(text)?;
        parse_report_value(&rewritten)
    }

    /// [`parse_report`] 的 `Value` 中转解析（重复键取最后一个）。
    fn parse_report_value(text: &str) -> Option<VitestJsonReport> {
        let value: Value = serde_json::from_str(text).ok()?;
        serde_json::from_value(value).ok()
    }

    /// 把 V8 `JSON.parse` 接受、但 `serde_json` 拒绝的两类字面量改写成等价形式：
    ///
    /// - 超出 f64 范围的数字（`1e999`）：V8 得到 `±Infinity`；替换成 [`nonfinite_sentinel`]。
    /// - 未配对的 UTF-16 代理项转义（`"\ud800"`）：V8 保留孤立代理项，Rust `String` 无法表示，
    ///   替换成 `�`（Node 把这种字符串编码成 UTF-8 时同样写成 U+FFFD）。
    ///
    /// 只在首次解析失败后调用；没有可改写的内容时返回 `None`。
    pub(super) fn rewrite_v8_json(text: &str) -> Option<String> {
        let bytes = text.as_bytes();
        let sentinel_text =
            serde_json::to_string(&nonfinite_sentinel()).expect("a sentinel object serializes");
        let mut output = String::with_capacity(text.len());
        let mut copied_until = 0usize;
        let mut index = 0usize;
        let mut changed = false;

        // 单遍扫描：`"` 进入字符串并跳过其中的转义；数字只在字符串外识别；其余字节原样复制。
        // 只有确实改写了内容才返回 `Some`，调用方据此判断是否值得再解析一次。
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    index += 1;
                    while index < bytes.len() {
                        match bytes[index] {
                            b'"' => {
                                index += 1;
                                break;
                            }
                            b'\\' => {
                                // `\uXXXX` 恰好看 6 个字节；截断或非 `\u` 转义走下面的通用分支。
                                let escape_end = index + 6;
                                let mut handled = false;
                                if escape_end <= bytes.len()
                                    && bytes[index + 1] == b'u'
                                    && let Some(code) = hex4(&bytes[index + 2..escape_end])
                                {
                                    if is_high_surrogate(code)
                                        && low_surrogate_escape(bytes, escape_end).is_some()
                                    {
                                        // 合法的代理对：两个转义都原样保留。
                                        index = escape_end + 6;
                                        handled = true;
                                    } else if is_high_surrogate(code) || is_low_surrogate(code) {
                                        output.push_str(&text[copied_until..index]);
                                        output.push_str("\\uFFFD");
                                        index = escape_end;
                                        copied_until = index;
                                        changed = true;
                                        handled = true;
                                    } else {
                                        index = escape_end;
                                        handled = true;
                                    }
                                }
                                if !handled {
                                    // 非 `\u` 转义：跳过反斜杠和下一个字符。
                                    index += 1;
                                    if let Some(character) = text[index..].chars().next() {
                                        index += character.len_utf8();
                                    }
                                }
                            }
                            _ => index += 1,
                        }
                    }
                }
                b'-' | b'0'..=b'9' => {
                    let start = index;
                    index = scan_number(bytes, index);
                    if text[start..index]
                        .parse::<f64>()
                        .is_ok_and(f64::is_infinite)
                    {
                        output.push_str(&text[copied_until..start]);
                        output.push_str(&sentinel_text);
                        copied_until = index;
                        changed = true;
                    }
                }
                _ => index += 1,
            }
        }

        if !changed {
            return None;
        }
        output.push_str(&text[copied_until..]);
        Some(output)
    }

    /// 返回 JSON 数字字面量在 `start` 之后的结束位置（不校验语法，非法的留给解析器报错）。
    fn scan_number(bytes: &[u8], start: usize) -> usize {
        let mut index = start;
        if index < bytes.len() && bytes[index] == b'-' {
            index += 1;
        }
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
            index += 1;
            if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
                index += 1;
            }
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        index
    }

    fn hex4(bytes: &[u8]) -> Option<u16> {
        let mut value = 0u16;
        for &byte in bytes {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            value = value * 16 + u16::from(digit);
        }
        Some(value)
    }

    fn low_surrogate_escape(bytes: &[u8], start: usize) -> Option<()> {
        let end = start + 6;
        if end > bytes.len() || bytes[start] != b'\\' || bytes[start + 1] != b'u' {
            return None;
        }
        let code = hex4(&bytes[start + 2..end])?;
        is_low_surrogate(code).then_some(())
    }

    fn is_high_surrogate(code: u16) -> bool {
        (0xD800..=0xDBFF).contains(&code)
    }

    fn is_low_surrogate(code: u16) -> bool {
        (0xDC00..=0xDFFF).contains(&code)
    }

    /// `readEvalTaskMeta`：只抽取 `eval` / `harness`，缺两者之一才继续。
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct EvalTaskMeta {
        #[serde(default, deserialize_with = "serde_option::deserialize")]
        pub eval: Option<EvalMeta>,
        #[serde(default, deserialize_with = "serde_option::deserialize")]
        pub harness: Option<HarnessMeta>,
    }

    pub fn read_eval_task_meta(input: Option<&Value>) -> Option<EvalTaskMeta> {
        let Some(Value::Object(record)) = input else {
            return None;
        };
        let eval_present = record.contains_key("eval");
        let harness_present = record.contains_key("harness");
        if !eval_present && !harness_present {
            return None;
        }
        // TS 只把这两个键复制进新对象，其余键被丢弃（顶层严格校验因此不会看到它们）。
        let mut selected = serde_json::Map::new();
        if eval_present {
            selected.insert("eval".to_string(), record["eval"].clone());
        }
        if harness_present {
            selected.insert("harness".to_string(), record["harness"].clone());
        }
        let mut selected = Value::Object(selected);
        // `avgScore` / `scores[].score` 走 `NullableFiniteNumberSchema`：`±Infinity` 变 `null`。
        // 其余位置若残留哨兵，zod 的 `finite`/`JsonValue` 校验会拒绝整个 meta。
        if !sanitize_nonfinite_in_task_meta(&mut selected) {
            return None;
        }
        serde_json::from_value(selected).ok()
    }

    /// 在 zod `preprocess` 位置把哨兵还原成 `null`；返回 `false` 表示仍有未处理哨兵。
    fn sanitize_nonfinite_in_task_meta(value: &mut Value) -> bool {
        if let Value::Object(record) = value
            && let Some(Value::Object(eval)) = record.get_mut("eval")
        {
            if let Some(avg_score) = eval.get_mut("avgScore")
                && is_nonfinite_sentinel(avg_score)
            {
                *avg_score = Value::Null;
            }
            if let Some(Value::Array(scores)) = eval.get_mut("scores") {
                for score in scores {
                    if let Value::Object(score) = score
                        && let Some(value) = score.get_mut("score")
                        && is_nonfinite_sentinel(value)
                    {
                        *value = Value::Null;
                    }
                }
            }
        }
        !contains_nonfinite(value)
    }

    /// `collectReportWorkspace` 里与 `report.ts` 相关的部分：只保留 cases。
    ///
    /// 只有 `meta` 中带 `eval` 或 `harness` 的断言才会成为 case；`display_file` 把反斜杠
    /// 归一成 `/`（TS 的 `file.replace(/\\/g, "/")`），但 `id` 与 `file` 仍使用原始 `file.name`。
    pub fn collect_report_workspace(report: &VitestJsonReport, source: &str) -> Vec<ReportCase> {
        let run_id = source;
        let mut cases = Vec::new();
        for file in &report.test_results {
            for assertion in &file.assertion_results {
                let Some(meta) = read_eval_task_meta(assertion.meta.as_ref()) else {
                    continue;
                };
                let eval_meta = match meta.eval {
                    Some(eval) => Some(eval),
                    None if meta.harness.is_some() => Some(EvalMeta {
                        avg_score: match assertion.status {
                            VitestStatus::Passed => Some(JsNumber(1.0)),
                            VitestStatus::Failed => Some(JsNumber(0.0)),
                            _ => None,
                        },
                        scores: Some(Vec::new()),
                        threshold_failed: Some(false),
                        output: None,
                        tool_calls: None,
                    }),
                    None => None,
                };
                // 展示名归一化路径分隔符；`file`/`id` 仍保留原始路径（case 身份
                // `id = runId:file:line:fullName` 依赖它）。
                let display_file = file.name.replace('\\', "/");
                cases.push(ReportCase {
                    id: create_case_id(run_id, &file.name, assertion),
                    run_id: run_id.to_string(),
                    source: Some(source.to_string()),
                    file: file.name.clone(),
                    display_file,
                    title: assertion.title.clone(),
                    full_name: assertion.full_name.clone(),
                    ancestor_titles: assertion.ancestor_titles.clone(),
                    tags: assertion.tags.clone(),
                    display_name: format_display_name(assertion),
                    status: assertion.status,
                    duration_ms: assertion.duration.map(JsNumber),
                    location: assertion.location.as_ref().map(|location| VitestLocation {
                        line: JsNumber(location.line),
                        column: JsNumber(location.column),
                    }),
                    failure_messages: assertion.failure_messages.clone().unwrap_or_default(),
                    eval: eval_meta,
                    harness: meta.harness,
                });
            }
        }
        cases
    }

    fn create_case_id(run_id: &str, file: &str, assertion: &VitestJsonAssertion) -> String {
        // 没有 location 时行号记为 0（TS 的 `location?.line ?? 0`），仍参与 id 文本。
        let line = assertion
            .location
            .as_ref()
            .map(|location| location.line)
            .unwrap_or(0.0);
        format!(
            "{run_id}:{file}:{}:{}",
            js_number::format_js_number(line),
            assertion.full_name
        )
    }

    fn format_display_name(assertion: &VitestJsonAssertion) -> String {
        // 空片段（空的祖先标题或空 title）被过滤，避免出现 `"a >  > b"`。
        let parts: Vec<&str> = assertion
            .ancestor_titles
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(assertion.title.as_str()))
            .filter(|part| !part.is_empty())
            .collect();
        parts.join(" > ")
    }

    /// `displayReportSource(resultFile, cwd)`：报告路径相对 cwd 的展示名。
    ///
    /// 只做词法相对化：取当前进程 cwd（读不到时退化为 `/`），不访问文件系统、不解析符号链接。
    /// 相对结果为空、`.` 或以 `../` 开头（报告在 cwd 之外）时保留原始路径文本。
    pub fn display_report_source(result_file: &Path) -> String {
        let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new("/").to_path_buf());
        let result_file_text = result_file.to_string_lossy();
        let Some(relative) = relative_path(&cwd, result_file) else {
            return result_file_text.into_owned();
        };
        if relative.is_empty() || relative == "." || relative.starts_with("../") {
            return result_file_text.into_owned();
        }
        relative.replace('\\', "/")
    }

    /// Node `path.relative(from, to)` 的简单等价物（两侧都必须是绝对路径）。
    ///
    /// 实现是按组件的公共前缀 + `..` 回退，纯词法：不解析 `.`/`..`，也没有 Windows 盘符
    /// 不同的特判（两侧根不同时会走「无公共前缀」的一般路径）。
    fn relative_path(from: &Path, to: &Path) -> Option<String> {
        if !from.is_absolute() || !to.is_absolute() {
            return None;
        }
        let from_parts: Vec<String> = from
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        let to_parts: Vec<String> = to
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        let common = from_parts
            .iter()
            .zip(to_parts.iter())
            .take_while(|(left, right)| left == right)
            .count();
        let mut parts: Vec<String> = Vec::new();
        for _ in common..from_parts.len() {
            parts.push("..".to_string());
        }
        parts.extend(to_parts[common..].iter().cloned());
        Some(parts.join("/"))
    }
}

// ---------------------------------------------------------------------------
// 观测身份与观测
// ---------------------------------------------------------------------------

/// 一次评测运行的身份（不包含结果）。
///
/// TS: `report.ts#EvalRunIdentity`（字段顺序即序列化顺序）
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalRunIdentity {
    /// 评测集名。
    pub eval_set: String,
    /// 用例名。
    pub case_id: String,
    /// 文档变体。
    pub variant: DocumentationVariant,
    /// `<provider>/<model>`。
    pub model: String,
    /// 重复序号。
    pub run_number: u64,
}

/// 期望出现的一次运行；与 [`EvalRunIdentity`] 结构完全相同。
///
/// TS: `report.ts#ExpectedEvalRun`（纯别名）
pub type ExpectedEvalRun = EvalRunIdentity;

impl From<&EvalTask> for EvalRunIdentity {
    fn from(task: &EvalTask) -> Self {
        EvalRunIdentity {
            eval_set: task.eval_set.clone(),
            case_id: task.case_id.clone(),
            variant: task.variant,
            model: task.model.clone(),
            run_number: task.run_number,
        }
    }
}

impl From<EvalTask> for EvalRunIdentity {
    fn from(task: EvalTask) -> Self {
        EvalRunIdentity::from(&task)
    }
}

/// 观测结果（`outcome` 联合）。
///
/// TS: `EvalObservation["outcome"]`
///
/// 不变量：`Scored` 当且仅当 `EvalObservation::score` 为 `Some`
/// （[`read_task_observation`] 先校验分数、再据其是否存在决定 outcome）；其余 outcome
/// 一律不带分数。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalOutcome {
    /// 有分数（`score` 同时存在）。
    Scored,
    /// 完成但没有分数。
    Unscored,
    /// 被跳过。
    Skipped,
    /// 待定。
    Pending,
    /// 基础设施/校验失败。
    Errored,
}

impl EvalOutcome {
    /// TS 字符串字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            EvalOutcome::Scored => "scored",
            EvalOutcome::Unscored => "unscored",
            EvalOutcome::Skipped => "skipped",
            EvalOutcome::Pending => "pending",
            EvalOutcome::Errored => "errored",
        }
    }
}

impl std::fmt::Display for EvalOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 单次运行的观测。
///
/// TS: `report.ts#EvalObservation`
///
/// 字段顺序：身份 → 存在的 metrics（固定顺序）→ `outcome` → `score`（仅 `scored`）。
///
/// 语义要点：
/// - metrics 都可缺失；缺失时序列化省略该键（`skip_serializing_if`），而不是写 `null`；
/// - `errored` 观测**可能仍有 metrics**：harness run 可用且 metrics 合法，但断言状态是
///   `failed`、run errors 非空或分数非法时，`read_task_observation` 会保留 metrics 与
///   `errored` outcome（`tests/report.rs` 有多条对应用例）；
/// - `score` 一旦是 `Some`，就保证落在 `0..=1`（校验发生在读取侧，见 `validate_score`）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalObservation {
    /// 评测集名。
    pub eval_set: String,
    /// 用例名。
    pub case_id: String,
    /// 文档变体。
    pub variant: DocumentationVariant,
    /// `<provider>/<model>`。
    pub model: String,
    /// 重复序号。
    pub run_number: u64,
    /// 输入 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<JsNumber>,
    /// 输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<JsNumber>,
    /// 缓存读 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<JsNumber>,
    /// 缓存写 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<JsNumber>,
    /// 总 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<JsNumber>,
    /// 工具调用次数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<JsNumber>,
    /// 总耗时（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<JsNumber>,
    /// 估算成本（美元）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<JsNumber>,
    /// 结果。
    pub outcome: EvalOutcome,
    /// 分数（仅 `scored`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<JsNumber>,
}

impl EvalObservation {
    /// 由身份构造一个只有 `outcome` 的观测。
    ///
    /// 所有 metric 与 `score` 都是 `None`，因此 JSON 里只剩身份字段与 `outcome`。
    pub fn from_identity(identity: &EvalRunIdentity, outcome: EvalOutcome) -> Self {
        EvalObservation {
            eval_set: identity.eval_set.clone(),
            case_id: identity.case_id.clone(),
            variant: identity.variant,
            model: identity.model.clone(),
            run_number: identity.run_number,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: None,
            tool_calls: None,
            total_ms: None,
            estimated_cost_usd: None,
            outcome,
            score: None,
        }
    }
}

/// `readTaskObservation` 的唯一错误路径：持久化会话快照时的 I/O 失败。
///
/// TS 侧 `persistSession` 的 `mkdir`/`writeFile` rejection 不被捕获，直接冒泡
/// （`fs.promises` 的 `Error.message` 是 Node 的 `SystemError` 文本）。
///
/// [`crate::cli`] 用 `?` 传播该错误，因此一次写盘失败会中止整轮评测，而不是把该任务记成
/// `errored`——这与 TS 的未捕获 rejection 行为一致。
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    /// 创建目录或写入 `session.jsonl` 失败（Node `SystemError` 文本）。
    #[error("{0}")]
    Message(String),
}

/// 按 Vitest 状态映射到非 scored 的观测结果。
///
/// TS: `report.ts#classifyCaseStatus`（`failed → errored`、`skipped|todo|disabled → skipped`、
/// `pending → pending`，其他 → `undefined`）
///
/// `Passed` 返回 `None` 表示「还要继续检查」：是否 `scored` 取决于 harness run 与分数，
/// 不能在这一步决定。
pub fn classify_case_status(status: VitestStatus) -> Option<EvalOutcome> {
    match status {
        VitestStatus::Failed => Some(EvalOutcome::Errored),
        VitestStatus::Skipped | VitestStatus::Todo | VitestStatus::Disabled => {
            Some(EvalOutcome::Skipped)
        }
        VitestStatus::Pending => Some(EvalOutcome::Pending),
        VitestStatus::Passed => None,
    }
}

/// 构造一个 `errored` 观测（只有身份）。
///
/// TS: `report.ts#erroredObservation`
///
/// CLI 在 `docker::run_task` 返回 `None`（该任务没有生成报告文件）时直接使用它，
/// 保证每个设计任务都有一条观测可写进 `observations.jsonl`。
pub fn errored_observation(task: &EvalTask) -> EvalObservation {
    EvalObservation::from_identity(&EvalRunIdentity::from(task), EvalOutcome::Errored)
}

/// `report.ts#optionalMetric`：`undefined` → `None`；不是有限非负数 → 错误。
///
/// 返回 `Result` 而不直接丢值：metrics 非法会整条转成 `errored`（并丢失全部 metrics），
/// 这是 TS 里 `optionalMetric` 抛错被同一个 try/catch 捕获的结果。
fn optional_metric(value: Option<JsNumber>, _name: &str) -> Result<Option<JsNumber>, ()> {
    match value {
        None => Ok(None),
        Some(value) if value.0.is_finite() && value.0 >= 0.0 => Ok(Some(value)),
        Some(_) => Err(()),
    }
}

/// `optionalMetric(run.usage.metadata?.<name>, name)`：区分「缺失」与 JSON `null`。
///
/// 键不存在 → `Ok(None)`；键存在但不是数字（包括 `null`）→ `Err`，因为 TS 的
/// `metadata?.x` 只有在键缺失时才是 `undefined`。
fn optional_metadata_metric(
    metadata: Option<&serde_json::Map<String, Value>>,
    name: &str,
) -> Result<Option<JsNumber>, ()> {
    let Some(value) = metadata.and_then(|metadata| metadata.get(name)) else {
        return Ok(None);
    };
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(number) if number >= 0.0 => Ok(Some(JsNumber(number))),
            _ => Err(()),
        },
        _ => Err(()),
    }
}

/// `report.ts#validateScore`：`null`/缺失 → `None`；有限且 `0..=1` 之外 → 错误。
///
/// `Some(0.0)` 与 `None` 必须区分：0 分仍是有效评分（outcome 为 `scored`），
/// 缺失才会变成 `unscored`。
fn validate_score(value: Option<JsNumber>) -> Result<Option<f64>, ()> {
    match value {
        None => Ok(None),
        Some(value) if value.0.is_finite() && (0.0..=1.0).contains(&value.0) => Ok(Some(value.0)),
        Some(_) => Err(()),
    }
}

/// 从 harness run 读取 metrics（`optionalMetric` 的 8 个字段）。
///
/// 返回数组的顺序固定，`apply_metrics` 按同一顺序解构；`cacheReadTokens`、
/// `cacheWriteTokens`、`estimatedCostUsd` 不在 usage 的强类型字段里，而是从
/// `usage.metadata` 读取。任何一个字段非法就让整个数组失败（调用方丢弃所有 metrics）。
fn collect_metrics(run: &HarnessRun) -> Result<[Option<JsNumber>; 8], ()> {
    Ok([
        optional_metric(run.usage.input_tokens, "inputTokens")?,
        optional_metric(run.usage.output_tokens, "outputTokens")?,
        optional_metadata_metric(run.usage.metadata.as_ref(), "cacheReadTokens")?,
        optional_metadata_metric(run.usage.metadata.as_ref(), "cacheWriteTokens")?,
        optional_metric(run.usage.total_tokens, "totalTokens")?,
        optional_metric(run.usage.tool_calls, "toolCalls")?,
        optional_metric(
            run.timings.as_ref().and_then(|timings| timings.total_ms),
            "totalMs",
        )?,
        optional_metadata_metric(run.usage.metadata.as_ref(), "estimatedCostUsd")?,
    ])
}

/// 把 8 个 metrics 套到观测上。
fn apply_metrics(observation: &mut EvalObservation, metrics: [Option<JsNumber>; 8]) {
    let [
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens,
        tool_calls,
        total_ms,
        estimated_cost_usd,
    ] = metrics;
    observation.input_tokens = input_tokens;
    observation.output_tokens = output_tokens;
    observation.cache_read_tokens = cache_read_tokens;
    observation.cache_write_tokens = cache_write_tokens;
    observation.total_tokens = total_tokens;
    observation.tool_calls = tool_calls;
    observation.total_ms = total_ms;
    observation.estimated_cost_usd = estimated_cost_usd;
}

/// `persistSession`：把 `harness.run.artifacts.kianaSessionJsonl` 写到
/// `<artifact>/<variant>/sessions/<sha256(JSON.stringify([evalSet,caseId,variant,model,runNumber]))>/session.jsonl`。
///
/// 目录 `0o700`、文件 `0o600`；artifacts 里没有字符串会话时什么也不做。
///
/// 哈希由固定五个值决定，因此同一任务重跑会写入同一目录；文件以 truncate 方式打开，
/// 旧快照被覆盖而不是追加。
async fn persist_session(
    case_result: &ReportCase,
    task: &EvalTask,
    artifact_directory: &Path,
) -> Result<(), ReportError> {
    let session = case_result
        .harness
        .as_ref()
        .and_then(|harness| harness.run.as_ref())
        .and_then(|run| run.artifacts.as_ref())
        .and_then(|artifacts| artifacts.get(KIANA_SESSION_SNAPSHOT_ARTIFACT))
        .and_then(Value::as_str);
    let Some(session) = session else {
        return Ok(());
    };

    // `JSON.stringify([evalSet, caseId, variant, model, runNumber])`
    // 变体的 serde 表示与 `as_str` 相同（snake_case），因此这里的字节稳定。
    let identity = serde_json::to_string(&(
        task.eval_set.as_str(),
        task.case_id.as_str(),
        task.variant,
        task.model.as_str(),
        task.run_number,
    ))
    .expect("a tuple of strings, a variant, and an integer is always serializable");
    let hash = hex::encode(Sha256::digest(identity.as_bytes()));
    let directory = artifact_directory
        .join(task.variant.as_str())
        .join("sessions")
        .join(hash);

    // Node 的 `mkdir(directory, {recursive: true, mode: 0o700})`：错误文本是
    // `${code}: ${strerror}, mkdir '<失败的组件>'`（`fs.promises` 的异步语义）。
    crate::docker::create_dir_all_mode(&directory, 0o700)
        .map_err(|error| ReportError::Message(error.to_string()))?;

    let session_path = directory.join("session.jsonl");
    // Node 的 `fs.writeFile` 默认覆盖已有文件：create + truncate 与之一致。
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    // `writeFile` 的 `open` 失败带路径，`write` 失败不带路径（libuv 约定）。
    let mut file = options.open(&session_path).await.map_err(|error| {
        ReportError::Message(node_system_error("open", Some(&session_path), &error))
    })?;
    use tokio::io::AsyncWriteExt as _;
    file.write_all(session.as_bytes())
        .await
        .map_err(|error| ReportError::Message(node_system_error("write", None, &error)))?;
    // `tokio::fs::File` 的 `poll_write` 把实际写系统调用丢给 blocking 线程后立刻返回；
    // `flush()` 会等到该写操作完成（Node 的 `writeFile` 在 resolve 前也已落盘）。
    file.flush()
        .await
        .map_err(|error| ReportError::Message(node_system_error("write", None, &error)))?;
    Ok(())
}

/// 读取单个任务对应的 Vitest 报告并归一成观测。
///
/// TS: `report.ts#readTaskObservation`
///
/// 执行顺序与 TS 逐条一致：读取失败 → errored；断言数量/名字 → errored；
/// skipped/pending **先于** workspace 检查返回；随后检查 workspace case、harness run、
/// 持久化会话、模型一致性、metrics、run errors、分数。唯一的 `Err` 来自 `persist_session`
/// 的 I/O 失败（TS 不捕获该 rejection）。
///
/// 几个容易忽略的约束：
/// - 匹配用的 `fullName` 是 `"<eval_set> <case_id>"`（**一个空格**），不是 `plan.rs` 里
///   `"<eval set> > <case>"` 的形状；报告里的名字必须与这个拼接完全相等，否则记 errored。
/// - 会话快照在模型/metrics/分数校验**之前**写出（顺序由 TS 固定），所以模型不匹配的用例
///   仍然会留下 `session.jsonl`（`tests/report.rs` 的
///   `records_an_errored_outcome_when_the_reported_model_does_not_match_the_task`）。
/// - `skipped`/`pending` 在 workspace 检查之前返回，因此不会写 session。
/// - 返回 `Err` 时 [`crate::cli`] 用 `?` 传播到 `main`，整个进程以 1 退出。
pub async fn read_task_observation(
    task: &EvalTask,
    report_path: impl AsRef<Path>,
    artifact_directory: impl AsRef<Path>,
) -> Result<EvalObservation, ReportError> {
    let identity = EvalRunIdentity::from(task);
    let report_path = report_path.as_ref();
    let artifact_directory = artifact_directory.as_ref();

    // 阶段 1：读报告并收集全部断言；读取/解析/schema 任一失败都只产生 errored 观测。
    let Some((report, cases)) = load_report(report_path).await else {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    };

    // 阶段 2：报告必须恰好一条断言，且 fullName 等于「eval set + 空格 + case id」。
    let reported_full_name = format!("{} {}", task.eval_set, task.case_id);
    let assertions: Vec<&reader::VitestJsonAssertion> = report
        .test_results
        .iter()
        .flat_map(|file| file.assertion_results.iter())
        .collect();
    if assertions.len() != 1 {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }
    let assertion = assertions[0];
    if assertion.full_name != reported_full_name {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }
    // 阶段 3：skipped/pending 先于 workspace 检查返回（不需要 harness run，也不会写 session）。
    let status_outcome = classify_case_status(assertion.status);
    if matches!(
        status_outcome,
        Some(EvalOutcome::Skipped | EvalOutcome::Pending)
    ) {
        return Ok(EvalObservation::from_identity(
            &identity,
            status_outcome.expect("checked above"),
        ));
    }

    // 阶段 4：workspace 也必须恰好一条 case，且 fullName/status 与断言的快照一致。
    if cases.len() != 1 {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }
    let case_result = &cases[0];
    if case_result.full_name != reported_full_name {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }
    if case_result.status != assertion.status {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }
    // 阶段 5：必须有 harness run，否则没有任何分数/metrics 可用。
    let Some(run) = case_result
        .harness
        .as_ref()
        .and_then(|harness| harness.run.as_ref())
    else {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    };

    // 阶段 6：位置关键——在模型一致性/metrics/score 校验之前写出会话快照（TS 固定顺序）。
    persist_session(case_result, task, artifact_directory).await?;

    // 阶段 7：provider/model 必须都非空，且拼出的 `<provider>/<model>` 与任务一致。
    let actual_model = match (&run.usage.provider, &run.usage.model) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
            Some(format!("{provider}/{model}"))
        }
        _ => None,
    };
    if actual_model.as_deref() != Some(task.model.as_str()) {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    }

    // 阶段 8：metrics 非法时整条观测只保留身份（已收集的值全部丢弃）。
    let Ok(metrics) = collect_metrics(run) else {
        return Ok(EvalObservation::from_identity(
            &identity,
            EvalOutcome::Errored,
        ));
    };

    // 阶段 9：断言 failed 或 run errors 非空 → errored，但 metrics 已经合法，保留它们。
    if status_outcome == Some(EvalOutcome::Errored) || !run.errors.is_empty() {
        let mut observation = EvalObservation::from_identity(&identity, EvalOutcome::Errored);
        apply_metrics(&mut observation, metrics);
        return Ok(observation);
    }

    // 阶段 10：分数非法 → errored（保留 metrics）；合法时按是否存在分数决定 scored/unscored。
    let score = match validate_score(case_result.eval.as_ref().and_then(|eval| eval.avg_score)) {
        Ok(score) => score,
        Err(()) => {
            let mut observation = EvalObservation::from_identity(&identity, EvalOutcome::Errored);
            apply_metrics(&mut observation, metrics);
            return Ok(observation);
        }
    };
    let outcome = if score.is_some() {
        EvalOutcome::Scored
    } else {
        EvalOutcome::Unscored
    };
    let mut observation = EvalObservation::from_identity(&identity, outcome);
    apply_metrics(&mut observation, metrics);
    observation.score = score.map(JsNumber);
    Ok(observation)
}

/// `Promise.all([readReportWorkspace, readVitestJsonReportFile]).catch(() => undefined)` 的等价物：
/// 任何读取/解析/校验失败都返回 `None`。
///
/// 调用方把 `None` 一律记成 `errored`：报告缺失、不是 JSON、schema 不符在观测层无法区分，
/// 这与 TS 的 `.catch(() => undefined)` 相同。
async fn load_report(report_path: &Path) -> Option<(reader::VitestJsonReport, Vec<ReportCase>)> {
    let bytes = tokio::fs::read(report_path).await.ok()?;
    // Node 的 `readFile(path, "utf8")` 对非法 UTF-8 做替换式解码。
    let text = String::from_utf8_lossy(&bytes);
    let report: reader::VitestJsonReport = reader::parse_report(&text)?;
    let source = reader::display_report_source(report_path);
    let cases = reader::collect_report_workspace(&report, &source);
    Some((report, cases))
}

// ---------------------------------------------------------------------------
// 配对汇总
// ---------------------------------------------------------------------------

/// 成对指标的汇总。
///
/// TS: `report.ts#PairedMetricSummary`
///
/// 只有 pair 两侧都带该指标时才计入 `eligible_pairs`，两个均值基于同一批 pair（真正的配对
/// 比较）。`mean_delta` 是 `Number((treatment_mean - control_mean).toPrecision(15))`，
/// 与通过率 `lift` 共用 `difference` 的舍入规则；没有可比较的 pair 时所有均值字段为 `null`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedMetricSummary {
    /// 两侧都有该指标的 pair 数。
    pub eligible_pairs: usize,
    /// 对照组均值。
    pub control_mean: Option<JsNumber>,
    /// 实验组均值。
    pub treatment_mean: Option<JsNumber>,
    /// `treatment - control`（15 位有效数字）。
    pub mean_delta: Option<JsNumber>,
}

/// 比较标记。
///
/// TS: `report.ts#EvalSetComparison["flags"]`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonFlag {
    /// 通过率完全相同。
    NoLift,
    /// 实验组通过率更低。
    NegativeDelta,
    /// 对照组已经全部通过。
    ControlSaturated,
    /// 实验组已经全部通过。
    TreatmentSaturated,
    /// 同一 case/variant 的多次运行结果不一致。
    Flaky,
}

impl ComparisonFlag {
    /// TS 字符串字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            ComparisonFlag::NoLift => "no-lift",
            ComparisonFlag::NegativeDelta => "negative-delta",
            ComparisonFlag::ControlSaturated => "control-saturated",
            ComparisonFlag::TreatmentSaturated => "treatment-saturated",
            ComparisonFlag::Flaky => "flaky",
        }
    }
}

/// 单个评测集的比较结果。
///
/// TS: `report.ts#EvalSetComparison`
///
/// `total_pairs` 是该 eval set 的分组总数（含 blocked），`eligible_pairs + blocked_pairs`
/// 恒等于它。通过率与 `lift` 只在「没有任何 blocked pair 且至少有一个可用 pair」时发布
/// （`summarize_eval_observations` 里的 `publish_headline`），否则三者都是 `null`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalSetComparison {
    /// 评测集名。
    pub eval_set: String,
    /// 设计上的 pair 总数（含 blocked）。
    pub total_pairs: usize,
    /// 可用的 pair 数。
    pub eligible_pairs: usize,
    /// 被阻断的 pair 数。
    pub blocked_pairs: usize,
    /// 对照组通过率（仅 headline 发布时有值）。
    pub control_pass_rate: Option<JsNumber>,
    /// 实验组通过率（仅 headline 发布时有值）。
    pub treatment_pass_rate: Option<JsNumber>,
    /// 通过率差（`treatment - control`）。
    pub lift: Option<JsNumber>,
    /// 标记列表。
    pub flags: Vec<ComparisonFlag>,
    /// 总 token 的成对汇总。
    pub total_tokens: PairedMetricSummary,
    /// 工具调用次数的成对汇总。
    pub tool_calls: PairedMetricSummary,
    /// 总耗时的成对汇总。
    pub total_ms: PairedMetricSummary,
    /// 估算成本的成对汇总。
    pub estimated_cost_usd: PairedMetricSummary,
}

/// 被阻断的 pair（没有 `variant` 字段）。
///
/// TS: `report.ts#BlockedPair`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedPair {
    /// 评测集名。
    pub eval_set: String,
    /// 用例名。
    pub case_id: String,
    /// `<provider>/<model>`。
    pub model: String,
    /// 重复序号。
    pub run_number: u64,
    /// 阻断原因，每条形如 `<variant>: <描述>`；按 `without_docs` 再 `with_docs` 的顺序累积，
    /// 只有渲染文本时才用 `"; "` 连接（见 [`format_eval_comparison_report`]）。
    pub reasons: Vec<String>,
}

/// 单个运营指标在某个变体上的总量。
///
/// TS: `report.ts#OperationalMetricTotal`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalMetricTotal {
    /// 有该指标的运行数。
    pub available_runs: usize,
    /// 总和（没有数据时为 `null`）。
    pub total: Option<JsNumber>,
}

/// 单个变体的运营总量。
///
/// TS: `report.ts#VariantTotals`
///
/// `runs` 是该变体的全部观测数（含 `errored`/`skipped`/`pending`）；每个指标的
/// `available_runs` 只统计该指标存在的观测，因此可能小于 `runs`。没有数据时 `total` 为
/// `null`，这与「总和恰好是 0」是不同状态（`tests/comparison.rs` 有专门断言）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantTotals {
    /// 变体。
    pub variant: DocumentationVariant,
    /// 运行数。
    pub runs: usize,
    /// 输入 token。
    pub input_tokens: OperationalMetricTotal,
    /// 输出 token。
    pub output_tokens: OperationalMetricTotal,
    /// 缓存读 token。
    pub cache_read_tokens: OperationalMetricTotal,
    /// 缓存写 token。
    pub cache_write_tokens: OperationalMetricTotal,
    /// 总 token。
    pub total_tokens: OperationalMetricTotal,
    /// 工具调用次数。
    pub tool_calls: OperationalMetricTotal,
    /// 总耗时。
    pub total_ms: OperationalMetricTotal,
    /// 估算成本。
    pub estimated_cost_usd: OperationalMetricTotal,
}

/// `report.json` 的完整内容。
///
/// TS: `report.ts#EvalComparisonReport`
///
/// 顺序契约：`comparisons` 按 eval set 的 `localeCompare` 升序；`operational_totals` 恒为
/// 两项且固定先 `without_docs` 后 `with_docs`。`schema_version` 恒为 3，`tests/comparison.rs`
/// 内联的逐字 JSON 断言依赖这些字段与顺序。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalComparisonReport {
    /// schema 版本（恒为 3）。
    pub schema_version: u32,
    /// 协议文本的 sha256 十六进制。
    pub protocol_digest: String,
    /// 对照组（恒为 `without_docs`）。
    pub control: DocumentationVariant,
    /// 实验组（恒为 `with_docs`）。
    pub treatment: DocumentationVariant,
    /// 每个评测集的比较结果。
    pub comparisons: Vec<EvalSetComparison>,
    /// 被阻断的 pair。
    pub blocked_pairs: Vec<BlockedPair>,
    /// 两个变体的运营总量（顺序固定）。
    pub operational_totals: Vec<VariantTotals>,
}

/// 一个可比较的 pair（两侧都必须是 `scored`）。
struct Pair {
    control: EvalObservation,
    treatment: EvalObservation,
}

impl Pair {
    fn score(observation: &EvalObservation) -> f64 {
        // TS 里 `score` 在 pair 中一定存在；`undefined` 参与 `>= 1` 时为 `false`。
        observation.score.map(|score| score.0).unwrap_or(f64::NAN)
    }
}

/// 分组后的设计/观测集合。
struct PairGroup {
    eval_set: String,
    case_id: String,
    model: String,
    run_number: u64,
    expected: [usize; 2],
    observations: [Vec<EvalObservation>; 2],
}

impl PairGroup {
    fn new(eval_set: &str, case_id: &str, model: &str, run_number: u64) -> Self {
        PairGroup {
            eval_set: eval_set.to_string(),
            case_id: case_id.to_string(),
            model: model.to_string(),
            run_number,
            expected: [0, 0],
            observations: [Vec::new(), Vec::new()],
        }
    }
}

/// 变体的数组下标（`DOCUMENTATION_VARIANTS` 的顺序）。
fn variant_index(variant: DocumentationVariant) -> usize {
    match variant {
        DocumentationVariant::WithoutDocs => 0,
        DocumentationVariant::WithDocs => 1,
    }
}

// ---------------------------------------------------------------------------
// localeCompare（ICU en-US，含 ASCII 控制字符）
// ---------------------------------------------------------------------------

/// ICU en-US 里完全 ignorable 的 ASCII 控制字符：`U+0000..U+0008`、`U+000E..U+001F`、
/// `U+007F`。
///
/// Node 26 实测：`"\u{0}".localeCompare("")`、`"a\u{0}b".localeCompare("ab")` 都是 0。
/// `kiana_ai::model_catalog::locale_compare` 把它们按 `1000 + 码点` 处理，因此含控制字符的
/// case id 会排在全部可打印 ASCII 之后（与 ICU 相反）；本模块的报告排序用这里的比较器。
fn is_collation_ignorable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}

/// 空白控制符的一次权重：TAB < LF < VT < FF < CR < 空格（空格权重 5，下表从 `_`（权重 6）
/// 开始）。
fn whitespace_control_primary(c: char) -> Option<u32> {
    match c {
        '\t' => Some(0),
        '\n' => Some(1),
        '\u{b}' => Some(2),
        '\u{c}' => Some(3),
        '\r' => Some(4),
        _ => None,
    }
}

/// ICU en-US 的可打印 ASCII 一次权重顺序（空格之后的标点/数字/小写字母，Node 26 实测）。
///
/// 与 `kiana_ai::model_catalog` 的 `ASCII_COLLATION_ORDER` 相同，但去掉大写字母——大写只通过
/// 三次权重（`is_uppercase`）区分。
const ICU_ASCII_PRIMARY_ORDER: &str =
    "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

/// 单个字符的 `(一次, 二次, 三次)` 排序键；`None` 表示 ignorable（不产生排序元素）。
fn icu_collation_weights(c: char) -> Option<(u32, u32, u32)> {
    if is_collation_ignorable(c) {
        return None;
    }
    if let Some(primary) = whitespace_control_primary(c) {
        return Some((primary, 0, 0));
    }
    if c == ' ' {
        return Some((5, 0, 0));
    }
    if let Some(index) = ICU_ASCII_PRIMARY_ORDER.find(c.to_ascii_lowercase()) {
        // 表中的第一个字符（`_`）紧跟在空格之后：空格权重 5，`_` 权重 6。
        return Some((6 + index as u32, 0, u32::from(c.is_ascii_uppercase())));
    }
    // 非 ASCII：与 `kiana_ai::model_catalog` 的回退一致（排在全部 ASCII 之后）。含控制字符的
    // 字符串才会走到这里；纯非 ASCII 输入仍由 `locale_compare` 处理。
    let lower = c.to_lowercase().next().unwrap_or(c);
    Some((1000 + lower as u32, 0, u32::from(c.is_uppercase())))
}

/// 带控制字符 ICU 语义的 `localeCompare`。
///
/// 两侧都不含 ASCII 控制字符时直接走 [`locale_compare`]（保留其对拉丁扩展字母的二次权重
/// 近似）；否则按 ICU 的三层比较：一次权重序列 → 二次 → 三次。
///
/// 控制字符路径的期望值来自 Node 26：`tests/fixtures/locale_pairs.json` 的 500 对随机
/// ASCII（含全部控制字符，由本文件单元测试
/// `locale_comparison_matches_icu_on_random_ascii_pairs` 使用）以及本文件单元测试里的
/// 手写矩阵。
fn eval_locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    if !left.contains(|c: char| c.is_ascii_control())
        && !right.contains(|c: char| c.is_ascii_control())
    {
        return locale_compare(left, right);
    }
    let left_keys: Vec<(u32, u32, u32)> = left.chars().filter_map(icu_collation_weights).collect();
    let right_keys: Vec<(u32, u32, u32)> =
        right.chars().filter_map(icu_collation_weights).collect();
    let shared = left_keys.len().min(right_keys.len());
    for index in 0..shared {
        let ordering = left_keys[index].0.cmp(&right_keys[index].0);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    if left_keys.len() != right_keys.len() {
        return left_keys.len().cmp(&right_keys.len());
    }
    for index in 0..shared {
        let ordering = left_keys[index].1.cmp(&right_keys[index].1);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    for index in 0..shared {
        let ordering = left_keys[index].2.cmp(&right_keys[index].2);
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

/// `JSON.stringify([evalSet, caseId, model, runNumber])` 的等价分组键。
type PairKey = (String, String, String, u64);

fn pair_key(eval_set: &str, case_id: &str, model: &str, run_number: u64) -> PairKey {
    (
        eval_set.to_string(),
        case_id.to_string(),
        model.to_string(),
        run_number,
    )
}

/// TS: `report.ts#groupPairs`
///
/// 先遍历 `expected_runs`、再遍历 `observations`，用 `IndexMap` 保留首次出现顺序；最后按
/// `(eval_set, case_id, model)` 的 ICU `localeCompare` + `run_number` 数值升序排序。
/// `sort_by` 是稳定排序，比较相等的分组保持插入顺序（`tests/comparison.rs` 的
/// `ignorable_control_characters_compare_equal` 依赖这一点）。
fn group_pairs(
    expected_runs: &[ExpectedEvalRun],
    observations: &[EvalObservation],
) -> Vec<PairGroup> {
    let mut groups: IndexMap<PairKey, PairGroup> = IndexMap::new();
    for expected in expected_runs {
        let key = pair_key(
            &expected.eval_set,
            &expected.case_id,
            &expected.model,
            expected.run_number,
        );
        let group = groups.entry(key).or_insert_with(|| {
            PairGroup::new(
                &expected.eval_set,
                &expected.case_id,
                &expected.model,
                expected.run_number,
            )
        });
        group.expected[variant_index(expected.variant)] += 1;
    }
    for observation in observations {
        let key = pair_key(
            &observation.eval_set,
            &observation.case_id,
            &observation.model,
            observation.run_number,
        );
        let group = groups.entry(key).or_insert_with(|| {
            PairGroup::new(
                &observation.eval_set,
                &observation.case_id,
                &observation.model,
                observation.run_number,
            )
        });
        group.observations[variant_index(observation.variant)].push(observation.clone());
    }
    let mut groups: Vec<PairGroup> = groups.into_values().collect();
    groups.sort_by(|left, right| {
        eval_locale_compare(&left.eval_set, &right.eval_set)
            .then_with(|| eval_locale_compare(&left.case_id, &right.case_id))
            .then_with(|| eval_locale_compare(&left.model, &right.model))
            .then_with(|| left.run_number.cmp(&right.run_number))
    });
    groups
}

enum ResolvedPair {
    Pair(Box<Pair>),
    Blocked(BlockedPair),
}

/// TS: `report.ts#resolvePair`
///
/// 原因按 `DOCUMENTATION_VARIANTS` 的顺序逐变体累积（先 without_docs 后 with_docs），
/// 所以 `BlockedPair.reasons` 的文本顺序固定；同一变体内三类检查互不 else，会一次报全。
fn resolve_pair(group: &PairGroup) -> ResolvedPair {
    let mut reasons: Vec<String> = Vec::new();
    for variant in DOCUMENTATION_VARIANTS {
        let index = variant_index(variant);
        let expected = group.expected[index];
        let observed = &group.observations[index];
        if expected != 1 {
            reasons.push(format!(
                "{variant}: design expected 1 run, found {expected}"
            ));
        }
        if observed.len() != expected {
            let plural = if expected == 1 { "" } else { "s" };
            reasons.push(format!(
                "{variant}: expected {expected} observation{plural}, found {}",
                observed.len()
            ));
        }
        if expected == 1 && observed.len() == 1 && observed[0].outcome != EvalOutcome::Scored {
            reasons.push(format!("{variant}: {}", observed[0].outcome));
        }
    }
    if !reasons.is_empty() {
        return ResolvedPair::Blocked(BlockedPair {
            eval_set: group.eval_set.clone(),
            case_id: group.case_id.clone(),
            model: group.model.clone(),
            run_number: group.run_number,
            reasons,
        });
    }
    ResolvedPair::Pair(Box::new(Pair {
        control: group.observations[0][0].clone(),
        treatment: group.observations[1][0].clone(),
    }))
}

/// TS: `report.ts#mean`
///
/// 空切片返回 `None` 而不是 0：缺数据与真实均值 0 在报告里是不同状态。
fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// `Number((treatment - control).toPrecision(15))`。
///
/// `toPrecision` 用**精确的十进制值**（f64 的二进制展开）四舍五入到 15 位有效数字，
/// 半数远离零；随后 `Number()` 按最近偶数把该十进制数解析回 f64。这里用
/// [`BigUint`] 复刻精确展开，再用 Rust 的正确舍入十进制解析器还原。
fn round_significant(value: f64, precision: u32) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let negative = value < 0.0;
    let magnitude = value.abs();

    // x = numerator / 10^scale 的精确形式（与 kiana-json 的 js_num 相同的二进制分解）。
    let bits = magnitude.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent) = if raw_exponent == 0 {
        (fraction, -1074i32)
    } else {
        (fraction | (1u64 << 52), raw_exponent - 1075)
    };
    let (numerator, scale) = if exponent >= 0 {
        (BigUint::from(mantissa) << (exponent as usize), 0u32)
    } else {
        let power = (-exponent) as u32;
        (
            BigUint::from(mantissa) * BigUint::from(5u32).pow(power),
            power,
        )
    };

    // 精确值写成十进制整数串（分母是 10^scale）：字符串长度就是有效数字位数。
    let digits = numerator.to_str_radix(10);
    let digit_count = digits.len() as i32;
    let precision = precision as i32;
    if digit_count <= precision {
        // 精确值本来就不超过 15 位有效数字，`Number()` 会还原成同一个 f64。
        return value;
    }

    let drop = (digit_count - precision) as u32;
    let divisor = BigUint::from(10u32).pow(drop);
    let quotient = &numerator / &divisor;
    let remainder = &numerator % &divisor;
    // 比较 2*remainder 与 divisor：正好一半时进位（半数远离零，与 `toPrecision` 一致）。
    let rounded = if &remainder * 2u32 >= divisor {
        quotient + 1u32
    } else {
        quotient
    };

    // rounded * 10^drop / 10^scale
    let exponent10 = drop as i32 - scale as i32;
    let text = format!("{}e{}", rounded.to_str_radix(10), exponent10);
    let parsed: f64 = text.parse().unwrap_or(f64::INFINITY);
    if negative { -parsed } else { parsed }
}

/// TS: `report.ts#difference`
fn difference(treatment: f64, control: f64) -> f64 {
    let delta = treatment - control;
    // 零差直接返回 +0：`toPrecision` 对零的文本解析回来就是 +0，这里提前归一，使结果
    // 与 TS 的 `Number((x).toPrecision(15))` 对零的输出（+0）一致，避免 -0 进入 JSON/文本输出。
    if delta == 0.0 {
        return 0.0;
    }
    round_significant(delta, 15)
}

/// TS: `report.ts#summarizeMetric`
///
/// 逐 pair 取两侧的指标值；任一侧缺失就跳过该 pair（不把缺失当 0）。两个均值基于同一批
/// pair，因此要么都有值，要么都是 `None`。
fn summarize_metric(
    pairs: &[Pair],
    select: impl Fn(&EvalObservation) -> Option<f64>,
) -> PairedMetricSummary {
    let mut control: Vec<f64> = Vec::new();
    let mut treatment: Vec<f64> = Vec::new();
    for pair in pairs {
        let (Some(control_value), Some(treatment_value)) =
            (select(&pair.control), select(&pair.treatment))
        else {
            continue;
        };
        control.push(control_value);
        treatment.push(treatment_value);
    }
    let control_mean = mean(&control);
    let treatment_mean = mean(&treatment);
    PairedMetricSummary {
        eligible_pairs: control.len(),
        control_mean: control_mean.map(JsNumber),
        treatment_mean: treatment_mean.map(JsNumber),
        mean_delta: match (control_mean, treatment_mean) {
            (Some(control), Some(treatment)) => Some(JsNumber(difference(treatment, control))),
            _ => None,
        },
    }
}

/// TS: `report.ts#operationalTotal`
///
/// 直接对原始 `f64` 求和：总和可能溢出成 `±Infinity`（两个 `1e308` 相加），此时 JSON 侧
/// [`JsNumber`] 会写 `null`，而文本侧走 `String(number)` 写成 `Infinity`
/// （`tests/comparison.rs` 的 `operational_totals_use_string_number_for_infinite_sums`）。
fn operational_total(
    runs: &[&EvalObservation],
    select: impl Fn(&EvalObservation) -> Option<f64>,
) -> OperationalMetricTotal {
    let values: Vec<f64> = runs.iter().filter_map(|run| select(run)).collect();
    OperationalMetricTotal {
        available_runs: values.len(),
        total: if values.is_empty() {
            None
        } else {
            Some(JsNumber(values.iter().sum()))
        },
    }
}

/// TS: `report.ts#variantTotals`
///
/// `runs` 统计该变体的全部观测（不看 outcome），这样文本里的 `N runs` 与
/// `available_runs` 才能形成覆盖率对比；各指标独立累加。
fn variant_totals(
    observations: &[EvalObservation],
    variant: DocumentationVariant,
) -> VariantTotals {
    let runs: Vec<&EvalObservation> = observations
        .iter()
        .filter(|observation| observation.variant == variant)
        .collect();
    VariantTotals {
        variant,
        runs: runs.len(),
        input_tokens: operational_total(&runs, |observation| {
            observation.input_tokens.map(|value| value.0)
        }),
        output_tokens: operational_total(&runs, |observation| {
            observation.output_tokens.map(|value| value.0)
        }),
        cache_read_tokens: operational_total(&runs, |observation| {
            observation.cache_read_tokens.map(|value| value.0)
        }),
        cache_write_tokens: operational_total(&runs, |observation| {
            observation.cache_write_tokens.map(|value| value.0)
        }),
        total_tokens: operational_total(&runs, |observation| {
            observation.total_tokens.map(|value| value.0)
        }),
        tool_calls: operational_total(&runs, |observation| {
            observation.tool_calls.map(|value| value.0)
        }),
        total_ms: operational_total(&runs, |observation| {
            observation.total_ms.map(|value| value.0)
        }),
        estimated_cost_usd: operational_total(&runs, |observation| {
            observation.estimated_cost_usd.map(|value| value.0)
        }),
    }
}

/// TS: `report.ts#comparisonFlags`
///
/// 通过率标记只有两侧都有通过率时才计算；`Flaky` 单独判断：按 `(case_id, variant)` 聚合
/// 所有 pair 的「是否满分」，同一键既有 `true` 又有 `false` 即标记。注意这个键不含
/// `eval_set`/`model`，同名 case 会共享一组结果。
fn comparison_flags(
    pairs: &[Pair],
    control_pass_rate: Option<f64>,
    treatment_pass_rate: Option<f64>,
) -> Vec<ComparisonFlag> {
    let mut flags = Vec::new();
    if let (Some(control), Some(treatment)) = (control_pass_rate, treatment_pass_rate) {
        if control == treatment {
            flags.push(ComparisonFlag::NoLift);
        }
        if treatment < control {
            flags.push(ComparisonFlag::NegativeDelta);
        }
        if control == 1.0 {
            flags.push(ComparisonFlag::ControlSaturated);
        }
        if treatment == 1.0 {
            flags.push(ComparisonFlag::TreatmentSaturated);
        }
    }

    let mut outcomes: IndexMap<(String, DocumentationVariant), HashSet<bool>> = IndexMap::new();
    for pair in pairs {
        outcomes
            .entry((pair.control.case_id.clone(), pair.control.variant))
            .or_default()
            .insert(Pair::score(&pair.control) >= 1.0);
        outcomes
            .entry((pair.treatment.case_id.clone(), pair.treatment.variant))
            .or_default()
            .insert(Pair::score(&pair.treatment) >= 1.0);
    }
    if outcomes.values().any(|values| values.len() > 1) {
        flags.push(ComparisonFlag::Flaky);
    }
    flags
}

/// 把设计运行与观测汇总成比较报告。
///
/// TS: `report.ts#summarizeEvalObservations`
///
/// 输入：
/// - `protocol_digest`：CLI 对 protocol 对象紧凑序列化文本算出的 sha256，原样写入报告；
/// - `expected_runs`：每个设计任务一条（CLI 由 `EvalTask` 转换而来）；
/// - `observations`：每个任务一条；任务没有产出报告时 CLI 已用
///   [`errored_observation`] 补位，因此这里通常不缺项。
///
/// 分组与判定：
/// - 组键是 `(eval_set, case_id, model, run_number)`，**不含 variant**；
/// - 每组要求两个变体各「设计 1 条、观测 1 条且 outcome 为 `scored`」，任一不满足就把原因
///   写入 [`BlockedPair`]，整组不参与成对统计；
/// - 函数不返回 `Result`：输入不完整属于正常输出的一部分（blocked pair），不是调用错误。
///
/// 输出顺序：`comparisons` 按 `eval_set` 的 `localeCompare` 升序；`blocked_pairs` 跟随分组
/// 排序（eval set → case id → model → run number）；`operational_totals` 恒为
/// `[without_docs, with_docs]` 两项，统计**全部**观测（含 errored/skipped），与 blocked 无关。
pub fn summarize_eval_observations(
    protocol_digest: &str,
    expected_runs: &[ExpectedEvalRun],
    observations: &[EvalObservation],
) -> EvalComparisonReport {
    let groups = group_pairs(expected_runs, observations);
    let mut blocked_pairs: Vec<BlockedPair> = Vec::new();
    let mut pairs_by_eval_set: IndexMap<String, Vec<Pair>> = IndexMap::new();
    let mut totals_by_eval_set: IndexMap<String, usize> = IndexMap::new();
    // 阶段 1：逐组判定。`totals_by_eval_set` 统计的是设计 pair 数（含 blocked），
    // blocked 数由它减去可用 pair 数得到。
    for group in &groups {
        *totals_by_eval_set
            .entry(group.eval_set.clone())
            .or_insert(0) += 1;
        match resolve_pair(group) {
            ResolvedPair::Blocked(blocked) => blocked_pairs.push(blocked),
            ResolvedPair::Pair(pair) => {
                pairs_by_eval_set
                    .entry(group.eval_set.clone())
                    .or_default()
                    .push(*pair);
            }
        }
    }

    // 阶段 2：eval set 列表按 ICU 顺序排序；`comparisons` 的最终顺序由此固定。
    let mut eval_sets: Vec<(String, usize)> = totals_by_eval_set.into_iter().collect();
    eval_sets.sort_by(|(left, _), (right, _)| eval_locale_compare(left, right));
    let comparisons: Vec<EvalSetComparison> = eval_sets
        .into_iter()
        .map(|(eval_set, total_pairs)| {
            let pairs = pairs_by_eval_set
                .get(&eval_set)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let blocked_pair_count = total_pairs - pairs.len();
            // 只有该 eval set 完全没有 blocked pair 且至少有一个可用 pair 时才发布通过率；
            // 否则通过率与 lift 都是 `None`（JSON `null`），文本显示 withheld/unavailable。
            let publish_headline = blocked_pair_count == 0 && !pairs.is_empty();
            let control_pass_rate = if publish_headline {
                Some(
                    pairs
                        .iter()
                        .filter(|pair| Pair::score(&pair.control) >= 1.0)
                        .count() as f64
                        / pairs.len() as f64,
                )
            } else {
                None
            };
            let treatment_pass_rate = if publish_headline {
                Some(
                    pairs
                        .iter()
                        .filter(|pair| Pair::score(&pair.treatment) >= 1.0)
                        .count() as f64
                        / pairs.len() as f64,
                )
            } else {
                None
            };
            EvalSetComparison {
                eval_set,
                total_pairs,
                eligible_pairs: pairs.len(),
                blocked_pairs: blocked_pair_count,
                control_pass_rate: control_pass_rate.map(JsNumber),
                treatment_pass_rate: treatment_pass_rate.map(JsNumber),
                lift: match (control_pass_rate, treatment_pass_rate) {
                    (Some(control), Some(treatment)) => {
                        Some(JsNumber(difference(treatment, control)))
                    }
                    _ => None,
                },
                flags: comparison_flags(pairs, control_pass_rate, treatment_pass_rate),
                total_tokens: summarize_metric(pairs, |observation| {
                    observation.total_tokens.map(|value| value.0)
                }),
                tool_calls: summarize_metric(pairs, |observation| {
                    observation.tool_calls.map(|value| value.0)
                }),
                total_ms: summarize_metric(pairs, |observation| {
                    observation.total_ms.map(|value| value.0)
                }),
                estimated_cost_usd: summarize_metric(pairs, |observation| {
                    observation.estimated_cost_usd.map(|value| value.0)
                }),
            }
        })
        .collect();

    EvalComparisonReport {
        schema_version: 3,
        protocol_digest: protocol_digest.to_string(),
        control: DocumentationVariant::WithoutDocs,
        treatment: DocumentationVariant::WithDocs,
        comparisons,
        blocked_pairs,
        operational_totals: vec![
            variant_totals(observations, DocumentationVariant::WithoutDocs),
            variant_totals(observations, DocumentationVariant::WithDocs),
        ],
    }
}

// ---------------------------------------------------------------------------
// 文本格式化
// ---------------------------------------------------------------------------

/// Node `getColorDepth` 的颜色档位（`internal/tty`）。
const COLORS_2: u8 = 1;
const COLORS_16: u8 = 4;
const COLORS_256: u8 = 8;
const COLORS_16M: u8 = 24;

/// `styleText("bold", text)`。
///
/// Node 的 `util.styleText` 用 `shouldColorize(process.stdout)`（`internal/util/colors`）决定
/// 是否着色：
/// - `FORCE_COLOR` 已设置 → `getColorDepth() > 2`，与 stdout 是否是 TTY 无关
///   （`""`/`"1"`/`"true"` → 16 色、`"2"` → 256 色、`"3"` → 24 位色，其余 → 2 色）；
/// - 否则 → stdout 是 TTY 且 `getColorDepth() > 2`。
///
/// 这比 spec §3.5 的简化规则更贴近 TS：`FORCE_COLOR=1` 且 stdout 被重定向时 Node 仍会写
/// ANSI，而 TTY 下的 `FORCE_COLOR=4`、`CI=true`（无 CI 颜色环境）、`TERM=dumb` 不会。
fn style_bold(text: &str) -> String {
    let colorize = should_colorize(&process_env(), std::io::stdout().is_terminal());
    style_bold_with(text, colorize)
}

fn style_bold_with(text: &str, colorize: bool) -> String {
    if colorize {
        format!("\u{1b}[1m{text}\u{1b}[22m")
    } else {
        text.to_string()
    }
}

/// Node `process.env` 的字符串快照（非 UTF-8 值按 lossy 处理）。
///
/// 每次调用都重新读取进程环境，不缓存：`style_bold` 的行为跟随调用时的环境。测试则直接
/// 调用 `should_colorize` 并传入构造好的 map，避免依赖真实进程环境。
fn process_env() -> HashMap<String, String> {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// `internal/util/colors` 的 `shouldColorize(stream)`。
fn should_colorize(env: &HashMap<String, String>, is_tty: bool) -> bool {
    if env.contains_key("FORCE_COLOR") {
        return get_color_depth(env) > 2;
    }
    is_tty && get_color_depth(env) > 2
}

/// Node `internal/tty` 的 `getColorDepth(env)`。
fn get_color_depth(env: &HashMap<String, String>) -> u8 {
    if let Some(force_color) = env.get("FORCE_COLOR") {
        return match force_color.as_str() {
            "" | "1" | "true" => COLORS_16,
            "2" => COLORS_256,
            "3" => COLORS_16M,
            _ => COLORS_2,
        };
    }

    if env
        .get("NODE_DISABLE_COLORS")
        .is_some_and(|value| !value.is_empty())
        || env.get("NO_COLOR").is_some_and(|value| !value.is_empty())
        || env.get("TERM").is_some_and(|term| term == "dumb")
    {
        return COLORS_2;
    }

    get_color_depth_for_platform(env)
}

/// `getColorDepth` 的平台分支。
///
/// Windows 上 Node 用 `os.release()` 的 build 号在 16/256/24 位色之间选择，但三个分支都
/// `> 2`，所以这里直接返回 24 位色，`shouldColorize` 的判定与 Node 一致。
#[cfg(windows)]
fn get_color_depth_for_platform(_env: &HashMap<String, String>) -> u8 {
    COLORS_16M
}

#[cfg(not(windows))]
fn get_color_depth_for_platform(env: &HashMap<String, String>) -> u8 {
    if env.get("TMUX").is_some_and(|value| !value.is_empty()) {
        return COLORS_16M;
    }
    // Azure DevOps
    if env.contains_key("TF_BUILD") && env.contains_key("AGENT_NAME") {
        return COLORS_16;
    }
    if env.contains_key("CI") {
        for (name, colors) in [
            ("APPVEYOR", COLORS_256),
            ("BUILDKITE", COLORS_256),
            ("CIRCLECI", COLORS_16M),
            ("DRONE", COLORS_256),
            ("GITEA_ACTIONS", COLORS_16M),
            ("GITHUB_ACTIONS", COLORS_16M),
            ("GITLAB_CI", COLORS_256),
            ("TRAVIS", COLORS_256),
        ] {
            if env.contains_key(name) {
                return colors;
            }
        }
        if env.get("CI_NAME").is_some_and(|name| name == "codeship") {
            return COLORS_256;
        }
        return COLORS_2;
    }
    if let Some(version) = env.get("TEAMCITY_VERSION") {
        return if teamcity_supports_color(version) {
            COLORS_16
        } else {
            COLORS_2
        };
    }
    match env.get("TERM_PROGRAM").map(String::as_str) {
        Some("iTerm.app") => {
            return match env.get("TERM_PROGRAM_VERSION") {
                Some(version)
                    if !version.is_empty() && !starts_with_legacy_iterm_version(version) =>
                {
                    COLORS_16M
                }
                _ => COLORS_256,
            };
        }
        Some("HyperTerm" | "MacTerm") => return COLORS_16M,
        Some("Apple_Terminal") => return COLORS_256,
        _ => {}
    }
    if let Some(colorterm) = env.get("COLORTERM")
        && (colorterm == "truecolor" || colorterm == "24bit")
    {
        return COLORS_16M;
    }
    if let Some(term) = env.get("TERM") {
        if term.contains("truecolor") {
            return COLORS_16M;
        }
        if term.starts_with("xterm-256") {
            return COLORS_256;
        }
        let term_env = term.to_lowercase();
        if let Some(colors) = named_term_color(&term_env) {
            return colors;
        }
        if term_pattern_matches(&term_env) {
            return COLORS_16;
        }
    }
    if env.get("COLORTERM").is_some_and(|value| !value.is_empty()) {
        return COLORS_16;
    }
    COLORS_2
}

/// `internal/tty` 的 `TERM_ENVS` 表。
fn named_term_color(term: &str) -> Option<u8> {
    Some(match term {
        "eterm" | "cons25" | "console" | "cygwin" | "dtterm" | "gnome" | "hurd" | "jfbterm"
        | "konsole" | "kterm" | "mlterm" | "putty" | "st" => COLORS_16,
        "mosh" | "rxvt-unicode-24bit" | "terminator" | "xterm-kitty" => COLORS_16M,
        _ => return None,
    })
}

/// `internal/tty` 的 `TERM_ENVS_REG_EXP`（对已小写的 `TERM` 匹配）。
fn term_pattern_matches(term: &str) -> bool {
    if ["ansi", "color", "linux", "direct"]
        .iter()
        .any(|needle| term.contains(needle))
    {
        return true;
    }
    if let Some(rest) = term.strip_prefix("con") {
        let bytes = rest.as_bytes();
        let mut index = 0;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'x') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
            return true;
        }
    }
    ["rxvt", "screen", "xterm", "vt100", "vt220"]
        .iter()
        .any(|prefix| term.starts_with(prefix))
}

/// `/^[0-2]\./`：iTerm 2 及更早版本按 256 色处理。
fn starts_with_legacy_iterm_version(version: &str) -> bool {
    let mut characters = version.chars();
    matches!(characters.next(), Some('0' | '1' | '2')) && characters.next() == Some('.')
}

/// `/^(9\.(0*[1-9]\d*)\.|\d{2,}\.)/`：TeamCity 9.1+ 支持 16 色。
fn teamcity_supports_color(version: &str) -> bool {
    let bytes = version.as_bytes();
    let leading_digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if leading_digits >= 2 && bytes.get(leading_digits) == Some(&b'.') {
        return true;
    }
    if let Some(rest) = version.strip_prefix("9.") {
        let rest = rest.as_bytes();
        let mut index = 0;
        while index < rest.len() && rest[index] == b'0' {
            index += 1;
        }
        if index < rest.len() && (b'1'..=b'9').contains(&rest[index]) {
            index += 1;
            while index < rest.len() && rest[index].is_ascii_digit() {
                index += 1;
            }
            return rest.get(index) == Some(&b'.');
        }
    }
    false
}

/// TS: `report.ts#percentage`
///
/// 缺失写 `unavailable`；有值时先乘 100 再 `toFixed(1)`，即 V8 的半数远离零。
fn percentage(value: Option<JsNumber>) -> String {
    match value {
        None => "unavailable".to_string(),
        Some(value) => format!("{}%", js_to_fixed(value.0 * 100.0, 1)),
    }
}

/// TS: `report.ts#signed`
///
/// 非负数（含 `+0`）补 `+` 前缀；负数不加前缀，`js_to_fixed` 自己会输出 `-`。
fn signed(value: f64, digits: u32) -> String {
    let sign = if value >= 0.0 { "+" } else { "" };
    format!("{sign}{}", js_to_fixed(value, digits))
}

/// TS: `report.ts#pairedMetric`
///
/// 三个均值缺一就整行写 `unavailable`。`N pairs` 不做单复数处理（1 也写 `pairs`），
/// 这是黄金文件的固定文本。
fn paired_metric(label: &str, metric: &PairedMetricSummary, unit: &str) -> String {
    match (
        metric.mean_delta,
        metric.control_mean,
        metric.treatment_mean,
    ) {
        (Some(delta), Some(control), Some(treatment)) => format!(
            "    {:>10}  {}{unit} (with {}{unit}, without {}{unit}, {} pairs)",
            label,
            signed(delta.0, 1),
            js_to_fixed(treatment.0, 1),
            js_to_fixed(control.0, 1),
            metric.eligible_pairs
        ),
        _ => format!("    {:>10}  unavailable", label),
    }
}

/// TS: `report.ts#operationalMetric`
///
/// 没有任何数据时写 `unavailable (0/<runs> measured)`；部分缺失时在总量后补
/// `(<available>/<runs> measured)`；全覆盖则不显示后缀。
fn operational_metric(
    metric: &OperationalMetricTotal,
    runs: usize,
    format: impl Fn(f64) -> String,
) -> String {
    match metric.total {
        None => format!("unavailable (0/{runs} measured)"),
        Some(total) => {
            let coverage = if metric.available_runs == runs {
                String::new()
            } else {
                format!(" ({}/{runs} measured)", metric.available_runs)
            };
            format!("{}{coverage}", format(total.0))
        }
    }
}

/// 渲染比较报告文本（`report.txt` 的内容，不含末尾换行）。
///
/// TS: `report.ts#formatEvalComparisonReport`
///
/// 行之间用 `"\n"` 连接，**没有**末尾换行；调用方负责补 `"\n"`。
///
/// 结构固定为四段：标题（可能带 ANSI 加粗，取决于 stdout 的 `should_colorize`）、每个
/// eval set 的缩进块、`Operational totals`、可选的 `Blocked pairs`。列宽与小数位（百分比
/// 1 位、成本 4 位、耗时 2 位）是黄金文件的一部分，改动会破坏 `tests/comparison.rs` 的逐字
/// 断言；`comparisons` 为空时直接返回空串。
pub fn format_eval_comparison_report(report: &EvalComparisonReport) -> String {
    if report.comparisons.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![style_bold("Documentation Eval Comparisons")];
    for comparison in &report.comparisons {
        lines.push(format!("  {}", comparison.eval_set));
        lines.push(format!(
            "         Pairs  {}/{} eligible",
            comparison.eligible_pairs, comparison.total_pairs
        ));
        match comparison.lift {
            None => {
                if comparison.blocked_pairs > 0 {
                    lines.push("     Pass rate  withheld because pairs are blocked".to_string());
                } else {
                    lines.push("     Pass rate  unavailable".to_string());
                }
            }
            Some(lift) => {
                lines.push(format!(
                    "     Pass rate  {} pp (with {}, without {})",
                    signed(lift.0 * 100.0, 1),
                    percentage(comparison.treatment_pass_rate),
                    percentage(comparison.control_pass_rate)
                ));
            }
        }
        if !comparison.flags.is_empty() {
            let names: Vec<&str> = comparison.flags.iter().map(|flag| flag.as_str()).collect();
            lines.push(format!("         Flags  {}", names.join(", ")));
        }
        lines.push(paired_metric("Tokens", &comparison.total_tokens, ""));
        lines.push(paired_metric("Tools", &comparison.tool_calls, ""));
        lines.push(paired_metric("Latency", &comparison.total_ms, "ms"));
        let cost = &comparison.estimated_cost_usd;
        match (cost.mean_delta, cost.control_mean, cost.treatment_mean) {
            (Some(delta), Some(control), Some(treatment)) => {
                let sign = if delta.0 >= 0.0 { "+" } else { "-" };
                lines.push(format!(
                    "     Est. cost  {sign}${} (with ${}, without ${}, {} pairs)",
                    js_to_fixed(delta.0.abs(), 4),
                    js_to_fixed(treatment.0, 4),
                    js_to_fixed(control.0, 4),
                    cost.eligible_pairs
                ));
            }
            _ => lines.push("     Est. cost  unavailable".to_string()),
        }
    }

    // 运营总量固定两个变体都输出，即使 0 runs 也写 `unavailable (0/0 measured)`。
    lines.push("  Operational totals".to_string());
    for totals in &report.operational_totals {
        let tokens = operational_metric(&totals.total_tokens, totals.runs, |total| {
            format!("{} tokens", js_number::format_js_string(total))
        });
        let tools = operational_metric(&totals.tool_calls, totals.runs, |total| {
            format!("{} tools", js_number::format_js_string(total))
        });
        let latency = operational_metric(&totals.total_ms, totals.runs, |total| {
            format!("{}s", js_to_fixed(total / 1000.0, 2))
        });
        let cost = operational_metric(&totals.estimated_cost_usd, totals.runs, |total| {
            format!("${} cost", js_to_fixed(total, 4))
        });
        lines.push(format!(
            "    {}: {} runs, {tokens}, {tools}, {latency}, {cost}",
            totals.variant, totals.runs
        ));
    }

    // blocked 段只在存在时出现；每行是 `<evalSet>/<caseId>/<model>/run-<n>: <reasons joined>`。
    if !report.blocked_pairs.is_empty() {
        lines.push("  Blocked pairs".to_string());
        for blocked in &report.blocked_pairs {
            lines.push(format!(
                "    {}/{}/{}/run-{}: {}",
                blocked.eval_set,
                blocked.case_id,
                blocked.model,
                blocked.run_number,
                blocked.reasons.join("; ")
            ));
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// 单元测试（私有函数）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 移植自 `crates/kiana-json/src/js_num.rs` 的黄金表（Node 26 实测）。
    #[test]
    fn formats_numbers_like_json_stringify() {
        assert_eq!(js_number::format_js_number(0.0), "0");
        assert_eq!(js_number::format_js_number(-0.0), "0");
        assert_eq!(js_number::format_js_number(150.0), "150");
        assert_eq!(js_number::format_js_number(1.0), "1");
        assert_eq!(js_number::format_js_number(-1.5), "-1.5");
        assert_eq!(js_number::format_js_number(0.1), "0.1");
        assert_eq!(js_number::format_js_number(1e-6), "0.000001");
        assert_eq!(js_number::format_js_number(1e-7), "1e-7");
        assert_eq!(js_number::format_js_number(1e21), "1e+21");
        assert_eq!(js_number::format_js_number(1e20), "100000000000000000000");
        assert_eq!(js_number::format_js_number(f64::NAN), "null");
        assert_eq!(js_number::format_js_number(f64::INFINITY), "null");
        assert_eq!(js_number::format_js_number(f64::NEG_INFINITY), "null");
        assert_eq!(js_number::format_js_number(1234.5), "1234.5");
    }

    /// `String(number)` 与 `JSON.stringify` 只在非有限值上不同（Node 26 实测）。
    #[test]
    fn formats_numbers_like_number_to_string() {
        assert_eq!(js_number::format_js_string(f64::INFINITY), "Infinity");
        assert_eq!(js_number::format_js_string(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(js_number::format_js_string(f64::NAN), "NaN");
        assert_eq!(js_number::format_js_string(-0.0), "0");
        assert_eq!(js_number::format_js_string(150.0), "150");
        assert_eq!(js_number::format_js_string(1e21), "1e+21");
        assert_eq!(js_number::format_js_string(1e-7), "1e-7");
        assert_eq!(js_number::format_js_string(1e-6), "0.000001");
        assert_eq!(js_number::format_js_string(0.5), "0.5");
    }

    fn color_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// 期望值全部来自 Node 26 `styleText("bold", "X")` 的实测矩阵（`internal/tty` 的
    /// `getColorDepth` + `internal/util/colors` 的 `shouldColorize`）。
    #[test]
    fn color_support_matches_node_should_colorize() {
        // 非 TTY：只有 FORCE_COLOR 能强制着色。
        assert!(!should_colorize(&color_env(&[]), false));
        assert!(!should_colorize(&color_env(&[("NO_COLOR", "1")]), false));
        assert!(!should_colorize(
            &color_env(&[("TERM", "xterm-256color")]),
            false
        ));
        assert!(should_colorize(&color_env(&[("FORCE_COLOR", "1")]), false));
        assert!(should_colorize(&color_env(&[("FORCE_COLOR", "2")]), false));
        assert!(should_colorize(&color_env(&[("FORCE_COLOR", "3")]), false));
        assert!(should_colorize(
            &color_env(&[("FORCE_COLOR", "true")]),
            false
        ));
        assert!(should_colorize(&color_env(&[("FORCE_COLOR", "")]), false));
        assert!(should_colorize(
            &color_env(&[("CI", "true"), ("FORCE_COLOR", "")]),
            false
        ));
        for disabled in ["0", "false", "4", "5", "garbage", "-1", "1.5"] {
            assert!(
                !should_colorize(&color_env(&[("FORCE_COLOR", disabled)]), false),
                "FORCE_COLOR={disabled}"
            );
        }

        // TTY：没有 FORCE_COLOR 时按环境里的颜色档位判断。
        assert!(!should_colorize(&color_env(&[]), true));
        for supported in [
            "xterm-256color",
            "xterm",
            "screen-256color",
            "vt100",
            "ansi",
            "konsole",
            "xterm-kitty",
            "con80x25",
        ] {
            assert!(
                should_colorize(&color_env(&[("TERM", supported)]), true),
                "TERM={supported}"
            );
        }
        assert!(!should_colorize(&color_env(&[("TERM", "dumb")]), true));
        assert!(should_colorize(
            &color_env(&[("COLORTERM", "truecolor")]),
            true
        ));
        assert!(should_colorize(&color_env(&[("COLORTERM", "1")]), true));
        assert!(!should_colorize(
            &color_env(&[("NO_COLOR", "1"), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("NO_COLOR", ""), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(!should_colorize(
            &color_env(&[("NODE_DISABLE_COLORS", "1"), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(!should_colorize(
            &color_env(&[("FORCE_COLOR", "4"), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("FORCE_COLOR", ""), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(should_colorize(&color_env(&[("TMUX", "1")]), true));
        // Node 对 `TMUX`/`COLORTERM` 用真值判断，空串不算设置。
        assert!(!should_colorize(&color_env(&[("TMUX", "")]), true));
        assert!(!should_colorize(&color_env(&[("COLORTERM", "")]), true));
        assert!(should_colorize(
            &color_env(&[("TF_BUILD", "1"), ("AGENT_NAME", "x")]),
            true
        ));
        // `TF_BUILD`/`AGENT_NAME`/`CI` 用 `hasOwn`，空串也算设置。
        assert!(should_colorize(
            &color_env(&[("TF_BUILD", ""), ("AGENT_NAME", "")]),
            true
        ));
        assert!(!should_colorize(
            &color_env(&[("CI", "true"), ("TERM", "xterm-256color")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("CI", "true"), ("GITHUB_ACTIONS", "true")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("CI", "true"), ("CI_NAME", "codeship")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("TEAMCITY_VERSION", "9.1.")]),
            true
        ));
        assert!(!should_colorize(
            &color_env(&[("TEAMCITY_VERSION", "9.0.1.")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("TEAMCITY_VERSION", "10.")]),
            true
        ));
        assert!(!should_colorize(
            &color_env(&[("TEAMCITY_VERSION", "9.")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("TERM_PROGRAM", "iTerm.app")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "0.10")
            ]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[
                ("TERM_PROGRAM", "iTerm.app"),
                ("TERM_PROGRAM_VERSION", "3.4")
            ]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("TERM_PROGRAM", "Apple_Terminal")]),
            true
        ));
        assert!(should_colorize(
            &color_env(&[("TERM_PROGRAM", "HyperTerm")]),
            true
        ));
    }

    #[test]
    fn style_bold_wraps_only_when_colorized() {
        assert_eq!(style_bold_with("X", true), "\u{1b}[1mX\u{1b}[22m");
        assert_eq!(style_bold_with("X", false), "X");
    }

    /// `rewrite_v8_json`：`1e999` → 哨兵对象；孤立代理项 → `�`；合法代理对不动。
    #[test]
    fn rewrites_v8_only_numbers_and_lone_surrogates() {
        let backslash = '\\';
        let text = format!(
            "{{\"a\":1e999,\"b\":\"1e999\",\"c\":\"{backslash}ud83d{backslash}ude00\",\"d\":\"{backslash}ud800\"}}"
        );
        let rewritten =
            reader::rewrite_v8_json(&text).expect("the text contains a v8-only literal");
        assert!(rewritten.contains("\"b\":\"1e999\""), "{rewritten}");
        assert!(
            rewritten.contains(&format!("\"c\":\"{backslash}ud83d{backslash}ude00\"")),
            "{rewritten}"
        );
        assert!(
            rewritten.contains(&format!("\"d\":\"{backslash}uFFFD\"")),
            "{rewritten}"
        );
        let parsed: Value = serde_json::from_str(&rewritten).unwrap();
        assert!(reader::is_nonfinite_sentinel(&parsed["a"]));
        assert_eq!(parsed["b"], "1e999");
        assert_eq!(parsed["c"], "😀");
        assert_eq!(parsed["d"], "\u{FFFD}");
    }

    #[test]
    fn serializes_number_fields_through_raw_value() {
        #[derive(Serialize)]
        struct Sample {
            a: JsNumber,
            b: JsNumber,
            c: JsNumber,
            d: JsNumber,
        }
        let sample = Sample {
            a: JsNumber(150.0),
            b: JsNumber(1e21),
            c: JsNumber(-0.0),
            d: JsNumber(0.5),
        };
        assert_eq!(
            serde_json::to_string(&sample).unwrap(),
            r#"{"a":150,"b":1e+21,"c":0,"d":0.5}"#
        );
    }

    /// 期望值由 Node 26 生成（`Number((t - c).toPrecision(15))`）。
    #[test]
    fn difference_matches_node_to_precision_15() {
        let cases: [(f64, f64, f64); 27] = [
            (1.0, 0.5, 0.5),
            (0.30000000000000004, 0.1, 0.2),
            (2.0 / 3.0, 1.0 / 3.0, 0.333333333333333),
            (2.5, 0.0, 2.5),
            (0.25, 0.5, -0.25),
            (1234567890123456.0, 0.0, 1234567890123460.0),
            (1234567890123456.0, 1.0, 1234567890123460.0),
            (1e21, 0.0, 1e21),
            (1e-7, 0.0, 1e-7),
            (1e-6, 0.0, 1e-6),
            (150.0, 150.0, 0.0),
            (150.5, 100.25, 50.25),
            (1234.5678, 123.4567, 1111.1111),
            (0.07, 0.02, 0.05),
            (1e15, 0.0, 1e15),
            (999999999999999.0, 0.0, 999999999999999.0),
            (1e16, 0.0, 1e16),
            (0.000001234567890123456, 0.0, 0.00000123456789012346),
            (9007199254740991.0, 0.0, 9007199254740990.0),
            (
                1.7976931348623157e308,
                1.7976931348623155e308,
                1.99584030953472e292,
            ),
            (5e-324, 0.0, 5e-324),
            (-0.5, 0.5, -1.0),
            (-2.5, 0.0, -2.5),
            (0.1, 0.3, -0.2),
            (1e300, 1e299, 9e299),
            (std::f64::consts::PI, std::f64::consts::E, 0.423310825130748),
            (0.5, 0.5, 0.0),
        ];
        for (treatment, control, expected) in cases {
            assert_eq!(
                difference(treatment, control),
                expected,
                "difference({treatment}, {control})"
            );
        }
    }

    #[test]
    fn optional_helpers_follow_typescript() {
        assert_eq!(optional_metric(None, "x").unwrap(), None);
        assert_eq!(
            optional_metric(Some(JsNumber(0.0)), "x").unwrap(),
            Some(JsNumber(0.0))
        );
        assert!(optional_metric(Some(JsNumber(-1.0)), "x").is_err());
        assert!(optional_metric(Some(JsNumber(f64::NAN)), "x").is_err());
        assert_eq!(validate_score(None).unwrap(), None);
        assert_eq!(validate_score(Some(JsNumber(1.0))).unwrap(), Some(1.0));
        assert_eq!(validate_score(Some(JsNumber(0.0))).unwrap(), Some(0.0));
        assert!(validate_score(Some(JsNumber(1.5))).is_err());
    }

    /// ICU en-US 的 ASCII 控制字符排序（golden 矩阵来自 Node 26
    /// `new Intl.Collator("en-US").compare`）。
    #[test]
    fn locale_comparison_matches_icu_for_control_characters() {
        let cases: &[(&str, &str, std::cmp::Ordering)] = &[
            ("case\ttab", "case \u{1} control", std::cmp::Ordering::Less),
            ("x\ty", "x y", std::cmp::Ordering::Less),
            ("x\ny", "x\ty", std::cmp::Ordering::Greater),
            ("x\u{b}y", "x\n y", std::cmp::Ordering::Greater),
            ("x\u{c}y", "x\u{b}y", std::cmp::Ordering::Greater),
            ("x\ry", "x\u{c}y", std::cmp::Ordering::Greater),
            ("a\u{0}b", "a b", std::cmp::Ordering::Greater),
            ("a\u{0}b", "ab", std::cmp::Ordering::Equal),
            ("a\u{7f}b", "ab", std::cmp::Ordering::Equal),
            ("a\u{1}b", "a\u{2}b", std::cmp::Ordering::Equal),
            ("A\t", "a ", std::cmp::Ordering::Less),
            ("a!", "a_", std::cmp::Ordering::Greater),
            ("a0", "a!", std::cmp::Ordering::Greater),
            ("aA", "aa", std::cmp::Ordering::Greater),
            ("aB", "aA", std::cmp::Ordering::Greater),
            ("x\t", "x", std::cmp::Ordering::Greater),
            ("x\u{0}", "x", std::cmp::Ordering::Equal),
            ("x\u{0}y", "xy", std::cmp::Ordering::Equal),
            ("a\t ", "a\t_", std::cmp::Ordering::Less),
            ("a\t_", "a\t-", std::cmp::Ordering::Less),
            ("a\t!", "a\t0", std::cmp::Ordering::Less),
        ];
        for (left, right, expected) in cases {
            assert_eq!(
                eval_locale_compare(left, right),
                *expected,
                "compare({left:?}, {right:?})"
            );
            assert_eq!(
                eval_locale_compare(right, left),
                expected.reverse(),
                "compare({right:?}, {left:?})"
            );
        }
    }

    /// 随机 ASCII（含全部控制字符）差分矩阵：期望值由 Node 26
    /// `new Intl.Collator("en-US").compare` 生成（`tests/fixtures/locale_pairs.json`）。
    #[test]
    fn locale_comparison_matches_icu_on_random_ascii_pairs() {
        let fixture = include_str!("../tests/fixtures/locale_pairs.json");
        let pairs: Vec<(String, String, i64)> =
            serde_json::from_str(fixture).expect("locale fixture parses");
        assert_eq!(pairs.len(), 500);
        for (left, right, expected) in pairs {
            let sign = match eval_locale_compare(&left, &right) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            assert_eq!(sign, expected, "compare({left:?}, {right:?})");
        }
    }
}
