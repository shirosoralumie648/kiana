# 注释清理工作单：kiana-ai--part-1

- 待处理条目：40
- 涉及文件：5

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

- `crates/kiana-ai/src/api/bedrock_converse_stream.rs`
- `crates/kiana-ai/src/api/mistral_conversations.rs`
- `crates/kiana-ai/src/api/openai_completions.rs`
- `crates/kiana-ai/src/auth/oauth/github_copilot.rs`
- `crates/kiana-ai/src/error.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/api/openai_completions.rs:334` | A | 与TS一致 | /// 「探测 + 覆盖」后的最终兼容表，字段名与 TS 一致（serde 用 camelCase，缺键取默认值）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/api/openai_completions.rs:442` | A | 逐字一致 | /// 组成，且除 DeepSeek 外都是大小写敏感的（与 TS 逐字一致）。自建反代/私有域名可能探测错， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/api/openai_completions.rs:504` | A | 与TS一致 | // 仅靠 baseUrl 命中 openrouter.ai 不算（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/src/api/openai_completions.rs:1496` | A | 与TS一致 | // `compat.open_router_routing`（resolved 结构里的同名字段在本模块未被使用，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ai/src/api/openai_completions.rs:1721` | A | 与TS一致 | // 这里不按 `model.input` 过滤图片（与 TS 一致；toolResult 分支会过滤）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ai/src/api/openai_completions.rs:3083` | A | 与TS一致, 完全一致 | // 收尾 2：状态校验。顺序与 TS 完全一致（spec §2.10.4 第 16 步），不能调整： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ai/src/api/openai_completions.rs:3289` | A | TS等价物, TS的 | /// 时直接返回 `Err`（等价于 TS 的同步 throw，见 `spec/ai-api-anthropic-openai.md` §1.3）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/api/openai_completions.rs:1576` | D | 保真 | /// 若输入可能含非 BMP 字符，这里是已知的保真差异。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ai/src/api/openai_completions.rs:1979` | D | 逐字一致 | // "(no tool output)"。两个字符串必须逐字一致（spec §2.10.10）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-ai/src/api/mistral_conversations.rs:369` | A | 与TS一致 | // 终态判定顺序与 TS 一致：取消优先；其次“流结束但没有 finish_reason”视为协议错误； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/api/mistral_conversations.rs:510` | A | 必须与TS, 逐位一致, TS的 | /// `short_hash` 必须与 TS 的 `shortHash` 逐位一致（`spec/ai-api-other.md` §2.12.2）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/api/mistral_conversations.rs:674` | A | 与TS一致 | // 与 TS 一致：`on_response` 在检查状态码之前调用，非 2xx 也会通知观察者。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-ai/src/api/mistral_conversations.rs:949` | A | 与TS一致 | /// 两个消息来源分工不同（与 TS 一致，不能合并）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/src/api/mistral_conversations.rs:1621` | A | 与TS一致 | // 收尾顺序与 TS 一致：先结束仍在打开的文本/思考块，再按打开顺序结束各个 tool call。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-ai/src/api/mistral_conversations.rs:2080` | A | TS等价物 | // 等价于 TS `lazyStream` 的 setup rejection（见 api/mod.rs 的错误契约）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-ai/src/api/mistral_conversations.rs:17` | D | 逐字一致 | //! `constrained_sampling`（strict JSON schema）、`kiana_json`（与 JS 逐字一致的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-ai/src/api/mistral_conversations.rs:1964` | D | 逐字一致 | // `Display`，因为派生 tool call id 的种子要求逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:198` | A | 与TS一致 | /// （与 TS 一致），因为输入来自 GitHub 下发的 Copilot token。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:248` | A | 与TS一致 | /// 输入是 `/models` 的 JSON，输出两个有序模型 id 列表。规则顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:476` | A | TS等价物, TS的 | /// 等价于 TS 的 `AbortSignal.any([signal, 预算 signal, AbortSignal.timeout(5000)])`； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:642` | A | 与TS一致 | /// 两条 URI 校验失败路径共用同一句文案（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:908` | A | 逐字节一致 | /// `JSON.stringify` 生成，保证与 TS 逐字节一致）；重试预算 2 次 / 5s。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:1010` | A | 与TS一致 | /// - prompt 不带 per-prompt signal（与 TS 一致），整个流程只受 `interaction.signal()` 控制； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:1099` | A | TS等价物, TS的 | // 按首次出现顺序去重，等价于 TS 的 `[...new Set([...available, ...enabled])]`： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-ai/src/auth/oauth/github_copilot.rs:1246` | D | 完全一致 | /// 对 `get_github_copilot_base_url` 的转发，语义完全一致：token > 企业域 > 默认值。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-ai/src/error.rs:63` | A | 逐字一致 | /// 与 TS `error.message` 逐字一致的文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/error.rs:129` | A | 逐字一致 | // `ParseError.message` 与 V8 `SyntaxError.message`（或 TS 宽松解析的错误文本）逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/error.rs:155` | A | 逐字一致 | /// 与 TS `error.message` 逐字一致的文本（可能已含 `cause_detail`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-ai/src/error.rs:283` | A | 与TS一致 | // 与 TS 一致：`Display` 只输出 `message`，`code` 不进入用户可见文本（它通过字段与序列化 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-ai/src/error.rs:13` | D | 保真 | //! （模型目录/鉴权）、`AuthError`（OAuth 交互）等。能用 JS 文本保真的路径尽量保留原文。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-ai/src/error.rs:112` | D | 保真 | // 不附加任何字段名/错误链（见模块文档的字节级保真说明）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-ai/src/error.rs:121` | D | 逐字一致 | // serde_json 的错误文本是 Rust 自己的描述，不保证与 V8 `JSON.parse` 的文本逐字一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-ai/src/error.rs:122` | D | 保真 | // 需要 JS 文本保真的解析路径应使用 `kiana_json`（见下一个 `From` 实现）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:289` | A | TS等价物, TS的 | /// 先解码做合法性校验，再按标准字母表重新编码（等价于 TS 的 `base64ToBytes` 后由 SDK | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:331` | A | 与TS一致 | /// （与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:383` | A | 与TS一致 | /// 4. toolResult：连续的多个工具结果合并成一条 user 消息（与 TS 一致）；Claude 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:2965` | A | TS等价物 | /// 测试/嵌入用入口：用给定的流项驱动完整的事件循环（等价于 TS 测试里 mock 的 SDK 流）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:4880` | A | 与TS一致 | // 顺序与 TS 一致：先按思考预算扩大上限，再按上下文窗口裁剪。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:2230` | D | 逐字一致 | /// 文本逐字保留（含前导换行、双空格缩进与 `{error}` 字面量），因为它会出现在 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 40 | `crates/kiana-ai/src/api/bedrock_converse_stream.rs:2294` | D | 逐字一致 | /// - 任何 JSON.parse 失败 → `SyntaxError`（`js_parse` 给出与 V8 逐字一致的文案）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
