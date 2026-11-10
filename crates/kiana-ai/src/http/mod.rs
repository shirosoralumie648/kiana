//! `FetchFunction` 的 Rust 等价物：provider 适配层只依赖这个 trait，不直接依赖 reqwest。
//!
//! TS 的 `options.fetch` 允许调用方替换整个 HTTP 栈（测试、代理、Cloudflare Workers）。
//! Rust 侧保留同一能力：默认实现是 `reqwest`（[`crate::http::reqwest_client`]），测试用 `wiremock`
//! 或自定义 `HttpClient` 注入。
//!
//! 本模块把骨架期的单文件 `http.rs` 拆成目录，文件所有权如下（§7.0 规则 1）：
//!
//! | 文件 | 内容 | 负责人 |
//! |---|---|---|
//! | `mod.rs` | 契约：`HttpClient`/`HttpRequest`/`HttpResponse`/`HttpError`/`ByteStream` | 已冻结（§4.9） |
//! | `reqwest_client.rs` | 默认 `HttpClient` 实现（reqwest + rustls） | ai-team |
//!
//! 契约本身**不得改名**（`ARCHITECTURE.md` §4.9）；实现者只填 `reqwest_client.rs`。
//!
//! # 为什么需要这一层
//!
//! TS 的 provider 最终都调用全局 `fetch`，调用方可以用 `options.fetch` 整体替换 HTTP 栈。
//! Rust 侧若让适配器直接依赖 `reqwest`，这个替换点就消失了。本模块因此只定义「一次 HTTP
//! 请求」的最小契约：输入 [`crate::http::HttpRequest`]、输出 [`crate::http::HttpResponse`]
//! （响应头 + 字节流）、失败 [`crate::http::HttpError`]；网络代码全部在具体实现里。
//!
//! 主要使用者是 `crate::api::*` 的 provider 适配器与 `crate::auth::oauth`，它们通过
//! [`crate::http::SharedHttpClient`]（`Arc<dyn HttpClient>`）调用。默认实现是
//! [`crate::http::reqwest_client::ReqwestHttpClient`]；Cloudflare Workers 下换成
//! `api/cloudflare.rs` 的 `AiBindingHttpClient`；测试注入 wiremock 或记录/脚本化的自定义
//! 实现（如 `api/google_shared.rs` 测试里的 `CaptureClient`）。
//!
//! # 一次请求的生命周期
//!
//! 1. provider 适配器算好 method/url/headers/body/timeout，构造
//!    [`crate::http::HttpRequest`]；客户端句柄来自 [`crate::types::StreamOptions`] 的
//!    `fetch` 字段，为 `None` 时由适配器选择默认实现。
//! 2. 调用 [`crate::http::HttpClient::execute`]，**响应头到达且 body 流（含 content-encoding
//!    解码链）建立成功**后才返回 `Ok`；建流阶段的失败（如响应声明的编码个数超过上限）同样
//!    以 `Err` 返回。更早阶段的失败（连接、URL/方法非法、取消、超时）也返回 `Err`。
//! 3. 适配器先看 [`crate::http::HttpResponse::response`] 的状态码与头。非 2xx 不在这里
//!    判错，provider 仍可读 [`crate::http::HttpResponse::body`] 取错误体。
//! 4. 需要时消费 body 流（SSE、JSON、二进制由适配器决定）；body 阶段的失败以流项 `Err`
//!    出现。
//!
//! # 重要约束
//!
//! - **`Ok` 不代表 2xx**：状态码判定属于 provider 层。
//! - **body 错误晚到**：`execute` 成功只说明响应头到了。读 body 失败时
//!   [`crate::http::HttpError::status`] 带上已到达的状态码，调用方据此归因。
//! - **超时与取消覆盖整条请求**：`timeout_ms` 对应 TS 的 `AbortSignal.timeout(ms)`，
//!   `signal` 对应 `options.signal`，两者都应约束「等响应头」与「读 body」两个阶段。
//!   默认实现已对齐；自定义实现若只在等头阶段观察，SSE 长连接会失去取消能力。
//! - **错误文本是契约**：默认实现的 `HttpError::message` 逐字对齐 TS 的 `error.message`
//!   （`ARCHITECTURE.md` §8 第 19 条）。`utils::retry` 按文本识别超时与 `fetch failed`
//!   （取消走结构化的 `AbortScope` / `StopReason::Aborted`，不依赖文本）；`auth/oauth` 与
//!   部分 provider 则按文本识别超时/取消（直接比较 `ABORTED_MESSAGE` 等常量）。
//!   改文案会改变可观察行为。
//! - **body 只能消费一次**：[`crate::http::ByteStream`] 是单向流，丢弃即放弃剩余 body；
//!   重试要重新构造请求（`HttpRequest` 的 `Clone` 就是给重试循环用的），不能重读旧流。
//! - **没有结构化错误种类**：`HttpError` 只有 `message` 与 `status`，取消/超时靠约定文案
//!   识别（`auth/oauth/mod.rs` 的 `http_error` 即如此）；若将来新增错误类别，必须保留这些
//!   文本，否则现有判定链路会失效。

