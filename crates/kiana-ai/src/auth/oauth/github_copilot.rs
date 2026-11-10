//! auth/oauth/github-copilot.ts——GitHub Copilot OAuth 设备码流程。
//!
//! 登录后把 GitHub access token 换成 Copilot token（`expires_at` 来自服务端），再从 token 的
//! `proxy-ep` 推导每个凭证专属的 API base URL。设备流程前先询问 GitHub Enterprise 域名
//! （空 = github.com）。拿到账号模型目录后，对"内置 + 支持工具 + unconfigured"的模型尽力
//! 启用策略（限流预算内）。
//!
//! TS: `src/auth/oauth/github-copilot.ts`
//!
//! # 在系统中的位置
//!
//! - 调用方：[`GitHubCopilotOAuth`] 由 `crate::auth::oauth::load` 的
//!   `load_github_copilot_oauth` 惰性构造，并在 `providers::github_copilot` 中注册为
//!   `github-copilot` provider 的 OAuth 认证方式；集成测试直接构造本类型。
//! - 依赖：`device_code` 提供 RFC 8628 轮询与可注入的 [`AuthClock`]；`oauth` 模块提供
//!   [`OAuthFetch`] 传输抽象与 JS 语义工具（`js_trim`、`form_urlencode`、`object_field` 等）；
//!   `providers::catalog` 提供内置模型表。
//! - 凭证约定：`access` 是 Copilot token，`refresh` 是 GitHub access token（设备流程只返回
//!   这个 token，不返回 refresh token）；`extra["enterpriseUrl"]` 记录企业域，
//!   `extra["availableModelIds"]` 记录账号实际可用的模型 id。后者由
//!   `providers::github_copilot` 的 `filter_models` 读取以裁剪模型目录；前者供本模块在
//!   刷新与推导 base URL 时还原企业域。
//!
//! # 一次登录的执行流程
//!
//! 1. `login` 询问 GitHub Enterprise 域名（空白 = github.com），用 WHATWG hostname 规范化；
//! 2. `start_device_flow` POST `/login/device/code` 取设备码，校验字段与验证地址；
//! 3. 通过 `AuthEvent::DeviceCode` 把 user_code/verification_uri 交给宿主；
//! 4. `poll_for_github_access_token` 轮询 `/login/oauth/access_token` 直到拿到 GitHub access token；
//! 5. 用它 GET `/copilot_internal/v2/token` 换 Copilot token（`expires_at` 秒 → 毫秒，提前 5 分钟过期）；
//! 6. GET `/models` 解析账号模型目录（available / policy 两个集合）；
//! 7. 若存在待启用模型：先发 `AuthEvent::Progress`，再逐个 POST `/models/<id>/policy`；
//! 8. 把 available 与成功启用的模型 id 去重合并后写入凭证并返回。
//!
//! `refresh` 只执行第 5、6 步：换 token + 拉目录；目录请求不重试，也不会启用任何模型。
//!
//! # 重要约束
//!
//! - 传输（[`OAuthFetch`]）与墙钟（[`AuthClock`]）都可注入：生产用 reqwest + 系统时钟，
//!   测试注入脚本化 fetch 与虚拟时钟，才能精确断言 5s 单请求超时、500ms 指数退避与
//!   `Retry-After` 的"秒数 / HTTP-date"两种解释。实现里不要绕过这两个抽象直接做网络或读系统时间。
//! - 解析规则与错误文案逐字对齐 TS（`spec/ai-auth.md` §2.13），包括 `Number.parseFloat`
//!   的前缀语义、`Date.parse` 的完整语法、以及 JS 真值判断（空串 `error_description` 不追加）。
//!   改动这些文案会同时破坏跨实现一致性与测试断言。
//! - 取消信号 [`AbortScope`] 会传入设备码轮询器的睡眠与每轮轮询前检查、所有 HTTP 请求，
//!   以及策略启用重试的退避等待。轮询器在睡眠或轮询前观察到取消时报 "Login cancelled"；
//!   在途 HTTP 请求被中止时由传输层返回 AbortError（"This operation was aborted"），该错误
//!   原样上抛。策略启用批处理在中止后不再继续，并把中止错误上抛。注意
//!   `interaction.prompt(...)` 不带 per-prompt signal，挂起的输入不会被 [`AbortScope`]
//!   打断。
//!
//! [`AuthClock`]: crate::auth::oauth::device_code::AuthClock
//! [`OAuthFetch`]: crate::auth::oauth::OAuthFetch
//! [`AbortScope`]: crate::utils::abort::AbortScope

use std::sync::Arc;

use regex::Regex;
use serde_json::Value;

use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, OAuthFetchResponse, default_oauth_fetch, is_object_like, js_trim,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, LoginOptions, ModelAuth,
    OAuthAuth, OAuthCredential,
};
use crate::providers::catalog;
use crate::utils::abort::AbortScope;
use crate::utils::sleep::sleep;

/// `CLIENT_ID`（`atob("SXYxLmI1MDdhMDhjODdlY2ZlOTg=")`）。
///
/// GitHub OAuth App 的公开标识；设备码流程不需要 client secret（`spec/ai-auth.md` §4.3.4）。
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

/// `COPILOT_HEADERS`。
///
/// 换 token、拉目录、启用策略三类请求共用；书写顺序与大小写逐字对照 TS，`headers` 会按
/// 给定顺序追加到请求上。
const COPILOT_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];
/// `COPILOT_API_VERSION`。
///
/// `/models` 请求的 `X-GitHub-Api-Version`；固定日期版本（`spec/ai-auth.md` §4.3.4）。
const COPILOT_API_VERSION: &str = "2026-06-01";
/// catalog 表里 GitHub Copilot 的 provider id。
const COPILOT_PROVIDER_ID: &str = "github-copilot";
/// `fetchWithRateLimitRetry` 的单次请求超时（`AbortSignal.timeout(5000)`）。
///
/// 有重试总预算时实际超时取 `min(剩余预算, 5s)`，避免预算耗尽后仍多等 5 秒。
const REQUEST_TIMEOUT_MS: f64 = 5000.0;
/// 默认 base URL。
///
/// Individual 账号的 API 端点；同时是 `fetch_github_copilot_models` 判定是否启用策略回退
/// （`allow_policy_fallback`）的比较基准。
const DEFAULT_BASE_URL: &str = "https://api.individual.githubcopilot.com";

