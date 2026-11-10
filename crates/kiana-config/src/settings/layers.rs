//! 分层配置：来源、递归合并、`origins()`、`fingerprint()` 与 managed policy。
//!
//! `SettingsManager` 只认识 global（用户）与 project 两个作用域，而且它的写回路径是
//! 字节级、面向“外部编辑保留”的。本模块是另一条**只读**的解析路径：把系统、管理员、
//! 企业、用户、项目、项目本地、命令行与会话八类来源按优先级合并成一份有效配置，并
//! 回答三个问题：
//!
//! 1. 这个值最终从哪一层来？—— [`MergedConfig::origins`]；
//! 2. 配置指纹是什么？—— [`MergedConfig::fingerprint`]，对每层 canonical JSON 做
//!    SHA256 再汇总，键序不敏感、改任一有效值都会变；
//! 3. 管理员策略有没有被下层覆盖？—— [`ManagedPolicy`] 在合并后强制生效。
//!
//! # 层的顺序
//!
//! [`ConfigLayerSource::ALL`] 按**低 → 高**排列：
//!
//! `Builtin < System < Managed < Enterprise < User < Project < Local < CliFlag < Session`
//!
//! 同一 key 出现在多层时高层胜；对象递归合并；数组由
//! [`array_merge_mode`] 的 key 表决定 `concat + 去重` 还是整体替换。默认替换：
//! 只有明确列出的“集合型”键才跨层累加，避免无意把 `npmCommand` 之类的顺序敏感
//! 配置拼在一起。
//!
//! # managed policy
//!
//! [`ManagedPolicy`] 只从管理员可控的层（[`ConfigLayerSource::is_policy_source`]：
//! System / Managed / Enterprise）读取，支持：
//!
//! - `allow_managed_permission_rules_only`：`allow`/`ask` 规则只采信管理员层；
//!   `deny` 规则仍然从所有层合并（拒绝只会收窄权限，不能被人为丢弃）；
//! - `allow_managed_hooks_only`：`hooks` 只采信管理员层；
//! - `disable_bypass_permissions_mode`：强制有效配置里的
//!   `permissions.disableBypassPermissionsMode = true`，项目层写 `false` 也盖不掉；
//! - `allowed_mcp_servers` / `denied_mcp_servers`：deny 永远合并，allow 在管理员层
//!   给出时只读管理员层；同一服务器同时出现在 allow 与 deny → deny 胜（冲突拒绝）。
//!
//! 键名同时接受 camelCase 与 snake_case；policy 值也接受放在 `permissions` 对象里，
//! 方便与现有 `permissions` settings 共存。
//!
//! # 严格诊断
//!
//! [`ConfigLayer::load_file`] 默认只对 System / Managed / Enterprise 启用
//! [`LayerLoadMode::Strict`]，旧用户/项目 settings 保持宽松（未知字段不报错）。严格
//! 模式的诊断复用 [`crate::settings::strict`]：文件、1-based 行列、dotted key path。
//! 诊断不会阻止层参与合并；语法错误才会让该层加载失败。
//!
//! # 与 `SettingsManager` 的关系
//!
//! 本模块**不**替换 `SettingsManager` 的加载/写回路径，也不修改 `settings.json`
//! 的字节。接线方式是：启动时把各层文件（或内存值）交给 [`ConfigLayers::resolve`]，
//! 把 `MergedConfig::value()` 当作有效设置、把 `origins()`/`fingerprint()` 用于“值为
//! 什么来自这里”的展示与缓存失效。把有效值回灌 `SettingsManager` 属于后续接线
//! （见 `docs/history/porting/handoffs/phase-03.md`）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::settings::key_path::{child_key_path, index_key_path};
use crate::settings::strict::{
    StrictDiagnostic, StrictDiagnosticKind, parse_strict_settings, strict_settings_schema,
};

/// 配置来源（按优先级从低到高）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigLayerSource {
    /// 程序内置默认值（`Settings::default()` 等）。
    Builtin,
    /// 主机级配置（Unix `/etc/kiana`，Windows `%ProgramData%\kiana`）。
    System,
    /// 管理员下发的托管配置（`managed-settings.json`）。
    Managed,
    /// 企业云/策略包下发的配置。
    Enterprise,
    /// 用户配置（`~/.kiana/settings.json`）。
    User,
    /// 项目配置（`<cwd>/.kiana/settings.json`）。
    Project,
    /// 项目本地配置（`<cwd>/.kiana/settings.local.json`）。
    Local,
    /// 命令行 flag（`--permission-mode`、`--allow` 等）。
    CliFlag,
    /// 当前会话内覆盖。
    Session,
}

impl ConfigLayerSource {
    /// 全部来源，按优先级从低到高。
    pub const ALL: [Self; 9] = [
        Self::Builtin,
        Self::System,
        Self::Managed,
        Self::Enterprise,
        Self::User,
        Self::Project,
        Self::Local,
        Self::CliFlag,
        Self::Session,
    ];

    /// 优先级数值（越大越优先）。数值留出间隔，便于以后插入新层。
    pub fn precedence(self) -> i16 {
        match self {
            Self::Builtin => 0,
            Self::System => 10,
            Self::Managed => 20,
            Self::Enterprise => 30,
            Self::User => 40,
            Self::Project => 50,
            Self::Local => 60,
            Self::CliFlag => 70,
            Self::Session => 80,
        }
    }

    /// 稳定字符串（事件/日志/`origins` 展示用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::System => "system",
            Self::Managed => "managed",
            Self::Enterprise => "enterprise",
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
            Self::CliFlag => "cli-flag",
            Self::Session => "session",
        }
    }

    /// 是否为管理员可控层（managed policy 只从这些层读取）。
    pub fn is_policy_source(self) -> bool {
        matches!(self, Self::System | Self::Managed | Self::Enterprise)
    }

    /// 是否默认启用严格 schema 诊断。
    ///
    /// 旧用户/项目 settings 是宽松格式：管理员文件写错不能静默失效，而用户文件里的
    /// 自定义键不应让整份配置报错。
    pub fn is_strict_by_default(self) -> bool {
        self.is_policy_source()
    }

    /// 该层对应的标准文件路径（`Builtin` / `CliFlag` / `Session` 没有文件）。
    ///
    /// User 层用 `kiana_core::config::get_settings_path()`（默认
    /// `~/.kiana/agent/settings.json`，可被 `KIANA_CODING_AGENT_DIR` 覆盖）；
    /// Project / Local 用 `cwd/.kiana/...`。System / Managed / Enterprise 在 Unix 用
    /// `/etc/kiana/...`，Windows 用 `%ProgramData%\kiana\...`。
    pub fn standard_path(self, cwd: &Path) -> Option<PathBuf> {
        match self {
            Self::Builtin | Self::CliFlag | Self::Session => None,
            Self::User => Some(user_settings_path()),
            Self::Project => Some(crate::settings::permissions::project_settings_path(cwd)),
            Self::Local => Some(crate::settings::permissions::local_settings_path(cwd)),
            Self::System => Some(system_config_dir().join("settings.json")),
            Self::Managed => Some(system_config_dir().join("managed-settings.json")),
            Self::Enterprise => Some(system_config_dir().join("enterprise-settings.json")),
        }
    }
}

impl std::fmt::Display for ConfigLayerSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 用户配置路径：`<agent-dir>/settings.json`（通常是 `~/.kiana/agent/settings.json`，
/// 可用 `KIANA_CODING_AGENT_DIR` 覆盖）。
///
/// 直接复用 `kiana_core::config::get_settings_path`，避免与 `SettingsManager` 的实际
/// 读写路径出现分叉。
fn user_settings_path() -> PathBuf {
    kiana_core::config::get_settings_path()
}

