//! `test/config-value-migration.test.ts` 的逐条移植。
//!
//! 迁移部分断言完整的迁移流程；`createModelRegistry` 的半段用 `kiana-config` 的
//! `ModelRuntime`（等价的公开面）断言配置值解析结果。

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
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set_agent_dir(path: &Path) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("KIANA_CODING_AGENT_DIR");
        // SAFETY: ENV_LOCK 串行化本二进制内所有环境变量修改。
        unsafe {
            std::env::set_var("KIANA_CODING_AGENT_DIR", path);
        }
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: ENV_LOCK 仍被持有。
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
    }
}

fn create_agent_dir() -> TempDir {
    TempDir::new().expect("agent dir")
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

#[test]
fn leaves_uppercase_auth_json_api_key_values_unchanged() {
    let agent_dir = create_agent_dir();
    let auth_path = agent_dir.path().join("auth.json");
    let original = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "anthropic": { "type": "api_key", "key": "ANTHROPIC_API_KEY" },
            "openai": { "type": "api_key", "key": "$OPENAI_API_KEY" },
            "opencode": { "type": "api_key", "key": "public" },
            "github": { "type": "oauth", "access": "ACCESS_TOKEN", "refresh": "REFRESH_TOKEN", "expires": 1 },
        }))
        .expect("json")
    );
    std::fs::write(&auth_path, &original).expect("write auth");

    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    assert_eq!(std::fs::read_to_string(&auth_path).expect("read"), original);
}

async fn assert_models_load_error(contents: &str) {
    let agent_dir = create_agent_dir();
    let models_path = agent_dir.path().join("models.json");
    std::fs::write(&models_path, contents).expect("write models");

    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    assert_eq!(
        std::fs::read_to_string(&models_path).expect("read"),
        contents
    );
    let registry = create_registry(agent_dir.path(), &models_path).await;
    let load_error = registry.get_error().expect("load error");
    assert!(
        load_error.contains("Failed to parse models.json"),
        "{load_error}"
    );
    assert!(
        load_error.contains(&format!("File: {}", models_path.to_string_lossy())),
        "{load_error}"
    );
}

#[tokio::test]
async fn does_not_throw_on_malformed_models_json_during_migrations() {
    assert_models_load_error("{\n  \"providers\": {\n").await;
}

#[tokio::test]
async fn does_not_throw_on_blank_models_json_during_migrations() {
    assert_models_load_error("").await;
}