/// `proxy-ep=([^;]+)`。
///
/// 只取第一个匹配，且不校验 host 的字符集与形状（对齐 TS）；base URL 的可信度因此取决于
/// token 确实由 GitHub 下发。
static PROXY_EP_PATTERN: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"proxy-ep=([^;]+)").expect("valid proxy-ep pattern"));

/// `DeviceCodeResponse`。
///
/// `/login/device/code` 响应里本流程需要的五个字段；`interval` 可缺省（轮询器按 RFC 8628
/// 回退到 5s），`expires_in` 必填（秒）。这里不做任何钳制：`interval` 由轮询器 floor 到
/// 至少 1s；`expires_in` 直接换算成轮询 deadline，NaN 或非正数会让循环一次都不执行、立即
/// 以超时错误结束；两者都没有上限。
#[derive(Debug, Clone)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<f64>,
    expires_in: f64,
}

/// `RetryPolicy`。
///
/// `max_retries == 0` 表示不重试；`max_elapsed_ms == 0` 表示不设总预算。只有两者都 > 0 时
/// `fetch_with_rate_limit_retry` 才计算 deadline，并据此缩短单请求超时、放弃放不下的退避。
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    max_retries: u32,
    max_elapsed_ms: u64,
}

/// 模型目录解析结果（`parseGitHubCopilotModelCatalog`）。
///
/// - `available_model_ids`：账号在模型选择器语义下应展示的模型；
/// - `policy_model_ids`：值得尝试用 policy 端点启用的模型。
///
/// 两者都只包含通过 `tool_calls` 过滤的账号模型，顺序与服务端返回一致；两者可以相交
/// （picker 判据看 `model_picker_enabled`，policy 判据看 `policy.state == "unconfigured"`）。
#[derive(Debug, Clone, Default, PartialEq)]
struct ModelCatalog {
    available_model_ids: Vec<String>,
    policy_model_ids: Vec<String>,
}

/// `getUrls(domain)`。
///
/// 三个端点由同一域名拼出（scheme 固定 https）：设备码、access token、Copilot token。
/// `domain` 必须是 [`normalize_domain`] 的产物（纯 hostname），否则拼出的 URL 不可信。
struct Urls {
    device_code_url: String,
    access_token_url: String,
    copilot_token_url: String,
}

/// `getUrls(domain)`。
///
/// 纯字符串拼接，不做 URL 解析；企业域与 github.com 共用同一模板。
fn get_urls(domain: &str) -> Urls {
    Urls {
        device_code_url: format!("https://{domain}/login/device/code"),
        access_token_url: format!("https://{domain}/login/oauth/access_token"),
        copilot_token_url: format!("https://api.{domain}/copilot_internal/v2/token"),
    }
}

/// `normalizeDomain(input)`：WHATWG 规范化后的 hostname（小写、punycode、去掉端口/path）。
///
/// 先做 JS `trim`；空串 → `None`；不含 `://` 时补 `https://` 再交给 `url::Url`（WHATWG
/// 解析器），成功则返回 hostname。
///
/// 失败返回 `None` 是登录流程的校验依据：输入非空却解析失败时直接报
/// "Invalid GitHub Enterprise URL/domain"，避免把任意字符串拼进 `https://{domain}/...`。
/// 该函数也被 `copilotEnterpriseDomain` 复用来规范化从磁盘读回的 `enterpriseUrl`。
fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url = url::Url::parse(&candidate).ok()?;
    url.host_str().map(str::to_string)
}

/// `getBaseUrlFromToken(token)`：`proxy.xxx` → `api.xxx`。
///
/// 只做前缀替换；host 不以 `proxy.` 开头时原样使用。不校验 host 形状、不做 URL 规范化，
fn get_base_url_from_token(token: &str) -> Option<String> {
    let captures = PROXY_EP_PATTERN.captures(token)?;
    let proxy_host = captures.get(1)?.as_str();
    let api_host = proxy_host
        .strip_prefix("proxy.")
        .map(|rest| format!("api.{rest}"))
        .unwrap_or_else(|| proxy_host.to_string());
    Some(format!("https://{api_host}"))
}

/// `getGitHubCopilotBaseUrl(token?, enterpriseDomain?)`。
///
/// 优先级：token 里的 `proxy-ep` > 企业域（`https://copilot-api.{domain}`）>
/// [`DEFAULT_BASE_URL`]。企业域只在 token 不含 `proxy-ep` 时生效，因此不同凭证可能落到
/// 不同的 API 域；`to_auth` 也调用它，保证请求与目录使用同一域名。
fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(token) = token
        && let Some(url) = get_base_url_from_token(token)
    {
        return url;
    }
    if let Some(domain) = enterprise_domain {
        return format!("https://copilot-api.{domain}");
    }
    DEFAULT_BASE_URL.to_string()
}

/// `asRecord(value)`。
///
/// 只接受对象与数组（JS 里 `typeof` 为 `"object"`；数组也算），`null` 与标量返回 `None`，
/// 调用方据此丢弃非法条目。
fn as_record(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(_) | Value::Array(_) => Some(value),
        _ => None,
    }
}

/// `Object.hasOwn(GITHUB_COPILOT_MODELS, model.id)` 的 Rust 等价物：
/// 模型 id 是否在 `providers/catalog.rs` 的 `github-copilot` chat 表里。
///
/// 只查 chat 表（不含 image/classifier）；catalog 是 `build.rs` 从 vendored JSON 生成的静态表。
/// 服务端返回但本地目录没有的模型（如 "remote-only-model"）视为非内置，不会启用策略。
fn is_builtin_copilot_model(model_id: &str) -> bool {
    catalog::get_model(COPILOT_PROVIDER_ID, model_id).is_some()
}

