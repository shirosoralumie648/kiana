//! `image-models.ts` / `images.ts` / `images-api-registry.ts`。
//!
//! 三个 TS 文件都是围绕 `IMAGE_MODELS` 与图像 api 注册表的小门面，且 `images.ts` 在导入时
//! 执行 `./providers/images/register-builtins.ts` 的注册副作用（在 Rust 里由
//! `ensure_builtins` 在首次访问注册表/`generate_images` 时完成，见下）。
//!
//! ## image-models.ts（@deprecated 兼容读取）
//!
//! 由 `IMAGE_MODELS` 构建 `imageModelsByProvider: Map<provider, Map<modelId, ImageModel>>`
//! （模块加载时，仅保留非空 provider）。Rust 的 [`crate::providers::catalog::IMAGE_MODELS`]
//! 已经是「provider → (model.id → 模型)」的形状，因此：
//! - [`get_image_model`]：`None` 表示 TS 的 `undefined`；
//! - [`get_image_providers`]：非空 provider（顺序即目录顺序）；
//! - [`get_image_models`]：缺失 provider 返回 `[]`。
//!
//! ## images-api-registry.ts
//!
//! `Map<api, {provider, sourceId?}>`；后注册覆盖前者。[`register_images_api_provider`] 用
//! `wrapGenerateImages` 检查 `model.api !== api`。TS 在那里**同步抛** `Error("Mismatched api:
//! …")`；Rust 的 [`crate::models::ProviderImages`] 契约是「不得 reject」，因此包装层把这条
//! 错误映射成 `stopReason: "error"` 的 [`AssistantImages`]（`errorMessage` 是当前稳定契约，
//! 改动需同步测试）。
//!
//! ## images.ts（旧全局入口）
//!
//! [`generate_images`] 从注册表取实现并委派；缺失实现时 TS 抛
//! `Error("No API provider registered for api: ${api}")`，Rust 用 `Err(AiError)` 表达
//! （`Result` 的 `Err` 即 TS 的 Promise rejection）。
//!
//! ## 模块加载副作用
//!
//! TS 导入 `images.ts` 即注册 `openrouter-images`。Rust 没有模块加载时机，因此注册表在首次
//! 被访问时执行一次 [`crate::providers::images::register_built_in_images_api_providers`]
//! （`ensure_builtins`，`OnceLock` 只执行一次），语义等价。
//!
//! # 注册表与并发
//!
//! - 注册表是进程级全局 `Mutex<IndexMap<api, ...>>`，键是 api 字符串；后注册覆盖先注册，
//!   没有注销入口（TS 也没有）。读取时克隆 `Arc` 后立即释放锁，**不跨 `await` 持锁**。
//! - `ensure_builtins` 用 `OnceLock` 只执行一次；`insert_images_api_provider` 是不触发
//!   内置注册的插入函数，供 `providers::images` 使用，避免 `OnceLock::get_or_init` 重入。
//! - 锁中毒时取回内部值继续（`poisoned.into_inner()`），注册表不会因其它线程 panic 而失效。
//!
//! # 失败形状（容易混淆）
//!
//! `ProviderImages` 的契约是“不得 reject”，因此失败有两种形状：
//! - 注册表缺失 → [`generate_images`] 返回 `Err(AiError)`；
//! - api 不匹配 → `Ok(AssistantImages { stop_reason: Error, ... })`（包装层契约是
//!   「不得 reject」，故用结果对象表达）。
//!
//! 第二种形状在 [`generate_images`] 的路径上**不可达**：它用
//! `resolve_images_api_provider(&model.api)` 按 `model.api` 查表，而
//! `insert_images_api_provider` 也以 `provider.api` 为键，因此取到的
//! [`ImagesApiProviderInternal`] 的 api 必然等于 `model.api`。该分支只有在调用方先通过
//! [`get_images_api_provider`] 取得 [`ImagesApiProviderInternal`]、再用 `api` 字段与之不同的
//! `model` 直接调用其 `generate_images` 时才触发（测试
//! `mismatched_api_surfaces_as_an_error_result` 就是这样绕过 [`generate_images`] 构造的）。
//!
//! `generate_images` 返回的 `Ok` 仍可能是 error 结果：适配器会把请求失败折叠进
//! `stop_reason`（如 `OpenRouterImagesApi`）。因此调用方仍需检查 `stop_reason`，但那是
//! 适配器失败，不是上面的 api 不匹配分支。
//!
//! # 调用方
//!
//! - `compat.rs` 通过 `pub use crate::image_models::*` 暴露旧全局 API（`generate_images` 等）。
//! - `kiana-extensions` 的 JS 虚拟模块桥用 [`get_image_model`] / [`get_image_models`] /
//!   [`get_image_providers`] 回答扩展运行时的模型查询。
//! - `providers::images` 只使用 [`ImagesApiProvider`] 与 `insert_images_api_provider` 做内置注册。

