# week-11b handoff

本文件给 wire agent 的接线清单。week11b（后台任务、验收、会话恢复、执行所有权、
checkpoint、agent-write ledger）没有修改根 `Cargo.toml` / `Cargo.lock`，也没有新增
依赖；不需要 wire agent 补 workspace 成员。以下接线都发生在
`crates/kiana-session/**` 与 `crates/kiana-session-core/**` 内部，wire agent 只需在
`AgentSession` 启动/tool dispatch 路径调用新 API。

## 1. 会话恢复（kiana-session-core）

**位置**：`AgentSession` 打开已有会话后、第一次 `prompt` 之前（`agent_session.rs` 的
会话初始化/请求上下文构建路径）。

**API**：

```rust
use kiana_session_core::recovery::{
    plan_manager_recovery, repair_request_view, RecoveryContinuation,
};

// 1) 只读分类（必须先于任何修复调用）。
let plan = plan_manager_recovery(&manager); // manager: &SessionManager
let class = plan.classification.class;

// 2) InterruptedTurn：为每个悬挂 tool_call 追加 failed tool result + turn 标记。
//    幂等；不会重新执行任何工具。
let applied = plan.apply(&mut manager)?;

// 3) InterruptedPrompt：重放尾部 user 一次，不要 append 新 user entry。
if let Some(RecoveryContinuation::ReplayUser { entry_id, message }) = plan.continuation() {
    // 用 entry_id 做去重键：同一个 id 只发送一次；message 原样进入下一轮请求。
}

// 4) DegradedHistory / 任何孤儿结果：只修请求视图，不改 canonical transcript。
let projection = manager.build_session_projection();
let repaired = repair_request_view(&projection.messages, &plan);
let request_messages = repaired.messages; // 交给 convert_to_llm / provider
```

**顺序要求**：

1. `plan_manager_recovery` 必须在 `plan.apply` 之前调用（分类先于修复）；
2. `apply` 只对 `InterruptedTurn` 写 canonical；`InterruptedPrompt` / `DegradedHistory`
   不得写 canonical；
3. `repair_request_view` 的结果只用于本次请求，绝不能回写 `SessionManager`；
4. replay 的 user 用 `entry_id` 去重，防止 resume 重试时重复发送。

**注意事项**：`plan_manager_recovery` 使用 `SessionManager::get_branch(None)` 的当前
分支顺序；如果恢复入口拿到的是其它分支，请改为 `plan_recovery(&branch_entries)`。

## 2. 后台任务（kiana-session `background`）

### 2.1 创建与启动

**位置**：工具执行层解析到 `background=true` 的分支（`AgentSession` 的工具 dispatch
或 `kiana-tools` 的 bash 工具适配层）。**不要**把任务 future 绑定到父 turn future 上。

```rust
use std::sync::Arc;
use kiana_session::background::{
    BackgroundTaskManager, BackgroundTaskOptions, BackgroundTaskSpec, MemoryTaskJournal,
    RecordingNotifier,
};

// 每个 session 一个 Arc<BackgroundTaskManager>，保存在 AgentSession 上（不是 turn 上）。
let manager = BackgroundTaskManager::new(
    BackgroundTaskOptions::new(output_dir, journal, notifier), // journal/notifier 见 2.2
);

// 立即返回 task id；work 在 manager 拥有的 tokio task 中继续运行。
let handle = manager.start(
    BackgroundTaskSpec::new(parent_session_id, parent_turn_id, tool_name)
        .with_description(description),
    move |context| async move {
        context.output.append("...")?;         // 增量输出，硬上限
        if context.is_cancelled() { /* 收尾 */ }
        TaskOutcome::completed(final_text).with_usage(TaskUsage { .. })
    },
).await?;
// 把 handle.task_id / handle.output_path 返回给模型；父 turn 结束不要 cancel。
```

**硬约束**：

- `start` 返回后父 turn 即可结束；manager 与 task 由 session 持有，turn 只保存
  `task_id` 供后续轮询。
- 任务内部所有输出必须走 `context.output.append`（不要直接 `std::fs::write`），
  这样才受硬上限与 `O_NOFOLLOW` 保护。
- 取消入口（用户 stop / TaskStop 工具）调用 `manager.cancel(task_id).await`；
  迟到的完成会被 `finish` 的 CAS 抑制。

### 2.2 kiana-durable 适配（必须实现，不能另建队列）

`BackgroundTaskJournal` / `BackgroundTaskNotifier` 是**注入接口**；生产实现应落在
`kiana-durable` 的 inbox/scheduler 上：

- `BackgroundTaskJournal::save`：在 `TaskScheduler` 里创建一个
  `TaskRecord { background: true, input: <record json>, ... }`（`kiana-durable`
  的 `TaskRecord` 已有 `background`、`restarts`、`state` 字段，正好对应
  `BackgroundTaskRecord`）。
- `finish`：在一个 `Tx` 里把任务状态改成 `TaskState::Terminal`；只有第一次
  Running -> Terminal 的提交返回 `Some(record)`。`TaskRecord.restarts` 对应
  `attempts`。
- `record_attempt`：递增 `restarts` 并提交。
- `list_recoverable`：查询运行中任务 + 终态但 memo 里 `notified != true` 的任务。
- `mark_notified`：在任务 memo 写 `notified: true`（同一事务）。
- `BackgroundTaskNotifier::notify`：把 `BackgroundTaskNotification` 序列化后
  `kiana_durable::harness::inbox::append_input(...)` 写入父会话 inbox；消费方用
  `(task_id, terminal_seq)` 去重。
- 启动时调用一次 `manager.sweep().await?`；`BackgroundTaskResumer` 可选：若
  durable 任务可以重新执行命令，返回 `Some(TaskOutcome)`；否则 `None` 会 terminalize
  为 `interrupted/process_lost`。

