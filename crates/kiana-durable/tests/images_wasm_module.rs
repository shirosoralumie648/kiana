//! `packages/durable/test/images-wasm-module.test.ts` 的移植。
//!
//! TS 的 `createPhotonImages(wasm)` 接收已经编译好的 WebAssembly 模块（Cloudflare Workers
//! 传进来）。Rust 侧原生链接同一个 Photon 源码（`photon-rs`），没有 wasm 参数；能跑通的
//! 不变式是同一个：一张能解码、无需转换的 1x1 GIF 原样返回。

use kiana_durable::images::node::create_photon_images;
use kiana_durable::tools::image::DEFAULT_IMAGE_LIMITS;
#[cfg(feature = "images")]
use kiana_durable::tools::image::{PreparedImage, to_base64};

/// 1x1 GIF。
const GIF: &[u8] =
    b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

#[cfg(feature = "images")]
#[test]
fn create_photon_images_prepares_a_gif_unchanged() {
    let prepared = create_photon_images().prepare(GIF, "image/gif", DEFAULT_IMAGE_LIMITS);
    assert_eq!(
        prepared,
        Some(PreparedImage {
            data: to_base64(GIF),
            mime_type: "image/gif".to_owned(),
            resized: None,
            converted_from: None,
        })
    );
}

#[cfg(not(feature = "images"))]
#[test]
fn without_the_images_feature_prepare_returns_none() {
    // 没有 Photon 时所有 `prepare` 都像“无法解码”一样返回 `None`（TS 的 `undefined`）。
    assert!(
        create_photon_images()
            .prepare(GIF, "image/gif", DEFAULT_IMAGE_LIMITS)
            .is_none()
    );
}
