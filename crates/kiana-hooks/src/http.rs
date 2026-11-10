//! HTTP hook 执行器：URL/header 白名单、SSRF、手动重定向重新校验。

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION};
use reqwest::{Client, Method};
use url::Url;

use crate::command::{CappedOutput, MAX_HOOK_OUTPUT_BYTES};
use crate::config::HttpHookConfig;
use crate::error::HookError;
use crate::output::{HookJsonOutput, ParsedStdout, parse_stdout};
use crate::ssrf::{self, GuardedDnsResolver};

/// 最多跟随的重定向跳数。
const MAX_REDIRECTS: usize = 5;

/// HTTP hook 的全局策略。
///
/// 由 [`crate::HookRunnerConfig::http_policy`] 提供：
/// - `allowed_url_patterns` 非空时，URL 必须匹配其中之一；`*` 是通配符。
/// - `allowed_env_vars` 为 `Some` 时，每个 hook 的 `allowedEnvVars` 还要再与它取
///   交集（managed policy 收紧用户配置）；`None` 表示不额外限制。
#[derive(Debug, Clone, Default)]
pub struct HttpHookPolicy {
    /// URL 白名单（glob，`*` 匹配任意字符）。
    pub allowed_url_patterns: Vec<String>,
    /// 全局允许插值的环境变量名。
    pub allowed_env_vars: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CompiledHttpPolicy {
    url_patterns: Vec<Regex>,
    env_allowlist: Option<HashSet<String>>,
}

/// 编译 URL 白名单正则。非法正则返回 [`HookError::Config`]。
pub(crate) fn compile_policy(policy: &HttpHookPolicy) -> Result<CompiledHttpPolicy, HookError> {
    let mut url_patterns = Vec::with_capacity(policy.allowed_url_patterns.len());
    for pattern in &policy.allowed_url_patterns {
        let regex = compile_url_pattern(pattern).map_err(|error| {
            HookError::Config(format!(
                "invalid allowed URL pattern {pattern:?} for HTTP hooks: {error}"
            ))
        })?;
        url_patterns.push(regex);
    }
    Ok(CompiledHttpPolicy {
        url_patterns,
        env_allowlist: policy
            .allowed_env_vars
            .as_ref()
            .map(|names| names.iter().cloned().collect()),
    })
}

fn compile_url_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    let escaped = regex::escape(pattern).replace(r"\*", ".*");
    Regex::new(&format!("(?i)^{escaped}$"))
}

pub(crate) fn build_client() -> Result<Client, HookError> {
    Client::builder()
        // 重定向必须手动处理：每一跳都要重新校验 URL、重新做 DNS 校验，并在跨源时
        // 剥离凭据 header。`Policy::none()` 保证 reqwest 不会偷偷跟随。
        .redirect(reqwest::redirect::Policy::none())
        // 不走环境变量代理：代理会在代理端解析目标域名，绕过本进程的 DNS 校验。
        .no_proxy()
        .dns_resolver(GuardedDnsResolver)
        .build()
        .map_err(|error| {
            HookError::Config(format!("failed to build the HTTP hook client: {error}"))
        })
}

/// 一次 HTTP hook 的执行结果。
#[derive(Debug, Clone)]
pub(crate) struct HttpRun {
    pub status: Option<u16>,
    pub body: CappedOutput,
    pub json: Option<HookJsonOutput>,
    pub timed_out: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub duration: Duration,
}

struct HttpExecution {
    status: u16,
    body: CappedOutput,
    warnings: Vec<String>,
}

/// 执行一个 HTTP hook。总超时覆盖重定向链的全部请求。
pub(crate) async fn run_http(
    client: &Client,
    hook: &HttpHookConfig,
    policy: &CompiledHttpPolicy,
    input_json: &str,
) -> HttpRun {
    let started = Instant::now();
    match tokio::time::timeout(hook.timeout, execute(client, hook, policy, input_json)).await {
        Err(_) => HttpRun {
            status: None,
            body: CappedOutput::default(),
            json: None,
            timed_out: true,
            error: Some(format!(
                "HTTP hook timed out after {}s",
                hook.timeout.as_secs_f32()
            )),
            warnings: Vec::new(),
            duration: started.elapsed(),
        },
        Ok(Err(error)) => HttpRun {
            status: None,
            body: CappedOutput::default(),
            json: None,
            timed_out: false,
            error: Some(error),
            warnings: Vec::new(),
            duration: started.elapsed(),
        },
        Ok(Ok(execution)) => {
            let mut warnings = execution.warnings;
            let json = if (200..300).contains(&execution.status) {
                if execution.body.text.trim().is_empty() {
                    None
                } else {
                    match parse_stdout(&execution.body.text) {
                        ParsedStdout::Json(json) => Some(json),
                        ParsedStdout::Empty => None,
                        ParsedStdout::Plain(_) => {
                            warnings.push(
                                "HTTP hook must return JSON; plain-text body was ignored"
                                    .to_string(),
                            );
                            None
                        }
                        ParsedStdout::InvalidJson { error, .. } => {
                            warnings.push(format!("invalid JSON from HTTP hook: {error}"));
                            None
                        }
                    }
                }
            } else {
                warnings.push(format!(
                    "HTTP hook returned non-success status {}",
                    execution.status
                ));
                None
            };
            HttpRun {
                status: Some(execution.status),
                body: execution.body,
                json,
                timed_out: false,
                error: None,
                warnings,
                duration: started.elapsed(),
            }
        }
    }
}

