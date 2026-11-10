# W1 — 权限与审批闭环

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 1 周（权限真正闭环）。目标是把「是/否」审批升级为
`AllowOnce` / `AllowAlways{Session|Local|Project}` / `Reject{reason}`，并且让审批结果
可以持久化、重启后仍然生效。

## 改了什么

### 1. 结构化审批结果

新增 `crates/kiana-permissions/src/approval.rs`：

- `ApprovalDecision::{AllowOnce, AllowAlways{scope}, Reject{reason}}`；
- `ApprovalScope::{Session, Local, Project}`；
- `ApprovalDecision::rules_for(request)`：`AllowAlways` 时生成规则。
  - 简单命令按前两个词生成 `CommandPrefix`；
  - 复合命令/分析不完整生成 `ExactCommand`（新增的精确匹配规则）；
  - 文件工具按每个写入路径生成 `PathPrefix`；
  - MCP/未知工具按工具名生成 `Tool`。
- `AllowAlways` 只生成 `Allow` 规则，不会覆盖 `Deny`：引擎仍按
  `Deny → Ask → Allow` 匹配，灾难性命令检查也仍然先执行。

`crates/kiana-permissions/src/policy.rs`：

- 新增 `PermissionRule::ExactCommand`、`exact_command()`、`to_settings_value()`；
- settings 规则支持 `{"command": "...", "exact": true, "decision": "allow"}`；
- `PermissionDecision::Ask` 增加 `reason` 字段：UI 用 `prompt`，事件/审计用 `reason`
  （不含完整命令/参数）。

`crates/kiana-permissions/src/request.rs`：

- 新增 `target_summary()`（命令前缀/路径/工具名）与 `redacted_args_summary()`
  （敏感键、`--password value`、`key=value`、`Bearer ...` 脱敏）。

### 2. 结构化审批接入 `AgentSession`

`crates/kiana-session/src/agent_session.rs`：

- 原来唯一的权限 `ExtensionUiContext::confirm` 调用点改为 `select` 流程：
  1. `Allow once` / `Always allow` / `Reject`；
  2. 选 `Always allow` 时再选 `This session` / `This project` / `This machine (local)`；
  3. 选 `Reject` 时用 `input` 收集可选原因。
- 没有交互式 UI（print/headless）时仍 fail-closed，不会把 `Ask` 当允许。
- 持久化：
  - `Session`：`PermissionEngine::record_session_allow`，只存内存；
  - `Local`：写 `<cwd>/.kiana/settings.local.json`；
  - `Project`：通过新增的 `SettingsManager::update_project_permissions` 写项目
    `settings.json`，只改 `permissions` 字段，项目未受信时拒绝写入。
- 启动时 `Core::new` 把 `settings.json` 的 `permissions` 与
  `settings.local.json` 的 `permissions` 合并后交给引擎。
- 新增事件：
  - `permission_requested`：`toolName`、`action`、脱敏 `summary`、`target`、`reason`；
  - `permission_decided`：`toolName`、`decision`、`scope`、`reason`、`target`；
  - `permission_denied`：`toolName`、`reason`、`target`。
- 事件里的 `reason` 只用决策原因（模式/规则/安全策略），不拼接完整命令；
  `summary` 走脱敏。

### 3. 配置文件写回

新增 `crates/kiana-config/src/settings/permissions.rs`：

- `local_settings_path` / `project_settings_path`；
- `read_permissions_file`；
- `append_permission_rule` / `append_rule_to_permissions`：只追加
  `permissions.rules`，其它字段和键序保留，重复规则幂等；
- `merge_permission_settings`：规则/allow/deny/addDirs 取并集，`mode` 以 local 为准；
- 文件损坏、根不是对象、`permissions`/`rules` 类型不对时返回错误且不写盘。

`SettingsManager` 新增：

- `update_project_permissions(update)`：原子更新项目 `permissions`；
- `project_permissions()`：只读项目作用域的 `permissions`。

### 4. 测试

