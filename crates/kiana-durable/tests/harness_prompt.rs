//! `test/harness-prompt.test.ts` 的 Rust 端口。
//!
//! TS 测试通过 `Harness`/`Conversation` 追加 `kiana.system` 条目；本单元的上层 `harness.rs` 尚未
//! 实现，因此这里直接用 [`SessionImpl`] 提交同一批条目，并复用 [`read_context`] 与
//! [`plan_system_entries`]/[`render_sections`]/[`replay_sections`] 的派生逻辑。

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::utils::transcript::{get_current_tools, to_tool_declaration};
use kiana_ai::{Message, SystemMessage, Tool, UserContent, UserMessage, UserRole};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::context::read_context;
use kiana_durable::harness::prompt::{
    SystemDraft, plan_system_entries, render_sections, replay_sections,
};
use kiana_durable::harness::types::{Agent, ContextView, PromptInput, PromptSection};
use kiana_durable::session::SessionImpl;
use kiana_durable::types::{DocTarget, DocumentReader, EntryDraft, EntryHead, EntryId, JsonObject};
use kiana_durable::{
    BoxError, ConversationId, EntryRecord, Storage, SystemEntry, TypedEntryTx, UserEntry,
    id_from_number,
};
use support::{commit, create_conversation, open_test_session};

struct Fixture {
    storage: Arc<support::ControlledStorage>,
    session: SessionImpl,
    conversation: ConversationId,
}

impl Fixture {
    async fn open() -> Self {
        let (storage, session, _publications) = open_test_session();
        let conversation = create_conversation(&session).await;
        Fixture {
            storage,
            session,
            conversation,
        }
    }

    async fn append(&self, draft: EntryDraft) -> EntryRecord {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            let draft = draft.clone();
            Box::pin(async move { transaction.append_entry(conversation, draft).await })
        })
        .await
        .expect("append succeeds")
    }

    async fn append_system(&self, draft: SystemDraft) -> EntryRecord {
        let conversation = self.conversation;
        commit(&self.session, move |transaction| {
            let draft = draft.clone();
            Box::pin(async move {
                Ok(transaction
                    .append_entry_typed(&SystemEntry, conversation, draft)
                    .await?
                    .record)
            })
        })
        .await
        .expect("system append succeeds")
    }

    async fn view(&self) -> ContextView {
        read_context(
            &self.session,
            &*self.storage as &dyn Storage,
            self.conversation,
            &**BACKGROUND_CONTEXT,
            None,
        )
        .await
        .expect("context read succeeds")
    }

    async fn marker(&self, head: EntryHead) -> EntryId {
        self.append(EntryDraft {
            key_order: None,
            kind: "summary".to_owned(),
            model: Some(vec![user("summary")]),
            data: None,
            head: Some(head),
            edits: None,
        })
        .await
        .id
    }

    async fn last_system_id(&self) -> EntryId {
        self.view()
            .await
            .entries
            .iter()
            .rfind(|entry| entry.kind == "kiana.system")
            .expect("a system entry exists")
            .id
    }

    /// 对当前上下文做计划、追加计划、检查重放得到 `desired`，并返回每条计划的分段与 omit。
    async fn apply(&self, desired: &[(&str, &str)], tools: &[Tool]) -> Vec<Planned> {
        let view = self.view().await;
        let desired_map: IndexMap<String, String> = desired
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let drafts = plan_system_entries(&view, &desired_map, tools, 7);
        for draft in &drafts {
            self.append_system(draft.clone()).await;
        }
        let replayed = replay_sections(&self.view().await.messages);
        assert_eq!(
            replayed
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>(),
            desired_map
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        );
        drafts.iter().map(planned_of).collect()
    }

    /// 只计划工具，追加计划，检查重放提供的工具，并返回每条消息的变化。
    async fn apply_tools(&self, tools: &[Tool], sections: &[(&str, &str)]) -> Vec<ToolPlan> {
        let view = self.view().await;
        let desired: IndexMap<String, String> = sections
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let drafts = plan_system_entries(&view, &desired, tools, 7);
        for draft in &drafts {
            self.append_system(draft.clone()).await;
        }
        let offered = get_current_tools(&self.view().await.messages);
        assert_eq!(
            offered,
            tools.iter().map(to_tool_declaration).collect::<Vec<_>>()
        );
        drafts.iter().map(tool_plan_of).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Planned {
    sections: IndexMap<String, Option<String>>,
    omit: Option<Vec<EntryId>>,
}

#[derive(Debug, Clone, PartialEq)]
struct ToolPlan {
    removed: Option<Vec<String>>,
    added: Option<Vec<String>>,
    sections: Option<IndexMap<String, Option<String>>>,
}

fn system_message(draft: &SystemDraft) -> &SystemMessage {
    let Message::System(message) = &draft.model.as_ref().expect("a model message")[0] else {
        panic!("expected a system message");
    };
    message
}

fn planned_of(draft: &SystemDraft) -> Planned {
    let message = system_message(draft);
    Planned {
        sections: message.sections.clone().unwrap_or_default(),
        omit: draft
            .edits
            .as_ref()
            .map(|edits| edits.iter().map(|edit| edit.target).collect()),
    }
}

fn tool_plan_of(draft: &SystemDraft) -> ToolPlan {
    let message = system_message(draft);
    ToolPlan {
        removed: message
            .tools_removed
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect()),
        added: message
            .tools_added
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect()),
        sections: message.sections.clone(),
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::default(),
        content: UserContent::Text(text.to_owned()),
        timestamp: 0,
    })
}

