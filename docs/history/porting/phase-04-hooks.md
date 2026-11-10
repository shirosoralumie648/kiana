# W4 — Hooks 配置、派发与执行契约

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 4 周（Hooks）。目标：让用户通过 settings 里的
`hooks` 字段，在工具调用前后、权限拒绝、prompt 提交、会话开始/结束、回合结束、
压缩前后挂 command / HTTP 脚本，并保证顺序、聚合、退出码与安全边界可预测。

## 参考设计对照表

只借鉴设计，代码按 Kiana 的类型与运行时重写；没有复制任何参考项目源码。

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
|---|---|---|
| `reference_project/ClaudeCode/src/types/hooks.ts` | 同步/异步 hook 输出 schema：`continue` / `stopReason` / `suppressOutput` / `systemMessage` / `decision` / `reason` / `hookSpecificOutput.*`；`permissionDecision` 三值；`updatedInput` 为对象 | `crates/kiana-hooks/src/output.rs`：`HookJsonOutput`、`HookSpecificOutput`、`ParsedStdout` |
| `reference_project/ClaudeCode/src/utils/hooks.ts` | 基础 stdin 字段（`session_id` / `transcript_path` / `cwd` / `hook_event_name`）；exit 0/2/其它退出码语义；stdout 先按首字符判断 JSON；matcher 的“纯名称/管道列表/正则”三态；聚合时 deny 优先 | `src/input.rs::HookInput`；`src/runner.rs::{apply_command_run, apply_decision}`；`src/matcher.rs::HookMatcher` |
| `reference_project/ClaudeCode/src/utils/hooks/hooksConfigManager.ts` | 每个事件有独立的 matcher 字段（tool_name / source / trigger / reason）；事件元数据表 | `src/event.rs::HookEvent::{matcher_field, blocks_on_exit_two, feedback_on_exit_two, plain_stdout_is_context}` |
| `reference_project/ClaudeCode/src/utils/hooks/execHttpHook.ts` | HTTP hook 是 POST JSON；header 值支持 `$VAR`，只解析 `allowedEnvVars`；CR/LF/NUL 注入清洗；策略层 `allowedHttpHookUrls` 与 `httpHookAllowedEnvVars` 取交集 | `src/http.rs::{build_headers, interpolate_header_value, sanitize_header_value, check_url_policy}` |
| `reference_project/ClaudeCode/src/utils/hooks/ssrfGuard.ts` | 阻断私网/链路本地/CGNAT/保留地址与云元数据；显式允许 loopback；IPv4-mapped IPv6 也要检查 | `src/ssrf.rs::{is_blocked_ip, is_metadata_ip, validate_url, GuardedDnsResolver}` |
| `reference_project/qwen-code/packages/core/src/hooks/types.ts` | 事件专属输入/输出类型；`updatedInput` 是**整体替换**而不是 merge；`additionalContext` 注入前做 `<`/`>` 转义 | `src/input.rs`；`src/runner.rs::apply_json_output`；`src/output.rs::escape_additional_context` + `src/runner.rs::HookOutcome::additional_context_xml` |
| `reference_project/qwen-code/packages/core/src/hooks/hookAggregator.ts` | 决策排名 `deny > ask > allow > passthrough`；reason 用换行拼接；`continue:false` 优先；同一事件按声明顺序 | `src/event.rs::HookDecision::{rank, merge}`；`src/runner.rs::{apply_decision, run_event_with_state}` |
| `reference_project/qwen-code/packages/core/src/hooks/hook-matcher.ts` | 空/`*`/`.*` 匹配全部；纯名称与管道分隔列表精确匹配；其它按正则；非法正则不参与匹配 | `src/matcher.rs::HookMatcher::compile`（非法正则在加载期产生诊断并丢弃 matcher 组） |
| `reference_project/qwen-code/packages/core/src/hooks/ssrfGuard.ts` + `urlValidator.ts` | 具体地址段（0/8、10/8、100.64/10、169.254/16、172.16/12、192.168/16、fc00::/7、fe80::/10、`::`）与元数据 IP `169.254.169.254` / `100.100.100.200`；只允许 http/https | `src/ssrf.rs::is_blocked_v4 / is_blocked_v6 / BLOCKED_HOSTS` |
| `reference_project/qwen-code/packages/core/src/hooks/hookRunner.ts` + `hook-timeout.ts` | 独立进程组 + `kill(-pid)`；超时先 SIGKILL 组；`kill_on_drop`；输出上限 | `src/command.rs::{run_command, kill_process_group, ProcessGroupGuard, read_capped_into}`；`MAX_HOOK_OUTPUT_BYTES = 64 KiB` |
| `reference_project/qwen-code/packages/core/src/hooks/asyncHookRegistry.ts` | async hook 立即返回、结果只进审计（不参与本轮决策）；后台任务仍有超时与回收 | `src/runner.rs::start_async_command`；`src/audit.rs` |
| `reference_project/qwen-code/packages/core/src/hooks/stopHookCap.ts` | 每轮 continuation/deny 有上限，达到后降级并给出用户警告，避免 hook 与模型死循环 | `src/runner.rs::{apply_decision, HookTurnState, DEFAULT_MAX_DENIES_PER_TURN}` |
| `reference_project/qwen-code/packages/core/src/hooks/hookPlanner.ts` + `hookRegistry.ts` | 注册表按事件/ matcher 过滤；来源有优先级；执行计划按来源与声明顺序生成 | `src/runner.rs::HookRegistry::{add_source, matching_hooks}`（按任务要求把顺序固定为 local > project > user，而不是 qwen 的 project > user > system） |
| `reference_project/codex/codex-rs/hooks/src/engine/matcher.rs` | 编译后的 `All / Exact / Regex` 三态；匹配期不再编译 | `src/matcher.rs`（额外支持 `-` / `.` / `:` 名称与带空格管道列表） |
| `reference_project/codex/codex-rs/hooks/src/engine/output_parser.rs` | 按事件分别解析输出，把 “block 原因” 与 “非法输出原因” 分开，避免坏 JSON 变成静默放行 | `src/runner.rs` 的 exit 0 / exit 2 分支 + `ParsedStdout::InvalidJson` 警告 |
| `reference_project/codex/codex-rs/hooks/src/engine/command_runner.rs` | 命令执行前做 trust 校验；异步任务有并发上限；超时错误与退出码分开记录 | `src/runner.rs::HookRegistry::add_source(trusted)`；`src/command.rs`（并发上限未实现，见风险） |
| `reference_project/gemini-cli/packages/core/src/hooks/types.ts` | `ConfigSource` 的优先级枚举（Runtime/Project/User/System/Extensions） | `src/event.rs::HookSource::{Local, Project, User} + priority()` |
| `reference_project/gemini-cli/packages/core/src/hooks/hookRunner.ts` | 顺序执行时把上一个 hook 的改写结果传给下一个；显式 `sequential` 与并行两种策略 | `src/runner.rs::run_event_with_state` 的 `current_input` 链（本 crate 统一顺序执行，updatedInput 必须顺序传递） |
| `reference_project/gemini-cli/packages/core/src/hooks/trustedHooks.ts` | project hook 需要单独的信任记录；未信任的 hook 不执行 | `src/runner.rs::HookRegistry::add_source` 的 trust gating（信任来源由调用方传入） |

