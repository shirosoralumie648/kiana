//! `oauth/discovery.ts`：`WWW-Authenticate`、well-known 发现与资源选择
//! （`spec/telemetry-mcp.md` §18.3、§20.3、§27.5）。
//!
//! TS 源：`packages/mcp/src/oauth/discovery.ts`
//! （改编自 `modelcontextprotocol/typescript-sdk` v1.29.0 `src/client/auth.ts`，MIT，见模块底部）。
//!
//! # `parseWwwAuthenticate` 的正则怪癖（§27.5）
//!
//! TS 用 `(?:^|[,\s])NAME=(?:"([^"]*)"|([^\s,]+))`（大小写不敏感）逐字段搜索**原始头字符串**，
//! 本模块逐项复刻，包括看起来像 bug 的行为（**不要**换成 RFC 7235 解析器）：
//! - 名字前必须有行首、逗号或空白 → `xerror="..."` 不匹配 `error`；
//! - `=` 两侧**不能**有空格 → `error = "x"` 解析不出；
//! - 无引号值读到空白/逗号为止 → `error=insufficient_scopeX` 得到 `"insufficient_scopeX"`；
//! - 引号值里的 `=`/`"` 属于值，但**后续字段的搜索仍会扫过引号内部** →
//!   `error_description="bad scope=x"` 之后 `scope` 命中 `x"`；
//! - `scope="a"error="b"` → `scope="a"`、`error` 缺失（`"` 后紧跟 `e` 不构成分隔符）；
//! - `""` → `undefined`（空值不携带信息）；第一个匹配胜出（`scope="a", scope="b"` → `a`）。
//!
//! scheme 判定同样是原文规则：`header.trimStart().split(/\s+/, 1)[0].toLowerCase()`，必须
//! 是 `bearer` 或 `dpop`；`Bearer,error="..."` 的第一个 token 是整个 `bearer,error="..."`，
//! 因此返回空 challenge。
//!
//! # URL 语义
//!
//! `origin` 用 `js_origin`，逐字复刻 JS 的 `URL.origin`（`https://x.example`，无尾斜杠）：
//! 非特殊 scheme 与 `file:` 得到 `null`；`blob:` 只有内层 URL 的 scheme 是 `http`/`https`/`file`
//! 时才取内层 origin（`blob:https://x/y` → `https://x`），其余（`blob:ftp://x/y`、嵌套
//! `blob:blob:…`）都是 `null`。`url` crate 的 [`Url::origin`] 对 `blob:` 内层**任何**特殊
//! scheme 都返回 tuple（`blob:ftp://x/y` → `ftp://x`），且会穿透嵌套 `blob:`，与 Node 不同；
//! 不修的话 `blob:ftp://x/y` 的发现 URL 会与 TS 分道扬镳，`selectResource` 的 origin 比较也会
//! 误判为相同。`blob:` URL 本身是 cannot-be-a-base，`origin_base` 必须从 origin **字符串**
//! 重建 base（直接 `set_path("/")` 会得到 `blob:%2F`）。
//!
//! 已知**不能**修掉的差异（`url::Url` 无法持有该形态）：`file:` URL 的**重复前导斜杠**。
//! Node 的 `new URL("file://host//a").href` 是 `file://host//a`（pathname `//a`），WPT 的
//! `url/resources/urltestdata.json` 也要求保留（`file:////foo` → `file:////foo`），而 `url`
//! crate 的 file 路径解析会把所有前导 `/` 折叠成一个（`parser.rs` 的
//! `trim_start_matches('/')`），`file://host//a`、`file:////a` 都表示不出来。受影响的是把
//! `new URL(...)` 结果**序列化**或比较 pathname 的地方：`resourceUrlFromServerUrl`、
//! `parseWwwAuthenticate` 的 `resourceMetadataUrl`、`selectResource` 的判定/消息。
//! `buildAuthorizationServerDiscoveryUrls` 不受影响：`file:` 的 origin 是 `null`，两边都在
//! 构造 base 时抛 `TypeError: Invalid URL`。`file:` URL 既不是 `MCP` 传输的合法端点（HTTP
//! 传输要求 http(s)），`parseProtectedResourceMetadata` 也用 `safeUrl` 把 `resource` 限制为
//! http(s)，因此现实输入到不了这些路径；`tests/fixtures/oauth_origin_golden.json` 里带
//! `"rust"` 标签的用例把归一化后的结果钉住。
//!
//! 发现 URL 走 `js_url`：它复刻 `new URL(relative, base)`（WHATWG 的 relative-URL 解析），
//! 而不是字符串拼接。二者的差别在 TS 里真实可见：第三个候选 `new URL(\`${path}/.well-known/…\`,
//! issuer.origin)` 的 `path` 以 `//` 开头时是**协议相对**引用，会解析到**另一个主机**
//! （`https://x.example//mcp` → `https://mcp/.well-known/openid-configuration`；`https://x.example//`
//! → `https://.well-known/openid-configuration`），主机段非法时整个调用抛 `TypeError: Invalid URL`
//! （`//%2F`、`//:1234`、`//a%20b` …）。
//!
//! `js_url` 同时补齐 WHATWG **特殊 scheme** 的路径百分号编码集合：Node 把路径里的 `^` 编码成
//! `%5E`，`url` crate 原样保留。这是本单元唯一一处 `url` crate 与 WHATWG 的编码差异
//! （对全部 BMP + 星形平面的逐码点扫描确认，见 `js_url` 的模块文档），但出现在
//! **返回值**里（`resourceMetadataUrl`、`selectResource` 的比较、发现请求的 URL），必须修掉。
//!
//! 网络请求全部经 [`McpFetch`]（`fetch(url, { headers: { Accept: "application/json",
//! "MCP-Protocol-Version": version }, signal })`），默认实现是 [`crate::auth_provider::ReqwestFetch`]。
//!
//! # 发现失败的分类
//!
//! 4xx 与 502 是 discovery miss（"这里没有"，继续试下一个候选），先丢弃响应体再继续；其他
//! 非 2xx 立即报 `` HTTP {status} loading authorization server metadata from {url} ``。
//! `discoverOAuthServerInfo` 吞掉受保护资源发现的所有错误，**但** `TypeError`（网络失败）向外传播。
//!
//! # 这个模块解决什么问题
//!
//! MCP 客户端第一次连接时手里只有服务器 URL（与可能的 `WWW-Authenticate` 挑战），OAuth 需要的
//! issuer、授权端点、令牌端点与受保护资源范围都还没有。RFC 9728 / RFC 8414 / OpenID Connect
//! Discovery 把这些信息发布在固定的 well-known URL 上，本文件负责**推导候选 URL、发请求、
//! 解析响应、分类失败**。没有它，[`crate::oauth::flow`] 的 `run_flow` 只能依赖调用方显式配置的
//! 元数据文档 URL（`authorization_server_metadata_url`），无法按 well-known 约定自动发现。
//!
//! 输入是字符串形式的服务器 URL（以及可选的 `WWW-Authenticate` 原文）与各 `*Options`；
//! 输出是解析好的 [`OAuthProtectedResourceMetadata`] / [`AuthorizationServerMetadata`] /
//! [`OAuthServerInfo`]，失败统一包在 [`OAuthFailure`] 里。
//!
//! 边界：只做"URL 推导 + HTTP GET + JSON 解析 + 失败分类"，不发令牌请求、不注册客户端、
//! 不持久化状态。字段级校验与错误文案在 [`crate::oauth::types`] 的 `parse_*`，错误信封在
//! [`crate::oauth::errors`]，HTTP 与 `Headers` 语义在 [`crate::auth_provider`]。
//!
//! # 调用关系
//!
//! - 本 crate 内：[`crate::oauth::flow`] 的 `run_flow` 首次发现时调用
//!   [`discover_oauth_server_info`]；缓存状态里缺元数据时改调
//!   [`discover_authorization_server_metadata`]，最后用 [`select_resource`] 校验受保护资源。
//!   元数据允许缺失，`run_flow` 会按 `?? new URL("/authorize", ...)` / `?? new URL("/token", ...)`
//!   回退到默认端点。
//! - 401 路径：[`parse_www_authenticate`] 解析挑战；crate 外的 `kiana-builtin-extensions`
//!   MCP OAuth 也直接调用它，把 `resourceMetadataUrl` 与 `scope` 交给授权流程。
//! - 网络：全部经 `McpFetch`（默认 [`ReqwestFetch`]），本模块不直接使用 `reqwest`。
//!
//! # 三条入口流程
//!
//! 1. [`discover_oauth_server_info`]：受保护资源发现 → 选授权服务器 URL → 授权服务器元数据
//!    发现，组装 [`OAuthServerInfo`]；只有第一步的错误按"是否 `TypeError`"决定吞掉或上抛。
//! 2. [`discover_protected_resource_metadata`]：`{origin}/.well-known/oauth-protected-resource`
//!    加去掉一个尾斜杠的路径后缀；非根路径首次 miss 时回退到裸路径。
//! 3. [`discover_authorization_server_metadata`]：按
//!    [`build_authorization_server_discovery_urls`] 的顺序依次尝试，命中后校验 `issuer`，
//!    全部 miss 返回 `Ok(None)`。
//!
//! [`parse_www_authenticate`]、[`select_resource`]、[`resource_url_from_server_url`] 与
//! [`build_authorization_server_discovery_urls`] 是纯函数，不发请求，可独立使用；本 crate 的
//! 集成测试直接调用它们，`kiana-builtin-extensions` 也直接使用 `parse_www_authenticate`。
//!
//! # 重要约束与注意事项
//!
//! - **不缓存**：每次调用都重新发请求；发现结果的持久化在 `run_flow` / `OAuthDiscoveryState`
//!   一侧，不在本模块。
//! - **顺序是契约**：候选顺序、`authorization_servers[0]` 的优先、路径后缀回退都会改变最终
//!   issuer，不能重排。
//! - **取消**：`signal` 只是原样放进 [`FetchInit::signal`]，本模块不轮询它；是否中断由
//!   `McpFetch` 实现决定（测试中已取消的 signal 仍会发出请求，由 fetch 实现返回 `AbortError`）。
//! - **协议版本头**：所有发现请求带 `MCP-Protocol-Version`。[`discover_oauth_server_info`]
//!   不暴露该选项，它的三处请求固定用 [`LATEST_PROTOCOL_VERSION`]；两个底层 `discover_*`
//!   允许调用方覆盖，缺省同样是 [`LATEST_PROTOCOL_VERSION`]。
//! - **失败分类**：授权服务器发现把 4xx/502 当作"这个候选没有"，其它非 2xx 立即终止；
//!   受保护资源发现把 4xx/502 当作"可以回退到裸路径"（仅自动推导且非根路径时），回退后
//!   仍非 2xx 即终止。`response.json()` 失败是 `SyntaxError`；[`discover_oauth_server_info`]
//!   只吞受保护资源发现这一步的非 `TypeError` 错误。

