# Kiana 抄作业计划

> 这份计划是给维护者照着干活的，不是架构论文。
>
> 前提：与旧 TypeScript 参照实现“行为完全一致”的要求已经作废。Kiana 可以直接换掉不好的设计，只保留还有用户价值的历史会话/配置格式。
>
> 时间估算按 **1 个人、每周 5 个工作日** 来写。标了 L 的工作通常不止一周；如果只有你一个人，把“第几周”当成顺序号，不要当死线。两个人可以并行的部分我会单独标出来。

---

## 一、先说人话：这份计划要干什么

Kiana 现在不是一个空架子。30 个 crate 里，agent loop、模型接入、工具系统、会话落盘、压缩、MCP、扩展运行时、TUI 都已经能跑，测试也不少。真正的问题是三个：

1. **安全相关的功能只做了一半。** 权限模块有了判定逻辑，但只能靠环境变量打开；审批只有“是/否”；`execute_bash` 和 PowerShell 能绕过沙箱；文件工具（read/write/edit）根本不经过沙箱。
2. **几个关键能力还没有。** 没有 hooks，没有跨会话记忆，没有子代理，没有 checkpoint/rewind，没有 OTel 导出，评测也还是旧的那套。
3. **有些东西做出来了但没接上。** `kiana-durable` 很大、功能不少，但主程序没用它；`kiana-chord` 还有占位模块；文档里写的 crate 数量还是 28/29。

所以这份计划的顺序不是“哪个功能酷先做哪个”，而是：

> **先把安全底座收口，让所有执行路径都走同一个判断；再补 hooks/MCP/skills 这些可扩展面；然后解决上下文、编辑和评测；最后做记忆、子代理、恢复和界面。**

原因很简单：安全底座不收口，后面每加一个工具、一个 MCP server、一个子代理，都会多一条绕过路径。先把这个洞堵上，后面的功能才敢默认打开。

---

## 二、Kiana 现在到底什么能用、什么不能用

### 已经能用的

- **Agent 循环**：`kiana-agent-core` 有完整的状态机，支持流式、工具调用、steering、follow-up、压缩边界。
- **模型接入**：`kiana-ai` 有 42 个 provider 适配，认证、模型目录、虚拟模型路由都在。
- **会话**：`kiana-session-core` 是 append-only 的 JSONL 会话树，支持 fork/branch；`kiana-session` 负责把各层组装起来。
- **工具**：read/write/edit/bash/grep/find 都有；edit 支持精确匹配、模糊匹配、unified patch，同一个文件的并发写入有锁。
- **MCP**：client、stdio/HTTP 传输、OAuth、分页、resources 都有，测试比较全。
- **扩展**：rquickjs + oxc，41 类事件，热重载、内存上限、project trust 都有。
- **TUI**：主屏行差分、备用屏、overlay、CJK、图片、diff 预览、会话选择器都在。
- **沙箱**：`kiana-sandbox` 已经有 bubblewrap 和 Landlock 两个后端，三档策略（只读/工作区可写/全放行），后端不可用时会拒绝执行。
- **权限**：`kiana-permissions` 有四档模式、工具/命令前缀/路径规则、灾难性命令拦截，并且已经接进了 `before_tool_call`。

### 还不够的

- 权限只能通过 `KIANA_PERMISSIONS` 环境变量打开，settings、CLI、SDK、slash command 都没有入口。
- 审批只有 Yes/No。没有“只允许这一次 / 本会话都允许 / 写到项目配置 / 拒绝并说明原因”，也没有审批事件和审计记录。
- print/json/RPC 没有审批通道。没有 UI 时，所有 Ask 直接拒绝，CI 里也没法用 `--yes`。
- 只有 bash 工具会在某些条件下自动套沙箱。`AgentSession::execute_bash`、PowerShell、文件工具都能绕过。
- 沙箱只返回一个 `tokio::Child`，分不清“命令真的执行了”“沙箱启动失败”“relay 被杀了”。调用方会把启动失败当成普通退出码。
- 配置只有 global 和 project 两个作用域。没有分层合并、没有“这个值到底从哪来的”、没有 managed policy。
- 没有 lifecycle hook。用户没法用 shell/HTTP 在工具调用前后做策略、审计、格式化或注入上下文。
- MCP 没有 prompts→slash command、没有 per-server 工具过滤、没有把 annotations 变成权限提示、没有子进程环境变量白名单。
- Skills 只有名字、描述、路径。没有 allowed-tools、没有 paths 条件激活、没有 1% 上下文预算、没有 skill 工具。
- 压缩只有“调模型做全量摘要”。没有工具结果裁剪、没有不依赖模型的兜底、失败时可能留下半截历史。
- 工具输出只有截断。大输出没有落盘、没有 artifact handle、没有结构化 JSON 摘要，截断就真丢了。
- 编辑没有 read-before-write。读完之后文件被别人改了，edit 还是会按旧内容去改。
- Provider 没有能力门、没有统一的输出预算、没有结构化的错误分类（认证/限流/上下文超限/配额）。
- Agent loop 没有 max turns/token/cost/wall-clock，没有重复调用检测和停滞检测。
- 没有持久记忆、子代理、checkpoint/rewind、会话搜索。
- 遥测只有内存实现，没有 OTel 导出；评测还是旧文档 A/B，没有原生任务集。

---

## 三、抄谁、抄什么、放到哪

只看设计，不复制代码。下面每一条都标了“主要抄谁”和“落到哪个 crate”。

| 要做的事 | 主要抄谁 | 具体抄什么 | 落到哪 |
|---|---|---|---|
| 权限模式、规则、审批 | Claude Code、Codex、gemini-cli、kilocode、goose | 四档模式、deny>ask>allow、规则作用域、Allow once/always、审批事件 | `kiana-permissions`、`kiana-session`、`kiana-config` |
| 沙箱与执行策略 | Codex、qwen-code、kilocode、OpenManus | 统一 execution policy、沙箱结果状态、relay、文件写入门禁、read-before-write | `kiana-sandbox`、`kiana-tools`、`kiana-session` |
| 配置分层 | Codex、Claude Code、SWE-agent | 8 层配置栈、逐键来源、managed policy、严格诊断 | `kiana-config` |
| shell 风险分析 | Claude Code、Codex、gemini-cli、qwen-code | 子命令拆分、重定向/替换/heredoc 识别、prefix 规则只对简单命令、规则建议 | `kiana-permissions`、`kiana-tools` |
| Hooks | Claude Code、qwen-code、Codex、gemini-cli | 事件×matcher、command/http handler、exit 2 阻塞、JSON 输出、trust、SSRF | 新 crate `kiana-hooks` |
| MCP 管理面 | gemini-cli、OpenManus、cline、Codex | prompts→slash、per-server 过滤、annotations→权限、manager 状态、env 白名单、OAuth issuer | `kiana-mcp`、`kiana-builtin-extensions`、`kiana-config` |
| Skills | Claude Code、qwen-code、gemini-cli、goose | frontmatter 扩展、allowed-tools、paths 条件激活、context:fork、1% 预算 | `kiana-resources`、`kiana-ext-api`、`kiana-session` |
| 上下文压缩 | oh-my-pi、Claude Code、cline、qwen-code | 工具结果裁剪、all-or-nothing 压缩、no-LLM 兜底、PTL 重试、压缩后恢复 | `kiana-compaction`、`kiana-tools`、`kiana-session` |
| 工具输出预算 | Claude Code、oh-my-pi、cline | 45k 落盘/40k preview、artifact store、JSON outline、聚合预算 | `kiana-tools`、`kiana-session` |
| 编辑与 apply_patch | aider、Codex、gemini-cli、Claude Code | 结构化编辑错误、多 edit 原子、apply_patch、流式预览 | `kiana-tools` |
| Provider 请求策略 | cline、SWE-agent、goose、opencode | 能力门、输出预算公式、错误分类、输入预检、cache 语义 | `kiana-ai`、`kiana-config` |
| OTel 遥测 | Codex、gemini-cli、oh-my-pi、goose | OTLP exporter、trace 传播、run summary、内容捕获开关 | `kiana-telemetry`、`kiana-agent-core`、`kiana-session` |
| 原生评测 | aider、SWE-agent、qwen-code、deer-flow、goose | 任务集、mock provider、pass@k、loop guard、context report | `kiana-evals`、`kiana-agent-core` |
| 持久记忆 | Claude Code、hermes-agent、qwen-code、kilocode、AutoGPT | MEMORY.md 索引、typed 主题、召回、后台抽取、脱敏/遗忘 | 新 crate `kiana-memory` |
| 子代理 | Claude Code、gemini-cli、qwen-code、goose、oh-my-pi、deer-flow | AgentDefinition、权限收窄、worktree 隔离、receipt、后台任务 | `kiana-session`、`kiana-durable`、`kiana-tools` |
| 会话恢复/checkpoint | qwen-code、deer-flow、opencode、hermes-agent、kilocode、cline | 恢复分类、dangling tool_call、执行 claim、私有 git checkpoint | `kiana-session`、`kiana-session-core` |
| TUI 审批与帧契约 | oh-my-pi、Codex、Claude Code、gemini-cli、OpenHands、qwen-code | FrameProvider、审批卡、modal 仲裁、i18n | `kiana-tui`、`kiana-interactive` |
| Web/编辑器/daemon | OpenManus、cline、qwen-code、hermes-agent、opencode | 搜索引擎链、SSRF、IDE lockfile、ACP、SSE/WS、cron | `kiana-builtin-extensions`、`kiana-mcp`、`kiana-rpc-print` |

