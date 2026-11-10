# 注释清理工作单：kiana-tui--part-2

- 待处理条目：40
- 涉及文件：9

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

- `crates/kiana-tui/src/autocomplete.rs`
- `crates/kiana-tui/src/components/scroll_view.rs`
- `crates/kiana-tui/src/components/stack.rs`
- `crates/kiana-tui/src/fuzzy.rs`
- `crates/kiana-tui/src/main_screen.rs`
- `crates/kiana-tui/src/oklab.rs`
- `crates/kiana-tui/src/terminal.rs`
- `crates/kiana-tui/tests/chat_simple.rs`
- `crates/kiana-tui/tests/latex.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-tui/src/autocomplete.rs:2607` | A | 与TS一致 | // 文本是否同时含 `/` 与空格，与 TS 一致；这里不额外补空格。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-tui/src/autocomplete.rs:2821` | A | 逐字一致, TS的, 对应TS | // `raw_prefix.is_empty()` 重复，保留是为了逐字对应 TS 的条件列表。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-tui/src/autocomplete.rs:2884` | A | 与TS一致 | // 路径形态。分支顺序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-tui/src/autocomplete.rs:921` | D | 逐位一致 | // locale），所以要让排序逐位一致，必须同时复刻 **locale 解析** 与 **collation 选择**。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-tui/src/autocomplete.rs:945` | D | 逐位一致 | // ICU4X 2.4.0 在**默认选项**下与 Node 的 ICU 78.3 逐位一致（en-US / und / zh-CN / zh-TW / | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 6 | `crates/kiana-tui/src/components/stack.rs:63` | A | TS等价物, TS的 | /// 固定基准尺寸；`None` 等价于 TS 的 `undefined` 与 `"auto"`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-tui/src/components/stack.rs:67` | A | TS等价物 | /// 剩余空间的分配权重；`None` 等价于 TS 默认值 `0`（不参与 grow）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-tui/src/components/stack.rs:69` | A | TS等价物 | /// 空间不足时的收缩权重；`None` 等价于 TS 默认值 `1`（可收缩）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-tui/src/components/stack.rs:71` | A | TS等价物 | /// 最小尺寸；`None` 等价于 TS 默认值 `0`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-tui/src/components/stack.rs:73` | A | TS等价物 | /// 最大尺寸；`None` 等价于 TS 默认值 `Number.MAX_SAFE_INTEGER`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-tui/src/main_screen.rs:27` | A | 逐字一致, TS的 | //! - TS 的 `throw new Error(...)`（超宽行）在 Rust 里是 `panic!`，载荷是**逐字相同**的错误 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-tui/src/main_screen.rs:38` | A | 与TS一致 | //! - `process.env` 的读取一律在渲染时现场进行（与 TS 一致），便于测试用环境变量隔离。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-tui/src/main_screen.rs:330` | A | 与TS一致 | /// 目标行是 `previous_lines.len()`（最后一行之下）。输出顺序与 TS 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-tui/src/main_screen.rs:877` | A | 与TS一致 | /// 触发条件是差分路径里非图片行的 `visible_width(line) > width`。顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-tui/src/main_screen.rs:2036` | A | 与TS一致 | // `resetRenderState` 不清 kitty id 集合（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-tui/src/oklab.rs:322` | A | 与TS一致 | /// 顺序与 TS 一致：先编码传递函数，再把结果夹到 `[0, 1]`，然后乘 255、`js_round`。返回值 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-tui/src/oklab.rs:570` | A | 与TS一致 | // 与 TS 一致：饱和度夹到 [0, 1]；色相已在 [0, 360)，明度不再夹取。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-tui/src/oklab.rs:43` | D | 逐位一致 | //! - 目标是**与 V8/Node 逐位一致**：`tests/oklab_parity.rs` 用 bit pattern 黄金数据断言 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-tui/src/oklab.rs:60` | D | 逐位一致 | /// `sqrt(a*a+b*b)`（同一样本集里约 24% 差 1 ulp）。这个逐位一致的版本在 165k 个 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-tui/src/oklab.rs:172` | D | 逐位一致 | /// 样本（含次正规数、负数、±0、Inf、NaN）上与 V8 逐位一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-tui/src/terminal.rs:14` | A | TS等价物, TS的 | //! 与已有输入管道的宿主使用，等价于 TS 的 `stdinBuffer.process(data)`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-tui/src/terminal.rs:75` | A | TS等价物, TS的 | /// `process.stdin.on("data", handler)` 的处理器类型（等价于 TS 的 `(data: string) => void`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-tui/src/terminal.rs:298` | A | TS等价物 | /// `resolveEscapeTimeoutMs(env)` 的显式环境查找版本（测试用；等价于 TS 传入 `env` 对象）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-tui/src/terminal.rs:338` | A | TS等价物, TS的, TS里 | /// 等价于 TS 里 `takeOverStdout()` 对 `process.stdout.write` 的替换：TS 的接管是运行时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-tui/src/terminal.rs:283` | D | 逐字节一致 | /// U+FFFD，因此 `Some` 不保证与原值逐字节一致。需要 JS 真值语义的调用方（如 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-tui/tests/latex.rs:766` | A | 与TS一致 | // 没有布局节点时（display=false）标记只是普通字符，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-tui/tests/latex.rs:870` | A | 与TS一致, 完全一致 | // 与 TS 完全一致；跨行的两半各自是 U+FFFD。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-tui/tests/latex.rs:6` | D | 逐字一致 | //! 19 个显式 `it` 的标题逐字保留为 Rust 测试名。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-tui/tests/latex.rs:61` | D | 逐字一致 | /// 每个 case 的标题都逐字保留（`JSON.stringify` 对字符串就是带引号的 JSON 字面量）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 30 | `crates/kiana-tui/tests/latex.rs:75` | D | 逐字一致 | // 19 个显式 `it`（标题逐字保留） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-tui/tests/chat_simple.rs:15` | A | 必须与TS, 与TS一致 | //! 行，但**不**为此额外插入子项（子项下标与插入位置必须与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-tui/tests/chat_simple.rs:75` | A | 逐字一致, 对应TS | /// 插入位置逐字对应 TS：每次都是 `children.length - 1`（输入框之前），而不是固定的槽位 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-tui/tests/chat_simple.rs:164` | A | TS等价物, TS里 | // 走一遍提交流程（等价于 TS 里用户输入一条消息）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-tui/tests/chat_simple.rs:7` | D | 逐字一致 | //! **逐字保留**：欢迎文本、两条斜杠命令的语义（`children.length > 3` 时删除倒数第二个子项； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-tui/src/components/scroll_view.rs:25` | A | 逐字一致, TS的 | //! 4. **构造错误**：TS 的 `throw` 在 Rust 侧变成 [`ScrollViewError`]（`Display` 输出逐字相同的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-tui/src/components/scroll_view.rs:92` | A | 逐字一致 | /// 每个变体的 `Display` 文案与 TS 异常的 `message` 逐字相同（见各变体的注释）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-tui/src/components/scroll_view.rs:316` | A | 与TS一致 | // 与 TS 一致：整段算术保持 f64，`Math.min/max` 在双精度下完成，避免 `i64` 溢出 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-tui/src/fuzzy.rs:11` | A | 逐位一致 | //! 保证分数与 TS 逐位一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-tui/src/fuzzy.rs:19` | A | 必须与TS, 逐位一致 | //! 决定列表顺序，因此必须与 TS 逐位一致：`tests/fuzzy.rs` 与 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-tui/src/fuzzy.rs:162` | D | 完全一致 | // 这个分支只可能出现在 text 与 query 码元序列完全一致时。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
