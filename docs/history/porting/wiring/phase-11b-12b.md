# Wire E — 第 11b/12b 周共享接线与本波集成验证

日期：2026-11-07
范围：`docs/history/porting/handoffs/phase-11b.md`、`docs/history/porting/handoffs/phase-12b.md`
（以及 `docs/history/porting/phase-11b-background-checkpoints.md` / `phase-12a-tui-frame-contract.md`）的跨 crate 接线与集成验证；
验收标准见 `docs/history/porting/plan.md` 第 11/12 周。

本文件记录实际应用的 handoff、与 handoff 的核对结论与有意偏差、共享文件改动、
真实检查/测试输出、剩余缺口，以及给后续波的风险。

---

## 1. 应用顺序与核对结论

按依赖顺序应用：

1. **week 11b（恢复 / 执行所有权 / checkpoint / ledger / 验收）**：
   - `kiana-session-core::recovery` 已在 lane 完成；本波把它接到 `AgentSession`
     的构造与消息视图重建路径。
   - `kiana-session::execution/checkpoint/background/acceptance` 的库层 API 与
     lane 测试一致；本波新增 `continuity` 运行时把它们挂在会话生命周期上。
2. **week 12b（cron / daemon / IDE / web）**：
   - `kiana-coding-agent::cron` 的 `FileCronStore`/`CronScheduler`/`schedule` 与
     handoff 一致；本波新增 `cron::runner` 生产 dispatcher 与 tick 循环。
   - `kiana-rpc-print::event_feed` 与 handoff 一致；本波新增 `daemon` 真实
     SSE/WS/REST listener（hyper 1 + hyper-util + tokio-tungstenite）。
   - `kiana-coding-agent::ide` 的发现/协议与 handoff 一致；本波新增
     `ide::startup` 启动接线。
   - `kiana-coding-agent::web` 的搜索/抓取与 handoff 一致；本波新增
     `web::tools` 工具定义并把扩展工厂加入启动列表。

工具执行顺序保持 `PreToolUse hook（可改写 args）→ 扩展 tool_call（可改写 args）→
记录触达路径 → 权限判断 → checkpoint → 执行`；任何一层的 deny 都优先，hook/扩展
的 allow 不能跳过权限判断。本波只在权限放行之后插入 checkpoint，没有改变原有
deny 优先级；既有 `hooks_wiring` / `permissions_integration` 用例全部通过。

---

## 2. 共享文件改动

| 文件 | 改动 |
|---|---|
| `crates/kiana-session/src/continuity.rs` | **新增**：`ContinuityOptions`/`ContinuityRuntime`、执行 claim 生命周期、checkpoint/ledger/验收采集、后台 manager 持有；4 个单测。 |
| `crates/kiana-session/src/agent_session.rs` | `AgentSessionConfig::continuity`；Core 的 `continuity`/`initial_recovery`/`pending_recovery`/`execution_refusal`/`run_sequence`；构造期恢复分类 + `InterruptedTurn` canonical 修复；请求视图修复；`before_tool_call` checkpoint；`after_tool_call` ledger/验收；`start_run` 执行门禁；公开 API（recovery、acceptance、checkpoint restore、background）。 |
| `crates/kiana-session/src/lib.rs` | 导出 `continuity` 模块。 |
| `crates/kiana-session/src/sdk.rs` | `AgentSessionConfig::continuity` 填 `None`；创建后调用一次 `sweep_background_tasks()`（默认关闭时 no-op）。 |
| `crates/kiana-session/Cargo.toml` | 新增 `libc = { workspace = true }`（执行 claim 的 pid 存活检查）。 |
| `crates/kiana-session/tests/common/mod.rs` | Harness 增加 `continuity` 选项。 |
| `crates/kiana-session/tests/agent_session_compaction_live.rs` | `AgentSessionConfig::continuity` 填 `None`。 |
| `crates/kiana-session/tests/continuity_wiring.rs` | **新增**：恢复分类/修复、continuation、checkpoint/ledger/验收、默认关闭共 4 个集成用例。 |
| `crates/kiana-coding-agent/src/cron/runner.rs` | **新增**：`SessionJobDispatcher`、`CronTurnRunner`/通道 runner、受管脚本执行（超时/输出上限/排空管道）、`run_cron_loop`、`build_cron_scheduler`；3 个单测。 |
| `crates/kiana-coding-agent/src/cron/mod.rs` | 导出 `runner` 与相关类型。 |
| `crates/kiana-coding-agent/src/ide/startup.rs` | **新增**：`agent_ide_dir`/`connect_for_workspace`/`connect_in`；2 个单测。 |
| `crates/kiana-coding-agent/src/ide/mod.rs` | 导出 startup。 |
| `crates/kiana-coding-agent/src/web/tools.rs` | **新增**：`web_search`/`web_fetch` ToolDefinition 与 `web_tools_extension()`；2 个单测。 |
| `crates/kiana-coding-agent/src/web/mod.rs` | 导出 tools。 |
| `crates/kiana-coding-agent/src/app.rs` | 启动扩展列表追加 `web_tools_extension()`（默认不激活）。 |
| `crates/kiana-rpc-print/src/daemon.rs` | **新增**：hyper SSE/WS/REST daemon、`DaemonConfig`、`bind_daemon`；4 个真实网络单测。 |
| `crates/kiana-rpc-print/src/lib.rs` | 导出 `daemon`。 |
| `crates/kiana-rpc-print/Cargo.toml` | 新增 `hyper`/`hyper-util`/`http-body-util`/`tokio-tungstenite`/`futures`（版本均已在根 workspace 依赖 pin）。 |
| `Cargo.lock` | 由 cargo 自动更新依赖边（未手工编辑）。 |

