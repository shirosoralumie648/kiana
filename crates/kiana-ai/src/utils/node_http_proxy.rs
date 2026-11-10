//! `utils/node-http-proxy.ts`——按环境变量（`HTTP_PROXY`、`NO_PROXY`…）解析代理 URL。
//!
//! TS: `packages/ai/src/utils/node-http-proxy.ts`。
//!
//! 逻辑逐条对应 TS：
//! - `getProxyEnvValue(key)`：先小写、再大写，先 scoped env、再进程环境（含 bun sandbox 回退）；
//! - `shouldProxyHostname`：`NO_PROXY` 的 `*`、逗号/空白分隔、可选端口、`*.`/`.domain` 前缀，
//!   全部条目都「不是排除项」时才代理；
//! - 代理值没有 `://` 时按目标的 protocol 补全；
//! - 非 `http:`/`https:` 的代理 URL 报 [`UNSUPPORTED_PROXY_PROTOCOL_MESSAGE`]。
//!
//! 与 TS 的差异：TS 的 `new URL()` 会做完整 WHATWG 解析（punycode、默认端口归一化等）；
//! Rust 用 `url` crate（WHATWG 兼容实现，工作区已 pin）。解析失败时的内插文本按 TS 的行为
//! 固定为 Node `new URL()` 的错误消息 `"Invalid URL"`——**不是** `url::ParseError` 的细分文本。
//!
//! # 为什么单独实现，而不是交给 HTTP 客户端
//!
//! TS 侧 `api/bedrock-converse-stream.ts` 与 `api/openai-codex-responses.ts` 需要自己
//! 决定走哪个代理：Bedrock 默认的 `NodeHttp2Handler` 不支持 http agent，Codex 的 Bun
//! WebSocket 不尊重代理环境变量（两处 TS 源码都有注释说明）。所以这段逻辑在 TS 里是
//! 独立工具。Rust 端口目前**没有生产调用方**：默认 HTTP 客户端
//! `http::reqwest_client` 使用 reqwest 的 `system-proxy` 特性（见该模块文档），Codex 的
//! WebSocket 也不读代理环境变量（`api/openai_codex_responses.rs` 模块文档）。本模块
//! 因此由文件内单测（本文件 `#[cfg(test)] mod tests`）与
//! `tests/node_http_proxy.rs` 共同覆盖，但仍是 crate 的公开面（`pub mod`）。
//! 新增调用方时请先确认上述两条传输路径的差异，不要假设它们已经会走这里的判定。
//!
//! # 执行流程（`resolve_http_proxy_url_for_target`）
//!
//! 1. [`get_proxy_for_url`] 解析目标 URL 的 scheme/host/port，并按 `NO_PROXY` 判定是否
//!    直连；
//! 2. 取 `<scheme>_proxy`，为空再用 `all_proxy`；值缺少 `://` 时按目标的 scheme 补全；
//! 3. 空串表示直连，返回 `Ok(None)`；
//! 4. 解析代理 URL，失败返回 `Err`（文本含 JSON 引号包裹的原值）；scheme 不是
//!    `http`/`https` 也返回 `Err`。
//!
//! # 约束与坑
//!
//! - 每次调用都重新读取进程环境（`get_provider_env_value` 内部用 `std::env::var`），
//!   结果可能随环境变化；并发修改环境变量的安全责任在调用方（测试用全局锁串行化）。
//! - 代理值不做凭据/路径校验，`Url::parse` 会保留 `user:pass@` 与 path；返回的 `Url`
//!   经 `to_string` 会做 WHATWG 归一化（补尾斜杠、scheme/host 小写），不要用原始字符串
//!   相等来比较“用户写的那串”。
//! - 目标 URL 解析失败时，两个 `*_str` 入口按 TS 语义返回 `Ok(None)` / `""`（直连），
//!   而不是报错；只有**代理 URL** 自身非法才会得到 `Err`。
//! - `NO_PROXY` 的匹配是 DNS 后缀式（`.` 边界），不做 IP 网段/CIDR，也不支持中间的
//!   `*`；`*` 只有当整个 `NO_PROXY` 恰好是 `"*"` 时才表示“全部直连”。

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use url::Url;

