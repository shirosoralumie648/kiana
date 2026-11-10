# Wave 0 深度修复记录（2026-11-07）

审计问题共 6 条（1 critical + 5 major），本波全部修复。构建统一使用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox`；磁盘曾满，删除 `debug/incremental`
后以 `CARGO_INCREMENTAL=0` 运行。

## 修复清单

### 1. critical：审批生成的 Tool 规则不区分工具来源

- 文件：`crates/kiana-permissions/src/policy.rs:107`（`PermissionRule::Approved`）、
  `:1053`（评估顺序）、`crates/kiana-permissions/src/approval.rs:123`
  （所有审批规则统一包装）、`crates/kiana-permissions/src/request.rs:79`（`builtin` 来源）。
- 行为：`approval_rules` 生成的所有规则都包在
  `PermissionRule::Approved { rule, builtin }` 里；`builtin` 记录生成规则时的工具来源
  （`mcp__*` 恒为非内建）。匹配时要求 `request.builtin` 相同，因此为自定义 `edit`
  写下的规则不会再命中内建 `edit`（包括 read-only 模式）。会话批准 key 也加入来源，
  自定义 `bash` 不能复用内建 `bash` 的会话批准。
- 持久化：`approved: true` + `origin: "builtin"|"custom"` 写回 settings；
  strict schema（`crates/kiana-config/src/settings/strict.rs`）接受这两个键；缺少/非法
  origin 的审批规则会被丢弃（审批规则只会是 Allow，丢弃是 fail-closed）。
- 测试：
  `kiana-permissions` `approval.rs::custom_tool_approvals_cannot_grant_builtin_tool_permissions`
  （自定义 edit 批准 → 写回 settings → 新引擎；内建 edit 在 read-only 下仍被拒绝，
  自定义 edit 仍被允许）、
  `request.rs::tool_origin_is_recorded_and_separates_approval_keys`。

### 2. major：审批 UI 不显示命令/路径

- 文件：`crates/kiana-ext-api/src/ui.rs:134`（`PermissionCardRequest::card_title`）、
  `:569`（默认 `permission_card` 使用 `card_title`，并按 `always_scopes` 决定是否展示
  “Always allow”）；`crates/kiana-session/src/agent_session.rs` 把完整 `args` 交给卡片。
- 行为：默认卡片标题现在包含工具名、`command` / `path` / `file_path` / `url`
  或脱敏截断后的参数、工作目录与决策原因。TUI 与 RPC 都使用这个默认实现，因此两条
  路径都会展示真实目标；没有可持久化作用域时不再展示 “Always allow”。
- 测试：`kiana-ext-api` `ui_contract.rs::default_permission_card_shows_tool_and_target_before_approval`
  （bash 命令、write 路径、Always 选项缺失）；
  `kiana-session` `permissions_integration.rs::allow_once_asks_again_on_the_next_identical_call`
  断言实际 `select` 标题包含 `bash` 与 `echo allow-once`。

### 3. major：Always allow（Local/Project）命中 Ask 规则时静默失效

- 文件：`crates/kiana-permissions/src/policy.rs:1053`（Approved 规则先于 Ask 规则求值）、
  `:1185`（`approval_can_persist`）；`crates/kiana-session/src/agent_session.rs:4602`
  （安全确认不提供持久作用域）、`:4689`（持久化入口再次 fail-closed）。
- 行为：用户显式批准的 `Approved` 规则在 `Deny`、灾难性命令检查和 `.git`/`.kiana`
  安全检查之后、`Ask` 规则之前求值，因此 Local/Project 作用域的“总是允许”真正生效；
  普通 Allow 规则仍不能覆盖 Ask（deny > ask > allow 不变）。受保护目录写入每次都必须
  重新确认：UI 不提供 Always，`persist_approval` 也拒绝任何作用域的 Always。
- 测试：`policy.rs::persisted_approvals_override_ask_rules_for_every_scope`（三个作用域 +
  普通 Allow 仍输给 Ask）、`policy.rs::protected_path_writes_always_ask_for_every_scope`
  （`.git`/`.kiana`、三个作用域、`approval_can_persist == false`）；
  `kiana-session` `always_allow_local_overrides_a_configured_ask_rule`（第二次不再询问，
  重启后仍覆盖 Ask）、`protected_git_writes_always_prompt_and_never_offer_always_allow`
  （选项中没有 Always allow，两次都询问）。

### 4. major：项目 settings 加载失败时 Always allow（项目）报告成功但不写盘

- 文件：`crates/kiana-config/src/settings/mod.rs:1455`（`mutate_project_and_save`）。
- 行为：`project_settings_load_error` 非空时直接返回 `SettingsValueError`，既不写盘也
  不更新内存；`AgentSession::persist_approval` 把错误转成 `permission_denied` 并拒绝
  这次调用（fail-closed）。
- 测试：`kiana-config` `settings_permissions.rs::project_permission_writeback_fails_when_the_project_file_cannot_be_loaded`
  （返回错误、损坏文件原样保留、重启后没有规则）；
  `kiana-session` `project_always_allow_fails_closed_when_project_settings_are_corrupt`
  （真实文件后端、error 文本、`permission_denied` 事件、重启无规则）。

### 5. major：权限 CLI flag 会把后面的选项当成值

- 文件：`crates/kiana-coding-agent/src/cli/args.rs:410` 起（`--permission-mode` /
  `--allow` / `--deny` / `--add-dir`）。
- 行为：取值以 `-` 开头视为缺值并记 error 诊断，且不消费该 token（后续选项继续正常
  解析）；末尾缺值同样报诊断，不再落入未知长选项分支。额外支持 `--flag=value`
  （值可以以 `-` 开头）。
- 测试：`kiana-coding-agent` `args.rs::permission_flags_reject_a_following_option_as_value`、
  `permission_flags_report_missing_values_at_end_of_args`、
  `permission_flags_accept_equals_values`；
  `permission_cli.rs::permission_flag_values_do_not_swallow_following_options`
  （`--deny --permission-mode read-only` 得到 read-only 引擎、无 deny 规则）。

### 6. major：配置层接受的 permissions 键从未被引擎执行

- 文件：`crates/kiana-permissions/src/policy.rs:754`（`defaultMode` 作为 `mode` 别名）、
  `:764`（`additionalDirectories` 加入工作区根）、`:787`（`ask` 简写解析成 Ask 规则）。
- 行为：`permissions.ask` 真正生成 Ask 规则；`defaultMode` 与 `mode` 等价（`mode` 优先），
  managed policy 的 `defaultMode: "read-only"` 现在会启用 read-only 引擎而不是关掉权限层；
  `additionalDirectories` 与 `addDirs`/`workspaceRoots` 一样扩展可写根。
- 测试：`policy.rs::settings_ask_default_mode_and_additional_directories_are_enforced`；
  `kiana-session` `default_mode_setting_activates_a_read_only_engine`
  （settings 里只写 `defaultMode`，`execute_bash` 被 read-only 拒绝）。

## 验证命令与真实结果

```text
CARGO_TARGET_DIR=/data/cargo-target-sandbox CARGO_INCREMENTAL=0 \
  timeout 900 cargo check --workspace
