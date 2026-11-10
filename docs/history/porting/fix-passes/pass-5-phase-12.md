# Deep-fix D — 第 12 周与跨波剩余发现的深修

日期：2026-11-08
工作目录：`/home/ctyun/code/kiana`
构建目录：`CARGO_TARGET_DIR=/data/cargo-target-sandbox`

本波处理 wire-D / week-12 审计列出的 13 条“已实现但未接线 / stub / 测试太浅”的发现。
约束：`reference_project/**` 与 `docs/history/porting/plan.md` 未修改；参考项目只读，行为按 Kiana
的类型与运行时重写，没有复制代码。

## 一、逐条修复位置

| # | 发现 | 修复位置（生产路径） | 回归测试 |
|---|---|---|---|
| 1 | `auto_extract` 从不读取、`ExtractionJob` 从不构造、Phase1/2/Source 只有测试实现 | `crates/kiana-session/src/memory_wiring.rs:123`（`MemoryModelBridge`）、`:224`（`ProviderMemorySelector`）、`:505`（`SessionFileSource`）、`:1025`（`spawn_background_extraction`）；`crates/kiana-session/src/agent_session.rs:7027`（`initialize_memory_runtime`，`AgentSession::new` 于 `:10158` 调用） | `memory_wiring::tests::extraction_runs_from_persisted_sessions_and_is_idempotent`、`extraction_skips_current_and_subagent_sessions`、`concurrent_extractions_claim_exactly_one_lease`、`crashed_extraction_retries_without_duplicates` |
| 2 | `MemorySelector` 从不使用（恒启发式） | `memory_wiring.rs:227` 的 `ProviderMemorySelector` 注入 `SessionMemory`，`recall` 先走 provider、失败才回退并记录 `fallback_reason` | `memory_wiring::tests::recall_uses_the_provider_selector_and_falls_back_on_failure` |
| 3 | user scope 从不构造，`target=user` 报错 | `MemoryWiringOptions.user_scope/user_root`；`SessionMemory::new` 同时加载 project/user，`merged_manifest` 以 project 优先去重；`apply/forget/list` 接受 `MemoryScope::User` | `memory_wiring::tests::user_and_project_scopes_stay_isolated_and_project_wins_dedup`、`user_scope_denied_when_not_configured` |
| 4 | memory audit 是 best-effort + 两次未加锁 `O_APPEND` | `crates/kiana-memory/src/service.rs`：`append_audit` 使用独立 `FileLock`、一次 `write_all` + `sync_data` 并返回 `MemoryResult`；`audit_rejected_request`（`:521`）/ `append_audit`（`:821`）覆盖 summary/rebuild/forget/gate 拒绝；`crates/kiana-memory/src/config.rs` 新增 `audit_lock_path` | `kiana-memory`：`audit_failure_is_not_silently_ignored`、`rejected_maintenance_operations_are_audited`、`forget_store_error_is_audited`、`concurrent_audit_appends_stay_well_formed` |
| 5 | 子代理运行时没有生产入口（只有测试调用 `prepare_subagent`） | 新增 `crates/kiana-session/src/subagent/runner.rs:38/145`（`SubagentRunOptions` / `run_subagent`：真实 `kiana_agent_core::Agent` 循环、工具统一走 `SubagentRuntime::dispatch_tool`、turn/usage/receipts/yield 终态）；`crates/kiana-session/src/agent_session.rs:10059/10084/10110`（`spawn_subagent` / `spawn_named_subagent` / `resume_and_run_subagent`，含 `model: inherit`/具名模型解析） | `tests/subagent_runner.rs`（4 条，见下） |
| 6 | ownership 记录启动时不清理 | `crates/kiana-session/src/subagent/isolation.rs:1115`（`sweep_ownership_records` / `scan_ownership_records` / `quarantine_record`）；`agent_session.rs:7045`（`sweep_subagent_workspaces`，`AgentSession::new` 于 `:10159` 调用） | `subagent::isolation` 单测（sweep 4 条：孤儿记录清理、损坏记录隔离、非状态目录拒绝、有效记录保留） |
| 7 | worktree 清理失败被吞、记录仍被删除 | `isolation.rs:1213`（`cleanup_ownership`）：`git worktree remove`/`remove_dir_all` 的 `Err`（`NotFound` 除外）向上传播；branch 删除后再验证；只有成功才移除 ownership 记录 | `subagent::isolation` 单测（cleanup 失败保留记录、成功删除 branch/目录） |
| 8 | `kiana-durable` 测试用固定 3ms `drained()`，负载下 flaky | `crates/kiana-durable/src/harness/view.rs:891`（`NativeWatch::is_idle`/`WatchCore::is_idle`）、`crates/kiana-durable/src/harness/events.rs:770/778`（`AgentEventStream::is_idle` / `wait_until_idle`）、`types.rs` 的 `WatchHandle::is_idle`（含 `session::Watch` 实现）；`tests/support/chat.rs` 的 `drained(&stream)` / `drained_watch(&watch)` 改成条件等待；`tests/harness_events.rs` 的 4 个终态敏感用例改成 `drained_until(stream, predicate)` | `cargo test -p kiana-durable --test harness_events --test harness_view` 连续 5 轮全绿（见下） |
| 9 | `render_core` 零生产调用、`experimental_enabled` 从不读取 | `crates/kiana-interactive/src/interactive_mode/render_core_host.rs:23/41`：构造 `InteractiveMode` 时**读一次** `KIANA_NEXT_RENDER_CORE`；`PendingUserQueue` 接入提交（`run()` 主循环）、`MessageStart` user 回显消费（`events.rs`）、10ms watchdog（`interactive_mode.rs` 轮询任务）、待发消息区展示（`render.rs::update_pending_messages_display`）、会话切换清理（`session.rs`）。审批路径另外使用 `ApprovalCard`/`ModalArbiter`（见 #10） | `interactive_mode::tests::render_core_flag_is_read_once_at_construction`、`render_core_pending_user_message_is_rendered_and_consumed_by_the_echo`；host 单测 5 条 |
| 10 | `InteractiveUiContext` 没有 `permission_card` 覆写，无 `ApprovalCard`/`ModalArbiter` 渲染 | `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs:451`（`ApprovalQueueComponent`：`ModalArbiter` 队列、`Approval i/N`、FIFO token、Ctrl+Enter `confirm_dangerous`）、`:854`（trait 覆写）、`:1456`（`begin_permission_card`：`ApprovalRequest::build`、单对话框、清空后关闭） | `tests/interactive_mode_permission_card.rs`（5 条：卡片内容/AllowOnce、危险动作 Enter 不批准、并发共享一个对话框、Escape fail-closed、Always scope） |
| 11 | 会话 Ask 硬编码 `dangerous:false` | `crates/kiana-permissions/src/request.rs:220`（`PermissionRequest::is_dangerous`，含 sudo/doas/su、git reset/clean/push、chmod -R、find -delete 等）；`crates/kiana-session/src/agent_session.rs:4068`（`build_permission_card_request` 传入 `request.is_dangerous()`） | `kiana-permissions` 的 `dangerous_commands_are_flagged_for_approval_cards`；`agent_session` 的 `permission_card_request_marks_dangerous_commands` |
| 12 | `output_language` 从不进入 system prompt | `crates/kiana-resources/src/system_prompt.rs:525`（`output_language_system_rule`）；`agent_session.rs` 的 `AgentSessionConfig.output_language` + settings `output_language` / `ui.outputLanguage` / `KIANA_OUTPUT_LANGUAGE`，`rebuild_system_prompt` 插入 `<output_language>` section | `tests/output_language_prompt.rs`（3 条） |
| 13 | 交互式 memory 审批未接线、`approve_write` 死代码、default 模式记忆写入不可用 | `memory_wiring.rs`：`SessionMemory::write_authorization` + `approve_write`；memory 工具 `run_with_ui`（`:1335`）→ `approve_if_needed`（`:1529`）经 `run_on_session_thread` 调用 `ExtensionUiContext::permission_card`，AllowOnce/AllowAlways 后消费精确 intent token，Reject/无 UI fail-closed；`MemoryWriteGate::authorize` 只接受完全相同的 scope/source/intents 且一次性消费 | `memory_wiring::tests::default_mode_ask_requires_an_approval_token_before_writing`（默认模式 Ask → 未审批拒绝 → 审批后成功 → token 消费后再次 Ask） |