async fn execute(
    client: &Client,
    hook: &HttpHookConfig,
    policy: &CompiledHttpPolicy,
    input_json: &str,
) -> Result<HttpExecution, String> {
    let mut url = ssrf::validate_url(&hook.url).map_err(|error| error.to_string())?;
    check_url_policy(&url, policy)?;

    let (mut headers, mut warnings) = build_headers(hook, policy);
    let mut method = Method::POST;
    let mut redirects = 0usize;

    loop {
        let mut request = client.request(method.clone(), url.clone());
        request = request.headers(headers.clone());
        if method == Method::POST {
            request = request.body(input_json.to_string());
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("HTTP hook request failed: {error}"))?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            if let Some(location) = location {
                if redirects >= MAX_REDIRECTS {
                    return Err(format!(
                        "HTTP hook exceeded the {MAX_REDIRECTS}-redirect limit"
                    ));
                }
                let next = url
                    .join(&location)
                    .map_err(|error| format!("invalid redirect location {location:?}: {error}"))?;
                // 每一跳都重新做 scheme / host / IP 校验。
                ssrf::validate_url(next.as_str())
                    .map_err(|error| format!("redirect target rejected: {error}"))?;
                check_url_policy(&next, policy)?;

                if next.origin() != url.origin() {
                    // 跨源：hook 配置的 header 里可能含自定义凭据（`X-Api-Key` 等），
                    // 无法可靠区分哪些是机密，因此只保留 Content-Type，其余全部丢弃。
                    headers = HeaderMap::new();
                    // 307/308 会保留方法与 body；跨源时 payload（工具输入/transcript
                    // 片段）不应跟着跳转走，统一降级为无 body 的 GET。
                    method = Method::GET;
                    warnings.push(format!(
                        "stripped configured headers and request body on cross-origin redirect to {}",
                        next.origin().ascii_serialization()
                    ));
                }
                // 301/302/303 的浏览器语义是转 GET；307/308 保留 POST body。
                if matches!(status.as_u16(), 301..=303) {
                    method = Method::GET;
                    headers.remove(CONTENT_TYPE);
                }
                url = next;
                redirects += 1;
                continue;
            }
            // 3xx 但没有 Location：按普通非 2xx 响应处理。
        }

        let body = read_capped_body(response).await?;
        return Ok(HttpExecution {
            status: status.as_u16(),
            body,
            warnings,
        });
    }
}

fn check_url_policy(url: &Url, policy: &CompiledHttpPolicy) -> Result<(), String> {
    if policy.url_patterns.is_empty() {
        return Ok(());
    }
    let rendered = url.as_str();
    if policy
        .url_patterns
        .iter()
        .any(|pattern| pattern.is_match(rendered))
    {
        Ok(())
    } else {
        Err(format!(
            "HTTP hook URL {rendered:?} does not match the allowed URL patterns"
        ))
    }
}

fn build_headers(hook: &HttpHookConfig, policy: &CompiledHttpPolicy) -> (HeaderMap, Vec<String>) {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let hook_allowed: HashSet<String> = hook.allowed_env_vars.iter().cloned().collect();
    let allowed: HashSet<String> = match &policy.env_allowlist {
        Some(global) => hook_allowed
            .into_iter()
            .filter(|name| global.contains(name))
            .collect(),
        None => hook_allowed,
    };

    let mut warnings = Vec::new();
    for (name, value) in &hook.headers {
        let (interpolated, interpolation_warnings) =
            interpolate_header_value(value, &allowed, |name| std::env::var(name).ok());
        warnings.extend(interpolation_warnings);
        let sanitized = sanitize_header_value(&interpolated);
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            warnings.push(format!("invalid HTTP header name {name:?}; header skipped"));
            continue;
        };
        match HeaderValue::from_str(&sanitized) {
            Ok(header_value) => {
                headers.insert(header_name, header_value);
            }
            Err(error) => warnings.push(format!(
                "invalid HTTP header value for {name:?}: {error}; header skipped"
            )),
        }
    }
    (headers, warnings)
}

