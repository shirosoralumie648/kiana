# 注释清理工作单：kiana-ai--part-8

- 待处理条目：10
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

- `crates/kiana-ai/tests/common/sse.rs`
- `crates/kiana-ai/tests/generate_models_strict.rs`
- `crates/kiana-ai/tests/images.rs`
- `crates/kiana-ai/tests/model_data_validation.rs`
- `crates/kiana-ai/tests/pre_generation_error.rs`
- `crates/kiana-ai/tests/providers.rs`
- `crates/kiana-ai/tests/stream.rs`
- `crates/kiana-ai/tests/streaming_json.rs`
- `crates/kiana-ai/tests/streaming_tool_arguments.rs`
- `crates/kiana-ai/tests/type_shapes.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/tests/common/sse.rs:3` | A | 逐字节一致 | //! TS 测试里 SSE 直接写成模板字符串；Rust 侧给几个小构造函数，输出逐字节一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/tests/generate_models_strict.rs:150` | D | 逐字节一致 | /// 3. 每个模型对象的字段与 **字段插入顺序**都相同，序列化后逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-ai/tests/images.rs:6` | A | 与TS一致, 完全一致 | //! 不传——保持与 TS 完全一致的调用形状与断言。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-ai/tests/model_data_validation.rs:5` | A | 完全一致 | //! `scripts/model-data.ts` 与 `scripts/hydrate-model-catalog.ts` 的导出函数。Rust 侧完全一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-ai/tests/pre_generation_error.rs:67` | D | 逐字一致 | // 文本逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-ai/tests/providers.rs:176` | D | 逐字一致 | /// 仍逐字保留原始 JSON，`ModelsStoreEntry::raw_models` 依赖它做无损往返。本测试锁定 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-ai/tests/stream.rs:14` | D | 逐字一致 | //! 调用方决定），断言本身逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 8 | `crates/kiana-ai/tests/streaming_json.rs:760` | A | TS等价物, TS的 | /// 按字符切分（等价于 TS 的 UTF-16 切分但不会拆开代理对；见 `support` 模块文档）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-ai/tests/streaming_tool_arguments.rs:5` | D | 完全一致 | //! 1. 流式参数只增不减、最终参数与完整 JSON 完全一致，且流内部状态（raw 文本快照）不会泄漏到 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-ai/tests/type_shapes.rs:273` | A | 逐字一致 | /// finding 9：`require_chat` 的错误文本与 TS `assertChatModel` 逐字一致，且带 `ModelsError` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
