# 注释清理工作单：kiana-tui--part-4

- 待处理条目：27
- 涉及文件：27

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

- `crates/kiana-tui/src/native/darwin.rs`
- `crates/kiana-tui/src/native/linux_x11.rs`
- `crates/kiana-tui/src/native/module_path.rs`
- `crates/kiana-tui/src/native/win32.rs`
- `crates/kiana-tui/src/terminal_image.rs`
- `crates/kiana-tui/src/text.rs`
- `crates/kiana-tui/tests/autocomplete.rs`
- `crates/kiana-tui/tests/autocomplete_skill_slash.rs`
- `crates/kiana-tui/tests/bug_regression_isimageline_startswith_bug.rs`
- `crates/kiana-tui/tests/cell_size_input.rs`
- `crates/kiana-tui/tests/colors.rs`
- `crates/kiana-tui/tests/common/mod.rs`
- `crates/kiana-tui/tests/image_test.rs`
- `crates/kiana-tui/tests/keybindings.rs`
- `crates/kiana-tui/tests/keys.rs`
- `crates/kiana-tui/tests/latex_review_regression.rs`
- `crates/kiana-tui/tests/markdown.rs`
- `crates/kiana-tui/tests/native_clipboard.rs`
- `crates/kiana-tui/tests/native_clipboard_linux.rs`
- `crates/kiana-tui/tests/native_platform.rs`
- `crates/kiana-tui/tests/overlay_options.rs`
- `crates/kiana-tui/tests/truncate_to_width.rs`
- `crates/kiana-tui/tests/tui_alt_screen.rs`
- `crates/kiana-tui/tests/visible_width.rs`
- `crates/kiana-tui/tests/word_navigation.rs`
- `crates/kiana-tui/tests/word_segmentation_node_parity.rs`
- `crates/kiana-tui/tests/wrap_ansi.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-tui/src/native/darwin.rs:32` | D | 逐字一致 | //! `Err(ClipboardError)`。失败文案与 C 源逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 2 | `crates/kiana-tui/src/native/linux_x11.rs:1033` | D | 强约束 | /// `clipboard-x11-test.c` 的 `metadata` 模式：INCR 分块元数据必须一致才累积。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-tui/src/native/module_path.rs:53` | A | 与TS一致 | /// 候选顺序与 TS 一致（测试逐项断言）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-tui/src/native/win32.rs:363` | D | 逐字一致 | // `String` 只能存合法的 Unicode 标量。配对代理项（全部正常文本）逐字保留，未配对的 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-tui/src/terminal_image.rs:2474` | A | 与TS一致 | /// 顺序与 TS 一致；`CMUX_WORKSPACE_ID` 在列表里但源码从不读取，保留以对齐清理行为。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-tui/src/text.rs:12` | A | 与TS一致, TS的 | //! 一律是 **UTF-16 码元**，与 TS 一致。光标落在代理对**中间**时，TS 的 `slice` 会切出 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-tui/tests/autocomplete.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/autocomplete.test.ts`（用例标题逐字保留在测试函数文档里）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-tui/tests/autocomplete_skill_slash.rs:1` | A | 逐字一致, packages/ | //! `packages/tui/test/autocomplete-skill-slash.test.ts`（7 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-tui/tests/bug_regression_isimageline_startswith_bug.rs:7` | D | 逐字一致 | //! 修复：改成 `includes()`，任意位置都能探测。四组标题逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-tui/tests/cell_size_input.rs:43` | A | TS等价物, TS的 | /// `withImageTerminal(...)`：把能力缓存播种为"支持 Kitty 图片"，等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-tui/tests/colors.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/colors.test.ts`（4 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-tui/tests/common/mod.rs:5` | A | 与TS一致 | //! 断言由各测试自己用 `contains`/前缀比较完成。行为上与 TS 一致的部分： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-tui/tests/image_test.rs:18` | A | TS等价物, TS里 | /// 只用 `handleInput` 的编辑器桩（Ctrl+C 退出，等价于 TS 里的匿名 editor 对象）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-tui/tests/keybindings.rs:4` | A | 与TS一致, 完全一致 | //! 这里只测 `KeybindingsManager` 实例（与 TS 完全一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-tui/tests/keys.rs:25` | A | TS等价物 | /// 设置 kitty 状态并在 drop 时复位为 `false`（等价于 TS 每个用例末尾的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-tui/tests/latex_review_regression.rs:550` | A | 强约束 | // 2,000 层：TS 能渲染，Rust 必须一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-tui/tests/markdown.rs:171` | A | 与TS一致 | /// `stripAnsi`：只移除 SGR 序列（`/\x1b\[[0-9;]*m/g`），与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-tui/tests/native_clipboard.rs:33` | D | 逐字一致 | /// `complete_clipboard_work` 的 `status != 0` 文案逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-tui/tests/native_clipboard_linux.rs:127` | A | 逐字一致, TS的 | /// 2. 否则尝试启动私有 `Xvfb`（`-displayfd 1`，与 TS 的 `startX11` 逐字相同的参数）；每个 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-tui/tests/native_platform.rs:204` | D | 强约束 | /// 未编译后端时 `native_backend_compiled()` 与 `getNativeClipboard()` 必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-tui/tests/overlay_options.rs:8` | A | 与TS一致, 完全一致 | //! 一一对应。断言内容与 TS 完全一致（行号、列号、包含关系）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-tui/tests/truncate_to_width.rs:4` | D | 逐字一致 | //! 嵌套的 `visibleWidth` 7 个 `it`，标题逐字保留）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-tui/tests/tui_alt_screen.rs:3981` | A | 逐字节一致 | /// 选择跨入视口时，起始行之后的可见行从第 0 列整行高亮（TS 帧逐字节一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-tui/tests/visible_width.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/visible-width.test.ts`（4 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-tui/tests/word_navigation.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/word-navigation.test.ts`（19 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-tui/tests/word_segmentation_node_parity.rs:9` | A | 与TS一致 | //! 这个测试是 [`kiana_tui::text::get_word_segments`] 与 TS 一致性的主要证据： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-tui/tests/wrap_ansi.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/wrap-ansi.test.ts`（19 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
