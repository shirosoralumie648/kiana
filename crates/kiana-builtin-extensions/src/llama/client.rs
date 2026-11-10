//! extensions/llama/client.ts。
//!
//! llama.cpp router 的 HTTP/SSE 客户端：模型目录、`/props`、加载/卸载/下载、加载与下载
//! 进度（SSE）。TS 的 `AbortSignal` 在本 crate 统一为 [`AbortScope`]；HTTP 走
//! [`kiana_ai::http`] 的冻结契约（`AbortSignal.timeout(15_000)` 对应
//! [`HttpRequest::timeout_ms`]）。
//!
//! 逐字行为（spec `ca-builtin-extensions.md` §2.16）：
//! - `formatBytes` 用 KiB/MiB/GiB/TiB（**不是** `kiana-tools` 的 KB/MB），1-2 位小数；
//! - `normalizeLlamaServerUrl` 复刻 WHATWG URL 的路径裁剪顺序（尾斜杠 → `/v1` → 空则 `/`，
//!   最后去掉 `toString()` 的尾斜杠）；
//! - SSE 只在 `\n\n` 处切帧，`\r\n` 逐 chunk 归一（**跨 chunk 的 `\r`+`\n` 不归一**，与 TS 的
//!   `decoder.decode(...).replaceAll("\r\n", "\n")` 一致），多行 `data:` 用 `\n` 连接，
//!   解析失败/缺字段静默丢弃；
//! - 轮询间隔：`unloadAndWait` 100 ms、`loadAndWait` 250 ms、`downloadAndWait` 500 ms。
//!
//! # 职责边界与调用方
//!
//! 本文件只处理「llama.cpp router 的 HTTP 形状」：拼路径与请求头、判 2xx、把 JSON 转成
//! 上层类型、把 SSE 字节流切帧。模型选择与命令编排在 `llama/index.rs`（`/llama`
//! 命令），provider 注册与目录刷新在 `llama/provider.rs`（native provider 注册），TUI
//! 渲染与带进度的执行封装在 `llama/ui.rs`；它们都通过 [`LlamaClient`]
//! 访问服务器；`format_bytes`/`normalize_llama_server_url`/`llama_inference_url` 还会被
//! crate 根重导出供下游复用。
//!
//! # 执行流程（`load_and_wait` / `download_and_wait`）
//!
//! 1. 建一个子 [`AbortScope`]（`watcher`），由 `link_signal` 把调用方 signal 的中止传播
//!    过来；`watch` 使用这个子 signal，而不是调用方 signal；
//! 2. `run_with_watch` 用 `tokio::select!` 同时驱动 SSE 与轮询 future（不 `spawn`）；
//! 3. 轮询 future 先发动作请求（load/download），然后按固定间隔读 `GET /models`，
//!    根据目录状态或 SSE 记录判定成功/失败；
//! 4. 本函数返回前移除 signal 监听并中止 `watcher`，避免监听器长期挂在调用方 signal 上。
//!
//! SSE 提供进度与结束信号；目录轮询才是权威（spec §2.16.10：解析失败/缺字段的
//! SSE 帧被静默丢弃），因此 `watch` 的错误在 `run_with_watch` 中被忽略。
//!
//! # 错误模型
//!
//! 所有错误都用 [`AiErrorKind::Other`] 构造，靠 `name`/`message` 区分：URL 解析失败是
//! `TypeError: Invalid URL`，请求超时是 `TimeoutError`，取消取 `signal.reason` 的
//! `name`/`message`（缺省 `Error: Cancelled`）。`llama/index.rs` 的 `is_connection_error`
//! 把 `name` 与 `message` 拼接、小写后按 `fetch failed`/`timeout`/`network` 子串识别连接
//! 问题。
//!
//! # 阅读顺序
//!
//! 先看 [`LlamaClient`] 的 `request`/`watch`（HTTP 与 SSE 两个入口），再看
//! `load_and_wait`/`download_and_wait`（组合两者的等待逻辑）；类型定义在文件上半部分，
//! 纯函数（`format_bytes`、URL 归一、进度解析）在中间。

// `AiError` 是 `kiana-ai` 的共享错误契约（128 字节）；本模块的公开签名由 TS 的
// `Promise`/`throw` 语义直接映射而来，不装箱（与 `kiana-config`/`kiana-ai` 的同类模块一致）。
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::http::reqwest_client::ReqwestHttpClient;
use kiana_ai::http::{HttpRequest, HttpResponse, SharedHttpClient};
use kiana_ai::utils::abort::{AbortListener, AbortReason, AbortScope};
use serde_json::{Value, json};

/// `request` 中每次 HTTP 请求的总超时（TS `AbortSignal.timeout(15_000)`）；
/// `watch` 的长连接不使用它（那里 `timeout_ms` 为 `None`）。
pub const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// `fetch` 的 `Content-Type`。
const CONTENT_TYPE: &str = "application/json";

/// `llama.cpp` HTTP 错误的统一构造点（`Display` == TS `error.message`）。
fn llama_error(message: impl Into<String>) -> AiError {
    AiError::new(AiErrorKind::Other, message)
}

/// `signal.reason ?? new Error("Cancelled")`。
///
/// reason 的 `name`/`message` 原样搬进 `AiError`，`kind` 固定为 [`AiErrorKind::Other`]
/// （因此 `AiError::is_aborted()` 对它为 false；与 TS 一样，可区分的信息只有
/// `name`/`message`）。
pub(crate) fn abort_error(signal: &AbortScope) -> AiError {
    match signal.reason() {
        Some(reason) => AiError::new(AiErrorKind::Other, reason.message).with_name(reason.name),
        None => llama_error("Cancelled"),
    }
}

/// `fetch` 失败 → TS 抛出的 `error`。
///
/// Node 的 `fetch` 在调用方 signal 中止时以 **`signal.reason`** reject；冻结的
/// [`kiana_ai::http::HttpClient`] 契约只接收 [`tokio_util::sync::CancellationToken`]（reason 不在
/// 契约里），所以客户端只能看到缺省的 `AbortError` 文案（`This operation was aborted`）。
/// 这里用调用方 signal 的 `reason` 还原 TS 的可见错误：`/llama` 的连接错误识别与
/// `connectionErrorMessage`（`Could not connect to the server.`）依赖它。
///
/// 请求自身的 15 s 超时（`HttpRequest::timeout_ms`，等价 `AbortSignal.timeout`）产生
/// `TimeoutError` 的名称与文案；其余传输错误保持 `HttpError::message`（`fetch failed` 等）。
fn http_error(error: kiana_ai::http::HttpError, signal: Option<&AbortScope>) -> AiError {
    if let Some(signal) = signal
        && signal.is_aborted()
    {
        return abort_error(signal);
    }
    if error.message == kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE {
        return AiError::new(AiErrorKind::Other, error.message).with_name("TimeoutError");
    }
    llama_error(error.message)
}

/// `if (apiKey)`：空字符串是 falsy，不发 `Authorization`。
fn authorization_header(api_key: &Option<String>) -> Option<(String, String)> {
    api_key
        .as_deref()
        .filter(|api_key| !api_key.is_empty())
        .map(|api_key| ("Authorization".to_string(), format!("Bearer {api_key}")))
}

/// 默认 `HttpClient`（等价全局 `fetch`）。
///
/// 进程内只构造一次（`OnceLock`），所有 `LlamaClient::new` 共享同一连接池。
///
/// 构造失败时退回 `ReqwestHttpClient::default()`（与 `kiana-ai` 的其它 provider 相同）。
/// 注意这条回退路径并不等价于 `ReqwestHttpClient::new()`：`default()` 使用
/// `reqwest::Client::default()`，没有关闭 reqwest 的内建解压，且其构建失败时会 panic
/// （见 `kiana_ai::http::reqwest_client::ReqwestHttpClient` 的文档）；`new()` 返回的
/// `Err` 在这里被吞掉，正常路径仍优先使用 `new()`。
pub fn default_http_client() -> SharedHttpClient {
    static DEFAULT: OnceLock<SharedHttpClient> = OnceLock::new();
    DEFAULT
        .get_or_init(|| match ReqwestHttpClient::new() {
            Ok(client) => client.shared(),
            Err(_) => std::sync::Arc::new(ReqwestHttpClient::default()),
        })
        .clone()
}

/// `LlamaModelStatus`（`"unloaded" | "loading" | "loaded" | "downloading" | "sleeping"`）。
///
/// TS 是字符串字面量联合；Rust 保留 `String` 以容纳服务端未来新增的状态值（`modelDescription`
/// 会原样显示它们）。
pub type LlamaModelStatus = String;

