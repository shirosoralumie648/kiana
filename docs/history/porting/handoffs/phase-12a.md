# 第 12 周（前半）wire-agent 接线清单

本波只修改 `crates/kiana-interactive/**`（新增 `src/render_core/`、`assets/locales/` 与
7 个 `tests/render_core_*.rs`），以及 `tests/common/mod.rs` 的 fixture 字段补齐。
**没有直接修改** `crates/kiana-session/**`、`crates/kiana-ext-api/**`、
`crates/kiana-resources/**`、根 `Cargo.toml` / `Cargo.lock`。下面是把本波能力接进现有
交互模式需要的步骤。所有新能力默认关闭（`KIANA_NEXT_RENDER_CORE=1` 才启用），
不做接线时现有 48 个组件的行为不变。

模块入口：`kiana_interactive::render_core::{approval,frame,history,i18n,modal,pending_user,transcript}`。

---

## 一、帧契约接线（最高优先级）

### 位置

- `crates/kiana-interactive/src/tui_renderer.rs`：`InteractiveTuiReference` 初始化/轮询；
- `crates/kiana-interactive/src/interactive_mode.rs` 的 `run()` 轮询循环（10ms tick）；
- `crates/kiana-interactive/src/interactive_mode/events.rs`：`AgentSessionEvent` 消费点。

### 1. 构造开关

```rust
if kiana_interactive::render_core::experimental_enabled() {
    // 新渲染核心；否则保持现有 InteractiveRenderer 路径。
}
```

开关只在启动时读一次；不要在一帧中间切换。

### 2. 终端 sink

`kiana_tui::terminal_colors::Terminal::write(&mut self, data: &str)` 没有 `Result`，
所以生产 sink 目前只能视为“进程内写入已接受”：

```rust
use kiana_interactive::render_core::frame::TerminalSink;
use kiana_tui::terminal_colors::Terminal;

struct ProcessTerminalSink<'a>(&'a mut dyn Terminal);

impl TerminalSink for ProcessTerminalSink<'_> {
    fn write_frame(&mut self, bytes: &str) -> std::io::Result<()> {
        self.0.write(bytes);
        Ok(())
    }
}
```

如果希望真实检测磁盘/管道写失败，需要给 `Terminal` 增加 `try_write`（`kiana-tui` 在
本波所有权内，但本波未改）。`FrameWriter` 已经支持失败路径：返回 `Err` 时不会推进
accepted id、不会 ack，下一帧 provider 会重发同一个 batch。

### 3. transcript provider 适配

`AgentSessionEvent` → `render_core::transcript::DurableEvent` 的映射建议：

| 会话事件 | durable / delta | 说明 |
|---|---|---|
| `EntryAppended`（user message） | `UserMessage` | 用 entry id 作为事件 id；同时喂给 pending queue（见第三节） |
| `MessageEnd`（assistant text） | `AssistantText` | id 用消息 id；如果之前有 provisional slot，id 必须一致 |
| `MessageUpdate`（assistant delta） | `DeltaChunk::text` | `anchor_seq` = 当前最后一条 durable 的 seq；`attempt` 来自 auto-retry attempt |
| `MessageStart`（thinking） / message 中的 thinking block | `Thought` | 独立条目 |
| `ToolExecutionStart` | `Action` | 事件 id 自造（`tool-execution:{tool_call_id}`）；`args` 原样保留 |
| `ToolExecutionEnd` | `Observation` | `action_id` 指向 start 的事件 id；`tool_call_id` 保持 |
| artifact / 错误条目 | `Artifact` / `Error` | group breaker |
| `AgentEnd` / `AgentSettled` | `Finish` | 结束组 |
| `BashExecutionUpdate` | `append_bash_delta`（可先忽略） | 当前 transcript 模块没有 bash 流式通道，可把增量当 tool observation 的追加文本 |

`TranscriptProjection` 只接收模型，不依赖 `AgentSessionEvent`；在
`handle_event` 里完成翻译。注意：

- **非 delta 事件先 `apply_durable`**（内部会 flush pending delta），再喂给 ledger；
- delta 用 `push_delta`，每个渲染 tick 调一次 `flush_due(now_ms)`；不要在每个 delta 上
  调 `flush_pending`，否则 3k chunk 会变成 3k 帧；
- `attempt` 必须来自真实 retry 计数（`AutoRetryStart.attempt`）；相同 attempt 追加，
  更高 attempt 覆盖，更低丢弃。

ledger/block 对应关系：

```rust
let block_id = ledger.add_block(render_rows_for_message(&message));
// 消息/tool 结束时：
ledger.settle(block_id);
```

active block 会在 viewport 里每帧重绘；settled 只在容量压力下退休；committed 由
`TranscriptFrameProvider` 在写端 ack 后自动推进。

### 4. 每帧

