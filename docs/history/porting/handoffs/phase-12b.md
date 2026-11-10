# 第 12 周后半（week-12b）wire agent 接线清单

本波新增的能力都在库层，尚未接入启动流程、工具表、daemon 服务器或会话调度。
以下按文件/函数给出需要的 API、调用顺序、示例与注意事项。**不要**在接线时修改本波
已测试的协议语义（错误文本、帧格式、上限、状态机），需要改就同步更新对应单测。

所有权提示：本波只改了 `crates/kiana-coding-agent/src/**` 与 `crates/kiana-rpc-print/**`。
下列接线会触及 `crates/kiana-tools/**`、`crates/kiana-builtin-extensions/**`、
`crates/kiana-session/**`、`crates/kiana-rpc-print/Cargo.toml`、根 `Cargo.toml`，
由 wire agent 统一应用。

---

## 1. Web 搜索/抓取接入工具表

### 现状

实现位于 `crates/kiana-coding-agent/src/web/`：

```text
web::ssrf::{validate_url, resolve_public_host, validate_redirect_target, ...}
web::http::{WebTransport, ReqwestWebTransport, WebHttpRequest, WebHttpResponse}
web::search::{SearchEngine, SearchChain, SearchChainConfig, SearchQuery, SearchResponse,
              BraveSearchEngine, GoogleCseEngine, BingSearchEngine, SearchChainError}
web::fetch::{fetch_urls, FetchOptions, FetchBatch, MAX_FETCH_CHARS}
```

### 推荐的落点（避免依赖环）

`kiana-tools` 不能依赖 `kiana-coding-agent`（后者已经依赖前者，会成环），而
`kiana-builtin-extensions` 也不便于直接引用 coding-agent。两种方案：

1. **推荐：把 `crates/kiana-coding-agent/src/web/` 整体移动到
   `crates/kiana-tools/src/web/`**。该模块只依赖 `kiana-core::http_dispatcher`、
   `tokio`、`async-trait`、`serde`/`serde_json`、`thiserror`，`kiana-tools` 已有这些依赖
   （若缺 `async-trait` 需在 `crates/kiana-tools/Cargo.toml` 补 `async-trait.workspace = true`）。
   移动后删除 `kiana-coding-agent/src/web/`，在 `lib.rs` 去掉 `pub mod web;`，并让
   coding-agent 通过 `kiana_tools::web` 引用（或直接不引用）。
2. 保留在 coding-agent：在 `MainOptions::extension_factories` 注入一个本地扩展工厂，
   用 `kiana_ext_api` 注册 `WebSearch`/`WebFetch` 工具。成本更高，且工具执行路径仍在
   coding-agent 里，不推荐。

### 构造引擎链

```rust
use std::sync::Arc;
use kiana_tools::web::{
    BraveSearchEngine, BingSearchEngine, GoogleCseEngine, ReqwestWebTransport,
    SearchChain, SearchChainConfig, SearchQuery,
};

let transport = Arc::new(ReqwestWebTransport::new());
let brave = std::env::var("BRAVE_API_KEY").ok().map(|key| {
    Arc::new(BraveSearchEngine::new(transport.clone(), key)) as Arc<dyn SearchEngine>
});
let google = match (std::env::var("GOOGLE_CSE_API_KEY"), std::env::var("GOOGLE_CSE_CX")) {
    (Ok(key), Ok(cx)) => Some(Arc::new(GoogleCseEngine::new(transport.clone(), key, cx)) as Arc<dyn SearchEngine>),
    _ => None,
};
let bing = std::env::var("BING_API_KEY").ok().map(|key| {
    Arc::new(BingSearchEngine::new(transport.clone(), key)) as Arc<dyn SearchEngine>
});
// 任务要求的顺序：首选 + fallback + 其余。没有 key 的引擎 is_available() == false，
// 链会自动跳过并记录 Unavailable，不需要调用方过滤。
let chain = SearchChain::new(brave, google.into_iter().collect(), bing.into_iter().collect());
```

调用：

```rust
let response = chain.run(&SearchQuery { query, limit: 10, recency: None }).await?;
// response.results: Vec<SearchResult>，所有引擎同一 schema；response.failures 记录切换原因。
```

工具层注意：

- `SearchChainError` 的 `Display` 已包含每个引擎的尝试次数与原因，可直接作为工具错误文本；
- `SearchResult.engine` 在成功时由链补齐；
- `ReqwestWebTransport` 的响应可能由于共享 client 跟随重定向而返回不同 `final_url`；
  引擎路径会校验最终 URL；
- 搜索端点来自官方常量/配置，不做 DNS 预解析；用户可控 URL 的 SSRF 边界在
  `fetch_urls`。

