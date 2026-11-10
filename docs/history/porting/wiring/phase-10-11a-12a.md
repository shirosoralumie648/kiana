# Wire D — 第 10/11a/12a 周共享接线与本波集成验证

日期：2026-11-07
范围：`docs/history/porting/handoffs/phase-10.md`、`phase-11a-subagent-runtime.md`、`phase-12a-tui-frame-contract.md` 的跨 crate
接线与集成验证；验收标准见 `docs/history/porting/plan.md` 第 10/11/12 周。

本文件记录实际应用的 handoff、与 handoff 的核对结论与有意偏差、共享文件改动、
真实检查/测试输出、剩余缺口，以及给后续波的风险。

---

## 1. 应用顺序与核对结论

按依赖顺序应用：

1. **week 10（持久记忆）**：`kiana-memory` 依赖、project scope 的
   `MemoryService`/`MemorySession`、权限 gate、system prompt `memory` section、
   压缩后快照刷新、每轮召回、`memory` 工具。
2. **week 11a（子代理运行时）**：lane 已直接修改 `crates/kiana-session/src/agent_session.rs`；
   本波只核对公开入口、编译与测试，没有重写实现，也没有共享文件需要应用。
3. **week 12a（TUI/审批 API）**：`kiana-ext-api` 增加 `permission_card` 契约与
   `PermissionCardRequest`/`PermissionCardChoice`/`PermissionScopeKind`；会话 `Ask`
   分支改成调用该契约并映射回 `ApprovalDecision`。

工具执行顺序保持 `PreToolUse hook（可改写 args）→ 扩展 tool_call（可改写 args）→
记录触达路径 → 权限判断 → 执行`；任何一层的 deny 都优先，hook/扩展的 allow 不能
跳过权限判断。该顺序与既有 `hooks_wiring::pre_tool_use_updated_input_is_seen_by_permission_layer`、
`hook_deny_blocks_tool_call` 以及 `permissions_integration` 的断言一致，本波未改动。

---

## 2. 共享文件改动

| 文件 | 改动 |
|---|---|
| `crates/kiana-session/Cargo.toml` | 新增 `kiana-memory = { workspace = true }`。 |
| `crates/kiana-session/src/lib.rs` | 导出 `memory_wiring` 模块。 |
| `crates/kiana-session/src/memory_wiring.rs` | **新增**：`MemoryWiringOptions`、`SessionMemory`、`SessionMemoryGate`、`memory` 工具定义与单元测试。 |
| `crates/kiana-session/src/agent_session.rs` | `AgentSessionConfig::memory`；`Core` 的记忆运行时/待注入消息；system prompt `memory` section；压缩后 `after_compaction`；每轮召回注入与瞬时消息抑制；`memory_enabled()`/`memory_prompt_index()`；Approval `Ask` 分支改用 `permission_card`。 |
| `crates/kiana-session/src/sdk.rs` | `AgentSessionConfig` 新字段填 `None`。 |
| `crates/kiana-session/tests/common/mod.rs` | Harness 增加 `memory` 选项并传给 `AgentSessionConfig`。 |
| `crates/kiana-session/tests/agent_session_compaction_live.rs` | `AgentSessionConfig` 新字段填 `None`。 |
| `crates/kiana-session/tests/memory_wiring_integration.rs` | **新增**：system prompt 索引、召回注入、transient 不落盘、默认关闭。 |
| `crates/kiana-ext-api/src/ui.rs` | **新增**：`PermissionCardRequest`、`PermissionCardChoice`、`PermissionScopeKind`、`ExtensionUiContext::permission_card` 默认实现。 |
| `crates/kiana-ext-api/src/index.rs` | 再导出上述审批卡类型。 |
| `Cargo.lock` | cargo 自动更新 `kiana-session` 的依赖表（新增 `kiana-memory` 边）；未手工编辑。 |

没有修改 `reference_project/**`、`docs/history/porting/plan.md`、`kiana-durable/**`、
`kiana-memory/**` 或 `kiana-interactive/**`。

说明：`kiana-ext-api` 是 week-12 handoff 明确交给 wire agent 的共享契约（lane 声明
`session/ext-api` 不在其所有权内）；不增加 `permission_card` 就无法应用本波的审批
API，因此按 handoff 修改。改动是向后兼容的默认 trait 方法 + 新类型再导出。

---

## 3. Week 10：持久记忆接线

### 3.1 构造与默认关闭

