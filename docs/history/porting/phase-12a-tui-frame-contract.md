# 第 12 周（前半）— TUI 帧契约与审批界面

日期：2026-11-07
对应 `docs/history/porting/plan.md` 第 12 周第 1–6 条（第 498–511 行）。

本波只修改 `crates/kiana-interactive/**`（含新增的 `src/render_core/`、语言包资源与测试），
以及 `crates/kiana-interactive/tests/common/mod.rs` 里一处已存在的 fixture 字段补齐。
没有修改 `crates/kiana-session/**`、`crates/kiana-ext-api/**`、`crates/kiana-resources/**`、
根 `Cargo.toml`、`Cargo.lock` 或 `docs/history/porting/plan.md`；跨 crate 的接线点全部写在
`docs/history/porting/handoffs/phase-12a.md`。

现有 48 个组件与 `tui_renderer` / `interactive_mode` 的默认渲染路径**没有改动**。
新能力由 `render_core::experimental_enabled()`（`KIANA_NEXT_RENDER_CORE=1`）控制，
接线层在开关打开时才切换到新模型与写端。

## 一、参考设计对照

| 参考项目 / 文件 | 借鉴的设计点 | Kiana 落点 |
|---|---|---|
| `oh-my-pi/packages/tui/src/tui.ts`（`HistoryBatch` / `TerminalFrameProvider` / `#emitPlanFrame`） | 每帧可选 history batch + 完整 viewport；batch 单调 id；只写 `id > accepted`；写成功后 ack；重复 batch 只重新 ack；replay 一次原子写；viewport 锚定剩余 history；光标在同步帧内 | `render_core/frame.rs`：`HistoryBatch`、`TerminalFramePlan`、`FrameProvider`、`FrameWriter::render`、`TerminalSink` |
| `oh-my-pi/docs/tui-core-renderer.md` | application 决定 finality；overlay 是屏幕坐标内容、永不进 history；行级 diff；宽度截断不抛异常 | `render_core/frame.rs` 的 `OverlayFrame` / `compose_overlay` / `fit_rows`；golden 测试 |
| `oh-my-pi/packages/tui/src/chrome/transcript-container.ts` | `active` / `settled` / `committed` 三态；容量压力才退休“最短 settled 前缀”；ack 前同 id 重发；每帧退休预算 | `render_core/history.rs`：`Finality`、`TranscriptLedger::peek_retirement` / `peek_flush` / `acknowledge_batch`、`TranscriptFrameProvider` |
| `OpenHands/src/utils/handle-event-for-ui.ts` | Action 被 Observation 原地替换（Think/Finish 的 observation 忽略）；provisional slot 的 `anchor_seq` 定位、`attempt` 高者覆盖、durable 到达退役 | `render_core/transcript.rs`：`DurableEvent`、`TranscriptProjection::apply_durable` / `push_delta` / `open_slot` |
| `OpenHands/src/stores/optimistic-user-message-store.ts` | trimmed 内容 + 服务端时间戳围栏确认；`conversation:event` 去重；150s watchdog 转可重试 error；失败后迟到回显仍可消费 | `render_core/pending_user.rs`：`PendingUserQueue::consume_matching` / `tick` / `mark_sending` |
| `ClaudeCode/src/components/permissions/**`（Bash / FileEdit / FileWrite / WebFetch / Skill / Fallback） | per-tool 卡片内容：命令/重定向/子命令、diff、写入内容、域名、脱敏参数、fallback | `render_core/approval.rs`：`ApprovalCard::build`、`analyze_bash`、`diff_body`、`redact_args` |
| `ClaudeCode/src/utils/permissions/permissionExplainer.ts` | advisory 风险解释结构 `riskLevel/explanation/reasoning/risk`；失败静默 | `render_core/approval.rs`：`RiskAdvisory` / `RiskAdvisoryState` / `tick` |
| `ClaudeCode/src/tools/AskUserQuestionTool/prompt.ts`、`AskUserQuestionPermissionRequest/**` | 单选/多选、常驻 Other、All；多问题逐步推进 | `render_core/modal.rs`：`AskUserQuestion` / `AskUserDialog` |
| `qwen-code/packages/cli/src/i18n/{index,languages,mustTranslateKeys}.ts` | 用户目录 > 内置 > 英文回退；strict locale key parity；`MUST_TRANSLATE_KEYS`；`{{param}}` 插值；多行数组 | `render_core/i18n.rs` + `assets/locales/{en,zh}.json` |
| `qwen-code/packages/cli/src/ui/components/messages/ToolConfirmationMessage.tsx` | 确认卡的选项顺序（Allow once / Always / Reject）与“显示建议而非直接执行” | `ApprovalRequest::suggestions` / `ApprovalCard::render_lines`（选项编号 1..N） |

只借鉴行为与状态机；没有复制参考项目代码（其中 ClaudeCode 为闭源/受限许可项目，
qwen-code 为 Apache-2.0，oh-my-pi/OpenHands 为各自许可项目；本实现全部重写）。

## 二、改了什么

