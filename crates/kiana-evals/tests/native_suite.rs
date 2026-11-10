//! 原生 task suite：失败反馈、断点续跑、dry-run/replay 与沙箱前置校验。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kiana_evals::native::{
    CommandOutput, CommandRunner, NativeError, NativeRunOptions, NativeRunner, NativeSuiteResults,
    SuiteSpec, TrialStatus,
};

struct FakeRunner {
    calls: AtomicUsize,
}

impl FakeRunner {
    fn new() -> Self {
        FakeRunner {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CommandRunner for FakeRunner {
    async fn run(
        &self,
        command: &[String],
        cwd: &Path,
        _timeout: Duration,
    ) -> Result<CommandOutput, NativeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = std::fs::read_to_string(cwd.join("answer.txt")).unwrap_or_default();
        let success = answer.trim() == "42";
        Ok(CommandOutput {
            command: command.to_vec(),
            exit_code: Some(if success { 0 } else { 1 }),
            timed_out: false,
            stdout: if success {
                String::new()
            } else {
                "expected exactly 42\n".to_string()
            },
            stderr: String::new(),
            duration_ms: 0,
        })
    }
}

fn suite_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("suites")
}

fn options(artifacts: &Path, task: Option<&str>) -> NativeRunOptions {
    NativeRunOptions {
        suite: "smoke".to_string(),
        suite_root: suite_root(),
        trials: 1,
        artifacts_dir: artifacts.to_path_buf(),
        sandbox: true,
        task_filter: task.map(str::to_string),
        ..NativeRunOptions::default()
    }
}

fn read_suite_results(artifacts: &Path) -> NativeSuiteResults {
    let path = artifacts.join("smoke").join("results.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("results.json"))
        .expect("valid results json")
}

#[tokio::test]
async fn failing_test_appends_feedback_and_second_attempt_passes() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeRunner::new());
    let mut runner = NativeRunner::with_command_runner(
        options(directory.path(), Some("write-42")),
        fake.clone(),
    );
    let results = runner.run().await.expect("native run");

    assert_eq!(results.records.len(), 1);
    let record = &results.records[0];
    assert_eq!(record.task, "write-42");
    assert_eq!(record.status, TrialStatus::Passed);
    assert!(record.pass);
    assert_eq!(
        record.attempts.len(),
        2,
        "failure feedback causes a second attempt"
    );
    assert!(
        record.attempts[0]
            .prompt_preview
            .contains("Write the number 42")
    );
    assert_eq!(
        record.attempts[0]
            .test
            .as_ref()
            .and_then(|test| test.exit_code),
        Some(1)
    );
    assert_eq!(
        record.attempts[1]
            .test
            .as_ref()
            .and_then(|test| test.exit_code),
        Some(0)
    );
    // 第二次尝试的 prompt 里带上了第一轮的失败反馈。
    assert!(record.attempts[1].prompt_preview.contains("Test failed"));
    // 每轮 2 次模型请求（write + final）。
    assert_eq!(results.model_calls, 4);
    assert_eq!(fake.calls(), 2);

    // 结果文件写盘：suite 级 + 每个任务一份。
    assert!(directory.path().join("smoke/results.json").is_file());
    assert!(
        directory
            .path()
            .join("smoke/tasks/write-42/results.json")
            .is_file()
    );
    let persisted = read_suite_results(directory.path());
    assert_eq!(persisted.summary.pass_at_1, 1.0);
    assert_eq!(persisted.summary.pass_at_k, 1.0);
    assert_eq!(persisted.summary.well_formed_percent, 100.0);
    assert!(persisted.summary.total_cost > 0.0);
}

#[tokio::test]
async fn three_trials_produce_pass_at_k_and_well_formed_percent() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeRunner::new());
    let mut options = options(directory.path(), Some("write-42"));
    options.trials = 3;
    let mut runner = NativeRunner::with_command_runner(options, fake.clone());
    let results = runner.run().await.expect("native run");
    assert_eq!(results.records.len(), 3);
    assert_eq!(results.summary.tasks, 1);
    assert_eq!(results.summary.k, 3);
    assert_eq!(results.summary.pass_at_1, 1.0);
    assert_eq!(results.summary.pass_at_k, 1.0);
    assert_eq!(results.summary.well_formed_percent, 100.0);
    assert!(results.records.iter().all(|record| record.pass));
    // 3 个 trial × 2 次请求 × 2 次尝试。
    assert_eq!(results.model_calls, 12);
    assert_eq!(fake.calls(), 6);
}