use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use indexmap::IndexMap;

use crate::error::AiError;
use crate::models::{ImagesOptions, ProviderImages};
use crate::types::{Api, AssistantImages, ImagesContext, ImagesStopReason, Model};

/// `ImagesApiFunction`：`(model, context, options?) => Promise<AssistantImages>`。
///
/// Rust 侧以 [`ProviderImages`] trait 表达同一契约（trait 方法就是该函数）。
pub type ImagesApiFunction = Arc<dyn ProviderImages>;

// ---------------------------------------------------------------------------
// image-models.ts
// ---------------------------------------------------------------------------

/// `getImageModel(provider, modelId)`：找不到返回 `None`（TS 的 `undefined`）。
///
/// 每次调用都从生成目录的 JSON 反序列化出一个新的 [`Model`]（owned 值），不是静态引用；
/// 目录里未声明的键在 [`Model`] 中不可见（见 `providers::all::parse_model`）。目录条目预期
/// 总是合法的 Model 文档；若生成数据损坏，`parse_model` 会 panic 而不是返回 `None`。
///
/// TS: `image-models.ts#getImageModel`
pub fn get_image_model(provider: &str, model_id: &str) -> Option<Model> {
    crate::providers::all::get_builtin_image_model(provider, model_id)
}

/// `getImageProviders()`：`Array.from(imageModelsByProvider.keys())`。
///
/// 只返回至少有一个图像模型的 provider，顺序即生成目录 `IMAGE_MODELS` 的顺序（IndexMap
/// 的插入序，构建时固定）。
///
/// TS: `image-models.ts#getImageProviders`
pub fn get_image_providers() -> Vec<String> {
    crate::providers::catalog::IMAGE_MODELS
        .iter()
        .filter(|(_, models)| !models.is_empty())
        .map(|(provider, _)| provider.clone())
        .collect()
}

/// `getImageModels(provider)`：缺失 provider 返回 `[]`。
///
/// 每次调用都重新反序列化目录条目（返回 owned `Vec<Model>`）；顺序即目录中的插入序。
///
/// TS: `image-models.ts#getImageModels`
pub fn get_image_models(provider: &str) -> Vec<Model> {
    crate::providers::all::get_builtin_image_models(provider)
}

// ---------------------------------------------------------------------------
// images-api-registry.ts
// ---------------------------------------------------------------------------

/// 注册项：`{ api, generateImages }`。
///
/// TS: `images-api-registry.ts#ImagesApiProvider`
#[derive(Clone)]
pub struct ImagesApiProvider {
    /// `api`（TS 的 `ImageApi`）。
    pub api: Api,
    /// `generateImages`。
    pub generate_images: Arc<dyn ProviderImages>,
}

impl ImagesApiProvider {
    /// 由 [`ProviderImages`] 实现构造注册项。
    pub fn from_provider(api: impl Into<Api>, generate_images: Arc<dyn ProviderImages>) -> Self {
        ImagesApiProvider {
            api: api.into(),
            generate_images,
        }
    }
}

/// `ImagesApiProviderInternal`：包装后的注册项（api 不匹配检查在这里）。
///
/// 注册表实际保存的是这个类型；它内部的 [`ProviderImages`] 被 `CheckedImagesProvider`
/// 包了一层，因此即使调用方绕过 [`generate_images`] 直接调用本类型的方法，也会执行
/// `model.api` 校验。
///
/// TS: `images-api-registry.ts#ImagesApiProviderInternal`
#[derive(Clone)]
pub struct ImagesApiProviderInternal {
    api: Api,
    generate_images: Arc<dyn ProviderImages>,
}