### 1. 帧契约（`render_core/frame.rs`）

- `ViewportSize`、`HistoryBatch`（`Append`/`Replay` + 单调 `id`）、`TerminalFramePlan`、
  `FrameProvider`（`render_frame` / `acknowledge_history` / `begin_history_replay` /
  `begin_history_flush`）、`TerminalSink`（可注入；测试用 `StringSink` / `FailingSink`）。
- `FrameWriter`：
  - 只接受 `id > accepted_history_id`；旧/重复 batch 只重新 ack，不二次写；
  - 写出成功后（`TerminalSink` 返回 `Ok`）才推进 accepted id 并 ack；写失败返回
    `FrameWriteError::Io`，状态不推进；
  - 行级 diff：仅重写变化行，单字符变化 = 1 次内容移动 + 1 次行重写；
  - 有 history 的帧先擦旧 viewport，再从旧 viewport 顶部写 history + viewport；
  - `Replay` 先 `ED2+ED3`，再写完整账本 + 最终 viewport，一次 `write`；
  - overlay 通过 `set_overlay` 合成进 viewport，永不进入 history；
  - 光标标记在行处理后提取，硬件光标定位位于 `ESC[?2026h` 与 `ESC[?2026l` 之间；
  - 备用屏模式忽略 history（不写、不 ack），首次渲染写 `?1049h` + `?7l`，`stop` 写
    `?7h` + `?1049l`；
  - 超宽行按 ANSI-aware 宽度截断，不 panic。
- `Multiplexer::{Direct,Tmux,Screen}` 决定默认 synchronized-output 能力；golden 测试锁定
  Screen / Tmux / AltScreen 的固定转义序列。

### 2. finality 账本（`render_core/history.rs`）

- `Finality::{Active,Settled,Committed}`；`TranscriptLedger` 保存 block 状态与已经提交的
  scrollback 行。
- `peek_retirement(width, capacity)`：仅在 `live_rows > capacity` 时，从最老 block 起只吃
  连续 `Settled` 前缀，找到“退休后剩余尾部能放进 capacity”的**最短**前缀；遇到
  `Active`/`Committed` 立即停止。每帧最多
  `DEFAULT_MAX_RETIRE_ROWS_PER_BATCH = 256` 行（至少一个 block），余下后续帧继续。
- batch 在 `acknowledge_batch` 之前以同一 id 原样重发；ack 后前缀 block 变
  `Committed`。
- `begin_replay` / `peek_replay` 返回完整 `committed_rows` 的新 id `Replay` batch；
  `begin_flush` / `peek_flush` 退休所有连续 settled block。
- `TranscriptFrameProvider` 把账本接成 `FrameProvider`：viewport = live rows 的尾部
  `rows` 行；history = replay / flush / pressure 三者之一。

### 3. transcript 投影（`render_core/transcript.rs`）

- `DurableEvent`（user / assistant / thought / action / observation / artifact / error /
  finish）与 `TranscriptProjection`。
- Action 先建 `ToolCard`；Observation 按 `action_id` 原地填 observation，**一张卡**。
  Think/Finish 的 observation 被忽略（thought 提升、finish 不重复）。
- `groups()` 动态重建：相邻 ToolCard 归入同一个 `ToolRun`；artifact / error / finish
  各自成组并打断 tool run。
- `open_slot(id, anchor_seq, attempt)` / `push_delta` / `flush_due` / `flush_pending`：
  - provisional slot 与最终 durable 事件同 id；`anchor_seq` 定位（同 anchor 的 slot 按
    到达顺序/锚点稳定排序）；durable 到达时原地退役；
  - 更高 attempt 重置内容，低/相等 attempt 追加或丢弃；
  - delta 只排队，`flush_due` 按最小帧间隔（默认 16ms）提交；任何非 delta 事件先
    flush，控制事件不会与 delta 乱序；3k chunk 不会产生 3k 帧。

### 4. 乐观用户消息（`render_core/pending_user.rs`）

- `PendingUserQueue`：`enqueue` / `mark_error` / `mark_sending` / `remove` / `clear` /
  `reassign_conversation` / `tick` / `consume_matching`。
- 确认条件：同会话 + `content.trim()` 相等 + 回显时间戳严格大于该会话的服务端围栏
  （`observe_server_timestamp(conversation, ts)`，按会话隔离）。
- `conversation:event_id` 去重，断线重放的确认事件不会二次消费。
- `PENDING_MESSAGE_TIMEOUT_MS = 150_000`；`tick` 把超时 `Sending` 转
  `Error { "Send timed out" }`（`retryable()`），`mark_sending` 刷新围栏与计时。

### 5. per-tool 审批卡（`render_core/approval.rs`）

