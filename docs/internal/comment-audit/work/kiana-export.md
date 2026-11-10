# 注释清理工作单：kiana-export

- 待处理条目：15
- 涉及文件：8

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

- `crates/kiana-export/src/index.rs`
- `crates/kiana-export/src/lib.rs`
- `crates/kiana-export/src/tool_renderer.rs`
- `crates/kiana-export/tests/assets.rs`
- `crates/kiana-export/tests/export_html_cjk.rs`
- `crates/kiana-export/tests/export_html_golden.rs`
- `crates/kiana-export/tests/export_html_system_messages.rs`
- `crates/kiana-export/tests/export_html_unknown_fields.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-export/src/index.rs:7` | A | 逐字节一致, TS的 | //! 与 TS 的逐字节一致性由三件事保证： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-export/src/index.rs:230` | A | 逐字一致, TS的 | /// 前三个变体的 `Display` 与 TS 的 `Error.message` 逐字一致；主题错误直接透传 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-export/src/lib.rs:81` | A | 逐字一致 | //! - 错误统一为 [`ExportError`]：前三个变体的文案与 TS 逐字一致。`Theme` 透传 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-export/src/lib.rs:90` | A | 逐字节一致 | //! - 与 TS 逐字节一致依赖三件事：资产不变；每个占位符恰好替换一次且顺序固定 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-export/src/lib.rs:92` | A | 必须与TS | //! JS `JSON.stringify` 的键序与数字格式写出。这三处必须与 TS 对齐；改动资产、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-export/src/lib.rs:95` | A | 与TS一致 | //! spec/ca-tools-compaction-export.md 2.22.4），与 TS 一致的顺序主要是便于审计。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-export/src/tool_renderer.rs:8` | A | 必须与TS, 与TS一致 | //! 不可信但必须与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-export/src/tool_renderer.rs:41` | A | 必须与TS, 与TS一致 | //! 这些值必须与 TS 一致，否则渲染器内部按标志分支的输出会不同。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-export/src/tool_renderer.rs:128` | A | TS等价物, TS的 | /// 渲染宽度；`None` 等价于 TS 的缺省值 `100`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-export/tests/export_html_golden.rs:3` | A | 必须与TS | //! 固定夹具 + 固定主题 → 导出的 HTML 必须与 TS 参考实现逐字节相同。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-export/tests/export_html_system_messages.rs:7` | A | 与TS一致, 完全一致 | //! 行为与 TS 完全一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-export/tests/export_html_unknown_fields.rs:88` | A | 逐字一致, TS的 | /// 导出载荷逐字保留它们；最后一条没有字符串 id 时 TS 的 `leafId` 是 `undefined`， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-export/src/lib.rs:59` | D | 保真 | //! - [`assets`]：逐字节内嵌的模板与 vendored 资源，是字节级保真的前提。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-export/tests/assets.rs:5` | D | 逐字节一致 | //! 内嵌同一份文件；任何字节变化都会破坏 HTML 导出的逐字节一致性。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-export/tests/export_html_cjk.rs:7` | D | 逐字节一致 | //! 1. 断言 `template.js` 与 `vendor/marked-cjk-friendly.min.js` 逐字节一致（SHA-256）； | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
