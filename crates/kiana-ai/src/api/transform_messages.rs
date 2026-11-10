//! api/transform-messages.ts。
//!
//! 每个 provider 请求前的消息归一化：
//! 1. 不支持图片的模型把图片降级为占位文本；
//! 2. 跨模型时丢弃 thinking（签名只对产出它的模型有效）、`textSignature`、`thoughtSignature`，
//!    并按需归一化 tool call id；
//! 3. 给没有结果的 tool call 合成 `"No result provided"` 结果；
//! 4. system 消息对 tool call 记账透明（夹在调用与结果之间的 system 消息延后到结果之后）。
//!
//! TS: `packages/ai/src/api/transform-messages.ts`。
//!
//! ## 宽松输入（`content == null`）
//!
//! TS 的第一步 `messages.map(msg => msg.content == null ? { ...msg, content: [] } : msg)`
//! 归一 `null` / 缺失的 `content`（无类型调用方：自定义工具、手工历史、旧会话文件，
//! issues #6259/#6276）。Rust 的 [`Message`]（`types.rs`，§4.9 冻结）在类型层要求 `content`
//! 存在，因此这一步必须在 **serde 解析之前**完成：无类型调用方把原始 JSON 交给
//! [`transform_lax_messages`]（内部先 [`normalize_message_content`] 再解析），或先单独调用
//! [`normalize_message_content`] 再解析成 [`Message`] 并调用 [`transform_messages`]。
//!
//! # 这个文件为什么存在
//!
//! 不同 provider 对历史消息的要求不同：有的不接受图片（图片块会让请求失败），有的不接受
//! 别的模型产生的 thinking 签名（签名只对产出它的模型有效），有的对 tool call id 的字符集
//! 或长度有限制。把归一化集中在本文件、由各适配器在发请求前按需调用，可以避免每个适配器
//! 各写一份、行为逐渐漂移。本文件不读写磁盘、不发网络请求；唯一的非确定性是合成
//! toolResult 的 `timestamp` 取 `Date.now()`。
//!
//! # 被谁调用
//!
//! - `api::anthropic_messages`、`api::bedrock_converse_stream`：除 [`transform_messages_with`]
//!   外还使用 [`split_error_tool_result_images`]（Claude 拒绝错误结果里带图片）；
//! - `api::openai_completions`、`api::openai_responses_shared`、`api::google_shared`、
//!   `api::mistral_conversations`：传入各自的 `normalize_tool_call_id` 回调；
//! - 无类型调用方（旧会话文件、手工历史、自定义工具）走 [`transform_lax_messages`]。
//!
//! # 执行流程（[`transform_messages_with`]）
//!
//! 1. `downgrade_unsupported_images`：模型 `input` 不含 `image` 时，把 user/toolResult 里的
//!    图片替换成占位文本；
//! 2. 第一遍逐条复制：跨模型时处理 thinking、`textSignature`、`thoughtSignature`，并按需
//!    归一化 tool call id；旧 id → 新 id 的映射记在 `toolCallIdMap`，供后面的 toolResult
//!    复用，保证调用与结果仍然配对；
//! 3. 第二遍重排并补全：为没有结果的 tool call 合成 `"No result provided"` 的 `isError`
//!    结果；夹在 tool call 与结果之间的 system 消息延后到结果之后发出。
//!
//! # 重要约束
//!
//! - 输入顺序有意义：toolResult 只能匹配**它之前**出现过的 assistant tool call（映射是
//!   单遍建立的）；序列错乱或缺失的历史不会被重新排序。
//! - `isSameModel` 的判定是 `provider`、`api`、`model` 三个字段都相同；provider 返回的
//!   `responseModel` 不参与判定。
//! - 已经以 `stop_reason = error/aborted` 结束的 assistant 消息不会被重放，也不参与
//!   孤儿 tool call 记账；它前面的待处理调用仍会在它出现时被补上合成结果。
//! - 函数返回全新的 `Vec<Message>`，不修改入参；所有变换都是 clone 后改写。

use std::collections::HashMap;

use serde_json::Value;

use crate::types::{
    AssistantContentBlock, AssistantMessage, ImageContent, Message, Model, ModelInputModality,
    TextContent, TextKind, ToolCall, ToolResultMessage, ToolResultRole, UserContent,
    UserContentBlock, UserMessage,
};

/// `NON_VISION_USER_IMAGE_PLACEHOLDER`。
///
/// 模型不支持图片输入时，user 消息里的每段连续图片被替换成这段文本（见
/// [`downgrade_unsupported_images`]）。文本必须逐字保持：它会被发给 provider 并进入历史。
pub const NON_VISION_USER_IMAGE_PLACEHOLDER: &str =
    "(image omitted: model does not support images)";
