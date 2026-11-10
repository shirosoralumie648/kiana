//! 子代理工具命名空间与「只能收窄」的权限集合。
//!
//! 背景：kiana 的工具来自三条互不相同的命名体系——内建工具（`read`/`bash`/…）、
//! MCP 工具（`mcp__<server>__<tool>`）与扩展工具（裸名字或 `plugin:<plugin>:<tool>`）。
//! 如果子代理的 allow/deny 列表只对裸字符串做前缀匹配，`mcp__foo__read` 与内建
//! `read` 会互相污染，也无法表达「只允许某个 MCP server」。
//!
//! 本模块先做两件事：
//!
//! 1. 把任意工具名解析成带命名空间的 [`CanonicalToolId`]（`builtin` / `mcp` /
//!    `plugin` / `extension`）；展示形式固定为 `builtin:read` 这种三段式，便于日志
//!    与测试断言。
//! 2. 用有限集合表达权限：[`ToolPermissionSet`] 的 `None` 表示「继承全部」，具体集合
//!    表示精确的允许列表。两个集合求交即「父代理 ∩ 子代理请求」，结果永远不会比任一
//!    输入更宽——这是子代理权限收窄的核心不变量。
//!
//! 注意：本模块只做名字与集合运算，不执行权限规则；真正的决策在
//! `kiana-permissions` 的 `PermissionEngine` 与 [`super::permissions`] 里。

use std::collections::BTreeSet;

use kiana_core::mcp_servers::is_mcp_tool_name;
use kiana_tools::ToolName;
use serde::{Deserialize, Serialize};

/// 工具命名空间。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolNamespace {
    /// 内建工具（`read`、`bash`、`edit` 等）。
    Builtin,
    /// `mcp__<server>__<tool>`。
    Mcp(String),
    /// 内置的 MCP 资源类工具（`list_mcp_resources` 等）。
    ///
    /// 展示为 `builtin:<tool>`，不占用 `mcp:resources:*` 命名空间：这些工具由
    /// 内建扩展提供，而真实存在名为 `resources` 的 MCP server 时，它的工具
    /// （`mcp__resources__<tool>`）必须能独立授权。共享展示名会让
    /// `mcp:resources:*` 同时放行两者，违背「只能收窄」。
    McpResources,
    /// 扩展/插件工具：`plugin:<plugin>:<tool>` 或 `plugin__<plugin>__<tool>`。
    Plugin(String),
    /// 无法归类的扩展工具（裸名字）。
    Extension,
}

impl ToolNamespace {
    /// 稳定前缀（用于展示与模式匹配）。
    pub fn prefix(&self) -> &str {
        match self {
            Self::Builtin | Self::McpResources => "builtin",
            Self::Mcp(_) => "mcp",
            Self::Plugin(_) => "plugin",
            Self::Extension => "ext",
        }
    }
}

/// 规范化的工具标识。
///
/// 两个工具名解析后 `namespace + name` 相同即视为同一个工具；原始拼写
/// （`mcp__foo__bar` vs `mcp:foo:bar`）不参与相等性，避免同一工具出现两条权限记录。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalToolId {
    /// 命名空间。
    pub namespace: ToolNamespace,
    /// 命名空间内的工具名；MCP 资源工具使用原始名字。
    pub name: String,
}

