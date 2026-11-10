# Deep-fix E — 第 11b/12b 周审计发现的深修

日期：2026-11-08
工作目录：`/home/ctyun/code/kiana`
构建目录：`CARGO_TARGET_DIR=/data/cargo-target-sandbox`

本波处理 wire-E / week-11b + week-12b 审计列出的 1 条 critical 与 12 条 major：
“已实现但未接线 / stub / 测试太浅”。约束：`reference_project/**` 与
`docs/history/porting/plan.md` 未修改；参考项目只读，行为按 Kiana 的类型与运行时重写，没有
复制代码；没有删除测试，也没有新增 `#[ignore]`。

## 一、逐条修复位置

| # | 发现 | 修复位置（生产路径） | 回归测试 |
|---|---|---|---|
| 1 | **critical**：生产后台 journal/notifier（含 resumer）仍是进程内 `MemoryTaskJournal`/`RecordingNotifier` | 新增 `crates/kiana-session/src/background/durable.rs:214/231/462/497/523`（`DurableTaskJournal`：kiana-durable `SessionImpl` 事务文档 + 每会话 JSONL；save/finish(CAS)/record_attempt/mark_notified/notify，通知投递行按 `(task_id, terminal_seq)` 幂等）；`crates/kiana-session/src/continuity.rs:221` 在 `ContinuityRuntime::new` 注入生产 journal+notifier；`crates/kiana-session/src/sdk.rs:832` 会话创建后 `sweep_background_tasks()`；`agent_session.rs:11587` 暴露生产 sweep 入口 | `crates/kiana-session/tests/background_durable.rs:34`（崩溃后新 journal 实例 sweep 恰好通知一次）；`continuity.rs:655`（`ContinuityRuntime` 的 manager 记录在另一实例可见）；`continuity_wiring.rs:753`（会话启动 sweep terminalize 崩溃记录并排入通知） |
| 2 | `ExecutionLease::finish` 未在每次 run 终态调用 | `crates/kiana-session/src/agent_session.rs:8429`（每次 run 前 `begin_run` 重新取 claim）、`:8439-8445`（Completed/Failed/Cancelled → `finish_run`）；`crates/kiana-session/src/continuity.rs:335/355`（per-run `begin_run`/`finish_run`，finish 发布一次 Terminal） | `continuity_wiring.rs:355`（同会话两轮 run 各取/放 claim，durable 记录 `terminalSeq` 每次 +1） |
| 3 | `take_recovery_continuation` 从未在 SDK/CLI 启动路径消费 | `crates/kiana-session/src/agent_session.rs:11462`（`resume_recovery_continuation`：先写 `kiana.recovery-replay` 去重标记，再以原 user entry 启动一次 run）；`crates/kiana-coding-agent/src/app.rs:763`（非 RPC 启动分发前调用并打印告警） | `continuity_wiring.rs:583`（重放一次、canonical 不新增 user entry，第二次调用为 no-op） |
| 4 | DegradedHistory 的 `repair_request_view` 只在会话打开时执行 | `crates/kiana-session/src/agent_session.rs:3637/3652`：`refresh_finalized_context_inner` 每次刷新都重新 `plan_manager_recovery` + `repair_request_view`，不再有“仅首次 apply”的分支 | `continuity_wiring.rs:539`（后续 context refresh 仍能剔除孤儿/重复 tool result） |
| 5 | continuity 只有未文档化的环境变量开关 | `crates/kiana-session/src/continuity.rs:117`（`ContinuityOptions::from_settings`：`continuity: true` 或 `{enabled,dir,execution,checkpoints,background,acceptance}`）；`crates/kiana-session/src/sdk.rs:804-805`（生产 SDK 从 settings 读取并传入 `AgentSessionConfig`）；`agent_session.rs:2338` 保留 `KIANA_CONTINUITY` 作为部署侧回退 | `continuity.rs:778`（settings 形状、`false`/`{enabled:false}`/缺省关闭）；`continuity_wiring.rs:337`（默认关闭时全部 no-op） |
| 6 | `bash background=true` 未调用 `start_background_task` | `crates/kiana-session/src/background/tool.rs:33/59`（会话注册内建 bash 时包装：continuity 开启时把可选 `background` 属性注入工具 schema，其余字段保持 TS 黄金 fixture 原样；`background:true` 剥掉参数后 `manager.start`，返回 task id；无 continuity 返回明确错误）；`agent_session.rs:13331` 接入 `refresh_tool_registry` | `continuity_wiring.rs:425`（工具结果含 task id、父 turn 结束后任务继续、终态通知在下一轮注入且只消费一次） |
| 7 | 子代理/后台完成时不跑验收 | 后台：`background/tool.rs:213`（用 `context.acceptance_criteria` + workdir 判定，空 criteria 也产出显式 `Unverified`）、`background/mod.rs:476/608/690`（report 进 terminal/notification）；子代理：`agent_session.rs:10235/10294`（`spawn_subagent`/`resume_and_run_subagent` 返回前判定 `runtime_options.acceptance_criteria`）；`subagent/runtime.rs:148` 增加 criteria 字段 | `continuity_wiring.rs:425`（后台 terminal 带 acceptance report，空 criteria 为 `Unverified`）；`subagent_runner.rs:244`（`file:... exists` 通过 → `all_hold()`） |
| 8 | `apply_patch` 的写入路径不进 agent-write ledger | `crates/kiana-session/src/continuity.rs:559`（`written_paths_from_args` 解析 `*** Add File:` / `*** Update File:` / `*** Delete File:` / `*** Move to:` 全部头）；`agent_session.rs:4526/4542`（每个工具结果按路径写 ledger） | `continuity.rs:762`（一个 patch 的 5 条路径按序解析）；`continuity_wiring.rs:240` 端到端 ledger/restore |
| 9 | `CheckpointManager::prune` 生产路径从不调用 | `crates/kiana-session/src/continuity.rs:274`（`begin_session` 首先调用 `prune_checkpoints`）；`:383`（默认 `RetentionPolicy`，权限错误只记诊断） | `continuity_wiring.rs:672`（启动 prune 删除孤儿项目、保留活跃项目并产出诊断） |
| 10 | cron scheduler/loop + LocalSet worker 未接入 `run_main` | `crates/kiana-coding-agent/src/app.rs:782`（opt-in：`KIANA_CRON=1` 或 settings `cron`）；`:2447`（`cron_startup_config` 解析 `cron: true` / `{enabled,dir,intervalSecs}`）；`:2481`（构建 scheduler，`spawn_local(run_cron_worker)` + `spawn_local(run_cron_loop)`，shutdown sender 随 run_main 结束 drop） | `app.rs:2712`（settings 对象开启并读取 dir/interval）；`cron/runner.rs:676/724`（loop 立即 tick 可停止、worker 消费 channel 并回复） |
| 11 | cron 存储是第二套文件账本，未使用 kiana-durable | `crates/kiana-coding-agent/src/cron/store.rs:574/576/583`（`FileCronStore` 改为包裹 `kiana_durable::harness::record_store::JsonRecordStore`：flock + 临时文件 rename + version CAS）；`:614` 的 job CAS 与 `claim_delivery` 走 `records.compare_and_swap`/`update` | `cron/store.rs:710`（`FileCronStore` 跨实例持久化 + CAS）；`kiana-durable/src/harness/record_store.rs` 单测 `records_survive_a_new_store_instance`、`compare_and_swap_is_versioned_and_update_is_atomic` |
| 12 | cron 脚本 timeout 只包住 read，静默长跑会卡在 `child.wait()` | `crates/kiana-coding-agent/src/cron/runner.rs:462-470`：`wait_and_read`（排空 stdout/stderr 后 `child.wait()`）整体放进同一个 `timeout`，超时 kill + 再 wait | `cron/runner.rs:751`（脚本关闭 stdout/stderr 后 `sleep 30`，1s 超时必须返回 `timed out` 且不挂起调用方） |
| 13 | WebFetch 跟随重定向时只校验首跳（SSRF） | `crates/kiana-coding-agent/src/web/http.rs:146/168/180/183`（`follow_redirects`：`redirect:manual` 客户端，每一跳先 `validate_url` + `resolve_public_host` 再发送；相对 `Location` 用 `url::Url` 解析；上限 20）；`:205` 使用 `kiana_core::http_dispatcher::redirect_manual_http_client`（`crates/kiana-core/src/http_dispatcher.rs:305` 提升为 pub） | `web/http.rs:302`（302 → 169.254.169.254 时第二跳从未发出）、`:330`（相对重定向正常解析）、`:366`（循环有界） |

