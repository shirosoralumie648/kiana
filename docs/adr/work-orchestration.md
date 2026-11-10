# ADR: Work 编排与异构 Agent 的演进路线

- 状态：已接受（Phase 0、Phase 1a、Phase 1b、Phase 1c、Phase 2、Phase 3 已实施；Phase 4 的 profile facet 与只读 Context Shadow 已实施，其余按本文档延后）。
- 日期：2026-11-08（2026-11-09 增补第二批参考并完成 Phase 1 调研）
- 相关：`crates/kiana-session`、`crates/kiana-durable`、`agent_definition`、`subagent`、`acceptance`、`docs/adr/durable.md`

## 背景

本 ADR 回答一个问题：Kiana 从「行为保真的 coding agent」走向「异构 Agent 集群」，
下一步应该做什么。

两份外部建议的方向一致：声明式 Agent Profile、Work-Centric 编排、动态上下文。
但它们的模块拆分（23 逻辑模块 / 5 层 / 首期 12 模块 / 10 个 crate）没有一手来源支持，
且与仓库现实冲突：

- 仓库已有 32 个 crate 与严格 DAG；`ARCHITECTURE.md` §2.2 要求每条依赖边都有实际用途。
- `docs/adr/durable.md` 已裁定 `kiana-durable` 是后台任务、子代理、cron、跨进程恢复的
  唯一调度与事务底座，并明确「不新增队列」。
- 行为保真是产品目标：`AgentEvent` / `AgentSessionEvent` / `SessionHeader` / RPC /
  会话 JSONL 都是冻结 wire，字段顺序即 JSON 键序。
- 主程序运行在单线程 `tokio::task::LocalSet`，会话侧状态 `!Send`；`kiana-durable` 是
  `Send + Sync`，两者不能混为同一线程模型。

全仓能力矩阵（42 个可判定条目）的结论是 **3 exists / 34 partial / 5 missing**。
Kiana 已经是「高保真单 Agent 执行内核 + 扩展与治理底座 + 一套已实现但未接线的持久任务
引擎」；缺的不是 SDK，而是把已有能力组织成 Work 的那一层。

## 关键事实

| 事实 | 证据 |
|---|---|
| 子代理运行时已实现，但没有产品入口 | `docs/guide/subagents.md` 明确说明交互式工具与 CLI 均未暴露；`AgentSession::spawn_named_subagent` 只有宿主 API（`crates/kiana-session/src/agent_session.rs:10433`） |
| 子代理验收使用空执行记录 | `agent_session.rs:10420-10427` 以 `AcceptanceContext::new(&self.core.cwd, &[])` 调用 `evaluate_acceptance`，`tests_passed` 只能落 `Unverified` |
| durable 已有完整的持久任务引擎 | `TaskDefinition` / `TaskRecord` / `TaskState` / `TaskScheduler` / `TaskGraph`（`crates/kiana-durable/src/types.rs`、`harness/scheduler.rs`） |
| durable 未被产品路径使用 | 能力矩阵结论；目前只有 `background/durable.rs` 的文档投影在使用 |
| AgentDefinition 已是声明式 profile | `crates/kiana-session/src/agent_definition/mod.rs:174-215`，18 个字段、6 种来源、3 个内置定义 |
| 部分 Profile 字段解析后未消费 | `effort` / `mcpServers` / `hooks` / `memory` / `background` / `omitClaudeMd`；`skills` 只注入名字 |
| 上下文管线已存在，但没有 Provider/Compiler 类型 | `prepare_run` / `prepare_request` / `prepare_next_turn`、`transform_context`、compaction、memory recall 构成唯一管线 |
| wire 序列化必须走 kiana-json | `CLAUDE.md` 硬约束 3；`crates/kiana-json` |
| 会话侧 `!Send` | `docs/START_HERE.md` §3；`CLAUDE.md` 硬约束 4 |

## 决定

1. **不按 23 模块 / 10 crate 拆分。** 外部参考的一手形态是单体 SDK、协议、论文或
   4 包 Python SDK；没有证据支持把论文的模块清单映射成 Rust crate。
2. **Work 协议先作为 `kiana-session` 内部模块落地**（`subagent/work.rs`），有真实
   宿主消费者、接口稳定后再抽 crate。遵循「先逻辑边界，后物理 crate」。
3. **复用 durable 作为唯一持久化与事务底座。** Phase 1 的 Work 账本使用 durable 的
   `Session`/`Tx` 与文档 CAS；durable 的 `TaskScheduler` 不直接驱动子代理（见 Phase 1
   调研结论）。Work envelope 只承载身份、结果与证据，不实现第二套队列、状态机或重试
   规则。子代理的前台执行仍由 `AgentSession` 在 LocalSet 上驱动。
4. **一律可加性、opt-in、可回滚。** 默认路径的事件顺序、JSON 键序、落盘格式、错误文案、
   默认值与超时不变；新能力默认关闭。
5. **Context Compiler 暂不实施。** 现有上下文管线是唯一实现；在没有 `peek` API 与
   逐字节门禁之前，Shadow 也会与 `take()` 语义冲突。
6. **Agent Profile 复用并渐进扩展 `AgentDefinition`。** 不新建第三个 profile 类型，
   避免与 `AgentDefinition` / `AgentOptions` 形成三套身份。
7. **明确出界：** Dynamic 编排、跨 Agent 共享上下文、自适应调度、A2A wire protocol、
   模型可见 `task` 工具均不在首期；其中模型可见工具需要单独的产品与保真决策。

## 分阶段计划

### Phase 0（首增量）：Work envelope 接在宿主子代理路径上

目标：用最小改动建立 `work_id` / `run_id` 身份与结构化结果，且有一个真实消费者。

- 新增 `crates/kiana-session/src/subagent/work.rs`：
  `WorkId`、`RunId`、`WorkRequest`、`WorkEvent`、`WorkResult`、`WorkTerminal`、
  `WorkEvidence`、`WorkUsage`、`WorkAcceptance`。
- `WorkRequest` 只引用 `AgentDefinition` 名字，不复制 profile 字段；不新建 crate。
- 新增 opt-in 入口 `spawn_subagent_with_work`（或 `SubagentRuntimeOptions.work`）；
  默认 `spawn_subagent` 路径完全不变。
- Work 记录通过 `WorkStore` 接口保存；Phase 0 提供进程内 `MemoryWorkStore`，Phase 1
  再注入基于 `kiana-durable` 的实现（经 `background/durable.rs` 适配层），不新增队列。
- 证据：把子代理逐工具执行记录传入 `evaluate_acceptance`；`tests_passed` 不再因空
  记录静默 `Unverified`。若某类证据仍不可得，在 `WorkResult` 中显式标注，不假装通过。
- 取消：沿用 `CancellationToken`，父取消级联到子代理；`WorkResult` 区分
  `Cancelled` / `TimedOut` / `Failed`。
- 事件：只在库内提供 `WorkEvent` 流；不改 `AgentEvent` / `AgentSessionEvent` /
  `SessionHeader` / RPC / 会话 JSONL。
- 序列化：所有 wire 走 `kiana-json`；`i64` 字段先规范化数字，覆盖超过 2^53 的用例。
- 测试：`crates/kiana-session/tests/subagent_work.rs`（真实 faux stream 端到端、
  WorkEvent 顺序、MemoryWorkStore、`tests_passed` 证据链）与 `subagent/work.rs`
  单元测试（wire 字节、tagged union、校验、终态映射）。
- 验收：`cargo test -p kiana-session`、`cargo clippy -p kiana-session --all-targets`、
  `cargo fmt --check -p kiana-session`；`git diff` 只含 `kiana-session`（含
  `Cargo.toml` 新增的 `kiana-json` 边）、`ARCHITECTURE.md`、`spec/` 与本文档。
- 回滚：删除模块与 opt-in 分支、回退 `Cargo.toml` 的 `kiana-json` 边与
  `ARCHITECTURE.md` 登记；无数据迁移。