没有修改 `reference_project/**`、`docs/history/porting/plan.md`、`crates/kiana-session-core/**`、
`crates/kiana-durable/**`、`crates/kiana-tools/**`、`crates/kiana-builtin-extensions/**`。

---

## 3. Week 11b 接线

### 3.1 会话恢复（默认开启，只读分类先行）

- `Core::new` 在持有 `SessionManager` 的第一时间调用
  `plan_manager_recovery(&session_manager)`（只读分类）。
- 只有 `InterruptedTurn` 调用 `plan.apply(&mut session_manager)`：为每个悬挂
  tool_call 追加 failed tool result（复用原 call id、错误限长），并写
  `kiana.recovery.interrupted_turn` 标记；`InterruptedPrompt` / `DegradedHistory`
  不改 canonical。修复失败只进 `config_diagnostics`，不阻断会话启动。
- `AgentSession::new` 在 `build_runtime` 之前调用
  `refresh_finalized_context_with_recovery()`：按当前 manager 重新分类，并用
  `repair_request_view` 生成**本次请求视图**（孤儿/重复 tool result 只从 messages
  移除，不写回 SessionManager）。
- 修复后的 Agent 历史来自 session projection，因此 `InterruptedTurn` 的合成结果
  对模型可见，且不会重新执行任何工具。
- 公开 API：`recovery_classification()`、`recovery_plan()`、
  `take_recovery_continuation()`（一次性交付；`ReplayUser` 的 `entry_id` 供宿主
  去重）。`InterruptedPrompt` 的实际发送仍由宿主决定：本波没有在会话构造时自动发起
  模型调用（见缺口 6）。

### 3.2 执行所有权（continuity 开关，默认关闭）

- 打开 `continuity.execution` 时，`Core::new` 调用
  `ExecutionController::recover(session_id, owner)`：
  - 活 claim（pid 存活且非 `clean_shutdown`）→ `Busy`；
  - 死进程/clean shutdown claim → `Resumed`（clean shutdown 不消耗 attempt）；
  - 尝试超限 → `Terminalized`。
- 失败原因保存在 `Core::execution_refusal`，`start_run` 直接返回
  `Cannot start a run: ...`，防止第二个进程同时驱动同一会话。
- lease 由 `ContinuityRuntime` 持有到会话销毁：`dispose()`/Drop 调用
  `clean_shutdown()`（保留 claim、不消耗 attempt）；崩溃（不调用 dispose）留下的
  claim 由下次 `recover` 的 sweep durably +1 attempt，默认 10 次后 terminalize。

### 3.3 Checkpoint 与 agent-write ledger（continuity 开关）

- `Core::before_tool_call` 在 hook 与权限都放行之后调用
  `ContinuityRuntime::checkpoint_before_tool`：
  - 只对 `MUTATING_TOOL_NAMES`（write/edit/apply_patch/bash/powershell 等）生效；
  - turn 键是 `<run_sequence>:<turn_index>`；`run_sequence` 每个 run 递增，避免
    跨 run 的 turn 0 被错误去重；
  - checkpoint 失败只记诊断，不阻断工具执行。
- `Core::after_tool_call` 在扩展 PostToolUse 之前采集：
  - 成功且参数含 `path`/`file_path`/`filePath` 的文件工具
    （write/edit/multi_edit/apply_patch 等）→ `record_agent_write_file` + `save`；
  - bash 的任意写入无法枚举，不进入 ledger（见缺口 4）。
