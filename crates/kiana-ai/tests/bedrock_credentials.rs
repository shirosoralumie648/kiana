//! `test/bedrock-credentials.test.ts` 的移植。
//!
//! 进程环境是全局状态：本文件所有用例用同一把锁串行化，并在结束时清空相关变量。

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
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_BEDROCK_SKIP_AUTH",
];

fn model() -> Model {
    Model {
        id: "us.anthropic.claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
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
fn prefers_explicit_and_scoped_profiles_over_ambient_access_keys() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_ACCESS_KEY_ID", "AKIAEXAMPLE");
    env.set("AWS_SECRET_ACCESS_KEY", "secretexample");

    let mut options = base_options();
    options.profile = Some("explicit-profile".into());
    let config = resolve_client_config(&model(), &options);
    assert_eq!(config.profile.as_deref(), Some("explicit-profile"));
    assert!(config.credentials.is_none());

    let mut options = base_options();
    options.base.env = Some(
        [("AWS_PROFILE".to_string(), "scoped-profile".to_string())]
            .into_iter()
            .collect(),
    );
    let config = resolve_client_config(&model(), &options);
    assert_eq!(config.profile.as_deref(), Some("scoped-profile"));
    assert!(config.credentials.is_none());
}

#[test]
fn uses_ambient_aws_access_keys_when_no_profile_is_configured() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_ACCESS_KEY_ID", "AKIAEXAMPLE");
    env.set("AWS_SECRET_ACCESS_KEY", "secretexample");

    let config = resolve_client_config(&model(), &base_options());
    assert!(config.profile.is_none());
    let credentials = config.credentials.expect("credentials");
    assert_eq!(credentials.access_key_id, "AKIAEXAMPLE");
    assert_eq!(credentials.secret_access_key, "secretexample");
    assert!(credentials.session_token.is_none());
}

#[test]
fn uses_ambient_aws_access_keys_when_only_an_ambient_profile_is_set() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_ACCESS_KEY_ID", "AKIAEXAMPLE");
    env.set("AWS_SECRET_ACCESS_KEY", "secretexample");
    env.set("AWS_PROFILE", "ambient-profile");

    let config = resolve_client_config(&model(), &base_options());
    assert_eq!(config.profile.as_deref(), Some("ambient-profile"));
    let credentials = config.credentials.expect("credentials");
    assert_eq!(credentials.access_key_id, "AKIAEXAMPLE");
    assert_eq!(credentials.secret_access_key, "secretexample");
}

#[test]
fn skip_auth_installs_dummy_credentials() {
    let env = EnvGuard::new(ENV_NAMES);
    env.set("AWS_BEDROCK_SKIP_AUTH", "1");
    let config = resolve_client_config(&model(), &base_options());
    let credentials = config.credentials.expect("credentials");
    assert_eq!(credentials.access_key_id, "dummy-access-key");
    assert_eq!(credentials.secret_access_key, "dummy-secret-key");
    assert!(config.token.is_none());
}

struct StubProvider;

impl kiana_ai::models::ProviderStreams for StubProvider {
    fn stream(
        &self,
        _model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        kiana_ai::event_stream::AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: kiana_ai::types::SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        kiana_ai::event_stream::AssistantMessageEventStream::new()
    }
}

#[test]
fn set_bedrock_provider_module_overrides_all_subsequent_factories() {
    use std::sync::Arc;

    use kiana_ai::api::bedrock_converse_stream::{
        bedrock_converse_stream_api, set_bedrock_provider_module,
    };
    use kiana_ai::models::ProviderStreams;

    let first: Arc<dyn ProviderStreams> = Arc::new(StubProvider);
    set_bedrock_provider_module(first.clone());
    assert!(Arc::ptr_eq(&first, &bedrock_converse_stream_api()));

    // 后设置覆盖先设置。
    let second: Arc<dyn ProviderStreams> = Arc::new(StubProvider);
    set_bedrock_provider_module(second.clone());
    let current = bedrock_converse_stream_api();
    assert!(Arc::ptr_eq(&second, &current));
    assert!(!Arc::ptr_eq(&first, &current));
}
