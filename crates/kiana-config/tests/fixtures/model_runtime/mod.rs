//! `model-runtime` 各测试套件的共享夹具。
//!
//! 对应 TS 的 `test/model-runtime-test-utils.ts` 与各 suite 内联的
//! `createRuntime`/`provider`/`model` 辅助函数。
// 冻结的 `AiError`（大 Err 变体）与记录型嵌套属于测试夹具的固有形状。
#![allow(dead_code, clippy::result_large_err, clippy::type_complexity)]

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthError, AuthEvent,
    AuthInteraction, AuthPromptRequest, AuthResult, ProviderAuth,
};
use kiana_ai::error::AiError;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::models::{
    ClassifierOptions, ImagesOptions, ModelsRefreshOptions, ModelsRefreshResult, Provider,
    ProviderClassifier, ProviderImages, ProviderStreams, RefreshModelsContext,
};
use kiana_ai::types::{
    AssistantImages, ClassifierContext, ClassifierResult, DeferredCancelOptions,
    DeferredFetchOptions, DeferredHandle, ImagesContext, Model, ProviderHeaders,
    SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::models_store::InMemoryCodingAgentModelsStore;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::composer::ProviderModelConfig;
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::{Value, json};
use tempfile::TempDir;

/// 盒装 future（与 `kiana_ai::http::BoxFuture` 同形，便于测试闭包）。
pub type BoxFuture<T> = Pin<Box<dyn std::future::Future<Output = T> + Send>>;

// ---------------------------------------------------------------------------
// models.json / runtime / registry
// ---------------------------------------------------------------------------

/// 临时目录 + `models.json` + 内存 `AuthStorage` + 内存 models store。
pub struct Harness {
    dir: TempDir,
    pub auth_storage: Arc<AuthStorage>,
    pub models_json_path: PathBuf,
    pub models_store: Arc<InMemoryCodingAgentModelsStore>,
}

impl Harness {
    /// 空凭证、空 models.json。
    pub fn new() -> Self {
        Self::with_auth(json!({}))
    }

    /// 给定 auth.json 内容的凭证存储。
    pub fn with_auth(auth: Value) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let models_json_path = dir.path().join("models.json");
        let auth_storage = Arc::new(AuthStorage::in_memory(auth_data_from_value(auth)));
        Self {
            dir,
            auth_storage,
            models_json_path,
            models_store: Arc::new(InMemoryCodingAgentModelsStore::new()),
        }
    }

    /// 临时目录（命令型 apiKey 的计数器文件等）。
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// `writeModelsJson(providers)`：`{"providers": providers}`。
    pub fn write_providers(&self, providers: Value) {
        self.write_raw(json!({ "providers": providers }));
    }

    /// `writeRawModelsJson` / `writeFileSync(modelsJsonPath, ...)`。
    pub fn write_raw(&self, value: Value) {
        std::fs::write(
            &self.models_json_path,
            serde_json::to_string(&value).expect("serialize models.json"),
        )
        .expect("write models.json");
    }

    /// `modelsJsonPath`（字符串）。
    pub fn models_path_string(&self) -> String {
        self.models_json_path.to_string_lossy().into_owned()
    }

    /// `ModelRegistry.createModelRegistry`（`allowModelNetwork: false`，内存 store）。
    pub async fn create(&self) -> Arc<ModelRuntime> {
        self.create_with_network(false, self.models_store.clone())
            .await
    }

    /// 指定是否允许联网的 runtime。
    pub async fn create_with_network(
        &self,
        allow_model_network: bool,
        models_store: Arc<dyn kiana_ai::models::ModelsStore>,
    ) -> Arc<ModelRuntime> {
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(
                self.auth_storage.clone() as Arc<dyn kiana_ai::auth::types::CredentialStore>
            ),
            models_path: ModelsPathOption::Path(self.models_path_string()),
            models_store: Some(models_store),
            allow_model_network,
            ..Default::default()
        })
        .await
        .expect("runtime create")
        .into()
    }

    /// 只创建 `ModelRuntime`，保留 `create()` 的错误。
    pub async fn try_create(&self) -> Result<ModelRuntime, AiError> {
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(
                self.auth_storage.clone() as Arc<dyn kiana_ai::auth::types::CredentialStore>
            ),
            models_path: ModelsPathOption::Path(self.models_path_string()),
            models_store: Some(self.models_store.clone()),
            allow_model_network: false,
            ..Default::default()
        })
        .await
    }

    /// `createModelRegistry(...)`。
    pub async fn registry(&self) -> (Arc<ModelRuntime>, ModelRegistry) {
        let runtime = self.create().await;
        (Arc::clone(&runtime), ModelRegistry::new(runtime))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// 不联网的 `registry.refresh()`。
pub async fn local_refresh(runtime: &ModelRuntime) -> Result<ModelsRefreshResult, AiError> {
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
}

/// `getModelsForProvider`。
pub fn models_for_provider(registry: &ModelRegistry, provider: &str) -> Vec<Model> {
    registry
        .get_all()
        .into_iter()
        .filter(|model| model.provider == provider)
        .collect()
}

/// `models.map((m) => m.id)`。
pub fn ids(models: &[Model]) -> Vec<String> {
    models.iter().map(|model| model.id.clone()).collect()
}

/// 大小写不敏感地取 header 值（`ProviderHeaders` 的 `null` 值保留为 `None`）。
pub fn header_value<'a>(headers: &'a ProviderHeaders, name: &str) -> Option<Option<&'a str>> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_deref())
}

