//! `identity.test.ts` 的 Rust 移植。
//!
//! 第二个 describe（请求路径）用自定义 `ProviderStreams` 复刻 API 适配器的头组装顺序：
//! identity 头最低，`model.headers` 次之（由 `Models.getAuth(model)` 合入 `options.headers`），
//! 请求 `options.headers` 最高，`null` 抑制同名默认头。

mod models_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, ModelsSimpleStreamOptions, ProviderApiImpl,
    ProviderStreams, create_models, create_provider,
};
use kiana_ai::types::{
    ClientAttribution, ClientIdentity, Context, Model, ProviderHeaders, SimpleStreamOptions,
    StreamOptions, UserAgentMode,
};
use kiana_ai::utils::identity::{
    ClientIdentityTarget, KIANA_ATTRIBUTION, format_user_agent, get_attribution_headers,
    get_identity_headers, merge_client_identity, with_client_identity,
};
use kiana_ai::utils::version::KIANA_AI_VERSION;

use models_support::{chat, response_stream};

const ACME: fn() -> ClientAttribution = || ClientAttribution {
    name: "acme".into(),
    url: "https://acme.example".into(),
    categories: None,
};

fn target(provider: &str, base_url: &str) -> ClientIdentityTarget {
    ClientIdentityTarget::new(provider, base_url)
}

fn header<'a>(headers: &'a ProviderHeaders, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_deref())
}

fn has_header(headers: &ProviderHeaders, name: &str) -> bool {
    header(headers, name).is_some()
}

fn completions_model(provider: &str, base_url: &str, headers: Option<ProviderHeaders>) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "openai-completions".into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        cost: Default::default(),
        context_window: Some(128_000),
        max_tokens: Some(4_096),
        headers: headers.map(|headers| {
            headers
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect()
        }),
        ..Model::default()
    }
}

fn model_headers(pairs: &[(&str, &str)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), Some((*value).to_owned())))
        .collect()
}

/// 复刻 API 适配器的头组装：identity < `options.headers`（`null` 抑制）。
fn assemble_headers(target: &ClientIdentityTarget, options: &StreamOptions) -> ProviderHeaders {
    let mut headers = get_identity_headers(target, options.identity.as_ref());
    if let Some(overrides) = &options.headers {
        for (name, value) in overrides {
            let lower = name.to_lowercase();
            let remove: Vec<String> = headers
                .keys()
                .filter(|existing| existing.to_lowercase() == lower)
                .cloned()
                .collect();
            for existing in remove {
                headers.shift_remove(&existing);
            }
            if value.is_some() {
                headers.insert(name.clone(), value.clone());
            }
        }
    }
    headers
}

struct CapturingStreams {
    captured: Arc<Mutex<Option<ProviderHeaders>>>,
}

#[async_trait]
impl ProviderStreams for CapturingStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        let target = ClientIdentityTarget::new(&model.provider, &model.base_url);
        *self.captured.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(assemble_headers(&target, &options));
        response_stream(model, "ok")
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        options: SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        let target = ClientIdentityTarget::new(&model.provider, &model.base_url);
        *self.captured.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(assemble_headers(&target, &options.base));
        response_stream(model, "ok")
    }
}

struct TestKeyAuth;

#[async_trait]
impl ApiKeyAuth for TestKeyAuth {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult::new(ModelAuth {
            api_key: Some("key".into()),
            ..Default::default()
        })))
    }
}

fn test_auth() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(TestKeyAuth)),
        oauth: None,
    }
}

fn empty_context() -> Context {
    Context::default()
}

#[test]
fn keeps_the_version_constant_in_sync_with_package_json() {
    // TS 读 `../package.json`；Rust 侧版本是字面量（ARCHITECTURE §8 第 24 条）。
    assert_eq!(KIANA_AI_VERSION, "1.4.0");
}