#[tokio::test]
// 环境变量必须在注册表断言（含 await）期间保持设置，才能复现 TS 的 #5661 场景。
#[allow(clippy::await_holding_lock)]
async fn leaves_uppercase_models_json_api_key_and_header_values_unchanged() {
    let agent_dir = create_agent_dir();
    let models_path = agent_dir.path().join("models.json");
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "providers": {
                "custom-provider": {
                    "baseUrl": "https://example.com/v1",
                    "apiKey": "CUSTOM_API_KEY",
                    "api": "openai-completions",
                    "headers": {
                        "x-api-key": "HEADER_API_KEY",
                        "x-literal": "literal",
                    },
                    "models": [
                        { "id": "model-a", "headers": { "x-model-key": "MODEL_API_KEY" } },
                    ],
                    "modelOverrides": {
                        "model-b": { "headers": { "x-override-key": "OVERRIDE_API_KEY" } },
                    },
                },
            },
        }))
        .expect("json")
    );
    std::fs::write(&models_path, &contents).expect("write models");

    let env_keys = [
        "CUSTOM_API_KEY",
        "HEADER_API_KEY",
        "MODEL_API_KEY",
        "OVERRIDE_API_KEY",
    ];
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let saved: Vec<(&str, Option<OsString>)> = env_keys
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    // SAFETY: ENV_LOCK 串行化本二进制内所有环境变量修改。
    unsafe {
        for key in env_keys {
            std::env::set_var(key, format!("env-{key}"));
        }
        std::env::set_var("KIANA_CODING_AGENT_DIR", agent_dir.path());
    }

    run_migrations(agent_dir.path());

    assert_eq!(
        std::fs::read_to_string(&models_path).expect("read"),
        contents
    );

    let registry = create_registry(agent_dir.path(), &models_path).await;
    let model = registry
        .get_model("custom-provider", "model-a")
        .expect("model-a");
    let provider_auth = registry
        .get_auth_for_provider("custom-provider", ModelRuntimeAuthOverrides::default())
        .await
        .expect("auth")
        .expect("provider auth");
    assert_eq!(
        provider_auth.auth.api_key.as_deref(),
        Some("CUSTOM_API_KEY")
    );

    let model_auth = registry
        .get_auth_for_model(&model, ModelRuntimeAuthOverrides::default())
        .await
        .expect("auth")
        .expect("model auth");
    assert_eq!(model_auth.auth.api_key.as_deref(), Some("CUSTOM_API_KEY"));
    let headers = model_auth.auth.headers.expect("headers");
    assert_eq!(
        headers.get("x-api-key").cloned().flatten().as_deref(),
        Some("HEADER_API_KEY")
    );
    assert_eq!(
        headers.get("x-literal").cloned().flatten().as_deref(),
        Some("literal")
    );
    assert_eq!(
        headers.get("x-model-key").cloned().flatten().as_deref(),
        Some("MODEL_API_KEY")
    );
    // `modelOverrides` 的 header 合并属于 kiana-config 的 model-registry 语义（TS 的
    // getApiKeyAndHeaders 会把 model-b 的 override 也算进 model-a）；这里只锁定迁移
    // 本身没有改写该字面量。
    let migrated: Value =
        serde_json::from_str(&std::fs::read_to_string(&models_path).expect("read")).expect("parse");
    assert_eq!(
        migrated
            .pointer("/providers/custom-provider/modelOverrides/model-b/headers/x-override-key"),
        Some(&Value::String("OVERRIDE_API_KEY".to_string()))
    );

    // #5661：同名 env 变量在整个注册表断言期间保持设置，否则一个会把
    // 字面量当 env 名展开的实现也会通过。
    // SAFETY: 仍持有 ENV_LOCK。
    unsafe {
        for (key, value) in &saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        std::env::remove_var("KIANA_CODING_AGENT_DIR");
    }
    drop(_lock);
}

// ---------------------------------------------------------------------------
// 补充：migrations.ts 的直接行为（TS 无独立测试文件，覆盖 spec §4.3/§4.4）
// ---------------------------------------------------------------------------

#[test]
fn migrates_oauth_json_and_settings_api_keys_to_auth_json() {
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        r#"{"github":{"access":"A","refresh":"R","expires":1},"openai":{"access":"OA","refresh":"OR","expires":2}}"#,
    )
    .expect("oauth");
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"theme":"dark","apiKeys":{"openai":"sk-openai","anthropic":"sk-ant","bad":42}}"#,
    )
    .expect("settings");

    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path())
    };

    assert_eq!(
        result.migrated_auth_providers,
        vec![
            "github".to_string(),
            "openai".to_string(),
            "anthropic".to_string()
        ]
    );

    let auth_path = agent_dir.path().join("auth.json");
    let auth_text = std::fs::read_to_string(&auth_path).expect("auth");
    assert_eq!(
        auth_text,
        "{\n  \"github\": {\n    \"type\": \"oauth\",\n    \"access\": \"A\",\n    \"refresh\": \"R\",\n    \"expires\": 1\n  },\n  \"openai\": {\n    \"type\": \"oauth\",\n    \"access\": \"OA\",\n    \"refresh\": \"OR\",\n    \"expires\": 2\n  },\n  \"anthropic\": {\n    \"type\": \"api_key\",\n    \"key\": \"sk-ant\"\n  }\n}"
    );
    assert!(!agent_dir.path().join("oauth.json").exists());
    assert!(agent_dir.path().join("oauth.json.migrated").exists());

    let settings_text =
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings");
    assert_eq!(settings_text, "{\n  \"theme\": \"dark\"\n}");
    assert!(!settings_text.ends_with('\n'));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&auth_path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn migrates_settings_with_out_of_range_numbers_like_json_parse() {
    // TS 的 JSON.parse("1e400") 得到 Infinity，JSON.stringify 写回 null；整份
    // settings.json 仍然被重写、apiKeys 仍然迁移（serde_json 会拒绝该文件并静默跳过）。
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"apiKeys":{"p":"k"},"k":1e400}"#,
    )
    .expect("settings");

    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path())
    };
    assert_eq!(result.migrated_auth_providers, vec!["p".to_string()]);
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"p\": {\n    \"type\": \"api_key\",\n    \"key\": \"k\"\n  }\n}"
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings"),
        "{\n  \"k\": null\n}"
    );
}

