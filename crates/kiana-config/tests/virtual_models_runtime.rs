//! `virtual-models.test.ts` 的 `ModelRuntime virtual models` 部分（7 个用例）。
//!
//! `createAgentSession` 相关的用例属于会话单元，不在此文件。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};

use kiana_ai::models::ModelsSimpleStreamOptions;
use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxProviderHandle, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider_with,
};
use kiana_ai::types::{
    AssistantMessage, Context, Message, Model, ModelInputModality, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, ThinkingLevel, UserContent, UserMessage, UserRole,
};
use kiana_config::models::runtime::{
    CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption, ResolveModelOptions,
};
use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, RouteFuture, RouteModelRef,
    VirtualModelDefinition, VirtualModelRouteError,
};
use support::*;

type RouteFn = Arc<dyn Fn(ModelRouteRequest) -> RouteFuture + Send + Sync>;

async fn create_runtime() -> Arc<ModelRuntime> {
    Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(
                kiana_config::auth::storage::AuthStorage::in_memory(
                    kiana_config::auth::storage::auth_data_from_value(serde_json::json!({})),
                ),
            )),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    )
}

/// 注册会触发后台 `void refresh({ allowNetwork: false })`；等快照追平后再断言
/// （TS 的上游测试同样依赖事件循环顺序，这里用轮询把顺序固定下来）。
async fn settle_auth(runtime: &ModelRuntime, provider_id: &str) {
    for _ in 0..200 {
        if runtime.has_configured_auth(provider_id) {
            return;
        }
        runtime
            .refresh(kiana_ai::models::ModelsRefreshOptions {
                allow_network: Some(false),
                ..Default::default()
            })
            .await
            .expect("settle refresh");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("provider {provider_id} never became configured");
}

fn faux_models() -> Vec<FauxModelDefinition> {
    vec![
        FauxModelDefinition {
            id: "small".to_string(),
            context_window: Some(1000),
            max_tokens: Some(100),
            input: Some(vec![ModelInputModality::Text]),
            reasoning: Some(false),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "large".to_string(),
            context_window: Some(50_000),
            max_tokens: Some(5000),
            input: Some(vec![ModelInputModality::Text, ModelInputModality::Image]),
            reasoning: Some(true),
            ..Default::default()
        },
    ]
}

fn make_route(runtime: Arc<ModelRuntime>, requests: Arc<Mutex<Vec<ModelRouteRequest>>>) -> RouteFn {
    Arc::new(move |request: ModelRouteRequest| {
        let runtime = Arc::clone(&runtime);
        let requests = Arc::clone(&requests);
        Box::pin(async move {
            let id = if request.thinking_level == ModelThinkingLevel::High {
                "large"
            } else {
                "small"
            };
            requests.lock().unwrap().push(request);
            let model = runtime.get_model("faux", id).expect("physical model");
            Ok(ModelRoute {
                model,
                thinking_level: ModelThinkingLevel::High,
                state: None,
            })
        }) as RouteFuture
    })
}

fn definition(
    provider: &str,
    id: &str,
    name: &str,
    route: Option<RouteFn>,
) -> VirtualModelDefinition {
    VirtualModelDefinition {
        provider: provider.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
        context_window: None,
        max_tokens: None,
        input: None,
        route,
    }
}

async fn create_fixture(
    requests: Arc<Mutex<Vec<ModelRouteRequest>>>,
) -> (Arc<ModelRuntime>, FauxProviderHandle, RouteFn, Model) {
    let runtime = create_runtime().await;
    let faux = faux_provider_with(RegisterFauxProviderOptions {
        models: Some(faux_models()),
        ..Default::default()
    });
    runtime
        .register_native_provider(Arc::clone(&faux.provider))
        .expect("register faux");
    let route = make_route(Arc::clone(&runtime), Arc::clone(&requests));
    runtime
        .register_virtual_model(definition(
            "router",
            "auto",
            "Auto",
            Some(Arc::clone(&route)),
        ))
        .expect("register virtual");
    runtime
        .refresh(kiana_ai::models::ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .expect("refresh");
    settle_auth(&runtime, "faux").await;
    let virtual_model = runtime.get_model("router", "auto").expect("virtual model");
    (runtime, faux, route, virtual_model)
}

fn assistant_from(model: &Model, text: &str) -> AssistantMessage {
    AssistantMessage {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        ..faux_assistant_message(text, FauxAssistantMessageOptions::default())
    }
}

fn user_message(content: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.to_string()),
        timestamp,
    })
}

