//! `images/cloudflare.ts`（`createCloudflarePhotonImages`）——**不移植**（ARCHITECTURE.md §8 第 3 条）。
//!
//! TS 版本把 `photon_rs_bg.wasm` 作为 Workers module 导入（Wrangler 的 wasm 模块绑定）。
//! Rust 侧图像处理走原生 `photon-rs`（feature `images`，见 [`super::node`]），没有 Workers
//! 运行时，因此这里保留同名入口并返回“不支持”。
//!
//! 错误文案逐字：`Cloudflare image processing is not supported in the Rust build`。
//!
//! ## 使用方式
//!
//! 该入口在**所有**构建配置下都存在（本文件没有 `cfg`，`images` 与 `cloudflare` feature
//! 都不影响它），且没有成功路径：返回值恒为 `Err`。调用方应把它当作“当前构建不支持此
//! 能力”的静态答复，而不是可重试的运行时错误；需要真正的图片处理时使用
//! `images::node::create_photon_images`（需开启 `images` feature）。
//!
//! ## 错误类型
//!
//! [`CloudflareImagesUnsupported`] 是零大小类型，只用于报告能力缺失；`Display` 输出就是
//! 上面那句固定文案，不携带原始错误或上下文。

/// `createCloudflarePhotonImages()`：Workers 后端的占位入口，恒定失败。
///
/// 返回 `Err(CloudflareImagesUnsupported)`；没有参数、不读取 wasm 模块，也没有副作用。
pub fn create_cloudflare_photon_images() -> Result<(), CloudflareImagesUnsupported> {
    Err(CloudflareImagesUnsupported)
}

/// `createCloudflarePhotonImages()` 的失败类型。
///
/// 零大小类型，`Display` 文案为 `Cloudflare image processing is not supported in the Rust build`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Cloudflare image processing is not supported in the Rust build")]
pub struct CloudflareImagesUnsupported;