use std::sync::Arc;

use serde_json::Value;

use crate::types::ProviderResponse;

pub mod reqwest_client;

/// 盒装 future。
///
/// `'a` 允许 future 借用 [`HttpClient::execute`] 的 `&self` 与 `signal`，无需为 `'static`
/// 克隆数据；`Send` 让 future 能跨线程轮询（tokio 多线程运行时）。
///
/// 做成 `Pin<Box<...>>` 而非 `async fn` 返回值：`HttpClient` 要以 `dyn HttpClient`
/// （trait object）保存和传递，而返回 `async fn`/`impl Future` 的 trait 方法不能做成
/// trait object。
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 一次 HTTP 请求（已包含 provider 逻辑算好的 header/body/超时）。
///
/// 构造时不校验、也不持有连接；method/url/header 是否可发送由
/// [`HttpClient::execute`] 的实现判定。`Default` 得到 method/url 为空的请求，只能作为
/// 构造起点。`Clone` 供重试循环复制请求（`body` 是 `Vec<u8>`，复制会整块拷贝请求体）。
#[derive(Debug, Clone, Default)]
pub struct HttpRequest {
    /// HTTP 方法（如 `"POST"`）。实现负责校验；默认实现按 Node `fetch` 的规则拒绝非法或
    /// 禁用的方法，并返回稳定的错误文案（改动需同步测试）。
    pub method: String,
    /// 绝对 URL（含 scheme）。部分适配器（OpenAI completions/responses 与 Azure 路径）会先用
    /// `api::openai_responses_shared::build_url` 归一化，其余适配器自行拼出并校验绝对 URL；
    /// 实现负责解析，失败时返回 [`HttpError`]。
    pub url: String,
    /// 最终请求头，顺序有意义：实现按列表顺序发送（默认实现可能在前后补协议默认头），
    /// 同名头可重复出现（多值头）。
    ///
    /// provider 已完成鉴权/模型/用户头的合并。默认实现会补协议默认头（如 `accept`），但
    /// 不覆盖调用方显式给出的同名头（名字大小写不敏感）；自定义实现若要与 `fetch` 行为
    /// 一致也应如此。
    pub headers: Vec<(String, String)>,
    /// 请求体；`None` 表示没有 body（GET/HEAD 的正常形态），`Some(vec![])` 是显式的空 body。
    pub body: Option<Vec<u8>>,
    /// 整条请求的超时（毫秒），对应 TS 的 `AbortSignal.timeout(ms)`，含等响应头与读 body；
    /// `None` 表示不设超时。
    ///
    /// 适配器可能先把该字段取走自行计时（例如 SDK 的超时只包住 `fetch`、不包 SSE 读取，
    /// 见 `api/anthropic_messages.rs`），因此实现不能假设它等于上层的超时配置，也不能假设
    /// 它一定存在。
    pub timeout_ms: Option<u64>,
}

/// 流式响应体：按到达顺序产出字节块。
///
/// `Ok` 块是任意大小的字节片段，不保证对齐 SSE 行或 JSON 边界，解析方需自行缓冲。
/// `Err` 表示读取失败：传输/解压错误，或 body 阶段的取消/超时（默认实现分别以
/// `reqwest_client::ABORTED_MESSAGE` / `TIMEOUT_MESSAGE` 产出，见该文件的
/// `ResponseByteStream`）。默认实现报错一次后即结束（后续 poll 返回 `None`），调用方
/// 应在第一次 `Err` 时停止读取；错误里的 [`HttpError::status`] 是已到达的响应状态码。
///
/// 流只能消费一次，丢弃它等于放弃剩余 body。
pub type ByteStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Vec<u8>, HttpError>> + Send>>;

