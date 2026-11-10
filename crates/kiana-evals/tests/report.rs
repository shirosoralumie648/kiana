//! `packages/evals/test/report.test.ts` 的移植，外加观测 JSON 的逐字断言。
//!
//! TS 测试用 `mkdtemp` + `afterEach(rm)`；这里用 `tempfile::TempDir`（Drop 时递归清理）。

use std::path::{Path, PathBuf};

use kiana_evals::plan::{DocumentationVariant, EvalTask};
use kiana_evals::report::{
    EvalObservation, EvalOutcome, JsNumber, KIANA_SESSION_SNAPSHOT_ARTIFACT, VitestStatus,
    classify_case_status, read_task_observation,
};
use serde_json::{Value, json};

const SESSION: &str = "{\"type\":\"session\"}\n";

fn task() -> EvalTask {
    EvalTask {
        file: "evals/example.docs.eval.ts".to_string(),
        full_name: "Example workflow > handles the case".to_string(),
        eval_set: "Example workflow".to_string(),
        case_id: "handles the case".to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
    }
}

fn write_task_report(directory: &Path, status: &str, meta: Value) -> PathBuf {
    let report = task_report(status, meta);
    let path = directory.join("vitest.json");
    std::fs::write(&path, serde_json::to_string(&report).unwrap()).unwrap();
    path
}

/// `scoredMeta` 的覆盖项：`None` 表示使用 TS 默认值。
#[derive(Default)]
struct MetaOverrides {
    avg_score: Option<Value>,
    model: Option<String>,
    errors: Option<Value>,
    artifacts: Option<Value>,
    /// 直接替换 harness.run（用于非法形状的用例）。
    run: Option<Value>,
    /// 直接替换 harness 对象（用于严格校验用例）。
    harness: Option<Value>,
}

fn scored_meta(overrides: MetaOverrides) -> Value {
    if let Some(harness) = overrides.harness {
        return json!({ "eval": default_eval(overrides.avg_score), "harness": harness });
    }
    let avg_score = overrides.avg_score.unwrap_or(json!(0.5));
    let model = overrides.model.unwrap_or_else(|| "model".to_string());
    let errors = overrides.errors.unwrap_or(json!([]));
    let artifacts = overrides
        .artifacts
        .unwrap_or_else(|| json!({ "runId": "run-1", KIANA_SESSION_SNAPSHOT_ARTIFACT: SESSION }));
    let run = overrides.run.unwrap_or_else(|| {
        json!({
            "output": { "ok": true },
            "session": { "events": [{ "type": "message", "role": "user", "content": "prompt" }] },
            "usage": {
                "provider": "fixture",
                "model": model,
                "inputTokens": 10,
                "outputTokens": 5,
                "totalTokens": 15,
                "toolCalls": 1,
                "metadata": { "cacheReadTokens": 2, "cacheWriteTokens": 3, "estimatedCostUsd": 0.01 }
            },
            "timings": { "totalMs": 1234 },
            "artifacts": artifacts,
            "errors": errors,
        })
    });
    json!({
        "eval": {
            "avgScore": avg_score,
            "scores": [{ "name": "StructuredOutputJudge", "score": 0.5 }],
            "thresholdFailed": false,
        },
        "harness": { "name": "without_docs", "run": run },
    })
}

fn default_eval(avg_score: Option<Value>) -> Value {
    json!({
        "avgScore": avg_score.unwrap_or(json!(0.5)),
        "scores": [{ "name": "StructuredOutputJudge", "score": 0.5 }],
        "thresholdFailed": false,
    })
}

async fn read_observation(directory: &Path, status: &str, meta: Value) -> EvalObservation {
    let report_path = write_task_report(directory, status, meta);
    read_task_observation(&task(), &report_path, directory)
        .await
        .expect("readTaskObservation does not fail on well-formed I/O")
}