/// `NON_VISION_TOOL_IMAGE_PLACEHOLDER`。
///
/// 与 [`NON_VISION_USER_IMAGE_PLACEHOLDER`] 并列，但用于 toolResult 内容；两条占位文本
/// 不同，发送时按消息类型选择。
pub const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";
/// `ERROR_TOOL_IMAGE_PLACEHOLDER`。
///
/// 错误 toolResult 拆分时留在原结果里的占位文本；真正的图片被移到本轮 tool result 之后
/// （见 [`split_error_tool_result_images`]）。
pub const ERROR_TOOL_IMAGE_PLACEHOLDER: &str = "(image attached after the tool results)";

/// An error tool result split into a text-only result and the images to send after the tool results.
///
/// 字段之间的顺序契约：
/// - `content` 一定只含文本块：图片被替换为 [`ERROR_TOOL_IMAGE_PLACEHOLDER`]；
/// - `caption` 点名来源（tool call id 与工具名），调用方必须先发 `content`、再发
///   `caption`、最后发 `images`；
/// - `images` 保持原消息里的顺序；没有图片时不会构造本结构。
///
/// TS: `src/api/transform-messages.ts#SplitErrorToolResult`
#[derive(Debug, Clone, PartialEq)]
pub struct SplitErrorToolResult {
    /// The result content with each run of images replaced by a note.
    pub content: Vec<TextContent>,
    /// Text naming the tool call, to send before [`SplitErrorToolResult::images`].
    pub caption: String,
    /// The images, in their original order.
    pub images: Vec<ImageContent>,
}

/// `splitErrorToolResultImages(msg)`。
///
/// 供拒绝「错误结果里带图片」的 API（Anthropic、Bedrock 上的 Claude）使用：tool result 只发
/// 文本，caption 与图片放在本轮所有 tool result 之后、同一条 user 消息里。
///
/// # 返回
///
/// 两种情形返回 `None`，调用方应把消息原样发送：
/// - `msg.is_error == false`（只有**错误**结果才需要拆分）；
/// - 内容里没有 `image` 块。
///
/// 否则返回 [`SplitErrorToolResult`]：`content` 是图片被占位文本替换后的纯文本块，
/// `caption` 形如 ``Images from the error result of tool call <id> (<name>):``，
/// `images` 是按原顺序收集的图片副本。
///
/// 图片放在 tool result 之后而不是结果内部，是为了不让 Claude 的
/// tool_result 内容校验失败，同时保留截图等诊断信息。
///
/// TS: `src/api/transform-messages.ts#splitErrorToolResultImages`
pub fn split_error_tool_result_images(msg: &ToolResultMessage) -> Option<SplitErrorToolResult> {
    if !msg.is_error {
        return None;
    }
    let images: Vec<ImageContent> = msg
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Image(image) => Some(image.clone()),
            UserContentBlock::Text(_) => None,
        })
        .collect();
    if images.is_empty() {
        return None;
    }
    Some(SplitErrorToolResult {
        content: replace_images_with_placeholder(&msg.content, ERROR_TOOL_IMAGE_PLACEHOLDER),
        caption: format!(
            "Images from the error result of tool call {} ({}):",
            msg.tool_call_id, msg.tool_name
        ),
        images,
    })
}

/// `replaceImagesWithPlaceholder(content, placeholder)`。
///
/// 连续多张图片只插入**一个**占位文本块；若原文本块内容恰好等于占位文本，也视为
/// 「上一个已经是占位」，从而抑制重复占位。
///
/// 返回值是 `TextContent` 列表而不是原来的块联合：调用方（不支持图片的降级、错误结果
/// 拆分）都只能发送纯文本。文本块是 clone 的，因此原有 `textSignature` 等字段保留；
/// 被替换的图片（MIME 与 base64）完全丢弃。
///
/// 算法只有一个状态 `previous_was_placeholder`：图片块后置真，文本块后重新按
/// 「文本是否等于占位符」计算。因此「图片、恰好等于占位符的文本、图片」中第二张图片
/// 不会再追加占位符（那段原文本身仍会保留），与 TS 的实现逐分支一致。
///
/// TS 里这是模块私有函数；Rust 侧公开给其它 api 适配器复用（`downgradeUnsupportedImages`
/// 与 `splitErrorToolResultImages` 的共用实现）。
/// 当前 crate 内只有本模块的两个调用方（共三处调用）。
pub fn replace_images_with_placeholder(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<TextContent> {
    let mut result: Vec<TextContent> = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(TextContent {
                        kind_text: TextKind::Text,
                        text: placeholder.to_string(),
                        text_signature: None,
                    });
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(text) => {
                result.push(text.clone());
                previous_was_placeholder = text.text == placeholder;
            }
        }
    }
    result
}

fn blocks_from_text(content: Vec<TextContent>) -> Vec<UserContentBlock> {
    content.into_iter().map(UserContentBlock::Text).collect()
}