## 二、关键实现说明

### 2.1 后台任务的 durable journal

- 存储：每个父会话一个 `kiana_durable::session::SessionImpl`，底层是带 fsync 的 JSONL；
  一个会话级 doc 保存 `records`（task id → `BackgroundTaskRecord`）与 `deliveries`
  （`task_id:terminal_seq` → 通知）。所有写操作都是 `SessionChange` 事务，`finish` 用
  `terminal_seq` 做 CAS（第二次 finish 返回 `None`）。
- 通知：`notify` 先写 durable 投递行，再把通知放进进程内队列；父会话下一轮
  `prepare_run` 通过 `take_background_notifications` 取走并写成
  `kiana.background-task` 自定义条目。重启后 `list_recoverable` 返回“运行中 + 终态但
  `notified=false`”的记录，由 `sweep` 补发。
- 启动：`ContinuityRuntime::new` 直接把 `DurableTaskJournal` 注入
  `BackgroundTaskManager`（不再有生产路径使用 `MemoryTaskJournal`/`RecordingNotifier`）。
  `create_agent_session` 在返回前 `sweep_background_tasks()`；CLI 通过同一 SDK 路径启动。

### 2.2 工具层接线

`kiana-tools` 的 bash schema 保持与 TS 黄金 fixture 逐字节一致；`background` 属性只在
会话注册工具时由 `wrap_bash_background` 注入（`manager.is_some()` 时）。`background`
参数在进入底层 bash 前被剥掉，因此不会污染 `spawn` 环境或参数校验。

