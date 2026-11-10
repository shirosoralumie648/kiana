//! `apply_patch` 工具：用 codex 风格的补丁文本一次性修改多个文件。
//!
//! 设计借鉴 codex-rs/apply-patch（parser / seek_sequence / file_update 的行为契约），
//! 按 Kiana 的类型与运行时重写。与参考实现的关键差异：**先把所有 hunk 在内存里验证并
//! 生成最终内容，全部成功后才进入 `file_mutation_queue` 写盘**；任一 hunk 失败时所有
//! 文件零写入（参考实现是逐文件边验证边写）。
//!
//! # 补丁格式
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/to/new.rs
//! +fn main() {}
//! *** Delete File: path/to/old.rs
//! *** Update File: path/to/existing.rs
//! *** Move to: path/to/renamed.rs
//! @@ optional context (e.g. fn name)
//!  unchanged context
//! -removed
//! +added
//! *** End of File
//! *** End Patch
//! ```
//!
//! - `strict` / `lenient` 两种解析模式；默认 lenient（额外接受 `<<'EOF'` heredoc 包装）；
//! - `@@` 可带一行上下文（函数/类名）用于缩小搜索起点；
//! - `*** End of File` 让当前 chunk 的 `old_lines` 从文件末尾向前锚定；
//! - 空 pattern（只有 `@@` 或 `*** End of File`）是合法 no-op，表示在 EOF 插入；
//! - 空补丁（只有 Begin/End）是成功 no-op，不写任何文件。
//!
//! # 行匹配
//!
//! [`seek_sequence`] 按 `exact → trim_end → trim → Unicode 标点归一化(trim)` 逐级放宽，
//! 与参考实现一致。Unicode 归一化把弯引号、各种破折号、特殊空格折叠成 ASCII，使得
//! 模型写的普通标点可以应用到排版过的源码上。
//!
//! # 应用与原子性
//!
//! 1. 解析补丁（无 IO）；
//! 2. 解析所有目标路径（cwd + 执行策略），去重排序后**按固定顺序**嵌套获取
//!    `file_mutation_queue` 锁（避免并发 apply_patch 死锁）；
//! 3. 在锁内读取所有相关文件的当前内容，把 hunk 依次应用在内存状态上（同一文件的多个
//!    hunk 看到前一个 hunk 的结果），生成 `new_content`；任何一步失败直接返回，磁盘未动；
//! 4. 为每个文件生成 unified diff；
//! 5. 执行写入/删除；若中途失败，按首次触碰顺序反向回滚已写文件并删除新建文件。
//!
//! CRLF 文件在 `SourceFile` 里逐行保留原始行尾；新增行使用文件的首个行尾风格，未修改
//! 的行逐字节保持原样。BOM 也作为首行文本的一部分保留。
//!
//! # 模型路由
//!
//! [`ApplyPatchSettings`] 对应 `tools.apply_patch.models`（camelCase 也接受
//! `tools.applyPatch.models`）。为空时 apply_patch 不暴露；仅当当前模型 id 匹配其中某个
//! 模式（`*` 通配，大小写不敏感）时才暴露，其余模型继续使用 `edit`。index.rs 的
//! `create_*_for_model` 负责替换工具集合。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_agent_core::types::BoxFuture;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{ConstrainedSampling, JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_sandbox::ExecutionPolicy;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::edit_diff::{generate_unified_patch, normalize_to_lf};
use crate::file_mutation_queue::try_with_file_mutation_queue;
use crate::index::streaming_parser::{
    Hunk, ParseError, ParseMode, UpdateFileChunk, parse_patch_with_mode,
};
use crate::path_utils::resolve_to_cwd;
use crate::read::{
    LineRange, ReadStateRegistry, ToolOperationError, effective_cwd, read_file_bytes,
    sandbox_denied_error, write_file_bytes,
};

/// `apply_patch` 工具名（不在 [`crate::index::ToolName`] 的 8 个内置工具里，按模型路由
/// 条件暴露）。
pub const APPLY_PATCH_TOOL_NAME: &str = "apply_patch";

/// 工具描述。
pub const APPLY_PATCH_TOOL_DESCRIPTION: &str = "Apply a patch to one or more files using the \
     *** Begin Patch / *** End Patch format. Supports Add File, Delete File, Update File, Move to, \
     @@ context and *** End of File. All hunks are validated before any file is written; if any \
     hunk fails, no file is modified.";

/// 系统提示中的一行摘要。
pub const APPLY_PATCH_PROMPT_SNIPPET: &str = "Apply multi-file patches with *** Begin Patch / \
     *** End Patch (preferred for models that support it)";

/// 详细格式说明（进入工具描述；也用于 TUI/文档）。
pub const APPLY_PATCH_INSTRUCTIONS: &str = include_str!("prompts/apply_patch_instructions.md");

/// 系统提示 Guidelines。
pub const APPLY_PATCH_PROMPT_GUIDELINES: &[&str] = &[
    "Use apply_patch with a single patch containing *** Begin Patch ... *** End Patch for all file edits in one turn",
    "Paths in the patch are relative to the current working directory unless absolute",
    "Include at least one context line (a line starting with a space) around each change when possible",
    "Use *** End of File when the change is anchored at the end of the file",
    "Do not combine apply_patch with edit for the same change; if a patch fails, re-read the file and regenerate the whole patch",
];

