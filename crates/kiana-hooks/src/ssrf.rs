//! HTTP hook 的 SSRF 防护。
//!
//! 两层校验：
//!
//! 1. **URL 静态校验**（加载配置与每次重定向时）：scheme 只允许 http/https、必须
//!    有 host、不允许 URL 内嵌凭据；host 是 IP 字面量时直接判断，host 是域名时
//!    拒绝云元数据专用域名。
//! 2. **DNS 解析校验**（真正 connect 时）：自定义 [`reqwest::dns::Resolve`] 在解析
//!    结果里发现任何被禁地址就报错。这样不存在“预检用公网 IP、连接时重新解析到
//!    内网 IP”的 DNS rebinding 窗口。
//!
//! 被禁范围：私网（10/8、172.16/12、192.168/16）、链路本地（169.254/16、
//! fe80::/10）、CGNAT（100.64/10）、保留段（0/8、240/4 等）、IPv6 ULA
//! （fc00::/7）与未指定地址。**loopback（127/8、::1）显式允许**，因为本地策略
//! server 是 HTTP hook 的主要用法之一。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use url::Url;

/// URL 校验错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    /// URL 解析失败。
    #[error("invalid URL {url:?}: {message}")]
    Parse {
        /// 原始 URL。
        url: String,
        /// 解析错误。
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
    // 不把原始 URL 写进错误信息：它可能包含用户名/密码，会进入诊断与日志。
    #[error("URL credentials are not allowed")]
    Credentials {
        /// 原始 URL（只用于内部诊断，不进入 Display）。
        url: String,
    },
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
}

/// 云元数据专用域名（IP 形式由 [`is_metadata_ip`] 覆盖）。
const BLOCKED_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata.goog",
    "metadata.azure.internal",
    "instance-data",
    "instance-data.ec2.internal",
];

/// 解析并校验一个 HTTP hook URL。
pub(crate) fn validate_url(raw: &str) -> Result<Url, UrlError> {
    let url = Url::parse(raw).map_err(|error| UrlError::Parse {
        url: raw.to_string(),
        message: error.to_string(),
    })?;

    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(UrlError::Scheme {
                scheme: scheme.to_string(),
            });
        }
    }

    let host = url.host_str().ok_or_else(|| UrlError::MissingHost {
        url: raw.to_string(),
    })?;
    if url.host().is_none() {
        return Err(UrlError::MissingHost {
            url: raw.to_string(),
        });
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Credentials {
            url: raw.to_string(),
        });
    }

    // `Url::host_str()` 对 IPv6 返回带方括号的形式（如 `[fd00::1]`），先剥掉再
    // 解析 IP。
    let host_text = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let normalized = host_text.trim_end_matches('.').to_ascii_lowercase();

    if let Ok(ip) = normalized.parse::<IpAddr>() {
        if is_metadata_ip(ip) {
            return Err(UrlError::Metadata {
                host: host.to_string(),
            });
        }
        if is_blocked_ip(ip) {
            return Err(UrlError::BlockedAddress {
                host: host.to_string(),
            });
        }
        return Ok(url);
    }

    if BLOCKED_HOSTS.contains(&normalized.as_str()) {
        return Err(UrlError::Metadata {
            host: host.to_string(),
        });
    }

    Ok(url)
}

/// 是否是云元数据 IP（`169.254.169.254`、阿里云 `100.100.100.200` 及其 IPv4 映射形式）。
pub fn is_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_metadata_v4(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(is_metadata_v4).unwrap_or(false),
    }
}

fn is_metadata_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets == [169, 254, 169, 254] || octets == [100, 100, 100, 200]
}

/// 是否是 HTTP hook 必须拒绝的地址。loopback 返回 `false`。
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    // loopback 显式允许（本地策略 server）。
    if ip.is_loopback() {
        return false;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let [a, b, _, _] = ip.octets();
    // 0.0.0.0/8、“this” network。
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
    // 127.0.0.0/8 已由 is_loopback 处理。
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
    // 240.0.0.0/4，reserved（含 255.255.255.255）。
    if a >= 240 {
        return true;
    }
    false
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() {
        return false;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    // IPv4-mapped（::ffff:a.b.c.d）与 IPv4-compatible（::a.b.c.d）。
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    if let Some(v4) = ip.to_ipv4() {
        return is_blocked_v4(v4);
    }
    // NAT64 / 6to4 把 IPv4 嵌在中间，`to_ipv4()` 看不到；不检查就会被翻译器带去内网。
    if let Some(v4) = embedded_ipv4(ip) {
        return is_blocked_v4(v4);
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

/// 在 connect 时校验 DNS 结果的 resolver。
#[derive(Debug, Clone, Copy)]
pub(crate) struct GuardedDnsResolver;

impl reqwest::dns::Resolve for GuardedDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if addrs.is_empty() {
                return Err(format!("DNS returned no addresses for {host:?}").into());
            }
            if let Some(blocked) = addrs.iter().find(|addr| is_blocked_ip(addr.ip())) {
                return Err(format!(
                    "HTTP hook host {host:?} resolved to blocked address {}",
                    blocked.ip()
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_allowed_but_private_and_link_local_are_not() {
        for allowed in ["127.0.0.1", "127.255.0.1", "::1"] {
            let ip: IpAddr = allowed.parse().unwrap();
            assert!(!is_blocked_ip(ip), "{allowed} should be allowed");
        }
        for blocked in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "100.100.100.200",
            "169.254.1.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "198.18.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "224.0.0.1",
            "239.255.255.250",
            "ff02::1",
            "64:ff9b::a00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:a00:1::1",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(is_blocked_ip(ip), "{blocked} should be blocked");
        }
    }

    #[test]
    fn metadata_ip_detection_covers_mapped_forms() {
        for metadata in [
            "169.254.169.254",
            "100.100.100.200",
            "::ffff:169.254.169.254",
        ] {
            let ip: IpAddr = metadata.parse().unwrap();
            assert!(is_metadata_ip(ip), "{metadata} should be metadata");
        }
        let normal: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_metadata_ip(normal));
    }

    #[test]
    fn url_validation_rejects_file_and_private_targets() {
        assert!(matches!(
            validate_url("file:///etc/passwd"),
            Err(UrlError::Scheme { .. })
        ));
        assert!(matches!(
            validate_url("http://169.254.169.254/latest/meta-data"),
            Err(UrlError::Metadata { .. })
        ));
        assert!(matches!(
            validate_url("http://10.1.2.3:8080/hook"),
            Err(UrlError::BlockedAddress { .. })
        ));
        assert!(matches!(
            validate_url("http://[fd00:ec2::254]/"),
            Err(UrlError::BlockedAddress { .. })
        ));
        assert!(matches!(
            validate_url("http://user:pass@example.com/"),
            Err(UrlError::Credentials { .. })
        ));
        assert!(matches!(
            validate_url("not a url"),
            Err(UrlError::Parse { .. })
        ));
    }

    #[test]
    fn url_validation_allows_loopback_and_public_hosts() {
        assert!(validate_url("http://127.0.0.1:1234/hook").is_ok());
        assert!(validate_url("http://localhost:1234/hook").is_ok());
        assert!(validate_url("https://example.com/hook").is_ok());
        assert!(matches!(
            validate_url("http://metadata.google.internal/"),
            Err(UrlError::Metadata { .. })
        ));
    }
}
