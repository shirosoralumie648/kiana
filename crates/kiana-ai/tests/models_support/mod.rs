//! models-runtime 单元测试的公共夹具（对应 `packages/ai/test/models-runtime.test.ts` 等
//! 文件顶部的本地 helper）。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthError, AuthInteraction, AuthResult,
    Credential, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth,
};
use kiana_ai::error::AiError;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateProviderOptions, Provider, ProviderApiImpl, ProviderStreams, RefreshModelsContext,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, Model,
    ModelCost, ModelInputModality, StopReason, StreamOptions, TextContent, Usage, UsageCost,
};
use kiana_ai::utils::abort::AbortScope;

/// `testModel(provider, id)`。
pub fn test_model(provider: &str, id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "test-api".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(10_000),
        max_tokens: Some(1_000),
        ..Model::default()
    }
}

/// `doneMessage(model, text)`。
pub fn done_message(model: &Model, text: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: Default::default(),
            text: text.into(),
            text_signature: None,
        })],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: now_ms(),
        ..AssistantMessage::default()
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// 一次 provider 调用的记录。
pub struct ProviderCall {
    pub model: Model,
    pub options: StreamOptions,
}

pub type StreamFn = Arc<dyn Fn(&Model, StreamOptions) -> AssistantMessageEventStream + Send + Sync>;
pub type SimpleStreamFn = Arc<
    dyn Fn(&Model, kiana_ai::types::SimpleStreamOptions) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
pub type RefreshFn = Arc<
    dyn Fn(RefreshModelsContext) -> futures::future::BoxFuture<'static, Result<(), AiError>>
        + Send
        + Sync,
>;
pub type GetModelsFn = Arc<dyn Fn() -> Vec<Model> + Send + Sync>;
pub type GetAllModelsFn = Arc<dyn Fn() -> Vec<kiana_ai::types::AnyModel> + Send + Sync>;
pub type FilterModelsFn = Arc<dyn Fn(Vec<Model>, Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// 一个可配置的测试 provider（TS `testProvider()` 的等价物）。
pub struct TestProvider {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub auth: ProviderAuth,
    pub get_models_fn: GetModelsFn,
    pub get_all_models_fn: Option<GetAllModelsFn>,
    pub refresh_fn: Option<RefreshFn>,
    pub calls: Arc<Mutex<Vec<ProviderCall>>>,
    pub stream_fn: StreamFn,
    pub stream_simple_fn: SimpleStreamFn,
    pub filter_models_fn: Option<FilterModelsFn>,
}

impl TestProvider {
    /// 默认 provider：一个 chat 模型，`stream` 立即返回 start+done。
    pub fn new(id: &str) -> Self {
        let models = vec![test_model(id, "model-a")];
        Self::with_models(id, models)
    }

    pub fn with_models(id: &str, models: Vec<Model>) -> Self {
        let models = Arc::new(Mutex::new(models));
        let get_models_fn: GetModelsFn = {
            let models = Arc::clone(&models);
            Arc::new(move || models.lock().unwrap_or_else(|e| e.into_inner()).clone())
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        Self {
            id: id.into(),
            name: id.into(),
            base_url: None,
            auth: ambient_auth(),
            get_models_fn,
            get_all_models_fn: None,
            refresh_fn: None,
            calls,
            stream_fn: Arc::new(|model, _options| response_stream(model, "ok")),
            stream_simple_fn: Arc::new(|model, _options| response_stream(model, "ok")),
            filter_models_fn: None,
        }
    }

    pub fn with_get_models(id: &str, get_models_fn: GetModelsFn) -> Self {
        let mut provider = Self::with_models(id, Vec::new());
        provider.get_models_fn = get_models_fn;
        provider
    }

    pub fn with_auth(mut self, auth: ProviderAuth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_refresh(mut self, refresh: RefreshFn) -> Self {
        self.refresh_fn = Some(refresh);
        self
    }

    pub fn with_calls(mut self, calls: Arc<Mutex<Vec<ProviderCall>>>) -> Self {
        self.calls = calls;
        self
    }

    pub fn with_get_all_models(mut self, get_all: GetAllModelsFn) -> Self {
        self.get_all_models_fn = Some(get_all);
        self
    }

    pub fn with_filter_models(mut self, filter: FilterModelsFn) -> Self {
        self.filter_models_fn = Some(filter);
        self
    }
}

/// 记录调用并以 `text` 完成的流。
pub fn response_stream(model: &Model, text: &str) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let message = done_message(model, text);
    let _ = stream.push(AssistantMessageEvent::Start {
        partial: Box::new(message.clone()),
    });
    let _ = stream.push(AssistantMessageEvent::Done {
        reason: kiana_ai::types::DoneReason::Stop,
        message: Box::new(message.clone()),
    });
    stream.end(message);
    stream
}

#[async_trait]
impl Provider for TestProvider {
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
        (self.get_models_fn)()
    }
    fn get_all_models(&self) -> Vec<kiana_ai::types::AnyModel> {
        match &self.get_all_models_fn {
            Some(get_all) => get_all(),
            None => self
                .get_models()
                .into_iter()
                .map(kiana_ai::types::AnyModel::Chat)
                .collect(),
        }
    }
    fn has_get_all_models(&self) -> bool {
        self.get_all_models_fn.is_some()
    }
    fn has_refresh_models(&self) -> bool {
        self.refresh_fn.is_some()
    }
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        match &self.refresh_fn {
            Some(refresh) => refresh(context).await,
            None => Ok(()),
        }
    }
    fn has_filter_models(&self) -> bool {
        self.filter_models_fn.is_some()
    }
    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        match &self.filter_models_fn {
            Some(filter) => filter(models, credential),
            None => models,
        }
    }
    fn stream(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        calls.push(ProviderCall {
            model: model.clone(),
            options: options.clone_for_test(),
        });
        drop(calls);
        (self.stream_fn)(model, options)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        options: kiana_ai::types::SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        calls.push(ProviderCall {
            model: model.clone(),
            options: options.base.clone_for_test(),
        });
        drop(calls);
        (self.stream_simple_fn)(model, options)
    }
}