## 改了什么

### 1. `src/event.rs`

- `HookEvent`：10 个事件，JSON 名与 `hook_event_name` 一致；`from_name` 先精确后忽略
  大小写；`ALL` 顺序稳定。
- 事件语义表：
  - `matcher_field`：工具事件 → `tool_name`；`SessionStart` → `source`；
    `SessionEnd` → `reason`；`PreCompact`/`PostCompact` → `trigger`；
    `UserPromptSubmit`/`Stop` → 无。
  - `blocks_on_exit_two`：`PreToolUse` / `UserPromptSubmit` / `Stop` / `PreCompact`。
  - `feedback_on_exit_two`：`PostToolUse` / `PostToolUseFailure` / `PermissionDenied`
    把 stderr 作为模型反馈。
  - `plain_stdout_is_context`：纯文本 stdout 作为 `additionalContext` 的事件。
- `HookSource::{Local, Project, User}`：`priority()` = 0/1/2；Local/Project 需要 trust。
- `HookDecision::{Passthrough, Allow, Ask, Deny}`：`rank` + `merge` 实现
  `deny > ask > allow > passthrough`。

### 2. `src/matcher.rs`

- 加载期编译 `HookMatcher`：`All` / `Exact(Vec<String>)` / `Regex`。
- `Exact` 支持 `|` 列表与带空格列表（`bash | write`），`.` 按字面量匹配（`mcp.foo`
  不会匹配 `mcpXfoo`）。
