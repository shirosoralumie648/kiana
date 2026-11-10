//! api/azure-openai-config.ts。
//!
//! Azure 模型不带 `baseUrl`：每个用户一个 resource，按请求解析。优先级（逐级回落）：
//! `azureBaseUrl` → `AZURE_OPENAI_BASE_URL` → `azureResourceName` / `AZURE_OPENAI_RESOURCE_NAME`
//! → `model.baseUrl` → 报错。
//!
//! TS: `packages/ai/src/api/azure-openai-config.ts`。
//!
//! # 这个文件负责什么
//!
//! 把「模型 + 请求选项」解析成一次 Azure 请求所需的端点信息：base URL、API 版本与
//! deployment 名。Azure 的 base URL 不能只靠模型目录确定：同一个模型 id 在不同用户、
//! 不同 resource 下对应不同主机，端点必须在每次请求时按选项与环境变量现算。两条调用
//! 路径共用这里的实现：
//!
//! - `api::azure_openai_responses` 的 `stream` / `stream_simple`（Responses 协议）；
//! - `providers::azure` 的 `AzureStreams`（把 `openai-completions` 的 Azure 模型包一层
//!   端点解析）。
//!
//! 解析函数都不发请求；唯一的外部输入是 [`Model`]、[`AzureEndpointOptions`] 与环境变量
//! （`options.base.env` 优先，其次是进程环境，另有 bun sandbox 兜底，见
//! `utils::provider_env::get_provider_env_value`）。
//!
//! # 一次解析的顺序
//!
//! 1. [`resolve_azure_base_url`]：显式 `azureBaseUrl` → `AZURE_OPENAI_BASE_URL` →
//!    `azureResourceName` / `AZURE_OPENAI_RESOURCE_NAME`（拼成默认主机）→ `model.base_url`；
//!    全部为空时返回错误。选中的值统一交给私有函数 `normalize_azure_base_url` 归一化。
//! 2. `normalize_azure_base_url`：按 Node `new URL()` 的语义解析；Azure 主机且路径属于
//!    空/`/openai`/`/openai/v1/responses` 等少数形式时改写为 `/openai/v1` 并清空 query，
//!    最后去掉末尾 `/`。
//! 3. [`resolve_azure_config`]：在 base URL 之外补 API 版本（选项 →
//!    `AZURE_OPENAI_API_VERSION` → `v1`）。
//! 4. [`resolve_deployment_name`]：deployment 名独立解析（选项 →
//!    `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` → `model.id`），与 base URL 的优先级互不影响。
//!
//! # 与 TS 的字节级兼容
//!
//! `fixtures` 测试用 `tests/fixtures/api-shared/azure_openai_config.json` 与 TS 参考实现
//! 逐条对照，因此下面这些细节不能“顺手简化”：
//!
//! - 空白判定用 `kiana_json::js_string::js_trim`（JS `trim()` 会去掉 U+FEFF），不是
//!   `str::trim`；
//! - Node 的 WHATWG URL 解析器比 `url` crate 宽松：纯 ASCII 主机里 punycode 解码失败的
//!   `xn--` 标签在 Node 中原样保留，这里用占位标签重解析后再换回（私有 `NodeUrl`）；
//! - WHATWG 序列化会编码 path 里的 `^`，`url` crate 不会，需要手动补 `%5E`；
//! - `parse_deployment_name_map` 复刻 JS `split("=", 2)` 与 `Map.set` 的覆盖/顺序语义。
//!
//! # 注意事项
//!
//! - 返回的 base URL 末尾没有 `/`，调用方可以直接拼 `/responses`；
//! - [`resolve_deployment_name`] 不会失败：没有显式值或映射时回落到 `model.id`（若模型
//!   数据的 id 为空，结果也会是空串）；base URL 解析失败返回 [`AiError`]
//!   （`AiErrorKind::Request`）；
//! - 错误消息里是归一化函数收到的那份字符串：`azureBaseUrl` 与 `AZURE_OPENAI_BASE_URL`
//!   已先 `trim`；`azureResourceName`/`AZURE_OPENAI_RESOURCE_NAME`（拼成默认主机后）与
//!   `model.base_url` 保持原文，含空白导致解析失败时错误消息会保留这些原始空白。

use indexmap::IndexMap;
use url::Url;

use crate::error::{AiError, AiErrorKind};
use crate::types::{Model, ProviderEnv, StreamOptions};
use crate::utils::provider_env::get_provider_env_value;
use kiana_json::js_string::js_trim;

/// `DEFAULT_AZURE_API_VERSION`。
///
/// 默认 API 版本；回落顺序见 [`resolve_azure_config`]。
const DEFAULT_AZURE_API_VERSION: &str = "v1";

