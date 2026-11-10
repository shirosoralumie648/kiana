//! `test/harness-compaction.test.ts` 的移植（阶段级子集）。
//!
//! TS 通过完整 `Harness` 驱动 compaction；本工作副本里 scheduler 仍是骨架，因此这里用
//! [`harness_runtime::TestRuntime`] 直接驱动 `SelectPhase` / `SummarizePhase`，覆盖不依赖调度器
//! 的行为：
//! - `beforeCompact` 抛错按 `extension_error` 上报并跳过，后面的处理器仍可给出决定；
//! - 摘要请求永不携带 `deferred`（TS `const { deferred: _deferred, ...forwarded }`），因此
//!   `settings.stream.deferred` 不会把摘要变成延迟响应。
//!
//! 依赖完整 Harness 的用例（阈值/溢出压缩编排、place 的提交接纳、失败重试）见交付报告。

mod harness_runtime;
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kiana_ai::models::{CreateModelsOptions, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
    faux_provider_with,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, ModelThinkingLevel,
    StopReason, TextContent, TextKind, UserContent, UserMessage, UserRole,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::compaction::{
    CompactionCheckpoint, CompactionInput, SelectPhase, SummarizePhase, SummaryRequest,
};
use kiana_durable::harness::types::ConversationStreamOptions;
use kiana_durable::harness::types::{
    Agent, BeforeCompactDecision, BeforeCompactInput, CompactionHooks, CompactionReason,
    CompactionSettings, CompactionSummary, Deferred, HarnessSettings, HookApi, ModelRef,
    ReportFrom, Settings,
};
use kiana_durable::types::{
    CompactionEntry, ConversationId, JsonValue, TaskOutcome, TaskPhase, TaskRecord, TaskState,
};
use kiana_durable::{AssistantEntry, NoData, TypedEntryDraft};

use harness_runtime::{Report, TestHookRunner, TestRuntime};
use support::{commit, create_conversation, open_test_session};

fn ctx() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn settings(keep_recent_tokens: f64, deferred: bool) -> Arc<Settings> {
    Arc::new(resolve_settings(Some(&HarnessSettings {
        compaction: Some(CompactionSettings {
            keep_recent_tokens: Some(keep_recent_tokens),
            ..Default::default()
        }),
        stream: Some(ConversationStreamOptions {
            deferred: deferred.then_some(Deferred::Enabled(true)),
            timeout_ms: Some(1234.0),
            ..Default::default()
        }),
        ..Default::default()
    })))
}

fn agent(reference: &ModelRef) -> Arc<Agent> {
    Arc::new(Agent {
        model: Some(reference.clone()),
        thinking_level: ModelThinkingLevel::default(),
        extensions: Vec::new(),
        tools: Vec::new(),
        callable: Vec::new(),
        deferred: Vec::new(),
        sections: Vec::new(),
        instructions: None,
        cwd: None,
    })
}

fn compaction_record(
    conversation_id: ConversationId,
    input: CompactionInput,
    checkpoint: CompactionCheckpoint,
) -> TaskRecord {
    TaskRecord {
        id: kiana_durable::id_from_number(2000),
        conversation_id,
        kind: kiana_durable::harness::live::COMPACTION_TASK_KIND.to_owned(),
        version: 1,
        input: serde_json::to_value(input).expect("compaction input"),
        trace: None,
        links: None,
        // 由 generation 拥有的压缩直接追加摘要条目（TS `current.owner !== undefined`）。
        owner: Some(kiana_durable::id_from_number(1999)),
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Running {
            checkpoint: serde_json::to_value(checkpoint).expect("checkpoint"),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    }
}

async fn append_user(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
    text: &str,
) -> kiana_durable::EntryId {
    let text = text.to_owned();
    commit(session, move |tx| {
        Box::pin(async move {
            let draft = kiana_durable::UserEntry.typed_draft(TypedEntryDraft {
                model: Some(vec![Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text(text),
                    timestamp: 1,
                })]),
                data: NoData,
                head: None,
                edits: None,
            })?;
            Ok(tx.append_entry(conversation_id, draft).await?.id)
        })
    })
    .await
    .expect("user entry appends")
}

