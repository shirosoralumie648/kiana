//! 第 10 周记忆子系统的会话层接线。
//!
//! 本模块把 `kiana-memory` 的存储/召回/抽取 API 接进 [`crate::agent_session::Core`]：
//!
//! - `SessionMemory` 同时持有 **project**（`<cwd>/.kiana/memory`）与 **user**
//!   （`~/.kiana/memory`）两个 scope 的 [`MemoryService`]、各自的 load-time
//!   [`MemorySession`] 快照与一个共享的 [`RecallSession`]；
//! - [`SessionMemoryGate`] 把 `WriteRequest` 映射成 `kiana-permissions` 的
//!   `PermissionRequest`；`Ask` 由 memory 工具通过 `ExtensionUiContext::permission_card`
//!   完成交互式审批（default 模式下 memory 写操作因此可用）；没有 UI 时 fail-closed；
//! - [`ProviderMemoryModel`] 用当前会话的 `ModelRuntime` 做三类 side query：
//!   召回选择器（[`MemorySelector`]）、Phase 1 抽取与 Phase 2 整合；
//! - `auto_extract=true` 时由 `Core` 在会话启动后运行 [`ExtractionJob`]（扫描
//!   persisted sessions，lease/watermark 用 [`FileExtractionStore`]）；
//! - [`memory_tool_definition`] 注册模型可见的 `memory` 工具，`target=project|user`
//!   路由到对应 scope，所有写入只经过 `MemoryService`。
//!
//! # 默认关闭
//!
//! 只有显式配置 [`MemoryWiringOptions::enabled`]（或环境变量 `KIANA_MEMORY=1`）时才创建
//! service。`auto_extract` 默认 false。
//!
//! # 锁
//!
//! 所有 `Mutex` 临界区都是短同步段。召回需要 `.await`，因此先克隆 `RecallSession`
//! （它只含一个 `HashSet`），完成后再写回，避免跨 await 持锁。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_agent_core::types::{AgentToolError, AgentToolResult};
use kiana_ai::types::{
    AssistantContentBlock, Context, JsonValue, Message, Model, SimpleStreamOptions, StopReason,
    SystemContent, SystemMessage, SystemRole, TextContent, TextKind, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::ui::{PermissionCardChoice, PermissionCardRequest};
use kiana_memory::{
    ConsolidationOperation, ConsolidationPlan, ExtractionDraft, ExtractionHooks, ExtractionJob,
    ExtractionOutcome, ExtractionStateStore, FileExtractionStore, ForgetReport, Manifest,
    ManifestEntry, MemoryConfig, MemoryEntry, MemoryEntryDraft, MemoryError, MemoryResult,
    MemoryScope, MemorySelector, MemoryService, MemorySession, MemoryType, MemoryWriteGate,
    NoHooks, Phase1Extractor, Phase2Consolidator, RawMemory, RecallCandidate, RecallReport,
    RecallSession, ScopePaths, SessionCandidate, SessionSource, SessionTranscript, SnapshotChange,
    TranscriptMessage, WriteBatch, WriteIntent, WriteOpKind, WriteReport, WriteRequest,
    WriteSource, now_millis, read_audit,
};
use kiana_permissions::{ActionKind, PermissionDecision, PermissionEngine, PermissionRequest};
use serde_json::json;

/// 记忆接线的显式配置。
///
/// `Core::new` 优先使用 `AgentSessionConfig::memory`；没有显式值时才读取环境变量
/// （`KIANA_MEMORY` / `KIANA_MEMORY_AUTO_EXTRACT`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryWiringOptions {
    /// 是否启用记忆子系统。
    pub enabled: bool,
    /// 是否允许后台自动抽取。
    pub auto_extract: bool,
    /// 覆盖 project scope 根目录（测试用；默认 `<cwd>/.kiana/memory`）。
    pub root: Option<PathBuf>,
    /// 是否同时启用 user scope（`~/.kiana/memory`）。生产默认开启；测试可关闭或覆盖。
    pub user_scope: bool,
    /// 覆盖 user scope 根目录（测试用；默认 `~/.kiana/memory`）。
    pub user_root: Option<PathBuf>,
    /// 覆盖 persisted session 目录（测试用；默认按 cwd 推导的 sessions 目录）。
    pub session_dir: Option<PathBuf>,
}

impl MemoryWiringOptions {
    /// 从环境变量构造：`KIANA_MEMORY=1` 启用，`KIANA_MEMORY_AUTO_EXTRACT=1` 打开抽取。
    pub fn from_env() -> Self {
        Self {
            enabled: env_truthy("KIANA_MEMORY"),
            auto_extract: env_truthy("KIANA_MEMORY_AUTO_EXTRACT"),
            root: None,
            user_scope: true,
            user_root: None,
            session_dir: None,
        }
    }

    /// 显式启用（root 使用默认 project scope；测试可再覆盖 `root`）。
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            user_scope: true,
            ..Self::default()
        }
    }
}

fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// model side query
// ---------------------------------------------------------------------------

/// 记忆 side query 的最小模型接口。
///
/// 生产实现是 [`ProviderMemoryModel`]（`ModelRuntime::stream_simple`）；测试可以注入
/// 确定性实现，从而覆盖「模型选择成功 / 返回空 / provider 失败」三条路径而不需要网络。
pub(crate) trait MemoryModelBridge: Send + Sync {
    /// 一次无工具的单轮补全，返回纯文本。
    fn complete<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
        max_tokens: u64,
    ) -> BoxFuture<'a, Result<String, String>>;
}

/// 用会话当前模型执行 side query 的生产桥。
pub(crate) struct ProviderMemoryModel {
    runtime: kiana_config::models::runtime::ModelRuntime,
    model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
}

impl ProviderMemoryModel {
    /// 构造；`model` 在每次请求时读取（会话可能切换模型）。
    pub(crate) fn new(
        runtime: kiana_config::models::runtime::ModelRuntime,
        model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
    ) -> Self {
        Self { runtime, model }
    }
}

impl std::fmt::Debug for ProviderMemoryModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderMemoryModel")
            .finish_non_exhaustive()
    }
}

impl MemoryModelBridge for ProviderMemoryModel {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
        max_tokens: u64,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let Some(model) = (self.model)() else {
                return Err("no active model for memory side query".to_string());
            };
            let context = Context {
                system_prompt: None,
                messages: vec![
                    Message::System(SystemMessage {
                        role: SystemRole::System,
                        content: SystemContent::Text(system.to_string()),
                        sections: None,
                        tools_added: None,
                        tools_removed: None,
                        timestamp: 0,
                    }),
                    Message::User(UserMessage {
                        role: UserRole::User,
                        content: UserContent::Text(user.to_string()),
                        timestamp: 0,
                    }),
                ],
                tools: None,
            };
            let mut options = SimpleStreamOptions::default();
            options.base.max_tokens = Some(max_tokens);
            options.base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
            let stream = self.runtime.stream_simple(&model, &context, options.into());
            let message = stream.result().await.map_err(|error| error.to_string())?;
            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                return Err(message.error_message.unwrap_or_else(|| {
                    format!("memory side query stopped: {:?}", message.stop_reason)
                }));
            }
            let mut text = String::new();
            for block in &message.content {
                if let AssistantContentBlock::Text(block) = block {
                    text.push_str(&block.text);
                }
            }
            Ok(text)
        })
    }
}

/// 抽取 JSON 载荷（容忍模型包了一层 markdown code fence 或解释文字）。
fn parse_json_payload(text: &str) -> Option<JsonValue> {
    let trimmed = text.trim();
    let start = trimmed.find(['{', '['])?;
    let end = trimmed.rfind(['}', ']'])?;
    if end < start {
        return None;
    }
    serde_json::from_str(&trimmed[start..=end]).ok()
}

// ---------------------------------------------------------------------------
// recall selector
// ---------------------------------------------------------------------------

/// 模型 side query 召回选择器。
struct ProviderMemorySelector {
    bridge: Arc<dyn MemoryModelBridge>,
}