### 明确不抄的

- **不抄代码，只抄设计。** Claude Code 是从 npm source map 还原的非官方树，根目录没有 LICENSE；AutoGPT 的 `autogpt_platform` 是 Polyform Shield；oh-my-pi 的 `pi-shell`/`pi-builtins` 含 GNU bash GPL 来源。Apache-2.0 项目如果复用实现，要保留 NOTICE/SPDX 和版权头。
- **不抄 god file。** `REPL.tsx` 876KB、`AgentTool.tsx` 228KB，Kiana 自己的 `agent_session.rs` 已经 390KB、`session_manager.rs` 6.4K 行，不能再往里塞。
- **不抄产品化外围。** cloud/account/billing/marketplace、Electron/VS Code webview、品牌资产、Statsig/Cleartop/PostHog 默认上报，都不要。
- **不抄危险默认值。** yolo/auto-approve 默认、aider 的默认 auto-commit/改写 git 身份、无条件 `git reset --hard`、把宿主 bash 当沙箱、把 MCP annotations 当授权。
- **不抄上游 prompt 原文。** Claude Code / oh-my-pi / aider / qwen 的 system/agent/compaction/memory prompt 是针对特定模型调优的，Kiana 要按自己的模型和工具重写。

---

## 四、逐周计划

### 第 0 周（3 天）：先止血

**这周要解决什么**

先把最明显的两个洞堵上：权限只能靠环境变量打开，以及 `execute_bash`/PowerShell 能绕过权限和沙箱。顺便把 `kiana-durable` 的定位写成 ADR，别让后面的人继续猜。

**具体做什么**

1. 在 `Core::new` 里把 `PermissionEngine::from_env(cwd)` 换成 `from_settings_and_flags(...)`。先支持三样：settings 里的 `permissions.mode`/`permissions.rules`、CLI 的 `--permission-mode`/`--allow`/`--deny`/`--add-dir`、环境变量兜底。
2. 在 `AgentSession` 上加 `set_permission_mode` 和 `set_permission_rules`，让运行期能切模式、加规则。
3. 在 `AgentSession::execute_bash` 执行前调用和 `permission_before_tool_call` 相同的判断；read-only 直接拒绝，default 走 Ask，bypass 放行。
4. PowerShell 不要再传“可以绕过沙箱”的显式 operations；如果暂时做不到，就在入口加同样的 policy 检查。
5. 写 `docs/adr/durable.md`：`kiana-durable` 是未来后台任务/子代理/cron 的唯一调度和事务存储底座；`kiana-session` 只保留前台会话。顺便把 `ARCHITECTURE.md` 的 crate 数量改成 30，把 `kiana-permissions`/`kiana-sandbox` 加进层表。

**做完的标准**

- `cargo test -p kiana-permissions -p kiana-session -p kiana-sandbox` 全绿。
- 手动跑：read-only 模式下 `execute_bash` 被拒绝；default 模式下会问；bypass 放行。
- print/json 模式遇到 Ask 直接拒绝，进程不挂起。
- `ARCHITECTURE.md` 和 `README` 不再写 28/29 个 crate。

**可能卡住的地方**

- `AgentSession::execute_bash` 不在 `before_tool_call` 路径上，要单独接。别为了省事在 bash 工具里再写一套判断，最后一定会分叉。
- PowerShell 的 operations 参数是现有 API 的一部分，直接删可能影响测试；先加 policy 检查，再考虑删。

---

### 第 1 周：权限真正闭环

**这周要解决什么**

把“是/否”升级成能用的审批流程：用户可以选择只允许这一次、本会话都允许、写到项目配置、或者拒绝并说明原因。审批结果要能持久化，重启后仍然生效。

**具体做什么**

1. 定义审批结果：`AllowOnce`、`AllowAlways { scope: Session | Local | Project }`、`Reject { reason }`。
2. 把 `ExtensionUiContext::confirm` 的调用点改成结构化审批。TUI 先做基础版：显示工具名、命令/路径、风险原因，三个按钮（只允许一次/总是允许/拒绝）。diff 预览和更漂亮的卡片放到第 12 周。
3. `AllowAlways` 生成一条权限规则：Session 只存内存；Local 写到项目本地配置；Project 写到项目配置。写回 settings 时只改目标字段，其他字段和键序保持原样。
4. 发 `permission_requested`、`permission_decided`、`permission_denied` 事件。payload 至少包含：工具名、参数摘要（不要放 secret 明文）、决策原因（命中哪条规则/哪个模式/哪个 safety check）、目标（路径/域名/命令前缀）。
5. 加 `crates/kiana-session/tests/permissions_integration.rs` 和 `crates/kiana-coding-agent/tests/permission_cli.rs`。覆盖：deny 阻止、TUI 批准后放行、会话记忆、headless 直接拒绝、模式切换立即生效、execute_bash/PowerShell 和 bash 工具结论一致。

**做完的标准**

- 在 TUI 里选“只允许一次”，下一次同样的调用还会问；选“本会话都允许”，下一次不再问；选“写到项目配置”，重启后仍然不问。
- 拒绝时能把原因回传给模型，模型能看到“用户拒绝了，因为……”。
- 事件流里能看到完整的决策链；审计日志里没有 secret 明文。
- 同一个请求不会弹两次。

**可能卡住的地方**

- “总是允许”不能覆盖 deny。deny 永远优先，否则就是提权漏洞。
- 会话批准的 key 不能太粗（`bash:*` 会把 `rm -rf /` 也放过去），也不能太细（每次命令都问）。简单命令按前两个词记，复合命令按完整命令记。
- 写 settings 的时候，如果文件被外部改过，不要整文件覆盖；保留未修改字段和键序。

---

### 第 2 周：所有执行路径都走沙箱 + 读后写