- 非法正则返回 `MatcherError::InvalidRegex`；配置解析把它降级为诊断并跳过该组。
- 事件没有可匹配字段时 `matches(None) == true`。

### 3. `src/input.rs`

- `HookInput`：`session_id`、`transcript_path`、`cwd`、`hook_event_name`、
  `permission_mode`、`tool_name`、`tool_input`、`tool_response`、`error`、`prompt`、
  `source`、`reason`、`trigger`、`custom_instructions`；`None` 字段不序列化。
- `to_json_string` 先 `serde_json::to_value` 再走 `kiana_json::stringify`（JS 语义）。
- builder：`with_transcript_path` / `with_tool_call` / `with_tool_response` /
  `with_error` / `with_prompt` / `with_source` / `with_reason` / `with_trigger` /
  `with_custom_instructions`。

### 4. `src/config.rs`

- 配置模型：`{ Event: [{ matcher, hooks: [...] }] }`。
- `CommandHookConfig`：`command`、`timeout`（秒，默认 30s，上限 3600s）、`async`。
- `HttpHookConfig`：`url`（加载期做 scheme/host/IP 校验）、`headers`、
  `allowedEnvVars`（camel/snake 都接受）、`timeout`（默认 30s）。
- `HooksConfig::from_value` / `from_settings`：宽松解析；未知事件、坏 matcher、坏
  handler、非法 URL、非法 timeout 都进入 `HookConfigDiagnostic` 并跳过，不影响同一
  文件里的其它 hook。
- `HookRegistry::add_source` 的 trust gating 也用 `HookConfigDiagnostic` 报告。

### 5. `src/output.rs`

- `HookJsonOutput` / `HookSpecificOutput` 的 serde 映射（camelCase）。
- `parse_stdout`：空 → `Empty`；非 `{` 开头 → `Plain`；JSON 语法/字段类型错误 →
  `InvalidJson`；成功 → `Json`。
- `escape_additional_context`：`&` → `&amp;`、`<` → `&lt;`、`>` → `&gt;`。

### 6. `src/ssrf.rs`

- `is_blocked_ip`：IPv4 0/8、10/8、100.64/10、169.254/16、172.16/12、192.168/16、
  198.18/15、240/4；IPv6 `::`、fc00::/7、fe80::/10、IPv4-mapped/compatible 递归检查；
  loopback（127/8、::1）显式允许。
- `is_metadata_ip`：169.254.169.254、100.100.100.200（含 IPv4-mapped）。
- `validate_url`：只允许 http/https、禁止 URL 内嵌凭据、IP 字面量与元数据域名直接
  拒绝。
- `GuardedDnsResolver`：实现 `reqwest::dns::Resolve`，在 connect 的 DNS 结果上检查；
  DNS rebinding 不再依赖请求前的一次性预检。

### 7. `src/command.rs`

- `run_command`：`/bin/sh -c`（Windows `cmd /C`），cwd 固定项目根。
- 环境变量：`env_clear()` + `build_hook_environment`（默认白名单 +
  `KIANA_PROJECT_DIR`/`KIANA_HOOK_EVENT`/`KIANA_HOOK_SOURCE`/`KIANA_HOOK_DEPTH`）。
- stdin：写入一行 JSON 后 shutdown；写入失败（EPIPE）不阻塞。
- stdout/stderr：每个流最多保留 64 KiB，超出继续读走并记 `truncated`/总字节数。
- 超时：Unix 上 `process_group(0)` 建组，超时 `kill(-pid, SIGKILL)`，再兜底
  `start_kill`；`ProcessGroupGuard` 在 future 被取消时也杀组。
- 子进程退出后 reader 有 2s 宽限，避免后台孙进程占住管道。

