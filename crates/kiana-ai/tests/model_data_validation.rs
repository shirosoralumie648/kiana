//! 移植 `packages/ai/test/model-data-validation.test.ts`（21 个用例）。
//!
//! TS 的 `createFixture()` 在临时目录里搭一个最小 package（`src/models.generated.ts`、
//! `src/providers/test-provider.models.ts`、`src/providers/data/`），然后直接调用
//! `scripts/model-data.ts` 与 `scripts/hydrate-model-catalog.ts` 的导出函数。Rust 侧对应
//! `kiana_ai::model_catalog::{model_data, hydrate}`，并复用与生产一致的目录布局，确保
//! fixture 覆盖的就是真实读取路径。

mod model_data_support;

use std::path::{Path, PathBuf};

use kiana_ai::model_catalog::hydrate::hydrate_model_catalog;
use kiana_ai::model_catalog::model_data::{
    MODEL_DATA_MANIFEST_FILE, MODEL_DATA_SCHEMA_VERSION, ModelDataStructure,
    assert_exact_model_ids, create_model_data_manifest, read_model_data_structure,
    validate_model_data_directory,
};
use model_data_support::{TempDir, assert_js_eq, write_file};
use serde_json::{Map, Value, json};

const GENERATED_AT: &str = "2026-07-23T10:00:00.000Z";

/// `createFixture()` 的返回值。
struct Fixture {
    /// 持有临时目录：drop 时递归删除（字段本身不被读取）。
    _root: TempDir,
    package_root: PathBuf,
    data_dir: PathBuf,
    structure: ModelDataStructure,
    values: Map<String, Value>,
}

impl Fixture {
    fn new() -> Fixture {
        let root = TempDir::new("kiana-model-data-");
        let package_root = root.path().join("package");
        let providers_dir = package_root.join("src").join("providers");
        let data_dir = providers_dir.join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        write_file(
            &package_root.join("src/models.generated.ts"),
            "import { TEST_PROVIDER_CLASSIFIER_MODELS, TEST_PROVIDER_IMAGE_MODELS, TEST_PROVIDER_MODELS } from \"./providers/test-provider.models.ts\";\n",
        );
        write_file(
            &providers_dir.join("test-provider.models.ts"),
            "import values from \"./data/test-provider.json\" with { type: \"json\" };\n",
        );

        let mut structure = ModelDataStructure::new();
        structure.insert(
            "test-provider".to_string(),
            [("chat:model-a".to_string(), "openai-completions".to_string())]
                .into_iter()
                .collect(),
        );
        let mut values = Map::new();
        values.insert("chat:model-a".to_string(), chat_model());

        let mut fixture = Fixture {
            _root: root,
            package_root,
            data_dir,
            structure,
            values,
        };
        fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
        fixture
    }

