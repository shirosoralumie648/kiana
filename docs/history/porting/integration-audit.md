# AUDIT — 按周并行移植最终审计

日期：2026-11-08
工作目录：`/home/ctyun/code/kiana`
审计范围：`docs/history/porting/` 下的 W0（`phase-00-convergence.md`/`phase-00-convergence-verify.md`）、W1（`phase-01-permissions-approval.md`）、
`phase-02-sandbox-execution-paths.md` … `phase-12a-tui-frame-contract.md`、`wiring/phase-02-03.md` … `wiring/phase-11b-12b.md`、`fix-passes/pass-1-cross-cutting.md` /
`fix-passes/pass-2-phases-02-03.md` … `fix-passes/pass-6-phases-11b-12b.md`、`integration-e2e.md`、`integration-final.md`，以及当前工作树的生产代码与测试。

环境：`rustc 1.99.1 (58cf8f9c3 2026-10-13)`、`cargo 1.99.1 (5f94df478 2026-08-27)`、
`bubblewrap 0.6.1`（`/usr/bin/bwrap`）；`CARGO_TARGET_DIR=/data/cargo-target-sandbox`、
`CARGO_INCREMENTAL=0`。

约束：未修改 `docs/history/porting/plan.md` 与 `reference_project/**`；未 commit；没有删除测试、
没有新增 `#[ignore]`；下文所有测试数字均来自实际执行，未运行的测试不记为通过。

## 1. 结论

- workspace 共 32 个 crate（`Cargo.toml` members、`crates/` 目录、`[package]` 数量均为 32）。
- `cargo fmt --all` 与 `cargo fmt --all -- --check` 均退出码 0。
- `timeout 900 cargo check --workspace --all-targets` 退出码 0：0 error；3 条仅出现在
  test 目标的 unused-import warning（见 §8.1）。
- 任务指定的五批测试、为覆盖其余 crate 补跑的两批测试，以及独立 E2E：
  **全部退出码 0，0 failed**；五批 + 两批补充覆盖全部 32 个 crate，合计
  **13110 passed / 0 failed / 401 ignored / 947 个测试目标**；独立 E2E 另计
  **12 passed / 0 failed / 1 target**，Linux 上真实执行了 bubblewrap 分支。
- 两条 critical 均已修复并有回归测试：Wave 0 的「审批生成的 Tool 规则不区分工具来源」、
  Wave E 的「生产后台 journal/notifier 仍是进程内实现」。
- 未发现未修复 critical；没有 blocked 波次/周。
- `W12a` 与 `W12b` 判为 **shallow**：前者的帧写端仍由默认关闭的实验开关控制，后者的
  daemon CLI 与 IDE 启动接线仍缺失。其余周为 **pass**，各自文档声明的残余项见 §7。
- 总体审计状态：**partial**。可作为 Linux x86_64 的内部 alpha 预览，不具备稳定版发布条件。

## 2. 状态定义与审计方法

状态定义：

- **pass**：该波/周声明的核心交付已在默认生产路径可用；深修发现项全部关闭并有回归测试；
  剩余项是文档已声明的增强/可选接线，不推翻核心交付。
- **shallow**：发现项已关闭，但核心交付仍主要停留在库层/测试/实验开关，或主入口尚未接入
  默认生产路径。
- **blocked**：存在未修复 critical，或 workspace check/指定测试失败。

方法：

1. 逐份核对 `docs/history/porting` 的审计结论、修复清单与「未完成/剩余缺口」；
2. 不只依赖文档自述，对声明的生产接线做代码抽查（关键 symbol 见 §5、§6）；
3. 在当前稳定工作树上重跑 fmt、workspace check、五批指定测试、两批补充测试与 E2E；
   每批独立 `timeout 900`，记录 passed/failed/ignored/目标数/用时/磁盘，并比较批次前后
   源码 mtime；
4. 与 `integration-final.md` 的旧结果对照，区分「已被后续 deep-fix 关闭的旧缺口」与「当前仍存在的缺口」。

工作树稳定性：最终一轮开始前源码最新 mtime 为 `2026-11-08 05:10:08`
（`crates/kiana-agent-core/tests/loop_guard.rs`）；最终测试从 `05:27:58` 开始。
驱动脚本在每批前后计算 `crates/**/*.rs` 的最大 mtime，结果均未出现
`SOURCE_CHANGED_DURING_BATCH`。此前第一轮测试期间有并发会话修改源码，第一轮数字已全部
作废，本报告只采用最终稳定树上的结果。

## 3. Deep-fix 波次审计

| 波次 | 覆盖 | 发现数 | 审计状态 | 关键修复 | 复核 |
|---|---|---|---|---|---|
| Wave 0 | 权限与审批（W0/W1） | 1 critical + 5 major | **pass** | 审批规则记录工具来源；审批 UI 展示命令/路径；Always allow(Local/Project) 命中 Ask 不再静默失效；项目 settings 写失败不再报成功；CLI flag 不再吞后续选项；`permissions` 键真正被执行 | `PermissionRule::Approved` + `request.builtin` 匹配（`policy.rs:107/275/425`）；最终 batch1/4 全绿 |
| Wave A | W2/W3 | 3 major | **pass** | managed `disableBypassPermissionsMode` 不能被 `mode:"off"` 绕过；PowerShell 不再用 POSIX lexer（`\;` 不能藏第二条语句）；取消不再吞 `aborted/unconfirmed` 与沙箱/拒绝错误 | 最终 batch1/batch4 全绿 |
| Wave B | W4/W8 | 6 major | **pass** | hook warning/systemMessage/suppressOutput 有可见通道；hook 输出契约补全；HTTP hook URL 白名单可由 settings 配置；context report/observe_condensation 接入生产；loop guard/预算可由 CLI 启用；预算 cap 写入 stop_reason | 最终 batch2/batch3/batch4 全绿 |
| Wave C | W5/W6/W9 | 17 major | **pass** | skill `/skill:` 激活/中途停用/paths 用会话 cwd/OAuth issuer 隔离/MCP annotations 验收；plan pruning 保护、fallback 大小复检、`MIN_PRUNE_TOKENS`、`skill` 受保护工具；provider 请求管线、telemetry 子 span、logs/metrics 导出、TRACEPARENT、输入预检、重试分类、collector 状态、cost 一致性 | 最终 batch1/2/3/7 全绿 |
| Wave D | W10/W11a/W12a | 13 条发现 | **pass** | memory 抽取/selector/user scope/审计加锁；子代理生产 runner/ownership sweep/清理失败传播；durable flaky 测试改条件等待；render_core 开关只读一次、pending-user/审批卡/危险动作标志/output_language/交互式记忆审批接线 | 最终 batch2/4/5/7 全绿；周级残余见 §4 |
| Wave E | W11b/W12b | 1 critical + 12 major | **pass** | 生产后台 journal/notifier 改 durable（`DurableTaskJournal`）；每次 run 的 lease finish；recovery continuation 被 CLI 消费；degraded history 每次刷新修复；continuity settings；`bash background=true`；子代理/后台验收；apply_patch ledger；checkpoint prune；cron 启动/durable 存储/timeout；WebFetch 重定向逐跳 SSRF | 最终 batch4/5 全绿；周级残余见 §4 |

