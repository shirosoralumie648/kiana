//! `test/codemode-models.test.ts` 的移植：code mode 的 `models.*`。
//!
//! 目录函数不泄露配置的请求头；`classify()`/`generateImages()` 通过内部工具跑成嵌套调用，
//! 带自己的用量与失败结果；同时最多四个模型调用；脚本没展示的生成图片会在结果里被点名。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::models::{
    ClassifierOptions, CreateProviderOptions, ImagesOptions, ProviderClassifier, ProviderImages,
    create_provider,
};
use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::{
    AnyModel, AssistantContentBlock, AssistantMessage, AssistantRole, ClassifierAnswer,
    ClassifierContext, ClassifierResult, ClassifierStopReason, ImageContent, ImageKind,
    ImagesContext, ImagesStopReason, JsonObject, Model, ModelCost, ModelInputModality, ModelType,
    StopReason, TextContent, TextKind, Usage, UsageCost, UserContentBlock,
};
use kiana_durable::Session as _;
use kiana_durable::codemode::codemode::CodemodeToolOptions;
use kiana_durable::harness::types::Registry as _;
use kiana_durable::harness::usage::USAGE_DOC;
use kiana_durable::types::{DocTarget, DocumentReader as _, JsonValue, Storage};
use serde_json::json;

use chat_support::{ChatSetup, context, open_chat};
use harness_support::text_input;

#[path = "support/harness.rs"]
mod harness_support;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 同一进程里并行跑多个 QuickJS/wasmtime sandbox 会偶发挂起甚至 SIGSEGV（kiana-codemode 的
/// 跨单元问题，见交付说明）；TS 侧同一个 vitest 文件里的测试默认串行，这里用一把进程级锁
/// 复刻同样的串行性，测试覆盖的内容不变。
static TEST_GATE: Mutex<()> = Mutex::new(());

fn test_gate() -> std::sync::MutexGuard<'static, ()> {
    lock(&TEST_GATE)
}

fn done() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "done".to_owned(),
            text_signature: None,
        })],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 3,
        ..Default::default()
    }
}

fn script(code: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(
            kiana_ai::providers::faux::faux_tool_call_with(
                "codemode",
                {
                    let mut arguments = JsonObject::new();
                    arguments.insert("code".to_owned(), JsonValue::String(code.to_owned()));
                    arguments
                },
                kiana_ai::providers::faux::FauxToolCallOptions {
                    id: Some("s1".to_owned()),
                },
            ),
        )],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

fn usage() -> Usage {
    Usage {
        input: 10,
        output: 2,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 12,
        cost: UsageCost {
            input: 0.001,
            output: 0.0002,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0012,
            total_from_provider: None,
        },
        ..Default::default()
    }
}

/// `apiKey: { name: "Test", resolve: async () => ({ auth: {} }) }`。
struct TestAuth;

#[async_trait]
impl ApiKeyAuth for TestAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult::new(ModelAuth::default())))
    }
}

#[derive(Default)]
struct Calls {
    classify: AtomicUsize,
    images: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

/// 可开合的闸门；`at_four` 时第四个进行中的调用在 50ms 后开闸。
struct Gate {
    open: Mutex<tokio::sync::watch::Sender<bool>>,
    at_four: AtomicBool,
}

impl Gate {
    fn new() -> Self {
        Gate {
            open: Mutex::new(tokio::sync::watch::channel(true).0),
            at_four: AtomicBool::new(false),
        }
    }

    fn close(&self) {
        lock(&self.open).send_replace(false);
    }

