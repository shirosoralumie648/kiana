# Wave A 深度修复记录（2026-11-07）

本波审计共 3 条 major（week-3 两条、week-2 一条），全部修复。构建统一使用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

执行过程中 `/data` 一度被 cargo target 占满（194G/196G，ENOSPC）。处理方式：删除
`/data/cargo-target-sandbox/debug/incremental`（3.3G）释放空间，并在后续命令中使用
`CARGO_INCREMENTAL=0` 复跑；没有删除任何测试或 artifact 之外的源码产物。

## 修复清单

### 1. major：managed `disableBypassPermissionsMode` 被 `permissions.mode: "off"` 绕过

- 文件：`crates/kiana-permissions/src/policy.rs:841`（`settings_engine` 的 `off` 分支）、
  `:2168`（新增测试）。
- 行为：`disableBypassPermissionsMode`（含 snake_case 与 `"disable"` 字符串形态）为真
  时，同层 `mode: "off"` 不再返回 `Some(None)`（整层关闭），而是按 `default`
  （fail-closed）继续解析同层 `rules` / `allow` / `deny` / `ask` 与目录。于是：
  - `from_settings_and_flags` 一定创建引擎（`Some(Some(engine))`），managed deny/allow
    规则真正生效；
  - `configuration_disables` 自动返回 `false`，session 的 `permissions_disabled` 与
    memory wiring 同步保持启用；
  - 同层的 `--permission-mode bypass` 仍被 `allowed_mode` 降级为 `default`，managed
    策略压过用户/项目层。
  未带该 managed 键的 `mode: "off"` 保持旧语义（返回 `None`，不改变现有测试）。
- 测试：`policy.rs::managed_bypass_disable_prevents_off_from_disabling_the_engine`
  （`mode:off` + managed 键 + deny 规则：引擎存在、mode=default、bash 被 deny、write 为
  ask、CLI bypass 被降级、snake_case/`"disable"` 形态同样生效）；
  `crates/kiana-session/tests/permissions_integration.rs:96::managed_disable_bypass_prevents_off_from_disabling_permissions`
  （真实 `AgentSession`：`permission_mode() == Some(Default)`，`execute_bash` 被 managed
  deny 拒绝且错误文本包含规则原因）。修复前第一项 `from_settings_and_flags` 返回
  `None`，第二项 session 会真的执行命令，两个测试都会失败。
- 命令与结果：`cargo test -p kiana-permissions` → 185 passed, 1 ignored；
  `cargo test -p kiana-session --test permissions_integration` → 17 passed。

### 2. major：powershell 用 POSIX lexer 分析，`\;` 隐藏第二条语句

- 文件：
  - `crates/kiana-permissions/src/shell.rs:48`（`ShellKind::Posix` / `PowerShell`）、
    `:288`（`analyze_power_shell`）、`:910`（`powershell_words`）、`:1048`
    （`scan_power_shell`）、`:1238` / `:1242`（`powershell_segments` /
    `split_raw_segments_power_shell`）；
  - `crates/kiana-permissions/src/request.rs:131`（按工具名选择方言，
    `ShellAnalysis::analyze_with_kind`，PowerShell 使用 `powershell_words`）；
  - `crates/kiana-permissions/src/command.rs:2583` / `:2609`
    （`any_segment_starts_with_kind` / `any_segment_matches_wildcard_kind`）、
    `powershell_command_substitutions` / `collect_powershell_fragments`（`$(…)`、
    `pwsh -Command` 内层脚本）；
  - `crates/kiana-permissions/src/policy.rs:340` / `:415`（CommandPrefix 与
    CommandWildcard 的 Deny/Ask 分支按 `request_shell_kind` 选择分段器）、`:444`
    （`request_shell_kind`）。
- 行为：PowerShell 路径实现独立词法：
  - 反引号 `` ` `` 是转义符（`` `; `` 不拆分；反引号 + 换行是续行）；
  - 反斜杠是**普通字符**，`\;` 里的 `;` 仍然是语句分隔符（审计绕过点）；
  - `;`、CR、LF、`|`、`&&`/`||`、括号与脚本块都作为语句边界；`''`/`""` 是引号内
    字面量引号；`#` 在词首开启注释。
  Deny/Ask 的 CommandPrefix 与 CommandWildcard 用同一方言重新分段，因此
  `Get-ChildItem \; Remove-Item …` 能看到第二段并命中 `Remove-Item` 规则；
  同时 `$(…)` 子表达式与 `pwsh -Command` 内层脚本会被递归收集。
  Allow 方向：命令含反斜杠或 CR 时状态标记为 `Unknown`（本 crate 的路径/写入提取器
  仍是 POSIX 语义，尚未 PowerShell 化），所有 Allow 规则失效、按 Ask 处理；
  `simple_command=false` 也会让 `Get-ChildItem \; Remove-Item` 无法命中
  `Get-ChildItem` 前缀 Allow。`bash` 与其它工具保持 POSIX 行为不变。
