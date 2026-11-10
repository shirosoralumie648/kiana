//! 移植 `packages/ai/test/generate-models-strict.test.ts`，并把整条生成管线与 TS 的
//! `--json-only --json-output` 黄金输出逐字节比对。
//!
//! TS 侧第一条测试会 `cpSync` 整个 `packages/ai` 到临时目录（含 1.1 MiB 生成数据）再跑
//! `node scripts/generate-models.ts --strict`。Rust 侧直接在临时 `packageRoot` 上调用
//! [`generate_catalog`]（strict 门禁与失败前的原子性由 `generate_models` 承担），断言同一段
//! 错误文本，并断言已有生成物未被改写。

mod model_data_support;

use std::path::Path;

use kiana_ai::model_catalog::generator::{
    GeneratorInputs, GeneratorOptions, GeneratorState, SOURCE_AI_GATEWAY, SOURCE_MODELS_DEV,
    SOURCE_MODELS_DEV_CLASSIFIER, SOURCE_OPEN_ROUTER, SOURCE_RADIUS, generate_catalog,
    generate_models, read_generator_options,
};
use model_data_support::{TempDir, golden_inputs, read_golden, read_input, write_file};
use serde_json::{Value, json};

/// TS 的 `modelIds`（`generate-models-strict.test.ts`）。
const INDIVIDUAL_MODEL_IDS: [&str; 10] = [
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "glm-5.2",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
    "qwen3.8-max-preview",
];

fn individual_models_dev(all_tool_capable: bool) -> Value {
    let mut models = serde_json::Map::new();
    for id in INDIVIDUAL_MODEL_IDS {
        models.insert(
            id.to_string(),
            json!({
                "id": id,
                "name": id,
                // 默认让 `deepseek-v4-flash-0731` 失去 tool support（strict 用例），
                // 其余用例要求全部 10 个 id 都被生成。
                "tool_call": all_tool_capable || id != "deepseek-v4-flash-0731",
            }),
        );
    }
    let mut provider = serde_json::Map::new();
    provider.insert("models".to_string(), Value::Object(models));
    let mut models_dev = serde_json::Map::new();
    models_dev.insert("alibaba-token-plan".to_string(), Value::Object(provider));
    Value::Object(models_dev)
}

fn strict_inputs(package_root: &Path) -> GeneratorInputs {
    GeneratorInputs {
        package_root: package_root.to_path_buf(),
        models_dev: Some(individual_models_dev(false)),
        ..Default::default()
    }
}

/// strict 模式能走完 `assertExactModelIds` 的输入（10 个 id 全部 tool-capable）。
fn strict_ready_inputs(package_root: &Path) -> GeneratorInputs {
    GeneratorInputs {
        package_root: package_root.to_path_buf(),
        models_dev: Some(individual_models_dev(true)),
        ..Default::default()
    }
}

