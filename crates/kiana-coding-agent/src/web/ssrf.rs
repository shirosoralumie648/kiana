//! URL / IP / DNS 三层 SSRF 校验。
//!
//! 设计参照：`reference_project/ClaudeCode/src/tools/WebFetchTool/utils.ts` 的
//! 预批准域名与私有地址判断、`reference_project/codex` 的 URL 校验。行为重写：
//!
//! 1. [`validate_url`]：只允许 `http`/`https`；必须有 host；拒绝 URL 内嵌凭据；
//!    host 是 IP 字面量时直接判断；域名时拒绝云元数据专用域名。
//! 2. [`resolve_public_host`]：连接前用 `tokio::net::lookup_host` 重新解析，任一地址
//!    落在私网/链路本地/CGNAT/保留段/云元数据即整条请求失败。这样不存在“预检用公网
//!    IP、连接时重新解析到内网 IP”的 DNS rebinding 窗口。
//! 3. 重定向：调用方拿到响应后必须用 [`validate_redirect_target`] 再校验最终 URL。
//!
//! 与 hooks 的 SSRF 策略不同，WebFetch 拒绝 loopback（`127.0.0.0/8`、`::1`）：
//! hooks 需要访问本地策略 server，WebFetch 没有这个需求。IDE 连接走独立模块，不经过这里。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use thiserror::Error;

/// URL 或地址校验错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UrlError {
    /// URL 为空。
    #[error("URL is empty")]
    Empty,
    /// URL 结构无法解析。
    #[error("invalid URL {url:?}: {message}")]
    Parse {
        /// 原始 URL。
        url: String,
        /// 解析失败原因。
        message: String,
    },
    /// scheme 不是 http/https。
    #[error("URL scheme {scheme:?} is not allowed (only http/https)")]
    Scheme {
        /// 违规 scheme。
        scheme: String,
    },
    /// 没有 host。
    #[error("URL has no host: {url:?}")]
    MissingHost {
        /// 原始 URL。
        url: String,
    },
    /// URL 内嵌用户名/密码。
    // 不回显原始 URL：它可能包含凭据。
    #[error("URL credentials are not allowed")]
    Credentials,
    /// 命中云元数据地址。
    #[error("URL host {host:?} is a cloud metadata endpoint")]
    Metadata {
        /// 违规 host。
        host: String,
    },
    /// 命中私网 / 链路本地 / 保留地址。
    #[error("URL host {host:?} is a blocked private, link-local or reserved address")]
    BlockedAddress {
        /// 违规 host。
        host: String,
    },
    /// DNS 没有返回地址。
    #[error("DNS returned no addresses for {host:?}")]
    DnsEmpty {
        /// 查询的 host。
        host: String,
    },
    /// DNS 解析失败。
    #[error("DNS resolution failed for {host:?}: {message}")]
    DnsError {
        /// 查询的 host。
        host: String,
        /// 底层错误文本。
        message: String,
    },
    /// 重定向目标被拒绝（错误文本带原目标，便于排查）。
    #[error("redirect to blocked URL {url:?}: {reason}")]
    BlockedRedirect {
        /// 最终 URL。
        url: String,
        /// 具体原因。
        reason: String,
    },
}

/// 解析后的 HTTP(S) URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    /// 原始输入（trim 后）。
    pub raw: String,
    /// 小写 scheme。
    pub scheme: String,
    /// 小写 host（IPv6 不带方括号）。
    pub host: String,
    /// 端口；未写时按 scheme 取 80/443。
    pub port: u16,
    /// 从 path 开始的剩余部分（空串或 `/...`）。
    pub path_and_query: String,
}

impl ParsedUrl {
    /// 重新拼出可发送的 URL（host 为 IPv6 时补方括号）。
    pub fn to_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default_port = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default_port {
            format!("{}://{}{}", self.scheme, host, self.path_and_query)
        } else {
            format!(
                "{}://{}:{}{}",
                self.scheme, host, self.port, self.path_and_query
            )
        }
    }

    /// host 是否是 IP 字面量。
    pub fn host_is_ip(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }
}