#[test]
fn formats_user_agents() {
    // TS 用 `process.versions.bun ?? process.version`；Rust 侧探测宿主运行时。
    let runtime = kiana_ai::utils::identity::runtime_description();
    let expected = match runtime.as_deref() {
        Some(runtime) => format!(
            "kiana/{KIANA_AI_VERSION} ({}; {runtime}; {})",
            kiana_ai::utils::identity::node_platform(),
            kiana_ai::utils::identity::node_arch()
        ),
        None => format!("kiana/{KIANA_AI_VERSION} (browser)"),
    };
    assert_eq!(format_user_agent(None), expected);
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            version: Some("9.9.9".into()),
            ..Default::default()
        })),
        expected.replace(KIANA_AI_VERSION, "9.9.9")
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::Short("short".into())),
            version: Some("9.9.9".into()),
            ..Default::default()
        })),
        "kiana"
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::Custom {
                custom: "acme/1.0".into()
            }),
            ..Default::default()
        })),
        "acme/1.0"
    );
}

/// 回归（review finding）：detailed `User-Agent` 必须使用**探测到的**宿主运行时版本
/// （`bun --version` 优先，其次 `node --version`），而不是硬编码常量（旧实现固定为
/// `node/v22.0.0`）。这里独立探测一次并与库的返回值比较。
#[test]
fn user_agent_reports_the_detected_host_runtime_version() {
    fn probe(executable: &str) -> Option<String> {
        let output = std::process::Command::new(executable)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if version.is_empty() {
            None
        } else {
            Some(version)
        }
    }

    let expected_runtime = match probe("bun") {
        Some(version) => Some(format!("bun/{version}")),
        None => probe("node").map(|version| format!("node/{version}")),
    };
    assert_eq!(
        kiana_ai::utils::identity::runtime_description(),
        expected_runtime,
        "runtime description must be detected, not a constant"
    );
    let expected_user_agent = match &expected_runtime {
        Some(runtime) => format!(
            "kiana/{KIANA_AI_VERSION} ({}; {runtime}; {})",
            kiana_ai::utils::identity::node_platform(),
            kiana_ai::utils::identity::node_arch()
        ),
        None => format!("kiana/{KIANA_AI_VERSION} (browser)"),
    };
    assert_eq!(format_user_agent(None), expected_user_agent);
}

#[test]
fn merges_identities_field_by_field() {
    assert!(merge_client_identity([None, None]).is_none());
    let merged = merge_client_identity([
        Some(ClientIdentity {
            attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
            user_agent: Some(UserAgentMode::Short("short".into())),
            version: None,
        }),
        Some(ClientIdentity {
            attribution: None,
            user_agent: None,
            version: Some("1.0.0".into()),
        }),
    ])
    .unwrap();
    assert_eq!(merged.attribution, Some(Some(KIANA_ATTRIBUTION.clone())));
    assert_eq!(
        merged.user_agent,
        Some(UserAgentMode::Short("short".into()))
    );
    assert_eq!(merged.version.as_deref(), Some("1.0.0"));
    let cleared = merge_client_identity([
        Some(ClientIdentity {
            attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
            ..Default::default()
        }),
        Some(ClientIdentity {
            attribution: Some(None),
            ..Default::default()
        }),
    ])
    .unwrap();
    assert_eq!(cleared.attribution, Some(None));
}

#[test]
fn sends_no_attribution_unless_enabled() {
    let open_router = target("openrouter", "https://openrouter.ai/api/v1");
    assert!(get_attribution_headers(&open_router, None).is_empty());
    assert!(
        get_attribution_headers(
            &open_router,
            Some(&ClientIdentity {
                attribution: Some(None),
                ..Default::default()
            })
        )
        .is_empty()
    );
    let identity_headers = get_identity_headers(&open_router, None);
    assert_eq!(
        header(&identity_headers, "User-Agent"),
        Some(format_user_agent(None).as_str())
    );
    assert_eq!(identity_headers.len(), 1);
}