/// `LlamaModelInfo.status`。
///
/// TS 的这些字段是原始 JSON，使用点的判定也是 JS 语义：`args ?? []` 后用 `.length`/下标
/// 索引（字符串按 UTF-16 码元、对象按数字键），`failed` 用真值，`exit_code` 区分
/// `undefined`（键不存在）与其它任意值，`progress` 只要能做 `Object.values`（对象或数组）
/// 就交给 `parse_download_progress`。
///
/// 字段因此保留原始 JSON 而不是归一成 `bool`/`String`：统一转换会改变上述判定；
/// `failed_truthy`/`args_length`/`arg_at` 是这些 JS 语义在本 crate 内的读取入口。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaModelStatusInfo {
    /// `status.value`。
    pub value: String,
    /// `status.args`（原始 JSON；缺失为 `null`，使用点按 `args ?? []` 处理）。
    pub args: Value,
    /// `status.failed`（原始 JSON；`None` = 键不存在）。
    pub failed: Option<Value>,
    /// `status.exit_code`（`None` = 键不存在；`null` 是真实值，显示为 `"null"`）。
    pub exit_code: Option<Value>,
    /// `status.progress`（对象或数组；其它类型为 `None`）。
    pub progress: Option<Value>,
}

impl LlamaModelStatusInfo {
    /// `status.failed` 的真值（`!status.failed` / `status.failed`）。
    pub(crate) fn failed_truthy(&self) -> bool {
        kiana_json::js_string::is_truthy(self.failed.as_ref())
    }

    /// `(status.args ?? []).length`（字符串按 UTF-16 码元数；其它非数组取 `.length` 的数字值）。
    pub(crate) fn args_length(&self) -> f64 {
        match &self.args {
            Value::Array(entries) => entries.len() as f64,
            Value::String(text) => text.encode_utf16().count() as f64,
            Value::Object(map) => map.get("length").map(js_number).unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }

    /// `(status.args ?? [])[index]`（缺失/不可索引时为 `undefined`，这里用 `null` 表示；
    /// 与 TS 一样，它既不会等于 `"--ctx-size"` 等标记，`Number(undefined)` 的 NaN 与
    /// `Number(null)` 的 0 在调用点（`> 0` 且有限/安全整数）判定相同）。
    pub(crate) fn arg_at(&self, index: usize) -> Value {
        match &self.args {
            Value::Array(entries) => entries.get(index).cloned().unwrap_or(Value::Null),
            Value::String(text) => text
                .encode_utf16()
                .nth(index)
                .map(|unit| Value::String(String::from_utf16_lossy(&[unit])))
                .unwrap_or(Value::Null),
            Value::Object(map) => map.get(&index.to_string()).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }
}

/// `LlamaModelInfo.architecture`。
///
/// 两个字段都只保留字符串项：非数组或其它类型得到空 `Vec`（TS 只用可选链读取，如
/// `model.architecture?.input_modalities?.includes(...)`，字段缺失即为 falsy，因此解析
/// 结果不需要区分“字段缺失”与“空数组”）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaModelArchitecture {
    /// `input_modalities`（仅字符串项）。
    pub input_modalities: Vec<String>,
    /// `output_modalities`（仅字符串项）。
    pub output_modalities: Vec<String>,
}

/// `LlamaModelInfo.meta`。
///
/// 字段只在类型匹配时保留：数值字段要求 JSON 数字，`ftype` 要求字符串；缺失或类型不符
/// 为 `None`。`n_ctx`/`n_ctx_train` 被 `llama/provider.rs` 用作上下文窗口的来源。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaModelMeta {
    /// `n_ctx`。
    pub n_ctx: Option<f64>,
    /// `n_ctx_train`。
    pub n_ctx_train: Option<f64>,
    /// `size`。
    pub size: Option<f64>,
    /// `ftype`。
    pub ftype: Option<String>,
}

/// `LlamaModelInfo`。
///
/// `list` 通过 `from_value` 构造它：只要求 `id` 与 `status.value` 是字符串，
/// 其余字段尽量提取、类型不符就丢弃。`id` 是 router 的唯一键，本文件的等待逻辑都用
/// `candidate.id == model` 查找条目。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaModelInfo {
    /// 模型 id（router 里的唯一键）。
    pub id: String,
    /// 别名。
    pub aliases: Option<Vec<String>>,
    /// 状态。
    pub status: LlamaModelStatusInfo,
    /// 架构（`input_modalities`/`output_modalities`）。
    pub architecture: Option<LlamaModelArchitecture>,
    /// 来源（`"preset"`、`"cache"`、`"models_dir"` …）。
    pub source: Option<String>,
    /// 运行时元数据。
    pub meta: Option<LlamaModelMeta>,
}

impl LlamaModelInfo {
    /// `isModelInfo(value)` + 宽松字段提取（TS 只校验 `id` 与 `status.value` 的类型）。
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = object.get("id")?.as_str()?.to_string();
        let status = object.get("status")?.as_object()?;
        let status_value = status.get("value")?.as_str()?.to_string();
        let status_info = LlamaModelStatusInfo {
            value: status_value,
            args: status.get("args").cloned().unwrap_or(Value::Null),
            failed: status.get("failed").cloned(),
            exit_code: status.get("exit_code").cloned(),
            // `typeof progress === "object" && progress !== null`：数组也保留。
            progress: status
                .get("progress")
                .filter(|value| value.is_object() || value.is_array())
                .cloned(),
        };
        Some(LlamaModelInfo {
            id,
            aliases: string_array(object.get("aliases")),
            status: status_info,
            architecture: object.get("architecture").and_then(Value::as_object).map(
                |architecture| LlamaModelArchitecture {
                    input_modalities: string_array(architecture.get("input_modalities"))
                        .unwrap_or_default(),
                    output_modalities: string_array(architecture.get("output_modalities"))
                        .unwrap_or_default(),
                },
            ),
            source: object
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string),
            meta: object
                .get("meta")
                .and_then(Value::as_object)
                .map(|meta| LlamaModelMeta {
                    n_ctx: meta.get("n_ctx").and_then(Value::as_f64),
                    n_ctx_train: meta.get("n_ctx_train").and_then(Value::as_f64),
                    size: meta.get("size").and_then(Value::as_f64),
                    ftype: meta
                        .get("ftype")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }),
        })
    }
}

/// 字符串数组字段（非数组或非字符串项被丢弃）。
fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|entries| {
        entries
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// `LlamaModelsResponse`（`GET /models` 的顶层形状）。
///
/// 只描述响应形状：`list()` 实际直接从原始 `Value` 取 `data`，不经过它反序列化。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaModelsResponse {
    /// `data`。
    pub data: Vec<LlamaModelInfo>,
    /// `object`。
    pub object: Option<String>,
}

/// `LlamaServerProps`（`GET /props` 里本单元关心的两个字段）。
///
/// `props()` 只做宽松提取（非对象 → 全空，类型不符 → `None`）；`models_autoload` 会被
/// `llama/provider.rs` 用来判断 router 是否会自动加载未加载的 preset。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaServerProps {
    /// `models_autoload`（仅布尔值）。
    pub models_autoload: Option<bool>,
    /// `chat_template`（仅字符串）。
    pub chat_template: Option<String>,
}

