# 第 11 周前半：子代理运行时

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 457-471 行中本 lane 负责的第 1-5 项。第 6-11 项
（后台任务、验收、会话恢复、执行所有权、checkpoint、ledger）属于 week11b，本文件不覆盖。

## 参考设计对照

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
|---|---|---|
| `reference_project/ClaudeCode/src/tools/AgentTool/loadAgentsDir.ts` | Markdown frontmatter 逐字段校验；`agentType` 缺省取文件名；来源固定顺序、后者覆盖前者（参考实现为 `builtin < plugin < user < project < flag < policy`；Kiana 采用 `builtin < plugin < user < project < policy < flag`，让 `--agents` 最高）；Markdown 正文作为 system prompt；未知键在影子 schema 中不阻止加载 | `crates/kiana-session/src/agent_definition/mod.rs` 的 `AgentDefinitionLoader::load`、`parse_markdown_definition`；`frontmatter.rs` 的 `parse_document` |
| `reference_project/qwen-code/docs/design/declarative-agents-port.md` | 15 个字段的类型/默认值表：`model: inherit`、`tools: "*"`、`disallowedTools`、`effort` 枚举或整数、`permissionMode` 枚举、`maxTurns` 正整数、`memory` 三值、`background` 布尔/字符串、`isolation` 枚举；单文件失败不影响目录 | 同上；`AgentDefinition` 字段与 `DefinitionBuilder::apply_fields` |
| `reference_project/ClaudeCode/src/tools/AgentTool/runAgent.ts` | fork context 过滤不完整 tool call；工具池由调用方按子代理权限模式单独计算；`allowedTools` 替换而不是继承父会话的全部 allow 规则；async/headless 代理不能弹权限提示 | `crates/kiana-session/src/subagent/runtime.rs` 的 `SubagentRuntime::prepare/build`；`permissions.rs` 的 `SubagentPermissionContext::new`；`tool_pool.rs` 的 `ToolPool::filter` |
| `reference_project/ClaudeCode/src/utils/worktree.ts` | `createAgentWorktree` / `removeAgentWorktree`：用 canonical git root、独立 worktree 目录、完成后删 worktree 与临时分支；失败必须显式报错 | `crates/kiana-session/src/subagent/isolation.rs` 的 `create_workspace`、`cleanup_ownership`；不支持时返回 `IsolationError::Unsupported` |
| `reference_project/qwen-code/packages/core/src/agents/forkedAgent.ts` | 共享 prompt cache 是 provider 能力 + 显式开关，不是子代理定义的默认行为；缓存参数按 session 隔离 | `runtime.rs` 的 `ProviderCapabilities { prompt_cache }` 与 `prompt_cache_sharing_enabled()`（`fork && share_prompt_cache && provider.prompt_cache`，默认全 false） |
| `reference_project/deer-flow/backend/packages/harness/deerflow/subagents/status_contract.py` | 终态枚举 `completed/failed/cancelled/timed_out/polling_timed_out`；`stop_reason` 与终态正交（capped 但产出了结果仍是 completed）；只对 completed 保留 result brief | `crates/kiana-session/src/subagent/result.rs` 的 `SubagentTerminal`、`SubagentStopReason`、`SubagentTerminalState` |
| `reference_project/deer-flow/backend/packages/harness/deerflow/subagents/executor.py` | 终态只能发布一次；token usage 与 tool receipts 从流式 chunk 累积；未结束的调用在取消/超时路径收尾 | `result.rs` 的 `TerminalCell::try_set_terminal`、`UsageAccumulator`、`ReceiptAccumulator::finalize_pending`；`runtime.rs` 的 `record_chunk` |

## 改了什么

### 1. AgentDefinition（`crates/kiana-session/src/agent_definition/`）

- 新增 `AgentDefinition`：`description`、`prompt`、`tools`/`disallowedTools`、`model`
  （`Inherit`/`Named`）、`effort`、`permissionMode`、`mcpServers`、`hooks`、`skills`、
  `maxTurns`、`memory`、`background`、`isolation`、`initialPrompt`、`omitClaudeMd`、
  `source`、`filePath`。
- 新增 `AgentDefinitionLoader`：
  - 固定覆盖顺序 `builtin < plugin < user < project < policy < flag`，与 root 的添加
    顺序无关；
  - `with_default_roots(cwd)` 读取 `<agent-dir>/agents`、`<cwd>/.kiana/agents`、
    `/etc/kiana/agents`；
  - `add_plugin_dir`、`add_flag_json`（`--agents <json>`）；
  - 单文件超过 1 MiB、YAML 错误、未知字段、类型错误都进入
    `AgentDefinitionLoadReport::errors`，其它文件继续加载；
  - 未知 frontmatter 字段显式报错（DoD 要求，和参考实现的宽松忽略不同）。