fn scored_observation() -> EvalObservation {
    EvalObservation {
        eval_set: task().eval_set,
        case_id: task().case_id,
        variant: task().variant,
        model: task().model,
        run_number: task().run_number,
        input_tokens: Some(JsNumber(10.0)),
        output_tokens: Some(JsNumber(5.0)),
        cache_read_tokens: Some(JsNumber(2.0)),
        cache_write_tokens: Some(JsNumber(3.0)),
        total_tokens: Some(JsNumber(15.0)),
        tool_calls: Some(JsNumber(1.0)),
        total_ms: Some(JsNumber(1234.0)),
        estimated_cost_usd: Some(JsNumber(0.01)),
        outcome: EvalOutcome::Scored,
        score: Some(JsNumber(0.5)),
    }
}

fn session_entries(directory: &Path) -> Vec<PathBuf> {
    let sessions = directory.join(task().variant.as_str()).join("sessions");
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return Vec::new();
    };
    entries.map(|entry| entry.unwrap().path()).collect()
}

fn assert_single_session_file(directory: &Path) {
    let entries = session_entries(directory);
    assert_eq!(entries.len(), 1, "expected exactly one session directory");
    let content = std::fs::read_to_string(entries[0].join("session.jsonl")).unwrap();
    assert_eq!(content, SESSION);
}

/// TS: `classifyCaseStatus > maps %s to skipped`（`it.each(["skipped","todo","disabled"])`）
#[test]
fn maps_statuses_to_skipped() {
    for status in [
        VitestStatus::Skipped,
        VitestStatus::Todo,
        VitestStatus::Disabled,
    ] {
        assert_eq!(classify_case_status(status), Some(EvalOutcome::Skipped));
    }
}

/// TS: `classifyCaseStatus > maps failed infrastructure to errored`
#[test]
fn maps_failed_infrastructure_to_errored() {
    assert_eq!(
        classify_case_status(VitestStatus::Failed),
        Some(EvalOutcome::Errored)
    );
}

/// TS: `classifyCaseStatus` 的其余分支（`pending` 与 `passed`）。
#[test]
fn maps_pending_and_passed_statuses() {
    assert_eq!(
        classify_case_status(VitestStatus::Pending),
        Some(EvalOutcome::Pending)
    );
    assert_eq!(classify_case_status(VitestStatus::Passed), None);
}

/// TS: `readTaskObservation > preserves a skipped outcome when no harness run exists`
#[tokio::test]
async fn preserves_a_skipped_outcome_when_no_harness_run_exists() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(directory.path(), "skipped", json!({})).await;
    assert_eq!(observation.outcome, EvalOutcome::Skipped);
    assert_eq!(observation.input_tokens, None);
}

/// TS: `readTaskObservation > preserves a pending outcome when no harness run exists`
#[tokio::test]
async fn preserves_a_pending_outcome_when_no_harness_run_exists() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(directory.path(), "pending", json!({})).await;
    assert_eq!(observation.outcome, EvalOutcome::Pending);
    assert_eq!(observation.input_tokens, None);
}

/// TS: `readTaskObservation > preserves metrics from a failed eval with a partial harness run`
#[tokio::test]
async fn preserves_metrics_from_a_failed_eval_with_a_partial_harness_run() {
    let directory = tempfile::tempdir().unwrap();
    let meta = scored_meta(MetaOverrides {
        errors: Some(json!([{ "message": "Prompt verification failed" }])),
        ..MetaOverrides::default()
    });
    let observation = read_observation(directory.path(), "failed", meta).await;
    let mut expected = scored_observation();
    expected.outcome = EvalOutcome::Errored;
    expected.score = None;
    assert_eq!(observation, expected);
    assert_single_session_file(directory.path());
}

/// TS: `readTaskObservation > records an errored outcome when a passed eval has no harness run`
#[tokio::test]
async fn records_an_errored_outcome_when_a_passed_eval_has_no_harness_run() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(directory.path(), "passed", json!({})).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, None);
    assert!(session_entries(directory.path()).is_empty());
}

