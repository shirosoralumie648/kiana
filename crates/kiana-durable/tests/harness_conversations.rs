//! `test/harness-conversations.test.ts` 的 Rust 端口。
//!
//! 覆盖：根的惰性创建（agent 变更 + init 在同一提交里）、独立会话的原子创建与回滚、
//! 在具体条目处分叉（as-of agent 与覆盖）、agent 字段的整字段替换/清除、会话创建 hook、
//! 句柄在关闭后的拒绝、以及 Session 文档 API 的转发。

#![allow(clippy::await_holding_lock)]

#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::types::Message;
use kiana_chord::context::Context;
use kiana_durable::Session;
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationCreateOptions, ConversationInit,
    EntryRangeQuery, Registry, RootOptions,
};
use kiana_durable::storage::memory::MemoryStorage;
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, DocDefinition, DocTarget,
    DocToken, DocumentFork, DocumentHistory, DocumentObserver, DocumentReader, DocumentSemantics,
    EntryDraft, EntryRecord, JsonObject, JsonValue, ROOT_CONVERSATION_ID, ScanOrder, ScopeKind,
    SessionChange, Storage, StorageWrite, Tx, define_doc, define_entry, entries::NoData,
};

use harness_support::{TestExtension, add_tool, describe_message, open_harness, tool, user};
use support::ControlledStorage;

// ---------------------------------------------------------------------------
// 文档与提交夹具
// ---------------------------------------------------------------------------

static NOTE_DOC: std::sync::LazyLock<DocToken> = std::sync::LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "test.note",
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
        .with_checkpoint_when(|_, _, _| false),
    )
});

fn note_target(id: ConversationId) -> DocTarget {
    DocTarget::conversation(&NOTE_DOC, id)
}

fn js(value: &JsonObject) -> JsonValue {
    JsonValue::Object(value.clone().into_iter().collect())
}

/// 追加一条 `message` 条目（TS: `conversation.commit((tx) => tx.appendEntry(...))`）。
struct AppendMessage {
    conversation_id: ConversationId,
    text: String,
}

