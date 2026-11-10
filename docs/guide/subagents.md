# 子代理

## 本页讲什么

子代理（subagent）是由父会话派生出的独立代理实例：它有自己的 system prompt、工具集合、transcript、turn/token 预算和终态，运行结束后把结构化结果交回调用方。本页说明 Kiana 当前版本中子代理的定义方式、创建与恢复协议、权限收窄、结果格式、工作区隔离和后台任务边界。

> **当前状态（先读这一节）**：`kiana-session` 已实现子代理运行时（`crates/kiana-session/src/subagent/`）和定义加载器（`crates/kiana-session/src/agent_definition/`），但**没有把子代理暴露成交互式工具或 CLI 子命令**。`kiana --help` 的内建工具说明里没有 `task`、`agent` 或 `subagent` 工具，代码中也没有注册同名内建工具；`--tools` 涉及的 8 个内建工具是 `read`、`bash`、`powershell`、`edit`、`write`、`grep`、`find`、`ls`（`apply_patch` 仅在模型命中配置时替代 `edit`）。因此普通 CLI/TUI 用户当前不能直接派生子代理；本页的“快速开始”面向嵌入 Kiana 的 Rust 宿主或 SDK 调用方。

版本说明：workspace 版本为 `0.1.0`；`kiana --version` 实际输出 `1.4.0`，来自 TS 兼容常量（`crates/kiana-core/src/config.rs` 的 `VERSION`），是有意保留的差异，不是构建错误。

## 快速开始（宿主 API）

当前没有 shell 命令可以创建子代理。下面的流程运行在嵌入 `kiana-session` 的 Rust 程序里。

### 1. 写一个定义文件

```bash
mkdir -p .kiana/agents
cat > .kiana/agents/reviewer.md <<'EOF'
---
description: Review code changes without editing files
tools: [read, grep, find, ls]
permissionMode: read-only
model: inherit
---
You are a read-only code review subagent. Inspect the requested changes,
report concrete problems with file paths, and do not modify files.
EOF
```

### 2. 加载并启动

```rust
use kiana_session::agent_definition::AgentDefinitionLoader;
use kiana_session::subagent::SubagentRuntimeOptions;

let cwd = std::env::current_dir().expect("current dir");
let report = AgentDefinitionLoader::with_default_roots(&cwd).load();
for error in &report.errors {
    eprintln!("agent definition error: {error}");
}

let definition = report
    .get("reviewer")
    .cloned()
    .expect("reviewer agent definition");

let outcome = session
    .spawn_subagent(
        definition,
        SubagentRuntimeOptions::default(),
        "检查 src/ 中的错误处理，列出三个最重要的问题",
        None, // 可选的 CancellationToken
    )
    .await?;

println!("terminal: {:?}", outcome.terminal);
if let Some(result) = outcome.terminal.result {
    println!("{result}");
}
```

只想用内置定义时，可以少写一步加载：

```rust
let outcome = session
    .spawn_named_subagent(
        "general-purpose",
        SubagentRuntimeOptions::default(),
        "总结这个仓库的会话恢复逻辑",
        None,
    )
    .await?;
```

`spawn_named_subagent` 从默认目录（用户、项目、策略，加上内置定义）按名字查找；名字不存在时错误信息会列出可用名字。内置定义始终包含 `general-purpose`、`explore`、`plan`。

## 定义子代理

### 文件位置与覆盖顺序

一个子代理是一份 Markdown 文件：`---` 包围的 frontmatter 是配置，正文是 system prompt。

| 来源 | 位置 | 默认是否加载 | 说明 |
| --- | --- | --- | --- |
| `builtin` | 代码内置 | 总是 | `general-purpose`、`explore`、`plan` |
| `plugin` | 宿主调用 `AgentDefinitionLoader::add_plugin_dir` | 否 | 插件目录 |
| `user` | `<agentDir>/agents`（默认 `~/.kiana/agent/agents`） | 是 | 用户级定义 |
| `project` | `<cwd>/.kiana/agents` | 是 | 项目级定义 |
| `policy` | `/etc/kiana/agents` | 仅 Unix | 系统策略目录 |
| `flag` | 宿主调用 `add_flag_json` | 否 | 代码保留了 `--agents <json>` 形状的库入口（未在 `--help` 中列出，Kiana 核心不解析；可能作为扩展 flag 暂存） |

覆盖优先级固定为 `builtin < plugin < user < project < policy < flag`，与目录添加顺序无关；同名定义以后者为准。

补充：