/// `compat?.<key>`（`compat` 是任意 JSON）。
pub fn compat_value<'a>(model: &'a Model, key: &str) -> Option<&'a Value> {
    model.compat.as_ref().and_then(|compat| compat.get(key))
}

// ---------------------------------------------------------------------------
// 模型构造
// ---------------------------------------------------------------------------

/// chat 模型（`openai-completions`，测试默认）。
pub fn chat_model(provider: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "id": id,
        "name": id,
        "api": "openai-completions",
        "provider": provider,
        "baseUrl": "https://example.test/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
        "maxTokens": 100
    }))
    .expect("chat model")
}

/// `type: "image"` 模型（`test-images`）。
pub fn image_model(provider: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "type": "image",
        "id": id,
        "name": id,
        "api": "test-images",
        "provider": provider,
        "baseUrl": "https://images.test/v1",
        "input": ["text"],
        "output": ["image"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
    }))
    .expect("image model")
}

/// `type: "classifier"` 模型（`test-classifier`）。
pub fn classifier_model(provider: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "type": "classifier",
        "id": id,
        "name": id,
        "api": "test-classifier",
        "provider": provider,
        "baseUrl": "https://classifier.test/v1",
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000
    }))
    .expect("classifier model")
}

/// 扩展注册的 chat 模型配置（`ProviderModelConfig::Chat`，provider 级 `api` 继承）。
pub fn chat_model_config(id: &str, name: &str) -> ProviderModelConfig {
    ProviderModelConfig::Chat(kiana_config::models::composer::ProviderChatModelConfig {
        base: kiana_config::models::composer::ProviderModelConfigBase {
            id: id.to_string(),
            name: name.to_string(),
            api: None,
            base_url: None,
            input: vec![kiana_ai::types::ModelInputModality::Text],
            input_limits: None,
            cost: kiana_ai::types::ModelCost::default(),
            headers: None,
            declared_type: None,
        },
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128000.0,
        max_tokens: 4096.0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    })
}

/// `ProviderHeaders` 构造：`(name, value?)`。
pub fn headers(pairs: &[(&str, Option<&str>)]) -> ProviderHeaders {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.map(str::to_string)))
        .collect()
}