- `ApprovalRequest`（tool/args/cwd/decision_reason/always_scopes/dangerous）→
  `ApprovalCard`：
  - Bash/PowerShell：Command、Redirects（`>`/`>>`/`<`/`2>&1`/heredoc/带引号目标）、
    Subcommands（`&&`/`||`/`;`/`|`/换行，引号内不拆）；
  - FileEdit：路径 + 行级 diff（公共前后缀，`-`/`+`/上下文）；
  - FileWrite：路径 + Create/Overwrite + 内容预览（40 行封顶）；
  - WebFetch：只显示域名（不显示 path/query）；
  - MCP：server/tool + 脱敏参数；
  - Skill：skill 名 + prompt 预览；
  - 未知：工具名 + 脱敏 JSON。
- suggestions 固定顺序：`Allow once` → 每个 `Always allow (scope)` → `Reject`。
- `decision_reason` 原样展示（与权限引擎一致）。
- 危险动作默认焦点 = `Reject`；`Enter` 在危险动作上对 Allow 项返回 `Ignored`，
  只能通过 `confirm_dangerous()`（接线层绑定非 Enter 键）显式批准。
- `RiskAdvisory` 是 advisory：`begin_risk` → `Loading`；`resolve_risk(Err)` 立即静默
  隐藏；`tick` 在 3s（`RISK_ADVISORY_TIMEOUT_MS`）后隐藏仍无结果的 Loading。
- `ApprovalCardComponent` 实现 `kiana_tui::tui::Component`，可直接挂进现有 TUI。

### 6. modal 仲裁 + ask-user（`render_core/modal.rs`）

- `ModalArbiter`：活动 modal 最多一个（`visible_modal_count() <= 1`）。
- 审批 FIFO；活动审批显示 `Approval {index}/{total}`（`total` 含活动项），解决后下一个
  变 `1/{n-1}`。
- `open_settings` 抢占当前 modal：被抢占的审批放回队首（ask-user 放回自己的队首），
  `close_settings` 恢复；不丢权限项。
- ask-user：`Single` / `Multi`（All + Other 自定义文本）/ `Text` / `YesNo`；多问题逐个
  推进；`Other` 常驻。
- `route_input`：有 modal 时键盘进入 modal；Mouse / Scroll / Resize 返回 `Ignored`，
  底层组件收不到；备用屏标记同时保存。
- `overlay_frame(width)` 产出屏幕坐标 overlay，交给 `FrameWriter::set_overlay` 后永不
  进入 history。

### 7. i18n（`render_core/i18n.rs` + `assets/locales/`）

- `Translator::t(key, params)`（`{{name}}` 插值）与 `ta(key)`（多行数组）。
- 数据文件：JSON；或 TOML 子集（`[strings]`、`key = "text"`、`key = ["a","b"]`、
  `#` 注释、常见转义）。**不执行用户 JS**；非字符串值忽略。
- `LocaleLoader`：用户目录覆盖内置目录，内置目录覆盖内置资源，字段缺失回退英文内置包，
  最后回退 key；损坏的用户/内置文件只记 diagnostics，不阻塞启动。
- `check_parity` / `locale_ci`：strict locale 必须 key 集合与英文一致，且
  `MUST_TRANSLATE_KEYS` 不能等于英文；`LocaleCiError` 列出所有缺失/多余/未翻译 key。
- `LanguageSettings { ui_locale, output_language }` 分离 UI 语言与模型回答语言；
  `output_language_system_rule("zh")` 返回含 “Chinese (简体中文)” 的 system prompt 片段。
- `resolve_supported_locale` 支持 `zh_CN.UTF-8` → `zh`、`zh-TW` → `zh-TW` 等。
- 内置 `en` / `zh` 两个包；zh 为 strict，`locale_ci(&[zh], &en)` 通过。

### 8. 测试夹具

`crates/kiana-interactive/tests/common/mod.rs` 的 `CreateAgentSessionOptions` 初始化补了
`loop_guard: None, budget_config: None`。这是其他周给 session 增加字段后遗留的编译错误，
与本波功能无关；不补则 `cargo test -p kiana-interactive --lib` 无法编译。

## 三、测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana` 下执行。

```text
$ cargo check -p kiana-interactive
cargo check (1 crates compiled)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.37s

$ cargo test -p kiana-interactive --test render_core_history \
    --test render_core_frame --test render_core_transcript \
    --test render_core_pending --test render_core_approval \
    --test render_core_modal --test render_core_i18n
test result: ok.  9 passed (render_core_history)
test result: ok. 11 passed (render_core_frame)
test result: ok. 10 passed (render_core_transcript)
test result: ok.  8 passed (render_core_pending)
test result: ok. 13 passed (render_core_approval)
test result: ok. 11 passed (render_core_modal)
test result: ok. 11 passed (render_core_i18n)
合计 73 passed; 0 failed; 0 ignored

$ cargo test -p kiana-interactive
suites=46 passed=762 failed=0 ignored=0

$ cargo fmt -p kiana-interactive -- --check
（无输出，退出码 0）
```

覆盖的 DoD 关键场景：

- `dropped_ack_batch_is_not_written_twice`：ack 丢失后重发同一 `HistoryBatch`，
  scrollback 只出现一次；
- `ten_thousand_line_transcript_survives_resize_cycles`：10k 行在 80×24 / 120×40 往返
  resize 后，committed + viewport 的非空行与逻辑 transcript 完全一致；
