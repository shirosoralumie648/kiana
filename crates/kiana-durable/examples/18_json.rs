//! 移植自 `packages/durable/test/examples/18-json.ts`：把一个提示的运行按 JSON 行流式输出，
//! 像 `kiana --mode json`。两种模式：
//!   --events（默认）：`watchEvents()` 的实验性 agent 事件，第一条是 `snapshot` 事件。
//!   --ops：原始 ConversationView 帧，第一条是视图本身；之后每行是一次提交的 ops。
//! --storage sqlite（默认）| jsonl | memory：sqlite 写一个临时数据库文件，jsonl 写一个临时目录；
//! 两者都不删除，路径最后打印到 stderr。设置了 `OPENAI_API_KEY` 时用 OpenAI，否则用脚本化的
//! faux 模型。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, FauxToolCallOptions,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider_with, faux_text,
    faux_tool_call_with,
};
use kiana_ai::providers::openai::openai_provider;
use kiana_ai::types::{AssistantContentBlock, JsonObject, StopReason};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::Op;
use kiana_chord::delta::index::encode;
use kiana_durable::env::ExecutionEnv;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::harness::define::{SectionRender, define_extension, section};
use kiana_durable::harness::events::AgentEvent;
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, ConversationView, EnvBuilder, Extension, HarnessOptions, ModelRef, ModelsProvider,
    PromptSection, Registry, RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind,
    ToolRegistration, UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::storage::jsonl::{JsonlStorageOptions, open_node_jsonl_storage};
use kiana_durable::tools::bash::create_bash_tool;
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use kiana_durable::types::{BoxError, JsonValue, Session, Storage, WatchListener};

struct Coding {
    tools: Vec<ToolRegistration>,
    sections: Vec<Arc<dyn PromptSection>>,
}

impl Extension for Coding {
    fn name(&self) -> &str {
        "coding"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }

    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
}

/// 输出是稳定的 JSON 行契约：数字走 JS 数字格式、对象键按插入顺序。
fn print_line(value: &impl serde::Serialize) {
    match serde_json::to_value(value) {
        Ok(value) => println!("{}", kiana_json::js_object::stringify(&value)),
        Err(error) => eprintln!("serialize failed: {error}"),
    }
}

/// 把一批事件逐条打印成 JSON 行。
struct EventPrinter;

#[async_trait]
impl WatchListener<Vec<AgentEvent>> for EventPrinter {
    async fn call(&self, value: Vec<AgentEvent>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        for event in &value {
            print_line(event);
        }
    }
}

/// 把一批视图 ops 打印成一行。
struct OpsPrinter;

