//! 目录工具测试的共享夹具：读 `tests/fixtures/catalog-tooling` 与仓库内 `crates/kiana-ai/data`。
//!
//! TS 侧的测试各自 `spawnSync(node scripts/generate-models.ts)`；Rust 侧把这些脚本当作库函数
//! 直接调用（见 `crates/kiana-ai/src/model_catalog.rs`），所以这里只提供「取上游 JSON 快照」与
//! 「临时目录」两件事。

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// `tests/fixtures/catalog-tooling` 的绝对路径。
pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("catalog-tooling")
}

/// 读 `tests/fixtures/catalog-tooling/inputs/<name>`。
pub fn read_input(name: &str) -> serde_json::Value {
    let path = fixtures_root().join("inputs").join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// 读 `tests/fixtures/catalog-tooling/golden/<name>` 的原始文本。
pub fn read_golden(name: &str) -> String {
    let path = fixtures_root().join("golden").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// 用 `generate_catalog` 的输入装配 [`GeneratorInputs`]。
pub fn golden_inputs(package_root: &Path) -> kiana_ai::model_catalog::generator::GeneratorInputs {
    use kiana_ai::model_catalog::generator::GeneratorInputs;
    let listed = |name: &str| -> Vec<serde_json::Value> {
        read_input(name)
            .get("data")
            .and_then(|data| data.as_array())
            .cloned()
            .unwrap_or_default()
    };
    GeneratorInputs {
        package_root: package_root.to_path_buf(),
        models_dev: Some(read_input("models-dev.json")),
        models_dev_decision: Some(read_input("models-dev-decision.json")),
        open_router_listed: listed("openrouter.json"),
        open_router_image_listed: listed("openrouter-image.json"),
        open_router_decision_listed: listed("openrouter-decisions.json"),
        ai_gateway: Some(read_input("ai-gateway.json")),
        nvidia_nim: Some(read_input("nvidia.json")),
        radius_config: Some(read_input("radius.json")),
        fetch_errors: Default::default(),
    }
}

/// 临时目录（drop 时递归删除）。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 新建 `kiana-model-data-<random>`。
    pub fn new(prefix: &str) -> TempDir {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).expect("random");
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("{prefix}{suffix}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 写文件（自动建父目录）。
pub fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// `expect.objectContaining` 语义：`expected` 的每个键都在 `actual` 中且递归匹配。
///
/// 数组按 `toEqual` 比较（长度与逐项），标量直接相等。
pub fn assert_contains(actual: &serde_json::Value, expected: &serde_json::Value, path: &str) {
    match (actual, expected) {
        (serde_json::Value::Object(actual), serde_json::Value::Object(expected)) => {
            for (key, expected_value) in expected {
                let actual_value = actual.get(key).unwrap_or_else(|| {
                    panic!(
                        "{path}: missing key {key:?} in {}",
                        serde_json::Value::Object(actual.clone())
                    )
                });
                assert_contains(actual_value, expected_value, &format!("{path}.{key}"));
            }
        }
        (serde_json::Value::Array(actual), serde_json::Value::Array(expected)) => {
            assert_eq!(actual.len(), expected.len(), "{path}: array length");
            for (index, (actual_item, expected_item)) in actual.iter().zip(expected).enumerate() {
                assert_contains(actual_item, expected_item, &format!("{path}[{index}]"));
            }
        }
        _ => assert_eq!(js(actual), js(expected), "{path}"),
    }
}

/// `JSON.stringify(value)`（JS 数字格式化）；用于比较 `1` 与 `1.0` 等 JSON 表示。
pub fn js(value: &serde_json::Value) -> String {
    kiana_ai::model_catalog::generator::serialize_json(value, false)
}

/// `expect(a).toEqual(b)`：按 JS JSON 文本比较（`1` 与 `1.0` 视为相同）。
pub fn assert_js_eq(actual: &serde_json::Value, expected: &serde_json::Value, label: &str) {
    assert_eq!(js(actual), js(expected), "{label}");
}

/// 仓库内 vendored 的生成数据目录：`crates/kiana-ai/data`。
pub fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data")
}

/// 读 `crates/kiana-ai/data/<name>`。
pub fn read_data(name: &str) -> serde_json::Value {
    let path = data_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// 把 `crates/kiana-ai/data/<name>` 的 `api -> "type:id" -> model` 结构转成 [`ModelGroups`]。
pub fn data_groups(name: &str) -> kiana_ai::model_catalog::ModelGroups<serde_json::Value> {
    let value = read_data(name);
    let mut groups = kiana_ai::model_catalog::ModelGroups::new();
    for (api, models) in value.as_object().expect("api groups") {
        let group = models
            .as_object()
            .expect("models")
            .iter()
            .map(|(key, model)| (key.clone(), model.clone()))
            .collect();
        groups.insert(api.clone(), group);
    }
    groups
}