- `AgentDefinitionLoader::with_default_roots(cwd)` 只加入 `user`、`project`、`policy` 三个目录，并始终包含 `builtin`。
- 默认加载器**不检查项目信任状态**：`<cwd>/.kiana/agents` 只要存在就会被读取。
- 单个文件解析失败只记录到 `AgentDefinitionLoadReport::errors`，不会中止整个目录。
- 单个文件上限 1 MiB。
- 加载器不做文件监听；重新加载需要再次调用 `load()`。

### Markdown 格式

```markdown
---
name: reviewer
description: Review code changes without editing files
tools: [read, grep, find, ls]
disallowedTools: [bash]
model: inherit
effort: medium
permissionMode: read-only
maxTurns: 12
isolation: shared
initialPrompt: Start by reading the requested diff.
omitClaudeMd: true
---
You are a read-only code review subagent. Report concrete problems with file paths.
```

`name` 缺省取文件名（去掉扩展名）；`description` 和正文（system prompt）必填。frontmatter 字段名区分大小写，未知字段直接报错，但只影响该文件。

### frontmatter 字段参考

| 字段 | 类型 | 缺省 | 当前行为 |
| --- | --- | --- | --- |
| `name` | string | 文件名 stem | 定义名；需非空 |
| `id` | string | 无 | Phase 4 profile 身份；只解析与透传给宿主 |
| `revision` | 正整数 | 无 | Phase 4 profile 修订号 |
| `kind` | string | 无 | Phase 4 profile 种类 |
| `capabilities` | string 或 string[] | 空 | Phase 4 能力引用；只解析与透传，重复项报错 |
| `schemaVersion` | 正整数 | 无 | 定义 schema 版本；使用 Phase 4 facet 且声明 1 时报错 |
| `description` | string | 无（必填） | 给父代理看的能力说明 |
| `prompt` | string | 正文 | JSON 定义可直接给 `prompt`；Markdown 正文优先 |
| `tools` | string 或 string[] | 继承父代理全部工具 | allowlist；`*` 等价于全部，空数组表示没有工具 |
| `disallowedTools` | string 或 string[] | 空 | denylist；始终优先于 allowlist |
| `model` | string | `inherit` | `inherit`（大小写不敏感）或模型名；具名模型找不到时显式报错，不回退 |
| `effort` | 枚举或正整数 | 无 | 已解析，但子代理运行时**尚未**把它映射到模型的 thinking level |
| `permissionMode` | string | 继承父模式 | 只能比父代理更严；见下表 |
| `mcpServers` | string 或数组 | 空 | 已解析保存，子代理运行时不挂载 |
| `hooks` | JSON | 无 | 已原样保存，子代理运行时不执行 |
| `skills` | string 或 string[] | 空 | 只在 system prompt 里列出技能名，**不加载技能正文** |
| `maxTurns` | 正整数 | 无 | 超过后写 `failed` + `turn_capped` 并中止 |
| `memory` | `user`/`project`/`local` | 无 | 已解析，子代理运行时不消费记忆 |
| `background` | bool | `false` | 已解析，但不会自动转成后台任务（见“后台任务与通知”） |
| `isolation` | `shared`/`worktree`/`cow-clone` | `shared` | 工作区隔离模式 |
| `initialPrompt` | string | 无 | 子会话 transcript 为空时先写入的文本 |
| `omitClaudeMd` | bool | `false` | 只暴露标志；`spawn_subagent` 没有组装 CLAUDE.md/user context，是否注入由调用方决定 |

接受的值和别名：

| 字段 | 可用值 |
| --- | --- |
| `permissionMode` | `read-only`、`default`、`accept-edits`、`bypass`；另接受 `readonly`/`plan`/`ro`、`ask`、`acceptedits`/`accept`/`workspace-write`/`workspace`、`bypass-permissions`/`danger-full-access` 等别名；`dontAsk`/`auto` 映射到 `default` |
| `isolation` | `shared`/`none`、`worktree`/`git-worktree`、`cow-clone`/`cowclone`/`reflink`/`overlayfs` |
| `background` | YAML 布尔，或字符串 `true`/`false`/`yes`/`no`/`on`/`off`/`1`/`0` |

frontmatter 解析器是手写的 YAML 子集：支持 map、sequence、行内数组、引号、布尔、null、整数、浮点与行内注释；多行折叠（`|`/`>`）、锚点等复杂写法会显式报错。

### 工具选择与命名空间

工具名先被规范化为带命名空间的形式：`builtin:read`、`mcp:<server>:<tool>`、`plugin:<plugin>:<tool>`、`ext:<name>`。模式匹配同时检查规范展示名和原始工具名（裸名字不会跨命名空间匹配同名工具）。