/// `AzureEndpointOptions extends StreamOptions`。
///
/// Rust 没有接口继承：`base` 承载 TS 继承来的 `StreamOptions` 字段（这些解析函数只用到
/// `env`），四个 `azure*` 字段与 TS 同名。
///
/// 所有 `azure*` 字段都是可选的；解析函数把 `None` 与空串都当作“未设置”，按固定顺序
/// 逐级回落。该结构只用于解析，不参与序列化。
///
/// TS: `src/api/azure-openai-config.ts#AzureEndpointOptions`
#[derive(Default)]
pub struct AzureEndpointOptions {
    /// TS 继承来的 `StreamOptions`。
    ///
    /// 解析时只读取其中的 `env`（见 `get_provider_env_value`），其他字段被忽略。
    pub base: StreamOptions,
    /// `azureApiVersion`。
    ///
    /// 显式 API 版本；空串视为未设置，回落到 `AZURE_OPENAI_API_VERSION`，再回落到 `v1`。
    pub azure_api_version: Option<String>,
    /// `azureResourceName`。
    ///
    /// resource 名；非空时拼成 `https://<name>.openai.azure.com/openai/v1`，
    /// 优先级低于 `azureBaseUrl`/`AZURE_OPENAI_BASE_URL`，高于 `model.base_url`。
    pub azure_resource_name: Option<String>,
    /// `azureBaseUrl`。
    ///
    /// 完整 base URL；解析前会做 JS `trim()` 并去掉末尾 `/`，空串/纯空白视为未设置。
    pub azure_base_url: Option<String>,
    /// `azureDeploymentName`。
    ///
    /// 显式 deployment 名；非空时直接作为请求的 `model` 字段；空串视为未设置。
    pub azure_deployment_name: Option<String>,
}

impl AzureEndpointOptions {
    fn env(&self) -> Option<&ProviderEnv> {
        self.base.env.as_ref()
    }
}

fn azure_error(message: impl Into<String>) -> AiError {
    AiError::new(AiErrorKind::Request, message)
}

/// `parseDeploymentNameMap(value?)`。
///
/// 逗号分隔、每项 trim、空项跳过；按**第一个** `=` 切分（JS 的 `split("=", 2)` 会丢弃第 3
/// 段及其后内容）。后出现的同名键覆盖前值但保留原位置（JS `Map.set`）。
fn parse_deployment_name_map(value: Option<&str>) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    let Some(value) = value else {
        return map;
    };
    if value.is_empty() {
        return map;
    }
    for entry in value.split(',') {
        let trimmed = js_trim(entry);
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split('=');
        let model_id = parts.next().unwrap_or("");
        let deployment_name = parts.next().unwrap_or("");
        if model_id.is_empty() || deployment_name.is_empty() {
            continue;
        }
        map.insert(
            js_trim(model_id).to_string(),
            js_trim(deployment_name).to_string(),
        );
    }
    map
}

/// `resolveDeploymentName(model, options?)`。
///
/// 解析顺序：非空 `azureDeploymentName` → `AZURE_OPENAI_DEPLOYMENT_NAME_MAP` 里以
/// `model.id` 为键的条目（值非空）→ `model.id`。因此不需要 `Result`：部署名缺失时用
/// 模型 id 兜底而不是报错（若 `model.id` 本身是空串，返回值也会是空串）。
///
/// 环境变量查找走 `get_provider_env_value`：`options.base.env` 优先，其次是进程环境。
/// map 的格式是 `"modelId=deploymentName,..."`；逗号项与 `=` 两侧的空白都会被 trim，
/// 空项与缺少一侧的项被跳过。
///
/// 注意这里得到的是**请求参数**里的模型名；调用方仍用 `model.id` 记录
/// `AssistantMessage::model`。
///
/// TS: `src/api/azure-openai-config.ts#resolveDeploymentName`
pub fn resolve_deployment_name(model: &Model, options: Option<&AzureEndpointOptions>) -> String {
    if let Some(deployment_name) = options
        .and_then(|options| options.azure_deployment_name.as_deref())
        .filter(|name| !name.is_empty())
    {
        return deployment_name.to_string();
    }
    let env_value = get_provider_env_value(
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
        options.and_then(AzureEndpointOptions::env),
    );
    let mapped = parse_deployment_name_map(env_value.as_deref())
        .get(&model.id)
        .cloned();
    match mapped {
        Some(mapped) if !mapped.is_empty() => mapped,
        _ => model.id.clone(),
    }
}