/// 各 scheme 的默认端口（TS: `DEFAULT_PROXY_PORTS`）。
///
/// 只在目标 URL 未给出端口时用于 `NO_PROXY` 的端口比较；未列出的 scheme 落到 0，表示
/// “端口未知”。`url::Url::port()` 对显式写出的默认端口也返回 `None`（WHATWG 归一化会
/// 省略默认端口），但表里的值与归一化后的结果相同，因此比较结果不受影响。
const DEFAULT_PROXY_PORTS: &[(&str, u16)] = &[
    ("ftp", 21),
    ("gopher", 70),
    ("http", 80),
    ("https", 443),
    ("ws", 80),
    ("wss", 443),
];

/// `getProxyEnv(key, env)`：`env[key] || env[KEY] || process[key] || process[KEY] || ""`。
///
/// 优先级与 TS 的 `||` 链一致：scoped 小写 → scoped 大写 → 进程环境小写 → 进程环境大写。
/// 每一层都把空字符串当作缺失（`filter(|value| !value.is_empty())`），因为 TS 的 `||`
/// 会跳过 `""`；这是与 `??` 的关键区别。返回 `String` 而不是 `Option<String>`，空串即
/// “未配置”。
fn get_proxy_env(key: &str, env: Option<&ProviderEnv>) -> String {
    let lowercase_key = key.to_lowercase();
    let uppercase_key = key.to_uppercase();
    let from_env = |name: &str| -> Option<String> {
        env.and_then(|env| env.get(name))
            .filter(|value| !value.is_empty())
            .cloned()
    };
    from_env(&lowercase_key)
        .or_else(|| from_env(&uppercase_key))
        .or_else(|| get_provider_env_value(&lowercase_key, None))
        .or_else(|| get_provider_env_value(&uppercase_key, None))
        .unwrap_or_default()
}

/// `stripBrackets(host)`：去掉 IPv6 字面量的方括号。
///
/// 只处理“首尾都是方括号”的完整形式；裸 IPv6（`::1`）原样返回。调用方随后统一转小写，
/// 因此这里不处理大小写。
fn strip_brackets(host: &str) -> &str {
    if host.starts_with('[') && host.ends_with(']') && host.len() >= 2 {
        &host[1..host.len() - 1]
    } else {
        host
    }
}

/// `parseNoProxyEntry(entry)`。
///
/// 把一条 `NO_PROXY` 记录解析成 `(host, port)`。
///
/// `port == 0` 表示该记录未指定端口。解析规则：
///
/// 1. 方括号包裹的 IPv6：`[::1]` / `[::1]:8080`，`]` 之后的 `:port` 用 `parseInt` 解析；
/// 2. 含两个及以上冒号且无方括号：整体当作主机（裸 IPv6），端口 0；
/// 3. 恰好一个冒号且后半段能解析出数字：拆成 host/port；
/// 4. 其它情况（含 `example.com:abc` 这种端口不是数字的记录）：整体当作主机，端口 0。
///
/// 返回的端口类型是 `f64` 而不是 `u16`，以保留 JS `Number` 语义：`-1`、`70000` 都保留
/// 原值，不会被截断——它们随后在 [`should_proxy_hostname`] 里与目标端口比较。
fn parse_no_proxy_entry(entry: &str) -> Option<(String, f64)> {
    let trimmed = entry.trim_matches(is_js_whitespace).to_lowercase();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('[')
        && let Some(closing_bracket) = trimmed.find(']')
    {
        let host = trimmed[1..closing_bracket].to_string();
        let rest = &trimmed[closing_bracket + 1..];
        if let Some(port_text) = rest.strip_prefix(':') {
            return Some((host, parse_int_prefix(port_text).unwrap_or(0.0)));
        }
        return Some((host, 0.0));
    }

    if trimmed.contains(':') && trimmed.split(':').count() > 2 {
        return Some((trimmed, 0.0));
    }

    if let Some(colon_index) = trimmed.rfind(':')
        && colon_index == trimmed.find(':').expect("contains a colon")
    {
        let host = trimmed[..colon_index].to_string();
        if let Some(port) = parse_int_prefix(&trimmed[colon_index + 1..]) {
            return Some((host, port));
        }
    }

    Some((trimmed, 0.0))
}

