# W3 — 配置分层 + shell 风险分析

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 3 周。目标：让“这个值从哪来”可回答、让 managed policy
不可被项目层覆盖；同时让 shell 前缀 allow 规则不能跨复合命令/重定向/命令替换误放行，
并给出精确/前缀/通配三种规则形态与 virtual operations。

## 参考设计对照表

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
|---|---|---|
| `reference_project/codex/codex-rs/config/src/config_layer_source.rs` | `ConfigLayerSource` 的来源枚举、`precedence()` 数值排序、层元数据 | `crates/kiana-config/src/settings/layers.rs`：`ConfigLayerSource::{ALL, precedence, standard_path}` |
| `reference_project/codex/codex-rs/config/src/fingerprint.rs` | 逐 leaf path 记录 `origins`；canonical JSON 后做 SHA256（`version_for_toml`） | `layers.rs`：`merge_value_into`/`record_tree_origins`、`canonical_json`、`layer_fingerprint`/`aggregate_fingerprint`、`MergedConfig::{origins, fingerprint}` |
| `reference_project/codex/codex-rs/config/src/merge.rs` | 低层 base + 高层 overlay 的递归合并；路径敏感的特殊合并 | `layers.rs`：`merge_value_into`、`array_merge_mode` 表驱动数组策略 |
| `reference_project/codex/codex-rs/config/src/diagnostics.rs` | 诊断带来源文件与路径；managed 文件错误要可定位 | `crates/kiana-config/src/settings/strict.rs`（复用）；`layers.rs::ConfigLayer::load_file` |
| `reference_project/codex/codex-rs/config/src/loader/local.rs` | system/user/project 层的标准文件发现顺序 | `layers.rs`：`ConfigLayerSource::standard_path`、`ConfigLayers::load_standard{,_filtered}` |
| `reference_project/ClaudeCode/src/utils/settings/settings.ts` | 数组跨来源 concat + 去重；managed-settings 文件的加载位置 | `layers.rs`：`ArrayMergeMode::ConcatDedup`、`array_merge_mode` |
| `reference_project/ClaudeCode/src/utils/settings/types.ts` | `allowManagedPermissionRulesOnly`、`disableBypassPermissionsMode`、allow/denied MCP servers 字段 | `layers.rs`：`ManagedPolicy::from_value`（camel/snake 双拼写） |
| `reference_project/ClaudeCode/src/utils/permissions/permissionsLoader.ts` | policy 开启时只采信 managed permission rules；不持久化新规则 | `layers.rs`：`ManagedPolicy::restrict_permission_rules`（allow/ask 只读 managed，deny 全层合并） |
| `reference_project/ClaudeCode/src/services/mcp/config.ts` | deny 绝对优先；空 allowlist 拒绝全部；name/command/url 三种匹配；冲突拒绝 | `layers.rs`：`McpServerRule::{matches,label}`、`ManagedPolicy::{mcp_server_decision,conflicts}`、`filter_conflicting_allowed` |
| `reference_project/ClaudeCode/src/utils/permissions/permissionSetup.ts` | bypass 模式被 policy 禁用 | `layers.rs`：`ManagedPolicy::apply` 强制 `permissions.disableBypassPermissionsMode=true`；`policy.rs`：`PermissionEngine::{from_settings_and_flags,set_mode,evaluate}` 降级 bypass |
| `reference_project/qwen-code/packages/core/src/permissions/rule-parser.ts` | 子命令拆分；`*` 词边界语义；模式不跨运算符匹配 | `crates/kiana-permissions/src/shell.rs`：`split_raw_segments`、`command_pattern_matches`；`crates/kiana-permissions/src/policy.rs`：`CommandWildcard` |
| `reference_project/qwen-code/packages/core/src/permissions/permission-manager.ts` | 复合命令逐段评估、取最严格结果；virtual ops 只能升级不能降级 | `kiana-permissions/src/policy.rs`：Allow 型 prefix/wildcard 只匹配单一简单命令；Ask/Deny 用 `any_segment_{starts_with,matches_wildcard}` 扫描每段 |
| `reference_project/qwen-code/packages/core/src/permissions/shell-semantics.ts` | `cat→read_file`、重定向→`write_file`、`curl/wget→web_fetch`、`ls→list_directory` 及已知局限 | `kiana-permissions/src/shell.rs`：`ShellAnalysis::virtual_operations`、`VirtualOperation` |
| `reference_project/qwen-code/packages/core/src/utils/shellAstParser.ts` | root + known subcommand 的最小规则；已知子命令表 | `kiana-permissions/src/shell.rs`：`ShellAnalysis::rule_suggestions`、`is_known_subcommand`（刻意不生成 `prog *`） |
| `reference_project/gemini-cli/packages/core/src/policy/policy-engine.ts` | 解析失败按默认 ask/deny；重定向/命令替换把 ALLOW 降级为 ASK；子命令逐个检查；包装器递归 | `kiana-permissions/src/shell.rs`：`ShellAnalysisStatus::{Unknown,Incomplete}`、`scan_shell`；`policy.rs::evaluate` 的 Unknown guard |
| `reference_project/gemini-cli/packages/core/src/utils/shell-utils.ts` | `hasRedirection` / `detectCommandSubstitution` / `parseCommandDetails` 的检测面 | `shell.rs`：`scan_shell`、`extract_redirections`、`unclosed_heredoc` |
| `reference_project/SWE-agent/sweagent/tools/tools.py` | `ToolFilterConfig` 的 blocklist（前缀）与 `blocklist_standalone`（精确）两种粒度 | `policy.rs`：`CommandPrefix`（仅简单命令）与 `ExactCommand`（复合/精确）的区分 |
| `reference_project/SWE-agent/sweagent/tools/utils.py` | 多行输入用 heredoc 包裹；heredoc delimiter 是命令边界 | `shell.rs`：`unclosed_heredoc`、`extract_redirections` 的 `Heredoc` 类别；复用 `command::strip_heredoc_bodies` |