async fn append_assistant(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
    text: &str,
) -> kiana_durable::EntryId {
    let text = text.to_owned();
    commit(session, move |tx| {
        Box::pin(async move {
            let draft = AssistantEntry.typed_draft(TypedEntryDraft {
                model: Some(vec![Message::Assistant(AssistantMessage {
                    role: AssistantRole::Assistant,
                    content: vec![AssistantContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text,
                        text_signature: None,
                    })],
                    stop_reason: StopReason::Stop,
                    timestamp: 2,
                    ..Default::default()
                })]),
                data: NoData,
                head: None,
                edits: None,
            })?;
            Ok(tx.append_entry(conversation_id, draft).await?.id)
        })
    })
    .await
    .expect("assistant entry appends")
}

/// 可配置的 `beforeCompact` 探针。
struct ProbeCompactionHook {
    error: Option<&'static str>,
    decision: Option<BeforeCompactDecision>,
    calls: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Option<BeforeCompactInput>>>,
}

#[async_trait]
impl CompactionHooks for ProbeCompactionHook {
    async fn before_compact(
        &self,
        compaction: BeforeCompactInput,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<BeforeCompactDecision>, BoxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.seen.lock().expect("seen") = Some(compaction);
        if let Some(error) = self.error {
            return Err(error.into());
        }
        Ok(self.decision.clone())
    }
}

/// 会话历史：u1, a1, u2, a2（返回全部条目 id）。
async fn history(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
) -> Vec<kiana_durable::EntryId> {
    vec![
        append_user(session, conversation_id, "u1").await,
        append_assistant(session, conversation_id, "a1").await,
        append_user(session, conversation_id, "u2").await,
        append_assistant(session, conversation_id, "a2").await,
    ]
}

#[tokio::test]
async fn a_throwing_before_compact_hook_is_reported_and_the_next_decision_wins() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let history = history(&session, conversation_id).await;

    let faux = faux_provider_with(RegisterFauxProviderOptions::default());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };

    let throwing = Arc::new(ProbeCompactionHook {
        error: Some("hook broke"),
        decision: None,
        calls: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(std::sync::Mutex::new(None)),
    });
    let deciding = Arc::new(ProbeCompactionHook {
        error: None,
        decision: Some(BeforeCompactDecision::Summary(CompactionSummary {
            summary: "FROM HOOK".to_owned(),
        })),
        calls: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(std::sync::Mutex::new(None)),
    });
    let declining = Arc::new(ProbeCompactionHook {
        error: None,
        decision: Some(BeforeCompactDecision::Decline(
            kiana_durable::harness::types::DeclineCompaction { decline: true },
        )),
        calls: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(std::sync::Mutex::new(None)),
    });
    let hooks = TestHookRunner::<dyn CompactionHooks>::new()
        .add(
            "beforeCompact",
            Some("ext.a"),
            Arc::clone(&throwing) as Arc<dyn CompactionHooks>,
        )
        .add(
            "beforeCompact",
            Some("ext.b"),
            Arc::clone(&deciding) as Arc<dyn CompactionHooks>,
        )
        .add(
            "beforeCompact",
            Some("ext.c"),
            Arc::clone(&declining) as Arc<dyn CompactionHooks>,
        );

    let record = compaction_record(
        conversation_id,
        CompactionInput {
            reason: CompactionReason::Manual,
            instructions: Some("why".to_owned()),
        },
        CompactionCheckpoint::Select,
    );
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(1.0, false),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference),
        Arc::new(hooks),
        1_000,
    );

    SelectPhase
        .run(&record, &runtime, ctx())
        .await
        .expect("a throwing hook must not fail the compaction");

    // 第一个决定之后，后面的处理器不再被调用。
    assert_eq!(throwing.calls.load(Ordering::SeqCst), 1);
    assert_eq!(deciding.calls.load(Ordering::SeqCst), 1);
    assert_eq!(declining.calls.load(Ordering::SeqCst), 0);

    // 抛错按 `extension_error` 上报并带扩展名。
    assert_eq!(
        runtime.reports(),
        vec![Report {
            message: "hook broke".to_owned(),
            from: Some(ReportFrom::Extension {
                extension: Some("ext.a".to_owned()),
            }),
        }]
    );

    // 决定收到了 TS 的 `compaction` 输入。
    let seen = deciding
        .seen
        .lock()
        .expect("seen")
        .clone()
        .expect("the deciding hook saw the compaction");
    assert_eq!(seen.reason, CompactionReason::Manual);
    assert_eq!(seen.instructions.as_deref(), Some("why"));
    assert_eq!(seen.first_kept, history[3]);
    assert_eq!(
        seen.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        history[..3].to_vec()
    );

    // hook 的摘要被放置成 `kiana.compaction` 条目，并结束任务。
    let entries = harness_runtime::conversation_entries(
        &runtime.session,
        &*runtime.storage,
        conversation_id,
        ctx(),
    )
    .await;
    let compaction = entries
        .iter()
        .find(|entry| entry.kind == CompactionEntry.kind.as_ref())
        .expect("the summary entry is appended");
    let text = match &compaction.model.as_ref().expect("model")[0] {
        Message::User(user) => match &user.content {
            UserContent::Blocks(blocks) => match &blocks[0] {
                kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
                _ => panic!("expected text"),
            },
            _ => panic!("expected blocks"),
        },
        _ => panic!("expected user message"),
    };
    assert!(text.contains("<summary>\nFROM HOOK\n</summary>"), "{text}");
    assert_eq!(compaction.head, Some(history[3]));
    assert!(matches!(
        runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { result }) if result.get("entryId").and_then(JsonValue::as_i64) == Some(compaction.id.get())
    ));
}