/// WHATWG `new URL()` 的等价解析结果。
///
/// `url` crate 的 IDNA 比 Node 严格：**punycode 解码失败的纯 ASCII 主机标签**
/// （如 `xn--r.openai.azure.com`）在 Node 的 WHATWG 解析器里原样保留（`beStrict=false`），
/// 而 `url` crate 报 `IdnaError`。fallback 解析把这类标签换成占位标签，序列化时再换回真实
/// 主机名（见 [`parse_ascii_punycode_fallback`]）。
struct NodeUrl {
    url: Url,
    /// `(占位主机名, 真实主机名)`；只有 fallback 解析时存在。
    host_replacement: Option<(String, String)>,
}

impl NodeUrl {
    /// `URL.prototype.hostname`。
    fn host_str(&self) -> &str {
        match &self.host_replacement {
            Some((_, host)) => host,
            None => self.url.host_str().unwrap_or(""),
        }
    }

    /// `URL.prototype.toString()`。
    fn serialize(&self) -> String {
        let mut serialized = self.url.as_str().to_string();
        if let Some((placeholder, host)) = &self.host_replacement {
            serialized = replace_serialized_host(&serialized, placeholder, host);
        }
        encode_path_carets(&serialized)
    }
}

/// `new URL(input)`。
///
/// 先交给 `url` crate；只有 `IdnaError` 才尝试 `parse_ascii_punycode_fallback`
/// （后者只接受纯 ASCII 主机，非 ASCII 的 IDNA 失败仍按原错误处理），其余解析错误直接
/// 返回 `Err(())`。返回 `NodeUrl` 而不是 `Url`，是为了让序列化时能把占位主机恢复成 Node
/// 会保留的原始主机名。
fn parse_url_like_node(input: &str) -> Result<NodeUrl, ()> {
    match Url::parse(input) {
        Ok(url) => Ok(NodeUrl {
            url,
            host_replacement: None,
        }),
        Err(url::ParseError::IdnaError) => parse_ascii_punycode_fallback(input).ok_or(()),
        Err(_) => Err(()),
    }
}

/// 把 ASCII 主机里解码失败的 `xn--` 标签换成 `x` 后重新解析（Node 的 UTS46 非严格行为）。
///
/// 只处理纯 ASCII 主机（非 ASCII 的 IDNA 行为与 Node 一致），并要求主机里只含普通标签字符；
/// 其它情况保持原有的解析错误。
///
/// 只有在能取出主机、主机是纯 ASCII、主机不含 URL 禁止字符、且至少有一个 `xn--` 标签时
/// 才返回 `Some`。多个 `xn--` 标签都替换成同一个占位标签 `x` 也没关系：恢复时是整段
/// 主机替换，不依赖标签一一对应。
fn parse_ascii_punycode_fallback(input: &str) -> Option<NodeUrl> {
    let (host_start, host_end, host) = split_authority_host(input)?;
    // 非 ASCII 主机的 IDNA 行为与 Node 一致，不走 fallback。
    if !host.is_ascii() {
        return None;
    }
    // 出现任何 URL 禁止的主机字符说明失败原因不只是 punycode，交回给 `url` crate 的错误。
    if host.chars().any(|character| {
        matches!(
            character,
            '\0' | '\t'
                | '\n'
                | '\r'
                | ' '
                | '#'
                | '/'
                | ':'
                | '<'
                | '>'
                | '?'
                | '@'
                | '['
                | '\\'
                | ']'
                | '^'
                | '|'
                | '%'
        )
    }) {
        return None;
    }

    // punycode 标签在 Node 的非严格 UTS46 解析里原样保留；换成合法标签才能让 `url` crate
    // 接受这个主机。
    let mut replaced = false;
    let sanitized_host = host
        .split('.')
        .map(|label| {
            if label.to_ascii_lowercase().starts_with("xn--") {
                replaced = true;
                "x"
            } else {
                label
            }
        })
        .collect::<Vec<_>>()
        .join(".");
    // 没有任何 xn-- 标签 → 失败原因不是 punycode，交回原始解析错误。
    if !replaced {
        return None;
    }

    // 只替换主机所在的字节区间，scheme / 端口 / 路径保持原文。
    let sanitized = format!(
        "{}{}{}",
        &input[..host_start],
        sanitized_host,
        &input[host_end..]
    );
    let url = Url::parse(&sanitized).ok()?;
    let placeholder = url.host_str()?.to_string();
    // 记录占位主机与原始（小写）主机；序列化时按前缀整段换回。小写化与 Node 的
    // hostname 归一化一致（测试 `matches_node_url_serialization` 锁定）。
    Some(NodeUrl {
        url,
        host_replacement: Some((placeholder, host.to_ascii_lowercase())),
    })
}

