# Week 2 — 所有执行路径都走沙箱 + 读后写

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 156-188 行。目标：bash / PowerShell / `execute_bash` /
MCP 子进程 / 文件写入共用同一个执行策略；沙箱结果区分「确定退出 / 未确认 / 后端不可用 /
中断」；write/edit 在 `file_mutation_queue` 锁内做路径门禁与 read-before-write。

## 改了什么

### 1. 统一执行策略（`kiana-sandbox`）

`crates/kiana-sandbox/src/execution_policy.rs`

- `ExecutionPolicy::resolve(tool_name, input) -> SandboxDecision` 覆盖
  `bash` / `powershell` / `execute_bash` / `mcp` / `mcp_stdio` / `write` / `edit` /
  `apply_patch` / `read`；未知工具、工具名与输入形状不匹配一律 `Deny`（fail-closed）。
- 文件写入门禁 `check_write_path`：realpath（含**悬空/嵌套符号链接**）、可写根、
  `.git`/`.kiana` 保护、大小写不敏感平台比较。
- `resolve_real_path` 从「canonicalize 最近祖先 + 词法拼接」升级为「逐组件展开符号
  链接」：最终组件是悬空链接时也会解析目标，堵住「工作区内链接指向工作区外新文件」
  的绕过；符号链接环返回 `ELOOP`（上限 40 跳），调用方 fail-closed。
- `SandboxManager::execution_policy()`：把 manager 的策略与 cwd 投影成同一份
  `ExecutionPolicy`，shell 与文件工具不再各自解释策略。

### 2. 结果状态与 relay

`crates/kiana-sandbox/src/execution_policy.rs`

- `ExecutionStatus::{ConfirmedExit{code,signal}, Unconfirmed, BackendUnavailable,
  Interrupted}`，新增：
  - `may_have_executed()`：只有 `Unconfirmed` 为真；
  - `allows_automatic_retry()`：只有 `BackendUnavailable` 为真；
  - `tool_label()` / `tool_message()`：CamelCase 状态名 + 面向模型的提示。
- `SandboxManager::spawn_relayed` 的 relay 语义（已有实现，本周接入工具）：
  - 后端不可用 → `SandboxError::Unavailable`，命令**没有**执行；
  - relay 写 `started` 后被外部 kill → `Unconfirmed`，提示「可能已经执行，不要自动
    重试」；
  - 只有 `started + exit` 都有才是 `ConfirmedExit`。
- `kiana-tools/src/bash.rs` 的本地执行器改为：沙箱路径一律
  `spawn_relayed`，退出码只信 `ConfirmedExit`；`BackendUnavailable` / `Unconfirmed`
  作为工具错误上报；超时/中止在 relay 已启动命令时输出 `timeout:<secs>|unconfirmed`
  / `aborted|unconfirmed`，`ShellExecute` 在保留旧文案的同时追加
  `UNCONFIRMED_COMMAND_NOTE`（「命令可能已经执行；不要自动重试」）。
- 非沙箱路径保持原来的直接 spawn 与 `128 + signal` 语义。

### 3. 文件写入门禁

- `write`/`edit` 的新选项：
  - `execution_policy: Option<Arc<ExecutionPolicy>>`：会话可传入合并了权限
    `addDirs`/多根工作区的策略；缺省按执行期 cwd 从 `KIANA_SANDBOX_POLICY` 构造。
  - `read_state: Option<Arc<ReadStateRegistry>>`：缺省使用进程级共享表（与 read 工具
    一致）。
- 门禁在 `try_with_file_mutation_queue` 的闭包内执行（锁内），顺序：
  1. `ExecutionPolicy::resolve` + `check_write_path`（realpath/可写根/受保护路径）；
  2. read-before-write（仅已存在的普通文件）；
  3. `edit` 额外检查截断读的行覆盖范围。
