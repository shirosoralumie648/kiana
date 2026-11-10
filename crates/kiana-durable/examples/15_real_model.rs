//! 移植自 `packages/durable/test/examples/15-real-model.ts`：从 OpenAI 流式读取回答。
//!
//! 从 `packages/durable` 运行（需要 `OPENAI_API_KEY`）：
//! `cargo run -p kiana-durable --features examples --example 15_real_model`
//!
//! 生产代码把一个带真实 provider 的 Models 集合交给 Harness；Harness 只通过它访问模型。
//! `openai_provider()` 从环境读取 `OPENAI_API_KEY`。回答流式到达时，generation 把节流的
//! partial 提交到会话的 `kiana.live` 文档；watch 该文档即可流式打印回答——watch 只看到已提交的值。

use std::any::Any;
use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{Models, create_models};
use kiana_ai::providers::openai::openai_provider;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::Op;
use kiana_durable::harness::define::{SectionRender, define_extension, section};
use kiana_durable::harness::harness::Harness;
use kiana_durable::harness::live::LIVE_DOC;
use kiana_durable::harness::registry::create_registry_impl;
use kiana_durable::harness::types::{
    AgentChange, Extension, HarnessOptions, ModelRef, ModelsProvider, PromptSection, Registry,
    RegistryReader, RootOptions, SubmissionDraft, SubmissionDraftKind, UserInput,
};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::types::{
    AssistantEntry, BoxError, CommitOptions, DocTarget, DocumentObserver, EntryId, EntryRecord,
    JsonObject, JsonValue, Session, SessionChange, SubmissionKind, SubmissionStatus, Tx,
    TypedEntryTx, WatchListener,
};

/// `concise` 扩展：一段不带标签的 preamble。
struct Concise {
    sections: Vec<Arc<dyn PromptSection>>,
}

impl Extension for Concise {
    fn name(&self) -> &str {
        "concise"
    }

    fn sections(&self) -> Vec<Arc<dyn PromptSection>> {
        self.sections.clone()
    }
}

fn concise() -> Arc<dyn Extension> {
    let preamble: SectionRender =
        Arc::new(|_, _| Box::pin(async { Ok(Some("You are a concise assistant.".to_owned())) }));
    define_extension(Concise {
        sections: vec![section("preamble", preamble, Some(Some(false)))],
    })
}

/// 读回一次回答的助手条目。
struct ReadEntry {
    id: EntryId,
}

#[async_trait]
impl SessionChange for ReadEntry {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let entry = tx.entry_typed(&AssistantEntry, self.id).await?;
        Ok(Box::new(entry.map(|entry| entry.record)))
    }
}

/// 只打印每个已提交 partial 相对已打印文本新增的部分。
struct TextPrinter {
    printed: Mutex<String>,
}

impl TextPrinter {
    fn print_text(&self, text: &str) {
        let mut printed = self
            .printed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if text.len() <= printed.len() || !text.starts_with(printed.as_str()) {
            return;
        }
        print!("{}", &text[printed.len()..]);
        let _ = std::io::stdout().flush();
        *printed = text.to_owned();
    }
}

/// 回答的文本块（TS: `content.find(content => content.type === "text")`）。
fn text_of_message(value: &JsonValue) -> Option<&str> {
    value
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("type").and_then(JsonValue::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(JsonValue::as_str))
                    .flatten()
            })
        })
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for TextPrinter {
    async fn call(&self, value: Option<JsonObject>, _ops: Vec<Op>, _context: Arc<dyn Context>) {
        let Some(value) = value else {
            return;
        };
        let block = value
            .get("generation")
            .and_then(|generation| generation.get("message"))
            .and_then(text_of_message);
        if let Some(text) = block {
            self.print_text(text);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    if std::env::var_os("OPENAI_API_KEY").is_none() {
        println!("skipped: OPENAI_API_KEY is not set");
        return Ok(());
    }

    let models: Arc<dyn Models> = {
        let models = create_models(Default::default());
        models.set_provider(openai_provider());
        models
    };
    let registry = create_registry_impl();
    registry.install(concise());
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
                        provider: "openai".to_owned(),
                        model_id: "gpt-6-sol".to_owned(),
                    })),
                    thinking_level: Some(Some(ModelThinkingLevel::High)),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await?;
    let printer = Arc::new(TextPrinter {
        printed: Mutex::new(String::new()),
    });
    let live_target = DocTarget::conversation(&LIVE_DOC, root.id());
    let live_watch = harness
        .watch_doc(&live_target, context)
        .await?
        .ok_or_else(|| std::io::Error::other("kiana.live is absent"))?;
    live_watch.start(Arc::clone(&printer) as Arc<dyn WatchListener<Option<JsonObject>>>);
    harness.resume()?;
    print!("answer: ");
    let _ = std::io::stdout().flush();
    let poem = root
        .submit(
            SubmissionDraft {
                request_id: None,
                kind: SubmissionDraftKind::Input {
                    content: UserInput::Text("Write a long poem".to_owned()),
                    when_busy: None,
                    entry: None,
                },
            },
            context,
        )
        .await?;
    let settled_poem = poem.wait(context).await?;
    let _ = live_watch.stop().await;
    if settled_poem.status == SubmissionStatus::Done && settled_poem.kind == SubmissionKind::Input {
        // 最后一个节流窗口未必作为 partial 提交过；回答条目里有剩下的部分。
        let entry = root
            .commit(
                &ReadEntry {
                    id: settled_poem
                        .answer
                        .expect("a done input has an answer entry"),
                },
                context,
                CommitOptions::default(),
            )
            .await?;
        let entry = *entry
            .downcast::<Option<EntryRecord>>()
            .expect("the commit returns the entry");
        let message =
            entry.and_then(|entry| entry.model.and_then(|model| model.into_iter().next()));
        if let Some(kiana_ai::Message::Assistant(assistant)) = message {
            let content = serde_json::to_value(&assistant.content)?;
            if let Some(text) = text_of_message(&serde_json::json!({ "content": content })) {
                printer.print_text(text);
            }
        }
        println!();
    } else {
        println!(
            "unanswered: {} {}",
            settled_poem.reason.as_deref().unwrap_or("unknown"),
            settled_poem
                .detail
                .as_ref()
                .map(kiana_json::js_object::stringify)
                .unwrap_or_else(|| "undefined".to_owned())
        );
    }

    harness.close(context).await;
    Ok(())
}