/// 从绝对 URL 文本里取出主机（不含 userinfo / 端口）及其在 `input` 里的字节范围。
///
/// 返回 `(host_start, host_end, host)`，两个下标是 `input` 的字节偏移，供
/// `parse_ascii_punycode_fallback` 原位替换主机。
///
/// 只接受带 scheme 的绝对 URL；IPv6 字面量（`[::1]`）、空主机、以及缺少 `//` 的非
/// special scheme 都返回 `None`，让调用方保留 `url` crate 的原始错误。
fn split_authority_host(input: &str) -> Option<(usize, usize, &str)> {
    let colon = input.find(':')?;
    let scheme = &input[..colon];
    let mut scheme_chars = scheme.chars();
    if !scheme_chars.next()?.is_ascii_alphabetic()
        || !scheme_chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
    {
        return None;
    }
    let special_scheme = matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "ws" | "wss" | "ftp" | "file"
    );
    let bytes = input.as_bytes();
    let mut index = colon + 1;
    if special_scheme {
        // WHATWG：special scheme 后面任意数量的 `/` 或 `\` 都被跳过（0 个也合法）。
        while index < bytes.len() && matches!(bytes[index], b'/' | b'\\') {
            index += 1;
        }
    } else {
        if bytes.get(index) != Some(&b'/') || bytes.get(index + 1) != Some(&b'/') {
            return None;
        }
        index += 2;
    }
    let authority_start = index;
    while index < bytes.len() && !matches!(bytes[index], b'/' | b'\\' | b'?' | b'#') {
        index += 1;
    }
    let authority = &input[authority_start..index];
    // userinfo 可以包含 `@`，以最后一个为准；它不参与主机替换。
    let host_offset = authority.rfind('@').map_or(0, |offset| offset + 1);
    let hostport = &authority[host_offset..];
    // IPv6 字面量不做 punycode 兜底。
    if hostport.starts_with('[') {
        return None;
    }
    // userinfo 已去掉；最后一个 ':' 之后是端口（IPv6 已在上面排除）。
    let host = match hostport.rfind(':') {
        Some(port_offset) => &hostport[..port_offset],
        None => hostport,
    };
    if host.is_empty() {
        return None;
    }
    let host_start = authority_start + host_offset;
    Some((host_start, host_start + host.len(), host))
}

/// 把序列化 URL 的 authority 里的占位主机名换回真实主机名。
///
/// 在 authority 段内跳过 userinfo 后按前缀匹配占位主机，命中时用真实主机替换；`rest`
/// 原样保留端口。找不到 scheme 或前缀不匹配时返回输入不变，调用方无需区分“没有替换”
/// 与“替换后相同”。
fn replace_serialized_host(serialized: &str, placeholder: &str, host: &str) -> String {
    let Some(scheme_end) = serialized.find("://") else {
        return serialized.to_string();
    };
    // 只在第一个 "://" 之后处理，避免误伤 path / query 中的同名字符串。
    let authority_start = scheme_end + 3;
    let authority_end = serialized[authority_start..]
        .find(['/', '?', '#'])
        .map_or(serialized.len(), |offset| authority_start + offset);
    let authority = &serialized[authority_start..authority_end];
    let host_offset = authority.rfind('@').map_or(0, |offset| offset + 1);
    let (before_host, host_and_port) = authority.split_at(host_offset);
    let Some(rest) = host_and_port.strip_prefix(placeholder) else {
        return serialized.to_string();
    };
    format!(
        "{}{}{}{}{}",
        &serialized[..authority_start],
        before_host,
        host,
        rest,
        &serialized[authority_end..]
    )
}

/// WHATWG 的 path percent-encode set 会编码 `^`（Node 输出 `%5E`），`url` crate 不编码。
///
/// 只处理 path 部分：query / fragment 里的 `^` 在 WHATWG 序列化里是字面量。
///
/// userinfo 里的 `^` 由 `url` crate 自己编码（见 `matches_node_url_serialization`），
/// 这里只补 path 部分；没有 path（authority 后直接是 `?`/`#`）时原样返回。
fn encode_path_carets(serialized: &str) -> String {
    let Some(scheme_end) = serialized.find("://") else {
        return serialized.to_string();
    };
    let authority_start = scheme_end + 3;
    let Some(path_offset) = serialized[authority_start..].find(['/', '?', '#']) else {
        return serialized.to_string();
    };
    let path_start = authority_start + path_offset;
    if !serialized[path_start..].starts_with('/') {
        return serialized.to_string();
    }
    let path_end = serialized[path_start..]
        .find(['?', '#'])
        .map_or(serialized.len(), |offset| path_start + offset);
    let path = &serialized[path_start..path_end];
    if !path.contains('^') {
        return serialized.to_string();
    }
    format!(
        "{}{}{}",
        &serialized[..path_start],
        path.replace('^', "%5E"),
        &serialized[path_end..]
    )
}

