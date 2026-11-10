//! `test/harness-host-inputs.test.ts` 的 Rust 端口：`models` 与 `settings` 可以是函数，
//! 每次请求/每次访问都读当前返回值。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxResponseInput, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_provider_with,
    faux_text, faux_tool_call,
};
use kiana_ai::types::{AssistantContentBlock, JsonObject, StopReason};
use kiana_chord::context::Context;
use kiana_durable::Session;
use kiana_durable::harness::harness::{Harness, HarnessImpl};
use kiana_durable::harness::registry::{RegistryImpl, create_registry_impl};
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationStreamOptions, HarnessOptions,
    HarnessSettings, ModelRef, ModelsProvider, Registry, RegistryReader, RootOptions,
    SettingsProvider, ToolExecuteFn, ToolExecutionResult,
};
use kiana_durable::types::{BoxError, JsonValue, SubmissionStatus};

use chat_support::{all_entries, context, text_of};
use harness_support::{TestExtension, text_input};

fn memory() -> Arc<dyn kiana_durable::types::Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new())
        as Arc<dyn kiana_durable::types::Storage>
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一个只有 faux provider、对每个请求回答 `text` 的 `Models`。
fn answering(text: &str) -> Arc<dyn Models> {
    let faux = faux_provider();
    faux.set_responses(
        (0..10)
            .map(|_| {
                FauxResponseStep::Message(faux_assistant_message(
                    faux_text(text.to_owned()),
                    Default::default(),
                ))
            })
            .collect(),
    );
    let models = create_models(Default::default());
    models.set_provider(Arc::clone(&faux.provider));
    let models: Arc<dyn Models> = models;
    models
}

