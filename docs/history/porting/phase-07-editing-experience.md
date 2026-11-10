# 第 7 周：编辑体验

日期：2026-11-07

## 本周改了什么

### 1. `multi_edit.rs`（新增）：EditError + 原子多 edit 引擎

`crates/kiana-tools/src/multi_edit.rs`：

- `EditError` 六个变体：
  - `NotFound { path, edit_index, total_edits, nearest: Vec<(LineRange, snippet, similarity)> }`
    —— 带最多 3 个最近行区间、片段与相似度；
  - `NotUnique { occurrences, line_ranges, .. }` —— 列出全部出现行号；
  - `AlreadyApplied { line_range, .. }` —— `oldText` 不在但 `newText` 已在；
  - `Overlap { conflicts_with, line_range, .. }`；
  - `EmptyReplacement` —— `oldText` 为空（或模糊归一化后为空）；
  - `FileChanged { path, reason }` —— read 之后文件被外部修改（由 `edit.rs` 映射）。
- 每条错误都包含 path / `edits[i]` / 真实 1-based 行号 / 是否已应用，并以
  `Do not resend the same failed edit; ...` 结尾；`edit.rs` 的多 edit 失败还会追加
  `Per-edit status (the file was not written)` 段，逐条列出 matched/failed/skipped。
- `plan_file_edits(path, content, edits) -> FileEditOutcome`：
  1. 逐个 edit 定位（Exact → QuoteNormalized → Fuzzy → NotFound），**全部校验后才构造
     `new_content`**；任一失败返回 `Rejected`，类型层面就没有可写内容；
  2. 检查重叠（原始下标）并按起始字节逆序替换；
  3. 成功返回 `FileEditPlan`：`new_content`、每 edit 的 `hunk` / `snippet`（±4 行、
     行号 + `>` 标记变更行）、整文件 diff/patch、整体 `strategy`。
- 匹配策略：
  - `Exact`：LF 归一化后的精确子串；
  - `QuoteNormalized`：仅 exact 失败后，弯引号→ASCII，并保留原文偏移映射；
  - `Fuzzy`：仅前两者失败后，按行窗口比较（NFKC + 行尾空白 + 智能标点归一化），
    加权 Levenshtein（空白差异权重 0.1），阈值 0.1、最短 10 字符、只接受唯一非重叠
    候选；替换时对齐首行缩进并保留末尾换行，未触及的行逐字节保留；
  - `Regex`：保留枚举值，当前不自动选用（避免静默切换）。
- `EditStrategyEvent { edit_index, strategy, line_range }`：供 metadata/telemetry。

### 2. `edit.rs`：接入新引擎

- 删除对 `edit_diff::apply_edits_to_normalized_content` 的执行路径依赖；保留其
  diff/patch 生成与 LF/BOM 工具函数。
- BOM/行尾处理、read-before-write 门禁、截断读覆盖检查、`file_mutation_queue` 串行化
  全部保留；覆盖检查改用校验阶段得到的真实行区间。
- 失败文案改为 `EditError`；`FileChanged` 保留 `FileChangedSinceRead` /
  `FILE_CHANGED_SINCE_READ` 两个稳定标记（兼容 week 2 测试）。
- 成功 details 在原有 `diff` / `patch` / `firstChangedLine` 之后追加：
  `strategy`（线格式 `exact`/`quoteNormalized`/`fuzzy`/`regex`）、`strategyVariant`
  （Rust 变体名 `Exact`/`QuoteNormalized`/`Fuzzy`/`Regex`）、`editStrategies`、
  `strategyEvents`、`hunks`（每 edit 的 unified hunk）、
  `edits`（status/strategy/lineRange/hunk/snippet）、`changedLineCount`。
  旧渲染器只按名读取已知字段，额外键前向兼容。

### 3. `streaming_parser.rs`（新增）：apply_patch 增量解析

- `StreamingPatchParser::push_delta` 接受任意字符边界，只为完整行产生
  `PreviewEvent`（StartedPatch / HunkStarted / MoveTo / Line / EndOfFile / Finished）；
  `finish` 处理结尾无换行并强制 `*** End Patch`。
- `parse_patch` / `parse_patch_with_mode`（Strict / Lenient，默认 Lenient）：
  - `*** Begin/End Patch`、Add/Delete/Update File、`*** Move to:`、`@@ context`、
    `*** End of File`；
  - `@@`/`*** End of File` 的空 pattern 是合法 no-op（EOF 插入）；
  - Lenient 额外接受 `<<EOF` / `<<'EOF'` / `<<"EOF"` heredoc 包装；
  - 解析期拒绝空 update hunk、EOF 之后的非法行、End Patch 之后的内容。

### 4. `apply_patch.rs`（新增）：apply_patch 工具

- `ApplyPatchToolOptions { operations, execution_policy, read_state, parse_mode }`；
  `ApplyPatchOperations` 可注入（测试用内存 FS 验证回滚）。