#[tokio::test]
async fn resume_and_dry_run_make_zero_model_calls() {
    let directory = tempfile::tempdir().unwrap();
    let first_fake = Arc::new(FakeRunner::new());
    let mut first = NativeRunner::with_command_runner(
        options(directory.path(), Some("write-42")),
        first_fake.clone(),
    );
    let first_results = first.run().await.expect("first run");
    assert!(first_results.model_calls > 0);
    assert_eq!(first_fake.calls(), 2);

    // 断点续跑：已完成记录全部跳过。
    let second_fake = Arc::new(FakeRunner::new());
    let mut second = NativeRunner::with_command_runner(
        options(directory.path(), Some("write-42")),
        second_fake.clone(),
    );
    let second_results = second.run().await.expect("second run");
    assert_eq!(second_fake.calls(), 0);
    assert_eq!(second_results.records.len(), 1);
    assert_eq!(second_results.model_calls, 0);

    // dry-run：fingerprint 不变，零模型调用。
    let dry_fake = Arc::new(FakeRunner::new());
    let mut dry_options = options(directory.path(), Some("write-42"));
    dry_options.dry_run = true;
    let mut dry = NativeRunner::with_command_runner(dry_options, dry_fake.clone());
    let dry_results = dry.run().await.expect("dry run");
    assert_eq!(dry_fake.calls(), 0);
    assert_eq!(dry_results.records.len(), 1);
    assert_eq!(dry_results.fingerprint, first_results.fingerprint);

    // replay 只读结果文件。
    let replay_fake = Arc::new(FakeRunner::new());
    let mut replay_options = options(directory.path(), None);
    replay_options.replay = Some(directory.path().join("smoke/results.json"));
    let mut replay = NativeRunner::with_command_runner(replay_options, replay_fake.clone());
    let replay_results = replay.run().await.expect("replay");
    assert_eq!(replay_fake.calls(), 0);
    assert_eq!(replay_results.records, first_results.records);
}

#[tokio::test]
async fn running_record_is_not_treated_as_complete() {
    let directory = tempfile::tempdir().unwrap();
    let suite = SuiteSpec::discover(&suite_root(), "smoke").expect("suite");
    let fingerprint = suite
        .fingerprint(&NativeRunOptions::default().model_cost)
        .expect("fingerprint");
    // 手工写一条 running 记录 + 匹配的 fingerprint。
    let results_path = directory.path().join("smoke/results.json");
    std::fs::create_dir_all(results_path.parent().unwrap()).unwrap();
    let mut fake_results = NativeSuiteResults {
        suite: "smoke".to_string(),
        fingerprint: fingerprint.clone(),
        model: "mock:smoke".to_string(),
        trials: 1,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        finished_at: None,
        records: vec![kiana_evals::native::TrialResult {
            task: "write-42".to_string(),
            trial: 1,
            status: TrialStatus::Running,
            pass: true,
            attempts: Vec::new(),
            duration_ms: 0,
            tokens: 0,
            cost: 0.0,
            cost_unknown: false,
            malformed: 0,
            context_exhausted: false,
            error: None,
            workspace: "stale".to_string(),
        }],
        summary: Default::default(),
        model_calls: 0,
    };
    fake_results.recompute_summary();
    std::fs::write(
        &results_path,
        serde_json::to_string_pretty(&fake_results).unwrap(),
    )
    .unwrap();

    let fake = Arc::new(FakeRunner::new());
    let mut runner = NativeRunner::with_command_runner(
        options(directory.path(), Some("write-42")),
        fake.clone(),
    );
    let results = runner.run().await.expect("resume");
    assert_eq!(fake.calls(), 2);
    let record = &results.records[0];
    assert_eq!(record.status, TrialStatus::Passed);
    assert_eq!(record.attempts.len(), 2);
    assert_ne!(record.workspace, "stale");
}

#[tokio::test]
async fn sandbox_is_required_before_any_model_call() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeRunner::new());
    let mut options = options(directory.path(), Some("write-42"));
    options.sandbox = false;
    let mut runner = NativeRunner::with_command_runner(options, fake.clone());
    let error = runner.run().await.expect_err("sandbox required");
    assert!(matches!(error, NativeError::SandboxRequired));
    assert_eq!(fake.calls(), 0);
    assert!(!directory.path().join("smoke/results.json").exists());
}

#[tokio::test]
async fn ambiguous_or_missing_task_layouts_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let suite_root = directory.path().join("suites");
    let task = suite_root.join("broken").join("no-meta");
    std::fs::create_dir_all(task.join("solution")).unwrap();
    std::fs::write(task.join("solution/answer.txt"), "42").unwrap();
    let error = SuiteSpec::discover(&suite_root, "broken").expect_err("no meta.json");
    assert!(error.to_string().contains("contains no tasks"));

    let task = suite_root.join("broken").join("unsafe");
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(
        task.join("meta.json"),
        serde_json::json!({
            "id": "unsafe",
            "language": "shell",
            "testCommand": ["sh", "-c", "true"],
            "files": [{"path": "../escape.txt", "role": "solution"}]
        })
        .to_string(),
    )
    .unwrap();
    let error = SuiteSpec::discover(&suite_root, "broken").expect_err("unsafe path");
    assert!(error.to_string().contains("unsafe file path"));
}
