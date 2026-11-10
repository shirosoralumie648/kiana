//! `env-api-keys.ts`——从环境变量发现 provider 的 API key。
//!
//! TS: `packages/ai/src/env-api-keys.ts`。
//!
//! TS 在 Node/Bun 下用动态 `import("node:fs")`/`import("node:os")`/`import("node:path")`
//! 探测 Google Application Default Credentials 文件；浏览器构建里这三个模块永远是 `null`。
//! Rust 侧用 `std::path::Path::exists` 探测文件、用 `HOME` 展开默认 ADC 路径（不引额外依赖）。
//!
//! 语义要点（逐字照抄 TS）：
//! - `findEnvKeys` 只报**真实的 API key 环境变量**，不含 AWS profile / IAM / ADC 这类“环境凭据”；
//! - `github-copilot` 只认 `COPILOT_GITHUB_TOKEN`（**不是** `GH_TOKEN`/`GITHUB_TOKEN`）；
//! - `anthropic` 的发现顺序是 `ANTHROPIC_AUTH_TOKEN` → `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY`，
//!   但 `getEnvApiKey` **跳过** `ANTHROPIC_AUTH_TOKEN`（它必须走 `Authorization: Bearer`）；
//! - `google-vertex` 在 ADC 文件 + project + location 齐备时返回字面量 `"<authenticated>"`；
//! - `amazon-bedrock` 在任一 AWS 凭据来源存在时返回 `"<authenticated>"`。
//!
//! 空字符串视为“未设置”（`getProviderEnvValue` 用 `||`）。
//!
//! # 调用方
//!
//! - `compat.rs` 的 `with_env_api_key*`：请求缺少显式 `apiKey` 时补上环境 key（跳过哨兵）。
//! - `kiana-extensions` 的 JS 虚拟模块桥：把 `findEnvKeys`/`getEnvApiKey` 暴露给扩展运行时。
//! - `providers/anthropic.rs`：只复用 Anthropic 变量名常量做凭据探测，不调用本模块的查找函数。
//!
//! # 一次 `get_env_api_key` 的取值流程
//!
//! 1. `get_api_key_env_vars(provider)` 给出候选变量表；未知 provider（以及没有 key 变量的
//!    `amazon-bedrock`）在这里就返回 `None`。
//! 2. `find_env_keys` 逐个用 `get_provider_env_value` 取值：scoped `env` 优先，其次进程环境，
//!    最后是 Linux Bun 沙箱的 `/proc/self/environ` 回退（见 `utils::provider_env`）；空串视为未设置。
//! 3. 有命中时按 provider 规则选一个值：`anthropic` 取第一个不是 `ANTHROPIC_AUTH_TOKEN` 的键，
//!    其余取第一个。变量表的顺序因此是语义的一部分，不能随意重排。
//! 4. 没有真实 key 时，再检查 Vertex ADC 与 Bedrock ambient 凭据，命中则返回哨兵
//!    `"<authenticated>"`。
//!
//! # 缓存与并发
//!
//! - Vertex ADC 的探测结果缓存在进程级 `VERTEX_ADC_CACHE`（`Mutex<Option<bool>>`）。
//!   只有“调用方 scoped `env` 里显式给出的 `GOOGLE_APPLICATION_CREDENTIALS`”每次重新检查文件、
//!   **不写缓存**（不同调用方可能带不同 `env`）；其余路径（进程环境变量或默认路径）只探测一次。
//! - 缓存一旦写入就不会感知后续文件系统/环境变化，直到进程重启或测试调用
//!   [`reset_vertex_adc_credentials_cache`]。
//! - 探测路径（`has_vertex_adc_credentials`）在锁中毒时取回内部值继续
//!   （`poisoned.into_inner()`）；[`reset_vertex_adc_credentials_cache`] 不同，它用
//!   `if let Ok(...)` 取锁，锁中毒时**静默跳过**、不清空缓存（旧值继续生效）。
//!   本模块没有网络 IO，也不持锁等待。
//!
//! # 注意事项
//!
//! - `"<authenticated>"` 不是可发送的密钥：`compat` 的 `with_env_api_key*` 会显式跳过它，
//!   由对应适配器自行解析 ambient 凭据（见 `compat.rs` 的 `AMBIENT_AUTH_MARKER`）。任何新
//!   调用方都必须做同样的过滤，否则会把字面量当 API key 发给服务商。
//! - `default_adc_path` 只读 `HOME`，不展开 `~`、也不读平台专属的用户目录变量。