    fn package_root(&self) -> &Path {
        &self.package_root
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `writeFixtureData(dataDir, structure, values, manifestSchemaVersion, apiGroup)`。
    fn write_fixture_data(&mut self, manifest_schema_version: u64, api_group: &str) {
        self.write_fixture_data_with(
            &self.structure.clone(),
            &self.values.clone(),
            manifest_schema_version,
            api_group,
        );
    }

    fn write_fixture_data_with(
        &mut self,
        structure: &ModelDataStructure,
        values: &Map<String, Value>,
        manifest_schema_version: u64,
        api_group: &str,
    ) {
        let filename = "test-provider.json";
        let content = format!(
            "{}\n",
            model_data_support::js(&json!({ api_group: Value::Object(values.clone()) }))
        );
        write_file(&self.data_dir.join(filename), &content);

        let mut files = indexmap::IndexMap::new();
        files.insert(filename.to_string(), content);
        let mut manifest = create_model_data_manifest(structure, &files, GENERATED_AT);
        manifest.schema_version = manifest_schema_version;
        write_file(
            &self.data_dir.join(MODEL_DATA_MANIFEST_FILE),
            &format!("{}\n", model_data_support::js(&manifest.to_json())),
        );
    }

    /// `catalog.json` 路径（TS 测试里都放在 packageRoot 下）。
    fn catalog_path(&self) -> PathBuf {
        self.package_root.join("catalog.json")
    }

    /// 写 catalog 文件。
    fn write_catalog(&self, catalog: &Value) {
        write_file(&self.catalog_path(), &model_data_support::js(catalog));
    }

    /// `fixture.values["chat:model-a"]` 的可变引用。
    fn chat_model_mut(&mut self) -> &mut Map<String, Value> {
        self.values
            .get_mut("chat:model-a")
            .and_then(Value::as_object_mut)
            .expect("chat:model-a")
    }
}

/// `values["chat:model-a"]` 的初始值。
fn chat_model() -> Value {
    json!({
        "type": "chat",
        "id": "model-a",
        "name": "Model A",
        "api": "openai-completions",
        "provider": "test-provider",
        "baseUrl": "https://example.test/v1",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
        "maxTokens": 100,
    })
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn error_of(result: Result<(), kiana_ai::model_catalog::CatalogError>) -> String {
    match result {
        Ok(()) => panic!("expected an error"),
        Err(error) => error.to_string(),
    }
}

// ---------------------------------------------------------------------------
// published model catalog hydration
// ---------------------------------------------------------------------------

#[test]
fn hydrates_a_clean_checkout_deterministically_without_changing_generated_typescript() {
    let fixture = Fixture::new();
    let aggregator_path = fixture.package_root().join("src/models.generated.ts");
    let aggregator = read_text(&aggregator_path);
    let model = fixture.values["chat:model-a"].clone();
    fixture.write_catalog(&json!({ "test-provider": [model], "extra-provider": [model] }));
    std::fs::remove_dir_all(fixture.data_dir()).expect("remove data dir");

    hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false).expect("hydrate");
    validate_model_data_directory(&fixture.structure, fixture.data_dir()).expect("validates");
    assert_js_eq(
        &serde_json::to_value(
            read_model_data_structure(fixture.package_root()).expect("structure"),
        )
        .expect("json"),
        &serde_json::to_value(&fixture.structure).expect("json"),
        "structure",
    );
    assert_eq!(
        read_text(&aggregator_path),
        aggregator,
        "aggregator is untouched"
    );

    // 同一输入再跑一次必须产生完全相同的字节。
    let manifest = read_text(&fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE));
    let provider = read_text(&fixture.data_dir().join("test-provider.json"));
    hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false)
        .expect("re-hydrate");
    assert_eq!(
        read_text(&fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE)),
        manifest
    );
    assert_eq!(
        read_text(&fixture.data_dir().join("test-provider.json")),
        provider
    );
}

#[test]
fn groups_models_by_api_and_keys_them_by_type_and_id() {
    let fixture = Fixture::new();
    let model = fixture.values["chat:model-a"].clone();
    let mut second = model.clone();
    second["id"] = json!("model-b");
    second["api"] = json!("anthropic-messages");
    fixture.write_catalog(&json!({ "test-provider": [model, second] }));
    hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false).expect("hydrate");

    let structure = read_model_data_structure(fixture.package_root()).expect("structure");
    assert_eq!(
        structure["test-provider"],
        [
            ("chat:model-a".to_string(), "openai-completions".to_string()),
            ("chat:model-b".to_string(), "anthropic-messages".to_string()),
        ]
        .into_iter()
        .collect::<indexmap::IndexMap<_, _>>()
    );
}

