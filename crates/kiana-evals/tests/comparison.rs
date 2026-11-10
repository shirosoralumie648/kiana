//! `packages/evals/test/comparison.test.ts` 的移植，外加 report.json / report.txt 的逐字黄金测试。

use kiana_evals::plan::DocumentationVariant;
use kiana_evals::report::{
    EvalObservation, EvalOutcome, ExpectedEvalRun, JsNumber, PairedMetricSummary,
    format_eval_comparison_report, summarize_eval_observations,
};

/// `scored()` 的 metrics 覆盖项（`None` 使用 TS helper 的默认值）。
#[derive(Clone, Copy, Default)]
struct MetricOverrides {
    total_tokens: Option<f64>,
    tool_calls: Option<f64>,
    total_ms: Option<f64>,
    estimated_cost_usd: Option<f64>,
}

fn scored(
    variant: DocumentationVariant,
    run_number: u64,
    score: f64,
    overrides: MetricOverrides,
) -> EvalObservation {
    EvalObservation {
        eval_set: "tool access".to_string(),
        case_id: "create".to_string(),
        variant,
        model: "fixture/model".to_string(),
        run_number,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: Some(JsNumber(overrides.total_tokens.unwrap_or(100.0))),
        tool_calls: Some(JsNumber(overrides.tool_calls.unwrap_or(2.0))),
        total_ms: Some(JsNumber(overrides.total_ms.unwrap_or(1000.0))),
        estimated_cost_usd: Some(JsNumber(overrides.estimated_cost_usd.unwrap_or(0.01))),
        outcome: EvalOutcome::Scored,
        score: Some(JsNumber(score)),
    }
}

fn errored(
    variant: DocumentationVariant,
    run_number: u64,
    total_tokens: Option<f64>,
) -> EvalObservation {
    EvalObservation {
        eval_set: "tool access".to_string(),
        case_id: "create".to_string(),
        variant,
        model: "fixture/model".to_string(),
        run_number,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: total_tokens.map(JsNumber),
        tool_calls: None,
        total_ms: None,
        estimated_cost_usd: None,
        outcome: EvalOutcome::Errored,
        score: None,
    }
}

fn expected_for(run_numbers: &[u64]) -> Vec<ExpectedEvalRun> {
    let mut expected = Vec::new();
    for run_number in run_numbers {
        for variant in [
            DocumentationVariant::WithoutDocs,
            DocumentationVariant::WithDocs,
        ] {
            expected.push(ExpectedEvalRun {
                eval_set: "tool access".to_string(),
                case_id: "create".to_string(),
                variant,
                model: "fixture/model".to_string(),
                run_number: *run_number,
            });
        }
    }
    expected
}

fn canonical_observations() -> Vec<EvalObservation> {
    vec![
        scored(
            DocumentationVariant::WithoutDocs,
            1,
            0.0,
            MetricOverrides {
                tool_calls: Some(3.0),
                ..MetricOverrides::default()
            },
        ),
        scored(
            DocumentationVariant::WithDocs,
            1,
            1.0,
            MetricOverrides {
                total_tokens: Some(120.0),
                total_ms: Some(800.0),
                ..MetricOverrides::default()
            },
        ),
        scored(
            DocumentationVariant::WithoutDocs,
            2,
            1.0,
            MetricOverrides {
                total_tokens: Some(200.0),
                ..MetricOverrides::default()
            },
        ),
        scored(
            DocumentationVariant::WithDocs,
            2,
            1.0,
            MetricOverrides {
                total_tokens: Some(180.0),
                ..MetricOverrides::default()
            },
        ),
    ]
}

/// TS 的 `stripVTControlCharacters`：`formatEvalComparisonReport` 只在 stdout 是 TTY 时给标题
/// 加粗（`\x1b[1m`…`\x1b[22m`），测试进程通常是管道，但交互式运行 `cargo test` 时 stdout 可能是
/// 终端；去掉这两段后仍与黄金文件逐字比较。
fn strip_style(text: &str) -> String {
    text.replace("\u{1b}[1m", "").replace("\u{1b}[22m", "")
}

fn metric(
    eligible_pairs: usize,
    control_mean: f64,
    treatment_mean: f64,
    mean_delta: f64,
) -> PairedMetricSummary {
    PairedMetricSummary {
        eligible_pairs,
        control_mean: Some(JsNumber(control_mean)),
        treatment_mean: Some(JsNumber(treatment_mean)),
        mean_delta: Some(JsNumber(mean_delta)),
    }
}