/// `normalizeAzureBaseUrl(baseUrl)`。
///
/// 把用户给的 base URL 归一化成 Azure SDK 可直接使用的形式，返回值没有末尾 `/`。
/// 解析失败时返回 `AiErrorKind::Request`，消息里带的是
/// **传进本函数的 `base_url`**（内部去尾斜杠之前的版本），文案为
/// `Invalid Azure OpenAI base URL: ...`（当前稳定契约，改动需同步测试）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
fn normalize_azure_base_url(base_url: &str) -> Result<String, AiError> {
    // TS 用 `String.prototype.trim()`（去掉 U+FEFF，但不去 U+0085）。
    // 随后去掉所有末尾 "/"。
    let trimmed = js_trim(base_url).trim_end_matches('/');
    let mut parsed = parse_url_like_node(trimmed)
        .map_err(|()| azure_error(format!("Invalid Azure OpenAI base URL: {base_url}")))?;

    // hostname 对 http(s) 等 special scheme 已由解析器小写化；fallback 情况下返回的是
    // 真实主机名。
    let hostname = parsed.host_str();
    // 后缀匹配（不是精确相等）：任意子域如 `sub.openai.azure.com` 都会命中，与 TS 的
    // endsWith 一致。
    let is_azure_host = hostname.ends_with(".openai.azure.com")
        || hostname.ends_with(".cognitiveservices.azure.com")
        || hostname.ends_with(".ai.azure.com");
    // 路径同样先去掉所有末尾 "/"，因此 "/openai/" 与 "/openai" 等价。
    let normalized_path = parsed.url.path().trim_end_matches('/');

    // Ensure Azure hosts have /openai/v1 as base path so the AzureOpenAI SDK can append
    // /deployments/<model>/... and ?api-version=v1 correctly.
    // 只在下列路径集合内改写；自定义路径与 query 原样保留（非 Azure 主机完全不改）。
    if is_azure_host
        && matches!(
            normalized_path,
            "" | "/" | "/openai" | "/openai/v1/responses"
        )
    {
        parsed.url.set_path("/openai/v1");
        parsed.url.set_query(None);
    }

    // 非 Azure 主机的根路径会被 url 序列化成 "https://host/"；这里统一去掉末尾 "/"。
    Ok(parsed.serialize().trim_end_matches('/').to_string())
}

/// `buildDefaultBaseUrl(resourceName)`。
fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

/// `resolveAzureBaseUrl(model, options?)`。
///
/// 逐级回落：非空 `azureBaseUrl`（trim 后）→ `AZURE_OPENAI_BASE_URL`（trim 后）→
/// 非空 `azureResourceName` 或 `AZURE_OPENAI_RESOURCE_NAME`（拼成
/// `https://<name>.openai.azure.com/openai/v1`）→ 非空 `model.base_url` → 错误。
/// 选中的字符串都会交给 `normalize_azure_base_url`，因此返回值不带末尾 `/`。
///
/// # 错误
///
/// - 所有来源都为空：`AiErrorKind::Request`，消息列出全部可用配置项（当前稳定契约，改动需同步测试）；
/// - 选中的值不是合法 URL：同样是 `AiErrorKind::Request`，消息包含归一化函数收到的
///   URL 值。
///
/// TS: `src/api/azure-openai-config.ts#resolveAzureBaseUrl`
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn resolve_azure_base_url(
    model: &Model,
    options: Option<&AzureEndpointOptions>,
) -> Result<String, AiError> {
    // 选项与环境变量先做 JS trim，空白串按“未设置”处理（TS 的 falsy 判定）。
    let base_from_options = options
        .and_then(|options| options.azure_base_url.as_deref())
        .map(js_trim)
        .filter(|value| !value.is_empty());
    let base_from_env = get_provider_env_value(
        "AZURE_OPENAI_BASE_URL",
        options.and_then(AzureEndpointOptions::env),
    )
    .map(|value| js_trim(&value).to_string())
    .filter(|value| !value.is_empty());
    // resource name 只做非空过滤、不 trim：与 TS 的 `||` 语义一致。
    let resource_name = options
        .and_then(|options| options.azure_resource_name.clone())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            get_provider_env_value(
                "AZURE_OPENAI_RESOURCE_NAME",
                options.and_then(AzureEndpointOptions::env),
            )
            .filter(|value| !value.is_empty())
        });

    // 前三级的优先级是线性的；resource name 通过默认主机参与竞争。
    let mut resolved_base_url = base_from_options.map(str::to_string).or(base_from_env);

    if resolved_base_url.is_none()
        && let Some(resource_name) = resource_name
    {
        resolved_base_url = Some(build_default_base_url(&resource_name));
    }

    // model.base_url 是最后一级：目录里显式配置了端点时仍可用。
    if resolved_base_url.is_none() && !model.base_url.is_empty() {
        resolved_base_url = Some(model.base_url.clone());
    }

    // 全部为空：错误文本逐字复刻 TS，便于用户知道所有可配置项。
    let Some(resolved_base_url) = resolved_base_url else {
        return Err(azure_error(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.",
        ));
    };

    normalize_azure_base_url(&resolved_base_url)
}