#[test]
fn keys_chat_and_image_models_with_the_same_id_separately() {
    let fixture = Fixture::new();
    let image = json!({
        "type": "image",
        "id": "model-a",
        "name": "Model A Image",
        "api": "openrouter-images",
        "provider": "test-provider",
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "output": ["image"],
        "cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 },
    });
    let model = fixture.values["chat:model-a"].clone();
    fixture.write_catalog(&json!({ "test-provider": [model, image] }));
    hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false).expect("hydrate");

    let structure = read_model_data_structure(fixture.package_root()).expect("structure");
    assert_eq!(
        structure["test-provider"],
        [
            ("chat:model-a".to_string(), "openai-completions".to_string()),
            ("image:model-a".to_string(), "openrouter-images".to_string()),
        ]
        .into_iter()
        .collect::<indexmap::IndexMap<_, _>>()
    );
}

#[test]
fn validates_without_replacing_existing_data() {
    let fixture = Fixture::new();
    let original = read_text(&fixture.data_dir().join("test-provider.json"));
    let model = fixture.values["chat:model-a"].clone();
    let mut second = model.clone();
    second["id"] = json!("model-b");
    fixture.write_catalog(&json!({ "test-provider": [model.clone(), second] }));
    hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), true)
        .expect("validate only");
    assert_eq!(
        read_text(&fixture.data_dir().join("test-provider.json")),
        original
    );

    fixture.write_catalog(&json!({ "other-provider": [model] }));
    let error = error_of(hydrate_model_catalog(
        fixture.package_root(),
        &fixture.catalog_path(),
        true,
    ));
    assert!(error.contains("missing provider"), "{error}");
}

#[test]
fn rejects_incomplete_catalogs_without_replacing_existing_data() {
    let catalogs = [
        Value::Null,
        json!([]),
        json!({}),
        json!({ "test-provider": [] }),
        json!({ "test-provider": {} }),
        json!({ "test-provider": [{ "id": "x" }] }),
    ];
    for catalog in catalogs {
        let fixture = Fixture::new();
        let original = read_text(&fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE));
        fixture.write_catalog(&catalog);
        let result = hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false);
        assert!(result.is_err(), "expected {catalog} to be rejected");
        assert_eq!(
            read_text(&fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE)),
            original
        );
    }
}

#[test]
fn validates_models_before_replacing_existing_data() {
    for field in ["api", "provider", "cost"] {
        let mut fixture = Fixture::new();
        let original = read_text(&fixture.data_dir().join("test-provider.json"));
        fixture
            .chat_model_mut()
            .insert(field.to_string(), Value::Null);
        let model = fixture.values["chat:model-a"].clone();
        fixture.write_catalog(&json!({ "test-provider": [model] }));
        let result = hydrate_model_catalog(fixture.package_root(), &fixture.catalog_path(), false);
        assert!(result.is_err(), "{field} = null must be rejected");
        assert_eq!(
            read_text(&fixture.data_dir().join("test-provider.json")),
            original
        );
    }
}

// ---------------------------------------------------------------------------
// generated model data validation
// ---------------------------------------------------------------------------

#[test]
fn rejects_a_missing_upstream_model_from_an_exact_generated_allowlist() {
    let error = assert_exact_model_ids(
        "qwen-token-plan-individual",
        ["model-a", "model-b"].map(str::to_string),
        ["model-a"].map(str::to_string),
    )
    .expect_err("missing model");
    assert_eq!(
        error.to_string(),
        "qwen-token-plan-individual model IDs do not match (missing: model-b)"
    );
}

#[test]
fn rejects_an_unexpected_model_from_an_exact_generated_allowlist() {
    let error = assert_exact_model_ids(
        "test-provider",
        ["model-a"].map(str::to_string),
        ["model-a", "model-b"].map(str::to_string),
    )
    .expect_err("extra model");
    assert_eq!(
        error.to_string(),
        "test-provider model IDs do not match (extra: model-b)"
    );
}

#[test]
fn reads_and_validates_api_grouped_model_data() {
    let fixture = Fixture::new();
    assert_js_eq(
        &serde_json::to_value(
            read_model_data_structure(fixture.package_root()).expect("structure"),
        )
        .expect("json"),
        &serde_json::to_value(&fixture.structure).expect("json"),
        "structure",
    );
    validate_model_data_directory(&fixture.structure, fixture.data_dir()).expect("validates");
}