| 写法 | 含义 |
| --- | --- |
| `*` | 全部工具 |
| `read` | 原始名精确匹配；内建 `read` |
| `builtin:*` | 全部内建工具 |
| `mcp:github:*` | 某个 MCP server 的全部工具 |
| `mcp__github__*` | 同上，兼容 `kiana-permissions` 的规则写法 |
| `plugin:ci:*` | 某个插件的全部工具 |

规则：

- `tools` 缺省或 `*` → 继承父代理当前可调用工具表。
- `tools: []` → 没有任何工具，不是“继承全部”。
- `disallowedTools` 在 schema 与 dispatch 两处都检查；即使父规则里有更宽的 `Allow` 也不能把它放回来。
- 父代理工具表已经应用了激活名单、exposure 与 `--exclude-tools` 过滤；子代理只能在这张表内取子集。
- 如果父会话启用了记忆且 `memory` 工具可调用，子代理默认也会继承它，除非被 `tools`/`disallowedTools` 收窄。

### 隔离模式

| 模式 | 实现 | 适用场景 | 限制 |
| --- | --- | --- | --- |
| `shared` | 直接使用父会话目录 | 只读调研、无副作用任务 | 无隔离 |
| `worktree` | `git worktree add --detach` | 并行改代码 | 需要 git 仓库和外部 `git`；不自动带入父工作区未提交改动 |
| `cow-clone` | btrfs/XFS reflink，失败后尝试 overlayfs | 非 git 项目或完整目录克隆 | reflink 依赖文件系统；overlayfs 需要 root/CAP_SYS_ADMIN |

隔离状态目录默认是 `<cwd>/.kiana/subagents`；`IsolationOptions::state_dir` 可覆盖。`shared` 不写 ownership 记录，也不会创建该目录。

隔离不可用时返回 `IsolationError::Unsupported`，**不会静默退化成 `shared`**。`SubagentRuntime` 被丢弃时会清理隔离工作区；进程启动时还会扫描 ownership 记录，清理崩溃遗留的资源。

## 创建与运行协议

### 父上下文快照

`AgentSession::subagent_parent_context()` 构造父上下文：

- `tools`：父会话 `get_callable_tools()` 的不可变快照（已应用激活名单、exposure 与 exclude 过滤）。
- `allowed_tools`：`ToolPermissionSet::all()`；真正的收窄发生在与子代理定义求交时。
- `permission_policy` / `permission_mode`：取当前权限引擎的策略与模式；父会话的“总是允许”记录**不**复制。
- `model`：父会话当前模型；`model: inherit` 使用它。
- `provider_capabilities`：默认全 false；`AgentSession` 当前没有声明 prompt cache 能力。

### 入口方法

| 方法 | 作用 |
| --- | --- |
| `subagent_parent_context()` | 生成父上下文快照 |
| `prepare_subagent(definition, options)` | 建立独立 tool pool、system prompt、权限上下文、token 预算、transcript，可选隔离工作区；**不启动模型循环** |
| `spawn_subagent(definition, options, prompt, cancel)` | `prepare` + `run_subagent`，驱动完整模型循环；返回 `SubagentRunOutcome`，并按 `acceptance_criteria` 自动判定验收；有改动的隔离工作区保留并在 `outcome.isolation` 报告路径 |
| `spawn_named_subagent(name, options, prompt, cancel)` | 从默认目录（含内置定义）按名字加载定义后调用 `spawn_subagent` |
| `resume_subagent(definition, options, session_path)` | 打开已有子会话，复用 transcript；工具与权限按当前父上下文重建 |
| `resume_and_run_subagent(definition, options, session_path, prompt, cancel)` | `resume_subagent` + `run_subagent` |

`resume_subagent` 会校验会话里的 `kiana.subagent.link`：agent 名或父会话 id 不匹配时返回 `ResumeMismatch`；缺少链接条目返回 `MissingLink`。恢复时不会重新创建隔离工作区，而是沿用子会话 header 记录的 cwd；记录的目录已不存在（无改动的隔离运行会被清理）时退回父上下文 cwd，调用方也可以自行重建工作区或改用 `shared`。

### 手动驱动协议

如果宿主想自己控制模型循环，可以只用 `prepare_subagent`：

1. 用 `runtime.system_prompt()` 作为 system prompt。
2. 用 `runtime.tool_schemas()` 交给模型；工具调用通过 `runtime.dispatch_tool(tool_call_id, tool_name, args)` 执行。
3. `dispatch_tool` 的顺序固定为：工具表注册检查 → allow/deny 二次检查 → 权限引擎 → 执行。
4. 流式 chunk 通过 `runtime.record_chunk(...)` 累积 usage/receipt；结束时调用 `try_set_terminal(...)`。