#[tokio::test]
async fn lists_a_virtual_model_and_routes_it_to_a_physical_model_with_a_clamped_thinking_level() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, _route, virtual_model) = create_fixture(Arc::clone(&requests)).await;

    assert_eq!(virtual_model.provider, "router");
    assert_eq!(virtual_model.id, "auto");
    assert_eq!(virtual_model.context_window, Some(0));
    assert_eq!(virtual_model.max_tokens, Some(0));
    assert_eq!(
        virtual_model.input,
        vec![ModelInputModality::Text, ModelInputModality::Image]
    );
    assert_eq!(
        get_supported_thinking_levels(&virtual_model),
        vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]
    );

    let large = runtime.get_model("faux", "large").expect("large");
    let messages = vec![
        user_message("first", 1),
        Message::Assistant(AssistantMessage {
            thinking_level: Some(ModelThinkingLevel::Medium),
            ..assistant_from(&large, "answer")
        }),
        user_message("second", 2),
    ];

    let low = runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::Low,
                signal: None,
                failed: None,
                state: None,
            },
        )
        .await
        .expect("resolve low");
    assert_eq!(low.model.id, "small");
    // The router asked for "high", but the small model does not reason.
    assert_eq!(low.thinking_level, ModelThinkingLevel::Off);
    {
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests[0].previous,
            Some(RouteModelRef {
                model: large.clone(),
                thinking_level: Some(ModelThinkingLevel::Medium),
            })
        );
    }

    let high = runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::High,
                signal: None,
                failed: None,
                state: None,
            },
        )
        .await
        .expect("resolve high");
    assert_eq!(high.model.id, "large");
    assert_eq!(high.thinking_level, ModelThinkingLevel::High);
}

#[tokio::test]
async fn reports_the_failed_request_of_a_retry_separately_from_the_latest_successful_response() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, _route, virtual_model) = create_fixture(Arc::clone(&requests)).await;
    let small = runtime.get_model("faux", "small").expect("small");
    let large = runtime.get_model("faux", "large").expect("large");
    let failed = AssistantMessage {
        thinking_level: Some(ModelThinkingLevel::High),
        stop_reason: StopReason::Error,
        error_message: Some("overloaded_error".to_string()),
        ..assistant_from(&large, "")
    };
    let messages = vec![
        user_message("first", 1),
        Message::Assistant(assistant_from(&small, "answer")),
        user_message("second", 2),
    ];

    runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::Retry,
                thinking_level: ModelThinkingLevel::Low,
                signal: None,
                failed: Some(failed.clone()),
                state: None,
            },
        )
        .await
        .expect("retry resolve");

    // A routing failure names the virtual model, so there is no failed physical request to report.
    let failed_route = AssistantMessage {
        stop_reason: StopReason::Error,
        ..assistant_from(&virtual_model, "")
    };
    runtime
        .resolve_model(
            &virtual_model,
            &messages,
            ResolveModelOptions {
                reason: ModelRouteReason::Retry,
                thinking_level: ModelThinkingLevel::Low,
                signal: None,
                failed: Some(failed_route),
                state: None,
            },
        )
        .await
        .expect("retry resolve with routing failure");

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0].previous.as_ref().map(|p| p.model.id.as_str()),
        Some("small")
    );
    let first_failed = requests[0].failed.as_ref().expect("failed request");
    assert_eq!(first_failed.model.id, "large");
    assert_eq!(first_failed.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(first_failed.message, failed);
    assert!(requests[1].failed.is_none());
}

