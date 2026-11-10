# 第 5/6/9 周 deep-fix C

工作目录：`/home/ctyun/code/kiana`。统一构建：`CARGO_TARGET_DIR=/data/cargo-target-sandbox`。
未修改 `reference_project/**` 与 `docs/history/porting/plan.md`。

## 结论

17 条 major finding 全部处理完毕；生产代码路径均已接线，并新增/更新会在实现退化时失败的测试。
最终验证：

| 命令 | 结果 |
| --- | --- |
| `timeout 900 cargo check --workspace` | 0 errors（19 crates compiled） |
| `timeout 900 cargo test -p kiana-session` | 83 个 suite，556 passed，0 failed |
| `timeout 900 cargo test -p kiana-agent-core -p kiana-tools` | 681 passed，0 failed |
| `timeout 900 cargo test -p kiana-builtin-extensions` | 291 passed，0 failed |
| `timeout 900 cargo test -p kiana-config -p kiana-resources -p kiana-builtin-extensions` | 83 个 suite，1534 passed，0 failed |
| `timeout 900 cargo test -p kiana-compaction` | 全部 passed（含新增 fallback/pruning 用例） |
| `timeout 900 cargo test -p kiana-ai` | 2103 passed，341 ignored，0 failed |

## 逐条 finding

### week-05

1. **`/skill:<name>` 不创建 SkillActivation / 不检查 user-invocable**
   - 修复：`crates/kiana-session/src/agent_session.rs:7582` `expand_skill_command` 在展开正文前调用
     `SkillTool::activate_user`；`user-invocable: false` 记录 `skill_expansion` 错误并原样回显命令。
   - 核心实现：`crates/kiana-resources/src/skills/tool.rs:134`（`InvocationSource::{Model,User}`，
     用户路径不检查 `disable-model-invocation`）、`:222`（命名/位置参数解析）。
   - 测试：`crates/kiana-session/tests/skill_activation_wiring.rs:81`、
     `:228`（拒绝不可用户调用技能）。

2. **技能激活无法在会话中途结束（deactivate/set_ttl 未用，expires_at 恒 None）**
   - 修复：`agent_session.rs:9479` 读取 settings 的技能激活 TTL（生产入口：
     `skills.activationTtlSeconds` / `activationTtl`，由 `migrate_skills` 迁移到顶层
     `skillActivationTtlSeconds` / `skillActivationTtl`，见
     `crates/kiana-config/src/settings/migrations.rs:144` 附近）；`ensure_skill_tool` 调用
     `set_ttl`。新增公开 API `deactivate_skill`、`set_skill_activation_ttl`、
     `active_skill_activations`（`agent_session.rs:10755` 起）。
   - 测试：`skill_activation_wiring.rs:132`（撤销后回到 Ask）、`:177`（TTL 到期后 prune）。
   - 兼容性说明：`SkillMetadata` 的 `Default` 改为 `user_invocable = true`
     （`crates/kiana-resources/src/skills/definition.rs:113`），与文档“缺省 true”一致；
     否则加载失败时的兜底定义会误判为不可用户调用。

3. **`paths` 可见性使用进程 cwd 而非会话 cwd**
   - 修复：`definition.rs:310`/`:330` 新增 `skill_visible_for_model_in` / `paths_match_in`；
     `prompt.rs:158` 新增 `format_skill_definitions_for_prompt_with_cwd`；会话在
     `agent_session.rs:9553` 传入 `Some(&self.cwd)`。
   - 同时修掉回退路径：有技能定义但本轮全部不可见时，系统提示不能回退到忽略 `paths` 的旧
     `format_skills_for_prompt`（`agent_session.rs` 的 `rebuild_system_prompt` 与
     `prepare_prompt_and_tool_loadout`）。
   - 测试：`crates/kiana-session/tests/skill_paths_plan_wiring.rs` 的
     `paths_visibility_uses_the_session_cwd`（触达前隐藏、`read src/a.txt` 后下一轮系统提示可见）。

4. **OAuth issuer 隔离 API 未接运行时**
   - 修复：`crates/kiana-builtin-extensions/src/mcp/oauth.rs:832` 新增
     `for_server_with_stored_issuer`，`:841` 新增 `invalidate_stored_issuer_mismatches`；
     `:973` 的 `save` 在写入 issuer 键时删除同族其它 issuer 的旧凭据
     （`:402` 的共享 helper）。运行时调用点：`mcp/cli.rs:1560`、`:1582` 与
     `mcp/index.rs:2198`、`:2212`。
     （按行为重写；未复制参考项目代码。）
   - 测试：`crates/kiana-builtin-extensions/tests/mcp_oauth_store.rs:397`
     （新 issuer 保存后旧 issuer 凭据被删除）、`:427`（按存储 issuer 打开作用域并清理）。
   - 未做：没有启动两个真实 OAuth 授权服务器做端到端“换 issuer 后重新登录”测试；协议层
     discovery/issuer 校验已有既有测试覆盖。

