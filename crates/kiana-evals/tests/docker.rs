//! `packages/evals` 没有 `docker.test.ts` / `cli.test.ts`（spec §7.2），这里补 Rust 集成测试：
//! 假 `docker` 可执行文件记录 argv、模拟 `image inspect`、按需写发现/报告产物。
//!
//! 所有测试共用 [`ENV_LOCK`] 串行化进程环境（PATH 与 `FAKE_DOCKER_*`）的修改；不要求真实
//! docker daemon。
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use kiana_evals::cli::run_with;
use kiana_evals::docker::{
    BuiltImage, BuiltImages, DockerContext, build_images, create_docker_context, discover_cases,
    package_root, run_task, typescript_repository_root,
};
use kiana_evals::plan::{DocumentationVariant, EvalTask};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// 进程环境修改的互斥量（测试二进制内的线程共享环境）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_environment() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// 安装假 `docker` 并把它的目录放到 PATH 最前。
struct FakeDocker {
    _directory: tempfile::TempDir,
    log: PathBuf,
    previous_path: Option<OsString>,
}

impl FakeDocker {
    fn install() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("typescript-repository");
        for relative in [
            "package.json",
            "package-lock.json",
            "packages/evals/package.json",
            "packages/evals/docker/entrypoint.ts",
            "packages/evals/docker/install-runtime.mjs",
            "packages/coding-agent/package.json",
        ] {
            let path = repository.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}\n").unwrap();
        }
        unsafe { std::env::set_var("KIANA_EVAL_TYPESCRIPT_REPOSITORY", &repository) };
        let script = directory.path().join("docker");
        std::fs::write(&script, include_str!("fixtures/fake-docker/docker")).unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let log = directory.path().join("argv.log");
        let previous_path = std::env::var_os("PATH");
        let mut paths = vec![directory.path().to_path_buf()];
        if let Some(previous) = &previous_path {
            paths.extend(std::env::split_paths(previous));
        }
        let joined = std::env::join_paths(paths).expect("PATH components are valid");
        // SAFETY: 环境写入由 ENV_LOCK 串行化；Drop 里恢复 PATH 并清理 FAKE_DOCKER_*。
        unsafe {
            std::env::set_var("PATH", joined);
            std::env::set_var("FAKE_DOCKER_LOG", &log);
        }
        FakeDocker {
            _directory: directory,
            log,
            previous_path,
        }
    }

    fn set(&self, name: &str, value: &str) {
        // SAFETY: 调用方持有 ENV_LOCK；变量在 Drop 中清理。
        unsafe { std::env::set_var(name, value) };
    }

    /// 解析 argv 记录：每次调用先是参数个数，随后每行一个参数。
    fn invocations(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let mut lines = text.lines();
        let mut invocations = Vec::new();
        while let Some(count) = lines.next() {
            let count: usize = count.parse().expect("recorded argument count");
            let mut args = Vec::with_capacity(count);
            for _ in 0..count {
                args.push(lines.next().expect("recorded argument").to_string());
            }
            invocations.push(args);
        }
        invocations
    }
}

impl Drop for FakeDocker {
    fn drop(&mut self) {
        // SAFETY: 调用方仍持有 ENV_LOCK（FakeDocker 在 guard 之后声明，先于 guard 析构）。
        unsafe {
            match &self.previous_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
            for name in [
                "FAKE_DOCKER_LOG",
                "FAKE_DOCKER_DISCOVERY_JSON",
                "FAKE_DOCKER_CREATE_VITEST",
                "FAKE_DOCKER_VITEST_JSON",
                "FAKE_DOCKER_BUILD_STATUS",
                "FAKE_DOCKER_RUN_STATUS",
                "FAKE_DOCKER_INSPECT_MODE",
                "FAKE_DOCKER_IMAGE_ID",
                "KIANA_EVAL_TYPESCRIPT_REPOSITORY",
            ] {
                std::env::remove_var(name);
            }
        }
    }
}

/// 临时把 PATH 换成给定目录（Drop 恢复）。调用方必须持有 [`ENV_LOCK`]。
struct PathOverride {
    previous: Option<OsString>,
}

