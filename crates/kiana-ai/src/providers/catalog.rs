//! `providers/<id>.models.ts`（42 个分片）+ `providers/data/<id>.json` + `models.generated.ts` 的编译期产物。
//!
//! TS 侧每个分片只有 14 行模板：从 `./data/<provider>.json` 摊平出 chat/image/classifier 三张表。
//! Rust 侧不手写 42 份重复代码，而是由 `build.rs` 读取同一批 JSON，生成
//! `MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS` 三张 `IndexMap`，键顺序固定为
//! `spec/ai-core.md` §2.3.1 列出的 42 个 provider 顺序。
//!
//! ## 数据来源（决策：`ARCHITECTURE.md` §8 第 5、21 条）
//!
//! TS 的 `packages/ai/src/providers/data/` 是 git-ignored 的 hydrate 产物
//! （`npm run hydrate-model-data` = `node scripts/generate-models.ts --strict --data-only`）。
//! 该目录已于 2026-10-26 生成并 **vendored** 到 `crates/kiana-ai/data/`（42 个 provider JSON +
//! `.manifest.json`，1.1 MiB，sha256 逐个对过 manifest；见该目录的 `README.md`）。
//!
//! `build.rs` 的目录选择顺序：
//!
//! 1. `KIANA_MODEL_DATA_DIR`（若设置）——指向另一份同结构目录；
//! 2. 仓库内 `crates/kiana-ai/data/`（默认路径，普通 `cargo build` 不需要 Node）；
//! 3. 两者都缺 → `panic!`（等价 TS 的 `check:model-data` 门禁；**不要**生成空表静默通过）。
//!
//! `build.rs` 还负责：对每个 JSON 发 `cargo:rerun-if-changed=`、按 `.manifest.json` 校验 sha256、
//! 校验 `structureHash`，并把 manifest 的 `generatedAt` 解析成 [`get_builtin_model_data_generated_at`] 的值。
//!
//! ## 静态表的形状
//!
//! 生成物是 `$OUT_DIR/model_catalog.json`，本模块用 `include_str!` 引入并**解析一次**
//! （`LazyLock`）。`serde_json` 开启 `preserve_order`，JSON 的键序 == `IndexMap` 的键序，
//! 因此三层键序都稳定：
//!
//! - 外层 provider 键序 == `spec/ai-core.md` §2.3.1（`build.rs` 的 `PROVIDER_ORDER`；注意
//!   `.manifest.json` 的 `files` 是**码元序**，与 §2.3.1 不同）；
//! - 内层模型键序 == `flatten*Catalog()` 的 `Object.fromEntries` 结果；
//! - 模型对象内部的键序 == 生成器构造该对象时的字面量顺序（`JSON.stringify` 同序）。
//!
//! 生成器保证每 provider 内 `type:id` 唯一，因此 `type` 与 `id` 的对应关系不需要在运行期重建。
//!
//! 公开 API 与 TS 的 `providers/all.ts` 一一对应（`getBuiltin*` 系列在 `providers/all.rs`，
//! 由 `ai-providers` 单元实现；本模块只提供表与 manifest 时间戳）。
//!
//! ## 运行期表示与不变量
//!
//! 首次访问任一查询函数时，`CATALOG`（`LazyLock`）把 `$OUT_DIR/model_catalog.json` 整个
//! 解析一次：顶层 `providers` 数组给出顺序，`models`/`imageModels`/`classifierModels` 三张
//! 表是 `provider -> (模型 id -> 模型对象)` 的嵌套对象，转成 `IndexMap` 后保持 JSON 键序。
//!
//! 数据由 `build.rs` 生成并校验，因此解析期只有一种失败可能——构建产物损坏：
//! 缺表、表不是对象、缺 `generatedAt`/`providers` 键都会 panic。`LazyLock` 初始化 panic
//! 后该静态量处于 poisoned 状态，后续访问会再次 panic，而不会得到空表；这是刻意的：
//! 静默空目录会让上层误以为「没有任何模型」。
//!
//! 公开表 `MODELS`/`IMAGE_MODELS`/`CLASSIFIER_MODELS` 是 `LazyLock<&'static ModelTable>`，
//! 只暴露共享引用，调用方无法通过它们修改目录；`provider_ids()` 返回的切片同样由
//! `CATALOG` 持有，进程内地址稳定。
//!
//! ## 与类型化视图的关系
//!
//! 本模块返回 `&Value`，原样保留目录条目的全部键（radius 的 `lab`/`enabled`/`providers`
//! 等）。`providers::all` 与 `providers::parse_catalog_model` 会把它们反序列化成 `Model`，
//! 未声明键在那一步丢失；要读取内置目录的原始条目（含未声明键与原始键序）就用本模块的
//! `&Value`。
//!
//! `ModelsStoreEntry::raw_models` 是另一处「保留原始 JSON」的实现，但它与本模块没有调用
//! 关系：它保留的是持久化数据里的原始 `models` 数组，由 `models::store` 在反序列化时填充，
//! 不读取 `providers::catalog`。

