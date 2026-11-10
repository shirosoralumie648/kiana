//! `skill` 工具：按需加载技能正文并返回 [`SkillActivation`]。
//!
//! 参考 Claude Code `SkillTool` 的 `validateInput` / `call` 顺序：
//! 1. 名字必须存在；
//! 2. `disable-model-invocation` 的技能拒绝模型调用；
//! 3. 参数按 frontmatter 的 `arguments` 校验（必填/未知）；
//! 4. 读取正文，替换 `$ARGUMENTS`/`${name}`/`$name`/`$1..`，**整体 XML 转义**后注入；
//! 5. 附技能目录清单（相对路径），让模型能解析 `scripts/` 等相对引用；
//! 6. 返回 `SkillActivation`（权限规则/hooks/agent scope/expires_at）并登记到
//!    [`SkillActivationSet`]；激活结束/会话结束由 [`SkillTool::deactivate`] /
//!    [`SkillTool::end_session`] 撤销。
//!
//! 系统提示只放 name/description/location；正文只在调用后进入上下文。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use kiana_agent_core::{AgentToolError, AgentToolResult};
use kiana_ai::types::{TextContent, TextKind, UserContentBlock};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolExposure};
use kiana_ext_api::{BoxFuture, ExtensionToolContextApi};

use crate::skills::activation::{SkillActivation, SkillActivationSet, now_utc};
use crate::skills::definition::{SkillDefinition, list_skill_directory};
use crate::skills::prompt::escape_xml;

/// 工具名。
pub const SKILL_TOOL_NAME: &str = "skill";

/// 默认激活 TTL（`None` 表示直到显式撤销/会话结束）。
pub const DEFAULT_SKILL_ACTIVATION_TTL: Option<Duration> = None;

/// 一次成功的技能调用结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SkillToolOutcome {
    /// 注入给模型的 user 内容（已转义）。
    pub content: String,
    /// 激活载荷。
    pub activation: SkillActivation,
    /// 显示在 details 里的技能目录清单。
    pub files: Vec<String>,
}

/// `skill` 工具的执行器与激活台账。
#[derive(Debug)]
pub struct SkillTool {
    definitions: Mutex<Vec<SkillDefinition>>,
    activations: SkillActivationSet,
    ttl: Mutex<Option<Duration>>,
}

impl SkillTool {
    /// 用一组技能定义构造。
    pub fn new(definitions: Vec<SkillDefinition>) -> Self {
        SkillTool {
            definitions: Mutex::new(definitions),
            activations: SkillActivationSet::new(),
            ttl: Mutex::new(DEFAULT_SKILL_ACTIVATION_TTL),
        }
    }

    /// 替换技能列表（资源 reload 后调用）；已有激活不清空。
    pub fn update_definitions(&self, definitions: Vec<SkillDefinition>) {
        *self
            .definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = definitions;
    }

    /// 设置激活 TTL（`None` 表示不自动过期）。
    pub fn set_ttl(&self, ttl: Option<Duration>) {
        *self
            .ttl
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ttl;
    }

