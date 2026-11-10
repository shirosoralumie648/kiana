//! `model-types.test.ts` 的 Rust 移植。
//!
//! 差异：TS 的 `{...chatModel, type: "embedding"}` 在 Rust 的 [`AnyModel`] 枚举里无法表示
//! （见 `models/store.rs` 的说明）；用 serde 反序列化断言未知类型的处理替代。

mod models_support;

use std::sync::Arc;

use kiana_ai::models::{
    CreateModelsOptions, InMemoryModelsStore, ModelsRefreshOptions, ModelsStore, ModelsStoreEntry,
    ProviderApiImpl, create_models, create_provider, get_model_type, is_model_type,
    models_are_equal,
};
use kiana_ai::models::{CreateProviderOptions, Provider};
use kiana_ai::types::{AnyModel, Context, Model, ModelCost, ModelInputModality, ModelType};

use models_support::{SilentStreams, TestProvider, test_model};

fn handwritten_provider() -> Arc<dyn Provider> {
    let models = vec![test_model("handwritten", "model-a")];
    Arc::new(TestProvider::with_models("handwritten", models))
}

fn chat_model(provider: &str, id: &str) -> Model {
    test_model(provider, id)
}

fn image_model(provider: &str, id: &str) -> AnyModel {
    AnyModel::Image(Model {
        model_type: Some(ModelType::Image),
        id: id.into(),
        name: id.into(),
        api: "test-images".into(),
        provider: provider.into(),
        base_url: "https://example.test/v1".into(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        output: Some(vec!["image".into()]),
        ..Model::default()
    })
}

#[tokio::test]
async fn chat_models_without_a_type_work_through_a_handwritten_provider_without_get_all_models() {
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(handwritten_provider());

    let model = models.get_model("handwritten", "model-a").unwrap();
    assert!(model.model_type.is_none());
    let any = AnyModel::Chat(model.clone());
    assert_eq!(get_model_type(&any), ModelType::Chat);
    assert!(kiana_ai::models::has_api(&any, &model.api));
    assert!(models_are_equal(
        Some(&any),
        Some(&AnyModel::Chat(Model {
            model_type: Some(ModelType::Chat),
            ..model.clone()
        }))
    ));
    assert!(!models_are_equal(
        Some(&any),
        Some(&image_model(&model.provider, &model.id))
    ));
    assert_eq!(
        models.get_models_of_type(ModelType::Chat, Some("handwritten")),
        vec![model.clone()]
    );
    assert_eq!(
        models.get_all_models(Some("handwritten")),
        vec![AnyModel::Chat(model.clone())]
    );
    assert!(
        models
            .get_models_of_type(ModelType::Image, Some("handwritten"))
            .is_empty()
    );

    let result = models
        .complete(&model, &Context::default(), Default::default())
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
}

#[test]
fn narrow_mixed_lists_with_is_model_type() {
    let mixed = [
        AnyModel::Chat(chat_model("p", "c")),
        AnyModel::Chat(Model {
            model_type: Some(ModelType::Chat),
            ..chat_model("p", "typed")
        }),
        image_model("p", "i"),
    ];
    let chat_ids: Vec<String> = mixed
        .iter()
        .filter(|model| is_model_type(model, ModelType::Chat))
        .map(|model| model.model().id.clone())
        .collect();
    assert_eq!(chat_ids, ["c", "typed"]);
    let image_ids: Vec<String> = mixed
        .iter()
        .filter(|model| is_model_type(model, ModelType::Image))
        .map(|model| model.model().id.clone())
        .collect();
    assert_eq!(image_ids, ["i"]);
    assert!(
        mixed
            .iter()
            .filter(|model| is_model_type(model, ModelType::Classifier))
            .count()
            == 0
    );
}

#[test]
#[allow(unused_assignments)] // TS 的编译期回归检查：同一 `api` 下的模型可以互相赋值。
fn built_in_catalog_getters_return_model_shapes_that_can_be_reassigned_within_one_api() {
    // Compile-time regression check: the return type must not carry literal model ids.
    let mut model: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openai", "gpt-4o-mini")
            .unwrap()
            .clone(),
    )
    .unwrap();
    model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openai", "gpt-4o")
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(model.id, "gpt-4o");
}

/// 与 `chat_model` 同形的 JSON（用于构造带未知 `type` 的持久化条目）。
fn model_json(provider: &str, id: &str, model_type: Option<&str>) -> serde_json::Value {
    let mut value = serde_json::json!({
        "id": id,
        "name": id,
        "api": "test-chat",
        "provider": provider,
        "baseUrl": "https://example.test/v1",
        "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0}
    });
    if let Some(model_type) = model_type {
        value["type"] = serde_json::Value::String(model_type.into());
        if model_type == "image" {
            value["api"] = serde_json::Value::String("test-images".into());
            value["output"] = serde_json::json!(["image"]);
        }
    }
    value
}