impl PathOverride {
    fn set(directories: &[&Path]) -> Self {
        let previous = std::env::var_os("PATH");
        let joined = std::env::join_paths(directories.iter().map(|path| path.to_path_buf()))
            .expect("PATH components are valid");
        // SAFETY: 环境写入由 ENV_LOCK 串行化；Drop 里恢复。
        unsafe { std::env::set_var("PATH", joined) };
        PathOverride { previous }
    }
}

impl Drop for PathOverride {
    fn drop(&mut self) {
        // SAFETY: 调用方仍持有 ENV_LOCK（PathOverride 在 guard 之后声明）。
        unsafe {
            match &self.previous {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn test_images() -> BuiltImages {
    BuiltImages {
        without_docs: BuiltImage {
            name: "kiana-evals-test-without-docs:local".to_string(),
            id: "sha256:without".to_string(),
        },
        with_docs: BuiltImage {
            name: "kiana-evals-test-with-docs:local".to_string(),
            id: "sha256:with".to_string(),
        },
    }
}

fn test_context(artifact_directory: &Path) -> DockerContext {
    create_docker_context(
        test_images(),
        artifact_directory.to_path_buf(),
        PathBuf::from("/tmp/kiana-eval-test-auth.json"),
        "fixture",
        "model",
        1.0,
    )
}

fn sample_task() -> EvalTask {
    EvalTask {
        file: "evals/example.docs.eval.ts".to_string(),
        full_name: "Demo > case one".to_string(),
        eval_set: "Demo".to_string(),
        case_id: "case one".to_string(),
        variant: DocumentationVariant::WithoutDocs,
        model: "fixture/model".to_string(),
        run_number: 1,
    }
}

/// spec §3.5：`main.rs` 把错误文本打到 stderr 并以 1 退出；argv 从第一个用户参数开始
/// （Rust 二进制没有 Node 的脚本路径那一层）。
#[test]
fn binary_reports_errors_on_stderr_with_exit_code_one() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kiana-evals"))
        .arg("--bogus")
        .output()
        .expect("spawn kiana-evals");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Unsupported eval argument: --bogus\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn build_images_rejects_a_rust_workspace_context_before_launching_docker() {
    let _guard = lock_environment();
    let root = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("KIANA_EVAL_TYPESCRIPT_REPOSITORY", root.path()) };
    let error = build_images().unwrap_err();
    assert!(error.to_string().contains("package.json"));
    assert!(
        error
            .to_string()
            .contains("Rust workspace is not a Docker build context")
    );
    unsafe { std::env::remove_var("KIANA_EVAL_TYPESCRIPT_REPOSITORY") };
}

/// spec §2.5.3：`buildImages` 的 argv 顺序、镜像名与 inspect 结果。
#[test]
fn build_images_uses_the_exact_argv_and_reads_image_ids() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();

    let built = build_images().unwrap();

    let repository_text = typescript_repository_root().to_string_lossy().into_owned();
    let digest = hex::encode(Sha256::digest(repository_text.as_bytes()));
    let prefix = format!("kiana-evals-{}", &digest[..12]);
    assert_eq!(
        built.without_docs.name,
        format!("{prefix}-without-docs:local")
    );
    assert_eq!(built.with_docs.name, format!("{prefix}-with-docs:local"));
    assert_eq!(
        built.without_docs.id,
        format!("fake-id-{}", built.without_docs.name)
    );
    assert_eq!(
        built.with_docs.id,
        format!("fake-id-{}", built.with_docs.name)
    );

    let dockerfile = package_root()
        .join("docker")
        .join("Dockerfile")
        .to_string_lossy()
        .into_owned();
    let invocations = fake.invocations();
    assert_eq!(invocations.len(), 4);
    assert_eq!(
        invocations[0],
        strings(&[
            "build",
            "--target",
            "without_docs",
            "--tag",
            &built.without_docs.name,
            "--file",
            &dockerfile,
            &repository_text,
        ])
    );
    assert_eq!(
        invocations[1],
        strings(&[
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            &built.without_docs.name,
        ])
    );
    assert_eq!(
        invocations[2],
        strings(&[
            "build",
            "--target",
            "with_docs",
            "--tag",
            &built.with_docs.name,
            "--file",
            &dockerfile,
            &repository_text,
        ])
    );
    assert_eq!(
        invocations[3],
        strings(&[
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            &built.with_docs.name,
        ])
    );
}

/// spec §2.5.3：`docker image inspect` 失败或输出为空 → `Cannot inspect <name>.`
#[test]
fn build_images_reports_an_unreadable_image_id() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();

    fake.set("FAKE_DOCKER_INSPECT_MODE", "empty");
    let error = build_images().unwrap_err();
    let prefix = {
        let repository_text = typescript_repository_root().to_string_lossy().into_owned();
        let digest = hex::encode(Sha256::digest(repository_text.as_bytes()));
        format!("kiana-evals-{}", &digest[..12])
    };
    assert_eq!(
        error.to_string(),
        format!("Cannot inspect {prefix}-without-docs:local.")
    );

    fake.set("FAKE_DOCKER_INSPECT_MODE", "fail");
    let error = build_images().unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Cannot inspect {prefix}-without-docs:local.")
    );
}

