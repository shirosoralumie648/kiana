//! 模型目录的编译期产物（`ARCHITECTURE.md` §8 第 5、21 条；`spec/ai-core.md` §2.3.1/§4.1）。
//!
//! TS 侧的模型目录是两段生成物：
//!
//! 1. `scripts/generate-models.ts --strict --data-only` 写 `providers/data/<provider>.json`
//!    （git-ignored 的 hydrate 产物），每个文件两层：`api -> "type:id" -> 模型对象`；
//! 2. 每个 `<provider>.models.ts` 用 `flattenChatModelCatalog()` 把它摊平成 `{[id]: model}`，
//!    `models.generated.ts` 再把 42 个分片聚合成 `MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS`。
//!
//! Rust 侧不手写 42 份重复代码，也不在运行期读盘：本脚本在编译期把同一批 JSON 摊平成
//! TS 那三张表的形状，写成 `$OUT_DIR/model_catalog.json`（按 provider 顺序、组内按摊平顺序），
//! 由 `src/providers/catalog.rs` 用 `include_str!` 引入并解析成 `IndexMap` 静态表。
//! `serde_json` 在工作区开启了 `preserve_order`，所以 JSON 的键序就是 `IndexMap` 的键序，
//! 生成器写入的顺序即最终查询顺序。
//!
//! 目录选择顺序（§8 第 21 条）：
//!
//! 1. `KIANA_MODEL_DATA_DIR`（若设置）——指向另一份同结构目录；
//! 2. 仓库内 `crates/kiana-ai/data/`；
//! 3. 两者都缺 → `panic!`（等价 TS 的 `check:model-data` 门禁；**不要**生成空表静默通过）。
//!
//! 校验（等价 `scripts/model-data.ts` 的 `validateModelDataDirectory` 的编译期部分）：
//! 每个 `<provider>.json` 的字节 sha256 与 `.manifest.json` 逐一对过；manifest 的 `schemaVersion`、
//! `structureHash`（把每个文件重新摊平成 `{provider: {"type:id": api}}`，两侧键都按码元序排序后
//! `JSON.stringify` 再 sha256）、`files` 键集合都必须吻合；每个模型对象再过 `validateModelValue`
//! 的全部字段检查（错误按 TS 的 `Invalid generated model data:` 汇总格式一次性抛出）。
//! `generatedAt` 非法在 TS 的运行时 getter（`getBuiltinModelDataGeneratedAt()`）里得到
//! `undefined`，Rust 侧沿用该语义输出 `null`（TS 的 `check:model-data` 另有独立校验）。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// `.manifest.json` 允许的 schema 版本（`MODEL_DATA_SCHEMA_VERSION`）。
const MODEL_DATA_SCHEMA_VERSION: i64 = 6;

/// 42 个 provider 的**固定顺序**：`spec/ai-core.md` §2.3.1 == `models.generated.ts` 的 import 顺序
/// == `Object.keys(MODELS)`。`.manifest.json` 的 `files` 是**码元序**，不是这个顺序
/// （`google-vertex` 排在 `google` 前、`minimax-cn` 排在 `minimax` 前……），所以顺序以本表为准。
const PROVIDER_ORDER: &[&str] = &[
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "azure",
    "baseten",
    "cerebras",
    "cloudflare-ai-gateway",
    "cloudflare-workers-ai",
    "deepseek",
    "fireworks",
    "github-copilot",
    "google",
    "google-vertex",
    "groq",
    "huggingface",
    "kimi-coding",
    "meta",
    "minimax",
    "minimax-cn",
    "mistral",
    "moonshotai",
    "moonshotai-cn",
    "nvidia",
    "openai",
    "openai-codex",
    "opencode",
    "opencode-go",
    "openrouter",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "radius",
    "together",
    "typesafe",
    "vercel-ai-gateway",
    "xai",
    "xiaomi",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-sgp",
    "zai",
    "zai-coding-cn",
];

