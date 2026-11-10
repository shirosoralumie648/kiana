//! runtime/protocol.ts：宿主与 worker 之间的消息。
//!
//! TS 源：`packages/codemode/src/runtime/protocol.ts`（120 行）。
//!
//! 工具参数、结果与值都以 **JSON 文本**跨边界：worker 只把它们当字符串送进/送出 QuickJS
//! VM，自己不构造结构化值。`undefined` 用 `Option<String>::None` 表示（JSON 里该键省略）。
//!
//! # 键序
//!
//! 判别式联合 `{ type: "call" | "output" | "done" | "crash" | "started" | "idle" }` 用
//! `#[serde(tag = "type", rename_all = "lowercase")]`，成员名是线上协议的稳定契约。
//! `done` 的 `ok` 再分两支：TS 是两个对象字面量，这里用 [`WorkerDoneOutcome`] 承载
//! （[`WorkerDone`] 成功 / [`WorkerDoneError`] 失败），序列化形状与 TS 相同
//! （`type: "done"` + `ok` + 各自字段）。
//!
//! # 守卫
//!
//! [`is_worker_to_host_message`] / [`is_host_to_worker_message`] **只**校验
//! `typeof value === "object" && value !== null` 与 `type` 是否属于对应集合，**不校验其余字段**。
//! 这是 TS 的行为，必须保持：畸形消息要由上层解析器报 `Sandbox bridge broken: …`。
//!
//! # 设计原因
//!
//! 本模块是三种运行位置共享的**唯一**线格式定义：worker 线程（`runtime::worker`）、
//! 调用方的 inline VM（`runtime::inline`）与远端服务（`runtime::remote`）都引用这里的类型，
//! 不各自复制消息结构；宿主侧 `runtime::execution` 也只按 `type` 分派。
//!
//! [`WorkerData`] 是唯一不能跨 JSON 的成员：它把编译后的 wasm 模块与共享中断标志
//! （`Arc<AtomicI32>`）和启动数据放在一起，只派生 `Debug`/`Clone`（进程内复制句柄）。跨
//! isolate 的请求只带其中的 [`VmStartData`]（纯数据）。
//!
//! # 一次往返
//!
//! 1. 宿主用 [`VmStartData`] 启动 VM；
//! 2. VM 产生 [`WorkerToHostMessage`]：`output`/`started`/`call`/`idle` 是过程消息，
//!    `done`/`crash` 是终态；
//! 3. 宿主只对 `call` 用 [`HostToWorkerMessage`]（`{ type: "result", id, … }`）回答；
//! 4. 判定一条 JSON 值属于哪一侧时先调对应的守卫，再按 `type` 解析。
//!
//! # 注意事项
//!
//! - 结构体字段的**声明顺序就是序列化键序**（`rename_all = "camelCase"` 只改键名）；
//!   `src/runtime/protocol.rs` 的单元测试与 `tests/protocol.rs` 的文本断言逐字锁定这些
//!   文本（`type` 在首位）。重排字段会改线格式。
//! - `Option::None` 一律配 `skip_serializing_if = "Option::is_none"`，对应 TS 的
//!   `undefined`（键缺失）；JSON 的 `null` 是另一个值，不能用 `None` 表达。
//! - `f64` 字段用 `serialize_js_number*` 写成 JS 的 `Number::toString` 文本（`5` 而不是
//!   `5.0`）；默认的 serde 数字序列化会写出后者。
//! - 本模块没有 IO、锁或任务：消息的产生与投递分别在 `runtime::vm`（VM 线程）与
//!   `runtime::execution`（宿主）里，这里只定义形状。
//! - `HostToWorkerMessage`/`VmStartData`/`WorkerToHostMessage` 经 `portable.rs` 再导出，
//!   属于 `/portable` 的冻结面（`tests/api_surface.rs` 在 portable 面锁定这些名字）；
//!   主入口 `index.rs` 不导出协议类型。改名或改字段属于 API/线格式变更。

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::types::{
    CodemodeDuring, CodemodeOutputItem, deserialize_false, deserialize_js_keyed_map,
    deserialize_optional_js_number, deserialize_true, serialize_js_keyed_map, serialize_js_number,
    serialize_optional_js_number,
};
use crate::wasm::CodemodeWasmModule;

