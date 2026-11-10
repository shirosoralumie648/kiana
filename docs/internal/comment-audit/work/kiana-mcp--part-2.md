# 注释清理工作单：kiana-mcp--part-2

- 待处理条目：16
- 涉及文件：12

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

- `crates/kiana-mcp/src/oauth/discovery.rs`
- `crates/kiana-mcp/src/oauth/provider.rs`
- `crates/kiana-mcp/src/transports.rs`
- `crates/kiana-mcp/src/transports/in_memory.rs`
- `crates/kiana-mcp/src/transports/stdio.rs`
- `crates/kiana-mcp/tests/api_surface.rs`
- `crates/kiana-mcp/tests/client.rs`
- `crates/kiana-mcp/tests/oauth.rs`
- `crates/kiana-mcp/tests/oauth_core.rs`
- `crates/kiana-mcp/tests/stdio_framing.rs`
- `crates/kiana-mcp/tests/streamable_http.rs`
- `crates/kiana-mcp/tests/wire.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-mcp/src/transports/stdio.rs:1033` | A | 逐字一致 | /// `handleStdout(chunk)`（§15.4）：切行、解析、派发，顺序与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-mcp/src/transports/stdio.rs:1043` | A | 与TS一致 | // 两种超限路径（与 TS 一致）：带换行的超长行整行丢弃后继续读下一行；没有换行的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-mcp/tests/client.rs:1` | A | 逐字一致, packages/ | //! `packages/mcp/test/client.test.ts` 的移植（13 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-mcp/tests/client.rs:1650` | A | 与TS一致 | // 键序与 TS 一致：jsonrpc、id、result。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-mcp/tests/oauth_core.rs:324` | A | 与TS一致 | /// 比较也改用同一份判定（那里最终结论通常由路径检查决定，但比较语义与 TS 一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-mcp/tests/oauth_core.rs:326` | D | 强约束 | /// 黄金数据里带 `"rust"` 的用例给出 Rust 侧归一化后的钉住值，其余用例两边必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-mcp/tests/stdio_framing.rs:1238` | A | 与TS一致 | // 行为无关，按 Node 的做法短暂重试。`start()` 与 TS 一致地在 spawn **之前**置 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-mcp/tests/stdio_framing.rs:5` | D | 逐字一致 | //! 的 stdin 得到）：服务器可能比较原始字节，因此断言逐字文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-mcp/src/oauth/discovery.rs:771` | A | 与TS一致 | /// `https://mcp/…`），主机段非法时抛 `TypeError`——与 TS 一致（见 `js_url`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-mcp/src/oauth/provider.rs:2127` | D | 逐字一致 | // ……但写回的 JSON 与读入时逐字一致（含未知的顶层/嵌套键与键序）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-mcp/src/transports.rs:408` | D | 逐字一致 | /// 收 [`Value`] 而不是 `JsonRpcMessage`：见模块文档（服务端风格的响应、键序逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-mcp/src/transports/in_memory.rs:507` | A | 与TS一致 | /// [`McpTransportSendOptions`] 被忽略（与 TS 一致：in-memory 传输没有任何取消点）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-mcp/tests/api_surface.rs:7` | D | 逐字一致 | //! 在冻结契约里合并成一个 [`McpError`] 枚举，`name()` 逐字保留四个类名，因此 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-mcp/tests/oauth.rs:2` | D | 逐字一致 | //! `MCP OAuth` 10 个 + `OAuthCallbackServer pages` 3 个，标题逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-mcp/tests/streamable_http.rs:1` | A | 逐字一致, packages/ | //! `packages/mcp/test/streamable-http.test.ts` 的移植（16 个 `it` 标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-mcp/tests/wire.rs:5` | D | 逐字一致 | //! `JSON.stringify`）。服务器可能比较原始字节，所以这里断言的是**逐字文本**。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
