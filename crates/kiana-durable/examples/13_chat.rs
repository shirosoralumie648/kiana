//! 移植自 `packages/durable/test/examples/13-chat.ts`：一次聊天回合。

use std::any::Any;
use std::sync::Arc;

use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message, faux_provider};
use kiana_ai::types::Message;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::define::{SectionRender, define_extension, section};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, EntryRangeQuery, Extension, HarnessOptions, ModelRef, ModelsProvider,
    PromptSection, Registry, RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind,
    UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    AssistantEntry, BoxError, CommitOptions, EntryId, EntryRecord, Session, SessionChange,
    SubmissionKind, SubmissionStatus, Tx, TypedEntryTx,
};

/// `terse` 扩展：一段不带标签的 preamble。
struct Terse {
    sections: Vec<Arc<dyn PromptSection>>,
}

impl Extension for Terse {
    fn name(&self) -> &str {
        "terse"
    }

    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
}

fn terse() -> Arc<dyn Extension> {
    let preamble: SectionRender =
        Arc::new(|_, _| Box::pin(async { Ok(Some("You answer in one word.".to_owned())) }));
    define_extension(Terse {
        sections: vec![section("preamble", preamble, Some(Some(false)))],
    })
}

/// 读回一次回答的助手条目。
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

    // faux provider 代替真实模型（见 15-real-model.ts）。
    let faux = faux_provider();
    let models: Arc<dyn Models> = {
        let models = create_models(Default::default());
        models.set_provider(Arc::clone(&faux.provider));
        models
    };
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        "Paris.",
        Default::default(),
    ))]);

    let registry = create_registry_impl();
    registry.install(terse());
    let harness = Harness::open(
        Arc::new(MemoryStorage::new()),
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
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
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await?;

    // submit() 持久接纳用户输入并返回一个 Submission。内建的 kiana.generation 任务用 agent 的
    // 分段准备系统提示、调用模型并追加回答；wait() 在输入被回答或失败后落定。
    let capital = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("Capital of France?".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    let answered = capital.wait(context).await?;
    if answered.status == SubmissionStatus::Done && answered.kind == SubmissionKind::Input {
        let entry = root
            .commit(
                &ReadEntry {
                    id: answered.answer.expect("a done input has an answer entry"),
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
                println!("answer: {}", serde_json::to_string(&assistant.content)?);
            }
            other => println!("answer: {other:?}"),
        }
    }

    // 转录里有用户输入、位置性的系统提示与回答。
    let transcript = root
        .entries(EntryRangeQuery::default(), 10, None, context)
        .await?;
    let kinds: Vec<String> = transcript
        .items
        .iter()
        .rev()
        .map(|entry| entry.kind.clone())
        .collect();
    println!("transcript: {kinds:?}");

    harness.close(context).await;
    Ok(())
}
