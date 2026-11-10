# 注释清理工作单：kiana-ai--part-2

- 待处理条目：40
- 涉及文件：6

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

- `crates/kiana-ai/src/api/azure_openai_responses.rs`
- `crates/kiana-ai/src/api/constrained_sampling.rs`
- `crates/kiana-ai/src/api/system_one_shared.rs`
- `crates/kiana-ai/src/model_catalog.rs`
- `crates/kiana-ai/src/models/provider.rs`
- `crates/kiana-ai/src/types.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-ai/src/model_catalog.rs:4253` | A | 逐字一致 | /// 与 TS 逐字一致。这样测试不需要 Node 也不需要网络。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-ai/src/model_catalog.rs:5049` | A | 必须与TS | // 键的插入顺序必须与 TS 字面量一致：生成的数据文件是逐字节产物，键序会被 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-ai/src/model_catalog.rs:11` | D | 逐字一致 | //! planner 约定），并保持逐字一致的错误文本与 JSON 字节： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 4 | `crates/kiana-ai/src/model_catalog.rs:23` | D | 保真 | //! 关键保真点见 `spec/ai-core.md` §2.4 与 §8： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-ai/src/model_catalog.rs:106` | D | 逐字节一致 | // JS 语义不同（U+FEFF / U+0085、半数取偶、UTF-16 码元序），替换会破坏逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-ai/src/model_catalog.rs:2364` | D | 完全一致 | /// 集合与它完全一致；再逐个解析 `data/<provider>.json`，得到 `"type:id" -> api` 结构。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-ai/src/model_catalog.rs:2582` | D | 完全一致 | // 第一层：数据文件集合必须与结构里的 provider 完全一致（manifest 自身不算数据文件）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 8 | `crates/kiana-ai/src/model_catalog.rs:8284` | D | 完全一致 | // 完全一致（多一个少一个都报错）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-ai/src/models/provider.rs:298` | A | 与TS一致 | /// auth 方法（TS 必填；Rust 构造期不校验，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-ai/src/models/provider.rs:738` | A | 逐字一致 | /// `lazy_stream_error` 复刻，消息中的 provider 取 `self.id`（与 TS 文案逐字一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-ai/src/models/provider.rs:1115` | A | 逐字一致 | /// 注意两段文案与 TS 逐字一致：provider 级有支持但 api 缺失时是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-ai/src/models/provider.rs:1177` | A | 逐字一致 | /// TS 在实现集合为空时 `throw new Error(...)`；Rust 返回 `Err`，消息逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-ai/src/models/provider.rs:1180` | A | 与TS一致 | /// （与 TS 一致，缺失的 auth 由后续 `Models.login` 等路径报错）。`api` 为 `ByApi` 时按 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-ai/src/models/provider.rs:26` | D | 逐字一致 | //! 消息逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-ai/src/models/provider.rs:328` | D | 逐字一致 | /// 键必须与 `model.api` 逐字相同；空 map 视为没有图像能力（`has_generate_images` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-ai/src/models/provider.rs:333` | D | 逐字一致 | /// 键必须与 `model.api` 逐字相同；空 map 视为没有分类能力（`has_classify` 为 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-ai/src/api/system_one_shared.rs:83` | A | 逐字一致, TS的 | /// 字段名与 TS 的 `SystemOneImageAsset` 逐字一致，因此结构体**没有** `rename_all`，只有 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-ai/src/api/system_one_shared.rs:211` | A | 必须与TS, 逐字一致, TS的 | /// 与值 `"true"` 必须与 TS 的常量逐字一致。`classifier_shared` 之后才合并 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-ai/src/api/system_one_shared.rs:303` | A | 逐字一致, 对应TS | /// 失败只产生普通 `ClassifierHttpError`（无 HTTP 字段），错误文本逐字对应 TS： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-ai/src/api/system_one_shared.rs:363` | A | 逐字一致, 对应TS | /// 全部是普通 `ClassifierHttpError`（无 HTTP 字段），文本逐字对应 TS： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-ai/src/api/system_one_shared.rs:492` | A | 与TS一致 | /// 数组前面，原 `state` 放最后；不做数量/格式校验（与 TS 一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-ai/src/api/system_one_shared.rs:774` | A | 与TS一致 | // `aborted` 只看调用方令牌是否已取消；超时算 `error`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-ai/src/api/system_one_shared.rs:590` | D | 逐字一致 | /// 其余字段（`instructions`、`criteria`）逐字保留。`choice`/`score` 的序列化结果已经是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-ai/src/api/azure_openai_responses.rs:488` | A | 与TS一致 | /// 合并顺序与 TS 一致：`Accept`、`api-key` → identity headers（User-Agent / Stainless | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-ai/src/api/azure_openai_responses.rs:684` | A | 与TS一致 | // 检查顺序与 TS 一致：取消优先于 stop_reason 检查，因此会盖过 pending/error 文案。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-ai/src/api/azure_openai_responses.rs:797` | A | 与TS一致 | // deployment 名解析不会失败；它位于 output 构造之前、错误处理之外（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-ai/src/api/azure_openai_responses.rs:835` | A | 逐字一致 | // 这是整个模块唯一的同步错误出口；消息文本与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-ai/src/api/azure_openai_responses.rs:888` | A | TS等价物 | /// trait 不能返回 `Err`，因此把同步错误转成只含 error 事件的流（等价于 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-ai/src/api/azure_openai_responses.rs:571` | D | 完全一致 | /// `thrown_value` 只有在快照的消息与 `error.message` 完全一致时才采用它来还原状态码与 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-ai/src/api/constrained_sampling.rs:52` | A | 与TS一致 | //! [`make_strict_json_schema`]（不做缓存，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-ai/src/api/constrained_sampling.rs:58` | A | 逐字一致, TS的 | //! - **错误文本是外部契约**：所有 `AiError.message` 与 TS 的 `error.message` 逐字相同， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-ai/src/api/constrained_sampling.rs:876` | A | 与TS一致 | /// 同一工具稍后会由 [`get_json_schema_tool_parameters`] 再转换一次（不做缓存，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-ai/src/api/constrained_sampling.rs:976` | A | 与TS一致 | // 只把「非空白」的变体视为已提供；定义本身保持原文（不 trim），与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-ai/src/api/constrained_sampling.rs:65` | D | 逐字一致 | //! 中断言，不在 fixture 里。完整逐字文本以 `spec/ai-api-anthropic-openai.md` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-ai/src/api/constrained_sampling.rs:803` | D | 逐字一致 | /// 消息逐字保留。其他非对象 `variants`（字符串 / 数字 / 数组）的属性访问不抛错，视作未设置。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-ai/src/types.rs:836` | A | 逐字节一致, TS的 | /// 内部 [`Model`]。因此 `serde_json::to_value(&AnyModel)` 与 TS 的模型对象逐字节一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-ai/src/types.rs:2088` | A | 逐字一致, TS的 | /// 转成 crate 的统一错误（`Display` 与 TS 的 `error.message` 逐字相同）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-ai/src/types.rs:90` | D | 逐字节一致 | /// 需要与 `JSON.stringify` 逐字节一致时不能替换成无序容器。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-ai/src/types.rs:1002` | D | 保真 | /// provider 原始的 stop reason 字符串，用于诊断与逐字保真。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 40 | `crates/kiana-ai/src/types.rs:1090` | D | 完全一致 | /// 普通联合体完全一致。serde 的 internally tagged 形式（`tag = "role"`）对“newtype 变体 + 自带同名 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