/// 工具参数 schema。
pub fn apply_patch_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["patch"],
        "properties": {
            "patch": {
                "type": "string",
                "description": "Patch text. Must start with '*** Begin Patch' and end with '*** End Patch'. Supports Add/Delete/Update File, Move to, @@ context and *** End of File."
            }
        }
    })
}

/// 兼容不同模型的参数形状。
///
/// - 顶层字符串 → `{ "patch": text }`；
/// - `{ "input": "..." }` → 改名为 `patch`；
/// - `patch` 是字符串数组 → 用 `\n` 连接（部分 provider 会把多行参数编码成数组）。
pub fn prepare_apply_patch_arguments(args: &mut JsonValue) -> Option<JsonValue> {
    match args {
        JsonValue::String(text) => Some(serde_json::json!({ "patch": text })),
        JsonValue::Object(object) => {
            let patch_value = object.get("patch").or_else(|| object.get("input"))?;
            let patch_text = match patch_value {
                JsonValue::String(text) => text.clone(),
                JsonValue::Array(items) if items.iter().all(JsonValue::is_string) => items
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return None,
            };
            if matches!(patch_value, JsonValue::String(_)) && object.contains_key("patch") {
                return None;
            }
            let mut folded = object.clone();
            folded.insert("patch".to_string(), JsonValue::String(patch_text));
            folded.remove("input");
            Some(JsonValue::Object(folded))
        }
        _ => None,
    }
}

/// `tools.apply_patch.models` 配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchSettings {
    /// 允许看到 apply_patch 的模型 id 模式（大小写不敏感，支持 `*` 通配）。
    #[serde(default)]
    pub models: Vec<String>,
}

impl ApplyPatchSettings {
    /// 当前模型是否匹配任一模式。空列表表示永不暴露。
    pub fn is_enabled_for(&self, model: &str) -> bool {
        self.models
            .iter()
            .any(|pattern| model_matches_pattern(pattern, model))
    }

    /// 从配置 JSON 读取。
    ///
    /// 接受 `{ "tools": { "apply_patch": { "models": [...] } } }`、
    /// `{ "tools": { "applyPatch": ... } }` 以及顶层的 `apply_patch`/`applyPatch`。
    pub fn from_config(config: &JsonValue) -> Self {
        let object = config.as_object();
        let value = object
            .and_then(|root| root.get("tools"))
            .and_then(JsonValue::as_object)
            .and_then(|tools| tools.get("apply_patch").or_else(|| tools.get("applyPatch")))
            .or_else(|| {
                object.and_then(|root| root.get("apply_patch").or_else(|| root.get("applyPatch")))
            });
        let Some(value) = value else {
            return Self::default();
        };
        match serde_json::from_value::<Self>(value.clone()) {
            Ok(settings) => settings,
            Err(_) => {
                // 也接受直接给数组 `tools.apply_patch: ["gpt-5*"]`。
                match value {
                    JsonValue::Array(items) => Self {
                        models: items
                            .iter()
                            .filter_map(JsonValue::as_str)
                            .map(ToString::to_string)
                            .collect(),
                    },
                    _ => Self::default(),
                }
            }
        }
    }
}

/// `*` 通配、大小写不敏感的模型匹配。
pub fn model_matches_pattern(pattern: &str, model: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let model = model.trim().to_ascii_lowercase();
    if pattern.is_empty() || model.is_empty() {
        return false;
    }
    if !pattern.contains('*') {
        return pattern == model;
    }
    let pattern = pattern.as_bytes();
    let model = model.as_bytes();
    let (mut pattern_index, mut model_index) = (0usize, 0usize);
    let (mut star, mut star_model_index) = (None, 0usize);
    while model_index < model.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == model[model_index] {
            pattern_index += 1;
            model_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            star_model_index = model_index;
            pattern_index += 1;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_model_index += 1;
            model_index = star_model_index;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// 文件元数据（`apply_patch` 只需要存在性/类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyPatchMetadata {
    /// 路径是否存在。
    pub exists: bool,
    /// 是否是普通文件（目录返回 false）。
    pub is_file: bool,
}

/// `apply_patch` 的文件操作抽象（测试可注入失败）。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ApplyPatchOperations: Send + Sync {
    /// 读取原始字节。
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError>;
    /// 写入原始字节。
    async fn write_file(
        &self,
        absolute_path: &Path,
        contents: &[u8],
    ) -> Result<(), ToolOperationError>;
    /// 删除文件。
    async fn remove_file(&self, absolute_path: &Path) -> Result<(), ToolOperationError>;
    /// 递归创建目录。
    async fn create_dir_all(&self, absolute_path: &Path) -> Result<(), ToolOperationError>;
    /// 查询元数据；不存在返回 `exists=false`，其它 IO 错误原样返回。
    async fn metadata(
        &self,
        absolute_path: &Path,
    ) -> Result<ApplyPatchMetadata, ToolOperationError>;
}

/// 本地文件系统实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalApplyPatchOperations;

#[async_trait]
impl ApplyPatchOperations for LocalApplyPatchOperations {
    async fn read_file(&self, absolute_path: &Path) -> Result<Vec<u8>, ToolOperationError> {
        read_file_bytes(absolute_path).await
    }

    async fn write_file(
        &self,
        absolute_path: &Path,
        contents: &[u8],
    ) -> Result<(), ToolOperationError> {
        write_file_bytes(absolute_path, contents).await
    }