impl CanonicalToolId {
    /// 解析任意工具名。
    ///
    /// 规则按优先级：
    /// 1. `mcp__<server>__<tool>` → [`ToolNamespace::Mcp`]；
    /// 2. 内置 MCP 资源类工具（`list_mcp_resources` 等）→ [`ToolNamespace::McpResources`]；
    /// 3. `plugin:<plugin>:<tool>` / `plugin__<plugin>__<tool>` → [`ToolNamespace::Plugin`]；
    /// 4. 8 个内建工具名 → [`ToolNamespace::Builtin`]；
    /// 5. 其余 → [`ToolNamespace::Extension`]。
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("mcp__") {
            let (server, tool) = match rest.split_once("__") {
                Some((server, tool)) => (server, tool),
                None => (rest, ""),
            };
            return Self {
                namespace: ToolNamespace::Mcp(server.to_string()),
                name: tool.to_string(),
            };
        }
        if is_mcp_tool_name(raw) {
            return Self {
                namespace: ToolNamespace::McpResources,
                name: raw.to_string(),
            };
        }
        if let Some(rest) = raw.strip_prefix("plugin:") {
            let (plugin, tool) = match rest.split_once(':') {
                Some((plugin, tool)) => (plugin, tool),
                None => (rest, ""),
            };
            return Self {
                namespace: ToolNamespace::Plugin(plugin.to_string()),
                name: tool.to_string(),
            };
        }
        if let Some(rest) = raw.strip_prefix("plugin__") {
            let (plugin, tool) = match rest.split_once("__") {
                Some((plugin, tool)) => (plugin, tool),
                None => (rest, ""),
            };
            return Self {
                namespace: ToolNamespace::Plugin(plugin.to_string()),
                name: tool.to_string(),
            };
        }
        if ToolName::ALL.iter().any(|name| name.as_str() == raw)
            || raw == kiana_tools::APPLY_PATCH_TOOL_NAME
        {
            return Self {
                namespace: ToolNamespace::Builtin,
                name: raw.to_string(),
            };
        }
        Self {
            namespace: ToolNamespace::Extension,
            name: raw.to_string(),
        }
    }

    /// 三段式展示名：`builtin:read` / `mcp:server:tool` / `plugin:p:tool` / `ext:name`。
    pub fn display(&self) -> String {
        match &self.namespace {
            ToolNamespace::Builtin => format!("builtin:{}", self.name),
            ToolNamespace::Mcp(server) => {
                if self.name.is_empty() {
                    format!("mcp:{server}")
                } else {
                    format!("mcp:{server}:{}", self.name)
                }
            }
            ToolNamespace::McpResources => format!("builtin:{}", self.name),
            ToolNamespace::Plugin(plugin) => {
                if self.name.is_empty() {
                    format!("plugin:{plugin}")
                } else {
                    format!("plugin:{plugin}:{}", self.name)
                }
            }
            ToolNamespace::Extension => format!("ext:{}", self.name),
        }
    }

    /// 是否是内建工具（权限请求里用于 `PermissionRequest::from_tool_call_with` 的
    /// `builtin` 标志）。
    pub fn is_builtin(&self) -> bool {
        matches!(
            self.namespace,
            ToolNamespace::Builtin | ToolNamespace::McpResources
        )
    }

    /// 匹配配置里的原始工具名时使用的字符串。
    ///
    /// 内置 MCP 资源工具（`read_mcp_resource` 等）在模型面前的原始名就是裸名；
    /// 它们的规范展示名是 `builtin:<裸名>`，[`Self::raw_alias`] 会补出不存在的
    /// `mcp__resources__<裸名>`，导致 `tools: read_mcp_resource` 之类的允许列表
    /// 静默失效。只有内置资源命名空间用裸名匹配：真实 server 名为 `resources` 时，
    /// 即使工具名恰好也是 `read_mcp_resource`，也必须用 `mcp__resources__...` 别名。
    pub fn raw_name(&self) -> String {
        match &self.namespace {
            ToolNamespace::McpResources => self.name.clone(),
            _ => self.raw_alias(),
        }
    }

    /// 原始工具名的规范近似形式：MCP 用 `mcp__<server>__<tool>`，插件用
    /// `plugin__<plugin>__<tool>`。用于在没有原始字符串时匹配既有配置里的写法。
    pub fn raw_alias(&self) -> String {
        match &self.namespace {
            ToolNamespace::Builtin | ToolNamespace::Extension => self.name.clone(),
            ToolNamespace::Mcp(server) => format!("mcp__{server}__{}", self.name),
            ToolNamespace::McpResources => format!("mcp__resources__{}", self.name),
            ToolNamespace::Plugin(plugin) => format!("plugin__{plugin}__{}", self.name),
        }
    }
}

