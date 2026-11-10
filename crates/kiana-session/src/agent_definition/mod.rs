//! 子代理定义（对应 Claude Code / qwen-code 的 declarative agents）。
//!
//! 一个子代理由一份 Markdown 文件描述：`---` 包围的 frontmatter 是配置，正文是
//! system prompt。本模块负责：
//!
//! 1. 解析 frontmatter（[`frontmatter`]）并转成强类型 [`AgentDefinition`]；
//! 2. 按来源优先级 `builtin < plugin < user < project < policy < flag` 合并同名定义；
//! 3. 单个文件解析失败只记录到 [`AgentDefinitionLoadReport::errors`]，不中止整目录。
//!
//! # 字段与默认值
//!
//! | 字段 | 说明 | 缺省 |
//! |---|---|---|
//! | `description` | 必填，给父代理看的能力说明 | — |
//! | `prompt` | Markdown 正文（JSON 定义可直接给 `prompt`） | 正文 |
//! | `tools` | allowlist；`*` = 继承全部 | 继承父代理 |
//! | `disallowedTools` | denylist；即使 `tools` 设置了也仍然生效 | 空 |
//! | `model` | `inherit`（大小写不敏感）或模型名 | `Inherit` |
//! | `effort` | `low`/`medium`/`high`/`xhigh`/`max` 或正整数 | 无 |
//! | `permissionMode` | 映射到 `kiana_permissions::PermissionMode` | 继承父模式 |
//! | `mcpServers` | 字符串引用或内联配置 | 空 |
//! | `hooks` | 原样保留为 JSON，由 hook 运行器解释 | 无 |
//! | `skills` | 预加载技能名 | 空 |
//! | `maxTurns` | 正整数 | 无 |
//! | `memory` | `user`/`project`/`local` | 无 |
//! | `background` | bool（也接受 `"true"`/`"false"`） | `false` |
//! | `isolation` | `shared`/`worktree`/`cow-clone` | `shared` |
//! | `initialPrompt` | 首次进入子会话时自动提交的文本 | 无 |
//! | `omitClaudeMd` | 不把父会话的 CLAUDE.md 注入子代理 | `false` |
//! | `stages` | 声明式顺序流水线（Phase 2，需 `isolation: shared`） | 无 |
//!
//! 未列出的字段视为错误。这与参考实现「宽松忽略未知键」不同，是工作流 DoD 明确要求
//! 的：「unknown frontmatter 字段报错但不中止目录」。

pub mod frontmatter;

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use kiana_permissions::PermissionMode;
use serde_json::Value as JsonValue;

use crate::subagent::canonical_tool::{CanonicalToolId, ToolPattern, ToolSelection};
use crate::subagent::isolation::IsolationMode;

/// agent 定义来源。顺序即覆盖优先级（后者覆盖前者）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentDefinitionSource {
    /// 内置（代码里硬编码）。
    Builtin,
    /// 插件提供的 agent 目录。
    Plugin,
    /// 用户级：`<agent-dir>/agents`。
    User,
    /// 项目级：`<cwd>/.kiana/agents`。
    Project,
    /// 系统策略目录（Linux 默认 `/etc/kiana/agents`）。
    Policy,
    /// 命令行 `--agents <json>`。
    Flag,
}

impl AgentDefinitionSource {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Plugin => "plugin",
            Self::User => "user",
            Self::Project => "project",
            Self::Policy => "policy",
            Self::Flag => "flag",
        }
    }
}

/// 模型选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentModel {
    /// 继承父会话当前模型。
    Inherit,
    /// 指定模型名。
    Named(String),
}

impl AgentModel {
    /// 解析 `inherit`（大小写不敏感）或模型名。
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("inherit") {
            Self::Inherit
        } else {
            Self::Named(trimmed.to_string())
        }
    }
}

/// effort 档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEffort {
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
    /// 极高。
    XHigh,
    /// 最大。
    Max,
    /// 显式 token 预算（参考实现允许整数）。
    Tokens(u32),
}

impl AgentEffort {
    fn parse(value: &frontmatter::YamlValue) -> Option<Self> {
        match value {
            frontmatter::YamlValue::Int(number) if *number > 0 && *number <= u32::MAX as i64 => {
                Some(Self::Tokens(*number as u32))
            }
            frontmatter::YamlValue::Str(text) => match text.trim().to_ascii_lowercase().as_str() {
                "low" => Some(Self::Low),
                "medium" | "med" => Some(Self::Medium),
                "high" => Some(Self::High),
                "xhigh" | "x-high" => Some(Self::XHigh),
                "max" => Some(Self::Max),
                other => other
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .map(Self::Tokens),
            },
            _ => None,
        }
    }
}

/// 记忆作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMemoryScope {
    /// 用户级记忆。
    User,
    /// 项目级记忆。
    Project,
    /// 本机级记忆。
    Local,
}

impl AgentMemoryScope {
    /// 解析字符串。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

/// MCP server 声明：引用已有 server，或内联一份配置。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMcpServer {
    /// 引用名字（连接由宿主解析）。
    Reference(String),
    /// 内联 `{ name: config }`。
    Inline {
        /// server 名。
        name: String,
        /// server 配置（与 settings 里的 MCP 配置同形）。
        config: JsonValue,
    },
}

/// 标准 workflow artifact。
///
/// 只允许这四个名字：stage 的 `consumes` / `produces` 必须从这里取，runner 用固定
/// JSON Schema 校验 stage 通过 `yield` 提交的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageArtifact {
    /// 实现计划。
    Plan,
    /// 改动文件列表。
    ChangedFiles,
    /// 测试证据。
    TestEvidence,
    /// 评审发现。
    ReviewFindings,
}

impl StageArtifact {
    /// 全部 artifact（声明顺序即序列化顺序）。
    pub const ALL: [Self; 4] = [
        Self::Plan,
        Self::ChangedFiles,
        Self::TestEvidence,
        Self::ReviewFindings,
    ];

    /// 稳定名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::ChangedFiles => "changed_files",
            Self::TestEvidence => "test_evidence",
            Self::ReviewFindings => "review_findings",
        }
    }

    /// 解析名字；未知名字返回 `None`。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "plan" => Some(Self::Plan),
            "changed_files" => Some(Self::ChangedFiles),
            "test_evidence" => Some(Self::TestEvidence),
            "review_findings" => Some(Self::ReviewFindings),
            _ => None,
        }
    }
}

/// 一个 stage 的预算；缺省沿用 Work / 运行时设置。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentStageBudget {
    /// 覆盖 owner 定义的 `maxTurns`。
    pub max_turns: Option<u32>,
    /// 该 stage 的 token 预算。
    pub token_budget: Option<u64>,
    /// 该 stage 的超时（秒），与 Work 级超时取更早者。
    pub timeout_seconds: Option<u64>,
}