async fn open(
    models: ModelsProvider,
    settings: Option<SettingsProvider>,
    registry: Arc<RegistryImpl>,
) -> (Arc<HarnessImpl>, Arc<dyn ConversationTrait>) {
    let harness = Harness::open(
        memory(),
        HarnessOptions {
            telemetry: None,
            models,
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        &*context(),
    )
    .await
    .expect("the harness opens");
    let root = harness
        .root(
            &*context(),
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await
        .expect("the root");
    (harness, root)
}

async fn turn(root: &Arc<dyn ConversationTrait>, content: &str) -> Option<String> {
    let submission = root
        .submit(text_input(content).into_draft(), &*context())
        .await
        .expect("the submission");
    let settled = submission
        .wait(&*context())
        .await
        .expect("the submission settles");
    assert_eq!(settled.status, SubmissionStatus::Done);
    let entries = all_entries(root, &*context()).await;
    text_of(
        entries
            .last()
            .and_then(|entry| entry.model.as_ref())
            .and_then(|messages| messages.first()),
    )
}

fn tool_use_message(name: &str) -> kiana_ai::types::AssistantMessage {
    faux_assistant_message(
        FauxContent::Blocks(vec![AssistantContentBlock::ToolCall(faux_tool_call(
            name,
            JsonObject::new(),
        ))]),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    )
}

fn plain(text: &str) -> kiana_ai::types::AssistantMessage {
    faux_assistant_message(faux_text(text.to_owned()), Default::default())
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_uses_the_current_return_value_of_a_models_function_at_each_request() {
    let current: Arc<Mutex<Arc<dyn Models>>> = Arc::new(Mutex::new(answering("first")));
    let reader = Arc::clone(&current);
    let (harness, root) = open(
        ModelsProvider::Dynamic(Arc::new(move || lock(&reader).clone())),
        None,
        create_registry_impl(),
    )
    .await;
    assert_eq!(turn(&root, "one").await.as_deref(), Some("first"));
    *lock(&current) = answering("second");
    assert_eq!(turn(&root, "two").await.as_deref(), Some("second"));
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn still_accepts_a_plain_models_object() {
    let (harness, root) = open(
        ModelsProvider::Direct(answering("plain")),
        None,
        create_registry_impl(),
    )
    .await;
    assert_eq!(turn(&root, "one").await.as_deref(), Some("plain"));
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_read_the_current_models_at_each_access_of_api_models() {
    let first = create_models(Default::default());
    let second = create_models(Default::default());
    let faux = faux_provider();
    faux.set_responses(vec![
        FauxResponseStep::Message(tool_use_message("probe")),
        FauxResponseStep::Message(plain("done")),
    ]);
    first.set_provider(Arc::clone(&faux.provider));
    second.set_provider(Arc::clone(&faux.provider));
    second.set_provider(
        faux_provider_with(RegisterFauxProviderOptions {
            provider: Some("second-only".to_owned()),
            ..Default::default()
        })
        .provider,
    );
    let current: Arc<Mutex<Arc<dyn Models>>> =
        Arc::new(Mutex::new(Arc::clone(&first) as Arc<dyn Models>));
    let seen: Arc<Mutex<Vec<Arc<dyn Models>>>> = Arc::new(Mutex::new(Vec::new()));
    let registry = create_registry_impl();
    let mut probe = harness_support::tool("probe");
    probe.tool.description = "Records api.models before and after the host swaps it".to_owned();
    probe.tool.parameters = serde_json::json!({"type": "object"});
    probe.execute = Some(Arc::new({
        let seen = Arc::clone(&seen);
        let current = Arc::clone(&current);
        let second = Arc::clone(&second) as Arc<dyn Models>;
        move |_arguments: JsonValue,
              api: Arc<dyn kiana_durable::harness::types::ToolExecutionApi>,
              _context: Arc<dyn Context>| {
            let seen = Arc::clone(&seen);
            let current = Arc::clone(&current);
            let second = Arc::clone(&second);
            Box::pin(async move {
                lock(&seen).push(api.models());
                *lock(&current) = second;
                lock(&seen).push(api.models());
                Ok(ToolExecutionResult {
                    output: Some(Vec::new()),
                    ..Default::default()
                })
            })
                as Pin<
                    Box<
                        dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>>
                            + Send,
                    >,
                >
        }
    }) as ToolExecuteFn);
    registry.install(kiana_durable::harness::define::define_extension(
        TestExtension::new("probe").with_tools(vec![probe]),
    ));
    let reader = Arc::clone(&current);
    let (harness, root) = open(
        ModelsProvider::Dynamic(Arc::new(move || lock(&reader).clone())),
        None,
        registry,
    )
    .await;
    assert_eq!(turn(&root, "go").await.as_deref(), Some("done"));
    let seen = lock(&seen);
    assert!(seen[0].get_provider("second-only").is_none());
    assert!(seen[1].get_provider("second-only").is_some());
    drop(seen);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_reads_the_current_return_value_of_a_settings_function() {
    let timeouts: Arc<Mutex<Vec<Option<u64>>>> = Arc::new(Mutex::new(Vec::new()));
    let faux = faux_provider();
    let replies = {
        let timeouts = Arc::clone(&timeouts);
        let factory = move |input: FauxResponseInput| {
            lock(&timeouts).push(
                input
                    .options
                    .as_ref()
                    .and_then(|options| options.timeout_ms),
            );
            async move { plain("ok") }
        };
        vec![
            FauxResponseStep::message_factory(factory.clone()),
            FauxResponseStep::message_factory(factory),
        ]
    };
    faux.set_responses(replies);
    let models = create_models(Default::default());
    models.set_provider(Arc::clone(&faux.provider));
    let settings: Arc<Mutex<HarnessSettings>> = Arc::new(Mutex::new(HarnessSettings {
        stream: Some(ConversationStreamOptions {
            timeout_ms: Some(1000.0),
            ..Default::default()
        }),
        ..Default::default()
    }));
    let reader = Arc::clone(&settings);
    let (harness, root) = open(
        ModelsProvider::Direct(models),
        Some(SettingsProvider::Dynamic(Arc::new(move || {
            Some(lock(&reader).clone())
        }))),
        create_registry_impl(),
    )
    .await;
    let _ = turn(&root, "one").await;
    *lock(&settings) = HarnessSettings {
        stream: Some(ConversationStreamOptions {
            timeout_ms: Some(2000.0),
            ..Default::default()
        }),
        ..Default::default()
    };
    let _ = turn(&root, "two").await;
    assert_eq!(*lock(&timeouts), vec![Some(1000), Some(2000)]);
    harness.close(&*context()).await;
}

/// TS: `Submissions.submit()` 在提交回调里求值 `#queueModes()`（宿主 settings）。宿主抛错时
/// 这次提交以同一错误拒绝，且什么都不入队——不是"报告 host_error 后按默认模式继续"。
#[tokio::test(flavor = "multi_thread")]
async fn a_throwing_settings_function_rejects_submit_without_enqueuing() {
    use kiana_durable::types::ReportInfo;
    let registry = create_registry_impl();
    let storage = memory();
    let reports: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let harness = Harness::open(
        storage,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(answering("never")),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: Some(SettingsProvider::Dynamic(Arc::new(|| {
                std::panic::panic_any(Box::new(std::io::Error::other("settings broke")) as BoxError)
            }))),
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: Some(Arc::new(move |error, _info: &ReportInfo| {
                lock(&sink).push(error.to_string());
            })),
            read_only: None,
        },
        &*context(),
    )
    .await
    .expect("the harness opens");
    let root = harness
        .root(&*context(), RootOptions::default())
        .await
        .expect("the root");
    let error = match root
        .submit(text_input("hi").into_draft(), &*context())
        .await
    {
        Ok(_) => panic!("submit rejects with the settings error"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "settings broke");
    assert!(
        all_entries(&root, &*context()).await.is_empty(),
        "nothing was enqueued"
    );
    // 调度器自己的 settings 读取仍可能报告 host_error/settings（TS 的 #restartLimit 等同样如此）；
    // 这里只要求提交本身以同一错误被拒、没有任何输入入队。
    let _ = &reports;
    assert!(
        !all_entries(&root, &*context())
            .await
            .iter()
            .any(|entry| entry.kind == "kiana.input"),
        "no input entry"
    );
    harness.close(&*context()).await;
}
