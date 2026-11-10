//! TS: `test/virtual-models.test.ts`（18 例中的纯函数部分）。
//!
//! 依赖 `ModelRuntime` / `createAgentSession` 的用例属于其它 unit；这里覆盖
//! `getBranchSelection`（含目录查询次数契约）、`findLatestResponse`、`getVirtualModelState`、
//! `createVirtualModel`、`unroutedStream` 与 `withVirtualModels` 的目录合并/过滤/委托行为
//! （spec `ca-config-models.md` §2.23/§7.4）。

mod common;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::auth::types::ProviderAuth;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{ClassifierOptions, ImagesOptions, Provider};
use kiana_ai::types::{
    AnyModel, AssistantImages, AssistantMessage, AssistantRole, ClassifierContext,
    ClassifierResult, ImagesContext, Message, Model, ModelInputModality, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, TranscriptContext,
};
use kiana_session_core::session_manager::{
    FileEntry, ModelRef, SessionEntry, SessionManager, parse_session_entries,
};
use kiana_session_core::virtual_models::{
    THINKING_LEVELS, VIRTUAL_MODEL_API, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelDefinition,
    create_virtual_model, find_latest_response, get_branch_selection, get_virtual_model_state,
    unrouted_stream, with_virtual_models,
};

fn model(provider: &str, id: &str, api: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        ..Default::default()
    }
}

fn assistant_from(provider: &str, id: &str, api: &str, stop_reason: StopReason) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.to_string(),
        model: id.to_string(),
        usage: default_usage(),
        stop_reason,
        timestamp: 0,
        ..Default::default()
    }))
}

fn default_usage() -> kiana_ai::types::Usage {
    usage_with(0, 0, 0, 0.0)
}

fn select(build: impl FnOnce(&mut SessionManager)) -> (Option<ModelRef>, Vec<String>) {
    let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
    build(&mut session);
    let lookups: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let lookups_for_catalog = Arc::clone(&lookups);
    let get_model = move |provider: &str, model_id: &str| -> Option<Model> {
        lookups_for_catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(format!("{provider}/{model_id}"));
        if provider == "router" {
            Some(model("router", model_id, VIRTUAL_MODEL_API))
        } else {
            Some(model(provider, model_id, "faux"))
        }
    };
    let selection = get_branch_selection(&session.get_branch(None), &get_model);
    let lookups = lookups
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    (selection, lookups)
}

/// TS: "looks up only the last model_change"
#[test]
fn looks_up_only_the_last_model_change() {
    let physical = select(|session| {
        session.append_model_change("faux", "small").unwrap();
        for _ in 0..100 {
            session
                .append_message(assistant_from("faux", "large", "faux", StopReason::Stop))
                .unwrap();
        }
    });
    assert_eq!(
        physical.0,
        Some(ModelRef {
            provider: "faux".to_string(),
            model_id: "large".to_string(),
        })
    );
    assert_eq!(physical.1, ["faux/small"]);

    let routed = select(|session| {
        session.append_model_change("faux", "small").unwrap();
        session
            .append_message(assistant_from("faux", "small", "faux", StopReason::Stop))
            .unwrap();
        session.append_model_change("router", "auto").unwrap();
        for _ in 0..100 {
            session
                .append_message(assistant_from("faux", "large", "faux", StopReason::Stop))
                .unwrap();
        }
    });
    assert_eq!(
        routed.0,
        Some(ModelRef {
            provider: "router".to_string(),
            model_id: "auto".to_string(),
        })
    );
    assert_eq!(routed.1, ["router/auto"]);
}

/// TS: "uses the last model_change without responses after it"
#[test]
fn uses_the_last_model_change_without_responses_after_it() {
    let (selection, lookups) = select(|session| {
        session.append_model_change("router", "auto").unwrap();
        session
            .append_message(assistant_from("faux", "small", "faux", StopReason::Stop))
            .unwrap();
        session.append_model_change("faux", "small").unwrap();
    });
    assert_eq!(
        selection,
        Some(ModelRef {
            provider: "faux".to_string(),
            model_id: "small".to_string(),
        })
    );
    assert!(lookups.is_empty());
}

