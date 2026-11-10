# E2E — 跨模块端到端验收

日期：2026-11-08
工作目录：`/home/ctyun/code/kiana`
构建目录：`CARGO_TARGET_DIR=/data/cargo-target-sandbox`、`CARGO_INCREMENTAL=0`
测试文件：`crates/kiana-session/tests/porting_e2e.rs`
状态：**done**

本文件记录「按周并行移植」完成后的端到端验收：覆盖了哪些真实调用链、哪些没有覆盖
及原因、实际执行的命令与输出、以及是否修改了生产代码。验收不改实现、不 mock 核心
路径；测试全部通过，**没有修改任何生产代码**。

---

## 1. 结论

- 新增 1 个集成测试文件、12 个测试；覆盖任务要求的全部 7 类场景。
- 使用真实公开 API：
  - `AgentSession`（权限、`execute_bash`、默认 `read`/`edit`/`bash` 工具表、hooks、
    子代理）；
  - `kiana-sandbox` 的真实 bubblewrap 后端与统一执行策略；
  - `kiana-compaction` 的 pruning/聚合预算 + `kiana-core` 的 `ArtifactStore`；
  - `kiana-memory` 的 `MemoryService`/`MemoryStore`/`RecallSession`/`forget`。
- 真实命令结果：
  - `cargo test --test porting_e2e`（任务指定命令，从仓库根执行）：
    **12 passed; 0 failed; 0 ignored**，退出码 0。
  - `cargo check --workspace`：`Finished`，退出码 0，日志中 0 warning / 0 error。
- 本机 `bubblewrap 0.6.1`（`/usr/bin/bwrap`）可用；真实 bwrap 分支实际执行，
  没有走「跳过」分支（串行 + `--nocapture` 运行也没有打印跳过原因）。
- 未修改 `reference_project/**`、`docs/history/porting/plan.md`；未 commit；未做重构。

---

## 2. 为什么放在 `kiana-session` 而不是 `kiana-coding-agent`

任务优先要求 `crates/kiana-coding-agent/tests/porting_e2e.rs`，但依赖不合适：

- 验收项需要 `kiana-sandbox`、`kiana-hooks`、`kiana-memory` 与
  `AgentSession` 的完整测试夹具（`crates/kiana-session/tests/common/mod.rs`，
  包含 JsEngine、faux provider、`ScriptedUi`、事件订阅等）。
- `kiana-coding-agent` 的 `[dependencies]` 没有 `kiana-sandbox`/`kiana-hooks`/
  `kiana-memory`，也没有这套夹具。放在那里需要新增三个 dev-dependency 并复制
  整套夹具（约 1100 行），属于为测试扩大依赖面，且会绕开共享测试夹具。
- `kiana-session` 本身就是权限、沙箱、hooks、压缩、memory、子代理的接线层；
  用它的真实公开 API 能把「模块自己的单元测试」提升为「会话调用链上的测试」，
  符合任务目标。

因此按任务给出的回退方案放在
`crates/kiana-session/tests/porting_e2e.rs`。文件名和测试目标名保持
`porting_e2e`，任务指定的 `cargo test --test porting_e2e` 在仓库根可直接运行。

---

## 3. 场景覆盖