/// JS `/\s/` 的字符集合（与 Rust 的 `char::is_whitespace` 相比：含 U+FEFF、不含 U+0085；
/// VT/FF 两者都含）。
///
/// 用于三处需要 JS 语义的位置：`NO_PROXY` 记录的首尾 trim、按 `/[,\s]/` 切分、以及
/// `Number.parseInt` 跳过前导空白。不能直接换成 `char::is_whitespace`：它不含 U+FEFF
/// （BOM），而 JS 的 `\s` 含；替换会让 `"example.com\u{FEFF}other.com"` 的切分结果与 TS
/// 不一致。现有单测只覆盖 VT/NBSP（这两个字符在 `char::is_whitespace` 中同样为真），
/// U+FEFF 与 U+0085 的集合差异尚无测试覆盖。
fn is_js_whitespace(ch: char) -> bool {
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

/// JS `Number.parseInt(text, 10)` 的前缀解析：`NaN` 表示没有数字。
///
/// 返回 `f64` 以保留 JS `Number` 的语义：负端口（`-1`）是 truthy、超过 65535 的端口不会被
/// 截断，二者都会让 `port !== targetPort` 成立（因此该条 NO_PROXY 记录不匹配）。
fn parse_int_prefix(text: &str) -> Option<f64> {
    // parseInt 跳过 JS 空白后只接受“可选正负号 + ASCII 数字”，遇到第一个非数字字符就
    // 停止；`+`/`-` 后面没有数字（如 `:-`）以及以字母开头（如 `:abc`）都返回 None。
    let text = text.trim_start_matches(is_js_whitespace);
    let bytes = text.as_bytes();
    let mut index = 0usize;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    let digits_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits_start {
        return None;
    }
    text[..index].parse::<f64>().ok()
}

/// `shouldProxyHostname(hostname, port, env)`。
///
/// 返回 `true` 表示目标需要走代理。流程：
///
/// 1. `NO_PROXY` 为空 → `true`（没有排除项，全部走代理）；
/// 2. `NO_PROXY` 整体等于 `"*"` → `false`（全部直连；注意是精确比较，`" * "` 不生效）；
/// 3. 按逗号或 JS 空白切分，**所有**记录都必须“不是排除项”才返回 `true`（TS 的
///    `.every(...)`：任意一条命中排除就直连）。
///
/// 单条记录的判定：空记录、端口非 0 且与目标端口不同 → 不算排除；去掉 `*.`、前导 `.`
/// 或前导 `*` 得到 domain，domain 为空 → 不算排除；目标主机与 domain 完全相等，或以
/// `.domain` 结尾（DNS 后缀边界，`notexample.com` 不会命中 `example.com`）→ 排除。
///
/// 端口比较沿用 JS 真值语义：记录端口 `0`/`-0` 视为“未指定”，不参与比较；负端口
/// （如 `-1`）是 truthy，而目标端口是非负的 `u16`，因此该记录不会排除目标。
fn should_proxy_hostname(hostname: &str, port: u16, env: Option<&ProviderEnv>) -> bool {
    let no_proxy = get_proxy_env("no_proxy", env).to_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }

    let normalized_target_host = strip_brackets(&hostname.to_lowercase()).to_string();

    // TS: `noProxy.split(/[,\s]/)`——JS 的 `\s` 包含 VT/FF/NBSP/U+2028 等。
    no_proxy
        .split(|ch| ch == ',' || is_js_whitespace(ch))
        .all(|entry| {
            let Some((host, entry_port)) = parse_no_proxy_entry(entry) else {
                return true;
            };
            // JS 里 `0`/`-0` 是 falsy：仅当端口非零且不匹配时才跳过该条记录。
            // TS: `if (parsed.port && parsed.port !== port) return true;`
            if entry_port != 0.0 && entry_port != port as f64 {
                return true;
            }

            let mut domain = strip_brackets(&host).to_string();
            if let Some(stripped) = domain.strip_prefix("*.") {
                domain = stripped.to_string();
            } else if domain.starts_with('.') || domain.starts_with('*') {
                // TS 只剥掉一个前导字符：`.example.com`、`*example.com` 归一成
                // `example.com`；中间的 `*` 保持字面量（不支持通配）。
                domain = domain[1..].to_string();
            }

            if domain.is_empty() {
                return true;
            }
            if normalized_target_host == domain {
                return false;
            }
            if normalized_target_host.ends_with(&format!(".{domain}")) {
                return false;
            }
            true
        })
}