逐波说明：

### 3.1 Wave 0 — 权限与审批

审计的 critical 是：审批生成的 `Tool` 规则不带工具来源，自定义工具（例如扩展注册的
`edit`）可以复用内建工具的同名规则。当前实现把所有审批规则包进
`PermissionRule::Approved { rule, builtin }`，评估时要求 `request.builtin == *builtin`
（`crates/kiana-permissions/src/policy.rs:275/425`），并把 `approved/origin` 写入 settings
与 strict schema。5 条 major 均落在同一闭环：审批 UI 展示、Always allow 作用域、项目
settings 写盘错误、CLI flag 解析、`permissions` 键执行。最终 batch1（permissions/config）
与 batch4（session 集成）全绿。

### 3.2 Wave A — W2/W3

3 条 major 均修复：managed 策略里的 `disableBypassPermissionsMode` 与 `mode:"off"` 的
交互改为 fail-closed；PowerShell 使用自己的词法规则，`\;` 不再被当作转义而隐藏第二条
语句；取消/中止路径把 `aborted|unconfirmed` 与沙箱状态、拒绝原因保留在错误里。最终
batch1 中 `kiana-permissions`/`kiana-sandbox`/`kiana-config`/`kiana-tools` 全绿。

### 3.3 Wave B — W4/W8

6 条 major 全部关闭：hook 的 warning/systemMessage/suppressOutput 通过
`AgentSessionEvent`/交互层可见；hook 输出契约覆盖 deny/ask/updatedInput/feedback 之外的
字段；HTTP hook URL 白名单来自 settings；`context_report`/`observe_condensation` 有生产
调用点；loop guard 与 turn/token/cost/wall-clock 预算可由 shipped CLI 启用；预算 cap
反映到 stop_reason。最终 batch2 全绿、batch3 中 `kiana-agent-core`/`kiana-evals` 全绿。

### 3.4 Wave C — W5/W6/W9

17 条 major 全部关闭，按周分布：

- W5：`/skill:<name>` 创建 `SkillActivation` 并检查 `user-invocable`；技能可在会话中途
  deactivate/设置 TTL；`paths` 使用会话 cwd；OAuth issuer 隔离接入运行时；MCP
  annotations 有会话级验收测试。
- W6：active plan pruning 保护与 plan/plan-mode 恢复；fallback 追加文件清单后复检大小；
  `MIN_PRUNE_TOKENS` 同时作用于 age 分支；默认保护工具补 `skill`。
- W9：provider 适配器调用 `prepare_request`/能力门/输出预算/cache 断点；telemetry 使用
  子 span 并导出 logs/metrics；TRACEPARENT 传给 bash 子进程；输入预检保证零请求；
  agent 重试使用 typed status/isRetryable（含 425/529）；run collector 表达超时/中止；
  run summary cost 与 telemetry cost 一致。

最终 batch1（config/tools）、batch2（compaction）、batch3（ai/telemetry/evals/agent-core）、
batch4（session 集成）、batch7（resources/builtin-extensions）全绿。

### 3.5 Wave D — W10/W11a/W12a

13 条发现全部关闭：memory 的 provider 抽取、provider selector、user scope、审计锁；
子代理生产 runner（真实 `kiana_agent_core::Agent` 循环）、ownership 记录启动清理、
worktree 清理失败不吞错；durable 测试的固定 3ms `drained()` 改为条件等待；render_core
的开关读取、pending-user 队列、交互式审批卡、危险动作标志、`output_language` 进入
system prompt、交互式 memory 审批（`permission_card` + 一次性 token）全部接入生产路径。

周级仍保留的 partial 已由 `fix-passes/pass-5-phase-12.md` 文档明确声明：FrameWriter/TranscriptLedger 未替换
默认 `TuiMainScreen`/`TuiAltScreen` 写路径（`KIANA_NEXT_RENDER_CORE` 默认关闭）、
子代理 `NeedsApproval` 在 headless runner 中成为 failed tool result、risk advisory 的
真实 sideQuery 未接、子代理 skills 正文/CLAUDE.md 未注入、memory 抽取没有周期调度。

### 3.6 Wave E — W11b/W12b

critical 是：生产 `ContinuityRuntime` 注入的后台 journal/notifier 仍是
`MemoryTaskJournal`/`RecordingNotifier`，进程退出即丢。现在
`crates/kiana-session/src/continuity.rs:221` 注入 `DurableTaskJournal`
（`background/durable.rs`，基于 `kiana-durable` 的 `SessionImpl` + 每会话 JSONL，
`finish` 用 `terminal_seq` CAS，通知投递行按 `(task_id, terminal_seq)` 幂等），
`create_agent_session` 返回前执行 `sweep_background_tasks()`。

12 条 major：每次 run 的 `ExecutionLease::finish`；`take_recovery_continuation` 在 CLI
启动路径消费；DegradedHistory 每次 context refresh 重新 `repair_request_view`；
continuity settings（`continuity: true`/对象）与 `KIANA_CONTINUITY` 回退；bash
`background=true` 包装；后台/子代理完成时执行 acceptance；apply_patch 写入 ledger；
启动 prune checkpoint；cron 在 `run_main` 启动（`KIANA_CRON=1` 或 settings `cron`）、
`FileCronStore` 改用 `kiana_durable` record store、脚本 timeout 覆盖 `wait()`；
WebFetch 重定向逐跳 SSRF 校验。

## 4. 周维度审计