impl std::fmt::Display for CanonicalToolId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display())
    }
}

/// 一条工具允许/拒绝模式。
///
/// 模式对 **解析后展示名 + 原始名** 做双重通配（`*` 匹配任意字符，包括 `:`），
/// 因此以下写法都成立：
///
/// - `*`：全部；
/// - `read`：原始名精确匹配（内建工具）；
/// - `builtin:*`：全部内建工具；
/// - `mcp:github:*`：某个 MCP server；
/// - `mcp__github__*`：原始名字前缀（与 `kiana-permissions` 的规则写法兼容）；
/// - `plugin:ci:*`：某个插件。
///
/// 裸名字**不**匹配其它命名空间里的同名工具：`read` 不会放行
/// `mcp__filesystem__read`，否则 allowlist 会被一个未声明的 MCP 工具绕过
/// （也与参照实现按原始名解析工具列表的行为一致）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolPattern(pub String);

impl ToolPattern {
    /// 构造模式。
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 模式原文。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 是否为「全部」。
    pub fn is_all(&self) -> bool {
        let trimmed = self.0.trim();
        trimmed == "*"
    }

    /// 判断某个工具是否命中本条模式。
    ///
    /// `raw_name` 是模型/扩展实际使用的工具名（如 `mcp__github__list_issues`）；
    /// 展示名由 [`CanonicalToolId::display`] 生成。
    pub fn matches(&self, id: &CanonicalToolId, raw_name: &str) -> bool {
        if self.is_all() {
            return true;
        }
        let pattern = self.0.trim();
        if wildcard_match(pattern, &id.display()) {
            return true;
        }
        // 原始名匹配让 `mcp__github__*` 这类既有配置继续有效。
        wildcard_match(pattern, raw_name)
    }
}

impl From<&str> for ToolPattern {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// 子代理定义里的 `tools` 字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelection {
    /// `*` 或未设置：继承父代理的全部工具。
    All,
    /// 显式允许列表（按 [`ToolPattern`] 匹配）。
    Tools(Vec<ToolPattern>),
}

impl ToolSelection {
    /// 解析 frontmatter 的字符串/数组形式。
    ///
    /// 字符串既可写 `"read, grep"` 也可写 `"read grep"`；单个 `*`（允许与其它项
    /// 同时出现）表示「继承全部」。
    pub fn parse(raw: &str) -> Self {
        let tokens: Vec<&str> = raw
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect();
        // 空字符串是“显式声明了 tools 但没有条目”：参照实现把它解析成空列表
        // （没有工具），而不是继承父代理的全部工具。
        if tokens.is_empty() {
            return Self::Tools(Vec::new());
        }
        if tokens.contains(&"*") {
            return Self::All;
        }
        Self::Tools(tokens.into_iter().map(ToolPattern::new).collect())
    }

    /// 从数组解析。
    ///
    /// 显式空数组表示「没有任何工具」（与缺省/`null` 的「继承全部」区分）；
    /// 单个 `*`（可与其它的项同时出现）仍表示继承全部。
    pub fn from_items(items: impl IntoIterator<Item = String>) -> Self {
        let mut patterns = Vec::new();
        for item in items {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "*" {
                return Self::All;
            }
            patterns.push(ToolPattern::new(trimmed));
        }
        Self::Tools(patterns)
    }

    /// 判断某个工具是否在允许列表内。
    pub fn allows(&self, id: &CanonicalToolId, raw_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Tools(patterns) => patterns.iter().any(|pattern| pattern.matches(id, raw_name)),
        }
    }
}

/// 收窄后的工具允许集合。
///
/// `allowed = None` 表示「继承父代理的全部工具」；`Some(set)` 是精确集合。
/// 对有限集合求交是封闭的，因此 `intersect` 的结果不需要再展开模式。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPermissionSet {
    allowed: Option<BTreeSet<CanonicalToolId>>,
}