- `Core::new` 在权限引擎构造之后创建 `SessionMemory`：
  - `AgentSessionConfig::memory` 优先；`None` 时读 `KIANA_MEMORY` /
    `KIANA_MEMORY_AUTO_EXTRACT`（`1/true/yes/on`）。
  - 只有 `enabled=true` 才 `MemoryStore::open`；默认不创建 `<cwd>/.kiana/memory`。
  - 构造失败（目录不可写、快照损坏）不阻断会话启动，写进 `config_diagnostics`。
- 只接 project scope（`<cwd>/.kiana/memory`）；`MemoryWiringOptions::root` 是测试/宿主
  覆盖入口。user scope 未构造。
- `MemoryConfig` 保留 crate 默认值（索引 200 行/25 KB、单条 200 字符、扫描 200 文件、
  每文件 30 行、召回 5 条、锁 2 s），只覆盖 `auto_extract`。

### 3.2 写入门禁

`SessionMemoryGate` 实现 `kiana_memory::MemoryWriteGate`：

- 每个 `WriteIntent` 生成一个 `PermissionRequest`：
  `tool_name="memory"`、`action=Write`、`paths=[topics/<id>.md]`（add 用 `topics/`）、
  `args={scope, op, name}`、`reason="memory write"`。
- `Allow` 继续；`Deny` 返回 `MemoryError::PermissionDenied`；`Ask` **不弹 UI**
  （trait 是同步的），返回 `PermissionDenied` 并附带原因。
- 权限引擎未启用或 settings 显式关闭权限层时使用 `DenyAllGate`（fail-closed）。
- `approve_write(WriteRequest)` 保留精确 intent 集合的一次性 token API；当前没有
  调用方，交互式记忆审批留给后续波（见缺口 1）。
- `MemoryService::apply` 仍负责脱敏、审计、revision 冲突与 flock；工具/会话层不直接
  写 `MEMORY.md`/topics。

### 3.3 system prompt（load-time 快照）

- `rebuild_system_prompt` 在 `sections` 中插入 `memory`（仅当快照有条目；避免注入只有
  格式头的空索引）。section 由 `build_system_prompt_sections` 包成
  `<memory>...</memory>`。
- 只在 session start / 工具集重建 / 压缩成功后重建；mid-session 写入只改磁盘，
  当前 turn 的 prompt 字节不变。
- 压缩提交成功后调用 `refresh_memory_after_compaction()`：`MemorySession::after_compaction()`，
  变化时 `reset_recall()` 并同时重建 base/run system prompt options；压缩失败路径不调用。

### 3.4 每轮召回

- `prepare_run` 在真实 user 消息之后调用一次
  `RecallSession::recall`（`selector=None`，走 crate 的关键词/路径启发式回退）。
- 有结果时追加一条 `role=user` 的 `<memory-context>` 消息；零选择不注入。
- 该消息是 transient 的：
  - `MessageStart`/`MessageEnd` 不对外发 `AgentSessionEvent`；
  - `MessageEnd` 持久化前按正文精确匹配 `pending_memory_contexts` 并跳过
    `append_message`；
  - 每次 `start_run` 清空未消费列表，避免 abort 后泄漏到下一轮。
- 召回失败/超时被吞掉，不阻断模型调用（crate 内部已回退/裁剪）。

### 3.5 `memory` 工具

- 工具定义在 `kiana-session`（与 `skill` 工具同一模式），由 `Core::new` 追加到
  `custom_tools`；`refresh_tool_registry` 走扩展/SDK 工具路径注册，默认激活。
- 支持 `add/replace/remove/patch/forget/list/batch`，只调用 `MemoryService`；
  `forget` 用 `WriteSource::User`。
- 参数 `target` 只接受 `project`；`user` 返回 `InvalidArgument`。
- 没有在 `kiana-tools/src/index.rs` 注册：工具执行需要会话持有的 `MemoryService` +
  权限 gate，放在 `kiana-tools` 会把依赖方向倒置（同 `skill` 工具先例）。

### 3.6 抽取/整合（Wire D 决策点）

- `kiana-durable` 仍没有跨进程 lease/watermark API；本波**没有**修改
  `kiana-durable/**`（不在权限内）。
- 保留 `FileExtractionStore` 作为 `ExtractionStateStore` 参考实现（单进程/文件锁）；
  没有在会话里启动 `ExtractionJob`，也没有实现 `SessionSource` 适配。
- 发现一个接口不匹配：`kiana_memory::SessionSource` 的 `scan_sessions`/`load_session`
  是**同步** trait，而 handoff 要求的 `SessionManager::list_all_in` 是 `async fn`；
  在 tokio `LocalSet` 内用 `block_on` 会 panic，因此没有硬接。
- 因此 `auto_extract` 目前只保存到 `MemoryConfig`，没有后台调度/两阶段 provider
  调用/摘要产物；`SourceKey` 幂等、claim、watermark 等能力仍由 `kiana-memory` 的
  测试覆盖。

