# 第 6 周：上下文压缩和工具输出

日期：2026-11-07
对应 `docs/history/porting/plan.md` 第 6 周（第 293-322 行）。

## 改了什么

### 1. 压缩前 tool-result pruning（`kiana-compaction`）

新增 `crates/kiana-compaction/src/pruning.rs`：

- `PruneConfig`：`protect_tokens = 40_000`、`minimum_savings = 20_000`、
  `prune_useless = true`、`prune_superseded = true`、`keep_boundary_id`、
  `protected_tools`。
- `prune_tool_outputs(entries, config) -> PruneOutcome`：纯函数，返回新的
  `Vec<SessionEntry>`，**不修改入参**。只替换 tool result 的 `content` 并清空
  `raw_message`；entry id / parentId / 顺序 / `toolCallId` 不变。
- 保护窗口：从新到旧累计 tool-output token；累计 < 40k 的结果不因年龄被 blank。
- 回收门槛：估算节省 < 20k 时 `applied = false`，entries 与入参逐项相等。
- `MIN_PRUNE_TOKENS = 50`：小于该值的结果不 blank（占位符本身有成本）。
- superseded read：`read_supersede_key` 把整文件 read 作为父键、带 offset/limit 的
  范围 read 作为子键；同 key 的新成功读取覆盖旧读取，父键的完整读取覆盖子键范围。
  占位符 `[Superseded by a newer read of this file]`。
- useless：零结果 grep（`No matches found` 等）、成功的 wait/sleep、工具
  `details.useless = true`；占位符 `[Uneventful result elided]`。useless 与 superseded
  可以穿透 40k 保护窗口（它们在任何年龄都是死重量）。
- 保护：skill 读取（`skill://`、`/skills/…/SKILL.md`）、active plan
  （`PlanRead { paths }`，忽略 `local:/` 前缀）、artifact 回读、按工具名。
- 错误结果默认不因年龄被清除（保留证据）。
- `tool_pairing_violations` / `messages_pairing_violations`：提交前自检
  tool call/result 配对。
- `enforce_aggregate_tool_result_budget(entries, max_tokens, store, config)`：
  聚合超限时从最旧结果开始清除；完整内容先落盘，占位符为
  `[Old tool result content cleared; artifact: <path>]`；没有 store 时退化为
  `[Old tool result content cleared]`。

### 2. 压缩 all-or-nothing、fallback、PTL、no-op 缓存

新增 `crates/kiana-compaction/src/pipeline.rs`：

- `HistorySnapshot`：逐条 JSON + 换行序列化，`matches()` 做逐字节比较；
  `hash()` 供 no-op 缓存。
- `run_compaction_pipeline(entries, settings, options)`：
  1. 快照原始历史；
  2. `prune_tool_outputs`（纯函数）；
  3. `prepare_compaction`；
  4. `check_split_turn_pairing`（历史侧 + turn 前缀侧）；
  5. 用第 6 周结构化模板生成摘要；
  6. LLM 失败时尝试确定性 fallback；
  7. **任何失败都返回原始 entries**（未 prune 的版本），`history_unchanged` 由
     字节快照验证。
- `PipelineOutcome`：`NothingToCompact` / `Compacted` / `Fallback` / `Failed`。
- `CompactModelOptions` 汇总 `compact` 的 12 个位置参数，便于 wire agent 传递。
- `compact_with_structured_template`：保留原 `compact` 的 split-turn 拼接格式
  （`---` + `**Turn Context (split turn):**`）与文件清单 details，只替换摘要 prompt
  和 prior summary 合并逻辑。

新增 `crates/kiana-compaction/src/fallback.rs`：

- `build_fallback_result(preparation, FallbackConfig)`：确定性（无时钟/uuid/HashMap
  迭代顺序），折叠旧 tool 活动为计数与路径列表，保留 typed user 与最近 N 条
  assistant 文本，输出 `## Objective/Details/Completed/Active/Blocked/Next/Files`。
- 只有 `summary_tokens < input_tokens` 且压到 `recovery_target_tokens` 以内才返回
  `Some`；否则返回 `None`，调用方保留原历史。
- `usage = None`（未调用模型），`details` 仍是 `CompactionDetails`。

新增 `crates/kiana-compaction/src/ptl.rs`：