/// spec §2.5.3：`discoverCases` 的 entrypoint 参数、返回路径与失败文本。
#[test]
fn discover_cases_runs_the_discovery_entrypoint_and_reports_failures() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    let artifact = tempfile::tempdir().unwrap();
    let context = test_context(artifact.path());

    let files = vec!["evals/example.docs.eval.ts".to_string()];
    let vitest_args = vec!["-t".to_string(), "case one".to_string()];
    let path = discover_cases(
        &context,
        DocumentationVariant::WithoutDocs,
        &files,
        &vitest_args,
    )
    .unwrap();
    let output_directory = artifact.path().join("discovery").join("without_docs");
    assert_eq!(path, output_directory.join("discovered-tests.json"));

    let invocations = fake.invocations();
    let args = invocations.last().unwrap();
    assert_eq!(args[0], "run");
    assert_eq!(
        &args[args.len() - 6..],
        &strings(&[
            "--discover",
            "--project",
            "docs",
            "evals/example.docs.eval.ts",
            "-t",
            "case one",
        ])[..]
    );
    assert!(args.iter().any(|argument| argument
        == &format!(
            "type=bind,source={},target=/artifacts",
            output_directory.to_string_lossy()
        )));

    fake.set("FAKE_DOCKER_RUN_STATUS", "3");
    let error = discover_cases(&context, DocumentationVariant::WithDocs, &files, &[]).unwrap_err();
    assert_eq!(error.to_string(), "with_docs eval discovery failed.");
}

/// spec §2.5.3：`runTask` 忽略退出码，只按 `vitest.json` 是否存在返回。
#[test]
fn run_task_returns_the_report_only_when_it_exists() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    let artifact = tempfile::tempdir().unwrap();
    let context = test_context(artifact.path());
    let task = sample_task();

    assert_eq!(run_task(&context, &task).unwrap(), None);

    fake.set("FAKE_DOCKER_CREATE_VITEST", "1");
    fake.set("FAKE_DOCKER_VITEST_JSON", r#"{"ok":true}"#);
    let path = run_task(&context, &task)
        .unwrap()
        .expect("vitest.json exists");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"ok":true}"#);
    assert_eq!(
        path.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .len(),
        64
    );

    let invocations = fake.invocations();
    let args = invocations.last().unwrap();
    assert_eq!(
        &args[args.len() - 5..],
        &strings(&[
            "--project",
            "docs",
            "evals/example.docs.eval.ts",
            "--testNamePattern",
            "^Demo case one$",
        ])[..]
    );
}

fn assert_key_order(text: &str, keys: &[&str]) {
    let mut position = 0;
    for key in keys {
        let needle = format!("\"{key}\"");
        let index = text[position..]
            .find(&needle)
            .unwrap_or_else(|| panic!("missing key {key}"))
            + position;
        position = index + needle.len();
    }
}