### 8. `src/http.rs`

- `build_client`：`redirect(Policy::none())`、`no_proxy()`、自定义
  `GuardedDnsResolver`；请求总超时覆盖整条重定向链。
- header：`Content-Type: application/json` 默认；配置 header 做名称/值校验与
  CR/LF/NUL 清洗；`$VAR`/`${VAR}` 只对 `allowedEnvVars`（再与全局策略取交集）生效。
- 重定向：最多 5 跳；每跳 `validate_url` + URL 白名单重新校验；跨源剥离
  `Authorization` / `Cookie` / `Proxy-Authorization`；301/302/303 转 GET，307/308
  保留 POST。
- 响应 body 64 KiB 截断；HTTP hook 必须返回 JSON（纯文本作为警告忽略）；非 2xx
  只警告。

### 9. `src/runner.rs`

- `HookRegistry`：`add_source(source, load, trusted)`；Local/Project 未受信时整体丢弃。
- `matching_hooks`：按事件 + matcher 过滤，稳定排序 `local > project > user`；
  同一来源保持声明顺序。
- `HookRunner::run_event{,_with_state}`：
  - 校验事件必要字段（工具事件必须有 `tool_name`）；
  - 深度超过 `max_depth` 返回 `HookError::RecursionLimit`；
  - 每个 hook 的 `cwd` 强制为项目根，`tool_input` 使用当前累计值；
  - command / HTTP 分别走 `apply_command_run` / `apply_http_run`；
  - `updatedInput` 整体替换并顺序传递，最终写入 `HookOutcome::updated_input`；
  - 决策合并 + 每轮 deny 上限（超限降级为 `Ask` 并记警告）；
  - async command 立即返回 `AsyncStarted`，后台结果只写审计。
- `HookOutcome`：`decision` / `reason` / `updated_input` / `additional_context` /
  `system_messages` / `model_feedback` / `stop_reason` / `continue_execution` /
  `suppress_output` / `retry` / `warnings` / `runs` / `deny_cap_reached`；
  `blocking_error()` 与 `additional_context_xml()` 给会话层直接使用。
- `HookTurnState`：`deny_count` + `depth`；`new()` 从 `KIANA_HOOK_DEPTH` 初始化。
- `HookRunnerConfig`：项目根、环境白名单、HTTP 策略、deny 上限、递归深度、审计 sink。

### 10. `src/audit.rs`

- `HookAuditSink` trait + `NoopAuditSink`；`HookAuditRecord` 带 event/source/handler/
  summary/outcome/exit_code/http_status/duration/asynchronous。
- async hook 的启动与完成各一条；同步 hook 每次执行一条。

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana`
下实际执行：

```text
$ cargo test -p kiana-hooks
   unit tests:        28 passed
   hooks_integration: 18 passed
   http_hooks:         7 passed
   doc-tests:          1 passed
   合计 54 passed；0 failed；0 ignored

$ cargo check -p kiana-hooks --all-targets
Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.05s

$ cargo clippy -p kiana-hooks --all-targets
Finished `dev` profile ...（0 warnings）

$ cargo doc -p kiana-hooks --no-deps
Finished ...（0 warnings）