/// TS: `readTaskObservation > reads a scored harness run and persists the session artifact`
#[tokio::test]
async fn reads_a_scored_harness_run_and_persists_the_session_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides::default()),
    )
    .await;
    let expected = scored_observation();
    assert_eq!(observation, expected);
    assert_eq!(
        serde_json::to_string(&observation).unwrap(),
        r#"{"evalSet":"Example workflow","caseId":"handles the case","variant":"without_docs","model":"fixture/model","runNumber":1,"inputTokens":10,"outputTokens":5,"cacheReadTokens":2,"cacheWriteTokens":3,"totalTokens":15,"toolCalls":1,"totalMs":1234,"estimatedCostUsd":0.01,"outcome":"scored","score":0.5}"#
    );
    assert_single_session_file(directory.path());
}

/// `session.jsonl` 的目录/文件权限（Node `mkdir(...,{mode:0o700})` / `writeFile(...,{mode:0o600})`）。
#[cfg(unix)]
#[tokio::test]
async fn persists_the_session_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides::default()),
    )
    .await;
    let entries = session_entries(directory.path());
    assert_eq!(entries.len(), 1);
    let directory_mode = std::fs::metadata(&entries[0]).unwrap().permissions().mode() & 0o777;
    let file_mode = std::fs::metadata(entries[0].join("session.jsonl"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    assert_eq!(file_mode, 0o600);
}

/// TS: `readTaskObservation > treats a zero score as scored data`
#[tokio::test]
async fn treats_a_zero_score_as_scored_data() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            avg_score: Some(json!(0)),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.0)));
}

/// TS: `readTaskObservation > records an unscored outcome when a completed eval has no score`
#[tokio::test]
async fn records_an_unscored_outcome_when_a_completed_eval_has_no_score() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            avg_score: Some(json!(null)),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Unscored);
    assert_eq!(observation.score, None);
}

/// TS: `readTaskObservation > records an errored outcome when the reported model does not match the task`
#[tokio::test]
async fn records_an_errored_outcome_when_the_reported_model_does_not_match_the_task() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            model: Some("other".to_string()),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(
        observation.input_tokens, None,
        "identity-only errored observation"
    );
    // TS 在模型检查之前就写出会话快照。
    assert_single_session_file(directory.path());
}

/// TS: `readTaskObservation > records an errored outcome when a completed harness run contains errors`
#[tokio::test]
async fn records_an_errored_outcome_when_a_completed_harness_run_contains_errors() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            errors: Some(json!([{ "message": "boom" }])),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, Some(JsNumber(10.0)));
    assert_eq!(observation.score, None);
}

/// TS: `readTaskObservation > still scores a completed eval when the session artifact is missing`
#[tokio::test]
async fn still_scores_a_completed_eval_when_the_session_artifact_is_missing() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            artifacts: Some(json!({ "runId": "run-1" })),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
    assert!(session_entries(directory.path()).is_empty());
}

/// TS: `actualModel = run.usage.provider && run.usage.model ? ... : undefined`：任一为空串都会
/// 让观测变成 errored（空串是 falsy）。
#[tokio::test]
async fn treats_an_empty_provider_or_model_as_errored() {
    for usage in [
        json!({ "provider": "", "model": "model", "inputTokens": 10 }),
        json!({ "provider": "fixture", "model": "", "inputTokens": 10 }),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let run = json!({
            "session": { "events": [] },
            "usage": usage,
            "errors": [],
        });
        let observation = read_observation(
            directory.path(),
            "passed",
            scored_meta(MetaOverrides {
                run: Some(run),
                ..MetaOverrides::default()
            }),
        )
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Errored);
        assert_eq!(observation.input_tokens, None);
    }
}

/// `persistSession` 的 I/O 失败不被捕获（TS 的 rejection 冒泡），错误文本是 Node 的
/// `SystemError` 形状：`fs.promises.mkdir` 的失败路径是失败的组件。
#[tokio::test]
async fn persist_session_errors_use_node_system_error_text() {
    let directory = tempfile::tempdir().unwrap();
    // `<artifact>/without_docs` 是普通文件 → mkdir 的祖先 EEXIST + 非目录 → ENOTDIR。
    std::fs::write(directory.path().join("without_docs"), "x").unwrap();
    let report_path = write_task_report(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides::default()),
    );
    let error = read_task_observation(&task(), &report_path, directory.path())
        .await
        .unwrap_err();
    // Node 的 `mkdir(recursive)` 在 ENOTDIR 时报告最初请求的完整路径。
    assert_eq!(
        error.to_string(),
        format!(
            "ENOTDIR: not a directory, mkdir '{}'",
            directory
                .path()
                .join("without_docs")
                .join("sessions")
                .join("886d4c2a51bad61e71d965ae2f45b6bdacc227ccc62708c130e7790075e472bf")
                .to_string_lossy()
        )
    );
}