### Phase 1：durable Work 账本、幂等与恢复（调研后的最终方案）

**调研结论（2026-11-09）：**

- `TaskScheduler` 不能直接作为子代理执行器：`TaskSchedulerOptions` 要求 `SessionImpl`、
  `Storage`、`RegistryReader`、`AgentResolver`、`EnvResolver`、`Timers`、
  `ConversationResolver` 等完整 Harness 设施（`harness/scheduler.rs:441`），phase
  handler 是 `Send + Sync`；仓库内除 `kiana-durable` 自身外没有消费者。接进来等于再建
  一套 Harness，并把 `!Send` 的 `AgentSession` 排除在外。
- `BackgroundTaskManager::start` 的工作闭包是 `Send + 'static` 且用 `tokio::spawn`
  （`background/mod.rs:369`）；`BackgroundTaskResumer::resume` 的 future 也是 `Send`
  （`journal.rs:393`），不能驱动 `!Send` 的子代理。
- Phase 0 的 `WorkStore` 只有终态 `save`（`work.rs:545`），没有写前记录；崩溃后无法
  区分“从未开始”和“运行中丢失”。
- `SubagentRuntime::session_file()` 在运行前就可用（`runtime.rs:580`），可以写进
  Running 记录，作为显式恢复的锚点。
- `next_work_id()` 是进程内 `AtomicI64`（`work.rs:760`）；跨进程共享 durable 存储时
  会从 1 开始重复，不能在 durable 模式下作为幂等键。
- `DurableTaskJournal` 已经验证了 “durable `Session`/`Tx` + per-session JSONL + CAS”
  的模式（`background/durable.rs`）；Work 账本应复用这套事务与目录隔离，而不是新队列。

**Phase 1a（已实施；默认开启，但只影响 opt-in Work 路径）：**

1. 新增 `subagent/work_store.rs`：`WorkRecord` / `WorkState` / `WorkBegin` /
   `WorkFinish` / `DurableWorkStore`。记录形状与 durable `TaskRecord` 的
   identity/attempt/timestamps 词汇对齐，但不使用 `TaskScheduler`：
   `version`、`protocol`、`workId`、`runId`、`parentSessionId`、`parentTurnId?`、
   `resumedFrom?`、`request`、`state`、`attempts`、`createdAtMs`、`updatedAtMs`、
   `pid?`。`state` 是 `running`（可选 `sessionFile`）或 `terminal`（`result` +
   `terminalSeq`）。
2. `WorkStore` 从“终态归档”扩展为“生命周期权威”：
   `allocate_work_id(parentSessionId)`、`begin(record)`、`attach_session(parentSessionId,
   workId, runId, sessionFile)`、`finish(parentSessionId, result)`、
   `load(parentSessionId, workId)`、`list_recoverable(parentSessionId)`、
   `list_for_session(parentSessionId)`。`begin`
   以 `(parentSessionId, workId)` 做 durable CAS；同 `workId` 已存在时返回 `Existing`，
   不覆盖。`finish` 以 `(workId, runId, running)` 做 CAS，只有赢家写终态。
3. `DurableWorkStore` 把记录写进每个父会话独立的 durable 文档
   `kiana.work_records`（`<state_dir>/work/<session-key>/`），由
   `Session`/`Tx` 提供原子读改写；`nextWorkId` 放在同一文档里，用事务分配，解决
   跨进程重复。`MemoryWorkStore` 同步实现同一接口。`nextWorkId` 缺失（旧版账本）
   时从现有记录的最大 `workId` 之后继续发号，**不回落 1**；字段存在但非法、小于 1
   或落后于现有记录说明计数器损坏，读写 fail closed，避免复用已有 id。
4. `spawn_subagent_with_work` 在注入 `WorkStore` 时改为写前记录：
   `allocate_work_id(parentSessionId)`（无显式覆盖时）→ `begin` → `Created`/`Started` →
   准备运行时并 `attach_session` → 运行 → `finish`。`begin` 失败是运行前错误；
   `begin` 返回 `Existing` 时不再运行子代理，直接返回
   `SubagentError::WorkAlreadyExists { workId, status, runId }`，宿主可用 `load`
   读取已有记录；锚点冲突同样以 `WorkAlreadyResumed` 变体返回，而不是只能匹配字符串。`attach_session` 失败终止运行并落 Failed；`finish` 失败/输家
   只写 `WorkRunOutcome.store_error`，不改变已发生的运行结果。未注入 store 的默认
   （`recovery=fail`）路径保持 Phase 0 行为；`recovery=resume` 的 Work 必须写账，
   未注入 store 时回退到会话的 `durable_work_store()`（没有则拒绝启动），保证
   work id 与 `resume_work` 读取的 per-session 计数器属于同一命名空间。程序化构造
   的 `AgentDefinition` 不经过 `DefinitionBuilder::finish`，启动前同样执行
   `validate_stages`（空 `produces`、consume 先于 produce、重名都会拒绝）。
   `WorkResult.isolation` 保留 `retainError`：`retained=false` 既可能是“没有改动”，
   也可能是“有改动但 ownership 记录写入失败”；丢掉后者会让宿主把仍有改动的工作区
   当成可以安全回收的无改动结果。
5. `AgentSession::recover_work()`：扫描本会话 Running 记录；pid 存活或进程内活任务
   登记表命中则跳过；否则按 `recovery` 处理。默认 `Fail` 直接落定
   `Failed { error: "work interrupted: process lost", stopReason: "interrupted" }`，
   保留 `sessionFile` 供审计；`Resume` 不自动重放，返回 `resumable` 列表并保持
   Running，直到宿主显式恢复或放弃；重复 `recover_work` 会再次返回这些记录。
6. `ContinuityOptions` 增加 `work` 开关与 `<state_dir>/work`；环境变量
   `KIANA_CONTINUITY_WORK`、settings `continuity.work` 可关。`ContinuityRuntime`
   持有 `DurableWorkStore`，`AgentSession::durable_work_store()` 暴露给宿主。
7. 为实现 3/4，把 `DurableTaskJournal` 的 per-session 存储打开/提交/扫描抽成
   `durable_store.rs` 的共享 helper；`background/durable.rs` 行为不变，由现有
   `background_durable` / `continuity_wiring` 测试守住。
8. 同步 `ARCHITECTURE.md` §5 的模块树（`durable_store`、`subagent::work_store`）；
   不新增 crate，也不新增依赖边。

**Phase 1a 实施记录：**

- 新增 `durable_store.rs`：共享 per-session durable 存储，`DurableTaskJournal` 已复用。
- 新增 `subagent/work_store.rs`：`WorkRecord` / `WorkState` / `WorkBegin` /
  `WorkFinish` / `DurableWorkStore`，`kiana.work_records` 文档与 `nextWorkId`
  事务计数器。
- `WorkStore` 升级为生命周期接口；`spawn_subagent_with_work` 采用
  `begin` -> `attach_session` -> `run` -> `finish`；同 `workId` 在运行前拒绝。
- `recover_work` 默认 `Fail` 落定 `interrupted`，`Resume` 只返回 `resumable`、
  不自动重放；`create_agent_session` 启动时与 background sweep 并列调用。
- durable 记录正文把 `WorkId` / `resumedFrom` / `nextWorkId` 写成十进制字符串：
  durable JSONL 的数字与 JS 一样走 `f64`，> 2^53 的 id 会丢精度；对外 wire 仍是数字。
  `tokenBudget` / `timeoutSeconds` / `maxCalls`、usage 计数与时间戳等其它已知 u64
  字段在超过 `Number.MAX_SAFE_INTEGER` 时同样写成字符串，解码时还原——否则重启后
  会因 f64 舍入超出 u64 范围而整条记录解码失败。`terminal.output` 是宿主任意 JSON，
  不做递归改写。
