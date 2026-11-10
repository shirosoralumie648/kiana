# 注释清理工作单：kiana-durable--part-3

- 待处理条目：40
- 涉及文件：35

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

- `crates/kiana-durable/Cargo.toml`
- `crates/kiana-durable/examples/11_tasks.rs`
- `crates/kiana-durable/examples/16_coding_tools.rs`
- `crates/kiana-durable/examples/18_json.rs`
- `crates/kiana-durable/src/codemode/codemode.rs`
- `crates/kiana-durable/src/codemode/describe.rs`
- `crates/kiana-durable/src/env/decode.rs`
- `crates/kiana-durable/src/env/path/posix.rs`
- `crates/kiana-durable/src/env/path/win32.rs`
- `crates/kiana-durable/src/harness/harness.rs`
- `crates/kiana-durable/src/harness/inbox.rs`
- `crates/kiana-durable/src/harness/json.rs`
- `crates/kiana-durable/src/harness/registry.rs`
- `crates/kiana-durable/src/harness/scheduler.rs`
- `crates/kiana-durable/src/harness/telemetry.rs`
- `crates/kiana-durable/src/harness/usage.rs`
- `crates/kiana-durable/src/images/exif.rs`
- `crates/kiana-durable/src/images/node.rs`
- `crates/kiana-durable/src/lib.rs`
- `crates/kiana-durable/src/search.rs`
- `crates/kiana-durable/src/storage/jsonl/node.rs`
- `crates/kiana-durable/src/storage/sqlite/schema.rs`
- `crates/kiana-durable/src/testing/storage_benchmark.rs`
- `crates/kiana-durable/src/tool_search/tool_search.rs`
- `crates/kiana-durable/src/tools/bash.rs`
- `crates/kiana-durable/src/tools/env.rs`
- `crates/kiana-durable/src/tools/image.rs`
- `crates/kiana-durable/src/tools/path_utils.rs`
- `crates/kiana-durable/src/tools/read.rs`
- `crates/kiana-durable/src/truncate.rs`
- `crates/kiana-durable/tests/env_node_spill.rs`
- `crates/kiana-durable/tests/harness_read_only.rs`
- `crates/kiana-durable/tests/memory_storage.rs`
- `crates/kiana-durable/tests/session_tables.rs`
- `crates/kiana-durable/tests/storage_memory.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-durable/src/testing/storage_benchmark.rs:237` | A | 逐字一致 | /// Rust 没有默认参数，因此默认规模由这个单参数入口提供（与 TS 省略 `scale` 的调用逐字对应）， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-durable/src/testing/storage_benchmark.rs:9` | D | 逐字一致 | //! 数据集与基准表集中在这里，名称、数值与顺序逐字对应 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 3 | `crates/kiana-durable/src/tools/read.rs:621` | A | 必须与TS, TS的 | // 续读诊断，必须与 TS 的计数一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-durable/src/tools/read.rs:763` | D | 保真 | /// 保真。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 5 | `crates/kiana-durable/src/truncate.rs:158` | A | 与TS一致 | /// 分支与 TS 一致：小于 1024 输出整数字节（如 `format_size(256) == "256B"`）；小于 1 MiB | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-durable/src/truncate.rs:10` | D | 逐字一致 | //! - 空串与结尾换行的计行规则逐字保留：`""` 是 0 行，`"a\n"` 是 1 行，`"\n"` 是 1 行。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 7 | `crates/kiana-durable/tests/memory_storage.rs:3661` | A | 与TS一致 | /// 显式报错，而不是静默丢弃或伪造值。提交本身（与 TS 一致）成功。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-durable/tests/memory_storage.rs:7` | D | 逐字一致 | //! `#[tokio::test]`，用例标题逐字保留在函数名与 `///` 注释里。合并后若 `testing` 单元提供 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 9 | `crates/kiana-durable/tests/storage_memory.rs:11` | A | TS等价物, TS的 | //! spawn 一个 worker 子进程（等价于 TS 的 `execFile(process.execPath, ...)`），解析报告并打印 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-durable/tests/storage_memory.rs:17` | A | 与TS一致, 完全一致 | //! sqlite `page_count`/`freelist_count`、jsonl 文档/任务文件数）与 TS 完全一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-durable/Cargo.toml:57` | D | 逐字节一致 | # PNG/JPEG 输出与 photon-node 的 wasm 逐字节一致或仅差末位舍入（见 ARCHITECTURE.md §8 第 20 条）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-durable/examples/11_tasks.rs:5` | D | 完全一致 | //! （每次 `commit` 保存 checkpoint、崩溃后从最后一步继续）完全一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 13 | `crates/kiana-durable/examples/16_coding_tools.rs:5` | A | 与TS一致 | //! 原语（同样的 `notes.txt`、同一次 `world` → `durable` 编辑、同一条 `cat`），输出与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-durable/examples/18_json.rs:60` | A | 逐字节一致, TS的 | /// 与 TS 的 `JSON.stringify`（`print()`）逐字节一致：数字走 JS 格式、对象键按 JS 顺序。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-durable/src/codemode/codemode.rs:1870` | D | 强约束 | // journal 里这个调用是 global：目标、名字、参数都必须一致，然后直接投记录值， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-durable/src/codemode/describe.rs:9` | D | 逐字一致 | //! 每个会话逐字相同；它被用作 `codemode` 工具自身的 `description`。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-durable/src/env/decode.rs:1` | A | 完全一致 | //! `env/decode.ts`：与一次性 `new TextDecoder().decode(bytes)` 完全一致的分块解码。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-durable/src/env/path/posix.rs:38` | D | 逐字节一致 | //! - **必须与 Node 逐字节一致**：实现按 UTF-8 字节扫描，而 Node 按 UTF-16 码元扫描；两者等价 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-durable/src/env/path/win32.rs:43` | D | 逐字节一致 | //! - **必须与 Node 逐字节一致**：不是"大致兼容"。语料锁定了尾随 `\`、`.\` 前缀、`CON:` 等 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 20 | `crates/kiana-durable/src/harness/harness.rs:311` | D | 逐字一致 | // 抛出的内容逐字保留（对应 JS 的 throw），其它负载归为一条通用文本。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 21 | `crates/kiana-durable/src/harness/inbox.rs:221` | A | 必须与TS | /// 会改变落盘与 fork 行为，必须与 TS 对齐。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-durable/src/harness/json.rs:12` | D | 逐字一致 | //! 并让 `partial_changes` 的调用点逐字对应。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 23 | `crates/kiana-durable/src/harness/registry.rs:51` | D | 逐字一致 | /// `SECTION_KEY.toString()` 的逐字文本；`validateExtension` 的错误消息用它。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-durable/src/harness/scheduler.rs:14` | D | 逐字一致 | //! "相同 checkpoint 不算进展"、`completing` 持有，全部逐字保留。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 25 | `crates/kiana-durable/src/harness/telemetry.rs:484` | A | 逐字节一致, TS的 | /// 顺序（`kiana-telemetry` 的黄金向量只按语义比较，不要求与 TS 的键序逐字节一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 26 | `crates/kiana-durable/src/harness/usage.rs:26` | A | 保真 | //! 没有 JS 原型，`add_usage_map` 里“定义而不是赋值”的注释是 TS 保真说明，不是 Rust 的 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 27 | `crates/kiana-durable/src/images/exif.rs:4` | D | 逐字一致 | //! APP1、WebP 的 `EXIF` chunk 前缀、值为 0 或 >8 时回落 1）保持逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-durable/src/images/node.rs:16` | A | 与TS一致, 完全一致 | //! 因此 `prepare()` 的“尺寸/格式/是否重缩放/base64 长度在限制内”与 TS 完全一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-durable/src/lib.rs:111` | A | 逐字一致, TS的, 对应TS | // 回调/异步签名逐字对应 TS 的函数类型，拆成别名会掩盖契约。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-durable/src/search.rs:142` | A | 逐字一致 | /// 顺序与 TS 逐字一致：两条 camelCase 正则 → `to_lowercase` → 只保留小写化后的 ASCII | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-durable/src/storage/jsonl/node.rs:57` | A | 与TS一致 | /// 每次调用都用**当前** `process.cwd()` 新建一个 [`NodeExecutionEnv`]，与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-durable/src/storage/sqlite/schema.rs:87` | A | 与TS一致 | /// 每个字段保存**最终**标识符（已带前缀）。字段顺序不是 SQL 依赖，但保持与 TS 一致便于 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-durable/src/tool_search/tool_search.rs:135` | A | 逐字一致, TS的 | /// 保留它是为了与 TS 的 `replaceAll("_", " ")` 逐字对应。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-durable/src/tools/bash.rs:148` | A | 逐字一致, TS的 | // 这样超时文案与 TS 的模板字符串逐字一致，而不是 Rust 默认的浮点格式。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-durable/src/tools/env.rs:10` | A | 逐字一致 | //! `No execution environment is configured` 与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-durable/src/tools/image.rs:228` | A | 与TS一致 | /// 判定顺序（与 TS 一致）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-durable/src/tools/path_utils.rs:44` | A | 逐字一致, TS的 | /// 空白集合与 TS 的字符类逐字对应（U+00A0、U+2000–U+200A、U+202F、U+205F、U+3000）； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-durable/tests/env_node_spill.rs:9` | A | TS等价物, TS的 | //! `write_all`（写期间不读管道，等价于 TS 的 pause）保证，已在 `env_node.rs` 的 spill 用例中 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-durable/tests/harness_read_only.rs:190` | A | TS等价物, TS里 | // Rust 的 `MemoryStorage` 把 close 记在共享状态上；重开这个句柄等价于 TS 里另一个 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-durable/tests/session_tables.rs:1158` | A | TS等价物, TS的, TS: | // TS: `#host.now` 是原始提供者，这里的 panic 等价于 TS 的 throw。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
