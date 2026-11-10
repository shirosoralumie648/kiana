//! per-tool 审批卡测试：五种工具内容、dangerous 默认 Reject/Enter 不批准、
//! decision reason、suggestions 与风险解释超时。

use kiana_interactive::render_core::approval::{
    ApprovalCard, ApprovalDecision, ApprovalKey, ApprovalOutcome, ApprovalRequest, ApprovalScope,
    ApprovalSuggestionKind, RISK_ADVISORY_TIMEOUT_MS, RiskAdvisory, RiskAdvisoryState, RiskLevel,
    analyze_bash,
};
use kiana_interactive::render_core::i18n::{Translator, builtin_pack};
use kiana_tui::tui::Component;
use serde_json::json;

fn render(card: &ApprovalCard) -> String {
    card.render_lines(100).join("\n")
}

fn chinese_translator() -> Translator {
    let zh = builtin_pack("zh").expect("builtin zh");
    let en = builtin_pack("en").expect("builtin en");
    Translator::new(zh, en)
}

#[test]
fn bash_card_shows_command_redirects_and_subcommands() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "bash",
        json!({"command": "rm -rf build && ls -la > out.txt 2>&1"}),
    ));
    let text = render(&card);
    assert!(text.contains("Command:"));
    assert!(text.contains("rm -rf build"));
    assert!(text.contains("Redirects:"));
    assert!(text.contains("> out.txt"));
    assert!(text.contains("2>&1"));
    assert!(text.contains("Subcommands:"));
    assert!(text.contains("1. rm -rf build"));
    assert!(text.contains("2. ls -la > out.txt 2>&1"));

    let analysis = analyze_bash("echo 'a | b' && cat < in.txt; pwd");
    assert_eq!(
        analysis.subcommands,
        vec!["echo 'a | b'", "cat < in.txt", "pwd"]
    );
    assert_eq!(analysis.redirects, vec!["< in.txt"]);
}

#[test]
fn file_descriptor_duplications_render_verbatim() {
    // `2>&1` 的 `&1` 是操作符的一部分，不是目标文件；审批卡不能把它渲染成
    // `2>& 1`。命令回显本身包含 `2>&1`，所以只断言子串会漏掉这个回归。
    let analysis = analyze_bash("rm -rf build && ls -la > out.txt 2>&1");
    assert_eq!(analysis.redirects, vec!["> out.txt", "2>&1"]);

    let analysis = analyze_bash("make >&2");
    assert_eq!(analysis.redirects, vec![">&2"]);

    let analysis = analyze_bash("make 2>&1 > log.txt");
    assert_eq!(analysis.redirects, vec!["2>&1", "> log.txt"]);
}

#[test]
fn relocalizing_updates_queued_card_labels() {
    let request = ApprovalRequest::new("bash", json!({"command": "ls -la"}));
    let en = builtin_pack("en").expect("builtin en");
    let english = Translator::new(en.clone(), en);
    let mut card = ApprovalCard::build_localized(&request, &english);
    assert_eq!(card.suggestions[0].label, "Allow once");

    // 运行期切换 UI 语言：已排队/已显示的卡片必须重译建议标签。
    card.relocalize(&chinese_translator());
    assert_ne!(card.suggestions[0].label, "Allow once");
    let text = render(&card);
    assert!(
        text.lines()
            .any(|line| line.contains(&card.suggestions[0].label)),
        "rendered suggestions must use the new locale: {text}"
    );
}

#[test]
fn file_edit_card_renders_diff() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "edit",
        json!({
            "file_path": "/repo/src/main.rs",
            "old_string": "fn main() {\n    old();\n}\n",
            "new_string": "fn main() {\n    new();\n}\n"
        }),
    ));
    let text = render(&card);
    assert!(text.contains("src/main.rs"));
    assert!(text.contains("-     old();"));
    assert!(text.contains("+     new();"));
    assert!(text.contains("  fn main() {"));
}

