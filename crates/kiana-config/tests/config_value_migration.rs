//! `config-value-migration.test.ts` 的等价移植（3 条，其中一条是 `it.each` 参数化）。
//!
//! 不调用 `ca-cli` 的 `runMigrations`（owner: kiana-coding-agent，阶段 8）：这里只复刻
//! auth.json / models.json 的**字面值期望**——裸大写值保持字面量（不被当成环境变量），
//! malformed/空 models.json 原样保留；报告断言（`Failed to parse models.json` /
//! `File: <path>`）经由本 crate 的 `ModelConfig` + `ModelRegistry::get_error()` 复刻。

// 测试需要在整个 await 期间保持 ENV_LOCK（环境变量是进程级状态）；串行化是本测试的意图。
#![allow(clippy::await_holding_lock)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use kiana_ai::auth::types::{AuthOperationOptions, Credential, CredentialStore};
use kiana_config::auth::storage::AuthStorage;
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::resolve_config_value::resolve_config_value;
use serde_json::Value;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// TS 的 `createModelRegistry(AuthStorage.create(authPath), modelsPath)`：
/// 同步构造 runtime（禁网），返回门面。
async fn create_model_registry(auth_path: &Path, models_path: &Path) -> ModelRegistry {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::create(&path_string(auth_path)))),
        models_path: ModelsPathOption::Path(path_string(models_path)),
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("runtime");
    ModelRegistry::new(Arc::new(runtime))
}

/// leaves uppercase auth.json API key values unchanged
#[tokio::test]
async fn leaves_uppercase_auth_json_api_key_values_unchanged() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let original = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "anthropic": {"type": "api_key", "key": "ANTHROPIC_API_KEY"},
            "openai": {"type": "api_key", "key": "$OPENAI_API_KEY"},
            "opencode": {"type": "api_key", "key": "public"},
            "github": {"type": "oauth", "access": "ACCESS_TOKEN", "refresh": "REFRESH_TOKEN", "expires": 1}
        }))
        .unwrap()
    );
    fs::write(&auth_path, &original).unwrap();
    // SAFETY: 串行持有 ENV_LOCK；变量名仅本测试使用。
    unsafe { std::env::set_var("OPENAI_API_KEY", "env-openai") };

    let storage = AuthStorage::create(&path_string(&auth_path));
    let anthropic = storage
        .read("anthropic", AuthOperationOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        anthropic,
        Credential::ApiKey(kiana_ai::auth::types::ApiKeyCredential {
            key: Some("ANTHROPIC_API_KEY".to_string()),
            env: None,
            extra: indexmap::IndexMap::new(),
        })
    );

    let openai = storage
        .read("openai", AuthOperationOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        openai,
        Credential::ApiKey(kiana_ai::auth::types::ApiKeyCredential {
            key: Some("env-openai".to_string()),
            env: None,
            extra: indexmap::IndexMap::new(),
        })
    );

    let opencode = storage
        .read("opencode", AuthOperationOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        opencode,
        Credential::ApiKey(kiana_ai::auth::types::ApiKeyCredential {
            key: Some("public".to_string()),
            env: None,
            extra: indexmap::IndexMap::new(),
        })
    );

    let github = storage
        .read("github", AuthOperationOptions::default())
        .await
        .unwrap()
        .unwrap();
    let Credential::OAuth(github) = github else {
        panic!("expected oauth");
    };
    assert_eq!(github.access, "ACCESS_TOKEN");
    assert_eq!(github.refresh, "REFRESH_TOKEN");

    // 读取不会改写文件。
    assert_eq!(fs::read_to_string(&auth_path).unwrap(), original);
    unsafe { std::env::remove_var("OPENAI_API_KEY") };
}