| 周 | 主题 | 状态 | 关键残余（详见 §7） |
|---|---|---|---|
| W0（`phase-00-convergence.md`） | 权限启用入口、运行期模式切换、`execute_bash` 权限路径 | pass | — |
| W1（`phase-01-permissions-approval.md`） | 结构化审批、Always allow 作用域、持久化与事件 | pass | 非交互 `--yes` 未实现；MCP 工具「总是允许」粒度偏粗 |
| W2（`phase-02-sandbox-execution-paths.md`） | 统一沙箱执行策略、`ExecutionStatus`、read-before-write | pass | MCP stdio 未注入会话级 `SandboxManager`；`BashResult` 仍为旧字符串契约；PowerShell 无固定沙箱 manager 字段 |
| W3（`phase-03-config-shell-risk.md`） | 配置分层、严格诊断、shell 风险分析 | pass | 配置层来源未展示在 UI；`SettingsManager::apply_overrides` 的 permissions 覆盖不进入分层 |
| W4（`phase-04-hooks.md`） | hooks 配置、matcher、派发、退出码/JSON 契约 | pass | managed hooks 无 `Managed` 来源；async hook 无全局并发上限；hook audit 仅内存；SessionStart `additionalContext` 只审计 |
| W5（`phase-05-mcp-skills.md`） | MCP prompts/stdio 环境白名单/重定向、Skills 按需加载 | pass | `context: fork`、激活 hooks/`agent_scope`/`model`/`effort` 未实现；`/mcp` 结构化状态未进详情页 |
| W6（`phase-06-compaction-output.md`） | 压缩前 pruning、工具输出预算、压缩恢复 | pass | PTL 头部丢弃仍是单次 compact+retry；恢复附件只注入一次 |
| W7（`phase-07-editing-experience.md`） | multi_edit、apply_patch、流式 patch 解析 | pass | Regex 编辑策略仅为保留值；apply_patch 无 TUI 渲染；跨文件无真正事务 |
| W8（`phase-08-evals-loop-guard.md`） | mock provider、loop guard、turn/token/cost 预算 | pass | `StopReason` 未新增 cap 变体；原生 eval suite 只跑确定性 mock；`pass@k` 无置信区间 |
| W9（`phase-09-request-policy-otel.md`） | provider 能力矩阵/预算/错误分类、OTel | pass | gRPC e2e 缺失；OTLP 为子集；内容捕获仅构造期读取；tracestate 只校验 |
| W10（`phase-10-persistent-memory.md`） | 持久记忆存储/索引/召回/抽取/治理 | pass | 抽取只在会话创建时启动一次，无周期调度；provider 抽取需要宿主配置 |
| W11a（`phase-11a-subagent-runtime.md`） | 子代理运行时、AgentDefinition、隔离 | pass | skills 正文/CLAUDE.md 未注入；overlayfs 需 root 未实测；worktree 不自动 seed 父改动；`NeedsApproval` headless 失败 |
| W11b（`phase-11b-background-checkpoints.md`） | 后台任务、恢复、checkpoint、acceptance | pass | `BackgroundTaskResumer` 未实现（可选）；`ExecutionStore` 仍是 `FileExecutionStore`；跨进程通知未接；无 `--continuity` flag |
| W12a（`phase-12a-tui-frame-contract.md` 前半） | TUI 帧契约、finality 账本、审批卡、i18n | **shallow** | FrameWriter/TranscriptLedger 未替换默认渲染器；risk sideQuery 未接；i18n 仅 en/zh；48 个旧组件未迁移 |
| W12b（`phase-12a-tui-frame-contract.md` 后半） | Web/IDE/daemon/cron + 发布硬化 | **shallow** | daemon CLI 启动与 IDE `connect_for_workspace` 未接入 `run_main`；web 工具默认不激活；daemon 无鉴权；cron 脚本不走权限/沙箱 |

Wire 与周的映射：Wire A = W2/W3；Wire B = W4/W7/W8；Wire C = W5/W6/W9；
Wire D = W10/W11a/W12a；Wire E = W11b/W12b。五份 wire 文档的「剩余缺口」中，已被
deep-fix C/D/E 关闭的项不再计入本表：MCP annotations e2e、TRACEPARENT、cron 启动、
bash background、memory 抽取/selector/user scope/交互审批、审批卡接管、WebFetch 重定向
等均已落地；仍存在的项见 §7。

## 5. 两条 critical 的独立复核

1. **Wave 0 / 权限规则来源**：`crates/kiana-permissions/src/approval.rs:119-128` 统一用
   `PermissionRule::Approved { rule, builtin }` 包装；`policy.rs:275/425` 的匹配要求
   `request.builtin == *builtin`；`request.rs` 的 `builtin` 区分内建与扩展/SDK 工具。
   回归测试覆盖「自定义 edit 的批准不命中内建 edit」与会话批准 key 带来源。
   结论：**已修复**。
2. **Wave E / 生产后台 journal**：`crates/kiana-session/src/continuity.rs:185/221` 持有并
   注入 `DurableTaskJournal`；`background/durable.rs` 使用 `kiana-durable` 的 `SessionImpl`
   事务文档 + JSONL，`finish` 以 `terminal_seq` CAS，通知投递行幂等；`sdk.rs:832` 在会话
   创建后 `sweep_background_tasks()`；回归测试覆盖「崩溃后新 journal 实例恰好通知一次」
   与「另一个 journal 实例可见 ContinuityRuntime 启动的任务」。结论：**已修复**。

其余 major 的修复位置与回归测试见 `fix-passes/pass-1-cross-cutting.md` … `fix-passes/pass-6-phases-11b-12b.md` 的逐条表格；本审计对
Wave A–E 的生产 symbol 做了抽查（`PermissionRule::Approved`、`DurableTaskJournal`、
`run_subagent(`、`ApprovalQueueComponent`、`output_language_system_rule`、
`follow_redirects`、`sweep_background_tasks`、`spawn_background_extraction`、
`wrap_bash_background`、`run_cron_loop`），均在生产路径存在调用点。

## 6. 参考项目对照

### 6.1 采用的设计（按参考项目）