use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::Value;

/// 一张 `MODELS`/`IMAGE_MODELS`/`CLASSIFIER_MODELS` 表：provider -> (模型 id -> 模型 JSON)。
///
/// 两层都是 `IndexMap`，键序即 TS 对象键序：外层是 §2.3.1 的 provider 顺序，内层是
/// `flatten*Catalog()` 摊平后的模型顺序。值保留原始 JSON 的键序与全部字段。
pub type ModelTable = IndexMap<String, IndexMap<String, Value>>;

/// `build.rs` 生成的三张表 + provider 顺序 + manifest 时间戳。
///
/// 单独聚成一个结构体，是为了让 `CATALOG` 只解析一次 JSON，并让时间戳、provider 顺序与
/// 三张表共享同一份解析结果；对外通过下面的查询函数与 `pub static` 暴露，结构体本身保持私有。
struct GeneratedCatalog {
    /// `getBuiltinModelDataGeneratedAt()` 的值；`Date.parse` 失败时为 `None`（TS 的 `undefined`）。
    ///
    /// JSON 里是非整数/浮点数/`null` 时 `as_i64()` 也返回 `None`；build.rs 只会在
    /// `Date.parse` 失败时写 `null`，所以正常数据不会落到这个分支。
    generated_at_ms: Option<i64>,
    /// provider 顺序（`build.rs` 的 `PROVIDER_ORDER`，即 §2.3.1）。
    providers: Vec<String>,
    /// chat 表（`MODELS`）。
    models: ModelTable,
    /// image 表（`IMAGE_MODELS`）。
    image_models: ModelTable,
    /// classifier 表（`CLASSIFIER_MODELS`）。
    classifier_models: ModelTable,
}

static CATALOG: LazyLock<GeneratedCatalog> = LazyLock::new(|| {
    // 阶段 1：解析 include_str! 进来的构建产物。失败即产物损坏（不变量），直接 panic。
    let mut root: Value = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/model_catalog.json"
    )))
    .expect("build.rs emitted a valid model catalog document");
    let object = root
        .as_object_mut()
        .expect("build.rs emitted a model catalog object");

    // 阶段 2：取出时间戳与 provider 顺序。两者都是必需键；顺序数组里的非字符串元素被
    // 过滤掉（正常数据不会出现，生成器保证全是 provider id）。
    let generated_at_ms = object
        .remove("generatedAt")
        .expect("model catalog carries a generation timestamp")
        .as_i64();
    let providers = object
        .remove("providers")
        .and_then(|value| value.as_array().cloned())
        .expect("model catalog carries the provider order")
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();

    // 阶段 3：把三张表搬成 `ModelTable`。缺失的表或非对象 provider 值都按构建产物损坏
    // panic；键序原样保留（serde_json 的 preserve_order）。
    let take_table = |object: &mut serde_json::Map<String, Value>, key: &str| -> ModelTable {
        let table = object
            .remove(key)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_else(|| panic!("model catalog is missing the {key} table"));
        table
            .into_iter()
            .map(|(provider, models)| {
                // 每个 provider 的值必须是「模型 id -> 模型对象」的对象；否则数据形状与
                // 生成器约定不符，按不变量 panic。
                let models = models.as_object().cloned().unwrap_or_else(|| {
                    panic!("model catalog table {key}.{provider} is not an object")
                });
                (provider, models.into_iter().collect())
            })
            .collect()
    };
    // 阶段 3（续）：三张表都必须存在；build.rs 总是同时写出它们。
    let models = take_table(object, "models");
    let image_models = take_table(object, "imageModels");
    let classifier_models = take_table(object, "classifierModels");

    GeneratedCatalog {
        generated_at_ms,
        providers,
        models,
        image_models,
        classifier_models,
    }
});