- 启动顺序：`sweep_subagent_workspaces` 在 `recover_work` 之后执行，并把 Work
  账本里仍可能被恢复引用的 pid 作为 keep-set 传给隔离工作区 sweep：所有会话的
  Running 记录，以及仍被**未终态后继**通过 `resumedFrom` 指向的锚点（后继已终态
  时不再保留，keep 集合不会随会话寿命无界增长）。`.kiana/subagents` 工作区按仓库
  共享，扫描必须跨会话（`DurableWorkStore::list_all_sessions`）；只看当前会话会把
  别的会话里尚未恢复的 worktree 当成垃圾删除。没有 Work 账本、账本损坏、或任一
  会话目录被另一个存活进程持有/打不开（部分扫描）时整体跳过回收：keep-set 不完整
  时宁可留下工作区。`Core::new` 不再用空 keep-set 做基线扫描。
- 每个 `SessionImpl` 由进程级 registry 按规范化目录复用；跨进程互斥由
  `owner.lock` 上的 `flock` 提供：陈旧 `owner.json` 的删除与替换只在持锁期间发生，
  消除了“读到死 pid 后直接 unlink”的 TOCTOU。目录租约按进程引用计数，最后一个
  `DurableSessionSlot` drop 时删除本进程的 `owner.json` 并释放 `flock`；只读
  `all_sessions` 扫描不把结果写入 store 缓存，因此不会把租约持有到进程结束。
  取得 `flock` 后不再参考 `owner.json` 的 pid：被无关进程复用的 pid 不能让目录
  永久不可回收。非 unix 没有目录锁，`process_is_alive` 与 `background` /
  `isolation` 的同名 helper 一致返回 `false`——宁可接受后写者接管，也不让陈旧
  标记把目录锁死。`ContinuityRuntime::shutdown` 会释放 work / background journal
  缓存槽位的租约，干净关停后接管进程可以立即打开目录；释放是终态的（`closed`
  标志 + `try_lock` 清空），在途的打开不会在清空后重新取得租约。进程内仍有在途
  Work / 后台任务时跳过释放并把租约留给进程退出：这些写入仍归属本进程，提前释放
  会让它们写进下一个进程接管的目录，`closed` 还会让写入直接失败。`flock` 在仍持有
  进程内注册表锁时释放，避免并发 acquire 看到“无租约但锁被占”而误报他人持有。
  启动恢复的结果通过 `AgentSession::work_recovery()` 暴露（`None` / `Ok(report)` /
  `Err`），sdk 不再丢弃错误。
- Work 计数器在 `begin` 显式 `workId` 后推进到该 id 之后；`MemoryWorkStore` 的
  记录、活跃登记与计数器都按父会话隔离。`WorkLiveGuard` 按 key 引用计数，重复登记
  同一 `(parent, workId, runId)` 时输家的 drop 不会注销赢家仍在运行的记录。
  `recover_work` 只在登记之后才可能看到 Running 记录；外部取消优先于超时；
  只有本进程的活动登记才算“仍在运行”（能读到账本就证明本进程持有目录租约），
  不再用 `kill(pid, 0)` 判定，避免 pid 复用让崩溃的 Work 永远停在 Running；
  `finish` 的 `NotApplied`/`Missing` 与 `Err` 一样上报（失败终态没有落账不能被当成
  成功）。`recovery=resume` 但没有 `sessionFile` 的记录永远无法恢复，启动时直接落定
  `interrupted`，不无限期占用可恢复列表。`list`/`load` 对缺失或非对象的 `records`
  fail-closed（与 background journal 一致，Work 没有 TS 参照实现）；`begin` 的写路径
  使用同一校验，不会静默重建损坏账本的空 `records` 表（那会丢旧记录并绕过锚点检查）；
  `allocate_work_id` 同样先校验账本，不会在损坏文档上分配一个随后 `begin` 必失败的 id；
  `resumedFrom` 无法解析或类型非法时 `begin` 报错而不是当成“没有后继”（避免锚点守卫
  fail open）。`WorkUsage.cost` 的非有限值（provider 浮点加法可溢出成 `inf`）在落盘前
  钳成 0，读取侧把 JSON `null` 还原为 0：一条坏 usage 不能让整个账本永久不可解码。
  `finish` 接受
  `runId = None` 的结果（`Skipped` / 依赖失败）：此时只对 Running 状态做 CAS，
  与 `MemoryWorkStore` 一致；background journal 的写路径对损坏文档同样返回错误
  而不是 panic，避免 finish 任务卡在 Running 且没有通知。
- 连续性开关：settings 提供 `continuity` 时，`KIANA_CONTINUITY_WORK` 等单个能力的
  环境变量 kill-switch 仍然生效。会话没有执行所有权（`execution_refusal`）时跳过
  `sweep_background_tasks` 与 `recover_work`：本进程明确不该执行，也不该去 claim
  另一进程可能仍在使用的 durable 目录。
- `all_sessions` 对单个打不开的会话目录跳过并继续，只有全部目录都失败时才报错；
  一个损坏会话不再阻断其它会话的 journal 操作。`all_sessions_metered` 与
  `slot_for` 一样受 `closed` 门禁：`release_sessions_blocking` 之后在途的扫描不会
  重新获取 owner.json/flock 租约。打开会话存储时不做路径 lossy 转换：非 UTF-8 的
  durable root 直接报错，避免 flock 锁一个目录、JSONL 写另一个目录。
- 验证命令：`cargo check -p kiana-session --all-targets`、
  `cargo fmt --check -p kiana-session`、`cargo clippy -p kiana-session --all-targets`、
  `cargo test -p kiana-session`。
- Phase 1b（显式 `resume_work`）与 Phase 1c（后台执行）已实施。

**Phase 1b（已实施：显式恢复）：**

- `AgentSession::resume_work(workId, continuationPrompt, cancel)`：要求
  `request.resumable == true` 且有 `sessionFile`，并拒绝仍在本进程运行的记录。
  原记录可以是 `interrupted` 终态，也可以是崩溃遗留的 Running（先用 CAS 落定为
  `interrupted`；并发恢复的输家在这一步失败，不会重复执行）；其它终态一律拒绝。
- 恢复创建一个新 `WorkRecord`（新 `workId` / `runId`，`resumedFrom` 指向原记录，
  `attempts + 1`），请求快照从原记录复制、只替换 `prompt`；之后复用与新建完全
  相同的 `run_work_lifecycle`（写前记录、事件、超时、失败落定、store 错误上报）。
- **一个恢复锚点只能有一个后继**：`begin` 在同一个 durable 事务里扫描
  `resumedFrom`，发现已有后继时返回 `WorkBegin::AlreadyResumed`，并发
  `resume_work` 的输家不会在同一 transcript 上再跑一个运行时。扫描只读
  `resumedFrom` 字段，不重新反序列化每条记录。
- 后继在真正开始运行前就 `begin`，因此打开 transcript / 准备隔离工作区失败时，
  生命周期会先清除它的 `resumedFrom`（`WorkStore::release_resume`）再落定 Failed：
  这次后继从未执行，锚点不能被它永久占用，宿主必须能重试。`recover_work` 落定
  崩溃遗留的 Running 后继时同样归还锚点（正常完成的后继保留占用，维持“一个锚点
  只成功恢复一次”），否则进程在恢复运行中崩溃会让原记录永久不可重试。
  `finish` 与 `release_resume` 是两次提交：启动恢复会扫描“已终态但仍带
  `resumedFrom` 的记录”并补做释放，覆盖两次提交之间崩溃/EIO 的窗口；释放是幂等的，
  修复按逐层 fixpoint 重入。保留锚点的只有两类：`Success`（“一个锚点只成功恢复
  一次”），以及 `resumable && sessionFile` 的 interrupted **lineage tip**（保证同一
  条链只有一个可恢复记录）。其余终态（failed/cancelled/timed_out、非 resumable
  或无 transcript 的 interrupted）一律归还，否则原记录永久不可重试；W1→W2→W3
  全部 interrupted 时只有 W3 的占用保留，不会出现两个 runtime 追加同一 child
  JSONL。失败路径先落定终态再归还锚点；finish 失败时保留锚点交给启动恢复，release
  失败只留下一个非 Success 终态，由同一修复再次归还。
