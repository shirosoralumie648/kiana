# W4 验证报告

日期：2026-11-07
验证对象：`docs/history/porting/phase-00-convergence.md`（第 0 周：半成品收敛与文档对齐）
工作树：`/home/ctyun/code/kiana`（master，无历史提交，全部文件为未跟踪状态）

结论：**通过**。必需的两个 `cargo check` 均为 Finished；相关测试实际执行并通过；
未发现 `todo!()` / `unimplemented!()` / 空函数；公共 API 调用点已更新；
文档 crate 数量与 workspace 成员一致（30）。

---

## 1. 必需命令与真实结果

全部在 `/home/ctyun/code/kiana` 下执行，前缀 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

```text
$ cargo check -p kiana-session
cargo check (15 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 00s

$ cargo check --workspace          # timeout 600000ms
cargo check (23 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 13s
```

说明：`--workspace` 是增量编译，只重新编译了 23 个 crate（其余命中缓存），不是失败。
workspace 共 30 个成员，见下文第 5 节。

## 2. 回归测试与真实结果

```text
$ cargo test -p kiana-permissions
cargo test: 121 passed (2 suites, 0.02s)

$ cargo test -p kiana-session --test permissions_integration
cargo test: 6 passed (1 suite, 0.63s)

$ cargo test -p kiana-coding-agent --test args
cargo test: 110 passed (1 suite, 0.01s)

$ cargo test -p kiana-permissions -p kiana-session -p kiana-sandbox
cargo test: 497 passed, 5 ignored (71 suites, 134.27s)

$ cargo fmt --all -- --check
（无输出，退出码 0）
```

这些是实际执行的命令与输出，不是照抄上一个 agent 的报告。

## 3. 是否真有实现（非桩代码）

对本次改动涉及的源文件做了定向扫描：

```text
grep -rn "todo!()\|unimplemented!()" \
  crates/kiana-permissions/src crates/kiana-session/src \
  crates/kiana-coding-agent/src crates/kiana-session/tests/permissions_integration.rs
=> 无匹配

grep -rnE "fn ...\(...\)( *-> *...)? *\{ *\}" <上述改动文件>
=> 无空函数体
```

关键实现点逐项确认：

- `crates/kiana-permissions/src/policy.rs`
  - `PermissionFlags`（L271）、`PermissionEngine::from_settings_and_flags`（L305）、
    `from_env`（L455）、`set_mode`（L512）、`set_rules`（L521）、`add_rules`（L530）、
    `add_workspace_root`（L540）、`parse_cli_rule`（L714）均存在且有实体逻辑。
  - policy 字段为 `RwLock<PermissionPolicy>`，setter 会加写锁并调用
    `clear_session_allow()`。
  - `from_settings_and_flags` 的分支完整：settings `off` 仅允许显式 CLI 模式重新启用；
    无 settings 回退 env；env 也未启用时，只有显式 flags 才创建引擎。
- `crates/kiana-session/src/agent_session.rs`
  - `Core::new` 在 L1745 使用 `PermissionEngine::from_settings_and_flags(cwd,
    settings.raw("permissions"), &PermissionFlags::default())`，不再是 `from_env`。
  - Core 层 `set_permission_mode`（L3077）/ `set_permission_rules`（L3091）/
    `add_permission_rules`（L3104）/ `add_permission_directory`（L3121）均会在引擎
    尚未启用时按需创建引擎。
  - `AgentSession` 公共方法（L8296-8316）转发到 Core；`permission_mode()` 返回
    `Option<PermissionMode>`。
  - `execute_bash`（L8328）在真正执行前调用 `bash_refusal(&resolved_command)`
    （L8351），检查的是包含 `shellCommandPrefix` 的实际执行文本；默认 operations
    走 `create_local_bash_operations_from_env`，保留沙箱隔离。
- `crates/kiana-coding-agent`
  - `cli/args.rs`：`permission_mode` / `permission_allow` / `permission_deny` /
    `permission_add_dirs` 字段与 `--permission-mode`、`--allow`、`--deny`、`--add-dir`
    解析分支（L407-431）齐全，非法值进入诊断。
  - `app.rs`（L598-618）：会话创建后应用 mode、合并 allow/deny 规则、追加目录。
  - `tests/golden/help.txt` 与 `help_with_extensions.txt` 均包含四项新帮助文本。

## 4. 新增行为是否有测试

- `crates/kiana-permissions/src/policy.rs`：6 个单元测试（settings 模式、`off` 与 CLI
  覆盖、deny 优先、rules/addDirs 解析、运行期 setter、CLI 规则语法）。包含在
  `cargo test -p kiana-permissions` 的 121 passed 内。
- `crates/kiana-session/tests/permissions_integration.rs`：6 个用例全部实际运行通过：
  `execute_bash_is_denied_in_read_only_mode_from_settings`、
  `execute_bash_default_mode_fails_closed_without_ui`、
  `execute_bash_is_allowed_in_bypass_mode`、
  `set_permission_mode_takes_effect_immediately`、
  `deny_rule_added_at_runtime_still_wins`、
  `set_permission_rules_creates_engine_when_absent`。
- `crates/kiana-coding-agent/tests/args.rs`：`parses_permission_mode_and_rules`、
  `invalid_permission_mode_is_an_error`、`invalid_permission_rule_is_an_error` 均在
  110 passed 内。

## 5. 文档与 crate 数量

- `Cargo.toml` workspace `members` = 30 项（实际计数 30）。
- `README.md` L3：30 个 crate。
- `docs/START_HERE.md`：30 个 crate，层表含 `kiana-sandbox`（L0）与
  `kiana-permissions`（L4）。
- `docs/ARCHITECTURE_OVERVIEW.md`：30 个 crate，层表含 L0 `kiana-sandbox`、L4
  `kiana-permissions`。
- 根 `ARCHITECTURE.md`：30 个 crate、29 个 path 依赖、`[workspace.dependencies]` 107 项
  （29 path + 78 外部），与 Cargo.toml 的 30 个成员（`kiana-evals` 未被其他 crate 作为
  path 依赖引用）自洽。
- `docs/adr/durable.md` 存在（约 3.0K），明确 `kiana-durable` 为后台任务/子代理/cron/
  恢复的唯一调度与事务存储底座，`kiana-session` 只保留前台会话。

## 6. 未覆盖 / 保留事项（与 phase-00-convergence.md 一致）

- 环境变量分支（`KIANA_PERMISSIONS` / `KIANA_PERMISSION_MODE`）没有进程级并行测试，
  因为 env 是全局状态；本次验证未单独跑 env 用例。
- 显式 `Allow` 规则在 `read-only` 模式下仍可放行执行（规则优先于模式默认），
  这是已记录的已知风险，属于产品取舍，不视为本次缺陷。
- `permissions` settings 仍按原始 JSON 读取，未类型化 schema；写错形状按未配置回退，
  不会静默关闭权限层。
- 审批结构化（AllowOnce/AllowAlways/Reject）、事件、settings 写回属于第 1 周。
- `kiana-chord` 的 host/object/connection/reload 仍是文档化骨架，属预期。

## 7. 验证过程中是否需要修复

不需要。两个必需命令和全部相关测试一次通过，未修改任何源代码或测试。
本次只新增了本验证文档。
