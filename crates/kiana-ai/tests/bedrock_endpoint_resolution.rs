//! `test/bedrock-endpoint-resolution.test.ts` 的移植。

use std::sync::{Mutex, MutexGuard};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, resolve_client_config};
use kiana_ai::types::{Model, ModelInputModality, StreamOptions};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(names: &[&'static str]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut saved = Vec::new();
        for name in names {
            saved.push((*name, std::env::var(name).ok()));
            // SAFETY: 所有读环境变量的用例都在本锁内。
            unsafe { std::env::remove_var(name) };
        }
        EnvGuard { _lock: lock, saved }
    }

    fn set(&self, name: &str, value: &str) {
        unsafe { std::env::set_var(name, value) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

const ENV_NAMES: &[&str] = &[
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_PROFILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_BEDROCK_SKIP_AUTH",
];

fn model(id: &str, base_url: &str) -> Model {
    Model {
        id: id.into(),
        name: "Claude".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: base_url.into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn us_model() -> Model {
    model(
        "us.anthropic.claude-opus-4-8",
        "https://bedrock-runtime.us-east-1.amazonaws.com",
    )
}

fn eu_model() -> Model {
    model(
        "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "https://bedrock-runtime.eu-central-1.amazonaws.com",
    )
}

fn base_options() -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(kiana_ai::types::CacheRetention::None),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn assigns_eu_central_1_runtime_urls_to_builtin_eu_inference_profiles() {
    let models = kiana_ai::providers::catalog::get_models("amazon-bedrock");
    let model = models
        .iter()
        .find(|model| {
            model["id"] == serde_json::json!("eu.anthropic.claude-sonnet-4-5-20250929-v1:0")
        })
        .expect("eu model");
    assert_eq!(
        model["baseUrl"],
        serde_json::json!("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
}

#[test]
fn does_not_pin_standard_aws_endpoints_when_aws_region_is_configured() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_REGION", "us-east-2");
    let config = resolve_client_config(&us_model(), &base_options());
    assert_eq!(config.region.as_deref(), Some("us-east-2"));
    assert!(config.endpoint.is_none());
}

#[test]
fn derives_region_from_a_builtin_eu_endpoint_without_region_or_profile() {
    let _env = EnvGuard::new(ENV_NAMES);
    let config = resolve_client_config(&eu_model(), &base_options());
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));
}

#[test]
fn handles_missing_regions_for_explicit_scoped_and_ambient_profiles() {
    let env = EnvGuard::new(ENV_NAMES);

    let mut options = base_options();
    options.profile = Some("bedrock-profile".into());
    let config = resolve_client_config(&eu_model(), &options);
    assert_eq!(config.profile.as_deref(), Some("bedrock-profile"));
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));

    let mut options = base_options();
    options.base.env = Some(
        [(
            "AWS_PROFILE".to_string(),
            "scoped-bedrock-profile".to_string(),
        )]
        .into_iter()
        .collect(),
    );
    let config = resolve_client_config(&eu_model(), &options);
    assert_eq!(config.profile.as_deref(), Some("scoped-bedrock-profile"));
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-runtime.eu-central-1.amazonaws.com")
    );
    assert_eq!(config.region.as_deref(), Some("eu-central-1"));

    env.set("AWS_PROFILE", "ambient-bedrock-profile");
    let config = resolve_client_config(&eu_model(), &base_options());
    assert_eq!(config.profile.as_deref(), Some("ambient-bedrock-profile"));
    assert!(config.endpoint.is_none());
    assert!(config.region.is_none());
}

#[test]
fn still_passes_custom_bedrock_endpoints_through() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_REGION", "us-west-2");
    let config = resolve_client_config(
        &model(
            "us.anthropic.claude-opus-4-8",
            "https://bedrock-vpc.example.com",
        ),
        &base_options(),
    );
    assert_eq!(
        config.endpoint.as_deref(),
        Some("https://bedrock-vpc.example.com")
    );
    assert_eq!(config.region.as_deref(), Some("us-west-2"));
}

#[test]
fn extracts_region_from_inference_profile_arns_regardless_of_aws_region() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_REGION", "us-east-1");
    let config = resolve_client_config(
        &model(
            "arn:aws:bedrock:us-west-2:123456789012:application-inference-profile/abc123",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        &base_options(),
    );
    assert_eq!(config.region.as_deref(), Some("us-west-2"));
}

#[test]
fn extracts_region_from_govcloud_inference_profile_arns() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_REGION", "us-east-1");
    let config = resolve_client_config(
        &model(
            "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:application-inference-profile/abc123",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        &base_options(),
    );
    assert_eq!(config.region.as_deref(), Some("us-gov-west-1"));
}

#[test]
fn preserves_ambient_aws_auth_for_custom_model_ids() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_PROFILE", "bedrock-profile");
    let config = resolve_client_config(
        &model(
            "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/example",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        &base_options(),
    );
    assert_eq!(config.profile.as_deref(), Some("bedrock-profile"));
    assert!(config.token.is_none());
    assert!(config.auth_scheme_preference.is_none());
}

#[test]
fn uses_the_generic_api_key_option_as_a_bedrock_bearer_token() {
    let _env = EnvGuard::new(ENV_NAMES);
    let mut options = base_options();
    options.base.api_key = Some("bedrock-api-key".into());
    let config = resolve_client_config(&us_model(), &options);
    assert_eq!(config.token.as_deref(), Some("bedrock-api-key"));
    assert_eq!(
        config.auth_scheme_preference,
        Some(vec!["httpBearerAuth".to_string()])
    );
}