直接调用工具实现会绕过 allow/deny 与权限引擎，因此宿主应始终走 `dispatch_tool`。

### 结果契约

`SubagentRunOutcome` 的字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `terminal` | `SubagentTerminalState` | 终态、`stop_reason`、结果/错误文本、usage、`seq` |
| `messages` | `AgentMessage[]` | 运行结束时的完整子会话 transcript |
| `usage` | `SubagentUsage` | 累计输入/输出/缓存 token 与费用 |
| `receipts` | `ToolReceipt[]` | 每次工具执行的参数/输出 sha256、状态与稳定序号 |
| `output` | `JsonValue?` | 配置了 yield 契约时的装配输出 |
| `acceptance` | `AcceptanceReport?` | 终态验收报告；`spawn_subagent` 会按 options 自动填充 |
| `session_file` | `string?` | 持久化子会话路径；内存会话为 `None` |

`terminal` 取值：`completed`、`failed`、`cancelled`、`timed_out`、`polling_timed_out`。

`stop_reason` 与终态正交，取值：`token_capped`、`turn_capped`、`loop_capped`、`model_length_capped`、`error`。例如 token 预算耗尽时终态是 `failed`，`stop_reason` 是 `token_capped`。

成功结果的文本按以下优先级产生：yield 装配输出的 JSON 字符串 → 最后一条 assistant 文本 → `"completed"`。

`acceptance` 支持 `file:<path> exists`、`file:<path> non-empty`、`file:<path> json-valid`、`file:<path> csv-columns:[...]`、`file_written:<path>`、`tests_passed:<command>` 等 criterion；无法用代码判定的条目会记为 `unverified`，criteria 为空时报告也是显式的 `unverified`，不会静默通过。

终态槽只允许写入一次（`TerminalCell::try_set_terminal`）：完成、超时、取消同时发生时先到者赢，后到者不会覆盖，`seq` 从 1 开始。

子代理**不会自动把结果写回父会话**：`spawn_subagent` 只返回 `SubagentRunOutcome`，由调用方决定如何展示或持久化。子会话自身带 `parentSession` header 和 `kiana.subagent.link` 自定义条目；父会话侧的 `kiana.subagent.spawned` 记录需要显式调用 `record_spawn_in_parent`（当前生产入口没有调用它）。

### yield 工具

如果 `SubagentRuntimeOptions.yield_contract` 存在，运行时会额外注册一个名为 `yield` 的工具：

- payload 形状：`{"label":"summary","value":...}`、`{"labels":{"summary":...}}`，或直接 `{"summary":...}`。
- 同一 label 的数组增量 append；标量和对象后提交者覆盖。
- `Strict` 模式下类型变化或 schema 校验失败会报错；`Permissive` 模式记录 warning 后接受。
- 失败重试上限默认 2 次；超过后 `exhausted=true`，调用方应把子代理置为失败，而不是把不完整结果当成功。
- 失败的提交不会写进装配状态：下一次提交从上一次成功的 label 继续，不会把错误内容重复 append。
- 模型结束回合时 runner 会调用最终校验（`finish()`）：`required` 字段缺失或 schema 不合法时子代理终态为 `failed`，不会把半成品当成功。只区分“从未调用 yield”（回退到最后一条 assistant 文本）与“调用过但结果不完整”（判失败）。
- 最终装配结果放入 `SubagentRunOutcome.output`，并序列化进 `terminal.result`（文本走 `kiana-json`，与 `JSON.stringify` 的数字/键序一致）。

## 权限继承与收窄

子代理的权限在三个层次上做“只能收窄”的投影：

1. **工具集合**：有效集合 = 父代理可调用工具表 ∩ 父允许集合 ∩ 子代理 `tools`，再减去 `disallowedTools`。
2. **显式 denylist**：`disallowedTools` 在 schema 与 dispatch 两处都检查，先于权限引擎规则。
3. **权限引擎**：复用父策略的规则与工作区，但模式取 `min(parent, requested)`。严格度顺序为 `read-only < default < accept-edits < bypass`。

关键行为：