#[test]
fn migrates_oauth_credentials_with_out_of_range_expiry_like_json_parse() {
    // oauth.json 里 `expires: 1e400` 在 TS 里是 Infinity，写出时变 null。
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        r#"{"p":{"access":"A","expires":1e400}}"#,
    )
    .expect("oauth");

    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path())
    };
    assert_eq!(result.migrated_auth_providers, vec!["p".to_string()]);
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"p\": {\n    \"type\": \"oauth\",\n    \"access\": \"A\",\n    \"expires\": null\n  }\n}"
    );
    assert!(agent_dir.path().join("oauth.json.migrated").exists());
}

#[test]
fn accepts_lone_surrogate_escapes_during_migration() {
    // 孤立代理项转义在 JSON 里合法，解析时应接受并完成迁移。Rust 的 String 无法表示孤立
    // 代理项，kiana-json 按 §8 的被迫偏差把它替换为 U+FFFD（重写出的字节与原转义不同），但
    // 迁移本身必须继续执行，而不是静默跳过整份文件。
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"apiKeys":{"p":"k"},"note":"\ud800"}"#,
    )
    .expect("settings");

    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path())
    };
    assert_eq!(result.migrated_auth_providers, vec!["p".to_string()]);
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"p\": {\n    \"type\": \"api_key\",\n    \"key\": \"k\"\n  }\n}"
    );
    let settings =
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings");
    assert!(!settings.contains("apiKeys"));
    assert!(settings.contains('\u{FFFD}'), "{settings}");
}

#[test]
fn does_not_write_auth_json_when_auth_exists_or_migration_is_empty() {
    // auth.json 已存在 → 不迁移、不改写 settings.json 的 apiKeys。
    let agent_dir = create_agent_dir();
    std::fs::write(agent_dir.path().join("auth.json"), "{\"keep\":true}").expect("auth");
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        "{\"openai\":{\"access\":\"A\"}}",
    )
    .expect("oauth");
    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path())
    };
    assert!(result.migrated_auth_providers.is_empty());
    assert!(agent_dir.path().join("oauth.json").exists());
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\"keep\":true}"
    );

    // 只有空 apiKeys 对象：settings.json 被重写（删除 apiKeys），但不创建 auth.json。
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        "{\"apiKeys\":{},\"theme\":\"dark\"}",
    )
    .expect("settings");
    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }
    assert!(!agent_dir.path().join("auth.json").exists());
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings"),
        "{\n  \"theme\": \"dark\"\n}"
    );
}

#[test]
fn migrates_top_level_sessions_and_skips_empty_first_lines() {
    let agent_dir = create_agent_dir();
    let session_dir = agent_dir
        .path()
        .join("sessions")
        .join("--workspace-project--");
    std::fs::create_dir_all(&session_dir).expect("session dir");

    std::fs::write(
        agent_dir.path().join("top.jsonl"),
        "{\"type\":\"session\",\"cwd\":\"/workspace/project\"}\n{\"type\":\"message\"}\n",
    )
    .expect("top");
    std::fs::write(
        agent_dir.path().join("empty.jsonl"),
        "\n{\"type\":\"session\",\"cwd\":\"/workspace/project\"}\n",
    )
    .expect("empty");
    std::fs::write(
        agent_dir.path().join("dup.jsonl"),
        "{\"type\":\"session\",\"cwd\":\"/workspace/project\"}\n",
    )
    .expect("dup");
    std::fs::write(session_dir.join("dup.jsonl"), "existing").expect("existing");

    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    let migrated = std::fs::read_to_string(session_dir.join("top.jsonl")).expect("migrated");
    assert_eq!(
        migrated,
        "{\"type\":\"session\",\"cwd\":\"/workspace/project\"}\n{\"type\":\"message\"}\n"
    );
    assert!(!agent_dir.path().join("top.jsonl").exists());
    assert!(agent_dir.path().join("empty.jsonl").exists());
    assert!(agent_dir.path().join("dup.jsonl").exists());
    assert_eq!(
        std::fs::read_to_string(session_dir.join("dup.jsonl")).expect("dup target"),
        "existing"
    );
}