- `group_messages`：开头 system 保留，其余每个 user 开启一组；组内不拆 tool pair。
- `drop_oldest_groups(messages, n)`：最多丢弃 `groups.len() - 1` 组（至少留一组）。
- `plan_ptl_retries`：最多 `MAX_PTL_RETRIES = 3` 个请求副本，每次多丢一组。
- `OverflowRecovery`：第一次 overflow → `CompactAndRetry`；第二次 → `Fail`，文本为
  `"Context overflow recovery failed after one compact-and-retry attempt. …"`。

新增 `crates/kiana-compaction/src/gate.rs`：

- `CompactionGate`：同一历史 hash 的 no-op 缓存（`NoOpCached`）；失败/ no-op 后
  `DEFAULT_COOLDOWN = 5 分钟`（`Cooldown { remaining_ms }`）；成功清空。
- `history_hash`：FNV-1a 64。

### 3. split-turn 模板与 prior summary 合并

新增 `crates/kiana-compaction/src/structured_summary.rs`：

- `STRUCTURED_SUMMARY_INSTRUCTIONS`：固定七个 section
  `Objective / Details / Completed / Active / Blocked / Next / Files`。
- `StructuredSummary::{parse,render,unfinished}`：宽容解析（`#`/`##`、大小写、
  `-`/`*`、`None`）。
- `merge_prior_summaries(previous, new)` 规则：
  - 旧 active/blocked 未被新对话提及 → carry forward；
  - 同名项在新对话处于 active/completed/blocked → 新对话优先；
  - 旧 active/blocked 在新对话 completed → 移动到 Completed，不再留在旧集合；
  - 旧 completed 被新对话重新激活 → 移动到 Active；
  - Objective 新值优先，Details 去重合并，Files 按路径去重合并。

### 4. 压缩后恢复（`kiana-compaction` + `kiana-core` + `kiana-tools`）

新增 `crates/kiana-core/src/file_read_cache.rs`：

- `FileReadSnapshot { path, content_hash, line_count, ranges, text,
  captured_at_ms, content_excerpt }`；`FileReadCache` 按路径去重、按时间排序，
  单文件 5k / 总计 50k 字符预算。
- `fingerprint_bytes` 与 `kiana-tools::read::fingerprint_bytes` 同一 FNV-1a 算法。

新增 `crates/kiana-compaction/src/recovery.rs`：

- `RecoveryBudget`：最近 5 文件、单文件 5k、文件总计 50k、技能总计 25k、单技能 5k。
- `collect_called_skills(entries, limit)`：从会话条目识别 `skill` / `skill_read` /
  `skill://` read，提取配对结果正文并按最新调用去重。
- `collect_context_deltas(entries)`：从 assistant tool call 收集工具名、agent 名与
  `mcp__<server>__<tool>` 的 server 名。
- `build_recovery_attachment(RecoveryInput, budget) -> RecoveryAttachment`：
  - 文本包在 `<post-compaction-recovery>` 中，明确写“这不是新的 tool result”；
  - 文件去重后取最近 5 个，超预算时省略 excerpt 并 warning；
  - 技能去重后按时间取，受总/单技能预算限制；
  - plan 内容与 plan mode/approved 状态；缺失内容只 warning；
  - `ContextDeltas` 输出 tools/agents/mcp_servers；
  - `restored_files` 携带 `content_hash` / ranges / text，供 wire agent 调用
    `ReadStateRegistry::restore_snapshot`；
  - `hidden = true`。

`crates/kiana-tools/src/read.rs`：

- `ReadRecord.captured_at_ms`（用于最近排序）。
- `ReadStateRegistry::snapshots(limit) -> Vec<FileReadSnapshot>`：压缩前导出。
- `ReadStateRegistry::restore_snapshot(&FileReadSnapshot) -> RestoreOutcome`：
  只有磁盘指纹与快照一致才恢复；`Restored` / `Missing` / `Changed` /
  `Unverifiable`。未恢复路径的 edit/write 继续被 read gate 拒绝。

### 5. 工具输出预算、artifact store、JSON outline

新增 `crates/kiana-core/src/artifact_store.rs`：

- `ArtifactStore::new(base_dir, session_id)`：会话级目录。
- 文件 `<08id>.<sanitized-tool>.log`；Unix 目录 0700、文件 0600；临时文件 + 字节数
  校验 + 原子 rename；`create_new` 永不覆盖。