- 父会话的“总是允许”（session allow）**不继承**；父代理批准过的操作不会泄漏给子代理。
- headless 子代理没有交互 UI：权限引擎返回 `Ask` 时一律转换成 `Deny`，不会触及工具实现。
- 交互式审批模式（`SubagentApprovalMode::Interactive`）会把 `Ask` 返回为 `NeedsApproval`；但当前 `run_subagent` 会把它变成 failed tool result，交互式审批卡尚未接线。
- 隔离模式下，权限策略的 `workspace_roots` 会被替换为子代理工作区；相对路径规则按子代理 cwd 重新解析，依赖绝对路径的规则不受影响。
- 父会话工具注册表里的 MCP annotations 会随父上下文快照传给子代理：`destructiveHint`/`openWorldHint` 仍强制 `Ask`（headless 下转 `Deny`），`readOnlyHint` 只在 read-only 模式放宽；显式 deny 规则优先于任何 hint。
- **已知限制**：扩展工具在子代理内通过 `ctx.executeTool` 发起的嵌套调用仍走父会话的工具管线与权限引擎（桥接在 `SessionNestedHost` 上），而不是子代理收窄后的权限上下文。扩展不应把嵌套调用当作子代理权限的边界；要收敛到子代理上下文需要重新设计 `kiana-extensions` / `kiana-session-core` 的桥接，目前未实现。

## 并发、取消与队列

- Kiana **没有**为子代理提供内建并发上限、排队、重试或公平调度。每个 `SubagentRuntime` 是独立实例，宿主可以并发驱动多个；`sidechain_id` 用来区分同一父会话下的并行子代理。
- `SubagentRuntimeOptions.depth` 是元数据，直接子代理为 1；代码没有强制最大嵌套深度，也没有把子代理创建能力放进子代理工具池，因此运行时不会自动产生“孙代理”。
- 取消通过 `CancellationToken` 传入 `spawn_subagent` / `run_subagent`；触发后中止当前 run，终态为 `cancelled`。
- `maxTurns` 在每个 turn 开始时计数，超过后写 `failed` + `turn_capped` 并中止。
- token 预算按输入、输出、缓存读、缓存写四项累计；预算耗尽写 `failed` + `token_capped`，并中止 run。
- 没有自动超时定时器：`SubagentRuntime::timeout()` 和 `timed_out` / `polling_timed_out` 终态存在，但需要宿主或后台任务层显式调用。
- `fork` 字段当前只参与 prompt cache 共享判定，不会把父 transcript 前缀复制进子会话。prompt cache 共享还要求显式 `share_prompt_cache=true` 且 provider 声明 `prompt_cache=true`；`AgentSession` 构造的父上下文没有声明该能力，因此通过 `spawn_subagent` 实际不会开启。
- 子代理会话默认是内存会话；只有设置 `SubagentRuntimeOptions.session_dir` 才会落盘。落盘后才能用 `resume_subagent` 恢复。

## 工作区隔离与清理