#[test]
fn migrates_managed_binaries_from_tools_to_bin() {
    let agent_dir = create_agent_dir();
    let tools_dir = agent_dir.path().join("tools");
    std::fs::create_dir_all(&tools_dir).expect("tools");
    std::fs::write(tools_dir.join("fd"), "fd-binary").expect("fd");
    std::fs::write(tools_dir.join("rg"), "rg-binary").expect("rg");

    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("bin").join("fd")).expect("fd"),
        "fd-binary"
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("bin").join("rg")).expect("rg"),
        "rg-binary"
    );
    assert!(!tools_dir.join("fd").exists());
    assert!(!tools_dir.join("rg").exists());
}

#[test]
fn migrates_commands_to_prompts_and_collects_deprecation_warnings() {
    let agent_dir = create_agent_dir();
    let project_dir = create_agent_dir();
    std::fs::create_dir_all(agent_dir.path().join("commands")).expect("commands");
    std::fs::write(agent_dir.path().join("commands").join("hello.md"), "hello").expect("hello");
    std::fs::create_dir_all(agent_dir.path().join("hooks")).expect("hooks");
    std::fs::create_dir_all(project_dir.path().join(".kiana").join("tools")).expect("tools");
    std::fs::write(
        project_dir
            .path()
            .join(".kiana")
            .join("tools")
            .join("custom"),
        "x",
    )
    .expect("custom");

    let result = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(project_dir.path())
    };

    assert!(!agent_dir.path().join("commands").exists());
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("prompts").join("hello.md")).expect("hello"),
        "hello"
    );
    assert_eq!(
        result.deprecation_warnings,
        vec![
            "Global hooks/ directory found. Hooks have been renamed to extensions.".to_string(),
            "Project tools/ directory contains custom tools. Custom tools have been merged into extensions.".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// 回归：与 TS 参考实现的差分（reviewer findings 1/4/5/6/7/8/9）
// ---------------------------------------------------------------------------

/// 回归 §4.3 的 `JSON.stringify(settings, null, 2)` 字节一致性：JS `delete` 保持
/// 其余键的相对顺序，serde_json 的 `Map::remove`（preserve_order 下等同于 swap_remove）
/// 会把最后一个键挪到被删键的位置。
#[test]
fn settings_rewrite_preserves_key_order_like_a_js_delete() {
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"theme":"x","apiKeys":{"p":"k"},"z":1,"y":2}"#,
    )
    .expect("settings");

    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };

    assert_eq!(providers, vec!["p".to_string()]);
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings"),
        "{\n  \"theme\": \"x\",\n  \"z\": 1,\n  \"y\": 2\n}"
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"p\": {\n    \"type\": \"api_key\",\n    \"key\": \"k\"\n  }\n}"
    );
}

