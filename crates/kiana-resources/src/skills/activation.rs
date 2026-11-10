//! 技能激活：`allowed-tools` → 权限规则、hooks、agent scope 与生命周期。
//!
//! 设计参考 Claude Code `SkillTool`（`allowedTools` 加入 `alwaysAllowRules.command`，
//! deny 仍然优先；`context: fork` 走子代理；`addInvokedSkill` 在压缩后恢复）与
//! oh-my-pi 的 skill 渲染（正文按需读取、`baseDir` 供相对路径解析）。
//!
//! # 安全边界
//!
//! - 激活只产生 **Allow** 规则；[`evaluate_with_activations`] 先跑权限引擎，`Deny` 原样
//!   返回，因此 hint/技能授权永远不会覆盖显式 deny、灾难性命令检查或 read-only 模式。
//! - 规则只在激活存活期间有效：`revoke` / `clear`（会话结束）/ `prune_expired` 之后同一
//!   请求重新回到 `Ask`。
//! - 不写持久授权：规则只存在于 [`SkillActivationSet`] 的内存里；调用方不得把它们写进
//!   settings。

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use kiana_permissions::{
    ActionKind, PermissionDecision, PermissionEngine, PermissionRequest, PermissionRule,
    RuleDecision,
};

use crate::skills::definition::{SkillContext, SkillMetadata};

/// 解析 `allowed-tools` 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedAllowedTools {
    /// 可执行的权限规则。
    pub rules: Vec<PermissionRule>,
    /// 无法识别的条目（原样保留，便于诊断；不会授予任何权限）。
    pub invalid: Vec<String>,
}

/// 把 `allowed-tools` 的条目解析成权限规则。
///
/// 支持语法（大小写不敏感的工具名）：
/// - `Bash(git status)` → 命令前缀 `git status`；
/// - `Bash(git status:*)` / `Bash(git status*)` → 命令通配；
/// - `Bash(*)` → `bash` 工具名；
/// - `Read(src/**)` / `Edit(src/**)` → 路径前缀 `src`（读/写动作）；
/// - `Read` / `mcp__docs__search` / `*` → 工具名规则。
///
/// 无法匹配的括号语法进入 [`ParsedAllowedTools::invalid`]，**不会**退化成宽泛的
/// 工具名规则（避免拼错的条目反而授予更多权限）。
pub fn parse_allowed_tools(specs: &[String]) -> ParsedAllowedTools {
    let mut parsed = ParsedAllowedTools::default();
    for spec in specs {
        match parse_allowed_tool(spec) {
            Some(rule) => parsed.rules.push(rule),
            None => parsed.invalid.push(spec.clone()),
        }
    }
    parsed
}

fn parse_allowed_tool(spec: &str) -> Option<PermissionRule> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (tool, argument) = match spec.split_once('(') {
        Some((tool, rest)) => {
            let rest = rest.strip_suffix(')')?;
            (tool.trim(), Some(rest.trim()))
        }
        None => (spec, None),
    };
    if tool.is_empty() {
        return None;
    }
    let lower = tool.to_ascii_lowercase();
    match argument {
        None => Some(PermissionRule::tool(tool, RuleDecision::Allow)),
        Some(argument) => match lower.as_str() {
            "bash" | "powershell" => {
                if argument.is_empty() || argument == "*" {
                    return Some(PermissionRule::tool(lower, RuleDecision::Allow));
                }
                // Claude Code 的 `cmd:*` 表示“以 cmd 开头”；普通 `cmd` 是命令前缀。
                if let Some(prefix) = argument.strip_suffix(":*") {
                    return Some(PermissionRule::wildcard_command(
                        format!("{prefix}*"),
                        RuleDecision::Allow,
                    ));
                }
                if argument.ends_with('*') {
                    return Some(PermissionRule::wildcard_command(
                        argument,
                        RuleDecision::Allow,
                    ));
                }
                let tokens: Vec<String> = argument.split_whitespace().map(str::to_string).collect();
                if tokens.is_empty() {
                    return None;
                }
                Some(PermissionRule::command(tokens, RuleDecision::Allow))
            }
            "read" | "ls" | "grep" | "glob" | "find" => path_rule(tool, argument, ActionKind::Read),
            "edit" | "write" | "apply_patch" => path_rule(tool, argument, ActionKind::Write),
            // 其它工具带参数：不猜测语义，避免把 `WebFetch(domain:x)` 错配成工具名。
            _ => None,
        },
    }
}

/// 路径参数：`src/**`/`src/*` 取前缀 `src`；裸路径原样作为前缀。
fn path_rule(tool: &str, argument: &str, action: ActionKind) -> Option<PermissionRule> {
    let argument = argument.trim();
    if argument.is_empty() || argument == "*" {
        return Some(PermissionRule::tool(tool, RuleDecision::Allow));
    }
    let prefix = argument
        .strip_suffix("/**")
        .or_else(|| argument.strip_suffix("/*"))
        .unwrap_or(argument);
    if prefix.is_empty() {
        return None;
    }
    Some(PermissionRule::PathPrefix {
        prefix: std::path::PathBuf::from(prefix),
        actions: Some(vec![action]),
        decision: RuleDecision::Allow,
        reason: None,
    })
}