**这周要解决什么**

现在只有 bash 工具在特定条件下会自动套沙箱。这周要做到：不管命令是从 bash、`execute_bash`、PowerShell 还是 MCP 子进程来的，都走同一个执行策略；文件工具写文件之前，也要检查路径是不是允许写。

**具体做什么**

1. 在 `kiana-sandbox` 加一个统一入口：`ExecutionPolicy::resolve(tool_name, input) -> SandboxDecision`。bash、PowerShell、`execute_bash`、MCP 子进程、文件写入都调它。现有的三档 `SandboxPolicy` 保留，作为构造参数。
2. 给沙箱加结果状态：`ConfirmedExit { code, signal }`、`Unconfirmed`、`BackendUnavailable`、`Interrupted`。沙箱启动失败要返回 `BackendUnavailable`，不能当成普通退出码；relay 在命令启动后被 kill 返回 `Unconfirmed`，工具要提示“命令可能已经执行，不要自动重试”。
3. 文件写入门禁：write/edit 在现有的 `file_mutation_queue` 锁里检查 effective writable roots、`.git`/`.kiana` 保护、realpath/symlink、大小写。工作区外或受保护路径拒绝或询问，结论要和 bash 的 workspace-write 一致。
4. read-before-write：read 记录 `(dev, ino/realpath, mtime, size, content hash, ranges)`；write/edit 要求当前有读记录，读完之后文件被外部改了返回 `FILE_CHANGED_SINCE_READ`；成功写入后让该路径的读记录失效；压缩把 read 结果删掉时也同步失效。
5. 加测试：bwrap 可用/不可用、Landlock ABI<4、workspace-write 多根、`.git`/`.kiana` 保护、relay 被杀、Unconfirmed 不重试、文件工具和 shell 结论一致。

**做完的标准**

- workspace-write 下，bash、`execute_bash`、PowerShell、edit、write 对工作区外路径的结论一致；`.git`/`.kiana` 写入一致失败。
- 沙箱后端不可用时，命令完全不执行，返回 `BackendUnavailable`，不会偷偷回退到宿主。
- relay 被 kill 时返回 `Unconfirmed`，不会自动重跑。
- 没读过文件就 edit/write 返回 `ReadBeforeEdit`；读后外部修改返回 `FileChangedSinceRead`；截断读只能改已经看到的部分。
- 两个并行写同一文件被串行化。

**可能卡住的地方**

- 进程内的文件工具没法用 OS 沙箱隔离自己，这一阶段先做“同策略 pre-check + 锁内复检”，helper 进程可以后面再做。
- inode 在 FAT/exFAT/SMB 上不可靠，拿不准就拒绝，不要猜。
- relay 会多一个进程和 status 文件，pid/namespace 竞态、stale 文件、信号转发都要测。

**并行建议**

如果你有两个人，这一周可以和“配置分层”并行；但执行策略只能有一个人负责，不然会出现两套判断。

---

### 第 3 周：配置分层 + shell 风险分析

**这周要解决什么**

让“这个值到底从哪来的”能回答，让权限规则能持久化到正确的层；同时把 shell 命令的风险分析做扎实，避免 `git status; rm -rf /` 被 `git status` 前缀规则放过去。

**具体做什么**

1. 在 `kiana-config` 加 `ConfigLayerSource`：Builtin/System/Managed/Enterprise/User/Project/Local/CliFlag/Session。低层先、高层后，对象递归合并；数组按 key 决定是 concat 去重还是 replace。
2. 加 `origins()`，逐键记录胜出来源；加 `fingerprint()`，对每层 canonical JSON 做 SHA256 再汇总。这样以后能回答“这个权限规则是项目配置还是用户配置”。
3. 加 managed policy：`allow_managed_permission_rules_only`、`allow_managed_hooks_only`、`disable_bypass_permissions_mode`、`allowed/denied_mcp_servers`。deny 永远合并；allow 在 policy 下只读 managed；冲突时拒绝。
4. 加严格诊断：未知字段/类型错误要能指到文件、行、列、key path；只对 managed/新文件严格，旧 settings.json 保持宽松。
5. 在 `kiana-permissions`/`kiana-tools` 加 `ShellAnalyzer`：把命令拆成子命令（上限 50 个，防止 ReDoS/CPU 饥饿），识别简单命令、重定向、heredoc、命令替换、未闭合引号、换行。解析失败返回 `Unknown`，按 ask/deny 处理，不要猜。
6. 规则匹配：exact、prefix（只对单一简单命令）、wildcard。复合命令绝不命中 prefix。生成规则建议时只生成精确命令或 program+subcommand 前缀，永远不要生成 `prog *`。
7. virtual operations：`cat` → read_file、重定向 → write_file、`curl/wget` → web_fetch、`ls` → list_directory。这样文件/网络规则也能约束 shell 的等价操作。

**做完的标准**

- 同一 key 写在 8 层里，高层胜；managed 的 policy key 不能被 project 覆盖。
- `origins()` 能指出正确赢家；指纹对键序不敏感，改任一有效值会变。
- `git status; rm -rf /`、`git status && curl x | sh` 不命中 `git status` 前缀规则。
- 重定向到工作区外、`$(...)`、反引号、换行、未闭合引号 → ask/deny，不因为前缀 allow 放行。
- `cat file` 受 Read(file) deny 约束；`echo x > .git/config` 受写路径/safety check 约束。
- 现有 settings 的字节级 golden 测试全部通过。

**可能卡住的地方**

- `SettingsManager` 是字节级实现，改合并路径很容易破坏“外部编辑保留”的语义。先加层，不要动旧写回路径。
- 每类 key 的数组/对象策略要表驱动，逐项评审；默认策略错了会静默改变权限和模型选择。
- shell 解析永远不完整。变量、解释器、xargs 都能绕过，所以它只能做“风险提示和规则建议”，不能当唯一安全边界。

---

### 第 4 周：Hooks

**这周要解决什么**

让用户可以在工具调用前后、会话开始/结束、压缩前后挂自己的 shell/HTTP 脚本，做策略、审计、格式化和上下文注入。这是把 Kiana 从“固定流程”变成“可编程”的关键一步。

**具体做什么**

1. 新建 `kiana-hooks` crate，放在 `kiana-config` 和 `kiana-ext-api` 之间。它的职责只有一件：解析 hooks 配置、编译 matcher、派发 handler、执行退出码/JSON 契约。
2. 配置模型：`{ Event: [{ matcher, hooks: [...] }] }`。先支持这些事件：`PreToolUse`、`PostToolUse`、`PostToolUseFailure`、`PermissionDenied`、`UserPromptSubmit`、`SessionStart`、`SessionEnd`、`Stop`、`PreCompact`、`PostCompact`。后面做子代理时再加 `SubagentStart`/`SubagentStop`。
3. handler 先做两种：Command（timeout 30s，支持 async）和 Http（URL/header 白名单、SSRF 防护、重定向重新校验）。prompt/agent 类型以后再说。
4. 执行契约：stdin 收 JSON（session/cwd/event/tool_name/tool_input/tool_response/error/transcript_path）；exit 0 成功，exit 2 阻塞并把 stderr 交给模型，其他退出码只给用户警告；stdout 支持纯文本或 JSON（`continue`/`stopReason`/`suppressOutput`/`systemMessage`/`hookSpecificOutput.permissionDecision`/`updatedInput`/`additionalContext`）。
5. 顺序和聚合：local > project > user；同事件按声明顺序；任一 deny 胜出；`updatedInput` 顺序传递；async 结果只审计；每轮 deny 次数有上限。
6. 安全：project hooks 必须 trust 后才加载；环境变量白名单；cwd 固定项目根；输出 64KB 截断；超时杀进程组；HTTP 阻断私网/链路本地/云元数据，允许 loopback；hook 递归深度限制。
7. 接进 `kiana-session`：`PreToolUse` 在权限层之前跑，改完参数后再做权限判断；`PostToolUse`/`PostToolUseFailure` 在结果合并前后跑；`Stop`/`PreCompact`/`PostCompact` 接 turn/压缩生命周期。