/// 系统级配置目录。
fn system_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(program_data) = std::env::var_os("ProgramData") {
            return PathBuf::from(program_data).join("kiana");
        }
        return PathBuf::from(r"C:\ProgramData\kiana");
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/kiana")
    }
}

/// 数组合并策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayMergeMode {
    /// 高层数组整体替换低层数组（默认）。
    Replace,
    /// 低层在前、高层在后拼接，并去掉完全重复的 JSON 元素。
    ConcatDedup,
}

/// 按 dotted key path 决定数组合并策略。
///
/// 表驱动的理由：每类 key 的语义不同。权限规则、deny/allow 列表、hooks 事件、
/// 扩展/技能包列表是“集合”，跨层应当累加；而 `npmCommand` 这类命令行顺序敏感，
/// `modelThinkingLevels` 这类对象由对象合并处理，都不应 concat。默认 [`Replace`]，
/// 只有本函数明确列出的路径才 concat，新增设置项时需要逐项评审。
pub fn array_merge_mode(path: &str) -> ArrayMergeMode {
    match path {
        // 权限规则与目录：跨层累加，否则项目层会把用户层的 deny 覆盖掉。
        "permissions.rules"
        | "permissions.allow"
        | "permissions.deny"
        | "permissions.ask"
        | "permissions.addDirs"
        | "permissions.additionalDirectories"
        | "permissions.workspaceRoots"
        | "permissions.enabledMcpjsonServers"
        | "permissions.disabledMcpjsonServers"
        | "permissions.allowedMcpServers"
        | "permissions.allowed_mcp_servers"
        | "permissions.deniedMcpServers"
        | "permissions.denied_mcp_servers" => ArrayMergeMode::ConcatDedup,
        // managed policy 的 MCP allow/deny（顶层或 `permissions` 里）：deny 必须
        // 跨层累加；allow 的“只读 managed”在 policy 应用阶段处理，但这里仍用
        // concat 保留所有层的输入，否则无法在 `origins` 里看到被策略丢弃的来源。
        "allowedMcpServers" | "allowed_mcp_servers" | "deniedMcpServers" | "denied_mcp_servers" => {
            ArrayMergeMode::ConcatDedup
        }
        // 资源/扩展类列表：用户装一份、项目再装一份是常见需求。
        "extensions" | "prompts" | "themes" | "packages" | "skills" | "enabledModels"
        | "defaultTools" => ArrayMergeMode::ConcatDedup,
        // hooks 的事件名是开放的（PreToolUse/PostToolUse/...），所有事件数组都累加。
        path if path.starts_with("hooks.") => ArrayMergeMode::ConcatDedup,
        _ => ArrayMergeMode::Replace,
    }
}

/// 层文件的解析模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerLoadMode {
    /// 宽松：未知字段/类型不符不报错（旧 settings.json 的既有语义）。
    Lenient,
    /// 严格：未知字段/类型错误带文件、行列与 key path。
    Strict,
}

impl LayerLoadMode {
    /// 该来源的默认模式。
    pub fn default_for(source: ConfigLayerSource) -> Self {
        if source.is_strict_by_default() {
            Self::Strict
        } else {
            Self::Lenient
        }
    }
}

/// 层加载失败（只有 IO 或 JSON 语法错误；字段级问题是诊断，不是失败）。
#[derive(Debug)]
pub enum LayerLoadError {
    /// 读文件失败。
    Io {
        /// 文件。
        path: PathBuf,
        /// 底层错误。
        error: std::io::Error,
    },
    /// JSON 语法错误（带 1-based 行列）。
    Parse(StrictDiagnostic),
}

impl std::fmt::Display for LayerLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(formatter, "{}: {error}", path.display()),
            Self::Parse(diagnostic) => write!(formatter, "{diagnostic}"),
        }
    }
}

impl std::error::Error for LayerLoadError {}

/// 一层配置。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigLayer {
    /// 来源。
    pub source: ConfigLayerSource,
    /// 文件路径（内存层为 `None`）。
    pub path: Option<PathBuf>,
    /// 解析后的 JSON 对象。非对象输入会被规范化为空对象并记录诊断。
    pub value: Value,
    /// 严格诊断（宽松加载时为空）。
    pub diagnostics: Vec<StrictDiagnostic>,
}

impl ConfigLayer {
    /// 从内存 JSON 值构造。非对象根会被规范化为 `{}`。
    pub fn new(source: ConfigLayerSource, value: Value) -> Self {
        let (value, diagnostics) = normalize_root(None, value, source.is_strict_by_default());
        Self {
            source,
            path: None,
            value,
            diagnostics,
        }
    }

    /// 带文件路径的内存层。
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// [`Self::new`] 的别名。
    pub fn from_value(source: ConfigLayerSource, value: Value) -> Self {
        Self::new(source, value)
    }

    /// 该层是否为“空层”（没有文件内容）。
    pub fn is_empty(&self) -> bool {
        self.value.as_object().is_none_or(Map::is_empty)
    }

    /// 从文件加载。
    ///
    /// 文件不存在返回 `Ok(None)`（调用方跳过该层）；IO 错误与 JSON 语法错误返回
    /// `Err`。字段级问题放在 [`ConfigLayer::diagnostics`] 里，不阻止层参与合并。
    pub fn load_file(
        source: ConfigLayerSource,
        path: &Path,
        mode: LayerLoadMode,
    ) -> Result<Option<Self>, LayerLoadError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(LayerLoadError::Io {
                    path: path.to_path_buf(),
                    error,
                });
            }
        };
        if text.trim().is_empty() {
            return Ok(Some(Self {
                source,
                path: Some(path.to_path_buf()),
                value: Value::Object(Map::new()),
                diagnostics: Vec::new(),
            }));
        }
        match mode {
            LayerLoadMode::Lenient => {
                let value: Value = serde_json::from_str(&text).map_err(|error| {
                    LayerLoadError::Parse(StrictDiagnostic {
                        file: path.to_path_buf(),
                        line: error.line().max(1),
                        column: error.column().max(1),
                        key_path: String::new(),
                        kind: StrictDiagnosticKind::ParseError,
                        message: format!("invalid JSON: {error}"),
                    })
                })?;
                let (value, diagnostics) = normalize_root(Some(path), value, false);
                Ok(Some(Self {
                    source,
                    path: Some(path.to_path_buf()),
                    value,
                    diagnostics,
                }))
            }
            LayerLoadMode::Strict => {
                let schema = strict_settings_schema();
                let (value, diagnostics) =
                    parse_strict_settings(path, &text, &schema).map_err(LayerLoadError::Parse)?;
                let (value, _) = normalize_root(Some(path), value, false);
                Ok(Some(Self {
                    source,
                    path: Some(path.to_path_buf()),
                    value,
                    diagnostics,
                }))
            }
        }
    }
}

/// 非对象根（数组/标量）按空对象处理；`emit_diagnostic` 为真时补一条诊断。
fn normalize_root(
    path: Option<&Path>,
    value: Value,
    emit_diagnostic: bool,
) -> (Value, Vec<StrictDiagnostic>) {
    if value.is_object() {
        return (value, Vec::new());
    }
    let diagnostic = StrictDiagnostic {
        file: path.unwrap_or_else(|| Path::new("<memory>")).to_path_buf(),
        line: 1,
        column: 1,
        key_path: String::new(),
        kind: StrictDiagnosticKind::InvalidType,
        message: format!(
            "expected object at the top level, found {}",
            describe_value(&value)
        ),
    };
    let diagnostics = if emit_diagnostic {
        vec![diagnostic]
    } else {
        Vec::new()
    };
    (Value::Object(Map::new()), diagnostics)
}