#[test]
fn file_write_card_shows_content_and_action() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "write",
        json!({
            "file_path": "/repo/new.txt",
            "file_exists": false,
            "content": "line one\nline two\n"
        }),
    ));
    let text = render(&card);
    assert!(text.contains("Action: Create file"));
    assert!(text.contains("new.txt"));
    assert!(text.contains("| line one"));
    assert!(text.contains("| line two"));
}

#[test]
fn webfetch_card_shows_only_domain() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "webfetch",
        json!({"url": "https://example.com/private/path?token=s3cr3t"}),
    ));
    let text = render(&card);
    assert!(text.contains("Domain: example.com"));
    assert!(!text.contains("private"));
    assert!(!text.contains("s3cr3t"));
}

#[test]
fn mcp_card_redacts_secrets() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "mcp__github__create_issue",
        json!({"title": "bug", "api_key": "s3cr3t", "token": "abc"}),
    ));
    let text = render(&card);
    assert!(text.contains("MCP server: github"));
    assert!(text.contains("Tool: create_issue"));
    assert!(text.contains("[redacted]"));
    assert!(!text.contains("s3cr3t"));
    assert!(!text.contains("\"abc\""));
}

#[test]
fn skill_card_and_unknown_fallback() {
    let card = ApprovalCard::build(&ApprovalRequest::new(
        "skill",
        json!({"skill": "pdf", "prompt": "extract tables"}),
    ));
    let text = render(&card);
    assert!(text.contains("Skill: pdf"));
    assert!(text.contains("extract tables"));

    let card = ApprovalCard::build(&ApprovalRequest::new("custom_tool", json!({"foo": "bar"})));
    let text = render(&card);
    assert!(text.contains("Custom_tool") || text.contains("custom_tool"));
    assert!(text.contains("Arguments:"));
}

#[test]
fn decision_reason_and_suggestions_are_displayed() {
    let request = ApprovalRequest::new("bash", json!({"command": "ls"}))
        .with_decision_reason("Matches Ask rule: workspace write")
        .with_always_scopes(vec![ApprovalScope::Session, ApprovalScope::Project]);
    let card = ApprovalCard::build(&request);
    let text = render(&card);
    assert!(text.contains("Reason: Matches Ask rule: workspace write"));
    let labels: Vec<&str> = card
        .suggestions
        .iter()
        .map(|suggestion| suggestion.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec![
            "Allow once",
            "Always allow (this session)",
            "Always allow (this project)",
            "Reject"
        ]
    );
}

#[test]
fn dangerous_action_focuses_reject_and_enter_never_approves() {
    let request = ApprovalRequest::new("bash", json!({"command": "rm -rf /"})).with_dangerous(true);
    let mut card = ApprovalCard::build(&request);
    assert_eq!(
        card.focused_suggestion().unwrap().kind,
        ApprovalSuggestionKind::Reject
    );

    // 显式移到 Allow once，Enter 仍被拒绝（危险动作）。
    card.focus_kind(ApprovalSuggestionKind::AllowOnce);
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Ignored
    );
    card.focus_kind(ApprovalSuggestionKind::AllowAlways(ApprovalScope::Session));
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Ignored
    );

    card.focus_kind(ApprovalSuggestionKind::Reject);
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Decision(ApprovalDecision::Reject { reason: None })
    );
}

#[test]
fn non_dangerous_enter_approves_focused_suggestion() {
    let request = ApprovalRequest::new("read", json!({"path": "a.txt"}));
    let mut card = ApprovalCard::build(&request);
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Decision(ApprovalDecision::AllowOnce)
    );
    let mut card = ApprovalCard::build(&request);
    card.focus_kind(ApprovalSuggestionKind::AllowAlways(ApprovalScope::Project));
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Decision(ApprovalDecision::AllowAlways(ApprovalScope::Project))
    );
    let mut card = ApprovalCard::build(&request);
    assert_eq!(
        card.handle_key(ApprovalKey::Escape),
        ApprovalOutcome::Decision(ApprovalDecision::Reject {
            reason: Some("Approval was cancelled.".to_string())
        })
    );
}