- 公开 API：`checkpoint_manager()`、`restore_checkpoint()`（返回
  `RestoreReport`，调用方必须展示 `skipped_user_edits` 并保留 `transaction` 以便
  `rollback()`）。

### 3.4 验收（continuity 开关）

- 每次工具完成（包括错误/被拦截的结果）`record_tool_effects` 用**运行时完整结果**
  构造 `ExecutionRecord::record(tool, args, output, status, seq)`；seq 从 1 开始。
- `acceptance_records()` / `evaluate_acceptance(criteria)` 使用会话 cwd 判定；
  未启用 continuity 时返回空记录集合，所有 criteria 都是 `Unverified`，不会静默通过。
- bash 的 `content` 是完整输出文本（工具自身上限之外不做 UI 截断），因此
  `tests_passed:<command>` 能看到测试摘要形状。

### 3.5 后台任务（continuity 开关）

- `ContinuityRuntime` 持有 `Arc<BackgroundTaskManager>`（进程内
  `MemoryTaskJournal` + `RecordingNotifier`），`AgentSession` 暴露
  `start_background_task` / `cancel_background_task` / `sweep_background_tasks` /
  `background_task_manager`。
- SDK 创建会话后调用一次 `sweep_background_tasks()`；崩溃遗留记录在 sweep 中
  terminalize 或重发未送达的 terminal 通知。
- 由于 `kiana-tools` 不在本波所有权内，模型发起的 `bash background=true` 还没有
  转成 `manager.start`（见缺口 3）；本波交付的是会话级 API 与生命周期接线。

---

## 4. Week 12b 接线

### 4.1 cron

- `cron::runner::SessionJobDispatcher` 实现 `JobDispatcher`：
  1. `job.validate(managed_script_dir)`；
  2. `workdir` 必须是存在的绝对目录；
  3. `script` 经 `resolve_script_path` 解析为受管目录内路径，用 `sh` 在 workdir
     执行；输出持续排空（超过上限只丢弃，避免 pipe 写满死锁），超时 kill，
     非零退出把 execution 记为 `failed`；
  4. 通过 `CronTurnRunner` 交付 `CronTurnRequest{prompt,skills,model,context_from,
     workdir,script_output,execution_id,slot}`。
- `ChannelTurnRunner` + `cron_turn_channel()` 提供 `Send + Sync` 的
  dispatcher→LocalSet 宿主桥（`AgentSession` 是 `!Send`，不能直接塞进
  `JobDispatcher`）。
- `run_cron_loop(scheduler, 30s, watch_shutdown, on_report)`：启动立即 tick 一次，
  之后每 30s；`on_report` 收到 `TickReport`/错误，由宿主按 `Output` 纪律写日志。
- `build_cron_scheduler(<agent_dir>/cron, dispatcher)`：创建 `scripts/` 0700、
  `FileCronStore`、`ProcessFingerprint::current()` 与 `managed_script_dir` 配置。

### 4.2 daemon

`kiana-rpc-print::daemon` 是 `EventFeed` 的真实传输层：

- `GET /events` → SSE（`id/event/data` 单帧，内容为 `FeedEvent::to_sse_frame`），
  轮询间隔默认 50ms，15s 心跳；
- `GET /events` + `Upgrade: websocket` → 101 + `Sec-WebSocket-Accept`
  （`tungstenite::handshake::derive_accept_key`），升级后每帧一条预编码 JSON；
  连接使用 `.with_upgrades()` 的 hyper http1 连接；
- `GET /events/latest?limit=N` → `{"events":[...],"since":...,"has_more":...}`，
  `since` 可直接交给 WS `?since=`；
- `GET /healthz` → `ok`；
- 每个连接一个 `Subscription`；慢客户端队列满时 `EventFeed` 只移除该订阅，
  SSE/WS 连接在 `is_failed()` 后自行关闭；
- 默认 `DaemonConfig::bind_addr = 127.0.0.1:0`；daemon 没有内建鉴权，跨主机暴露
  必须由调用方加认证/反代；
- 辅助函数 `channel_for_event_type`（`.delta`/`.update` → volatile，其余 durable）
  与 `publish_json` 供 app 事件桥使用。

**与 handoff 的偏差**：`SubscribeOutcome::gap` 没有进入 SSE 帧格式（协议里没有该
字段）；需要 gap 语义的调用方应直接用 `EventFeed::subscribe`。SSE 心跳由 daemon
线程调用 `publish_heartbeat`，因此也会进入其它订阅者（volatile，不重放）。

