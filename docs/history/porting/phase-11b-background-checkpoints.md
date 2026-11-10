# 第 11 周后半：后台任务、恢复与 checkpoint

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 470-475 行中本 lane 负责的第 6-11 项：
后台任务、验收、会话恢复、执行所有权、checkpoint、agent-write ledger。
第 1-5 项（子代理运行时）已在 week11a 完成，本文件不重复。

## 参考设计对照

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
|---|---|---|
| `reference_project/ClaudeCode/src/utils/task/diskOutput.ts` | 任务输出文件 `O_NOFOLLOW`；写入队列与硬上限；超过上限后丢弃后续 chunk 并标 truncated | `crates/kiana-session/src/background/output.rs` 的 `BackgroundTaskOutput::open`/`append`；`O_NOFOLLOW` 用 `OpenOptionsExt::custom_flags` |
| `reference_project/ClaudeCode/src/tasks/LocalShellTask/LocalShellTask.tsx` | `notified` 原子 check-and-set；后台任务不随父 turn 结束；通知携带 task id / output path / status | `crates/kiana-session/src/background/mod.rs` 的 `start`/`finish_task`/`notify_record`；`background/journal.rs` 的 `finish` CAS |
| `reference_project/ClaudeCode/src/tasks/stopTask.ts` | cancel 先占位 notified，迟到完成不再发通知 | `background/mod.rs` 的 `cancel`；`journal.rs` 的 `terminal_seq`/`notified` |
| `reference_project/codex/codex-rs/core/src/tasks/mod.rs` | 后台 terminal 生命周期、终态只发布一次、list/terminate 语义 | `background/mod.rs` 的 `finish_task`/`cancel`/`sweep`；`execution.rs` 的 `finish_state` |
| `reference_project/qwen-code/packages/cli/src/serve/prompt-terminal-ledger.ts` | terminal ledger 按 `(task_id, terminal_seq)` 去重 | `background/journal.rs` 的 `terminal_seq`/`notified`；`BackgroundTaskNotification` |
| `reference_project/deer-flow/backend/packages/harness/deerflow/subagents/acceptance_checks.py` | `file:`/`file_written:`/`tests_passed:` leaf 家族；`checked`/`holds` 分层；通过/失败 test-summary 形状；不可判定为 UNVERIFIED | `crates/kiana-session/src/acceptance.rs` 的 `evaluate_acceptance`/`evaluate_leaf`/`evaluate_tests_passed`/`contains_pass_shape`/`contains_fail_shape` |
| `reference_project/deer-flow/backend/packages/harness/deerflow/agents/middlewares/tool_receipt.py` | runtime-stamped `args_sha256`/`output_sha256`/status/seq；凭证从消息流派生 | `acceptance.rs` 的 `ExecutionRecord::record`/`verify`；复用 week11a 的 `subagent::result::ToolReceipt` 与 sha256 |
| `reference_project/deer-flow/backend/packages/harness/deerflow/runtime/goal_progress.py` | 凭证新鲜度/哈希复算，改写过的记录不能作为证据 | `acceptance.rs` 的 `ExecutionRecord::verify` |
| `reference_project/qwen-code/packages/core/src/core/turn-interruption.ts` | 尾部 user -> `interrupted_prompt`；尾部 model 的未应答 functionCall -> `interrupted_turn`；合成长度为限的 failed tool result | `crates/kiana-session-core/src/recovery.rs` 的 `classify_recovery`/`synthetic_failed_tool_result` |
| `reference_project/qwen-code/packages/core/src/core/session-recovery.ts` | 分类先于修复；plan 的 kind/repairs/continuation 分离；`degraded_history` 只修请求视图 | `recovery.rs` 的 `plan_recovery`/`RecoveryPlan::continuation`/`plan.apply`/`repair_request_view` |
| `reference_project/ClaudeCode/src/utils/conversationRecovery.ts` | 尾部 user 重放；interrupted turn 检测；不重复 user entry | `recovery.rs` 的 `InterruptedPrompt`/`RecoveryContinuation::ReplayUser` |
| `reference_project/qwen-code/docs/design/cancelled-turn-recovery.md` | 重试只记录必要的执行尝试；显式取消阻止恢复 | `execution.rs` 的 `ExecutionClaim.attempts` 与 `sweep_state` |
| `reference_project/opencode/packages/core/src/session/execution/restart.ts` | 每 turn 的执行 claim 在 turn 开始时写盘；崩溃/graceful restart 都留同一 durable 签名；attempt 预算默认 10，terminal 事件才重置/释放 claim | `execution.rs` 的 `ExecutionClaim`、`ExecutionController::recover`、`sweep_state`（默认 `DEFAULT_MAX_EXECUTION_ATTEMPTS = 10`） |
| `reference_project/oh-my-pi/packages/coding-agent/src/modes/session-teardown.ts` | clean teardown 显式收尾后台 job，而不是把“进程退出”都当成崩溃 | `background/mod.rs` 的 `cancel`/`finish_task`；`execution.rs` 的 `clean_shutdown` |
| `reference_project/cline/apps/examples/desktop-app/sidecar/session-changes.ts` | 每个 turn 首次模型调用前记录 git checkpoint，用 tree diff 计算工作区变化 | `checkpoint/mod.rs` 的 `ensure_checkpoint`（turn 去重）与 `restore` 的 `git diff --name-only` |
| `reference_project/hermes-agent/tools/checkpoint_maintenance.py` | 持久化计数；用父目录 `(st_dev, st_ino)` 区分“项目删除”和“卷未挂载”；空父目录保持保守 | `execution.rs` 的 `FileExecutionStore`；`checkpoint/mod.rs` 的 `parent_identity`/`project_orphaned`/`prune` |
| `reference_project/gemini-cli/packages/core/src/services/gitService.ts` | shadow git repo；隔离 `GIT_CONFIG_GLOBAL`/`SYSTEM`；固定作者；关闭 gpg/hooks；安全环境 | `checkpoint/mod.rs` 的 `GitRunner::command` |
| `reference_project/gemini-cli/packages/core/src/utils/checkpointUtils.ts` | 每次工具调用前的 checkpoint 元数据（commit + tool call + message id） | `checkpoint/mod.rs` 的 `CheckpointMetadata`、`ensure_checkpoint_before_tool` |
| `reference_project/hermes-agent/tools/checkpoint_manager.py` | per-workdir ref/index/metadata；`_too_broad`；`_MAX_FILES`；ledger；safe restore；nested repo 拒绝；pre-rollback snapshot | `checkpoint/mod.rs`（`ensure_checkpoint`/`restore`/`take`/`scan_tree`）与 `checkpoint/ledger.rs`（`AgentWriteLedger`） |
| `reference_project/deer-flow/backend/docs/checkpoint-retention-contract.md` | “活跃/可恢复的 checkpoint 属于保护集合，retention 不能按时间无条件删除” | `checkpoint/mod.rs` 的 `prune`：workdir 仍存在或无法证明删除时保留 |