impl LlamaServerProps {
    /// 宽松解析：非对象 → 全空（TS 的解构 + 类型守卫）。
    fn from_value(payload: &Value) -> Self {
        let Some(object) = payload.as_object() else {
            return LlamaServerProps::default();
        };
        LlamaServerProps {
            models_autoload: object.get("models_autoload").and_then(Value::as_bool),
            chat_template: object
                .get("chat_template")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

/// `LlamaModelEvent`（SSE `data:` 解析后的形状）。
///
/// `from_value` 要求 `model`/`event` 都是字符串，不满足的帧被 `watch` 静默丢弃。
/// `data` 保留原始 JSON：键缺失是 `None`，显式 `null` 则是 `Some(null)`。
#[derive(Debug, Clone, PartialEq)]
pub struct LlamaModelEvent {
    /// `model`。
    pub model: String,
    /// `event`。
    pub event: String,
    /// `data`。
    pub data: Option<Value>,
}

impl LlamaModelEvent {
    /// `event && typeof event.model === "string" && typeof event.event === "string"`。
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        Some(LlamaModelEvent {
            model: object.get("model")?.as_str()?.to_string(),
            event: object.get("event")?.as_str()?.to_string(),
            data: object.get("data").cloned(),
        })
    }
}

/// `LlamaProgress`（`{ message; ratio?; detail? }`）。
///
/// `runWithProgress` 用 JS 的 `Object.assign(state, progress)` 合并更新，因此“键不存在”与
/// “键存在但为 `undefined`”必须区分：`ratio`/`detail` 用双层 `Option`——
/// `None` = 对象里没有该键（保留旧值）、`Some(None)` = 显式 `undefined`（清除）、
/// `Some(Some(value))` = 值。`parse_load_progress` 只带 `ratio` 键（`detail` 为 `None`），
/// `parse_download_progress` 两个键都带；而 `loadAndWait`/`downloadAndWait` 的首条
/// `{ message }` 不携带它们（spec §2.16.11/§2.16.12、§2.20.9）。
///
/// `ratio` 的取值范围按来源不同：`parse_load_progress` 把 `value` 夹取到 \[0,1\]；
/// `parse_download_progress` 的 `done / total` 原样保留（不夹取）——UI 画进度条时会夹取，
/// 但百分比文本用的是原值（`llama/ui.rs`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlamaProgress {
    /// `message`。
    pub message: String,
    /// `ratio` 键（见类型文档）。
    pub ratio: Option<Option<f64>>,
    /// `detail` 键（见类型文档）。
    pub detail: Option<Option<String>>,
}

impl LlamaProgress {
    /// `{ message }`：只更新文案，保留之前的 `ratio`/`detail`。
    pub fn message(message: impl Into<String>) -> Self {
        LlamaProgress {
            message: message.into(),
            ratio: None,
            detail: None,
        }
    }
}

/// `errorMessage(payload, fallback)`：优先 `payload.error.message`（非空字符串）。
fn error_message(payload: &Value, fallback: String) -> String {
    payload
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map_or(fallback, str::to_string)
}

/// `linkSignal(source, target)`：返回移除监听器的句柄（`None` 表示无需移除）。
///
/// `source` 已中止时同步把 `target` 以 `source.reason()`（缺失则用默认 AbortError）中止，
/// 不注册监听；否则注册一次性监听器，调用方在收尾时用 [`AbortListener::remove`] 解除。
///
/// 注意 `is_aborted()` 与 `add_abort_listener()` 不是原子操作（见 [`AbortScope`] 的文档）：
/// 若 source 恰好在两者之间中止，监听器不会补发，`target` 也不会被中止；此时等待逻辑仍会
/// 在下一次 `is_aborted()` 检查（或已取消 token 的 HTTP 请求）处发现自己被中止。
fn link_signal(source: Option<&AbortScope>, target: &AbortScope) -> Option<AbortListener> {
    let source = source?;
    if source.is_aborted() {
        target.abort_with(
            source
                .reason()
                .unwrap_or_else(AbortReason::default_abort_error),
        );
        return None;
    }
    let target = target.clone();
    let source_for_listener = source.clone();
    Some(source.add_abort_listener(std::sync::Arc::new(move || {
        target.abort_with(
            source_for_listener
                .reason()
                .unwrap_or_else(AbortReason::default_abort_error),
        );
    })))
}

/// `sleep(ms, signal)`：中止时以 `signal.reason ?? Error("Cancelled")` 结束。
async fn sleep(ms: u64, signal: Option<&AbortScope>) -> Result<(), AiError> {
    if let Some(signal) = signal
        && signal.is_aborted()
    {
        return Err(abort_error(signal));
    }
    let duration = Duration::from_millis(ms);
    match signal {
        None => tokio::time::sleep(duration).await,
        Some(signal) => {
            tokio::select! {
                () = tokio::time::sleep(duration) => {}
                () = signal.cancelled() => return Err(abort_error(signal)),
            }
        }
    }
    Ok(())
}

/// JS `Number(value)`（`args` 的任意 JSON 值）。
///
/// 大部分规则同 ECMAScript 的 `Number()`：`null` → 0、布尔 → 0/1、数字原样、字符串走
/// `kiana_json::js_num::parse_js_number`（`"8192"` → 8192、空串/空白 → 0、无法解析为
/// 数字的串 → NaN）、空数组 → 0、多元素数组与对象 → NaN。与 JS 有一处不同：单元素数组
/// 直接递归该元素，而 JS 的 `Number([x])` 先把元素转成字符串再解析——例如
/// `Number([true])` 在 JS 里是 NaN，这里得到 1。调用点是
/// `llama/provider.rs` 与 `llama/ui.rs` 对 `--ctx-size` 等参数值的解析。
pub(crate) fn js_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::js_num::parse_js_number(text),
        Value::Array(entries) => match entries.len() {
            0 => 0.0,
            1 => js_number(&entries[0]),
            _ => f64::NAN,
        },
        Value::Object(_) => f64::NAN,
    }
}

/// `Number.isSafeInteger(value)`。
///
/// 排除 NaN、±Infinity、小数以及 |value| > 2^53-1；调用方（`llama/provider.rs` 的
/// `configured_context_window`）用它判断 `--ctx-size`/`-c`/`-ctx` 后面的参数能否当作
/// 上下文窗口，因此 NaN（参数缺失/非数字）必须返回 `false`。
pub(crate) fn is_safe_integer(value: f64) -> bool {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
}

/// `parseLoadProgress(data)`。
///
/// 输入是 SSE 事件的 `data` 对象。`data.progress` 缺失或既不是对象也不是数组时返回
/// `None`（不报告进度，但事件本身仍参与加载状态判定）；数组按“没有字段”处理，仍会返回
/// 默认消息与 `ratio: Some(None)`（对应 TS 解构数组得到的 `undefined`）。
/// 返回 `Some` 时 `ratio` 键总是存在，`detail` 从不设置。
fn parse_load_progress(data: &Value) -> Option<LlamaProgress> {
    // JS 的 `typeof x === "object"` 包含数组；数组没有字符串键，因此各字段都是 undefined。
    let progress = match data.get("progress")? {
        Value::Object(map) => Some(map),
        Value::Array(_) => None,
        _ => return None,
    };
    let stage = progress
        .and_then(|map| map.get("current"))
        .and_then(Value::as_str)
        .or_else(|| {
            progress
                .and_then(|map| map.get("stage"))
                .and_then(Value::as_str)
        })
        // TS 先做类型选择，再用 `stage ? … : …` 判真值：`current: ""` 仍是空串（不会回落到
        // `stage`），但按假值处理（消息退化为 "Loading model"，且不参与 stages 比例合成）。
        .filter(|stage| !stage.is_empty());
    // `stages` 只保留字符串项；它既是 `${index + stageRatio} / stages.length` 的分母，
    // 也决定当前 stage 是否有下标（不在列表里的 stage 不参与合成）。
    let stages: Vec<&str> = progress
        .and_then(|map| map.get("stages"))
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let stage_ratio = progress
        .and_then(|map| map.get("value"))
        .and_then(Value::as_f64)
        .map(|value| value.clamp(0.0, 1.0));
    let mut ratio = stage_ratio;
    // 当前阶段在 `stages` 中时，总比例 = (阶段下标 + 阶段内比例) / 阶段总数；
    // 阶段内比例缺失按 0 计（TS 的 `stageRatio ?? 0`），否则沿用夹取后的 `value`。
    if let Some(stage) = stage
        && !stages.is_empty()
        && let Some(index) = stages.iter().position(|entry| *entry == stage)
    {
        ratio = Some((index as f64 + stage_ratio.unwrap_or(0.0)) / stages.len() as f64);
    }
    Some(LlamaProgress {
        message: stage.map_or_else(
            || "Loading model".to_string(),
            |stage| format!("Loading {}", stage.replace('_', " ")),
        ),
        // TS 的对象字面量总是带 `ratio` 键（可能是 `undefined`）。
        ratio: Some(ratio),
        detail: None,
    })
}

/// `parseDownloadProgress(data)`：累加 `{file: {done,total}}` 的字节数。
///
/// `data` 或 `data.progress` 可以是对象或数组（TS 的 `typeof === "object"`）；遍历其中
/// 每个值，只有同时带数字 `done` 与 `total` 的对象才参与累加（字符串数字不算）。
/// `total <= 0` 时返回 `None`（无法给出比例）；否则返回固定消息、未夹取的
/// `done / total`，以及 `formatBytes` 格式化的 `done / total` 明细。
/// 返回 `Some` 时 `detail` 总是 `Some(Some(...))`。
fn parse_download_progress(data: &Value) -> Option<LlamaProgress> {
    // `typeof data !== "object" || data === null` → undefined（数组也算 object）。
    if !data.is_object() && !data.is_array() {
        return None;
    }
    let nested = data.get("progress");
    // 服务器有两种形状：事件 `data` 里再包一层 `progress`，或者 `data` 本身就是文件表；
    // 前者是对象/数组时优先。
    let files = match nested {
        // `typeof nested === "object" && nested !== null`（数组同样成立）。
        Some(value) if value.is_object() || value.is_array() => value,
        _ => data,
    };
    let values: Vec<&Value> = match files {
        Value::Object(entries) => entries.values().collect(),
        Value::Array(entries) => entries.iter().collect(),
        _ => Vec::new(),
    };
    let mut done = 0.0;
    let mut total = 0.0;
    for value in values {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let (Some(entry_done), Some(entry_total)) = (
            entry.get("done").and_then(Value::as_f64),
            entry.get("total").and_then(Value::as_f64),
        ) else {
            continue;
        };
        done += entry_done;
        total += entry_total;
    }
    // 没有任何“带数字 done/total 的条目”、或总量非正时不报告进度：
    // 否则会得到 0/0 的 NaN 比例（TS 的 `if (total <= 0) return undefined`）。
    if total <= 0.0 {
        return None;
    }
    Some(LlamaProgress {
        message: "Downloading model".to_string(),
        ratio: Some(Some(done / total)),
        detail: Some(Some(format!(
            "{} / {}",
            format_bytes(done),
            format_bytes(total)
        ))),
    })
}