/// HTTP 失败。
///
/// `Display` == TS `error.message`。`status` 只在响应头已到、但后续读取失败时出现。
///
/// 没有结构化的错误种类：取消、超时、连接失败只能靠 `message` 的约定文本区分。
/// `utils::retry` 只按文本识别超时与 `fetch failed`（取消走 `AbortScope` /
/// `StopReason::Aborted` 的结构化判定），`auth/oauth` 与部分 provider 则按文本比较常量
/// （如 `ABORTED_MESSAGE`、`TIMEOUT_MESSAGE`）；改文案等同于改行为。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct HttpError {
    /// 错误文本；默认实现与 Node `fetch` 的 `error.message` 保持一致，是稳定契约。
    pub message: String,
    /// body 阶段失败时的 HTTP 状态码（响应头已到）；连接/URL/取消/超时等头部到达前的
    /// 失败为 `None`。
    pub status: Option<u16>,
}

impl HttpError {
    /// 构造没有状态码的错误（头部到达前的失败：连接、URL/header 解析、取消、超时）。
    pub fn new(message: impl Into<String>) -> Self {
        HttpError {
            message: message.into(),
            status: None,
        }
    }

    /// 构造带状态码的错误（响应头已到，随后读取 body 失败）。
    pub fn with_status(message: impl Into<String>, status: u16) -> Self {
        HttpError {
            message: message.into(),
            status: Some(status),
        }
    }
}

/// `fetch` 的实现点。
///
/// 一个实现封装一套 HTTP 栈（生产实现把连接池、代理、TLS 都放在内部）。trait 要求
/// `Send + Sync`，因为句柄以 [`SharedHttpClient`]（`Arc<dyn HttpClient>`）在任务间共享，
/// `&self` 方法因此必须能并发调用。
pub trait HttpClient: Send + Sync {
    /// 发起请求并返回状态/头/流式体。
    ///
    /// 返回 `Ok` 只保证响应头已到（非 2xx 也是 `Ok`）；body 的失败由
    /// [`HttpResponse::body`] 的 `Err` 项传播。`signal` 是整条请求的取消信号，`None`
    /// 表示不可取消；`timeout_ms` 的处理见 [`HttpRequest`] 的字段说明。
    ///
    /// future 借用 `&'a self` 与 `signal`；实现若要把工作转交给别的任务，需要先克隆所需
    /// 数据（`api/cloudflare.rs` 的 `AiBindingHttpClient` 是一个例子）。
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>>;
}

/// 响应的统一视图。
///
/// 头与 body 分开返回：适配器先按状态码决定后续动作（读错误体、铺 SSE 解析器、直接丢弃），
/// 再决定是否消费 body。未实现 `Debug`/`Clone`（`ByteStream` 只能消费一次，也无法格式化）；
/// 测试里不要对 `Result<HttpResponse, _>` 调用 `unwrap_err`（要求 `T: Debug`，
/// `reqwest_client.rs` 的测试用 `into_error` 辅助函数绕开）。
pub struct HttpResponse {
    /// 状态码与响应头快照，在 `execute` 返回时已经确定。
    ///
    /// 不含 reason phrase；需要标准原因短语的调用方从状态码反推（OAuth 路径即如此）。
    pub response: ProviderResponse,
    /// 尚未读取的响应体字节流；丢弃即放弃剩余 body。
    pub body: ByteStream,
}

/// 用 `serde_json::Value` 直接构造请求体（provider 侧统一用 `kiana_json` 输出）。
///
/// 走 [`kiana_json::js_object::stringify`]（`JSON.stringify(value)` 的等价物）：数字按 JS
/// 格式化、对象键按 JS 顺序输出，与 TS 写出的字节一致；与 `serde_json::to_string` 的差异
/// 见该函数文档。函数不会失败，返回 UTF-8 字节、无结尾换行。
///
/// 不设置 `Content-Type`：调用方要在 `headers` 里自己加（调用本函数的 provider 适配器都
/// 设为 `application/json`）。
pub fn json_body(value: &Value) -> Vec<u8> {
    kiana_json::js_object::stringify(value).into_bytes()
}

/// 共享的客户端句柄。
///
/// `Arc` 让同一实现（及其连接池）被多个 provider 与多次请求复用；[`crate::types::StreamOptions`]
/// 的 `fetch` 字段类型就是 `Option<Arc<dyn HttpClient>>`，测试注入和适配器默认实现都通过
/// 本别名传递。
pub type SharedHttpClient = Arc<dyn HttpClient>;
