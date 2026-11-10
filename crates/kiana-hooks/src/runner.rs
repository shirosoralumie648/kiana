//! Hook registry、执行顺序与结果聚合。

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::audit::{
    HookAuditOutcome, HookAuditRecord, HookAuditSink, HookHandlerKind, NoopAuditSink,
};
use crate::command::{self, CommandContext, DEFAULT_HOOK_ENV_ALLOWLIST};
use crate::config::{
    CommandHookConfig, HookConfigDiagnostic, HookConfigLoad, HookHandlerConfig, HttpHookConfig,
};
use crate::error::HookError;
use crate::event::{HookDecision, HookEvent, HookSource};
use crate::http::{self, CompiledHttpPolicy, HttpHookPolicy};
use crate::input::HookInput;
use crate::matcher::HookMatcher;
use crate::output::{HookJsonOutput, ParsedStdout, escape_additional_context, parse_stdout};

/// 递归深度环境变量：command hook 子进程里是“当前深度 + 1”。
pub const HOOK_DEPTH_ENV: &str = "KIANA_HOOK_DEPTH";

/// 默认每轮允许的 deny 次数。
pub const DEFAULT_MAX_DENIES_PER_TURN: u32 = 8;
/// 默认递归深度上限。
pub const DEFAULT_MAX_HOOK_DEPTH: u32 = 5;

/// `HookRunner` 配置。
pub struct HookRunnerConfig {
    /// 项目根：command hook 的进程 cwd 与 stdin `cwd` 都固定为它。
    pub project_root: PathBuf,
    /// 传给 command hook 的环境变量白名单。
    pub env_allowlist: Vec<String>,
    /// HTTP hook 全局策略。
    pub http_policy: HttpHookPolicy,
    /// 每轮 deny 上限；`0` 表示不限制。
    pub max_denies_per_turn: u32,
    /// 递归深度上限。
    pub max_depth: u32,
    /// 审计 sink。
    pub audit: Arc<dyn HookAuditSink>,
}

impl HookRunnerConfig {
    /// 用项目根创建默认配置（30s handler 超时、8 次 deny 上限、深度 5）。
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            env_allowlist: DEFAULT_HOOK_ENV_ALLOWLIST
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            http_policy: HttpHookPolicy::default(),
            max_denies_per_turn: DEFAULT_MAX_DENIES_PER_TURN,
            max_depth: DEFAULT_MAX_HOOK_DEPTH,
            audit: Arc::new(NoopAuditSink),
        }
    }

    /// 替换审计 sink。
    pub fn with_audit(mut self, audit: Arc<dyn HookAuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// 替换环境变量白名单（会覆盖默认值；需要默认集合请先 clone 默认）。
    pub fn with_env_allowlist(mut self, allowlist: Vec<String>) -> Self {
        self.env_allowlist = allowlist;
        self
    }
}

impl fmt::Debug for HookRunnerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRunnerConfig")
            .field("project_root", &self.project_root)
            .field("env_allowlist", &self.env_allowlist)
            .field("http_policy", &self.http_policy)
            .field("max_denies_per_turn", &self.max_denies_per_turn)
            .field("max_depth", &self.max_depth)
            .finish_non_exhaustive()
    }
}

/// 已注册的 hook。
#[derive(Debug, Clone)]
struct RegisteredHook {
    source: HookSource,
    event: HookEvent,
    matcher: Option<HookMatcher>,
    handler: HookHandlerConfig,
}

/// Hook 注册表：按来源收集配置，查询时按 `local > project > user` 排序。
#[derive(Debug, Clone, Default)]
pub struct HookRegistry {
    hooks: Vec<RegisteredHook>,
}

