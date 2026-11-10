//! core/tools/edit.ts。
//!
//! `edit` 工具：在 mutation 队列里完成「可写性预检 → 读 → BOM/行尾处理 →
//! [`apply_edits_to_normalized_content`] → 写回 → 生成 diff/patch」。
//!
//! - `prepareArguments` 兼容三类模型输出（`edits` 是 JSON 字符串、单个 edit 对象、
//!   legacy 顶层 `oldText`/`newText`），键顺序的覆盖语义与 JS 对象字面量一致。
//! - 中止语义与 write 相同：只在每个 `await` 之后检查，不从监听器 reject。
//! - `realpath` key 计算失败（TS 的 `getMutationQueueKey` rethrow）在 `access` 之前直接
//!   以 Node 的原始文案失败（`ELOOP: ..., realpath '<abs>'`）。
//! - 访问失败文案：`Could not edit file: {path}. Error code: {code}.`；
//!   没有 `code` 的 Error 用 `Error: {message}`（TS 的 `String(error)`）。
//!
//! # 调用方
//!
//! `crate::index` 的 `create_tool_definition` / `create_tool` / `create_coding_tool_definitions`
//! 等工厂通过 [`create_edit_tool_definition`] / [`create_edit_tool`] 组装 `edit`；
//! `kiana-coding-agent` 的 SDK 导出层再导出这两个函数（`kiana-session::sdk` 只再导出
//! [`create_edit_tool`]）。
//! `tests/edit.rs` 用自定义 [`EditOperations`] 驱动同一条执行路径。
//!
//! # 执行流程
//!
//! 1. `validate_edit_input` 校验 `edits`（必须是数组且非空，逐项可反序列化），失败时统一
//!    返回 [`INVALID_EDIT_INPUT_MESSAGE`]；`path` 缺失或不是字符串时退化为空串，不在此
//!    报错。此时**还没有**任何文件系统访问；
//! 2. 用 `ctx.cwd()` 解析绝对路径；占位上下文或空 cwd 时回落到工具构造时的 `cwd`；
//! 3. 进入 [`try_with_file_mutation_queue`]：同一真实路径的编辑串行，排队期间可被取消；
//! 4. 队列内依次：中止检查 → `access` 预检 → `read_file` → WHATWG UTF-8 解码 →
//!    `split_bom` → 检测行尾 → 归一为 LF → [`apply_edits_to_normalized_content`] →
//!    恢复 BOM/行尾 → `write_file`；
//! 5. 用**归一化后**的 base/new 内容生成 diff 与 unified patch，放进 `details`。
//!
//! # 边界与失败模式
//!
//! - 中止检查在进入队列闭包时一次，之后每个 `await` 结束后各一次；`write_file` 之后的
//!   那次检查意味着「文件已经写入、工具却返回 Operation aborted」是可能的结果（不回滚）。
//! - `access` 失败被包装成 `Could not edit file: {path}. …`；`read_file`/`write_file` 的
//!   错误只取 `message` 原样透传（不包装、不带路径前缀）。文案里的路径是模型给的原始
//!   `path`，不是绝对路径。
//! - 队列 key 计算失败（例如 `ELOOP`/`EACCES`）在任何文件操作之前返回，错误文案保持
//!   Node 原样，不会被改写成 `Could not edit file`。
//! - 写回的内容包含原来的 BOM 与**检测到的**行尾风格。检测规则是「首个 `\r\n` 出现在
//!   首个 `\n` 之前才算 CRLF」；写回时所有换行都按这一种风格重建，因此混合行尾的文件
//!   会被统一（单独出现的 `\r` 也先被 `normalize_to_lf` 变成 `\n`）。
//! - diff/patch 基于 LF 归一化、去掉 BOM 的文本，不反映原始文件的行尾/BOM 字节。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{ConstrainedSampling, JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::text::split_bom;
pub use kiana_ext_api::tool_types::{EditToolDetails, EditToolInput};
use kiana_ext_api::tool_types::{
    RenderShell, ReplaceEdit, ToolDefinition, ToolDefinitionExecute, ToolRenderers,
};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_sandbox::ExecutionPolicy;
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::edit_diff::{decode_utf8, detect_line_ending, normalize_to_lf, restore_line_endings};
use crate::file_mutation_queue::try_with_file_mutation_queue;
use crate::index::multi_edit::{
    EditError as MultiEditError, EditStrategyEvent, FileEditOutcome, plan_file_edits,
};
use crate::path_utils::resolve_to_cwd;
use crate::read::{
    PriorReadStatus, R_OK, ReadRecord, ReadStateRegistry, ToolOperationError, W_OK, access_path,
    effective_cwd, fingerprint_bytes, prior_read_verification_error, read_before_write_error,
    read_file_bytes, read_not_text_error, read_range_not_seen_error, sandbox_denied_error,
    write_file_bytes,
};
pub use crate::renderers::edit::EditRenderState;
use crate::renderers::edit::edit_renderers;