## 改了什么

### 1. 后台任务（`crates/kiana-session/src/background/`）

- `output.rs`：`BackgroundTaskOutput`
  - `O_NOFOLLOW`（Linux 常量）+ `O_APPEND` + 0600 打开；符号链接最终组件被拒绝；
  - 字节级硬上限（默认 8 MiB）；超出部分丢弃，按 UTF-8 char 边界切分；
  - `written`/`is_truncated` 可从已有文件恢复。
- `journal.rs`：
  - `BackgroundTaskRecord`、`BackgroundTaskStatus`（running/completed/failed/
    cancelled/timed_out/interrupted）、`BackgroundTaskTerminal`、`TaskUsage`、
    `BackgroundTaskNotification`；
  - `BackgroundTaskJournal` 窄接口：`save` / `load` / `list_recoverable` /
    `finish`（Running→terminal 的原子 CAS）/ `record_attempt` / `mark_notified`；
  - `BackgroundTaskNotifier`、`BackgroundTaskResumer`；
  - `MemoryTaskJournal`、`RecordingNotifier`（测试/嵌入参考，**不是生产队列**）。
- `mod.rs`：`BackgroundTaskManager`
  - `start` 打开输出文件、写 journal、spawn 后立即返回 `BackgroundTaskHandle`；
  - 任务上下文提供取消 token 与增量输出句柄；父 turn 结束不影响任务；
  - `finish_task`：终态 CAS，赢家发一次通知；输家（迟到完成）被抑制；
  - `cancel`：传播 token、abort join、抢占 Cancelled 终态；
  - `sweep`：运行中记录 +1 attempt；有 resumer 则恢复，否则 terminalize
    （`process_lost`）；超过 `max_resume_attempts`（默认 3）terminalize
    `resume_attempts_exhausted`；已终态未通知的记录重发同一
    `(task_id, terminal_seq)`；
  - usage 同时累计到 `usage_for_parent` 与 `usage_for_task`。