### 4.3 IDE 启动

`ide::startup::connect_for_workspace(cwd)`：

- `KIANA_IDE_DIR` 覆盖，否则 `<agent_dir>/../ide`（即 `~/.kiana/ide`）；
- `cleanup_stale_locks` → `discover_ides`（dead pid / workspace 不匹配已过滤）；
- `transport: "ws"` 明确返回 `IdeError::Transport`（HTTP-only，不静默降级）；
- `HttpIdeTransport::new(ReqwestWebTransport, port, authToken)` → `McpIdeClient`
  → `initialize()`；
- 没有 IDE 是 `Ok(None)`；找到但连不上是 `Err`，调用方记录后继续。

### 4.4 Web 工具

- `web::tools::web_search_tool_definition()` / `web_fetch_tool_definition()` 是
  正式 `ToolDefinition`，执行器只走 `fetch_urls`（SSRF 三层校验）与
  `SearchChain`（Brave/Google CSE/Bing 有序链）。
- `web_tools_extension()` 以隐藏内联扩展注册两个工具；`app.rs` 启动时追加到扩展
  列表。
- 两个工具 `default_active = Some(false)`：没有搜索 key 的会话不会多出永远失败的
  工具；用 `--tools web_search,web_fetch` / `defaultTools` 显式启用。
- 未注册 scraper 搜索（任务标注可选）。

---

## 5. 真实检查与测试输出