/// 端到端：假 docker 只写 discovery 产物（任务没有 `vitest.json` → errored），验证
/// protocol.json / expected-runs.json / observations.jsonl / report.json / report.txt 的
/// 字节、blocked-pair 退出码与产物目录权限。
#[tokio::test]
// 该测试在整个 CLI 运行期间持有 ENV_LOCK：假 docker 依赖进程级 PATH 与 FAKE_DOCKER_*，
// 一旦在 await 点释放就会被并行测试破坏。
#[allow(clippy::await_holding_lock)]
async fn cli_run_writes_every_artifact_and_exits_one_for_blocked_pairs() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    fake.set(
        "FAKE_DOCKER_DISCOVERY_JSON",
        r#"[{"name":"Demo > case one","file":"/repo/packages/evals/evals/example.docs.eval.ts"},{"name":"Demo > case two","file":"/repo/packages/evals/evals/nested/other.docs.eval.ts"}]"#,
    );

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals").join("nested")).unwrap();
    // `dot: false`：隐藏目录里的 `*.docs.eval.ts` 不得被默认 glob 选中。
    std::fs::create_dir_all(root.path().join("evals").join(".hidden")).unwrap();
    std::fs::write(
        root.path()
            .join("evals")
            .join(".hidden")
            .join("skipped.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    std::fs::write(
        root.path()
            .join("evals")
            .join("nested")
            .join("other.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(
        agent.path().join("auth.json"),
        r#"{"fixture": {"type": "api_key"}}"#,
    )
    .unwrap();

    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };
    let args = strings(&["--provider", "fixture", "--model", "model"]);
    let exit_code = run_with(args, root.path(), &environment).await.unwrap();
    assert_eq!(exit_code, 1);

    let eval_root = root.path().join(".eval");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&eval_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let artifact_directory = entries.remove(0);
    let mode = std::fs::metadata(&artifact_directory)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);

    // protocol.json：键序、镜像顺序、digest。
    let protocol_text = std::fs::read_to_string(artifact_directory.join("protocol.json")).unwrap();
    assert!(protocol_text.ends_with("\n"));
    assert_key_order(
        &protocol_text,
        &[
            "schemaVersion",
            "model",
            "runsPerVariant",
            "images",
            "without_docs",
            "with_docs",
            "files",
            "cases",
            "tasks",
            "protocolDigest",
        ],
    );
    let mut protocol: Value = serde_json::from_str(&protocol_text).unwrap();
    assert_eq!(protocol["schemaVersion"], 1);
    assert_eq!(protocol["model"], "fixture/model");
    assert_eq!(protocol["runsPerVariant"], 1);
    assert_eq!(
        protocol["files"],
        serde_json::json!([
            "evals/example.docs.eval.ts",
            "evals/nested/other.docs.eval.ts"
        ])
    );
    assert_eq!(
        protocol["cases"],
        serde_json::json!([
            {
                "evalSet": "Demo",
                "caseId": "case one",
                "file": "evals/example.docs.eval.ts"
            },
            {
                "evalSet": "Demo",
                "caseId": "case two",
                "file": "evals/nested/other.docs.eval.ts"
            }
        ])
    );
    assert!(
        protocol["images"]["without_docs"]["name"]
            .as_str()
            .unwrap()
            .ends_with("-without-docs:local")
    );
    assert_ne!(
        protocol["images"]["without_docs"]["id"],
        protocol["images"]["with_docs"]["id"]
    );
    let recorded_digest = protocol
        .as_object_mut()
        .unwrap()
        .remove("protocolDigest")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let compact = serde_json::to_string(&protocol).unwrap();
    assert_eq!(
        recorded_digest,
        hex::encode(Sha256::digest(compact.as_bytes()))
    );

    // expected-runs.json = pretty(tasks) + "\n"。
    let tasks_pretty = serde_json::to_string_pretty(&protocol["tasks"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(artifact_directory.join("expected-runs.json")).unwrap(),
        format!("{tasks_pretty}\n")
    );

    // observations.jsonl：每次全量重写，最后是两条用例 × 两个变体的 errored 观测。
    let observations =
        std::fs::read_to_string(artifact_directory.join("observations.jsonl")).unwrap();
    let expected_observations = [
        r#"{"evalSet":"Demo","caseId":"case one","variant":"without_docs","model":"fixture/model","runNumber":1,"outcome":"errored"}"#,
        r#"{"evalSet":"Demo","caseId":"case one","variant":"with_docs","model":"fixture/model","runNumber":1,"outcome":"errored"}"#,
        r#"{"evalSet":"Demo","caseId":"case two","variant":"without_docs","model":"fixture/model","runNumber":1,"outcome":"errored"}"#,
        r#"{"evalSet":"Demo","caseId":"case two","variant":"with_docs","model":"fixture/model","runNumber":1,"outcome":"errored"}"#,
    ]
    .join("\n")
        + "\n";
    assert_eq!(observations, expected_observations);

    // report.json / report.txt。
    let report_text = std::fs::read_to_string(artifact_directory.join("report.json")).unwrap();
    let report: kiana_evals::report::EvalComparisonReport =
        serde_json::from_str(&report_text).unwrap();
    assert_eq!(report.protocol_digest, recorded_digest);
    assert_eq!(report.blocked_pairs.len(), 2);
    assert_eq!(report.comparisons.len(), 1);
    assert_eq!(report.comparisons[0].total_pairs, 2);
    assert_eq!(report.comparisons[0].eligible_pairs, 0);
    let formatted = kiana_evals::report::format_eval_comparison_report(&report);
    assert_eq!(
        std::fs::read_to_string(artifact_directory.join("report.txt")).unwrap(),
        format!("{formatted}\n")
    );

    // 每个任务都有独立目录（sha256 命名），且没有 vitest.json。
    let task_directories: Vec<PathBuf> = std::fs::read_dir(artifact_directory.join("tasks"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(task_directories.len(), 4);
    for directory in &task_directories {
        assert_eq!(directory.file_name().unwrap().to_string_lossy().len(), 64);
        assert!(!directory.join("vitest.json").exists());
    }

    // 发现产物位于 discovery/<variant>/ 下。
    for variant in ["without_docs", "with_docs"] {
        assert!(
            artifact_directory
                .join("discovery")
                .join(variant)
                .join("discovered-tests.json")
                .is_file()
        );
    }
}

/// spec §2.5.2 第 5 步：没有任何 `*.docs.eval.ts`（也没有显式请求文件）→
/// `No documentation eval files were selected.`（在鉴权与 docker 之前失败）。
#[tokio::test]
async fn cli_requires_at_least_one_doc_eval_file() {
    let root = tempfile::tempdir().unwrap();
    let environment = |_: &str| -> Option<String> { None };
    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "No documentation eval files were selected."
    );
}

/// spec §2.5.2 第 10 步：两个变体 inspect 出同一个镜像 id →
/// `Documentation variants resolved to the same image.`
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cli_rejects_identical_variant_images() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    fake.set("FAKE_DOCKER_IMAGE_ID", "sha256:same");

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Documentation variants resolved to the same image."
    );
}

