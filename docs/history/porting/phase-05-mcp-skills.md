# W5 — MCP 管理面与 Skills 按需加载

日期：2026-11-07

对应 `docs/history/porting/plan.md` 第 5 周（MCP + Skills）。目标：补齐 MCP 的 prompts、
per-server 工具过滤、annotations 权限策略、服务器状态诊断、子进程环境白名单、OAuth
凭据隔离，并把 skills 从静态列表升级为带 frontmatter 扩展、按需加载、有预算、能携带
工具权限的机制。

第 5 周只修改以下路径：`crates/kiana-mcp/**`、`crates/kiana-resources/**`、
`crates/kiana-extensions/**`、`crates/kiana-builtin-extensions/**`、
`crates/kiana-coding-agent/src/**`。**没有**修改 `crates/kiana-session/**`、根
`Cargo.toml`、`Cargo.lock` 或 `reference_project/**`；需要会话接线的部分写在
`docs/history/porting/handoffs/phase-05.md`。

## 参考设计对照表

只借鉴设计，代码按 Kiana 的类型与运行时重写；没有复制任何参考项目源码。

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
|---|---|---|
| `reference_project/ClaudeCode/src/services/mcp/client.ts` | `prompts/list` 分页与 `prompts/get` 参数；prompt 命令名 `mcp__<server>__<prompt>`；正文作为 user 内容 | `crates/kiana-mcp/src/client.rs::{list_prompts, list_prompts_page, get_prompt}`；`crates/kiana-builtin-extensions/src/mcp/index.rs::{register_prompts, handle_prompt_command, build_mcp_prompt_content}` |
| `reference_project/gemini-cli/packages/core/src/tools/mcp-client.ts` | `includeTools`/`excludeTools` 的 glob 过滤；exclude 优先于 include；过滤在注册前生效 | `crates/kiana-builtin-extensions/src/mcp/filter.rs::{McpToolFilter, check_tool_enabled}` |
| `reference_project/ClaudeCode/src/services/mcp/client.ts` + `reference_project/gemini-cli/packages/core/src/policy/types.ts` | `readOnlyHint`/`destructiveHint`/`openWorldHint` 只作为提示，不作为授权；只读工具才可放宽，破坏性/开放世界操作要额外确认 | `crates/kiana-resources/src/mcp_annotations.rs::apply_mcp_annotations`（纯策略，待会话接线） |
| `reference_project/ClaudeCode/src/services/mcp/useManageMCPConnections.ts` | 每服务器状态机与诊断字段（连接中/已连接/需要登录/失败/禁用，错误、工具数） | `crates/kiana-builtin-extensions/src/mcp/runtime.rs::{McpServerStatus, McpServerStatusState, McpServerConnection::status}` |
| `reference_project/codex/codex-rs/core/src/exec_env.rs` | 子进程环境白名单：只继承最小集合，敏感变量默认剥离，显式配置覆盖 | `crates/kiana-mcp/src/transports/stdio.rs::{DEFAULT_ENV_ALLOWLIST, StdioEnvPolicy, sanitized_env, is_sensitive_env_name}` |
| `reference_project/ClaudeCode/src/services/mcp/auth.ts` + `reference_project/gemini-cli/packages/core/src/mcp/oauth-callback-iss-handling.test.ts` | OAuth 凭据按 issuer 隔离（RFC 8414/9207）；issuer 不匹配时失效而不误用旧 token | `crates/kiana-builtin-extensions/src/mcp/oauth.rs::{McpOAuthServerStore, McpOAuthCredentialStore::for_server_with_issuer, invalidate_if_issuer_mismatch}` |
| `reference_project/ClaudeCode/src/tools/SkillTool/SkillTool.ts` | skill 工具：校验名字、`disable-model-invocation`、`$ARGUMENTS`/`$N` 替换、正文包裹、技能目录清单、激活结果 | `crates/kiana-resources/src/skills/tool.rs::{SkillTool, create_skill_tool_definition, SKILL_TOOL_NAME}` |
| `reference_project/ClaudeCode/src/tools/SkillTool/prompt.ts` | 技能索引预算是上下文窗口的 1%（`tokens * 4 * 0.01`，默认 8000 字符）；单条描述上限 250 字符 | `crates/kiana-resources/src/skills/prompt.rs::{skill_index_budget_chars, format_skill_definitions_for_prompt, MAX_SKILL_DESCRIPTION_CHARS}` |
| `reference_project/ClaudeCode/src/skills/loadSkillsDir.ts` | frontmatter 扩展：`allowed-tools`、`model`/`effort`、`context`、`agent`、`hooks`、`paths`、`user-invocable`、`disable-model-invocation`、`arguments`/`argument-hint` | `crates/kiana-resources/src/skills/{mod.rs, definition.rs, activation.rs}` |
| `reference_project/oh-my-pi/packages/coding-agent/src/extensibility/skills.ts` | frontmatter 宽松解析与字符串/数组两态；技能目录作为附件；`paths` 用 glob 控制可见性 | `crates/kiana-resources/src/skills/definition.rs::{SkillDefinition, SkillMetadata, paths_match, skill_visible_for_model, list_skill_directory}` |

