//! loop guard 单元测试：每个检测器的 Warn/Stop 阈值与误报边界。

use indexmap::IndexMap;
use kiana_agent_core::loop_guard::{
    DetectorThreshold, LoopDetectorKind, LoopGuard, LoopGuardConfig, LoopGuardOutcome,
    canonical_json, repeat_key,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, TextContent, TextKind, ThinkingContent,
    ThinkingKind, ToolCall, ToolCallKind,
};
use serde_json::{Value, json};

fn args(value: Value) -> IndexMap<String, Value> {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected object, got {other}"),
    }
}

fn tool_call(name: &str, arguments: IndexMap<String, Value>) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: format!("call-{name}"),
        name: name.to_string(),
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

fn assistant(blocks: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: blocks,
        api: "test".to_string(),
        provider: "test".to_string(),
        model: "test".to_string(),
        ..Default::default()
    }
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn thinking_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent {
        kind_thinking: ThinkingKind::Thinking,
        thinking: text.to_string(),
        thinking_signature: None,
        redacted: None,
    })
}

fn only(kind: LoopDetectorKind, reminder_at: u32, stop_at: u32) -> LoopGuardConfig {
    let mut config = LoopGuardConfig::default();
    // 抬高其它检测器，避免测试互相干扰。
    for key in LoopGuardConfig::default_thresholds().keys() {
        config
            .thresholds
            .insert(*key, DetectorThreshold::new(10_000, 20_000));
    }
    config
        .thresholds
        .insert(kind, DetectorThreshold::new(reminder_at, stop_at));
    config
}

#[test]
fn repeat_key_is_canonical_and_ignores_description_and_display() {
    let left = args(json!({
        "a": 1,
        "b": {"y": 2, "x": 1},
        "description": "first",
        "display": "show first"
    }));
    let right = args(json!({
        "display": "show second",
        "b": {"x": 1, "y": 2},
        "a": 1,
        "description": "second"
    }));
    assert_eq!(repeat_key("edit", &left), repeat_key("edit", &right));
    assert_ne!(repeat_key("edit", &left), repeat_key("write", &left));
    assert_eq!(
        canonical_json(&json!({"b": 1, "a": [3, {"d": 4, "c": 3}]})),
        "{\"a\":[3,{\"c\":3,\"d\":4}],\"b\":1}"
    );
}

#[test]
fn consecutive_tool_calls_warn_at_three_and_stop_at_five() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::ConsecutiveToolCalls, 3, 5));
    let arguments = args(json!({"command": "echo hi"}));
    assert_eq!(
        guard.observe_tool_call("bash", &arguments),
        LoopGuardOutcome::Continue
    );
    assert_eq!(
        guard.observe_tool_call("bash", &arguments),
        LoopGuardOutcome::Continue
    );
    let third = guard.observe_tool_call("bash", &arguments);
    assert!(matches!(third, LoopGuardOutcome::Warn(detection) if detection.count == 3));
    assert!(guard.take_reminder().is_some());
    assert!(guard.take_reminder().is_none());
    guard.observe_tool_call("bash", &arguments);
    let fifth = guard.observe_tool_call("bash", &arguments);
    assert!(matches!(fifth, LoopGuardOutcome::Stop(detection) if detection.count == 5));
    assert!(guard.stopped().is_some());
}

#[test]
fn global_repetition_warns_without_consecutive_calls() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::GlobalRepeatedToolCalls, 4, 99));
    let repeated = args(json!({"path": "a.txt"}));
    let mut last = LoopGuardOutcome::Continue;
    for index in 0..4 {
        last = guard.observe_tool_call("read", &repeated);
        // 中间插入不同调用，保证不是连续重复。
        if index + 1 < 4 {
            guard.observe_tool_call("read", &args(json!({"path": format!("other-{index}.txt")})));
        }
    }
    assert!(matches!(
        last,
        LoopGuardOutcome::Warn(detection)
            if detection.kind == LoopDetectorKind::GlobalRepeatedToolCalls && detection.count == 4
    ));
}

#[test]
fn alternating_calls_warn_after_two_cycles() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::AlternatingToolCalls, 2, 5));
    let a = args(json!({"command": "a"}));
    let b = args(json!({"command": "b"}));
    assert_eq!(
        guard.observe_tool_call("bash", &a),
        LoopGuardOutcome::Continue
    );
    assert_eq!(
        guard.observe_tool_call("bash", &b),
        LoopGuardOutcome::Continue
    );
    guard.observe_tool_call("bash", &a);
    let fourth = guard.observe_tool_call("bash", &b);
    assert!(matches!(fourth, LoopGuardOutcome::Warn(detection) if detection.count >= 2));
}

