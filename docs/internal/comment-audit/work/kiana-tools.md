# 注释清理工作单：kiana-tools

- 待处理条目：32
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

- `crates/kiana-tools/src/bash.rs`
- `crates/kiana-tools/src/bash_executor.rs`
- `crates/kiana-tools/src/definition.rs`
- `crates/kiana-tools/src/edit.rs`
- `crates/kiana-tools/src/edit_diff.rs`
- `crates/kiana-tools/src/file_mutation_queue.rs`
- `crates/kiana-tools/src/find.rs`
- `crates/kiana-tools/src/grep.rs`
- `crates/kiana-tools/src/lib.rs`
- `crates/kiana-tools/src/ls.rs`
- `crates/kiana-tools/src/read.rs`
- `crates/kiana-tools/src/render_utils.rs`
- `crates/kiana-tools/src/renderers/edit.rs`
- `crates/kiana-tools/src/renderers/ls.rs`
- `crates/kiana-tools/src/renderers/read.rs`
- `crates/kiana-tools/src/renderers/write.rs`
- `crates/kiana-tools/src/write.rs`
- `crates/kiana-tools/tests/file_tools_renderers.rs`
- `crates/kiana-tools/tests/tool_definitions_golden.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-tools/src/bash.rs:154` | A | 与TS一致 | /// 校验，以保持与 TS 一致的报错行为。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-tools/src/bash.rs:559` | A | 逐字一致 | // 阶段 4：cwd 必须存在；错误文本是单个 \n 的固定格式（与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-tools/src/bash.rs:700` | A | 与TS一致 | // 顺序与 TS 一致：先报告中止，再报告超时；两者同时发生时 abort 优先。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-tools/src/bash_executor.rs:17` | A | 逐字一致, TS的 | //! 而阈值是 `DEFAULT_MAX_BYTES * 2`（字节数）——TS 的这个量纲混用逐字保留； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-tools/src/bash_executor.rs:191` | A | 逐字一致, TS的 | // 按「UTF-16 code unit」计数，阈值却是字节数（TS 的量纲混用逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-tools/src/definition.rs:89` | A | 逐字一致, TS: | /// TS: `wrapToolDefinition(definition, ctxFactory?)`。字段投影与 TS 逐字一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-tools/src/edit.rs:460` | A | 逐字一致 | // 计数用 edits.len()；1 也写作 "block(s)"（TS 文案逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-tools/src/edit_diff.rs:192` | A | 必须与TS, 与TS一致 | /// 顺序必须与 TS 一致（NFKC 可能把空白变体折叠成普通空格，之后才会被 `trimEnd` 去掉）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-tools/src/file_mutation_queue.rs:363` | A | 与TS一致 | /// `path.resolve(filePath)`：按进程 cwd 词法解析（不展开 `~`、不做 Unicode 归一，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-tools/src/find.rs:95` | A | 逐字一致 | /// 这样 `details.truncation.maxLines` 与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-tools/src/find.rs:764` | A | 必须与TS, TS的 | /// details 的键序必须与 TS 的赋值顺序一致（`resultLimitReached` → `truncation`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-tools/src/grep.rs:499` | A | 与TS一致 | // 阶段 3：拼 argv。前缀顺序与 TS 一致（测试断言 argv[0] 是 `--no-config`，它让 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-tools/src/grep.rs:687` | A | 逐字一致 | // `details.truncation.maxLines` 与 TS 逐字一致；字节上限取默认 50 KiB）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-tools/src/lib.rs:119` | A | 逐字一致 | //! - 大量行为以“与 TS 逐字一致”为准，而不是以更符合 Rust 直觉的实现为准；遇到 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-tools/src/ls.rs:422` | A | 与TS一致 | // 键顺序与 TS 一致：entryLimitReached 先插入，truncation 后插入。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-tools/src/read.rs:52` | A | 逐字一致 | //! 50 KiB）截断，并拼接与 TS 逐字一致的脚注。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-tools/src/render_utils.rs:31` | A | 逐字一致 | //! TS `state` 字段的逐字对应，由 `start_tool_timer`/`tool_timer_text` 写入、后台线程只读 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-tools/src/render_utils.rs:82` | A | 逐字一致 | /// TS 用 `resolvePath(rawPath, cwd)` 再 `pathToFileURL(...).href`；URL 编码必须逐字一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-tools/src/renderers/edit.rs:400` | A | 与TS一致 | /// 写回预览并清掉 `preview_pending`；返回值表示「调用行需要重绘」。比较规则与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-tools/src/renderers/read.rs:456` | A | 与TS一致 | // 展开后不做紧凑分类（与 TS 一致）；cwd 转成字符串供纯词法解析使用。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-tools/src/write.rs:276` | A | 逐字一致 | // 成功文案使用原始参数 path（不是 absolute_path），与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-tools/tests/tool_definitions_golden.rs:3` | A | 逐字一致, 逐字节一致 | //! 「8 个工具的 schema 与 TS 逐字节一致」「description/promptSnippet/promptGuidelines 逐字一致」）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-tools/src/bash.rs:375` | D | 逐字节一致 | /// `read` 工具导出面之外，bash schema 也必须逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-tools/src/bash.rs:760` | D | 完全一致 | /// `sandbox` 为 `None` 时行为与前者完全一致。`Some` 时每次 exec 都通过 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-tools/src/bash.rs:808` | D | 完全一致 | /// 不一致。未设置策略时与 [`create_local_bash_operations`] 完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-tools/src/bash.rs:1057` | D | 逐字一致 | /// 省略 Node 错误末尾的 ` '<path>'`（其余逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-tools/src/edit_diff.rs:8` | D | 逐字节一致 | //! hunk 边界、删除/新增的先后顺序与 JS 逐字节一致（黄金测试见 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-tools/src/edit_diff.rs:21` | D | 逐字一致 | //! - [`apply_edits_to_normalized_content`] 的错误文本逐字保留（§2.8.6），并且 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-tools/src/lib.rs:98` | D | 逐字一致 | //! '{path}'`，与 Node 的 `Error.message` 逐字一致；无法识别时回落为 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-tools/src/renderers/ls.rs:72` | D | 逐字一致 | /// 非字符串值（`null`/布尔/数组/对象）也逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-tools/src/renderers/write.rs:12` | D | 逐字一致 | //! 缓存输出与全量重建逐字一致，纯属性能优化（流式参数每帧只高亮新增行 + 刷新前 50 行）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-tools/tests/file_tools_renderers.rs:618` | D | 逐字一致 | // 参数完整时一次性重建；输出必须逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
