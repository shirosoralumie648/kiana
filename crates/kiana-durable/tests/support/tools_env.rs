//! `tools.test.ts` 的测试替身：包住 [`NodeExecutionEnv`] 的 [`TestEnv`]（可注入读/写/打开/exec
//! 钩子与文件系统 id）与最小 [`ToolExecutionApi`]。
//!
//! 这些只用于本目录的集成测试；生产代码没有对应物（TS 的测试用子类与对象字面量）。

#![allow(dead_code)]

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::resolve::AuthResolutionOverrides;
use kiana_ai::auth::types::{
    AuthCheck, AuthError, AuthInteraction, AuthOperationOptions, AuthResult, AuthType, Credential,
    LoginOptions,
};
use kiana_ai::models::{
    GetAuthTarget, Models, ModelsClassifierOptions, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsImagesOptions, ModelsRefreshOptions, ModelsRefreshResult,
    ModelsSimpleStreamOptions, ModelsStreamOptions, Provider,
};
use kiana_ai::{
    AiError, AnyModel, AssistantImages, AssistantMessage, AssistantMessageEventStream,
    ClassifierContext, ClassifierResult, Context as AiContext, DeferredHandle, ImagesContext,
    Model, ModelType,
};
use kiana_chord::context::AbortSignal;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context, ContextRef};
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirReader, ExecutionEnv, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileListing, FileSystem,
    FileWatcher, LineRange, LineScan, ListFilesOptions, OpenBinaryReaderOptions,
    ReadTextLinesOptions, RemoveOptions, Shell, ShellCommand, ShellExecOptions, ShellExecResult,
    TextLineReader, WatchCallback, WatchTarget,
};
use kiana_durable::env::{ShellOutputSkip, ShellOutputWindow};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, NestedToolExecutionResult, NestedToolOptions,
    RegistrySnapshot, RetainedOutput, SettledTask, ToolDiagnostic, ToolExecutionApi, ToolOutput,
};
use kiana_durable::{
    BoxError, CommitOptions, ConversationId, DocTarget, DocumentObserver, DocumentReader,
    DocumentWatch, EntryId, JsonObject, JsonValue, SessionChange, TaskId, TaskOptions, TaskRecord,
    id_from_number,
};
use tokio::sync::Semaphore;

// ---------------------------------------------------------------------------
// 临时目录
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// `createTempDir()`：进程唯一目录，Drop 时递归删除。
pub struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-tools-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
            counter
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// 延迟信号（TS 的 deferred()）
// ---------------------------------------------------------------------------

/// 一个可等待、可解析的闸门。
#[derive(Clone)]
pub struct Deferred {
    semaphore: Arc<Semaphore>,
}

impl Deferred {
    pub fn new() -> Self {
        Deferred {
            semaphore: Arc::new(Semaphore::new(0)),
        }
    }

    pub fn resolve(&self) {
        self.semaphore.add_permits(1);
    }

    pub async fn wait(&self) {
        let permit =
            tokio::time::timeout(std::time::Duration::from_secs(10), self.semaphore.acquire())
                .await
                .expect("timed out waiting for the test gate")
                .expect("the gate is open");
        permit.forget();
    }
}

impl Default for Deferred {
    fn default() -> Self {
        Self::new()
    }
}

/// `delay(ms)`。
pub async fn delay(milliseconds: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(milliseconds)).await;
}

/// `withAbortSignal(controller.signal, BACKGROUND_CONTEXT)`。
pub fn with_signal(signal: AbortSignal) -> ContextRef {
    kiana_chord::context::with_abort_signal(signal, BACKGROUND_CONTEXT.clone())
}

/// `new AbortController()`；返回信号与中止函数。
pub fn abort_controller() -> (AbortSignal, Arc<dyn Fn() + Send + Sync>) {
    let signal = AbortSignal::new();
    let handle = signal.clone();
    (signal, Arc::new(move || handle.abort_without_reason()))
}

// ---------------------------------------------------------------------------
// TestEnv
// ---------------------------------------------------------------------------

pub type ReadHook = Arc<
    dyn for<'a> Fn(
            Arc<NodeExecutionEnv>,
            String,
            &'a dyn Context,
        ) -> Pin<Box<dyn Future<Output = Result<String, FileError>> + Send + 'a>>
        + Send
        + Sync,
>;