### 2. 验收（`crates/kiana-session/src/acceptance.rs`）

- `ExecutionRecord::record(tool, args, output, status, seq)` 运行时计算
  `args_sha256` / `output_sha256`；`verify()` 重新计算，改写过的凭证不被采信。
- `evaluate_acceptance(&criteria, &AcceptanceContext)` 支持：
  - `file:<path> exists|non-empty|json-valid|csv-columns:["..."]`；
  - `file_written:<path>`；
  - `tests_passed:<command>`：只匹配 shell 工具、`status=Succeeded`、receipt 可复算、
    命令按 `&&`/`;`/`||` 分段整段相等；输出必须含测试摘要形状（cargo/pytest/jest/
    go/unittest/TAP 等），失败形状优先；没有匹配执行或没有摘要形状 -> UNVERIFIED；
  - 未知 criterion -> undecidable/UNVERIFIED。
- 文件检查有界读取（50,000 字节），拒绝 `..` 逃逸、符号链接、目录、非 UTF-8。
- 聚合：任意 `checked && !holds` -> Failed；否则任意 `!checked` -> Unverified；
  否则 Passed；空 criteria -> Unverified。

### 3. 会话恢复（`crates/kiana-session-core/src/recovery.rs`）

- `RecoveryClass`：`Clean` / `InterruptedPrompt` / `InterruptedTurn` /
  `DegradedHistory`；`classify_recovery` 只读，先于任何修复。
- 分类：尾部 user -> InterruptedPrompt；最后 assistant 的未应答调用且其后无 user
  -> InterruptedTurn；孤儿/重复 tool_result 或非尾部悬挂调用 -> DegradedHistory。
- `plan_recovery`：分类 + `canonical_repairs` + `request_view_repairs`。
- `RecoveryPlan::apply`：
  - InterruptedTurn 为每个悬挂调用追加 failed tool result（`tool_call_id` 复用原 id、
    错误文本 512 字符上限），再追加 `kiana.recovery.interrupted_turn` custom 标记
    （不进入 LLM 上下文）；幂等，不重试任何工具；
  - InterruptedPrompt / DegradedHistory 不改 canonical。
- `repair_request_view`：移除孤儿/重复 tool_result、给悬挂调用补合成结果；
  只返回新的消息列表，不写回 `SessionManager`。
- `RecoveryPlan::continuation`：`ReplayUser { entry_id, message }`（调用方按 entry id
  去重）或 `SyntheticToolResults { messages }`。
- 会话层便捷入口：`plan_manager_recovery` / `classify_manager_recovery`。

### 4. 执行所有权（`crates/kiana-session/src/execution.rs`）

- `ExecutionClaim`（session/execution/owner/pid/attempts/clean_shutdown）、
  `ExecutionEvent::{Started,Resumed,Terminal}`、`ExecutionState`。
- `ExecutionStore` trait：`begin`（claim + Started 同一事务）、`finish`（释放 claim +
  terminal 只一次）、`keep_on_clean_shutdown`、`load`、`sweep`、`adopt`。
- `ExecutionController`：`acquire` / `recover` / `startup_sweep`；默认
  `max_attempts = 10`。
- `ExecutionLease`：`finish` 释放，`clean_shutdown` 保留且不消耗 attempt；不调用直接
  drop = 崩溃，claim 留给 sweep。