use std::sync::Arc;

use url::Url;

use crate::auth_provider::{FetchInit, FetchResponse, JsHeaders, McpFetch, ReqwestFetch};
use crate::oauth::errors::{OAuthFailure, OAuthIssuerMismatchError};
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthProtectedResourceMetadata, OAuthServerInfo,
    parse_authorization_server_metadata, parse_protected_resource_metadata,
};
use crate::protocol_types::LATEST_PROTOCOL_VERSION;

/// 默认的 `McpFetch`（TS 的 `globalThis.fetch`；Rust 侧是 [`ReqwestFetch`]）。
///
/// 进程级复用一个 `reqwest::Client`（连接池/TLS 会话），与 Node 的全局 `fetch` 一致。
///
/// 只在调用方没有显式给 `fetch` 选项时使用：`OnceLock` 保证第一次构造后全局复用同一个
/// `Arc`（也就复用同一个连接池），后续调用不会再 `new`。测试与嵌入方要换 HTTP 栈时传
/// 选项里的 `fetch`（各 `Discover*Options` 都有该字段），不要改这里。
fn default_fetch() -> Arc<dyn McpFetch> {
    use std::sync::OnceLock;
    static DEFAULT: OnceLock<Arc<dyn McpFetch>> = OnceLock::new();
    Arc::clone(DEFAULT.get_or_init(|| ReqwestFetch::new().shared()))
}

/// `discard(response)`：`response.body?.cancel().catch(() => {})`。
///
/// 丢弃响应体（Rust 侧 drop 流即取消），错误被吞掉。
///
/// `None` 直接返回（调用点传 `Some(response)`）；`Some` 会取走体流。函数没有返回值：
/// 与 TS 的 `.catch(() => {})` 一样，丢弃失败不会向调用方传播。
pub async fn discard(response: Option<FetchResponse>) {
    if let Some(mut response) = response {
        response.discard().await;
    }
}

/// `isDiscoveryMiss(status)`：4xx 与 502 表示"这里没有"，试下一个候选 URL。
///
/// 只看状态码，不读响应体。401/403 与 404 一样都算 miss，因此不能用它区分"鉴权失败"与
/// "资源不存在"——这个分类只是 TS 对"继续尝试下一个 well-known 候选"的约定。
pub fn is_discovery_miss(status: u16) -> bool {
    (400..500).contains(&status) || status == 502
}

/// `pathSuffix(pathname)`：`/.well-known/<kind><path>` 的路径后缀；根路径为空。
///
/// 只去掉**一个**尾斜杠：`"/"` → `""`、`"/mcp/"` → `"/mcp"`、`"/mcp"` → `"/mcp"`。
pub fn path_suffix(pathname: &str) -> &str {
    pathname.strip_suffix('/').unwrap_or(pathname)
}

/// `header.trimStart()`（ECMAScript 的 `WhiteSpace` + `LineTerminator`）。
fn trim_start_js(text: &str) -> &str {
    let start = text
        .char_indices()
        .find(|(_, ch)| !is_js_whitespace(*ch))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[start..]
}

/// `\s`（JS 正则，不含 `u` 标志）：`[\t\n\v\f\r    -     　﻿]`。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// 逐字符匹配 TS 的 `[,\s]`。
fn is_field_separator(ch: char) -> bool {
    ch == ',' || is_js_whitespace(ch)
}

/// `field(header, name)`：`(?:^|[,\s])NAME=(?:"([^"]*)"|([^\s,]+))`（大小写不敏感）。
///
/// 返回第 1 组（引号内）或第 2 组（无引号 token）；`""` → `None`（与 TS 的
/// `match?.[1] || match?.[2] || undefined` 一致：空串落到下一项，最终 `undefined`）。
///
/// **只认第一个匹配位置**：正则引擎在该位置匹配成功（哪怕第 1 组为空）后就不再向后搜索，
/// 因此 `error="" scope, error=x` 的 `error` 是 `undefined` 而不是后面那个 `x`。
pub fn field(header: &str, name: &str) -> Option<String> {
    // `name` 是内部常量（`resource_metadata`/`scope`/`error`/`error_description`），但 TS 把它
    // 插进 `new RegExp` 的源码里，正则元字符会被解释。这里保持同样语义。
    let name_chars: Vec<char> = name.chars().collect();
    let haystack: Vec<char> = header.chars().collect();
    for start in 0..haystack.len() {
        if let Some(value) = match_at(&haystack, start, &name_chars) {
            return value;
        }
    }
    None
}

/// 尝试在 `haystack[at..]` 匹配 `(?:^|[,\s])NAME=(?:"([^"]*)"|([^\s,]+))`。
///
/// 返回 `None` 表示该位置不匹配；`Some(value)` 表示匹配成功（`value` 即 `field` 的返回值，
/// 带引号的空值是 `Some(None)`）。
fn match_at(haystack: &[char], at: usize, name: &[char]) -> Option<Option<String>> {
    let mut cursor = at;
    // `(?:^|[,\s])`
    if at != 0 {
        let previous = haystack[at - 1];
        if !is_field_separator(previous) {
            return None;
        }
    }
    // NAME（大小写不敏感）
    for letter in name {
        let candidate = haystack.get(cursor)?;
        if !chars_equal_ignore_case(*candidate, *letter) {
            return None;
        }
        cursor += 1;
    }
    // `=`
    if *haystack.get(cursor)? != '=' {
        return None;
    }
    cursor += 1;
    // `"([^"]*)"`：先试带引号的分支（`""` 匹配成功但值是空串 → `undefined`）。
    if haystack.get(cursor) == Some(&'"')
        && let Some(close) = haystack[cursor + 1..].iter().position(|ch| *ch == '"')
    {
        let value: String = haystack[cursor + 1..cursor + 1 + close].iter().collect();
        // `match?.[1] || match?.[2] || undefined`：空串 → 第 2 组不存在 → None。
        return Some((!value.is_empty()).then_some(value));
    }
    // `([^\s,]+)`：没有闭合引号时正则回溯到这个分支，从开引号处开始吃 token
    // （实测 TS：`error="abc` → `"abc`、`error="a b` → `"a`、`error="` → `"`）。
    let value_start = cursor;
    let mut end = value_start;
    while let Some(ch) = haystack.get(end) {
        // `\s` 的集合见 `is_js_whitespace`（`\u{85}` NEL 在 JS 正则里**不是** `\s`）。
        if is_js_whitespace(*ch) || *ch == ',' {
            break;
        }
        end += 1;
    }
    if end == value_start {
        return None;
    }
    Some(Some(haystack[value_start..end].iter().collect()))
}