/// `editToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `editToolSystemPromptContribution`。
///
/// `snippet`/`guidelines` 会原样进入 [`ToolDefinition`] 的 prompt 元数据（由
/// [`create_edit_tool_definition`] 使用）。
pub fn edit_tool_system_prompt_contribution() -> EditToolSystemPromptContribution {
    EditToolSystemPromptContribution {
        snippet: "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
        guidelines: &[
            "Use edit for precise changes (edits[].oldText must match exactly)",
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
        ],
    }
}

/// `edit` 的工具描述（逐字）。
pub const EDIT_TOOL_DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.";

/// `Edit tool input is invalid. edits must contain at least one replacement.`
pub const INVALID_EDIT_INPUT_MESSAGE: &str =
    "Edit tool input is invalid. edits must contain at least one replacement.";

/// 可插拔的文件编辑操作。TS: `EditOperations`。
///
/// 契约（默认实现 [`LocalEditOperations`] 满足它，自定义实现也必须满足）：
///
/// - `read_file` 返回**原始字节**，本模块按 WHATWG UTF-8 规则解码（非法字节变 U+FFFD），
///   不做编码转换；
/// - `write_file` 收到的 `content` 已经包含 BOM 与最终行尾，实现必须原样写盘，不要再做
///   换行/编码归一；
/// - `access` 是读/写之前的权限预检；它只判断权限，目录也可能通过（默认实现：Unix 用
///   `access(2)`，非 Unix 用 stat + open 探测且目录直接放行，随后 `read_file` 才会报
///   `EISDIR`）；
/// - 返回的 [`ToolOperationError`] 的 `message` 会逐字进入工具错误，`code` 用于
///   `access` 失败的 `Error code:` 文案。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性（见 kiana-ai 同类处理）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait EditOperations: Send + Sync {
    /// 读取文件内容。
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError>;
    /// 写入文件内容。
    async fn write_file(
        &self,
        absolute_path: &Path,
        content: &str,
    ) -> Result<(), ToolOperationError>;
    /// 检查文件可读**且**可写（`R_OK | W_OK`）。
    async fn access(&self, absolute_path: &Path) -> Result<(), ToolOperationError>;
}

/// 默认操作：本地文件系统。
///
/// `access` 直接用 `access(2)`（经 `crate::read::access_path` 的 Unix 分支），不是
/// open+close：目录只要权限位允许就会通过预检。非 Unix 没有 `access(2)`，改为
/// stat + open 探测，目录直接放行（错误消息仍以 access 为 syscall 名）。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalEditOperations;

#[async_trait]
impl EditOperations for LocalEditOperations {
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError> {
        read_file_bytes(absolute_path).await
    }

    async fn write_file(
        &self,
        absolute_path: &Path,
        content: &str,
    ) -> Result<(), ToolOperationError> {
        write_file_bytes(absolute_path, content.as_bytes()).await
    }

    async fn access(&self, absolute_path: &Path) -> Result<(), ToolOperationError> {
        // `fs.access(path, R_OK | W_OK)` 是 access(2)：目录只要可写就通过（不会返回 EISDIR），
        // 后续 readFile 才以 `EISDIR: ..., read '<path>'` 失败。
        access_path(absolute_path, R_OK | W_OK)
    }
}

