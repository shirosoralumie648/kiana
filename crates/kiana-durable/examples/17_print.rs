//! 移植自 `packages/durable/test/examples/17-print.ts`：打印模式，提交一个提示并打印它的回答，
//! 像 `kiana -p`。宿主等的是自己的 Submission，而不是全局空闲。设置了 `OPENAI_API_KEY` 时用 OpenAI，
//! 否则用脚本化的 faux 模型。

use std::any::Any;
use std::sync::Arc;

use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, FauxToolCallOptions, faux_assistant_message,
    faux_provider, faux_text, faux_tool_call_with,
};
use kiana_ai::providers::openai::openai_provider;
use kiana_ai::types::{AssistantContentBlock, JsonObject, Message, StopReason};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::env::ExecutionEnv;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::harness::define::{SectionRender, define_extension, section};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, EnvBuilder, Extension, HarnessOptions, ModelRef, ModelsProvider, PromptSection,
    Registry, RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind, ToolRegistration,
    UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::tools::bash::create_bash_tool;
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use kiana_durable::types::{
    AssistantEntry, BoxError, CommitOptions, EntryId, EntryRecord, JsonValue, Session,
    SessionChange, SubmissionKind, SubmissionStatus, Tx, TypedEntryTx,
};

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

struct ReadEntry {
    id: EntryId,
}

#[async_trait::async_trait]
impl SessionChange for ReadEntry {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let entry = tx.entry_typed(&AssistantEntry, self.id).await?;
        Ok(Box::new(entry.map(|entry| entry.record)))
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "What is in this directory?".to_owned());

    let models = create_models(Default::default());
    let mut model = ModelRef {
        provider: "openai".to_owned(),
        model_id: "gpt-6-sol".to_owned(),
    };
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        models.set_provider(openai_provider());
    } else {
        let faux = faux_provider();
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
        Arc::new(MemoryStorage::new()),
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
    let settled = submission.wait(context).await?;
    if settled.status == SubmissionStatus::Done && settled.kind == SubmissionKind::Input {
        let entry = root
            .commit(
                &ReadEntry {
                    id: settled.answer.expect("a done input has an answer entry"),
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        let entry = *entry
            .downcast::<Option<EntryRecord>>()
            .expect("the commit returns the entry");
        match entry.and_then(|entry| entry.model.and_then(|model| model.into_iter().next())) {
            Some(Message::Assistant(assistant)) => {
                let text: String = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect();
                println!("{text}");
            }
            _ => {
                eprintln!("unanswered: no assistant message");
                std::process::exit(1);
            }
        }
    } else {
        let reason = if settled.status == SubmissionStatus::Unanswered {
            settled.reason.clone().unwrap_or_default()
        } else {
            format!("{:?}", settled.status).to_lowercase()
        };
        eprintln!("unanswered: {reason}");
        harness.close(context).await;
        std::process::exit(1);
    }
    harness.close(context).await;
    Ok(())
}