/// TS `transformMessages` 的预处理：把 `content: null` / 缺失的 `content` 归一成 `[]`。
///
/// 无类型调用方（自定义工具、手工拼接的历史、旧会话文件）会在 JSON 里写 `content: null` 或
/// 省略 `content`；[`Message`] 在类型层要求它存在（`types.rs` 是 §4.9 冻结契约），所以这一步
/// 必须在 `serde_json::from_value::<Message>` **之前**执行。返回的数组可直接逐条解析成
/// [`Message`] 后交给 [`transform_messages`]。
///
/// 与 JS 的 `{ ...msg, content: [] }` 一致：已有 `content` 键**原位**替换（键序不变），缺失时
/// 追加到对象末尾；`content` 非 `null` 的消息原样返回。非对象条目不在 `Message` 契约内，
/// 原样返回。
///
/// TS: `src/api/transform-messages.ts`（`messages.map(msg => msg.content == null ? {...msg, content: []} : msg)`）
pub fn normalize_message_content(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let Value::Object(object) = message else {
                return message.clone();
            };
            if object
                .get("content")
                .is_some_and(|content| !content.is_null())
            {
                return message.clone();
            }
            let mut object = object.clone();
            // `serde_json::Map`（`preserve_order`）的 `insert` 对已有键保持位置，与 JS 赋值一致。
            object.insert("content".to_string(), Value::Array(Vec::new()));
            Value::Object(object)
        })
        .collect()
}

/// `downgradeUnsupportedImages(messages, model)`。
///
/// `model.input` 含 `"image"` 时原样返回；否则 `user` / `toolResult` 的图片替换为占位文本。
///
/// 具体改写（Rust 侧的「原样返回」是 `messages.to_vec()`，即逐条 clone）：
/// - `user` 且 `content` 是块数组：图片替换为 [`NON_VISION_USER_IMAGE_PLACEHOLDER`]，
///   结果仍是块数组（但只剩文本块）；`UserContent::Text` 形态没有图片，保持不变。
/// - `toolResult`：内容整体替换为纯文本块，图片替换为
///   [`NON_VISION_TOOL_IMAGE_PLACEHOLDER`]；其余字段（id/name/details/usage 等）通过
///   `..result.clone()` 保留。
/// - `system` / `assistant`：原样 clone（本层不改它们的内容）。
///
/// 不修改入参；没有图片的块数组也会被重建，但内容与顺序不变。是否支持图片只看
/// `model.input`，不看消息里实际有没有图片。
///
/// TS: `src/api/transform-messages.ts#downgradeUnsupportedImages`
pub fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model.input.contains(&ModelInputModality::Image) {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.content {
                UserContent::Blocks(blocks) => Message::User(UserMessage {
                    role: user.role,
                    content: UserContent::Blocks(blocks_from_text(
                        replace_images_with_placeholder(blocks, NON_VISION_USER_IMAGE_PLACEHOLDER),
                    )),
                    timestamp: user.timestamp,
                }),
                UserContent::Text(_) => message.clone(),
            },
            Message::ToolResult(result) => Message::ToolResult(ToolResultMessage {
                content: blocks_from_text(replace_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                )),
                ..result.clone()
            }),
            Message::System(_) | Message::Assistant(_) => message.clone(),
        })
        .collect()
}

/// `normalizeToolCallId?(id, model, source)` 的回调类型。
///
/// 参数：原始 id、本次请求的目标模型、产生该 id 的 assistant 消息。返回新的 id；
/// 返回与输入相同的字符串表示「不需要改」。
///
/// 回调只在**跨模型**的 tool call 上调用；若返回的 id 不同，则写入旧→新映射，并同步应用到
/// 之后出现、且归一化结果非空的 toolResult；空串映射会被忽略（与 TS 的
/// `if (normalizedId && ...)` 一致），此时调用与原结果 id 可能不再配对。适配器应保证
/// 同一原始 id 得到相同结果：`toolCallIdMap` 只按 id 保存一个值，同一 id 再次出现时会
/// 覆盖前一次的结果。
///
/// `Send + Sync` 让带内部可变状态的闭包（如用互斥锁保护映射表的
/// `MistralToolCallIdNormalizer`）也能传进来；[`transform_messages_with`] 本身是同步、
/// 串行地调用它。
pub type NormalizeToolCallIdFn = dyn Fn(&str, &Model, &AssistantMessage) -> String + Send + Sync;