/// `VmStartData`：运行一个脚本所需的全部数据，与运行位置无关（纯数据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmStartData {
    /// 脚本源码。
    pub code: String,
    /// 工具：`js_name` 是脚本使用的标识符，`description` 列在 `ALL_TOOLS`。
    pub tools: Vec<VmToolInfo>,
    /// 全局函数。
    pub globals: Vec<VmGlobalInfo>,
    /// 内存上限；`None` 即 TS 的 `undefined`。
    ///
    /// JSON 文本按 JS 的 `Number::toString`：`33554432` 而不是 `33554432.0`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number",
        deserialize_with = "deserialize_optional_js_number"
    )]
    pub memory_limit_bytes: Option<f64>,
    /// `load()` 的快照：key → JSON 文本。
    ///
    /// 是 JS 的 `Record<string, string>`：整数索引键按 JS 枚举序排在前（`serialize_js_map`）。
    #[serde(
        serialize_with = "serialize_js_keyed_map",
        deserialize_with = "deserialize_js_keyed_map"
    )]
    pub store: IndexMap<String, String>,
    /// 确定性 run（`CodemodeSandboxBase.start()`）。
    ///
    /// `Some` 时 VM 用 UTC 与虚拟时钟、种子随机数，按 `seq` 定序应用结果，并产生
    /// `started`/`idle` 消息；具体见 `runtime::vm` 的确定性模式一节。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deterministic: Option<VmDeterministic>,
}

/// `VmStartData.tools` 的一项：`{ name, jsName, description }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmToolInfo {
    /// 工具原名（`tools["<name>"]` 的键）。
    pub name: String,
    /// 脚本使用的标识符（`tools.<jsName>`）。
    pub js_name: String,
    /// `ALL_TOOLS` 里的描述；缺省为空串（TS 侧 `description ?? ""`）。
    pub description: String,
}

impl VmToolInfo {
    /// 由工具名与描述构造，`js_name` 取 `to_codemode_identifier(name)`
    /// （TS 的 `t.jsName ?? toCodemodeIdentifier(name)` 的默认分支）。
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        let name = name.into();
        VmToolInfo {
            js_name: crate::identifier::to_codemode_identifier(&name),
            name,
            description: description.into(),
        }
    }
}

/// `VmStartData.globals` 的一项：`{ name, spread }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmGlobalInfo {
    /// 全局名（可含一个 `.`）。
    pub name: String,
    /// 是否把全部参数作为数组传给 `execute`。
    ///
    /// 宿主构造 [`VmStartData`] 时把缺省（TS 的 `undefined`）按 `false` 处理：只把第一个参数传给
    /// `execute`（TS 的 `spread ? args : args[0]`）。
    pub spread: bool,
}

/// `deterministic?: { startedAt, seed }`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmDeterministic {
    /// VM 时钟起点（ms since epoch）；JSON 文本按 JS 的 `Number::toString`。
    #[serde(serialize_with = "serialize_js_number")]
    pub started_at: f64,
    /// 随机种子（32 位无符号）；JSON 文本按 JS 的 `Number::toString`。
    #[serde(serialize_with = "serialize_js_number")]
    pub seed: f64,
}

/// `WorkerData extends VmStartData`：worker 线程还需要编译好的模块与中断标志。
///
/// 只有 `Debug`/`Clone`，没有 `Serialize`/`Deserialize`：`wasm` 与 `interrupt` 都不能过
/// JSON，因此“structured clone”在这里只表示进程内复制句柄。本 crate 内没有构造它的代码，
/// 保留该类型是为了对齐 TS `protocol.ts` 的源码级导出；按 spec §2.2.15 与 `index.rs` 的
/// “不 从主入口导出”清单，它不属于 `index.rs`/`portable.rs` 的公开面；worker 后端实际交接用
/// `runtime::worker` 的 `WorkerStart`。
#[derive(Debug, Clone)]
pub struct WorkerData {
    /// 启动数据。
    pub data: VmStartData,
    /// 编译好的 `quickjs.wasm`（structured clone 共享编译代码）。
    pub wasm: CodemodeWasmModule,
    /// 一个 Int32：宿主在 terminate 前设成非零，VM 的中断处理器轮询它。
    pub interrupt: std::sync::Arc<std::sync::atomic::AtomicI32>,
}