- `MemoryExecutionStore`（参考）与 `FileExecutionStore`：
  - 每 session 一个 JSON 状态文件，读-改-写在 `flock(LOCK_EX)` 下完成；
  - 跨进程 acquire 只有一个赢家；attempts 重开进程后仍保留；
  - terminal 事件每个 execution 恰好一次。

### 5. 私有 git checkpoint（`crates/kiana-session/src/checkpoint/`）

- `CheckpointManager`：
  - `<state>/projects/<workdir_key>/repo.git` 私有 git dir；每 workdir 独立
    `refs/kiana/checkpoints/<key>`、index、metadata；
  - 所有 git 命令显式 `--git-dir`/`--work-tree`/`GIT_INDEX_FILE`，隔离
    `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`，固定作者，关闭 gpg/hooks/gc/fsmonitor；
  - `ensure_checkpoint(workdir, turn_id)`：每 turn/目录最多一次；跳过 root/home
    （含 home 祖先）、`node_modules`/`target`/`.git`/`.kiana`、文件数 > 20,000、
    总大小 > 256 MiB；
  - `git add -A` -> `write-tree`；tree == 上次 tree 时只 touch metadata，不提交；
    否则 `commit-tree` + `update-ref`；
  - metadata 原子写（temp + rename），含父目录 `(dev, ino)`。
- `is_mutating_tool` / `ensure_checkpoint_before_tool`：write/edit/bash/powershell 等
  mutating 工具前的统一入口。
- `restore`：
  - 先 stage 当前树并与目标 commit diff；拒绝含 gitlink 的 checkpoint 与当前目录里的
    嵌套 repo；
  - 只还原 ledger 里记录且当前内容仍等于 agent 最后写入的文件；其余
    `skipped_user_edits` / `skipped_no_ledger` / `skipped_unsupported`；
  - 恢复前做 `restore-transaction` checkpoint + 内存备份；
    `RestoreReport::rollback()` 可写回恢复前内容。
- `AgentWriteLedger`（`ledger.rs`）：
  - `record_agent_write(path, sha256)` / `record_agent_write_file(path)`；
  - 路径规范化为 workdir 相对；哈希 64 位 hex 校验；损坏 ledger 当作空；
  - 保存时按保留期（默认 30 天）与条目数（默认 10,000）裁剪，原子写。
- `prune(&RetentionPolicy)`：
  - workdir 仍存在的项目永不删除；
  - workdir 不存在时只有父目录仍在、`(dev, ino)` 与记录一致、父目录非空才判定为
    删除；否则 `kept_unreachable`（卷未挂载）；
  - 总量超过 `max_total_bytes` 时只删除可证明已删除的项目；
  - 保留项目的 ledger 也按保留期裁剪。

## 做完的标准对照