/// 声明式 workflow stage。Phase 2 只支持按声明顺序执行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStage {
    /// stage 名（同一 agent 内唯一）。
    pub name: String,
    /// 执行该 stage 的 agent 名；缺省为声明它的 agent（忽略其 `stages`）。
    pub owner: Option<String>,
    /// 消费的 artifact；必须由更早的 stage 产出。
    pub consumes: Vec<StageArtifact>,
    /// 产出的 artifact；至少一个。
    pub produces: Vec<StageArtifact>,
    /// stage 结束时要判定的 acceptance criteria（与 Work 同一词汇）。
    pub exit_criteria: Vec<String>,
    /// 预算。
    pub budget: AgentStageBudget,
}

/// 一份完整的子代理定义。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDefinition {
    /// 唯一名字（Markdown 缺省取文件名）。
    pub name: String,
    /// 稳定 profile 身份（Phase 4 facet；旧文件为 `None`）。
    pub id: Option<String>,
    /// profile 修订号（Phase 4 facet）。
    pub revision: Option<u32>,
    /// profile 种类（Phase 4 facet；当前只解析与透传）。
    pub kind: Option<String>,
    /// 能力引用（Phase 4 facet；当前只解析与透传）。
    pub capabilities: Vec<String>,
    /// 定义文件声明的 schema 版本。含 Phase 4 facet 时写出应使用 2。
    pub schema_version: Option<u32>,
    /// 能力说明（必填）。
    pub description: String,
    /// system prompt。
    pub prompt: String,
    /// allowlist；`None` 表示继承父代理的全部工具。
    pub tools: Option<ToolSelection>,
    /// denylist（始终生效）。
    pub disallowed_tools: Vec<ToolPattern>,
    /// 模型。
    pub model: AgentModel,
    /// effort。
    pub effort: Option<AgentEffort>,
    /// 权限模式（子代理只能比父代理更严）。
    pub permission_mode: Option<PermissionMode>,
    /// MCP servers。
    pub mcp_servers: Vec<AgentMcpServer>,
    /// hooks 配置（原样 JSON）。
    pub hooks: Option<JsonValue>,
    /// 预加载技能名。
    pub skills: Vec<String>,
    /// 最大 agentic turn 数。
    pub max_turns: Option<u32>,
    /// 记忆作用域。
    pub memory: Option<AgentMemoryScope>,
    /// 是否默认后台运行（week11b 的 task runtime 消费）。
    pub background: bool,
    /// 工作区隔离模式。
    pub isolation: IsolationMode,
    /// 首轮自动提交的 prompt。
    pub initial_prompt: Option<String>,
    /// 省略父会话的 CLAUDE.md。
    pub omit_claude_md: bool,
    /// 声明式顺序流水线（Phase 2）。
    pub stages: Option<Vec<AgentStage>>,
    /// 来源。
    pub source: AgentDefinitionSource,
    /// 文件路径（flag/builtin 为 `None`）。
    pub file_path: Option<PathBuf>,
}

impl AgentDefinition {
    /// 写出该定义所需的最低 schemaVersion。
    ///
    /// Phase 4 facet（`id` / `revision` / `kind` / `capabilities`）只在
    /// `schemaVersion >= 2` 的定义里写出；旧定义仍是 1。
    pub fn write_schema_version(&self) -> u32 {
        if self.id.is_some()
            || self.revision.is_some()
            || self.kind.is_some()
            || !self.capabilities.is_empty()
        {
            2
        } else {
            1
        }
    }

    /// 解析后校验工具名（把 `disallowedTools` 规范成 [`ToolPattern`]）。
    ///
    /// 空名字/空描述/空 prompt 在解析阶段已经拒绝；这里只做跨字段归一化：
    /// `tools: []` 表示「没有任何工具」，而不是「继承全部」。
    pub fn is_read_only(&self) -> bool {
        self.permission_mode == Some(PermissionMode::ReadOnly)
    }

    /// 把 `tools` 选择转成显式工具 id 集合（需要父工具表作为全集）。
    pub fn requested_tool_ids(
        &self,
        parent_tools: impl IntoIterator<Item = CanonicalToolId>,
    ) -> Vec<CanonicalToolId> {
        let universe: Vec<CanonicalToolId> = parent_tools.into_iter().collect();
        match &self.tools {
            None | Some(ToolSelection::All) => universe,
            Some(ToolSelection::Tools(patterns)) => universe
                .into_iter()
                .filter(|id| {
                    patterns
                        .iter()
                        .any(|pattern| pattern.matches(id, &id.raw_name()))
                })
                .collect(),
        }
    }

    /// 是否允许某个工具（allowlist + denylist）。
    pub fn allows_tool(&self, id: &CanonicalToolId, raw_name: &str) -> bool {
        if self
            .disallowed_tools
            .iter()
            .any(|pattern| pattern.matches(id, raw_name))
        {
            return false;
        }
        match &self.tools {
            None | Some(ToolSelection::All) => true,
            Some(selection) => selection.allows(id, raw_name),
        }
    }
}

/// 加载错误：单文件失败只进报告，不中止目录扫描。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinitionError {
    /// 来源文件（目录扫描时总是有；flag 为 `None`）。
    pub path: Option<PathBuf>,
    /// 名字（能解析出来时）。
    pub agent: Option<String>,
    /// 错误说明。
    pub message: String,
}

impl AgentDefinitionError {
    /// 构造一个带路径的错误。
    pub fn at(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            agent: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AgentDefinitionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(formatter, "{}: {}", path.display(), self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for AgentDefinitionError {}

/// 某个来源的 agent 目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinitionRoot {
    /// 来源。
    pub source: AgentDefinitionSource,
    /// 目录路径。
    pub dir: PathBuf,
}

impl AgentDefinitionRoot {
    /// 构造。
    pub fn new(source: AgentDefinitionSource, dir: impl Into<PathBuf>) -> Self {
        Self {
            source,
            dir: dir.into(),
        }
    }
}

/// 一次加载的结果。
#[derive(Debug, Clone, Default)]
pub struct AgentDefinitionLoadReport {
    /// 按名字合并后的定义（顺序稳定：首次出现的位置）。
    pub definitions: Vec<AgentDefinition>,
    /// 单文件/单条 flag 错误。
    pub errors: Vec<AgentDefinitionError>,
    /// 扫描过的 `.md` 文件数（含失败）。
    pub scanned_files: usize,
}

impl AgentDefinitionLoadReport {
    /// 按名字查找。
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.definitions
            .iter()
            .find(|definition| definition.name == name)
    }
}

/// agent 定义加载器。
///
/// `load()` 是幂等的纯读取；缓存由调用方决定（参考实现用 chokidar 热更新，本层不做
/// 文件监听，避免把运行时资源生命周期耦合进来）。
#[derive(Debug, Clone)]
pub struct AgentDefinitionLoader {
    builtins: Vec<AgentDefinition>,
    roots: Vec<AgentDefinitionRoot>,
    flag_definitions: Vec<(String, JsonValue)>,
    flag_errors: Vec<AgentDefinitionError>,
    max_file_bytes: usize,
}

impl Default for AgentDefinitionLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentDefinitionLoader {
    /// 空加载器（只有调用方显式添加的来源）。
    pub fn new() -> Self {
        Self {
            builtins: builtin_agent_definitions(),
            roots: Vec::new(),
            flag_definitions: Vec::new(),
            flag_errors: Vec::new(),
            max_file_bytes: 1024 * 1024,
        }
    }