| 参考项目 | 采用的核心想法 | Kiana 落点 |
|---|---|---|
| codex | 三档沙箱策略、bwrap 参数顺序、fail-closed、`SandboxErr`/`ExecExpired` 与普通退出码分离；配置层优先级/origins/fingerprint；hooks matcher/trust/输出解析；apply_patch 流式解析；后台 terminal 生命周期；子进程环境白名单 | `kiana-sandbox`、`kiana-config/settings/layers.rs`、`kiana-hooks`、`kiana-tools/streaming_parser.rs`、`kiana-session/background`、`kiana-mcp/transports/stdio.rs` |
| ClaudeCode | hook 同步/异步契约与 SSRF/HTTP 白名单；managed permission policy（禁用 bypass、限制规则持久化、数组合并）；MCP prompts/状态/过滤/annotations/OAuth issuer；skills frontmatter 与技能索引预算；memory 类型/索引上限/扫描/召回/side query；后台任务 `notified` CAS、磁盘输出、stopTask；子代理加载与运行；会话恢复；worktree；压缩后文件状态恢复 | 对应 `kiana-hooks`、`kiana-permissions`、`kiana-builtin-extensions/mcp`、`kiana-resources/skills`、`kiana-memory`、`kiana-session/background`、`kiana-session/subagent`、`kiana-session-core/recovery.rs`、`checkpoint` |
| gemini-cli | hooks 顺序执行/trusted hooks/ConfigSource 优先级；policy engine 的 shell 分析；shadow git checkpoint；loop detection；MCP include/exclude glob；checkpoint 元数据；shell utils 检测面；MCP OAuth issuer | `kiana-hooks`、`kiana-permissions/shell.rs`、`kiana-session/checkpoint`、`kiana-agent-core/loop_guard.rs`、`kiana-builtin-extensions/mcp/filter.rs` |
| cline | provider manifest capabilities、typed error classification、gateway 输出预算；每 turn 首次模型调用前的 git checkpoint | `kiana-ai/request_policy`、`kiana-session/checkpoint` |
| goose | cache semantics、context report、`gen_ai.*` telemetry、turn 预算 | `kiana-ai`、`kiana-agent-core`、`kiana-telemetry` |
| deer-flow | 子代理终态契约/执行器/验收 leaf；tool receipts 哈希；checkpoint retention 保护集合 | `kiana-session/subagent`、`acceptance.rs`、`checkpoint/mod.rs` |
| hermes-agent | 记忆威胁扫描/`<memory-context>` fence/memory 工具锁/checkpoint manager | `kiana-memory`、`kiana-session/checkpoint` |
| kilocode / opencode | memory schema/limits/slug/decision 审计；read-before-write 工具级契约；opencode SSE 共享事件 | `kiana-memory`、`kiana-tools`、`kiana-rpc-print/event_feed.rs` |
| oh-my-pi | TUI frame/history batch/ledger；run collector 字段 | `kiana-interactive/render_core`、`kiana-agent-core/run_collector.rs` |
| OpenHands | transcript 投影、optimistic user message | `kiana-interactive/render_core/transcript.rs`、`pending_user.rs` |
| qwen-code | async hook registry、shell AST 最小规则、重试分类、i18n、SSE Last-Event-ID | `kiana-hooks`、`kiana-permissions/shell.rs`、`kiana-ai`、`render_core/i18n.rs`、`event_feed.rs` |
| aider | 评测任务目录/逐题结果 JSON | `kiana-evals/native.rs` |
| AutoGPT | 记忆 schedule 首次 claim 唯一胜者；provenance 形状 | `kiana-memory/extract.rs`、`entry.rs` |

所有参考仅用于理解行为与字段形状；ClaudeCode 为闭源/受限许可项目，其余为各自许可项目，
Kiana 代码均为重写，没有复制参考代码。

### 6.2 明确不采用的想法及理由

| 不采用 | 理由 |
|---|---|
| Windows 进程组/job object 终止（W4） | 本机 Linux，无法验证；当前仅 `kill_on_drop` 杀直接子进程，文档已标注 partial |
| codex async hook 的 8 并发上限（W4） | project hook 受 trust gating 保护，风险有限；未移植全局 semaphore |
| ClaudeCode「stdout 首行返回 `{"async":true}` 转后台」的运行时协议（W4） | 只实现配置级 `async:true`，避免在结果协议里引入二级解析 |
| HTTP hook 走环境代理（W4） | 有意 `no_proxy()`：代理会在代理端解析域名，绕过本进程 DNS/SSRF 校验；企业代理环境不可用是已知代价 |
| shell 规则自动生成 `prog *` 通配、以及 virtual Allow 规则参与 shell 权限扩张（W3） | 避免扩大 shell 权限；只让 Deny/Ask 型 virtual 规则约束等价操作 |
| 正则编辑策略（W7） | `EditStrategy::Regex` 仅保留值，避免模型误触不可预期的正则替换 |
| 把 multi_edit 暴露为新的模型工具（W7） | 保持 8 个内置工具的 golden 契约；多 edit 作为 `edit` 的原子引擎 |
| 新增 `StopReason::{TurnCapped,...}` 变体（W8） | 会破坏下游穷尽 match；cap 通过 `BudgetStopReason`/`raw_stop_reason` 表达 |
| 原生 eval suite 接真实 provider、隐藏测试移出 workspace（W8） | 当前只跑确定性 mock 轨迹；真实模型评测仍走旧 docs A/B 路径 |
| 完整 OTLP proto、gRPC e2e、运行期内容捕获切换、tracestate 合并（W9） | 只编码本仓库会产生的事件类型；本地无 HTTP/2 collector；内容捕获在 `SessionTelemetry::new` 时读取一次 |
| week-10 的 PID dream lock（W10） | 用 TTL + flock 达到同等互斥，不复制 PID 过期逻辑 |
| `BackgroundTaskResumer` 的 pid/fd 收养（W11b） | handoff 明确 `None` 即 terminalize；真正收养子进程属于后续波 |
| i18n 除 en/zh 外的语言包、旧 48 个组件批量迁移（W12a） | 旧路径未切换，批量迁移会牵动全部渲染 golden |
| daemon 鉴权与非 loopback 监听（W12b） | 默认 loopback 只降低暴露面；跨主机必须由反代提供认证，文档已声明 |
| 非交互 `--yes` 自动批准（W1） | 无 UI 时保持 fail-closed，避免静默默认放行 |
| cron 脚本经过权限/沙箱层（W12b） | 当前按「管理员可信输入」处理；接入 AgentSession 时需要补权限/沙箱 |

### 6.3 与参考项目的剩余差距

- 平台：Windows/macOS 沙箱后端未实现/未验证（Linux bwrap/Landlock 专有）。
- 进程生命周期：后台子进程的跨进程收养（pidfd/reopen）未实现。
- 可观测性：完整 OTLP proto、gRPC 端到端、TTFT/TTFM、hook/startup 埋点仍缺。
- 真实模型路径：sideQuery（风险解释/记忆召回）、真实 provider eval、真实压缩摘要未在
  E2E 中覆盖。
- TUI：帧契约写端未替换默认渲染器；旧组件与新 render_core 并存。
- MCP：stdio 子进程尚未统一走会话沙箱；annotations 只有策略层 + 会话接线，缺进程内
  fixture 的完整链路。
- 安全：daemon 无认证；cron 脚本不经权限/沙箱；Web 工具默认不激活。

## 7. 未修复项与残余风险（按当前代码，而非 FINAL 的旧清单）

1. **W2 沙箱覆盖不完整**：`kiana_builtin_extensions::builtin_extensions()` 仍用
   `McpExtensionOptions::default()`（`sandbox: None`），会话级 `SandboxManager` 没有注入
   MCP stdio 子进程；`BashResult` 仍是旧字符串契约，结构化 `ExecutionStatus` 未导出；
   PowerShell 工具选项没有固定沙箱 manager 字段。