    /// 当前定义快照。
    pub fn definitions(&self) -> Vec<SkillDefinition> {
        self.definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 用户可调用的技能（`user-invocable`）。
    pub fn user_invocable(&self) -> Vec<SkillDefinition> {
        self.definitions()
            .into_iter()
            .filter(|definition| definition.metadata.user_invocable)
            .collect()
    }

    /// 当前有效激活。
    pub fn active_activations(&self, now: DateTime<Utc>) -> Vec<SkillActivation> {
        self.activations.active(now)
    }

    /// 撤销一个技能的激活（激活结束）。
    pub fn deactivate(&self, skill_name: &str) -> bool {
        self.activations.revoke(skill_name)
    }

    /// 会话结束：撤销全部激活。
    pub fn end_session(&self) {
        self.activations.clear();
    }

    /// 激活台账。
    pub fn activations(&self) -> &SkillActivationSet {
        &self.activations
    }

    /// 执行一次技能调用。
    pub fn invoke(
        &self,
        name: &str,
        arguments: Option<&Map<String, Value>>,
    ) -> Result<SkillToolOutcome, String> {
        self.invoke_inner(name, arguments, InvocationSource::Model)
    }

    /// 用户路径（`/skill:<name>`）：校验 `user-invocable` 与 `arguments` 后登记激活。
    ///
    /// 与模型路径的区别：
    /// - `user-invocable=false` 拒绝；
    /// - `disable-model-invocation` **不**影响用户调用（这正是两个 flag 的分工）；
    /// - 原始参数文本可以按 `key=value` / `--key value` 解析成声明的命名参数；
    ///   技能没有声明 `arguments` 时原样交给正文替换（兼容既有 `/skill:name free text`）。
    pub fn activate_user(
        &self,
        name: &str,
        raw_arguments: &str,
    ) -> Result<SkillActivation, String> {
        let outcome = self.invoke_inner(
            name,
            Some(&parse_user_arguments(
                name,
                raw_arguments,
                &self.definitions(),
            )?),
            InvocationSource::User,
        )?;
        Ok(outcome.activation)
    }

    fn invoke_inner(
        &self,
        name: &str,
        arguments: Option<&Map<String, Value>>,
        source: InvocationSource,
    ) -> Result<SkillToolOutcome, String> {
        let definitions = self.definitions();
        let definition = definitions
            .iter()
            .find(|definition| definition.name() == name)
            .ok_or_else(|| format!("Unknown skill: {name}"))?;
        match source {
            InvocationSource::Model => {
                if definition.metadata.disable_model_invocation {
                    return Err(format!(
                        "Skill {name} cannot be used with the {SKILL_TOOL_NAME} tool due to disable-model-invocation"
                    ));
                }
            }
            InvocationSource::User => {
                if !definition.metadata.user_invocable {
                    return Err(format!("Skill {name} is not user-invocable"));
                }
            }
        }
        let arguments = arguments.cloned().unwrap_or_default();
        let rendered_args = validate_and_render_arguments(definition, &arguments)?;
        let body = substitute_arguments(&definition.content, &rendered_args);
        let files = list_skill_directory(Path::new(definition.base_dir()));
        let mut content = format!(
            "<skill name=\"{}\" location=\"{}\">",
            escape_xml(definition.name()),
            escape_xml(definition.file_path())
        );
        if !files.is_empty() {
            content.push_str("\n<skill_files>");
            for file in &files {
                content.push('\n');
                content.push_str(&escape_xml(file));
            }
            content.push_str("\n</skill_files>");
        }
        content.push_str("\n<skill_content>\n");
        content.push_str(&escape_xml(&body));
        content.push_str("\n</skill_content>\n</skill>");

        let mut activation =
            SkillActivation::from_metadata(definition.name(), &definition.metadata);
        if let Some(ttl) = *self
            .ttl
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            activation = activation.with_ttl(ttl);
        }
        self.activations.activate(activation.clone());
        Ok(SkillToolOutcome {
            content,
            activation,
            files,
        })
    }
}

/// 调用来源：模型 `skill` 工具或用户 `/skill:<name>`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvocationSource {
    Model,
    User,
}

/// 把 `/skill:<name> ...` 的原始参数解析成声明的命名参数。
///
/// 支持 `key=value`、`--key=value`、`--key value` 与按声明顺序的位置参数。技能没有声明
/// `arguments` 时返回空 map（兼容既有的自由文本参数，正文替换仍由调用方保留）。
fn parse_user_arguments(
    name: &str,
    raw: &str,
    definitions: &[SkillDefinition],
) -> Result<Map<String, Value>, String> {
    let Some(definition) = definitions
        .iter()
        .find(|definition| definition.name() == name)
    else {
        return Ok(Map::new());
    };
    let spec = &definition.metadata.arguments;
    if spec.is_empty() {
        return Ok(Map::new());
    }
    let mut values: Map<String, Value> = Map::new();
    let mut positional: Vec<String> = Vec::new();
    let mut tokens = raw.split_whitespace();
    while let Some(token) = tokens.next() {
        if token.is_empty() {
            continue;
        }
        if let Some(rest) = token.strip_prefix("--") {
            if let Some((key, value)) = rest.split_once('=') {
                insert_named_argument(spec, &mut values, key, value.to_string())?;
            } else {
                let value = tokens
                    .next()
                    .ok_or_else(|| format!("Missing value for argument \"{rest}\""))?;
                insert_named_argument(spec, &mut values, rest, value.to_string())?;
            }
        } else if let Some((key, value)) = token.split_once('=') {
            insert_named_argument(spec, &mut values, key, value.to_string())?;
        } else {
            positional.push(token.to_string());
        }
    }
    for value in positional {
        let Some(argument) = spec
            .iter()
            .find(|argument| !values.contains_key(&argument.name))
        else {
            return Err(format!("Unknown argument \"{value}\""));
        };
        values.insert(argument.name.clone(), Value::String(value));
    }
    Ok(values)
}

fn insert_named_argument(
    spec: &[crate::skills::definition::SkillArgument],
    values: &mut Map<String, Value>,
    key: &str,
    value: String,
) -> Result<(), String> {
    if !spec.iter().any(|argument| argument.name == key) {
        return Err(format!("Unknown argument \"{key}\""));
    }
    values.insert(key.to_string(), Value::String(value));
    Ok(())
}

