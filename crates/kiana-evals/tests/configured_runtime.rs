//! `packages/evals/test/configured-runtime.test.ts` 的移植。
//!
//! 使用 `tempfile` 创建 agentDir（models.json / auth.json / models-store.json 都落在其中），
//! 并只与 127.0.0.1 上的 acme fixture 通信，不使用真实 key。

#[path = "support/acme_server.rs"]
mod acme_server;
#[path = "support/configured_runtime.rs"]
mod configured_runtime;

use std::path::Path;
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, MutexGuard};

use acme_server::{
    AcmeServer, OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, OPENAI_PROBE_RESPONSE, OPENAI_PROVIDER_ID,
    ServerMode,
};
use configured_runtime::{
    AddedModelOutput, AddedModelResult, AddedModelSuccess, ErrorResult, ModelCostFields,
    ModelFields, ProviderResponseFields, ProviderRuntimeOutput, ProviderRuntimeResult,
    ProviderScenario, ProviderSuccess, inspect_added_model, inspect_provider,
    load_configured_model_runtime,
};
use kiana_ai::types::{
    Context, Message, ProviderEnv, SimpleStreamOptions, StreamOptions, UserContent, UserMessage,
    UserRole,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// 进程环境是全局状态：`ModelRuntime::create` / `refresh` 会读 `KIANA_OFFLINE` 等变量，
/// 而 [`isolated_agent_dir`] 会写 `KIANA_CODING_AGENT_DIR`。所有用例都持有这把锁，
/// 保证 `set_var` 与并发 env 读取不重叠（Rust 2024 的 unsafe 前提）。
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

/// `ModelRuntime` 的默认 auth 路径来自 `KIANA_CODING_AGENT_DIR`；把它指向临时目录，避免触碰
/// 真实的 `~/.kiana`（ARCHITECTURE §4.8.8）。返回值只用于保持 tempdir 存活。
///
/// 调用方必须持有 [`ENV_LOCK`]。
fn isolated_agent_dir() -> &'static Path {
    static DIRECTORY: OnceLock<TempDir> = OnceLock::new();
    let directory = DIRECTORY.get_or_init(|| {
        let directory = tempfile::tempdir().expect("isolated agent dir");
        // SAFETY: 测试持有 ENV_LOCK，独占进程环境；此后整个二进制在锁内读写该值。
        unsafe { std::env::set_var("KIANA_CODING_AGENT_DIR", directory.path()) };
        directory
    });
    directory.path()
}

fn expected_model() -> ModelFields {
    ModelFields {
        id: OPENAI_MODEL_ID.to_string(),
        name: "Acme Chat".to_string(),
        provider: OPENAI_PROVIDER_ID.to_string(),
        reasoning: false,
        input: vec!["text".to_string()],
        cost: ModelCostFields {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 32768,
        max_tokens: 4096,
    }
}

fn acme_models_json(base_url: &str) -> Value {
    json!({
        "providers": {
            "acme": {
                "baseUrl": base_url,
                "api": "openai-completions",
                "apiKey": "$ACME_API_KEY",
                "models": [{
                    "id": OPENAI_MODEL_ID,
                    "name": "Acme Chat",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 32768,
                    "maxTokens": 4096,
                }],
            },
        },
    })
}

fn agent_dir_with(models_json: &Value) -> TempDir {
    let directory = tempfile::tempdir().expect("temp agent dir");
    std::fs::write(
        directory.path().join("models.json"),
        format!(
            "{}\n",
            serde_json::to_string(models_json).expect("serialize models.json")
        ),
    )
    .expect("write models.json");
    directory
}

fn agent_dir_with_raw(contents: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("temp agent dir");
    std::fs::write(directory.path().join("models.json"), contents).expect("write models.json");
    directory
}

fn probe(server: &Arc<AcmeServer>, content: &str, api_key: &str) -> ProviderScenario {
    let server = Arc::clone(server);
    let content = content.to_string();
    let api_key = api_key.to_string();
    ProviderScenario {
        provider_id: OPENAI_PROVIDER_ID.to_string(),
        model_id: OPENAI_MODEL_ID.to_string(),
        create_context: Box::new(move || Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text(content.clone()),
                timestamp: 0,
            })],
            ..Context::default()
        }),
        options: Some(kiana_ai::models::ModelsSimpleStreamOptions {
            base: SimpleStreamOptions {
                base: StreamOptions {
                    env: Some(
                        [("ACME_API_KEY".to_string(), api_key)]
                            .into_iter()
                            .collect::<ProviderEnv>(),
                    ),
                    max_tokens: Some(32),
                    ..StreamOptions::default()
                },
                ..SimpleStreamOptions::default()
            },
            ..Default::default()
        }),
        valid_request_received: Box::new(move || server.valid_request_received()),
    }
}