/// JS 正则 `/i` **不带 `u` 标志**时的 canonicalize（ECMA-262 `Canonicalize`）。
///
/// 规则（逐字）：`ch.toUpperCase()` 必须是**单个 code unit**，否则原样返回；再若
/// `ch` 的 code unit ≥ 128 而大写结果的 code unit < 128，也原样返回（因此 `ſ`~`s`、
/// `K`(U+212A)~`k` **不**成立 —— V8 实测如此）。
fn canonicalize(ch: char) -> char {
    let upper: Vec<char> = ch.to_uppercase().collect();
    if upper.len() != 1 {
        return ch;
    }
    let upper = upper[0];
    if (ch as u32) >= 128 && (upper as u32) < 128 {
        return ch;
    }
    upper
}

/// `/i` 下的字符相等：两侧各自 canonicalize 后比较。
fn chars_equal_ignore_case(a: char, b: char) -> bool {
    a == b || canonicalize(a) == canonicalize(b)
}

/// `parseWwwAuthenticate(header)`。
///
/// 返回的结构**总有**四个键（`None` 即 TS 的 `undefined`，`JSON.stringify` 会省略）。
/// `null`/空串 → 空 challenge。
pub fn parse_www_authenticate(header: Option<&str>) -> OAuthChallenge {
    let Some(header) = header.filter(|text| !text.is_empty()) else {
        return OAuthChallenge::default();
    };
    // 空串与缺失都归一成空 challenge；接着先判定 scheme，只有 bearer/dpop 才继续解析参数，
    // 因此 `Basic realm="x"` 这类挑战连 `scope` 都不会被扫描。
    // `header.trimStart().split(/\s+/, 1)[0]?.toLowerCase()`
    let trimmed = trim_start_js(header);
    let scheme: String = trimmed
        .split(is_js_whitespace)
        .next()
        .unwrap_or("")
        .to_lowercase();
    if scheme != "bearer" && scheme != "dpop" {
        return OAuthChallenge::default();
    }
    // 四个字段各自在**原始** header 上独立搜索（见 [`field`]），不做 token 切分；
    // `resource_metadata` 的 URL 解析失败只丢弃该字段，不影响其余三个。
    let resource_metadata = field(header, "resource_metadata");
    let mut resource_metadata_url = None;
    if let Some(resource_metadata) = resource_metadata {
        // `new URL(...)` 失败被吞掉（`try {} catch {}`）。
        resource_metadata_url = new_url(&resource_metadata).ok();
    }
    OAuthChallenge {
        resource_metadata_url,
        scope: field(header, "scope"),
        error: field(header, "error"),
        error_description: field(header, "error_description"),
    }
}