- 记录的 `sessionFile` 在 `attach_session` 时写入，但子会话文件要等首条消息落盘
  才创建。崩溃在这个窗口里时文件不存在：`resume_work` 按新建运行时继续（没有历史
  可复用）并记录诊断；直接打开会因缺少 `kiana.subagent.link` 条目失败。
- `resume_work_with_options` 允许宿主传入 `event_sink`、时钟与 store；恢复出来的新
  `WorkId` 因此仍能发 `WorkEvent`。宿主显式注入的 `WorkOptions.store` 优先，缺省才
  回退到会话的 durable 账本；`spawn_subagent_with_work` 的 `recovery=resume` 路径
  也遵循同一规则（见上），读取锚点、占用与生命周期必须使用同一个 store。请求快照
  与 `timeoutSeconds` 以原记录为准。
- 超时预算从进入工作生命周期开始计算：`prepare`（worktree / reflink / transcript
  打开）是不可取消的同步调用，无法中断，但它的耗时会从剩余预算中扣除。
  `timeoutSeconds` 超出时钟可表示范围时按无截止时间运行并记录诊断
  （`Instant + Duration` 溢出会 panic）。
- 运行时选项从记录重建：`session_dir` 取 `sessionFile` 的父目录，`depth` /
  `tokenBudget` / `isolation` / `acceptance_criteria` 取自请求，定义按 `profile`
  名字重新加载。yield 契约、运行时的 `approval`、`fork`、`sharePromptCache`、
  `isolation_options` 不在 `WorkRequest` 里，恢复后的运行使用它们的默认值——这是
  记录形状的已知边界。
- 隔离语义：`SubagentRuntime::resume` 强制按 `Shared` 恢复，复用 transcript 头里
  记录的 cwd；工作区已被清理时回退到父 cwd，**不重建 worktree**。启动 sweep 的
  keep-set 包含仍在 Running 的记录、被未终态后继引用的锚点，以及
  `recovery=resume`、`sessionFile` 存在且**尚无后继**的 `interrupted` 终态记录；
  出现后继后该锚点不再保留，keep 集合不会随会话寿命无界增长。resume 会刷新
  ownership 记录的 pid；keep-set 不能只看 `WorkRecord.pid`，还要从子会话 header 的
  cwd 找到实际 ownership 记录并保留它当前的 pid，否则失败 resume 之后的启动 sweep
  会删掉仍可恢复的 worktree。WorkId 由每个父会话的账本计数器独立分配，锚点判定
  按 `(parentSessionId, workId)` 隔离：另一个会话里同号的终态后继不能把本会话仍可
  恢复的记录排除出 keep-set。扫描目录除默认 `<cwd>/.kiana/subagents` 外，还包含
  本会话准备子代理时见过的自定义 `IsolationOptions.state_dir`，以及从 Work 记录
  推导出的状态目录。自定义 `state_dir` 在 `begin` 前写进
  `WorkRecord.isolationStateDir`：子会话 header 要等首条消息落盘才存在，崩溃在
  `attach_session` 之后、首条消息之前的 Work 仍必须能找到并回收它的 worktree/
  branch。扫描本身也不静默丢条目：目录项/类型探测失败、非 UTF-8 目录名与符号链接
  目录都会计入 `skipped`，与打开失败一样触发整体跳过。宿主仍应在恢复前确认工作区
  是否仍存在；需要全新隔离工作区的场景应重新创建 Work，而不是 resume。
- 不自动恢复：工具调用可能有副作用，恢复前必须由宿主显式确认。
- `resumable` 由 `recovery=resume` 决定；没有 `SubagentRuntimeOptions.session_dir`
  时 `spawn_subagent_with_work` 直接拒绝，避免“声称可恢复但没有 transcript”。
- 没有执行所有权（`execution_refusal`）时 `spawn_subagent_with_work` 与
  `resume_work` 都拒绝启动，与 `start_run` 的拒绝保持一致。

**Phase 1c（已实施：后台执行）：**

- 执行器：`BackgroundTaskManager::start_local` 把 `!Send` future `spawn_local` 到
  **会话线程的常驻 `LocalSet`**（`AgentSession` 安装的派发表，`session_local_set()`），
  而不是调用点恰好处于的临时 `LocalSet`。没有常驻句柄时在写 journal / 打开输出
  文件之前返回 `NoRuntime`，不留半初始化记录，也不发“任务失败”通知。
- 入口：`AgentSession::spawn_subagent_with_work_in_background` /
  `spawn_named_subagent_with_work_in_background` 同步完成与前台相同的预检（执行
  所有权、staged 边界、请求校验、WorkId 分配与已存在检查），然后返回
  `BackgroundWorkHandle { taskId, workId, runId, outputPath }`。同一
  `(parentSessionId, workId)` 已存在时在创建任务前返回 `WorkAlreadyExists`；并发
  窗口里的冲突仍由生命周期里的 `begin` 兜底。后台 Work 必须落在 durable Work
  账本上：未显式提供 `store` 时注入会话的 durable store，没有 durable work 存储
  直接拒绝。
- 双账：`WorkRecord` 是权威状态，账本里写入 `backgroundTaskId` 关联 attempt；后台
  journal 记录带 `workBacked` 标记，只是本次 attempt 的输出/通知载体。
- 终态映射：Success → Completed（结构化 output 追加到输出文件）、Failed → Failed、
  Cancelled → Cancelled、TimedOut → TimedOut、Suspended/Skipped → Failed；崩溃恢复
  落定的 `Failed { stopReason: "interrupted" }` → Interrupted/`process_lost`。
  `WorkRunOutcome.store_error` 非空时不直接发布内存里的终态：重新读取账本，只有
  本 run 的终态确实落账才映射；否则 attempt 记 Failed 并写 diagnostic，不宣称
  Completed。
- 取消：`cancel_background_task` 对 `!Send` Work 只取消 `CancellationToken`，不
  abort 生命周期、不抢写 journal 终态；Work 写出 `Cancelled` 后由映射提交 task
  终态。manager 里没有执行槽时只返回 `false`（取消请求未送达），不代写终态。
- 跨进程恢复：启动顺序是 `recover_work()` → background sweep → workspace sweep。
  `recover_work` 落定 Work 后用 `reconcile_background_work` 把权威终态写回 attempt
  （`finish_task` 幂等）。通用 sweep 扫描所有会话的 journal，但**跳过未终态的
  `workBacked` 记录**：Work 驱动的 attempt 只由所属会话的 `recover_work` 对账，
  另一个会话的启动 sweep 不能在账本对账之前把它误写成 `process_lost`（终态 CAS
  一旦落定就无法再被 Work 对账修正）。代价是所属会话不再打开时该 attempt 记录会
  一直停在 Running：fail-closed，宁可留下记录也不写错终态。`workBacked` 缺省
  false，旧 journal 记录按普通任务处理。
- 幂等：`finish_task` / `finish_resumed` 在无内存 slot 的恢复路径上先把输出文件
  截回记录里的 `outputBytes`，再追加终态输出；崩溃在“追加”与“CAS”之间不会重复写。
- 锚点策略：只有 `Success`，或“仍可被 `resume_work` 接受的 interrupted 记录”
  （`resumable && sessionFile`，即 lineage tip）保留 `resumedFrom` 占用；其余终态
  在 `finish` 成功后归还。`recover_work` 在落定终态后做逐层 fixpoint（每轮重读
  账本），把非 tip 终态的锚点全部释放，原记录不会永久不可重试。
- WorkId 计数器：`begin` 在 `i64::MAX` 处 fail-closed（`work id exhausted`），不
  wrap 到负数或复用 id；`MemoryWorkStore` 与 durable 实现一致，测试可注入
  `WorkClock` 固定 `updatedAtMs`。
