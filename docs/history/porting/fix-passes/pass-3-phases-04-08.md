# Wave B 深度修复记录（2026-11-08）

本波审计共 6 条 major（week-4 三条、week-8 三条），全部修复。构建统一使用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

执行过程中 `/data` 再次被 cargo target 占满（194G/196G，ENOSPC）。处理方式：删除
`/data/cargo-target-sandbox/debug/incremental`、`debug/examples`、`debug/doc`，随后
删除 `debug/deps` 下两小时前的测试可执行文件（cargo 会按需重新链接）；没有删除源码
或测试。

## 修复清单

### 1. major（week-4）：hook 的 warnings/systemMessage/suppressOutput 没有用户可见通道

- 文件：
  - `crates/kiana-session/src/agent_session.rs:4011`（`run_hook_event`：所有事件成功后
    统一调用通知通道）、`:4032`（`emit_hook_notifications`）、`:440`/`:451`
    （新增 `AgentSessionEvent::Condensation` 与 `HookNotification`）。
  - `crates/kiana-interactive/src/interactive_mode/events.rs`（`HookNotification`：
    warning 走 `show_warning`，info 走 `show_status`）。
  - `crates/kiana-hooks/src/audit.rs`（`HookAuditRecord.warnings`）、
    `crates/kiana-hooks/src/runner.rs`（`HookRunRecord.warnings` +
    `finish_run_record`，每次 handler 运行把新增警告写进审计记录）。
- 行为：`systemMessage` 与所有非阻塞警告（exit 1、超时、非法 JSON、HTTP 非 2xx、
  SSRF/白名单拒绝）在每个 hook 事件后都转成 `hook_notification` 事件；warning 级别
  始终展示，`suppressOutput: true` 只隐藏 `systemMessage`。审计记录的 `warnings`
  保留同一份文本，`hook_audit_records()` 可读。
- 测试：`crates/kiana-session/tests/hooks_wiring.rs::exit_one_hook_warns_the_user_and_keeps_the_tool_running`
  （exit 1：工具成功执行、`hook_notification` 含 stderr、审计记录含 warning）。
- 命令与结果：`cargo test -p kiana-session --test hooks_wiring` → 14 passed。

### 2. major（week-4）：hook 输出契约只消费了 deny/ask/updatedInput/feedback

- 文件：`crates/kiana-session/src/agent_session.rs`
  - `:3972`（`PreToolUseHookEffect::{Continue, Block}`）、`:4081`（`pre_tool_use_hook`）；
  - `before_tool_call`（`permission_before_tool_call(&context, hook_allow)`）；
  - `permission_refusal_inner`（`hook_allowed && Ask` → `None`，Deny 不跳过）；
  - `after_tool_call`（PostToolUse `continue:false` → 追加 stopReason、`terminate`、
    置 abort）；
  - `run_before_settle_boundary`（Stop `continue:false` → 终止 run）；
  - `run_auto_compaction` / `compact`（PreCompact `continue:false` → 取消压缩）；
  - `post_compact_hook` / `run_session_start_hook` / `before_tool_call`
    （`additionalContext` 经 `queue_hook_context` 暂存，由 `attach_hook_contexts`
    注入下一次请求）。
- 行为：
  - PreToolUse `continue:false` 阻断本次调用（`terminate:true`）并终止 run；
  - PostToolUse `continue:false` 保留已执行结果，把 stopReason 作为模型文本追加并把
    结果标记 `terminate`，run 在批次后停止；
  - Stop `continue:false` 阻止下一轮（置 abort，不再派发 before_settle 扩展）；
  - PreCompact `continue:false` 等价于取消压缩；PostCompact `continue:false` 终止
    run；
  - `permissionDecision: allow` 只跳过权限层的 Ask，deny 规则/显式 deny 永远优先；
  - PreToolUse / SessionStart / PostCompact 的 `additionalContext` 进入下一次 provider
    请求（`<hook-context>`，已 XML 转义）。
- 测试（均端到端，经真实 `AgentSession` + faux provider 请求捕获）：
  `hooks_wiring.rs::pre_tool_use_continue_false_blocks_and_terminates_the_run`、
  `::pre_tool_use_additional_context_reaches_the_next_request`、
  `::hook_allow_skips_ask_but_not_deny`、
  `::post_tool_use_continue_false_terminates_the_run`、
  `::stop_hook_continue_false_stops_the_run`、
  `::session_start_additional_context_reaches_the_first_request`、
  `::post_compact_additional_context_reaches_the_next_request`。