## 改了什么

### 1. `kiana-mcp`：prompts、stdio 环境白名单、重定向校验

- `src/protocol_types.rs`
  - 新增 `PromptArgument`、`Prompt`、`ListPromptsResult`、`PromptRole`、`PromptMessage`、
    `GetPromptResult`，全部宽松反序列化：缺字段/坏成员降级而不是整包失败；未知字段保留在
    `extra`，回写不丢数据。
  - `PromptRole` 只接受 `user`/`assistant`；`PromptMessage::from_value` 强制校验 `content`。
- `src/client.rs`
  - `list_prompts` / `list_prompts_page` / `get_prompt(name, args, options)`；参数原样下发，
    不做本地改写。
  - `validate_get_prompt_result` 的错误文案固定为 `Invalid MCP prompts/get result` /
    `Invalid message in MCP prompts/get result`，让调用方能把“协议坏数据”与“网络失败”区分开。
- `src/transports/stdio.rs`
  - `DEFAULT_STDIO_MAX_LINE_BYTES = 8 MiB`，作为 stdio 单行帧默认上限（streamable HTTP 的
    message 上限保持 16 MiB）。
  - 环境白名单：`DEFAULT_ENV_ALLOWLIST`（PATH/HOME/SHELL/LANG/LC_*/TMPDIR/TMP/TEMP 与
    `http(s)_proxy`/`no_proxy`/`all_proxy` 的大小写形式）+ `StdioEnvPolicy { extra_allow }`；
    `sanitized_env` 先过滤父进程环境再写入显式 `env`，显式配置可以覆盖敏感变量。
  - `is_sensitive_env_name` 默认剥离 `*_KEY` / `*_TOKEN` / `*_SECRET` / `*_PASSWORD` /
    `*_CREDENTIAL*`。
  - `StdioTransportOptions` 新增 `env_policy`；未设置时保持原来的“全量继承”行为，避免破坏
    旧调用方。
- `src/auth_provider.rs`
  - `default_redirect_policy`：最多 10 跳、只允许 http/https、HTTPS→HTTP 只允许 loopback；
    reqwest 自身在跨源重定向时剥离 `Authorization`/`Cookie`，策略层再校验一次目标地址。
- `tests/prompts.rs` 新增 loopback 传输级测试：分页、参数原样下发、非法 role/缺 content 的
  错误文案。

### 2. `kiana-builtin-extensions`：prompts 命令、过滤、状态、OAuth issuer

- `src/mcp/runtime.rs`
  - `ConnectionFields` 新增 `prompts`；`setup_connected_client` 只在服务器声明 `prompts`
    能力时调用 `prompts/list`，并订阅 `notifications/prompts/list_changed` → `refresh_prompts`。
  - `McpServerStatusState { Connecting, Connected, NeedsAuth, Failed, Disabled }` 与
    `McpServerStatus { state, last_error, tool_count, prompt_count }`；`status(enabled)` 把
    `Disconnected` 归到 `connecting`、`Closed` 归到 `disabled`。
  - `McpServerConnection::{prompts, get_prompt}`；`get_prompt` 复用只读请求的重试/登录路径。
  - 默认 stdio 传输显式启用 `StdioEnvPolicy::default()`，所以 `/proc/self/environ` 不再带
    provider API key（除非服务器配置的 `env` 显式列出）。