- `worktree` 使用 `git worktree add --detach` 创建独立工作树；如果父 cwd 位于仓库子目录，子代理工作区会映射到 worktree 内对应的相对子目录。
- `cow-clone` 先尝试逐文件 `FICLONE`（btrfs/XFS reflink），失败后尝试 overlayfs。两者都不可用时显式返回 `Unsupported`。克隆目录位于仓库内时，父 cwd 必须是 git 仓库顶层：子目录克隆里没有 `.git`，子代理的 git 命令会向上发现原仓库并改写父 checkout，因此显式拒绝（改用 `worktree` 或从仓库根运行）。linked worktree / submodule 的 `.git` 是 `gitdir:` 指针文件，克隆无法把它物化成独立仓库，同样显式拒绝。
- 隔离能力还包括 baseline/delta：`capture_baseline` 记录 HEAD、staged、unstaged、untracked 与 nested repo；`capture_delta` 先在临时 index 上重建 baseline 快照（HEAD + staged + unstaged + untracked），再以 index→worktree 生成可被 `git apply --binary` 重放的补丁（untracked 文件的 mode-only 变化也会生成 mode 补丁，应用后按 `new mode` 复核权限）。因此脏父工作区的既有改动不会混进子代理的 delta，baseline 时的 untracked 文件被修改/删除或 `git add` 成 tracked 都表现为修改/删除，而不是“新建”；`apply_delta` 负责应用，且要求传入的根就是 delta 捕获时的仓库根（在子目录应用会被拒绝，避免 `git apply` 静默跳过目录外路径）。`create_workspace` 不会自动把父工作区的未提交改动 seed 进 worktree。baseline 会序列化成 JSON，无法表示非 UTF-8 文件名：这类路径在 baseline 里以 `skipped` 报告，delta 遇到时显式返回 `DeltaUnsupported`，不会静默漏掉改动；因大小/类型被 baseline 跳过的路径同样无法生成可应用到 baseline 快照的补丁，delta 会显式失败。delta 只覆盖 git 跟踪与 untracked（非 ignored）文件：写进 `.gitignore` 的交付物要靠保留工作区取回，不会出现在 `capture_delta` 里。
- 生产入口 `spawn_subagent` 的收尾策略与参照实现一致：终态后**有改动就保留**工作区（ownership 记录标记 `retained`，路径写在 `outcome.isolation`，可继续 `resume_subagent`，或由调用方自行 `capture_delta`/`apply_delta` 合并），**无改动才清理**。不会自动 apply delta：父工作区的未提交改动、cow-clone 的脏拷贝与嵌套仓库都无法可靠表示成一条可重放补丁。`retained` 记录不会被启动 sweep 回收；需要显式 `IsolatedWorkspace::cleanup`。有改动但 ownership 记录写入失败时，`outcome.isolation.retain_error` 会携带错误且 `retained=false`：本进程仍靠 `Drop` 守卫保住工作区，但下一个进程的 sweep 可能回收它，宿主必须区分这与“无改动”。`cow-clone`/`overlayfs` 复制的是父工作区当前状态，无法区分父改动与子代理改动，因此一律按“有改动”保留（宁可多留一个工作区，也不丢产物）。
- ownership 记录写在 `<state_dir>/ownership/<id>.json`；`Drop` 会清理工作区，启动时 `sweep_ownership_records` 清理崩溃遗留记录。加载记录时会校验路径在状态目录内（必须是严格后代）且符合本记录自己的 `<state>/worktrees|clones/<id>` 布局，拒绝被篡改的记录诱导删除其它会话的工作区；worktree 的 git 操作只对注册在该仓库里的 worktree 生效，伪造的 `gitRoot` 不会删除任意仓库的分支。记录里的 pid 是本进程时 sweep 跳过（同一进程里的 `/new`、`/resume` 可能还有别的会话在用），崩溃残留由下一次进程启动回收。`resume_subagent` 恢复上次的隔离工作区时会认领记录（pid 刷新为当前进程）并接管工作区句柄：认领前先 canonicalize 会话头里的路径，记录指向的工作区必须真实位于声明根之内；若记录仍被另一个存活进程持有（pid 不同且进程存在）则拒绝认领。有改动的恢复运行照常标记 `retained`，接管的工作区不会在提前失败时被 `Drop` 删除。
- 平台支持是 Linux 优先；overlayfs 需要 root/CAP_SYS_ADMIN，当前未实测（无特权环境会明确返回 `Unsupported`）。

## 后台任务与通知

子代理与后台任务共享同一套 journal / 通知机制；Phase 1c 提供显式的后台 Work 入口：

- 定义里的 `background: true` 只被解析保存；`spawn_subagent` 不会因此把子代理变成后台任务。后台执行需要宿主显式调用下面的后台 Work 入口。
- 真正可用的后台执行入口是 `bash` 工具的 `background: true` 参数，并且需要启用 continuity：
  - settings 中设置 `"continuity": true` 或 `{"enabled":true,"background":true,...}`（该键不在 settings schema 的 58 个类型化键中，由运行时读取）；
  - 或设置 `KIANA_CONTINUITY=1`，用 `KIANA_CONTINUITY_BACKGROUND=0` 单独关闭后台能力。
- 启用后 `bash` 的 JSON Schema 会增加可选的 `background` 属性。工具会立即返回 `task_<uuid>` 与输出文件路径，任务在 manager 拥有的 tokio task 中继续，父 turn 结束不会取消。
- 输出文件位于 `<continuity state dir>/background/<task_id>.output`，默认状态目录是 `<agentDir>/continuity`；单文件硬上限 8 MiB，使用 `O_NOFOLLOW` 打开，超限后截断并置 `outputTruncated`。
- 终态通知在下一轮 `run` 开始时作为 `kiana.background-task` 自定义消息注入父会话，包含 `taskId`、`terminalSeq`、`status`、`reason`、`exitCode`、`outputPath`、`outputBytes`、`outputTruncated`。
- 后台 Work：`AgentSession::spawn_subagent_with_work_in_background(...)` /
  `spawn_named_subagent_with_work_in_background(...)` 在会话线程的常驻 `LocalSet`
  上运行 `!Send` 子代理，立即返回
  `BackgroundWorkHandle { taskId, workId, runId, outputPath }`。需要 continuity 的
  `background` 与 durable work 存储（`work`）；未显式传 `store` 时注入会话的
  durable 账本。Work 记录是权威状态，后台记录带 `workBacked` 标记；启动时
  `recover_work()` 先对账，再跑 background sweep，通用 sweep 不会落定
  `workBacked` 记录（只能由所属会话的 `recover_work` 对账）。