## 改了什么

### 1. `crates/kiana-config/src/settings/layers.rs`（新增）

- `ConfigLayerSource`：`Builtin/System/Managed/Enterprise/User/Project/Local/CliFlag/Session`，
  `ALL` 按低 → 高排列；`precedence()`：0/10/20/30/40/50/60/70/80。
- `ConfigLayer`：内存值或文件路径 + 严格诊断；`load_file` 按
  `LayerLoadMode::{Lenient,Strict}` 解析，文件不存在返回 `Ok(None)`，IO/语法错误返回
  `LayerLoadError`。
- `ConfigLayers::{push,with_layer,resolve,merge}`：稳定按优先级排序后合并；对象递归，
  数组由 `array_merge_mode` 表决定：
  - `ConcatDedup`：`permissions.rules/allow/deny/ask/addDirs/workspaceRoots/...`、
    MCP allow/deny、`extensions/prompts/themes/packages/skills/enabledModels/defaultTools`、
    所有 `hooks.*`；
  - 其它（`npmCommand` 等）默认 `Replace`。
- `origins`：叶值与数组下标记录最终胜出来源；同一元素被高层重复声明时来源也更新为
  高层。容器 key（`permissions` 等）由 `origin()` 汇总其后代里优先级最高的来源。
- `fingerprint`：每层 canonical JSON（递归排序 key，数组顺序保留）SHA256 →
  `sha256:<hex>`；整体指纹按“来源 + 每层 canonical JSON”汇总。键序不敏感，改任一
  有效值会变。