5. **缺少会话级 MCP annotations 验收测试**
   - 生产实现位于 `agent_session.rs` 权限链（`apply_mcp_annotations`、
     `allows_persistent_approval`）。
   - 新增 `crates/kiana-session/tests/mcp_annotations_permissions.rs`，从 `session.prompt`
     走完整路径：read-only 模式 `readOnlyHint` 放行、default 模式仍 ask、destructive 覆盖
     Allow 规则强制 ask、显式 deny 优先、destructive 的 `AllowAlways` 不产生持久授权。
     测试支撑：`HarnessOptions.custom_tools`（`tests/common/mod.rs:289`、`:625`）。
   - `timeout 900 cargo test -p kiana-session --test mcp_annotations_permissions`：5 passed。

### week-06

6. **active plan pruning 保护与 plan/plan-mode 恢复未接线**
   - 修复：`agent_session.rs:858` 的 `SessionPlanState`、`:9658` 的
     `plan_state_from_entries`（会话启动从 `plan-mode` custom entry 恢复）、`:9674` 的
     `apply_plan_entry_data`；`appendEntry` 处理器调用 `observe_plan_custom_entry`
     （`:9536`），pruning/压缩分别使用 `:9525` `plan_prune_config` 与 `:9531`
     `plan_recovery`。
   - 测试：`skill_paths_plan_wiring.rs:128`（custom entry 恢复）、`:161`
     （active plan 的 read 结果不被聚合预算清除；无 plan 的对照被清除）。

7. **fallback 追加文件清单后未重新检查大小**
   - 修复：`crates/kiana-compaction/src/fallback.rs` 在追加
     `format_file_operations` 后重新估算，超 `recovery_target_tokens` 再执行
     `enforce_target`，仍不满足（>= 输入）则返回 `None`。
   - 测试：`fallback.rs:505`、`:537`。

8. **`MIN_PRUNE_TOKENS` 只作用于 age 分支**
   - 修复：`crates/kiana-compaction/src/pruning.rs:510-517`，superseded / useless / age
     三个分支都要求 `tokens >= MIN_PRUNE_TOKENS`；保护匹配器命中的结果仍完全不改。
   - 测试：`pruning.rs` 新增 `small_superseded_and_useless_results_are_not_blanked`；
     更新旧用例 `tests/pruning.rs` 的
     `zero_result_grep_and_wait_use_exact_placeholders`：原断言“小 useless 结果应被
     blank”正是 finding 描述的缺陷，现改为断言原样保留（未删除测试）。

9. **默认保护工具缺 `ToolName("skill")`**
   - 修复：`pruning.rs:110`。
   - 测试：`pruning.rs` 的 `skill_tool_results_are_protected_from_age_pruning`；
     `tests/pruning.rs` 的 `skill_and_active_plan_reads_are_protected`。

### week-09

10. **provider 请求管线未调用 prepare_request/能力门/输出预算/cache 断点**
    - 修复：`agent_session.rs:8818` `apply_session_request_policy`，由
      `stream_simple_stream_fn` 在 `compat::stream_simple` 之前调用：
      `CapabilitySet::language_chat().with_vision(model.input...)` 能力门 →
      `precheck_input` → `compute_output_budget` 写回 `options.base.max_tokens`；
      `ExplicitBreakpoints` 时 `:8853` 组合 `onPayload` 在最终 wire payload 注入断点。
      faux provider 跳过（脚本化测试语义）。
    - 测试：`agent_session.rs` 单测 `request_policy_writes_budget_and_cache_breakpoints`
      （预算 4096、调用方 payload 改写后再注入 `cache_control`）、
      `request_policy_rejects_vision_for_a_text_model`；
      `crates/kiana-session/tests/request_policy_wiring.rs:63`/`:87`
      （Unsupported / ContextWindowExceeded 在 HTTP 前返回，计数 client 断言 0 请求）。

11. **SessionTelemetry span 全是 root（start_child 未用）**
    - 修复：`crates/kiana-agent-core/src/session_telemetry.rs` 新增
      `active_root`/`active_turn`/`trace_context` 状态；`:365` `begin_agent_run` 建根，
      `:477` `start_session_span` 经 `:463` `active_parent` 以 turn（优先）或 root 为父；
      turn/api/tool span 因此嵌套。`end_turn`/`end_agent_run` 清空父引用。
    - 测试：`crates/kiana-session/tests/telemetry_wiring.rs:91`
      （root 无父；turn 父为 root；chat/tool 父为 turn）。

12. **flush 只导出 spans，没有 logs/metrics**
    - 修复：`session_telemetry.rs:1007` `flush` 依次 enqueue spans、
      `memory.get_metrics()`、缓冲 logs，再 `exporter.flush()`；`:508` `record_log`，
      `record_api_call` 写结构化日志。
    - 测试：`telemetry_wiring.rs:153`，本地 OTLP collector 断言
      `/v1/traces`、`/v1/logs`、`/v1/metrics` 三条请求都到达，且
      `exported_spans/logs/metrics > 0`。

