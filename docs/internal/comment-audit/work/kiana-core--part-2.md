# 注释清理工作单：kiana-core--part-2

- 待处理条目：35
- 涉及文件：21

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

- `crates/kiana-core/src/abort.rs`
- `crates/kiana-core/src/changelog.rs`
- `crates/kiana-core/src/child_process.rs`
- `crates/kiana-core/src/config.rs`
- `crates/kiana-core/src/diagnostics.rs`
- `crates/kiana-core/src/exec.rs`
- `crates/kiana-core/src/html.rs`
- `crates/kiana-core/src/http_dispatcher.rs`
- `crates/kiana-core/src/image_process.rs`
- `crates/kiana-core/src/kiana_manifest.rs`
- `crates/kiana-core/src/management_http.rs`
- `crates/kiana-core/src/output_files.rs`
- `crates/kiana-core/src/paths.rs`
- `crates/kiana-core/src/slash_commands.rs`
- `crates/kiana-core/src/source_info.rs`
- `crates/kiana-core/src/text.rs`
- `crates/kiana-core/src/zip.rs`
- `crates/kiana-core/tests/child_process.rs`
- `crates/kiana-core/tests/paths.rs`
- `crates/kiana-core/tests/slash_commands.rs`
- `crates/kiana-core/tests/syntax_highlight.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-core/src/child_process.rs:24` | D | 逐字一致 | //! `spawn_process` 返回 `Err`、`spawn_process_sync` 放进 `error` 字段（message 逐字相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 2 | `crates/kiana-core/src/child_process.rs:452` | D | 逐字一致 | // 放进 `error` 字段（message 逐字相同）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-core/src/child_process.rs:601` | D | 保真 | /// 非法字节被替换为 U+FFFD；需要逐字节保真（Node 未指定 `encoding` 时的 `Buffer`）时用 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 4 | `crates/kiana-core/src/slash_commands.rs:12` | A | 与TS一致, 完全一致 | //! 表里 24 项的 `name` / `description` / `argumentHint` 与 TS 完全一致（顺序即 UI 顺序）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-core/src/slash_commands.rs:55` | A | 与TS一致 | /// 字段顺序与 TS 一致：`name`、`description`、`source`、`sourceInfo`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-core/src/slash_commands.rs:72` | A | 与TS一致 | /// 字段顺序与 TS 一致：`name`、`description`、`argumentHint`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-core/src/abort.rs:74` | A | 逐字一致 | /// 与 TS `error.message` 逐字相同的消息。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-core/src/abort.rs:106` | A | TS等价物, TS的 | /// operation 对应的任务 panic 时，panic 会在调用方重新抛出（等价于 TS 的 unhandled | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-core/src/changelog.rs:669` | A | 与TS一致 | /// 会格式化出 `fetch failed (ECONNREFUSED)`，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-core/src/changelog.rs:1070` | D | 逐字一致 | /// 消息文本逐字保留 `Deprecation warning: {message}`（chalk 的颜色在 Rust 侧丢失，由 TUI | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-core/src/config.rs:85` | D | 逐字一致 | //! 以保持字符串逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-core/src/config.rs:1171` | D | 逐字一致 | /// 起点目录 `start_dir` 自身也会被检查，之后才逐级上移。`dist/` 特例逐字保留：目录名是 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-core/src/diagnostics.rs:48` | A | 与TS一致 | /// 字段顺序与 TS 一致：`resourceType`、`name`、`winnerPath`、`loserPath`、`winnerSource`、 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-core/src/diagnostics.rs:84` | A | 与TS一致 | /// 字段顺序与 TS 一致：`type`、`message`、`path`、`collision`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-core/src/exec.rs:37` | A | 与TS一致 | //! - 退出后仍持有继承管道的孙进程在**下一次写入**时收到 EPIPE/SIGPIPE（与 TS 一致）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-core/src/exec.rs:194` | A | 与TS一致 | // 时 `killed` 已为真，不会重复发信号，循环会一直等到它自行退出（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-core/src/image_process.rs:278` | A | 与TS一致 | // 顺序与 TS 一致：两步都可能触发，第二步基于第一步更新后的另一维重算。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-core/src/image_process.rs:18` | D | 逐字节一致 | //! `photon-rs` **原生**编译，不引入 wasm 运行时。实测 JPEG 编码逐字节一致，PNG 的像素数据可能 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-core/src/management_http.rs:50` | A | 与TS一致 | //! `formatVersionCheckError` 与 `ensureTool` 的 warning 与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-core/src/management_http.rs:104` | D | 逐字一致 | /// Node `fetch` 传输失败时 `TypeError.message` 的逐字文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-core/src/output_files.rs:24` | A | 与TS一致 | //! （`normalize` 折叠重复 `/`、解析 `.`/`..`），返回的路径文本与 TS 一致 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-core/src/output_files.rs:34` | D | 逐字一致 | //! 分隔符时返回的**文本**与 Node 不同（指向同一文件）。POSIX 分支逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-core/src/paths.rs:1360` | A | 与TS一致 | /// `to` 的绝对形式），因此 `..config/AGENTS.md` 这类名字仍算在 `cwd` 内（与 TS 一致， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-core/src/paths.rs:1464` | D | 逐字一致 | /// 供需要与 Node `join` 逐字一致的其它 crate 使用（例如会话文件命名）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-core/tests/syntax_highlight.rs:764` | A | 与TS一致 | // 未映射的 params 继承 meta 的 formatter，但文本按 span 边界分成三段（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-core/tests/syntax_highlight.rs:778` | A | 逐字一致, TS的 | // 自己的文本时不会产生可见的 params 段（与 TS 的输出逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-core/src/html.rs:141` | A | 与TS一致 | /// 条件与 TS 一致：必须有限、是整数、且 `0 <= cp <= 0x10FFFF`。`-0.0` 会通过 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-core/src/http_dispatcher.rs:377` | D | 逐字一致 | // `gzip,deflate,br`，因此显式设置后请求头逐字一致，解压仍按响应头工作。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-core/src/kiana_manifest.rs:46` | A | 与TS一致 | /// 字段顺序与 TS 一致：`extensions`、`skills`、`prompts`、`themes`。`None` 表示该字段不存在或 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-core/src/source_info.rs:86` | A | 与TS一致 | /// §3.5 决定由本 crate 持有（`kiana-resources` 只再导出）。字段顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-core/src/text.rs:11` | D | 完全一致 | //! `&s[3..]`（U+FEFF 的 UTF-8 编码是 `EF BB BF`）语义完全一致，不需要 UTF-16 适配。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-core/src/zip.rs:20` | D | 逐字节一致 | //! **字节**可能不同，但解压结果一致；CRC 与头字段逐字节一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 33 | `crates/kiana-core/tests/child_process.rs:249` | D | 逐字一致 | // message 必须逐字一致（冻结签名不能抛，放进 Result::Err / error 字段）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 34 | `crates/kiana-core/tests/paths.rs:254` | A | 与TS一致 | // resolvePath 之后还会走 path.resolve，它的 normalize 会把 `//` 折叠成 `/`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-core/tests/slash_commands.rs:5` | D | 逐字一致 | //! 保证名称、描述、参数提示逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
