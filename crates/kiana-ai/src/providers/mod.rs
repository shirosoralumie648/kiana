//! providers 目录：42 个 builtin provider 工厂（顺序见 `spec/ai-core.md` §2.3.1）+ 目录级辅助模块。
//!
//! `providers/<id>.models.ts`（42 个分片）与 `models.generated.ts` 不在源码里逐个展开：
//! 它们的唯一内容是从 `providers/data/<id>.json` 摊平出的静态表。本 crate 用 `build.rs`
//! （阶段 1 加入）在编译期读取同一批 JSON，生成 [`catalog`] 里的有序表；键顺序沿用
//! `spec/ai-core.md` §2.3.1 的 provider 顺序（`IndexMap`）。理由见 `ARCHITECTURE.md` §3.2。
//!
//! # 为什么保留 42 个独立文件
//!
//! 多数 provider 文件是「固定参数 + `createProvider(...)`」的薄工厂；radius、azure、
//! cloudflare-* 及带自定义鉴权/模型过滤的文件包含额外逻辑。Rust 保留同样的文件粒度，
//! 是为了让模块边界与 TS 目录一一对应（`ARCHITECTURE.md` §3.1），每个 provider 的迁移
//! 差异都能逐文件对照 TS 源码；如果压成一张配置表，行号与结构的一一对应就会丢失。
//!
//! # 工厂如何被调用
//!
//! - [`all::builtin_providers`] 按 `spec/ai-core.md` §2.3.1 的固定顺序调用全部工厂，
//!   [`all::builtin_models`] 再把结果逐个 `set_provider` 注册进 `Models`；
//! - 除 radius 手写 `Provider` 外，其余工厂返回 `Arc<dyn Provider>`，内部调用
//!   `crate::models::create_provider`；参数里的 `models` 来自 [`catalog`] 的编译期静态表，
//!   经 [`parse_catalog_model`] 解析；
//! - 每个工厂都至少提供一个实现（`api`、`images` 或 `classifiers`）；`create_provider`
//!   唯一可能失败的条件是三者全空，因此工厂用 `expect` 把这种数据错误变成构造期 panic，
//!   避免拖到第一次 `stream` 才暴露。radius 不经过 `create_provider`，也就不参与这项
//!   三者非空校验（见 `radius.rs` 的模块文档）。
//!
//! # 目录辅助函数
//!
//! [`catalog_chat_models`] 等函数对应 TS 的 `Object.values(<PROVIDER>_MODELS)`。静态表由
//! `catalog::CATALOG`（`LazyLock`）解析一次并缓存；这些辅助函数每次调用只重新遍历对应
//! 表，并把条目反序列化为 [`Model`]（结果不缓存）。返回顺序沿用生成表（`IndexMap` 插入序）。
//! 模型数据的来源与修改禁忌见 `crates/kiana-ai/data/README.md`：数据由 TS 生成器产出并
//! vendored，不能手改单个 JSON。

// --- 子模块 ---
pub mod all;
pub mod catalog;
pub mod cloudflare_auth;
pub mod cloudflare_stream;
pub mod compat_schema;
pub mod faux;
pub mod images;
pub mod mock;
pub mod model_schema;
pub mod opencode_headers;
pub mod openrouter_api;
pub mod radius_config;

pub mod amazon_bedrock;
pub mod ant_ling;
pub mod anthropic;
pub mod azure;
pub mod baseten;
pub mod cerebras;
pub mod cloudflare_ai_gateway;
pub mod cloudflare_workers_ai;
pub mod deepseek;
pub mod fireworks;
pub mod github_copilot;
pub mod google;
pub mod google_vertex;
pub mod groq;
pub mod huggingface;
pub mod kimi_coding;
pub mod meta;
pub mod minimax;
pub mod minimax_cn;
pub mod mistral;
pub mod moonshotai;
pub mod moonshotai_cn;
pub mod nvidia;
pub mod openai;
pub mod openai_codex;
pub mod opencode;
pub mod opencode_go;
pub mod openrouter;
pub mod qwen_token_plan;
pub mod qwen_token_plan_cn;
pub mod qwen_token_plan_individual;
pub mod radius;
pub mod together;
pub mod typesafe;
pub mod vercel_ai_gateway;
pub mod xai;
pub mod xiaomi;
pub mod xiaomi_token_plan_ams;
pub mod xiaomi_token_plan_cn;
pub mod xiaomi_token_plan_sgp;
pub mod zai;
pub mod zai_coding_cn;