/// TS: "uses the latest physical response without a model_change"
#[test]
fn uses_the_latest_physical_response_without_a_model_change() {
    let (selection, lookups) = select(|session| {
        session
            .append_message(assistant_from("faux", "small", "faux", StopReason::Stop))
            .unwrap();
        session
            .append_message(assistant_from("faux", "large", "faux", StopReason::Stop))
            .unwrap();
        // 路由失败的虚拟模型消息仍保留在消息上。
        session
            .append_message(assistant_from(
                "router",
                "auto",
                VIRTUAL_MODEL_API,
                StopReason::Error,
            ))
            .unwrap();
    });
    assert_eq!(
        selection,
        Some(ModelRef {
            provider: "faux".to_string(),
            model_id: "large".to_string(),
        })
    );
    assert!(lookups.is_empty());
}

fn virtual_model(provider: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: VIRTUAL_MODEL_API.to_string(),
        provider: provider.to_string(),
        ..Default::default()
    }
}

fn definition(id: &str) -> VirtualModelDefinition {
    VirtualModelDefinition {
        provider: "router".to_string(),
        id: id.to_string(),
        name: id.to_string(),
        thinking_levels: None,
        context_window: None,
        max_tokens: None,
        input: None,
        route: None,
    }
}

/// `createVirtualModel`：目录条目字段、thinkingLevelMap 顺序/显式 null、默认 limits。
#[test]
fn create_virtual_model_builds_the_catalog_entry() {
    let model = create_virtual_model(&VirtualModelDefinition {
        thinking_levels: Some(vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]),
        context_window: Some(1200),
        max_tokens: Some(300),
        ..definition("auto")
    });
    assert_eq!(model.id, "auto");
    assert_eq!(model.name, "auto");
    assert_eq!(model.api, VIRTUAL_MODEL_API);
    assert_eq!(model.provider, "router");
    assert_eq!(model.base_url, "");
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(model.context_window, Some(1200));
    assert_eq!(model.max_tokens, Some(300));
    assert_eq!(
        model.input,
        vec![ModelInputModality::Text, ModelInputModality::Image]
    );
    assert_eq!(model.cost, kiana_ai::types::ModelCost::default());
    assert_eq!(model.model_type, None);

    let map = model.thinking_level_map.expect("thinking level map");
    let keys: Vec<ModelThinkingLevel> = map.keys().copied().collect();
    assert_eq!(keys, THINKING_LEVELS.to_vec());
    assert_eq!(map[&ModelThinkingLevel::Low], Some("low".to_string()));
    assert_eq!(map[&ModelThinkingLevel::High], Some("high".to_string()));
    assert_eq!(map[&ModelThinkingLevel::Off], None);
    assert_eq!(map[&ModelThinkingLevel::Max], None);

    // 默认：仅 "off"，reasoning=false，limits 0，input text+image。
    let default_model = create_virtual_model(&definition("plain"));
    assert_eq!(default_model.reasoning, Some(false));
    assert_eq!(default_model.context_window, Some(0));
    assert_eq!(default_model.max_tokens, Some(0));
    let map = default_model
        .thinking_level_map
        .expect("thinking level map");
    assert_eq!(map[&ModelThinkingLevel::Off], Some("off".to_string()));
    assert_eq!(map[&ModelThinkingLevel::Low], None);
}

/// `unroutedStream`：错误消息逐字为
/// `Virtual model {provider}/{id} must be routed before streaming`。
#[tokio::test]
async fn unrouted_stream_reports_the_exact_error_message() {
    let model = virtual_model("router", "auto");
    let message = unrouted_stream(&model)
        .result()
        .await
        .expect("error result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Virtual model router/auto must be routed before streaming")
    );
}

struct MockProvider {
    id: String,
    name: String,
    models: Vec<Model>,
    auth: ProviderAuth,
    base_url: Option<String>,
    stream_error: String,
    /// `hasGenerateImages` / `hasClassify`（TS 的 `provider.generateImages !== undefined`）。
    has_media: bool,
}