    async fn remove_file(&self, absolute_path: &Path) -> Result<(), ToolOperationError> {
        tokio::fs::remove_file(absolute_path)
            .await
            .map_err(|error| ToolOperationError::from_io("unlink", absolute_path, error))
    }

    async fn create_dir_all(&self, absolute_path: &Path) -> Result<(), ToolOperationError> {
        tokio::fs::create_dir_all(absolute_path)
            .await
            .map_err(|error| ToolOperationError::from_io("mkdir", absolute_path, error))
    }

    async fn metadata(
        &self,
        absolute_path: &Path,
    ) -> Result<ApplyPatchMetadata, ToolOperationError> {
        match tokio::fs::metadata(absolute_path).await {
            Ok(metadata) => Ok(ApplyPatchMetadata {
                exists: true,
                is_file: metadata.is_file(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ApplyPatchMetadata {
                exists: false,
                is_file: false,
            }),
            Err(error) => Err(ToolOperationError::from_io("stat", absolute_path, error)),
        }
    }
}

/// `apply_patch` 选项。
#[derive(Clone, Default)]
pub struct ApplyPatchToolOptions {
    /// 文件操作实现；默认本地文件系统。
    pub operations: Option<Arc<dyn ApplyPatchOperations>>,
    /// 写入门禁使用的执行策略；`None` 时按执行期 cwd 从环境构造。
    pub execution_policy: Option<Arc<ExecutionPolicy>>,
    /// 写成功后的读记录表。
    pub read_state: Option<Arc<ReadStateRegistry>>,
    /// 解析严格度；默认 lenient。
    pub parse_mode: Option<ParseMode>,
}

impl std::fmt::Debug for ApplyPatchToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplyPatchToolOptions")
            .field("operations", &self.operations.is_some())
            .field("execution_policy", &self.execution_policy.is_some())
            .field("read_state", &self.read_state.is_some())
            .field("parse_mode", &self.parse_mode)
            .finish()
    }
}

/// 工具错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyPatchError {
    /// 补丁语法错误。
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// 应用错误（找不到上下文、目标不存在等）。
    #[error("{0}")]
    Apply(String),
    /// 文件操作错误。
    #[error("{0}")]
    Io(String),
}

fn apply_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: Some("ApplyPatchError".to_string()),
        stack: None,
    }
}

struct ApplyPatchExecute {
    cwd: String,
    operations: Arc<dyn ApplyPatchOperations>,
    execution_policy: Option<Arc<ExecutionPolicy>>,
    read_state: Arc<ReadStateRegistry>,
    parse_mode: ParseMode,
}

impl ToolDefinitionExecute for ApplyPatchExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            let input = decode_input(&params)?;
            let hunks = parse_patch_with_mode(&input.patch, self.parse_mode)
                .map_err(|error| apply_error(error.to_string()))?;
            if hunks.is_empty() {
                return Ok(no_op_result());
            }
            let cwd = effective_cwd(&ctx, &self.cwd);
            let policy = self
                .execution_policy
                .clone()
                .unwrap_or_else(|| Arc::new(ExecutionPolicy::from_env(&cwd)));

            // 先解析所有路径：路径错误（sandbox 拒绝等）在加锁/读文件之前返回。
            let mut paths: Vec<PathBuf> = Vec::new();
            for hunk in &hunks {
                match hunk {
                    Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => {
                        paths.push(resolve_patch_path(&policy, &cwd, path)?);
                    }
                    Hunk::UpdateFile {
                        path, move_path, ..
                    } => {
                        paths.push(resolve_patch_path(&policy, &cwd, path)?);
                        if let Some(move_path) = move_path {
                            paths.push(resolve_patch_path(&policy, &cwd, move_path)?);
                        }
                    }
                }
            }
            paths.sort();
            paths.dedup();

            let throw_if_aborted = || -> Result<(), AgentToolError> {
                match signal {
                    Some(token) if token.is_cancelled() => Err(AgentToolError {
                        message: "Operation aborted".to_string(),
                        name: None,
                        stack: None,
                    }),
                    _ => Ok(()),
                }
            };
            throw_if_aborted()?;

            let result = with_locks(paths, || {
                Box::pin(async move { self.apply(&policy, &cwd, &hunks).await })
            })
            .await
            .map_err(|error| apply_error(error.to_string()))?;
            // 写盘已经完成：此时如果用户取消，返回错误会让模型重发同一个 patch（并因
            // 目标已存在/上下文过期失败）。取消只阻止**尚未开始**的写入。
            Ok(result)
        })
    }
}

