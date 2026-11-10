# 注释清理工作单：kiana-ai--part-3

- 待处理条目：40
- 涉及文件：7

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

- `crates/kiana-ai/src/api/azure_openai_config.rs`
- `crates/kiana-ai/src/api/kiana_messages.rs`
- `crates/kiana-ai/src/api/lazy.rs`
- `crates/kiana-ai/src/api/openai_codex_responses.rs`
- `crates/kiana-ai/src/api/openai_responses_shared.rs`
- `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs`
- `crates/kiana-ai/src/providers/faux.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/api/kiana_messages.rs:43` | A | 与TS一致 | //! 本适配器**不做重试**（单次请求；`StreamOptions::max_retries` 等字段被忽略），与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/api/kiana_messages.rs:129` | A | 与TS一致 | /// `append_rewrite_diagnostic`）；本类型只描述形状。字段按 camelCase 序列化，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/api/kiana_messages.rs:135` | A | 保真 | /// 策略版本。TS 是 `number`，用 `f64` 保真（协议没有声明必须是整数）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/src/api/kiana_messages.rs:995` | A | TS等价物 | /// `on_event` 返回 `true` 时立即 `return Ok(())`，不再读取剩余 body（等价于 TS 生成器的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ai/src/api/kiana_messages.rs:1298` | A | 与TS一致 | // 会删除已有的同名头。注意**不读取 `model.headers`**（与 TS 一致，不同于 Mistral）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ai/src/api/kiana_messages.rs:974` | D | 逐字一致 | // `error.message` 作为 errorMessage。`js_parse` 的文案与 V8 逐字一致，且接受 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-ai/src/api/lazy.rs:409` | A | 与TS一致 | /// **不**调用 `target.end()`，由调用方按 TS catch 分支结束 `outer`（与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/api/lazy.rs:545` | A | 逐字一致, 对应TS | /// （消息逐字对应 TS）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-ai/src/api/lazy.rs:562` | A | TS等价物, TS的 | /// [`LazyApiLoadFuture`] 的工厂（每次调用等价于 TS 的 `await load()`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/api/lazy.rs:608` | A | 逐字一致, 对应TS | /// 消息逐字对应 TS，测试断言依赖它；不要改写。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/api/lazy.rs:612` | A | 逐字一致, 对应TS | /// 消息逐字对应 TS，测试断言依赖它；不要改写。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/api/lazy.rs:689` | D | 逐字一致 | // error 事件（消息逐字相同）。实现返回 None 也走同一条错误。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-ai/src/api/openai_codex_responses.rs:390` | A | 与TS一致 | /// 可取消的 sleep：有取消令牌时用 `biased` select，取消优先于计时器；错误文本与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/src/api/openai_codex_responses.rs:462` | A | 与TS一致 | /// 其余步骤与 TS 一致：先用 [`build_base_options`] 把 [`SimpleStreamOptions`] 冻结成 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-ai/src/api/openai_codex_responses.rs:1417` | A | 与TS一致 | /// `-`/`_` 替换（spec §2.14.14：JWT 段含这些字符时会失败，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-ai/src/api/openai_codex_responses.rs:2164` | D | 完全一致 | /// input/previous_response_id 外完全一致”的比较；`last_response_items` 是本轮助手输出 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-ai/src/api/openai_codex_responses.rs:2712` | D | 完全一致 | /// 去掉 `input` 与 `previous_response_id` 后的请求体，用于判断“除增量部分外完全一致”。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-ai/src/api/openai_codex_responses.rs:2737` | D | 完全一致 | /// 1. 上次请求体除 `input`/`previous_response_id` 外与本次完全一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-ai/src/api/openai_responses_shared.rs:879` | A | 逐字一致 | /// 模型支持图片时产出数组。空文本用与 TS 逐字一致的占位文案 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-ai/src/api/openai_responses_shared.rs:1312` | A | 必须与TS | /// 这段提示文本是协议的一部分（模型据此理解「增量工具列表」的语义），必须与 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/api/openai_responses_shared.rs:761` | D | 完全一致 | /// 只有 `AiError` 的消息与最后一次 HTTP 失败完全一致时才复用 `RequestFailure.value`： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 22 | `crates/kiana-ai/src/api/openai_responses_shared.rs:1071` | D | 逐字一致 | /// 不支持；错误消息由 `constrained_sampling` 生成并逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-ai/src/api/openai_responses_shared.rs:1313` | D | 逐字一致 | /// 逐字一致；有不可用工具时追加 `Still unavailable: ...` 一行。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-ai/src/api/openai_responses_shared.rs:2263` | D | 逐字一致 | /// 直接成为用户可见错误，必须逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:182` | A | 逐字一致, 对应TS | /// 校验顺序与错误文案逐字对应 TS（spec/ai-auth.md §2.12）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:728` | A | 与TS一致 | /// 只要求「是字符串且 JS `trim` 后非空」，返回**原始**字符串（保留两侧空白），与 TS 一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:749` | A | 与TS一致 | /// `auth.json` 的字段顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:791` | A | 逐字一致 | /// 表单字段顺序与 TS 逐字一致（spec/ai-auth.md §2.12）：`grant_type`、`client_id`、`code`、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:972` | A | 逐字一致, TS的 | // 阶段 4：参数顺序与 TS 的输出逐字一致；agent_name_hint 可由调用方覆盖。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-ai/src/auth/oauth/openai_chatgpt.rs:23` | D | 完全一致 | //! 必须与兑换 token 时提交的完全一致，固定值让两处天然相同。代价是同一主机同一时刻只能 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-ai/src/providers/faux.rs:10` | A | 与TS一致, 完全一致 | //! - 流式事件与 TS 完全一致：每个内容块先 push `*_start`，按 token 大小切块 push | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-ai/src/providers/faux.rs:1193` | A | 与TS一致 | // 顺序与 TS 一致：即使 handle 不存在也照样记入 `cancelledDeferred`（只追加）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-ai/src/providers/faux.rs:1303` | A | 与TS一致 | /// - `api` 缺省是**随机 id**（`faux:<毫秒>:<base36>`），与 TS 一致；`provider` 缺省才是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-ai/src/providers/faux.rs:1570` | A | 与TS一致 | /// 该函数只服务于 token 估算，不参与请求构造，因此格式只需与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-ai/src/providers/faux.rs:1736` | A | 与TS一致 | /// 估算口径（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-ai/src/providers/faux.rs:2159` | A | 与TS一致 | // 结束前换回原始 arguments 对象（与 TS 一致），不依赖解析器对完整文本的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-ai/src/api/azure_openai_config.rs:431` | A | TS等价物 | /// 解析失败（等价于 TS `new URL()` 抛错）时返回 `AiErrorKind::Request`，消息里带的是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/api/azure_openai_config.rs:438` | A | TS等价物, TS的 | // 随后去掉所有末尾 "/"，等价于 TS 的 trim().replace(/\/+$/, "")。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/src/api/azure_openai_config.rs:485` | A | 逐字一致 | /// - 所有来源都为空：`AiErrorKind::Request`，消息列出全部可用配置项（与 TS 逐字一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-ai/src/api/azure_openai_config.rs:433` | D | 逐字一致 | /// `Invalid Azure OpenAI base URL: ...` 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
