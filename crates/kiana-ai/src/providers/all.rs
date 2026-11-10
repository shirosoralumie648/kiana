//! providers/all.ts：builtin provider 表与目录读取入口。
//!
//! TS: `packages/ai/src/providers/all.ts`
//!
//! # 这个文件负责什么
//!
//! TS 的 `providers/all.ts` 是模型目录的门面：把 `models.generated.ts` 的三张静态表
//! （`MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS`）包装成 `getBuiltin*` 查询，并负责
//! 构造 42 个内置 provider。Rust 侧沿用同一分界：
//!
//! - 原始 JSON 表、provider 顺序与 `generatedAt` 时间戳在 [`crate::providers::catalog`]
//!   （由 `build.rs` 生成并校验）；
//! - 本模块只做「原始 JSON → 类型化 [`Model`]」与「把 provider 注册进 `Models` 集合」两件事。
//!
//! # 主要流程
//!
//! - 查询：`get_builtin_model*` 先向 `catalog` 取 `&Value`，再由私有 `parse_model` 反序列化。
//!   未知 provider/model 返回 `None` 或空 `Vec`（对应 TS 的 `MODELS[p]?.[id]` 与 `?? {}`）。
//! - 组装：[`builtin_providers`] 每次调用都重新实例化全部 42 个 provider，顺序固定为
//!   `spec/ai-core.md` §2.3.1；[`builtin_models`] 在其上建一个 `Models` 集合，逐个
//!   `set_provider` 后返回。
//!
//! # 调用方
//!
//! - `compat` 的旧全局 API `getModel`/`getModels`/`getProviders`：三者分别直接转发到本模块的
//!   [`get_builtin_model`]/[`get_builtin_models`]/[`get_builtin_providers`]
//!   （`compat.rs` 中的同名函数，不经过 `COMPAT_MODELS`）；
//! - `compat::COMPAT_MODELS`（`builtin_models(None)` 的 `LazyLock` 缓存）：不供上述三个旧
//!   查询函数使用，而是供 `compat::stream`/`stream_simple` 的内置 provider 查找与
//!   Cloudflare 回退分支使用；
//! - `cli`（列 provider/login 目标）、`image_models`；
//! - `kiana-config` 的模型运行时与 `kiana-extensions` 的 JS 虚拟模块（内置目录快照）。
//!
//! # 约束与失败模式
//!
//! - 目录 JSON 由 `build.rs` 在编译期校验（sha256、structureHash、字段形状）；运行期解析
//!   失败只能意味着构建产物损坏，因此 `parse_model` 直接 panic，而不是返回 `Result`。
//! - [`Model`] 是冻结的固定结构体：目录里 `Model` 未声明的键（radius 的 `lab`/`enabled`/
//!   `providers`、`recursive/flint` 的 `free` 等）会在类型化视图中丢失，原始键序也不保留。
//!   需要逐字往返的调用方必须直接使用 `catalog` 的原始 `Value`。注意 `ModelsStoreEntry::raw_models`
//!   只是同一「保留原始 JSON」思路的另一处应用：它的数组来自持久化的 models store
//!   （`models::store` 反序列化时原样保留 `models` 键），并不经过本模块的 `catalog`。
//!   真正读取本模块原始条目的是 `kiana-agent-core` 的 `wire_keys::lookup_catalog_model`
//!   这类调用方，它用原始 JSON 的键序与全部字段做覆盖层合并。
//! - [`builtin_providers`] 不是零成本调用：每次都会重新解析所有 provider 的目录条目；
//!   需要多次使用时调用方应缓存结果（`compat` 用 `LazyLock` 缓存整个 `Models` 集合）。

use std::sync::Arc;

use serde_json::Value;

use crate::models::{CreateModelsOptions, MutableModels, Provider, create_models};
use crate::providers::catalog;
use crate::types::{AnyModel, Model};

// TS 的 `all.ts` 里有一行 `export { radiusProvider }`：半径 provider 的工厂与选项类型
// 由 `radius.rs` 实现，但对外入口在 all 模块，因此这里原样再导出（`kiana-config` 等
// 调用方按 TS 的习惯从 all 取）。
pub use crate::providers::radius::{RadiusProviderOptions, radius_provider};

