//! 验收检查（`acceptance.rs`）测试。
//!
//! 覆盖 DoD：
//! - `tests_passed:<command>` 没有真实 bash receipt 时判 `UNVERIFIED`；
//! - 只有匹配到运行时打戳、哈希可复算的执行记录才可能通过；
//! - 输出没有测试摘要形状（例如只有 BUILD SUCCESS）判 `UNVERIFIED`；
//! - 失败摘要优先于通过摘要；
//! - 文件 exists/non-empty/json-valid/csv-columns 用代码判定；
//! - 不可判定的 criterion 一律 `UNVERIFIED`，绝不静默通过。

mod common;

use std::fs;

use kiana_session::acceptance::{
    AcceptanceContext, AcceptanceStatus, ExecutionRecord, classify_test_output, evaluate_acceptance,
};
use kiana_session::subagent::result::ReceiptStatus;
use serde_json::json;

fn context<'a>(
    workdir: &std::path::Path,
    executions: &'a [ExecutionRecord],
) -> AcceptanceContext<'a> {
    AcceptanceContext::new(workdir.to_path_buf(), executions)
}

fn bash(command: &str, output: &str, status: ReceiptStatus, seq: u64) -> ExecutionRecord {
    ExecutionRecord::record("bash", json!({ "command": command }), output, status, seq)
}

#[test]
fn tests_passed_accepts_powershell_executions() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![ExecutionRecord::record(
        "powershell",
        json!({ "command": "cargo test -p demo" }),
        "running 3 tests\ntest result: ok. 3 passed; 0 failed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test -p demo".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");
}

#[test]
fn tests_passed_without_any_recorded_execution_is_unverified() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &["tests_passed:cargo test -p demo".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified);
    let leaf = &report.leaves[0];
    assert!(!leaf.checked);
    assert!(!leaf.holds);
    assert!(leaf.detail.contains("no matching recorded shell execution"));
    assert_eq!(report.unchecked.len(), 1);
}

#[test]
fn tests_passed_anchors_to_a_verified_successful_execution_with_a_pass_summary() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "cargo test -p demo",
        "running 3 tests\ntest result: ok. 3 passed; 0 failed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test -p demo".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");
    let leaf = &report.leaves[0];
    assert!(leaf.checked);
    assert!(leaf.holds);
}

#[test]
fn tampered_receipt_is_not_execution_evidence() {
    let temp = tempfile::tempdir().expect("temp");
    let mut execution = bash(
        "cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        1,
    );
    // 模拟持久化后被改写：output 变了，receipt 仍是旧哈希。
    execution.output.push_str("forged");
    assert!(!execution.verify());
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &[execution]),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified);
    assert!(!report.leaves[0].checked);
}

#[test]
fn mentioning_the_command_inside_echo_does_not_anchor_the_criterion() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "echo 'cargo test'",
        "cargo test\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified);
}