### WebFetch 工具

```rust
use kiana_tools::web::{fetch_urls, FetchOptions};

let options = FetchOptions::new(Arc::new(ReqwestWebTransport::new()));
let batch = fetch_urls(&urls, &options).await;
// batch.pages[i].content 已转文本并截断到 10_000 字符；batch.failures 逐 URL 给出原因。
```

- 输入 URL 数上限 `MAX_FETCH_URLS = 20`；需要更多请分批；
- 拒绝私网/链路本地/云元数据/非 http(s)/loopback；`169.254.169.254` 的错误文本含
  `cloud metadata endpoint`；
- `FetchedPage.truncated` 与 `bytes` 可用于工具结果提示；
- 不要绕过 `resolve_public_host` 直接用 `ReqwestWebTransport` 抓用户 URL，否则失去
  DNS 阶段的私网防护。

---

## 2. IDE 接入启动流程

### 发现 lockfile

```rust
use std::path::Path;
use kiana_coding_agent::ide::{
    cleanup_stale_locks, discover_ides, HttpIdeTransport, McpIdeClient,
};
use kiana_coding_agent::web::ReqwestWebTransport;

let ide_dir = kiana_core::config::get_agent_dir()
    .parent()
    .unwrap_or(Path::new("."))
    .join("ide"); // <home>/.kiana/ide；也支持 KIANA_IDE_DIR 覆盖（建议新增）
cleanup_stale_locks(&ide_dir, &cwd);
let Some(ide) = discover_ides(&ide_dir, &cwd).into_iter().next() else { return; };
let transport = HttpIdeTransport::new(
    std::sync::Arc::new(ReqwestWebTransport::new()),
    ide.port,
    ide.lock.auth_token.clone(),
);
let client = McpIdeClient::new(std::sync::Arc::new(transport));
client.initialize().await?;
```

- `discover_ides` 已过滤 dead pid/ppid 与 workspace 不符，并按 mtime 新→旧排序；
- `cleanup_stale_locks` 会删除 workspace 不匹配的 lockfile（任务要求）。如果产品上不希望
  删除其它项目的 lock，可只调用 `discover_ides`，把清理限制在当前 workspace；
- `authToken` 只在 `HttpIdeTransport` 内部进入 `Authorization` 头，错误文本不含 token；
- lockfile 的 `transport: "ws"` 目前未实现 WebSocket；遇到时可按“无 IDE”处理或后续补。

### contextUpdate

编辑器事件到 `IdeContext` 的映射：

```rust
let context = kiana_coding_agent::ide::IdeContext {
    open_files,              // Vec<OpenFile { path, content, language_id }>，按 LRU 排序
    current_file,            // Option<String>
    cursor,                  // Option<Position { line, character }>，UTF-16 code unit
    selection,               // Option<Selection { start, end }>
    trust,                   // 项目信任结果
};
client.send_context_update(&context).await?;
```

- 上限由 `build_context_update` 强制执行：≤10 文件、payload ≤16 KiB，超出会截断并设置
  `truncated = true`；调用方不需要自己截断；
- 建议在“打开/切换文件、光标或选区变化（去抖 100–300ms）、信任变化”时推送；
- `McpIdeClient::initialize` 幂等；`send_context_update` 不要求先 initialize，但生产路径
  应先初始化。

### openDiff round-trip

在 FileEdit/FileWrite 权限流程里：

```rust
use kiana_coding_agent::ide::{DiffBroker, DiffRequest, DiffOutcome, IdeError};

let broker = DiffBroker::new(); // 进程级单例，供通知路由使用
match broker.open_diff(transport.as_ref(), DiffRequest {
    id: format!("diff-{turn_id}-{tool_call_id}"),
    path: path.to_string_lossy().into_owned(),
    old_content,
    new_content,
    read_only,
}).await {
    Ok(DiffOutcome::Accepted { content }) => {
        // 必须写 content（用户可能手工修改过），不能写 new_content。
    }
    Ok(DiffOutcome::Rejected) => { /* 关闭 diff，不写文件 */ }
    Err(IdeError::ReadOnlyDiff) => {
        // readOnly 明确降级：回退到标准审批 UI，并提示“IDE 不支持只读 diff”。
    }
    Err(error) => { /* 网络/超时：记录并回退标准审批 UI */ }
}
```

MCP 流监听收到通知时路由：

```rust
// params 形如 {"id": "...", "content": "..."} / {"id": "..."}
if method == "ide/diffAccepted" {
    broker.resolve_accepted(id, content);          // 无对应 id 时返回 false，忽略即可
} else if method == "ide/diffRejected" {
    broker.resolve_rejected(id);
}
```