impl ApplyPatchExecute {
    async fn apply(
        &self,
        policy: &ExecutionPolicy,
        cwd: &str,
        hunks: &[Hunk],
    ) -> Result<AgentToolResult<JsonValue>, ApplyPatchError> {
        let mut state: IndexMap<PathBuf, PlannedEntry> = IndexMap::new();

        for hunk in hunks {
            match hunk {
                Hunk::AddFile { path, contents } => {
                    let absolute = resolve_patch_path(policy, cwd, path)
                        .map_err(|error| ApplyPatchError::Apply(error.message))?;
                    ensure_entry(&mut state, &*self.operations, &absolute).await?;
                    let entry = state.get_mut(&absolute).expect("inserted");
                    // 用 `current` 而不是 `original`：同一 patch 里前面的 A→B move 或
                    // 另一个 Add 可能已经把这个路径“占住”，此时 original 已无意义，
                    // 继续覆盖会静默丢弃先写的内容。
                    if entry.current.is_some() {
                        return Err(ApplyPatchError::Apply(format!(
                            "Add File Error: {path} already exists"
                        )));
                    }
                    entry.current = Some(contents.clone());
                    entry.change = ChangeKind::Add;
                    entry.display_path = path.clone();
                }
                Hunk::DeleteFile { path } => {
                    let absolute = resolve_patch_path(policy, cwd, path)
                        .map_err(|error| ApplyPatchError::Apply(error.message))?;
                    ensure_entry(&mut state, &*self.operations, &absolute).await?;
                    let entry = state.get_mut(&absolute).expect("inserted");
                    if entry.current.is_none() {
                        return Err(ApplyPatchError::Apply(format!(
                            "Delete File Error: {path} does not exist"
                        )));
                    }
                    entry.current = None;
                    entry.change = ChangeKind::Delete;
                    entry.display_path = path.clone();
                }
                Hunk::UpdateFile {
                    path,
                    move_path,
                    chunks,
                } => {
                    let absolute = resolve_patch_path(policy, cwd, path)
                        .map_err(|error| ApplyPatchError::Apply(error.message))?;
                    ensure_entry(&mut state, &*self.operations, &absolute).await?;
                    let original = state[&absolute].current.clone().ok_or_else(|| {
                        ApplyPatchError::Apply(format!("Update File Error: {path} does not exist"))
                    })?;
                    let new_contents = apply_chunks(&original, chunks, path)?;
                    {
                        let entry = state.get_mut(&absolute).expect("present");
                        entry.current = Some(new_contents.clone());
                        entry.change = ChangeKind::Update;
                        entry.display_path = path.clone();
                    }
                    if let Some(move_path) = move_path {
                        let destination = resolve_patch_path(policy, cwd, move_path)
                            .map_err(|error| ApplyPatchError::Apply(error.message))?;
                        ensure_entry(&mut state, &*self.operations, &destination).await?;
                        {
                            let source = state.get_mut(&absolute).expect("present");
                            source.current = None;
                            source.change = ChangeKind::MoveFrom;
                            source.display_path = path.clone();
                        }
                        {
                            let destination_entry = state.get_mut(&destination).expect("inserted");
                            destination_entry.current = Some(new_contents);
                            destination_entry.change = ChangeKind::MoveTo;
                            destination_entry.display_path = move_path.clone();
                        }
                    }
                }
            }
        }

        // 生成 diff / details。
        let mut files: Vec<JsonValue> = Vec::new();
        let mut combined_patch = String::new();
        let mut summary_lines: Vec<String> = Vec::new();
        // Update 条目里补丁真正新增的新文件行：写盘后按这些范围登记“已读”。
        let mut added_ranges: HashMap<PathBuf, Vec<LineRange>> = HashMap::new();
        for (path, entry) in &state {
            let old_text = entry.original.clone().unwrap_or_default();
            let new_text = entry.current.clone().unwrap_or_default();
            if entry.original == entry.current && !matches!(entry.change, ChangeKind::MoveTo) {
                continue;
            }
            let diff = generate_unified_patch(
                &entry.display_path,
                &normalize_to_lf(&old_text),
                &normalize_to_lf(&new_text),
            );
            combined_patch.push_str(&diff);
            if matches!(entry.change, ChangeKind::Update) {
                added_ranges.insert(path.clone(), added_new_line_ranges(&diff));
            }
            let marker = match entry.change {
                ChangeKind::Add => "A",
                ChangeKind::Delete | ChangeKind::MoveFrom => "D",
                ChangeKind::Update => "M",
                ChangeKind::MoveTo => "M",
            };
            let summary_line = match entry.change {
                ChangeKind::MoveTo => {
                    format!("{marker} {} (moved)", entry.display_path)
                }
                _ => format!("{marker} {}", entry.display_path),
            };
            summary_lines.push(summary_line);
            let mut detail = serde_json::Map::new();
            detail.insert(
                "path".to_string(),
                JsonValue::String(entry.display_path.clone()),
            );
            detail.insert(
                "change".to_string(),
                JsonValue::String(entry.change.as_str().to_string()),
            );
            detail.insert("diff".to_string(), JsonValue::String(diff));
            if entry.current.is_some() {
                detail.insert(
                    "newContent".to_string(),
                    JsonValue::String(new_text.clone()),
                );
            }
            if entry.original.is_some() {
                detail.insert(
                    "oldContent".to_string(),
                    JsonValue::String(old_text.clone()),
                );
            }
            let _ = path;
            files.push(JsonValue::Object(detail));
        }

        // 写盘：先写/改，再删除（move 先写目标再删源）。任何失败都回滚。
        let mut touched: IndexMap<PathBuf, Option<String>> = IndexMap::new();
        for (path, entry) in &state {
            if entry.original == entry.current {
                continue;
            }
            let original = entry.original.clone();
            if entry.current.is_some() {
                let contents = entry.current.clone().expect("checked");
                if let Some(parent) = path.parent()
                    && let Err(error) = self.operations.create_dir_all(parent).await
                {
                    // 目录创建失败也要回滚：前面的文件已经写盘，"全有或全无"不能破。
                    let unrestored = rollback(&*self.operations, &touched).await;
                    return Err(ApplyPatchError::Io(rollback_error_message(
                        error.message,
                        &unrestored,
                    )));
                }
                if let Err(error) = self.operations.write_file(path, contents.as_bytes()).await {
                    touched.entry(path.clone()).or_insert(original);
                    let unrestored = rollback(&*self.operations, &touched).await;
                    return Err(ApplyPatchError::Io(rollback_error_message(
                        error.message,
                        &unrestored,
                    )));
                }
                touched.entry(path.clone()).or_insert(original);
            }
        }
        for (path, entry) in &state {
            if entry.original == entry.current {
                continue;
            }
            if entry.current.is_none() && entry.original.is_some() {
                let original = entry.original.clone();
                if let Err(error) = self.operations.remove_file(path).await {
                    touched.entry(path.clone()).or_insert(original);
                    let unrestored = rollback(&*self.operations, &touched).await;
                    return Err(ApplyPatchError::Io(rollback_error_message(
                        error.message,
                        &unrestored,
                    )));
                }
                touched.entry(path.clone()).or_insert(original);
            }
        }

        // 写成功后更新读记录，避免紧接着的 edit 被 read-before-write 拒绝。
        //
        // `apply_patch` 不要求先 read：模型只看到补丁 hunk，而不是整个文件。因此
        // **新建**文件（`original == None`，模型提供了全部内容）算“看过全文件”；
        // Update 条目只登记补丁里真正新增（`+`）的新文件行——这几行是模型亲手写的，
        // 刚 patch 完再 edit 它们不应被拒；补丁没出现过的行仍然要重新 `read`。
        // Move 目标（MoveTo）的内容来自磁盘上的源文件，模型未必读过，保守地不登记。
        for (path, entry) in &state {
            if let Some(contents) = &entry.current
                && entry.original != entry.current
            {
                match entry.change {
                    ChangeKind::Add => {
                        self.read_state
                            .record_write(path, contents.as_bytes(), true);
                    }
                    ChangeKind::Update => {
                        let ranges = added_ranges.get(path).cloned().unwrap_or_default();
                        self.read_state
                            .record_write_ranges(path, contents.as_bytes(), ranges);
                    }
                    ChangeKind::Delete | ChangeKind::MoveFrom | ChangeKind::MoveTo => {
                        self.read_state
                            .record_write(path, contents.as_bytes(), false);
                    }
                }
            }
        }

        let summary = if summary_lines.is_empty() {
            "No files were modified.".to_string()
        } else {
            format!(
                "Success. Updated the following files:\n{}",
                summary_lines.join("\n")
            )
        };
        let mut details = serde_json::Map::new();
        details.insert("files".to_string(), JsonValue::Array(files));
        details.insert("patch".to_string(), JsonValue::String(combined_patch));
        details.insert("summary".to_string(), JsonValue::String(summary.clone()));

        Ok(AgentToolResult {
            content: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: summary,
                text_signature: None,
            })],
            details: JsonValue::Object(details),
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        })
    }
}