- Review 路径增加 `<stage-key>` 命名空间：不同 stage 产出 `review_findings` 时不会
  互相覆盖。`review.json` 现在是
  `<cwd>/.kiana/reviews/<session-key>/<workId>/<run-key>/<stage-key>/review.json`。
- 现有 bash 后台任务（`start`）的 resumer / journal / sweep 行为不变。

**非目标：**

- 不使用 `TaskScheduler` 作为执行器，也不实现 `Waiting`/`Sleeping`/`Completing`；
  Work 在 Phase 1 只有 Running → Terminal 两态。Phase 3 的 Work Graph 再引入依赖态。
- 不持久化 `WorkEvent`；事件仍是 live best-effort，权威状态是 `WorkRecord`。
- Phase 1 每个父会话只有一个 `kiana.work_records` 文档，每次提交整文档重写；Work
  数量大时的分片/归档留到 Phase 3（与 durable 文档语义一致，不在本阶段造第二套存储）。
- 不新增 crate、不新增队列、不改冻结 wire、不自动重放有副作用的工具。
- 不在 Phase 1 实现 `max_calls`（`WorkRequest` 注释同步改为 Phase 2）。

**测试与验收：**

- `subagent/work_store.rs` 单元测试：编号分配、`begin` 重复、`finish` CAS、
  跨实例重开、`list_recoverable`、大 `WorkId`（> 2^53）的耐久往返与计数器字符串形态；
  缺失/非对象 `records` 时读写都 fail-closed。
- `tests/subagent_work_durable.rs`：持久 `session_dir` 下跑真实 faux 子代理，
  重开 store 读回 Terminal + `sessionFile`；同 `workId` 第二次调用拒绝且工具只执行
  一次；手工写入 pid=None 的 Running 记录后 `recover_work` 落
  `stopReason=interrupted`；Phase 1b 覆盖成功恢复并追加 transcript、锚点单后继、
  后继在 `prepare` 失败时归还锚点、`recover_work` 落定崩溃后继时归还锚点、缺失
  transcript 时按新会话恢复。
- `MemoryWorkStore` 与 `DurableWorkStore` 的行为门禁：锚点按父会话隔离、
  `release_resume` 可重试、`list_for_session` 按创建时间排序（实现间顺序一致）、
  `begin` 的身份/Running 校验一致、状态转换推进 `updatedAtMs`。账本损坏
  （缺失 `records`、非法 `resumedFrom`、非有限 `cost`）时读写与编号分配都
  fail-closed；`recover_work` 覆盖“终态后继仍占锚点”的崩溃窗口修复，并按 tip-only 策略
  **逐层**归还：只有 Success 与仍可恢复的 interrupted tip 保留锚点
  （W1→W2→W3 全部 interrupted 时只保留 W3 的占用）；`release_resume` 在账本事务内复查该记录仍无后继，并发 `begin` 不能从两次
  读取之间插入新的占用，后继仍在运行时不归还上游锚点；sweep keep-set 覆盖
  ownership pid 刷新、后继上界与锚点的 per-parent-session 隔离；记录里的
  `isolationStateDir` 在没有 session header 时也能被 sweep 找到；扫描计数覆盖
  非 UTF-8 名，符号链接目录**不跟随**并计入 `skipped`（链接可能指向状态目录外）；
  `recover_work` 失败时启动流程跳过 workspace sweep 并记 diagnostic（keep-set 可能
  不完整）；失败路径覆盖 release 提交失败时保留 Running。
- `tests/subagent_work_background.rs`：后台入口的双账关联、预检失败不建任务、
  无 background manager / 无 durable store 拒绝、成功 attempt 的启动对账、可恢复
  attempt 关闭为 interrupted、`Failed { stopReason: "interrupted" }` 映射为
  Interrupted、重复 WorkId 同步拒绝、取消写出 Work `cancelled` 终态。
- `tests/background.rs`：`start_local` 在常驻 LocalSet 上跑 `!Send` future、协作式
  取消、sweep 跳过 `workBacked` 记录、无 slot 的 cancel 不代写终态、恢复路径截断
  重复终态输出。
- `tests/subagent_stages.rs`：预取消的 staged Work 报 Cancelled（不先解析缺失
  owner）、定义声明 worktree + 运行时 shared override 成功、review 按 stage 命名
  空间隔离。
- `subagent/work_graph.rs` 单测：lineage 边/根、interrupted tip 的 runnable 判定、
  大于 2^53 的 WorkId 以十进制字符串编码、`WorkGraphView` 从账本重建（含跨会话
  集合）。
- `tests/subagent_work_graph.rs`：session 级 `work_graph()` / `work_graph_set_view()`
  从 durable 账本投影节点、边与 runnable。
- `tests/subagent_work.rs`：事件顺序不变，且每个事件携带 `WorkEvent.planId`。
- `subagent/work_store.rs`：账本提交产生 `kiana.work.commit` span（operation/id/
  runId/status）。
- `context_shadow.rs` 单测与 `tests/context_report_wiring.rs`：只读捕获、diff
  配对、`kiana-json` 编码、`is_enforced() == false` 且不追加会话消息。
- `agent_definition` 单测：facet 解析、旧文件兼容、`schemaVersion: 1` 与 facet
  组合被拒、重复 capability 被拒、JSON 路径同一校验。
- `tests/background_durable.rs` / `tests/continuity_wiring.rs` 保持通过（helper
  重构的行为门禁）。
- `cargo test -p kiana-session`、`cargo check --workspace`、
  `cargo fmt --check -p kiana-session`、改动文件 clippy。

**回滚：** 删除 `work_store.rs`、`durable_store.rs` 与 `ContinuityOptions.work`；
`spawn_subagent_with_work` 回到 Phase 0 终态归档；删除 `<state_dir>/work` 不影响
会话 JSONL 或 background journal。无数据迁移。

### Phase 2：声明式 stages 与 Review Loop

- `AgentDefinition` 增加可选 `stages`：owner、consumes/produces、exit criteria、预算。
- 首期只实现 sequential（plan → implement → verify）；四类标准 artifact：
  plan / changed_files / test_evidence / review_findings，复用 `yield_tool` 的 JSON Schema。
- Review Loop 的 `review.json` 必须按 `(work_id, run_id, stage)` 命名空间隔离；
  缺失/非法/陈旧按 reject。
- 审批复用 `PermissionEngine` / `ExecutionPolicy`，不新增第二条审批路径。

**Phase 2 实施记录（顺序 stages + review gate）：**

- 数据模型：`AgentDefinition.stages: Option<Vec<AgentStage>>`。每个 stage 有
  `name`（同一 agent 内唯一）、`owner`（执行它的 agent 名，缺省为声明者）、
  `consumes` / `produces`（只能取 `StageArtifact` 的四个固定名字）、`exitCriteria`
  （与 Work 相同的 acceptance 词汇）与 `budget`（`maxTurns` / `tokenBudget` /
  `timeoutSeconds`）。frontmatter 与 JSON 定义共用同一解析路径；未知字段报错；
  显式 `stages: null` 与其它可选字段一致，按缺省处理。
- 加载期校验：`stages` 非空、stage 名唯一、`produces` 非空、`consumes` 必须由更早
  的 stage 产出、artifact 名合法且同列表内不重复、声明了 `stages` 时
  `isolation` 必须是 `shared`。程序化构造的 `AgentDefinition` 不经过
  `DefinitionBuilder::finish`，`spawn_subagent_with_work` 在启动前调用同一个
  `validate_stages`，非法图不会带进运行。
- 执行边界：staged Work 只通过 `spawn_subagent_with_work` 运行（普通
  `spawn_subagent` / `resume_and_run_subagent` 明确报错，不静默只跑第一阶段）；
  不允许 `recovery=resume`、`fork`、调用方固定的 `sessionId`/`sidechainId` 或
  调用方 `yieldContract`。每个 stage 是独立子代理运行（独立会话、共享父 cwd），
  owner 定义重新加载且忽略它自己的 `stages`（不递归）。