/// `parseGitHubCopilotModelCatalog(raw, allowPolicyFallback)`。
///
/// 输入是 `/models` 的 JSON，输出两个有序模型 id 列表。规则顺序固定：
///
/// 1. `raw.data` 必须是数组，否则 `"Invalid Copilot models response"`；
/// 2. 逐条投影：`id` 非字符串的条目丢弃；`capabilities.supports.tool_calls === false` 的条目
///    丢弃（缺失 capabilities/policy 都按"未设置"处理，不丢）；
/// 3. `available` = `model_picker_enabled === true` 且 `policy.state != "disabled"`；
/// 4. 仅当 `allowPolicyFallback` 且 picker 列表为空时，`available` 回退为
///    `policy.state == "enabled"` 的模型（部分 Individual 账号不给 picker 标志）；
/// 5. `policy` = `policy.state == "unconfigured"` 且是内置模型、且（`model_picker_enabled`
///    为真或用了第 4 步的回退）的模型。
///
/// 失败模式只有第 1 步的形状错误；其余非法条目是逐条跳过而非整体报错。返回的两个列表
/// 允许相交：一个模型既可能被 picker 展示、也可能处于 unconfigured 待启用状态。
fn parse_github_copilot_model_catalog(
    raw: &Value,
    allow_policy_fallback: bool,
) -> Result<ModelCatalog, AuthError> {
    let data = as_record(raw).and_then(|raw| crate::auth::oauth::object_field(raw, "data"));
    let Some(Value::Array(data)) = data else {
        return Err(AuthError::new("Invalid Copilot models response"));
    };

    // 服务端条目中本流程用到的三个字段。
    struct AccountModel {
        id: String,
        picker_enabled: bool,
        policy_state: Option<String>,
    }

    // 第一步：把每条服务端模型投影成 AccountModel；id 非字符串或明确声明
    // tool_calls === false 的条目直接丢弃。其余字段缺失一律按"未设置"处理，不阻断整份目录。
    let account_models: Vec<AccountModel> = data
        .iter()
        .filter_map(|raw_item| {
            let item = as_record(raw_item)?;
            let id = crate::auth::oauth::object_field(item, "id")?.as_str()?;
            let capabilities =
                crate::auth::oauth::object_field(item, "capabilities").and_then(as_record);
            let supports = capabilities
                .and_then(|capabilities| crate::auth::oauth::object_field(capabilities, "supports"))
                .and_then(as_record);
            if supports
                .and_then(|supports| crate::auth::oauth::object_field(supports, "tool_calls"))
                .and_then(Value::as_bool)
                == Some(false)
            {
                return None;
            }
            Some(AccountModel {
                id: id.to_string(),
                picker_enabled: crate::auth::oauth::object_field(item, "model_picker_enabled")
                    .and_then(Value::as_bool)
                    == Some(true),
                policy_state: crate::auth::oauth::object_field(item, "policy")
                    .and_then(as_record)
                    .and_then(|policy| crate::auth::oauth::object_field(policy, "state"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();

    // 第二步：picker 语义；policy.state == "disabled" 明确禁用，即使 picker 标志为真也不展示。
    let picker_model_ids: Vec<String> = account_models
        .iter()
        .filter(|model| model.picker_enabled && model.policy_state.as_deref() != Some("disabled"))
        .map(|model| model.id.clone())
        .collect();
    // 只有"允许回退 + picker 为空"才切到 policy.enabled；非 Individual 端点
    // （allow_policy_fallback == false）永远保持严格 picker 语义，宁可返回空列表。
    let use_policy_fallback = allow_policy_fallback && picker_model_ids.is_empty();
    // 与 TS 的三元表达式同构：有 picker 结果或不允许回退 → 用 picker；否则用 policy.enabled。
    let available_model_ids = if !picker_model_ids.is_empty() || !allow_policy_fallback {
        picker_model_ids
    } else {
        account_models
            .iter()
            .filter(|model| model.policy_state.as_deref() == Some("enabled"))
            .map(|model| model.id.clone())
            .collect()
    };
    // 第三步：待启用集合。只碰"内置 + unconfigured + 会出现在 picker（或已走回退）"的模型，
    // 因此远端独有模型与用户不可见的模型不会收到 policy 请求。
    let policy_model_ids = account_models
        .iter()
        .filter(|model| {
            model.policy_state.as_deref() == Some("unconfigured")
                && is_builtin_copilot_model(&model.id)
                && (model.picker_enabled || use_policy_fallback)
        })
        .map(|model| model.id.clone())
        .collect();
    Ok(ModelCatalog {
        available_model_ids,
        policy_model_ids,
    })
}

/// `Number.parseFloat(value)`（前导空白 + 最长数值前缀；失败 → NaN）。
///
/// 用于解释 `Retry-After`：先尝试当秒数，失败再当 HTTP-date。语义边界与 JS 一致：
/// 前导空白按 ECMAScript WhiteSpace + LineTerminator 跳过；`Infinity` 单独处理；整数与
/// 小数部分合计至少要有一位数字；指数标记后没有数字时回退到指数前的前缀；`trimmed[..end]`
/// 的下标只落在 ASCII 扫描范围内，不会切断 UTF-8 字符。
fn parse_float(value: &str) -> f64 {
    // 跳过前导空白：字符集与 JS 的 WhiteSpace + LineTerminator 一致（含 BOM）。
    let trimmed = value.trim_start_matches(|c: char| {
        matches!(
            c,
            '\u{0009}'
                | '\u{000A}'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    });
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    if trimmed[end..].starts_with("Infinity") {
        return if trimmed.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let mut has_digits = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        has_digits = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            has_digits = true;
        }
    }
    if !has_digits {
        return f64::NAN;
    }
    // 指数部分：`e`/`E` 后必须跟至少一位数字，否则只保留指数前的前缀（parseFloat 语义）。
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let exponent_start = end;
        let mut probe = end + 1;
        if probe < bytes.len() && (bytes[probe] == b'+' || bytes[probe] == b'-') {
            probe += 1;
        }
        let digits_start = probe;
        while probe < bytes.len() && bytes[probe].is_ascii_digit() {
            probe += 1;
        }
        if probe > digits_start {
            end = probe;
        } else {
            end = exponent_start;
        }
    }
    trimmed[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// `Date.parse(value)`：V8 `DateParser` 的完整移植（含 HTTP-date、月份名、ISO 无偏移等）。
///
/// 失败 → NaN。实现见 [`crate::model_catalog::js_date_parse`]（与 `Date.parse` 逐条对齐）。
/// 这里只作为 `Retry-After` 的非数字回退；返回的是与 `Date.now()` 同基的绝对毫秒值，
/// 调用方再减去当前时间得到延迟。无时区的时间按进程本地时区解释（V8 行为）。
fn date_parse(value: &str) -> f64 {
    crate::model_catalog::js_date_parse(value).unwrap_or(f64::NAN)
}

/// 一次 `fetchWithRateLimitRetry` 的请求模板（`RequestInit` 的简化）。
///
/// 重试时从同一份模板逐轮重建请求（body 每轮 clone）；头不做去重或合并，传输层负责补
/// 默认头。`Default` 的方法/头/body 均为空，等价于一个无 body 的 GET 模板。
#[derive(Debug, Clone, Default)]
struct FetchInit {
    method: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl FetchInit {
    fn new() -> Self {
        FetchInit::default()
    }

    fn method(mut self, method: &str) -> Self {
        self.method = Some(method.to_string());
        self
    }

    fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    fn headers(mut self, headers: &[(&str, &str)]) -> Self {
        for (name, value) in headers {
            self.headers
                .push(((*name).to_string(), (*value).to_string()));
        }
        self
    }

    fn body(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self
    }
}

/// `fetchWithRateLimitRetry(url, init, signal, retryPolicy)`。
///
/// 只对 HTTP 429 做有限重试，其余状态原样返回给调用方判定。规则（`spec/ai-auth.md` §2.13）：
///
/// - 每个请求独立 5s 超时；若设置了总预算，超时取 `min(剩余预算, 5s)`（至少 1ms），
///   即同时受调用方 signal、预算 signal 与 5s 超时三者约束；
/// - 仅当 `status == 429 && retry < max_retries` 才进入退避；
/// - 退避延迟：默认 `500ms * 2^retry`；有非空 `Retry-After` 时先按 `Number.parseFloat`
///   当秒数，得到 NaN 再按 `Date.parse - now` 当 HTTP-date；
/// - 延迟是非有限值（NaN/±Infinity）或超出剩余预算时**不再重试**，直接把 429 响应返回
///   （错误文案由调用方生成）；负延迟（HTTP-date 在过去）钳到 0，立即重试；
/// - `fetch` 的传输错误与等待期间的中止都直接上抛，不重试。
async fn fetch_with_rate_limit_retry(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    url: &str,
    init: FetchInit,
    signal: &AbortScope,
    retry_policy: RetryPolicy,
) -> Result<OAuthFetchResponse, AuthError> {
    // 预算：两个参数都 > 0 才生效（对应 TS 的 `maxRetries > 0 && maxElapsedMs > 0`）。
    let retry_deadline = if retry_policy.max_retries > 0 && retry_policy.max_elapsed_ms > 0 {
        Some(clock.now_ms() + retry_policy.max_elapsed_ms as f64)
    } else {
        None
    };
    let mut retry = 0u32;
    loop {
        // TS 只传调用方的 `init.headers`（传输层自己补默认头）；不再额外塞 Accept。
        let mut request = OAuthFetchRequest::new(url, signal);
        request.method = init.method.clone().unwrap_or_else(|| "GET".to_string());
        request.headers.extend(init.headers.iter().cloned());
        request.body = init.body.clone();
        // 每个请求 5s 超时；重试预算更小时取剩余预算（`AbortSignal.any` 的等价物）。
        let timeout_ms = match retry_deadline {
            Some(deadline) => {
                let remaining = deadline - clock.now_ms();
                if remaining <= 0.0 {
                    1
                } else {
                    remaining.clamp(1.0, REQUEST_TIMEOUT_MS) as u64
                }
            }
            None => REQUEST_TIMEOUT_MS as u64,
        };
        request.timeout_ms = Some(timeout_ms);

        let response = fetch.fetch(request).await?;
        if response.status != 429 || retry == retry_policy.max_retries {
            return Ok(response);
        }

        // `if (retryAfter)`：空串在 TS 里是假值 → 走指数退避。
        let retry_after = response
            .header("retry-after")
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let mut delay_ms = 500.0 * 2f64.powi(retry as i32);
        if let Some(retry_after) = retry_after {
            let seconds = parse_float(&retry_after);
            delay_ms = if seconds.is_nan() {
                date_parse(&retry_after) - clock.now_ms()
            } else {
                seconds * 1000.0
            };
            // 非有限值意味着无法安排重试（TS 的 `!Number.isFinite`），把 429 原样返回。
            if !delay_ms.is_finite() {
                return Ok(response);
            }
        }
        delay_ms = if delay_ms.is_nan() {
            0.0
        } else {
            delay_ms.max(0.0)
        };
        // 延迟不能超过剩余预算；放不下就不重试（保留 429 响应，由调用方报错）。
        if let Some(deadline) = retry_deadline
            && delay_ms >= deadline - clock.now_ms()
        {
            return Ok(response);
        }
        // `await response.body?.cancel()`：丢弃响应即可释放连接。
        drop(response);
        sleep(crate::auth::oauth::delay_duration_ms(delay_ms), signal)
            .await
            .map_err(|error| AuthError::with_name(error.name, error.message))?;
        retry += 1;
    }
}

/// `fetchJson(url, init)`。
///
/// 单次请求、无重试、无内置超时（只受调用方 signal 约束）；非 2xx 的错误文案是
/// `"{status} {statusText}: {body}"`，上层把它原样包进 [`AuthError`]。
/// JSON 解析失败由 `response.json()` 返回 `SyntaxError` 命名的错误。
async fn fetch_json(
    fetch: &dyn OAuthFetch,
    url: &str,
    init: FetchInit,
    signal: &AbortScope,
) -> Result<Value, AuthError> {
    let mut request = OAuthFetchRequest::new(url, signal);
    request.method = init.method.clone().unwrap_or_else(|| "GET".to_string());
    request.headers = init.headers.clone();
    request.body = init.body.clone();
    let response = fetch.fetch(request).await?;
    if !response.ok() {
        let text = response.text();
        return Err(AuthError::new(format!(
            "{} {}: {text}",
            response.status, response.status_text
        )));
    }
    response.json()
}

/// `fetchGitHubCopilotModels(copilotToken, enterpriseDomain, signal, retryPolicy)`。
///
/// GET `{baseUrl}/models`，baseUrl 由 Copilot token 的 `proxy-ep` 推导（见
/// [`get_github_copilot_base_url`]）。`allowPolicyFallback` 只在 baseUrl 等于
/// [`DEFAULT_BASE_URL`]（Individual 账号）时为真，企业端点不启用 policy 回退。
///
/// 失败路径：重试预算耗尽仍是 429 时由这里生成 `"{status} {statusText}: {body}"` 错误；
/// 传输错误直接上抛；响应体不是合法 JSON 时返回 `SyntaxError`；目录形状非法由
/// [`parse_github_copilot_model_catalog`] 报错。
async fn fetch_github_copilot_models(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    copilot_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortScope,
    retry_policy: RetryPolicy,
) -> Result<ModelCatalog, AuthError> {
    let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
    // 部分 Individual 账号对所有 picker 标志都返回 false，但仍显式启用策略；
    // 只在该端点启用回退，其它账号保持严格 picker 语义。
    let allow_policy_fallback = base_url == DEFAULT_BASE_URL;
    let init = FetchInit::new()
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {copilot_token}"))
        .headers(COPILOT_HEADERS)
        .header("X-GitHub-Api-Version", COPILOT_API_VERSION);
    let response = fetch_with_rate_limit_retry(
        fetch,
        clock,
        &format!("{base_url}/models"),
        init,
        signal,
        retry_policy,
    )
    .await?;
    if !response.ok() {
        return Err(AuthError::new(format!(
            "{} {}: {}",
            response.status,
            response.status_text,
            response.text()
        )));
    }
    let raw = response.json()?;
    parse_github_copilot_model_catalog(&raw, allow_policy_fallback)
}

/// `startDeviceFlow(domain, signal)`。
///
/// POST `/login/device/code`（表单 `client_id` + `scope=read:user`），返回设备码。
///
/// 校验分两层：先确认响应是对象且五个字段类型正确（`device_code`/`user_code`/
/// `verification_uri` 为字符串，`interval` 缺省或数字，`expires_in` 为数字），再要求
/// `verification_uri` 能解析成 URL 且 scheme 为 http/https。后者是安全边界：该地址会交给
/// 宿主用系统浏览器打开，不能是 `file:`/`javascript:` 或裸命令。
/// 两条 URI 校验失败路径共用同一句文案。
async fn start_device_flow(
    fetch: &dyn OAuthFetch,
    domain: &str,
    signal: &AbortScope,
) -> Result<DeviceCodeResponse, AuthError> {
    let urls = get_urls(domain);
    let init = FetchInit::new()
        .method("POST")
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .body(
            crate::auth::oauth::form_urlencode(&[("client_id", CLIENT_ID), ("scope", "read:user")])
                .into_bytes(),
        );
    let data = fetch_json(fetch, &urls.device_code_url, init, signal).await?;

    if !is_object_like(&data) {
        return Err(AuthError::new("Invalid device code response"));
    }
    let device_code = crate::auth::oauth::object_field(&data, "device_code");
    let user_code = crate::auth::oauth::object_field(&data, "user_code");
    let verification_uri = crate::auth::oauth::object_field(&data, "verification_uri");
    let interval = crate::auth::oauth::object_field(&data, "interval");
    let expires_in = crate::auth::oauth::object_field(&data, "expires_in");

    // 这里只校验类型不校验非空：空的 device_code 会被原样放进轮询请求发给服务端；
    // 空的 user_code 只进入后面的 AuthEvent::DeviceCode 供宿主展示；空的
    // verification_uri 在下面 URL 解析处失败。
    let valid = device_code.is_some_and(Value::is_string)
        && user_code.is_some_and(Value::is_string)
        && verification_uri.is_some_and(Value::is_string)
        && interval.is_none_or(Value::is_number)
        && expires_in.is_some_and(Value::is_number);
    if !valid {
        return Err(AuthError::new("Invalid device code response fields"));
    }

    // 验证 URI 会被浏览器打开；强制为 URL，防止 `open` 执行任意程序。
    let raw_verification_uri = verification_uri.and_then(Value::as_str).unwrap_or_default();
    let parsed_uri = url::Url::parse(raw_verification_uri)
        .map_err(|_| AuthError::new("Untrusted verification_uri in device code response"))?;
    if parsed_uri.scheme() != "https" && parsed_uri.scheme() != "http" {
        return Err(AuthError::new(
            "Untrusted verification_uri in device code response",
        ));
    }

    Ok(DeviceCodeResponse {
        device_code: device_code
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        user_code: user_code
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        verification_uri: parsed_uri.into(),
        interval: interval.and_then(Value::as_f64),
        expires_in: expires_in.and_then(Value::as_f64).unwrap_or(f64::NAN),
    })
}

/// `pollForGitHubAccessToken(domain, device, signal)`。
///
/// 把"一次轮询"封装成闭包交给 [`poll_oauth_device_code_flow_with_clock`]，由后者负责
/// 间隔、`slow_down` 递增与超时。轮询结果映射：
///
/// - 响应含字符串 `access_token` → `Complete`（拿到的是 GitHub access token）；
/// - `error == "authorization_pending"` → `Pending`；
/// - `error == "slow_down"` → `SlowDown { interval_seconds }`（`interval` 非数字 → `None`，
///   轮询器按 RFC 8628 加 5s）；
/// - 其它 `error` → `Failed("Device flow failed: <error[: description]>")`；
/// - 两者都没有 → `Failed("Invalid device token response")`。
///
/// 业务状态（pending/slow_down）与业务失败（其它 `error`、缺少 `access_token`）都以 `Ok`
/// 返回；`Err` 只来自传输错误、非 2xx HTTP 响应（`fetch_json` 生成
/// `"{status} {statusText}: {body}"`）与 JSON 解析失败，这符合 device_code 的接口约定。
/// 闭包必须 `Send + Sync + 'static` 且可重复调用，因此每轮都重新 clone
/// `fetch`/`signal`/`device` 并在内部重建请求。
async fn poll_for_github_access_token(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    domain: String,
    device: DeviceCodeResponse,
    signal: &AbortScope,
) -> Result<String, AuthError> {
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<String> {
            let fetch = Arc::clone(&fetch);
            let urls = get_urls(&domain);
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                let init = FetchInit::new()
                    .method("POST")
                    .header("Accept", "application/json")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .header("User-Agent", "GitHubCopilotChat/0.35.0")
                    .body(
                        crate::auth::oauth::form_urlencode(&[
                            ("client_id", CLIENT_ID),
                            ("device_code", &device.device_code),
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ])
                        .into_bytes(),
                    );
                let raw = fetch_json(fetch.as_ref(), &urls.access_token_url, init, &signal).await?;

                if is_object_like(&raw)
                    && let Some(access_token) =
                        crate::auth::oauth::object_field(&raw, "access_token")
                            .and_then(Value::as_str)
                {
                    return Ok(OAuthDeviceCodePollResult::Complete {
                        value: access_token.to_string(),
                    });
                }

                if is_object_like(&raw)
                    && let Some(error) =
                        crate::auth::oauth::object_field(&raw, "error").and_then(Value::as_str)
                {
                    // TS 不解构类型：`description ? `: ${description}` : ""` 只看真值，
                    // 非字符串的真值也会被模板字符串 `String()` 化。
                    let description = crate::auth::oauth::object_field(&raw, "error_description");
                    if error == "authorization_pending" {
                        return Ok(OAuthDeviceCodePollResult::Pending);
                    }
                    if error == "slow_down" {
                        let interval = crate::auth::oauth::object_field(&raw, "interval")
                            .and_then(Value::as_f64);
                        return Ok(OAuthDeviceCodePollResult::SlowDown {
                            interval_seconds: interval,
                        });
                    }
                    let description_suffix = match description {
                        Some(value) if kiana_json::is_truthy(Some(value)) => {
                            format!(": {}", kiana_json::js_string::js_string(Some(value)))
                        }
                        _ => String::new(),
                    };
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!("Device flow failed: {error}{description_suffix}"),
                    });
                }

                Ok(OAuthDeviceCodePollResult::Failed {
                    message: "Invalid device token response".into(),
                })
            })
        }
    };

    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds: device.interval,
            expires_in_seconds: Some(device.expires_in),
            wait_before_first_poll: true,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// `refreshGitHubCopilotAccessToken(refreshToken, enterpriseDomain, signal)`。
///
/// GET `/copilot_internal/v2/token`，`Authorization: Bearer <refreshToken>`；这里的
/// refreshToken 实际是 GitHub access token（设备流程的产物），返回 Copilot token 凭证。
///
/// - `domain` 缺省 github.com，企业域只改变请求主机；
/// - `expires_at` 是 Unix 秒；凭证 `expires` 转成毫秒后减去 5 分钟，即比服务端过期时间
///   提前 5 分钟；
/// - 有企业域时写入 `extra["enterpriseUrl"]`，供 `refresh`/`to_auth` 下次还原域名；缺省
///   时不写该键（与 TS 的 `undefined` 序列化时被丢弃等价）；
/// - 非对象或字段类型不符 → 固定文案错误；本函数不重试。
async fn refresh_github_copilot_access_token(
    fetch: &dyn OAuthFetch,
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let domain = enterprise_domain.unwrap_or("github.com");
    let urls = get_urls(domain);
    let init = FetchInit::new()
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {refresh_token}"))
        .headers(COPILOT_HEADERS);
    let raw = fetch_json(fetch, &urls.copilot_token_url, init, signal).await?;

    if !is_object_like(&raw) {
        return Err(AuthError::new("Invalid Copilot token response"));
    }
    let token = crate::auth::oauth::object_field(&raw, "token").and_then(Value::as_str);
    let expires_at = crate::auth::oauth::object_field(&raw, "expires_at").and_then(Value::as_f64);
    let (Some(token), Some(expires_at)) = (token, expires_at) else {
        return Err(AuthError::new("Invalid Copilot token response fields"));
    };

    let mut credential = OAuthCredential::new(
        token,
        refresh_token,
        expires_at * 1000.0 - 5.0 * 60.0 * 1000.0,
    );
    if let Some(enterprise_domain) = enterprise_domain {
        credential.extra.insert(
            "enterpriseUrl".into(),
            Value::String(enterprise_domain.to_string()),
        );
    }
    Ok(credential)
}

/// `refreshGitHubCopilotToken(refreshToken, enterpriseDomain, signal)`。
///
/// 换 token 后立即拉一次模型目录，把 `available_model_ids` 写入
/// `extra["availableModelIds"]`。目录请求使用 `max_retries: 0`：刷新路径不重试限流
/// （测试断言 429 时只请求一次），错误直接上抛，由上层决定如何处理刷新失败。
/// 刷新**不会**启用任何模型，也不使用 `policy_model_ids`。
async fn refresh_github_copilot_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let credentials =
        refresh_github_copilot_access_token(fetch, refresh_token, enterprise_domain, signal)
            .await?;
    // 刷新不重试（测试断言限流时只请求一次）。
    let models = fetch_github_copilot_models(
        fetch,
        clock,
        &credentials.access,
        enterprise_domain,
        signal,
        RetryPolicy {
            max_retries: 0,
            max_elapsed_ms: 0,
        },
    )
    .await?;
    let mut credential = credentials;
    credential.extra.insert(
        "availableModelIds".into(),
        Value::Array(
            models
                .available_model_ids
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    Ok(credential)
}

/// `enableGitHubCopilotModel(token, modelId, enterpriseDomain, signal)`。
///
/// POST `{baseUrl}/models/{modelId}/policy`，body 为 `{"state":"enabled"}`（用 JS 的
/// `JSON.stringify` 生成，输出为当前稳定契约，改动需同步测试）；重试预算 2 次 / 5s。
///
/// 返回值区分三种结局：
/// - `Ok(true)`：最终响应 2xx；
/// - `Ok(false)`：未启用但可以继续下一个模型——非 429 的非 2xx，或未中止时的传输错误；
/// - `Err`：重试预算耗尽仍是 429（文案含状态码与响应体），或调用方已中止（原样上抛）。
async fn enable_github_copilot_model(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    token: &str,
    model_id: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortScope,
) -> Result<bool, AuthError> {
    let base_url = get_github_copilot_base_url(Some(token), enterprise_domain);
    let url = format!("{base_url}/models/{model_id}/policy");
    let init = FetchInit::new()
        .method("POST")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .headers(COPILOT_HEADERS)
        .header("openai-intent", "chat-policy")
        .header("x-interaction-type", "chat-policy")
        .body(
            kiana_json::js_object::stringify(&crate::auth::oauth::json_object(vec![(
                "state",
                Value::String("enabled".into()),
            )]))
            .into_bytes(),
        );
    let response = match fetch_with_rate_limit_retry(
        fetch,
        clock,
        &url,
        init,
        signal,
        RetryPolicy {
            max_retries: 2,
            max_elapsed_ms: 5000,
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            if signal.is_aborted() {
                return Err(error);
            }
            return Ok(false);
        }
    };
    if response.status == 429 {
        return Err(AuthError::new(format!(
            "{} {}: {}",
            response.status,
            response.status_text,
            response.text()
        )));
    }
    Ok(response.ok())
}

/// `enableGitHubCopilotModels(token, modelIds, enterpriseDomain, signal)`。
///
/// 顺序逐个启用（不并发），返回成功启用的 id（保持输入顺序）。遇到第一个返回 `Err` 的
/// 模型（重试预算耗尽仍是 429，或请求/退避等待因 signal 中止而失败）时：未中止 → 停止
/// 批处理但保留已启用结果（登录仍算成功）；已中止 → 立即把该错误上抛。传输错误与普通
/// 非 2xx 响应在未中止时由 [`enable_github_copilot_model`] 转成 `Ok(false)`，只跳过该模型
/// 并继续下一个（`tests/github_copilot_oauth.rs` 的
/// `continues_policy_updates_after_a_fetch_error` 锁定该行为）。
async fn enable_github_copilot_models(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    token: &str,
    model_ids: &[String],
    enterprise_domain: Option<&str>,
    signal: &AbortScope,
) -> Result<Vec<String>, AuthError> {
    let mut enabled_model_ids = Vec::new();
    for model_id in model_ids {
        match enable_github_copilot_model(fetch, clock, token, model_id, enterprise_domain, signal)
            .await
        {
            Ok(true) => enabled_model_ids.push(model_id.clone()),
            Ok(false) => {}
            Err(error) => {
                if signal.is_aborted() {
                    return Err(error);
                }
                break;
            }
        }
    }
    Ok(enabled_model_ids)
}

/// `loginGitHubCopilot(interaction)`。
///
/// 完整登录编排：企业域 → 设备码 → 轮询 GitHub access token → 换 Copilot token →
/// 拉模型目录 → 启用待启用模型。语义要点：
///
/// - 输入 `trim` 后为空 → github.com；非空但无法规范化 → "Invalid GitHub Enterprise URL/domain"；
/// - prompt 不带 per-prompt signal，整个流程只受 `interaction.signal()` 控制；
/// - `DeviceCode` 事件在轮询前发出，`open_browser_on_confirm`/`copy_code_on_confirm` 不指定；
/// - 只有存在 `policy_model_ids` 时才发 `Progress("Enabling models...")`；
/// - 最终 `availableModelIds` = 目录 available + 成功启用，按首次出现顺序去重；
/// - 中止（设备轮询、换 token、目录、策略任一步）都会以中止错误结束登录。
async fn login_github_copilot(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    interaction: &dyn AuthInteraction,
) -> Result<OAuthCredential, AuthError> {
    let signal = interaction.signal();
    let input = interaction
        .prompt(AuthPromptRequest::new(AuthPrompt::Text {
            message: "GitHub Enterprise URL/domain (blank for github.com)".into(),
            placeholder: Some("company.ghe.com".into()),
        }))
        .await?;
    // prompt 可能因取消而失败；这里再检查一次 signal，保持与 TS 相同的检查点。
    if signal.is_aborted() {
        return Err(AuthError::new("Login cancelled"));
    }

    // 空输入合法（表示 github.com）；非空却解析不出 hostname 才是错误。
    let trimmed = js_trim(&input);
    let enterprise_domain = normalize_domain(&input);
    if !trimmed.is_empty() && enterprise_domain.is_none() {
        return Err(AuthError::new("Invalid GitHub Enterprise URL/domain"));
    }
    let domain = enterprise_domain
        .clone()
        .unwrap_or_else(|| "github.com".to_string());

    let device = start_device_flow(fetch.as_ref(), &domain, signal).await?;
    // 设备码事件先于轮询发出：宿主据此展示用户码并打开验证页。两个 on_confirm 选项不传，
    // 表示由宿主采用默认行为。
    interaction.notify(&AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
    });

    let github_access_token = poll_for_github_access_token(
        Arc::clone(&fetch),
        Arc::clone(&clock),
        domain.clone(),
        device,
        signal,
    )
    .await?;
    let credentials = refresh_github_copilot_access_token(
        fetch.as_ref(),
        &github_access_token,
        enterprise_domain.as_deref(),
        signal,
    )
    .await?;
    let models = fetch_github_copilot_models(
        fetch.as_ref(),
        clock.as_ref(),
        &credentials.access,
        enterprise_domain.as_deref(),
        signal,
        RetryPolicy {
            max_retries: 2,
            max_elapsed_ms: 5000,
        },
    )
    .await?;
    let mut enabled_model_ids = Vec::new();
    // 只有存在待启用模型才提示进度。普通失败（Ok(false)）只跳过该模型；返回 Err 时若
    // signal 未中止则只截断批处理；这两种情况都不会让整个登录失败（中止错误会立即上抛）。
    if !models.policy_model_ids.is_empty() {
        interaction.notify(&AuthEvent::Progress {
            message: "Enabling models...".into(),
        });
        enabled_model_ids = enable_github_copilot_models(
            fetch.as_ref(),
            clock.as_ref(),
            &credentials.access,
            &models.policy_model_ids,
            enterprise_domain.as_deref(),
            signal,
        )
        .await?;
    }

    // 按首次出现顺序去重：
    // 目录里的模型在前，新启用的追加在后。
    let mut available_model_ids: Vec<String> = Vec::new();
    for model_id in models
        .available_model_ids
        .into_iter()
        .chain(enabled_model_ids)
    {
        if !available_model_ids.contains(&model_id) {
            available_model_ids.push(model_id);
        }
    }
    let mut credential = credentials;
    credential.extra.insert(
        "availableModelIds".into(),
        Value::Array(available_model_ids.into_iter().map(Value::String).collect()),
    );
    Ok(credential)
}

/// `copilotEnterpriseDomain(credential)`。
///
/// 读取 `extra["enterpriseUrl"]` 并重新规范化：凭证可能来自磁盘（`auth.json`），不保证仍是
/// 规范化形式。缺失或空串 → `None`；存在但解析失败 → `None`（随后 base URL 回落到
/// token 里的 `proxy-ep` 或默认值）。`refresh` 与 `to_auth` 用它还原企业域。
fn copilot_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    let enterprise_url = credential.enterprise_url()?;
    if enterprise_url.is_empty() {
        return None;
    }
    normalize_domain(enterprise_url)
}

/// GitHub Copilot OAuth 流程。
///
/// 实现 `OAuthAuth`：`login` 走设备码 + 模型策略；`refresh` 只换 token + 拉目录；`to_auth`
/// 无副作用地从凭证推导 `api_key` 与 base URL。两个依赖都以 `Arc<dyn ...>` 保存，实例可
/// 跨线程共享；`new()` 用默认 reqwest 传输与系统时钟，其余构造器供测试注入。
pub struct GitHubCopilotOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl GitHubCopilotOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    ///
    /// `default_oauth_fetch()` 在测试里可被 `set_default_oauth_fetch_for_tests` 顶替。
    pub fn new() -> Self {
        GitHubCopilotOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        GitHubCopilotOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    ///
    /// 测试用它注入脚本化 fetch 与虚拟时钟，精确控制超时/退避时序并断言请求头与 body。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        GitHubCopilotOAuth { fetch, clock }
    }
}

impl Default for GitHubCopilotOAuth {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    /// 交互式登录：企业域 → 设备码 → 轮询 → 换 Copilot token → 拉目录 → 启用策略。
    ///
    /// `options` 未使用：本流程不需要 device id / agent name，企业域来自交互提示。取消由
    /// `interaction.signal()` 控制。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        login_github_copilot(
            Arc::clone(&self.fetch),
            Arc::clone(&self.clock),
            interaction,
        )
        .await
    }

    /// 用凭证的 `refresh`（GitHub access token）换新 Copilot token，并重新拉取模型目录。
    ///
    /// 企业域取自 `extra["enterpriseUrl"]`；目录请求不重试；任一网络/解析失败都会让整个
    /// 刷新失败（`Models` 层在 store 锁内调用它）。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        refresh_github_copilot_token(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &credential.refresh,
            copilot_enterprise_domain(credential).as_deref(),
            signal,
        )
        .await
    }

    /// 无副作用：`api_key` 用 Copilot token，`base_url` 按 token 的 `proxy-ep` 推导
    /// （无 `proxy-ep` 时回落到企业域/Individual 默认）。
    ///
    /// 过期判断不在这里做：调用方应先依据 `credential.expires` 决定是否需要 `refresh`。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            base_url: Some(get_github_copilot_base_url(
                Some(&credential.access),
                copilot_enterprise_domain(credential).as_deref(),
            )),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 每次调用都新建实例；默认传输底层的 HTTP client 由 `oauth` 模块的进程级 `LazyLock` 复用。