/// 从 unified diff 里取出**新增行**在目标文件中的行号范围（1-based）。
///
/// 只认 `+` 行：上下文行虽出现在补丁里，但也可能是被覆盖目标文件的旧内容（例如
/// Move to 一个已存在的文件），把它当成“已读”会放开模型从未见过的内容。纯删除的
/// hunk（`+c,0`）没有新增行，返回空。
fn added_new_line_ranges(diff: &str) -> Vec<LineRange> {
    let mut ranges = Vec::new();
    let mut next_new_line: Option<u64> = None;
    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            next_new_line = header
                .split_whitespace()
                .find_map(|token| token.strip_prefix('+'))
                .and_then(|token| {
                    let (start, count) = token.split_once(',')?;
                    let start: u64 = start.parse().ok()?;
                    let count: u64 = count.parse().ok()?;
                    // `+c,0` 是纯删除：c 指向“第 c 行之后”，没有新增行。
                    Some(if count == 0 { start + 1 } else { start })
                });
            continue;
        }
        let Some(next) = next_new_line.as_mut() else {
            continue;
        };
        if line.starts_with('+') {
            ranges.push(LineRange::new(*next, *next));
            *next += 1;
        } else if line.starts_with(' ') {
            *next += 1;
        }
        // `-` 只消费旧文件行号；`\ No newline at end of file` 不是文件内容。
    }
    ranges
}

struct PlannedEntry {
    /// 第一次触碰时的磁盘内容（用于回滚与 diff）。
    original: Option<String>,
    /// 应用完当前 hunk 之后的计划内容；`None` 表示删除。
    current: Option<String>,
    change: ChangeKind,
    display_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Add,
    Update,
    Delete,
    MoveFrom,
    MoveTo,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Add => "add",
            ChangeKind::Update => "update",
            ChangeKind::Delete => "delete",
            ChangeKind::MoveFrom => "moveFrom",
            ChangeKind::MoveTo => "moveTo",
        }
    }
}

async fn ensure_entry(
    state: &mut IndexMap<PathBuf, PlannedEntry>,
    operations: &dyn ApplyPatchOperations,
    path: &Path,
) -> Result<(), ApplyPatchError> {
    if state.contains_key(path) {
        return Ok(());
    }
    let original = read_optional_text(operations, path).await?;
    state.insert(
        path.to_path_buf(),
        PlannedEntry {
            original,
            current: None,
            change: ChangeKind::Update,
            display_path: path.display().to_string(),
        },
    );
    // 新插入的 entry 的 `current` 由调用方按 hunk 类型设置；这里先保留磁盘内容，便于
    // Update/Delete 直接读取。
    if let Some(entry) = state.get_mut(path)
        && entry.current.is_none()
    {
        entry.current = entry.original.clone();
    }
    Ok(())
}