/// `getBuiltinModelDataGeneratedAt()`：`.manifest.json` 的 `generatedAt`（Unix 毫秒）。
///
/// TS 是 `Date.parse(manifest.generatedAt)`，`NaN` 时返回 `undefined`；`build.rs` 用同一套
/// `Date.parse` 语义解析并把失败写成 `null`，这里还原成 `None`。正常数据下恒为 `Some`。
/// `kiana-config` 的远程目录用它作为本地目录的时间基准，与远端 catalog 的
/// `lastModified` 比较决定是否采用远端数据（`is_newer`）。
///
/// TS: `providers/all.ts#getBuiltinModelDataGeneratedAt`
pub fn get_builtin_model_data_generated_at() -> Option<i64> {
    CATALOG.generated_at_ms
}

/// `MODELS`：chat 模型的静态表（`getBuiltinProviders()` 的键序来源）。
///
/// 首次解引用触发 `CATALOG` 初始化；之后共享同一份数据。外层按 §2.3.1 排序，
/// 内层按模型在数据文件中的摊平顺序排序。
///
/// TS: `models.generated.ts#MODELS`
pub static MODELS: LazyLock<&'static ModelTable> = LazyLock::new(|| &CATALOG.models);

/// `IMAGE_MODELS`。
///
/// image 模型表；未知 provider 的查询由 `get_image_models` 返回空 `Vec`，表本身仍包含
/// 全部 42 个 provider 键（即使某个 provider 没有 image 模型）。
///
/// TS: `models.generated.ts#IMAGE_MODELS`
pub static IMAGE_MODELS: LazyLock<&'static ModelTable> = LazyLock::new(|| &CATALOG.image_models);

/// `CLASSIFIER_MODELS`。
///
/// classifier 模型表；结构同 [`IMAGE_MODELS`]。
///
/// TS: `models.generated.ts#CLASSIFIER_MODELS`
pub static CLASSIFIER_MODELS: LazyLock<&'static ModelTable> =
    LazyLock::new(|| &CATALOG.classifier_models);

/// provider id 的顺序（`Object.keys(MODELS)`）。
///
/// 等于 `build.rs` 的 `PROVIDER_ORDER`（`spec/ai-core.md` §2.3.1），不是 manifest 的
/// 码元序。返回切片由 `CATALOG` 持有，进程内稳定。
///
/// TS: `providers/all.ts#getBuiltinProviders`
pub fn provider_ids() -> &'static [String] {
    &CATALOG.providers
}

/// `getBuiltinModel(provider, modelId)`：缺 provider/model 时为 `None`。
///
/// 返回目录里的原始 JSON 引用（含 `Model` 未声明的键）；不做类型化、不复制数据。
///
/// TS: `providers/all.ts#getBuiltinModel`
pub fn get_model(provider: &str, model_id: &str) -> Option<&'static Value> {
    CATALOG.models.get(provider)?.get(model_id)
}

/// `getBuiltinImageModel(provider, modelId)`。
///
/// 只在 image 表中查找；返回原始 JSON 引用。
///
/// TS: `providers/all.ts#getBuiltinImageModel`
pub fn get_image_model(provider: &str, model_id: &str) -> Option<&'static Value> {
    CATALOG.image_models.get(provider)?.get(model_id)
}