- id / session id / 工具名校验：拒绝空串、`.`、`..`、`/`、`\`、绝对路径、NUL；
  词法归一化后必须位于 root 内。
- `save` / `save_with_id` / `path_for_id` / `resolve("artifact://<id>")` / `read` /
  `read_range_bytes` / `read_range` / `list` / `remove`。
- `redact_secrets`：PEM 私钥、AWS access key、OpenAI `sk-`、GitHub `ghp_`、Slack
  `xox*`、`Bearer <token>`、`password|secret|token|api_key = …` 全部替换为
  `[REDACTED:<kind>]`，secret 不落盘。
- handle 是稳定的 `artifact://<id>`，压缩只改消息内容，不删目录，跨压缩可解析到
  同一文件。

新增 `crates/kiana-core/src/output_budget.rs`：

- 每工具上限：Read 无上限、Edit/Write/ApplyPatch/MultiEdit 100k、Bash/PowerShell/
  Grep 30k、默认 50k（字符）。
- `SPILL_THRESHOLD_CHARS = 45_000`、`MIDDLE_OUT_PREVIEW_CHARS = 40_000`
  （head/tail 各半，中间一行省略说明）。
- `apply_output_budget(tool_name, content, store) -> BudgetedOutput`：
  - 未超预算原样返回；
  - 超落盘阈值（且有 store）→ 完整内容落盘 + `<persisted-output>` 包装；
  - 只是超过工具上限 → middle-out 截断；有 store 时优先落盘避免丢数据；
  - 落盘失败 → 截断并记录 `artifact_error`；
  - JSON 输出（≤2MB）额外附 `JSON outline (path kind offset length)`。

新增 `crates/kiana-core/src/json_outline.rs`：

- 递归扫描器（不依赖 serde 反序列化后的偏移），为每个值记录 JSONPath、类型、
  **字节 offset/length**；容器条目覆盖整个子树。
- `max_depth` / `max_entries` / `max_children` 限制输出规模；超限仍保证已生成条目
  是合法子串。
- `OutlineEntry::slice` / `is_valid_json`；`JsonOutline::render`。

`crates/kiana-tools/src/definition.rs`：

- `OutputBudgetOptions { store: Arc<ArtifactStore> }`。
- `wrap_tool_definition_with_output_budget(definition, ctx_factory, options)`：
  包装后的 `AgentTool` 在每次执行后对**纯文本**结果应用预算；图片结果原样返回；
  落盘时把 `{id,uri,path,bytes,redacted}` 写入 `details.artifact`。
- `kiana-tools/src/index.rs` 再导出该适配器与 `RestoreOutcome`。

### 6. 压缩提交事务（`kiana-session-core`）

新增 `crates/kiana-session-core/src/compaction_support.rs`：

- `HistorySnapshot::capture/matches/bytes`：提交前字节级快照。
- `CommitTransaction::begin/record/rollback`：把 prune 的 context_edit 与压缩条目
  作为一个整体回滚；底层新增 `SessionManager::append_transaction_marker` /
  `rollback_append_transaction` / `appended_entry_id` / `remove_appended_entry`。
- `append_compaction_transactional(manager, snapshot, …)`：
  - 历史被并发修改 → `CompactionCommitError::ConcurrentModification`，不追加；
  - 追加失败 → 回滚内存（`file_entries` / `by_id` / leaf），返回原始错误；
  - 成功 → 恰好新增一个 compaction 条目。
- 边界：磁盘上已写出的半行无法安全回滚（进程崩溃/IO 错误）；本模块承诺内存状态
  与提交前一致，正常路径整行一次 `write_all`。

## 参考项目 → 借鉴点 → Kiana 落点