- `ManagedPolicy`：从 System/Managed/Enterprise 读取；camelCase/snake_case 都接受，
  也接受放在 `permissions` 对象里。
  - `allow_managed_permission_rules_only`：allow/ask 只保留 managed；deny 从所有层合并；
    `rules` 保留 managed 全部 + 非 managed 的 deny。
  - `allow_managed_hooks_only`：`hooks` 只保留 managed。
  - `disable_bypass_permissions_mode`：强制有效配置
    `permissions.disableBypassPermissionsMode=true`。
  - `allowed/denied_mcp_servers`：deny 永远合并（camel/snake 合并到 snake 规范键）；
    allow 在 managed 配置时只读 managed；allow+deny 冲突条目从有效 allowlist 移除，
    并记录 `ManagedPolicyConflict`；`mcp_server_decision` 中 deny 绝对优先，空 allowlist
    拒绝全部。
  - **全层 deny 参与决策**：`apply` 把有效配置里所有层（含用户/项目）的
    `deniedMcpServers`/`denied_mcp_servers` 并进 `ManagedPolicy::denied_mcp_servers`，
    并用这份全层 deny 集合过滤 managed allowlist。这样“项目 deny + managed allow”
    的组合会判定 Denied，而不是只有管理员自己的 deny 生效。
  - policy 布尔标志（`allowManagedPermissionRulesOnly` / `allowManagedHooksOnly`）
    在有效值里也会被强制为 true（snake 别名同步），project 层写 `false` 不能覆盖；
    `disable_bypass_permissions_mode` 继续强制写入 `permissions`。
  - 嵌套写法 `permissions.allowedMcpServers` / `permissions.deniedMcpServers` 与顶层
    键一样参与 allowlist 合并和规范键输出。
- `standard_path` / `load_standard_filtered`：User 用
  `kiana_core::config::get_settings_path()`（默认 `~/.kiana/agent/settings.json`），
  Project/Local 用 `cwd/.kiana/...`，System 系用 `/etc/kiana/...` 或
  `%ProgramData%\kiana\...`。`load_standard_filtered` 支持项目未受信时跳过
  Project/Local。

### 2. 严格诊断接线

- `settings/mod.rs` 声明了 `key_path`、`strict`、`layers` 三个模块。
- 工作树里原有的 `key_path.rs` / `strict.rs` 是部分实现，保留并首次编译；只修了
  `strict.rs` 的未使用 import。
- `strict_settings_schema()` 的顶层已知键与 policy 键、`permissions` 内部字段均已覆盖
  （含 `wildcard`）；未知字段/类型/枚举值带文件、1-based 行列、dotted key path。
- 默认只对 System/Managed/Enterprise 严格；User/Project/Local 保持宽松。

### 3. `crates/kiana-permissions/src/shell.rs`（新增）

- `ShellAnalysis::analyze`：
  - 子命令拆分上限 `MAX_SUBCOMMANDS = 50`，超出 `truncated` + `Incomplete`；
  - 识别简单命令、重定向（`<`/`>`/`>>`/`>|`/`&>`/`<<`/`<<<`/fd dup）、heredoc、
    命令替换（`$(…)`/反引号/`<()`/`>()`）、未闭合引号/替换/heredoc、结尾反斜杠、
    引号外换行；
  - 无法可靠解析 → `Unknown`；截断 → `Incomplete`；两者都 `is_trustworthy()==false`。
- `virtual_operations(cwd)`：`cat/head/tail/...` → `ReadFile`；`ls/tree` → `ListDirectory`；
  `tee`/重定向 → `WriteFile`；`curl/wget` → `WebFetch{url,host}`。
- `rule_suggestions()`：单一简单命令且第二个词是已知子命令 → `program+subcommand`
  前缀；其它 → 完整精确命令。**不生成 `prog *`**。
- 复用 `command.rs` 的词法/heredoc/注释/包装器实现；只把
  `strip_heredoc_bodies`、`strip_shell_comments`、`heredoc_delimiter`、`analyze_words`
  的可见性改为 `pub(crate)`，未改变任何既有签名。

### 4. `kiana-permissions` 规则与请求接线

- `PermissionRequest` 新增 `pub shell: Option<ShellAnalysis>` 与 `shell_analysis()`；
  `simple_command` / `analysis_incomplete` 由 analyzer 推导；virtual operations 补齐
  `paths`/`read_paths` 与 `network`。
- `PermissionRule` 新增 `CommandWildcard { pattern, ... }`：
  - settings 形态 `{"command": "git status*", "wildcard": true, "decision": "allow"}`；
  - CLI `--allow 'command:git status*'` 或含空格的 `'git status*'` 生成通配规则；
  - 无 `*` 的 wildcard 规则按精确匹配；`git *` 也匹配单独的 `git`。