```rust
use kiana_interactive::render_core::frame::{FrameWriter, FrameWriterOptions, ViewportSize};

let size = ViewportSize { columns: terminal.columns(), rows: terminal.rows() };
writer.set_overlay(modal_arbiter.overlay_frame(size.columns));
match writer.render(&mut provider, size, &mut sink) {
    Ok(_) => {}
    Err(_) => { /* 下一 tick 重试；不要手动 ack */ }
}
```

默认建议：

- regular 模式：`TerminalMode::Inline`，`Multiplexer::Direct`（或按 TERM 探测选择
  `Tmux`/`Screen`；`Screen` 默认关闭 synchronized-output）；
- fullscreen 模式：现有 `TuiAltScreen` 继续负责备用屏。**不要**把
  `TerminalMode::AltScreen` 直接接在 fullscreen 上——该模式下 history 永远不被接受，
  只适合纯 viewport 的临时备用屏。若要替换 fullscreen，需要另写 resize/alternate
  buffer 生命周期（本波未做）。

### 5. 关停 flush

```rust
provider.begin_history_flush();
loop {
    let out = writer.render(&mut provider, size, &mut sink)?;
    if out.history_rows_written == 0 { break; }
}
```

`FrameWriter::request_replay(&mut provider)` 可触发一次完整账本重放（`/clear` 等），
replay 会先清屏（ED2+ED3）。

---

## 二、per-tool 审批卡接线

### 位置

- `crates/kiana-session/src/agent_session.rs` 的 `ask_for_approval`（约 3659 行）与
  `permission_refusal_inner` 的 `Ask` 分支（约 4330 行）；
- `crates/kiana-ext-api/src/ui.rs` 的 `ExtensionUiContext`；
- `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs`（UI 侧实现）；
- `crates/kiana-rpc-print` / `kiana-extensions` 的 no-op 实现（保持默认回退）。

### 问题

`AgentSessionEvent::PermissionRequested` 目前只有 `tool_name/action/summary/target/reason`，
没有 `args`。审批卡需要 `args` 才能渲染 diff、完整命令、域名、MCP 参数。不要把这些
参数塞进事件（W1 的事件脱敏契约明确避免完整命令/secret 进入事件流）。

### 建议改法（新增 trait 方法 + 默认回退）

在 `kiana-ext-api/src/ui.rs` 新增中立 payload（不要依赖 `kiana-permissions`）：

```rust
#[derive(Clone, Debug)]
pub struct PermissionCardRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub cwd: String,
    pub decision_reason: String,
    /// 可选作用域；空向量表示只用 Session/Project/Local 默认。
    pub always_scopes: Vec<PermissionScopeKind>,
    /// 引擎判定的危险动作。
    pub dangerous: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionScopeKind { Session, Local, Project }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionCardChoice {
    AllowOnce,
    AllowAlways(PermissionScopeKind),
    Reject { reason: Option<String> },
}

pub trait ExtensionUiContext: Send + Sync {
    // ...现有方法...

    /// 默认实现：退化成 select（RPC/print/扩展侧无需实现）。
    fn permission_card(
        &self,
        request: PermissionCardRequest,
    ) -> LocalBoxFuture<'_, Option<PermissionCardChoice>> {
        let title = format!("Permission required\n{}", request.decision_reason);
        let options = vec![
            "Allow once".to_string(),
            "Always allow (this session)".to_string(),
            "Always allow (this project)".to_string(),
            "Always allow (this machine)".to_string(),
            "Reject".to_string(),
        ];
        // 伪代码：调用 self.select(&title, &options, None)，把结果映射成 Choice。
    }
}
```

`Ask` 分支把完整 `PermissionRequest` 传进来：

```rust
let card_request = PermissionCardRequest {
    tool_name: request.tool_name.clone(),
    args: request.args.clone(),
    cwd: request.cwd.display().to_string(),
    decision_reason: reason.clone(),           // 与引擎一致
    always_scopes: vec![Session, Project, Local],
    dangerous: /* 见下 */,
};
let choice = dispatch_local(async move {
    ui_context.permission_card(card_request).await
}).await;
```

危险动作判定：`PermissionDecision::Ask.reason` 里的 catastrophic 原因、或
`kiana_permissions::ShellAnalysis` 的分析结果。`catastrophic_reason` 目前是
`pub(crate)`，需要 wire agent 在 `kiana-permissions` 导出一个只读判定（例如
`PermissionRequest::is_dangerous()`），不要复制规则。`dangerous=true` 时：
- `ApprovalCard` 默认焦点 Reject；
- `Enter` 对 Allow 项返回 `Ignored`；
- 接线层必须把 `confirm_dangerous()` 绑到 Ctrl+Enter / 二次确认等非 Enter 键，否则
  危险动作无法批准（这是有意的默认 Deny）。

