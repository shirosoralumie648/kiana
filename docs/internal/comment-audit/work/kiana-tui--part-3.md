# 注释清理工作单：kiana-tui--part-3

- 待处理条目：40
- 涉及文件：24

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

- `crates/kiana-tui/src/alt_screen_search.rs`
- `crates/kiana-tui/src/colors.rs`
- `crates/kiana-tui/src/components/box_component.rs`
- `crates/kiana-tui/src/components/input.rs`
- `crates/kiana-tui/src/components/loader.rs`
- `crates/kiana-tui/src/components/markdown/mod.rs`
- `crates/kiana-tui/src/components/settings_list.rs`
- `crates/kiana-tui/src/editor_component.rs`
- `crates/kiana-tui/src/flash.rs`
- `crates/kiana-tui/src/keys.rs`
- `crates/kiana-tui/src/layout.rs`
- `crates/kiana-tui/src/lib.rs`
- `crates/kiana-tui/src/markdown/block.rs`
- `crates/kiana-tui/src/markdown/inline.rs`
- `crates/kiana-tui/src/markdown/lexer.rs`
- `crates/kiana-tui/src/markdown/mod.rs`
- `crates/kiana-tui/src/markdown/table.rs`
- `crates/kiana-tui/src/native/mod.rs`
- `crates/kiana-tui/tests/editor.rs`
- `crates/kiana-tui/tests/exit_restore.rs`
- `crates/kiana-tui/tests/fuzzy.rs`
- `crates/kiana-tui/tests/regression_review_fixes.rs`
- `crates/kiana-tui/tests/tab_width.rs`
- `crates/kiana-tui/tests/terminal_image.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-tui/src/colors.rs:82` | A | 与TS一致 | /// `oklchColor(l, c, h)`：`h` 归一化到 `[0, 360)`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-tui/src/colors.rs:111` | D | 完全一致 | /// `[0-9]` / `[eE]`，字母关键字的 `i` 也展开成 `[oO][kK]…` 形式（与 JS 旧式 `i` 完全一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-tui/src/components/box_component.rs:20` | D | 完全一致 | //! 完全一致则直接返回缓存的最终行。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 4 | `crates/kiana-tui/src/components/box_component.rs:176` | D | 强约束 | /// 判断缓存能否用于本次输出：宽度、背景采样、子项原始行（逐行比较）都必须一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-tui/src/components/loader.rs:13` | A | TS等价物, TS的 | //! 每次到期调用 `requestRender` 并重新排程，等价于 TS 的 `setInterval`。未安装定时器时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-tui/src/components/loader.rs:319` | A | 与TS一致 | /// 规则（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-tui/src/components/settings_list.rs:78` | A | 与TS一致 | /// 旧值相同（例如只有一个候选值）也会调用，与 TS 一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-tui/src/components/settings_list.rs:527` | A | 与TS一致 | // 分支优先级与 TS 一致：有 submenu 就打开子菜单，否则才在 values 之间循环。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-tui/src/keys.rs:367` | A | 与TS一致 | /// 顺序与 TS 一致。用途有两个：`is_symbol_key` 判断“这个字符是不是符号键”；Kitty 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-tui/src/keys.rs:530` | A | 必须与TS | // 所以数组保存全部变体，匹配时做包含判断。凡是标注【逐字】的表，顺序与拼写都必须与 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-tui/src/layout.rs:178` | A | 逐字一致, TS的 | /// 字段本身保留是为了与 TS 的比较器逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-tui/src/layout.rs:487` | A | 与TS一致 | // visible 回调拿到的是整帧视口，而不是本 Stack 的分配尺寸（与 TS 一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-tui/src/markdown/block.rs:124` | D | 完全一致 | /// 列表缩进判断依赖这一点（与 marked 的 `/[^ ]/` 完全一致）。返回 `-1` 表示整行只有空格； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-tui/src/markdown/block.rs:1064` | D | 强约束 | // 表头与对齐列数必须一致；行可以不同。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-tui/src/markdown/lexer.rs:173` | A | 必须与TS, TS源 | /// 顺序必须与 TS 源保持一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-tui/src/markdown/lexer.rs:200` | A | 逐字一致 | // 规则源（逐字对应 marked 18.0.11 的 src/rules.ts） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-tui/src/markdown/mod.rs:422` | A | TS等价物 | /// 新建一个空表格 token（等价于 TS 对象字面量里的空数组），由 `block` 的表格 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-tui/src/markdown/mod.rs:23` | D | 完全一致 | //! 换算成 UTF-16 码元（见 `lexer::utf16_len` 等）。ASCII 输入完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-tui/src/native/mod.rs:17` | A | 与TS一致 | //! 缓存的语义也与 TS 一致：缓存的是**后端是否可用**，不是 `DISPLAY` 的可用性——Linux 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-tui/src/native/mod.rs:102` | A | 逐字一致, 对应TS | /// 完整错误文案，逐字对应 TS `reject(new Error(message))` 的 message；构造后不再加工。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-tui/tests/editor.rs:3` | A | 逐字一致, TS的 | //! 用例标题与分组逐字保留在测试函数名/注释里。TS 的 `flushAutocomplete()`（排空微任务） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-tui/tests/editor.rs:4003` | D | 逐字一致 | /// 时模板字符串会写出字面 "undefined"（`[paste #1undefined]`）。该行为必须逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-tui/tests/exit_restore.rs:75` | A | 与TS一致 | /// 分组粘贴复位、Kitty 键盘协议弹出（都在 `restoreOnExit` 里，顺序与 TS 一致）以及输出结尾； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-tui/tests/exit_restore.rs:179` | A | 与TS一致, 完全一致 | /// 恢复管线（顺序、次数、结尾）与 TS 完全一致；`TuiAltScreen` 落地后其自己的 fixture 会 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-tui/tests/fuzzy.rs:3` | A | 逐字一致, TS源, packages/ | //! TS 源：`packages/tui/test/fuzzy.test.ts`（17 个 `it`，标题逐字保留）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-tui/tests/fuzzy.rs:4` | A | 完全一致, TS的 | //! 分数是 TS 的逐位数值，Rust 侧必须完全一致，因此这里用 `assert_eq!` 断言精确值。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-tui/tests/regression_review_fixes.rs:15` | A | 逐位一致 | //! \| 8 \| Oklab / OKHSL 数值与 TS 逐位一致 \| | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-tui/tests/regression_review_fixes.rs:433` | A | 与TS一致 | /// `okhslToRgb` 的整条链条（`Math.cbrt` → `chromaStops` → 8 位裁剪）与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-tui/tests/tab_width.rs:5` | A | 必须与TS, TS的 | //! 终端行"：Rust 侧直接检查**写入流**——按 `\r\n` 切开后每行的可见文本必须与 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-tui/tests/tab_width.rs:6` | D | 完全一致 | //! `getViewport()` 完全一致（三行内容都短于终端宽度，不存在滚动），并且写入流里不能出现 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-tui/tests/terminal_image.rs:3` | A | 逐字一致, TS的 | //! 结构按 TS 的 `describe` 分组，标题逐字保留。需要 `Image` 组件（tui-components 单元）的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-tui/tests/terminal_image.rs:1884` | A | 必须与TS, 逐字节一致, TS的 | /// 回归（review finding 2/7）：Kitty 分块必须与 TS 的 `slice(offset, offset + 4096)` 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-tui/src/alt_screen_search.rs:7` | A | 与TS一致, 完全一致 | //! UTF-16 偏移，再与 span 的 `text_start`/`text_end` 比较——数值与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-tui/src/components/input.rs:25` | A | 与TS一致 | //! [`Component::handle_input`] 是一条"首个匹配即返回"的分支链，顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-tui/src/components/markdown/mod.rs:876` | A | 必须与TS, 与TS保持 | /// 后应用的样式在外层；顺序变化会改变 ANSI 嵌套结果，必须与 TS 保持一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-tui/src/editor_component.rs:132` | A | TS等价物, TS的 | /// 传 `None` 等价于 TS 的 `borderColor = undefined`（渲染时用 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-tui/src/flash.rs:13` | D | 逐字一致 | //! `flash()` 的插入顺序与 `render()` 的输出顺序保持逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-tui/src/lib.rs:89` | A | 逐字一致 | //! 文案与 TS 异常逐字相同）与 `native::ClipboardError`（剪贴板传输失败）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-tui/src/markdown/inline.rs:4` | A | 与TS一致, 完全一致 | //! `inlineTokens` 的执行顺序与 TS 完全一致：LaTeX 扩展 → escape → tag → link → reflink → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-tui/src/markdown/table.rs:4` | D | 逐字一致 | //! 太窄时回退为原始 markdown。边框字符与空格数逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
