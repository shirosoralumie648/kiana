# Week 5 接线清单（wire agent 用）

本文件说明把第 5 周的 MCP / Skills 能力接进 `kiana-session` 需要改哪些共享文件。
第 5 周 agent 只修改了 `crates/kiana-mcp/**`、`crates/kiana-resources/**`、
`crates/kiana-builtin-extensions/**` 与 `docs/history/porting/phase-05-mcp-skills.md`，**没有**修改
`crates/kiana-session/**`、根 `Cargo.toml` 或 `Cargo.lock`。

## 0. 前置条件

`crates/kiana-session/Cargo.toml` 已经依赖 `kiana-resources`、`kiana-permissions`、
`kiana-ext-api`，**不需要**新增依赖，也不需要改根 `Cargo.toml` / `Cargo.lock`。

本文件涉及的公开面都已在 crate 根导出：

```rust
use kiana_resources::mcp_annotations::{
    apply_mcp_annotations, allows_persistent_approval,
};
use kiana_resources::skills::activation::{
    SkillActivation, SkillActivationSet, evaluate_with_activations, now_utc,
};
use kiana_resources::skills::{
    SkillContext, SkillDefinition, SkillTool, create_skill_tool_definition,
    format_skill_definitions_for_prompt, load_skill_definitions, skill_index_budget_chars,
    skill_visible_for_model, LoadSkillsOptions,
};
```

## 1. MCP prompts 与 per-server 工具过滤（无需会话改动）

这两项已经在 `kiana-builtin-extensions` 内部完成：

- 握手时若服务器声明 `prompts` 能力，会拉 `prompts/list` 并注册
  `/mcp__<server>__<prompt>` 命令；`prompts/get` 的正文经 XML 转义后作为 user 消息注入，
  同时 `append_entry("mcp_prompt", ...)` 记录来源。会话只需要保证 MCP 扩展被正常挂载，
  命令会出现在 slash command 列表里。
- `enabledTools`/`disabledTools` 已在工具注册前过滤，并在工具调用入口再次校验
  （typed 错误 `McpToolDisabledError`）。MCP 工具直接调用、codemode、旧工具名都走同一条
  `get_client` 闭包，不需要会话额外拦截。
- `/mcp` 的文本状态行 `describe_state` 已显示 `failed: <last_error>`、工具数与 prompt 数。

可选改进（不是 DoD 必需）：`ui.rs` 的服务器详情页可以改为消费
`McpServerConnection::status(enabled)`，直接拿到
`McpServerStatus { state, last_error, tool_count, prompt_count }`；`state` 的序列化拼写是
`connecting` / `connected` / `needs_auth` / `failed` / `disabled`。

## 2. MCP annotations → 权限（必须接线）

策略函数在 `kiana_resources::mcp_annotations`，是纯函数，不读写持久状态。

### 2.1 取得当前工具声明的 annotations

`Core` 的 `tool_definitions: Mutex<IndexMap<String, ToolDefinitionEntry>>` 里已经有
`entry.definition.annotations`（`kiana_ext_api::tool_types::ToolAnnotations`）。建议加一个
私有 helper：

```rust
fn tool_annotations(&self, tool_name: &str) -> Option<kiana_ext_api::tool_types::ToolAnnotations> {
    self.flush_pending_tool_refresh();
    lock(&self.tool_definitions)
        .get(tool_name)
        .and_then(|entry| entry.definition.annotations.clone())
}
```

### 2.2 在权限求值处应用

文件：`crates/kiana-session/src/agent_session.rs`，`Core::permission_refusal_inner`
（约 4090 行）。当前直接 `match engine.evaluate(request)`。改成：

```rust
let annotations = if request.action == kiana_permissions::ActionKind::Mcp {
    self.tool_annotations(&request.tool_name)
} else {
    None
};
let explicit_deny = engine
    .policy()
    .rules
    .iter()
    .any(|rule| {
        rule.decision() == kiana_permissions::RuleDecision::Deny && rule.matches(request)
    });
let decision = apply_mcp_annotations(
    annotations.as_ref(),
    engine.mode(),
    engine.evaluate(request),
    explicit_deny,
);
match decision { /* 原来的 Allow / Deny / Ask 分支 */ }
```

要点：

- **必须传 `explicit_deny`**：`PermissionEngine::evaluate` 的 `Deny` 同时表示“显式规则
  拒绝”和“read-only 模式默认拒绝”，策略函数需要靠这个布尔值区分。漏传的后果是
  `readOnlyHint` 可能把显式 deny 翻案。