#[async_trait]
impl WatchListener<ConversationView> for OpsPrinter {
    async fn call(&self, _value: ConversationView, ops: Vec<Op>, _context: Arc<dyn Context>) {
        print_line(&serde_json::json!({ "ops": encode(&ops) }));
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn temp_dir() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn bash_call(command: &str) -> FauxResponseStep {
    let mut arguments = JsonObject::new();
    arguments.insert("command".to_owned(), JsonValue::from(command));
    FauxResponseStep::Message(faux_assistant_message(
        AssistantContentBlock::ToolCall(faux_tool_call_with(
            "bash",
            arguments,
            FauxToolCallOptions {
                id: Some("call-1".to_owned()),
            },
        )),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode_ops = args.iter().any(|arg| arg == "--ops");
    let storage_index = args.iter().position(|arg| arg == "--storage");
    let storage_kind = storage_index
        .and_then(|index| args.get(index + 1).cloned())
        .unwrap_or_else(|| {
            if cfg!(feature = "sqlite") {
                "sqlite".to_owned()
            } else {
                "memory".to_owned()
            }
        });
    let prompt = args
        .iter()
        .enumerate()
        .find(|(index, arg)| {
            !arg.starts_with("--") && storage_index.is_none_or(|storage| *index != storage + 1)
        })
        .map(|(_, arg)| arg.clone())
        .unwrap_or_else(|| "What is in this directory?".to_owned());

    let mut location: Option<String> = None;
    let storage: Arc<dyn Storage> = match storage_kind.as_str() {
        "sqlite" => {
            #[cfg(feature = "sqlite")]
            {
                let path = format!(
                    "{}/kiana-durable-json-{}.sqlite",
                    temp_dir(),
                    unique_suffix()
                );
                let opened = kiana_durable::storage::sqlite::node::open_node_sqlite_storage(
                    &path,
                    kiana_durable::storage::sqlite::node::NodeSqliteStorageOptions::default(),
                )
                .await?;
                location = Some(path);
                Arc::new(opened)
            }
            #[cfg(not(feature = "sqlite"))]
            {
                return Err(Box::new(std::io::Error::other(
                    "Unknown --storage sqlite; this build lacks the sqlite feature. Use jsonl or memory",
                )) as BoxError);
            }
        }
        "jsonl" => {
            let directory = format!("{}/kiana-durable-json-{}", temp_dir(), unique_suffix());
            tokio::fs::create_dir_all(&directory)
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            let opened =
                open_node_jsonl_storage(&directory, context, JsonlStorageOptions::default())
                    .await?;
            location = Some(directory);
            Arc::new(opened)
        }
        "memory" => Arc::new(MemoryStorage::new()),
        other => {
            return Err(Box::new(std::io::Error::other(format!(
                "Unknown --storage {other}; use sqlite, jsonl, or memory"
            ))) as BoxError);
        }
    };

    let models = create_models(Default::default());
    let mut model = ModelRef {
        provider: "openai".to_owned(),
        model_id: "gpt-6-sol".to_owned(),
    };
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        models.set_provider(openai_provider());
    } else {
        let faux = faux_provider_with(RegisterFauxProviderOptions {
            tokens_per_second: Some(200.0),
            ..Default::default()
        });
        models.set_provider(Arc::clone(&faux.provider));
        model = ModelRef {
            provider: "faux".to_owned(),
            model_id: "faux-1".to_owned(),
        };
        faux.set_responses(vec![
            bash_call("ls"),
            FauxResponseStep::Message(faux_assistant_message(
                vec![AssistantContentBlock::Text(faux_text(
                    "This directory holds the durable package sources, tests, and docs.",
                ))],
                Default::default(),
            )),
        ]);
    }
    let models: Arc<dyn Models> = models;

    let preamble: SectionRender = Arc::new(|_, _| {
        Box::pin(async { Ok(Some("You are a concise coding assistant.".to_owned())) })
    });
    let registry = create_registry_impl();
    registry.install(define_extension(Coding {
        tools: vec![
            create_read_tool(ReadToolOptions::default()),
            create_bash_tool(None),
        ],
        sections: vec![section("preamble", preamble, Some(Some(false)))],
    }));

    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let env: EnvBuilder = Arc::new(move |_target, _context| {
        let cwd = cwd.clone();
        Box::pin(async move {
            let env = NodeExecutionEnv::new(NodeEnvOptions::new(cwd));
            Ok(Some(Arc::new(env) as Arc<dyn ExecutionEnv>))
        })
    });
    let harness = Harness::open(
        Arc::clone(&storage),
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: Some(env),
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        context,
    )
    .await?;
    let root = harness
        .root(
            context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(model)),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await?;

    // 提交之前挂上流，这样它覆盖整个运行。
    let mut ops_watch: Option<kiana_durable::harness::types::ConversationWatch> = None;
    let stream = if mode_ops {
        let watch = root.watch(context).await?;
        print_line(&serde_json::json!({ "view": watch.value() }));
        watch.start(Arc::new(OpsPrinter));
        ops_watch = Some(watch);
        None
    } else {
        let stream = harness.watch_events(root.id(), context).await?;
        print_line(&stream.snapshot);
        stream.start(Arc::new(EventPrinter));
        Some(stream)
    };

    let submission = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text(prompt),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    submission.wait(context).await?;
    harness.wait_for_idle(context).await?;
    // 让最后一批到达监听器，再停止。
    tokio::time::sleep(Duration::from_millis(1)).await;
    if let Some(stream) = &stream {
        stream.stop().await;
    }
    if let Some(watch) = &ops_watch {
        watch.stop().await;
    }
    harness.close(context).await;
    if let Some(location) = location {
        eprintln!("{storage_kind} storage: {location}");
    }
    Ok(())
}