/// JS 枚举顺序：整数索引键按数值升序排在字符串键之前（`Object.entries` /
/// `JSON.stringify`），数组展开的索引属性也排在最前。
#[test]
fn migration_json_writers_follow_js_integer_key_order() {
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        r#"{"10":{"t":1},"2":{"t":2},"a":{"t":3}}"#,
    )
    .expect("oauth");

    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };

    assert_eq!(
        providers,
        vec!["2".to_string(), "10".to_string(), "a".to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"2\": {\n    \"type\": \"oauth\",\n    \"t\": 2\n  },\n  \"10\": {\n    \"type\": \"oauth\",\n    \"t\": 1\n  },\n  \"a\": {\n    \"type\": \"oauth\",\n    \"t\": 3\n  }\n}"
    );

    // settings 的数字键同样按数值升序。
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"10":1,"2":2,"apiKeys":{"p":"k"},"z":3}"#,
    )
    .expect("settings");
    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json();
    }
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("settings.json")).expect("settings"),
        "{\n  \"2\": 2,\n  \"10\": 1,\n  \"z\": 3\n}"
    );

    // 数组凭证：`{type:"oauth", ...["a","b"]}` 的键序是 0、1、type。
    let agent_dir = create_agent_dir();
    std::fs::write(agent_dir.path().join("oauth.json"), r#"{"p":["a","b"]}"#).expect("oauth");
    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json();
    }
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"p\": {\n    \"0\": \"a\",\n    \"1\": \"b\",\n    \"type\": \"oauth\"\n  }\n}"
    );
}

/// `Object.entries` 对非对象根：`null` 抛错（不改名），字符串按码元枚举，
/// 数字没有条目（解析成功后仍改名）。
#[test]
fn oauth_json_root_values_follow_js_object_entries() {
    // null：`Object.entries(null)` 抛 TypeError → catch → oauth.json 原样保留。
    let agent_dir = create_agent_dir();
    std::fs::write(agent_dir.path().join("oauth.json"), "null").expect("oauth");
    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };
    assert!(providers.is_empty());
    assert!(agent_dir.path().join("oauth.json").exists());
    assert!(!agent_dir.path().join("oauth.json.migrated").exists());
    assert!(!agent_dir.path().join("auth.json").exists());

    // 字符串：每个字符一个索引 provider，展开成 `{0: "a", type: "oauth"}`。
    let agent_dir = create_agent_dir();
    std::fs::write(agent_dir.path().join("oauth.json"), "\"abc\"").expect("oauth");
    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };
    assert_eq!(
        providers,
        vec!["0".to_string(), "1".to_string(), "2".to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"0\": {\n    \"0\": \"a\",\n    \"type\": \"oauth\"\n  },\n  \"1\": {\n    \"0\": \"b\",\n    \"type\": \"oauth\"\n  },\n  \"2\": {\n    \"0\": \"c\",\n    \"type\": \"oauth\"\n  }\n}"
    );
    assert!(agent_dir.path().join("oauth.json.migrated").exists());

    // 数字：没有可枚举自有属性 → 无 provider，但改名仍发生。
    let agent_dir = create_agent_dir();
    std::fs::write(agent_dir.path().join("oauth.json"), "5").expect("oauth");
    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };
    assert!(providers.is_empty());
    assert!(!agent_dir.path().join("auth.json").exists());
    assert!(agent_dir.path().join("oauth.json.migrated").exists());
}

/// `!migrated[provider]` 的 `Object.prototype` 真值语义：`toString`/`constructor` 等
/// 继承属性是 truthy，因此 settings 里同名的 apiKeys 不会被迁移；`__proto__` 读取也 truthy。
#[test]
fn settings_api_keys_with_object_prototype_names_are_skipped() {
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("settings.json"),
        r#"{"apiKeys":{"toString":"sk","constructor":"sk3","hasOwnProperty":"sk4","__proto__":"sk2","normal":"ok"}}"#,
    )
    .expect("settings");

    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };

    assert_eq!(providers, vec!["normal".to_string()]);
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"normal\": {\n    \"type\": \"api_key\",\n    \"key\": \"ok\"\n  }\n}"
    );
}

/// `migrated["__proto__"] = {...}` 设置原型而非自有属性：provider 仍被报告，
/// 但不会出现在 auth.json 里；普通 provider 照常写入。
#[test]
fn oauth_proto_provider_sets_the_prototype_and_is_still_reported() {
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        r#"{"__proto__":{"a":1},"toString":{"b":2}}"#,
    )
    .expect("oauth");

    let providers = {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    };

    assert_eq!(
        providers,
        vec!["__proto__".to_string(), "toString".to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(agent_dir.path().join("auth.json")).expect("auth"),
        "{\n  \"toString\": {\n    \"type\": \"oauth\",\n    \"b\": 2\n  }\n}"
    );
}