---

## 4. Week 11a：子代理运行时核对

- `crates/kiana-session/src/agent_definition/**`、`subagent/**` 与
  `AgentSession::subagent_parent_context()` / `prepare_subagent()` /
  `resume_subagent()` 均在当前树中；共享文件没有待应用的 lane 改动
  （`docs/history/porting/handoffs/phase-11a.md` handoff 为“无”）。
- `kiana-session` 全量测试通过（见 §6），包括 `subagent_runtime` 的 9 个用例与
  子代理相关的 lib 单测。
- 本波没有重做或修改 week 11a 的实现；其声明的缺口（worktree 不带父未提交改动、
  overlayfs 未实机 mount、skills 正文预加载、background/task runtime 等）原样保留。

---

## 5. Week 12a：审批 API

- `kiana-ext-api` 新增：
  - `PermissionCardRequest { tool_name, args, cwd, decision_reason, always_scopes, dangerous }`；
  - `PermissionCardChoice { AllowOnce, AllowAlways(scope), Reject{reason} }`；
  - `PermissionScopeKind { Session, Local, Project }`；
  - `ExtensionUiContext::permission_card` 默认实现。
- 默认实现**保持第 1 周的两步 select**（`Allow once`/`Always allow`/`Reject`，随后
  选择作用域；Reject 可输入原因），因此 RPC/print/扩展 no-op 上下文行为不变，
  `permissions_integration` 的 12 个用例全部通过。
- 会话 `permission_refusal_inner` 的 `Ask` 分支现在构造 `PermissionCardRequest`：
  - `args` 是完整工具参数，只交给 UI，不进入 `PermissionRequested` 事件；
  - `decision_reason` 原样来自引擎的 `Ask.reason`；
  - `always_scopes` 由 `allows_persistent_approval` 过滤：destructive/open-world MCP
    只允许一次性批准；
  - `dangerous` 目前恒为 `false`（见缺口 6）。
- `PermissionCardChoice` 映射回 `ApprovalDecision` 后仍走既有 `persist_approval`，
  `AllowAlways` 的作用域写回与事件流不变。

---

## 6. 真实检查与测试输出

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana`
下实际执行。

```text
$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 24.22s
（在最后一次编辑与 fmt 之后重跑，exit 0）

$ cargo test -p kiana-memory
61 passed, 0 failed, 0 ignored（8 个 test target）

$ cargo test -p kiana-session
467 passed, 0 failed, 5 ignored（71 个 test target）
（含新增 memory_wiring 6 个 lib 用例 + memory_wiring_integration 2 个用例）

$ cargo test -p kiana-session-core
255 passed, 0 failed, 1 ignored（29 个 test target）

$ cargo test -p kiana-durable
首次运行：1 failed / 20 passed（harness_events::reports_queued_submissions_inbox_changes_and_retries，
期望末尾 `done` 事件未在 drained 前到达；单独重跑该用例通过）
全量重跑：896 passed, 0 failed, 11 ignored（88 个 test target）

$ cargo test -p kiana-tui
1409 passed, 0 failed, 6 ignored（60 个 test target）

$ cargo test -p kiana-coding-agent
331 passed, 0 failed, 5 ignored（27 个 test target）

$ cargo test -p kiana-ext-api
79 passed, 0 failed, 0 ignored（9 个 test target；改动 crate 的回归）

$ cargo fmt -p kiana-memory -p kiana-session -p kiana-session-core \
    -p kiana-durable -p kiana-tui -p kiana-coding-agent -p kiana-ext-api -- --check