/// TS: `summarizeEvalObservations > computes paired lift and efficiency deltas`
#[test]
fn computes_paired_lift_and_efficiency_deltas() {
    let observations = canonical_observations();
    let report = summarize_eval_observations("digest", &expected_for(&[1, 2]), &observations);
    assert_eq!(report.comparisons.len(), 1);
    let comparison = &report.comparisons[0];
    assert_eq!(comparison.eval_set, "tool access");
    assert_eq!(comparison.total_pairs, 2);
    assert_eq!(comparison.eligible_pairs, 2);
    assert_eq!(comparison.blocked_pairs, 0);
    assert_eq!(comparison.control_pass_rate, Some(JsNumber(0.5)));
    assert_eq!(comparison.treatment_pass_rate, Some(JsNumber(1.0)));
    assert_eq!(comparison.lift, Some(JsNumber(0.5)));
    assert_eq!(comparison.total_tokens, metric(2, 150.0, 150.0, 0.0));
    assert_eq!(comparison.tool_calls, metric(2, 2.5, 2.0, -0.5));
}

/// TS: `summarizeEvalObservations > fails closed for incomplete and errored pairs while retaining totals`
#[test]
fn fails_closed_for_incomplete_and_errored_pairs_while_retaining_totals() {
    let observations = vec![
        scored(
            DocumentationVariant::WithoutDocs,
            1,
            0.0,
            MetricOverrides::default(),
        ),
        errored(DocumentationVariant::WithDocs, 1, Some(120.0)),
        scored(
            DocumentationVariant::WithoutDocs,
            2,
            1.0,
            MetricOverrides {
                total_tokens: Some(200.0),
                ..MetricOverrides::default()
            },
        ),
    ];
    let report = summarize_eval_observations("digest", &expected_for(&[1, 2]), &observations);
    let comparison = &report.comparisons[0];
    assert_eq!(comparison.total_pairs, 2);
    assert_eq!(comparison.eligible_pairs, 0);
    assert_eq!(comparison.blocked_pairs, 2);
    assert_eq!(comparison.lift, None);

    assert_eq!(report.blocked_pairs.len(), 2);
    assert_eq!(report.blocked_pairs[0].run_number, 1);
    assert_eq!(report.blocked_pairs[0].reasons, vec!["with_docs: errored"]);
    assert_eq!(report.blocked_pairs[1].run_number, 2);
    assert_eq!(
        report.blocked_pairs[1].reasons,
        vec!["with_docs: expected 1 observation, found 0"]
    );

    assert_eq!(report.operational_totals[0].total_tokens.available_runs, 2);
    assert_eq!(
        report.operational_totals[0].total_tokens.total,
        Some(JsNumber(300.0))
    );
}

/// TS: `summarizeEvalObservations > blocks duplicate observations and keeps missing metrics distinct from zero`
#[test]
fn blocks_duplicate_observations_and_keeps_missing_metrics_distinct_from_zero() {
    let mut without_tokens = scored(
        DocumentationVariant::WithoutDocs,
        1,
        1.0,
        MetricOverrides::default(),
    );
    without_tokens.total_tokens = None;
    let report = summarize_eval_observations(
        "digest",
        &expected_for(&[1]),
        &[
            without_tokens.clone(),
            without_tokens,
            scored(
                DocumentationVariant::WithDocs,
                1,
                1.0,
                MetricOverrides {
                    total_tokens: Some(0.0),
                    ..MetricOverrides::default()
                },
            ),
        ],
    );
    assert_eq!(
        report.blocked_pairs[0].reasons,
        vec!["without_docs: expected 1 observation, found 2"]
    );
    assert_eq!(report.operational_totals[0].total_tokens.available_runs, 0);
    assert_eq!(report.operational_totals[0].total_tokens.total, None);
    assert_eq!(report.operational_totals[1].total_tokens.available_runs, 1);
    assert_eq!(
        report.operational_totals[1].total_tokens.total,
        Some(JsNumber(0.0))
    );
}