/// `fetchMetadata(url, fetch, protocolVersion, signal)`：GET + 两个头。
///
/// TS 把 `{ headers: { Accept, "MCP-Protocol-Version" } }` 交给 `fetch`，`Request` 构造时按
/// `new Headers(record)` 逐对 `append`。非法值（`\n`/`\r`/`\0`、非 ByteString）在**发请求前**
/// 抛 `TypeError`（消息前缀 `Headers.append:`，逐字与 Node 相同），底层 `fetch` 不会被调用。
/// 这里必须显式构造 [`JsHeaders`]：`FetchInit::header` 是 builder 风格，会吞掉 `Headers.set`
/// 的错误，把非法值静默丢掉。错误经 `From<McpTransportError>` 变成 `TypeError`，名字与消息
/// 都保留。
///
/// 与自定义 `McpFetch` 的唯一差别：TS 里头记录只是交给 fetch 实现的普通对象，是否校验取决于
/// 该实现；Rust 的 [`FetchInit`] 用 [`JsHeaders`] 持有头，非法头**根本无法**进入请求，因此校验
/// 固定在构造处（默认 `fetch` 的行为）。
///
/// `signal` 只是放进 [`FetchInit`]；本函数不等待取消，也不检查令牌状态——是否中断由传入的
/// `McpFetch` 实现决定（默认实现见 [`ReqwestFetch`]，被取消时返回 `AbortError` 等价的传输错误）。
pub async fn fetch_metadata(
    url: &Url,
    fetch: &Arc<dyn McpFetch>,
    protocol_version: &str,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<FetchResponse, OAuthFailure> {
    let headers = JsHeaders::from_pairs([
        ("Accept", "application/json"),
        ("MCP-Protocol-Version", protocol_version),
    ])?;
    let init = FetchInit::get()
        .with_headers(headers)
        .with_signal(signal.cloned());
    Ok(fetch.fetch(url, init).await?)
}

/// `new URL(...)` 失败：Node 报 `TypeError: Invalid URL`（逐字）。
fn invalid_url() -> OAuthFailure {
    OAuthFailure::type_error("Invalid URL")
}

/// `new URL(input)`：WHATWG 绝对 URL 解析，失败是 `TypeError: Invalid URL`。
///
/// 见 `js_url` 的模块文档。
fn new_url(input: &str) -> Result<Url, OAuthFailure> {
    js_url::parse(input).map_err(|_| invalid_url())
}

/// `new URL(relative, base)`：WHATWG relative-URL 解析（`base` 必须是已解析的 URL）。
///
/// 见 `js_url` 的模块文档。
fn new_url_relative(base: &Url, relative: &str) -> Result<Url, OAuthFailure> {
    js_url::parse_relative(base, relative).map_err(|_| invalid_url())
}

/// JS 的 `URL.origin`（HTML 的 origin 序列化），返回 `"null"` 表示不透明 origin。
///
/// 与 [`Url::origin`] 的差别全部在 `blob:`（见模块文档的「URL 语义」）：
/// - 内层 URL 的 scheme 是 `http`/`https`/`file` 时才取内层 origin（`file:` 本身是不透明的）；
/// - 其他内层 scheme（`ftp`、`ws`、`wss`、嵌套 `blob:` …）与解析失败都是 `"null"`。
///
/// 其余 scheme 与 `url` crate 一致（`ftp`/`ws`/`wss` 是 tuple origin，`file:`/非特殊 scheme 是
/// `"null"`）。
fn js_origin(url: &Url) -> String {
    if url.scheme() == "blob" {
        return match Url::parse(url.path()) {
            Ok(inner) => match inner.scheme() {
                "http" | "https" => inner.origin().ascii_serialization(),
                _ => "null".to_owned(),
            },
            Err(_) => "null".to_owned(),
        };
    }
    url.origin().ascii_serialization()
}

/// `new URL(relative, url.origin)` 的 base：TS 的 `origin` 是 `scheme://host[:port]` 字符串
/// （不透明 origin 是 `"null"`，用作 base 时抛 `TypeError: Invalid URL`）。
///
/// 非 `blob:` URL 直接由已解析的 [`Url`] 改出 base（清掉 userinfo/path/query/fragment），
/// **不**重新解析 `origin().ascii_serialization()`：那个字符串对 `.well-known` 这类主机只是
/// 文本，重新解析会多走一遍 `idna`，失败面比 TS 大。
///
/// `blob:` URL 是 cannot-be-a-base，`set_path("/")` 只会把路径写成 `%2F`（得到 `blob:%2F`），
/// 因此必须像 TS 一样从 origin **字符串**重建 base——这也是 `blob:https://host/uuid` 的发现
/// 请求落在 `https://host/...` 的唯一途径。
fn origin_base(url: &Url) -> Result<Url, OAuthFailure> {
    let origin = js_origin(url);
    if origin == "null" {
        // 不透明 origin（非特殊 scheme、`file:`、`blob:` 的非 http(s) 内层）：
        // `new URL(rel, "null")` 抛 TypeError。
        return Err(invalid_url());
    }
    if url.scheme() == "blob" {
        return new_url(&origin);
    }
    let mut base = url.clone();
    base.set_path("/");
    base.set_query(None);
    base.set_fragment(None);
    base.set_username("").map_err(|_| invalid_url())?;
    base.set_password(None).map_err(|_| invalid_url())?;
    Ok(base)
}

pub(crate) mod js_url {
    //! `new URL()` 的等价物：`url` crate + WHATWG 的两处修正。
    //!
    //! 不是 TS 里的文件：`oauth/types.ts` 与 `oauth/discovery.ts` 都直接调用 JS 的 `URL`，本模块
    //! 把那份语义收成一份实现。**不**进任何再导出面（TS 没有对应名字）。
    //!
    //! `oauth/types.ts`（`safeUrl` 的 `URL.canParse` + `new URL`）与 `oauth/discovery.ts`（发现 URL、
    //! `resourceUrlFromServerUrl`、`selectResource`、`parseWwwAuthenticate` 的 `resourceMetadataUrl`）
    //! 都直接调用 JS 的 `URL`，所以两处共用这一份实现。
    //!
    //! `url` crate 已经实现了 WHATWG 的 URL 解析器，与 Node 的 `URL` 在**结构**上一致（scheme/host/
    //! port/点段/默认端口/大小写/IPv4/IPv6/制表符换行裁剪…都对得上：对 13 万个构造用例与 6 万个随机
    //! 串做过逐字段比对）。本模块只补两处 TS 源里**可观察**的差异：
    //!
    //! 1. **特殊 scheme 的路径百分号编码集合**（`^`）。WHATWG 的 path percent-encode set 对特殊
    //!    scheme 含 `^`，Node 把它写成 `%5E`；`url` crate 的集合里没有 `^`，原样保留。逐码点扫描
    //!    （BMP + 全部星形平面，path/query/fragment/userinfo 四个位置）确认 `^` 是**唯一**的差异
    //!    码点。修正办法：解析后把路径里未转义的 `^` 重写成 `%5E` 再 `set_path`——`set_path` 不会
    //!    重复编码已有的 `%XX`（`url` crate 保留百分号序列），因此对无 `^` 的输入是恒等变换。
    //! 2. **协议相对引用**（`//` 前缀的相对引用）。`new URL("//mcp/x", origin)` 在 TS 里解析到另一个
    //!    主机 `https://mcp/x`；`Url::join` 语义相同，所以 [`parse_relative`] 用 `join` 而不是字符串
    //!    拼接。另外 WHATWG 在**特殊** scheme 的 base 下有 *special authority ignore slashes* 规则
    //!    （见 [`collapse_special_authority_slashes`]），`url` crate 的 `join` 没有，需要先归一化。
    //!
    //! 已知**不能**修掉的差异：`url` crate 的 IDNA（`idna` 1.1，Unicode 16 数据）与 Node 的
    //! `domainToASCII`（ICU 78，Unicode 17 数据）对少数码点的接受度不同，而且 [`url::Url`] 根本
    //! 无法持有被它判为非法的 host：
    //!
    //! - Node 接受、`idna` 拒绝：`xn--a.example` 这类**非法 punycode** 标签，以及 U+061D、
    //!   U+0870–U+088F、U+0897–U+089F、U+08B5、U+08C8–U+08D2、U+1AC1–U+1ADD、U+1AE0–U+1AEB、
    //!   U+107BB 等 ICU 78 已分配而 Unicode 16 未分配的码点；
    //! - Node 拒绝、`idna` 接受：U+0558、U+058B/058C、U+05C8/05C9、U+0B53/0B54、U+1ADE/1ADF、
    //!   U+1AEC–U+1AF0、U+208F、U+209D–U+209F、U+20C2–U+20C4、U+2E60–U+2E63、U+A7DD、U+A7E2、
    //!   U+AB6C/AB6D（Unicode 16 尚未分配而 Node 的 IDNA 数据仍按"未分配"处理）。
    //!
    //! 因此 `safe_url` 可能把 Node 接受的元数据 URL 判成 `Invalid <name>`，发现函数可能对 Node 能
    //! 解析的主机报 `TypeError: Invalid URL`。主机名来自服务器输入，正常部署不会出现这些码点；要彻底
    //! 消除需要把 `idna` 的数据换成与 Node 相同的版本（超出本单元的范围，也无法靠 `url` 的公开 API
    //! 补丁——`Url` 不持有 host 原文）。`tests/fixtures/oauth_url_normalization_golden.json` 里带
    //! `"rust"` 标签的用例把这批差异逐条钉住，`url_normalization_matches_the_typescript_golden_vectors`
    //! 断言它们保持在"Node 接受 / Rust 报错"（或反向）这一已知状态，而不是悄悄改掉。
    //!
    //! 另一处已知差异是 `file:` URL 的重复前导斜杠（`Url` 无法表示，见 `discovery` 模块文档的
    //! 「URL 语义」）。`tests/fixtures/oauth_origin_golden.json` 里带 `"rust"` 标签的用例把
    //! `resourceUrlFromServerUrl`、`parseWwwAuthenticate`、`selectResource` 归一化后的结果钉住。

    use url::Url;

    /// `new URL(input)`（绝对 URL 解析）。
    ///
    /// 失败对应 JS 的 `TypeError: Invalid URL`；调用方各自决定文案（`safeUrl` 是 `Invalid {name}`，
    /// 其余是 `TypeError: Invalid URL`）。
    pub fn parse(input: &str) -> Result<Url, ()> {
        let mut url = Url::parse(input).map_err(|_| ())?;
        normalize_special_path(&mut url);
        Ok(url)
    }

    /// `new URL(relative, base)`。
    ///
    /// `base` 必须是已解析的 `Url`（TS 里调用点的第二参都是已解析的 `origin`）。`cannot-be-a-base`
    /// 的 base（`foo:a`）在 TS 里抛 `TypeError`，`join` 同样报错。
    pub fn parse_relative(base: &Url, relative: &str) -> Result<Url, ()> {
        let relative = collapse_special_authority_slashes(base, relative);
        let mut url = base.join(&relative).map_err(|_| ())?;
        normalize_special_path(&mut url);
        Ok(url)
    }

    /// WHATWG 的 *special authority ignore slashes state*：特殊 scheme 的 base 下，相对引用以两个或
    /// 更多 `/`（或 `\`——特殊 scheme 里两者等价）开头时，**全部**前导斜杠都被吞掉。
    ///
    /// `new URL("///x/y", "https://base")` = `https://x/y`（`url` crate 的 `join` 会报 `EmptyHost`）；
    /// `new URL("////", "https://base")` 因为主机为空而抛 `TypeError`（归一化成 `//` 后 `join` 报同样
    /// 的错）。非特殊 scheme 没有这条规则（`foo:///x/y` 的主机是空串）。
    fn collapse_special_authority_slashes(base: &Url, relative: &str) -> String {
        if !base.is_special() {
            return relative.to_owned();
        }
        let mut chars = relative.chars();
        let is_slash = |ch: Option<char>| matches!(ch, Some('/' | '\\'));
        if !(is_slash(chars.next()) && is_slash(chars.next())) {
            return relative.to_owned();
        }
        let rest = relative.trim_start_matches(['/', '\\']);
        format!("//{rest}")
    }

    /// 把路径里的 `^` 编码成 `%5E`（WHATWG 的 path percent-encode set）。
    ///
    /// 只处理 `cannot_be_a_base() == false` 的 URL：opacity 路径（`foo:a^b`）Node 保持原样，`url`
    /// crate 也一样。hierarchical 的非特殊 scheme（`foo://x/a^b`）Node 同样编码成 `%5E`，所以这里
    /// 不只限特殊 scheme。
    fn normalize_special_path(url: &mut Url) {
        if url.cannot_be_a_base() {
            return;
        }
        let path = url.path();
        if !path.contains('^') {
            return;
        }
        let encoded = path.replace('^', "%5E");
        url.set_path(&encoded);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_matches_whatwg_for_ordinary_urls() {
            let url = parse("https://x.example/a/../b?q=1#f").unwrap();
            assert_eq!(url.as_str(), "https://x.example/b?q=1#f");
            assert_eq!(
                parse("HTTPS://X.EXAMPLE:443/a").unwrap().as_str(),
                "https://x.example/a"
            );
            assert_eq!(
                parse("https://x.example:0/a").unwrap().as_str(),
                "https://x.example:0/a"
            );
            assert_eq!(
                parse("https://x.example:/a").unwrap().as_str(),
                "https://x.example/a"
            );
        }

        #[test]
        fn parse_rejects_relative_and_malformed_inputs() {
            assert!(parse("not a url").is_err());
            assert!(parse("").is_err());
            assert!(parse("/relative").is_err());
        }

        /// Node 26：`new URL("https://x.example/a^b").href === "https://x.example/a%5Eb"`。
        #[test]
        fn parse_encodes_a_caret_in_hierarchical_paths() {
            assert_eq!(
                parse("https://x.example/a^b").unwrap().as_str(),
                "https://x.example/a%5Eb"
            );
            assert_eq!(
                parse("https://x.example/a^b?q=^#^").unwrap().as_str(),
                "https://x.example/a%5Eb?q=^#^"
            );
            // 已有的百分号编码保持不变（不重复编码）。
            assert_eq!(
                parse("https://x.example/a%5eb").unwrap().as_str(),
                "https://x.example/a%5eb"
            );
            assert_eq!(
                parse("https://x.example/a%5Eb").unwrap().as_str(),
                "https://x.example/a%5Eb"
            );
            // 非特殊 scheme 的 hierarchical 路径同样编码；opacity 路径保持原样。
            assert_eq!(
                parse("foo://x.example/a^b").unwrap().as_str(),
                "foo://x.example/a%5Eb"
            );
            assert_eq!(parse("foo:a^b").unwrap().as_str(), "foo:a^b");
        }

        /// Node 26：`new URL("//mcp/x", "https://base")` 换主机；多余的斜杠被吞掉。
        #[test]
        fn parse_relative_implements_protocol_relative_references() {
            let base = Url::parse("https://x.example").unwrap();
            assert_eq!(
                parse_relative(&base, "//mcp/.well-known/openid-configuration")
                    .unwrap()
                    .as_str(),
                "https://mcp/.well-known/openid-configuration"
            );
            assert_eq!(
                parse_relative(&base, "//.well-known/openid-configuration")
                    .unwrap()
                    .as_str(),
                "https://.well-known/openid-configuration"
            );
            assert_eq!(
                parse_relative(&base, "/.well-known/x").unwrap().as_str(),
                "https://x.example/.well-known/x"
            );
            // `special authority ignore slashes`：`///x` 仍是 `https://x/`。
            assert_eq!(
                parse_relative(&base, "///x").unwrap().as_str(),
                "https://x/"
            );
            assert_eq!(
                parse_relative(&base, "////x").unwrap().as_str(),
                "https://x/"
            );
            assert!(parse_relative(&base, "///").is_err());
            assert!(parse_relative(&base, "//").is_err());
            // 主机段非法 → 与 Node 一样是 `TypeError`。
            assert!(parse_relative(&base, "//%2F/x").is_err());
            assert!(parse_relative(&base, "//:1234/x").is_err());
            assert!(parse_relative(&base, "//a b/x").is_err());
        }
    }
}

/// [`discover_protected_resource_metadata`] 的选项（TS 的第二个参数）。
///
/// 所有字段都可省略：`Default` 表示自动推导 URL、用 [`LATEST_PROTOCOL_VERSION`]、
/// 用 [`ReqwestFetch`]、不取消。
#[derive(Clone, Default)]
pub struct DiscoverProtectedResourceOptions {
    /// 显式给定的元数据 URL（跳过 well-known 推导与回退）。
    pub resource_metadata_url: Option<Url>,
    /// `MCP-Protocol-Version` 头的值，缺省 [`LATEST_PROTOCOL_VERSION`]。
    pub protocol_version: Option<String>,
    /// HTTP 栈，缺省 [`ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// 取消信号。
    pub signal: Option<tokio_util::sync::CancellationToken>,
}

impl std::fmt::Debug for DiscoverProtectedResourceOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoverProtectedResourceOptions")
            .field("resource_metadata_url", &self.resource_metadata_url)
            .field("protocol_version", &self.protocol_version)
            .field("fetch", &self.fetch.is_some())
            .field("signal", &self.signal.is_some())
            .finish()
    }
}