/// `edit` 工具的选项。
#[derive(Clone, Default)]
pub struct EditToolOptions {
    /// 自定义文件编辑操作。默认本地文件系统。
    pub operations: Option<Arc<dyn EditOperations>>,
    /// 写入门禁使用的执行策略。`None` 时按执行期 cwd 从 `KIANA_SANDBOX_POLICY` 构造。
    pub execution_policy: Option<Arc<ExecutionPolicy>>,
    /// 读记录表。`None` 时使用进程级共享实例（与 read 工具一致）。
    pub read_state: Option<Arc<ReadStateRegistry>>,
}

impl std::fmt::Debug for EditToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EditToolOptions")
            .field("operations", &self.operations.is_some())
            .field("execution_policy", &self.execution_policy.is_some())
            .field("read_state", &self.read_state.is_some())
            .finish()
    }
}

/// `editSchema`（TypeBox 序列化结果，逐字节）。
///
/// 每次调用重新构造 JSON；schema 的 `properties` 里没有 legacy 的顶层 `oldText`/`newText`
/// （它们只由 [`prepare_edit_arguments`] 兼容），`edits` 的每个元素必须同时有这两个键。
pub fn edit_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["path", "edits"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["oldText", "newText"],
                    "properties": {
                        "oldText": { "type": "string", "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." },
                        "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                    }
                },
                "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead."
            }
        }
    })
}

/// `{ type: "json_schema", strict: "prefer" }`。
fn json_schema_prefer() -> ConstrainedSampling {
    ConstrainedSampling::Config(kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
        strict: kiana_ai::types::StrictMode::Prefer,
    })
}

fn aborted_error() -> AgentToolError {
    AgentToolError {
        message: "Operation aborted".to_string(),
        name: None,
        stack: None,
    }
}

fn operation_error(error: ToolOperationError) -> AgentToolError {
    AgentToolError {
        message: error.message,
        name: None,
        stack: None,
    }
}

/// `String(error)` / `Error code: X` 的编辑访问错误文案。
///
/// 有 `code` 时只显示 code（不带 `message`），否则用 `Error: {message}`；两者都以句号结尾，
/// 前缀固定为 `Could not edit file: {path}. `。
pub(crate) fn format_edit_access_error(path: &str, error: &ToolOperationError) -> String {
    let detail = match &error.code {
        Some(code) => format!("Error code: {code}"),
        None => format!("Error: {}", error.message),
    };
    format!("Could not edit file: {path}. {detail}.")
}

/// `isSingleEditInput(value)`：非数组对象且 `oldText`/`newText` 都是字符串。
fn is_single_edit_input(value: Option<&JsonValue>) -> bool {
    let Some(JsonValue::Object(object)) = value else {
        return false;
    };
    matches!(object.get("oldText"), Some(JsonValue::String(_)))
        && matches!(object.get("newText"), Some(JsonValue::String(_)))
}