- stage 输入输出：yield 契约由 `produces` 派生（每个 artifact 一个必填 label），
  提示词按固定顺序附带 `consumes` 的上游 artifact（`kiana-json` 序列化），并把每个
  `produces` 的完整 artifact schema（required/enum/类型）渲染进提示词，模型不需要
  猜输出形状。
  artifact schema：`plan {steps[], notes?}`、`changed_files string[]`、
  `test_evidence {command,status,summary?}[]`、`review_findings
  {severity,summary,file?,line?}[]`。缺失或非法 artifact 让 Work 失败。
- 预算与终态：`tokenBudget` 交给运行时强制，且**Work 级预算跨 stage 共享**：每个
  stage 拿到的是扣除前面 stage 已消耗后的余额，stage 自己的 `tokenBudget` 只能
  进一步收紧（否则 N 个 stage 会各花一份完整预算）。`maxTurns` 覆盖 owner 定义；
  `timeoutSeconds` 用独立的 stage 定时器取消该 stage；只有**严格早于** Work 级
  截止时间时才单独 arm（相等或更晚归 Work 定时器），避免两个定时器同一 Instant
  到期时先被调度的一方改写终态的 reason/预算归属。到点把 Work 映射为 `TimedOut`；
  定时器与 stage 收尾竞争时，已 `Completed` 的运行与 Work 级取消（host cancel /
  Work deadline）优先，不会被改写成 stage 超时。
  Work 级 `timeoutSeconds` 对 staged Work 同样生效（独立于 stage 预算），否则没有
  stage 预算的流水线可以无限超期。
- exit criteria 是硬门禁：声明了 criteria 时只有 `Passed` 通过，`Failed` 与
  `Unverified` 都让 Work 失败并列出未成立/无法判定的 criterion。标量
  `exitCriteria` 是一条完整 criterion（保留空格），需要多条时用列表。
- stage 失败/取消/超时/准备失败都**合成**终态返回：已完成 stage 的 usage、
  receipt、执行记录与 transcript 保留；Work 级 acceptance 在每条路径上都用宿主
  声明的 criteria 判定，stage 自己的 exit-criteria 报告不会进入 `WorkResult`。
  跨 stage 的执行记录与证据（`outcome.receipts`）分别重新编号 `seq`，保证
  `tests_passed` 的“最新一次匹配”不会选中前一个 stage 的旧执行，Work evidence
  也不会出现重复序号。usage 聚合使用 `SubagentUsage::add_snapshot`，把每个 stage
  provider 上报的 `totalTokens` 额外量一并累加，staged 与单运行时 WorkResult 的
  `totalTokens` 口径一致。
- stage owner 定义声明了非 `shared` 隔离时明确拒绝（staged 流水线无法为单个
  stage 建工作区），不静默改写。低层 `prepare_subagent` / `resume_subagent` 同样
  拒绝带 `stages` 的定义，避免 prepare+run / resume+run 协议静默只跑一次普通 loop。
- Review gate：任何 stage 产出 `review_findings` 时**立即**写入
  `<cwd>/.kiana/reviews/<session-key>/<workId>/<run-key>/<stage-key>/review.json`
  （身份字段写成十进制字符串，与 Work 账本一致），随后严格回读：文件缺失、JSON/schema 非法、
  `workId` / `runId` / `stage` 不匹配或 `writtenAtMs` 早于本次运行都按 reject，
  Work 终止为 `Failed`。`run-key` 是必需的：没有 durable 账本时 workId 来自进程级
  计数器，同一会话在新进程里会再次从 1 开始，只按 workId 命名会让新评审覆盖旧评审
  并共用同一个临时文件。`<stage-key>` 隔离同一 Work 内不同 stage 的评审，后一个
  stage 不会覆盖前一个。`<session-key>` / `<run-key>` / `<stage-key>` 都是可读前缀
  + FNV-1a 哈希（并限制长度），清洗后相同的不同 id 不会共享命名空间。写入前逐级拒绝 symlink
  目录组件与 symlink 目标文件；临时文件用 `O_NOFOLLOW` 打开（检查与写入之间没有
  可跟随的窗口），rename 前再复核目标：克隆仓库不能通过 `.kiana` 链接把
  `review.json` 写到工作区外。持久化发生在 stage 收尾时、终态与 exit-criteria
  判定之前：产出该 artifact 的 stage 自身失败/取消/超时，或后续 stage 失败，都不能
  丢掉已经产生的评审。`review.json` 是给宿主/审阅者的持久产物，不是权威状态。
- 本轮只实现**单轮 review gate**：verify 之后不再自动回到 implement。自动返工需要
  artifact 版本与 attempt 预算，留给后续阶段；Phase 2 不发明第二套重试队列。
- 审批仍走现有 `PermissionEngine` / `ExecutionPolicy`（stage 的 owner 定义沿用其
  `permissionMode`），没有新增审批路径或旁路。
- 测试：`agent_definition` 的单测覆盖 frontmatter/JSON 解析与五类非法声明；
  `subagent/stages.rs` 覆盖 artifact schema、yield 契约、提示词、review 缺失/非法/
  陈旧；`tests/subagent_stages.rs` 用 faux provider 跑通 plan → implement → verify、
  artifact 缺失、exit criteria 失败/不可判定、stage 与 Work 级超时、owner 隔离
  拒绝、准备失败保留上游结果、review 在产出 stage 自身失败及后续 stage 失败时仍
  持久化、跨 stage 执行与 evidence 序号单调（Work 级验收取最新一次执行）、预算跨
  stage 共享/超时优先级（含相等 deadline 归 Work 定时器）与 sweep keep-set 的单测、
  程序化构造定义的非法 stage 图在启动前拒绝、session-key/run-key 哈希与 symlink
  拒绝、临时文件 `O_NOFOLLOW`、提示词包含 artifact required/enum、review 写入失败
  与启动边界。frontmatter 单测覆盖裸文本撇号（`it's` 不吞注释）与 inline map 重复
  键；Work 账本单测覆盖 `nextWorkId` 缺失/非法/落后与“后继仍占用时拒绝归还锚点”。

### Phase 3（已实施）：Work Graph、WorkEvent 流与观测

- 图投影：`subagent/work_graph.rs` 的 `WorkGraph` / `WorkGraphView` / `WorkGraphSet`
  从**权威 `WorkRecord` 账本**重建图。节点是 Work 记录（`workId`、`runId`、
  `parentSessionId`、`profile`、状态、`depth`、`resumedFrom`、`backgroundTaskId`、
  `resumable`、`planId`、`sessionFile`、`attempts`、时间戳），边是
  `resumedFrom`（原记录 → 恢复它的后继）。`parentSessionId` 是所有权域，
  `depth` 是嵌套深度。`WorkGraphView` 不缓存状态，每次 `refresh()` 都从账本重建。
- 身份与分组：`WorkId` 按父会话独立分配，跨会话的同号记录属于不同 Work。
  单会话图用 `AgentSession::work_graph()`；跨会话用 `work_graph_set_view()` 返回
  `WorkGraphSet`（按父会话分组，并报告 `skippedSessions`），不把不同会话塞进同一
  张图。durable 的 `TaskGraph` 绑定 `Harness`、数值 `ConversationId` 与
  `TaskRecord`，Work 账本是字符串父会话 + per-session `WorkId`；这里只借用其
  图/视图词汇，不引入调度器或第二套队列。
- runnable：`WorkGraph::runnable()` 标出 Running 记录，以及仍可被 `resume_work`
  接受的 lineage tip（`resumable && sessionFile && interrupted && 尚无后继`）。
  互相独立的 lineage 会同时可运行，并行由独立分支自然获得；该方法不启动任何
  Work。图编码用 `kiana-json`，身份字段写成十进制字符串（> 2^53 的 `WorkId`
  不丢精度），边带 `kind: "resumed_from"`。