#[tokio::test]
async fn stored_and_fetched_models_of_unknown_types_are_dropped_instead_of_failing_the_refresh() {
    let models_store = Arc::new(InMemoryModelsStore::new());
    // TS 直接把带未知 `type` 的对象放进 store；Rust 的 `AnyModel` 无法表示未知类型，因此
    // 在 `ModelsStoreEntry` 反序列化时完成同一过滤（等价 TS 的 `withKnownModelTypes()`）。
    let entry: ModelsStoreEntry = serde_json::from_value(serde_json::json!({
        "models": [
            model_json("dyn", "stored-chat", None),
            model_json("dyn", "stored-image", Some("image")),
            model_json("dyn", "future-embedding", Some("embedding")),
            model_json("dyn", "future-video", Some("video")),
        ]
    }))
    .expect("known models survive; unknown types are dropped");
    assert_eq!(
        entry
            .models
            .iter()
            .map(|model| model.model().id.clone())
            .collect::<Vec<_>>(),
        ["stored-chat", "stored-image"]
    );
    // 未知类型的模型无法进入 `models`，但原始数组被保留：原样写回时仍是 4 条。
    let round_trip = serde_json::to_value(&entry).unwrap();
    assert_eq!(round_trip["models"].as_array().unwrap().len(), 4);
    models_store
        .write("dyn", entry, Default::default())
        .await
        .unwrap();

    let fetched: Arc<std::sync::Mutex<Vec<Model>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let models = create_models(CreateModelsOptions {
        models_store: Some(models_store.clone() as Arc<dyn ModelsStore>),
        ..Default::default()
    });
    let fetched_for = Arc::clone(&fetched);
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: "dyn".into(),
            auth: models_support::ambient_auth(),
            models: vec![],
            fetch_models: Some(Arc::new(move |_context| {
                let fetched = Arc::clone(&fetched_for);
                Box::pin(async move {
                    let fetched = fetched.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    Ok(fetched.into_iter().map(AnyModel::Chat).collect())
                })
            })),
            api: Some(ProviderApiImpl::Single(Arc::new(SilentStreams))),
            ..Default::default()
        })
        .unwrap(),
    );

    let restored = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["dyn".into()]),
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(restored.errors.len(), 0);
    assert_eq!(
        models
            .get_all_models(Some("dyn"))
            .iter()
            .map(|model| model.model().id.clone())
            .collect::<Vec<_>>(),
        ["stored-chat", "stored-image"]
    );

    *fetched.lock().unwrap_or_else(|e| e.into_inner()) = vec![chat_model("dyn", "fetched-chat")];
    let refreshed = models
        .refresh(ModelsRefreshOptions {
            providers: Some(vec!["dyn".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(refreshed.errors.len(), 0);
    assert_eq!(
        models
            .get_all_models(Some("dyn"))
            .iter()
            .map(|model| model.model().id.clone())
            .collect::<Vec<_>>(),
        ["fetched-chat"]
    );
    assert_eq!(
        models_store
            .read("dyn", Default::default())
            .await
            .unwrap()
            .unwrap()
            .models[0]
            .model()
            .id,
        "fetched-chat"
    );
}

#[test]
fn unknown_model_types_are_dropped_when_parsing_a_stored_entry() {
    // TS 的 `withKnownModelTypes()` 静默丢弃这些条目（`model-types.test.ts` 的
    // "are dropped instead of failing the refresh"）；Rust 在条目反序列化时完成同一过滤。
    let entry = serde_json::json!({
        "models": [
            model_json("p", "future", Some("embedding")),
            model_json("p", "known", None),
            model_json("p", "future-video", Some("video")),
        ]
    });
    let parsed: ModelsStoreEntry =
        serde_json::from_value(entry).expect("unknown types are dropped");
    assert_eq!(
        parsed
            .models
            .iter()
            .map(|model| model.model().id.as_str())
            .collect::<Vec<_>>(),
        ["known"]
    );

    // 直接反序列化单个 `AnyModel` 没有 serde 实现（枚举无法表示未知类型，且该类型由冻结的
    // `types.rs` 拥有）；持久化路径改用 `ModelsStoreEntry` 的原始数组往返。
    let entry: ModelsStoreEntry = serde_json::from_value(
        serde_json::json!({"models": [model_json("p", "future", Some("embedding"))]}),
    )
    .expect("unknown types do not fail the entry");
    assert!(entry.models.is_empty());
    assert_eq!(entry.raw_models.as_deref().unwrap().len(), 1);

    // TS: `getModelType(model) = model.type ?? "chat"` + `Object.hasOwn`。`null` 视为 chat；
    // 非字符串的 `type`（数字/布尔）不是已知类型，同样丢弃而不是让整条目录解析失败。
    let mut null_type = model_json("p", "null-type", None);
    null_type["type"] = serde_json::Value::Null;
    let mut numeric_type = model_json("p", "numeric-type", None);
    numeric_type["type"] = serde_json::json!(123);
    let mut boolean_type = model_json("p", "boolean-type", None);
    boolean_type["type"] = serde_json::json!(true);
    let parsed: ModelsStoreEntry = serde_json::from_value(serde_json::json!({
        "models": [null_type, numeric_type, boolean_type]
    }))
    .expect("non-string types are dropped, null stays chat");
    assert_eq!(
        parsed
            .models
            .iter()
            .map(|model| model.model().id.as_str())
            .collect::<Vec<_>>(),
        ["null-type"]
    );
}