/// `prepareEditArguments(input)`：模型参数兼容 shim。
///
/// 返回 `None` 表示「返回同一个对象」（TS 的 `return args`），对 `args` 的就地改写保留；
/// `Some` 表示 TS 的 `return { ...rest, edits }`：**新对象**，原对象只保留步骤 1-2 的就地
/// 改写（`edits` 字符串/单对象归一），顶层 `oldText`/`newText` 仍在原对象上。键顺序与 JS
/// 对象字面量一致：`rest` 的键（`path`、`edits` 等）先，`edits` 若原本存在则保持原位置
/// （值被覆写），否则追加到末尾。
///
/// `edits` 是字符串但不是合法 JSON、或解析结果既不是数组也不是 edit 对象时保持原样；
/// 这类输入随后由 `validate_edit_input` 以固定文案拒绝。没有任何 legacy 字段、且 `edits`
/// 不需要归一化时返回 `None`。
pub fn prepare_edit_arguments(args: &mut JsonValue) -> Option<JsonValue> {
    let JsonValue::Object(object) = args else {
        return None;
    };

    // 1) edits 是 JSON 字符串。
    if let Some(JsonValue::String(raw)) = object.get("edits") {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(raw) {
            if parsed.is_array() {
                object.insert("edits".to_string(), parsed);
            } else if is_single_edit_input(Some(&parsed)) {
                // TS: `args.edits = [parsed]` —— 原样包成单元素数组，保留额外属性与键顺序。
                object.insert("edits".to_string(), JsonValue::Array(vec![parsed]));
            }
        }
    } else if let Some(value) = object.get("edits") {
        // 2) edits 是单个 edit 对象：同样原样包裹（`args.edits = [args.edits]`）。
        if is_single_edit_input(Some(value)) {
            let value = value.clone();
            object.insert("edits".to_string(), JsonValue::Array(vec![value]));
        }
    }

    // 3) legacy 顶层 oldText/newText：TS 解构出 `rest` 后返回**新对象**，原对象不动。
    let legacy_old = match object.get("oldText") {
        Some(JsonValue::String(text)) => Some(text.clone()),
        _ => None,
    };
    let legacy_new = match object.get("newText") {
        Some(JsonValue::String(text)) => Some(text.clone()),
        _ => None,
    };
    let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) else {
        return None;
    };

    let mut edits = match object.get("edits") {
        Some(JsonValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    edits.push(edit_to_json(&ReplaceEdit { old_text, new_text }));

    let mut folded = serde_json::Map::with_capacity(object.len());
    for (key, value) in object.iter() {
        match key.as_str() {
            "oldText" | "newText" => continue,
            "edits" => {
                folded.insert(key.clone(), JsonValue::Array(edits.clone()));
            }
            _ => {
                folded.insert(key.clone(), value.clone());
            }
        }
    }
    if !folded.contains_key("edits") {
        folded.insert("edits".to_string(), JsonValue::Array(edits));
    }
    Some(JsonValue::Object(folded))
}

/// `ReplaceEdit` → JSON（键顺序 oldText/newText）。
fn edit_to_json(edit: &ReplaceEdit) -> JsonValue {
    serde_json::to_value(edit).unwrap_or(JsonValue::Null)
}

/// `validateEditInput(input)`。
///
/// `edits` 必须是数组且非空，每个元素按 [`ReplaceEdit`] 反序列化；
/// 所有失败分支复用同一条固定文案，调用方无法区分具体原因。
fn validate_edit_input(params: &JsonValue) -> Result<(String, Vec<ReplaceEdit>), AgentToolError> {
    let invalid = || AgentToolError {
        message: INVALID_EDIT_INPUT_MESSAGE.to_string(),
        name: None,
        stack: None,
    };
    let Some(JsonValue::Array(items)) = params.get("edits") else {
        return Err(invalid());
    };
    if items.is_empty() {
        return Err(invalid());
    }
    let mut edits = Vec::with_capacity(items.len());
    for item in items {
        let Ok(edit) = serde_json::from_value::<ReplaceEdit>(item.clone()) else {
            return Err(invalid());
        };
        edits.push(edit);
    }
    // `path` 缺失或不是字符串时退化为空串：这里不单独报错，后续 `resolve_to_cwd("")`
    // 会指向 cwd 本身，由 access/read 的文件错误决定最终文案。
    let path = params
        .get("path")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((path, edits))
}

/// `edit` 的 `execute` 实现。
struct EditExecute {
    /// 工具构造时的 cwd；上下文缺失或上下文 cwd 为空时作为解析基准。
    cwd: String,
    /// 读写实现；默认是 [`LocalEditOperations`]。
    operations: Arc<dyn EditOperations>,
    /// 写入门禁使用的执行策略；`None` 时按执行期 cwd 从环境构造。
    execution_policy: Option<Arc<ExecutionPolicy>>,
    /// 读记录表；与 read 工具共享。
    read_state: Arc<ReadStateRegistry>,
}

/// 统一执行策略 + realpath 归一。返回后续读写使用的真实路径。
///
/// `edit` 只修改已存在的文件，因此这里不做「路径是否存在」的判断：不存在的目标继续
/// 走 `access`/`read_file`，保持原来的 ENOENT/EACCES/EISDIR 文案。
fn resolve_edit_target(
    policy: &ExecutionPolicy,
    absolute_path: &Path,
) -> Result<PathBuf, AgentToolError> {
    policy
        .resolve_write_target("edit", absolute_path)
        .map_err(sandbox_denied_error)
}

/// 读后写门禁。只有「已存在的普通文件」需要读记录；目录/不存在路径交给后续操作报错。
fn check_edit_gate(
    read_state: &ReadStateRegistry,
    real_path: &Path,
    display_path: &str,
) -> Result<Option<ReadRecord>, AgentToolError> {
    let existing_regular_file = std::fs::metadata(real_path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);
    if !existing_regular_file {
        return Ok(None);
    }
    match read_state.check_prior_read(real_path) {
        PriorReadStatus::Fresh(record) => Ok(Some(record)),
        PriorReadStatus::Missing => Err(read_before_write_error(real_path)),
        PriorReadStatus::Changed(reason) => Err(multi_edit_error(MultiEditError::FileChanged {
            path: display_path.to_string(),
            reason,
        })),
        PriorReadStatus::Unverifiable(reason) => {
            Err(prior_read_verification_error(real_path, &reason))
        }
        PriorReadStatus::NotText => Err(read_not_text_error(real_path)),
    }
}

/// 多 edit 引擎的错误 → 工具错误（保留稳定 name 便于日志筛选）。
fn multi_edit_error(error: MultiEditError) -> AgentToolError {
    let name = match error.code() {
        "file_changed" => "FileChangedSinceRead",
        "already_applied" => "EditAlreadyApplied",
        _ => "EditError",
    };
    AgentToolError {
        message: error.to_string(),
        name: Some(name.to_string()),
        stack: None,
    }
}

/// 截断读只允许改看到过的行：使用校验阶段得到的真实行区间。
fn check_edit_coverage(
    record: &ReadRecord,
    planned: &crate::index::multi_edit::FileEditPlan,
    path: &Path,
) -> Result<(), AgentToolError> {
    if record.covers_all_lines() {
        return Ok(());
    }
    for edit in &planned.edits {
        if !record.covers(edit.line_range.start, edit.line_range.end) {
            return Err(read_range_not_seen_error(
                path,
                edit.line_range.start,
                edit.line_range.end,
            ));
        }
    }
    Ok(())
}

impl ToolDefinitionExecute for EditExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>
    {
        Box::pin(async move {
            // 校验在队列之外完成：非法参数不会占用队列，也不会产生任何文件系统副作用。
            let (path, edits) = validate_edit_input(&params)?;
            let cwd = effective_cwd(&ctx, &self.cwd);
            let absolute_path = resolve_to_cwd(&path, &cwd);
            let policy = self
                .execution_policy
                .clone()
                .unwrap_or_else(|| Arc::new(ExecutionPolicy::from_env(&cwd)));

            // key 用解析后的绝对路径计算；队列内部再做 realpath 归一，因此不同拼写指向
            // 同一文件时共享一条队列。
            try_with_file_mutation_queue(absolute_path.clone(), || async {
                // 中止是「检查点」语义：与 TS 一样只在每个 await 之后检查，工具不会主动
                // 打断已经发出的读写。
                let throw_if_aborted = || -> Result<(), AgentToolError> {
                    match signal {
                        Some(token) if token.is_cancelled() => Err(aborted_error()),
                        _ => Ok(()),
                    }
                };

                throw_if_aborted()?;

                // 写入门禁的第一层：统一执行策略（realpath/可写根/.git/.kiana/大小写）。
                // 与同一策略下的 bash/PowerShell 结论一致；在锁内执行，避免并行写绕过。
                let real_path = resolve_edit_target(&policy, &absolute_path)?;

                if let Err(error) = self.operations.access(&real_path).await {
                    // 取消与访问错误同时发生时，取消优先（上面刚检查过）。
                    throw_if_aborted()?;
                    return Err(AgentToolError {
                        message: format_edit_access_error(&path, &error),
                        name: None,
                        stack: None,
                    });
                }
                throw_if_aborted()?;

                // 第二层：read-before-write。已存在的普通文件必须有当前读记录。
                let record = check_edit_gate(&self.read_state, &real_path, &path)?;
                throw_if_aborted()?;

                // 读取错误不包装文案，原样透传。
                let buffer = self
                    .operations
                    .read_file(&real_path)
                    .await
                    .map_err(operation_error)?;
                // 读记录检查与本次实际读取之间可能被外部改写：用内容哈希复检一次。
                if let Some(record) = &record
                    && fingerprint_bytes(&buffer) != record.content_hash
                {
                    return Err(multi_edit_error(MultiEditError::FileChanged {
                        path: path.clone(),
                        reason: "content hash changed while the edit was starting".to_string(),
                    }));
                }
                // WHATWG 非 fatal 解码：非法字节变 U+FFFD；BOM 由下一行的 split_bom
                // 单独保留，最后原样写回。
                let raw_content = decode_utf8(&buffer);
                throw_if_aborted()?;

                let split = split_bom(&raw_content);
                let original_ending = detect_line_ending(&split.text);
                let normalized_content = normalize_to_lf(&split.text);
                // 新引擎先校验全部 edit（逐 edit 记录策略与真实行号），任一失败都不返回
                // 可写内容——因此「任一失败该文件零落盘」由类型保证，而不是靠调用顺序。
                let plan = match plan_file_edits(&path, &normalized_content, &edits) {
                    FileEditOutcome::Applied(plan) => plan,
                    FileEditOutcome::Rejected(rejection) => {
                        return Err(AgentToolError {
                            message: rejection.message(),
                            name: Some("EditError".to_string()),
                            stack: None,
                        });
                    }
                };
                // 第三层：截断读只能改看到过的行。
                if let Some(record) = &record {
                    check_edit_coverage(record, &plan, &real_path)?;
                }
                throw_if_aborted()?;

                // 恢复 BOM 与检测到的行尾风格后整文件写回。
                let final_content = format!(
                    "{}{}",
                    split.bom,
                    restore_line_endings(&plan.new_content, original_ending)
                );
                // 写入错误原样透传；写到一半失败时文件可能已被部分覆盖（没有回滚）。
                self.operations
                    .write_file(&real_path, &final_content)
                    .await
                    .map_err(operation_error)?;
                // 写成功后旧读快照立即失效，用写出的字节建立新基线；完整读过的文件
                // 记录覆盖全文件（并行 edit 的第二个调用仍可继续），截断读只清空范围。
                let was_full_read = record.as_ref().is_some_and(ReadRecord::covers_all_lines);
                self.read_state
                    .record_write(&real_path, final_content.as_bytes(), was_full_read);
                // 注意：写入已经完成。这里被取消会返回 Operation aborted，但文件保留新内容。
                throw_if_aborted()?;

                // diff/patch 使用 LF 归一化、去 BOM 的文本，不是写盘的最终字节。
                // `details` 在旧的 diff/patch/firstChangedLine 之后追加策略与逐 edit
                // 产物；额外字段对旧渲染器是前向兼容的（它只按名读取已知字段）。
                let mut details = serde_json::Map::new();
                details.insert("diff".to_string(), JsonValue::String(plan.diff.clone()));
                details.insert("patch".to_string(), JsonValue::String(plan.patch.clone()));
                if let Some(first_changed_line) = plan.first_changed_line {
                    details.insert(
                        "firstChangedLine".to_string(),
                        JsonValue::Number(serde_json::Number::from(first_changed_line)),
                    );
                }
                details.insert(
                    "strategy".to_string(),
                    JsonValue::String(plan.strategy.as_str().to_string()),
                );
                details.insert(
                    "strategyVariant".to_string(),
                    JsonValue::String(plan.strategy.variant_name().to_string()),
                );
                details.insert(
                    "editStrategies".to_string(),
                    JsonValue::Array(
                        plan.edits
                            .iter()
                            .map(|edit| {
                                serde_json::json!({
                                    "editIndex": edit.edit_index,
                                    "strategy": edit.strategy,
                                    "lineRange": edit.line_range,
                                })
                            })
                            .collect(),
                    ),
                );
                details.insert(
                    "strategyEvents".to_string(),
                    serde_json::to_value(
                        plan.edits
                            .iter()
                            .map(|edit| {
                                EditStrategyEvent::new(
                                    edit.edit_index,
                                    edit.strategy,
                                    edit.line_range,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_else(|_| JsonValue::Array(Vec::new())),
                );
                details.insert(
                    "hunks".to_string(),
                    JsonValue::Array(
                        plan.edits
                            .iter()
                            .map(|edit| {
                                serde_json::json!({
                                    "editIndex": edit.edit_index,
                                    "lineRange": edit.line_range,
                                    "hunk": edit.hunk,
                                })
                            })
                            .collect(),
                    ),
                );
                details.insert(
                    "edits".to_string(),
                    JsonValue::Array(
                        plan.edits
                            .iter()
                            .map(|edit| {
                                serde_json::json!({
                                    "editIndex": edit.edit_index,
                                    "status": "applied",
                                    "strategy": edit.strategy,
                                    "lineRange": edit.line_range,
                                    "hunk": edit.hunk,
                                    "snippet": edit.snippet,
                                })
                            })
                            .collect(),
                    ),
                );
                details.insert(
                    "changedLineCount".to_string(),
                    JsonValue::Number(serde_json::Number::from(plan.edits.len() as u64)),
                );

                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        // 计数用 edits.len()；1 也写作 "block(s)"（当前稳定文案，改动需同步测试）。
                        text: format!("Successfully replaced {} block(s) in {path}.", edits.len()),
                        text_signature: None,
                    })],
                    details: JsonValue::Object(details),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
            .await
            .map_err(operation_error)?
        })
    }
}

/// 创建 `edit` 的 [`ToolDefinition`]。
///
/// TS: `createEditToolDefinition(cwd, options?)`。额外设置
/// `renderShell: "self"` 与 `prepareArguments`。
///
/// `cwd` 只在没有真实上下文（或上下文 cwd 为空）时作为路径解析基准；`options.operations`
/// 缺省为 [`LocalEditOperations`]。`RenderShell::SelfRender` 表示调用/结果由
/// `crate::renderers::edit` 自己渲染完整外壳。
pub fn create_edit_tool_definition(
    cwd: impl Into<String>,
    options: Option<EditToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = edit_tool_system_prompt_contribution();
    let renderers: ToolRenderers = edit_renderers();
    ToolDefinition {
        name: "edit".to_string(),
        label: "edit".to_string(),
        description: EDIT_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        prompt_guidelines: Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        parameters: edit_parameters_schema(),
        constrained_sampling: Some(json_schema_prefer()),
        render_shell: Some(RenderShell::SelfRender),
        prepare_arguments: Some(Arc::new(prepare_edit_arguments)),
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(EditExecute {
            cwd: cwd.into(),
            operations: options
                .operations
                .unwrap_or_else(|| Arc::new(LocalEditOperations)),
            execution_policy: options.execution_policy,
            read_state: options.read_state.unwrap_or_else(ReadStateRegistry::global),
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// 创建 `edit` 的 [`AgentTool`]（`wrapToolDefinition` 投影）。
///
/// 注意：`wrapToolDefinition` 会复制 `prepareArguments`，因此 `AgentTool` 上仍有兼容 shim。
/// 这里不传上下文工厂，执行时使用占位上下文；edit 的 `effective_cwd` 在占位上下文下回落到
/// 构造时的 `cwd`，因此 `cwd` 会生效。
pub fn create_edit_tool(cwd: impl Into<String>, options: Option<EditToolOptions>) -> AgentTool {
    wrap_tool_definition(create_edit_tool_definition(cwd, options), None)
}
