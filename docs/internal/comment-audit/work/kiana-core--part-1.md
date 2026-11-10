# 注释清理工作单：kiana-core--part-1

- 待处理条目：40
- 涉及文件：7

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

- `crates/kiana-core/src/clipboard.rs`
- `crates/kiana-core/src/frontmatter.rs`
- `crates/kiana-core/src/git.rs`
- `crates/kiana-core/src/mcp_servers.rs`
- `crates/kiana-core/src/shell.rs`
- `crates/kiana-core/src/syntax_highlight.rs`
- `crates/kiana-core/src/tools_manager.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-core/src/mcp_servers.rs:10` | A | 逐字一致, TS的 | //! ## 与 TS 的已知差异（都是类型化带来的，行为已尽量逐字保留） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-core/src/mcp_servers.rs:15` | A | 逐字节一致, TS的 | //! 字段与未识别键（整数索引键按 JS 规则升序在前），与 TS 的 `JSON.stringify` 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-core/src/mcp_servers.rs:59` | A | 逐字一致 | //! 哪一条）。下面的顺序按代码实际执行列出；函数文档声明该顺序与错误文案同 TS 逐字一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-core/src/mcp_servers.rs:81` | A | 逐字节一致, TS的 | //! `JSON.stringify` 逐字节一致（见文件头“与 TS 的已知差异”第 1 条）。`PartialEq` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-core/src/mcp_servers.rs:450` | A | 与TS一致 | /// 字段顺序与 TS 一致：`exposure`、`description`、`toolExposure`、`enabled`、`timeout`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-core/src/mcp_servers.rs:722` | A | 与TS一致, TS里 | /// 与 TS 一致（`createToolNameMatcher(["*"])("a\rb")` 在 TS 里是 `false`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-core/src/mcp_servers.rs:966` | A | TS等价物, TS的 | // `oauth` 键缺失（`None`）是合法的，等价于 TS 的 `undefined`；显式 `null` 不是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-core/src/mcp_servers.rs:1007` | A | 与TS一致 | // 默认端口（如 http 的 `:80`，`url` crate/WHATWG 会规范化掉）时都不比较，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-core/src/mcp_servers.rs:1296` | A | 逐字一致, TS里 | /// 校验顺序与错误文案与 TS 逐字一致；显式的 JSON `null` 视为“值非法”（TS 里 `null` 不是 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-core/src/mcp_servers.rs:1332` | A | 与TS一致 | // 第 4 步：公共字段。顺序与 TS 一致，先失败的先返回。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-core/src/mcp_servers.rs:1467` | A | 与TS一致 | // 这条兜底文案不区分具体原因，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-core/src/clipboard.rs:18` | A | 与TS一致 | //! 公开函数用进程的真实状态构造默认的 [`ClipboardTestIo`]，因此行为与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-core/src/clipboard.rs:152` | A | 逐字一致, 对应TS | /// 返回值语义（逐字对应 TS）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-core/src/clipboard.rs:590` | A | 逐字一致 | /// 命令链与 TS 逐字一致：`TERMUX_VERSION` → `termux-clipboard-get`；Linux 且 `WAYLAND_DISPLAY` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-core/src/clipboard.rs:799` | A | 逐字一致 | // Wayland → X11 → 通用。顺序与 TS 逐字一致，测试按整句断言。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-core/src/clipboard.rs:499` | D | 完全一致 | /// [`ClipboardTestIo::from_process`]），编码与超限语义完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-core/src/clipboard.rs:674` | D | 逐字一致 | /// Promise rejection），而不是压成 `None`。其余映射逐字一致：没有原生后端 / 后端不导出 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-core/src/clipboard.rs:947` | D | 逐字一致 | /// 字面量 `undefined\temp`，这里逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-core/src/git.rs:60` | A | TS等价物 | //! - hosted 候选中的 percent 解码失败只会跳过该候选（等价于 TS 捕获 `URIError` 后 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-core/src/git.rs:76` | A | 与TS一致 | /// 序列化后的字段名与 TS 一致（`type`/`repo`/`host`/`path`/`ref`/`pinned`）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-core/src/git.rs:118` | A | 逐字一致, TS的, 对应TS | /// scp-like `git@host:path` —— 逐字对应 TS 的 `/^git@([^:]+):(.+)$/`： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-core/src/git.rs:385` | A | TS等价物 | /// `@`/`:` 不应影响 URL 修补。找不到 `#` 时等价于 TS 传入 `Infinity`，搜索全串。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-core/src/git.rs:669` | A | TS等价物 | /// 5. 所有 percent 解码失败都返回 `None`（等价于 TS 在 `fromUrl` 里捕获 `URIError` | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-core/src/git.rs:20` | D | 逐字一致 | //! `..` 路径段）逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-core/src/git.rs:25` | D | 逐字一致 | //! 解 `%XX`、整体校验 UTF-8」的等价实现（合法 UTF-8 输入下逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-core/src/tools_manager.rs:15` | A | 逐字一致 | //! - 解压**调用外部命令**（tar/unzip/tar.exe/PowerShell `Expand-Archive`），与 TS 逐字一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-core/src/tools_manager.rs:170` | A | 逐字一致, TS的 | /// 与 TS 的 `getAssetName` 逐字一致；资产名是 GitHub release 的冻结契约，拼错会导致下载失败 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-core/src/tools_manager.rs:359` | A | 与TS一致 | /// `message` 是逐字契约（`ensure_tool` 生成的 info/warning 文本与 TS 一致），本类型不负责 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-core/src/tools_manager.rs:479` | A | 与TS一致 | // 基址是固定的 github.com（与 TS 一致），不是入参 `base`；后面只取 pathname。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-core/src/tools_manager.rs:701` | A | 逐字一致, TS的 | /// 以保证与 TS 的输出逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-core/src/tools_manager.rs:725` | D | 完全一致 | /// 时输出 `spawnSync <command> <io Display>`（可能含 `(os error N)`），不保证与 Node 完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-core/src/shell.rs:215` | A | 与TS一致 | /// 规则与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-core/src/shell.rs:358` | A | 与TS一致 | /// 解析顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-core/src/shell.rs:444` | A | 与TS一致 | /// 只接受 Windows 平台；解析顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-core/src/shell.rs:847` | D | 逐字一致 | // 非 UTF-8 的 PATH 条目必须逐字保留，不能被 lossy 替换成 U+FFFD。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 36 | `crates/kiana-core/src/syntax_highlight.rs:42` | A | 与TS一致 | //! 与 TS 一致：loadAll 之前只认 21 种急切语言及其别名，因此 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-core/src/syntax_highlight.rs:185` | A | TS等价物, TS的 | /// 一次 `highlight` 调用的参数集合。`Default` 等价于 TS 的空对象：无语言（走自动 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-core/src/syntax_highlight.rs:241` | A | 与TS一致, TS: | /// TS: `eagerLanguages` 的 Rust 访问入口（21 种，顺序与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-core/src/syntax_highlight.rs:856` | A | 与TS一致 | /// `getLanguage` 先 `toLowerCase()` 再查名字表与别名表。可见性分两阶段，与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-core/src/frontmatter.rs:180` | A | 逐字一致 | /// 与 TS `error.message` 逐字相同的消息。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
