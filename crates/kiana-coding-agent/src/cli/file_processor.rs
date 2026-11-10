//! `cli/file-processor.ts`（88 行）：把 `@file` 参数转换成文本与图片附件。
//!
//! TS 里 `process.exit(1)` 的两条错误路径在 Rust 侧先经 [`Output`] 打印逐字消息，再返回
//! [`CliExit`]（由 app 结束进程）。
//!
//! # 这个文件负责什么
//! `@file` 是 CLI 位置参数里的文件引用。本模块把每个路径解析成绝对路径、嗅探是否为受支持
//! 的图片，产出一段可直接拼进初始提示的文本（`<file name="…">…</file>`），以及需要作为
//!   图片附件单独发送的 [`ImageContent`]。它不读 stdin、不拼接用户消息——后两者由
//!   [`crate::cli::initial_message`] 完成；调用方是 `crate::app` 的
//!   `prepare_initial_message`。
//!
//! # 逐文件流程与边界
//! 1. 参数经 `resolve_read_path` 转成绝对路径（路径规则属于 tools 的 path-utils 单元）；
//! 2. 路径不存在（Rust 的 `Path::exists` 对任何 `stat` 失败都返回 false）→ 打印
//!    `Error: File not found: …` 并以退出码 1 中止**整批**文件，而不是跳过该项；
//! 3. 空文件（`metadata.len() == 0`）跳过，不产生任何文本或附件；
//! 4. MIME 嗅探判定为图片 → 走 [`process_image`]；解码/缩放失败只把提示文案放进文本，
//!    不产生附件，也不中止进程；
//! 5. 其余按 UTF-8 有损解码、去掉 BOM，再包进 `<file name="…">`。
//!
//! # 重要约束
//! - 本模块不按模型是否支持图片做过滤；图片一律收集，过滤发生在下游的
//!   convert-to-LLM 层（`tests/file_processor.rs` 的测试名说明了这一分工）。
//! - 文本包装格式是发给模型的字节的一部分；`tests/file_processor.rs` 逐字锁定了标签、
//!   换行与失败文案，改动会改变模型输入。
//! - MIME 嗅探阶段的 IO 错误在 TS 中位于 try 之外，会以未捕获异常结束进程；Rust 侧因此
//!   打印 Node 风格消息（`Error: <code>: <description>, …`）并返回退出码 1，而**不**落到
//!   “Could not read file …”这条带文件内容的提示路径。
//! - 错误路径必须先写用户消息再返回 [`CliExit`]；真正退出由二进制入口执行。
//!
//! TS: `packages/coding-agent/src/cli/file-processor.ts`

use std::path::Path;

use kiana_ai::types::{ImageContent, ImageKind};
use kiana_core::image_process::{ProcessImageOptions, ProcessImageResult, process_image};
use kiana_core::mime::detect_supported_image_mime_type_from_file;
use kiana_core::text::strip_bom;
use kiana_tools::path_utils::resolve_read_path;

use crate::cli::setup::{CliExit, CliResult, Output, Style, node_error_parts};

/// `ProcessedFiles`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessedFiles {
    /// 文本部分（含 `<file …>` 包装）。
    ///
    /// 按输入参数顺序拼接；图片处理失败时对应的 `<file>` 标签里是整段面向用户的失败文案，
    /// 成功时是转换/尺寸 hints（可能为空，此时标签内容为空）。
    pub text: String,
    /// 图片附件。
    ///
    /// 只包含**处理成功**的图片，顺序与输入参数顺序一致；失败或空文件不会出现在这里。
    pub images: Vec<ImageContent>,
}

/// `ProcessFileOptions`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessFileOptions {
    /// 是否自动缩放图片。`None` 采用 TS 默认值 `true`。
    pub auto_resize_images: Option<bool>,
}

/// `processFileArguments(fileArgs, options?)`。
///
/// 对每个参数：
/// 1. `resolve(resolveReadPath(fileArg, process.cwd()))`；
/// 2. 不存在 → `Error: File not found: <path>` + exit 1；
/// 3. 空文件跳过；
/// 4. 图片（`detectSupportedImageMimeTypeFromFile`）→ `processImage`；
///    失败写 `<file name="…">message</file>\n`，成功 push 附件并按 hints 写包装；
/// 5. 文本 → `stripBom` 后 `<file name="…">\ncontent\n</file>\n`。
///
/// 输出写到进程通道（[`Output::process`]）；测试可用
/// [`process_file_arguments_with_output`] 捕获逐字消息。
///
/// # 错误
/// 返回 `Err(CliExit::new(1))` 的三种情况：路径不存在、MIME 嗅探 IO 错误、文件内容读取
/// 失败。任何一项都会中止整批处理，已经写入的 stderr 消息保留。
pub async fn process_file_arguments(
    file_args: &[String],
    options: ProcessFileOptions,
) -> CliResult<ProcessedFiles> {
    process_file_arguments_with_output(file_args, options, &Output::process()).await
}

