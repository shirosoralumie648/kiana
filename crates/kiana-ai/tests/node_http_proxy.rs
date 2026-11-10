//! `packages/ai/test/node-http-proxy.test.ts`。
//!
//! TS 直接改 `process.env` 并在 `afterEach` 还原 12 个代理变量；Rust 侧同样操作进程 env，
//! 用串行锁 + 保存/还原保证互不干扰（scoped `ProviderEnv` 只能覆盖部分断言——「进程 env 别名」
//! 那几个用例必须真的写进程 env）。

use std::sync::{Mutex, MutexGuard};

use kiana_ai::types::ProviderEnv;
use kiana_ai::utils::node_http_proxy::{
    UNSUPPORTED_PROXY_PROTOCOL_MESSAGE, resolve_http_proxy_url_for_target_str,
};
use url::Url;

const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "npm_config_http_proxy",
    "npm_config_https_proxy",
    "npm_config_proxy",
    "npm_config_no_proxy",
];

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvScope {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvScope {
    fn new() -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let saved = PROXY_ENV_KEYS
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        let scope = EnvScope {
            _guard: guard,
            saved,
        };
        scope.reset();
        scope
    }

    fn reset(&self) {
        for key in PROXY_ENV_KEYS {
            // SAFETY: ENV_LOCK 保证本进程内只有本测试改这些变量。
            unsafe { std::env::remove_var(key) };
        }
    }

    fn set(&self, key: &str, value: &str) {
        // SAFETY: 同上。
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            // SAFETY: 同上；还原到进入前的值。
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn resolve(target: &str, env: Option<&ProviderEnv>) -> Result<Option<String>, String> {
    resolve_http_proxy_url_for_target_str(target, env)
        .map(|url| url.map(|url: Url| url.to_string()))
}

fn scoped(pairs: &[(&str, &str)]) -> ProviderEnv {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

// describe("node HTTP proxy resolution")

#[test]
fn respects_no_proxy_exclusions() {
    let scope = EnvScope::new();
    scope.set("HTTPS_PROXY", "http://proxy.example:8080");
    scope.set("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com");

    assert_eq!(
        resolve("https://bedrock-runtime.us-east-1.amazonaws.com", None),
        Ok(None)
    );
}

#[test]
fn resolves_http_and_https_proxy_urls() {
    let scope = EnvScope::new();
    scope.set("HTTPS_PROXY", "http://proxy.example:8080");

    assert_eq!(
        resolve("https://bedrock-runtime.us-east-1.amazonaws.com", None),
        Ok(Some("http://proxy.example:8080/".to_string()))
    );
}

#[test]
fn prefers_scoped_proxy_env_aliases_before_process_env_aliases() {
    let scope = EnvScope::new();
    scope.set("https_proxy", "http://process-proxy.example:8080");

    assert_eq!(
        resolve(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            Some(&scoped(&[(
                "HTTPS_PROXY",
                "http://scoped-proxy.example:8080"
            )])),
        ),
        Ok(Some("http://scoped-proxy.example:8080/".to_string()))
    );
}

#[test]
fn rejects_socks_and_pac_proxy_urls_explicitly() {
    let scope = EnvScope::new();
    scope.set("HTTPS_PROXY", "socks5://proxy.example:1080");

    let error = resolve("https://bedrock-runtime.us-east-1.amazonaws.com", None)
        .expect_err("unsupported proxy protocol");
    assert!(
        error.contains(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE),
        "unexpected error: {error}"
    );
}

#[test]
fn handles_subdomain_wildcards_ipv6_and_ports_in_no_proxy() {
    let scope = EnvScope::new();
    scope.set("HTTPS_PROXY", "http://proxy.example:8080");
    scope.set(
        "NO_PROXY",
        "example.com, .wildcard.org, *.star.net, ::1, [2001:db8::1], 127.0.0.1:8080",
    );

    assert_eq!(resolve("https://example.com", None), Ok(None));
    assert_eq!(resolve("https://api.example.com", None), Ok(None));
    assert_eq!(resolve("https://wildcard.org", None), Ok(None));
    assert_eq!(resolve("https://api.wildcard.org", None), Ok(None));
    assert_eq!(resolve("https://star.net", None), Ok(None));
    assert_eq!(resolve("https://api.star.net", None), Ok(None));
    assert_eq!(
        resolve("https://notexample.com", None),
        Ok(Some("http://proxy.example:8080/".to_string()))
    );

    assert_eq!(resolve("https://[::1]:80", None), Ok(None));
    assert_eq!(resolve("https://[2001:db8::1]", None), Ok(None));
    assert_eq!(resolve("https://127.0.0.1:8080", None), Ok(None));
    assert_eq!(
        resolve("https://127.0.0.1:3000", None),
        Ok(Some("http://proxy.example:8080/".to_string()))
    );
}