/// `formatBytes(bytes)`：KiB/MiB/GiB/TiB，`value >= 10` 用 1 位小数，否则 2 位。
///
/// TS: `client.ts#formatBytes`。注意与 `kiana-tools` 的 `format_size`（KB/MB）不同。
///
/// 边界：`bytes < 1024` 才走 B 分支（恰好 1024 → `1.00 KiB`）；单位最多到 TiB，
/// 更大的值继续用 TiB 表示（如测试中的 `1024.0 TiB`）。非有限值按 JS 的
/// `String(number)` 输出，不会变成 JSON 语义的 `"null"`：测试分别锁定
/// `-Infinity B`、`Infinity TiB` 与 `NaN KiB`。
pub fn format_bytes(bytes: f64) -> String {
    if bytes < 1024.0 {
        // `${bytes}` 是 `String(number)`：非有限值写作 "NaN"/"Infinity"/"-Infinity"，
        // 不能用 JSON.stringify 语义的 `format_js_number`（那会得到 "null"）。
        return format!("{} B", kiana_json::js_num::to_radix_string(bytes, 10));
    }
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes / 1024.0;
    let mut unit = UNITS[0];
    let mut index = 1;
    while index < UNITS.len() && value >= 1024.0 {
        value /= 1024.0;
        unit = UNITS[index];
        index += 1;
    }
    let text = if value >= 10.0 {
        kiana_ai::model_catalog::js_to_fixed(value, 1)
    } else {
        kiana_ai::model_catalog::js_to_fixed(value, 2)
    };
    format!("{text} {unit}")
}

/// 去掉尾部所有 `/`（TS `/\/+$/u`）。
fn strip_trailing_slashes(path: &str) -> &str {
    path.trim_end_matches('/')
}

/// `normalizeLlamaServerUrl(value)`。
///
/// 处理顺序：`trim` → `new URL` → 协议检查 → 清空 hash/search → 路径去尾斜杠 →
/// 去结尾 `/v1` → 空则 `/` → `toString()` → 去掉一个尾斜杠。
///
/// 错误：`url::Url::parse` 失败（空串、`http://` 等）→ `name = "TypeError"`、
/// `message = "Invalid URL"`；协议不是 http/https → `message = "Server URL must use http
/// or https"`（`name = "Error"`）。两种都是 `AiErrorKind::Other`。
///
/// TS: `client.ts#normalizeLlamaServerUrl`（spec §2.16.9、§8.4 #8）。
pub fn normalize_llama_server_url(value: &str) -> Result<String, AiError> {
    let trimmed = kiana_json::js_string::js_trim(value);
    let mut url = url::Url::parse(trimmed)
        .map_err(|_| AiError::new(AiErrorKind::Other, "Invalid URL").with_name("TypeError"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(llama_error("Server URL must use http or https"));
    }
    url.set_fragment(None);
    url.set_query(None);
    let path = url.path().to_string();
    let without_trailing = strip_trailing_slashes(&path);
    let without_v1 = without_trailing
        .strip_suffix("/v1")
        .unwrap_or(without_trailing);
    let next_path = if without_v1.is_empty() {
        "/"
    } else {
        without_v1
    };
    url.set_path(next_path);
    let mut text = url.to_string();
    if text.ends_with('/') {
        text.pop();
    }
    Ok(text)
}

/// `llamaInferenceUrl(serverUrl)`：`${normalize(serverUrl)}/v1`。
///
/// 规范化已经去掉结尾的一个 `/v1`，所以输入 `.../v1` 与 `.../v1/` 都得到同一个推理根
/// （测试 `inference_url_appends_v1_once`）；规范化只去一层，输入 `.../v1/v1` 会先变成
/// `.../v1`，追加后仍是 `.../v1/v1`。URL 错误原样传播。
pub fn llama_inference_url(server_url: &str) -> Result<String, AiError> {
    Ok(format!("{}/v1", normalize_llama_server_url(server_url)?))
}

/// `list({ reload?, signal? })`。
#[derive(Debug, Clone, Default)]
pub struct ListOptions<'a> {
    /// `reload === true` 时追加 `?reload=1`。
    pub reload: bool,
    /// 取消信号；future 只在本次调用期间借用它。
    pub signal: Option<&'a AbortScope>,
}

/// `props({ model?, signal? })`。
#[derive(Debug, Clone, Default)]
pub struct PropsOptions<'a> {
    /// 查询单个模型（`?model=<id>&autoload=false`）。
    pub model: Option<&'a str>,
    /// 取消信号；future 只在本次调用期间借用它。
    pub signal: Option<&'a AbortScope>,
}

/// llama.cpp router 客户端。
///
/// 不可变句柄：`server_url` 已规范化（无尾斜杠；`request`/`watch` 直接把它与路径做字符串
/// 拼接），`api_key` 为 `None` 或空串时不发 `Authorization`；`http` 是所有请求共用的
/// 连接池句柄。`Clone` 复制两个 `String` 并克隆 `Arc`（`SharedHttpClient`），可跨任务共享。
#[derive(Clone)]
pub struct LlamaClient {
    /// 规范化后的服务器根 URL。
    pub server_url: String,
    api_key: Option<String>,
    http: SharedHttpClient,
}