#[async_trait]
impl MemorySelector for ProviderMemorySelector {
    async fn select(
        &self,
        query: &str,
        candidates: &[RecallCandidate],
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let mut manifest = String::new();
        for candidate in candidates {
            manifest.push_str(&format!(
                "- id: {}\n  name: {}\n  type: {}\n  description: {}\n",
                candidate.id,
                candidate.name,
                candidate.mtype.as_deref().unwrap_or("unknown"),
                candidate.description
            ));
        }
        let system = "You select which stored memories are relevant to the user's next request. \
                      Return ONLY a JSON array of at most 5 candidate ids. \
                      Return [] when nothing is clearly relevant. Never explain.";
        let user = format!("Request:\n{query}\n\nCandidates:\n{manifest}");
        let response = self
            .bridge
            .complete(system, &user, 200)
            .await
            .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { error.into() })?;
        let Some(value) = parse_json_payload(&response) else {
            return Err("selector response was not valid JSON".into());
        };
        let ids = match value {
            JsonValue::Array(items) => items
                .into_iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>(),
            JsonValue::Object(mut object) => object
                .remove("ids")
                .and_then(|ids| ids.as_array().cloned())
                .map(|items| {
                    items
                        .into_iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// extraction phases
// ---------------------------------------------------------------------------

/// Phase 1：把 transcript 交给模型，解析 raw memories + synopsis。
struct ProviderPhase1Extractor {
    bridge: Arc<dyn MemoryModelBridge>,
}

#[derive(serde::Deserialize)]
struct RawMemoryJson {
    name: String,
    description: String,
    #[serde(rename = "type", default)]
    mtype: Option<String>,
    body: String,
    #[serde(default)]
    source_key: Option<String>,
    #[serde(default)]
    derived_from: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ExtractionJson {
    #[serde(default)]
    memories: Vec<RawMemoryJson>,
    #[serde(default)]
    synopsis: String,
}

fn memory_type_from_str(raw: Option<&str>) -> MemoryType {
    match raw
        .unwrap_or("project")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "user" => MemoryType::User,
        "feedback" => MemoryType::Feedback,
        "reference" => MemoryType::Reference,
        _ => MemoryType::Project,
    }
}

#[async_trait]
impl Phase1Extractor for ProviderPhase1Extractor {
    async fn extract(&self, transcript: &SessionTranscript) -> MemoryResult<ExtractionDraft> {
        let mut body = String::new();
        let messages = transcript.messages.len().saturating_sub(200);
        for message in &transcript.messages[messages..] {
            let mut text = message.text.clone();
            if text.chars().count() > 4_000 {
                text = text.chars().take(4_000).collect();
            }
            body.push_str(&format!("{}: {text}\n", message.role));
        }
        let system = "Extract durable memories from this session transcript. \
                      Return ONLY JSON: {\"memories\":[{\"name\":\"short title\",\
                      \"description\":\"one line\",\"type\":\"user|feedback|project|reference\",\
                      \"body\":\"durable fact, feedback must include Why: and How to apply:\"}],\
                      \"synopsis\":\"one paragraph session summary\"}. \
                      Only include information worth remembering across sessions. \
                      Never include secrets.";
        let user = format!("Session {}:\n{body}", transcript.session_id);
        let response = self
            .bridge
            .complete(system, &user, 1_500)
            .await
            .map_err(MemoryError::Provider)?;
        let payload: ExtractionJson = serde_json::from_value(
            parse_json_payload(&response)
                .ok_or_else(|| MemoryError::Provider("phase 1 returned invalid JSON".into()))?,
        )
        .map_err(|error| MemoryError::Provider(format!("phase 1 JSON shape: {error}")))?;
        let memories = payload
            .memories
            .into_iter()
            .filter(|memory| !memory.name.trim().is_empty() && !memory.body.trim().is_empty())
            .map(|memory| RawMemory {
                name: memory.name,
                description: memory.description,
                mtype: memory_type_from_str(memory.mtype.as_deref()),
                body: memory.body,
                derived_from: memory.derived_from,
                source_key: memory.source_key,
            })
            .collect();
        Ok(ExtractionDraft {
            memories,
            synopsis: payload.synopsis,
        })
    }
}

/// Phase 2：结合现有 manifest 合并 draft。
struct ProviderPhase2Consolidator {
    bridge: Arc<dyn MemoryModelBridge>,
}

#[derive(serde::Deserialize)]
struct ConsolidationOperationJson {
    action: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "type", default)]
    mtype: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    source_key: Option<String>,
    #[serde(default)]
    derived_from: Vec<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct ConsolidationJson {
    #[serde(default)]
    operations: Vec<ConsolidationOperationJson>,
    #[serde(default)]
    synopsis: String,
    #[serde(default)]
    carried_forward: usize,
}

#[async_trait]
impl Phase2Consolidator for ProviderPhase2Consolidator {
    async fn consolidate(
        &self,
        existing: &[ManifestEntry],
        draft: &ExtractionDraft,
    ) -> MemoryResult<ConsolidationPlan> {
        let mut existing_lines = String::new();
        for entry in existing {
            existing_lines.push_str(&format!(
                "- id: {} | name: {} | type: {} | {}\n",
                entry.id,
                entry.name,
                entry.mtype.map(MemoryType::as_str).unwrap_or("unknown"),
                entry.description
            ));
        }
        let mut draft_lines = String::new();
        for memory in &draft.memories {
            draft_lines.push_str(&format!(
                "- name: {} | type: {} | {}\n  {}\n",
                memory.name,
                memory.mtype.as_str(),
                memory.description,
                memory.body
            ));
        }
        let system = "Consolidate extracted memories into the existing memory index. \
                      Return ONLY JSON: {\"operations\":[{\"action\":\"upsert\",\"name\":...,\
                      \"description\":...,\"type\":...,\"body\":...,\"source_key\":...}],\
                      \"synopsis\":\"merged summary\"}. Use \
                      {\"action\":\"remove\",\"id\":\"<existing id>\",\"reason\":\"<why>\"} \
                      for completed/stale entries. Carry forward existing entries unless removed. \
                      Keep each memory durable and non-secret.";
        let user = format!(
            "Existing entries:\n{existing_lines}\nSession synopsis:\n{}\nNew memories:\n{draft_lines}",
            draft.synopsis
        );
        let response = self
            .bridge
            .complete(system, &user, 2_000)
            .await
            .map_err(MemoryError::Provider)?;
        let payload: ConsolidationJson = serde_json::from_value(
            parse_json_payload(&response)
                .ok_or_else(|| MemoryError::Provider("phase 2 returned invalid JSON".into()))?,
        )
        .map_err(|error| MemoryError::Provider(format!("phase 2 JSON shape: {error}")))?;
        let mut operations = Vec::new();
        for operation in payload.operations {
            match operation.action.to_ascii_lowercase().as_str() {
                "upsert" | "add" | "replace" => {
                    let name = operation.name.unwrap_or_default();
                    let body = operation.body.unwrap_or_default();
                    if name.trim().is_empty() || body.trim().is_empty() {
                        continue;
                    }
                    operations.push(ConsolidationOperation::Upsert(MemoryEntryDraft {
                        name,
                        description: operation.description.unwrap_or_default(),
                        mtype: memory_type_from_str(operation.mtype.as_deref()),
                        body,
                        derived_from: operation.derived_from,
                        source_key: operation.source_key,
                    }));
                }
                "remove" | "delete" => {
                    if let Some(id) = operation.id {
                        operations.push(ConsolidationOperation::Remove {
                            id,
                            reason: operation
                                .reason
                                .unwrap_or_else(|| "consolidated as completed".to_string()),
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(ConsolidationPlan {
            operations,
            synopsis: payload.synopsis,
            carried_forward: payload.carried_forward,
        })
    }
}

// ---------------------------------------------------------------------------
// persisted session source
// ---------------------------------------------------------------------------

/// 基于 persisted session 文件（`kiana-session-core` JSONL 格式）的 [`SessionSource`]。
///
/// 文件 IO 全部在 `spawn_blocking` 里执行：会话目录可能有上千个大 transcript，同步解析
/// 会冻结 current-thread runtime。扫描只读取每个文件的第一行（header），只有
/// [`SessionSource::load_session`] 真正需要正文时才完整解析目标文件。
pub(crate) struct SessionFileSource {
    session_dir: PathBuf,
    cwd: Option<String>,
}

/// header 读取上限：会话 header 远小于此；超过的异常文件按无法解析跳过。
const SESSION_HEADER_LIMIT: u64 = 64 * 1024;

impl SessionFileSource {
    pub(crate) fn new(session_dir: PathBuf, cwd: Option<String>) -> Self {
        Self { session_dir, cwd }
    }

    fn clone_for_blocking(&self) -> Self {
        Self {
            session_dir: self.session_dir.clone(),
            cwd: self.cwd.clone(),
        }
    }

    fn read_entries(&self, path: &Path) -> MemoryResult<Vec<kiana_session_core::FileEntry>> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| MemoryError::Source(format!("{}: {error}", path.display())))?;
        Ok(kiana_session_core::parse_session_entries(&text))
    }

    /// 只读取并解析第一行（session header）。
    fn read_header(&self, path: &Path) -> Option<kiana_session_core::SessionHeader> {
        use std::io::{BufRead, BufReader, Read};
        let file = std::fs::File::open(path).ok()?;
        let mut first_line = String::new();
        BufReader::new(file)
            .take(SESSION_HEADER_LIMIT)
            .read_line(&mut first_line)
            .ok()?;
        kiana_session_core::parse_session_entries(&first_line)
            .first()
            .and_then(|entry| entry.as_header())
            .cloned()
    }

    fn session_paths(&self) -> MemoryResult<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.session_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(MemoryError::Source(format!(
                    "{}: {error}",
                    self.session_dir.display()
                )));
            }
        };
        Ok(entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .collect())
    }

    fn scan_sessions_sync(&self) -> MemoryResult<Vec<SessionCandidate>> {
        let mut candidates = Vec::new();
        for path in self.session_paths()? {
            let Some(header) = self.read_header(&path) else {
                continue;
            };
            if header.id.is_empty() {
                continue;
            }
            if let (Some(expected), Some(actual)) = (self.cwd.as_deref(), header.cwd.as_deref())
                && actual != expected
            {
                continue;
            }
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let modified_at = metadata.modified().unwrap_or(UNIX_EPOCH);
            candidates.push(SessionCandidate {
                session_id: header.id.clone(),
                path,
                modified_at,
                modified_ms: system_time_millis(modified_at),
                is_subagent: header.parent_session.is_some(),
            });
        }
        Ok(candidates)
    }

    fn load_session_sync(&self, session_id: &str) -> MemoryResult<SessionTranscript> {
        for path in self.session_paths()? {
            // 先只比对 header，避免为每个无关文件完整解析正文。
            let Some(header) = self.read_header(&path) else {
                continue;
            };
            if header.id != session_id {
                continue;
            }
            let file_entries = self.read_entries(&path)?;
            let mut messages = Vec::new();
            for entry in &file_entries {
                let Some(entry) = entry.as_entry() else {
                    continue;
                };
                let Some(message) = entry.message() else {
                    continue;
                };
                let role = message.role().to_string();
                if !matches!(role.as_str(), "user" | "assistant") {
                    continue;
                }
                if let Some(text) = message_text(message)
                    && !text.trim().is_empty()
                {
                    messages.push(TranscriptMessage { role, text });
                }
            }
            return Ok(SessionTranscript {
                session_id: session_id.to_string(),
                messages,
                is_subagent: header.parent_session.is_some(),
            });
        }
        Err(MemoryError::Source(format!(
            "session `{session_id}` not found in {}",
            self.session_dir.display()
        )))
    }
}

#[async_trait]
impl SessionSource for SessionFileSource {
    async fn scan_sessions(&self) -> MemoryResult<Vec<SessionCandidate>> {
        let source = self.clone_for_blocking();
        tokio::task::spawn_blocking(move || source.scan_sessions_sync())
            .await
            .map_err(|error| MemoryError::Source(format!("session scan task failed: {error}")))?
    }

    async fn load_session(&self, session_id: &str) -> MemoryResult<SessionTranscript> {
        let source = self.clone_for_blocking();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || source.load_session_sync(&session_id))
            .await
            .map_err(|error| MemoryError::Source(format!("session load task failed: {error}")))?
    }
}

fn message_text(message: &kiana_agent_core::types::AgentMessage) -> Option<String> {
    let message = message.as_standard()?;
    match message {
        Message::User(user) => Some(match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        Message::Assistant(assistant) => Some(
            assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

fn system_time_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// scope runtime
// ---------------------------------------------------------------------------

/// 单个 scope 的运行时：service + 冻结快照 + 权限 gate。
struct ScopeMemory {
    service: MemoryService,
    session: Mutex<MemorySession>,
    paths: ScopePaths,
    /// 权限 gate 的动态槽位。`Core::new` 时权限层可能还没启用（CLI
    /// `--permission-mode` 稍后才创建引擎），因此不能把 `None` 固化成 `DenyAllGate`。
    gate: Arc<PermissionGateSlot>,
}

/// 可在运行期注入 [`SessionMemoryGate`] 的 gate 包装。
struct PermissionGateSlot {
    inner: Mutex<Option<Arc<SessionMemoryGate>>>,
}

impl MemoryWriteGate for PermissionGateSlot {
    fn authorize(&self, request: &WriteRequest) -> MemoryResult<()> {
        let gate = lock(&self.inner).clone();
        match gate {
            Some(gate) => gate.authorize(request),
            None => Err(MemoryError::PermissionDenied {
                reason: "memory writes require the permission layer to be enabled".to_string(),
            }),
        }
    }
}

impl ScopeMemory {
    fn new(
        paths: ScopePaths,
        config: &MemoryConfig,
        permissions: Option<Arc<PermissionEngine>>,
        permissions_disabled: bool,
        cwd: &str,
    ) -> MemoryResult<Self> {
        let store = kiana_memory::MemoryStore::open(paths.clone(), config.clone())?;
        let permission_gate = match (permissions, permissions_disabled) {
            (Some(engine), false) => Some(Arc::new(SessionMemoryGate {
                engine,
                cwd: PathBuf::from(cwd),
                paths: paths.clone(),
                approved: Mutex::new(Vec::new()),
            })),
            _ => None,
        };
        let gate = Arc::new(PermissionGateSlot {
            inner: Mutex::new(permission_gate),
        });
        let service = MemoryService::new(store, Arc::clone(&gate) as Arc<dyn MemoryWriteGate>);
        let session = MemorySession::load(service.clone())?;
        Ok(Self {
            service,
            session: Mutex::new(session),
            paths,
            gate,
        })
    }

    /// 当前权限 gate（未启用时 `None`）。
    fn gate(&self) -> Option<Arc<SessionMemoryGate>> {
        lock(&self.gate.inner).clone()
    }

    /// 权限引擎在会话构造之后才创建时（CLI `--permission-mode`），补上门禁。
    fn enable_permission_gate(&self, engine: Arc<PermissionEngine>, cwd: &str) {
        let mut slot = lock(&self.gate.inner);
        if slot.is_none() {
            *slot = Some(Arc::new(SessionMemoryGate {
                engine,
                cwd: PathBuf::from(cwd),
                paths: self.paths.clone(),
                approved: Mutex::new(Vec::new()),
            }));
        }
    }

    fn index(&self) -> Option<String> {
        let session = lock(&self.session);
        let snapshot = session.snapshot();
        if snapshot.entries.is_empty() {
            None
        } else {
            Some(snapshot.prompt_index().to_string())
        }
    }

    fn after_compaction(&self) -> MemoryResult<SnapshotChange> {
        lock(&self.session).after_compaction()
    }
}

/// 一个 scope 的预检查结果。
enum ScopeAuthorization {
    /// 权限层允许（或已经登记过一次精确审批）。
    Allowed,
    /// 需要交互式审批。
    NeedsApproval { reason: String },
    /// 拒绝。
    Denied { reason: String },
}

/// 会话级记忆运行时：project + user 两个 scope + 冻结快照 + 召回/模型状态。
pub(crate) struct SessionMemory {
    project: ScopeMemory,
    user: Option<ScopeMemory>,
    recall: Mutex<RecallSession>,
    config: MemoryConfig,
    cwd: String,
    model: Mutex<Option<Arc<dyn MemoryModelBridge>>>,
    session_source: SessionFileSource,
    last_extraction: Mutex<Option<ExtractionOutcome>>,
}

impl SessionMemory {
    /// 构造并加载 load-time 快照。`enabled == false` 时返回 `Ok(None)`。
    pub(crate) fn new(
        options: &MemoryWiringOptions,
        cwd: &str,
        permissions: Option<Arc<PermissionEngine>>,
        permissions_disabled: bool,
    ) -> MemoryResult<Option<Arc<Self>>> {
        if !options.enabled {
            return Ok(None);
        }
        let paths = match &options.root {
            Some(root) => ScopePaths::at_root(MemoryScope::Project, root.clone()),
            None => ScopePaths::project_at(cwd),
        };
        let config = MemoryConfig {
            auto_extract: options.auto_extract,
            ..MemoryConfig::default()
        };
        let project = ScopeMemory::new(
            paths,
            &config,
            permissions.clone(),
            permissions_disabled,
            cwd,
        )?;
        let user = if options.user_scope {
            let user_paths = match &options.user_root {
                Some(root) => Some(ScopePaths::at_root(MemoryScope::User, root.clone())),
                None => ScopePaths::user_default(),
            };
            match user_paths {
                Some(user_paths) => match ScopeMemory::new(
                    user_paths,
                    &config,
                    permissions.clone(),
                    permissions_disabled,
                    cwd,
                ) {
                    Ok(scope) => Some(scope),
                    Err(error) => {
                        // 显式给了 user_root 说明调用方要求该 scope；失败必须可见。
                        if options.user_root.is_some() {
                            return Err(error);
                        }
                        // 默认 home 不可写时仍保留 project 记忆能力。
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };
        let session_dir = match &options.session_dir {
            Some(dir) => dir.clone(),
            None => session_dir_default(cwd),
        };
        Ok(Some(Arc::new(Self {
            project,
            user,
            recall: Mutex::new(RecallSession::new()),
            config,
            cwd: cwd.to_string(),
            model: Mutex::new(None),
            session_source: SessionFileSource::new(session_dir, Some(cwd.to_string())),
            last_extraction: Mutex::new(None),
        })))
    }

    /// 注入 side query 桥（生产由 `Core::initialize_memory_runtime` 调用）。
    pub(crate) fn set_model_bridge(&self, bridge: Arc<dyn MemoryModelBridge>) {
        *lock(&self.model) = Some(bridge);
    }

    /// 是否已经接上模型 side query。
    pub(crate) fn has_model_bridge(&self) -> bool {
        lock(&self.model).is_some()
    }

    fn scope(&self, scope: MemoryScope) -> MemoryResult<&ScopeMemory> {
        match scope {
            MemoryScope::Project => Ok(&self.project),
            MemoryScope::User => self.user.as_ref().ok_or_else(|| {
                MemoryError::InvalidArgument(
                    "user memory scope is not available in this session".to_string(),
                )
            }),
        }
    }

    /// 当前快照的索引文本；两个 scope 都有条目时分别加标题。
    pub(crate) fn system_prompt_index(&self) -> Option<String> {
        let project = self.project.index();
        let user = self.user.as_ref().and_then(ScopeMemory::index);
        match (user, project) {
            (Some(user), Some(project)) => Some(format!(
                "## User memory\n{user}\n\n## Project memory\n{project}"
            )),
            (Some(user), None) => Some(user),
            (None, Some(project)) => Some(project),
            (None, None) => None,
        }
    }

    /// 压缩提交成功后重新加载两个 scope 的快照。
    pub(crate) fn after_compaction(&self) -> MemoryResult<SnapshotChange> {
        let project = self.project.after_compaction()?;
        let user = match &self.user {
            Some(user) => Some(user.after_compaction()?),
            None => None,
        };
        let changed = project.changed || user.as_ref().is_some_and(|change| change.changed);
        Ok(SnapshotChange {
            previous_revision: project.previous_revision
                + user
                    .as_ref()
                    .map(|change| change.previous_revision)
                    .unwrap_or(0),
            new_revision: project.new_revision
                + user.as_ref().map(|change| change.new_revision).unwrap_or(0),
            changed,
        })
    }

    /// 合并两个 scope 的 manifest：project 优先，同名/同 id 去重。
    fn merged_manifest(&self) -> MemoryResult<Manifest> {
        let project = self.project.service.store().scan_manifest()?;
        let user = match &self.user {
            Some(user) => Some(user.service.store().scan_manifest()?),
            None => None,
        };
        let mut ids: HashSet<String> = HashSet::new();
        let mut names: HashSet<String> = HashSet::new();
        let mut merged = Vec::new();
        let mut scanned_files = project.scanned_files;
        let mut truncated = project.truncated;
        let mut errors = project.errors.clone();
        for entry in project.entries {
            ids.insert(entry.id.clone());
            names.insert(entry.name.clone());
            merged.push(entry);
        }
        if let Some(user) = user {
            scanned_files += user.scanned_files;
            truncated |= user.truncated;
            errors.extend(user.errors);
            for entry in user.entries {
                if ids.contains(&entry.id) || names.contains(&entry.name) {
                    continue;
                }
                ids.insert(entry.id.clone());
                names.insert(entry.name.clone());
                merged.push(entry);
            }
        }
        Ok(Manifest {
            entries: merged,
            scanned_files,
            truncated,
            errors,
        })
    }

    /// 每轮召回。返回一条 `<memory-context>` user 消息正文；零选择时为 `None`。
    pub(crate) async fn recall(&self, query: &str) -> Option<String> {
        self.recall_report(query)
            .await
            .ok()?
            .message
            .map(|message| message.content)
    }

    /// 召回报告（诊断/测试）：生产同样走这里。
    pub(crate) async fn recall_report(&self, query: &str) -> MemoryResult<RecallReport> {
        if query.trim().is_empty() {
            return Ok(empty_recall_report());
        }
        let manifest = self.merged_manifest()?;
        let mut recall = lock(&self.recall).clone();
        let selector = lock(&self.model)
            .clone()
            .map(|bridge| ProviderMemorySelector { bridge });
        let report = recall
            .recall_from_manifest(
                manifest,
                query,
                selector
                    .as_ref()
                    .map(|selector| selector as &dyn MemorySelector),
                &self.config.recall,
            )
            .await?;
        *lock(&self.recall) = recall;
        Ok(report)
    }

    /// 权限引擎在会话构造之后才创建时，为两个 scope 补上写入门禁。
    pub(crate) fn enable_permission_gate(&self, engine: Arc<PermissionEngine>, cwd: &str) {
        self.project
            .enable_permission_gate(Arc::clone(&engine), cwd);
        if let Some(user) = &self.user {
            user.enable_permission_gate(engine, cwd);
        }
    }

    /// 压缩后允许重新召回（`RecallSession` 已注入集合清空）。
    pub(crate) fn reset_recall(&self) {
        lock(&self.recall).reset();
    }

    /// 显式预批准一次写入（供交互式审批在上层完成后的 token 登记）。
    pub(crate) fn approve_write(&self, request: &WriteRequest) {
        if let Ok(scope) = self.scope(request.scope)
            && let Some(gate) = scope.gate()
        {
            lock(&gate.approved).push((request.scope, request.source, request.intents.clone()));
        }
    }

    /// 对一批写入做权限预检查（不消费审批 token）。
    fn authorize(&self, request: &WriteRequest) -> ScopeAuthorization {
        let Ok(scope) = self.scope(request.scope) else {
            return ScopeAuthorization::Denied {
                reason: "requested memory scope is not available".to_string(),
            };
        };
        let Some(gate) = scope.gate() else {
            return ScopeAuthorization::Denied {
                reason: "memory writes require the permission layer to be enabled".to_string(),
            };
        };
        gate.evaluate(request)
    }

    /// 工具/审批层唯一的写入口。
    pub(crate) fn apply(
        &self,
        scope: MemoryScope,
        batch: WriteBatch,
        source: WriteSource,
    ) -> MemoryResult<WriteReport> {
        self.scope(scope)?.service.apply(batch, source)
    }

    /// 计算一次写入的权限请求与预检查结果（工具层在弹审批卡前调用）。
    fn write_authorization(
        &self,
        scope: MemoryScope,
        batch: &WriteBatch,
        source: WriteSource,
    ) -> MemoryResult<(WriteRequest, ScopeAuthorization)> {
        let request = self
            .scope(scope)?
            .service
            .authorization_request(batch, source);
        let authorization = self.authorize(&request);
        Ok((request, authorization))
    }

    /// 遗忘入口（工具调用使用 `WriteSource::User`；审批层可传 `Approval`）。
    pub(crate) fn forget(
        &self,
        scope: MemoryScope,
        id: &str,
        reason: &str,
        source: WriteSource,
    ) -> MemoryResult<ForgetReport> {
        self.scope(scope)?
            .service
            .forget(id, reason, now_millis(), source)
    }

    /// 读取条目（工具 list/replace 解析 id 用）。
    pub(crate) fn list_entries(&self, scope: MemoryScope) -> MemoryResult<Vec<MemoryEntry>> {
        self.scope(scope)?.service.store().list_entries()
    }

    /// 审计记录（诊断/测试）。
    #[allow(dead_code)]
    pub(crate) fn audit(&self, scope: MemoryScope) -> MemoryResult<Vec<kiana_memory::AuditRecord>> {
        read_audit(&self.scope(scope)?.paths.audit_path)
    }

    /// 是否配置了后台自动抽取。
    pub(crate) fn auto_extract(&self) -> bool {
        self.config.auto_extract
    }

    /// 最近一次后台抽取结果（诊断/测试）。
    #[allow(dead_code)]
    pub(crate) fn last_extraction(&self) -> Option<ExtractionOutcome> {
        lock(&self.last_extraction).clone()
    }

    /// 在后台运行一次抽取；调用方通常是 `Core` 的会话启动路径。
    pub(crate) fn spawn_background_extraction(
        self: &Arc<Self>,
        current_session_id: Option<String>,
    ) {
        if !self.auto_extract() || !self.has_model_bridge() {
            return;
        }
        let memory = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                match memory.run_extraction(current_session_id.as_deref()).await {
                    Ok(outcome) => *lock(&memory.last_extraction) = Some(outcome),
                    Err(error) => {
                        *lock(&memory.last_extraction) = Some(ExtractionOutcome {
                            status: kiana_memory::ExtractionStatus::Failed,
                            sessions_seen: 0,
                            sessions_processed: 0,
                            entries_written: 0,
                            bytes_written: 0,
                            skipped: Vec::new(),
                            errors: vec![error.to_string()],
                            revision: None,
                            watermark_ms: 0,
                        });
                    }
                }
            });
        }
    }

    /// 用当前时间运行一次抽取（生产入口）。
    pub(crate) async fn run_extraction(
        &self,
        current_session_id: Option<&str>,
    ) -> MemoryResult<ExtractionOutcome> {
        self.run_extraction_at(now_millis(), current_session_id, &NoHooks)
            .await
    }

    /// 注入时钟/hook 的抽取入口（测试与复用）。
    pub(crate) async fn run_extraction_at(
        &self,
        now_ms: u64,
        current_session_id: Option<&str>,
        hooks: &dyn ExtractionHooks,
    ) -> MemoryResult<ExtractionOutcome> {
        if !self.config.auto_extract {
            return Ok(ExtractionOutcome {
                status: kiana_memory::ExtractionStatus::SkippedDisabled,
                sessions_seen: 0,
                sessions_processed: 0,
                entries_written: 0,
                bytes_written: 0,
                skipped: Vec::new(),
                errors: Vec::new(),
                revision: None,
                watermark_ms: 0,
            });
        }
        let bridge = lock(&self.model).clone().ok_or_else(|| {
            MemoryError::Provider("memory extraction requires a model bridge".to_string())
        })?;
        let state: Arc<dyn ExtractionStateStore> = Arc::new(FileExtractionStore::for_scope(
            &self.project.paths,
            self.config.lock_timeout,
        ));
        let extractor = ProviderPhase1Extractor {
            bridge: Arc::clone(&bridge),
        };
        let consolidator = ProviderPhase2Consolidator { bridge };
        let mut job = ExtractionJob::new(
            &self.project.service,
            state,
            &self.session_source,
            &extractor,
            &consolidator,
            &self.config,
        );
        job.current_session_id = current_session_id.map(str::to_string);
        job.hooks = hooks;
        job.run_at(now_ms).await
    }
}

fn empty_recall_report() -> RecallReport {
    RecallReport {
        strategy: kiana_memory::RecallStrategy::None,
        selected: Vec::new(),
        dropped: Vec::new(),
        fallback_reason: None,
        message: None,
        tokens_estimated: 0,
        manifest_count: 0,
        surfaced_count: 0,
    }
}

/// 默认 persisted session 目录：与 `SessionManager` 的 `<agentDir>/sessions/--<cwd>--`
/// 布局一致。
fn session_dir_default(cwd: &str) -> PathBuf {
    let agent_dir = kiana_core::config::get_agent_dir();
    PathBuf::from(kiana_session_core::get_default_session_dir_path(
        cwd,
        &agent_dir.to_string_lossy(),
    ))
}

impl std::fmt::Debug for SessionMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionMemory")
            .field("cwd", &self.cwd)
            .field("auto_extract", &self.config.auto_extract)
            .field("user_scope", &self.user.is_some())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// permission gate
// ---------------------------------------------------------------------------

/// `MemoryWriteGate` 的权限层实现。
///
/// 每个 `WriteIntent` 单独评估；`Deny` 永远优先。`Ask` 不能在这里弹 UI（trait 是同步
/// 的），因此只有在 `approved` 里登记过精确 intent 集合时才放行，否则返回
/// `PermissionDenied`。memory 工具在调用 `apply` 之前通过 `permission_card` 完成审批并
/// 登记 token；RPC/print/无 UI 路径 fail-closed。
struct SessionMemoryGate {
    engine: Arc<PermissionEngine>,
    cwd: PathBuf,
    paths: ScopePaths,
    approved: Mutex<Vec<(MemoryScope, WriteSource, Vec<WriteIntent>)>>,
}

impl SessionMemoryGate {
    fn evaluate(&self, request: &WriteRequest) -> ScopeAuthorization {
        {
            let approved = lock(&self.approved);
            if approved.iter().any(|entry| {
                entry.0 == request.scope && entry.1 == request.source && entry.2 == request.intents
            }) {
                return ScopeAuthorization::Allowed;
            }
        }
        for intent in &request.intents {
            let path = match &intent.id {
                Some(id) => self.paths.entry_path(id),
                None => self.paths.topics_dir.clone(),
            };
            let permission = PermissionRequest {
                tool_name: "memory".to_string(),
                action: ActionKind::Write,
                cwd: self.cwd.clone(),
                paths: vec![path],
                read_paths: Vec::new(),
                command: None,
                command_tokens: Vec::new(),
                shell: None,
                network: false,
                simple_command: true,
                analysis_incomplete: false,
                args: json!({
                    "scope": request.scope.as_str(),
                    "op": intent.kind.as_str(),
                    "name": intent.name,
                }),
                builtin: false,
                reason: Some("memory write".to_string()),
            };
            match self.engine.evaluate(&permission) {
                PermissionDecision::Allow { .. } => {}
                PermissionDecision::Deny { reason } => {
                    return ScopeAuthorization::Denied { reason };
                }
                PermissionDecision::Ask { reason, .. } => {
                    return ScopeAuthorization::NeedsApproval { reason };
                }
            }
        }
        ScopeAuthorization::Allowed
    }
}

impl MemoryWriteGate for SessionMemoryGate {
    fn authorize(&self, request: &WriteRequest) -> MemoryResult<()> {
        {
            let mut approved = lock(&self.approved);
            if let Some(index) = approved.iter().position(|entry| {
                entry.0 == request.scope && entry.1 == request.source && entry.2 == request.intents
            }) {
                approved.remove(index);
                return Ok(());
            }
        }
        match self.evaluate(request) {
            ScopeAuthorization::Allowed => Ok(()),
            ScopeAuthorization::Denied { reason } => Err(MemoryError::PermissionDenied { reason }),
            ScopeAuthorization::NeedsApproval { reason } => Err(MemoryError::PermissionDenied {
                reason: format!(
                    "{reason} (interactive memory approval is required and was not completed)"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// memory 工具
// ---------------------------------------------------------------------------

/// 构造模型可见的 `memory` 工具。
pub(crate) fn memory_tool_definition(memory: Arc<SessionMemory>) -> ToolDefinition {
    ToolDefinition {
        name: "memory".to_string(),
        label: "Memory".to_string(),
        description: "Manage persistent memory. Project scope is <cwd>/.kiana/memory and user \
                      scope is ~/.kiana/memory. Use add/replace/remove/patch to update memories, \
                      forget to delete, and list to inspect them."
            .to_string(),
        prompt_snippet: Some(
            "Persist durable user feedback and project facts across sessions.".to_string(),
        ),
        prompt_guidelines: Some(vec![
            "Do not store secrets, transient task state, or conversation transcripts in memory."
                .to_string(),
            "Use target=user for cross-project preferences and target=project for repository facts."
                .to_string(),
            "feedback/project memories must include a rule/fact line plus `Why:` and `How to apply:`."
                .to_string(),
        ]),
        parameters: memory_tool_schema(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: Some(true),
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(MemoryToolExecute { memory }),
        render_call: None,
        render_result: None,
    }
}

fn memory_tool_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["add", "replace", "remove", "patch", "forget", "list", "batch"]
            },
            "target": {
                "type": "string",
                "enum": ["project", "user"],
                "default": "project"
            },
            "id": { "type": "string" },
            "name": { "type": "string" },
            "description": { "type": "string" },
            "type": {
                "type": "string",
                "enum": ["user", "feedback", "project", "reference"]
            },
            "body": { "type": "string" },
            "old_text": { "type": "string" },
            "new_text": { "type": "string" },
            "reason": { "type": "string" },
            "operations": {
                "type": "array",
                "items": { "type": "object" }
            }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

struct MemoryToolExecute {
    memory: Arc<SessionMemory>,
}

impl ToolDefinitionExecute for MemoryToolExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            self.run_with_ui(params, ctx)
                .await
                .map(text_result)
                .map_err(|error| AgentToolError {
                    message: error.to_string(),
                    name: None,
                    stack: None,
                })
        })
    }
}

impl MemoryToolExecute {
    async fn run_with_ui(
        &self,
        params: JsonValue,
        ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> MemoryResult<String> {
        let target = params
            .get("target")
            .and_then(JsonValue::as_str)
            .unwrap_or("project");
        let scope = match target {
            "project" => MemoryScope::Project,
            "user" => MemoryScope::User,
            other => {
                return Err(MemoryError::InvalidArgument(format!(
                    "unknown memory target `{other}`"
                )));
            }
        };
        let action = params
            .get("action")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| MemoryError::InvalidArgument("missing `action`".to_string()))?;
        match action {
            "list" => {
                let entries = self.memory.list_entries(scope)?;
                let mut lines = Vec::with_capacity(entries.len());
                for entry in entries {
                    lines.push(format!(
                        "- [{}]({}) — {} ({})",
                        entry.name,
                        entry.id,
                        entry.description,
                        entry.mtype.as_str()
                    ));
                }
                if lines.is_empty() {
                    Ok(format!("No memories stored for target `{target}`."))
                } else {
                    Ok(lines.join("\n"))
                }
            }
            "add" => {
                let draft = self.draft_from(&params)?;
                let report = self
                    .apply_tool(scope, WriteBatch::new().add(draft), WriteSource::Tool, &ctx)
                    .await?;
                Ok(format_report(&report))
            }
            "replace" => {
                let id = self.resolve_id(scope, &params)?;
                let draft = self.draft_from(&params)?;
                let report = self
                    .apply_tool(
                        scope,
                        WriteBatch::new().replace(id, draft),
                        WriteSource::Tool,
                        &ctx,
                    )
                    .await?;
                Ok(format_report(&report))
            }
            "remove" => {
                let id = self.resolve_id(scope, &params)?;
                let report = self
                    .apply_tool(scope, WriteBatch::new().remove(id), WriteSource::Tool, &ctx)
                    .await?;
                Ok(format_report(&report))
            }
            "patch" => {
                let id = self.resolve_id(scope, &params)?;
                let old_text = params
                    .get("old_text")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        MemoryError::InvalidArgument("missing `old_text`".to_string())
                    })?;
                let new_text = params
                    .get("new_text")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        MemoryError::InvalidArgument("missing `new_text`".to_string())
                    })?;
                let report = self
                    .apply_tool(
                        scope,
                        WriteBatch::new().patch(id, old_text, new_text),
                        WriteSource::Tool,
                        &ctx,
                    )
                    .await?;
                Ok(format_report(&report))
            }
            "forget" => {
                let id = self.resolve_id(scope, &params)?;
                let reason = params
                    .get("reason")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("forgotten by tool call");
                let request = WriteRequest {
                    scope,
                    source: WriteSource::User,
                    intents: vec![WriteIntent {
                        kind: WriteOpKind::Remove,
                        id: Some(id.clone()),
                        name: None,
                    }],
                };
                self.approve_if_needed(&request, &ctx).await?;
                let report = self.memory.forget(scope, &id, reason, WriteSource::User)?;
                Ok(format!(
                    "Forgot {} entr{} (revision {}).",
                    report.removed.len(),
                    if report.removed.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    report.revision
                ))
            }
            "batch" => {
                let operations = params
                    .get("operations")
                    .and_then(JsonValue::as_array)
                    .ok_or_else(|| {
                        MemoryError::InvalidArgument("missing `operations` array".to_string())
                    })?;
                let mut batch = WriteBatch::new();
                for operation in operations {
                    let action = operation
                        .get("action")
                        .and_then(JsonValue::as_str)
                        .ok_or_else(|| {
                            MemoryError::InvalidArgument(
                                "each operation needs an `action`".to_string(),
                            )
                        })?;
                    batch = match action {
                        "add" => batch.add(self.draft_from(operation)?),
                        "replace" => {
                            let id = self.resolve_id(scope, operation)?;
                            batch.replace(id, self.draft_from(operation)?)
                        }
                        "remove" => {
                            let id = self.resolve_id(scope, operation)?;
                            batch.remove(id)
                        }
                        "patch" => {
                            let id = self.resolve_id(scope, operation)?;
                            let old_text = operation
                                .get("old_text")
                                .and_then(JsonValue::as_str)
                                .ok_or_else(|| {
                                    MemoryError::InvalidArgument("missing `old_text`".to_string())
                                })?;
                            let new_text = operation
                                .get("new_text")
                                .and_then(JsonValue::as_str)
                                .ok_or_else(|| {
                                    MemoryError::InvalidArgument("missing `new_text`".to_string())
                                })?;
                            batch.patch(id, old_text, new_text)
                        }
                        other => {
                            return Err(MemoryError::InvalidArgument(format!(
                                "unsupported batch action `{other}`"
                            )));
                        }
                    };
                }
                let report = self
                    .apply_tool(scope, batch, WriteSource::Tool, &ctx)
                    .await?;
                Ok(format_report(&report))
            }
            other => Err(MemoryError::InvalidArgument(format!(
                "unsupported memory action `{other}`"
            ))),
        }
    }

    /// 权限预检查 + （必要时）per-tool 审批卡 + 应用。
    async fn apply_tool(
        &self,
        scope: MemoryScope,
        batch: WriteBatch,
        source: WriteSource,
        ctx: &Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> MemoryResult<WriteReport> {
        let (request, _) = self.memory.write_authorization(scope, &batch, source)?;
        self.approve_if_needed(&request, ctx).await?;
        self.memory.apply(scope, batch, source)
    }

    async fn approve_if_needed(
        &self,
        request: &WriteRequest,
        ctx: &Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> MemoryResult<()> {
        match self.memory.authorize(request) {
            ScopeAuthorization::Allowed => Ok(()),
            ScopeAuthorization::Denied { reason } => Err(MemoryError::PermissionDenied { reason }),
            ScopeAuthorization::NeedsApproval { reason } => {
                let card = PermissionCardRequest {
                    tool_name: "memory".to_string(),
                    args: json!({
                        "scope": request.scope.as_str(),
                        "source": request.source.as_str(),
                        "intents": request.intents.iter().map(|intent| json!({
                            "op": intent.kind.as_str(),
                            "id": intent.id,
                            "name": intent.name,
                        })).collect::<Vec<_>>(),
                    }),
                    cwd: self.memory.cwd.clone(),
                    decision_reason: reason,
                    // 一次性批准：gate 的 token 只绑定这一组精确 intent。
                    always_scopes: Vec::new(),
                    dangerous: false,
                };
                let ctx = Arc::clone(ctx);
                let choice = crate::agent_session::run_on_session_thread(async move {
                    ctx.ui().permission_card(card).await
                })
                .await
                .flatten();
                match choice {
                    Some(
                        PermissionCardChoice::AllowOnce | PermissionCardChoice::AllowAlways(_),
                    ) => {
                        self.memory.approve_write(request);
                        Ok(())
                    }
                    Some(PermissionCardChoice::Reject { reason }) => {
                        Err(MemoryError::PermissionDenied {
                            reason: reason
                                .unwrap_or_else(|| "memory write rejected by the user".to_string()),
                        })
                    }
                    None => Err(MemoryError::PermissionDenied {
                        reason: "memory approval was cancelled".to_string(),
                    }),
                }
            }
        }
    }

    fn draft_from(&self, params: &JsonValue) -> MemoryResult<MemoryEntryDraft> {
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| MemoryError::InvalidArgument("missing `name`".to_string()))?;
        let description = params
            .get("description")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| MemoryError::InvalidArgument("missing `description`".to_string()))?;
        let body = params
            .get("body")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| MemoryError::InvalidArgument("missing `body`".to_string()))?;
        let mtype = match params
            .get("type")
            .and_then(JsonValue::as_str)
            .unwrap_or("project")
        {
            "user" => MemoryType::User,
            "feedback" => MemoryType::Feedback,
            "project" => MemoryType::Project,
            "reference" => MemoryType::Reference,
            other => {
                return Err(MemoryError::InvalidArgument(format!(
                    "unknown memory type `{other}`"
                )));
            }
        };
        let draft = MemoryEntryDraft::new(name, description, mtype, body);
        draft.validate()?;
        Ok(draft)
    }

    fn resolve_id(&self, scope: MemoryScope, params: &JsonValue) -> MemoryResult<String> {
        if let Some(id) = params.get("id").and_then(JsonValue::as_str) {
            return Ok(id.to_string());
        }
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                MemoryError::InvalidArgument(
                    "provide `id` or `name` to identify the entry".to_string(),
                )
            })?;
        let entries = self.memory.list_entries(scope)?;
        let matches: Vec<&MemoryEntry> =
            entries.iter().filter(|entry| entry.name == name).collect();
        match matches.as_slice() {
            [entry] => Ok(entry.id.clone()),
            [] => Err(MemoryError::EntryNotFound(name.to_string())),
            _ => Err(MemoryError::InvalidArgument(format!(
                "multiple memories named `{name}`; pass `id` instead"
            ))),
        }
    }
}

fn format_report(report: &WriteReport) -> String {
    format!(
        "Applied {} operation(s). revision={} usage={:.1}%",
        report.applied.len(),
        report.revision,
        report.usage_ratio * 100.0
    )
}

/// 把工具结果包成 `AgentToolResult`（文本块）。
fn text_result(text: impl Into<String>) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        })],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_permissions::{PermissionMode, PermissionPolicy};
    use std::time::Duration;
    use tempfile::tempdir;

    fn options(root: &std::path::Path) -> MemoryWiringOptions {
        MemoryWiringOptions {
            enabled: true,
            auto_extract: false,
            root: Some(root.to_path_buf()),
            user_scope: false,
            user_root: None,
            session_dir: None,
        }
    }

    #[test]
    fn permission_gate_can_be_enabled_after_construction() {
        let temp = tempdir().expect("temp");
        let cwd = temp.path().to_string_lossy().into_owned();
        let paths = ScopePaths::project_at(&cwd);
        let scope =
            ScopeMemory::new(paths, &MemoryConfig::default(), None, false, &cwd).expect("scope");
        assert!(
            scope.gate().is_none(),
            "构造时没有引擎，槽位应为空而不是固化 deny-all"
        );
        scope.enable_permission_gate(bypass_engine(temp.path()), &cwd);
        let gate = scope.gate().expect("gate installed");
        let request = WriteRequest {
            scope: MemoryScope::Project,
            source: WriteSource::Tool,
            intents: vec![WriteIntent {
                kind: WriteOpKind::Add,
                id: None,
                name: Some("rule".to_string()),
            }],
        };
        gate.authorize(&request)
            .expect("bypass engine allows memory writes");
    }

    fn bypass_engine(cwd: &std::path::Path) -> Arc<PermissionEngine> {
        Arc::new(PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::BypassPermissions,
            cwd,
        )))
    }

    fn read_only_engine(cwd: &std::path::Path) -> Arc<PermissionEngine> {
        Arc::new(PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::ReadOnly,
            cwd,
        )))
    }

    fn default_engine(cwd: &std::path::Path) -> Arc<PermissionEngine> {
        Arc::new(PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::Default,
            cwd,
        )))
    }

    struct FixedBridge {
        response: Result<String, String>,
        calls: Mutex<usize>,
    }

    impl MemoryModelBridge for FixedBridge {
        fn complete<'a>(
            &'a self,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: u64,
        ) -> BoxFuture<'a, Result<String, String>> {
            *lock(&self.calls) += 1;
            let response = self.response.clone();
            Box::pin(async move { response })
        }
    }

    #[test]
    fn disabled_options_do_not_create_a_store() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &MemoryWiringOptions::default(),
            dir.path().to_str().unwrap(),
            Some(bypass_engine(dir.path())),
            false,
        )
        .unwrap();
        assert!(memory.is_none());
        assert!(!dir.path().join(".kiana").exists());
    }

    #[test]
    fn empty_index_is_not_injected_and_mid_session_writes_stay_frozen() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &options(dir.path()),
            dir.path().to_str().unwrap(),
            Some(bypass_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");

        assert!(memory.system_prompt_index().is_none());

        memory
            .apply(
                MemoryScope::Project,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "store-preference",
                    "how the user likes commits",
                    MemoryType::User,
                    "Use imperative commit subjects.",
                )),
                WriteSource::Tool,
            )
            .expect("write allowed by bypass engine");

        // mid-session：磁盘已更新，但 load-time 快照不变。
        assert!(memory.system_prompt_index().is_none());
        let change = memory.after_compaction().unwrap();
        assert!(change.changed);
        let index = memory
            .system_prompt_index()
            .expect("index after compaction");
        assert!(index.contains("store-preference"), "index: {index}");
    }

    #[test]
    fn read_only_permissions_deny_memory_writes() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &options(dir.path()),
            dir.path().to_str().unwrap(),
            Some(read_only_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        let error = memory
            .apply(
                MemoryScope::Project,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "nope",
                    "should be denied",
                    MemoryType::User,
                    "body",
                )),
                WriteSource::Tool,
            )
            .expect_err("read-only must deny memory writes");
        assert!(
            matches!(error, MemoryError::PermissionDenied { .. }),
            "unexpected error: {error}"
        );
        assert!(
            memory
                .list_entries(MemoryScope::Project)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_permission_engine_fails_closed() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &options(dir.path()),
            dir.path().to_str().unwrap(),
            None,
            false,
        )
        .unwrap()
        .expect("memory enabled");
        let error = memory
            .apply(
                MemoryScope::Project,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "nope",
                    "should be denied",
                    MemoryType::User,
                    "body",
                )),
                WriteSource::Tool,
            )
            .expect_err("no permission engine must fail closed");
        assert!(matches!(error, MemoryError::PermissionDenied { .. }));
    }

    #[tokio::test]
    async fn recall_uses_the_provider_selector_and_falls_back_on_failure() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &options(dir.path()),
            dir.path().to_str().unwrap(),
            Some(bypass_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        memory
            .apply(
                MemoryScope::Project,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "deployment-rule",
                    "never deploy on fridays",
                    MemoryType::Feedback,
                    "Rule: never deploy on Fridays.\nWhy: weekend coverage is thin.\nHow to apply: block Friday deploys.",
                )),
                WriteSource::Tool,
            )
            .expect("write");
        let entry_id = memory.list_entries(MemoryScope::Project).unwrap()[0]
            .id
            .clone();

        // 模型选择成功：策略必须是 Model，且只注入一条。
        memory.set_model_bridge(Arc::new(FixedBridge {
            response: Ok(format!("[\"{entry_id}\"]")),
            calls: Mutex::new(0),
        }));
        let report = memory.recall_report("deployment friday").await.unwrap();
        assert_eq!(report.strategy, kiana_memory::RecallStrategy::Model);
        assert_eq!(report.selected.len(), 1);
        assert!(report.message.is_some());

        // 模型返回空数组：零选择不注入，也不回退启发式。
        memory.reset_recall();
        memory.set_model_bridge(Arc::new(FixedBridge {
            response: Ok("[]".to_string()),
            calls: Mutex::new(0),
        }));
        let report = memory.recall_report("deployment friday").await.unwrap();
        assert_eq!(report.strategy, kiana_memory::RecallStrategy::None);
        assert!(report.message.is_none());

        // provider 失败：回退关键词启发式，仍然注入。
        memory.reset_recall();
        memory.set_model_bridge(Arc::new(FixedBridge {
            response: Err("provider unavailable".to_string()),
            calls: Mutex::new(0),
        }));
        let report = memory.recall_report("deployment friday").await.unwrap();
        assert_eq!(report.strategy, kiana_memory::RecallStrategy::Heuristic);
        assert!(report.fallback_reason.is_some());
        assert!(report.message.is_some());
    }

    #[test]
    fn user_and_project_scopes_stay_isolated_and_project_wins_dedup() {
        let dir = tempdir().unwrap();
        let mut wiring = options(&dir.path().join("project"));
        wiring.user_scope = true;
        wiring.user_root = Some(dir.path().join("user"));
        let memory = SessionMemory::new(
            &wiring,
            dir.path().to_str().unwrap(),
            Some(bypass_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");

        memory
            .apply(
                MemoryScope::User,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "style",
                    "user style",
                    MemoryType::User,
                    "User scope body: prefer terse answers.",
                )),
                WriteSource::Tool,
            )
            .unwrap();
        memory
            .apply(
                MemoryScope::Project,
                WriteBatch::new().add(MemoryEntryDraft::new(
                    "style",
                    "project style",
                    MemoryType::User,
                    "Project scope body: use the repository formatter.",
                )),
                WriteSource::Tool,
            )
            .unwrap();

        assert_eq!(memory.list_entries(MemoryScope::User).unwrap().len(), 1);
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);
        assert!(
            dir.path().join("user").join("MEMORY.md").exists(),
            "user scope must live under the configured root"
        );

        // 同名条目在召回时去重，project 优先。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = rt
            .block_on(memory.recall_report("style formatter"))
            .unwrap();
        assert_eq!(report.selected.len(), 1, "{report:?}");
        assert!(
            report.selected[0].body.contains("Project scope body"),
            "{report:?}"
        );

        // forget project 后，user 的同名条目可以再次被召回（不再是重复项）。
        let id = memory.list_entries(MemoryScope::Project).unwrap()[0]
            .id
            .clone();
        memory
            .forget(MemoryScope::Project, &id, "rotate", WriteSource::User)
            .unwrap();
        memory.reset_recall();
        let report = rt.block_on(memory.recall_report("style")).unwrap();
        assert_eq!(report.selected.len(), 1, "{report:?}");
        assert!(
            report.selected[0].body.contains("User scope body"),
            "{report:?}"
        );
    }

    #[test]
    fn user_scope_denied_when_not_configured() {
        let dir = tempdir().unwrap();
        let memory = SessionMemory::new(
            &options(dir.path()),
            dir.path().to_str().unwrap(),
            Some(bypass_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        let error = memory
            .apply(
                MemoryScope::User,
                WriteBatch::new().add(MemoryEntryDraft::new("x", "x", MemoryType::User, "x")),
                WriteSource::Tool,
            )
            .expect_err("user scope is disabled");
        assert!(matches!(error, MemoryError::InvalidArgument(_)), "{error}");
    }

    #[test]
    fn default_mode_ask_requires_an_approval_token_before_writing() {
        let dir = tempdir().unwrap();
        let wiring = MemoryWiringOptions {
            enabled: true,
            auto_extract: false,
            // 默认 project 路径是 `<cwd>/.kiana/memory`：受保护的 `.kiana` 子目录，
            // default 模式会 Ask。
            root: None,
            user_scope: false,
            user_root: None,
            session_dir: None,
        };
        let memory = SessionMemory::new(
            &wiring,
            dir.path().to_str().unwrap(),
            Some(default_engine(dir.path())),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        let batch = WriteBatch::new().add(MemoryEntryDraft::new(
            "ask-entry",
            "requires interactive approval",
            MemoryType::User,
            "body",
        ));

        let (request, authorization) = memory
            .write_authorization(MemoryScope::Project, &batch, WriteSource::Tool)
            .unwrap();
        assert!(
            matches!(authorization, ScopeAuthorization::NeedsApproval { .. }),
            "default mode must ask before writing under .kiana"
        );

        // 没有审批 token：ask 不是 allow，写入被拒绝。
        let error = memory
            .apply(MemoryScope::Project, batch.clone(), WriteSource::Tool)
            .expect_err("Ask must not write without approval");
        assert!(
            matches!(error, MemoryError::PermissionDenied { .. }),
            "{error}"
        );
        assert!(
            memory
                .list_entries(MemoryScope::Project)
                .unwrap()
                .is_empty()
        );

        // 审批 token 只对完全相同的 scope/source/intents 生效，且消费一次。
        memory.approve_write(&request);
        memory
            .apply(MemoryScope::Project, batch, WriteSource::Tool)
            .expect("approved write");
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);

        let another = WriteBatch::new().add(MemoryEntryDraft::new(
            "second-entry",
            "another write",
            MemoryType::User,
            "body",
        ));
        let error = memory
            .apply(MemoryScope::Project, another, WriteSource::Tool)
            .expect_err("the approval token was consumed");
        assert!(
            matches!(error, MemoryError::PermissionDenied { .. }),
            "{error}"
        );
    }

    struct PhaseBridge;

    impl MemoryModelBridge for PhaseBridge {
        fn complete<'a>(
            &'a self,
            system: &'a str,
            _user: &'a str,
            _max_tokens: u64,
        ) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                if system.contains("Consolidate") {
                    Ok(r#"{"operations":[{"action":"upsert","name":"deployment policy","description":"release cadence","type":"project","body":"Fact: deploy on Tuesdays.\nWhy: team availability.\nHow to apply: use Tuesday releases.","source_key":"sess-1:0-1"}],"synopsis":"Tuesday releases"}"#.to_string())
                } else {
                    Ok(r#"{"memories":[{"name":"deployment policy","description":"release cadence","type":"project","body":"Fact: deploy on Tuesdays.\nWhy: team availability.\nHow to apply: use Tuesday releases.","source_key":"sess-1:0-1"}],"synopsis":"Tuesday releases"}"#.to_string())
                }
            })
        }
    }

    fn write_session(dir: &std::path::Path, id: &str, parent: Option<&str>, cwd: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mut lines = vec![format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{cwd}\"}}"
        )];
        if let Some(parent) = parent {
            // 重新写 header，带上 parentSession。
            lines[0] = format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{cwd}\",\"parentSession\":\"{parent}\"}}"
            );
        }
        lines.push(format!(
            "{{\"type\":\"message\",\"id\":\"{id}-u\",\"parentId\":null,\"timestamp\":\"2026-01-01T00:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"please remember the release cadence\"}}],\"timestamp\":1}}}}"
        ));
        lines.push(format!(
            "{{\"type\":\"message\",\"id\":\"{id}-a\",\"parentId\":\"{id}-u\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"we deploy on Tuesdays\"}}],\"api\":\"openai-completions\",\"provider\":\"faux\",\"model\":\"faux-1\",\"usage\":{{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0,\"cost\":{{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}}}},\"stopReason\":\"stop\",\"timestamp\":2}}}}"
        ));
        std::fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n") + "\n").unwrap();
    }

    fn extraction_options(
        root: &std::path::Path,
        sessions: &std::path::Path,
    ) -> MemoryWiringOptions {
        MemoryWiringOptions {
            enabled: true,
            auto_extract: true,
            root: Some(root.to_path_buf()),
            user_scope: false,
            user_root: None,
            session_dir: Some(sessions.to_path_buf()),
        }
    }

    #[tokio::test]
    async fn extraction_runs_from_persisted_sessions_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        write_session(&sessions, "sess-1", None, cwd.to_str().unwrap());
        let memory = SessionMemory::new(
            &extraction_options(&dir.path().join("memory"), &sessions),
            cwd.to_str().unwrap(),
            Some(bypass_engine(&cwd)),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        memory.set_model_bridge(Arc::new(PhaseBridge));

        let outcome = memory
            .run_extraction_at(now_millis() + 600_000, None, &NoHooks)
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            kiana_memory::ExtractionStatus::Extracted,
            "{outcome:?}"
        );
        assert_eq!(outcome.sessions_processed, 1);
        assert_eq!(outcome.entries_written, 1);
        assert!(outcome.watermark_ms > 0);
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);

        // watermark 前进后重跑不会重复产物。
        let rerun = memory
            .run_extraction_at(now_millis() + 700_000, None, &NoHooks)
            .await
            .unwrap();
        assert_eq!(
            rerun.status,
            kiana_memory::ExtractionStatus::NoSessions,
            "{rerun:?}"
        );
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn extraction_skips_current_and_subagent_sessions() {
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        write_session(&sessions, "current", None, cwd.to_str().unwrap());
        write_session(
            &sessions,
            "sub",
            Some("/tmp/parent.jsonl"),
            cwd.to_str().unwrap(),
        );
        let memory = SessionMemory::new(
            &extraction_options(&dir.path().join("memory"), &sessions),
            cwd.to_str().unwrap(),
            Some(bypass_engine(&cwd)),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        memory.set_model_bridge(Arc::new(PhaseBridge));
        let outcome = memory
            .run_extraction_at(now_millis() + 600_000, Some("current"), &NoHooks)
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            kiana_memory::ExtractionStatus::NoSessions,
            "{outcome:?}"
        );
        assert!(outcome.skipped.iter().any(|skipped| {
            skipped.session_id == "current"
                && skipped.reason == kiana_memory::SkipReason::CurrentSession
        }));
        assert!(outcome.skipped.iter().any(|skipped| {
            skipped.session_id == "sub" && skipped.reason == kiana_memory::SkipReason::Subagent
        }));
        assert!(
            memory
                .list_entries(MemoryScope::Project)
                .unwrap()
                .is_empty()
        );
    }

    struct SlowPhaseBridge;

    impl MemoryModelBridge for SlowPhaseBridge {
        fn complete<'a>(
            &'a self,
            system: &'a str,
            _user: &'a str,
            _max_tokens: u64,
        ) -> BoxFuture<'a, Result<String, String>> {
            Box::pin(async move {
                // 让第一个 claim 的抽取停在 side query 上，第二个 job 得以在同一窗口
                // 尝试 claim（验证跨进程 lease 的 Busy 分支）。
                tokio::time::sleep(Duration::from_millis(50)).await;
                if system.contains("Consolidate") {
                    Ok(r#"{"operations":[],"synopsis":"busy"}"#.to_string())
                } else {
                    Ok(r#"{"memories":[],"synopsis":"busy"}"#.to_string())
                }
            })
        }
    }

    #[tokio::test]
    async fn concurrent_extractions_claim_exactly_one_lease() {
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        write_session(&sessions, "sess-1", None, cwd.to_str().unwrap());
        let wiring = extraction_options(&dir.path().join("memory"), &sessions);
        let first = SessionMemory::new(
            &wiring,
            cwd.to_str().unwrap(),
            Some(bypass_engine(&cwd)),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        let second = SessionMemory::new(
            &wiring,
            cwd.to_str().unwrap(),
            Some(bypass_engine(&cwd)),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        first.set_model_bridge(Arc::new(SlowPhaseBridge));
        second.set_model_bridge(Arc::new(SlowPhaseBridge));
        let now = now_millis() + 600_000;
        let first_task = {
            let first = Arc::clone(&first);
            tokio::spawn(async move { first.run_extraction_at(now, None, &NoHooks).await })
        };
        // 让第一个任务先拿到 lease 并停在模型调用上。
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second_outcome = second.run_extraction_at(now, None, &NoHooks).await.unwrap();
        assert_eq!(
            second_outcome.status,
            kiana_memory::ExtractionStatus::SkippedLeaseBusy,
            "{second_outcome:?}"
        );
        let first_outcome = first_task.await.unwrap().unwrap();
        assert_eq!(
            first_outcome.status,
            kiana_memory::ExtractionStatus::Extracted,
            "{first_outcome:?}"
        );
    }

    struct CrashOnce {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ExtractionHooks for CrashOnce {
        fn before_commit(&self, _session_id: &str) -> MemoryResult<()> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err(MemoryError::Provider("simulated crash".to_string()))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn crashed_extraction_retries_without_duplicates() {
        let dir = tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        let cwd = dir.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        write_session(&sessions, "sess-1", None, cwd.to_str().unwrap());
        let memory = SessionMemory::new(
            &extraction_options(&dir.path().join("memory"), &sessions),
            cwd.to_str().unwrap(),
            Some(bypass_engine(&cwd)),
            false,
        )
        .unwrap()
        .expect("memory enabled");
        memory.set_model_bridge(Arc::new(PhaseBridge));
        let crash = CrashOnce {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let now = now_millis() + 600_000;
        memory
            .run_extraction_at(now, None, &crash)
            .await
            .unwrap_err();
        // 应用已写入、watermark 未提交、lease 未释放。
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);
        let state =
            FileExtractionStore::for_scope(&memory.project.paths, Duration::from_millis(500));
        assert!(state.load().unwrap().lease.is_some());

        // stale lease 过期后重试：source_key 幂等，不重复写。
        let retry_now = now + memory.config.extraction.lease_ttl_ms + 1;
        let outcome = memory
            .run_extraction_at(retry_now, None, &NoHooks)
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            kiana_memory::ExtractionStatus::Extracted,
            "{outcome:?}"
        );
        assert_eq!(memory.list_entries(MemoryScope::Project).unwrap().len(), 1);
    }
}