impl WorkerData {
    /// 取出其中的 [`VmStartData`]。
    pub fn start_data(&self) -> &VmStartData {
        &self.data
    }
}

/// `WorkerData extends VmStartData`：字段访问自动落到内嵌的启动数据
/// （`worker_data.code` 通过 Deref 落到 `VmStartData::code`）。
impl std::ops::Deref for WorkerData {
    type Target = VmStartData;

    fn deref(&self) -> &VmStartData {
        &self.data
    }
}

impl VmStartData {
    /// 只给 `code` 的最小构造；其余字段为空/`None`。
    pub fn new(code: impl Into<String>) -> Self {
        VmStartData {
            code: code.into(),
            tools: Vec::new(),
            globals: Vec::new(),
            memory_limit_bytes: None,
            store: IndexMap::new(),
            deterministic: None,
        }
    }
}

/// `ScriptErrorJson = string`：脚本错误的 JSON `{ name?, message, stack? }`。
pub type ScriptErrorJson = String;

/// `WorkerToHostMessage`。
///
/// `#[serde(tag = "type", rename_all = "lowercase")]` 产生内部标签：序列化时 `type` 在对象
/// 首位，其余字段按变体声明顺序；反序列化要求 `type` 精确等于六个成员名之一。`done` 的两支
/// 由 [`WorkerDoneOutcome`] 再按 `ok` 判别。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WorkerToHostMessage {
    /// `{ type: "call"; id; target; name; args }`。
    Call {
        /// VM 的调用编号。
        id: u64,
        /// `"tool" | "global"`。
        target: CallTarget,
        /// 工具名或全局名。
        name: String,
        /// 参数的 JSON 文本；`None` 即 TS 的 `undefined`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<String>,
    },
    /// `{ type: "output"; item }`。
    Output {
        /// 输出项。
        item: CodemodeOutputItem,
    },
    /// `{ type: "done"; ok: true; value; writes; during? }` 或
    /// `{ type: "done"; ok: false; error; during? }`。
    #[serde(rename = "done")]
    Done(WorkerDoneOutcome),
    /// `{ type: "crash"; message }`：VM 在脚本控制之外失败（如 wasm trap）。
    Crash {
        /// 错误文本。
        message: String,
    },
    /// `{ type: "started"; fingerprint }`：确定性模式下脚本运行前的 VM 指纹。
    Started {
        /// 指纹。
        fingerprint: String,
    },
    /// `{ type: "idle"; seq; issued }`：确定性模式下 VM 处理完结果后再次等待。
    Idle {
        /// 已处理的结果号（0 = 脚本首次运行）。
        seq: u64,
        /// 已发出的调用总数。
        issued: u64,
    },
}

/// `done` 的两支：TS 的 `{ type:"done"; ok: true; … } | { type:"done"; ok: false; … }`。
///
/// 两个对象字面量共享 `type: "done"`，用 `ok` 区分；`untagged` 的顺序（成功在前）
/// 与两支各自的必填字段（成功必有 `writes`，失败必有 `error`）保证判别正确。
///
/// 反序列化没有 `ok` 之外的提示：serde 先试 [`WorkerDone`]（`ok` 由
/// `deserialize_true` 限定为 `true`，缺 `writes` 也会失败），失败后再试
/// [`WorkerDoneError`]。因此给任一支新增字段时应使用 `Option` 或 `#[serde(default)]`；否则
/// 缺少该字段的旧消息会在本支解析失败（`ok` 判别互斥，不会落到另一支）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkerDoneOutcome {
    /// `ok: true`。
    Ok(WorkerDone),
    /// `ok: false`。
    Error(WorkerDoneError),
}

impl WorkerDoneOutcome {
    /// TS 的 `message.ok`。
    pub fn ok(&self) -> bool {
        matches!(self, WorkerDoneOutcome::Ok(_))
    }
}

/// `WorkerToHostMessage` 的 `done`、`ok: true` 一支的字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerDone {
    /// 恒为 `true`（判别式）。
    ///
    /// 反序列化只接受 `true`；序列化会原样写出字段值，因此手工构造时应传 `true`
    /// （[`WorkerToHostMessage::done_ok`] 是安全入口）。
    #[serde(deserialize_with = "deserialize_true")]
    pub ok: bool,
    /// 返回值的 JSON 文本；`None` 即 TS 的 `undefined`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// store 写集合的 JSON 数组文本。
    pub writes: String,
    /// 确定性模式：脚本结束时正在处理的结果与已发出的调用数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<CodemodeDuring>,
}

