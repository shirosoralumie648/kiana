//! `model-config.ts` 的 spec 派生测试（TS 侧没有专门的 `model-config.test.ts`）。
//!
//! 错误文案与遍历顺序由 TS 参考实现生成（`tests/fixtures/models/model_config_cases.json`，
//! 用 `ModelConfig.load` 逐例捕获，`__DIR__` 是临时目录占位符）。另外覆盖
//! `config-schemas.test.ts` 里与 models.json 相关的三个用例（非正 token 上限、自定义
//! compatibility、`$schema` 运行时校验）。

use std::path::Path;

use kiana_config::models::config_file::{
    ModelConfig, format_validation_path, models_config_schema, models_json_schemas,
    strip_json_comments,
};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct Fixture {
    errors: Vec<ErrorCase>,
    success: Vec<SuccessCase>,
}

#[derive(Debug, Deserialize)]
struct ErrorCase {
    name: String,
    content: String,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuccessCase {
    name: String,
    content: String,
    error: Option<String>,
    #[serde(rename = "providerIds")]
    provider_ids: Vec<String>,
    #[serde(rename = "provider")]
    _provider: Value,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/models/model_config_cases.json"))
        .expect("valid fixture")
}

fn write_models(directory: &Path, content: &str) -> String {
    let path = directory.join("models.json");
    std::fs::write(&path, content).expect("write models.json");
    path.to_string_lossy().to_string()
}

async fn load_error(directory: &Path, content: &str) -> Option<String> {
    let path = write_models(directory, content);
    let config = ModelConfig::load(Some(&path)).await;
    config.get_error().map(str::to_string)
}

#[tokio::test]
async fn matches_typebox_error_messages_and_ordering() {
    let fixture = fixture();
    let directory = TempDir::new().expect("tempdir");
    for case in &fixture.errors {
        let error = load_error(directory.path(), &case.content).await;
        let expected = case
            .error
            .as_ref()
            .map(|error| error.replace("__DIR__", &directory.path().to_string_lossy()));
        assert_eq!(error, expected, "case {:?}", case.name);
    }
}

#[tokio::test]
async fn accepts_valid_configs_and_preserves_provider_order() {
    let fixture = fixture();
    let directory = TempDir::new().expect("tempdir");
    for case in &fixture.success {
        let path = write_models(directory.path(), &case.content);
        let config = ModelConfig::load(Some(&path)).await;
        assert_eq!(
            config.get_error(),
            case.error.as_deref(),
            "case {:?}",
            case.name
        );
        assert_eq!(
            config.get_provider_ids(),
            case.provider_ids,
            "case {:?}",
            case.name
        );
    }
}

#[tokio::test]
async fn preserves_compat_and_unknown_fields() {
    let directory = TempDir::new().expect("tempdir");
    let path = write_models(
        directory.path(),
        r#"{"providers":{"demo":{"api":"custom-api","compat":{"customOption":"value","supportsStore":true},"extra":1}}}"#,
    );
    let config = ModelConfig::load(Some(&path)).await;
    assert!(config.get_error().is_none());
    let provider = config.get_provider("demo").expect("provider");
    assert_eq!(
        provider.compat,
        Some(serde_json::json!({"customOption": "value", "supportsStore": true}))
    );
    assert_eq!(provider.api.as_deref(), Some("custom-api"));
}

#[tokio::test]
async fn jsonc_bom_and_trailing_commas() {
    let directory = TempDir::new().expect("tempdir");
    let content = "\u{FEFF}{\n  // line comment\n  \"providers\": {\n    \"demo\": {\n      \"baseUrl\": \"http://localhost:8080/v1\", // trailing comment\n      \"models\": [\n        { \"id\": \"demo\", \"name\": \"a // b\", },\n      ],\n    },\n  },\n}\n";
    let path = write_models(directory.path(), content);
    let config = ModelConfig::load(Some(&path)).await;
    assert_eq!(config.get_error(), None);
    let provider = config.get_provider("demo").expect("provider");
    let models = provider.models.as_ref().expect("models");
    assert_eq!(models[0].id, "demo");
    assert_eq!(models[0].name.as_deref(), Some("a // b"));
}

#[tokio::test]
async fn block_comments_are_not_jsonc() {
    let directory = TempDir::new().expect("tempdir");
    let error = load_error(directory.path(), "{\"providers\":{/*x*/}}").await;
    assert_eq!(
        error.as_deref(),
        Some(
            "Failed to parse models.json: Expected property name or '}' in JSON at position 14 (line 1 column 15)\n\nFile: __DIR__/models.json"
                .replace("__DIR__", &directory.path().to_string_lossy())
                .as_str()
        )
    );
}

#[tokio::test]
async fn missing_file_is_an_empty_config() {
    let directory = TempDir::new().expect("tempdir");
    let missing = directory.path().join("missing.json");
    let config = ModelConfig::load(Some(&missing.to_string_lossy())).await;
    assert_eq!(config.get_error(), None);
    assert!(config.get_provider_ids().is_empty());
    // `undefined` / 空串等价于未提供路径。
    assert_eq!(ModelConfig::load(None).await.get_error(), None);
    assert_eq!(ModelConfig::load(Some("")).await.get_error(), None);
}

#[tokio::test]
async fn directory_read_reports_eisdir() {
    let directory = TempDir::new().expect("tempdir");
    let config = ModelConfig::load(Some(&directory.path().to_string_lossy())).await;
    let expected = format!(
        "Failed to load models.json: EISDIR: illegal operation on a directory, read '{}'\n\nFile: {}",
        directory.path().to_string_lossy(),
        directory.path().to_string_lossy()
    );
    assert_eq!(config.get_error(), Some(expected.as_str()));
}

#[tokio::test]
async fn rejects_non_positive_model_token_limits() {
    // `config-schemas.test.ts`：models[] 与 modelOverrides 的 contextWindow/maxTokens。
    let directory = TempDir::new().expect("tempdir");
    for field in ["contextWindow", "maxTokens"] {
        for value in [0, -1] {
            let content = serde_json::json!({
                "providers": {"local": {"models": [{"id": "model", (field): value}]}}
            })
            .to_string();
            let error = load_error(directory.path(), &content).await;
            assert!(
                error
                    .as_deref()
                    .is_some_and(|error| error.contains("Invalid models.json schema")),
                "models[].{field}={value}"
            );
            let content = serde_json::json!({
                "providers": {"local": {"modelOverrides": {"model": {(field): value}}}}
            })
            .to_string();
            let error = load_error(directory.path(), &content).await;
            assert!(
                error
                    .as_deref()
                    .is_some_and(|error| error.contains("Invalid models.json schema")),
                "modelOverrides.{field}={value}"
            );
        }
    }
    let content = r#"{"providers":{"local":{"models":[{"id":"model","contextWindow":1,"maxTokens":1}],"modelOverrides":{"model":{"contextWindow":1,"maxTokens":1}}}}}"#;
    assert_eq!(load_error(directory.path(), content).await, None);
}

#[tokio::test]
async fn rejects_malformed_known_compat_fields() {
    let directory = TempDir::new().expect("tempdir");
    let error = load_error(
        directory.path(),
        r#"{"providers":{"demo":{"compat":{"supportsLongCacheRetention":"yes"}}}}"#,
    )
    .await;
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("Invalid models.json schema"))
    );
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("providers.demo.compat.supportsLongCacheRetention"))
    );
}