- `write`/`edit` 读写都使用门禁返回的真实路径；`mkdir` 也从真实路径推导父目录。
- 新错误（消息里同时带计划里的两种写法，便于匹配）：
  - `ReadBeforeEdit: ...`（没读/只读了部分就整文件覆盖）；
  - `FileChangedSinceRead (FILE_CHANGED_SINCE_READ): ...`；
  - `READ_RANGE_NOT_SEEN: ...`（截断读修改未看到行）；
  - `READ_NOT_TEXT`、`PRIOR_READ_VERIFICATION_FAILED`（非文本 / 无法验证时 fail-closed）。

### 4. read-before-write 记录

`crates/kiana-tools/src/read.rs`

- `ReadStateRegistry`（会话级；生产默认进程级共享，测试可独立实例化）：
  - 身份：`(dev, ino)`；`ino == 0`（FAT/exFAT/SMB 等）不记录，写入门禁随后
    fail-closed（`PRIOR_READ_VERIFICATION_FAILED`），不猜。
  - 记录：`real_path`、`mtime`、`size`、内容 FNV-1a 64 哈希、按 `\n` 计的行数、
    `text` 标记、模型实际看到过的 1-based 行范围。
  - 读取成功后写入；同一份字节的重复读取会合并行范围（sticky），指纹变化时重置。
- read 工具在两个分支记录：
  - 文本：只记录 `offset`/`limit` 选中且未被 `truncate_head` 截掉的行；首行超限时
    范围为空；
  - 图片：`text=false`（可继续 read，但不能当文本 edit/write）。