impl ToolPermissionSet {
    /// 不限制（继承父代理）。
    pub fn all() -> Self {
        Self { allowed: None }
    }

    /// 显式集合。
    pub fn from_ids(ids: impl IntoIterator<Item = CanonicalToolId>) -> Self {
        Self {
            allowed: Some(ids.into_iter().collect()),
        }
    }

    /// 从父工具表与请求的 allowlist 计算子代理的有效集合。
    ///
    /// 父集合先与父工具表求交（`parent_allowed` 为 `None` 时取父工具表的全部），
    /// 再按 `requested` 过滤。结果永远是父集合的子集。
    pub fn resolve(
        parent_allowed: &Self,
        parent_tools: impl IntoIterator<Item = CanonicalToolId>,
        requested: Option<&ToolSelection>,
    ) -> Self {
        let universe: BTreeSet<CanonicalToolId> = parent_tools.into_iter().collect();
        let parent_set = match &parent_allowed.allowed {
            None => universe.clone(),
            Some(allowed) => allowed.intersection(&universe).cloned().collect(),
        };
        let requested_set = match requested {
            None => parent_set.clone(),
            Some(ToolSelection::All) => parent_set.clone(),
            Some(ToolSelection::Tools(patterns)) => parent_set
                .into_iter()
                .filter(|id| {
                    patterns
                        .iter()
                        .any(|pattern| pattern.matches(id, &id.raw_name()))
                })
                .collect(),
        };
        Self {
            allowed: Some(requested_set),
        }
    }

    /// 两个集合求交：`None` 视为全集。
    ///
    /// 这是「子代理只能收窄」的形式化定义——返回值对两个输入都满足包含关系：
    /// `result ⊆ self` 且 `result ⊆ other`。
    pub fn intersect(&self, other: &Self) -> Self {
        match (&self.allowed, &other.allowed) {
            (None, None) => Self::all(),
            (Some(set), None) | (None, Some(set)) => Self {
                allowed: Some(set.clone()),
            },
            (Some(left), Some(right)) => Self {
                allowed: Some(left.intersection(right).cloned().collect()),
            },
        }
    }

    /// 是否允许指定工具。
    pub fn allows(&self, id: &CanonicalToolId) -> bool {
        match &self.allowed {
            None => true,
            Some(set) => set.contains(id),
        }
    }

    /// 显式集合（`None` 表示不限制）。
    pub fn ids(&self) -> Option<&BTreeSet<CanonicalToolId>> {
        self.allowed.as_ref()
    }

    /// 是否比另一个集合更窄（用于测试与诊断）。
    pub fn is_subset_of(&self, other: &Self) -> bool {
        match (&self.allowed, &other.allowed) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(left), Some(right)) => left.is_subset(right),
        }
    }
}

/// `*` 通配匹配（`*` 匹配任意字符序列；没有 `?` 与转义）。
///
/// 实现为两指针贪心回溯：`star` 记住最近一个 `*` 的位置，失配时只回溯到那里。
/// 工具名都很短，不需要正则编译开销。
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            star_text = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            star_text += 1;
            ti = star_text;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