async fn read_optional_text(
    operations: &dyn ApplyPatchOperations,
    path: &Path,
) -> Result<Option<String>, ApplyPatchError> {
    let metadata = operations
        .metadata(path)
        .await
        .map_err(|error| ApplyPatchError::Io(error.message))?;
    if !metadata.exists {
        return Ok(None);
    }
    if !metadata.is_file {
        return Err(ApplyPatchError::Apply(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let bytes = operations
        .read_file(path)
        .await
        .map_err(|error| ApplyPatchError::Io(error.message))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ApplyPatchError::Apply(format!("{} is not valid UTF-8", path.display())))
}

/// 回滚已写入的文件；返回**无法恢复**的路径。
///
/// 回滚失败不能被吞掉：调用方必须把它拼进错误消息，否则工具会声称「全有或全无」，
/// 而磁盘上可能仍有半写入的内容。
async fn rollback(
    operations: &dyn ApplyPatchOperations,
    touched: &IndexMap<PathBuf, Option<String>>,
) -> Vec<PathBuf> {
    let mut unrestored = Vec::new();
    for (path, original) in touched.iter().rev() {
        let restored = match original {
            Some(contents) => {
                let mut restored = true;
                if let Some(parent) = path.parent()
                    && operations.create_dir_all(parent).await.is_err()
                {
                    restored = false;
                }
                if restored
                    && operations
                        .write_file(path, contents.as_bytes())
                        .await
                        .is_err()
                {
                    restored = false;
                }
                restored
            }
            None => operations.remove_file(path).await.is_ok(),
        };
        if !restored {
            unrestored.push(path.clone());
        }
    }
    unrestored
}

/// 把「原错误 + 回滚失败的文件」合成模型可见消息。
fn rollback_error_message(original: String, unrestored: &[PathBuf]) -> String {
    if unrestored.is_empty() {
        return original;
    }
    let paths = unrestored
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{original}; rollback failed for {} file(s), which may have been modified: {paths}",
        unrestored.len()
    )
}

fn decode_input(params: &JsonValue) -> Result<ApplyPatchInput, AgentToolError> {
    let candidate = match params {
        JsonValue::String(text) => serde_json::json!({ "patch": text }),
        JsonValue::Object(object) => {
            if object.get("patch").is_some_and(JsonValue::is_string) {
                params.clone()
            } else if let Some(JsonValue::String(input)) = object.get("input") {
                serde_json::json!({ "patch": input })
            } else if let Some(JsonValue::Array(items)) = object.get("patch")
                && items.iter().all(JsonValue::is_string)
            {
                serde_json::json!({
                    "patch": items.iter().filter_map(JsonValue::as_str).collect::<Vec<_>>().join("\n")
                })
            } else {
                return Err(apply_error(
                    "apply_patch input is invalid: expected {\"patch\": \"...\"}",
                ));
            }
        }
        _ => {
            return Err(apply_error(
                "apply_patch input is invalid: expected an object with a string 'patch' field",
            ));
        }
    };
    serde_json::from_value(candidate)
        .map_err(|error| apply_error(format!("apply_patch input is invalid: {error}")))
}

/// 工具输入。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchInput {
    /// 补丁文本。
    pub patch: String,
}

fn resolve_patch_path(
    policy: &ExecutionPolicy,
    cwd: &str,
    path: &str,
) -> Result<PathBuf, AgentToolError> {
    let absolute = resolve_to_cwd(path, cwd);
    policy
        .resolve_write_target(APPLY_PATCH_TOOL_NAME, &absolute)
        .map_err(sandbox_denied_error)
}

fn no_op_result() -> AgentToolResult<JsonValue> {
    let summary = "No files were modified.";
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: summary.to_string(),
            text_signature: None,
        })],
        details: serde_json::json!({
            "files": [],
            "patch": "",
            "summary": summary,
        }),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// 嵌套获取所有路径的 mutation queue 锁。
///
/// 路径必须已排序去重（调用方保证），因此并发调用不会以相反顺序加锁而死锁。
fn with_locks<'a, T, F>(paths: Vec<PathBuf>, action: F) -> BoxFuture<'a, Result<T, ApplyPatchError>>
where
    T: Send + 'a,
    F: FnOnce() -> BoxFuture<'a, Result<T, ApplyPatchError>> + Send + 'a,
{
    match paths.split_first() {
        None => action(),
        Some((head, rest)) => {
            let head = head.clone();
            let rest = rest.to_vec();
            Box::pin(async move {
                try_with_file_mutation_queue(head, move || with_locks(rest, action))
                    .await
                    .map_err(|error| ApplyPatchError::Io(error.message))?
            })
        }
    }
}

// ===========================================================================
// hunk 应用（SourceFile + seek_sequence）
// ===========================================================================

/// 文件的逐行表示：保留每行原始行尾。
struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }
}

struct SourceFile {
    lines: Vec<SourceLine>,
    preferred_ending: LineEnding,
}