/// `persistSession` 只在 artifact 是字符串时写文件（`typeof session !== "string"` → 直接返回）。
#[tokio::test]
async fn ignores_a_non_string_session_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            artifacts: Some(json!({ "runId": "run-1", KIANA_SESSION_SNAPSHOT_ARTIFACT: 42 })),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert!(session_entries(directory.path()).is_empty());
}

// ---------------------------------------------------------------------------
// 顺序/校验的补充用例
// ---------------------------------------------------------------------------

/// session 在 metrics 校验失败之前写出；失败的 metrics 不进入观测。
#[tokio::test]
async fn persists_the_session_before_rejecting_invalid_metrics() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [] },
        "usage": { "provider": "fixture", "model": "model", "inputTokens": -1 },
        "artifacts": { "runId": "run-1", KIANA_SESSION_SNAPSHOT_ARTIFACT: SESSION },
        "errors": [],
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, None);
    assert_single_session_file(directory.path());
}

/// 元数据里的非数字 metric 走同一个 try/catch（session 已写出，metrics 全丢）。
#[tokio::test]
async fn rejects_non_numeric_metadata_metrics() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [] },
        "usage": {
            "provider": "fixture",
            "model": "model",
            "inputTokens": 10,
            "metadata": { "cacheReadTokens": "two" }
        },
        "artifacts": { "runId": "run-1", KIANA_SESSION_SNAPSHOT_ARTIFACT: SESSION },
        "errors": [],
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, None);
    assert_single_session_file(directory.path());
}

/// 非法的分数在 errors/metrics 之后判定：观测保留 metrics，session 已写出。
#[tokio::test]
async fn persists_the_session_before_rejecting_an_invalid_score() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            avg_score: Some(json!(2)),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, Some(JsNumber(10.0)));
    assert_eq!(observation.score, None);
    assert_single_session_file(directory.path());
}

/// zod `.strict()`：harness run 里的未知键让该断言不产生 workspace case。
#[tokio::test]
async fn rejects_unknown_harness_run_fields() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [] },
        "usage": { "provider": "fixture", "model": "model" },
        "errors": [],
        "unexpected": true,
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, None);
}

/// zod `.optional()` 拒绝 `null`（只有缺失才是 `undefined`）。
#[tokio::test]
async fn rejects_null_optional_fields() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [] },
        "usage": { "provider": null, "model": "model" },
        "errors": [],
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, None);
}