impl ToolNamespace {
    /// 是否为 MCP 命名空间（测试与诊断用）。
    pub fn is_mcp(&self) -> bool {
        matches!(self, Self::Mcp(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(raw: &str) -> CanonicalToolId {
        CanonicalToolId::parse(raw)
    }

    #[test]
    fn parses_builtin_mcp_plugin_and_extension_names() {
        assert_eq!(id("read").namespace, ToolNamespace::Builtin);
        assert_eq!(id("apply_patch").namespace, ToolNamespace::Builtin);
        assert_eq!(
            id("mcp__github__list_issues").namespace,
            ToolNamespace::Mcp("github".into())
        );
        assert_eq!(id("mcp__github__list_issues").name, "list_issues");
        assert_eq!(
            id("plugin:ci:deploy").namespace,
            ToolNamespace::Plugin("ci".into())
        );
        assert_eq!(id("plugin__ci__deploy").name, "deploy");
        assert_eq!(id("my_custom_tool").namespace, ToolNamespace::Extension);
        assert_eq!(
            id("mcp__resources__list_mcp_resources").namespace,
            ToolNamespace::Mcp("resources".into())
        );
        assert_eq!(
            id("list_mcp_resources").namespace,
            ToolNamespace::McpResources
        );
        assert!(!id("list_mcp_resources").namespace.is_mcp());
        assert_eq!(
            id("list_mcp_resources").display(),
            "builtin:list_mcp_resources"
        );
    }

    #[test]
    fn display_is_namespaced_and_stable() {
        assert_eq!(id("read").display(), "builtin:read");
        assert_eq!(
            id("mcp__github__list_issues").display(),
            "mcp:github:list_issues"
        );
        assert_eq!(id("plugin:ci:deploy").display(), "plugin:ci:deploy");
        assert_eq!(id("my_custom_tool").display(), "ext:my_custom_tool");
    }

    #[test]
    fn patterns_match_both_namespaced_and_raw_forms() {
        let github = id("mcp__github__list_issues");
        assert!(ToolPattern::new("mcp:github:*").matches(&github, "mcp__github__list_issues"));
        assert!(ToolPattern::new("mcp__github__*").matches(&github, "mcp__github__list_issues"));
        assert!(!ToolPattern::new("mcp:gitlab:*").matches(&github, "mcp__github__list_issues"));
        assert!(ToolPattern::new("builtin:re*").matches(&id("read"), "read"));
        assert!(ToolPattern::new("*").matches(&github, "mcp__github__list_issues"));
        // 裸名字只匹配原始名：不能用一个未声明的 MCP/插件同名工具绕过 allowlist。
        assert!(ToolPattern::new("read").matches(&id("read"), "read"));
        assert!(!ToolPattern::new("read").matches(&id("mcp__fs__read"), "mcp__fs__read"));
        assert!(!ToolPattern::new("deploy").matches(&id("plugin:ci:deploy"), "plugin__ci__deploy"));
    }

    #[test]
    fn tool_selection_parses_star_and_lists() {
        assert_eq!(ToolSelection::parse("*"), ToolSelection::All);
        // 显式空字符串/只有分隔符 = 没有任何工具（参照实现对 falsy tools 返回 []）。
        assert_eq!(ToolSelection::parse(""), ToolSelection::Tools(vec![]));
        assert_eq!(
            ToolSelection::parse(" , , \t"),
            ToolSelection::Tools(vec![])
        );
        assert_eq!(
            ToolSelection::from_items([] as [String; 0]),
            ToolSelection::Tools(vec![])
        );
        assert_eq!(
            ToolSelection::parse("read, grep"),
            ToolSelection::Tools(vec![ToolPattern::new("read"), ToolPattern::new("grep"),])
        );
        assert_eq!(
            ToolSelection::from_items(["read".to_string(), "*".to_string()]),
            ToolSelection::All
        );
    }

    #[test]
    fn intersection_never_widens_either_side() {
        let read = id("read");
        let bash = id("bash");
        let parent = ToolPermissionSet::from_ids([read.clone()]);
        let requested = ToolPermissionSet::all();

        let effective = parent.intersect(&requested);
        assert!(effective.allows(&read));
        assert!(!effective.allows(&bash));
        assert!(effective.is_subset_of(&parent));
        assert!(effective.is_subset_of(&requested));
    }

    #[test]
    fn resolve_star_against_parent_pool_yields_parent_subset() {
        let read = id("read");
        let bash = id("bash");
        let parent = ToolPermissionSet::from_ids([read.clone()]);
        let pool = vec![read.clone(), bash.clone()];

        let effective =
            ToolPermissionSet::resolve(&parent, pool.clone(), Some(&ToolSelection::All));
        assert!(effective.allows(&read));
        assert!(!effective.allows(&bash));

        let only_bash = ToolPermissionSet::resolve(
            &parent,
            pool,
            Some(&ToolSelection::Tools(vec![ToolPattern::new("bash")])),
        );
        assert!(
            !only_bash.allows(&bash),
            "parent deny wins over child request"
        );
        assert!(!only_bash.allows(&read), "child request removed read");
    }

    #[test]
    fn resolve_accepts_raw_mcp_patterns() {
        let github = id("mcp__github__list_issues");
        let gitlab = id("mcp__gitlab__list_issues");
        let effective = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![github.clone(), gitlab.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new(
                "mcp__github__*",
            )])),
        );
        assert!(effective.allows(&github));
        assert!(!effective.allows(&gitlab));
    }