- 只对 `ActionKind::Mcp` 应用；其它工具保持原行为。
- `deny` 永远优先：策略函数内部也会在 `default`/`accept-edits` 模式下原样返回引擎的
  `Deny`，但 `explicit_deny` 是第二道保险。
- 不要缓存 `apply_mcp_annotations` 的结果；annotations 来自服务器，可能在
  `tools/list_changed` 后变化。

### 2.3 禁止持久授权

文件：同一个 `Core::persist_approval`（约 4184 行）。在把 `ApprovalDecision::AllowAlways`
转成规则之前加：

```rust
let annotations = self.tool_annotations(&request.tool_name);
if !allows_persistent_approval(annotations.as_ref()) {
    // destructive/open-world 工具只允许本次批准：不要写 session/local/user 规则。
    return Err(
        "This MCP tool declares destructive/open-world behavior; approve it once per call."
            .to_string(),
    );
}
```

如果 UI 能提前知道 annotations，应在确认框里隐藏“总是允许”选项；上面的检查是兜底，
避免通过事件/程序化审批绕过。

### 2.4 建议的会话测试

- `default` 模式 + `destructiveHint=true` + 已有 allow 规则 → `Ask`（reason 含
  `destructiveHint`）。
- `read-only` 模式 + `readOnlyHint=true` + 无显式 deny → `Allow`。
- 显式 `Tool{"mcp__x__y", Deny}` + `readOnlyHint=true` → 仍然 `Deny`。
- `destructiveHint=true` 的 `AllowAlways` 请求被拒绝，规则列表不增加。

## 3. Skill 工具注册与激活

### 3.1 加载 `SkillDefinition`

现有资源加载路径在 `AgentSession` 的 reload 流程里用
`loader.get_skills()`（`agent_session.rs` 约 8042 行）拿到 `Vec<Skill>`。新增：

```rust
let definitions = load_skill_definitions(LoadSkillsOptions {
    cwd: /* 与 load_skills 相同 */,
    agent_dir: /* 相同 */,
    skill_paths: /* 相同 */,
    include_defaults: /* 相同 */,
});
```

把 `definitions.skills`（`Vec<SkillDefinition>`）存进 `Core`，与现有 `Vec<Skill>` 并存；
`definitions.diagnostics` 并入现有 `ResourceDiagnostic` 列表。资源热重载时调用
`SkillTool::update_definitions`。

### 3.2 注册 `skill` 工具

```rust
let skill_tool = Arc::new(SkillTool::new(definitions.skills.clone()));
let definition = create_skill_tool_definition(Arc::clone(&skill_tool));
```

把 `definition` 放进 `base_tool_definitions`（或 `tool_definitions` 中带来源信息的注册
路径），并给它 `ToolExposure::Direct` / 默认 active。`definition.annotations` 为 `None`；
`definition.execute` 返回的 details 是 `{ skill, activation, files }`。

`SkillTool` 需要按会话保存：会话结束时调用 `skill_tool.end_session()`（清空全部激活），
资源 reload 时 `update_definitions`，若产品要求 TTL 则 `set_ttl(Some(Duration))`。

### 3.3 应用 `allowed-tools` 激活

`SkillTool::invoke` 内部已经把 `SkillActivation` 写进自己的 `SkillActivationSet`
（`invoke` 成功即激活）。会话不需要再次 `activate`，只需要在权限求值时读取这个台账：

```rust
let now = now_utc();
skill_tool.activations().prune_expired(now);
let decision = evaluate_with_activations(&engine, skill_tool.activations(), request, now);
```

如果会话也为别的入口（例如 slash command 直接调用技能）产生激活，可以用独立的
`SkillActivationSet`，但同一技能不要在两套台账里各存一份。

权限求值改为对**所有工具**使用带激活的入口（或只对非 MCP 工具使用；MCP 工具的
`allowed-tools` 一般不指向 MCP）。

注意顺序：先跑 `apply_mcp_annotations` 还是先跑技能激活？推荐：

1. `engine.evaluate`（含 deny）；
2. 若结果是 `Ask`，再尝试 `SkillActivationSet::allows`（`evaluate_with_activations`
   已经实现这一顺序，`Deny` 永远不被翻案）；
3. MCP 工具再应用 annotations 策略。

如果同时接两者，最稳妥的是让 `evaluate_with_activations` 先跑，再把其结果作为
`apply_mcp_annotations` 的 `base`（只对 `ActionKind::Mcp`）。

撤销入口是 `SkillTool::deactivate(name)`（用户显式停用技能）与
`SkillTool::end_session()`（会话结束/重置）；`SkillActivationSet::prune_expired(now)`
每轮调用（`eval` 前）。如果需要监听撤销事件，读取 `skill_tool.activations().active(now)`。