pub type WriteHook = Arc<
    dyn for<'a> Fn(
            Arc<NodeExecutionEnv>,
            String,
            FileContent,
            &'a dyn Context,
        ) -> Pin<Box<dyn Future<Output = Result<(), FileError>> + Send + 'a>>
        + Send
        + Sync,
>;

pub type ReaderHook = Arc<
    dyn for<'a> Fn(
            Arc<NodeExecutionEnv>,
            String,
            Option<OpenBinaryReaderOptions>,
            &'a dyn Context,
        ) -> Pin<
            Box<dyn Future<Output = Result<Arc<dyn BinaryReader>, FileError>> + Send + 'a>,
        > + Send
        + Sync,
>;

pub type ExecHook = Arc<
    dyn for<'a> Fn(
            Arc<NodeExecutionEnv>,
            ShellCommand,
            Option<ShellExecOptions>,
            &'a dyn Context,
        ) -> Pin<
            Box<dyn Future<Output = Result<ShellExecResult, ExecutionError>> + Send + 'a>,
        > + Send
        + Sync,
>;

/// 可注入钩子的 [`NodeExecutionEnv`] 包装。
pub struct TestEnv {
    inner: Arc<NodeExecutionEnv>,
    id: String,
    read_hook: Option<ReadHook>,
    write_hook: Option<WriteHook>,
    reader_hook: Option<ReaderHook>,
    exec_hook: Option<ExecHook>,
}