### 2.3 cron

- `FileCronStore` 的 `jobs`/`executions` 两个 collection 都由 `JsonRecordStore` 承载；
  `claim_delivery` 用 `update` 在锁内完成读改写，不再有独立的 JSON 读写路径。
- `run_cron_loop` 与 `run_cron_worker` 都是 `!Send`（`AgentSession` 在 LocalSet 上），
  由 `run_main` 在会话所在 LocalSet 上 `spawn_local`；worker 的 `CronTurnHost` 应用
  `model`/`skills`/`context_from`/`workdir`/`script_output` 后调用 `prompt`。

### 2.4 WebFetch

`fetch_urls` 仍先做首跳 SSRF 校验；`ReqwestWebTransport` 改用禁止自动重定向的
reqwest client，`follow_redirects` 在每次真正发送前对下一跳重新 `validate_url` 与 DNS
解析校验，因此 302 到 metadata/private 地址在第二跳发出前即被拒绝。

## 三、真实测试命令与结果

全部在 `/home/ctyun/code/kiana` 下用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox` 实际执行（`cargo fmt` 后复跑）：

```text
$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 21.57s
0 errors, 0 warnings

$ cargo test -p kiana-session
586 passed, 5 ignored（全部 suite，0 failed）
  tests/continuity_wiring.rs: 10 passed
  tests/background_durable.rs: 1 passed
  background::durable::tests: 2 passed

$ cargo test -p kiana-durable
898 passed, 11 ignored（全部 suite，0 failed）
  harness::record_store::tests: 2 passed

$ cargo test -p kiana-coding-agent
431 passed, 5 ignored（全部 suite，0 failed）
  cron::runner::tests: 8 passed（含 silent_long_running_script_is_killed_by_the_timeout）
  web::http::tests: 3 passed（含 redirect_to_metadata_is_rejected_before_the_next_hop_is_sent）
  app::tests::cron_settings_object_enables_startup_with_dir_and_interval: 1 passed

$ cargo test -p kiana-tools
440 passed, 0 ignored（全部 suite，0 failed）
  tool_definitions_golden: 3 passed（bash schema 保持逐字节一致）

$ cargo fmt -p kiana-session -p kiana-coding-agent -p kiana-durable \
    -p kiana-tools -p kiana-core -- --check