/// TS: `summarizeEvalObservations > formats blocked comparisons and operational totals`
#[test]
fn formats_blocked_comparisons_and_operational_totals() {
    let observations = vec![
        scored(
            DocumentationVariant::WithoutDocs,
            1,
            1.0,
            MetricOverrides::default(),
        ),
        scored(
            DocumentationVariant::WithDocs,
            1,
            1.0,
            MetricOverrides::default(),
        ),
    ];
    let report = summarize_eval_observations("digest", &expected_for(&[1, 2]), &observations);
    let formatted = strip_style(&format_eval_comparison_report(&report));
    // TS 同样的断言（stripVTControlCharacters 后 contains）。
    assert!(formatted.contains("Documentation Eval Comparisons"));
    assert!(formatted.contains("Pass rate  withheld because pairs are blocked"));
    assert!(formatted.contains("without_docs: 1 runs"));
    // 完整文本（由 TS 生成），覆盖 blocked 行与 1 pairs 的复数写法。
    assert_eq!(
        formatted,
        "Documentation Eval Comparisons\n  tool access\n         Pairs  1/2 eligible\n     Pass rate  withheld because pairs are blocked\n        Tokens  +0.0 (with 100.0, without 100.0, 1 pairs)\n         Tools  +0.0 (with 2.0, without 2.0, 1 pairs)\n       Latency  +0.0ms (with 1000.0ms, without 1000.0ms, 1 pairs)\n     Est. cost  +$0.0000 (with $0.0100, without $0.0100, 1 pairs)\n  Operational totals\n    without_docs: 1 runs, 100 tokens, 2 tools, 1.00s, $0.0100 cost\n    with_docs: 1 runs, 100 tokens, 2 tools, 1.00s, $0.0100 cost\n  Blocked pairs\n    tool access/create/fixture/model/run-2: without_docs: expected 1 observation, found 0; with_docs: expected 1 observation, found 0"
    );
}

/// `unavailable` 的三种写法：成对指标缺失、运营指标无数据（含 0/0）与 blocked 原因。
#[test]
fn formats_unavailable_metrics_and_empty_variant_totals() {
    let observation = EvalObservation {
        eval_set: "tool access".to_string(),
        case_id: "create".to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        total_tokens: None,
        tool_calls: None,
        total_ms: None,
        estimated_cost_usd: None,
        outcome: EvalOutcome::Scored,
        score: Some(JsNumber(1.0)),
    };
    let report = summarize_eval_observations("digest", &expected_for(&[1]), &[observation]);
    let formatted = strip_style(&format_eval_comparison_report(&report));
    assert_eq!(
        formatted,
        "Documentation Eval Comparisons\n  tool access\n         Pairs  0/1 eligible\n     Pass rate  withheld because pairs are blocked\n        Tokens  unavailable\n         Tools  unavailable\n       Latency  unavailable\n     Est. cost  unavailable\n  Operational totals\n    without_docs: 1 runs, unavailable (0/1 measured), unavailable (0/1 measured), unavailable (0/1 measured), unavailable (0/1 measured)\n    with_docs: 0 runs, unavailable (0/0 measured), unavailable (0/0 measured), unavailable (0/0 measured), unavailable (0/0 measured)\n  Blocked pairs\n    tool access/create/fixture/model/run-1: with_docs: expected 1 observation, found 0"
    );
}

/// 黄金文件：`tests/golden/report_format.txt` 由 TS 直接生成（567 字节、11 个换行、无末尾换行）。
#[test]
fn formats_the_canonical_report_byte_for_byte() {
    let report =
        summarize_eval_observations("digest", &expected_for(&[1, 2]), &canonical_observations());
    let formatted = strip_style(&format_eval_comparison_report(&report));
    let golden = include_str!("golden/report_format.txt");
    assert!(
        !formatted.contains('\u{1b}'),
        "unexpected ANSI escape in report text"
    );
    assert_eq!(formatted, golden);
    assert_eq!(golden.len(), 567);
    assert_eq!(golden.matches('\n').count(), 11);
    assert!(!golden.ends_with('\n'));
}

/// `JSON.stringify(report, null, 2)` 的逐字输出（键序 + JsNumber 数字格式）。
#[test]
fn serializes_report_json_exactly() {
    let report =
        summarize_eval_observations("digest", &expected_for(&[1, 2]), &canonical_observations());
    assert_eq!(serde_json::to_string_pretty(&report).unwrap(), REPORT_JSON);
}