- 命令与结果：`cargo test -p kiana-session --test hooks_wiring` → 14 passed。

### 3. major（week-4）：HTTP hook URL 白名单无法从 settings 配置

- 文件：`crates/kiana-session/src/agent_session.rs:2123`（`build_hook_runner` 设置
  `config.http_policy`）、`:2182`（`http_hook_policy_from_settings`）。
- 行为：从合并后的有效设置读取顶层 `allowedHttpHookUrls` /
  `httpHookAllowedEnvVars`，同时接受 `security.*` 下的同名键。键缺省 → 不限制；
  显式 `[]` → 拒绝所有（用不可匹配的空模式表达，`HttpHookPolicy` 的既有“空列表 =
  不限制”契约不被改变）；非空 glob → 必须在任何 I/O 前匹配。每个 hook 的
  `allowedEnvVars` 与全局白名单取交集。
- 测试：`hooks_wiring.rs::http_hook_url_allowlist_is_read_from_settings`
  （settings 白名单为 `allowed.example`，hook 指向 `blocked.example`：工具继续、
  `hook_notification` 与审计记录都含 “does not match the allowed URL patterns”，
  审计 outcome 为 `SpawnError`）。
- 命令与结果：`cargo test -p kiana-session --test hooks_wiring` → 14 passed；
  `cargo test -p kiana-hooks` → 54 passed。

### 4. major（week-8）：context report 与 observe_condensation 没有生产入口

- 文件：
  - `crates/kiana-session/src/agent_session.rs:11000`（`context_tokens`）、`:11005`
    （`last_condensation`）、`:11014`（`context_report`）；
  - `:8333`（`Core::compact` 记录基线）、`:8571`（提交后 token 未下降则立即 Failed，
    否则 `observe_condensation`）、`:8579`（成功发 `condensation` 事件）、
    `:8604`（失败/取消发 `phase: failed`）；
  - `:10978`（`AgentSession::compact` / `compact_with_condensation_options`）；
  - `crates/kiana-interactive/src/interactive_mode/events.rs`（`condensation` 状态行/
    警告）。
- 行为：`AgentSession::context_report(ContextReportOptions)` 用当前 projection +
  `agent.tools()` + 当前模型调用 `build_context_report`；手动压缩在提交前采集基线
  （`estimate_messages_tokens`，与 `context_tokens()` 同一估算器），提交后等待 token
  下降并稳定（默认 2.5s settle / 90s timeout），发 `AgentSessionEvent::Condensation`
  并保留在 `last_condensation()`。若压缩没有降低 token（小会话摘要可能更长）立即
  发 Failed，不空等 90s；压缩失败/取消也发 Failed。TUI 显示 saved tokens。
- 测试：`crates/kiana-session/tests/context_report_wiring.rs`：
  - `context_report_api_uses_the_current_context_and_model`（模型/provider/总 token/
    分类覆盖）；
  - `manual_compaction_emits_condensation_with_matching_saved_tokens`
    （`before_tokens == compact 前 context_tokens()`、
    `after_tokens == compact 后 context_tokens()`、`saved == 差值`、事件已发出）；
  - `manual_compaction_failure_emits_failed_condensation`。
- 命令与结果：`cargo test -p kiana-session --test context_report_wiring` →
  3 passed；`cargo test -p kiana-session --test agent_session_compaction` →
  28 passed。

### 5. major（week-8）：loop guard / budget 无法通过 shipped CLI 启用

- 文件：
  - `crates/kiana-session/src/agent_session_services.rs:206`/`:210`
    （`CreateAgentSessionFromServicesOptions.loop_guard` / `.budget_config`）；
  - `:499`（`loop_guard_from_settings_or_default`）、`:581`
    （`budget_config_from_settings`）、`create_agent_session_from_services`
    （显式选项优先，其次 settings）；
  - `crates/kiana-coding-agent/src/cli/args.rs`（`--max-turns` / `--max-tokens` /
    `--max-cost-usd` / `--max-wall-clock-ms` / `--no-loop-guard` /
    `--loop-guard-stop`）；
  - `crates/kiana-coding-agent/src/app.rs`（`build_session_options` 构造显式
    `BudgetConfig` / `LoopGuardHandle` 并传给 services）。
