//! `providers/images/register-builtins.ts`：内置图像 API 的注册。
//!
//! TS 侧只有一个 provider：`openrouter-images`（`api/openrouter-images.ts`）。注册项是
//! [`generateImagesOpenRouter`](register-builtins.ts#generateImagesOpenRouter)，它**惰性**地
//! 动态 `import("../../api/openrouter-images.ts")`，把加载/调用时的异常转换成
//! `createLazyLoadErrorImages`（`stopReason: "error"` + `errorMessage`）。
//!
//! Rust 是静态链接：`api::openrouter_images::openrouter_images_api()` 不会失败，因此本模块
//! 直接委派给该实现（模块加载错误这条分支不可达；保留错误结果形状见
//! [`crate::image_models`] 的 `ProviderImages` 包装）。
//!
//! 模块级副作用对齐：TS 在模块加载时调用
//! `registerBuiltInImagesApiProviders()`；Rust 由 [`crate::image_models`] 的
//! `ensure_builtins()` 在首次访问图像注册表时调用
//! [`register_built_in_images_api_providers`]（`OnceLock` 只执行一次）。
//!
//! # 执行流程（注册 → 分派 → 生成）
//!
//! 1. 首次访问图像注册表时，`image_models` 的 `ensure_builtins` 调用
//!    [`register_built_in_images_api_providers`]；它把 `"openrouter-images"` 与
//!    `Arc<OpenRouterImagesProvider>` 交给内部的 `insert_images_api_provider`。
//! 2. `insert_images_api_provider` 再用 `CheckedImagesProvider` 包一层 `model.api` 一致性
//!    校验，注册表实际保存的是这个包装器（细节见 [`crate::image_models`]）。
//! 3. 旧全局入口 [`crate::image_models::generate_images`]（经 `compat` 再导出）按
//!    `model.api` 取到包装器并调用；校验通过后经本模块的
//!    [`generate_images_open_router`] 转发到
//!    [`crate::api::openrouter_images::generate_images`]，由后者完成请求与响应解析。
//!
//! `Models::generate_images`（新入口）不经过这张注册表：`providers/openrouter.rs` 把同一
//! `openrouter_images_api()` 直接挂进 provider 的 images map，两条路径最终都调用
//! `api::openrouter_images::generate_images`。本模块的两个公开函数也经 `compat` 的
//! `pub use` 再导出（对应 TS `compat.ts` 的 `export *`），兼容入口可直接看到它们。
//!
//! # 设计原因与约束
//!
//! - 注册表要求 `Arc<dyn ProviderImages>`，自由函数不能直接转成 trait object，因此需要
//!   无字段的 `OpenRouterImagesProvider` 适配结构；它没有内部状态，被注册表共享持有。
//! - 注册只能走内部的 `insert_images_api_provider`：公开的
//!   `image_models::register_images_api_provider` 会先执行 `ensure_builtins`，而本函数正是
//!   `ensure_builtins` 的初始化回调，走公开路径会重入同一个 `OnceLock`。
//! - 注册是进程级副作用且没有注销入口；重复调用是覆盖语义（同一 key 换成新的 `Arc`），
//!   最后一次注册生效。`image_models` 的 `builtin_openrouter_images_api_is_registered_lazily`
//!   演示了后注册覆盖，但该测试未显式断言替换生效。
//! - 本模块不解析鉴权：`options` 里没有 `api_key` 时下游返回
//!   `stop_reason = Error`、`error_message = "No API key for provider: <provider>"` 的
//!   结果对象，而不是 `Err` 或 panic。

use std::sync::Arc;

use crate::api::openrouter_images::openrouter_images_api;
use crate::image_models::{ImagesApiProvider, insert_images_api_provider};
use crate::models::ProviderImages;
use crate::types::{AssistantImages, ImagesContext, Model};