- `src/mcp/filter.rs`（新）
  - `enabledTools`/`disabledTools`（同时接受 `enabled_tools`/`disabled_tools`）；glob 支持
    `*?[]**`；非法 glob 退化为字面量匹配，不让服务器启动失败；exclude 优先。
  - 注册前过滤；直接调用路径也调用 `check_tool_enabled`，返回 typed 错误
    `McpToolDisabledError`，避免 codemode/记忆中的旧工具名绕过。
- `src/mcp/index.rs`
  - 工具注册使用 `McpToolFilter::from_config`；`get_client` 闭包在调用前再次校验（配置可能
    在会话中改变）。
  - prompt 命令：`parse_prompt_arguments` 支持 `name=value` 与位置参数（引号/反斜杠转义），
    必填缺失/未知参数/参数过多都有可读错误；`handle_prompt_command` 调 `prompts/get`，正文经
    `build_mcp_prompt_content` 转义后通过 `send_user_message` 注入；`append_entry("mcp_prompt",
    {server, prompt, arguments})` 记录来源（不发给模型）。
  - 正文包装为 `<mcp_prompt server="..." prompt="..."><message role="...">...</message>
    </mcp_prompt>`；`<`/`>`/`&`/引号全部转义，图片/音频降级为占位文本。
  - `describe_state` 在 connected 后面追加 prompt 数量；`/mcp` 的 failed 行保留
    `failed: <last_error>` 与工具数。
- `src/mcp/oauth.rs`
  - 凭据键从 `<namespace>|<url>` 扩展为 `<namespace>|<url>|<issuer>`；`for_server_with_issuer`
    写入发现到的 issuer，`invalidate_if_issuer_mismatch` 只删除 issuer 不匹配的条目。
  - 兼容旧键：URL-only 键仍可读取并迁移；只有一个 issuer 键时按“当前服务器”使用；`remove()`
    删除 base/legacy/全部 `<base>|*` 家族。
- 测试：`tests/mcp_connections.rs` 的假服务器支持 prompts/list/get；新增 prompt 加载/状态、
  失败状态、单服务器失败隔离测试；`tests/mcp_oauth_store.rs` 新增 issuer 键、条件失效、
  legacy 迁移与家族删除测试。

### 3. `kiana-resources`：Skills frontmatter、激活、工具、预算、annotations 策略

- `src/skills/mod.rs`
  - `SkillFrontmatter` 新增 `allowed-tools`、`model`、`effort`、`context`、`agent`、`hooks`、
    `paths`、`user-invocable`、`arguments`、`argument-hint`；snake_case 与 kebab-case 都接受。
- `src/skills/definition.rs`（新）
  - `SkillMetadata`/`SkillDefinition`/`SkillContext::{Inline, Fork}`/`SkillArgument`；
    `load_skill_definitions` 读取目录、解析 frontmatter、保留正文与目录归属。
  - `paths_match`/`skill_visible_for_model`：`paths` 是 glob，只有本会话碰过匹配路径后才把
    技能暴露给模型。
  - `list_skill_directory`：技能目录的相对路径清单（排序、跳过隐藏文件、最多 200 条 + `…`）。
- `src/skills/activation.rs`（新）
  - `parse_allowed_tools`：`Bash(git status)` → `CommandPrefix`，`Bash(cmd:*)`/`cmd*` →
    `CommandWildcard`，`Read(src/**)`/`Edit(...)` → `PathPrefix`，裸名字 → `Tool`；无法解析的
    规格保留在 `invalid` 而不是静默丢弃。
  - `SkillActivation { permission_rules, allowed_tools, invalid_allowed_tools, hooks,
    agent_scope, expires_at, context, model, effort }` 与 `SkillActivationSet`
    （`activate`/`revoke`/`clear`/`prune_expired`）。
  - `evaluate_with_activations`：先跑原引擎；`Deny` 原样返回（deny 优先）；只有 `Ask` 且存在
    激活的 `Allow` 规则时才放行，并标上技能原因。