/// `getProxyForUrl(targetUrl, env)`：返回代理 URL 文本，空串表示不使用代理。
///
/// # 参数与返回
///
/// - `target_url`：已解析的目标 URL；`scheme` 为空或没有 host 时返回 `""`（无法判断，
///   按直连处理）。
/// - `env`：可选的 scoped 环境变量表；`None` 时只读进程环境。
/// - 返回值是**原始文本**（未解析、未校验 scheme），可能不含 `://`；只有
///   [`resolve_http_proxy_url_for_target`] 会把它解析成 `Url` 并拒绝非 HTTP(S) 协议。
///
/// # 流程
///
/// 1. 取 host、端口；没有显式端口时按 `DEFAULT_PROXY_PORTS` 补默认端口，仅用于
///    `NO_PROXY` 比较；
/// 2. `should_proxy_hostname` 判定直连；
/// 3. 先查 `{scheme}_proxy`，再查 `all_proxy`；空串视为未配置（TS 的 `||`）；
/// 4. 值不含 `://` 时补成 `{scheme}://{value}`（裸 `host:port` 的常见写法）。
pub fn get_proxy_for_url(target_url: &Url, env: Option<&ProviderEnv>) -> String {
    let protocol = target_url.scheme();
    if protocol.is_empty() || target_url.host_str().is_none() {
        return String::new();
    }

    let hostname = strip_brackets(target_url.host_str().unwrap_or_default()).to_string();
    // TS: `parseInt(parsedUrl.port, 10) || DEFAULT_PROXY_PORTS[protocol] || 0`。
    // `Url::port()` 在未显式写端口（以及写了默认端口）时返回 None，正好落到默认表。
    let port = target_url.port().unwrap_or(0);
    let port = if port == 0 {
        DEFAULT_PROXY_PORTS
            .iter()
            .find_map(|(scheme, port)| (*scheme == protocol).then_some(*port))
            .unwrap_or(0)
    } else {
        port
    };
    if !should_proxy_hostname(&hostname, port, env) {
        return String::new();
    }

    // 协议专用变量优先，`all_proxy` 兜底；TS 的 `||` 让空串等同于缺失。
    let mut proxy = get_proxy_env(&format!("{protocol}_proxy"), env);
    if proxy.is_empty() {
        proxy = get_proxy_env("all_proxy", env);
    }
    if !proxy.is_empty() && !proxy.contains("://") {
        // TS：`proxy = `${protocol}://${proxy}``——原始值没有协议时按目标的 scheme 补全。
        proxy = format!("{protocol}://{proxy}");
    }
    proxy
}

/// `UNSUPPORTED_PROXY_PROTOCOL_MESSAGE`。
///
/// 文本是当前稳定契约；`tests/node_http_proxy.rs` 断言错误里包含它，改动需同步测试。
/// 完整的错误还会在后面追加 ` Got {scheme}:`。
pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str = "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