fn main() {
    println!("cargo:rerun-if-env-changed=KIANA_MODEL_DATA_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    let data_dir = resolve_data_dir();
    let manifest_path = data_dir.join(".manifest.json");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest_bytes = std::fs::read(&manifest_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", manifest_path.display()));
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .unwrap_or_else(|error| panic!("{} is not valid JSON: {error}", manifest_path.display()));

    let schema_version = manifest.get("schemaVersion").and_then(Value::as_i64);
    if schema_version != Some(MODEL_DATA_SCHEMA_VERSION) {
        panic!(
            "model data schema is {}, expected {MODEL_DATA_SCHEMA_VERSION}",
            js_json_stringify(manifest.get("schemaVersion"))
        );
    }

    let manifest_files = manifest
        .get("files")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("model data manifest has no file hashes"));
    // 顺序以 `PROVIDER_ORDER` 为准；manifest 必须恰好覆盖这 42 个文件（哈希逐个取出）。
    let provider_files: Vec<(String, String)> = PROVIDER_ORDER
        .iter()
        .map(|provider| {
            let filename = format!("{provider}.json");
            let hash = manifest_files.get(&filename).unwrap_or_else(|| {
                panic!("{} has no hash for {filename}", manifest_path.display())
            });
            let hash = hash
                .as_str()
                .unwrap_or_else(|| panic!("manifest hash for {filename} is not a string"));
            (filename, hash.to_string())
        })
        .collect();

    // manifest 的键集合必须与目录里的数据文件集合完全一致（多一个少一个都报错）。
    let mut on_disk: Vec<String> = std::fs::read_dir(&data_dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", data_dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json") && name != ".manifest.json")
        .collect();
    on_disk.sort();
    let mut expected: Vec<String> = provider_files
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    expected.sort();
    if on_disk != expected {
        panic!(
            "provider data files do not match the generated catalog ({})",
            describe_set_difference(&expected, &on_disk)
        );
    }
    // manifest 里不能有 `PROVIDER_ORDER` 之外的条目（否则 `structureHash` 会覆盖不到它们）。
    if manifest_files.len() != PROVIDER_ORDER.len() {
        let mut manifest_names: Vec<String> = manifest_files.keys().cloned().collect();
        manifest_names.sort();
        panic!(
            "manifest file hashes do not match provider data files ({})",
            describe_set_difference(&expected, &manifest_names)
        );
    }

    // 三张表：provider -> (模型 id -> 模型对象)。空表也要有该 provider 的键，与 TS 的分片常量一致。
    let mut chat: Map<String, Value> = Map::new();
    let mut image: Map<String, Value> = Map::new();
    let mut classifier: Map<String, Value> = Map::new();
    let mut structure: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // `throwValidationErrors`：所有模型字段错误先收集，最后一次性按 TS 的格式抛出。
    let mut model_errors: Vec<String> = Vec::new();

    for (filename, expected_hash) in &provider_files {
        let provider_id = filename
            .strip_suffix(".json")
            .unwrap_or_else(|| panic!("provider data file {filename} does not end in .json"));
        let path = data_dir.join(filename);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

        // sha256 用原始字节：TS 的 `sha256(readFileSync(path, "utf8"))` 对 UTF-8 文本与字节一一对应。
        let actual_hash = hex_sha256(&bytes);
        if &actual_hash != expected_hash {
            panic!("{filename} does not match its manifest hash");
        }

        let groups: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("{filename} is not valid JSON: {error}"));
        let groups = groups
            .as_object()
            .unwrap_or_else(|| panic!("{filename} must contain a JSON object"));

        // `flattenModelCatalog(groups, type)`：按 API 组顺序摊平，只保留 type 匹配的项，
        // 以 `model.id` 为键（同 id 后者覆盖前者，`Object.fromEntries` 语义）。
        let mut chat_models: Map<String, Value> = Map::new();
        let mut image_models: Map<String, Value> = Map::new();
        let mut classifier_models: Map<String, Value> = Map::new();
        let mut model_apis: BTreeMap<String, String> = BTreeMap::new();

        for (api, models) in groups {
            let models = models.as_object().unwrap_or_else(|| {
                panic!("{filename} API group {} must be an object", quote_json(api))
            });
            for (model_key, model) in models {
                if model_apis.insert(model_key.clone(), api.clone()).is_some() {
                    // `validateModelDataDirectory` 的逐文件检查（不是 `readProviderStructure`）。
                    model_errors.push(format!(
                        "{provider_id}/{model_key} appears in more than one API group"
                    ));
                }
                validate_model_entry(model, provider_id, model_key, api, &mut model_errors);
                if let Value::Object(object) = model {
                    // `flatten*Catalog()` 只看 `type` 与 `id`；非法 type 已经进 `model_errors`，
                    // 这里跳过它的表项（有错误时整个构建会失败，不会产出残缺目录）。
                    let model_type = object.get("type").and_then(Value::as_str).unwrap_or("chat");
                    let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
                    match model_type {
                        "chat" => {
                            chat_models.insert(id.to_string(), model.clone());
                        }
                        "image" => {
                            image_models.insert(id.to_string(), model.clone());
                        }
                        "classifier" => {
                            classifier_models.insert(id.to_string(), model.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
        if model_apis.is_empty() {
            panic!("{} contains no generated model data", path.display());
        }
        structure.insert(provider_id.to_string(), model_apis);
        chat.insert(provider_id.to_string(), Value::Object(chat_models));
        image.insert(provider_id.to_string(), Value::Object(image_models));
        classifier.insert(provider_id.to_string(), Value::Object(classifier_models));
    }

    if !model_errors.is_empty() {
        throw_validation_errors(&model_errors);
    }

    // `structureHash` = sha256(JSON.stringify({provider: {"type:id": api}}))，两侧键按码元序升序。
    // `BTreeMap<String, _>` 的迭代序是 UTF-8 字节序；在合法字符串上与 UTF-16 码元序同序。
    let structure_json = stringify_structure(&structure);
    let expected_structure_hash = manifest.get("structureHash").and_then(Value::as_str);
    let actual_structure_hash = hex_sha256(structure_json.as_bytes());
    if expected_structure_hash != Some(actual_structure_hash.as_str()) {
        panic!(
            "model data generation stamp does not match the generated catalog (manifest {expected_structure_hash:?}, actual {actual_structure_hash})"
        );
    }

    // `getBuiltinModelDataGeneratedAt()`（`providers/all.ts`）：`Date.parse(generatedAt)`，NaN → `undefined`。
    // 时间戳非法**不是**编译错误（TS 运行时同样只得到 `undefined`），因此这里输出 `null`，
    // 由 `providers/catalog.rs` 还原成 `None`。
    let generated_at_ms = manifest
        .get("generatedAt")
        .and_then(Value::as_str)
        .and_then(parse_iso8601_ms);

    // 输出：三个表 + 时间戳 + provider 顺序。键顺序即 TS 的分片/聚合顺序。
    let mut document = Map::new();
    document.insert(
        "generatedAt".to_string(),
        generated_at_ms.map_or(Value::Null, Value::from),
    );
    document.insert(
        "providers".to_string(),
        Value::Array(
            provider_files
                .iter()
                .map(|(filename, _)| Value::String(filename.trim_end_matches(".json").to_string()))
                .collect(),
        ),
    );
    document.insert("models".to_string(), Value::Object(chat));
    document.insert("imageModels".to_string(), Value::Object(image));
    document.insert("classifierModels".to_string(), Value::Object(classifier));

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let out_path = out_dir.join("model_catalog.json");
    let text =
        serde_json::to_string(&Value::Object(document)).expect("catalog document is serializable");
    std::fs::write(&out_path, text)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", out_path.display()));
}

/// 目录选择顺序：`KIANA_MODEL_DATA_DIR` → 仓库内 `crates/kiana-ai/data/` → `panic!`。
fn resolve_data_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("KIANA_MODEL_DATA_DIR").filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(&value);
        if !path.is_dir() {
            panic!(
                "KIANA_MODEL_DATA_DIR does not point to a directory: {}",
                path.display()
            );
        }
        return path;
    }
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let vendored = manifest_dir.join("data");
    if vendored.is_dir() {
        return vendored;
    }
    panic!(
        "generated model data is missing: set KIANA_MODEL_DATA_DIR to a directory holding the 42 \
         <provider>.json files plus .manifest.json, or restore {} (ARCHITECTURE.md §8 items 5/21; \
         in the TypeScript repo run `npm run hydrate-model-data`)",
        vendored.display()
    );
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `JSON.stringify` 对「字符串→字符串」两层对象的紧凑输出（键已按码元序排好）。
///
/// 走 `serde_json` 保证与 TS 相同的字符串转义（`\"`、`\\`、`\u00XX`）；`structureHash` 的输入
/// 全是 provider id、`type:id` 与 api id，字符集是 `[a-z0-9:./-]`。
fn stringify_structure(structure: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let value = Value::Object(
        structure
            .iter()
            .map(|(provider, models)| {
                (
                    provider.clone(),
                    Value::Object(
                        models
                            .iter()
                            .map(|(key, api)| (key.clone(), Value::String(api.clone())))
                            .collect(),
                    ),
                )
            })
            .collect(),
    );
    serde_json::to_string(&value).expect("structure is serializable")
}

/// `validateModelDataDirectory` 的逐模型检查：`validateModelValue` 的字段错误，加上
/// `modelKey !== `${String(model.type)}:${String(model.id)}`` 的身份错误（TS 在目录检查里做）。
pub(crate) fn validate_model_entry(
    value: &Value,
    provider_id: &str,
    model_key: &str,
    expected_api: &str,
    errors: &mut Vec<String>,
) {
    // `modelId` 是 `type:id` 键里冒号之后的部分（没有冒号就是整个键）。
    let model_id = model_key.split_once(':').map_or(model_key, |(_, id)| id);
    validate_model_value(value, provider_id, model_id, expected_api, errors);
    if let Value::Object(object) = value {
        let identity = format!(
            "{}:{}",
            js_string(object.get("type")),
            js_string(object.get("id"))
        );
        if model_key != identity {
            errors.push(format!(
                "{provider_id}/{model_key} has mismatched type/id identity"
            ));
        }
    }
}

/// `validateModelValue`（`scripts/model-data.ts`）：把字段错误逐条追加到 `errors`，错误文本
/// 是当前稳定格式（`${providerId}/${modelId} ...`）。`model_id` 是 `type:id` 键里冒号之后的部分
/// （调用方负责切分）。
pub(crate) fn validate_model_value(
    value: &Value,
    provider_id: &str,
    model_id: &str,
    expected_api: &str,
    errors: &mut Vec<String>,
) {
    let label = format!("{provider_id}/{model_id}");
    let Value::Object(object) = value else {
        errors.push(format!("{label} must be an object"));
        return;
    };
    if object.get("id").and_then(Value::as_str) != Some(model_id) {
        errors.push(format!(
            "{label} has id {}, expected {}",
            js_json_stringify(object.get("id")),
            quote_json(model_id)
        ));
    }
    if object.get("provider").and_then(Value::as_str) != Some(provider_id) {
        errors.push(format!(
            "{label} has provider {}, expected {}",
            js_json_stringify(object.get("provider")),
            quote_json(provider_id)
        ));
    }
    if object.get("api").and_then(Value::as_str) != Some(expected_api) {
        errors.push(format!(
            "{label} has api {}, expected {}",
            js_json_stringify(object.get("api")),
            quote_json(expected_api)
        ));
    }
    match object.get("name") {
        Some(Value::String(name)) if !name.is_empty() => {}
        _ => errors.push(format!("{label} has no model name")),
    }
    if !matches!(object.get("baseUrl"), Some(Value::String(_))) {
        errors.push(format!("{label} has no baseUrl string"));
    }
    if !is_modality_list(object.get("input")) {
        errors.push(format!("{label} has invalid input modalities"));
    }
    let model_type = object.get("type");
    if model_type.and_then(Value::as_str) == Some("image") {
        let output = object.get("output");
        let has_image = output
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("image")));
        if !is_modality_list(output) || !has_image {
            errors.push(format!("{label} has invalid output modalities"));
        }
    } else if object.contains_key("output") {
        errors.push(format!("{label} has unsupported output modalities"));
    }
    match model_type.and_then(Value::as_str) {
        Some("image") => {}
        Some("chat") => {
            if !matches!(object.get("reasoning"), Some(Value::Bool(_))) {
                errors.push(format!("{label} has no reasoning boolean"));
            }
            if !is_positive_number(object.get("contextWindow")) {
                errors.push(format!("{label} has invalid contextWindow"));
            }
            if !is_positive_number(object.get("maxTokens")) {
                errors.push(format!("{label} has invalid maxTokens"));
            }
        }
        Some("classifier") => {
            if !is_positive_number(object.get("contextWindow")) {
                errors.push(format!("{label} has invalid contextWindow"));
            }
        }
        _ => errors.push(format!(
            "{label} has type {}, expected \"chat\", \"image\", or \"classifier\"",
            js_json_stringify(model_type)
        )),
    }
    match object.get("cost") {
        Some(Value::Object(cost)) => {
            for field in ["input", "output", "cacheRead", "cacheWrite"] {
                if !matches!(cost.get(field), Some(Value::Number(_))) {
                    errors.push(format!("{label} has invalid cost.{field}"));
                }
            }
        }
        _ => errors.push(format!("{label} has invalid cost metadata")),
    }
}

/// `isModalityList`：非空数组且每项都是 `"text"` / `"image"`。
fn is_modality_list(value: Option<&Value>) -> bool {
    matches!(
        value,
        Some(Value::Array(items)) if !items.is_empty()
            && items
                .iter()
                .all(|item| matches!(item.as_str(), Some("text" | "image")))
    )
}

/// JS `typeof value === "number" && Number.isFinite(value) && value > 0`。
fn is_positive_number(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|number| number > 0.0)
}

/// `throwValidationErrors`：只显示前 30 条，多余追加 `\n  ... and N more`。
pub(crate) fn throw_validation_errors(errors: &[String]) -> ! {
    let mut message = String::from("Invalid generated model data:\n");
    for error in errors.iter().take(30) {
        let _ = writeln!(message, "  - {error}");
    }
    message.pop();
    if errors.len() > 30 {
        let _ = write!(message, "\n  ... and {} more", errors.len() - 30);
    }
    panic!("{message}");
}

/// JS `JSON.stringify(value)`（`undefined` 用 `None` 表示）。
fn js_json_stringify(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        Some(Value::Number(number)) => js_number(number),
        Some(Value::String(string)) => quote_json(string),
        Some(Value::Array(items)) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| js_json_stringify(Some(item)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(Value::Object(map)) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    quote_json(key),
                    js_json_stringify(Some(value))
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// JS `String(value)`（`undefined` 用 `None` 表示），用于 `type:id` 身份比较。
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(boolean)) => boolean.to_string(),
        Some(Value::Number(number)) => js_number(number),
        Some(Value::String(string)) => string.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// `JSON.stringify`/`String` 里的数字格式：整数不带 `.0`，其余用最短十进制表示。
fn js_number(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_i64() {
        return value.to_string();
    }
    if let Some(value) = number.as_u64() {
        return value.to_string();
    }
    let value = number.as_f64().unwrap_or(0.0);
    if value == 0.0 {
        // `-0` 在两种 JS 转换里都是 "0"。
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        return format!("{value:.0}");
    }
    format!("{value}")
}

/// `JSON.stringify` 的字符串字面量（serde_json 与 V8 的转义规则一致）。
fn quote_json(value: &str) -> String {
    serde_json::to_string(value).expect("strings are serializable")
}

/// `describeSetDifference`（`scripts/model-data.ts`）：`missing: ...; extra: ...`。
fn describe_set_difference(expected: &[String], actual: &[String]) -> String {
    let missing: Vec<&str> = expected
        .iter()
        .filter(|value| !actual.contains(value))
        .map(String::as_str)
        .collect();
    let extra: Vec<&str> = actual
        .iter()
        .filter(|value| !expected.contains(value))
        .map(String::as_str)
        .collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing: {}", missing.join(", ")));
    }
    if !extra.is_empty() {
        parts.push(format!("extra: {}", extra.join(", ")));
    }
    parts.join("; ")
}

/// `Date.parse` 的等价实现：ECMAScript 的 Date Time String Format（Node 26 / V8 实测量准）。
///
/// 接受 `YYYY`、`YYYY-MM`、`YYYY-MM-DD`（这些是 UTC 零点）；可选的 `T`/`t`/空格后跟
/// `HH:mm`、可选的 `:ss`、可选的 `.fraction`、可选的偏移 `Z`/`z`/`±HH:mm`/`±HHmm`；
/// 扩展年份 `±YYYYYY`。没有偏移的日期时间按**本地时区**解释（与 `Date.parse` 一致）。
/// 小数秒任意长度、截断到毫秒；月 1–12、日 1–31（月内溢出顺延）、时 0–24（24 点只允许
/// 零分/秒/毫秒）、分/秒 0–59、偏移小时 0–23/分钟 0–59。超出 `Date` 的 ±8.64e15 ms 范围返回 `None`。
///
/// 返回 `None` 等价于 `Number.isNaN(Date.parse(...))`（TS 此时把 `generatedAt` 当 `undefined`）。
/// V8 的 legacy 解析（`"Oct 26 2026"`、`"2026/10/26"`、个位数月/日等非 ISO 形式）不在这里
/// 实现；空格分隔的日期时间按 ISO 规则处理——与 V8 对常规年份/合法偏移的结果一致，
/// 但 V8 会把这些输入交给 legacy parser 的两类边角（年份 < 100、`+24:00` 偏移）不保证相同。
/// `generate-models.ts` 写出的 `generatedAt` 是 `toISOString()` 的 `T...Z` 形式，不走这些路径。
pub(crate) fn parse_iso8601_ms(value: &str) -> Option<i64> {
    if !value.is_ascii() {
        return None;
    }
    let bytes = value.as_bytes();
    fn digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
        let slice = bytes.get(start..start.checked_add(len)?)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(
            slice
                .iter()
                .fold(0i64, |acc, digit| acc * 10 + i64::from(digit - b'0')),
        )
    }

    let mut index;
    let year = if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        let negative = bytes[0] == b'-';
        let year = digits(bytes, 1, 6)?;
        // V8：`-000000` 是 NaN（负零年份非法），`+000000` 合法。
        if negative && year == 0 {
            return None;
        }
        index = 7;
        if negative { -year } else { year }
    } else {
        let year = digits(bytes, 0, 4)?;
        index = 4;
        year
    };

    let mut month = 1i64;
    let mut day = 1i64;
    if bytes.get(index) == Some(&b'-') {
        month = digits(bytes, index + 1, 2)?;
        if !(1..=12).contains(&month) {
            return None;
        }
        index += 3;
        if bytes.get(index) == Some(&b'-') {
            day = digits(bytes, index + 1, 2)?;
            if !(1..=31).contains(&day) {
                return None;
            }
            index += 3;
        }
    }

    // 只有日期（或日期 + `Z`）是 UTC 零点；带时间但没有偏移时是本地时间。
    let date_only = index == bytes.len()
        || (matches!(bytes.get(index), Some(b'Z') | Some(b'z')) && index + 1 == bytes.len());
    let (hour, minute, second, millis, offset_minutes) = if date_only {
        (0, 0, 0, 0, Some(0))
    } else if matches!(bytes.get(index), Some(b'T') | Some(b't') | Some(b' ')) {
        index += 1;
        let hour = digits(bytes, index, 2)?;
        index += 2;
        if bytes.get(index) != Some(&b':') {
            return None;
        }
        index += 1;
        let minute = digits(bytes, index, 2)?;
        index += 2;
        let mut second = 0;
        let mut millis = 0;
        if bytes.get(index) == Some(&b':') {
            index += 1;
            second = digits(bytes, index, 2)?;
            index += 2;
            if bytes.get(index) == Some(&b'.') {
                index += 1;
                let start = index;
                while matches!(bytes.get(index), Some(digit) if digit.is_ascii_digit()) {
                    index += 1;
                }
                if index == start {
                    return None;
                }
                // 小数秒按十进制小数处理：`.7` → 700ms，`.7777` → 777ms（截断）。
                for position in 0..3 {
                    millis *= 10;
                    if start + position < index {
                        millis += i64::from(bytes[start + position] - b'0');
                    }
                }
            }
        }
        let offset_minutes = if matches!(bytes.get(index), Some(b'Z') | Some(b'z')) {
            index += 1;
            Some(0)
        } else if matches!(bytes.get(index), Some(b'+') | Some(b'-')) {
            let negative = bytes[index] == b'-';
            index += 1;
            let offset_hour = digits(bytes, index, 2)?;
            index += 2;
            if bytes.get(index) == Some(&b':') {
                index += 1;
            }
            let offset_minute = digits(bytes, index, 2)?;
            index += 2;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let total = offset_hour * 60 + offset_minute;
            Some(if negative { -total } else { total })
        } else {
            None
        };
        if index != bytes.len() {
            return None;
        }
        (hour, minute, second, millis, offset_minutes)
    } else {
        return None;
    };

    if hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    // `24:00:00.000` 等价次日零点；分/秒/毫秒非零则是 NaN。
    if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    match offset_minutes {
        Some(offset) => {
            let ms = days
                .checked_mul(86_400_000)?
                .checked_add(hour * 3_600_000 + minute * 60_000 + second * 1000 + millis)?
                .checked_sub(offset * 60_000)?;
            clip_date_value(ms)
        }
        None => local_time_ms(days, hour, minute, second, millis),
    }
}