- 执行顺序：
  1. 解析补丁（无 IO）；空补丁成功 no-op；
  2. 解析所有目标路径（cwd + `ExecutionPolicy::resolve_write_target("apply_patch")`）；
  3. 路径排序去重后**按固定顺序**嵌套获取 `file_mutation_queue` 锁（避免死锁）；
  4. 锁内读取当前内容，把 hunk 应用在内存状态上（同一文件多个 hunk 看到前一个结果），
     全部成功后才生成 `new_content`；任一失败直接返回，**所有文件零写入**；
  5. 生成逐文件 unified diff 与汇总 patch；
  6. 先写/改、再删除；中途失败按首次触碰顺序反向回滚（写回原文/删除新建文件）。
- `SourceFile` 逐行保留行尾（LF/CRLF/CR）；未改动行逐字节保留，新增行使用文件首个
  行尾风格；BOM 随首行文本保留。
- `seek_sequence`：exact → `trim_end` → `trim` → Unicode 标点归一化（trim 后），
  `eof=true` 时从文件末尾向前锚定；空 pattern 返回 `start`；pattern 过长返回 `None`
  不 panic。
- `tools.apply_patch.models` 路由：
  `ApplyPatchSettings::from_config` 支持
  `{tools:{apply_patch:{models}}}`、`{tools:{applyPatch:{models}}}`、顶层同名字段与
  直接数组；`model_matches_pattern` 支持 `*` 通配、大小写不敏感。
  `index.rs` 提供 `create_coding_tool_definitions_for_model` /
  `create_all_tool_definitions_for_model` / `create_*_tools_for_model`，在 edit 的位置
  替换成 apply_patch（不会同时暴露两者）。

### 5. `index.rs`：模块挂载与模型路由

- 因为 `lib.rs`（跨周共享）不在本单元所有权内，用
  `#[path = "apply_patch.rs"] pub mod apply_patch;` 等把三个新模块挂在 `index.rs` 上；
  `lib.rs` 的 `pub use index::*;` 会把模块名一并再导出（已验证），因此
  `kiana_tools::multi_edit` / `kiana_tools::apply_patch` / `kiana_tools::streaming_parser`
  可用。
- 新增模型路由构造函数（见上）；`ToolName` 保持 8 个内置工具的封闭枚举不变，
  apply_patch 作为按模型替换的编辑工具，不进入 golden 定义。

### 6. 测试与 fixture

- `tests/fixtures/apply_patch.golden.json`：12 个用例，覆盖
  add、update（多 hunk）、delete、move、多文件、多 hunk、CRLF、Unicode 标点、EOF、
  重复 context、空 patch、lenient heredoc、多文件 add、delete-only。初始文件与期望
  文件都按**字节**比较。
- `tests/apply_patch.rs`：fixture runner + stale context 零写入 + diff preview 与最终
  写入一致 + 写失败回滚 + delete 不存在 + strict/lenient + prepareArguments +
  schema + 模型路由。
- `tests/edit.rs`：新增 NotFound 3 个最近行区间、NotUnique 全部行号、AlreadyApplied、
  逐 edit 状态、fuzzy/exact/quoteNormalized 策略 metadata、hunk/±4 行片段、
  FileChangedSinceRead；旧 TS 文案断言已按新契约更新（`edit_diff.rs` 的黄金测试不动）。

## 参考项目 → 借鉴的设计点 → Kiana 落点