/// `resolveHttpProxyUrlForTarget(targetUrl, env)`。
///
/// 返回 `Ok(None)` 表示不用代理；`Err` 的两条文本是当前稳定契约，改动需同步测试。
///
/// # 错误（`Err(String)`，不是类型化错误）
///
/// - 代理文本不能解析成 URL：`Invalid proxy URL {JSON.stringify(proxy)}: Invalid URL`。
///   这里固定内插 `"Invalid URL"`（Node `new URL()` 的消息），而不是 `url::ParseError`
///   的细分文本；原值用 [`kiana_json::js_string::js_quote`] 转义，模拟
///   `JSON.stringify` 的引号与转义。
/// - 解析成功但 scheme 不是 `http`/`https`：
///   `{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {scheme}:`（scheme 小写、不带冒号，末尾
///   的冒号是消息的一部分）。
///
/// # 注意
///
/// 该函数只校验代理 URL 的协议，不校验端口是否可达、凭据是否正确；`Ok(Some(url))` 仍
/// 可能在连接阶段失败。
pub fn resolve_http_proxy_url_for_target(
    target_url: &Url,
    env: Option<&ProviderEnv>,
) -> Result<Option<Url>, String> {
    let proxy = get_proxy_for_url(target_url, env);
    if proxy.is_empty() {
        return Ok(None);
    }

    let proxy_url = match Url::parse(&proxy) {
        Ok(url) => url,
        Err(_) => {
            // TS 内插的是 Node `new URL()` 抛出的 `error.message`，它恒为 "Invalid URL"
            // （与 `url::ParseError` 的细分文本不同）。
            // 引号与转义用 `js_quote` 复刻 `JSON.stringify(proxy)`；测试
            // `reports_invalid_proxy_urls` 锁定 `Invalid proxy URL "http://[bad": Invalid URL`。
            return Err(format!(
                "Invalid proxy URL {}: Invalid URL",
                kiana_json::js_string::js_quote(&proxy)
            ));
        }
    };

    // `Url::scheme()` 已归一化为小写且不含冒号；既有错误文本的协议部分带冒号，
    // 因此下面的错误格式串在 `{}` 之后补了一个 `:`，以保持错误消息稳定。
    if proxy_url.scheme() != "http" && proxy_url.scheme() != "https" {
        return Err(format!(
            "{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {}:",
            proxy_url.scheme()
        ));
    }

    Ok(Some(proxy_url))
}

/// 便捷入口：以字符串目标调用（TS 的 `string | URL` 联合）。
///
/// TS 的 `parseProxyTargetUrl` 在 `new URL()` 失败时返回 `undefined`，`getProxyForUrl`
/// 因此返回 `""`，最终 `resolveHttpProxyUrlForTarget` 返回 `undefined`——**不是错误**。
/// 这里用 `Url::parse` 复刻：目标串非法（缺 scheme、`127.0.0.1:8080` 等）时返回
/// `Ok(None)`，调用方无法区分“目标非法”与“命中了 NO_PROXY”，二者都按直连处理。
pub fn resolve_http_proxy_url_for_target_str(
    target_url: &str,
    env: Option<&ProviderEnv>,
) -> Result<Option<Url>, String> {
    match Url::parse(target_url) {
        Ok(parsed) => resolve_http_proxy_url_for_target(&parsed, env),
        Err(_) => Ok(None),
    }
}

