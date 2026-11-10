//! `cli/initial-message.ts`（43 行）：把 stdin、`@file` 文本与首个 CLI 消息合并成初始提示。
//!
//! # 为什么需要
//! 一次运行开始时，第一条用户消息可能有三个来源：管道 stdin、`@file` 参数展开出的文本、
//! 命令行位置参数里的第一条消息。TS 把它们用**空字符串**直接拼接（不是换行分隔），
//! 本模块是该行为的唯一实现，调用方是 `crate::app` 的 `prepare_initial_message`。
//!
//! # 约束与副作用
//! - 拼接顺序固定：`stdinContent` → `fileText` → 第一条 CLI 消息。
//! - `stdin_content` 是 `Some` 就参与拼接，**即使内容是空串**；因此单独一个空的 stdin
//!   会得到 `Some("")` 而不是 `None`。
//! - 第一条 CLI 消息会被从 `parsed.messages` 中**移除**（对应 TS 的 `shift()`）；
//!   其余消息原地保留。
//! - 空 `file_images` 归一化为 `None`，避免下游把“没有图片”当成空附件列表处理。
//!
//! TS: `packages/coding-agent/src/cli/initial-message.ts`

use kiana_ai::types::ImageContent;

use crate::cli::args::Args;

/// [`build_initial_message`] 的输入（TS `InitialMessageInput` 去掉 `parsed` 后的部分）。
#[derive(Debug, Clone, Default)]
pub struct InitialMessageInput {
    /// `@file` 参数产生的文本。
    pub file_text: Option<String>,
    /// `@file` 参数产生的图片附件。
    pub file_images: Option<Vec<ImageContent>>,
    /// 从管道读入的 stdin 内容。
    pub stdin_content: Option<String>,
}

/// `InitialMessageResult`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InitialMessageResult {
    /// 合并后的初始提示。
    pub initial_message: Option<String>,
    /// 图片附件。
    pub initial_images: Option<Vec<ImageContent>>,
}

/// `buildInitialMessage({parsed, fileText, fileImages, stdinContent})`。
///
/// parts 顺序：`stdinContent`（定义即加入，含空串）→ `fileText`（非空）→
/// `parsed.messages[0]`（**shift 掉**，原地修改 `parsed.messages`）。用空字符串 join。
///
/// # 副作用
/// `parsed.messages` 会被原地修改：首条消息被移除。调用方之后看到的
/// `parsed.messages` 不含这条消息。
///
/// # 返回值
/// - `initial_message`：`parts` 非空时为 `Some(parts.join(""))`；三个来源都缺席时为
///   `None`。注意 `Some("")` 是合法结果（见模块文档的空 stdin 情形）。
/// - `initial_images`：`file_images` 为 `None` 或空 `Vec` 时为 `None`，否则原样透传。
pub fn build_initial_message(
    parsed: &mut Args,
    input: InitialMessageInput,
) -> InitialMessageResult {
    let mut parts: Vec<String> = Vec::new();
    if let Some(stdin_content) = input.stdin_content {
        parts.push(stdin_content);
    }
    if let Some(file_text) = input.file_text
        && !file_text.is_empty()
    {
        parts.push(file_text);
    }
    // TS 的 `shift()`：直接取走并移除首条消息（O(n) 移动剩余元素），其余消息留在原地。
    if !parsed.messages.is_empty() {
        parts.push(parsed.messages.remove(0));
    }

    InitialMessageResult {
        initial_message: (!parts.is_empty()).then(|| parts.join("")),
        initial_images: input.file_images.filter(|images| !images.is_empty()),
    }
}
