//! 移植自 `packages/durable/test/examples/04-watches.ts`：用 watch 串行化异步文档工作。
//!
//! watch 从一个稳定的获取版本开始；回调不会重叠——回调运行期间到达的提交会折叠进下一次调用，
//! 后者拿到最新值与把上次投递值变成它的 ops（超过 10,000 个 pending op 后是一次整值替换）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_chord::delta::Op;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{
    BoxError, DocDefinition, DocTarget, DocumentFork, DocumentHistory, DocumentSemantics,
    JsonObject, JsonValue, ScopeKind, WatchListener, define_doc,
};

fn notes_definition() -> DocDefinition {
    DocDefinition::new(
        "example.notes",
        1,
        DocumentSemantics {
            scope: ScopeKind::Conversation,
            history: Some(DocumentHistory::Rewindable),
            fork: Some(DocumentFork::AsOf),
        },
    )
    .with_initial(|_| {
        let mut value = JsonObject::new();
        value.insert("text".to_owned(), JsonValue::String(String::new()));
        value
    })
}

struct NotesListener {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl WatchListener<Option<JsonObject>> for NotesListener {
    async fn call(&self, value: Option<JsonObject>, ops: Vec<Op>, _context: Arc<dyn Context>) {
        println!(
            "watch update: {value:?} {}",
            serde_json::to_string(&kiana_chord::delta::index::encode(&ops)).unwrap_or_default()
        );
        // 第一次调用保持忙碌，像一个慢消费者；期间到达的提交会折叠。
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());
    let notes = define_doc(notes_definition());

    let notes_for_commit = notes.clone();
    let chat = session
        .commit_with(
            move |tx| {
                let notes = notes_for_commit.clone();
                Box::pin(async move {
                    let conversation = tx
                        .create_conversation(&kiana_durable::CreateConversationOptions {
                            ownership: kiana_durable::ConversationOwnership::Ownerless,
                        })
                        .await?;
                    let target = DocTarget::conversation(&notes, conversation.id);
                    let mut draft = tx.doc(&target).await?;
                    draft
                        .value_mut()
                        .insert("text".to_owned(), JsonValue::from("first"));
                    Ok(conversation)
                })
            },
            context,
            Default::default(),
        )
        .await?;

    let notes_target = DocTarget::conversation(&notes, chat.id);
    let notes_watch = session
        .watch_doc(&notes_target, context)
        .await?
        .ok_or_else(|| std::io::Error::other("notes are absent"))?;
    println!("watch baseline: {:?}", notes_watch.value());

    let calls = Arc::new(AtomicUsize::new(0));
    notes_watch.start(Arc::new(NotesListener {
        calls: Arc::clone(&calls),
    }));

    let seen_target = notes_target.clone();
    session
        .commit_with(
            move |tx| {
                let seen_target = seen_target.clone();
                Box::pin(async move {
                    let mut draft = tx.doc(&seen_target).await?;
                    draft
                        .value_mut()
                        .insert("text".to_owned(), JsonValue::from("observed"));
                    Ok(())
                })
            },
            context,
            Default::default(),
        )
        .await?;
    for more in [" asynchronously", ", folded", "!"] {
        let more_target = notes_target.clone();
        let more = more.to_owned();
        session
            .commit_with(
                move |tx| {
                    let more_target = more_target.clone();
                    let more = more.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&more_target).await?;
                        let current = draft
                            .get("text")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_owned();
                        draft.value_mut().insert(
                            "text".to_owned(),
                            JsonValue::from(format!("{current}{more}")),
                        );
                        Ok(())
                    })
                },
                context,
                Default::default(),
            )
            .await?;
    }

    for _ in 0..200 {
        if calls.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    println!("listener calls: {}", calls.load(Ordering::SeqCst));
    notes_watch.stop().await;

    session.close(context).await;
    Ok(())
}