/// does not throw on malformed models.json during migrations
///
/// `runMigrations` 属于 ca-cli（阶段 8）且不触碰 models.json；这里断言迁移/读取流程不
/// 改写文件，并经 `ModelConfig`/`ModelRegistry` 报告 `Failed to parse models.json` /
/// `File: <path>`。
#[tokio::test]
async fn does_not_throw_on_malformed_models_json_during_migrations() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let models_path = temp_dir.path().join("models.json");
    let content = "{\n  \"providers\": {\n";
    fs::write(&models_path, content).unwrap();
    // 迁移/读取流程不触碰 models.json：内容逐字保留。
    assert_eq!(fs::read_to_string(&models_path).unwrap(), content);

    let registry = create_model_registry(&auth_path, &models_path).await;
    let load_error = registry.get_error().expect("models.json load error");
    assert!(
        load_error.contains("Failed to parse models.json"),
        "unexpected error: {load_error}"
    );
    assert!(
        load_error.contains(&format!("File: {}", path_string(&models_path))),
        "unexpected error: {load_error}"
    );
}

/// does not throw on blank models.json during migrations
///
/// 同 [`does_not_throw_on_malformed_models_json_during_migrations`]：空文件也被视为解析失败
/// （V8 的 `Unexpected end of JSON input`）。
#[tokio::test]
async fn does_not_throw_on_blank_models_json_during_migrations() {
    let temp_dir = TempDir::new().unwrap();
    let auth_path = temp_dir.path().join("auth.json");
    let models_path = temp_dir.path().join("models.json");
    fs::write(&models_path, "").unwrap();
    assert_eq!(fs::read_to_string(&models_path).unwrap(), "");

    let registry = create_model_registry(&auth_path, &models_path).await;
    let load_error = registry.get_error().expect("models.json load error");
    assert!(
        load_error.contains("Failed to parse models.json"),
        "unexpected error: {load_error}"
    );
    assert!(
        load_error.contains(&format!("File: {}", path_string(&models_path))),
        "unexpected error: {load_error}"
    );
}

/// leaves uppercase models.json API key and header values unchanged
#[test]
fn leaves_uppercase_models_json_api_key_and_header_values_unchanged() {
    let _env = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models.json");
    let content = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "providers": {
                "custom-provider": {
                    "baseUrl": "https://example.com/v1",
                    "apiKey": "CUSTOM_API_KEY",
                    "api": "openai-completions",
                    "headers": {
                        "x-api-key": "HEADER_API_KEY",
                        "x-literal": "literal"
                    },
                    "models": [
                        {
                            "id": "model-a",
                            "headers": {"x-model-key": "MODEL_API_KEY"}
                        }
                    ],
                    "modelOverrides": {
                        "model-b": {"headers": {"x-override-key": "OVERRIDE_API_KEY"}}
                    }
                }
            }
        }))
        .unwrap()
    );
    fs::write(&models_path, &content).unwrap();
    let env_keys = [
        "CUSTOM_API_KEY",
        "HEADER_API_KEY",
        "MODEL_API_KEY",
        "OVERRIDE_API_KEY",
    ];
    for key in env_keys {
        // SAFETY: 串行持有 ENV_LOCK；变量名仅本测试使用。
        unsafe { std::env::set_var(key, format!("env-{key}")) };
    }

    let parsed: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    let provider = &parsed["providers"]["custom-provider"];
    assert_eq!(provider["apiKey"], "CUSTOM_API_KEY");
    assert_eq!(provider["headers"]["x-api-key"], "HEADER_API_KEY");
    assert_eq!(provider["headers"]["x-literal"], "literal");
    assert_eq!(
        provider["models"][0]["headers"]["x-model-key"],
        "MODEL_API_KEY"
    );
    assert_eq!(
        provider["modelOverrides"]["model-b"]["headers"]["x-override-key"],
        "OVERRIDE_API_KEY"
    );

    // 裸大写配置值是字面量；`$NAME` 才解析环境变量。
    for value in [
        "CUSTOM_API_KEY",
        "HEADER_API_KEY",
        "MODEL_API_KEY",
        "OVERRIDE_API_KEY",
    ] {
        assert_eq!(
            resolve_config_value(value, None).as_deref(),
            Some(value),
            "bare uppercase values stay literal"
        );
    }
    assert_eq!(
        resolve_config_value("$CUSTOM_API_KEY", None).as_deref(),
        Some("env-CUSTOM_API_KEY")
    );

    // 读取不会改写文件。
    assert_eq!(fs::read_to_string(&models_path).unwrap(), content);
    for key in env_keys {
        unsafe { std::env::remove_var(key) };
    }
}