/// 指定 [`Output`] 的 [`process_file_arguments`]（测试/嵌入调用方使用）。
///
/// 所有用户可见消息都写进 `output`：缺失文件/读取失败为红色 `Error: …`，MIME 嗅探失败为
/// 无样式 `Error: …`。颜色由 [`Style::detect`] 决定（进程首次检测后固定），与 `output`
/// 是否为捕获模式无关。
pub async fn process_file_arguments_with_output(
    file_args: &[String],
    options: ProcessFileOptions,
    output: &Output,
) -> CliResult<ProcessedFiles> {
    let auto_resize_images = options.auto_resize_images.unwrap_or(true);
    let style = Style::detect();
    let mut text = String::new();
    let mut images: Vec<ImageContent> = Vec::new();

    for file_arg in file_args {
        let cwd = std::env::current_dir()
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let absolute_path = resolve_read_path(file_arg, &cwd);
        let absolute_path_text = absolute_path.to_string_lossy().into_owned();

        // `fs.access(p, F_OK)`：任何错误都算不存在。
        if !absolute_path.exists() {
            output.write_line_stderr(
                &style.red(&format!("Error: File not found: {absolute_path_text}")),
            );
            return Err(CliExit::new(1));
        }

        let metadata = match std::fs::metadata(&absolute_path) {
            Ok(metadata) => metadata,
            // 与“不存在”不同：stat 失败（例如路径组件是普通文件）视为不可处理并静默跳过。
            Err(_) => continue,
        };
        if metadata.len() == 0 {
            continue;
        }

        let mime_type = match detect_supported_image_mime_type_from_file(&absolute_path) {
            Ok(mime_type) => mime_type,
            Err(error) => {
                // TS 的 `detectSupportedImageMimeTypeFromFile` 位于任何 try 之外：open/read
                // 失败会成为未捕获异常（Node 打印错误 stack 并以 1 退出），**不**会落到下面
                // “Could not read file”的用户提示路径。Rust 无法复刻 JS stack trace，这里打印
                // Node 的错误消息行并以 1 退出。
                output.write_line_stderr(&format!(
                    "Error: {}",
                    node_sniff_error_message(&error, &absolute_path_text)
                ));
                return Err(CliExit::new(1));
            }
        };

        if let Some(mime_type) = mime_type {
            // 嗅探命中：重新读取原始字节交给 process_image。两次 IO 之间文件状态可能变化，
            // 因此这里仍可能失败，失败走带路径的 `Could not read file` 提示。
            let content = match std::fs::read(&absolute_path) {
                Ok(content) => content,
                Err(error) => {
                    let message = node_read_error_message(&error, &absolute_path_text);
                    output.write_line_stderr(&style.red(&format!(
                        "Error: Could not read file {absolute_path_text}: {message}"
                    )));
                    return Err(CliExit::new(1));
                }
            };
            let processed = process_image(
                &content,
                &mime_type,
                Some(ProcessImageOptions {
                    auto_resize_images: Some(auto_resize_images),
                    resize_options: None,
                }),
            );
            match processed {
                ProcessImageResult::Err { message } => {
                    // 失败只影响文本：整段面向用户的文案原样放进标签，不产生附件。
                    text.push_str(&format!(
                        "<file name=\"{absolute_path_text}\">{message}</file>\n"
                    ));
                }
                ProcessImageResult::Ok {
                    data,
                    mime_type,
                    hints,
                } => {
                    images.push(ImageContent {
                        kind_image: ImageKind::Image,
                        data,
                        mime_type,
                    });
                    // 文本里只保留 hints（格式转换/尺寸提示）；没有 hints 时是空标签。
                    if hints.is_empty() {
                        text.push_str(&format!("<file name=\"{absolute_path_text}\"></file>\n"));
                    } else {
                        text.push_str(&format!(
                            "<file name=\"{absolute_path_text}\">{}</file>\n",
                            hints.join("\n")
                        ));
                    }
                }
            }
        } else {
            // 非图片：Node 的 `readFile(path, "utf-8")` 用 U+FFFD 替换非法字节而非报错，
            // `read_utf8_lossy` 复刻该行为；随后去掉开头 BOM。
            match read_utf8_lossy(&absolute_path) {
                Ok(content) => {
                    let content = strip_bom(&content);
                    text.push_str(&format!(
                        "<file name=\"{absolute_path_text}\">\n{content}\n</file>\n"
                    ));
                }
                Err(error) => {
                    let message = node_read_error_message(&error, &absolute_path_text);
                    output.write_line_stderr(&style.red(&format!(
                        "Error: Could not read file {absolute_path_text}: {message}"
                    )));
                    return Err(CliExit::new(1));
                }
            }
        }
    }

    Ok(ProcessedFiles { text, images })
}

/// `readFile(path, "utf-8")`：Node 用 U+FFFD 替换非法字节，而不是报错。
///
/// 只有底层 IO 失败才会返回 `Err`；调用方用 `node_read_error_message` 转成 Node 文案。
fn read_utf8_lossy(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `detectSupportedImageMimeTypeFromFile`（`open` + `FileHandle.read`）的 Node 消息。
///
/// `open` 阶段的错误带路径（`EACCES: permission denied, open '<path>'`）；
/// `read` 阶段（目录）的 libuv 消息不带路径（`EISDIR: illegal operation on a directory, read`）。
fn node_sniff_error_message(error: &std::io::Error, path: &str) -> String {
    let (code, description) = node_error_parts(error);
    if error.kind() == std::io::ErrorKind::IsADirectory {
        format!("{code}: {description}, read")
    } else {
        format!("{code}: {description}, open '{path}'")
    }
}

/// `readFile(path, "utf-8")` 的 Node 消息：read 阶段（目录）也带路径。
fn node_read_error_message(error: &std::io::Error, path: &str) -> String {
    let (code, description) = node_error_parts(error);
    let syscall = if error.kind() == std::io::ErrorKind::IsADirectory {
        "read"
    } else {
        "open"
    };
    format!("{code}: {description}, {syscall} '{path}'")
}
