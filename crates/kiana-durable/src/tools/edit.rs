//! `tools/edit.ts`：基于精确文本替换的单文件编辑。
//!
//! 参数先经过 [`prepare_edit_arguments`] 修复模型常发的形状（`edits` 是 JSON 字符串、
//! 单个编辑对象，或顶层 `oldText`/`newText` 对），再在文件变更队列里读、改、写。
//!
//! # 职责与边界
//! 本文件只负责参数修复、文件读写、BOM/行尾保持与错误文案；真正的文本匹配、模糊归一化、
//! 唯一性与重叠检查在 `crate::tools::edit_diff`。`edit` 永远只改一个文件，且所有替换都
//! 针对**原文件**（不是上一条替换后的内容）。
//!
//! # 执行流程
//! 1. harness 先运行 `prepare_arguments`（即 [`prepare_edit_arguments`]），再按 schema
//!    校验；因此 `edits` 是 JSON 字符串、单个对象或顶层旧字段的调用也能进入 execute。
//! 2. `execute_edit` 手工校验输入，取执行环境，把路径解析成绝对路径，然后在
//!    `with_file_mutation_queue` 内串行化同一文件的"读 → 改 → 写"。
//! 3. 队列内顺序：abort 检查 → `file_info`（必须是 file 或 symlink）→ 读文本 → abort 检查
//!    → 去 BOM、记录原行尾、统一成 LF → 应用替换 → abort 检查 → 恢复 BOM/行尾并写入
//!    → abort 检查 → 生成 diff 与 patch。
//!
//! # 边界与失败模式
//! - 某条 `oldText` 找不到、匹配不唯一或与其它条重叠时，`edit_diff` 直接报错；错误发生在
//!    写入之前，磁盘内容保持不变。
//! - 写入完成后仍会再做一次 abort 检查：这时写入已经生效，但工具调用返回
//!    `Operation aborted`。`tests/tools.rs` 的
//!    `keeps_the_mutation_queue_locked_until_an_aborted_edit_write_settles` 依赖该行为，
//!    也依赖队列锁一直持有到写完成。
//! - 文件访问错误里只有 `file_info`、`read_text_file`、`write_file` 三类经 `edit_access_error`
//!   转成只含路径与错误码名的文案，底层 `message`/`cause` 不进入该文案；`resolve_tool_path`
//!   的 `absolute_path` 与 `with_file_mutation_queue` 内 `mutation_key` 的
//!   `absolute_path`/`canonical_path` 返回的 `FileError` 不经转换、原样上抛，其 `message`
//!   会经 `error.to_string()` 进入工具错误诊断（见 `harness/tool.rs` 的 `tool_error_result`）。
//! - 成功结果的 `details` 总是含 `diff` 与 `patch`；只有存在首处变化时才含
//!    `firstChangedLine`。
//!
//! # 注意事项
//! - BOM 与行尾风格按原文件恢复：先 `strip_bom`，用剩余文本检测**第一种**行尾，写回前
//!   拼回 BOM 并把所有 LF 统一恢复成该风格。原本混用行尾的文件会被统一成同一种。
//! - diff 与 patch 都基于 LF 归一化后的 `baseContent`/`newContent`，不是磁盘上的原始字节。

use std::sync::Arc;

use kiana_ai::{TextContent, TextKind, UserContentBlock};
use kiana_chord::context::Context;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::env::{FileContent, FileErrorCode, FileKind};
use crate::harness::define::define_tool;
use crate::harness::types::{
    ToolExecuteFn, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
};
use crate::tools::edit_diff::{
    Edit, apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom,
};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;
use crate::types::BoxError;

/// `edit` 的参数。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    /// 文件路径（相对或绝对）。
    pub path: String,
    /// 一处或多处定向替换；每条都针对**原文件**匹配，且不能互相重叠。
    pub edits: Vec<Edit>,
}

/// `EditToolDetails`。
///
/// `details` 的 Rust 视图；`execute_edit` 直接构造 JSON 对象，字段与 JSON 键的对应关系
/// （`first_changed_line` ↔ `firstChangedLine`）需要与这里手工保持同步。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// 带行号的展示 diff（不是 unified diff）。
    pub diff: String,
    /// 标准 unified patch。
    pub patch: String,
    /// 新文件里第一处变化的行号（1 起）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

// 单个编辑对象的判定：`oldText` 与 `newText` 都存在且都是字符串；只有一个字段的形状
// 不在这里修复，留给 schema 校验报错。
fn is_single_edit_input(value: &JsonValue) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    matches!(object.get("oldText"), Some(JsonValue::String(_)))
        && matches!(object.get("newText"), Some(JsonValue::String(_)))
}

