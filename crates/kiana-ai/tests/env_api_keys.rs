//! `packages/ai/test/env-api-keys.test.ts`。
//!
//! TS 直接改 `process.env`，用 `afterEach` 还原七个变量；Rust 侧照搬同一手法：本文件用
//! 一个串行锁 + 保存/还原，确保这些进程级变量在测试结束后回到原值（否则并行运行的其它测试
//! 与同一进程内的后续断言都会被污染）。
//!
//! `findEnvKeys`/`getEnvApiKey` 在 scoped env 未命中时回落到进程 env（`getProviderEnvValue`），
//! 所以这里必须真的操作进程 env，不能只传 scoped map。

use std::sync::{Mutex, MutexGuard};

use kiana_ai::env_api_keys::{find_env_keys, get_env_api_key};

/// 被测试改写的环境变量（与 TS 的 `original*` 列表一致）。
const MANAGED: &[&str] = &[
    "COPILOT_GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "ZAI_CODING_CN_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
];

/// 进程 env 是全局状态：串行执行并支持保存/还原。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 测试作用域：进入时清空受管变量，离开时还原（对应 TS 的 `afterEach`）。
struct EnvScope {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

impl EnvScope {
    fn new() -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let saved = MANAGED
            .iter()
            .map(|name| ((*name).to_string(), std::env::var_os(name)))
            .collect();
        for name in MANAGED {
            // SAFETY: ENV_LOCK 保证本进程内只有本测试在改这些变量。
            unsafe { std::env::remove_var(name) };
        }
        EnvScope {
            _guard: guard,
            saved,
        }
    }

    fn set(&self, name: &str, value: &str) {
        // SAFETY: 同上，串行锁保证独占。
        unsafe { std::env::set_var(name, value) };
    }

    fn unset(&self, name: &str) {
        // SAFETY: 同上。
        unsafe { std::env::remove_var(name) };
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            // SAFETY: 同上。还原到进入前的值。
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

// describe("environment API keys")

#[test]
fn does_not_treat_generic_github_tokens_as_github_copilot_credentials() {
    let scope = EnvScope::new();
    scope.set("GH_TOKEN", "gh-token");
    scope.set("GITHUB_TOKEN", "github-token");

    assert_eq!(find_env_keys("github-copilot", None), None);
    assert_eq!(get_env_api_key("github-copilot", None), None);
}

#[test]
fn resolves_github_copilot_credentials_from_copilot_github_token() {
    let scope = EnvScope::new();
    scope.set("COPILOT_GITHUB_TOKEN", "copilot-token");
    scope.set("GH_TOKEN", "gh-token");
    scope.set("GITHUB_TOKEN", "github-token");

    assert_eq!(
        find_env_keys("github-copilot", None),
        Some(vec!["COPILOT_GITHUB_TOKEN"])
    );
    assert_eq!(
        get_env_api_key("github-copilot", None),
        Some("copilot-token".to_string())
    );
}

#[test]
fn resolves_zai_china_coding_plan_credentials_from_zai_coding_cn_api_key() {
    let scope = EnvScope::new();
    scope.set("ZAI_CODING_CN_API_KEY", "zai-coding-cn-token");

    assert_eq!(
        find_env_keys("zai-coding-cn", None),
        Some(vec!["ZAI_CODING_CN_API_KEY"])
    );
    assert_eq!(
        get_env_api_key("zai-coding-cn", None),
        Some("zai-coding-cn-token".to_string())
    );
}

#[test]
fn reports_anthropic_auth_token_but_preserves_oauth_token_api_key_lookup() {
    let scope = EnvScope::new();
    scope.set("ANTHROPIC_AUTH_TOKEN", "auth-token");
    scope.set("ANTHROPIC_OAUTH_TOKEN", "oauth-token");
    scope.set("ANTHROPIC_API_KEY", "api-key");

    assert_eq!(
        find_env_keys("anthropic", None),
        Some(vec![
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY"
        ])
    );
    assert_eq!(
        get_env_api_key("anthropic", None),
        Some("oauth-token".to_string())
    );
}

#[test]
fn does_not_return_anthropic_auth_token_as_an_api_key() {
    let scope = EnvScope::new();
    scope.set("ANTHROPIC_AUTH_TOKEN", "auth-token");
    scope.unset("ANTHROPIC_OAUTH_TOKEN");
    scope.unset("ANTHROPIC_API_KEY");

    assert_eq!(
        find_env_keys("anthropic", None),
        Some(vec!["ANTHROPIC_AUTH_TOKEN"])
    );
    assert_eq!(get_env_api_key("anthropic", None), None);
}

#[test]
fn preserves_anthropic_oauth_token_as_an_api_key() {
    let scope = EnvScope::new();
    scope.unset("ANTHROPIC_AUTH_TOKEN");
    scope.set("ANTHROPIC_OAUTH_TOKEN", "oauth-token");
    scope.unset("ANTHROPIC_API_KEY");

    assert_eq!(
        find_env_keys("anthropic", None),
        Some(vec!["ANTHROPIC_OAUTH_TOKEN"])
    );
    assert_eq!(
        get_env_api_key("anthropic", None),
        Some("oauth-token".to_string())
    );
}

#[test]
fn falls_back_to_anthropic_api_key_for_api_key_lookup() {
    let scope = EnvScope::new();
    scope.unset("ANTHROPIC_AUTH_TOKEN");
    scope.unset("ANTHROPIC_OAUTH_TOKEN");
    scope.set("ANTHROPIC_API_KEY", "api-key");

    assert_eq!(
        get_env_api_key("anthropic", None),
        Some("api-key".to_string())
    );
}
