//! transcript 投影测试：Action/Observation 单卡、工具组折叠、thought 提升、
//! group breaker、provisional slot 的 anchor/attempt/退役与 delta 帧批量。

use kiana_interactive::render_core::transcript::{
    DeltaChunk, DeltaKind, DurableEvent, DurableEventKind, RenderGroupKind, RenderItemKind,
    TranscriptProjection,
};
use serde_json::json;

fn action(id: &str, seq: u64, call: &str, tool: &str) -> DurableEvent {
    DurableEvent::new(
        id,
        seq,
        DurableEventKind::Action {
            tool_call_id: call.to_string(),
            tool_name: tool.to_string(),
            args: json!({"command": "ls"}),
        },
    )
}

fn observation(id: &str, seq: u64, action_id: &str, call: &str, tool: &str) -> DurableEvent {
    DurableEvent::new(
        id,
        seq,
        DurableEventKind::Observation {
            action_id: action_id.to_string(),
            tool_call_id: call.to_string(),
            tool_name: tool.to_string(),
            content: "ok".to_string(),
            is_error: false,
        },
    )
}

#[test]
fn action_and_observation_render_one_tool_card() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(action("a1", 1, "call-1", "bash"));
    projection.apply_durable(observation("o1", 2, "a1", "call-1", "bash"));
    assert_eq!(projection.items().len(), 1);
    match &projection.items()[0].kind {
        RenderItemKind::ToolCard(card) => {
            assert_eq!(card.tool_name, "bash");
            assert!(card.observation.is_some());
        }
        other => panic!("expected tool card, got {other:?}"),
    }
    assert_eq!(projection.groups().len(), 1);
}

#[test]
fn consecutive_tool_runs_fold_into_one_group() {
    let mut projection = TranscriptProjection::new();
    for index in 0..3 {
        let action_id = format!("a{index}");
        let call_id = format!("call-{index}");
        projection.apply_durable(action(&action_id, index as u64 * 2, &call_id, "bash"));
        projection.apply_durable(observation(
            &format!("o{index}"),
            index as u64 * 2 + 1,
            &action_id,
            &call_id,
            "bash",
        ));
    }
    assert_eq!(projection.items().len(), 3);
    let groups = projection.groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].kind, RenderGroupKind::ToolRun);
    assert_eq!(groups[0].items, vec![0, 1, 2]);
}

#[test]
fn thought_is_promoted_and_its_observation_is_ignored() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(DurableEvent::new(
        "t1",
        1,
        DurableEventKind::Thought {
            text: "thinking".to_string(),
        },
    ));
    projection.apply_durable(DurableEvent::new(
        "t1-observation",
        2,
        DurableEventKind::Observation {
            action_id: "t1".to_string(),
            tool_call_id: "think".to_string(),
            tool_name: "think".to_string(),
            content: "thinking observation".to_string(),
            is_error: false,
        },
    ));
    assert_eq!(projection.items().len(), 1);
    match &projection.items()[0].kind {
        RenderItemKind::Thought { text } => assert_eq!(text, "thinking"),
        other => panic!("expected thought, got {other:?}"),
    }
}

#[test]
fn finish_observation_is_ignored() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(DurableEvent::new(
        "f1",
        1,
        DurableEventKind::Finish {
            message: "done".to_string(),
        },
    ));
    projection.apply_durable(DurableEvent::new(
        "f1-observation",
        2,
        DurableEventKind::Observation {
            action_id: "f1".to_string(),
            tool_call_id: "finish".to_string(),
            tool_name: "finish".to_string(),
            content: "done".to_string(),
            is_error: false,
        },
    ));
    assert_eq!(projection.items().len(), 1);
    assert!(matches!(
        projection.items()[0].kind,
        RenderItemKind::Finish { .. }
    ));
}

#[test]
fn artifact_error_finish_break_tool_runs() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(action("a1", 1, "c1", "bash"));
    projection.apply_durable(observation("o1", 2, "a1", "c1", "bash"));
    projection.apply_durable(DurableEvent::new(
        "art",
        3,
        DurableEventKind::Artifact {
            path: "out.txt".to_string(),
            summary: "created".to_string(),
        },
    ));
    projection.apply_durable(action("a2", 4, "c2", "bash"));
    projection.apply_durable(observation("o2", 5, "a2", "c2", "bash"));
    projection.apply_durable(DurableEvent::new(
        "err",
        6,
        DurableEventKind::Error {
            message: "boom".to_string(),
        },
    ));
    projection.apply_durable(action("a3", 7, "c3", "bash"));
    projection.apply_durable(observation("o3", 8, "a3", "c3", "bash"));
    projection.apply_durable(DurableEvent::new(
        "fin",
        9,
        DurableEventKind::Finish {
            message: "done".to_string(),
        },
    ));

    let kinds: Vec<RenderGroupKind> = projection.groups().iter().map(|group| group.kind).collect();
    assert_eq!(
        kinds,
        vec![
            RenderGroupKind::ToolRun,
            RenderGroupKind::Artifact,
            RenderGroupKind::ToolRun,
            RenderGroupKind::Error,
            RenderGroupKind::ToolRun,
            RenderGroupKind::Finish,
        ]
    );
    assert_eq!(projection.groups()[0].items.len(), 1);
    assert_eq!(projection.groups()[2].items.len(), 1);
}