/// `generateImagesOpenRouter`：`openrouter-images` 的实现（TS 的惰性加载包装）。
///
/// TS: `providers/images/register-builtins.ts#generateImagesOpenRouter`
///
/// 语义上是“先确保实现已加载，再调用 `generateImages`” /
/// “加载失败则返回 `createLazyLoadErrorImages(model, error)`”：返回值永远是结果对象，
/// 函数本身不返回 `Result`、不把错误抛给调用方。实现上每次调用都经
/// `openrouter_images_api()` 新建一个零大小适配器（TS 缓存的是动态 import 的 Promise，
/// 这里没有缓存，但适配器无状态，不影响可见行为）。
///
/// 参数与返回：
/// - `model`：目标图像模型；输出对象的 `api`/`provider`/`model` 原样取自它的
///   `api`/`provider`/`id`，`base_url` 决定请求地址，`provider` 还会出现在缺 key 的错误
///   文案里。
/// - `context`：图像生成的输入块，原样交给下游。
/// - `options`：`None` 表示没有额外选项，也意味着没有 `api_key`，下游会直接返回
///   `stop_reason = Error`（`"No API key for provider: <provider>"`），不会发请求。
///
/// 失败形态（错误都折叠在返回值里，调用方必须检查 `stop_reason`）：
/// - 缺/空 `api_key`、HTTP 或网络错误、响应解析失败 → `stop_reason = Error`，
///   `error_message` 有值；
/// - 失败发生时 `options.signal` 已取消 → `stop_reason = Aborted`；
/// - 成功 → `stop_reason = Stop`。
///
/// 本函数**不**校验 `model.api` 是否等于 `"openrouter-images"`。注册表路径上的这项检查由
/// `image_models` 的 `CheckedImagesProvider` 在调用前完成；直接调用本函数会跳过它。
pub async fn generate_images_open_router(
    model: &Model,
    context: &ImagesContext,
    options: Option<&crate::models::ImagesOptions>,
) -> AssistantImages {
    // TS: `await loadOpenRouterImagesProviderModule()` 后调用 `module.generateImages`；
    // 任一环节抛错都由 `createLazyLoadErrorImages` 兜底。Rust 的模块加载不可失败，实现本身
    // 也不 reject（`api::openrouter_images::generate_images` 把错误放进结果对象）。
    openrouter_images_api()
        .generate_images(model, context, options)
        .await
}

/// `registerBuiltInImagesApiProviders()`：注册 `openrouter-images`。
///
/// TS: `providers/images/register-builtins.ts#registerBuiltInImagesApiProviders`
///
/// 向进程级图像注册表插入 `api = "openrouter-images"` 的实现；`source_id` 传 `None`，
/// 因为 TS 注册内置项时也没有传（`source_id` 在 Rust 注册表中只保存、不读取）。
///
/// 本函数**不**调用公开的 `image_models::register_images_api_provider`：那条路径会先执行
/// `ensure_builtins`，而本函数正是 `ensure_builtins` 的初始化回调，走公开路径会重入同一个
/// `OnceLock`。重复调用是覆盖语义（同一 key 换成新的 `Arc`），不会累积注册项；这里也没有
/// 对应的注销入口，测试通过“后注册覆盖”替换内置实现。
pub fn register_built_in_images_api_providers() {
    insert_images_api_provider(
        ImagesApiProvider::from_provider(
            "openrouter-images",
            Arc::new(OpenRouterImagesProvider) as Arc<dyn ProviderImages>,
        ),
        None,
    );
}

/// [`generate_images_open_router`] 的 [`ProviderImages`] 适配。
///
/// 注册表存的是 `Arc<dyn ProviderImages>`，而 [`generate_images_open_router`] 是自由函数，
/// 不能直接转成 trait object，因此需要这个零大小转发结构。它没有字段与内部状态，注册表
/// 持有的那一个实例可被所有调用方共享；并发安全由 [`ProviderImages`] 的 `Send + Sync`
/// 约束保证。
struct OpenRouterImagesProvider;

#[async_trait::async_trait]
impl ProviderImages for OpenRouterImagesProvider {
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&crate::models::ImagesOptions>,
    ) -> AssistantImages {
        generate_images_open_router(model, context, options).await
    }
}

#[cfg(test)]
mod tests {
    //! 锁定两件事：内置注册项的 api 名，以及自由函数到 OpenRouter 实现的失败形状——
    //! 缺 API key 时不 panic、不返回 `Err`，而是 `stop_reason = Error` + `error_message`。
    //! 两个用例都不会发出网络请求（缺 key 的检查早于建 URL 与发送）。

    use super::*;
    use crate::types::{
        ImagesStopReason, ModelCost, ModelInputModality, ModelType, TextContent, TextKind,
        UserContentBlock,
    };

    #[test]
    fn registers_openrouter_images_without_a_source_id() {
        register_built_in_images_api_providers();
        let provider =
            crate::image_models::get_images_api_provider("openrouter-images").expect("registered");
        assert_eq!(provider.api(), "openrouter-images");
    }

    #[tokio::test]
    async fn delegates_to_the_openrouter_implementation() {
        // 无网络：实现返回 `stopReason: "error"` 的结果（不是 panic），与 TS 的 catch 分支一致。
        let model = Model {
            model_type: Some(ModelType::Image),
            id: "black-forest-labs/flux.2-pro".into(),
            name: "FLUX.2 Pro".into(),
            api: "openrouter-images".into(),
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            input: vec![ModelInputModality::Text],
            output: Some(vec!["image".into()]),
            cost: ModelCost::default(),
            ..Model::default()
        };
        let context = ImagesContext {
            input: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "Generate a dog".into(),
                text_signature: None,
            })],
        };
        let output = generate_images_open_router(&model, &context, None).await;
        assert_eq!(output.api, "openrouter-images");
        assert_eq!(output.provider, "openrouter");
        assert_eq!(output.model, "black-forest-labs/flux.2-pro");
        // 缺少 api key：实现把它写进 errorMessage 并标记 error。
        assert_eq!(output.stop_reason, ImagesStopReason::Error);
        assert!(output.error_message.is_some());
    }
}