    /// 含默认用户/项目/策略目录的加载器。
    pub fn with_default_roots(cwd: impl AsRef<Path>) -> Self {
        let mut loader = Self::new();
        let cwd = cwd.as_ref();
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::User,
            kiana_core::config::get_agent_dir().join("agents"),
        ));
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Project,
            cwd.join(".kiana").join("agents"),
        ));
        #[cfg(unix)]
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Policy,
            PathBuf::from("/etc/kiana/agents"),
        ));
        loader
    }

    /// 追加一个来源目录。
    pub fn add_root(&mut self, root: AgentDefinitionRoot) {
        self.roots.push(root);
    }

    /// 追加插件目录。
    pub fn add_plugin_dir(&mut self, dir: impl Into<PathBuf>) {
        self.add_root(AgentDefinitionRoot::new(AgentDefinitionSource::Plugin, dir));
    }

    /// 追加 `--agents <json>` 形式的内联定义。
    pub fn add_flag_json(&mut self, value: JsonValue) {
        match value {
            JsonValue::Object(object) => {
                for (name, definition) in object {
                    self.flag_definitions.push((name, definition));
                }
            }
            _ => self.flag_errors.push(AgentDefinitionError {
                path: None,
                agent: Some("--agents".to_string()),
                message: "`--agents` must be an object keyed by agent name".to_string(),
            }),
        }
    }

    /// 覆盖单文件大小上限。
    pub fn with_max_file_bytes(mut self, bytes: usize) -> Self {
        self.max_file_bytes = bytes;
        self
    }

    /// 扫描并合并所有来源。
    pub fn load(&self) -> AgentDefinitionLoadReport {
        let mut report = AgentDefinitionLoadReport::default();
        let mut merged: IndexMap<String, AgentDefinition> = IndexMap::new();
        for definition in &self.builtins {
            merged.insert(definition.name.clone(), definition.clone());
        }

        // 固定优先级顺序：plugin < user < project < policy。roots 的添加顺序不影响
        // 结果，避免调用方无意中改变覆盖语义。
        let mut roots = self.roots.clone();
        roots.sort_by_key(|root| root.source);
        for root in roots {
            if !root.dir.exists() {
                continue;
            }
            let mut entries: Vec<PathBuf> = match std::fs::read_dir(&root.dir) {
                Ok(read_dir) => read_dir
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .collect(),
                Err(error) => {
                    report.errors.push(AgentDefinitionError::at(
                        &root.dir,
                        format!("cannot read agent directory: {error}"),
                    ));
                    continue;
                }
            };
            entries.sort();
            for path in entries {
                let is_markdown = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
                if !is_markdown || !path.is_file() {
                    continue;
                }
                report.scanned_files += 1;
                match self.load_markdown_file(&path, root.source) {
                    Ok(definition) => {
                        merged.insert(definition.name.clone(), definition);
                    }
                    Err(error) => report.errors.push(error),
                }
            }
        }

        for (name, value) in &self.flag_definitions {
            report.scanned_files += 1;
            match parse_json_definition(name, value, AgentDefinitionSource::Flag) {
                Ok(definition) => {
                    merged.insert(definition.name.clone(), definition);
                }
                Err(message) => report.errors.push(AgentDefinitionError {
                    path: None,
                    agent: Some(name.clone()),
                    message,
                }),
            }
        }
        report.errors.extend(self.flag_errors.iter().cloned());
        report.definitions = merged.into_values().collect();
        report
    }

    /// 只加载一个 Markdown 文件（方便测试与热更新）。
    pub fn load_markdown_file(
        &self,
        path: impl AsRef<Path>,
        source: AgentDefinitionSource,
    ) -> Result<AgentDefinition, AgentDefinitionError> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path).map_err(|error| {
            AgentDefinitionError::at(path, format!("cannot read file metadata: {error}"))
        })?;
        if metadata.len() as usize > self.max_file_bytes {
            return Err(AgentDefinitionError::at(
                path,
                format!(
                    "agent file is {} bytes, exceeding the {} byte limit",
                    metadata.len(),
                    self.max_file_bytes
                ),
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|error| {
            AgentDefinitionError::at(path, format!("cannot read file: {error}"))
        })?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        parse_markdown_definition(&text, &stem, source, Some(path)).map_err(|message| {
            AgentDefinitionError {
                path: Some(path.to_path_buf()),
                agent: Some(stem),
                message,
            }
        })
    }
}

/// 解析 Markdown 定义。`fallback_name` 是文件名 stem。
pub fn parse_markdown_definition(
    text: &str,
    fallback_name: &str,
    source: AgentDefinitionSource,
    file_path: Option<&Path>,
) -> Result<AgentDefinition, String> {
    let document = frontmatter::parse_document(text).map_err(|error| error.to_string())?;
    let name = match document.get("name") {
        Some(frontmatter::YamlValue::Str(name)) if !name.trim().is_empty() => {
            name.trim().to_string()
        }
        Some(frontmatter::YamlValue::Str(_)) | Some(_) => {
            return Err("field `name` must be a non-empty string".to_string());
        }
        None => fallback_name.to_string(),
    };
    if name.is_empty() {
        return Err("agent name cannot be empty".to_string());
    }
    let mut builder = DefinitionBuilder::new(name, source, file_path.map(Path::to_path_buf));
    builder.prompt = document.body;
    builder.apply_fields(&document.fields)?;
    builder.finish()
}

/// 解析 JSON 定义（`--agents` 或 settings 的 `agents` 对象）。
pub fn parse_json_definition(
    name: &str,
    value: &JsonValue,
    source: AgentDefinitionSource,
) -> Result<AgentDefinition, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "agent definition must be an object".to_string())?;
    let mut fields = Vec::new();
    for (key, value) in object {
        let yaml = json_to_yaml(value)?;
        fields.push((key.clone(), yaml));
    }
    let mut builder = DefinitionBuilder::new(name.to_string(), source, None);
    if let Some(value) = object.get("prompt").and_then(JsonValue::as_str) {
        builder.prompt = value.trim().to_string();
    }
    builder.apply_fields(&fields)?;
    builder.finish()
}