（无输出，退出码 0）
```

新增/强化的关键回归用例：

- `continuity_wiring::startup_sweep_terminalizes_durable_records_from_a_crashed_process`：
  先在 durable journal 写入 Running 记录，再用同一 state dir 建会话并
  `sweep_background_tasks()`；断言记录变 `Interrupted`、`notified=true`、通知入队。
  回退到内存 journal 时该记录不可见，测试失败。
- `background_durable::crash_leaves_a_running_record_that_the_next_process_terminalizes_once`：
  “进程 A”drop 后不 finish，“进程 B”的新 journal 实例能读回并恰好通知一次。
- `continuity::background_manager_persists_records_in_the_durable_journal`：
  `ContinuityRuntime` 的 manager 启动的任务在另一个 journal 实例中可见（证明生产注入
  的是 durable 而不是内存实现）。
- `continuity_wiring::bash_background_returns_task_id_and_delivers_terminal_notification`：
  模型发起的 `background:true` 返回 task id、父 turn 结束后任务继续、终态 acceptance
  报告为显式 `Unverified`、下一轮通知只消费一次。
- `subagent_runner::agent_session_spawn_named_subagent_runs_a_builtin_definition`：
  传入 `file:subagent-done.txt exists` 后 `outcome.acceptance.all_hold()`，证明子代理
  完成时执行验收。
- `continuity_wiring::session_startup_prunes_orphaned_checkpoints_but_keeps_active_ones`：
  启动时 prune 删除孤儿项目、保留 active 项目。
- `cron::runner::tests::silent_long_running_script_is_killed_by_the_timeout`：脚本关闭
  stdout/stderr 后 `sleep 30`，1s 超时必须返回并可被 5s 外层超时观察到。
- `web::http::tests::redirect_to_metadata_is_rejected_before_the_next_hop_is_sent`：
  302 的目标地址在发出前被 SSRF 校验拒绝。

## 四、未修复 / partial / 与 handoff 的偏差

1. **durable 适配的存储形态**：handoff §2.2 建议落在 kiana-durable 的
   `TaskScheduler`/`harness::inbox` 上；本实现使用 `SessionImpl` 的会话级事务文档 +
   JSONL。原因：`TaskScheduler`/inbox 是 Harness agent-loop 的提交队列（`append_input`
   需要 `SubmissionId` 与 entry 草稿），而 bash 后台任务是独立子进程，不是 Harness
   任务；硬塞进 scheduler 会引入第二套任务定义且无法表达“attach 到已有子进程”。
   当前实现仍满足“唯一 durable 事务底座、没有第二套队列”的约束，通知投递行也持久化。
2. **BackgroundTaskResumer 仍未实现**：进程崩溃后 `sweep` 把 Running 记录 terminalize 为
   `interrupted/process_lost`；没有尝试收养/重跑子进程。handoff §2.2 明确 resumer 可选
   （`None` 即 terminalize），但审计若要求“可恢复”能力，需要 pid/fd 收养，属于后续波。
3. **execution claim 仍在 `FileExecutionStore`**：per-run finish/terminal 已接通，但
   claim 文件不是 kiana-durable 事务存储；handoff §3 允许“实现 `ExecutionStore` trait
   即可”，本波未迁移。
4. **continuity 的 CLI flag**：实现了 settings（`continuity: true` / 对象）与
   `KIANA_CONTINUITY` 环境变量回退，没有新增 `--continuity` 命令行参数；settings 是
   主渠道，避免再复制一套 flag→settings 覆盖逻辑。
5. **SDK 自动重放**：CLI 启动路径在分发前调用 `resume_recovery_continuation()`；直接使用
   `create_agent_session` 的嵌入方需要自己调用同一个公开方法。SDK 创建时自动发模型请求
   会让“打开会话”产生网络副作用，本波按 wire-E 的有意行为保留为宿主决定。
6. **后台通知的跨进程投递**：durable 投递行保证幂等键，但通知进入的是当前进程的队列；
   父会话必须在同一进程后续 run 中消费。跨进程推送/订阅仍由 daemon/EventFeed 负责，
   本波未接。
7. **cron `context_from`**：worker 目前只接受会话文件路径并用 `kiana.cron.context-from`
   自定义条目注入；任意表达式/跨会话引用未实现（审计未要求）。