/// `WorkerToHostMessage` 的 `done`、`ok: false` 一支的字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerDoneError {
    /// 恒为 `false`（判别式）。
    ///
    /// 反序列化只接受 `false`；序列化会原样写出字段值，因此手工构造时应传 `false`
    /// （[`WorkerToHostMessage::done_error`] 是安全入口）。
    #[serde(deserialize_with = "deserialize_false")]
    pub ok: bool,
    /// 脚本错误的 JSON 文本。
    pub error: ScriptErrorJson,
    /// 确定性模式：脚本结束时正在处理的结果与已发出的调用数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<CodemodeDuring>,
}

/// `target`：`"tool" | "global"`。与 `types::CodemodeHostCall.target` 是同一个联合，
/// 因此复用 [`crate::types::CodemodeCallTarget`]（只在本模块再导出一次名字）。
pub use crate::types::CodemodeCallTarget as CallTarget;

/// `HostToWorkerMessage`：宿主回答一个调用。
///
/// 目前只有一个成员：`{ type: "result", … }`。序列化时 `type` 在首位，随后按声明顺序写
/// [`HostResult`] 的字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostToWorkerMessage {
    /// `{ type: "result"; id; ok; payload; seq?; now?; properties? }`。
    #[serde(rename = "result")]
    Result(HostResult),
}

/// `HostToWorkerMessage` 的字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostResult {
    /// 调用编号。
    pub id: u64,
    /// 成功与否。
    pub ok: bool,
    /// `ok` 时是结果的 JSON 文本，否则是错误消息；`None` 即 TS 的 `undefined`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// 确定性模式：结果序号，从 1 起按 VM 应用顺序编号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// 确定性模式：处理该结果期间 VM 的时钟读数（ms since epoch）。
    ///
    /// JSON 文本按 JS 的 `Number::toString`（`1700000000000`、`0.000001`、`0`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number",
        deserialize_with = "deserialize_optional_js_number"
    )]
    pub now: Option<f64>,
    /// 确定性模式、`ok: false`：脚本 `Error` 要带上的自有属性（JSON 对象文本）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<String>,
}

impl HostToWorkerMessage {
    /// 成功结果的回答。
    ///
    /// `payload` 是结果的 JSON 文本，`None` 即 TS 的 `undefined`（键省略）；确定性模式用的
    /// `seq`/`now`/`properties` 都留空。
    pub fn ok(id: u64, payload: Option<String>) -> Self {
        HostToWorkerMessage::Result(HostResult {
            id,
            ok: true,
            payload,
            seq: None,
            now: None,
            properties: None,
        })
    }

    /// 失败结果的回答。
    ///
    /// `payload` 是错误**文本**（不是 JSON），会作为 `payload` 字段写出；确定性字段留空。
    pub fn err(id: u64, payload: String) -> Self {
        HostToWorkerMessage::Result(HostResult {
            id,
            ok: false,
            payload: Some(payload),
            seq: None,
            now: None,
            properties: None,
        })
    }

    /// 该回答的调用编号。
    pub fn id(&self) -> u64 {
        match self {
            HostToWorkerMessage::Result(result) => result.id,
        }
    }
}

impl WorkerToHostMessage {
    /// 成功终态：`{ type: "done", ok: true, value, writes, during? }`。
    ///
    /// `writes` 是 store 写集合的 JSON 数组文本：写入是 `[key, json]`，删除是 `[key]`
    /// （TS 的 `WorkerToHostMessage` 注释给出的格式）。
    pub fn done_ok(value: Option<String>, writes: String, during: Option<CodemodeDuring>) -> Self {
        WorkerToHostMessage::Done(WorkerDoneOutcome::Ok(WorkerDone {
            ok: true,
            value,
            writes,
            during,
        }))
    }

    /// 失败终态：`{ type: "done", ok: false, error, during? }`。
    pub fn done_error(error: ScriptErrorJson, during: Option<CodemodeDuring>) -> Self {
        WorkerToHostMessage::Done(WorkerDoneOutcome::Error(WorkerDoneError {
            ok: false,
            error,
            during,
        }))
    }