- `src/skills/tool.rs`（新）
  - `SkillTool`：校验名字、`disable-model-invocation`、参数；替换 `$ARGUMENTS`/`$N`；正文
    转义后包进 `<skill name location><skill_files>…</skill_files><skill_content>…</skill_content>
    </skill>`；返回 `SkillActivation`（含 `expires_at`）；`deactivate`/`end_session` 撤销。
  - `create_skill_tool_definition` 注册为 `skill` 工具，details 里返回 `{skill, activation, files}`。
- `src/skills/prompt.rs`
  - `DEFAULT_SKILL_INDEX_BUDGET_CHARS = 8000`；`skill_index_budget_chars(tokens) =
    tokens * 4 / 100`（1%）；`MAX_SKILL_DESCRIPTION_CHARS = 250`。
  - `format_skill_definitions_for_prompt`：放得下就全量；bundled 技能描述不截断；非 bundled
    先截到 250，再按比例均分剩余预算；低于 20 字符时省略描述；仍超预算则从尾部丢弃非 bundled
    条目；全部是 bundled 时允许超出。算法只依赖输入，确定性、可测试。
  - 旧 `format_skills_for_prompt` 保留原 XML 格式，但已改为复用同一预算/250 字符截断逻辑
    （默认 8000 字符），因此默认系统提示路径已经受预算约束；按模型窗口缩放仍需会话接线。
- `src/mcp_annotations.rs`（新）
  - `apply_mcp_annotations(annotations, mode, base, explicit_deny)`：
    - `readOnlyHint=true` 只在 `read-only` 模式把模式级拒绝改为 allow；
    - `destructiveHint=true` 在 `default`/`accept-edits` 强制 ask（可覆盖 allow 规则）；
    - `openWorldHint=true` 额外 ask；
    - 显式 deny（`explicit_deny`）和引擎的 `Deny` 永远优先；没有 hint 时保持引擎决策。
  - `allows_persistent_approval`：destructive/open-world 工具不允许写持久授权。

### 4. 测试与格式化

新增/扩展的测试文件：
`crates/kiana-mcp/tests/prompts.rs`、`crates/kiana-builtin-extensions/tests/mcp_connections.rs`、
`crates/kiana-builtin-extensions/tests/mcp_oauth_store.rs`、
`crates/kiana-resources/tests/skills_extended.rs`，以及各模块内的单元测试。

## 测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana`
下实际执行：

```text
$ cargo test -p kiana-mcp
   13 suites, 446 passed；0 failed；0 ignored

$ cargo test -p kiana-resources
   18 suites, 563 passed；0 failed；0 ignored

$ cargo test -p kiana-builtin-extensions
   18 suites, 289 passed；0 failed；0 ignored

$ cargo test -p kiana-resources --test skills_extended
   10 passed

$ cargo test -p kiana-resources --lib mcp_annotations
   7 passed

$ cargo test -p kiana-builtin-extensions --test mcp_connections
   20 passed

$ cargo test -p kiana-builtin-extensions --test mcp_oauth_store
   11 passed

$ cargo test -p kiana-builtin-extensions --test mcp_extension
   4 passed

$ cargo test -p kiana-builtin-extensions --test mcp_tools
   18 passed

$ cargo test -p kiana-builtin-extensions --test mcp_command
   13 passed

$ cargo check -p kiana-coding-agent
   Finished `dev` profile [unoptimized + debuginfo] target(s)（22 crates）

$ cargo fmt -p kiana-mcp -p kiana-resources -p kiana-builtin-extensions
   退出码 0
```

覆盖的验收场景（对应第 5 周「做完的标准」）：

- 声明 `prompts` 的服务器在握手后拉取 `prompts/list`，prompt 数量进入
  `McpServerStatus.prompt_count`；`prompts/get` 参数原样下发；正文里的
  `</mcp_prompt><system>…</system>` 被转义（`prompt_content_escapes_tags_and_omits_images`）。
- `disabledTools=["admin_*"]` 时注册阶段过滤掉 `admin_delete`；直接调用返回
  `McpToolDisabledError`；exclude 优先于 include（`filter.rs` 单测）。