- `DiffBroker::open_diff` 先注册 channel 再发请求，通知早到不会丢；
- `read_only = true` 时不会发送任何请求；不要用 `new_content` 静默写文件；
- 300s 超时后 channel 会被注销，重复通知返回 `false`。

---

## 3. daemon / 多客户端接入

### 依赖

`crates/kiana-rpc-print/Cargo.toml` 需要新增（版本已在根 `[workspace.dependencies]` pin）：

```toml
hyper = { workspace = true }
hyper-util = { workspace = true }
http-body-util = { workspace = true }
tokio-tungstenite = { workspace = true }
futures = { workspace = true }
```

并让 `kiana-agent-core`/`kiana-session` 的事件类型能够被 `EventFeed::publish` 序列化
（`AgentSessionEvent: Serialize` 已满足；不确定时用 `serde_json::to_value` 后
`publish_value`）。

### 事件映射

```rust
let feed = EventFeed::new(); // 进程级单例

// 流式 delta / 进度：volatile，不重放。
feed.publish(EventChannel::Volatile, "session.delta", &delta)?;
// 最终消息、工具终态、错误：durable，断线可补。
feed.publish(EventChannel::Durable, "session.event", &event)?;
```

建议保留一个 `AgentSessionEventListener`，把 `AgentSessionEvent` 映射为
`(channel, event_type, payload)`；事件类型用稳定字符串（如 `message.final`、
`tool.completed`、`session.error`），不要直接把 Rust enum 的 Debug 名写进协议。

### SSE

```rust
let outcome = feed.subscribe(last_event_id); // Last-Event-ID -> since
for event in outcome.subscription.drain() {
    response_body.write_all(event.to_sse_frame().as_bytes()).await?;
}
loop {
    if let Some(event) = subscription.try_recv() {
        response_body.write_all(event.to_sse_frame().as_bytes()).await?;
    } else if subscription.is_failed() {
        // 只关闭这个连接；其它订阅不受影响。
        break;
    } else {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
```

心跳：每 `HEARTBEAT_INTERVAL_SECS`（15s）调用 `feed.publish_heartbeat(now_ms)`；新连接的
第一条事件已由 `subscribe` 放入 `server.connected`（payload 含
`heartbeatIntervalSeconds`），不需要服务器另发。

### WebSocket

`subscription.try_recv()` 后写 `event.to_ws_frame()`（预编码 JSON，单帧）。同一个
`Subscription` 只能给一个连接；不要 fan-out 同一个 queue。

### REST 最新页 → WS since

```rust
let page = feed.latest_page(200);
// JSON: {"events": [解析 page.events[i].json()], "since": page.since, "has_more": page.has_more}
// 客户端随后 WS 连接 /events?since=<page.since>，服务端调用 feed.subscribe(Some(since))。
```

- `subscribe` 在锁内把 `server.connected`、补放事件依次入队，再注册 live，因此不会
  出现 live 插到补放之前；
- `gap = Some(earliest)` 表示 `since` 太旧、replay log 已裁剪；客户端应重新拉 REST
  最新页而不是假设连续；
- 慢客户端：每次 publish 后检查 `subscription.is_failed()`；失败只关闭该连接。
  `EventFeed::publish` 内部已把满队列的订阅移除，其它连接不受影响。

### `SeqCursor`（客户端/服务端乱序）

如果事件可能乱序到达（多路合并），用：

```rust
let mut cursor = SeqCursor::new(last_confirmed_seq);
for seq in incoming {
    match cursor.observe(seq) {
        Ok(confirmed) => { /* 按 confirmed 顺序处理/ack */ }
        Err(SeqCursorError::GapTooLarge { .. }) => { /* 回退 REST 最新页 */ }
    }
}
```

`MAX_AHEAD = 1024`；重复/过旧 seq 返回空列表，幂等。

---

## 4. cron 接入会话

### 构造

```rust
use std::sync::Arc;
use kiana_coding_agent::cron::{
    CronConfig, CronScheduler, FileCronStore, JobDispatcher, ProcessFingerprint,
    DispatchRequest, time::now_secs,
};

struct SessionJobDispatcher { /* Arc<AgentSession> 或 runtime 句柄 */ }

#[async_trait::async_trait]
impl JobDispatcher for SessionJobDispatcher {
    async fn dispatch(&self, request: DispatchRequest) -> Result<(), String> {
        // 见下方“dispatch 内容”。
        Ok(())
    }
}

let cron_root = kiana_core::config::get_agent_dir().join("cron");
let store = Arc::new(FileCronStore::new(&cron_root).map_err(|e| e.to_string())?);
let scheduler = CronScheduler::new(
    store,
    ProcessFingerprint::current(),
    CronConfig {
        managed_script_dir: Some(cron_root.join("scripts")),
        ..Default::default()
    },
    Arc::new(SessionJobDispatcher { /* ... */ }),
);
```

