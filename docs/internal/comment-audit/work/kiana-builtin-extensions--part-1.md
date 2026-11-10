# 注释清理工作单：kiana-builtin-extensions--part-1

- 待处理条目：40
- 涉及文件：15

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

- `crates/kiana-builtin-extensions/src/codemode/execute.rs`
- `crates/kiana-builtin-extensions/src/codemode/index.rs`
- `crates/kiana-builtin-extensions/src/codemode/renderer.rs`
- `crates/kiana-builtin-extensions/src/codemode/tool.rs`
- `crates/kiana-builtin-extensions/src/index.rs`
- `crates/kiana-builtin-extensions/src/llama/client.rs`
- `crates/kiana-builtin-extensions/src/llama/mod.rs`
- `crates/kiana-builtin-extensions/src/llama/provider.rs`
- `crates/kiana-builtin-extensions/src/llama/ui.rs`
- `crates/kiana-builtin-extensions/src/mcp/cli.rs`
- `crates/kiana-builtin-extensions/src/mcp/resources.rs`
- `crates/kiana-builtin-extensions/src/mcp/tools.rs`
- `crates/kiana-builtin-extensions/src/mcp/ui.rs`
- `crates/kiana-builtin-extensions/src/theme_snapshot.rs`
- `crates/kiana-builtin-extensions/src/tool_search/tool.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-builtin-extensions/src/llama/ui.rs:245` | A | 与TS一致 | /// 两阶段，顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-builtin-extensions/src/llama/ui.rs:908` | A | 逐字一致 | /// `handleInput(data)`（顺序与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-builtin-extensions/src/llama/ui.rs:1077` | A | 与TS一致 | /// 顺序与 TS 一致：先取消旧目标的焦点 → 清空进度 promise/resolver（在飞的进度页 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-builtin-extensions/src/llama/ui.rs:1460` | A | 与TS一致 | // （40 格），与 TS 一致。NaN 比例得到 "NaN%"、格子数为 0。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-builtin-extensions/src/llama/ui.rs:1644` | A | 与TS一致 | /// 在取消路径被丢弃，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-builtin-extensions/src/llama/ui.rs:1699` | A | 与TS一致 | /// 取消路径与 TS 一致：`ui.progress(state)`（每次循环**重新创建**）先到 → `confirm` → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:16` | A | 逐字一致 | //! TypeError）；Rust 侧返回 `Result<_, AgentToolError>`，`error.message` 与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:664` | A | TS等价物, TS的 | /// `Default` 等价于 TS 的 `{}`：按需创建临时文件，且不提示 `read_mcp_resource`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:750` | A | 与TS一致 | /// 三档与 TS 一致：`< 1024` 用原始值的 `String()` 形态（`null` → `nullB`，字符串 `"2048"` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:14` | D | 逐字一致 | //! 3. 渲染器每帧新建组件而不是复用 `lastComponent`；输出逐字一致（复用只是性能优化）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:178` | D | 逐字一致 | /// → 放弃。文案必须与 Node 的 `uv_strerror` 逐字一致，测试 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-builtin-extensions/src/mcp/tools.rs:784` | D | 逐字一致 | /// 文案与 Node 的参数校验逐字一致，`received` 部分按实际类型描述；用于服务器发来非法 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-builtin-extensions/src/codemode/renderer.rs:42` | A | 逐字一致, TS版本 | //! 渲染文本要和 TS 版本逐字一致，所以数字与截断都按 ECMAScript 语义实现： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-builtin-extensions/src/codemode/renderer.rs:103` | A | 必须与TS, TS的 | /// 逐项列出（含 BOM U+FEFF 与行分隔符 U+2028/U+2029），必须与 TS 的 `\s` 保持一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-builtin-extensions/src/codemode/renderer.rs:10` | D | 逐字一致 | //! 因此每次都新建 `Container`（与 `mcp/tools.rs` 渲染器相同的处理）。输出逐字一致，复用 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-builtin-extensions/src/codemode/renderer.rs:40` | D | 保真 | //! # 数值与字符串的 JS 保真 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-builtin-extensions/src/llama/client.rs:661` | A | 与TS一致, 完全一致 | /// 顺序与 TS 完全一致：`trim` → `new URL` → 协议检查 → 清空 hash/search → 路径去尾斜杠 → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-builtin-extensions/src/llama/client.rs:779` | A | TS等价物, TS的 | /// 回退到 `llama.cpp returned HTTP <status>`；读体失败等价于 TS 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-builtin-extensions/src/llama/client.rs:991` | A | 逐字一致 | // 不会被归一），与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-builtin-extensions/src/llama/client.rs:1268` | A | 与TS一致 | // 结束条件与 TS 一致：`finished` 单独成立即可；否则要求条目存在， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-builtin-extensions/src/codemode/tool.rs:204` | A | 逐字一致 | /// 名字是 TS 导出名的逐字对应，`#[allow(non_upper_case_globals)]` 仅为保留它；重命名会丢掉 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-builtin-extensions/src/codemode/tool.rs:257` | A | 逐字一致 | /// `serde_json` 开启了 `preserve_order`，所以序列化字符串与 TS 逐字一致；测试 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-builtin-extensions/src/codemode/tool.rs:316` | A | 与TS一致 | /// future 的版本，让实现可以把调用转发到同线程任务并等待完成（写入顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-builtin-extensions/src/mcp/ui.rs:888` | A | 与TS一致 | // 与 TS 一致：处理器之后仍请求一次重绘（换页时 set_content 已经请求过一次）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-builtin-extensions/src/mcp/ui.rs:62` | D | 逐字一致 | //! 逐字一致；修改会同时改变 `/mcp` 的输出与测试断言。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-builtin-extensions/src/mcp/ui.rs:1015` | D | 逐字节一致 | /// 快照必须与 `Theme::fg`/`Theme::bold` 逐字节一致（空串与探针含 NUL 的情况也要覆盖）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-builtin-extensions/src/tool_search/tool.rs:324` | A | 与TS一致 | /// `b = 0.75` 与 TS 一致；构造器不校验取值，非有限值会原样参与运算——产生的 `NaN` 分数会 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-builtin-extensions/src/tool_search/tool.rs:630` | A | 逐字一致, TS的 | /// 文案与 TS 的 `TOOL_SEARCH_DESCRIPTION` 逐字一致（spec §2.15.11）；它直接进入模型可见的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-builtin-extensions/src/tool_search/tool.rs:637` | A | TS等价物, TS的 | /// `\r`，等价于 TS 的 `/\r?\n/`。只保留第一行；后续行丢弃，行内空白不做额外规整。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-builtin-extensions/src/llama/provider.rs:85` | A | 逐字一致 | /// 目录失效。与 TS 常量逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-builtin-extensions/src/llama/provider.rs:313` | A | 逐字一致 | /// 映射形状与 TS 逐字一致：`off`/`medium` 有字符串值，其余等级显式写 `None`（JSON `null`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-builtin-extensions/src/mcp/resources.rs:40` | A | 必须与TS, 逐字一致 | //! - 参数与输出 schema 是模型可见的工具声明，必须与 TS 常量逐字一致（list 的参数对象在 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-builtin-extensions/src/mcp/resources.rs:499` | A | 与TS一致 | // 顺序与 TS 一致；`join_all` 保持输入顺序，因此可以用下标把结果对回服务器名。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-builtin-extensions/src/theme_snapshot.rs:7` | D | 逐字节一致 | //! 捕获到的开/关序列与 `Theme::fg`/`Theme::bold` 逐字节一致：用一个不会出现在 ANSI 序列 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 35 | `crates/kiana-builtin-extensions/src/theme_snapshot.rs:26` | D | 完全一致 | //! `AnsiWrap::wrap` 只做 `open + text + close`。这与 `Theme::fg` 的形状完全一致；但 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-builtin-extensions/src/codemode/execute.rs:349` | A | TS等价物, TS的 | /// `"The script already settled"`（host 用 `catch_unwind` 接住，等价于 TS 的同步抛出）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-builtin-extensions/src/codemode/index.rs:147` | A | TS等价物, TS的 | /// 等待应答让调用方在返回的 future 完成时知道写入已经执行，等价于 TS 的同步 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-builtin-extensions/src/index.rs:69` | A | 必须与TS | /// 顺序；`replaceable` 参与加载完成后的同名占位判定，不改变工厂的执行顺序。名字必须与 TS | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-builtin-extensions/src/llama/mod.rs:43` | A | 逐字一致 | //! - 对外错误统一是 `kiana_ai::error::AiError`；用户可见文案与 TS 逐字一致（见 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-builtin-extensions/src/mcp/cli.rs:108` | A | 逐字一致, TS的 | /// 文本与 TS 的 `HELP` 模板逐字一致：本文件的单元测试 `the_help_banner_matches_the_ts_text` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