**做完的标准**

- `PreToolUse` 的 command hook exit 2 时，工具不执行，stderr 作为模型可见错误出现一次，审计记录 deny。
- exit 1 只给用户警告，工具继续；async hook 立即返回但结果进审计；超时被杀进程组且无残留。
- `updatedInput` 改写后，权限层看到的是改写后的参数，但不能绕过 deny。
- local > project > user 顺序执行；user allow + project deny 最终 deny。
- HTTP hook 拒绝 `169.254.169.254`、`10.x`、`file://`；跨源重定向剥离 Authorization/Cookie 并重新校验。
- 未受信项目的 hooks 不加载；递归超过上限返回 typed error；`additionalContext` 经 XML 转义后注入。

**可能卡住的地方**

- Hook 本质上是任意命令执行和网络访问。project hook 就是供应链 RCE 面，trust gating 不能省。
- hook 能改写工具参数，如果绕过权限求值就是提权路径。顺序必须是“hook 改写 → 权限判断 → 执行”。
- Windows 的进程组和 shell 语义跟 Linux 不一样，测试要分平台。

---

### 第 5 周：MCP + Skills

**这周要解决什么**

补齐 MCP 的管理面，让 server 的 prompts、工具过滤、annotations、环境变量、OAuth 都可控；同时把 skills 从“静态文件”升级成“按需加载、有预算、能带工具权限”的机制。

**具体做什么**

1. MCP prompts：实现 `prompts/list` + `prompts/get`，把每个 prompt 暴露成 `/mcp__<server>__<prompt>`，参数来自 arguments schema；正文作为转义后的 user 内容注入，记录 server/prompt 来源。
2. per-server 工具过滤：`enabledTools`/`disabledTools`（glob），注册前过滤，直接调用也过滤，exclude 优先于 include。
3. annotations→权限：`read_only_hint` 只在 read-only 模式默认 allow；`destructive_hint` 在 default 强制 ask；`open_world_hint` 额外 ask。hint 不能覆盖 deny，缺省按 write/ask 处理，且不写入持久授权。
4. manager 诊断：`McpServerStatus { connecting/connected/needs_auth/failed/disabled, last_error, tool_count, prompt_count }`；单个 server 失败不影响其他 server。
5. 子进程环境变量白名单：只传 PATH/HOME/SHELL/LANG/LC_*/TMPDIR/代理 + 显式配置；默认剥离 `*_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD`/`*_CREDENTIAL*`。
6. OAuth：凭据键加入 issuer，issuer 不匹配时条件性失效；跨源重定向剥离 Authorization/Cookie 并重新校验；stdio 单行上限 8MiB。
7. Skills：扩展 frontmatter，支持 `allowed-tools`、`model`/`effort`、`context: inline|fork`、`agent`、`hooks`、`paths`（glob）、`user-invocable`、`disable-model-invocation`、`arguments`/`argument-hint`。新增 skill 工具：校验名字/参数，加载正文作为转义后的 user 内容，附加技能目录清单，返回 `SkillActivation { permission_rules, hooks, agent_scope, expires_at }`；激活结束/会话结束撤销。
8. 技能预算：系统提示里的技能索引按模型上下文窗口的 1% 分配（默认 8000 字符），单条描述上限 250 字符，bundled 技能不截断；截断要确定性、可测试。

**做完的标准**

- server 声明 prompt 后出现在 slash command 列表；调用带必填参数；正文里的伪造 system 标签被转义。
- `disabledTools=["admin_*"]` 时 `admin_delete` 不出现在模型 schema，直接调用也返回 typed 拒绝。
- `destructive_hint=true` 的 MCP 工具在 default 模式必须 ask；`read_only_hint=true` 只在 read-only 模式默认 allow；deny 规则仍然优先。
- 一个 server 启动失败时其他 server 的工具可用；`/mcp` 显示 failed + last_error + tool_count。
- stdio server 的 `/proc/self/environ` 里没有 `OPENAI_API_KEY`/`ANTHROPIC_API_KEY`（除非配置显式列出）。
- 系统提示只含技能的 name/description/location；正文在 skill 调用前不出现在上下文。
- 激活带 `allowed-tools=[Bash(git status)]` 的技能后，`git status` 不再 ask；deny 仍然拒绝；激活结束后恢复询问。
- `paths=["src/**/*.rs"]` 的技能在没碰过 src 文件前不出现在列表；read 了 `src/lib.rs` 后下一轮出现。
- 超过 1% 预算时确定性截断；bundled 技能不被截断。

**可能卡住的地方**

- MCP prompts 和 server instructions 都是不可信 prompt 注入面，annotations 只能当提示，不能当授权。
- per-server 环境变量白名单可能让依赖登录态的 server 失效，需要诊断和 override。
- Skills 的 `allowed-tools` 是权限授予，规则解析和 deny 优先必须精确；正文仍然是 prompt 注入面。

---

### 第 6 周：上下文压缩和工具输出

**这周要解决什么**

长会话现在只有“调模型做全量摘要”，成本高，失败时还会留下半截历史。这周先做工具结果裁剪，再做能回滚的压缩，最后把大输出落盘，避免“截断即丢数据”。

**具体做什么**

1. 压缩前先做 tool-result pruning：保护最新的 40k tool-output token；能回收 ≥20k 才执行；小于 50 token 不 blank；skill 读取和 active plan 保护；superseded read 优先 elide；零结果 grep/等待用精确 placeholder；原地 blank，保持 tool call/result 配对。
2. 压缩：保留现有的 `CompactionEntry`，增加 all-or-nothing 提交（摘要失败时历史逐字节不变）、确定性 no-LLM fallback（折叠旧 tool 活动，保留 typed user 和最近 assistant，严格更小且在 recovery target 内）、PTL 重试（头部丢最旧消息最多 3 次，至少留一组）、no-op 缓存和 5 分钟冷却。
3. split-turn 摘要不能切断 `assistant(tool_calls)` 和 `tool_result`。摘要模板改成 objective/details/completed-active-blocked/next/files；prior summary 合并规则：未完成 carry forward、新对话冲突优先、完成项移动。
4. 压缩后恢复：`FileReadCache` 重建，最近 5 个文件（总 50k、单文件 5k）、已调用技能（总 25k、单技能 5k）、plan/plan mode、工具/agent/MCP 增量。恢复作为 hidden context/system attachment，不伪造 tool result；缺失只 warning，不阻塞。
5. 工具输出预算：每个工具的最大输出（Read 大/Edit 100k/Bash/Grep 30k/默认 50k）+ 45k 落盘阈值 + 40k middle-out preview；artifact store（session 级、0600、路径 traversal 拒绝）+ JSON structural outline（offset/length）+ artifact handle 跨压缩存活；聚合 token 超限时把最旧结果替换成 `[Old tool result content cleared; artifact: path]`。

**做完的标准**