#[test]
fn rejects_a_missing_model_data_directory() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(fixture.data_dir()).expect("remove data dir");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("does not exist"), "{error}");
}

#[test]
fn rejects_a_wrong_model_id() {
    let mut fixture = Fixture::new();
    fixture
        .chat_model_mut()
        .insert("id".to_string(), json!("wrong-id"));
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("has id"), "{error}");
}

#[test]
fn rejects_a_wrong_model_provider() {
    let mut fixture = Fixture::new();
    fixture
        .chat_model_mut()
        .insert("provider".to_string(), json!("wrong-provider"));
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("has provider"), "{error}");
}

#[test]
fn rejects_a_wrong_model_api() {
    let mut fixture = Fixture::new();
    fixture
        .chat_model_mut()
        .insert("api".to_string(), json!("anthropic-messages"));
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("has api"), "{error}");
}

#[test]
fn rejects_a_model_without_a_known_type() {
    let mut fixture = Fixture::new();
    fixture.chat_model_mut().shift_remove("type");
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(
        error.contains("expected \"chat\", \"image\", or \"classifier\""),
        "{error}"
    );
}

#[test]
fn validates_image_models_with_output_modalities_and_without_chat_limits() {
    let mut fixture = Fixture::new();
    let mut structure = ModelDataStructure::new();
    structure.insert(
        "test-provider".to_string(),
        [("image:image-a".to_string(), "test-images".to_string())]
            .into_iter()
            .collect(),
    );
    let image = json!({
        "type": "image",
        "id": "image-a",
        "name": "Image A",
        "api": "test-images",
        "provider": "test-provider",
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "output": ["image", "text"],
        "cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 },
    });

    let mut validate = |image: &Value| -> Result<(), kiana_ai::model_catalog::CatalogError> {
        let mut values = Map::new();
        values.insert("image:image-a".to_string(), image.clone());
        fixture.write_fixture_data_with(
            &structure,
            &values,
            MODEL_DATA_SCHEMA_VERSION,
            "test-images",
        );
        validate_model_data_directory(&structure, fixture.data_dir())
    };

    validate(&image).expect("valid image model");

    let mut no_output = image.clone();
    no_output
        .as_object_mut()
        .expect("object")
        .shift_remove("output");
    let error = error_of(validate(&no_output));
    assert!(error.contains("invalid output modalities"), "{error}");

    let mut text_output = image.clone();
    text_output["output"] = json!(["text"]);
    let error = error_of(validate(&text_output));
    assert!(error.contains("invalid output modalities"), "{error}");
}

#[test]
fn rejects_output_modalities_on_chat_models() {
    let mut fixture = Fixture::new();
    fixture
        .chat_model_mut()
        .insert("output".to_string(), json!(["text"]));
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("unsupported output modalities"), "{error}");
}

#[test]
fn validates_classifier_models_without_chat_output_limits() {
    let mut fixture = Fixture::new();
    let mut structure = ModelDataStructure::new();
    structure.insert(
        "test-provider".to_string(),
        [(
            "classifier:classifier-a".to_string(),
            "test-classifier".to_string(),
        )]
        .into_iter()
        .collect(),
    );
    let classifier = json!({
        "type": "classifier",
        "id": "classifier-a",
        "name": "Classifier A",
        "api": "test-classifier",
        "provider": "test-provider",
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
    });
    let mut values = Map::new();
    values.insert("classifier:classifier-a".to_string(), classifier);
    fixture.write_fixture_data_with(
        &structure,
        &values,
        MODEL_DATA_SCHEMA_VERSION,
        "test-classifier",
    );
    validate_model_data_directory(&structure, fixture.data_dir()).expect("valid classifier model");
}

#[test]
fn rejects_a_model_in_the_wrong_api_group() {
    let mut fixture = Fixture::new();
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "anthropic-messages");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("grouped under API"), "{error}");
}