/// 修复模型常发的参数形状：`edits` 是 JSON 字符串或单个编辑对象，以及顶层
/// `oldText`/`newText` 对。只改副本，调用方参数保持不变。
///
/// 输入不是 JSON 对象时原样返回；harness 之后会因为"修复结果不是对象"以
/// `invalid_arguments` 结束调用（见 `ToolRegistration::prepare_arguments` 的契约）。
/// 字符串解析失败时也保持原样，让 schema 校验报错，而不是在这里静默丢弃。
///
/// 顶层 `oldText`/`newText` 必须**同时**是字符串才会被搬进 `edits` 数组末尾，并从顶层删除；
/// 只有一个存在时不动，避免把半截输入吞掉。
///
/// 当前实现总是返回 `Ok`；`Result` 只是为了满足 `prepare_arguments` 的统一签名。
pub fn prepare_edit_arguments(input: &JsonValue) -> Result<JsonValue, BoxError> {
    let Some(object) = input.as_object() else {
        return Ok(input.clone());
    };
    let mut arguments: JsonMap<String, JsonValue> = object.clone();
    // 字符串形态：只有能解析成数组或单个编辑对象时才替换，解析失败保持原文。
    if let Some(JsonValue::String(text)) = arguments.get("edits").cloned() {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(&text) {
            if parsed.is_array() {
                arguments.insert("edits".to_owned(), parsed);
            } else if is_single_edit_input(&parsed) {
                arguments.insert("edits".to_owned(), JsonValue::Array(vec![parsed]));
            }
        }
    } else if let Some(value) = arguments.get("edits").cloned()
        && is_single_edit_input(&value)
    {
        // 单对象形态包成数组，满足 schema 的 array 约束。
        arguments.insert("edits".to_owned(), JsonValue::Array(vec![value]));
    }

    // 遗留字段：两者都是字符串才搬移；追加在已有 edits 之后，顺序交给 edit_diff 按位置处理。
    let legacy_old = match arguments.get("oldText") {
        Some(JsonValue::String(value)) => Some(value.clone()),
        _ => None,
    };
    let legacy_new = match arguments.get("newText") {
        Some(JsonValue::String(value)) => Some(value.clone()),
        _ => None,
    };
    let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) else {
        return Ok(JsonValue::Object(arguments));
    };
    let mut edits = match arguments.get("edits") {
        Some(JsonValue::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    edits.push(json!({ "oldText": old_text, "newText": new_text }));
    arguments.shift_remove("oldText");
    arguments.shift_remove("newText");
    arguments.insert("edits".to_owned(), JsonValue::Array(edits));
    Ok(JsonValue::Object(arguments))
}

// 手工校验输入而不是只依赖 schema：`edits` 为空时的错误文案是当前稳定契约（改动需同步
// 测试）；`path` 不是字符串、某条 edit 缺少字符串 `oldText`/`newText` 这两种情况同样需要
// 明确拒绝，因此补了本文本（`path must be a string.` /
// `Every edit must have string oldText and newText.`）。校验同时保证
// 后续 `Edit` 构造和 diff 逻辑拿到的一定是字符串，不做二次检查。
// 注意 `edits` 必须是非空数组：空数组得到 invalid 错误，而不是成功结果。
fn validate_edit_input(input: &JsonValue) -> Result<(String, Vec<Edit>), BoxError> {
    let edits = match input.get("edits") {
        Some(JsonValue::Array(edits)) if !edits.is_empty() => edits,
        _ => {
            return Err(std::io::Error::other(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            )
            .into());
        }
    };
    let Some(path) = input.get("path").and_then(JsonValue::as_str) else {
        return Err(
            std::io::Error::other("Edit tool input is invalid. path must be a string.").into(),
        );
    };
    let parsed = edits
        .iter()
        .map(|edit| {
            let old_text = edit.get("oldText").and_then(JsonValue::as_str);
            let new_text = edit.get("newText").and_then(JsonValue::as_str);
            match (old_text, new_text) {
                (Some(old_text), Some(new_text)) => Ok(Edit {
                    old_text: old_text.to_owned(),
                    new_text: new_text.to_owned(),
                }),
                _ => Err(std::io::Error::other(
                    "Edit tool input is invalid. Every edit must have string oldText and newText.",
                )
                .into()),
            }
        })
        .collect::<Result<Vec<Edit>, BoxError>>()?;
    Ok((path.to_owned(), parsed))
}

// 名字与 `FileErrorCode` 的 snake_case 序列化值一致（`testing/env_conformance.rs` 的期望
// JSON 也使用这些名字）；`edit_access_error` 把名字嵌进对外错误文本，改这里会改变工具错误。
fn file_error_code_name(code: FileErrorCode) -> &'static str {
    match code {
        FileErrorCode::Aborted => "aborted",
        FileErrorCode::NotFound => "not_found",
        FileErrorCode::PermissionDenied => "permission_denied",
        FileErrorCode::NotDirectory => "not_directory",
        FileErrorCode::IsDirectory => "is_directory",
        FileErrorCode::Invalid => "invalid",
        FileErrorCode::NotSupported => "not_supported",
        FileErrorCode::Unknown => "unknown",
    }
}

// 把文件系统错误折成工具错误文案。文案只含路径与错误码名（当前稳定契约）；`FileError` 的
// `message` 与 `cause` 不会进入工具结果，排查时只能拿到错误码。
fn edit_access_error(path: &str, error: &crate::env::FileError) -> BoxError {
    std::io::Error::other(format!(
        "Could not edit file: {path}. Error code: {}.",
        file_error_code_name(error.code)
    ))
    .into()
}