// ---------------------------------------------------------------------------
// 目录读取辅助（TS 各工厂的 `Object.values(<PROVIDER>_MODELS)`）
// ---------------------------------------------------------------------------

use serde_json::Value;

use crate::types::{AnyModel, Model, ModelType};

/// 把 `providers/data/*.json` 摊平后的一条模型 JSON 解析成 [`AnyModel`]。
///
/// TS 的目录条目是普通对象（`Model` 的严格超集）；Rust 用 [`Model`] 承载，
/// `type` 缺省为 `"chat"`（与 `getModelType` 一致）。构建期 `build.rs` 已校验数据形状，
/// 因此解析失败属于数据损坏，按不变量 panic。
///
/// TS: `providers/<id>.models.ts` 的 `flattenChatModelCatalog` 等
///
/// # 参数与返回
///
/// - `value`：单条模型对象，必须能反序列化为 [`Model`]。
/// - 返回值按 `type` 选择 [`AnyModel`] 变体；`type` 缺失、为 `null` 或为未知字符串时
///   一律按 [`ModelType::Chat`] 处理。
///
/// # Panics
///
/// `type` 存在但不是字符串，或对象结构与 [`Model`] 不符时 panic。数据由生成器与
/// `build.rs` 保证，运行期不做容错。
pub fn parse_catalog_model(value: &Value) -> AnyModel {
    let model_type = match value.get("type") {
        None | Some(Value::Null) => ModelType::Chat,
        Some(Value::String(name)) => match name.as_str() {
            "image" => ModelType::Image,
            "classifier" => ModelType::Classifier,
            _ => ModelType::Chat,
        },
        Some(other) => panic!("model catalog entry has a non-string type: {other}"),
    };
    let model: Model = serde_json::from_value(value.clone())
        .expect("model catalog entry is a valid Model document");
    match model_type {
        ModelType::Image => AnyModel::Image(model),
        ModelType::Classifier => AnyModel::Classifier(model),
        ModelType::Chat => AnyModel::Chat(model),
    }
}

/// `Object.values(<PROVIDER>_MODELS)`：chat 模型（已解析）。
///
/// TS: `providers/<id>.models.ts`
///
/// 返回顺序与生成表一致；未知 provider 返回空 `Vec`。每次调用都会重新解析目录 JSON，
/// 结果不与调用方共享。
pub fn catalog_chat_models(provider: &str) -> Vec<AnyModel> {
    catalog::get_models(provider)
        .into_iter()
        .map(parse_catalog_model)
        .collect()
}

/// `Object.values(<PROVIDER>_IMAGE_MODELS)`（已解析）。
///
/// 顺序与生成表一致；未知 provider 返回空 `Vec`。
pub fn catalog_image_models(provider: &str) -> Vec<AnyModel> {
    catalog::get_image_models(provider)
        .into_iter()
        .map(parse_catalog_model)
        .collect()
}

/// `Object.values(<PROVIDER>_CLASSIFIER_MODELS)`（已解析）。
///
/// 顺序与生成表一致；未知 provider 返回空 `Vec`。
pub fn catalog_classifier_models(provider: &str) -> Vec<AnyModel> {
    catalog::get_classifier_models(provider)
        .into_iter()
        .map(parse_catalog_model)
        .collect()
}

/// 依次拼接 chat → image → classifier（`getAllBuiltinModels` 的 provider 侧用法）。
///
/// 三段顺序固定；未知 provider 返回空 `Vec`。
pub fn catalog_all_models(provider: &str) -> Vec<AnyModel> {
    let mut models = catalog_chat_models(provider);
    models.extend(catalog_image_models(provider));
    models.extend(catalog_classifier_models(provider));
    models
}