impl ImagesApiProviderInternal {
    /// `api`。
    pub fn api(&self) -> &str {
        &self.api
    }

    /// `generateImages(model, context, options?)`。
    ///
    /// TS 的包装函数在 `model.api !== api` 时抛错；Rust 映射成 error 形状的结果。
    pub async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        self.generate_images
            .generate_images(model, context, options)
            .await
    }
}

/// `Mismatched api: ${model.api} expected ${api}`（TS 的同步抛错文本）。
///
/// 文本必须逐字保持：测试 `mismatched_api_surfaces_as_an_error_result` 直接断言它。
fn mismatched_api_error(model: &Model, expected: &str) -> AiError {
    AiError::new(
        crate::error::AiErrorKind::Request,
        format!("Mismatched api: {} expected {}", model.api, expected),
    )
}

/// `wrapGenerateImages(api, generateImages)`。
struct CheckedImagesProvider {
    api: Api,
    inner: Arc<dyn ProviderImages>,
}

#[async_trait::async_trait]
impl ProviderImages for CheckedImagesProvider {
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        if model.api != self.api {
            // TS 在这里 `throw`；`ProviderImages` 的实现不得 reject，因此返回 error 结果
            // （与 `providers/images/register-builtins.ts` 的 lazy-load 错误形状一致）。
            return error_images(model, mismatched_api_error(model, &self.api).message);
        }
        self.inner.generate_images(model, context, options).await
    }
}

/// `createLazyLoadErrorImages(model, error)` 的等价物。
///
/// 因为 [`ProviderImages`] 不能 reject，错误只能编码在结果对象里：`stop_reason` 设为
/// [`ImagesStopReason::Error`]、`error_message` 承载文本、`output` 为空。`timestamp` 用
/// 当前 Unix 毫秒（TS 的 `Date.now()`）。
fn error_images(model: &Model, message: impl Into<String>) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Error,
        error_message: Some(message.into()),
        timestamp: now_ms(),
    }
}

/// `Date.now()`：Unix 毫秒；系统时钟早于 epoch 时退化为 0（只用于结果的时间戳字段）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

struct RegisteredImagesApiProvider {
    provider: Arc<ImagesApiProviderInternal>,
    /// TS 保存 `sourceId` 但图像注册表没有 `unregister*` 入口；保留字段以对齐注册项形状。
    #[allow(dead_code)]
    source_id: Option<String>,
}

/// 进程级注册表：api 字符串 → 包装后的注册项。
///
/// `IndexMap` 保留插入顺序（与 TS 的 `Map` 一致），但当前没有按顺序遍历的公开接口；
/// 后注册直接覆盖同一 key。全局单例、无失效机制，测试需要覆盖时同样靠“后注册覆盖”。
static REGISTRY: LazyLock<Mutex<IndexMap<String, RegisteredImagesApiProvider>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

/// 取注册表锁；中毒时取回内部值（见模块文档的并发约定）。
fn lock_registry() -> std::sync::MutexGuard<'static, IndexMap<String, RegisteredImagesApiProvider>>
{
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 注册内置图像 api（`openrouter-images`），只执行一次。
///
/// TS 在 `images.ts` 导入时执行 `./providers/images/register-builtins.ts` 的模块级
/// `registerBuiltInImagesApiProviders()`；Rust 在首次访问注册表时执行。
fn ensure_builtins() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(crate::providers::images::register_built_in_images_api_providers);
}

/// 不做内置注册的插入（供 `register-builtins` 使用，避免 `OnceLock` 重入）。
pub(crate) fn insert_images_api_provider(provider: ImagesApiProvider, source_id: Option<String>) {
    let api = provider.api.clone();
    let internal = Arc::new(ImagesApiProviderInternal {
        api: api.clone(),
        generate_images: Arc::new(CheckedImagesProvider {
            api: api.clone(),
            inner: provider.generate_images,
        }),
    });
    lock_registry().insert(
        api,
        RegisteredImagesApiProvider {
            provider: internal,
            source_id,
        },
    );
}