2. **W3/W4 hooks 管理面**：`HookSource` 无 `Managed` 变体，
   `allowManagedHooksOnly=true` 会停用所有 hooks（安全但功能缺失）；hook audit 只在内存
   环形缓冲；SessionStart `additionalContext` 仅审计；async hook 无全局并发上限；
   HTTP hook 不支持企业代理。
3. **W5 Skills/MCP**：`context: fork` 未执行；技能激活 hooks/`agent_scope`/`model`/
   `effort` 未实现；`/mcp` 结构化状态未进管理器详情页。
4. **W6/W9 压缩与遥测**：PTL 头部丢弃仍是单次 compact+retry；恢复附件只注入一次；
   gRPC e2e、完整 OTLP、运行期内容捕获切换、tracestate 合并未做；`run collector` 的超时/
   中止依赖错误文本识别。
5. **W7 编辑**：Regex 策略未实现；apply_patch 无 TUI 渲染；跨文件无真正文件系统事务。
6. **W8 评测**：`StopReason` cap 变体未一等化；真实 provider eval 未接；`context_exhausted`
   仅按文案推断；`pass@k` 无置信区间。
7. **W10 记忆**：抽取只在 `AgentSession::new` 启动一次，无周期性调度；provider 抽取/召回
   依赖宿主配置；无 provider 时召回回退启发式。
8. **W11a 子代理**：skills 正文/CLAUDE.md 未注入；overlayfs 需 root 未实测；worktree 不
   自动带入父工作区未提交改动；`NeedsApproval` 在 headless runner 成为 failed tool result。
9. **W11b 后台**：`BackgroundTaskResumer` 未实现；`ExecutionStore` 仍是
   `FileExecutionStore`；跨进程通知投递未接；无 `--continuity` CLI flag；cron
   `context_from` 只支持会话文件路径。
10. **W12a TUI**：FrameWriter/TranscriptLedger 未替换默认 `TuiMainScreen`/`TuiAltScreen`
    写路径（`KIANA_NEXT_RENDER_CORE` 默认关闭，仅 `render_core_host.rs:42` 读取）；risk
    advisory 的真实 sideQuery 未接；i18n 仅 en/zh；48 个旧组件未迁移。
11. **W12b 启动与运维**：`run_main` 没有 daemon 模式（`DaemonServer`/`bind_daemon` 仅库
    可用）；IDE `connect_for_workspace` 未被启动流程调用；web 工具默认不激活；daemon 无
    鉴权；`EventFeed` replay 有界 4096 会 gap；cron 脚本不经权限/沙箱、`grace` 默认 0。
12. **跨领域**：Windows/macOS 未验证；3 条 test-only unused-import warning；本次未重跑
    clippy（FINAL 记录过 0 error/197 warning 诊断，但 deep-fix D/E 之后未复核）；真实
    provider/网络路径未测；满载并发下 `JsEngine` prelude 的 100 ms watchdog 会让
    `regression_2835_tools_allowlist` 与 `regression_9789_context_system_messages`
    两个目标失败（隔离重跑通过，见 §8.4）；`integration-final.md` 与 `integration-e2e.md` 的部分缺口描述已被
    deep-fix 关闭但文档未同步（见 §9）；并发会话在 04:54–05:10 修改过 12 个源码文件且
    没有 workflow 文档记录（见 §9）。

## 8. 真实命令与结果

### 8.1 格式化与 workspace check

```text
$ cargo fmt --all
（无输出，退出码 0；/tmp/kiana-audit-final-fmt.log）

$ cargo fmt --all -- --check
（无输出，退出码 0；/tmp/kiana-audit-final-fmtcheck2.log）

$ timeout 900 cargo check --workspace --all-targets
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 48s
CHECK_EXIT=0（/tmp/kiana-audit-final-check2.log）
```

check 的 3 条 warning 全部位于 test 目标、不影响构建：

- `crates/kiana-compaction/tests/pruning.rs:16` 未使用 `USELESS_NOTICE`；
- `crates/kiana-tools/src/bash.rs:1851/1881`（lib test）未使用
  `kiana_agent_core::types::AgentToolExecute`。

说明：在磁盘清理过程中曾误删 `debug/deps` 下的 proc-macro `.so`，导致一次 check 以
E0463（找不到 `serde`/`tokio` 等 crate）失败；从另一 target 目录恢复 66 个 `.so` 后，
同一命令通过。该失败是构建缓存维护问题，不是源码问题。

### 8.2 指定五批测试 + 两批补充测试

每批均为 `timeout 900 cargo test ...`，`CARGO_TARGET_DIR=/data/cargo-target-sandbox`、
`CARGO_INCREMENTAL=0`。表中「目标」为日志中 `test result:` 行数（lib/integration/doc
测试目标个数），数字由日志汇总。

| 批次 | 命令（包列表） | 退出码 | passed | failed | ignored | 目标 | 用时 |
|---|---|---|---|---|---|---|---|
| 1 | `kiana-permissions kiana-sandbox kiana-config kiana-tools` | 0 | 1367 | 0 | 1 | 84 | 121s |
| 2 | `kiana-hooks kiana-memory kiana-compaction kiana-mcp` | 0 | 728 | 0 | 2 | 37 | 51s |
| 3 | `kiana-ai kiana-telemetry kiana-evals kiana-agent-core` | 0 | 2705 | 0 | 341 | 240 | 240s |
| 4 | `kiana-session kiana-session-core kiana-durable` | 0 | 1760 | 0 | 17 | 205 | 418s |
| 5 | `kiana-tui kiana-coding-agent kiana-rpc-print` | 0 | 2071 | 0 | 35 | 101 | 54s |
| 6 | `kiana-json kiana-core kiana-theme kiana-export kiana-ext-api kiana-ui-kit kiana-env kiana-env-client` | 0 | 1429 | 0 | 1 | 129 | 175s |
| 7 | `kiana-resources kiana-extensions kiana-builtin-extensions kiana-interactive kiana-chord kiana-codemode` | 0 | 3050 | 0 | 4 | 151 | 386s |
| **合计** | 五批 + 两批补充 | 0 | **13110** | **0** | **401** | **947** | — |

补充说明：

- 五个指定批次覆盖 18 个 crate；第 6/7 批是为覆盖其余 14 个 crate 而补跑（与 FINAL 相同的
  做法），使 32 个 crate 全部至少运行过一次自己的测试。
- 第 4 批包含 `kiana-session/tests/porting_e2e.rs`，因此下表 E2E 的 12 个测试是批 4 的
  子集；报告总数不重复计入 E2E 的 12 个。
