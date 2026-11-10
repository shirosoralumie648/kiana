# ca-tools-compaction-export 单元移植规格（TypeScript -> Rust）

本文件是 kiana coding-agent 中 **内置工具（core/tools）**、**上下文压缩（core/compaction）**、
**HTML 导出（core/export-html）** 三个紧密相关子系统的完整移植规格。源仓库
`/home/ctyun/code/kiana_rev/kiana` 是**只读参考**；Rust 实现在 `/home/ctyun/code/kiana_rev/kiana`。

- 源包：`/home/ctyun/code/kiana_rev/kiana/packages/coding-agent`（`@earendil-works/kiana-coding-agent`，
  ESM，`engines.node >= 22.19.0`），本单元全部位于 `src/core/` 下。
- 本单元文件范围（**全部逐行读过**，非摘录）：

  `src/core/tools/`
  - `bash.ts` 442 行、`edit-diff.ts` 556、`edit.ts` 220、`file-mutation-queue.ts` 61、
    `find.ts` 318、`grep.ts` 325、`index.ts` 224、`ls.ts` 175、`output-accumulator.ts` 257、
    `path-utils.ts` 118、`powershell.ts` 67、`read.ts` 224、`render-utils.ts` 185、
    `tool-definition-wrapper.ts` 58、`truncate.ts` 314、`write.ts` 97
  - `src/core/tools/renderers/`：`bash.ts` 109、`edit.ts` 252、`find.ts` 77、`grep.ts` 82、
    `index.ts` 63、`ls.ts` 70、`read.ts` 173、`write.ts` 181

  `src/core/compaction/`
  - `branch-summarization.ts` 380、`compaction.ts` 1104、`index.ts` 7、`utils.ts` 163

  `src/core/export-html/`
  - `index.ts` 318、`ansi-to-html.ts` 258、`tool-renderer.ts` 174、
    `template.html` 56（2271 字节）、`template.css` 1085（23131 字节）、`template.js` 2044（86652 字节）、
    `vendor/marked.min.js`（44640 字节）、`vendor/marked-cjk-friendly.min.js`（4754 字节）、
    `vendor/highlight.min.js`（121727 字节）

  文档：`packages/coding-agent/docs/compaction.md`（463 行）、`docs/shell-aliases.md`（93 行）。

- **不在本单元范围、但本单元直接调用**（接口契约见第 5 节，实现由对应单元负责）：
  `core/session-manager.ts`、`core/messages.ts`、`core/extensions/**`、`core/agent-session.ts`、
  `core/settings-manager.ts`、`core/usage-totals.ts`、`core/bash-executor.ts`、`core/system-prompt.ts`、
  `modes/interactive/**`（theme、组件）、`utils/**`（paths/shell/output-files/text/ansi/child-process/
  image-process/mime/tools-manager/management-http/kiana-user-agent）、`config.ts`、
  `packages/agent`（`@earendil-works/kiana-agent-core`）、`packages/ai`（`@earendil-works/kiana-ai`）、
  `packages/tui`（`@earendil-works/kiana-tui`）。

- 本文件同时是：(a) Rust 移植的**逐条可测验收清单**；(b) Rust 侧**设计说明**。
  凡标注「逐字」的字符串，Rust 侧必须逐字节相同。

## 目录

1. 概览
2. 逐文件规格（导出符号 + 行为）
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / 子进程 / 协议）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射
7. 测试清单
8. Rust 模块布局与最难复现的部分
9. Audit additions（审计补遗：新增/修正清单）

---

## 1. 概览

### 1.1 目的

本单元提供 kiana 的「手与记忆」：

1. **内置工具**（`core/tools`）：模型可调用的 8 个工具 `read` / `bash` / `powershell` / `edit` /
   `write` / `grep` / `find` / `ls`。每个工具 = **参数 schema（TypeBox）+ 系统提示贡献 +
   执行实现 + 输出 schema（供程序化调用者）+ TUI 渲染器**。执行实现通过可替换的
   `*Operations` 接口解耦（远程 SSH、测试 stub、扩展拦截）。输出统一走
   `truncate.ts` 的行/字节双限制截断，bash 额外走流式 `OutputAccumulator` + 临时文件。
2. **上下文压缩**（`core/compaction`）：纯函数实现的「选切点 → 抽取消息 → 调 LLM 生成结构化摘要 →
   产出 `CompactionResult`」，以及树导航时的分支摘要。会话读写由 `SessionManager` 负责，
   本单元只做计算与 LLM 调用。
3. **HTML 导出**（`core/export-html`）：把会话树 + 条目渲染成**单文件、自包含、离线可用**的 HTML
   （内联 CSS、内联 vendored marked/highlight.js、base64 内嵌会话 JSON、纯前端 JS 查看器：
   树侧栏、过滤、搜索、折叠、深链、JSONL 下载）。扩展工具的输出通过「调用 TUI 渲染器 →
   ANSI → 内联 CSS HTML」预渲染。

### 1.2 在 monorepo 中的位置

- 上游依赖：`@earendil-works/kiana-agent-core`（`AgentTool` 类型、`wrapToolDefinition` 的对端）、
  `@earendil-works/kiana-ai`（消息与 usage 类型、`contentText`、`normalizeContext`、`retryAssistantCall`、
  `uuidv7`、`completeSimple`）、`@earendil-works/kiana-tui`（`Text`、`Container`、`Box`、`Spacer`、
  `Component`、`getCapabilities`、`hyperlink`、`imageFallback`、`getImageDimensions`）。
- 下游被调用：`core/agent-session.ts`（创建工具、调用 `prepareCompaction`/`compact`/
  `generateBranchSummary`/`saveCompaction`、`exportToHtml`）、
  `core/extensions/**`（`resolveToolRenderers` 供 HTML 导出）、
  `modes/interactive/components/tool-execution.ts`（调用 `withBuiltInRenderers`）、
  `main.ts`（`--export` CLI）、`core/sdk.ts`、codemode（`structuredContent` 消费方）。
- 本单元**不**决定：何时压缩（`AgentSession` 决定并传入 settings）、会话文件格式（`SessionManager`）、
  主题色值（`theme.ts`）、系统提示拼装（`system-prompt.ts`）。

### 1.3 关键不变量（Rust 实现必须逐条成立）

1. **双限制截断，先命中者胜**：`DEFAULT_MAX_LINES = 2000`、`DEFAULT_MAX_BYTES = 50*1024`。
   `truncateHead` 永不返回半行（首行超字节限制时返回空内容 + `firstLineExceedsLimit`）；
   `truncateTail` 允许「最后一行超限 → 从行尾截取部分」并置 `lastLinePartial`。
2. **截断后的字节数是重新计算的**：`outputBytes` = 拼接后内容的 `Buffer.byteLength`，
   而不是累加值（两者在首行之外可能差 1，见 2.1 的 `+1 newline` 规则）。
3. **工具输出文本里嵌入的提示语是用户/模型双重可见的**（`[Showing lines 3-2002 of 5000. ...]` 等），
   必须逐字一致，因为它们既进入模型上下文，也进入会话文件与 HTML 导出。
4. **所有路径解析都经过 `resolveToCwd`**（`~` 展开、Unicode 空格归一、`@` 前缀剥离、绝对路径原样），
   只有 `read` 额外尝试 macOS 变体（AM/PM 窄空格、NFD、弯引号、NFD+弯引号）。
5. **编辑不改动未触及的行**：多编辑一律对**原始内容**匹配（不是增量），
   且一旦用到模糊匹配，就以「行级覆盖」方式把改动贴回原文，未触及行保留原始字节
   （因此 CRLF / BOM / 行尾空白都能保留）。
6. **`edit` 与 `write` 对同一真实路径串行**（`withFileMutationQueue`，key = `realpath`），
   且**中途中止不会提前释放队列**：只在每个 await 之后检查 `signal.aborted`。
7. **bash 输出约束**：显示用快照按 tail 截断（保尾部），滚动缓冲上限 `maxBytes*2`；
   一旦超限即落临时文件（`0600`、`wx`、`${tmpdir}/${prefix}-<16 hex>.log`）；
   程序化调用者另有 `structuredContent`，其 `output` 上限 1 MiB（超限时保头尾各半 + 中间省略标记）。
8. **bash 流式更新节流 100ms**：首个 `onUpdate` 是空 content + `details: undefined`；
   后续快照 `details = { truncation?: <仅在 truncated 时>, fullOutputPath? }`。
9. **压缩切点永不落在 toolResult 上**；`isSplitTurn` 表示切点不是 turn 起点；
   切点前移只发生在「recovery omission suffix」这一种情形（详见 2.26）。
10. **压缩/分支摘要的 LLM 请求一律关闭 prompt cache 写入**：`cacheRetention: "none"`，
    `sessionId` 复用调用方提供的路由 id，否则新建 `uuidv7()`。
11. **摘要输出必须完整**：`stopReason === "length"` 一律视为失败（不得把半截摘要写进会话）；
   摘要里出现 toolCall 一律失败（错误文本逐字）。
12. **fileOps 是累积的**：默认压缩从「上一个 kiana 生成的压缩」的 `details` 继承，
    分支摘要从被摘要范围内的 kiana 生成 `branch_summary` 的 `details` 继承；
    `fromHook === true` 的条目不继承（扩展自管格式）。
13. **HTML 导出是纯字符串替换**：模板/JS/CSS 从磁盘读入后按固定占位符替换；
    会话数据 `JSON.stringify` 后 base64。占位符替换是**单次**（`String.prototype.replace` 只替换首个匹配）。
14. **HTML 中的 vendored JS 必须原样嵌入**：markdown 渲染（marked + CJK 友好插件）与语法高亮
    （highlight.js）都在浏览器里跑，Rust 不重写它们，必须**逐字节**打包同样的文件。
15. **JSON 键顺序**：`SessionData`、`CompactionEntry`、`BranchSummaryEntry`、`Usage` 等写入磁盘/
    base64 的对象依赖对象字面量构造顺序；Rust 侧 `serde_json` 必须开 `preserve_order`
    （或手写有序 map），否则导出 HTML 与 JSONL 字节不同。

### 1.4 术语

- **tool definition**：`ToolDefinition<TSchema, TDetails, TState>`，含 `name/label/description/
  promptSnippet/promptGuidelines/parameters/outputSchema/constrainedSampling/prepareArguments/
  renderShell/execute/renderCall/renderResult`。
- **AgentTool**：运行时形状（`@earendil-works/kiana-agent-core`），无 prompt 元数据与渲染器。
- **details**：工具结果里给 UI/后续逻辑用的结构化附加数据（不进模型上下文）。
- **structuredContent**：给程序化调用者（codemode 脚本）的结果对象，受 `outputSchema` 约束。
- **projection**：`SessionManager.buildSessionProjection()` 产出的「模型可见消息 + 来源条目」列表。
- **cut point**：压缩时「第一条保留的条目」索引。
- **turn / user-message span**：从一条 user 类消息开始到下一个 user 类消息之前。
- **split turn**：压缩切点落在某个 span 内部（不是 turn 起点）。
- **recovery omission suffix**：因 overflow/length 恢复而写入的 `context_edit: null` 省略编辑
  所构成的「对上下文不可见」的尾部。

---

## 2. 逐文件规格

### 2.1 `src/core/tools/truncate.ts`

#### 2.1.1 常量（逐字）

```ts
export const DEFAULT_MAX_LINES = 2000;
export const DEFAULT_MAX_BYTES = 50 * 1024;      // 即 51200
export const GREP_MAX_LINE_LENGTH = 500;          // 每个 grep 匹配行的最大字符数
```

#### 2.1.2 类型

```ts
export interface TruncationResult {
  content: string;
  truncated: boolean;
  truncatedBy: "lines" | "bytes" | null;
  totalLines: number;
  totalBytes: number;
  outputLines: number;
  outputBytes: number;
  lastLinePartial: boolean;
  firstLineExceedsLimit: boolean;
  maxLines: number;
  maxBytes: number;
}
export interface TruncationOptions { maxLines?: number; maxBytes?: number; }
export interface MiddleTruncationResult {
  content: string; truncated: boolean; removedChars: number;
  totalBytes: number; totalLines: number;
}
```

字段顺序即 `JSON.stringify(details)` 的键顺序（`BashToolDetails.truncation`、`ReadToolDetails.truncation`、
`GrepToolDetails.truncation`、`FindToolDetails.truncation`、`LsToolDetails.truncation` 都会进会话文件），
Rust 结构体字段声明顺序必须与上表一致。

#### 2.1.3 `splitLinesForCounting(content) -> string[]`（内部）

- `content.length === 0` → `[]`。
- `lines = content.split("\n")`；若 `content.endsWith("\n")` 则 `lines.pop()`（丢掉末尾空串）。
- 语义：`"a\n"` → `["a"]`（1 行）；`"a\n\n"` → `["a",""]`（2 行）；`""` → `[]`（0 行）。
- 注意 JS 的 `.length` 是 UTF-16 码元数；Rust 用 `encode_utf16().count()` 或先转 `Vec<u16>`
  再实现，不要直接用 `str::len()` 判空以外的用途（此处只用于判空，`!content.is_empty()` 即可）。

#### 2.1.4 `formatSize(bytes: number) -> string`

- `bytes < 1024` → `` `${bytes}B` ``（整数原样，无小数）。
- `bytes < 1024*1024` → `` `${(bytes/1024).toFixed(1)}KB` ``。
- 否则 → `` `${(bytes/(1024*1024)).toFixed(1)}MB` ``。
- `toFixed(1)` 语义：**始终保留 1 位小数**；舍入规则按 ECMAScript 规范是「取精确十进制值的
  最近邻，**恰好在中点时取较大的 n**（round half away from zero on the exact decimal value）」，
  不是银行家舍入，也不是二进制半值判断。
  （`51200` → `"50.0KB"`，`1536` → `"1.5KB"`，`1023` → `"1023B"`，`1048576` → `"1.0MB"`。）
- **Rust 不能用 `format!("{:.1}", x)` 直接等价**：Rust 的 `{:.1}` 对 f64 采用「精确值最近舍入 +
  中点取偶」（round half to even），与 JS 在中点处相反。已实测的字节数反例（`bytes/1024` 恰为二进制
  可精确表示的中点；四列依次为 bytes / x / JS `toFixed(1)` / Rust `{:.1}`）：
  `256 → 0.25 → "0.3" → "0.2"`；`768 → 0.75 → "0.8" → "0.8"`（0.75 精确，取偶 → 0.8，恰好相同）；
  `1280 → 1.25 → "1.3" → "1.2"`；`2304 → 2.25 → "2.3" → "2.2"`。
  Rust 必须自己实现：用 `format!("{:.20}", x)`（或 `ryu`）取精确十进制展开，按 JS 规则在
  第 2 位小数处做「≥5 进位」，或直接以整数运算实现（`bytes*10/1024` 的商与余数判断），
  并对上述边界加单测。
- 输入为非负整数（字节数），无 NaN 路径。`1024*1024 - 1` → `"1024.0KB"`（仍是 KB 分支）。

#### 2.1.5 `truncateHead(content, options = {}) -> TruncationResult`

`maxLines = options.maxLines ?? 2000`，`maxBytes = options.maxBytes ?? 51200`。

1. `totalBytes = Buffer.byteLength(content,"utf-8")`；`lines = splitLinesForCounting(content)`；
   `totalLines = lines.length`。
2. **无需截断**（`totalLines <= maxLines && totalBytes <= maxBytes`）：
   ```json
   {content, truncated:false, truncatedBy:null, totalLines, totalBytes,
    outputLines: totalLines, outputBytes: totalBytes,
    lastLinePartial:false, firstLineExceedsLimit:false, maxLines, maxBytes}
   ```
3. **首行超字节限制**（`Buffer.byteLength(lines[0]) > maxBytes`）：返回
   `content:""`、`truncated:true`、`truncatedBy:"bytes"`、`outputLines:0`、`outputBytes:0`、
   `lastLinePartial:false`、`firstLineExceedsLimit:true`。**注意 `lines[0]` 在 lines 为空时是
   `undefined`**，但该分支只会在 `totalBytes > maxBytes`（进而 lines 非空）时可达。
4. 逐行累积：`for i in 0..lines.len() && i < maxLines`：
   `lineBytes = byteLength(lines[i]) + (i > 0 ? 1 : 0)`（除第一行外每行算 1 个换行）；
   若 `outputBytesCount + lineBytes > maxBytes` → `truncatedBy = "bytes"` 并 break；
   否则 push 该行、`outputBytesCount += lineBytes`。
5. 若 `outputLinesArr.length >= maxLines && outputBytesCount <= maxBytes` → `truncatedBy = "lines"`。
6. `outputContent = outputLinesArr.join("\n")`；`finalOutputBytes = byteLength(outputContent)`。
   返回 `truncated:true`、`outputLines: outputLinesArr.length`、`outputBytes: finalOutputBytes`、
   `lastLinePartial:false`、`firstLineExceedsLimit:false`。

#### 2.1.6 `truncateTail(content, options = {}) -> TruncationResult`

1、2 步同上（无截断与 head 相同）。3.

- 从尾往前：`for i = lines.len()-1 down to 0 && outputLinesArr.len() < maxLines`：
  `lineBytes = byteLength(lines[i]) + (outputLinesArr.len() > 0 ? 1 : 0)`；
  若 `outputBytesCount + lineBytes > maxBytes`：
  - `truncatedBy = "bytes"`；
  - **边界情况**：若尚未加入任何行（`outputLinesArr.isEmpty()`），
    `truncatedLine = truncateStringToBytesFromEnd(lines[i], maxBytes)`；unshift；重算
    `outputBytesCount`；`lastLinePartial = true`；
  - break。
  否则 unshift 该行并累加。
- 行数限制判定同上；`outputContent = join("\n")`；`outputBytes` 重算；`firstLineExceedsLimit:false`。

#### 2.1.7 `truncateStringToBytesFromEnd(str, maxBytes)`（内部，不导出）

- `buf = Buffer.from(str,"utf-8")`；若 `buf.length <= maxBytes` 原样返回。
- `start = buf.length - maxBytes`；while `start < buf.length && (buf[start] & 0xc0) === 0x80` 时 `start++`
  （跳过续字节，保证落在字符起始处）；返回 `buf.slice(start).toString("utf-8")`。
- 极端：若整串都是续字节（不可能，首字节必非续），安全。

#### 2.1.8 `truncateLine(line, maxChars = GREP_MAX_LINE_LENGTH) -> {text, wasTruncated}`

- `line.length <= maxChars` → `{text: line, wasTruncated:false}`（JS 的 `length` 是 UTF-16 码元）。
- 否则 `{text: `${line.slice(0,maxChars)}... [truncated]`, wasTruncated:true}`。
- **注意**：`maxChars` 按 **UTF-16 码元** 计，`line.slice` 也按码元切，可能切在代理对中间
  （JS 会产生孤立代理；Rust `char` 无法表达）。Rust 实现建议：把 `line` 转为 `Vec<u16>`，
  按码元切片，再 `String::from_utf16_lossy`（孤立代理 → U+FFFD）。这对含 emoji 的长行是
  唯一能保持「字符预算相同」的做法；若用 `chars().take(500)` 则预算不同（emoji 占 2 码元）。
  这是**本单元最容易被忽略的差异之一**，见第 8 节。

#### 2.1.9 `truncateMiddle(content, maxBytes) -> MiddleTruncationResult`

- `buf = Buffer.from(content,"utf-8")`；`totalLines = splitLinesForCounting(content).length`。
- `buf.length <= maxBytes` → `{content, truncated:false, removedChars:0, totalBytes: buf.length, totalLines}`。
- `isBoundary(i) = i >= buf.length || (buf[i] & 0xc0) !== 0x80`。
- `headEnd = floor(maxBytes/2)`；while `headEnd > 0 && !isBoundary(headEnd)` → `headEnd--`。
- `tailStart = buf.length - (maxBytes - floor(maxBytes/2))`；while `tailStart < buf.length && !isBoundary(tailStart)` → `tailStart++`。
- `head = buf[0..headEnd]` 的 UTF-8 文本；`tail = buf[tailStart..]`；
  `removedChars = Array.from(buf[headEnd..tailStart] 的 UTF-8 文本).length`（**按 code point 计数**，不是码元、不是字节）。
- 返回 `content = `${head}…${removedChars} chars truncated…${tail}``，`truncated:true`。
- 分隔符是 U+2026 HORIZONTAL ELLIPSIS（`…`），前后无空格；`removedChars` 与 " chars truncated" 之间一个空格。
- **消费者（源码实测）**：唯一调用点是 `extensions/mcp/tools.ts:185` 的 `limitMcpContent`
  （`MCP_OUTPUT_MAX_BYTES = 20 * 1024`）；它把结果拼成
  `` `Warning: truncated output (original token count: ${ceil(totalBytes/4)})\nTotal output lines: ${totalLines}\n\n${truncation.content}\n\n[Full output: ${path} (read it with offset/limit)]` ``
  （保存失败时改为 `[Could not save the full output: ${message}]`），并把 image 块追加到文本块**之后**。
  codemode 的 `truncateOutput`（`extensions/codemode/execute.ts`）用的是**另一套**实现，
  标记是 `…${ceil(removed / CHARS_PER_TOKEN)} tokens truncated…`（**tokens**，不是 chars），不要混淆。
  本单元导出 `truncateMiddle` 但两个消费者都在别单元；Rust 侧仍需实现且行为一致
  （`mcp-extension.test.ts` 的 "cuts the middle of model-facing text over 20KB"、
  "cuts multi-byte text only at character boundaries" 是直接验收）。

### 2.2 `src/core/tools/path-utils.ts`

- `const NARROW_NO_BREAK_SPACE = "\u202F";`（U+202F NARROW NO-BREAK SPACE）
- `tryMacOSScreenshotPath(p)`：`p.replace(/ (AM|PM)\./gi, "\u202F$1.")`——把 `" 12.30.45 PM.png"`
  里的半角空格换成窄不换行空格；`gi` 大小写不敏感（`am`/`pm` 也命中）。
- `tryNFDVariant(p)`：`p.normalize("NFD")`。
- `tryCurlyQuoteVariant(p)`：`p.replace(/'/g, "\u2019")`（U+2019）（所有直引号 → 右单弯引号）。
- `fileExists(p)`：`accessSync(p, F_OK)` try/catch → boolean。
- `export async function pathExists(p)`：`await access(p, F_OK)` → boolean（吞掉所有错误）。
- `export function expandPath(filePath)`：`normalizePath(filePath, {normalizeUnicodeSpaces:true, stripAtPrefix:true})`。
- `export function resolveToCwd(filePath, cwd)`：`resolvePath(filePath, cwd, {normalizeUnicodeSpaces:true, stripAtPrefix:true})`。
  语义（`utils/paths.ts`，`normalizePath(input, options)`，选项 `{trim, expandTilde, homeDir,
  stripAtPrefix, normalizeUnicodeSpaces}`）：
  1. `options.trim` 为真时先 `input.trim()`（`expandPath`/`resolveToCwd` **未**开启 `trim`）；
  2. `normalizeUnicodeSpaces` 时把 `/[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g` 换成半角空格；
  3. `stripAtPrefix` 且以 `@` 开头 → 去掉一个 `@`；
  4. **仅 win32**：`normalizeWindowsShellPath(p)` —— 仅当 `p` 以单 `/` 开头（不是 `//`）且不含 `\` 时，
     按 `/^\/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/i` 把 Git Bash/MSYS/Cygwin/WSL 路径
     （`/c/Users/x`、`/mnt/c/Users/x`、`/cygdrive/c/Users/x`）转成 `C:\Users\x`（盘符大写，
     其余 `/`→`\`，无后缀时结果是 `C:\`）；
  5. `expandTilde`（默认 true）：`~` → `homeDir`（默认 `os.homedir()`）；`~/x`（win32 还有 `~\x`）→
     `join(home, x)`；`~user` 等**保持字面**（测试 `should keep tilde-prefixed filenames literal`）；
  6. `/^file:\/\//` → `fileURLToPath(p)`（`file:` URL 视为本地路径，见 `isLocalPath`）；
  7. 否则原样。
  `resolvePath(input, baseDir = cwd, options)`：两边都 `normalizePath` 后，
  绝对 → `resolve(normalized)`，相对 → `resolve(normalizedBaseDir, normalized)`。
  `getCwdRelativePath(filePath, cwd)`：解析后 `relative(cwd, path)`；在 cwd 内（`""`、不等于 `..`、
  不以 `..${sep}` 开头、非绝对）→ 返回 `relative || "."`，否则 `undefined`。
  `formatPathRelativeToCwdOrAbsolute(filePath, cwd)`：相对化优先，否则绝对路径，最后把 `sep` 全换成 `/`。
  另有（本单元不直接用但同文件导出）：`canonicalizePath`（`realpathSync` 失败回原串）、
  `getFileRevision`（`dev:ino:size:mtimeNs:ctimeNs`，bigint stat）、`isLocalPath`
  （`npm:/git:/github:/http:/https:/ssh:/builtin:` 前缀 → false）、`markPathIgnoredByCloudSync`
  （darwin `xattr -w com.dropbox.ignored`/`com.apple.fileprovider.ignore#P`，linux `setfattr -n user.com.dropbox.ignored`）。
- **实际使用点**（决定哪些工具享受 macOS 变体回退）：
  `read` 用 `resolveReadPathAsync`（异步，享受 AM/PM、NFD、弯引号、NFD+弯引号 四级回退）；
  `write`/`edit` 用 `resolveToCwd`（**不做**这些回退，路径必须字面存在）；
  `ls`/`grep`/`find` 用 `resolveToCwd`；`cli/file-processor.ts` 用同步版 `resolveReadPath`。
  `expandPath` 在**整个 `src/` 里没有任何调用点**（只有 `path-utils.test.ts` 直接测它），
  不要把它误写进 read/write/edit 的解析链；Rust 侧仍需导出以保持 API 对齐。
- `export function resolveReadPath(filePath, cwd)`（同步）与
  `export async function resolveReadPathAsync(filePath, cwd)`（异步）：同一算法，仅存在性检查方式不同。
  顺序严格为：
  1. `resolved = resolveToCwd(...)`；存在 → 返回。
  2. `amPmVariant = tryMacOSScreenshotPath(resolved)`；`!== resolved` 且存在 → 返回。
  3. `nfdVariant = tryNFDVariant(resolved)`；`!== resolved` 且存在 → 返回。
  4. `curlyVariant = tryCurlyQuoteVariant(resolved)`；`!== resolved` 且存在 → 返回。
  5. `nfdCurlyVariant = tryCurlyQuoteVariant(nfdVariant)`；`!== resolved` 且存在 → 返回。
  6. 全不命中 → 返回 `resolved`（第 1 步的路径，即使不存在）。
- Rust：`normalize("NFD")` 需 `unicode-normalization`（`NFD`），`"NFKC"` 需同 crate（见 edit-diff）。
  注意 JS 的 `normalize` 对非法 UTF-16 序列不报错；Rust 输入必为合法 UTF-8，无此风险。

### 2.3 `src/core/tools/file-mutation-queue.ts`

模块级状态：

```ts
const fileMutationQueues = new Map<string, Promise<void>>(); // key -> 尾部 promise
let registrationQueue = Promise.resolve();                   // 串行化「入队」本身
```

- `isMissingPathError(e)`：`e` 是对象且有 `code` 且 `code === "ENOENT" || "ENOTDIR"`。
- `getMutationQueueKey(filePath)`：`resolvedPath = resolve(filePath)`；
  `await realpath(resolvedPath)`；ENOENT/ENOTDIR → 返回 `resolvedPath`；其他错误**重新抛出**。
  （`resolve` 是 `path.resolve`，把相对路径按 `process.cwd()` 解析。）
- `export async function withFileMutationQueue<T>(filePath, fn)`：
  1. `registration = registrationQueue.then(async () => { key = await getMutationQueueKey(filePath);
     currentQueue = fileMutationQueues.get(key) ?? Promise.resolve();
     建 `nextQueue` 与 `releaseNext`；`chainedQueue = currentQueue.then(() => nextQueue)`;
     `fileMutationQueues.set(key, chainedQueue)`；返回 `{key,currentQueue,chainedQueue,releaseNext}` })`。
  2. `registrationQueue = registration.then(()=>undefined, ()=>undefined)`（注册失败也不毒化后续）。
  3. `await registration` → `await currentQueue`。
  4. `try { return await fn() } finally { releaseNext(); if (fileMutationQueues.get(key) === chainedQueue) fileMutationQueues.delete(key); }`

语义要点：入队顺序（谁先拿 key）由 `registrationQueue` 保证；同一 key 的 `fn` 严格串行，
不同 key 并行；`fn` 抛错不影响队列；只有仍是队尾时才删 map（避免删掉后来者的链）。

Rust 设计：用 `tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>` 也可以，
但**必须**复现「key 计算本身串行」这一点，否则两个并发调用可能拿到不同 key（realpath 竞态）。
建议：
`struct FileMutationQueue { inner: tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> }`，
`async fn with<F>(&self, path: &Path, f: F)` 在 `inner` 锁内算 key 并 clone `Arc`，
释放 `inner` 后再 `let _guard = arc.lock().await;`（顺序即公平 FIFO 由 tokio Mutex 保证；
严格来说 tokio Mutex 是 FIFO，符合「先注册先执行」）。
清理：`Arc::strong_count(&arc) == 2` 时移除（持有 inner 锁时判断）。

### 2.4 `src/core/tools/output-accumulator.ts`

```ts
export interface OutputAccumulatorOptions { maxLines?: number; maxBytes?: number; tempFilePrefix?: string; }
export interface OutputSnapshot { content: string; truncation: TruncationResult; fullOutputPath?: string; }
export interface FullOutput { content: string; truncated: boolean; }
```

构造：`maxLines ?? 2000`、`maxBytes ?? 51200`、`maxRollingBytes = max(maxBytes*2, 1)`、
`tempFilePrefix ?? "kiana-output"`，`decoder = new TextDecoder()`（非 fatal，非法字节 → U+FFFD）。

内部字段：`rawChunks: Buffer[]`、`tailText: string`、`tailBytes: number`、
`tailStartsAtLineBoundary = true`、`totalRawBytes`、`totalDecodedBytes`、`completedLines`、
`totalLines`、`currentLineBytes`、`hasOpenLine`、`finished`、
`tempFilePath: string | undefined`、`tempFileStream: WriteStream | undefined`。

计数语义（Rust 容易搞错，务必按名字区分）：
- `totalRawBytes` = **累计写入的原始字节数**（`append` 里 `data.length` 之和）。
- `totalDecodedBytes` = **解码后文本重新按 UTF-8 编码的字节数之和**（`byteLength(decodedChunk)`）。
  非法输入被替换成 U+FFFD（3 字节），因此它可能**大于** `totalRawBytes`；对合法 UTF-8 两者相等。
- `tailBytes` 同理是**文本的 UTF-8 字节数**，不是缓冲容量。
- `maxRollingBytes = max(maxBytes*2, 1)`；滚动裁剪触发条件是 `tailBytes > maxRollingBytes * 2`。

#### `append(data: Buffer): void`

1. `finished` → 抛 `new Error("Cannot append to a finished output accumulator")`。
2. `totalRawBytes += data.length`；`appendDecodedText(decoder.decode(data, {stream:true}))`。
3. 若已有 tempFileStream **或** `shouldUseTempFile()`：
   `ensureTempFile()`；`tempFileStream?.write(data)`（写**原始字节**）。
   否则若 `data.length > 0`：`rawChunks.push(data)`。

#### `finish(): void`

幂等（`finished` 已为 true 直接返回）。置 `finished = true`；`appendDecodedText(decoder.decode())`
（flush 剩余不完整序列）；若 `shouldUseTempFile()` → `ensureTempFile()`。

#### `snapshot({persistIfTruncated} = {}): OutputSnapshot`

1. `tailTruncation = truncateTail(getSnapshotText(), {maxLines, maxBytes})`。
2. `truncated = totalLines > maxLines || totalDecodedBytes > maxBytes`。
3. `truncatedBy = truncated ? (tailTruncation.truncatedBy ?? (totalDecodedBytes > maxBytes ? "bytes" : "lines")) : null`
   （`truncateTail` 对「截断后的快照文本」可能返回 `truncatedBy:null`——例如滚动缓冲裁掉了开头，
   但快照文本本身在限制内——此时用总量判断回填）。
4. `truncation = {...tailTruncation, truncated, truncatedBy, totalLines, totalBytes: totalDecodedBytes,
   maxLines, maxBytes}`。**注意 `outputLines/outputBytes` 来自 `tailTruncation`（即快照文本的统计），
   `totalLines/totalBytes` 是全量统计**。
5. `persistIfTruncated && truncation.truncated` → `ensureTempFile()`。
6. 返回 `{content: truncation.content, truncation, fullOutputPath: tempFilePath}`。

#### `async closeTempFile(): Promise<void>`

无 stream 直接返回。取走 stream 引用并置 undefined；`stream.end()` 并等 `finish` 或 `error`
（error 时移除 finish 监听并 reject）。Rust：`tokio::fs::File` + `flush`/`sync` 或 `std::fs::File`
（写入在 `spawn_blocking` 中）——注意必须**在返回前确保数据落盘**，因为后续 `readFullOutput` 会再打开它。

#### `async readFullOutput(maxBytes): Promise<FullOutput>`

- **无 tempFilePath**：`{content: new TextDecoder().decode(Buffer.concat(rawChunks)), truncated:false}`。
- 有：`file = await open(path,"r")`；`size = (await file.stat()).size`：
  - `size <= maxBytes` → `{content: decode(await file.readFile()), truncated:false}`；
  - 否则 `headBytes = floor(maxBytes/2)`、`tailBytes = maxBytes - headBytes`；
    `head = Buffer.alloc(headBytes)`、`tail = Buffer.alloc(tailBytes)`；
    `await file.read(head,0,headBytes,0)`、`await file.read(tail,0,tailBytes,size-tailBytes)`；
    `headText = new TextDecoder().decode(head,{stream:true})`（保留不完整尾，即**丢掉** head 末尾的半个字符）；
    `tailStart` 前跳续字节；`tailText = decode(tail.subarray(tailStart))`；
    `omitted = size - headBytes - tailBytes`；
    返回 `content = `${headText}\n\n[... ${omitted} bytes omitted ...]\n\n${tailText}``、`truncated:true`。
  - `finally { await file.close() }`。
- 注意 `file.read` 可能短读（Node 允许）；Rust 用 `read_exact` 或循环读满。

#### `getLastLineBytes(): number` → `currentLineBytes`

#### 私有 `appendDecodedText(text)`

1. 空串直接返回。
2. `bytes = byteLength(text)`；`totalDecodedBytes += bytes`；`tailText += text`；`tailBytes += bytes`；
   若 `tailBytes > maxRollingBytes * 2` → `trimTail()`。
3. 统计 `text` 中的 `\n`：`newlines`（个数）、`lastNewline`（最后一个下标）。
   - `newlines === 0`：`currentLineBytes += bytes`；`hasOpenLine = true`。
   - 否则：`completedLines += newlines`；`tail = text.slice(lastNewline+1)`；
     `currentLineBytes = byteLength(tail)`；`hasOpenLine = tail.length > 0`。
4. `totalLines = completedLines + (hasOpenLine ? 1 : 0)`。

样本点（测试 `should not count a trailing newline as an extra truncated bash output line` 固定）：
`"a\n"` → completedLines=1, hasOpenLine=false, totalLines=1。

#### 私有 `trimTail()`

`buffer = Buffer.from(tailText,"utf-8")`；若 `buffer.length <= maxRollingBytes`：仅更新 `tailBytes`
并返回。`start = buffer.length - maxRollingBytes`；前跳续字节；
`tailStartsAtLineBoundary = start === 0 ? tailStartsAtLineBoundary : buffer[start-1] === 0x0a`；
`tailText = buffer.subarray(start).toString("utf-8")`；`tailBytes = byteLength(tailText)`。

#### 私有 `getSnapshotText()`

`tailStartsAtLineBoundary` → 返回 `tailText`；否则丢弃第一个不完整行：
`firstNewline = tailText.indexOf("\n")`；-1 → 返回 `tailText`；否则 `tailText.slice(firstNewline+1)`。

#### 私有 `shouldUseTempFile()`

`totalRawBytes > maxBytes || totalDecodedBytes > maxBytes || totalLines > maxLines`。

#### 私有 `ensureTempFile()`

已有 `tempFilePath` 返回。`createOutputFileStream(tempFilePrefix, ".log")`（见 4.1）；
把 `rawChunks` 全部写入，清空 `rawChunks`。

### 2.5 `src/core/tools/read.ts`

#### 2.5.1 参数 schema（TypeBox → JSON Schema 逐字）

```ts
const readSchema = Type.Object({
  path:   Type.String({ description: "Path to the file to read (relative or absolute)" }),
  offset: Type.Optional(Type.Number({ description: "Line number to start reading from (1-indexed)" })),
  limit:  Type.Optional(Type.Number({ description: "Maximum number of lines to read" })),
});
```

必填数组为 `["path"]`，`additionalProperties:false`（TypeBox 默认）。Rust 用 `serde_json::Value`
常量或 `schemars` 生成，但**描述文本必须逐字**（它们会进入 provider 的 tools JSON）。

#### 2.5.2 系统提示贡献

```ts
export const readToolSystemPromptContribution = {
  snippet: "Read file contents",
  guidelines: ["Use read to examine files instead of cat or sed."],
} as const;
```

#### 2.5.3 输出 schema

```ts
const readOutputSchema = Type.Union([
  Type.String(),
  Type.Object({ type: Type.Literal("image"), data: Type.String(), mimeType: Type.String(), note: Type.String() }),
]);
```
（刻意不写属性描述，让工具描述里类型保持单行。）

#### 2.5.4 类型

```ts
export type ReadToolInput = Static<typeof readSchema>;
export type ReadToolOutput = Static<typeof readOutputSchema>;
export interface ReadToolDetails { truncation?: TruncationResult; }
export interface ReadOperations {
  readFile: (absolutePath: string) => Promise<Buffer>;
  access: (absolutePath: string) => Promise<void>;
  detectImageMimeType?: (absolutePath: string) => Promise<string | null | undefined>;
}
export interface ReadToolOptions {
  autoResizeImages?: boolean;           // 默认 true
  resizeOptions?: ModelImageResizeOptions; // 无模型元数据时的回退档
  operations?: ReadOperations;
}
```

默认操作：`readFile = fs.readFile(path)`、`access = fs.access(path, R_OK)`、
`detectImageMimeType = detectSupportedImageMimeTypeFromFile`（`utils/mime.ts`，按文件魔数嗅探）。

#### 2.5.5 工具描述（逐字，`${...}` 已代入）

```
Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.
```
（`DEFAULT_MAX_LINES` = 2000；`DEFAULT_MAX_BYTES/1024` = 50——注意是整数除法后的 50，
所以是 `50KB` 而不是 `50.0KB`。）

#### 2.5.6 `toReadOutput(content) -> ReadToolOutput`

`text = 第一个 type==="text" 块的 text ?? ""`；`image = 第一个 type==="image" 块`；
有 image → `{type:"image", data: image.data, mimeType: image.mimeType, note: text}`；否则返回 `text`。
（键顺序恰为 type/data/mimeType/note。）

#### 2.5.7 `getNonVisionImageNote(model) -> string | undefined`

`!model || model.input.includes("image")` → `undefined`；否则逐字：
```
[Current model does not support images. The image will be omitted from this request.]
```

#### 2.5.8 定义对象

`name:"read"`、`label:"read"`、`description` 见上、`promptSnippet`、`promptGuidelines`（数组拷贝）、
`parameters: readSchema`、`outputSchema: readOutputSchema`、
`constrainedSampling: {type:"json_schema", strict:"prefer"}`、`execute`、`...readRenderers`。

#### 2.5.9 `execute` 流程（逐分支）

包在一个 `new Promise` 里实现「可中断」：

1. 若 `signal?.aborted` → `reject(new Error("Operation aborted"))`，返回。
2. 注册一次 `abort` 监听：置 `aborted=true` 并 reject `new Error("Operation aborted")`。
3. 异步主体（每个 await 后检查 `aborted` 决定是否提前 return；`catch` 中若 `aborted` 则不 reject）：
   1. `absolutePath = await resolveReadPathAsync(path, ctx?.cwd || cwd)`。
   2. `await ops.access(absolutePath)`——不存在/不可读时抛原生 ENOENT/EACCES（错误消息由 Node 决定，
      例如 `ENOENT: no such file or directory, access '/x'`）。
   3. `mimeType = ops.detectImageMimeType ? await ops.detectImageMimeType(absolutePath) : undefined`。
   4. `nonVisionImageNote = getNonVisionImageNote(ctx?.model)`。
   5. **图片分支**（`mimeType` 为真值）：
      `buffer = await ops.readFile(absolutePath)`；
      `processed = await processImage(buffer, mimeType, {autoResizeImages, resizeOptions: ctx?.model?.inputLimits?.images?.resize ?? fallbackResizeOptions})`。
      - `!processed.ok` → 单一 text 块：
        `` `Read image file [${mimeType}]\n${processed.message}` ``，若 `nonVisionImageNote` 再追加 `\n${note}`。
      - 成功 → `` `Read image file [${processed.mimeType}]` ``；`processed.hints.length > 0` 时追加
        `\n${hints.join("\n")}`；再追加 nonVision note；content =
        `[{type:"text", text:textNote}, {type:"image", data:processed.data, mimeType:processed.mimeType}]`。
      `details` 保持 `undefined`。
   6. **文本分支**：
      - `buffer.toString("utf-8")`（非法 UTF-8 → U+FFFD，与 `TextDecoder` 一致）；
      - `allLines = textContent.split("\n")`；`totalFileLines = allLines.length`；
      - `startLine = offset ? Math.max(0, offset - 1) : 0`（`offset` 为 0 或 undefined 时取 0；
        **负数 offset** 经 `Math.max` 变 0；`offset` 为 1 → 0）；
      - `startLineDisplay = startLine + 1`；
      - `startLine >= allLines.length` → 抛
        `` `Offset ${offset} is beyond end of file (${allLines.length} lines total)` ``
        （注意插的是**原始 `offset` 参数**，可能是 undefined；但该分支要求 `startLine >= len`，
        在 `offset === undefined` 时 `startLine = 0`，只在文件为空（`allLines = [""]`，len 1）时不可能触发，
        故实际只在 offset 超界时出现）；
      - `limit !== undefined`：`endLine = Math.min(startLine + limit, allLines.length)`；
        `selectedContent = allLines.slice(startLine, endLine).join("\n")`；`userLimitedLines = endLine - startLine`；
        否则 `selectedContent = allLines.slice(startLine).join("\n")`，`userLimitedLines = undefined`；
      - `truncation = truncateHead(selectedContent)`；
      - **首行超限**：`firstLineSize = formatSize(Buffer.byteLength(allLines[startLine], "utf-8"))`；
        `outputText =` 逐字
        ```
        [Line {startLineDisplay} is {firstLineSize}, exceeds 50.0KB limit. Use bash: sed -n '{startLineDisplay}p' {path} | head -c 51200]
        ```
        （`formatSize(DEFAULT_MAX_BYTES)` = `50.0KB`；`${path}` 是**原始参数**，不是绝对路径；
        末尾 `]` 前无空格；`head -c 51200` 是十进制字节数）；`details = {truncation}`。
      - **普通截断**：`endLineDisplay = startLineDisplay + truncation.outputLines - 1`；
        `nextOffset = endLineDisplay + 1`；`outputText = truncation.content`，再追加：
        - `truncatedBy === "lines"`：
          `` `\n\n[Showing lines ${startLineDisplay}-${endLineDisplay} of ${totalFileLines}. Use offset=${nextOffset} to continue.]` ``
        - 否则（bytes）：
          `` `\n\n[Showing lines ${startLineDisplay}-${endLineDisplay} of ${totalFileLines} (50.0KB limit). Use offset=${nextOffset} to continue.]` ``
        `details = {truncation}`。
      - **用户 limit 提前停止**（`userLimitedLines !== undefined && startLine + userLimitedLines < allLines.length`）：
        `remaining = allLines.length - (startLine + userLimitedLines)`；
        `nextOffset = startLine + userLimitedLines + 1`；
        `` `${truncation.content}\n\n[${remaining} more lines in file. Use offset=${nextOffset} to continue.]` ``
      - 否则 `outputText = truncation.content`。
      - `content = [{type:"text", text: outputText}]`。
   7. 成功后 `resolve({content, details})`。
4. `.then(result => ({...result, structuredContent: toReadOutput(result.content)}))`。

`createReadTool(cwd, options)` = `wrapToolDefinition(createReadToolDefinition(cwd, options))`。

Rust 注意：`Buffer.toString("utf-8")` 采用 WHATWG「maximal subpart」规则（每个最大非法子部分一个
U+FFFD）。已实测 Node 26 对以下输入的结果：`[E0,80,80]`→3 个、`[F0,80,80,80]`→4 个、`[C0,80]`→2 个、
`[ED,A0,80]`→3 个、`[F5,80,80,80]`→4 个、`[E2,82]`（截断）→1 个、`[80,80,80]`→3 个。
Rust 的 `String::from_utf8_lossy` 对「首个续字节非法」的 `error_len` 为 1，因此**在上述用例上结果一致**；
但两者并非规范级等价（`from_utf8_lossy` 不承诺 WHATWG 语义）。若要求严格一致，用
`encoding_rs::UTF_8.decode_without_bom_handling`（WHATWG 实现）或自写解码器，并把上面 7 个用例
作为黄金测试；**不要**假设 `from_utf8_lossy` 一定等价。
若要字节级一致，需自写 WHATWG UTF-8 解码器（第 8 节）。

### 2.6 `src/core/tools/write.ts`

- schema：`path` 描述 `"Path to the file to write (relative or absolute)"`、
  `content` 描述 `"Content to write to the file"`；必填 `["path","content"]`。
- 系统提示：`snippet: "Create or overwrite files"`；
  `guidelines: ["Use write only for new files or complete rewrites."]`。
- 描述（逐字）：`Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.`
- `WriteOperations { writeFile(abs, content): Promise<void>; mkdir(dir): Promise<void> }`；
  默认 `fs.writeFile(path, content, "utf-8")` 与 `fs.mkdir(dir,{recursive:true}).then(()=>{})`。
- 无 `outputSchema`、无 `constrainedSampling` 字段？——实际有 `constrainedSampling: {type:"json_schema", strict:"prefer"}`，
  无 `outputSchema`。
- `execute`：
  1. `absolutePath = resolveToCwd(path, ctx?.cwd || cwd)`；`dir = dirname(absolutePath)`。
  2. `withFileMutationQueue(absolutePath, async () => {...})`：
     - `throwIfAborted()`（`signal?.aborted` → `new Error("Operation aborted")`）；
     - `await ops.mkdir(dir)`；`throwIfAborted()`；
     - `await ops.writeFile(absolutePath, content)`；`throwIfAborted()`；
     - 返回 `{content:[{type:"text", text:`Successfully wrote to ${path}`}], details: undefined}`——
       **用原始 `path` 参数**，不是绝对路径。
  3. 注释明确说明：**不在 abort 监听里 reject**，否则会在文件操作仍在飞行时释放 mutation 队列。
- `createWriteTool` = wrap。

### 2.7 `src/core/tools/edit.ts`

#### 2.7.1 schema（逐字描述）

```ts
const replaceEditSchema = Type.Object({
  oldText: Type.String({ description:
    "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call." }),
  newText: Type.String({ description: "Replacement text for this targeted edit." }),
}, {});

const editSchema = Type.Object({
  path: Type.String({ description: "Path to the file to edit (relative or absolute)" }),
  edits: Type.Array(replaceEditSchema, { description:
    "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead." }),
}, {});
```

#### 2.7.2 系统提示（逐字）

```json
{
  "snippet": "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
  "guidelines": [
    "Use edit for precise changes (edits[].oldText must match exactly)",
    "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
    "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
    "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions."
  ]
}
```

#### 2.7.3 类型与描述

```ts
export type EditToolInput = Static<typeof editSchema>;
export interface EditToolDetails { diff: string; patch: string; firstChangedLine?: number; }
export interface EditOperations {
  readFile: (abs: string) => Promise<Buffer>;
  writeFile: (abs: string, content: string) => Promise<void>;
  access: (abs: string) => Promise<void>;   // R_OK | W_OK
}
export interface EditToolOptions { operations?: EditOperations; }
```

描述（逐字）：
```
Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.
```

定义对象额外含：`constrainedSampling`、**`renderShell: "self"`**、`prepareArguments: prepareEditArguments`、
`...editRenderers`。

#### 2.7.4 `prepareEditArguments(input)`（兼容 shim，**未导出**，仅挂在 definition 上；`edit.ts` 是唯一设置 `prepareArguments` 的工具）

1. `input` 非对象 → 原样返回。
2. `args = input as Record<string, unknown>`。
3. 若 `typeof args.edits === "string"`：尝试 `JSON.parse`；数组 → 直接替换；
   若解析结果是「单条 edit 对象」（非数组对象且 `oldText`/`newText` 均为 string）→ `[parsed]`；
   解析失败吞掉（`catch {}`）。
4. 否则若 `isSingleEditInput(args.edits)`（对象、非数组、两字段皆 string）→ `[args.edits]`。
5. **legacy 折叠**：`legacy = args`；若 `typeof legacy.oldText !== "string" || typeof legacy.newText !== "string"`
   → 返回 `args`；否则
   `edits = Array.isArray(legacy.edits) ? [...legacy.edits] : []`；`edits.push({oldText, newText})`；
   解构去掉 `oldText`/`newText` 后 `{...rest, edits}`（**键顺序：rest 的键（path, edits 等）先，
   edits 在最后被覆写为新的数组**；由于 `rest` 里已含 `edits`，展开后 `edits` 保持原位置，
   值被覆盖——JS 语义如此，Rust 需用有序 map 覆盖同键保持原位）。
6. `validateEditInput(input)`：`!Array.isArray(input.edits) || input.edits.length === 0` →
   抛 `new Error("Edit tool input is invalid. edits must contain at least one replacement.")`。

#### 2.7.5 `execute`

1. `const {path, edits} = validateEditInput(input)`；`absolutePath = resolveToCwd(path, ctx?.cwd || cwd)`。
2. `withFileMutationQueue(absolutePath, async () => {...})`：
   1. `throwIfAborted()`。
   2. `ops.access(absolutePath)`；失败时：先 `throwIfAborted()`（若已中止则抛中止而不是访问错误），
      再构造：`errorMessage = error instanceof Error && "code" in error ? `Error code: ${error.code}` : String(error)`；
      `throw new Error(`Could not edit file: ${path}. ${errorMessage}.`)`。
      （`String(error)` 对非 Error 对象给出 `[object Object]` 等；Node 的 fs 错误恒为 Error 且有 `code`。）
   3. `throwIfAborted()`；`buffer = await ops.readFile(absolutePath)`；`rawContent = buffer.toString("utf-8")`；
      `throwIfAborted()`。
   4. `{bom, text: content} = splitBom(rawContent)`（`splitBom`：以 `﻿` 开头 → `{bom:"﻿", text: content.slice(1)}`，
      否则 `{bom:"", text: content}`）。
   5. `originalEnding = detectLineEnding(content)`；`normalizedContent = normalizeToLF(content)`；
      `{baseContent, newContent} = applyEditsToNormalizedContent(normalizedContent, edits, path)`；
      `throwIfAborted()`。
   6. `finalContent = bom + restoreLineEndings(newContent, originalEnding)`；
      `await ops.writeFile(absolutePath, finalContent)`；`throwIfAborted()`。
   7. `diffResult = generateDiffString(baseContent, newContent)`；
      `patch = generateUnifiedPatch(path, baseContent, newContent)`。
   8. 返回 text = `` `Successfully replaced ${edits.length} block(s) in ${path}.` ``；
      `details = {diff: diffResult.diff, patch, firstChangedLine: diffResult.firstChangedLine}`。
      （键顺序 diff/patch/firstChangedLine；`firstChangedLine` 为 `undefined` 时 JSON 序列化会**省略**该键。）

### 2.8 `src/core/tools/edit-diff.ts`

#### 2.8.1 行尾处理

- `detectLineEnding(content) -> "\r\n" | "\n"`：`crlfIdx = content.indexOf("\r\n")`、`lfIdx = content.indexOf("\n")`；
  `lfIdx === -1` → `"\n"`；`crlfIdx === -1` → `"\n"`；否则 `crlfIdx < lfIdx ? "\r\n" : "\n"`。
- `normalizeToLF(text)`：`text.replace(/\r\n/g,"\n").replace(/\r/g,"\n")`。
- `restoreLineEndings(text, ending)`：`ending === "\r\n" ? text.replace(/\n/g,"\r\n") : text`。

#### 2.8.2 `normalizeForFuzzyMatch(text) -> string`（导出）

严格按顺序：
1. `text.normalize("NFKC")`（Unicode 兼容分解 + 组合；全角标点 → 半角等）。
2. 按 `\n` split → 每行 `trimEnd()` → join `\n`（**只去行尾空白**，行首保留）。
3. `[‘’‚‛]` → `'`。
4. `[“”„‟]` → `"`。
5. `[‐‑‒–—―−]` → `-`。
6. `[  -   　]` → ` `（空格）。

Rust：`unicode-normalization::UnicodeNormalization::nfkc()`。**注意** JS 的 `normalize` 作用于整个字符串
（可能产生跨行变化，但 NFKC 不引入换行），顺序必须一致（先 NFKC 再去行尾空白，因为 NFKC 可能把
某些空白变体变成普通空格后又被 trimEnd 去掉）。

#### 2.8.3 `splitLinesWithEndings(content) -> string[]`

`content.match(/[^\n]*\n|[^\n]+/g) ?? []`——每行**保留换行符**；末尾无换行的最后一行也收；
空串 → `[]`。例：`"a\nb"` → `["a\n","b"]`；`"a\n"` → `["a\n"]`；`""` → `[]`。

Rust 实现：逐 `char_indices` 找 `\n`，含换行地切；注意不能用 `str::lines()`（会丢换行且会吃掉 `\r` 语义差异——
这里 `\r` 是普通字符）。

#### 2.8.4 行span 与替换

```ts
interface LineSpan { start: number; end: number; }   // 字符偏移（UTF-16 码元）
interface MatchedEdit { editIndex: number; matchIndex: number; matchLength: number; newText: string; }
type TextReplacement = Pick<MatchedEdit, "matchIndex" | "matchLength" | "newText">;
```

- `getLineSpans(content)`：对 `splitLinesWithEndings` 的每行累计 `{start: offset, end: offset + line.length}`。
- `getReplacementLineRange(lines, replacement)`：
  `replacementStart = matchIndex`；`replacementEnd = matchIndex + matchLength`；
  找第一个 `line` 使 `start <= replacementStart < end` → `startLine`；找不到 → 抛
  `new Error("Replacement range is outside the base content.")`；
  `endLine = startLine`；while `endLine < lines.length && lines[endLine].end < replacementEnd` → `endLine++`；
  若 `endLine >= lines.length` → 抛同一错误；返回 `{startLine, endLine: endLine + 1}`。
- `applyReplacements(content, replacements, offset = 0)`：**逆序**应用
  `content.substring(0, matchIndex-offset) + newText + content.substring(matchIndex-offset+matchLength)`。
- `applyReplacementsPreservingUnchangedLines(originalContent, baseContent, replacements)`：
  1. `originalLines = splitLinesWithEndings(originalContent)`；`baseLines = getLineSpans(baseContent)`；
     行数不等 → 抛
     `new Error("Cannot preserve unchanged lines because the base content has a different line count.")`。
  2. 按 `matchIndex` 升序排序 replacements，逐个求 `range = getReplacementLineRange(baseLines, r)`；
     与上一组重叠（`range.startLine < current.endLine`）→ 合并（`endLine = max(...)`、追加 replacement）；
     否则新组。
  3. `originalLineIndex = 0`；`result = ""`；对每组：
     - `originalLines.slice(originalLineIndex, group.startLine).join("")` 原样拼接；
     - `groupStartOffset = baseLines[group.startLine].start`；`groupEndOffset = baseLines[group.endLine-1].end`；
     - 对 `baseContent.slice(groupStartOffset, groupEndOffset)` 应用该组替换（offset=groupStartOffset）；
     - `originalLineIndex = group.endLine`。
  4. 末尾补 `originalLines.slice(originalLineIndex).join("")`。

#### 2.8.5 模糊查找

```ts
export interface FuzzyMatchResult {
  found: boolean; index: number; matchLength: number;
  usedFuzzyMatch: boolean; contentForReplacement: string;
}
export interface Edit { oldText: string; newText: string; }
export interface AppliedEditsResult { baseContent: string; newContent: string; }
```

`fuzzyFindText(content, oldText)`：
1. `exactIndex = content.indexOf(oldText)`；命中 → `{found:true, index:exactIndex, matchLength: oldText.length,
   usedFuzzyMatch:false, contentForReplacement: content}`（`oldText` 为空串时 `indexOf` 返回 0——由上层空检查拦截）。
2. 否则 `fuzzyContent = normalizeForFuzzyMatch(content)`、`fuzzyOldText = normalizeForFuzzyMatch(oldText)`；
   `fuzzyIndex = fuzzyContent.indexOf(fuzzyOldText)`；未命中 → `{found:false, index:-1, matchLength:0,
   usedFuzzyMatch:false, contentForReplacement: content}`。
3. 命中 → `{found:true, index: fuzzyIndex, matchLength: fuzzyOldText.length, usedFuzzyMatch:true,
   contentForReplacement: fuzzyContent}`。

`countOccurrences(content, oldText)`：`normalizeForFuzzyMatch(content).split(normalizeForFuzzyMatch(oldText)).length - 1`。
（对空 oldText 会返回 `fuzzyContent.length + 1`，但空串在更早处被拦截。）

#### 2.8.6 错误工厂（**逐字**，`${}` 已代入格式）

| 工厂 | 条件 | 消息 |
|---|---|---|
| `getNotFoundError(path, i, total)` | `total === 1` | `Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.` |
| 同上 | `total > 1` | `Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines.` |
| `getDuplicateError(path, i, total, n)` | `total === 1` | `Found {n} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.` |
| 同上 | `total > 1` | `Found {n} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique.` |
| `getEmptyOldTextError(path, i, total)` | `total === 1` | `oldText must not be empty in {path}.` |
| 同上 | `total > 1` | `edits[{i}].oldText must not be empty in {path}.` |
| `getNoChangeError(path, total)` | `total === 1` | `No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.` |
| 同上 | `total > 1` | `No changes made to {path}. The replacements produced identical content.` |
| overlap（内联） | — | `edits[{previous.editIndex}] and edits[{current.editIndex}] overlap in {path}. Merge them into one edit or target disjoint regions.` |

`i` 是**归一化后 edits 数组的下标**（0 起）；`n` 是出现次数（`countOccurrences` 结果，≥2）。

#### 2.8.7 `applyEditsToNormalizedContent(normalizedContent, edits, path) -> AppliedEditsResult`

1. `normalizedEdits = edits.map(e => ({oldText: normalizeToLF(e.oldText), newText: normalizeToLF(e.newText)}))`。
2. 逐个检查 `oldText.length === 0` → 抛 `getEmptyOldTextError(path, i, n)`。
3. `initialMatches = normalizedEdits.map(e => fuzzyFindText(normalizedContent, e.oldText))`；
   `usedFuzzyMatch = initialMatches.some(m => m.usedFuzzyMatch)`；
   `replacementBaseContent = usedFuzzyMatch ? normalizeForFuzzyMatch(normalizedContent) : normalizedContent`。
4. 逐 edit **重新**在 `replacementBaseContent` 上 `fuzzyFindText`：
   - 未找到 → `getNotFoundError(path, i, n)`；
   - `occurrences = countOccurrences(replacementBaseContent, edit.oldText)`；`> 1` → `getDuplicateError(path, i, n, occurrences)`；
   - push `{editIndex: i, matchIndex, matchLength, newText}`。
5. `matchedEdits.sort((a,b) => a.matchIndex - b.matchIndex)`；相邻检查
   `previous.matchIndex + previous.matchLength > current.matchIndex` → overlap 错误。
6. `baseContent = normalizedContent`；
   `newContent = usedFuzzyMatch ? applyReplacementsPreservingUnchangedLines(normalizedContent, replacementBaseContent, matchedEdits)
                               : applyReplacements(replacementBaseContent, matchedEdits)`。
7. `baseContent === newContent` → `getNoChangeError(path, n)`。
8. 返回 `{baseContent, newContent}`（`baseContent` 恒为**未模糊归一化**的 `normalizedContent`，
   因此 `details.diff` 里展示的是原始文本而非归一化文本）。

**重要边界**：`occurrences > 1` 的判定在**每个** edit 上都做，即便该 edit 在初始匹配里是精确命中；
且计数在 `replacementBaseContent` 上进行（有任一 edit 用到模糊匹配时，所有 edit 的重复检测都在归一化空间）。
测试 `should preserve the correct occurrence when fuzzy replacement equals a nearby line` 固定了该行为。

#### 2.8.8 `generateUnifiedPatch(path, oldContent, newContent, contextLines = 4)`

`Diff.createTwoFilesPatch(path, path, old, new, undefined, undefined, {context: 4, headerOptions: Diff.FILE_HEADERS_ONLY})`。
JS `diff` 的 `FILE_HEADERS_ONLY` 输出**不含 `Index:` 行**，形如：

```
===================================================================
--- {path}
+++ {path}
@@ -1,3 +1,3 @@
 context
-old
+new
```

（第一行是 67 个 `=`；`---`/`+++` 行不带时间戳；`@@` 头由 `diff` 的 `structuredPatch` 生成；
无 hunk 时（内容相同）返回空字符串。）Rust 必须用 `similar` 手写同样的格式（不提供 `Index:` 行），
详见 8.3。

#### 2.8.9 `generateDiffString(oldContent, newContent, contextLines = 4) -> {diff, firstChangedLine?}`

基于 `Diff.diffLines(oldContent, newContent)`（**按行 diff，忽略行尾差异?——不，`diffLines` 默认
`ignoreWhitespace:false`，逐字符比较**）。输出行格式：

- 变更行：`+{行号右对齐到宽度} {内容}` / `-{行号} {内容}`；
- 上下文行：` {行号} {内容}`；
- 跳过行：` {"".padStart(width)} ...`（前缀空格 + 宽度个空格 + ` ...`，即 `" " + " ".repeat(width) + " ..."`）。

细节：
1. `parts = Diff.diffLines(old, new)`；`oldLines = old.split("\n")`、`newLines = new.split("\n")`；
   `maxLineNum = max(oldLines.length, newLines.length)`；`lineNumWidth = String(maxLineNum).length`。
2. 状态：`oldLineNum = 1`、`newLineNum = 1`、`lastWasChange = false`、`firstChangedLine = undefined`。
3. 对每个 part：`raw = part.value.split("\n")`；若最后一个元素是 `""` 则 `raw.pop()`。
   - **added / removed**：若 `firstChangedLine === undefined` → `firstChangedLine = newLineNum`（**在写行之前取**）；
     逐行 push（added 用 `newLineNum++`；removed 用 `oldLineNum++`）；`lastWasChange = true`。
   - **context**：`nextPartIsChange = i < parts.length-1 && (parts[i+1].added || parts[i+1].removed)`；
     `hasLeadingChange = lastWasChange`；`hasTrailingChange = nextPartIsChange`：
     - 两者皆真：`raw.length <= contextLines*2` → 全部输出并双计数；
       否则 `leading = raw.slice(0, ctx)`、`trailing = raw.slice(-ctx)`、`skipped = raw.length - 2*ctx`；
       输出 leading → ` ...` 行（`oldLineNum += skipped`、`newLineNum += skipped`）→ trailing。
     - 仅 leading：显示前 `ctx` 行；`skipped > 0` → 输出 ` ...` 行并双计数跳过。
     - 仅 trailing：`skipped = max(0, raw.length - ctx)`；`> 0` → 输出 ` ...` 行并双计数；
       再输出 `raw.slice(skipped)`。
     - 都不是：整段跳过（双计数 `raw.length`）。
     之后 `lastWasChange = false`。
4. 返回 `{diff: output.join("\n"), firstChangedLine}`。

注意「` ...` 跳过行」的行号推进：`oldLineNum` 与 `newLineNum` 同步加 `skipped`（上下文行在两份文件中都存在）。
注意 `firstChangedLine` 只在**第一次**变更时设置，且取的是**新文件**行号。

#### 2.8.10 `computeEditsDiff(path, edits, cwd)` / `computeEditDiff(path, oldText, newText, cwd)`

- `absolutePath = resolveToCwd(path, cwd)`。
- `access(absolutePath, R_OK)` 失败 → `{error: `Could not edit file: ${path}. ${errorMessage}.`}`，
  `errorMessage` 同上（`Error code: ${code}` 或 `String(error)`）。
- `readFile(absolutePath, "utf-8")`（**直接按 utf-8 读成字符串**，不是 Buffer 再 toString；结果等价）。
- `splitBom` → `normalizeToLF` → `applyEditsToNormalizedContent` → `generateDiffString`。
- 任何抛出 → `{error: err instanceof Error ? err.message : String(err)}`。
- 返回 `EditDiffResult {diff, firstChangedLine} | EditDiffError {error}`。
- `computeEditDiff` 是单编辑便捷包装。

用途：TUI 在 `argsComplete` 时**异步预演** diff（见 2.16）。

### 2.9 `src/core/tools/bash.ts`

#### 2.9.1 常量与超时解析

```ts
const MAX_TIMEOUT_MS = 2_147_483_647;               // 2^31-1
const STRUCTURED_OUTPUT_MAX_BYTES = 1024 * 1024;     // 1 MiB
const MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS / 1000;   // 2147483.647
```

`resolveTimeoutMs(timeout)`：
- `undefined` → `undefined`；
- `!Number.isFinite(timeout) || timeout <= 0` → 抛
  `new Error("Invalid timeout: must be a finite number of seconds")`；
- `timeoutMs = timeout * 1000`；`> MAX_TIMEOUT_MS` → 抛
  `` new Error(`Invalid timeout: maximum is ${MAX_TIMEOUT_SECONDS} seconds`) ``
  （逐字：`Invalid timeout: maximum is 2147483.647 seconds`）；
- 返回 `timeoutMs`。

#### 2.9.2 schema（逐字）

```ts
const bashSchema = Type.Object({
  command: Type.String({ description: "Shell command to execute" }),
  timeout: Type.Optional(Type.Number({ description: "Timeout in seconds (optional, no default timeout)" })),
});
```

输出 schema（`bashOutputSchema`，用于 codemode 等程序化调用者）：

```ts
Type.Object({
  output: Type.String({ description: "Combined stdout and stderr, possibly truncated" }),
  truncated: Type.Boolean(),
  full_output_path: Type.Optional(Type.String({ description: "Full output, when truncated" })),
  exit_code: Type.Number(),
  wall_time_seconds: Type.Number(),
})
```

#### 2.9.3 系统提示

```ts
export const bashToolSystemPromptContribution = {
  snippet: "Execute bash commands (ls, grep, find, etc.)",
  guidelines: ["You can inspect KIANA_* environment variables for current model and session details."],
} as const;
```

#### 2.9.4 类型

```ts
export interface BashToolDetails { truncation?: TruncationResult; fullOutputPath?: string; }
export interface BashOperations {
  exec: (command: string, cwd: string, options: {
    onData: (data: Buffer) => void;
    signal?: AbortSignal;
    timeout?: number;        // 秒
    env?: NodeJS.ProcessEnv;
  }) => Promise<{ exitCode: number | null }>;
}
export interface BashSpawnContext { command: string; cwd: string; env: NodeJS.ProcessEnv; }
export type BashSpawnHook = (context: BashSpawnContext) => BashSpawnContext;
export interface BashToolOptions {
  operations?: BashOperations;
  commandPrefix?: string;
  shellPath?: string;
  exposeSessionEnvironment?: boolean;  // 默认 true
  spawnHook?: BashSpawnHook;
}
export type BashRenderState = {
  startedAt: number | undefined;
  endedAt: number | undefined;
  interval: NodeJS.Timeout | undefined;
};
export interface ShellToolConfig {
  name: string; label: string; shellName: string; prompt: string;
  promptSnippet: string; promptGuidelines?: readonly string[]; tempFilePrefix: string;
}
export type BashToolOutput = Static<typeof bashOutputSchema>;
```

#### 2.9.5 `createLocalShellOperations(shellName, resolveShellConfig)`（导出）

`exec(command, cwd, {onData, signal, timeout, env})`：
1. `timeoutMs = resolveTimeoutMs(timeout)`（可能抛，**在检查 cwd 与 abort 之前**）。
2. `signal?.aborted` → 抛 `new Error("aborted")`。
3. `shellConfig = resolveShellConfig()`（可能抛，例如 `Custom shell path not found: ...`）。
4. `fsAccess(cwd, F_OK)` 失败 → 抛
   `` new Error(`Working directory does not exist: ${cwd}\nCannot execute ${shellName} commands.`) ``
   （中间是**单个 `\n`**，无空行）。
5. `commandFromStdin = shellConfig.commandTransport === "stdin"`；
   `spawn(shellConfig.shell, commandFromStdin ? shellConfig.args : [...shellConfig.args, command], {cwd,
   detached: process.platform !== "win32", env: env ?? getShellEnv(),
   stdio: [commandFromStdin ? "pipe" : "ignore", "pipe", "pipe"], windowsHide: true})`。
6. stdin 传输：`child.stdin?.on("error", () => {})`；`child.stdin?.end(command)`。
7. `if (child.pid) trackDetachedChildPid(child.pid)`（全局集合，见 `utils/shell.ts`）。
8. `timedOut = false`；`timeoutHandle`；`onAbort = () => { if (child.pid) killProcessTree(child.pid) }`。
9. try：
   - `timeoutMs !== undefined` → `setTimeout(() => { timedOut = true; if (child.pid) killProcessTree(child.pid) }, timeoutMs)`；
   - `child.stdout?.on("data", onData)`、`child.stderr?.on("data", onData)`；
   - `signal` 存在：已 aborted → 立即 `onAbort()`；否则 `addEventListener("abort", onAbort, {once:true})`；
   - `exitCode = await waitForChildProcess(child)`（见 2.28.3）；
   - `signal?.aborted` → 抛 `new Error("aborted")`；
   - `timedOut` → 抛 `` new Error(`timeout:${timeout}`) ``（`timeout` 是**原始秒数参数**，可能带小数，
     例如 `timeout:1.5`；随后 bash.ts 用 `err.message.split(":")[1]` 取回它）；
   - `signalCode = child.signalCode`；返回
     `{exitCode: exitCode ?? (signalCode ? 128 + (osConstants.signals[signalCode] ?? 0) : 1)}`。
10. finally：`untrackDetachedChildPid`；`clearTimeout`；`removeEventListener`。

`createLocalBashOperations({shellPath}?)` = `createLocalShellOperations("bash", () => getShellConfig(shellPath))`。

#### 2.9.6 `resolveSpawnContext(command, cwd, spawnHook, exposeSessionEnvironment, ctx)`

1. `env = {...getShellEnv()}`（当前进程环境 + 把 `getBinDir()` 前置/补进 PATH）。
2. `delete env.KIANA_SESSION_ID / KIANA_SESSION_FILE / KIANA_PROVIDER / KIANA_MODEL / KIANA_REASONING_LEVEL`。
3. `exposeSessionEnvironment && ctx`：
   - `env.KIANA_SESSION_ID = ctx.sessionManager.getSessionId()`；
   - `sessionFile = ctx.sessionManager.getSessionFile()`；非空 → `env.KIANA_SESSION_FILE = sessionFile`；
   - `model` 存在 → `env.KIANA_PROVIDER = model.provider`、`env.KIANA_MODEL = model.id`；
   - `ctx.thinkingLevel` 为真 → `env.KIANA_REASONING_LEVEL = ctx.thinkingLevel`。
4. `baseContext = {command, cwd, env}`；有 hook → 返回 `spawnHook(baseContext)`，否则 `baseContext`。

（`delete` 再按需设置：扩展的 spawnHook 看不到过期的 KIANA_* 值。）

#### 2.9.7 `createShellToolDefinition(cwd, config, options?)`

- `ops = options?.operations ?? createLocalBashOperations({shellPath: options?.shellPath})`；
- `commandPrefix`、`exposeSessionEnvironment = ?? true`、`spawnHook`；
- 描述（逐字，`${}` 已代入；`shellName` 为 `bash` 或 `PowerShell`）：
  ```
  Execute a {shellName} command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.
  ```
- `promptSnippet = config.promptSnippet`；
  `promptGuidelines = exposeSessionEnvironment && config.promptGuidelines ? [...config.promptGuidelines] : undefined`
  （关闭会话环境时**整个 guidelines 为 undefined**；测试 `keeps %s session-environment guidance conditional` 固定）。
- `parameters: bashSchema`、`outputSchema: bashOutputSchema`、
  `constrainedSampling: {type:"json_schema", strict:"prefer"}`。

#### 2.9.8 `execute` 流程（逐分支）

前置：
- `resolvedCommand = commandPrefix ? `${commandPrefix}\n${command}` : command`（**单个 `\n`**）。
- `spawnContext = resolveSpawnContext(resolvedCommand, ctx?.cwd || cwd, spawnHook, exposeSessionEnvironment, ctx)`。
- `output = new OutputAccumulator({tempFilePrefix: config.tempFilePrefix})`。
- `acceptingOutput = true`；`updateTimer`；`updateDirty = false`；`lastUpdateAt = 0`。

`emitOutputUpdate()`：`!onUpdate || !updateDirty` → 返回；`updateDirty = false`；`lastUpdateAt = Date.now()`；
`snapshot = output.snapshot({persistIfTruncated:true})`；
`onUpdate({content:[{type:"text", text: snapshot.content || ""}],
  details: {truncation: snapshot.truncation.truncated ? snapshot.truncation : undefined,
            fullOutputPath: snapshot.fullOutputPath}})`。

`scheduleOutputUpdate()`：`!onUpdate` → 返回；`updateDirty = true`；
`delay = 100 - (Date.now() - lastUpdateAt)`；`delay <= 0` → 清 timer 并立即 emit；
否则 `updateTimer ??= setTimeout(() => { updateTimer = undefined; emitOutputUpdate() }, delay)`。

`if (onUpdate) onUpdate({content: [], details: undefined})`——**首个更新是空内容**（测试
`bash execute emits an initial empty partial update before output arrives` 固定）。

`handleData(data)`：`!acceptingOutput` → 返回；`output.append(data)`；`scheduleOutputUpdate()`。

`finishOutput()`（async）：`acceptingOutput = false`；`output.finish()`；清 timer；`emitOutputUpdate()`；
`snapshot = output.snapshot({persistIfTruncated:true})`；`await output.closeTempFile()`；返回 snapshot。

`formatOutput(snapshot, emptyText = "(no output)")`：
- `text = snapshot.content || emptyText`；`details = undefined`；
- 若 `truncation.truncated`：
  - `details = {truncation, fullOutputPath: snapshot.fullOutputPath}`；
  - `startLine = truncation.totalLines - truncation.outputLines + 1`；`endLine = truncation.totalLines`；
  - `lastLinePartial` → 追加
    `` `\n\n[Showing last ${formatSize(truncation.outputBytes)} of line ${endLine} (line is ${lastLineSize}). Full output: ${snapshot.fullOutputPath}]` ``
    其中 `lastLineSize = formatSize(output.getLastLineBytes())`；
  - `truncatedBy === "lines"` → 追加
    `` `\n\n[Showing lines ${startLine}-${endLine} of ${truncation.totalLines}. Full output: ${snapshot.fullOutputPath}]` ``；
  - 否则 → 追加
    `` `\n\n[Showing lines ${startLine}-${endLine} of ${truncation.totalLines} (50.0KB limit). Full output: ${snapshot.fullOutputPath}]` ``
    （`formatSize(DEFAULT_MAX_BYTES)`）；
- 返回 `{text, details}`。

`appendStatus(text, status) = text ? `${text}\n\n${status}` : status`。

主流程（`startedAt = performance.now()`）：
1. try：
   1. 内层 try：
      `result = await ops.exec(spawnContext.command, spawnContext.cwd, {onData: handleData, signal, timeout, env: spawnContext.env})`；
      `exitCode = result.exitCode`。
      内层 catch（err）：
      - `snapshot = await finishOutput()`；`{text} = formatOutput(snapshot, "")`（空输出时 `text` 为空串，
        因此 `appendStatus` 只给状态行）；
      - `err instanceof Error && err.message === "aborted"` → 抛 `new Error(appendStatus(text, "Command aborted"))`；
      - `err.message.startsWith("timeout:")` → `timeoutSecs = err.message.split(":")[1]`；
        抛 `new Error(appendStatus(text, `Command timed out after ${timeoutSecs} seconds`))`；
      - 否则原样抛（例如 spawn ENOENT 的错误对象）。
   2. `snapshot = await finishOutput()`；`{text: outputText, details} = formatOutput(snapshot)`。
   3. `exitCode === null` → 抛 `new Error(appendStatus(outputText, "Command terminated without an exit code"))`。
   4. `wallTimeSeconds = Math.round((performance.now() - startedAt) / 100) / 10`（**先除以 100 取整再除以 10**，
      即 0.1 秒精度；`Math.round` 对正数的 .5 向 +∞ 取整，得到一位小数）。
   5. `fullOutput = await output.readFullOutput(STRUCTURED_OUTPUT_MAX_BYTES)`；
      `structuredContent = {output: fullOutput.content, truncated: fullOutput.truncated,
        ...(fullOutput.truncated && snapshot.fullOutputPath ? {full_output_path: snapshot.fullOutputPath} : {}),
        exit_code: exitCode, wall_time_seconds: wallTimeSeconds}`（键顺序 output/truncated/[full_output_path]/exit_code/wall_time_seconds）。
   6. `exitCode !== 0` → 返回 `{content:[{type:"text", text: appendStatus(outputText, `Command exited with code ${exitCode}`)}],
      details, structuredContent, isError: true}`。
   7. 否则返回 `{content:[{type:"text", text: outputText}], details, structuredContent}`（**无 isError**）。
2. finally：`clearUpdateTimer()`。

#### 2.9.9 bash 配置与工厂

```ts
const bashToolConfig: ShellToolConfig = {
  name: "bash", label: "bash", shellName: "bash", prompt: "$",
  promptSnippet: bashToolSystemPromptContribution.snippet,
  promptGuidelines: bashToolSystemPromptContribution.guidelines,
  tempFilePrefix: "kiana-bash",
};
```

`createBashToolDefinition(cwd, options?)` = `createShellToolDefinition(cwd, bashToolConfig, options)`；
`createBashTool(cwd, options?)` = `wrapToolDefinition(definition)` 并 `Object.assign(tool,
{promptSnippet: definition.promptSnippet, promptGuidelines: definition.promptGuidelines})`
（AgentTool 上没有这两个字段，但运行时**附加**它们，供系统提示构建读取）。

定义对象尾部 `...createShellRenderers(config.prompt)`——渲染器在 `bash.ts` 里展开，
`prompt` 为 `"$"`（bash）或 `"PS>"`（powershell）。

### 2.10 `src/core/tools/powershell.ts`

- `const UTF8_OUTPUT_PREFIX = "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";`
  （结尾有换行）。
- 系统提示：`snippet: "Execute PowerShell commands"`，guidelines 与 bash **相同**。
- 类型别名：`PowerShellOperations = BashOperations`、`PowerShellSpawnContext = BashSpawnContext`、
  `PowerShellSpawnHook = BashSpawnHook`、`PowerShellToolDetails = BashToolDetails`、
  `PowerShellToolInput = BashToolInput`。
- `PowerShellToolOptions extends Pick<BashToolOptions, "operations" | "exposeSessionEnvironment" | "spawnHook">`
  （**没有** `commandPrefix`、`shellPath`）。
- `createLocalPowerShellOperations()`：包 `createLocalShellOperations("PowerShell", getPowerShellConfig)`，
  `exec` 前把 command 变成 `${UTF8_OUTPUT_PREFIX}${command}`。
- 配置：`{name:"powershell", label:"powershell", shellName:"PowerShell", prompt:"PS>",
  promptSnippet, promptGuidelines, tempFilePrefix:"kiana-powershell"}`。
- `createPowerShellToolDefinition(cwd, options?)`：默认 `operations ?? createLocalPowerShellOperations()`。
- `createPowerShellTool(cwd, options?)`：wrap + 附加 promptSnippet/promptGuidelines。
- 平台限制：`getPowerShellConfig()` 在非 win32 抛
  `new Error("The powershell tool is only available on Windows.")`（见 2.31）。

### 2.11 `src/core/tools/grep.ts`

#### 2.11.1 schema（逐字）

```ts
pattern:    String  "Search pattern (regex or literal string)"
path:       Optional String "Directory or file to search (default: current directory)"
glob:       Optional String "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"
ignoreCase: Optional Boolean "Case-insensitive search (default: false)"
literal:    Optional Boolean "Treat pattern as literal string instead of regex (default: false)"
context:    Optional Number "Number of lines to show before and after each match (default: 0)"
limit:      Optional Number "Maximum number of matches to return (default: 100)"
```
必填 `["pattern"]`。

- 系统提示：`snippet: "Search file contents for patterns (respects .gitignore)"`，`guidelines: []`。
- `const DEFAULT_LIMIT = 100;`
- 描述（逐字）：
  ```
  Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.
  ```
- `GrepToolDetails { truncation?: TruncationResult; matchLimitReached?: number; linesTruncated?: boolean; }`
- `GrepOperations { isDirectory(abs): Promise<boolean>|boolean; readFile(abs): Promise<string>|string }`；
  默认 `fs.stat(p).isDirectory()` 与 `fs.readFile(p,"utf-8")`。

#### 2.11.2 `execute` 流程

Promise 包装；`signal?.aborted` → reject `Error("Operation aborted")`；`settled` 单次结算保护。

1. `rgPath = await ensureTool("rg")`；`!rgPath` → reject
   `new Error("ripgrep (rg) is not available and could not be downloaded")`。
2. `searchPath = resolveToCwd(searchDir || ".", ctx?.cwd || cwd)`；`ops = customOps ?? default`。
3. `ops.isDirectory(searchPath)` 抛错 → reject `` new Error(`Path not found: ${searchPath}`) ``。
4. `contextValue = context && context > 0 ? context : 0`；
   `effectiveLimit = Math.max(1, limit ?? 100)`。
5. `formatPath(filePath)`：若 `isDirectory`：`relative = path.relative(searchPath, filePath)`；
   `relative && !relative.startsWith("..")` → `relative.replace(/\\/g,"/")`；否则 `path.basename(filePath)`。
6. `fileCache: Map<string,string[]>`；`getFileLines(p)`：命中返回；否则
   `content.replace(/\r\n/g,"\n").replace(/\r/g,"\n").split("\n")`；读失败 → `[]`；写缓存。
7. `args = ["--no-config","--json","--line-number","--color=never","--hidden"]`；
   `ignoreCase` → push `--ignore-case`；`literal` → `--fixed-strings`；`glob` → push `--glob`, glob；
   push `--`, pattern, searchPath。
8. `spawn(rgPath, args, {stdio:["ignore","pipe","pipe"]})`；`createInterface({input: child.stdout})`。
9. 状态：`stderr=""`、`matchCount=0`、`matchLimitReached=false`、`linesTruncated=false`、
   `aborted=false`、`killedDueToLimit=false`、`outputLines: string[]`。
   `cleanup()`：`rl.close()` + 移除 abort 监听。
   `stopChild(dueToLimit=false)`：`!child.killed` → 记录 `killedDueToLimit` 并 `child.kill()`。
   `onAbort()`：`aborted = true`；`stopChild()`。
   `child.stderr.on("data", chunk => stderr += chunk.toString())`。
10. `formatBlock(filePath, lineNumber)`（async）：
    `relativePath = formatPath(filePath)`；`lines = await getFileLines(filePath)`；
    `!lines.length` → `[`${relativePath}:${lineNumber}: (unable to read file)`]`；
    `start = contextValue > 0 ? Math.max(1, lineNumber - contextValue) : lineNumber`；
    `end = contextValue > 0 ? Math.min(lines.length, lineNumber + contextValue) : lineNumber`；
    `for current in start..=end`：`lineText = lines[current-1] ?? ""`；`sanitized = lineText.replace(/\r/g,"")`；
    `{text, wasTruncated} = truncateLine(sanitized)`（500 字符）；`wasTruncated` → `linesTruncated = true`；
    match 行 push `` `${relativePath}:${current}: ${text}` ``，否则 `` `${relativePath}-${current}- ${text}` ``。
11. `matches: {filePath, lineNumber, lineText?}[]`；`rl.on("line")`：
    `!line.trim() || matchCount >= effectiveLimit` → 忽略；`JSON.parse` 失败 → 忽略；
    `event.type === "match"` → `matchCount++`；取 `event.data?.path?.text`、`event.data?.line_number`、
    `event.data?.lines?.text`；`filePath && typeof lineNumber === "number"` → push；
    `matchCount >= effectiveLimit` → `matchLimitReached = true`；`stopChild(true)`。
12. `child.on("error", e)` → cleanup + reject `` new Error(`Failed to run ripgrep: ${e.message}`) ``。
13. `child.on("close", async code)`：
    cleanup；
    - `aborted` → reject `Error("Operation aborted")`；
    - `!killedDueToLimit && code !== 0 && code !== 1` → reject `stderr.trim() || `ripgrep exited with code ${code}``；
    - `matchCount === 0` → resolve `{content:[{type:"text", text:"No matches found"}], details: undefined}`；
    - 逐 match 格式化：
      - `contextValue === 0 && match.lineText !== undefined`：`sanitized = lineText.replace(/\r\n/g,"\n")
        .replace(/\r/g,"").replace(/\n$/,"")`；`truncateLine`（记录 linesTruncated）；
        push `` `${relativePath}:${lineNumber}: ${truncatedText}` ``；
      - 否则 `formatBlock(...)` 展开 push。
    - `rawOutput = outputLines.join("\n")`；`truncation = truncateHead(rawOutput, {maxLines: Number.MAX_SAFE_INTEGER})`
      （只做字节限制）；`output = truncation.content`；`details: GrepToolDetails = {}`；
    - notices 顺序固定：
      1. `matchLimitReached` → `` `${effectiveLimit} matches limit reached. Use limit=${effectiveLimit*2} for more, or refine pattern` ``，
         `details.matchLimitReached = effectiveLimit`；
      2. `truncation.truncated` → `` `${formatSize(DEFAULT_MAX_BYTES)} limit reached` ``（`50.0KB limit reached`），
         `details.truncation = truncation`；
      3. `linesTruncated` → `` `Some lines truncated to ${GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines` ``，
         `details.linesTruncated = true`；
    - `notices.length > 0` → `output += `\n\n[${notices.join(". ")}]``；
    - resolve `{content:[{type:"text", text: output}], details: Object.keys(details).length > 0 ? details : undefined}`。

注意：`--no-config` 是硬要求（测试 `should ignore RIPGREP_CONFIG_PATH`）；`--` 分隔符保证
`-foo` 形式的 pattern 被当作搜索文本（测试 `should treat flag-like patterns as search text`）。

### 2.12 `src/core/tools/find.ts`

#### 2.12.1 `relativizeFindResultPath(resultPath, searchPath, pathModule = path)`（**导出**）

- `hadTrailingSeparator = resultPath.endsWith(pathModule.sep) || (pathModule.sep === "\\" && resultPath.endsWith("/"))`。
- `relativePath = pathModule.isAbsolute(resultPath) ? pathModule.relative(searchPath, resultPath) : resultPath`。
- `posixPath = relativePath.split(pathModule.sep).join("/")`。
- `hadTrailingSeparator && !posixPath.endsWith("/")` → `` `${posixPath}/` ``；否则 `posixPath`。
- 测试覆盖：`3302-find-path-glob`、`6104-find-root-relativization`。
  例：`relative(searchPath, searchPath)` 为 `""`（搜索根自身）→ 保持 `""`（不臆造 `./`）；
  `relative` 对根目录结果为 `""`，`posixPath` 为空 → 无尾斜杠时返回 `""`。
- Rust：需实现「Windows 分隔符语义」的可替换 `pathModule`（测试会传入 `path.win32`）。

#### 2.12.2 schema / 提示 / 描述

```ts
pattern: String  "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"
path:    Optional String "Directory to search in (default: current directory)"
limit:   Optional Number "Maximum number of results (default: 1000)"
```
`snippet: "Find files by glob pattern (respects .gitignore)"`、`guidelines: []`、`DEFAULT_LIMIT = 1000`。
描述（逐字）：
```
Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).
```
`FindToolDetails { truncation?: TruncationResult; resultLimitReached?: number; }`
`FindOperations { exists(abs): Promise<boolean>|boolean; glob(pattern, cwd, {ignore: string[]; limit: number}): Promise<string[]>|string[] }`。
默认操作：`exists: pathExists`，`glob: () => []`（占位；实际 fd 路径在 `execute` 内）。
**注意**：分支条件是 `customOps?.glob`（看的是**调用方传入的** `customOps`，不是 `ops`），
所以默认占位 `glob` **永远不会被调用**。Rust 侧不要为了「统一接口」把默认实现换成真实 glob
（那会让默认 find 永远返回空结果）。

#### 2.12.3 `execute`

Promise 包装；`signal?.aborted` → reject `Error("Operation aborted")`；`settled` 保护；
`stopChild`；`onAbort = () => { stopChild?.(); settle(reject(Error("Operation aborted"))) }`；
`settle` 会移除 abort 监听并把 `stopChild` 置 undefined。

1. `searchPath = resolveToCwd(searchDir || ".", ctx?.cwd || cwd)`；`effectiveLimit = limit ?? 1000`；
   `ops = customOps ?? default`。
2. **自定义 glob 分支**（`customOps?.glob` 存在）：
   - `!(await ops.exists(searchPath))` → reject `` `Path not found: ${searchPath}` ``；
   - `signal?.aborted` → reject `"Operation aborted"`；
   - `results = await ops.glob(pattern, searchPath, {ignore: ["**/node_modules/**","**/.git/**"], limit: effectiveLimit})`；
   - aborted 再查；`results.length === 0` → resolve `"No files found matching pattern"`；
   - `relativized = results.map(p => relativizeFindResultPath(p, searchPath))`；
     `resultLimitReached = relativized.length >= effectiveLimit`；
     `truncation = truncateHead(join("\n"), {maxLines: MAX_SAFE_INTEGER})`；
     notices：`resultLimitReached` → `` `${effectiveLimit} results limit reached` ``（自定义路径**无** "Use limit=" 后缀），
     `truncated` → `` `${formatSize(DEFAULT_MAX_BYTES)} limit reached` ``；
     `output += `\n\n[${notices.join(". ")}]``；
     details `{resultLimitReached?, truncation?}` 或 undefined。
3. **默认 fd 分支**：
   - `fdPath = await ensureTool("fd")`；aborted 检查；`!fdPath` → reject
     `"fd is not available and could not be downloaded"`。
   - `args = ["--glob","--color=never","--hidden"]`。
   - **git 仓库探测**：从 `searchPath` 向上，逐级 `pathExists(path.join(current, ".git"))`，命中 → `insideGitRepo = true`；
     `parent === current`（根）→ break。`!insideGitRepo` → push `--no-require-git`。
   - push `--max-results`, `String(effectiveLimit)`。
   - `effectivePattern = pattern`；`pattern.includes("/")`：
     `args.push("--full-path")`；`!pattern.startsWith("/") && !pattern.startsWith("**/") && pattern !== "**"`
     → `effectivePattern = `**/${pattern}``；
     `process.platform === "win32"` → `effectivePattern = effectivePattern.replaceAll("/", String.raw`[/\\]`)`
     （即把每个 `/` 换成字符类 `[/\\]`）。
   - `args.push("--", effectivePattern, searchPath)`。
   - `spawn(fdPath, args, {stdio:["ignore","pipe","pipe"]})`；readline；`stderr` 累积；`lines` 收集（原样，去 `\r` 步骤在后面）。
   - `child.on("error")` → reject `` `Failed to run fd: ${e.message}` ``。
   - `child.on("close", code)`：
     - `signal?.aborted` → reject `"Operation aborted"`；
     - `output = lines.join("\n")`；
     - `code !== 0` → `errorMsg = stderr.trim() || `fd exited with code ${code}``；**若无 output** → reject errorMsg
       （有 output 则容忍非零码继续，用于「部分结果 + 权限错误」）；
     - `!output` → resolve `"No files found matching pattern"`；
     - 逐行：`line = rawLine.replace(/\r$/,"").trim()`；空则跳过；push `relativizeFindResultPath(line, searchPath)`；
     - `resultLimitReached = relativized.length >= effectiveLimit`；`truncateHead(..., MAX_SAFE_INTEGER)`；
     - notices：`resultLimitReached` → `` `${effectiveLimit} results limit reached. Use limit=${effectiveLimit*2} for more, or refine pattern` ``
       （fd 路径**有** "Use limit=" 后缀），`truncated` → `` `${formatSize(DEFAULT_MAX_BYTES)} limit reached` ``。
4. 外层 catch：若 `signal?.aborted` → reject `"Operation aborted"`；否则 `e instanceof Error ? e : new Error(String(e))`。

### 2.13 `src/core/tools/ls.ts`

- schema：`path` `"Directory to list (default: current directory)"`、
  `limit` `"Maximum number of entries to return (default: 500)"`（都 Optional，无必填）。
- `snippet: "List directory contents"`、`guidelines: []`、`DEFAULT_LIMIT = 500`。
- 描述（逐字）：
  ```
  List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).
  ```
- `LsToolDetails { truncation?: TruncationResult; entryLimitReached?: number; }`
- `LsOperations { exists(abs); stat(abs): {isDirectory(): boolean}; readdir(abs): string[] }`；
  默认 `pathExists` / `fs.stat` / `fs.readdir`。
- `execute`：
  1. `signal?.aborted` → reject `"Operation aborted"`；注册 abort 监听（直接 reject）。
  2. `dirPath = resolveToCwd(path || ".", ctx?.cwd || cwd)`；`effectiveLimit = limit ?? 500`。
  3. `!(await ops.exists(dirPath))` → reject `` `Path not found: ${dirPath}` ``。
  4. `stat = await ops.stat(dirPath)`；`!stat.isDirectory()` → reject `` `Not a directory: ${dirPath}` ``。
  5. `ops.readdir(dirPath)` 抛错（`e: any`）→ reject `` `Cannot read directory: ${e.message}` ``（`undefined.message` → `Cannot read directory: undefined`）。
  6. `entries.sort((a,b) => a.toLowerCase().localeCompare(b.toLowerCase()))`（**大小写不敏感 + locale 比较**；
     注意 JS 的 `localeCompare` 默认 locale 相关，Rust 需 ICU 或明确退化策略，见 8.6）。
  7. 逐 entry：`results.length >= effectiveLimit` → `entryLimitReached = true` 并 break；
     `fullPath = path.join(dirPath, entry)`；`ops.stat(fullPath)` 抛错 → **跳过该 entry（continue）**；
     `isDirectory()` → 后缀 `"/"`；push `entry + suffix`。
  8. 移除 abort 监听；`results.length === 0` → resolve `"(empty directory)"`。
  9. `truncateHead(join("\n"), {maxLines: MAX_SAFE_INTEGER})`；
     notices：`entryLimitReached` → `` `${effectiveLimit} entries limit reached. Use limit=${effectiveLimit*2} for more` ``，
     `truncated` → `` `${formatSize(DEFAULT_MAX_BYTES)} limit reached` ``；`output += `\n\n[...]``。
  10. catch 中移除 abort 监听并 reject 原错误。

### 2.14 `src/core/tools/index.ts`

导出（逐项）：

- 类型与值：`BashOperations`/`BashSpawnContext`/`BashSpawnHook`/`BashToolDetails`/`BashToolInput`/
  `BashToolOptions`/`createBashTool`/`createBashToolDefinition`/`createLocalBashOperations`；
  `createEditTool`/`createEditToolDefinition`/`EditOperations`/`EditToolDetails`/`EditToolInput`/`EditToolOptions`；
  `withFileMutationQueue`；
  `createFindTool`/`createFindToolDefinition`/`FindOperations`/`FindToolDetails`/`FindToolInput`/`FindToolOptions`；
  `createGrepTool`/`createGrepToolDefinition`/`GrepOperations`/`GrepToolDetails`/`GrepToolInput`/`GrepToolOptions`；
  `createLsTool`/`createLsToolDefinition`/`LsOperations`/`LsToolDetails`/`LsToolInput`/`LsToolOptions`；
  `createLocalPowerShellOperations`/`createPowerShellTool`/`createPowerShellToolDefinition`/`PowerShellOperations`/
  `PowerShellSpawnContext`/`PowerShellSpawnHook`/`PowerShellToolDetails`/`PowerShellToolInput`/`PowerShellToolOptions`；
  `createReadTool`/`createReadToolDefinition`/`ReadOperations`/`ReadToolDetails`/`ReadToolInput`/`ReadToolOptions`；
  `DEFAULT_MAX_BYTES`/`DEFAULT_MAX_LINES`/`formatSize`/`TruncationOptions`/`TruncationResult`/`truncateHead`/
  `truncateLine`/`truncateTail`；
  `createWriteTool`/`createWriteToolDefinition`/`WriteOperations`/`WriteToolInput`/`WriteToolOptions`。

- `export type Tool = AgentTool<any>; export type ToolDef = ToolDefinition<any, any>;`
- `export type ToolName = "read" | "bash" | "powershell" | "edit" | "write" | "grep" | "find" | "ls";`
- `export const allToolNames: Set<ToolName>`，内容顺序：read, bash, powershell, edit, write, grep, find, ls。
- `export interface ToolsOptions { read?, bash?, powershell?, write?, edit?, grep?, find?, ls? }`（各自对应 Options）。
- `createToolDefinition(toolName, cwd, options?)`：switch 到 8 个 `create*ToolDefinition`；
  未知名字 → `throw new Error(`Unknown tool name: ${toolName}`)`（default 分支，类型上不可达）。
- `createTool(toolName, cwd, options?)`：同上，返回 AgentTool；同样的未知名字错误。
- `createCodingToolDefinitions(cwd, options?)` → `[read, bash, edit, write]`。
- `createReadOnlyToolDefinitions(cwd, options?)` → `[read, grep, find, ls]`。
- `createAllToolDefinitions(cwd, options?)` → `Record<ToolName, ToolDef>`，键顺序 read/bash/powershell/edit/write/grep/find/ls。
- `createCodingTools(cwd, options?)` → `[read, bash, edit, write]`。
- `createReadOnlyTools(cwd, options?)` → `[read, grep, find, ls]`。
- `createAllTools(cwd, options?)` → `Record<ToolName, Tool>`（键顺序同上）。

**已知不一致（移植必须照抄）**：`createCodingToolDefinitions`/`createReadOnlyToolDefinitions`/
`createCodingTools`/`createReadOnlyTools` **不含 powershell**，而 `createToolDefinition`/`createTool`/
`createAllToolDefinitions`/`createAllTools` 含。测试 `tool-renderer-examples` 与
`default-tools-setting`（`can select powershell instead of bash`）依赖此行为。

### 2.15 `src/core/tools/tool-definition-wrapper.ts`

- `export type ToolContextFactory = (toolCallId: string, signal: AbortSignal | undefined) => ExtensionToolContext;`
- `wrapToolDefinition(definition, ctxFactory?) -> AgentTool`：复制 `name`、`label`、`description`、
  `parameters`、`outputSchema`、`constrainedSampling`、`prepareArguments`、`executionMode`；
  `execute: (toolCallId, params, signal, onUpdate, ctx?) =>
     definition.execute(toolCallId, params, signal, onUpdate, ctx ?? ctxFactory?.(toolCallId, signal) as ExtensionToolContext)`。
- `wrapToolDefinitions(definitions, ctxFactory?)` → map。
- `createToolDefinitionFromAgentTool(tool)` → 合成最小定义：复制
  `name/label/description/parameters/outputSchema/constrainedSampling/prepareArguments/executionMode`；
  `execute: async (toolCallId, params, signal, onUpdate) => tool.execute(toolCallId, params, signal, onUpdate)`；
  **不复制渲染器与 prompt 元数据**（在 HTML 导出/UI 上退化为内置渲染器或 fallback）。

Rust 设计：`ToolDefinition` 用 `Arc<dyn ...>` 或 enum 承载 `execute`（`Box<dyn Fn(...) -> BoxFuture>`），
`AgentTool` 是它的「瘦视图」；`wrap` 与 `from_agent_tool` 分别做字段投影（丢字段是**语义要求**，
不是实现细节）。

### 2.16 `src/core/tools/render-utils.ts`（TUI 渲染共享工具）

逐函数（都是 `export`，除 `parseLineNumber`）：

- `shortenPath(path: unknown): string`：非 string → `""`；以 `os.homedir()` 开头 → `` `~${path.slice(home.length)}` ``；
  否则原样。
- `linkPath(styledText, rawPath, cwd): string`：`!getCapabilities().hyperlinks` → `styledText`；
  否则 `hyperlink(styledText, pathToFileURL(resolvePath(rawPath, cwd)).href)`（OSC 8 超链接）。
- `str(value: unknown): string | null`：string → 本身；`== null`（null/undefined）→ `""`；
  其他（number/object/boolean）→ `null`。**这是「参数类型错误」的判定入口**：
  `null` 表示「有值但类型不对」→ 渲染成 `[invalid arg]`。
- `parseLineNumber(value)`：string 且 `trim() !== ""` → `Number(value)`；
  `typeof === "number" && Number.isFinite` → 返回，否则 `undefined`。
- `formatReadLineRange(offset, limit): string`：
  - `offset == null && limit == null` → `""`；
  - `start = offset == null ? 1 : parseLineNumber(offset)`；`undefined` → `""`；
  - `limit == null` → `` `:${start}` ``；
  - `count = parseLineNumber(limit)`；`undefined` → `""`；
  - `end = start + count - 1`；返回 `` `:${start}${end ? `-${end}` : ""}` ``（`end === 0` 时省略 `-0`）。
- `replaceTabs(text)`：`\t` → 三个空格。
- `normalizeDisplayText(text)`：删除所有 `\r`。
- `getTextOutput(result, showImages): string`：
  - 无 result → `""`；
  - text 块：`sanitizeBinaryOutput(stripAnsi(text || "")).replace(/\r/g,"")`，用 `"\n"` 连接；
  - `caps = getCapabilities()`；image 块存在且（`!caps.images || !showImages`）→ 每个 image 生成
    `imageFallback(mimeType ?? "image/unknown", dims)`（`dims` 由 `getImageDimensions(data, mimeType)` 得到，
    失败为 undefined），`"\n"` 连接后追加到文本（文本非空时用 `\n` 分隔）。
- `formatToolCallWithArgs(title, args, theme, expanded): string`（通用 fallback 头部）：
  - `header = theme.fg("toolTitle", theme.bold(title))`；`args == null` → header；
  - entries：对象且非数组 → `Object.entries(args)`；否则 `[["args", args]]`；空 → header；
  - **expanded**：每行 `` `  ${key}: ${text}` ``，`text` 为 string 原样，否则 `JSON.stringify(value, null, 2) ?? String(value)`；
    再 `replaceTabs(...)`、删除 `\r`、多行缩进 `    `（四个空格，`\n` → `\n    `）；整体 `theme.fg("muted", lines.join("\n"))`，
    格式 `` `${header}\n${muted}` ``；
  - **collapsed**：`pairs = entries.map(([k,v]) => `${k}=${JSON.stringify(v) ?? String(v)}`).join(" ")`；
    `pairs.length > 100` → `` `${pairs.slice(0,97)}...` ``（`COLLAPSED_ARGS_CHARS - 3 = 97`）；
    格式 `` `${header} ${theme.fg("muted", preview)}` ``。
- `formatToolDuration(ms): string`：`seconds = ms/1000`；`< 60` → `` `${seconds.toFixed(1)}s` ``；
  否则 `totalSeconds = floor(seconds)`、`minutes = floor(totalSeconds/60)`、`remainder = totalSeconds%60`；
  `minutes < 60` → `` `${minutes}m ${remainder}s` ``；否则
  `` `${floor(minutes/60)}h ${minutes%60}m ${remainder}s` ``。
  **注意 `< 60s` 分支的 `toFixed(1)` 同样有中点问题**（见 2.1.4/8.2 #7）：
  `1250ms` → `1.25` → JS `"1.3s"`，Rust `{:.1}` 会得到 `"1.2s"`。必须复用同一个 JS 风格格式化函数。
- `startToolTimer(context)`：`context.executionStarted && state.startedAt === undefined` →
  `state.startedAt = Date.now()`、`state.endedAt = undefined`。
- `toolTimerText(context, isPartial): string | undefined`：
  - `startedAt !== undefined && isPartial && !state.interval` → `state.interval = setInterval(() => context.invalidate(), 1000)`；
  - `!isPartial || context.isError` → `state.endedAt ??= Date.now()`；清 interval；
  - `!isPartial && context.durationMs !== undefined` → `` `Took ${formatToolDuration(durationMs)}` ``；
  - `startedAt === undefined` → `undefined`；
  - 否则 `` `${isPartial ? "Elapsed" : "Took"} ${formatToolDuration(endTime - startedAt)}` ``。
- `invalidArgText(theme)` → `theme.fg("error", "[invalid arg]")`。
- 类型 `ToolRenderResultLike<TDetails> = { content: (TextContent | ImageContent)[]; details: TDetails }`
  （导出；渲染器接收的 result 形状，`details` 可能为 undefined 的调用方需自行放宽）。
- `renderToolPath(rawPath, theme, cwd, options?): string`：
  - `rawPath === null` → `invalidArgText`；
  - `value = rawPath || options?.emptyFallback`；`!value` → `theme.fg("toolOutput", "...")`；
  - 否则 `linkPath(theme.fg("accent", shortenPath(value)), value, cwd)`。

### 2.17 `src/core/tools/renderers/*`

#### 2.17.1 `renderers/bash.ts`

- `const BASH_PREVIEW_LINES = 5; export const BASH_UPDATE_THROTTLE_MS = 100;`（后者被 `bash.ts` 导入用于节流，
  模块依赖方向是 bash.ts → renderers/bash.ts）。
- `formatShellCall(args, prompt)`：
  `command = str(args?.command)`；`timeout = args?.timeout`；
  `timeoutSuffix = timeout ? theme.fg("muted", ` (timeout ${timeout}s)`) : ""`；
  `commandDisplay = command === null ? invalidArgText(theme) : command ? command : theme.fg("toolOutput", "...")`；
  返回 `theme.fg("toolTitle", theme.bold(`${prompt} ${commandDisplay}`)) + timeoutSuffix`。
- `rebuildBashResultRenderComponent(component, result, options, showImages, timerText)`：
  1. `component.clear()`；`output = getTextOutput(result, showImages).trim()`；
  2. `truncation = result.details?.truncation`；`fullOutputPath = result.details?.fullOutputPath`；
  3. **最终结果去重脚注**：`!options.isPartial && truncation?.truncated && fullOutputPath && output.endsWith("]")` →
     `footerStart = output.lastIndexOf("\n\n[")`；找到且 `output.slice(footerStart).includes(fullOutputPath)`
     → `output = output.slice(0, footerStart).trimEnd()`（避免与下面的 warning 行重复；
     测试 `bash renderer does not duplicate final full output truncation details`）；
  4. `output` 非空：逐行 `theme.fg("toolOutput", line)` 后用 `\n` 连接；
     - `options.expanded` → `new Text(`\n${styledOutput}`, 0, 0)`；
     - 否则 `Spacer(1)` + `VisualLinePreview({text: styledOutput, maxVisualLines: 5, keep: "end",
       formatHint: hidden => theme.fg("muted", `... (${hidden} earlier lines,`) + ` ` +
         keyHint("app.tools.expand", "to expand") + theme.fg("muted", ")")})`；
  5. `truncation?.truncated || fullOutputPath` → warnings 数组：
     `fullOutputPath` → `` `Full output: ${fullOutputPath}` ``；
     `truncation.truncated`：`truncatedBy === "lines"` → `` `Truncated: showing ${outputLines} of ${totalLines} lines` ``；
     否则 `` `Truncated: ${outputLines} lines shown (${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit)` ``；
     最终 `new Text(`\n${theme.fg("warning", `[${warnings.join(". ")}]`)}`, 0, 0)`；
  6. `timerText` → `new Text(`\n${theme.fg("muted", timerText)}`, 0, 0)`。
- `createShellRenderers(prompt)` → `{renderCall, renderResult}`：
  - `renderCall(args, theme, context)`：`startToolTimer(context)`；复用 `context.lastComponent`（Text）
    或新建 `Text("",0,0)`；`setText(formatShellCall(...))`；返回。
  - `renderResult(result, options, theme, context)`：`timerText = toolTimerText(context, options.isPartial)`；
    复用 `Container` 或新建；`rebuildBashResultRenderComponent(...)`；`component.invalidate()`；返回。
- `createAllToolRenderers()` 中 bash 用 `createShellRenderers("$")`，powershell 用 `"PS>"`。

#### 2.17.2 `renderers/read.ts`

- `COMPACT_RESOURCE_FILE_NAMES = new Set(["AGENTS.override.md","AGENTS.md","AGENTS.MD","CLAUDE.md","CLAUDE.MD"])`。
- `styledReadLineRange(args, theme)`：`formatReadLineRange(args?.offset, args?.limit)`，非空 →
  `theme.fg("warning", range)`。
- `formatReadCall(args, theme, cwd)`：`` `${theme.fg("toolTitle", theme.bold("read"))} ${renderToolPath(str(args?.file_path ?? args?.path), theme, cwd)}${styledReadLineRange(...)}` ``
  （**`file_path` 优先于 `path`**，兼容旧参数名；测试 `preserves legacy file_path rendering compatibility`）。
- `trimTrailingEmptyLines(lines)`：去掉尾部空串。
- `getKianaDocsClassification(absolutePath)`：`packageRoot = dirname(getReadmePath())`；
  `relativePath = relative(resolve(packageRoot), resolve(absolutePath))`；
  越界（`""`、`".."`、以 `..${sep}` 开头、绝对）→ undefined；
  `label = toPosixPath(relativePath)`；`label === "README.md" || label.startsWith("docs/") || label.startsWith("examples/")`
  → `{kind:"docs", label}`。
- `getCompactReadClassification(args, cwd)`：`rawPath = str(args?.file_path ?? args?.path)`；空 → undefined；
  `absolutePath = resolveToCwd(rawPath, cwd)`；`fileName = basename`；
  `SKILL.md` → `{kind:"skill", label: basename(dirname(absolutePath)) || fileName}`；
  否则 docs 分类；否则 `COMPACT_RESOURCE_FILE_NAMES.has(fileName)` →
  `{kind:"resource", label: formatPathRelativeToCwdOrAbsolute(absolutePath, cwd)}`；否则 undefined。
- `formatCompactReadCall(classification, args, theme)`：
  `expandHint = theme.fg("dim", ` (${keyText("app.tools.expand")} to expand)`)`；
  - skill：`theme.fg("customMessageLabel", `\x1b[1m[skill]\x1b[22m `) + theme.fg("customMessageText", label) +
    styledReadLineRange + expandHint`（注意字面 ANSI 加粗/取消加粗包裹 `[skill]`）；
  - 其他：`` theme.fg("toolTitle", theme.bold(`read ${kind}`)) + " " + theme.fg("accent", label) + range + hint ``
    （即 `read docs` / `read resource`）。
- `formatReadResult(args, result, options, theme, showImages, _cwd, isError)`：
  - `!options.expanded && !isError` → `""`（折叠时**不显示结果**）；
  - `rawPath`；`output = getTextOutput(...)`；`lang = !isError && rawPath ? getLanguageFromPath(rawPath) : undefined`；
    `renderedLines = lang ? highlightCode(replaceTabs(output), lang) : output.split("\n")`；
    `lines = trimTrailingEmptyLines(renderedLines)`；`maxLines = expanded ? lines.length : 10`；
    `displayLines = lines.slice(0, maxLines)`；`remaining = lines.length - maxLines`；
  - 文本起始为 `\n` + 每行（有 lang 时 `replaceTabs(line)` 原样；否则 `theme.fg("toolOutput", replaceTabs(line))`）以 `\n` 连接；
  - `remaining > 0` → 追加 `` theme.fg("muted", `\n... (${remaining} more lines,`) + " " + keyHint("app.tools.expand","to expand") + theme.fg("muted", ")") ``；
  - 截断脚注（warning）：
    `firstLineExceedsLimit` → `` `[First line exceeds ${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit]` ``；
    `truncatedBy === "lines"` → `` `[Truncated: showing ${outputLines} of ${totalLines} lines (${maxLines ?? DEFAULT_MAX_LINES} line limit)]` ``；
    否则 `` `[Truncated: ${outputLines} lines shown (${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit)]` ``。
- `readRenderers.renderCall`：`classification = !context.expanded ? getCompactReadClassification(args, context.cwd) : undefined`；
  有分类 → `formatCompactReadCall`，否则 `formatReadCall`。
- `readRenderers.renderResult`：调用上面的 `formatReadResult`。

#### 2.17.3 `renderers/write.ts`

- `WRITE_PARTIAL_FULL_HIGHLIGHT_LINES = 50`。
- `WriteCallRenderComponent extends Text`，附加 `cache?: WriteHighlightCache`。
- `highlightSingleLine(line, lang)` = `highlightCode(line, lang)[0] ?? ""`。
- `refreshWriteHighlightPrefix(cache)`：`prefixCount = min(50, normalizedLines.length)`；0 → 返回；
  对前 `prefixCount` 行整体 `highlightCode(join("\n"), lang)`，逐行回填（缺失则单行高亮）。
- `rebuildWriteHighlightCacheFull(rawPath, fileContent)`：无 lang（`getLanguageFromPath` 为 undefined）→ undefined；
  否则 `{rawPath, lang, rawContent: fileContent, normalizedLines: replaceTabs(normalizeDisplayText(fileContent)).split("\n"),
  highlightedLines: highlightCode(normalized, lang)}`。
- `updateWriteHighlightCacheIncremental(cache, rawPath, fileContent)`：
  无 lang → undefined；无 cache / lang 变 / path 变 / 新内容不以旧 rawContent 开头 → 重建；
  长度相同 → 返回 cache（未变）；否则计算 delta（原始）→ `normalizeDisplayText` → `replaceTabs`；
  更新 rawContent；若 normalizedLines 为空 push `""`；
  按 `\n` 切 delta：首段追加到最后一行并重新高亮该行；其余段 push 并单行高亮；最后 `refreshWriteHighlightPrefix`。
- `trimTrailingEmptyLines`（同上）。
- `formatWriteCall(args, options, theme, cache, cwd)`：
  - `rawPath = str(args?.file_path ?? args?.path)`；`fileContent = str(args?.content)`；
  - 头部 `` `${theme.fg("toolTitle", theme.bold("write"))} ${renderToolPath(rawPath, theme, cwd)}` ``；
  - `fileContent === null` → 追加 `` `\n\n${theme.fg("error", "[invalid content arg - expected string]")}` ``；
  - 非空：`lang`；`renderedLines = lang ? (cache?.highlightedLines ?? highlightCode(replaceTabs(normalizeDisplayText(fileContent)), lang))
    : normalizeDisplayText(fileContent).split("\n")`；`lines = trimTrailingEmptyLines`；`totalLines = lines.length`；
    `maxLines = expanded ? lines.length : 10`；`displayLines = lines.slice(0, maxLines)`；`remaining = lines.length - maxLines`；
    追加 `\n\n` + 每行（lang 时原样（已高亮）；否则 `theme.fg("toolOutput", replaceTabs(line))`）`\n` 连接；
    `remaining > 0` → 追加 `` theme.fg("muted", `\n... (${remaining} more lines, ${totalLines} total,`) + " " +
      keyHint("app.tools.expand","to expand") + theme.fg("muted", ")") ``。
- `formatWriteResult(result, theme)`：`!isError` → undefined；text 块 join `\n`；空 → undefined；
  否则 `` `\n${theme.fg("error", output)}` ``。
- `writeRenderers.renderCall`：缓存更新（`context.argsComplete` → 全量重建，否则增量）+ `setText(formatWriteCall(...))`。
- `writeRenderers.renderResult`：无输出 → 复用/新建 `Container` 并 `clear()`；有 → 复用/新建 `Text` 并 setText。

#### 2.17.4 `renderers/edit.ts`

- 状态类型 `EditRenderState = {callComponent?: EditCallRenderComponent}`；
  `EditCallRenderComponent = Box & {preview?, previewArgsKey?, previewPending?, settledError?}`。
- `createEditCallRenderComponent()`：`new Box(1, 1, text => text)` 并 `Object.assign` 默认值
  （Box 参数：paddingX=1、paddingY=1、bgFn=identity）。
- `getEditCallRenderComponent(state, lastComponent)`：`lastComponent instanceof Box` → 复用并写入 `state.callComponent`；
  否则已有 `state.callComponent` → 复用；否则新建。
- `getRenderablePreviewInput(args)`：path 取 `args.path`（string）否则 `args.file_path`；
  无 path → null；`args.edits` 为非空数组且每项 `oldText/newText` 都是 string → `{path, edits}`；
  否则 `args.oldText/newText` 都是 string → `{path, edits:[{oldText,newText}]}`；否则 null。
- `formatEditCall(args, theme, cwd)`：`` `${theme.fg("toolTitle", theme.bold("edit"))} ${renderToolPath(str(args?.file_path ?? args?.path), theme, cwd)}` ``。
- `formatEditResult(args, preview, result, theme, isError)`：
  `previewDiff = preview && !("error" in preview) ? preview.diff : undefined`；
  `previewError = preview && "error" in preview ? preview.error : undefined`；
  - `isError`：errorText = text 块 join `\n`；空或等于 `previewError` → undefined；否则 `theme.fg("error", errorText)`；
  - 否则 `resultDiff = result.details?.diff`；`resultDiff && resultDiff !== previewDiff` → `renderDiff(resultDiff, {filePath: rawPath ?? undefined})`；
  - 否则 undefined。
- `getEditHeaderBg(preview, settledError, theme)`：preview 为 error → `theme.bg("toolErrorBg", ...)`；
  preview 为 diff → `toolSuccessBg`；无 preview 但 settledError → `toolErrorBg`；否则 `toolPendingBg`。
- `buildEditCallComponent(component, args, theme, cwd, outputPad)`：
  `setBgFn(...)`、`setPaddingX(outputPad)`、`clear()`、加入 `Text(formatEditCall(...), 0, 0)`；
  有 preview → `Spacer(1)` + `Text(body, 0, 0)`，`body` 为 `theme.fg("error", preview.error)` 或
  `renderDiff(preview.diff)`（**不传 filePath**，与结果渲染不同）。
- `setEditPreview(component, preview, argsKey) -> changed`：
  比较旧 preview 与新 preview：无旧 → true；两者都 error → `error` 字符串是否不同；
  一 error 一非 error → true；两者都非 error → `diff` 或 `firstChangedLine` 不同；
  然后写回 `preview`、`previewArgsKey`、`previewPending = false`，返回 changed。
- `renderCall(args, theme, context)`：
  1. 取/建组件；`previewInput`；`argsKey = previewInput ? JSON.stringify({path, edits}) : undefined`；
  2. `component.previewArgsKey !== argsKey` → 清空 preview / 设 argsKey / `previewPending = false` / `settledError = false`；
  3. `context.argsComplete && previewInput && !component.preview && !component.previewPending` →
     `previewPending = true`；`void computeEditsDiff(previewInput.path, previewInput.edits, context.cwd).then(preview => {
       if (component.previewArgsKey === requestKey) { setEditPreview(component, preview, requestKey); context.invalidate(); } })`；
  4. 返回 `buildEditCallComponent(component, args, theme, context.cwd, context.outputPad)`。
- `renderResult(result, _options, theme, context)`：
  1. `callComponent = context.state.callComponent`；`previewInput`/`argsKey`（同上）；
  2. `resultDiff = !context.isError ? result.details?.diff : undefined`；
  3. 有 callComponent 时：若 `resultDiff` 是 string → `setEditPreview(callComponent, {diff: resultDiff,
     firstChangedLine: result.details?.firstChangedLine}, argsKey)`（`||` 累积 changed）；
     `callComponent.settledError !== context.isError` → 更新并 `changed = true`；
     `changed` → 重建 `buildEditCallComponent(...)`；
  4. `output = formatEditResult(...)`；组件为复用/新建 `Container`，`clear()`；无 output → 返回空 Container；
     否则 `Spacer(1)` + `Text(output, context.outputPad, 0)`。
- **性能约束（测试 `edit-tool-no-full-redraw`）**：结果settle时若 preview diff 与 result diff 相同，
  不重建整个 Box（`changed === false`），从而避免全量重绘；`argsComplete` 为 false（如从已保存结果重建）时
  仍能通过 `renderResult` 的 `setEditPreview` 恢复盒子内容。

#### 2.17.5 `renderers/find.ts`

- `formatFindCall(args, theme)`：
  `pattern = str(args?.pattern)`；`rawPath = str(args?.path)`；`path = rawPath !== null ? shortenPath(rawPath || ".") : null`；
  `limit = args?.limit`；`invalidArg = invalidArgText(theme)`；
  text = `theme.fg("toolTitle", theme.bold("find")) + " " +
    (pattern === null ? invalidArg : theme.fg("accent", pattern || "")) +
    theme.fg("toolOutput", ` in ${path === null ? invalidArg : path}`)`；
  `limit !== undefined` → 追加 `theme.fg("toolOutput", ` (limit ${limit})`)`。
  （path 缺失 → `rawPath === ""` → `shortenPath(".")` → `"."`；pattern 缺失 → `[invalid arg]`；
  pattern 空串 → 空字符串但仍是 accent 着色。）
- `formatFindResult`：`output = getTextOutput(...).trim()`；有输出 → 前 20 行（expanded 全部）+
  `... (${remaining} more lines,` + keyHint(`app.tools.expand`,`to expand`) + `)` 提示；
  warnings：`resultLimitReached` → `` `${n} results limit` ``（注意是 `results`，不是 `entries`/`matches`）；
  `truncation.truncated` → `` `${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit` ``；
  `` `\n${theme.fg("warning", `[Truncated: ${warnings.join(", ")}]`)}` ``。

#### 2.17.6 `renderers/grep.ts`

- `formatGrepCall`：`grep` 加粗 + ` ` + （pattern 为 null → `[invalid arg]`；否则 `` theme.fg("accent", `/${pattern || ""}/`) ``）
  + `` theme.fg("toolOutput", ` in ${path}`) ``（path 缺失 → `"."`，为 null → invalid）；
  `glob` 真值 → `theme.fg("toolOutput", ` (${glob})`)`；`limit !== undefined` → `theme.fg("toolOutput", ` limit ${limit}`)`
  （**无括号**，与 find 不同）。
- `formatGrepResult`：`output = getTextOutput(result, showImages).trim()`；有输出 → 前 **15** 行
  （`options.expanded` 时全部）+ `remaining > 0` 的 `... ({n} more lines,` + keyHint(`app.tools.expand`,
  `to expand`) + `)`；warnings 顺序固定 matchLimit（`` `${n} matches limit` ``）、
  `truncation.truncated`（`` `${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit` ``）、
  `linesTruncated`（`"some lines truncated"`），以 `", "` 连接，整体
  `` `\n${theme.fg("warning", `[Truncated: ${warnings.join(", ")}]`)}` ``。
  `renderCall`/`renderResult` 都复用 `context.lastComponent as Text ?? new Text("",0,0)`（`setText` 原地改）。

#### 2.17.7 `renderers/ls.ts`

- `formatLsCall`：`` `${theme.fg("toolTitle", theme.bold("ls"))} ${renderToolPath(str(args?.path), theme, cwd, {emptyFallback: "."})}` ``；
  `limit !== undefined` → `theme.fg("toolOutput", ` (limit ${limit})`)`。
- `formatLsResult`：`getTextOutput(...).trim()`；前 20 行（expanded 全部）+ `remaining > 0` 的
  `... ({n} more lines,` + keyHint + `)`；warnings：`details.entryLimitReached` → `` `${n} entries limit` ``、
  `truncation.truncated` → `` `${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit` ``，
  `", "` 连接后包成 `` `\n${theme.fg("warning", `[Truncated: ...]`)}` ``。

#### 2.17.8 `renderers/index.ts`

- 再导出 `createShellRenderers`、`editRenderers`、`findRenderers`、`grepRenderers`、`lsRenderers`、
  `readRenderers`、`writeRenderers`、`ToolRenderers` 类型。
- `createAllToolRenderers(): Record<ToolName, ToolRenderers>`，键顺序 read/bash/powershell/edit/write/grep/find/ls。
- `withBuiltInRenderers(toolName, definition)`：`builtIn = createAllToolRenderers()[toolName]`；
  `!definition` → builtIn；`!builtIn` → definition；否则 `{...definition,
    renderCall: definition.renderCall ?? builtIn.renderCall,
    renderResult: definition.renderResult ?? builtIn.renderResult}`。

### 2.18 `src/core/compaction/index.ts`

```ts
export * from "./branch-summarization.ts";
export * from "./compaction.ts";
export * from "./utils.ts";
```

（注意 `compaction.ts` 与 `branch-summarization.ts` 都再导出 `FileOperations`；
`branch-summarization.ts` 内部 `export type { FileOperations } from "./utils.ts"`——重复导出同名类型在
TS 里合法（同一声明）。Rust 侧用 `pub use` 时避免重复 `pub use` 同一路径引起命名冲突。）

### 2.19 `src/core/compaction/utils.ts`

#### 2.19.1 文件操作跟踪

```ts
export interface FileOperations { read: Set<string>; written: Set<string>; edited: Set<string>; }
export function createFileOps(): FileOperations;   // 三个空 Set
export function extractFileOpsFromMessage(message: AgentMessage, fileOps: FileOperations): void;
export function computeFileLists(fileOps): {readFiles: string[]; modifiedFiles: string[]};
export function formatFileOperations(readFiles: string[], modifiedFiles: string[]): string;
```

`extractFileOpsFromMessage`：
1. `message.role === "toolResult"`：遍历 `message.nestedCalls?.calls ?? []`，对每项
   `addFileOp(call.name, call.arguments, fileOps)`，然后**返回**（不再看 content）。
2. `message.role !== "assistant"` → 返回。
3. `!("content" in message) || !Array.isArray(message.content)` → 返回。
4. 逐 block：非对象/null 跳过；`!("type" in block) || block.type !== "toolCall"` 跳过；
   `!("arguments" in block) || !("name" in block)` 跳过；`addFileOp(block.name, block.arguments, fileOps)`。

`addFileOp(toolName, args, fileOps)`：`path = typeof args?.path === "string" ? args.path : undefined`；
无 path 返回；`read` → `read.add`；`write` → `written.add`；`edit` → `edited.add`；其他工具忽略。
（只认 `path` 参数名；`file_path` 不算。路径按**原始字符串**记录，不做归一化。）

`computeFileLists`：`modified = new Set([...edited, ...written])`；
`readOnly = [...read].filter(f => !modified.has(f)).sort()`；`modifiedFiles = [...modified].sort()`。
`sort()` 是**默认字典序（UTF-16 码元序）**，不是 locale。

`formatFileOperations`：`sections`：
- `readFiles.length > 0` → `` `<read-files>\n${readFiles.join("\n")}\n</read-files>` ``；
- `modifiedFiles.length > 0` → `` `<modified-files>\n${modifiedFiles.join("\n")}\n</modified-files>` ``；
- 空 → `""`；否则 `` `\n\n${sections.join("\n\n")}` ``（**前导两个换行**）。

#### 2.19.2 序列化

`const TOOL_RESULT_MAX_CHARS = 2000;`（未导出）

`truncateForSummary(text, maxChars)`：`text.length <= maxChars` → 原样；否则
`` `${text.slice(0,maxChars)}\n\n[... ${truncatedChars} more characters truncated]` ``，
`truncatedChars = text.length - maxChars`（**UTF-16 码元数差**）。

`export function serializeConversation(messages: Message[]): string`：
逐消息：
- `user`：`content = contentText(msg.content, "")`；非空 → push `` `[User]: ${content}` ``。
- `assistant`：先收集 toolCalls：每个 `block.type === "toolCall"`：
  `argsStr = Object.entries(block.arguments).map(([k,v]) => `${k}=${JSON.stringify(v)}`).join(", ")`；
  push `` `${block.name}(${argsStr})` ``。
  然后 `msg.content.some(b => b.type === "text")` → push `` `[Assistant]: ${contentText(msg.content)}` ``；
  `toolCalls.length > 0` → push `` `[Assistant tool calls]: ${toolCalls.join("; ")}` ``。
  （顺序：先文本行，再 tool calls 行；若只有 thinking 无 text 且无 toolCall，则该消息不产出任何部分。）
- `toolResult`：`content = contentText(msg.content, "")`；非空 → push `` `[Tool result]: ${truncateForSummary(content, 2000)}` ``。
- 其他角色（含 `system`）忽略。

最终 `parts.join("\n\n")`。

注意：`JSON.stringify(v)` 对 `undefined` 返回 `undefined`（不是字符串），模板插值会得到 `"undefined"`；
对字符串加双引号并转义；对对象按插入序输出；数字按 JS 规则。toolCall 参数里嵌套对象/数组常见，
Rust 必须用「保序 + JS 数字格式」的序列化器（见 8.2）。

#### 2.19.3 `SUMMARIZATION_SYSTEM_PROMPT`（逐字，导出）

```
You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.
```

### 2.20 `src/core/compaction/compaction.ts`

#### 2.20.1 类型与常量

```ts
export interface CompactionDetails { readFiles: string[]; modifiedFiles: string[]; }

export interface CompactionResult<T = unknown> {
  summary: string;
  firstKeptEntryId: string;
  tokensBefore: number;
  estimatedTokensAfter?: number;
  usage?: Usage;
  details?: T;
}

export interface CompactionSettings { enabled: boolean; reserveTokens: number; keepRecentTokens: number; }
export const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = {
  enabled: true, reserveTokens: 16384, keepRecentTokens: 20000,
};

export interface ContextUsageEstimate {
  tokens: number; usageTokens: number; trailingTokens: number; lastUsageIndex: number | null;
}

export interface CutPointResult {
  firstKeptEntryIndex: number; turnStartIndex: number; isSplitTurn: boolean;
}

export interface CompactionPreparation {
  firstKeptEntryId: string;
  messagesToSummarize: AgentMessage[];
  turnPrefixMessages: AgentMessage[];
  isSplitTurn: boolean;
  tokensBefore: number;
  previousSummary?: string;
  fileOps: FileOperations;
  settings: CompactionSettings;
}
```

#### 2.20.2 `calculateContextTokens(usage)`

`usage.totalTokens || usage.input + usage.output + usage.cacheRead + usage.cacheWrite`
（`||`：`totalTokens === 0` 时回退到分量求和；负数/NaN 走真值判断——NaN 为假值，会回退）。

#### 2.20.3 `getAssistantUsage(msg)`（内部）

`msg.role === "assistant" && "usage" in msg` 且 `stopReason !== "aborted"` 且 `!== "error"` 且
`usage` 存在且 `calculateContextTokens(usage) > 0` → 返回 usage；否则 undefined。

#### 2.20.4 `getLastAssistantUsage(entries)`（导出）

从尾向头扫 `entry.type === "message"`，第一个有 usage 的返回。

#### 2.20.5 `estimateContextTokens(messages)`（导出）

`getLastAssistantUsageInfo(messages)`（尾部第一个有效 usage 及其下标）：
- 无 → 全量 `estimateTokens` 求和，返回 `{tokens: estimated, usageTokens: 0, trailingTokens: estimated, lastUsageIndex: null}`；
- 有 → `usageTokens = calculateContextTokens(usage)`；`trailingTokens = sum(estimateTokens(messages[i])) for i > index`；
  返回 `{tokens: usageTokens + trailingTokens, usageTokens, trailingTokens, lastUsageIndex: index}`。

#### 2.20.6 `estimateProjectedContextTokens(projection, branchEntries)`（导出）

1. `estimate = estimateContextTokens(projection.messages)`。
2. `estimate.lastUsageIndex !== null`：
   - 遍历 `projection.entries`，累计 `projectedMessageIndex`；若 `lastUsageIndex < nextMessageIndex` →
     `usageEntryId = entry.sourceEntry.id` 并 break；
   - `usageEntryIndex = usageEntryId ? branchEntries.findIndex(e => e.id === usageEntryId) : -1`；
   - 从尾向头找第一个 `type === "context_edit" || type === "compaction"` 的下标 → `latestInvalidatingEntryIndex`（-1 表示无）；
   - `usageEntryIndex > latestInvalidatingEntryIndex` → 返回 `estimate`（usage 仍可信）。
3. 否则（usage 在最近一次 context_edit/compaction 之前，或没有 usage）：
   `currentSystem = getCurrentSystemMessage(projection.messages)`；
   `tokens = currentSystem ? estimateTokens(currentSystem) : 0`；
   对 `projection.messages` 中 **非 system** 消息累加 `estimateTokens`；
   返回 `{tokens, usageTokens: 0, trailingTokens: tokens, lastUsageIndex: null}`。

语义：压缩后/上下文编辑后，旧 usage 不再代表当前上下文，改用字符启发式重算。

#### 2.20.7 `shouldCompact(contextTokens, contextWindow, settings)`

`!settings.enabled` → false；否则 `contextTokens > contextWindow - settings.reserveTokens`
（严格大于；`reserveTokens` 为 0 时等价于 `> contextWindow`）。

#### 2.20.8 估算与分类（内部）

```ts
const ESTIMATED_IMAGE_CHARS = 4800;
```

`estimateTextAndImageContentChars(content)`：
- string → `content.length`（UTF-16 码元）；
- 数组 → 逐块：`type === "text" && block.text` → `+= block.text.length`；
  `type === "image"` → `+= 4800`；其他类型忽略。

`export function estimateTokens(message)`：
- `system`：`chars = estimateTextAndImageContentChars(system.content)`；`system.sections` 存在 →
  遍历 `Object.values(sections)`，真值项 `+= section.length`；`system.toolsAdded` 存在 →
  `+= JSON.stringify(system.toolsAdded).length`；返回 `Math.ceil(chars / 4)`。
- `user`：`ceil(content chars / 4)`。
- `assistant`：逐块：text → `+= text.length`；thinking → `+= thinking.length`；
  toolCall → `+= name.length + JSON.stringify(arguments).length`；返回 `ceil(chars/4)`。
- `custom` / `toolResult`：`ceil(estimateTextAndImageContentChars(content)/4)`。
- `bashExecution`：`ceil((command.length + output.length)/4)`。
- `branchSummary` / `compactionSummary`：`ceil(summary.length/4)`。
- 其他 → `0`（函数末尾 `return 0`）。

`isCutPointMessage(message)`：user/assistant/bashExecution/custom/branchSummary/compactionSummary → true；
toolResult → false；其他 → false（末尾）。

`isTurnStartMessage(message)`：user/bashExecution/custom/branchSummary/compactionSummary → true；
assistant/toolResult → false。

`isTurnStartEntry(entry)`：`entry.type === "compaction"` → false；
否则 `sessionEntryToContextMessages(entry).some(isTurnStartMessage)`。

#### 2.20.9 `findValidCutPoints(entries, startIndex, endIndex)`（内部）

`for i in startIndex..endIndex`（**不含 endIndex**）：`entry.type === "compaction"` → 跳过；
`sessionEntryToContextMessages(entry).some(isCutPointMessage)` → push i。

#### 2.20.10 `findTurnStartIndex(entries, entryIndex, startIndex)`（导出）

从 `entryIndex` 向 `startIndex`（含）回扫，第一个 `isTurnStartEntry` 的下标；无 → `-1`。

#### 2.20.11 `findCutPoint(entries, startIndex, endIndex, keepRecentTokens)`（导出）

1. `cutPoints = findValidCutPoints(...)`；空 → `{firstKeptEntryIndex: startIndex, turnStartIndex: -1, isSplitTurn: false}`。
2. `accumulatedTokens = 0`；`cutIndex = cutPoints[0]`。
3. `for i = endIndex-1 down to startIndex`：
   `messageTokens = sum(estimateTokens(m) for m in sessionEntryToContextMessages(entries[i]))`；
   `=== 0` → continue；`+= messageTokens`；
   `>= keepRecentTokens` → `cutIndex = cutPoints.find(c => c >= i) ?? cutPoints[last]`；break。
4. **元数据回扫**：`while cutIndex > startIndex`：
   `prev = entries[cutIndex-1]`；`prev.type === "compaction" || sessionEntryToContextMessages(prev).length > 0` → break；
   `cutIndex--`。
5. `cutEntry = entries[cutIndex]`；`startsTurn = isTurnStartEntry(cutEntry)`；
   `turnStartIndex = startsTurn ? -1 : findTurnStartIndex(entries, cutIndex, startIndex)`。
6. 返回 `{firstKeptEntryIndex: cutIndex, turnStartIndex, isSplitTurn: !startsTurn && turnStartIndex !== -1}`。

#### 2.20.12 摘要 prompt（**逐字**，全部为模块内常量）

`SUMMARIZATION_PROMPT`：
```
The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.
```

`UPDATE_SUMMARIZATION_INSTRUCTIONS`：
```
Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.
```

`UPDATE_SUMMARIZATION_PROMPT` = （逐字拼接，中间一个空行）
```
The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

{UPDATE_SUMMARIZATION_INSTRUCTIONS}
```

`TURN_PREFIX_SUMMARIZATION_PROMPT`：
```
The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages.
```

#### 2.20.13 `getSummarizationFailure(response, label)`（导出）

- `response.stopReason === "error"` → `` `${label} failed: ${response.errorMessage || "Unknown error"}` ``；
- `response.stopReason === "length"` → `` `${label} failed: generation hit the token cap and the summary is incomplete` ``；
- 否则 `undefined`。

调用方传入的 label：`"Summarization"`、`"Turn prefix summarization"`、`"Branch summarization"`。

#### 2.20.14 `createSummarizationOptions(model, apiKey, headers, env, signal, thinkingLevel, sessionId)`（内部）

```ts
const options: SimpleStreamOptions = { signal, apiKey, headers, env, sessionId };
if (model.reasoning && thinkingLevel && thinkingLevel !== "off") options.reasoning = thinkingLevel;
return options;
```
（键顺序 signal/apiKey/headers/env/sessionId，可能追加 reasoning。）

#### 2.20.15 `completeSummarization(model, context, options, streamFn?, retry?, callbacks?)`（导出）

```ts
const requestOptions = {...options, cacheRetention: "none", sessionId: options.sessionId ?? uuidv7()};
const produce = async () => streamFn
  ? (await streamFn(model, context, requestOptions)).result()
  : completeSimple(model, context, requestOptions);
return retryAssistantCall(produce, retry, requestOptions.signal, callbacks);
```
（唯一 LLM 调用汇聚点；分行摘要也用同一入口。）

#### 2.20.16 `generateSummary(...)`（导出，返回 `Promise<string>`）

参数顺序（**位置参数，Rust 侧应改为具名结构体**）：
`currentMessages, model, _reserveTokens (未使用), apiKey, headers?, signal?, customInstructions?, previousSummary?,
thinkingLevel?, streamFn?, env?, retry?, callbacks?, sessionId?`。
实现 = `(await generateSummaryWithUsage(...)).text`。注释说明 `_reserveTokens` 保留但未用（摘要使用模型输出上限）。

#### 2.20.17 `buildSummarizationContext(promptText)`（内部）

```ts
normalizeContext({
  systemPrompt: SUMMARIZATION_SYSTEM_PROMPT,
  messages: [{ role: "user", content: [{type:"text", text: promptText}], timestamp: Date.now() }],
});
```

#### 2.20.18 `generateSummaryWithUsage(...)`（导出，返回 `{text, usage}`）

1. `basePrompt = previousSummary ? UPDATE_SUMMARIZATION_PROMPT : SUMMARIZATION_PROMPT`；
   `customInstructions` 非空 → `` `${basePrompt}\n\nAdditional focus: ${customInstructions}` ``。
2. `llmMessages = convertToLlm(currentMessages)`；`conversationText = serializeConversation(llmMessages)`。
3. `promptText = `<conversation>\n${conversationText}\n</conversation>\n\n``；
   `previousSummary` 存在 → 追加 `` `<previous-summary>\n${previousSummary}\n</previous-summary>\n\n` ``；
   追加 `basePrompt`。
4. `completionOptions = createSummarizationOptions(...)`；
   `response = await completeSummarization(model, buildSummarizationContext(promptText), completionOptions, streamFn, retry, callbacks)`。
5. `failure = getSummarizationFailure(response, "Summarization")`；有 → `throw new Error(failure)`。
6. `response.content.some(b => b.type === "toolCall")` → `throw new Error("Summarization attempted to call a tool")`。
7. 返回 `{text: contentText(response.content), usage: response.usage}`。

#### 2.20.19 投影版切点（内部）

- `isProjectedTurnStart(entry)`：`entry.sourceEntry.type === "compaction"` → false；
  否则 `entry.messages.some(isTurnStartMessage)`。
- `findProjectedTurnStartIndex(entries, entryIndex, startIndex)`：回扫第一个 `isProjectedTurnStart`；无 → -1。
- `findProjectedCutPoint(entries, startIndex, endIndex, keepRecentTokens)`：
  1. cutPoints：`i in startIndex..endIndex`，`sourceEntry.type !== "compaction" && messages.some(isCutPointMessage)`。
     空 → `{firstKeptEntryIndex: startIndex, turnStartIndex: -1, isSplitTurn: false}`。
  2. 反向累加同 `findCutPoint`，但记录 `exceededBudget = true`。
  3. **recovery omission suffix 前移**：
     ```ts
     const suffix = entries.slice(cutIndex + 1, endIndex);
     const isIntrinsicallyVisible = (e) => e.sourceEntry.type !== "context_edit" &&
        sessionEntryToContextMessages(e.sourceEntry).length > 0;
     const isOmitted = (e) => isIntrinsicallyVisible(e) && e.messages.length === 0;
     const omittedSuffixIds = new Set(suffix.filter(isOmitted).map(e => e.sourceEntry.id));
     const hasExternalReplacement = suffix.some(e =>
        e.sourceEntry.type === "context_edit" && e.sourceEntry.replacement !== null &&
        !omittedSuffixIds.has(e.sourceEntry.targetId));
     const isRecoveryOmissionSuffix =
        exceededBudget && !hasExternalReplacement &&
        suffix.some(e => e.sourceEntry.type === "message" && e.sourceEntry.message.role === "assistant" && isOmitted(e)) &&
        suffix.every(e => e.sourceEntry.type !== "compaction" && (!isIntrinsicallyVisible(e) || isOmitted(e)));
     if (isRecoveryOmissionSuffix) cutIndex++;
     ```
  4. 元数据回扫：`previous.sourceEntry.type === "compaction" || previous.messages.length > 0` → break；否则 `cutIndex--`。
  5. `startsTurn = isProjectedTurnStart(entries[cutIndex])`；
     `turnStartIndex = startsTurn ? -1 : findProjectedTurnStartIndex(...)`；
     返回 `{firstKeptEntryIndex: cutIndex, turnStartIndex, isSplitTurn: !startsTurn && turnStartIndex !== -1}`。

#### 2.20.20 `prepareCompaction(pathEntries, settings)`（导出）

1. `pathEntries` 非空且最后一条 `type === "compaction"` → `undefined`（已压缩到底）。
2. `projection = buildSessionProjection(pathEntries)`；`projectedEntries = projection.entries`；
   `sourceEntries = projectedEntries.map(e => e.sourceEntry)`。
3. `prevCompactionIndex = projectedEntries.findIndex(e => e.sourceEntry.type === "compaction" && e.messages.length > 0)`
   （最新的压缩最先被投影；更老的压缩条目可能仍在保留的原始区间里，但其投影贡献为空）。
4. `previousSummary` / `boundaryStart = 0`：`prevCompactionIndex >= 0` →
   `previousSummary = (sourceEntry as CompactionEntry).summary`；`boundaryStart = prevCompactionIndex + 1`。
5. `boundaryEnd = projectedEntries.length`；
   `tokensBefore = estimateProjectedContextTokens(projection, pathEntries).tokens`。
6. `cutPoint = findProjectedCutPoint(projectedEntries, boundaryStart, boundaryEnd, settings.keepRecentTokens)`。
7. `firstKeptEntry = projectedEntries[cutPoint.firstKeptEntryIndex]?.sourceEntry`；
   无 `id` → `undefined`。
8. `historyEnd = cutPoint.isSplitTurn ? cutPoint.turnStartIndex : cutPoint.firstKeptEntryIndex`。
9. `messagesToSummarize = projectedEntries.slice(boundaryStart, historyEnd).flatMap(getMessagesFromProjectedEntryForCompaction)`；
   `turnPrefixMessages = isSplitTurn ? projectedEntries.slice(turnStartIndex, firstKeptEntryIndex).flatMap(...) : []`。
   其中 `getMessagesFromProjectedEntryForCompaction(entry)`：`sourceEntry.type === "compaction"` → `[]`；
   否则 `entry.messages.filter(m => m.role !== "system")`（system 消息属于提示状态，不进总结）。
10. 两者都为空 → `undefined`。
11. `fileOps = extractFileOperations(messagesToSummarize, sourceEntries, prevCompactionIndex)`；
    `isSplitTurn` 时再对 `turnPrefixMessages` 逐个 `extractFileOpsFromMessage`。
12. 返回 `{firstKeptEntryId, messagesToSummarize, turnPrefixMessages, isSplitTurn, tokensBefore,
    previousSummary, fileOps, settings}`。

**retain-none 与前次 kept 边界的回退**（来自 `docs/compaction.md`，由 `buildSessionProjection` 实现）：
- 「retain-none 压缩」把**自己的 id** 写进 `firstKeptEntryId`（`appendCompaction` 在
  `firstKeptEntryId === null` 时用 `firstKeptEntryId ?? id`），因此重建上下文时不保留任何前置条目；
  下一次压缩的 `boundaryStart` 落在该压缩条目**之后**（第 4 步的 `prevCompactionIndex + 1` 自然成立）。
- 若上一个压缩的 `firstKeptEntryId` 指向的条目**不在当前路径上**（分支被切换/裁剪），回退到
  「上一个压缩条目之后的那个条目」。本单元的 `prepareCompaction` 不自己解引用 `firstKeptEntryId`，
  该规则**完全由 `buildSessionProjection`（ca-session 单元）负责**；Rust 侧只需保证
  `boundaryStart = prevCompactionIndex + 1` 且投影已按此规则重建。
- `tokensBefore` 必须用**重建后的（含 context_edit 的）投影**重新估算（第 5 步），而不是沿用历史值；
  被 omit 的原始条目仍保存在磁盘上，但不参与切点选择、摘要、检查点与 token 估算。

`extractFileOperations(messages, entries, prevCompactionIndex)`（内部）：
- `fileOps = createFileOps()`；
- `prevCompactionIndex >= 0`：`prev = entries[prevCompactionIndex] as CompactionEntry`；
  `!prev.fromHook && prev.details` → `details = prev.details as CompactionDetails`；
  `Array.isArray(details.readFiles)` → 全部 `fileOps.read.add`；
  `Array.isArray(details.modifiedFiles)` → 全部 `fileOps.edited.add`；
- 对 `messages` 逐条 `extractFileOpsFromMessage`。

#### 2.20.21 `compact(preparation, model, apiKey, headers?, customInstructions?, signal?, thinkingLevel?, streamFn?, env?, retry?, callbacks?, sessionId?)`（导出）

1. 解构 preparation。
2. **split turn 且 turnPrefixMessages 非空**：
   - `historyText = previousSummary ?? "No prior history."`；`historyUsage`；
   - `messagesToSummarize.length > 0` → `generateSummaryWithUsage(messagesToSummarize, model, undefined,
     apiKey, headers, signal, customInstructions, previousSummary, thinkingLevel, streamFn, env, retry, callbacks,
     sessionId)` → 覆盖 `historyText`/`historyUsage`；
   - `turnPrefixResult = await generateTurnPrefixSummary(turnPrefixMessages, model, apiKey, headers, env, signal,
     thinkingLevel, streamFn, retry, callbacks, sessionId, customInstructions)`；
   - `summary = `${historyText}\n\n---\n\n**Turn Context (split turn):**\n\n${turnPrefixResult.text}``；
   - `summaryUsage = historyUsage ? combineUsage(historyUsage, turnPrefixResult.usage) : turnPrefixResult.usage`。
3. 否则：`generateSummaryWithUsage(messagesToSummarize, ...)`；`summary = result.text`；`summaryUsage = result.usage`。
4. `{readFiles, modifiedFiles} = computeFileLists(fileOps)`；`summary += formatFileOperations(readFiles, modifiedFiles)`。
5. `!firstKeptEntryId` → `throw new Error("First kept entry has no UUID - session may need migration")`。
6. 返回 `{summary, firstKeptEntryId, tokensBefore, usage: summaryUsage,
   details: {readFiles, modifiedFiles} as CompactionDetails}`（键顺序；无 `estimatedTokensAfter`）。

#### 2.20.22 `generateTurnPrefixSummary(...)`（内部）

1. `llmMessages = convertToLlm(messages)`；`conversationText = serializeConversation(llmMessages)`；
   `promptText = `# Conversation\n${conversationText}\n\n# Instructions\n${TURN_PREFIX_SUMMARIZATION_PROMPT}``；
   `customInstructions` → 追加 `` `\n\nAdditional focus: ${customInstructions}` ``。
2. `completeSummarization(model, buildSummarizationContext(promptText), createSummarizationOptions(...), streamFn, retry, callbacks)`。
3. `getSummarizationFailure(response, "Turn prefix summarization")` → throw。
4. toolCall → `throw new Error("Turn prefix summarization attempted to call a tool")`。
5. 返回 `{text: contentText(response.content), usage: response.usage}`。

### 2.21 `src/core/compaction/branch-summarization.ts`

#### 2.21.1 类型

```ts
export interface BranchSummaryResult {
  summary?: string; usage?: Usage; readFiles?: string[]; modifiedFiles?: string[];
  aborted?: boolean; error?: string;
}
export interface BranchSummaryDetails { readFiles: string[]; modifiedFiles: string[]; }
export type { FileOperations } from "./utils.ts";
export interface BranchPreparation { messages: AgentMessage[]; fileOps: FileOperations; totalTokens: number; }
export interface CollectEntriesResult { entries: SessionEntry[]; commonAncestorId: string | null; }
export interface GenerateBranchSummaryOptions {
  model: Model<any>;
  apiKey?: string;
  headers?: Record<string,string>;
  env?: Record<string,string>;
  signal: AbortSignal;                 // 必填
  customInstructions?: string;
  replaceInstructions?: boolean;
  reserveTokens?: number;              // 默认 16384
  streamFn?: StreamFn;
  retry?: RetryPolicy;
  callbacks?: RetryCallbacks;
}
```

#### 2.21.2 `collectEntriesForBranchSummary(session, oldLeafId, targetId)`（导出）

1. `!oldLeafId` → `{entries: [], commonAncestorId: null}`。
2. `oldPath = new Set(session.getBranch(oldLeafId).map(e => e.id))`；
   `targetPath = session.getBranch(targetId)`（根在前）。
3. 从 `targetPath` 尾部向头找第一个 `oldPath.has(id)` → `commonAncestorId`；无 → null。
4. 从 `oldLeafId` 沿 `parentId` 上溯直到 `commonAncestorId`（或 `getEntry` 返回 undefined 中断），逐个 push。
5. `entries.reverse()`（时间顺序）。
6. 返回 `{entries, commonAncestorId}`。

（不因 compaction 边界停止：压缩摘要作为上下文一起被总结。）

#### 2.21.3 `getMessageFromEntry(entry)`（内部）

- `message`：`entry.message.role === "toolResult"` → undefined（上下文已在其 toolCall 里）；否则 `entry.message`。
- `custom_message`：`createCustomMessage(entry.customType, entry.content, entry.display, entry.details, entry.timestamp)`。
- `branch_summary`：`createBranchSummaryMessage(entry.summary, entry.fromId, entry.timestamp)`。
- `compaction`：`createCompactionSummaryMessage(entry.summary, entry.tokensBefore, entry.timestamp)`。
- `thinking_level_change` / `model_change` / `custom` / `label` / `session_info` → undefined（逐字列出的 case）。
- **其余类型全部落到 switch 之外 → 隐式返回 `undefined`**：包括 `usage`（计费/统计条目）、
  `context_edit`（省略/替换编辑）以及任何未来新增条目类型。Rust 侧必须用 `_ => None` 兜底，
  不能因为枚举未覆盖而 panic 或把 `usage` 条目当成对话内容。

#### 2.21.4 `prepareBranchEntries(entries, tokenBudget = 0)`（导出）

1. **第一遍**（不受预算限制）：对每个 `entry.type === "branch_summary" && !entry.fromHook && entry.details`：
   `details = entry.details as BranchSummaryDetails`；`readFiles` 数组 → `fileOps.read.add`；
   `modifiedFiles` 数组 → `fileOps.edited.add`（**只进 edited，不进 written**；注释说 "both edited and written
   for proper deduplication"，但代码只加 edited；`computeFileLists` 的 modified 是 edited ∪ written，故等效）。
2. **第二遍**从新到旧：`message = getMessageFromEntry(entry)`；无 → continue；
   `extractFileOpsFromMessage(message, fileOps)`；`tokens = estimateTokens(message)`；
   若 `tokenBudget > 0 && totalTokens + tokens > tokenBudget`：
   - 若 `entry.type === "compaction" || "branch_summary"` 且 `totalTokens < tokenBudget * 0.9` →
     `messages.unshift(message)`、`totalTokens += tokens`；
   - `break`（无论如何都停）。
   否则 `messages.unshift(message)`、`totalTokens += tokens`。
3. 返回 `{messages, fileOps, totalTokens}`。

（注意：`messages.unshift` 在尾部遍历中保持时间顺序；预算判断是「加入后会超」即停，
但摘要类条目有 90% 的例外。）

#### 2.21.5 prompt 常量（**逐字**）

`BRANCH_SUMMARY_PREAMBLE`（注意结尾有一个空行，即两个 `\n`）：
```
The user explored a different conversation branch before returning here.
Summary of that exploration:

```
（Rust 字面量应为 `"The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n"`。）

`BRANCH_SUMMARY_PROMPT`：
```
Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages.
```

#### 2.21.6 `generateBranchSummary(entries, options)`（导出）

1. 解构 options（`reserveTokens = 16384` 默认）。
2. `contextWindow = model.contextWindow || 128000`（0/undefined → 128000）；
   `tokenBudget = contextWindow - reserveTokens`。
3. `{messages, fileOps} = prepareBranchEntries(entries, tokenBudget)`。
4. `messages.length === 0` → 返回 `{summary: "No content to summarize"}`（**无 usage/readFiles**）。
5. `llmMessages = convertToLlm(messages)`；`conversationText = serializeConversation(llmMessages)`。
6. `instructions`：
   - `replaceInstructions && customInstructions` → `customInstructions`；
   - 否则 `customInstructions` → `` `${BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: ${customInstructions}` ``；
   - 否则 `BRANCH_SUMMARY_PROMPT`。
7. `promptText = `<conversation>\n${conversationText}\n</conversation>\n\n${instructions}``。
8. `summarizationMessages = [{role:"user", content:[{type:"text", text:promptText}], timestamp: Date.now()}]`。
9. `context = normalizeContext({systemPrompt: SUMMARIZATION_SYSTEM_PROMPT, messages: summarizationMessages})`；
   `requestOptions: SimpleStreamOptions = {apiKey, headers, env, signal}`（**不含 reasoning/sessionId**；
   `completeSummarization` 会补 `cacheRetention:"none"` 与 `sessionId ?? uuidv7()`）。
10. `response = await completeSummarization(model, context, requestOptions, streamFn, retry, callbacks)`。
11. `response.stopReason === "aborted"` → `{aborted: true}`。
12. `failure = getSummarizationFailure(response, "Branch summarization")` → `{error: failure}`。
13. toolCall → `{error: "Branch summarization attempted to call a tool"}`。
14. `summary = contentText(response.content)`；`summary = BRANCH_SUMMARY_PREAMBLE + summary`；
    `{readFiles, modifiedFiles} = computeFileLists(fileOps)`；`summary += formatFileOperations(readFiles, modifiedFiles)`。
15. 返回 `{summary: summary || "No summary generated", usage: response.usage, readFiles, modifiedFiles}`。
    （`summary` 因有 preamble 永远非空，故 `|| "No summary generated"` 实际不可达——照抄。）

### 2.22 `src/core/export-html/index.ts`

#### 2.22.1 接口与类型

```ts
export interface ToolHtmlRenderer {
  renderCall(toolCallId: string, toolName: string, args: unknown): string | undefined;
  renderResult(toolCallId: string, toolName: string,
    result: Array<{type:string; text?:string; data?:string; mimeType?:string}>,
    details: unknown, isError: boolean,
  ): { collapsed?: string; expanded?: string } | undefined;
}

interface RenderedToolHtml { callHtml?: string; resultHtmlCollapsed?: string; resultHtmlExpanded?: string; }

export interface ExportOptions {
  outputPath?: string;
  themeName?: string;
  toolRenderer?: ToolHtmlRenderer;
}

interface SessionData {
  header: ...; entries: ...; leafId: string | null;
  systemPrompt?: string;
  tools?: Array<Pick<ToolDefinition, "name" | "description" | "parameters">>;
  renderedTools?: Record<string, RenderedToolHtml>;
}
```
`SessionData` 的 `JSON.stringify` 键顺序即 header/entries/leafId/systemPrompt/tools/renderedTools，
值为 `undefined` 的键**被省略**（所以 `renderedTools` 未渲染时不出现）。

#### 2.22.2 颜色工具（内部）

- `parseColor(color)`：`/^#([0-9a-fA-F]{2})([0-9a-fA-F]{2})([0-9a-fA-F]{2})$/` → 三个 16 进制；或
  `/^rgb\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)$/` → 三个十进制；否则 undefined。
  （只接受 6 位 hex，不带 alpha；不接受 `oklch()`。）
- `getLuminance(r,g,b)`：`toLinear(c)`：`s = c/255`；`s <= 0.03928 ? s/12.92 : ((s+0.055)/1.055) ** 2.4`；
  返回 `0.2126*R + 0.7152*G + 0.0722*B`。
- `adjustBrightness(color, factor)`：解析失败 → 原样返回 color；否则每个通道
  `min(255, max(0, round(c * factor)))`，返回 `` `rgb(${r}, ${g}, ${b})` ``（逗号后**一个空格**）。
- `deriveExportColors(baseColor)`：
  - 解析失败 → `{pageBg:"rgb(24, 24, 30)", cardBg:"rgb(30, 30, 36)", infoBg:"rgb(60, 55, 40)"}`；
  - `isLight = luminance > 0.5`；
  - 亮色：`pageBg = adjustBrightness(base, 0.96)`、`cardBg = base`（**原样字符串**，可能是 `#rrggbb`）、
    `infoBg = `rgb(${min(255,r+10)}, ${min(255,g+5)}, ${max(0,b-20)})``；
  - 暗色：`pageBg = adjustBrightness(base, 0.7)`、`cardBg = adjustBrightness(base, 0.85)`、
    `infoBg = `rgb(${min(255,r+20)}, ${min(255,g+15)}, ${b})``。

#### 2.22.3 `generateThemeVars(themeName?)`（内部）

1. `colors = getResolvedThemeColors(themeName)`（`theme.ts`：把主题 token 映射成 hex 字符串，键序为
   主题 JSON 的 token 顺序（`Object.entries` 保插入序））；
2. 每对 `` `--${key}: ${value};` `` 一行；
3. `themeExport = getThemeExportColors(themeName)`；`userMessageBg = colors.userMessageBg || "#343541"`；
   `derived = deriveExportColors(userMessageBg)`；
4. push `` `--exportPageBg: ${themeExport.pageBg ?? derived.pageBg};` ``、`--exportCardBg`、`--exportInfoBg`；
5. `lines.join("\n      ")`（**换行 + 6 个空格**，嵌入 CSS 的缩进）。

#### 2.22.4 `generateHtml(sessionData, themeName?)`（内部）

1. `templateDir = getExportTemplateDir()`（构建期路径，见 2.33）。
2. 读入 `template.html`、`template.css`、`template.js`、`vendor/marked.min.js`、
   `vendor/marked-cjk-friendly.min.js`、`vendor/highlight.min.js`（全部 utf-8 字符串）。
3. `themeVars = generateThemeVars(themeName)`；`colors = getResolvedThemeColors(themeName)`；
   `themeExport = getThemeExportColors(themeName)`；`derivedExportColors = deriveExportColors(colors.userMessageBg || "#343541")`；
   `bodyBg = themeExport.pageBg ?? derived.pageBg`；`containerBg = themeExport.cardBg ?? derived.cardBg`；
   `infoBg = themeExport.infoBg ?? derived.infoBg`。
4. `sessionDataBase64 = Buffer.from(JSON.stringify(sessionData)).toString("base64")`（标准 base64，
   无换行；UTF-8 编码再 base64）。
5. `css = templateCss.replace("{{THEME_VARS}}", themeVars).replace("{{BODY_BG}}", bodyBg)
   .replace("{{CONTAINER_BG}}", containerBg).replace("{{INFO_BG}}", infoBg)`（**每个只替换第一次出现**；
   若插入值里含 `{{...}}` 也不会被二次替换，因为 replace 不递归且后续 replace 的搜索串不同）。
6. `html = template.replace("{{CSS}}", css).replace("{{JS}}", templateJs)
   .replace("{{SESSION_DATA}}", sessionDataBase64)
   .replace("{{MARKED_JS}}", markedJs).replace("{{MARKED_CJK_FRIENDLY_JS}}", markedCjkFriendlyJs)
   .replace("{{HIGHLIGHT_JS}}", hljsJs)`。
7. 返回 html。

**替换顺序的关键风险**：`{{JS}}` 在 `template.html` 中出现于 `<script>{{JS}}</script>`；
而 `template.js` 内容里若含 `{{SESSION_DATA}}` 之类字面量会被后续替换误伤——实际不会（JS 里没有）。
但 Rust 必须严格按**同一顺序**做单次替换（不能用 `replace_all`，也不能先替换 `{{SESSION_DATA}}`：
base64 可能包含 `{{`? base64 字母表不含 `{`，安全；但顺序仍需一致以便审计）。

#### 2.22.5 `TEMPLATE_RENDERED_TOOLS`（内部）

```ts
const TEMPLATE_RENDERED_TOOLS = new Set(["bash", "read", "write", "edit", "ls"]);
```
**精确语义**：`template.js` 的 `renderToolCall` 只对 `bash` / `read` / `write` / `edit` / `ls`
有内联 HTML 分支，其余工具（含 `grep`、`find`、`powershell` 与所有扩展工具）走 `default` 分支：
优先用 `renderedTools[call.id]` 的 `callHtml`/`resultHtmlCollapsed`/`resultHtmlExpanded`；
没有预渲染时退化为「工具名 + `JSON.stringify(args, null, 2)` + 文本结果」。
因此 `grep`/`find` 必须走**预渲染**路径（它们不在 `TEMPLATE_RENDERED_TOOLS` 里），
而 `powershell` 不在集合里 → 也会预渲染（这也解释了为什么 `powershell` 没有被模板内联）。

#### 2.22.6 `preRenderCustomTools(entries, toolRenderer)`（内部）

遍历 `entries`：
1. `entry.type !== "message"` → continue。
2. assistant 且 `content` 是数组：每个 `block.type === "toolCall" && !TEMPLATE_RENDERED_TOOLS.has(block.name)`：
   `callHtml = toolRenderer.renderCall(block.id, block.name, block.arguments)`；真值 → `renderedTools[block.id] = {callHtml}`。
3. `msg.role === "toolResult" && msg.toolCallId`：
   `toolName = msg.toolName || ""`；`existing = renderedTools[msg.toolCallId]`；
   `existing || !TEMPLATE_RENDERED_TOOLS.has(toolName)` 时：
   `rendered = toolRenderer.renderResult(msg.toolCallId, toolName, msg.content, msg.details, msg.isError || false)`；
   真值 → `renderedTools[id] = {...existing, resultHtmlCollapsed: rendered.collapsed,
     resultHtmlExpanded: rendered.expanded}`。
4. 返回映射（**键顺序 = 首次出现顺序**）。

#### 2.22.7 `exportSessionToHtml(sm, state?, options?)`（导出）

1. `opts = typeof options === "string" ? {outputPath: options} : options || {}`（支持旧签名直接传路径）。
2. `sessionFile = sm.getSessionFile()`；无 → `throw new Error("Cannot export in-memory session to HTML")`。
3. `!existsSync(sessionFile)` → `throw new Error("Nothing to export yet - start a conversation first")`。
4. `entries = sm.getEntries()`。
5. `opts.toolRenderer` 存在 → `renderedTools = preRenderCustomTools(entries, opts.toolRenderer)`；
   空对象 → undefined。
6. `sessionData = {header: sm.getHeader(), entries, leafId: sm.getLeafId(),
   systemPrompt: state?.systemPrompt,
   tools: state?.tools?.map(t => ({name: t.name, description: t.description, parameters: t.parameters})),
   renderedTools}`。
7. `html = generateHtml(sessionData, opts.themeName)`。
8. `outputPath = opts.outputPath ? normalizePath(opts.outputPath) : undefined`；
   无 → `` `${APP_NAME}-session-${basename(sessionFile, ".jsonl")}.html` ``（`APP_NAME` 通常为 `"kiana"`）。
9. `writeFileSync(outputPath, html, "utf8")`；返回 `outputPath`（可能是相对路径，未做 resolve）。

#### 2.22.8 `exportFromFile(inputPath, options?)`（导出）

1. `opts` 同上；`resolvedInputPath = resolvePath(inputPath)`（绝对化 + `~` 展开 + Unicode 空格归一）。
2. `!existsSync` → `` throw new Error(`File not found: ${resolvedInputPath}`) ``。
3. `sm = SessionManager.open(resolvedInputPath)`（**会做迁移**；迁移写盘行为由 ca-session 单元负责）。
4. `sessionData = {header, entries, leafId, systemPrompt: undefined, tools: undefined}`
   （**没有 renderedTools**）。
5. `html = generateHtml(...)`；输出路径默认
   `` `${APP_NAME}-session-${basename(resolvedInputPath, ".jsonl")}.html` ``。
6. `writeFileSync` + 返回。

调用点：`main.ts` 的 `--export`（CLI）、`core/agent-session.ts` 的 `exportToHtml`
（构造 `createToolHtmlRenderer({getToolRenderers: name => extensionRunner.resolveToolRenderers(name,
() => this.getToolDefinition(name)), theme, cwd: sessionManager.getCwd()})`，
并挑选 themeName：`[options.themeName, settingsManager.getTheme()].find(c => c !== undefined && getThemeByName(c) !== undefined)`）。

### 2.23 `src/core/export-html/ansi-to-html.ts`

#### 2.23.1 调色板与 256 色

```ts
const ANSI_COLORS = ["#000000","#800000","#008000","#808000","#000080","#800080","#008080","#c0c0c0",
                     "#808080","#ff0000","#00ff00","#ffff00","#0000ff","#ff00ff","#00ffff","#ffffff"];
```

`color256ToHex(index)`：
- `< 16` → `ANSI_COLORS[index]`；
- `< 232`：`cubeIndex = index - 16`；`r = floor(cubeIndex/36)`、`g = floor((cubeIndex%36)/6)`、`b = cubeIndex%6`；
  `toComponent(n) = n === 0 ? 0 : 55 + n*40`；`toHex(n) = toComponent(n).toString(16).padStart(2,"0")`；
  返回 `` `#${toHex(r)}${toHex(g)}${toHex(b)}` ``（小写 hex）；
- 否则：`gray = 8 + (index-232)*10`；`grayHex = gray.toString(16).padStart(2,"0")`；
  返回 `#${grayHex}${grayHex}${grayHex}`。

#### 2.23.2 `escapeHtml(text)`（内部）

按顺序：`&`→`&amp;`、`<`→`&lt;`、`>`→`&gt;`、`"`→`&quot;`、`'`→`&#039;`。
（注意与 `template.js` 里的 `escapeHtml` 不同：后者用 `&#39;`。两处不可混用。）

#### 2.23.3 样式

`TextStyle {fg: string|null; bg: string|null; bold; dim; italic; underline}`；
`createEmptyStyle()` 全 null/false。

`styleToInlineCSS(style)`：按固定顺序 push：
`color:${fg}`、`background-color:${bg}`、`font-weight:bold`、`opacity:0.6`（dim）、
`font-style:italic`、`text-decoration:underline`；`";"` 连接（**无空格**）。

`hasStyle(style)`：任一非默认。

`applySgrCode(params, style)`：`while i < params.length`：
- `0` 全重置；`1` bold=true；`2` dim=true；`3` italic=true；`4` underline=true；
  `22` bold=dim=false；`23` italic=false；`24` underline=false；
  `30..37` fg=ANSI_COLORS[code-30]；`39` fg=null；`40..47` bg=ANSI_COLORS[code-40]；`49` bg=null；
  `38`：`params[i+1]===5 && params.length > i+2` → fg=color256ToHex(params[i+2])、`i+=2`；
           `params[i+1]===2 && params.length > i+4` → fg=`rgb(r,g,b)`（R/G/B 为 params[i+2..i+4]，**逗号无空格**）、`i+=4`；
  `48`：同上但 bg；
  `90..97` fg=ANSI_COLORS[code-90+8]；`100..107` bg=ANSI_COLORS[code-100+8]；
  其余忽略；`i++`。
  注意 `38`/`48` 后若参数不足（`38;5` 缺 N，或 `38;2;1;2` 缺 B），不改变样式且 `i` 只加 1
  （后续数字会被当作独立 code 解析，通常被忽略）。

#### 2.23.4 `ANSI_REGEX = /\x1b\[([\d;]*)m/g`

只匹配 SGR（`m` 结尾），不支持 OSC/其他 CSI。参数串为 `[\d;]*`（可空）。

#### 2.23.5 `export function ansiToHtml(text)`

1. `style = empty`；`result = ""`；`lastIndex = 0`；`inSpan = false`；`ANSI_REGEX.lastIndex = 0`。
2. 循环 `exec`：`beforeText = text.slice(lastIndex, match.index)`，非空 → `escapeHtml`；
   `params = paramStr ? paramStr.split(";").map(p => parseInt(p,10) || 0) : [0]`
   （空参数串 → `[0]`；`"1;;3"` → `[1,0,3]`；非数字段 → 0）。
   有开着的 span → 先 `</span>`，`inSpan=false`；`applySgrCode(params, style)`；
   `hasStyle(style)` → 输出 `<span style="${styleToInlineCSS(style)}">` 并置 `inSpan=true`；
   `lastIndex = match.index + match[0].length`。
3. 剩余文本 `escapeHtml`；若有 span 收尾 `</span>`。

（样式在**每个转义序列处重新开 span**，即使样式未变；因此连续同色片段会产生相邻 span，后续换行/拼接才有差异。）

#### 2.23.6 `export function ansiLinesToHtml(lines)`

`lines.map(line => `<div class="ansi-line">${ansiToHtml(line) || "&nbsp;"}</div>`).join("")`
——**行间无任何空白字符**（测试 `does not insert source whitespace between ANSI-rendered lines`）；
空行（转换后空串）填 `&nbsp;`。

### 2.24 `src/core/export-html/tool-renderer.ts`

- `ToolHtmlRendererDeps { getToolRenderers(name): ToolRenderers | undefined; theme: Theme; cwd: string; width?: number /* 默认 100 */ }`
- `const ANSI_ESCAPE_REGEX = /\x1b\[[\d;]*m/g;`
- `isBlankRenderedLine(line)`：去掉 ANSI 后 `trim().length === 0`。
- `trimRenderedResultLines(lines)`：去掉头部与尾部空白行（只去两端，中间保留）。
- `createToolHtmlRenderer(deps)`：
  - 四个 Map：`renderedCallComponents`、`renderedResultComponents`、`renderedStates`、`renderedArgs`（按 toolCallId）。
  - `getState(id)`：首次 `{}`。
  - `createRenderContext(toolCallId, lastComponent, expanded, isPartial, isError)`：
    ```ts
    { args: renderedArgs.get(toolCallId), toolCallId, invalidate: () => {}, lastComponent,
      state: getState(toolCallId), cwd, executionStarted: true, argsComplete: true,
      isPartial, expanded, showImages: false, isError, durationMs: undefined, outputPad: 1 }
    ```
  - `renderCall(toolCallId, toolName, args)`：
    `renderedArgs.set(id, args)`；`toolDef = getToolRenderers(toolName)`；无 `renderCall` → undefined；
    `component = toolDef.renderCall(args, theme, createRenderContext(id, last, false, true, false))`；
    保存 component；`lines = component.render(width)`；返回 `ansiLinesToHtml(lines)`；
    **任何异常 → undefined**（调用方回退到结构化渲染）。
  - `renderResult(id, name, result, details, isError)`：
    `toolDef?.renderResult` 缺失 → undefined；
    `agentToolResult = {content: result, details, isError}`；
    collapsed：`toolDef.renderResult(agentToolResult, {expanded:false, isPartial:false}, theme,
      createRenderContext(id, last, false, false, isError))` → 保存 → `ansiLinesToHtml(trimRenderedResultLines(component.render(width)))`；
    expanded：同上但 `expanded: true`，且 `lastComponent` 为**刚渲染的 collapsed 组件**；
    返回 `{...(collapsed && collapsed !== expanded ? {collapsed} : {}), expanded}`；
    异常 → undefined。
  - 关键：渲染宽度固定 100（不随导出设备变化），`isPartial:false`，`showImages:false`，
    `durationMs: undefined`（故 timer 行会走 `Elapsed/Took` 的本地时钟路径——`startedAt` 因
    `executionStarted` 为 true 会在首次 renderCall 时被设置为当前时间，输出 `Took 0.0s`；
    这是既定行为，Rust 需照抄：**预渲染的时长文本不可信但必须一致**）。

### 2.25 `src/core/export-html/template.html`（逐字结构）

56 行 / 2271 字节，固定 DOM（**下为逐字节内容**，勿按视觉重排换行）：

```
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Session Export</title>
  <style>
{{CSS}}
  </style>
</head>
<body>
  <button id="hamburger" title="Open sidebar"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" stroke="none"><circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><rect x="5" y="6" width="2" height="12"/><path d="M6 12h10c1 0 2 0 2-2V8"/></svg></button>
  <div id="sidebar-overlay"></div>
  <div id="app">
    <aside id="sidebar">
      <div class="sidebar-header">
        <div class="sidebar-controls">
          <input type="text" class="sidebar-search" id="tree-search" placeholder="Search...">
        </div>
        <div class="sidebar-filters">
          <button class="filter-btn active" data-filter="default" title="Hide settings entries">Default</button>
          <button class="filter-btn" data-filter="no-tools" title="Default minus tool results">No-tools</button>
          <button class="filter-btn" data-filter="user-only" title="Only user messages">User</button>
          <button class="filter-btn" data-filter="labeled-only" title="Only labeled entries">Labeled</button>
          <button class="filter-btn" data-filter="all" title="Show everything">All</button>
          <button class="sidebar-close" id="sidebar-close" title="Close">✕</button>
        </div>
      </div>
      <div class="tree-container" id="tree-container"></div>
      <div class="tree-status" id="tree-status"></div>
    </aside>
    <div id="sidebar-resizer" role="separator" aria-orientation="vertical" aria-label="Resize session tree sidebar"></div>
    <main id="content">
      <div id="header-container"></div>
      <div id="messages"></div>
    </main>
    <div id="image-modal" class="image-modal">
      <img id="modal-image" src="" alt="">
    </div>
  </div>

  <script id="session-data" type="application/json">{{SESSION_DATA}}</script>

  <!-- Vendored libraries -->
  <script>{{MARKED_JS}}</script>
  <script>{{MARKED_CJK_FRIENDLY_JS}}</script>

  <!-- highlight.js -->
  <script>{{HIGHLIGHT_JS}}</script>

  <!-- Main application code -->
  <script>
{{JS}}
  </script>
</body>
</html>
```

文件 SHA-256：`47d208235b066c856dae69c60281a3b99d164b99be9b81153d880375c9327105`。
Rust 必须**按字节**打包该文件（不要重排空白/属性顺序）。

### 2.26 `src/core/export-html/template.css`

1085 行 / 23131 字节，SHA-256 `c9d26cc314336192021d604c0f1a4a63115172b24d03500571379f9172c06fc9`。
只有 4 个占位符（第 2-5 行）：`{{THEME_VARS}}`、`{{BODY_BG}}`、`{{CONTAINER_BG}}`、`{{INFO_BG}}`。
关键布局常量（必须保持）：

- `:root { --line-height: 18px; --sidebar-width: 400px; --sidebar-min-width: 240px;
  --sidebar-max-width: 840px; --sidebar-resizer-width: 6px; }`
- `body { font-family: ui-monospace, 'Cascadia Code', 'Source Code Pro', Menlo, Consolas,
  'DejaVu Sans Mono', monospace; font-size: 12px; line-height: var(--line-height);
  color: var(--text); background: var(--body-bg); }`
- 其余为会话视图样式（`.ansi-line`、`.msg`、`.tool-*`、`.tree-*`、`.header-*`、`.image-modal` 等）。
- Rust 侧**不要**改写 CSS：直接以 `include_str!` 内嵌同一文件。

### 2.27 `src/core/export-html/template.js`（2044 行 / 86652 字节）

SHA-256 `14fbe96123a82ee0f3efdd9f9634dd316c59810509fdbd8c5a5f41f3918c07a4`。
这是一个 IIFE 浏览器脚本，**必须原样嵌入**，Rust 不重写。其契约（导出 HTML 的行为验收点）：

#### 2.27.1 数据装载

- 读 `#session-data` 的文本 → `atob` → `Uint8Array` → `TextDecoder` → `JSON.parse`。
  得到 `{header, entries, leafId, systemPrompt, tools, renderedTools}`。
- URL 参数来源（逐字）：`const injectedParams = document.querySelector('meta[name="kiana-url-params"]')`；
  `searchString = injectedParams ? injectedParams.content : window.location.search.substring(1)`，
  再 `new URLSearchParams(searchString)`。**不是** `getElementById`，也不是 `.textContent`。
  参数名为 `leafId`（覆盖默认 leaf，`urlLeafId || defaultLeafId`）与 `targetId`（深链目标条目；
  注意是 **targetId**，不是 `target`）。二者均为 `URLSearchParams.get`，缺失为 `null`。
- 建立 `byId`、`toolCallMap`（toolCallId → 调用块）、`labelMap`（entry id → label）。

#### 2.27.2 树构建与拍平

- `buildTree()`：按 `timestamp`（`new Date(...).getTime()`）排序子节点（新在后）；返回 roots。
- `buildActivePathIds(leafId)`：从 leaf 回溯到 root 的 id 集合。
- `getPath(entryId)`：root→该节点。
- `findNewestLeaf(nodeId)`：迭代（不用递归），沿每个节点的最后一个子节点到底。
- `flattenTree(roots, activePathIds)`：**迭代**后序算 `containsActive`；再用显式栈做前序，
  产出 `{node, indent, showConnector, isLast, gutters, isVirtualRootChild, multipleRoots}`。
  缩进规则（与 `tree-selector.ts` 完全一致）：
  - `multipleRoots` → 根 indent = 1 且 `showConnector = true`；
  - 子节点 indent：`multipleChildren` → `indent+1`；否则 `justBranched && indent > 0` → `indent+1`；否则 `indent`；
  - gutters：`connectorDisplayed = showConnector && !isVirtualRootChild`；
    `connectorPosition = max(0, (multipleRoots ? max(0, indent-1) : indent) - 1)`；
    子 gutters = `connectorDisplayed ? [...gutters, {position: connectorPosition, show: !isLast}] : gutters`；
  - 排序：活动分支优先（`Number(containsActive.get(b)) - Number(containsActive.get(a))`，稳定排序）。
  - 栈元素为 `[node, indent, justBranched, showConnector, isLast, gutters, isVirtualRootChild]`：
    roots 压入 `[root, multipleRoots ? 1 : 0, multipleRoots, multipleRoots, isLast, [], multipleRoots]`；
    **子节点压入 `[child, childIndent, multipleChildren, multipleChildren, childIsLast, childGutters, false]`**
    —— 即子节点的 `justBranched` 与 `showConnector` 都等于**父节点的 `multipleChildren`**（不是继承父值），
    `isVirtualRootChild` 恒为 `false`。`isLast` 是「在本父节点的排序后子列表里是否为最后一个」。
  - **必须能处理 2 万层深的单链**（测试 `export-html-deep-tree`：`builds and flattens a single-child chain
    deeper than the call stack`），故不得用递归。
- `buildTreePrefix(flatNode)`：**只接受一个 `flatNode` 参数**（解构 `indent, showConnector, isLast,
  gutters, isVirtualRootChild, multipleRoots`）。`displayIndent = multipleRoots ? max(0, indent-1) : indent`；
  `connector = showConnector && !isVirtualRootChild ? (isLast ? '└─ ' : '├─ ') : ''`；
  `connectorPosition = connector ? displayIndent - 1 : -1`。
  前缀按 **3 字符一格**逐字符构造（`totalChars = displayIndent * 3`，`level = floor(i/3)`、
  `posInLevel = i % 3`）：
  - 该 level 在 `gutters` 里 → `posInLevel === 0` 时 push `gutter.show ? '│' : ' '`，其余位置 `' '`；
  - 否则若 `connector && level === connectorPosition` → `pos === 0` push `isLast ? '└' : '├'`，
    `pos === 1` push `'─'`，`pos === 2` push `' '`；
  - 否则 `' '`。
  即连接符是 `└`/`├` + `─` + 空格三字符，**不是** `"└ "` 这类两字符串；gutter 只有 `│` 一个字符，
  剩余两格为空格。返回值 `prefixChars.join('')`。
- 树行 HTML：`prefixSpan.textContent = prefix`；`marker.textContent = isOnPath ? '•' : ' '`；
  `content.innerHTML = getTreeNodeDisplayHtml(entry, label)`。
- 状态行：`` document.getElementById('tree-status').textContent = `${filtered.length} / ${flatNodes.length} entries` ``。

**筛选后的视觉重算**（`recalculateVisualStructure(filteredNodes, allFlatNodes)`，2.27.3 过滤后调用）：
过滤会删掉中间节点，因此必须**在可见子集上重跑与 `flattenTree` 相同的缩进/连接符算法**，
否则子节点会悬空。逐字语义：
1. 空 `filteredNodes` → 直接返回（不改任何节点的视觉属性）。
2. `visibleIds = 过滤后节点 id 集合`；`entryMap` 用**完整** `allFlatNodes` 建（用来向上找祖先）。
3. `findVisibleAncestor(nodeId)`：从 `entryMap.get(nodeId)?.node.entry.parentId` 起向上，返回第一个在
   `visibleIds` 里的祖先；`null` 表示根级。
4. 建 `visibleParent: Map<id, ancestorId>` 与 `visibleChildren: Map<ancestorId|null, id[]>`（**先**
   `visibleChildren.set(null, [])`）；按 `filteredNodes` 的既有顺序 push（保持 flatten 顺序）。
5. `visibleRootIds = visibleChildren.get(null)`；`multipleRoots = visibleRootIds.length > 1`
   （**注意**：这里用「可见根数」，而 `flattenTree` 用全局 `roots.length`——筛选后可能从 1 个根变成多个可见根）。
6. 用**同一个栈布局** `[nodeId, indent, justBranched, showConnector, isLast, gutters, isVirtualRootChild]`
   做 DFS，把结果**写回** `flatNode` 的 `indent/showConnector/isLast/gutters/isVirtualRootChild/multipleRoots`
   （即原地改对象，随后 `buildTreePrefix(flatNode)` 直接吃这些字段）。
   根压栈 `[id, multipleRoots?1:0, multipleRoots, multipleRoots, isLast, [], multipleRoots]`；
   子压栈 `[childId, childIndent, multipleChildren, multipleChildren, childIsLast, childGutters, false]`；
   `childIndent`/`childGutters` 的公式与 2.27.2 完全相同（`multipleChildren` 或
   `justBranched && indent>0` → `+1`，否则不变）。
7. `filteredNodeMap` 里查不到的 id → `continue`（`filteredNodes` 与 `allFlatNodes` 不一致时的防御）。

- `renderEntryToNode(entry)`：`entryCache.has(entry.id)` → 返回 `entryCache.get(id).cloneNode(true)`；
  否则 `html = renderEntry(entry)`；空 → `null`；用 `<template>` 解析，取 `content.firstElementChild`；
  真值 → 写入 `entryCache`（**存 clone**）并返回 `node`。
- `forceTreeRerender()`：`treeRendered = false`；`renderTree()`（搜索框 input 与筛选按钮都调用它）。
- `carriesSystemState(entry)`：`(type==='message' && message.role==='system') || (type==='compaction' && entry.systemMessage)`。
- 侧栏宽度四件套：
  - `clampSidebarWidth(width)` = `max(minWidth, min(maxWidth, width))`（边界来自 `getSidebarBounds()`）；
  - `applySidebarWidth(width)`：`document.documentElement.style.setProperty('--sidebar-width', `${Math.round(clampSidebarWidth(width))}px`)`；
  - `loadSidebarWidth()`：`localStorage.getItem`；`null` → null；`Number(raw)` 非有限 → null；异常吞掉 → null；
  - `saveSidebarWidth(width)`：`String(Math.round(clampSidebarWidth(width)))`，异常吞掉；
  - `setupSidebarResize()`：先 `loadSidebarWidth()` 非 null 则 `applySidebarWidth`；`!sidebarResizer` → return；
    拖拽用 `pointerdown` + `setPointerCapture`，`startWidth = sidebar.getBoundingClientRect().width`，
    move 时 `applySidebarWidth(startWidth + dx)`，`cleanupDrag` 里 `saveSidebarWidth(当前宽度)`；
    `dblclick` → `applySidebarWidth(400)` + `saveSidebarWidth(400)`；`window.resize` → 重新
    `applySidebarWidth(当前宽度)`（即重新 clamp）。**移动端三个入口（pointerdown/dblclick/resize）都直接 return**。

#### 2.27.3 过滤与搜索

- 过滤模式：`default`（隐藏 settings 条目）、`no-tools`（default 再去掉 toolResult）、`user-only`
  （仅 user 消息）、`labeled-only`（仅有 label）、`all`。
- `isSettingsEntry` 的**逐字**类型列表（这 5 种在 `default`/`no-tools` 下隐藏，`all` 下显示）：
  `['label', 'custom', 'context_edit', 'model_change', 'thinking_level_change']`。
  注意 `custom_message`、`compaction`、`branch_summary` **不在**其中（默认可见）。
- **当前 leaf 永远显示**（`entry.id === currentLeafId` 时在过滤前直接 `return true`）。
- **仅含 toolCall 的 assistant 消息默认隐藏**：`entry.type === 'message' && role === 'assistant'` 且
  `!hasTextContent(msg.content)` 且**不是**错误/中止（`msg.stopReason && stopReason !== 'stop' && !== 'toolUse'`）
  → `return false`。`hasTextContent` 对 string 看 `trim().length > 0`；对数组看是否存在
  `type === 'text' && text.trim().length > 0`。
- 搜索：`searchQuery.toLowerCase().split(/\s+/).filter(Boolean)`；每个 token 必须命中节点的可搜索文本，
  `searchTokens.every(t => nodeText.includes(t))`（`nodeText` 已整体 `toLowerCase()`）。
  `getSearchableText(entry, label)` 逐字组成：label；`message` → `msg.role`，system 追加
  `'system prompt' | 'system update'` + `getSystemMessageText(msg)` + `sections` 的键名 +
  `toolsAdded/toolsRemoved` 的 `tool.name`；其余 `extractContent(msg.content)`，bashExecution 追加 `command`；
  `custom_message` → `customType` + 内容；`compaction` → 字面量 `'compaction'`；
  `branch_summary` → `'branch summary'` + `summary`；`model_change` → `'model'` + `modelId`；
  `thinking_level_change` → `'thinking'` + `thinkingLevel`；
  `context_edit` → `'context edit'` + (`replacement === null ? 'omit' : 'replace'`) + `targetId`。
  注意 `compaction` 条目**不**把 summary 纳入搜索文本（只有字面量 `compaction`），
  而 `branch_summary` 会把 summary 纳入。
- `filterNodes` 后重建 indent/gutters（具体算法见 2.27.2 末的 `recalculateVisualStructure`）；
  首次全量 `renderTree`，之后只更新 class/marker（性能）。`renderTree()` 的逐字流程：
  1. `tree = buildTree()`；`activePathIds = buildActivePathIds(currentLeafId)`；
     `flatNodes = flattenTree(tree, activePathIds)`；`filtered = filterNodes(flatNodes, currentLeafId)`。
  2. `!treeRendered`（首次或 `forceTreeRerender` 之后）：`container.innerHTML = ''`，为每个 flatNode 建
     `<div class="tree-node">`（`in-path` 当 `activePathIds.has(id)`；`active` 当 `id === currentTargetId`；
     `dataset.id = entry.id`），依次 append `.tree-prefix`（`textContent = buildTreePrefix(flatNode)`）、
     `.tree-marker`（`textContent = isOnPath ? '•' : ' '`）、`.tree-content`
     （`innerHTML = getTreeNodeDisplayHtml(entry, flatNode.node.label)`）；**click 绑定**：
     `window.getSelection().toString()` 非空则忽略（拖选不跳转），否则
     `leafId = findNewestLeaf(entry.id)`，`navigateTo(leafId, 'target', entry.id)`（**滚到被点的节点，
     但跳到该子树最新叶**）。结束后 `treeRendered = true`。
  3. 否则（增量更新）：遍历现有 `.tree-node`，按 `dataset.id` 重算 `in-path`/`active` class 并更新
     `.tree-marker` 文本；**不重建 DOM**，也不更新 prefix（因此筛选变化必须走 `forceTreeRerender`）。
  4. `#tree-status` 文本 `` `${filtered.length} / ${flatNodes.length} entries` ``。
  5. `setTimeout(..., 0)` 后把 `.tree-node.active` `scrollIntoView({block: 'nearest'})`。

#### 2.27.4 消息渲染

- `renderEntry(entry, ...)` 按 `entry.type`：
  - `message` + `user`：`parseSkillBlock` 命中 → 渲染 skill 头块（`[skill] {name}` 标签、
    `{name} (click to expand)` 折叠行、markdown 内容）**加**用户正文（含图片）两个兄弟块；
    否则普通用户气泡（markdown；text 为**空/纯空白时不输出** markdown 容器）。
  - `message` + `assistant`：先遍历 text/thinking 块，再单独遍历 toolCall 块；
    `text.trim()` 为空不渲染；`thinking.trim()` 为空不渲染；thinking 块为
    `<div class="thinking-block"><div class="thinking-text">…</div><div class="thinking-collapsed">Thinking ...</div></div>`
    （**省略号是三个半角点 + 空格前导**：字面量 `Thinking ...`），默认展开（见 2.27.6）；
    `stopReason === 'aborted'` 追加 `<div class="error-text">Aborted</div>`；
    `stopReason === 'error'` 追加 `<div class="error-text">Error: {errorMessage || 'Unknown error'}</div>`。
  - `message` + `bashExecution`：`<div class="tool-execution {error|success}">`，
    `isError = msg.cancelled || (exitCode !== 0 && exitCode !== null)`；
    `<div class="tool-command">$ {command}</div>`；`output` 走 `formatExpandableOutput(output, 10)`；
    `cancelled` → `<div style="color: var(--warning)">(cancelled)</div>`；
    否则非 0 退出码 → `<div style="color: var(--error)">(exit {code})</div>`（`exitCode === null` 不显示）。
  - `message` + `toolResult` → 返回 `''`（结果由对应的 toolCall 行渲染）。
  - `system` 条目：`isLeadingSystemEntry(entry)`（向上回溯直到遇到另一个携带 system 状态的父条目；
    无 → true）时渲染 `renderSystemPromptBlock('System Prompt', text)` + `renderToolsList('Available Tools',
    msg.toolsAdded)`；否则渲染 `renderSystemPromptBlock(\`System Prompt Update${changes ? ': ' + changes : ''}\`, text)`
    + `renderToolsList('Tools Added', msg.toolsAdded)` + `Tools Removed` 列表；
    `changes` 由 `describeSystemUpdate` 生成（见下）；整块包在 `<div class="system-entry">`，空则不渲染。
  - `model_change`：`<div class="model-change">Switched to model: <span class="model-name">{provider}/{modelId}</span></div>`。
  - `compaction`：`<div class="compaction">` 含 `[compaction]` 标签、折叠行
    `Compacted from {tokensBefore.toLocaleString()} tokens`、内容区
    `<strong>Compacted from {tokensBefore.toLocaleString()} tokens</strong>\n\n{escapeHtml(summary)}`；
    点击切换 `expanded`（也受 O 键控制）。
  - `branch_summary`：`<div class="branch-summary">` + 标题 `Branch Summary` + markdown 渲染的 summary
    （**summary 走 markdown，不转义**）。
  - `custom_message`：`<div class="hook-message[ hook-message-hidden]">` + `[{customType}]`
    （`display === false` 时追加 ` · Hidden in terminal`）+ markdown 内容
    （string 原样；否则 `JSON.stringify(content)`）；`hook-message-hidden` 由 H 键控制显示。
- `describeSystemUpdate(msg)`（逐字）：`systemContentText(msg.content)` 非空 → push `'instructions'`；
  每个 `sections` 项 → `value === null ? \`-${name}\` : name`；
  `toolsAdded` → `+${t.name}`、`toolsRemoved` → `-${t.name}`，非空时整体 push
  `` `tools ${changes.join(' ')}` ``；最后 `parts.join(', ')`（例：`instructions, project_context, -rules, tools +bash -read`）。
- `renderSystemMessageUpdate(msg)`（后续 system 条目的文本）：先 push `systemContentText(msg.content)`（非空时）；
  每个 `sections` 项 → `Removed system prompt section "{name}".`（null）或
  `Updated system prompt section "{name}":\n\n{value}`；`join('\n\n')`。
- 三个文本辅助（**注意连接符不同**）：
  - `systemContentText(content)`：string 原样；数组则为 `type==='text' && text` 的 `text` 用 **`'\n'`** 连接；
    否则 `''`。
  - `getSystemMessageText(msg)`：`[systemContentText(content), ...非 null 的 sections 值]`，
    过滤掉空串后用 **`'\n\n'`** 连接。
  - `extractContent(content)`：string 原样；数组则为 `type==='text' && text` 的 `text` 用 **`''`（空串）** 连接；
    否则 `''`。
- `isSettingsEntry` 的搜索/显示类型列表见 2.27.3。
- `parseSkillBlock` 正则（**逐字**）：
  `/^<skill name="([^"]+)" location="([^"]+)">\n([\s\S]*?)\n<\/skill>(?:\n\n([\s\S]+))?$/`
- `sanitizeMarkdownUrl(value)`（逐字逻辑）：
  `href = String(value || '').trim().replace(/[\x00-\x1f\x7f]/g, '')`；
  `!href` → 返回 `href`（空串）；
  `scheme = href.match(/^([A-Za-z][A-Za-z0-9+.-]*):/)`；有 scheme 且 `!/^(https?|mailto|tel|ftp)$/i.test(scheme[1])`
  → **返回 `null`**（调用方据此不输出 `href`/`src`）；否则返回 `href`。
  无 scheme 的相对 URL（`foo/bar`、`#anchor`）直接通过。
- `escapeHtml`（JS 侧）：`& < > " '` → `&amp; &lt; &gt; &quot; &#39;`（**`&#39;` 不是 `&#039;`**）；
  且先 `String(text)`（`null` → `"null"`）。
- `findToolResult(toolCallId)`：线性扫描 `entries`，找 `type==="message" && role==="toolResult" &&
  toolCallId === id` 的**第一条**；无 → `null`。
- `formatExpandableOutput(text, maxLines, lang)`：`replaceTabs`（tab → 3 空格）；
  `displayLines = lines.slice(0, maxLines)`；`remaining = lines.length - maxLines`；
  有 `lang` 时用 `hljs.highlight(text, {language: lang})`（抛错回退 `escapeHtml`）；
  `remaining > 0` → 折叠块
  ```html
  <div class="tool-output expandable" onclick="if(window.getSelection().toString())return;this.classList.toggle('expanded')">
                <div class="output-preview"><pre><code class="hljs">{previewHighlighted}</code></pre>
                <div class="expand-hint">... ({remaining} more lines)</div></div>
                <div class="output-full"><pre><code class="hljs">{highlighted}</code></pre></div></div>
  ```
  （preview 是对**前 maxLines 行**单独高亮的结果，因此跨行语法高亮在折叠态可能与全文不同）；
  无 `remaining` → `<div class="tool-output"><pre><code class="hljs">{highlighted}</code></pre></div>`；
  无 `lang` 时走纯文本分支（每行 `<div>{escapeHtml(replaceTabs(line))}</div>` 包在 `.tool-output` 内）。
- `getLanguageFromPath(filePath)`（**模板 JS 侧的独立副本**，与 `theme.ts` 的映射表**不同**）：
  `ext = filePath.split('.').pop()?.toLowerCase()`（注意：无 `.` 的路径整段作为 ext；`?.` 表示
  `split` 结果可能为空的防御），然后查一张**恰好 28 个键**的表（逐字）：
  `ts→typescript`、`tsx→typescript`、`js→javascript`、`jsx→javascript`、`py→python`、`rb→ruby`、
  `rs→rust`、`go→go`、`java→java`、`c→c`、`cpp→cpp`、`h→c`、`hpp→cpp`、`cs→csharp`、`php→php`、
  `sh→bash`、`bash→bash`、`zsh→bash`、`sql→sql`、`html→html`、`css→css`、`scss→scss`、
  `json→json`、`yaml→yaml`、`yml→yaml`、`xml→xml`、`md→markdown`、`dockerfile→dockerfile`；
  未命中 → `undefined`。
  **因此 `mjs/cjs/markdown/toml/makefile/kt/swift/ps1/htm/...` 在 HTML 导出中不高亮**，而在 TUI 中会
  （见 5.6 的 `theme.ts` 映射表，键数远多于模板侧）；`h→c`、`hpp→cpp` 亦与 TUI 一致但键集更小。Rust 侧必须**保留两份表**，
  不要把 HTML 导出统一到 theme 的表上。
- 工具行 DOM（模板内联分支，逐字 class 名）：
  - bash：`<div class="tool-command">$ {cmd}</div>`（command 为 null → `<span class="tool-error">[invalid arg]</span>`；
    空串 → `...`；`escapeHtml`），结果 `formatExpandableOutput(output, 5)`；
  - read：`filePath = str(args.file_path ?? args.path)`（**`file_path` 优先**）；path 为空串则 `shortenPath('')`
    → `''`；`filePath === null`（类型不对）→ `[invalid arg]`。`readLineRange = formatReadLineRange(args.offset,
    args.limit)`，**仅当 `filePath !== null && readLineRange` 非空**时追加
    `<span class="line-numbers">{range}</span>`（注意 `<span>` 前**没有**空格，紧贴路径）。
    头为 `<div class="tool-header"><span class="tool-name">read</span> <span class="tool-path">{path + range}</span></div>`，
    结果：先 `renderResultImages()`（`<div class="tool-images">` + 每图
    `<img src="data:{mime};base64,{data}" class="tool-image" />`，mime 缺省 `image/png`），
    再 `formatExpandableOutput(output, 10, lang)`，`lang = filePath ? getLanguageFromPath(filePath) : null`；
  - write：`filePath = str(args.file_path ?? args.path)`；`content = str(args.content)`；头内
    **仅当 content 是非 null 且非空的字符串且 `content.split('\n').length > 10`** 时追加
    ` <span class="line-count">({lines} lines)</span>`（注意前导空格）；`content === null`（类型不对）→
    `<div class="tool-error">[invalid content arg - expected string]</div>`；`content` 非空 →
    `formatExpandableOutput(content, 10, lang)`；结果 `trim()` 后非空 →
    `<div class="tool-output"><div>{escapeHtml(output)}</div></div>`；
  - edit：`filePath = str(args.file_path ?? args.path)`；头之后，若 `result?.details?.diff` 为真：
    `<div class="tool-diff">`，每行按 `/^\+/`→`diff-added`、`/^-/`→`diff-removed`、否则 `diff-context`，
    行内容 `escapeHtml(replaceTabs(line))`；否则若有 result 且 `output.trim()` 非空 →
    `<div class="tool-output"><pre>{escapeHtml(output)}</pre></div>`；
  - ls：`dirPath = str(args.path)`；null → `[invalid arg]`；`limit !== undefined` 时追加
    ` <span class="line-count">(limit {limit})</span>`（转义 string）；结果 `trim()` 后
    `formatExpandableOutput(output, 20)`；
  - 所有工具行外层 `<div class="tool-execution {pending|success|error}" id="tool-call-{escapeHtml(call.id)}">`，
    结尾追加 `renderNestedCalls()`（`Nested calls: {n}[ (incomplete record)]` 标题 + 每行
    `{✓|✗|…} {name} {JSON args | "[arguments omitted, N bytes]"} {durationMs}ms` + 缩进错误文本，
    用 `formatExpandableOutput(text, 1)` 渲染）。
- 工具行渲染 `renderToolCall(...)`：
  - 状态类 `pending`/`success`/`error`；
  - 内置模板分支：`bash`、`read`、`write`、`edit`、`ls`；
  - 其他工具（`default` 分支，逐字逻辑）：
    - 若 `renderedTools?.[call.id]` 的 `callHtml` / `resultHtmlCollapsed` / `resultHtmlExpanded`
      **任一**存在：`callHtml` 存在 → `<div class="tool-header ansi-rendered">{callHtml}</div>`；
      否则 `<div class="tool-header"><span class="tool-name">{name}</span></div>`。
      结果：`collapsed && expanded && collapsed !== expanded` → 可展开块
      （`output-preview` 放 collapsed、`output-full` 放 expanded，外层 `tool-output expandable ansi-rendered`）；
      否则 `resultHtmlExpanded` 存在 → `<div class="tool-output ansi-rendered">{expanded}</div>`；
      否则若 result 的文本非空 → `formatExpandableOutput(output, 10)`。
    - 完全无预渲染 → 头 + `<div class="tool-output"><pre>{escapeHtml(JSON.stringify(args, null, 2))}</pre></div>`
      + 结果文本 `formatExpandableOutput(output, 10)`。
    - 预渲染 HTML **不再转义**（已由 tool-renderer 产出 HTML），直接拼进字符串。
  - `nestedCalls` 图标：`✓` / `✗` / `…`；
  - `formatExpandableOutput`：最多 5/10/20 行（按工具），用 `hljs.highlight` 高亮文本，
    可展开（`... (N more lines)`）。
- `formatToolCall(name, args)`（树节点显示用；**全部带方括号**，逐字）：
  - `read` → `` `[read: ${shortenPath(args.path || args.file_path || '')}${formatReadLineRange(args.offset, args.limit)}]` ``；
  - `write` → `` `[write: ${path}]` ``；`edit` → `` `[edit: ${path}]` ``；
  - `bash` → `` `[bash: ${cmd}${rawCmd.length > 50 ? '...' : ''}]` ``，其中
    `cmd = String(args.command || '').replace(/[\n\t]/g, ' ').trim().slice(0, 50)`
    （**先把换行/制表符替换为空格，再 trim，再截 50**；截断标记按**替换前的原始长度** `rawCmd.length > 50` 决定，
    而不是按 `cmd.length` —— 因此原始命令含换行时可能出现 `cmd` 未截断但仍带 `...`）；
  - `grep` → `` `[grep: /${args.pattern || ''}/ in ${shortenPath(String(args.path || '.'))}]` ``（默认路径 `.`）；
  - `find` → `` `[find: ${args.pattern || ''} in ${shortenPath(String(args.path || '.'))}]` ``（默认 `.`）；
  - `ls` → `` `[ls: ${shortenPath(String(args.path || '.'))}]` ``（默认 `.`）；
  - 其他 → `` `[${name}: ${JSON.stringify(args).slice(0, 40)}${JSON.stringify(args).length > 40 ? '...' : ''}]` ``
    （**`JSON.stringify(args)` 被调用两次**，截断标记按完整序列化长度判断）。
- `shortenPath(p)`：**模板侧独立实现，与 `render-utils.shortenPath` 不同**。非 string → `''`；
  以 `/Users/` 或 `/home/` 开头且 `split('/').length > 2` 时，返回
  `'~' + p.slice(('/Users/' + parts[2]).length)`（把 `/<home>/<user>` 换成 `~`，保留其余路径）；
  否则原样。对比：`render-utils.shortenPath` 用 `os.homedir()` 前缀替换（`/root/x` → `~/x` 也行），
  模板侧只认 `/Users/` 与 `/home/` 两个前缀，`/root/...` **不缩短**。
- `truncate(s, maxLen = 100)`：`s.length <= maxLen` → 原样；否则 `s.slice(0, maxLen) + '...'`。
  （`maxLen` 是**包含**在保留部分内的，即超长结果长度为 `maxLen + 3`。）
- `formatReadLineRange(offset, limit)`：与 TUI 相同的 `:start-end` 规则（`offset == null && limit == null` → `''`；
  `offset == null` → start=1；`limit == null` → `:${start}`；`end === 0` 时省略 `-0`）。
  依赖模板自己的 `parseLineNumber(value)`（**与 `render-utils.ts` 的同名私有函数逐字一致**）：
  `typeof value === 'string' && value.trim() !== ''` → `value = Number(value)`；
  再 `typeof value === 'number' && Number.isFinite(value)` → 返回，否则 `undefined`。
  因此 `"3"` → 3、`""` → undefined、`null`/`undefined` → undefined、`"abc"` → NaN → undefined、
  `Infinity` → undefined；**`offset` 传 `0` 时 `0 == null` 为 false**，`start = 0`（不是 1）。

#### 2.27.5 头部与统计

- `computeStats(entries)` 累计 `userMessages/assistantMessages/toolResults/customMessages/compactions/
  branchSummaries/toolCalls`、`tokens {input,output,cacheRead,cacheWrite}`、`cost {…}`（`cost.total` 优先，
  缺失时回退到分量求和）、`models` 集合（按出现顺序）。
- `renderHeader()`：
  - `<h1>Session: {escapeHtml(header?.id || 'unknown')}</h1>`；
  - help bar 文本逐字：`T toggle thinking · O toggle tools · H toggle hidden messages`
    （`·` 两侧各一个半角空格）；
  - 三个 toggle 按钮 `Toggle thinking` / `Toggle tools` / `Show hidden messages`↔`Hide hidden messages`，
    title 分别为 `Toggle thinking (T)`、`Toggle tools (O)`、
    `Show custom messages marked as hidden in the terminal (H).`；
  - 下载按钮 `↓ JSONL`，title `Download session as JSONL`，`onclick="downloadSessionJson()"`；
  - info 项：`Date:`（`new Date(header.timestamp).toLocaleString()` 或 `unknown`）、
    `Models:`（`, ` 连接或 `unknown`）、`Messages:`（`N user, N assistant, N tool results, N custom,
    N compactions, N branch summaries` 的子集，空则 `0`）、`Tool Calls:`、`Tokens:`
    （`↑n ↓n Rn Wn` 子集，空则 `0`）、`Cost: $${total.toFixed(3)}`；
  - `!hasSystemEntries` 时追加 `renderSystemPromptBlock('System Prompt', systemPrompt)` 与
    `renderToolsList('Available Tools', tools)`（系统提示预览 10 行可展开；工具列表含 required/optional 参数）。
    **注意**：`hasSystemEntries = entries.some(e => e.type === 'message' && e.message.role === 'system')`
    —— 只看 message 条目，**不**看 `compaction.systemMessage`。因此「只有压缩快照携带 system 状态」的会话
    仍会走这段回退，把导出时的 `state.systemPrompt`/`state.tools` 追加到头部之后。
    另：`systemPrompt` 为空字符串时不渲染该块（`if (systemPrompt)`），而 `renderToolsList` 对空数组返回 `''`。
- `formatTokens(count)`：`<1000` → `count.toString()`；`<10000` → `(count/1000).toFixed(1)+"k"`；
  `<1e6` → `Math.round(count/1000)+"k"`；否则 `(count/1e6).toFixed(1)+"M"`。
- `formatTimestamp(ts)`：`!ts` → `''`；否则
  `new Date(ts).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' })`。
  **注意**：带 options 的 `toLocaleTimeString` 用**运行环境默认 locale**；`en-US` 下输出
  **12 小时制 + AM/PM**（实测 `"09:05:03 PM"`），`zh-CN`/`de-DE` 等为 24 小时制。这是浏览器侧行为，
  Rust 不实现；验收时不要假设固定的 24 小时制或 AM/PM 形式。
- `computeStats` 的细节：只有 `message` 条目的 `role === 'assistant'` 才累加 token/cost/toolCalls；
  `models` 为 `provider ? \`${provider}/${model}\` : model` 的插入序去重集合；
  `cost.total` 用 `typeof msg.usage.cost.total === 'number' ? total : 分量和`；
  `toolCalls` 统计 `msg.content.filter(c => c.type === 'toolCall').length`（**含结果未到达的调用**）。
  `msg.usage` 缺失（或 `cost` 缺失）时该条目不贡献 token/成本。

#### 2.27.6 交互

- 键盘：`t` / `o` / `h` 切换 thinking / tool outputs / hidden messages；仅在
  `!e.ctrlKey && !e.metaKey && !e.altKey && !isEditableTarget(activeElement)` 时生效
  （`isEditableTarget` 对 `INPUT`/`TEXTAREA`/`SELECT`/`BUTTON`、`isContentEditable`、
  或 `closest('[contenteditable="true"]')` 返回 true）。
  **`Escape` 不受上述限制**（在可编辑判断之前处理）：清空 `searchInput.value` 与 `searchQuery`
  并 `navigateTo(leafId, 'bottom')`。
- 三个 toggles 的初值（逐字）：`thinkingExpanded = true`、`toolOutputsExpanded = false`、
  `showHiddenMessages = false`。
  - `setThinkingExpanded`：切换 `.thinking-text` 的 `display` 与 `.thinking-collapsed` 的 `display`，
    并更新按钮 `aria-pressed`。
  - `setToolOutputsExpanded`：切换 `.tool-output.expandable` 的 `expanded` class，
    **同时**切换 `.compaction` 与 `.skill-invocation` 的 `expanded` class。
  - `setHiddenMessagesVisible`：切换 `document.body` 的 `show-hidden-messages` class，
    并把按钮文本在 `Show hidden messages` ↔ `Hide hidden messages` 之间切换。
- 深链：初始化时若 `urlTargetId && byId.has(urlTargetId)` → `navigateTo(leafId, 'target', urlTargetId)`；
  否则有 `leafId` → `navigateTo(leafId, 'none')`；否则若无 `leafId` 但 `entries.length > 0` →
  `navigateTo(最后一条 entry 的 id, 'none')`。**`'none'` 不滚动**（只有 `'bottom'` / `'target'` 滚动）。
- `navigateTo(targetId, scrollMode = 'target', scrollToEntryId = null)`，逐字步骤：
  1. `currentLeafId = targetId`；`currentTargetId = scrollToEntryId || targetId`；
  2. `targetEntry = byId.get(currentTargetId)`；`scrollMode === 'target'` 且
     `targetEntry?.type === 'custom_message' && targetEntry.display === false` → `setHiddenMessagesVisible(true)`
     （条目不存在时用可选链兜底，不抛错）；
  3. `path = getPath(targetId)`（**按第一个参数取根→target 的路径**，不是 `scrollToEntryId`；
     因此滚动定位条目可以不在渲染路径上——此时 `getElementById` 找不到，退化为不滚动）；
  4. `renderTree()`；`#header-container.innerHTML = renderHeader()`；`attachHeaderHandlers()`（重新绑定三个
     toggle 按钮的 click，见下）；
  5. 用 `DocumentFragment` + `renderEntryToNode(entry)`（**命中 `entryCache` 时返回 clone**）收集 `path`；
     `messagesEl.innerHTML = ''` 后 append fragment（**整段重建**，不是增量插入）；
  6. **重新应用** `setThinkingExpanded(thinkingExpanded)` 与 `setToolOutputsExpanded(toolOutputsExpanded)`
     （cached 节点带着首次渲染的状态，必须重放；`setHiddenMessagesVisible` 不重放，因为它是 body class）；
  7. `messagesEl.querySelectorAll('.copy-link-btn')` 逐个绑定 click：
     `e.stopPropagation()`；`buildShareUrl(btn.dataset.entryId)` → `copyToClipboard(shareUrl, btn)`；
  8. `setTimeout(0)` 滚动：`'bottom'` → `content.scrollTop = content.scrollHeight`；
     `'target'` → `el = getElementById(getScrollTargetElementId(scrollToEntryId || targetId)) ??
     getElementById(`entry-${scrollToEntryId || targetId}`)`；存在 → `scrollIntoView({block:'center'})`，
     且**仅当 `scrollToEntryId` 非空**时加 `highlight` class 并在 2000ms 后移除。
     `'none'` 不做任何滚动。
  - `getScrollTargetElementId(entryId)`：`byId.get(entryId)` 是
    `type==='message' && message.role==='toolResult' && message.toolCallId` → `` `tool-call-${toolCallId}` ``；
    否则 `` `entry-${entryId}` ``。（toolResult 的 DOM 由对应的 toolCall 块内联渲染，故必须路由。）
  - `attachHeaderHandlers()`：给 `[data-action="toggle-thinking"|"toggle-tools"|"toggle-hidden-messages"]`
    三个选择器（可选链，缺失不报错）绑定 click，分别取反对应 state。
- 侧栏：
  - 拖拽/存储：`localStorage` 键 `kiana-share:v1:sidebar-width`（存 `Math.round(clamp(width))` 的字符串；
    读取时 `Number(raw)` 且 `Number.isFinite` 才用，异常吞掉）。
  - `MIN_CONTENT_WIDTH = 320`；`getSidebarBounds()` 从 CSS 变量读 `--sidebar-min-width`（解析失败回退 240）
    与 `--sidebar-max-width`（解析失败回退 **720**，而 CSS 里是 840），再与
    `window.innerWidth - MIN_CONTENT_WIDTH` 取 `max(minWidth, min(...))`。
  - 移动端 `isMobileLayout()` = `matchMedia('(max-width: 900px)')`；移动端不允许拖拽/双击复位，
    `#sidebar-overlay` 显示。
  - `pointerdown` 拖拽（`setPointerCapture`）；`pointerup`/`pointercancel` 结束并保存；
    `dblclick` 复位到 **400** 并保存；`window.resize` 时重新 clamp。
  - 汉堡按钮：给 `#sidebar` 与 `#sidebar-overlay` 加 `open` class 并隐藏 `#hamburger`；
    `#sidebar-close` 与点击 overlay 关闭。
- `copyToClipboard(text, button)`：优先 `navigator.clipboard.writeText`；失败回退隐藏 `<textarea>` +
  `document.execCommand('copy')`；成功且传入 button 时把 `innerHTML` 换成 `✓`、加 `copied` class，
  **1500 ms** 后还原（**不是 1s**）。
- `renderCopyLinkButton(entryId)`：`<button class="copy-link-btn" data-entry-id="{escapeHtml(entryId)}"
  title="Copy link to this message">` + 内联 SVG（两个 `path`，`stroke-width="2"` 等）。
- `buildShareUrl(entryId)`：base URL 优先 `document.querySelector('meta[name="kiana-share-base-url"]')?.content`，
  否则 `window.location.href.split('?')[0]`；从当前 URL 的 `searchParams` 里找**第一个没有值**的键作为
  gistId（`?abc123` 形态）；参数为 `leafId=currentLeafId` 与 `targetId=entryId`；
  有 baseUrl meta 时返回 `` `${baseUrl}&${params}` ``，否则改写 `url.search`（有 gistId 时
  `` `?${gistId}&${params}` ``，否则 `` `?${params}` ``）后 `url.toString()`。
- `window.downloadSessionJson`：重建原始 JSONL —— 首行 `JSON.stringify({ type: 'header', ...header })`
  （**把 `type:'header'` 放在 header 字段之前**），随后每条 entry 一行 `JSON.stringify(entry)`，
  `lines.join('\n')`（**无尾随换行**）；Blob type `application/x-ndjson`；下载文件名
  `` `${header?.id || 'session'}.jsonl` ``；用临时 `<a>` + `URL.createObjectURL` 触发后 `revokeObjectURL`。

#### 2.27.7 marked 配置（`marked.use({...})`，逐字）

`template.js` 在初始化时调用一次 `marked.use`，以下配置直接影响导出 HTML 的渲染结果
（虽然 Rust 不执行 JS，但**不许替换 vendor marked 版本**，否则这些行为会变）：

```js
const strictStrikethroughRegex = /^(~~)(?=[^\s~])((?:\\.|[^\\])*?(?:\\.|[^\s~\\]))\1(?=[^~]|$)/;

marked.use({
  breaks: true,
  gfm: true,
  tokenizer: {
    html() { return undefined; },      // HTML 当纯文本，与 TUI 渲染器一致
    tag()  { return undefined; },
    del(src) {                          // 严格删除线：~~ 内两端不能是空白/~
      const match = strictStrikethroughRegex.exec(src);
      if (!match) return undefined;
      return { type: 'del', raw: match[0], text: match[2], tokens: this.lexer.inlineTokens(match[2]) };
    },
    emStrong: markedCjkFriendly().tokenizer.emStrong,   // CJK 友好的 **粗体**
  },
  renderer: {
    link(token)  { /* sanitizeMarkdownUrl；null → 只输出 token 内容；否则 <a href=... [title=...]> */ },
    image(token) { /* sanitizeMarkdownUrl；null → escapeHtml(token.text)；否则 <img src=... alt=... [title=...]> */ },
    code(token)  { /* lang && hljs.getLanguage(lang) → hljs.highlight；否则 hljs.highlightAuto；抛错回退 escapeHtml；
                      输出 <pre><code class="hljs">…</code></pre>，高亮结果不再转义 */ },
    codespan(token) { return `<code>${escapeHtml(token.text)}</code>`; },
  },
});
```

- `breaks: true` 使单个 `\n` 变成 `<br>`；`gfm: true` 开启表格/任务列表/autolink。
- `html()`/`tag()` 返回 `undefined` → 原始 `<div>` 等标签按文本显示（与 TUI 的 markdown 渲染一致）。
- link/image 的 `null` 分支**只输出内容/文本**（不输出标签），`xss` 测试依赖此行为。
- `safeMarkedParse = (text) => marked.parse(text)`（不再二次转义；转义由各 renderer 负责）。
- `sanitizeMarkdownUrl(value)`：实现逐字见 **2.27.4**（`String(value||'').trim()` → 剥
  `/C0 + DEL/` → 判 scheme → 白名单 `https?/mailto/tel/ftp`，否则 `null`）。
  两个 renderer 分支都依赖它；**顺序是「先剥控制字符、再判 scheme」**（浏览器同样会剥，
  顺序反了 `java\x00script:` 之类就能绕过白名单）。
- `code(token)` 的语言判定：`lang && hljs.getLanguage(lang)` → `hljs.highlight(code, {language: lang}).value`；
  否则 `hljs.highlightAuto(code).value`；两个分支各自 `try/catch`，抛错 → `escapeHtml(code)`。
  高亮输出**不再转义**（hljs 已转义），直接拼进 `<pre><code class="hljs">…</code></pre>`。

**验收方式**：Rust 导出的 HTML 在浏览器里与 TS 导出的 HTML **逐字节相同**（因为模板、JS、CSS、
base64 数据都相同），因此 Rust 侧的正确性等价于「字符串替换 + base64 + JSON 序列化」正确。
这使 template.js 的行为**不需要**在 Rust 里重新实现，只需保证资产与数据序列化一致。
**前提**：资产必须逐字节一致（SHA-256 见 2.25/2.26/2.27/2.29），且 `template.js` / `template.css` /
`vendor/*` 内含的 `{{` 出现次数必须与 TS 一致（实测：`template.js` 0 处；`vendor/marked.min.js` 0 处；
`template.css` 4 处，即 4 个占位符；`template.html` 6 处占位符 `{{CSS}}`/`{{SESSION_DATA}}`/
`{{MARKED_JS}}`/`{{MARKED_CJK_FRIENDLY_JS}}`/`{{HIGHLIGHT_JS}}`/`{{JS}}`）。
替换顺序若不同，`{{JS}}` 里的字面 `{{...}}` 不会误伤（实测没有），但**顺序仍必须与 TS 相同**以便审计。

### 2.28 本单元直接依赖的工具函数（契约；实现归 utils 单元）

这些函数被本单元大量调用，Rust 侧必须存在且语义一致；其详细规格由 `utils` 所属单元负责，
这里只记录本单元依赖的确切契约。

#### 2.28.1 `src/utils/shell.ts`

```ts
export interface ShellConfig { shell: string; args: string[]; commandTransport?: "argv" | "stdin"; }
export function getShellConfig(customShellPath?: string): ShellConfig;
export const POWERSHELL_ARGS = ["-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-Command"] as const;
export function getPowerShellConfig(): ShellConfig;
export function getShellEnv(): NodeJS.ProcessEnv;
export function sanitizeBinaryOutput(str: string): string;
export function trackDetachedChildPid(pid: number): void;
export function untrackDetachedChildPid(pid: number): void;
export function killTrackedDetachedChildren(): void;
export function killProcessTree(pid: number): void;
```

- `isLegacyWslBashPath(path)`：`win32.normalize(path).toLowerCase()`；`SystemRoot` 存在且形如 `X:\...` 时，
  与 `win32.join(SystemRoot, "System32"|"Sysnative", "bash.exe")` 比较；否则正则
  `/^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$/`。
- `getBashShellConfig(shell)`：legacy WSL → `{shell, args:["-s"], commandTransport:"stdin"}`；否则 `{shell, args:["-c"]}`。
- `getShellConfig(customShellPath?)`：
  1. `customShellPath`：存在 → `getBashShellConfig`；不存在 → 抛
     `` `Custom shell path not found: ${customShellPath}` ``。
  2. win32：`%ProgramFiles%\Git\bin\bash.exe`、`%ProgramFiles(x86)%\Git\bin\bash.exe` 依次探测；
     再 `findExecutableOnPath("bash.exe")`；都失败 → 抛
     ```
     No bash shell found. Options:
       1. Install Git for Windows: https://git-scm.com/download/win
       2. Add your bash to PATH (Cygwin, MSYS2, etc.)
       3. Set shellPath in settings.json

     Searched Git Bash in:
       {path1}
       {path2}
     ```
     （`"No bash shell found. Options:\n" + "  1. ...\n" + "  2. ...\n" + "  3. Set shellPath in settings.json\n\n" +
     "Searched Git Bash in:\n" + paths.map(p => `  ${p}`).join("\n")`。）
  3. 非 win32：`/bin/bash` 存在 → 用之；否则 `findExecutableOnPath("bash")`；否则 `{shell:"sh", args:["-c"]}`。
- `findExecutableOnPath(executable)`：win32 用 `spawnSync("where", [exe], {encoding:"utf-8", timeout:5000, windowsHide:true})`，
  取 stdout 第一行并 `shellPathExists` 校验；Unix 用 `spawnSync("which", [exe], {encoding:"utf-8", timeout:5000})`，
  取 stdout 第一行（不校验存在性，信任 which）。
- `getPowerShellConfig()`：非 win32 → 抛 `"The powershell tool is only available on Windows."`；
  `pwsh.exe` → `%ProgramFiles%\PowerShell\7\pwsh.exe` → `powershell.exe` →
  `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`；都失败 → 抛
  `"No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."`；
  返回 `{shell, args: [...POWERSHELL_ARGS]}`。
- `getShellEnv()`：`binDir = getBinDir()`；在 `process.env` 里找键名**大小写不敏感**等于 `path` 的键
  （找不到用 `"PATH"`）；按 `path.delimiter` 切分并过滤空项；已含 `binDir` 则原样，否则前置；
  返回 `{...process.env, [pathKey]: updatedPath}`（键顺序：原环境后追加/覆写 pathKey 保持在原位置）。
- `sanitizeBinaryOutput(str)`：删除 `[\x00-\x08\x0B\x0C\x0E-\x1F\uFFF9-\uFFFB]`
  （保留 `\t`(09)、`\n`(0A)、`\r`(0D)、`\x1B`(ESC)；删除 U+FFF9..U+FFFB 的 3 个字符）。
- `killProcessTree(pid)`：win32 → `spawn(%SystemRoot%\System32\taskkill.exe, ["/F","/T","/PID",pid],
  {stdio:"ignore", detached:true, windowsHide:true})` 并吞掉 error；
  非 win32：darwin 先 best-effort `process.kill(-pid, "SIGSTOP")`；然后 `process.kill(-pid,"SIGKILL")`，
  失败则 `process.kill(pid,"SIGKILL")`，再失败忽略。

#### 2.28.2 `src/utils/output-files.ts`

```ts
export async function writeOutputFile(prefix: string, extension: string, data: string | Uint8Array): Promise<string>;
export function createOutputFileStream(prefix: string, extension: string): { path: string; stream: WriteStream };
```
- `OUTPUT_FILE_MODE = 0o600`；路径 `${os.tmpdir()}/${prefix}-${randomBytes(8).toString("hex")}${extension}`
  （`extension` 含前导点，例如 `.log`；16 位小写 hex）。
- `writeOutputFile`：`writeFile(path, data, {mode: 0o600, flag: "wx"})`。
- `createOutputFileStream`：`createWriteStream(path, {mode: 0o600, flags: "wx"})`。
- Rust：`OpenOptions::new().write(true).create_new(true).mode(0o600)`（`std::os::unix::fs::OpenOptionsExt`；
  Windows 上无 mode，用默认 ACL，与 Node 行为对齐——Node 在 Windows 忽略 mode）。

#### 2.28.3 `src/utils/child-process.ts`

- `EXIT_STDIO_GRACE_MS = 100`。
- `spawnProcess(command, args, options)`：win32 用 `cross-spawn`，否则 `nodeSpawn`。
- `spawnProcessSync` 同理。
- `waitForChildProcess(child): Promise<number | null>`：
  - 状态：`settled`、`exited`、`exitCode`、`postExitTimer`、`stdoutEnded = child.stdout === null`、
    `stderrEnded = child.stderr === null`。
  - `cleanup()`：清 timer、移除 error/exit/close/end/data 监听。
  - `finalize(code)`：单次；cleanup；`child.stdout?.destroy()`、`child.stderr?.destroy()`；resolve(code)。
  - `maybeFinalizeAfterExit()`：`exited && !settled && stdoutEnded && stderrEnded` → finalize(exitCode)。
  - `armIdleTimer()`：清旧 timer，`setTimeout(() => finalize(exitCode), 100)`。
  - `onData()`：`exited && !settled` → 重新 arm（**每个 chunk 重置 100ms 空闲计时**，见 #5303）。
  - `onStdoutEnd`/`onStderrEnd` → 置标志并 maybeFinalize。
  - `onError(err)` → 单次 reject。
  - `onExit(code)` → `exited = true`、`exitCode = code`、maybeFinalize，未 settled 时 arm。
  - `onClose(code)` → `finalize(code)`（**close 优先于 idle timer**，且用 close 的 code）。
  - 监听：stdout/stderr 的 `end`+`data`、child 的 `error`/`exit`/`close`。
  - 验收：测试 `bash-close-hang-windows`（`executeBash resolves after the shell exits even if inherited
    stdio handles stay open`、`bash tool resolves after ...`）与 `5208-late-bash-output`（晚期输出不丢）。
  - Rust：`tokio::process::Child`；需自己实现「exit 事件 + 每个 chunk 重置的 100ms 空闲计时 + close 语义」。
    `tokio::process` 的 `Child::wait()` 会等 stdio 关闭（如同 Node 的 close），因此需要
    `child.wait()` 与独立的 stdout/stderr 读任务配合，用 `tokio::time::sleep` 重置计时器
    （`tokio::sync::watch` + `select!`），或直接实现「exit 后 100ms 无新数据即返回」。

#### 2.28.4 `src/utils/text.ts`

`splitBom(content)` → `{bom, text}`（`\uFEFF` 前缀）；`stripBom(content)` → `.text`。

#### 2.28.5 `src/utils/ansi.ts`

- `ST = "(?:\\u0007|\\u001B\\u005C|\\u009C)"`；
  `OSC_START = "\\u001B\\]"`；
  `CSI_START = "[\\u001B\\u009B][[\\]()#;?]*(?:\\d{1,4}(?:[;:]\\d{0,4})*)?"`；
  `CSI_FINAL = "[\\dA-PR-TZcf-nq-uy=><~]"`；
  `ansiRegex = new RegExp(`(?:${OSC_START}[\\s\\S]*?${ST})|${CSI_START}${CSI_FINAL}`, "g")`；
  `unfinishedAnsiAtEndRegex = new RegExp(`(?:${OSC_START}(?:[^\\u0007\\u009C\\u001B]|\\u001B(?!\\\\))*|${CSI_START})$`)`；
  `MAX_PENDING_ANSI_LENGTH = 256`。
- `splitIncompleteAnsiSuffix(value)`：无 ESC 且无 U+009B → `{complete: value, pending: ""}`；
  否则在 `value.slice(max(0, len-256))` 上 exec `unfinishedAnsiAtEndRegex`；无匹配 → 同上；
  有 → `splitAt = windowStart + match.index`，`{complete: value.slice(0,splitAt), pending: value.slice(splitAt)}`。
- `stripAnsi(value)`：**非 string → 抛 `TypeError(`Expected a \`string\`, got \`${typeof value}\``)`**；
  无 ESC/009B 快速返回；否则 `value.replace(ansiRegex, "")`。
  验收：`ansi-utils.test.ts` 用 chalk 的 `strip-ansi` 参考实现逐例比对（含 OSC、DCS/PM/APC 字符串、
  RIS、单字节 ESC 序列、8-bit CSI）。
- 本单元使用者：`render-utils.getTextOutput`、`bash-executor`（非本单元）、`tool-renderer`（正则不同，见下）。
- **`splitIncompleteAnsiSuffix` 的流式用法**（`core/bash-executor.ts:108`，`executeBash` 的 `onData`）：
  `({complete, pending} = splitIncompleteAnsiSuffix(pendingAnsi + decoder.decode(data,{stream:true})))`，
  `pendingAnsi = pending`，只把 `complete` 交给 `sanitizeBinaryOutput(stripAnsi(...)).replace(/\r/g,"")`；
  `flushOutput()` 时 `rest = pendingAnsi + decoder.decode()` 再 sanitize。**顺序是先拼 pending、再解码、
  再切分**——若把解码放在切分之后，被 chunk 切开的转义序列会泄漏成可见字符。
  实测验收（`suite/agent-session-bash-persistence.test.ts` 的 "escape sequences split across output chunks"）：
  `["\x1b[31mERROR: file.py:1\x1b[0", "m\n"]` → `"ERROR: file.py:1\n"`；
  `["before\x1b", "[32mafter\n"]` → `"beforeafter\n"`；
  `["a\x1b]0;window ", "title\x1b", "\\b\n"]` → `"ab\n"`（**第三块是字面反斜杠 + `b` + 换行**，
  即 TS 源码里的 `"\\b\n"`；这里 `ESC \` 才构成 ST，纯 `\b` 不构成——照抄时不要写成 `"\b\n"`）；
  末尾半个 `é` → `ok\uFFFD`；`"\x1b]" + "x"*300`（超过 256 窗口）→ 流式回调立刻看到 `"]" + "x"*300`（不扣留）。
  注意 `MAX_PENDING_ANSI_LENGTH = 256` 是**窗口**：更长的未终止序列按原样处理（不无限缓冲）。

#### 2.28.6 `src/utils/mime.ts` / `src/utils/image-process.ts`

- `detectSupportedImageMimeTypeFromFile(path)`：只读**前 4100 字节**（`IMAGE_TYPE_SNIFF_BYTES = 4100`）
  交给 `detectSupportedImageMimeType(buffer)`，按文件魔数（**不是扩展名**）返回
  `image/png`、`image/jpeg`、`image/gif`、`image/webp`、`image/bmp`，否则 **null**。
  逐分支：
  - JPEG：`FF D8 FF` 前缀，且第 4 字节 **≠ 0xF7**（`0xF7` 是 JPEG-LS，返回 null）；
  - PNG：8 字节签名 **且** `isPng`（长度 ≥ 16、偏移 8 的 chunk 长度 == 13、偏移 12 是 `IHDR`）
    **且非** `isAnimatedPng`；
  - GIF：`GIF87a` / `GIF89a` 前缀；
  - WebP：`RIFF` + 偏移 8 处 `WEBP`；
  - BMP：`BM` 前缀 + `isBmp` 结构校验（见下）。
- `isAnimatedPng(buffer)`：从偏移 8 逐个 chunk 走（`length = u32be`，类型在 `offset+4`），
  遇到 `acTL` → **true**，遇到 `IDAT` → **false**，越界/回绕 → false。
  所以**动画 PNG 会被 sniff 判为「不是图片」**（随后走文本分支，而不是报 "Animation" 错误）。
- `isBmp(buffer)`：长度 < 26 → false；`declaredFileSize = u32le(2)`、`pixelDataOffset = u32le(10)`、
  `dibHeaderSize = u32le(14)`；`declaredFileSize !== 0 && declaredFileSize < 26` → false；
  `pixelDataOffset < 14 + dibHeaderSize` → false；`declaredFileSize !== 0 && pixelDataOffset >= declaredFileSize` → false；
  `dibHeaderSize === 12` → `colorPlanes = u16le(22)`、`bitsPerPixel = u16le(24)`；
  `40 ≤ dibHeaderSize ≤ 124` → 需要长度 ≥ 30，`colorPlanes = u16le(26)`、`bitsPerPixel = u16le(28)`；
  其他 → false；最终要求 `colorPlanes === 1` 且 `bitsPerPixel ∈ {1,4,8,16,24,32}`（**这就是「BMP 校验严格」**）。
  验收：`tools.test.ts` 的 `should detect image MIME type from file magic (not extension)`、
  `should read BMP files from disk as PNG image attachments`、
  `should treat files with image extension but non-image content as text`。
- `processImage(bytes, mimeType, {autoResizeImages, resizeOptions})` →
  `{ok:true, data, mimeType, hints: string[]}` 或 `{ok:false, message}`。逐分支（**文本逐字**）：
  1. `autoResizeImages ?? true`；`normalizeImage`：`baseMimeType = mimeType.split(";")[0].trim().toLowerCase()`；
     `image/png|image/jpeg|image/jpg→image/jpeg|image/gif|image/webp` 视为已支持；其余尝试
     `convertImageBytesToPng`（BMP 在此转 PNG），失败 → `{ok:false, message:"[Image omitted: could not be converted to a supported inline image format.]"}`。
  2. `autoResizeImages` 为真：`resizeImage(...)` 返回 null（无法压到 `maxBytes` 以下）→
     `{ok:false, message:"[Image omitted: could not be resized below the inline image size limit.]"}`。
  3. 成功且缩放过：`hints` 先放转换提示（仅当 `convertedFrom` 存在且 ≠ 新 mimeType）：
     `` `[Image converted from ${from} to ${to}.]` ``；再放 `formatDimensionNote(resized)`（仅当 `wasResized`）：
     `` `[Image: original ${ow}x${oh}, displayed at ${w}x${h}. Multiply coordinates by ${(ow/w).toFixed(2)} to map to original image.]` ``
     ——**`toFixed(2)` 同样受 2.1.4 的中点规则约束**（如 `1.125 → "1.13"`，Rust `{:.2}` 会得 `"1.12"`）。
  4. `autoResizeImages` 为假：只做格式转换，`data = base64(normalized.bytes)`，`hints` 仅含转换提示（无尺寸提示）。
  5. 缩放默认档（`utils/image-resize-core.ts` 的 `DEFAULT_OPTIONS`）：`maxWidth: 2000`、`maxHeight: 2000`、
     `maxBytes: 4.5 * 1024 * 1024`（**base64 编码后的大小**，给 Anthropic 5MB 限制留余量）、`jpegQuality: 80`；
     策略为「先按 maxWidth/maxHeight 缩放 → PNG/JPEG 各试一次取更小 → JPEG 降质量 → 逐步降尺寸到 1x1」。
  BMP 会被转成 PNG（`processed.mimeType` 变化）；动画 PNG 在 **sniff 阶段**就被判为非图片（见上），
  因此 `processImage` 根本收不到它；BMP 校验严格。
- **read 工具不受 `images.blockImages` 影响**：`createReadTool` 永远返回 image 块。
  过滤发生在 `core/sdk.ts` 的 `convertToLlmWithBlockImages` 包装层（属 SDK 单元）：只在
  `user` / `toolResult` 消息里把 image 块换成 `{type:"text", text:"Image reading is disabled."}`，
  并**去重相邻**的同文本块；每次调用动态读 `settingsManager.getBlockImages()`（会话中改设置立即生效）。
  验收：`block-images.test.ts` 的 `should always read images (filtering happens at convertToLlm layer)`。
- `normalizeToolResultImages(content, {autoResizeImages, resizeOptions})`：用于工具结果里已存在的
  image 块（`tool-result-images.test.ts` 覆盖：无图原样返回、已在限制内原样、超大缩放并报告原始尺寸、
  关闭自动缩放仍转换不支持格式、无法解码的图保留、文本块顺序保持）。

#### 2.28.7 `src/utils/tools-manager.ts`（`ensureTool`）

- 常量：`NETWORK_TIMEOUT_MS = 10_000`、`DOWNLOAD_TIMEOUT_MS = 120_000`。
- `isOfflineModeEnabled()`：`KIANA_OFFLINE` 为 `"1"`、`"true"`、`"yes"`（大小写不敏感）。
- `getReleaseTarget(plat, arch)`：`arch` 只接受 `arm64`→`aarch64`、`x64`→`x86_64`；
  `darwin` → `${arch}-apple-darwin.tar.gz`；`linux` → `${arch}-unknown-linux-musl.tar.gz`；
  `win32` → `${arch}-pc-windows-msvc.zip`；其他 → undefined（**arm/ia32 不得回退到 x86-64**，见 #11209）。
- `TOOLS`：
  - `fd`：repo `sharkdp/fd`、binary `fd`、systemBinaryNames `["fd","fdfind"]`、tagPrefix `"v"`、
    asset `fd-v${version}-${target}`；
  - `rg`：repo `BurntSushi/ripgrep`、binary `rg`、tagPrefix `""`、asset `ripgrep-${version}-${target}`。
- `getToolPath(tool)`：先查 `getBinDir()/binaryName(.exe)`；再按 `systemBinaryNames` 用
  `spawnSync(cmd, ["--version"])` 判断存在（`result.error` 为 null/undefined 即存在），命中返回**命令名**
  （依赖 PATH）。
- `getLatestVersion(repo)`：`fetchWithRetry(https://github.com/${repo}/releases/latest,
  {headers: {"User-Agent": getKianaUserAgent(VERSION)}, redirect: "manual"}, {timeoutMs: 10000})`；
  丢弃 body（`response.body?.cancel()`）；`status` 在 `[300,400)` 时取 `location`；
  无 location → 抛 `Failed to resolve latest ${repo} release: HTTP ${status} without redirect`；
  从 location 解析 tag（去 `v` 前缀；相对重定向解析为绝对）。验收：`tools-manager.test.ts`
  的 6 个 `getLatestVersion` 用例。
- `ensureTool(tool, {onStatus}?)`：离线模式只查本地/PATH；否则必要时下载、解压（tar.gz / zip）、
  `chmod 0o755`、原子 rename；成功返回可执行路径；失败返回 null（bash/grep/find 据此报错文本）。
  错误链要保留 cause（测试 `surfaces the error cause chain when a download fails`）。
- Rust：`reqwest`（rustls）+ `flate2` + `tar` + `zip`；`which` crate 或自写 PATH 搜索；
  **不要**在下载目录内执行任何脚本（只解压二进制）。

#### 2.28.8 `src/config.ts`（本单元用到的部分）

- `APP_NAME = pkg.kianaConfig?.name || "kiana"`；`APP_TITLE = kianaConfig?.name ? APP_NAME : "π"`
  （**只有覆盖时才等于 APP_NAME，否则是希腊字母 π**）；`PACKAGE_NAME = pkg.name || "@earendil-works/kiana-coding-agent"`；
  `CONFIG_DIR_NAME = pkg.kianaConfig?.configDir || ".kiana"`。
- `VERSION`（package.json version，缺失时 `"0.0.0"`）。
- `getPackageDir()`：**优先 `process.env.KIANA_PACKAGE_DIR`（经 `normalizePath`）**；Bun 单文件二进制 →
  `dirname(process.execPath)`；否则从 `__dirname` 向上找 node package 目录（`findNodePackageDir`）。
- `getThemesDir()` = Bun 二进制 → `${packageDir}/theme`；否则 `${packageDir}/{src|dist}/modes/interactive/theme`
  （`src` 存在则 `src`）。
- `getBinDir()` = `${getAgentDir()}/bin`。
- `getReadmePath()` = `resolve(${getPackageDir()}/README.md)`；另有 `getDocsDir()`、`getExamplesDir()`、
  `getChangelogPath()`、`getAssetsDir()`、`getPackageJsonPath()`。
- `getExportTemplateDir()`：Bun 单文件二进制 → `${getPackageDir()}/export-html`；
  否则 `${getPackageDir()}/{src|dist}/core/export-html`（`src` 存在则 `src`）。
- `getShareViewerUrl(gistId)` = `${process.env.KIANA_SHARE_VIEWER_URL || "https://kiana.dev/session/"}#${gistId}`
  （Rust：未设置 `KIANA_SHARE_VIEWER_URL` 时回退 `KIANA_API_BASE_URL/session/`；两者都没有时返回不可用并提示。）
  （**注意是 `#` 锚点拼接**，不是路径拼接）。
- `expandTildePath(p)` = `normalizePath(p)`（默认 `expandTilde: true`）。
- `ENV_AGENT_DIR = \`${APP_NAME.toUpperCase()}_CODING_AGENT_DIR\``；
  `ENV_SESSION_DIR = \`${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR\``。
- Rust：用 `include_str!`/`include_bytes!` 内嵌模板与 vendor JS（编译期固定），
  `getExportTemplateDir` 的运行时路径查找在 Rust 里退化为编译期资产（但**导出内容必须一致**）。
  若需支持外部覆盖（如发行版打包），保留「优先读磁盘目录、否则用内嵌」的双路径。

#### 2.28.9 `ensureTool` 与下载细节（补充）

`ensureTool(tool: "fd"|"rg", onStatus?: (status: {type:"info"|"warning"; message:string}) => void): Promise<string|undefined>`
依次：

1. `getToolPath(tool)` 命中 → 返回（本地 bin 目录优先，其次 PATH 上的系统命令名）。
2. `TOOLS[tool]` 不存在 → `undefined`。
3. `KIANA_OFFLINE` 生效 → `onStatus({type:"warning", message: `${name} not found. Offline mode enabled, skipping download.`})`；返回 undefined。
4. `platform() === "android"`（Termux）→ `onStatus({type:"warning", message: `${name} not found. Install with: pkg install ${pkgName}`})`
   （`TERMUX_PACKAGES = {fd:"fd", rg:"ripgrep"}`）；返回 undefined。
5. `!getReleaseTarget(platform(), arch())` → warning
   `` `${name} not found. Automatic download is not supported on ${plat}/${arch}. Install ${name} with your system package manager.` ``；返回 undefined。
6. 否则 `onStatus({type:"info", message: `${name} not found. Downloading...`})`；
   `downloadTool(tool)` 成功 → `onStatus({type:"info", message: `${name} installed to ${path}`})`；返回 path。
   失败 → 收集错误 cause 链（最多 5 层、去重 message），warning
   `` `Failed to download ${name}: ${messages.join(": ") || String(e)}` ``；返回 undefined。

`downloadTool(tool)`：
1. `target = getReleaseTarget(platform(), arch())`；无 → 抛 `` `Unsupported platform: ${plat}/${arch}` ``。
2. 版本：`fd` + darwin + x64 **固定 `"10.3.0"`**（跳过版本查询）；否则 `getLatestVersion(repo)`。
3. `assetName = config.getAssetName(version, target)`；`mkdirSync(TOOLS_DIR, {recursive:true})`。
4. `downloadUrl = https://github.com/${repo}/releases/download/${tagPrefix}${version}/${assetName}`；
   `archivePath = ${TOOLS_DIR}/${assetName}`；`binaryExt = win32 ? ".exe" : ""`；`binaryPath = ${TOOLS_DIR}/${binaryName}${binaryExt}`。
5. `downloadFile(url, archivePath)`：`fetchWithRetry(url, undefined, {timeoutMs: 120000})`；
   `!response.ok` → 抛 `` `Download failed with HTTP ${status}: ${url}` ``；无 body → `"No response body"`；
   `pipeline(Readable.fromWeb(body), createWriteStream(dest))`。
6. 解压目录：`${TOOLS_DIR}/extract_tmp_${binaryName}_${pid}_${Date.now()}_${Math.random().toString(36).slice(2,10)}`
   （**唯一目录，避免 fd 与 rg 并发下载竞态**）。
7. 解压：`.tar.gz` → `tar xzf archive -C dir`；`.zip` → win32 优先 `%SystemRoot%\System32\tar.exe xf ...`，
   失败回退 `powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command <script> archive dir`；
   非 win32 先 `unzip -q archive -d dir`，失败回退 `tar xf archive -C dir`；都不行 → 抛
   `` `Failed to extract ${assetName}: ${failures.join("; ")}` ``；未知格式 → `` `Unsupported archive format: ${assetName}` ``。
8. 找二进制：先 `${extractDir}/${assetName去掉扩展名}/${binaryFileName}`、再 `${extractDir}/${binaryFileName}`，
   否则迭代式（显式栈）递归搜索；找不到 → 抛
   `` `Binary not found in archive: expected ${binaryFileName} under ${extractDir}` ``。
9. `renameSync(extractedBinary, binaryPath)`；非 win32 `chmodSync(binaryPath, 0o755)`。
10. finally：删除 archive 与解压目录（`force:true`）。

`getLatestVersion` 的错误（逐字）：
- 无 redirect：`` `Failed to resolve latest ${repo} release: HTTP ${status} without redirect` ``；
- redirect 不含 `/releases/tag/` 或 tag 为空：
  `` `Failed to resolve latest ${repo} release: unexpected redirect to ${location}` ``；
- tag 处理：`new URL(location, "https://github.com").pathname.split("/").pop()`，`decodeURIComponent`，去前导 `v`。
  验收用例：`resolves the version from the release page redirect`（`.../tag/v1.2.3` → `1.2.3`）、
  `keeps tags without a v prefix intact`（`.../tag/0.9` → `0.9`）、`resolves relative redirect targets`、
  `discards the redirect response body`、上面两条错误。
- 测试还会替换 `os.platform/arch` 为 `arm`/`ia32`，断言不下载 x86-64 二进制（`does not download x86-64 binaries on %s`）。

`formatSpawnFailure(result)`：`result.error?.message` → 否则 `stderr.trim()` → 否则 `stdout.trim()` →
`` `exit status ${status ?? "unknown"}` ``；`runExtractionCommand` 返回 `` `${command}: ${failure}` `` 或 null。

`ToolStatus` 类型：`{type: "info" | "warning"; message: string}`（导出）。

### 2.29 `src/core/export-html/vendor/*`（逐字节资产）

| 文件 | 字节数 | SHA-256 |
|---|---|---|
| `vendor/marked.min.js` | 44640（约 43.6K） | `438eedfcf932a414d0d0bfeea32dc365c063563b8ca713b4687fc8f8b501e5e4` |
| `vendor/marked-cjk-friendly.min.js` | 4754（约 4.6K） | `93d2c938754cb24eef5b9ffecc268a98ef6beeb75dad614b0e874176aeeba565` |
| `vendor/highlight.min.js` | 121727（约 118.9K） | `837a6fa5b0c736b52bbde2b2b6190f305da3fc9ed41681db5321507057b5c846` |

文件头版本（用于识别误换包）：**marked v18.0.11**、**highlight.js v11.9.0**、
**marked-cjk-friendly v0.1.2**。三者都带版权注释，SHA-256 已覆盖，故只需断言哈希即可。

三项均按字节内嵌；`marked-cjk-friendly` 用于 CJK 强调（`**粗体**` 紧随中文标点），
`highlight.js` 提供 `hljs.highlight` / `highlightAuto`。

补充事实（实测，用于校验内嵌是否完整）：

- 目录里还有 **`vendor/highlight.min.js` 之外的第三类资源**吗？没有——只有上述 3 个 `.js`；
  但 `template.html` 引用了 `<script>{{MARKED_JS}}</script>`、`<script>{{MARKED_CJK_FRIENDLY_JS}}</script>`、
  `<script>{{HIGHLIGHT_JS}}</script>`，顺序固定为 marked → marked-cjk-friendly → highlight。
- 三个 vendor 文件里 **`{{` 出现次数均为 0**，因此插入它们不会与任何占位符冲突。
- marked 的版本行为由 `template.js` 的 `marked.use` 覆写（见 2.27.7）：**不得换成别的 marked 版本**
  （例如新版 marked 移除 `tokenizer.emStrong` 或改变 `del` token 形状会导致渲染差异）。

### 2.30 `docs/compaction.md` 与 `docs/shell-aliases.md`（文档约束的逐条校验）

两份文档是**用户可见行为的权威描述**。Rust 侧实现完成后应逐条对照；下表把每条断言映射到规格内的
实现位置（文档中出现但实现位置在别单元的，标注归属）。

**`docs/compaction.md`**

| 文档断言（逐字要点） | 本规格位置 / 归属 |
|---|---|
| 触发条件 `contextTokens > contextWindow - reserveTokens`；默认 `reserveTokens = 16384` | 2.20.7 / 2.20.1 |
| `keepRecentTokens` 默认 `20000` | 2.20.1 |
| 摘要请求使用模型输出上限（`reserveTokens` 不参与摘要请求）；**该上限在需要时会下调**，以保证输入与输出都能装进上下文窗口 | 2.20.16（`_reserveTokens` 未使用；"reduced when needed so input and output fit the context window" 的降 limit 逻辑位于 kiana-ai 的 provider 请求层，不在本单元；Rust 侧调用 `completeSummarization` 时不得自行传 `maxTokens`，否则会改变行为） |
| 阈值检查发生在「工具结束、结果追加之后、下一个 assistant 之前」；`prepareNextTurn` 压缩后仍做 catch-up steering 轮询；若完成的工具批次终止了运行且没有排队消息则跳过 | **agent-session 单元**（ca-session），本单元只提供 `shouldCompact`/`prepareCompaction` |
| provider 上下文溢出或早期 `stopReason:"length"` 可选一次「压缩并重试」；带 tool call 的 length 响应保留合成失败结果并走常规工具/队列调度 | 同上（本单元提供 `getSummarizationFailure` 与 `compact`） |
| 手动 `/compact [instructions]`，指令聚焦摘要 | 3.x / `compact(customInstructions)`，见 2.20.21 |
| 压缩五步：找切点 → 抽消息 → 生成结构化摘要（有前次摘要则迭代）→ 追加 `CompactionEntry` → 重建上下文 | 2.20.20 / 2.20.21 / **ca-session** |
| 重复压缩的跨度从上一次压缩的 `firstKeptEntryId` 开始；找不到该条目则回退到压缩条目之后；retain-none 以自身 id 为界；`tokensBefore` 用重建后的投影重算；被 omit 的条目仍保存但不影响切点/摘要/检查点/估算 | 2.20.20（含紧随其后的「retain-none 与回退」段）+ **ca-session** 的 `buildSessionProjection` |
| 溢出/长度恢复顺序：持久化最终响应 → `turn_end` → `agent_end` → 追加 context_edit omissions → （溢出/长度时）`session_before_compact` 并追加压缩 → 以新 run 重试；失败则保留 omission、不追加压缩、不内部重试；`agent_before_settle` 看到修复后的投影 | **ca-session / agent-session**；本单元只被调用 |
| split 用户消息跨度：切点落在该跨度内、`isSplitTurn = true`、`turnPrefixMessages` 为跨度前缀、生成两份摘要后合并 | 2.20.11 / 2.20.21（`{history}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}`） |
| 合法切点：user / assistant / bashExecution / custom_message / branch_summary；**绝不切在 tool result** | 2.20.9（`isCutPointMessage`） |
| 准备阶段仅在「后缀只含被 omit 的 assistant 尝试、且没有未 omit 的生产上下文的条目」时才推进 kept 边界；纯元数据与新增 custom 消息不推进；影响候选输入或被摘要前缀的 replacement edit 会阻止推进；对最终被 omit 的后缀条目的 replacement 是安全的 | 2.20.19（`findProjectedCutPoint` 的 recovery-omission-suffix 规则） |
| `CompactionEntry` / `BranchSummaryEntry` 字段与默认 `details`（`{readFiles, modifiedFiles}`）；扩展可存任意 JSON；`usage` 计入会话总计；`fromHook` 为 legacy 字段名 | 4.2 / 2.19.1 |
| 分支摘要：找共同祖先 → 从旧叶回走收集 → 按 token 预算（最新优先）→ 生成 → 追加 `BranchSummaryEntry` | 2.21.2 / 2.21.4 / 2.21.6 |
| 累积文件跟踪：默认压缩从上一个 kiana 生成的压缩继承；分支摘要从被摘要条目里的 kiana 生成分支摘要继承；`fromHook === true` 的不继承 | 2.20.20（`extractFileOperations`）/ 2.21.4 |
| 摘要格式：Goal / Constraints & Preferences / Progress（Done/In Progress/Blocked）/ Key Decisions / Next Steps / Critical Context；分支摘要到 Next Steps 为止；文件列表以 `<read-files>` / `<modified-files>` 追加 | 2.20.12 / 2.19.3 / 2.21.5；追加逻辑见 2.19.1（`formatFileOperations`） |
| `serializeConversation` 行前缀 `[User]:` / `[Assistant]:` / `[Assistant tool calls]: read(path="foo.ts"); edit(path="bar.ts", ...)` / `[Tool result]:`；thinking 被省略；tool result 截断到 2000 字符并带截断标记 | 2.19.2（含 `[... N more characters truncated]`） |
| 扩展钩子 `session_before_compact`（`cancel:true` 或提供 `{summary, firstKeptEntryId, tokensBefore, usage?, details?}`）、`session_compact_failed`（`reason`/`errorMessage`/`aborted`/`willRetry`/`fromExtension`）、`session_before_tree`（`cancel` / `summary` 仅在 `userWantsSummary` 时使用） | 5.3（事件类型）；挂载点在 ca-session / interactive |
| 设置：`compaction.enabled` / `reserveTokens` / `keepRecentTokens`；`compaction.modelOverrides` 以精确 `provider/modelId`（含斜杠、区分大小写）为键，字段独立回退（override → 普通设置 → 内置默认），值必须是非负安全整数，匹配到的 override 中非法值读取即报错，普通设置非法值即使当前模型有合法 override 也报错，`enabled` 始终全局，模型切换不影响普通设置与已进行中的压缩，分支摘要设置不受影响 | **settings-manager 单元**（`getCompactionSettings(model?)`，见 5.10）；本单元只消费解析后的 `{enabled, reserveTokens, keepRecentTokens}` |
| 文档明确把 `prepareCompaction()` / `compact()` 与**编程式摘要** `generateSummary()`（返回 summary 文本）/ `generateSummaryWithUsage()`（返回 `{ text, usage }`）列为公开入口 | 2.20.16 / 2.20.18 / 2.20.21：Rust 侧这三个函数都必须 `pub` 且签名可编程调用，不能只保留内部 `_with_usage` 变体 |
| 设置文件位置：全局 `~/.kiana/agent/settings.json` 与项目 `<project-dir>/.kiana/settings.json` | 本单元不读设置文件；路径由 config/settings-manager 单元定义（`ENV_AGENT_DIR` 可覆盖，见 4.3） |
| **项目与全局设置递归合并后再查找 modelOverrides**：全局的某模型条目优先于项目的通用回退；项目必须显式覆盖该模型条目才能改变 | 7.2 `settings-manager-compaction.test.ts`（"项目级逐字段合并"）；本单元不实现，但 Rust 侧集成测试应按此断言 |

**`docs/shell-aliases.md`**

| 文档断言 | 本规格位置 |
|---|---|
| 每次 Bash 命令启动独立的非交互 shell；非交互 Bash 默认不展开 alias、通常不加载交互启动文件 | 2.28.1（无登录 shell） |
| `shellPath` 选择 Bash 可执行文件；`shellCommandPrefix` 在每条命令前运行 setup | 2.9.1（`commandPrefix`）/ 2.28.1 |
| `!command` 与 `!!command` 使用同一个已解析的 Bash；`powershell` 工具用 `pwsh.exe` 或 Windows PowerShell；扩展可替换 shell 工具 | **interactive / agent-session 单元**；本单元只保证 `createLocalBashOperations` 与 `createLocalPowerShellOperations` 的可替换点 |
| Kiana 用 `bash -c`；Unix 依次 `/bin/bash` → PATH 上的 `bash` → `sh`；原生 Windows 先用配置路径，再 Git Bash，再 PATH 上的 `bash.exe` | 2.28.1（`getShellConfig`） |
| 设置变更后需 `/reload` | **cli / interactive 单元** |
| 「前缀与命令用换行连接」（`Kiana joins the prefix and requested command with a newline`） | 2.9.7：`resolvedCommand` 在前缀非空时为「前缀 + 单个 U+000A + 命令」，否则为原命令 |
| 前缀每条命令都重新运行、应快且无交互提示 | 同上（无缓存、无去重） |
| 示例 `shellCommandPrefix: "shopt -s expand_aliases\nsource ~/.bash_aliases"` 必须能工作（即前缀里的换行原样进入子进程 argv） | 2.9.7 / 4.4（argv `[shell, "-c", command]`） |
| 故障排查：前缀只对 `!` 生效、扩展工具不受影响；`shopt` 未找到说明回退到了 `sh` 或 `shellPath` 指向非 Bash；含交互命令的前缀会挂住 | 2.28.1 的探测顺序即决定了这些行为，无额外代码 |

---

## 3. 用户可见表面（逐字文本）

本节汇总**所有**进入用户视野（TUI、会话文件、模型上下文、HTML）的固定文本。Rust 侧应把它们集中为
常量模块（`strings.rs`），并由测试逐条断言。

### 3.1 工具描述（`description`，进入 provider tools JSON）

```
read:        Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.
bash:        Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.
powershell:  Execute a PowerShell command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.
edit:        Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.
write:       Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.
grep:        Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.
find:        Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).
ls:          List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).
```

### 3.2 系统提示贡献（snippet / guidelines）

| 工具 | snippet | guidelines |
|---|---|---|
| read | `Read file contents` | `Use read to examine files instead of cat or sed.` |
| bash | `Execute bash commands (ls, grep, find, etc.)` | `You can inspect KIANA_* environment variables for current model and session details.` |
| powershell | `Execute PowerShell commands` | 同上 |
| edit | `Make precise file edits with exact text replacement, including multiple disjoint edits in one call` | 见 2.7.2（4 条） |
| write | `Create or overwrite files` | `Use write only for new files or complete rewrites.` |
| grep | `Search file contents for patterns (respects .gitignore)` | （空数组） |
| find | `Find files by glob pattern (respects .gitignore)` | （空数组） |
| ls | `List directory contents` | （空数组） |

`bash`/`powershell` 在 `exposeSessionEnvironment: false` 时 `promptGuidelines` 为 `undefined`。

**导出常量名对照**（每个工具模块都导出一个 `{snippet, guidelines} as const` 常量，
`createXToolDefinition` 把它的字段填进定义；Rust 侧建议同样集中为常量并在定义里引用）：

| 工具 | 导出常量 |
|---|---|
| read | `readToolSystemPromptContribution` |
| bash | `bashToolSystemPromptContribution` |
| powershell | `powershellToolSystemPromptContribution` |
| edit | `editToolSystemPromptContribution` |
| write | `writeToolSystemPromptContribution` |
| grep | `grepToolSystemPromptContribution` |
| find | `findToolSystemPromptContribution` |
| ls | `lsToolSystemPromptContribution` |

edit 的 4 条 guidelines **逐字**：
```
Use edit for precise changes (edits[].oldText must match exactly)
When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls
Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.
Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.
```

### 3.3 工具结果文本（进入模型上下文与会话文件）

全部逐字（`{}` 为插值）：

- read：`Offset {offset} is beyond end of file ({n} lines total)`
- read：`[Line {start} is {size}, exceeds 50.0KB limit. Use bash: sed -n '{start}p' {path} | head -c 51200]`
- read：`\n\n[Showing lines {a}-{b} of {total}. Use offset={next} to continue.]`
- read：`\n\n[Showing lines {a}-{b} of {total} (50.0KB limit). Use offset={next} to continue.]`
- read：`\n\n[{remaining} more lines in file. Use offset={next} to continue.]`
- read：`Read image file [{mimeType}]`、`Read image file [{mimeType}]\n{message}`、`\n{hints}`、
  `\n[Current model does not support images. The image will be omitted from this request.]`
- write：`Successfully wrote to {path}`
- edit：`Successfully replaced {n} block(s) in {path}.`
- bash/powershell：`(no output)`（空输出占位）、`\n\n[Showing lines {a}-{b} of {total}. Full output: {path}]`、
  `\n\n[Showing lines {a}-{b} of {total} (50.0KB limit). Full output: {path}]`、
  `\n\n[Showing last {size} of line {n} (line is {lastLineSize}). Full output: {path}]`、
  `Command aborted`、`Command timed out after {secs} seconds`、
  `Command terminated without an exit code`、`Command exited with code {code}`
- bash 临时文件：`{tmpdir}/kiana-bash-{16hex}.log`；powershell：`{tmpdir}/kiana-powershell-{16hex}.log`
- grep：`No matches found`、`\n\n[{notices}]`（notices 见 2.11.2）、
  `{path}:{line}: {text}`、`{path}-{line}- {text}`、`{path}:{line}: (unable to read file)`
- find：`No files found matching pattern`、`\n\n[{notices}]`、
  `{limit} results limit reached. Use limit={2*limit} for more, or refine pattern`（fd）、
  `{limit} results limit reached`（自定义 glob）
- ls：`(empty directory)`、`\n\n[{notices}]`、`{limit} entries limit reached. Use limit={2*limit} for more`

### 3.4 工具错误（`Error.message`，逐字）

```
Unknown tool name: {toolName}
Invalid timeout: must be a finite number of seconds
Invalid timeout: maximum is 2147483.647 seconds
Working directory does not exist: {cwd}\nCannot execute {shellName} commands.
aborted                                  （BashOperations 内部信号，非用户可见）
timeout:{timeout}                        （同上，被 bash.ts 转换成用户文本）
Operation aborted                        （read/write/edit/grep/find/ls 的中止）
Path not found: {searchPath}
Not a directory: {dirPath}
Cannot read directory: {message}
fd is not available and could not be downloaded
ripgrep (rg) is not available and could not be downloaded
fd exited with code {code}
ripgrep exited with code {code}
Failed to run fd: {message}
Failed to run ripgrep: {message}
Could not edit file: {path}. {Error code: X | String(error)}.
Edit tool input is invalid. edits must contain at least one replacement.
Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines.
Could not find edits[{i}] in {path}. The oldText must match exactly including all whitespace and newlines.
Found {n} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique.
Found {n} occurrences of edits[{i}] in {path}. Each oldText must be unique. Please provide more context to make it unique.
oldText must not be empty in {path}.
edits[{i}].oldText must not be empty in {path}.
No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.
No changes made to {path}. The replacements produced identical content.
edits[{a}] and edits[{b}] overlap in {path}. Merge them into one edit or target disjoint regions.
Replacement range is outside the base content.
Cannot preserve unchanged lines because the base content has a different line count.
Cannot append to a finished output accumulator
Custom shell path not found: {path}
No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{indented paths}
The powershell tool is only available on Windows.
No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.
```

压缩相关（逐字）：

```
First kept entry has no UUID - session may need migration
Summarization failed: {errorMessage || Unknown error}
Summarization failed: generation hit the token cap and the summary is incomplete
Summarization attempted to call a tool
Turn prefix summarization failed: ...
Turn prefix summarization failed: generation hit the token cap and the summary is incomplete
Turn prefix summarization attempted to call a tool
Branch summarization failed: ...
Branch summarization failed: generation hit the token cap and the summary is incomplete
Branch summarization attempted to call a tool
```

导出相关（逐字）：

```
Cannot export in-memory session to HTML
Nothing to export yet - start a conversation first
File not found: {resolvedInputPath}
```

### 3.5 TUI 渲染文本（颜色 token 见括号）

- bash call：`$ {command}`（`toolTitle` + bold）或 `PS> {command}`；` (timeout {n}s)`（`muted`）；
  空 command → `...`（`toolOutput`）；类型错误 → `[invalid arg]`（`error`）。
- bash 折叠预览提示：`... ({hidden} earlier lines,`（`muted`）+ 空格 + `{key} to expand`（`dim` 键 / `muted` 描述）+ `)`（`muted`）。
- bash 输出警告（`warning`）：`[Full output: {path}. Truncated: showing {a} of {b} lines]` /
  `[Full output: {path}. Truncated: {n} lines shown (50.0KB limit)]`（逗号分隔的多条用 `. ` 连接）。
- bash timer（`muted`）：`Elapsed {duration}` / `Took {duration}`。
- read call：`read {path}`（`toolTitle` bold + `accent` 路径 + `warning` 范围）；
  紧凑模式：`[skill] {name} ({key} to expand)`（`customMessageLabel` + 字面 `\x1b[1m...\x1b[22m`）、
  `read docs {label}` / `read resource {label}`（`toolTitle` bold + `accent`）。
- read 结果折叠时**不渲染**；展开或出错时输出全文（最多 10 行折叠）；
  提示 `... ({remaining} more lines,` + `{key} to expand)`；
  警告：`[First line exceeds 50.0KB limit]`、`[Truncated: showing {a} of {b} lines (2000 line limit)]`、
  `[Truncated: {n} lines shown (50.0KB limit)]`。
- write call：`write {path}`；内容预览 10 行；`... ({remaining} more lines, {total} total,` + `{key} to expand)`；
  非字符串 content → `[invalid content arg - expected string]`（`error`）；错误结果前缀换行 + `error` 色。
- edit call：`edit {path}`；预览 diff（Box 背景 `toolPendingBg`/`toolSuccessBg`/`toolErrorBg`）。
- grep call：`grep /{pattern}/ in {path}` + ` ({glob})` + ` limit {n}`；结果 15 行；
  警告 `[Truncated: {n} matches limit, {size} limit, some lines truncated]`。
- find call：`find {pattern} in {path}` + ` (limit {n})`；结果 20 行；
  警告 `[Truncated: {n} results limit, {size} limit]`。
- ls call：`ls {path}`（缺省 `.`）+ ` (limit {n})`；结果 20 行；警告 `[Truncated: {n} entries limit, {size} limit]`。
- 通用 fallback 头部：`{title} {key}={json} {key}={json} ...`（超过 100 字符截为 97 + `...`）；
  展开时每个 `  {key}: {value}`。
- 时长：`4.2s` / `1m 30s` / `2h 3m 4s`。
- 无效参数：`[invalid arg]`（`error`）。

### 3.6 HTML 导出中的固定文本

见 2.25/2.27.5：`Session Export`（title）、`Search...`（placeholder）、
`Default` / `No-tools` / `User` / `Labeled` / `All` / `✕`（过滤按钮）、
`Open sidebar` / `Close` / `Resize session tree sidebar`（title/aria）、
`Session: {id}`、`T toggle thinking · O toggle tools · H toggle hidden messages`、
`Toggle thinking` / `Toggle tools` / `Show hidden messages` / `Hide hidden messages`、
`↓ JSONL`、`Download session as JSONL`、`Date:` / `Models:` / `Messages:` / `Tool Calls:` / `Tokens:` / `Cost:`、
`unknown`、`$0.000`（3 位小数）、`↑{n} ↓{n} R{n} W{n}`、`{n} user, {n} assistant, ...`、
`System Prompt`、`Available Tools`、`{n} / {n} entries`、`✓` / `✗` / `…`、`... ({n} more lines)`、
`Copy link to this message`、`(none)` / `unknown` 等。

补充（本次审计新增，全部逐字）：

- 系统提示展开提示：`... ({remaining} more lines, click to expand)`（`renderSystemPromptBlock`，
  预览 10 行）。
- 后续 system 条目：标题 `` `System Prompt Update${changes ? ': ' + changes : ''}` ``、
  `Tools Added`、`Tools Removed`。
- 压缩条目：`[compaction]`、`Compacted from {tokensBefore.toLocaleString()} tokens`（出现两次：折叠行与内容首行）。
- 分支摘要：`Branch Summary`；model 变更：`Switched to model: {provider}/{modelId}`。
- `custom_message`：`[{customType}]`，隐藏时 ` · Hidden in terminal`；`hook-message-hidden` class。
- thinking：`Thinking ...` 与 `Aborted`、`Error: {errorMessage || 'Unknown error'}`。
- bashExecution：`(cancelled)`、`(exit {code})`。
- skill 块：`[skill] {name}`、`{name} (click to expand)`。
- 树节点（`getTreeNodeDisplayHtml`，逐字；注意树里**不显示 provider**）：`user:` / `assistant:` / `(aborted)` / `(no text)` /
  `[bash]:` / `[system prompt]` / `[system update{changes}]`（changes 为空时无冒号后缀）/
  `[compaction: {Math.round(tokensBefore/1000)}k tokens]` / `[branch summary]:` / `[{customType}]:` /
  `[model: {modelId}]` / `[thinking: {thinkingLevel}]` / `[context omit|replace: {targetId}]` /
  `[{entry.type}]`（default，如 `[label]`、`[session_info]`）；skill 节点为
  `skill: {name}`，有用户文本时追加 ` · ` 与 `user: {text}`；label 前缀为 `[{label}] `（**带尾随空格**）。
  所有文本先 `normalize`（`[
	]` → 空格、trim），再 `truncate(…, 100)`。
- 嵌套调用：`Nested calls: {n}`、` (incomplete record)`、`[arguments omitted, {n} bytes]`、`{n}ms`。
- 工具参数无效：`[invalid arg]`、`[invalid content arg - expected string]`。
- 工具栏（`renderToolsList`）：`Available Tools` / `Tools Added` / `Tools Removed`；参数展开提示由
  **CSS 伪元素**生成 `[click to show parameters]` / `[hide parameters]`（`template.css` 的
  `.tool-params-hint::after` 与 `.tool-item.params-expanded .tool-params-hint::after`），
  每个参数行含 `required` / `optional` 徽标。
- 空行占位（ANSI 行）：`&nbsp;`。
- 工具结果折叠提示（HTML）：`... ({remaining} more lines)`（**无 `, click to expand`**，与系统提示不同）。

### 3.7 ANSI/布局细节

- `truncateLine` 追加 `... [truncated]`。
- `truncateMiddle` 标记：`…{n} chars truncated…`（U+2026）。
- bash 折叠预览保留**最后 5 个视觉行**（`VisualLinePreview` 按终端宽度折行后取尾部，见 2.16 与
  `components/visual-truncate.ts`：`truncateToVisualLines(text, maxVisualLines, width, paddingX=0, keep)`，
  用临时 `Text(text, paddingX, 0)` 渲染后切片）。
- 编辑 diff 渲染（`components/diff.ts`）：`+{行号} 内容` / `-{行号} 内容` / ` {行号} 内容`，`+`/`-` 行用
  `toolDiffAdded`/`toolDiffRemoved` 前景 + `theme.inverse` 做行内词级高亮（`diffWords`），上下文行 `toolDiffContext`。

---

## 4. 外部格式（磁盘 / 环境变量 / 子进程 / 协议）

### 4.1 磁盘文件

| 文件 | 何时写 | 权限 | 命名 |
|---|---|---|---|
| `{os.tmpdir()}/{prefix}-{16 hex}.log` | bash/powershell 输出超过行/字节限制时（`OutputAccumulator.ensureTempFile`，`prefix` 为 `kiana-output` / `kiana-bash` / `kiana-powershell`） | `0600` | `createOutputFileStream`，`flags:"wx"`（已存在则失败） |
| 用户在工具调用里指定的任意文件 | `write` / `edit` | 默认（`fs.writeFile` 不带 mode → `0666 & ~umask`） | 原样路径（经 `resolveToCwd`） |
| `{path}.html`（默认 `{APP_NAME}-session-{basename}.html`） | `exportSessionToHtml` / `exportFromFile` | 默认（`writeFileSync`，`"utf8"`） | 相对当前工作目录，不 resolve |
| `{agentDir}/bin/{fd|rg}` | `ensureTool` 下载 | `0755`（非 win32） | 见 2.28.9 |

`OutputAccumulator` 临时文件内容 = **全部原始字节**（streaming 写入，含不完整 UTF-8 尾），
因此 `readFullOutput` 可以在任意字节偏移切分。

### 4.2 会话条目中属于本单元的结构

`message` 条目里的工具结果由 `agent-core` 记录；本单元贡献的是 `details` 与 `structuredContent`
的**形状**（键序固定）：

```jsonc
// read
{ "truncation": { /* TruncationResult，见 2.1.2 */ } }
// write
// （无 details，序列化为 undefined → 字段省略）
// edit
{ "diff": "...", "patch": "...", "firstChangedLine": 12 }   // firstChangedLine 为 undefined 时省略
// bash / powershell
{ "truncation": {...}, "fullOutputPath": "/tmp/kiana-bash-…log" }
// grep
{ "truncation": {...}, "matchLimitReached": 100, "linesTruncated": true }
// find
{ "truncation": {...}, "resultLimitReached": 1000 }
// ls
{ "truncation": {...}, "entryLimitReached": 500 }
```

`CompactionEntry.details`（默认压缩写入）：

```jsonc
{ "readFiles": ["a.ts"], "modifiedFiles": ["b.ts"] }
```

`BranchSummaryEntry.details`：同形状。

`CompactionEntry` 本体（由 `SessionManager` 写盘，字段顺序见 ca-session 规格）：
`type, id, parentId, timestamp, summary, firstKeptEntryId, tokensBefore, usage?, fromHook?, details?`。
`BranchSummaryEntry`：`type, id, parentId, timestamp, summary, fromId, usage?, fromHook?, details?`。

本单元会**读到**、但不产生的条目类型（完整 `SessionEntry` 联合，来自 `core/session-manager.ts`）：
`message`、`thinking_level_change`、`model_change`、**`usage`**（`UsageEntry`，`type:"usage"`，
计费/统计记录，`appendUsage()` 写入）、`compaction`、`branch_summary`、`custom`、`custom_message`、
**`context_edit`**（`type:"context_edit"`，`targetId` + `replacement: {content} | null`）、`label`
（`targetId` + `label`）、`session_info`。
其中 `usage` 与 `context_edit` 在 `getMessageFromEntry` 里都落到 default → 不贡献摘要上下文；
但 `context_edit` 会影响**投影**（ca-session 的 `buildSessionProjection`），进而影响本单元的
`tokensBefore`/切点（见 2.20.19）。`label` 条目的 `targetId` 是树/HTML 侧 `labelMap` 的来源。

### 4.3 环境变量

| 变量 | 方向 | 语义 |
|---|---|---|
| `KIANA_SESSION_ID` | 写入子进程 | 当前会话 id（`exposeSessionEnvironment` 且 ctx 存在时） |
| `KIANA_SESSION_FILE` | 写入子进程 | 会话文件绝对路径（仅当非空） |
| `KIANA_PROVIDER` / `KIANA_MODEL` | 写入子进程 | 当前模型 provider/id |
| `KIANA_REASONING_LEVEL` | 写入子进程 | thinking level（真值时） |
| `KIANA_OFFLINE` | 读取 | `"1"` / `"true"` / `"yes"`（不区分大小写）→ 禁止下载外部工具 |
| `SystemRoot` / `ProgramFiles` / `ProgramFiles(x86)` | 读取 | Windows shell/PowerShell 探测 |
| `RIPGREP_CONFIG_PATH` | **刻意忽略** | 通过 `--no-config` 保证工具契约稳定 |
| `KIANA_PACKAGE_DIR` | 读取 | `getPackageDir()` 的最高优先级覆盖（`config.ts`），决定 `export-html` 模板、`theme/`、`package.json`、`README.md`、`docs/`、`CHANGELOG.md`、`assets/` 的根目录；本单元的 `getExportTemplateDir()` 因此可被重定向 |
| `KIANA_SHARE_VIEWER_URL` | 读取 | `getShareViewerUrl()`：覆盖 `DEFAULT_SHARE_VIEWER_URL`（HTML 分享链接的 viewer 基址，导出产物本身不内嵌该值） |
| `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`（默认 `KIANA_CODING_AGENT_DIR`） | 读取 | `ENV_AGENT_DIR`：agent 目录覆盖；`getBinDir() = ${agentDir}/bin` 即 `ensureTool` 的安装目标与 PATH 注入源 |
| `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`（默认 `KIANA_CODING_AGENT_SESSION_DIR`） | 读取 | `ENV_SESSION_DIR`：会话目录覆盖（影响 `exportSessionToHtml` 的会话文件位置，归 ca-session） |
| `KIANA_EXPERIMENTAL` | 读取 | **严格等于 `"1"`** 才开启实验特性（`core/experimental.ts` 的 `process.env.KIANA_EXPERIMENTAL === "1"`；`"true"`/`"yes"` **不**生效，与 `KIANA_OFFLINE` 不同）。注意：`builtin-tool-strict-mode` 测试**故意**在 `undefined`/`"0"`/`"1"` 三种取值下都断言相同的 `constrainedSampling`，即 strict sampling **不受** `KIANA_EXPERIMENTAL` 影响 |

`APP_NAME` 来自 `kianaConfig.name`，默认 `"kiana"`；`VERSION` 来自 package.json，缺失时 `"0.0.0"`。
**因此上述 `KIANA_*` 名称在改名发布（如 `TAU_*`）时会变**：Rust 侧应通过同一 `APP_NAME` 常量拼接，
不要硬编码 `KIANA_` 前缀（`KIANA_SESSION_ID` 等注入变量是另一个常量来源，见 2.9 与 ca-session）。
`getShellEnv()` 还会把 `{agentDir}/bin` 注入 PATH（已存在则不重复前置）。

### 4.4 子进程契约

**bash/powershell**（`createLocalShellOperations`）：
- argv：`[shell, "-c", command]`，legacy WSL bash → `[shell, "-s"]` + command 从 stdin 写入。
- `cwd`、`env`（`spawnContext.env`）、`detached: true`（非 win32）、stdio `[pipe|ignore, pipe, pipe]`、
  `windowsHide: true`。
- 输出：stdout/stderr **都**通过 `onData(Buffer)` 汇入同一 accumulator（交错顺序由 Node 事件循环决定）。
- 终止：超时/中止 → `killProcessTree(pid)`（进程组 SIGKILL；darwin 先 SIGSTOP）。
- 退出码：正常退出码；信号终止 → `128 + signalNumber`；无退出码且无信号 → `1`。
- 超时由 **coding-agent** 用 `setTimeout` 实现（不是子进程自己），因此超时后仍会把已收到的输出交给调用方。

**grep**：`rg --no-config --json --line-number --color=never --hidden [--ignore-case] [--fixed-strings]
[--glob G] -- PATTERN PATH`。解析 **JSON Lines**：每行 `{type:"match", data:{path:{text}, line_number,
lines:{text}}}`；其他事件（`begin`/`end`/`summary`/`context`）忽略。
非零退出码：0=有匹配、1=无匹配（都正常）；其他 → 错误（用 stderr 文本）。

**find**：`fd --glob --color=never --hidden [--no-require-git] --max-results N [--full-path] --
PATTERN PATH`。输出逐行一个路径（绝对路径，因为 searchPath 是绝对路径）。非零码且**有输出** → 容忍。

**taskkill/tar/unzip/powershell**：见 2.28.1 / 2.28.9。

### 4.5 HTML 导出格式（磁盘产物）

- 单文件 UTF-8 HTML；`<script id="session-data" type="application/json">{base64}</script>`，
  base64 解码后是 `JSON.stringify(sessionData)` 的 UTF-8 字节。
- 结构：`template.html` 的固定 DOM + 内联 CSS（`template.css` 经 4 处替换）+
  三个 vendored `<script>` + 内联 `template.js`。
- 无外部网络依赖（无 CDN、无外链字体）。
- 文件名默认 `kiana-session-{sessionBase}.html`（`sessionBase` = 会话文件名去掉 `.jsonl`）。
- 相对/绝对输出路径：`normalizePath(outputPath)`（`~` 展开、Unicode 空格归一、`@` 剥离），
  但**不做 `resolve`**（相对路径按进程 cwd 写出，返回值也是该相对字符串）。

### 4.6 无 HTTP/RPC 端点

本单元不直接发起 HTTP 请求，除 `ensureTool` 的 GitHub 下载/重定向解析：
- `GET https://github.com/{repo}/releases/latest`（`redirect: "manual"`，头 `User-Agent: {getKianaUserAgent(VERSION)}`），
  期望 3xx + `location`；解析 tag。
- `GET https://github.com/{repo}/releases/download/{tagPrefix}{version}/{assetName}`（默认重定向跟随），
  响应体流式写盘。
- 两者都经 `fetchWithRetry`（`timeoutMs` 10s / 120s，重试策略在 `utils/management-http.ts`，由 utils 单元负责）。

---

## 5. 跨单元接口

### 5.1 `core/session-manager.ts`（由 ca-session 单元提供）

本单元使用：

- 类型：`SessionEntry`、`ProjectedSessionEntry`、`SessionProjection`、`CompactionEntry`、
  `BranchSummaryEntry`、`ReadonlySessionManager`、`SessionHeader`。
- `sessionEntryToContextMessages(entry): AgentMessage[]`：条目 → 模型可见消息（`context_edit` 会改变结果；
  metadata 条目返回 `[]`）。
- `buildSessionProjection(entries): SessionProjection`，`SessionProjection = {entries: ProjectedSessionEntry[],
  messages: AgentMessage[]}`；`ProjectedSessionEntry = {sourceEntry: SessionEntry, messages: AgentMessage[]}`。
- `SessionManager` 实例方法（HTML 导出用）：`getSessionFile()`、`getEntries()`、`getHeader()`、`getLeafId()`；
  静态 `SessionManager.open(path)`。
- `ReadonlySessionManager` 供分支摘要：`getBranch(id?)`、`getEntry(id)`。

**接口不变量**：`projection.messages` 与 `projection.entries[i].messages` 的拼接**必须一致**
（`estimateProjectedContextTokens` 依赖「全局消息下标 → 来源条目」的映射）。

### 5.2 `core/messages.ts`

- `convertToLlm(messages: AgentMessage[]): Message[]`：把 `bashExecution`/`custom`/`branchSummary`/
  `compactionSummary` 转成 user/assistant 文本消息（前缀/后缀包裹规则见 ca-session 规格）。
  本单元的 `serializeConversation` 只处理 `user`/`assistant`/`toolResult`（其余角色被忽略），
  因此 `convertToLlm` 必须保证没有任何 `system` 残留（`getMessagesFromProjectedEntryForCompaction`
  已先滤掉 system）。
- `createCustomMessage(customType, content, display, details, timestamp)`、
  `createBranchSummaryMessage(summary, fromId, timestamp)`、
  `createCompactionSummaryMessage(summary, tokensBefore, timestamp)`（分支摘要用）。

### 5.3 `core/extensions/types.ts`

- `ToolDefinition<TSchema, TDetails, TState>` 字段（本单元读写的全部）：
  `name`、`label`、`description`、`promptSnippet?`、`promptGuidelines?`、`parameters`、
  `outputSchema?`、`constrainedSampling?: {type:"json_schema", strict?: "prefer"|"require"}`、
  `renderShell?: "self"`、`prepareArguments?(args): args`、`executionMode?`、
  `expose?`、`annotations?`、`execute(toolCallId, params, signal?, onUpdate?, ctx?)`、
  `renderCall?(args, theme, context)`、`renderResult?(result, options, theme, context)`。
- `ToolRenderContext<TState>`：`args`、`toolCallId`、`invalidate()`、`lastComponent`、`state`、`cwd`、
  `executionStarted`、`argsComplete`、`isPartial`、`expanded`、`showImages`、`isError`、`durationMs`、`outputPad`。
- `ToolRenderResultOptions`：`{expanded: boolean; isPartial: boolean}`。
- `ToolRenderers`：`Pick<ToolDefinition, "renderCall"|"renderResult">`。
- `ExtensionContext`（bash/read/write/edit 的 `ctx`）：`cwd`、`model?`（含 `provider`、`id`、`input`、
  `inputLimits?.images?.resize`）、`sessionManager`（`getSessionId()`、`getSessionFile()`）、
  `thinkingLevel?`。
- `ExtensionToolContext`（`wrapToolDefinition` 的 ctxFactory 产型）：`ExtensionContext` 的超集。
- 压缩事件类型（`AgentSession` 触发，本单元产出的数据形状）：
  `SessionBeforeCompactEvent {preparation, branchEntries, customInstructions, reason, willRetry, signal}`、
  `SessionCompactFailedEvent {reason, errorMessage, aborted, willRetry, fromExtension}`、
  `SessionBeforeTreeEvent {preparation, signal}`、`TreePreparation {targetId, oldLeafId,
  commonAncestorId, entriesToSummarize, userWantsSummary}`。`reason: "manual" | "threshold" | "overflow"`。

### 5.4 `@earendil-works/kiana-ai`

- 类型：`AssistantMessage`、`Usage`、`Model<Api>`、`TextContent`、`ImageContent`、`Message`、
  `SystemMessage`、`TranscriptContext`、`SimpleStreamOptions`、`StreamFn`、`RetryPolicy`、`RetryCallbacks`。
- 值：`contentText(content, fallback?)`、`normalizeContext({systemPrompt?, messages})`、
  `retryAssistantCall(produce, policy?, signal?, callbacks?)`、`uuidv7()`、`completeSimple(model, context, options)`、
  `getCurrentSystemMessage(messages)`；`kiana-ai/compat` 再导出 `completeSimple` 与上述类型。
- `Usage`：`{input, output, cacheRead, cacheWrite, totalTokens, cost:{input,output,cacheRead,cacheWrite,total}}`
  （可能含 `cacheWrite1h?`、`reasoning?`）。`calculateContextTokens` 只读前 5 个数值字段。
- `AssistantMessage.content` 块类型：`{type:"text", text}`、`{type:"thinking", thinking}`、
  `{type:"toolCall", id, name, arguments}`；`stopReason: "pending"|"stop"|"length"|"toolUse"|"error"|"aborted"`；
  `errorMessage?`。
- `SimpleStreamOptions`：`{signal?, apiKey?, headers?, env?, sessionId?, reasoning?, cacheRetention?}`。
- `normalizeContext`：把 `{systemPrompt, messages}` 变成 provider 上下文（system 作为首条 system 消息）。

### 5.5 `core/usage-totals.ts`

`combineUsage(a: Usage, b: Usage): Usage`：逐字段相加，`cost` 各字段相加（split-turn 合并用）。

### 5.6 `modes/interactive/theme/theme.ts`

- `theme.fg(token, text)`、`theme.bg(token, text)`、`theme.bold(text)`、`theme.inverse(text)`。
- 本单元用到的 token：`toolTitle`、`toolOutput`、`warning`、`error`、`muted`、`dim`、`accent`、
  `toolPendingBg`、`toolSuccessBg`、`toolErrorBg`、`toolDiffAdded`、`toolDiffRemoved`、`toolDiffContext`、
  `customMessageLabel`、`customMessageText`、`mdCodeBlock`。
- `getResolvedThemeColors(themeName?)`、`getThemeExportColors(themeName?)`、`getThemeByName(name)`、
  `getLanguageFromPath(path)`（扩展名表见下方；注意 HTML 导出模板内另有一张更小的表，见 2.27.4）、
  `highlightCode(code, lang)`（cli-highlight，
  未识别语言时用 `mdCodeBlock` 逐行着色）、`supportsLanguage(lang)`。

`getLanguageFromPath` 的扩展名 → 语言映射（逐字，全部小写键）：
`ts/tsx→typescript`、`js/jsx/mjs/cjs→javascript`、`py→python`、`rb→ruby`、`rs→rust`、`go→go`、
`java→java`、`kt→kotlin`、`swift→swift`、`c/h→c`、`cpp/cc/cxx/hpp→cpp`、`cs→csharp`、`php→php`、
`sh/bash/zsh→bash`、`fish→fish`、`ps1→powershell`、`sql→sql`、`html/htm→html`、`css→css`、`scss→scss`、
`sass→sass`、`less→less`、`json→json`、`yaml/yml→yaml`、`toml→toml`、`xml→xml`、`md/markdown→markdown`、
`dockerfile→dockerfile`、`makefile→makefile`、`cmake→cmake`、`lua→lua`、`perl→perl`、`r→r`、
`scala→scala`、`clj→clojure`、`ex/exs→elixir`、`erl→erlang`、`hs→haskell`、`ml→ocaml`、`vim→vim`、
`graphql→graphql`、`proto→protobuf`、`tf/hcl→hcl`。

### 5.7 `modes/interactive/components/*`

- `keybinding-hints.ts`：`keyHint(keybinding, description)` = `theme.fg("dim", keyText(kb)) +
  theme.fg("muted", ` ${description}`)`；`keyText(kb)`（键名，`darwin` 上 `alt`→`option`，
  `formatKeyText` 按 `/` 与 `+` 拆分）、`rawKeyHint`。本单元只用 `app.tools.expand`。
- `visual-truncate.ts`：`truncateToVisualLines(text, maxVisualLines, width, paddingX=0, keep="end")`
  与 `VisualLinePreview`（按宽度缓存行；渲染时把 hint 放在保留行的前/后）。`VisualLinePreview.render(width)`：
  `truncateToVisualLines(text, maxVisualLines, width, 0, keep)`；`skippedCount > 0` 时插入
  `formatHint(skippedCount)`（keep="end" 时在**前面**，keep="start" 时在**后面**）。
- `diff.ts`：`renderDiff(diffText, {filePath?})`：逐行解析 `/^([+-\s])(\s*\d*)\s(.*)$/`；
  非匹配行 → `theme.fg("toolDiffContext", line)`；连续 `-` 行后跟连续 `+` 行且各只有 1 行 →
  `renderIntraLineDiff`（`Diff.diffWords`，首个 removed/added 部分的前导空白保留在行外，
  其余变更片段用 `theme.inverse`）；`+` 行用 `toolDiffAdded`、`-` 行用 `toolDiffRemoved`、
  ` ` 上下文用 `toolDiffContext`；tab → 3 空格；`renderDiff` 结尾会 `trimEnd()` 并去掉末尾空行
  （实现细节见文件；Rust 侧应对齐输出的精确行集）。
- `tool-execution.ts`：`ToolExecutionComponent` 是渲染调度器，使用 `withBuiltInRenderers` 与
  `createAllToolRenderers`；工具结果里的 image 块在加载 transcoder 后转换（`converts non-PNG tool images
  once the transcoder loads`）；自定义 call/result 渲染器堆叠顺序（`stacks custom call and result
  renderers like the old implementation`）；`self` 渲染壳的空行不占布局（`self-rendered empty tool rows
  take no layout space`）。

### 5.8 `@earendil-works/kiana-tui`

- 组件：`Text(text, paddingX=0, paddingY=0)`（`setText`、`render(width): string[]`、`invalidate`）、
  `Container`（`addChild`、`clear`、`render`、`invalidate`）、`Box(paddingX, paddingY, bgFn)`（`setBgFn`、
  `setPaddingX`）、`Spacer(lines)`、`Component` 接口。
- 工具函数：`getCapabilities()` → `{hyperlinks, images, ...}`、`hyperlink(text, url)`、
  `getImageDimensions(base64, mimeType)`、`imageFallback(mimeType, dims?, filename?)`、
  `truncateToWidth(text, width, ellipsis)`、`stripAnsi`（TUI 版本）。

### 5.9 `core/agent-session.ts`（消费方，契约）

- 调用 `prepareCompaction(pathEntries, settings)` / `compact(...)`；
  失败/取消时发 `session_compact_failed`；成功时 `saveCompaction({summary, firstKeptEntryId, tokensBefore,
  usage, details, fromHook})`。
- 调用 `collectEntriesForBranchSummary(sessionManager, oldLeafId, targetId)` /
  `generateBranchSummary(entries, options)`；把结果存成 `BranchSummaryEntry`。
- `exportToHtml(outputPath?, {themeName})`（见 2.22.8 末尾）。
- `executeBash`（`core/bash-executor.ts`）与本单元的 bash 工具**共享** `DEFAULT_MAX_BYTES` 与
  `truncateTail`；其临时文件前缀也是 `kiana-bash`，但滚动缓冲是 `maxOutputBytes = DEFAULT_MAX_BYTES*2`
  的字符串缓冲（无 `OutputAccumulator`），行为需保持各自的既有语义。

### 5.10 `core/settings-manager.ts`

`getCompactionSettings(model?)` 返回已解析的 `{enabled, reserveTokens, keepRecentTokens}`
（含 per-model overrides 与校验错误，见 `settings-manager-compaction.test.ts`）。
`getBranchSummarySettings()` → `{reserveTokens, skipPrompt}`。`getTheme()`、`getImageAutoResize()`、
`getShellPath()`、`getShellCommandPrefix()`、`getRetrySettings()`、`getDefaultTools()`。

### 5.11 `core/system-prompt.ts`

`buildSystemPrompt` 读取每个定义的 `promptSnippet` 与 `promptGuidelines` 生成工具段；
`tool-system-prompt-contributions.test.ts` 断言每个定义的 `promptSnippet`/`promptGuidelines`
与其导出的 contribution 常量一致（Rust 侧对应「定义与常量不得漂移」的测试）。

### 5.12 codemode / SDK（`outputSchema` 消费方）

- `bash` 的 `structuredContent`：`{output, truncated, full_output_path?, exit_code, wall_time_seconds}`。
- `read` 的 `structuredContent`：字符串或 `{type:"image", data, mimeType, note}`。
- 其余工具无 `outputSchema` → 程序化调用者拿到的是普通 `content` 数组。
- `constrainedSampling`：`{type:"json_schema", strict:"prefer"}`；`KIANA_EXPERIMENTAL` 为 `0`/`1`/undefined 时
  仍偏好 strict（`builtin-tool-strict-mode.test.ts`），但**显式 opt-out 必须保留**（wrap 不得覆盖）。

---

## 6. npm 依赖与 Rust crate 映射

### 6.1 本单元实际使用的 npm 包

`packages/coding-agent/package.json` 的 `dependencies` 中，本单元直接 import 的：

| npm 包 | 版本 | 本单元用途 | Rust 等价 | 差异与备注 |
|---|---|---|---|---|
| `typebox` | 1.3.27 | 参数/输出 JSON Schema | `serde_json::json!` 常量，或 `schemars` | TypeBox `Type.Object` 默认 `additionalProperties:false`、`required` 只含非 Optional；Rust 要**手写** schema 常量以保证字段顺序与描述逐字一致。不要用 derive 生成的命名/`$ref` 结构。 |
| `diff` | 8.0.4 | `diffLines`、`createTwoFilesPatch`、`FILE_HEADERS_ONLY`、`diffWords`（TUI） | `similar`（`TextDiff` / `udiff` / `ChangeTag`） | `createTwoFilesPatch` 的 67 个 `=` 分隔行、`---`/`+++` 无时间戳、`@@ -a,b +c,d @@` 头、无 hunk 时返回空串——`similar` 的 `unified_diff()` 输出格式不同（无 `=` 行、头部带 `---`/`+++`）。必须**自写**格式化层（见 8.3）。`diffLines` 的分组顺序（先 removed 后 added）也要对齐。 |
| `highlight.js` | 10.7.3（dev，vendored 到 export-html） | HTML 语法高亮（浏览器端） | 不移植，原样内嵌 | 只作为静态资产。 |
| `marked` | 18.0.11（dev，vendored） | HTML markdown 渲染（浏览器端） | 不移植，原样内嵌 | 只作为静态资产。 |
| `cross-spawn` | 7.0.6 | win32 spawn 兼容 | `tokio::process::Command` | Windows 上 Node 需要 cross-spawn 处理 `.cmd`/PATHEXT；Rust 的 `Command` 已有类似解析，但 `.cmd` 需 `cmd /c`（若需要，另行处理）。 |
| `ignore` / `minimatch` | — | **不在本单元**（find 交给 fd） | — | find/grep 依赖外部 fd/rg 而非 JS 实现。 |
| `chalk` | 6.0.0 | `stripAnsi` 参考实现（测试） | `strip-ansi-escapes` 或自写 | 测试用参考实现；Rust 需按 2.28.5 的正则语义自写（`strip-ansi-escapes` 语义不同）。 |
| `yaml` | 2.9.0 | 不用于本单元 | `serde_yaml`/`serde_norway` | 主题/设置由其他单元负责。 |

Node 内置模块（无 crate）：

| Node API | Rust 方案 |
|---|---|
| `node:fs` / `fs/promises`（readFile/writeFile/stat/readdir/access/mkdir/realpath/open/rename/chmod/rm） | `std::fs` + `tokio::fs`；`realpath` = `std::fs::canonicalize`（注意 JS `realpath` 不要求路径存在以外语义一致；ENOENT/ENOTDIR → 用 `resolved` 路径） |
| `node:path`（join/resolve/dirname/basename/relative/isAbsolute/sep/delimiter/normalize、`path.win32`） | `std::path`；**必须自实现 JS 语义**（`path.resolve` 的 cwd 归一、`path.relative` 的 `..` 输出、Windows 分隔符/盘符大小写）。`path-clean` 不够（不处理 Windows）。建议写 `js_path` 模块并对照测试 `6104`。 |
| `node:os`（homedir/tmpdir/platform/arch） | `dirs`/`home`、`std::env::temp_dir()`、`std::env::consts::{OS, ARCH}` |
| `node:child_process`（spawn/spawnSync、ChildProcess 事件、信号） | `tokio::process::Command` + `nix`（`killpg`/`SIGSTOP`）/ win32 `taskkill` |
| `node:readline`（createInterface 逐行） | `tokio::io::BufReader` + `AsyncBufReadExt::lines()`（**JS 会把 `\r\n` 剥离**，Rust 需 `.trim_end_matches('\r')` 对齐） |
| `node:crypto`（`randomBytes`） | `rand::random::<[u8; 8]>()` + `hex`（或用 `uuid` v4 的简单字节） |
| `TextDecoder`（streaming UTF-8） | 自写 `Utf8StreamDecoder`（保留 0-3 字节不完整尾；非法序列按 WHATWG 替换） |
| `Buffer.byteLength(str,"utf-8")` | `s.as_bytes().len()` |
| `Buffer.toString("utf-8")` | 见 2.5 末的 7 个黄金用例（WHATWG 替换语义） |
| `performance.now()` | `std::time::Instant` |
| `AbortSignal` / `AbortController` | `tokio_util::sync::CancellationToken` + `tokio::select!`；或 `Arc<AtomicBool>` + `Notify`（工具实现只需「检查 + 杀掉子进程」） |
| `URL` / `pathToFileURL`（hyperlink） | `url` crate（`Url::from_file_path`） |
| `fetch`（ensureTool） | `reqwest`（rustls-tls、`redirect::Policy::none()` 读 `location`） |
| `tar`/`unzip`/`taskkill`（子进程） | 直接 `Command` 调用系统工具（与 TS 行为一致，**不要**改用 Rust crate 解压，否则错误消息/容忍度不同） |

### 6.2 建议的 Rust crate 清单（本单元）

```toml
tokio = { version = "1", features = ["full"] }          # 异步、进程、fs、时间
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }  # IndexMap 保插入序
similar = "2"                                            # 行 diff 原始数据（格式化自写）
unicode-normalization = "0.1"                            # NFKC / NFD
unicode-width = "0.2"                                    # TUI 宽度（kiana-tui 单元共用）
regex = "1"                                              # ANSI/URL/技能块正则
base64 = "0.22"                                          # 导出数据
tempfile = "3"                                           # 可选（本单元用自写 0600+wx 文件，故可不引入）
rand = "0.8"                                             # 临时文件名随机
hex = "0.4"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream"] }
flate2 = "1" / tar = "0.4" / zip = "2"                   # 仅在「不想调系统 tar/unzip」时用；见 6.1 备注
tokio-util = { version = "0.7", features = ["rt"] }      # CancellationToken
nix = { version = "0.29", features = ["signal", "process"] }  # killpg / SIGSTOP（unix）
libc = "0.2"                                             # 兜底
uuid = { version = "1", features = ["v7"] }              # 仅测试/兜底（uuidv7 由 ai 单元提供）
icu_collator = "1"（可选）                                # ls 的 localeCompare 对齐
```

**不要**引入：`markdown`/`pulldown-cmark`（HTML 导出用浏览器端 marked）、`syntect`
（TUI 高亮走 cli-highlight 的等价物，由 theme 单元负责）、`ignore`/`globset`
（find 用 fd，不在 Rust 内实现 glob）。

### 6.3 JS 与 Rust 行为差异清单（必须显式处理）

1. **字符串长度与索引是 UTF-16 码元**。受影响位置：
   `truncateLine`（`maxChars` 与 `slice`）、`truncateForSummary`、`estimateTokens` 的所有 `.length`
   （文本/thinking/summary/command/output/JSON）、`formatToolCallWithArgs` 的 100 字符截断、
   `template.js` 的 `truncate`（不移植）、`serializeConversation` 的 `truncatedChars` 计数、
   `read` 的 `allLines.split("\n")`（安全，按字节模式匹配）。
   **Rust 方案**：为本单元建立 `JsStr` 辅助：`utf16_len(&str)`、`utf16_slice(&str, start, len) -> String`
   （按码元切，边界落在代理对中间时输出 U+FFFD，与 JS 产生孤立代理后的下游行为对齐——若下游是
   `JSON.stringify`，JS 会输出 `\ud83d` 转义，Rust `serde_json` 会输出 U+FFFD，**这是已知不可完全对齐点**，
   需在测试里固定取舍：建议按码点边界安全截断并在文档中记录）。
2. **`JSON.stringify` 的数字格式**：JS 用最短往返表示（`1`、`1.5`、`1e+21`、`-0`→`0`、`1e-7`→`1e-7`）。
   `serde_json` 对 `f64` 输出 `1.0`、`1e21`（无 `+`）、`-0.0`。受影响：toolCall 参数序列化
   （`serializeConversation`）、`toolsAdded` 的 token 估算、HTML 导出的 base64 数据（`tokensBefore`、
   `wall_time_seconds`、`cost.total`）。**Rust 方案**：`JsonNumber` 包装枚举（整数/浮点），
   浮点用 `ryu` 的 `format_finite` 后按 ECMAScript `Number::toString` 规则改写（处理指数阈值
   `>=1e21`/`<1e-6`、`-0`、整数尾 `.0` 去除）。整数一律 `i64`/`u64` 直出。
3. **`JSON.stringify` 的键序**：对象字面量顺序；`undefined` 值键被省略；数组保持顺序。
   `serde_json` + `preserve_order` 可对齐「插入序」，但 `skip_serializing_if = "Option::is_none"`
   必须逐字段显式标注（例如 `firstChangedLine`、`fullOutputPath`、`truncation`、`usage`、`details`）。
4. **`JSON.stringify` 的字符串转义**：JS 只转义 `"`、`\`、控制字符（`\b\f\n\r\t` 与 `\u00XX`），
   **不转义** `/`、`<`、`>`、`&`、U+2028/U+2029（现代引擎）；`serde_json` 也不转义这些，但会转义
   U+0000-U+001F（相同）。**差异**：JS 对孤立代理输出 `\udXXX`；Rust 无孤立代理。
5. **正则方言**：本单元的 JS 正则都很简单（`[\d;]*`、`[^\n]*\n|[^\n]+`、`\s*`、URL scheme、
   技能块 `[\s\S]*?` + 可选组）。`regex` crate 不支持反向引用/环视，但本单元**未使用**。
   注意 `.` 在 `regex` 默认不匹配 `\n`（与 JS 相同，无 `s` 标志时）；技能块需要 `(?s)` 或 `[\s\S]`。
   `template.js` 的正则不移植。
6. **`localeCompare`（ls 排序）**：JS 默认 locale（Node 用系统 ICU，通常 en-US）。
   `"a".localeCompare("B")` 在 en-US 下为负（a < b，大小写不敏感主序）。本单元先 `toLowerCase()`
   再比较，因此只剩「同小写后的 tie-break」（如 `"a"` vs `"A"`、变音符号、数字）。
   Rust 方案：优先 `icu_collator`（`Collator::try_new(&locale!("en-US"), CollatorOptions::default())`）；
   退化方案 `a.to_lowercase().cmp(&b.to_lowercase())` + 原始串 tie-break，并在测试中固定
   `ls` 的输出顺序用例（`should list dotfiles and directories`）。
7. **`Math.round` / `toFixed`**：`Math.round(-0.5)` → `-0`，Rust `round()` → `-1`（本单元输入非负，
   不受影响）。**`toFixed(1)` 与 `format!("{:.1}")` 并不一致**：JS 规范要求「精确十进制值的最近邻，
   中点向上（away from zero）」，Rust 的 `{:.1}` 对 f64 是「精确值最近邻，中点取偶」。
   已实测反例（`formatSize` 的 `bytes/1024`）：`256 → "0.3"`（Rust `"0.2"`）、`768 → "0.8"`
   （Rust `"0.8"`）、`1280 → "1.3"`（Rust `"1.2"`）、`2304 → "2.3"`（Rust `"2.2"`）。
   见 2.1.4 的实现建议与单测要求。
   `Math.round(x*10)/10` 的 `wall_time_seconds` 与 `formatToolDuration` 需逐位对齐（Rust 用
   `(ms / 100.0).round() / 10.0` 并注意 `f64` 打印；`Math.round` 对 `.5` 向上，Rust `round()` 亦然，
   但负零/NaN 需单独处理）。
8. **`Date` 格式化**：HTML 里 `new Date(ts).toLocaleString()` / `toLocaleTimeString()` 在**浏览器**执行，
   Rust 无需实现（但导出数据里的 `timestamp` 字符串必须与 TS 相同：ISO 8601 `YYYY-MM-DDTHH:mm:ss.sssZ`）。
9. **Base64**：`Buffer.from(str).toString("base64")` = 标准字母表 + `=` 填充，无换行。
   `base64::engine::general_purpose::STANDARD` 一致。
10. **TextDecoder 流式解码**：`{stream:true}` 保留不完整尾（最多 3 字节），`decode()` flush。
    非法字节 → U+FFFD，按 WHATWG「maximal subpart」分组。已验证的边界：`[E0,80,80]`→3 个、
    `[F0,80,80,80]`→4 个、`[C0,80]`→2 个、`[ED,A0,80]`→3 个、`[F5,80,80,80]`→4 个、
    `[E2,82]`→1 个、`[80,80,80]`→3 个（`Buffer.toString("utf8")` 与 Python `errors="replace"` 在这些
    用例上一致）；`from_utf8_lossy` 在这些用例上也一致，但它**不保证** WHATWG 语义，且**没有
    stream 模式**。`OutputAccumulator` 的 `tailBytes/totalDecodedBytes` 依赖解码后的字节数，
    因此 Rust 必须实现「保留不完整尾」的增量解码器（或用 `encoding_rs` 的 `Decoder`），
    不要用 `from_utf8_lossy` 逐块解码（算法要点见 8.2 #8；模块划分见 8.1 的 `Utf8StreamDecoder`）。
11. **`realpath`**：Node `fs.realpath` 默认解析符号链接并返回绝对路径；Rust `canonicalize` 在 Windows
    会加 `\\?\` 前缀（需剥离 `\\?\` 并转 `/`? —— 只需与其他 key 一致），在路径不存在时同样报 ENOENT。
    注意 Rust `canonicalize` 对 `..` 的处理与 Node 一致（都解析）。
12. **`process.env` 大小写**：Windows 环境变量键不区分大小写；Node 的 `process.env` 在 Windows 上
    代理为不区分大小写。Rust `std::env::vars` 保留原样大小写。`getShellEnv` 的「找 path 键」逻辑
    需实现为大小写不敏感查找（并在 Windows 上把找到的键作为写入键）。
13. **`spawn` 的 shell 语义**：Node 在 win32 无 shell 时用 `cross-spawn` 解析 `.cmd`；Rust 需显式处理
    （或仅在 Windows 上用 `cmd /c`）。Linux/macOS 上两者等价。
14. **进程组**：`detached:true` 使子进程成为进程组组长，`process.kill(-pid)` 杀整组。Rust 用
    `Command::new(...).process_group(0)`（unix，Rust 1.64+ 的 `std::os::unix::process::CommandExt`）
    再 `nix::sys::signal::killpg(Pid::from_raw(pid), SIGKILL)`。
15. **退出码与信号**：Node `child.signalCode` 是信号名字符串；`os.constants.signals[name]` 是号码。
    Rust `ExitStatus::signal()` 直接给号码 → `128 + n`；无信号无码 → 1。
16. **`String.slice` 负数/越界**：JS 对负数从尾部计、越界截断。本单元出现的 `slice` 入参都非负
    （`slice(0, maxChars)`、`slice(1)`、`slice(start, end)`），Rust 按字节边界即可，但要按码元语义
    （见 1）。
17. **`Array.prototype.sort` 稳定性**：JS（V8）排序稳定。`computeFileLists` 用默认字符串比较
    （UTF-16 码元序）；`ls` 用 `localeCompare`；`matchedEdits.sort` 按数字。Rust `sort_by` 稳定，
    `sort_unstable` 不可用于这些位置。
18. **`Object.entries` 顺序**：插入序（非整数样键）。`serializeConversation` 的 toolCall 参数顺序
    = 模型返回的 JSON 键序 = `serde_json` 保留序（模型输出原文解析后保序）。
19. **异常 vs Result**：TS 大量使用 `catch {}`（吞掉一切）与 `Promise` 的隐式错误传播。
    Rust 需逐处把「吞掉」写成显式 `let _ = ...;` 或 `if let Ok(...)`，尤其：
    `read` 的 abort catch、`grep`/`find` 的 JSON 解析失败、`ls` 的 stat 失败跳过、
    `tools-manager` 的 `commandExists`、`getThemeExportColors` 的 `catch { return {} }`。
20. **`Buffer.concat` / 子数组**：`buf.subarray` 是零拷贝视图；Rust `&[u8]` 切片等价。
21. **`atob`/`btoa`**：仅在浏览器端（template.js），Rust 不涉及。
22. **`Number.MAX_SAFE_INTEGER`**：`truncateHead(..., {maxLines: Number.MAX_SAFE_INTEGER})` 表示
    「不限制行数」。Rust 用 `usize::MAX` 或 `Option<usize>`；注意比较 `totalLines <= maxLines`
    与 `outputLinesArr.len() >= maxLines` 在 `usize::MAX` 下语义一致。

---

## 7. 测试清单

路径均相对 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/test/`。测试框架 vitest 4。
「必须复现」= Rust 侧要么移植为等价单测，要么作为验收脚本。

行数说明：表中数字是对当前工作树 `wc -l` 的实测值（审计时逐条核对；`1000+`/`200+` 已改为精确值）。
若上游继续演进，行数会变，但**用例名与断言文本**是稳定契约。

### 7.1 工具实现

| 文件 | 行数 | 覆盖行为（必须复现） |
|---|---|---|
| `tools.test.ts` | 1452 | read：正常读取、不存在、行截断、字节截断、offset、limit、offset+limit、offset 越界错误文本、truncation details、按魔数识别图片 MIME、BMP 转 PNG、图片扩展名但非图片内容按文本；write：写内容、创建父目录；edit：基本替换、未找到、ENOENT 消息、多次出现、多区间、大间隙折叠、按原始文件匹配、空 edits、重叠、失败不部分应用、EACCES、未知错误消息、diff 预览 ENOENT/EACCES；bash：简单命令、非零退出码（error result + structuredContent）、1 MiB structured output、自定义操作返回 null 退出码被拒、超时、超时/中止时带 full output path、cwd 不存在、spawn 错误、shellPath 透传、stdin 传输、legacy WSL 检测、commandPrefix（及两者输出）、无前缀、流式更新合并（节流）、尾随换行不额外计行、跨 chunk UTF-8、`createLocalBashOperations` 可复用、与 executeBash 一致的 sanitize、仅按行截断也持久化全文；grep：单文件含文件名、全局 limit + context 行、忽略 `RIPGREP_CONFIG_PATH`、`-` 开头 pattern 当文本；find：隐藏文件（未被 gitignore）、遵守 .gitignore、fd glob 解析错误、`-` 开头 pattern；ls：dotfiles 与目录后缀。另有：工具 cwd 解析（7 个工具都用 `ctx.cwd`）、edit 模糊匹配 12 例（行尾空白、全角标点、兼容形式、智能单/双引号、Unicode 破折号、NBSP、精确优先、仍失败、归一化后重复、多编辑模糊、等值邻近行的正确出现、保持未触及行且 patch 可应用）、CRLF 6 例（LF oldText 匹配 CRLF、保留 CRLF、保留 LF、CRLF/LF 变体重复检测、保留 BOM、多编辑 BOM+CRLF）。 |
| `tool-system-prompt-contributions.test.ts` | 44 | 8 个定义的 `promptSnippet`/`promptGuidelines` 与导出的 contribution 常量逐字一致；bash/powershell 在 `exposeSessionEnvironment:false` 时 guidelines 为 undefined。 |
| `builtin-tool-strict-mode.test.ts` | 110 | `KIANA_EXPERIMENTAL` 为 undefined/"0"/"1" 时都偏好 strict sampling；wrap 时保留显式 opt-out；不同 activeTools 组合下 strict 列表。 |
| `tool-result-images.test.ts` | 137 | `normalizeToolResultImages`：无图原样（同一数组）、已在限制内原样、超大缩放并报告原始尺寸、关闭自动缩放仍转换不支持格式、无法解码保留、文本块顺序保持。 |
| `powershell-tool.test.ts` / `powershell-config.test.ts` | 30 / 90 | 进程内执行策略 bypass（UTF8 前缀）；PowerShell 解析顺序 5 例（PATH pwsh7 优先、ProgramFiles 下的 pwsh7 优先于 PATH 上的 powershell、PATH 上的 powershell 优先于系统安装、SystemRoot 兜底、找不到报错）。 |
| `default-tools-setting.test.ts` | 476 | defaultTools 初始选择、可用 powershell 替代 bash、`+name` 激活扩展工具、保留扩展/SDK 工具、显式选项优先、`+/-` 修饰符、非法修饰符报错；reload 与 resume 的 loadout 恢复（含压缩前的移除、树导航、排除工具）。 |
| `tool-renderer-examples.test.ts` | 112 | 各工具渲染器示例输出；minimal 模式下 edit 仍在默认 shell。 |
| `tool-execution-component.test.ts` | 777 | 渲染调度：非 PNG 工具图片在 transcoder 加载后转换；自定义 call/result 堆叠顺序；self 渲染空行不占布局；内置 override 无自定义渲染器时用内置；`file_path` 兼容；bash 首个空 partial update；bash 渲染器不重复最终 full output 截断细节；结果记录的时长（部分结果也显示）；不重复内置头；read 的 null offset/limit 渲染为整文件读；缺失的 result/call 渲染槽从内置继承；复用内置定义时的自定义渲染器；共享渲染状态；result 上下文暴露 args；fallback 头显示参数；fallback 折叠直到展开；write 预览去尾空行；流式部分参数每帧一次；read 结果去尾空行；read 错误不按路径高亮。 |
| `bash-execution-width.test.ts` | 80 | 折叠预览按渲染时宽度（非构造时）折行；宽度变化重算。 |
| `bash-close-hang-windows.test.ts` | 126 | 继承 stdio 句柄不关闭时，`executeBash` 与 bash 工具仍在 shell 退出后 resolve。 |
| `truncate-to-width.test.ts` | 81 | Unicode/emoji/宽字符截断、不需截断、省略号、崩溃回归用例（kiana-tui 的 `truncateToWidth`，由 tui 单元实现，但被本单元渲染器使用）。 |
| `read-line-range-display.test.ts` | 155 | `formatReadLineRange` 的原始参数格式化；字符串范围在 TUI、会话树、HTML 导出三处一致。 |
| `edit-tool-legacy-input.test.ts` | 116 | `prepareArguments`：legacy 字段不进公开 schema；顶层 oldText/newText 折叠进 edits；追加到已有 edits；有效输入原样；非对象原样；折叠后能执行；`edits` 为 JSON 字符串时解析；非法 JSON 原样。 |
| `edit-tool-no-full-redraw.test.ts` | 236 | 大 diff 在 call 预览中渲染且结果 settle 时不整体重绘；无 argsComplete 时从已存结果重建盒子预览；编辑不适用时显示预检错误且不渲染 diff。 |
| `file-mutation-queue.test.ts` | 274 | 同文件串行、不同文件并行、符号链接别名共用队列；内置 edit/write 并行编辑同一文件都保留；edit 与 write 共用队列；中止的 write/edit 仍在飞行时队列保持锁定（4 例）。 |
| `path-utils.test.ts` | 174 | `expandPath`（~、~/、字面 `~file`、Unicode 空格）；`resolveToCwd`（绝对、相对、`~file`）；`resolveReadPath`（存在、NFC/NFD、弯引号、NFC+弯引号、AM/PM 窄空格、小写 am/pm）。 |
| `ansi-utils.test.ts` | 110 | `stripAnsi` 与 chalk `strip-ansi` 在生成用例上一致（OSC、未终止 OSC、DCS/PM/APC、8-bit CSI、RIS、单字节 ESC 序列）；非字符串抛 TypeError（消息逐字）；常见组合序列。 |
| `tools-manager.test.ts` | 175 | `getLatestVersion` 6 例；`ensureTool` 通过回调报告状态（不写 console）；下载失败暴露 cause 链；arm/ia32 不下载 x86-64 二进制。 |
| `mcp-extension.test.ts` | 868 | 直接消费者验收 `truncateMiddle`：`cuts the middle of model-facing text over 20KB and keeps the full result for scripts`（header 两行 + `…N chars truncated…` + `[Full output: …]`，最终文本 < 21KB，image 块跟在文本后，`structuredContent` 保留原始）、`cuts multi-byte text only at character boundaries`（1001 字节预算下 head ≤ 500、tail ≤ 501、无 U+FFFD、code point 总数守恒）。其余 27 例属 MCP 单元。 |
| `block-images.test.ts` | 148 | `images.blockImages` 的 **read 工具侧**契约：`should always read images (filtering happens at convertToLlm layer)`（read 必须返回 image 块）、文本文件正常读、`processFileArguments` 同样不过滤、BMP 落盘为 PNG 附件。 |
| `image-resize-callers.test.ts` | 95 | read 工具与 `processFileArguments` 的 image 失败路径：`resizeImage` 返回 null → 仅文本且含 `Image omitted`；以及 `ctx.model.inputLimits.images.resize` 透传给 read 工具。 |
| `agent-session-dynamic-tools.test.ts` | 256 | bash 工具的 `exposeSessionEnvironment` 与 `spawnHook`：hook 能看到 `KIANA_SESSION_ID/KIANA_SESSION_FILE/KIANA_PROVIDER/KIANA_MODEL/KIANA_REASONING_LEVEL`；`exposeSessionEnvironment:false` 时这些变量**全部不存在**（不是空串）。 |
| `suite/agent-session-bash-persistence.test.ts` | 399 | `executeBash` 的流式契约（与 `splitIncompleteAnsiSuffix` 同源）：跨 chunk 被切开的 ANSI 序列（颜色复位、紧跟在 ESC 后的 CSI、OSC 未终止在 ST）、末尾不完整多字节字符 flush 为 U+FFFD、超长未终止序列（`MAX_PENDING_ANSI_LENGTH=256`）不得扣留输出。 |

### 7.2 压缩

| 文件 | 行数 | 覆盖行为 |
|---|---|---|
| `compaction.test.ts` | 655 | Token 计算：`calculateContextTokens`（totalTokens 优先、零值回退）；`getLastAssistantUsage`（跳过 aborted、全零、无 assistant）；`estimateContextTokens`（用最后一个非零 usage 作为锚）；`shouldCompact`（超阈值 true、禁用 false）；`findCutPoint`（按实际 token 差、无有效切点返回 startIndex、全部在预算内保留全部、切在 assistant 时 isSplitTurn、context-visible custom 消息计预算、超大尾随 tool result 时回退到最近有效切点）；`buildSessionContext`（无压缩、单压缩、多次压缩只用最新、firstKeptEntryId 为首条时保留全部、model/thinking 变化）；`prepareCompaction`（system 消息不算对话历史）；有前次压缩时（保留消息仍在窗口内则跳过重复压缩、窗口移过则重新总结）；大型会话 fixture 5 例（解析、找切点、加载、生成压缩结果、压缩后会话合法）。 |
| `compaction-serialization.test.ts` | 109 | `serializeConversation`：长 tool result 截断（含 `[... N more characters truncated]`）；短的不截断；assistant/user 不截断；省略 thinking。 |
| `compaction-summary-reasoning.test.ts` | 320 | `generateSummary` 的 reasoning 选项：有 reasoning 能力的模型用给定 thinking level；保留字符串返回；使用全新 routing session 且不写 prompt cache；尊重调用方 sessionId 与 tool choice；split turn 下保留 previousSummary 且不发起空历史请求；自定义指令应用到 turn prefix；拒绝会话摘要的 toolCall；拒绝 split turn 摘要的 toolCall；拒绝 length 限制历史摘要；拒绝 length 限制 split-turn 摘要；thinking off 不设 reasoning；非 reasoning 模型不设；Anthropic refusal fallback 交给 kiana-ai 元数据；split-turn usage 合并。 |
| `compaction-nested-calls.test.ts` | 27 | 文件操作包含 tool result 上记录的嵌套调用（codemode）。 |
| `compaction-extensions.test.ts` | 416 | before_compact/compact 事件；扩展取消；扩展提供自定义压缩；compact 事件含已保存条目；扩展抛错时继续默认压缩；多扩展顺序；before_compact 数据正确；扩展压缩用不同值（firstKeptEntryId/tokensBefore/details）。 |
| `compaction-extensions-example.test.ts` | 152 | 文档示例类型检查；示例通过 modelRegistry.complete 分派；compact 事件字段。 |
| `branch-summarization.test.ts` | 114 | `does not override tool choice for branch summaries`；`rejects tool calls from branch summaries`（错误文本 `Branch summarization attempted to call a tool`）；`rejects length-limited branch summaries`（`Branch summarization failed: generation hit the token cap and the summary is incomplete`）。 |
| `branch-summary-extensions.test.ts` | 58 | 扩展提供 summary 的 usage 持久化进会话总计。 |
| `agent-session-compaction.test.ts` | 209 | `compact()` 手动压缩；压缩后会话状态合法；持久化到会话文件；`--no-session` 内存模式；压缩事件。 |
| `agent-session-auto-compaction-queue.test.ts` | 449 | 阈值压缩后仅 agent 级排队消息时继续；溢出恢复只一次；忽略 stale pre-compaction usage；error 消息用最后一次成功 usage 触发阈值；无先前 usage 不触发；仅有保留的压缩前 usage 不触发。 |
| `settings-manager-compaction.test.ts` | 192 | modelOverrides：默认值；字段独立解析；缺失字段回退；精确匹配 `provider/modelId`（含斜杠）；项目级逐字段合并；`enabled` 全局且保存开关时保留 overrides；非法值（null/-1/1.5/"400000"/true/{}/[]/超安全整数）与 NaN/Infinity 报错；接受 0。 |
| `interactive-mode-compaction.test.ts` | 283 | 压缩与分支摘要成本使用 cache miss 通知设置；每条压缩成本渲染在其摘要后；保留条目并追加最新摘要成本；同一 agent run 恢复后更新工作状态；交互式响应中止经 AgentSession；flush 进活动 run 时的 steering 行为。 |
| `suite/agent-session-compaction.test.ts` | 1018 | 压缩表征测试 27 例（扩展摘要、系统状态检查点、队列恢复、无模型/无 auth 报错、custom streamFn、bearer auth、独立压缩请求上下文、usage 持久化、auto-compact custom streamFn、auto-compact 失败通知、length stop 后恢复、steering 合并、终止性 tool result 后不压缩、length 达输出上限不压缩、二次截断只重试一次、重复 length 填满上下文保留 overflow 措辞、abortCompaction 取消、等待空闲、阈值压缩后队列、溢出恢复不重试、成功溢出响应不重试、stale usage、error 消息阈值、无 usage、仅保留 usage、低于阈值或禁用时不压缩）。 |
| `suite/agent-session-compaction-model-overrides.test.ts` | 175 | 4 种 reason（manual/pre-prompt/post-run/overflow）都用模型 overrides；切换模型不改普通设置；在 await 摘要鉴权前捕获模型身份。 |
| `suite/regressions/11227-recovery-omits-tool-results.test.ts` | 79 | 恢复省略 tool results 的顺序与投影。 |
| `suite/regressions/5217-compaction-reason.test.ts` | 95 | 压缩 reason 取值。 |
| `suite/regressions/6647-compaction-retries-transient-stream-drop.test.ts` | 192 | 压缩重试瞬时流中断。 |
| `suite/regressions/7048-compaction-truncated-summary.test.ts` | 48 | 截断的摘要被拒绝。 |
| `suite/regressions/6768-copilot-compaction-base-url.test.ts` | 114 | `uses the auth-resolved base URL through the SDK-style stream wrapper`：压缩请求走 auth 解析出的 base URL。 |
| `suite/regressions/6324-branch-summary-ambient-auth.test.ts` | 62 | `summarizes tree branches when request auth has no API key`：请求鉴权无 API key 时仍能生成分支摘要（环境鉴权兜底）。 |
| `suite/regressions/7150-rpc-prompt-during-compaction.test.ts` | 92 | 手动压缩进行中拒绝 RPC prompt（错误含 `compaction is in progress`），不写入用户消息、不发 agent_start/agent_settled。 |
| `suite/regressions/7253-manual-compact-during-response.test.ts` | 83 | 响应中手动压缩：先持久化 aborted 响应，再压缩（compaction 条目在 aborted 之后，只有一条）。 |
| `suite/regressions/9178-tree-during-compaction.test.ts` | 102 | 压缩中拒绝树导航（错误逐字 `Wait for the current compaction or tree navigation to finish before navigating the session tree.`）；第二次导航在第一次等待时被拒。 |
| `suite/regressions/9340-9777-auto-compaction-cancellation.test.ts` | 158 | abort 后不启动 post-run auto-compaction；取消摘要鉴权；从 compaction_start 同步取消；把匹配错误文本与无关 AbortError 都报告为失败（`aborted:false`）；扩展取消报告为 aborted。 |
| `suite/regressions/3688-tree-cancel-compacting.test.ts` | 36 | 压缩中取消树导航。 |
| `suite/regressions/pre-prompt-compaction-no-continue.test.ts` | 75 | pre-prompt 压缩后不继续。 |
| `suite/regressions/8328-zero-usage-auto-compaction.test.ts` | 80 | 无任何 assistant 报告 usage 时用**消息文本估算**触发自动压缩（`uses the message estimate when no assistant has reported usage`）；估算低于阈值时不压缩（`does not compact when the zero-usage message estimate is below the threshold`）。对应 `estimateContextTokens` 的零 usage 回退路径。 |
| `suite/regressions/8989-fork-compaction-label-boundary.test.ts` | 24 | fork 删除压缩边界上的 label 条目时仍保留压缩上下文：`firstKeptEntryId` 重新指向 `keptId`，`buildSessionContext().messages` 为 compactionSummary + user `kept` + user `after`。 |
| `suite/regressions/9789-context-handler-system-messages.test.ts` | 226 | context handler 与 system 消息：handler 从压缩摘要处切片时保留 prompt/tools；不动会话时保留中途 system 消息位置；原地改 `event.messages` 而无返回值也生效；handler 在重放头部之后添加的 system 消息保留。`context_with_system` handler 在恢复的 transcript 上运行并逐字发送其输出；丢弃前导 system 消息的 handler 仍尊重其输出。 |
| `suite/regressions/copilot-request-size-overflow.test.ts` | 102 | Copilot 414/413 请求体过大：对图片密集历史执行压缩后重试（`compacts an image-heavy history and retries after the real 413`）。本单元提供压缩能力，触发/重试在 agent-session。 |
| `session-context-edit.test.ts` | 364 | `appendContextEdit`：`null` replacement 只在模型投影中省略目标（原始 transcript 保留）；replacement 只改内容；多次编辑同一目标；投影与 `buildSessionContext` 的一致形状。是 2.20.19 投影/恢复规则的直接验收。 |
| `session-manager/build-context.test.ts` | 305 | `buildSessionContext` 的完整矩阵（空 entries、单条 user、简单会话、thinking level 变化、model 变化、压缩边界等），本单元 `prepareCompaction`/`tokensBefore` 依赖其语义。 |
| `session-manager/load-entries.test.ts` | 182 | 内存 SessionManager 预载 entries：逐字采用、保留 leaf、id 不碰撞、重建分支结构、重建 labels。 |
| `session-manager/tree-traversal.test.ts` | 579 | `getBranch`/`getTree`/`getChildren`/祖先查找等树遍历，直接决定 2.21.2 的 `commonAncestorId` 与 2.20 的路径切点。 |
| `agent-session-tree-navigation.test.ts` | 323 | 分支导航：user 消息导航把文本放进编辑器；非 user 不填编辑器；`summarize=true` 生成分支摘要；摘要挂到正确父节点（嵌套 user / 选中 assistant）；摘要过程中中止。 |

### 7.3 HTML 导出

| 文件 | 行数 | 覆盖行为 |
|---|---|---|
| `export-html-cjk.test.ts` | 66 | CJK 标点旁的强调渲染（marked-cjk-friendly）；非 CJK 保持 CommonMark flanking 规则。 |
| `export-html-cost.test.ts` | 52 | 使用记录的 totals，旧会话回退到分量求和。 |
| `export-html-deep-tree.test.ts` | 93 | 20000 层单链 `buildTree`/`flattenTree`/`findNewestLeaf`/`getPath` 不爆栈（在 node:vm 中执行导出 HTML 的 viewer 脚本）。 |
| `export-html-skill-block.test.ts` | 40 | `parseSkillBlock` 去掉 skill XML 包裹；skill 调用与用户消息渲染为两个兄弟块；skill 内容按 markdown；侧栏树显示 skill 名与用户消息。 |
| `export-html-system-messages.test.ts` | 83 | 系统消息文本与模型收到的一致；分支上第一个 system 条目渲染为 prompt、后续为 update；压缩快照视为更早的系统状态；系统条目出现在 transcript。 |
| `export-html-whitespace.test.ts` | 42 | CSS 中 `.output-preview > div:not(.expand-hint), .output-full > div:not(.expand-hint) { ... white-space: pre-wrap; }`、`.ansi-line { ... white-space: pre; }`，且不得给 `.output-preview, .output-full` 整体加 `pre-wrap`；`ansiLinesToHtml` 行间无空白；自定义工具结果 HTML 去掉 TUI 首尾空行（期望 `'<div class="ansi-line"><span style="color:#800000">one</span></div><div class="ansi-line">two</div>'`）。 |
| `export-html-xss.test.ts` | 67 | marked 的 link/image 渲染器改用 scheme 白名单；URL 中的 C0 控制字符被剥离；href 属性转义；image mimeType 与 data 属性转义；条目 id 进属性前转义；树元数据转义；模型名转义。 |
| `export-jsonl-share.test.ts` | 122 | JSONL 分享导出添加展示数据但不改变会话 id 与链接。 |
| `theme-export.test.ts` | 127 | `getThemeExportColors`：以与颜色相同的语法解析 export 变量引用；OKHSL 转 hex（CSS 不支持）；递归 vars 与 256 色转 hex。 |

### 7.4 跨单元但直接影响本单元行为的回归测试

| 文件 | 覆盖行为 |
|---|---|
| `suite/regressions/3302-find-path-glob.test.ts` | find 的路径 glob 与 `--full-path` 前缀逻辑。 |
| `suite/regressions/3303-find-nested-gitignore.test.ts` | 嵌套 .gitignore 与 `--no-require-git` 判定（源码注释引用 #5960）。 |
| `suite/regressions/6104-find-root-relativization.test.ts` | `relativizeFindResultPath` 的根目录/尾分隔符。 |
| `suite/regressions/5303-bash-output-truncation.test.ts` | 退出后仍到达的输出不被丢弃（`waitForChildProcess` 空闲计时）。 |
| `suite/regressions/5208-late-bash-output.test.ts` | 晚期 bash 输出。 |
| `suite/regressions/8611-thinking-toggle-pending-bash-output.test.ts` | 切换 thinking 可见性时保留运行中 bash 的部分输出。 |
| `suite/regressions/9068-user-bash-fail-closed.test.ts` | `user_bash` 扩展返回空结果时 fail-closed（不执行 bash），RPC 返回错误并发 extension_error。 |
| `suite/regressions/11227-recovery-omits-tool-results.test.ts` | 恢复省略与 tool result。 |
| `codemode-renderer.test.ts` / `tool-search.test.ts` | codemode 对 structuredContent 与工具描述的消费（跨单元）。 |
| `suite/agent-session-tool-result-images.test.ts` | 图像设置与模型 profile 传入工具结果归一化。 |
| `nested-tool-calls.test.ts` | 嵌套调用记录（`nestedCalls.calls` 的字段名与裁剪上限），压缩的文件操作跟踪依赖它。 |
| `agent-session-stats.test.ts`（316 行） | `getSessionStats`：当前上下文用量与 token 总计并列暴露；压缩后立即报告 unknown 当前上下文；用压缩后 usage 而非保留的 stale usage。 |
| `cache-stats.test.ts`（177 行） | `computeCacheWaste`：跨轮累计未命中 token 与成本；健康会话计 0；**压缩重置后的那一轮跳过**。 |
| `output-pad.test.ts`（94 行） | 渲染器 `outputPad` 语义（HTML 预渲染固定 `outputPad: 1`，见 2.24）。 |
| `collapsible-message-components.test.ts`（83 行） | 可折叠消息组件（thinking 折叠/展开），HTML 导出的 thinking 块与之对应。 |
| `suite/regressions/4167-thinking-toggle-pending-tool-render.test.ts`（186 行） | `InteractiveMode.renderSessionEntries`：切换 thinking 时不丢失进行中的 tool 渲染。 |
| `suite/regressions/8537-custom-message-tool-result-ordering.test.ts`（151 行） | 工具执行期间注入的 custom message 追加在本轮 tool results **之后**；会话条目与 message 事件顺序一致；LLM 历史里每个 tool result 紧随其 tool call。 |
| `session-manager/tree-traversal.test.ts` / `build-context.test.ts` / `load-entries.test.ts` | 见 7.2 末（树/上下文基座）。 |
| `agent-session-tree-navigation.test.ts` | 见 7.2 末（分支摘要挂载点）。 |

### 7.5 建议的 Rust 测试组织

- `truncate`：表驱动，覆盖 `totalLines/totalBytes/outputLines/outputBytes/truncatedBy/lastLinePartial/
  firstLineExceedsLimit` 全字段；含尾换行、空串、单行超字节、emoji、CRLF。
- `edit_diff`：直接用 TS 测试的输入/输出对做快照（`tools.test.ts` 的 12+6 例），
  另加 `diff` crate 输出格式的黄金文件（`patch` 与 `diff` 两个字段）。
- `bash`：用假 `BashOperations`（脚本化 data 块 + 退出码）覆盖节流、截断脚注、错误文本；
  用真实 `/bin/sh` 覆盖 stdin 传输与超时。
- `compaction`：纯函数表驱动（`estimateTokens`/`findCutPoint`/`prepareCompaction`），
  以及假 `streamFn` 覆盖 prompt 文本（逐字断言发给模型的 `messages[0].content[0].text`）。
- `export_html`：与 TS 输出做**字节级 golden**：固定会话 JSONL + 固定主题 →
  期望 HTML 文件 sha256（模板/vendor 已固定，数据由我们自己序列化）。
- `ansi_to_html`：与 TS 输出表驱动比对（含 256 色、RGB、多 span 相邻）。

---

## 8. Rust 模块布局与最难复现的部分

### 8.1 建议 crate 与模块

crate 名：`kiana-tools`（`ARCHITECTURE.md` §3.7 已拒绝“单 crate `kiana-coding-agent`”，本单元的模块划分被采纳，但三个子系统各自成 crate：`kiana-tools`（L8）、`kiana-compaction`（L5）、`kiana-export`（L8））。
下面这棵 `src/` 树是**模块划分**，路径前缀分别为 `crates/kiana-tools/src/`、`crates/kiana-compaction/src/`、`crates/kiana-export/src/`：

```
src/
  lib.rs
  tools/
    mod.rs                 // ToolName / allToolNames / create* / ToolsOptions / ToolDefinition 组装
    definition.rs          // ToolDefinition, AgentTool, wrapToolDefinition, createToolDefinitionFromAgentTool
    truncate.rs            // DEFAULT_MAX_*, TruncationResult, truncate_head/tail/line/middle, format_size
    path_utils.rs          // expand_path, resolve_to_cwd, resolve_read_path[_async]
    file_mutation_queue.rs // FileMutationQueue
    output_accumulator.rs  // OutputAccumulator + Utf8StreamDecoder
    read.rs
    write.rs
    edit.rs
    edit_diff.rs           // fuzzy matching, apply_edits, generate_diff_string, generate_unified_patch
    bash.rs                // create_shell_tool_definition, create_local_shell_operations, BashOperations trait
    powershell.rs
    grep.rs
    find.rs
    ls.rs
    render_utils.rs        // shorten_path, link_path, get_text_output, format_tool_call_with_args, timer
    renderers/
      mod.rs               // create_all_tool_renderers, with_builtin_renderers
      bash.rs read.rs write.rs edit.rs grep.rs find.rs ls.rs
  compaction/
    mod.rs
    utils.rs               // FileOperations, extract/compute/format, serialize_conversation, SYSTEM_PROMPT
    compaction.rs          // estimate_tokens, cut points, prepare_compaction, compact, prompts
    branch_summarization.rs
  export_html/
    mod.rs                 // generate_html, export_session_to_html, export_from_file, theme vars
    ansi_to_html.rs
    tool_renderer.rs
    assets/
      template.html        // include_str!
      template.css         // include_str!
      template.js          // include_str!
      marked.min.js        // include_str!
      marked-cjk-friendly.min.js
      highlight.min.js
  strings.rs               // 第 3 节所有逐字常量
  jsstr.rs                 // UTF-16 长度/切片、JS 数字格式化、JSON 保序写
  process/
    shell.rs               // getShellConfig/getPowerShellConfig/getShellEnv/sanitize/killProcessTree
    child.rs               // wait_for_child_process
    output_files.rs        // 0600 + wx 临时文件
    tools_manager.rs       // ensure_tool/get_latest_version/download
```

核心类型建议：

```rust
pub struct ToolDefinition {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    pub prompt_snippet: Option<&'static str>,
    pub prompt_guidelines: Option<Vec<&'static str>>,
    pub parameters: serde_json::Value,          // 手写 schema
    pub output_schema: Option<serde_json::Value>,
    pub constrained_sampling: Option<ConstrainedSampling>,
    pub render_shell: Option<RenderShell>,
    pub prepare_arguments: Option<Box<dyn Fn(&mut serde_json::Value) + Send + Sync>>,
    pub execution_mode: Option<ExecutionMode>,
    pub execute: Box<dyn ExecuteFn>,
    pub render_call: Option<Box<dyn RenderCallFn>>,
    pub render_result: Option<Box<dyn RenderResultFn>>,
}

pub type ExecuteFn = dyn Fn(ToolCallId, serde_json::Value, Option<CancellationToken>,
                             Option<UpdateFn>, Option<ExtensionContext>)
                            -> BoxFuture<'static, Result<ToolResult, ToolError>> + Send + Sync;
```

`ToolResult { content: Vec<ContentBlock>, details: Option<serde_json::Value>,
structured_content: Option<serde_json::Value>, is_error: bool }`。

`BashOperations` trait：

```rust
#[async_trait]
pub trait BashOperations: Send + Sync {
    async fn exec(&self, command: &str, cwd: &Path,
                  opts: ExecOptions<'_>) -> Result<ExecResult, ExecError>;
}
pub struct ExecOptions<'a> {
    pub on_data: &'a (dyn Fn(&[u8]) + Send + Sync),
    pub signal: Option<CancellationToken>,
    pub timeout_secs: Option<f64>,
    pub env: Option<EnvMap>,
}
pub enum ExecError { Aborted, Timeout { secs: String }, Spawn(io::Error), Other(anyhow::Error) }
```

渲染器 trait 需与 kiana-tui 的 `Component` 对接：`fn render(&mut self, width: u16) -> Vec<String>`。
TUI 组件树（`Text`/`Container`/`Box`/`Spacer`/`VisualLinePreview`）由 `tui-core`/`tui-components`
单元提供；本单元只依赖其 trait。

### 8.2 最难复现的部分（按风险排序）

1. **`diff` 库的两种输出格式**（`generateDiffString` 与 `generateUnifiedPatch`）。
   - `createTwoFilesPatch` 的精确字节：首行 `"=".repeat(67)`、`--- {path}`、`+++ {path}`、
     `@@ -{os},{ol} +{ns},{nl} @@`（`ol/nl` 为 1 时省略 `,1`? —— JS `diff` 的 `formatRange` 在长度为 1 时
     只写起始行号；必须实测确认）、hunk 上下文 4 行、多条 hunk 之间无额外空行。
   - `diffLines` 的变更分组：`diff` 库对「先删除后新增」的输出顺序与 hunk 切分（相邻变更是否合并）
     有自己的启发式（`diffLines` 内部用 `diff` 的 Myers 实现 + `ignoreWhitespace` false）。
     `similar` 的 Myers 实现 tie-break 可能不同 → 同一对输入可能产生不同的 hunk 边界。
     **验收手段**：把 TS 测试里的每对 (old,new) 跑一遍，把 `diff` 与 `patch` 存为黄金文件，
     Rust 实现必须逐字节匹配；若不能，必须在规格里明确「允许 hunk 边界差异」并调整下游（但
     `edit` 的 `details.diff` 会进会话文件与 UI，差异会破坏字节级一致性）。建议：**内嵌一个
     与 `diff@8` 语义一致的 Myers + 行分组移植**（约 300 行），而不是把 `similar` 的输出硬改。
   - `renderIntraLineDiff` 用 `diffWords`（TUI 用，非 HTML 导出）：词级切分规则（空白归属）需一致。

2. **UTF-16 语义**（`truncateLine`、`truncateForSummary`、`estimateTokens`、`formatToolCallWithArgs`）。
   风险点：emoji/CJK 扩展 B 字符会让「字符预算」与 Rust `chars()` 不同（差 2 倍）。
   建议实现 `utf16_len`、`utf16_truncate(s, units)`；`utf16_truncate` 在代理对中间时
   **返回整个字符**（比 JS 的孤立代理更安全），并记录为已知偏差；
   但 `truncateLine` 的 `line.slice(0, maxChars)` 若切在代理对中间，JS 的输出是孤立高代理
   （JSON 里 `\ud83d`），Rust 会多保留一个字符。测试里用纯 BMP 用例即可全覆盖，emoji 用例
   接受「多保留一个字符」的差异（或在 `strings.rs` 中写明）。

3. **`JSON.stringify` 兼容的序列化**：所有进入会话文件/HTML base64/摘要 prompt 的对象。
   - 整数 vs 浮点：`wall_time_seconds` 可能是 `0`（不是 `0.0`）——Serde 的 `f64` 会输出 `0.0`。
     **必须**用「若 `v.fract() == 0.0` 且 `|v| < 1e21` 则输出整数字面量」的包装。
   - `cost.total` 等价格是小数，JS 会输出如 `0.00123`；`ryu` 的最短表示与 JS 的
     `Number.prototype.toString` 在绝大多数值上一致，但指数阈值不同（JS 在 `>=1e21` 或 `<1e-6`
     用指数；`ryu` 在 `>=1e16` 左右就用指数）。必须按 ECMAScript 算法重写。
   - `undefined` 字段省略（`Option::None` + `skip_serializing_if`）。
   - 键序：`preserve_order` + 手写 struct 字段顺序。

4. **HTML 导出的字节级一致**：模板 3 个文件 + 3 个 vendor 必须逐字节内嵌；
   `generateThemeVars` 输出依赖主题 JSON 的 token 顺序（`getResolvedThemeColors` 的 `Object.entries`
   顺序 = 主题文件顺序）与 `colorToHex` 的格式（`#rrggbb` 小写? 需与 theme 单元对齐）。
   风险：主题解析由 theme 单元负责，其 `colorToHex` 输出格式必须与本单元的 `parseColor`/
   `deriveExportColors` 假设一致（`#rrggbb` 或 `rgb(r, g, b)`）。
   导出结果的 sha256 黄金测试依赖主题固定（`initTheme("dark")`）。

5. **压缩切点算法的等价性**：`findProjectedCutPoint` 的 recovery-omission-suffix 规则、
   `slice` 边界（`entries.slice(cutIndex+1, endIndex)` 不含 `endIndex`）、
   `cutPoints.find(c => c >= i) ?? last` 的回退、元数据回扫。任何 off-by-one 都会改变
   `firstKeptEntryId` 与摘要内容。必须用 `compaction.test.ts` 的全部用例做黄金断言
   （`firstKeptEntryId`、`tokensBefore`、`messagesToSummarize` 长度、`isSplitTurn`）。

6. **prompt 文本逐字**：4 个压缩/分支 prompt + system prompt。它们含 Markdown、`##`、`- [x]`、
   空行数量，容易在转写时出错。建议用 `include_str!` 从 `.md`/`.txt` 资源文件加载（避免转义问题），
   并用测试断言 `contains` 关键行与总字节数。

7. **`localeCompare`（ls）**：Rust 无内置 ICU 排序。建议 `icu_collator`，并在
   `Cargo.toml` 里放为可选 feature；退化实现 `to_lowercase()` 比较 + 原始串 tie-break 在
   纯 ASCII 用例上等价，但非 ASCII（`ä`、`ß`、数字）不同。测试 `ls` 的排序用例需明确用 ASCII。

8. **流式 UTF-8 解码（`OutputAccumulator`）**：关键差异**不是**替换分组，而是**状态保留**：
   `[0xF0, 0x9F, 0x92]`（截断的 emoji 尾）在 `{stream:true}` 下必须原样保留 3 字节等下一块，
   而 `from_utf8_lossy` 会立刻替换成 U+FFFD；下一块补齐后前者产出 `😀`，后者永久损坏。
   非流式场景（`read` 的 `buffer.toString("utf-8")`）两者在已知用例上一致，但 WHATWG 的
   maximal-subpart 分组仍应作为黄金测试（7 个用例见 2.5 末）。必须实现「保留不完整尾」的
   增量解码器（约 80 行）或使用 `encoding_rs::Decoder`。

9. **子进程输出的**「exit 后 100ms 空闲」**语义**：必须用「每个 chunk 重置计时器」，
   而不是固定从 exit 计时（#5303 的回归）。tokio 里可用一个 `Notify`/`watch` 通道 +
   `tokio::select!` 与 `sleep` 循环；或用 `Arc<Mutex<Instant>>` + 后台任务。
   同时要保证 `close`（stdout/stderr 都 EOF）优先于计时器。

10. **进程组与信号**：`detached:true` + `kill(-pid)`；macOS 先 `SIGSTOP`。Rust 需
    `process_group(0)`（`CommandExt`），Windows 用 `taskkill /F /T /PID`。若用 `tokio::process`，
    注意 `kill_on_drop` 与 `Child::kill` 只杀单进程——必须走 `killpg`。

11. **`withFileMutationQueue` 的「注册串行」**：key 计算（含 `realpath`）本身必须在锁内完成，
    否则并发调用可能得到不同 key（realpath 竞态）。tokio Mutex FIFO 公平性需确认
    （tokio 文档保证 FIFO）。

12. **`grep`/`find` 的 fd/rg 外部依赖**：Rust 必须调用外部 `rg`/`fd`（与 TS 一致），
    不能换成 `ignore`/`grep` crate——否则 gitignore 语义、JSON 事件、`--max-results`、
    `--full-path` 匹配规则都会漂移，进而输出文本不同。

13. **`find` 的 fd `--full-path` 模式**：`**/` 前缀与 Windows 的 `[/\\]` 替换是硬编码规则，
    必须照抄；`--no-require-git` 的「向上找 .git」循环（含根目录 break）。

14. **HTML 导出中 `renderedTools` 的键序与「仅渲染非模板工具」规则**：
    `TEMPLATE_RENDERED_TOOLS = {bash, read, write, edit, ls}`；`grep`/`find`/`powershell` 与扩展工具
    走预渲染路径（`grep`/`find` 的渲染器存在 → 产出 `callHtml`/`resultHtml*`）。
    若 Rust 侧误把 grep/find 加进模板集合，`renderedTools` 会缺失对应键，浏览器端 fallback 到
    «工具名 + JSON» —— 视觉与 TS 不同。反之若把 powershell 加进去，会重复渲染（模板无 powershell 分支，
    它本来就走 default → 无影响，但会改变 `renderedTools` 的键集与导出数据 base64 字节）。
    另注意 `renderCall` 在**结果尚未出现时**也会被调用（`preRenderCustomTools` 先遍历 assistant 的
    toolCall，再遍历 toolResult），所以 `renderCall` 的 `isPartial:true` 会走渲染器的 partial 分支。

15. **`formatSize` 的 `toFixed(1)`**：`50*1024 = 51200` → `50.0KB`；`51200.0` 的 `toFixed(1)`
    是 `"50.0"`。Rust `format!("{:.1}", 50.0)` = `"50.0"`。此处一致，但 `1023` → `"1023B"`（无小数）；
    `1024*1024 - 1` → `"1024.0KB"`（`(1048575/1024).toFixed(1)` = `"1024.0"`）——注意此处
    结果看起来像 1MB 但仍是 KB 分支，Rust 必须保持同一分支判断顺序。
    **但中点值不一致**：JS `toFixed(1)` 在中点向上（`256B → "0.3KB"`、`1280B → "1.3KB"`），
    Rust `{:.1}` 取偶（`"0.2KB"`、`"1.2KB"`）。必须按 2.1.4 的自实现方案处理，并把这 4 个
    字节值（256/768/1280/2304）作为必测用例。

16. **`truncateHead` 的 `outputBytes` 重算 vs 累加**：累加值包含「每行 +1 换行」，
    但 `join("\n")` 后字节数少 1（首行无换行）。因此 `outputBytes` 可能比累加值小 1。
    `truncatedBy` 的判定用的是**累加值**（`outputBytesCount <= maxBytes`），`outputBytes` 是**重算值**。
    这个细节影响 `[Showing lines ... (50.0KB limit)]` 的条件（`truncatedBy`）。

### 8.3 `createTwoFilesPatch` 输出格式的实测规格（必须验证）

给定 `path="foo.ts"`、`old="a\nb\nc\n"`、`new="a\nB\nc\n"`、`context=4`，
`diff@8` 的输出（`FILE_HEADERS_ONLY`）为：

```
===================================================================
--- foo.ts
+++ foo.ts
@@ -1,3 +1,3 @@
 a
-b
+B
 c
```

要点：
- 分隔线是 67 个 `=`（`===================================================================`）；
- `---`/`+++` 后是单空格 + 路径，**无制表符、无时间戳**；
- `@@ -1,3 +1,3 @@`：行数 1 时 `diff` 会输出 `@@ -5 +5 @@`（无 `,1`）——需实测确认；
- 无尾随换行（`split("\n")` 语义下最后一个 hunk 行后无 `\n`）；
- 内容相同 → 空字符串；
- 文件末尾无换行时，`diff` 会在最后一行前加 `\ No newline at end of file`? —— `createTwoFilesPatch`
  的 `noNewline` 选项默认不开启该标记；需实测。
Rust 侧建议：**不要**依赖 `similar::udiff`，而是自己实现「按行 LCS/Myers → hunks → 上述格式」，
并用黄金文件锁定。

### 8.4 验收清单（Definition of Done）

- [ ] 8 个工具的 schema（含描述、required、additionalProperties）与 TS 逐字节一致。
- [ ] 8 个工具的 description/promptSnippet/promptGuidelines 逐字一致。
- [ ] 所有 `Error.message` 文本逐字一致（第 3.4 节清单全覆盖）。
- [ ] `truncateHead`/`truncateTail`/`truncateLine`/`truncateMiddle`/`formatSize` 全字段表驱动通过；
      `formatSize` 必须包含 JS `toFixed(1)` 中点用例（`256B→"0.3KB"`、`768B→"0.8KB"`、
      `1280B→"1.3KB"`、`2304B→"2.3KB"`），`formatToolDuration` 包含 `1250ms→"1.3s"`。
- [ ] `formatSize` 与 `formatToolDuration` 共用同一个「JS 风格 toFixed(1)」实现（见 2.1.4）。
- [ ] `edit` 的 18 个 TS 用例（fuzzy 12 + CRLF/BOM 6）与 `patch`/`diff` 黄金文件一致。
- [ ] bash 的分段输出、节流、脚注、超时/中止消息、structuredContent 全部通过。
- [ ] `read` 的图片/文本分支与 macOS 路径变体全部通过。
- [ ] `grep`/`find`/`ls` 的 notices 顺序与文本一致。
- [ ] 压缩的 4 个 prompt 与 `SUMMARIZATION_SYSTEM_PROMPT` 逐字节一致。
- [ ] `prepareCompaction` 在所有 `compaction.test.ts` 用例上产出相同 `firstKeptEntryId`/`tokensBefore`/
      `messagesToSummarize.len()`/`isSplitTurn`。
- [ ] `compact` 在 split-turn 下的合并文本 `{history}\n\n---\n\n**Turn Context (split turn):**\n\n{prefix}` 一致。
- [ ] `generateBranchSummary` 的 preamble/prompt/文件列表追加顺序一致。
- [ ] `exportSessionToHtml` 对固定输入产出 sha256 与 TS 相同的 HTML。
- [ ] `ansiToHtml`/`ansiLinesToHtml` 与 TS 逐字节一致（含 256 色/RGB/多 span）。
- [ ] `tool-renderer` 的宽度 100、`isPartial:false`、`showImages:false`、`outputPad:1`、
      首尾空行裁剪一致。
- [ ] 所有 `unwrap()` 在用户输入路径上被移除（返回 `Result`），但**不得**改变错误文本/顺序。
- [ ] `KIANA_OFFLINE`/`KIANA_SESSION_*` 环境变量语义一致。
- [ ] `KIANA_PACKAGE_DIR`/`KIANA_SHARE_VIEWER_URL`/`ENV_AGENT_DIR`/`ENV_SESSION_DIR`/`KIANA_EXPERIMENTAL`
      的取值与优先级一致（见 4.3 与 2.28.8）。
- [ ] 外部 `fd`/`rg` 的调用参数与解析规则一致。
- [ ] HTML 导出资产（`template.html/js/css` + 3 个 vendor）的 SHA-256 与 2.25/2.26/2.27/2.29 完全一致，
      且 6 处 `{{...}}` 单次替换顺序与 `generateHtml` 相同。
- [ ] `template.js` 的 `getLanguageFromPath` 表为 28 键，`formatToolCall` 为方括号格式（见 2.27.4）。
- [ ] `processImage` 的两条 `[Image omitted: ...]` 文本、`[Image converted from X to Y.]`、
      尺寸提示的 `toFixed(2)`（`1.125 → "1.13"`）逐字一致；`detectSupportedImageMimeType` 对
      JPEG-LS(0xF7)、动画 PNG(acTL)、非法 BMP 均返回 null。
- [ ] `read` 之外的路径解析**不**做 macOS 变体回退（`write`/`edit`/`ls`/`grep`/`find` 用 `resolveToCwd`）。
- [ ] `truncateMiddle` 的 `…N chars truncated…`（chars，不是 tokens）与 MCP 侧包装文本一致。
- [ ] `executeBash` 的跨 chunk ANSI 切分与末尾不完整字符 flush 与本单元 bash 工具的差异被保留：
      `executeBash` 每块都 `stripAnsi`；**bash 工具不 strip**（`OutputAccumulator.append` 直接收原始字节，
      ANSI 一直保留到 `getTextOutput`/渲染器与 `ansiToHtml`）。
      因此工具结果文本里的 ANSI 在 TUI/HTML 里变成颜色，而 `executeBash` 输出里没有。

### 8.5 未决问题（需要在实现时与 TS 实测确认）

1. `diff@8` 的 `@@` 头在行数为 1 时的精确格式（`,1` 是否省略）。
2. `createTwoFilesPatch` 对「文件末尾无换行」的处理（是否输出 `\ No newline at end of file`）。
3. `diffLines` 在「删除与新增块相邻」时的分组（一个 hunk 还是两个；`renderDiff` 的行内高亮依赖
   连续 `-`/`+` 各一行）。
4. `theme` 的 `colorToHex` 输出大小写（影响 `generateThemeVars` 的字节）。
5. `getThemeExportColors` 在 `SYSTEM_THEME_NAME`（默认主题）下返回 `{}`，此时
   `deriveExportColors` 使用 `colors.userMessageBg`——系统主题的该 token 值需与 theme 单元确认。
6. `ls` 的 `localeCompare` 在 Node 22（无 ICU 全量包或 `small-icu`）下的实际 locale 行为。
7. `TextDecoder` 对「超长非法序列」的替换分组（影响 `totalDecodedBytes`）。已用 7 个短用例确认
   `Buffer.toString("utf8")` 与 Python `errors="replace"` 一致；**流式**下的跨块分组（一个非法序列
   被 chunk 边界切开）仍未实测，建议用 2.5 末的用例在各切分点做表驱动测试。
8. `waitForChildProcess` 里 `close` 与 idle timer 的竞态在 Rust 侧的等价实现（是否可能
   在 EOF 后仍等待 100ms —— TS 里 `stdoutEnded && stderrEnded` 会立即 finalize，不等计时器）。

---

## 9. Audit additions（审计补遗，2026-10-26）

本节记录对 `ca-tools-compaction-export` 的完整性审计结果：逐文件重读源码/测试后发现的**遗漏**与
**错误陈述**。正文中能就地修正的错误已直接改在对应小节（下表「位置」列指出落点），
新增内容也已在正文补齐；本节同时给出集中索引，便于评审时逐条核对。

### 9.1 已就地修正的错误/不精确陈述（wrong statements, fixed in place）

| # | 位置 | 原陈述（错误） | 事实（源码依据） |
|---|---|---|---|
| 1 | 2.1.4 / 8.2 #7 / 8.2 #15 | `formatSize` 的 `toFixed(1)` 与 Rust `format!("{:.1}")` 「在二进制半值一致」 | JS `toFixed` 在中点**向上**（away from zero），Rust `{:.1}` **取偶**。实测：`256/1024=0.25` → JS `"0.3"`、Rust `"0.2"`；`1280/1024=1.25` → `"1.3"` vs `"1.2"`；`2304/1024=2.25` → `"2.3"` vs `"2.2"`（`768/1024=0.75` 两者都是 `"0.8"`）。必须自实现 JS 风格格式化，并新增 `formatToolDuration` 的 `1250ms → "1.3s"` 用例 |
| 2 | 2.16 `formatToolDuration` | 只写了 `toFixed(1)` 规则，未提示同一中点问题 | 同上；`render-utils.ts:120` 确实用 `seconds.toFixed(1)` |
| 3 | 2.25 `template.html`「逐字结构」 | 代码块是**示意性重排**（`<!DOCTYPE html><html ...><head>` 挤在一行、`<div id="image-modal">` 与 `<img>` 合一行、结尾 `</body></html>` 合一行），与真实文件不符 | 真实文件 56 行 / **2271 字节**，`<!DOCTYPE html>`、`<html lang="en">`、`<head>`、`</head>`、`<body>`、`</body>`、`</html>` 各占一行；`image-modal` 与 `modal-image` 分两行。已替换为逐字节内容 |
| 4 | 2.27.1 URL 参数 | `document.getElementById('kiana-url-params')?.textContent`；参数名 `target` | 实际为 `document.querySelector('meta[name="kiana-url-params"]')` 的 **`.content`**；参数名是 **`targetId`**；`leafId` 用 `urlLeafId || defaultLeafId` |
| 5 | 2.27.2 `buildTreePrefix` | `buildTreePrefix(node, indent, showConnector, isLast, gutters, isVirtualRootChild)`，3 字符单元为 `"│ "`/`"├ "`/`"└ "`/`"─ "` | 实际签名 `buildTreePrefix(flatNode)`；连接符是 `└`/`├` + `─` + 空格三字符；gutter 仅 `│` 一字符 + 两空格；另有 `displayIndent = multipleRoots ? max(0, indent-1) : indent` 与 `connectorPosition = displayIndent - 1` |
| 6 | 2.27.2 `flattenTree` | 未写子节点栈元素的 `justBranched`/`showConnector`/`isVirtualRootChild` 取值 | 子节点压入 `[child, childIndent, multipleChildren, multipleChildren, childIsLast, childGutters, false]`：`justBranched` 与 `showConnector` 都取**父节点的 `multipleChildren`**（不是继承），`isVirtualRootChild` 恒 `false` |
| 7 | 2.27.4 `getLanguageFromPath` | 「**仅 22 个键**」 | 实际 **28 个键**（`ts, tsx, js, jsx, py, rb, rs, go, java, c, cpp, h, hpp, cs, php, sh, bash, zsh, sql, html, css, scss, json, yaml, yml, xml, md, dockerfile`） |
| 8 | 2.27.4 `formatToolCall` | `bash` → `$ command`；`read` → `read path:offset-limit`；`grep` → `grep /pattern/ in path` 等（无方括号） | 实际全部是**方括号**格式：`[read: path:range]`、`[write: path]`、`[edit: path]`、`[bash: cmd]`（`\n`/`\t`→空格、trim、截 50，截断标记按**原始长度** >50）、`[grep: /pattern/ in path]`、`[find: pattern in path]`、`[ls: path]`、其他 `[name: JSON.slice(0,40)...]`；`path`/`pattern` 缺省为 `.` |
| 9 | 2.27.6 复制反馈 | `copyToClipboard` 的 `✓` 反馈「1s」 | 实际 `setTimeout(..., 1500)`，即 **1500 ms** |
| 10 | 2.27.5 `formatTimestamp` | 「`toLocaleTimeString()`（`HH:MM:SS` 本地时区）」 | 实际带 options `{hour:'2-digit',minute:'2-digit',second:'2-digit'}`；输出随默认 locale 变化（`en-US` → **12 小时制 + AM/PM**，如 `09:05:03 PM`；`zh-CN` → 24 小时制），不是固定 `HH:MM:SS` |
| 11 | 2.27.6 深链 | 初始化「否则滚动到最后一条」 | 实际：有 `leafId` 且 URL 有有效 `targetId` → `navigateTo(leafId,'target',targetId)`；有 `leafId` → `navigateTo(leafId,'none')`；**只有 `leafId` 为空**时才对最后一条 entry 调 `navigateTo(...,'none')`；`'none'` **不滚动** |
| 12 | 2.28.8 `getPackageDir` | 未提 `KIANA_PACKAGE_DIR` 覆盖 | `getPackageDir()` 第一步就是 `process.env.KIANA_PACKAGE_DIR`（经 `normalizePath`） |
| 13 | 2.28.8 `APP_NAME` | 说由 `kianaConfigName` 覆盖 | 字段名是 `kianaConfig.name`（另有 `APP_TITLE` 在未覆盖时为 `"π"`） |
| 14 | 2.30 文档映射 | `getCompactionSettings` 交叉引用写成「见 5.6」 | 5.6 是 `theme.ts`；`getCompactionSettings` 在 **5.10**（已改） |
| 15 | 7.2 测试行数 | `branch-summarization.test.ts` 46 行；`suite/agent-session-compaction-model-overrides.test.ts` 200+ 行；`6768-copilot-compaction-base-url` 91 行；`6324-branch-summary-ambient-auth` 83 行 | 实际 114 / 175 / 114 / 62 行 |
| 16 | 1. 文件清单 | `template.html` 只写行数 56，无字节数 | 补 2271 字节；vendor 三个文件补精确字节数 44640 / 4754 / 121727 |
| 17 | 2.27.4 read/write/edit/ls DOM 描述 | read 未写 `file_path ?? path` 优先级与行范围的 `<span>` 紧贴细节；write 的 `(N lines)` 条件写成「超过 10 行」但没写「content 非空字符串」；edit 的 fallback 没写 `output.trim()` 才渲染；ls 没写 `dirPath === null` 分支 | 已按 `template.js` 逐行补齐（含 `<span>` 前空格、`String(limit)` 转义、`trim()` 条件） |
| 18 | 2.27.4 非模板工具 fallback | 只写「有 renderedTools 则插入，否则 JSON fallback」 | 实际有三层分支：`callHtml` 优先/否则仅工具名；结果按 `collapsed !== expanded` 决定可展开、否则直接 `expanded`、否则文本降级；完全无预渲染才走 `JSON.stringify(args, null, 2)`。预渲染 HTML **不转义** |
| 19 | 2.27.4 `shortenPath` | 只写「`/Users/` 与 `/home/` 前缀缩短为 `~`」 | 补：模板侧与 `render-utils.shortenPath`（用 `os.homedir()`）**不是同一实现**，`/root/...` 在模板侧不缩短 |
| 20 | 2.29 vendor | 只给近似「K」大小 | 补精确字节数与文件头版本（marked v18.0.11、highlight.js v11.9.0、marked-cjk-friendly v0.1.2） |
| 21 | 2.5 末 / 6.3 #10 / 8.2 #8 | 「`from_utf8_lossy` 每个非法序列一个 U+FFFD，Node 每字节一个」；「两者分组规则不同」 | 实测 Node、Python `errors="replace"`、Rust `from_utf8_lossy` 在 `[E0,80,80]`、`[F0,80,80,80]`、`[C0,80]`、`[ED,A0,80]`、`[F5,80,80,80]`、`[E2,82]`、`[80,80,80]` 上**结果相同**（WHATWG maximal subpart）。真正的差异是**流式状态保留**：`from_utf8_lossy` 没有 stream 模式，会把被 chunk 切开的序列立刻替换。已改写三处并给出黄金用例 |
| 22 | 2.4 `OutputAccumulator` | `totalDecodedBytes` 未定义计数口径 | 它是**解码后文本按 UTF-8 重新计的字节数**（U+FFFD 3 字节，故可能大于 `totalRawBytes`）；`totalRawBytes` 才是原始输入字节数。已补计数语义段 |
| 23 | 2.9.8 步骤 4 | 「四舍五入到偶数?」的自我怀疑 | `Math.round` 对正数 .5 向 +∞；无「取偶」。已改为确定表述 |
| 24 | 2.1.9 `truncateMiddle` 消费者 | 「由 codemode/工具结果压缩使用」 | 实测唯一调用点是 `extensions/mcp/tools.ts:185`（`20*1024` 字节预算，标记 `…N chars truncated…`）；codemode 的 `truncateOutput` 是**另一套**实现，标记为 `…N tokens truncated…`。已改写并给出 `limitMcpContent` 的逐字包装文本 |
| 25 | 2.28.6 `processImage` | 只写「BMP 转 PNG、动画 PNG 被拒绝、BMP 校验严格」 | 两个失败消息、转换提示、尺寸提示（含 `toFixed(2)`）、`DEFAULT_OPTIONS`（2000/2000/4.5MiB base64/80）与策略均缺失；已逐字补齐，并补 `blockImages` 不作用于 read 工具的事实 |
| 26 | 7.2 `suite/agent-session-compaction.test.ts` 行数/例数 | `1000+`、「30 例」 | 实测 **1018 行 / 27 例**；已改 |
| 27 | 2.28.5 `splitIncompleteAnsiSuffix` | 只给正则与函数体，未写调用方式与验收 | 补 `bash-executor.ts:108` 的「先拼 pending → 再解码 → 再切分」顺序、逐字黄金用例、256 窗口语义与不扣留保证 |
| 28 | 2.12.2 `FindOperations.glob` | 「默认操作 `glob: () => []`（占位）」但未说分支判据 | 分支判据是 `customOps?.glob`，因此默认占位永不执行；Rust 若把默认实现接到 fd 会破坏默认 find。已在 2.12.2 明确 |
| 29 | 2.2 路径解析器使用点 | 未区分哪个工具用哪个解析器 | `read` → `resolveReadPathAsync`（四级 macOS 回退）；`write`/`edit`/`ls`/`grep`/`find` → `resolveToCwd`（无回退）；`expandPath` 在本单元工具内**未被调用** |
| 30 | 2.28.6 `detectSupportedImageMimeType` | 只写「按魔数返回五种格式」 | 实际只嗅探前 **4100** 字节；JPEG-LS（`FF D8 FF F7`）→ null；PNG 需 `IHDR` 且非 `acTL`（动画 PNG 判为非图片）；BMP 需 DIB 头/位深校验（1/4/8/16/24/32 且 planes==1） |
| 31 | 2.7.4 `prepareEditArguments` | 标题里写着「导出？—— 未导出」 | 已改为确定表述：**未导出**，且 `edit.ts` 是唯一设置 `prepareArguments` 的工具 |
| 32 | 2.28.8 `APP_TITLE` | 只说「未覆盖时 `"π"`」 | 精确规则：`kianaConfig?.name ? APP_NAME : "π"`，只有 package.json 覆盖 name 时才等于 APP_NAME |
| 33 | 2.2 / 8.4 `expandPath` 调用点 | 「只有 CLI/resume 等路径使用」 | 全仓库 `src/` **无任何调用点**，只有 `path-utils.test.ts` 直接测；已改 |
| 34 | 8.4 验收项（ANSI） | 未写 bash 工具与 `executeBash` 的 ANSI 差异 | bash 工具的 `OutputAccumulator` 收**原始字节**、不 strip ANSI（供 TUI/HTML 上色）；`executeBash` 每块 `stripAnsi`。已写成可判定验收项 |
| 35 | 2.17.6/2.17.7 `formatGrepResult`/`formatLsResult` | 只写「前 15/20 行；warnings」 | 两者都是 `getTextOutput(...).trim()` 后折叠，且 `remaining > 0` 时追加 `... (N more lines,` + keyHint + `)`；warning 文本为 `` `${formatSize(maxBytes ?? DEFAULT_MAX_BYTES)} limit` ``；整体包成 `[Truncated: ...]`。已补 |
| 36 | 2.27.7 `marked` 配置 | renderer 的 `code` 分支只写「highlight/auto + 抛错回退」，未写语言判定与不转义 | 补：`lang && hljs.getLanguage(lang)` → `hljs.highlight(code,{language:lang})`，否则 `hljs.highlightAuto(code)`；两分支各自 `try/catch` → `escapeHtml(code)`；高亮结果**不再转义**。并给 `sanitizeMarkdownUrl` 加上「先剥 C0 再判 scheme」的守门说明（实现在 2.27.4，避免两处重复） |

### 9.2 新增的遗漏内容（missing items, added to the body）

- **2.1.4**：JS `toFixed(1)` 的精确规则、Rust 反例表、实现建议（`format!("{:.20}")` 或整数运算）。
- **2.2**：`normalizePath` 的完整选项与顺序（`trim` / `normalizeUnicodeSpaces` / `stripAtPrefix` /
  **win32 `normalizeWindowsShellPath`** / `expandTilde` / **`file://` → `fileURLToPath`**）；
  `resolvePath`、`getCwdRelativePath`、`formatPathRelativeToCwdOrAbsolute`；同文件其他导出
  （`canonicalizePath`、`getFileRevision`、`isLocalPath`、`markPathIgnoredByCloudSync`）。
  **原规格完全没提 `file://` 与 Git Bash/WSL 盘符转换**，这两条都会改变 `read`/`write`/`edit` 的解析结果。
- **2.19 / 2.21.3**：`getMessageFromEntry` 的 switch **没有 default**；`usage` 与 `context_edit`
  隐式落空 → `undefined`。Rust 必须用 `_ => None` 兜底（否则给新条目类型加 enum 变体时会漏编译错误或 panic）。
- **2.21.4**：第一遍文件跟踪的注释与代码不一致（注释说 "both edited and written"，代码只加 `edited`）
  —— 已确认，Rust 只需加 `edited`（`computeFileLists` 再并入 written）。
- **2.27 末（验收段）**：`generateHtml` 的 6 次单次替换顺序补充了各资产的 `{{` 出现次数
  （`template.html` 6 处、`template.css` 4 处、`template.js` 0 处、3 个 vendor 各 0 处），
  作为「插入内容不会与占位符冲突」的守门断言。
- **2.27.4**：`renderEntry` 的完整分支（assistant 的空 text/thinking 不渲染、`Aborted`/`Error:`、
  bashExecution 的 `(cancelled)`/`(exit N)`、`model_change` 的 `Switched to model:`、
  `compaction` 的 `Compacted from … tokens` 两处、`branch_summary` 走 markdown、
  `custom_message` 的 ` · Hidden in terminal`）；`describeSystemUpdate` / `renderSystemMessageUpdate`
  的逐字输出；三个文本辅助的**不同连接符**（`systemContentText` 用 `\n`、`getSystemMessageText` 用 `\n\n`、
  `extractContent` 用 `""`）。
- **2.27.3**：`isSettingsEntry` 的 5 类列表；**当前 leaf 永远显示**；**仅 toolCall 的 assistant 消息默认隐藏**；
  `getSearchableText` 各类型的精确组成（`compaction` 不索引 summary，`branch_summary` 索引）。
- **2.27.5**：`computeStats` 的细节（models 为 `provider/model` 字符串集、`cost.total` 回退、
  `toolCalls` 计数含未完成调用）；`formatTokens` 的 4 段规则逐字。
- **2.27.6**：三个 toggle 的初值与作用域（`setToolOutputsExpanded` 也控制 `.compaction` 与
  `.skill-invocation`）；`setHiddenMessagesVisible` 切换 body class 与按钮文本；
  `navigateTo` 的完整 8 步语义（自动显示隐藏 custom_message；`renderTree()` 后**重写 header 并
  `attachHeaderHandlers()` 重新绑定三个 toggle**；按 `getPath(targetId)` 用 `entryCache` clone 拼
  `DocumentFragment` **整段重建**消息列表；重放 `setThinkingExpanded`/`setToolOutputsExpanded`
  但**不重放** `setHiddenMessagesVisible`（body class 天然保留）；`.copy-link-btn` 逐个绑定；
  `setTimeout(0)` 滚动里 `getScrollTargetElementId` 的 toolResult→`tool-call-{id}` 路由与
  `|| entry-{id}` 兜底；highlight 仅在有 `scrollToEntryId` 时、2000ms 移除；`'none'` 不滚动）；侧栏 `getSidebarBounds` 的 **720 回退值**
  与 CSS 的 840 不同、移动端 `max-width: 900px`、双击 400、storage key 与四舍五入；
  `buildShareUrl` 的 `kiana-share-base-url` meta + gistId（无值首参）+ `targetId`；
  `downloadSessionJson` 的 header 行构造（`{type:'header', ...header}`）、`application/x-ndjson`、
  文件名 `${header?.id || 'session'}.jsonl`。
- **2.27.7（新增小节）**：`marked.use` 的完整配置（`breaks`、`gfm`、`html`/`tag` 返回 undefined、
  严格删除线正则、`emStrong` 用 marked-cjk-friendly、link/image 的 scheme 白名单分支、
  `code` 的 highlight/auto + 抛错回退、`codespan` 转义）。
- **2.28.8**：`getPackageDir` 的 `KIANA_PACKAGE_DIR`、`getThemesDir`、`getShareViewerUrl`（`#` 拼接）、
  `expandTildePath`、`ENV_AGENT_DIR`/`ENV_SESSION_DIR`、`PACKAGE_NAME`/`CONFIG_DIR_NAME`/`APP_TITLE`。
- **2.29**：vendor 精确字节数；三个 vendor 中 `{{` 均为 0；marked 版本不可替换的理由。
- **3.6**：新增约 20 条逐字文本（系统提示展开提示、`System Prompt Update`、`Tools Added/Removed`、
  `Compacted from`、`Branch Summary`、`Switched to model:`、` · Hidden in terminal`、
  `Thinking ...`、`Aborted`、`Error:`、`(cancelled)`、`(exit N)`、skill 块两条、树节点全部标签、
  嵌套调用的 `(incomplete record)`/`[arguments omitted, N bytes]`、`[invalid arg]`、
  `[invalid content arg - expected string]`、`&nbsp;`、CSS 伪元素
  `[click to show parameters]`/`[hide parameters]`）。
- **4.2**：补全 `SessionEntry` 联合中本单元会读到的 **`usage`** 与 **`context_edit`**，
  以及 `label`（`targetId`）对 HTML 树 label 的作用。
- **4.3**：补 `KIANA_PACKAGE_DIR`、`KIANA_SHARE_VIEWER_URL`、`*_CODING_AGENT_DIR`、`*_CODING_AGENT_SESSION_DIR`、
  `KIANA_EXPERIMENTAL`（严格 `"1"`），并注明改名发布时会变前缀。
- **7.2 / 7.4**：补 21 个此前缺失的测试文件与用例描述（见 9.3），并修正 5 处行数（见 9.1 #15、#26）。
- **2.28.6**：`processImage` 的逐分支、两条 `[Image omitted: ...]` 消息、`[Image converted from X to Y.]`、
  `[Image: original WxH, displayed at ... Multiply coordinates by N.NN ...]`（`toFixed(2)`）、
  缩放默认档 2000/2000/4.5MiB/80 与「PNG/JPEG 择优 → 降质量 → 降尺寸」策略；
  以及 `images.blockImages` **不影响** read 工具（过滤在 `convertToLlm`）。
- **2.28.5**：`splitIncompleteAnsiSuffix` 的运行时契约（拼接/解码/切分顺序、5 条黄金用例、256 窗口）。
- **7.1 / 9.3**：补 5 个测试文件（`mcp-extension`、`block-images`、`image-resize-callers`、
  `agent-session-dynamic-tools`、`suite/agent-session-bash-persistence`）。
- **2.12.2**：`FindOperations.glob` 的默认占位与 `customOps?.glob` 判据。
- **2.2**：各工具实际使用的路径解析器（`read` 用异步四级回退，其余用 `resolveToCwd`；`expandPath` 在 `src/` 无调用点）。
- **2.17.6 / 2.17.7**：`formatGrepResult` / `formatLsResult` 的 `trim()`、折叠行数、`more lines` 提示与
  warning 文本逐字（含 `[Truncated: ...]` 包裹）。
- **7.2**：`suite/agent-session-compaction.test.ts` 之行数/例数改为实测 1018 / 27。
- **8.4**：新增 5 条验收项（toFixed 中点、JS 风格格式化共用、环境变量、资产哈希与替换顺序、
  模板 `getLanguageFromPath` 28 键与 `formatToolCall` 方括号）。

### 9.3 此前未列入测试清单、但覆盖本单元行为的测试（已补入 §7）

| 文件 | 行数 | 与本单元的关系 |
|---|---|---|
| `suite/regressions/8328-zero-usage-auto-compaction.test.ts` | 80 | `estimateContextTokens` 零 usage 回退 |
| `suite/regressions/8989-fork-compaction-label-boundary.test.ts` | 24 | 压缩边界落在 label 条目上，fork 后 `firstKeptEntryId` 重指 |
| `suite/regressions/9789-context-handler-system-messages.test.ts` | 226 | 压缩摘要切片/系统消息重放（与 `hasSystemEntries`、`isLeadingSystemEntry` 同源概念） |
| `suite/regressions/copilot-request-size-overflow.test.ts` | 102 | 图片密集历史的压缩+重试 |
| `session-context-edit.test.ts` | 364 | `appendContextEdit` 的投影语义（2.20.19 的 `context_edit` 规则） |
| `session-manager/build-context.test.ts` | 305 | `buildSessionContext` 全矩阵 |
| `session-manager/load-entries.test.ts` | 182 | 预载 entries / id 不碰撞 / 重建 labels |
| `session-manager/tree-traversal.test.ts` | 579 | `getBranch`/`getTree`（2.21.2 共同祖先） |
| `agent-session-tree-navigation.test.ts` | 323 | 分支摘要挂载点与 abort |
| `agent-session-stats.test.ts` | 316 | 压缩后当前上下文 unknown / 不用 stale usage |
| `cache-stats.test.ts` | 177 | 压缩重置后跳过一轮 |
| `output-pad.test.ts` | 94 | `outputPad` 语义 |
| `collapsible-message-components.test.ts` | 83 | thinking 折叠 |
| `suite/regressions/4167-thinking-toggle-pending-tool-render.test.ts` | 186 | 切换 thinking 不丢进行中渲染 |
| `suite/regressions/8537-custom-message-tool-result-ordering.test.ts` | 151 | custom message 在 tool results 之后 |
| `branch-summarization.test.ts` | 114（原记 46） | 3 个用例的名称与错误文本已逐字补全 |
| `mcp-extension.test.ts` | 868 | `truncateMiddle` 的两条直接验收（20KB 中点截断格式、多字节只切字符边界） |
| `block-images.test.ts` | 148 | read 工具始终返回 image 块（`blockImages` 过滤在 `convertToLlm` 层） |
| `image-resize-callers.test.ts` | 95 | read 的 `processImage` 失败路径（"Image omitted"）与 `inputLimits.images.resize` 透传 |
| `agent-session-dynamic-tools.test.ts` | 256 | bash `spawnHook` 看到 `KIANA_*`，`exposeSessionEnvironment:false` 时全部缺失 |
| `suite/agent-session-bash-persistence.test.ts` | 399 | `executeBash` 的跨 chunk ANSI 切分与末尾半个字符 flush（`splitIncompleteAnsiSuffix` 的运行时验收） |

### 9.4 Rust 映射层的行为缺口（crate/API 选择相关）

1. **数值格式化**：`formatSize` / `formatToolDuration` / `formatTokens`（`template.js` 侧不移植）都依赖
   JS 数字→字符串语义。Rust 侧建议实现单一 `js_number` 模块：
   `js_to_fixed(x: f64, digits: u8) -> String`（按精确十进制展开做 half-away-from-zero）、
   `js_round(x: f64) -> f64`（`.5` 远离零，保留 `-0.0` 语义）、`js_number_to_string(f64)`（最短往返，
   `1` 不输出 `1.0`）。**不要**直接用 `format!` / `to_string`。
2. **`serde_json` 数字**：`tokensBefore`、`wall_time_seconds`、`cost.total` 进入 HTML 的 base64 JSON
   与 JSONL 下载数据；JS 会输出 `1`、`1.5`、`50`，`serde_json` 对 `f64` 会输出 `1.0`。必须在
   序列化前决定「整数值的 f64 按整数输出」的策略，否则导出的 HTML 字节与 TS 不同（8.4 的 sha256 验收会失败）。
   建议用 `serde_json::Number::from_f64` + 自定义 `Serialize`，或把已知整数字段建模为 `i64`/`u64`。
3. **`toLocaleString()` / `toLocaleTimeString()`**：出现在浏览器端（`template.js`），Rust 不实现；
   但**不要在 Rust 侧预格式化时间**，否则数据不同。`timestamp` 必须保持 ISO 8601 字符串。
4. **`localeCompare`（`ls` 排序）**：`icu_collator` 是可选依赖；若省略，必须把
   `tools.test.ts` 的 `should list dotfiles and directories` 固定为唯一权威顺序，并在文档中标注
   「非 en-US 系统上的 JS 行为可能不同」。Rust 侧 `String::cmp` 是字节序，与 JS 的
   `localeCompare` 在大小写/数字/变音符号上都不同（例如 `"a".localeCompare("B") < 0` vs `"a" > "B"`）。
5. **`killProcessTree`**：Linux 依赖「进程组」语义（`detached: true` + `kill(-pid)`）。Rust 侧
   `tokio::process::Command` 需 `process_group(0)`（或 `pre_exec` 里 `setpgid(0,0)`），并实现
   darwin 的 `SIGSTOP` 先行使能；`taskkill` 的路径必须取 `SystemRoot` 而非 PATH。
6. **`waitForChildProcess`**：Rust 没有 Node 的 `close` 事件；`tokio::process::Child::wait()` 会等待
   stdio 关闭。必须并行读 stdout/stderr，并以「exit 事件 + 100ms 无新数据」的等价状态机复现
   （`stdoutEnded && stderrEnded` 立即 finalize，不等计时器）。
7. **`TextDecoder` 流式**：Rust `encoding_rs` 的 `Decoder::decode_to_string(..., last=false)` 就是
   WHATWG 实现（推荐直接用），但 `totalDecodedBytes` 需按**解码后字符串的 UTF-8 字节数**计数
   （`byteLength(text)`，即先解码再计字节；不是原始输入字节数，也不是字符数）——替换字符 U+FFFD 是
   3 字节，所以非法输入会让 `totalDecodedBytes` 比输入更大。`totalRawBytes` 才是原始字节数。
8. **`NFKC` fuzzy 匹配**：`unicode-normalization` 的 `nfkc()` 与 JS `String.normalize("NFKC")` 在
   少数兼容字符上可能有版本差异（Unicode 版本），需把 `tools.test.ts` 的 12 个 fuzzy 用例作为黄金测试。
9. **`template.js` 的 `String.prototype.slice` 与表情**：`truncate`/`formatToolCall` 的 50/40 字符截断
   是 **UTF-16 码元**；Rust 若在 `template.js` 里不执行（推荐），则无影响——但若未来把树节点文本
   也在 Rust 侧生成（当前不需要），必须用 `encode_utf16`。

### 9.5 仍未验证 / 建议实测的点（补充 8.5，编号接续）

9. `diff@8` 的 `@@ -1 +1 @@` 与 `,1` 省略规则仍未实测（本机 `node_modules` 未安装，无法直接跑 `diff`）；
   建议在实现前用 `npm install --ignore-scripts` 后写一个最小脚本实测，并把结果写入 8.3。
10. `createTwoFilesPatch` 的 `\ No newline at end of file` 行（同上）。
11. `marked` 版本：文件头已确认 **marked v18.0.11**（highlight.js v11.9.0、marked-cjk-friendly v0.1.2）；
    升级 marked 会改变 `tokenizer.emStrong`/`del` 的形状，必须锁定（用 SHA-256 断言）。
12. `template.js` 的 `renderHeader` 在 `hasSystemEntries === false` 时才回退到
    `state.systemPrompt`/`state.tools`；若会话里已有 system 条目但**没有** `tools`，则不会渲染工具列表
    —— 这是有意的，不要「修复」。
13. `getSidebarBounds` 的 max 回退 720 与 CSS `--sidebar-max-width: 840px` 的不一致是**既有行为**；
    若 Rust 侧不改 `template.css`，则运行时仍以 CSS 值为准（`getComputedStyle` 优先），回退值只在
    CSS 变量缺失时生效。
14. `formatSize` 的 JS 风格 `toFixed` 实现本身仍需在 Rust 侧用**更大的随机样本**与 Node 对拍
    （本审计只手工验证了 6 个值；建议 0..2 MiB 全量或随机 10 万点的对拍脚本）。
15. `template.js` 的 `renderEntry` 对各 `entry.type` 的**未知类型**走 `return ''`（不渲染），
    与树侧 `default` 分支不同（树侧会显示 `[{type}]`）。Rust 无需实现，但写验收脚本时不要误以为
    「未知条目一定可见」。
16. `renderNestedCalls` 的 `icons[c.status] || '?'`：`status` 不在 `ok/error/unfinished` 时输出 `?`，
    且 `durationMs !== undefined` 才追加 ` {n}ms`（`0ms` 也会显示）。