use std::sync::Mutex;

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

// 三个基础凭据变量。顺序即发现顺序：AUTH_TOKEN 参与发现/状态，但 `get_env_api_key`
// 会跳过它——适配器要用 `Authorization: Bearer` 而不是 `x-api-key` 发送它
// （见 `providers/anthropic.rs` 的 `AnthropicApiKeyAuth`）。
/// `ANTHROPIC_AUTH_TOKEN`。
pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
/// `ANTHROPIC_OAUTH_TOKEN`。
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
/// `ANTHROPIC_API_KEY`。
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
// Anthropic workload identity federation 的五个配置变量：`api::anthropic_messages` 在缺少
// API key/OAuth 时用它们构造 OIDC federation 配置；`providers::anthropic` 的凭据探测也读取
// 同一组名字。变量名是用户可见契约（用户文档与报错会展示它们），改动需同步文档与测试。
/// `ANTHROPIC_FEDERATION_RULE_ID`。
pub const ANTHROPIC_FEDERATION_RULE_ID_ENV: &str = "ANTHROPIC_FEDERATION_RULE_ID";
/// `ANTHROPIC_ORGANIZATION_ID`。
pub const ANTHROPIC_ORGANIZATION_ID_ENV: &str = "ANTHROPIC_ORGANIZATION_ID";
/// `ANTHROPIC_SERVICE_ACCOUNT_ID`。
pub const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV: &str = "ANTHROPIC_SERVICE_ACCOUNT_ID";
/// `ANTHROPIC_IDENTITY_TOKEN_FILE`。
pub const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV: &str = "ANTHROPIC_IDENTITY_TOKEN_FILE";
/// `ANTHROPIC_WORKSPACE_ID`。
pub const ANTHROPIC_WORKSPACE_ID_ENV: &str = "ANTHROPIC_WORKSPACE_ID";

/// `cachedVertexAdcCredentialsExists`：`None` 表示尚未探测，`Some` 是“默认路径或进程环境里
/// 的 `GOOGLE_APPLICATION_CREDENTIALS` 文件存在”的缓存结果。
///
/// 只缓存非显式 scoped `env` 的路径；显式 scoped 路径每次重新检查、不写入这里（见
/// [`has_vertex_adc_credentials`]）。
static VERTEX_ADC_CACHE: Mutex<Option<bool>> = Mutex::new(None);

/// 测试用：清空 ADC 探测缓存（TS 的测试直接改 `process.env`，缓存因此不会命中）。
///
/// 生产代码没有缓存失效钩子（TS 同样没有）；改 `HOME` 或创建/删除 ADC 文件后，同一进程内
/// 的后续查询仍会看到旧结果，直到调用本函数或重启进程。
pub fn reset_vertex_adc_credentials_cache() {
    if let Ok(mut cache) = VERTEX_ADC_CACHE.lock() {
        *cache = None;
    }
}

/// `hasVertexAdcCredentials(env)`：`GOOGLE_APPLICATION_CREDENTIALS` 指向的文件或默认 ADC
/// 文件存在即为真。
///
/// 分两条路径：
/// 1. 调用方 scoped `env` 里显式给出的路径——**每次**检查、不写缓存，因为不同调用方可能
///    传不同的 `env`；
/// 2. 否则先看 `get_provider_env_value`（scoped → 进程环境），再回落到
///    [`default_adc_path`]，结果写入进程级缓存。
///
/// 空字符串路径按“未给出”处理（`filter(|path| !path.is_empty())`），与模块文档的 `||`
/// 语义一致。缓存锁中毒时取回内部值继续，不会因为别的线程 panic 而永久失败。
fn has_vertex_adc_credentials(env: Option<&ProviderEnv>) -> bool {
    if let Some(explicit) = env
        .and_then(|env| env.get("GOOGLE_APPLICATION_CREDENTIALS"))
        .filter(|path| !path.is_empty())
    {
        return std::path::Path::new(explicit).exists();
    }
    let mut cache = match VERTEX_ADC_CACHE.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cached) = *cache {
        return cached;
    }
    let candidate = match get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env) {
        Some(path) => Some(std::path::PathBuf::from(path)),
        None => default_adc_path(),
    };
    let exists = candidate.is_some_and(|path| path.exists());
    *cache = Some(exists);
    exists
}

