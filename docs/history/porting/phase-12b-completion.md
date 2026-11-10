# W12a / W12b 补完记录

日期：2026-11-08
范围：`docs/history/porting/integration-audit.md` 判定为 **shallow** 的两周，以及第 12 周 handoff 中
“留给 wire agent”的接线项。

本文件只记录本轮实际落地的代码、真实执行的测试，以及**仍未完成**的部分。没有改
`docs/history/porting/plan.md`，没有改 `reference_project/**`，没有 commit。

## 1. W12b — Web / IDE / daemon / cron

### 1.1 daemon CLI（`kiana --serve`）

- `crates/kiana-coding-agent/src/cli/args.rs`
  - 新增 `--serve`、`--serve-addr <host:port>`、`--serve-token <token>`；
  - help 文本与 `tests/golden/help.txt` / `help_with_extensions.txt` 同步。
- `crates/kiana-coding-agent/src/app.rs`
  - `run_daemon_mode`：`EventFeed::new()` → `session.subscribe` 把每个
    `AgentSessionEvent` 序列化一次并发布（事件类型取 serde 变体名，通道由
    `channel_for_event_type` 决定：`*.delta`/`*.update` 为 volatile，其余 durable）→
    `bind_daemon` 监听 → 打印地址与 token → `Ctrl-C` 时中止。
  - 默认监听 `127.0.0.1:0`；`--serve` 分支在交互/打印分发之前返回，不接管 stdout。
- `crates/kiana-rpc-print/src/daemon.rs`
  - `DaemonConfig` 新增 `auth_token: Option<String>`；
  - `/events`、`/events/latest` 要求 `Authorization: Bearer <token>` 或 `?token=<token>`
    （`EventSource` / WebSocket 无法设置请求头）；`/healthz` 有意免鉴权；
  - 比较使用与长度无关的逐字节比较（`constant_time_eq`），不提前返回。

真实冒烟（本机二进制）：

```text
$ kiana --serve --serve-token testtoken
Daemon listening on http://127.0.0.1:39613
Bearer token: testtoken
Endpoints: GET /events (SSE or WebSocket), GET /events/latest?limit=N, GET /healthz

$ curl /healthz                                    -> 200
$ curl /events/latest（无 token）                   -> 401
$ curl /events/latest -H 'Authorization: Bearer …'  -> 200 {"events":[],"since":0,"has_more":false}
```

### 1.2 IDE 启动接线

- `crates/kiana-coding-agent/src/app.rs`
  - 会话创建后、模式分发前调用 `connect_for_workspace(&session_cwd)`；
  - 成功时发送初始 `contextUpdate`（`trust` 取 `SettingsManager::is_project_trusted()`），
    并把连接存入进程级 `OnceLock`；
  - 失败只告警，不阻止启动；`KIANA_IDE=0/false/no/off` 关闭，离线模式跳过。
- `crates/kiana-coding-agent/src/ide/startup.rs`
  - 进程级 `DiffBroker`、`IdeConnection` 存储与 `route_ide_notification`；
  - `HttpIdeTransport` 安装 SSE 通知接收端：`ide/diffAccepted` → `resolve_accepted`，
    `ide/diffRejected` → `resolve_rejected`；
  - `IdeConnection::open_diff(request)` 走全局 broker。
- `crates/kiana-coding-agent/src/ide/protocol.rs`
  - `HttpIdeTransport` 新增 `with_notification_sink`；
  - SSE 解析不再“读到响应就返回”，而是收集同一条流里的无 `id` 通知后统一回调；
  - JSON 响应路径不受影响（通知只存在于 SSE 流）。

### 1.3 web 工具默认激活

`crates/kiana-coding-agent/src/web/tools.rs`

- `web_fetch`：`default_active: Some(true)`（只依赖网络与 SSRF 校验）；
- `web_search`：`default_active` 在注册时按环境变量计算——`BRAVE_API_KEY`、
  `BING_API_KEY`、或 `GOOGLE_CSE_API_KEY`+`GOOGLE_CSE_CX` 任一存在才默认激活；
  没有 key 时仍不激活，避免多出永远失败的工具；
- 仍可用 `--tools` / `--exclude-tools` 覆盖。

### 1.4 cron 脚本走沙箱

`crates/kiana-coding-agent/src/cron/runner.rs`

- 新增 `spawn_managed_script`：`KIANA_SANDBOX_POLICY` 配置了策略时通过
  `kiana_sandbox::SandboxManager::from_env(workdir)` 启动 `sh <script>`
  （bubblewrap / Landlock；后端不可用 = 脚本不执行，fail-closed）；