fn json_to_yaml(value: &JsonValue) -> Result<frontmatter::YamlValue, String> {
    Ok(match value {
        JsonValue::Null => frontmatter::YamlValue::Null,
        JsonValue::Bool(value) => frontmatter::YamlValue::Bool(*value),
        JsonValue::Number(number) => {
            if let Some(integer) = number.as_i64() {
                frontmatter::YamlValue::Int(integer)
            } else {
                frontmatter::YamlValue::Float(number.as_f64().unwrap_or_default())
            }
        }
        JsonValue::String(value) => frontmatter::YamlValue::Str(value.clone()),
        JsonValue::Array(items) => frontmatter::YamlValue::Seq(
            items
                .iter()
                .map(json_to_yaml)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        JsonValue::Object(object) => {
            let mut entries = Vec::new();
            for (key, value) in object {
                entries.push((key.clone(), json_to_yaml(value)?));
            }
            frontmatter::YamlValue::Map(entries)
        }
    })
}

struct DefinitionBuilder {
    name: String,
    id: Option<String>,
    revision: Option<u32>,
    kind: Option<String>,
    capabilities: Vec<String>,
    schema_version: Option<u32>,
    description: Option<String>,
    prompt: String,
    tools: Option<ToolSelection>,
    disallowed_tools: Vec<ToolPattern>,
    model: AgentModel,
    effort: Option<AgentEffort>,
    permission_mode: Option<PermissionMode>,
    mcp_servers: Vec<AgentMcpServer>,
    hooks: Option<JsonValue>,
    skills: Vec<String>,
    max_turns: Option<u32>,
    memory: Option<AgentMemoryScope>,
    background: bool,
    isolation: IsolationMode,
    initial_prompt: Option<String>,
    omit_claude_md: bool,
    stages: Option<Vec<AgentStage>>,
    source: AgentDefinitionSource,
    file_path: Option<PathBuf>,
}

impl DefinitionBuilder {
    fn new(name: String, source: AgentDefinitionSource, file_path: Option<PathBuf>) -> Self {
        Self {
            name,
            id: None,
            revision: None,
            kind: None,
            capabilities: Vec::new(),
            schema_version: None,
            description: None,
            prompt: String::new(),
            tools: None,
            disallowed_tools: Vec::new(),
            model: AgentModel::Inherit,
            effort: None,
            permission_mode: None,
            mcp_servers: Vec::new(),
            hooks: None,
            skills: Vec::new(),
            max_turns: None,
            memory: None,
            background: false,
            isolation: IsolationMode::Shared,
            initial_prompt: None,
            omit_claude_md: false,
            stages: None,
            source,
            file_path,
        }
    }

    fn apply_fields(&mut self, fields: &[(String, frontmatter::YamlValue)]) -> Result<(), String> {
        for (key, value) in fields {
            match key.as_str() {
                "name" => {
                    // 已由 parse_markdown_definition 消费；JSON 路径也允许显式覆盖。
                    if let frontmatter::YamlValue::Str(raw) = value
                        && !raw.trim().is_empty()
                    {
                        self.name = raw.trim().to_string();
                    }
                }
                "id" => {
                    let raw = yaml_string(value, "id")?;
                    if raw.trim().is_empty() {
                        return Err("field `id` cannot be empty".to_string());
                    }
                    self.id = Some(raw.trim().to_string());
                }
                "revision" => {
                    self.revision = Some(parse_positive_u32(value, "revision")?);
                }
                "kind" => {
                    let raw = yaml_string(value, "kind")?;
                    if raw.trim().is_empty() {
                        return Err("field `kind` cannot be empty".to_string());
                    }
                    self.kind = Some(raw.trim().to_string());
                }
                "capabilities" => {
                    let capabilities = parse_string_list(value, "capabilities")?;
                    let mut seen = std::collections::HashSet::new();
                    for capability in &capabilities {
                        if !seen.insert(capability.clone()) {
                            return Err(format!("field `capabilities` repeats `{capability}`"));
                        }
                    }
                    self.capabilities = capabilities;
                }
                "schemaVersion" => {
                    self.schema_version = Some(parse_positive_u32(value, "schemaVersion")?);
                }
                "description" => {
                    self.description = Some(yaml_string(value, "description")?);
                }
                "prompt" => {
                    // Markdown 正文优先；JSON 定义没有正文，使用该字段。
                    if self.prompt.is_empty() {
                        self.prompt = yaml_string(value, "prompt")?;
                    }
                }
                "tools" => {
                    self.tools = Some(parse_tool_selection(value)?);
                }
                "disallowedTools" => {
                    self.disallowed_tools = parse_tool_list(value)?;
                }
                "model" => {
                    let raw = yaml_string(value, "model")?;
                    if raw.trim().is_empty() {
                        return Err("field `model` cannot be empty".to_string());
                    }
                    self.model = AgentModel::parse(&raw);
                }
                "effort" => {
                    self.effort = Some(AgentEffort::parse(value).ok_or_else(|| {
                        "field `effort` must be low/medium/high/xhigh/max or a positive integer"
                            .to_string()
                    })?);
                }
                "permissionMode" => {
                    let raw = yaml_string(value, "permissionMode")?;
                    self.permission_mode = Some(parse_permission_mode(&raw)?);
                }
                "mcpServers" => {
                    self.mcp_servers = parse_mcp_servers(value)?;
                }
                "hooks" => {
                    self.hooks = Some(value.to_json());
                }
                "skills" => {
                    self.skills = parse_string_list(value, "skills")?;
                }
                "maxTurns" => {
                    self.max_turns = Some(parse_positive_u32(value, "maxTurns")?);
                }
                "memory" => {
                    let raw = yaml_string(value, "memory")?;
                    self.memory = Some(AgentMemoryScope::parse(&raw).ok_or_else(|| {
                        "field `memory` must be one of user/project/local".to_string()
                    })?);
                }
                "background" => {
                    self.background = parse_bool(value, "background")?;
                }
                "isolation" => {
                    let raw = yaml_string(value, "isolation")?;
                    self.isolation = IsolationMode::parse(&raw).ok_or_else(|| {
                        "field `isolation` must be shared/worktree/cow-clone".to_string()
                    })?;
                }
                "initialPrompt" => {
                    self.initial_prompt = Some(yaml_string(value, "initialPrompt")?);
                }
                "omitClaudeMd" => {
                    self.omit_claude_md = parse_bool(value, "omitClaudeMd")?;
                }
                "stages" => {
                    // 与其它可选字段一致：显式 null 当作缺省，而不是非法类型。
                    self.stages = if matches!(value, frontmatter::YamlValue::Null) {
                        None
                    } else {
                        Some(parse_stages(value)?)
                    };
                }
                other => {
                    return Err(format!("unknown frontmatter field `{other}`"));
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<AgentDefinition, String> {
        let description = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .ok_or_else(|| "field `description` is required and cannot be empty".to_string())?;
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err(
                "field `prompt` (markdown body) is required and cannot be empty".to_string(),
            );
        }
        if self.name.trim().is_empty() {
            return Err("agent name cannot be empty".to_string());
        }
        if let Some(stages) = &self.stages {
            validate_stages(stages)?;
            if self.isolation != IsolationMode::Shared {
                return Err("field `isolation` must be `shared` when `stages` is set".to_string());
            }
        }
        if self.schema_version.is_some_and(|version| version < 2)
            && (self.id.is_some()
                || self.revision.is_some()
                || self.kind.is_some()
                || !self.capabilities.is_empty())
        {
            return Err("Phase 4 profile fields require schemaVersion 2 or newer".to_string());
        }
        Ok(AgentDefinition {
            name: self.name,
            id: self.id,
            revision: self.revision,
            kind: self.kind,
            capabilities: self.capabilities,
            schema_version: self.schema_version,
            description: description.to_string(),
            prompt: prompt.to_string(),
            tools: self.tools,
            disallowed_tools: self.disallowed_tools,
            model: self.model,
            effort: self.effort,
            permission_mode: self.permission_mode,
            mcp_servers: self.mcp_servers,
            hooks: self.hooks,
            skills: self.skills,
            max_turns: self.max_turns,
            memory: self.memory,
            background: self.background,
            isolation: self.isolation,
            initial_prompt: self.initial_prompt,
            omit_claude_md: self.omit_claude_md,
            stages: self.stages,
            source: self.source,
            file_path: self.file_path,
        })
    }
}

fn parse_stages(value: &frontmatter::YamlValue) -> Result<Vec<AgentStage>, String> {
    let frontmatter::YamlValue::Seq(items) = value else {
        return Err("field `stages` must be a list".to_string());
    };
    let mut stages = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let frontmatter::YamlValue::Map(entries) = item else {
            return Err(format!("field `stages[{index}]` must be a map"));
        };
        let mut name = None;
        let mut owner = None;
        let mut consumes = Vec::new();
        let mut produces = Vec::new();
        let mut exit_criteria = Vec::new();
        let mut budget = AgentStageBudget::default();
        for (key, value) in entries {
            match key.as_str() {
                "name" => {
                    let raw = yaml_string(value, "stages.name")?;
                    if raw.trim().is_empty() {
                        return Err("field `stages.name` cannot be empty".to_string());
                    }
                    name = Some(raw.trim().to_string());
                }
                "owner" => {
                    let raw = yaml_string(value, "stages.owner")?;
                    if raw.trim().is_empty() {
                        return Err("field `stages.owner` cannot be empty".to_string());
                    }
                    owner = Some(raw.trim().to_string());
                }
                "consumes" => consumes = parse_artifacts(value, "stages.consumes")?,
                "produces" => produces = parse_artifacts(value, "stages.produces")?,
                "exitCriteria" => exit_criteria = parse_exit_criteria(value)?,
                "budget" => budget = parse_stage_budget(value)?,
                other => return Err(format!("unknown stage field `{other}`")),
            }
        }
        let name = name.ok_or_else(|| format!("field `stages[{index}].name` is required"))?;
        stages.push(AgentStage {
            name,
            owner,
            consumes,
            produces,
            exit_criteria,
            budget,
        });
    }
    Ok(stages)
}

/// 解析 stage 的 exit criteria。
///
/// 标量字符串是**一条**完整 criterion（`file:README.md exists` 里含空格），不能像
/// `skills` 那样按空格/逗号拆分，否则会变成不可判定的半截 leaves。需要多条时用列表。
fn parse_exit_criteria(value: &frontmatter::YamlValue) -> Result<Vec<String>, String> {
    match value {
        frontmatter::YamlValue::Str(raw) => {
            let criterion = raw.trim();
            if criterion.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![criterion.to_string()])
            }
        }
        frontmatter::YamlValue::Seq(items) => items
            .iter()
            .map(|item| match item {
                frontmatter::YamlValue::Str(value) if !value.trim().is_empty() => {
                    Ok(value.trim().to_string())
                }
                _ => Err("field `stages.exitCriteria` items must be non-empty strings".to_string()),
            })
            .collect(),
        frontmatter::YamlValue::Null => Ok(Vec::new()),
        _ => Err("field `stages.exitCriteria` must be a string or a list of strings".to_string()),
    }
}

fn parse_artifacts(
    value: &frontmatter::YamlValue,
    field: &str,
) -> Result<Vec<StageArtifact>, String> {
    let names = parse_string_list(value, field)?;
    let mut artifacts = Vec::new();
    for name in names {
        let artifact = StageArtifact::parse(&name).ok_or_else(|| {
            let allowed = StageArtifact::ALL
                .iter()
                .map(|artifact| artifact.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "field `{field}` contains unknown artifact `{name}`; expected one of: {allowed}"
            )
        })?;
        if artifacts.contains(&artifact) {
            return Err(format!("field `{field}` repeats artifact `{name}`"));
        }
        artifacts.push(artifact);
    }
    Ok(artifacts)
}

fn parse_stage_budget(value: &frontmatter::YamlValue) -> Result<AgentStageBudget, String> {
    let frontmatter::YamlValue::Map(entries) = value else {
        return Err("field `stages.budget` must be a map".to_string());
    };
    let mut budget = AgentStageBudget::default();
    for (key, value) in entries {
        match key.as_str() {
            "maxTurns" => {
                budget.max_turns = Some(parse_positive_u32(value, "stages.budget.maxTurns")?);
            }
            "tokenBudget" => {
                budget.token_budget = Some(parse_positive_u64(value, "stages.budget.tokenBudget")?);
            }
            "timeoutSeconds" => {
                budget.timeout_seconds =
                    Some(parse_positive_u64(value, "stages.budget.timeoutSeconds")?);
            }
            other => return Err(format!("unknown stage budget field `{other}`")),
        }
    }
    Ok(budget)
}

fn parse_positive_u64(value: &frontmatter::YamlValue, field: &str) -> Result<u64, String> {
    let number = match value {
        frontmatter::YamlValue::Int(value) => *value,
        frontmatter::YamlValue::Str(raw) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("field `{field}` must be a positive integer"))?,
        _ => return Err(format!("field `{field}` must be a positive integer")),
    };
    u64::try_from(number)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("field `{field}` must be a positive integer"))
}