fn describe_value(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 多层配置集合。
#[derive(Debug, Clone, Default)]
pub struct ConfigLayers {
    layers: Vec<ConfigLayer>,
}

impl ConfigLayers {
    /// 空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一层。同一来源可以出现多次，按追加顺序合并（后追加的更高）。
    pub fn push(&mut self, layer: ConfigLayer) -> &mut Self {
        self.layers.push(layer);
        self
    }

    /// 链式追加。
    pub fn with_layer(mut self, layer: ConfigLayer) -> Self {
        self.layers.push(layer);
        self
    }

    /// [`Self::push`] 的别名。
    pub fn add_layer(&mut self, layer: ConfigLayer) -> &mut Self {
        self.push(layer)
    }

    /// 已追加的层。
    pub fn layers(&self) -> &[ConfigLayer] {
        &self.layers
    }

    /// 按标准路径加载所有存在的层。
    ///
    /// 只加载实际存在的文件；不存在的层跳过（返回 `None`）。IO/语法错误收集到
    /// `errors`，不阻止其它层参与合并。
    pub fn load_standard(cwd: &Path) -> (Self, Vec<LayerLoadError>) {
        Self::load_standard_filtered(cwd, |_| true)
    }

    /// 同 [`Self::load_standard`]，但按来源过滤（例如项目未受信时跳过
    /// Project/Local 层）。
    pub fn load_standard_filtered(
        cwd: &Path,
        include: impl Fn(ConfigLayerSource) -> bool,
    ) -> (Self, Vec<LayerLoadError>) {
        let mut layers = Self::new();
        let mut errors = Vec::new();
        for source in ConfigLayerSource::ALL {
            if !include(source) {
                continue;
            }
            let Some(path) = source.standard_path(cwd) else {
                continue;
            };
            let mode = LayerLoadMode::default_for(source);
            match ConfigLayer::load_file(source, &path, mode) {
                Ok(Some(layer)) => {
                    layers.push(layer);
                }
                Ok(None) => {}
                Err(error) => errors.push(error),
            }
        }
        (layers, errors)
    }

    /// 合并所有层，应用 managed policy，产出有效配置。
    ///
    /// 层按 [`ConfigLayerSource::precedence`] 排序（同优先级保持追加顺序），
    /// 低层先、高层后。`origins` 只记录叶值与数组下标的胜出来源；容器 key（例如
    /// `permissions`）由 [`MergedConfig::origin`] 汇总其后代里优先级最高的来源。
    pub fn resolve(&self) -> MergedConfig {
        let mut ordered: Vec<&ConfigLayer> = self.layers.iter().collect();
        // 稳定排序：同来源/同优先级按追加顺序。
        ordered.sort_by_key(|layer| layer.source.precedence());

        let mut value = Value::Object(Map::new());
        let mut origins = BTreeMap::new();
        for layer in &ordered {
            merge_value_into(
                &mut value,
                &layer.value,
                "",
                layer.source,
                &mut origins,
                &mut Vec::new(),
            );
        }

        let mut policy = ManagedPolicy::from_layers(&ordered);
        let policy_conflicts = policy.conflicts();
        policy.apply(&mut value, &mut origins, &ordered);

        let diagnostics = ordered
            .iter()
            .flat_map(|layer| layer.diagnostics.iter().cloned())
            .collect();
        let layer_fingerprints = ordered
            .iter()
            .map(|layer| LayerFingerprint {
                source: layer.source,
                path: layer.path.clone(),
                fingerprint: layer_fingerprint(layer),
            })
            .collect();
        let fingerprint = aggregate_fingerprint(&ordered);

        MergedConfig {
            value,
            origins,
            layer_fingerprints,
            fingerprint,
            diagnostics,
            policy,
            policy_conflicts,
        }
    }

    /// [`Self::resolve`] 的别名。
    pub fn merge(&self) -> MergedConfig {
        self.resolve()
    }
}

/// 单层的指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerFingerprint {
    /// 来源。
    pub source: ConfigLayerSource,
    /// 文件（内存层为 `None`）。
    pub path: Option<PathBuf>,
    /// `sha256:<hex>`。
    pub fingerprint: String,
}

/// 合并后的有效配置。
#[derive(Debug, Clone, PartialEq)]
pub struct MergedConfig {
    value: Value,
    origins: BTreeMap<String, ConfigLayerSource>,
    layer_fingerprints: Vec<LayerFingerprint>,
    fingerprint: String,
    diagnostics: Vec<StrictDiagnostic>,
    policy: ManagedPolicy,
    policy_conflicts: Vec<ManagedPolicyConflict>,
}

impl MergedConfig {
    /// 合并后的有效 JSON 对象。
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// 逐 key path 的胜出来源（只含叶子与数组下标）。
    pub fn origins(&self) -> &BTreeMap<String, ConfigLayerSource> {
        &self.origins
    }

    /// 某个 key path 的胜出来源。
    ///
    /// 叶子/数组下标直接命中；容器 key（例如 `permissions`）返回其后代里优先级最高的
    /// 来源，便于 UI 回答“这个对象整体上是谁配的”。
    pub fn origin(&self, key_path: &str) -> Option<ConfigLayerSource> {
        if let Some(source) = self.origins.get(key_path) {
            return Some(*source);
        }
        let prefix = if key_path.is_empty() {
            String::new()
        } else {
            format!("{key_path}.")
        };
        self.origins
            .iter()
            .filter(|(candidate, _)| candidate.starts_with(&prefix))
            .map(|(_, source)| *source)
            .max()
    }

    /// 每层 canonical JSON 的 SHA256（按优先级从低到高）。
    pub fn layer_fingerprints(&self) -> &[LayerFingerprint] {
        &self.layer_fingerprints
    }

    /// 整体指纹：`sha256:<hex>`，对键序不敏感，改任一有效值都会变。
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// 各层严格诊断（按优先级从低到高）。
    pub fn diagnostics(&self) -> &[StrictDiagnostic] {
        &self.diagnostics
    }

    /// 生效的 managed policy。
    pub fn policy(&self) -> &ManagedPolicy {
        &self.policy
    }

    /// managed policy 内部冲突（allow 与 deny 同时命中的服务器）。
    pub fn policy_conflicts(&self) -> &[ManagedPolicyConflict] {
        &self.policy_conflicts
    }
}

// ---------------------------------------------------------------------------
// 合并
// ---------------------------------------------------------------------------

fn merge_value_into(
    base: &mut Value,
    overlay: &Value,
    path: &str,
    source: ConfigLayerSource,
    origins: &mut BTreeMap<String, ConfigLayerSource>,
    ancestors: &mut Vec<String>,
) {
    // 防止恶意/意外的深层嵌套耗尽栈：超过上限后整体替换并记录最深层来源。
    if !path.is_empty() && ancestors.len() >= MAX_MERGE_DEPTH {
        *base = overlay.clone();
        clear_descendant_origins(origins, path);
        record_tree_origins(origins, path, overlay, source);
        return;
    }
    if let (Value::Object(base_object), Value::Object(overlay_object)) = (&mut *base, overlay) {
        ancestors.push(path.to_string());
        for (key, overlay_value) in overlay_object {
            let child_path = child_key_path(path, key);
            match base_object.get_mut(key) {
                Some(base_value) => merge_value_into(
                    base_value,
                    overlay_value,
                    &child_path,
                    source,
                    origins,
                    ancestors,
                ),
                None => {
                    record_tree_origins(origins, &child_path, overlay_value, source);
                    base_object.insert(key.clone(), overlay_value.clone());
                }
            }
        }
        ancestors.pop();
        return;
    }
    if let (Value::Array(base_items), Value::Array(overlay_items)) = (&mut *base, overlay)
        && array_merge_mode(path) == ArrayMergeMode::ConcatDedup
    {
        for overlay_item in overlay_items {
            match base_items.iter().position(|item| item == overlay_item) {
                Some(index) => {
                    // 高层重复声明同一个元素：值不变，但胜出来源变成高层。
                    let item_path = index_key_path(path, index);
                    record_tree_origins(origins, &item_path, overlay_item, source);
                }
                None => {
                    let index = base_items.len();
                    base_items.push(overlay_item.clone());
                    let item_path = index_key_path(path, index);
                    record_tree_origins(origins, &item_path, overlay_item, source);
                }
            }
        }
        return;
    }
    *base = overlay.clone();
    clear_descendant_origins(origins, path);
    record_tree_origins(origins, path, overlay, source);
}