- pruning 回收 ≥20k token，最新 40k 完整，tool call/result 仍然合法；superseded read 被 elide，未被覆盖的保留。
- summarizer 失败时历史逐字节不变；no-LLM fallback 产出严格更小的上下文；provider overflow 只 compact+retry 一次，第二次明确报错。
- split-turn 不切断 tool pair，产出 turn-prefix summary；压缩后最近文件/技能/plan 按预算恢复；未恢复路径的 edit 被 read gate 拒绝。
- 10MB bash 输出 wrapper <50k 字符，artifact 可以 offset 回读且逐字节一致；JSON outline 的 offset/length 能切出有效子串。
- 聚合超预算时最旧结果被 cleared，但 artifact 仍可读；handle 跨 compaction 解析到同一个 artifact。
- artifact 路径 traversal 被拒、文件 0600、secret 没落盘。

**可能卡住的地方**

- pruning 可能误删证据；cache lookback 随 provider 变；token 估算有误差。
- 恢复会破坏 prefix cache，要加门控。
- all-or-nothing 和 PTL 重试的边界容易写错，可能留下半替换历史。

---

### 第 7 周：编辑体验

**这周要解决什么**

现在 edit 失败只给一个很模糊的错误；多 edit 不是原子；没有 apply_patch；fuzzy 匹配是整基底切换，用户不知道发生了什么。这周把编辑做成“失败能看懂、成功可验证、批量不出半截”。

**具体做什么**

1. 定义 `EditError`：`NotFound { nearest: Vec<(LineRange, snippet, similarity)> }`、`NotUnique { occurrences }`、`AlreadyApplied`、`Overlap`、`EmptyReplacement`、`FileChanged`。错误文本要包含 path/edit_index/真实行号/最近片段/是否已存在，并明确写“不要重发同一个失败编辑”。
2. 多 edit 先全部校验，再按文件原子应用；任一失败该文件零落盘，返回每个 edit 的状态；成功返回应用后的 hunk 和 ±4 行片段。
3. 模糊匹配只在 exact 失败后启用，发出 `EditStrategyEvent { Exact | Fuzzy | QuoteNormalized | Regex }` 到 tool metadata/telemetry；不要保留“整基底无声切换”。
4. 新增 `apply_patch` 工具：支持 `*** Begin Patch`/`Add`/`Delete`/`Update`/`Move to`/`@@ context`/`*** End of File`；strict/lenient 解析；`seek_sequence` 按 exact → trim_end → trim → Unicode 标点归一化；EOF 锚定；空 pattern no-op；应用前读取当前文件验证全部 hunk，生成 unified diff/new_content，全部成功才在 `file_mutation_queue` 锁里写入。
5. `streaming_parser` 给 TUI 做增量预览；按模型路由 `tools.apply_patch.models` 只对 Codex/GPT 等暴露，其他模型保留 edit。

**做完的标准**

- oldText 不存在时返回 3 个最近行区间和行号，文件不变；多处匹配返回全部出现行号，`replace_all=false` 不写。
- newText 已存在返回 `AlreadyApplied`，不写；多 edit 里一个失败，该文件零改动，逐 edit 状态正确。
- fuzzy 触发时 `strategy=Fuzzy` 出现在 tool metadata；成功结果包含 hunk 和 ±4 行片段。
- apply_patch fixture 覆盖 add/update/delete/move、多文件、多 hunk、CRLF、Unicode、EOF、重复 context、空 patch，输出逐字节一致。
- stale context 时所有文件零写入；diff preview 和最终写入一致；模型路由只对配置的模型暴露 apply_patch。

**可能卡住的地方**

- 模糊匹配可能落错 hunk；新工具的 prompt 和 UI 要写清楚。
- 不要把 apply_patch 和 edit 同时暴露给所有模型，模型会选错。
- 多 edit 的原子性要和 `file_mutation_queue` 的锁配合，否则并行写会破坏。

---

### 第 8 周：评测和 loop guard

**这周要解决什么**

现在的评测还是旧文档 A/B，靠 Docker 和外部仓库，不能回答“这次改动到底变好了还是变坏了”。这周建立 Kiana 自己的原生任务集，同时给 agent loop 加失控保护。

**具体做什么**

1. 在 `kiana-evals` 加原生 task suite（保留现有 docs A/B）：每个任务隔离目录，meta 区分 test/example/solution，运行前重置 solution；执行语言测试命令；失败反馈追加为下一轮 prompt；每个任务写 `results.json`（pass/cost/duration/token/malformed/context_exhausted）；汇总 pass@k、well-formed%、cost；支持断点跳过、replay；所有模型命令都走 `kiana-sandbox`。
2. 加 mock-LLM/确定性 provider：命名轨迹、admin 注册/重置、请求记录断言；没有 API key 也能跑完整工具循环；支持 changed-files→affected-tests 映射。
3. 在 `kiana-agent-core` 加 loop guard：`repeat_key=sha256(tool_name+canonical_json(args))`（剔除 description/display）；检测连续相同调用、全局重复、A-B 交替、read-file 循环、shell 停滞、action 停滞、非法参数重复、执行失败重复、重复 thought、内容 chanting、长周期逐字重复；每个检测器有 `reminder_at`/`stop_at`，默认先 Warn；软上限 100/turn，硬上限 10x；reminder 只在下一次 `prepare_request` 注入，绝不插在 `assistant(tool_calls)` 和 `ToolResult` 之间；硬停返回 `LoopDetected`，不 panic。
4. 加 turn/token/cost/wall-clock 预算：50%/80% 注入提示，100% 移除工具列表、强制最终文本或剥掉 tool_calls；`stop_reason` 用 `TurnCapped`/`TokenCapped`/`CostCapped`/`TimeCapped`，不覆盖 `completed`/`failed`。
5. 加 context report：用 stand-in prompt 重建下一次请求，按 tool/prompt/turn-context/segment 统计 token 和预览；手工压缩以 `CondensationEvent` + token 下降为准（2.5s settle、90s timeout，超时算 Failed）。

**做完的标准**

- `kiana-evals run --suite smoke --trials 3 --sandbox` 生成确定性 `results.json`，含 pass@1/pass@3、cost、duration、well-formed%；测试失败后自动追加反馈并产生第 2 次尝试。
- 断点续跑跳过已完成，不把半截轨迹当完成；baseline fingerprint 不变时 dry-run 零模型调用；没有沙箱时拒绝运行。
- 同一 bash 参数连续 5 次：第 3 次 reminder 位于下一次请求末尾，第 5 次返回 `LoopDetected` 且前 3 个 tool result 保留；20 个不同 edit/read 零告警；stateful poll 输出变化不触发。
- `max_turns=3` 恰好 3 个 assistant turn 后 `TurnCapped`；token/cost 超额最后一轮没有工具列表且产生最终文本；unknown price 返回 `cost_unknown`，不按 0。
- context report total 与 provider reported input 误差 <10%；压缩后 saved_tokens 和实际差值一致，超时返回 Failed。

**可能卡住的地方**

- 测试执行的是模型写的代码，必须走 `kiana-sandbox`。
- 脚本轨迹会和真实模型漂移；loop guard 先 Warn 观测，别一上来就 Stop。
- pass@k 没有置信区间和预算控制，先跑小样本。

---

### 第 9 周：Provider 请求策略 + OTel

**这周要解决什么**

让模型请求“发之前就知道会不会失败”，失败后知道该重试、该压缩、还是该换模型；同时把 trace/metrics 导出来，能回答“这次 run 慢在哪、花了多少 token”。

**具体做什么**