/// `discoverProtectedResourceMetadata(serverUrl, options?)`。
///
/// 1. `server = new URL(serverUrl)`；首个 URL 是 `options.resourceMetadataUrl`，否则
///    `{origin}/.well-known/oauth-protected-resource{pathSuffix(server.pathname)}`；
/// 2. 未显式给 URL、`pathname !== "/"` 且首个响应是 discovery miss → 丢弃响应后重试裸路径；
/// 3. 非 ok → `HTTP {status} loading OAuth protected resource metadata`；
/// 4. `parseProtectedResourceMetadata(await response.json())`（JSON 解析失败是 `SyntaxError`）。
///
/// `serverUrl` 解析失败是 `TypeError`（TS 的 `new URL(serverUrl)`）。
pub async fn discover_protected_resource_metadata(
    server_url: impl AsRef<str>,
    options: &DiscoverProtectedResourceOptions,
) -> Result<OAuthProtectedResourceMetadata, OAuthFailure> {
    let server_url = server_url.as_ref();
    // 阶段 1：解析 serverUrl。失败即 `TypeError: Invalid URL`（TS 的 `new URL(serverUrl)`），
    // 此时一个请求都不会发出。协议版本与 HTTP 栈也在这里定下缺省值。
    let server = new_url(server_url)?;
    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    let version = options
        .protocol_version
        .clone()
        .unwrap_or_else(|| LATEST_PROTOCOL_VERSION.to_owned());
    // 阶段 2：确定第一个候选。显式 URL 原样使用（也因此禁用回退）；否则由 origin 拼上
    // well-known 路径与去掉一个尾斜杠的 pathname——`origin_base` 会去掉 userinfo，所以发现
    // 请求不带凭据；路径沿用 URL 解析后的形态（`^` 此时已是 `%5E`）。
    let first_url = match &options.resource_metadata_url {
        Some(url) => url.clone(),
        None => new_url_relative(
            &origin_base(&server)?,
            &format!(
                "/.well-known/oauth-protected-resource{}",
                path_suffix(server.path())
            ),
        )?,
    };
    let mut response =
        fetch_metadata(&first_url, &fetch, &version, options.signal.as_ref()).await?;
    // 阶段 3：只有"自动推导 + 非根路径 + 首个响应是 4xx/502"才回退到裸路径；显式 URL 与
    // 根路径都不重试。重试前先丢弃旧响应体（见 [`discard`]）。
    if options.resource_metadata_url.is_none()
        && server.path() != "/"
        && is_discovery_miss(response.status())
    {
        discard(Some(response)).await;
        response = fetch_metadata(
            &new_url_relative(
                &origin_base(&server)?,
                "/.well-known/oauth-protected-resource",
            )?,
            &fetch,
            &version,
            options.signal.as_ref(),
        )
        .await?;
    }
    // 阶段 4：回退后仍非 2xx 就是终止性错误；消息只带状态码，响应体不进入消息。
    if !response.ok() {
        let status = response.status();
        discard(Some(response)).await;
        return Err(OAuthFailure::error(format!(
            "HTTP {status} loading OAuth protected resource metadata"
        )));
    }
    // 阶段 5：JSON 解析失败是 `SyntaxError`（`McpTransportError`），字段校验失败是
    // `Invalid …` 普通错误；本函数两者都不吞，直接上抛。
    let value = response.json().await?;
    parse_protected_resource_metadata(&value)
}

/// `buildAuthorizationServerDiscoveryUrls(authorizationServerUrl)` 的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryUrl {
    /// 候选 URL。
    pub url: Url,
    /// `"oauth"`（RFC 8414 的授权服务器元数据）或 `"oidc"`（OpenID Connect Discovery）。
    pub kind: DiscoveryUrlKind,
}

/// `DiscoveryUrl.type`（TS 的字面量联合 `"oauth" | "oidc"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryUrlKind {
    /// `"oauth"`。
    OAuth,
    /// `"oidc"`。
    Oidc,
}

impl DiscoveryUrlKind {
    /// TS 的字面量值（`"oauth"` / `"oidc"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            DiscoveryUrlKind::OAuth => "oauth",
            DiscoveryUrlKind::Oidc => "oidc",
        }
    }
}