/// spec §2.5.2 第 13 步：两个变体都发现 0 个用例 →
/// `No documentation eval cases matched the selection.`
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cli_requires_at_least_one_discovered_case() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    fake.set("FAKE_DOCKER_DISCOVERY_JSON", "[]");

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "No documentation eval cases matched the selection."
    );
}

/// `.eval` 是悬空符号链接：Node 的 `fs.promises.mkdir(..., { recursive: true })` 在祖先
/// 组件上先 `EEXIST`、再 stat 失败，`MKDirpAsync` 把这种失败统一改写成 `ENOTDIR`（路径是
/// 失败组件 `.eval`，不是完整产物目录）。
#[cfg(unix)]
#[tokio::test]
async fn cli_reports_a_dangling_eval_symlink_as_enotdir_like_node() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let eval_root = root.path().join(".eval");
    std::os::unix::fs::symlink("missing-target", &eval_root).unwrap();

    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        format!(
            "ENOTDIR: not a directory, mkdir '{}'",
            eval_root.to_string_lossy()
        )
    );
}

/// `JSON.parse` 失败的消息逐字对齐 V8 的 `SyntaxError`，而不是 serde_json 的文本。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cli_reports_invalid_discovery_json_with_the_v8_message() {
    let _guard = lock_environment();
    let fake = FakeDocker::install();
    fake.set("FAKE_DOCKER_DISCOVERY_JSON", "{not json");

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
}