- `single_char_change_is_one_move_and_one_rewrite`：单字符变化 = 1 次内容移动 +
  1 次行重写 + 1 次 `write`；
- `screen_mode_has_fixed_golden_without_synchronized_output` /
  `tmux_mode_wraps_frame_in_synchronized_output` / `alt_screen_golden_and_history_is_never_acked`：
  固定转义 golden；
- `overwide_row_is_truncated_not_panicking` / `write_failure_does_not_ack_and_retry_succeeds`：
  超宽行与写失败；
- `action_and_observation_render_one_tool_card` /
  `consecutive_tool_runs_fold_into_one_group` / `provisional_slot_uses_anchor_seq_and_retires_on_durable` /
  `deltas_are_batched_per_frame_and_control_events_flush_first`；
- `confirmation_id_dedupe_prevents_double_consume_on_replay` /
  `watchdog_turns_sending_into_retryable_error_after_150s`；
- `bash_card_shows_command_redirects_and_subcommands` /
  `file_edit_card_renders_diff` / `file_write_card_shows_content_and_action` /
  `webfetch_card_shows_only_domain` / `mcp_card_redacts_secrets` /
  `dangerous_action_focuses_reject_and_enter_never_approves` /
  `risk_advisory_is_optional_advisory_and_hides_after_3s`；
- `three_queued_approvals_render_one_modal_with_1_of_3` /
  `settings_preempts_without_losing_approvals` / `ask_user_multi_supports_all_and_other`；
- `builtin_packs_are_valid_and_strict_zh_passes_parity` /
  `strict_parity_failure_lists_missing_and_untranslated_keys` /
  `missing_key_falls_back_to_english_then_key_without_panicking` /
  `locale_detection_and_ui_output_language_are_separate`。

## 四、未完成项（partial / 留给 wire agent）

1. **未接线**：新的 frame/provider/transcript/pending/approval/modal/i18n 都是独立模块，
   默认渲染路径未切换。接线清单见 `docs/history/porting/handoffs/phase-12a.md`。
2. **per-tool 审批卡拿不到参数**：`AgentSessionEvent::PermissionRequested` 目前只有
   `tool_name/summary/target/reason`，没有 `args`；本模块的 `ApprovalRequest` 需要完整
   `args` 才能渲染 diff/命令/域名。手交单建议给 `ExtensionUiContext` 增加
   `permission_card` 请求（或在事件里带 args），由 session 把
   `PermissionRequest.args` 传进来。
3. **风险解释没有真正的 sideQuery 执行**：本波只做 UI 状态机（Loading/Shown/Hidden、
   3s 超时、失败静默）。真正的模型调用与 3s 超时取消需要 `kiana-ai`/session 提供 side
   query 通道；接线层在 3s 内没有结果时调用 `tick` 隐藏即可。
4. **output_language 未进 system prompt**：`output_language_system_rule` 已实现并测试，
   但 system prompt 构造在 `kiana-resources::system_prompt` / `kiana-session`（本波所有权
   之外）；需 wire agent 把该片段追加到 system prompt（见 handoff）。
5. **i18n 仅内置 en/zh**：其它 locale 回退英文；`zh-TW` 只有在提供语言包并标 `strict`
   后才参与 CI。UI 文案目前只在 `render_core` 的测试/新组件里使用，旧 48 个组件未批量
   替换（按任务“不要整体重写”）。
6. **modal 鼠标交互**：只实现“隔离”（鼠标/滚动不穿透），没有在 modal 内做鼠标命中；
   键盘完整。
7. **帧写端简化**：没有移植 oh-my-pi 的图片预算、CPR 锚点恢复、multiplexer 探测、
   resize replay 的 bottom-split、append-only stable head。当前 replay 是清屏 + 完整
   账本 + viewport 的简化原子写，满足“不重复、一次写”的 DoD，但不是图片/滚动位置
   级别的完整产品实现。
8. **pending 队列没有真实 timer**：`tick(now_ms)` 由宿主轮询调用；接线层需在 10ms
   轮询循环里调用。
9. **`resolve_active` 只处理审批**；ask-user 通过 `route_input` 完成。设置关闭走
   `close_settings`。这些是刻意的 API 分工。

## 五、风险

- `FrameWriter` 的物理滚动模型是“写 history 后 viewport 跟随”，没有模拟终端 scrollback
  的精确行为；golden 锁定了本实现的序列，换实现时必须同步更新 golden。真实终端上的
  resize + history 重放仍需 tmux/screen 实机验证。
- 历史 batch 一旦 ack 就视为 committed；如果 provider 在 ack 后改变内容，写端不会补救
  （这是契约要求，也是风险所在）。provider 必须在 ack 前冻结行字节。
- `ApprovalCard` 目前是纯文本行，样式由接线层的 Component/Theme 包装；直接挂进现有 TUI
  时不会自动使用主题色。