async fn start_server() -> Arc<AcmeServer> {
    let server = Arc::new(AcmeServer::new(ServerMode::Openai));
    server.start().await.expect("fixture server starts");
    server
}

// ---------------------------------------------------------------------------
// inspectProvider
// ---------------------------------------------------------------------------

/// TS: `inspectProvider > probes a models.json provider through ModelRuntime`
#[tokio::test]
async fn probes_a_models_json_provider_through_model_runtime() {
    let _env = env_lock().await;
    let server = start_server().await;
    let directory = agent_dir_with(&acme_models_json(&server.base_url()));
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_provider(
        &runtime,
        probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"),
    )
    .await
    .expect("inspectProvider");
    assert_eq!(
        output,
        ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Success(ProviderSuccess {
                valid_request_received: true,
                model: expected_model(),
                response: ProviderResponseFields {
                    text: OPENAI_PROBE_RESPONSE.to_string(),
                    stop_reason: "stop".to_string(),
                    input_tokens: 3,
                    output_tokens: 2,
                },
            }),
        }
    );
    server.stop().await.expect("fixture server stops");
}

/// TS: `inspectProvider > keeps a non-probe completion distinct from a valid probe`
#[tokio::test]
async fn keeps_a_non_probe_completion_distinct_from_a_valid_probe() {
    let _env = env_lock().await;
    let server = start_server().await;
    let directory = agent_dir_with(&acme_models_json(&server.base_url()));
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_provider(&runtime, probe(&server, "hello", "resolved-acme-key"))
        .await
        .expect("inspectProvider");
    assert_eq!(
        output,
        ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Success(ProviderSuccess {
                valid_request_received: false,
                model: expected_model(),
                response: ProviderResponseFields {
                    text: OPENAI_PROBE_RESPONSE.to_string(),
                    stop_reason: "stop".to_string(),
                    input_tokens: 3,
                    output_tokens: 2,
                },
            }),
        }
    );
    server.stop().await.expect("fixture server stops");
}

/// TS: `inspectProvider > returns a structured error when the configured model is missing`
#[tokio::test]
async fn returns_a_structured_error_when_the_configured_model_is_missing() {
    let _env = env_lock().await;
    let server = start_server().await;
    let directory = agent_dir_with(&json!({ "providers": {} }));
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_provider(
        &runtime,
        probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"),
    )
    .await
    .expect("inspectProvider");
    assert_eq!(
        output,
        ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Error(ErrorResult {
                error: format!(
                    "Model {OPENAI_PROVIDER_ID}/{OPENAI_MODEL_ID} is unavailable after reload."
                ),
            }),
        }
    );
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `inspectProvider > returns a structured error when models.json cannot be parsed`
#[tokio::test]
async fn returns_a_structured_error_when_models_json_cannot_be_parsed() {
    let _env = env_lock().await;
    let server = start_server().await;
    let directory = agent_dir_with_raw("{");
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_provider(
        &runtime,
        probe(&server, OPENAI_PROBE_PROMPT, "resolved-acme-key"),
    )
    .await
    .expect("inspectProvider");
    match output.result {
        ProviderRuntimeResult::Error(ErrorResult { error }) => {
            assert!(
                error.contains("Failed to parse models.json"),
                "unexpected error: {error}"
            );
        }
        other => panic!("expected an error result, got {other:?}"),
    }
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `inspectProvider > does not treat an unauthorized completion as a valid probe`
#[tokio::test]
async fn does_not_treat_an_unauthorized_completion_as_a_valid_probe() {
    let _env = env_lock().await;
    let server = start_server().await;
    let directory = agent_dir_with(&acme_models_json(&server.base_url()));
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_provider(&runtime, probe(&server, OPENAI_PROBE_PROMPT, "wrong-key"))
        .await
        .expect("inspectProvider");
    assert_eq!(
        output,
        ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Success(ProviderSuccess {
                valid_request_received: false,
                model: expected_model(),
                response: ProviderResponseFields {
                    text: String::new(),
                    stop_reason: "error".to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                },
            }),
        }
    );
    server.stop().await.expect("fixture server stops");
}