/// `join(homedir(), ".config", "gcloud", "application_default_credentials.json")`。
///
/// 只读 `HOME`：未设置或为空串时返回 `None`（没有可探测的默认位置）。不展开 `~`，也不
/// 读取其它用户目录变量；因此 `HOME` 不可用的环境不会自动发现 ADC，调用方应通过
/// `GOOGLE_APPLICATION_CREDENTIALS` 显式提供路径。
fn default_adc_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("gcloud")
            .join("application_default_credentials.json"),
    )
}

/// `getApiKeyEnvVars(provider)`：该 provider 认哪些 API key 环境变量（按发现顺序）。
///
/// 返回 `None` 表示 TS 的 `undefined`：provider 未知，或它根本没有 API key 变量
/// （`amazon-bedrock` 只有 ambient 凭据）。`anthropic` 是唯一的多变量 provider；
/// `github-copilot` 是变量名（`COPILOT_GITHUB_TOKEN`）与 provider id 不同名的单变量特例；
/// 其余 provider 恰好一个。`google-vertex` 的 `GOOGLE_CLOUD_API_KEY` 只是候选之一，ADC
/// 回退在 [`get_env_api_key`] 里单独判断。
pub fn get_api_key_env_vars(provider: &str) -> Option<Vec<&'static str>> {
    if provider == "github-copilot" {
        return Some(vec!["COPILOT_GITHUB_TOKEN"]);
    }
    if provider == "anthropic" {
        // `ANTHROPIC_AUTH_TOKEN` 参与发现/状态，但 `getEnvApiKey` 会跳过它。
        return Some(vec![
            ANTHROPIC_AUTH_TOKEN_ENV,
            ANTHROPIC_OAUTH_TOKEN_ENV,
            ANTHROPIC_API_KEY_ENV,
        ]);
    }
    let env_var = match provider {
        "ant-ling" => "ANT_LING_API_KEY",
        "qwen-token-plan" => "QWEN_TOKEN_PLAN_API_KEY",
        "qwen-token-plan-cn" => "QWEN_TOKEN_PLAN_CN_API_KEY",
        "qwen-token-plan-individual" => "QWEN_TOKEN_PLAN_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "azure" => "AZURE_OPENAI_API_KEY",
        "nvidia" => "NVIDIA_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "google" => "GEMINI_API_KEY",
        "google-vertex" => "GOOGLE_CLOUD_API_KEY",
        "groq" => "GROQ_API_KEY",
        "cerebras" => "CEREBRAS_API_KEY",
        "xai" => "XAI_API_KEY",
        "typesafe" => "TYPESAFE_API_KEY",
        "radius" => "RADIUS_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "vercel-ai-gateway" => "AI_GATEWAY_API_KEY",
        "zai" => "ZAI_API_KEY",
        "zai-coding-cn" => "ZAI_CODING_CN_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "minimax-cn" => "MINIMAX_CN_API_KEY",
        "moonshotai" => "MOONSHOT_API_KEY",
        "moonshotai-cn" => "MOONSHOT_API_KEY",
        "huggingface" => "HF_TOKEN",
        "fireworks" => "FIREWORKS_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "baseten" => "BASETEN_API_KEY",
        "opencode" => "OPENCODE_API_KEY",
        "opencode-go" => "OPENCODE_API_KEY",
        "kimi-coding" => "KIMI_API_KEY",
        "meta" => "META_API_KEY",
        "cloudflare-workers-ai" => "CLOUDFLARE_API_KEY",
        "cloudflare-ai-gateway" => "CLOUDFLARE_API_KEY",
        "xiaomi" => "XIAOMI_API_KEY",
        "xiaomi-token-plan-cn" => "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        "xiaomi-token-plan-ams" => "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        "xiaomi-token-plan-sgp" => "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        _ => return None,
    };
    Some(vec![env_var])
}

/// `findEnvKeys(provider, env?)`：找到的 API key 环境变量名（全未设置时 `None`）。
///
/// 只报**真实 API key 变量**：AWS profile/IAM 与 Google ADC 这类环境凭据不算。
/// 返回值保持 `get_api_key_env_vars` 的顺序；要么是 `Some(非空)`，要么是 `None`（从不
/// 返回空 `Vec`）。判断“是否设置”用的是 `get_provider_env_value`，因此空字符串等同于缺失。
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<&'static str>> {
    let env_vars = get_api_key_env_vars(provider)?;
    let found: Vec<&'static str> = env_vars
        .into_iter()
        .filter(|env_var| {
            get_provider_env_value(env_var, env).is_some_and(|value| !value.is_empty())
        })
        .collect();
    if found.is_empty() { None } else { Some(found) }
}