| 参考项目文件 | 借鉴的设计点 | Kiana 文件/函数 |
| --- | --- | --- |
| `oh-my-pi/packages/agent/src/compaction/pruning.ts` | `protectTokens=40k`、`minimumSavings=20k`、`MIN_PRUNE_TOKENS=50`、superseded/useless 穿透保护窗口、原地 blank、精确 placeholder | `kiana-compaction/src/pruning.rs`：`PruneConfig`、`prune_tool_outputs`、`read_supersede_key`、`classify_useless` |
| `oh-my-pi/packages/agent/src/compaction/tool-protection.ts` | tool call 配对索引、`skill://` 读取保护、active plan 保护、artifact 回读保护 | `pruning.rs`：`collect_tool_calls`、`is_protected_result`、`ProtectedToolMatcher` |
| `oh-my-pi/packages/coding-agent/src/plan-mode/plan-protection.ts` | `local://PLAN.md` 别名与 session plan 路径的 read 保护 | `pruning.rs`：`PlanRead` + `normalize_plan_path` |
| `oh-my-pi/packages/coding-agent/src/tools/read-supersede.ts` | 完整读取才能 supersede 范围读取 | `pruning.rs`：`is_complete_read_result` |
| `oh-my-pi/packages/coding-agent/src/session/artifacts.ts` | session 级 artifact 目录、工具名清洗、临时文件 + 原子 rename、`artifact://<id>` | `kiana-core/src/artifact_store.rs`：`ArtifactStore`、`sanitize_tool_name`、`write_private_file` |
| `oh-my-pi/packages/coding-agent/src/tools/settings.ts` / `output-meta.ts` | artifact spill threshold、head/tail middle elision、行边界切分 | `kiana-core/src/output_budget.rs`：`SPILL_THRESHOLD_CHARS`、`MIDDLE_OUT_PREVIEW_CHARS`、`middle_out_preview` |
| `oh-my-pi/packages/coding-agent/src/tools/read.ts` | 大 JSON 先给结构/路径再按区间读取 | `kiana-core/src/json_outline.rs`：`structural_outline`、`OutlineEntry` |
| `oh-my-pi/packages/coding-agent/src/session/session-maintenance.ts` | no-op 后 rearm / 不反复付摘要成本、prune 不足则返回输入 | `kiana-compaction/src/gate.rs`：`CompactionGate`；`pruning.rs`：`minimum_savings` |
| `ClaudeCode/src/services/compact/microCompact.ts` | `[Old tool result content cleared]`、只改 content 不删消息、compactable tool 集合 | `pruning.rs`：`enforce_aggregate_tool_result_budget`、`CLEARED_NOTICE` |
| `ClaudeCode/src/utils/toolResultStorage.ts` | 大结果落盘 + `<persisted-output>` 预览、按 tool_use_id 幂等、预览换行边界 | `kiana-core/src/output_budget.rs`、`artifact_store.rs` |
| `ClaudeCode/src/constants/toolLimits.ts` | 默认 50k 字符、Read `Infinity`、按工具声明上限 | `output_budget.rs`：`ToolOutputPolicy::for_tool` |
| `ClaudeCode/src/utils/fileReadCache.ts` + `postCompactCleanup.ts` | 压缩后恢复最近文件读状态、未恢复路径由 read gate 拒绝 | `kiana-core/src/file_read_cache.rs`、`kiana-tools/src/read.rs`、`kiana-compaction/src/recovery.rs` |
| `ClaudeCode/src/services/compact/prompt.ts` | objective/details/completed/active/blocked/next/files 结构化模板 | `kiana-compaction/src/structured_summary.rs`：`STRUCTURED_SUMMARY_INSTRUCTIONS` |
| `ClaudeCode/src/services/compact/autoCompact.ts` | compact-and-retry 一次、第二次明确报错 | `kiana-compaction/src/ptl.rs`：`OverflowRecovery`（`AgentSession` 已有同名状态机，handoff 说明复用） |
| `qwen-code/packages/core/src/services/microcompaction/microcompact.ts` | 只清除旧 tool 结果、保留最近结果、`[Old tool result content cleared]`、小结果不 blank | `kiana-compaction/src/pruning.rs` |
| `qwen-code/packages/core/src/services/fileReadCache.integration.test.ts` | 恢复快照而不是伪造 tool result | `kiana-compaction/src/recovery.rs`（hidden context）+ `read.rs`（restore） |
| `cline/apps/vscode/src/sdk/sdk-compaction.ts` | 压缩只影响工作上下文（sidecar），不替换 canonical transcript | `kiana-session-core/src/compaction_support.rs`（快照/事务）+ `pipeline.rs`（失败保留原历史） |
| `cline/sdk/examples/plugins/custom-compaction.ts` | 保留首个 user + 最近上下文、旧中间历史合并成一条 continuation summary | `kiana-compaction/src/fallback.rs` |

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

