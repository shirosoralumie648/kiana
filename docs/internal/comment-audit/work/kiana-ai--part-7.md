# 注释清理工作单：kiana-ai--part-7

- 待处理条目：40
- 涉及文件：38

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

- `crates/kiana-ai/src/api/cloudflare.rs`
- `crates/kiana-ai/src/api/github_copilot_headers.rs`
- `crates/kiana-ai/src/api/google_shared.rs`
- `crates/kiana-ai/src/api/google_vertex.rs`
- `crates/kiana-ai/src/api/typesafe_system_one.rs`
- `crates/kiana-ai/src/auth/helpers.rs`
- `crates/kiana-ai/src/auth/oauth/device_code.rs`
- `crates/kiana-ai/src/auth/oauth/mod.rs`
- `crates/kiana-ai/src/auth/resolve.rs`
- `crates/kiana-ai/src/bin/kiana-ai.rs`
- `crates/kiana-ai/src/cli.rs`
- `crates/kiana-ai/src/legacy_api_aliases.rs`
- `crates/kiana-ai/src/models/cost.rs`
- `crates/kiana-ai/src/providers/all.rs`
- `crates/kiana-ai/src/providers/cloudflare_ai_gateway.rs`
- `crates/kiana-ai/src/providers/cloudflare_workers_ai.rs`
- `crates/kiana-ai/src/providers/images/mod.rs`
- `crates/kiana-ai/src/providers/opencode.rs`
- `crates/kiana-ai/src/providers/opencode_go.rs`
- `crates/kiana-ai/src/providers/openrouter.rs`
- `crates/kiana-ai/src/providers/qwen_token_plan.rs`
- `crates/kiana-ai/src/session_resources.rs`
- `crates/kiana-ai/src/utils/abort_signals.rs`
- `crates/kiana-ai/src/utils/error_body.rs`
- `crates/kiana-ai/src/utils/event_stream.rs`
- `crates/kiana-ai/src/utils/models_error.rs`
- `crates/kiana-ai/src/utils/oauth_page.rs`
- `crates/kiana-ai/src/utils/provider_env.rs`
- `crates/kiana-ai/src/utils/text.rs`
- `crates/kiana-ai/src/utils/uuid.rs`
- `crates/kiana-ai/src/utils/version.rs`
- `crates/kiana-ai/tests/abort.rs`
- `crates/kiana-ai/tests/anthropic_oauth.rs`
- `crates/kiana-ai/tests/bedrock_convert_messages.rs`
- `crates/kiana-ai/tests/cli_login.rs`
- `crates/kiana-ai/tests/common/credentials.rs`
- `crates/kiana-ai/tests/models_runtime.rs`
- `crates/kiana-ai/tests/support/streaming_json_reference.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/tests/models_runtime.rs:2638` | A | 必须与TS, TS的 | /// `ModelsApiStreamOptions<TApi>` 的 API 专属/任意额外字段必须与 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/tests/models_runtime.rs:4` | D | 逐字一致 | //! 断言与测试标题逐字保留（标题作为测试函数名或注释）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-ai/tests/support/streaming_json_reference.rs:113` | A | 逐字一致 | /// （与 TS `render` 的求值顺序逐字一致：先 map/子渲染，再求分隔符）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/tests/support/streaming_json_reference.rs:247` | D | 逐字一致 | // `String.fromCharCode` 逐字一致；孤立代理项按 U+FFFD 处理（Rust 无法表示）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-ai/src/api/cloudflare.rs:45` | D | 完全一致 | //! [`HttpClient`] 时完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-ai/src/api/github_copilot_headers.rs:19` | A | 与TS一致 | //! 与 `system` 里的图片不计（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ai/src/api/google_shared.rs:1032` | A | 与TS一致 | /// `toolConfig` 都不发。优先级与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/api/google_vertex.rs:252` | D | 逐字一致 | // aborted/error；只有缺 finishReason 的文案带 `Vertex`，其余与 Gemini 版逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ai/src/api/typesafe_system_one.rs:115` | A | TS等价物, TS的 | // 等价于 TS 的 `new URL("systemone", baseUrl)`（baseUrl 先去掉末尾所有 `/` 再补一个 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/auth/helpers.rs:20` | A | 与TS一致 | //! 请求阶段（与 TS 一致，见 spec/ai-auth.md §2.3）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/auth/oauth/device_code.rs:188` | A | 与TS一致 | /// 使可观察的睡眠时长与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/auth/oauth/mod.rs:737` | D | 逐字一致 | /// `parseAuthorizationInput` 的结果（anthropic 与 openai-codex 的同名函数逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-ai/src/auth/resolve.rs:366` | D | 强约束 | // 同一个闭包既做锁外乐观检查、又作为锁内 `needs_refresh`：两处判定必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-ai/src/bin/kiana-ai.rs:41` | D | 逐字一致 | //! `cli::main`，在这里另起一套会绕开 `cli.rs` 的逐字文本/退出码契约，使子进程测试与 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-ai/src/cli.rs:1201` | A | 与TS一致 | /// 回归（critical）：登录写入 `type: "oauth"` 且保留文件中已有条目，键序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-ai/src/legacy_api_aliases.rs:23` | D | 逐字一致 | //! `GenerativeAI`/`Conversations` 后缀，逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-ai/src/models/cost.rs:57` | A | 与TS一致 | // 分档判断用“请求级输入总量”：缓存读/写的 token 也计入，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-ai/src/providers/all.rs:67` | D | 逐字一致 | /// [`crate::models::ModelsStoreEntry::raw_models`] 逐字保留原始 JSON 数组（无损往返）； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-ai/src/providers/cloudflare_ai_gateway.rs:20` | D | 逐字一致 | //! - api map 的键必须与模型目录里的 `api` 逐字相同，否则命中的模型会拿到错误流 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-ai/src/providers/cloudflare_workers_ai.rs:13` | D | 逐字一致 | //! - classifier：key 为 `cloudflare-workers-ai-system-one`（与目录模型的 `api` 逐字对应）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-ai/src/providers/images/mod.rs:59` | A | TS等价物, TS的 | /// 语义上等价于 TS 的 `try { await load...(); return module.generateImages(...) }` / | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/providers/opencode.rs:23` | D | 逐字一致 | //! - `ByApi` 的键必须与目录里每条模型的 `api` 逐字相同（区分大小写）。新增或改名一个 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-ai/src/providers/opencode_go.rs:15` | D | 逐字一致 | //! - 约束相同：`ByApi` 的键必须与目录里的 `api` 逐字一致，四个适配器都要经过 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-ai/src/providers/openrouter.rs:301` | D | 逐字一致 | // 键必须与目录里 chat 模型的 `api` 值逐字一致；未命中的模型会得到错误流。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-ai/src/providers/qwen_token_plan.rs:38` | A | 逐字一致 | /// `QWEN_TOKEN_PLAN_API_KEY`。`base_url` 与 TS 逐字一致，模型目录里的 `baseUrl` 也 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/session_resources.rs:148` | A | 与TS一致 | /// [`SessionResourceCleanupError`]（即使某些回调失败，其余回调仍会执行，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/utils/abort_signals.rs:10` | A | 必须与TS | //! 中止必须与 TS/DOM 一样是**同步**的：监听器在 `abort()` 的调用栈里执行，不经过任务调度。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/utils/error_body.rs:42` | D | 保真 | //! 保真的边界，不要"顺手优化"。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-ai/src/utils/event_stream.rs:73` | A | 与TS一致 | /// 三个前提（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-ai/src/utils/models_error.rs:64` | A | 逐字一致, TS源 | /// 返回值与 TS 源码中的字符串常量逐字一致。它经 [`TsModelsErrorCode::frozen_variant`] | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-ai/src/utils/oauth_page.rs:132` | D | 逐字一致 | /// 逐字一致（单引号是 `&#39;`，不是 `&#x27;`）。不处理反引号、控制字符或非 ASCII 字符。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-ai/src/utils/provider_env.rs:31` | D | 保真 | //! `process.env` 则总能给出字符串。需要逐字节保真的调用方要自行处理这个差异。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-ai/src/utils/text.rs:247` | A | TS等价物 | /// - `end = None` 等价于 TS 缺省的 `text.length`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-ai/src/utils/uuid.rs:54` | A | 逐字一致, TS的 | /// `Display` 文本与 TS 的 `error.message` 逐字一致（上限用十进制），因为上层可能直接展示 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-ai/src/utils/version.rs:5` | A | 逐字一致 | //! detailed `User-Agent`（`kiana/1.4.0 (...)`，见 `utils/identity.rs`）与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-ai/tests/abort.rs:6` | D | 逐字一致 | //! `retry: 3` 在 Rust 侧没有对应物（断言本身逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-ai/tests/anthropic_oauth.rs:665` | D | 逐字一致 | /// 但格式化本身与注入错误逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-ai/tests/bedrock_convert_messages.rs:162` | A | TS等价物, TS的 | // 用户块只能是 text/image；空块列表等价于 TS 的“只有未知块”。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/tests/cli_login.rs:4` | D | 逐字一致 | //! （`spec/ai-core.md` §3.1 的逐字文本）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 40 | `crates/kiana-ai/tests/common/credentials.rs:4` | A | 必须与TS, 与TS一致 | //! 这些函数决定“需要真 key 的测试”是运行还是 `#[ignore]`。语义必须与 TS 一致（同样的环境变量、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