/// 云元数据专用域名（IP 形式由 [`is_metadata_ip`] 覆盖）。
const BLOCKED_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "metadata.azure.internal",
    "instance-data",
    "instance-data.ec2.internal",
];

/// 解析并校验一个 Web URL；loopback 也拒绝。
pub fn validate_url(raw: &str) -> Result<ParsedUrl, UrlError> {
    validate_url_with(raw, false)
}

/// 解析并校验一个 Web URL。
///
/// `allow_loopback` 只应给测试/本地服务使用；WebFetch 生产路径固定传 `false`。
pub fn validate_url_with(raw: &str, allow_loopback: bool) -> Result<ParsedUrl, UrlError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(UrlError::Empty);
    }
    let (scheme, rest) = raw.split_once("://").ok_or_else(|| UrlError::Parse {
        url: raw.to_string(),
        message: "missing scheme separator".to_string(),
    })?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(UrlError::Scheme { scheme });
    }
    if rest.is_empty() {
        return Err(UrlError::MissingHost {
            url: raw.to_string(),
        });
    }

    // authority 到第一个 `/`、`?` 或 `#` 为止。
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path_and_query = if rest[authority_end..].starts_with('/') {
        rest[authority_end..].to_string()
    } else if rest[authority_end..].is_empty() {
        "/".to_string()
    } else {
        format!("/{}", &rest[authority_end..])
    };
    if authority.is_empty() {
        return Err(UrlError::MissingHost {
            url: raw.to_string(),
        });
    }
    if authority.contains('@') {
        return Err(UrlError::Credentials);
    }

    let (host, port_text) = split_authority(authority).ok_or_else(|| UrlError::Parse {
        url: raw.to_string(),
        message: "invalid authority".to_string(),
    })?;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlError::MissingHost {
            url: raw.to_string(),
        });
    }
    let default_port = if scheme == "https" { 443 } else { 80 };
    let port = match port_text {
        Some(value) if !value.is_empty() => {
            let parsed = value.parse::<u32>().map_err(|_| UrlError::Parse {
                url: raw.to_string(),
                message: format!("invalid port {value:?}"),
            })?;
            if parsed == 0 || parsed > u16::MAX as u32 {
                return Err(UrlError::Parse {
                    url: raw.to_string(),
                    message: format!("port {parsed} out of range"),
                });
            }
            parsed as u16
        }
        _ => default_port,
    };

    if let Ok(ip) = host.parse::<IpAddr>() {
        reject_ip(ip, &host, allow_loopback)?;
    } else if BLOCKED_HOSTS.contains(&host.as_str()) {
        return Err(UrlError::Metadata { host });
    }

    Ok(ParsedUrl {
        raw: raw.to_string(),
        scheme,
        host,
        port,
        path_and_query,
    })
}

/// 拆分 authority 为 host 与可选端口，支持 `[::1]:8080`。
fn split_authority(authority: &str) -> Option<(String, Option<&str>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        let after = &rest[close + 1..];
        if after.is_empty() {
            return Some((host.to_string(), None));
        }
        let port = after.strip_prefix(':')?;
        return Some((host.to_string(), Some(port)));
    }
    match authority.matches(':').count() {
        0 => Some((authority.to_string(), None)),
        1 => {
            let (host, port) = authority.rsplit_once(':')?;
            Some((host.to_string(), Some(port)))
        }
        // 不带方括号的 IPv6 无法可靠拆分，按非法 URL 处理。
        _ => None,
    }
}

fn reject_ip(ip: IpAddr, host: &str, allow_loopback: bool) -> Result<(), UrlError> {
    if is_metadata_ip(ip) {
        return Err(UrlError::Metadata {
            host: host.to_string(),
        });
    }
    if is_blocked_ip(ip, allow_loopback) {
        return Err(UrlError::BlockedAddress {
            host: host.to_string(),
        });
    }
    Ok(())
}

/// 是否是云元数据 IP（`169.254.169.254`、阿里云 `100.100.100.200`，以及 IPv4 映射 /
/// NAT64 / 6to4 等把 IPv4 嵌在中间的 IPv6 形式）。
pub fn is_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_metadata_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_metadata_v4(v4);
            }
            if let Some(v4) = embedded_ipv4(v6) {
                return is_metadata_v4(v4);
            }
            false
        }
    }
}