/// 无类型调用方的 `transformMessages` 入口：先做 [`normalize_message_content`]，再解析成
/// [`Message`] 并交给 [`transform_messages`]。
///
/// TS 的 `transformMessages` 接受任意 JS 对象（`content == null` 会被归一为 `[]`）；Rust 的
/// 类型化入口做不到这一点，因此宽松 JSON（旧会话文件、手工历史、自定义工具）走这里。
///
/// # 参数与返回
///
/// `messages` 是原始 JSON 数组，逐条先补 `content`，再按 [`Message`] 的 serde 形状解析。
/// 解析阶段与原数组逐项对应；随后的 transform 可能延后 system 消息、追加合成结果或丢弃
/// `stop_reason` 为 `error`/`aborted` 的 assistant，因此返回值不保证与输入等长同序。
///
/// # 错误
///
/// 归一化之后仍不符合 [`Message`] 契约（例如缺 `role`、字段类型错误）时返回
/// `serde_json::Error`；错误来自第一个解析失败的条目，不包含「第几条」的额外信息。
/// 非对象条目会被 [`normalize_message_content`] 原样保留，随后同样在解析阶段失败。
///
/// TS: `src/api/transform-messages.ts#transformMessages`（宽松输入路径）
pub fn transform_lax_messages(
    messages: &[Value],
    model: &Model,
) -> Result<Vec<Message>, serde_json::Error> {
    let parsed = normalize_message_content(messages)
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<Message>, serde_json::Error>>()?;
    Ok(transform_messages(&parsed, model))
}

/// `transformMessages(messages, model)`（无 id 归一化回调的常见形式）。
///
/// 等价于 [`transform_messages_with`] 传 `None`：不改变任何 tool call id。需要让 id 符合
/// 目标 provider 字符集/长度限制的适配器应改用 [`transform_messages_with`]。
///
/// TS: `src/api/transform-messages.ts#transformMessages`
pub fn transform_messages(messages: &[Message], model: &Model) -> Vec<Message> {
    transform_messages_with(messages, model, None)
}