#[test]
fn command_segment_match_works_for_chained_commands() {
    let temp = tempfile::tempdir().expect("temp");
    // 前置 `&&` 在记录为成功时可以归因：左侧成功才会执行右侧的测试命令。
    // `cd` 目标必须在 workdir 之内（auto-prefix 的形状），否则输出不可归因。
    let executions = vec![bash(
        &format!("cd {} && cargo test -p demo", temp.path().display()),
        "test result: ok. 2 passed\n",
        ReceiptStatus::Succeeded,
        4,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test -p demo".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 匹配段后面还有命令时，整条命令的 exit status 不属于测试命令，不能归因。
    let executions = vec![bash(
        &format!(
            "cd {} && cargo test -p demo && echo done",
            temp.path().display()
        ),
        "test result: ok. 2 passed\n",
        ReceiptStatus::Succeeded,
        5,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test -p demo".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn shell_comments_and_escaped_operators_are_not_execution_evidence() {
    let temp = tempfile::tempdir().expect("temp");
    // 注释里的 `; cargo test` 不执行任何东西，不能锚定 criterion。
    let executions = vec![bash(
        "echo 'test result: ok. 1 passed' # ; cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 转义的 `\;` 是 echo 的参数，不是分隔符。
    let executions = vec![bash(
        "echo hi \\; cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        2,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 换行是真实分隔符，最后一段确实是测试命令；但前面的 echo 不静默，输出仍不可归因。
    let executions = vec![bash(
        "echo hi\ncargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        3,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn the_latest_unprovable_match_overrides_an_earlier_pass() {
    let temp = tempfile::tempdir().expect("temp");
    // 第一次真的跑过并通过；第二次文本命中但尾随了 `&& echo done`，状态不可归因。
    // 最新一次匹配是决定性的：必须判 UNVERIFIED，而不是沿用旧证据。
    let executions = vec![
        bash(
            "cargo test",
            "test result: ok. 1 passed\n",
            ReceiptStatus::Succeeded,
            1,
        ),
        bash(
            "cargo test && echo done",
            "test result: ok. 1 passed\n",
            ReceiptStatus::Succeeded,
            2,
        ),
    ];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
    assert!(!report.leaves[0].checked);
}

#[test]
fn out_of_scope_cd_and_export_make_the_run_unprovable() {
    let temp = tempfile::tempdir().expect("temp");
    // 子代理可控目录（workdir 之外）里的 `cargo` 可以伪造摘要。
    let executions = vec![bash(
        "cd /tmp/fake && cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // PATH 前缀让实际执行的可执行文件不可见。
    let executions = vec![bash(
        "export PATH=/tmp/fake:$PATH; cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        2,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // `..` 走出 workdir。
    let executions = vec![bash(
        "cd .. && cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        3,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // workdir 内的 auto-prefix `cd` 是静默、可归因的。
    let executions = vec![bash(
        &format!("cd {} && cargo test", temp.path().display()),
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        4,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 相对子目录同样在范围内。
    let executions = vec![bash(
        "cd sub && cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        5,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 末段重定向把真正的摘要带走了。
    let executions = vec![bash(
        "cargo test > out.txt",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        6,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn failing_test_summary_fails_and_failure_wins_over_pass_text() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "cargo test",
        "test result: ok. 1 passed\n1 failed\ntest result: FAILED. 1 passed; 1 failed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Failed);
}

#[test]
fn build_success_banner_without_test_summary_is_unverified() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "cargo build",
        "Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo build".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified);
    assert!(!report.leaves[0].checked);
}

#[test]
fn failed_execution_record_is_a_checked_failure() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Failed,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Failed);
    assert!(report.leaves[0].checked);
}

#[test]
fn file_exists_non_empty_json_and_csv_are_checked_in_code() {
    let temp = tempfile::tempdir().expect("temp");
    fs::write(temp.path().join("data.json"), r#"{"ok": true}"#).expect("json");
    fs::write(temp.path().join("table.csv"), "\u{feff}id,name\n1,alice\n").expect("csv");
    fs::write(temp.path().join("empty.txt"), "").expect("empty");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file:data.json exists".to_string(),
            "file:data.json non-empty".to_string(),
            "file:data.json json-valid".to_string(),
            "file:table.csv csv-columns:[\"id\", \"name\"]".to_string(),
            "file:table.csv csv-columns:[\"name\", \"id\"]".to_string(),
            "file:empty.txt non-empty".to_string(),
            "file:missing.txt exists".to_string(),
            "file_written:data.json".to_string(),
            "file_written:missing.txt".to_string(),
        ],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Failed);
    assert!(report.leaves[0].holds);
    assert!(report.leaves[1].holds);
    assert!(report.leaves[2].holds);
    assert!(report.leaves[3].holds);
    // csv-columns 是「包含」语义（顺序无关）：表头 `id,name` 包含 `["name", "id"]`。
    assert!(report.leaves[4].holds);
    assert!(report.leaves[5].checked);
    assert!(!report.leaves[5].holds);
    assert!(report.leaves[6].checked);
    assert!(!report.leaves[6].holds);
    assert!(report.leaves[7].checked);
    assert!(report.leaves[7].holds);
    assert!(report.leaves[8].checked);
    assert!(!report.leaves[8].holds);
}

#[test]
fn zero_byte_file_written_holds_and_directories_stay_unverified() {
    let temp = tempfile::tempdir().expect("temp");
    fs::write(temp.path().join("empty.txt"), "").expect("empty");
    fs::create_dir(temp.path().join("dir")).expect("dir");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file_written:empty.txt".to_string(),
            "file:dir exists".to_string(),
            "file_written:dir".to_string(),
        ],
        &context(temp.path(), &executions),
    );
    // 零字节文件：存在 + 0 字节回读成功 → holds（与参照实现的 file_written 一致）。
    assert!(report.leaves[0].checked);
    assert!(report.leaves[0].holds, "{:?}", report.leaves[0]);
    // 目录不是普通文件：参照实现降级为 UNVERIFIED，而不是已执行的失败。
    assert!(!report.leaves[1].checked);
    assert!(!report.leaves[2].checked);
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn csv_columns_require_unique_names_and_accept_supersets() {
    let temp = tempfile::tempdir().expect("temp");
    fs::write(
        temp.path().join("users.csv"),
        "id,name,email\n1,a,b@example.com\n",
    )
    .expect("csv");
    fs::write(temp.path().join("dup.csv"), "id,id\n1,2\n").expect("dup");
    let executions = Vec::new();
    let context = context(temp.path(), &executions);
    let report = evaluate_acceptance(
        &[
            "file:users.csv csv-columns:[\"id\",\"name\"]".to_string(),
            "file:users.csv csv-columns:[\"name\",\"id\"]".to_string(),
            "file:users.csv csv-columns:[\"id\",\"missing\"]".to_string(),
            "file:dup.csv csv-columns:[\"id\"]".to_string(),
            // 重复/空的 criterion 列名不是可判定的检查，保持 UNVERIFIED。
            "file:users.csv csv-columns:[\"id\",\"id\"]".to_string(),
            "file:users.csv csv-columns:[]".to_string(),
        ],
        &context,
    );
    assert!(report.leaves[0].holds, "{:?}", report.leaves[0]);
    assert!(report.leaves[1].holds, "{:?}", report.leaves[1]);
    assert!(report.leaves[2].checked);
    assert!(!report.leaves[2].holds);
    assert!(report.leaves[3].checked);
    assert!(
        !report.leaves[3].holds,
        "duplicate header names cannot certify"
    );
    assert!(!report.leaves[4].checked);
    assert!(!report.leaves[5].checked);
}

#[test]
fn node_test_counters_decide_the_verdict() {
    let failing = concat!(
        "ok 1 - a\n",
        "not ok 2 - b\n",
        "# tests 2\n",
        "# suites 1\n",
        "# pass 1\n",
        "# fail 1\n",
        "# cancelled 0\n",
        "# skipped 0\n",
        "# todo 0\n"
    );
    assert_eq!(classify_test_output(failing), AcceptanceStatus::Failed);

    // 只有 TAP 测试点、没有完整 counters：不是完整摘要，保持 UNVERIFIED。
    assert_eq!(
        classify_test_output("ok 1 - a\nnot ok 2 - b\n"),
        AcceptanceStatus::Unverified
    );

    let passing = concat!(
        "ℹ tests 3\n",
        "ℹ suites 1\n",
        "ℹ pass 3\n",
        "ℹ fail 0\n",
        "ℹ cancelled 0\n",
        "ℹ skipped 0\n",
        "ℹ todo 0\n"
    );
    assert_eq!(classify_test_output(passing), AcceptanceStatus::Passed);

    // 计数不自洽的块不算证据。
    let inconsistent = concat!(
        "# tests 3\n",
        "# suites 1\n",
        "# pass 1\n",
        "# fail 0\n",
        "# cancelled 0\n",
        "# skipped 0\n",
        "# todo 0\n"
    );
    assert_eq!(
        classify_test_output(inconsistent),
        AcceptanceStatus::Unverified
    );
}

#[test]
fn go_and_unittest_require_an_actually_executed_test() {
    // 全 skip 的 Go 包同样打印 `ok`；只有 `--- PASS` 才能支撑通过。
    assert_eq!(
        classify_test_output("--- SKIP: TestX\nPASS\nok example.com/pkg 0.002s\n"),
        AcceptanceStatus::Unverified
    );
    assert_eq!(
        classify_test_output(
            "=== RUN   TestX\n--- PASS: TestX (0.00s)\nPASS\nok example.com/pkg 0.002s\n"
        ),
        AcceptanceStatus::Passed
    );
    // 空包不能通过 `^ok` 混成证据。
    assert_eq!(
        classify_test_output("ok example.com/pkg 0.002s [no test files]\n"),
        AcceptanceStatus::Unverified
    );

    // unittest：执行数必须大于 skipped/expected failures。
    assert_eq!(
        classify_test_output("Ran 1 test in 0.000s\n\nOK (skipped=1)\n"),
        AcceptanceStatus::Unverified
    );
    assert_eq!(
        classify_test_output("Ran 2 tests in 0.010s\n\nOK (skipped=1)\n"),
        AcceptanceStatus::Passed
    );
    assert_eq!(
        classify_test_output("Ran 0 tests in 0.000s\n\nOK\n"),
        AcceptanceStatus::Unverified
    );
}

#[test]
fn json_valid_accepts_js_documents_and_rejects_non_json_literals() {
    let temp = tempfile::tempdir().expect("temp");
    // V8 的 JSON.parse 接受 1e400（得到 Infinity）；kiana-json 按 V8 接受范围解析。
    fs::write(temp.path().join("huge.json"), r#"{"score": 1e400}"#).expect("json");
    // 深层嵌套超过 serde_json 的 128 层默认限制。
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    fs::write(temp.path().join("deep.json"), deep).expect("json");
    // 嵌套的 NaN/Infinity 不是 JSON。
    fs::write(temp.path().join("nan.json"), r#"{"a": NaN}"#).expect("json");
    fs::write(temp.path().join("infinity.json"), "Infinity").expect("json");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file:huge.json json-valid".to_string(),
            "file:deep.json json-valid".to_string(),
            "file:nan.json json-valid".to_string(),
            "file:infinity.json json-valid".to_string(),
        ],
        &context(temp.path(), &executions),
    );
    assert!(report.leaves[0].holds, "{:?}", report.leaves[0]);
    assert!(report.leaves[1].holds, "{:?}", report.leaves[1]);
    assert!(report.leaves[2].checked);
    assert!(!report.leaves[2].holds, "{:?}", report.leaves[2]);
    assert!(report.leaves[3].checked);
    assert!(!report.leaves[3].holds, "{:?}", report.leaves[3]);
}

#[cfg(unix)]
#[test]
fn special_files_are_unverified_without_blocking() {
    let temp = tempfile::tempdir().expect("temp");
    let fifo = temp.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success());
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file:pipe exists".to_string(),
            "file:pipe json-valid".to_string(),
        ],
        &context(temp.path(), &executions),
    );
    // O_NONBLOCK 打开 FIFO，fstat 发现不是普通文件 → UNVERIFIED（且不会挂起）。
    assert!(!report.leaves[0].checked);
    assert!(!report.leaves[1].checked);
}

#[test]
fn trailing_shell_comments_do_not_break_command_matching() {
    let temp = tempfile::tempdir().expect("temp");
    let executions = vec![bash(
        "pytest # unit tests",
        "5 passed in 0.12s\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");
}

#[test]
fn invalid_json_and_unknown_criteria_are_unverified() {
    let temp = tempfile::tempdir().expect("temp");
    fs::write(temp.path().join("bad.json"), "{not json").expect("json");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file:bad.json json-valid".to_string(),
            "make it look nice".to_string(),
        ],
        &context(temp.path(), &executions),
    );
    // 一条明确失败 + 一条不可判定：失败优先（不能因为未知而掩盖已确证的失败）。
    assert_eq!(report.status, AcceptanceStatus::Failed);
    assert!(report.leaves[0].checked);
    assert!(!report.leaves[0].holds);
    assert!(!report.leaves[1].checked);
    assert_eq!(report.unchecked.len(), 1);
}

#[cfg(unix)]
#[test]
fn path_outside_workdir_and_symlink_are_unverified() {
    let temp = tempfile::tempdir().expect("temp");
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "secret").expect("outside");
    let workdir = temp.path().join("work");
    fs::create_dir_all(&workdir).expect("workdir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, workdir.join("link.txt")).expect("symlink");
    let executions = Vec::new();
    let report = evaluate_acceptance(
        &[
            "file:../outside.txt exists".to_string(),
            "file:link.txt exists".to_string(),
        ],
        &context(&workdir, &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified);
    assert!(!report.leaves[0].checked);
    #[cfg(unix)]
    assert!(!report.leaves[1].checked);
}

#[test]
fn shell_control_flow_must_prove_the_test_command_ran() {
    let temp = tempfile::tempdir().expect("temp");
    // `echo` 成功使 `|| cargo test` 永远不执行；回显出来的通过形状不能锚定 criterion。
    let executions = vec![bash(
        "echo 'test result: ok. 1 passed' || cargo test",
        "test result: ok. 1 passed\n",
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
    assert!(!report.leaves[0].checked);

    // 成功状态下的前置 `&&` 可以归因：左侧成功才会执行右侧的测试命令。
    let executions = vec![bash(
        "cd missing && cargo test",
        "running 3 tests\ntest result: ok. 3 passed; 0 failed\n",
        ReceiptStatus::Succeeded,
        2,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 记录为失败时，`&&` 右侧是否执行无法证明。
    let executions = vec![bash(
        "cd missing && cargo test",
        "running 0 tests\ntest result: FAILED. 0 passed; 0 failed\n",
        ReceiptStatus::Failed,
        3,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:cargo test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn empty_cargo_doc_target_does_not_veto_a_real_pass() {
    let output = concat!(
        "running 3 tests\n",
        "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
        "   Doc-tests demo\n\n",
        "running 0 tests\n\n",
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
    );
    assert_eq!(classify_test_output(output), AcceptanceStatus::Passed);

    // 只有一个空 target 时仍然不能被当成通过。
    assert_eq!(
        classify_test_output(
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
        ),
        AcceptanceStatus::Unverified
    );
}

#[test]
fn unittest_summary_requires_the_ok_line() {
    assert_eq!(
        classify_test_output("Ran 12 tests in 0.34s\n"),
        AcceptanceStatus::Unverified
    );
    assert_eq!(
        classify_test_output("Ran 12 tests in 0.34s\n\nOK\n"),
        AcceptanceStatus::Passed
    );
}

#[test]
fn empty_criteria_is_unverified_not_passed() {
    let temp = tempfile::tempdir().expect("temp");
    let report = evaluate_acceptance(&[], &context(temp.path(), &[]));
    assert_eq!(report.status, AcceptanceStatus::Unverified);
    assert!(report.leaves.is_empty());
}

// 让 `summarize_test_output` 的判决形状可被外部测试（本测试文件）直接覆盖。
#[test]
fn summarize_shapes() {
    assert_eq!(classify_test_output("3 passed"), AcceptanceStatus::Passed);
    assert_eq!(
        classify_test_output("test result: FAILED. 0 passed; 1 failed"),
        AcceptanceStatus::Failed
    );
    assert_eq!(
        classify_test_output("BUILD SUCCESS"),
        AcceptanceStatus::Unverified
    );
}

#[test]
fn maven_and_gradle_summaries_are_test_evidence() {
    let temp = tempfile::tempdir().expect("temp");
    let pass = "Tests run: 5, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.1 s\n";
    let executions = vec![bash("mvn test", pass, ReceiptStatus::Succeeded, 1)];
    let report = evaluate_acceptance(
        &["tests_passed:mvn test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 计数行里的失败优先：没有 BUILD FAILURE banner 也必须判失败。
    let failing = "Tests run: 5, Failures: 2, Errors: 0, Skipped: 0\n";
    let executions = vec![bash("mvn test", failing, ReceiptStatus::Succeeded, 2)];
    let report = evaluate_acceptance(
        &["tests_passed:mvn test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Failed, "{report:?}");

    let executions = vec![bash(
        "gradle test",
        "5 tests completed, 0 failed\n",
        ReceiptStatus::Succeeded,
        3,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:gradle test".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");
}

#[test]
fn csv_header_check_reads_only_a_bounded_prefix_and_ignores_data_rows() {
    let temp = tempfile::tempdir().expect("temp");
    // 数据行里的非法 UTF-8 与 header 判定无关。
    fs::write(temp.path().join("latin1.csv"), b"id,name\n1,\xE9\n").expect("csv");
    // header 本身不是 UTF-8：已执行检查且不成立。
    fs::write(temp.path().join("bad-header.csv"), b"id,\xE9\n").expect("csv");
    // 超过读取上限的大文件：只需要第一条记录。
    let mut large = b"id,name\n".to_vec();
    large.extend(std::iter::repeat_n(b'x', 60_000));
    fs::write(temp.path().join("large.csv"), &large).expect("csv");
    // 表头字段的空格是数据的一部分，不能被 trim 掉。
    fs::write(temp.path().join("padded.csv"), " id , name \n1,2\n").expect("csv");

    let report = evaluate_acceptance(
        &[
            "file:latin1.csv csv-columns:[\"id\",\"name\"]".to_string(),
            "file:bad-header.csv csv-columns:[\"id\"]".to_string(),
            "file:large.csv csv-columns:[\"id\",\"name\"]".to_string(),
            "file:padded.csv csv-columns:[\"id\",\"name\"]".to_string(),
        ],
        &context(temp.path(), &[]),
    );
    assert!(report.leaves[0].holds, "{:?}", report.leaves[0]);
    assert!(report.leaves[1].checked);
    assert!(!report.leaves[1].holds, "{:?}", report.leaves[1]);
    assert!(report.leaves[2].holds, "{:?}", report.leaves[2]);
    assert!(report.leaves[3].checked);
    assert!(
        !report.leaves[3].holds,
        "padded header names must not match exact-name criteria: {:?}",
        report.leaves[3]
    );
}

#[test]
fn token_level_command_matching_follows_the_reference_rules() {
    let temp = tempfile::tempdir().expect("temp");
    let pass = "test result: ok. 5 passed\n";
    let context = |executions| context(temp.path(), executions);

    // 裸 criterion 接受同一可执行文件的任意路径拼写，安全 flag 不改变选择。
    let executions = vec![bash(
        ".venv/bin/pytest tests -q",
        pass,
        ReceiptStatus::Succeeded,
        1,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 与命中目标无关的排除不影响匹配。
    let executions = vec![bash(
        "pytest tests/unit --ignore tests/slow",
        pass,
        ReceiptStatus::Succeeded,
        2,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests/unit".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Passed, "{report:?}");

    // 缩小选择的 flag（`-k`）让命中不可证明。
    let executions = vec![bash(
        "pytest -k smoke tests",
        pass,
        ReceiptStatus::Succeeded,
        3,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 排除目标与 criterion 目标重叠：部分测试没有运行。
    let executions = vec![bash(
        "pytest tests --deselect tests/unit/test_auth.py",
        pass,
        ReceiptStatus::Succeeded,
        4,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 显式路径的 criterion 只认同路径的执行，裸名会走 PATH。
    let executions = vec![bash("pytest tests", pass, ReceiptStatus::Succeeded, 5)];
    let report = evaluate_acceptance(
        &["tests_passed:/opt/project/.venv/bin/pytest tests".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 环境变量前缀不同：运行环境不可比。
    let executions = vec![bash("CI=1 pytest tests", pass, ReceiptStatus::Succeeded, 6)];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests".to_string()],
        &context(&executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}

#[test]
fn command_matching_parses_the_criterion_instead_of_trusting_text_equality() {
    let temp = tempfile::tempdir().expect("temp");
    let pass = "test result: ok. 5 passed\n";

    // `;` 与注释是 shell 语法，不是参数；criterion 必须先解析再取 token。
    for (seq, (criterion, executed)) in [
        ("pytest tests", "pytest tests"),
        ("pytest tests;", "pytest tests"),
        ("pytest tests # note", "pytest tests"),
    ]
    .into_iter()
    .enumerate()
    {
        let executions = vec![bash(
            executed,
            pass,
            ReceiptStatus::Succeeded,
            seq as u64 + 1,
        )];
        let report = evaluate_acceptance(
            &[format!("tests_passed:{criterion}")],
            &context(temp.path(), &executions),
        );
        assert_eq!(
            report.status,
            AcceptanceStatus::Passed,
            "criterion {criterion:?} should match {executed:?}: {report:?}"
        );
    }

    // 文本完全相等也不能绕过运行时展开检查：`$EXTRA` 实际注入了什么不可见。
    let executions = vec![bash(
        "pytest tests $EXTRA",
        pass,
        ReceiptStatus::Succeeded,
        10,
    )];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests $EXTRA".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");

    // 尾部 `&` 让退出状态不再属于匹配段。
    let executions = vec![bash("pytest tests &", pass, ReceiptStatus::Succeeded, 11)];
    let report = evaluate_acceptance(
        &["tests_passed:pytest tests".to_string()],
        &context(temp.path(), &executions),
    );
    assert_eq!(report.status, AcceptanceStatus::Unverified, "{report:?}");
}