impl MockProvider {
    fn new(id: &str, models: Vec<Model>, stream_error: &str) -> Self {
        MockProvider {
            id: id.to_string(),
            name: format!("{id} name"),
            models,
            auth: ProviderAuth::default(),
            base_url: Some(format!("https://{id}.example")),
            stream_error: stream_error.to_string(),
            has_media: true,
        }
    }

    /// 不实现 `generateImages` / `classify` 的 provider（`has*` 为 false）。
    fn without_media(mut self) -> Self {
        self.has_media = false;
        self
    }
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }
    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }
    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        kiana_ai::api::lazy::lazy_stream_error(
            model,
            AiError::new(AiErrorKind::Other, self.stream_error.clone()),
        )
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        kiana_ai::api::lazy::lazy_stream_error(
            model,
            AiError::new(AiErrorKind::Other, self.stream_error.clone()),
        )
    }

    fn has_generate_images(&self) -> bool {
        self.has_media
    }

    fn generate_images<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        model: &'life1 Model,
        _context: &'life2 ImagesContext,
        _options: Option<&'life3 ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        let model = model.clone();
        Box::pin(async move {
            AssistantImages {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: format!("delegated-images/{}", model.id),
                ..Default::default()
            }
        })
    }

    fn has_classify(&self) -> bool {
        self.has_media
    }

    fn classify<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        _model: &'life1 Model,
        _context: &'life2 ClassifierContext,
        _options: Option<&'life3 ClassifierOptions>,
    ) -> Pin<Box<dyn Future<Output = ClassifierResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            ClassifierResult {
                model: "delegated-classify".to_string(),
                ..Default::default()
            }
        })
    }
}

/// `withVirtualModels(providerId, undefined, virtualModels)`：无凭据 provider 只列虚拟模型，
/// auth 名称/来源逐字，stream 未路由即失败。
#[tokio::test]
async fn with_virtual_models_without_a_provider_lists_only_virtual_models() {
    let model = virtual_model("router", "auto");
    let provider = with_virtual_models("router", None, vec![model.clone()]);

    assert_eq!(provider.id(), "router");
    assert_eq!(provider.name(), "router");
    assert_eq!(provider.base_url(), None);
    assert_eq!(provider.get_models(), vec![model.clone()]);
    let api_key = provider.auth().api_key.as_ref().expect("api key auth");
    assert_eq!(api_key.name(), "Virtual model");

    let message = provider
        .stream(
            &model,
            &TranscriptContext::default(),
            StreamOptions::default(),
        )
        .result()
        .await
        .expect("error result");
    assert_eq!(
        message.error_message.as_deref(),
        Some("Virtual model router/auto must be routed before streaming")
    );

    let message = provider
        .stream_simple(
            &model,
            &TranscriptContext::default(),
            SimpleStreamOptions::default(),
        )
        .result()
        .await
        .expect("error result");
    assert_eq!(message.stop_reason, StopReason::Error);
}