| # | 验收项 | 测试 | 真实调用链与断言 |
|---|---|---|---|
| 1 | 权限：read-only / default / bypass | `permission_modes_are_enforced_on_the_real_execute_bash_path` | 通过 settings 构造 `PermissionEngine`，调 `AgentSession::execute_bash`。read-only 返回含 `read-only` 的错误；default 无 UI 返回含 `No interactive UI` 的错误（fail-closed，不挂起）；bypass 真正执行并回读 stdout `e2e-bypass`。 |
| 2 | AllowOnce 不持久；Session 持久 | `allow_once_does_not_persist_and_session_scope_only_lasts_for_the_session` | 用 `ScriptedUi` 走完整审批：AllowOnce 后第二次相同调用再次询问并可 Reject（原因回传），`local_settings_path` 不存在 permissions，模拟重启后的 `PermissionEngine` 仍 `Ask`；AllowAlways{Session} 第二次不再询问（select 次数保持 2），同样不写 local settings，重启后仍 `Ask`。 |
| 3 | 沙箱：文件工具工作区外写入被拒 | `workspace_write_file_tool_rejects_writes_outside_the_workspace` | 用显式 `ExecutionPolicy::new(SandboxPolicy::workspace_write(root), root)` 构造真实 `write` 工具（不是 mock operations），写工作区外返回 `OUTSIDE_WORKSPACE` 且目标文件不存在；同一工具写工作区内成功，排除「工具本身坏了」。 |
| 4 | 沙箱：真实 bwrap / 后端不可用 fail-closed | `workspace_write_shell_uses_real_bubblewrap_or_reports_backend_unavailable` | `SandboxManager::with_type(LinuxBubblewrap, workspace-write)`：可用时真实 `spawn_relayed`，工作区外写入得到非零 `ConfirmedExit`、marker 不存在、stderr 非空；同一 manager 写工作区内 exit 0 且内容正确。不可用时断言 `SandboxError::Unavailable/Unsupported` → `ExecutionStatus::BackendUnavailable`、`!may_have_executed()`、命令未执行（不假装跑过）。 |
| 5 | 沙箱：非法策略 fail-closed | `invalid_sandbox_policy_fails_closed_without_running_the_command` + `invalid_policy_sandbox_worker` | 在独立子进程里设置 `KIANA_SANDBOX_POLICY=not-a-real-policy`；`SandboxManager::from_env` 产生 fail-closed manager，`spawn_relayed` 返回 `Unavailable`，`BackendUnavailable`、`!may_have_executed()`、marker 不存在。父测试断言 worker 确实执行，避免「过滤器没匹配、0 个测试也返回成功」的假通过。 |
| 6 | 读后写：未读 edit 被拒 | `edit_requires_a_fresh_read_and_detects_external_modification` | 走 `AgentSession` 默认工具表：模型发起 `edit`（未 read）→ 工具结果 `is_error` 且含 `ReadBeforeEdit`，文件字节不变。 |
| 7 | 读后写：外部修改检测 | 同上 | 模型发起 `read`（记录会话级 `ReadStateRegistry`）→ 外部进程改写文件 → 再 `edit` → 结果含 `FileChangedSinceRead` 与 `FILE_CHANGED_SINCE_READ`，外部内容未被覆盖。 |
| 8 | hooks：PreToolUse exit 2 | `pre_tool_use_exit_two_blocks_the_tool_before_execution` | settings 配置 command hook；模型 `bash` 调用被 hook exit 2 阻止：工具结果 `is_error`、stderr 只出现一次、`touch` 目标不存在、审计记录 `HookAuditOutcome::Blocked`。 |
| 9 | hooks：updatedInput 先于权限 | `pre_tool_use_updated_input_is_seen_by_the_permission_layer` | hook 把 `printf original` 整体改写为 `printf denied`；权限层的 deny 规则只匹配改写后的命令，工具结果含规则 reason `denied after hook rewrite`，证明权限判断看到的是 hook 后的参数。 |
| 10 | 压缩：pruning 后配对合法 | `pruning_keeps_tool_pairs_valid_and_artifacts_are_offset_readable` | 用 `kiana_compaction::pruning::prune_tool_outputs`（`AgentSession::prepare_with_pruning` 使用的同一函数）处理 20 组 read call/result：`applied`、回收 ≥20k token、entry id/顺序不变、每个 result 的 `tool_call_id` 与 call 一一对应、`tool_pairing_violations` 为空；最旧结果变成精确 `[Output truncated ...]` 占位符，最新结果完整。 |
| 11 | 压缩：artifact offset 回读 | 同上 | `enforce_aggregate_tool_result_budget(..., Some(&ArtifactStore), ...)` 把完整内容落盘；从占位符 `artifact: <path>` 取路径与 id，`read_range(id, offset, length)` 与原始内容的字节区间完全一致；占位符替换后 tool pair 仍合法。 |
| 12 | memory：写入 → 召回 → forget；secret 不落盘 | `memory_write_recall_forget_keeps_secrets_off_disk` | `MemoryService::trusted` 写入含 `password=...` 与 `sk-...` 的条目（落盘前脱敏），`report.redactions >= 2`；递归扫描整个 scope（索引/主题/审计/状态）确认 secret 不存在；`RecallSession` 固定选择器召回 → 一条 `<memory-context>`，内容含 `[REDACTED` 与条目标题、不含 secret；`forget` 删除条目并写只含 digest 的墓碑，扫描仍无 secret；再次召回无消息。 |
| 13 | 子代理：父权限收窄 | `narrowed_parent_permissions_are_enforced_for_subagents` | 父会话 `set_permission_mode(ReadOnly)` 后 `prepare_subagent`：子代理 `dispatch_tool("bash")` / `("write")` 返回 `SubagentError::PermissionDenied`（reason 含 `read-only`），`read` 仍执行并返回文件内容；切回 bypass 并追加父 deny 规则后，新子代理调用 `bash` 返回含 `parent policy denies bash` 的拒绝。 |