// ---------------------------------------------------------------------------
// Provider 夹具
// ---------------------------------------------------------------------------

/// 无输出的流实现（`stream: () => throw new Error("unused")` 的 Rust 等价物：
/// 返回一个不会产生事件的流，调用方不应消费它）。
pub struct UnusedStreams;

impl ProviderStreams for UnusedStreams {
    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }
}

/// `refreshModels(context)` 闭包。
pub type RefreshModelsFn =
    Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<(), AiError>> + Send + Sync>;
/// `fetchDeferred(model, handle, options)` 闭包。
pub type FetchDeferredFn = Arc<
    dyn Fn(&Model, &DeferredHandle, DeferredFetchOptions) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
/// `cancelDeferred(model, handle, options)` 闭包。
pub type CancelDeferredFn = Arc<
    dyn Fn(&Model, &DeferredHandle, DeferredCancelOptions) -> BoxFuture<Result<(), AiError>>
        + Send
        + Sync,
>;

/// TS 测试里的 `Provider` 字面量：可注入 auth、refreshModels、deferred 方法。
pub struct TestProvider {
    pub id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub auth: ProviderAuth,
    pub models: Vec<Model>,
    pub refresh_models: Option<RefreshModelsFn>,
    pub fetch_deferred: Option<FetchDeferredFn>,
    pub cancel_deferred: Option<CancelDeferredFn>,
    pub images: IndexMap<String, Arc<dyn ProviderImages>>,
    pub classifiers: IndexMap<String, Arc<dyn ProviderClassifier>>,
}

impl TestProvider {
    /// `provider(id)`：id 即 name，模型为单个 chat 模型 `dynamic`。
    pub fn new(id: &str, auth: ProviderAuth, models: Vec<Model>) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            base_url: None,
            auth,
            models,
            refresh_models: None,
            fetch_deferred: None,
            cancel_deferred: None,
            images: IndexMap::new(),
            classifiers: IndexMap::new(),
        }
    }

    /// 带单个 `dynamic` chat 模型的 provider（credential-sync 的默认夹具）。
    pub fn dynamic(id: &str, auth: ProviderAuth) -> Self {
        Self::new(id, auth, vec![chat_model(id, "dynamic")])
    }

    /// `withRefreshModels`。
    pub fn with_refresh_models(mut self, f: RefreshModelsFn) -> Self {
        self.refresh_models = Some(f);
        self
    }

    /// `withName`。
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// `withBaseUrl`。
    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = Some(base_url.to_string());
        self
    }

    /// `fetchDeferred`。
    pub fn with_fetch_deferred(mut self, f: FetchDeferredFn) -> Self {
        self.fetch_deferred = Some(f);
        self
    }

    /// `cancelDeferred`。
    pub fn with_cancel_deferred(mut self, f: CancelDeferredFn) -> Self {
        self.cancel_deferred = Some(f);
        self
    }

    /// `Arc<dyn Provider>`。
    pub fn into_provider(self) -> Arc<dyn Provider> {
        Arc::new(self)
    }
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
        self.models.clone()
    }

    fn has_refresh_models(&self) -> bool {
        self.refresh_models.is_some()
    }

    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        match &self.refresh_models {
            Some(refresh) => refresh(context).await,
            None => Ok(()),
        }
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        AssistantMessageEventStream::new()
    }

    fn has_fetch_deferred(&self) -> bool {
        self.fetch_deferred.is_some()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.fetch_deferred
            .as_ref()
            .map(|fetch| fetch(model, handle, options))
    }

    fn has_cancel_deferred(&self) -> bool {
        self.cancel_deferred.is_some()
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Result<(), AiError> {
        match &self.cancel_deferred {
            Some(cancel) => cancel(model, handle, options).await,
            None => Err(kiana_ai::error::AiError::new(
                kiana_ai::error::AiErrorKind::Other,
                "Provider does not support deferred responses",
            )),
        }
    }

    fn has_generate_images(&self) -> bool {
        !self.images.is_empty()
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        match self.images.get(&model.api) {
            Some(images) => images.generate_images(model, context, options).await,
            None => error_images(model),
        }
    }

    fn has_classify(&self) -> bool {
        !self.classifiers.is_empty()
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        match self.classifiers.get(&model.api) {
            Some(classifier) => classifier.classify(model, context, options).await,
            None => error_classifier(model),
        }
    }
}