fn declaration(name: &str) -> Tool {
    declaration_with(name, name)
}

fn declaration_with(name: &str, description: &str) -> Tool {
    Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
    }
}

fn sections_map(entries: &[(&str, Option<&str>)]) -> IndexMap<String, Option<String>> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
        .collect()
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

struct NoDocuments;

#[async_trait]
impl DocumentReader for NoDocuments {
    async fn snapshot(
        &self,
        _target: &DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        Ok(None)
    }

    async fn snapshot_as_of(
        &self,
        _target: &DocTarget,
        _at: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        Ok(None)
    }
}

fn prompt_input() -> PromptInput {
    PromptInput {
        conversation_id: id_from_number(1),
        agent: Arc::new(Agent {
            model: None,
            thinking_level: Default::default(),
            extensions: Vec::new(),
            tools: Vec::new(),
            callable: Vec::new(),
            deferred: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        }),
        env: None,
        shown: IndexMap::new(),
        read: Arc::new(NoDocuments),
    }
}

struct AppendGit {
    inner: Arc<dyn PromptSection>,
}

#[async_trait]
impl PromptSection for AppendGit {
    fn key(&self) -> &str {
        self.inner.key()
    }

    async fn render(
        &self,
        input: &PromptInput,
        context: &dyn Context,
    ) -> Result<Option<String>, BoxError> {
        let rendered = self.inner.render(input, context).await?.unwrap_or_default();
        Ok(Some(format!("{rendered} (git)")))
    }

    fn tag(&self) -> Option<bool> {
        self.inner.tag()
    }
}

type Render = Arc<
    dyn Fn(
            &PromptInput,
            &dyn Context,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<String>, BoxError>> + Send>,
        > + Send
        + Sync,
>;

fn render_of(text: &'static str) -> Render {
    Arc::new(move |_, _| Box::pin(async move { Ok(Some(text.to_owned())) }))
}

fn failing(text: &'static str) -> Render {
    Arc::new(move |_, _| Box::pin(async move { Err(text.into()) }))
}

#[tokio::test]
async fn renders_sections_in_order_with_tags_omissions_wrappers_and_failures() {
    let preamble = kiana_durable::harness::define::section(
        "preamble",
        render_of("You are helpful."),
        Some(Some(false)),
    );
    let cwd_inner = kiana_durable::harness::define::section("cwd", render_of("/repo"), None);
    let cwd: Arc<dyn PromptSection> = Arc::new(AppendGit { inner: cwd_inner });
    let skipped = kiana_durable::harness::define::section(
        "skipped",
        Arc::new(|_, _| Box::pin(async { Ok(None) })),
        None,
    );
    let failing_section =
        kiana_durable::harness::define::section("failing", failing("render failed"), None);
    let new_failing =
        kiana_durable::harness::define::section("new-failing", failing("also failed"), None);
    let sections = vec![preamble, cwd, skipped, failing_section, new_failing];

    let mut shown = IndexMap::new();
    shown.insert(
        "failing".to_owned(),
        "<failing>\nold\n</failing>".to_owned(),
    );
    shown.insert("cwd".to_owned(), "stale".to_owned());
    let input = prompt_input();
    let mut reports: Vec<String> = Vec::new();
    let desired = render_sections(
        &sections,
        &input,
        &shown,
        &mut |error, _section| reports.push(error.to_string()),
        &**BACKGROUND_CONTEXT,
    )
    .await
    .expect("rendering succeeds");
    assert_eq!(
        desired
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("preamble".to_owned(), "You are helpful.".to_owned()),
            ("cwd".to_owned(), "<cwd>\n/repo (git)\n</cwd>".to_owned()),
            (
                "failing".to_owned(),
                "<failing>\nold\n</failing>".to_owned()
            ),
        ]
    );
    assert_eq!(reports, vec!["render failed", "also failed"]);
}