# 结果（rustfmt 前）：cargo check (15 crates compiled)
#      Finished `dev` profile [unoptimized + debuginfo] target(s) in 45.51s
# rustfmt 后复跑：cargo check (7 crates compiled), Finished in 25.41s

CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-permissions
# 结果：181 passed, 1 ignored (3 suites)

CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-config
# 结果：680 passed (47 suites)

CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-ext-api
# 结果：80 passed (9 suites)

CARGO_TARGET_DIR=/data/cargo-target-sandbox CARGO_INCREMENTAL=0 timeout 900 cargo test -p kiana-session
# 结果：517 passed, 5 ignored (76 suites)

CARGO_TARGET_DIR=/data/cargo-target-sandbox CARGO_INCREMENTAL=0 timeout 900 cargo test -p kiana-coding-agent
# 结果：422 passed, 5 ignored (27 suites)
```

rustfmt 之后重跑受影响目标（全部通过）：

```text
cargo test -p kiana-session --test permissions_integration         # 16 passed (1 suite)
cargo test -p kiana-coding-agent --test args --test permission_cli # 121 passed (2 suites)
cargo test -p kiana-ext-api --test ui_contract                     # 6 passed (1 suite)
cargo test -p kiana-config --test settings_permissions             # 2 passed (1 suite)
```

（各 crate 的 `ignored` 均为本波之前已存在的忽略测试，本波没有新增 `#[ignore]`。）

## 未修项

无。6 条 finding（critical ×1、major ×5）全部修复并带回归测试。

## 备注

- `/data/cargo-target-sandbox` 曾达到 100%（194G）；删除 `debug/incremental`（3.9G）
  后测试恢复，后续命令使用 `CARGO_INCREMENTAL=0`。
- 旧的 settings 中已经存在的普通 `{"tool":"edit","decision":"allow"}` 规则无法与用户
  手写规则区分，行为保持不变；本修复保证**新生成**的审批规则带来源标记。