- `analyze_bash` 是启发式 shell 扫描，不做完整 AST；复杂命令（嵌套 heredoc、函数定义、
  `eval`）的重定向/子命令展示可能不完整；权限引擎的 `ShellAnalysis` 仍是权威。
- `PendingUserQueue` 的围栏依赖调用方及时 `observe_server_timestamp`；如果宿主忘记更新，
  旧围栏会拒绝合法回显（表现为 pending 不消费直到 watchdog）。
- i18n TOML 解析只支持文档化的子集；完整 TOML（表数组、多行字符串）不支持，语言包应
  优先用 JSON。
- `MUST_TRANSLATE_KEYS` 的“与英文不同”判断是文本相等；某些语言中产品术语可能合理地
  保持英文，后续需要 allowlist 而不是放宽整条规则。

---

# 第 12 周（后半）— Web / IDE / daemon / cron + 发布硬化

日期：2026-11-07
对应 `docs/history/porting/plan.md` 第 12 周第 7–11 条（第 512–516 行）。

本波只修改：

- `crates/kiana-coding-agent/src/**`（新增 `web/`、`ide/`、`cron/`）；
- `crates/kiana-rpc-print/**`（新增 `src/event_feed.rs`）；
- `README.md`、`ARCHITECTURE.md`、`docs/ARCHITECTURE_OVERVIEW.md`、`docs/START_HERE.md`、
  `docs/release/**`。

没有修改 `crates/kiana-session/**`、`crates/kiana-durable/**`、`crates/kiana-tools/**`、
根 `Cargo.toml`、`Cargo.lock`。跨 crate 的接线点见
`docs/history/porting/handoffs/phase-12b.md`。

## 一、参考设计对照

| 参考项目 / 文件 | 借鉴的设计点 | Kiana 落点 |
|---|---|---|
| `oh-my-pi/packages/coding-agent/src/web/search/{index,types,providers/base}.ts` | 有序候选链（首选/fallback/其余）、统一 `SearchResponse`/失败明细、逐引擎失败后顺序切换 | `web/search.rs`：`SearchEngine`、`EngineRole`、`SearchChain::run`、`SearchResponse`/`EngineFailure` |
| `ClaudeCode/src/tools/WebSearchTool/WebSearchTool.ts` | 全部失败时聚合“引擎 + 原因”，401/403 不重试、429/5xx 可重试 | `web/search.rs` 的 `get_json`/`SearchChainError`；`SearchEngineError::is_retryable` |
| `gemini-cli/packages/core/src/tools/{web-search,web-fetch}.ts` | 官方 API 优先、fetch 结果转文本、按长度截断 | `web/search.rs` 的 Brave/Google CSE/Bing 适配器；`web/fetch.rs` 的 `html_to_text`/`truncate_chars` |
| `ClaudeCode/src/tools/WebFetchTool/{WebFetchTool,utils}.ts` | URL 预检（scheme/凭据/私网）、逐 URL 独立错误、内容按上限截断 | `web/ssrf.rs` 的三层校验；`web/fetch.rs` 的 `FetchBatch`/`FetchFailure` |
| `ClaudeCode/src/utils/ide.ts` | `~/.claude/ide/<port>.lock`、`workspaceFolders`/`pid`/`authToken`/`transport`、dead pid 与 workspace 不符清理、mtime 最新优先 | `ide/lockfile.rs`：`IdeLockfile`、`discover_ides`、`cleanup_stale_locks`、`process_is_alive` |
| `ClaudeCode/src/hooks/useDiffInIDE.ts` | `ide/openDiff` 往返、`ide/diffAccepted` 返回用户最终内容、`ide/diffRejected` 关闭、readOnly 显式降级 | `ide/protocol.rs`：`DiffBroker::open_diff`、`DiffOutcome`、`IdeError::ReadOnlyDiff` |
| `opencode/packages/client/src/shared-events.ts`、`qwen-code/packages/cli/src/serve/generation-sse.ts` | SSE 单帧、事件 id、慢客户端隔离 | `kiana-rpc-print/src/event_feed.rs`：`FeedEvent::to_sse_frame`、`Subscription` dropping queue |
| `qwen-code/packages/cli/src/serve/sse-last-event-id.ts` | 断线 `Last-Event-ID`/`since` 只补缺失事件 | `event_feed.rs` 的 `subscribe(Some(since))`、`SeqCursor`、`latest_page` |
| `ClaudeCode/src/utils/cron.ts` | 5-field cron、wildcard/step/range/list、DOM/DOW “任一匹配”、366 天边界 | `cron/schedule.rs` 的 `CronExpression::next_after`/`parse_cron` |
| `ClaudeCode/src/utils/cronScheduler.ts`、`cronTasks.ts` | `nextRunAt` 推进、one-shot、启动追赶、missed | `cron/scheduler.rs` 的 `tick`/`advance`/`record_missed` |
| `deer-flow/.../persistence/scheduled_tasks/sql.py`、`scheduled_task_runs/finalization.py` | 先持久化 `next_run_at` 再 launch、owner/lease 存活证明、终态推进 | `cron/store.rs` 的 `CronJob.pending_slot`/`pending_owner`、`ProcessFingerprint`、`CronExecution` 账本 |
| `cline/sdk/packages/core/src/cron/runner/cron-runner.ts`、`store/sqlite-cron-store.ts` | claim token、delivery 只结算一次、慢任务不重复启动 | `cron/scheduler.rs` 的 `claim_delivery`/`mark_delivered`；`cron/store.rs` 的原子 claim |