1. 能力矩阵：provider manifest 声明支持哪些 `ModelOperation` 和 `tools`/`json_output`/`parallel_tool_calls`/`vision`；adapter 也声明自己实现了哪些；请求必须同时满足两边，否则发网络前返回 `Unsupported` 并指出缺什么。
2. 输出预算公式：`min(caller_max_tokens, model.max_output, context_window - estimated_prompt - 1024 reserve, product_default_32k)`，再取 `max(budget, min(0.3*model.max_output, available))` 作为 floor；`available<1` 返回 typed `context_overflow`；保留现有显式 0 语义。
3. 错误分类：先读 typed 字段，再保守遍历结构（depth 8），分类 `Auth`/`RateLimit`/`ContextWindow`/`Quota`/`InvalidRequest`/`Transient`/`Unknown`；429 或 rate limit 不允许被误判成 context length；quota/billing 优先于 transient。
4. 可重试：typed `isRetryable` 优先，否则 `{408,409,425,429,500,502,503,504,529}`；每个错误只做一次 message fallback；分类驱动 agent 重试、overflow 压缩和故障转移。
5. 输入预检：请求前用 token counter 估算，超过窗口直接 typed `ContextWindow`，零请求；usage 返回后按 canonical/用户声明/provider-reported 优先级记账，记录 `cost_source`。
6. cache 语义：`ExplicitBreakpoints`/`ImplicitStrict`/`ImplicitTolerant`/`Uncached` 决定是否注入断点、是否有 cache-warmer 资格；声明式 provider 加载期校验 ID/字段，保留未知字段往返。
7. OTel：在 `kiana-telemetry` 实现 logs/metrics/traces 三路 exporter（OTLP gRPC/HTTP，可选 TLS），保持现有 `TelemetryContext`/`Span`/`Metrics`/`Propagation`/`Store` 契约不变。
8. W3C `traceparent`/`tracestate` 注入/提取/父子 span 绑定；`TRACEPARENT` 环境变量传入子进程/沙箱。
9. typed `SessionTelemetry`：turn TTFT/TTFM、API call、SSE event、tool call、compaction、hook run、token usage/cost、startup phase；指标名集中到 `metrics/names`，标签低基数化/脱敏。
10. `AgentRunCollector` → `AgentRunSummary`/`AgentRunCoverage`：每次 run 汇总 chat/tool 记录（latency、tokens、cost、stop reason、tool status ok/error/skipped/blocked/timeout/aborted），供 TUI 状态栏和 evals 使用。
11. 内容捕获默认关闭，只有 `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true` 时才记录；SDK 初始化前事件缓冲，shutdown 前 flush；exporter 不可达不影响 agent。

**做完的标准**

- text-only provider 请求 image 生成时，在发 HTTP 前返回 `Unsupported`，错误包含缺失 capability。
- 随机 context/max_output/prompt/caller 下预算恒 `<= context-prompt-1024` 且 `>= floor`，除非 `available<1` 返回 typed error。
- HTTP 429 + body 含 "context length" 被分类 `RateLimit`，不触发压缩；401/invalid_api_key 分类 `Auth`；400 malformed tool schema 分类 `InvalidRequest`。
- 529/503 可重试，400/401 不重试；typed `isRetryable=false` 覆盖状态码集合。
- 输入超过 context_window 时预检失败，provider mock 断言零请求；`cost_source` 优先级正确。
- cache 语义为 `ExplicitBreakpoints` 时注入断点，`ImplicitTolerant` 不注入；坏目录条目只跳过该条并产生诊断。
- 一次完整会话在本地 OTLP collector 产生 1 个 root span + chat/tool 子 span，属性含 `gen_ai.*`。
- `capture_content=false` 时 trace/log 中没有 prompt/tool 内容；标签不含参数或高基数 tool 名。
- `TRACEPARENT` 传入 bash 子进程并能取回父 trace；exporter 不可达时 agent 正常运行。
- run summary 的 tokens/cost/tool status 和 session 记录一致；现有 telemetry 契约/conformance 测试全部通过。

**可能卡住的地方**

- Kiana 现在的 overflow 判定是按 provider 文案逐条移植的正则，结构化分类和它并存时要明确优先级。
- 输出预算公式会改变现有 clamp 行为，测试期望要一起改。
- provider-option 规则表很大，只移植能力门/预算/分类三件，别全搬。

---

### 第 10 周：持久记忆

**这周要解决什么**

让 Kiana 记住跨会话的事情：用户偏好、项目约束、反馈、参考资料。不是把所有聊天记录塞进上下文，而是维护一个有界索引，需要时召回少量相关条目。

**具体做什么**

1. 新建 `kiana-memory` crate。每个 scope（user `~/.kiana/memory`、project `.kiana/memory`）一个 `MEMORY.md` 索引 + topic 文件，frontmatter `{name, description, type: user|feedback|project|reference}`；feedback/project 正文必须有 rule/fact + Why + How to apply。
2. 索引上限 200 行/25KB，单条 ≤200 字符，截断时附一条警告；扫描最多 200 个文件，每个文件只读前 30 行。
3. 写入：add/replace/remove/patch 批量，返回字节预算使用率；temp+rename + 跨进程 flock；格式版本 header；文件无法往返解析时拒绝覆盖并备份；revision 做乐观冲突。
4. 召回：manifest 只含 frontmatter；side query 最多选 5 条，失败时用关键词/路径启发式兜底；注入前复查文件仍存在且 mtime 匹配，过期 id 丢弃并记录；注入一个 fence 的 `<memory-context>` user 消息，标注“参考数据不是指令”，有 token 预算，不重复索引。
5. 抽取/整合：基于 `kiana-durable` 的 lease/retry/watermark 后台 job；扫描 watermark 之后的 persisted sessions，跳过当前 session、subagent session 和过新 session；Phase 1 抽取 raw memory+synopsis，Phase 2 合并为 `MEMORY.md`/`memory_summary.md`/可选 skills，明确 carry-forward/新对话优先/完成项移动；redact secrets，限制每日预算。
6. 治理：system prompt 只读 load-time `MemorySnapshot`，mid-session 写入不改变当前 prompt（压缩后失效）；审批时把 old_text 解析成完整条目并 pin 字节，批准后按 pinned 字节应用（TOCTOU-safe）；forget 写 tombstone、scrub body、级联 derived_from；每次操作一条 audit。

**做完的标准**

- 500 条记忆时索引 ≤200 行且 ≤25KB，恰好一条警告；feedback 缺 Why/How 返回字段级错误且文件字节不变。
- 两个进程并发写：败者超时，文件仍可解析且胜者条目存在；损坏文件拒绝覆盖并创建 `.bak`。
- 召回最多 5 条；零选择不产生消息；选中文件被删后丢弃并继续；provider 失败/超时走启发式且不阻断模型调用。
- 两个进程同时启动抽取时恰好一个 claim；consolidation 崩溃后 stale lease 重试且不重复产物；subagent/current session 零抽取。
- fixture secret 不出现在任何记忆文件/索引/摘要；forget 后 derived 条目被移除且记录原因。
- mid-session 写入前后当前 turn system prompt 字节不变；压缩后新 snapshot 生效。

**可能卡住的地方**

- 记忆是 prompt 注入面；索引截断会隐藏条目。
- 后台模型调用有成本和隐私问题；锁/租约有时钟偏差；redaction 只是尽力而为。
- 不要默认开启自动记忆；不要把 memory 写入绕过权限/审计。

---

### 第 11 周：子代理 + 会话恢复

**这周要解决什么**

让 Kiana 能把一部分工作交给子代理（只读调研、并行改代码、独立验收），并且在进程崩溃/中断后能恢复，不重复执行危险操作。

**具体做什么**