13. **TRACEPARENT 未传给 bash 子进程**
    - 修复：`crates/kiana-tools/src/bash.rs:239` `TraceContextProvider`、`:270`
      `BashToolOptions.trace_context`；`:1472-1490` 在执行前写入/删除
      `TRACEPARENT`/`TRACESTATE`。会话侧 `agent_session.rs:4519`
      `builtin_tool_options` 从 `SessionTelemetry::current_trace_context` 构造 provider；
      `:10836` 暴露 `current_traceparent`。
    - 测试：`bash.rs:1846`/`:1872`（env 注入、无 provider 时不注入）；
      `telemetry_wiring.rs:208`（真实 bash 读到合法 W3C traceparent，`TRACESTATE` 未泄漏）。

14. **输入预检不保证零请求 / pending 错误类型**
    - 修复：`agent_session.rs:9713` `transcript_for_context` 现同时携带 system + tools；
      `:5418` `ensure_request_fits_context` 在压缩关闭/无可用压缩时返回 typed
      `AiError`（`ContextWindowExceeded` 经 `From` 转换），不再落成 `Other`；
      `pending_stream_error` 保存 typed 错误，wrapper 原样返回。
    - 测试：`precheck_cost_wiring.rs:14`：`provider=mock`（非 faux）超窗 prompt 后
      faux pending response 仍为 1（零 provider 请求），assistant 错误文本来自预检；
      `request_policy_wiring.rs:87` 断言 `name == "ContextWindowExceeded"` 且 detail 存在。

15. **agent 重试未使用 provider status / typed isRetryable；provider_retry 缺 425/529/false**
    - 修复：`crates/kiana-ai/src/utils/provider_retry.rs:131` 构造
      `TypedProviderError`（status + `x-should-retry` → `is_retryable`），走
      `RETRYABLE_STATUS_CODES`（含 425/529）。
      `agent_session.rs:9739` 从错误文本提取 HTTP 状态（`API error (429): …` /
      `"status": 529`），`:9773` `assistant_error_is_retryable` 用 typed 分类 + request_policy
      正向判定，旧正则只作兜底。
    - 测试：`provider_retry.rs` 的 `retries_sdk_statuses` / 新增
      `typed_should_retry_false_overrides_a_retryable_status`；`agent_session.rs` 单测
      `provider_status_participates_in_retry_decisions`（425/529 重试，501/401 与
      上下文溢出不重试）。

16. **run collector 只能表达 Ok/Error**
    - 修复：`crates/kiana-agent-core/src/run_collector.rs:658`
      `observe_event_with_usage` → `:794` `tool_status_from_event`：
      `duration_ms=None && is_error` → Blocked，`None && !is_error` → Skipped，
      “Operation aborted” → Aborted，已执行且错误文本含 timeout → Timeout，其余 Ok/Error；
      `error_type` 由 `:816` 统一给出。
    - 测试：`run_collector.rs:1356`
      `event_driven_tools_distinguish_blocked_skipped_timeout_aborted`（五种终态 + by_name）。

17. **run summary cost 与 SessionTelemetry canonical cost 不一致**
    - 修复：collector 新增 `end_chat_with_usage`（`run_collector.rs:475`）；
      `agent_session.rs:2926` `observe_run_telemetry` 在 `TurnEnd` 用
      `account_provider_usage` 的结果喂 collector，与 `record_api_call` 同源。
    - 测试：`run_collector.rs:1403`（canonical 1.25 覆盖 provider 0）；
      `precheck_cost_wiring.rs:71`（定价模型一次 run 的 summary 金额 == canonical 公式）。

## 跨 crate 接线 / 兼容性

- `AgentSessionConfig` 增加可选 `telemetry: Option<Arc<SessionTelemetry>>`；所有内部
  struct literal（`sdk.rs`、测试 harness/compaction-live）已补 `telemetry: None/options.telemetry`。
- `BashToolOptions`/`PowerShellToolOptions` 新增可选 `trace_context`，`Default` 为 `None`，
  既有调用方字面量通过 `..Default` 无需改动（PowerShell 显式字段已补）。
- `HarnessOptions` 增加 `telemetry` 与 `custom_tools`，仅测试夹具使用。
- settings：`migrate_skills` 将对象形式的技能 TTL 迁移到顶层未知键
  `skillActivationTtlSeconds` / `skillActivationTtl`，由 `Settings.extra` 保留；
  没有新增 schema 顶层字段，`settings_input_mirrors_the_59_top_level_properties` 保持通过。

## 未修 / 限制

- 第 4 条没有双授权服务器的端到端登录测试（仅 store 级 + 运行时调用点覆盖）。
- `SessionPlanState` 的持久化依赖 plan-mode 扩展 `appendEntry("plan-mode", …)` 与宿主
  `set_plan_state`；没有为第三方扩展提供自动写入 plan 内容的订阅钩子。
- 运行 collector 的 `Timeout/Aborted` 依赖工具错误文本（`duration_ms`/结果内容）识别，
  这是事件协议现有字段下可达的最强判定；直接 API `end_tool(status)` 可精确指定。