/// `withVirtualModels` 有 provider 时：物理 chat 模型中与虚拟模型同 id 的被隐藏，虚拟模型按
/// 注册顺序追加；未注册的物理模型保留；委托 id/name/base_url/auth/stream。
#[tokio::test]
async fn with_virtual_models_merges_catalogs_and_delegates() {
    let physical_auto = Model {
        id: "auto".to_string(),
        name: "physical auto".to_string(),
        api: "faux".to_string(),
        provider: "faux".to_string(),
        ..Default::default()
    };
    let small = Model {
        id: "small".to_string(),
        name: "small".to_string(),
        api: "faux".to_string(),
        provider: "faux".to_string(),
        ..Default::default()
    };
    let inner = Arc::new(MockProvider::new(
        "faux",
        vec![physical_auto.clone(), small.clone()],
        "inner error",
    ));
    let auto = virtual_model("faux", "auto");
    let fast = virtual_model("faux", "fast");
    let provider = with_virtual_models("faux", Some(inner), vec![auto.clone(), fast.clone()]);

    assert_eq!(provider.id(), "faux");
    assert_eq!(provider.name(), "faux name");
    assert_eq!(provider.base_url(), Some("https://faux.example"));
    assert!(provider.auth().api_key.is_none());

    let ids: Vec<String> = provider
        .get_models()
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(ids, ["small", "auto", "fast"]);
    let all_ids: Vec<String> = provider
        .get_all_models()
        .into_iter()
        .map(|model| model.model().id.clone())
        .collect();
    assert_eq!(all_ids, ["small", "auto", "fast"]);

    // filter_models：物理集合先过滤（隐藏同 id chat），再委托 inner，最后追加输入里的虚拟模型。
    let filtered = provider.filter_models(
        vec![
            small.clone(),
            physical_auto.clone(),
            virtual_model("faux", "auto"),
        ],
        None,
    );
    let filtered_ids: Vec<String> = filtered.into_iter().map(|model| model.id).collect();
    assert_eq!(filtered_ids, ["small", "auto"]);

    let filtered_all = provider.filter_all_models(
        vec![
            AnyModel::Chat(small.clone()),
            AnyModel::Chat(physical_auto.clone()),
            AnyModel::Chat(virtual_model("faux", "auto")),
        ],
        None,
    );
    let filtered_all_ids: Vec<String> = filtered_all
        .into_iter()
        .map(|model| model.model().id.clone())
        .collect();
    assert_eq!(filtered_all_ids, ["small", "auto"]);

    // 虚拟模型：未路由错误；物理模型：委托给 inner。
    for model in [&auto, &fast] {
        let message = provider
            .stream(
                model,
                &TranscriptContext::default(),
                StreamOptions::default(),
            )
            .result()
            .await
            .expect("error result");
        assert_eq!(
            message.error_message.as_deref(),
            Some(
                format!(
                    "Virtual model {}/{} must be routed before streaming",
                    model.provider, model.id
                )
                .as_str()
            )
        );
    }
    let message = provider
        .stream(
            &small,
            &TranscriptContext::default(),
            StreamOptions::default(),
        )
        .result()
        .await
        .expect("delegated error result");
    assert_eq!(message.error_message.as_deref(), Some("inner error"));
}

/// `findLatestResponse`：跳过 error/aborted，返回最新的成功响应。
#[test]
fn find_latest_response_skips_failed_responses() {
    let messages = vec![
        assistant_from("p", "first", "api", StopReason::Stop),
        assistant_from("p", "error", "api", StopReason::Error),
        assistant_from("p", "aborted", "api", StopReason::Aborted),
        AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text("hi".to_string()),
            timestamp: 0,
        })),
    ];
    let response = find_latest_response(&messages).expect("latest response");
    assert_eq!(response.model, "first");
}

/// `getVirtualModelState`：反向找最后一个匹配的 `kiana.virtual-model-state` 自定义条目。
#[test]
fn get_virtual_model_state_reads_the_latest_matching_entry() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    session
        .append_custom_entry(
            VIRTUAL_MODEL_STATE_ENTRY,
            Some(
                serde_json::json!({"provider": "router", "modelId": "auto", "state": {"step": 1}}),
            ),
        )
        .unwrap();
    session
        .append_custom_entry(
            VIRTUAL_MODEL_STATE_ENTRY,
            Some(
                serde_json::json!({"provider": "router", "modelId": "fast", "state": {"step": 2}}),
            ),
        )
        .unwrap();
    session
        .append_custom_entry(
            VIRTUAL_MODEL_STATE_ENTRY,
            Some(
                serde_json::json!({"provider": "router", "modelId": "auto", "state": {"step": 3}}),
            ),
        )
        .unwrap();
    session
        .append_custom_entry("other", Some(serde_json::json!({"provider": "router"})))
        .unwrap();

    let branch = session.get_branch(None);
    assert_eq!(
        get_virtual_model_state(&branch, "router", "auto"),
        Some(serde_json::json!({"step": 3}))
    );
    assert_eq!(
        get_virtual_model_state(&branch, "router", "fast"),
        Some(serde_json::json!({"step": 2}))
    );
    assert_eq!(get_virtual_model_state(&branch, "router", "missing"), None);
}