    /// 若本消息是 `done`，返回其载荷。
    pub fn done(&self) -> Option<&WorkerDoneOutcome> {
        match self {
            WorkerToHostMessage::Done(outcome) => Some(outcome),
            _ => None,
        }
    }

    /// TS 的 `message.type` 字符串（用于分支与错误文本）。
    pub fn type_name(&self) -> &'static str {
        match self {
            WorkerToHostMessage::Call { .. } => "call",
            WorkerToHostMessage::Output { .. } => "output",
            WorkerToHostMessage::Done(_) => "done",
            WorkerToHostMessage::Crash { .. } => "crash",
            WorkerToHostMessage::Started { .. } => "started",
            WorkerToHostMessage::Idle { .. } => "idle",
        }
    }
}

/// `isWorkerToHostMessage(value)`：只校验对象与 `type` 标签。
///
/// TS:
/// ```ts
/// if (typeof value !== "object" || value === null) return false;
/// const type = (value as { type?: unknown }).type;
/// return type === "call" || type === "output" || type === "done" || type === "crash" ||
///        type === "started" || type === "idle";
/// ```
///
/// 返回 `true` 只表示“形状上像一条 worker 消息”，不代表能反序列化成
/// [`WorkerToHostMessage`]：其余字段由上层按自己的规则读取，坏字段会变成
/// `Sandbox bridge broken: …`。
pub fn is_worker_to_host_message(value: &serde_json::Value) -> bool {
    let serde_json::Value::Object(object) = value else {
        // 数组也满足 `typeof value === "object"`，但数组没有 `type` 自有键 → false。
        return false;
    };
    matches!(
        object.get("type").and_then(serde_json::Value::as_str),
        Some("call" | "output" | "done" | "crash" | "started" | "idle")
    )
}