## 二、真实测试命令与结果

全部在 `/home/ctyun/code/kiana` 下用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox` 执行。

```text
$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.40s
0 errors, 0 warnings

$ cargo test -p kiana-memory
65 passed (9 suites)

$ cargo test -p kiana-permissions
186 passed, 1 ignored (3 suites)

$ cargo test -p kiana-session --lib
110 passed (1 suite)

$ cargo test -p kiana-session
573 passed, 5 ignored (85 suites, 179.81s)
（全量运行早于新增的 `default_mode_ask_requires_an_approval_token_before_writing`；
随后重新跑了 `--lib`（110 passed）确认新增用例通过。）

$ cargo test -p kiana-session --test subagent_runner \
    --test memory_wiring_integration --test output_language_prompt --test subagent_runtime
subagent_runner: 4 passed
memory_wiring_integration: 2 passed
output_language_prompt: 3 passed
subagent_runtime: 9 passed

$ cargo test -p kiana-interactive --lib
80 passed (1 suite)

$ cargo test -p kiana-interactive
775 passed (47 suites, 82.76s)

$ cargo test -p kiana-interactive --test interactive_mode_permission_card
5 passed (1 suite)

$ cargo test -p kiana-durable --test harness_events --test harness_view
harness_events: 21 passed
harness_view: 14 passed
（连续 5 轮重复，全部通过）