只借鉴行为与状态机；没有复制参考代码（ClaudeCode 为闭源/受限许可项目，其余为各自许可项目；
实现全部重写）。

## 二、改了什么

### 1. Web 搜索与抓取（`crates/kiana-coding-agent/src/web/`）

- `ssrf.rs`：URL 静态校验（仅 http/https、必须有 host、拒绝内嵌凭据、IP 字面量直接判定、
  云元数据域名）；IP 分类覆盖 10/8、172.16/12、192.168/16、100.64/10、169.254/16、
  198.18/15、0/8、224/4、240/4、IPv6 ULA/link-local/组播与 IPv4-mapped；WebFetch 额外
  拒绝 loopback。`resolve_public_host` 在连接前重新解析并拒绝任一被禁地址；
  `validate_redirect_target` 对最终 URL 再校验。
- `http.rs`：`WebTransport` trait + `ReqwestWebTransport`（复用
  `kiana_core::http_dispatcher::shared_http_client`，不新增依赖）；请求带
  `max_response_bytes` 硬上限；测试注入脚本化传输，单测不访问网络。
- `search.rs`：`SearchEngine` trait、`EngineRole`、`SearchChain`（首选 → fallback → 其余）；
  单引擎默认 3 次尝试（指数退避 250ms…2s），整链默认最多 6 次额外重试；
  `SearchResult`/`SearchResponse` 在所有引擎间同一 schema；全部失败返回
  `SearchChainError`（含每个引擎的尝试次数与原因）。官方 API 适配器：Brave
  (`X-Subscription-Token`)、Google CSE (`key`/`cx`)、Bing (`Ocp-Apim-Subscription-Key`)。
  scraper 未实现（见 gaps）。
- `fetch.rs`：`fetch_urls` 并行抓取（JoinSet），结果按输入顺序；每 URL 独立
  SSRF/DNS 校验、状态码检查、最终 URL 校验、content-type 过滤（HTML/text/JSON/XML），
  HTML → 文本，按 Unicode 字符截断到 10k（默认）并标记 `truncated`；非法 URL 不进入
  传输层。

### 2. 编辑器集成（`crates/kiana-coding-agent/src/ide/`）

- `lockfile.rs`：`IdeLockfile` 支持 JSON（`workspacePath`/`workspaceFolders`/`port`/
  `authToken`/`ppid`/`pid`/`ideName`/`transport`/`runningInWindows`）与旧版“每行一个
  路径”；端口以文件名为准（`45123.lock`）；`discover_ides` 只返回
  “pid/ppid 存活 + workspace 等于或包含当前目录”的候选，按 mtime 新→旧排序；
  `cleanup_stale_locks` 删除无法解析、dead pid、workspace 不符的 lockfile。
- `protocol.rs`：
  - `IdeTransport` trait + `HttpIdeTransport`：`POST http://127.0.0.1:<port>/mcp`，
    `Authorization: Bearer`，`accept: application/json, text/event-stream`；响应支持
    JSON 与 SSE（取第一条 `data:` 行）。错误文本不含 token。
  - `McpIdeClient::initialize` 发送 MCP `initialize` + `notifications/initialized`。
  - `build_context_update`：≤10 个文件、JSON ≤16 KiB；先按 UTF-8 边界截断内容，仍超限
    则丢尾部文件，`truncated: true`。
  - `DiffBroker::open_diff`：readOnly 立即 `IdeError::ReadOnlyDiff`（不发请求、不写文件）；
    `{accepted, content}` 直接返回用户最终内容；`{accepted:false}`/`{rejected:true}` 返回
    `DiffOutcome::Rejected`；`pending`/`{}` 等待 `ide/diffAccepted` / `ide/diffRejected`
    （300s 超时，失败/超时注销 channel）。

### 3. daemon 多客户端（`crates/kiana-rpc-print/src/event_feed.rs`）

- `EventFeed::publish` 对 typed payload **只序列化一次**，得到 `FeedEvent`
  （`json` 同时是 WS 单帧，`to_sse_frame` 是唯一 `data:` 行的 SSE 帧）。
- 每个 `Subscription` 一条 4096 的 dropping queue；发布时队列满 → 只把该订阅标
  failed、清空并移出 feed，其它订阅与进程内存不受影响。
- `EventChannel::Durable` 进入有界 replay log（默认 4096），`Volatile` 不重放；
  `subscribe(Some(since))` 只补 `seq > since` 的 durable 事件，队列顺序为
  `server.connected` → 补放 → live；补放超过队列容量时只保留最近的并返回 `gap`。