1. 定义 `AgentDefinition`：description、prompt、tools/disallowedTools、model（含 inherit）、effort、permissionMode、mcpServers、hooks、skills 预加载、maxTurns、memory scope、background、isolation（worktree/shared）、initialPrompt、omitClaudeMd；来源 builtin/plugin/user/project/policy/flag 有覆盖顺序；单文件解析错误不中止整个目录。
2. 在 `kiana-session` 里做子代理运行时（不新建 crate）：独立 tool pool、system prompt、permission context、token budget；子 session 以 parent/sidechain 关联，resume 复用不重复消息；fork 的 prompt-cache 共享只作为 provider capability flag，默认关闭。
3. 权限收窄：`CanonicalToolId` 命名空间 + `intersect(parent, requested)`，子代理只能收窄；hidden/denied 工具在 dispatch 再检查；headless 子代理的 Ask 一律 Deny。
4. 工作区隔离：`IsolationMode { Shared, Worktree, CowClone }`；baseline 捕获 HEAD/staged/unstaged/untracked（含 nested repo）；delta patch 对 git 树用 `git diff`，保证 `git apply` 字节一致；ownership 记录供下次清理；Linux 先支持 git worktree/btrfs-XFS reflink/overlayfs，失败显式拒绝。
5. 结果契约：`terminal={completed|failed|cancelled|timed_out|polling_timed_out}` + `stop_reason`；`try_set_terminal` 保证超时/完成竞争只有一个赢家；`yield` 工具按 JSON Schema 校验（strict/permissive，有限重试），支持 label 增量装配（数组 append、标量 replace）；usage/receipt 从流式 chunk 累积。
6. 后台任务：`background=true` 立即返回 task id；每个 task 输出文件有硬上限、`O_NOFOLLOW`；terminal 通知按 task id + terminal seq 恰好一次；父 turn 结束不杀后台任务；崩溃后 durable sweep 恢复或 terminalize；接受 `kiana-durable` 的 inbox/scheduler，不另造队列。
7. 验收：runtime-stamped `ToolReceipt { tool, args_sha256, output_sha256, status, seq }`；acceptance checks 用代码判定（文件存在/非空、JSON syntax、CSV header、`tests_passed:<command>` 必须锚定真实执行且输出含测试摘要形状）；不可判定一律 `UNVERIFIED`，绝不静默通过。
8. 会话恢复：先分类 `Clean`/`InterruptedPrompt`/`InterruptedTurn`/`DegradedHistory`，分类先于任何修复；`InterruptedTurn` 为每个无结果的 tool_call 合成 failed tool result（稳定 id、错误限长），绝不重新执行 mutating 工具；`InterruptedPrompt` 重放一次尾部 user；`DegradedHistory` 只修 request view，不改 canonical transcript。
9. 执行所有权：per-session 单 execution；busy period 在同一事务写 `ExecutionClaim` + `Execution.Started`；terminal 释放 claim，clean shutdown 故意保留；启动 sweep 按 claim 生成 Resume/Terminalize，durably 计数尝试（默认 10），超过则 terminalize。
10. checkpoint：每项目私有 git dir（`--work-tree`）、per-workdir ref + index + metadata，隔离 `GIT_CONFIG_GLOBAL/SYSTEM`，不碰用户仓库；每个 mutating tool 前 `ensure_checkpoint`（每 turn/目录最多一次；跳过 root/home、`node_modules`/`target`/`.git`、文件数/大小超限；树没变不提交）。
11. agent-write ledger：`record_agent_write(path, sha256)`；safe restore 只还原“当前内容 == agent 最后一次写入”的文件，其余列 `skipped_user_edits`；nested git repo/gitlink 拒绝恢复；restore 前做 restore-transaction 快照，可 rollback；保留期/总量上限清理，用父目录 `(dev,ino)` 区分项目删除和卷未挂载。

**做完的标准**

- 子代理 allow `{*}`、父 allow `{read}` 时，有效权限是 `{read}`；父 deny 的工具从 schema 消失，直接 dispatch 也失败；headless 子代理 Ask 为 Deny。
- 子代理工具 schema 和 dispatch 只含 allowlist；unknown frontmatter 字段报错但不中止目录；resume 复用 transcript 且消息数不翻倍。
- 两个 sibling 在 worktree 隔离下编辑同一路径互不可见；delta patch 可被 `git apply` 字节一致应用；崩溃留 ownership 记录且下次 sweep 清理。
- 完成与超时竞争只有一个 terminal；yield schema 违规有限重试后标 invalid 但不丢结果；没有真实 bash receipt 的 `tests_passed` 判 `UNVERIFIED`。
- 后台任务父 turn 结束后继续，terminal 通知只一次；cancel 传播并抑制迟到通知；token usage 归因到父和 task。
- 在 tool_calls 和 tool_result 之间 kill，resume 为每个 call 合成一个失败结果、不重复执行、turn 标记 interrupted。
- 在 user prompt 和 assistant 之间 kill，prompt 重放一次且无重复 user entry；孤儿 tool result 从 request view 移除但 canonical transcript 保留。
- 两个进程同时 acquire 同一 session 只有一个运行；clean shutdown 不消耗 attempt；attempt 超限 terminalize 且有原因；terminal 事件只发布一次。
- 用户手改文件后 restore 跳过该文件并报告；agent 最后写入的文件被还原；nested repo 拒绝；restore 失败可回滚到恢复前。
- checkpoint 不修改用户 git status/refs/stash；无变化不产生 commit；retention 不删活跃 checkpoint。

**可能卡住的地方**

- Kiana 是 current-thread tokio + `LocalSet`（rquickjs 不是 `Send`），子代理必须在同一个 `LocalSet` 或专用 runtime，不能开 OS 线程。
- 递归委派成本/深度会爆炸；worktree 的平台差异和清理竞态要测。
- 子代理结果是不可信文本；acceptance 解析对版本/语言很脆弱，失败必须 `UNVERIFIED`。

---

### 第 12 周：TUI、集成和发布硬化

**这周要解决什么**

把前面做的权限、审批、hooks、记忆、子代理暴露成好用的界面；补 web/编辑器/daemon 这些外部集成；最后做一轮回归和发布检查。

**具体做什么**