#[test]
fn attributes_open_router_requests() {
    let open_router = target("openrouter", "https://openrouter.ai/api/v1");
    let kiana_identity = ClientIdentity {
        attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
        ..Default::default()
    };
    let headers = get_attribution_headers(&open_router, Some(&kiana_identity));
    assert_eq!(header(&headers, "HTTP-Referer"), Some("https://kiana.dev"));
    assert_eq!(header(&headers, "X-OpenRouter-Title"), Some("kiana"));
    assert_eq!(
        header(&headers, "X-OpenRouter-Categories"),
        Some("cli-agent")
    );
    let acme_identity = ClientIdentity {
        attribution: Some(Some(ClientAttribution {
            categories: Some(vec!["cli-agent".into(), "cloud-agent".into()]),
            ..ACME()
        })),
        ..Default::default()
    };
    let headers = get_attribution_headers(&open_router, Some(&acme_identity));
    assert_eq!(
        header(&headers, "X-OpenRouter-Categories"),
        Some("cli-agent,cloud-agent")
    );
    let empty_categories = ClientIdentity {
        attribution: Some(Some(ClientAttribution {
            categories: Some(vec![]),
            ..ACME()
        })),
        ..Default::default()
    };
    let headers = get_attribution_headers(&open_router, Some(&empty_categories));
    assert!(!has_header(&headers, "X-OpenRouter-Categories"));
    // Custom providers routed through OpenRouter, including legacy base URLs that are not valid URLs.
    assert_eq!(
        header(
            &get_attribution_headers(
                &target("custom", "not-a-url-openrouter.ai"),
                Some(&kiana_identity)
            ),
            "X-OpenRouter-Title"
        ),
        Some("kiana")
    );
}

#[test]
fn attributes_nvidia_nim_requests_with_a_capitalized_name_and_opencode_requests_with_the_name() {
    let kiana_identity = ClientIdentity {
        attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
        ..Default::default()
    };
    assert_eq!(
        header(
            &get_attribution_headers(
                &target("nvidia", "https://example.test/v1"),
                Some(&kiana_identity)
            ),
            "X-BILLING-INVOKE-ORIGIN"
        ),
        Some("Kiana")
    );
    let acme_identity = ClientIdentity {
        attribution: Some(Some(ACME())),
        ..Default::default()
    };
    assert_eq!(
        header(
            &get_attribution_headers(
                &target("custom-nim", "https://integrate.api.nvidia.com/v1"),
                Some(&acme_identity)
            ),
            "X-BILLING-INVOKE-ORIGIN"
        ),
        Some("Acme")
    );
    assert_eq!(
        header(
            &get_attribution_headers(
                &target("opencode", "https://opencode.ai/zen/v1"),
                Some(&kiana_identity)
            ),
            "x-opencode-client"
        ),
        Some("kiana")
    );
    assert_eq!(
        header(
            &get_attribution_headers(
                &target("custom", "https://opencode.ai/zen/v1"),
                Some(&acme_identity)
            ),
            "x-opencode-client"
        ),
        Some("acme")
    );
}

#[test]
fn does_not_attribute_nvidia_models_routed_through_gateways_to_nvidia() {
    let kiana_identity = ClientIdentity {
        attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
        ..Default::default()
    };
    assert!(!has_header(
        &get_attribution_headers(
            &target("openrouter", "https://openrouter.ai/api/v1"),
            Some(&kiana_identity)
        ),
        "X-BILLING-INVOKE-ORIGIN"
    ));
    assert!(
        get_attribution_headers(
            &target("vercel-ai-gateway", "https://ai-gateway.vercel.sh/v1"),
            Some(&kiana_identity)
        )
        .is_empty()
    );
}