- `frontmatter.rs` 是手写的 YAML 子集解析器（workspace 没有 YAML 依赖，且禁止改
  根 `Cargo.toml`）：map/sequence/行内数组与 map/引号/布尔/null/整数/浮点/行内注释；
  不支持的 YAML 特性直接报错。
- 同样不新增依赖：`result.rs` 自带 SHA-256（FIPS 180-4，公开向量测试），
  `isolation.rs` 用 `extern "C"` 直接声明 `ioctl`/`mount`/`umount2`/`geteuid`。
- 内建定义：`general-purpose`、`explore`（只读、`omitClaudeMd`）、`plan`（只读）。

### 2. 子代理运行时（`crates/kiana-session/src/subagent/runtime.rs`）

- `SubagentParentContext`：父会话 id/cwd、工具快照、父允许集合、权限策略与模式、
  模型、provider 能力。
- `SubagentRuntime`：
  - `prepare` 先建立隔离工作区，再创建子会话（header 的 cwd 是子代理真实工作目录）；
  - 独立 `ToolPool`、`SubagentPermissionContext`、system prompt、`TokenBudget`、
    `TerminalCell`、`UsageAccumulator`、`ReceiptAccumulator`；
  - `dispatch_tool` 顺序：工具表注册 → allow/deny 二次检查 → 权限引擎 → 执行；
  - `resume` 通过 `SubagentTranscript::resume` 复用 transcript，工具/权限按当前父上下文
    重建，不再新建隔离工作区（沿用会话记录 cwd）；
  - `record_turn` 执行 `maxTurns`；`record_chunk` 累积 usage/receipt 并在预算耗尽时
    写 `failed + token_capped`；
  - `resume` 校验会话链接的 agent 名与父会话 id，拒绝跨 agent/跨父会话恢复。
- `AgentSession` 新增 `subagent_parent_context()`、`prepare_subagent()`、
  `resume_subagent()`（直接改在 `agent_session.rs`，本周拥有该文件）。

### 3. 工具命名空间与权限收窄（`canonical_tool.rs`、`tool_pool.rs`、`permissions.rs`）

- `CanonicalToolId`：`builtin` / `mcp:<server>` / `plugin:<plugin>` / `ext`，
  展示名三段式；`ToolPattern` 支持 `*`、`builtin:*`、`mcp:server:*`、
  `mcp__server__*` 等写法。
- `ToolPermissionSet::resolve` 先与父工具表求交，再按子代理 `tools` 过滤；
  `intersect` 对两个输入都满足子集关系。
- `ToolPool::schemas()` 与 `get_checked()` 共用允许集合与 denylist，父 deny 的工具
  既不出现在 schema，也无法直接 dispatch。
- `SubagentPermissionContext`：权限模式取 `min(parent, requested)`；父会话的
  session allow 不继承；`disallowedTools` 先于引擎规则；headless 的 `Ask` 一律
  `Deny`，交互模式返回 `NeedsApproval` 交给调用方。

### 4. 工作区隔离（`isolation.rs`）

- `IsolationMode { Shared, Worktree, CowClone }`；后端
  `Shared/GitWorktree/Reflink/Overlayfs`。
- `create_workspace`：
  - Worktree：`git rev-parse --show-toplevel` + `git worktree add --detach`；
  - CowClone：先尝试逐文件 `FICLONE`（btrfs/XFS reflink），失败后尝试 overlayfs；
  - 任何不可用都返回 `IsolationError::Unsupported`，不静默退化成共享目录。
- `capture_baseline`：HEAD、staged、unstaged、untracked（含 bytes/sha256/mode）、
  nested repo（各自 HEAD/patch/untracked）。
- `capture_delta`：
  - tracked 用 `git diff --binary --full-index <baseline HEAD>`；
  - untracked 新/改文件用 `git diff --no-index --binary -- /dev/null <path>`；符号链接
    按 `120000` 模式输出，超过 baseline 单文件上限的文件仍会生成补丁（只是不做哈希短路）；
  - baseline 时存在、现在删除的 untracked 文件生成删除补丁（把 baseline 字节写到临时
    文件再改写路径；符号链接写成临时 symlink，避免重放成普通文件）；
  - nested repo 生成独立的 `NestedDelta`；nested repo 被删除或新增时无法用文件补丁
    表示，返回 `DeltaUnsupported` 而不是静默丢失。