UI 侧（`extension_ui`）新增 `UiRequest::PermissionCard`：

1. 构造 `ApprovalRequest::new(tool_name, args).with_cwd(...).with_decision_reason(...)`；
   `with_dangerous` / `with_always_scopes`（`ApprovalScope` 转 `PermissionScopeKind`）。
2. `ApprovalCard::build`，放进 `ModalArbiter::enqueue_approval`（见第四节）。
3. 把 `ApprovalDecision` 转成 `PermissionCardChoice`，resolve oneshot；取消/关闭按
   `Reject { reason: Some("Approval was cancelled.") }`（fail-closed）。
4. 无 UI / 非交互模式必须走默认 `select` 或直接拒绝，不能把 Ask 当 Allow。

会话侧把 `PermissionCardChoice` 映射回 `kiana_permissions::ApprovalDecision`；
`AllowAlways` 的作用域直接复用现有 `persist_approval`。decision reason 必须原样传递，
这是 DoD “decision reason 与引擎一致”的落点。

### 风险解释（sideQuery）

`ApprovalCard::begin_risk(now_ms)` → 异步模型调用 → `resolve_risk(Ok/Err)`。接线层：
- 在请求发出后 3 秒内没有结果就调用 `tick(now_ms)`，卡片自动隐藏；
- 调用失败必须 `resolve_risk(Err(()))`（静默）；
- 结果只能标 advisory；不要用它改变建议或默认焦点。
- sideQuery 执行通道需要 `kiana-ai`/session 提供（本波未做），可先不启用。

---

## 三、用户消息 pending 队列接线

### 位置

- `crates/kiana-interactive/src/interactive_mode/commands/mod.rs`（用户提交）；
- `crates/kiana-interactive/src/interactive_mode/events.rs`（`EntryAppended`）；
- `crates/kiana-interactive/src/interactive_mode/render.rs`（pending 气泡展示）。

### 步骤

1. 用户提交（按下回车、`session.prompt` 之前）：
   ```rust
   queue.observe_server_timestamp(&session_id, latest_entry_timestamp_ms);
   let pending_id = queue.enqueue(
       &session_id, &text, Some(&content_sent), image_urls, file_urls, now_ms,
   );
   ```
   `content_sent` 是实际发送给服务端的文本（含附件说明）；气泡显示 `text`。
2. `AgentSessionEvent::EntryAppended` 里 role=user 的条目：
   ```rust
   queue.observe_server_timestamp(&session_id, Some(entry_timestamp_ms));
   let consumed = queue.consume_matching(
       &session_id, &content, &entry_id, entry_timestamp_ms,
   );
   // consumed 为 Some 时：把 pending 气泡替换成 durable 条目（原地），不要追加第二份
   ```
   注意：`EntryAppended` 的 timestamp 必须来自服务端/会话条目，不要用本地时钟。
3. 渲染 tick（与帧 tick 同一个 10ms 循环）：`queue.tick(now_ms)`；返回 >0 时把对应
   pending 气泡标为 Failed，展示 Retry。
4. Retry：`queue.mark_sending(id, now_ms)` 后重新调用 `session.prompt`（同内容），
   成功回显后 `consume_matching` 会移除。
5. 会话切换：`queue.clear()` 或按会话过滤 `messages_for`，避免串台；云任务临时 id
   用 `reassign_conversation`。

确认逻辑已经在 `render_core::pending_user` 里锁定：trimmed 内容、严格大于围栏、
`conversation:event` 去重、150s watchdog、错误项可被迟到回显消费。宿主只需要提供
真实时间戳和 tick。

---

## 四、单一 modal / 审批队列接线

### 位置

- `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs`
  （`process_request` / `begin_extension_selector` / `push_extension_dialog`）；
- `crates/kiana-interactive/src/tui_renderer.rs` 的输入分发；
- 设置界面打开路径（`interactive_mode/commands/settings.rs`）。

### 步骤

1. 用 `ModalArbiter` 取代/包住现有对话框栈：
   - `ui.select` / `ui.confirm` / `ui.input` 不再各自挂多个对话框，统一转成
     `AskUserDialog` 或 `ApprovalCard` 入队；
   - `permission_card` → `enqueue_approval`；
   - 设置打开 → `open_settings(SettingsModal { lines })`；关闭 → `close_settings`。
2. 渲染：每帧只渲染 `arbiter.render(width)`（或作为 overlay 交给
   `FrameWriter::set_overlay`）。不要同时渲染 `active` 之外的 modal。
3. 输入：在 `TuiAltScreen::handle_terminal_input` / `handle_event` 的最前面调用
   `arbiter.route_input(...)`；返回 `Underlying` 才继续原路径，`Modal`/`Ignored` 都直接
   返回。这样鼠标/滚动/resize 在 modal 期间不会穿透到底层编辑器。