/// `resolveAzureConfig(model, options?)` 的返回值。
///
/// `base_url` 已归一化（无末尾 `/`），`api_version` 已按回落顺序解析；两者直接用于
/// 拼请求 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureConfig {
    /// 解析后的 base URL。
    ///
    /// 调用方可以直接在其后拼 `/responses`。
    pub base_url: String,
    /// API 版本。
    ///
    /// 选项与 env 都未给出时是 `v1`。
    pub api_version: String,
}

/// `resolveAzureConfig(model, options?)`。
///
/// 返回一次 Azure 请求需要的两部分配置：已归一化的 base URL 与 API 版本。
/// API 版本优先级：非空 `azureApiVersion` → `AZURE_OPENAI_API_VERSION` →
/// `DEFAULT_AZURE_API_VERSION`（`"v1"`）；空串与 `None` 同样视为未设置。
///
/// # 错误
///
/// base URL 无法解析时原样返回 [`resolve_azure_base_url`] 的错误；API 版本解析不会失败。
///
/// TS: `src/api/azure-openai-config.ts#resolveAzureConfig`
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn resolve_azure_config(
    model: &Model,
    options: Option<&AzureEndpointOptions>,
) -> Result<AzureConfig, AiError> {
    let base_url = resolve_azure_base_url(model, options)?;
    // 版本不 trim：TS 也只做非空判断（与 base URL 的处理不同）。
    let api_version = options
        .and_then(|options| options.azure_api_version.as_deref())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            get_provider_env_value(
                "AZURE_OPENAI_API_VERSION",
                options.and_then(AzureEndpointOptions::env),
            )
            .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());
    Ok(AzureConfig {
        base_url,
        api_version,
    })
}

#[cfg(test)]
mod tests {
    //! 单元测试：部署名 map、base URL 优先级、Node URL 序列化细节与错误文本。

    use super::*;
    use crate::types::ModelInputModality;