/// `getBranchSelection`：宽松解析成 `AgentMessage::Custom` 的 assistant 消息同样提供物理响应
/// （评审 #5）。
#[test]
fn get_branch_selection_reads_lenient_custom_assistant_messages() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","timestamp":5}}"#,
    ];
    let entries: Vec<SessionEntry> = parse_session_entries(&lines.join("\n"))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect();
    let lookups = Arc::new(Mutex::new(Vec::new()));
    let lookups_for_catalog = Arc::clone(&lookups);
    let selection = get_branch_selection(&entries, &|provider: &str, model_id: &str| {
        lookups_for_catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(format!("{provider}/{model_id}"));
        None
    });
    assert_eq!(
        selection,
        Some(ModelRef {
            provider: "p".to_string(),
            model_id: "m".to_string(),
        })
    );
    assert!(lookups.lock().unwrap().is_empty());
}

/// `getBranchSelection`：已注销的虚拟模型不再保持，回退到最近的物理响应。
#[test]
fn get_branch_selection_falls_back_when_the_virtual_model_is_unregistered() {
    let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
    session.append_model_change("router", "auto").unwrap();
    session
        .append_message(assistant_from("faux", "large", "faux", StopReason::Stop))
        .unwrap();
    let selection = get_branch_selection(&session.get_branch(None), &|_provider, _model_id| None);
    assert_eq!(
        selection,
        Some(ModelRef {
            provider: "faux".to_string(),
            model_id: "large".to_string(),
        })
    );
}

/// 回归（评审 round 2 #4）：TS 的 `withVirtualModels` 用 `{...provider}` 展开，内层
/// provider 的 `generateImages` / `classify`（及 `has*` 谓词）会被保留。Rust 的包装 provider
/// 必须逐项委托，而不是落到 `Provider` trait 的「不支持」默认实现。
#[tokio::test]
async fn with_virtual_models_delegates_image_and_classifier_support() {
    let inner = Arc::new(MockProvider::new("faux", vec![], "inner error"));
    let wrapped = with_virtual_models("faux", Some(inner), vec![virtual_model("faux", "auto")]);
    let model = model("faux", "m", "faux");

    assert!(wrapped.has_generate_images());
    assert!(wrapped.has_classify());
    let images = wrapped
        .generate_images(&model, &ImagesContext::default(), None)
        .await;
    assert_eq!(images.model, "delegated-images/m");
    let classified = wrapped
        .classify(&model, &ClassifierContext::default(), None)
        .await;
    assert_eq!(classified.model, "delegated-classify");

    // 内层没有实现时，包装 provider 也报告不支持（`has*` 委托，而不是默认 true）。
    let inner = Arc::new(MockProvider::new("faux", vec![], "inner error").without_media());
    let wrapped = with_virtual_models("faux", Some(inner), vec![virtual_model("faux", "auto")]);
    assert!(!wrapped.has_generate_images());
    assert!(!wrapped.has_classify());
}

/// 回归（评审 round 2 #13）：`findLatestResponse` 只看 `role === "assistant"` 与
/// `stopReason`，宽松解析成 `AgentMessage::Custom` 的 assistant 消息同样参与；`stopReason`
/// 缺省（TS 的 `undefined`）不是 error/aborted。
#[test]
fn find_latest_response_includes_lenient_assistant_messages() {
    let messages: Vec<AgentMessage> = vec![
        serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [], "api": "a", "provider": "p", "model": "first",
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "stop", "timestamp": 1,
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": null, "provider": "p2", "model": "lax",
        }))
        .unwrap(),
    ];
    let response = find_latest_response(&messages).expect("latest response");
    assert_eq!(response.provider, "p2");
    assert_eq!(response.model, "lax");
    assert_eq!(response.stop_reason, StopReason::Pending);

    // 宽松消息的 stopReason: "error" 照样跳过。
    let messages: Vec<AgentMessage> = vec![
        serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [], "api": "a", "provider": "p", "model": "ok",
            "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                      "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "stop", "timestamp": 1,
        }))
        .unwrap(),
        serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": null, "provider": "p2", "model": "failed",
            "stopReason": "error",
        }))
        .unwrap(),
    ];
    let response = find_latest_response(&messages).expect("latest successful response");
    assert_eq!(response.model, "ok");
}