/// transcript 事件也是严格对象；未知键让 meta 校验失败。
#[tokio::test]
async fn rejects_unknown_transcript_event_fields() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [{ "type": "message", "role": "user", "unexpected": 1 }] },
        "usage": { "provider": "fixture", "model": "model" },
        "errors": [],
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// `readTaskObservation` 要求报告里恰好一条断言；两条 → errored。
#[tokio::test]
async fn requires_exactly_one_assertion() {
    let directory = tempfile::tempdir().unwrap();
    let report = json!({
        "numFailedTests": 0,
        "numPassedTests": 2,
        "numPendingTests": 0,
        "numTodoTests": 0,
        "numTotalTests": 2,
        "startTime": 0,
        "success": true,
        "testResults": [{
            "message": "",
            "name": "/repo/packages/evals/evals/example.docs.eval.ts",
            "status": "passed",
            "assertionResults": [
                { "ancestorTitles": ["Example workflow"], "fullName": "Example workflow handles the case", "status": "passed", "title": "handles the case", "meta": scored_meta(MetaOverrides::default()) },
                { "ancestorTitles": ["Example workflow"], "fullName": "Example workflow handles the case", "status": "passed", "title": "handles the case", "meta": scored_meta(MetaOverrides::default()) }
            ]
        }]
    });
    let report_path = directory.path().join("vitest.json");
    std::fs::write(&report_path, serde_json::to_string(&report).unwrap()).unwrap();
    let observation = read_task_observation(&task(), &report_path, directory.path())
        .await
        .unwrap();
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// 断言 `fullName` 与 `"<evalSet> <caseId>"` 不一致 → errored。
#[tokio::test]
async fn rejects_a_mismatched_assertion_name() {
    let directory = tempfile::tempdir().unwrap();
    let report = json!({
        "numFailedTests": 0,
        "numPassedTests": 1,
        "numPendingTests": 0,
        "numTodoTests": 0,
        "numTotalTests": 1,
        "startTime": 0,
        "success": true,
        "testResults": [{
            "message": "",
            "name": "f",
            "status": "passed",
            "assertionResults": [{ "fullName": "Other name", "status": "passed", "title": "t" }]
        }]
    });
    let report_path = directory.path().join("vitest.json");
    std::fs::write(&report_path, serde_json::to_string(&report).unwrap()).unwrap();
    let observation = read_task_observation(&task(), &report_path, directory.path())
        .await
        .unwrap();
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// 报告缺失/不是合法 JSON → errored（而不是 Err）。
#[tokio::test]
async fn missing_or_malformed_reports_are_errored_observations() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.json");
    let observation = read_task_observation(&task(), &missing, directory.path())
        .await
        .unwrap();
    assert_eq!(observation.outcome, EvalOutcome::Errored);

    let malformed = directory.path().join("malformed.json");
    std::fs::write(&malformed, b"not json").unwrap();
    let observation = read_task_observation(&task(), &malformed, directory.path())
        .await
        .unwrap();
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// `erroredObservation` 只产生身份 + outcome。
#[test]
fn errored_observation_is_identity_only() {
    let observation = kiana_evals::report::errored_observation(&task());
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.eval_set, task().eval_set);
    assert_eq!(observation.case_id, task().case_id);
    assert_eq!(observation.variant, task().variant);
    assert_eq!(observation.model, task().model);
    assert_eq!(observation.run_number, task().run_number);
    assert_eq!(observation.score, None);
    assert_eq!(observation.total_tokens, None);
    assert_eq!(KIANA_SESSION_SNAPSHOT_ARTIFACT, "kianaSessionJsonl");
}

/// 未评分观测的 JSON 不包含 score（`undefined` 在 JSON 里被省略）。
#[test]
fn unscored_observation_json_omits_score() {
    let observation = EvalObservation {
        eval_set: "Example workflow".to_string(),
        case_id: "handles the case".to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: Some(JsNumber(15.0)),
        tool_calls: None,
        total_ms: None,
        estimated_cost_usd: None,
        outcome: EvalOutcome::Unscored,
        score: None,
    };
    assert_eq!(
        serde_json::to_string(&observation).unwrap(),
        r#"{"evalSet":"Example workflow","caseId":"handles the case","variant":"without_docs","model":"fixture/model","runNumber":1,"totalTokens":15,"outcome":"unscored"}"#
    );
}

/// 反序列化观测（供 CLI/测试读取 `observations.jsonl`）。
#[test]
fn observations_round_trip_through_json() {
    let observation = scored_observation();
    let text = serde_json::to_string(&observation).unwrap();
    let parsed: EvalObservation = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, observation);
}

