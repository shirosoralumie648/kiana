//! `model-resolver.test.ts` 的移植。
//!
//! 黄金夹具由 TS 参考实现生成（`tests/fixtures/resolver/*.json`）：
//! - `reference_cases.json`：`test.each(referenceCases)` 的 36 条（含目录正反两种顺序）；
//! - `scope_cases.json`：`resolveModelScopeWithDiagnostics` 的 6 组（诊断、同 id 多 provider、
//!   方括号 id、glob）；
//! - `cli_cases.json`：`resolveCliModel` 的 13 条；
//! - `initial_model_cases.json`：`findInitialModel` 的 2 条；
//! - `default_models.json`：`defaultModelPerProvider` 的声明顺序与字面值。
//!
//! `persisted default model scoping` 的 3 条依赖 `AgentSession` + `createTestResourceLoader`，
//! 按计划改由 `kiana-session`（阶段 6）覆盖。`modelResolveScope` 的 `console.warn` 输出属于
//! 进程级副作用，这里只断言返回值（文案已由诊断夹具逐字覆盖）。

use std::collections::HashSet;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{AuthError, AuthOperationOptions};
use kiana_ai::types::{Model, ModelInputModality, ModelThinkingLevel};
use kiana_config::models::resolver::{
    DEFAULT_MODEL_PER_PROVIDER, FindInitialModelOptions, ModelReferenceRequest,
    ModelReferenceResult, ModelResolverRuntime, ResolveCliModelOptions, ScopedModel,
    ThinkingSuffixMode, default_models_ordered, find_exact_model_reference_match,
    find_initial_model, parse_thinking_level, resolve_cli_model, resolve_model_reference,
    resolve_model_scope, resolve_model_scope_from_models, resolve_model_scope_with_diagnostics,
    resolve_model_scope_writing_to, restore_model_from_session,
};
use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ReferenceFixture {
    catalog: Vec<Model>,
    cases: Vec<ReferenceCase>,
}