- WorkEvent 是提交的投影：`Created` / `Started` 在 `begin` 提交成功后发出；
  `AcceptanceEvaluated` / `Terminal` 移到 `finish` CAS **成功后**（失败路径同样先
  CAS 再发 `Terminal`）。CAS 失败时 Work 仍是 Running，不能投影终态。事件携带
  `seq` / `workId` / `runId` / `planId`；`seq` 是该 Work 的事件序号（1-based），
  不是账本全局提交序号。`WorkOptions.plan_id` 写入 `WorkRequest.planId`（缺省不
  参与编码，旧记录解码为 `None`）。事件仍是 best-effort，权威状态是 `WorkRecord`。
- 编排层 telemetry：`DurableWorkStore` 可注入 `Arc<dyn TelemetryContext>`
  （`set_telemetry`；`AgentSession::durable_work_store()` 自动注入会话 context）。
  每次账本提交后追加一个 `kiana.work.commit` span，属性为
  `kiana.work.operation` / `id` / `run_id` / `parent_session_id` /
  `duration_ms` / `status`；失败提交把 span 标为 error。span 在提交之后发射，
  遥测失败不会吞掉或回滚事务。既有 `SessionTelemetry` 的 agent-run/turn span
  不变。

### Phase 4：Profile facet、Context Shadow、审批与长期自主（facets + Shadow 已实施）

**已实施（默认关闭/只读，不改变既有行为）：**

- `AgentDefinition` 增加可选 `id` / `revision` / `kind` / `capabilities` /
  `schemaVersion`。Markdown 与 JSON 定义共用解析路径；未知字段仍报错。
  `capabilities` 必须是非空字符串列表且不得重复；`revision` / `schemaVersion`
  是正整数。
- 兼容与写出：旧文件（无 `schemaVersion`、无 facet）保持可读；声明
  `schemaVersion: 1` 却使用 facet 时 fail closed；`AgentDefinition::write_schema_version()`
  报告写出所需版本（使用 facet 时为 2，否则 1）。当前没有定义写回路径，facet 只
  解析、透传与暴露给宿主，不改变子代理运行。
- Context Shadow：`AgentSession::context_shadow(options)` 复用 `context_report`
  捕获只读快照（`ContextShadow`），提供总 token、利用率、分段与两次快照的
  `diff`（按 `(kind, label, role)` 分组、组内按出现顺序配对）。它不修改消息、
  工具或压缩状态；`is_enforced()` 恒为 `false`。Enforce 未实现：按 ADR 仍需先补
  `peek` API 并通过逐字节门禁。

**仍延后（按真实需求再排期）：**

- work 级审批、lease/fencing、跨 Agent 共享上下文、自适应调度。当前 Work 的
  并发边界仍由 `WorkLiveGuard` 与 durable 目录租约提供；不新增第二套审批或
  租约路径。

## 为什么不是「先建 Orchestration crate」

工作流初稿建议新建 `kiana-orchestration` 并先冻结 Work 协议。但对抗评审指出：
该协议在仓库里没有生产消费者，与「AgentProfile 没有消费者」是同一问题；
同时新 crate 会经 `kiana-session` / `kiana-durable` 传递引入 rquickjs、wasmtime、ICU
等重依赖，并需要修改根 `Cargo.toml` 与 ARCHITECTURE 的层号。

因此本 ADR 把第一步收回到 `kiana-session` 内部：先让现有宿主子代理路径产出 Work
身份与结果，等接口被真实使用后再抽取 crate。这与外部建议里「先逻辑边界、后物理 crate」
的判断一致，也避免在仓库没有提交基线时做大规模结构调整。

## 与外部参考的关系

以下结论来自 8 组一手来源调研与 4 条高风险主张的独立复核（2026-11-08）：

- **Temporal Rust SDK** 确已发布 v1.0.0（2026-09-04），且 Workflow 不要求 `Send`
  （`Rc<RefCell<W>>` + `LocalBoxFuture`）。但确定性重放、外部 Server、51,200 events /
  50 MB 上限与 durable ADR 冲突，**不引入**；只借事件日志与恢复检查的思想。
- **MCP Rust SDK** 是官方 Tier 1，稳定协议为 `2026-07-28`；但 `kiana-mcp` 是 TS 在
  `2025-11-25` 上的保真移植，**不默认升级**；rmcp 仅作 dev-dependency 互操作 oracle。
- **A2A** 有官方 Rust SDK，但全是 0.x（`a2a-lf` 0.4.2 等），**不作生产依赖**；
  其八态 Task 词表可在 Phase 3 之后作为内部词表参考。
- **ArborMem / AdaCoM / ACM** 均为 arXiv v1 预印本；四篇中只有 ACE 有 ICLR 2026
  记录。不能把其消融与提升当作工程依据，需要 `kiana-evals` 复测。
- **Microsoft Agent Framework** 1.0 已 GA，但「Agent Loops / Workflows / Harnesses」
  来自架构博客，不是 API 契约；Harness 只是组合既有 building blocks。
- **AutoGen** 已归档；**LangGraph / OpenAI Agents SDK** 没有 Rust 实现；
  **OpenHands** 是 4 包 Python SDK，V1 已删除 Runtime；**Open SWE** 仍是
  under active development，无常规范 semver。
- **Open WebUI** 的许可证限制是 branding 条款，不是「50 用户上限」。

可直接采纳的小增量（与主路线并行、默认关闭）：

1. `kiana-evals` 增加基于 AgentDojo 的注入安全套件（确定性状态断言，不用 LLM judge）。
2. `acceptance.rs` 扩展 `state_assert` / `no_collateral` / `multiple_valid` 三个 family。
3. `MemoryEntry` 增加可选 `valid_at` / `invalid_at` / `recorded_at` / `source`。
4. `kiana-mcp` 的 rmcp dev-dependency 互操作测试。
5. `AgentDefinition` 的 `id` / `revision` / `kind` 字段（Phase 2 之后）。

### 第二批：10 条技术路线、65 个条目

对 10 条路线做了逐项核实与裁决：**5 adopt / 24 later / 36 skip**。
结论是：没有任何外部运行时可以直接成为 Kiana 的依赖——所有 Python/TS/Node/Go 框架、
外部 durable 服务、Actor Runtime、浏览器/VM 栈，都与单线程 LocalSet + `!Send` 会话状态 +
冻结 wire + `docs/adr/durable.md` 的单一调度底座冲突。能移植的是契约、枚举与边界。

**可进入 Phase 0-2 的契约级借鉴：**

| 来源 | 借鉴内容 | 落点 |
|---|---|---|
| Google ADK 2.0 | chat/task/single_turn 委派模式；task 必须 leaf；input/output schema 复用 yield_tool | `subagent/work.rs` 的 `WorkRequest.mode` |
| Mastra（仅类型概念） | `WorkTerminal` 用穷尽 tagged union，拒绝裸字符串 | `subagent/work.rs` |
| CQRS / DDD Aggregate / Bounded Context | `TaskRecord` 是唯一写权威；WorkEvent 是 best-effort 投影；session 与 durable 只经 ACL 交换不可变 DTO | `subagent/work.rs`、`background/durable.rs` |
| Restate / DBOS（仅概念） | `work_id` 作 durable 入队幂等键；工具边界记录 operation + input/output/exit/duration | `work.rs`、`TaskOptions` |
| Pydantic AI（0.x 字段语义） | max_depth、usage_limits、timeout_seconds、max_calls、on_failure、read_only、resumable | `WorkRequest` |
| Strands Agents（仅分类） | WorkUsage/WorkTerminal 记录 turns/tokens/attempts/cancelled；上限前置声明 | `work.rs` |
| τ-bench | `pass^k` 与 `pass@k` 并列；acceptance 坚持最终状态断言，禁止行动序列回放与 LLM judge | `kiana-evals`、`acceptance.rs` |

**对输入建议的关键纠正：**

