//! extensions/llama/huggingface.ts。
//!
//! Hugging Face 搜索与模型详情：GGUF 量化名与大小、gated 标记、token 查找顺序、429 限流文案。
//! HTTP 走 [`kiana_ai::http`]（15 s 超时；`AbortSignal.any` 由 `timeout_ms` + 取消令牌表达）。
//!
//! TS 源：`client.ts` 的 `QUANTIZATION_PATTERN` / `SHARD_SUFFIX_PATTERN` 逐字移植为 `regex`
//! （`\d` 显式写成 `[0-9]`，因为 Rust 的 `\d` 默认匹配 Unicode 数字，而 JS 只匹配 ASCII）。
//!
//! # 这个模块解决什么问题
//!
//! 下载流程（`index.rs` 的 `download_model`）需要在 TUI 里搜索 Hugging Face 仓库、解析
//! GGUF 量化名与大小；llama.cpp 服务器不提供这两类查询，而且 HF token 与 llama.cpp 的
//! api-key 是两套独立凭据。本模块只做 REST 查询与纯解析，不依赖 TUI，便于单测。
//!
//! # 一次请求的执行流程
//!
//! 1. `request` 组装 `GET {base_url}{path}`；token 非空时才加 `Authorization: Bearer …`；
//!    `timeout_ms = 15_000`，调用方传入的 [`AbortScope`] 令牌是另一条取消路径
//!    （两者任一先到即中止，等价 TS 的 `AbortSignal.any([signal, timeout])`）。
//! 2. 读完整响应体后尽力解析 JSON：读流失败或不是合法 JSON 都当作 `undefined`
//!    （`Value::Null`），与 TS 的 `try { await response.json() } catch {}` 一致。
//! 3. 2xx 返回 payload；429 用 `retry-after`/`ratelimit` 生成限流文案；其余状态用
//!    `payload.error`（非空字符串）或 `Hugging Face returned HTTP {status}`。
//!
//! # 约束与注意事项
//!
//! - HTTP/状态错误统一是 `AiErrorKind::Transport`，响应形状错误是 `AiErrorKind::Parse`；
//!   调用方（`index.rs`）的展示与连接判定只读 `message`/`name`，不读 `kind`。
//! - `search` 会静默跳过没有字符串 `id` 的条目；`details` 只拒绝非对象/`null`——数组能
//!   通过形状检查，随后所有字段缺失。
//! - Token 查找只读文件、不发网络请求；文件缺失、不可读或 trim 后为空都跳到下一个候选。
//!
//! # 阅读顺序
//!
//! 先看 [`HuggingFaceClient`] 的私有方法 `request`，再看 `search`/`details`，最后看
//! `parse_details`（纯函数，行为由文件末尾的 golden 测试锁定）与
//! [`find_huggingface_token`]（与网络无关，可单独测试）。

// `AiError` 是 `kiana-ai` 的共享错误契约（128 字节）；本模块的公开签名由 TS 的
// `Promise`/`throw` 语义直接映射而来，不装箱（与 `kiana-config`/`kiana-ai` 的同类模块一致）。
#![allow(clippy::result_large_err)]

use std::sync::{Arc, LazyLock, OnceLock};

use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::http::reqwest_client::ReqwestHttpClient;
use kiana_ai::http::{HttpRequest, HttpResponse, SharedHttpClient};
use kiana_ai::utils::abort::AbortScope;
use regex::Regex;
use serde_json::Value;

/// `DEFAULT_HUGGING_FACE_URL`。
///
/// 构造函数 `base_url` 参数的缺省值；测试通过 `with_http_client` / `new` 的
/// `base_url` 指向本地回环服务器。
pub const DEFAULT_HUGGING_FACE_URL: &str = "https://huggingface.co";