    fn model(base_url: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: "Azure".into(),
            api: "azure-openai-responses".into(),
            provider: "azure".into(),
            base_url: base_url.to_string(),
            input: vec![ModelInputModality::Text],
            ..Model::default()
        }
    }

    fn options() -> AzureEndpointOptions {
        AzureEndpointOptions::default()
    }

    #[test]
    fn parses_deployment_name_map() {
        let map = parse_deployment_name_map(Some("a=one, b = two ,,c=three=extra,=missing,x="));
        assert_eq!(map.get("a").map(String::as_str), Some("one"));
        assert_eq!(map.get("b").map(String::as_str), Some("two"));
        // `split("=", 2)` 丢弃第二段之后的内容。
        assert_eq!(map.get("c").map(String::as_str), Some("three"));
        assert_eq!(map.len(), 3);
        assert!(parse_deployment_name_map(None).is_empty());
        assert!(parse_deployment_name_map(Some("")).is_empty());
    }

    #[test]
    fn resolves_deployment_name_precedence() {
        let mut opts = options();
        opts.azure_deployment_name = Some("explicit".into());
        assert_eq!(
            resolve_deployment_name(&model("", "gpt-4o"), Some(&opts)),
            "explicit"
        );
        assert_eq!(
            resolve_deployment_name(&model("", "gpt-4o"), None),
            "gpt-4o"
        );
    }

    #[test]
    fn normalizes_azure_hosts_to_openai_v1() {
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/").unwrap(),
            "https://r.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com").unwrap(),
            "https://r.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/openai").unwrap(),
            "https://r.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/openai/v1/responses").unwrap(),
            "https://r.openai.azure.com/openai/v1"
        );
        // 其它路径保持，query 保留。
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/custom/path/?x=1").unwrap(),
            "https://r.openai.azure.com/custom/path/?x=1"
        );
        // 非 Azure 主机：不改写。
        assert_eq!(
            normalize_azure_base_url("https://proxy.example.com/openai").unwrap(),
            "https://proxy.example.com/openai"
        );
    }

    #[test]
    fn invalid_base_url_reports_original_value() {
        let error = normalize_azure_base_url("  not a url  ").unwrap_err();
        assert_eq!(
            error.message,
            "Invalid Azure OpenAI base URL:   not a url  "
        );
    }

    #[test]
    fn matches_node_url_serialization() {
        // JS `String.prototype.trim()` 去掉 U+FEFF（BOM），Rust 的 `str::trim` 不去。
        assert_eq!(
            normalize_azure_base_url("\u{FEFF}https://r.openai.azure.com/\u{FEFF}").unwrap(),
            "https://r.openai.azure.com/openai/v1"
        );
        // Node 的 UTS46 非严格解析接受 punycode 解码失败的 ASCII 标签；`url` crate 会报错。
        assert_eq!(
            normalize_azure_base_url("https://xn--r.openai.azure.com/").unwrap(),
            "https://xn--r.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://xn--invalid-.openai.azure.com/").unwrap(),
            "https://xn--invalid-.openai.azure.com/openai/v1"
        );
        // 大小写归一由解析器完成（Node 也小写主机名）。
        assert_eq!(
            normalize_azure_base_url("https://XN--R.OpenAI.Azure.COM/").unwrap(),
            "https://xn--r.openai.azure.com/openai/v1"
        );
        // WHATWG 序列化在 path 里编码 `^`（`url` crate 不编码）；query / fragment 保持字面量。
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/x^y").unwrap(),
            "https://r.openai.azure.com/x%5Ey"
        );
        assert_eq!(
            normalize_azure_base_url("https://R.OpenAI.Azure.COM/a^b?c=^d#e^f").unwrap(),
            "https://r.openai.azure.com/a%5Eb?c=^d#e^f"
        );
        assert_eq!(
            normalize_azure_base_url("https://us^er:pa^ss@r.openai.azure.com/x^y").unwrap(),
            "https://us%5Eer:pa%5Ess@r.openai.azure.com/x%5Ey"
        );
        assert_eq!(
            normalize_azure_base_url("https://r.openai.azure.com/%5E").unwrap(),
            "https://r.openai.azure.com/%5E"
        );
        // 省略 `//` 的 special scheme 与 Node 一样补全。
        assert_eq!(
            normalize_azure_base_url("https:example.com").unwrap(),
            "https://example.com"
        );
        // 非 Azure 主机的 punycode 标签同样保留。
        assert_eq!(
            normalize_azure_base_url("https://xn--r.example.com/").unwrap(),
            "https://xn--r.example.com"
        );
    }

    #[test]
    fn deployment_name_map_uses_javascript_trim() {
        // U+FEFF 在 JS 的 trim 里算空白：解析后键值都不带 BOM。
        let map = parse_deployment_name_map(Some("\u{FEFF}m\u{FEFF}=\u{FEFF}dep\u{FEFF}"));
        assert_eq!(map.get("m").map(String::as_str), Some("dep"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn resolve_azure_base_url_trims_with_javascript_semantics() {
        let mut opts = options();
        opts.azure_base_url = Some("\u{FEFF} https://explicit.example.com/v1/ ".into());
        assert_eq!(
            resolve_azure_base_url(&model("", "m"), Some(&opts)).unwrap(),
            "https://explicit.example.com/v1"
        );
        // 只有空白字符的 azureBaseUrl 在 JS 里是 falsy（trim 后为空）→ 回落到 resource name。
        let mut opts = options();
        opts.azure_base_url = Some("\u{FEFF}".into());
        opts.azure_resource_name = Some("resource".into());
        assert_eq!(
            resolve_azure_base_url(&model("", "m"), Some(&opts)).unwrap(),
            "https://resource.openai.azure.com/openai/v1"
        );
    }

    #[test]
    fn resolves_base_url_precedence() {
        let mut opts = options();
        opts.azure_base_url = Some(" https://explicit.example.com/v1/ ".into());
        assert_eq!(
            resolve_azure_base_url(&model("https://model.example.com", "m"), Some(&opts)).unwrap(),
            "https://explicit.example.com/v1"
        );

        let mut opts = options();
        opts.azure_resource_name = Some("resource".into());
        assert_eq!(
            resolve_azure_base_url(&model("", "m"), Some(&opts)).unwrap(),
            "https://resource.openai.azure.com/openai/v1"
        );

        assert_eq!(
            resolve_azure_base_url(&model("https://model.example.com/", "m"), None).unwrap(),
            "https://model.example.com"
        );

        let error = resolve_azure_base_url(&model("", "m"), None).unwrap_err();
        assert_eq!(
            error.message,
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl."
        );
    }

    #[test]
    fn resolves_config_with_default_api_version() {
        let config = resolve_azure_config(&model("https://model.example.com", "m"), None).unwrap();
        assert_eq!(config.base_url, "https://model.example.com");
        assert_eq!(config.api_version, "v1");

        let mut opts = options();
        opts.azure_api_version = Some("2024-10-21".into());
        let config =
            resolve_azure_config(&model("https://model.example.com", "m"), Some(&opts)).unwrap();
        assert_eq!(config.api_version, "2024-10-21");
    }
}

#[cfg(test)]
mod fixtures {
    //! 与 TS 参考实现逐条对照的黄金测试（`tests/fixtures/api-shared/azure_openai_config.json`）。
    //!
    //! fixture 的每个用例给出模型、选项与期望的 `ok`/`error`；断言对整串结果做相等比较，
    //! 因此 URL 的大小写、百分号编码与错误文本都被锁定。

    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-shared/azure_openai_config.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture parses")
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct OptionsJson {
        azure_api_version: Option<String>,
        azure_resource_name: Option<String>,
        azure_base_url: Option<String>,
        azure_deployment_name: Option<String>,
        env: Option<ProviderEnv>,
    }

    impl OptionsJson {
        fn into_options(self) -> AzureEndpointOptions {
            AzureEndpointOptions {
                base: StreamOptions {
                    env: self.env,
                    ..StreamOptions::default()
                },
                azure_api_version: self.azure_api_version,
                azure_resource_name: self.azure_resource_name,
                azure_base_url: self.azure_base_url,
                azure_deployment_name: self.azure_deployment_name,
            }
        }
    }

    fn model_of(value: &Value) -> Model {
        Model {
            id: value["id"].as_str().unwrap().to_string(),
            base_url: value["baseUrl"].as_str().unwrap().to_string(),
            ..Model::default()
        }
    }

    fn options_of(value: &Value) -> Option<AzureEndpointOptions> {
        if value.is_null() {
            return None;
        }
        Some(
            serde_json::from_value::<OptionsJson>(value.clone())
                .expect("options deserialize")
                .into_options(),
        )
    }

    fn assert_url(expected: &Value, actual: Result<String, AiError>, label: &str) {
        if let Some(error) = expected.get("error").and_then(Value::as_str) {
            assert_eq!(actual.expect_err(label).message, error, "{label}");
        } else {
            assert_eq!(
                actual.unwrap_or_else(|error| panic!("{label}: {error}")),
                expected["ok"].as_str().unwrap(),
                "{label}"
            );
        }
    }

    #[test]
    fn base_urls_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["baseUrlCases"].as_array().unwrap() {
            let model = model_of(&case["model"]);
            let options = options_of(&case["options"]);
            assert_url(
                &case["expected"],
                resolve_azure_base_url(&model, options.as_ref()),
                case["model"]["baseUrl"].as_str().unwrap_or("<empty>"),
            );
        }
    }

    #[test]
    fn option_cases_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["optionCases"].as_array().unwrap() {
            let model = model_of(&case["model"]);
            let options = options_of(&case["options"]);
            assert_url(
                &case["expected"],
                resolve_azure_base_url(&model, options.as_ref()),
                case["name"].as_str().unwrap(),
            );
        }
    }

    #[test]
    fn config_cases_match_typescript_reference() {
        let fixture = fixture();
        for (index, case) in fixture["configCases"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let model = model_of(&case["model"]);
            let options = options_of(&case["options"]);
            let expected = &case["expected"];
            match resolve_azure_config(&model, options.as_ref()) {
                Ok(config) => {
                    let expected = &expected["ok"];
                    assert_eq!(
                        config.base_url,
                        expected["baseUrl"].as_str().unwrap(),
                        "case {index}"
                    );
                    assert_eq!(
                        config.api_version,
                        expected["apiVersion"].as_str().unwrap(),
                        "case {index}"
                    );
                }
                Err(error) => assert_eq!(
                    error.message,
                    expected["error"].as_str().unwrap(),
                    "case {index}"
                ),
            }
        }
    }

    #[test]
    fn deployment_cases_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["deploymentCases"].as_array().unwrap() {
            let model = Model {
                id: case["id"].as_str().unwrap().to_string(),
                ..Model::default()
            };
            let options = options_of(&case["options"]);
            let actual = resolve_deployment_name(&model, options.as_ref());
            assert_eq!(
                actual,
                case["expected"]["ok"].as_str().unwrap(),
                "deployment case {}",
                case["id"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn deployment_cases_do_not_leak_process_env() {
        // 环境变量回退只应在 scoped `env` 缺失时生效；这里确认 scoped env 优先且不读进程 env。
        let mut env = ProviderEnv::new();
        env.insert("AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(), "m=envy".into());
        let mut options = AzureEndpointOptions::default();
        options.base.env = Some(env);
        let model = Model {
            id: "m".into(),
            ..Model::default()
        };
        assert_eq!(resolve_deployment_name(&model, Some(&options)), "envy");
    }
}