- `crates/kiana-permissions/src/approval.rs`：4 个单元测试（规则粒度、简单/复合命令、
  路径规则、AllowOnce/Reject 不产生规则）。
- `crates/kiana-permissions/src/policy.rs`：新增精确命令匹配、settings 往返、
  Allow 不覆盖 Deny、bash/powershell 同策略等单元测试。
- `crates/kiana-permissions/src/request.rs`：新增脱敏与 target 摘要测试。
- `crates/kiana-config/src/settings/permissions.rs`：4 个测试（键序保留、幂等、合并、
  坏 JSON 不覆盖）。
- `crates/kiana-session/tests/permissions_integration.rs`：从 6 个扩展到 12 个用例，
  新增 AllowOnce 再次询问、Session 记忆、Local 持久化、Project 写回、事件脱敏、
  bash 工具调用同一审批流。
- `crates/kiana-coding-agent/tests/permission_cli.rs`（新）：7 个用例，覆盖 CLI flag
  解析、CLI 覆盖 settings、deny 优先、`--add-dir`、headless 仍返回 Ask。

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在
`/home/ctyun/code/kiana` 下实际执行。

```text
$ cargo check -p kiana-session
cargo check (9 crates compiled)
Finished `dev` profile ... in 17.88s

$ cargo check --workspace
cargo check (22 crates compiled)
Finished `dev` profile ... in 28.34s

$ cargo test -p kiana-permissions
cargo test: 134 passed (2 suites, 0.03s)

$ cargo test -p kiana-session
cargo test: 359 passed, 5 ignored (66 suites, 128.72s)

$ cargo test -p kiana-config
cargo test: 633 passed (45 suites, 32.98s)

$ cargo test -p kiana-coding-agent --test args --test permission_cli
cargo test: 117 passed (2 suites, 0.01s)

$ cargo fmt --all -- --check
（无输出，退出码 0）
```

## 剩余 TODO

- TUI 目前复用通用 `select` 列表（主选择 + 作用域二级选择），还没有专用审批卡、
  diff 预览和更漂亮的按钮布局；按计划留到第 12 周。
- print/json 模式仍没有审批通道：无 UI 时直接拒绝；`--yes` / 非交互自动批准策略未做。
- `settings.local.json` 的写回走独立文件读写，不经过 `SettingsManager` 的写队列和
  文件锁；与外部进程同时写这个文件时没有互斥。
- `SettingsManager::reload` 不会重新读取 local 文件；会话内规则已即时加入引擎，
  只有重启/新建会话才会重新合并 local。
- Project 作用域写入受项目信任限制；未受信时「总是允许（项目）」会 fail-closed，
  用户仍可选 Session。
- MCP/未知工具的「总是允许」按工具名放行，粒度仍偏粗；参数级规则需要新的规则类型。
- ShellAnalyzer 的子命令拆分、重定向/heredoc/替换识别、规则建议仍属第 3 周。
- 事件 `target` 对多路径写入只取第一个路径；完整路径列表仍在审批 UI 中展示。

## 风险

- `AllowAlways` 调用 `engine.add_rules` 会触发 `clear_session_allow`，此前通过
  「只允许一次/本会话」记住的其它动作会重新询问。方向偏严，不会放宽权限，但会影响
  体验。
- 简单命令按「前两个词」记忆，像 `git -C /path status` 会以 `git -C` 为规则前缀，
  比预期宽；这是计划里「前两个词」取舍的直接结果，需要后续用 ShellAnalyzer 改进。
- 启动时 local 文件损坏会跳过 local 层（project/global 仍生效），这可能让用户写在
  local 的 `deny` 规则暂时失效；文件写回路径本身会报错且不覆盖。
- 事件脱敏是启发式：常见 `key=value`、`--flag value`、`Bearer` 能覆盖，但把 secret
  作为普通位置参数时可能识别不到；审批 UI 仍显示真实命令。
- 并发发起的两个相同工具调用是两次独立请求，会各自询问一次；没有做全局去重。
- CLI `--allow` / `--deny` 只影响当前进程，不会写回配置。