#[tokio::test]
async fn lists_several_virtual_models_under_a_provider_with_physical_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, route, _virtual_model) = create_fixture(Arc::clone(&requests)).await;

    runtime
        .register_virtual_model(definition("faux", "auto", "Auto", Some(Arc::clone(&route))))
        .expect("register faux auto");
    runtime
        .register_virtual_model(definition("faux", "fast", "Fast", Some(Arc::clone(&route))))
        .expect("register fast");
    runtime
        .register_virtual_model(definition("router", "second", "Second", None))
        .expect("register second");

    assert_eq!(
        ids(&runtime.get_models(Some("faux"))),
        vec![
            "small".to_string(),
            "large".to_string(),
            "auto".to_string(),
            "fast".to_string()
        ]
    );
    assert_eq!(
        ids(&runtime.get_models(Some("router"))),
        vec!["auto".to_string(), "second".to_string()]
    );

    let available = runtime
        .get_available(None, Default::default())
        .await
        .expect("available");
    assert_eq!(
        ids(&available
            .into_iter()
            .filter(|model| model.provider == "faux")
            .collect::<Vec<_>>()),
        vec![
            "small".to_string(),
            "large".to_string(),
            "auto".to_string(),
            "fast".to_string()
        ]
    );

    let fast = runtime.get_model("faux", "fast").expect("fast");
    let route = runtime
        .resolve_model(
            &fast,
            &[],
            ResolveModelOptions {
                reason: ModelRouteReason::User,
                thinking_level: ModelThinkingLevel::Off,
                signal: None,
                failed: None,
                state: None,
            },
        )
        .await
        .expect("resolve fast");
    assert_eq!(route.model.provider, "faux");
    assert_eq!(route.model.id, "small");

    let error = runtime
        .register_virtual_model(definition("faux", "large", "large", None))
        .expect_err("conflict");
    assert!(error.message().contains("conflicts with a physical model"));

    runtime.unregister_virtual_model("faux", "fast");
    runtime.unregister_virtual_model("router", "auto");
    assert_eq!(
        ids(&runtime.get_models(Some("faux"))),
        vec!["small".to_string(), "large".to_string(), "auto".to_string()]
    );
    assert_eq!(
        ids(&runtime.get_models(Some("router"))),
        vec!["second".to_string()]
    );
}

#[tokio::test]
async fn rejects_routes_to_virtual_or_unknown_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, _route, virtual_model) = create_fixture(Arc::clone(&requests)).await;
    let unknown = Model {
        provider: "faux".to_string(),
        id: "missing".to_string(),
        ..virtual_model.clone()
    };

    for model in [virtual_model.clone(), unknown] {
        let target = model.clone();
        let route: RouteFn = Arc::new(move |_request: ModelRouteRequest| {
            let target = target.clone();
            Box::pin(async move {
                Ok(ModelRoute {
                    model: target,
                    thinking_level: ModelThinkingLevel::Off,
                    state: None,
                })
            }) as RouteFuture
        });
        runtime
            .register_virtual_model(definition("router", "auto", "Auto", Some(route)))
            .expect("register route");
        let error = runtime
            .resolve_model(
                &virtual_model,
                &[],
                ResolveModelOptions {
                    reason: ModelRouteReason::User,
                    thinking_level: ModelThinkingLevel::Low,
                    signal: None,
                    failed: None,
                    state: None,
                },
            )
            .await
            .expect_err("route should fail");
        assert!(
            error.message().contains("which is not a physical model"),
            "unexpected error: {}",
            error.message()
        );
    }

    // 类型断言：路由错误类型是公开的。
    let _ = VirtualModelRouteError::new("x");
}