- ignored 均为既有 live/provider/凭据/`KIANA_BIN`/平台类用例，没有新增 `#[ignore]`。
- passed 为日志中 `test result:` 行求和；batch5 的 autocomplete locale 用例会在子进程内以
  过滤器自跑一次（`1 passed; 56 filtered out`），因此该用例被计两次（+1）；FINAL 使用同一
  统计口径。
- 每批结束后清理了该批新生成的测试可执行文件（保留 `.so` 与 rlib）以控制磁盘；清理后
  `/data` 可用空间保持在约 105–111 GB。

### 8.3 独立 E2E

```text
$ timeout 900 cargo test -p kiana-session --test porting_e2e
    Finished `test` profile [unoptimized] target(s) in 3.99s
    Running tests/porting_e2e.rs (…/porting_e2e-deb152409ad24129)
running 12 tests
...
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.31s
E2E_EXIT=0 ELAPSED=5s
```

12 个测试全部通过，包含真实 bwrap 分支（`workspace_write_shell_uses_real_bubblewrap_or_reports_backend_unavailable`
在本机 `/usr/bin/bwrap` 可用时执行真实隔离；不可用时按 `BackendUnavailable` fail-closed）。
该目标也出现在 batch4 的 205 个目标内（12 passed），本节的独立复跑不重复计入总数。

### 8.4 独立并发全量运行（参考证据，不计入本轮数字）

审计期间，本机另一个 Claude 会话在同一工作树上执行了
`CARGO_TARGET_DIR=/data/cargo-target CARGO_PROFILE_TEST_DEBUG=0 cargo test --workspace --no-fail-fast`
（2026-11-08 05:17:56–05:38:29）。结果：**13104 passed / 6 failed / 401 ignored /
947 targets**。6 个失败全部集中在两个 kiana-session regression 目标：

- `tests/regression_2835_tools_allowlist.rs`：2 failed / 0 passed；
- `tests/regression_9789_context_system_messages.rs`：4 failed / 0 passed。

失败位置均为 `kiana_extensions::engine::install(&engine).expect("engine prelude")`
（`tests/session_support/mod.rs:54`、`tests/common/mod.rs:416`），错误为 `Exception`。
失败时间与本轮 batch4 编译 `kiana-session` 的 CPU 饱和窗口重合；`JsEngine` 的默认
watchdog 只有 100 ms（`crates/kiana-extensions/src/engine/limits.rs:81`），prelude 求值
在满载机器上可能被中断。该目标是同一棵树上的独立全量证据，但 6 个失败说明测试在满载
环境下不稳定，需要在发布前处理（安装 prelude 用显式更长 deadline，或降低测试并行度）。
**本轮 batch4 在无其它全量测试竞争时，两个目标均通过**：
`regression_2835_tools_allowlist` 2 passed / 0 failed，
`regression_9789_context_system_messages` 4 passed / 0 failed。因此判定为负载相关的
测试脆弱性（环境 flake），不是产品逻辑失败；但它仍是发布前的稳定性风险。

### 8.5 与 FINAL 的对照

- FINAL（2026-11-07）五批为 8498 passed / 0 failed / 396 ignored / 656 targets，补充两批
  4463/0/5/279，总计 12961/0/401/935。本轮七批为 **13110 / 0 / 401 / 947**；其中任务五批
  子集为 **8631 / 0 / 396 / 667**，补充两批为 **4479 / 0 / 5 / 280**。相比 FINAL，五批
  多 133 passed、11 targets，补充多 16 passed、1 target，来自 deep-fix 新增的回归测试。
- FINAL 的 check 只有 1 条 `dead_code` warning；当前树的 1 条 `dead_code` 已消失，新增/保留
  3 条 test-only unused-import warning。
- FINAL 的「已知缺口」中，cron 启动、bash background、memory 抽取/审批/user scope/selector、
  审批卡接管、MCP annotations e2e、TRACEPARENT、WebFetch 重定向已由 deep-fix C/D/E 关闭；
  仍存在的项已并入 §7。

## 9. 文档一致性与并发修改

- `integration-final.md` 的「已知缺口」与测试数字是 2026-11-07 的状态；deep-fix D/E 之后至少 8 项已
  关闭，但 FINAL 未更新。`integration-e2e.md` 仍称「后台记忆抽取在 FINAL 中仍是已声明缺口」，该说法
  已被 `fix-passes/pass-5-phase-12.md`（memory_wiring 的 `spawn_background_extraction`）推翻。
- `phase-00-convergence-verify.md` 写 workspace 30 个 crate；当前为 32 个（hooks/memory 之后新增），
  FINAL 已修正为 32，但 W4-verify 未更新。
- 并发会话（另一个 Claude 进程，约 04:54–05:10）修改了 12 个源码文件且没有 workflow 文档
  记录：`kiana-hooks/src/ssrf.rs`、`kiana-hooks/tests/http_hooks.rs`、
  `kiana-tools/src/apply_patch.rs`、`kiana-tools/tests/apply_patch.rs`、
  `kiana-tools/src/multi_edit.rs`、`kiana-agent-core/src/agent.rs`、
  `kiana-config/src/settings/strict.rs`、`kiana-session/src/agent_session.rs`、
  `kiana-agent-core/src/loop_guard.rs`、`kiana-hooks/src/http.rs`、
  `kiana-agent-core/src/context_report.rs`、`kiana-agent-core/tests/loop_guard.rs`。
  本轮最终测试包含这些改动，但它们的审计记录缺失，建议补一份变更说明。
- 本轮审计只新增 `docs/history/porting/integration-audit.md`；没有修改源码、`docs/history/porting/plan.md` 或
  `reference_project/**`。

## 10. E2E 覆盖

`crates/kiana-session/tests/porting_e2e.rs` 共 12 个测试，覆盖任务要求的 7 类场景：

1. 权限：read-only/default/bypass 在真实 `execute_bash` 路径上的行为；
2. AllowOnce 不持久、Session 作用域只在会话内；
3. 文件工具拒绝工作区外写入；
4. 真实 bubblewrap workspace-write（或后端不可用时 `BackendUnavailable` fail-closed）；
5. 非法 `KIANA_SANDBOX_POLICY` 在子进程 fail-closed；
6. read-before-write（未读 edit 被拒、外部修改检测）；
7. hooks（PreToolUse exit 2 阻断、updatedInput 先于权限判断）；
8. 压缩 pruning 保持 tool pair 合法、artifact offset 回读；
9. memory 写入→召回→forget，secret 不落盘；
10. 子代理继承父权限收窄。