#[test]
fn provisional_slot_uses_anchor_seq_and_retires_on_durable() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(DurableEvent::new(
        "before",
        5,
        DurableEventKind::UserMessage {
            text: "before".to_string(),
        },
    ));
    projection.apply_durable(DurableEvent::new(
        "after",
        10,
        DurableEventKind::UserMessage {
            text: "after".to_string(),
        },
    ));
    projection.open_slot("streamed", Some(7), 1);
    projection.push_delta(DeltaChunk::text("streamed", Some(7), 1, "hello"));
    projection.flush_pending();
    let ids: Vec<&str> = projection
        .items()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, vec!["before", "streamed", "after"]);
    assert!(projection.items()[1].provisional);
    assert_eq!(projection.provisional_count(), 1);

    projection.apply_durable(DurableEvent::new(
        "streamed",
        7,
        DurableEventKind::AssistantText {
            text: "hello world".to_string(),
        },
    ));
    assert_eq!(projection.items().len(), 3);
    assert_eq!(projection.provisional_count(), 0);
    assert_eq!(projection.slots_retired(), 1);
    assert_eq!(
        projection.items()[1].kind,
        RenderItemKind::AssistantMessage {
            text: "hello world".to_string()
        }
    );
}

#[test]
fn higher_attempt_supersedes_and_lower_attempt_is_dropped() {
    let mut projection = TranscriptProjection::new();
    projection.open_slot("m", None, 1);
    projection.push_delta(DeltaChunk::text("m", None, 1, "old"));
    projection.flush_pending();
    projection.push_delta(DeltaChunk::text("m", None, 2, "new"));
    projection.flush_pending();
    let text = match &projection.items()[0].kind {
        RenderItemKind::AssistantMessage { text } => text.clone(),
        other => panic!("expected assistant, got {other:?}"),
    };
    assert_eq!(text, "new");

    projection.push_delta(DeltaChunk {
        item_id: "m".to_string(),
        anchor_seq: None,
        attempt: 1,
        kind: DeltaKind::Text,
        content: "stale".to_string(),
    });
    projection.flush_pending();
    let text = match &projection.items()[0].kind {
        RenderItemKind::AssistantMessage { text } => text.clone(),
        other => panic!("expected assistant, got {other:?}"),
    };
    assert_eq!(text, "new");
    assert_eq!(projection.deltas_dropped(), 1);
}

#[test]
fn late_delta_after_durable_is_dropped() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(DurableEvent::new(
        "m",
        1,
        DurableEventKind::AssistantText {
            text: "final".to_string(),
        },
    ));
    projection.push_delta(DeltaChunk::text("m", None, 2, "late"));
    projection.flush_pending();
    assert_eq!(projection.provisional_count(), 0);
    assert_eq!(
        projection.items()[0].kind,
        RenderItemKind::AssistantMessage {
            text: "final".to_string()
        }
    );
    assert_eq!(projection.deltas_dropped(), 1);
}

#[test]
fn deltas_are_batched_per_frame_and_control_events_flush_first() {
    let mut projection = TranscriptProjection::new().with_min_frame_interval_ms(16);
    let mut commits = 0usize;
    for index in 0..3_000 {
        let now = index as i64;
        projection.push_delta(DeltaChunk::text("m", None, 1, "a"));
        if projection.flush_due(now) {
            commits += 1;
        }
    }
    projection.flush_due(10_000);
    assert!(
        commits <= 200,
        "3000 个 chunk 不能产生 3000 帧，实际 {commits}"
    );
    let text = match &projection.items()[0].kind {
        RenderItemKind::AssistantMessage { text } => text.clone(),
        other => panic!("expected assistant, got {other:?}"),
    };
    assert_eq!(text.chars().count(), 3_000);

    // 控制事件之前必须 flush 掉 pending delta，顺序不能乱。
    let mut projection = TranscriptProjection::new();
    projection.push_delta(DeltaChunk::text("x", None, 1, "pending"));
    projection.apply_durable(DurableEvent::new(
        "control",
        1,
        DurableEventKind::UserMessage {
            text: "control".to_string(),
        },
    ));
    assert_eq!(projection.pending_delta_count(), 0);
    assert!(projection.frame_commits() >= 1);
    let ids: Vec<&str> = projection
        .items()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, vec!["x", "control"]);
}

#[test]
fn provisional_slots_are_ordered_by_anchor_seq() {
    let mut projection = TranscriptProjection::new();
    projection.apply_durable(DurableEvent::new(
        "before",
        5,
        DurableEventKind::UserMessage {
            text: "before".to_string(),
        },
    ));
    projection.apply_durable(DurableEvent::new(
        "after",
        10,
        DurableEventKind::UserMessage {
            text: "after".to_string(),
        },
    ));
    projection.open_slot("anchor-7", Some(7), 1);
    projection.open_slot("anchor-5", Some(5), 1);
    let ids: Vec<&str> = projection
        .items()
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, vec!["before", "anchor-5", "anchor-7", "after"]);
}
