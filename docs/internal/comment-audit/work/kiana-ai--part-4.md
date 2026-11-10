# 注释清理工作单：kiana-ai--part-4

- 待处理条目：40
- 涉及文件：10

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

- `crates/kiana-ai/Cargo.toml`
- `crates/kiana-ai/src/api/llama_cpp_classify.rs`
- `crates/kiana-ai/src/api/openai_responses.rs`
- `crates/kiana-ai/src/auth/oauth/openai_codex.rs`
- `crates/kiana-ai/src/compat.rs`
- `crates/kiana-ai/src/http/reqwest_client.rs`
- `crates/kiana-ai/src/image_models.rs`
- `crates/kiana-ai/src/models/mod.rs`
- `crates/kiana-ai/src/utils/overflow.rs`
- `crates/kiana-ai/src/utils/streaming_json.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/utils/overflow.rs:11` | A | 必须与TS | //! 正则表必须与 TS 逐条一致（顺序、大小写不敏感、`NON_OVERFLOW_PATTERNS` 的排除）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/utils/overflow.rs:39` | A | 逐字一致 | //! （**不含** `cache_write`），与 TS 逐字一致，改口径会让 z.ai/MiMo 的阈值行为偏离 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/utils/overflow.rs:55` | A | 与TS一致, TS: | /// 溢出模式表（TS: `OVERFLOW_PATTERNS`），顺序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/src/utils/overflow.rs:58` | A | 必须与TS, 与TS保持 | /// 结果（命中任意一条即返回 `true`），但必须与 TS 保持同序，便于逐条核对和定位差异。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ai/src/utils/overflow.rs:170` | A | 与TS一致 | /// 判定顺序（与 TS 一致，命中即短路返回）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-ai/src/utils/overflow.rs:228` | A | 与TS一致 | // 口径与 TS 一致：只加净输入与缓存命中，不含 cache_write。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-ai/src/utils/streaming_json.rs:306` | A | 与TS一致, 完全一致 | // 与 TS 完全一致：每次都问解析器要快照，再按 `snapshot === this.#snapshot` 判断是否变了。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/utils/streaming_json.rs:624` | A | 逐字一致, TS的 | /// 完整数字 token 的语法（与 TS 的 `NUMBER` 常量逐字相同）；编译结果进程内复用。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-ai/src/utils/streaming_json.rs:1051` | A | 与TS一致 | /// 刻意不校验括号类型：`}` 也能关闭数组（与 TS 一致）。空白被忽略；未知字符一律当作 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/utils/streaming_json.rs:1370` | A | 与TS一致 | /// 键字符串没有父容器时（根上的键）直接丢弃，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/utils/streaming_json.rs:1480` | A | 与TS一致 | /// 一条 `Set` 上报。游离的 `}`/`]`（栈为空）被忽略；`}` 也能关闭数组，与 TS 一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/api/llama_cpp_classify.rs:40` | A | 逐字一致, 对应TS | //! 与指令句都逐字对应 TS 实现；改格式会改变模型输入。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-ai/src/api/llama_cpp_classify.rs:164` | A | 逐字一致, 对应TS | /// 数量不符时返回普通 [`ClassifierHttpError`]（消息逐字对应 TS），调用方因此能在发任何 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/src/api/llama_cpp_classify.rs:904` | A | 与TS一致 | /// 校验顺序与 TS 一致，且全部发生在第一个网络请求之前：API 名、图片输入、温度、每个问题 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-ai/src/api/llama_cpp_classify.rs:1027` | A | 逐位一致 | /// 这里锁定的是与 TS 参考实现逐位一致的数值（V8 `Math.exp` 的黄金值）、纯函数的映射关系 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-ai/src/api/openai_responses.rs:123` | A | 逐字节一致 | /// 与 TS 逐字节一致，不要改写。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-ai/src/api/openai_responses.rs:317` | A | 逐字一致 | /// 不以 `sk-` 开头。条件与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-ai/src/api/openai_responses.rs:930` | A | 与TS一致 | // 收尾顺序与 TS 一致：取消优先（即便流已正常结束，只要 signal 已触发也报 aborted）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-ai/src/api/openai_responses.rs:1028` | D | 逐字一致 | // 两个判定都是**子串包含**，因此错误正文里带 code 也能命中；提示语必须逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-ai/src/auth/oauth/openai_codex.rs:859` | A | 与TS一致 | /// （含预取消）被忽略：流程仍会发 `AuthUrl` 并等待手动粘贴——与 TS 一致的回退行为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/auth/oauth/openai_codex.rs:870` | A | 与TS一致 | // catch 会吞掉预取消的 "Login cancelled"（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/auth/oauth/openai_codex.rs:885` | A | TS等价物, TS里 | // 手动粘贴（等价于 TS 里 `callback?.wait()` 的短路）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-ai/src/auth/oauth/openai_codex.rs:133` | D | 完全一致 | /// 这里的值，才能与授权时登记的 redirect_uri 完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-ai/src/compat.rs:21` | A | 与TS一致 | //! `REGISTRY`（`get_builtin_provider_for_model` 先查注册表），因此顺序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-ai/src/compat.rs:744` | A | 逐字一致, 对应TS | /// `(None, None)` 返回 `true` 是逐字对应 TS（`undefined !== undefined` 为 false）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/compat.rs:27` | D | 逐字一致 | //! 流上的 `error` 事件（[`lazy_stream_error`]，`errorMessage` 逐字相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-ai/src/compat.rs:870` | D | 逐字一致 | /// 的兜底逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-ai/src/http/reqwest_client.rs:19` | A | 逐字一致 | //! - **错误文本**：`HttpError::message` 必须是 TS `error.message` 的逐字文本 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-ai/src/http/reqwest_client.rs:204` | A | TS等价物, TS的 | // deadline 只计算一次：等响应头与后续读 body 共用同一截止时刻，等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-ai/src/http/reqwest_client.rs:1629` | A | 逐字一致 | // ARCHITECTURE §8 第 19 条：`message` 是 TS `error.message` 的逐字文本 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-ai/src/http/reqwest_client.rs:286` | D | 完全一致 | // 本实现与 fetch 的 forbidden method 判定**不完全一致**（待核实的代码问题）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-ai/src/image_models.rs:46` | A | TS等价物, TS的 | //! - 注册表缺失 → [`generate_images`] 返回 `Err(AiError)`（等价于 TS 的 rejection）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-ai/src/image_models.rs:47` | A | TS等价物 | //! - api 不匹配 → `Ok(AssistantImages { stop_reason: Error, ... })`（等价于 TS 包装层的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-ai/src/image_models.rs:21` | D | 逐字一致 | //! 错误映射成 `stopReason: "error"` 的 [`AssistantImages`]（`errorMessage` 逐字相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-ai/src/image_models.rs:181` | D | 逐字一致 | /// 文本必须逐字保持：测试 `mismatched_api_surfaces_as_an_error_result` 直接断言它。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-ai/src/models/mod.rs:760` | A | 与TS一致 | /// 原样发布回去，行为与 TS 一致：只写过滤后的模型）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-ai/src/models/mod.rs:960` | A | TS等价物, TS的 | /// 同一 provider 的发布串行化（每个 provider 一把 `tokio::sync::Mutex` 等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/models/mod.rs:1416` | A | 与TS一致 | /// 顺序与 TS 一致：非 chat 模型即使 provider 也未注册，报的是「不是 chat 模型」而不是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/src/models/mod.rs:2500` | A | 与TS一致 | // 顺序与 TS 一致：先断言图片模型，再查 provider，再检查能力，最后才解析凭证 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-ai/Cargo.toml:42` | D | 逐位一致 | # 5 万个样本里约 10% 差 1-2 ulp；libm（musl/fdlibm 系）与 V8 逐位一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