测试数量：12 个（上表 #5 含 2 个测试函数；#6/#7、#10/#11 各由同一个测试覆盖两个断言面）。

补充说明：

- memory 行使用 `MemoryService`/`MemoryStore`/`RecallSession`，也就是会话 `memory`
  工具实际调用的同一套服务层；session 级别的索引注入与召回接线另由既有
  `memory_wiring_integration`（本文件 §4.2 一并回归）覆盖。这样既不重复会话夹具，
  又保证写入/召回/forget/脱敏走真实的存储与召回实现。
- 压缩行的 `prune_tool_outputs` 是 `AgentSession` 自动/手动压缩路径中
  `prepare_with_pruning` 调用的同一函数；artifact 行使用的
  `enforce_aggregate_tool_result_budget` 也是会话每次 provider 请求前
  （`prepare_request`）执行的同一聚合预算函数。

---

## 4. 真实命令与输出

全部在 `/home/ctyun/code/kiana` 下执行，统一使用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox CARGO_INCREMENTAL=0`。

### 4.1 任务指定测试命令

```text
$ timeout 900 cargo test --test porting_e2e
   Finished `test` profile [unoptimized] target(s) in 3.29s
   Running tests/porting_e2e.rs (.../deps/porting_e2e-...)
running 12 tests
test allow_once_does_not_persist_and_session_scope_only_lasts_for_the_session ... ok
test edit_requires_a_fresh_read_and_detects_external_modification ... ok
test invalid_policy_sandbox_worker ... ok
test invalid_sandbox_policy_fails_closed_without_running_the_command ... ok
test memory_write_recall_forget_keeps_secrets_off_disk ... ok
test narrowed_parent_permissions_are_enforced_for_subagents ... ok
test permission_modes_are_enforced_on_the_real_execute_bash_path ... ok
test pre_tool_use_exit_two_blocks_the_tool_before_execution ... ok
test pre_tool_use_updated_input_is_seen_by_the_permission_layer ... ok
test pruning_keeps_tool_pairs_valid_and_artifacts_are_offset_readable ... ok
test workspace_write_file_tool_rejects_writes_outside_the_workspace ... ok
test workspace_write_shell_uses_real_bubblewrap_or_reports_backend_unavailable ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.86s
```

同一组用例在默认并行模式下也通过：

```text
$ timeout 900 cargo test -p kiana-session --test porting_e2e
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.25s
```

说明：

- 实际串行 + `--nocapture` 运行没有出现
  `real bwrap assertion skipped: ...`，即第 4 项走了真实 bubblewrap 分支；
  本机 `/usr/bin/bwrap` 版本为 `bubblewrap 0.6.1`。
- `invalid_policy_sandbox_worker` 在普通运行中立即返回（没有子进程环境变量），
  只在 `invalid_sandbox_policy_fails_closed_without_running_the_command` 子进程里
  真正执行；父测试通过 stdout 标记确认它执行过。

### 4.2 相关既有套件回归

为避免新测试与既有接线测试互相污染，另跑了相邻套件：

```text
$ timeout 900 cargo test -p kiana-session \
    --test porting_e2e \
    --test permissions_integration \
    --test hooks_wiring \
    --test memory_wiring_integration \
    --test subagent_runtime
hooks_wiring:              14 passed; 0 failed
memory_wiring_integration:  2 passed; 0 failed
permissions_integration:   17 passed; 0 failed
porting_e2e:               12 passed; 0 failed
subagent_runtime:            9 passed; 0 failed
（全部退出码 0）
```

### 4.3 工作区检查

```text
$ timeout 900 cargo check --workspace
    Checking kiana-rpc-print v0.1.0 (...)
    Checking kiana-interactive v0.1.0 (...)
    Checking kiana-coding-agent v0.1.0 (...)
    Checking kiana-evals v0.1.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 11s
