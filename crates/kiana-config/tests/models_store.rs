//! `core/models-store.ts` 的测试移植。
//!
//! 与原测试的差异：`vi.spyOn(lockfile, "lock")` 的「锁调用次数」断言改为可观察行为
//! （并发读者读到同一份新快照、单个读者取消不打断共享重载）。

use std::fs;
use std::path::Path;
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::models::{ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use kiana_ai::types::{AnyModel, Model, ModelCost, ModelInputModality};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::lock::{LockOptions, lock_sync};
use kiana_config::auth::models_store::{FileModelsStore, InMemoryCodingAgentModelsStore};
use serde_json::{Value, json};
use tempfile::TempDir;

fn model(provider: &str, id: &str) -> AnyModel {
    AnyModel::Chat(Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "openai-completions".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Model::default()
    })
}

fn entry(id: &str, provider: &str, checked_at: Option<i64>) -> ModelsStoreEntry {
    ModelsStoreEntry {
        models: vec![model(provider, id)],
        raw_models: None,
        last_modified: None,
        checked_at,
        etag: None,
        extra: IndexMap::new(),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn options(signal: Option<AbortScope>) -> ModelsStoreOperationOptions {
    ModelsStoreOperationOptions { signal }
}

fn entry_ids(entry: &ModelsStoreEntry) -> Vec<String> {
    entry
        .models
        .iter()
        .map(|model| model.model().id.clone())
        .collect()
}

fn model_json(provider: &str, id: &str) -> Value {
    serde_json::json!({
        "id": id,
        "name": id,
        "api": "openai-completions",
        "provider": provider,
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "contextWindow": 1000,
        "maxTokens": 100
    })
}

/// persists provider catalogs without replacing unrelated providers
#[tokio::test]
async fn persists_provider_catalogs_without_replacing_unrelated_providers() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");

    let store = FileModelsStore::new(&path_string(&models_path));
    store
        .write("one", entry("m1", "one", Some(100)), options(None))
        .await
        .unwrap();
    store
        .write("two", entry("m2", "two", Some(200)), options(None))
        .await
        .unwrap();

    let reloaded = FileModelsStore::new(&path_string(&models_path));
    let one = reloaded.read("one", options(None)).await.unwrap().unwrap();
    assert_eq!(entry_ids(&one), ["m1"]);
    assert_eq!(one.checked_at, Some(100));
    let two = reloaded.read("two", options(None)).await.unwrap().unwrap();
    assert_eq!(entry_ids(&two), ["m2"]);

    reloaded.delete("one", options(None)).await.unwrap();
    assert!(reloaded.read("one", options(None)).await.unwrap().is_none());
    let two = reloaded.read("two", options(None)).await.unwrap().unwrap();
    assert_eq!(entry_ids(&two), ["m2"]);
}

/// preserves the mode of an existing models file
#[cfg(unix)]
#[tokio::test]
async fn preserves_the_mode_of_an_existing_models_file() {
    use std::os::unix::fs::PermissionsExt;
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("managed-mode.json");
    fs::write(&models_path, "{}").unwrap();
    fs::set_permissions(&models_path, fs::Permissions::from_mode(0o660)).unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    store
        .write("one", entry("m1", "one", Some(100)), options(None))
        .await
        .unwrap();
    let mode = fs::metadata(&models_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
}

/// coalesces file reloads across concurrent readers and interleaved storage instances
#[tokio::test]
async fn coalesces_file_reloads_across_concurrent_readers_and_interleaved_storage_instances() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    fs::write(
        &models_path,
        serde_json::json!({
            "one": {"models": [model_json("one", "old")]},
            "two": {"models": [model_json("two", "m2")]}
        })
        .to_string(),
    )
    .unwrap();
    let first = FileModelsStore::new(&path_string(&models_path));
    let second = FileModelsStore::new(&path_string(&models_path));

    let (one, two, missing) = tokio::join!(
        first.read("one", options(Some(AbortScope::new()))),
        second.read("two", options(Some(AbortScope::new()))),
        first.read("missing", options(Some(AbortScope::new()))),
    );
    assert_eq!(entry_ids(&one.unwrap().unwrap()), ["old"]);
    assert_eq!(entry_ids(&two.unwrap().unwrap()), ["m2"]);
    assert!(missing.unwrap().is_none());

    let second_one = second.read("one", options(None)).await.unwrap().unwrap();
    assert_eq!(entry_ids(&second_one), ["old"]);

    let other_path = temp_dir.path().join("other-models-store.json");
    fs::write(&other_path, "{}").unwrap();
    let other = FileModelsStore::new(&path_string(&other_path));
    assert!(other.read("one", options(None)).await.unwrap().is_none());
    assert!(other.read("one", options(None)).await.unwrap().is_none());

    let third = FileModelsStore::new(&path_string(&models_path));
    fs::write(
        &models_path,
        serde_json::json!({"one": {"models": [model_json("one", "newest-model")]}}).to_string(),
    )
    .unwrap();
    let (first_reload, third_reload) = tokio::join!(
        first.read("one", options(None)),
        third.read("one", options(None)),
    );
    assert_eq!(entry_ids(&first_reload.unwrap().unwrap()), ["newest-model"]);
    assert_eq!(entry_ids(&third_reload.unwrap().unwrap()), ["newest-model"]);
}

/// keeps a coalesced reload alive while another reader is still waiting
#[tokio::test]
async fn keeps_a_coalesced_reload_alive_while_another_reader_is_still_waiting() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    fs::write(
        &models_path,
        serde_json::json!({"one": {"models": [model_json("one", "stored")]}}).to_string(),
    )
    .unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    let held = lock_sync(
        Path::new(&path_string(&models_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();

    let first_controller = AbortScope::new();
    let second_controller = AbortScope::new();
    let first = tokio::spawn({
        let store = FileModelsStore::new(&path_string(&models_path));
        let signal = first_controller.clone();
        async move { store.read("one", options(Some(signal))).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = tokio::spawn({
        let signal = second_controller.clone();
        async move { store.read("one", options(Some(signal))).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    first_controller.abort();
    let first_result = first.await.unwrap();
    assert_eq!(first_result.unwrap_err().name, "AbortError");

    drop(held);
    let second_result = second.await.unwrap().unwrap().unwrap();
    assert_eq!(entry_ids(&second_result), ["stored"]);
}

/// cancels a catalog write waiting for a held file lock without writing later
#[tokio::test]
async fn cancels_a_catalog_write_waiting_for_a_held_file_lock_without_writing_later() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    fs::write(
        &models_path,
        serde_json::json!({"one": {"models": [model_json("one", "existing")]}}).to_string(),
    )
    .unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    let held = lock_sync(
        Path::new(&path_string(&models_path)),
        &LockOptions::sync_default(),
    )
    .unwrap();
    let controller = AbortScope::new();
    let pending_options = options(Some(controller.clone()));
    let pending = store.write("two", entry("cancelled", "two", None), pending_options);

    tokio::time::sleep(Duration::from_millis(20)).await;
    controller.abort();
    let result = pending.await;
    assert_eq!(result.unwrap_err().name, "AbortError");

    drop(held);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let stored: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    assert!(stored.get("one").is_some());
    assert!(stored.get("two").is_none());
}

/// 顶层不是对象时按 JS 属性语义运行（models-store.json 从不报 auth.json 错误）。
#[tokio::test]
async fn tolerates_non_object_top_level_model_documents() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");

    // 数组：命名属性写入被 JSON.stringify 忽略。
    fs::write(&models_path, "[1,2]").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    assert!(store.read("p", options(None)).await.unwrap().is_none());
    store
        .write("p", entry("m1", "p", None), options(None))
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&models_path).unwrap(), "[\n  1,\n  2\n]");
    store.delete("p", options(None)).await.unwrap();
    assert_eq!(fs::read_to_string(&models_path).unwrap(), "[\n  1,\n  2\n]");

    // 数字：read/delete 原样；write 抛 TypeError。
    fs::write(&models_path, "42").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    assert!(store.read("p", options(None)).await.unwrap().is_none());
    let error = store
        .write("p", entry("m1", "p", None), options(None))
        .await
        .unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(error.message, "Cannot create property 'p' on number '42'");
    store.delete("p", options(None)).await.unwrap();
    assert_eq!(fs::read_to_string(&models_path).unwrap(), "42");

    // 字符串：write 抛 TypeError。
    fs::write(&models_path, "\"x\"").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    let error = store
        .write("p", entry("m1", "p", None), options(None))
        .await
        .unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(error.message, "Cannot create property 'p' on string 'x'");

    // null：read/write/delete 都是 TypeError。
    fs::write(&models_path, "null").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    let error = store.read("p", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'p')"
    );
    let error = store
        .write("p", entry("m1", "p", None), options(None))
        .await
        .unwrap_err();
    assert_eq!(error.message, "Cannot set properties of null (setting 'p')");
    let error = store.delete("p", options(None)).await.unwrap_err();
    assert_eq!(error.message, "Cannot convert undefined or null to object");

    // malformed JSON：V8 文案，且不再出现 auth.json。
    fs::write(&models_path, "{oops").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    let error = store.read("p", options(None)).await.unwrap_err();
    assert_eq!(
        error.message,
        "Expected property name or '}' in JSON at position 1 (line 1 column 2)"
    );
    assert!(!error.message.contains("auth.json"));

    // 空文件 → `{}`（不是解析错误）。
    fs::write(&models_path, "").unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));
    assert!(store.read("p", options(None)).await.unwrap().is_none());
}

/// in-memory store 的读写克隆语义
#[tokio::test]
async fn in_memory_store_round_trips_and_aborts() {
    let store = InMemoryCodingAgentModelsStore::new();
    assert!(store.read("p", options(None)).await.unwrap().is_none());
    store
        .write("p", entry("m1", "p", Some(1)), options(None))
        .await
        .unwrap();
    let stored = store.read("p", options(None)).await.unwrap().unwrap();
    assert_eq!(entry_ids(&stored), ["m1"]);
    store.delete("p", options(None)).await.unwrap();
    assert!(store.read("p", options(None)).await.unwrap().is_none());

    let aborted = AbortScope::new();
    aborted.abort();
    let error = store
        .write("p", entry("m1", "p", None), options(Some(aborted)))
        .await
        .unwrap_err();
    assert!(error.is_aborted());
}

fn key_order(value: &Value) -> Vec<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

/// 无法投影到冻结 `ModelsStoreEntry` 的条目不再让 read 报错；可表达的字段正常读出，
/// 无法表达的已知字段原样保留在 `extra` 并写回。
#[tokio::test]
async fn reads_entries_with_unrepresentable_fields_without_erroring() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    fs::write(
        &models_path,
        r#"{"p":{"checkedAt":"soon","models":{},"etag":5,"zzz":1}}"#,
    )
    .unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));

    let entry = store.read("p", options(None)).await.unwrap().unwrap();
    assert!(entry.models.is_empty());
    assert_eq!(entry.checked_at, None);
    assert_eq!(entry.etag, None);
    assert_eq!(entry.extra.get("checkedAt"), Some(&json!("soon")));
    assert_eq!(entry.extra.get("etag"), Some(&json!(5)));
    assert_eq!(entry.extra.get("zzz"), Some(&json!(1)));

    // 写回后字段不丢失（`models` 非数组无法表达，按空数组写回）。
    store.write("p", entry, options(None)).await.unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    assert_eq!(written["p"]["checkedAt"], json!("soon"));
    assert_eq!(written["p"]["etag"], json!(5));
    assert_eq!(written["p"]["zzz"], json!(1));
    assert_eq!(written["p"]["models"], json!([]));
}