/// 替换 header 值里的 `$VAR` / `${VAR}`。
///
/// 只有 `allowed` 中的变量会读取进程环境；其它引用替换为空串并产生警告（防止
/// project hook 通过 `$OPENAI_API_KEY` 把密钥发到外部）。
pub(crate) fn interpolate_header_value(
    value: &str,
    allowed: &HashSet<String>,
    lookup: impl Fn(&str) -> Option<String>,
) -> (String, Vec<String>) {
    static ENV_REFERENCE: OnceLock<Regex> = OnceLock::new();
    let pattern = ENV_REFERENCE.get_or_init(|| {
        Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")
            .expect("static env-var regex compiles")
    });

    let mut warnings = Vec::new();
    let mut replaced = String::with_capacity(value.len());
    let mut last = 0usize;
    for captures in pattern.captures_iter(value) {
        let whole = captures.get(0).expect("capture 0 exists");
        replaced.push_str(&value[last..whole.start()]);
        let name = captures
            .get(1)
            .or_else(|| captures.get(2))
            .expect("one capture group exists")
            .as_str();
        if allowed.contains(name) {
            replaced.push_str(&lookup(name).unwrap_or_default());
        } else {
            warnings.push(format!(
                "environment variable ${name} is not in allowedEnvVars; replaced with empty string"
            ));
        }
        last = whole.end();
    }
    replaced.push_str(&value[last..]);
    (sanitize_header_value(&replaced), warnings)
}

/// 去掉 header 值里的 CR / LF / NUL，防止 CRLF 注入。
pub(crate) fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n' | '\0'))
        .collect()
}

async fn read_capped_body(mut response: reqwest::Response) -> Result<CappedOutput, String> {
    let mut bytes = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read HTTP hook response body: {error}"))?
    {
        total += chunk.len();
        let remaining = MAX_HOOK_OUTPUT_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            truncated = true;
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str(&format!(
            "\n...[hook output truncated: kept {} of {} bytes]",
            bytes.len(),
            total
        ));
    }
    Ok(CappedOutput {
        text,
        truncated,
        total_bytes: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_patterns_use_star_as_wildcard() {
        let policy = compile_policy(&HttpHookPolicy {
            allowed_url_patterns: vec!["https://api.example.com/*".to_string()],
            allowed_env_vars: None,
        })
        .expect("policy compiles");
        assert!(
            check_url_policy(
                &Url::parse("https://api.example.com/hook").unwrap(),
                &policy
            )
            .is_ok()
        );
        assert!(
            check_url_policy(
                &Url::parse("https://evil.example.com/hook").unwrap(),
                &policy
            )
            .is_err()
        );
    }

    #[test]
    fn url_patterns_are_escaped_globs_not_raw_regex() {
        // `regex::escape` 会中和正则元字符；白名单始终是“字面量 + `*`”语义，
        // 不会因为用户写了 `([` 就变成非法配置。
        let policy = compile_policy(&HttpHookPolicy {
            allowed_url_patterns: vec!["https://example.com/([".to_string()],
            allowed_env_vars: None,
        })
        .expect("glob patterns are always valid");
        assert!(check_url_policy(&Url::parse("https://example.com/([").unwrap(), &policy).is_ok());
        assert!(check_url_policy(&Url::parse("https://example.com/x").unwrap(), &policy).is_err());
    }

    #[test]
    fn env_interpolation_only_uses_allowed_variables() {
        let allowed: HashSet<String> = ["TOKEN".to_string()].into_iter().collect();
        let (value, warnings) = interpolate_header_value(
            "Bearer $TOKEN $SECRET ${TOKEN}",
            &allowed,
            |name| match name {
                "TOKEN" => Some("abc".to_string()),
                "SECRET" => Some("leak".to_string()),
                _ => None,
            },
        );
        assert_eq!(value, "Bearer abc  abc");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("SECRET"));
    }

    #[test]
    fn header_values_are_sanitized_against_crlf_injection() {
        assert_eq!(sanitize_header_value("a\r\nX-Evil: 1\0"), "aX-Evil: 1");
        let allowed: HashSet<String> = HashSet::new();
        let (value, _) = interpolate_header_value("a\r\nb", &allowed, |_| None);
        assert_eq!(value, "ab");
    }
}