（无输出，退出码 0）
```

新增/修改用例：

- `memory_wiring::tests::disabled_options_do_not_create_a_store`：默认关闭不建目录。
- `memory_wiring::tests::empty_index_is_not_injected_and_mid_session_writes_stay_frozen`：
  空索引不注入；mid-session 写入后快照不变；`after_compaction` 后索引生效。
- `memory_wiring::tests::read_only_permissions_deny_memory_writes` /
  `missing_permission_engine_fails_closed`：Deny/无引擎都 fail-closed。
- `memory_wiring::tests::recall_injects_a_single_fenced_user_message`：启发式召回、
  fence/“参考数据不是指令”、同会话去重。
- `memory_wiring::tests::memory_tool_add_and_list_go_through_the_service`：工具只走
  `MemoryService`，read-only 被拒。
- `memory_wiring_integration::memory_index_and_recall_are_wired_into_the_session`：
  `<memory>` section 进入 system prompt、召回消息注入、且不写入会话文件。
- `memory_wiring_integration::memory_is_disabled_by_default_and_creates_no_directory`。

`cargo test -p kiana-session` 的 5 个 ignored 与 week 11a 报告一致（既有用例）。

---

## 7. 剩余缺口

1. **交互式记忆审批未接线**：`MemoryWriteGate::authorize` 是同步的，`Ask` 只能
   fail-closed。`SessionMemory::approve_write` token API 已就绪，但没有 UI 调用方。
   在 `default`/`accept-edits` 模式下，`<cwd>/.kiana/memory` 属于受保护的 `.kiana`
   子目录，引擎会给 `Ask`，因此记忆写入（以及 `memory` 工具写操作）实际不可用，
   除非使用 bypass 或显式 Allow 规则。
2. **抽取 job 未落地**：没有 `SessionSource` 实现、Phase1/Phase2 provider 调用、
   durable-lease 适配或后台调度；`auto_extract` 只是配置值。接口不匹配见 §3.6。
3. **user scope 未启用**：只构造 project scope；工具 `target=user` 显式报错。
4. **memory 工具的端到端调用未覆盖**：单测直接调用执行器，集成测试只断言工具
   active；没有通过 agent loop 发起一次 `memory` tool call（需要 faux tool-call
   fixture + bypass 权限）。
5. **召回 side query 未实现**：`selector=None` 恒走启发式；没有 provider prompt、
   fork agent、3 s/800 ms 取消适配。
6. **`dangerous` 恒 false**：`PermissionRequest::is_dangerous()` /
   `catastrophic_reason` 的公开只读入口不在本波权限内；危险动作默认聚焦 Reject
   仍需 `kiana-permissions` 导出判定，之后在 `card_request.dangerous` 传入。
7. **TUI 审批卡未接管**：`kiana-interactive` 的 `InteractiveUiContext` 没有覆写
   `permission_card`，仍走默认两步 select；`render_core::approval::ApprovalCard`
   与 `ModalArbiter` 的接线属于 week 12 lane/后续波。默认路径无行为回归。
8. **`output_language` 未进 system prompt**：`LanguageSettings::output_rule` 未接入
   `kiana-resources`/`kiana-session`；UI locale 与模型输出语言分离仍只在
   `kiana-interactive` crate 内可用。
9. **瞬时消息抑制的边界**：如果扩展在 `message_end` 替换了召回消息正文，
   `pending_memory_contexts` 的精确匹配会失败，替换后的消息可能被持久化。
10. **frame/pending-user/modal 等 week 12 渲染核心未接线**：`KIANA_NEXT_RENDER_CORE`
    仍是实验开关，默认路径未切换（lane 已声明 partial）。
11. **索引 token 成本**：启用记忆后 `<memory>` section 最大 25 KB 会进入每次请求；
    对上下文预算敏感的会话需要显式关闭或调小 `MemoryConfig::limits`。

---

## 8. 给后续波的风险

- **默认模式下的记忆写死路**：受保护 `.kiana` 路径 + 同步 gate + 没有交互式审批，
  用户会看到 “memory write” 被拒；若要让工具真正可用，必须先把审批 UI 接到
  `WriteRequest`（stage → 用户选择 → `approve_write`/`approve`），并保持
  decision reason 与权限引擎一致。
- **memory 工具是 SDK custom tool**：外层权限按 `Other` 工具分类，read-only/default
  模式会先被工具级权限拦住；即使内层 gate 放行，外层也可能 Ask/Deny。后续要么给
  工具一个受控的 `ActionKind::Read/Write` 分类，要么在工具级规则里显式处理
  `memory`。
- **召回扫描成本**：每轮最多 200 个文件、每个 30 行，且是同步 IO；在慢盘/大 scope
  下会给首 token 增加延迟。side query 未接入时启发式质量也有限。
- **压缩刷新时机**：只在两个压缩提交成功点调用；扩展触发的其它历史重写（不经过
  `append_compaction_transactional`）不会刷新记忆快照，需要后续审计。
- **事件流**：`<memory-context>` 消息在 `MessageStart/End` 不对外可见，但
  `AgentEnd.messages` 仍可能包含它（pending 集合在 MessageEnd 已被消费）；TUI 若
  直接消费 `AgentEnd` 需要自行过滤。
- **审批卡默认回退是两步 select**：如果 UI 覆写 `permission_card` 但忘记处理
  `always_scopes` 为空（destructive MCP），必须返回 `AllowOnce`/`Reject`，不能
  自己发明持久批准。
- **Cargo.lock**：新增 `kiana-session -> kiana-memory` 依赖边由 cargo 自动写入；
  后续波不要手工回退该条目。