/// `getEnvApiKey(provider, env?)`：环境里的 API key。
///
/// 需要 OAuth token 的 provider 不会返回 key；`anthropic` 跳过 `ANTHROPIC_AUTH_TOKEN`。
///
/// 返回值有三种可能：
/// - 真实 API key 字符串（可直接放进鉴权头）；
/// - `"<authenticated>"` 哨兵，表示 `google-vertex` 的 ADC 或 `amazon-bedrock` 的 ambient
///   凭据已就绪，**不得**当密钥发送，由适配器自行解析（见模块文档）；
/// - `None`，表示没有任何可用凭据。
///
/// 查找顺序：先真实 key（[`find_env_keys`]），再 Vertex ADC（project + location + 文件），
/// 最后 Bedrock ambient 凭据链。
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(env_keys) = find_env_keys(provider, env) {
        let api_key_env = if provider == "anthropic" {
            env_keys
                .iter()
                .find(|key| **key != ANTHROPIC_AUTH_TOKEN_ENV)
                .copied()
        } else {
            env_keys.first().copied()
        };
        if let Some(api_key_env) = api_key_env {
            return get_provider_env_value(api_key_env, env).filter(|value| !value.is_empty());
        }
    }

    // Google Vertex AI：API key 或 Application Default Credentials 二选一。
    if provider == "google-vertex" {
        let has_credentials = has_vertex_adc_credentials(env);
        let has_project = get_provider_env_value("GOOGLE_CLOUD_PROJECT", env)
            .is_some_and(|value| !value.is_empty())
            || get_provider_env_value("GCLOUD_PROJECT", env).is_some_and(|value| !value.is_empty());
        let has_location = get_provider_env_value("GOOGLE_CLOUD_LOCATION", env)
            .is_some_and(|value| !value.is_empty());
        if has_credentials && has_project && has_location {
            return Some("<authenticated>".to_string());
        }
    }

    if provider == "amazon-bedrock" {
        // AWS 官方凭据链的最小集合：profile、成对的静态 key、Bedrock bearer token、
        // 容器凭据端点、或 web identity。任一来源存在即认为“环境已提供凭据”，
        // 具体能否签发请求由适配器/SigV4 在调用时决定。
        let has =
            |name: &str| get_provider_env_value(name, env).is_some_and(|value| !value.is_empty());
        if has("AWS_PROFILE")
            || (has("AWS_ACCESS_KEY_ID") && has("AWS_SECRET_ACCESS_KEY"))
            || has("AWS_BEARER_TOKEN_BEDROCK")
            || has("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            || has("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            || has("AWS_WEB_IDENTITY_TOKEN_FILE")
        {
            return Some("<authenticated>".to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_known_provider_to_its_api_key_variable() {
        assert_eq!(
            get_api_key_env_vars("anthropic"),
            Some(vec![
                ANTHROPIC_AUTH_TOKEN_ENV,
                ANTHROPIC_OAUTH_TOKEN_ENV,
                ANTHROPIC_API_KEY_ENV,
            ])
        );
        assert_eq!(
            get_api_key_env_vars("github-copilot"),
            Some(vec!["COPILOT_GITHUB_TOKEN"])
        );
        assert_eq!(
            get_api_key_env_vars("openrouter"),
            Some(vec!["OPENROUTER_API_KEY"])
        );
        assert_eq!(
            get_api_key_env_vars("zai-coding-cn"),
            Some(vec!["ZAI_CODING_CN_API_KEY"])
        );
    }

    #[test]
    fn unknown_providers_have_no_environment_lookup_at_all() {
        // 未知 provider 在查环境之前就返回，因此与进程 env 内容无关。
        assert_eq!(get_api_key_env_vars("not-a-provider"), None);
        assert_eq!(
            find_env_keys("not-a-provider", Some(&ProviderEnv::new())),
            None
        );
        assert_eq!(
            get_env_api_key("not-a-provider", Some(&ProviderEnv::new())),
            None
        );
    }

    #[test]
    fn the_default_adc_path_lives_under_the_home_directory() {
        let path = default_adc_path();
        match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => {
                let path = path.expect("HOME is set");
                assert!(path.starts_with(std::path::PathBuf::from(home)));
                assert!(path.ends_with("application_default_credentials.json"));
            }
            _ => assert!(path.is_none()),
        }
    }
}