#[derive(Debug, Deserialize)]
struct ReferenceCase {
    name: String,
    text: String,
    provider: Option<String>,
    #[serde(rename = "thinkingSuffix")]
    thinking_suffix: String,
    fuzzy: bool,
    usable: Vec<String>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum Expected {
    Resolved {
        model: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: Option<String>,
        warning: Option<String>,
    },
    Ambiguous {
        candidates: Vec<String>,
        #[serde(rename = "usableCount")]
        usable_count: usize,
        #[serde(rename = "thinkingLevel")]
        thinking_level: Option<String>,
        warning: Option<String>,
    },
    NotFound,
}

fn fixture(name: &str) -> Value {
    let text = match name {
        "reference_cases.json" => include_str!("fixtures/resolver/reference_cases.json"),
        "scope_cases.json" => include_str!("fixtures/resolver/scope_cases.json"),
        "scope_unicode_cases.json" => {
            include_str!("fixtures/resolver/scope_unicode_cases.json")
        }
        "cli_cases.json" => include_str!("fixtures/resolver/cli_cases.json"),
        "initial_model_cases.json" => include_str!("fixtures/resolver/initial_model_cases.json"),
        "default_models.json" => include_str!("fixtures/resolver/default_models.json"),
        other => panic!("unknown fixture {other}"),
    };
    serde_json::from_str(text).expect("fixture parses")
}

fn model_from(value: &Value) -> Model {
    serde_json::from_value(value.clone()).expect("model fixture parses")
}

fn models_from(value: &Value) -> Vec<Model> {
    value
        .as_array()
        .expect("models array")
        .iter()
        .map(model_from)
        .collect()
}

fn thinking_suffix_mode(value: &str) -> ThinkingSuffixMode {
    match value {
        "none" => ThinkingSuffixMode::None,
        "strict" => ThinkingSuffixMode::Strict,
        "lenient" => ThinkingSuffixMode::Lenient,
        other => panic!("unknown thinking suffix mode {other}"),
    }
}

fn thinking(value: &Option<String>) -> Option<ModelThinkingLevel> {
    value
        .as_deref()
        .map(|level| parse_thinking_level(level).expect("valid thinking level"))
}

fn model_ref(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

/// 排序键与 TS 测试一致（`localeCompare`）。
fn sorted_refs(models: &[Model]) -> Vec<String> {
    let mut refs: Vec<String> = models.iter().map(model_ref).collect();
    refs.sort_by(|a, b| kiana_ai::model_catalog::locale_compare(a, b));
    refs
}

// ---------------------------------------------------------------------------
// 假模型运行时（TS 的 duck-typed registry）
// ---------------------------------------------------------------------------

struct FakeRuntime {
    models: Vec<Model>,
    configured: HashSet<String>,
    available: Vec<Model>,
    /// `getModel` 的查找表（provider, modelId, model）。
    lookups: Vec<Model>,
}

impl FakeRuntime {
    fn new(models: Vec<Model>) -> Self {
        Self {
            available: models.clone(),
            lookups: models.clone(),
            configured: HashSet::new(),
            models,
        }
    }

    fn with_configured(mut self, providers: &[String]) -> Self {
        self.configured = providers.iter().cloned().collect();
        self
    }

    fn with_available(mut self, available: Vec<Model>) -> Self {
        self.available = available;
        self
    }
}

#[async_trait]
impl ModelResolverRuntime for FakeRuntime {
    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.lookups
            .iter()
            .find(|model| model.provider == provider && model.id == model_id)
            .cloned()
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        self.configured.contains(provider)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.available.clone()
    }

    async fn get_available(
        &self,
        _provider: Option<&str>,
        _options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        Ok(self.available.clone())
    }
}

// ---------------------------------------------------------------------------
// resolveModelReference（test.each(referenceCases)）
// ---------------------------------------------------------------------------

#[test]
fn reference_cases_match_the_reference_implementation() {
    let fixture: ReferenceFixture =
        serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
    let mut reversed_catalog = fixture.catalog.clone();
    reversed_catalog.reverse();

    for test_case in &fixture.cases {
        let usable: HashSet<String> = test_case.usable.iter().cloned().collect();
        let request = ModelReferenceRequest {
            text: test_case.text.clone(),
            provider: test_case.provider.clone(),
            thinking_suffix: thinking_suffix_mode(&test_case.thinking_suffix),
            fuzzy: test_case.fuzzy,
        };
        // 目录顺序永不决定结果。
        for catalog in [&fixture.catalog, &reversed_catalog] {
            let result = resolve_model_reference(&request, catalog, &|provider: &str| {
                usable.contains(provider)
            });
            assert_case(&test_case.name, &result, &test_case.expected);
        }
    }
}

fn assert_case(name: &str, result: &ModelReferenceResult, expected: &Expected) {
    match expected {
        Expected::NotFound => {
            assert!(
                matches!(result, ModelReferenceResult::NotFound { .. }),
                "{name}: expected not-found, got {result:?}"
            );
        }
        Expected::Ambiguous {
            candidates,
            usable_count,
            thinking_level,
            warning,
        } => {
            let ModelReferenceResult::Ambiguous {
                candidates: actual,
                usable_count: actual_usable,
                thinking_level: actual_thinking,
                warning: actual_warning,
            } = result
            else {
                panic!("{name}: expected ambiguous, got {result:?}");
            };
            assert_eq!(&sorted_refs(actual), candidates, "{name}");
            assert_eq!(actual_usable, usable_count, "{name}");
            assert_eq!(*actual_thinking, thinking(thinking_level), "{name}");
            assert_eq!(actual_warning.as_deref(), warning.as_deref(), "{name}");
        }
        Expected::Resolved {
            model,
            thinking_level,
            warning,
        } => {
            let ModelReferenceResult::Resolved {
                model: actual,
                thinking_level: actual_thinking,
                warning: actual_warning,
            } = result
            else {
                panic!("{name}: expected resolved, got {result:?}");
            };
            assert_eq!(&model_ref(actual), model, "{name}");
            assert_eq!(*actual_thinking, thinking(thinking_level), "{name}");
            assert_eq!(actual_warning.as_deref(), warning.as_deref(), "{name}");
        }
    }
}

#[test]
fn find_exact_model_reference_match_ignores_suffixes_and_fuzzy() {
    let fixture: ReferenceFixture =
        serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
    let catalog = &fixture.catalog;
    assert_eq!(
        find_exact_model_reference_match("anthropic/claude-sonnet-4-5", catalog)
            .map(|model| model_ref(&model)),
        Some("anthropic/claude-sonnet-4-5".to_string())
    );
    assert_eq!(
        find_exact_model_reference_match("sonnet", catalog).map(|model| model_ref(&model)),
        None
    );
    assert_eq!(
        find_exact_model_reference_match("gpt-4o:high", catalog).map(|model| model_ref(&model)),
        None
    );
    assert_eq!(
        find_exact_model_reference_match("anthropic/exact", catalog).map(|model| model_ref(&model)),
        Some("anthropic/exact".to_string())
    );
}

// ---------------------------------------------------------------------------
// resolveModelScope*
// ---------------------------------------------------------------------------

#[test]
fn scope_cases_match_the_reference_implementation() {
    let fixture = fixture("scope_cases.json");
    for case in fixture["cases"].as_array().expect("cases") {
        let models = models_from(&case["models"]);
        let patterns: Vec<String> = case["patterns"]
            .as_array()
            .expect("patterns")
            .iter()
            .map(|value| value.as_str().expect("pattern").to_string())
            .collect();
        let result = resolve_model_scope_from_models(&patterns, &models);
        let actual_scoped: Vec<(String, Option<String>)> = result
            .scoped_models
            .iter()
            .map(|scoped| {
                (
                    model_ref(&scoped.model),
                    scoped
                        .thinking_level
                        .and_then(|level| serde_json::to_value(level).ok())
                        .and_then(|value| value.as_str().map(str::to_string)),
                )
            })
            .collect();
        let expected_scoped: Vec<(String, Option<String>)> = case["scopedModels"]
            .as_array()
            .expect("scopedModels")
            .iter()
            .map(|scoped| {
                (
                    scoped["model"].as_str().expect("model").to_string(),
                    scoped["thinkingLevel"].as_str().map(str::to_string),
                )
            })
            .collect();
        assert_eq!(
            actual_scoped, expected_scoped,
            "scoped models for {}",
            case["name"]
        );

        let actual_diagnostics: Vec<(String, String, String, String)> = result
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.diagnostic_type.as_str().to_string(),
                    diagnostic.code.as_str().to_string(),
                    diagnostic.message.clone(),
                    diagnostic.pattern.clone(),
                )
            })
            .collect();
        let expected_diagnostics: Vec<(String, String, String, String)> = case["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic["type"].as_str().expect("type").to_string(),
                    diagnostic["code"].as_str().expect("code").to_string(),
                    diagnostic["message"].as_str().expect("message").to_string(),
                    diagnostic["pattern"].as_str().expect("pattern").to_string(),
                )
            })
            .collect();
        assert_eq!(
            actual_diagnostics, expected_diagnostics,
            "diagnostics for {}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn resolve_model_scope_with_diagnostics_reads_available_models() {
    let fixture: ReferenceFixture =
        serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
    let runtime = FakeRuntime::new(fixture.catalog.clone()).with_available(vec![
        find_model(&fixture.catalog, "anthropic", "claude-sonnet-4-5"),
        find_model(&fixture.catalog, "openai", "gpt-4o"),
    ]);
    let patterns = vec![
        "sonnet:high".to_string(),
        "gpt-4o:invalid".to_string(),
        "missing".to_string(),
    ];
    let result =
        resolve_model_scope_with_diagnostics(&patterns, &runtime, AuthOperationOptions::default())
            .await
            .expect("scope");
    let ids: Vec<String> = result
        .scoped_models
        .iter()
        .map(|scoped| scoped.model.id.clone())
        .collect();
    assert_eq!(ids, vec!["claude-sonnet-4-5", "gpt-4o"]);
    assert_eq!(
        result.scoped_models[0].thinking_level,
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(result.scoped_models[1].thinking_level, None);
    assert_eq!(result.diagnostics.len(), 2);
    assert_eq!(
        result.diagnostics[0].message,
        "Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". Using default instead."
    );
    assert_eq!(
        result.diagnostics[1].message,
        "No models match pattern \"missing\""
    );
}

#[test]
fn unicode_and_minimatch_scope_cases_match_the_reference_implementation() {
    // 审查修复的回归：Unicode 大小写折叠、`?` 按 UTF-16 码元计数、POSIX 类、
    // extglob、未闭合 `[`、dot 规则、`{}` 字面量与 `**/`。夹具由 TS 参考实现生成。
    let fixture = fixture("scope_unicode_cases.json");
    let models = models_from(&fixture["models"]);
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let patterns: Vec<String> = case["patterns"]
            .as_array()
            .expect("patterns")
            .iter()
            .map(|value| value.as_str().expect("pattern").to_string())
            .collect();
        let result = resolve_model_scope_from_models(&patterns, &models);
        let actual_scoped: Vec<(String, Option<String>)> = result
            .scoped_models
            .iter()
            .map(|scoped| {
                (
                    model_ref(&scoped.model),
                    scoped
                        .thinking_level
                        .and_then(|level| serde_json::to_value(level).ok())
                        .and_then(|value| value.as_str().map(str::to_string)),
                )
            })
            .collect();
        let expected_scoped: Vec<(String, Option<String>)> = case["scopedModels"]
            .as_array()
            .expect("scopedModels")
            .iter()
            .map(|scoped| {
                (
                    scoped["model"].as_str().expect("model").to_string(),
                    scoped["thinkingLevel"].as_str().map(str::to_string),
                )
            })
            .collect();
        assert_eq!(actual_scoped, expected_scoped, "scoped models for {name}");

        let actual_diagnostics: Vec<(String, String, String, String)> = result
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.diagnostic_type.as_str().to_string(),
                    diagnostic.code.as_str().to_string(),
                    diagnostic.message.clone(),
                    diagnostic.pattern.clone(),
                )
            })
            .collect();
        let expected_diagnostics: Vec<(String, String, String, String)> = case["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic["type"].as_str().expect("type").to_string(),
                    diagnostic["code"].as_str().expect("code").to_string(),
                    diagnostic["message"].as_str().expect("message").to_string(),
                    diagnostic["pattern"].as_str().expect("pattern").to_string(),
                )
            })
            .collect();
        assert_eq!(
            actual_diagnostics, expected_diagnostics,
            "diagnostics for {name}"
        );
    }
}

