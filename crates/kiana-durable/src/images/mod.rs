//! images 目录（feature `images`）。
//!
//! TS 源：`packages/durable/src/images/{index,exif,node,node-worker,cloudflare}.ts` +
//! `photon-wasm.d.ts`（纯类型垫片，不生成 `.rs`）。
//!
//! 后端映射（决策：ARCHITECTURE.md §8 第 20 条）：
//!
//! | TS | Rust |
//! |---|---|
//! | `createPhotonImages(wasm)` | [`node::create_photon_images`]（原生 `photon-rs`） |
//! | `createNodePhotonImages()` | [`node::create_node_photon_images`]（无 worker 线程） |
//! | `createCloudflarePhotonImages()` | [`cloudflare::create_cloudflare_photon_images`]（不支持，§8 第 3 条） |
//! | `exifOrientation(bytes)` | [`exif::exif_orientation`]（保留手写解析，与 TS 逐行一致） |
//!
//! **不做** Worker 线程隔离：TS 用它是因为 Photon 的 wasm 内存永不回缩；原生 `photon-rs`
//! 没有这个问题（`spec/durable-rest.md` §2.26.3 的 Rust 注记）。
//!
//! ## 编译边界
//!
//! 三个子模块都无条件编译；`images` feature 只控制 [`node`] 内部的解码/编码实现
//! （`photon-rs`、`image` 0.24 等可选依赖）。因此默认构建下 [`exif::exif_orientation`] 与
//! [`cloudflare::create_cloudflare_photon_images`] 照常可用，而
//! [`node::NodePhotonImages::prepare`] 一律返回 `None`。
//!
//! ## 阅读顺序
//!
//! 1. [`exif`]：无外部依赖的字节解析，也是 `prepare` 的第一步；
//! 2. [`cloudflare`]：只有“不支持”占位，用来对齐 TS 的入口形状；
//! 3. [`node`]：核心后端，包含解码、扶正、缩放与重编码。
//!
//! ## 与 `tools` 的关系
//!
//! `crate::tools::image` 直接重导出 [`node::ImageLimits`]、[`node::PreparedImage`] 与
//! [`node::ResizedDimensions`]（见 `tools/image.rs`），所以改动这些类型的字段会同时影响
//! 工具层 `ImageProcessor` 的契约。

// --- 子模块 ---
pub mod cloudflare;
pub mod exif;
pub mod node;