#[tokio::test]
async fn propagates_section_errors_after_cancellation() {
    let signal = kiana_chord::context::AbortSignal::new();
    signal.abort(Some(kiana_chord::context::AbortReason::error(
        std::io::Error::other("cancelled"),
    )));
    let context = kiana_chord::context::with_abort_signal(signal, Arc::clone(&BACKGROUND_CONTEXT));
    let sections = vec![kiana_durable::harness::define::section(
        "a",
        failing("cancelled"),
        None,
    )];
    let input = prompt_input();
    let error = render_sections(
        &sections,
        &input,
        &IndexMap::new(),
        &mut |_, _| {},
        context.as_ref(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "cancelled");
}

#[test]
fn replays_sections_in_place_deletes_on_null_and_appends_readditions() {
    fn system(entries: &[(&str, Option<&str>)]) -> Message {
        Message::System(SystemMessage {
            role: Default::default(),
            content: kiana_ai::SystemContent::Text(String::new()),
            sections: Some(sections_map(entries)),
            tools_added: None,
            tools_removed: None,
            timestamp: 1,
        })
    }
    let shown = replay_sections(&[
        system(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))]),
        user("x"),
        system(&[("b", Some("20")), ("a", None)]),
        system(&[("a", Some("10"))]),
    ]);
    assert_eq!(
        shown
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("b".to_owned(), "20".to_owned()),
            ("c".to_owned(), "3".to_owned()),
            ("a".to_owned(), "10".to_owned()),
        ]
    );
}

// ---------------------------------------------------------------------------
// 分段计划
// ---------------------------------------------------------------------------

#[tokio::test]
async fn emits_minimal_value_patches_removals_and_additions() {
    let fixture = Fixture::open().await;
    assert_eq!(
        fixture
            .apply(&[("a", "1"), ("b", "2"), ("c", "3")], &[])
            .await,
        vec![Planned {
            sections: sections_map(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))]),
            omit: None,
        }]
    );
    assert_eq!(
        fixture
            .apply(&[("a", "1"), ("b", "20"), ("c", "3"), ("d", "4")], &[])
            .await,
        vec![Planned {
            sections: sections_map(&[("b", Some("20")), ("d", Some("4"))]),
            omit: None,
        }]
    );
    assert_eq!(
        fixture
            .apply(&[("a", "1"), ("c", "3"), ("d", "4")], &[])
            .await,
        vec![Planned {
            sections: sections_map(&[("b", None)]),
            omit: None,
        }]
    );
    assert!(
        fixture
            .apply(&[("a", "1"), ("c", "3"), ("d", "4")], &[])
            .await
            .is_empty()
    );
    assert_eq!(
        fixture.apply(&[], &[]).await,
        vec![Planned {
            sections: sections_map(&[("a", None), ("c", None), ("d", None)]),
            omit: None,
        }]
    );
}

#[tokio::test]
async fn rewrites_order_only_changes_and_readditions_as_two_entries() {
    let fixture = Fixture::open().await;
    fixture.apply(&[("a", "1"), ("b", "2")], &[]).await;
    assert_eq!(
        fixture.apply(&[("b", "2"), ("a", "1")], &[]).await,
        vec![
            Planned {
                sections: sections_map(&[("a", None), ("b", None)]),
                omit: None,
            },
            Planned {
                sections: sections_map(&[("b", Some("2")), ("a", Some("1"))]),
                omit: None,
            },
        ]
    );

    let readded = Fixture::open().await;
    readded
        .apply(&[("a", "1"), ("b", "2"), ("c", "3")], &[])
        .await;
    assert_eq!(
        readded.apply(&[("a", "1"), ("c", "3")], &[]).await,
        vec![Planned {
            sections: sections_map(&[("b", None)]),
            omit: None,
        }]
    );
    // 打补丁会把 `b` 追加到 `c` 之后。
    assert_eq!(
        readded
            .apply(&[("a", "1"), ("b", "2"), ("c", "3")], &[])
            .await,
        vec![
            Planned {
                sections: sections_map(&[("a", None), ("c", None)]),
                omit: None,
            },
            Planned {
                sections: sections_map(&[("a", Some("1")), ("b", Some("2")), ("c", Some("3"))]),
                omit: None,
            },
        ]
    );
}