// ---------------------------------------------------------------------------
// inspectAddedModel
// ---------------------------------------------------------------------------

const ADDED_MODEL_ID: &str = "fixture-chat";

fn added_model() -> ModelFields {
    ModelFields {
        id: ADDED_MODEL_ID.to_string(),
        name: "Fixture Chat".to_string(),
        provider: "openai".to_string(),
        reasoning: true,
        input: vec!["text".to_string()],
        cost: ModelCostFields {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        context_window: 32768,
        max_tokens: 4096,
    }
}

fn added_models_json() -> Value {
    json!({
        "providers": {
            "openai": {
                "models": [{
                    "id": ADDED_MODEL_ID,
                    "name": "Fixture Chat",
                    "reasoning": true,
                    "input": ["text"],
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                    "contextWindow": 32768,
                    "maxTokens": 4096,
                }],
            },
        },
    })
}

/// TS: `inspectAddedModel > loads an added model without dropping built-in models`
#[tokio::test]
async fn loads_an_added_model_without_dropping_builtin_models() {
    let _env = env_lock().await;
    isolated_agent_dir();
    let directory = agent_dir_with(&added_models_json());
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_added_model(&runtime, "openai", ADDED_MODEL_ID)
        .await
        .expect("inspectAddedModel");
    assert_eq!(
        output,
        AddedModelOutput {
            result: AddedModelResult::Success(AddedModelSuccess {
                model: added_model(),
                existing_models_preserved: true,
            }),
        }
    );
}

/// TS: `inspectAddedModel > returns a structured error when the added model is missing`
#[tokio::test]
async fn returns_a_structured_error_when_the_added_model_is_missing() {
    let _env = env_lock().await;
    isolated_agent_dir();
    let directory = agent_dir_with(&json!({ "providers": {} }));
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_added_model(&runtime, "openai", ADDED_MODEL_ID)
        .await
        .expect("inspectAddedModel");
    assert_eq!(
        output,
        AddedModelOutput {
            result: AddedModelResult::Error(ErrorResult {
                error: format!("Model openai/{ADDED_MODEL_ID} is unavailable after reload."),
            }),
        }
    );
}

/// TS: `inspectAddedModel > returns a structured error when models.json cannot be parsed`
#[tokio::test]
async fn returns_a_structured_error_when_models_json_cannot_be_parsed_for_added_models() {
    let _env = env_lock().await;
    isolated_agent_dir();
    let directory = agent_dir_with_raw("{");
    let runtime = load_configured_model_runtime(&directory.path().to_string_lossy())
        .await
        .expect("configured runtime");
    let output = inspect_added_model(&runtime, "openai", ADDED_MODEL_ID)
        .await
        .expect("inspectAddedModel");
    match output.result {
        AddedModelResult::Error(ErrorResult { error }) => {
            assert!(
                error.contains("Failed to parse models.json"),
                "unexpected error: {error}"
            );
        }
        other => panic!("expected an error result, got {other:?}"),
    }
}