/// `buildAuthorizationServerDiscoveryUrls(authorizationServerUrl)`。
///
/// 依次是：
/// 1. `{origin}/.well-known/oauth-authorization-server{path}`（`oauth`）；
/// 2. `{origin}/.well-known/openid-configuration{path}`（`oidc`）；
/// 3. 仅当 `path` 非空：`new URL(\`{path}/.well-known/openid-configuration\`, origin)`（`oidc`）。
///
/// 第 3 条是**相对引用**：`path` 以 `//` 开头时它会换主机（`https://x.example//mcp` →
/// `https://mcp/…`），主机段非法时抛 `TypeError`（见 `js_url`）。
/// 前两条以 `/.well-known/…` 开头，相对解析的结果等于 `origin + path`。
///
/// `path = pathSuffix(issuer.pathname)`（去掉恰好一个尾斜杠）。`issuer` 解析失败是 `TypeError`。
pub fn build_authorization_server_discovery_urls(
    authorization_server_url: impl AsRef<str>,
) -> Result<Vec<DiscoveryUrl>, OAuthFailure> {
    let issuer = new_url(authorization_server_url.as_ref())?;
    let base = origin_base(&issuer)?;
    let path = path_suffix(issuer.path()).to_owned();
    let mut urls = vec![
        DiscoveryUrl {
            url: new_url_relative(
                &base,
                &format!("/.well-known/oauth-authorization-server{path}"),
            )?,
            kind: DiscoveryUrlKind::OAuth,
        },
        DiscoveryUrl {
            url: new_url_relative(&base, &format!("/.well-known/openid-configuration{path}"))?,
            kind: DiscoveryUrlKind::Oidc,
        },
    ];
    // 根路径时 `path` 为空，第三条会与第二条完全相同，因此不添加。
    if !path.is_empty() {
        urls.push(DiscoveryUrl {
            url: new_url_relative(&base, &format!("{path}/.well-known/openid-configuration"))?,
            kind: DiscoveryUrlKind::Oidc,
        });
    }
    Ok(urls)
}

/// [`discover_authorization_server_metadata`] 的选项。
///
/// `Default` 表示：默认 HTTP 栈、[`LATEST_PROTOCOL_VERSION`]、**校验** issuer、不取消。
#[derive(Clone, Default)]
pub struct DiscoverAuthorizationServerOptions {
    /// HTTP 栈，缺省 [`ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// `MCP-Protocol-Version` 头的值，缺省 [`LATEST_PROTOCOL_VERSION`]。
    pub protocol_version: Option<String>,
    /// 跳过 issuer 校验（配置的元数据文档被当作可信输入时）。
    pub skip_issuer_validation: bool,
    /// 取消信号。
    pub signal: Option<tokio_util::sync::CancellationToken>,
}

impl std::fmt::Debug for DiscoverAuthorizationServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoverAuthorizationServerOptions")
            .field("fetch", &self.fetch.is_some())
            .field("protocol_version", &self.protocol_version)
            .field("skip_issuer_validation", &self.skip_issuer_validation)
            .field("signal", &self.signal.is_some())
            .finish()
    }
}

/// `trim = value => value.endsWith("/") ? value.slice(0, -1) : value`（只去掉**一个**尾斜杠）。
fn trim_one_trailing_slash(value: &str) -> &str {
    value.strip_suffix('/').unwrap_or(value)
}

/// `discoverAuthorizationServerMetadata(authorizationServerUrl, options?)`。
///
/// 按 [`build_authorization_server_discovery_urls`] 的顺序尝试；discovery miss（4xx/502）
/// 继续下一个候选，其他非 ok 立即报
/// `` HTTP {status} loading authorization server metadata from {url} ``（`url` 是候选本身的
/// `href`）。全部候选耗尽 → `Ok(None)`。
///
/// 除非 `skip_issuer_validation`，`trim(metadata.issuer)` 必须等于
/// `trim(String(authorizationServerUrl))`（`trim` 只去掉一个尾斜杠），否则
/// [`OAuthIssuerMismatchError`]。
pub async fn discover_authorization_server_metadata(
    authorization_server_url: impl AsRef<str>,
    options: &DiscoverAuthorizationServerOptions,
) -> Result<Option<AuthorizationServerMetadata>, OAuthFailure> {
    let authorization_server_url = authorization_server_url.as_ref();
    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    let version = options
        .protocol_version
        .clone()
        .unwrap_or_else(|| LATEST_PROTOCOL_VERSION.to_owned());
    // 候选按固定顺序尝试：第一个成功（2xx、可解析、issuer 校验通过）的候选直接返回。
    // 候选的网络失败经 `?` 中断整个循环；已拿到 2xx 后的 JSON/字段解析失败与 issuer 不匹配
    // 同样立即上抛，都不会退到后续候选。
    for candidate in build_authorization_server_discovery_urls(authorization_server_url)? {
        let mut response =
            fetch_metadata(&candidate.url, &fetch, &version, options.signal.as_ref()).await?;
        // 先丢弃响应体再分类：miss 继续下一个候选，其它状态立即以
        // `HTTP {status} loading authorization server metadata from {候选 href}` 终止。
        if !response.ok() {
            let status = response.status();
            discard(Some(response)).await;
            if is_discovery_miss(status) {
                continue;
            }
            return Err(OAuthFailure::error(format!(
                "HTTP {status} loading authorization server metadata from {}",
                candidate.url.as_str()
            )));
        }
        let value = response.json().await?;
        let metadata = parse_authorization_server_metadata(&value)?;
        if !options.skip_issuer_validation {
            let expected = authorization_server_url;
            // URL 解析会给裸 origin 补尾斜杠，因此比较时两侧各去掉一个尾斜杠。
            // 比较的是**原始字符串**：`expected` 是调用方传入的 authorizationServerUrl（未经
            // URL 规范化），`received` 是元数据里的原文，因此 `^`→`%5E` 这类解析器编码会造成
            // 不匹配（测试 `discovery_urls_encode_a_caret_in_special_scheme_paths` 钉住）。
            let received = metadata.issuer().unwrap_or_default();
            if trim_one_trailing_slash(received) != trim_one_trailing_slash(expected) {
                return Err(OAuthFailure::from(OAuthIssuerMismatchError::new(
                    Some(expected.to_owned()),
                    metadata.issuer().map(str::to_owned),
                )));
            }
        }
        return Ok(Some(metadata));
    }
    Ok(None)
}

/// [`discover_oauth_server_info`] 的选项。
///
/// 注意这里**没有** `protocol_version`：TS 的 `discoverOAuthServerInfo` 同样不接收它，
/// 函数内部的三处请求都固定用 [`LATEST_PROTOCOL_VERSION`]。
#[derive(Clone, Default)]
pub struct DiscoverOAuthServerInfoOptions {
    /// 显式给定的受保护资源元数据 URL（有值时跳过自动推导，也不会回退到裸路径）。
    pub resource_metadata_url: Option<Url>,
    /// 配置的授权服务器元数据文档：**替代**发现，且因为是配置输入而不校验 issuer；
    /// 请求同样带 `MCP-Protocol-Version: LATEST_PROTOCOL_VERSION`。
    pub authorization_server_metadata_url: Option<Url>,
    /// HTTP 栈，缺省 [`ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// 跳过 issuer 校验。
    pub skip_issuer_validation: bool,
    /// 取消信号。
    pub signal: Option<tokio_util::sync::CancellationToken>,
}

impl std::fmt::Debug for DiscoverOAuthServerInfoOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoverOAuthServerInfoOptions")
            .field("resource_metadata_url", &self.resource_metadata_url)
            .field(
                "authorization_server_metadata_url",
                &self.authorization_server_metadata_url,
            )
            .field("fetch", &self.fetch.is_some())
            .field("skip_issuer_validation", &self.skip_issuer_validation)
            .field("signal", &self.signal.is_some())
            .finish()
    }
}

