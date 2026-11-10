# 注释清理工作单：kiana-rpc-print

- 待处理条目：27
- 涉及文件：16

## 规则（必须遵守）

1. **只改注释/文档文字，不改代码、字符串字面量、测试断言、类型签名或行为。**
2. 目标是把“因为 TS 这样，所以必须这样”改成“当前设计的技术原因”，或者删除纯 TS 理由。
3. 如果注释描述的是用户可见契约（错误文案、字段顺序、落盘格式、ANSI 输出），保留契约，但不要再用 TS 作为权威；改成“当前稳定契约，改动需同步测试”。
4. 如果注释只是出处（`TS 源：...`），保留出处，把约束性措辞去掉；不要删除整个出处。
5. D 类没有明确提到 TS：先判断是否仍有独立技术原因；有就保留并补技术原因，没有就删除。
6. 不要重写无关注释，不要扩大 diff；一个文件尽量只动列出的行附近。
7. 不要运行 `cargo fmt` / `cargo check` / 测试；统一由 verifier 执行。
8. 不要修改本工作单或 `docs/internal/comment-audit/scan*.csv`。

## 文件清单

- `crates/kiana-rpc-print/src/index.rs`
- `crates/kiana-rpc-print/src/jsonl.rs`
- `crates/kiana-rpc-print/src/lib.rs`
- `crates/kiana-rpc-print/src/print_mode.rs`
- `crates/kiana-rpc-print/src/rpc_client.rs`
- `crates/kiana-rpc-print/src/rpc_mode.rs`
- `crates/kiana-rpc-print/src/rpc_types.rs`
- `crates/kiana-rpc-print/tests/client_process_exit.rs`
- `crates/kiana-rpc-print/tests/export_surface.rs`
- `crates/kiana-rpc-print/tests/js_json_parity.rs`
- `crates/kiana-rpc-print/tests/json_event_parity.rs`
- `crates/kiana-rpc-print/tests/jsonl_parity.rs`
- `crates/kiana-rpc-print/tests/print_mode_parity.rs`
- `crates/kiana-rpc-print/tests/rpc_client_commands.rs`
- `crates/kiana-rpc-print/tests/rpc_signal_parity.rs`
- `crates/kiana-rpc-print/tests/rpc_ui_parity.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-rpc-print/src/index.rs:25` | A | 与TS一致 | //! **刻意省略**（与 TS 一致）：`RpcSlashCommand`、`RpcCommandType`、`toJsonEvent` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-rpc-print/src/lib.rs:18` | A | 必须与TS, 逐字节一致 | //! JSON 记录（键序、`null` 与缺键的区分、转义与数字格式）都必须与 TS 逐字节一致，否则解析 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-rpc-print/src/print_mode.rs:155` | A | 逐字节一致, TS的 | /// [`header_json`](Self::header_json)，与 TS 的 `JSON.stringify(getHeader())` 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-rpc-print/src/print_mode.rs:900` | A | 与TS一致 | /// 顺序与 TS 一致：先取会话并绑定扩展 -> 退订旧订阅 -> 建立新订阅 -> （JSON 模式） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-rpc-print/src/rpc_client.rs:638` | A | TS等价物, TS的 | /// 不主动 reject（supervisor 已在退出时拒绝过；若进程从未退出，等价于 TS 的永久挂起）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-rpc-print/src/rpc_client.rs:1038` | A | 与TS一致 | /// 与 TS 一致，监听器与计时器在**调用时**建立。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-rpc-print/src/rpc_client.rs:1145` | A | 与TS一致 | // 检查顺序与 TS 一致：未启动 -> exitError -> exitCode -> stdin 可写。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-rpc-print/src/rpc_mode.rs:1231` | A | 与TS一致 | // （随后 `checkShutdownRequested()` 仍会执行，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-rpc-print/src/rpc_types.rs:8` | A | 必须与TS, 完全一致 | //! JSON 行通信。这些 JSON 行的字段名、可选性、键序甚至“键是否存在”都必须与 TS 实现完全一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-rpc-print/src/rpc_types.rs:696` | A | 逐字一致, TS: | /// TS: `rpc-types.ts#RpcExtensionUIRequest`；字面量逐字保留 `setStatus` / `setWidget` / | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-rpc-print/tests/client_process_exit.rs:105` | A | 与TS一致 | // 被信号杀死：Node 的 `exitCode` 为 null，`start()` 因此不抛错（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-rpc-print/tests/export_surface.rs:14` | A | 与TS一致 | //! 刻意不在 `index` 里的名字（与 TS 一致）：`RpcSlashCommand`、`RpcCommandType`、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-rpc-print/tests/json_event_parity.rs:78` | A | 逐字节一致 | /// 与 TS 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-rpc-print/tests/jsonl_parity.rs:3` | A | 逐字一致 | //! - 四个 TS 用例标题逐字保留（`test/rpc-jsonl.test.ts`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-rpc-print/tests/print_mode_parity.rs:3` | A | 逐字一致, packages/ | //! - `runPrintMode` 的 4 个 TS 用例（`packages/coding-agent/test/print-mode.test.ts`）标题逐字保留。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-rpc-print/tests/print_mode_parity.rs:1296` | A | 逐字一致 | /// 事件必须经 `serialize_event_line`：键序与 TS `JSON.stringify(toJsonEvent(event))` 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-rpc-print/tests/rpc_signal_parity.rs:1` | A | TS等价物, TS的 | //! 真实 OS 信号回归（评审发现 9）：`shutdown()` 开头等价于 TS 的 `process.off`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-rpc-print/src/jsonl.rs:20` | D | 逐字节一致 | //! 一个未完成的前缀结尾。逐段解码与 V8 的替换结果逐字节一致（由随机分块对拍保证）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-rpc-print/src/lib.rs:17` | D | 保真 | //! 字节级保真是整个移植项目的验收标准：print/JSON 模式与 RPC 模式在 stdout 上写出的每一条 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-rpc-print/src/rpc_mode.rs:26` | D | 逐字一致 | //! - `success` / `error` 记录用 [`crate::rpc_types::RpcResponse`]（字段序与省略规则逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-rpc-print/tests/js_json_parity.rs:13` | D | 逐字节一致 | /// `serializeJsonLine` 的数字格式与 Node 的 `JSON.stringify` 逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-rpc-print/tests/json_event_parity.rs:3` | D | 逐字一致 | //! - 回归套件 #7290 / #7911 / #7925 的标题逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-rpc-print/tests/json_event_parity.rs:285` | D | 逐字节一致 | /// `message_start`/`message_end`/`turn_end`/`agent_end` 都必须逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-rpc-print/tests/json_event_parity.rs:341` | D | 强约束 | /// 顶层与嵌套载荷（`get_state` 的 `data.model`）都必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-rpc-print/tests/jsonl_parity.rs:110` | D | 逐字节一致 | /// 黄金数据：与 Node v26.12.1 `attachJsonlLineReader` 的分块行为逐字节一致 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-rpc-print/tests/rpc_client_commands.rs:324` | D | 逐字一致 | /// 30s 请求超时的逐字文本（用 50ms 覆盖快速验证）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-rpc-print/tests/rpc_ui_parity.rs:202` | D | 逐字节一致 | /// 等待一条新请求，校验它是 UUID v4 且与 fixture 的模板逐字节一致（`{id}` 替换为实际 id）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