```

日志中 `warning` / `error` 计数均为 0，退出码 0。

### 4.4 格式化

```text
$ cargo fmt -p kiana-session -- --check
（无输出，退出码 0）
```

---

## 5. 未覆盖的场景与原因

以下不是遗漏的验收项，而是本文件明确不覆盖、并有既有测试或其他原因的部分：

1. **完整 LLM 压缩管线（摘要生成 / split-turn / fallback）**：本测试覆盖的是
   `prepare_with_pruning` 实际调用的确定性 pruning 函数和 artifact 契约；摘要生成
   需要模型响应与接近上下文窗口的历史，属于 `kiana-compaction`/`kiana-session`
   既有套件的范围（`pipeline`、`agent_session_compaction*`）。本文件不重新
   构造一个巨大的 faux 会话，以免把「prune 配对」验证淹没在无关的 LLM 编排里。
2. **TUI 审批渲染**：`ScriptedUi` 只实现 `select`/`input` 契约，验证的是权限决策与
   持久化边界，不是真实终端渲染；审批卡的渲染属于 `kiana-tui`/`kiana-interactive`
   自己的测试。
3. **HTTP hooks 与 async hooks**：本文件只覆盖任务要求的 command hook 的
   PreToolUse 退出码/updatedInput 契约；HTTP/SSRF/async 在 `kiana-hooks` 的
   `http_hooks` 等套件里。
4. **子代理工作区隔离（worktree/overlayfs）**：本文件用 `IsolationMode::Shared`
   验证权限继承；隔离需要 git/root，属于第 11 周的独立套件。
5. **后台记忆抽取 job（SessionSource/Phase1/Phase2）**：任务要求的是
   「写入→召回→forget」；后台抽取在 FINAL 中仍是已声明缺口，且有
   `kiana-memory/tests/extraction.rs` 覆盖两阶段脱敏。
6. **MCP 子进程沙箱、PowerShell 执行路径**：共享同一 `ExecutionPolicy`，但需要
   MCP server/PowerShell 环境；既有 `kiana-tools/tests/execution_policy_gate.rs`
   覆盖了命名一致性，本文件不重复。
7. **Windows/macOS**：沙箱后端是 Linux 专有（bwrap/Landlock）；本机 Linux，
   Windows/macOS 无法真实验证。测试在非 Linux 上会把 bwrap 构造失败映射为
   `BackendUnavailable`（分支已写），但未在那些平台运行。
8. **进程级环境变量优先级（`KIANA_PERMISSIONS` 等）**：会污染并行测试；本文件
   只在独立子进程中改 `KIANA_SANDBOX_POLICY`，权限环境变量仍由既有测试覆盖。
9. **AllowAlways{Local|Project} 的磁盘写回**：已由
   `permissions_integration` 覆盖；本文件刻意聚焦 AllowOnce 不持久与 Session
   只在会话内生效这两个**不落盘**的边界，避免重复。
10. **读后写的「同尺寸同 mtime、仅内容哈希变化」**：本文件用外部改写（size 变化）
    走真实与会话工具链；同尺寸/同 mtime 的哈希检测在
    `kiana-tools/tests/read_before_write.rs` 已有专门用例。

---

## 6. 生产代码修复

**无。** 12 个测试在第一次运行即全部通过：

- 没有出现「实现断了」的失败，因此没有修改
  `crates/*/src/**` 的任何生产代码。
- 没有为了通过而放宽断言、删除测试或添加 `#[ignore]`。
- 测试自身在验收过程中补强了两处**防假阳性**（不是生产修复）：
  1. 非法策略 worker 打印 `KIANA_E2E_INVALID_POLICY_WORKER_RAN`，父测试断言该标记，
     防止 `--exact` 过滤器未命中时「0 个测试也成功」；
  2. pruning 断言从「内容与原文不同」收紧为「以精确占位符 `[Output truncated` 开头」，
     防止任意改写被误判为 pruning 成功。

---

## 7. 风险与剩余说明

- `workspace_write_shell_uses_real_bubblewrap_or_reports_backend_unavailable`
  在无 bwrap 的宿主只验证 `BackendUnavailable` 契约；真实隔离行为需要 Linux +
  可用 bwrap。这是任务允许的显式分支，测试不会在无后端时假装执行。
- 沙箱测试创建的临时目录在 `CARGO_TARGET_TMPDIR` 下（位于 target 目录），
  不占用仓库工作区；bwrap 的 `--ro-bind / /` 使这些 target 目录中的「工作区外」
  路径在该命名空间内只读，因此能真实观察 `EROFS`。
- 权限测试依赖 `ScriptedUi` 的选择文本（`Allow once` / `Always allow` /
  `This session` / `Reject`）；这些是与当前审批 UI 的稳定契约，若产品文案改变，
  测试会显式失败而不是静默跳过。
- memory secret 扫描是递归读取整个 scope 的所有文件；扫描不忽略点文件/二进制，
  因此审计 JSONL、墓碑、索引、状态文件都被覆盖。