fn is_metadata_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets == [169, 254, 169, 254] || octets == [100, 100, 100, 200]
}

/// 是否必须拒绝的地址。
///
/// `allow_loopback = false` 时 loopback（127/8、::1）也拒绝；WebFetch 固定使用该模式。
pub fn is_blocked_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4, allow_loopback),
        IpAddr::V6(v6) => is_blocked_v6(v6, allow_loopback),
    }
}

fn is_blocked_v4(ip: Ipv4Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    if ip.is_unspecified() || ip.is_broadcast() {
        return true;
    }
    let [a, b, _, _] = ip.octets();
    // 0.0.0.0/8“this” network。
    if a == 0 {
        return true;
    }
    // 10.0.0.0/8。
    if a == 10 {
        return true;
    }
    // 100.64.0.0/10，CGNAT / shared address space（含阿里云元数据网段）。
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    // 169.254.0.0/16，link-local。
    if a == 169 && b == 254 {
        return true;
    }
    // 172.16.0.0/12。
    if a == 172 && (16..=31).contains(&b) {
        return true;
    }
    // 192.168.0.0/16。
    if a == 192 && b == 168 {
        return true;
    }
    // 198.18.0.0/15，benchmarking。
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    // 224.0.0.0/4 组播与 240.0.0.0/4 保留段。
    if (224..=239).contains(&a) || a >= 240 {
        return true;
    }
    false
}

fn is_blocked_v6(ip: Ipv6Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4, allow_loopback);
    }
    if let Some(v4) = ip.to_ipv4() {
        return is_blocked_v4(v4, allow_loopback);
    }
    // NAT64 / 6to4 把 IPv4 嵌在中间，`to_ipv4()` 看不到；不检查会被翻译器带去内网。
    if let Some(v4) = embedded_ipv4(ip) {
        return is_blocked_v4(v4, allow_loopback);
    }
    let segments = ip.segments();
    // fc00::/7，unique local。
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // fe80::/10，link-local。
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    false
}

/// 从 NAT64（`64:ff9b::/96`）或 6to4（`2002::/16`）地址里取出被嵌入的 IPv4。
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    if segments[0] == 0x0064
        && segments[1] == 0xff9b
        && segments[2..6].iter().all(|segment| *segment == 0)
    {
        return Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        ));
    }
    if segments[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        ));
    }
    None
}

/// 连接前解析并校验 host；IP 字面量不触发 DNS。
///
/// 返回去重后的地址列表；任一地址被禁则整条请求失败（不会“挑一个安全的地址”）。
pub async fn resolve_public_host(
    parsed: &ParsedUrl,
    allow_loopback: bool,
) -> Result<Vec<IpAddr>, UrlError> {
    if let Ok(ip) = parsed.host.parse::<IpAddr>() {
        reject_ip(ip, &parsed.host, allow_loopback)?;
        return Ok(vec![ip]);
    }
    let addrs = tokio::net::lookup_host((parsed.host.as_str(), parsed.port))
        .await
        .map_err(|error| UrlError::DnsError {
            host: parsed.host.clone(),
            message: error.to_string(),
        })?;
    let mut checked = Vec::new();
    for addr in addrs {
        let ip = addr.ip();
        reject_ip(ip, &parsed.host, allow_loopback).map_err(|error| match error {
            UrlError::Metadata { host } => UrlError::Metadata { host },
            UrlError::BlockedAddress { host } => UrlError::BlockedAddress { host },
            other => other,
        })?;
        if !checked.contains(&ip) {
            checked.push(ip);
        }
    }
    if checked.is_empty() {
        return Err(UrlError::DnsEmpty {
            host: parsed.host.clone(),
        });
    }
    Ok(checked)
}