/// `isHostToWorkerMessage(value)`：只校验对象与 `type === "result"`。
///
/// 与 [`is_worker_to_host_message`] 一样，字段是否合法不在守卫的职责内：worker 放行后还要
/// 整体反序列化成 [`HostToWorkerMessage`]（见 `runtime::worker`）。
pub fn is_host_to_worker_message(value: &serde_json::Value) -> bool {
    matches!(value, serde_json::Value::Object(object)
        if object.get("type").and_then(serde_json::Value::as_str) == Some("result"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn guards_check_only_the_type_tag() {
        assert!(!is_worker_to_host_message(&json!(null)));
        assert!(!is_worker_to_host_message(&json!("call")));
        assert!(!is_worker_to_host_message(&json!([])));
        assert!(is_worker_to_host_message(&json!({"type": "call"})));
        assert!(is_worker_to_host_message(&json!({"type": "output"})));
        assert!(is_worker_to_host_message(&json!({"type": "done"})));
        assert!(is_worker_to_host_message(&json!({"type": "crash"})));
        assert!(is_worker_to_host_message(&json!({"type": "started"})));
        assert!(is_worker_to_host_message(&json!({"type": "idle"})));
        assert!(!is_worker_to_host_message(&json!({"type": "result"})));
        assert!(!is_worker_to_host_message(&json!({})));
        // 其余字段完全不校验（TS 行为）。
        assert!(is_worker_to_host_message(
            &json!({"type": "call", "id": "x"})
        ));

        assert!(!is_host_to_worker_message(&json!(null)));
        assert!(is_host_to_worker_message(&json!({"type": "result"})));
        assert!(!is_host_to_worker_message(&json!({"type": "call"})));
        assert!(!is_host_to_worker_message(&json!(["result"])));
    }

    #[test]
    fn call_message_shape() {
        let message = WorkerToHostMessage::Call {
            id: 3,
            target: CallTarget::Tool,
            name: "read".into(),
            args: None,
        };
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"type":"call","id":3,"target":"tool","name":"read"}"#
        );
        let parsed: WorkerToHostMessage = serde_json::from_str(
            r#"{"type":"call","id":3,"target":"global","name":"g","args":"[1]"}"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            WorkerToHostMessage::Call {
                id: 3,
                target: CallTarget::Global,
                name: "g".into(),
                args: Some("[1]".into()),
            }
        );
    }

    #[test]
    fn done_messages_round_trip() {
        let ok = WorkerToHostMessage::Done(WorkerDoneOutcome::Ok(WorkerDone {
            ok: true,
            value: Some("1".into()),
            writes: "[]".into(),
            during: Some(CodemodeDuring { seq: 2, issued: 5 }),
        }));
        let text = serde_json::to_string(&ok).unwrap();
        assert_eq!(
            text,
            r#"{"type":"done","ok":true,"value":"1","writes":"[]","during":{"seq":2,"issued":5}}"#
        );
        assert_eq!(
            serde_json::from_str::<WorkerToHostMessage>(&text).unwrap(),
            ok
        );

        let err = WorkerToHostMessage::Done(WorkerDoneOutcome::Error(WorkerDoneError {
            ok: false,
            error: r#"{"message":"boom"}"#.into(),
            during: None,
        }));
        let text = serde_json::to_string(&err).unwrap();
        assert_eq!(
            text,
            r#"{"type":"done","ok":false,"error":"{\"message\":\"boom\"}"}"#
        );
        assert_eq!(
            serde_json::from_str::<WorkerToHostMessage>(&text).unwrap(),
            err
        );
        assert_eq!(err.type_name(), "done");
    }

    #[test]
    fn output_crash_started_idle_shapes() {
        let output = WorkerToHostMessage::Output {
            item: CodemodeOutputItem::console("hi"),
        };
        assert_eq!(
            serde_json::to_string(&output).unwrap(),
            r#"{"type":"output","item":{"type":"text","text":"hi","console":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&WorkerToHostMessage::Crash {
                message: "boom".into()
            })
            .unwrap(),
            r#"{"type":"crash","message":"boom"}"#
        );
        assert_eq!(
            serde_json::to_string(&WorkerToHostMessage::Started {
                fingerprint: "fp".into()
            })
            .unwrap(),
            r#"{"type":"started","fingerprint":"fp"}"#
        );
        assert_eq!(
            serde_json::to_string(&WorkerToHostMessage::Idle { seq: 0, issued: 1 }).unwrap(),
            r#"{"type":"idle","seq":0,"issued":1}"#
        );
    }

    #[test]
    fn host_result_shape_and_key_order() {
        let message = HostToWorkerMessage::Result(HostResult {
            id: 1,
            ok: true,
            payload: Some("2".into()),
            seq: Some(1),
            now: Some(1000.0),
            properties: None,
        });
        // `now` 是 JS 数字：`JSON.stringify({now:1000})` 写 `1000`，不是 serde 的 `1000.0`。
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"type":"result","id":1,"ok":true,"payload":"2","seq":1,"now":1000}"#
        );
        let failed = HostToWorkerMessage::Result(HostResult {
            id: 2,
            ok: false,
            payload: Some("nope".into()),
            seq: None,
            now: None,
            properties: Some(r#"{"a":1}"#.into()),
        });
        assert_eq!(
            serde_json::to_string(&failed).unwrap(),
            r#"{"type":"result","id":2,"ok":false,"payload":"nope","properties":"{\"a\":1}"}"#
        );
        assert_eq!(failed.id(), 2);
    }

    #[test]
    fn vm_start_data_shape() {
        let mut store = IndexMap::new();
        store.insert("k".to_string(), "1".to_string());
        let data = VmStartData {
            code: "return 1".into(),
            tools: vec![VmToolInfo {
                name: "my-tool".into(),
                js_name: "my_tool".into(),
                description: String::new(),
            }],
            globals: vec![VmGlobalInfo {
                name: "models.list".into(),
                spread: false,
            }],
            memory_limit_bytes: None,
            store,
            deterministic: Some(VmDeterministic {
                started_at: 5.0,
                seed: 7.0,
            }),
        };
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            // 数字按 JS 的 `Number::toString`：`5` 与 `7`，不是 `5.0`/`7.0`。
            r#"{"code":"return 1","tools":[{"name":"my-tool","jsName":"my_tool","description":""}],"globals":[{"name":"models.list","spread":false}],"store":{"k":"1"},"deterministic":{"startedAt":5,"seed":7}}"#
        );
        assert_eq!(
            serde_json::from_str::<VmStartData>(&serde_json::to_string(&data).unwrap()).unwrap(),
            data
        );
    }
}