/// `collectReportWorkspace` 在「只有 harness、没有 eval」时合成 `avgScore`：
/// passed → 1。
#[tokio::test]
async fn synthesizes_a_passing_score_when_only_harness_meta_exists() {
    let directory = tempfile::tempdir().unwrap();
    let meta = json!({
        "harness": {
            "name": "without_docs",
            "run": {
                "session": { "events": [] },
                "usage": { "provider": "fixture", "model": "model" },
                "errors": [],
            }
        }
    });
    let observation = read_observation(directory.path(), "passed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(1.0)));
}

/// `failed` + 只有 harness → 合成 `avgScore: 0`，但 run 的错误列表为空、断言状态 failed
/// 仍然让观测变成 errored（保留 metrics）。
#[tokio::test]
async fn harness_only_failed_case_is_errored() {
    let directory = tempfile::tempdir().unwrap();
    let meta = json!({
        "harness": {
            "name": "without_docs",
            "run": {
                "session": { "events": [] },
                "usage": { "provider": "fixture", "model": "model", "inputTokens": 7 },
                "errors": [],
            }
        }
    });
    let observation = read_observation(directory.path(), "failed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.input_tokens, Some(JsNumber(7.0)));
}

/// skipped 在 workspace/harness 检查之前返回：即使存在合法 run 也不写 session。
#[tokio::test]
async fn skipped_outcome_returns_before_persisting_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let observation = read_observation(
        directory.path(),
        "skipped",
        scored_meta(MetaOverrides::default()),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Skipped);
    assert!(session_entries(directory.path()).is_empty());
}

/// eval 元数据同样严格：未知键让断言不产生 workspace case。
#[tokio::test]
async fn rejects_unknown_eval_meta_fields() {
    let directory = tempfile::tempdir().unwrap();
    let meta = json!({
        "eval": { "avgScore": 0.5, "unexpected": true },
        "harness": {
            "name": "without_docs",
            "run": {
                "session": { "events": [] },
                "usage": { "provider": "fixture", "model": "model" },
                "errors": [],
            }
        }
    });
    let observation = read_observation(directory.path(), "passed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// `eval: null` 不是 `undefined`：zod 的 `.optional()` 拒绝 null，整个 meta 丢弃。
#[tokio::test]
async fn rejects_null_eval_meta() {
    let directory = tempfile::tempdir().unwrap();
    let meta = json!({ "eval": null });
    let observation = read_observation(directory.path(), "passed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// session 目录名 = `sha256(JSON.stringify([evalSet,caseId,variant,model,runNumber]))`
/// （期望值由 Node 26 生成）。
#[test]
fn session_directory_name_matches_node_sha256() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        read_observation(
            directory.path(),
            "passed",
            scored_meta(MetaOverrides::default()),
        )
        .await;
    });
    let entries = session_entries(directory.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].file_name().unwrap().to_string_lossy(),
        "886d4c2a51bad61e71d965ae2f45b6bdacc227ccc62708c130e7790075e472bf"
    );
}

/// `ToolCallSchema` 的每个 variant 都是严格对象：未知键让 meta 失效。
#[tokio::test]
async fn rejects_unknown_tool_call_fields() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [] },
        "usage": { "provider": "fixture", "model": "model" },
        "errors": [],
    });
    let meta = json!({
        "eval": {
            "avgScore": 0.5,
            "toolCalls": [{ "status": "ok", "name": "read", "unexpected": 1 }]
        },
        "harness": { "name": "without_docs", "run": run }
    });
    let observation = read_observation(directory.path(), "passed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);

    // 合法形状（含 `result`）通过校验并正常评分。
    let run = json!({
        "session": { "events": [] },
        "usage": { "provider": "fixture", "model": "model" },
        "errors": [],
    });
    let meta = json!({
        "eval": {
            "avgScore": 0.5,
            "toolCalls": [{ "status": "ok", "name": "read", "result": { "ok": true } }]
        },
        "harness": { "name": "without_docs", "run": run }
    });
    let observation = read_observation(directory.path(), "passed", meta).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
}

/// `HarnessRunSchema` 的必填键：`session`、`usage`、`errors` 缺一不可，`errors` 必须是对象数组。
#[tokio::test]
async fn rejects_incomplete_harness_runs() {
    let directory = tempfile::tempdir().unwrap();
    let runs = [
        json!({ "usage": {}, "errors": [] }),
        json!({ "session": { "events": [] }, "errors": [] }),
        json!({ "session": { "events": [] }, "usage": {} }),
        json!({ "session": { "events": [] }, "usage": {}, "errors": [1] }),
    ];
    for run in runs {
        let observation = read_observation(
            directory.path(),
            "passed",
            scored_meta(MetaOverrides {
                run: Some(run),
                ..MetaOverrides::default()
            }),
        )
        .await;
        assert_eq!(observation.outcome, EvalOutcome::Errored);
        assert_eq!(observation.input_tokens, None);
    }
}