#[async_trait]
impl SessionChange for AppendMessage {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "message".to_owned(),
                    model: Some(vec![Message::User(user(&self.text))]),
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

async fn append(
    conversation: &Arc<dyn ConversationTrait>,
    text: &str,
    context: &dyn Context,
) -> EntryRecord {
    let change = AppendMessage {
        conversation_id: conversation.id(),
        text: text.to_owned(),
    };
    let value = conversation
        .commit(&change, context, CommitOptions::default())
        .await
        .expect("the entry commits");
    *value.downcast::<EntryRecord>().expect("an entry record")
}

async fn all_entries(
    conversation: &Arc<dyn ConversationTrait>,
    order: Option<ScanOrder>,
    context: &dyn Context,
) -> Vec<String> {
    let message = define_entry::<NoData>("message");
    let mut texts = Vec::new();
    let mut cursor = None;
    loop {
        let page = conversation
            .entries(
                EntryRangeQuery {
                    order,
                    ..Default::default()
                },
                2,
                cursor.clone(),
                context,
            )
            .await
            .expect("history");
        for entry in &page.items {
            assert!(message.is(entry), "all fixture entries are messages");
            let model = entry.model.as_ref().expect("a model message");
            texts.push(match &model[0] {
                Message::User(user) => match &user.content {
                    kiana_ai::types::UserContent::Text(text) => text.clone(),
                    _ => String::new(),
                },
                other => describe_message(other),
            });
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    texts
}

fn write_types(writes: &[StorageWrite]) -> Vec<&'static str> {
    writes
        .iter()
        .map(|write| match write {
            StorageWrite::Conversation { .. } => "conversation",
            StorageWrite::Entry { .. } => "entry",
            StorageWrite::Task { .. } => "task",
            StorageWrite::Submission { .. } => "submission",
            StorageWrite::DocumentCreate { .. } => "document.create",
            StorageWrite::DocumentCopy { .. } => "document.copy",
            StorageWrite::DocumentChange { .. } => "document.change",
            StorageWrite::DocumentRetire { .. } => "document.retire",
        })
        .collect()
}

fn commit_count(storage: &ControlledStorage) -> usize {
    support::lock(&storage.commits).len()
}

fn js_error(message: &str) -> BoxError {
    Box::new(std::io::Error::other(message.to_owned()))
}

fn is_uuid_v7(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes[14] == b'7'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn creates_the_root_lazily_with_its_agent_change_and_init_in_one_commit() {
    let storage = Arc::new(ControlledStorage::new());
    let storage_arc: Arc<dyn Storage> = Arc::clone(&storage) as Arc<dyn Storage>;
    let (harness, _registry) = open_harness(storage_arc, &["read", "bash"]).await;
    let context = harness_support::context();
    assert_eq!(commit_count(&storage), 0);

    let saw_high = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_high);
    let init: ConversationInit = Arc::new(move |tx, id| {
        let flag = Arc::clone(&flag);
        Box::pin(async move {
            let agent = tx
                .doc(&DocTarget::conversation(
                    &kiana_durable::harness::agent::AGENT_DOC,
                    id,
                ))
                .await?;
            flag.store(
                agent.get("thinkingLevel") == Some(&JsonValue::String("high".to_owned())),
                Ordering::SeqCst,
            );
            let mut note = tx.doc(&note_target(id)).await?;
            note.insert("text".to_owned(), JsonValue::String("root note".to_owned()));
            Ok(())
        })
    });
    let root = harness
        .root(
            &*context,
            RootOptions {
                agent: Some(AgentChange {
                    thinking_level: Some(Some(ModelThinkingLevel::High)),
                    ..Default::default()
                }),
                init: Some(init),
            },
        )
        .await
        .expect("root");
    assert_eq!(root.id(), ROOT_CONVERSATION_ID);
    assert!(
        saw_high.load(Ordering::SeqCst),
        "init ran after the agent change"
    );
    assert_eq!(commit_count(&storage), 1);
    // conversation、五个内建文档、以及 init 的 note。
    let commits = support::lock(&storage.commits);
    assert_eq!(
        write_types(&commits[0]),
        vec![
            "conversation",
            "document.create",
            "document.create",
            "document.create",
            "document.create",
            "document.create",
            "document.create",
        ]
    );
    drop(commits);

    let live = harness
        .snapshot(
            &DocTarget::conversation(
                &kiana_durable::harness::live::LIVE_DOC,
                ROOT_CONVERSATION_ID,
            ),
            &*context,
        )
        .await
        .expect("live")
        .expect("live");
    assert_eq!(js(&live), serde_json::json!({}));
    let provider = harness
        .snapshot(
            &DocTarget::conversation(
                &kiana_durable::harness::provider::PROVIDER_DOC,
                ROOT_CONVERSATION_ID,
            ),
            &*context,
        )
        .await
        .expect("provider")
        .expect("provider");
    let session_id = provider
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .expect("sessionId");
    assert!(is_uuid_v7(session_id), "{session_id} is a UUIDv7");
    let agent_state = harness
        .snapshot(
            &DocTarget::conversation(
                &kiana_durable::harness::agent::AGENT_DOC,
                ROOT_CONVERSATION_ID,
            ),
            &*context,
        )
        .await
        .expect("agent")
        .expect("agent");
    assert_eq!(
        js(&agent_state),
        serde_json::json!({"thinkingLevel": "high"})
    );
    let agent = root.agent(&*context).await.expect("agent");
    assert_eq!(agent.thinking_level, ModelThinkingLevel::High);
    let tool_names: Vec<String> = agent
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(tool_names, vec!["read", "bash"]);
    let note = harness
        .snapshot(&note_target(ROOT_CONVERSATION_ID), &*context)
        .await
        .expect("note")
        .expect("note");
    assert_eq!(js(&note), serde_json::json!({"text": "root note"}));

    // 第二次 root() 复用；不运行 init。
    let again = harness
        .root(
            &*context,
            RootOptions {
                agent: Some(AgentChange {
                    thinking_level: Some(Some(ModelThinkingLevel::Low)),
                    ..Default::default()
                }),
                init: Some(Arc::new(|_tx, _id| {
                    Box::pin(async { panic!("init must not run") })
                })),
            },
        )
        .await
        .expect("root again");
    assert_eq!(again.id(), root.id());
    assert_eq!(commit_count(&storage), 1);
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creates_independent_conversations_atomically_with_init_and_rolls_back_failures() {
    let storage = Arc::new(ControlledStorage::new());
    let storage_arc: Arc<dyn Storage> = Arc::clone(&storage) as Arc<dyn Storage>;
    let (harness, registry) = open_harness(storage_arc, &["read"]).await;
    let context = harness_support::context();

    let init: ConversationInit = Arc::new(|tx, id| {
        Box::pin(async move {
            tx.append_entry(
                id,
                EntryDraft {
                    key_order: None,
                    kind: "message".to_owned(),
                    model: Some(vec![Message::User(user("seed"))]),
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
            Ok(())
        })
    });
    let created = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: Some(init),
            },
            &*context,
        )
        .await
        .expect("created");
    assert_eq!(commit_count(&storage), 1);
    assert_eq!(
        all_entries(&created, None, &*context).await,
        vec!["seed".to_owned()]
    );

    let before = commit_count(&storage);
    let failing: ConversationInit =
        Arc::new(|_tx, _id| Box::pin(async { Err(js_error("init failed")) }));
    let error = match harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    thinking_level: Some(Some(ModelThinkingLevel::High)),
                    ..Default::default()
                }),
                init: Some(failing),
            },
            &*context,
        )
        .await
    {
        Ok(_) => panic!("init failure rolls back"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("init failed"));
    assert_eq!(commit_count(&storage), before);

    // 默认选择的会话跟随 live install。
    add_tool(&registry, tool("bash"));
    let names: Vec<String> = created
        .agent(&*context)
        .await
        .expect("agent")
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect();
    assert_eq!(names, vec!["read", "bash"]);
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forks_at_a_concrete_entry_with_the_as_of_agent_and_applies_agent_and_init_overrides() {
    let (harness, registry) = open_harness(
        Arc::new(MemoryStorage::new()) as Arc<dyn Storage>,
        &["read"],
    )
    .await;
    let context = harness_support::context();
    let legacy = tool("legacy");
    add_tool(&registry, legacy.clone());
    let root = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    root.configure(
        AgentChange {
            thinking_level: Some(Some(ModelThinkingLevel::Low)),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("low");
    let at = append(&root, "one", &*context).await;
    root.configure(
        AgentChange {
            thinking_level: Some(Some(ModelThinkingLevel::High)),
            tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![tool(
                "read",
            )])),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("high");
    let _ = append(&root, "two", &*context).await;

    let child = root
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("fork");
    let state = harness
        .snapshot(
            &DocTarget::conversation(&kiana_durable::harness::agent::AGENT_DOC, child.id()),
            &*context,
        )
        .await
        .expect("agent")
        .expect("agent");
    assert_eq!(js(&state), serde_json::json!({"thinkingLevel": "low"}));
    assert_eq!(
        all_entries(&child, None, &*context).await,
        vec!["one".to_owned()]
    );

    let saw_minimal = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_minimal);
    let init: ConversationInit = Arc::new(move |tx, id| {
        let flag = Arc::clone(&flag);
        Box::pin(async move {
            let agent = tx
                .doc(&DocTarget::conversation(
                    &kiana_durable::harness::agent::AGENT_DOC,
                    id,
                ))
                .await?;
            flag.store(
                agent.get("thinkingLevel") == Some(&JsonValue::String("minimal".to_owned())),
                Ordering::SeqCst,
            );
            Ok(())
        })
    });
    let overridden = root
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: Some(AgentChange {
                    thinking_level: Some(Some(ModelThinkingLevel::Minimal)),
                    tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![legacy])),
                    ..Default::default()
                }),
                init: Some(init),
            },
            &*context,
        )
        .await
        .expect("overridden fork");
    assert!(saw_minimal.load(Ordering::SeqCst));
    let state = harness
        .snapshot(
            &DocTarget::conversation(&kiana_durable::harness::agent::AGENT_DOC, overridden.id()),
            &*context,
        )
        .await
        .expect("agent")
        .expect("agent");
    assert_eq!(
        js(&state),
        serde_json::json!({"thinkingLevel": "minimal", "tools": ["legacy"]})
    );
    let parent_state = harness
        .snapshot(
            &DocTarget::conversation(&kiana_durable::harness::agent::AGENT_DOC, root.id()),
            &*context,
        )
        .await
        .expect("agent")
        .expect("agent");
    assert_eq!(
        js(&parent_state),
        serde_json::json!({"thinkingLevel": "high", "tools": ["read"]})
    );

    let unrelated = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("unrelated");
    assert!(
        root.fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .is_ok()
    );
    let error = match unrelated
        .fork(
            at.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
    {
        Ok(_) => panic!("the fork is not visible"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("is not visible"), "{error}");
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn paginates_fork_aware_history_through_deep_ancestor_caps() {
    let (harness, _registry) =
        open_harness(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &[]).await;
    let context = harness_support::context();
    let root = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    let _ = append(&root, "r1", &*context).await;
    let r2 = append(&root, "r2", &*context).await;
    let _ = append(&root, "r3", &*context).await;

    let child = root
        .fork(
            r2.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("child");
    let c1 = append(&child, "c1", &*context).await;
    let _ = append(&child, "c2", &*context).await;
    let grandchild = child
        .fork(
            c1.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("grandchild");
    let _ = append(&grandchild, "g1", &*context).await;

    assert_eq!(
        all_entries(&root, None, &*context).await,
        vec!["r3", "r2", "r1"]
    );
    assert_eq!(
        all_entries(&child, None, &*context).await,
        vec!["c2", "c1", "r2", "r1"]
    );
    assert_eq!(
        all_entries(&grandchild, None, &*context).await,
        vec!["g1", "c1", "r2", "r1"]
    );
    assert_eq!(
        all_entries(&grandchild, Some(ScanOrder::Ascending), &*context).await,
        vec!["r1", "r2", "c1", "g1"]
    );

    let bounded = grandchild
        .entries(
            EntryRangeQuery {
                min_entry_id: Some(r2.id),
                max_entry_id: Some(c1.id),
                order: None,
            },
            10,
            None,
            &*context,
        )
        .await
        .expect("bounded");
    let ids: Vec<_> = bounded.items.iter().map(|entry| entry.id).collect();
    assert_eq!(ids, vec![c1.id, r2.id]);
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replaces_whole_agent_fields_clears_them_with_null_and_leaves_undefined_fields_alone() {
    let (harness, registry) =
        open_harness(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &[]).await;
    let context = harness_support::context();
    let read = tool("read");
    let bash = tool("bash");
    let edit = tool("edit");
    registry.install(kiana_durable::harness::define::define_extension(
        TestExtension::new("coding").with_tools(vec![read.clone(), bash.clone(), edit.clone()]),
    ));
    let root = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    let stored = |harness: &Arc<kiana_durable::harness::harness::HarnessImpl>| {
        let harness = Arc::clone(harness);
        let context = Arc::clone(&context);
        let id = root.id();
        async move {
            harness
                .snapshot(
                    &DocTarget::conversation(&kiana_durable::harness::agent::AGENT_DOC, id),
                    &*context,
                )
                .await
                .expect("agent")
                .map(|value| js(&value))
        }
    };
    let offered = |conversation: &Arc<dyn ConversationTrait>| {
        let conversation = Arc::clone(conversation);
        let context = Arc::clone(&context);
        async move {
            conversation
                .agent(&*context)
                .await
                .expect("agent")
                .tools
                .iter()
                .map(|tool| tool.name().to_owned())
                .collect::<Vec<_>>()
        }
    };

    let agent = root.agent(&*context).await.expect("agent");
    assert_eq!(agent.thinking_level, ModelThinkingLevel::Off);
    assert_eq!(
        agent
            .tools
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect::<Vec<_>>(),
        vec!["read", "bash", "edit"]
    );
    assert!(agent.model.is_none());

    root.configure(
        AgentChange {
            model: Some(Some(kiana_durable::harness::types::ModelRef {
                provider: "openai".to_owned(),
                model_id: "gpt".to_owned(),
            })),
            thinking_level: Some(Some(ModelThinkingLevel::Medium)),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("configure");
    root.configure(
        AgentChange {
            instructions: Some(Some("Be terse.".to_owned())),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("configure");
    assert_eq!(
        stored(&harness).await,
        Some(serde_json::json!({
            "model": {"provider": "openai", "modelId": "gpt"},
            "thinkingLevel": "medium",
            "instructions": "Be terse.",
        }))
    );
    root.configure(
        AgentChange {
            model: Some(None),
            instructions: Some(None),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("clear");
    assert_eq!(
        stored(&harness).await,
        Some(serde_json::json!({"thinkingLevel": "medium"}))
    );

    root.configure(
        AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Remove {
                remove: vec![edit.clone()],
            }),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("remove");
    assert_eq!(offered(&root).await, vec!["read", "bash"]);
    // 新过滤器替换旧的：edit 重新提供。
    root.configure(
        AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Remove { remove: vec![bash] }),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("remove");
    assert_eq!(offered(&root).await, vec!["read", "edit"]);
    root.configure(
        AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![
                tool("edit"),
                tool("read"),
            ])),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("set");
    assert_eq!(offered(&root).await, vec!["edit", "read"]);
    root.configure(
        AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Clear),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("clear tools");
    assert_eq!(offered(&root).await, vec!["read", "bash", "edit"]);
    // 名字按原样存储，不查 registry。
    root.configure(
        AgentChange {
            tools: Some(kiana_durable::harness::types::ToolChange::Set(vec![
                tool("missing"),
                tool("read"),
            ])),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("set");
    let stored_value = stored(&harness).await.expect("stored");
    assert_eq!(
        stored_value["tools"],
        serde_json::json!(["missing", "read"])
    );
    assert_eq!(offered(&root).await, vec!["read"]);
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_conversation_created_in_every_creating_commit_after_the_built_ins() {
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let handler: kiana_durable::harness::types::ConversationCreatedHandler =
        Arc::new(move |tx, record| {
            let sink = Arc::clone(&sink);
            let record = record.clone();
            Box::pin(async move {
                let agent = tx
                    .doc(&DocTarget::conversation(
                        &kiana_durable::harness::agent::AGENT_DOC,
                        record.id,
                    ))
                    .await?;
                let cwd = agent
                    .get("cwd")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("undefined")
                    .to_owned();
                sink.lock().unwrap().push(format!(
                    "{}:{}:{cwd}",
                    record.id,
                    if record.parent.is_none() {
                        "new"
                    } else {
                        "fork"
                    }
                ));
                let mut note = tx.doc(&note_target(record.id)).await?;
                if note.get("text").and_then(JsonValue::as_str) != Some("created") {
                    note.insert("text".to_owned(), JsonValue::String("created".to_owned()));
                }
                if agent.get("cwd") == Some(&JsonValue::String("/fail".to_owned())) {
                    return Err(js_error("no"));
                }
                Ok(())
            })
        });
    // 用真正的 conversationCreated 选项打开一个 harness。
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let registry = kiana_durable::harness::registry::create_registry_impl();
    let harness = kiana_durable::harness::harness::Harness::open(
        storage,
        kiana_durable::harness::types::HarnessOptions {
            telemetry: None,
            models: kiana_durable::harness::types::ModelsProvider::Direct(
                kiana_ai::models::create_models(Default::default()),
            ),
            identity: None,
            registry: Arc::clone(&registry)
                as Arc<dyn kiana_durable::harness::types::RegistryReader>,
            settings: None,
            env: None,
            conversation_created: Some(handler),
            now: None,
            set_timer: None,
            on_wake: None,
            on_report: None,
            read_only: None,
        },
        &*harness_support::context(),
    )
    .await
    .expect("harness");
    let context = harness_support::context();

    // `init` 在创建 hook 之后运行：它能看到 hook 写下的 note。
    let init_seen = Arc::clone(&seen);
    let init: kiana_durable::harness::types::ConversationInit = Arc::new(move |tx, id| {
        let init_seen = Arc::clone(&init_seen);
        Box::pin(async move {
            let note = tx.doc(&note_target(id)).await?;
            init_seen.lock().unwrap().push(format!(
                "init:{}",
                note.get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("undefined")
            ));
            Ok(())
        })
    });
    let root = harness
        .root(
            &*context,
            RootOptions {
                agent: Some(AgentChange {
                    cwd: Some(Some("/root".to_owned())),
                    ..Default::default()
                }),
                init: Some(init),
            },
        )
        .await
        .expect("root");
    let raw = harness
        .commit(&RawCreate, &*context, CommitOptions::default())
        .await
        .expect("raw create");
    let raw_id = *raw.downcast::<ConversationId>().expect("an id");
    let entry = append(&root, "hello", &*context).await;
    let fork = root
        .fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("fork");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            format!("{}:new:undefined", root.id()),
            "init:created".to_owned(),
            format!("{raw_id}:new:undefined"),
            format!("{}:fork:/root", fork.id()),
        ]
    );
    let note = harness
        .snapshot(&note_target(raw_id), &*context)
        .await
        .expect("note")
        .expect("note");
    assert_eq!(js(&note), serde_json::json!({"text": "created"}));
    // 创建 hook 抛错会让创建提交失败；更早条目上的 fork 仍用 as-of agent（cwd=/root）。
    root.configure(
        AgentChange {
            cwd: Some(Some("/fail".to_owned())),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("configure");
    let _ = root
        .fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("a fork before the failing configure");
    let failing = append(&root, "after", &*context).await;
    let error = match root
        .fork(
            failing.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
    {
        Ok(_) => panic!("the hook fails the creating commit"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("no"), "{error}");
    harness.close(&*context).await;
}

/// `tx.createConversation()` 的原始创建（TS 的 `harness.commit(async (tx) => ...)`）。
struct RawCreate;

#[async_trait]
impl SessionChange for RawCreate {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .create_conversation(&kiana_durable::types::CreateConversationOptions {
                ownership: ConversationOwnership::Ownerless,
            })
            .await?;
        Ok(Box::new(record.id))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn returns_stateless_handles_and_rejects_operations_after_close() {
    let (harness, _registry) =
        open_harness(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &[]).await;
    let context = harness_support::context();
    let root = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    let again = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    assert!(!Arc::ptr_eq(&root, &again));
    assert_eq!(again.id(), root.id());
    harness.close(&*context).await;
    let error = match harness.root(&*context, RootOptions::default()).await {
        Ok(_) => panic!("the closed harness rejects"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("is closed"), "{error}");
    let error = match harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
    {
        Ok(_) => panic!("the closed harness rejects"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("is closed"), "{error}");
    let error = match harness.conversation(root.id(), &*context).await {
        Ok(_) => panic!("the closed harness rejects"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("is closed"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn forwards_generic_session_document_apis() {
    let (harness, _registry) =
        open_harness(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &[]).await;
    let context = harness_support::context();
    let root = harness
        .root(&*context, RootOptions::default())
        .await
        .expect("root");
    let entry = {
        let change = SetNoteAndAppend {
            conversation_id: root.id(),
            text: "first".to_owned(),
            model_text: "m".to_owned(),
        };
        let value = root
            .commit(&change, &*context, CommitOptions::default())
            .await
            .expect("commit");
        *value.downcast::<EntryRecord>().expect("entry")
    };
    harness
        .commit(
            &SetNote {
                conversation_id: root.id(),
                text: "second".to_owned(),
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("commit");
    let snapshot = harness
        .snapshot(&note_target(root.id()), &*context)
        .await
        .expect("snapshot")
        .expect("note");
    assert_eq!(js(&snapshot), serde_json::json!({"text": "second"}));
    let as_of = harness
        .snapshot_as_of(&note_target(root.id()), entry.id, &*context)
        .await
        .expect("as of")
        .expect("note");
    assert_eq!(js(&as_of), serde_json::json!({"text": "first"}));
    let watch = harness
        .watch_doc(&note_target(root.id()), &*context)
        .await
        .expect("watch")
        .expect("watch");
    assert_eq!(
        watch.value().as_ref().map(js),
        Some(serde_json::json!({"text": "second"}))
    );
    let _ = watch.stop().await;
    harness.close(&*context).await;
}

struct SetNote {
    conversation_id: ConversationId,
    text: String,
}

#[async_trait]
impl SessionChange for SetNote {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut note = tx.doc(&note_target(self.conversation_id)).await?;
        note.insert("text".to_owned(), JsonValue::String(self.text.clone()));
        Ok(Box::new(()))
    }
}

struct SetNoteAndAppend {
    conversation_id: ConversationId,
    text: String,
    model_text: String,
}

#[async_trait]
impl SessionChange for SetNoteAndAppend {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let mut note = tx.doc(&note_target(self.conversation_id)).await?;
        note.insert("text".to_owned(), JsonValue::String(self.text.clone()));
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "message".to_owned(),
                    model: Some(vec![Message::User(user(&self.model_text))]),
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

// ---------------------------------------------------------------------------
// 其余 TS 用例：提交绑定、Tx 创建的文档、缺失 agent 的读取
// ---------------------------------------------------------------------------

struct NeverPhase;

#[async_trait]
impl kiana_durable::types::TaskPhase<JsonValue, JsonValue, JsonValue, ()> for NeverPhase {
    async fn run(
        &self,
        _task: &kiana_durable::types::TaskRecord,
        _runtime: &dyn kiana_durable::types::TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

fn never_task(name: &str) -> kiana_durable::harness::types::AnyTask {
    use indexmap::IndexMap;
    use std::sync::Arc as StdArc;
    let definition = kiana_durable::types::TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: StdArc::new(|_| serde_json::json!({"phase": "never"})),
        phases: IndexMap::from([(
            "never".to_owned(),
            StdArc::new(NeverPhase)
                as StdArc<dyn kiana_durable::types::TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: StdArc::new(NeverPhase)
            as StdArc<dyn kiana_durable::types::TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

#[tokio::test(flavor = "multi_thread")]
async fn binds_commits_and_task_creation_to_the_conversation() {
    let (harness, _registry) =
        open_harness(Arc::new(MemoryStorage::new()) as Arc<dyn Storage>, &[]).await;
    let context = harness_support::context();
    let conversation = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &*context,
        )
        .await
        .expect("conversation");
    let task = never_task("test.work");
    struct CreateWork {
        task: kiana_durable::harness::types::AnyTask,
        arguments: JsonValue,
    }
    #[async_trait]
    impl SessionChange for CreateWork {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let options = kiana_durable::types::TaskOptions::new(
                kiana_durable::types::TaskOwnership::Conversation,
            );
            let id = tx
                .create_task(&self.task, self.arguments.clone(), options)
                .await?;
            Ok(Box::new(id))
        }
    }
    // conversation.commit() 把任务绑定到该会话。
    let value = conversation
        .commit(
            &CreateWork {
                task: task.clone(),
                arguments: serde_json::json!({"n": 1}),
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("the bound commit");
    let task_id = *value
        .downcast::<kiana_durable::types::TaskId>()
        .expect("task id");
    let record = harness
        .commit(
            &ReadTask { id: task_id },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("read");
    let record = record
        .downcast::<Option<kiana_durable::types::TaskRecord>>()
        .expect("record")
        .expect("the task");
    assert_eq!(record.conversation_id, conversation.id());
    // Harness 上的提交没有会话绑定：创建会话拥有的任务会失败。
    let error = match harness
        .commit(
            &CreateWork {
                task,
                arguments: serde_json::json!({"n": 2}),
            },
            &*context,
            CommitOptions::default(),
        )
        .await
    {
        Ok(_) => panic!("the task needs a conversation"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires options.conversationId"),
        "{error}"
    );
    harness.close(&*context).await;
}

struct ReadTask {
    id: kiana_durable::types::TaskId,
}

#[async_trait]
impl SessionChange for ReadTask {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        Ok(Box::new(tx.task(self.id).await?))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_an_absent_agent_for_conversations_a_plain_session_created_without_writing() {
    let storage = Arc::new(ControlledStorage::new());
    let session = kiana_durable::session::session::create_session(Arc::clone(&storage));
    let context = harness_support::context();
    struct CreatePlain;
    #[async_trait]
    impl SessionChange for CreatePlain {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let record = tx
                .create_conversation(&kiana_durable::types::CreateConversationOptions {
                    ownership: ConversationOwnership::Ownerless,
                })
                .await?;
            Ok(Box::new(record.id))
        }
    }
    let value = session
        .commit(&CreatePlain, &*context, CommitOptions::default())
        .await
        .expect("create");
    let id = *value.downcast::<ConversationId>().expect("id");
    let (harness, _registry) = open_harness(storage.clone() as Arc<dyn Storage>, &["read"]).await;
    let raw = harness
        .conversation(id, &*context)
        .await
        .expect("read the conversation")
        .expect("it exists");
    assert!(
        harness
            .snapshot(
                &DocTarget::conversation(&kiana_durable::harness::live::LIVE_DOC, id),
                &*context,
            )
            .await
            .expect("live")
            .is_none()
    );
    let commits = storage.commits_len();
    let agent = raw.agent(&*context).await.expect("the resolved agent");
    assert_eq!(
        agent.thinking_level,
        kiana_ai::types::ModelThinkingLevel::Off
    );
    assert_eq!(
        agent
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect::<Vec<_>>(),
        vec!["read"]
    );
    assert_eq!(storage.commits_len(), commits, "reading commits nothing");
    raw.configure(
        AgentChange {
            thinking_level: Some(Some(kiana_ai::types::ModelThinkingLevel::Low)),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("configure");
    let state = harness
        .snapshot(
            &DocTarget::conversation(&kiana_durable::harness::agent::AGENT_DOC, id),
            &*context,
        )
        .await
        .expect("agent doc")
        .expect("agent doc exists");
    assert_eq!(js(&state), serde_json::json!({"thinkingLevel": "low"}));
    harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_conversations_created_through_tx_their_documents() {
    use kiana_durable::harness::agent::{AGENT_DOC, configure};
    use kiana_durable::harness::live::LIVE_DOC;
    use kiana_durable::harness::provider::PROVIDER_DOC;
    use kiana_durable::types::TaskOptions;

    let (harness, _registry) = open_harness(
        Arc::new(MemoryStorage::new()) as Arc<dyn Storage>,
        &["read"],
    )
    .await;
    let context = harness_support::context();
    let model = kiana_durable::harness::types::ModelRef {
        provider: "faux".to_owned(),
        model_id: "m".to_owned(),
    };
    let root = harness
        .root(
            &*context,
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(model.clone())),
                    instructions: Some(Some("Main role.".to_owned())),
                    cwd: Some(Some("/repo".to_owned())),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await
        .expect("root");
    struct CreateThroughTx {
        task: kiana_durable::harness::types::AnyTask,
        owner_agent_cwd: Arc<std::sync::Mutex<Option<String>>>,
    }
    #[async_trait]
    impl SessionChange for CreateThroughTx {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let task_id = tx
                .create_task(
                    &self.task,
                    JsonValue::Object(Default::default()),
                    TaskOptions::new(kiana_durable::types::TaskOwnership::Conversation),
                )
                .await?;
            let plain = tx
                .create_conversation(&kiana_durable::types::CreateConversationOptions {
                    ownership: ConversationOwnership::Ownerless,
                })
                .await?;
            let owned = tx
                .create_conversation(&kiana_durable::types::CreateConversationOptions {
                    ownership: ConversationOwnership::Task { task_id },
                })
                .await?;
            // 任务拥有的会话在 createConversation() 返回时已有 owner 的 agent 副本。
            let agent = tx
                .doc(&DocTarget::conversation(&AGENT_DOC, owned.id))
                .await?;
            *self.owner_agent_cwd.lock().unwrap() = agent
                .get("cwd")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            configure(
                tx,
                owned.id,
                &AgentChange {
                    cwd: Some(Some("/worktree".to_owned())),
                    ..Default::default()
                },
            )
            .await?;
            Ok(Box::new((task_id, plain.id, owned.id)))
        }
    }
    let owner_cwd = Arc::new(std::sync::Mutex::new(None));
    let value = root
        .commit(
            &CreateThroughTx {
                task: never_task("test.owner"),
                owner_agent_cwd: Arc::clone(&owner_cwd),
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("create through tx");
    assert_eq!(owner_cwd.lock().unwrap().as_deref(), Some("/repo"));
    let (task_id, plain, owned) = *value
        .downcast::<(kiana_durable::types::TaskId, ConversationId, ConversationId)>()
        .expect("ids");
    let snapshot = |id, token: &'static DocToken| {
        let harness = Arc::clone(&harness);
        let context = harness_support::context();
        async move {
            harness
                .snapshot(&DocTarget::conversation(token, id), &*context)
                .await
                .expect("snapshot")
        }
    };
    let plain_agent = snapshot(plain, &AGENT_DOC).await.expect("plain agent");
    assert_eq!(js(&plain_agent), serde_json::json!({}));
    let plain_live = snapshot(plain, &LIVE_DOC).await.expect("plain live");
    assert_eq!(js(&plain_live), serde_json::json!({}));
    let plain_provider = snapshot(plain, &PROVIDER_DOC)
        .await
        .expect("plain provider");
    let plain_session = plain_provider
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .expect("a session id")
        .to_owned();
    assert!(is_uuid_v7(&plain_session), "{plain_session}");
    let root_provider = snapshot(root.id(), &PROVIDER_DOC)
        .await
        .expect("root provider");
    assert_ne!(
        Some(plain_session.as_str()),
        root_provider.get("sessionId").and_then(JsonValue::as_str)
    );
    let owned_provider = snapshot(owned, &PROVIDER_DOC)
        .await
        .expect("owned provider");
    assert_ne!(
        owned_provider.get("sessionId").and_then(JsonValue::as_str),
        root_provider.get("sessionId").and_then(JsonValue::as_str)
    );
    let owned_agent = snapshot(owned, &AGENT_DOC).await.expect("owned agent");
    assert_eq!(
        js(&owned_agent),
        serde_json::json!({
            "model": {"provider": "faux", "modelId": "m"},
            "instructions": "Main role.",
            "cwd": "/worktree"
        })
    );
    // 后来的 owner 变更不会传到子会话。
    root.configure(
        AgentChange {
            thinking_level: Some(Some(ModelThinkingLevel::High)),
            ..Default::default()
        },
        &*context,
    )
    .await
    .expect("configure root");
    let owned_agent = snapshot(owned, &AGENT_DOC).await.expect("owned agent");
    assert!(owned_agent.get("thinkingLevel").is_none());
    // 任务拥有的 fork 保留 fork 父的 as-of 副本，而不是 owner 的 agent。
    let at = root
        .commit(
            &AppendNoteTo {
                conversation_id: plain,
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("append");
    let at = *at.downcast::<EntryRecord>().expect("entry");
    struct ForkPlain {
        parent: ConversationId,
        at: kiana_durable::types::EntryId,
        task_id: kiana_durable::types::TaskId,
    }
    #[async_trait]
    impl SessionChange for ForkPlain {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
            let record = tx
                .fork_conversation(
                    self.parent,
                    self.at,
                    &kiana_durable::types::CreateConversationOptions {
                        ownership: ConversationOwnership::Task {
                            task_id: self.task_id,
                        },
                    },
                )
                .await?;
            Ok(Box::new(record.id))
        }
    }
    let value = root
        .commit(
            &ForkPlain {
                parent: plain,
                at: at.id,
                task_id,
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("fork");
    let fork = *value.downcast::<ConversationId>().expect("fork id");
    let fork_agent = snapshot(fork, &AGENT_DOC).await.expect("fork agent");
    assert_eq!(js(&fork_agent), serde_json::json!({}));
    let fork_live = snapshot(fork, &LIVE_DOC).await.expect("fork live");
    assert_eq!(js(&fork_live), serde_json::json!({}));
    let fork_provider = snapshot(fork, &PROVIDER_DOC).await.expect("fork provider");
    assert_ne!(
        fork_provider.get("sessionId").and_then(JsonValue::as_str),
        Some(plain_session.as_str())
    );
    harness.close(&*context).await;
}

struct AppendNoteTo {
    conversation_id: ConversationId,
}

#[async_trait]
impl SessionChange for AppendNoteTo {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let record = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(record))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_root_and_conversation_identity_and_state_across_reopen() {
    use kiana_durable::harness::types::ModelRef;
    use kiana_durable::types::id_from_number;

    async fn provider_session(
        harness: &Arc<kiana_durable::harness::harness::HarnessImpl>,
        id: ConversationId,
        context: &dyn Context,
    ) -> String {
        let document = harness
            .snapshot(
                &DocTarget::conversation(&kiana_durable::harness::provider::PROVIDER_DOC, id),
                context,
            )
            .await
            .expect("provider snapshot")
            .expect("the provider document exists");
        document
            .get("sessionId")
            .and_then(JsonValue::as_str)
            .expect("a session id")
            .to_owned()
    }

    let storage = Arc::new(support::MemoryStorage::new());
    let context_ref = harness_support::context();
    let context = &*context_ref;
    let (first, _registry) =
        open_harness(Arc::clone(&storage) as Arc<dyn Storage>, &["read"]).await;
    let root = first
        .root(context, RootOptions::default())
        .await
        .expect("the root");
    root.configure(
        AgentChange {
            model: Some(Some(ModelRef {
                provider: "anthropic".to_owned(),
                model_id: "claude".to_owned(),
            })),
            cwd: Some(Some("/repo".to_owned())),
            ..Default::default()
        },
        context,
    )
    .await
    .expect("configure");
    let entry = append(&root, "hello", context).await;
    let child = first
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context,
        )
        .await
        .expect("create the child");
    let fork = root
        .fork(
            entry.id,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context,
        )
        .await
        .expect("fork");
    let provider_session_ids = vec![
        provider_session(&first, root.id(), context).await,
        provider_session(&first, child.id(), context).await,
        provider_session(&first, fork.id(), context).await,
    ];
    // 回归 #10424：fork 不能继承父会话的 provider 身份。
    for session_id in &provider_session_ids {
        assert!(is_uuid_v7(session_id), "{session_id}");
    }
    let mut unique = provider_session_ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        3,
        "each conversation has its own provider identity"
    );
    first.close(context).await;
    assert!(
        root.agent(context).await.is_err(),
        "the closed harness refuses"
    );

    // 新进程什么都不安装：存下来的选择还在，工具不再解析。
    storage.reopen();
    let (second, _registry) = open_harness(Arc::clone(&storage) as Arc<dyn Storage>, &[]).await;
    let init: kiana_durable::harness::types::ConversationInit = Arc::new(|_, _| {
        Box::pin(async { panic!("init must not run when the root already exists") })
    });
    let reopened = second
        .root(
            context,
            RootOptions {
                agent: None,
                init: Some(init),
            },
        )
        .await
        .expect("reopen the root");
    assert_eq!(reopened.id(), ROOT_CONVERSATION_ID);
    let agent = reopened.agent(context).await.expect("the stored agent");
    assert_eq!(
        agent.model,
        Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned(),
        })
    );
    assert_eq!(agent.cwd.as_deref(), Some("/repo"));
    assert!(
        agent.tools.is_empty(),
        "{:?}",
        agent
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        all_entries(&reopened, None, context).await,
        vec!["hello".to_owned()]
    );
    let read_child = second
        .conversation(child.id(), context)
        .await
        .expect("read the child")
        .expect("the child exists");
    assert_eq!(read_child.id(), child.id());
    let reopened_ids = vec![
        provider_session(&second, root.id(), context).await,
        provider_session(&second, child.id(), context).await,
        provider_session(&second, fork.id(), context).await,
    ];
    assert_eq!(reopened_ids, provider_session_ids);
    let reopened_fork = second
        .conversation(fork.id(), context)
        .await
        .expect("read the fork")
        .expect("the fork exists");
    assert_eq!(
        all_entries(&reopened_fork, None, context).await,
        vec!["hello".to_owned()]
    );
    let fork_agent = reopened_fork.agent(context).await.expect("the fork agent");
    assert_eq!(
        fork_agent.model,
        Some(ModelRef {
            provider: "anthropic".to_owned(),
            model_id: "claude".to_owned(),
        })
    );
    assert!(
        second
            .conversation(id_from_number(999), context)
            .await
            .expect("read a missing conversation")
            .is_none()
    );
    second.close(context).await;
}