impl SourceFile {
    /// 解析内容；首个行尾成为新增行的风格，无行尾时默认 LF。
    fn parse(contents: &str) -> Self {
        let mut lines = Vec::new();
        let mut preferred_ending = None;
        let mut line_start = 0usize;
        let mut cursor = 0usize;
        let bytes = contents.as_bytes();
        while cursor < bytes.len() {
            let (ending, ending_len) = match bytes[cursor] {
                b'\r' if bytes.get(cursor + 1) == Some(&b'\n') => (LineEnding::CrLf, 2),
                b'\r' => (LineEnding::Cr, 1),
                b'\n' => (LineEnding::Lf, 1),
                _ => {
                    cursor += 1;
                    continue;
                }
            };
            preferred_ending.get_or_insert(ending);
            lines.push(SourceLine {
                text: contents[line_start..cursor].to_string(),
                ending: Some(ending),
            });
            cursor += ending_len;
            line_start = cursor;
        }
        if line_start < contents.len() {
            lines.push(SourceLine {
                text: contents[line_start..].to_string(),
                ending: None,
            });
        }
        Self {
            lines,
            preferred_ending: preferred_ending.unwrap_or(LineEnding::Lf),
        }
    }

    fn line_texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }

    /// 应用 `(start_index, old_len, new_lines)` 替换；未触及的行保留原行尾。
    fn apply_replacements(&mut self, replacements: &[(usize, usize, Vec<String>)]) {
        let mut source_lines = std::mem::take(&mut self.lines).into_iter();
        let mut new_lines = Vec::new();
        let mut source_index = 0usize;
        for (start_index, old_len, new_segment) in replacements {
            debug_assert!(*start_index >= source_index);
            for line in source_lines.by_ref().take(start_index - source_index) {
                new_lines.push(line);
            }
            for _ in source_lines.by_ref().take(*old_len) {}
            new_lines.extend(new_segment.iter().map(|text| SourceLine {
                text: text.clone(),
                ending: Some(self.preferred_ending),
            }));
            source_index = start_index + old_len;
        }
        new_lines.extend(source_lines);
        self.lines = new_lines;
        // update hunk 历史上总会以换行结尾；同时给被移动到中间的无行尾行补上行尾。
        for line in &mut self.lines {
            line.ending.get_or_insert(self.preferred_ending);
        }
    }

    fn into_contents(self) -> String {
        let mut contents = String::new();
        for line in self.lines {
            contents.push_str(&line.text);
            if let Some(ending) = line.ending {
                contents.push_str(ending.as_str());
            }
        }
        contents
    }
}

/// 把 chunks 应用到文本，返回新内容。
fn apply_chunks(
    contents: &str,
    chunks: &[UpdateFileChunk],
    path: &str,
) -> Result<String, ApplyPatchError> {
    let mut source = SourceFile::parse(contents);
    let lines = source.line_texts();
    let replacements = compute_replacements(&lines, path, chunks)?;
    source.apply_replacements(&replacements);
    Ok(source.into_contents())
}

fn compute_replacements(
    original_lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, ApplyPatchError> {
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            match seek_sequence(
                original_lines,
                std::slice::from_ref(context),
                line_index,
                false,
            ) {
                Some(index) => line_index = index + 1,
                None => {
                    return Err(ApplyPatchError::Apply(format!(
                        "Failed to find context '{context}' in {path}"
                    )));
                }
            }
        }

        if chunk.old_lines.is_empty() {
            replacements.push((original_lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }

        // 补丁末尾的空行代表文件末尾的换行；原始行列表里没有这一项，失败时去掉重试。
        let mut pattern: &[String] = &chunk.old_lines;
        let mut new_slice: &[String] = &chunk.new_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        let Some(start_index) = found else {
            return Err(ApplyPatchError::Apply(format!(
                "Failed to find expected lines in {path}:\n{}",
                chunk.old_lines.join("\n")
            )));
        };

        // 上下文行原样保留（包括行尾），只替换真正变化的区段。
        let mut old_start = 0usize;
        let mut new_start = 0usize;
        for &(old_context, new_context) in &chunk.context_line_indices {
            if old_context >= pattern.len() || new_context >= new_slice.len() {
                break;
            }
            if old_start != old_context || new_start != new_context {
                replacements.push((
                    start_index + old_start,
                    old_context - old_start,
                    new_slice[new_start..new_context].to_vec(),
                ));
            }
            old_start = old_context + 1;
            new_start = new_context + 1;
        }
        if old_start != pattern.len() || new_start != new_slice.len() {
            replacements.push((
                start_index + old_start,
                pattern.len() - old_start,
                new_slice[new_start..].to_vec(),
            ));
        }
        line_index = start_index + pattern.len();
    }

    replacements.sort_by_key(|(index, _, _)| *index);
    Ok(replacements)
}

/// 在 `lines` 中从 `start` 起查找 `pattern`。
///
/// 逐级放宽：精确 → 忽略行尾空白 → 忽略首尾空白 → Unicode 标点归一化。`eof = true`
/// 时先把搜索起点推到「文件末尾能容纳 pattern」的位置。空 pattern 返回 `Some(start)`
/// （no-op）；pattern 比输入长返回 `None`（不 panic）。
pub(crate) fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof && lines.len() >= pattern.len() {
        (lines.len() - pattern.len()).max(start)
    } else {
        start
    };
    let last_start = lines.len() - pattern.len();

    for index in search_start..=last_start {
        if lines[index..index + pattern.len()] == *pattern {
            return Some(index);
        }
    }
    for index in search_start..=last_start {
        if lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(line, expected)| line.trim_end() == expected.trim_end())
        {
            return Some(index);
        }
    }
    for index in search_start..=last_start {
        if lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(line, expected)| line.trim() == expected.trim())
        {
            return Some(index);
        }
    }
    for index in search_start..=last_start {
        if lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(line, expected)| normalize_punctuation(line) == normalize_punctuation(expected))
        {
            return Some(index);
        }
    }
    None
}