$ cargo fmt -p kiana-hooks -- --check
（无输出，退出码 0）
```

覆盖的验收场景：

- `PreToolUse` exit 2：工具阻塞、stderr 作为唯一 `blocking_error()`、审计
  `Blocked`、matcher 不匹配的 `read` 不执行；
- exit 1：只记警告、决策保持 passthrough；
- exit 0 纯文本：`UserPromptSubmit` 进入 `additionalContext`，`PreToolUse` 不进入；
- 完整 JSON 契约：`continue` / `stopReason` / `suppressOutput` / `systemMessage` /
  `permissionDecision` / `updatedInput` / `additionalContext`；
- 来源顺序 local → project → user；user allow + project deny 最终 deny；
- 两个 `PreToolUse` hook 的 `updatedInput` 顺序传递（第二个 hook 的 stdin 看到第一个
  改写后的参数），且后续 deny 不能被改写绕过；
- 未受信 project/local 不加载，user 仍加载；
- deny 上限：前两次 deny 生效，第三次降级；同一轮下一次调用直接得到 `Ask`；
- 递归深度 typed error + `KIANA_HOOK_DEPTH` 子进程 +1；
- 进程 cwd 与 stdin `cwd` 都固定项目根；
- 环境变量白名单注入与 64 KiB 截断；
- async hook 立即返回、不参与决策、完成结果进审计；
- 超时杀掉整个进程组（后台 `sleep` 无残留）；
- 事件专属 exit 2 语义（`PostToolUseFailure` 反馈、`SessionStart` 忽略、`Stop` 拒绝）；
- 非法 JSON 只警告；
- stdin 携带 `transcript_path` / `permission_mode` / `tool_response` / `error`；
- HTTP loopback 成功、跨源重定向剥离 Authorization/Cookie 并重新校验、重定向到
  `169.254.169.254` 被拒、URL 白名单、非 2xx、超时、纯文本 body、header CRLF 清洗。

## 未完成项

- **接进 `kiana-session` 未做**（文件所有权限制）：PreToolUse → 权限判断 → 执行、
  PostToolUse/PostToolUseFailure、PermissionDenied、UserPromptSubmit、
  SessionStart/SessionEnd、Stop、PreCompact/PostCompact 的接线说明见
  `docs/history/porting/handoffs/phase-04.md`。因此「`updatedInput` 被权限层看到」只有 crate
  内的顺序传递测试，没有端到端权限测试（partial）。
- **Windows**：命令 shell 是 `cmd /C`，但进程组 kill 只在 Unix 实现；Windows 上依赖
  `kill_on_drop` 杀直接子进程，没有 job object，未在本机测试（partial）。
- **async 只支持配置级**：`{"async": true}` 的 handler 会立即后台执行；ClaudeCode 的
  “stdout 第一行返回 `{"async":true}` 再转为后台”的运行时协议没有实现。
- **async 并发上限**：每个 async hook 直接 `tokio::spawn`，没有全局并发 semaphore；
  Codex 的 8 并发上限未移植。project hook 受 trust gating 保护，风险有限，但恶意配置
  可以一次开很多进程。
- **HTTP 代理**：为了 SSRF 校验可靠，客户端显式 `no_proxy()`；企业代理环境下的 HTTP
  hook 目前不可用。
- **prompt / agent handler**：按计划留到后续周。
- **managed hooks**：`HookSource` 只有 Local/Project/User，没有 Managed/System；week-3
  的 `allowManagedHooksOnly` 只能在接线层先按“跳过非 policy 层”处理，managed hooks
  本身还不能执行（见 handoff §11）。
- **SessionEnd**：`kiana-session` 目前没有统一的 session-end 生命周期点；handoff 给的是
  在 shutdown / extension_shutdown_handler 处 best-effort 的建议，可能需要产品确认。

## 风险

- hook 是任意命令执行 + 网络访问。project/local trust gating 是唯一供应链边界，不能
  在接线时省略；`HookRegistry::add_source` 的 `trusted` 必须是真实的项目信任结论。
- `updatedInput` 必须严格按「hook 改写 → 权限判断 → 执行」接入。若先执行权限再跑
  hook，改写后的参数会绕过 deny；本 crate 只能保证顺序传递，无法强制调用方顺序。
- 环境变量白名单默认不含 `KIANA_*` 与任何 API key；接入方若把 `env_allowlist` 扩大，
  等于把秘密暴露给 hook。
- HTTP 的 DNS 校验在 connect 时执行，但显式使用代理/自定义 transport 时需要重新评估
  SSRF 边界（当前禁用代理）。
- 超时杀进程组只覆盖同一进程组：hook 自己 `setsid` / 双 fork 逃逸后无法保证清理
  （Claude Code / qwen-code 同样限制）。真正的资源隔离需要 sandbox，不应只依赖 hook
  超时。
- 输出截断是“保留前 64 KiB + 追加截断标记”，因此字符串长度会略大于 64 KiB；消费方应
  以 `output_truncated` 为准，而不是精确等长。
- async hook 的完成审计依赖 tokio task 被执行；会话立刻退出时可能丢失最后一条审计
  （没有 shutdown flush 接口）。
