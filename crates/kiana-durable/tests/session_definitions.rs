//! `test/session-definitions.test.ts` 的端口（运行期部分）。
//!
//! TS 文件的大部分断言是类型层（owner/key/seed 重载、typed entries、历史读取）；Rust 侧由
//! 类型系统在编译期检查，运行期只剩定义校验。

use kiana_durable::{
    DocDefinition, DocTarget, DocumentSemantics, JsonObject, define_doc, define_doc_family,
};
use serde_json::json;

fn definition(version: i64) -> DocDefinition {
    DocDefinition::new("definition.version", version, DocumentSemantics::SESSION)
}

#[test]
#[should_panic(expected = "version must be a positive integer")]
fn rejects_version_zero() {
    let _ = define_doc(definition(0));
}

#[test]
#[should_panic(expected = "version must be a positive integer")]
fn rejects_negative_versions() {
    let _ = define_doc(definition(-1));
}

#[test]
#[should_panic(expected = "version must be a positive integer")]
fn rejects_versions_past_the_javascript_safe_integer_range() {
    let _ = define_doc(definition(9_007_199_254_740_992));
}

#[test]
#[should_panic(expected = "version must be a positive integer")]
fn validates_family_versions_too() {
    let mut family = DocDefinition::family(
        "definition.version-family",
        1,
        DocumentSemantics {
            scope: kiana_durable::ScopeKind::Conversation,
            history: Some(kiana_durable::DocumentHistory::Latest),
            fork: Some(kiana_durable::DocumentFork::Initial),
        },
    );
    family.version = 0;
    let _ = define_doc_family(family);
}

#[test]
fn accepts_the_largest_safe_integer_version() {
    let token = define_doc(definition(9_007_199_254_740_991));
    assert_eq!(token.definition.version, 9_007_199_254_740_991);
}

#[test]
fn targets_carry_owner_key_and_seed() {
    let token = define_doc(
        DocDefinition::new("definition.targets", 1, DocumentSemantics::SESSION)
            .with_initial(|_| JsonObject::new()),
    );
    let conversation = kiana_durable::id_from_number::<kiana_durable::ConversationBrand>(7);
    let task = kiana_durable::id_from_number::<kiana_durable::TaskBrand>(9);

    let session_target = DocTarget::session(&token);
    assert_eq!(session_target.owner, kiana_durable::DocOwner::Session);
    assert!(session_target.key.is_none());
    assert!(session_target.seed.is_none());

    let conversation_target = DocTarget::conversation(&token, conversation);
    assert_eq!(
        conversation_target.owner,
        kiana_durable::DocOwner::Conversation(conversation)
    );

    let task_target = DocTarget::task(&token, task);
    assert_eq!(task_target.owner, kiana_durable::DocOwner::Task(task));

    let family = define_doc_family(
        DocDefinition::family(
            "definition.family",
            1,
            DocumentSemantics {
                scope: kiana_durable::ScopeKind::Conversation,
                history: Some(kiana_durable::DocumentHistory::Latest),
                fork: Some(kiana_durable::DocumentFork::Initial),
            },
        )
        .with_initial(|seed| {
            let mut value = JsonObject::new();
            value.insert("seed".to_owned(), seed.cloned().unwrap_or(json!(null)));
            value
        }),
    );
    let member = DocTarget::conversation_family(&family, conversation, "a", json!(3));
    assert_eq!(member.key.as_deref(), Some("a"));
    assert_eq!(member.seed, Some(json!(3)));
    let task_member = DocTarget::task_family(&family, task, "b", json!(4));
    assert_eq!(task_member.owner, kiana_durable::DocOwner::Task(task));
    assert_eq!(task_member.key.as_deref(), Some("b"));
}