| DoD | 结果 | 测试 |
|---|---|---|
| 后台任务父 turn 结束后继续，terminal 通知只一次 | 通过 | `background.rs::start_returns_immediately_and_task_survives_parent_turn`；`completion_race_has_one_winner_and_one_notification` |
| cancel 传播并抑制迟到通知 | 通过 | `cancel_propagates_and_suppresses_late_completion_notification` |
| 输出文件硬上限 + O_NOFOLLOW | 通过 | `output_file_is_hard_capped_and_truncation_is_reported`；`output_open_rejects_symlink` |
| token usage 归因到父和 task | 通过 | `start_returns_immediately_and_task_survives_parent_turn`；`sweep_resumes_when_a_resumer_is_available_and_attributes_usage` |
| 崩溃后 durable sweep 恢复或 terminalize，通知不重复 | 通过 | `sweep_terminalizes_lost_tasks_and_does_not_repeat_notifications`；`sweep_resumes_when_a_resumer_is_available_and_attributes_usage`；`sweep_terminalizes_after_resume_attempts_are_exhausted`；`sweep_reemits_terminal_notification_that_was_not_delivered` |
| 没有真实 bash receipt 的 `tests_passed` 判 UNVERIFIED | 通过 | `acceptance.rs::tests_passed_without_any_recorded_execution_is_unverified`；`tampered_receipt_is_not_execution_evidence`；`mentioning_the_command_inside_echo_does_not_anchor_the_criterion`；`build_success_banner_without_test_summary_is_unverified` |
| 文件存在/非空、JSON syntax、CSV header 用代码判定 | 通过 | `file_exists_non_empty_json_and_csv_are_checked_in_code`；`invalid_json_and_unknown_criteria_are_unverified`；`path_outside_workdir_and_symlink_are_unverified` |
| 在 tool_calls 和 tool_result 之间 kill：每个 call 合成失败结果、不重复执行、turn 标记 interrupted | 通过 | `week-11b` recovery 测试：`kill_between_tool_calls_and_results_synthesizes_failed_results_without_reexecution` |
| 在 user prompt 和 assistant 之间 kill：prompt 重放一次且无重复 user entry | 通过 | `kill_between_user_prompt_and_assistant_replays_tail_user_once_without_new_entry` |
| 孤儿 tool result 从 request view 移除但 canonical 保留 | 通过 | `orphan_tool_result_is_removed_from_request_view_but_kept_in_canonical_transcript`；`historical_dangling_call_is_closed_in_request_view_only` |
| 分类先于修复 | 通过 | `classification_and_plan_are_read_only` |
| 两个进程同时 acquire 同一 session 只有一个运行 | 通过 | `execution.rs::only_one_execution_can_hold_a_session`；`file_store_persists_attempts_across_reopen_and_serializes_processes` |
| busy period 同一事务写 claim + Started | 通过 | `busy_period_writes_claim_and_started_together` |
| clean shutdown 不消耗 attempt | 通过 | `clean_shutdown_keeps_the_claim_without_spending_an_attempt` |
| attempt 超限 terminalize 且有原因；terminal 只发布一次 | 通过 | `crash_sweeps_increment_attempts_and_terminalize_after_the_limit`；`terminal_releases_claim_and_is_published_once` |
| 用户手改文件后 restore 跳过并报告；agent 最后写入的文件被还原 | 通过 | `checkpoint.rs::safe_restore_restores_agent_files_and_skips_user_edits` |
| nested repo 拒绝恢复 | 通过 | `restore_refuses_nested_git_repository` |
| restore 失败可回滚到恢复前（用显式 rollback 验证） | 通过 | `safe_restore_restores_agent_files_and_skips_user_edits` 的 rollback 段 |
| checkpoint 不修改用户 git status/refs/stash | 通过 | `checkpoint_does_not_touch_user_git_state` |
| 无变化不产生 commit | 通过 | `per_turn_dedup_and_unchanged_tree_produces_no_commit` |
| 跳过 root/home、node_modules/target/.git、文件数超限 | 通过 | `root_and_home_are_skipped`；`node_modules_target_and_git_are_not_snapshotted`；`file_count_limit_skips_before_git_runs` |
| retention 不删活跃 checkpoint，区分删除与未挂载 | 通过 | `prune_keeps_active_and_unmounted_but_removes_deleted_projects` |

## 真实测试命令与输出摘要

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在
`/home/ctyun/code/kiana` 下实际执行：

```text
$ cargo test -p kiana-session-core
cargo test: 264 passed, 1 ignored (30 suites, 23.11s)

$ cargo test -p kiana-session-core --test recovery
cargo test: 9 passed (1 suite, 0.00s)

$ cargo test -p kiana-session --test background
cargo test: 9 passed (1 suite, 0.05s)

$ cargo test -p kiana-session --test checkpoint
cargo test: 9 passed (1 suite, 0.09s)

$ cargo test -p kiana-session --test execution
cargo test: 7 passed (1 suite, 0.00s)

$ cargo test -p kiana-session --test acceptance
cargo test: 13 passed (1 suite, 0.05s)

$ cargo fmt -p kiana-session -p kiana-session-core -- --check
fmt-check=0

$ cargo test -p kiana-session
cargo test: 505 passed, 5 ignored (75 suites, 147.52s)
```

## 未完成项 / partial