impl std::fmt::Debug for LlamaClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlamaClient")
            .field("server_url", &self.server_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

impl LlamaClient {
    /// `new LlamaClient(serverUrl, apiKey?)`：先规范化 URL。
    ///
    /// 错误来自 [`normalize_llama_server_url`]：非法 URL（`name = "TypeError"`）或非
    /// http/https 协议，都是 `AiErrorKind::Other`。`api_key: Some("")` 与 `None` 等价
    /// （TS 的 `if (apiKey)` 是假值判断，空串不发 `Authorization`）。
    pub fn new(server_url: &str, api_key: Option<String>) -> Result<Self, AiError> {
        Self::with_http_client(server_url, api_key, default_http_client())
    }

    /// 注入 `HttpClient` 的构造（测试/宿主自定义传输）。
    ///
    /// URL 规范化与错误同 [`LlamaClient::new`]，但不构造默认客户端；`http` 由调用方负责
    /// 生命周期（`SharedHttpClient` 即 `Arc<dyn HttpClient>`）。
    pub fn with_http_client(
        server_url: &str,
        api_key: Option<String>,
        http: SharedHttpClient,
    ) -> Result<Self, AiError> {
        Ok(LlamaClient {
            server_url: normalize_llama_server_url(server_url)?,
            api_key,
            http,
        })
    }

    /// `request(path, init)`：15 s 总超时，body 存在时补 `Content-Type`，有 key 时补
    /// `Authorization: Bearer`。
    ///
    /// `path` 以 `/` 开头，直接拼在规范化后的 `server_url` 后面（依赖构造时已去掉尾斜杠）。
    /// 超时通过 [`HttpRequest::timeout_ms`] 交给 HTTP 层，覆盖等响应头与读 body 两个阶段。
    ///
    /// 非 2xx 时**先**把 body 读完并解析，再从 `payload.error.message` 取错误文案，
    /// 回退到 `llama.cpp returned HTTP <status>`；读体失败按 `undefined` 处理（忽略解析错误）。返回的 `Value` 是解析后的
    /// body，读体/解析失败为 `Null`——所以 2xx 也可能拿到 `Null`，调用方必须自行校验形状。
    async fn request(
        &self,
        path: &str,
        method: &str,
        body: Option<Vec<u8>>,
        signal: Option<&AbortScope>,
    ) -> Result<Value, AiError> {
        let mut headers = Vec::new();
        if body.is_some() {
            headers.push(("Content-Type".to_string(), CONTENT_TYPE.to_string()));
        }
        if let Some(header) = authorization_header(&self.api_key) {
            headers.push(header);
        }
        let token = signal.map(AbortScope::token);
        let request = HttpRequest {
            method: method.to_string(),
            url: format!("{}{}", self.server_url, path),
            headers,
            body,
            timeout_ms: Some(REQUEST_TIMEOUT_MS),
        };
        let response = self
            .http
            .execute(request, token.as_ref())
            .await
            .map_err(|error| http_error(error, signal))?;
        let status = response.response.status;
        // TS 的 `try { await response.json() } catch { payload = undefined }` 把**读体失败**
        // 也一起吞掉，因此这里任何 body 读取错误都等价于 `undefined`。
        let payload = read_json_body(response).await;
        if !(200..=299).contains(&status) {
            return Err(llama_error(error_message(
                &payload,
                format!("llama.cpp returned HTTP {status}"),
            )));
        }
        Ok(payload)
    }

    /// `list(...)`：`GET /models`（`reload` 时 `?reload=1`）。
    ///
    /// 错误是“全有或全无”的：`data` 缺失/不是数组 → `llama.cpp returned an invalid model
    /// catalog`；`data` 里任一条目不满足 `isModelInfo`（缺字符串 `id` 或 `status.value`）→
    /// `Server is not running in llama.cpp router mode`。后者是 TS 用来表达“服务器不是
    /// router 模式”的文案，见 spec §2.16.10。
    pub async fn list(&self, options: ListOptions<'_>) -> Result<Vec<LlamaModelInfo>, AiError> {
        let path = if options.reload {
            "/models?reload=1"
        } else {
            "/models"
        };
        let payload = self.request(path, "GET", None, options.signal).await?;
        let Some(data) = payload.get("data").and_then(Value::as_array) else {
            return Err(llama_error("llama.cpp returned an invalid model catalog"));
        };
        let mut models = Vec::with_capacity(data.len());
        for entry in data {
            let Some(model) = LlamaModelInfo::from_value(entry) else {
                return Err(llama_error(
                    "Server is not running in llama.cpp router mode",
                ));
            };
            models.push(model);
        }
        Ok(models)
    }

    /// `props(...)`：`GET /props`；`model` 为非空字符串时追加
    /// `?model=<编码>&autoload=false`。
    ///
    /// `model: Some("")` 与 `None` 相同（TS 的 `if (model)` 是假值判断）。查询串由
    /// `url::form_urlencoded` 生成，空格编码为 `+`（`URLSearchParams` 语义，测试
    /// `encodes_query_strings_like_url_search_params` 锁定）。
    ///
    /// 响应解析是宽松的：非对象得到全空结构，字段缺失/类型不符为 `None`；HTTP 之外的
    /// 形状问题不会报错（与 TS 的解构 + 类型守卫一致）。
    pub async fn props(&self, options: PropsOptions<'_>) -> Result<LlamaServerProps, AiError> {
        let query = match options.model.filter(|model| !model.is_empty()) {
            Some(model) => {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                serializer.append_pair("model", model);
                serializer.append_pair("autoload", "false");
                format!("?{}", serializer.finish())
            }
            None => String::new(),
        };
        let payload = self
            .request(&format!("/props{query}"), "GET", None, options.signal)
            .await?;
        Ok(LlamaServerProps::from_value(&payload))
    }

    /// `load(model, signal?)`：`POST /models/load`。
    ///
    /// 只等待服务器返回 2xx（响应体读/解析失败会被当作 `Null` 吞掉，不影响成功判定），
    /// 不等待模型真正加载完成；需要等结果时
    /// 用 [`LlamaClient::load_and_wait`]。
    pub async fn load(&self, model: &str, signal: Option<&AbortScope>) -> Result<(), AiError> {
        self.post_model("/models/load", model, signal).await
    }

    /// `unload(model, signal?)`：`POST /models/unload`。
    ///
    /// 同样只等待请求被接受；需要等服务器确认卸载完成时用
    /// [`LlamaClient::unload_and_wait`]。
    pub async fn unload(&self, model: &str, signal: Option<&AbortScope>) -> Result<(), AiError> {
        self.post_model("/models/unload", model, signal).await
    }

    /// `download(model, signal?)`：`POST /models`（由服务器执行下载）。
    ///
    /// 下载在服务器侧异步进行；本方法只发起请求，进度与完成靠
    /// [`LlamaClient::download_and_wait`]（或 SSE）观察。
    pub async fn download(&self, model: &str, signal: Option<&AbortScope>) -> Result<(), AiError> {
        self.post_model("/models", model, signal).await
    }

    /// `POST` 一个 `{ "model": model }` 体，并丢弃响应内容。
    ///
    /// body 用 `kiana_json::js_object::stringify`（`JSON.stringify` 的等价物）生成，
    /// 与 TS 发出的字节一致；2xx 判定与错误文案仍由 `request` 负责。
    async fn post_model(
        &self,
        path: &str,
        model: &str,
        signal: Option<&AbortScope>,
    ) -> Result<(), AiError> {
        let body = kiana_json::js_object::stringify(&json!({ "model": model })).into_bytes();
        self.request(path, "POST", Some(body), signal).await?;
        Ok(())
    }

    /// `unloadAndWait(model, signal?)`：卸载后每 100 ms 轮询目录，直到条目消失或 `unloaded`。
    ///
    /// `unload` 失败或任一 `list` 失败会立即把错误上抛（不重试、不继续轮询）。循环本身没有
    /// 总截止时间（每次 HTTP 请求各自有 15 s 超时）：只要服务器持续报告其它状态且调用方
    /// 不中止，本函数会一直轮询。每轮重新 `list`（不缓存），判断的是服务器最新状态。
    pub async fn unload_and_wait(
        &self,
        model: &str,
        signal: Option<&AbortScope>,
    ) -> Result<(), AiError> {
        self.unload(model, signal).await?;
        loop {
            let entry = self
                .list(ListOptions {
                    reload: false,
                    signal,
                })
                .await?
                .into_iter()
                .find(|candidate| candidate.id == model);
            match entry {
                None => return Ok(()),
                Some(entry) if entry.status.value == "unloaded" => return Ok(()),
                Some(_) => {}
            }
            sleep(100, signal).await?;
        }
    }

    /// `watch(onEvent, signal?)`：读取 `/models/sse` 并按 spec §2.16.10 的算法切帧。
    ///
    /// 这是长连接：不设 `timeout_ms`（TS 只传外部 signal），正常返回只发生在服务器关闭
    /// 流；非 2xx、`fetch` 意义上没有 body 的状态（204/205/304）或传输错误会返回错误。
    /// 解析失败、缺 `model`/`event` 字段的帧被静默丢弃（目录轮询仍会给出权威状态）；
    /// 本函数返回的错误也会被 `run_with_watch` 丢弃——SSE 只是加速通道。
    ///
    /// `on_event` 在 poll 本 future 的任务上同步调用、且要求 `Fn`（会被多次调用）；
    /// 不要在回调里执行阻塞操作。帧没有大小上限：在遇到 `\n\n` 之前所有字节都留在内存里，
    /// 与 TS 的字符串 buffer 相同。
    pub async fn watch<F>(&self, on_event: F, signal: Option<&AbortScope>) -> Result<(), AiError>
    where
        F: Fn(LlamaModelEvent),
    {
        // SSE 与 `request` 的差别：GET、无 body、无超时；仍按 api_key 补 Authorization。
        let mut headers = Vec::new();
        if let Some(header) = authorization_header(&self.api_key) {
            headers.push(header);
        }
        let token = signal.map(AbortScope::token);
        let request = HttpRequest {
            method: "GET".to_string(),
            url: format!("{}/models/sse", self.server_url),
            headers,
            body: None,
            // TS 的 `watch` 只传外部 signal，不设置超时。
            timeout_ms: None,
        };
        let response = self
            .http
            .execute(request, token.as_ref())
            .await
            .map_err(|error| http_error(error, signal))?;
        // TS: `if (!response.ok || !response.body)`。`fetch` 对
        // 101/204/205/304 返回 `body === null`（GET 请求），因此即使状态是 2xx 也要报错；
        // 冻结的 `HttpResponse` 总是带一个（可能为空的）字节流，这里显式补上该分支。
        let status = response.response.status;
        if !(200..=299).contains(&status) || matches!(status, 204 | 205 | 304) {
            return Err(llama_error(format!("llama.cpp SSE returned HTTP {status}")));
        }
        let mut body = response.body;
        let mut decoder = Utf8StreamDecoder::default();
        let mut buffer = String::new();
        // 增量解码 + 缓冲：一个帧可能跨多个 chunk，未出现的 `\n\n` 之前都留在 buffer 里。
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|error| http_error(error, signal))?;
            // 注意：`replaceAll` 只作用于**本 chunk 解码后的文本**（跨 chunk 的 `\r`+`\n`
            // 不会被归一）。
            buffer.push_str(&decoder.decode(&chunk).replace("\r\n", "\n"));
            while let Some(boundary) = buffer.find("\n\n") {
                let frame: String = buffer[..boundary].to_string();
                buffer.drain(..boundary + 2);
                // 只有 `data:` 行参与；冒号后可无空格（`trimStart`），多行用 `\n` 连接成
                // 一个 JSON 文档。
                let data = frame
                    .split('\n')
                    .filter(|line| line.starts_with("data:"))
                    .map(|line| js_trim_start(&line[5..]))
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                // 解析失败或缺字段的帧静默丢弃（TS 的 `if (data) try {…} catch {}`）。
                if let Ok(value) = serde_json::from_str::<Value>(&data)
                    && let Some(event) = LlamaModelEvent::from_value(&value)
                {
                    on_event(event);
                }
            }
        }
        Ok(())
    }

    /// `loadAndWait(model, onProgress, signal?)`。
    ///
    /// TS 用 `void watch(...)` 起一个后台 promise 并同时轮询；Rust 把两者放进同一个
    /// `select!`（不 spawn，因此可在 current-thread 测试里直接调用）。
    ///
    /// # 执行流程
    ///
    /// 1. `watcher` 子 signal 由 `link_signal` 跟随调用方 signal；SSE 请求只用这个子 signal；
    /// 2. SSE 回调只处理 `model` 匹配且 `event` 为 `model_status`/`status_change` 的事件：
    ///    `status == "loaded"` 记成功、`status == "unloaded"` 记 `Model failed to load`；
    ///    同一条事件还会交给 `parse_load_progress` 报告进度（`on_progress`）；
    /// 3. 轮询体先 `POST /models/load`，成功后立刻回调一次
    ///    `{ message: "Loading model" }`（不含 ratio/detail 键），然后每 250 ms 读目录：
    ///    - `status.value == "loaded"` → 返回该目录条目；
    ///    - SSE 已报 loaded 但本次目录里条目消失 → 合成一个 loaded 条目返回；
    ///    - `status.failed` 为真或 SSE 报过 unloaded → 返回错误（优先 `exit_code`）；
    /// 4. 本函数返回前移除 signal 监听并中止 `watcher`（清理顺序见函数末尾）。
    ///
    /// 错误：`load`/`list` 的 HTTP 错误原样上抛；加载失败消息为
    /// `Model exited with code <exit_code>`（键存在即用，`null` 格式化为 `null`）或
    /// `Model failed to load`；中止时为 `signal.reason ?? Error("Cancelled")`。
    /// SSE 不可用不会中断加载：`run_with_watch` 丢弃 `watch` 的错误，轮询仍能完成或失败。
    ///
    /// 注意：SSE 状态用 `Rc<RefCell<...>>` 在回调与轮询体之间共享（不加锁），因此本函数的
    /// future 不满足 `Send`，需在当前线程驱动（例如 `llama/ui.rs` 的 `RunWithProgressFn`
    /// 使用 `LocalBoxFuture`）。
    pub async fn load_and_wait<F>(
        &self,
        model: &str,
        on_progress: F,
        signal: Option<&AbortScope>,
    ) -> Result<LlamaModelInfo, AiError>
    where
        F: Fn(LlamaProgress),
    {
        // 子 signal：`watch` 只听它；调用方 signal 的中止由 `link_signal` 传播过来。
        let watcher = AbortScope::new();
        let unlink = link_signal(signal, &watcher);
        let state = Rc::new(RefCell::new(LoadWatchState::default()));
        let on_event = {
            let state = Rc::clone(&state);
            let on_progress = &on_progress;
            move |event: LlamaModelEvent| {
                if event.model != model {
                    return;
                }
                if event.event != "model_status" && event.event != "status_change" {
                    return;
                }
                let status = event
                    .data
                    .as_ref()
                    .and_then(|data| data.get("status"))
                    .and_then(Value::as_str);
                {
                    let mut state = state.borrow_mut();
                    match status {
                        Some("loaded") => state.event_loaded = true,
                        Some("unloaded") => {
                            state.event_error = Some("Model failed to load".to_string());
                        }
                        _ => {}
                    }
                }
                if let Some(data) = event.data.as_ref()
                    && let Some(progress) = parse_load_progress(data)
                {
                    on_progress(progress);
                }
            }
        };
        // 轮询体：先触发 load，再按目录状态判定结束；SSE 只通过共享 state 提供旁路信号。
        let polling = async {
            self.load(model, signal).await?;
            // TS 的 `onProgress({ message: "Loading model" })`：对象里没有 ratio/detail 键，
            // `Object.assign` 因此保留之前渲染的值。
            on_progress(LlamaProgress::message("Loading model"));
            loop {
                if let Some(signal) = signal
                    && signal.is_aborted()
                {
                    return Err(abort_error(signal));
                }
                let entry = self
                    .list(ListOptions {
                        reload: false,
                        signal,
                    })
                    .await?
                    .into_iter()
                    .find(|candidate| candidate.id == model);
                if entry
                    .as_ref()
                    .is_some_and(|entry| entry.status.value == "loaded")
                {
                    return Ok(entry.expect("checked above"));
                }
                let (event_loaded, event_error) = {
                    let state = state.borrow();
                    (state.event_loaded, state.event_error.clone())
                };
                // SSE 已看到 loaded、但本次目录快照里没有该条目：按 TS 合成一个 loaded
                // 条目返回（不依赖目录里一定有该 id）。
                if event_loaded && entry.is_none() {
                    return Ok(LlamaModelInfo {
                        id: model.to_string(),
                        status: LlamaModelStatusInfo {
                            value: "loaded".to_string(),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                }
                // TS: `entry?.status.failed || eventError`——`failed` 是原始 JSON 的真值，
                // 不是严格的 `=== true`。
                let failed = entry
                    .as_ref()
                    .is_some_and(|entry| entry.status.failed_truthy());
                if failed || event_error.is_some() {
                    // TS: `entry?.status.exit_code === undefined`——键存在但为 `null` 时也算
                    // “有值”，插值得到 "null"。
                    let exit_code = entry
                        .as_ref()
                        .and_then(|entry| entry.status.exit_code.as_ref());
                    return Err(llama_error(match exit_code {
                        Some(code) => format!(
                            "Model exited with code {}",
                            kiana_json::js_string::js_string(Some(code))
                        ),
                        None => event_error.unwrap_or_else(|| "Model failed to load".to_string()),
                    }));
                }
                sleep(250, signal).await?;
            }
        };
        // 收尾：先解除对调用方 signal 的监听（避免它之后触发 watcher），再中止 watcher。
        let result = self
            .run_with_watch(on_event, watcher.clone(), polling)
            .await;
        if let Some(listener) = unlink {
            listener.remove();
        }
        watcher.abort();
        result
    }

    /// `downloadAndWait(model, onProgress, signal?)`。
    ///
    /// 结构与 `load_and_wait` 相同（子 signal + `select!` 驱动 SSE 与轮询），差别在结束
    /// 条件与返回值：成功后返回 `?reload=1` 重新读取的**完整目录**（`llama/index.rs` 用它
    /// 刷新 provider catalog）。
    ///
    /// # 轮询流程（每 500 ms）
    ///
    /// 1. 先响应调用方中止，再检查 SSE 记录的 `download_failed`（立即返回该错误）；
    /// 2. `GET /models`；若条目 `status.value == "downloading"`，标记 `saw_downloading`
    ///    并尝试从 `status.progress` 报告进度；
    /// 3. 否则（条目缺失或已是其它状态）读取 `finished`/`saw_downloading` 快照：
    ///    若 SSE 报过 `download_finished`，或条目存在且（见过 downloading 或已轮询
    ///    至少 2 次），立即 `GET /models?reload=1` 并返回；
    /// 4. 都不满足则睡 500 ms。
    ///
    /// 失败模式：条目始终不存在且收不到结束事件时会一直轮询（循环没有总超时，单次请求
    /// 仍有 15 s 超时）；`list` 错误直接上抛。SSE 的 `download_progress` 与目录里的
    /// `status.progress` 都可能触发同一回调，两条路径互不排斥。
    pub async fn download_and_wait<F>(
        &self,
        model: &str,
        on_progress: F,
        signal: Option<&AbortScope>,
    ) -> Result<Vec<LlamaModelInfo>, AiError>
    where
        F: Fn(LlamaProgress),
    {
        // 子 signal：`watch` 只听它；调用方 signal 的中止由 `link_signal` 传播过来。
        let watcher = AbortScope::new();
        let unlink = link_signal(signal, &watcher);
        let state = Rc::new(RefCell::new(DownloadWatchState::default()));
        let on_event = {
            let state = Rc::clone(&state);
            let on_progress = &on_progress;
            move |event: LlamaModelEvent| {
                if event.model != model {
                    return;
                }
                match event.event.as_str() {
                    "download_finished" => state.borrow_mut().finished = true,
                    "download_failed" => {
                        let message = event.data.as_ref().map_or_else(
                            || "Download failed".to_string(),
                            |data| error_message(data, "Download failed".to_string()),
                        );
                        state.borrow_mut().failure = Some(message);
                    }
                    "download_progress" => {
                        state.borrow_mut().saw_downloading = true;
                        if let Some(data) = event.data.as_ref()
                            && let Some(progress) = parse_download_progress(data)
                        {
                            on_progress(progress);
                        }
                    }
                    _ => {}
                }
            }
        };
        // 轮询体：先触发下载，再按目录状态与 SSE 记录判定结束。
        let polling = async {
            self.download(model, signal).await?;
            // TS 的 `onProgress({ message: "Downloading model" })`：同样不含 ratio/detail 键。
            on_progress(LlamaProgress::message("Downloading model"));
            let mut polls = 0u32;
            loop {
                if let Some(signal) = signal
                    && signal.is_aborted()
                {
                    return Err(abort_error(signal));
                }
                // SSE 的 download_failed 优先于目录状态：每轮开头先取一次失败快照。
                let failure = state.borrow().failure.clone();
                if let Some(failure) = failure {
                    return Err(llama_error(failure));
                }
                let models = self
                    .list(ListOptions {
                        reload: false,
                        signal,
                    })
                    .await?;
                polls += 1;
                let entry = models.into_iter().find(|candidate| candidate.id == model);
                if let Some(entry) = &entry
                    && entry.status.value == "downloading"
                {
                    state.borrow_mut().saw_downloading = true;
                    if let Some(progress) = entry
                        .status
                        .progress
                        .as_ref()
                        .and_then(parse_download_progress)
                    {
                        on_progress(progress);
                    }
                } else {
                    // TS 在 `await this.list(...)` **之后**读取 `finished`/`sawDownloading`：
                    // 请求期间到达的 SSE 事件必须在本次迭代就生效（否则会多睡一个 500 ms）。
                    let (finished, saw_downloading) = {
                        let state = state.borrow();
                        (state.finished, state.saw_downloading)
                    };
                    // 结束条件：`finished` 单独成立即可；否则要求条目存在，
                    // 且见过 downloading 或已轮询至少 2 次。
                    if finished || (entry.is_some() && (saw_downloading || polls >= 2)) {
                        return self
                            .list(ListOptions {
                                reload: true,
                                signal,
                            })
                            .await;
                    }
                }
                sleep(500, signal).await?;
            }
        };
        // 收尾：先解除对调用方 signal 的监听（避免它之后触发 watcher），再中止 watcher。
        let result = self
            .run_with_watch(on_event, watcher.clone(), polling)
            .await;
        if let Some(listener) = unlink {
            listener.remove();
        }
        watcher.abort();
        result
    }

    /// 同时轮询 `watch`（前台优先）与 `polling`，返回 `polling` 的结果。
    ///
    /// `biased` 固定每轮先 poll `watch` 分支：两边同时就绪时先处理 SSE 事件。`watch`
    /// 完成后置 `None`，用 `std::future::pending` 占位，让 `select!` 只等 `polling`；
    /// `polling` 结束（成功或错误）即返回，watch future 随之被丢弃。`watch` 的错误被
    /// `let _ =` 丢弃，对应 TS 的 `.catch(() => {})`：SSE 的失败不影响等待结果。
    async fn run_with_watch<T, F, P>(
        &self,
        on_event: F,
        watcher: AbortScope,
        polling: P,
    ) -> Result<T, AiError>
    where
        F: Fn(LlamaModelEvent),
        P: std::future::Future<Output = Result<T, AiError>>,
    {
        // `watch_future` 需要能被置空，且两个 future 都要能被反复 `&mut` 轮询，因此
        // 统一 `Box::pin` 成可 `Unpin` 的 trait object。
        let mut watch_future: Option<Pin<Box<dyn std::future::Future<Output = ()> + '_>>> =
            Some(Box::pin(async {
                let _ = self.watch(on_event, Some(&watcher)).await;
            }));
        let mut polling = Box::pin(polling);
        loop {
            let watch_branch = async {
                match watch_future.as_mut() {
                    Some(future) => future.await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                () = watch_branch => {
                    watch_future = None;
                }
                result = &mut polling => return result,
            }
        }
    }
}

/// `loadAndWait` 的 SSE 事件状态（TS 的两个闭包变量）。
#[derive(Debug, Default)]
struct LoadWatchState {
    /// 见过 `status == "loaded"` 的 SSE 事件。
    event_loaded: bool,
    /// 见过 `status == "unloaded"` 时的错误文案（`Model failed to load`）。
    event_error: Option<String>,
}

/// `downloadAndWait` 的 SSE 事件状态（TS 的四个闭包变量；`polls` 在轮询循环里）。
#[derive(Debug, Default)]
struct DownloadWatchState {
    /// 收到过 `download_finished`。
    finished: bool,
    /// 收到过 `download_failed` 时提取的错误文案。
    failure: Option<String>,
    /// 是否见过下载中状态（SSE 的 progress 事件或目录的 `downloading` 都算）。
    saw_downloading: bool,
}

/// 读取完整响应体并解析 JSON；读体失败/解析失败都对应 TS 的 `payload = undefined`。
///
/// 不是流式解析：先把整个 body 收进内存再 `serde_json::from_slice`。body 流第一次报错就
/// 放弃剩余字节并返回 `Null`（后续 chunk 不再读取），与 TS 的
/// `try { await response.json() } catch {}` 一致。
async fn read_json_body(response: HttpResponse) -> Value {
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(_) => return Value::Null,
        }
    }
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// 增量 UTF-8 解码（`TextDecoder` 的 `{stream:true}` 语义）。
///
/// 每次 `decode` 只保证返回当前已能确定的文本：末尾被截断的多字节序列留在 `pending`
/// 等下一个 chunk；非法字节序列用 U+FFFD 替换并跳过。`\r\n` 归一是 `watch` 在调用
/// `decode` 之后对本次输出单独做的，不在这里。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// 追加一个 chunk 并返回新解出的文本；未完成的多字节序列保留到下次调用。
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        // 合法前缀立即输出并移出缓冲，避免整块等待。
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    match error.error_len() {
                        // `None` = 缓冲尾部是被截断的合法前缀：保留，等下一个 chunk。
                        None => break,
                        // `Some(length)` = 真正的非法序列：写一个替换字符并跳过这些字节。
                        Some(length) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..length);
                        }
                    }
                }
            }
        }
        out
    }
}