- virtual operations 也参与 Tool 规则匹配：`Tool{"read_file"/"write_file"/"web_fetch"/
  "list_directory", Deny|Ask}` 会约束 shell 的等价操作；同时接受 Kiana 内建工具名
  `read`/`write`/`edit`/`ls`（见第 5 节）。Allow 型 virtual 规则不参与，避免用读规则
  扩大 shell 权限。
- 匹配语义：
  - Allow 型 `CommandPrefix`/`CommandWildcard` **只**匹配单一简单命令；
  - Ask/Deny 型继续扫描复合命令每一段（含 `bash -c` 内层与命令替换），保持
    deny-overrides 的 fail-closed 行为（不改变第 1 周既有测试）；
  - `analysis_incomplete`/`Unknown` 时引擎在应用 Allow 规则前返回 Ask；
    read-only 仍走 base Deny，bypass 仍全部放行。
- shell 写 `.git`/`.kiana`：在 Deny 规则之后、会话批准与 Allow 规则之前检查
  `request.paths` 的受保护子目录；即使有 `Tool{bash,Allow}` 也先 Ask。
- `approval.rs` 改用 `ShellAnalysis::rule_suggestions`，`git -C /tmp status` 不再生成
  过宽的 `["git","-C"]` 前缀。
- `matches()` 改为 `pub`，供 UI/测试预览规则效果。

### 5. managed bypass 在权限引擎内生效

`crates/kiana-permissions/src/policy.rs`：

- `PermissionEngine` 新增私有 `bypass_disabled` 标志（不改变 `PermissionPolicy` 的
  公开字段形状）；`from_settings_and_flags` 从 `permissions` 对象读取
  `disableBypassPermissionsMode`（camel/snake，`true`/`"disable"`/`"true"`）。
- 为真时：settings 的 `mode: "bypass"`、`--permission-mode bypass`、运行期
  `set_mode(bypass)` 全部降级为 `default`；`evaluate` 再做一次防御性降级。只设置该
  标志、没有 mode/rules 的对象也会创建引擎（否则策略会被当成“没有配置”忽略）。
- 新增只读 `bypass_permissions_disabled()` 供诊断/UI 使用。

`crates/kiana-permissions/src/policy.rs`（virtual operations）：

- Deny/Ask 型 `Tool` 规则同时接受虚拟操作名与 Kiana 内建工具名：`read_file` ↔
  `read`、`write_file` ↔ `write`/`edit`、`list_directory` ↔ `ls`。这样
  `Tool{read,Deny}` 会约束 `cat file`，而不是只认参考设计里的 `read_file`。Allow
  仍不参与虚拟操作，避免扩大 shell 权限。

### 6. `kiana-core/src/paths.rs`

未修改。配置层路径直接在 `kiana-config` 中实现，避免动这个有 2000+ 对拍用例的
TS 路径移植文件。

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana`
下实际执行。

```text
$ cargo test -p kiana-config
cargo test: 674 passed (47 suites, 35.75s)

$ cargo test -p kiana-config --test config_layers
cargo test: 11 passed (1 suite, 0.00s)

$ cargo test -p kiana-config --lib layers
cargo test: 17 passed, 63 filtered out (1 suite, 0.00s)

$ cargo test -p kiana-permissions
cargo test: 169 passed, 1 ignored (3 suites, 0.04s)
（lib 151 + tests/shell_analysis.rs 18；ignored 是文档示例）

$ cargo check -p kiana-config -p kiana-permissions
cargo check (3 crates compiled)
Finished `dev` profile ... in 4.43s

$ cargo check -p kiana-core
Finished `dev` profile ... in 0.39s