/// 测试用的 `StreamOptions::clone`：`StreamOptions` 本身未实现 `Clone`（types.rs 冻结）。
trait CloneForTest {
    fn clone_for_test(&self) -> StreamOptions;
}

impl CloneForTest for StreamOptions {
    fn clone_for_test(&self) -> StreamOptions {
        StreamOptions {
            api_key: self.api_key.clone(),
            env: self.env.clone(),
            headers: self.headers.clone(),
            identity: self.identity.clone(),
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
            temperature: self.temperature,
            sampling_params: self.sampling_params.clone(),
            max_tokens: self.max_tokens,
            transport: self.transport,
            cache_retention: self.cache_retention,
            session_id: self.session_id.clone(),
            websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
            metadata: self.metadata.clone(),
            extra: self.extra.clone(),
            signal: self.signal.clone(),
            on_payload: self.on_payload.clone(),
            on_payload_async: self.on_payload_async.clone(),
            on_response: self.on_response.clone(),
            on_provider_stream_event: self.on_provider_stream_event.clone(),
            fetch: self.fetch.clone(),
            telemetry: self.telemetry.clone(),
        }
    }
}

/// `ambientAuth`：无凭证、报告已配置。
pub fn ambient_auth() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(AmbientAuth)),
        oauth: None,
    }
}

pub struct AmbientAuth;

#[async_trait]
impl ApiKeyAuth for AmbientAuth {
    fn name(&self) -> &str {
        "Ambient"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult::new(ModelAuth::default())))
    }
}

/// `envKeyAuth(key)`：stored key 优先，其次是环境 key。
pub fn env_key_auth(key: Option<&str>) -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(EnvKeyAuth {
            key: key.map(str::to_owned),
        })),
        oauth: None,
    }
}

pub struct EnvKeyAuth {
    pub key: Option<String>,
}

#[async_trait]
impl ApiKeyAuth for EnvKeyAuth {
    fn name(&self) -> &str {
        "Test API key"
    }
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let resolved = input
            .credential
            .and_then(|credential| credential.key.clone())
            .or_else(|| self.key.clone());
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        let source = if input.credential.is_some() {
            "stored"
        } else {
            "env"
        };
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(resolved),
                ..Default::default()
            },
            env: None,
            source: Some(source.into()),
        }))
    }
}

/// OAuth 刷新覆盖（`testOAuth({refresh})`）。
pub type OAuthRefreshFn = Arc<
    dyn Fn(
            OAuthCredential,
        ) -> futures::future::BoxFuture<'static, Result<OAuthCredential, AuthError>>
        + Send
        + Sync,
>;

/// `testOAuth(overrides?)`。
#[derive(Default)]
pub struct TestOAuth {
    pub refresh_fn: Option<OAuthRefreshFn>,
}

impl TestOAuth {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_refresh(refresh: OAuthRefreshFn) -> Arc<Self> {
        Arc::new(TestOAuth {
            refresh_fn: Some(refresh),
        })
    }
}

/// `testOAuth()` 默认行为：返回同样的凭证（Rust 版本用可覆盖的 refresh）。
pub fn test_oauth() -> Arc<TestOAuth> {
    Arc::new(TestOAuth::new())
}

#[async_trait]
impl OAuthAuth for TestOAuth {
    fn name(&self) -> &str {
        "Test OAuth"
    }
    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::new("not used"))
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        match &self.refresh_fn {
            Some(refresh) => refresh(credential.clone()).await,
            None => Ok(credential.clone()),
        }
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

pub fn oauth_auth(oauth: Arc<dyn OAuthAuth>) -> ProviderAuth {
    ProviderAuth {
        api_key: None,
        oauth: Some(oauth),
    }
}

/// 静态 `ProviderStreams`：`stream`/`stream_simple` 都返回一个已完成的空消息流。
pub struct SilentStreams;

#[async_trait]
impl ProviderStreams for SilentStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &kiana_ai::types::TranscriptContext,
        _options: kiana_ai::types::SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let _ = model;
        AssistantMessageEventStream::new()
    }
}

/// 便于构造 `CreateProviderOptions` 的默认值。
pub fn provider_options(id: &str, models: Vec<kiana_ai::types::AnyModel>) -> CreateProviderOptions {
    CreateProviderOptions {
        id: id.into(),
        auth: ambient_auth(),
        models,
        api: Some(ProviderApiImpl::Single(Arc::new(SilentStreams))),
        ..Default::default()
    }
}

/// 把 `Model` 变成 chat 类型的 `AnyModel`。
pub fn chat(model: Model) -> kiana_ai::types::AnyModel {
    kiana_ai::types::AnyModel::Chat(model)
}

/// 收集流事件直到结束。
pub async fn collect_events(stream: &AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    events
}

/// 等待条件（轮询），对应 TS 的 `vi.waitFor`。
pub async fn wait_for<F>(mut condition: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("condition was not met in time");
}

/// 空的 `ApiKeyCredential` 值（供断言使用）。
pub fn empty_api_key_credential() -> ApiKeyCredential {
    ApiKeyCredential::default()
}

/// `Credential` 的 api-key 取值。
pub fn credential_key(credential: &Credential) -> Option<&str> {
    credential
        .as_api_key()
        .and_then(|credential| credential.key.as_deref())
}