/// `registerImagesApiProvider(provider, sourceId?)`：后注册覆盖前者。
///
/// 先执行一次内置注册（`ensure_builtins`），再插入调用方的项；因此同 api 的自定义实现会
/// 覆盖内置的 `openrouter-images`，且不受调用顺序影响。`source_id` 只保存不使用（TS 保存
/// 它用于来源标识，但图像注册表没有查询/注销接口）。
///
/// TS: `images-api-registry.ts#registerImagesApiProvider`
pub fn register_images_api_provider(provider: ImagesApiProvider, source_id: Option<String>) {
    ensure_builtins();
    insert_images_api_provider(provider, source_id);
}

/// `getImagesApiProvider(api)`。
///
/// 返回注册项的共享 `Arc`（调用方之间共享同一个包装器），未注册时 `None`。访问会先触发
/// 内置注册，因此首次调用永远能看到 `openrouter-images`。
///
/// TS: `images-api-registry.ts#getImagesApiProvider`
pub fn get_images_api_provider(api: &str) -> Option<Arc<ImagesApiProviderInternal>> {
    ensure_builtins();
    lock_registry()
        .get(api)
        .map(|entry| Arc::clone(&entry.provider))
}

// ---------------------------------------------------------------------------
// images.ts
// ---------------------------------------------------------------------------

/// `resolveImagesApiProvider(api)`：缺失时 TS 抛 `No API provider registered for api: ${api}`。
#[allow(clippy::result_large_err)] // `AiError` 是共享契约类型，与 TS 的 `Error` 一一对应。
fn resolve_images_api_provider(api: &str) -> Result<Arc<ImagesApiProviderInternal>, AiError> {
    get_images_api_provider(api).ok_or_else(|| {
        AiError::new(
            crate::error::AiErrorKind::Unsupported,
            format!("No API provider registered for api: {api}"),
        )
    })
}