#[test]
fn read_file_loop_ignores_different_paths() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::ReadFileLoop, 3, 8));
    let same = args(json!({"path": "src/main.rs", "offset": 0}));
    guard.observe_tool_call("read", &same);
    guard.observe_tool_call("read", &args(json!({"path": "src/main.rs", "offset": 100})));
    let third =
        guard.observe_tool_call("read", &args(json!({"path": "src/main.rs", "offset": 200})));
    assert!(matches!(third, LoopGuardOutcome::Warn(detection) if detection.count == 3));

    let mut different = LoopGuard::new(only(LoopDetectorKind::ReadFileLoop, 2, 8));
    assert_eq!(
        different.observe_tool_call("read", &args(json!({"path": "a.rs"}))),
        LoopGuardOutcome::Continue
    );
    assert_eq!(
        different.observe_tool_call("read", &args(json!({"path": "b.rs"}))),
        LoopGuardOutcome::Continue
    );
    assert!(different.take_reminder().is_none());
}

#[test]
fn shell_stagnation_requires_identical_output() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::ShellStagnation, 2, 4));
    let command = args(json!({"command": "kubectl get pods"}));
    guard.observe_tool_call("bash", &command);
    let second = guard.observe_tool_call("bash", &command);
    // 输出变化：不触发 shell stagnation（等同于 stateful poll 有进展）。
    assert_eq!(
        guard.observe_tool_result("bash", &command, "pod A Running", false),
        LoopGuardOutcome::Continue
    );
    assert_eq!(second, LoopGuardOutcome::Continue);
    guard.observe_tool_call("bash", &command);
    let outcome = guard.observe_tool_result("bash", &command, "pod A Terminating", false);
    assert_eq!(outcome, LoopGuardOutcome::Continue);
    assert!(guard.take_reminder().is_none());

    // 完全相同的输出：第 2 次提醒、第 4 次停止。
    let mut stagnant = LoopGuard::new(only(LoopDetectorKind::ShellStagnation, 2, 4));
    stagnant.observe_tool_call("bash", &command);
    let first = stagnant.observe_tool_result("bash", &command, "unchanged", false);
    assert_eq!(first, LoopGuardOutcome::Continue);
    stagnant.observe_tool_call("bash", &command);
    let second = stagnant.observe_tool_result("bash", &command, "unchanged", false);
    assert!(matches!(second, LoopGuardOutcome::Warn(detection) if detection.count == 2));
}

#[test]
fn invalid_arguments_and_execution_failures_are_separate_detectors() {
    let mut guard = LoopGuard::new(only(LoopDetectorKind::InvalidArgumentRepeat, 2, 3));
    let arguments = args(json!({"path": 1}));
    guard.observe_tool_call("read", &arguments);
    let first = guard.observe_tool_result(
        "read",
        &arguments,
        "invalid arguments: path must be a string",
        true,
    );
    assert_eq!(first, LoopGuardOutcome::Continue);
    guard.observe_tool_call("read", &arguments);
    let second = guard.observe_tool_result(
        "read",
        &arguments,
        "invalid arguments: path must be a string",
        true,
    );
    assert!(matches!(second, LoopGuardOutcome::Warn(_)));

    let mut failures = LoopGuard::new(only(LoopDetectorKind::ExecutionFailureRepeat, 2, 3));
    for _ in 0..2 {
        failures.observe_tool_call("bash", &args(json!({"command": "false"})));
        let outcome = failures.observe_tool_result(
            "bash",
            &args(json!({"command": "false"})),
            "command failed",
            true,
        );
        if matches!(outcome, LoopGuardOutcome::Warn(_)) {
            return;
        }
    }
    panic!("execution failure repetition did not warn");
}

#[test]
fn different_edits_and_reads_do_not_warn() {
    let mut guard = LoopGuard::new(LoopGuardConfig::default());
    let mut outcome = LoopGuardOutcome::Continue;
    for index in 0..20 {
        let edit = args(json!({
            "path": format!("src/file-{index}.rs"),
            "oldText": format!("old-{index}"),
            "newText": format!("new-{index}")
        }));
        outcome = outcome.merge(guard.observe_tool_call("edit", &edit));
        outcome = outcome.merge(guard.observe_tool_result("edit", &edit, "ok", false));
        let read = args(json!({"path": format!("src/file-{index}.rs")}));
        outcome = outcome.merge(guard.observe_tool_call("read", &read));
        outcome = outcome.merge(guard.observe_tool_result("read", &read, "contents", false));
    }
    assert_eq!(outcome, LoopGuardOutcome::Continue);
    assert!(guard.take_reminder().is_none());
    assert!(guard.stopped().is_none());
}

#[test]
fn repeated_thought_and_content_chanting_warn() {
    let mut thoughts = LoopGuard::new(only(LoopDetectorKind::RepeatedThought, 2, 3));
    let message = assistant(vec![thinking_block("I should check the logs again")]);
    assert_eq!(
        thoughts.observe_assistant(&message),
        LoopGuardOutcome::Continue
    );
    let second = thoughts.observe_assistant(&message);
    assert!(matches!(second, LoopGuardOutcome::Warn(_)));

    let mut chanting = LoopGuard::new(only(LoopDetectorKind::ContentChanting, 3, 10));
    let repeated = "The build is still running. ".repeat(4);
    let mut outcome = LoopGuardOutcome::Continue;
    for _ in 0..4 {
        outcome =
            outcome.merge(chanting.observe_assistant(&assistant(vec![text_block(&repeated)])));
    }
    assert!(matches!(
        outcome,
        LoopGuardOutcome::Warn(_) | LoopGuardOutcome::Stop(_)
    ));
}

