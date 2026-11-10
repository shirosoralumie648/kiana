//! 真实凭据探测：`test/azure-utils.ts`、`test/bedrock-utils.ts`、`test/cloudflare-utils.ts`
//! 的逐字移植。
//!
//! 这些函数决定“需要真 key 的测试”是运行还是 `#[ignore]`。判断口径固定为：读同名环境变量、
//! 空串视为未设置（Rust 里用 `std::env::var` 的 `Ok` + 非空判断，`""` 为假）。

use std::collections::HashMap;

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `azure-utils.ts#hasAzureOpenAICredentials`：
/// `!!AZURE_OPENAI_API_KEY && !!(AZURE_OPENAI_BASE_URL || AZURE_OPENAI_RESOURCE_NAME)`。
pub fn has_azure_openai_credentials() -> bool {
    let has_key = env_set("AZURE_OPENAI_API_KEY");
    let has_base_url = env_set("AZURE_OPENAI_BASE_URL") || env_set("AZURE_OPENAI_RESOURCE_NAME");
    has_key && has_base_url
}

/// `azure-utils.ts#parseDeploymentNameMap`：`"a=b,c=d"` → `{a: b, c: d}`。
///
/// TS 用 `entry.trim()`、`split("=", 2)`、两侧再 `trim()`，空项与缺任一侧的项跳过。
pub fn parse_deployment_name_map(value: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(value) = value else {
        return map;
    };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((model_id, deployment_name)) = trimmed.split_once('=') else {
            continue;
        };
        let (model_id, deployment_name) = (model_id.trim(), deployment_name.trim());
        if model_id.is_empty() || deployment_name.is_empty() {
            continue;
        }
        map.insert(model_id.to_owned(), deployment_name.to_owned());
    }
    map
}

/// `azure-utils.ts#resolveAzureDeploymentName`。
pub fn resolve_azure_deployment_name(model_id: &str) -> Option<String> {
    let map_value = env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP");
    parse_deployment_name_map(map_value.as_deref()).remove(model_id)
}

/// `bedrock-utils.ts#hasBedrockCredentials`：
/// `!!(AWS_PROFILE || (AWS_ACCESS_KEY_ID && AWS_SECRET_ACCESS_KEY) || AWS_BEARER_TOKEN_BEDROCK)`。
pub fn has_bedrock_credentials() -> bool {
    env_set("AWS_PROFILE")
        || (env_set("AWS_ACCESS_KEY_ID") && env_set("AWS_SECRET_ACCESS_KEY"))
        || env_set("AWS_BEARER_TOKEN_BEDROCK")
}

/// `cloudflare-utils.ts#hasCloudflareWorkersAICredentials`。
pub fn has_cloudflare_workers_ai_credentials() -> bool {
    env_set("CLOUDFLARE_API_KEY") && env_set("CLOUDFLARE_ACCOUNT_ID")
}

/// `cloudflare-utils.ts#hasCloudflareAiGatewayCredentials`。
pub fn has_cloudflare_ai_gateway_credentials() -> bool {
    env_set("CLOUDFLARE_API_KEY")
        && env_set("CLOUDFLARE_ACCOUNT_ID")
        && env_set("CLOUDFLARE_GATEWAY_ID")
}