#[tokio::test]
async fn rebaselines_after_a_head_marker_omitting_retained_deltas_on_both_sides() {
    let fixture = Fixture::open().await;
    fixture.apply(&[("a", "1"), ("b", "2")], &[]).await;
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "kiana.user".to_owned(),
            model: Some(vec![user("hi")]),
            data: None,
            head: None,
            edits: None,
        })
        .await;
    fixture.apply(&[("a", "1"), ("b", "20")], &[]).await;
    let delta = fixture.last_system_id().await;
    fixture.marker(EntryHead::Id(delta)).await;
    assert_eq!(
        fixture.apply(&[("a", "1"), ("b", "20")], &[]).await,
        vec![Planned {
            sections: sections_map(&[("a", Some("1")), ("b", Some("20"))]),
            omit: Some(vec![delta]),
        }]
    );
    let baseline = fixture.last_system_id().await;
    // 现在 head 标记之后有一个 system 条目，后续变化是普通补丁。
    assert_eq!(
        fixture.apply(&[("a", "1"), ("b", "21")], &[]).await,
        vec![Planned {
            sections: sections_map(&[("b", Some("21"))]),
            omit: None,
        }]
    );
    let after = fixture.last_system_id().await;
    // 第二个标记保留第一个标记两侧的 delta。
    fixture.marker(EntryHead::Id(delta)).await;
    assert_eq!(
        fixture.apply(&[("a", "1"), ("b", "21")], &[]).await,
        vec![Planned {
            sections: sections_map(&[("a", Some("1")), ("b", Some("21"))]),
            omit: Some(vec![delta, baseline, after]),
        }]
    );
}

#[tokio::test]
async fn writes_a_complete_post_head_baseline_even_when_replay_already_matches() {
    let fixture = Fixture::open().await;
    fixture.apply(&[("a", "1")], &[]).await;
    let baseline = fixture.last_system_id().await;
    fixture.marker(EntryHead::Id(baseline)).await;
    assert_eq!(
        fixture.apply(&[("a", "1")], &[]).await,
        vec![Planned {
            sections: sections_map(&[("a", Some("1"))]),
            omit: Some(vec![baseline]),
        }]
    );
    fixture.marker(EntryHead::SelfHead).await;
    assert_eq!(
        fixture.apply(&[], &[]).await,
        vec![Planned {
            sections: sections_map(&[]),
            omit: None,
        }]
    );
    assert!(fixture.apply(&[], &[]).await.is_empty());
}

// 回归（major）：TS 的最小分段补丁是普通对象的属性赋值，`patch["__proto__"] = …` 命中的是
// `Object.prototype` 的访问器，不会创建自有属性，所以名为 `__proto__` 的分段（由 JSON.parse 或
// 直接写入产生）在最小补丁里永远不会被修改或删除。存储往返也必须保留这个自有键。
#[tokio::test]
async fn a_stored_proto_section_is_never_changed_or_removed_by_a_minimal_patch() {
    use kiana_durable::types::NoData;

    let fixture = Fixture::open().await;
    let mut sections: IndexMap<String, Option<String>> = IndexMap::new();
    sections.insert("__proto__".to_owned(), Some("a".to_owned()));
    sections.insert("b".to_owned(), Some("1".to_owned()));
    fixture
        .append_system(SystemDraft {
            model: Some(vec![Message::System(SystemMessage {
                role: Default::default(),
                content: kiana_ai::SystemContent::Text(String::new()),
                sections: Some(sections),
                tools_added: None,
                tools_removed: None,
                timestamp: 1,
            })]),
            data: NoData,
            head: None,
            edits: None,
        })
        .await;

    let view = fixture.view().await;
    let replayed = replay_sections(&view.messages);
    assert_eq!(
        replayed.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["__proto__", "b"]
    );
    assert_eq!(replayed.get("__proto__").map(String::as_str), Some("a"));

    // 改值：TS 不计划任何条目。
    let mut changed = IndexMap::new();
    changed.insert("__proto__".to_owned(), "changed".to_owned());
    changed.insert("b".to_owned(), "1".to_owned());
    assert!(plan_system_entries(&view, &changed, &[], 7).is_empty());

    // 删除：同样什么也不写。
    let mut removed = IndexMap::new();
    removed.insert("b".to_owned(), "1".to_owned());
    assert!(plan_system_entries(&view, &removed, &[], 7).is_empty());

    // 同时改 `b`：补丁只有 `b`。
    let mut together = IndexMap::new();
    together.insert("__proto__".to_owned(), "changed".to_owned());
    together.insert("b".to_owned(), "2".to_owned());
    assert_eq!(
        plan_system_entries(&view, &together, &[], 7)
            .iter()
            .map(planned_of)
            .collect::<Vec<_>>(),
        vec![Planned {
            sections: sections_map(&[("b", Some("2"))]),
            omit: None,
        }]
    );
}