pub(crate) fn validate_stages(stages: &[AgentStage]) -> Result<(), String> {
    if stages.is_empty() {
        return Err("field `stages` must not be empty".to_string());
    }
    let mut names = std::collections::HashSet::new();
    let mut produced = std::collections::HashSet::new();
    for stage in stages {
        if !names.insert(stage.name.as_str()) {
            return Err(format!("duplicate stage name `{}`", stage.name));
        }
        if stage.produces.is_empty() {
            return Err(format!(
                "stage `{}` must produce at least one artifact",
                stage.name
            ));
        }
        for artifact in &stage.consumes {
            if !produced.contains(artifact) {
                return Err(format!(
                    "stage `{}` consumes `{}` before any earlier stage produces it",
                    stage.name,
                    artifact.as_str()
                ));
            }
        }
        for artifact in &stage.produces {
            produced.insert(*artifact);
        }
    }
    Ok(())
}

fn yaml_string(value: &frontmatter::YamlValue, field: &str) -> Result<String, String> {
    match value {
        frontmatter::YamlValue::Str(value) => Ok(value.clone()),
        _ => Err(format!("field `{field}` must be a string")),
    }
}

fn parse_tool_selection(value: &frontmatter::YamlValue) -> Result<ToolSelection, String> {
    match value {
        frontmatter::YamlValue::Str(raw) => Ok(ToolSelection::parse(raw)),
        frontmatter::YamlValue::Seq(items) => {
            let list = items
                .iter()
                .map(|item| match item {
                    frontmatter::YamlValue::Str(value) => Ok(value.clone()),
                    _ => Err("field `tools` items must be strings".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ToolSelection::from_items(list))
        }
        frontmatter::YamlValue::Null => Ok(ToolSelection::All),
        _ => Err("field `tools` must be a string or a list of strings".to_string()),
    }
}

fn parse_tool_list(value: &frontmatter::YamlValue) -> Result<Vec<ToolPattern>, String> {
    match value {
        // 不能用 `ToolSelection::parse`：字符串里只要出现 `*`，它会把整个选择变成
        // `All`，`patterns()` 随即为空，denylist 被静默丢弃（`"bash, *"` 变成
        // “什么都不禁止”）。这里按 pattern 列表直接拆分，`*` 作为普通 pattern 保留。
        frontmatter::YamlValue::Str(raw) => Ok(raw
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(ToolPattern::new)
            .collect()),
        frontmatter::YamlValue::Seq(items) => items
            .iter()
            .map(|item| match item {
                frontmatter::YamlValue::Str(value) if !value.trim().is_empty() => {
                    Ok(ToolPattern::new(value.trim()))
                }
                _ => Err("field `disallowedTools` items must be non-empty strings".to_string()),
            })
            .collect(),
        frontmatter::YamlValue::Null => Ok(Vec::new()),
        _ => Err("field `disallowedTools` must be a string or a list of strings".to_string()),
    }
}

fn parse_string_list(value: &frontmatter::YamlValue, field: &str) -> Result<Vec<String>, String> {
    match value {
        frontmatter::YamlValue::Str(raw) => Ok(raw
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect()),
        frontmatter::YamlValue::Seq(items) => {
            let mut values = Vec::new();
            for item in items {
                match item {
                    frontmatter::YamlValue::Str(value) => {
                        let trimmed = value.trim();
                        if !trimmed.is_empty() {
                            values.push(trimmed.to_string());
                        }
                    }
                    _ => return Err(format!("field `{field}` items must be strings")),
                }
            }
            Ok(values)
        }
        frontmatter::YamlValue::Null => Ok(Vec::new()),
        _ => Err(format!(
            "field `{field}` must be a string or a list of strings"
        )),
    }
}

fn parse_mcp_servers(value: &frontmatter::YamlValue) -> Result<Vec<AgentMcpServer>, String> {
    let items = match value {
        frontmatter::YamlValue::Seq(items) => items.clone(),
        frontmatter::YamlValue::Str(raw) => {
            return Ok(vec![AgentMcpServer::Reference(raw.trim().to_string())]);
        }
        frontmatter::YamlValue::Null => return Ok(Vec::new()),
        _ => return Err("field `mcpServers` must be a list".to_string()),
    };
    let mut servers = Vec::new();
    for item in items {
        match item {
            frontmatter::YamlValue::Str(raw) if !raw.trim().is_empty() => {
                servers.push(AgentMcpServer::Reference(raw.trim().to_string()));
            }
            frontmatter::YamlValue::Map(entries) => {
                for (name, config) in entries {
                    servers.push(AgentMcpServer::Inline {
                        name,
                        config: config.to_json(),
                    });
                }
            }
            _ => {
                return Err(
                    "field `mcpServers` items must be a server name or a `{name: config}` map"
                        .to_string(),
                );
            }
        }
    }
    Ok(servers)
}

fn parse_positive_u32(value: &frontmatter::YamlValue, field: &str) -> Result<u32, String> {
    let number = match value {
        frontmatter::YamlValue::Int(value) => *value,
        frontmatter::YamlValue::Str(raw) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("field `{field}` must be a positive integer"))?,
        _ => return Err(format!("field `{field}` must be a positive integer")),
    };
    u32::try_from(number)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("field `{field}` must be a positive integer"))
}

