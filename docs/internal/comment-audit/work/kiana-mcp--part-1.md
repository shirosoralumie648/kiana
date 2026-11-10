# 注释清理工作单：kiana-mcp--part-1

- 待处理条目：40
- 涉及文件：11

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

- `crates/kiana-mcp/src/auth_provider.rs`
- `crates/kiana-mcp/src/client.rs`
- `crates/kiana-mcp/src/index.rs`
- `crates/kiana-mcp/src/jsonrpc.rs`
- `crates/kiana-mcp/src/lib.rs`
- `crates/kiana-mcp/src/oauth/errors.rs`
- `crates/kiana-mcp/src/oauth/flow.rs`
- `crates/kiana-mcp/src/oauth/types.rs`
- `crates/kiana-mcp/src/protocol_types.rs`
- `crates/kiana-mcp/src/transports/streamable_http.rs`
- `crates/kiana-mcp/tests/content.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-mcp/src/jsonrpc.rs:4` | A | 逐字一致 | //! untagged 枚举 + **手写**判别函数 [`parse_json_rpc_message`]，判别顺序与 TS 逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-mcp/src/jsonrpc.rs:15` | A | 逐字一致 | //! 靠 `name()` 逐字保留 TS 类名——外部调用方按这个名字区分错误种类。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-mcp/src/jsonrpc.rs:46` | A | 逐字一致 | //! 否则线上字节与 TS 不同，`tests/wire.rs` 断言的是逐字文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-mcp/src/jsonrpc.rs:141` | A | 保真 | /// 否则请求 id 会在线上与 TS 不同（§22.3 / 任务说明的键序与数字保真要求）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-mcp/src/jsonrpc.rs:284` | A | 逐字一致, TS的 | /// [`is_json_rpc_response`] + `parse_response`，与 TS 的判别逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-mcp/src/jsonrpc.rs:7` | D | 逐字一致 | //! 错误类的 `name` 必须逐字保留（`McpError` / `McpConnectionClosedError` / `McpTimeoutError` / | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-mcp/src/jsonrpc.rs:41` | D | 保真 | //! # 保真约束 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 8 | `crates/kiana-mcp/src/jsonrpc.rs:180` | D | 逐字一致 | /// 断言逐字文本）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-mcp/tests/content.rs:1` | A | 逐字一致, packages/ | //! `packages/mcp/test/content.test.ts` 的移植（2 个 `it`，标题逐字保留）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-mcp/tests/content.rs:482` | A | 逐字一致 | /// 1. [`try_to_llm_content`] 给出与 TS 逐字一致的 `TypeError` 信封； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-mcp/tests/content.rs:577` | A | 逐字一致 | /// `try_to_llm_content_from_value` 是 TS `toLlmContent(result: Pick<…>)` 的逐字对应物 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-mcp/tests/content.rs:249` | D | 逐字一致 | /// 占位字符串逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-mcp/tests/content.rs:683` | D | 逐字一致 | // 静默合并，因此这里在逐字文本里按对象深度统计键名。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-mcp/tests/content.rs:823` | D | 强约束 | // 类型化路径给出同样的结果（两条 API 必须一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-mcp/src/oauth/errors.rs:21` | A | 逐字一致, TS的, 对应TS | //! [`OAuthFailure`] 把它们收在一起：`name()` 逐字对应 TS 的 `error.name`，`as_oauth()` / | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-mcp/src/oauth/errors.rs:6` | D | 逐字一致 | //! 它们的消息构造逐字对应 `spec/telemetry-mcp.md` §27.2： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-mcp/src/oauth/errors.rs:536` | D | 逐字一致 | /// `OAuthRegistrationError`、`McpOAuthAuthorizationRequiredError`），消息逐字保留； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-mcp/src/oauth/errors.rs:545` | D | 逐字一致 | /// - 其余五个 OAuth 类 → `McpTransportError::named`：`name()` 是各自的类名，消息逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-mcp/src/oauth/errors.rs:559` | D | 逐字一致 | // 这些测试锁定三件事：五类消息的逐字文本（含 issuer 的 `none`/引号格式与 `message \|\| code` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-mcp/src/protocol_types.rs:1152` | A | 逐字一致 | /// 原始 JSON 内容块的**严格**转换（TS `blockToLlmContent(block)` 的逐字对应物）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-mcp/src/protocol_types.rs:1235` | A | 与TS一致 | /// 回退路径（`content` 为空且 `structuredContent !== undefined`）不会抛错，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-mcp/src/protocol_types.rs:2524` | A | 与TS一致 | // 本模块锁定三类行为：协议版本常量与 TS 一致；未知/类型不符字段的宽松往返；以及 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-mcp/src/protocol_types.rs:51` | D | 逐字一致 | //! 它们也不能在 `extra` 里再放一份（`serde(flatten)` 会写出重复键）。要逐字保留这类对象， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-mcp/src/protocol_types.rs:972` | D | 逐字一致 | /// `error.message` 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-mcp/src/transports/streamable_http.rs:51` | D | 逐字一致 | //! `!onUnauthorized` 分支逐字一致（`tests/streamable_http.rs` 的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-mcp/src/transports/streamable_http.rs:173` | D | 逐字一致 | /// 返回字段）。保留别名是为了让 `index.rs` 的再导出名与 §19.2 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-mcp/src/transports/streamable_http.rs:340` | D | 完全一致 | /// `Buffer.byteLength(buffered) > maxEventBytes` 的判定，必须在字节数上完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-mcp/src/transports/streamable_http.rs:535` | D | 逐字一致 | /// 逐字保留的规则： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-mcp/src/client.rs:116` | A | 与TS一致 | //! 它排在当前请求的 reject 之后，与 TS 一致（先 `entry.reject`，再 `void send`）。发送失败 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-mcp/src/client.rs:532` | A | 与TS一致 | // 从 method 映射里摘除（那样它也不会再被派发，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-mcp/src/client.rs:1775` | A | 与TS一致 | /// 因此 id 是否被消耗、请求是否已经发出都与 TS 一致（评审 finding 2）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-mcp/src/index.rs:20` | D | 逐字一致 | //! `McpAbortError` 在骨架里合并成 **1 个** `enum McpError`（`name()` 逐字保留四个名字）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-mcp/src/index.rs:107` | D | 逐字一致 | // 三个错误类在骨架里统一成 `McpTransportError` 信封（`name()` 逐字保留三个类名）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-mcp/src/lib.rs:64` | A | 逐字一致, TS的 | //! - [`auth_provider::McpTransportError`]：跨层统一的错误信封，`name()` 逐字保留 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-mcp/src/lib.rs:140` | A | 逐字一致 | /// 名字与 `src/protocol/content.ts`、`src/protocol/types.ts` 的导出逐字对应（Rust 侧把函数改成 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-mcp/src/oauth/flow.rs:1301` | D | 逐字一致 | /// 步骤顺序与回退规则逐字对应 §18.4（见模块文档）；`metadataUrl` 存在时跳过缓存的发现结果、 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-mcp/src/oauth/flow.rs:2342` | D | 强约束 | // WHATWG 的 `URL.hostname` 对 IPv6 带方括号，`url::Url::host_str()` 必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-mcp/src/oauth/types.rs:735` | A | 与TS一致 | /// 求值顺序与 TS 一致：`object` → `resource` → `authorization_servers`（数组校验后逐项 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-mcp/src/oauth/types.rs:285` | D | 逐字一致 | // 读写语义」悄悄分叉，而它们必须逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 40 | `crates/kiana-mcp/src/auth_provider.rs:445` | D | 逐字一致 | /// `Display` 与 [`McpTransportError::message`] 逐字一致：`McpError` 变体走 thiserror 的模板， | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