#[test]
fn repeated_thought_requires_consecutive_thinking_messages() {
    // 中间夹一条没有 thinking 的 assistant 消息时，思考不再“连续”，不能累计到硬停。
    let mut guard = LoopGuard::new(only(LoopDetectorKind::RepeatedThought, 2, 3));
    let with_thinking = assistant(vec![thinking_block("I should check the logs again")]);
    let without_thinking = assistant(vec![text_block("Running the command now.")]);
    for _ in 0..3 {
        assert_eq!(
            guard.observe_assistant(&with_thinking),
            LoopGuardOutcome::Continue
        );
        assert_eq!(
            guard.observe_assistant(&without_thinking),
            LoopGuardOutcome::Continue
        );
    }
    assert!(guard.stopped().is_none());
}

#[test]
fn markdown_structure_and_fenced_code_do_not_trigger_chanting() {
    // 参考实现会在代码块与表格/列表/标题/引用/分隔线上重置统计；否则模型正常输出的
    // 重复排版（大段代码、分隔线、列表模板）会被误判成内容复读。
    let mut guard = LoopGuard::new(only(LoopDetectorKind::ContentChanting, 3, 10));
    let divider = "=".repeat(120);
    let list = "- item one\n- item two\n".repeat(20);
    for index in 0..12 {
        // 代码块内部重复，但围栏外的说明文字每次不同，避免说明文字自身构成复读。
        let fenced = format!(
            "Here is log number {index}:\n```\n{}\n```\n",
            "repeated-line-with-payload\n".repeat(30)
        );
        for text in [&fenced, &divider, &list] {
            assert_eq!(
                guard.observe_assistant(&assistant(vec![text_block(text)])),
                LoopGuardOutcome::Continue,
                "markdown-structural content must not be treated as chanting"
            );
        }
    }
    assert!(guard.take_reminder().is_none());
}

#[test]
fn long_cycle_repetition_warns_on_second_verbatim_block() {
    let mut config = LoopGuardConfig::default();
    config.thresholds.insert(
        LoopDetectorKind::ContentChanting,
        DetectorThreshold::new(10_000, 20_000),
    );
    config.thresholds.insert(
        LoopDetectorKind::LongCycleRepetition,
        DetectorThreshold::new(2, 4),
    );
    let mut guard = LoopGuard::new(config);
    let block: String = (0..20)
        .map(|index| format!("line {index} with a distinctive payload\n"))
        .collect();
    guard.observe_assistant(&assistant(vec![text_block(&block)]));
    let second = guard.observe_assistant(&assistant(vec![text_block(&block)]));
    assert!(matches!(
        second,
        LoopGuardOutcome::Warn(_) | LoopGuardOutcome::Stop(_)
    ));
}

#[test]
fn turn_tool_call_limit_honors_configured_thresholds() {
    let mut config = LoopGuardConfig::default();
    config.thresholds.insert(
        LoopDetectorKind::TurnToolCallLimit,
        DetectorThreshold::new(3, 5),
    );
    let mut guard = LoopGuard::new(config);
    let calls = |count: u32| -> Vec<AssistantContentBlock> {
        (0..count)
            .map(|index| tool_call("read", args(json!({"path": format!("{index}.txt")}))))
            .collect()
    };
    assert!(matches!(
        guard.observe_assistant(&assistant(calls(3))),
        LoopGuardOutcome::Warn(_)
    ));
    // 恰好等于 stop_at 也必须硬停（「达到」而非「超过」）。
    assert!(matches!(
        guard.observe_assistant(&assistant(calls(5))),
        LoopGuardOutcome::Stop(_)
    ));
}

#[test]
fn zero_turn_tool_limit_does_not_stop_text_only_replies() {
    let config = LoopGuardConfig {
        soft_tool_calls_per_turn: 0,
        ..LoopGuardConfig::default()
    };
    let mut guard = LoopGuard::new(config);
    let outcome = guard.observe_assistant(&assistant(vec![text_block("hello")]));
    assert!(matches!(outcome, LoopGuardOutcome::Continue), "{outcome:?}");
}

#[test]
fn turn_tool_call_limit_warns_and_stops() {
    let config = LoopGuardConfig {
        soft_tool_calls_per_turn: 2,
        hard_tool_call_multiplier: 3,
        ..LoopGuardConfig::default()
    };
    let mut guard = LoopGuard::new(config);
    let calls: Vec<AssistantContentBlock> = (0..3)
        .map(|index| tool_call("read", args(json!({"path": format!("{index}.txt")}))))
        .collect();
    let outcome = guard.observe_assistant(&assistant(calls));
    assert!(matches!(outcome, LoopGuardOutcome::Warn(_)));

    let calls: Vec<AssistantContentBlock> = (0..7)
        .map(|index| tool_call("read", args(json!({"path": format!("{index}.txt")}))))
        .collect();
    let outcome = guard.observe_assistant(&assistant(calls));
    assert!(matches!(outcome, LoopGuardOutcome::Stop(_)));
}