### tick 循环

- 启动时调用一次 `scheduler.tick(now_secs()).await`（恢复 pending/missed）；
- 之后每 30s 左右调用一次；tick 是幂等的，多进程并发由 `FileCronStore` 的 flock +
  job version CAS 保证只有一个赢家；
- `TickReport` 的 `dispatched/recovered/unknown/missed/failed/skipped` 写入启动日志；
- 不要在同一进程里并发调用 `tick`（内部会争用同一批 job；虽然 CAS 安全，但会浪费）。

### dispatch 内容

`DispatchRequest { job, slot, execution_id }`：

1. 校验 job（`job.validate(Some(managed_script_dir))`）；
2. 如果 `job.script` 存在：`resolve_script_path(managed_script_dir, script)`，在
   `job.workdir` 下、经权限/沙箱执行；把脚本输出（受上限）拼进 prompt 或作为附件；
   脚本失败是否中止由产品决定，建议把失败写进 `Err` 让 execution 记 `failed`；
3. 用 `job.prompt` 发起一次新的 agent turn；应用 `job.skills`、`job.model`、
   `job.context_from`（从指定会话/上下文加载摘要）；
4. 成功 `Ok(())`，失败 `Err(error_text)`；scheduler 会写 `completed`/`failed`。

### delivery（如果 cron 结果要投递到外部通道）

- 发送前：`scheduler.claim_delivery(&execution_id, token)`；返回
  `CronError::DeliveryAlreadyClaimed` 表示已被别的进程投递，跳过；
- 发送成功：`scheduler.mark_delivered(&execution_id, &token, now)`；
- 进程在 claim 后死亡：下次 `tick` 的 `recover_orphaned_executions` 会把 execution 标
  `unknown` 且 `delivery.unknown = true`，`mark_delivered` 返回 `DeliveryUnknown`；
  **不要重试**；
- 如果投递目标就是会话内 prompt（没有外部通道），不需要 claim。

### 脚本目录

`managed_script_dir` 默认建议 `<agent_dir>/cron/scripts`；`resolve_script_path` 拒绝
绝对路径、`..`、符号链接逃逸。接线时创建目录并设置 0700。

### 与 kiana-durable 的关系

本波**没有**修改 `kiana-durable`，也不要求它的 scheduler。`FileCronStore` 已提供
`next_run_at` 先持久化、pending 槽位、execution 账本与 owner 指纹。若未来要把 cron
迁到 durable，需要的是：

- durable task record 增加 `pending_slot`/`pending_owner` 字段；
- 一个 `CronStore` 适配器把 `list_jobs`/`compare_and_swap_job`/`claim_delivery` 映射到
  durable 的 task/commit API；
- 迁移迁移前的 JSON 账本。

当前不需要 Wire E 改动 durable。

---

## 5. 发布检查

wire agent 负责：

```bash
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo test --workspace
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo clippy --workspace --all-targets
CARGO_TARGET_DIR=/data/cargo-target-sandbox cargo fmt --all -- --check
```

补充：

- 新增 daemon 依赖后同步 `Cargo.lock`；
- 二进制冒烟：`kiana --version`、`kiana --help`、`kiana -p "…"`；
- 若采纳“把 web 下移到 kiana-tools”，删除 coding-agent 的 `src/web/` 并更新
  `lib.rs`，再跑 `cargo test -p kiana-tools -p kiana-coding-agent`；
- `docs/release/checklist.md` 已列出默认配置/迁移/回滚步骤，执行后勾选。

---

## 6. 不要做的事

- 不要改 `web::ssrf` 的拒绝范围（尤其不要把 loopback 加回 WebFetch）、10k 截断、
  `MAX_CONTEXT_FILES/MAX_CONTEXT_BYTES`、`DEFAULT_QUEUE_CAPACITY = 4096`、
  `MAX_AHEAD = 1024`、`HEARTBEAT_INTERVAL_SECS = 15`、catch-up clamp `120s..2h`；
  这些都有对应单测与 DoD。
- 不要把 `DiffBroker` 的 `read_only` 处理改成“仍写文件”；DoD 明确要求显式降级。
- 不要在慢客户端队列满时阻塞 `publish` 或扩大队列；要求是失败该订阅。
- 不要删除 `ProcessFingerprint.start_time` 的比较退化成只比 pid；PID 复用测试会失败。
- 不要修改 `docs/history/porting/plan.md`。