#[tokio::test]
async fn summary_requests_never_forward_deferred() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let history = history(&session, conversation_id).await;

    let faux = faux_provider_with(RegisterFauxProviderOptions::default());
    // 若请求带 `deferred`，faux 会返回 stopReason "deferred" 的占位消息（摘要因此失败）。
    faux.set_responses(vec![
        faux_assistant_message("the summary", FauxAssistantMessageOptions::default()).into(),
    ]);
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };

    let request = SummaryRequest {
        attempt: 1,
        model: reference.clone(),
        thinking_level: ModelThinkingLevel::default(),
        // 设置里打开了 deferred，并被 select 原样放进请求。
        stream_options: ConversationStreamOptions {
            deferred: Some(Deferred::Enabled(true)),
            timeout_ms: Some(1234.0),
            ..Default::default()
        },
        max_tokens: 1_000.0,
        tail: history[3],
        first_kept: history[1],
    };
    let record = compaction_record(
        conversation_id,
        CompactionInput {
            reason: CompactionReason::Manual,
            instructions: None,
        },
        CompactionCheckpoint::Summarize(request),
    );
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(1.0, true),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference),
        Arc::new(TestHookRunner::<dyn CompactionHooks>::new()),
        1_000,
    );

    SummarizePhase
        .run(&record, &runtime, ctx())
        .await
        .expect("the summary request must complete synchronously");
    // faux 只被调用一次，并且没有进入延迟兑换路径。
    assert_eq!(faux.state().call_count(), 1);
    assert_eq!(faux.state().deferred_fetch_count(), 0);
    let entries = harness_runtime::conversation_entries(
        &runtime.session,
        &*runtime.storage,
        conversation_id,
        ctx(),
    )
    .await;
    let compaction = entries
        .iter()
        .find(|entry| entry.kind == CompactionEntry.kind.as_ref())
        .expect("the summary entry is appended");
    assert_eq!(compaction.head, Some(history[1]));
    assert!(matches!(
        runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { .. })
    ));
}