fn normalize_punctuation(text: &str) -> String {
    text.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

// ===========================================================================
// 工具定义
// ===========================================================================

/// 创建 `apply_patch` 的 [`ToolDefinition`]。
pub fn create_apply_patch_tool_definition(
    cwd: impl Into<String>,
    options: Option<ApplyPatchToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    ToolDefinition {
        name: APPLY_PATCH_TOOL_NAME.to_string(),
        label: APPLY_PATCH_TOOL_NAME.to_string(),
        description: format!("{APPLY_PATCH_TOOL_DESCRIPTION}\n\n{APPLY_PATCH_INSTRUCTIONS}"),
        prompt_snippet: Some(APPLY_PATCH_PROMPT_SNIPPET.to_string()),
        prompt_guidelines: Some(
            APPLY_PATCH_PROMPT_GUIDELINES
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        parameters: apply_patch_parameters_schema(),
        constrained_sampling: Some(ConstrainedSampling::Config(
            kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                strict: kiana_ai::types::StrictMode::Prefer,
            },
        )),
        render_shell: None,
        prepare_arguments: Some(Arc::new(prepare_apply_patch_arguments)),
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(ApplyPatchExecute {
            cwd: cwd.into(),
            operations: options
                .operations
                .unwrap_or_else(|| Arc::new(LocalApplyPatchOperations)),
            execution_policy: options.execution_policy,
            read_state: options.read_state.unwrap_or_else(ReadStateRegistry::global),
            parse_mode: options.parse_mode.unwrap_or_default(),
        }),
        render_call: None,
        render_result: None,
    }
}

/// 创建 `apply_patch` 的 [`AgentTool`]（`wrap_tool_definition` 投影）。
pub fn create_apply_patch_tool(
    cwd: impl Into<String>,
    options: Option<ApplyPatchToolOptions>,
) -> AgentTool {
    wrap_tool_definition(create_apply_patch_tool_definition(cwd, options), None)
}

/// 解析补丁但不接触文件系统（供 TUI 预览/测试）。
pub fn preview_patch(patch: &str, mode: ParseMode) -> Result<Vec<Hunk>, ParseError> {
    parse_patch_with_mode(patch, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_sequence_prefers_exact_then_trims() {
        let lines = vec!["foo".to_string(), "  bar  ".to_string(), "baz".to_string()];
        assert_eq!(
            seek_sequence(&lines, &["  bar  ".into()], 0, false),
            Some(1)
        );
        assert_eq!(seek_sequence(&lines, &["bar".into()], 0, false), Some(1));
        assert_eq!(seek_sequence(&lines, &["  bar".into()], 0, false), Some(1));
        assert_eq!(seek_sequence(&lines, &["missing".into()], 0, false), None);
        assert_eq!(seek_sequence(&lines, &[], 2, false), Some(2));
    }

    #[test]
    fn seek_sequence_supports_unicode_punctuation() {
        let lines = vec!["const msg = \u{201c}hi\u{201d};".to_string()];
        assert_eq!(
            seek_sequence(&lines, &["const msg = \"hi\";".into()], 0, false),
            Some(0)
        );
    }

    #[test]
    fn seek_sequence_eof_anchors_at_end() {
        let lines = vec!["a".to_string(), "target".to_string(), "target".to_string()];
        assert_eq!(seek_sequence(&lines, &["target".into()], 0, true), Some(2));
    }

    #[test]
    fn apply_chunks_preserves_crlf_untouched_lines() {
        let contents = "first\r\nsecond\r\nthird\r\n";
        let chunks = vec![UpdateFileChunk {
            old_lines: vec!["second".into()],
            new_lines: vec!["SECOND".into()],
            ..Default::default()
        }];
        let updated = apply_chunks(contents, &chunks, "f.txt").unwrap();
        assert_eq!(updated, "first\r\nSECOND\r\nthird\r\n");
    }

    #[test]
    fn model_patterns_support_wildcards() {
        assert!(model_matches_pattern("gpt-5*", "gpt-5.2-codex"));
        assert!(model_matches_pattern("*codex*", "my-codex-model"));
        assert!(!model_matches_pattern("gpt-5*", "claude-sonnet"));
        assert!(model_matches_pattern("Codex", "codex"));
        assert!(!model_matches_pattern("", "codex"));
    }

    #[test]
    fn settings_are_read_from_tools_apply_patch() {
        let config = serde_json::json!({
            "tools": { "apply_patch": { "models": ["gpt-5*", "codex*"] } }
        });
        let settings = ApplyPatchSettings::from_config(&config);
        assert_eq!(settings.models, vec!["gpt-5*", "codex*"]);
        assert!(settings.is_enabled_for("gpt-5.2-codex"));
        assert!(!settings.is_enabled_for("claude-opus"));
        let camel = serde_json::json!({
            "tools": { "applyPatch": { "models": ["o3"] } }
        });
        assert_eq!(ApplyPatchSettings::from_config(&camel).models, vec!["o3"]);
        assert!(ApplyPatchSettings::default().models.is_empty());
    }
}
