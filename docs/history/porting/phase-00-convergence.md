# W4 — 半成品收敛与文档对齐

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 0 周（先止血）：权限的启用入口、运行期切换、
`execute_bash` 权限路径、以及 crate 数量/层表文档。

## 改了什么

### 1. 权限启用不再只依赖环境变量

`crates/kiana-permissions/src/policy.rs`

- 新增 `PermissionFlags`（`mode` / `allow` / `deny` / `add_dirs`）与
  `PermissionEngine::from_settings_and_flags(cwd, settings, flags)`。
- settings 里的 `permissions` 对象支持：
  - `mode`: `read-only` / `default` / `accept-edits` / `bypass` / `off`；
  - `rules`: `{tool, decision, reason}`、`{command: [...], decision, reason}`、
    `{path, decision, reason}`；
  - `allow` / `deny` 简写数组；
  - `addDirs` / `workspaceRoots`。
- 优先级：命令行模式 > settings > 环境变量。settings 显式 `off` 时只有显式
  `--permission-mode` 能重新启用。
- 新增 `parse_cli_rule`，命令行规则支持 `tool:<pattern>`、`command:<words>`、
  `path:<prefix>`，以及裸工具名 / 含空白的命令前缀 / 绝对路径。
- `PermissionEngine` 的 policy 改为 `RwLock`，新增 `set_mode` / `set_rules` /
  `add_rules` / `add_workspace_root`。模式或规则变化会清空本会话的「总是允许」记录，
  避免旧批准在新模式下继续生效。

`crates/kiana-session/src/agent_session.rs`

- `Core::new` 从 `PermissionEngine::from_env(cwd)` 改为
  `from_settings_and_flags(cwd, settings["permissions"], &PermissionFlags::default())`，
  环境变量仍是兜底。
- 新增 `set_permission_mode` / `set_permission_rules` / `add_permission_rules` /
  `add_permission_directory` / `permission_mode`，引擎未启用时会按需创建。
- `execute_bash` 继续在真正执行前调用 `bash_refusal`（与 `before_tool_call` 共用同一套
  `PermissionRequest`、规则与会话批准）；read-only 拒绝、default 无 UI 时 fail-closed、
  bypass 放行由测试锁定。
- PowerShell 默认 operations 已经走 `create_local_shell_operations_from_env`（与 bash 同
  一套沙箱策略），且 tool call 走 `before_tool_call` 的权限判断；调用方自带
  `operations` 时由调用方负责隔离（crate 文档已声明）。

`crates/kiana-coding-agent`（CLI）

- 新增 `--permission-mode`、`--allow`、`--deny`、`--add-dir` 四个选项；非法模式或空规则
  进入诊断并以退出码 1 结束。
- 会话创建后立即应用这些 flags（settings/env 已在 `Core::new` 生效）。
- 更新 `tests/golden/help.txt` 与 `help_with_extensions.txt`。

### 2. 文档对齐

- `README.md`：29 → 30 个 crate。
- `docs/START_HERE.md`：29 → 30，层表新增 `kiana-permissions`（L4）。
- `docs/ARCHITECTURE_OVERVIEW.md`：28 → 30，层表新增 `kiana-sandbox`（L0）与
  `kiana-permissions`（L4），去掉过期依赖边计数。
- 根 `ARCHITECTURE.md`：path 依赖 28 → 29、workspace 条目 106 → 107、新增
  `kiana-permissions` 行/层/依赖边（`kiana-permissions -> kiana-core, kiana-sandbox`），
  `kiana-session` 的边补上 `kiana-permissions`。
- 新增 `docs/adr/durable.md`：`kiana-durable` 是后台任务/子代理/cron/恢复的唯一调度与
  事务存储底座；`kiana-session` 只保留前台会话；不另造队列。

### 3. 测试

- `crates/kiana-permissions/src/policy.rs` 新增 6 个单元测试：settings 模式、`off` 与
  CLI 覆盖、deny 优先、rules/addDirs 解析、运行期 setter、CLI 规则语法。
- 新增 `crates/kiana-session/tests/permissions_integration.rs`（6 个用例）：read-only
  拒绝 `execute_bash`、default 无 UI fail-closed、bypass 执行、运行期切模式立即生效、
  运行期 deny 覆盖 bypass、无引擎时 `set_permission_rules` 创建引擎。
- `crates/kiana-coding-agent/tests/args.rs` 新增 3 个用例：权限 flag 解析、非法模式、
  非法规则。

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

```text
$ cargo test -p kiana-permissions
cargo test: 121 passed (2 suites, 0.03s)

$ cargo test -p kiana-session --test permissions_integration
cargo test: 6 passed (1 suite, 0.68s)

$ cargo test -p kiana-coding-agent --test args
test result: ok. 110 passed; 0 failed; 0 ignored

$ cargo test -p kiana-permissions -p kiana-session -p kiana-sandbox
cargo test: 497 passed, 5 ignored (71 suites, 162.97s)

$ cargo check -p kiana-session
Finished `dev` profile ... in 1m 05s

$ cargo check --workspace
cargo check (30 crates compiled)
Finished `dev` profile ... in 1m 21s

$ cargo fmt --all -- --check
（无输出）
```

## 剩余 TODO

- 审批仍是 Yes/No；`AllowOnce` / `AllowAlways{Session|Local|Project}` / `Reject{reason}`、
  `permission_requested`/`permission_decided`/`permission_denied` 事件、写回 settings
  属于第 1 周。
- CLI 规则语法目前是轻量解析；没有 `ShellAnalyzer` 的子命令拆分、重定向/heredoc 识别，
  也没有规则建议。复杂 shell 仍只由 `kiana-sandbox` 隔离。
- `permissions` settings 仍按未知键读原始 JSON（不参与 schema 校验）；后续改成类型化
  settings 时需保留对原始 `permissions` 的兼容。
- `kiana-chord` 的 `host` / `object` / `connection` / `reload` 仍是文档化骨架；
  `kiana-durable` 的 browser IndexedDB 与 Cloudflare 后端是产品边界（明确返回
  not supported），不是待办。
- `--add-dir` 只影响权限层的工作区根；沙箱策略仍由 `KIANA_SANDBOX_POLICY` 决定，后续
  执行策略统一时要把两者合并（第 2 周）。

## 风险

- 环境变量测试没有直接覆盖：`KIANA_PERMISSIONS` 是进程级全局状态，并行测试会互相污染；
  现有测试只覆盖 settings/flags，env 分支保持原实现。
- 显式 `Allow` 规则可以在 `read-only` 模式下放行执行（规则优先级高于模式默认）。这是
  用户显式写规则时的预期行为，但如果希望 read-only 绝对不可执行，需要重新评审规则顺序。
- settings 的 `permissions` 对象没有 schema 诊断；形状写错时按「未配置」回退到 env，
  不会静默关闭权限层，但也不会有"第几行第几列"的错误提示。
- 辅助进程/沙箱 relay 的结果状态（`Unconfirmed`/`BackendUnavailable`）仍未实现，属于
  第 2 周。