/// `getBuiltinClassifierModel(provider, modelId)`。
///
/// 只在 classifier 表中查找；返回原始 JSON 引用。
///
/// TS: `providers/all.ts#getBuiltinClassifierModel`
pub fn get_classifier_model(provider: &str, model_id: &str) -> Option<&'static Value> {
    CATALOG.classifier_models.get(provider)?.get(model_id)
}

/// `getBuiltinModels(provider)`：`Object.values(MODELS[provider] ?? {})`，未知 provider 为 `[]`。
///
/// 顺序是模型在数据文件中的摊平顺序；每个元素都是目录原始 JSON 的引用。
///
/// TS: `providers/all.ts#getBuiltinModels`
pub fn get_models(provider: &str) -> Vec<&'static Value> {
    CATALOG
        .models
        .get(provider)
        .map(|models| models.values().collect())
        .unwrap_or_default()
}

/// `getBuiltinImageModels(provider)`。
///
/// 未知 provider 返回空 `Vec`；其余同 [`get_models`]。
///
/// TS: `providers/all.ts#getBuiltinImageModels`
pub fn get_image_models(provider: &str) -> Vec<&'static Value> {
    CATALOG
        .image_models
        .get(provider)
        .map(|models| models.values().collect())
        .unwrap_or_default()
}

/// `getBuiltinClassifierModels(provider)`。
///
/// 未知 provider 返回空 `Vec`；其余同 [`get_models`]。
///
/// TS: `providers/all.ts#getBuiltinClassifierModels`
pub fn get_classifier_models(provider: &str) -> Vec<&'static Value> {
    CATALOG
        .classifier_models
        .get(provider)
        .map(|models| models.values().collect())
        .unwrap_or_default()
}

/// `getAllBuiltinModels(provider)`：chat + image + classifier 三段拼接。
///
/// 与 [`crate::providers::catalog_all_models`] 的区别：本函数返回原始 JSON 引用，
/// 后者会按 `type` 解析成 `AnyModel`。未知 provider 返回空 `Vec`。
///
/// TS: `providers/all.ts#getAllBuiltinModels`
pub fn get_all_models(provider: &str) -> Vec<&'static Value> {
    let mut all = get_models(provider);
    all.extend(get_image_models(provider));
    all.extend(get_classifier_models(provider));
    all
}

// 测试锁定三类不变量：provider 顺序与 spec/ai-core.md §2.3.1 完全一致且三张表各有 42 个
// provider 键；未知 provider 的所有查询返回空；vendored manifest 的 generatedAt 能解析成
// 预期毫秒值。`known_models_are_present` 额外确认几种模型类型确实来自对应分片。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_order_matches_spec_2_3_1() {
        let expected = [
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
        assert_eq!(provider_ids(), expected);
        assert_eq!(MODELS.len(), 42);
        assert_eq!(IMAGE_MODELS.len(), 42);
        assert_eq!(CLASSIFIER_MODELS.len(), 42);
    }

    #[test]
    fn unknown_provider_yields_empty_lists() {
        assert!(get_models("not-a-provider").is_empty());
        assert!(get_image_models("not-a-provider").is_empty());
        assert!(get_classifier_models("not-a-provider").is_empty());
        assert!(get_all_models("not-a-provider").is_empty());
        assert!(get_model("not-a-provider", "x").is_none());
        assert!(get_image_model("not-a-provider", "x").is_none());
        assert!(get_classifier_model("not-a-provider", "x").is_none());
    }

    #[test]
    fn generated_at_matches_manifest() {
        // 2026-10-26T11:07:41.777Z
        assert_eq!(
            get_builtin_model_data_generated_at(),
            Some(1_793_012_861_777)
        );
    }

    #[test]
    fn known_models_are_present() {
        assert_eq!(
            get_model("anthropic", "claude-haiku-4-5").unwrap()["api"],
            Value::String("anthropic-messages".into())
        );
        assert_eq!(
            get_model("radius", "balanced").unwrap()["provider"],
            Value::String("radius".into())
        );
        assert!(!get_classifier_models("typesafe").is_empty());
        assert!(!get_image_models("openrouter").is_empty());
    }
}
