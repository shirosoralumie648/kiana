# 注释清理工作单：kiana-telemetry

- 待处理条目：34
- 涉及文件：18

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

- `crates/kiana-telemetry/src/index.rs`
- `crates/kiana-telemetry/src/jsjson.rs`
- `crates/kiana-telemetry/src/lib.rs`
- `crates/kiana-telemetry/src/memory.rs`
- `crates/kiana-telemetry/src/render.rs`
- `crates/kiana-telemetry/src/store_conformance.rs`
- `crates/kiana-telemetry/src/testing.rs`
- `crates/kiana-telemetry/src/timsort.rs`
- `crates/kiana-telemetry/tests/conformance.rs`
- `crates/kiana-telemetry/tests/cursor_pages.rs`
- `crates/kiana-telemetry/tests/js_edges.rs`
- `crates/kiana-telemetry/tests/memory_edges.rs`
- `crates/kiana-telemetry/tests/metrics.rs`
- `crates/kiana-telemetry/tests/propagation.rs`
- `crates/kiana-telemetry/tests/render.rs`
- `crates/kiana-telemetry/tests/reviewer_fixes.rs`
- `crates/kiana-telemetry/tests/schema_golden.rs`
- `crates/kiana-telemetry/tests/store.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-telemetry/src/index.rs:767` | A | TS等价物, TS的 | /// 自由函数形式，等价于 TS 的 `context.startSpan(options, callback)`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-telemetry/src/index.rs:1082` | A | 逐字一致 | /// `const` 泛型推断，因此它在这里主要是“这是一份 schema”的标记，并与 TS 调用点逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-telemetry/src/index.rs:1168` | A | 逐字一致, 对应TS | /// 逐字对应 TS，并把「schema 词汇表」留在签名上。schema 的编译期校验在 Rust 中丢失（仅类型层面 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-telemetry/src/memory.rs:684` | A | 与TS一致, 完全一致 | /// 判定规则与 TS 完全一致：`JSON null` ↔ `undefined` 之外的空值；只有字符串值、且三个必需键 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-telemetry/src/render.rs:33` | A | 必须与TS, 逐字节一致 | //! 子路径；它不是 store 查询的一部分，也不产生持久状态。输出必须与 TS 逐字节一致，因为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-telemetry/src/store_conformance.rs:30` | A | 逐字一致, TS的 | //! 4. 六个 case 的标题逐字保留 TS 的 `it(...)` 文案，runner 可以用它做报告与过滤。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-telemetry/src/testing.rs:175` | A | 逐字一致 | /// TS `describe(...)` 的标题，逐字保留，便于和上游测试对照。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-telemetry/src/testing.rs:177` | A | 逐字一致 | /// TS `it(...)` 的标题，逐字保留；runner 常用它筛选单个 case。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-telemetry/src/timsort.rs:37` | A | 逐字一致 | //! 这个数组排成什么顺序”。因此新增调用方时需要自己保证比较器与 TS 侧逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-telemetry/tests/conformance.rs:7` | A | 逐字一致 | //! - `registers_the_verbatim_describe_and_it_titles` 断言 group / name 字符串与 TS 逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-telemetry/tests/cursor_pages.rs:171` | A | 强约束 | // 但**错误名**是 TS 侧可观察的（`error instanceof SyntaxError`、`error.name`），必须一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-telemetry/tests/js_edges.rs:427` | A | 与TS一致 | /// 用 upstream-reference 录制的向量逐条断言（排序已换成 V8 TimSort 的移植，顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-telemetry/tests/metrics.rs:1` | A | 逐字一致, TS的 | //! metrics.test.ts 的 Rust 移植（4 个 `it` 标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-telemetry/tests/propagation.rs:1` | A | 逐字一致, TS的 | //! propagation.test.ts 的 Rust 移植（6 个 `it` 标题逐字保留，describe 为 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-telemetry/tests/propagation.rs:569` | A | 必须与TS, 与TS一致 | /// （只看三个必需键）。两种情形都必须与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-telemetry/tests/render.rs:1` | A | 逐字一致, TS的 | //! render.test.ts 的 Rust 移植（8 个 `it` 标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-telemetry/tests/render.rs:3` | A | 逐字一致 | //! 前 6 条与 TS 测试逐字对应，另加 2 条黄金向量（`tests/fixtures/render_traces.json` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-telemetry/tests/reviewer_fixes.rs:646` | A | 与TS一致 | // 上述四条都在结算后进入 store（与 TS 一致：rejection 不影响可见性）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-telemetry/tests/reviewer_fixes.rs:795` | A | 与TS一致 | /// 小数起点的 `storedSpans` 顺序（`a.startedMs - b.startedMs \|\| id`）同样与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-telemetry/tests/schema_golden.rs:25` | A | 逐字节一致 | //! 偏差记录：schema JSON 的键序不保证与 TS 逐字节一致（仅值集合相等）；AI schema 目前逐字节一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-telemetry/tests/schema_golden.rs:116` | A | 逐字节一致, TS的 | /// AI schema 恰好与 TS 的 `JSON.stringify` 逐字节一致（结构体字段序 == 该文件的字面量序）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-telemetry/tests/store.rs:110` | A | 必须与TS, 与TS一致 | /// `inherits remote trace IDs …` 的 fallback 计数必须与 TS 一致：每个 carrier 键两条 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-telemetry/src/index.rs:648` | D | 逐字一致 | /// `start_span_typed` 逐字对应这一点 —— 回调 future 的输出**就是**结果值，即使它是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-telemetry/src/jsjson.rs:844` | D | 逐字一致 | /// 且接受时 `JSON.stringify(JSON.parse(input))` 逐字相同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-telemetry/src/lib.rs:80` | D | 保真 | //! - 输出保真：进入文本/JSON 的浮点与键序按 JS 语义处理（`kiana-json` 与私有 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-telemetry/src/lib.rs:86` | D | 保真 | //! → `render`/`json`（输出保真）→ `testing`/`store_conformance`（适配器一致性套件）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-telemetry/src/memory.rs:1850` | D | 完全一致 | /// JS `===`：变体必须完全一致；数组是对象引用，永不等于查询里的标量。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-telemetry/src/render.rs:3` | D | 逐字节一致 | //! 输出必须逐字节一致（spec §6）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-telemetry/src/store_conformance.rs:563` | D | 逐字一致 | // `expect(...map(t => t.spans[0].name)).toEqual([...])` 逐字对应。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-telemetry/src/store_conformance.rs:693` | D | 完全一致 | /// 再用 `name:"root", limit:3` 逐页拉取，拼出的 id 序列必须与一次性查询完全一致 —— | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-telemetry/src/testing.rs:959` | D | 强约束 | // 同一 span 的 inject 应保持稳定；创建子树不能改变它输出的 carrier。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-telemetry/tests/cursor_pages.rs:241` | D | 逐字一致 | // `traces.at(-1)` 对 undefined 读属性 → `TypeError`（文本逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-telemetry/tests/memory_edges.rs:94` | D | 逐字一致 | /// `Number(span)` 的本地 id、`RecordedTelemetrySpan` 快照与 `StoredSpan` 快照逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-telemetry/tests/store.rs:7` | D | 逐字一致 | //! `describe("in-memory store snapshots")` 下的 3 个 `it` 逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