/// `transformMessages(messages, model, normalizeToolCallId?)` 的完整签名。
///
/// # 参数
///
/// - `messages`：会话历史，按原顺序；返回值是新 `Vec`，入参不被修改。
/// - `model`：本次请求的目标模型；`isSameModel` 与图片能力都以它为准。
/// - `normalize_tool_call_id`：仅对跨模型的 tool call 调用；`None` 表示不改 id。
///
/// # 返回与结构变化
///
/// - 跨模型的 thinking 块整块丢弃，文本块的 `textSignature` 被清空；
/// - 跨模型的 tool call 先清 `thoughtSignature`（仅非空值），再按需替换 id；
/// - 之后出现的 toolResult 在归一化结果非空时同步替换 `tool_call_id`；空串映射会被忽略
///   （与 TS 的 `if (normalizedId && ...)` 一致），此时调用与原结果 id 可能不再配对；
/// - 追加合成的 `isError` toolResult（文本固定 `"No result provided"`，时间戳为当前时间）；
/// - system 消息相对 toolResult 的位置可能后移。
///
/// # 不变量
///
/// - 每个进入待处理集合且没有结果的 tool call 都会补一条合成结果；每条待处理 tool call
///   至多一条（`error`/`aborted` 的 assistant 见下一条）。
/// - `stop_reason` 为 `error`/`aborted` 的 assistant 消息整条丢弃，它自己的 tool call
///   不进入待处理集合，也不会产生合成结果。
/// - 合成的 toolResult 角色固定为 `toolResult`，`isError = true`，其余可选字段为 `None`。
///
/// TS: `src/api/transform-messages.ts#transformMessages`
pub fn transform_messages_with(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&NormalizeToolCallIdFn>,
) -> Vec<Message> {
    // Build a map of original tool call IDs to normalized IDs.
    // 映射只对跨模型的 tool call 建立；toolResult 在后面按 id 查表同步改名。因为只在
    // 单遍扫描中填充，因此排在前面的 toolResult 匹配不到后面的 assistant（前向引用不处理）。
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    // 阶段 0：先做图片降级；后续所有处理都在这个副本上进行，入参保持不变。
    let image_aware_messages = downgrade_unsupported_images(messages, model);

    // First pass: transform messages (unsupported image downgrade, thinking blocks, tool call ID
    // normalization).
    // 第一遍逐条复制，只改 assistant 内容与 toolResult 的 id，不增删消息、不重排。
    let transformed: Vec<Message> = image_aware_messages
        .iter()
        .map(|message| match message {
            Message::System(_) | Message::User(_) => message.clone(),
            // 仅当归一化结果非空、与当前 id 不同，且该 toolResult 出现在调用之后时才替换；
            // 空串在 TS 里是 falsy，因此不应用映射（对应 `if (normalizedId && ...)`）。
            // 忽略空串映射的代价是：调用 id 已被改成 ""，原结果保留旧 id，两者不再配对；
            // 第二遍会再为 "" 追加一条合成结果（fixture 的 `normalize: "empty"` 用例）。
            Message::ToolResult(result) => match tool_call_id_map.get(&result.tool_call_id) {
                Some(normalized_id)
                    if !normalized_id.is_empty() && normalized_id != &result.tool_call_id =>
                {
                    Message::ToolResult(ToolResultMessage {
                        tool_call_id: normalized_id.clone(),
                        ..result.clone()
                    })
                }
                _ => message.clone(),
            },
            Message::Assistant(assistant) => {
                // 只有 provider、api、model id 三者都相同才算同一个模型；缺一不可。
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;

                let mut transformed_content: Vec<AssistantContentBlock> =
                    Vec::with_capacity(assistant.content.len());
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            // Thinking from another model is dropped: signatures and encrypted
                            // reasoning are only valid for the model that produced them.
                            // 跨模型时连明文 thinking 一起丢：新模型会把这段文本当成自己的
                            // 可见输出，而不是内部推理。
                            if !is_same_model {
                                continue;
                            }
                            // Keep redacted and signed blocks even if the thinking text is empty
                            // (OpenAI encrypted reasoning); they are needed for replay.
                            // TS 用真值判断（`block.redacted || block.thinkingSignature`），空串
                            // 签名是 falsy，不能当作「已签名」。
                            if thinking.redacted == Some(true)
                                || thinking
                                    .thinking_signature
                                    .as_deref()
                                    .is_some_and(|signature| !signature.is_empty())
                            {
                                transformed_content.push(block.clone());
                                continue;
                            }
                            // TS: `!block.thinking || block.thinking.trim() === ""`；JS 的 trim
                            // 会去掉 U+FEFF 但不去 U+0085，必须用 JS 空白集合。
                            if kiana_json::js_string::js_trim(&thinking.thinking).is_empty() {
                                continue;
                            }
                            transformed_content.push(block.clone());
                        }
                        AssistantContentBlock::Text(text) => {
                            // 同模型保留原块（含 textSignature）；跨模型只留文本本身，
                            // 签名对新模型没有意义。
                            if is_same_model {
                                transformed_content.push(block.clone());
                            } else {
                                transformed_content.push(AssistantContentBlock::Text(
                                    TextContent {
                                        kind_text: TextKind::Text,
                                        text: text.text.clone(),
                                        text_signature: None,
                                    },
                                ));
                            }
                        }
                        AssistantContentBlock::ToolCall(tool_call) => {
                            let mut normalized_tool_call: ToolCall = tool_call.clone();
                            // TS: `if (!isSameModel && toolCall.thoughtSignature)` —— 真值判断，
                            // 空串签名保留在输出里。
                            if !is_same_model
                                && normalized_tool_call
                                    .thought_signature
                                    .as_deref()
                                    .is_some_and(|signature| !signature.is_empty())
                            {
                                normalized_tool_call.thought_signature = None;
                            }
                            // 只有跨模型且 id 真的变化时才登记映射；未变化的 id 不需要
                            // 让 toolResult 跟着改。
                            if !is_same_model && let Some(normalize) = normalize_tool_call_id {
                                let normalized_id = normalize(&tool_call.id, model, assistant);
                                if normalized_id != tool_call.id {
                                    tool_call_id_map
                                        .insert(tool_call.id.clone(), normalized_id.clone());
                                    normalized_tool_call.id = normalized_id;
                                }
                            }
                            transformed_content
                                .push(AssistantContentBlock::ToolCall(normalized_tool_call));
                        }
                    }
                }

                Message::Assistant(AssistantMessage {
                    content: transformed_content,
                    ..assistant.clone()
                })
            }
        })
        .collect();

    // Second pass: insert synthetic empty tool results for orphaned tool calls.
    //
    // 第二遍是一个小状态机，四个变量共同描述「上一轮工具调用是否已经收尾」：
    // - `pending_tool_calls`：最近一条 assistant 发出、尚未看到结果的调用；
    // - `existing_tool_result_ids`：自该 assistant 以来已经出现的结果 id（用于去重，
    //   避免给已有结果的调用再合成一条）；
    // - `held_system_messages`：夹在调用与结果之间、需要延后发出的 system 消息；
    // - `result`：最终输出。
    // 触发收尾（close_pending_tool_calls）的三个位置：下一条 assistant、下一条 user、
    // 以及遍历结束；因此待处理状态不会跨出这三种边界。
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // System messages are transparent to tool-call accounting: one that lands between a tool call
    // and its results is held back and emitted after the results (synthetic ones included).
    let mut held_system_messages: Vec<Message> = Vec::new();

    // 收尾一批待处理调用：先为每个没有结果的调用补一条合成 toolResult，再统一放行暂存的
    // system 消息（顺序：合成结果在前、system 在后，这样 system 不会插进调用与结果之间）。
    // `existing_tool_result_ids` 只在有待处理调用时清空；没有 pending 时，孤立 toolResult
    // 也会被记入，下一次带调用的 assistant 会显式清空它。
    fn close_pending_tool_calls(
        result: &mut Vec<Message>,
        pending_tool_calls: &mut Vec<ToolCall>,
        existing_tool_result_ids: &mut std::collections::HashSet<String>,
        held_system_messages: &mut Vec<Message>,
    ) {
        if !pending_tool_calls.is_empty() {
            for tool_call in pending_tool_calls.iter() {
                if !existing_tool_result_ids.contains(&tool_call.id) {
                    result.push(Message::ToolResult(ToolResultMessage {
                        role: ToolResultRole::ToolResult,
                        tool_call_id: tool_call.id.clone(),
                        tool_name: tool_call.name.clone(),
                        content: vec![UserContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: "No result provided".to_string(),
                            text_signature: None,
                        })],
                        details: None,
                        usage: None,
                        nested_calls: None,
                        is_error: true,
                        timestamp: now_ms(),
                        duration_ms: None,
                    }));
                }
            }
            pending_tool_calls.clear();
            existing_tool_result_ids.clear();
        }
        result.append(held_system_messages);
    }

    for message in &transformed {
        match message {
            Message::Assistant(assistant) => {
                // If we have pending orphaned tool calls from a previous assistant, insert
                // synthetic results now.
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );

                // 收尾之后再判断本条是否参与记账：error/aborted 的 assistant 整条跳过，
                // 且它自己的 tool call 不进入 pending（不完整的一轮不能重放）。
                // Skip errored/aborted assistant messages entirely: incomplete turns must not be
                // replayed.
                if matches!(
                    assistant.stop_reason,
                    crate::types::StopReason::Error | crate::types::StopReason::Aborted
                ) {
                    continue;
                }

                let tool_calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
                        _ => None,
                    })
                    .collect();
                // 只有带 tool call 的 assistant 才开启新一轮记账；没有调用的普通回复
                // 不会清空上一批（上一批已在进入本分支时收尾）。
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }

                result.push(message.clone());
            }
            Message::ToolResult(tool_result) => {
                // 只记录 id 用于去重，位置原样保留；孤立结果（没有对应调用）也照常输出。
                existing_tool_result_ids.insert(tool_result.tool_call_id.clone());
                result.push(message.clone());
            }
            Message::System(_) => {
                // 系统消息对 tool call 记账透明：pending 非空时暂存，等结果（含合成结果）
                // 之后由收尾函数统一放行；否则就地输出。
                if !pending_tool_calls.is_empty() {
                    held_system_messages.push(message.clone());
                } else {
                    result.push(message.clone());
                }
            }
            Message::User(_) => {
                // A new user turn interrupts tool flow - insert synthetic results for orphaned
                // calls.
                // 用户轮次是硬边界：先补齐上一轮的结果，再输出用户消息。
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );
                result.push(message.clone());
            }
        }
    }

    // If the conversation ends with unresolved tool calls, synthesize results now.
    close_pending_tool_calls(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        &mut held_system_messages,
    );

    result
}