/// 一次技能激活的完整授权载荷。
///
/// `expires_at` 为 `None` 表示直到显式撤销或会话结束；`Duration` 形式的 TTL 由
/// [`SkillActivation::with_ttl`] 换算。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillActivation {
    /// 技能名。
    pub skill_name: String,
    /// 解析出的 Allow 规则。
    pub permission_rules: Vec<PermissionRule>,
    /// 原始 `allowed-tools`（诊断/审计）。
    pub allowed_tools: Vec<String>,
    /// 无法识别的条目。
    pub invalid_allowed_tools: Vec<String>,
    /// 激活期间安装的 hooks。
    pub hooks: Option<Value>,
    /// fork 时的子代理名。
    pub agent_scope: Option<String>,
    /// 过期时刻（UTC）。
    pub expires_at: Option<DateTime<Utc>>,
    /// inline/fork。
    pub context: SkillContext,
    /// 模型覆盖。
    pub model: Option<String>,
    /// effort 覆盖。
    pub effort: Option<String>,
}

impl SkillActivation {
    /// 从技能元数据构造（没有过期时间）。
    pub fn from_metadata(skill_name: impl Into<String>, metadata: &SkillMetadata) -> Self {
        let parsed = parse_allowed_tools(&metadata.allowed_tools);
        SkillActivation {
            skill_name: skill_name.into(),
            permission_rules: parsed.rules,
            allowed_tools: metadata.allowed_tools.clone(),
            invalid_allowed_tools: parsed.invalid,
            hooks: metadata.hooks.clone(),
            agent_scope: metadata.agent.clone(),
            expires_at: None,
            context: metadata.context,
            model: metadata.model.clone(),
            effort: metadata.effort.clone(),
        }
    }

    /// 设置相对 TTL（从现在起）。
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        let expires = Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_default();
        self.expires_at = Some(expires);
        self
    }

    /// 设置绝对过期时刻（测试/宿主恢复）。
    pub fn with_expires_at(mut self, expires_at: Option<DateTime<Utc>>) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// 在 `now` 时刻是否仍然有效。
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
    }

    /// 审计/详情 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "skill": self.skill_name,
            "allowedTools": self.allowed_tools,
            "invalidAllowedTools": self.invalid_allowed_tools,
            "context": self.context.as_str(),
            "agentScope": self.agent_scope,
            "model": self.model,
            "effort": self.effort,
            "hooks": self.hooks,
            "expiresAt": self.expires_at.map(|time| time.to_rfc3339()),
        })
    }
}

/// 活动激活的集合（内存；会话结束由 [`Self::clear`] 清空）。
#[derive(Debug, Default)]
pub struct SkillActivationSet {
    activations: Mutex<Vec<SkillActivation>>,
}

impl SkillActivationSet {
    /// 空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入/替换一个技能的激活（同名只保留最新）。
    pub fn activate(&self, activation: SkillActivation) {
        let mut guard = self.lock();
        guard.retain(|existing| existing.skill_name != activation.skill_name);
        guard.push(activation);
    }

    /// 撤销一个技能；返回是否找到。
    pub fn revoke(&self, skill_name: &str) -> bool {
        let mut guard = self.lock();
        let before = guard.len();
        guard.retain(|activation| activation.skill_name != skill_name);
        guard.len() != before
    }

    /// 会话结束：清空全部激活。
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// 清理过期激活；返回清理数量。
    pub fn prune_expired(&self, now: DateTime<Utc>) -> usize {
        let mut guard = self.lock();
        let before = guard.len();
        guard.retain(|activation| activation.is_active_at(now));
        before - guard.len()
    }

    /// 当前有效激活的快照。
    pub fn active(&self, now: DateTime<Utc>) -> Vec<SkillActivation> {
        self.lock()
            .iter()
            .filter(|activation| activation.is_active_at(now))
            .cloned()
            .collect()
    }

    /// 某个请求是否被一个仍然有效的激活放行；返回造成放行的技能名。
    pub fn allows(&self, request: &PermissionRequest, now: DateTime<Utc>) -> Option<String> {
        let guard = self.lock();
        for activation in guard
            .iter()
            .filter(|activation| activation.is_active_at(now))
        {
            if activation
                .permission_rules
                .iter()
                .any(|rule| rule.decision() == RuleDecision::Allow && rule.matches(request))
            {
                return Some(activation.skill_name.clone());
            }
        }
        None
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SkillActivation>> {
        self.activations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 带技能激活的权限判定。
///
/// 顺序：
/// 1. `engine.evaluate`（含灾难性命令、Deny 规则、会话批准、Ask/Allow 规则与模式默认）；
/// 2. 结果不是 `Ask` 时原样返回（`Deny` 永远赢）；
/// 3. 否则若某个有效激活的 Allow 规则命中请求，返回 `Allow`（reason 指明技能名）；
/// 4. 没有命中则保留 `Ask`。
pub fn evaluate_with_activations(
    engine: &PermissionEngine,
    activations: &SkillActivationSet,
    request: &PermissionRequest,
    now: DateTime<Utc>,
) -> PermissionDecision {
    let decision = engine.evaluate(request);
    if !decision.is_ask() {
        return decision;
    }
    match activations.allows(request, now) {
        Some(skill) => PermissionDecision::Allow {
            reason: format!("Allowed by active skill \"{skill}\"."),
        },
        None => decision,
    }
}

/// 当前时间（测试可注入固定值，生产用）。
pub fn now_utc() -> DateTime<Utc> {
    let now = SystemTime::now();
    DateTime::<Utc>::from(now)
}