/// 全局图像生成：按 `model.api` 从注册表分派；鉴权必须由调用方通过
/// `options.api_key` 显式传入（与 TS 文档一致；新代码用 `Models.generateImages()`）。
///
/// 错误语义：
/// - 注册表里没有该 api → `Err`（[`crate::error::AiErrorKind::Unsupported`]，消息逐字为
///   `No API provider registered for api: <api>`），对应 TS 的 rejection；
/// - `model.api` 与注册项不一致导致的 error 结果（`Ok` +
///   `AssistantImages { stop_reason: Error }`，`ProviderImages` 不得 reject）在**本函数上
///   不可达**：注册表以 `provider.api` 为键，而这里也按 `model.api` 查表，取到的包装器 api
///   必然一致。该形状只会出现在“先用 [`get_images_api_provider`] 拿到
///   [`ImagesApiProviderInternal`]、再用 api 不同的 `model` 直接调用”的路径上；
/// - 适配器自身失败时也返回 `Ok` + `stop_reason: Error`（`error_message` 带原因），因此调用
///   方仍需检查 `stop_reason`。
///
/// 分派在取到 `Arc` 后释放注册表锁，再 `await` 适配器；调用期间注册表被并发改写不会
/// 影响本次调用使用的实现。
///
/// TS: `images.ts#generateImages`
#[allow(clippy::result_large_err)] // 与 TS 的 `Promise` rejection 对应，`AiError` 是共享契约类型。
pub async fn generate_images(
    model: &Model,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> Result<AssistantImages, AiError> {
    let provider = resolve_images_api_provider(&model.api)?;
    Ok(provider.generate_images(model, context, options).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantImages, ImageKind, ImagesStopReason, TextContent, TextKind, UserContentBlock,
    };

    fn image_model() -> Model {
        Model {
            model_type: Some(crate::types::ModelType::Image),
            id: "test-image-model".into(),
            name: "Test Image Model".into(),
            api: "test-images".into(),
            provider: "test-provider".into(),
            base_url: "https://example.test/v1".into(),
            output: Some(vec!["image".into()]),
            ..Model::default()
        }
    }

    fn context() -> ImagesContext {
        ImagesContext {
            input: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "a red circle".into(),
                text_signature: None,
            })],
        }
    }

    struct FixedImages(AssistantImages);

    #[async_trait::async_trait]
    impl ProviderImages for FixedImages {
        async fn generate_images(
            &self,
            _model: &Model,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
        ) -> AssistantImages {
            self.0.clone()
        }
    }

    fn ok_images(model: &Model) -> AssistantImages {
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: vec![UserContentBlock::Image(crate::types::ImageContent {
                kind_image: ImageKind::Image,
                data: "aGVsbG8=".into(),
                mime_type: "image/png".into(),
            })],
            response_id: None,
            usage: None,
            stop_reason: ImagesStopReason::Stop,
            error_message: None,
            timestamp: 1,
        }
    }

    #[test]
    fn image_catalog_reads_match_the_generated_tables() {
        // 目录里只有 openrouter 有 image 模型（data/openrouter.json 的 openrouter-images 组）。
        assert_eq!(get_image_providers(), vec!["openrouter".to_string()]);
        let models = get_image_models("openrouter");
        assert!(!models.is_empty());
        let first = &models[0];
        assert_eq!(first.model_type, Some(crate::types::ModelType::Image));
        assert_eq!(first.provider, "openrouter");
        assert_eq!(first.api, "openrouter-images");
        assert_eq!(
            get_image_model("openrouter", &first.id).as_ref(),
            Some(first)
        );
        assert_eq!(get_image_model("openrouter", "does-not-exist"), None);
        assert_eq!(get_image_model("nope", "x"), None);
        assert!(get_image_models("nope").is_empty());
    }

    #[tokio::test]
    async fn generate_images_dispatches_and_reports_missing_api() {
        let model = image_model();
        let fixed = ok_images(&model);
        register_images_api_provider(
            ImagesApiProvider::from_provider("test-images", Arc::new(FixedImages(fixed.clone()))),
            Some("unit-test".into()),
        );
        let result = generate_images(&model, &context(), None).await.unwrap();
        assert_eq!(result, fixed);

        let mut unknown = model.clone();
        unknown.api = "unknown-images".into();
        let error = generate_images(&unknown, &context(), None)
            .await
            .unwrap_err();
        assert_eq!(
            error.message,
            "No API provider registered for api: unknown-images"
        );
    }

    #[tokio::test]
    async fn mismatched_api_surfaces_as_an_error_result() {
        let model = image_model();
        let provider = ImagesApiProvider::from_provider(
            "test-images",
            Arc::new(FixedImages(ok_images(&model))),
        );
        register_images_api_provider(provider, None);
        let mut other = model.clone();
        other.api = "test-images".into();
        // 直接调用注册表内部实现，`model.api` 与注册 api 不同。
        let internal = get_images_api_provider("test-images").unwrap();
        let mut different = other.clone();
        different.api = "other-api".into();
        let images = internal.generate_images(&different, &context(), None).await;
        assert_eq!(images.stop_reason, ImagesStopReason::Error);
        assert_eq!(
            images.error_message.as_deref(),
            Some("Mismatched api: other-api expected test-images")
        );
    }

    #[test]
    fn builtin_openrouter_images_api_is_registered_lazily() {
        // 首次访问即注册 `openrouter-images`（TS 的 images.ts 导入副作用）。
        let provider = get_images_api_provider("openrouter-images").expect("builtin registered");
        assert_eq!(provider.api(), "openrouter-images");
        // 后注册覆盖前者。
        let replacement = OkImagesForOverride;
        register_images_api_provider(
            ImagesApiProvider::from_provider("openrouter-images", Arc::new(replacement)),
            Some("override".into()),
        );
        assert!(Arc::ptr_eq(
            &get_images_api_provider("openrouter-images").unwrap(),
            &get_images_api_provider("openrouter-images").unwrap()
        ));
    }

    struct OkImagesForOverride;

    #[async_trait::async_trait]
    impl ProviderImages for OkImagesForOverride {
        async fn generate_images(
            &self,
            model: &Model,
            _context: &ImagesContext,
            _options: Option<&ImagesOptions>,
        ) -> AssistantImages {
            ok_images(model)
        }
    }
}
