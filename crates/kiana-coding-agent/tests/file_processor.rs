//! `test/block-images.test.ts` 与 `test/image-resize-callers.test.ts` 中
//! `describe("processFileArguments")` 的部分。
//!
//! 其余 describe（SettingsManager / Read tool）属于 kiana-config 与 kiana-tools 单元。
//! `image-resize-callers` 用 `vi.mock("../src/utils/image-resize.js")` 让 `resizeImage`
//! 恒返回 `null`；Rust 侧没有运行时注入点，改为喂入一个能通过 MIME 嗅探（PNG 签名 + IHDR）
//! 但无法解码/缩放的字节流，走同一条 `ProcessImageResult::Err` 分支。

use kiana_coding_agent::cli::file_processor::{
    ProcessFileOptions, ProcessedFiles, process_file_arguments_with_output,
};
use kiana_coding_agent::cli::setup::Output;
use tempfile::TempDir;

const TINY_PNG: &[u8] = include_bytes!("fixtures/files/tiny.png");
const TINY_BMP: &[u8] = include_bytes!("fixtures/files/tiny.bmp");

async fn process(paths: &[String], options: ProcessFileOptions) -> ProcessedFiles {
    let output = Output::capture();
    process_file_arguments_with_output(paths, options, &output)
        .await
        .expect("processed files")
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

// describe("processFileArguments") from block-images.test.ts
#[tokio::test]
async fn should_always_process_images_filtering_happens_at_convert_to_llm_layer() {
    let test_dir = TempDir::new().expect("temp dir");
    let image_path = test_dir.path().join("test.png");
    std::fs::write(&image_path, TINY_PNG).expect("write image");

    let result = process(&[path_string(&image_path)], ProcessFileOptions::default()).await;

    assert_eq!(result.images.len(), 1);
    assert_eq!(
        result.images[0].kind_image,
        kiana_ai::types::ImageKind::Image
    );
}

#[tokio::test]
async fn should_process_bmp_images_from_disk_as_png_attachments() {
    let test_dir = TempDir::new().expect("temp dir");
    let image_path = test_dir.path().join("test.bmp");
    std::fs::write(&image_path, TINY_BMP).expect("write image");

    let result = process(&[path_string(&image_path)], ProcessFileOptions::default()).await;

    assert_eq!(result.images.len(), 1);
    assert_eq!(result.images[0].mime_type, "image/png");
    assert!(
        result
            .text
            .contains("[Image converted from image/bmp to image/png.]")
    );
}

#[tokio::test]
async fn should_process_text_files_normally() {
    let test_dir = TempDir::new().expect("temp dir");
    let text_path = test_dir.path().join("test.txt");
    std::fs::write(&text_path, "Hello, world!").expect("write text");

    let result = process(&[path_string(&text_path)], ProcessFileOptions::default()).await;

    assert_eq!(result.images.len(), 0);
    assert!(result.text.contains("Hello, world!"));
    assert!(
        result
            .text
            .contains(&format!("<file name=\"{}\">", path_string(&text_path)))
    );
}

#[tokio::test]
async fn should_skip_empty_files() {
    let test_dir = TempDir::new().expect("temp dir");
    let empty_path = test_dir.path().join("empty.txt");
    std::fs::write(&empty_path, "").expect("write empty");
    let result = process(&[path_string(&empty_path)], ProcessFileOptions::default()).await;
    assert_eq!(result.text, "");
    assert!(result.images.is_empty());
}

// describe("image-resize-callers") — file processor 部分
#[tokio::test]
async fn file_processor_omits_image_attachments_when_auto_resize_cannot_produce_a_safe_image() {
    let test_dir = TempDir::new().expect("temp dir");
    let image_path = test_dir.path().join("test.png");
    // 通过 `isPng` 嗅探（签名 + IHDR 长度/类型）但无法被 photon 解码，因此 resize 返回 null；
    // 这与 TS 把 `resizeImage` mock 成 null 后走的分支和文案完全相同。
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0u8; 64]);
    std::fs::write(&image_path, &bytes).expect("write image");

    let result = process(&[path_string(&image_path)], ProcessFileOptions::default()).await;

    assert_eq!(result.images.len(), 0);
    assert!(result.text.contains("Image omitted"));
    assert_eq!(
        result.text,
        format!(
            "<file name=\"{}\">[Image omitted: could not be resized below the inline image size limit.]</file>\n",
            path_string(&image_path)
        )
    );
}

#[tokio::test]
async fn can_defer_resizing_file_attachments_until_prompt_dispatch() {
    let test_dir = TempDir::new().expect("temp dir");
    let image_path = test_dir.path().join("test.png");
    std::fs::write(&image_path, TINY_PNG).expect("write image");

    let result = process(
        &[path_string(&image_path)],
        ProcessFileOptions {
            auto_resize_images: Some(false),
        },
    )
    .await;

    assert_eq!(result.images.len(), 1);
    // TS 还断言 resizeImage 未被调用；Rust 侧没有可观察的 mock 注入点。
}

// ---------------------------------------------------------------------------
// 回归：mime 嗅探的 I/O 错误必须传播（TS 中位于 try 之外）
// ---------------------------------------------------------------------------

/// TS 的 `detectSupportedImageMimeTypeFromFile` 不在任何 try 内：打开目录时
/// `FileHandle.read` 抛出 `EISDIR: illegal operation on a directory, read`（不带路径），
/// 进程以未捕获异常中止。Rust 不再把它吞掉后落到 “Could not read file” 用户提示路径。
#[tokio::test]
async fn mime_sniff_errors_propagate_instead_of_becoming_could_not_read_file() {
    let test_dir = TempDir::new().expect("temp dir");
    let output = Output::capture();
    let error = process_file_arguments_with_output(
        &[path_string(test_dir.path())],
        ProcessFileOptions::default(),
        &output,
    )
    .await
    .expect_err("directory must abort");

    assert_eq!(error, kiana_coding_agent::cli::setup::CliExit::new(1));
    let stderr = output.captured_stderr();
    assert_eq!(
        stderr,
        "Error: EISDIR: illegal operation on a directory, read\n"
    );
    assert!(!stderr.contains("Could not read file"));
}

/// 同一条路径上 `open` 阶段的 EACCES 带路径（Node 26 实测）。
#[cfg(unix)]
#[tokio::test]
async fn mime_sniff_open_errors_use_the_node_message_with_path() {
    use std::os::unix::fs::PermissionsExt;

    if running_as_root() {
        return;
    }
    let test_dir = TempDir::new().expect("temp dir");
    let path = test_dir.path().join("secret.txt");
    std::fs::write(&path, "secret").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let output = Output::capture();
    let error = process_file_arguments_with_output(
        &[path_string(&path)],
        ProcessFileOptions::default(),
        &output,
    )
    .await
    .expect_err("permission denied must abort");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    assert_eq!(error, kiana_coding_agent::cli::setup::CliExit::new(1));
    assert_eq!(
        output.captured_stderr(),
        format!(
            "Error: EACCES: permission denied, open '{}'\n",
            path_string(&path)
        )
    );
}

#[cfg(unix)]
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|uid| uid.trim().parse::<u32>().ok())
        == Some(0)
}