- 写入门禁检查：
  - `Fresh` 且覆盖全文件才允许 `write`；
  - `edit` 允许 `Fresh`，但截断读时逐个 `oldText` 计算行范围并要求被记录覆盖
    （精确匹配失败时用与 `apply_edits_to_normalized_content` 相同的模糊归一化，行号
    不变）；
  - 文件消失/替换（inode 变）/mtime/size/**内容哈希**变化 → `FileChangedSinceRead`；
  - 无记录 → `ReadBeforeEdit`。
- 写入成功后：旧快照立即失效，用写出的字节建立新基线（等价 qwen 的 `recordWrite`）。
  `write` 与「已覆盖全文件」的 `edit` 记录全文件范围；截断读之后的 `edit` 清空范围，
  要求重新 read。这样队列里排队的第二个并行 edit 看到的是「刚写完」的当前内容，而不是
  被自己的上一个写误判为外部修改。
- `invalidate(path)` 供外部失效（压缩删除 read 结果、会话结束）；`clear()` 清空整表。

### 5. 测试

新增/扩展（全部真实运行）：

- `crates/kiana-sandbox/tests/execution_policy.rs`（12 个）：
  - 多可写根、工作区外拒绝、`.git`/`.kiana`、符号链接逃逸/环、父目录穿越；
  - `spawn_relayed` 确定退出、relay 被 SIGKILL 后 `Unconfirmed` 且命令只执行一次
    （不自动重试）；SIGTERM 转发给命令后仍得到 `ConfirmedExit { code: 143,
    signal: Some(15) }`（不是把信号折叠成后端错误）；
  - 后端不可用（非法策略）命令不执行并映射 `BackendUnavailable`；
  - bwrap 可用则跑真实 relay，不可用显式跳过（`bubblewrap_backend_is_available_or_explicitly_skipped`；
    `unavailable_bubblewrap_is_backend_unavailable_or_explicitly_skipped`）。
- `crates/kiana-sandbox/src/landlock.rs` 单元测试
  `abi_below_four_cannot_enforce_network_denial`：ABI<3 无 TRUNCATE、ABI 3 无网络位，
  策略禁止网络时 fail-closed；`access_mask_is_abi_aware` 锁定各 ABI 的位。
- `crates/kiana-sandbox/src/execution_policy.rs` 单元测试新增
  `reads_are_not_restricted_by_the_write_policy`：read 在三档策略下都允许（与
  bwrap/Landlock 的「全盘可读、只限制写」语义一致）。
- `crates/kiana-tools/tests/read_before_write.rs`（15 个）：未读拒绝、读后外部修改、
  同尺寸同 mtime 靠内容哈希发现、重新 read 后恢复写入、截断读只改看到行、截断读不能
  整文件覆盖、并行 edit 串行且都生效、显式失效、新文件无需读、非文本（图片）读取不能
  解锁 edit/write（`READ_NOT_TEXT`）。
- `crates/kiana-tools/tests/execution_policy_gate.rs`（9 个）：workspace-write 的
  工作区外/`.git`/`.kiana` 拒绝（write 与 edit 都覆盖）、悬空符号链接逃逸、read-only、
  多可写根；bwrap 可用时 bash 与文件工具对同一批路径结论一致（含第二可写根）、
  PowerShell 命名的 shell 与 bash 同结论；`execute_bash` 的入口
  （`create_local_bash_operations_from_env` + `KIANA_SANDBOX_POLICY`）同样被约束；
  不可用显式跳过。
- `crates/kiana-tools/tests/sandbox_outcomes.rs`（6 个）：`Unconfirmed` 不自动重试、
  `BackendUnavailable` 不执行命令、超时/中止的 unconfirmed 提示、旧协议文案不变。
- `crates/kiana-tools/tests/bash_sandbox.rs` 保持通过（沙箱路径已切到 relay）。

## 参考设计对照

| 参考项目文件 | 借鉴的设计点 | Kiana 落点 |
| --- | --- | --- |
| `reference_project/codex/codex-rs/sandboxing/src/*`、`codex-rs/linux-sandbox` | 三档策略、可写根 + 根内只读子路径的减法模型、bwrap 参数顺序、fail-closed | `kiana-sandbox/src/policy.rs`、`bwrap.rs`（沿用并扩展） |
| `reference_project/codex/codex-rs/core/src/exec.rs`（`SandboxErr`、`ExecExpired`、合成退出状态） | 把「后端失败 / 超时 / 信号」与普通退出码分开，不能折叠成 137 | `execution_policy.rs` 的 `ExecutionStatus`、`relay.rs`、`bash.rs` 的状态映射 |
| `reference_project/qwen-code/packages/core/src/services/fileReadCache.ts` | `(dev, ino)` 身份、`mtime+size` 指纹、`ino==0` fail-closed、`recordWrite` 刷新基线、inode key 合并符号链接/硬链接 | `kiana-tools/src/read.rs` 的 `ReadStateRegistry`（另加内容哈希与行范围） |
| `reference_project/qwen-code/packages/core/src/tools/priorReadEnforcement.ts` | 结构化拒绝（`FILE_CHANGED_SINCE_READ` / `EDIT_REQUIRES_PRIOR_READ` / 验证失败分开）、stat 错误 fail-closed、截断读语义、`expectExisting` 消失检测 | `PriorReadStatus`、`read_before_write_error` / `file_changed_since_read_error`、`write.rs`/`edit.rs` 门禁 |
| `reference_project/kilocode/packages/opencode/src/tool/edit.txt`、`write.txt` | read-before-write 是**工具级契约**，模型必须知道「没读会失败」 | 运行时错误文案（`ReadBeforeEdit`）；工具描述保持与 TS 黄金文本一致，不改 |
| `reference_project/OpenManus/app/tool/str_replace_editor.py`、`app/sandbox/*` | 所有文件/命令走同一个 operator/后端；`view_range` 只暴露部分内容 | `ExecutionPolicy` 统一入口 + `ReadStateRegistry` 行范围（截断读只改看到部分） |

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana` 下执行。

```text
$ cargo test -p kiana-sandbox -p kiana-tools
30 个 suite：439 passed; 0 failed; 0 ignored
（其中 kiana-sandbox 55 个、kiana-tools 384 个）
```

关键子集：

```text
$ cargo test -p kiana-sandbox
lib: 36 passed（含 Landlock ABI<4 位掩码、read 不受写策略限制）
execution_policy: 12 passed（含 relay kill -> Unconfirmed、SIGTERM 转发）
sandbox: 7 passed（bwrap + Landlock 真实隔离）

$ cargo test -p kiana-tools
read_before_write: 15 passed
execution_policy_gate: 9 passed
sandbox_outcomes: 6 passed
edit: 46 passed（原有用例全部保留）
write: 12 passed
bash: 32 passed / bash_sandbox: 1 passed
read: 15 passed / ...: 全部通过
```

本机 bwrap 可用（`/usr/bin/bwrap`，内核 6.8），因此 shell 一致性测试真实执行；
bwrap 不可用的机器上相关用例会打印原因后跳过，不会假装通过。

## 未完成项（partial）

- `execute_bash`（`kiana-session/src/agent_session.rs`）没有显式的
  `ExecutionPolicy::resolve("execute_bash", ...)` 调用：它通过
  `create_local_bash_operations_from_env` 进入同一个 `LocalShellOperations`，实际已走
  统一策略与 relay（`execution_policy_gate.rs` 的 env 用例直接覆盖该入口）；若要显式
  标注工具名、或把 `ExecutionStatus` 结构化返回给 RPC，需要第 3 周/接线 agent 处理
  （见 handoff）。
- `ExecutionStatus::Interrupted` 已定义、有单元测试，但工具错误通道仍是字符串协议：
  启动前中止时 `bash` 执行器返回旧的 `aborted` 文案，没有把 `Interrupted` 结构体交给
  调用方。结构化需要改 `kiana-ext-api` 的 `BashResult`，不在本周所有权内。
- MCP stdio 子进程尚未接入 `ExecutionPolicy`/`SandboxManager`：`kiana-mcp` 不在本周
  所有权内，接线清单见 `docs/history/porting/handoffs/phase-02.md`。
- 压缩删除 read 结果时的记录失效尚未接线：`kiana-compaction`/`kiana-session` 不在
  本周所有权内；`ReadStateRegistry::invalidate/clear` 已就绪，调用点见 handoff。
- 会话级读记录：默认是进程级共享表（单进程多会话会共享记录）；会话构造时传入独立
  `Arc<ReadStateRegistry>` 的接线见 handoff。
- 权限层的 `read-only` / `addDirs` 尚未映射进文件工具的 `ExecutionPolicy`：
  `write`/`edit` 已支持显式策略，session 传入即可；当前缺省仍由
  `KIANA_SANDBOX_POLICY` 决定。
- 文件写入没有用 OS 级沙箱隔离进程内工具本身（计划里已说明：这一阶段是「同策略
  pre-check + 锁内复检」）；helper 进程属于后续工作。
- 工具描述（`EDIT_TOOL_DESCRIPTION`/`WRITE_TOOL_DESCRIPTION`）保持 TS 黄金文本，
  没有加入「先 read」提示；模型从 `ReadBeforeEdit` 错误里得知契约。

## 风险

- 读记录检查会整文件读入内存并计算哈希；大文件上的 write/edit 有额外 I/O 成本。
  这是有意的安全取舍（mtime/size 相同时仍能发现内容替换）。
- 门禁与真实写入之间仍有 TOCTOU 窗口：`file_mutation_queue` 只串行化进程内调用，
  外部进程可以在检查后改写文件。`edit` 在读取后会用内容哈希再复检一次；`write` 的
  窗口仍存在，需要 helper 进程/文件锁才能彻底关闭。
- 成功写入后用写出的字节重建基线（而不是彻底删除记录）：这是为了并行 edit 的第二个
  调用能继续工作，同时保证旧快照不会授权基于过期内容的修改；与计划里「失效读记录」
  的字面表述略有差异，旧快照确实失效了，但记录被新基线替换。
- `ExecutionPolicy::from_env` 在 write/edit 每次执行时读取进程环境；会话内修改环境
  会影响后续调用（工具构造时传显式策略可避免）。
- 非 Unix 平台只在 Windows 上通过 `volume_serial_number + file_index` 建立身份；
  其它平台 `ino == 0` 会让写入门禁拒绝（fail-closed），需要后续补路径身份。
- relay 多了一个 `/bin/sh` 进程与状态目录；超时/取消的进程树清理依赖 relay 是进程组
  组长（`detached`）。非 detached 调用方需要自行保证。