- 取消后台任务使用 `AgentSession::cancel_background_task(task_id)`；这是宿主 API，
  不是模型可见工具。`start` 启动的 bash 任务会 abort 并立即抢占终态；后台 Work
  只传播 `CancellationToken`（协作式），终态由 Work 生命周期写出后映射。
- 当前限制：journal 只做记录与一次性 CAS，**不是队列**；通知只投递到当前进程，跨进程投递未接线；`BackgroundTaskResumer` 没有生产实现，进程崩溃后启动 sweep 会把仍为 `Running` 的普通记录置为 `interrupted/process_lost`，不会收养或重跑子进程。`workBacked` 记录例外：sweep 保持它 Running，等所属会话启动时由 `recover_work` 对账；若所属会话不再打开，这条 attempt 记录会一直停在 Running（fail-closed，不会写错终态）。

## Work envelope 与崩溃恢复

Work 是宿主 opt-in 的结构化委派协议（`docs/adr/work-orchestration.md` Phase 1a）；默认 `spawn_subagent` 路径完全不变。

- 账本：`AgentSession::durable_work_store()` 在 continuity 启用且 `work` 未关闭时返回 `Arc<DurableWorkStore>`。每个父会话一个 `kiana.work_records` durable 文档，记录只有 `running` / `terminal` 两态。
- 注入：`WorkOptions.store` 被 `spawn_subagent_with_work` / `spawn_named_subagent_with_work` 与两个后台入口使用；不传时 Work 事件与结果不落盘（后台入口例外：它要求 durable 账本，缺省会自动注入）。
- 写前记录：运行前 `begin` 写入 Running（同一 workId 重复 begin 会在启动前拒绝），随后 `attach_session` 补齐持久会话文件；结束用一次 CAS `finish` 落定终态，只有赢家写入结果。
- 崩溃恢复：`AgentSession::recover_work()` 扫描本会话 Running 记录。默认 `recovery = Fail` 把 pid 已死的记录落定为 `Failed { stopReason: "interrupted" }` 并保留 `sessionFile`；`Recovery::Resume` 不自动重放，只把记录放进 `resumable`（Phase 1b 的 `resume_work` 由宿主显式调用）。只有 `pid == 当前进程` 且命中本进程活 Work 登记表的记录进入 `skipped_live`；其它 pid（包括仍存活的其它进程）不视为运行中——能读到账本已证明本进程持有会话目录租约，而 `kill(pid, 0)` 会因 pid 复用把崩溃记录永久误判为存活。
- 锚点：只有 `Success`，或“仍可被 `resume_work` 接受的 interrupted 记录”
  （`resumable && sessionFile`，即 lineage tip）保留 `resumedFrom` 占用；其余终态
  在 `finish` 成功后归还。`recover_work` 在落定终态后逐层修复，把非 tip 终态的
  占用全部释放，因此一条全崩溃的恢复链（W1←W2←W3）只有 tip 可恢复，原记录也不会
  永久不可重试。
- 后台 Work 对账：`recover_work` 用 `reconcile_background_work` 把 Work 的权威
  终态写回带 `backgroundTaskId` 的 attempt（`finish_task` 幂等；崩溃恢复落定的
  interrupted 映射为 task `interrupted/process_lost`）。`WorkRunOutcome.store_error`
  非空时后台入口会重读账本，只有本 run 的终态确实落账才发布 Completed。
- 图投影：`AgentSession::work_graph()` 从本会话 durable 账本重建 `WorkGraph`
  （节点 = WorkRecord，边 = `resumedFrom`）；`work_graph_set_view()` 按父会话分组
  返回跨会话集合。`runnable()` 标出 Running 记录与仍可恢复的 lineage tip，不启动
  任何 Work；编码把 `WorkId` 写成十进制字符串。
- 事件：`WorkEvent` 是提交后的投影——`Created` / `Started` 在 `begin` 成功后，
  `AcceptanceEvaluated` / `Terminal` 在 `finish` CAS 成功后；每个事件携带
  `planId`（`WorkOptions.plan_id`）。事件是 best-effort，权威状态仍是 WorkRecord。
- 遥测：每次账本提交后产生 `kiana.work.commit` span（operation/id/runId/
  parentSessionId/duration/status），不改变既有 agent-run/turn span。
- 报告：`WorkRecoveryReport { terminalized, resumable, skipped_live }`。
- 宿主应在会话启动时调用 `recover_work()`；它不是 `run` 的隐式步骤。