impl TestEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        TestEnv {
            inner: Arc::new(NodeExecutionEnv::new(NodeEnvOptions::new(cwd))),
            id: NodeExecutionEnv::ID.to_owned(),
            read_hook: None,
            write_hook: None,
            reader_hook: None,
            exec_hook: None,
        }
    }

    pub fn with_options(options: NodeEnvOptions) -> Self {
        TestEnv {
            inner: Arc::new(NodeExecutionEnv::new(options)),
            id: NodeExecutionEnv::ID.to_owned(),
            read_hook: None,
            write_hook: None,
            reader_hook: None,
            exec_hook: None,
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_read_hook(mut self, hook: ReadHook) -> Self {
        self.read_hook = Some(hook);
        self
    }

    pub fn with_write_hook(mut self, hook: WriteHook) -> Self {
        self.write_hook = Some(hook);
        self
    }

    pub fn with_reader_hook(mut self, hook: ReaderHook) -> Self {
        self.reader_hook = Some(hook);
        self
    }

    pub fn with_exec_hook(mut self, hook: ExecHook) -> Self {
        self.exec_hook = Some(hook);
        self
    }

    pub fn inner(&self) -> &Arc<NodeExecutionEnv> {
        &self.inner
    }
}

macro_rules! delegate_file_system {
    ($ty:ty) => {
        #[async_trait]
        impl FileSystem for $ty {
            fn id(&self) -> &str {
                &self.id
            }

            fn cwd(&self) -> &str {
                self.inner.cwd()
            }

            fn set_cwd(&self, cwd: String) {
                self.inner.set_cwd(cwd)
            }

            async fn absolute_path(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<String, FileError> {
                self.inner.absolute_path(path, context).await
            }

            async fn join_path(
                &self,
                parts: &[String],
                context: &dyn Context,
            ) -> Result<String, FileError> {
                self.inner.join_path(parts, context).await
            }

            async fn read_text_file(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<String, FileError> {
                match &self.read_hook {
                    Some(hook) => hook(Arc::clone(&self.inner), path.to_owned(), context).await,
                    None => self.inner.read_text_file(path, context).await,
                }
            }

            async fn open_text_line_reader(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<Arc<dyn TextLineReader>, FileError> {
                self.inner.open_text_line_reader(path, context).await
            }

            async fn read_text_lines(
                &self,
                path: &str,
                options: Option<&ReadTextLinesOptions>,
                context: &dyn Context,
            ) -> Result<Vec<String>, FileError> {
                self.inner.read_text_lines(path, options, context).await
            }

            async fn read_binary_file(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<Vec<u8>, FileError> {
                self.inner.read_binary_file(path, context).await
            }

            async fn open_binary_reader(
                &self,
                path: &str,
                options: Option<&OpenBinaryReaderOptions>,
                context: &dyn Context,
            ) -> Result<Arc<dyn BinaryReader>, FileError> {
                match &self.reader_hook {
                    Some(hook) => {
                        hook(
                            Arc::clone(&self.inner),
                            path.to_owned(),
                            options.copied(),
                            context,
                        )
                        .await
                    }
                    None => self.inner.open_binary_reader(path, options, context).await,
                }
            }

            async fn write_file(
                &self,
                path: &str,
                content: FileContent,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                match &self.write_hook {
                    Some(hook) => {
                        hook(Arc::clone(&self.inner), path.to_owned(), content, context).await
                    }
                    None => self.inner.write_file(path, content, context).await,
                }
            }

            async fn append_file(
                &self,
                path: &str,
                content: FileContent,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                self.inner.append_file(path, content, context).await
            }

            async fn truncate_file(
                &self,
                path: &str,
                size: u64,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                self.inner.truncate_file(path, size, context).await
            }

            async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError> {
                self.inner.flush_file(path, context).await
            }

            async fn rename_file(
                &self,
                source_path: &str,
                destination_path: &str,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                self.inner
                    .rename_file(source_path, destination_path, context)
                    .await
            }

            async fn file_info(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<FileInfo, FileError> {
                self.inner.file_info(path, context).await
            }

            async fn list_dir(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<Vec<FileInfo>, FileError> {
                self.inner.list_dir(path, context).await
            }

            async fn list_files(
                &self,
                path: &str,
                options: Option<&ListFilesOptions>,
                context: &dyn Context,
            ) -> Result<FileListing, FileError> {
                self.inner.list_files(path, options, context).await
            }

            async fn open_dir_reader(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<Arc<dyn DirReader>, FileError> {
                self.inner.open_dir_reader(path, context).await
            }

            async fn watch(
                &self,
                targets: &[WatchTarget],
                on_change: WatchCallback,
                context: &dyn Context,
            ) -> Result<Arc<dyn FileWatcher>, FileError> {
                self.inner.watch(targets, on_change, context).await
            }

            async fn canonical_path(
                &self,
                path: &str,
                context: &dyn Context,
            ) -> Result<String, FileError> {
                self.inner.canonical_path(path, context).await
            }

            async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError> {
                self.inner.exists(path, context).await
            }

            async fn create_dir(
                &self,
                path: &str,
                options: Option<&CreateDirOptions>,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                self.inner.create_dir(path, options, context).await
            }

            async fn remove(
                &self,
                path: &str,
                options: Option<&RemoveOptions>,
                context: &dyn Context,
            ) -> Result<(), FileError> {
                self.inner.remove(path, options, context).await
            }

            async fn create_temp_dir(
                &self,
                prefix: Option<&str>,
                context: &dyn Context,
            ) -> Result<String, FileError> {
                self.inner.create_temp_dir(prefix, context).await
            }

            async fn create_temp_file(
                &self,
                options: Option<&CreateTempFileOptions>,
                context: &dyn Context,
            ) -> Result<String, FileError> {
                self.inner.create_temp_file(options, context).await
            }

            async fn cleanup(&self, context: &dyn Context) {
                self.inner.cleanup(context).await
            }
        }
    };
}

delegate_file_system!(TestEnv);

#[async_trait]
impl Shell for TestEnv {
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        match &self.exec_hook {
            Some(hook) => hook(Arc::clone(&self.inner), command, options.cloned(), context).await,
            None => self.inner.exec(command, options, context).await,
        }
    }

    async fn cleanup(&self, context: &dyn Context) {
        Shell::cleanup(self.inner.as_ref(), context).await
    }
}

/// `OpenBinaryReaderOptions` 需要可复制以跨 await 传给钩子。
trait CopyReaderOptions {
    fn copied(&self) -> Option<OpenBinaryReaderOptions>;
}

impl CopyReaderOptions for Option<&OpenBinaryReaderOptions> {
    fn copied(&self) -> Option<OpenBinaryReaderOptions> {
        (*self).copied()
    }
}

// ---------------------------------------------------------------------------
// FakeApi
// ---------------------------------------------------------------------------

/// 只提供 `get_model` 的模型集合：图片相关的用例需要“会话模型”的形状。
pub struct StaticModels {
    pub model: Model,
}

#[async_trait]
impl Models for StaticModels {
    fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        Vec::new()
    }

    fn get_provider(&self, _id: &str) -> Option<Arc<dyn Provider>> {
        None
    }

    fn get_models(&self, _provider: Option<&str>) -> Vec<Model> {
        vec![self.model.clone()]
    }

    fn get_model(&self, _provider: &str, _id: &str) -> Option<Model> {
        Some(self.model.clone())
    }

    fn get_models_of_type(&self, _model_type: ModelType, _provider: Option<&str>) -> Vec<Model> {
        Vec::new()
    }

    fn get_model_of_type(
        &self,
        _model_type: ModelType,
        _provider: &str,
        _id: &str,
    ) -> Option<Model> {
        None
    }

    fn get_all_models(&self, _provider: Option<&str>) -> Vec<AnyModel> {
        Vec::new()
    }

    async fn refresh(
        &self,
        _options: ModelsRefreshOptions,
    ) -> Result<ModelsRefreshResult, AiError> {
        unreachable!("the fake model catalog never refreshes")
    }

    async fn check_auth(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<Option<AuthCheck>, AuthError> {
        unreachable!("the fake model catalog has no auth")
    }

    async fn get_available(
        &self,
        _provider_id: Option<&str>,
        _options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        unreachable!("the fake model catalog has no auth")
    }

    async fn get_available_of_type(
        &self,
        _model_type: ModelType,
        _provider_id: Option<&str>,
        _options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        unreachable!("the fake model catalog has no auth")
    }

    async fn get_all_available(
        &self,
        _provider_id: Option<&str>,
        _options: AuthOperationOptions,
    ) -> Result<Vec<AnyModel>, AuthError> {
        unreachable!("the fake model catalog has no auth")
    }

    async fn get_auth(
        &self,
        _target: GetAuthTarget<'_>,
        _overrides: AuthResolutionOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        unreachable!("the fake model catalog has no auth")
    }

    async fn login(
        &self,
        _provider_id: &str,
        _auth_type: AuthType,
        _interaction: &dyn AuthInteraction,
        _options: LoginOptions,
    ) -> Result<Credential, AuthError> {
        unreachable!("the fake model catalog cannot log in")
    }

    async fn logout(
        &self,
        _provider_id: &str,
        _options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        unreachable!("the fake model catalog cannot log out")
    }

    fn stream(
        &self,
        _model: &Model,
        _context: &AiContext,
        _options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream {
        unreachable!("the fake model catalog never streams")
    }

    async fn complete(
        &self,
        _model: &Model,
        _context: &AiContext,
        _options: ModelsStreamOptions,
    ) -> AssistantMessage {
        unreachable!("the fake model catalog never completes")
    }

    fn stream_simple(
        &self,
        _model: &Model,
        _context: &AiContext,
        _options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        unreachable!("the fake model catalog never streams")
    }

    async fn complete_simple(
        &self,
        _model: &Model,
        _context: &AiContext,
        _options: ModelsSimpleStreamOptions,
    ) -> AssistantMessage {
        unreachable!("the fake model catalog never completes")
    }

    fn stream_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: ModelsDeferredFetchOptions,
    ) -> AssistantMessageEventStream {
        unreachable!("the fake model catalog never streams")
    }

    async fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: ModelsDeferredFetchOptions,
    ) -> AssistantMessage {
        unreachable!("the fake model catalog never fetches")
    }

    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: ModelsDeferredCancelOptions,
    ) -> Result<(), AiError> {
        unreachable!("the fake model catalog never cancels")
    }

    async fn generate_images(
        &self,
        _model: &Model,
        _context: &ImagesContext,
        _options: ModelsImagesOptions,
    ) -> AssistantImages {
        unreachable!("the fake model catalog never generates images")
    }

    async fn classify(
        &self,
        _model: &Model,
        _context: &ClassifierContext,
        _options: ModelsClassifierOptions,
    ) -> ClassifierResult {
        unreachable!("the fake model catalog never classifies")
    }
}

/// `fakeApi(env)`：只实现编码工具用到的部分。
pub struct FakeApi {
    env: Option<Arc<dyn ExecutionEnv>>,
    chunks: Mutex<Vec<(String, Option<ShellOutputSkip>)>>,
    diagnostics: Mutex<Vec<ToolDiagnostic>>,
    window: Option<ShellOutputWindow>,
    model: Option<Model>,
}

impl FakeApi {
    pub fn new(env: Option<Arc<dyn ExecutionEnv>>) -> Arc<Self> {
        Arc::new(FakeApi {
            env,
            chunks: Mutex::new(Vec::new()),
            diagnostics: Mutex::new(Vec::new()),
            window: None,
            model: None,
        })
    }

    pub fn with_window(env: Option<Arc<dyn ExecutionEnv>>, window: ShellOutputWindow) -> Arc<Self> {
        Arc::new(FakeApi {
            env,
            chunks: Mutex::new(Vec::new()),
            diagnostics: Mutex::new(Vec::new()),
            window: Some(window),
            model: None,
        })
    }

    /// 会话有一个（假的）模型：`read` 用它决定是否支持图片与图片限额。
    pub fn with_model(env: Option<Arc<dyn ExecutionEnv>>, model: Model) -> Arc<Self> {
        Arc::new(FakeApi {
            env,
            chunks: Mutex::new(Vec::new()),
            diagnostics: Mutex::new(Vec::new()),
            window: None,
            model: Some(model),
        })
    }

    /// `output.join("")`。
    pub fn streamed(&self) -> String {
        self.chunks
            .lock()
            .unwrap()
            .iter()
            .map(|(chunk, _)| chunk.as_str())
            .collect()
    }

    /// 记录下的每个块与它之前的省略量。
    pub fn chunks(&self) -> Vec<(String, Option<ShellOutputSkip>)> {
        self.chunks.lock().unwrap().clone()
    }

    pub fn reported(&self) -> Vec<ToolDiagnostic> {
        self.diagnostics.lock().unwrap().clone()
    }
}

#[async_trait]
impl DocumentReader for FakeApi {
    async fn snapshot(
        &self,
        _target: &DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        unreachable!("coding tools do not read documents")
    }

    async fn snapshot_as_of(
        &self,
        _target: &DocTarget,
        _at: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        unreachable!("coding tools do not read documents")
    }
}

#[async_trait]
impl DocumentObserver for FakeApi {
    async fn watch_doc(
        &self,
        _target: &DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        unreachable!("coding tools do not watch documents")
    }
}

#[async_trait]
impl ToolExecutionApi for FakeApi {
    fn task_id(&self) -> TaskId {
        id_from_number(1)
    }

    fn conversation_id(&self) -> ConversationId {
        id_from_number(1)
    }

    fn call_id(&self) -> &str {
        "call"
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        unreachable!("coding tools do not read the registry")
    }

    async fn agent(&self, _context: &dyn Context) -> Result<Arc<Agent>, BoxError> {
        // 没有模型：`read` 视作接受图片的模型。
        Ok(Arc::new(Agent {
            model: self
                .model
                .as_ref()
                .map(|model| kiana_durable::harness::types::ModelRef {
                    provider: model.provider.clone(),
                    model_id: model.id.clone(),
                }),
            thinking_level: Default::default(),
            extensions: Vec::new(),
            tools: Vec::new(),
            callable: Vec::new(),
            deferred: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        }))
    }

    fn models(&self) -> Arc<dyn kiana_ai::models::Models> {
        match &self.model {
            Some(model) => Arc::new(StaticModels {
                model: model.clone(),
            }),
            None => unreachable!("the fake agent has no model"),
        }
    }

    fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.env.clone()
    }

    fn now(&self) -> i64 {
        0
    }

    fn output(&self, chunk: ToolOutput, skipped: Option<ShellOutputSkip>) {
        let text = match chunk {
            ToolOutput::Text(text) => text,
            ToolOutput::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        };
        self.chunks.lock().unwrap().push((text, skipped));
    }

    fn output_window(&self) -> Option<ShellOutputWindow> {
        self.window
    }

    fn retained_output(&self) -> RetainedOutput {
        RetainedOutput {
            text: self.streamed(),
            truncated: false,
        }
    }

    fn diagnostic(&self, diagnostic: ToolDiagnostic) {
        self.diagnostics.lock().unwrap().push(diagnostic);
    }

    async fn details(&self, _value: JsonValue, _context: &dyn Context) -> Result<(), BoxError> {
        Ok(())
    }

    async fn commit(
        &self,
        _change: &dyn SessionChange,
        _context: &dyn Context,
        _options: CommitOptions,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        unreachable!("coding tools do not commit")
    }

    async fn memo(
        &self,
        _name: &str,
        _context: &dyn Context,
    ) -> Result<Option<JsonValue>, BoxError> {
        unreachable!("coding tools do not use memos")
    }

    async fn memo_put(
        &self,
        _name: &str,
        _candidate: JsonValue,
        _context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        unreachable!("coding tools do not use memos")
    }

    async fn create_task(
        &self,
        _task: &AnyTask,
        _input: JsonValue,
        _options: TaskOptions,
        _context: &dyn Context,
    ) -> Result<TaskId, BoxError> {
        unreachable!("coding tools do not create tasks")
    }

    async fn get_task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        unreachable!("coding tools do not read tasks")
    }

    async fn wait_for_task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        unreachable!("coding tools do not wait for tasks")
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError> {
        unreachable!("coding tools do not open conversations")
    }

    fn execute_tool(
        self: Arc<Self>,
        _name: &str,
        _args: JsonObject,
        _context: &dyn Context,
        _options: NestedToolOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<NestedToolExecutionResult, BoxError>>
                + Send
                + 'static,
        >,
    > {
        unreachable!("coding tools do not nest tools")
    }
}

// ---------------------------------------------------------------------------
// 运行器
// ---------------------------------------------------------------------------

/// 调用一次工具，返回结果与收集通道。
pub async fn run(
    tool: &kiana_durable::harness::types::ToolRegistration,
    args: JsonValue,
    env: Option<Arc<dyn ExecutionEnv>>,
    context: ContextRef,
) -> (
    Result<kiana_durable::harness::types::ToolExecutionResult, BoxError>,
    Arc<FakeApi>,
) {
    let api = FakeApi::new(env);
    let execute = tool.execute.as_ref().expect("the tool has an execute");
    let result = execute(args, api.clone(), context).await;
    (result, api)
}

/// `textOutput(result)`。
pub fn text_output(result: &kiana_durable::harness::types::ToolExecutionResult) -> String {
    result
        .output
        .as_ref()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| match part {
                    kiana_ai::UserContentBlock::Text(text) => Some(text.text.as_str()),
                    kiana_ai::UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// `diagnosticText(result)`。
pub fn diagnostic_text(result: &kiana_durable::harness::types::ToolExecutionResult) -> String {
    result
        .diagnostics
        .as_ref()
        .map(|diagnostics| {
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 一个记录写入门控状态的测试替身。
pub struct BlockingWriteState {
    pub first_write_started: Deferred,
    pub finish_first_write: Deferred,
    pub second_write_started: AtomicBool,
}

impl Default for BlockingWriteState {
    fn default() -> Self {
        BlockingWriteState {
            first_write_started: Deferred::new(),
            finish_first_write: Deferred::new(),
            second_write_started: AtomicBool::new(false),
        }
    }
}

/// 一个记录编辑写入门控状态的测试替身。
pub struct BlockingEditState {
    pub first_edit_write_started: Deferred,
    pub finish_first_edit_write: Deferred,
    pub first_edit_write_settled: AtomicBool,
    pub second_edit_write_started: AtomicBool,
}

impl Default for BlockingEditState {
    fn default() -> Self {
        BlockingEditState {
            first_edit_write_started: Deferred::new(),
            finish_first_edit_write: Deferred::new(),
            first_edit_write_settled: AtomicBool::new(false),
            second_edit_write_started: AtomicBool::new(false),
        }
    }
}

/// `new ExecutionError("timeout", message)`。
pub fn execution_error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message)
}

/// 一个会追加内容的二进制读取器：每次 `read` 先往文件尾写 `"more\n"`。
pub struct GrowingReader {
    inner: Arc<dyn BinaryReader>,
    log_path: String,
}

impl GrowingReader {
    pub fn new(inner: Arc<dyn BinaryReader>, log_path: String) -> Self {
        GrowingReader { inner, log_path }
    }
}

#[async_trait]
impl BinaryReader for GrowingReader {
    async fn info(&self, context: &dyn Context) -> Result<FileInfo, FileError> {
        self.inner.info(context).await
    }

    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.log_path)
            .map_err(|error| FileError::new(FileErrorCode::Unknown, error.to_string()))?;
        file.write_all(b"more\n")
            .map_err(|error| FileError::new(FileErrorCode::Unknown, error.to_string()))?;
        self.inner.read(offset, length, context).await
    }

    async fn scan_lines(
        &self,
        range: LineRange,
        context: &dyn Context,
    ) -> Result<LineScan, FileError> {
        self.inner.scan_lines(range, context).await
    }

    async fn close(&self, context: &dyn Context) {
        self.inner.close(context).await
    }
}