```text
$ cargo test -p kiana-compaction --test pruning
cargo test: 6 passed (1 suite, 0.02s)

$ cargo test -p kiana-compaction --test pipeline
cargo test: 5 passed (1 suite, 0.09s)

$ cargo test -p kiana-compaction
cargo test: 159 passed, 2 ignored (11 suites, 1.42s)

$ cargo test -p kiana-compaction --lib
cargo test: 43 passed (1 suite, 0.01s)

$ cargo test -p kiana-core --lib artifact_store
cargo test: 7 passed, 91 filtered out (1 suite, 0.02s)

$ cargo test -p kiana-core --lib output_budget
cargo test: 8 passed, 90 filtered out (1 suite, 1.89s)

$ cargo test -p kiana-core --lib json_outline
cargo test: 5 passed, 93 filtered out (1 suite, 0.06s)

$ cargo test -p kiana-core --lib file_read_cache
cargo test: 4 passed, 94 filtered out (1 suite, 0.00s)

$ cargo test -p kiana-session-core --lib compaction_support
cargo test: 5 passed, 3 filtered out (1 suite, 0.00s)

$ cargo test -p kiana-tools --test output_budget
cargo test: 3 passed (1 suite, 1.92s)

$ cargo test -p kiana-tools --test read_gate_after_compaction
cargo test: 3 passed (1 suite, 0.00s)

$ cargo test -p kiana-tools
cargo test: 435 passed (29 suites, 8.23s)

$ cargo test -p kiana-session-core
cargo test: 255 passed, 1 ignored (29 suites, 23.61s)

$ cargo test -p kiana-core
cargo test: 677 passed, 1 ignored (53 suites, 28.79s)

$ cargo check -p kiana-compaction -p kiana-core -p kiana-session-core -p kiana-tools --tests
Finished `dev` profile ... in 1m 07s

$ cargo fmt -p kiana-compaction -p kiana-core -p kiana-session-core -p kiana-tools -- --check
（无输出）
```

## 未完成 / partial

- **pruning 持久化**：`SessionManager` 是 append-only。本单元返回纯函数结果；
  wire agent 需要用 `append_context_edit(target_id, Some(replacement))` 把 blank
  写进投影，并用 `CommitTransaction` 与压缩条目一起回滚。见 handoff。
- **read state 恢复接线**：`ReadStateRegistry::snapshots/restore_snapshot` 已实现并
  测试；`AgentSession` 在压缩前后的调用顺序由 handoff 给出，本单元未改
  `crates/kiana-session/**`。
- **PTL 头部丢弃接线**：`plan_ptl_retries` 是纯函数；`AgentSession` 现有的
  compact-and-retry 状态机未改为使用它（该文件不在所有权内）。handoff 说明可选接入点。
- **no-op 缓存/冷却**：`CompactionGate` 已实现，但状态需要跨压缩调用持有，由
  `AgentSession` 持有并在 handoff 指定。
- **工具输出预算接线**：`wrap_tool_definition_with_output_budget` 已实现，但
  `AgentSession::build_runtime` / `kiana-extensions` 的包装点不在所有权内；当前工具
  定义仍走无预算的 `wrap_tool_definition`。handoff 给出替换方式。
- **provider overflow 第二次报错**：`AgentSession` 已有实现（`overflow_recovery_attempted`
  与明确错误文本），本单元只补了可复用的 `OverflowRecovery` + 测试，未改 session。
- **磁盘半行回滚**：`append_compaction_transactional` 只保证内存一致；写盘中途失败
  可能留下残行（正常整行 write_all，风险低，已在模块文档声明）。
- **JSON outline 只在落盘时附带**：未落盘的中等 JSON 不生成 outline（避免为每条
  工具结果付解析成本）。

## 风险

- **token 估算误差**：pruning 用 `estimate_tokens`（字符/4）；provider 的真实
  token 数可能不同。保护窗口与 20k 门槛是启发式，极端情况下可能多删或少删。
- **误删证据**：年龄裁剪只跳过错误结果与小结果，但成功的大结果可能就是关键证据。
  保护窗口 40k 与最小回收 20k 是主要缓解。
- **secret redaction 误伤**：`credential_assignment` 正则会替换普通文本里形如
  `token: abcdef` 的内容；artifact 回读得到的是 redaction 后的文本，不再是原始
  字节。
- **prompt cache**：pruning 改写旧内容会破坏 provider 前缀缓存；本实现没有
  cache-warm suffix / lookback 保护（参考实现有），大量 prune 可能导致一次昂贵的
  cacheWrite。
- **恢复破坏 prefix cache**：hidden context attachment 追加在末尾，不影响旧前缀；
  但如果每次压缩都重建，仍可能增加尾部 token。
- **事务边界**：`CommitTransaction` 只回滚内存；磁盘半行与并发写会话文件不在保护
  范围内。
- **结构化模板未被旧 `compact` 使用**：`compact` 保持 TS 冻结 prompt；新流水线
  使用 `compact_with_structured_template`。调用方如果直接调用旧 `compact`，不会得到
  objective/details/... 模板。