/// 便捷入口：`getProxyForUrl` 的字符串目标版本。
///
/// 目标串无法解析为 URL 时返回空串（直连），与 TS 的 `parseProxyTargetUrl` 失败路径
/// 一致；代理值本身是否合法不在本函数的检查范围内（见
/// [`resolve_http_proxy_url_for_target`]）。
pub fn get_proxy_for_url_str(target_url: &str, env: Option<&ProviderEnv>) -> String {
    match Url::parse(target_url) {
        Ok(url) => get_proxy_for_url(&url, env),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
        let mut map: IndexMap<String, String> = IndexMap::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), (*value).to_string());
        }
        map
    }

    fn resolve(target: &str, pairs: &[(&str, &str)]) -> Result<Option<String>, String> {
        let env = env(pairs);
        resolve_http_proxy_url_for_target_str(target, Some(&env))
            .map(|url| url.map(|url| url.to_string()))
    }

    #[test]
    fn reads_uppercase_and_lowercase_proxy_variables() {
        assert_eq!(
            resolve(
                "http://example.com/",
                &[("HTTP_PROXY", "http://proxy:8080")]
            )
            .expect("valid"),
            Some("http://proxy:8080/".to_string())
        );
        assert_eq!(
            resolve("http://example.com/", &[("http_proxy", "proxy:8080")]).expect("valid"),
            Some("http://proxy:8080/".to_string())
        );
        // 进程环境的回退是 TS 的行为（`getProviderEnvValue(lowercaseKey)`），因此这里只在
        // `no_proxy=*` 下断言「没有 scoped 值时不代理」，不依赖测试机的真实环境变量。
        assert_eq!(
            resolve("http://example.com/", &[("no_proxy", "*")]).expect("none"),
            None
        );
    }

    #[test]
    fn respects_no_proxy_entries() {
        let proxy = ("http_proxy", "http://proxy:8080");
        assert_eq!(
            resolve("http://example.com/", &[proxy, ("no_proxy", "example.com")])
                .expect("excluded"),
            None
        );
        assert_eq!(
            resolve(
                "http://api.example.com/",
                &[proxy, ("no_proxy", ".example.com")]
            )
            .expect("excluded"),
            None
        );
        assert_eq!(
            resolve("http://example.com/", &[proxy, ("no_proxy", "*")]).expect("excluded"),
            None
        );
        assert_eq!(
            resolve("http://example.com/", &[proxy, ("no_proxy", "other.com")]).expect("proxied"),
            Some("http://proxy:8080/".to_string())
        );
    }

    #[test]
    fn matches_no_proxy_ports() {
        let proxy = ("http_proxy", "http://proxy:8080");
        // 端口不匹配时不排除。
        assert_eq!(
            resolve(
                "http://example.com:9000/",
                &[proxy, ("no_proxy", "example.com:8080")]
            )
            .expect("proxied"),
            Some("http://proxy:8080/".to_string())
        );
        assert_eq!(
            resolve(
                "http://example.com:8080/",
                &[proxy, ("no_proxy", "example.com:8080")]
            )
            .expect("excluded"),
            None
        );
    }

    #[test]
    fn rejects_unsupported_proxy_protocols() {
        let error = resolve(
            "http://example.com/",
            &[("http_proxy", "socks5://proxy:1080")],
        )
        .expect_err("unsupported");
        assert_eq!(
            error,
            "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL. Got socks5:"
        );
    }

    #[test]
    fn reports_invalid_proxy_urls() {
        // TS 内插的是 Node `new URL()` 的错误消息，恒为 "Invalid URL"（不是 url crate 的细分文本）。
        let error =
            resolve("http://example.com/", &[("http_proxy", "http://[bad")]).expect_err("invalid");
        assert_eq!(error, "Invalid proxy URL \"http://[bad\": Invalid URL");
        let error =
            resolve("http://example.com/", &[("http_proxy", "http://   ")]).expect_err("invalid");
        assert_eq!(error, "Invalid proxy URL \"http://   \": Invalid URL");
    }

    #[test]
    fn treats_an_unparseable_target_as_no_proxy() {
        // TS 的 `parseProxyTargetUrl` 在 `new URL()` 失败时返回 undefined → `Ok(None)`，不是错误。
        for target in [
            "not a url",
            "127.0.0.1:8080",
            "//example.com/x",
            "example.com/api",
        ] {
            assert_eq!(
                resolve(target, &[("http_proxy", "http://proxy:8080")]).expect("no error"),
                None,
                "{target}"
            );
        }
    }

    #[test]
    fn splits_no_proxy_on_js_whitespace_and_keeps_unbounded_ports() {
        let proxy = ("http_proxy", "http://proxy:8080");
        // JS 的 `\s` 含 VT/NBSP：两条记录被分开后 example.com 命中。
        assert_eq!(
            resolve(
                "http://example.com/",
                &[proxy, ("no_proxy", "example.com\u{000b}other.com")]
            )
            .expect("excluded"),
            None
        );
        assert_eq!(
            resolve(
                "http://example.com/",
                &[proxy, ("no_proxy", "example.com\u{00a0}other.com")]
            )
            .expect("excluded"),
            None
        );
        // 负端口是 truthy 且不等于目标端口 → 该条记录不匹配（继续代理）。
        assert_eq!(
            resolve("http://[::1]:8080/", &[proxy, ("no_proxy", "[::1]:-1")]).expect("proxied"),
            Some("http://proxy:8080/".to_string())
        );
        // 超过 65535 的端口不截断：70000/65536 都不等于 65535。
        for port in ["70000", "65536"] {
            assert_eq!(
                resolve(
                    "http://example.com:65535/",
                    &[proxy, ("no_proxy", &format!("example.com:{port}"))]
                )
                .expect("proxied"),
                Some("http://proxy:8080/".to_string()),
                "{port}"
            );
        }
    }
}
