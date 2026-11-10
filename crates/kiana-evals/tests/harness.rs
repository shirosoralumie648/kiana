//! `packages/evals/test/harness.test.ts` 的移植。
//!
//! 10 个 `it` 声明展开为 15 个用例（`it.each` 逐项展开）；每个 Rust 测试的 doc comment
//! 保留 Vitest 的完整标题。
//!
//! 进程环境是全局状态：所有读写环境的用例都持有 [`ENV_LOCK`]，并用 `EnvRestore` 在
//! panic 时也恢复原值。

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kiana_evals::harness::{
    DOCUMENTATION_EVAL_TOOLS, HarnessContext, KianaCodingAgentHarnessOptions,
    KianaCodingAgentModelSelection, ModelSelectionEnvironment, apply_isolated_environment,
    create_kiana_coding_agent_harness, create_kiana_documentation_eval_harness,
    exclude_kiana_documentation, resolve_documentation_variant, resolve_model_selection_from,
    verify_system_prompt,
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// 在 Drop 时把指定环境变量恢复为快照值。
struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn capture(names: &[&'static str]) -> Self {
        EnvRestore(
            names
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect(),
        )
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            // SAFETY: 测试持有 ENV_LOCK，独占进程环境。
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

fn selection(provider: &str, id: &str) -> KianaCodingAgentModelSelection {
    KianaCodingAgentModelSelection {
        provider: provider.to_string(),
        id: id.to_string(),
    }
}

/// 文档评测用的字面 system prompt（含 rules / docs / cwd 三段）。
///
/// TS 用 `buildSystemPrompt` 生成；Rust 侧不导出该函数，因此用等价字面量固定同样的性质。
fn documentation_prompt() -> String {
    [
        "\n<rules>",
        "Use the read tool to inspect files.",
        &format!("Selected tools: {}", DOCUMENTATION_EVAL_TOOLS.join(", ")),
        "</rules>",
        "",
        "<docs>",
        "Kiana documentation (read only)",
        "- readme: /kiana/README.md",
        "- docs: /kiana/docs",
        "- examples: /kiana/examples",
        "docs/models.md",
        "</docs>",
        "<cwd>",
        "/workspace",
        "</cwd>",
    ]
    .join("\n")
}

// ---------------------------------------------------------------------------
// resolveModelSelection
// ---------------------------------------------------------------------------

/// TS: `resolveModelSelection > prefers an explicit harness model`
#[test]
fn resolve_model_selection_prefers_an_explicit_harness_model() {
    let selected = resolve_model_selection_from(
        Some(&selection("anthropic", "claude-opus-4-6")),
        &ModelSelectionEnvironment {
            kiana_provider: Some("openai-codex".to_string()),
            kiana_model: Some("gpt-5.6-sol".to_string()),
        },
    )
    .expect("explicit model");
    assert_eq!(selected, selection("anthropic", "claude-opus-4-6"));
}

/// TS: `resolveModelSelection > uses trimmed environment defaults`
#[test]
fn resolve_model_selection_uses_trimmed_environment_defaults() {
    let selected = resolve_model_selection_from(
        None,
        &ModelSelectionEnvironment {
            kiana_provider: Some(" openai-codex ".to_string()),
            kiana_model: Some(" gpt-5.6-sol ".to_string()),
        },
    )
    .expect("environment defaults");
    assert_eq!(selected, selection("openai-codex", "gpt-5.6-sol"));
}

/// TS: `resolveModelSelection > rejects incomplete model selection`（`{}`）
#[test]
fn resolve_model_selection_rejects_incomplete_model_selection_with_empty_environment() {
    let error = resolve_model_selection_from(None, &ModelSelectionEnvironment::default())
        .expect_err("empty environment must fail");
    assert!(
        error
            .to_string()
            .contains("Select a harness model explicitly"),
        "unexpected error: {error}"
    );
}

/// TS: `resolveModelSelection > rejects incomplete model selection`（只有 `KIANA_PROVIDER`）
#[test]
fn resolve_model_selection_rejects_incomplete_model_selection_with_only_provider() {
    let error = resolve_model_selection_from(
        None,
        &ModelSelectionEnvironment {
            kiana_provider: Some("openai-codex".to_string()),
            kiana_model: None,
        },
    )
    .expect_err("missing model must fail");
    assert!(
        error
            .to_string()
            .contains("Select a harness model explicitly"),
        "unexpected error: {error}"
    );
}

/// TS: `resolveModelSelection > rejects incomplete model selection`（只有 `KIANA_MODEL`）
#[test]
fn resolve_model_selection_rejects_incomplete_model_selection_with_only_model() {
    let error = resolve_model_selection_from(
        None,
        &ModelSelectionEnvironment {
            kiana_provider: None,
            kiana_model: Some("gpt-5.6-sol".to_string()),
        },
    )
    .expect_err("missing provider must fail");
    assert!(
        error
            .to_string()
            .contains("Select a harness model explicitly"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------------------------
// isolateProcessEnvironment
// ---------------------------------------------------------------------------

/// TS: `isolateProcessEnvironment > removes runner metadata and restores the process environment`
#[test]
fn removes_runner_metadata_and_restores_the_process_environment() {
    let _lock = env_lock();
    let _restore = EnvRestore::capture(&[
        "HOME",
        "USERPROFILE",
        "KIANA_CODING_AGENT_DIR",
        "KIANA_EVAL_VARIANT",
        "KIANA_EVAL_ARTIFACT_DIR",
    ]);
    // SAFETY: 测试持有 ENV_LOCK，独占进程环境。
    unsafe {
        std::env::set_var("KIANA_EVAL_VARIANT", "with_docs");
        std::env::set_var("KIANA_EVAL_ARTIFACT_DIR", "/tmp/artifacts");
    }
    let old_home = std::env::var_os("HOME");
    let restore = apply_isolated_environment("/tmp/eval-home", "/tmp/eval-agent");
    assert_eq!(std::env::var("HOME").expect("HOME set"), "/tmp/eval-home");
    assert_eq!(
        std::env::var("KIANA_CODING_AGENT_DIR").expect("agent dir set"),
        "/tmp/eval-agent"
    );
    assert!(std::env::var_os("KIANA_EVAL_VARIANT").is_none());
    assert!(std::env::var_os("KIANA_EVAL_ARTIFACT_DIR").is_none());
    restore();
    assert_eq!(std::env::var_os("HOME"), old_home);
    assert_eq!(
        std::env::var("KIANA_EVAL_VARIANT").expect("variant restored"),
        "with_docs"
    );
}

// ---------------------------------------------------------------------------
// documentation variant
// ---------------------------------------------------------------------------

/// TS: `documentation variant > accepts without_docs`
#[test]
fn documentation_variant_accepts_without_docs() {
    assert_eq!(
        resolve_documentation_variant(Some("without_docs")).expect("valid variant"),
        kiana_evals::plan::DocumentationVariant::WithoutDocs
    );
}

/// TS: `documentation variant > accepts with_docs`
#[test]
fn documentation_variant_accepts_with_docs() {
    assert_eq!(
        resolve_documentation_variant(Some("with_docs")).expect("valid variant"),
        kiana_evals::plan::DocumentationVariant::WithDocs
    );
}

/// TS: `documentation variant > rejects invalid variant undefined`
#[test]
fn documentation_variant_rejects_invalid_variant_undefined() {
    let _lock = env_lock();
    let _restore = EnvRestore::capture(&["KIANA_EVAL_VARIANT"]);
    // SAFETY: 测试持有 ENV_LOCK，独占进程环境。
    unsafe { std::env::remove_var("KIANA_EVAL_VARIANT") };
    let error = resolve_documentation_variant(None).expect_err("undefined variant must fail");
    assert!(
        error.to_string().contains("KIANA_EVAL_VARIANT"),
        "unexpected error: {error}"
    );
}

/// TS: `documentation variant > rejects invalid variant `（空字符串）
#[test]
fn documentation_variant_rejects_invalid_variant_empty() {
    let error = resolve_documentation_variant(Some("")).expect_err("empty variant must fail");
    assert!(
        error.to_string().contains("KIANA_EVAL_VARIANT"),
        "unexpected error: {error}"
    );
}

/// TS: `documentation variant > rejects invalid variant other`
#[test]
fn documentation_variant_rejects_invalid_variant_other() {
    let error =
        resolve_documentation_variant(Some("other")).expect_err("unknown variant must fail");
    assert!(
        error.to_string().contains("KIANA_EVAL_VARIANT"),
        "unexpected error: {error}"
    );
}

/// TS: `documentation variant > strips only the documentation routing section from the default Kiana prompt`
#[test]
fn strips_only_the_documentation_routing_section_from_the_default_kiana_prompt() {
    let prompt = documentation_prompt();
    assert!(prompt.contains("\n<docs>\nKiana documentation (read only"));
    assert!(prompt.contains("\n<rules>\n"));
    assert!(prompt.contains("\n<cwd>\n/workspace\n</cwd>"));
    assert!(prompt.contains("docs/models.md"));

    let stripped = exclude_kiana_documentation(&prompt).expect("strip documentation");
    assert!(stripped.contains("\n<rules>\n"));
    assert!(stripped.contains("\n<cwd>\n/workspace\n</cwd>"));
    assert!(!stripped.contains("<docs>"));
    assert!(!stripped.contains("Kiana documentation"));
    assert!(!stripped.contains("docs/models.md"));
    assert!(!stripped.contains("/kiana/README.md"));
    assert!(!stripped.contains("/kiana/docs"));
    assert!(!stripped.contains("/kiana/examples"));
}

/// TS: `documentation variant > verifies the prompt that was sent`
#[test]
fn verifies_the_prompt_that_was_sent() {
    let prompt = documentation_prompt();
    let stripped = exclude_kiana_documentation(&prompt).expect("strip documentation");
    let without_docs = KianaCodingAgentHarnessOptions {
        name: Some("without_docs".to_string()),
        expected_kiana_documentation: Some(false),
        ..Default::default()
    };
    assert_eq!(
        verify_system_prompt(&stripped, &without_docs).expect("stripped prompt verifies"),
        stripped
    );
    let error = verify_system_prompt(&prompt, &without_docs).expect_err("full prompt must fail");
    assert!(
        error.to_string().contains("does not match"),
        "unexpected error: {error}"
    );
}

/// TS: `documentation variant > fails closed when prompt markers are missing`
#[test]
fn fails_closed_when_prompt_markers_are_missing() {
    let error = exclude_kiana_documentation("Instructions").expect_err("missing docs must fail");
    assert!(
        error.to_string().contains("no Kiana documentation section"),
        "unexpected error: {error}"
    );
    let error = exclude_kiana_documentation("\n<docs>\nKiana documentation\n</docs>")
        .expect_err("missing cwd must fail");
    assert!(
        error.to_string().contains("no working-directory section"),
        "unexpected error: {error}"
    );
}

/// TS: `documentation variant > verifies the prompt that was sent`（`with_docs` 方向）。
#[test]
fn verifies_a_with_docs_prompt_and_rejects_a_stripped_one() {
    let prompt = documentation_prompt();
    let with_docs = KianaCodingAgentHarnessOptions {
        name: Some("with_docs".to_string()),
        expected_kiana_documentation: Some(true),
        ..Default::default()
    };
    assert_eq!(
        verify_system_prompt(&prompt, &with_docs).expect("documented prompt verifies"),
        prompt
    );
    let stripped = exclude_kiana_documentation(&prompt).expect("strip documentation");
    let error = verify_system_prompt(&stripped, &with_docs).expect_err("stripped prompt must fail");
    assert!(
        error.to_string().contains("does not match"),
        "unexpected error: {error}"
    );
}

/// `expectedKianaDocumentation === undefined` 时不做任何检查。
#[test]
fn skips_verification_when_no_documentation_expectation_is_set() {
    let options = KianaCodingAgentHarnessOptions::default();
    assert_eq!(
        verify_system_prompt("anything", &options).expect("no expectation"),
        "anything"
    );
}

/// 缺 `<rules>` 段时报告 `lost its rules`（`${options.name}` 为 `undefined` 时逐字输出）。
#[test]
fn reports_a_missing_rules_section() {
    let options = KianaCodingAgentHarnessOptions {
        expected_kiana_documentation: Some(false),
        ..Default::default()
    };
    let error = verify_system_prompt("no sections", &options).expect_err("missing rules must fail");
    assert_eq!(
        error.to_string(),
        "Kiana system prompt lost its rules in the undefined eval variant."
    );
    let named = KianaCodingAgentHarnessOptions {
        name: Some("without_docs".to_string()),
        ..options
    };
    let error = verify_system_prompt("no sections", &named).expect_err("missing rules must fail");
    assert_eq!(
        error.to_string(),
        "Kiana system prompt lost its rules in the without_docs eval variant."
    );
}

/// TS: `documentation variant > rejects documentation harnesses outside the container sandbox`
#[test]
fn rejects_documentation_harnesses_outside_the_container_sandbox() {
    let _lock = env_lock();
    let _restore = EnvRestore::capture(&[
        "KIANA_EVAL_CONTAINER",
        "KIANA_EVAL_SANDBOX_UID",
        "KIANA_EVAL_SANDBOX_GID",
    ]);
    // SAFETY: 测试持有 ENV_LOCK，独占进程环境。
    unsafe {
        std::env::remove_var("KIANA_EVAL_CONTAINER");
        std::env::remove_var("KIANA_EVAL_SANDBOX_UID");
        std::env::remove_var("KIANA_EVAL_SANDBOX_GID");
    }
    match create_kiana_documentation_eval_harness(KianaCodingAgentHarnessOptions::default()) {
        Ok(_) => panic!("documentation harness must be rejected outside the container sandbox"),
        Err(error) => assert!(
            error.to_string().contains("isolated container sandbox"),
            "unexpected error: {error}"
        ),
    }
}

// ---------------------------------------------------------------------------
// runKianaCodingAgent（TS 无对应用例；Rust-only 冒烟测试，验证隔离环境与 cleanup 链路）
// ---------------------------------------------------------------------------

/// Rust-only: a missing model fails before any session is created and still restores the
/// process environment. The failure must carry a normalized harness run (vitest-evals
/// `createFailedHarnessRun`) with the input user message, the context artifacts and a trace.
#[test]
fn missing_model_reports_eval_model_not_found_and_restores_the_environment() {
    let _lock = env_lock();
    let _restore = EnvRestore::capture(&["HOME", "USERPROFILE", "KIANA_CODING_AGENT_DIR"]);
    let old_home = std::env::var_os("HOME");
    let host_agent_dir = tempfile::tempdir().expect("host agent dir");
    // SAFETY: 测试持有 ENV_LOCK；把 host agent dir 指向临时目录，避免读取真实的 ~/.kiana。
    unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", host_agent_dir.path()) };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    let error = local.block_on(&runtime, async {
        let harness = create_kiana_coding_agent_harness(KianaCodingAgentHarnessOptions {
            model: Some(selection("missing", "missing")),
            ..Default::default()
        });
        let context = HarnessContext::new(None);
        context.set_artifact("preset", serde_json::json!("value"));
        harness
            .run(
                kiana_evals::harness::KianaCodingAgentInput::Prompt("hello".to_string()),
                context,
            )
            .await
            .expect_err("missing model must fail")
    });
    assert_eq!(error.to_string(), "Eval model not found: missing/missing");
    assert_eq!(error.name(), "Error");

    let run = error.run().expect("failed run is attached");
    assert_eq!(run.session.events.len(), 1);
    match &run.session.events[0] {
        kiana_evals::report::TranscriptEvent::Message { role, content, .. } => {
            assert_eq!(*role, kiana_evals::report::TranscriptRole::User);
            assert_eq!(content, &Some(serde_json::json!("hello")));
        }
        other => panic!("expected a user message event, got {other:?}"),
    }
    assert_eq!(run.errors.len(), 1);
    assert_eq!(
        run.errors[0]
            .get("message")
            .and_then(|value| value.as_str()),
        Some("Eval model not found: missing/missing")
    );
    assert_eq!(
        run.artifacts
            .as_ref()
            .and_then(|artifacts| artifacts.get("preset")),
        Some(&serde_json::json!("value"))
    );
    let traces = run.traces.as_ref().expect("failed run carries a trace");
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].spans.len(), 1);
    assert_eq!(
        traces[0].spans[0].status,
        Some(kiana_evals::report::SpanStatus::Error)
    );
    assert_eq!(std::env::var_os("HOME"), old_home);
}

/// Rust-only 回归：`mkdtemp` 失败的错误文案带 libuv 生成的随机后缀（不是模板 `XXXXXX`），
/// 并且仍会挂载合成的失败 run（`createFailedHarnessRun`）。
#[test]
fn mkdtemp_failures_report_the_generated_template_path() {
    let _lock = env_lock();
    let _restore = EnvRestore::capture(&[
        "TMPDIR",
        "HOME",
        "USERPROFILE",
        "KIANA_CODING_AGENT_DIR",
        "KIANA_PROVIDER",
        "KIANA_MODEL",
    ]);
    let root = tempfile::tempdir().expect("root");
    let missing = root.path().join("missing");
    let host_agent_dir = tempfile::tempdir().expect("host agent dir");
    // SAFETY: 测试持有 ENV_LOCK，独占进程环境。
    unsafe {
        std::env::set_var("TMPDIR", &missing);
        std::env::set_var("KIANA_CODING_AGENT_DIR", host_agent_dir.path());
        std::env::set_var("KIANA_PROVIDER", "fixture");
        std::env::set_var("KIANA_MODEL", "fixture/model");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    let error = local.block_on(&runtime, async {
        let harness = create_kiana_coding_agent_harness(KianaCodingAgentHarnessOptions::default());
        let context = HarnessContext::new(None);
        harness
            .run(
                kiana_evals::harness::KianaCodingAgentInput::Prompt("hello".to_string()),
                context,
            )
            .await
            .expect_err("missing TMPDIR must fail")
    });

    let message = error.to_string();
    let prefix = format!(
        "ENOENT: no such file or directory, mkdtemp '{}",
        missing.join("kiana-eval-").display()
    );
    assert!(message.starts_with(&prefix), "{message}");
    assert!(message.ends_with('\''), "{message}");
    let suffix = &message[prefix.len()..message.len() - 1];
    assert_eq!(suffix.len(), 6, "{message}");
    assert!(
        suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        "{message}"
    );
    assert!(!message.contains("XXXXXX"), "{message}");
    assert!(error.run().is_some(), "failed runs are attached");
}