/// 会话迁移目录名来自 header 里的**原始** cwd 字符串，不做 node `path.resolve`：
/// 相对路径保持相对、`..` 不被折叠、末尾斜杠保留。
#[test]
fn session_migration_encodes_the_raw_header_cwd() {
    let agent_dir = create_agent_dir();
    for (index, cwd) in [
        "relative/proj",
        "/workspace/../project",
        "/workspace/project/",
    ]
    .iter()
    .enumerate()
    {
        std::fs::write(
            agent_dir.path().join(format!("s{index}.jsonl")),
            format!("{{\"type\":\"session\",\"cwd\":\"{cwd}\"}}\n"),
        )
        .expect("session file");
    }

    {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        run_migrations(agent_dir.path());
    }

    assert!(
        agent_dir
            .path()
            .join("sessions")
            .join("--relative-proj--")
            .join("s0.jsonl")
            .exists()
    );
    assert!(
        agent_dir
            .path()
            .join("sessions")
            .join("--workspace-..-project--")
            .join("s1.jsonl")
            .exists()
    );
    assert!(
        agent_dir
            .path()
            .join("sessions")
            .join("--workspace-project---")
            .join("s2.jsonl")
            .exists()
    );
}

/// rename 失败时的警告使用 Node 的 `error.message`
/// （`EACCES: permission denied, rename '<src>' -> '<dst>'`），不是 Rust 的 io::Error 文案。
#[cfg(unix)]
#[test]
fn commands_rename_failure_warns_with_the_node_fs_message() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let base = TempDir::new().expect("temp dir");
    std::fs::create_dir(base.path().join("commands")).expect("commands");
    std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o555)).expect("chmod");

    let output = kiana_coding_agent::cli::setup::Output::capture();
    let migrated = kiana_coding_agent::migrations::migrate_commands_to_prompts_with_output(
        base.path(),
        "Global",
        &output,
    );
    std::fs::set_permissions(base.path(), std::fs::Permissions::from_mode(0o755)).expect("chmod");

    assert!(!migrated);
    let plain = kiana_core::ansi::strip_ansi(&output.captured_stdout());
    assert_eq!(
        plain,
        format!(
            "Warning: Could not migrate Global commands/ to prompts/: EACCES: permission denied, rename '{}' -> '{}'\n",
            base.path().join("commands").display(),
            base.path().join("prompts").display()
        )
    );
}

/// auth.json 写入失败在 TS 里位于 try 之外 → 未捕获异常中止启动。Rust 用 panic 表示
/// 同样的启动中止（无法复刻 JS stack trace），而不是静默继续。
#[cfg(unix)]
#[test]
fn auth_json_write_failure_aborts_startup() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let agent_dir = create_agent_dir();
    std::fs::write(
        agent_dir.path().join("oauth.json"),
        r#"{"openai":{"access":"A"}}"#,
    )
    .expect("oauth");
    std::fs::set_permissions(agent_dir.path(), std::fs::Permissions::from_mode(0o555))
        .expect("chmod");

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _env = EnvGuard::set_agent_dir(agent_dir.path());
        kiana_coding_agent::migrations::migrate_auth_to_auth_json()
    }));
    std::fs::set_permissions(agent_dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");

    let payload = outcome.expect_err("write failure must abort startup");
    let message = payload
        .downcast_ref::<kiana_coding_agent::cli::setup::FatalStartupError>()
        .map(|error| error.message.clone())
        .unwrap_or_default();
    assert!(
        message.starts_with("EACCES: permission denied, open '"),
        "{message}"
    );
    assert!(!agent_dir.path().join("auth.json").exists());
}

#[cfg(unix)]
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|uid| uid.trim().parse::<u32>().ok())
        == Some(0)
}