`MemoryTaskJournal`/`RecordingNotifier` 只是测试与嵌入参考，**不要**作为生产队列。

### 2.3 通知与 usage

`BackgroundTaskNotification` 携带 `task_id`、`terminal_seq`、父 session/turn、
`usage`；`manager.usage_for_parent(session_id)` / `usage_for_task(task_id)` 分别给出
父与 task 的累计 token。把 terminal 通知写回父会话时保留这两个值。

## 3. 执行所有权（kiana-session `execution`）

**位置**：session 启动与 run 生命周期（`AgentSession::prompt` 外层、dispose）。

```rust
use kiana_session::execution::{
    ExecutionController, ExecutionStatus, FileExecutionStore, RecoverResult,
};

let store = Arc::new(FileExecutionStore::new(state_dir.join("executions")));
let controller = ExecutionController::new(store); // 默认 max_attempts = 10

// 启动：
match controller.recover(&session_id, owner)? {
    RecoverResult::Acquired(lease) | RecoverResult::Resumed(lease) => {
        // 持有 lease；正常/失败结束：
        lease.finish(ExecutionStatus::Completed, None)?;
        // 进程正常关闭（不是终态）：
        // lease.clean_shutdown()?;
    }
    RecoverResult::Terminalized { reason, .. } => { /* 标记会话中断并展示原因 */ }
    RecoverResult::Busy(claim) => { /* 另一个进程正在执行，拒绝启动 */ }
}
```

- `lease` 未调用 `finish`/`clean_shutdown` 就 drop = 崩溃，claim 保留；下次
  `recover` 会 durably +1 attempt，超过 10 次 terminalize（reason
  `execution_attempts_exhausted`）。
- `clean_shutdown` 故意保留 claim 且不消耗 attempt。
- terminal 事件只发布一次；`FinishOutcome.terminal_event` 为 `None` 表示输掉竞争。
- 若要改为 kiana-durable 事务存储，实现 `ExecutionStore` trait 即可，接口
  `begin` 的语义是“claim + Started 同事务写入”。

## 4. Checkpoint 与 agent-write ledger（kiana-session `checkpoint`）

**位置**：工具 dispatch 前/后（所有 mutating tool）。

```rust
use kiana_session::checkpoint::{
    CheckpointManager, CheckpointOptions, ensure_checkpoint_before_tool,
};

let manager = CheckpointManager::new(state_dir.join("checkpoints"), CheckpointOptions::default());

// 每个 mutating tool 前（每 turn/目录最多一次；只读工具自动跳过）：
ensure_checkpoint_before_tool(&manager, cwd, turn_id, tool_name)?;

// 工具成功写入后（每个文件一次）：
let mut ledger = manager.ledger(cwd)?;
ledger.record_agent_write_file(written_path)?;
ledger.save()?;
```

- 恢复入口：`manager.restore(cwd, None, &ledger)?`；报告里的
  `skipped_user_edits` 必须展示给用户，`transaction` 保留用于失败/用户反悔时
  `report.rollback()`。
- 启动时调用 `manager.prune(&RetentionPolicy::default())`；它不会删除活跃项目。
- `ensure_checkpoint_before_tool` 只按 `MUTATING_TOOL_NAMES` 判断；如果新增了
  mutating 工具，请把工具名加进该常量（或直接调用 `ensure_checkpoint`）。
- 不要在用户自己的 repo 上跑 `git add/commit`；所有操作都在私有 git dir 内。

## 5. 验收（kiana-session `acceptance`）

**位置**：每次工具执行完成后采集；子代理/任务结束时判定 acceptance criteria。

```rust
use kiana_session::acceptance::{
    AcceptanceContext, ExecutionRecord, evaluate_acceptance,
};

// 运行时打戳（必须用完整输出，不要用 UI 截断后的 tail）：
let record = ExecutionRecord::record(tool_name, arguments, full_output, receipt_status, seq);

let report = evaluate_acceptance(
    &criteria,
    &AcceptanceContext::new(cwd, &records),
);
// report.status == Passed 才可以当作通过；Unverified 绝不能静默通过。
```

- `ExecutionRecord::record` 会计算 `args_sha256` / `output_sha256`；持久化后再次判定时
  用 `verify()` 自动复算，被改写的凭证不会被当作证据。
- `tests_passed:<command>` 只认 shell 工具（`bash`/`shell`/`run_command`/
  `exec_command`/`unified_exec`/`terminal`）的匹配执行；命令按 `&&`/`;`/`||` 分段
  整段相等，`echo "..."` 不能锚定。
- 未知 criterion 与任何不可判定条件都是 `Unverified`。

## 6. 共享文件

无。根 `Cargo.toml`、`Cargo.lock`、`crates/kiana-session/src/agent_session.rs` 均未
修改。`crates/kiana-session/src/lib.rs` 只新增了四个 `pub mod`（acceptance /
background / checkpoint / execution）；`kiana-session-core` 的 `lib.rs`/`index.rs`
新增 `recovery` 再导出。

## 7. 已知缺口（wire 阶段可决定是否补）

- kiana-durable 的 journal/notifier/execution 适配尚未实现（本 lane 不能新增
  session -> durable 依赖，否则会改 `Cargo.lock`）。
- `AgentSession` 内部没有自动调用上述入口；本文件给出的是接线点。
- 后台任务不接管已崩溃进程的 fd/pid：sweep 只能通过 `BackgroundTaskResumer`
  重新执行命令，否则 terminalize。
- checkpoint 恢复只支持整目录快照（不支持按文件路径选择）；符号链接恢复跳过。