全部使用 `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在
`/home/ctyun/code/kiana` 下实际执行（磁盘曾满，已删除
`/data/cargo-target-sandbox/debug/incremental` 腾出空间后重跑）：

```text
$ cargo check --workspace
cargo check (32 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 13s
（最后一次编辑/fmt 后重跑：5 crates compiled，23.35s，exit 0）

$ cargo test -p kiana-session
513 passed, 5 ignored（76 suites，150.58s）

$ cargo test -p kiana-session-core
264 passed, 1 ignored（30 suites，22.87s）

$ cargo test -p kiana-durable
896 passed, 11 ignored（88 suites，45.04s）

$ cargo test -p kiana-rpc-print
231 passed, 24 ignored（14 suites，5.47s）

$ cargo test -p kiana-coding-agent
418 passed, 5 ignored（27 suites，11.62s）

$ cargo fmt -p kiana-session -p kiana-coding-agent -p kiana-rpc-print -- --check
fmt-check=0
```

新增用例：

- `kiana-session/src/continuity.rs`：默认开启项/目录懒创建、执行 claim 与
  clean shutdown 接管、验收记录 runtime stamp、文件路径提取（4）。
- `kiana-session/tests/continuity_wiring.rs`：`InterruptedTurn` 构造期修复并进入
  Agent 历史、`InterruptedPrompt` 的稳定 ReplayUser 且不新增 user entry、
  checkpoint+ledger+acceptance+restore 跳过用户手改、默认关闭（4）。
- `kiana-coding-agent::cron::runner`：脚本执行+字段透传、脚本失败不跑 turn、
  tick 循环立即执行并可 shutdown（3）。
- `kiana-coding-agent::web::tools`：两个 schema 为 object 且默认不激活、
  `url`/`urls` 参数解析（2）。
- `kiana-coding-agent::ide::startup`：空目录=无 IDE、dead pid lockfile 被清理（2）。
- `kiana-rpc-print::daemon`：真实 HTTP 健康检查/最新页、SSE 重放 + connected、
  WebSocket 握手 + since 重放、query/事件类型辅助（4）。

全量 `kiana-session` 由 lane 的 505 passed 增至 513 passed（+4 continuity 单测、
+4 continuity_wiring 集成）。`kiana-coding-agent` 由 331 passed 增至 418 passed，
为本波新增用例与既有套件的实际结果（未改动既有断言）。

---

## 6. 剩余缺口

1. **kiana-durable 适配未落地**：`BackgroundTaskJournal`/`BackgroundTaskNotifier`
   的生产实现仍是进程内 `MemoryTaskJournal`/`RecordingNotifier`；
   `ExecutionStore` 仍是 `FileExecutionStore`。按本波权限（durable 只在 cron
   确需时修改），没有新增 `kiana-session -> kiana-durable` 依赖，也没有 inbox/
   scheduler 事务适配。跨进程崩溃后的后台任务只能 terminalize（无 pid/fd 收养）。
2. **daemon 未接入 CLI 启动**：`DaemonServer`/`bind_daemon` 是真实 listener，
   但 `run_main` 还没有 `--daemon` 模式或从 `EventFeed` 到
   `AgentSessionEvent` 的桥；当前由嵌入方创建 `EventFeed` 并调用 `bind_daemon`。
3. **`bash background=true` 未接工具层**：模型发起的 bash 工具定义在
   `kiana-tools`（本波无所有权），无法在 `before_tool_call` 之后把执行改成后台；
   会话级 `start_background_task` API 已就绪，工具层需要一次后续接线。
4. **bash 的 agent 写入不进 ledger**：ledger 只记录参数含显式路径的文件工具；
   bash 通过 shell 写入/删除的文件无法枚举，restore 不会主动还原。
5. **cron 未在 `run_main` 启动**：`SessionJobDispatcher` + `ChannelTurnRunner`
   已提供宿主桥与脚本执行，但 `run_main` 没有创建 cron 目录/循环，也没有把
   `AgentSession` 的 prompt 放进 channel worker。`skills`/`model`/`context_from`
   的“应用”由宿主 runner 负责（`PromptOptions` 本身没有这些字段）。
6. **`InterruptedPrompt` 不自动重放**：分类与 continuation 已接线，但会话构造
   不会自动发起模型调用；宿主需要 `take_recovery_continuation()` 后按 `entry_id`
   去重发送一次。这是有意避免“打开会话就产生网络请求”。
7. **IDE 未在 app 启动流程调用**：`connect_for_workspace` 已可用，但
   `run_main`/InteractiveMode 还没有调用它，也没有把 `contextUpdate`/`openDiff`
   通知路由到 `DiffBroker`。
8. **web 工具默认不激活**：没有 `BRAVE_API_KEY`/`GOOGLE_CSE_*`/`BING_API_KEY`
   时搜索会返回可操作错误；WebFetch 也需要显式激活。scraper 搜索未实现。
9. **checkpoint 请求视图修复只在会话打开时执行**：之后的树导航/压缩走
   `refresh_finalized_context()` 的 canonical 投影，不会重新应用
   `repair_request_view`；DegradedHistory 的孤儿结果可能在后续刷新中重新进入
   请求视图。
10. **acceptance 记录只在内存**：没有持久化 `ExecutionRecord`；重启后无法用旧
    凭证重新判定（lane 的 `verify()` 逻辑已支持持久化后的复算）。

---

## 7. 给后续波的风险

- **磁盘**：`/data/cargo-target-sandbox` 曾达到 100%（deps 117G / incremental
  24G）。本轮删除 incremental 后恢复；后续波建议构建时设置
  `CARGO_INCREMENTAL=0` 或定期清理。
- **执行 claim 的 owner 存活检查依赖 pid**：Linux 用 `kill(pid,0)`，其它平台
  保守视为存活。pid 复用 + 无 clean shutdown 会把会话判为 Busy；生产需要
  pidfd/start-time 指纹（cron 已有 `ProcessFingerprint`，execution 没有）。
- **checkpoint 调用同步 git**：在 async 路径里直接执行（与现有 memory/IO 风格
  一致），大仓库的 `git add -A` 会阻塞 LocalSet。默认关闭，但开启后需要评估
  首次快照延迟。
- **daemon 无鉴权**：默认 loopback 只降低暴露面，不是认证；跨主机部署必须在
  反代加认证，且 `EventFeed` 的 durable replay log 是有界 4096，长时间断线会
  触发 gap（SSE 没暴露 gap 字段）。
- **cron 脚本执行**：`sh script` 在宿主机直接运行，没有经过权限/沙箱层；本波
  只校验路径逃逸与输出上限。接 AgentSession 时必须在 `CronTurnRunner` 宿主里
  走权限/沙箱（或明确脚本是受信任的管理员输入）。
- **cron grace 默认 0**：生产配置应设 `grace_minutes >= 5`，否则 tick 晚一秒
  就记 missed（lane 已标注）。
- **web 工具权限**：扩展工具在外层权限引擎里按 `Other` 分类，默认模式会 Ask/
  Deny；启用后仍受权限层约束，不会绕过 SSRF。
- **恢复分类是加载时快照**：`recovery_plan()` 不会随会话后续变化更新；宿主
  若在运行中重新读取，应重新调用 `plan_manager_recovery`。
- **`ContinuityRuntime` 默认关闭**：所有 DoD 行为需要 `KIANA_CONTINUITY=1` 或
  `AgentSessionConfig::continuity` 才生效；默认关闭是为了不改变 500+ 既有会话
  测试与一次性 SDK 会话的语义。后续波若要默认打开，应先跑全量回归并处理
  执行 claim 的跨进程/同进程多会话场景。