- **kiana-durable 适配未实现**：`BackgroundTaskJournal` / `BackgroundTaskNotifier` /
  `ExecutionStore` 都是注入接口，生产实现应由 wire agent 落在 kiana-durable 的
  inbox/scheduler/事务上（`docs/history/porting/handoffs/phase-11b.md` 有字段映射与调用
  顺序）。本 lane 没有给 kiana-session 增加 `kiana-durable` 依赖（会改
  `Cargo.lock`，硬约束禁止）。因此“接受 kiana-durable 的 inbox/scheduler”是
  **接口已就绪 + 接线待应用**，不是本 lane 内可运行的端到端路径。
- **AgentSession 自动接线未做**：`plan_manager_recovery`/`ensure_checkpoint_before_tool`/
  `ExecutionController::recover`/`manager.start`/`evaluate_acceptance` 都需要在
  `agent_session.rs`/tool dispatch 中调用；本 lane 只提供 API 与测试，wire agent
  按 handoff 接入。`agent_session.rs` 本身未修改。
- **后台任务不接管崩溃进程的 pid/fd**：sweep 只能通过 `BackgroundTaskResumer`
  重新执行命令；默认 terminalize 为 `interrupted/process_lost`。真正的进程收养
  （pidfd/reopen）未实现。
- **通知是 at-least-once + 幂等 key**：内存路径保证恰好一次；崩溃发生在
  “notifier 已发送、mark_notified 未落盘”之间时，sweep 会用同一
  `(task_id, terminal_seq)` 重发，消费方需要去重。这是有意的取舍（避免丢通知）。
- **checkpoint 恢复是整目录级**：没有实现按单个文件路径选择恢复；符号链接目标与
  目录目标标记为 `skipped_unsupported`；agent 删除文件（而非写入）不会进入
  ledger，因此 `restore` 不会主动恢复“agent 删除的用户文件”。
- **Windows**：`parent_identity` 返回 `None`，孤儿项目不会被自动删除（保守）；
  `flock`/`O_NOFOLLOW` 分支只在 unix 生效。
- **checkpoint 快照上限**：默认 20,000 文件 / 256 MiB；超过即跳过，不使用
  partial snapshot。`max_snapshots` 没有实现为历史 commit 数上限（当前只按
  retention/total bytes 清理整个项目目录）。
- **acceptance 的 shell 工具名是内置白名单**：新增 shell 工具需要同步扩展
  `is_shell_tool`；命令匹配不做 `sudo`/`env`/`timeout` 前缀剥离。
- **InterruptedPrompt 的“一次”依赖调用方**：plan 提供稳定的 `entry_id`，实际
  发送一次由 AgentSession 保证；本模块只保证不新增 user entry。

## 风险

- 后台任务输出上限是 8 MiB 默认值；长任务可能被截断而丢失尾部证据。通知里带
  `output_truncated`，调用方应据此提示。
- `FileExecutionStore` 用 `flock`（advisory）跨进程串行化；不遵守锁的外部进程可以
  并发写状态文件。状态文件损坏时返回 `Corrupt`，不会静默重置 claim。
- checkpoint 依赖外部 `git`；git 版本差异、路径含换行/非法 UTF-8、超大仓库都会
  显式跳过或报错。私有 git dir 的 `git add -A` 仍会读取 workdir 的 `.gitignore`，
  用户忽略规则会影响快照内容。
- checkpoint 的 `git add -A` 对嵌套 repo 生成 gitlink；恢复会整体拒绝该 checkpoint，
  这是安全选择但会让含子模块的项目无法恢复。
- 恢复请求视图只按 tool_call_id 配对；provider 若产生重复 id 或空 id，恢复结果可能
  仍被 provider 拒绝，需要上层再做 id 校验。
- `classify_recovery` 以调用方传入的分支顺序为准；如果传入的不是 active branch，
  尾部判断会失真。便捷入口 `plan_manager_recovery` 已固定使用 `get_branch(None)`。
- acceptance 的 `tests_passed` 只看最新一次匹配执行；如果命令历史里既有失败又有
  后续成功，以成功为准；如果最新一次无法判定（输出被截断），不会回退到更早的成功。

## 全量测试

全量 `cargo test -p kiana-session`：**505 passed, 5 ignored（75 suites，147.52s）**，
退出码 0。相比 week11a 的 459 passed / 70 suites，新增 4 个测试二进制（38 个用例）
及 session-core 的 recovery 7 个用例。