    #[test]
    fn resolve_matches_mcp_resource_tools_by_their_bare_names() {
        let read_resource = id("read_mcp_resource");
        let list_resources = id("list_mcp_resources");
        assert_eq!(
            read_resource,
            CanonicalToolId {
                namespace: ToolNamespace::McpResources,
                name: "read_mcp_resource".into(),
            }
        );
        assert_eq!(read_resource.display(), "builtin:read_mcp_resource");
        let effective = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![read_resource.clone(), list_resources.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new(
                "read_mcp_resource",
            )])),
        );
        assert!(effective.allows(&read_resource));
        assert!(!effective.allows(&list_resources));

        // 内建资源工具属于 builtin 命名空间；`mcp:resources:*` 只覆盖真实
        // 名为 `resources` 的 server，不能顺带放行内建工具。
        let by_display = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![read_resource.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new("builtin:*")])),
        );
        assert!(by_display.allows(&read_resource));
        let mcp_namespace = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![read_resource.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new(
                "mcp:resources:*",
            )])),
        );
        assert!(!mcp_namespace.allows(&read_resource));
    }

    #[test]
    fn resolve_does_not_treat_a_real_server_named_resources_as_bare_names() {
        // 真的存在 server 名为 `resources` 时，裸 `foo` 不应匹配 `mcp__resources__foo`。
        let tool = CanonicalToolId {
            namespace: ToolNamespace::Mcp("resources".into()),
            name: "foo".into(),
        };
        let effective = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![tool.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new("foo")])),
        );
        assert!(!effective.allows(&tool));
        assert_eq!(tool.raw_name(), "mcp__resources__foo");
    }

    #[test]
    fn real_server_named_resources_cannot_shadow_a_builtin_resource_tool() {
        // server 名为 `resources` 且工具名恰好与内置资源工具同名时，两者必须是
        // 不同的 canonical id；否则按内置工具收窄的 allowlist 会放行 server 工具。
        let builtin = id("read_mcp_resource");
        let server = id("mcp__resources__read_mcp_resource");
        assert_ne!(builtin, server);
        assert_eq!(server.raw_name(), "mcp__resources__read_mcp_resource");
        assert_eq!(builtin.raw_name(), "read_mcp_resource");
        let effective = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![builtin.clone(), server.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new(
                "read_mcp_resource",
            )])),
        );
        assert!(effective.allows(&builtin));
        assert!(
            !effective.allows(&server),
            "a bare builtin name must not allow the colliding server tool"
        );

        // `mcp:resources:*` 只授权真实 server 的工具；`builtin:*` 只授权内建工具。
        let server_scope = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![builtin.clone(), server.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new(
                "mcp:resources:*",
            )])),
        );
        assert!(!server_scope.allows(&builtin));
        assert!(server_scope.allows(&server));
        let builtin_scope = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            vec![builtin.clone(), server.clone()],
            Some(&ToolSelection::Tools(vec![ToolPattern::new("builtin:*")])),
        );
        assert!(builtin_scope.allows(&builtin));
        assert!(!builtin_scope.allows(&server));
    }

    #[test]
    fn wildcard_matcher_handles_multiple_stars() {
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("a*b*c", "aXXbYY"));
        assert!(wildcard_match("builtin:*", "builtin:read"));
        assert!(wildcard_match("mcp:*:*", "mcp:github:list"));
    }
}
