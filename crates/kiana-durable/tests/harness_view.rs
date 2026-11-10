//! `test/harness-view.test.ts` 的 Rust 端口。
//!
//! 结构与操作断言用 `kiana_chord` 的 `Op`/`apply_immutable` 逐帧重放；TS 里用对象身份
//! (`toBe`) 断言的共享在 Rust 侧按结构相等断言（值按值持有，见报告的 deviation）。

#![allow(clippy::await_holding_lock)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_chord::delta::Op;
use kiana_chord::delta::Seg;
use kiana_chord::delta::index::apply_immutable;
use kiana_durable::Session;
use kiana_durable::harness::harness::HarnessImpl;
use kiana_durable::harness::live::LIVE_DOC;
use kiana_durable::harness::types::{
    Conversation as ConversationTrait, ConversationView, SubmissionDraft, SubmissionDraftKind,
};
use kiana_durable::types::{
    BoxError, CommitChange, ConversationId, DocDefinition, DocTarget, DocToken,
    DocumentCommitChange, DocumentFork, DocumentHistory, DocumentReader, DocumentSemantics,
    EntryDraft, JsonObject, JsonValue, ScopeKind, SessionChange, Storage, TableCommitChange, Tx,
    WatchEnd, WatchListener, define_doc,
};

use chat_support::{ChatSetup, all_entries, answer, context, drained_watch, open_chat, wait_for};

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn js_object(value: &JsonObject) -> JsonValue {
    JsonValue::Object(value.clone().into_iter().collect())
}

fn view_json(view: &ConversationView) -> JsonValue {
    serde_json::to_value(view).expect("the view serializes")
}

const MOUNTED: [&str; 5] = [
    "kiana.agent",
    "kiana.inbox",
    "kiana.live",
    "kiana.provider",
    "kiana.usage",
];

struct Frame {
    value: ConversationView,
    ops: Vec<Op>,
}

struct Recorder {
    frames: Mutex<Vec<Frame>>,
}

#[async_trait]
impl WatchListener<ConversationView> for Recorder {
    async fn call(&self, value: ConversationView, ops: Vec<Op>, _context: Arc<dyn Context>) {
        self.frames.lock().unwrap().push(Frame { value, ops });
    }
}

struct RecorderHandle {
    watch: kiana_durable::harness::types::ConversationWatch,
    initial: ConversationView,
    frames: Arc<Recorder>,
}

impl RecorderHandle {
    fn last(&self) -> ConversationView {
        self.frames
            .frames
            .lock()
            .unwrap()
            .last()
            .map(|frame| frame.value.clone())
            .unwrap_or_else(|| self.initial.clone())
    }

    async fn stop(&self) {
        let _ = self.watch.stop().await;
    }

    /// 等到至少 `count` 帧被投递（并行测试负载下用真实时间轮询）。
    async fn wait_frames(&self, count: usize) {
        for _ in 0..2_000 {
            if self.frames.frames.lock().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        panic!(
            "expected {count} frames, got {}",
            self.frames.frames.lock().unwrap().len()
        );
    }
}

/// 观察 `conversation`：获取时的修订与之后每一帧。
async fn record(conversation: &Arc<dyn ConversationTrait>) -> RecorderHandle {
    let watch = conversation.watch(&*context()).await.expect("watch");
    let initial = watch.value().clone();
    let frames = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&frames) as Arc<dyn WatchListener<ConversationView>>);
    RecorderHandle {
        watch,
        initial,
        frames,
    }
}

async fn fresh(conversation: &Arc<dyn ConversationTrait>) -> ConversationView {
    let watch = conversation.watch(&*context()).await.expect("watch");
    let value = watch.value().clone();
    let _ = watch.stop().await;
    value
}