/// 校验参数并按声明顺序拼成 `$ARGUMENTS` 的文本。
fn validate_and_render_arguments(
    definition: &SkillDefinition,
    arguments: &Map<String, Value>,
) -> Result<Vec<String>, String> {
    let spec = &definition.metadata.arguments;
    for key in arguments.keys() {
        if !spec.iter().any(|argument| &argument.name == key) {
            return Err(format!("Unknown argument \"{key}\""));
        }
    }
    let mut values = Vec::with_capacity(spec.len());
    for argument in spec {
        let value = arguments.get(&argument.name);
        let value = match value {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            Some(Value::Bool(flag)) => Some(flag.to_string()),
            Some(Value::Null) | None => None,
            Some(other) => {
                return Err(format!(
                    "Argument \"{}\" must be a string, got {}",
                    argument.name,
                    json_type_name(other)
                ));
            }
        };
        if argument.required {
            match &value {
                Some(text) if !text.is_empty() => {}
                _ => {
                    return Err(format!("Missing required argument \"{}\"", argument.name));
                }
            }
        }
        values.push(value.unwrap_or_default());
    }
    // 未声明参数但传了参数：上面已经按 unknown 拒绝；没有声明时保持空。
    Ok(values)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 替换正文里的参数占位符。
///
/// 先替换 `$ARGUMENTS`（全部参数按声明顺序空格连接），再替换 `${name}`/`$name`/`$N`。
/// 替换是纯文本的；调用方随后对整段正文做 XML 转义。
fn substitute_arguments(body: &str, values: &[String]) -> String {
    let mut rendered = body.replace("$ARGUMENTS", &values.join(" "));
    // `$1`..`$N`：先替换多位数，避免 `$1` 命中 `$10` 的前缀。
    for (index, value) in values.iter().enumerate().rev() {
        rendered = rendered.replace(&format!("${}", index + 1), value);
    }
    rendered
}

/// 工具描述。
const SKILL_TOOL_DESCRIPTION: &str = "Load and run a skill by name. The skill body is injected as a user message and may grant \
     temporary permissions (allowed-tools), install hooks, or select a model/effort. Skills are \
     discoverable from the system prompt; use this tool only when the task matches a skill.";

fn skill_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Skill name exactly as listed in the available skills section."
            },
            "arguments": {
                "type": "object",
                "description": "Named arguments declared by the skill (see argument-hint).",
                "additionalProperties": { "type": ["string", "number", "boolean"] }
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

/// 构造可直接注册到 `ExtensionApi` 的 `skill` 工具定义。
///
/// `execute` 的细节：
/// - `name` 缺失/非字符串 → `AgentToolError`；
/// - 参数校验/未知技能 → `AgentToolError`（消息与 [`SkillTool::invoke`] 相同）；
/// - 成功时 `details` 是 `{ "skill": ..., "activation": {...}, "files": [...] }`，
///   宿主可在 `after_tool_call` 里读取 `activation` 并应用权限/hooks（见 handoff）。
pub fn create_skill_tool_definition(tool: Arc<SkillTool>) -> ToolDefinition {
    ToolDefinition {
        name: SKILL_TOOL_NAME.to_string(),
        label: "Skill".to_string(),
        description: SKILL_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(
            "Load a skill's instructions and temporarily activate its permissions".to_string(),
        ),
        prompt_guidelines: None,
        parameters: skill_parameters(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: Some(ToolExposure::Direct),
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(SkillToolExecutable { tool }),
        render_call: None,
        render_result: None,
    }
}

struct SkillToolExecutable {
    tool: Arc<SkillTool>,
}

impl ToolDefinitionExecute for SkillToolExecutable {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: Value,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<Value>, AgentToolError>> {
        Box::pin(async move {
            if signal.is_some_and(|signal| signal.is_cancelled()) {
                return Err(agent_error("Skill invocation aborted"));
            }
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| agent_error("Missing required parameter \"name\""))?;
            let arguments = match params.get("arguments") {
                None | Some(Value::Null) => None,
                Some(Value::Object(arguments)) => Some(arguments),
                Some(_) => return Err(agent_error("Parameter \"arguments\" must be an object")),
            };
            let outcome = self.tool.invoke(name, arguments).map_err(agent_error)?;
            let details = json!({
                "skill": name,
                "activation": outcome.activation.to_json(),
                "files": outcome.files,
            });
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: outcome.content,
                    text_signature: None,
                })],
                details,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn agent_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

/// 当前时间（供调用方构造 `now` 参数）。
pub fn current_time() -> DateTime<Utc> {
    now_utc()
}