/// docker 二进制存在且可执行、但不是合法可执行格式：libuv 的 `execvp` 会回退到 `/bin/sh`，
/// 因此 `spawnSync` 不报 spawn 错误，而是得到脚本的退出码。
#[test]
fn build_images_falls_back_to_the_shell_for_an_enoexec_docker() {
    let _guard = lock_environment();
    let _fake = FakeDocker::install();
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("docker");
    std::fs::write(&script, "exit 3\n").unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    let _path = PathOverride::set(&[directory.path()]);

    let error = build_images().unwrap_err();
    assert_eq!(error.to_string(), "docker exited with status 3.");
}

/// PATH 里没有 docker：`spawnSync docker ENOENT`。
#[test]
fn build_images_reports_a_missing_docker_command() {
    let _guard = lock_environment();
    let _fake = FakeDocker::install();
    let empty = tempfile::tempdir().unwrap();
    let _path = PathOverride::set(&[empty.path()]);

    let error = build_images().unwrap_err();
    assert_eq!(error.to_string(), "spawnSync docker ENOENT");
}

/// 假 docker 退出 0 但不写 discovered-tests.json：Node 的 `readFile` 错误文本
/// （`ENOENT: ..., open '<path>'`），而不是 Rust 的 io::Error Display。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cli_reports_a_missing_discovery_file_like_node() {
    let _guard = lock_environment();
    let _fake = FakeDocker::install();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();

    let entries: Vec<PathBuf> = std::fs::read_dir(root.path().join(".eval"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let expected = entries[0]
        .join("discovery")
        .join("without_docs")
        .join("discovered-tests.json");
    assert_eq!(
        error.to_string(),
        format!(
            "ENOENT: no such file or directory, open '{}'",
            expected.to_string_lossy()
        )
    );
}

/// 只读的 `.eval` 目录：`mkdir` 失败文本与 Node 一致（含完整产物路径）。
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cli_reports_mkdir_failures_like_node() {
    use std::os::unix::fs::PermissionsExt as _;
    // root 会忽略权限位。
    // SAFETY: `geteuid` 没有前置条件。
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let _guard = lock_environment();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("evals")).unwrap();
    std::fs::write(
        root.path().join("evals").join("example.docs.eval.ts"),
        "// fixture\n",
    )
    .unwrap();
    let eval_root = root.path().join(".eval");
    std::fs::create_dir(&eval_root).unwrap();
    let mut permissions = std::fs::metadata(&eval_root).unwrap().permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&eval_root, permissions).unwrap();

    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("auth.json"), r#"{"fixture": 1}"#).unwrap();
    let agent_directory = agent.path().to_path_buf();
    let environment = move |name: &str| -> Option<String> {
        match name {
            "KIANA_CODING_AGENT_DIR" => Some(agent_directory.to_string_lossy().into_owned()),
            _ => None,
        }
    };

    let error = run_with(
        strings(&["--provider", "fixture", "--model", "model"]),
        root.path(),
        &environment,
    )
    .await
    .unwrap_err();

    let mut permissions = std::fs::metadata(&eval_root).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&eval_root, permissions).unwrap();

    let message = error.to_string();
    let prefix = format!(
        "EACCES: permission denied, mkdir '{}",
        eval_root.to_string_lossy()
    );
    assert!(message.starts_with(&prefix), "{message}");
    assert!(message.ends_with('\''), "{message}");
    // 报错路径是请求的完整产物目录（`.eval/<runId>`），而不是父目录。
    assert!(
        message.len() > prefix.len() + 2,
        "expected the full artifact path: {message}"
    );
}