/// 提交状态定义的视图（不经过任何挂载）。
async fn committed(
    harness: &Arc<HarnessImpl>,
    conversation: &Arc<dyn ConversationTrait>,
    record: &kiana_durable::types::ConversationRecord,
) -> ConversationView {
    let mut docs = indexmap::IndexMap::new();
    for token in [
        &kiana_durable::harness::agent::AGENT_DOC,
        &LIVE_DOC,
        &kiana_durable::harness::inbox::INBOX_DOC,
        &kiana_durable::harness::provider::PROVIDER_DOC,
        &kiana_durable::harness::usage::USAGE_DOC,
    ] {
        let target = DocTarget::conversation(token, conversation.id());
        if let Some(value) = harness
            .snapshot(&target, &*context())
            .await
            .expect("snapshot")
        {
            docs.insert(token.definition.kind.clone(), value);
        }
    }
    let context_view = conversation
        .context(&*context(), Default::default())
        .await
        .expect("context");
    ConversationView {
        conversation: record.clone(),
        entries: context_view.entries,
        docs,
    }
}

/// 从 `initial` 重放每一帧的操作，路上逐帧校验。
fn replay(initial: &ConversationView, frames: &[Frame]) {
    let mut value = view_json(initial);
    for frame in frames {
        value = apply_immutable(value, &frame.ops).expect("the operations apply");
        assert_eq!(value, view_json(&frame.value), "the ops rebuild the frame");
    }
}

