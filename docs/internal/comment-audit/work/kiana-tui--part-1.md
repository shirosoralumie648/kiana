# 注释清理工作单：kiana-tui--part-1

- 待处理条目：40
- 涉及文件：5

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

- `crates/kiana-tui/src/alt_screen.rs`
- `crates/kiana-tui/src/components/editor/mod.rs`
- `crates/kiana-tui/src/latex.rs`
- `crates/kiana-tui/src/stdin.rs`
- `crates/kiana-tui/src/tui.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-tui/src/latex.rs:94` | A | 与TS一致 | //! `Pi` 条目与 TS 一致（此前的 `Kiana` 重命名残留已修正，`\Pi` 现在解析为 `Π`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-tui/src/latex.rs:108` | A | 逐字一致, TS的 | //! TS 的逐字对应关系。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-tui/src/latex.rs:120` | A | 逐字一致, 对应TS | /// `RenderLatexOptions`（逐字对应 TS）：`display` 为真才启用二维布局。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-tui/src/latex.rs:197` | A | 与TS一致 | /// 落单占位符 → `U+FFFD`（终端字节与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-tui/src/latex.rs:332` | A | TS等价物 | /// `RangeError`（约 7k 层，随栈状态浮动），这里用同一个 8192 作为"等价于 TS 抛异常"的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-tui/src/latex.rs:391` | A | 完全一致, TS的 | // 这是 TS 的缺陷，但"行为完全一致"要求复刻，而且普通文档也可能触发（`\constructor` 不是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-tui/src/latex.rs:2236` | A | 与TS一致 | /// 都不会展开其中的标记——`Operator` 的 `lower`/`upper` 原样拼进结果（与 TS 一致）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-tui/src/latex.rs:2623` | A | TS等价物 | /// 布局节点图（只含 renderLayout 真正递归的字段）跑一遍迭代 DFS：有环就等价于 TS 抛异常 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-tui/src/latex.rs:2877` | A | 逐字一致, TS的 | /// 分支顺序就是匹配优先级（与 TS 的 `parseSequence` 逐字对应，调整顺序会改变输出）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-tui/src/latex.rs:3111` | A | 完全一致, TS的 | /// 分派顺序与 TS 的 `parseCommand` 完全一致，顺序本身影响结果——同一命令名同时出现在 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-tui/src/latex.rs:3143` | A | TS等价物 | /// `None`（等价于 TS 抛出的 `TypeError` 冒泡出 `renderLatex`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-tui/src/latex.rs:24` | D | 完全一致 | //! 不同（Rust 是 `U+FFFD`）；但**终端字节完全一致**——Node 把落单代理写成 UTF-8 时正是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-tui/src/latex.rs:37` | D | 逐字一致 | //! `space_named_operator_left` / `space_named_operator_right`（匹配互不重叠，结果逐字相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-tui/src/latex.rs:2035` | D | 逐字一致 | /// 单遍替换，不用正则。语义与 `replace` 逐字相同：匹配 `\s* 运算符 \s*` 并把整段换成运算符， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 15 | `crates/kiana-tui/src/components/editor/mod.rs:44` | A | 与TS一致 | //! 交互结果与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-tui/src/components/editor/mod.rs:662` | A | TS等价物 | /// 参数：`width`、隐藏行数、边框着色函数、**默认实现已着色好的边框**（等价于 TS 覆写里 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-tui/src/components/editor/mod.rs:829` | A | 与TS一致 | /// 等 provider 归位后按同一快照补做判定，结果与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-tui/src/components/editor/mod.rs:1152` | A | 与TS一致 | /// 纯查询：不会推进防抖/在飞请求（与 TS 一致）。宿主要让到期的防抖定时器生效， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-tui/src/components/editor/mod.rs:3144` | A | 逐字一致, TS的 | // 这是 TS 的既有行为，必须逐字保留。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-tui/src/components/editor/mod.rs:3931` | A | 与TS一致 | /// 顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-tui/src/components/editor/mod.rs:4566` | A | TS等价物, TS的 | // `None` 等价于 TS 的 `undefined`：恢复恒等着色（不加 ANSI）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-tui/src/components/editor/mod.rs:3424` | D | 逐字一致 | /// 粘性列决策表（源码注释逐字保留）： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-tui/src/components/editor/mod.rs:4230` | D | 完全一致 | /// 文本与发起时完全一致、光标行列与发起时一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-tui/src/tui.rs:461` | A | 完全一致, TS的 | /// 给底层组件，因此路径层级与 TS 的 `Container` 层级完全一致，任意深度的嵌套容器都 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-tui/src/tui.rs:718` | A | 与TS一致 | /// 与 TS 一致：**不动 `mouse_layout`**（缓存里的是 id，不是下标，所以移除后不会把 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-tui/src/tui.rs:1238` | A | 与TS一致 | /// [`TuiBase::pump_pending_input`]）都会先刷新，因此"下一个观察点"看到的标志与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-tui/src/tui.rs:2163` | A | TS等价物 | /// `base` 是 `TuiBase` 的默认实现（`?2031l` 视通知开关 + `?25h`），等价于 TS 子类里的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-tui/src/tui.rs:3301` | A | 与TS一致 | /// `handleTerminalInput(data)`——顺序与 TS 一致（`spec/tui-core.md` §2.9.11）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-tui/src/tui.rs:3618` | A | 与TS一致 | /// 与 TS 一致：无论是否有 `setCellDimensions` 的接收方，命中格式就**消费**掉这段输入 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-tui/src/tui.rs:4218` | A | 必须与TS, 与TS一致, TS: | // 运算顺序必须与 TS 一致（`tui.ts:1272-1275`）：先 `parseFloat(p) / 100` 再乘，最后 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-tui/src/tui.rs:3045` | D | 逐字一致 | /// `kiana-coding-agent` 的启动流程）或测试在进程收尾时调用本方法。语义逐字一致： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-tui/src/stdin.rs:212` | A | 与TS一致 | /// 它的 UTF-16 宽度同样是 1，因此后续的长度判定与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-tui/src/stdin.rs:578` | A | 与TS一致 | /// 单字节 `> 127` 的 Buffer 走 `ESC + (byte - 128)` 的高字节转换（与 TS 一致，旧式 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-tui/src/stdin.rs:591` | A | TS等价物, TS的 | /// [`StdinBuffer::process`] 的显式时钟版本（测试用；等价于 TS 的 mock timers）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-tui/src/stdin.rs:606` | A | TS等价物 | /// 等价于 TS `setEncoding("utf8")` 之后的 `data` 事件：数据已经是完整的 UTF-8 文本。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-tui/src/stdin.rs:629` | A | 与TS一致 | // 与 TS 一致：空 data 在空缓冲上也会发出一个空 data 事件（测试 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-tui/src/stdin.rs:32` | D | 逐字一致 | //! - 对 BMP 文本（含中日韩）行为逐字一致； | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 38 | `crates/kiana-tui/src/alt_screen.rs:4773` | A | 与TS一致 | /// [`Tui::add_child`]、[`Tui::remove_child`] 与 [`Tui::clear_children`] 都与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-tui/src/alt_screen.rs:3729` | D | 完全一致 | // 单击判定：从未拖动，且落点（行/列/是否同一滚动视图）与按下点完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 40 | `crates/kiana-tui/src/alt_screen.rs:4053` | D | 逐字一致 | /// 原有 SGR 序列逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