/// 合并深度的防护上限。配置嵌套远不该到这个深度；超过时退化为整体替换，
/// 避免递归栈溢出（与 shell 分析的 fail-closed 思路一致）。
const MAX_MERGE_DEPTH: usize = 128;

fn record_tree_origins(
    origins: &mut BTreeMap<String, ConfigLayerSource>,
    path: &str,
    value: &Value,
    source: ConfigLayerSource,
) {
    match value {
        // 对象/数组本身不记录，只记录叶子与数组下标；容器路径由
        // [`MergedConfig::origin`] 汇总其后代来源。
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = child_key_path(path, key);
                record_tree_origins(origins, &child_path, child, source);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let item_path = index_key_path(path, index);
                record_tree_origins(origins, &item_path, item, source);
            }
        }
        _ => {
            if !path.is_empty() {
                origins.insert(path.to_string(), source);
            }
        }
    }
}

fn clear_descendant_origins(origins: &mut BTreeMap<String, ConfigLayerSource>, path: &str) {
    if path.is_empty() {
        origins.clear();
        return;
    }
    let prefix = format!("{path}.");
    origins.retain(|candidate, _| !candidate.starts_with(&prefix));
}

// ---------------------------------------------------------------------------
// 指纹
// ---------------------------------------------------------------------------