#[test]
fn rejects_duplicate_model_ids_across_api_groups() {
    let fixture = Fixture::new();
    let filename = "test-provider.json";
    let content = format!(
        "{}\n",
        model_data_support::js(&json!({
            "openai-completions": Value::Object(fixture.values.clone()),
            "anthropic-messages": Value::Object(fixture.values.clone()),
        }))
    );
    write_file(&fixture.data_dir().join(filename), &content);
    let mut files = indexmap::IndexMap::new();
    files.insert(filename.to_string(), content);
    let manifest = create_model_data_manifest(&fixture.structure, &files, GENERATED_AT);
    write_file(
        &fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE),
        &format!("{}\n", model_data_support::js(&manifest.to_json())),
    );
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("more than one API group"), "{error}");
}

#[test]
fn rejects_missing_model_ids_and_stale_file_hashes() {
    let fixture = Fixture::new();
    write_file(&fixture.data_dir().join("test-provider.json"), "{}\n");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(
        error.contains("manifest hash") || error.contains("model IDs"),
        "{error}"
    );
}

#[test]
fn rejects_incompatible_schema_and_generation_stamps() {
    let mut fixture = Fixture::new();
    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION + 1, "openai-completions");
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("model data schema"), "{error}");

    fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
    let manifest_path = fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE);
    let mut manifest: Value =
        serde_json::from_str(&read_text(&manifest_path)).expect("manifest json");
    manifest["structureHash"] = json!("stale");
    write_file(
        &manifest_path,
        &format!("{}\n", model_data_support::js(&manifest)),
    );
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("generation stamp"), "{error}");
}

#[test]
fn rejects_an_invalid_generation_timestamp() {
    let fixture = Fixture::new();
    let manifest_path = fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE);
    let mut manifest: Value =
        serde_json::from_str(&read_text(&manifest_path)).expect("manifest json");
    manifest["generatedAt"] = json!("invalid");
    write_file(
        &manifest_path,
        &format!("{}\n", model_data_support::js(&manifest)),
    );
    let error = error_of(validate_model_data_directory(
        &fixture.structure,
        fixture.data_dir(),
    ));
    assert!(error.contains("generation timestamp"), "{error}");
}