/// 把目录里的一条模型 JSON 解析成 [`Model`]。
///
/// TS 的 `getBuiltin*` 返回目录里的**原始对象**（`flattenModelCatalog` 保留对象引用），
/// 因此数据文件里的额外键——radius 的 `lab`/`enabled`/`providers`/`free`——以及数据的
/// 原始键序对调用方可见。Rust 的 [`Model`] 是冻结的固定结构体（`types.rs`），未声明的键
/// 在反序列化时被丢弃，重新序列化时按键结构体字段序输出。持久化不受影响：
/// [`crate::models::ModelsStoreEntry::raw_models`] 逐字保留原始 JSON 数组（无损往返）；
/// 只有直接读取 [`Model`] 的库调用方看不到这些键（与 `radius_config.rs` 模块文档记录的
/// 是同一个限制）。
fn parse_model(value: &Value) -> Model {
    serde_json::from_value(value.clone()).expect("model catalog entry is a valid Model document")
}

/// `getBuiltinModel(provider, modelId)`：缺 provider/model 时为 `None`。
///
/// 只在 chat 表中查找；返回的类型化 [`Model`] 会丢弃目录条目里 [`Model`] 未声明的键。
/// 目录本身在编译期已校验，因此 `Some` 时反序列化不会失败。
///
/// TS: `providers/all.ts#getBuiltinModel`
pub fn get_builtin_model(provider: &str, model_id: &str) -> Option<Model> {
    catalog::get_model(provider, model_id).map(parse_model)
}

/// `getBuiltinImageModel(provider, modelId)`。
///
/// 只在 image 表中查找，语义与 [`get_builtin_model`] 相同（未知时 `None`）。
///
/// TS: `providers/all.ts#getBuiltinImageModel`
pub fn get_builtin_image_model(provider: &str, model_id: &str) -> Option<Model> {
    catalog::get_image_model(provider, model_id).map(parse_model)
}

/// `getBuiltinClassifierModel(provider, modelId)`。
///
/// 只在 classifier 表中查找，语义与 [`get_builtin_model`] 相同（未知时 `None`）。
///
/// TS: `providers/all.ts#getBuiltinClassifierModel`
pub fn get_builtin_classifier_model(provider: &str, model_id: &str) -> Option<Model> {
    catalog::get_classifier_model(provider, model_id).map(parse_model)
}

/// `getBuiltinProviders()`：`Object.keys(MODELS)`（固定顺序）。
///
/// 返回的切片由 `catalog` 的静态表持有，进程内多次调用返回同一份数据；顺序即
/// `spec/ai-core.md` §2.3.1（CLI 的 `list`/help 输出依赖它，不要重排）。
///
/// TS: `providers/all.ts#getBuiltinProviders`
pub fn get_builtin_providers() -> &'static [String] {
    catalog::provider_ids()
}

/// Generation timestamp shared by all built-in provider catalogs.
///
/// 值是 vendored 目录 `.manifest.json` 的 `generatedAt` 解析出的 Unix 毫秒。TS 的
/// `Date.parse` 失败时返回 `undefined`；`build.rs` 把失败写成 JSON `null`，这里因此得到
/// `None`。正常构建下恒为 `Some`（测试锁定了具体时间戳）。
///
/// TS: `providers/all.ts#getBuiltinModelDataGeneratedAt`
pub fn get_builtin_model_data_generated_at() -> Option<i64> {
    catalog::get_builtin_model_data_generated_at()
}

/// `getBuiltinModels(provider)`：`Object.values(MODELS[provider] ?? {})`。
///
/// 返回顺序是模型在生成数据中的摊平顺序；未知 provider 返回空 `Vec`（不报错）。
/// 每个元素都经过 `parse_model`，未声明键会丢失（见模块文档的约束一节）。
///
/// TS: `providers/all.ts#getBuiltinModels`
pub fn get_builtin_models(provider: &str) -> Vec<Model> {
    catalog::get_models(provider)
        .into_iter()
        .map(parse_model)
        .collect()
}

/// `getBuiltinImageModels(provider)`。
///
/// 未知 provider 返回空 `Vec`；其余同 [`get_builtin_models`]。
///
/// TS: `providers/all.ts#getBuiltinImageModels`
pub fn get_builtin_image_models(provider: &str) -> Vec<Model> {
    catalog::get_image_models(provider)
        .into_iter()
        .map(parse_model)
        .collect()
}

/// `getBuiltinClassifierModels(provider)`。
///
/// 未知 provider 返回空 `Vec`；其余同 [`get_builtin_models`]。
///
/// TS: `providers/all.ts#getBuiltinClassifierModels`
pub fn get_builtin_classifier_models(provider: &str) -> Vec<Model> {
    catalog::get_classifier_models(provider)
        .into_iter()
        .map(parse_model)
        .collect()
}