/// 生成失败结果（默认 `generate_images` 的错误形态）。
pub fn error_images(model: &Model) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: kiana_ai::types::ImagesStopReason::Error,
        error_message: Some(format!("No image implementation for api {}", model.api)),
        timestamp: 0,
    }
}

/// 分类失败结果。
pub fn error_classifier(model: &Model) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: kiana_ai::types::ClassifierStopReason::Error,
        error_message: Some(format!(
            "No classifier implementation for api {}",
            model.api
        )),
        timestamp: 0,
    }
}

// ---------------------------------------------------------------------------
// Auth 夹具
// ---------------------------------------------------------------------------

/// `login()` 闭包。
pub type LoginFn = Arc<dyn Fn() -> BoxFuture<Result<ApiKeyCredential, AuthError>> + Send + Sync>;
/// `check({ credential })` 闭包。
pub type CheckFn = Arc<
    dyn Fn(Option<ApiKeyCredential>) -> BoxFuture<Result<Option<AuthCheck>, AuthError>>
        + Send
        + Sync,
>;
/// `resolve({ credential })` 闭包。
pub type ResolveFn = Arc<
    dyn Fn(Option<ApiKeyCredential>) -> BoxFuture<Result<Option<AuthResult>, AuthError>>
        + Send
        + Sync,
>;

/// TS 测试里的 `auth.apiKey` 字面量。
pub struct TestApiKeyAuth {
    pub name: String,
    pub login: Option<LoginFn>,
    pub check: Option<CheckFn>,
    pub resolve: ResolveFn,
}

impl TestApiKeyAuth {
    /// 默认 resolve：无凭证时 `None`。
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            login: None,
            check: None,
            resolve: Arc::new(|_| Box::pin(async { Ok(None) })),
        }
    }

    /// 自定义 `resolve`。
    pub fn with_resolve(mut self, resolve: ResolveFn) -> Self {
        self.resolve = resolve;
        self
    }

    /// 自定义 `login`。
    pub fn with_login(mut self, login: LoginFn) -> Self {
        self.login = Some(login);
        self
    }

    /// 自定义 `check`。
    pub fn with_check(mut self, check: CheckFn) -> Self {
        self.check = Some(check);
        self
    }

    /// 包装成 `ProviderAuth`（仅 api_key）。
    pub fn provider_auth(self: Arc<Self>) -> ProviderAuth {
        ProviderAuth {
            api_key: Some(self),
            oauth: None,
        }
    }
}

#[async_trait]
impl ApiKeyAuth for TestApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn has_login(&self) -> bool {
        self.login.is_some()
    }

    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        match &self.login {
            Some(login) => login().await,
            None => Err(AuthError::new(format!(
                "{} does not support login",
                self.name
            ))),
        }
    }

    fn has_check(&self) -> bool {
        self.check.is_some()
    }

    async fn check(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        match &self.check {
            Some(check) => check(input.credential.cloned()).await,
            None => Ok(None),
        }
    }

    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        (self.resolve)(input.credential.cloned()).await
    }
}

/// TS 里的 `prompt`/`notify` 回调对象。
pub struct TestInteraction {
    signal: AbortScope,
    answer: String,
}

impl TestInteraction {
    /// `{ prompt: async () => "unused", notify: () => {} }`。
    pub fn new() -> Self {
        Self {
            signal: AbortScope::new(),
            answer: "unused".to_string(),
        }
    }