- `apply_delta` 用 `git apply --binary`；测试覆盖文本、二进制、untracked、nested repo。
- `WorkspaceOwnership` 写 `<state_dir>/ownership/<id>.json`；`cleanup` 按后端删除
  worktree/branch、目录或 umount overlay；加载记录时校验 workspace/worktree 在状态
  目录内、worktree 后端的 git_root 存在且 branch 属于本实现、overlayfs 的 workspace
  必须是 `<state>/overlays/<id>/merged`（拒绝被篡改的记录诱导删除状态目录本身）；
  创建时校验 id 为安全文件名（拒绝 `../` 等路径穿越）。创建失败（含写 ownership
  记录失败）会回滚已创建的 worktree/clone/overlay 布局。
- `SubagentRuntime` 析构时回收隔离工作区（见 `Drop`）；调用方要在销毁前取走内容。

### 5. 结果契约（`result.rs`、`yield_tool.rs`）

- `SubagentTerminal` 五值 + `SubagentStopReason`（token/turn/loop/model_length/error）。
- `TerminalCell::try_set_terminal`：互斥写入，只有一个赢家，`seq` 从 1 开始；后到者
  返回 false 且不覆盖。
- `UsageAccumulator` / `ReceiptAccumulator`：从 `SubagentStreamChunk` 累积；
  receipt 记录 `tool`、参数/输出 sha256、状态、稳定序号；未完成调用可
  `finalize_pending`。
- `YieldAssembler`：支持 `{label,value}`、`{labels:{...}}`、直接 label map；
  数组 append、标量 replace；strict 下类型变化与 schema 失败报错，permissive 记录
  warning；`max_retries` 默认 2，超限 `exhausted=true`；`finish()` 做最终校验。
- `create_yield_tool(contract)` 把装配器包装成可注册的 `AgentTool`（`name = "yield"`）；
  `SubagentRuntime::build_yield_tool()` 在存在 yield 契约时返回它。

### 6. 子会话关联与 resume（`transcript.rs`）

- 子会话 header 的 `parentSession` 指向父会话 id，并追加
  `kiana.subagent.link` 自定义条目（parent/child/sidechain/agent/depth/createdAt）。
- `record_spawn_in_parent` 在父会话追加 `kiana.subagent.spawned`。
- `seed_initial_prompt` 只在空 transcript 写入；`append_message_if_new` 对最后一条
  做忽略时间戳的重复检查；`append_message` 保持无条件写入。

## 做完的标准对照

| DoD | 结果 | 测试 |
|---|---|---|
| 子代理 allow `{*}`、父 allow `{read}` → 有效 `{read}` | 通过 | `tests/subagent_runtime.rs::child_star_is_intersected_with_parent_read_only`；`runtime`/`canonical_tool` 单测 |
| 父 deny 的工具从 schema 消失，直接 dispatch 也失败 | 通过 | `parent_denied_tool_is_missing_from_schema_and_dispatch`；`tool_pool` 单测 |
| headless 子代理 Ask 为 Deny | 通过 | `headless_subagent_turns_ask_into_deny`；`permissions` 单测 |
| 子代理工具 schema/dispatch 只含 allowlist | 通过 | 同上 + `ToolPool::get_checked` 单测 |
| unknown frontmatter 报错但不中止目录 | 通过 | `unknown_frontmatter_field_does_not_abort_directory`；loader 单测 |
| resume 复用 transcript 且消息数不翻倍 | 通过 | `resume_reuses_transcript_without_doubling_messages`；transcript 单测 |
| `maxTurns` 超限写入 `turn_capped`（额外契约） | 通过 | `max_turns_sets_turn_capped_terminal_once` |
| resume 拒绝跨 agent/跨父会话（额外契约） | 通过 | `resume_rejects_a_different_agent` |
| terminal 竞争只有一个赢家（额外契约） | 通过 | `terminal_race_has_one_winner_and_budget_cap_is_sticky`；`result` 单测 |
| yield strict/permissive、有限重试、label append/replace（额外契约） | 通过 | `yield_tool` 单测；`yield_tool_executes_incrementally_and_reports_errors` |
| yield 工具可注册（额外契约） | 通过 | `yield_tool_is_built_only_when_contract_is_present` |
| delta patch 可被 `git apply` 字节一致重放（额外契约） | 通过 | `isolation_delta_round_trips_through_git_apply`；isolation 单测（文本/二进制/nested/删除） |

## 真实测试命令与输出摘要

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在
`/home/ctyun/code/kiana` 下实际执行：