/// 校验重定向后的最终 URL；与初始 URL 相同的检查强度。
pub fn validate_redirect_target(raw: &str) -> Result<ParsedUrl, UrlError> {
    validate_url(raw).map_err(|error| UrlError::BlockedRedirect {
        url: raw.to_string(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_http_and_https() {
        let url = validate_url("https://example.com/a?b=1#c").expect("valid");
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.path_and_query, "/a?b=1#c");
        assert_eq!(url.to_url(), "https://example.com/a?b=1#c");

        let url = validate_url("http://example.com").expect("valid");
        assert_eq!(url.port, 80);
        assert_eq!(url.path_and_query, "/");
    }

    #[test]
    fn rejects_non_http_schemes_and_credentials() {
        assert!(matches!(
            validate_url("file:///etc/passwd"),
            Err(UrlError::Scheme { .. })
        ));
        assert!(matches!(
            validate_url("ftp://example.com"),
            Err(UrlError::Scheme { .. })
        ));
        assert!(matches!(
            validate_url("https://user:pass@example.com/"),
            Err(UrlError::Credentials)
        ));
    }

    #[test]
    fn rejects_private_link_local_and_metadata_literals() {
        for raw in [
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://172.16.3.4/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.100.100.200/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            // NAT64 / 6to4 嵌入的 IPv4 必须按 IPv4 规则拒绝。
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[2002:a9fe:a9fe::]/",
        ] {
            assert!(validate_url(raw).is_err(), "{raw} should be rejected");
        }
        assert!(matches!(
            validate_url("http://169.254.169.254/"),
            Err(UrlError::Metadata { .. })
        ));
        assert!(matches!(
            validate_url("http://metadata.google.internal/"),
            Err(UrlError::Metadata { .. })
        ));
    }

    #[test]
    fn loopback_can_be_allowed_for_explicit_local_use() {
        assert!(validate_url("http://127.0.0.1:8080/").is_err());
        let url = validate_url_with("http://127.0.0.1:8080/health", true).expect("allowed");
        assert_eq!(url.port, 8080);
    }

    #[test]
    fn ipv6_and_ports_round_trip() {
        let url = validate_url("https://[2001:db8::1]:8443/x").expect("valid");
        assert_eq!(url.host, "2001:db8::1");
        assert_eq!(url.port, 8443);
        assert_eq!(url.to_url(), "https://[2001:db8::1]:8443/x");
    }

    #[test]
    fn address_classification_matches_ranges() {
        assert!(is_blocked_ip("0.0.0.0".parse().unwrap(), false));
        assert!(is_blocked_ip("100.64.0.1".parse().unwrap(), false));
        assert!(is_blocked_ip("198.18.0.1".parse().unwrap(), false));
        assert!(is_blocked_ip("224.0.0.1".parse().unwrap(), false));
        assert!(is_blocked_ip("255.255.255.255".parse().unwrap(), false));
        assert!(!is_blocked_ip("8.8.8.8".parse().unwrap(), false));
        assert!(!is_blocked_ip(
            "2001:4860:4860::8888".parse().unwrap(),
            false
        ));
        assert!(is_metadata_ip("169.254.169.254".parse().unwrap()));
        assert!(is_metadata_ip("::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_metadata_ip("64:ff9b::a9fe:a9fe".parse().unwrap()));
        assert!(is_metadata_ip("2002:a9fe:a9fe::".parse().unwrap()));
        assert!(is_blocked_ip("64:ff9b::a00:1".parse().unwrap(), false));
        assert!(is_blocked_ip("2002:a00:1::1".parse().unwrap(), false));
        assert!(!is_blocked_ip("64:ff9b::808:808".parse().unwrap(), false));
        assert!(!is_blocked_ip("2002:808:808::1".parse().unwrap(), false));
    }

    #[test]
    fn redirect_targets_are_revalidated_with_actionable_error() {
        let error = validate_redirect_target("http://127.0.0.1/admin").expect_err("blocked");
        match error {
            UrlError::BlockedRedirect { url, reason } => {
                assert_eq!(url, "http://127.0.0.1/admin");
                assert!(reason.contains("blocked"), "reason: {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn literal_ip_resolution_does_not_need_network() {
        let url = validate_url("https://93.184.216.34/path").expect("valid");
        let addrs = resolve_public_host(&url, false).await.expect("resolved");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "93.184.216.34");
    }
}
