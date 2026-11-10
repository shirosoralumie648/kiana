# 注释清理工作单：kiana-ai--part-6

- 待处理条目：40
- 涉及文件：19

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

- `crates/kiana-ai/src/api/anthropic_messages.rs`
- `crates/kiana-ai/src/api/mod.rs`
- `crates/kiana-ai/src/api/transform_messages.rs`
- `crates/kiana-ai/src/auth/oauth/anthropic.rs`
- `crates/kiana-ai/src/auth/oauth/meta.rs`
- `crates/kiana-ai/src/auth/oauth/radius.rs`
- `crates/kiana-ai/src/auth/oauth/xai.rs`
- `crates/kiana-ai/src/http/mod.rs`
- `crates/kiana-ai/src/models/store.rs`
- `crates/kiana-ai/src/providers/openrouter_api.rs`
- `crates/kiana-ai/src/providers/radius_config.rs`
- `crates/kiana-ai/src/providers/typesafe.rs`
- `crates/kiana-ai/src/providers/vercel_ai_gateway.rs`
- `crates/kiana-ai/src/telemetry.rs`
- `crates/kiana-ai/src/utils/abort.rs`
- `crates/kiana-ai/src/utils/typebox_helpers.rs`
- `crates/kiana-ai/src/utils/validation.rs`
- `crates/kiana-ai/tests/lazy_module_load.rs`
- `crates/kiana-ai/tests/message_types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/utils/validation.rs:162` | A | 与TS一致, 完全一致 | /// `coercePrimitiveByType(value, type)`：逐分支与 TS 完全一致（见 spec §2.2.28）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/utils/validation.rs:5964` | A | 与TS一致 | /// 步骤与 TS 一致：`argumentsError` 短路 → `structuredClone` → `normalizeOptionalNulls` → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/utils/validation.rs:4086` | D | 完全一致 | /// `CheckSchema`：关键字顺序与 typebox 的 `CheckSchema` 完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 4 | `crates/kiana-ai/tests/lazy_module_load.rs:474` | A | 与TS一致 | // 与 TS 一致：每个流都会 `await load()`（去重由加载器/宿主模块缓存负责）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ai/tests/lazy_module_load.rs:12` | D | 逐字一致 | //! - setup 失败 → `error` 事件 + `stopReason: "error"` 的合成消息（消息文本逐字相同）； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-ai/tests/lazy_module_load.rs:480` | D | 逐字一致 | // capabilities 声明了 fetchDeferred，但实现没有 → 流以错误事件结束，消息逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-ai/src/api/anthropic_messages.rs:54` | A | 保真 | //! - **字节级保真**：`build_params` 中字段的插入顺序与 TS 对象字面量一致（JSON 序列化顺序 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-ai/src/api/anthropic_messages.rs:3843` | A | 逐字一致 | /// 原生工具变更协议保留的占位工具（与 TS `DEFERRED_TOOL_PLACEHOLDER` 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-ai/src/api/mod.rs:25` | A | TS等价物 | //! 适配器把 `Err` 映射成 [`lazy::lazy_stream_error`]（等价于 TS `Models.streamSimple` 里 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/api/mod.rs:22` | D | 逐字一致 | //! `Result<AssistantMessageEventStream, AiError>`（panic-free 的 `Err`，错误消息逐字相同）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-ai/src/api/transform_messages.rs:362` | A | 与TS一致 | // 单遍扫描中填充，排在前面的 toolResult 匹配不到后面的 assistant（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/api/transform_messages.rs:70` | D | 逐字一致 | /// [`downgrade_unsupported_images`]）。文本必须逐字保持：它会被发给 provider 并进入历史。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-ai/src/auth/oauth/anthropic.rs:525` | A | 与TS一致 | /// 未知返回值直接报错，不静默回退到 browser（与 TS 一致）。`LoginOptions` 在本流程 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/src/auth/oauth/anthropic.rs:187` | D | 逐字一致 | /// 使用的值逐字一致（browser 用回调服务器的地址，copy_code 用固定常量）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-ai/src/auth/oauth/meta.rs:37` | A | 逐字一致 | //! - 错误文案与 TS 逐字一致，`tests/meta_oauth.rs` 有断言。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-ai/src/auth/oauth/meta.rs:441` | A | 与TS一致 | // 一条进度事件（文案与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-ai/src/auth/oauth/radius.rs:262` | A | 与TS一致 | /// 与 TS 一致，成功响应不做字段校验：`access_token`/`refresh_token` 缺失或非字符串时回落为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-ai/src/auth/oauth/radius.rs:336` | A | TS等价物, TS的 | /// `callback.close()` 在 `wait()` 结束后无条件执行（等价于 TS 的 try/finally），确保监听 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-ai/src/auth/oauth/xai.rs:121` | A | TS等价物, TS的 | /// 不可信）；返回 `url::Url` 的序列化结果，等价于 TS 的 `url.href`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-ai/src/auth/oauth/xai.rs:135` | A | 与TS一致 | /// 表单 POST，固定 `Accept` 与 `Content-Type`。响应体归一化规则与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/http/mod.rs:87` | A | 与TS一致 | /// 禁用的方法，并返回与 TS 一致的错误文案。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/http/mod.rs:134` | A | 逐字一致, TS的 | /// 错误文本；默认实现保证与 TS 的 `error.message` 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-ai/src/models/store.rs:282` | A | TS等价物 | // 等价于 TS 中对象未被改写时的 structuredClone 往返。未知类型模型不参与 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-ai/src/models/store.rs:93` | D | 逐字一致 | /// 反序列化时的原始 `models` 数组（逐字保留，含未知类型/未知字段的模型）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-ai/src/providers/openrouter_api.rs:26` | A | 与TS一致, 完全一致 | //! （数组 + 对象条目）与 TS 完全一致（测试 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/providers/openrouter_api.rs:140` | A | 与TS一致, 完全一致 | // 形状；正常的上游数据（数组 + 对象条目）与 TS 完全一致，只有 type-invalid 的输入 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/providers/radius_config.rs:85` | A | TS等价物, TS的 | /// 保留原始 JSON 对象，等价于 TS 的 `{...model}`（未知字段在转 [`Model`] 时丢弃）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/providers/radius_config.rs:490` | D | 完全一致 | /// [`load_radius_gateway_config`] 传 `default_oauth_fetch()`。除传输实现外行为完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-ai/src/providers/typesafe.rs:14` | D | 逐字一致 | //! - `classifiers` 的键必须与目录模型条目的 `api` 逐字相同（`typesafe-system-one`）， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-ai/src/providers/typesafe.rs:53` | D | 逐字一致 | // 键必须与目录模型条目的 `api` 逐字一致；`create_provider` 按 `model.api` 查这张表。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-ai/src/providers/vercel_ai_gateway.rs:19` | D | 逐字一致 | //! - `classifiers` 的键必须与 classifier 模型条目的 `api` 逐字相同 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-ai/src/providers/vercel_ai_gateway.rs:57` | D | 逐字一致 | // 键必须与 classifier 模型条目的 `api` 逐字一致；`create_provider` 按 `model.api` 查表。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-ai/src/telemetry.rs:3` | A | 逐字一致, TS: | //! TS: `src/telemetry.ts`。`AI_TELEMETRY_SCHEMA`（version 1）逐字保留 span/metric 定义； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-ai/src/telemetry.rs:783` | A | 与TS一致 | /// 不能，`from_utf16_lossy` 把孤立代理项替换为 U+FFFD。截断长度仍与 TS 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-ai/src/utils/abort.rs:386` | A | TS等价物, TS里 | // 被放弃的 future panic：等价于 TS 里未处理的 rejection，按原样传播。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-ai/src/utils/abort.rs:84` | D | 保真 | /// 保真测试分别锁定了这两个字符串，不要合并。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-ai/src/utils/typebox_helpers.rs:40` | A | TS等价物 | /// `values` 按传入顺序写入；`options` 为 `None` 等价于 TS 省略第二个参数。`description` / | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/utils/typebox_helpers.rs:44` | A | 与TS一致 | /// 符号，也没有 `anyOf`/`const`）；这是与 TS 一致的协议约束，不是实现缺失。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-ai/tests/message_types.rs:123` | A | 逐字节一致, TS的 | /// 补充：`Message` 的 JSON 形状与 TS 的普通联合体逐字节一致（`JSON.stringify` 同序同键）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-ai/tests/message_types.rs:140` | A | 逐字节一致 | // 序列化逐字节一致（键序 == TS 接口声明序）。浮点必须先过 `kiana_json` 的 JS 数字格式化 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