| 参考项目文件 | 借鉴的设计点 | Kiana 落在哪个文件/函数 |
| --- | --- | --- |
| `codex/codex-rs/apply-patch/src/parser.rs` | Begin/Add/Delete/Update/Move to/@@/End of File 语法；strict/lenient 边界与 heredoc；空 update hunk 校验 | `crates/kiana-tools/src/streaming_parser.rs`：`parse_patch_with_mode` / `StreamingPatchParser::process_line` |
| `codex/codex-rs/apply-patch/src/seek_sequence.rs` | exact → trim_end → trim → Unicode 标点归一化；EOF 锚定；空 pattern no-op；pattern 过长返回 None | `crates/kiana-tools/src/apply_patch.rs`：`seek_sequence` / `normalize_punctuation` |
| `codex/codex-rs/apply-patch/src/file_update.rs` | 先读文件、按 chunk 计算 replacements、上下文行只保留原字节、生成 new_contents；末尾空行重试 | `crates/kiana-tools/src/apply_patch.rs`：`compute_replacements` / `apply_chunks` / `SourceFile` |
| `codex/codex-rs/apply-patch/src/text_file.rs` | 逐行保留行尾与 preferred ending；新增行继承风格；更新后补齐末尾换行 | `crates/kiana-tools/src/apply_patch.rs`：`SourceFile::{parse, apply_replacements, into_contents}` |
| `codex/codex-rs/apply-patch/src/streaming_parser.rs` | 行缓冲增量解析、每种 hunk 的状态机、`finish` 处理无换行末尾 | `crates/kiana-tools/src/streaming_parser.rs`：`StreamingPatchParser` / `PreviewEvent` |
| `codex/codex-rs/apply-patch/src/lib.rs` | Add/Delete/Update/Move 的落盘语义、Move 先写目标再删源、汇总输出 `A/M/D` | `crates/kiana-tools/src/apply_patch.rs`：`ApplyPatchExecute::apply`（额外做了全部验证后写 + 回滚） |
| `ClaudeCode/src/tools/FileEditTool/FileEditTool.ts` | read 快照比对（内容哈希兜底）、`findActualString`、多处匹配必须唯一、先 patch 再写 | `crates/kiana-tools/src/edit.rs` + `multi_edit.rs`：`check_edit_gate` / `find_matches` / `plan_file_edits` |
| `ClaudeCode/src/tools/FileEditTool/utils.ts` | `normalizeQuotes` 只替换弯引号；`preserveQuoteStyle` 的缩进/引号保持思路 | `multi_edit.rs`：`quote_normalize` / `QuoteNormalized::map_range`（保留原文偏移；未做新文本弯引号回写） |
| `gemini-cli/packages/core/src/tools/edit.ts` | `EditStrategyEvent`；fuzzy 仅在 exact 失败后；行窗口 + 加权 Levenshtein（阈值 0.1、空白权重 0.1、最短 10 字符）；非重叠候选 | `multi_edit.rs`：`EditStrategyEvent` / `fuzzy_matches`；`edit.rs` 的 details `strategyEvents` |
| `aider/aider/coders/editblock_coder.py` | `SequenceMatcher` 相似度反馈、最近失败块提示 | `multi_edit.rs`：`nearest_matches` / `text_similarity` / `EditError::NotFound` 文案 |

## 真实测试命令与输出摘要

```text
$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo check -p kiana-tools
cargo check: 0 errors (9 crates)

$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-tools --lib
cargo test: 64 passed (1 suite)

$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-tools --test apply_patch
cargo test: 12 passed (1 suite)

$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-tools --test edit
cargo test: 54 passed (1 suite)

$ CARGO_TARGET_DIR=/data/cargo-target-sandbox timeout 900 cargo test -p kiana-tools
cargo test: 429 passed (27 suites, 6.30s)
```

> 并发工作区说明：中途另一个并行工作流先改了
> `crates/kiana-tools/src/powershell.rs`（新增 `PowerShellToolOptions::sandbox`）、
> 随后同步了 `tests/powershell.rs`。期间包级测试一度以 E0063 失败（与本单元无关的
> 文件），该工作流补齐后最终运行恢复为 **429 passed / 27 suites**；排除期间的
> 25 个 suite / 422 tests 也全绿（`passed=422 failed=0`）。本单元新增/修改的
> `--lib` / `--test edit` / `--test apply_patch` / `--test index` / `--test edit_diff` /
> `--test read_before_write` 始终全部通过。

`cargo fmt -p kiana-tools` 已执行。

## 未完成项 / partial

- **Regex 策略**：`EditStrategy::Regex` 只是一个保留值，当前没有自动正则匹配（计划没有
  要求新的正则语法；避免模型误触）；需要时再加显式开关。
- **跨文件事务**：apply_patch 在写盘前完成全部校验，并对运行期写失败做反向回滚，但
  文件系统没有真正事务；进程在写盘中途崩溃仍可能留下部分写入。风险与参考实现相同。
- **TUI 渲染器**：apply_patch 的 `render_call`/`render_result` 为 `None`（使用默认
  外壳）；`StreamingPatchParser` 已提供增量 `PreviewEvent`，接线在 handoff 里说明。
- **配置 schema**：`tools.apply_patch.models` 的读取函数已完成，但 `kiana-config`
  的 `Settings` 还没有 `tools` 字段（不在本单元所有权内）；session 接线见 handoff。
- **`multi_edit` 不是独立模型工具**：计划只要求“多 edit 先全部校验、按文件原子应用”，
  因此做成 `edit` 的引擎模块；不新增工具名，避免改变 8 个内置工具的 golden 契约。
- `edit` 成功文案保持 `Successfully replaced N block(s) in <path>.`（既有测试与 UI 依赖）。
- `edit_diff.rs` 的旧 `apply_edits_to_normalized_content` 仍保留（预览/黄金测试使用），
  但执行路径已切换到 `multi_edit.rs`。

## 风险

- Fuzzy 匹配是行窗口级：`oldText` 必须覆盖完整的行（行尾空白允许差异），不做行内
  片段的模糊替换；短于 10 个字符的 `oldText` 不做 fuzzy（沿用 gemini-cli 的防误命中
  阈值），必须 exact / quoteNormalized 命中，否则返回 NotFound + 最近片段。
- `nearest_matches` 对超大文件按 stride 抽样（最多约 2 万个窗口），错误提示的候选可能
  不是全局最优，但一定落在文件内且不会阻塞。
- `LineRange` 文案单行显示为 `line N`、多行 `lines A-B`；测试已按此契约断言。