// 上下文没有 abort signal 时视为未取消；有 signal 时以它的 `is_aborted` 为准。
fn aborted(context: &dyn Context) -> bool {
    context
        .abort_signal()
        .is_some_and(|signal| signal.is_aborted())
}

/// `createEditTool()`。
///
/// 注册项没有结构化输出 schema 与输出限制；`prepare_arguments` 指向
/// [`prepare_edit_arguments`]，因此形状修复发生在 schema 校验之前。`parameters` 要求
/// `path` 与 `edits`，并在 schema 描述里明确"每条 `oldText` 必须唯一且互不重叠"。
pub fn create_edit_tool() -> ToolRegistration {
    let parameters = json!({
        "type": "object",
        "required": ["path", "edits"],
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to edit (relative or absolute)"
            },
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["oldText", "newText"],
                    "properties": {
                        "oldText": {
                            "type": "string",
                            "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                        },
                        "newText": {
                            "type": "string",
                            "description": "Replacement text for this targeted edit."
                        }
                    }
                },
                "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead."
            }
        }
    });
    let execute: ToolExecuteFn = Arc::new(|args, api, context| {
        Box::pin(async move { execute_edit(args, api, context).await })
    });
    let prepare: Arc<dyn Fn(&JsonValue) -> Result<JsonValue, BoxError> + Send + Sync> =
        Arc::new(prepare_edit_arguments);
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "edit".to_owned(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".to_owned(),
            parameters,
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: Some(prepare),
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(execute),
    })
}

// 一次 edit 调用的主体：校验输入 → 取环境并解析绝对路径 → 在文件变更队列内完成读改写。
// 队列外的路径解析不检查存在性；文件访问、替换与取消都在队列内转换成错误。
async fn execute_edit(
    args: JsonValue,
    api: Arc<dyn ToolExecutionApi>,
    context: Arc<dyn Context>,
) -> Result<ToolExecutionResult, BoxError> {
    let (path, edits) = validate_edit_input(&args)?;
    let env = require_env(api.as_ref())?;
    // 队列外先解析一次路径供读写使用；进队列后 `with_file_mutation_queue` 会自行再算
    // 绝对/规范路径作为 key（因此这里的绝对路径不必是规范路径）。
    let absolute_path = resolve_tool_path(env.as_ref(), &path, context.as_ref()).await?;
    with_file_mutation_queue(
        env.as_ref(),
        &absolute_path,
        || async {
            // 可能刚在队列上等过，先拦下排队期间触发的取消，避免做无用的读改写。
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }
            // 访问失败（不存在、权限不足等）统一转成带错误码名的文案。
            let info = env
                .file_info(&absolute_path, context.as_ref())
                .await
                .map_err(|error| edit_access_error(&path, &error))?;
            // 只允许普通文件与符号链接；目录、设备等一律拒绝。
            if info.kind != FileKind::File && info.kind != FileKind::Symlink {
                return Err(std::io::Error::other(format!(
                    "Could not edit file: {path}. Path is not a file."
                ))
                .into());
            }

            // 整份读入内存：唯一性/重叠检查需要看完整原文。
            let content = env
                .read_text_file(&absolute_path, context.as_ref())
                .await
                .map_err(|error| edit_access_error(&path, &error))?;
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }

            // BOM 不属于正文：先剥离，行尾检测与替换都在无 BOM 文本上进行。
            let (bom, text) = strip_bom(&content);
            // 记录原文件的第一种行尾，写回时统一恢复成同一种风格。
            let original_ending = detect_line_ending(&text);
            let normalized_content = normalize_to_lf(&text);
            // 替换、唯一性与重叠检查都在这里；失败时尚未写文件。
            let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }

            // 写回时恢复原文件的 BOM 与行尾风格。
            let final_content = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, original_ending)
            );
            env.write_file(
                &absolute_path,
                FileContent::Text(final_content),
                context.as_ref(),
            )
            .await
            .map_err(|error| edit_access_error(&path, &error))?;
            // 写入已经生效；此处取消让调用返回错误，但磁盘内容已经是新内容。
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }

            // 展示 diff 与 unified patch 都基于 LF 归一化的内容；上下文行数固定 4。
            let diff_result = generate_diff_string(&applied.base_content, &applied.new_content, 4);
            let mut details = JsonMap::new();
            details.insert("diff".to_owned(), json!(diff_result.diff));
            details.insert(
                "patch".to_owned(),
                json!(generate_unified_patch(
                    &path,
                    &applied.base_content,
                    &applied.new_content,
                    4
                )),
            );
            // 没有变化时省略该字段（TS 的 `undefined` 不进入 JSON）。
            if let Some(first_changed_line) = diff_result.first_changed_line {
                details.insert("firstChangedLine".to_owned(), json!(first_changed_line));
            }
            // 输出文案里的块数是输入 edits 的条数；成功时全部已应用（失败会提前返回）。
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("Successfully replaced {} block(s) in {path}.", edits.len()),
                    text_signature: None,
                })]),
                details: Some(JsonValue::Object(details)),
                ..ToolExecutionResult::default()
            })
        },
        context.as_ref(),
    )
    .await
}
