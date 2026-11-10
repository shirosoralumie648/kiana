//! `test/oauth.ts` 的逐字移植：从 `~/.kiana/agent/auth.json` 解析真实 API key。
//!
//! TS 侧支持两种凭据：`{type:"api_key", key}` 与 `{type:"oauth", ...}`（过期时用 provider 的
//! `oauth.refresh` 刷新并写回）。Rust 侧本单元只做**只读**部分：
//!
//! - 读文件、解析 JSON（失败静默返回空，与 TS 的 `try/catch` 一致）；
//! - `api_key` 直接返回 `key`；
//! - `oauth` 返回 `access`/`accessToken` 字段（不刷新：刷新逻辑属于 `ai-auth` 单元，见
//!   `spec/ai-auth.md`；测试里需要刷新时用真实 CLI 预置凭据）。
//!
//! 与 TS 的差异：不写回 `auth.json`（Rust 测试进程不应修改用户的凭据文件）。

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

/// `~/.kiana/agent/auth.json`（TS 的 `join(homedir(), ".kiana", "agent", "auth.json")`）。
pub fn auth_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".kiana")
            .join("agent")
            .join("auth.json"),
    )
}

/// `loadAuthStorage()`：文件不存在或解析失败时返回空表。
pub fn load_auth_storage() -> HashMap<String, Value> {
    let Some(path) = auth_path() else {
        return HashMap::new();
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&content) else {
        return HashMap::new();
    };
    map.into_iter().collect()
}

/// `resolveApiKey(provider)` 的只读部分。
///
/// - `{type:"api_key", key}` → `key`；
/// - `{type:"oauth", access}`（或 `accessToken`）→ token；
/// - 其它 → `None`。
pub fn resolve_api_key(provider: &str) -> Option<String> {
    let storage = load_auth_storage();
    let entry = storage.get(provider)?;
    match entry.get("type").and_then(Value::as_str) {
        Some("api_key") => entry.get("key").and_then(Value::as_str).map(str::to_owned),
        Some("oauth") => entry
            .get("access")
            .or_else(|| entry.get("accessToken"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// 测试是否应跳过真实 API 调用：环境变量 `KIANA_TEST_NO_NETWORK` 或 `KIANA_OFFLINE` 为真时跳过。
///
/// TS 侧用 `it.skipIf(!resolveApiKey(...))`；Rust 侧把两者合成一个谓词，让 `#[ignore]`
/// 的测试在被显式 `--ignored` 运行时也能先自查。
pub fn live_tests_disabled() -> bool {
    std::env::var("KIANA_TEST_NO_NETWORK").is_ok_and(|value| !value.is_empty())
        || std::env::var("KIANA_OFFLINE").is_ok_and(|value| !value.is_empty())
}