### 3.4 `hooks` / `agent_scope` / `model` / `effort` / `context`

`SkillActivation` 里这些字段目前只是携带值，需要会话按产品语义应用：

- `hooks`：合并进第 4 周的 `HookRegistry`，作用域限定为本次技能激活（激活撤销时移除）；
- `agent_scope`：限制技能激活期间可用的子代理/工具集合；
- `model`/`effort`：在技能调用产生的子回合或 `context: fork` 子会话里覆盖当前模型设置；
- `context: SkillContext::Inline`：正文直接进入当前上下文（`SkillTool` 已返回内容）；
- `context: SkillContext::Fork`：创建子会话，把技能正文作为 system/user 输入，父会话只接收
  子会话的最终结果。fork 的隔离与结果回传在会话层实现，本仓库目前没有现成 API。

## 4. `paths` 可见性（需要触达路径记录）

`skill_visible_for_model(definition, touched)` 与 `paths_match(patterns, touched)` 是纯函数。
会话需要维护“本会话已触达的路径”集合：

- 在 `PermissionRequest` 构造处（或工具成功执行后）把 `request.paths` 与
  `request.read_paths` 加入 `touched_paths: Mutex<HashSet<PathBuf>>`；
- `ToolInfo` 的 `read`/`write`/`edit`/`bash` 调用都要记录；bash 的虚拟读写路径已经在
  `PermissionRequest` 里提取；
- 每轮重建系统提示时，把 `touched_paths` 的 `Vec<PathBuf>` 传给
  `format_skill_definitions_for_prompt(definitions, reader, budget, &touched)`。

DoD 的“read 了 `src/lib.rs` 后下一轮出现”就是：第一次读之前 `touched` 为空 → 技能不出现；
读入路径后下一轮 `rebuild_system_prompt` 传入 touched → 技能出现。

## 5. 技能索引预算

`format_skill_definitions_for_prompt` 已经实现 1% 预算与 250 字符上限。会话接入点：

文件：`crates/kiana-session/src/agent_session.rs`，`Core::rebuild_system_prompt`
（约 5935 行）。当前 `sources.skills` 是 `Vec<Skill>`，最终由
`build_system_prompt_sections` 调用旧的 `format_skills_for_prompt`；旧函数现在也应用
`DEFAULT_SKILL_INDEX_BUDGET_CHARS = 8000` 与 250 字符上限，但**无法按模型窗口缩放**。

建议二选一：

1. 在 `Core::rebuild_system_prompt` 里用当前模型的上下文窗口计算预算：
   ```rust
   let window = self.current_model_context_window(); // u64 tokens
   let budget = skill_index_budget_chars(Some(window));
   let skills_text = format_skill_definitions_for_prompt(
       &definitions,
       reader,
       budget,
       &touched_paths,
   );
   ```
   然后把 `skills_text` 作为 `sections["skills"]` 传入
   `BuildSystemPromptOptions.sections`，同时把 `skills` 设为空列表，避免默认路径重复生成
   技能段（`IndexMap::insert` 会保留内置 `skills` 的位置）。
2. 或者扩展 `BuildSystemPromptOptions`（`kiana-ext-api`，不在本周所有权内）增加
   `skills_budget_chars`，由 `system_prompt.rs` 透传。

模型窗口从当前模型元数据读取；未知窗口用 `None`，`skill_index_budget_chars(None)` 返回
8000。预算算法是确定性的，不需要缓存。

## 6. 会话测试清单（接线后）

- MCP：`destructiveHint` 工具在 default 模式弹确认；deny 规则仍然拒绝；
  `readOnlyHint` 只在 read-only 模式放行；`AllowAlways` 不写持久规则。
- Skills：`/skill <name> arg` 后 `allowed-tools` 生效；激活过期/会话结束后恢复 ask；
  `disable-model-invocation` 的工具调用被拒绝；`paths` 技能在触达文件前不出现。
- 预算：小窗口模型下技能索引按 1% 截断；bundled 技能始终完整。
- `context: fork`：技能正文进入子会话，父会话只收到结果（如果本迭代实现）。

## 7. 不需要改的文件

- 根 `Cargo.toml`、`Cargo.lock`：无新 crate、无新 workspace 依赖。
- `kiana-builtin-extensions`：MCP prompts / 过滤 / 状态已经接线完毕，会话不需要直接依赖
  这个 crate。
- `crates/kiana-ext-api`：annotations/`Skill` 类型已存在，本周没有改动它。