/// `Date.now()`。
///
/// 返回 Unix 毫秒；系统时钟早于 epoch 时返回 0（不 panic），亚毫秒部分被截断。
/// 只用于合成 toolResult 的时间戳，不参与排序或去重。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Model;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-shared/transform_messages.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture parses")
    }

    /// `"No result provided"` 的合成结果带 `Date.now()`；比较前把这些时间戳归一为 0。
    fn normalize_synthetic_timestamps(messages: &mut [Value]) {
        for message in messages {
            let is_synthetic = message["role"] == "toolResult"
                && message["isError"] == true
                && message["content"]
                    == serde_json::json!([{ "type": "text", "text": "No result provided" }]);
            if is_synthetic {
                message["timestamp"] = Value::from(0);
            }
        }
    }

    fn messages_from(value: &Value) -> Vec<Message> {
        serde_json::from_value(value.clone()).expect("messages deserialize")
    }

    fn to_values(messages: Vec<Message>) -> Vec<Value> {
        messages
            .into_iter()
            .map(|message| serde_json::to_value(message).expect("message serializes"))
            .collect()
    }

    #[test]
    fn transform_cases_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let model: Model =
                serde_json::from_value(case["model"].clone()).expect("model deserializes");
            let messages = messages_from(&case["messages"]);

            let actual = if let Some(kind) = case["normalize"].as_str() {
                // TS 测试里的 `anthropicNormalizeToolCallId`：非 [a-zA-Z0-9_-] → `_`，截断 64。
                let anthropic = |id: &str, _model: &Model, _source: &AssistantMessage| {
                    id.chars()
                        .map(|character| {
                            if character.is_ascii_alphanumeric()
                                || character == '_'
                                || character == '-'
                            {
                                character
                            } else {
                                '_'
                            }
                        })
                        .take(64)
                        .collect::<String>()
                };
                let empty = |_id: &str, _model: &Model, _source: &AssistantMessage| String::new();
                let same = |id: &str, _model: &Model, _source: &AssistantMessage| id.to_string();
                let normalize: &NormalizeToolCallIdFn = match kind {
                    "empty" => &empty,
                    "same" => &same,
                    _ => &anthropic,
                };
                transform_messages_with(&messages, &model, Some(normalize))
            } else {
                transform_messages(&messages, &model)
            };

            let mut actual = to_values(actual);
            let mut expected = case["output"].as_array().expect("output array").clone();
            normalize_synthetic_timestamps(&mut actual);
            normalize_synthetic_timestamps(&mut expected);
            // 用 `js_object::stringify` 比较：JS 的 `0` 与 Rust `f64` 的 `0.0` 都输出 `0`，
            // 同时校验键序与数字格式。
            assert_eq!(
                kiana_json::js_object::stringify(&Value::Array(actual)),
                kiana_json::js_object::stringify(&Value::Array(expected)),
                "case {name} should match the TypeScript reference"
            );
        }
    }

    #[test]
    fn synthetic_results_get_a_fresh_timestamp() {
        let model: Model = serde_json::from_value(serde_json::json!({
            "id": "target-model", "name": "Target", "api": "openai-completions",
            "provider": "sference", "baseUrl": "https://api.example.com/v1",
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000, "maxTokens": 4096,
        }))
        .unwrap();
        let messages = vec![
            Message::User(UserMessage {
                role: crate::types::UserRole::User,
                content: UserContent::Text("go".into()),
                timestamp: 1,
            }),
            Message::Assistant(AssistantMessage {
                api: "openai-completions".into(),
                provider: "sference".into(),
                model: "target-model".into(),
                content: vec![AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: crate::types::ToolCallKind::ToolCall,
                    id: "a".into(),
                    name: "bash".into(),
                    arguments: Default::default(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                })],
                stop_reason: crate::types::StopReason::ToolUse,
                timestamp: 1,
                ..AssistantMessage::default()
            }),
        ];
        let before = now_ms();
        let result = transform_messages(&messages, &model);
        let after = now_ms();
        let Message::ToolResult(synthetic) = result.last().unwrap() else {
            panic!("last message is a synthetic tool result");
        };
        // 允许时钟小幅回拨/跳变（`Date.now()` 语义，不依赖严格单调）。
        assert!(
            (synthetic.timestamp - before).abs() <= 60_000
                && (synthetic.timestamp - after).abs() <= 60_000
        );
    }

    /// `transformMessages` 的真值/trim 边界（逐条对照 TS 参考实现运行结果）。
    fn target_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "target-model", "name": "Target", "api": "openai-completions",
            "provider": "sference", "baseUrl": "https://api.example.com/v1",
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000, "maxTokens": 4096,
        }))
        .expect("model")
    }

    fn assistant_message(content: Value, overrides: Value) -> Message {
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": content,
            "api": "openai-completions",
            "provider": "sference",
            "model": "target-model",
            "usage": {
                "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
            },
            "stopReason": "stop",
            "timestamp": 1,
        });
        if let (Some(message), Some(overrides)) = (message.as_object_mut(), overrides.as_object()) {
            for (key, value) in overrides {
                message.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(message).expect("assistant message")
    }

    fn transformed_content(messages: &[Message], model: &Model) -> Value {
        let transformed = transform_messages(messages, model);
        serde_json::to_value(&transformed[0]).expect("message serializes")["content"].clone()
    }

    #[test]
    fn empty_thinking_signature_is_not_treated_as_signed() {
        let model = target_model();
        // TS: `block.redacted || block.thinkingSignature` 是真值判断；空串签名 + 空 thinking
        // 会被丢弃（reviewer 案例，9/400 随机差分命中）。
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "", "thinkingSignature": "" }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([])
        );
        // thinking 非空时空串签名仍保留（跌落到 `!block.thinking` 检查）。
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "hi", "thinkingSignature": "" }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([{ "type": "thinking", "thinking": "hi", "thinkingSignature": "" }])
        );
        // `redacted: false` 不保留；`redacted: true` 保留。
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "", "redacted": false }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([])
        );
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "", "redacted": true }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([{ "type": "thinking", "thinking": "", "redacted": true }])
        );
    }

    #[test]
    fn thinking_emptiness_uses_javascript_trim_semantics() {
        let model = target_model();
        // U+FEFF（BOM/ZWNBSP）在 JS 的 trim 里算空白 → 丢弃；Rust 的 `str::trim` 不去掉它。
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "\u{FEFF}" }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([])
        );
        // U+0085（NEL）在 JS 的 trim 里**不是**空白 → 保留；Rust 的 `char::is_whitespace` 会去掉。
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "\u{0085}" }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([{ "type": "thinking", "thinking": "\u{0085}" }])
        );
        assert_eq!(
            transformed_content(
                &[assistant_message(
                    serde_json::json!([{ "type": "thinking", "thinking": "\u{00A0}\u{3000}" }]),
                    serde_json::json!({}),
                )],
                &model,
            ),
            serde_json::json!([])
        );
    }

    #[test]
    fn empty_thought_signature_survives_cross_model_transform() {
        let model = target_model();
        // TS 只删除真值的 `thoughtSignature`；空串保留在输出里。
        let content = transformed_content(
            &[assistant_message(
                serde_json::json!([{
                    "type": "toolCall", "id": "t", "name": "n", "arguments": {},
                    "thoughtSignature": "",
                }]),
                serde_json::json!({ "model": "other" }),
            )],
            &model,
        );
        assert_eq!(
            content,
            serde_json::json!([{
                "type": "toolCall", "id": "t", "name": "n", "arguments": {},
                "thoughtSignature": "",
            }])
        );
        // 非空签名仍被删除。
        let content = transformed_content(
            &[assistant_message(
                serde_json::json!([{
                    "type": "toolCall", "id": "t", "name": "n", "arguments": {},
                    "thoughtSignature": "x",
                }]),
                serde_json::json!({ "model": "other" }),
            )],
            &model,
        );
        assert_eq!(
            content,
            serde_json::json!([{ "type": "toolCall", "id": "t", "name": "n", "arguments": {} }])
        );
    }

    #[test]
    fn lax_message_content_is_normalized_before_typed_parsing() {
        // `test/lax-message-content.test.ts`：无类型调用方的 `content: null` / 缺失 content。
        let raw = vec![
            serde_json::json!({ "role": "user", "content": null, "timestamp": 1_700_000_000_000i64 }),
            serde_json::json!({
                "role": "assistant",
                "content": null,
                "api": "openai-completions",
                "provider": "openai",
                "model": "test-model",
                "usage": {
                    "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
                },
                "stopReason": "stop",
                "timestamp": 1_700_000_000_000i64,
            }),
            serde_json::json!({
                "role": "toolResult",
                "toolCallId": "call_1",
                "toolName": "web_search",
                "isError": false,
                "timestamp": 1_700_000_000_000i64,
            }),
        ];

        let normalized = normalize_message_content(&raw);
        assert_eq!(normalized[0]["content"], serde_json::json!([]));
        assert_eq!(normalized[1]["content"], serde_json::json!([]));
        assert_eq!(normalized[2]["content"], serde_json::json!([]));
        // `{ ...msg, content: [] }` 对已有键保持原位（键序不变）。
        assert_eq!(
            normalized[0]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec!["role", "content", "timestamp"]
        );

        let result = transform_lax_messages(&raw, &target_model()).expect("lax messages parse");
        assert_eq!(result.len(), 3);
        for message in &result {
            let content = match message {
                Message::User(user) => serde_json::to_value(&user.content).unwrap(),
                Message::Assistant(assistant) => serde_json::to_value(&assistant.content).unwrap(),
                Message::ToolResult(tool_result) => {
                    serde_json::to_value(&tool_result.content).unwrap()
                }
                Message::System(system) => serde_json::to_value(&system.content).unwrap(),
            };
            assert_eq!(content, serde_json::json!([]));
        }
        // 非 `null` 的 content 原样返回（含字符串简写）。
        let untouched = serde_json::json!({ "role": "user", "content": "hi", "timestamp": 1 });
        assert_eq!(
            normalize_message_content(std::slice::from_ref(&untouched)),
            vec![untouched]
        );
    }

    #[test]
    fn split_error_tool_result_matches_typescript_reference() {
        let fixture = fixture();
        let cases = &fixture["split"];
        let message_of = |value: &Value| -> ToolResultMessage {
            serde_json::from_value(value.clone()).expect("tool result deserializes")
        };
        let error_message = message_of(&serde_json::json!({
            "role": "toolResult", "toolCallId": "call_9", "toolName": "screenshot",
            "content": [
                { "type": "text", "text": "before" },
                { "type": "image", "data": "AA", "mimeType": "image/png" },
                { "type": "image", "data": "BB", "mimeType": "image/png" },
                { "type": "text", "text": "(image attached after the tool results)" },
                { "type": "image", "data": "CC", "mimeType": "image/png" }
            ],
            "isError": true, "timestamp": 1
        }));

        let split = split_error_tool_result_images(&error_message).expect("split");
        let expected = &cases["error"];
        assert_eq!(split.caption, expected["caption"].as_str().unwrap());
        assert_eq!(
            serde_json::to_value(&split.content).unwrap(),
            expected["content"]
        );
        assert_eq!(
            serde_json::to_value(&split.images).unwrap(),
            expected["images"]
        );

        let mut non_error = error_message.clone();
        non_error.is_error = false;
        assert_eq!(
            split_error_tool_result_images(&non_error).is_none(),
            cases["nonError"].is_null()
        );

        let mut no_images = error_message.clone();
        no_images.content = vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "x".into(),
            text_signature: None,
        })];
        assert!(split_error_tool_result_images(&no_images).is_none());
        assert!(cases["noImages"].is_null());

        let single = message_of(&serde_json::json!({
            "role": "toolResult", "toolCallId": "call_9", "toolName": "screenshot",
            "content": [{ "type": "image", "data": "A", "mimeType": "image/png" }],
            "isError": true, "timestamp": 1
        }));
        let split = split_error_tool_result_images(&single).expect("split single");
        assert_eq!(
            serde_json::to_value(&split.content).unwrap(),
            cases["singleImage"]["content"]
        );
    }
}