## Context Shadow（Phase 4）

`AgentSession::context_shadow(options)` 复用 `context_report` 捕获只读快照：
总 token、利用率、工具/prompt/turn-context/消息分段，以及两次快照的 `diff`。
它不修改消息、工具或压缩状态，`is_enforced()` 恒为 `false`。Enforce 尚未实现：
需要先补 `peek` API 并通过逐字节门禁。

## 限制与排查

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| 在 TUI/CLI 里找不到创建子代理的工具或命令 | 当前版本没有把子代理暴露成工具/子命令 | 使用宿主 API；不要依赖 `--tools` 里的 `task`/`agent` |
| `kiana --agents ...` 没有加载子代理 | Kiana 核心不解析 `--agents`；只有 `AgentDefinitionLoader::add_flag_json` 这个库入口 | 改用 Markdown 定义文件，或由宿主调用库 API；不要依赖 `--agents` |
| 定义文件没有被加载 | 加载器只在宿主调用 `load()`/`spawn_named_subagent` 时读取；项目目录也不受信任检查影响 | 确认路径是 `<agentDir>/agents` 或 `<cwd>/.kiana/agents`；检查 `AgentDefinitionLoadReport::errors` |
| 定义文件报 unknown frontmatter field | 解析器只接受上表字段，且大小写敏感 | 删除或改正字段名；多行 `|`/`>` 也要改成单行/行内写法 |
| `spawn_named_subagent` 报 unknown subagent | 名字不存在或来源目录没有加载 | 错误信息会列出可用名字；内置 `general-purpose`/`explore`/`plan` 始终可用 |
| 子代理没有工具或工具比预期少 | 有效工具 = 父工具表 ∩ `tools` − `disallowedTools`；`tools: []` 是空集 | 检查父会话的 `--tools`/`--exclude-tools`、exposure 和子代理定义 |
| 子代理工具调用被拒绝 | 父/子权限模式取更严者；headless 下 `Ask` → `Deny` | 提高父模式或调整规则；不要把 `bypass` 给不可信子代理 |
| 交互式审批没有弹卡 | `run_subagent` 把 `NeedsApproval` 变成 failed tool result | 当前限制；需要审批的场景改用宿主自己的工具循环 |
| worktree 隔离报 Unsupported | 不在 git 仓库、没有 commit、外部 `git` 不可用、状态目录不可写 | 先提交一个基线 commit；或改用 `shared`/`cow-clone` |
| worktree 里看不到父工作区的未提交改动 | `git worktree add` 从 HEAD 创建，不 seed 父 delta | 先提交，或由调用方用 `capture_baseline`/`capture_delta` 自行叠加 |
| overlayfs 报 Unsupported | 需要 root/CAP_SYS_ADMIN；当前只在 Linux 验证 | 用 `cow-clone` 的 reflink 路径，或改用 `worktree`/`shared` |
| resume 报 MissingLink / ResumeMismatch | 子会话没有 `kiana.subagent.link`，或 agent/父会话不匹配 | 只恢复由本运行时创建的会话；默认内存会话没有文件，需创建时设置 `session_dir` |
| resume 后工作目录不存在 | 恢复不重建隔离工作区；记录目录不存在时退回父 cwd | 需要继续在隔离工作区工作时自行重建，或改用 `shared` 后重新运行 |
| 隔离子代理的文件验收结果不符合预期 | `spawn_subagent` 的自动验收使用父会话 cwd 作为文件检查根目录 | 宿主自行用子代理工作区调用 `evaluate_acceptance`，不要依赖自动验收的相对路径 |
| 真实模型下行为与测试不同 | 现有测试使用 faux provider；真实 provider 未做端到端冒烟 | 先在测试环境验证模型解析、usage 与 stop reason；平台目前只验证 Linux x86_64 |

本页事实核对自 `crates/kiana-session/src/subagent/`、`crates/kiana-session/src/agent_definition/mod.rs`、`crates/kiana-session/src/agent_session.rs`（`spawn_subagent` / `spawn_named_subagent` / `resume_subagent`）、`crates/kiana-session/src/background/` 与 `crates/kiana-session/src/continuity.rs`。`effort`、`mcpServers`、`hooks`、`skills` 正文、`memory`、`background`、`omitClaudeMd` 的“已解析但未消费”状态来自上述源码，不是推测。

## 相关页面

- [权限与审批](permissions.md)
- [设置（settings）](settings.md)
- [内置工具](tools.md)
- [会话、分支与恢复](sessions.md)
- [持久记忆](memory.md)
- [故障排查](troubleshooting.md)