4. `Approve` 的 `resolve_active` 只接受 `ApprovalDecision`；ask-user 通过
   `route_input` 的返回值完成（`ModalResolution::AskUser`）；设置用 `close_settings`。
5. `visible_modal_count() <= 1` 是硬不变量：不要在别处再挂 dialog overlay。
6. `overlay_frame(width)` 的 `full_screen` 由 `set_alt_screen` 控制；overlay 永不进入
   history。

---

## 五、i18n 接线

### 位置

- `crates/kiana-interactive/src/interactive_mode.rs` / `render_core` 调用点；
- 设置读取：`kiana-config`（本波未改）新增 `ui.locale` / `output_language` 或复用现有
  language 设置；
- system prompt：`crates/kiana-resources/src/system_prompt.rs`
  （`build_system_prompt` / `build_system_prompt_options`）与
  `crates/kiana-session/src/agent_session.rs` 的 `rebuild_system_prompt`。

### 步骤

1. 启动时：
   ```rust
   let loader = LocaleLoader::new(
       dirs::home_dir().unwrap().join(".kiana/locales"),
       /* 发布包 locales 目录；没有就用内置资源 */ PathBuf::new(),
   );
   let translator = loader.load_translator(&language_settings.ui_locale);
   ```
   用户目录 > 内置目录 > 内置资源 > 英文回退。损坏文件只记 diagnostics。
2. UI 文案用 `translator.t("key", &[("index", "1")])` / `translator.ta("key")`；本波
   `render_core` 的卡片/队列文案可逐步替换为 key（当前为了独立测试仍是英文常量）。
3. **UI locale 与 output language 分离**：
   ```rust
   let settings = LanguageSettings::new(ui_locale, output_language);
   // output_language 只影响下面这条 system prompt：
   if let Some(rule) = settings.output_rule() {
       options.append_system_prompt.push(rule);
   }
   ```
   `output_language_system_rule("zh")` 已包含 “Chinese (简体中文)”；不要在 UI 语言切换时
   改 output language，反之亦然。
4. CI：在 CI 里跑
   ```text
   cargo test -p kiana-interactive --test render_core_i18n
   ```
   或调用 `locale_ci(&packs, &builtin_pack("en").unwrap())`。`strict` 语言包缺 key 会
   返回 `LocaleCiError`，`Display` 列出 missing/extra/untranslated key。内置 `zh` 已
   strict 并通过；新增 `zh-TW` 等语言包时在文件里写 `"strict": true`（JSON）或
   `strict = true`（TOML）。

---

## 六、其他注意事项

- `tests/common/mod.rs` 的 `CreateAgentSessionOptions` 已补 `loop_guard: None`,
  `budget_config: None`；如果 wire agent 又改了 session 的选项结构，请同步。
- 新模块不依赖全局主题；`ApprovalCardComponent` 目前输出纯文本行。若要样式化，在
  接线层用现有 `DynamicBorder` / `ThemedText` 包装，或在 `render_lines` 后按行着色。
- `FrameWriter` 的 `Multiplexer::Screen` 默认关闭 synchronized-output；tmux/direct 默认
  开启。`TERM` 探测逻辑未实现，接线层可先固定 `Direct`，再按 `$TERM` 选 `Screen`/
  `Tmux`。
- 本波没有实现 `AgentSessionEvent` 的完整映射表；映射时如果某个事件暂时无法表达，
  宁可丢给旧渲染路径，也不要伪造 durable 事件（finality 一旦提交不可撤回）。

---

## 第 12 周后半（week-12b）接线清单

第 12 周后半（Web search/fetch、IDE、daemon EventFeed、cron、发布硬化）的接线清单
单独写在 [`phase-12b.md`](phase-12b.md)，避免与前半的渲染/审批/i18n 内容混在一起。
摘要：

- Web 工具注册：建议把 `crates/kiana-coding-agent/src/web/` 下移到 `kiana-tools` 后注册
  `WebSearch`/`WebFetch`（避免依赖环）。
- IDE：启动时 `discover_ides` → `HttpIdeTransport` → `McpIdeClient::initialize`；
  `ide/diffAccepted`/`ide/diffRejected` 通知路由到 `DiffBroker`。
- daemon：`EventFeed::publish` 编码一次；SSE 用 `to_sse_frame`，WS 用 `to_ws_frame`；
  慢订阅由队列满自动移除；`latest_page` 的 `since` 交给 `subscribe(Some(since))`。
- cron：`FileCronStore` + `CronScheduler::tick(now_secs())`（启动一次 + 每 30s）；
  `JobDispatcher` 生产实现需要 `kiana-session` 的 AgentSession prompt 通道。
- 发布：workspace 全量测试/clippy/fmt、二进制冒烟与 `Cargo.lock` 同步由 wire agent 执行。