#[tokio::test]
async fn applies_the_identity_below_model_and_request_headers() {
    let model = completions_model(
        "openrouter",
        "https://openrouter.ai/api/v1",
        Some(model_headers(&[("X-OpenRouter-Title", "model-title")])),
    );
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "openrouter".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::new(Mutex::new(None)),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    let captured = Arc::new(Mutex::new(None));
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "openrouter".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::clone(&captured),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    let mut headers = ProviderHeaders::new();
    headers.insert("HTTP-Referer".into(), None);
    models
        .complete_simple(
            &model,
            &empty_context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        identity: Some(ClientIdentity {
                            attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
                            user_agent: Some(UserAgentMode::Short("short".into())),
                            version: None,
                        }),
                        headers: Some(headers),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    let headers = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    assert_eq!(header(&headers, "user-agent"), Some("kiana"));
    assert_eq!(header(&headers, "x-openrouter-title"), Some("model-title"));
    assert!(!has_header(&headers, "http-referer"));
}

#[tokio::test]
async fn keeps_impersonating_model_headers_over_the_identity_user_agent() {
    let model = completions_model(
        "github-copilot",
        "https://api.githubcopilot.com",
        Some(model_headers(&[("User-Agent", "GitHubCopilotChat/0.35.0")])),
    );
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "github-copilot".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::clone(&captured),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    models
        .complete_simple(
            &model,
            &empty_context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        identity: Some(ClientIdentity {
                            user_agent: Some(UserAgentMode::Custom {
                                custom: "acme".into(),
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    let headers = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    assert_eq!(
        header(&headers, "user-agent"),
        Some("GitHubCopilotChat/0.35.0")
    );
}

fn attribution(name: &str) -> ClientAttribution {
    ClientAttribution {
        name: name.into(),
        url: "https://acme.example".into(),
        categories: None,
    }
}

#[tokio::test]
async fn merges_request_identity_over_the_models_and_wrapper_identities() {
    let model = completions_model("openrouter", "https://openrouter.ai/api/v1", None);
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(CreateModelsOptions {
        identity: Some(ClientIdentity {
            attribution: Some(Some(attribution("models"))),
            user_agent: Some(UserAgentMode::Short("short".into())),
            version: None,
        }),
        ..Default::default()
    });
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "openrouter".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::clone(&captured),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    // 直接用 Models 发送（identity 来自 Models 默认）。
    let stream_models = create_models(CreateModelsOptions::default());
    let _ = stream_models;
    models
        .complete_simple(
            &model,
            &empty_context(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    let headers = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    assert_eq!(header(&headers, "user-agent"), Some("kiana"));
    assert_eq!(header(&headers, "x-openrouter-title"), Some("models"));

    let wrapped = with_client_identity(
        Arc::clone(&models),
        ClientIdentity {
            attribution: Some(Some(attribution("wrapper"))),
            ..Default::default()
        },
    );
    wrapped
        .complete_simple(
            &model,
            &empty_context(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    let headers = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    assert_eq!(header(&headers, "user-agent"), Some("kiana"));
    assert_eq!(header(&headers, "x-openrouter-title"), Some("wrapper"));
    assert_eq!(
        wrapped.get_model("openrouter", "test-model").unwrap().id,
        "test-model"
    );
    // A MutableModels stays mutable.
    wrapped.delete_provider("openrouter");
    assert!(models.get_provider("openrouter").is_none());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "openrouter".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::clone(&captured),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );

    let from_request = wrapped
        .complete_simple(
            &model,
            &empty_context(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        identity: Some(ClientIdentity {
                            attribution: Some(None),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(from_request.stop_reason, kiana_ai::types::StopReason::Stop);
    let headers = captured
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap();
    assert!(!has_header(&headers, "x-openrouter-title"));
    assert_eq!(header(&headers, "user-agent"), Some("kiana"));
}

#[tokio::test]
async fn sends_the_opencode_session_header_to_custom_providers_pointing_at_opencode() {
    let model = completions_model("custom-opencode", "https://opencode.ai/zen/v1", None);
    let captured = Arc::new(Mutex::new(None));
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "custom-opencode".into(),
            auth: test_auth(),
            models: vec![chat(model.clone())],
            api: Some(ProviderApiImpl::Single(Arc::new(CapturingStreams {
                captured: Arc::clone(&captured),
            }))),
            ..Default::default()
        })
        .unwrap(),
    );
    let send = |headers: Option<ProviderHeaders>| {
        let models = Arc::clone(&models);
        let captured = Arc::clone(&captured);
        let model = model.clone();
        async move {
            models
                .complete_simple(
                    &model,
                    &empty_context(),
                    ModelsSimpleStreamOptions {
                        base: SimpleStreamOptions {
                            base: StreamOptions {
                                session_id: Some("session-1".into()),
                                headers,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await;
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap()
        }
    };
    let headers = send(None).await;
    assert_eq!(header(&headers, "x-opencode-session"), Some("session-1"));
    let mut configured = ProviderHeaders::new();
    configured.insert("X-OpenCode-Session".into(), Some("configured".into()));
    let headers = send(Some(configured)).await;
    assert_eq!(header(&headers, "x-opencode-session"), Some("configured"));
}