/// `REQUEST_TIMEOUT_MS`（TS `AbortSignal.timeout(15_000)`）。
///
/// 作为 `HttpRequest::timeout_ms` 传给每次请求，覆盖等响应头与读 body 两个阶段；
/// 它与调用方 `AbortScope` 的令牌互不替代（TS 用 `AbortSignal.any` 合并两者）。
pub const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// `QUANTIZATION_PATTERN`（`i` + `u` 标志）。
///
/// 结构：`(?:^|[-_.])` 要求量化名位于文件名开头或紧跟分隔符，捕获组只含量化名本身；
/// 分支顺序把 `IQ` 放在 `Q` 之前、`UD-` 作为可选前缀；结尾 `$` 保证不会匹配名字中间的
/// 片段（例如 `foo_Q4_0_extra` 只会把整个 `Q4_0_EXTRA` 当作量化名）。匹配不区分大小写，
/// 因此小写文件名也能识别，大小写归一由调用方 `to_uppercase` 完成。
static QUANTIZATION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[-_.])((?:UD-)?(?:IQ[0-9](?:_[A-Z0-9]+)+|Q[0-9](?:_[A-Z0-9]+)+|BF16|F16|F32|MXFP[0-9](?:_[A-Z0-9]+)*))$",
    )
    .expect("QUANTIZATION_PATTERN is a valid static regex")
});

/// `SHARD_SUFFIX_PATTERN`（`-\d{5}-of-\d{5}$`）。
///
/// 匹配 llama.cpp 分片命名的固定宽度后缀；必须在量化匹配前剥掉，否则锚定结尾的
/// `QUANTIZATION_PATTERN` 会失配。
static SHARD_SUFFIX_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-[0-9]{5}-of-[0-9]{5}$").expect("SHARD_SUFFIX_PATTERN is a valid static regex")
});

/// `parseRateLimitDelay` 的 `t=<n>` 捕获。
///
/// `(?:^|;)` 让 `t=` 只出现在 header 值开头或分号参数之后（如 `"resolvers=1;t=30"`），
/// 数字只接受 ASCII `[0-9]+`，与 TS 正则一致。
static RATE_LIMIT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|;)t=([0-9]+)").expect("RATE_LIMIT_PATTERN is a valid static regex")
});

/// `HuggingFaceModel`。
#[derive(Debug, Clone, PartialEq)]
pub struct HuggingFaceModel {
    /// 仓库 id（`owner/repo`）。
    pub id: String,
    /// 下载数（JS number）。
    pub downloads: f64,
}

/// `HuggingFaceQuantization`。
#[derive(Debug, Clone, PartialEq)]
pub struct HuggingFaceQuantization {
    /// 量化名（大写）。
    pub name: String,
    /// 所有分片大小之和（单位：字节）；有未知大小的分片时为 `None`。
    ///
    /// `None` 不是 0：它表示“至少一个分片没报告大小”，因此不能把总和当作完整大小。
    /// `index.rs` 用 `format_bytes` 把它格式化进量化选择列表。
    pub size: Option<f64>,
}

/// `gated: false | "auto" | "manual"`。
///
/// TS 的联合里 `false` 是假值，另外两个字符串是仅有的真值；Rust 用三态枚举表达。
/// 响应里字段缺失或出现其它值（包括布尔 `true`）时映射为 [`HuggingFaceGated::No`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HuggingFaceGated {
    /// `false`。
    #[default]
    No,
    /// `"auto"`。
    Auto,
    /// `"manual"`。
    Manual,
}

impl HuggingFaceGated {
    /// TS 的真值判定（`details.gated`）。
    ///
    /// `Auto`/`Manual` 返回 `true`，`No` 返回 `false`；`index.rs` 用它在下载前决定是否
    /// 展示访问条款提示。
    pub fn is_gated(self) -> bool {
        !matches!(self, HuggingFaceGated::No)
    }
}

/// `HuggingFaceModelDetails`。
#[derive(Debug, Clone, PartialEq)]
pub struct HuggingFaceModelDetails {
    /// 仓库 id（响应里的 `id`，缺失时回退到参数）。
    ///
    /// 只接受 JSON 字符串；`id` 是数字/对象等其它类型时同样回退到参数。
    pub id: String,
    /// 访问要求。
    pub gated: HuggingFaceGated,
    /// 量化列表（`Q4_K_M` 优先，其余按大小、名称排序）。
    pub quantizations: Vec<HuggingFaceQuantization>,
}