1. TUI 帧契约：`FrameProvider` 每帧返回可选 `HistoryBatch`（append/replay，单调 id）+ 完整可变 viewport；TUI 对每个接受的 batch 只写一次并 ack；viewport 锚定在剩余 history 之后，只做行级 diff，overlay 单独合成，光标在 synchronized-output 帧内；finality 归应用（active/settled/committed，capacity pressure 才退休最短 settled 前缀）。
2. transcript 投影：不可变事件日志 → render model；ActionEvent 被 ObservationEvent 原地替换；连续 tool run 折叠；thought 提升；artifact/error/Finish 作为 group breaker；流式文本进 UI-only provisional slot（id 与最终 durable 事件相同、anchor_seq 定位、durable 到达退役）；delta 按帧批量，非 delta 帧先 flush，attempt 高者覆盖。
3. 用户消息 pending 队列：按 trimmed 内容 + 服务端时间戳围栏确认，确认 id 去重 replay，150s watchdog 转 error 可重试。
4. per-tool 审批卡：Bash 显示命令/重定向/子命令，FileEdit 显示 diff，FileWrite 显示内容，WebFetch 显示域名，MCP/Skill 定制，未知 fallback；显示 suggestions（Allow once/Always{scope}/Reject）、decision reason、可选异步 sideQuery 风险解释（失败静默、标 advisory）；危险动作默认聚焦 Reject。
5. 单一 modal 仲裁 + 审批队列：同一时刻只渲染一个 modal，显示 index/total；ask-user 支持单选/多选（All/Other）/文本/yes-no；键盘/鼠标/滚动/备用屏隔离。
6. i18n：`t(key, params)` + 多行字符串数组；语言包数据文件（TOML/JSON，不执行用户 JS）；用户目录 > 内置 > fallback；CI 对 strict locale 做 key parity + `MUST_TRANSLATE_KEYS`；UI locale 和模型 output language 分离。
7. Web search/fetch：`SearchEngine` trait + 有序引擎链（首选 + fallback + 其余），单引擎 3 次退避、整链 max_retries；结果归一化 `SearchResult`/`SearchResponse`；fetch 并行抓取、10k 字符截断、SSRF 防护（拒私网/链路本地/云元数据/非 http-s）；优先官方 API（Brave/Google CSE/Bing），scraper 作为可选。
8. 编辑器集成（二选一或先 mock）：lockfile 发现（`~/.kiana/ide/<port>.lock`：port/workspacePath/authToken/ppid），dead ppid/workspace 不符清理；MCP Streamable HTTP + bearer；`ide/contextUpdate`（打开文件/当前文件/光标/选区/trust，上限 10 文件/16KiB）；`openDiff` round-trip（`ide/diffAccepted` 返回用户最终内容，`ide/diffRejected` 关闭，readOnly 显式降级）。如果选 ACP：`initialize`/`newSession`/`loadSession`/`prompt` 流式/cancel/setSessionModel/setSessionConfigOption/permission request。
9. daemon/多客户端（可选）：扩展现有 `kiana-rpc-print`，不新建 crate；`EventFeed` 全局 typed 流 → 只编码一次 → 每连接 dropping queue（4096，满则失败该订阅）→ SSE/WS 单帧；`server.connected` + 15s 心跳；volatile/durable 双通道；`SeqCursor`（最高连续 seq + ahead-set + MAX_AHEAD）与 REST 最新页→WS `since` 交接。
10. 无人值守 cron（依赖 durable 的 ADR）：job 支持 duration/every/5-field cron/ISO one-shot、skills/model/script（受管目录）/context_from/workdir/timezone/grace_minutes；tick 先持久化 `next_run_at` + `pending_slot`；executions 账本 claimed/running/completed/failed/unknown/cancelled/missed，owner 死亡用 pid+start-time 指纹证明；catch-up 半周期 clamp 120s–2h；delivery 每行最多 claim 一次，claimed 后进程死亡记 unknown 永不重试。
11. 发布硬化：跑全量回归；检查默认配置、迁移、回滚；更新 `ARCHITECTURE.md`/`README`/`START_HERE` 的 crate 数量和层表；写发布 checklist。

**做完的标准**

- 丢弃 ack 后重发同一 `HistoryBatch`，终端输出不重复；10k 行流式 transcript 在 80x24/120x40 来回 resize 后内容与逻辑 transcript 一致。
- 单字符变化的帧只产生一个 cursor move + 一个 write；tmux/screen 和 alt-screen 有固定转义 golden；超宽行/写盘失败不再 panic。
- action+observation 只一张卡；3k delta chunk 每秒最多提交 N 帧且控制事件不乱序；用户消息只回显一次，watchdog 后状态 Failed 可重试。
- 五种工具各渲染对应卡片；危险动作默认 Deny 且 Enter 不批准；decision reason 与引擎一致；风险解释失败 3s 内隐藏。
- 3 个审批排队时只渲染一个 modal 且显示 1/3；设置 modal 抢占但不丢权限项；任意时刻 modal 数量 ≤1。
- strict locale 缺 key 时 CI 失败并列出 key；缺失 key 回退英文不 panic；`output_language=zh` 时系统提示含中文回答规则。
- 首选搜索失败自动切下一引擎且输出 schema 一致；全失败返回含尝试引擎/原因的可操作错误；fetch 拒绝私网/`169.254.169.254`/非 http(s)。
- 死 pid lock 清理；活 pid 且 workspace 匹配才连接；`openDiff` 返回用户手工修改后的内容，readOnly 明确报错不静默写；`contextUpdate` 超 10 文件/16KiB 截断。
- 慢客户端队列满只失败该订阅，进程内存不增长；断线 `after_seq` 只补缺失 durable 事件且无重复副作用。
- cron 在 `next_run_at` 推进后、dispatch 前 kill，重启后 exactly one `pending_slot` 恢复或记 missed；pid 复用但启动时间不同判死亡；delivery claimed 后死亡永久 unknown 不重试。

**可能卡住的地方**

- 整体替换 TUI 风险高，用实验开关增量替换渲染核心，不要重写 48 个组件。
- 终端/multiplexer 边界多；并发对话框和流式渲染有竞态。
- daemon 协议/鉴权面积大；cron 有时区/DST 和多进程争用；浏览器调试端口是安全边界。

---

## 五、两个新 crate

### `kiana-hooks`

放在 `kiana-config` 和 `kiana-ext-api` 之间。它只做一件事：把 hooks 配置变成可执行的 handler。

为什么不塞进 `kiana-session`：`agent_session.rs` 已经 390KB，再塞进去会变成新的 god file。为什么不放 `kiana-agent-core`：hook 是会话级能力，不应该让底层循环依赖配置。

### `kiana-memory`

放在 `kiana-config` 之上，被 `kiana-resources` 和 `kiana-session` 使用。它负责跨会话记忆的存储、召回、抽取、整合、脱敏和遗忘。

为什么不放 `kiana-resources`：resources 只做加载和渲染，memory 要后台 job、AI 调用和会话扫描，职责不一样。

---

## 六、每项工作的验收方式

| 工作 | 最低验收 |
|---|---|
| 权限闭环 | `cargo test -p kiana-permissions -p kiana-session`；TUI/headless 两条路径一致 |
| 沙箱覆盖 | workspace-write 下所有执行路径结论一致；后端不可用不执行；Unconfirmed 不重试 |
| 配置分层 | 8 层优先级矩阵；`origins()` 正确；managed policy 不可覆盖 |
| shell 分析 | `git status; rm -rf /` 不放行；重定向/替换/heredoc 走 ask/deny |
| hooks | exit 2 阻塞、exit 1 警告、updatedInput 后仍受权限约束、SSRF/trust 测试 |
| MCP | prompts、per-server 过滤、annotations、env 白名单、OAuth issuer 各有测试 |
| skills | 条件激活、allowed-tools、1% 预算、collision 诊断 |
| 压缩 | pruning ≥20k、all-or-nothing、no-LLM fallback、PTL 一次、artifact 可回读 |
| 编辑 | 结构化错误、多 edit 原子、apply_patch fixture、fuzzy 可观测 |
| 评测 | mock provider 无 key 可跑；pass@k/cost/well-formed%；loop guard 先 Warn |
| provider | 能力门在发 HTTP 前失败；预算 property test；429 不误判 |
| OTel | 本地 collector 有 root+子 span；默认不捕获内容；TRACEPARENT 可传子进程 |
| 记忆 | 索引有界、召回 ≤5、抽取单 claim、secret 不落盘、forget 级联 |
| 子代理 | 权限只能收窄、worktree 隔离、receipt、后台通知恰好一次 |
| 恢复 | dangling tool_call 不重跑、单 execution、checkpoint 只回滚 agent 写入 |
| TUI | history batch/ack、审批卡、modal 仲裁、i18n key parity |

---

## 七、我建议你现在就做的三件事

1. **先把第 0 周做完。** 它只需要 3 天，但能把“权限靠环境变量、execute_bash 绕过、文档数量不对”这三个最容易被后面工作放大的问题解决掉。
2. **第 1 周不要急着做 hooks 或 memory。** 审批闭环没做好，后面所有“用户确认”都会返工。
3. **每周结束跑一次固定回归**：`cargo test -p kiana-permissions -p kiana-session -p kiana-sandbox`，再加一个手动的 TUI/headless 对照。不要等到第 12 周才发现权限路径分叉。

如果你同意这个顺序，我就从第 0 周开始改代码；如果你想把某一周提前（比如先做记忆或先做 TUI），告诉我换哪一周，我重排依赖。