fn touching(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
) -> Arc<std::sync::atomic::AtomicUsize> {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink = Arc::clone(&counter);
    // 与 TS 的 `touches` 相同：统计所有会推进挂载的提交。Rust 侧用 `observeCommits`
    // （含内存中采纳的 deferred 提交）；这些提交在 TS 里也会推进视图。
    let _unsubscribe = harness.observe_commits(Arc::new(move |publication, _context| {
        let touched = publication.changes().iter().any(|change| match change {
            CommitChange::Table(TableCommitChange::Entry { value }) => {
                value.conversation_id == conversation_id
            }
            CommitChange::Document(DocumentCommitChange::Document {
                record,
                conversation_id: owner,
                ops,
                ..
            }) => {
                owner.as_ref() == Some(&conversation_id)
                    && MOUNTED.contains(&record.kind.as_str())
                    && !ops.is_empty()
            }
            _ => false,
        });
        if touched {
            sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    counter
}

fn count(counter: &std::sync::atomic::AtomicUsize) -> usize {
    counter.load(std::sync::atomic::Ordering::SeqCst)
}

fn input(text: &str) -> SubmissionDraft {
    SubmissionDraft {
        request_id: None,
        kind: SubmissionDraftKind::Input {
            content: kiana_ai::types::UserContent::Text(text.to_owned()),
            when_busy: None,
            entry: None,
        },
    }
}

/// 追加一条只有 kind 的展示条目。
struct AppendKind {
    conversation_id: ConversationId,
    kind: String,
    head: Option<kiana_durable::types::EntryId>,
}

#[async_trait]
impl SessionChange for AppendKind {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: self.kind.clone(),
                    model: None,
                    data: None,
                    head: self.head.map(kiana_durable::types::EntryHead::Id),
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

async fn note(
    conversation: &Arc<dyn ConversationTrait>,
    kind: &str,
) -> kiana_durable::types::EntryRecord {
    let change = AppendKind {
        conversation_id: conversation.id(),
        kind: kind.to_owned(),
        head: None,
    };
    let value = conversation
        .commit(&change, &*context(), Default::default())
        .await
        .expect("commit");
    *value
        .downcast::<kiana_durable::types::EntryRecord>()
        .expect("entry")
}

async fn note_at(
    conversation: &Arc<dyn ConversationTrait>,
    kind: &str,
    head: kiana_durable::types::EntryId,
) -> kiana_durable::types::EntryRecord {
    let change = AppendKind {
        conversation_id: conversation.id(),
        kind: kind.to_owned(),
        head: Some(head),
    };
    let value = conversation
        .commit(&change, &*context(), Default::default())
        .await
        .expect("commit");
    *value
        .downcast::<kiana_durable::types::EntryRecord>()
        .expect("entry")
}

fn kinds(view: &ConversationView) -> Vec<String> {
    view.entries
        .iter()
        .map(|entry| entry.kind.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn hydrates_the_active_entries_and_the_built_in_documents() {
    let setup = ChatSetup::new();
    setup.faux.set_responses(vec![answer("hello")]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let submission = root
        .submit(input("hi"), &*context())
        .await
        .expect("submitted");
    let _ = submission.wait(&*context()).await.expect("settled");

    let view = fresh(&root).await;
    assert_eq!(view.conversation.id, root.id());
    assert_eq!(view.entries, all_entries(&root, &*context()).await);
    let mut keys: Vec<String> = view.docs.keys().cloned().collect();
    keys.sort();
    let mut expected: Vec<String> = MOUNTED.iter().map(|kind| (*kind).to_owned()).collect();
    expected.sort();
    assert_eq!(keys, expected);
    assert_eq!(
        view.docs.get("kiana.live").map(js_object),
        Some(serde_json::json!({}))
    );
    assert_eq!(
        view.docs.get("kiana.inbox").map(js_object),
        Some(serde_json::json!({"items": []}))
    );
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn publishes_one_frame_per_touching_commit_whose_operations_rebuild_every_revision() {
    let setup = ChatSetup::new();
    let release = Arc::new(tokio::sync::Notify::new());
    let step = {
        let release = Arc::clone(&release);
        kiana_ai::providers::faux::FauxResponseStep::message_factory(move |_input| {
            let release = Arc::clone(&release);
            async move {
                release.notified().await;
                kiana_ai::providers::faux::faux_assistant_message(
                    kiana_ai::providers::faux::faux_text("a longer answer"),
                    Default::default(),
                )
            }
        })
    };
    setup.faux.set_responses(vec![step]);
    let (harness, root) = open_chat(memory(), &setup).await;
    let recorder = record(&root).await;
    let touching = touching(&harness, root.id());
    let submission = root
        .submit(input("hi"), &*context())
        .await
        .expect("submitted");
    let frames = Arc::clone(&recorder.frames);
    wait_for(
        move || {
            let frames = Arc::clone(&frames);
            async move {
                frames.frames.lock().unwrap().iter().any(|frame| {
                    frame
                        .value
                        .docs
                        .get("kiana.live")
                        .and_then(|live| live.get("generation"))
                        .is_some()
                })
            }
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("a generation frame arrives");
    // `notify_one` 在没有等待者时保留一个许可：faux 步骤可能还没注册等待就被释放
    // （TS 的 deferred promise 不存在这个竞态）。
    release.notify_one();
    let _ = submission.wait(&*context()).await.expect("settled");
    harness.wait_for_idle(&*context()).await.expect("idle");
    // 提交的发布（internal listeners）在结算之后：等投递追上已记录的 touching 提交数，
    // 排除测试线程先于发布读到计数的竞态。
    wait_for(
        {
            let frames = Arc::clone(&recorder.frames);
            let touching = Arc::clone(&touching);
            move || {
                let frames = Arc::clone(&frames);
                let touching = Arc::clone(&touching);
                async move { frames.frames.lock().unwrap().len() == count(&touching) }
            }
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("every touching commit delivers a frame");
    drained_watch(&*recorder.watch).await;
    let expected = count(&touching);
    let frames = recorder.frames.frames.lock().unwrap();
    // TS 断言 `toHaveLength(touching.count)`：每个 touching 提交恰好一帧。
    assert_eq!(frames.len(), expected, "one frame per touching commit");
    assert!(
        frames.iter().all(|frame| !frame.ops.is_empty()),
        "no empty frames"
    );
    replay(&recorder.initial, &frames);
    let first = &frames[0];
    assert!(
        first.ops.iter().any(|op| matches!(
            op,
            Op::Splice { path, index, remove, items }
                if path == &vec![Seg::Key("entries".into())]
                    && *index == 0
                    && *remove == 0
                    && items.first().and_then(|item| item.get("kind")) == Some(&serde_json::json!("kiana.user"))
        )),
        "the first frame splices the user entry"
    );
    assert!(
        frames
            .iter()
            .flat_map(|frame| &frame.ops)
            .any(|op| matches!(
                op,
                Op::Set(path, value)
                    if path == &vec![
                        Seg::Key("docs".into()),
                        Seg::Key("kiana.live".into()),
                        Seg::Key("generation".into()),
                    ] && value == &serde_json::json!({"attempt": 1})
            )),
        "document ops keep their exact shape under the mount path"
    );
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_unchanged_parts_and_skips_commits_that_touch_nothing_mounted() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let other = harness
        .create_conversation(chat_support::ownerless(None), &*context())
        .await
        .expect("other");
    let recorder = record(&root).await;
    let _ = note(&other, "note").await;
    root.configure(
        kiana_durable::harness::types::AgentChange {
            thinking_level: Some(Some(kiana_ai::ModelThinkingLevel::High)),
            ..Default::default()
        },
        &*context(),
    )
    .await
    .expect("configure");
    let _ = note(&root, "note").await;
    recorder.wait_frames(2).await;
    let frames = recorder.frames.frames.lock().unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0].ops,
        vec![Op::Set(
            vec![
                Seg::Key("docs".into()),
                Seg::Key("kiana.agent".into()),
                Seg::Key("thinkingLevel".into()),
            ],
            serde_json::json!("high"),
        )]
    );
    assert_eq!(frames[0].value.entries, recorder.initial.entries);
    assert_eq!(
        frames[0].value.docs.get("kiana.live"),
        recorder.initial.docs.get("kiana.live")
    );
    assert_eq!(frames[1].value.docs, frames[0].value.docs);
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cuts_the_entries_at_a_head_marker_keeping_the_entries_from_its_head() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let _ = note(&root, "a").await;
    let b = note(&root, "b").await;
    let _ = note(&root, "c").await;
    let recorder = record(&root).await;
    let summary = note_at(&root, "summary", b.id).await;
    let _ = note(&root, "d").await;
    root.reset(None, &*context()).await.expect("reset");
    recorder.wait_frames(3).await;
    let frames = recorder.frames.frames.lock().unwrap();
    let all: Vec<Vec<String>> = frames.iter().map(|frame| kinds(&frame.value)).collect();
    assert_eq!(
        all,
        vec![
            vec!["summary".to_owned(), "b".to_owned(), "c".to_owned()],
            vec![
                "summary".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ],
            vec!["kiana.reset".to_owned()],
        ]
    );
    assert_eq!(
        frames[0].ops,
        vec![Op::Splice {
            path: vec![Seg::Key("entries".into())],
            index: 0,
            remove: 1,
            items: vec![serde_json::to_value(&summary).expect("summary")],
        }]
    );
    replay(&recorder.initial, &frames);
    let committed = committed(&harness, &root, &recorder.initial.conversation).await;
    assert_eq!(
        frames.last().map(|frame| view_json(&frame.value)),
        Some(view_json(&committed))
    );
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_only_mounted_entries_for_a_raw_head_write_before_the_active_range() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let old = note(&root, "old").await;
    root.reset(None, &*context()).await.expect("reset");
    let recorder = record(&root).await;
    let _ = note_at(&root, "summary", old.id).await;
    recorder.wait_frames(1).await;
    let frames = recorder.frames.frames.lock().unwrap();
    assert_eq!(kinds(&frames[0].value), vec!["summary"]);
    drop(frames);
    recorder.stop().await;
    // 重建的挂载显示它。
    assert_eq!(kinds(&fresh(&root).await), vec!["summary", "old"]);
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cuts_a_forks_view_into_its_inherited_entries() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let a = note(&root, "a").await;
    let b = note(&root, "b").await;
    let fork = root
        .fork(b.id, chat_support::ownerless(None), &*context())
        .await
        .expect("fork");
    let recorder = record(&fork).await;
    let _ = note_at(&fork, "summary", b.id).await;
    recorder.wait_frames(1).await;
    let ids: Vec<_> = recorder
        .initial
        .entries
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, vec![a.id, b.id]);
    let frames = recorder.frames.frames.lock().unwrap();
    assert_eq!(kinds(&frames[0].value), vec!["summary", "b"]);
    let committed = committed(&harness, &fork, &recorder.initial.conversation).await;
    assert_eq!(view_json(&frames[0].value), view_json(&committed));
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shows_a_forks_inherited_entries_and_follows_only_the_forks_own_commits() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let first = note(&root, "first").await;
    let fork = root
        .fork(first.id, chat_support::ownerless(None), &*context())
        .await
        .expect("fork");
    let recorder = record(&fork).await;
    assert_eq!(kinds(&recorder.initial), vec!["first"]);
    assert_eq!(
        recorder.initial.conversation.parent,
        Some(kiana_durable::types::ConversationParent {
            conversation_id: root.id(),
            at: first.id,
        })
    );
    let _ = note(&root, "parent").await;
    let _ = note(&fork, "child").await;
    recorder.wait_frames(1).await;
    drained_watch(&*recorder.watch).await;
    let frames = recorder.frames.frames.lock().unwrap();
    let all: Vec<Vec<String>> = frames.iter().map(|frame| kinds(&frame.value)).collect();
    assert_eq!(all, vec![vec!["first".to_owned(), "child".to_owned()]]);
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unmounts_a_retired_document_and_mounts_its_recreation_whole() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let recorder = record(&root).await;
    struct Retire {
        conversation_id: ConversationId,
    }
    #[async_trait]
    impl SessionChange for Retire {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            tx.retire_doc(&DocTarget::conversation(&LIVE_DOC, self.conversation_id))
                .await?;
            Ok(Box::new(()))
        }
    }
    struct SetTools {
        conversation_id: ConversationId,
    }
    #[async_trait]
    impl SessionChange for SetTools {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut live = tx
                .doc(&DocTarget::conversation(&LIVE_DOC, self.conversation_id))
                .await?;
            live.insert("tools".to_owned(), serde_json::json!([]));
            Ok(Box::new(()))
        }
    }
    let _ = root
        .commit(
            &Retire {
                conversation_id: root.id(),
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("retire");
    recorder.wait_frames(1).await;
    let _ = root
        .commit(
            &SetTools {
                conversation_id: root.id(),
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("set");
    drained_watch(&*recorder.watch).await;
    let frames = recorder.frames.frames.lock().unwrap();
    assert_eq!(
        frames
            .iter()
            .map(|frame| frame.ops.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![Op::Delete(vec![
                Seg::Key("docs".into()),
                Seg::Key("kiana.live".into()),
            ])],
            vec![Op::Set(
                vec![Seg::Key("docs".into()), Seg::Key("kiana.live".into()),],
                serde_json::json!({"tools": []}),
            )],
        ]
    );
    assert!(frames[0].value.docs.get("kiana.live").is_none());
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn folds_undelivered_commits_into_one_call_whose_operations_reach_the_newest_view() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let watch = root.watch(&*context()).await.expect("watch");
    let initial = watch.value().clone();
    // 观察者还没启动：这段窗口里的提交在启动后折叠成一次投递。
    for _ in 0..101 {
        let _ = note(&root, "note").await;
    }
    let frames = Arc::new(Recorder {
        frames: Mutex::new(Vec::new()),
    });
    watch.start(Arc::clone(&frames) as Arc<dyn WatchListener<ConversationView>>);
    wait_for(
        {
            let frames = Arc::clone(&frames);
            move || {
                let frames = Arc::clone(&frames);
                async move { !frames.frames.lock().unwrap().is_empty() }
            }
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("the folded frame arrives");
    drained_watch(&*watch).await;
    let frames = frames.frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    assert_ne!(frames[0].ops.first().map(op_verb), Some("r"));
    let mut value = view_json(&initial);
    value = apply_immutable(value, &frames[0].ops).expect("apply");
    assert_eq!(value, view_json(&frames[0].value));
    assert_eq!(frames[0].value.entries.len(), 101);
    drop(frames);
    let _ = watch.stop().await;
    harness.close(&*context()).await;
}

fn op_verb(op: &Op) -> &'static str {
    match op {
        Op::Replace(_) => "r",
        Op::Set(..) => "s",
        Op::Delete(_) => "d",
        Op::Append(..) => "a",
        Op::Truncate(..) => "t",
        Op::Splice { .. } => "p",
        Op::Permute(..) => "m",
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_watches_of_one_conversation_independent_and_remounts_after_the_last_one_detaches() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let kept = record(&root).await;
    let second = record(&root).await;
    let _ = note(&root, "one").await;
    kept.wait_frames(1).await;
    second.wait_frames(1).await;
    assert_eq!(kinds(&kept.last()), vec!["one"]);
    second.stop().await;
    let second_frames = second.frames.frames.lock().unwrap().len();
    assert_eq!(second_frames, 1);
    let _ = note(&root, "two").await;
    wait_for(
        {
            let frames = Arc::clone(&kept.frames);
            move || {
                let frames = Arc::clone(&frames);
                async move {
                    frames
                        .frames
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(|frame| frame.value.entries.len() == 2)
                }
            }
        },
        std::time::Duration::from_secs(5),
    )
    .await
    .expect("the second entry arrives");
    let last = kept.last();
    assert_eq!(kinds(&last), vec!["one", "two"]);
    kept.stop().await;
    // 没有观察者了：新观察者从提交状态重建一个修订。
    let rebuilt = fresh(&root).await;
    assert_eq!(view_json(&rebuilt), view_json(&last));
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_watches_at_close_and_rejects_later_acquisition() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let watch = root.watch(&*context()).await.expect("watch");
    harness.close(&*context()).await;
    assert!(matches!(watch.closed().await, WatchEnd::Ended));
    assert!(watch.value().entries.is_empty());
    assert!(root.watch(&*context()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn publishes_one_frame_for_a_commit_that_appends_several_entries_and_edits_a_document() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let other: DocToken = define_doc(
        DocDefinition::new(
            "app.other",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("n".to_owned(), serde_json::json!(0));
            value
        })
        .with_checkpoint_when(|_, _, _| false),
    );
    struct Multi {
        conversation_id: ConversationId,
    }
    #[async_trait]
    impl SessionChange for Multi {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            tx.append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "a".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
            let mut live = tx
                .doc(&DocTarget::conversation(&LIVE_DOC, self.conversation_id))
                .await?;
            live.insert("tools".to_owned(), serde_json::json!([]));
            tx.append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "b".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
            Ok(Box::new(()))
        }
    }
    struct SetOther {
        conversation_id: ConversationId,
        token: DocToken,
    }
    #[async_trait]
    impl SessionChange for SetOther {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut doc = tx
                .doc(&DocTarget::conversation(&self.token, self.conversation_id))
                .await?;
            doc.insert("n".to_owned(), serde_json::json!(1));
            Ok(Box::new(()))
        }
    }
    let recorder = record(&root).await;
    let _ = root
        .commit(
            &Multi {
                conversation_id: root.id(),
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("multi");
    let _ = root
        .commit(
            &SetOther {
                conversation_id: root.id(),
                token: other,
            },
            &*context(),
            Default::default(),
        )
        .await
        .expect("other");
    drained_watch(&*recorder.watch).await;
    let frames = recorder.frames.frames.lock().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(kinds(&frames[0].value), vec!["a", "b"]);
    replay(&recorder.initial, &frames);
    let committed = committed(&harness, &root, &recorder.initial.conversation).await;
    assert_eq!(view_json(&frames[0].value), view_json(&committed));
    drop(frames);
    recorder.stop().await;
    harness.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_one_mount_between_concurrent_observers_and_isolates_a_failing_listener() {
    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let failing = root.watch(&*context()).await.expect("watch");
    let recorder = record(&root).await;
    let shared = root.watch(&*context()).await.expect("watch");
    assert_eq!(failing.value(), shared.value());
    shared.stop().await;

    struct Thrower;
    #[async_trait]
    impl WatchListener<ConversationView> for Thrower {
        async fn call(&self, _value: ConversationView, _ops: Vec<Op>, _context: Arc<dyn Context>) {
            panic!("listener failed");
        }
    }
    failing.start(Arc::new(Thrower));
    let _ = note(&root, "one").await;
    recorder.wait_frames(1).await;
    let _ = note(&root, "two").await;
    recorder.wait_frames(2).await;
    assert!(matches!(
        failing.closed().await,
        WatchEnd::ListenerError(message) if message.contains("listener failed")
    ));
    assert_eq!(recorder.frames.frames.lock().unwrap().len(), 2);
    recorder.stop().await;
    harness.close(&*context()).await;
}

/// 一个等释放的提交，用来占住 Session 线。
struct BlockingCommit {
    release: Arc<tokio::sync::Notify>,
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl SessionChange for BlockingCommit {
    async fn run(&self, _tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::new(()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_an_acquisition_cancelled_or_closed_while_it_waits_for_the_session_line() {
    use kiana_chord::context::{AbortReason, AbortSignal, with_abort_signal};

    let setup = ChatSetup::new();
    let (harness, root) = open_chat(memory(), &setup).await;
    let hold = || {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let change = BlockingCommit {
            release: Arc::clone(&release),
            started: Arc::clone(&started),
        };
        let conversation = Arc::clone(&root);
        let commit_context = context();
        let blocking = tokio::spawn(async move {
            conversation
                .commit(&change, &*commit_context, Default::default())
                .await
                .expect("the blocking commit joins")
        });
        (release, started, blocking)
    };

    // 1. 等待 Session 线时被取消。
    let (release, started, blocking) = hold();
    started.notified().await;
    let signal = AbortSignal::new();
    let cancelled_context = with_abort_signal(signal.clone(), Arc::clone(&context()));
    let watching = {
        let root = Arc::clone(&root);
        tokio::spawn(async move { root.watch(&*cancelled_context).await })
    };
    tokio::task::yield_now().await;
    signal.abort(Some(AbortReason::error(std::io::Error::other("cancelled"))));
    release.notify_one();
    blocking.await.expect("the blocking commit joins");
    let error = match watching.await.expect("the watch task joins") {
        Ok(_) => panic!("the acquisition is cancelled"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("cancelled"), "{error}");

    // 2. 等待 Session 线时 Harness 开始关闭。
    let (release, started, blocking) = hold();
    started.notified().await;
    // TS 的 `root.watch(context)` 在调用时就同步进入 `readOnLine` 排队；Rust 的 future 是惰性
    // 的，先 poll 一次让获取先入队，再让 close 开始（否则两者顺序不确定）。
    let watch_context = context();
    let mut queued = Box::pin(root.watch(&*watch_context));
    assert!(
        futures::poll!(queued.as_mut()).is_pending(),
        "the acquisition waits for the line"
    );
    // 和上面一样先 poll 一次 close：`close()` 的第一步是进入 Closing 并封住新的
    // acquisition。用 spawn + yield 不能保证 close 任务在 release 之前被调度，
    // 并行负载下会偶发地让 watch 在关闭前拿到会话线。
    let close_context = context();
    let mut closing = Box::pin(harness.close(&*close_context));
    assert!(
        futures::poll!(closing.as_mut()).is_pending(),
        "close waits for the held session line"
    );
    release.notify_one();
    blocking.await.expect("the blocking commit joins");
    let error = match queued.await {
        Ok(_) => panic!("the acquisition is closed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("Harness is closed"), "{error}");
    closing.await;
}
