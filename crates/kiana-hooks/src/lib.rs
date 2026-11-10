//! Hooks：把用户配置的 shell / HTTP 脚本挂到会话与工具生命周期上。
//!
//! # 这个 crate 解决什么问题
//!
//! 固定流程只能覆盖内置策略；用户需要在自己的仓库里做审计、格式化、策略拦截、
//! 上下文注入。本 crate 把 `{ Event: [{ matcher, hooks: [...] }] }` 配置编译成
//! 可执行 handler，并实现**退出码 / JSON 输出契约**、顺序聚合与安全边界。
//!
//! 本 crate 不负责权限判断（`kiana-permissions`）、不负责会话编排
//! （`kiana-session`）、不负责配置分层（`kiana-config`）。它只回答三件事：
//!
//! 1. 某个事件应该跑哪些 hook、按什么顺序跑；
//! 2. 每个 hook 的 stdin / 退出码 / stdout 怎么解释；
//! 3. 多个 hook 的结果怎么合并成一个 [`HookOutcome`]。
//!
//! # 事件
//!
//! 支持 [`HookEvent`]：
//! `PreToolUse`、`PostToolUse`、`PostToolUseFailure`、`PermissionDenied`、
//! `UserPromptSubmit`、`SessionStart`、`SessionEnd`、`Stop`、`PreCompact`、
//! `PostCompact`。每个事件能匹配的字段不同：
//!
//! | 事件 | matcher 匹配对象 |
//! |---|---|
//! | `PreToolUse` / `PostToolUse` / `PostToolUseFailure` / `PermissionDenied` | `tool_name` |
//! | `SessionStart` | `source` |
//! | `SessionEnd` | `reason` |
//! | `PreCompact` / `PostCompact` | `trigger` |
//! | `UserPromptSubmit` / `Stop` | 无（配置了 matcher 也视为匹配全部） |
//!
//! # 配置
//!
//! ```json
//! {
//!   "PreToolUse": [
//!     {
//!       "matcher": "bash|write",
//!       "hooks": [
//!         { "type": "command", "command": "scripts/guard.sh", "timeout": 30 },
//!         { "type": "http", "url": "https://policy.example/hook",
//!           "headers": { "Authorization": "Bearer $HOOK_TOKEN" },
//!           "allowedEnvVars": ["HOOK_TOKEN"] }
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! - `matcher`：空 / `*` / `.*` 匹配全部；纯标识符（含 `.`、`-`、`:`）与 `|`
//!   分隔列表按**精确名**匹配；其它形式按**正则**编译。编译在加载期完成，匹配期
//!   不再编译；非法正则产生诊断并丢弃该 matcher 组。
//! - `command` handler：`timeout` 单位是秒（默认 30s），`async: true` 时只启动并
//!   写审计，不参与本轮决策。
//! - `http` handler：只允许 `http` / `https`；`headers` 的值支持
//!   `$VAR` / `${VAR}`，但只有列在 `allowedEnvVars` 里的变量才会被替换，其它引用
//!   替换为空串。URL 与 header 值都会做注入清洗。
//!
//! # 执行契约
//!
//! ## stdin
//!
//! 每个 hook 从 stdin 收到一行 JSON（[`HookInput`]）：
//! `session_id`、`cwd`、`hook_event_name`、`transcript_path`、`permission_mode`，
//! 以及按事件出现的 `tool_name` / `tool_input` / `tool_response` / `error` /
//! `prompt` / `source` / `reason` / `trigger` / `custom_instructions`。
//! `cwd` 与进程工作目录都固定为项目根（`HookRunnerConfig::project_root`）。
//!
//! ## 退出码
//!
//! | 退出码 | 行为 |
//! |---|---|
//! | `0` | 成功；解析 stdout（纯文本或 JSON） |
//! | `2` | 阻塞；stderr 作为模型可见错误；审计记为 blocked |
//! | 其它 | 只给用户警告，工具/会话继续 |
//! | 超时 | 杀掉整个进程组，按警告处理 |
//!
//! 退出码 2 的“阻塞”按事件解释：`PreToolUse` 拒绝工具调用，`UserPromptSubmit`
//! 阻止提交，`Stop` 阻止结束回合，`PreCompact` 阻止压缩；`PostToolUse` /
//! `PostToolUseFailure` / `PermissionDenied` 把 stderr 作为模型反馈；`SessionStart` /
//! `SessionEnd` / `PostCompact` 忽略阻塞，只记警告。
//!
//! ## stdout JSON
//!
//! `0` 退出的 stdout 以 `{` 开头时按 JSON 解析，支持：
//! `continue`、`stopReason`、`suppressOutput`、`systemMessage`、
//! `hookSpecificOutput.hookEventName` / `permissionDecision` /
//! `permissionDecisionReason` / `updatedInput` / `additionalContext`；
//! 以及通用 `decision` / `reason`。JSON 无效时按警告处理，不改变决策。
//!
//! # 顺序与聚合
//!
//! - 来源优先级：`local > project > user`（[`HookSource::priority`]）；
//! - 同一来源内按配置声明顺序执行（先 matcher 组顺序，再组内 `hooks` 顺序）；
//! - 决策按 `deny > ask > allow > passthrough` 合并，任一 deny 胜出；
//! - `updatedInput` **整体替换**当前工具参数，并顺序传给后续 hook：第二个 hook
//!   的 stdin 里看到的是第一个 hook 改写后的 `tool_input`；
//! - async hook 立即返回，结果只进审计；
//! - 每轮（[`HookTurnState`]）的 deny 次数有上限，超过后 deny 降级为 `ask` 并记
//!   警告，避免 hook 与模型互相拉扯形成死循环。
//!
//! 接线顺序必须是「hook 改写 → 权限判断 → 执行」：`updatedInput` 不能绕过
//! `kiana-permissions` 的 deny。
//!
//! # 安全边界
//!
//! - **trust gating**：`Project` / `Local` 来源在项目未受信时整体丢弃；
//! - **环境变量白名单**：command hook 只拿到 [`DEFAULT_HOOK_ENV_ALLOWLIST`] 中的变量，
//!   以及本 crate 注入的 `KIANA_PROJECT_DIR` / `KIANA_HOOK_EVENT` /
//!   `KIANA_HOOK_SOURCE` / `KIANA_HOOK_DEPTH`；API key 等默认不可见；
//! - **cwd 固定**：进程工作目录与 JSON `cwd` 都是项目根；
//! - **输出 64KB 截断**：stdout / stderr / HTTP body 单流最多保留
//!   [`MAX_HOOK_OUTPUT_BYTES`] 字节，超出部分继续读走但丢弃；
//! - **进程组**：Unix 上 command hook 在独立进程组中运行，超时对 `-pid` 发
//!   `SIGKILL`，并带取消保护（future 被 drop 时同样清理）；
//! - **SSRF**：HTTP handler 只接受 http/https，拒绝私网、链路本地、CGNAT 与云
//!   元数据地址，**允许 loopback**；DNS 解析在 connect 时校验（自定义
//!   `reqwest::dns::Resolve`），不是请求前的一次性预检；重定向手动逐跳处理，
//!   每跳重新校验 URL，跨源时剥离 `Authorization` / `Cookie` /
//!   `Proxy-Authorization`；
//! - **递归深度**：`HookTurnState::depth` 超过 `HookRunnerConfig::max_depth` 时返回
//!   [`HookError::RecursionLimit`]；command hook 子进程拿到 `KIANA_HOOK_DEPTH`
//!   （当前深度 + 1），由被 hook 启动的 Kiana 进程续接；
//! - **additionalContext 注入**：使用 [`HookOutcome::additional_context_xml`] 做
//!   XML 转义（`&` / `<` / `>`），不要直接把原始字符串拼进 prompt。
//!
//! # 一次典型调用
//!
//! ```
//! use std::path::PathBuf;
//! use serde_json::json;
//! use kiana_hooks::{
//!     HookEvent, HookInput, HookRegistry, HookRunner, HookRunnerConfig, HookSource,
//!     HooksConfig,
//! };
//!
//! # async fn run() -> Result<(), kiana_hooks::HookError> {
//! let load = HooksConfig::from_value(&json!({
//!     "PreToolUse": [{
//!         "matcher": "bash",
//!         "hooks": [{ "type": "command", "command": "true" }]
//!     }]
//! }));
//! let mut registry = HookRegistry::new();
//! registry.add_source(HookSource::User, load, true);
//!
//! let runner = HookRunner::new(registry, HookRunnerConfig::new(PathBuf::from("/tmp")))?;
//! let input = HookInput::new(HookEvent::PreToolUse, "session", "/tmp")
//!     .with_tool_call("bash", json!({ "command": "ls" }));
//! let outcome = runner.run_event(&input).await?;
//! assert!(!outcome.is_denied());
//! # Ok(())
//! # }
//! ```
//!
//! # 边界与失败模式
//!
//! - 配置解析是**宽松**的：未知事件、坏 matcher、坏 handler 都变成
//!   [`HookConfigDiagnostic`] 并跳过，不会让整个配置文件失效。
//! - 单个 hook 的 spawn 失败、非零退出、超时、非法 JSON 都不返回 `Err`；它们进入
//!   [`HookOutcome::warnings`] 与审计。只有递归超限、缺少必要事件字段、HTTP
//!   策略正则非法等**调用方错误**返回 [`HookError`]。
//! - `HookRunner` 不持有全局状态；`HookTurnState` 由调用方按“回合”创建并在同一
//!   回合内复用。事件之间（例如 `PreToolUse` 与 `Stop`）是否共享 deny 计数由调用方
//!   决定；默认建议一轮一个 state。