    /// 给定取消信号与答题文本。
    pub fn with_signal(signal: AbortScope) -> Self {
        Self {
            signal,
            answer: "unused".to_string(),
        }
    }

    /// 取消信号（测试可在登录进行中 abort）。
    pub fn signal(&self) -> &AbortScope {
        &self.signal
    }
}

impl Default for TestInteraction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthInteraction for TestInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(&self, _request: AuthPromptRequest) -> Result<String, AuthError> {
        Ok(self.answer.clone())
    }

    fn notify(&self, _event: &AuthEvent) {}
}

// ---------------------------------------------------------------------------
// 可记录的 HTTP 客户端（`options.fetch` 注入）
// ---------------------------------------------------------------------------

/// 一次被记录的请求。
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl RecordedRequest {
    /// 大小写不敏感地取 header。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 请求 body 解析成 JSON。
    pub fn json(&self) -> Value {
        serde_json::from_slice(self.body.as_deref().unwrap_or_default()).expect("json body")
    }
}

/// 固定响应。
pub struct MockResponse {
    pub status: u16,
    pub headers: IndexMap<String, String>,
    pub body: Vec<u8>,
}

impl MockResponse {
    /// 200 + JSON body。
    pub fn json(value: Value) -> Self {
        let mut headers = IndexMap::new();
        headers.insert("content-type".into(), "application/json".into());
        Self {
            status: 200,
            headers,
            body: serde_json::to_vec(&value).expect("json"),
        }
    }

    /// 200 + SSE body。
    pub fn sse(text: &str) -> Self {
        let mut headers = IndexMap::new();
        headers.insert("content-type".into(), "text/event-stream".into());
        Self {
            status: 200,
            headers,
            body: text.as_bytes().to_vec(),
        }
    }
}

/// 记录请求并按 handler 返回响应的 `HttpClient`。
pub struct MockHttpClient {
    requests: Mutex<Vec<RecordedRequest>>,
    handler: Box<dyn Fn(&RecordedRequest) -> Result<MockResponse, HttpError> + Send + Sync>,
}

impl MockHttpClient {
    /// 新建。
    pub fn new(
        handler: impl Fn(&RecordedRequest) -> Result<MockResponse, HttpError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }

    /// 所有已记录的请求。
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 请求数。
    pub fn request_count(&self) -> usize {
        self.requests().len()
    }

    /// `options.fetch`。
    pub fn fetch(self: &Arc<Self>) -> Option<Arc<dyn HttpClient>> {
        Some(Arc::clone(self) as Arc<dyn HttpClient>)
    }
}

impl HttpClient for MockHttpClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        let recorded = RecordedRequest {
            url: request.url.clone(),
            method: request.method.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
        };
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(recorded.clone());
        let response = (self.handler)(&recorded);
        Box::pin(async move {
            let response = response?;
            let body: kiana_ai::http::ByteStream =
                Box::pin(futures::stream::once(async move { Ok(response.body) }));
            Ok(HttpResponse {
                response: kiana_ai::types::ProviderResponse {
                    status: response.status,
                    headers: response.headers,
                },
                body,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// 其它
// ---------------------------------------------------------------------------

/// `json!({...}).as_object()` → `IndexMap<String, Value>`。
pub fn json_object(value: Value) -> IndexMap<String, Value> {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected object, got {other}"),
    }
}

/// `AuthStorage` 的 api_key 凭证 JSON。
pub fn api_key_credential(key: &str) -> Value {
    json!({ "type": "api_key", "key": key })
}

/// `AuthStorage` 的 oauth 凭证 JSON（expires 用 JS `Date.now()` 时基）。
pub fn oauth_credential(access: &str, refresh: &str, expires: i64) -> Value {
    json!({
        "type": "oauth",
        "access": access,
        "refresh": refresh,
        "expires": expires
    })
}

/// 当前毫秒（JS `Date.now()`）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as i64
}
