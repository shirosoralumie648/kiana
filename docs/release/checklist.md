# 发布检查清单（第 12 周后半）

日期：2026-11-07
范围：Web search/fetch、IDE 集成、daemon EventFeed、无人值守 cron、文档/层表更新。
结论：**代码与单测通过；库层能力尚未接线到工具表/启动流程/daemon 服务器**，见文末
“未接线项”。本清单只覆盖第 12 周后半所有权内的两个 crate，workspace 全量回归由 wire
agent 执行。

## 1. 构建与测试

全部命令在 `/home/ctyun/code/kiana` 下执行，使用
`CARGO_TARGET_DIR=/data/cargo-target-sandbox`。

- [x] `cargo check -p kiana-coding-agent -p kiana-rpc-print` — 0 errors。
- [x] `cargo test -p kiana-coding-agent --lib` — 133 passed / 0 failed / 4 ignored。
- [x] `cargo test -p kiana-rpc-print --lib` — 44 passed / 0 failed。
- [x] `cargo test -p kiana-coding-agent -p kiana-rpc-print` — 41 suites、637 passed、
  0 failed、29 ignored（含 `tests/*.rs` 与 doc tests；ignored 全部是既有“需要
  `KIANA_BIN` 二进制”的用例）。
- [x] `cargo fmt -p kiana-coding-agent -- --check` 与
  `cargo fmt -p kiana-rpc-print -- --check` — 无 diff。
- [ ] `cargo test --workspace` — **留给 wire agent**（本波硬约束禁止跑 workspace）。
- [ ] `cargo clippy --workspace --all-targets` — 留给 wire agent。本波局部运行
  `cargo clippy -p kiana-coding-agent -p kiana-rpc-print --all-targets`：新增文件无
  warning，但命令因依赖 crate（`kiana-memory`/`kiana-sandbox`/`kiana-session` 等）的
  既有 clippy 警告以非零退出；需在 workspace 层统一处理。
- [ ] 两个二进制冒烟（`kiana --version` / `kiana --help` / print 模式）— 留给 wire
  agent；本波未改二进制入口。

## 2. 默认配置

- [x] `kiana-coding-agent` 未新增运行时环境变量；新模块全部通过构造参数显式注入
  （`SearchChain`、`FetchOptions`、`HttpIdeTransport`、`CronScheduler`）。
- [x] 未修改现有默认值：配置目录 `~/.kiana/agent`、会话目录
  `~/.kiana/agent/sessions`、`KIANA_CODING_AGENT_DIR` / `KIANA_CODING_AGENT_SESSION_DIR`
  的优先级由 `kiana-core` 保持不变。
- [x] cron 状态目录建议 `<agent_dir>/cron`（`jobs/`、`executions/`、`scripts/`、`.lock`），
  首次调用 `FileCronStore::new` 时创建；不配置/不调用时进程不产生任何文件。
- [x] IDE lock 目录建议 `<agent_dir>/../ide`（即 `~/.kiana/ide`）；发现逻辑只读，
  `cleanup_stale_locks` 只删除 stale `.lock`。
- [x] Web 引擎 key 通过接线层读取（建议 `BRAVE_API_KEY`、`GOOGLE_CSE_API_KEY` +
  `GOOGLE_CSE_CX`、`BING_API_KEY`）；缺 key 时引擎 `is_available() == false`，链自动跳过。
- [x] 无网络访问的默认测试：Web/IDE 单测全部使用注入的脚本化传输，不访问网络。

## 3. 迁移

- [x] 启动迁移顺序未变：`auth.json` → 顶层会话 → `tools/`→`bin/` → keybindings →
  扩展弃用检查（`crates/kiana-coding-agent/src/migrations.rs`）。
- [x] 迁移幂等性由既有测试覆盖并通过：`tests/config_value_migration.rs`、
  `tests/keybindings_migration.rs`、`tests/migrations_file_moves.rs`。
- [x] 新功能不需要数据迁移：cron 的 JSON 账本是新目录新格式；IDE lockfile 由编辑器
  插件写入；Web 无落盘状态。
- [x] 若 cron 账本升级，`CronJob.version` 提供乐观并发；未来字段变更应走“读旧 → 补默认
  → 写回”的一次性迁移，不要依赖 serde 的未知字段。

## 4. 回滚

- [x] 无数据库 schema 变更、无 `Cargo.lock` 变更（本波未新增依赖）。
- [x] 回滚二进制即可停用所有新能力：新模块未被入口调用，旧行为不受影响。
- [x] cron：把 `<agent_dir>/cron/jobs/*.json` 的 `enabled` 置为 `false` 可立即停跑；
  归档整个 `<agent_dir>/cron` 目录可清空状态。`pending_slot`/`pending_owner` 在下次
  tick 会按 owner 指纹判定恢复或 unknown，不会重复执行已 claimed 的 delivery。
- [x] IDE：删除 `~/.kiana/ide/*.lock` 或退出编辑器；`cleanup_stale_locks` 会清理 dead pid。
- [x] daemon EventFeed 是内存态；回滚/重启丢失 volatile 事件符合语义，durable 事件由
  会话 JSONL 记录负责。

## 5. 文档与层表

- [x] `README.md`：30 → 32 个 crate。
- [x] `ARCHITECTURE.md`：crate 数、`[workspace.dependencies]` path 依赖数、层表加入
  `kiana-hooks`（L6）与 `kiana-memory`（L6）；边表加入 `kiana-session -> kiana-hooks,
  kiana-memory`。
- [x] `docs/ARCHITECTURE_OVERVIEW.md`：crate 数、层表、校验记录。
- [x] `docs/START_HERE.md`：crate 数、层表新增两行、目录树与注释覆盖统计。
- [x] `docs/history/porting/phase-12a-tui-frame-contract.md`：追加第 12 周后半的实现/测试/风险。
- [x] `docs/history/porting/handoffs/phase-12b.md`：wire agent 接线清单。

## 6. 未接线项（发布前必须由 wire agent 完成）

- [ ] WebSearch/WebFetch 注册进工具表（见 handoff §1；建议把 `web/` 下移到
  `kiana-tools`，避免依赖环）。
- [ ] IDE 启动发现 + `ide/contextUpdate` + `openDiff` 通知路由（handoff §2）。
- [ ] daemon SSE/WS listener、REST 最新页、心跳（handoff §3；需要给
  `kiana-rpc-print/Cargo.toml` 增加 hyper/tungstenite 依赖）。
- [ ] cron 的 `JobDispatcher` 生产实现与 30s tick 循环（handoff §4；需要
  `kiana-session` 的 AgentSession prompt 通道）。
- [ ] workspace 全量测试/clippy/fmt 与二进制冒烟（handoff §5）。

## 7. 已知限制

- WebFetch 的重定向由共享 reqwest client 自动跟随；拿到响应后才校验最终 URL，请求
  字节已发出。需要 manual-redirect client 才能彻底关闭窗口（`kiana-core` 所有权）。
- IANA 时区（`Asia/Shanghai` 等）不支持；只接受 UTC/Local/±HH:MM，非法值显式报错。
- scraper 搜索未实现，只支持 Brave/Google CSE/Bing 官方 API。
- cron `grace_minutes` 默认 0（严格按槽位）；生产建议 ≥5。
- IDE WebSocket transport（`transport: "ws"`）未实现；只支持 MCP Streamable HTTP。
- `EventFeed` 是库层，不含网络 listener；慢客户端隔离已实现，但真实服务器需接线。