```text
$ cargo check -p kiana-session
cargo check (1 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.29s

$ cargo check -p kiana-tools -p kiana-session-core -p kiana-durable
cargo check (52 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 07s

$ cargo test -p kiana-session --lib
cargo test: 86 passed (1 suite, 0.41s)

$ cargo test -p kiana-session --test subagent_runtime
cargo test: 9 passed (1 suite, 0.49s)

$ cargo test -p kiana-session
cargo test: 459 passed, 5 ignored (70 suites, 150.54s)

$ cargo fmt -p kiana-session -- --check
fmt-exit=0
```

说明：功能改动完成后重新跑了全量 `cargo test -p kiana-session`（见上）；把 sha2/libc
依赖替换为内置实现后，`--lib`（86 个，含新增 SHA-256 公开向量测试）、
`--test subagent_runtime`（9 个）和全量 459 个测试都重新实跑通过；`Cargo.lock` 中
`kiana-session` 的依赖表已回到不含 `sha2`/`libc` 的原始集合。

## 未完成项 / partial

- **Worktree 不自动带入父工作区未提交改动**：`git worktree add` 从 HEAD 创建，父目录
  的 staged/unstaged/untracked 不会自动同步。`capture_baseline`/`capture_delta`/
  `apply_delta` 已提供完整能力，但 `create_workspace` 没有自动 seed；需要在调用方
  （或 week11b 的 task runtime）决定是否叠加父 delta。
- **overlayfs 未在测试中实际 mount**：实现与清理路径已写好，但需要 root/CAP_SYS_ADMIN；
  无权限时显式返回 `Unsupported`。测试只覆盖 reflink 探测与显式拒绝。
- **skills 预加载只注入名字**：`system_prompt()` 列出 `skills`，但没有加载技能正文；
  真正的内容加载需要 `ResourceLoader` 快照，本 lane 没有把它引入子代理运行时。
- **memory scope 只解析不消费**：`memory` 字段已解析；记忆注入/抽取属于记忆子系统。
- **background 只解析**：`background=true` 的任务化执行、输出文件、通知属于 week11b。
- **omitClaudeMd 只暴露标志**：子代理运行时没有组装 user context，因此由调用方按
  `omit_claude_md()` 决定是否注入父 CLAUDE.md。
- **隔离 ownership 没有启动 sweep**：记录与清理 API 已有，崩溃后自动恢复属于 week11b。
- **`resume` 不重建隔离工作区**：沿用子会话 header 记录的 cwd；若工作区已被清理，
  调用方需要重新创建或回退 Shared。
- **`append_message_if_new` 只比较最后一条**：用于 resume 重放尾部消息；如果调用方需要
  “整段 transcript 去重”，应由上层按消息 id 处理。
- **MCP/hooks 配置只保存不执行**：`mcpServers`/`hooks` 已按原样解析进定义；每代理
  MCP server 与 hook 的实际挂载需要在子代理启动流程里接线（未在本 lane 做）。

## 风险

- frontmatter 是手写 YAML 子集；多行折叠（`|`/`>`）、锚点、复杂键会显式报错。用户
  从别的工具迁移 agent 文件时可能遇到解析差异。
- `permissionMode` 的 `dontAsk`/`auto` 映射到 `default`；语义比参考实现更保守
  （headless 下 Ask 仍会 Deny），但可能让交互式子代理多问一次。
- 父权限规则里的相对路径在隔离子代理中按子代理工作区重新解析；依赖绝对路径的规则
  不受影响，但相对路径规则的语义会变。
- Worktree 隔离依赖外部 `git`；仓库没有 commit、路径含特殊字符、git 版本差异都会
  显式失败。CowClone 的 reflink 依赖文件系统；overlayfs 依赖 root。
- Worktree 会映射父 cwd 在仓库内的相对子目录；如果该子目录只在父工作区以 untracked
  形式存在（没有被 HEAD 跟踪），worktree 里会创建一个空目录，不会自动带入未跟踪文件。
- `capture_delta` 把 baseline 后修改过的 untracked 文件当作“新文件补丁”输出；对
  “baseline 快照 + delta”是正确语义，但对“干净 HEAD + delta”会重建当前内容而不是
  纯增量。
- receipt/usage 的累积假设流式 chunk 是增量；如果 provider 适配层直接传累计 usage，
  会重复计数，需要在适配层转换。
- `AgentSession::subagent_parent_context()` 每次调用都会复制整个可调用工具表；工具
  很多时是 O(n) 快照，但只在创建子代理时调用。