/// transcript 事件按 TS 的键序序列化，`durationMs` 走 JsNumber（整数不带 `.0`）。
#[test]
fn transcript_events_serialize_with_typescript_key_order() {
    use kiana_evals::report::{TranscriptEvent, TranscriptRole};

    assert_eq!(
        serde_json::to_string(&TranscriptEvent::Message {
            role: TranscriptRole::User,
            content: Some(json!("prompt")),
            metadata: None,
        })
        .unwrap(),
        r#"{"type":"message","role":"user","content":"prompt"}"#
    );
    assert_eq!(
        serde_json::to_string(&TranscriptEvent::ToolCall {
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments: None,
            started_at: Some("2026-01-01T00:00:00.000Z".to_string()),
            finished_at: None,
            duration_ms: Some(JsNumber(5.0)),
            metadata: None,
        })
        .unwrap(),
        r#"{"type":"tool_call","id":"call-1","name":"read","startedAt":"2026-01-01T00:00:00.000Z","durationMs":5}"#
    );
    assert_eq!(
        serde_json::to_string(&TranscriptEvent::ToolResult {
            tool_call_id: "call-1".to_string(),
            name: Some("read".to_string()),
            content: Some(json!("text")),
            error: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            metadata: None,
        })
        .unwrap(),
        r#"{"type":"tool_result","toolCallId":"call-1","name":"read","content":"text"}"#
    );
}

/// `durationMs` 是 optional finite（不接受 `null`）。
#[tokio::test]
async fn rejects_null_transcript_duration() {
    let directory = tempfile::tempdir().unwrap();
    let run = json!({
        "session": { "events": [{ "type": "tool_call", "id": "call-1", "name": "read", "durationMs": null }] },
        "usage": { "provider": "fixture", "model": "model" },
        "errors": [],
    });
    let observation = read_observation(
        directory.path(),
        "passed",
        scored_meta(MetaOverrides {
            run: Some(run),
            ..MetaOverrides::default()
        }),
    )
    .await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
}

/// 与 [`write_task_report`] 相同的报告值，便于先序列化再注入 V8-only 字面量。
fn task_report(status: &str, meta: Value) -> Value {
    json!({
        "numFailedTests": 0,
        "numPassedTests": if status == "passed" { 1 } else { 0 },
        "numPendingTests": if status == "pending" || status == "skipped" { 1 } else { 0 },
        "numTodoTests": 0,
        "numTotalTests": 1,
        "startTime": 0,
        "success": true,
        "testResults": [
            {
                "message": "",
                "name": "/repo/packages/evals/evals/example.docs.eval.ts",
                "status": "passed",
                "assertionResults": [
                    {
                        "ancestorTitles": [task().eval_set],
                        "fullName": format!("{} {}", task().eval_set, task().case_id),
                        "status": status,
                        "title": task().case_id,
                        "failureMessages": [],
                        "meta": meta,
                    }
                ],
            }
        ],
    })
}

async fn read_raw_report(directory: &Path, text: &str) -> EvalObservation {
    let report_path = directory.join("vitest.json");
    std::fs::write(&report_path, text).unwrap();
    read_task_observation(&task(), &report_path, directory)
        .await
        .expect("readTaskObservation does not fail on well-formed I/O")
}