- 行为：services 入口默认启用保守 loop guard（`LoopGuardConfig::default()`：
  enabled、只 Warn 不 Stop）；`settings.loopGuard` 可配置 `enabled`、
  `stopOnDetection`、软/硬上限、窗口与每个检测器的 `reminderAt`/`stopAt`；
  `settings.budget` 的 `maxTurns` / `maxTokens` / `maxCostUsd` /
  `maxWallClockMs` / `warnRatios` 有任一 cap 时启用预算。显式 options 与 CLI flag
  覆盖 settings（`--no-loop-guard` 传一个 `enabled:false` 的 handle）。
- 测试：`crates/kiana-session/tests/loop_guard_wiring.rs`：
  `services_path_reads_loop_guard_and_budget_from_settings`、
  `services_path_enables_a_conservative_loop_guard_by_default`、
  `services_path_honors_an_explicit_loop_guard_override`（原有 SDK 直连测试保留）；
  `crates/kiana-coding-agent/tests/args.rs::parses_budget_and_loop_guard_flags`、
  `::rejects_invalid_budget_values`、`::parses_no_loop_guard`。
- 命令与结果：`cargo test -p kiana-session --test loop_guard_wiring` → 7 passed
  （20.72s）；`cargo test -p kiana-coding-agent --test args` → 116 passed。

### 6. major（week-8）：预算 cap 没有写进 stop_reason

- 文件：
  - `crates/kiana-ai/src/types.rs:593`（`StopReason` 新增 `TurnCapped` /
    `TokenCapped` / `CostCapped` / `TimeCapped`）；
  - `crates/kiana-agent-core/src/budget.rs:82`（`BudgetStopReason::stop_reason()`
    返回精确变体）；
  - `crates/kiana-agent-core/src/agent_loop.rs:1559`（`apply_stop_override` 写入
    精确变体；`raw_stop_reason` 仍保留 camelCase 文本）；
  - 下游穷尽 match 同步：`kiana-ai/src/telemetry.rs`、`kiana-ai/src/providers/faux.rs`、
    `kiana-agent-core/src/run_collector.rs`、`kiana-session/src/agent_session.rs`、
    `kiana-interactive/src/components/tree_selector.rs`、
    `kiana-durable/src/harness/{compaction,generation}.rs`、`kiana-evals/src/harness.rs`；
  - `crates/kiana-rpc-print/src/print_mode.rs`（cap 输出 “Request stopped: the X budget
    was exhausted.” 并以退出码 1 结束）。
- 行为：token/cost final turn 的 assistant 消息 `stop_reason` 直接是
  `TokenCapped`/`CostCapped`（不再折到 `Length`），wall-clock 请求前停止时控制器为
  `TimeCapped`。turn cap 发生在第 N 个 assistant turn 之后、下一请求之前，此时没有
  新的 assistant 消息可写 stop_reason，精确原因由 `BudgetController::stop_reason()` /
  `BudgetSnapshot.status = Capped { reason }` 表达（这是“恰好 N 个 assistant turn”
  的必然结果，不是折中）。`StopReason::Length` 现在只表示 provider 输出截断，
  overflow 恢复逻辑不会再把预算 cap 当成 context overflow。
- 测试：
  `crates/kiana-agent-core/tests/loop_guard_budget.rs::token_budget_forces_final_turn_without_tools`
  断言 `last_assistant.stop_reason == StopReason::TokenCapped`（原为 `Length`）；
  新增 `budget_stop_reasons_map_to_typed_stop_reasons` 锁定四个 cap 的映射与
  `raw_stop_reason` 文本。
- 命令与结果：`cargo test -p kiana-agent-core --test loop_guard_budget` →
  11 passed。

## 全量验证

| 命令 | 结果 |
|---|---|
| `timeout 900 cargo check --workspace` | 通过（Finished dev profile） |
| `timeout 900 cargo test -p kiana-session` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-agent-core` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-ai` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-hooks` | 54 passed |
| `timeout 900 cargo test -p kiana-rpc-print` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-interactive` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-coding-agent` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-durable` | 全部 suite 通过（EXIT 0） |
| `timeout 900 cargo test -p kiana-evals` | 全部 suite 通过（EXIT 0） |

## 未修项与说明

- 无未修的 finding。
- 已知边界（不是缺陷）：
  - turn cap 没有 assistant 消息可携带 `stop_reason`（见第 6 条）；
  - 手动压缩使用默认 2.5s settle，`/compact` 会多等这一段时间；测试与交互命令可用
    `compact_with_condensation_options` 覆盖；
  - HTTP 白名单 `[]` 用“不可匹配空模式”表达 deny-all，因为
    `HttpHookPolicy.allowed_url_patterns` 的既有公开契约没有“已配置”布尔；不改变
    现有 API。