/// canonical JSON：递归按 key 排序后序列化（数组顺序保留）。
///
/// `serde_json` 开启了 `preserve_order`，直接 `to_string` 会保留输入键序；指纹必须
/// 对键序不敏感，所以这里重建对象。
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let mut output = String::from("{");
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()));
                output.push(':');
                output.push_str(&canonical_json(&object[key]));
            }
            output.push('}');
            output
        }
        Value::Array(items) => {
            let mut output = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&canonical_json(item));
            }
            output.push(']');
            output
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut output = String::with_capacity(7 + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn layer_fingerprint(layer: &ConfigLayer) -> String {
    sha256_hex(&canonical_json(&layer.value))
}

fn aggregate_fingerprint(layers: &[&ConfigLayer]) -> String {
    let mut hasher = Sha256::new();
    for layer in layers {
        hasher.update(layer.source.as_str().as_bytes());
        hasher.update([0u8]);
        hasher.update(canonical_json(&layer.value).as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut output = String::from("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

// ---------------------------------------------------------------------------
// managed policy
// ---------------------------------------------------------------------------

/// managed policy 的一台 MCP 服务器规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerRule {
    /// 按服务器名精确匹配。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// 按启动命令数组精确匹配（stdio server）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_command: Option<Vec<String>>,
    /// 按 URL 通配匹配（含 `*`，例如 `https://*.example.com/*`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
}

impl McpServerRule {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            // 字符串简写按服务器名处理。
            Value::String(name) => Some(Self {
                server_name: Some(name.clone()),
                server_command: None,
                server_url: None,
            }),
            Value::Object(object) => {
                let server_name = object
                    .get("serverName")
                    .or_else(|| object.get("server_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let server_command = object
                    .get("serverCommand")
                    .or_else(|| object.get("server_command"))
                    .and_then(command_rule_value);
                let server_url = object
                    .get("serverUrl")
                    .or_else(|| object.get("server_url"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if server_name.is_none() && server_command.is_none() && server_url.is_none() {
                    return None;
                }
                Some(Self {
                    server_name,
                    server_command,
                    server_url,
                })
            }
            _ => None,
        }
    }

    /// 规则是否命中一台服务器。
    pub fn matches(
        &self,
        server_name: &str,
        server_command: Option<&[String]>,
        server_url: Option<&str>,
    ) -> bool {
        if let Some(name) = &self.server_name
            && name != server_name
        {
            return false;
        }
        if let Some(command) = &self.server_command {
            let Some(actual) = server_command else {
                return false;
            };
            if command.as_slice() != actual {
                return false;
            }
        }
        if let Some(pattern) = &self.server_url {
            let Some(actual) = server_url else {
                return false;
            };
            if !wildcard_match(pattern, actual) {
                return false;
            }
        }
        self.server_name.is_some() || self.server_command.is_some() || self.server_url.is_some()
    }

    /// 供冲突报告使用的可读标签。
    pub fn label(&self) -> String {
        if let Some(name) = &self.server_name {
            return name.clone();
        }
        if let Some(command) = &self.server_command {
            return command.join(" ");
        }
        self.server_url
            .clone()
            .unwrap_or_else(|| "<empty>".to_string())
    }
}

fn command_rule_value(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(text) => Some(text.split_whitespace().map(str::to_string).collect()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

/// MCP 服务器被策略判定后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerDecision {
    /// 策略明确允许。
    Allowed,
    /// 策略明确拒绝（deny 命中、空 allowlist、或 allow/deny 冲突）。
    Denied,
    /// 策略没有配置 allowlist；调用方沿用普通权限规则。
    Unspecified,
}

/// allow 与 deny 同时命中同一服务器时的冲突。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPolicyConflict {
    /// 服务器名或规则标签。
    pub server: String,
    /// 固定说明。
    pub message: String,
}

/// 管理员策略。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedPolicy {
    /// `allowManagedPermissionRulesOnly`。
    pub allow_managed_permission_rules_only: bool,
    /// `allowManagedHooksOnly`。
    pub allow_managed_hooks_only: bool,
    /// `disableBypassPermissionsMode`。
    pub disable_bypass_permissions_mode: bool,
    /// `allowedMcpServers`；`None` 表示策略未配置 allowlist。
    pub allowed_mcp_servers: Option<Vec<McpServerRule>>,
    /// `deniedMcpServers`。`from_layers` 先收集管理员层；`apply` 再把有效配置里
    /// 所有层（含用户/项目）的 deny 条目并进来，所以 resolve 之后的策略判定满足
    /// “deny 永远合并”。
    pub denied_mcp_servers: Vec<McpServerRule>,
    /// deny 配置存在却无法解析（不是数组，或数组里有坏条目）时为 `true`：无法证明
    /// 某台服务器被允许，按“全部拒绝”fail-closed，而不是把限制当成没写。
    pub deny_all_mcp_servers: bool,
}

impl ManagedPolicy {
    /// 从管理员层收集策略。层顺序应为低 → 高。
    pub fn from_layers(layers: &[&ConfigLayer]) -> Self {
        let mut policy = Self::default();
        for layer in layers
            .iter()
            .copied()
            .filter(|layer| layer.source.is_policy_source())
        {
            let next = Self::from_value(&layer.value);
            policy.allow_managed_permission_rules_only |= next.allow_managed_permission_rules_only;
            policy.allow_managed_hooks_only |= next.allow_managed_hooks_only;
            policy.disable_bypass_permissions_mode |= next.disable_bypass_permissions_mode;
            policy.deny_all_mcp_servers |= next.deny_all_mcp_servers;
            if next.allowed_mcp_servers.is_some() {
                policy.allowed_mcp_servers = next.allowed_mcp_servers;
            }
            for rule in next.denied_mcp_servers {
                push_unique(&mut policy.denied_mcp_servers, rule);
            }
        }
        policy
    }

    /// 从一层 JSON 解析策略值。未知/类型不符的值按“未配置”处理（严格诊断由层负责）。
    pub fn from_value(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let permissions = object.get("permissions").and_then(Value::as_object);
        let bool_key = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| object.get(*name))
                .or_else(|| {
                    permissions.and_then(|permissions| {
                        names.iter().find_map(|name| permissions.get(*name))
                    })
                })
                .is_some_and(is_true_or_disable)
        };
        let denied = mcp_rules_key(
            object,
            permissions,
            &["deniedMcpServers", "denied_mcp_servers"],
        )
        .unwrap_or_default();
        Self {
            allow_managed_permission_rules_only: bool_key(&[
                "allowManagedPermissionRulesOnly",
                "allow_managed_permission_rules_only",
            ]),
            allow_managed_hooks_only: bool_key(&[
                "allowManagedHooksOnly",
                "allow_managed_hooks_only",
            ]),
            disable_bypass_permissions_mode: bool_key(&[
                "disableBypassPermissionsMode",
                "disable_bypass_permissions_mode",
            ]),
            allowed_mcp_servers: match mcp_rules_key(
                object,
                permissions,
                &["allowedMcpServers", "allowed_mcp_servers"],
            ) {
                None => None,
                // 值是数组以外的东西：作者显然想限制，但无法解析出任何规则，
                // 按空 allowlist（全部拒绝）处理。
                Some(rules) if rules.shape_invalid => Some(Vec::new()),
                Some(rules) => Some(rules.rules),
            },
            denied_mcp_servers: denied.rules,
            deny_all_mcp_servers: denied.shape_invalid || denied.invalid_entries,
        }
    }

    /// 策略是否启用了任意一项“只读 managed”限制。
    pub fn restricts_sources(&self) -> bool {
        self.allow_managed_permission_rules_only || self.allow_managed_hooks_only
    }

    /// 判定一台 MCP 服务器。
    ///
    /// 顺序：deny → allowlist。空 allowlist 表示“全部拒绝”（与 Claude Code 一致），
    /// allow 与 deny 冲突时 deny 胜。
    pub fn mcp_server_decision(
        &self,
        server_name: &str,
        server_command: Option<&[String]>,
        server_url: Option<&str>,
    ) -> McpServerDecision {
        if self.deny_all_mcp_servers
            || self
                .denied_mcp_servers
                .iter()
                .any(|rule| rule.matches(server_name, server_command, server_url))
        {
            return McpServerDecision::Denied;
        }
        match &self.allowed_mcp_servers {
            None => McpServerDecision::Unspecified,
            Some(rules) if rules.is_empty() => McpServerDecision::Denied,
            Some(rules) => {
                if rules
                    .iter()
                    .any(|rule| rule.matches(server_name, server_command, server_url))
                {
                    McpServerDecision::Allowed
                } else {
                    McpServerDecision::Denied
                }
            }
        }
    }

    /// 找出 allow 与 deny 同时命中的规则（用于冲突拒绝与诊断）。
    pub fn conflicts(&self) -> Vec<ManagedPolicyConflict> {
        let Some(allowed) = &self.allowed_mcp_servers else {
            return Vec::new();
        };
        let mut conflicts = Vec::new();
        for allow in allowed {
            for deny in &self.denied_mcp_servers {
                if rules_overlap(allow, deny) {
                    conflicts.push(ManagedPolicyConflict {
                        server: allow.label(),
                        message: "server is present in both allowedMcpServers and deniedMcpServers; deniedMcpServers wins".to_string(),
                    });
                }
            }
        }
        conflicts
    }

    /// 把策略应用到合并后的有效配置。
    ///
    /// 会就地更新 `denied_mcp_servers`：决策用的 deny 集合包含**所有层**的条目，
    /// 而不只是管理员层。`policy_conflicts()` 在调用本方法之前计算，所以冲突列表
    /// 仍然只描述管理员策略内部的 allow/deny 冲突。
    fn apply(
        &mut self,
        value: &mut Value,
        origins: &mut BTreeMap<String, ConfigLayerSource>,
        layers: &[&ConfigLayer],
    ) {
        let Some(object) = value.as_object_mut() else {
            return;
        };
        let canonical_source = layers
            .iter()
            .rev()
            .find(|layer| layer.source.is_policy_source())
            .map(|layer| layer.source);

        if self.disable_bypass_permissions_mode {
            let permissions = object
                .entry("permissions".to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if !permissions.is_object() {
                *permissions = Value::Object(Map::new());
            }
            if let Some(permissions) = permissions.as_object_mut() {
                permissions.insert(
                    "disableBypassPermissionsMode".to_string(),
                    Value::Bool(true),
                );
                if let Some(source) = canonical_source {
                    origins.insert(
                        "permissions.disableBypassPermissionsMode".to_string(),
                        source,
                    );
                }
            }
        }

        if self.allow_managed_permission_rules_only {
            self.restrict_permission_rules(object, origins, layers);
        }

        if self.allow_managed_hooks_only {
            let managed_hooks = merge_policy_subtree(layers, "hooks");
            match managed_hooks {
                Some(hooks) => {
                    object.insert("hooks".to_string(), hooks.clone());
                    clear_descendant_origins(origins, "hooks");
                    if let Some(source) = canonical_source {
                        record_tree_origins(origins, "hooks", &hooks, source);
                    }
                }
                None => {
                    object.remove("hooks");
                    clear_descendant_origins(origins, "hooks");
                }
            }
        }

        // denied 永远合并：把 camel/snake 两种拼写从**所有层**收集到 snake_case 的
        // 规范键。随后让策略决策使用合并后的 deny 集合，这样用户/项目层写下的 deny
        // 也能否决管理员 allowlist，而不是只有管理员自己的 deny 生效。
        let mut denied = Vec::new();
        for key in ["deniedMcpServers", "denied_mcp_servers"] {
            if let Some(value) = object.get(key)
                && !value.is_null()
            {
                match value.as_array() {
                    Some(items) => {
                        for item in items {
                            push_unique(&mut denied, item.clone());
                        }
                    }
                    // 键存在但不是数组：语义不可知，fail-closed。
                    None => self.deny_all_mcp_servers = true,
                }
            }
            object.remove(key);
        }
        // 兼容写法：policy 键也可以放在 `permissions` 对象里（不删除原键，只并进
        // 规范键，避免破坏调用方对 permissions 子树的其它读取）。
        if let Some(permissions) = object.get("permissions").and_then(Value::as_object) {
            for key in ["deniedMcpServers", "denied_mcp_servers"] {
                if let Some(value) = permissions.get(key)
                    && !value.is_null()
                {
                    match value.as_array() {
                        Some(items) => {
                            for item in items {
                                push_unique(&mut denied, item.clone());
                            }
                        }
                        None => self.deny_all_mcp_servers = true,
                    }
                }
            }
        }
        for item in &denied {
            match McpServerRule::from_value(item) {
                Some(rule) => push_unique(&mut self.denied_mcp_servers, rule),
                // 某层写了无法理解的 deny 条目：不能当它不存在。
                None => self.deny_all_mcp_servers = true,
            }
        }
        clear_descendant_origins(origins, "deniedMcpServers");
        clear_descendant_origins(origins, "denied_mcp_servers");
        if !denied.is_empty() {
            let denied_value = Value::Array(denied);
            if let Some(source) = canonical_source {
                record_tree_origins(origins, "denied_mcp_servers", &denied_value, source);
            }
            object.insert("denied_mcp_servers".to_string(), denied_value);
        }

        // allowlist 只采信管理员层；deny 用上面合并后的**全层**集合过滤，保证
        // 有效 allowlist 与 `mcp_server_decision` 的 deny-first 结果一致。
        if self.allowed_mcp_servers.is_some() {
            let managed_allowed =
                merge_policy_mcp_list(layers, &["allowedMcpServers", "allowed_mcp_servers"]);
            // 冲突拒绝：allow 与 deny 同时命中的条目从有效 allowlist 中移除。
            let filtered = filter_conflicting_allowed(&managed_allowed, &self.denied_mcp_servers);
            object.insert("allowed_mcp_servers".to_string(), filtered.clone());
            object.remove("allowedMcpServers");
            clear_descendant_origins(origins, "allowed_mcp_servers");
            clear_descendant_origins(origins, "allowedMcpServers");
            if let Some(source) = canonical_source {
                record_tree_origins(origins, "allowed_mcp_servers", &filtered, source);
            }
        }

        // policy 布尔标志也不能被 project 层写回 false：生效值里的规范键强制为 true，
        // 存在 snake_case 别名时同步，避免两种拼写互相矛盾。
        for (canonical, snake, enabled) in [
            (
                "allowManagedPermissionRulesOnly",
                "allow_managed_permission_rules_only",
                self.allow_managed_permission_rules_only,
            ),
            (
                "allowManagedHooksOnly",
                "allow_managed_hooks_only",
                self.allow_managed_hooks_only,
            ),
        ] {
            if !enabled {
                continue;
            }
            object.insert(canonical.to_string(), Value::Bool(true));
            if object.contains_key(snake) {
                object.insert(snake.to_string(), Value::Bool(true));
            }
            if let Some(source) = canonical_source {
                origins.insert(canonical.to_string(), source);
                if object.contains_key(snake) {
                    origins.insert(snake.to_string(), source);
                }
            }
        }
    }

    fn restrict_permission_rules(
        &self,
        object: &mut Map<String, Value>,
        origins: &mut BTreeMap<String, ConfigLayerSource>,
        layers: &[&ConfigLayer],
    ) {
        let permissions = object
            .entry("permissions".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !permissions.is_object() {
            *permissions = Value::Object(Map::new());
        }
        let Some(permissions) = permissions.as_object_mut() else {
            return;
        };

        // 有效 deny = 所有层 deny 的并集（通用合并已经按 concat 表累加）。
        let all_deny = permissions
            .get("deny")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // allow/ask 只采信管理员层。
        for key in ["allow", "ask"] {
            let managed = merge_policy_subtree(layers, &format!("permissions.{key}"));
            match managed {
                Some(value) => {
                    permissions.insert(key.to_string(), value.clone());
                    clear_descendant_origins(origins, &format!("permissions.{key}"));
                    if let Some(source) = layers
                        .iter()
                        .rev()
                        .find(|layer| layer.source.is_policy_source())
                        .map(|layer| layer.source)
                    {
                        record_tree_origins(origins, &format!("permissions.{key}"), &value, source);
                    }
                }
                None => {
                    permissions.remove(key);
                    clear_descendant_origins(origins, &format!("permissions.{key}"));
                }
            }
        }
        permissions.insert("deny".to_string(), Value::Array(all_deny));

        // rules：管理员层的全部规则 + 其它层的 deny 规则。
        let mut rules = Vec::new();
        if let Some(managed_rules) = merge_policy_subtree(layers, "permissions.rules")
            .and_then(|value| value.as_array().cloned())
        {
            rules.extend(managed_rules);
        }
        if let Some(all_rules) = permissions.get("rules").and_then(Value::as_array).cloned() {
            for rule in all_rules {
                if rule
                    .get("decision")
                    .and_then(Value::as_str)
                    .is_some_and(|decision| decision.eq_ignore_ascii_case("deny"))
                {
                    push_unique(&mut rules, rule);
                }
            }
        }
        let rules_value = Value::Array(rules);
        permissions.insert("rules".to_string(), rules_value.clone());
        clear_descendant_origins(origins, "permissions.rules");
        if let Some(source) = layers
            .iter()
            .rev()
            .find(|layer| layer.source.is_policy_source())
            .map(|layer| layer.source)
        {
            record_tree_origins(origins, "permissions.rules", &rules_value, source);
        }
    }
}

/// 合并管理员层里的某个子树（不存在时返回 `None`）。
fn merge_policy_subtree(layers: &[&ConfigLayer], key: &str) -> Option<Value> {
    let mut value = Value::Null;
    let mut any = false;
    for layer in layers
        .iter()
        .copied()
        .filter(|layer| layer.source.is_policy_source())
    {
        if let Some(field) = nested_value(&layer.value, key) {
            any = true;
            let mut origins = BTreeMap::new();
            merge_value_into(
                &mut value,
                field,
                key,
                layer.source,
                &mut origins,
                &mut Vec::new(),
            );
        }
    }
    any.then_some(value)
}

/// 按 dotted path 取嵌套值（只用于本模块的固定 key，不做转义）。
fn nested_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, key| current.get(key))
}

/// 合并管理员层的 MCP allowlist；接受 camelCase 与 snake_case 两种键。
fn merge_policy_mcp_list(layers: &[&ConfigLayer], keys: &[&str]) -> Value {
    let mut items = Vec::new();
    for layer in layers
        .iter()
        .copied()
        .filter(|layer| layer.source.is_policy_source())
    {
        // 与 `ManagedPolicy::from_value` 一致：顶层键优先，其次接受放在
        // `permissions` 对象里的兼容写法。
        let field = keys
            .iter()
            .find_map(|key| layer.value.get(*key))
            .or_else(|| {
                layer
                    .value
                    .get("permissions")
                    .and_then(|permissions| keys.iter().find_map(|key| permissions.get(*key)))
            });
        let Some(field) = field else {
            continue;
        };
        let Some(list) = field.as_array() else {
            continue;
        };
        for item in list {
            push_unique(&mut items, item.clone());
        }
    }
    Value::Array(items)
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn is_true_or_disable(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(text) => {
            text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("disable")
        }
        _ => false,
    }
}

/// 解析出来的 MCP 规则列表，附带失败信息（供调用方决定 fail-closed 的方式）。
#[derive(Debug, Default)]
struct McpRules {
    rules: Vec<McpServerRule>,
    /// 值存在但不是数组：作者意图无法解析。
    shape_invalid: bool,
    /// 数组里有无法解析的条目。
    invalid_entries: bool,
}

fn mcp_rules_key(
    object: &Map<String, Value>,
    permissions: Option<&Map<String, Value>>,
    names: &[&str],
) -> Option<McpRules> {
    let value = names
        .iter()
        .find_map(|name| object.get(*name))
        .or_else(|| {
            permissions.and_then(|permissions| names.iter().find_map(|name| permissions.get(*name)))
        })
        // 与其它 settings 语义一致：`null` 表示「没有配置」，不是空列表。
        .filter(|value| !value.is_null())?;
    let Some(items) = value.as_array() else {
        return Some(McpRules {
            shape_invalid: true,
            ..McpRules::default()
        });
    };
    let mut rules = McpRules::default();
    for item in items {
        match McpServerRule::from_value(item) {
            Some(rule) => rules.rules.push(rule),
            None => rules.invalid_entries = true,
        }
    }
    Some(rules)
}

fn rules_overlap(allowed: &McpServerRule, denied: &McpServerRule) -> bool {
    // 冲突只在两侧约束同一维度时可判定；不同维度（name vs command）无法证明是
    // 同一台服务器，不记为冲突。实际判定仍由 deny 规则逐条匹配完成。
    if let (Some(left), Some(right)) = (&allowed.server_name, &denied.server_name) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (&allowed.server_command, &denied.server_command) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (&allowed.server_url, &denied.server_url) {
        return wildcard_match(left, right) || wildcard_match(right, left);
    }
    false
}

/// 从 allowlist 里移除与 deny 冲突的条目。
fn filter_conflicting_allowed(allowed: &Value, denied: &[McpServerRule]) -> Value {
    let Value::Array(items) = allowed else {
        return allowed.clone();
    };
    Value::Array(
        items
            .iter()
            .filter(|item| {
                let Some(rule) = McpServerRule::from_value(item) else {
                    return true;
                };
                !denied.iter().any(|deny| rules_overlap(&rule, deny))
            })
            .cloned()
            .collect(),
    )
}

/// 线性时间的 `*` 通配匹配（`*` 匹配任意字符序列，含空串）。
///
/// 不用正则，避免多通配符模式的灾难性回溯。
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_text = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layer(source: ConfigLayerSource, value: Value) -> ConfigLayer {
        ConfigLayer::new(source, value)
    }

    #[test]
    fn higher_layer_wins_across_all_sources() {
        let mut layers = ConfigLayers::new();
        for source in ConfigLayerSource::ALL {
            layers.push(layer(
                source,
                json!({"theme": source.as_str(), "nested": {"value": source.as_str()}}),
            ));
        }
        let merged = layers.resolve();
        assert_eq!(merged.value()["theme"], json!("session"));
        assert_eq!(merged.value()["nested"]["value"], json!("session"));
        assert_eq!(merged.origin("theme"), Some(ConfigLayerSource::Session));
        assert_eq!(
            merged.origin("nested.value"),
            Some(ConfigLayerSource::Session)
        );
        assert_eq!(merged.origin("nested"), Some(ConfigLayerSource::Session));
    }

    #[test]
    fn objects_merge_recursively_and_scalars_override() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"permissions": {"mode": "default", "allow": ["read"]}, "theme": "dark"}),
            ))
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({"permissions": {"mode": "read-only"}}),
            ))
            .resolve();
        assert_eq!(merged.value()["permissions"]["mode"], json!("read-only"));
        assert_eq!(merged.value()["permissions"]["allow"], json!(["read"]));
        assert_eq!(merged.value()["theme"], json!("dark"));
        assert_eq!(
            merged.origin("permissions.mode"),
            Some(ConfigLayerSource::Project)
        );
        assert_eq!(
            merged.origin("permissions.allow"),
            Some(ConfigLayerSource::User)
        );
        assert_eq!(merged.origin("theme"), Some(ConfigLayerSource::User));
    }

    #[test]
    fn arrays_use_the_key_table() {
        assert_eq!(
            array_merge_mode("permissions.deny"),
            ArrayMergeMode::ConcatDedup
        );
        assert_eq!(
            array_merge_mode("permissions.rules"),
            ArrayMergeMode::ConcatDedup
        );
        assert_eq!(array_merge_mode("npmCommand"), ArrayMergeMode::Replace);
        assert_eq!(
            array_merge_mode("hooks.PreToolUse"),
            ArrayMergeMode::ConcatDedup
        );
        // 嵌套 `permissions` 里的 MCP 键也必须跨层累加，否则项目层会把 managed
        // deny 整体覆盖掉。
        for key in [
            "permissions.allowedMcpServers",
            "permissions.allowed_mcp_servers",
            "permissions.deniedMcpServers",
            "permissions.denied_mcp_servers",
        ] {
            assert_eq!(array_merge_mode(key), ArrayMergeMode::ConcatDedup, "{key}");
        }
        assert_eq!(array_merge_mode("extensions"), ArrayMergeMode::ConcatDedup);
        assert_eq!(
            array_merge_mode("modelThinkingLevels"),
            ArrayMergeMode::Replace
        );
    }

    #[test]
    fn concat_arrays_deduplicate_and_keep_order() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"permissions": {"deny": ["a", "b"], "addDirs": ["/one"]}}),
            ))
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({"permissions": {"deny": ["b", "c"], "addDirs": ["/two"]}}),
            ))
            .resolve();
        assert_eq!(
            merged.value()["permissions"]["deny"],
            json!(["a", "b", "c"])
        );
        assert_eq!(
            merged.value()["permissions"]["addDirs"],
            json!(["/one", "/two"])
        );
        // 重复元素 b 由高层重新声明，origin 指向 Project。
        assert_eq!(
            merged.origin("permissions.deny.1"),
            Some(ConfigLayerSource::Project)
        );
        assert_eq!(
            merged.origin("permissions.deny.0"),
            Some(ConfigLayerSource::User)
        );
    }

    #[test]
    fn replace_arrays_do_not_concatenate() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"npmCommand": ["npm", "install"]}),
            ))
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({"npmCommand": ["pnpm", "install"]}),
            ))
            .resolve();
        assert_eq!(merged.value()["npmCommand"], json!(["pnpm", "install"]));
        assert_eq!(
            merged.origin("npmCommand"),
            Some(ConfigLayerSource::Project)
        );
    }

    #[test]
    fn fingerprint_is_key_order_insensitive_and_value_sensitive() {
        let first = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"a": 1, "b": {"c": 2, "d": 3}}),
            ))
            .resolve();
        let second = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"b": {"d": 3, "c": 2}, "a": 1}),
            ))
            .resolve();
        assert_eq!(first.fingerprint(), second.fingerprint());
        let changed = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"a": 1, "b": {"c": 99, "d": 3}}),
            ))
            .resolve();
        assert_ne!(first.fingerprint(), changed.fingerprint());
        assert_eq!(first.layer_fingerprints().len(), 1);
        assert!(first.fingerprint().starts_with("sha256:"));
    }

    #[test]
    fn managed_policy_keys_are_not_overridden_by_project() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowManagedPermissionRulesOnly": true,
                    "allowManagedHooksOnly": true,
                    "disableBypassPermissionsMode": true,
                    "allowedMcpServers": [{"serverName": "docs"}],
                    "deniedMcpServers": [{"serverName": "evil"}]
                }),
            ))
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({
                    "allowManagedPermissionRulesOnly": false,
                    "allowManagedHooksOnly": false,
                    "disableBypassPermissionsMode": false,
                    "permissions": {"disableBypassPermissionsMode": false},
                    "allowedMcpServers": [{"serverName": "user"}],
                    "deniedMcpServers": [{"serverName": "user-denied"}]
                }),
            ))
            .resolve();
        let policy = merged.policy();
        assert!(policy.allow_managed_permission_rules_only);
        assert!(policy.allow_managed_hooks_only);
        assert!(policy.disable_bypass_permissions_mode);
        assert_eq!(
            merged.value()["permissions"]["disableBypassPermissionsMode"],
            json!(true)
        );
        // policy 布尔标志在生效值里也不能被 project 写回 false。
        assert_eq!(
            merged.value()["allowManagedPermissionRulesOnly"],
            json!(true)
        );
        assert_eq!(merged.value()["allowManagedHooksOnly"], json!(true));
        assert_eq!(
            merged.value()["allowed_mcp_servers"],
            json!([{"serverName": "docs"}])
        );
        // deny 永远合并：项目层新增的 deny 保留。
        assert_eq!(
            merged.value()["denied_mcp_servers"],
            json!([{"serverName": "evil"}, {"serverName": "user-denied"}])
        );
        // 合并后的 deny 也进入策略判定：用户/项目 deny 可以否决管理员 allowlist。
        assert_eq!(
            policy.mcp_server_decision("docs", None, None),
            McpServerDecision::Allowed
        );
        assert_eq!(
            policy.mcp_server_decision("user-denied", None, None),
            McpServerDecision::Denied
        );
    }

    #[test]
    fn non_managed_mcp_deny_wins_over_managed_allowlist() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowedMcpServers": [
                        {"serverName": "docs"},
                        {"serverName": "shared"}
                    ]
                }),
            ))
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({"deniedMcpServers": [{"serverName": "shared"}]}),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Allowed
        );
        assert_eq!(
            merged.policy().mcp_server_decision("shared", None, None),
            McpServerDecision::Denied,
            "project deny 必须能否决 managed allowlist（deny 永远合并）"
        );
        // 有效 allowlist 同步剔除冲突条目，避免 value() 与决策打架。
        assert_eq!(
            merged.value()["allowed_mcp_servers"],
            json!([{"serverName": "docs"}])
        );
        assert_eq!(
            merged.value()["denied_mcp_servers"],
            json!([{"serverName": "shared"}])
        );
    }

    #[test]
    fn nested_permission_mcp_policy_keys_are_applied() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "permissions": {
                        "allowedMcpServers": [{"serverName": "docs"}],
                        "deniedMcpServers": [{"serverName": "evil"}]
                    }
                }),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Allowed
        );
        assert_eq!(
            merged.policy().mcp_server_decision("evil", None, None),
            McpServerDecision::Denied
        );
        // 规范键是顶层 snake_case；嵌套写法不应导致空 allowlist（拒绝全部）。
        assert_eq!(
            merged.value()["allowed_mcp_servers"],
            json!([{"serverName": "docs"}])
        );
    }

    #[test]
    fn non_array_mcp_allowlist_denies_every_server() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                // 类型错误：作者显然想限制 MCP，不能退化成「没有配置」。
                json!({"allowedMcpServers": {"serverName": "docs"}}),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Denied
        );
        assert_eq!(
            merged.policy().mcp_server_decision("anything", None, None),
            McpServerDecision::Denied
        );
    }

    #[test]
    fn null_mcp_lists_are_treated_as_absent() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowedMcpServers": null,
                    "deniedMcpServers": null
                }),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Unspecified,
            "null 是「未配置」，不能变成「拒绝全部」"
        );
        assert!(!merged.policy().deny_all_mcp_servers);
    }

    #[test]
    fn unparsable_mcp_deny_entries_deny_every_server() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Project,
                json!({"deniedMcpServers": [42]}),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Denied,
            "无法解析的 deny 条目必须 fail-closed，而不是被忽略"
        );
    }

    #[test]
    fn managed_permission_rules_only_keeps_deny_from_all_layers() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"permissions": {
                    "rules": [
                        {"tool": "read", "decision": "allow"},
                        {"command": ["curl"], "decision": "deny"}
                    ],
                    "allow": ["read"],
                    "deny": ["curl"]
                }}),
            ))
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowManagedPermissionRulesOnly": true,
                    "permissions": {
                        "rules": [{"tool": "bash", "decision": "allow"}],
                        "allow": ["bash"],
                        "ask": ["write"]
                    }
                }),
            ))
            .resolve();
        let permissions = &merged.value()["permissions"];
        // allow/ask 只来自 managed。
        assert_eq!(permissions["allow"], json!(["bash"]));
        assert_eq!(permissions["ask"], json!(["write"]));
        // rules：managed 全部 + 非 managed 的 deny。
        let rules = permissions["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().any(|rule| rule["tool"] == json!("bash")));
        assert!(rules.iter().any(|rule| rule["command"] == json!(["curl"])));
        // deny 仍然合并所有层。
        assert_eq!(permissions["deny"], json!(["curl"]));
        assert_eq!(
            merged.origin("permissions.allow"),
            Some(ConfigLayerSource::Managed)
        );
    }

    #[test]
    fn managed_hooks_only_drops_user_hooks() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::User,
                json!({"hooks": {"PreToolUse": [{"matcher": "bash"}]}}),
            ))
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowManagedHooksOnly": true,
                    "hooks": {"PreToolUse": [{"matcher": "read"}]}
                }),
            ))
            .resolve();
        assert_eq!(
            merged.value()["hooks"]["PreToolUse"],
            json!([{"matcher": "read"}])
        );
    }

    #[test]
    fn mcp_allow_deny_conflicts_reject_the_server() {
        let merged = ConfigLayers::new()
            .with_layer(layer(
                ConfigLayerSource::Managed,
                json!({
                    "allowedMcpServers": [{"serverName": "docs"}, {"serverName": "both"}],
                    "deniedMcpServers": [{"serverName": "both"}]
                }),
            ))
            .resolve();
        assert_eq!(
            merged.policy().mcp_server_decision("docs", None, None),
            McpServerDecision::Allowed
        );
        assert_eq!(
            merged.policy().mcp_server_decision("both", None, None),
            McpServerDecision::Denied
        );
        assert_eq!(
            merged.policy().mcp_server_decision("other", None, None),
            McpServerDecision::Denied
        );
        assert_eq!(merged.policy_conflicts().len(), 1);
        // 冲突条目从有效 allowlist 中移除。
        assert_eq!(
            merged.value()["allowed_mcp_servers"],
            json!([{"serverName": "docs"}])
        );
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let policy = ManagedPolicy::from_value(&json!({"allowedMcpServers": []}));
        assert_eq!(
            policy.mcp_server_decision("docs", None, None),
            McpServerDecision::Denied
        );
        let unset = ManagedPolicy::default();
        assert_eq!(
            unset.mcp_server_decision("docs", None, None),
            McpServerDecision::Unspecified
        );
    }

    #[test]
    fn strict_diagnostics_only_for_managed_layers() {
        let dir = tempfile::tempdir().expect("temp");
        let strict_path = dir.path().join("managed-settings.json");
        std::fs::write(
            &strict_path,
            "{\n  \"permissions\": {\n    \"mode\": \"default\",\n    \"allowed\": []\n  }\n}",
        )
        .expect("write");
        let strict = ConfigLayer::load_file(
            ConfigLayerSource::Managed,
            &strict_path,
            LayerLoadMode::default_for(ConfigLayerSource::Managed),
        )
        .expect("load")
        .expect("layer");
        assert_eq!(strict.diagnostics.len(), 1);
        assert_eq!(strict.diagnostics[0].line, 4);
        assert_eq!(strict.diagnostics[0].key_path, "permissions.allowed");

        let lenient_path = dir.path().join("settings.json");
        std::fs::write(
            &lenient_path,
            "{\"permissions\": {\"allowed\": []}, \"custom\": true}",
        )
        .expect("write");
        let lenient = ConfigLayer::load_file(
            ConfigLayerSource::User,
            &lenient_path,
            LayerLoadMode::default_for(ConfigLayerSource::User),
        )
        .expect("load")
        .expect("layer");
        assert!(lenient.diagnostics.is_empty());
    }

    #[test]
    fn syntax_errors_fail_the_layer_with_line_and_column() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("managed-settings.json");
        std::fs::write(&path, "{\n  \"a\": }").expect("write");
        let error =
            ConfigLayer::load_file(ConfigLayerSource::Managed, &path, LayerLoadMode::Strict)
                .expect_err("syntax error");
        match error {
            LayerLoadError::Parse(diagnostic) => {
                assert_eq!(diagnostic.kind, StrictDiagnosticKind::ParseError);
                assert_eq!(diagnostic.line, 2);
                assert!(diagnostic.column > 1);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn wildcard_matching_is_linear_and_anchored() {
        assert!(wildcard_match(
            "https://*.example.com/*",
            "https://a.example.com/x"
        ));
        assert!(wildcard_match("*", "anything"));
        assert!(!wildcard_match("git *", "github"));
        assert!(wildcard_match("git *", "git "));
        assert!(!wildcard_match(
            "https://a.example.com",
            "https://b.example.com"
        ));
    }

    #[test]
    fn layer_fingerprints_are_per_layer() {
        let merged = ConfigLayers::new()
            .with_layer(layer(ConfigLayerSource::User, json!({"a": 1})))
            .with_layer(layer(ConfigLayerSource::Project, json!({"b": 2})))
            .resolve();
        let fingerprints = merged.layer_fingerprints();
        assert_eq!(fingerprints.len(), 2);
        assert_eq!(fingerprints[0].source, ConfigLayerSource::User);
        assert_eq!(fingerprints[1].source, ConfigLayerSource::Project);
        assert_ne!(fingerprints[0].fingerprint, fingerprints[1].fingerprint);
    }
}