/// `Date` 的取值范围（`TimeClip`）：±8.64e15 ms（含端点）。
fn clip_date_value(ms: i64) -> Option<i64> {
    const MAX_DATE_MS: i64 = 8_640_000_000_000_000;
    (-MAX_DATE_MS..=MAX_DATE_MS).contains(&ms).then_some(ms)
}

/// 把“本地时间”的民用日期换算成 Unix 毫秒（`Date.parse` 对无偏移时间的行为）。
fn local_time_ms(days: i64, hour: i64, minute: i64, second: i64, millis: i64) -> Option<i64> {
    use chrono::{Local, LocalResult, NaiveDate, NaiveTime, TimeZone};

    // 24:00 已校验为零分/秒/毫秒，等价次日零点。
    let (days, hour) = if hour == 24 {
        (days + 1, 0)
    } else {
        (days, hour)
    };
    let (year, month, day) = civil_from_days(days);
    let date = NaiveDate::from_ymd_opt(
        i32::try_from(year).ok()?,
        u32::try_from(month).ok()?,
        u32::try_from(day).ok()?,
    )?;
    let time = NaiveTime::from_hms_milli_opt(
        u32::try_from(hour).ok()?,
        u32::try_from(minute).ok()?,
        u32::try_from(second).ok()?,
        u32::try_from(millis).ok()?,
    )?;
    let naive = date.and_time(time);
    let timestamp_ms = match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => value.timestamp_millis(),
        // 秋季回拨的歧义时间：V8 取较早的那次（夏令时偏移）。
        LocalResult::Ambiguous(earliest, _) => earliest.timestamp_millis(),
        LocalResult::None => {
            // 春季跳变的“不存在”时间：V8 沿用跳变前的偏移量（实测 02:30 → 03:30 的瞬间）。
            let before = naive - chrono::Duration::hours(2);
            let LocalResult::Single(value) = Local.from_local_datetime(&before) else {
                return None;
            };
            let offset = chrono::Offset::fix(value.offset());
            let utc = naive - offset;
            utc.and_utc().timestamp_millis()
        }
    };
    clip_date_value(timestamp_ms)
}

/// `days_from_civil` 的逆变换（Howard Hinnant），返回 (year, month, day)。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `Date.UTC` 的日序号换算（Howard Hinnant 的 `days_from_civil`，proleptic Gregorian）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year; // [0, 146096]
    era * 146_097 + day_of_era - 719_468
}