/// `getAllBuiltinModels(provider)`：chat + image + classifier。
///
/// 返回 `AnyModel` 变体，由目录条目的 `type` 字段决定（缺省按 chat 处理）。注意实现走
/// [`crate::providers::catalog_all_models`]（内部用 `parse_catalog_model`），而不是本文件的
/// `parse_model`；两者对 [`Model`] 未知键的处理相同。
///
/// TS: `providers/all.ts#getAllBuiltinModels`
pub fn get_all_builtin_models(provider: &str) -> Vec<AnyModel> {
    crate::providers::catalog_all_models(provider)
}

/// All built-in providers, freshly constructed.
///
/// 顺序固定为 `spec/ai-core.md` §2.3.1 的 42 项（与 [`get_builtin_providers`] 相同），
/// 其中 `radius` 使用 `RadiusProviderOptions::default()`。每次调用都会重新构造函数、
/// 重新解析目录，返回的实例互不共享可变状态；需要长期持有时调用方应自行缓存。
///
/// TS: `providers/all.ts#builtinProviders`
pub fn builtin_providers() -> Vec<Arc<dyn Provider>> {
    use crate::providers::{
        amazon_bedrock::amazon_bedrock_provider, ant_ling::ant_ling_provider,
        anthropic::anthropic_provider, azure::azure_provider, baseten::baseten_provider,
        cerebras::cerebras_provider, cloudflare_ai_gateway::cloudflare_ai_gateway_provider,
        cloudflare_workers_ai::cloudflare_workers_ai_provider, deepseek::deepseek_provider,
        fireworks::fireworks_provider, github_copilot::github_copilot_provider,
        google::google_provider, google_vertex::google_vertex_provider, groq::groq_provider,
        huggingface::huggingface_provider, kimi_coding::kimi_coding_provider, meta::meta_provider,
        minimax::minimax_provider, minimax_cn::minimax_cn_provider, mistral::mistral_provider,
        moonshotai::moonshotai_provider, moonshotai_cn::moonshotai_cn_provider,
        nvidia::nvidia_provider, openai::openai_provider, openai_codex::openai_codex_provider,
        opencode::opencode_provider, opencode_go::opencode_go_provider,
        openrouter::openrouter_provider, qwen_token_plan::qwen_token_plan_provider,
        qwen_token_plan_cn::qwen_token_plan_cn_provider,
        qwen_token_plan_individual::qwen_token_plan_individual_provider,
        together::together_provider, typesafe::typesafe_provider,
        vercel_ai_gateway::vercel_ai_gateway_provider, xai::xai_provider, xiaomi::xiaomi_provider,
        xiaomi_token_plan_ams::xiaomi_token_plan_ams_provider,
        xiaomi_token_plan_cn::xiaomi_token_plan_cn_provider,
        xiaomi_token_plan_sgp::xiaomi_token_plan_sgp_provider, zai::zai_provider,
        zai_coding_cn::zai_coding_cn_provider,
    };
    vec![
        amazon_bedrock_provider(),
        ant_ling_provider(),
        anthropic_provider(),
        azure_provider(),
        baseten_provider(),
        cerebras_provider(),
        cloudflare_ai_gateway_provider(),
        cloudflare_workers_ai_provider(),
        deepseek_provider(),
        fireworks_provider(),
        github_copilot_provider(),
        google_provider(),
        google_vertex_provider(),
        groq_provider(),
        huggingface_provider(),
        kimi_coding_provider(),
        meta_provider(),
        minimax_provider(),
        minimax_cn_provider(),
        mistral_provider(),
        moonshotai_provider(),
        moonshotai_cn_provider(),
        nvidia_provider(),
        openai_provider(),
        openai_codex_provider(),
        opencode_provider(),
        opencode_go_provider(),
        openrouter_provider(),
        qwen_token_plan_provider(),
        qwen_token_plan_cn_provider(),
        qwen_token_plan_individual_provider(),
        radius_provider(RadiusProviderOptions::default()),
        together_provider(),
        typesafe_provider(),
        vercel_ai_gateway_provider(),
        xai_provider(),
        xiaomi_provider(),
        xiaomi_token_plan_ams_provider(),
        xiaomi_token_plan_cn_provider(),
        xiaomi_token_plan_sgp_provider(),
        zai_provider(),
        zai_coding_cn_provider(),
    ]
}

/// A `Models` collection with every built-in provider registered.
///
/// `options` 为 `None` 时等价于 `CreateModelsOptions::default()`（内存凭证存储、默认
/// auth 上下文）。返回的 trait 对象可继续 `set_provider` 覆盖或追加 provider——
/// `set_provider` 按 id 替换，`builtin_models` 之后注册的实例优先。
///
/// TS: `providers/all.ts#builtinModels`
pub fn builtin_models(options: Option<CreateModelsOptions>) -> Arc<dyn MutableModels> {
    let models = create_models(options.unwrap_or_default());
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}