pub fn github_copilot_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(GitHubCopilotOAuth::new())
}

/// `getGitHubCopilotBaseUrl` 的公开别名（provider 的 toAuth 也可直接用）。
///
/// 对 `get_github_copilot_base_url` 的转发，语义完全一致：token > 企业域 > 默认值。
pub fn github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    get_github_copilot_base_url(token, enterprise_domain)
}

/// `normalizeDomain` 的公开别名。
///
/// 对 `normalize_domain` 的转发：返回 WHATWG hostname，失败为 `None`。
pub fn normalize_github_copilot_domain(input: &str) -> Option<String> {
    normalize_domain(input)
}

/// 纯函数单测：base URL 推导、域名规范化、目录解析、`parseFloat`/`Date.parse` 语义。
///
/// 登录/刷新/策略启用的端到端断言（含虚拟时钟下的退避时序）在
/// `tests/github_copilot_oauth.rs`。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_from_token_replaces_proxy_prefix() {
        assert_eq!(
            get_base_url_from_token("tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest")
                .as_deref(),
            Some("https://api.enterprise.example")
        );
        assert_eq!(get_base_url_from_token("no-proxy-ep"), None);
    }

    #[test]
    fn base_url_falls_back_to_enterprise_then_individual() {
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), Some("company.ghe.com")),
            "https://copilot-api.company.ghe.com"
        );
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), None),
            "https://api.individual.githubcopilot.com"
        );
    }

    #[test]
    fn normalize_domain_uses_whatwg_hostnames() {
        assert_eq!(
            normalize_domain("company.ghe.com").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(
            normalize_domain("https://Company.GHE.com:8443/x").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(normalize_domain("   "), None);
        assert_eq!(normalize_domain("$(id>/tmp/pwned)"), None);
    }

    #[test]
    fn parses_the_picker_catalog() {
        let raw = crate::auth::oauth::json_object(vec![(
            "data",
            Value::Array(vec![
                crate::auth::oauth::json_object(vec![
                    ("id", Value::String("picker".into())),
                    ("model_picker_enabled", Value::Bool(true)),
                    (
                        "capabilities",
                        crate::auth::oauth::json_object(vec![(
                            "supports",
                            crate::auth::oauth::json_object(vec![(
                                "tool_calls",
                                Value::Bool(true),
                            )]),
                        )]),
                    ),
                ]),
                crate::auth::oauth::json_object(vec![
                    ("id", Value::String("disabled".into())),
                    ("model_picker_enabled", Value::Bool(true)),
                    (
                        "policy",
                        crate::auth::oauth::json_object(vec![(
                            "state",
                            Value::String("disabled".into()),
                        )]),
                    ),
                ]),
                crate::auth::oauth::json_object(vec![
                    ("id", Value::String("hidden".into())),
                    ("model_picker_enabled", Value::Bool(false)),
                    (
                        "policy",
                        crate::auth::oauth::json_object(vec![(
                            "state",
                            Value::String("enabled".into()),
                        )]),
                    ),
                ]),
            ]),
        )]);
        let catalog = parse_github_copilot_model_catalog(&raw, true).expect("catalog");
        assert_eq!(catalog.available_model_ids, vec!["picker".to_string()]);
        assert!(catalog.policy_model_ids.is_empty());
    }

    #[test]
    fn parse_float_matches_js_prefix_semantics() {
        assert_eq!(parse_float("1"), 1.0);
        assert_eq!(parse_float(" 2.5s"), 2.5);
        assert!(parse_float("abc").is_nan());
        assert_eq!(parse_float("Infinity"), f64::INFINITY);
        assert_eq!(parse_float("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(parse_float("5e2x"), 500.0);
    }

    #[test]
    fn date_parse_matches_javascript_forms() {
        // Node 26 实测值：三种 HTTP-date 写法都是 2015-10-21T07:28:00Z。
        for value in [
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "Wed, 21 Oct 2015 07:28:00 UTC",
            "Wed, 21 Oct 2015 07:28:00 GMT+0000",
        ] {
            assert_eq!(date_parse(value), 1_445_412_480_000.0, "{value}");
        }
        // 只有日期的 ISO 形式按 UTC 零点解释。
        assert_eq!(date_parse("2015-10-21"), 1_445_385_600_000.0);
        assert!(date_parse("not a date").is_nan());
    }
}