- AG-UI 不能作为 Phase 0 项：没有外部 UI 消费者，且 `work.rs` 尚不存在；只能后续以默认关闭的只读 `UiViewEvent` 投影接入。
- ADK 的 task 模式在其 2.0 graph workflows 内被禁用，task 子代理必须 leaf；只能作为可选 mode，不能当默认委派。
- Pydantic AI Harness/SubAgents 仍是 0.x；只借字段，不锁 API。
- Restate/DBOS/Temporal 均不能作为依赖：外部 Server/Postgres、确定性重放、`Send` handler；DBOS Rust 仍是 preview 且无 scheduler/事务 step。
- ractor/kameo/dactor 都是 `Send` 绑定；Actix 虽允许 `!Send` 但要第二套 System/Arbiter，且已软弃用；都等于第二套运行时。
- CQRS/Event Sourcing 不是新读库：TaskRecord 是写权威，WorkEvent 是投影；不重放有副作用的工具调用。
- OpenFGA 是 Go 服务且无官方 Rust SDK；只在 `kiana-permissions` 内实现 Work 作用域模型，Phase 4 再排期。
- LLMLingua/ACON 不能替换现有压缩：破坏式 token 删除、额外模型、无恢复；pruning + artifact_store + 七段摘要已覆盖主要收益。
- Sculptor 的 fold/restore 只能做 opt-in 扩展工具，读 artifact_store；默认工具表/系统提示/事件顺序不变。
- Mem0 的 ADD-only 与 Kiana 的 replace 语义相反；Hindsight 的 evidence/supersedes 只作为 later 的 frontmatter 扩展。
- Firecracker/gVisor、Browser Use、NATS JetStream、OpenFGA 服务、tau2-bench/BrowserGym/OSWorld 均因外部运行时、语言或许可证跳过。
- Microsoft dactor 确实存在于 `github.com/microsoft/dactor`，但 LICENSE 版权为个人、crates.io 发布者为个人，**不是微软正式产品**；且 `Send` 绑定，不能用。
- DBOS 支持 Rust 仅是 official preview（0.5.0，无 scheduler/事务 step）；Restate Rust SDK 是官方但 0.13.0 pre-1.0。
- τ³ 没有独立仓库，是 `sierra-research/tau2-bench` 的 v1.0.0/tau3 分支；其评分规则与 1.x 不可比。
- SWE-bench full 数据集未声明许可证，只有 Multilingual 是 MIT；任何分发只能用 Multilingual。

**裁决摘要：**

- **adopt（5）：** Google ADK 2.0、CQRS、DDD Aggregate、Bounded Context、τ-bench evaluation protocol。
- **later（24）：** Strands hooks、Pydantic AgentSpec、AG2 Envelope、mcp-agent Router/Map-Reduce、Erlang/OTP Supervisor、kube-rs reconciliation、Event Sourcing、Mem0/Hindsight/Cognee、ACON/Sculptor、Reflexion/Voyager/ToT/DSPy/GPT Researcher/Generative Agents、AG-UI/A2UI、Agent Zero/Stagehand、OpenFGA task model、SWE-bench Multilingual 等。
- **skip（36）：** Mastra/CrewAI/LlamaIndex Workflows、AG2 Network/Classic、mcp-agent 运行时、Restate/DBOS/Temporal 运行时、ractor/kameo/Actix/dactor、Supermemory/LangMem、LLMLingua 三件套、LATS/树搜索、AG-UI Rust SDK/CopilotKit/A2UI renderer/NATS、Browser Use/Firecracker/gVisor/OpenHands、OpenFGA 服务、tau2-bench/BrowserGym/OSWorld 等。

完整的 65 条矩阵与逐条理由保留在调研输出中；本 ADR 只保留可执行结论。

## 待决事项

1. 首个消费者：仅宿主 SDK API（本 ADR 默认），还是同时提供模型可见的 opt-in `task` 工具？
2. ~~Work 状态根目录：复用会话目录，还是独立目录？~~ 已定：`<state_dir>/work/<session-key>/`，
   与 `background/` 并列，不复用会话 JSONL 目录。
3. ~~恢复默认：`Fail` 还是 `Resume`？~~ 已定：默认 `Fail`；`Resume` 只做显式恢复，
   不自动重放有副作用的工具。
4. ~~是否在 Phase 0 同时修复 `tests_passed` 的证据链？~~ 已实施：子代理运行时会记录
   shell 执行记录，并由 `evaluate_acceptance` 判定。
5. Context Compiler 是否最终要做；若做，先补 `peek` API 还是接受 Shadow 的局限？
6. 是否提供 work 级审批通道；若提供，如何与现有 `PermissionEngine` 组合？

## 参考阅读顺序

1. 本仓库 `docs/adr/work-orchestration.md` 与 `docs/adr/durable.md`——先确认 Work envelope 边界、单一调度底座、冻结 wire 与 `!Send` 约束。
2. Google ADK 2.0（github.com/google/adk-python）——委派模式、input/output schema、独立 session 分支；对应 `work.rs`。
3. Pydantic AI（github.com/pydantic/pydantic-ai）——per-delegate 控制字段与 AgentDefinition 的 schemaVersion/合并规则。
4. CQRS / DDD Aggregate / Bounded Context（martinfowler.com）——写权威/读投影、Work 事务边界、session↔durable ACL。
5. Restate Rust SDK（github.com/restatedev/sdk-rust）与 DBOS Rust（github.com/dbos-inc/dbos-transact-rust）——work_id 幂等、single-writer、恢复词汇；同时确认外部服务与 preview 状态。
6. Microsoft Agent Framework 与 Magentic-One（arXiv 2411.04468）——编排模式、Task Ledger / Progress Ledger / stall。
7. OpenHands SDK 论文（arXiv 2511.03690）与 LangGraph Checkpointers / Interrupts——AgentProfile 身份、事件溯源、stage 边界与审批幂等。
8. τ-bench（arXiv 2406.12045）与 SWE-bench——pass^k、FAIL_TO_PASS/PASS_TO_PASS、no_collateral。
9. AgentDojo 与 AppWorld——注入安全评测、state-based acceptance 与 collateral damage。
10. Sculptor（arXiv 2508.04664）、ACON（ICML 2026）与 Reflexion（arXiv 2303.11366）——上下文 fold/restore、离线摘要优化、反思证据锚定（均为 later）。

## 后果

- Phase 0 不改变事件、会话 JSONL、RPC 与模型可见工具；Work 能力只有显式 opt-in 的
  宿主才会看到。
- 唯一的行为修正是子代理 acceptance 现在使用运行时执行记录：`tests_passed` 不再因
  空记录静默落为 `Unverified`。这是子代理功能（非 TS 参照路径）的证据链修复。
- 在接口稳定前不新增 crate、不改根 `Cargo.toml`、不改冻结 wire；`kiana-session`
  新增的 `kiana-json` 依赖边已登记到 `ARCHITECTURE.md` §2.2。
- Phase 1a 只新增 `<state_dir>/work/` 下的 durable Work 文档与宿主可注入的
  `WorkStore`；没有注入 store 的调用仍走 Phase 0 的终态归档，事件、JSONL、RPC 与
  background journal 不变。
- Phase 1b 只新增宿主可调用的 `resume_work`；恢复是显式的，默认路径不会自动重放
  有副作用的工具。原记录保留为 `resumedFrom` 的审计锚点，恢复本身写入新记录。
- `TaskScheduler` 不被采用为 Work 执行器；它的 `TaskRecord`/`TaskState` 只作为记录形状
  的参照。这是对 Phase 1 初稿的修正，理由是 Harness 绑定与 `Send` 约束。
- `next_work_id()` 继续服务无 store 的 Phase 0 路径；durable 路径必须用文档内计数器
  分配 `WorkId`，否则跨进程会重复。
- 代价是：模型可见的多 Agent 能力仍不会立即出现在 CLI/TUI；这是有意的保真取舍。
- 如果后续证明 Work 协议需要跨进程/跨组织，再按 A2A 词表与 durable lease 扩展。