/// V8 的 `JSON.parse` 把 `1e999` 解析成 `Infinity`；zod 的 `NullableFiniteNumberSchema`
/// 把 `avgScore: Infinity` 预处理成 `null`，于是观测是 `unscored`（仍带 metrics）。
#[tokio::test]
async fn treats_an_overflowing_avg_score_as_unscored() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace("\"avgScore\":0.5", "\"avgScore\":1e999");
    assert!(text.contains("\"avgScore\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Unscored);
    assert_eq!(observation.score, None);
    assert_eq!(observation.total_tokens, Some(JsNumber(15.0)));
}

/// `scores[].score` 同样走 `NullableFiniteNumberSchema`：溢出的单项分数变成 `null`，
/// 平均分不受影响。
#[tokio::test]
async fn treats_an_overflowing_score_item_as_null() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"name\":\"StructuredOutputJudge\",\"score\":0.5",
        "\"name\":\"StructuredOutputJudge\",\"score\":1e999",
    );
    assert!(text.contains("\"score\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}

/// 文件级 `startTime`/`endTime` 走 `OptionalFiniteNumberSchema`：溢出数字被当成缺失，
/// 报告仍然有效，观测正常。
#[tokio::test]
async fn ignores_overflowing_file_timestamps() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"status\":\"passed\",\"assertionResults\"",
        "\"status\":\"passed\",\"startTime\":1e999,\"endTime\":-1e999,\"assertionResults\"",
    );
    assert!(text.contains("\"startTime\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}

/// `duration` 是 `FiniteNumberSchema`（没有 preprocess）：溢出数字让整份报告无效。
#[tokio::test]
async fn rejects_an_overflowing_assertion_duration() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"title\":\"handles the case\",\"failureMessages\"",
        "\"title\":\"handles the case\",\"duration\":1e999,\"failureMessages\"",
    );
    assert!(text.contains("\"duration\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.total_tokens, None);
}

/// harness run 内部任意 `JsonValue`/finite 字段出现溢出数字：整个 harness meta 校验失败，
/// 该断言不产生 workspace case（与 zod 行为一致）。
#[tokio::test]
async fn rejects_overflowing_numbers_inside_the_harness_meta() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"estimatedCostUsd\":0.01",
        "\"estimatedCostUsd\":0.01,\"overflow\":1e999",
    );
    assert!(text.contains("\"overflow\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.total_tokens, None);
}

/// `usage.totalTokens` 是 `FiniteNumberSchema.optional()`（没有 preprocess）：溢出同样让
/// harness meta 无效。
#[tokio::test]
async fn rejects_an_overflowing_usage_metric() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace("\"totalTokens\":15", "\"totalTokens\":1e999");
    assert!(text.contains("\"totalTokens\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Errored);
    assert_eq!(observation.total_tokens, None);
}

/// V8 的 `JSON.parse` 接受未配对的代理项转义；Rust `String` 无法表示，读取器按 Node 写 UTF-8
/// 时的行为替换成 U+FFFD，报告仍然有效。
#[tokio::test]
async fn accepts_lone_surrogate_escapes() {
    let directory = tempfile::tempdir().unwrap();
    let backslash = '\\';
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"message\":\"\"",
        &format!("\"message\":\"bad {backslash}ud800 name\""),
    );
    assert!(text.contains(&format!("{backslash}ud800")));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}

/// V8 的 `JSON.parse` 对重复键保留最后一个值（`serde` 的 derive 会报 `duplicate field`）。
/// `avgScore` 重复时按最后的值评分。
#[tokio::test]
async fn last_duplicate_key_wins_like_json_parse() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace("\"avgScore\":0.5", "\"avgScore\":0.1,\"avgScore\":0.5");
    assert!(text.contains("\"avgScore\":0.1,\"avgScore\":0.5"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}

/// 类型化字段（文件 `status`）上的重复键同样取最后一个值：报告仍然有效并正常评分。
#[tokio::test]
async fn duplicate_typed_fields_take_the_last_value() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"status\":\"passed\",\"assertionResults\"",
        "\"status\":\"passed\",\"status\":\"failed\",\"assertionResults\"",
    );
    assert!(text.contains("\"status\":\"passed\",\"status\":\"failed\""));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}

/// 顶层 passthrough 字段里的溢出数字：zod 保留但不使用，报告仍然有效。
#[tokio::test]
async fn ignores_overflowing_numbers_in_unknown_passthrough_fields() {
    let directory = tempfile::tempdir().unwrap();
    let text = serde_json::to_string(&task_report(
        "passed",
        scored_meta(MetaOverrides::default()),
    ))
    .unwrap()
    .replace(
        "\"success\":true",
        "\"success\":true,\"unknownOverflow\":1e999",
    );
    assert!(text.contains("\"unknownOverflow\":1e999"));
    let observation = read_raw_report(directory.path(), &text).await;
    assert_eq!(observation.outcome, EvalOutcome::Scored);
    assert_eq!(observation.score, Some(JsNumber(0.5)));
}