fn parse_bool(value: &frontmatter::YamlValue, field: &str) -> Result<bool, String> {
    match value {
        frontmatter::YamlValue::Bool(value) => Ok(*value),
        frontmatter::YamlValue::Str(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(true),
            "false" | "no" | "off" | "0" => Ok(false),
            _ => Err(format!("field `{field}` must be a boolean")),
        },
        _ => Err(format!("field `{field}` must be a boolean")),
    }
}

fn parse_permission_mode(raw: &str) -> Result<PermissionMode, String> {
    let normalized = raw.trim();
    // 参考实现还接受 `dontAsk` / `auto`；kiana 的权限模式没有对应档位。两者都映射到
    // `default`（headless 子代理的 Ask 会在运行期一律 Deny，因此不会意外放行）。
    match normalized.to_ascii_lowercase().as_str() {
        "dontask" | "dont-ask" | "auto" => Ok(PermissionMode::Default),
        _ => PermissionMode::parse(normalized)
            .ok_or_else(|| format!("unsupported permissionMode `{normalized}`")),
    }
}

/// 内置 agent。名字与 prompt 按 Kiana 的工具集合重写，不复制参考实现的文案。
pub fn builtin_agent_definitions() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            name: "general-purpose".to_string(),
            id: None,
            revision: None,
            kind: None,
            capabilities: Vec::new(),
            schema_version: None,
            description: "General-purpose subagent with the full inherited tool set.".to_string(),
            prompt: "You are a general-purpose subagent. Complete the delegated task and return a concise result to the parent agent.".to_string(),
            tools: Some(ToolSelection::All),
            disallowed_tools: Vec::new(),
            model: AgentModel::Inherit,
            effort: None,
            permission_mode: None,
            mcp_servers: Vec::new(),
            hooks: None,
            skills: Vec::new(),
            max_turns: None,
            memory: None,
            background: false,
            isolation: IsolationMode::Shared,
            initial_prompt: None,
            omit_claude_md: false,
            stages: None,
            source: AgentDefinitionSource::Builtin,
            file_path: None,
        },
        AgentDefinition {
            name: "explore".to_string(),
            id: None,
            revision: None,
            kind: None,
            capabilities: Vec::new(),
            schema_version: None,
            description: "Read-only codebase exploration subagent.".to_string(),
            prompt: "You are a read-only exploration subagent. Search the codebase and report findings without modifying files.".to_string(),
            tools: Some(ToolSelection::from_items([
                "read".to_string(),
                "ls".to_string(),
                "grep".to_string(),
                "find".to_string(),
            ])),
            disallowed_tools: Vec::new(),
            model: AgentModel::Inherit,
            effort: None,
            permission_mode: Some(PermissionMode::ReadOnly),
            mcp_servers: Vec::new(),
            hooks: None,
            skills: Vec::new(),
            max_turns: None,
            memory: None,
            background: false,
            isolation: IsolationMode::Shared,
            initial_prompt: None,
            omit_claude_md: true,
            stages: None,
            source: AgentDefinitionSource::Builtin,
            file_path: None,
        },
        AgentDefinition {
            name: "plan".to_string(),
            id: None,
            revision: None,
            kind: None,
            capabilities: Vec::new(),
            schema_version: None,
            description: "Read-only planning subagent that proposes an implementation plan.".to_string(),
            prompt: "You are a planning subagent. Inspect the codebase and produce a concrete implementation plan. Do not modify files.".to_string(),
            tools: Some(ToolSelection::from_items([
                "read".to_string(),
                "ls".to_string(),
                "grep".to_string(),
                "find".to_string(),
            ])),
            disallowed_tools: Vec::new(),
            model: AgentModel::Inherit,
            effort: None,
            permission_mode: Some(PermissionMode::ReadOnly),
            mcp_servers: Vec::new(),
            hooks: None,
            skills: Vec::new(),
            max_turns: None,
            memory: None,
            background: false,
            isolation: IsolationMode::Shared,
            initial_prompt: None,
            omit_claude_md: true,
            stages: None,
            source: AgentDefinitionSource::Builtin,
            file_path: None,
        },
    ]
}