#[test]
fn rejects_missing_provider_shards_imported_by_the_aggregator() {
    let fixture = Fixture::new();
    write_file(
        &fixture.package_root().join("src/models.generated.ts"),
        "import { TEST_PROVIDER_CLASSIFIER_MODELS, TEST_PROVIDER_IMAGE_MODELS, TEST_PROVIDER_MODELS } from \"./providers/test-provider.models.ts\";\nimport { MISSING_CLASSIFIER_MODELS, MISSING_IMAGE_MODELS, MISSING_MODELS } from \"./providers/missing.models.ts\";\n",
    );
    let error = match read_model_data_structure(fixture.package_root()) {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("aggregator and provider shards do not match"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// `check-model-data.ts`（`check:model-data` 门禁，spec 2.4.2）
// ---------------------------------------------------------------------------

mod check_model_data_gate {
    use super::*;
    use kiana_ai::model_catalog::check::{FAILURE_HINT, SUCCESS_LINE, check_model_data};

    #[test]
    fn prints_the_success_line_for_valid_data() {
        let fixture = Fixture::new();
        assert_eq!(
            check_model_data(fixture.package_root()).expect("valid"),
            "Generated model data is valid."
        );
        assert_eq!(SUCCESS_LINE, "Generated model data is valid.");
    }

    #[test]
    fn prints_the_error_then_the_hydration_hint_for_stale_data() {
        let fixture = Fixture::new();
        // 把 provider 分片清空：`readModelDataStructure` 会先报「没有模型数据」。
        write_file(&fixture.data_dir().join("test-provider.json"), "{}\n");
        let (stdout, stderr) = check_model_data(fixture.package_root()).expect_err("stale");
        assert_eq!(stdout, "", "失败时不写 stdout");
        assert!(
            stderr.ends_with(FAILURE_HINT),
            "stderr must end with the hint: {stderr}"
        );
        assert_eq!(
            FAILURE_HINT,
            "\nModel data is missing or stale. Run `npm run hydrate:model-data` from the repository root.\n",
            "`console.error` 在 hint 文本后追加换行"
        );
        let message = stderr.trim_end_matches(FAILURE_HINT);
        assert!(!message.is_empty(), "错误消息在前，hint 在后");
        assert!(
            message.contains("contains no generated model data"),
            "unexpected message: {message}"
        );
    }
}

// ---------------------------------------------------------------------------
// Node / V8 错误文本
//
// `hydrate-model-catalog.ts` 里 `JSON.parse(readFileSync(catalogPath, "utf8"))` 两层都没有
// `try`，所以读失败抛 Node 的 `ENOENT: no such file or directory, open '<path>'`，
// 解析失败抛 V8 的 `SyntaxError` 文本（`Unexpected token ...` / `Unexpected end of JSON input`）。
// `model-data.ts` 的 `readJsonObject` 则把两者都包成
// `"<description> is not valid JSON: <message>"`。
// ---------------------------------------------------------------------------

mod node_error_text {
    use super::*;
    use kiana_ai::model_catalog::model_data::validate_model_data_directory;

    #[test]
    fn hydrate_reports_a_missing_catalog_like_node() {
        let fixture = Fixture::new();
        let missing = fixture.package_root().join("does-not-exist.json");
        let error = error_of(hydrate_model_catalog(
            fixture.package_root(),
            &missing,
            false,
        ));
        assert_eq!(
            error,
            format!(
                "ENOENT: no such file or directory, open '{}'",
                missing.display()
            )
        );
    }

    #[test]
    fn hydrate_reports_json_syntax_errors_verbatim() {
        let fixture = Fixture::new();
        write_file(&fixture.catalog_path(), "{\"a\":}");
        let error = error_of(hydrate_model_catalog(
            fixture.package_root(),
            &fixture.catalog_path(),
            false,
        ));
        assert_eq!(
            error,
            "Unexpected token '}', \"{\"a\":}\" is not valid JSON"
        );

        write_file(&fixture.catalog_path(), "");
        let error = error_of(hydrate_model_catalog(
            fixture.package_root(),
            &fixture.catalog_path(),
            false,
        ));
        assert_eq!(error, "Unexpected end of JSON input");
    }

    #[test]
    fn hydrate_reports_a_non_object_catalog() {
        let fixture = Fixture::new();
        fixture.write_catalog(&json!([]));
        let error = error_of(hydrate_model_catalog(
            fixture.package_root(),
            &fixture.catalog_path(),
            false,
        ));
        assert_eq!(error, "Model catalog must be an object");
    }

    #[test]
    fn validation_wraps_a_missing_manifest_as_invalid_json() {
        let mut fixture = Fixture::new();
        fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
        let manifest_path = fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE);
        std::fs::remove_file(&manifest_path).expect("remove manifest");
        let error = match validate_model_data_directory(&fixture.structure, fixture.data_dir()) {
            Ok(()) => panic!("expected an error"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(&format!(
                "model data manifest is not valid JSON: ENOENT: no such file or directory, open '{}'",
                manifest_path.display()
            )),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn validation_wraps_malformed_json_as_invalid_json() {
        let mut fixture = Fixture::new();
        fixture.write_fixture_data(MODEL_DATA_SCHEMA_VERSION, "openai-completions");
        let manifest_path = fixture.data_dir().join(MODEL_DATA_MANIFEST_FILE);
        write_file(&manifest_path, "{\"schemaVersion\":}");
        let error = match validate_model_data_directory(&fixture.structure, fixture.data_dir()) {
            Ok(()) => panic!("expected an error"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("model data manifest is not valid JSON: Unexpected token '}'"),
            "unexpected message: {error}"
        );
    }
}

// ---------------------------------------------------------------------------
// 审查修复：Node fs 错误文本覆盖所有读写路径，readFileSync("utf8") 对无效字节
// 做 U+FFFD 替换而不是报错。
// ---------------------------------------------------------------------------

mod filesystem_error_text {
    use kiana_ai::model_catalog::hydrate::create_staging_dir;
    use kiana_ai::model_catalog::model_data::{
        read_model_data_provider_ids, read_model_data_structure,
    };

    use super::*;

    #[test]
    fn read_model_data_provider_ids_reports_enoent_open() {
        let root = TempDir::new("kiana-model-data-missing-agg-");
        let package_root = root.path().join("package");
        let path = package_root.join("src/models.generated.ts");
        let error = read_model_data_provider_ids(&package_root)
            .expect_err("missing aggregator")
            .to_string();
        assert_eq!(
            error,
            format!(
                "ENOENT: no such file or directory, open '{}'",
                path.display()
            )
        );
    }

    #[test]
    fn read_model_data_structure_reports_enoent_scandir() {
        let root = TempDir::new("kiana-model-data-missing-providers-");
        let package_root = root.path().join("package");
        write_file(
            &package_root.join("src/models.generated.ts"),
            "import { TEST_PROVIDER_CLASSIFIER_MODELS, TEST_PROVIDER_IMAGE_MODELS, TEST_PROVIDER_MODELS } from \"./providers/test-provider.models.ts\";\n",
        );
        let providers_dir = package_root.join("src/providers");
        let error = read_model_data_structure(&package_root)
            .expect_err("missing providers")
            .to_string();
        assert_eq!(
            error,
            format!(
                "ENOENT: no such file or directory, scandir '{}'",
                providers_dir.display()
            )
        );
    }

    #[test]
    fn create_staging_dir_reports_enoent_mkdtemp_with_the_template() {
        let root = TempDir::new("kiana-model-data-missing-staging-");
        let providers_dir = root.path().join("package/src/providers");
        let error = create_staging_dir(&providers_dir, ".model-hydration-")
            .expect_err("missing providers dir")
            .to_string();
        assert_eq!(
            error,
            format!(
                "ENOENT: no such file or directory, mkdtemp '{}'",
                providers_dir.join(".model-hydration-XXXXXX").display()
            )
        );
    }

    #[test]
    fn reading_a_directory_reports_the_pathless_eisdir() {
        let fixture = Fixture::new();
        let provider_path = fixture.data_dir().join("test-provider.json");
        std::fs::remove_file(&provider_path).expect("remove provider file");
        std::fs::create_dir(&provider_path).expect("create directory in its place");
        let error = validate_model_data_directory(&fixture.structure, fixture.data_dir())
            .expect_err("reading a directory fails");
        assert_eq!(
            error.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );
    }

    #[test]
    fn invalid_utf8_is_replaced_with_u_fffd_like_node() {
        let fixture = Fixture::new();
        let provider_path = fixture.data_dir().join("test-provider.json");
        let mut bytes = br#"{"openai-completions":{"chat:model-a":{"type":"chat","id":"model-a","api":"openai-completions","name":"caf"#.to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(br#""}}}"#);
        std::fs::write(&provider_path, bytes).expect("write invalid utf8");
        let structure =
            read_model_data_structure(fixture.package_root()).expect("lossy read succeeds");
        assert_eq!(structure, fixture.structure);

        // 校验不再因为 UTF-8 报错；内容变化只表现为 manifest 哈希不匹配。
        let error = validate_model_data_directory(&fixture.structure, fixture.data_dir())
            .expect_err("hash mismatch");
        assert!(
            !error
                .to_string()
                .contains("stream did not contain valid UTF-8"),
            "unexpected message: {error}"
        );
    }
}