$ cargo fmt -p kiana-config -p kiana-permissions -- --check
（无输出，退出码 0）
```

新增测试覆盖：

- `config_layers.rs`：9 层同 key 高层胜、origins、指纹键序/值敏感、数组表、
  managed policy 不可被 project 覆盖（策略对象与有效值都断言）、managed allow 规则
  + 全层 deny、用户 deny 否决 managed allowlist、严格诊断（文件/行/列/key path）、
  标准路径、camel/snake policy。
- `shell_analysis.rs`：复合命令不命中前缀 Allow、重定向/替换/反引号/换行/未闭合引号
  不放行、Unknown 与 50 上限 fail-closed、cat/重定向受路径规则约束、
  `.git/.kiana` 受保护写入、exact/prefix/wildcard 语义、wildcard settings 往返、
  virtual operations、内建工具名（read/write/ls）约束虚拟操作、规则建议不含
  `prog *`。
- `layers.rs` 内联单测：合并/数组去重/指纹/策略/全层 MCP deny/嵌套 policy 键/
  严格诊断/heredoc 检测/通配匹配。
- `policy.rs` 内联单测：managed `disableBypassPermissionsMode` 降级 settings、
  CLI flag 与运行期 `set_mode`；snake/字符串形态；只有策略标志时仍创建引擎。

## 未完成项 / 后续接线

- `ConfigLayers` 尚未接入 `SettingsManager` / `AgentSession::Core::new`；当前有效设置仍由
  旧的 global+project 合并路径产生。接线清单见 `docs/history/porting/handoffs/phase-03.md`。
- `disable_bypass_permissions_mode` 已由 `PermissionEngine` 从 `permissions` 对象读取
  并强制降级（settings / CLI flag / 运行期 `set_mode`）；接线方只要把分层后的
  `permissions` 交给 `from_settings_and_flags` 即可。若 `SettingsManager` 仍走旧的
  合并路径，则要等 handoff 第 1 步完成才会看到 managed 标志。
- virtual operations 现在既通过 `paths`/`read_paths` 约束 `PathPrefix` 规则，也通过
  `Tool{Deny|Ask}`（虚拟名 `read_file`/`write_file`/`web_fetch`/`list_directory` 或
  Kiana 内建名 `read`/`write`/`edit`/`ls`）约束 shell 的等价操作；Allow 型 virtual
  规则刻意不参与（避免扩大 shell 权限）。
- strict 模式对“新文件”需要调用方显式传 `LayerLoadMode::Strict`；默认只对
  System/Managed/Enterprise 严格。
- ShellAnalyzer 是启发式：变量展开、解释器脚本（`python x.py`）、`xargs`、`eval` 仍可
  绕过静态分析；它是风险提示与规则建议，不是唯一安全边界。
- `kiana-core/src/paths.rs` 未改；如果后续要把层路径也收敛到 core，可迁移
  `standard_path`，但当前没有必要。

## 风险

- `origins` 对 concat 数组只有一个“最终胜出层”的标量来源（下标级也会记录）；
  多层共同贡献的数组，顶层路径的 origin 是最高贡献层，不代表所有元素来源。
- managed policy 的 MCP allowlist 在 managed 配置存在时完全丢弃用户/项目的 allow
  条目（deny 仍从所有层合并，并且现在会进入 `mcp_server_decision`，也能剔除冲突的
  allow 条目）。这是按“allow 在 policy 下只读 managed”的直译；如果产品希望
  “没有 allowManagedMcpServersOnly 时合并所有来源”，需要调整 `ManagedPolicy::apply`。
- `permissions.disableBypassPermissionsMode` 的规范键是 camelCase；MCP 的规范键是
  snake_case（`allowed_mcp_servers`/`denied_mcp_servers`）。接线方必须读规范键。
- 未闭合引号/反引号的检测是词法扫描：字符串里的注释、复杂算术扩展等可能误报；
  误报只会让命令更严格（Ask），不会放宽。
- managed `disableBypassPermissionsMode` 与用户显式 `mode: "off"` 同时出现时，仍按
  旧语义整体关闭权限层（`off` 优先于“创建引擎”）；此时 bypass 标志没有可执行的
  检查对象。若产品要求 managed policy 连 `off` 也禁止，需要在接线层把 `off` 改判为
  `default`。
- 保护目录检查依赖 `canonical_with_missing`；符号链接环/无权限路径按受保护处理。
- 新增 `pub shell` 字段改变了 `PermissionRequest` 的结构体形状；仓库内没有外部
  struct literal 构造点，已用 `cargo check -p kiana-session` 验证。外部 crate 若手写
  struct literal 需要补字段（应改用 `from_tool_call`）。