/// `payloadError(payload, fallback)`：`payload.error` 为非空字符串时用它。
fn payload_error(payload: &Value, fallback: String) -> String {
    payload
        .get("error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
        .map_or(fallback, str::to_string)
}

/// `parseRateLimitDelay(value)`：`/(?:^|;)t=(\d+)/` → `Number(match[1])`。
fn parse_rate_limit_delay(value: Option<&str>) -> Option<f64> {
    let value = value?;
    let captures = RATE_LIMIT_PATTERN.captures(value)?;
    let digits = captures.get(1)?.as_str();
    Some(kiana_json::js_num::parse_js_number(digits))
}

/// 429 的文案。TS:
/// `const delay = Number(retry-after) || parseRateLimitDelay(ratelimit); delay ? … : …`
///
/// 第二处 `delay ?` 是 JS truthiness：`0`（例如 `retry-after: 0` 且 `ratelimit: t=0`）
/// 也走通用文案，而不是 `retry in 0s`。`retry-after` 头缺失、为空串或非数字（`Number`
/// 得到 `0`/`NaN`）时都会落到 `ratelimit` 解析；两者都没有非零延迟时用通用文案。
fn rate_limit_message(retry_after: Option<&str>, ratelimit: Option<&str>) -> String {
    let retry_after = retry_after.map(kiana_json::js_num::parse_js_number);
    let delay = match retry_after {
        Some(value) if !value.is_nan() && value != 0.0 => Some(value),
        _ => parse_rate_limit_delay(ratelimit),
    };
    match delay.filter(|delay| *delay != 0.0 && !delay.is_nan()) {
        Some(delay) => format!(
            // `${delay}` 是 `String(number)`：非有限值写作 "Infinity"，
            // 不是 JSON.stringify 语义的 "null"。
            "Hugging Face rate limit reached; retry in {}s",
            kiana_json::js_num::to_radix_string(delay, 10)
        ),
        None => "Hugging Face rate limit reached".to_string(),
    }
}

/// `fetch("…").json()` 的宽松读取（读体/解析失败 → `undefined`，这里用 [`Value::Null`]）。
///
/// 会消费整个 `response.body` 流，因此调用方必须先取走状态码与响应头（`request` 就是这么
/// 做的）。任何流错误立即返回 `Null`；成功读完但不是合法 UTF-8 JSON 时也返回 `Null`，
/// 所以返回值无法区分“空 body”“非法 JSON”和“真正的 `null` JSON”。
async fn read_json_body(response: HttpResponse) -> Value {
    use futures::StreamExt;
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

/// `encodeURIComponent`（只保留 `A-Za-z0-9-_.!~*'()`）。
///
/// 按 UTF-8 字节逐个百分号编码（大写十六进制），因此非 ASCII 字符会变成多个 `%XX`
/// （如 `ü` → `%C3%BC`），与 JS 先做 UTF-8 编码再转义的语义一致；`/` 也会被编码，
/// 调用方需要保留路径分隔符时要自己分段（`details` 就是这样处理的）。
fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(byte as char);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    out
}

/// 进程环境（`None` 表示读取 `process.env`）。
///
/// `Some(map)` 时只查 map、不读进程环境（测试与需要确定性的调用方使用）；`None` 走
/// `std::env::var_os`，非 UTF-8 的值经 `to_string_lossy` 有损替换为 `String`。
fn env_value(env: Option<&indexmap::IndexMap<String, String>>, name: &str) -> Option<String> {
    match env {
        Some(values) => values.get(name).cloned(),
        None => std::env::var_os(name).map(|value| value.to_string_lossy().into_owned()),
    }
}

/// `readToken(path)`：读文件 + trim；任何错误 → `None`。
///
/// 用 [`kiana_json::js_string::js_trim`] 而不是 `str::trim`：JS 的 `String.prototype.trim`
/// 与 Rust 的空白字符集合不同（例如 U+FEFF 在 JS 里算空白）。文件不存在、无权限、不是
/// UTF-8 或 trim 后为空都折叠成 `None`，调用方无法区分这些情况，只会尝试下一个候选路径。
async fn read_token(path: &str) -> Option<String> {
    let token = tokio::fs::read_to_string(path).await.ok()?;
    let token = kiana_json::js_string::js_trim(&token);
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// `findHuggingFaceToken(env = process.env)`。
///
/// 顺序（去重、保序）：`HF_TOKEN`（trim 后非空）→ `HF_TOKEN_PATH` → `$HF_HOME/token` →
/// `$XDG_CACHE_HOME/huggingface/token` → `~/.cache/huggingface/token`。
///
/// `env = None` 读真实进程环境；`Some(map)` 只查该 map 里的 `HF_TOKEN`/`HF_TOKEN_PATH`/
/// `HF_HOME`/`XDG_CACHE_HOME`。注意最后的 `~/.cache/huggingface/token` 仍来自
/// `dirs::home_dir()`（读取真实的 home），传自定义 map 也绕不开它——与 TS 传自定义 `env`
/// 时仍调用 `homedir()` 一致。路径列表按 `new Set(paths)` 去重：同一路径出现多次只读一次，
/// 但相对顺序不变。拿不到 home 时最后一项被跳过。
/// 函数只做文件 IO，不返回错误——所有失败都表现为继续下一个候选，最终 `None`。
pub async fn find_huggingface_token(
    env: Option<&indexmap::IndexMap<String, String>>,
) -> Option<String> {
    if let Some(from_environment) = env_value(env, "HF_TOKEN") {
        let trimmed = kiana_json::js_string::js_trim(&from_environment);
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let mut paths: Vec<String> = Vec::new();
    if let Some(path) = env_value(env, "HF_TOKEN_PATH")
        && !path.is_empty()
    {
        paths.push(path);
    }
    if let Some(home) = env_value(env, "HF_HOME")
        && !home.is_empty()
    {
        paths.push(
            std::path::Path::new(&home)
                .join("token")
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(cache) = env_value(env, "XDG_CACHE_HOME")
        && !cache.is_empty()
    {
        paths.push(
            std::path::Path::new(&cache)
                .join("huggingface")
                .join("token")
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".cache")
                .join("huggingface")
                .join("token")
                .to_string_lossy()
                .into_owned(),
        );
    }

    // `new Set(paths)`：去重且保留首次出现顺序。
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    for path in paths {
        if let Some(token) = read_token(&path).await {
            return Some(token);
        }
    }
    None
}

/// 默认 `HttpClient`（与 llama client 相同策略）。
///
/// 进程内只构造一次并缓存（`OnceLock`），所有经 [`HuggingFaceClient::new`] 创建的客户端
/// 共享同一个 `SharedHttpClient`；`ReqwestHttpClient::new()` 构造失败时退回
/// `ReqwestHttpClient::default()`（与 `llama/client.rs` 的 `default_http_client` 同策略）。
fn default_http_client() -> SharedHttpClient {
    static DEFAULT: OnceLock<SharedHttpClient> = OnceLock::new();
    DEFAULT
        .get_or_init(|| match ReqwestHttpClient::new() {
            Ok(client) => client.shared(),
            Err(_) => Arc::new(ReqwestHttpClient::default()),
        })
        .clone()
}

/// Hugging Face API 客户端。
///
/// 三个字段：可选的 HF token、去掉尾斜杠的 API 根地址、共享 HTTP 客户端。`request` 用
/// 字符串拼接 `{base_url}{path}`，因此 `base_url` 不能以 `/` 结尾（构造函数负责裁剪）。
/// `Clone` 只复制字段（`SharedHttpClient` 是 `Arc`），克隆共享同一个底层 HTTP 客户端。
#[derive(Clone)]
pub struct HuggingFaceClient {
    token: Option<String>,
    base_url: String,
    http: SharedHttpClient,
}

// 手写 `Debug`：token 只显示是否设置（`<set>`），避免凭据进入日志或错误输出。
impl std::fmt::Debug for HuggingFaceClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HuggingFaceClient")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

impl HuggingFaceClient {
    /// `new HuggingFaceClient(token?, baseUrl = DEFAULT_HUGGING_FACE_URL)`。
    ///
    /// `token = None` 表示匿名请求（空字符串同样不会发送 `Authorization`，见 `request`）；
    /// `base_url = None` 用 [`DEFAULT_HUGGING_FACE_URL`]，传入时去掉末尾所有 `/`。
    /// 使用进程级共享的默认 HTTP 客户端。
    pub fn new(token: Option<String>, base_url: Option<&str>) -> Self {
        Self::with_http_client(token, base_url, default_http_client())
    }

    /// 注入 `HttpClient` 的构造。
    ///
    /// 测试与宿主自定义传输的入口；`base_url` 的裁剪规则同 [`HuggingFaceClient::new`]。
    pub fn with_http_client(
        token: Option<String>,
        base_url: Option<&str>,
        http: SharedHttpClient,
    ) -> Self {
        HuggingFaceClient {
            token,
            base_url: base_url
                .unwrap_or(DEFAULT_HUGGING_FACE_URL)
                .trim_end_matches('/')
                .to_string(),
            http,
        }
    }

    /// `request(path, signal?)`。
    ///
    /// 唯一的请求出口（`search`/`details` 都经过它）。成功条件是状态码 2xx；429 用限流
    /// 文案，其它状态优先用响应体顶层的 `error` 字符串。注意状态错误也被标为
    /// `AiErrorKind::Transport`（`llama/client.rs` 的同类错误是 `Other`），且 `name` 保持
    /// `AiError::new` 的缺省值 `"Error"`；现有调用方只读 `message` 与做子串连接判定。
    async fn request(&self, path: &str, signal: Option<&AbortScope>) -> Result<Value, AiError> {
        let mut headers = Vec::new();
        // TS: `if (this.token)`——空字符串是假值，不发 Authorization。
        if let Some(token) = self.token.as_deref().filter(|token| !token.is_empty()) {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        let token = signal.map(AbortScope::token);
        let request = HttpRequest {
            method: "GET".to_string(),
            url: format!("{}{}", self.base_url, path),
            headers,
            body: None,
            timeout_ms: Some(REQUEST_TIMEOUT_MS),
        };
        let response = self
            .http
            .execute(request, token.as_ref())
            .await
            .map_err(|error| AiError::new(AiErrorKind::Transport, error.message))?;
        // 先复制状态与响应头：`read_json_body` 会把 body 流消费掉。
        let status = response.response.status;
        let headers = response.response.headers.clone();
        let payload = read_json_body(response).await;
        if !(200..=299).contains(&status) {
            let fallback = format!("Hugging Face returned HTTP {status}");
            if status == 429 {
                return Err(AiError::new(
                    AiErrorKind::Transport,
                    rate_limit_message(
                        headers.get("retry-after").map(String::as_str),
                        headers.get("ratelimit").map(String::as_str),
                    ),
                ));
            }
            return Err(AiError::new(
                AiErrorKind::Transport,
                payload_error(&payload, fallback),
            ));
        }
        Ok(payload)
    }

    /// `search(query, signal?)`：`filter=gguf&sort=downloads&direction=-1&limit=20`。
    ///
    /// 查询串用 `application/x-www-form-urlencoded` 编码（与 TS `URLSearchParams` 一致，
    /// 空格编码为 `+`）。响应必须是 JSON 数组，否则返回 `AiErrorKind::Parse`；数组里的
    /// 非对象项、`id` 不是字符串的项被静默丢弃（TS `flatMap` 返回空数组），`downloads`
    /// 缺失或不是 JSON 数字时按 `0.0` 处理。
    pub async fn search(
        &self,
        query: &str,
        signal: Option<&AbortScope>,
    ) -> Result<Vec<HuggingFaceModel>, AiError> {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("search", query);
        serializer.append_pair("filter", "gguf");
        serializer.append_pair("sort", "downloads");
        serializer.append_pair("direction", "-1");
        serializer.append_pair("limit", "20");
        let payload = self
            .request(&format!("/api/models?{}", serializer.finish()), signal)
            .await?;
        let Some(entries) = payload.as_array() else {
            return Err(AiError::new(
                AiErrorKind::Parse,
                "Hugging Face returned invalid search results",
            ));
        };
        let mut models = Vec::new();
        for value in entries {
            let Some(object) = value.as_object() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            models.push(HuggingFaceModel {
                id: id.to_string(),
                downloads: object
                    .get("downloads")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            });
        }
        Ok(models)
    }

    /// `details(id, signal?)`。
    ///
    /// `id`（通常是 `owner/repo`）按 `/` 分段做 `encodeURIComponent` 后再用 `/` 拼回，
    /// 请求 `/api/models/{id}?blobs=true`（`blobs=true` 是响应里带 `siblings[].size`
    /// 的前提）。2xx 但 payload 不是对象或数组时返回 `AiErrorKind::Parse`；解析细节见
    /// `parse_details`。返回值的 `id` 优先用响应字段，缺失时回退到参数 `id`。
    pub async fn details(
        &self,
        id: &str,
        signal: Option<&AbortScope>,
    ) -> Result<HuggingFaceModelDetails, AiError> {
        // 逐段编码：`encodeURIComponent` 会把 `/` 编成 `%2F`，而仓库 id 的 `/` 必须保留。
        let encoded_id = id
            .split('/')
            .map(encode_uri_component)
            .collect::<Vec<_>>()
            .join("/");
        let payload = self
            .request(&format!("/api/models/{encoded_id}?blobs=true"), signal)
            .await?;
        // TS 只拒绝非对象/null；数组仍是 object（随后字段全部缺失）。
        if !payload.is_object() && !payload.is_array() {
            return Err(AiError::new(
                AiErrorKind::Parse,
                "Hugging Face returned invalid model details",
            ));
        }
        Ok(parse_details(id, &payload))
    }
}

/// 从 GGUF 文件名提取量化名（去掉 `-0000N-of-0000M` 分片后缀，匹配锚定结尾的量化模式）。
///
/// 前提：调用方已确认文件名以 `.gguf`（大小写不敏感）结尾，因此这里直接切掉末尾 5 个
/// ASCII 字节，不再重复校验。先剥分片后缀再匹配量化名；命中后统一转大写，所以同一量化的
/// 多个分片会得到同一个名字，由调用方累加大小。
fn quantization_from_filename(filename: &str) -> Option<String> {
    // 去掉结尾的 ".gguf"（5 个 ASCII 字节）。
    let stem = &filename[..filename.len() - 5];
    let stem = SHARD_SUFFIX_PATTERN.replace(stem, "");
    QUANTIZATION_PATTERN
        .captures(&stem)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_uppercase())
}

/// `details` 的响应解析（拆出来便于 golden 测试）。
///
/// 阶段：
/// 1. 遍历 `siblings`：只接受 `rfilename` 以 `.gguf` 结尾（大小写不敏感）的项，取路径
///    最后一段作为文件名，跳过 `mmproj*`（多模态投影权重，不属于任一量化）；
/// 2. 按量化名累加 `size`（字节）。任一分片没有数字 `size` 就把该量化的 `complete`
///    置 false，最终 `size = None`——对外只有“完整总和”或“未知”，没有部分和；
/// 3. 排序：`Q4_K_M` 固定第一；其余按大小升序，未知大小当作 `MAX_SAFE_INTEGER`
///    （排最后），大小相等时用 `locale_compare` 复刻 JS 的 `localeCompare`；
/// 4. `gated` 只识别字符串 `"auto"`/`"manual"`，其余一律 `No`；`id` 缺失时回退到参数。
///
/// 不变量：`quantizations` 里的名字唯一（来自 `IndexMap` 的键），每个名字最多一项；
/// 空的 `siblings`/非法条目都只是让列表为空，不会报错。
fn parse_details(id: &str, payload: &Value) -> HuggingFaceModelDetails {
    #[derive(Debug, Clone, Copy)]
    struct Size {
        total: f64,
        complete: bool,
    }
    let mut sizes: indexmap::IndexMap<String, Size> = indexmap::IndexMap::new();
    if let Some(siblings) = payload.get("siblings").and_then(Value::as_array) {
        for value in siblings {
            let Some(file) = value.as_object() else {
                continue;
            };
            let Some(rfilename) = file.get("rfilename").and_then(Value::as_str) else {
                continue;
            };
            if !rfilename.to_lowercase().ends_with(".gguf") {
                continue;
            }
            let filename = rfilename.rsplit('/').next().unwrap_or("");
            if filename.to_lowercase().starts_with("mmproj") {
                continue;
            }
            let Some(quantization) = quantization_from_filename(filename) else {
                continue;
            };
            let current = sizes.entry(quantization).or_insert(Size {
                total: 0.0,
                complete: true,
            });
            match file.get("size").and_then(Value::as_f64) {
                Some(size) => current.total += size,
                None => current.complete = false,
            }
        }
    }
    let mut quantizations: Vec<HuggingFaceQuantization> = sizes
        .into_iter()
        .map(|(name, size)| HuggingFaceQuantization {
            name,
            size: if size.complete {
                Some(size.total)
            } else {
                None
            },
        })
        .collect();
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    quantizations.sort_by(|left, right| {
        if left.name == "Q4_K_M" {
            return std::cmp::Ordering::Less;
        }
        if right.name == "Q4_K_M" {
            return std::cmp::Ordering::Greater;
        }
        let left_size = left.size.unwrap_or(MAX_SAFE_INTEGER);
        let right_size = right.size.unwrap_or(MAX_SAFE_INTEGER);
        let difference = left_size - right_size;
        if difference < 0.0 {
            std::cmp::Ordering::Less
        } else if difference > 0.0 {
            std::cmp::Ordering::Greater
        } else {
            kiana_tui::autocomplete::locale_compare(&left.name, &right.name)
        }
    });
    let gated = match payload.get("gated").and_then(Value::as_str) {
        Some("auto") => HuggingFaceGated::Auto,
        Some("manual") => HuggingFaceGated::Manual,
        _ => HuggingFaceGated::No,
    };
    HuggingFaceModelDetails {
        id: payload
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string(),
        gated,
        quantizations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_uri_components_like_javascript() {
        assert_eq!(encode_uri_component("owner/model"), "owner%2Fmodel");
        assert_eq!(encode_uri_component("a b+c"), "a%20b%2Bc");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(encode_uri_component("ü"), "%C3%BC");
    }

    #[test]
    fn parses_quantizations_and_gated_flag() {
        let payload = json!({
            "id": "owner/model-GGUF",
            "gated": "manual",
            "siblings": [
                { "rfilename": "model-Q5_K_M.gguf", "size": 6000 },
                { "rfilename": "model-Q4_K_M-00001-of-00002.gguf", "size": 2000 },
                { "rfilename": "model-Q4_K_M-00002-of-00002.gguf", "size": 3000 },
                { "rfilename": "mmproj-F16.gguf", "size": 1000 },
                { "rfilename": "sub/model-IQ2_M.gguf", "size": 100 }
            ]
        });
        let details = parse_details("fallback", &payload);
        assert_eq!(details.id, "owner/model-GGUF");
        assert_eq!(details.gated, HuggingFaceGated::Manual);
        assert_eq!(
            details.quantizations,
            vec![
                HuggingFaceQuantization {
                    name: "Q4_K_M".to_string(),
                    size: Some(5000.0),
                },
                HuggingFaceQuantization {
                    name: "IQ2_M".to_string(),
                    size: Some(100.0),
                },
                HuggingFaceQuantization {
                    name: "Q5_K_M".to_string(),
                    size: Some(6000.0),
                },
            ]
        );
    }

    #[test]
    fn missing_sizes_sort_last_and_stay_unknown() {
        let payload = json!({
            "siblings": [
                { "rfilename": "a-Q8_0.gguf" },
                { "rfilename": "b-Q4_K_M.gguf", "size": 10 }
            ]
        });
        let details = parse_details("x", &payload);
        assert_eq!(details.id, "x");
        assert_eq!(details.gated, HuggingFaceGated::No);
        assert_eq!(details.quantizations[0].name, "Q4_K_M");
        assert_eq!(details.quantizations[0].size, Some(10.0));
        assert_eq!(details.quantizations[1].name, "Q8_0");
        assert_eq!(details.quantizations[1].size, None);
    }

    #[test]
    fn extracts_quantization_names_like_the_ts_regex() {
        // golden values generated from `client.ts` 的 QUANTIZATION_PATTERN（Node 26）。
        let cases = [
            ("model-Q4_K_M.gguf", Some("Q4_K_M")),
            ("model-UD-Q4_K_XL.gguf", Some("UD-Q4_K_XL")),
            ("model-IQ2_M.gguf", Some("IQ2_M")),
            ("model-BF16.gguf", Some("BF16")),
            ("model-F16.gguf", Some("F16")),
            ("model-MXFP4.gguf", Some("MXFP4")),
            ("model-MXFP4_MOE.gguf", Some("MXFP4_MOE")),
            ("qwen2.5-coder-7b-instruct-q4_k_m.gguf", Some("Q4_K_M")),
            ("model.Q5_K_S.gguf", Some("Q5_K_S")),
            ("foo_Q4_0_extra.gguf", Some("Q4_0_EXTRA")),
            ("model-IQ4_XS.gguf", Some("IQ4_XS")),
            ("model-Q4_K_M-00001-of-00003.gguf", Some("Q4_K_M")),
            ("model-00001-of-00003.gguf", None),
            ("model-F32.gguf", Some("F32")),
            ("model-MXFP8_E4M3.gguf", Some("MXFP8_E4M3")),
            ("x.gguf", None),
            ("model-Q2_K.gguf", Some("Q2_K")),
            ("model-Q4_1.gguf", Some("Q4_1")),
        ];
        for (filename, expected) in cases {
            assert_eq!(
                quantization_from_filename(filename).as_deref(),
                expected,
                "filename: {filename}"
            );
        }
    }

    #[tokio::test]
    async fn finds_token_files_in_configured_order() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = directory.path().join("first-token");
        let second = directory.path().join("second-token");
        tokio::fs::write(&first, "  first-token\n")
            .await
            .expect("write first");
        tokio::fs::write(&second, "second-token\n")
            .await
            .expect("write second");
        let mut env = indexmap::IndexMap::new();
        env.insert("HF_TOKEN".to_string(), "   ".to_string());
        env.insert(
            "HF_TOKEN_PATH".to_string(),
            first.to_string_lossy().into_owned(),
        );
        env.insert(
            "HF_HOME".to_string(),
            directory.path().to_string_lossy().into_owned(),
        );
        assert_eq!(
            find_huggingface_token(Some(&env)).await.as_deref(),
            Some("first-token")
        );
        env.shift_remove("HF_TOKEN_PATH");
        tokio::fs::write(directory.path().join("token"), "home-token\n")
            .await
            .expect("write home token");
        assert_eq!(
            find_huggingface_token(Some(&env)).await.as_deref(),
            Some("home-token"),
            "HF_HOME/token must be checked before later fallbacks"
        );
        let mut env = indexmap::IndexMap::new();
        env.insert(
            "XDG_CACHE_HOME".to_string(),
            directory.path().to_string_lossy().into_owned(),
        );
        tokio::fs::create_dir_all(directory.path().join("huggingface"))
            .await
            .expect("mkdir");
        tokio::fs::write(directory.path().join("huggingface/token"), "x")
            .await
            .expect("write xdg token");
        assert_eq!(
            find_huggingface_token(Some(&env)).await.as_deref(),
            Some("x")
        );
    }

    #[test]
    fn parses_rate_limit_delay() {
        assert_eq!(parse_rate_limit_delay(Some("t=30")), Some(30.0));
        assert_eq!(parse_rate_limit_delay(Some("foo;t=12")), Some(12.0));
        assert_eq!(parse_rate_limit_delay(Some("t=x")), None);
        assert_eq!(parse_rate_limit_delay(None), None);
    }

    /// 评审回归：`delay ? … : …` 的 JS truthiness——解析为 0 的 delay 用通用文案
    /// （期望值用 Node 26 跑 `request()` 的 429 分支逐字验证）。
    #[test]
    fn rate_limit_message_uses_generic_text_for_a_zero_delay() {
        assert_eq!(
            rate_limit_message(Some("0"), Some("t=0")),
            "Hugging Face rate limit reached"
        );
        assert_eq!(
            rate_limit_message(Some("0"), None),
            "Hugging Face rate limit reached"
        );
        assert_eq!(
            rate_limit_message(None, Some("t=0")),
            "Hugging Face rate limit reached"
        );
        assert_eq!(
            rate_limit_message(Some("0"), Some("t=5")),
            "Hugging Face rate limit reached; retry in 5s"
        );
        assert_eq!(
            rate_limit_message(Some("2"), Some("t=0")),
            "Hugging Face rate limit reached; retry in 2s"
        );
        assert_eq!(
            rate_limit_message(None, Some("t=30")),
            "Hugging Face rate limit reached; retry in 30s"
        );
        // `Number("abc") || …` → NaN 为假值，继续看 ratelimit。
        assert_eq!(
            rate_limit_message(Some("abc"), Some("t=7")),
            "Hugging Face rate limit reached; retry in 7s"
        );
        assert_eq!(
            rate_limit_message(Some("abc"), None),
            "Hugging Face rate limit reached"
        );
    }
}