impl ToolSelection {
    /// 显式模式列表；`All` 返回空切片。
    pub fn patterns(&self) -> &[ToolPattern] {
        match self {
            Self::All => &[],
            Self::Tools(patterns) => patterns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::isolation::IsolationMode;
    use std::io::Write;

    fn write_agent(dir: &Path, file: &str, contents: &str) {
        std::fs::create_dir_all(dir).expect("create dir");
        let mut handle = std::fs::File::create(dir.join(file)).expect("create file");
        handle.write_all(contents.as_bytes()).expect("write");
    }

    #[test]
    fn parses_all_documented_fields() {
        let definition = parse_markdown_definition(
            "---\n\
             name: reviewer\n\
             description: Reviews code\n\
             tools: [read, grep]\n\
             disallowedTools: bash\n\
             model: inherit\n\
             effort: high\n\
             permissionMode: plan\n\
             mcpServers:\n  - slack\n  - github:\n      command: gh\n\
             hooks:\n  PreToolUse:\n    - matcher: Bash\n\
             skills: pdf, docs\n\
             maxTurns: 7\n\
             memory: project\n\
             background: true\n\
             isolation: worktree\n\
             initialPrompt: Review the diff\n\
             omitClaudeMd: true\n\
             ---\n\
             You review code.\n",
            "reviewer",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("definition parses");
        assert_eq!(definition.name, "reviewer");
        assert_eq!(definition.model, AgentModel::Inherit);
        assert_eq!(definition.effort, Some(AgentEffort::High));
        assert_eq!(definition.permission_mode, Some(PermissionMode::ReadOnly));
        assert_eq!(definition.skills, vec!["pdf", "docs"]);
        assert_eq!(definition.max_turns, Some(7));
        assert_eq!(definition.memory, Some(AgentMemoryScope::Project));
        assert!(definition.background);
        assert_eq!(definition.isolation, IsolationMode::Worktree);
        assert_eq!(
            definition.initial_prompt.as_deref(),
            Some("Review the diff")
        );
        assert!(definition.omit_claude_md);
        assert_eq!(definition.mcp_servers.len(), 2);
        assert!(matches!(
            definition.mcp_servers[0],
            AgentMcpServer::Reference(_)
        ));
    }

    #[test]
    fn parses_declared_stages() {
        let definition = parse_markdown_definition(
            "---\n\
             name: pipeline\n\
             description: Sequential workflow\n\
             stages:\n  - name: plan\n    owner: planner\n    produces: [plan]\n  - name: implement\n    consumes: [plan]\n    produces: [changed_files]\n    exitCriteria: file:README.md exists\n    budget:\n      maxTurns: 12\n      tokenBudget: 200000\n      timeoutSeconds: 600\n---\n\
             Run the pipeline.\n",
            "pipeline",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("definition parses");
        let stages = definition.stages.expect("stages");
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].name, "plan");
        assert_eq!(stages[0].owner.as_deref(), Some("planner"));
        assert_eq!(stages[0].produces, vec![StageArtifact::Plan]);
        assert_eq!(stages[1].consumes, vec![StageArtifact::Plan]);
        assert_eq!(stages[1].produces, vec![StageArtifact::ChangedFiles]);
        assert_eq!(
            stages[1].exit_criteria,
            vec!["file:README.md exists"],
            "a scalar exit criterion keeps its spaces"
        );
        assert_eq!(stages[1].budget.max_turns, Some(12));
        assert_eq!(stages[1].budget.token_budget, Some(200_000));
        assert_eq!(stages[1].budget.timeout_seconds, Some(600));
    }

    #[test]
    fn parses_stages_from_json_definitions() {
        let definition = parse_json_definition(
            "pipeline",
            &serde_json::json!({
                "description": "Sequential workflow",
                "prompt": "Run the pipeline.",
                "stages": [
                    {"name": "verify", "produces": ["review_findings"]}
                ]
            }),
            AgentDefinitionSource::Flag,
        )
        .expect("definition parses");
        let stages = definition.stages.expect("stages");
        assert_eq!(stages[0].produces, vec![StageArtifact::ReviewFindings]);
    }

    #[test]
    fn null_stages_means_no_stages() {
        let definition = parse_json_definition(
            "pipeline",
            &serde_json::json!({
                "description": "Sequential workflow",
                "prompt": "Run the pipeline.",
                "stages": null
            }),
            AgentDefinitionSource::Flag,
        )
        .expect("explicit null stages must be treated as absent");
        assert!(definition.stages.is_none());
    }

    /// Phase 4：`id` / `revision` / `kind` / `capabilities` 是可选 facet；旧文件
    /// （无 schemaVersion）保持可读，声明 `schemaVersion: 1` 却使用 facet 时
    /// fail closed，写出所需版本由 `write_schema_version()` 报告。
    #[test]
    fn profile_facets_are_parsed_and_version_gated() {
        let definition = parse_markdown_definition(
            "---\ndescription: Planner\nid: planner\nrevision: 3\nkind: planner\ncapabilities: [plan, review]\nschemaVersion: 2\n---\nprompt",
            "planner",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("facets parse");
        assert_eq!(definition.id.as_deref(), Some("planner"));
        assert_eq!(definition.revision, Some(3));
        assert_eq!(definition.kind.as_deref(), Some("planner"));
        assert_eq!(definition.capabilities, vec!["plan", "review"]);
        assert_eq!(definition.schema_version, Some(2));
        assert_eq!(definition.write_schema_version(), 2);

        // 旧文件保持可读。
        let legacy = parse_markdown_definition(
            "---\ndescription: legacy\n---\nprompt",
            "legacy",
            AgentDefinitionSource::Project,
            None,
        )
        .expect("old definitions stay readable");
        assert!(legacy.id.is_none() && legacy.capabilities.is_empty());
        assert_eq!(legacy.write_schema_version(), 1);

        let error = parse_markdown_definition(
            "---\ndescription: x\nid: p\nschemaVersion: 1\n---\nprompt",
            "p",
            AgentDefinitionSource::Project,
            None,
        )
        .unwrap_err();
        assert!(error.contains("schemaVersion 2"), "{error}");

        let error = parse_markdown_definition(
            "---\ndescription: x\ncapabilities: [plan, plan]\n---\nprompt",
            "p",
            AgentDefinitionSource::Project,
            None,
        )
        .unwrap_err();
        assert!(error.contains("repeats `plan`"), "{error}");

        let definition = parse_json_definition(
            "json",
            &serde_json::json!({
                "description": "x",
                "prompt": "p",
                "id": "json-id",
                "capabilities": ["review"]
            }),
            AgentDefinitionSource::Flag,
        )
        .expect("json facets parse");
        assert_eq!(definition.id.as_deref(), Some("json-id"));
        assert_eq!(definition.capabilities, vec!["review"]);
    }

    #[test]
    fn rejects_invalid_stage_declarations() {
        let cases = [
            (
                "stages:\n  - name: plan\n    produces: [unknown]\n",
                "unknown artifact",
            ),
            (
                "stages:\n  - name: plan\n    consumes: [plan]\n    produces: [plan]\n",
                "before any earlier stage produces it",
            ),
            (
                "stages:\n  - name: plan\n    produces: [plan]\n  - name: plan\n    produces: [changed_files]\n",
                "duplicate stage name",
            ),
            (
                "isolation: worktree\nstages:\n  - name: plan\n    produces: [plan]\n",
                "must be `shared`",
            ),
            (
                "stages:\n  - name: plan\n    produces: []\n",
                "must produce at least one artifact",
            ),
        ];
        for (fields, expected) in cases {
            let text = format!(
                "---\nname: pipeline\ndescription: Sequential workflow\n{fields}---\nBody\n"
            );
            let error =
                parse_markdown_definition(&text, "pipeline", AgentDefinitionSource::Project, None)
                    .expect_err("invalid stages must be rejected");
            assert!(
                error.contains(expected),
                "{error} does not contain {expected}"
            );
        }
    }

    #[test]
    fn name_defaults_to_file_stem() {
        let definition = parse_markdown_definition(
            "---\ndescription: Does things\n---\nBody\n",
            "from-file",
            AgentDefinitionSource::User,
            None,
        )
        .expect("definition parses");
        assert_eq!(definition.name, "from-file");
    }

    #[test]
    fn unknown_field_is_an_error() {
        let error = parse_markdown_definition(
            "---\ndescription: test\nsurprise: true\n---\nBody\n",
            "x",
            AgentDefinitionSource::User,
            None,
        )
        .unwrap_err();
        assert!(
            error.contains("unknown frontmatter field `surprise`"),
            "{error}"
        );
    }

    #[test]
    fn load_continues_after_one_bad_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("agents");
        write_agent(
            &dir,
            "good.md",
            "---\ndescription: good agent\n---\nGood prompt\n",
        );
        write_agent(
            &dir,
            "bad.md",
            "---\ndescription: bad agent\nunknownField: 1\n---\nBad prompt\n",
        );
        write_agent(&dir, "broken.md", "---\ndescription: broken\n---\n");
        let mut loader = AgentDefinitionLoader::new();
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Project,
            &dir,
        ));
        let report = loader.load();
        assert!(report.get("good").is_some(), "good file still loads");
        assert!(report.get("bad").is_none());
        assert!(report.get("broken").is_none());
        assert_eq!(report.scanned_files, 3);
        assert_eq!(report.errors.len(), 2);
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.message.contains("unknown"))
        );
    }

    #[test]
    fn source_precedence_is_fixed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plugin = temp.path().join("plugin");
        let user = temp.path().join("user");
        let project = temp.path().join("project");
        let policy = temp.path().join("policy");
        write_agent(&plugin, "x.md", "---\ndescription: plugin\n---\nplugin\n");
        write_agent(&user, "x.md", "---\ndescription: user\n---\nuser\n");
        write_agent(
            &project,
            "x.md",
            "---\ndescription: project\n---\nproject\n",
        );
        write_agent(&policy, "x.md", "---\ndescription: policy\n---\npolicy\n");

        let mut loader = AgentDefinitionLoader::new();
        // 故意用反序添加，确认 roots 的添加顺序不改变覆盖顺序。
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Policy,
            &policy,
        ));
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Project,
            &project,
        ));
        loader.add_root(AgentDefinitionRoot::new(AgentDefinitionSource::User, &user));
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Plugin,
            &plugin,
        ));
        let report = loader.load();
        let definition = report.get("x").expect("definition");
        assert_eq!(definition.source, AgentDefinitionSource::Policy);
        assert_eq!(definition.description, "policy");
    }

    #[test]
    fn flag_json_overrides_everything_and_bad_entries_do_not_abort() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        write_agent(
            &project,
            "x.md",
            "---\ndescription: project\n---\nproject\n",
        );
        let mut loader = AgentDefinitionLoader::new();
        loader.add_root(AgentDefinitionRoot::new(
            AgentDefinitionSource::Project,
            &project,
        ));
        loader.add_flag_json(serde_json::json!({
            "x": {"description": "flag", "prompt": "flag prompt"},
            "bad": {"description": 3, "prompt": "x"}
        }));
        let report = loader.load();
        assert_eq!(report.get("x").expect("x").description, "flag");
        assert!(report.get("bad").is_none());
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn builtins_exist() {
        let report = AgentDefinitionLoader::new().load();
        assert!(report.get("general-purpose").is_some());
        let explore = report.get("explore").expect("explore");
        assert!(explore.omit_claude_md);
        assert_eq!(explore.permission_mode, Some(PermissionMode::ReadOnly));
    }

    #[test]
    fn invalid_typed_fields_are_reported() {
        for body in [
            "---\ndescription: x\nmaxTurns: -1\n---\nprompt",
            "---\ndescription: x\nmaxTurns: nope\n---\nprompt",
            "---\ndescription: x\nisolation: remote\n---\nprompt",
            "---\ndescription: x\npermissionMode: whatever\n---\nprompt",
            "---\ndescription: x\neffort: insane\n---\nprompt",
        ] {
            assert!(
                parse_markdown_definition(body, "x", AgentDefinitionSource::User, None).is_err(),
                "{body:?} should fail"
            );
        }
    }

    #[test]
    fn disallowed_star_denies_every_tool() {
        let definition = parse_markdown_definition(
            "---\ndescription: x\ndisallowedTools: \"*\"\n---\nprompt",
            "x",
            AgentDefinitionSource::User,
            None,
        )
        .expect("definition");
        let read = CanonicalToolId::parse("read");
        assert!(!definition.allows_tool(&read, "read"));
    }

    #[test]
    fn disallowed_string_list_keeps_a_star_alongside_other_patterns() {
        let definition = parse_markdown_definition(
            "---\ndescription: x\ndisallowedTools: \"read, *\"\n---\nprompt",
            "x",
            AgentDefinitionSource::User,
            None,
        )
        .expect("definition");
        assert_eq!(definition.disallowed_tools.len(), 2);
        assert_eq!(definition.disallowed_tools[0], ToolPattern::new("read"));
        assert_eq!(definition.disallowed_tools[1], ToolPattern::new("*"));
        // `*` 必须仍然禁止未显式列出的工具；旧实现会把整个列表变成空。
        let grep = CanonicalToolId::parse("grep");
        assert!(!definition.allows_tool(&grep, "grep"));
    }

    #[test]
    fn tool_selection_intersects_with_definition_denylist() {
        let definition = parse_markdown_definition(
            "---\ndescription: x\ntools: \"*\"\ndisallowedTools: bash\n---\nprompt",
            "x",
            AgentDefinitionSource::User,
            None,
        )
        .expect("definition");
        let read = CanonicalToolId::parse("read");
        let bash = CanonicalToolId::parse("bash");
        assert!(definition.allows_tool(&read, "read"));
        assert!(!definition.allows_tool(&bash, "bash"));
    }
}