- 测试：`crates/kiana-permissions/tests/shell_analysis.rs:401`
  `powershell_backslash_semicolon_cannot_hide_a_second_statement`
  （Get-ChildItem 前缀 Allow 不放行、Remove-Item 前缀 Deny 与 wildcard Deny 命中、
  同一文本在 bash 下仍是 1 段）、`:457`
  `powershell_cr_and_pipeline_are_statement_separators`、`:480`
  `powershell_backtick_escapes_and_backslash_is_literal`（反引号转义不拆分、
  `.\src` 保留反斜杠、纯 PowerShell 命令仍可 Complete）。修复前第一条命令被
  POSIX 解析为 1 段、simple=true，`Get-ChildItem` 前缀 Allow 会放行，两个 Allow/Deny
  断言都会失败。
- 命令与结果：`cargo test -p kiana-permissions powershell` → 8 passed, 177 filtered
  out；`cargo test -p kiana-permissions` → 185 passed, 1 ignored。

### 3. major：取消吞掉 `aborted|unconfirmed` 与沙箱状态/拒绝错误

- 文件：`crates/kiana-tools/src/bash_executor.rs:250`
  （`exec_error_carries_safety_conclusion`）、`:341`（`exec` 错误处理）。
- 行为：`execute_bash_with_operations` 在 token 已取消时只吞掉普通错误（`aborted`、
  `timeout:…` 等，保持 TS 的“取消返回已产生输出”语义）；以下安全结论即使取消也
  原样返回 `Err`：
  - 文本含 `|unconfirmed`（relay 已启动命令后被 kill，命令可能已经执行）；
  - `ExecutionStatus` 前缀 `Unconfirmed:` / `BackendUnavailable:` / `Interrupted:`；
  - `SandboxDenial` 码 `INVALID_CONFIG:` / `UNKNOWN_TOOL:` / `TOOL_INPUT_MISMATCH:` /
    `NO_WRITE_PATHS:` / `PATH_UNRESOLVED:` / `READ_ONLY:` / `OUTSIDE_WORKSPACE:` /
    `PROTECTED_PATH:`。
  `AgentSession::execute_bash` 因此把 “可能已执行、不要自动重试” 的错误交给调用方，
  不再记录成 `cancelled` 的 bash 结果。`ShellExecute` 原有的 `aborted|unconfirmed`
  提示逻辑不受影响（它直接调用 `operations.exec`）。
- 测试：
  - `crates/kiana-tools/tests/bash_executor.rs:254`
    `unconfirmed_abort_survives_cancellation`（cancelled token + Err
    `"aborted|unconfirmed"`，断言错误文本到达调用方）；
  - `:283` `sandbox_status_and_denial_errors_survive_cancellation`（7 种状态/拒绝
    文本逐一断言原样返回）；
  - 同一文件保留 `errors_are_swallowed_after_cancellation` 与新增
    `plain_abort_and_timeout_are_still_swallowed_after_cancellation`，证明普通
    `aborted`/`timeout:` 仍被吞成 `Ok(cancelled)`；
  - `crates/kiana-session/tests/agent_session_bash_persistence.rs:308`
    `unconfirmed_bash_abort_surfaces_to_the_caller`（真实 `AgentSession::execute_bash`
    + `abort_bash()`，断言 `Err` 且包含 `unconfirmed`）。修复前 `exec` 的 Err 在
    token 取消后被丢弃，返回 `Ok(cancelled)`，上述安全断言都会失败。
- 命令与结果：`cargo test -p kiana-tools --test bash_executor` → 9 passed；
  `cargo test -p kiana-session --test agent_session_bash_persistence unconfirmed` →
  1 passed, 16 filtered out。

## 全量验证

在 `/data` 已满后删除 `debug/incremental` 并设置 `CARGO_INCREMENTAL=0`：

| 命令 | 结果 |
| --- | --- |
| `timeout 900 cargo check --workspace` | Finished dev profile（10 crates compiled） |
| `timeout 900 cargo test -p kiana-permissions` | 185 passed, 1 ignored |
| `timeout 900 cargo test -p kiana-tools` | 438 passed |
| `timeout 900 cargo test -p kiana-session` | 519 passed, 5 ignored |
| `timeout 900 cargo test -p kiana-config` | 680 passed |
| `timeout 900 cargo test -p kiana-coding-agent` | 422 passed, 5 ignored |

`cargo fmt --check --edition 2024` 已对全部改动文件通过；未删除/忽略任何测试，
未修改 `reference_project/**` 与 `docs/history/porting/plan.md`。

## 未修项与已知限制

- 无未修 finding。
- PowerShell 的路径/写入提取仍是 POSIX 实现，因此含反斜杠或 CR 的 powershell 命令
  一律 `Unknown`（Allow 失效、Ask 处理）——这是本波采用的 fail-closed 过渡策略，
  不影响 Deny/Ask 看到每条语句。后续若要允许 Windows 路径命令走前缀 Allow，需要把
  `extract_write_paths_with_completeness` / `extract_read_paths` 一并 PowerShell 化。
- PowerShell 的 virtual operations（`Get-Content`/`Get-ChildItem`/`Invoke-WebRequest`
  到 read_file/list_directory/web_fetch 的映射）不在本 finding 范围内，未扩展；
  当前只影响 Deny/Ask 的等价工具规则覆盖度，不影响语句分段安全结论。