/// `discoverOAuthServerInfo(serverUrl, options?)`。
///
/// - 受保护资源发现失败时吞掉错误（`TypeError` 例外，向上传播），`resourceMetadata` 保持
///   `None`（元数据非法也走这条路）。
/// - 给了 `authorizationServerMetadataUrl` 时用 [`LATEST_PROTOCOL_VERSION`] 取该文档，非 ok →
///   `` HTTP {status} loading authorization server metadata from {url} ``；**不**校验 issuer；
///   返回 `authorizationServerUrl = metadata.issuer`。
/// - 否则 `authorizationServerUrl = resourceMetadata?.authorization_servers?.[0] ??
///   String(new URL("/", serverUrl))`（带尾斜杠的 origin），再按需发现授权服务器元数据。
///
/// 顺序固定：即使给了 `authorization_server_metadata_url`，也会**先**做受保护资源发现
/// （结果仍进入返回值，错误仍按上面的规则吞或抛），然后才取配置文档；此时完全跳过授权
/// 服务器 well-known 发现。
pub async fn discover_oauth_server_info(
    server_url: impl AsRef<str>,
    options: &DiscoverOAuthServerInfoOptions,
) -> Result<OAuthServerInfo, OAuthFailure> {
    let server_url = server_url.as_ref();
    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    // 阶段 1：受保护资源发现。这里没有 protocolVersion 选项，传 `None` 即
    // LATEST_PROTOCOL_VERSION；`resource_metadata_url` 原样透传（有值时禁用回退）。
    let resource_metadata = match discover_protected_resource_metadata(
        server_url,
        &DiscoverProtectedResourceOptions {
            resource_metadata_url: options.resource_metadata_url.clone(),
            protocol_version: None,
            fetch: options.fetch.clone(),
            signal: options.signal.clone(),
        },
    )
    .await
    {
        Ok(metadata) => Some(metadata),
        Err(error) => {
            // `error instanceof TypeError` → 重新抛出；其余吞掉。
            // 于是 HTTP 非 2xx、`SyntaxError`、`Invalid …` 都变成"没有资源元数据"，
            // 后续继续做授权服务器发现。
            if error.is_type_error() {
                return Err(error);
            }
            None
        }
    };
    // 阶段 2a：配置了元数据文档时以它为准——跳过授权服务器 well-known 发现，也跳过 issuer
    // 校验（配置输入被视为可信）；请求固定用 LATEST_PROTOCOL_VERSION。
    if let Some(url) = &options.authorization_server_metadata_url {
        let mut response = fetch_metadata(
            url,
            &fetch,
            LATEST_PROTOCOL_VERSION,
            options.signal.as_ref(),
        )
        .await?;
        if !response.ok() {
            let status = response.status();
            discard(Some(response)).await;
            return Err(OAuthFailure::error(format!(
                "HTTP {status} loading authorization server metadata from {}",
                url.as_str()
            )));
        }
        let value = response.json().await?;
        let metadata = parse_authorization_server_metadata(&value)?;
        let authorization_server_url = metadata.issuer().unwrap_or_default().to_owned();
        return Ok(OAuthServerInfo {
            authorization_server_url,
            authorization_server_metadata: Some(metadata),
            resource_metadata,
        });
    }
    // 阶段 2b：优先用资源元数据广告的第一个授权服务器（原样字符串，不做 URL 规范化）；
    // 没有可用项时回退到 `String(new URL("/", serverUrl))`——带尾斜杠的根 URL，保留 userinfo
    // 与端口（与 well-known 请求使用的 origin 不同：后者经 `origin_base` 去掉了 userinfo）。
    let authorization_server_url = match resource_metadata
        .as_ref()
        .and_then(OAuthProtectedResourceMetadata::first_authorization_server)
    {
        Some(url) => url.to_owned(),
        // `String(new URL("/", serverUrl))`：带尾斜杠的 origin（含 userinfo/端口，`file:` 例外）。
        None => {
            let server = new_url(server_url)?;
            let mut root = js_url::parse_relative(&server, "/").map_err(|_| invalid_url())?;
            root.set_fragment(None);
            root.set_query(None);
            root.as_str().to_owned()
        }
    };
    // 阶段 3：授权服务器发现。TypeError、非 miss 的 HTTP 状态与 issuer 不匹配都不再被吞；
    // 全部候选 miss 时 metadata 为 `None`，但 authorization_server_url 仍照常返回。
    let authorization_server_metadata = discover_authorization_server_metadata(
        &authorization_server_url,
        &DiscoverAuthorizationServerOptions {
            fetch: options.fetch.clone(),
            protocol_version: None,
            skip_issuer_validation: options.skip_issuer_validation,
            signal: options.signal.clone(),
        },
    )
    .await?;
    Ok(OAuthServerInfo {
        authorization_server_url,
        authorization_server_metadata,
        resource_metadata,
    })
}

/// `resourceUrlFromServerUrl(value)`：`new URL(value)` 且清掉 `hash`。
///
/// query 原样保留；`new URL` 完成的规范化（scheme/host 大小写、点段、默认端口）都会保留在
/// 返回值里。解析失败是 `TypeError`（TS 的 `new URL`）。
pub fn resource_url_from_server_url(value: impl AsRef<str>) -> Result<Url, OAuthFailure> {
    let mut url = new_url(value.as_ref())?;
    url.set_fragment(None);
    Ok(url)
}