/// JS 的空白集合（`\s` 与 `trimStart` 相同）：`WhiteSpace` + `LineTerminator`。
///
/// 与 Rust 的 `char::is_whitespace` 有两处不同：包含 `U+FEFF`（BOM/ZWNBSP），不包含
/// `U+0085`（NEL，属于 Cc，不在 ES 的空白/行终止符集合里）。
pub(crate) fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trimStart`（JS 空白集合，见 [`is_js_whitespace`]）。
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_like_javascript() {
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1023.0), "1023 B");
        assert_eq!(format_bytes(1536.0), "1.50 KiB");
        assert_eq!(format_bytes(1024.0 * 1024.0 * 10.0), "10.0 MiB");
        assert_eq!(format_bytes(1024.0 * 1024.0 * 1024.0 * 1.25), "1.25 GiB");
        assert_eq!(
            format_bytes(1024.0 * 1024.0 * 1024.0 * 1024.0 * 2.0),
            "2.00 TiB"
        );
        assert_eq!(format_bytes(512.5), "512.5 B");
        assert_eq!(format_bytes(1023.99), "1023.99 B");
        assert_eq!(format_bytes(1048575.9), "1024.0 KiB");
        assert_eq!(format_bytes(1125899906842624.0), "1024.0 TiB");
        assert_eq!(format_bytes(1374389534720.0), "1.25 TiB");
        // `${bytes}` 是 `String(number)`，不是 JSON.stringify：非有限值不能变成 "null"。
        assert_eq!(format_bytes(f64::NEG_INFINITY), "-Infinity B");
        assert_eq!(format_bytes(f64::INFINITY), "Infinity TiB");
        assert_eq!(format_bytes(f64::NAN), "NaN KiB");
        assert_eq!(format_bytes(-0.0), "0 B");
    }

    #[test]
    fn normalizes_urls_like_whatwg() {
        // golden values generated from the TS reference with Node 26 (see unit report).
        let cases = [
            ("http://127.0.0.1:8080/v1/", "http://127.0.0.1:8080"),
            (
                "https://example.com/prefix/v1",
                "https://example.com/prefix",
            ),
            ("http://h:8080/", "http://h:8080"),
            ("http://h/v1/v1", "http://h/v1"),
            ("http://h/v1/", "http://h"),
            ("HTTP://H:80/x", "http://h/x"),
            ("http://h/a/../b", "http://h/b"),
            ("http://user:pass@h", "http://user:pass@h"),
            ("http://[::1]:8080", "http://[::1]:8080"),
            ("https://h/prefix//", "https://h/prefix"),
            ("https://h/p?q=1#frag", "https://h/p"),
            ("  http://h:8080/v1  ", "http://h:8080"),
            ("https://h/%7Euser/a b", "https://h/%7Euser/a%20b"),
            ("https://h/a%2Fb", "https://h/a%2Fb"),
            (
                "https://bücher.example/x",
                "https://xn--bcher-kva.example/x",
            ),
            ("http://h:8080/v1?x=1", "http://h:8080"),
            ("http://h:8080//v1//", "http://h:8080"),
            ("http://h/v1%2", "http://h/v1%2"),
            ("http://h/a/v1/b", "http://h/a/v1/b"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                normalize_llama_server_url(input).expect(input),
                expected,
                "input: {input}"
            );
        }
        let error = normalize_llama_server_url("file:///tmp/llama").expect_err("file url");
        assert_eq!(error.message, "Server URL must use http or https");
        for invalid in ["not a url", "", "http://"] {
            let error = normalize_llama_server_url(invalid).expect_err(invalid);
            assert_eq!(error.name, "TypeError");
            assert_eq!(error.message, "Invalid URL");
        }
    }

    #[test]
    fn inference_url_appends_v1_once() {
        assert_eq!(
            llama_inference_url("http://localhost:8080/v1/").expect("url"),
            "http://localhost:8080/v1"
        );
        assert_eq!(
            llama_inference_url("http://localhost:8080").expect("url"),
            "http://localhost:8080/v1"
        );
    }

    /// 请求自带的 15 s 超时（TS `AbortSignal.timeout`）以 `TimeoutError` 的名称/文案上抛。
    #[tokio::test]
    async fn request_timeout_becomes_a_timeout_error() {
        struct TimeoutClient;
        impl kiana_ai::http::HttpClient for TimeoutClient {
            fn execute<'a>(
                &'a self,
                _request: HttpRequest,
                _signal: Option<&tokio_util::sync::CancellationToken>,
            ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, kiana_ai::http::HttpError>>
            {
                Box::pin(async {
                    Err(kiana_ai::http::HttpError::new(
                        kiana_ai::http::reqwest_client::TIMEOUT_MESSAGE,
                    ))
                })
            }
        }
        let client = LlamaClient::with_http_client(
            "http://127.0.0.1:8080",
            None,
            std::sync::Arc::new(TimeoutClient),
        )
        .expect("client");
        let error = client
            .list(ListOptions::default())
            .await
            .expect_err("timeout");
        assert_eq!(error.name, "TimeoutError");
        assert_eq!(error.message, "The operation was aborted due to timeout");
    }

    /// 调用方 signal 中止时，冻结的 HTTP 契约只给出缺省 `AbortError` 文案；客户端必须用
    /// `signal.reason` 还原 TS 的可见错误（`AbortSignal.any` 的 reason 语义）。
    #[tokio::test]
    async fn aborted_http_errors_keep_the_signal_reason() {
        struct AbortedClient;
        impl kiana_ai::http::HttpClient for AbortedClient {
            fn execute<'a>(
                &'a self,
                _request: HttpRequest,
                _signal: Option<&tokio_util::sync::CancellationToken>,
            ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, kiana_ai::http::HttpError>>
            {
                Box::pin(async {
                    Err(kiana_ai::http::HttpError::new(
                        kiana_ai::http::reqwest_client::ABORTED_MESSAGE,
                    ))
                })
            }
        }
        let client = LlamaClient::with_http_client(
            "http://127.0.0.1:8080",
            None,
            std::sync::Arc::new(AbortedClient),
        )
        .expect("client");
        let signal = AbortScope::new();
        signal.abort_with(AbortReason {
            name: "Error".to_string(),
            message: "Cancelled".to_string(),
        });
        let error = client
            .list(ListOptions {
                reload: false,
                signal: Some(&signal),
            })
            .await
            .expect_err("aborted");
        assert_eq!(error.name, "Error");
        assert_eq!(error.message, "Cancelled");
    }

    #[test]
    fn parses_load_progress_like_typescript() {
        let data = json!({
            "progress": {
                "stages": ["init", "load_tensors"],
                "current": "load_tensors",
                "value": 0.5,
            }
        });
        assert_eq!(
            parse_load_progress(&data),
            Some(LlamaProgress {
                message: "Loading load tensors".to_string(),
                ratio: Some(Some((1.0 + 0.5) / 2.0)),
                detail: None,
            })
        );
        let data = json!({ "progress": { "stage": "text_model" } });
        assert_eq!(
            parse_load_progress(&data),
            Some(LlamaProgress {
                message: "Loading text model".to_string(),
                // `ratio` 键存在但为 `undefined` → 清除旧比例。
                ratio: Some(None),
                detail: None,
            })
        );
        assert_eq!(parse_load_progress(&json!({})), None);
        assert_eq!(parse_load_progress(&json!({ "progress": 3 })), None);
        // 比例夹取到 [0,1]。
        let data = json!({ "progress": { "value": 3 } });
        assert_eq!(
            parse_load_progress(&data).expect("progress").ratio,
            Some(Some(1.0))
        );
        // 空字符串 stage 是 JS 假值：消息退化为 "Loading model"，且不参与 stages 比例合成
        //（`current` 的类型分支仍然生效，不会回落到 `stage`）。
        let data = json!({
            "progress": { "current": "", "stage": "load_tensors", "stages": ["", "load_tensors"], "value": 0.5 }
        });
        assert_eq!(
            parse_load_progress(&data),
            Some(LlamaProgress {
                message: "Loading model".to_string(),
                ratio: Some(Some(0.5)),
                detail: None,
            })
        );
    }

    #[test]
    fn parses_download_progress_with_js_byte_formatting() {
        let data = json!({ "progress": { "a": { "done": 512, "total": 1024 } } });
        assert_eq!(
            parse_download_progress(&data),
            Some(LlamaProgress {
                message: "Downloading model".to_string(),
                ratio: Some(Some(0.5)),
                detail: Some(Some("512 B / 1.00 KiB".to_string())),
            })
        );
        // `data` 本身可以作为文件表。
        let direct = json!({ "f": { "done": 1, "total": 2 } });
        assert_eq!(
            parse_download_progress(&direct).expect("progress").detail,
            Some(Some("1 B / 2 B".to_string()))
        );
        // 数组也是 JS 的 `typeof === "object"`：`Object.values` 遍历元素。
        let array = json!([{ "done": 1, "total": 2 }]);
        assert_eq!(
            parse_download_progress(&array)
                .expect("array progress")
                .ratio,
            Some(Some(0.5))
        );
        assert_eq!(
            parse_download_progress(&json!({ "f": { "done": 1 } })),
            None
        );
        assert_eq!(
            parse_download_progress(&json!({ "f": { "done": 0, "total": 0 } })),
            None
        );
    }

    #[test]
    fn extracts_model_info_leniently() {
        let value = json!({
            "id": "m",
            "status": { "value": "loaded", "args": ["--ctx-size", "32768"], "failed": false },
            "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] },
            "meta": { "n_ctx": 32768, "n_ctx_train": 131072, "ftype": "Q4_K_M" }
        });
        let model = LlamaModelInfo::from_value(&value).expect("model");
        assert_eq!(model.id, "m");
        assert_eq!(model.status.value, "loaded");
        assert_eq!(model.status.args[1], json!("32768"));
        assert_eq!(
            model.architecture.expect("arch").input_modalities,
            vec!["text".to_string(), "image".to_string()]
        );
        assert_eq!(model.meta.expect("meta").n_ctx, Some(32768.0));
        assert!(LlamaModelInfo::from_value(&json!({ "id": "x" })).is_none());
        assert!(LlamaModelInfo::from_value(&json!({ "id": "x", "status": {} })).is_none());
        assert!(LlamaModelInfo::from_value(&json!({ "status": { "value": "loaded" } })).is_none());

        // TS 保留原始 JSON：failed 是 JS 真值、exit_code 区分 undefined/null、progress 数组
        // 也参与 Object.values 遍历、args 非数组时按 `.length`/下标读取。
        let raw = LlamaModelInfo::from_value(&json!({
            "id": "raw",
            "status": {
                "value": "unloaded",
                "failed": 1,
                "exit_code": null,
                "progress": [{ "done": 1, "total": 2 }],
                "args": { "length": 3, "0": "llama-server", "1": "--ctx-size", "2": "8192" }
            }
        }))
        .expect("raw model");
        assert!(raw.status.failed_truthy());
        assert_eq!(raw.status.exit_code, Some(Value::Null));
        assert_eq!(
            raw.status.progress,
            Some(json!([{ "done": 1, "total": 2 }]))
        );
        assert_eq!(raw.status.args_length(), 3.0);
        assert_eq!(raw.status.arg_at(1), json!("--ctx-size"));
        assert_eq!(raw.status.arg_at(2), json!("8192"));
        // 字符串 args 按 UTF-16 码元索引；缺失为 undefined（`null` 占位）。
        let text_args = LlamaModelInfo::from_value(&json!({
            "id": "text",
            "status": { "value": "unloaded", "args": "ab" }
        }))
        .expect("text args");
        assert_eq!(text_args.status.args_length(), 2.0);
        assert_eq!(text_args.status.arg_at(0), json!("a"));
        assert_eq!(text_args.status.arg_at(5), Value::Null);
        // `failed: 0` / `failed: ""` 是假值，`[]`/`{}` 是真值。
        let falsy = LlamaModelInfo::from_value(&json!({
            "id": "falsy",
            "status": { "value": "unloaded", "failed": 0 }
        }))
        .expect("falsy");
        assert!(!falsy.status.failed_truthy());
        let truthy_array = LlamaModelInfo::from_value(&json!({
            "id": "truthy",
            "status": { "value": "unloaded", "failed": [] }
        }))
        .expect("truthy");
        assert!(truthy_array.status.failed_truthy());
    }

    #[test]
    fn encodes_query_strings_like_url_search_params() {
        // golden values from Node 26 `new URLSearchParams(...).toString()`。
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("model", "a b~c*d.e_f");
        serializer.append_pair("autoload", "false");
        assert_eq!(serializer.finish(), "model=a+b%7Ec*d.e_f&autoload=false");
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("search", "qwen coder ~x");
        serializer.append_pair("filter", "gguf");
        serializer.append_pair("sort", "downloads");
        serializer.append_pair("direction", "-1");
        serializer.append_pair("limit", "20");
        assert_eq!(
            serializer.finish(),
            "search=qwen+coder+%7Ex&filter=gguf&sort=downloads&direction=-1&limit=20"
        );
    }

    #[test]
    fn trims_start_like_javascript() {
        assert_eq!(js_trim_start("\u{feff} x"), "x");
        assert_eq!(js_trim_start(" \t\nx"), "x");
        assert_eq!(js_trim_start("x "), "x ");
        // JS 的 `\s`/trimStart 与 Rust 的 Unicode White_Space 不同：
        // U+0085 (NEL) 不是 JS 空白，U+FEFF (BOM) 是。
        assert_eq!(js_trim_start("\u{0085}x"), "\u{0085}x");
        assert_eq!(js_trim_start("\u{feff}\u{0085}x"), "\u{0085}x");
        assert!(!is_js_whitespace('\u{0085}'));
        assert!(is_js_whitespace('\u{feff}'));
        assert!(is_js_whitespace('\u{3000}'));
        assert!(is_js_whitespace('\u{2028}'));
    }
}