- `server.connected`（含 `heartbeatIntervalSeconds: 15`）与
  `publish_heartbeat`/`HEARTBEAT_INTERVAL_SECS = 15`。
- `latest_page(limit)` 返回最新 durable 页与 `since`；把它交给 `subscribe(Some(since))`
  即完成 REST → WS 交接。
- `SeqCursor`：最高连续 seq + ahead-set，上限 `MAX_AHEAD = 1024`；超限返回
  `GapTooLarge`，调用方应重新走 REST 最新页。

### 4. 无人值守 cron（`crates/kiana-coding-agent/src/cron/`）

- `time.rs`：epoch 秒、Hinnant civil/day 换算、ISO 8601（日期/时间/小数秒/`Z`/`±HH:MM`）、
  `FixedOffset`（UTC/Local/±HH:MM；IANA 名显式报错）。
- `schedule.rs`：`ScheduleSpec` 支持对象
  `{"duration"|"every"|"cron"|"at"}` 与字符串（cron → ISO → duration 自动判定）；
  5-field cron 支持 `*`、`*/n`、`a-b`、`a-b/n`、逗号列表、7=Sunday；DOM/DOW 同时受限
  时任一匹配；最多向前 366 天。`duration` 相对上一槽位，`every` 对齐 epoch 整倍数；
  catch-up 半周期：间隔/2，cron 取相邻两次间隔/2。
- `store.rs`：`CronJob`（skills/model/script/context_from/workdir/timezone/grace_minutes/
  next_run_at/pending_slot/pending_owner/version）、`CronExecution` 账本
  （claimed/running/completed/failed/unknown/cancelled/missed）、`DeliveryRecord`；
  `CronStore` 契约（CAS job、原子 claim delivery）；`MemoryCronStore` 与
  `FileCronStore`（每 job/execution 一个 JSON、flock 串行化、临时文件 + rename）；
  `ProcessFingerprint` 用 pid + `/proc/<pid>/stat` starttime 证明 owner 死亡；
  `resolve_script_path` 拒绝绝对路径/`..`/符号链接逃逸。
- `scheduler.rs`：`tick` 先扫描 owner 已死的 active execution → unknown；
  有 `pending_slot` 时按“已有 active execution（owner 活 → 跳过 / owner 死 → unknown）/
  已有终态 execution → 清 pending 推进 / 无 execution → grace 内恰好 dispatch 一次或
  记 missed”处理；普通 due job 先 CAS 写 `next_run_at`（已推进）+ `pending_slot`，再写
  execution 并 dispatch；超过 `next_run_at + grace_minutes` 记 missed；catch-up 延迟
  clamp 到 `[120s, 2h]`。delivery 每行最多 claim 一次；claim 后 owner 死亡 →
  `unknown` 且 `mark_delivered` 拒绝。one-shot 成功/失败/错过之后把 `enabled` 置
  `false`，避免下一次 tick 把过去的 `at` 重新初始化成 `next_run_at` 后二次触发。

### 5. 发布硬化

- `README.md`、`ARCHITECTURE.md`、`docs/ARCHITECTURE_OVERVIEW.md`、`docs/START_HERE.md` 的 crate
  数量从 30 更新为 32，层表加入 `kiana-hooks`（L6）与 `kiana-memory`（L6）。
- 新增 `docs/release/checklist.md`：默认配置、迁移、回滚、构建/测试命令与已知风险。
- 本波只跑所有权内的两个包；workspace 全量回归由 wire agent 执行（见 gaps）。

## 三、测试命令与真实结果

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`，在 `/home/ctyun/code/kiana` 下执行。

```text
$ cargo check -p kiana-coding-agent -p kiana-rpc-print
Finished `dev` profile ... (0 errors)

$ cargo test -p kiana-coding-agent --lib
133 passed; 0 failed; 4 ignored

$ cargo test -p kiana-rpc-print --lib
44 passed; 0 failed; 0 ignored