const REPORT_JSON: &str = r#"{
  "schemaVersion": 3,
  "protocolDigest": "digest",
  "control": "without_docs",
  "treatment": "with_docs",
  "comparisons": [
    {
      "evalSet": "tool access",
      "totalPairs": 2,
      "eligiblePairs": 2,
      "blockedPairs": 0,
      "controlPassRate": 0.5,
      "treatmentPassRate": 1,
      "lift": 0.5,
      "flags": [
        "treatment-saturated",
        "flaky"
      ],
      "totalTokens": {
        "eligiblePairs": 2,
        "controlMean": 150,
        "treatmentMean": 150,
        "meanDelta": 0
      },
      "toolCalls": {
        "eligiblePairs": 2,
        "controlMean": 2.5,
        "treatmentMean": 2,
        "meanDelta": -0.5
      },
      "totalMs": {
        "eligiblePairs": 2,
        "controlMean": 1000,
        "treatmentMean": 900,
        "meanDelta": -100
      },
      "estimatedCostUsd": {
        "eligiblePairs": 2,
        "controlMean": 0.01,
        "treatmentMean": 0.01,
        "meanDelta": 0
      }
    }
  ],
  "blockedPairs": [],
  "operationalTotals": [
    {
      "variant": "without_docs",
      "runs": 2,
      "inputTokens": {
        "availableRuns": 0,
        "total": null
      },
      "outputTokens": {
        "availableRuns": 0,
        "total": null
      },
      "cacheReadTokens": {
        "availableRuns": 0,
        "total": null
      },
      "cacheWriteTokens": {
        "availableRuns": 0,
        "total": null
      },
      "totalTokens": {
        "availableRuns": 2,
        "total": 300
      },
      "toolCalls": {
        "availableRuns": 2,
        "total": 5
      },
      "totalMs": {
        "availableRuns": 2,
        "total": 2000
      },
      "estimatedCostUsd": {
        "availableRuns": 2,
        "total": 0.02
      }
    },
    {
      "variant": "with_docs",
      "runs": 2,
      "inputTokens": {
        "availableRuns": 0,
        "total": null
      },
      "outputTokens": {
        "availableRuns": 0,
        "total": null
      },
      "cacheReadTokens": {
        "availableRuns": 0,
        "total": null
      },
      "cacheWriteTokens": {
        "availableRuns": 0,
        "total": null
      },
      "totalTokens": {
        "availableRuns": 2,
        "total": 300
      },
      "toolCalls": {
        "availableRuns": 2,
        "total": 4
      },
      "totalMs": {
        "availableRuns": 2,
        "total": 1800
      },
      "estimatedCostUsd": {
        "availableRuns": 2,
        "total": 0.02
      }
    }
  ]
}"#;

/// 没有 comparison 时返回空字符串（调用方仍会打印一个空行 + Artifacts 行）。
#[test]
fn empty_report_formats_to_an_empty_string() {
    let report = summarize_eval_observations("digest", &[], &[]);
    assert_eq!(format_eval_comparison_report(&report), "");
    assert!(report.comparisons.is_empty());
    assert!(
        report
            .operational_totals
            .iter()
            .all(|totals| totals.runs == 0)
    );
}