未覆盖（`integration-e2e.md` 已声明原因）：完整 LLM 压缩摘要管线、TUI 审批渲染、HTTP/async hooks、
worktree/overlayfs 隔离、后台记忆抽取、MCP 子进程沙箱、PowerShell、Windows/macOS、
进程级权限环境变量、AllowAlways Local/Project 磁盘写回、同尺寸/同 mtime 的读后写哈希检测。
其中后台记忆抽取已由 `fix-passes/pass-5-phase-12.md` 实现并在 `kiana-session` 单测覆盖，但 E2E 未重跑该链路。

## 11. 发布建议

- **建议**：作为 `0.1.0-alpha` 内部预览发布（Linux x86_64）。32 个 crate 全部跑过测试、
  check 0 error、两条 critical 已修复，具备内部试用条件。
- **不建议**作为稳定版对外发布。发布前至少需要：
  1. 完成 `W12b` 的 daemon CLI 与 IDE 启动接线，并决定 `KIANA_NEXT_RENDER_CORE` /
     continuity 的默认值后跑全量回归；
  2. 为 MCP stdio 注入会话沙箱、补 `BashResult` 结构化状态；
  3. 在干净、磁盘 >100 GB 的环境跑一次 `cargo test --workspace` 复核（本轮按任务分批
     执行，未跑单条 full-workspace 命令）；
  4. 真实 provider 冒烟、macOS/Windows 构建验证；
  5. 重跑 clippy 并处理 197 条既有 warning 中与新增代码相关的部分；
  6. 补齐并发会话 12 个源码改动的审计记录。
- 版本号仍为 `0.1.0` 且 workspace `publish = false`；建议以 tag `v0.1.0-alpha` 标记当前
  已验证状态，便于回滚。

## 12. 审计限制

- 未重跑 `cargo clippy`（任务未要求；FINAL 的 clippy 结果早于 deep-fix D/E）。
- 未跑单条 `cargo test --workspace`；采用任务指定的分批方式，覆盖全部 32 个 crate。
- 未接入真实模型/网络/真实 OTLP collector；相关路径只有单测/mock 覆盖。
- 未在 Windows/macOS 运行；沙箱语义依赖 Linux。
- 最终测试与另一会话的全 workspace 测试并发运行（不同 target 目录），CPU 竞争拉长了
  用时；该会话的 6 个失败是满载下的 JS watchdog flake（见 §8.4），本轮隔离批次无失败。
- 测试数字来自 cargo 日志的 `test result:` 行汇总；E2E 的 12 个测试包含在第 4 批内，
  未重复计入总数。

## 附录：日志路径

- `/tmp/kiana-audit-final-batch1.log` … `/tmp/kiana-audit-final-batch7.log`
- `/tmp/kiana-audit-final-e2e.log`
- `/tmp/kiana-audit-final-check2.log`、`/tmp/kiana-audit-final-fmt.log`、
  `/tmp/kiana-audit-final-fmtcheck2.log`
- 驱动脚本：`/tmp/kiana-audit-final-run.sh`

## 13. 2026-11-08 补完记录（W12a / W12b）

`docs/history/porting/phase-12b-completion.md` 记录了本轮对两个 shallow 周的接线：

- **W12b**：daemon `--serve` CLI（`--serve-addr`/`--serve-token`）+ bearer 鉴权 +
  `AgentSessionEvent` → `EventFeed` 桥接；IDE 启动时 `connect_for_workspace` →
  `contextUpdate`，并安装 SSE 通知接收端把 `ide/diffAccepted`/`ide/diffRejected`
  路由到进程级 `DiffBroker`；`web_fetch` 默认激活、`web_search` 按 API key 默认激活；
  cron 受管脚本通过 `kiana_sandbox::SandboxManager` 启动（后端不可用 fail-closed）。
  这些能力已有生产调用点，不再是纯库层实现。
- **W12a**：乐观用户消息队列默认开启；`RenderCoreHost` 按 settings 的 `ui.locale` /
  `language` 从 `~/.kiana/locales` 加载翻译器，待发消息文案走 i18n key。
  **`FrameWriter`/`TranscriptLedger` 仍未替换默认 TUI 写端**（`TuiMainScreen` 是
  逐字节移植，切换需要重基线 100+ golden 与光标/鼠标/备用屏生命周期），风险解释的
  真实 sideQuery 也未接。因此 **W12a 仍应视为 shallow**，W12b 的剩余项见新文件第 4 节。

## 14. 2026-11-08 发布 blocker 清理与 alpha 产物

按 `§11` 的「发布前至少需要」处理其中的第 2、3、5 项，并产出本地 alpha 产物。
本文只记录真实执行过的命令与结果；未 commit、未 tag、未 push。

### 14.1 §11 第 2 项：MCP stdio 沙箱 + `BashResult` 结构化状态（已关闭）

MCP stdio：

- `kiana-builtin-extensions` 新增进程级 `OnceLock<Arc<SandboxManager>>` 注入点
  （`install_default_mcp_sandbox` / `default_mcp_sandbox`，首次写入生效）。
- `create_default_transport_with_sandbox` 的 stdio 分支按
  `options.sandbox` > 进程级默认 > `KIANA_SANDBOX_POLICY` 解析；HTTP 分支不解析。
  沙箱后端不可用时连接失败，不回退到无沙箱 spawn。
- `kiana-coding-agent` 在创建会话运行时之前，用**信任结论已知**的 `SettingsManager`
  与 CLI flags 求有效权限模式并安装默认 manager：read-only → `read_only()`，
  default/accept-edits → `workspace_write_with_roots(cwd + --add-dir)`，
  bypass → `danger_full_access()`。`KIANA_SANDBOX_POLICY` 非空时让 env 优先；
  `KIANA_SANDBOX=none/off/disabled/false` 时不安装。
- 新增 10 个单测：4 个解析优先级 + 6 个权限模式映射；全量测试中实际执行并通过
  （`mcp::runtime::tests::{explicit_sandbox_beats_process_default_and_env,
  process_default_beats_env_and_skips_from_env,
  explicit_sandbox_is_wired_into_the_stdio_transport}` 等）。

Bash 结果：

- `kiana-ext-api::tool_result` 新增中立枚举 `BashExecutionStatus`
  （`ConfirmedExit` / `Unsandboxed` / `Unconfirmed` / `BackendUnavailable` /
  `Interrupted`，外部标签 + camelCase，不依赖 `kiana-sandbox`）。
- `BashResult` 与 `BashExecResult` 新增可选 `execution_status`，
  `#[serde(default, skip_serializing_if = "Option::is_none")]`：旧 JSON 缺键反序列化为
  `None`，`None` 时序列化与旧版逐字一致。
- 沙箱 relay 的 `ConfirmedExit` → `ConfirmedExit { code }`；非沙箱直接 spawn →
  `Unsandboxed { code, signal }`（外层 `exit_code` 保持 128+signal 的旧语义）。