#[tokio::test]
async fn validates_dollar_schema_at_runtime() {
    let directory = TempDir::new().expect("tempdir");
    let path = write_models(
        directory.path(),
        r#"{"$schema":"https://kiana.dev/schemas/models.schema.json","providers":{"demo":{"baseUrl":"http://localhost:8080/v1","models":[{"id":"demo"}]}}}"#,
    );
    let config = ModelConfig::load(Some(&path)).await;
    assert_eq!(config.get_error(), None);
    let provider = config.get_provider("demo").expect("provider");
    assert_eq!(provider.models.as_ref().expect("models")[0].id, "demo");

    let path = write_models(
        directory.path(),
        r#"{"$schema":"https://kiana.dev/schemas/models.schema.json","providers":{"demo":{"models":[{"id":""}]}}}"#,
    );
    let error = ModelConfig::load(Some(&path))
        .await
        .get_error()
        .map(str::to_string);
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("Invalid models.json schema"))
    );

    let path = write_models(directory.path(), r#"{"$schema":42,"providers":{}}"#);
    let error = ModelConfig::load(Some(&path))
        .await
        .get_error()
        .map(str::to_string);
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("$schema"))
    );
}

#[tokio::test]
async fn accepts_unvalidated_unknown_map_values() {
    // TS 只把 schema 校验当作接受门槛：`Record` 类型字段里**未知键**的值不受约束，
    // 校验通过后仍按原对象使用（composer 从不读取未知键）。Rust 侧同样接受这些配置。
    let directory = TempDir::new().expect("tempdir");
    let path = write_models(
        directory.path(),
        r#"{"providers":{"demo":{"models":[{"id":"m","thinkingLevelMap":{"weird":5,"low":"low"},"promptCache":{"weird":"x","short":60},"samplingParamsByThinkingLevel":{"weird":"x","low":{"temperature":0.5}}}],"modelOverrides":{"m":{"thinkingLevelMap":{"weird":5},"promptCache":{"weird":"x"},"samplingParamsByThinkingLevel":{"weird":"x"}}}}}}"#,
    );
    let config = ModelConfig::load(Some(&path)).await;
    assert_eq!(config.get_error(), None);
    let provider = config.get_provider("demo").expect("provider");
    let model = &provider.models.as_ref().expect("models")[0];
    let levels = model.thinking_level_map.as_ref().expect("levels");
    assert_eq!(levels.get("low"), Some(&Some("low".to_string())));
    assert!(!levels.contains_key("weird"));
    let cache = model.prompt_cache.as_ref().expect("cache");
    assert_eq!(cache.get("short"), Some(&60.0));
    assert!(!cache.contains_key("weird"));
    let by_level = model
        .sampling_params_by_thinking_level
        .as_ref()
        .expect("sampling levels");
    assert!(by_level.contains_key("low"));
    assert!(!by_level.contains_key("weird"));
}