#[tokio::test]
async fn routes_direct_stream_simple_calls_within_the_routed_models_limits() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, faux, _route, virtual_model) = create_fixture(Arc::clone(&requests)).await;
    let max_tokens = Arc::new(Mutex::new(None));
    let max_tokens_slot = Arc::clone(&max_tokens);
    faux.set_responses(vec![FauxResponseStep::factory(
        move |input: kiana_ai::providers::faux::FauxResponseInput| {
            let max_tokens = Arc::clone(&max_tokens_slot);
            let model = input.model.clone();
            Box::pin(async move {
                *max_tokens.lock().unwrap() = input
                    .options
                    .as_ref()
                    .and_then(|options| options.max_tokens);
                Ok(AssistantMessage {
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    ..faux_assistant_message("hello", FauxAssistantMessageOptions::default())
                })
            })
        },
    )]);

    // The caller sized the request without knowing the routed model.
    let message = runtime
        .complete_simple(
            &virtual_model,
            &Context {
                messages: vec![user_message("hi", 1)],
                ..Default::default()
            },
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    reasoning: Some(ThinkingLevel::High),
                    base: kiana_ai::types::StreamOptions {
                        max_tokens: Some(20_000),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.reason)
            .collect::<Vec<_>>(),
        vec![ModelRouteReason::Direct]
    );
    assert_eq!(message.provider, "faux");
    assert_eq!(message.model, "large");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(*max_tokens.lock().unwrap(), Some(5000));
}

#[tokio::test]
async fn does_not_forward_caller_credentials_to_a_routed_model_of_another_provider() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, faux, route, virtual_model) = create_fixture(Arc::clone(&requests)).await;

    type SeenCalls = Arc<Mutex<Vec<(Option<String>, Option<kiana_ai::types::ProviderHeaders>)>>>;
    let seen: SeenCalls = Arc::new(Mutex::new(Vec::new()));
    let respond = {
        let seen = Arc::clone(&seen);
        move |input: kiana_ai::providers::faux::FauxResponseInput| {
            let seen = Arc::clone(&seen);
            let model = input.model.clone();
            Box::pin(async move {
                seen.lock().unwrap().push((
                    input
                        .options
                        .as_ref()
                        .and_then(|options| options.api_key.clone()),
                    input
                        .options
                        .as_ref()
                        .and_then(|options| options.headers.clone()),
                ));
                Ok(AssistantMessage {
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    ..faux_assistant_message("hello", FauxAssistantMessageOptions::default())
                })
            })
        }
    };
    faux.set_responses(vec![
        FauxResponseStep::factory(respond.clone()),
        FauxResponseStep::factory(respond),
    ]);

    let context = Context {
        messages: vec![user_message("hi", 1)],
        ..Default::default()
    };
    let options = || ModelsSimpleStreamOptions {
        base: SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                api_key: Some("caller-key".to_string()),
                headers: Some(headers(&[("x-caller", Some("1"))])),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // 纯虚拟 provider（router）路由到 faux：调用方凭证不跨 provider 转发。
    runtime
        .complete_simple(&virtual_model, &context, options())
        .await;
    runtime
        .register_virtual_model(definition("faux", "auto", "Auto", Some(Arc::clone(&route))))
        .expect("register faux auto");
    let faux_virtual = runtime.get_model("faux", "auto").expect("faux auto");
    runtime
        .complete_simple(&faux_virtual, &context, options())
        .await;

    let seen = seen.lock().unwrap();
    assert_ne!(seen[0].0.as_deref(), Some("caller-key"));
    assert!(
        seen[0]
            .1
            .as_ref()
            .and_then(|headers| header_value(headers, "x-caller"))
            .is_none()
    );
    assert_eq!(seen[1].0.as_deref(), Some("caller-key"));
    assert_eq!(
        seen[1]
            .1
            .as_ref()
            .and_then(|headers| header_value(headers, "x-caller")),
        Some(Some("1"))
    );
}

#[tokio::test]
async fn fails_unrouted_stream_calls_on_virtual_models() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (runtime, _faux, _route, virtual_model) = create_fixture(Arc::clone(&requests)).await;

    let message = runtime
        .complete(
            &virtual_model,
            &Context {
                messages: vec![user_message("hi", 1)],
                ..Default::default()
            },
            Default::default(),
        )
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .expect("error message")
            .contains("must be routed before streaming")
    );
}