$ cargo test -p kiana-coding-agent -p kiana-rpc-print
41 suites: 637 passed; 0 failed; 29 ignored
（ignored 全部是既有“需要 KIANA_BIN 二进制”的用例，未新增 ignore）
```

覆盖的 DoD 关键场景：

- `primary_failure_falls_back_to_next_engine_with_same_schema` /
  `retryable_engine_is_retried_three_times_before_fallback` /
  `all_failures_produce_actionable_error_with_engine_and_reason` /
  `results_are_truncated_to_config_limit`；
- `private_and_metadata_urls_never_reach_the_transport`（127.0.0.1、169.254.169.254、
  file://）、`blocked_redirect_final_url_is_rejected`、`long_pages_are_truncated_to_ten_thousand_chars`；
- `discovers_only_live_matching_workspaces_and_sorts_by_mtime`、
  `invalid_lockfiles_are_removed_by_cleanup`、`process_liveness_for_current_and_impossible_pids`；
- `read_only_diff_degrades_explicitly_without_any_request`、
  `accepted_response_returns_user_final_content`、
  `pending_diff_is_resolved_by_diff_accepted_notification`、
  `context_update_caps_files_and_bytes`；
- `slow_subscriber_is_failed_without_affecting_fast_subscriber`、
  `reconnect_since_replays_only_missing_durable_events`、
  `latest_page_hands_off_to_ws_since_without_duplicates`、
  `bounded_durable_log_reports_gap`、`seq_cursor_handles_out_of_order_and_bounds_ahead_set`；
- `one_shot_fires_once_and_stops`、
  `next_run_at_and_pending_slot_are_persisted_before_dispatch`、
  `pending_slot_without_execution_is_recovered_exactly_once`、
  `expired_pending_slot_is_missed_not_dispatched`、
  `dead_owner_execution_becomes_unknown_and_is_never_redelivered`、
  `overdue_jobs_are_missed_and_next_run_uses_catch_up_clamp`、
  `delivery_is_claimed_once_and_unknown_after_owner_death`、
  `process_fingerprint_detects_pid_reuse_via_start_time`。

## 四、未完成项（partial / 留给 wire agent）

1. **Web 工具未注册进工具表**：`web` 模块是库层实现，尚未作为 `WebSearch`/`WebFetch`
   内建工具暴露给模型。`kiana-tools` 不在本波所有权内，且 `kiana-tools` 不能反向依赖
   `kiana-coding-agent`（依赖环）。接线方式见 handoff：在 `kiana-builtin-extensions` 或
   `kiana-tools` 中复用本模块的 `SearchChain`/`fetch_urls`，或把 `web` 下移到
   `kiana-tools`。
2. **IDE 未接入启动流程**：`discover_ides`/`HttpIdeTransport`/`DiffBroker` 都是库层；
   `app.rs` 尚未在启动时发现 lockfile、推送 `ide/contextUpdate` 或把
   `ide/diffAccepted` 通知路由到 broker。接线点见 handoff。
3. **daemon 网络服务未实现**：`EventFeed` 只提供帧与队列，没有 SSE/WS listener。
   任务允许“扩展现有 kiana-rpc-print、不新建 crate”，本波完成库层；实际 HTTP/WS
   服务器需要 `kiana-rpc-print` 增加 `hyper`/`tokio-tungstenite` 依赖（Cargo.toml 不在
   本波所有权内），已写 handoff。
4. **cron 未接会话**：`JobDispatcher` 的生产实现（把 `DispatchRequest` 变成
   `AgentSession` prompt、注入 skills/model/context_from、执行受管脚本）在
   `kiana-session`，本波不能修改。handoff 给出所需 API 与调用顺序。
5. **scraper 搜索可选未实现**：只实现官方 API（Brave/Google CSE/Bing）；HTML scraper
   需要独立的反爬/浏览器策略，按任务“scraper 可选”留作后续。
6. **IANA 时区未支持**：只支持 UTC/Local/±HH:MM；`Asia/Shanghai` 等显式报错而不是
   静默按 UTC。需要 tzdata 或 chrono-tz 依赖（本波不能改 Cargo.toml）。
7. **重定向的 SSRF 窗口**：共享 reqwest client 自动跟随重定向，本模块在拿到响应后
   校验 `final_url` 并丢弃响应体；请求字节已经发出。彻底修复需要在 `kiana-core` 暴露
   manual-redirect client（handoff）。
8. **`grace_minutes` 默认 0**：严格按槽位，tick 晚一秒即 missed；生产建议 ≥5。这是
   刻意保持的可配置语义，文档与 `CronConfig` 都有说明。

## 五、风险

- `web::http::ReqwestWebTransport` 依赖共享 client 的默认重定向策略；DNS rebinding 已由
  `resolve_public_host` 的“连接前解析”缓解，但预检与实际连接之间仍有极短 TOCTOU 窗口
  （与 hooks 的自定义 resolver 相比弱一层）。
- `SearchChain` 的退避是真实 `tokio::time::sleep`；生产配置 `chain_max_retries=6` 时最坏
  等待约 10s，测试通过 `backoff_base_ms=0` 避免慢测。
- IDE lockfile 的清理会删除 workspace 不匹配的 lockfile；如果用户同时打开多个项目，
  另一个项目的 IDE 实例会被误删。这是任务要求的“workspace 不符清理”，接线层可改为
  “仅删除 dead pid 或当前项目目录下的 lock”。
- `FileCronStore` 用 `flock` 串行化，跨 NFS/网络文件系统不保证可靠；cron 状态目录应在
  本地磁盘。某个 job/execution JSON 损坏会让 `list_*` 返回错误并阻塞 tick（刻意选择
  fail-loud，避免静默漏跑）；运维需要修复或移除损坏文件。
- cron 的固定偏移时区没有 DST 规则；跨 DST 的墙钟 cron 需要 tzdata（gap）。
- `DiffBroker` 的 300s 超时是硬编码；长编辑会话可能超时，接线层可按需扩展。
- `EventFeed` 的 `Subscription` 队列在 `publish` 时同步锁；订阅者极多时发布会变慢，
  但不会为慢客户端分配更多内存（队列满即移除）。