#[test]
fn risk_advisory_is_optional_advisory_and_hides_after_3s() {
    let request = ApprovalRequest::new("bash", json!({"command": "ls"}));
    let mut card = ApprovalCard::build(&request);
    assert!(matches!(card.risk, RiskAdvisoryState::Hidden));

    card.begin_risk(0);
    assert!(render(&card).contains("analyzing"));
    assert!(!card.tick(RISK_ADVISORY_TIMEOUT_MS - 1));
    assert!(card.tick(RISK_ADVISORY_TIMEOUT_MS));
    assert!(matches!(card.risk, RiskAdvisoryState::Hidden));
    assert!(!render(&card).contains("analyzing"));

    // 成功：显示 advisory 标记。
    card.begin_risk(10_000);
    card.resolve_risk(Ok(RiskAdvisory {
        level: RiskLevel::High,
        explanation: "Deletes build output".to_string(),
        reasoning: "I need a clean build".to_string(),
        risk: "Irreversible deletion".to_string(),
    }));
    let text = render(&card);
    assert!(text.contains("Risk (advisory): HIGH"));
    assert!(text.contains("Irreversible deletion"));

    // 失败：静默隐藏，不阻塞审批。
    card.begin_risk(20_000);
    card.resolve_risk(Err(()));
    assert!(matches!(card.risk, RiskAdvisoryState::Hidden));
    assert!(!render(&card).contains("Risk"));
}

#[test]
fn card_component_renders_and_routes_keys() {
    let request = ApprovalRequest::new("bash", json!({"command": "ls"}));
    let mut component = kiana_interactive::render_core::approval::ApprovalCardComponent::new(
        ApprovalCard::build(&request),
    );
    assert!(
        component
            .render(60)
            .join("\n")
            .contains("Permission required")
    );
    component.handle_input("\x1b[B");
    component.handle_input("\r");
    assert_eq!(
        component.decision(),
        Some(&ApprovalDecision::AllowAlways(ApprovalScope::Session))
    );
}

#[test]
fn localized_card_uses_language_pack_labels() {
    let request = ApprovalRequest::new("bash", json!({"command": "rm -rf /"}))
        .with_dangerous(true)
        .with_decision_reason("危险命令");
    let mut card = ApprovalCard::build_localized(&request, &chinese_translator());
    let text = render(&card);
    assert!(
        text.contains("需要权限: Run shell command (bash)"),
        "{text}"
    );
    assert!(text.contains("原因: 危险命令"), "{text}");
    assert!(text.contains("只允许一次"), "{text}");
    assert!(text.contains("总是允许（this session）"), "{text}");
    assert!(text.contains("拒绝"), "{text}");
    assert!(text.contains("可选操作"), "{text}");
    assert!(text.contains("危险操作：默认选中「拒绝」"), "{text}");
    let labels: Vec<&str> = card
        .suggestions
        .iter()
        .map(|suggestion| suggestion.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec![
            "只允许一次",
            "总是允许（this session）",
            "总是允许（this project）",
            "总是允许（this machine）",
            "拒绝"
        ]
    );

    card.begin_risk(0);
    let text = render(&card);
    assert!(text.contains("风险分析：分析中……（仅供参考）"), "{text}");
}

#[test]
fn long_lines_are_truncated() {
    let request = ApprovalRequest::new("bash", json!({"command": "x".repeat(500)}));
    let card = ApprovalCard::build(&request);
    for line in card.render_lines(40) {
        assert!(
            kiana_tui::text::visible_width(&line) <= 40,
            "line too wide: {line:?}"
        );
    }
}

#[test]
fn dangerous_action_can_only_be_approved_through_explicit_confirmation() {
    let request = ApprovalRequest::new("bash", json!({"command": "rm -rf /"})).with_dangerous(true);
    let mut card = ApprovalCard::build(&request);
    card.focus_kind(ApprovalSuggestionKind::AllowOnce);
    assert_eq!(
        card.handle_key(ApprovalKey::Enter),
        ApprovalOutcome::Ignored
    );
    assert_eq!(card.confirm_dangerous(), Some(ApprovalDecision::AllowOnce));
}