impl HookRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入一个来源的配置。
    ///
    /// `trusted` 只对 [`HookSource::requires_trust`] 的来源生效：未受信的
    /// project/local 配置整体丢弃，返回一条诊断。配置自身的解析诊断也会一并返回。
    pub fn add_source(
        &mut self,
        source: HookSource,
        load: HookConfigLoad,
        trusted: bool,
    ) -> Vec<HookConfigDiagnostic> {
        let mut diagnostics = load.diagnostics;
        if source.requires_trust() && !trusted {
            diagnostics.push(HookConfigDiagnostic {
                event: None,
                message: format!(
                    "{source} hooks were not loaded because the project is not trusted"
                ),
            });
            return diagnostics;
        }

        for event in HookEvent::ALL {
            for group in load.config.event_groups(event) {
                for handler in &group.hooks {
                    self.hooks.push(RegisteredHook {
                        source,
                        event,
                        matcher: group.matcher.clone(),
                        handler: handler.clone(),
                    });
                }
            }
        }
        diagnostics
    }

    /// 已注册 hook 数。
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    fn matching_hooks(&self, input: &HookInput) -> Vec<&RegisteredHook> {
        let subject = input.matcher_subject();
        let mut matching: Vec<&RegisteredHook> = self
            .hooks
            .iter()
            .filter(|hook| hook.event == input.hook_event_name)
            .filter(|hook| {
                hook.matcher
                    .as_ref()
                    .map(|matcher| matcher.matches(subject))
                    .unwrap_or(true)
            })
            .collect();
        // 稳定排序：同来源保持声明顺序。
        matching.sort_by_key(|hook| hook.source.priority());
        matching
    }
}

/// 一轮（turn）内的 hook 状态。
///
/// 调用方每个回合创建一个，并在同一回合的多个事件之间复用（或按产品语义重置
/// `deny_count`）。`depth` 从 [`HOOK_DEPTH_ENV`] 初始化，被 hook 启动的 Kiana
/// 进程因此能续接递归深度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookTurnState {
    /// 本轮已经产生的 deny 数。
    pub deny_count: u32,
    /// 当前递归深度。
    pub depth: u32,
}

impl HookTurnState {
    /// 新回合：`deny_count = 0`，深度来自环境变量。
    pub fn new() -> Self {
        Self {
            deny_count: 0,
            depth: current_hook_depth(),
        }
    }

    /// 指定深度（测试 / 显式控制）。
    pub fn at_depth(depth: u32) -> Self {
        Self {
            deny_count: 0,
            depth,
        }
    }

    /// 重置本回合的 deny 计数（保留深度）。
    pub fn reset_turn(&mut self) {
        self.deny_count = 0;
    }
}

impl Default for HookTurnState {
    fn default() -> Self {
        Self::new()
    }
}