/// TS 的 `{...stored, checkedAt}` 保留 `stored` 的键序：read → write 不重排顶层键。
#[tokio::test]
async fn write_preserves_the_stored_entry_key_order() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    let original = format!(
        r#"{{"prov":{{"checkedAt":123,"models":[{}],"zzz":5,"etag":"\"abc\"","lastModified":9}}}}"#,
        model_json("prov", "m1")
    );
    fs::write(&models_path, &original).unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));

    let entry = store.read("prov", options(None)).await.unwrap().unwrap();
    store.write("prov", entry, options(None)).await.unwrap();

    let written: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    assert_eq!(
        key_order(&written["prov"]),
        ["checkedAt", "models", "zzz", "etag", "lastModified"]
    );
    assert_eq!(written["prov"]["etag"], json!("\"abc\""));
    assert_eq!(written["prov"]["lastModified"], json!(9));
    assert_eq!(
        entry_ids(&store.read("prov", options(None)).await.unwrap().unwrap()),
        ["m1"]
    );
}

/// 显式 `null` 的可选字段在 read → write 往返中保留；之后给类型化字段赋值时以新值为准。
#[tokio::test]
async fn write_preserves_explicit_nulls_until_the_field_is_set() {
    let temp_dir = TempDir::new().unwrap();
    let models_path = temp_dir.path().join("models-store.json");
    fs::write(
        &models_path,
        r#"{"prov":{"models":[],"lastModified":null,"etag":null}}"#,
    )
    .unwrap();
    let store = FileModelsStore::new(&path_string(&models_path));

    let entry = store.read("prov", options(None)).await.unwrap().unwrap();
    store
        .write("prov", entry.clone(), options(None))
        .await
        .unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    assert!(
        written["prov"]
            .as_object()
            .unwrap()
            .contains_key("lastModified")
    );
    assert_eq!(written["prov"]["lastModified"], Value::Null);
    assert_eq!(written["prov"]["etag"], Value::Null);

    // 调用方设置 checkedAt 后，类型化字段优先于暂存的旧值。
    let mut updated = entry;
    updated.checked_at = Some(7);
    store.write("prov", updated, options(None)).await.unwrap();
    let written: Value = serde_json::from_str(&fs::read_to_string(&models_path).unwrap()).unwrap();
    assert_eq!(written["prov"]["checkedAt"], json!(7));
    assert_eq!(written["prov"]["lastModified"], Value::Null);
}