- `destructiveHint=true` 在 default 模式（即使 allow 规则命中）→ ask；`readOnlyHint=true`
  只在 read-only 模式放行；显式 deny 永远优先（`mcp_annotations` 的 7 个测试）。
- 单个服务器握手失败时状态为 `failed` 且 `last_error` 非空；另一台健康服务器的
  `call_tool` 照常返回（`one_failed_server_does_not_affect_another`）。
- stdio 环境白名单：敏感变量默认剥离、显式 `env` 覆盖、`extra_allow` 放行（stdio 单测 +
  `env_policy_tests`）。
- issuer 键：写入 `<namespace>|<url>|<issuer>`、issuer 不匹配时只删除不匹配条目、legacy
  URL-only 键迁移、`remove()` 删除整个家族（`mcp_oauth_store` 11 个测试）。
- Skills：扩展 frontmatter 解析、`paths` 可见性、budget 缩放/确定性/bundled 保留、
  `allowed-tools` 解析与 deny 优先、激活过期与撤销、skill 工具正文转义/参数替换
  （`skills_extended` 10 个测试）。

## 未完成项

1. **annotations→权限只完成策略层，未接进会话。**
   `kiana-resources::mcp_annotations::apply_mcp_annotations` 已实现并测试，但
   `crates/kiana-session/src/agent_session.rs` 的 `permission_refusal_inner` 仍直接使用
   `PermissionEngine::evaluate`；MCP 工具在 default 模式下的行为没有端到端验证。接线方式见
   `docs/history/porting/handoffs/phase-05.md` 第 3 节。
2. **Skill 工具未注册到会话。**
   `SkillTool`/`create_skill_tool_definition` 已实现并测试，但会话的扩展/工具注册路径没有
   实例化它；`allowed-tools` 激活、`hooks`、`agent_scope`、`expires_at` 的运行时应用属于
   会话层。
3. **`context: fork` 未执行。**
   frontmatter 与 `SkillActivation.context` 已携带该值，但 fork 子会话的创建、结果回传与
   父会话隔离需要会话层实现。
4. **`paths` 的“碰过文件后下一轮出现”未接线。**
   `skill_visible_for_model`/`paths_match` 是纯函数；会话需要在每次工具调用后记录读/写路径，
   下一轮系统提示重建时传入。
5. **技能预算未按模型上下文窗口缩放。**
   `system_prompt::build_system_prompt` 仍调用旧的 `format_skills_for_prompt`，它现在会应用
   默认 8000 字符预算与 250 字符描述上限，但 `BuildSystemPromptOptions` 没有上下文窗口
   字段，无法按具体模型缩放。会话层需要改用 `format_skill_definitions_for_prompt` 并传入
   `skill_index_budget_chars(model_context_window)`。
6. **`/mcp` 结构化状态尚未被管理器视图消费。**
   `McpServerConnection::status()` 已提供 `failed + last_error + tool_count + prompt_count`，
   `/mcp` 文本行（`describe_state`）已显示失败原因、工具数与 prompt 数；但 `ui.rs` 的菜单/
   详情页还没有把结构化 `McpServerStatus` 接进去。

## 风险

- annotations 是服务器提供的不可信提示。即使接线完成，也只能作为“是否额外确认”的依据；
  绝不能生成持久 allow，也不能覆盖 deny。本实现已把 `allows_persistent_approval` 独立出来，
  接线时必须同时使用。
- 子进程环境白名单可能让依赖登录态的 server 失效（例如只认 `XDG_*` 或自定义变量的工具）。
  排查入口是显式 `env` 与 `StdioEnvPolicy.extra_allow`；默认行为为了安全收紧了继承面。
- prompt 正文和 skill 正文都是 prompt 注入面。本实现只做 XML 转义和边界标签，不能阻止模型
  被正文里的自然语言影响；需要在产品文案与权限层继续把“正文 ≠ 授权”作为原则。
- OAuth issuer 键迁移涉及旧凭据：如果服务器在发现文档里改了 issuer，旧 token 会被删除并
  要求重新登录；这是安全上的预期行为，但会影响已登录用户。
- 技能预算截断保留 bundled 技能，可能在极端配置下让索引超过 1% 预算；这是刻意的取舍
  （bundled 技能必须始终可见）。