- 未配置策略时才退化为宿主 `sh`；
- `crates/kiana-coding-agent/Cargo.toml` 增加 `kiana-sandbox` 依赖。

cron 是无人值守执行，没有交互式审批通道，因此脚本仍不经过权限引擎的 Ask 流程；
脚本必须位于 `0700` 的受管目录（`resolve_script_path` 已拒绝绝对路径、`..` 与符号
链接逃逸）。这一点与审计的“cron 脚本不经权限/沙箱”相比只关闭了沙箱部分。

## 2. W12a — TUI 渲染核心

### 2.1 已落地

- **乐观用户消息队列默认开启**（`crates/kiana-interactive/src/render_core/mod.rs`
  `pending_user_enabled`）：未设置 `KIANA_NEXT_RENDER_CORE` 时启用；
  `KIANA_NEXT_RENDER_CORE=0/false/no/off` 关闭。
- **i18n 接线**（`interactive_mode/render_core_host.rs`）：
  - `RenderCoreHost` 持有 `Translator`，构造时按 settings 的 `ui.locale` /
    `language` 从 `~/.kiana/locales` 加载（用户目录 > 内置 en/zh > 英文回退）；
  - 待发消息文案改用 `pending.sending` / `pending.failed` / `pending.retry` key；
  - `zh` 内置包有对应翻译（“发送中……”“发送失败：{{error}}”“重试”）。
- per-tool 审批卡、`ModalArbiter`、`AskUser` 对话框此前已经接入生产路径
  （`interactive_mode/extension_ui`），本轮未改动其协议。

### 2.2 仍未完成（按 AUDIT §7 第 10 条）

1. **`FrameWriter` / `TranscriptLedger` 未替换默认 TUI 写端。**
   现有 `kiana-tui` 的 `TuiMainScreen` / `TuiAltScreen` 是 TS 渲染器的逐字节移植，
   自带差分、Kitty 图片、硬件光标与 100+ golden；`render_core::frame` 是独立的
   “应用层定 finality、终端层只写”的替代实现。把默认路径切过去不是接线，而是替换
   整条输出管线（含 golden 重基线、鼠标布局、光标定位、备用屏生命周期）。本轮没有
   做假接线：`KIANA_NEXT_RENDER_CORE` 仍只影响 `experimental_enabled()`，
   `FrameWriter` 没有生产调用点。
2. **风险解释的真实 sideQuery 未接。** `ApprovalCard::begin_risk` / `resolve_risk`
   状态机与 3s 超时已测试，但没有向 `kiana-ai` 发起模型调用；`PermissionCardRequest`
   仍不含异步风险通道。
3. **48 个旧组件未迁移到 render_core。** 计划原本明确“不要整体重写”，本轮保持该边界。
4. i18n 只覆盖新组件的待发消息；审批卡/模态框的英文常量尚未全部换成 key。

## 3. 测试

全部使用 `CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

```text
$ cargo test -p kiana-rpc-print -p kiana-coding-agent
cargo test: 678 passed, 29 ignored (41 suites)

$ cargo test -p kiana-interactive --lib render_core
test result: ok. 9 passed; 0 failed (72 filtered out)

$ cargo test -p kiana-interactive --lib render_core_pending_user_message
test result: ok. 1 passed; 0 failed

$ cargo check -p kiana-coding-agent --all-targets
0 errors

$ cargo check -p kiana-interactive --all-targets
0 errors
```

新增测试：

- `kiana-rpc-print::daemon::tests::auth_token_is_required_and_accepts_header_or_query`
- `kiana-coding-agent::ide::startup::tests::ide_diff_notifications_resolve_the_global_broker`
- `kiana-coding-agent::web::tools::tests::{web_fetch_is_active_by_default,
  web_search_default_activation_follows_api_key_presence}`
- `kiana-coding-agent` 集成测试 `tests/args.rs::{parses_serve_flags, serve_defaults_are_off}`
- `kiana-interactive::render_core_host::tests::locale_changes_pending_wording`

## 4. 剩余风险

- daemon token 只打印到 stderr，没有轮换/持久化；默认只绑定 loopback，非 loopback
  部署仍需反向代理提供 TLS 与额外认证。
- `--serve` 仍要求会话有可用模型（与非交互模式的既有检查一致），没有模型时会在
  进入 daemon 分支前退出 1。
- IDE 连接只在启动时建立一次；编辑器事件没有持续推送（当前没有编辑器的打开/切换
  事件源），`openDiff` 已有 broker 与通知路由，但没有工具调用点。
- cron 脚本的沙箱策略沿用 `KIANA_SANDBOX_POLICY`；没有独立的 cron 策略配置项。
- web 工具的默认激活依赖注册时的环境变量；在 settings 里配置 key 不会改变默认值。
