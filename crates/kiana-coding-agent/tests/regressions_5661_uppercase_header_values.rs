//! `test/suite/regressions/5661-uppercase-header-values.test.ts` 的移植。
//!
//! TS 用 session harness 提供临时 agent 目录；这里直接用 `tempfile` + 环境变量守卫。
//! 迁移后的 `models.json` 必须逐字保持不变；随后用 `ModelRuntime` 断言大写 env-var
//! 字面量没有被展开。

use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_ai::models::store::InMemoryModelsStore;
use kiana_coding_agent::migrations::run_migrations;
use kiana_config::auth::storage::AuthStorage;
use kiana_config::models::runtime::{
    CreateModelRuntimeOptions, ModelRuntime, ModelRuntimeAuthOverrides, ModelsPathOption,
};
use serde_json::Value;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(agent_dir: &Path, keys: &[&'static str]) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved: Vec<(&'static str, Option<OsString>)> = keys
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        // SAFETY: ENV_LOCK 串行化本二进制内所有环境变量修改。
        unsafe {
            for key in keys {
                std::env::set_var(key, format!("env-{key}"));
            }
            std::env::set_var("KIANA_CODING_AGENT_DIR", agent_dir);
        }
        Self { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: 仍持有 ENV_LOCK。
        unsafe {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            std::env::remove_var("KIANA_CODING_AGENT_DIR");
        }
    }
}

async fn create_registry(agent_dir: &Path, models_path: &Path) -> ModelRuntime {
    ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::new(AuthStorage::create(
            &agent_dir.join("auth.json").to_string_lossy(),
        ))),
        models_path: ModelsPathOption::Path(models_path.to_string_lossy().into_owned()),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("model runtime")
}

#[tokio::test]
async fn keeps_uppercase_header_strings_as_literals_during_startup_migrations() {
    let agent_dir = TempDir::new().expect("agent dir");
    let models_path = agent_dir.path().join("models.json");
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "providers": {
                "my-provider": {
                    "baseUrl": "https://example.com/v1",
                    "apiKey": "CUSTOM_API_KEY",
                    "api": "openai-completions",
                    "headers": { "Authorization": "BEARER" },
                    "models": [{ "id": "my-model" }],
                },
            },
        }))
        .expect("json")
    );
    std::fs::write(&models_path, &contents).expect("write models");

    // TS 在整个断言期间保持同名 env 变量存在——这正是 #5661 的回归点：即使 env 有值，
    // 配置里的 `CUSTOM_API_KEY` / `BEARER` 也必须是字面量。守卫必须活到注册表断言结束，
    // 提前 drop 会让“把字面量当 env 名展开”的实现也通过。
    let _env = EnvGuard::set(agent_dir.path(), &["CUSTOM_API_KEY", "BEARER"]);
    run_migrations(agent_dir.path());

    let migrated: Value =
        serde_json::from_str(&std::fs::read_to_string(&models_path).expect("read")).expect("parse");
    assert_eq!(
        migrated.pointer("/providers/my-provider/apiKey"),
        Some(&Value::String("CUSTOM_API_KEY".to_string()))
    );
    assert_eq!(
        migrated.pointer("/providers/my-provider/headers/Authorization"),
        Some(&Value::String("BEARER".to_string()))
    );

    let registry = create_registry(agent_dir.path(), &models_path).await;
    let model = registry
        .get_model("my-provider", "my-model")
        .expect("my-model");
    let auth = registry
        .get_auth_for_model(&model, ModelRuntimeAuthOverrides::default())
        .await
        .expect("auth")
        .expect("model auth");
    assert_eq!(auth.auth.api_key.as_deref(), Some("CUSTOM_API_KEY"));
    let headers = auth.auth.headers.expect("headers");
    assert_eq!(
        headers.get("Authorization").cloned().flatten().as_deref(),
        Some("BEARER")
    );
}