- 真实执行的测试：`bash_execution_status_is_backward_compatible_and_round_trips`、
  `execution_status_is_copied_to_the_bash_result`、`kiana-tools` 的 bash/bash_sandbox/
  bash_executor 目标。

残余（不阻塞 alpha，未关闭）：

- 错误路径（`Unconfirmed` / `BackendUnavailable` / `Interrupted`）仍以 `Err` 文本
  返回，结果对象上不带结构化状态。
- 扩展 JS `user_bash` 返回对象尚未解析 `executionStatus`。
- 进程级 OnceLock 只写一次；提前返回的子命令（`kiana mcp ...`、package、config）
  不经过安装点，这些路径仍只按 `KIANA_SANDBOX_POLICY` 决定沙箱。

### 14.2 §11 第 3 项：单条全量测试（已在最终快照上关闭）

```text
cd /home/ctyun/code/kiana
CARGO_TARGET_DIR=/data/cargo-target-sandbox CARGO_INCREMENTAL=0 timeout 3600 \
  cargo test --workspace --no-fail-fast 2>&1 | tee /tmp/kiana-full-workspace.log
```

结果：退出码 0；947 条 `test result:` 行，**13166 passed / 0 failed / 401 ignored**；
0 条 `FAILED`、0 个 panic、0 条编译错误。未触发两个已知 JsEngine watchdog flake。
运行区间约 13:42–14:12。

**重要限制**：FullTest 之后，另一并发会话（`kiana-13`）继续修改了
`kiana-session` 的 `background/{durable,mod,tool}.rs` 与 `subagent/isolation.rs`
（14:31–16:04）。发布前在最终工作树上重跑同一条命令：

- 首次运行因该会话新增的 `session_id` 参数与 `durable.rs:888` / `:898` 两处测试调用
  不匹配而无法编译（2 处 E0061，退出码 101，
  `/tmp/kiana-full-workspace-final.log`）。
- 补上这两处测试调用（`Some("session-c")`，只改测试、未改库代码与断言）后重跑：947 个
  `test result` 目标，**13176 passed / 2 failed / 401 ignored**，退出码 101
  （`/tmp/kiana-full-workspace-final2.log`）。失败项：
  1. `kiana-durable::harness_storage_failure::closes_the_backend_only_after_a_paged_read_underway`
     —— 单独重跑 3 次均通过（`/tmp/kiana-durable-retry-{1,2,3}.log`），判断为高负载
     下的时序 flake；
  2. `kiana-session::continuity_wiring::startup_sweep_terminalizes_durable_records_from_a_crashed_process`
     —— 期望 `["task_crashed"]`、实际 `[]`。并发会话正在把 sweep 改成按当前会话过滤
     （`sweep_background_tasks` → `sweep_for_session(Some(self.session_id()))`），
     而测试仍用 `parent_session_id: "crashed-session"` 的旧记录；这是未完成改动的
     直接结果。
- 运行期间该会话又修改了 `subagent/isolation.rs`（16:04），因此这次结果也对应一个
  移动中的快照。

并发会话在 16:33 完成修改后，最终快照稳定；第三次重跑（17:23–17:42）：
947 个 `test result` 目标，**13187 passed / 0 failed / 401 ignored**，退出码 0
（`/tmp/kiana-full-workspace-final3.log`），运行前后源码哈希均为
`7ae82eb8db40c5cf3fda457569b778e5540e96287407b4d7b8c0fe3696791c70`。

结论：**§11 第 3 项已在最终快照上关闭**；`13166 / 0 / 401` 对应更早的快照，仅作历史
记录。

### 14.3 §11 第 5 项：clippy 重跑（已重跑，范围内清零）

```text
cargo clippy --workspace --all-targets   # 日志 /tmp/kiana-clippy-before.log
cargo clippy --workspace --all-targets   # 日志 /tmp/kiana-clippy-after.log
```

- 按 `^warning:` 行（不含 `generated N warnings` 汇总）统计：211 → 114；
  唯一 `file:line:col`：223 → 117；**0 error**。
- 最终快照（`7ae82eb8...`，含并发会话 16:33 前的改动）复核：
  `/tmp/kiana-clippy-final.log`，113 条 / 116 处 / **0 error**，与上次持平。
- 指定范围（第 5–12 周新增模块、本轮改过的文件、kiana-hooks/kiana-memory/
  kiana-sandbox、render_core、web/ide/cron 等）内 warning 清零；
  范围外既有 warning 只统计不修。
- `cargo fmt --all -- --check` 退出码 0。

### 14.4 alpha 产物

- 目录：`dist/v0.1.0-alpha/`；`kiana`、`kiana-rpc` 均为 stripped 的
  ELF 64-bit x86-64，各约 81.3 MiB（`du` 82M）；`SHA256SUMS` 已生成并在目录内校验
  通过：`kiana` = `127a48e5...f2c4e`，`kiana-rpc` = `d56d25e7...e56373`。
- 产物对应源码快照
  `7ae82eb8db40c5cf3fda457569b778e5540e96287407b4d7b8c0fe3696791c70`
  （第三次 release 构建 17:42–17:59，耗时 16m12s，开始/结束哈希一致；前两次构建因
  并发改动漂移或快照被取代而未用于发布）。该快照构建前通过了 13187/0/401 的全量测试。
- 独立复验（本机，17:59–18:00）：
  `kiana --version` → `1.4.0`；`--help` 含 `--serve` / `--serve-addr` /
  `--serve-token`；daemon 监听 `http://127.0.0.1:39847`；
  `/healthz` 无 token → 200 `ok`；`/events/latest` 无 token → 401 `unauthorized`；
  带 `Authorization: Bearer` → 200 `{"events":[],"since":0,"has_more":false}`；
  SIGINT 退出码 0。`kiana-rpc --version` → `1.4.0`。
- 发布说明：`docs/release/v0.1.0-alpha.md`。

### 14.5 仍未关闭

- `§11` 第 1 项：`W12b` 已完成（见 §13）；`W12a` 仍未替换默认 TUI 写端，保持 shallow。
- `§11` 第 4 项：只有 Linux x86_64 构建与本地 loopback 冒烟；没有真实模型 provider
  冒烟，也没有 macOS/Windows 产物。
- `§11` 第 6 项：并发会话的改动仍没有逐条审计记录；本次又新增了 14:31–16:33 的
  并发修改，尚未审计（该区间已在最终快照上跑过全量测试，但不等于逐条审计）。
- 版本：workspace 为 `0.1.0`，`kiana --version` 仍输出 `1.4.0`（`kiana-core` 的
  TS 兼容常量），本次未改。