const SCOPE_WARNING_CHILD_ENV: &str = "KIANA_CONFIG_SCOPE_WARNING_CHILD";

fn plain_model(provider: &str, id: &str) -> Model {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": id,
        "api": "openai-completions",
        "provider": provider,
        "baseUrl": "https://example.invalid",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128000,
        "maxTokens": 8192
    }))
    .expect("model")
}

/// 审查修复的端到端回归：`resolveModelScopeFromModels` 的 glob 分支使用
/// minimatch@10.2.6 的转义/快捷路径、dot 遍历守卫、Canonicalize 与 `String(number)`。
#[test]
fn scope_glob_escapes_dot_rules_and_numeric_braces_match_the_reference() {
    let models = vec![
        plain_model("p", "a.json"),
        plain_model("p", "ab"),
        plain_model("p", "x"),
        plain_model("p", "1e+21"),
        plain_model("p", "."),
        plain_model("p", ".."),
        plain_model("p", ".x"),
    ];
    let scope = |pattern: &str| {
        let result = resolve_model_scope_from_models(&[pattern.to_string()], &models);
        (
            result
                .scoped_models
                .iter()
                .map(|scoped| scoped.model.id.clone())
                .collect::<Vec<_>>(),
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str().to_string())
                .collect::<Vec<_>>(),
        )
    };

    // `starDotExtRE` 的快捷路径覆盖 AST：`*\.json` 匹配不到任何 id。
    assert_eq!(scope("*\\.json"), (vec![], vec!["no-match".to_string()]));
    // `\|` 注入正则后是交替：`\|*` 匹配所有模型（`^|...` 的空分支）。
    assert_eq!(
        scope("\\|*").0,
        vec!["a.json", "ab", "x", "1e+21", ".", "..", ".x"]
    );
    // dot 遍历守卫："." / ".." 不被 `.*` 选中。
    assert_eq!(scope(".*").0, vec![".x"]);
    // brace 数字范围用 `String(number)`：1e21 写成 "1e+21"。
    assert_eq!(
        scope("*{1000000000000000000000..1000000000000000000000}").0,
        vec!["1e+21"]
    );
}