#[tokio::test]
async fn accepts_integers_beyond_u64_range() {
    // TypeBox 的 `Integer` 只要求 `Number.isInteger`；Rust 的 `u64` 字段饱和转换。
    let directory = TempDir::new().expect("tempdir");
    let path = write_models(
        directory.path(),
        r#"{"providers":{"demo":{"models":[{"id":"m","inputLimits":{"maxRequestBytes":1e20}}],"modelOverrides":{"m":{"inputLimits":{"maxRequestBytes":1e300}}}}}}"#,
    );
    let config = ModelConfig::load(Some(&path)).await;
    assert_eq!(config.get_error(), None);
    let provider = config.get_provider("demo").expect("provider");
    let limits = provider.models.as_ref().expect("models")[0]
        .input_limits
        .as_ref()
        .expect("limits");
    assert_eq!(limits.max_request_bytes, Some(u64::MAX));
    let override_limits = provider.model_overrides.as_ref().expect("overrides")["m"]
        .input_limits
        .as_ref()
        .expect("override limits");
    assert_eq!(override_limits.max_request_bytes, Some(u64::MAX));
}

#[tokio::test]
async fn reports_node_errno_messages() {
    // Node 的 `readFile` 错误文案来自 libuv（`<CODE>: <description>, <syscall> '<path>'`）。
    let directory = TempDir::new().expect("tempdir");
    let parent = directory.path().join("regular-file");
    std::fs::write(&parent, "x").expect("write");
    let path = parent.join("models.json");
    let config = ModelConfig::load(Some(&path.to_string_lossy())).await;
    let expected = format!(
        "Failed to load models.json: ENOTDIR: not a directory, open '{}'\n\nFile: {}",
        path.to_string_lossy(),
        path.to_string_lossy()
    );
    assert_eq!(config.get_error(), Some(expected.as_str()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = directory.path().join("loop");
        symlink(&link, &link).expect("symlink");
        let config = ModelConfig::load(Some(&link.to_string_lossy())).await;
        let expected = format!(
            "Failed to load models.json: ELOOP: too many symbolic links encountered, open '{}'\n\nFile: {}",
            link.to_string_lossy(),
            link.to_string_lossy()
        );
        assert_eq!(config.get_error(), Some(expected.as_str()));

        let long = directory.path().join("a".repeat(300));
        let config = ModelConfig::load(Some(&long.to_string_lossy())).await;
        let expected = format!(
            "Failed to load models.json: ENAMETOOLONG: name too long, open '{}'\n\nFile: {}",
            long.to_string_lossy(),
            long.to_string_lossy()
        );
        assert_eq!(config.get_error(), Some(expected.as_str()));
    }
}

#[tokio::test]
async fn provider_order_follows_json_insertion_order() {
    let directory = TempDir::new().expect("tempdir");
    let path = write_models(
        directory.path(),
        r#"{"providers":{"zeta":{"baseUrl":"http://x"},"alpha":{"baseUrl":"http://y"},"mid":{"baseUrl":"http://z"}}}"#,
    );
    let config = ModelConfig::load(Some(&path)).await;
    assert_eq!(config.get_provider_ids(), ["zeta", "alpha", "mid"]);
}

#[test]
fn schema_exports_are_validation_shaped() {
    let config = models_config_schema();
    assert_eq!(config["type"], "object");
    assert_eq!(config["required"], serde_json::json!(["providers"]));
    let schemas = models_json_schemas();
    assert_eq!(schemas.model["required"], serde_json::json!(["id"]));
    assert!(schemas.model_override.get("required").is_none());
    assert_eq!(schemas.provider["properties"]["oauth"]["const"], "radius");
    assert_eq!(
        schemas.provider["properties"]["models"]["items"],
        schemas.model
    );
}

#[test]
fn strip_json_comments_is_shared_with_foundation() {
    // 字符串内的 `//` 与 `,}` 原样保留；块注释不是 JSONC。
    assert_eq!(
        strip_json_comments("{\"a\":\"x // y\", // c\n\"b\":1,}"),
        "{\"a\":\"x // y\", \n\"b\":1}"
    );
    assert_eq!(strip_json_comments("{/*x*/}"), "{/*x*/}");
}

#[test]
fn format_validation_path_handles_root_and_required() {
    use kiana_config::models::config_file::ModelsJsonValidationError;
    let root = ModelsJsonValidationError {
        keyword: "type".into(),
        instance_path: String::new(),
        message: "must be object".into(),
        required_properties: Vec::new(),
    };
    assert_eq!(format_validation_path(&root), "root");
}