/// 读取当前进程继承的 hook 递归深度。
pub fn current_hook_depth() -> u32 {
    std::env::var(HOOK_DEPTH_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// 单个 hook 的运行记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunRecord {
    /// 事件。
    pub event: HookEvent,
    /// 来源。
    pub source: HookSource,
    /// handler 类型。
    pub handler: HookHandlerKind,
    /// command 文本或 URL。
    pub summary: String,
    /// 运行结果。
    pub outcome: HookAuditOutcome,
    /// 退出码（command）。
    pub exit_code: Option<i32>,
    /// HTTP 状态码。
    pub http_status: Option<u16>,
    /// 保留的 stdout（已截断）。
    pub stdout: String,
    /// 保留的 stderr（已截断）。
    pub stderr: String,
    /// 是否发生输出截断。
    pub output_truncated: bool,
    /// 耗时。
    pub duration: Duration,
    /// 是否 async。
    pub asynchronous: bool,
    /// 该次运行产生的警告文本（聚合后写入审计记录）。
    pub warnings: Vec<String>,
}

/// 聚合后的 hook 结果。
#[derive(Debug, Clone, PartialEq)]
pub struct HookOutcome {
    /// 事件。
    pub event: HookEvent,
    /// 合并后的决策。
    pub decision: HookDecision,
    /// deny / ask 的原因（多个 deny 用换行连接）。
    pub reason: Option<String>,
    /// 最终的工具参数（`updatedInput` 顺序替换的结果）。
    pub updated_input: Option<Value>,
    /// 需要注入模型上下文的附加文本（原始、未转义）。
    pub additional_context: Vec<String>,
    /// 展示给用户的消息。
    pub system_messages: Vec<String>,
    /// 模型可见的反馈（非阻塞事件的 exit 2 stderr）。
    pub model_feedback: Vec<String>,
    /// `continue: false` 的停止原因。
    pub stop_reason: Option<String>,
    /// 是否继续执行；`continue: false` 时为 `false`。
    pub continue_execution: bool,
    /// 是否隐藏 stdout。
    pub suppress_output: bool,
    /// `PermissionDenied` 的 `retry`。
    pub retry: Option<bool>,
    /// 非阻塞警告（超时、非零退出、非法 JSON、SSRF 拒绝等）。
    pub warnings: Vec<String>,
    /// 每个 hook 的运行记录（按执行顺序）。
    pub runs: Vec<HookRunRecord>,
    /// 本轮 deny 上限是否被触发（触发后 deny 降级为 ask）。
    pub deny_cap_reached: bool,
}

impl HookOutcome {
    fn new(event: HookEvent) -> Self {
        Self {
            event,
            decision: HookDecision::Passthrough,
            reason: None,
            updated_input: None,
            additional_context: Vec::new(),
            system_messages: Vec::new(),
            model_feedback: Vec::new(),
            stop_reason: None,
            continue_execution: true,
            suppress_output: false,
            retry: None,
            warnings: Vec::new(),
            runs: Vec::new(),
            deny_cap_reached: false,
        }
    }

    /// 是否被 hook 拒绝。
    pub fn is_denied(&self) -> bool {
        self.decision.is_denied()
    }

    /// 是否要求询问用户。
    pub fn is_asking(&self) -> bool {
        self.decision == HookDecision::Ask
    }

    /// deny 的原因（模型可见错误只应出现一次）。
    pub fn blocking_error(&self) -> Option<&str> {
        if self.is_denied() {
            self.reason.as_deref().or(Some("Blocked by hook"))
        } else {
            None
        }
    }

    /// 拼接并 XML 转义 `additionalContext`；没有内容时返回 `None`。
    ///
    /// 会话层应把返回值放进自己的 XML 标签里（例如 `<hook-context>...</hook-context>`），
    /// 不要直接使用 `additional_context` 原始值。
    pub fn additional_context_xml(&self) -> Option<String> {
        if self.additional_context.is_empty() {
            None
        } else {
            Some(escape_additional_context(
                &self.additional_context.join("\n"),
            ))
        }
    }
}

/// Hook 执行器。
pub struct HookRunner {
    registry: HookRegistry,
    config: HookRunnerConfig,
    client: reqwest::Client,
    policy: CompiledHttpPolicy,
}

impl fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRunner")
            .field("registry", &self.registry)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl HookRunner {
    /// 创建 runner。HTTP URL 白名单编译失败或 HTTP client 构建失败时返回
    /// [`HookError::Config`]。
    pub fn new(registry: HookRegistry, config: HookRunnerConfig) -> Result<Self, HookError> {
        let policy = http::compile_policy(&config.http_policy)?;
        let client = http::build_client()?;
        Ok(Self {
            registry,
            config,
            client,
            policy,
        })
    }

    /// 注册表引用。
    pub fn registry(&self) -> &HookRegistry {
        &self.registry
    }

    /// 配置引用。
    pub fn config(&self) -> &HookRunnerConfig {
        &self.config
    }

    /// 用新回合状态执行一个事件。
    pub async fn run_event(&self, input: &HookInput) -> Result<HookOutcome, HookError> {
        let mut state = HookTurnState::new();
        self.run_event_with_state(input, &mut state).await
    }

    /// 用调用方提供的回合状态执行一个事件。
    pub async fn run_event_with_state(
        &self,
        input: &HookInput,
        state: &mut HookTurnState,
    ) -> Result<HookOutcome, HookError> {
        self.validate_input(input)?;
        if state.depth > self.config.max_depth {
            return Err(HookError::RecursionLimit {
                depth: state.depth,
                limit: self.config.max_depth,
            });
        }

        let mut outcome = HookOutcome::new(input.hook_event_name);
        let mut current_input = input.tool_input.clone();
        let mut updated_input: Option<Value> = None;

        for hook in self.registry.matching_hooks(input) {
            let mut hook_input = input.clone();
            // cwd 固定项目根，不允许调用方/Hook 配置改写。
            hook_input.cwd = self.config.project_root.to_string_lossy().into_owned();
            hook_input.tool_input = current_input.clone();
            let input_json = hook_input.to_json_string();

            match &hook.handler {
                HookHandlerConfig::Command(command) => {
                    if command.is_async {
                        let record = self.start_async_command(
                            hook.source,
                            input.hook_event_name,
                            command,
                            state.depth,
                            &input_json,
                        );
                        outcome.runs.push(record);
                        continue;
                    }
                    let context = CommandContext {
                        project_root: self.config.project_root.clone(),
                        env_allowlist: self.config.env_allowlist.clone(),
                        depth: state.depth,
                        event: input.hook_event_name,
                        source: hook.source,
                    };
                    let run = command::run_command(
                        &command.command,
                        command.timeout,
                        &input_json,
                        &context,
                    )
                    .await;
                    let record = self.apply_command_run(
                        &mut outcome,
                        state,
                        hook.source,
                        command,
                        run,
                        &mut current_input,
                        &mut updated_input,
                    );
                    outcome.runs.push(record);
                }
                HookHandlerConfig::Http(http_config) => {
                    let run =
                        http::run_http(&self.client, http_config, &self.policy, &input_json).await;
                    let record = self.apply_http_run(
                        &mut outcome,
                        state,
                        hook.source,
                        http_config,
                        run,
                        &mut current_input,
                        &mut updated_input,
                    );
                    outcome.runs.push(record);
                }
            }
        }

        // `updatedInput` 是顺序替换后的最终参数；即使最终 deny，也保留最后一次改写，
        // 由调用方决定是否使用（deny 时不应使用）。没有任何 hook 改写时为 `None`。
        outcome.updated_input = updated_input;
        Ok(outcome)
    }

    fn validate_input(&self, input: &HookInput) -> Result<(), HookError> {
        if input.hook_event_name.requires_tool_name()
            && input
                .tool_name
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            return Err(HookError::InvalidInput(format!(
                "{} requires a non-empty tool_name",
                input.hook_event_name
            )));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_command_run(
        &self,
        outcome: &mut HookOutcome,
        state: &mut HookTurnState,
        source: HookSource,
        command: &CommandHookConfig,
        run: command::CommandRun,
        current_input: &mut Option<Value>,
        updated_input_out: &mut Option<Value>,
    ) -> HookRunRecord {
        let mut record = HookRunRecord {
            event: outcome.event,
            source,
            handler: HookHandlerKind::Command,
            summary: command.command.clone(),
            outcome: HookAuditOutcome::Success,
            exit_code: run.exit_code,
            http_status: None,
            stdout: run.stdout.text.clone(),
            stderr: run.stderr.text.clone(),
            output_truncated: run.stdout.truncated || run.stderr.truncated,
            duration: run.duration,
            asynchronous: false,
            warnings: Vec::new(),
        };
        let warnings_start = outcome.warnings.len();

        if let Some(error) = run.spawn_error {
            record.outcome = HookAuditOutcome::SpawnError;
            outcome.warnings.push(format!(
                "{source} command hook `{}` failed to start: {error}",
                command.command
            ));
            self.finish_run_record(&mut record, outcome, warnings_start);
            return record;
        }

        if run.timed_out {
            record.outcome = HookAuditOutcome::Timeout;
            outcome.warnings.push(format!(
                "{source} command hook `{}` timed out after {}s and its process group was killed",
                command.command,
                command.timeout.as_secs_f32()
            ));
            self.finish_run_record(&mut record, outcome, warnings_start);
            return record;
        }

        match run.exit_code {
            Some(0) => match parse_stdout(&run.stdout.text) {
                ParsedStdout::Empty => {}
                ParsedStdout::Plain(text) => {
                    if outcome.event.plain_stdout_is_context() {
                        outcome.additional_context.push(text);
                    }
                }
                ParsedStdout::Json(json) => {
                    if let Some(updated) =
                        self.apply_json_output(outcome, state, source, &json, None, true)
                    {
                        *current_input = Some(updated.clone());
                        *updated_input_out = Some(updated);
                    }
                }
                ParsedStdout::InvalidJson { error, .. } => {
                    record.outcome = HookAuditOutcome::InvalidOutput;
                    outcome.warnings.push(format!(
                        "{source} command hook `{}` produced invalid output: {error}",
                        command.command
                    ));
                }
            },
            Some(2) => {
                record.outcome = HookAuditOutcome::Blocked;
                let mut reason = run.stderr.text.trim().to_string();
                // 退出码 2 时 stdout 仍可能是 JSON；应用其中的 systemMessage /
                // updatedInput 等字段，但最终决策固定为 deny（exit 2 优先）。
                if let ParsedStdout::Json(json) = parse_stdout(&run.stdout.text) {
                    if reason.is_empty() {
                        reason = json
                            .hook_specific_output
                            .as_ref()
                            .and_then(|specific| specific.permission_decision_reason.clone())
                            .or_else(|| json.reason.clone())
                            .unwrap_or_default();
                    }
                    // 只应用 non-decision 字段（systemMessage / updatedInput /
                    // additionalContext）；决策由下面的 exit-2 强制 deny 决定，
                    // 避免同一个 hook 的 deny 被计数两次。
                    if let Some(updated) =
                        self.apply_json_output(outcome, state, source, &json, Some(&reason), false)
                    {
                        *current_input = Some(updated.clone());
                        *updated_input_out = Some(updated);
                    }
                }
                if reason.is_empty() {
                    reason = "Blocked by hook".to_string();
                }
                if outcome.event.blocks_on_exit_two() {
                    self.apply_decision(outcome, state, source, HookDecision::Deny, Some(reason));
                } else if outcome.event.feedback_on_exit_two() {
                    outcome.model_feedback.push(reason);
                } else {
                    outcome.warnings.push(format!(
                        "{source} command hook `{}` returned exit code 2, which is not blocking for {}; ignored",
                        command.command, outcome.event
                    ));
                }
            }
            Some(code) => {
                record.outcome = HookAuditOutcome::Warning;
                let stderr = run.stderr.text.trim();
                outcome.warnings.push(format!(
                    "{source} command hook `{}` exited with status {code}{}",
                    command.command,
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    }
                ));
            }
            None => {
                record.outcome = HookAuditOutcome::Warning;
                outcome.warnings.push(format!(
                    "{source} command hook `{}` was terminated by a signal",
                    command.command
                ));
            }
        }

        self.finish_run_record(&mut record, outcome, warnings_start);
        record
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_http_run(
        &self,
        outcome: &mut HookOutcome,
        state: &mut HookTurnState,
        source: HookSource,
        hook: &HttpHookConfig,
        run: http::HttpRun,
        current_input: &mut Option<Value>,
        updated_input_out: &mut Option<Value>,
    ) -> HookRunRecord {
        let mut record = HookRunRecord {
            event: outcome.event,
            source,
            handler: HookHandlerKind::Http,
            summary: hook.url.clone(),
            outcome: HookAuditOutcome::Success,
            exit_code: None,
            http_status: run.status,
            stdout: run.body.text.clone(),
            stderr: String::new(),
            output_truncated: run.body.truncated,
            duration: run.duration,
            asynchronous: false,
            warnings: Vec::new(),
        };
        let warnings_start = outcome.warnings.len();

        for warning in &run.warnings {
            outcome
                .warnings
                .push(format!("{source} HTTP hook `{}`: {warning}", hook.url));
        }

        if run.timed_out {
            record.outcome = HookAuditOutcome::Timeout;
            outcome.warnings.push(format!(
                "{source} HTTP hook `{}` timed out after {}s",
                hook.url,
                hook.timeout.as_secs_f32()
            ));
        } else if let Some(error) = run.error {
            record.outcome = HookAuditOutcome::SpawnError;
            outcome
                .warnings
                .push(format!("{source} HTTP hook `{}` failed: {error}", hook.url));
        } else if !matches!(run.status, Some(200..=299)) {
            record.outcome = HookAuditOutcome::Warning;
        } else if let Some(json) = run.json {
            if let Some(updated) = self.apply_json_output(outcome, state, source, &json, None, true)
            {
                *current_input = Some(updated.clone());
                *updated_input_out = Some(updated);
            }
        } else if !run.body.text.trim().is_empty() {
            record.outcome = HookAuditOutcome::InvalidOutput;
        }

        self.finish_run_record(&mut record, outcome, warnings_start);
        record
    }

    /// 应用 JSON 输出；返回 `updatedInput`（仅 `PreToolUse`，且必须是对象）。
    fn apply_json_output(
        &self,
        outcome: &mut HookOutcome,
        state: &mut HookTurnState,
        source: HookSource,
        json: &HookJsonOutput,
        fallback_reason: Option<&str>,
        apply_decisions: bool,
    ) -> Option<Value> {
        if json.continue_execution == Some(false) {
            outcome.continue_execution = false;
            if let Some(reason) = &json.stop_reason {
                outcome.stop_reason = Some(reason.clone());
            }
        }
        if json.suppress_output == Some(true) {
            outcome.suppress_output = true;
        }
        if let Some(message) = &json.system_message {
            outcome.system_messages.push(message.clone());
        }

        let mut decision: Option<HookDecision> = None;
        let mut reason = json.reason.clone();
        match json.decision.as_deref() {
            Some("approve" | "allow") => {
                if outcome.event == HookEvent::PreToolUse {
                    decision = Some(HookDecision::Allow);
                }
            }
            Some("ask") => decision = Some(HookDecision::Ask),
            Some("block" | "deny") => decision = Some(HookDecision::Deny),
            Some(other) => outcome.warnings.push(format!(
                "{source} hook returned unknown decision {other:?}; ignored"
            )),
            None => {}
        }

        let mut updated_input = None;
        if let Some(specific) = &json.hook_specific_output {
            let event_matches = specific
                .hook_event_name
                .as_deref()
                .map(|name| name == outcome.event.as_str())
                .unwrap_or(true);
            if !event_matches {
                outcome.warnings.push(format!(
                    "{source} hook returned hookSpecificOutput for {:?}, expected {:?}; event-specific fields ignored",
                    specific.hook_event_name.as_deref().unwrap_or(""),
                    outcome.event.as_str()
                ));
            } else {
                if let Some(permission) = &specific.permission_decision {
                    if outcome.event == HookEvent::PreToolUse {
                        match permission.as_str() {
                            "allow" => decision = Some(HookDecision::Allow),
                            "ask" => decision = Some(HookDecision::Ask),
                            "deny" => decision = Some(HookDecision::Deny),
                            other => outcome.warnings.push(format!(
                                "{source} hook returned unknown permissionDecision {other:?}; ignored"
                            )),
                        }
                        if let Some(permission_reason) = &specific.permission_decision_reason {
                            reason = Some(permission_reason.clone());
                        }
                    } else {
                        outcome.warnings.push(format!(
                            "{source} hook set permissionDecision on {}; only PreToolUse uses it",
                            outcome.event
                        ));
                    }
                }

                if let Some(context) = &specific.additional_context {
                    if outcome.event.accepts_additional_context() {
                        outcome.additional_context.push(context.clone());
                    } else {
                        outcome.warnings.push(format!(
                            "{source} hook set additionalContext on {}; event does not accept it",
                            outcome.event
                        ));
                    }
                }

                if let Some(input) = &specific.updated_input {
                    if outcome.event == HookEvent::PreToolUse {
                        if input.is_object() {
                            updated_input = Some(input.clone());
                        } else {
                            outcome.warnings.push(format!(
                                "{source} hook returned non-object updatedInput; ignored"
                            ));
                        }
                    } else {
                        outcome.warnings.push(format!(
                            "{source} hook set updatedInput on {}; only PreToolUse can rewrite tool input",
                            outcome.event
                        ));
                    }
                }

                if let Some(retry) = specific.retry {
                    if outcome.event == HookEvent::PermissionDenied {
                        outcome.retry = Some(retry);
                    } else {
                        outcome.warnings.push(format!(
                            "{source} hook set retry on {}; only PermissionDenied uses it",
                            outcome.event
                        ));
                    }
                }
            }
        }

        if apply_decisions && let Some(decision) = decision {
            self.apply_decision(
                outcome,
                state,
                source,
                decision,
                reason.or_else(|| fallback_reason.map(str::to_string)),
            );
        }

        updated_input
    }

    fn apply_decision(
        &self,
        outcome: &mut HookOutcome,
        state: &mut HookTurnState,
        source: HookSource,
        decision: HookDecision,
        reason: Option<String>,
    ) {
        let mut decision = decision;
        if decision == HookDecision::Deny
            && self.config.max_denies_per_turn > 0
            && state.deny_count >= self.config.max_denies_per_turn
        {
            outcome.deny_cap_reached = true;
            outcome.warnings.push(format!(
                "{source} hook deny downgraded to ask: the per-turn deny cap ({}) was reached",
                self.config.max_denies_per_turn
            ));
            decision = HookDecision::Ask;
        } else if decision == HookDecision::Deny {
            state.deny_count = state.deny_count.saturating_add(1);
        }

        outcome.decision = outcome.decision.merge(decision);
        if matches!(decision, HookDecision::Deny | HookDecision::Ask)
            && let Some(reason) = reason.filter(|reason| !reason.trim().is_empty())
        {
            match &mut outcome.reason {
                Some(existing) => {
                    existing.push('\n');
                    existing.push_str(&reason);
                }
                None => outcome.reason = Some(reason),
            }
        }
    }

    fn start_async_command(
        &self,
        source: HookSource,
        event: HookEvent,
        command: &CommandHookConfig,
        depth: u32,
        input_json: &str,
    ) -> HookRunRecord {
        self.config.audit.record(HookAuditRecord {
            event,
            source,
            handler: HookHandlerKind::Command,
            summary: command.command.clone(),
            outcome: HookAuditOutcome::AsyncStarted,
            exit_code: None,
            http_status: None,
            duration: Duration::ZERO,
            asynchronous: true,
            warnings: Vec::new(),
        });

        let audit = Arc::clone(&self.config.audit);
        let context = CommandContext {
            project_root: self.config.project_root.clone(),
            env_allowlist: self.config.env_allowlist.clone(),
            depth,
            event,
            source,
        };
        let command_text = command.command.clone();
        let timeout = command.timeout;
        let input = input_json.to_string();
        let summary = command.command.clone();

        tokio::spawn(async move {
            let run = command::run_command(&command_text, timeout, &input, &context).await;
            let outcome = if run.timed_out {
                HookAuditOutcome::Timeout
            } else if run.spawn_error.is_some() {
                HookAuditOutcome::SpawnError
            } else {
                match run.exit_code {
                    Some(0) => HookAuditOutcome::Success,
                    Some(2) => HookAuditOutcome::Blocked,
                    _ => HookAuditOutcome::Warning,
                }
            };
            // async hook 的结果只进审计；非零退出时把 stderr 保留在 warnings 里，
            // 否则审计只能看到一个没有上下文的 `warning`。
            let warnings = match run.exit_code {
                Some(code) if code != 0 && code != 2 => {
                    let stderr = run.stderr.text.trim();
                    vec![format!(
                        "{source} async command hook `{command_text}` exited with status {code}{}",
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!(": {stderr}")
                        }
                    )]
                }
                _ => Vec::new(),
            };
            audit.record(HookAuditRecord {
                event,
                source,
                handler: HookHandlerKind::Command,
                summary,
                outcome,
                exit_code: run.exit_code,
                http_status: None,
                duration: run.duration,
                asynchronous: true,
                warnings,
            });
        });

        HookRunRecord {
            event,
            source,
            handler: HookHandlerKind::Command,
            summary: command.command.clone(),
            outcome: HookAuditOutcome::AsyncStarted,
            exit_code: None,
            http_status: None,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
            duration: Duration::ZERO,
            asynchronous: true,
            warnings: Vec::new(),
        }
    }

    /// 把本次 handler 产生的警告写进运行记录，再写审计。
    fn finish_run_record(
        &self,
        record: &mut HookRunRecord,
        outcome: &HookOutcome,
        warnings_start: usize,
    ) {
        record.warnings = outcome.warnings[warnings_start..].to_vec();
        self.audit(record);
    }

    fn audit(&self, record: &HookRunRecord) {
        self.config.audit.record(HookAuditRecord {
            event: record.event,
            source: record.source,
            handler: record.handler,
            summary: record.summary.clone(),
            outcome: record.outcome,
            exit_code: record.exit_code,
            http_status: record.http_status,
            duration: record.duration,
            asynchronous: record.asynchronous,
            warnings: record.warnings.clone(),
        });
    }
}