#[tokio::test]
async fn resolve_model_scope_preserves_cli_warning_output() {
    // TS 用 `vi.spyOn(console, "warn")` 断言 `resolveModelScope` 的输出。Rust 侧用
    // 可注入 writer 覆盖文案/顺序，并用子进程捕获真实 stderr 覆盖「写到 stderr」。
    if let Some(mode) = std::env::var_os(SCOPE_WARNING_CHILD_ENV) {
        let fixture: ReferenceFixture =
            serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
        let runtime =
            FakeRuntime::new(fixture.catalog.clone()).with_available(fixture.catalog.clone());
        let patterns = vec![
            "missing".to_string(),
            "gpt-4o:invalid".to_string(),
            "also-missing".to_string(),
        ];
        if mode == "writer" {
            let mut buffer: Vec<u8> = Vec::new();
            let scoped = resolve_model_scope_writing_to(
                &patterns,
                &runtime,
                AuthOperationOptions::default(),
                &mut buffer,
            )
            .await
            .expect("scope");
            assert_eq!(
                scoped
                    .iter()
                    .map(|scoped| model_ref(&scoped.model))
                    .collect::<Vec<_>>(),
                vec!["openai/gpt-4o".to_string()]
            );
            let output = String::from_utf8(buffer).expect("utf8");
            let expected = concat!(
                "Warning: No models match pattern \"missing\"\n",
                "Warning: Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". ",
                "Using default instead.\n",
                "Warning: No models match pattern \"also-missing\"\n",
            );
            assert_eq!(output, expected);
        } else if mode == "diagnostics" {
            // TS 的负向断言：`resolveModelScopeWithDiagnostics` 不写 console.warn。
            let result = resolve_model_scope_with_diagnostics(
                &patterns,
                &runtime,
                AuthOperationOptions::default(),
            )
            .await
            .expect("scope");
            // missing 与 also-missing 各一条 no-match，gpt-4o:invalid 一条
            // invalid-thinking-level。
            assert_eq!(result.diagnostics.len(), 3);
        } else {
            // public / force-color 都走公开的 resolve_model_scope。
            let scoped = resolve_model_scope(&patterns, &runtime, AuthOperationOptions::default())
                .await
                .expect("scope");
            assert_eq!(
                scoped
                    .iter()
                    .map(|scoped| model_ref(&scoped.model))
                    .collect::<Vec<_>>(),
                vec!["openai/gpt-4o".to_string()]
            );
        }
        return;
    }

    let executable = std::env::current_exe().expect("current test executable");
    let huge_force_color = "9".repeat(400);
    for mode in ["writer", "public", "diagnostics", "force-color"] {
        let mut command = std::process::Command::new(&executable);
        command
            .args([
                "--exact",
                "resolve_model_scope_preserves_cli_warning_output",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(SCOPE_WARNING_CHILD_ENV, mode)
            .env_remove("FORCE_COLOR");
        if mode == "force-color" {
            // supports-color：超长数字 FORCE_COLOR 仍是 level 3（审查修复的溢出回归）。
            command.env("FORCE_COLOR", &huge_force_color);
        }
        let output = command.output().expect("spawn child test");
        assert!(
            output.status.success(),
            "{mode} child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode == "public" || mode == "force-color" {
            let expected = concat!(
                "Warning: No models match pattern \"missing\"\n",
                "Warning: Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". ",
                "Using default instead.\n",
                "Warning: No models match pattern \"also-missing\"\n",
            );
            let expected = if mode == "force-color" {
                expected
                    .lines()
                    .map(|line| format!("\u{1b}[33m{line}\u{1b}[39m"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n"
            } else {
                // 子进程的 stdout 是管道（非 TTY）且没有 FORCE_COLOR：chalk 不上色。
                expected.to_string()
            };
            assert_eq!(String::from_utf8_lossy(&output.stderr), expected, "{mode}");
        } else {
            // writer / diagnostics：都不应写 stderr。
            assert!(output.stderr.is_empty(), "{mode}: {:?}", output.stderr);
        }
    }
}

#[tokio::test]
async fn all_valid_thinking_levels_work_in_the_custom_model_fallback_path() {
    // TS: model-resolver.test.ts "all valid thinking levels work in fallback path".
    let fixture: ReferenceFixture =
        serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
    let mut models = fixture.catalog.clone();
    models.push(Model {
        id: "some-base-model".to_string(),
        name: "Some Base Model".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "neuralwatt".to_string(),
        base_url: "https://api.neuralwatt.com".to_string(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 1.0,
            ..kiana_ai::types::ModelCost::default()
        },
        context_window: Some(128000),
        max_tokens: Some(8192),
        ..Model::default()
    });
    let runtime = FakeRuntime::new(models);

    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let result = resolve_cli_model(&ResolveCliModelOptions {
            cli_provider: None,
            cli_model: Some(format!("neuralwatt/zai-org/GLM-5.1-FP8:{level}")),
            cli_thinking: None,
            inline_api_key: false,
            model_runtime: &runtime,
        });
        assert_eq!(result.error, None, "{level}");
        let model = result.model.expect("fallback model");
        assert_eq!(model.provider, "neuralwatt", "{level}");
        assert_eq!(model.id, "zai-org/GLM-5.1-FP8", "{level}");
        assert_eq!(
            result.thinking_level,
            parse_thinking_level(level),
            "{level}"
        );
        // TS：除 off 外把 fallback 模型标成 reasoning:true。
        assert_eq!(model.reasoning, Some(level != "off"), "{level}");
        assert_eq!(
            result.warning.as_deref(),
            Some(
                "Model \"zai-org/GLM-5.1-FP8\" not found for provider \"neuralwatt\". Using custom model id."
            ),
            "{level}"
        );
    }
}

fn find_model(models: &[Model], provider: &str, id: &str) -> Model {
    models
        .iter()
        .find(|model| model.provider == provider && model.id == id)
        .unwrap_or_else(|| panic!("missing fixture model {provider}/{id}"))
        .clone()
}

// ---------------------------------------------------------------------------
// resolveCliModel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cli_cases_match_the_reference_implementation() {
    let fixture = fixture("cli_cases.json");
    for case in fixture["cases"].as_array().expect("cases") {
        let models = models_from(&case["models"]);
        let configured: Vec<String> = case["configuredProviders"]
            .as_array()
            .expect("configuredProviders")
            .iter()
            .map(|value| value.as_str().expect("provider").to_string())
            .collect();
        let runtime = FakeRuntime::new(models).with_configured(&configured);
        let result = resolve_cli_model(&ResolveCliModelOptions {
            cli_provider: case["cliProvider"].as_str().map(str::to_string),
            cli_model: case["cliModel"].as_str().map(str::to_string),
            cli_thinking: case["cliThinking"].as_str().and_then(parse_thinking_level),
            inline_api_key: case["inlineApiKey"].as_bool().unwrap_or(false),
            model_runtime: &runtime,
        });
        let expected = &case["expected"];
        let actual_model = result.model.as_ref().map(|model| {
            (
                model.provider.clone(),
                model.id.clone(),
                model.reasoning.unwrap_or(false),
            )
        });
        let expected_model = expected["model"].as_object().map(|model| {
            (
                model["provider"].as_str().expect("provider").to_string(),
                model["id"].as_str().expect("id").to_string(),
                model["reasoning"].as_bool().unwrap_or(false),
            )
        });
        assert_eq!(actual_model, expected_model, "{}", case["name"]);
        assert_eq!(
            result
                .thinking_level
                .and_then(|level| serde_json::to_value(level).ok())
                .and_then(|value| value.as_str().map(str::to_string)),
            expected["thinkingLevel"].as_str().map(str::to_string),
            "{}",
            case["name"]
        );
        assert_eq!(
            result.warning.as_deref(),
            expected["warning"].as_str(),
            "{}",
            case["name"]
        );
        assert_eq!(
            result.error.as_deref(),
            expected["error"].as_str(),
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn resolve_cli_model_without_a_model_returns_nothing() {
    let runtime = FakeRuntime::new(Vec::new());
    let result = resolve_cli_model(&ResolveCliModelOptions {
        cli_provider: None,
        cli_model: None,
        cli_thinking: None,
        inline_api_key: false,
        model_runtime: &runtime,
    });
    assert_eq!(result.model, None);
    assert_eq!(result.warning, None);
    assert_eq!(result.error, None);
}

#[tokio::test]
async fn resolve_cli_model_treats_empty_strings_as_absent() {
    let fixture: ReferenceFixture =
        serde_json::from_value(fixture("reference_cases.json")).expect("reference fixture");
    let runtime = FakeRuntime::new(fixture.catalog.clone());
    let gpt = find_model(&fixture.catalog, "openai", "gpt-4o");

    // JS `!cliModel`：空串等同未提供，即使模型列表非空也返回空结果。
    let result = resolve_cli_model(&ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some(String::new()),
        cli_thinking: None,
        inline_api_key: false,
        model_runtime: &runtime,
    });
    assert_eq!(result.model, None);
    assert_eq!(result.error, None);

    // JS `cliProvider ? ... : undefined`：空串 provider 不构成约束，也不报 Unknown provider。
    let result = resolve_cli_model(&ResolveCliModelOptions {
        cli_provider: Some(String::new()),
        cli_model: Some("gpt-4o".to_string()),
        cli_thinking: None,
        inline_api_key: false,
        model_runtime: &runtime,
    });
    assert_eq!(result.error, None);
    assert_eq!(result.model.as_ref().map(model_ref), Some(model_ref(&gpt)));
}

// ---------------------------------------------------------------------------
// findInitialModel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initial_model_cases_match_the_reference_implementation() {
    let fixture = fixture("initial_model_cases.json");
    let saved = model_from(&fixture["savedDeepSeekModel"]);
    let local = model_from(&fixture["localDeepSeekModel"]);
    let ai_gateway = model_from(&fixture["aiGatewayModel"]);

    for case in fixture["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let available = models_from(&case["availableSnapshot"]);
        let configured: Vec<String> = case["configuredProviders"]
            .as_array()
            .expect("configuredProviders")
            .iter()
            .map(|value| value.as_str().expect("provider").to_string())
            .collect();
        let lookups = match case["getModel"].as_object() {
            Some(get_model) => vec![Model {
                provider: get_model["provider"]
                    .as_str()
                    .expect("provider")
                    .to_string(),
                id: get_model["id"].as_str().expect("id").to_string(),
                ..saved.clone()
            }],
            None => Vec::new(),
        };
        let runtime = FakeRuntime {
            models: available.clone(),
            configured: configured.into_iter().collect(),
            available,
            lookups,
        };
        let options = &case["options"];
        let result = find_initial_model(&FindInitialModelOptions {
            scoped_models: &[],
            is_continuing: options["isContinuing"].as_bool().unwrap_or(false),
            default_provider: options["defaultProvider"].as_str(),
            default_model_id: options["defaultModelId"].as_str(),
            default_thinking_level: None,
            model_thinking_levels: None,
            model_runtime: &runtime,
        })
        .await;
        let expected = &case["expected"];
        let actual_model = result
            .model
            .as_ref()
            .map(|model| (model.provider.clone(), model.id.clone()));
        let expected_model = expected["model"].as_object().map(|model| {
            (
                model["provider"].as_str().expect("provider").to_string(),
                model["id"].as_str().expect("id").to_string(),
            )
        });
        assert_eq!(actual_model, expected_model, "{name}");
        assert_eq!(
            Some(result.thinking_level),
            parse_thinking_level(expected["thinkingLevel"].as_str().expect("thinkingLevel")),
            "{name}"
        );
        assert_eq!(result.fallback_message, None, "{name}");
        let _ = (&saved, &local, &ai_gateway);
    }
}

#[tokio::test]
async fn find_initial_model_prefers_scoped_models_unless_continuing() {
    let fixture = json_fixture("initial_model_cases.json");
    let ai_gateway = model_from(&fixture["aiGatewayModel"]);
    let runtime = FakeRuntime::new(vec![ai_gateway.clone()]);
    let scoped = vec![ScopedModel {
        model: ai_gateway.clone(),
        thinking_level: Some(ModelThinkingLevel::High),
    }];
    let mut levels: IndexMap<String, ModelThinkingLevel> = IndexMap::new();
    levels.insert(
        format!("{}/{}", ai_gateway.provider, ai_gateway.id),
        ModelThinkingLevel::Low,
    );
    let result = find_initial_model(&FindInitialModelOptions {
        scoped_models: &scoped,
        is_continuing: false,
        default_provider: None,
        default_model_id: None,
        default_thinking_level: Some(ModelThinkingLevel::Max),
        model_thinking_levels: Some(&levels),
        model_runtime: &runtime,
    })
    .await;
    assert_eq!(
        result.model.map(|model| model.id),
        Some(ai_gateway.id.clone())
    );
    assert_eq!(result.thinking_level, ModelThinkingLevel::High);

    // 继续会话时跳过作用域模型，回落到第 3 步。
    let result = find_initial_model(&FindInitialModelOptions {
        scoped_models: &scoped,
        is_continuing: true,
        default_provider: None,
        default_model_id: None,
        default_thinking_level: None,
        model_thinking_levels: None,
        model_runtime: &runtime,
    })
    .await;
    assert_eq!(result.thinking_level, ModelThinkingLevel::Medium);
}

fn json_fixture(name: &str) -> Value {
    fixture(name)
}

// ---------------------------------------------------------------------------
// defaultModelPerProvider
// ---------------------------------------------------------------------------

#[test]
fn default_model_per_provider_is_verbatim_and_ordered() {
    let expected: Vec<(String, String)> =
        serde_json::from_value(fixture("default_models.json")).expect("default models fixture");
    let actual: Vec<(String, String)> = DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .map(|(provider, model)| (provider.to_string(), model.to_string()))
        .collect();
    assert_eq!(actual, expected);
    // `default_models_ordered()` 是同一份数据的稳定视图。
    assert_eq!(default_models_ordered(), expected);
    assert_eq!(actual.len(), 41);
    assert_eq!(DEFAULT_MODEL_PER_PROVIDER.get("openai"), Some(&"gpt-5.5"));
    assert_eq!(
        DEFAULT_MODEL_PER_PROVIDER.get("qwen-token-plan-individual"),
        Some(&"qwen3.8-max")
    );
    assert_eq!(
        DEFAULT_MODEL_PER_PROVIDER.get("xiaomi-token-plan-sgp"),
        Some(&"mimo-v2.6-pro")
    );
}

#[test]
fn built_in_chat_providers_have_defaults_in_their_generated_catalogs() {
    // TS: model-resolver.test.ts「built-in chat providers have defaults in their
    // generated catalogs」。默认 id 必须在对应 provider 的内置 chat 目录里，且没有
    // chat 模型的 provider 不能有默认值。
    use kiana_ai::providers::all::{get_builtin_models, get_builtin_providers};

    for provider in get_builtin_providers() {
        let chat_models = get_builtin_models(provider);
        let default_id = DEFAULT_MODEL_PER_PROVIDER.get(provider.as_str()).copied();
        if chat_models.is_empty() {
            assert_eq!(
                default_id, None,
                "{provider} has no chat models and should have no chat default"
            );
            continue;
        }
        let default_id =
            default_id.unwrap_or_else(|| panic!("{provider} has chat models but no default"));
        assert!(
            chat_models.iter().any(|model| model.id == default_id),
            "{provider} default {default_id} should exist in its generated catalog"
        );
    }
    // 反向：默认表里的每个 provider 都必须是内置 provider（否则上面的循环覆盖不到）。
    for provider in DEFAULT_MODEL_PER_PROVIDER.keys() {
        assert!(
            get_builtin_providers()
                .iter()
                .any(|known| known.as_str() == *provider),
            "{provider} is not a built-in provider"
        );
    }
}

// ---------------------------------------------------------------------------
// restoreModelFromSession（spec §2.21.14；TS 测试未直接覆盖）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restore_model_from_session_returns_the_configured_model() {
    let fixture = json_fixture("initial_model_cases.json");
    let saved = model_from(&fixture["savedDeepSeekModel"]);
    let runtime = FakeRuntime::new(vec![saved.clone()]).with_configured(&["deepseek".to_string()]);
    let result =
        restore_model_from_session("deepseek", "deepseek-v4-flash", None, false, &runtime).await;
    assert_eq!(result.model, Some(saved));
    assert_eq!(result.fallback_message, None);
}

#[tokio::test]
async fn restore_model_from_session_falls_back_to_the_current_model() {
    let fixture = json_fixture("initial_model_cases.json");
    let saved = model_from(&fixture["savedDeepSeekModel"]);
    let current = model_from(&fixture["aiGatewayModel"]);
    let runtime = FakeRuntime::new(vec![saved]);
    let result = restore_model_from_session(
        "deepseek",
        "deepseek-v4-flash",
        Some(&current),
        false,
        &runtime,
    )
    .await;
    assert_eq!(
        result.model.as_ref().map(model_ref),
        Some(model_ref(&current))
    );
    assert_eq!(
        result.fallback_message.as_deref(),
        Some(format!(
            "Could not restore model deepseek/deepseek-v4-flash (no auth configured). Using {}/{}.",
            current.provider, current.id
        ).as_str())
    );
}

#[tokio::test]
async fn restore_model_from_session_reports_a_missing_model_and_falls_back() {
    let fixture = json_fixture("initial_model_cases.json");
    let ai_gateway = model_from(&fixture["aiGatewayModel"]);
    let runtime = FakeRuntime::new(vec![ai_gateway.clone()]);
    let result = restore_model_from_session("deepseek", "gone", None, false, &runtime).await;
    assert_eq!(
        result.model.as_ref().map(model_ref),
        Some(model_ref(&ai_gateway))
    );
    assert_eq!(
        result.fallback_message.as_deref(),
        Some(
            format!(
                "Could not restore model deepseek/gone (model no longer exists). Using {}/{}.",
                ai_gateway.provider, ai_gateway.id
            )
            .as_str()
        )
    );
}

#[tokio::test]
async fn restore_model_from_session_without_any_model_returns_nothing() {
    let runtime = FakeRuntime::new(Vec::new());
    let result = restore_model_from_session("deepseek", "gone", None, false, &runtime).await;
    assert_eq!(result.model, None);
    assert_eq!(result.fallback_message, None);
}