mod audit;
mod command;
mod config;
mod error;
mod event;
mod http;
mod input;
mod matcher;
mod output;
mod runner;
mod ssrf;

pub use audit::{HookAuditOutcome, HookAuditRecord, HookAuditSink, HookHandlerKind, NoopAuditSink};
pub use command::{CappedOutput, DEFAULT_HOOK_ENV_ALLOWLIST, MAX_HOOK_OUTPUT_BYTES};
pub use config::{
    CommandHookConfig, DEFAULT_COMMAND_TIMEOUT, DEFAULT_HTTP_TIMEOUT, HookConfigDiagnostic,
    HookConfigLoad, HookHandlerConfig, HookMatcherGroup, HooksConfig, HttpHookConfig,
};
pub use error::HookError;
pub use event::{HookDecision, HookEvent, HookSource};
pub use http::HttpHookPolicy;
pub use input::HookInput;
pub use matcher::{HookMatcher, MatcherError};
pub use output::{
    HookJsonOutput, HookSpecificOutput, ParsedStdout, escape_additional_context, parse_stdout,
};
pub use runner::{
    DEFAULT_MAX_DENIES_PER_TURN, DEFAULT_MAX_HOOK_DEPTH, HOOK_DEPTH_ENV, HookOutcome, HookRegistry,
    HookRunRecord, HookRunner, HookRunnerConfig, HookTurnState, current_hook_depth,
};
