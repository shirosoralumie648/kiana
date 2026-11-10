//! `packages/evals/test/plan.test.ts` 的移植。
//!
//! 每个 Rust 测试名对应 TS 的 `it` 标题（保留在 doc comment 里）。

use kiana_evals::plan::{
    DOCUMENTATION_VARIANTS, DiscoveredEvalCase, DocumentationVariant, create_task_plan,
    parse_discovered_cases,
};
use serde_json::{Value, json};

fn discovered() -> Value {
    json!([{ "name": "Add model > adds the model", "file": "evals/models.docs.eval.ts" }])
}

/// TS: `parseDiscoveredCases > derives stable case identity from ordinary Vitest names`
#[test]
fn derives_stable_case_identity_from_ordinary_vitest_names() {
    let cases = parse_discovered_cases(&discovered()).unwrap();
    assert_eq!(
        cases,
        vec![DiscoveredEvalCase {
            file: "evals/models.docs.eval.ts".to_string(),
            full_name: "Add model > adds the model".to_string(),
            eval_set: "Add model".to_string(),
            case_id: "adds the model".to_string(),
        }]
    );
}

/// TS: `parseDiscoveredCases > rejects ambiguous and duplicate identities`
#[test]
fn rejects_ambiguous_and_duplicate_identities() {
    let error = parse_discovered_cases(&json!([{ "name": "adds the model", "file": "model.ts" }]))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("<eval set> > <case>"),
        "unexpected error: {error}"
    );

    let mut duplicated = discovered().as_array().unwrap().clone();
    duplicated.extend(discovered().as_array().unwrap().clone());
    let error = parse_discovered_cases(&Value::Array(duplicated))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Duplicate eval case identity"),
        "unexpected error: {error}"
    );
}

/// TS: `createTaskPlan > creates one isolated task per case, variant, model, and repetition`
#[test]
fn creates_one_isolated_task_per_case_variant_model_and_repetition() {
    let cases = parse_discovered_cases(&discovered()).unwrap();
    let tasks = create_task_plan(&cases, "fixture/model", 2.0).unwrap();
    assert_eq!(tasks.len(), 4);
    let order: Vec<(DocumentationVariant, u64)> = tasks
        .iter()
        .map(|task| (task.variant, task.run_number))
        .collect();
    assert_eq!(
        order,
        vec![
            (DocumentationVariant::WithoutDocs, 1),
            (DocumentationVariant::WithDocs, 1),
            (DocumentationVariant::WithDocs, 2),
            (DocumentationVariant::WithoutDocs, 2),
        ]
    );
}

/// TS: `createTaskPlan > rejects invalid model identities and repetitions`
#[test]
fn rejects_invalid_model_identities_and_repetitions() {
    let cases = parse_discovered_cases(&discovered()).unwrap();
    let error = create_task_plan(&cases, "model", 1.0)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("provider and model"),
        "unexpected error: {error}"
    );
    let error = create_task_plan(&cases, "fixture/model", 0.0)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("positive integer"),
        "unexpected error: {error}"
    );
}

/// TS: `DOCUMENTATION_VARIANTS` 的顺序（`without_docs` 先，`with_docs` 后）。
#[test]
fn documentation_variants_have_a_fixed_order() {
    assert_eq!(
        DOCUMENTATION_VARIANTS,
        [
            DocumentationVariant::WithoutDocs,
            DocumentationVariant::WithDocs
        ]
    );
    assert_eq!(DOCUMENTATION_VARIANTS[0].as_str(), "without_docs");
    assert_eq!(DOCUMENTATION_VARIANTS[1].as_str(), "with_docs");
}

/// `protocol.json` / `expected-runs.json` 依赖的键序（TS `JSON.stringify` 的插入顺序）。
#[test]
fn serializes_cases_and_tasks_with_typescript_key_order() {
    let cases = parse_discovered_cases(&discovered()).unwrap();
    assert_eq!(
        serde_json::to_string(&cases).unwrap(),
        r#"[{"file":"evals/models.docs.eval.ts","fullName":"Add model > adds the model","evalSet":"Add model","caseId":"adds the model"}]"#
    );
    let tasks = create_task_plan(&cases, "fixture/model", 1.0).unwrap();
    assert_eq!(
        serde_json::to_string(&tasks).unwrap(),
        r#"[{"file":"evals/models.docs.eval.ts","fullName":"Add model > adds the model","evalSet":"Add model","caseId":"adds the model","variant":"without_docs","model":"fixture/model","runNumber":1},{"file":"evals/models.docs.eval.ts","fullName":"Add model > adds the model","evalSet":"Add model","caseId":"adds the model","variant":"with_docs","model":"fixture/model","runNumber":1}]"#
    );
}