#[test]
fn fails_before_mutating_generated_data_when_an_individual_model_loses_tool_support() {
    let root = TempDir::new("kiana-generate-models-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
    write_file(
        &package_root.join("src/models.generated.ts"),
        "import { QWEN_TOKEN_PLAN_INDIVIDUAL_CLASSIFIER_MODELS, QWEN_TOKEN_PLAN_INDIVIDUAL_IMAGE_MODELS, QWEN_TOKEN_PLAN_INDIVIDUAL_MODELS } from \"./providers/qwen-token-plan-individual.models.ts\";\n",
    );
    write_file(
        &package_root.join("src/providers/qwen-token-plan-individual.models.ts"),
        "// existing shard\n",
    );
    let generated_paths = [
        "src/models.generated.ts",
        "src/providers/qwen-token-plan-individual.models.ts",
        "src/providers/data/qwen-token-plan-individual.json",
        "src/providers/data/.manifest.json",
    ];
    for path in generated_paths {
        write_file(&package_root.join(path), "before\n");
    }
    let before: Vec<String> = generated_paths
        .iter()
        .map(|path| std::fs::read_to_string(package_root.join(path)).expect("read"))
        .collect();

    let inputs = strict_inputs(&package_root);
    let options = GeneratorOptions {
        strict: true,
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let error =
        generate_models(&inputs, &options, &mut state).expect_err("strict generation must fail");
    assert!(
        error.message.contains(
            "qwen-token-plan-individual model IDs do not match (missing: deepseek-v4-flash-0731)"
        ),
        "unexpected message: {}",
        error.message
    );

    let after: Vec<String> = generated_paths
        .iter()
        .map(|path| std::fs::read_to_string(package_root.join(path)).expect("read"))
        .collect();
    assert_eq!(after, before);
}

#[test]
fn non_strict_generation_ignores_the_missing_individual_model() {
    let root = TempDir::new("kiana-generate-models-lax-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
    write_file(
        &package_root.join("src/models.generated.ts"),
        "import { QWEN_TOKEN_PLAN_INDIVIDUAL_CLASSIFIER_MODELS, QWEN_TOKEN_PLAN_INDIVIDUAL_IMAGE_MODELS, QWEN_TOKEN_PLAN_INDIVIDUAL_MODELS } from \"./providers/qwen-token-plan-individual.models.ts\";\n",
    );
    let inputs = strict_inputs(&package_root);
    let mut state = GeneratorState::default();
    let catalog = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
        .expect("non-strict generation succeeds");
    let provider = catalog
        .providers
        .get("qwen-token-plan-individual")
        .expect("individual provider");
    assert!(!provider.chat.contains_key("deepseek-v4-flash-0731"));
    // 10 个 id 中 `qwen3.8-max-preview` 被排除、`deepseek-v4-flash-0731` 缺 `tool_call`。
    assert_eq!(provider.chat.len(), 8);
}

/// 整条管线与 TS 的 `--json-only --json-output` 黄金输出一致。
///
/// 比对分三层：
/// 1. `providers.json` 逐字节相同（provider id 的码元序排序）；
/// 2. 每个 provider 的 chat/all 目录**键序**相同（`localeCompare` 排序，这是语义相关的顺序）；
/// 3. 每个模型对象的字段与 **字段插入顺序**都相同，序列化后逐字节一致。
///
/// 第 3 点是刻意保留的：生成的数据文件是逐字节入库的产物（`.manifest.json` 存的是
/// 文件内容的 sha256），TS 直接 `JSON.stringify` 各自的对象字面量，所以模型内部字段序
/// 会进入哈希。Rust 侧用共享的 `build_chat_model` + metadata 补丁构造，必须靠
/// `reorder_model` 还原每个字面量的顺序。
#[test]
fn matches_the_typescript_json_catalog() {
    let root = TempDir::new("kiana-generate-models-golden-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
    let output_dir = root.path().join("catalog");
    let inputs = golden_inputs(&package_root);
    let options = GeneratorOptions {
        json_only: true,
        json_output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    generate_models(&inputs, &options, &mut state).expect("generation succeeds");

    // 1. providers.json 逐字节。
    assert_eq!(
        std::fs::read_to_string(output_dir.join("providers.json")).expect("providers.json"),
        read_golden("providers.json")
    );

    let provider_ids: Vec<String> =
        serde_json::from_str(&read_golden("providers.json")).expect("providers");
    let read_json = |path: &std::path::Path| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    };
    let models_json = read_json(&output_dir.join("models.json"));
    let models_all = read_json(&output_dir.join("models.all.json"));

    // 合并视图的 provider 键序 == `providers.json`（码元序）。
    assert_eq!(
        keys_of(&models_json),
        provider_ids,
        "models.json provider order"
    );
    assert_eq!(
        keys_of(&models_all),
        provider_ids,
        "models.all.json provider order"
    );

    // golden 里保存了 10 个 provider 的分片：键序 + 规范内容 + 合并视图逐项比对。
    let golden_shards = model_data_support::fixtures_root().join("golden/providers");
    let mut checked = 0;
    for entry in std::fs::read_dir(&golden_shards).expect("golden providers dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let provider_id = name
            .strip_suffix(".all.json")
            .or_else(|| name.strip_suffix(".json"))
            .expect("shard name")
            .to_string();
        let golden = read_json(&entry.path());
        let actual = read_json(&output_dir.join("providers").join(&name));

        if name.ends_with(".all.json") {
            // 数组：顺序（chat -> image -> classifier，组内 localeCompare）与内容。
            assert_eq!(
                model_ids(&actual),
                model_ids(&golden),
                "{name}: typed model order"
            );
            assert_eq!(
                canonical(&actual),
                canonical(&golden),
                "{name}: typed model content"
            );
            assert_eq!(
                model_field_orders(&actual),
                model_field_orders(&golden),
                "{name}: typed model field order"
            );
        } else {
            assert_eq!(
                keys_of(&actual),
                keys_of(&golden),
                "{name}: chat model order"
            );
            assert_eq!(
                canonical(&actual),
                canonical(&golden),
                "{name}: chat model content"
            );
            assert_eq!(
                model_field_orders(&actual),
                model_field_orders(&golden),
                "{name}: chat model field order"
            );
            assert_eq!(
                models_json
                    .get(&provider_id)
                    .cloned()
                    .unwrap_or(Value::Null),
                actual,
                "models.json entry for {provider_id}"
            );
        }
        if name.ends_with(".all.json") {
            assert_eq!(
                models_all.get(&provider_id).cloned().unwrap_or(Value::Null),
                actual,
                "models.all.json entry for {provider_id}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 18, "golden shard count");
}

/// 数组里每个元素的 `id`（非数组则为空）。
fn model_ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|models| {
            models
                .iter()
                .map(|model| {
                    model
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 每个模型对象的键序列（`["id", "name", ...].join(",")`），保持目录自身顺序。
fn model_field_orders(value: &serde_json::Value) -> Vec<String> {
    let entries: Vec<&serde_json::Value> = match value {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(object) => object.values().collect(),
        _ => Vec::new(),
    };
    entries
        .into_iter()
        .map(|model| {
            model
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
                .unwrap_or_default()
        })
        .collect()
}

/// 对象的键序列（数组 -> 空）。
fn keys_of(value: &serde_json::Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

/// 递归按键名排序后序列化（`JSON.stringify` 数字格式化，与 TS 的 `serializeJson` 一致）。
fn canonical(value: &serde_json::Value) -> String {
    fn sorted(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let mut entries: Vec<(&String, &serde_json::Value)> = object.iter().collect();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                let mut sorted_object = serde_json::Map::new();
                for (key, entry) in entries {
                    sorted_object.insert(key.clone(), sorted(entry));
                }
                serde_json::Value::Object(sorted_object)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sorted).collect())
            }
            other => other.clone(),
        }
    }
    kiana_ai::model_catalog::generator::serialize_json(&sorted(value), false)
}

/// 不带 `--json-output` 时的统计输出与 TS stdout 的统计块一致。
#[test]
fn statistics_output_matches_the_typescript_stdout_tail() {
    let root = TempDir::new("kiana-generate-models-stats-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
    let inputs = golden_inputs(&package_root);
    let options = GeneratorOptions {
        json_only: true,
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let output = generate_models(&inputs, &options, &mut state).expect("generation succeeds");
    let golden_stdout = read_golden("stdout.txt");
    let stats_index = golden_stdout
        .find("\nModel Statistics:")
        .expect("stats header");
    let expected = &golden_stdout[stats_index..];
    let actual_stdout = format!("{}\n", output.join("\n"));
    let actual_index = actual_stdout
        .find("\nModel Statistics:")
        .expect("stats header");
    assert_eq!(&actual_stdout[actual_index..], expected);
}

/// `--data-only` 时只写 `readModelDataProviderIds` 列出的 provider。
#[test]
fn data_only_limits_writes_to_imported_providers() {
    let root = TempDir::new("kiana-generate-models-dataonly-");
    let package_root = root.path().join("package");
    write_file(
        &package_root.join("src/models.generated.ts"),
        "import { QWEN_TOKEN_PLAN_CLASSIFIER_MODELS, QWEN_TOKEN_PLAN_IMAGE_MODELS, QWEN_TOKEN_PLAN_MODELS } from \"./providers/qwen-token-plan.models.ts\";\n",
    );
    let mut inputs = golden_inputs(&package_root);
    inputs.models_dev = Some(read_input("models-dev.json"));
    let options = GeneratorOptions {
        data_only: true,
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let catalog = generate_catalog(&inputs, &options, &mut state).expect("generation succeeds");
    assert_eq!(
        catalog.file_contents.keys().cloned().collect::<Vec<_>>(),
        vec!["qwen-token-plan.json".to_string()]
    );

    // 缺 provider 时给出 `Cannot hydrate missing providers: ...`。
    write_file(
        &package_root.join("src/models.generated.ts"),
        "import { MISSING_CLASSIFIER_MODELS, MISSING_IMAGE_MODELS, MISSING_MODELS } from \"./providers/missing.models.ts\";\n",
    );
    let error = generate_catalog(&inputs, &options, &mut GeneratorState::default())
        .expect_err("missing provider must fail");
    assert_eq!(error.message, "Cannot hydrate missing providers: missing");
}

/// 生成的数据目录能通过 `validateGeneratedModelData`，并写出 `.manifest.json`。
#[test]
fn writes_manifest_and_passes_its_own_validation() {
    let root = TempDir::new("kiana-generate-models-write-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
    write_file(
        &package_root.join("src/models.generated.ts"),
        "import { FIREWORKS_CLASSIFIER_MODELS, FIREWORKS_IMAGE_MODELS, FIREWORKS_MODELS } from \"./providers/fireworks.models.ts\";\n",
    );
    let mut inputs = GeneratorInputs {
        package_root: package_root.clone(),
        ..Default::default()
    };
    let mut provider = serde_json::Map::new();
    provider.insert(
        "models".to_string(),
        json!({
            "accounts/fireworks/models/glm-5p3": {
                "id": "accounts/fireworks/models/glm-5p3",
                "name": "GLM 5.3",
                "tool_call": true,
                "reasoning": true,
                "limit": { "context": 1048573, "output": 262144 },
                "cost": { "input": 1.4, "output": 4.4, "cache_read": 0.26, "cache_write": 0 },
            }
        }),
    );
    let mut models_dev = serde_json::Map::new();
    models_dev.insert("fireworks-ai".to_string(), Value::Object(provider));
    inputs.models_dev = Some(Value::Object(models_dev));

    let options = GeneratorOptions::default();
    let mut state = GeneratorState::default();
    generate_models(&inputs, &options, &mut state).expect("generation succeeds");

    let data_dir = package_root.join("src/providers/data");
    assert!(data_dir.join("fireworks.json").is_file());
    assert!(data_dir.join(".manifest.json").is_file());
    let manifest = std::fs::read_to_string(data_dir.join(".manifest.json")).expect("manifest");
    assert!(manifest.ends_with("}\n"));
    assert!(manifest.contains("\"schemaVersion\":6"));
    kiana_ai::model_catalog::model_data::validate_generated_model_data(&package_root)
        .expect("valid");

    // 分片与聚合器按模板写出。
    let shard = std::fs::read_to_string(package_root.join("src/providers/fireworks.models.ts"))
        .expect("shard");
    assert!(shard.starts_with("// This file is auto-generated by scripts/generate-models.ts\n"));
    assert!(shard.contains(
        "export const FIREWORKS_MODELS: ChatModelCatalog<typeof values, \"fireworks\"> =\n"
    ));
    let aggregator =
        std::fs::read_to_string(package_root.join("src/models.generated.ts")).expect("aggregator");
    assert!(aggregator.contains("export const MODELS: {\n"));
}

// ---------------------------------------------------------------------------
// `readGeneratorOptions` 与 TypeScript 模板（spec 2.4.9）
//
// 这两块没有独立的 TS 测试文件（`generate-models-strict.test.ts` 只覆盖 `--strict` 的数据源失败），
// 但 spec 逐字规定了选项错误文案，模板则由 TS 仓库里真实的 `src/providers/*.models.ts` 与
// `src/models.generated.ts` 固定（fixture 在 `tests/fixtures/catalog-tooling/golden/typescript/`）。
// ---------------------------------------------------------------------------

mod generator_options_and_templates {
    use super::read_golden;
    use kiana_ai::model_catalog::generator::{
        GeneratorOptions, catalog_const_name, classifier_catalog_const_name,
        generate_models_generated_ts, generate_provider_shard, image_catalog_const_name,
        read_generator_options,
    };
    use std::path::PathBuf;

    fn error_of(args: &[&str]) -> String {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        match read_generator_options(&owned) {
            Ok(options) => panic!("expected an error for {args:?}, got {options:?}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn parses_the_documented_flags() {
        let options = read_generator_options(&["--strict".to_string(), "--pretty".to_string()])
            .expect("flags");
        assert_eq!(
            options,
            GeneratorOptions {
                strict: true,
                pretty: true,
                ..Default::default()
            }
        );

        let options = read_generator_options(&[
            "--json-only".to_string(),
            "--json-output".to_string(),
            "/tmp/out".to_string(),
        ])
        .expect("json output");
        assert!(options.json_only);
        assert_eq!(options.json_output_dir, Some(PathBuf::from("/tmp/out")));

        let options = read_generator_options(&["--data-only".to_string()]).expect("data only");
        assert!(options.data_only);
    }

    #[test]
    fn rejects_unknown_arguments_verbatim() {
        assert_eq!(error_of(&["--nope"]), "Unknown argument: --nope");
        assert_eq!(error_of(&["--strict", "extra"]), "Unknown argument: extra");
    }

    #[test]
    fn requires_a_value_for_json_output() {
        assert_eq!(
            error_of(&["--json-output"]),
            "--json-output requires a directory"
        );
    }

    #[test]
    fn requires_json_output_for_json_only() {
        assert_eq!(
            error_of(&["--json-only"]),
            "--json-only requires --json-output"
        );
    }

    #[test]
    fn rejects_data_only_combined_with_json_catalog_output() {
        assert_eq!(
            error_of(&["--data-only", "--json-only", "--json-output", "/tmp/out"]),
            "--data-only cannot be combined with JSON catalog output"
        );
        assert_eq!(
            error_of(&["--data-only", "--json-output", "/tmp/out"]),
            "--data-only cannot be combined with JSON catalog output"
        );
    }

    #[test]
    fn derives_const_names_from_provider_ids() {
        assert_eq!(catalog_const_name("fireworks"), "FIREWORKS_MODELS");
        assert_eq!(
            catalog_const_name("xiaomi-token-plan-ams"),
            "XIAOMI_TOKEN_PLAN_AMS_MODELS"
        );
        assert_eq!(catalog_const_name("openrouter"), "OPENROUTER_MODELS");
        assert_eq!(
            image_catalog_const_name("fireworks"),
            "FIREWORKS_IMAGE_MODELS"
        );
        assert_eq!(
            classifier_catalog_const_name("fireworks"),
            "FIREWORKS_CLASSIFIER_MODELS"
        );
    }

    #[test]
    fn provider_shard_matches_the_typescript_template_byte_for_byte() {
        for provider_id in ["fireworks", "xiaomi-token-plan-ams"] {
            let golden = read_golden(&format!("typescript/{provider_id}.models.ts"));
            assert_eq!(
                generate_provider_shard(provider_id),
                golden,
                "{provider_id}.models.ts differs"
            );
        }
    }

    #[test]
    fn aggregator_matches_the_typescript_template_byte_for_byte() {
        // golden 里的 provider 顺序就是 TS 仓库 `models.generated.ts` 的 import 顺序。
        let golden = read_golden("typescript/models.generated.ts");
        let mut provider_ids: Vec<String> = Vec::new();
        for line in golden.lines() {
            let Some(rest) = line.strip_prefix("import { ") else {
                continue;
            };
            let Some((_, tail)) = rest.split_once(" } from \"./providers/") else {
                continue;
            };
            let id = tail.strip_suffix(".models.ts\";").expect("shard path");
            provider_ids.push(id.to_string());
        }
        assert_eq!(provider_ids.len(), 42, "42 provider imports");
        assert_eq!(generate_models_generated_ts(&provider_ids), golden);
    }
}

// ---------------------------------------------------------------------------
// 逐模型的字段插入序
//
// 生成的数据文件是逐字节入库的产物：`.manifest.json` 存的是文件内容的 sha256，
// 而 TS 直接 `JSON.stringify` 各 provider 的对象字面量，所以模型内部的字段插入序
// 会进入哈希与文件字节。Rust 侧所有模型都由共享的 `build_chat_model` 构造，
// 必须用 `reorder_model` 还原每个字面量的顺序。
//
// `golden/field-orders.json` 由 TS 跑同一批输入产出：`provider -> "api|type:id" -> "字段序"`。
// ---------------------------------------------------------------------------

mod model_field_order {
    use super::{
        model_data_support::{TempDir, golden_inputs, read_golden},
        *,
    };
    use serde_json::Value;

    fn expected_orders() -> serde_json::Map<String, Value> {
        serde_json::from_str(&read_golden("field-orders.json")).expect("field-orders.json")
    }

    #[test]
    fn every_generated_model_keeps_the_typescript_literal_field_order() {
        let root = TempDir::new("kiana-generate-models-order-");
        let package_root = root.path().join("package");
        std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
        let inputs = golden_inputs(&package_root);
        let mut state = GeneratorState::default();
        let catalog = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
            .expect("generation succeeds");

        let expected = expected_orders();
        let mut mismatches: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for (provider_id, models) in &expected {
            let models = models.as_object().expect("provider map");
            let shard = catalog
                .file_contents
                .get(&format!("{provider_id}.json"))
                .unwrap_or_else(|| panic!("missing shard for {provider_id}"));
            let actual: Value = serde_json::from_str(shard).expect("generated shard");
            for (key, order) in models {
                checked += 1;
                let Some((api, identity)) = key.split_once('|') else {
                    panic!("bad fixture key {key}")
                };
                let Some((model_type, model_id)) = identity.split_once(':') else {
                    panic!("bad identity {identity}")
                };
                // 分片的键是 `type:id`；`type` 由 provider 级合并时的 `chat` 字段决定。
                let mut found = None;
                if let Some(group) = actual.get(api).and_then(Value::as_object) {
                    for candidate_key in [identity.to_string(), format!("{model_type}:{model_id}")]
                    {
                        if let Some(model) = group.get(&candidate_key) {
                            found = Some(model);
                            break;
                        }
                    }
                }
                let Some(model) = found else {
                    mismatches.push(format!("{provider_id}: missing {api}/{identity}"));
                    continue;
                };
                let actual_order = model
                    .as_object()
                    .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                if actual_order != order.as_str().expect("order string") {
                    mismatches.push(format!(
                        "{provider_id}/{api}:{model_id}\n  rust: {actual_order}\n  ts  : {}",
                        order.as_str().unwrap_or_default()
                    ));
                }
            }
        }
        assert!(checked > 1500, "fixture unexpectedly small: {checked}");
        assert!(
            mismatches.is_empty(),
            "{} field-order mismatches (first 10):\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

// ---------------------------------------------------------------------------
// strict 门禁：OpenRouter 图像目录 / Radius 配置
//
// `fetchOpenRouterModels()` 在 `--strict` 下要求至少有一个可用图像模型；
// `fetchRadiusModels()` 在非 strict 时把失败 `console.error` 后返回 `[]`。
// 两者都发生在抓取阶段，因此 Rust 侧由 `build_all_models` 承担。
// ---------------------------------------------------------------------------

mod fetch_strict_gates {
    use super::model_data_support::{TempDir, golden_inputs, read_input};
    use super::*;

    fn package_root(root: &TempDir) -> std::path::PathBuf {
        let package_root = root.path().join("package");
        std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
        package_root
    }

    #[test]
    fn strict_generation_rejects_an_empty_openrouter_image_catalog() {
        let root = TempDir::new("kiana-openrouter-images-");
        let package_root = package_root(&root);
        let mut inputs = golden_inputs(&package_root);
        inputs.open_router_image_listed = read_input("openrouter-image-empty.json")["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let options = GeneratorOptions {
            strict: true,
            ..Default::default()
        };
        let error = generate_catalog(&inputs, &options, &mut GeneratorState::default())
            .expect_err("strict generation must fail");
        assert_eq!(
            error.message,
            "OpenRouter API returned no usable image models"
        );

        // 非 strict 时同样输入照常生成，只是没有图像模型。
        let catalog = generate_catalog(
            &inputs,
            &GeneratorOptions::default(),
            &mut GeneratorState::default(),
        )
        .expect("non-strict generation succeeds");
        let openrouter = catalog
            .providers
            .get("openrouter")
            .expect("openrouter provider");
        assert!(openrouter.image.is_empty());
        assert!(!openrouter.chat.is_empty());
    }

    #[test]
    fn strict_generation_rejects_an_invalid_radius_config() {
        let root = TempDir::new("kiana-radius-strict-");
        let package_root = package_root(&root);
        let mut inputs = golden_inputs(&package_root);
        inputs.radius_config = Some(json!({ "baseUrl": 42 }));
        let options = GeneratorOptions {
            strict: true,
            ..Default::default()
        };
        let error = generate_catalog(&inputs, &options, &mut GeneratorState::default())
            .expect_err("strict generation must fail");
        assert!(
            error.message.contains("Invalid Radius config"),
            "unexpected message: {}",
            error.message
        );

        // 非 strict 时 Radius 失败被吞掉，其余 provider 照常生成。
        let catalog = generate_catalog(
            &inputs,
            &GeneratorOptions::default(),
            &mut GeneratorState::default(),
        )
        .expect("non-strict generation tolerates a broken Radius config");
        assert!(!catalog.providers.contains_key("radius"));
        assert!(catalog.providers.contains_key("anthropic"));
    }

    #[test]
    fn non_strict_generation_tolerates_an_empty_radius_catalog() {
        let root = TempDir::new("kiana-radius-empty-");
        let package_root = package_root(&root);
        let mut inputs = golden_inputs(&package_root);
        inputs.radius_config = Some(json!({
            "baseUrl": "https://radius.kiana.dev",
            "models": [],
        }));
        let catalog = generate_catalog(
            &inputs,
            &GeneratorOptions::default(),
            &mut GeneratorState::default(),
        )
        .expect("non-strict generation succeeds with no Radius models");
        assert!(!catalog.providers.contains_key("radius"));

        let options = GeneratorOptions {
            strict: true,
            ..Default::default()
        };
        let error = generate_catalog(&inputs, &options, &mut GeneratorState::default())
            .expect_err("strict generation must fail");
        assert_eq!(error.message, "Radius API returned no models");
    }
}

// ---------------------------------------------------------------------------
// `--pretty`
//
// `generate-models.ts` 用同一个 `serializeJson`
// （`JSON.stringify(value, null, pretty ? 2 : undefined) + "\n"`）写数据分片和
// `.manifest.json`；`hydrate-model-catalog.ts` 写分片时始终紧凑。
// ---------------------------------------------------------------------------

mod pretty_serialization {
    use super::model_data_support::{TempDir, golden_inputs};
    use super::*;
    use kiana_ai::model_catalog::flatten::ModelGroups;
    use kiana_ai::model_catalog::model_data::{
        ModelDataStructure, serialize_groups, serialize_groups_pretty,
    };

    fn tiny_groups() -> ModelGroups<Value> {
        let mut groups: ModelGroups<Value> = ModelGroups::new();
        let mut chat: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
        let mut model = serde_json::Map::new();
        model.insert("id".to_string(), json!("m1"));
        model.insert("name".to_string(), json!("M1"));
        chat.insert("chat:m1".to_string(), Value::Object(model));
        groups.insert("openai-completions".to_string(), chat);
        groups
    }

    #[test]
    fn pretty_uses_two_space_indentation_and_a_trailing_newline() {
        let groups = tiny_groups();
        let compact = serialize_groups(&groups);
        assert_eq!(
            compact,
            "{\"openai-completions\":{\"chat:m1\":{\"id\":\"m1\",\"name\":\"M1\"}}}\n"
        );
        assert_eq!(serialize_groups_pretty(&groups, false), compact);

        let pretty = serialize_groups_pretty(&groups, true);
        let expected_lines = [
            "{",
            "  \"openai-completions\": {",
            "    \"chat:m1\": {",
            "      \"id\": \"m1\",",
            "      \"name\": \"M1\"",
            "    }",
            "  }",
            "}",
            "",
        ];
        // `expected_lines` 的最后一个元素是结尾换行后的空串：`join("\n")` 已经带上了它。
        assert_eq!(pretty, expected_lines.join("\n"));

        // 两种输出解析后是同一份数据。
        let compact_value: Value = serde_json::from_str(&compact).expect("compact");
        let pretty_value: Value = serde_json::from_str(&pretty).expect("pretty");
        assert_eq!(compact_value, pretty_value);
    }

    #[test]
    fn generate_catalog_applies_pretty_to_data_shards() {
        let root = TempDir::new("kiana-generate-models-pretty-");
        let package_root = root.path().join("package");
        std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
        let inputs = golden_inputs(&package_root);

        let mut state = GeneratorState::default();
        let compact = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
            .expect("compact generation succeeds");
        let mut state = GeneratorState::default();
        let pretty = generate_catalog(
            &inputs,
            &GeneratorOptions {
                pretty: true,
                ..Default::default()
            },
            &mut state,
        )
        .expect("pretty generation succeeds");

        assert_eq!(compact.file_contents.len(), pretty.file_contents.len());
        for (name, compact_text) in &compact.file_contents {
            let pretty_text = pretty.file_contents.get(name).expect("same shard set");
            assert!(
                !compact_text.contains('\n') || compact_text.trim_end().lines().count() == 1,
                "{name}: compact"
            );
            assert!(
                pretty_text.lines().count() > 2,
                "{name}: pretty should span lines"
            );
            let compact_value: Value = serde_json::from_str(compact_text).expect("compact json");
            let pretty_value: Value = serde_json::from_str(pretty_text).expect("pretty json");
            assert_eq!(compact_value, pretty_value, "{name}: same data");
        }
        // 固定一个已知分片做逐字节核对（前 3 行）。
        let openrouter = pretty
            .file_contents
            .get("openrouter.json")
            .expect("openrouter shard");
        let mut lines = openrouter.lines();
        assert_eq!(lines.next(), Some("{"));
        assert_eq!(lines.next(), Some("  \"anthropic-messages\": {"));
        assert_eq!(
            lines.next(),
            Some("    \"chat:anthropic/claude-fable-5\": {")
        );
        assert!(openrouter.ends_with("}\n"));
    }

    #[test]
    fn manifest_and_data_structure_are_independent_of_pretty() {
        // 结构哈希只依赖分组结构，不依赖序列化格式。
        let root = TempDir::new("kiana-generate-models-structure-");
        let package_root = root.path().join("package");
        std::fs::create_dir_all(package_root.join("src/providers/data")).expect("create dirs");
        let inputs = golden_inputs(&package_root);
        let mut state = GeneratorState::default();
        let compact =
            generate_catalog(&inputs, &GeneratorOptions::default(), &mut state).expect("compact");
        let mut state = GeneratorState::default();
        let pretty = generate_catalog(
            &inputs,
            &GeneratorOptions {
                pretty: true,
                ..Default::default()
            },
            &mut state,
        )
        .expect("pretty");
        assert_eq!(compact.model_data_structure, pretty.model_data_structure);
        let structure: &ModelDataStructure = &compact.model_data_structure;
        assert!(structure.contains_key("openrouter"));
    }
}

// ---------------------------------------------------------------------------
// `readModelDataProviderIds`
//
// TS 用 `/^import ...$/gm`：JS 的 `m` 把 `\n`、`\r`、` `、` ` 都当作行终止符，
// 所以 CRLF 与 U+2028 分隔的聚合器同样能匹配。Rust 的 `(?m)` 只认 `\n`（`$` 也不会落在
// `\r` 之前），必须先归一化行终止符。
// ---------------------------------------------------------------------------

mod provider_id_scanning {
    use super::model_data_support::{TempDir, write_file};
    use kiana_ai::model_catalog::model_data::read_model_data_provider_ids;

    const AGGREGATOR: &str = "import { A_CLASSIFIER_MODELS, A_IMAGE_MODELS, A_MODELS } from \"./providers/aaa.models.ts\";\nimport { Z_CLASSIFIER_MODELS, Z_IMAGE_MODELS, Z_MODELS } from \"./providers/zzz.models.ts\";\n";

    fn package_root(root: &TempDir) -> std::path::PathBuf {
        let package_root = root.path().join("package");
        std::fs::create_dir_all(&package_root).expect("create dirs");
        package_root
    }

    #[test]
    fn reads_lf_separated_imports() {
        let root = TempDir::new("kiana-provider-ids-lf-");
        let package_root = package_root(&root);
        write_file(&package_root.join("src/models.generated.ts"), AGGREGATOR);
        assert_eq!(
            read_model_data_provider_ids(&package_root).expect("reads"),
            vec!["aaa".to_string(), "zzz".to_string()]
        );
    }

    #[test]
    fn reads_crlf_and_javascript_line_separator_imports() {
        for separator in ["\r\n", "\u{2028}", "\u{2029}"] {
            let root = TempDir::new("kiana-provider-ids-crlf-");
            let package_root = package_root(&root);
            let aggregator = AGGREGATOR.replace('\n', separator);
            write_file(&package_root.join("src/models.generated.ts"), &aggregator);
            assert_eq!(
                read_model_data_provider_ids(&package_root).expect("reads"),
                vec!["aaa".to_string(), "zzz".to_string()],
                "separator {:?}",
                separator.escape_debug().to_string()
            );
        }
    }

    #[test]
    fn rejects_an_aggregator_without_imports() {
        let root = TempDir::new("kiana-provider-ids-empty-");
        let package_root = package_root(&root);
        write_file(
            &package_root.join("src/models.generated.ts"),
            "// nothing here\n",
        );
        let error = read_model_data_provider_ids(&package_root).expect_err("no imports");
        assert!(
            error
                .message
                .contains("No generated provider imports found"),
            "unexpected message: {}",
            error.message
        );
    }
}

// ---------------------------------------------------------------------------
// 审查修复的回归测试：CLI 选项、fetch 失败语义、写盘回滚。
// ---------------------------------------------------------------------------

fn decision_payload() -> Value {
    json!({
        "typesafe/jev-latest": {
            "type": "decision",
            "name": "Jev",
            "limit": { "context": 64000 },
        },
    })
}

fn image_listing() -> Vec<Value> {
    vec![json!({
        "id": "image-1",
        "name": "Image 1",
        "architecture": { "output_modalities": ["image"], "input_modalities": ["text"] },
        "pricing": { "prompt": "0.000001" },
    })]
}

/// `--json-output ""` 与缺值同错：空串会被 `path.resolve` 成 CWD。
#[test]
fn rejects_an_empty_json_output_directory() {
    for args in [
        vec![
            "--json-only".to_string(),
            "--json-output".to_string(),
            String::new(),
        ],
        vec!["--json-only".to_string(), "--json-output".to_string()],
    ] {
        let error = read_generator_options(&args).expect_err("empty value must be rejected");
        assert_eq!(error.message, "--json-output requires a directory");
    }
}

/// `rmSync(dir, { recursive: true, force: true })`：已存在的普通文件会被替换成目录。
#[test]
fn replaces_an_existing_regular_file_at_the_json_output_path() {
    let root = TempDir::new("kiana-generate-models-jsonfile-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let output_dir = root.path().join("out");
    write_file(&output_dir, "not a directory\n");
    let inputs = GeneratorInputs {
        package_root,
        ..Default::default()
    };
    let options = GeneratorOptions {
        json_only: true,
        json_output_dir: Some(output_dir.clone()),
        ..Default::default()
    };
    generate_models(&inputs, &options, &mut GeneratorState::default())
        .expect("generation succeeds");
    assert!(output_dir.is_dir(), "file must be replaced by a directory");
    let models: Value = serde_json::from_str(
        &std::fs::read_to_string(output_dir.join("models.json")).expect("models.json"),
    )
    .expect("models.json is JSON");
    assert!(models.is_object(), "models.json is written");
    assert!(
        output_dir.join("providers").is_dir(),
        "providers directory is created"
    );
}

/// strict 模式下 models.dev 取回失败会重新抛出，且已打印的进度行仍保留。
#[test]
fn strict_mode_rethrows_a_models_dev_fetch_error() {
    let root = TempDir::new("kiana-generate-models-msdev-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut inputs = GeneratorInputs {
        package_root,
        ..Default::default()
    };
    inputs.fetch_errors.insert(
        SOURCE_MODELS_DEV.to_string(),
        "models.dev API returned 500".to_string(),
    );
    let options = GeneratorOptions {
        strict: true,
        json_only: true,
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let error = generate_models(&inputs, &options, &mut state).expect_err("strict must fail");
    assert_eq!(error.message, "models.dev API returned 500");
    assert_eq!(
        state.stderr_lines,
        vec!["Failed to load models.dev data: models.dev API returned 500".to_string()]
    );
    // TS 的 console.log 在 throw 之前已经打印。
    assert_eq!(
        state.stdout_lines,
        vec!["Fetching models from models.dev API...".to_string()]
    );
}

/// 非 strict：失败被记录到 stderr，生成继续（没有 `Loaded …` 行）。
#[test]
fn non_strict_logs_a_models_dev_fetch_error_and_continues() {
    let root = TempDir::new("kiana-generate-models-msdev-lax-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut inputs = GeneratorInputs {
        package_root,
        ..Default::default()
    };
    inputs.fetch_errors.insert(
        SOURCE_MODELS_DEV.to_string(),
        "models.dev API returned 500".to_string(),
    );
    let options = GeneratorOptions {
        json_only: true,
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    let output = generate_models(&inputs, &options, &mut state).expect("non-strict continues");
    assert!(
        state
            .stderr_lines
            .contains(&"Failed to load models.dev data: models.dev API returned 500".to_string())
    );
    assert!(output.contains(&"Fetching models from models.dev API...".to_string()));
    assert!(
        !output.iter().any(|line| line.starts_with("Loaded ")),
        "failed models.dev load has no Loaded line: {output:?}"
    );
}

/// 非法/缺失的 classifier payload：strict 抛出，非 strict 记日志并降级为空。
#[test]
fn classifier_payload_failures_follow_strict_mode() {
    let root = TempDir::new("kiana-generate-models-classifier-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut inputs = strict_ready_inputs(&package_root);
    inputs.models_dev_decision = Some(json!({}));

    let mut state = GeneratorState::default();
    let catalog = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
        .expect("non-strict degrades");
    assert!(
        !catalog.providers.contains_key("typesafe"),
        "classifier must be absent"
    );
    assert!(state.stderr_lines.iter().any(|line| line
        == "Failed to load models.dev classifier data: models.dev did not return decision model typesafe/jev-latest"));

    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(
        error.message,
        "models.dev did not return decision model typesafe/jev-latest"
    );
}

/// classifier fetch 失败（`fetch_errors`）与 payload 失败走同一条 catch。
#[test]
fn classifier_fetch_errors_follow_strict_mode() {
    let root = TempDir::new("kiana-generate-models-classifier-fetch-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut inputs = strict_ready_inputs(&package_root);
    inputs.fetch_errors.insert(
        SOURCE_MODELS_DEV_CLASSIFIER.to_string(),
        "models.dev classifier API returned 503".to_string(),
    );
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(error.message, "models.dev classifier API returned 503");
}

/// NVIDIA NIM 只在 models.dev 声明了（truthy 的）`nvidia.models` 时取回。
#[test]
fn nvidia_fetch_failures_follow_strict_mode() {
    let root = TempDir::new("kiana-generate-models-nvidia-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let inputs = GeneratorInputs {
        package_root,
        models_dev: Some(json!({ "nvidia": { "models": {} } })),
        ..Default::default()
    };
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(error.message, "TypeError: fetch failed");
    let mut state = GeneratorState::default();
    generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: true,
            ..Default::default()
        },
        &mut state,
    )
    .expect("non-strict continues");
    assert!(
        state
            .stderr_lines
            .contains(&"Failed to fetch NVIDIA NIM models: TypeError: fetch failed".to_string())
    );

    // `models: null` 在 JS 里是 falsy：不发起 NVIDIA 请求。
    let inputs = GeneratorInputs {
        package_root: root.path().join("package"),
        models_dev: Some(json!({ "nvidia": { "models": null } })),
        ..Default::default()
    };
    let mut state = GeneratorState::default();
    generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: true,
            ..Default::default()
        },
        &mut state,
    )
    .expect("non-strict continues");
    assert!(
        !state
            .stdout_lines
            .iter()
            .any(|line| line.contains("NVIDIA")),
        "no NVIDIA fetch for falsy models: {:?}",
        state.stdout_lines
    );
}

/// OpenRouter 三个列表任一取回失败：strict 抛出，非 strict 空目录。
#[test]
fn open_router_fetch_failures_follow_strict_mode() {
    let root = TempDir::new("kiana-generate-models-openrouter-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut inputs = strict_ready_inputs(&package_root);
    inputs.models_dev_decision = Some(decision_payload());
    inputs.fetch_errors.insert(
        SOURCE_OPEN_ROUTER.to_string(),
        "OpenRouter API returned 502".to_string(),
    );
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(error.message, "OpenRouter API returned 502");

    let mut state = GeneratorState::default();
    let _catalog = generate_catalog(&inputs, &GeneratorOptions::default(), &mut state)
        .expect("non-strict continues");
    assert!(
        state.stderr_lines.contains(
            &"Failed to fetch OpenRouter models: OpenRouter API returned 502".to_string()
        )
    );
}

/// AI Gateway / Radius 取回或配置失败：strict 抛出，非 strict 记日志。
#[test]
fn ai_gateway_and_radius_failures_follow_strict_mode() {
    let root = TempDir::new("kiana-generate-models-gateway-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(&package_root).expect("create package");
    let mut base = strict_ready_inputs(&package_root);
    base.models_dev_decision = Some(decision_payload());
    base.open_router_image_listed = image_listing();
    let base_inputs = || base.clone();

    // AI Gateway fetch 失败（在 OpenRouter 之后）。
    let mut inputs = base_inputs();
    inputs.fetch_errors.insert(
        SOURCE_AI_GATEWAY.to_string(),
        "Vercel AI Gateway API returned 503".to_string(),
    );
    let mut state = GeneratorState::default();
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(error.message, "Vercel AI Gateway API returned 503");
    generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: true,
            ..Default::default()
        },
        &mut state,
    )
    .expect("non-strict continues");
    assert!(state.stderr_lines.contains(
        &"Failed to fetch Vercel AI Gateway models: Vercel AI Gateway API returned 503".to_string()
    ));

    // Radius fetch 失败（AI Gateway 成功但为空）。
    inputs.ai_gateway = Some(json!({ "data": [] }));
    inputs.fetch_errors.insert(
        SOURCE_RADIUS.to_string(),
        "getaddrinfo ENOTFOUND radius.kiana.dev".to_string(),
    );
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(error.message, "getaddrinfo ENOTFOUND radius.kiana.dev");

    // Radius 配置非法（存在但缺 baseUrl / models）。
    inputs.radius_config = Some(json!({}));
    inputs.fetch_errors.shift_remove(SOURCE_RADIUS);
    let mut state = GeneratorState::default();
    let error = generate_catalog(
        &inputs,
        &GeneratorOptions {
            strict: true,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("strict must fail");
    assert_eq!(
        error.message,
        "Invalid Radius config from https://radius.kiana.dev"
    );
    generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: true,
            ..Default::default()
        },
        &mut state,
    )
    .expect("non-strict continues");
    assert!(
        state.stderr_lines.contains(
            &"Failed to fetch Radius models: Invalid Radius config from https://radius.kiana.dev"
                .to_string()
        )
    );

    let _ = (SOURCE_AI_GATEWAY, SOURCE_RADIUS);
}

/// 快照之后的任何错误都会回滚 `*.models.ts` 与聚合器；已打印的 stdout 行保留。
#[test]
fn restores_the_previous_generated_catalog_after_a_late_write_error() {
    // root 会绕过 0444；那种环境下无法制造写失败。
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping restore test as root");
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new("kiana-generate-models-restore-");
    let package_root = root.path().join("package");
    let providers_dir = package_root.join("src/providers");
    std::fs::create_dir_all(providers_dir.join("data")).expect("create dirs");
    let aggregator_path = package_root.join("src/models.generated.ts");
    write_file(&aggregator_path, "ORIGINAL AGGREGATOR\n");
    write_file(&providers_dir.join("stale.models.ts"), "// stale shard\n");
    write_file(&providers_dir.join("custom.models.ts"), "// custom shard\n");
    let mut permissions = std::fs::metadata(&aggregator_path)
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&aggregator_path, permissions).expect("chmod");

    let inputs = golden_inputs(&package_root);
    let mut state = GeneratorState::default();
    let error = generate_models(&inputs, &GeneratorOptions::default(), &mut state)
        .expect_err("aggregator write must fail");
    assert!(
        error.message.contains("EACCES: permission denied, open '"),
        "unexpected message: {}",
        error.message
    );
    assert!(
        error
            .message
            .contains(&aggregator_path.display().to_string()),
        "message names the aggregator: {}",
        error.message
    );
    // 失败前打印的进度行仍保留（TS 已写 stdout）。
    assert!(
        state
            .stdout_lines
            .contains(&"Fetching models from models.dev API...".to_string()),
        "stdout lines: {:?}",
        state.stdout_lines
    );

    // 快照里的分片（含被删除的 stale）全部恢复，生成的新分片不残留。
    assert_eq!(
        std::fs::read_to_string(providers_dir.join("stale.models.ts")).expect("stale"),
        "// stale shard\n"
    );
    assert_eq!(
        std::fs::read_to_string(providers_dir.join("custom.models.ts")).expect("custom"),
        "// custom shard\n"
    );
    let remaining: Vec<String> = std::fs::read_dir(&providers_dir)
        .expect("read providers")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".models.ts"))
        .collect();
    assert_eq!(
        remaining,
        vec![
            "stale.models.ts".to_string(),
            "custom.models.ts".to_string()
        ],
        "generated shards must be rolled back"
    );
    assert_eq!(
        std::fs::read_to_string(&aggregator_path).expect("aggregator"),
        "ORIGINAL AGGREGATOR\n"
    );
}

/// 生成器的 staging 与聚合器读取也使用 Node 的错误文本。
#[test]
fn reports_node_errors_for_missing_generation_paths() {
    let root = TempDir::new("kiana-generate-models-fs-errors-");
    let package_root = root.path().join("package");
    std::fs::create_dir_all(package_root.join("src")).expect("create src");
    let inputs = GeneratorInputs {
        package_root: package_root.clone(),
        ..Default::default()
    };

    // 没有 src/providers：mkdtempSync 报 ENOENT，模板名是 `<prefix>XXXXXX`。
    let error = generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: false,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("missing providers dir");
    let providers_dir = package_root.join("src/providers");
    assert_eq!(
        error.message,
        format!(
            "ENOENT: no such file or directory, mkdtemp '{}'",
            providers_dir.join(".model-generation-XXXXXX").display()
        )
    );

    // providers 存在但没有聚合器：快照读取报 ENOENT open。
    std::fs::create_dir_all(&providers_dir).expect("create providers");
    let error = generate_models(
        &inputs,
        &GeneratorOptions {
            json_only: false,
            ..Default::default()
        },
        &mut GeneratorState::default(),
    )
    .expect_err("missing aggregator");
    assert_eq!(
        error.message,
        format!(
            "ENOENT: no such file or directory, open '{}'",
            package_root.join("src/models.generated.ts").display()
        )
    );
}