    fn release(&self) {
        lock(&self.open).send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = lock(&self.open).subscribe();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

struct TestClassifier {
    calls: Arc<Calls>,
    gate: Arc<Gate>,
}

#[async_trait]
impl ProviderClassifier for TestClassifier {
    async fn classify(
        &self,
        model: &Model,
        classifier_context: &ClassifierContext,
        _options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        self.calls.classify.fetch_add(1, Ordering::SeqCst);
        if classifier_context.state.get("fail") == Some(&json!(true)) {
            return ClassifierResult {
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                answers: IndexMap::new(),
                usage: None,
                stop_reason: ClassifierStopReason::Error,
                error_message: Some("quota exceeded".to_owned()),
                timestamp: 0,
            };
        }
        let in_flight = self.calls.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.calls
            .max_in_flight
            .fetch_max(in_flight, Ordering::SeqCst);
        if self.gate.at_four.load(Ordering::SeqCst) && in_flight == 4 {
            let gate = Arc::clone(&self.gate);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                gate.release();
            });
        }
        self.gate.wait().await;
        self.calls.in_flight.fetch_sub(1, Ordering::SeqCst);
        let mut answers = IndexMap::new();
        answers.insert(
            "good".to_owned(),
            ClassifierAnswer::Bool { probability: 1.0 },
        );
        ClassifierResult {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            answers,
            usage: Some(usage()),
            stop_reason: ClassifierStopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }
}

struct TestImages {
    calls: Arc<Calls>,
}

#[async_trait]
impl ProviderImages for TestImages {
    async fn generate_images(
        &self,
        model: &Model,
        _context: &ImagesContext,
        _options: Option<&ImagesOptions>,
    ) -> kiana_ai::types::AssistantImages {
        self.calls.images.fetch_add(1, Ordering::SeqCst);
        kiana_ai::types::AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: vec![UserContentBlock::Image(ImageContent {
                kind_image: ImageKind::Image,
                data: PNG.to_owned(),
                mime_type: "image/png".to_owned(),
            })],
            stop_reason: ImagesStopReason::Stop,
            timestamp: 0,
            ..Default::default()
        }
    }
}

fn model(
    id: &str,
    name: &str,
    api: &str,
    model_type: ModelType,
    context_window: Option<u64>,
    output: Option<Vec<String>>,
) -> Model {
    Model {
        id: id.to_owned(),
        name: name.to_owned(),
        api: api.to_owned(),
        provider: "test".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        headers: Some(IndexMap::from([(
            "authorization".to_owned(),
            "secret".to_owned(),
        )])),
        model_type: Some(model_type),
        context_window,
        output,
        ..Default::default()
    }
}

struct ModelsChat {
    setup: ChatSetup,
    calls: Arc<Calls>,
    gate: Arc<Gate>,
}

/// 模型目录里还有一个测试 provider 的 classifier 与 image 模型，加上 code mode 扩展。
fn models_chat() -> ModelsChat {
    let setup = ChatSetup::new();
    let calls = Arc::new(Calls::default());
    let gate = Arc::new(Gate::new());
    let provider = create_provider(CreateProviderOptions {
        id: "test".to_owned(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(TestAuth)),
            oauth: None,
        },
        models: vec![
            AnyModel::Classifier(model(
                "judge",
                "Judge",
                "test-classifier",
                ModelType::Classifier,
                Some(1000),
                None,
            )),
            AnyModel::Image(model(
                "painter",
                "Painter",
                "test-images",
                ModelType::Image,
                None,
                Some(vec!["image".to_owned()]),
            )),
        ],
        classifiers: Some(IndexMap::from([(
            "test-classifier".to_owned(),
            Arc::new(TestClassifier {
                calls: Arc::clone(&calls),
                gate: Arc::clone(&gate),
            }) as Arc<dyn ProviderClassifier>,
        )])),
        images: Some(IndexMap::from([(
            "test-images".to_owned(),
            Arc::new(TestImages {
                calls: Arc::clone(&calls),
            }) as Arc<dyn ProviderImages>,
        )])),
        ..Default::default()
    })
    .expect("the test provider");
    setup.models.set_provider(provider);
    let inner =
        kiana_codemode::CodemodeSandbox::new(kiana_codemode::types::CodemodeSandboxOptions {
            base: kiana_codemode::types::CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(64.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("the sandbox");
    let sandbox: Arc<dyn kiana_durable::codemode::codemode::CodemodeSandbox> = Arc::new(inner);
    setup.registry.install(
        kiana_durable::codemode::codemode::create_codemode_extension(CodemodeToolOptions::new(
            sandbox,
        )),
    );
    ModelsChat { setup, calls, gate }
}

struct RunOutput {
    text: String,
    message: kiana_ai::types::ToolResultMessage,
    usage: Option<JsonObject>,
}

fn content_text(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => String::new(),
        })
        .collect()
}

fn strip_header(text: &str) -> String {
    let rest = text
        .strip_prefix("Script completed\n")
        .or_else(|| text.strip_prefix("Script failed\n"));
    let Some(rest) = rest else {
        return text.to_owned();
    };
    let Some(rest) = rest.strip_prefix("Wall time ") else {
        return text.to_owned();
    };
    let Some((_, rest)) = rest.split_once(" seconds\nOutput:\n") else {
        return text.to_owned();
    };
    rest.to_owned()
}

async fn run(chat: &ModelsChat, code: &str) -> RunOutput {
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(code)),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) = open_chat(memory(), &chat.setup).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    let entries = chat_support::all_entries(&root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("a tool result");
    let message = match entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message")
    {
        kiana_ai::types::Message::ToolResult(result) => result.clone(),
        _ => panic!("a tool result message"),
    };
    let text = strip_header(&content_text(&message.content));
    let usage = harness
        .snapshot(&DocTarget::conversation(&USAGE_DOC, root.id()), &*context())
        .await
        .expect("the usage snapshot");
    harness.close(&*context()).await;
    RunOutput {
        text,
        message,
        usage,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_the_model_catalog_without_credentials_in_headers() {
    let _guard = test_gate();
    let chat = models_chat();
    let output = run(
        &chat,
        r#"const classifiers = await models.getModelsOfType("classifier", "test");
        const one = await models.getModelOfType("classifier", "test", "judge");
        const available = await models.getAvailableOfType("image");
        return [classifiers.map((m) => m.id), one.id, "headers" in one, available.map((m) => m.id)];"#,
    )
    .await;
    let value: JsonValue = serde_json::from_str(&output.text).expect("the returned array");
    assert_eq!(value, json!([["judge"], "judge", false, ["painter"]]));
}

#[tokio::test(flavor = "multi_thread")]
async fn classifies_through_an_internal_tool_a_nested_call_with_its_usage_hidden_from_tools_and_listings()
 {
    let _guard = test_gate();
    let chat = models_chat();
    let output = run(
        &chat,
        r#"const model = await models.getModelOfType("classifier", "test", "judge");
        const result = await models.classify(model, {
            state: { text: "hi" },
            questions: { good: { type: "bool", instructions: "Is it good?", criteria: { true: "yes", false: "no" } } },
        });
        const found = (await searchTools("classify generate images models")).length;
        const described = (await describeTool("kiana.codemode.models.classify")) !== undefined;
        return [result.stopReason, result.answers.good.probability === 1, Object.keys(tools).some((name) => name.includes("models")), ALL_TOOLS.some((tool) => tool.name.includes("models")), found, described];"#,
    )
    .await;
    let value: JsonValue = serde_json::from_str(&output.text).expect("the returned array");
    // TS 的假答案是 `{ good: { value: true } }`；Rust 的 ClassifierAnswer 是强类型联合，
    // 这里返回 bool 答案，脚本读它的 `probability`，验证的是同一件事：完整结果到达脚本。
    assert_eq!(value, json!(["stop", true, false, false, 0, false]));
    assert_eq!(chat.calls.classify.load(Ordering::SeqCst), 1);
    let details = output.message.details.clone().expect("details");
    assert_eq!(details["calls"][0]["name"], json!("models.classify"));
    assert_eq!(details["calls"][0]["status"], json!("ok"));
    // Harness 把它记在内部工具名下。
    let total = output
        .usage
        .as_ref()
        .and_then(|usage| usage.get("tools"))
        .and_then(|tools| tools.get("kiana.codemode.models.classify"))
        .and_then(|bucket| bucket.get("cost"))
        .and_then(|cost| cost.get("total"))
        .and_then(JsonValue::as_f64)
        .expect("the recorded usage");
    assert!((total - 0.0012).abs() < 1e-9, "{total}");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolves_a_model_call_that_failed_with_its_result_as_the_coding_agent_does() {
    let _guard = test_gate();
    let chat = models_chat();
    let output = run(
        &chat,
        r#"const model = await models.getModelOfType("classifier", "test", "judge");
        const result = await models.classify(model, {
            state: { fail: true },
            questions: { good: { type: "bool", instructions: "?", criteria: { true: "y", false: "n" } } },
        });
        return [result.stopReason, result.errorMessage];"#,
    )
    .await;
    let value: JsonValue = serde_json::from_str(&output.text).expect("the returned array");
    assert_eq!(value, json!(["error", "quota exceeded"]));
    let details = output.message.details.clone().expect("details");
    assert_eq!(details["calls"][0]["name"], json!("models.classify"));
    assert_eq!(details["calls"][0]["status"], json!("error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_bad_classifier_context_with_the_expected_shape_before_any_request() {
    let _guard = test_gate();
    let chat = models_chat();
    let output = run(
        &chat,
        r#"const model = await models.getModelOfType("classifier", "test", "judge");
        try { await models.classify(model, { prompt: "x" }); } catch (error) { return error.message; }"#,
    )
    .await;
    assert!(
        output.text.contains("context.state must be an object"),
        "{}",
        output.text
    );
    assert!(output.text.contains("Expected context:"), "{}", output.text);
    assert_eq!(chat.calls.classify.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_at_most_four_model_calls_at_once() {
    let _guard = test_gate();
    let chat = models_chat();
    chat.gate.close();
    chat.gate.at_four.store(true, Ordering::SeqCst);
    let output = run(
        &chat,
        r#"const model = await models.getModelOfType("classifier", "test", "judge");
        const context = { state: {}, questions: { good: { type: "bool", instructions: "?", criteria: { true: "y", false: "n" } } } };
        return (await Promise.all(Array.from({ length: 8 }, () => models.classify(model, context)))).length;"#,
    )
    .await;
    assert_eq!(output.text, "8");
    assert_eq!(chat.calls.classify.load(Ordering::SeqCst), 8);
    assert_eq!(chat.calls.max_in_flight.load(Ordering::SeqCst), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_generated_images_the_script_did_not_show() {
    let _guard = test_gate();
    let chat = models_chat();
    let hidden = run(
        &chat,
        r#"const model = await models.getModelOfType("image", "test", "painter");
        const result = await models.generateImages(model, { input: [{ type: "text", text: "a cat" }] });
        return result.output.length;"#,
    )
    .await;
    assert!(
        hidden.text.contains(
            "Note: models.generateImages() returned 1 image that the script did not show"
        ),
        "{}",
        hidden.text
    );
    let chat = models_chat();
    let shown = run(
        &chat,
        r#"const model = await models.getModelOfType("image", "test", "painter");
        const result = await models.generateImages(model, { input: [{ type: "text", text: "a cat" }] });
        image(result.output[0]);"#,
    )
    .await;
    assert!(!shown.text.contains("Note:"), "{}", shown.text);
    assert!(
        shown
            .message
            .content
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_))),
        "{:?}",
        shown.message.content
    );
    assert_eq!(chat.calls.images.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 脚本结束后才等到槽位的模型调用不再开始（TS codemode-models.test.ts:222）
// ---------------------------------------------------------------------------

use chat_support::{OpenChatOptions, open_chat_with};
use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirReader, ExecutionEnv, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileListing, FileWatcher,
    ListFilesOptions, OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions, ShellCommand,
    ShellExecOptions, ShellExecResult, TextLineReader, WatchCallback, WatchTarget,
};
use kiana_durable::harness::types::{EnvBuilder, EnvTarget};

/// 一个在 `createTempDir` 里开闸并变慢的环境（TS 的 `SlowTempFiles`）。
struct SlowTempEnv {
    gate: Arc<Gate>,
    dir: String,
    files: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
}

fn unsupported() -> FileError {
    FileError::new(FileErrorCode::NotSupported, "not supported")
}

#[async_trait]
impl kiana_durable::env::FileSystem for SlowTempEnv {
    fn id(&self) -> &str {
        "test-slow-temp"
    }

    fn cwd(&self) -> &str {
        "/"
    }

    fn set_cwd(&self, _cwd: String) {}

    async fn absolute_path(
        &self,
        path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        Ok(path.to_owned())
    }

    async fn join_path(
        &self,
        parts: &[String],
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        Ok(parts.join("/"))
    }

    async fn read_text_file(
        &self,
        path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        lock(&self.files)
            .get(path)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .ok_or_else(|| FileError::new(FileErrorCode::NotFound, format!("no file {path}")))
    }

    async fn open_text_line_reader(
        &self,
        _path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        Err(unsupported())
    }

    async fn read_text_lines(
        &self,
        _path: &str,
        _options: Option<&ReadTextLinesOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Vec<String>, FileError> {
        Err(unsupported())
    }

    async fn read_binary_file(
        &self,
        path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Vec<u8>, FileError> {
        lock(&self.files)
            .get(path)
            .cloned()
            .ok_or_else(|| FileError::new(FileErrorCode::NotFound, format!("no file {path}")))
    }

    async fn open_binary_reader(
        &self,
        _path: &str,
        _options: Option<&OpenBinaryReaderOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        Err(unsupported())
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        lock(&self.files).insert(path.to_owned(), bytes);
        Ok(())
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        lock(&self.files)
            .entry(path.to_owned())
            .or_default()
            .extend(bytes);
        Ok(())
    }

    async fn truncate_file(
        &self,
        _path: &str,
        _size: u64,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        Err(unsupported())
    }

    async fn flush_file(
        &self,
        _path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        Ok(())
    }

    async fn rename_file(
        &self,
        _source_path: &str,
        _destination_path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        Err(unsupported())
    }

    async fn file_info(
        &self,
        _path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<FileInfo, FileError> {
        Err(unsupported())
    }

    async fn list_dir(
        &self,
        _path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        Err(unsupported())
    }

    async fn list_files(
        &self,
        _path: &str,
        _options: Option<&ListFilesOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<FileListing, FileError> {
        Err(unsupported())
    }

    async fn open_dir_reader(
        &self,
        _path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        Err(unsupported())
    }

    async fn watch(
        &self,
        _targets: &[WatchTarget],
        _on_change: WatchCallback,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        Err(unsupported())
    }

    async fn canonical_path(
        &self,
        path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        Ok(path.to_owned())
    }

    async fn exists(
        &self,
        path: &str,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<bool, FileError> {
        Ok(lock(&self.files).contains_key(path))
    }

    async fn create_dir(
        &self,
        _path: &str,
        _options: Option<&CreateDirOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        Ok(())
    }

    async fn remove(
        &self,
        path: &str,
        _options: Option<&RemoveOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<(), FileError> {
        lock(&self.files).remove(path);
        Ok(())
    }

    async fn create_temp_dir(
        &self,
        _prefix: Option<&str>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        // 结果落盘发生在脚本结束之后：这时开闸，让等在槽位上的调用看到脚本已经结束。
        self.gate.release();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(self.dir.clone())
    }

    async fn create_temp_file(
        &self,
        _options: Option<&CreateTempFileOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<String, FileError> {
        Err(unsupported())
    }

    async fn cleanup(&self, _context: &dyn kiana_chord::context::Context) {}
}

#[async_trait]
impl kiana_durable::env::Shell for SlowTempEnv {
    async fn exec(
        &self,
        _command: ShellCommand,
        _options: Option<&ShellExecOptions>,
        _context: &dyn kiana_chord::context::Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            "no shell",
        ))
    }

    async fn cleanup(&self, _context: &dyn kiana_chord::context::Context) {}
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_start_a_model_call_that_waited_for_a_slot_after_the_script_ended() {
    let _guard = test_gate();
    let chat = models_chat();
    chat.gate.close();
    let env: EnvBuilder = {
        let gate = Arc::clone(&chat.gate);
        Arc::new(move |_target: EnvTarget, _context| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                let env: Arc<dyn ExecutionEnv> = Arc::new(SlowTempEnv {
                    gate,
                    dir: "/kiana-codemode-models".to_owned(),
                    files: Mutex::new(std::collections::BTreeMap::new()),
                });
                Ok(Some(env))
            })
        })
    };
    chat.setup.faux.set_responses(vec![
        FauxResponseStep::Message(script(
            r#"const model = await models.getModelOfType("classifier", "test", "judge");
            const context = { state: {}, questions: { good: { type: "bool", instructions: "?", criteria: { true: "y", false: "n" } } } };
            for (let i = 0; i < 6; i++) models.classify(model, context).catch(() => {});
            // A journaled call after them: by its result, the first four have been launched.
            await models.getModelsOfType("image");
            return "x".repeat(60000);"#,
        )),
        FauxResponseStep::Message(done()),
    ]);
    let (harness, root) =
        open_chat_with(memory(), &chat.setup, OpenChatOptions { env: Some(env) }).await;
    let submission = root
        .submit(text_input("go").into_draft(), &*context())
        .await
        .expect("the submission");
    submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    let entries = chat_support::all_entries(&root, &*context()).await;
    let entry = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("a tool result");
    let message = match entry
        .model
        .as_ref()
        .and_then(|model| model.first())
        .expect("the message")
    {
        kiana_ai::types::Message::ToolResult(result) => result,
        _ => panic!("a tool result message"),
    };
    let text = strip_header(&content_text(&message.content));
    assert!(text.contains("[Full output: "), "{text}");
    assert_eq!(chat.calls.classify.load(Ordering::SeqCst), 4);
    harness.close(&*context()).await;
}
