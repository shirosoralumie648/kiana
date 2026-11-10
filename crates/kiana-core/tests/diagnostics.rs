//! `core/diagnostics.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! TS: `packages/coding-agent/src/core/diagnostics.ts`。

use kiana_core::diagnostics::{
    DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType,
};
use serde_json::json;

fn collision() -> ResourceCollision {
    ResourceCollision {
        resource_type: ResourceType::Skill,
        name: "review".into(),
        winner_path: "/global/skills/review/SKILL.md".into(),
        loser_path: "/project/.kiana/skills/review/SKILL.md".into(),
        winner_source: Some("local".into()),
        loser_source: Some("npm:foo".into()),
    }
}

#[test]
fn resource_collision_serializes_with_ts_field_names_and_order() {
    assert_eq!(
        serde_json::to_string(&collision()).expect("serialize"),
        concat!(
            r#"{"resourceType":"skill","name":"review","#,
            r#""winnerPath":"/global/skills/review/SKILL.md","#,
            r#""loserPath":"/project/.kiana/skills/review/SKILL.md","#,
            r#""winnerSource":"local","loserSource":"npm:foo"}"#
        )
    );
}

#[test]
fn resource_collision_omits_absent_sources() {
    let value = serde_json::to_value(ResourceCollision {
        winner_source: None,
        loser_source: None,
        ..collision()
    })
    .expect("serialize");
    assert_eq!(
        value,
        json!({
            "resourceType": "skill",
            "name": "review",
            "winnerPath": "/global/skills/review/SKILL.md",
            "loserPath": "/project/.kiana/skills/review/SKILL.md"
        })
    );
}

#[test]
fn resource_types_use_the_ts_literals() {
    for (kind, literal) in [
        (ResourceType::Extension, "extension"),
        (ResourceType::Skill, "skill"),
        (ResourceType::Prompt, "prompt"),
        (ResourceType::Theme, "theme"),
    ] {
        assert_eq!(
            serde_json::to_value(kind).expect("serialize"),
            json!(literal)
        );
    }
}

#[test]
fn diagnostic_serializes_type_message_path_and_collision() {
    let diagnostic = ResourceDiagnostic {
        kind: DiagnosticKind::Collision,
        message: "skill \"review\" from npm:foo is ignored".into(),
        path: Some("/project/.kiana/skills/review/SKILL.md".into()),
        collision: Some(collision()),
    };
    assert_eq!(
        serde_json::to_string(&diagnostic).expect("serialize"),
        concat!(
            r#"{"type":"collision","message":"skill \"review\" from npm:foo is ignored","#,
            r#""path":"/project/.kiana/skills/review/SKILL.md","collision":{"resourceType":"skill","#,
            r#""name":"review","winnerPath":"/global/skills/review/SKILL.md","#,
            r#""loserPath":"/project/.kiana/skills/review/SKILL.md","winnerSource":"local","loserSource":"npm:foo"}}"#
        )
    );
}

#[test]
fn diagnostic_omits_absent_optional_fields() {
    let diagnostic = ResourceDiagnostic {
        kind: DiagnosticKind::Warning,
        message: "w".into(),
        path: None,
        collision: None,
    };
    assert_eq!(
        serde_json::to_string(&diagnostic).expect("serialize"),
        r#"{"type":"warning","message":"w"}"#
    );
    assert_eq!(
        serde_json::to_value(DiagnosticKind::Error).expect("serialize"),
        json!("error")
    );
}

#[test]
fn diagnostics_round_trip() {
    let original = ResourceDiagnostic {
        kind: DiagnosticKind::Error,
        message: "bad".into(),
        path: None,
        collision: Some(collision()),
    };
    let text = serde_json::to_string(&original).expect("serialize");
    let decoded: ResourceDiagnostic = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(decoded, original);
}
