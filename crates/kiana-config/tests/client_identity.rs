//! `sdk-client-identity.test.ts` 的移植。
//!
//! TS 测试通过完整的 `createAgentSession` 观察 `SimpleStreamOptions.identity`；本单元只拥有
//! `core/client-identity.ts`，因此直接断言 [`get_client_identity`] 的结果，并用 `kiana-ai` 的
//! `getAttributionHeaders` / `formatUserAgent` 复核这些身份会产生的请求头/UA（与 TS 测试的
//! 断言对象一致）。
//!
//! 两条 OpenCode 会话头用例（“adds OpenCode session headers …” / “lets configured OpenCode
//! headers override …”）的对象是 `model-runtime` 的请求选项装配，已按计划改派给
//! `kiana-config:model-runtime` 单元。

use kiana_ai::types::{ClientAttribution, ClientIdentity, UserAgentMode, UserAgentModeDetailed};
use kiana_ai::utils::identity::{
    ClientIdentityTarget, KIANA_ATTRIBUTION, format_user_agent, get_attribution_headers,
};
use kiana_config::client_identity::{get_client_identity, get_client_identity_with_env};
use kiana_config::settings::SettingsManager;
use kiana_core::config::VERSION;

fn expected_identity(attribution: Option<ClientAttribution>) -> ClientIdentity {
    ClientIdentity {
        user_agent: Some(UserAgentMode::Detailed(UserAgentModeDetailed::Detailed)),
        attribution: Some(attribution),
        version: Some(VERSION.to_string()),
    }
}

fn settings() -> SettingsManager {
    SettingsManager::in_memory(Default::default())
}

fn openrouter_target() -> ClientIdentityTarget {
    ClientIdentityTarget::new("openrouter", "https://openrouter.ai/api/v1")
}

fn nvidia_target() -> ClientIdentityTarget {
    ClientIdentityTarget::new("nvidia", "https://integrate.api.nvidia.com/v1")
}

// ---------------------------------------------------------------------------
// "identifies kiana with a detailed User-Agent and attribution as a CLI agent"
// ---------------------------------------------------------------------------

#[test]
fn identifies_kiana_with_a_detailed_user_agent_and_attribution_as_a_cli_agent() {
    let identity = get_client_identity_with_env(&settings(), None);

    assert_eq!(identity, expected_identity(Some(KIANA_ATTRIBUTION.clone())));
    // `attribution: undefined` 与 `attribution: null` 不同：这里必须是 `Some(Some(..))`。
    assert!(matches!(identity.attribution, Some(Some(_))));

    let headers = get_attribution_headers(&openrouter_target(), Some(&identity));
    assert_eq!(
        headers.get("HTTP-Referer").cloned().flatten().as_deref(),
        Some("https://kiana.dev")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Title")
            .cloned()
            .flatten()
            .as_deref(),
        Some("kiana")
    );
    assert_eq!(
        headers
            .get("X-OpenRouter-Categories")
            .cloned()
            .flatten()
            .as_deref(),
        Some("cli-agent")
    );

    let user_agent = format_user_agent(Some(&identity));
    // detailed UA = `kiana/<version> (<platform>; <arch>; <runtime>)`，运行时探测取决于宿主
    // 是否装有 bun/node（都没有时是 `(browser)`），所以这里只断言与宿主无关的前缀。
    assert!(
        user_agent.starts_with(&format!("kiana/{VERSION} ")),
        "detailed UA should start with kiana/<VERSION>: {user_agent}"
    );
    assert!(
        user_agent.ends_with(')'),
        "detailed UA should end with the runtime parens: {user_agent}"
    );
}

// ---------------------------------------------------------------------------
// "turns attribution off when telemetry is disabled"
// ---------------------------------------------------------------------------

#[test]
fn turns_attribution_off_when_telemetry_is_disabled() {
    let settings = settings();
    settings.set_enable_install_telemetry(false);
    let identity = get_client_identity_with_env(&settings, None);

    assert_eq!(identity, expected_identity(None));
    // 明确关闭（`null`）而不是缺省（`undefined`）：合并身份时它必须覆盖默认值。
    assert_eq!(identity.attribution, Some(None));
    assert!(
        get_attribution_headers(&nvidia_target(), Some(&identity)).is_empty(),
        "nvidia attribution headers must be empty when attribution is null"
    );
}

// ---------------------------------------------------------------------------
// "turns attribution off with KIANA_TELEMETRY=0"
// ---------------------------------------------------------------------------

#[test]
fn turns_attribution_off_with_kiana_telemetry_0() {
    let settings = settings();
    // 设置里保持默认（开启）；环境变量优先。
    assert!(settings.get_enable_install_telemetry());
    let identity = get_client_identity_with_env(&settings, Some("0"));
    assert_eq!(identity.attribution, Some(None));
    assert_eq!(identity, expected_identity(None));
}

#[test]
fn environment_value_wins_over_settings_in_both_directions() {
    let disabled_by_settings = settings();
    disabled_by_settings.set_enable_install_telemetry(false);
    assert_eq!(
        get_client_identity_with_env(&disabled_by_settings, Some("1")).attribution,
        Some(Some(KIANA_ATTRIBUTION.clone()))
    );

    let enabled_by_settings = settings();
    // `isTruthyEnvFlag`：`"1"` / `"true"` / `"yes"`（大小写不敏感）为真，其余为假。
    for truthy in ["1", "true", "TRUE", "yes", "Yes"] {
        assert_eq!(
            get_client_identity_with_env(&enabled_by_settings, Some(truthy)).attribution,
            Some(Some(KIANA_ATTRIBUTION.clone())),
            "KIANA_TELEMETRY={truthy} should keep attribution"
        );
    }
    for falsy in ["0", "false", "no", "", "on"] {
        assert_eq!(
            get_client_identity_with_env(&enabled_by_settings, Some(falsy)).attribution,
            Some(None),
            "KIANA_TELEMETRY={falsy:?} should disable attribution"
        );
    }
}

// ---------------------------------------------------------------------------
// 进程环境入口（`getClientIdentity`）
// ---------------------------------------------------------------------------

#[test]
fn get_client_identity_matches_the_explicit_environment_entry_point() {
    let settings = settings();
    let telemetry_env = std::env::var("KIANA_TELEMETRY").ok();
    assert_eq!(
        get_client_identity(&settings),
        get_client_identity_with_env(&settings, telemetry_env.as_deref())
    );
}