// ---------------------------------------------------------------------------
// 工具装载
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adds_removes_replaces_changed_declarations_and_rewrites_the_order_when_needed() {
    let fixture = Fixture::open().await;
    let a = declaration("a");
    let b = declaration("b");
    let c = declaration("c");
    assert_eq!(
        fixture.apply_tools(&[a.clone(), b.clone()], &[]).await,
        vec![ToolPlan {
            removed: None,
            added: Some(vec!["a".to_owned(), "b".to_owned()]),
            sections: None,
        }]
    );
    assert!(
        fixture
            .apply_tools(&[a.clone(), b.clone()], &[])
            .await
            .is_empty()
    );
    assert_eq!(
        fixture
            .apply_tools(&[a.clone(), b.clone(), c.clone()], &[])
            .await,
        vec![ToolPlan {
            removed: None,
            added: Some(vec!["c".to_owned()]),
            sections: None,
        }]
    );
    assert_eq!(
        fixture.apply_tools(&[a.clone(), c.clone()], &[]).await,
        vec![ToolPlan {
            removed: Some(vec!["b".to_owned()]),
            added: None,
            sections: None,
        }]
    );
    let c2 = declaration_with("c", "changed");
    assert_eq!(
        fixture.apply_tools(&[a.clone(), c2.clone()], &[]).await,
        vec![ToolPlan {
            removed: Some(vec!["c".to_owned()]),
            added: Some(vec!["c".to_owned()]),
            sections: None,
        }]
    );
    let a2 = declaration_with("a", "changed");
    assert_eq!(
        fixture.apply_tools(&[a2.clone(), c2.clone()], &[]).await,
        vec![ToolPlan {
            removed: Some(vec!["a".to_owned(), "c".to_owned()]),
            added: Some(vec!["a".to_owned(), "c".to_owned()]),
            sections: None,
        }]
    );
    assert_eq!(
        fixture.apply_tools(&[c2.clone(), a2.clone()], &[]).await,
        vec![ToolPlan {
            removed: Some(vec!["a".to_owned(), "c".to_owned()]),
            added: Some(vec!["c".to_owned(), "a".to_owned()]),
            sections: None,
        }]
    );
    assert_eq!(
        fixture.apply_tools(&[], &[]).await,
        vec![ToolPlan {
            removed: Some(vec!["c".to_owned(), "a".to_owned()]),
            added: None,
            sections: None,
        }]
    );
}

#[tokio::test]
async fn puts_tool_changes_on_the_last_section_entry_and_redeclares_after_a_head_cut() {
    let fixture = Fixture::open().await;
    let a = declaration("a");
    let b = declaration("b");
    assert_eq!(
        fixture
            .apply_tools(std::slice::from_ref(&a), &[("x", "1"), ("y", "2")])
            .await,
        vec![ToolPlan {
            removed: None,
            added: Some(vec!["a".to_owned()]),
            sections: Some(sections_map(&[("x", Some("1")), ("y", Some("2"))])),
        }]
    );
    // 分段顺序改变需要两条条目；工具变化搭在第二条上。
    assert_eq!(
        fixture
            .apply_tools(&[a.clone(), b.clone()], &[("y", "2"), ("x", "1")])
            .await,
        vec![
            ToolPlan {
                removed: None,
                added: None,
                sections: Some(sections_map(&[("x", None), ("y", None)])),
            },
            ToolPlan {
                removed: None,
                added: Some(vec!["b".to_owned()]),
                sections: Some(sections_map(&[("y", Some("2")), ("x", Some("1"))])),
            },
        ]
    );
    fixture.marker(EntryHead::SelfHead).await;
    assert_eq!(
        fixture
            .apply_tools(&[a.clone(), b.clone()], &[("y", "2"), ("x", "1")])
            .await,
        vec![ToolPlan {
            removed: None,
            added: Some(vec!["a".to_owned(), "b".to_owned()]),
            sections: Some(sections_map(&[("y", Some("2")), ("x", Some("1"))])),
        }]
    );
}

#[tokio::test]
async fn user_entry_token_is_available_to_planning() {
    // `SystemEntry` 与 `UserEntry` 的 kind 不同，计划条目只写 `kiana.system`。
    let fixture = Fixture::open().await;
    fixture
        .append(EntryDraft {
            key_order: None,
            kind: "kiana.user".to_owned(),
            model: Some(vec![user("hi")]),
            data: None,
            head: None,
            edits: None,
        })
        .await;
    let drafts = plan_system_entries(&fixture.view().await, &IndexMap::new(), &[], 7);
    assert!(drafts.is_empty());
    let _ = UserEntry;
}