/// `selectResource(serverUrl, metadata?)`：受保护资源必须覆盖 MCP 服务器。
///
/// - `metadata` 缺省 → `Ok(None)`；
/// - origin 不同 → `` Protected resource {metadata.resource} does not match MCP server {requested} ``；
/// - 两侧路径补一个尾斜杠后比较前缀，不匹配 → 同一错误；
/// - 否则返回 `metadata.resource`。
pub fn select_resource(
    server_url: impl AsRef<str>,
    metadata: Option<&OAuthProtectedResourceMetadata>,
) -> Result<Option<String>, OAuthFailure> {
    let server_url = server_url.as_ref();
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    // TS 先 `resourceUrlFromServerUrl(serverUrl)` 再 `new URL(metadata.resource)`：后者在
    // `resource` 缺失时得到 `new URL(undefined)` → `TypeError: Invalid URL`（`String(undefined)`
    // 是 `"undefined"`，不是合法 URL）。这里照抄，包括求值顺序。
    let requested = resource_url_from_server_url(server_url)?;
    let resource = kiana_json::js_string::js_string(metadata.get("resource"));
    let configured = new_url(&resource)?;
    let mismatch = || {
        OAuthFailure::error(format!(
            "Protected resource {resource} does not match MCP server {}",
            requested.as_str()
        ))
    };
    // `requested.origin !== configured.origin` 比较的是 origin 的**字符串**：`file:` 两边都是
    // `"null"`（相等），`blob:` 只在内层 http(s) 时才有 tuple origin。`Url::origin` 对
    // `blob:ftp://…` / 嵌套 `blob:` 会给出 tuple，与 Node 不同，因此用 [`js_origin`]。
    if js_origin(&requested) != js_origin(&configured) {
        return Err(mismatch());
    }
    // 前缀比较前两侧各补一个尾斜杠（已带则不动），因此按路径段边界匹配：`configured`
    // （元数据里的 resource）的 `/mcp` 能覆盖 `requested` 的 `/mcp/x`，但 `/mcp` 与 `/mcpx`
    // 不匹配。query 与 fragment 不参与比较。
    let requested_path = requested.path();
    let configured_path = configured.path();
    let requested_path = if requested_path.ends_with('/') {
        requested_path.to_owned()
    } else {
        format!("{requested_path}/")
    };
    let configured_path = if configured_path.ends_with('/') {
        configured_path.to_owned()
    } else {
        format!("{configured_path}/")
    };
    if !requested_path.starts_with(&configured_path) {
        return Err(mismatch());
    }
    // 返回 `metadata.resource` 经 `String()` 的原文（不是解析后的 URL）：即使它通过了
    // `new URL`，也可能在大小写、转义或 fragment 上与 `requested` 的规范化形态不同。
    Ok(Some(resource))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// §27.5 的向量（本机 Node 26 实测，见 `tests/fixtures/oauth_www_authenticate_golden.json`）。
    #[test]
    fn www_authenticate_vectors_match_the_ts_implementation() {
        let challenge = parse_www_authenticate(Some(
            "Bearer resource_metadata=\"https://auth.example/.well-known/oauth-protected-resource\", \
             scope=\"repo read\"",
        ));
        assert_eq!(
            challenge.resource_metadata_url.as_ref().map(Url::as_str),
            Some("https://auth.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(challenge.scope.as_deref(), Some("repo read"));
        assert!(challenge.error.is_none());
        assert!(challenge.error_description.is_none());

        let challenge = parse_www_authenticate(Some(
            "Bearer error=\"insufficient_scope\", scope=\"repo admin\", error_description=\"need admin\"",
        ));
        assert_eq!(challenge.scope.as_deref(), Some("repo admin"));
        assert_eq!(challenge.error.as_deref(), Some("insufficient_scope"));
        assert_eq!(challenge.error_description.as_deref(), Some("need admin"));

        // scheme 不是 bearer/dpop → 空对象。
        assert_eq!(
            parse_www_authenticate(Some("Basic realm=\"x\"")),
            OAuthChallenge::default()
        );
        assert_eq!(
            parse_www_authenticate(Some("Bearer,error=\"insufficient_scope\"")),
            OAuthChallenge::default()
        );
        assert_eq!(
            parse_www_authenticate(Some("xerror=\"insufficient_scope\"")),
            OAuthChallenge::default()
        );

        // 空值视为缺省；解析失败的 URL 被吞掉。
        let challenge = parse_www_authenticate(Some("Bearer scope=\"\""));
        assert!(challenge.scope.is_none());
        let challenge = parse_www_authenticate(Some("dpop resource_metadata=\"not a url\""));
        assert!(challenge.resource_metadata_url.is_none());
        assert!(challenge.scope.is_none());

        // 无引号值可解析；`=` 两侧不能有空格；无尾边界检查。
        let challenge = parse_www_authenticate(Some("bearer error=insufficient_scope"));
        assert_eq!(challenge.error.as_deref(), Some("insufficient_scope"));
        let challenge = parse_www_authenticate(Some("Bearer error = \"insufficient_scope\""));
        assert!(challenge.error.is_none());
        let challenge = parse_www_authenticate(Some("Bearer error=insufficient_scopeX"));
        assert_eq!(challenge.error.as_deref(), Some("insufficient_scopeX"));

        // 后续字段搜索会扫过引号内部。
        let challenge = parse_www_authenticate(Some("Bearer error_description=\"bad scope=x\""));
        assert_eq!(challenge.error_description.as_deref(), Some("bad scope=x"));
        assert_eq!(challenge.scope.as_deref(), Some("x\""));
        // `"` 后紧跟字母不构成分隔符。
        let challenge = parse_www_authenticate(Some("Bearer scope=\"a\"error=\"b\""));
        assert_eq!(challenge.scope.as_deref(), Some("a"));
        assert!(challenge.error.is_none());

        // 大小写不敏感；首尾空白；制表符分隔。
        let challenge = parse_www_authenticate(Some("BEARER SCOPE=\"a b\""));
        assert_eq!(challenge.scope.as_deref(), Some("a b"));
        let challenge = parse_www_authenticate(Some("   Bearer error=\"x\""));
        assert_eq!(challenge.error.as_deref(), Some("x"));
        let challenge = parse_www_authenticate(Some("Bearer\t error=\"x\""));
        assert_eq!(challenge.error.as_deref(), Some("x"));

        // 第一个匹配胜出。
        let challenge = parse_www_authenticate(Some("Bearer scope=\"repo read\", scope=\"other\""));
        assert_eq!(challenge.scope.as_deref(), Some("repo read"));
        // 名字前必须是行首/逗号/空白。
        let challenge = parse_www_authenticate(Some(
            "Bearer Xresource_metadata=\"https://evil\" resource_metadata=\"https://ok.example/meta\"",
        ));
        assert_eq!(
            challenge.resource_metadata_url.as_ref().map(Url::as_str),
            Some("https://ok.example/meta")
        );
    }

    #[test]
    fn www_authenticate_handles_null_and_empty_headers() {
        assert_eq!(parse_www_authenticate(None), OAuthChallenge::default());
        assert_eq!(parse_www_authenticate(Some("")), OAuthChallenge::default());
        // 只有 scheme、没有参数。
        let challenge = parse_www_authenticate(Some("Bearer"));
        assert_eq!(challenge, OAuthChallenge::default());
    }

    #[test]
    fn field_returns_none_for_empty_quoted_values() {
        assert_eq!(field("error=\"\"", "error"), None);
        assert_eq!(field("error=\"\"", "error"), None::<String>);
        assert_eq!(field("error", "error"), None);
        assert_eq!(field("error =x", "error"), None);
        assert_eq!(field(",error=x", "error"), Some("x".to_owned()));
        assert_eq!(field("x=1 error=x", "error"), Some("x".to_owned()));
    }

    #[test]
    fn path_suffix_strips_exactly_one_trailing_slash() {
        assert_eq!(path_suffix("/"), "");
        assert_eq!(path_suffix("/mcp"), "/mcp");
        assert_eq!(path_suffix("/mcp/"), "/mcp");
        assert_eq!(path_suffix("/mcp//"), "/mcp/");
        assert_eq!(path_suffix(""), "");
    }

    #[test]
    fn discovery_misses_are_4xx_and_502() {
        assert!(is_discovery_miss(400));
        assert!(is_discovery_miss(404));
        assert!(is_discovery_miss(499));
        assert!(is_discovery_miss(502));
        assert!(!is_discovery_miss(500));
        assert!(!is_discovery_miss(503));
        assert!(!is_discovery_miss(200));
        assert!(!is_discovery_miss(300));
    }

    #[test]
    fn discovery_urls_follow_the_well_known_rules() {
        let urls = build_authorization_server_discovery_urls("https://x.example").unwrap();
        assert_eq!(
            urls.iter()
                .map(|entry| entry.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://x.example/.well-known/oauth-authorization-server",
                "https://x.example/.well-known/openid-configuration",
            ]
        );
        assert_eq!(urls[0].kind, DiscoveryUrlKind::OAuth);
        assert_eq!(urls[1].kind, DiscoveryUrlKind::Oidc);

        let urls = build_authorization_server_discovery_urls("https://x.example/tenant/").unwrap();
        assert_eq!(
            urls.iter()
                .map(|entry| entry.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://x.example/.well-known/oauth-authorization-server/tenant",
                "https://x.example/.well-known/openid-configuration/tenant",
                "https://x.example/tenant/.well-known/openid-configuration",
            ]
        );
        assert_eq!(urls[2].kind, DiscoveryUrlKind::Oidc);

        // 端口、大小写与点段规范化来自 URL 解析。
        let urls =
            build_authorization_server_discovery_urls("HTTPS://X.example:443/a/../b").unwrap();
        assert_eq!(
            urls[0].url.as_str(),
            "https://x.example/.well-known/oauth-authorization-server/b"
        );
        assert_eq!(DiscoveryUrlKind::OAuth.as_str(), "oauth");
        assert_eq!(DiscoveryUrlKind::Oidc.as_str(), "oidc");
    }

    #[test]
    fn resource_url_from_server_url_drops_the_fragment() {
        assert_eq!(
            resource_url_from_server_url("https://x.example/mcp#frag?q=1")
                .unwrap()
                .as_str(),
            "https://x.example/mcp"
        );
        assert_eq!(
            resource_url_from_server_url("https://x.example/mcp?a=1#frag")
                .unwrap()
                .as_str(),
            "https://x.example/mcp?a=1"
        );
        assert!(
            resource_url_from_server_url("not a url")
                .unwrap_err()
                .is_type_error()
        );
    }

    fn resource_metadata(resource: &str) -> OAuthProtectedResourceMetadata {
        let mut metadata = OAuthProtectedResourceMetadata::new();
        metadata.set("resource", Value::String(resource.to_owned()));
        metadata
    }

    #[test]
    fn select_resource_compares_origin_then_path_prefix() {
        assert_eq!(
            select_resource("https://x.example/mcp", None).unwrap(),
            None
        );
        let metadata = resource_metadata("https://x.example/mcp");
        assert_eq!(
            select_resource("https://x.example/mcp", Some(&metadata)).unwrap(),
            Some("https://x.example/mcp".to_owned())
        );
        // 尾斜杠差异被吸收。
        let metadata = resource_metadata("https://x.example/mcp/");
        assert_eq!(
            select_resource("https://x.example/mcp", Some(&metadata)).unwrap(),
            Some("https://x.example/mcp/".to_owned())
        );
        // 配置的路径是请求路径的前缀。
        let metadata = resource_metadata("https://x.example");
        assert_eq!(
            select_resource("https://x.example/mcp", Some(&metadata)).unwrap(),
            Some("https://x.example".to_owned())
        );
        // 反向不成立。
        let metadata = resource_metadata("https://x.example/mcp/deeper");
        assert_eq!(
            select_resource("https://x.example/mcp", Some(&metadata))
                .unwrap_err()
                .message(),
            "Protected resource https://x.example/mcp/deeper does not match MCP server https://x.example/mcp"
        );
        // origin 不同。
        let metadata = resource_metadata("https://y.example/other");
        assert_eq!(
            select_resource("https://x.example/mcp", Some(&metadata))
                .unwrap_err()
                .message(),
            "Protected resource https://y.example/other does not match MCP server https://x.example/mcp"
        );
        // hash 被忽略。
        let metadata = resource_metadata("https://x.example/mcp");
        assert_eq!(
            select_resource("https://x.example/mcp#frag", Some(&metadata)).unwrap(),
            Some("https://x.example/mcp".to_owned())
        );
    }

    /// 前缀比较按**路径段**边界：`/mcp` 与 `/mcpx` 不是前缀关系（补尾斜杠后 `/mcpx/` 不以
    /// `/mcp/` 开头）。
    #[test]
    fn select_resource_does_not_match_partial_segments() {
        let metadata = resource_metadata("https://x.example/mcp");
        assert!(select_resource("https://x.example/mcpx", Some(&metadata)).is_err());
    }

    #[test]
    fn trim_one_trailing_slash_removes_exactly_one() {
        assert_eq!(trim_one_trailing_slash("https://a/"), "https://a");
        assert_eq!(trim_one_trailing_slash("https://a//"), "https://a/");
        assert_eq!(trim_one_trailing_slash("https://a"), "https://a");
    }
}