$ cargo test -p kiana-durable
896 passed, 11 ignored (88 suites, 45.82s)

$ cargo fmt -p kiana-interactive -p kiana-session -p kiana-memory \
    -p kiana-permissions -p kiana-durable -- --check
（无输出，退出码 0）

$ cargo test -p kiana-session
（全量 573 passed / 5 ignored，见上；新增 memory 审批用例后 `--lib` 复跑 110 passed）
```

关键回归用例说明：

- `subagent_runner::runner_drives_the_model_loop_and_collects_terminal_usage_receipts`：
  faux provider 两次响应（toolCall → final text），断言 transcript 含工具输出、usage > 0、
  receipts 恰好 1 条、持久化 session 文件存在。删掉 `run_subagent` 的 Agent 循环或
  `dispatch_tool` 包装会直接失败。
- `runner_aborts_and_terminalizes_when_max_turns_is_exceeded`：`maxTurns=1` 时第二轮
  `TurnStart` 触发 abort，终态必须是 `Failed + TurnCapped`，且只有一次工具执行。
- `runner_surfaces_yield_output_and_persists_the_session`：yield 工具的装配输出进入终态
  `result`；去掉 yield 工具接线会得到 `output=None`。
- `interactive_mode_permission_card::dangerous_card_defaults_to_reject_and_enter_cannot_approve`：
  Enter 在 Allow 项上必须被忽略（future 未完成），Ctrl+Enter 才能批准。
- `render_core_pending_user_message_is_rendered_and_consumed_by_the_echo`：删除 `MessageStart`
  的 user 分支消费逻辑后，pending 行会残留。

## 三、未修复 / 仍为 partial

1. **FrameWriter / TranscriptLedger 的写端仍未替换现有渲染器**：`render_core::frame` 与
   `render_core::history` 的完整帧契约需要替换 `TuiMainScreen`/`TuiAltScreen` 的写路径
   （行级 diff、history batch ack、alt-screen 序列）。本波把 `experimental_enabled` 改为
   构造时读取一次，并把 approval/modal/pending-user 三条模型接进生产；帧写端保持
   `render_core` 单测覆盖，未做整机替换。原因：替换会牵动 48 个组件与全部渲染 golden，
   超出本波“深修发现”的范围；`KIANA_NEXT_RENDER_CORE` 仍只影响 pending-user 展示。
2. **子代理交互式审批未接 UI**：`SubagentRuntime::dispatch_tool` 的 `NeedsApproval`
   目前会成为 failed tool result；生产入口默认按 headless 语义运行。要把 Ask 交给父会话
   的 `ApprovalQueueComponent` 需要把 `ExtensionUiContext` 传进 runner，属于后续接线。
3. **risk advisory（sideQuery）**：`ApprovalCard::begin_risk/tick` 已实现并测试，但真实
   模型 side query 通道未接（week-12 handoff §2 也已注明）。
4. **`tests/support/task.rs::drained_for_tests`** 仍是 3ms 固定让出；调用点均在
   `eventually`/`wait_for`/`stop()` 之后，未观察到 flaky。`drained()`（audit 指出的那个
   3ms 助手）已改为条件等待。
5. **子代理 skills 正文/CLAUDE.md**：运行时仍只列出技能名、暴露 `omit_claude_md`，
   正文注入需要 `ResourceLoader` 快照（week-11a 已声明 partial）。
6. **memory 抽取调度**：抽取在 `AgentSession::new` 启动一次后台任务；没有周期性重跑或
   跨进程 lease 续约之外的调度器（`FileExtractionStore` 的 lease/watermark 已保证并发
   安全）。