/// 分组排序：`localeCompare(evalSet)` → `localeCompare(caseId)` → `localeCompare(model)`
/// → `runNumber` 数值升序（期望值由 Node 26 生成）。
#[test]
fn sorts_groups_by_eval_set_case_id_model_and_run_number() {
    let identity = |eval_set: &str, case_id: &str, run_number: u64| ExpectedEvalRun {
        eval_set: eval_set.to_string(),
        case_id: case_id.to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "m".to_string(),
        run_number,
    };
    let expected = vec![
        identity("zeta", "a", 1),
        identity("alpha", "b", 10),
        identity("alpha", "b", 2),
        identity("alpha", "a", 1),
    ];
    let report = summarize_eval_observations("digest", &expected, &[]);
    let order: Vec<(&str, &str, u64)> = report
        .blocked_pairs
        .iter()
        .map(|pair| {
            (
                pair.eval_set.as_str(),
                pair.case_id.as_str(),
                pair.run_number,
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            ("alpha", "a", 1),
            ("alpha", "b", 2),
            ("alpha", "b", 10),
            ("zeta", "a", 1),
        ]
    );
    let comparisons: Vec<(&str, usize)> = report
        .comparisons
        .iter()
        .map(|comparison| (comparison.eval_set.as_str(), comparison.total_pairs))
        .collect();
    assert_eq!(comparisons, vec![("alpha", 3), ("zeta", 1)]);
    assert_eq!(
        report.blocked_pairs[0].reasons,
        vec![
            "without_docs: expected 1 observation, found 0",
            "with_docs: design expected 1 run, found 0"
        ]
    );
}

/// TS 的 `${total} tokens` 是 `String(number)`，不是 `JSON.stringify`：两个 `1e308` 求和溢出成
/// `Infinity` 时，report.txt 写 `Infinity tokens` / `Infinity tools`；report.json 里的总量
/// 仍然是 `null`（`JSON.stringify(Infinity)`）。
#[test]
fn operational_totals_use_string_number_for_infinite_sums() {
    let observations = vec![
        scored(
            DocumentationVariant::WithoutDocs,
            1,
            1.0,
            MetricOverrides {
                total_tokens: Some(1e308),
                tool_calls: Some(1e308),
                ..MetricOverrides::default()
            },
        ),
        scored(
            DocumentationVariant::WithoutDocs,
            2,
            1.0,
            MetricOverrides {
                total_tokens: Some(1e308),
                tool_calls: Some(1e308),
                ..MetricOverrides::default()
            },
        ),
    ];
    let report = summarize_eval_observations("digest", &[], &observations);
    let totals = &report.operational_totals[0];
    assert_eq!(totals.total_tokens.total, Some(JsNumber(f64::INFINITY)));
    assert_eq!(totals.tool_calls.total, Some(JsNumber(f64::INFINITY)));

    let formatted = strip_style(&format_eval_comparison_report(&report));
    assert!(
        formatted.contains("2 runs, Infinity tokens, Infinity tools, 2.00s, $0.0200 cost"),
        "{formatted}"
    );

    let json: serde_json::Value = serde_json::to_value(totals).unwrap();
    assert_eq!(json["totalTokens"]["total"], serde_json::Value::Null);
    assert_eq!(json["toolCalls"]["total"], serde_json::Value::Null);
}

// ---------------------------------------------------------------------------
// ICU en-US 排序（ASCII 控制字符）
// ---------------------------------------------------------------------------

fn expected_run(eval_set: &str, case_id: &str) -> ExpectedEvalRun {
    ExpectedEvalRun {
        eval_set: eval_set.to_string(),
        case_id: case_id.to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "m".to_string(),
        run_number: 1,
    }
}

/// Node 26 实测：`"case\ttab".localeCompare("case \u0001 control") === -1`（TAB 排在空格前，
/// U+0001 是 ignorable）。修复前 Rust 把控制字符排在可打印 ASCII 之后，顺序相反。
#[test]
fn blocked_pairs_use_icu_control_character_ordering() {
    let expected = vec![
        expected_run("case", "case \u{1} control"),
        expected_run("case", "case\ttab"),
    ];
    let report = summarize_eval_observations("digest", &expected, &[]);
    let order: Vec<&str> = report
        .blocked_pairs
        .iter()
        .map(|pair| pair.case_id.as_str())
        .collect();
    assert_eq!(order, vec!["case\ttab", "case \u{1} control"]);
}

/// 比较报告按 eval set 排序时同样走 ICU 控制字符权重。
#[test]
fn comparisons_use_icu_control_character_ordering() {
    let expected = vec![
        expected_run("case \u{1} control", "a"),
        expected_run("case\ttab", "b"),
    ];
    let report = summarize_eval_observations("digest", &expected, &[]);
    let order: Vec<&str> = report
        .comparisons
        .iter()
        .map(|comparison| comparison.eval_set.as_str())
        .collect();
    assert_eq!(order, vec!["case\ttab", "case \u{1} control"]);
}

/// ignorable 控制字符不参与比较：`"a\u0000b"` 与 `"ab"` 相等，稳定排序保留输入顺序。
#[test]
fn ignorable_control_characters_compare_equal() {
    let expected = vec![expected_run("case", "a\u{0}b"), expected_run("case", "ab")];
    let report = summarize_eval_observations("digest", &expected, &[]);
    let order: Vec<&str> = report
        .blocked_pairs
        .iter()
        .map(|pair| pair.case_id.as_str())
        .collect();
    assert_eq!(order, vec!["a\u{0}b", "ab"]);
}
