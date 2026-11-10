# `tui-text` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-tui` 中 **tui-text** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

- 单元 id：`tui-text`
- 参考实现（只读，**禁止修改**）：`/home/ctyun/code/kiana_rev/kiana/packages/tui/`
- 目标工作区：`/home/ctyun/code/kiana_rev/kiana`

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。Rust 实现必须输出完全一致的文本与字节序列（除另有说明）。中文用于解释语义。凡标 **【逐字】** 的字符串/序列，测试会逐字节比对。
> `\x1b` 表示 U+001B（ESC）；`\x07` 表示 U+0007（BEL）；`\x1b\\` 表示 ESC + 反斜杠（ST）；`\t` 表示 U+0009。
> 所有 JS 字符串索引、`slice`、`indexOf`、`length` 均为 **UTF-16 码元**语义；Rust 侧必须显式选择 UTF-16 或"码点/簇序列"表示，见 §6.1。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/tui/src/utils.ts                    (1402 行)  宽度/换行/截断/切片/ANSI 追踪（本单元最大件）
packages/tui/src/latex.ts                    (1506 行)  LaTeX → Unicode 终端渲染
packages/tui/src/autocomplete.ts             (858 行)   CombinedAutocompleteProvider（斜杠命令 + 路径补全）
packages/tui/src/colors.ts                   (367 行)   颜色类型/校验/解析/转换/ANSI 样式
packages/tui/src/oklab.ts                    (233 行)   Oklab / OKHSL ↔ sRGB 数值转换
packages/tui/src/alt-screen-search.ts        (327 行)   备用屏文本搜索索引、匹配与搜索面板组件
packages/tui/src/fuzzy.ts                    (159 行)   模糊匹配与过滤
packages/tui/src/word-navigation.ts          (117 行)   按词移动光标
packages/tui/src/kill-ring.ts                (46 行)    Emacs 风格 kill ring
packages/tui/src/undo-stack.ts               (28 行)    克隆式撤销栈
packages/tui/src/editor-component.ts         (74 行)    EditorComponent 接口
```

合计 5117 行。全部 11 个文件都必须移植，无"次要文件"。

配套测试（本单元验收基准，见 §7）：

```
packages/tui/test/visible-width.test.ts                       4 it
packages/tui/test/wrap-ansi.test.ts                          19 it
packages/tui/test/truncate-to-width.test.ts                   16 it
packages/tui/test/regression-slice-by-column-ansi-order.test.ts 2 it（issue #10169）
packages/tui/test/tab-width.test.ts                           4 it
packages/tui/test/regression-regional-indicator-width.test.ts  5 it
packages/tui/test/regression-overlay-cjk-boundary.test.ts      4 it
packages/tui/test/colors.test.ts                              4 it
packages/tui/test/fuzzy.test.ts                              17 it（issue #11220）
packages/tui/test/latex.test.ts                              111 个用例（19 个显式 it + 92 个 defineCases 生成；计数见 §7.10）
packages/tui/test/word-navigation.test.ts                    19 it
packages/tui/test/autocomplete.test.ts                       41 个用例（4 it + 37 test()；其中 24 个 fd 用例整组 skip）
packages/tui/test/autocomplete-skill-slash.test.ts            7 it（issue #9944）
packages/tui/test/tui-alt-screen.test.ts                     其中 8 个 it 覆盖 alt-screen-search
```

文档：`packages/tui/README.md`（Colors §"Colors and terminal styles"、Utilities §"Utilities"、ANSI §"ANSI Code Considerations"、Alt-screen 搜索段 "Press `Ctrl+Shift+F` to open or close its bordered search panel…"）、`packages/tui/CHANGELOG.md`、`packages/tui/package.json`（依赖 `get-east-asian-width` 1.6.0）。

CHANGELOG 中与本单元直接相关的条目（作为"为什么这样实现"的背景，**不改变**行为契约）：

| 变更 | 条目（逐字节选） |
| --- | --- |
| fuzzy 连续重打分 | `Fixed \`fuzzyMatch()\` ranking a query scattered over an earlier part of the text above a contiguous match later in it. It now also scores each contiguous occurrence of the query and keeps the best alignment (#11220, #11296)` |
| `@` 补全正则元字符 | `Fixed \`@\` file autocomplete returning no results for queries containing regex metacharacters such as \`[\` or \`(\` (#10972)` |
| `/skill` 空列表 | `Fixed \`/skill\` autocomplete appearing empty when loaded skill names did not contain the letters in \`skill\` (#9944)` |
| 切片 ANSI 顺序 | `Fixed color bleeding past mouse selections and search highlights in fullscreen mode when a styled token ends at the highlight boundary (#10169)` |
| LaTeX 字体切换/cases/嵌套 script | `Fixed LaTeX legacy font switches falling back to raw source, centered \`cases\` layouts around surrounding equations, and vertically laid out unsupported and nested display scripts (#8827, #9564, #7929)` |
| LaTeX 必需参数换行 | `Fixed required LaTeX arguments starting on a new line being parsed as empty (#7760)` |
| LaTeX 控制空格跨行 | `Fixed LaTeX control spaces split across line endings causing complete expressions to fall back to raw source.` |
| LaTeX 间距/矩阵 | `Fixed LaTeX relation, multiplication, and named-operator spacing, and correctly composed matrices with stacked fractions, operator limits, and adjacent matrices.` |
| LaTeX 关系代数 join | `Added LaTeX rendering for relational algebra join symbols (#9050)` |
| Tab 宽度 | `Fixed tab width accounting in column slicing and overlay compositing so tab-containing output cannot exceed the terminal width (#5218)` |
| Thai/Lao AM | `Fixed editor rendering artifacts for Thai Sara Am and Lao AM vowel characters (#3904)` |
| 区域指示符 | `Fixed TUI width calculation for regional indicator symbols (e.g. partial flag sequences like \`🇨\` during streaming) to prevent wrap drift and stale character artifacts in differential rendering.` |

### 0.2 与相邻单元的边界重叠（必须先读）

`packages/tui` 的单元划分并不互斥。同目录已有两份规格：

- `tui-core.md` §0.2 明确把 `src/utils.ts`、`src/alt-screen-search.ts`、`src/fuzzy.ts`、`src/kill-ring.ts`、`src/undo-stack.ts`、`src/word-navigation.ts`、`src/editor-component.ts` 视为 **tui-core** 侧（其中 `utils.ts` 只记录契约、不实现；`alt-screen-search.ts` 在 `tui-core.md` §2.12 完整记录——那是**另一份文档**的小节号，本文件内不存在）。
- `tui-components.md` 同样把上述文件标为 **tui-core**，把 `latex.ts` 标为 **tui-latex**、`autocomplete.ts` 标为 **tui-autocomplete**。

本单元任务文本把上述 11 个文件全部划给 **tui-text**。处理策略：

1. **本规格是实现权威**：凡本文件覆盖的符号，Rust 侧只允许有一份实现， crate 名 `tui_text`（或 `kiana_tui::text::*`），由 `tui-core` / `tui-components` 直接复用，不得重复实现。
2. 与 `tui-core.md` §2.0 / §2.12 的重叠条款取**并集**（这两个编号属于 `tui-core.md`，本文件内不存在）；若两处数值/边界有冲突，以 **TS 源码**为准，并把本文件视为更新。
3. `tui-core.md` 记录的是 `utils.ts` 的"依赖契约"（调用者视角），本文件记录的是**完整实现**（含内部缓存、私有 helper、OSC 8 终止符保留等）。

### 0.3 明确**不**属于本单元（但本单元依赖其符号）

- `src/tui.ts`：`Component`、`Focusable`、`CURSOR_MARKER = "\x1b_kiana:c\x07"`、`FAKE_CURSOR_START = "\x1b_kiana:fc\x07"`、`FAKE_CURSOR_END = "\x1b_kiana:/fc\x07"`、`renderFakeCursor`、`compositeTuiLine`（单元 tui-core）。
- `src/components/input.ts`：`Input` 类（单元 tui-components）。`alt-screen-search.ts` 以 `prompt:" "`、`placeholder:"Find in transcript"`、`placeholderStyle` 构造它。
- `src/keybindings.ts`：`getKeybindings()`、`KeybindingsManager`、`TUI_KEYBINDINGS`（单元 tui-core）。
- `src/terminal-colors.ts`：`RgbColor = { r: number; g: number; b: number }`（单元 tui-core）。
- `src/tui-alt-screen.ts`：搜索功能的宿主（工具条定位、高亮合成、滚动定位）。本单元只提供它调用的 `AltScreenSearchComponent` / `AltScreenSearchIndex` / `findAltScreenSearchMatches` / `getAltScreenSearchMatchKey`。

### 0.4 建议阅读顺序

`utils.ts`（宽度 → ANSI 提取 → tracker → 换行 → 截断 → 切片/分段）→ `fuzzy.ts` → `word-navigation.ts` → `kill-ring.ts` → `undo-stack.ts` → `editor-component.ts` → `colors.ts` → `oklab.ts` → `latex.ts` → `autocomplete.ts` → `alt-screen-search.ts`。

---

## 1. 概览

### 1.1 包的用途

`@earendil-works/kiana-tui` 是一个零运行时依赖（除 `get-east-asian-width`、`marked`、`marked-cjk-friendly`）的终端 UI 库。本单元承载其中所有"文本层"能力：

- **终端列宽**：Unicode 簇分割 + East Asian Width + emoji/区域指示符/间距标记的保守宽度规则；
- **ANSI/OSC/APC 感知的文本处理**：宽度、去序列、换行、截断、按列切片、overlay 前后分段，以及跨行保持样式（SGR + OSC 8 超链接）；
- **颜色科学**：sRGB / ANSI-256 / OKLCH / OKHSL 表示、校验、解析、混色、ANSI 序列生成；
- **LaTeX 数学渲染**：把 `$...$` 内的子集 LaTeX 转为终端可显示的 Unicode（含 display 模式的分数/上下标/矩阵/分段函数二维排版）；
- **补全**：斜杠命令（含 `skill:` 名称折叠匹配）、fd 驱动的 `@`/路径模糊补全、引号与包装符处理、CJK 标点边界；
- **模糊匹配**：连续片段重打分的打分函数（`fuzzyFilter` 供设置列表、命令面板等复用）；
- **编辑器基础设施**：按词移动、kill ring（yank/yank-pop）、撤销栈、`EditorComponent` 扩展接口；
- **备用屏搜索**：把渲染后的转录行规范化为可搜索语料，映射回 (row, col) 区间，并提供搜索工具条组件。

### 1.2 与其他包/单元的关系

```
                ┌──────────────────────────────┐
                │ tui-text (本单元)            │
                │ utils / latex / autocomplete │
                │ colors / oklab / fuzzy ...   │
                └───────┬─────────────┬────────┘
                        │             │
      types: Component/Focusable      │ RgbColor (terminal-colors)
      Input 组件 (tui-components)     │ getKeybindings (tui-core)
                        │             │
        ┌───────────────▼──┐   ┌──────▼───────────────────────┐
        │ tui-components   │   │ tui-core                     │
        │ editor, input,   │   │ layout, tui, tui-alt-screen, │
        │ markdown, box …  │   │ terminal-image, terminal     │
        └───────────────┬──┘   └──────┬───────────────────────┘
                        │             │
                        └────► coding-agent（消费者）
```

消费者清单（已核对源码）：

| 消费者 | 使用的本单元符号 |
| --- | --- |
| `components/editor.ts`（逐字核对 import 块） | `KillRing`、`UndoStack<EditorSnapshot>`、`findWordBackward`/`findWordForward`（带 `segment` + `isAtomicSegment`）、`autocompleteBoundaryRegex`、`autocompleteSeparatorRegex`、`cjkBreakRegex`、`getGraphemeSegmenter`、`getWordSegmenter`、`isWhitespaceChar`、`sliceByColumn`、`visibleWidth`、`AutocompleteProvider`/`AutocompleteSuggestions`。**不导入** `truncateToWidth`/`wrapTextWithAnsi`/`applyBackgroundToLine`/`extractAnsiCode` |
| `components/input.ts` | `KillRing`、`UndoStack<InputState>`、`findWordBackward`/`findWordForward`、`getGraphemeSegmenter`、`isWhitespaceChar`、`sliceByColumn`、`truncateToWidth`、`visibleWidth` |
| `components/markdown.ts` | `renderLatex`（display / inline 两种模式）、`applyBackgroundToLine`、`flattenLines`、`visibleWidth`、`wrapTextWithAnsi` |
| `components/text.ts` | `applyBackgroundToLine`、`flattenLines`、`visibleWidth`、`wrapTextWithAnsi` |
| `components/box.ts` | `flattenLines`、`visibleWidth` |
| `components/h-stack.ts` | `visibleWidth` |
| `components/truncated-text.ts` | `truncateToWidth`、`visibleWidth` |
| `components/scroll-view.ts` | **无**（只导入 `layout-node.ts` 与 `tui.ts`） |
| `components/settings-list.ts` | `fuzzyFilter`（`getText = item.label`）、`truncateToWidth`、`visibleWidth`、`wrapTextWithAnsi` |
| `components/select-list.ts` | `truncateToWidth`、`visibleWidth` |
| `components/image.ts`、`alt-screen-flash.ts` | `truncateToWidth` |
| `layout.ts` | `extractAnsiCode`、`getActiveBackgroundAnsi`、`getGraphemeCellRange`、`sliceByColumn`、`visibleWidth`（**不含** `sliceWithWidth`/`extractSegments`） |
| `tui.ts` | `extractSegments`、`normalizeTerminalOutput`、`sliceByColumn`、`sliceWithWidth`、`visibleWidth`（**不含** `truncateToWidth`） |
| `tui-main-screen.ts` | `truncateToWidth`、`visibleWidth` |
| `alt-screen-search.ts` | `getGraphemeSegmenter`、`stripTerminalSequences`、`truncateToWidth`、`visibleWidth` |
| `word-navigation.ts` | `getWordSegmenter`、`isWhitespaceChar`、`PUNCTUATION_REGEX` |
| `latex.ts` | `visibleWidth`（`renderCases`/`renderMatrix` 的列宽计算） |
| `tui-alt-screen.ts` | `AltScreenSearchComponent`、`AltScreenSearchIndex`、`type AltScreenSearchMatch`、`getAltScreenSearchMatchKey`、`extractAnsiCode`、`getGraphemeCellRange`、`getOsc8LinkAtColumn`、`getWordSegmenter`、`sliceByColumn`、`stripTerminalSequences`、`truncateToWidth`、`visibleWidth` |
| `coding-agent/interactive-mode.ts` | `CombinedAutocompleteProvider([...slashCommands, ...templateCommands, ...extensionCommands, ...skillCommandList], cwd, fdPath)`、`fuzzyFilter` |

### 1.3 入口点与导出面

`src/index.ts` 中继导出（本单元部分，逐字）：

```ts
// Autocomplete support
export {
	type AutocompleteItem,
	type AutocompleteProvider,
	type AutocompleteSuggestions,
	CombinedAutocompleteProvider,
	type SlashCommand,
} from "./autocomplete.ts";
// Colors and styling
export {
	backgroundAnsi,
	type Color,
	type ColorMixSpace,
	colorToHex,
	colorToOkhsl,
	colorToOklch,
	colorToRgb,
	foregroundAnsi,
	type IndexedColor,
	indexedColor,
	mixColors,
	type OkhslChannels,
	type OklchChannels,
	type OklchColorValue,
	okhslColor,
	oklchColor,
	parseColor,
	type RgbColorValue,
	rgbColor,
	styleText,
	styleTextWithAnsi,
	type TerminalColorMode,
	type TextAttributes,
	type TextStyle,
} from "./colors.ts";
export type { EditorComponent } from "./editor-component.ts";
export { type FuzzyMatch, fuzzyFilter, fuzzyMatch } from "./fuzzy.ts";
export { type RenderLatexOptions, renderLatex } from "./latex.ts";
export { oklabToOkhslLightness } from "./oklab.ts";
// Utilities（只导出 6 个；其余为内部或深导入）
export {
	getOsc8LinkAtColumn,
	sliceByColumn,
	stripTerminalSequences,
	truncateToWidth,
	visibleWidth,
	wrapTextWithAnsi,
} from "./utils.ts";
```

说明：

- `visibleWidth` 等**未**出现在 `index.ts` 的 `utils.ts` 符号（`getGraphemeSegmenter`、`cjkBreakRegex`、`extractAnsiCode`、`sliceWithWidth`、`extractSegments`、`getGraphemeCellRange`、`getActiveBackgroundAnsi`、`normalizeTerminalOutput`、`flattenLines`、`applyBackgroundToLine`、`isWhitespaceChar`、`isPunctuationChar`、`PUNCTUATION_REGEX`、`cjkPunctuationRegex`、`autocompleteSeparatorRegex`、`autocompleteBoundaryRegex`、`getWordSegmenter`）是**深导入**（`import {...} from "../utils.ts"`），Rust 侧仍必须是公开 API（crate 内 `pub`）。
- `alt-screen-search.ts` 的符号不在 `index.ts`，由 `tui-alt-screen.ts` 深导入。
- `kill-ring.ts`、`undo-stack.ts`、`word-navigation.ts` 的符号由 `components/*` 深导入，不在根导出面。

本单元**没有** CLI flag、slash 命令、环境变量、子命令、HTTP 端点；"用户可见面"是终端输出字节、错误文案、搜索面板 UI 与 fd 子进程契约（见 §3、§4）。

### 1.4 运行面特征（决定 Rust 设计）

| 特征 | 现状 | Rust 影响 |
| --- | --- | --- |
| 无 I/O 的纯函数占多数 | `utils.ts`、`colors.ts`、`oklab.ts`、`latex.ts`、`fuzzy.ts`、`word-navigation.ts`、`kill-ring.ts`、`undo-stack.ts` | 可做 `#![no_std]`-友好纯逻辑 + 独立 crate，便于差分测试 |
| `visibleWidth` 有全局可变缓存 | 模块级 `Map<string, number>`，上限 512，FIFO 逐出 | 需要 `Mutex`/线程本地；或者用 `RwLock<HashMap>`；**缓存是纯优化**，但会影响内存与性能契约，需保留（见 §2.1.3） |
| `extractSegments` 复用池化 tracker | 模块级 `pooledStyleTracker`，每次调用 `clear()` | 单线程假设；Rust 用 `&mut` 局部或线程局部；**不可重入** |
| `autocomplete` 需要子进程与文件系统 | `child_process.spawn("fd", ...)`、`readdirSync`、`statSync`、`homedir()` | `tokio::process::Command` + `tokio::fs`；`AbortSignal` → `tokio_util::sync::CancellationToken` |
| `autocomplete.getSuggestions` 是异步 | `async` 返回 `Promise`；`signal.aborted` 在多个点检查 | Rust `async fn`；取消必须是协作式并 `kill(SIGKILL)` 子进程 |
| `alt-screen-search` 依赖 `Input` 组件与全局 keybindings | `getKeybindings()` 单例 | Rust 需要可注入的 `Keybindings` 句柄；**每次 `render` 都重新读取**（`render` 内调用 `getKeybindings()`），不是构造时缓存，因此运行时改键立即生效（见 §2.11.6 与 §A.12 第 9 条） |
| 浮点敏感 | OKLab/OKHSL 往返、256 色量化、LaTeX 布局 | 所有 `Math.*` 必须逐条桥接（§6.3），否则十六进制往返与像素级布局会漂移 |
| 平台敏感 | `process.platform === "darwin"` → "Option"；`win32` 影响 fd 查找命令（测试侧） | 用 `cfg!(target_os = "macos")`，但**必须可注入**以便测试 |
| 时区/区域无关 | 无 `Date`、无 `Intl` 数字格式（只有 `Intl.Segmenter` 与 `localeCompare`） | 无需 chrono；需要 ICU 兼容 collation/segmentation |

---

## 2. 逐文件规范

### 2.1 `src/utils.ts`（1402 行）— 宽度、ANSI、换行、截断、切片

#### 2.1.1 模块级状态与常量

```ts
import { eastAsianWidth } from "get-east-asian-width";

const graphemeSegmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });
const wordSegmenter     = new Intl.Segmenter(undefined, { granularity: "word" });

const WIDTH_CACHE_SIZE = 512;
const widthCache = new Map<string, number>();     // 模块级、FIFO 逐出
const pooledStyleTracker = new AnsiCodeTracker(); // extractSegments 复用
```

【逐字】正则（全部照抄，注意 `/v` 标志与集合减法 `--`）：

```
zeroWidthRegex      = /^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Mark}|\p{Surrogate})+$/v
leadingNonPrintingRegex = /^[\p{Default_Ignorable_Code_Point}\p{Control}\p{Format}\p{Mark}\p{Surrogate}]+/v
nonPrintingCharRegex = /^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Format}|\p{Mark}|\p{Surrogate})$/v
markCharRegex       = /^\p{Mark}$/v
terminalSpacingMarkRegex =
  /^(?:[\p{Spacing_Mark}--[\u1734\u302E\u302F]]|[\u065F\u0F7F\u102B\u102C\u1031\u1033-\u1035\u1038\u103A-\u103E])+$/v
rgiEmojiRegex       = /^\p{RGI_Emoji}$/v
THAI_LAO_AM_REGEX        = /[\u0e33\u0eb3]/
THAI_LAO_AM_GLOBAL_REGEX = /[\u0e33\u0eb3]/g
```

`terminalSpacingMarkRegex` 的集合减法 `\p{Spacing_Mark}--[᜴〮〯]` 表示"Unicode 间距标记类，**除去** U+1734（᜴ HANUNOO PAMUDPOD）、U+302E/U+302F（HANGUL 声调标记）"；`|` 右侧是**非** Spacing_Mark 但终端确实占格的历史例外码点：U+065F（阿拉伯 WAW WITH HAMZA）、U+0F7F（藏文 NAMGYEL）、U+102B/U+102C（缅甸）、U+1031、U+1033–U+1035、U+1038、U+103A–U+103E。

【逐字】导出正则：

```ts
export const cjkBreakRegex =
  /[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}\p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]/u;

export const cjkPunctuationRegex = new RegExp(
  `(?:(?=\\p{Punctuation})${cjkBreakRegex.source}|[，．：；！？（）［］｛｝“”‘’…—])`, "u");

export const autocompleteSeparatorRegex = new RegExp(`(?:\\s|${cjkPunctuationRegex.source})`, "u");
export const autocompleteBoundaryRegex  = new RegExp(`(?:^|${autocompleteSeparatorRegex.source})`, "u");

export const PUNCTUATION_REGEX = /[(){}[\]<>.,;:'"!?+\-=*/\\|&%^$#@~`]/;
```

语义要点：

- `cjkBreakRegex` 是**无锚点**测试：`test(segment)` 只要簇内任一码点属于这些 Script_Extensions 即为 true。实测：`"中"→true`、`"ㄅ"→true`（Bopomofo）、`"한"→true`、`"a"→false`。
- `cjkPunctuationRegex` 是"同一位置既是 `\p{Punctuation}` 又是 CJK 脚本"**或**"显式 CJK 标点/全角标点集合"（见下方第三条的精确定义与实测）。粗略实测 `"，"→true`、`"，x"→true`（子串）、`"中"→false`、`"a"→false`。
- `autocompleteBoundaryRegex` 因含 `^` 分支，**对任意字符串 test 恒为 true**（`^` 是零宽断言，任何串在位置 0 都能满足）。真正用作"文本末尾是否为边界"的是 `tokenStartRegex = new RegExp(\`${autocompleteBoundaryRegex.source}$\`, "u")`（见 §2.10.2）。
- **`cjkPunctuationRegex` 的精确语义**（审计补充）：`(?=\p{Punctuation})` 与 `${cjkBreakRegex.source}` 是**同一位置**的零宽前瞻 + 紧随的字符类，因此第一分支 = "该字符同时是 `\p{P}` 与 CJK 脚本"；实际能命中的只有 CJK 文字里的**全角/表意标点**（`。`、`、`、`「`、`」`、`『`、`』`、`《`、`》`、`【`、`】`、`〈`、`〉`、`〜` 等，其 `Script_Extensions` 含 Hani/Hira/Kana/Hangul/Bopo）。`，` `．` `：` `；` `！` `？` `（` `）` `［` `］` `｛` `｝` `“` `”` `‘` `’` `…` `—` 的 scx **不含** CJK 脚本，只能靠**显式列表**分支命中。`cjkPunctuationRegex.test("中")` = **false**（字母不是标点）。**注意**该正则**无锚点**：`test("，x")` = true（子串命中），所以消费方（`autocompleteSeparatorRegex`）是按**单个字符**调用的。逐字符实测表见 §A.5。

#### 2.1.2 `graphemeWidth(segment)`（私有，宽度算法的核心）

签名：`function graphemeWidth(segment: string): number`

步骤（**顺序即语义**）：

1. `if (segment.length === 1)`（UTF-16 长度）：
   - `code = segment.charCodeAt(0)`；`0x20 ≤ code ≤ 0x7e` → 返回 `1`；
   - `code === 0x09` → 返回 `3`。
2. `terminalSpacingMarkRegex.test(segment)` → 返回 `[...segment].length`（**码点个数**，非簇数）。
3. `zeroWidthRegex.test(segment)` → 返回 `0`。
4. `couldBeEmoji(segment) && rgiEmojiRegex.test(segment)` → 返回 `2`。
5. `base = segment.replace(leadingNonPrintingRegex, "")`（去掉前导 Default_Ignorable/Control/Format/Mark/Surrogate）；`cp = base.codePointAt(0)`；`cp === undefined` → 返回 `0`。
6. `0x1F1E6 ≤ cp ≤ 0x1F1FF`（区域指示符）→ 返回 `2`（单个 RI 也按 2 计，避免流式渲染漂移）。
7. `width = eastAsianWidth(cp)`（1 或 2，见 §6.2）。
8. 遍历 `[...base].slice(1)` 的每个码点 `char`：
   - `terminalSpacingMarkRegex.test(char)` → `width += 1`；`followsMark = false`；
   - 否则 `markCharRegex.test(char)` → `followsMark = true`（不加宽）；
   - 否则 `!nonPrintingCharRegex.test(char)`：`c = char.codePointAt(0)!`；
     - `followsMark || (0xFF00 ≤ c ≤ 0xFFEF)`（半角/全角形式）→ `width += eastAsianWidth(c)`；
     - 否则 `c === 0x0E33 || c === 0x0EB3`（Thai/Lao AM 元音）→ `width += 1`；
     - `followsMark = false`；
   - 其中被 `nonPrintingCharRegex` 命中的码点（Mark/Format/Control/...）不改变 `followsMark`。
9. 返回 `width`。

`couldBeEmoji(segment)`（私有）：

```ts
const cp = segment.codePointAt(0)!;
return (
  (cp >= 0x1f000 && cp <= 0x1fbff) ||   // Emoji and Pictograph
  (cp >= 0x2300 && cp <= 0x23ff) ||     // Misc technical
  (cp >= 0x2600 && cp <= 0x27bf) ||     // Misc symbols, dingbats
  (cp >= 0x2b50 && cp <= 0x2b55) ||     // Specific stars/circles
  segment.includes("️") ||         // VS16
  segment.length > 2                    // 多码点序列（ZWJ、肤色、键帽等）
);
```

**实测锚点**（全部为 `visibleWidth` 的结果，单簇时即 `graphemeWidth`）：

| 输入 | 宽度 | 说明 |
| --- | --- | --- |
| `"a"`、`"─"`、`"→"`、`"©"` | 1 | 普通字符；`©` 无 VS16 |
| `"\t"` | 3 | 单码点 Tab |
| `"界"`、`"が"`、`"が"`、`"가"`、`"각"`、`"각"` | 2 | Wide / 组合 |
| `"网络"` | 4 | |
| `"ネットワーク"` | 12 | |
| `"🙂界"` | 4 | |
| `"👍"`、`"👍🏻"`、`"✅"`、`"⚡"`、`"⚡️"`、`"👨"`、`"👨‍💻"`、`"🏳️‍🌈"`、`"👨‍👩‍👧‍👦"`、`"🏴󠁧󠁢󠁳󠁣󠁴󠁿"` | 2 | RGI emoji |
| `"©️"`、`"1️⃣"`、`"#️⃣"`、`"*️⃣"` | 2 | VS16 / 键帽序列 |
| `"🇨"`、`"🇨🇳"`、`"🇯🇵"`、所有 U+1F1E6..U+1F1FF 单例 | 2 | 区域指示符 |
| `"é"`、`"čřžůú"`、`"שָׁ"`、`"بّ"`、`"རྐ"`、`"ᜠ᜴"` | 1 | 普通组合标记零宽 |
| `"ग"` 类：`"र्क"`、`"র্ক"`、`"ર્ક"`、`"ର୍କ"`、`"ర్క"`、`"ര്‍ക"` | 2 | 簇内间距标记加 1 |
| `"नेटवर्क"` | 5 | |
| `"सर्वाधिकार सुरक्षित। ऑर्डर पर क्लिक करें"` | 33 | |
| `"क"` 之外：`"ा"`（U+093E 单独） | 1 | terminalSpacingMarkRegex 命中，按码点数 |
| `"ٟ"`(U+065F)、`"ཿ"`(U+0F7F)、`"ါ"`、`"ေ"` | 1 | 显式例外列表 |
| `"᜴"`(U+1734)、`"〮"`(U+302E)、`"〯"`(U+302F) | 0 | 被排除出间距标记 → Mark 零宽 |
| `"ကာ"`、`"ကေ"`、`"က်"`、`"ကျ"`、`"ကြ"`、`"ကဳ"`、`"ကဴ"`、`"ကဵ"`、`"ကး"` | 2 | 缅甸符号占格 |
| `"ကို"`、`"က္"` | 1 | 非占格（U+102D/U+102F 等为 Mn） |
| `"ำ"`(U+0E33)、`"ຳ"`(U+0EB3) | 1 | AM 单独 |
| `"กำ"`、`"ກຳ"` | 2 | 基字 + AM 加 1 |
| `"\r\n"` | 0 | Control 簇 |
| `"\x7f"` | 0 | Control |

#### 2.1.3 `visibleWidth(str)`（导出）

签名：`export function visibleWidth(str: string): number`

```
if (str.length === 0) return 0;
const asciiWidth = asciiVisibleWidth(str);
if (asciiWidth !== -1) return asciiWidth;                  // 快路径，无缓存
const cached = widthCache.get(str);
if (cached !== undefined) return cached;                    // 缓存（键=原始串）
let clean = str;
if (str.includes("\t")) clean = clean.replace(/\t/g, "   "); // 先展开 TAB
if (clean.includes("\x1b")) { …一遍扫描，剥掉所有可识别序列… }
let width = 0;
for (const { segment } of graphemeSegmenter.segment(clean)) width += graphemeWidth(segment);
if (widthCache.size >= 512) widthCache.delete(widthCache.keys().next().value);
widthCache.set(str, width);
return width;
```

剥序列扫描（注意**跳过**不可识别的 ESC，而不是丢弃整段）：

```ts
let escapeIndex = clean.indexOf("\x1b"); let stripped = ""; let copyFrom = 0;
while (escapeIndex !== -1) {
  const length = ansiCodeLength(clean, escapeIndex);
  if (length > 0) { stripped += clean.slice(copyFrom, escapeIndex); escapeIndex += length; copyFrom = escapeIndex; }
  else escapeIndex++;
  escapeIndex = clean.indexOf("\x1b", escapeIndex);
}
clean = stripped + clean.slice(copyFrom);
```

`asciiVisibleWidth(str)`（私有）：逐 UTF-16 码元；`0x20..0x7e` → `width++`；`0x09` → `width += 3`；`0x1b` → `ansiCodeLength` 为 0 则返回 `-1`，否则跳过；其它 → `-1`。

**实测**：

```
visibleWidth("") = 0
visibleWidth("abc") = 3
visibleWidth("\x1b[38;5;4mhello\x1b[39m world") = 11
visibleWidth("\x1b]8;;https://example.com\x07link\x1b]8;;\x07") = 4
visibleWidth("\x1b]133;A\x1b\\prompt") = 6
visibleWidth("\x1b_kiana:c\x07cursor") = 6
visibleWidth("\x1b[1ma\tb\x1b[22m") = 5
visibleWidth("\x1b[31m日本\x1b[39m ok") = 7
visibleWidth("\x1b[31m─→\x1b[39m") = 2
visibleWidth("\x1b[31") = 3            // 未终止 CSI：ESC 与 "[" "3" "1" 各计 0/1/1/1
visibleWidth("a\x1b") = 1
visibleWidth("\x1b[2A") = 3            // 'A' 不是允许的 CSI 终止符 → 不识别
visibleWidth("\x1b[?25l") = 5
visibleWidth("\x1b[1;31mX") = 1
visibleWidth("\x1b[K") = 0
visibleWidth("\x1b[1K") = 0
visibleWidth("\x1b[G") = 0
visibleWidth("\x1b[3H") = 0
visibleWidth("\x1b[2J") = 0
visibleWidth("\x1b[1;2X") = 5          // X 不是终止符
visibleWidth("\x1b]8;;u") = 5           // 未终止 OSC
visibleWidth("\x1b]8;;u\x07") = 0
visibleWidth("\x1b_payload\x1b\\") = 0
visibleWidth("\x1b_p") = 2
visibleWidth("\x1b[") = 1
visibleWidth("a\rb") = 2                // \r 零宽
visibleWidth("a\u0000b") = 2
visibleWidth("\t") = 3
visibleWidth("a\tb") = 5
```

#### 2.1.4 `ansiCodeLength(str, pos)`（私有，**必须逐字复刻的识别规则**）

签名：`function ansiCodeLength(str: string, pos: number): number`

```
if (pos >= str.length || str.charCodeAt(pos) !== 0x1b) return 0;
const next = str[pos + 1];
if (next === "[") {
  for (let j = pos + 2; j < str.length; j++) {
    const c = str.charCodeAt(j);
    // 只接受 m(0x6d)、G(0x47)、K(0x4b)、H(0x48)、J(0x4a) 作为终止符
    if (c === 0x6d || c === 0x47 || c === 0x4b || c === 0x48 || c === 0x4a) return j + 1 - pos;
  }
  return 0;                       // 没有终止符 → 整段不识别
}
if (next === "]" || next === "_") {
  for (let j = pos + 2; j < str.length; j++) {
    const c = str.charCodeAt(j);
    if (c === 0x07) return j + 1 - pos;                        // BEL
    if (c === 0x1b && str[j + 1] === "\\") return j + 2 - pos; // ST
  }
  return 0;
}
return 0;
```

要点：

- CSI 的**参数区间不校验**（可以是任意字节，包括 `?`、字母、空格），只看是否在某个位置出现 m/G/K/H/J。
- 其它 ESC 引导（`\x1b(`、`\x1b7`、`\x1b=` 等）一律不识别 → ESC 本身与后续字符按普通字符计宽。
- `pos` 可以是任意位置；不要求前缀对齐。
- 复杂度 O(n)：`visibleWidth` 与其它扫描函数对同一串反复调用该函数时可能导致 O(n²)（截断大字符串时的已知行为，见 §2.1.9 的连续前缀扫描）。

`extractAnsiCode(str, pos)`：`length > 0 ? { code: str.substring(pos, pos + length), length } : null`。

实测：`extractAnsiCode("\x1b[31mX",0) = {code:"\x1b[31m",length:5}`；`extractAnsiCode("X\x1b[31m",1)` 同上；`extractAnsiCode("\x1b[2A",0) = null`；越界 → `null`。

#### 2.1.5 `stripTerminalSequences(str)`

```
if (!str.includes("\x1b")) return str;
let result = ""; let i = 0;
while (i < str.length) { const ansi = extractAnsiCode(str, i); if (ansi) { i += ansi.length; continue; } result += str[i]; i++; }
return result;
```

实测：`"\x1b[31mred\x1b[0m"→"red"`；`"a\x1b"→"a\x1b"`（不可识别 ESC 原样保留）；`"\x1b]8;;u\x07link\x1b]8;;\x07"→"link"`；`"\x1b[2A"→"\x1b[2A"`。

#### 2.1.6 `getGraphemeCellRange(line, column)`

返回 `{ start: number; end: number } | undefined`（**UTF-16 无关的终端列号**）。

算法：`currentCol = 0`；循环：
- 若 `extractAnsiCode(line,i)` 命中 → `i += length`（零宽）；
- 否则找 `textEnd` = 下一个能识别 ANSI 的起点（逐 UTF-16 前进）；
- 对该段 `line.slice(i, textEnd)` 逐 grapheme：`width = graphemeWidth(segment)`；若 `width > 0 && column >= currentCol && column < currentCol + width` → 返回 `{start: currentCol, end: currentCol + width}`；否则 `currentCol += width`；
- `i = textEnd`。

实测：`("abcd",0)→{0,1}`；`("abcd",4)→undefined`；`("界a",1)→{0,2}`；`("界a",2)→{2,3}`；`("\x1b[31mab\x1b[0m",1)→{1,2}`；`("\tX",0)→{0,3}`；`("\tX",3)→{3,4}`。

#### 2.1.7 `getOsc8LinkAtColumn(line, column)`

返回覆盖该列的 OSC 8 URL（`string | undefined`）。

- 维护 `activeUrl: string | undefined`；
- ANSI 片段用 `^\x1b\]8;[^;]*;([^\x07\x1b]*)(?:\x07|\x1b\\)$` 匹配：命中则 `activeUrl = hyperlink[1] || undefined`（空 URL = 关闭）；
- 文本按 grapheme 推进，`width = segment === "\t" ? 3 : graphemeWidth(segment)`；`column >= currentCol && column < currentCol+width` → 返回 `activeUrl`。

实测：`"\x1b]8;;https://a\x07ab\x1b]8;;\x07cd"` 列 0/1 → `"https://a"`，列 2/3 → `undefined`；带 `id=1;` 参数与 ST 终止符同样返回 URL；`"\x1b]8;;u\x07\tab\x1b]8;;\x07"` 列 1（TAB 占 0..3）→ `"u"`。

#### 2.1.8 `normalizeTerminalOutput(str)`

```ts
let normalized = str;
if (THAI_LAO_AM_REGEX.test(normalized))
  normalized = normalized.replace(THAI_LAO_AM_GLOBAL_REGEX, ch => ch === "ำ" ? "ํา" : "ໍາ");
if (!normalized.includes("\t")) return normalized;
let result = ""; let i = 0;
while (i < normalized.length) {
  const ansi = extractAnsiCode(normalized, i);
  if (ansi) { result += ansi.code; i += ansi.length; continue; }
  result += normalized[i] === "\t" ? "   " : normalized[i]; i++;
}
return result;
```

要点：

- AM 规范化在扫描前对**整串**执行，包括终端序列内部（可观察行为）。
- 替换串逐字：`"ำ"`（U+0E33）→ `"ํา"`（**U+0E4D + U+0E32**）；`"ຳ"`（U+0EB3）→ `"ໍາ"`（**U+0ECD + U+0EB2**）。源码里字面量的视觉形态可能是 `"ํา"`/`"ໍາ"`，**必须**按码点比对，不能按"看起来像空格"理解。
- TAB 展开只在**可识别序列之外**进行；序列内 TAB 保持原字节（测试 `keeps tabs inside terminal control sequences byte-identical`）。
- 展开固定 3 空格，不做 tab stop 对齐。

实测：`"ำabc"→"ําabc"`；`"ຳabc"→"ໍາabc"`；`"a\tb"→"a   b"`；`"\x1b]8;;u\tv\x07x\ty"→"\x1b]8;;u\tv\x07x   y"`；`"\x1b[31m\t"→"\x1b[31m   "`；`"\x1b[2A\t"→"\x1b[2A   "`（不可识别序列的 TAB 也被展开）。

#### 2.1.9 `truncateToWidth(text, maxWidth, ellipsis = "...", pad = false)`

完整控制流（**必须逐条复刻**）：

```
1  if (maxWidth <= 0) return "";
2  if (text.length === 0) return pad ? " ".repeat(maxWidth) : "";
3  const ellipsisWidth = visibleWidth(ellipsis);
4  if (ellipsisWidth >= maxWidth) {
5     const textWidth = visibleWidth(text);
6     if (textWidth <= maxWidth) return pad ? text + " ".repeat(maxWidth - textWidth) : text;
7     const clippedEllipsis = truncateFragmentToWidth(ellipsis, maxWidth);
8     if (clippedEllipsis.width === 0) return pad ? " ".repeat(maxWidth) : "";
9     return finalizeTruncatedResult("", 0, clippedEllipsis.text, clippedEllipsis.width, maxWidth, pad);
10 }
11 if (isPrintableAscii(text)) {          // 定义见 §A.3
12    if (text.length <= maxWidth) return pad ? text + " ".repeat(maxWidth - text.length) : text;
13    const targetWidth = maxWidth - ellipsisWidth;
14    return finalizeTruncatedResult(text.slice(0, targetWidth), targetWidth, ellipsis, ellipsisWidth, maxWidth, pad);
15 }
16 const targetWidth = maxWidth - ellipsisWidth;
17 // 通用路径：单遍扫描，保留"连续前缀"
18 let result = "", pendingAnsi = "", visibleSoFar = 0, keptWidth = 0;
19 let keepContiguousPrefix = true, overflowed = false, exhaustedInput = false;
20 const hasAnsi = text.includes("\x1b"), hasTabs = text.includes("\t");
21 if (!hasAnsi && !hasTabs) {
22    for (const { segment } of graphemeSegmenter.segment(text)) {
23       const width = graphemeWidth(segment);
24       if (keepContiguousPrefix && keptWidth + width <= targetWidth) { result += segment; keptWidth += width; }
25       else keepContiguousPrefix = false;
26       visibleSoFar += width;
27       if (visibleSoFar > maxWidth) { overflowed = true; break; }
28    }
29    exhaustedInput = !overflowed;
30 } else {
31    let i = 0;
32    while (i < text.length) {
33       const ansi = extractAnsiCode(text, i);
34       if (ansi) { pendingAnsi += ansi.code; i += ansi.length; continue; }
35       if (text[i] === "\t") {
36          if (keepContiguousPrefix && keptWidth + 3 <= targetWidth) {
37             if (pendingAnsi) { result += pendingAnsi; pendingAnsi = ""; }
38             result += "\t"; keptWidth += 3;
39          } else { keepContiguousPrefix = false; pendingAnsi = ""; }
40          visibleSoFar += 3;
41          if (visibleSoFar > maxWidth) { overflowed = true; break; }
42          i++; continue;
43       }
44       let end = i;                       // 找到下一个 TAB 或可识别 ANSI 的起点
45       while (end < text.length && text[end] !== "\t") { if (extractAnsiCode(text, end)) break; end++; }
46       for (const { segment } of graphemeSegmenter.segment(text.slice(i, end))) {
47          const width = graphemeWidth(segment);
48          if (keepContiguousPrefix && keptWidth + width <= targetWidth) {
49             if (pendingAnsi) { result += pendingAnsi; pendingAnsi = ""; }
50             result += segment; keptWidth += width;
51          } else { keepContiguousPrefix = false; pendingAnsi = ""; }
52          visibleSoFar += width;
53          if (visibleSoFar > maxWidth) { overflowed = true; break; }
54       }
55       if (overflowed) break;
56       i = end;
57    }
58    exhaustedInput = i >= text.length;
59 }
60 if (!overflowed && exhaustedInput) return pad ? text + " ".repeat(Math.max(0, maxWidth - visibleSoFar)) : text;
61 return finalizeTruncatedResult(result, keptWidth, ellipsis, ellipsisWidth, maxWidth, pad);
```

`truncateFragmentToWidth(text, maxWidth)`（私有）：

```
if (maxWidth <= 0 || text.length === 0) return {text: "", width: 0};
if (isPrintableAscii(text)) { const clipped = text.slice(0, maxWidth); return {text: clipped, width: clipped.length}; }
if (!text.includes("\x1b") && !text.includes("\t")) {
   result=""; width=0; for each grapheme { w; if (width+w > maxWidth) break; result+=g; width+=w; } return {result,width};
}
// 含 ANSI 或 TAB：
i=0; result=""; width=0; pendingAnsi="";
while (i < text.length) {
   ansi → pendingAnsi += code; i += len; continue;
   "\t" → if (width+3 > maxWidth) break; flush pendingAnsi; result += "\t"; width += 3; i++; continue;
   end = i; while (end < len && text[end] !== "\t") { if (extractAnsiCode(text,end)) break; end++; }
   for each grapheme of text.slice(i,end): w; if (width+w > maxWidth) return {text:result,width}; flush pendingAnsi; result+=g; width+=w;
   i = end;
}
return {text: result, width};
```

`finalizeTruncatedResult(prefix, prefixWidth, ellipsis, ellipsisWidth, maxWidth, pad)`（私有）：

```
const reset = "\x1b[0m";
const hyperlinkClose = getActiveOsc8Close(prefix);      // "" 或 OSC 8 关闭（沿用原终止符）
const visible = prefixWidth + ellipsisWidth;
let result = ellipsis.length > 0 ? `${prefix}${hyperlinkClose}${reset}${ellipsis}${reset}`
                                 : `${prefix}${hyperlinkClose}${reset}`;
return pad ? result + " ".repeat(Math.max(0, maxWidth - visible)) : result;
```

**实测（逐字节）**：

```
truncateToWidth("Hello World", 8)                       = "Hello\x1b[0m...\x1b[0m"
truncateToWidth("Hello World", 8, "")                   = "Hello Wo\x1b[0m"
truncateToWidth("abcdef", 1, "🙂")                       = ""            // 宽省略号裁到 0 宽
truncateToWidth("abcdef", 2, "🙂")                       = "\x1b[0m🙂\x1b[0m"
truncateToWidth("abcdef", 3, "界界")                      = "\x1b[0m界\x1b[0m"
truncateToWidth("a", 2, "🙂")                            = "a"
truncateToWidth("界", 2, "🙂")                           = "界"
truncateToWidth("🙂\t界 \x1b_abc\x07", 7, "…", true)      = "🙂\t\x1b[0m…\x1b[0m "
truncateToWidth("", 5, "...", true)                     = "     "
truncateToWidth("abc", 0) / ("abc", -1)                 = ""
truncateToWidth("abc", 10, "...", true)                 = "abc       "
truncateToWidth("abc", 3, "", true)                     = "abc"        // 早退分支不加 padding? 注意：ellipsisWidth=0 < 3 → ASCII 分支 text.length<=max → pad=true 时加 0 空格 → "abc"
truncateToWidth("\x1b[31mhello world\x1b[0m", 8)         = "\x1b[31mhello\x1b[0m...\x1b[0m"
truncateToWidth("\x1b[31mhello\x1b[0m world", 8)         = "\x1b[31mhello\x1b[0m...\x1b[0m"
truncateToWidth("\x1b]8;;https://x\x07abcdefgh\x1b]8;;\x07", 6)
                                                        = "\x1b]8;;https://x\x07abc\x1b]8;;\x07\x1b[0m...\x1b[0m"
truncateToWidth("\x1b]8;;u\x07abcdefgh", 6)              = "\x1b]8;;u\x07abc\x1b]8;;\x07\x1b[0m...\x1b[0m"
truncateToWidth("\x1b]8;id=1;u\x1b\\abcdefgh", 6)        = "\x1b]8;id=1;u\x1b\\abc\x1b]8;;\x1b\\\x1b[0m...\x1b[0m"
truncateToWidth("\x1b]8;;u\x07abcdefgh", 6, "")          = "\x1b]8;;u\x07abcdef\x1b]8;;\x07\x1b[0m"
truncateToWidth("\x1b]8;;u\x07ab\x1b]8;;\x07cdefgh", 6)  = "\x1b]8;;u\x07ab\x1b]8;;\x07c\x1b[0m...\x1b[0m"
truncateToWidth("界", 1)                                 = "\x1b[0m.\x1b[0m"
truncateToWidth("界界", 2)                               = "\x1b[0m..\x1b[0m"
truncateToWidth("ab\tcd", 4)                             = "a\x1b[0m...\x1b[0m"
truncateToWidth("ab\tcd", 5)                             = "ab\x1b[0m...\x1b[0m"
truncateToWidth("ab\tcd", 6, undefined, true)             = "ab\x1b[0m...\x1b[0m "
truncateToWidth("\t\t\t", 7)                             = "\t\x1b[0m...\x1b[0m"
```

`"\x1b]8;;u\x07abcdefgh"` 无省略号的结果说明：当文本恰好耗尽而光标仍在链接里时，走早退分支 `!overflowed && exhaustedInput` → 返回**原串**，但测试 `closes a BEL-terminated OSC 8 link when truncating its label` 用的是 `"some-longer-label-here"` 触发截断 → 输出 `${open}some-longer-${close}\x1b[0m...\x1b[0m`。

#### 2.1.10 `getActiveOsc8Close(prefix)` / OSC 8 解析（私有）

```ts
type Osc8Terminator = "\x07" | "\x1b\\";
interface ActiveHyperlink { params: string; url: string; terminator: Osc8Terminator; }

function parseOsc8Hyperlink(ansiCode: string): ActiveHyperlink | null | undefined {
  if (!ansiCode.startsWith("\x1b]8;")) return undefined;
  const terminator = ansiCode.endsWith("\x07") ? "\x07" : "\x1b\\";
  const body = ansiCode.slice(4, terminator === "\x07" ? -1 : -2);
  const separatorIndex = body.indexOf(";");
  if (separatorIndex === -1) return undefined;
  const params = body.slice(0, separatorIndex);
  const url = body.slice(separatorIndex + 1);
  if (!url) return null;                     // 空 URL = 关闭链接
  return { params, url, terminator };
}
function formatOsc8Hyperlink(h) { return `\x1b]8;${h.params};${h.url}${h.terminator}`; }
function formatOsc8Close(t)     { return `\x1b]8;;${t}`; }

function getActiveOsc8Close(prefix: string): string {
  if (!prefix.includes("\x1b]8;")) return "";
  let active: ActiveHyperlink | null = null; let i = 0;
  while (i < prefix.length) {
    const ansi = extractAnsiCode(prefix, i);
    if (ansi) { const h = parseOsc8Hyperlink(ansi.code); if (h !== undefined) active = h; i += ansi.length; }
    else i++;
  }
  return active ? formatOsc8Close(active.terminator) : "";
}
```

三态约定：`undefined` = 不是 OSC 8 序列；`null` = 关闭；对象 = 打开。**必须保留原终止符**（BEL 与 ST 不互换），测试 `preserves BEL terminators when wrapping OAuth-style hyperlinks` 会逐字节检查。

#### 2.1.11 `AnsiCodeTracker`（私有类，跨行样式状态的唯一来源）

字段：`bold/dim/italic/underline/blink/inverse/hidden/strikethrough: boolean`、`fgColor/bgColor: string | null`、`activeHyperlink: ActiveHyperlink | null`。

`process(ansiCode)`：

1. `const hyperlink = parseOsc8Hyperlink(ansiCode); if (hyperlink !== undefined) { this.activeHyperlink = hyperlink; return; }`
2. `if (!ansiCode.endsWith("m")) return;`
3. `const match = ansiCode.match(/\x1b\[([\d;]*)m/); if (!match) return;`
4. `params === "" || params === "0"` → `reset()` 并返回（**不触碰 hyperlink**）。
5. `parts = params.split(";")`，`i=0` 循环：
   - `code = Number.parseInt(parts[i], 10)`（可能 NaN）；
   - `code === 38 || code === 48`：
     - `parts[i+1] === "5" && parts[i+2] !== undefined` → `colorCode = \`${parts[i]};${parts[i+1]};${parts[i+2]}\``（用**原字符串**），写 fg/bg，`i += 3`，continue；
     - `parts[i+1] === "2" && parts[i+4] !== undefined` → `colorCode = 五个原字符串用 ";" 连接`，写 fg/bg，`i += 5`，continue；
   - `switch (code)`：0→reset；1 bold；2 dim；3 italic；4 underline；5 blink；7 inverse；8 hidden；9 strikethrough；21 bold=false；22 bold=dim=false；23 italic=false；24 underline=false；25 blink=false；27 inverse=false；28 hidden=false；29 strikethrough=false；39 fg=null；49 bg=null；default：30–37/90–97 → `fgColor = String(code)`；40–47/100–107 → `bgColor = String(code)`；
   - `i++`。

`process` 的**逐条补充**（源码精确）：

- `process` 先试 OSC 8：`parseOsc8Hyperlink` 返回 `undefined`（不是 OSC 8）才继续；返回 `null`（关闭）或对象（打开）都写入 `activeHyperlink` 并**立即 return**。
- `!ansiCode.endsWith("m")` → return（**不处理** `\x1b[K`、`\x1b[2J` 等）。
- `/\x1b\[([\d;]*)m/` **无 `g`/`^`**：它会在字符串中**任意位置**找第一个匹配（但 ANSI 码本身总是以 `\x1b[` 开头，等价于从头匹配）。参数只允许数字与 `;`。
- `params === ""`（`\x1b[m`）与 `params === "0"` 都是完整 reset。
- `Number.parseInt(parts[i], 10)`：`""` → `NaN`；`NaN` 落到 switch 的 `default`，且 `NaN >= 30` 为 false → **无操作**。
- `38`/`48` 的多参形式只识别 `5;N`（需 `parts[i+2]` 存在）与 `2;R;G;B`（需 `parts[i+4]` 存在）；不满足时**落到 switch**（`case 38`/`48` 不存在 → `default` → 范围检查不命中 → 无操作），随后 `i++` 逐个跳过。
- 颜色值存的是**原始参数串**（`"38;5;240"`、`"48;2;1;2;3"`、`"31"`），`getActiveCodes` 直接拼接，因此 `\x1b[38;5;240;41m` 这类复合序列会保留原始写法。

`reset()`（私有）：8 个布尔清零、fg/bg 置 null，**不改** `activeHyperlink`。
`clear()`：`reset()` + `activeHyperlink = null`。
`getActiveCodes()`：按顺序 `1,2,3,4,5,7,8,9,fg,bg` 收集非空项，`\x1b[` + `join(";")` + `m`（无项则 `""`），再追加 `formatOsc8Hyperlink(activeHyperlink)`（若有）。
`getActiveBackgroundCode()`：`bgColor ? \`\x1b[${bgColor}m\` : ""`。
`hasActiveCodes()`：任一无布尔/颜色/链接为真。
`getLineEndReset()`：`underline` → `"\x1b[24m"`；`activeHyperlink` → `formatOsc8Close(terminator)`；拼接返回。

实测：

```
getActiveBackgroundAnsi("")                  = ""
getActiveBackgroundAnsi("\x1b[41mabc")       = "\x1b[41m"
getActiveBackgroundAnsi("\x1b[41mabc\x1b[0m")= ""
getActiveBackgroundAnsi("\x1b[48;5;240mabc") = "\x1b[48;5;240m"
getActiveBackgroundAnsi("\x1b[1;44mabc")     = "\x1b[44m"
getActiveBackgroundAnsi("\x1b[41m\x1b[44mab\x1b[49mc") = ""     // 49 清除
getActiveBackgroundAnsi("\x1b[38;5;9;48;2;1;2;3mX") = "\x1b[48;2;1;2;3m"
getActiveBackgroundAnsi("\x1b[44m\x1b[0mX") = ""
```

`getActiveCodes()` 组合示例：`"\x1b[4m" + "\x1b[41m"` → `"\x1b[4;41m"`（换行续行前缀）。测试 `should reset underline but preserve background when wrapping underlined text inside background` 会检查 `"[4;41m"` 形式。

#### 2.1.12 `splitIntoTokensWithAnsi(text)`（私有）

返回 token 数组，token 是"连续同类可见字符 + 附着 ANSI"：

- `pendingAnsi` 缓存待附着到下一个可见字符的 ANSI；
- 可见段 = 从 `i` 到下一个**可识别** ANSI 起点（逐 UTF-16 前进；不可识别 ESC 视为普通字符）；
- `ascii = isPrintableAscii(chunk)`：ASCII 段按 UTF-16 码元迭代，否则按 grapheme 迭代；
- 对每个 `segment`：
  - `segmentIsSpace = segment === " "`（**仅 ASCII 空格**；TAB 属于 word 类）；
  - 若 `!ascii && !segmentIsSpace && cjkBreakRegex.test(segment)` → 先 flush current，再把 `pendingAnsi + segment` 作为独立 token 推入，`pendingAnsi = ""`，continue；
  - `segmentKind = segmentIsSpace ? "space" : "word"`；若 current 非空且 kind 变了 → flush；
  - `pendingAnsi` 非空 → 追加到 current 并清空；
  - `currentKind = segmentKind`；`current += segment`。
- 循环结束：残余 `pendingAnsi` → 追加到 current（有 current）或最后一个 token，否则作为 current；
- `current` 非空 → 推入。

#### 2.1.13 `wrapTextWithAnsi(text, width)`（导出）

```
if (!text) return [""];
const inputLines = text.split(/\r\n|\r|\n/);
const result: string[] = [];
const tracker = new AnsiCodeTracker();
for (const inputLine of inputLines) {
  const prefix = result.length > 0 ? tracker.getActiveCodes() : "";
  for (const wrappedLine of wrapSingleLine(prefix + inputLine, width)) result.push(wrappedLine);
  updateTrackerFromText(inputLine, tracker);
}
return result.length > 0 ? result : [""];
```

`wrapSingleLine(line, width)`（私有）：

```
if (!line) return [""];
const visibleLength = visibleWidth(line);
if (visibleLength <= width) return [line];              // 不 trimEnd
const wrapped = []; const tracker = new AnsiCodeTracker();
const tokens = splitIntoTokensWithAnsi(line);
let currentLine = "", currentVisibleLength = 0;
for (const token of tokens) {
  const tokenVisibleLength = visibleWidth(token);
  const isWhitespace = token.trim() === "";
  if (tokenVisibleLength > width && !isWhitespace) {
     if (currentLine) { const r = tracker.getLineEndReset(); if (r) currentLine += r; wrapped.push(currentLine); currentLine=""; currentVisibleLength=0; }
     const broken = breakLongWord(token, width, tracker);
     for (let i = 0; i < broken.length - 1; i++) wrapped.push(broken[i]!);
     currentLine = broken[broken.length - 1]; currentVisibleLength = visibleWidth(currentLine);
     continue;                                          // 注意：跳过 updateTrackerFromText
  }
  const totalNeeded = currentVisibleLength + tokenVisibleLength;
  if (totalNeeded > width && currentVisibleLength > 0) {
     let lineToWrap = currentLine.trimEnd();
     const r = tracker.getLineEndReset(); if (r) lineToWrap += r;
     wrapped.push(lineToWrap);
     if (isWhitespace) { currentLine = tracker.getActiveCodes(); currentVisibleLength = 0; }
     else { currentLine = tracker.getActiveCodes() + token; currentVisibleLength = tokenVisibleLength; }
  } else { currentLine += token; currentVisibleLength += tokenVisibleLength; }
  updateTrackerFromText(token, tracker);
}
if (currentLine) wrapped.push(currentLine);
return wrapped.length > 0 ? wrapped.map(line => line.trimEnd()) : [""];
```

`breakLongWord(word, width, tracker)`（私有）：

1. `currentLine = tracker.getActiveCodes(); currentWidth = 0;`
2. 把 word 拆成 `{type:"ansi"|"grapheme", value}` 序列（ANSI 片段整体保留；文本段再按 grapheme 拆）；
3. 逐段：ansi → `currentLine += value; tracker.process(value);` continue；grapheme 空 → skip；`w = visibleWidth(grapheme)`；
   - 若 `currentWidth + w > width` → 追加 `getLineEndReset()`，push currentLine，`currentLine = tracker.getActiveCodes()`，`currentWidth = 0`；
   - `currentLine += grapheme; currentWidth += w;`
4. 末尾 currentLine 非空 → push（**不加 reset**）；
5. `return lines.length > 0 ? lines : [""]`。

**实测**：

```
wrapTextWithAnsi("", 10)                                  = [""]
wrapTextWithAnsi("a\nb", 10)                              = ["a","b"]
wrapTextWithAnsi("first\nsecond\r\nthird\rfourth", 80)    = ["first","second","third","fourth"]
wrapTextWithAnsi("\x1b[31mfirst\r\nsecond\rthird\x1b[0m", 80)
                                                          = ["\x1b[31mfirst","\x1b[31msecond","\x1b[31mthird\x1b[0m"]
wrapTextWithAnsi("hello world this is a test", 10)        = ["hello","world this","is a test"]
wrapTextWithAnsi("This is an example 中文汉字测试段落内容中文汉字测试段落内容.", 40)
                                                          = ["This is an example 中文汉字测试段落内容","中文汉字测试段落内容."]
wrapTextWithAnsi("  ", 1)                                 = [""]
wrapTextWithAnsi("   ", 5)                                = ["   "]        // 未超宽 → 直接返回原行
wrapTextWithAnsi("      - 🇨", 9)                          = ["      -","🇨"]
wrapTextWithAnsi("averylongwordthatneedstobebroken", 5)   = ["avery","longw","ordth","atnee","dstob","ebrok","en"]
wrapTextWithAnsi("\x1b[4munderlined text here \x1b[24mmore", 18)
                                                          = ["\x1b[4munderlined text\x1b[24m","\x1b[4mhere \x1b[24mmore"]
wrapTextWithAnsi("\x1b[41mprefix \x1b[4mUNDERLINED_CONTENT_THAT_WRAPS\x1b[24m suffix\x1b[0m", 20)
   = ["\x1b[41mprefix","\x1b[41m\x1b[4mUNDERLINED_CONTENT_T\x1b[24m","\x1b[4;41mHAT_WRAPS\x1b[24m suffix\x1b[0m"]
wrapTextWithAnsi("abc def", 3)                            = ["abc","def"]
wrapTextWithAnsi("a b c", 1)                              = ["a","b","c"]
wrapTextWithAnsi("\x1b]8;;u\x07abc def\x1b]8;;\x07", 4)   = ["\x1b]8;;u\x07abc\x1b]8;;\x07","\x1b]8;;u\x07def\x1b]8;;\x07"]
wrapTextWithAnsi("x\ty", 1)                               = ["x","","y"]     // TAB token 超宽 → breakLongWord 产生空行
wrapTextWithAnsi("hello\n", 5)                            = ["hello",""]
wrapTextWithAnsi("\n", 5)                                 = ["",""]
wrapTextWithAnsi("abcdef", 0) / (…, -1)                   = ["","a","b","c","d","e","f"]
wrapTextWithAnsi("\x1b[31m", 5)                           = ["\x1b[31m"]
wrapTextWithAnsi("\x1b]8;;u\x07\x1b]8;;\x07", 5)          = ["\x1b]8;;u\x07\x1b]8;;\x07"]
```

`updateTrackerFromText(text, tracker)`（私有）：扫描 `indexOf("\x1b")`；可识别 → `tracker.process(substring)`；否则 `i++`；继续下一个 ESC。

#### 2.1.14 `applyBackgroundToLine(line, width, bgFn)`（导出）

```
const visibleLen = visibleWidth(line);
const paddingNeeded = Math.max(0, width - visibleLen);
return bgFn(line + " ".repeat(paddingNeeded));
```

实测：`applyBackgroundToLine("ab",5,t=>"["+t+"]") = "[ab   ]"`；`("界",3,…) = "[界 ]"`；`("abcdef",3,…) = "[abcdef]"`（不截断）。

#### 2.1.15 `sliceWithWidth(line, startCol, length, strict = false)` / `sliceByColumn`

```
sliceByColumn(line, startCol, length, strict=false) = sliceWithWidth(...).text

sliceWithWidth:
if (length <= 0) return {text:"", width:0};
const endCol = startCol + length;
let result="", resultWidth=0, currentCol=0, i=0, pendingAnsi="";
while (i < line.length) {
  const ansi = extractAnsiCode(line, i);
  if (ansi) {
     if (currentCol >= startCol && currentCol < endCol) { result += pendingAnsi + ansi.code; pendingAnsi = ""; }
     else if (currentCol < startCol) pendingAnsi += ansi.code;
     i += ansi.length; continue;
  }
  let textEnd = i; while (textEnd < line.length && !extractAnsiCode(line, textEnd)) textEnd++;
  for (const { segment } of graphemeSegmenter.segment(line.slice(i, textEnd))) {
     const w = graphemeWidth(segment);
     const inRange = currentCol >= startCol && currentCol < endCol;
     const fits = !strict || currentCol + w <= endCol;
     if (inRange && fits) { if (pendingAnsi) { result += pendingAnsi; pendingAnsi=""; } result += segment; resultWidth += w; }
     currentCol += w;
     if (currentCol >= endCol) break;
  }
  i = textEnd;
  if (currentCol >= endCol) break;
}
return {text: result, width: resultWidth};
```

**关键点**：

- 起始列之前的 ANSI 全部累积进 `pendingAnsi`，在第一个被保留的字符（或恰好落在区间内的 ANSI）前**按原顺序**输出 —— 这就是 issue #10169 的修复：`\x1b[32mfoo\x1b[39m bar` 从第 3 列切 4 列 → `"\x1b[32m\x1b[39m bar"`。
- `strict=false` 时宽字符可以溢出 `length`：`sliceWithWidth("界界界",0,3,false) = {text:"界界", width:4}`；`strict=true` → `{text:"界", width:2}`。
  更细的边界（实测）：`("界界界",0,1,false) = {text:"界", width:2}`（宽字素从 0 开始、0<1 命中且 `strict=false` → 收下）；`("界界界",0,1,true) = {"",0}`（`0+2 <= 1` 为假）；`("界界界",0,2,true) = {text:"界", width:2}`；`("界界界",1,2,true) = {"",0}`（从列 1 开始正好落在宽字素内部，`currentCol=0<1` 跳过该字素后 `currentCol=2`，再 `currentCol >= endCol(3)`？否；下一字素从 2 开始、`2+2<=3` 为假 → 空）；`("界界界",1,2,false) = {text:"界", width:2}`。
- 负 `startCol` 可用：`("abc",-1,2,false) = {text:"a",width:1}`。
- `startCol` 超长 → `{"",0}`。
- TAB 按其 grapheme 宽度 3 处理：`("\tX",0,2,false) = {text:"\t",width:3}`。

实测补充：

```
sliceWithWidth("abcdef",0,10,false)            = {text:"abcdef", width:6}
sliceWithWidth("\x1b[31m\x1b[0mabc",0,0,false) = {text:"", width:0}
sliceWithWidth("界a界",1,2,true)               = {text:"a", width:1}
sliceWithWidth("界a界",2,1,true)               = {text:"a", width:1}
sliceWithWidth("abcdef",2,100,true)            = {text:"cdef", width:4}
sliceWithWidth("\x1b[31mabc\x1b[0m",1,2,false) = {text:"\x1b[31mbc", width:2}
sliceWithWidth("\x1b[31mabc\x1b[0m",3,3,false) = {text:"\x1b[31m\x1b[0m", width:0}
```

#### 2.1.16 `extractSegments(line, beforeEnd, afterStart, afterLen, strictAfter = false)`

返回 `{ before, beforeWidth, after, afterWidth }`。使用模块级 `pooledStyleTracker`（每次调用先 `clear()`，**不可重入**）。

```
pooledStyleTracker.clear();
const afterEnd = afterStart + afterLen;
while (i < line.length) {
  const ansi = extractAnsiCode(line, i);
  if (ansi) {
     pooledStyleTracker.process(ansi.code);
     if (currentCol < beforeEnd) pendingAnsiBefore += ansi.code;
     else if (currentCol >= afterStart && currentCol < afterEnd && (afterStarted || ansi.code.startsWith("\x1b_")))
        after += ansi.code;
     i += ansi.length; continue;
  }
  let textEnd = i; while (textEnd < line.length && !extractAnsiCode(line, textEnd)) textEnd++;
  for (const { segment } of graphemeSegmenter.segment(line.slice(i, textEnd))) {
     const w = graphemeWidth(segment);
     if (currentCol < beforeEnd && currentCol + w <= beforeEnd) {
        if (pendingAnsiBefore) { before += pendingAnsiBefore; pendingAnsiBefore = ""; }
        before += segment; beforeWidth += w;
     } else if (currentCol >= afterStart && currentCol < afterEnd) {
        const fits = !strictAfter || currentCol + w <= afterEnd;
        if (fits) {
           if (!afterStarted) { after += pooledStyleTracker.getActiveCodes(); afterStarted = true; }
           after += segment; afterWidth += w;
        }
     }
     currentCol += w;
     if (afterLen <= 0 ? currentCol >= beforeEnd : currentCol >= afterEnd) break;
  }
  i = textEnd;
  if (afterLen <= 0 ? currentCol >= beforeEnd : currentCol >= afterEnd) break;
}
```

**关键点**：

- `before` 永远只收"完整落在 beforeEnd 之内"的 grapheme（宽字符跨界丢弃）。`("abcd让EFGH",5,9,11,true)` → `before="abcd"`（宽 4）。
- `after` 的第一段前会插入 `pooledStyleTracker.getActiveCodes()`（包含 OSC 8 打开），从而继承 overlay 之前的样式；之后只包含 `afterStarted` 之后出现的 SGR 与 APC。
- APC（`\x1b_…`）即使在 `after` 起点也会被保留（用于 `CURSOR_MARKER`）；普通 SGR 在起点被丢弃（因为已由 `getActiveCodes` 表达）。
- `afterLen <= 0` 时提前终止条件只看 `beforeEnd`。

实测：

```
("abcd让EFGH",5,9,11,true)   = {before:"abcd", beforeWidth:4, after:"H", afterWidth:1}
("abcdG EFGH",5,9,11,true)   = {before:"abcdG", beforeWidth:5, after:"H", afterWidth:1}
("out 192M\t.kiana/skill-tests/results-ha",10,13,10,true)
                             = {before:"out 192M", beforeWidth:8, after:"i/skill-te", afterWidth:10}
("out 192M\t.kiana/skill-tests/results-ha",11,13,10,true)
                             = {before:"out 192M\t", beforeWidth:11, after:"i/skill-te", afterWidth:10}
("\x1b[31mabcdef\x1b[0m",2,2,2,false) = {before:"\x1b[31mab", beforeWidth:2, after:"\x1b[31mcd", afterWidth:2}
("abcdef",2,2,0,false)       = {before:"ab", beforeWidth:2, after:"", afterWidth:0}
("abcdef",0,0,3,false)       = {before:"", beforeWidth:0, after:"abc", afterWidth:3}
("abcdef",3,1,3,true)        = {before:"abc", beforeWidth:3, after:"d", afterWidth:1}
("\x1b[31mab\x1b[0mcd",1,2,2,false) = {before:"\x1b[31ma", beforeWidth:1, after:"cd", afterWidth:2}
("\x1b_kiana:c\x07abc",0,1,2,false)    = {before:"", beforeWidth:0, after:"bc", afterWidth:2}
("abc\x1b_kiana:c\x07def",2,3,3,false) = {before:"ab", beforeWidth:2, after:"\x1b_kiana:c\x07def", afterWidth:3}
("界界界",0,1,4,true)        = {before:"", beforeWidth:0, after:"界", afterWidth:2}
("界界界",0,0,4,true)        = {before:"", beforeWidth:0, after:"界界", afterWidth:4}
```

#### 2.1.17 字符分类与 `flattenLines`

```ts
export function isWhitespaceChar(char: string): boolean { return /\s/.test(char); }
export function isPunctuationChar(char: string): boolean { return PUNCTUATION_REGEX.test(char); }
export function flattenLines(lines: readonly string[]): void { for (const line of lines) Number(line); }
```

- **均未锚定**：只要 `char` 内任意一处匹配即为 true（`isWhitespaceChar("a b") === true`）。Rust 必须用 `chars().any(...)`。
- JS `\s` 集合（逐字码点，**共 25 个**，已用 `for cp in 0..=0x10FFFF` 实测穷举确认）：U+0009 `\t`、U+000A `\n`、U+000B `\v`、U+000C `\f`、U+000D `\r`、U+0020 空格、U+00A0 NBSP、U+1680 OGHAM SPACE MARK、U+2000、U+2001、U+2002、U+2003、U+2004、U+2005、U+2006、U+2007、U+2008、U+2009、U+200A（EN QUAD … HAIR SPACE 逐个列出）、U+2028 LINE SEPARATOR、U+2029 PARAGRAPH SEPARATOR、U+202F NNBSP、U+205F MMSP、U+3000 IDEOGRAPHIC SPACE、U+FEFF BOM/ZWNBSP。
  **注意**：`\s` **不含** U+0085（NEL）、U+180E（MONGOLIAN VOWEL SEPARATOR，Unicode 6.3 起已非空白）、U+200B（ZWSP）。
  Rust `char::is_whitespace`（`White_Space` 属性）与之的差集：**多** U+0085、**少** U+FEFF；其余 23 个一致（U+180E 两边都 false）。
  **实测**（Rust 单测必须全部一致）：

  | 输入 | `isWhitespaceChar` |
  | --- | --- |
  | `" "`、`"\t"`、`"　"`(U+3000)、`"\u00A0"`、`"\n"`、`"\u2028"`、`"\u2029"`、`"\uFEFF"` | true |
  | `"a"` | false |
  | `"\u0085"`（NEL） | **false**（Rust `char::is_whitespace` 为 **true** → 不能直接替换） |
  | `"\u200B"`（ZWSP） | false |
  | `"a b"`（多字符，未锚定 → 任意一处命中） | true |
- `isPunctuationChar` 实测：ASCII 集合 31 个字符全 true；`"中"`/`"。"/"’"/"_"` false；`","`/`"-"` true。
- `flattenLines` 在 Rust 中是 **no-op**，但必须保留签名（V8 字符串扁平化优化；删除它会改变 JS 侧内存行为，Rust 无对应物）。

---

### 2.2 `src/fuzzy.ts`（159 行）— 模糊匹配

#### 导出

```ts
export interface FuzzyMatch { matches: boolean; score: number; }   // 分数越低越好
export function fuzzyMatch(query: string, text: string): FuzzyMatch;
export function fuzzyFilter<T>(items: T[], query: string, getText: (item: T) => string): T[];
```

#### `scorePositions(textLower, positions)`（私有）

```
let score = 0, lastMatchIndex = -1, consecutiveMatches = 0;
for (const i of positions) {
  if (lastMatchIndex === i - 1) { consecutiveMatches++; score -= consecutiveMatches * 5; }
  else { consecutiveMatches = 0; if (lastMatchIndex >= 0) score += (i - lastMatchIndex - 1) * 2; }
  if (i === 0 || /[\s\-_./:]/.test(textLower[i - 1]!)) score -= 10;   // 词边界奖励
  score += i * 0.1;                                                    // 越靠后越差
  lastMatchIndex = i;
}
return score;
```

注意：

- `positions` 是 **UTF-16 索引**；
- 词边界测试的是 `textLower[i-1]` **单个 UTF-16 码元**，边界集合逐字为 `[\s\-_./:]`（空格、连字符、下划线、点、斜杠、冒号）；
- `consecutiveMatches` 是连续命中长度：第 n 个连续命中扣 `5n`（即 5、10、15…）。

#### `matchQuery(normalizedQuery, textLower)`（私有）

1. 空 query → `{matches:true, score:0}`。
2. `normalizedQuery.length > textLower.length` → `{matches:false, score:0}`。
3. 贪心对齐：对每个 query 码元，从 `lastMatchIndex + 1` 起 `indexOf`；某码元找不到 → `{matches:false, score:0}`。
4. `score = scorePositions(textLower, positions)`。
5. **连续出现重打分**（issue #11220 修复）：对 `textLower.indexOf(normalizedQuery)` 的**每一次**出现（`start = indexOf(q, start+1)` 迭代），把 `positions` 原地覆写为 `[start, start+1, …, start+k-1]`（长度不变），`score = Math.min(score, scorePositions(textLower, positions))`。
6. `if (normalizedQuery === textLower) score -= 100;`
7. 返回 `{matches:true, score}`。

#### `fuzzyMatch(query, text)`

```
queryLower = query.toLowerCase(); textLower = text.toLowerCase();
primary = matchQuery(queryLower, textLower);
if (primary.matches) return primary;
alphaNumeric = queryLower.match(/^(?<letters>[a-z]+)(?<digits>[0-9]+)$/);
numericAlpha = queryLower.match(/^(?<digits>[0-9]+)(?<letters>[a-z]+)$/);
swapped = alphaNumeric ? digits+letters : numericAlpha ? letters+digits : "";
if (!swapped) return primary;
swappedMatch = matchQuery(swapped, textLower);
if (!swappedMatch.matches) return primary;
return { matches: true, score: swappedMatch.score + 5 };
```

注意：

- `toLowerCase()` 是 **ICU 全量小写**（`"İ".toLowerCase()` 产生 2 个码点等）；Rust 用 `to_lowercase()`（同样可扩展），但土耳其语 I 等特例需按 JS 默认 locale 语义（`toLowerCase()` 不带 locale）与 ICU 结果一致。见 §6.4。
- 交换匹配只识别 `[a-z]+[0-9]+` / `[0-9]+[a-z]+`（ASCII，且已经在 `queryLower` 上）。
- 交换匹配分数 **+5** 惩罚。

#### `fuzzyFilter(items, query, getText)`

```
if (!query.trim()) return items;                       // 原数组引用（同一顺序）
tokens = query.trim().split(/[\s/]+/).filter(t => t.length > 0);
if (tokens.length === 0) return items;
results = [];
for (const item of items) {
  const text = getText(item);
  let totalScore = 0, allMatch = true;
  for (const token of tokens) {
    const m = fuzzyMatch(token, text);
    if (m.matches) totalScore += m.score; else { allMatch = false; break; }
  }
  if (allMatch) results.push({ item, totalScore });
}
results.sort((a, b) => a.totalScore - b.totalScore);
return results.map(r => r.item);
```

注意：

- **多 token 是"全部必须命中"**，分数相加；token 之间用 `\s` 或 `/` 分隔（因此 `"openai-codex/gpt-5.5"` 会被拆成两个 token 去匹配 `"gpt-5.5 openai-codex"`）。
- `Array.prototype.sort` 在 V8 中是**稳定**的（ES2019 起要求稳定）。Rust 必须用稳定排序（`sort_by` 是稳定排序），且比较器只用 `totalScore`。
- 空白 query 返回**同一数组对象**（JS 引用相等；测试 `empty query returns all items unchanged` 用 `deepStrictEqual`，但调用方可能依赖引用）。Rust 侧返回 `Vec<T>` 克隆或 `Cow`/借用在语义上等价即可，但请保持顺序与长度。

#### 实测分数（逐位）

```
fuzzyMatch("", "anything")                                    = {matches:true,  score:0}
fuzzyMatch("longquery", "short")                              = {matches:false, score:0}
fuzzyMatch("test", "test")                                    = {matches:true,  score:-159.4}
fuzzyMatch("abc", "aXbXc")                                    = {matches:true,  score:-10.4}
fuzzyMatch("abc", "cba")                                      = {matches:false, score:0}
fuzzyMatch("ABC", "abc")                                      = {matches:true,  score:-139.7}
fuzzyMatch("abc", "ABC")                                      = {matches:true,  score:-139.7}
fuzzyMatch("foo", "foobar")                                   = {matches:true,  score:-39.699999999999996}
fuzzyMatch("foo", "f_o_o_bar")                                = {matches:true,  score:-30.400000000000002}
fuzzyMatch("fb", "foo-bar")                                   = {matches:true,  score:-18.6}
fuzzyMatch("fb", "afbx")                                      = {matches:true,  score:-4.7}
fuzzyMatch("opus", "openrouter openrouter/anthropic/claude-opus-4.1")
                                                              = {matches:true,  score:-23.8}
fuzzyMatch("opus", "openrouter openrouter/anthropic/claude-sonnet-4.5")
                                                              = {matches:true,  score:41.6}
fuzzyMatch("opus", "openrouter openrouter/openai/gpt-oss-120b")= {matches:true,  score:41.1}
fuzzyMatch("opus", "xxopus opus")                             = {matches:true,  score:-36.6}
fuzzyMatch("opus", "xxxxxx opus")                             = {matches:true,  score:-36.6}
fuzzyMatch("codex52", "gpt-5.2-codex")                        = {matches:true,  score:-64.99999999999999}
fuzzyMatch("astra", "radius radius/gpt-6-astra radius gpt-6-astra GPT 6 Astra")
                                                              = {matches:true,  score:-49.00000000000001}
fuzzyMatch("app", "a_p_p")                                    = {matches:true,  score:-30.400000000000002}
fuzzyMatch("app", "app")                                      = {matches:true,  score:-139.7}
fuzzyMatch("app", "application")                              = {matches:true,  score:-39.699999999999996}
fuzzyMatch("cl", "clone")                                     = {matches:true,  score:-24.9}
fuzzyMatch("cl", "cl")                                        = {matches:true,  score:-124.9}

fuzzyFilter(["a_p_p","app","application"], "app", x=>x)       = ["app","application","a_p_p"]
fuzzyFilter(["clone","cl"], "cl", x=>x)                       = ["cl","clone"]
fuzzyFilter(["a","b"], "", x=>x)                              = ["a","b"]
fuzzyFilter(["a","b"], "   ", x=>x)                           = ["a","b"]
fuzzyFilter(["a/b","ab","a b"], "a b", x=>x)                  = ["a/b","a b","ab"]
fuzzyFilter([...4 个 radius 模型...], "astra", x=>x)[0]        = "radius radius/gpt-6-astra radius gpt-6-astra GPT 6 Astra"
fuzzyFilter([{id:"gpt-5.5",provider:"openai-codex"}], "openai-codex/gpt-5.5", m=>`${m.id} ${m.provider}`)
                                                              = [该对象]
```

浮点细节：分数含 `i * 0.1` 的累加，必须逐位复刻 IEEE-754 double 累加顺序（`-39.699999999999996` 而非 `-39.7`）。Rust `f64` 与 JS `Number` 同为 IEEE-754 binary64，且加法顺序一致即可得到相同位模式。

---

### 2.3 `src/word-navigation.ts`（117 行）

```ts
export interface WordNavigationOptions {
  segment?: (text: string) => Iterable<Intl.SegmentData>;
  isAtomicSegment?: (segment: string) => boolean;
}
export function findWordBackward(text: string, cursor: number, options?: WordNavigationOptions): number;
export function findWordForward(text: string, cursor: number, options?: WordNavigationOptions): number;
```

模块级：`const wordSegmenter = getWordSegmenter();`（共享 `Intl.Segmenter` word 粒度）。

`Intl.SegmentData` = `{ segment: string; index: number; input: string; isWordLike?: boolean }`（`index` 为 UTF-16 偏移；`isWordLike` 仅 word 粒度存在）。

#### `findWordBackward(text, cursor, options?)`

```
if (cursor <= 0) return 0;
const textBeforeCursor = text.slice(0, cursor);
const segments = segmentFn ? [...segmentFn(textBeforeCursor)] : [...wordSegmenter.segment(textBeforeCursor)];
let newCursor = cursor;

// 1) 跳过尾部空白（原子段不算空白）；每轮重新取栈顶
const top = () => segments[segments.length - 1];
while (segments.length > 0
       && !isAtomic?.(top()?.segment || "")
       && isWhitespaceChar(top()?.segment || "")) {
  newCursor -= segments.pop()?.segment.length || 0;
}
if (segments.length === 0) return newCursor;

const last = segments[segments.length - 1]!;
if (isAtomic?.(last.segment)) {
  newCursor -= last.segment.length;                       // 整个原子段
} else if (last.isWordLike) {
  const segment = last.segment;
  const matches = [...segment.matchAll(new RegExp(PUNCTUATION_REGEX, "g"))];
  if (matches.length <= 0) newCursor -= segment.length;
  else {
    const lastMatch = matches[matches.length - 1]!;
    newCursor -= segment.length - (lastMatch.index + lastMatch[0].length);
  }
} else {
  // 非单词、非空白：连续标点段整体回退（每轮重新取栈顶 top()）
  while (segments.length > 0
         && !isAtomic?.(top()?.segment || "")
         && !top()?.isWordLike
         && !isWhitespaceChar(top()?.segment || "")) {
    newCursor -= segments.pop()?.segment.length || 0;
  }
}
return newCursor;
```

**"保留 ASCII 标点边界"的精确含义**：对 word-like 段，回退到**最后一个标点字符之后**的位置；若段内无标点，回退整段。即 `foo.bar` 的段 `"foo.bar"` 从光标 7 回退到 4（`. bar` 中 `.` 之后）。注意 `PUNCTUATION_REGEX` 无 `g` 标志，代码用 `new RegExp(PUNCTUATION_REGEX, "g")` 重新构造带 `g` 的版本 —— 因为源正则没有 `g`，`matchAll` 否则会抛错。

`findWordForward(text, cursor, options?)` 对称：

```
if (cursor >= text.length) return text.length;
const textAfterCursor = text.slice(cursor);
const segments = segmentFn ? segmentFn(textAfterCursor) : wordSegmenter.segment(textAfterCursor);
const it = segments[Symbol.iterator](); let next = it.next(); let newCursor = cursor;

// 1) 跳过前导空白
while (!next.done && !isAtomic?.(next.value.segment) && isWhitespaceChar(next.value.segment)) {
  newCursor += next.value.segment.length; next = it.next();
}
if (next.done) return newCursor;

if (isAtomic?.(next.value.segment)) {
  newCursor += next.value.segment.length;
} else if (next.value.isWordLike) {
  newCursor += PUNCTUATION_REGEX.exec(next.value.segment)?.index ?? next.value.segment.length;
} else {
  while (!next.done && !isAtomic?.(next.value.segment) && !next.value.isWordLike && !isWhitespaceChar(next.value.segment)) {
    newCursor += next.value.segment.length; next = it.next();
  }
}
return newCursor;
```

注意 forward 用 `PUNCTUATION_REGEX.exec(...)` 取**第一个**标点位置（无 `g`，每次都从头匹配，不保留 `lastIndex`）；无标点则整段。

#### 实测（word-navigation.test.ts 全部断言）

```
findWordBackward("hello world", 11) = 6;  (…, 6) = 0
findWordBackward("foo.bar", 7) = 4; (…,4) = 3; (…,3) = 0
findWordBackward("foo:bar", 7) = 4; (…,4) = 3; (…,3) = 0
findWordBackward("path/to/file", 12) = 8; (…,8) = 7; (…,7) = 5; (…,5) = 4; (…,4) = 0
findWordBackward("你好世界 test", 9) = 5; (…,5) = 2; (…,2) = 0   // 9 = text.length（源码用 text.length，不是字面量 7）
findWordBackward("  hello  ", 9) = 2; (…,2) = 0
findWordBackward("foo...bar", 9) = 6; (…,6) = 3; (…,3) = 0
findWordBackward("hello", 0) = 0
findWordForward("hello world", 0) = 5; (…,5) = 11
findWordForward("foo.bar", 0) = 3; (…,3) = 4; (…,4) = 7
findWordForward("foo:bar", 0) = 3; (…,3) = 4; (…,4) = 7
findWordForward("path/to/file", 0) = 4; (…,4) = 5; (…,5) = 7; (…,7) = 8; (…,8) = 12
findWordForward("你好世界 test", 0) ∈ (0,4]（实测 = 2），逐段走到 9（= text.length）
findWordForward("  hello  ", 0) = 7; (…,7) = 9
findWordForward("foo...bar", 0) = 3; (…,3) = 6; (…,6) = 9
findWordForward("hello", 5) = 5
```

原子段测试（`marker = "[paste #1 +5 lines]"`，`text = "hello [paste #1 +5 lines] world"`，`segment` 由测试自带映射表提供）：

```
findWordBackward(text, 31, opts) = 26    // 先跳过尾部 "world"
findWordBackward(text, 26, opts) = 6     // 跳过空格后整个 marker 作为单位
findWordForward(text, 6, opts) = 25    // = 6 + marker.length，marker = "[paste #1 +5 lines]" 长 19（不是 22）
```

要点：原子段判定优先于空白/单词判定；被 `isAtomicSegment` 命中的段即使用户自定义 `segment` 返回的 `isWordLike` 不同也按整段处理。

**跨单元契约**：`components/editor.ts` 传入
`{ segment: (t) => segmentWithMarkers(t, wordSegmenter, validPasteIds), isAtomicSegment: isPasteMarker }`。
因此 `segment` 的 `index`/`input` 字段与 `isWordLike` 必须来自**该自定义分段器**，且 `segment` 字符串长度是 UTF-16 长度。

---

### 2.4 `src/kill-ring.ts`（46 行）

```ts
export class KillRing {
  private ring: string[] = [];
  push(text: string, opts: { prepend: boolean; accumulate?: boolean }): void;
  peek(): string | undefined;
  rotate(): void;
  get length(): number;
}
```

- `push`：`if (!text) return;`（空串不产生条目）。若 `opts.accumulate && ring.length > 0` → `last = ring.pop()!; ring.push(opts.prepend ? text + last : last + text);`，否则 `ring.push(text)`。
- `peek`：`ring.length > 0 ? ring[ring.length - 1] : undefined`。
- `rotate`：`if (ring.length > 1) { const last = ring.pop()!; ring.unshift(last); }`（"最后一个移到最前"）。
- `length`：条目数。

注意：**没有上限**（无 max size）；`rotate` 在长度 ≤ 1 时是 no-op；累积时是"替换栈顶"而不是新增条目（因此 `length` 不增）。

---

### 2.5 `src/undo-stack.ts`（28 行）

```ts
export class UndoStack<S> {
  private stack: S[] = [];
  push(state: S): void { this.stack.push(structuredClone(state)); }
  pop(): S | undefined { return this.stack.pop(); }
  clear(): void { this.stack.length = 0; }
  get length(): number { return this.stack.length; }
}
```

要点：

- **push 时深拷贝**（`structuredClone`），pop 时**不再拷贝**。
- `structuredClone` 语义：支持基本类型、数组、普通对象、Map/Set、Date、RegExp、ArrayBuffer/类型化数组；遇到函数/`WeakMap`/`WeakSet`/DOM 节点会抛 `DataCloneError`。本库只用 `{ value: string, cursor: number }` 与 `EditorSnapshot`（普通对象 + 数组 + 字符串/数字/布尔），Rust 只需 `Clone`。
- `clear()` 不改引用（原地清空）；`length` 是 getter。
- 无上限。

---

### 2.6 `src/editor-component.ts`（74 行）

```ts
export interface EditorComponent extends Component {
  getText(): string;
  setText(text: string): void;
  handleInput(data: string): void;

  onSubmit?: (text: string) => void;
  onChange?: (text: string) => void;

  addToHistory?(text: string): void;

  insertTextAtCursor?(text: string): void;
  getExpandedText?(): string;

  setAutocompleteProvider?(provider: AutocompleteProvider): void;

  borderColor?: (str: string) => string;
  setPaddingX?(padding: number): void;
  setAutocompleteMaxVisible?(maxVisible: number): void;
}
```

要点：

- 继承 `Component`（`render(width: number): string[]`、`invalidate(): void`）。
- 可选成员在 Rust 中映射为 `Option<Box<dyn Fn...>>` / 默认空实现（`addToHistory` 默认 no-op 会改变语义：TS 里 `?:` 缺失表示"不支持"，调用方用 `editor.getExpandedText?.() ?? editor.getText()` 之类判断。Rust 必须能**区分"未提供"与"提供了空实现"**，用 `Option` 表达。）
- 消费者（coding-agent）用 `setAutocompleteProvider`、`setPaddingX`、`setAutocompleteMaxVisible`、`getExpandedText`、`borderColor`；`interactive-mode.ts` 在自定义 editor 上逐一探测 `typeof editor.setPaddingX === "function"`。

---

### 2.7 `src/colors.ts`（367 行）

#### 类型（逐字）

```ts
export interface IndexedColor    { readonly kind: "indexed"; readonly index: number; }
export interface RgbColorValue   { readonly kind: "rgb";     readonly r: number; readonly g: number; readonly b: number; }
export interface OklchColorValue { readonly kind: "oklch";   readonly l: number; readonly c: number; readonly h: number; }
export type Color = IndexedColor | RgbColorValue | OklchColorValue;
export type TerminalColorMode = "256color" | "truecolor";
export type ColorMixSpace = "oklch" | "srgb";
export interface OklchChannels { l: number; c: number; h: number; }
export interface OkhslChannels { h: number; s: number; l: number; }
export interface TextAttributes { bold?: boolean; dim?: boolean; italic?: boolean; underline?: boolean; inverse?: boolean; strikethrough?: boolean; }
export interface TextStyle extends TextAttributes { fg?: Color; bg?: Color; }
```

`Color` 是 **discriminated union on `kind`**；Rust 用 `enum Color { Indexed{index:u8}, Rgb{r:f64,g:f64,b:f64}, Oklch{l:f64,c:f64,h:f64} }`（注意 `r/g/b` 在 TS 是 `number`，构造时校验 0–255 但**可为小数**，例如 `mixColors(..., "srgb")` 产生 `127.5`）。

#### 校验与构造

```ts
function requireFinite(value: number, name: string): void {
  if (!Number.isFinite(value)) throw new Error(`${name} must be finite`);
}
export function indexedColor(index: number): IndexedColor {
  if (!Number.isInteger(index) || index < 0 || index > 255)
    throw new Error(`ANSI color index must be an integer from 0 to 255: ${index}`);
  return Object.freeze({ kind: "indexed", index });
}
export function rgbColor(r, g, b): RgbColorValue {
  for (const [name, value] of [["r",r],["g",g],["b",b]] as const) {
    requireFinite(value, name);
    if (value < 0 || value > 255) throw new Error(`${name} must be between 0 and 255: ${value}`);
  }
  return Object.freeze({ kind: "rgb", r, g, b });
}
export function oklchColor(l, c, h): OklchColorValue {
  requireFinite(l, "l"); requireFinite(c, "c"); requireFinite(h, "h");
  if (l < 0 || l > 1) throw new Error(`l must be between 0 and 1: ${l}`);
  if (c < 0) throw new Error(`c must not be negative: ${c}`);
  return Object.freeze({ kind: "oklch", l, c, h: ((h % 360) + 360) % 360 });
}
export function okhslColor(h, s, l): RgbColorValue {
  requireFinite(h, "h"); requireFinite(s, "s"); requireFinite(l, "l");
  if (s < 0 || s > 1) throw new Error(`s must be between 0 and 1: ${s}`);
  if (l < 0 || l > 1) throw new Error(`l must be between 0 and 1: ${l}`);
  const { r, g, b } = okhslToRgb(h, s, l);
  return rgbColor(r, g, b);
}
```

**【逐字】错误文案**（测试用正则匹配）：

```
ANSI color index must be an integer from 0 to 255: ${index}
${name} must be finite                         // name ∈ {"r","g","b","l","c","h","s","amount"}
${name} must be between 0 and 255: ${value}     // r/g/b
l must be between 0 and 1: ${l}
c must not be negative: ${c}
s must be between 0 and 1: ${s}
amount must be between 0 and 1: ${amount}
Invalid color value: ${value}
```

数字插值必须是 JS `Number.prototype.toString()` 语义（`-0.1`→`"-0.1"`、`300`→`"300"`、`1.5`→`"1.5"`、`1e21`→`"1e+21"`、`NaN`→`"NaN"`）。见 §3.1 末尾的 ECMAScript Number::toString 段。

实测：

```
indexedColor(-1)          → "ANSI color index must be an integer from 0 to 255: -1"
indexedColor(256)         → "…: 256"
parseColor(300)           → "…: 300"
parseColor(1.5)           → "…: 1.5"
rgbColor(NaN,0,0)         → "r must be finite"
rgbColor(0,256,0)         → "g must be between 0 and 255: 256"
rgbColor(0,0,-1)          → "b must be between 0 and 255: -1"
oklchColor(-0.1,0,0)      → "l must be between 0 and 1: -0.1"
oklchColor(1.1,0,0)       → "l must be between 0 and 1: 1.1"
oklchColor(0.5,-1,0)      → "c must not be negative: -1"
oklchColor(0.5,0,Infinity)→ "h must be finite"
okhslColor(0,2,0.5)       → "s must be between 0 and 1: 2"
okhslColor(0,0.5,-0.1)    → "l must be between 0 and 1: -0.1"
okhslColor(0,0.5,1.1)     → "l must be between 0 and 1: 1.1"
okhslColor(0,NaN,1)       → "s must be finite"
mixColors(...,1.5)        → "amount must be between 0 and 1: 1.5"
mixColors(...,NaN)        → "amount must be finite"
parseColor("")            → "Invalid color value: "
parseColor("red")         → "Invalid color value: red"
parseColor("#abcd")       → "Invalid color value: #abcd"
```

注意：`okhslColor` 的校验顺序是 **h → s → l**，且 `s` 检查先于 `l`；`rgbColor` 顺序 r→g→b（先 finite 再范围，逐通道）。

#### `parseColor(value: string | number)`

```
if (typeof value === "number") return indexedColor(value);

const hex = /^#([\da-f]{3}|[\da-f]{6})$/i.exec(value);
if (hex) {
  const digits = hex[1].length === 3 ? [...hex[1]].map(d => d + d).join("") : hex[1];
  return rgbColor(parseInt(digits.slice(0,2),16), parseInt(digits.slice(2,4),16), parseInt(digits.slice(4,6),16));
}

const oklch = OKLCH_PATTERN.exec(value);
if (oklch) {
  const lightness = Number.parseFloat(oklch[1]) / (oklch[2] ? 100 : 1);
  return oklchColor(lightness, Number.parseFloat(oklch[3]), Number.parseFloat(oklch[4]));
}

const okhsl = OKHSL_PATTERN.exec(value);
if (okhsl) {
  const saturation = Number.parseFloat(okhsl[2]) / (okhsl[3] ? 100 : 1);
  const lightness  = Number.parseFloat(okhsl[4]) / (okhsl[5] ? 100 : 1);
  return okhslColor(Number.parseFloat(okhsl[1]), saturation, lightness);
}
throw new Error(`Invalid color value: ${value}`);
```

【逐字】数值子模式与两个正则：

```ts
const NUMBER_PATTERN = String.raw`[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?`;
const OKLCH_PATTERN = new RegExp(
  `^oklch\\(\\s*(${NUMBER_PATTERN})(%)?\\s+(${NUMBER_PATTERN})\\s+(${NUMBER_PATTERN})(?:deg)?\\s*\\)$`, "i");
const OKHSL_PATTERN = new RegExp(
  `^okhsl\\(\\s*(${NUMBER_PATTERN})(?:deg)?\\s+(${NUMBER_PATTERN})(%)?\\s+(${NUMBER_PATTERN})(%)?\\s*\\)$`, "i");
```

要点：

- hex 仅接受 3 位或 6 位（`#abcd` 不匹配）；大小写不敏感；3 位按位复制（`#abc` → `aa bb cc` → 170/187/204）。
- OKLCH：第 1 组是 L，可带 `%`（除以 100）；第 3、4 组是 C、H（H 可带 `deg`）。**`oklch(50 0.1 400deg)` 中 `50` 无 `%` → l=50 → 抛 `l must be between 0 and 1: 50`**。
- OKHSL：H 可带 `deg`；S 带 `%` 则 /100；L 带 `%` 则 /100。
- 参数之间必须是**一个或多个空白**（`\s+`）；允许 `\s*` 在括号内侧。
- `1e1%` 被接受（`NUMBER_PATTERN` 支持指数）→ l = 10/100 = 0.1。
- 解析用 `Number.parseFloat`，其兼容行为与 JS 数字字面量不同（`parseFloat("1e1")` = 10），但正则已确保输入形态。
- `parseColor("OKLCH(.5 .1 400)")` → `{l:0.5,c:0.1,h:40}`（h 归一化到 [0,360)）。

实测：

```
parseColor("#abc")               = {kind:"rgb", r:170, g:187, b:204}
parseColor("#AABBCC")            = {kind:"rgb", r:170, g:187, b:204}
parseColor("oklch(62% 0.1 200)") = {kind:"oklch", l:0.62, c:0.1, h:200}
parseColor("OKLCH(.5 .1 400)")   = {kind:"oklch", l:0.5, c:0.1, h:40}
parseColor("okhsl(250 60% 55%)") = {kind:"rgb", r:78, g:136, b:194}
parseColor("okhsl(250deg 60% 55%)") = {kind:"rgb", r:78, g:136, b:194}
parseColor("okhsl(250 0.6 0.55)")   = {kind:"rgb", r:78, g:136, b:194}
parseColor("okhsl(250 160% 55%)")   → "s must be between 0 and 1: 1.6"
parseColor("okhsl(29.23 100% 56.8%)") = rgbColor(255,0,0)
parseColor("OKHSL(250deg 60% 55%)") = okhslColor(250,0.6,0.55)
parseColor(9)                    = {kind:"indexed", index:9}
```

#### `indexedToRgb(index)` 与 256 色基表

```ts
const BASIC_COLORS = [[0,0,0],[128,0,0],[0,128,0],[128,128,0],[0,0,128],[128,0,128],[0,128,128],[192,192,192],
                      [128,128,128],[255,0,0],[0,255,0],[255,255,0],[0,0,255],[255,0,255],[0,255,255],[255,255,255]];
const CUBE_VALUES = [0, 95, 135, 175, 215, 255];
const GRAY_VALUES = Array.from({length:24}, (_, i) => 8 + i * 10);   // 8,18,…,238

function indexedToRgb(index) {
  if (index < 16) return { ...BASIC_COLORS[index] };
  if (index < 232) {
    const cubeIndex = index - 16;
    return { r: CUBE_VALUES[Math.floor(cubeIndex/36)], g: CUBE_VALUES[Math.floor((cubeIndex%36)/6)], b: CUBE_VALUES[cubeIndex%6] };
  }
  const gray = 8 + (index - 232) * 10;
  return { r: gray, g: gray, b: gray };
}
```

实测：idx 0→(0,0,0)、1→(128,0,0)、7→(192,192,192)、8→(128,128,128)、9→(255,0,0)、15→(255,255,255)、16→(0,0,0)、17→(0,0,95)、18→(0,0,135)、21→(0,0,255)、22→(0,95,0)、51→(0,255,255)、196→(255,0,0)、231→(255,255,255)、232→(8,8,8)、233→(18,18,18)、255→(238,238,238)。

#### `oklchToRgb`（色域映射，见 §2.8 的 `oklabToLinearSrgb`）

```ts
function isInSrgbGamut(linear: number[]): boolean {
  const epsilon = 1e-7;
  return linear.every(channel => channel >= -epsilon && channel <= 1 + epsilon);
}
function oklchToRgb({ l, c, h }) {
  const radians = (h * Math.KIANA) / 180;
  const cos = Math.cos(radians), sin = Math.sin(radians);
  const atChroma = (chroma) => oklabToLinearSrgb([l, chroma * cos, chroma * sin]);
  const direct = atChroma(c);
  if (isInSrgbGamut(direct)) return linearSrgbToRgb(direct);
  let linear = atChroma(0), low = 0, high = c;
  for (let index = 0; index < 20; index++) {
    const chroma = (low + high) / 2;
    const candidate = atChroma(chroma);
    if (isInSrgbGamut(candidate)) { low = chroma; linear = candidate; } else high = chroma;
  }
  return linearSrgbToRgb(linear);
}
```

要点：`h` 已由 `oklchColor` 归一化；**20 次**二分；`atChroma(0)` 作为兜底（保证 `oklch(100% 0.3 150)` → 白、`oklch(0 0.3 150)` → 黑）。

实测：`oklchColor(0.627955,0.257683,29.2339)` → `{255,0,0}`；`(1,0.3,150)` → `{255,255,255}`；`(0,0.3,150)` → `{0,0,0}`。

#### `colorToRgb` / `colorToOklch` / `colorToOkhsl` / `colorToHex`

```ts
export function colorToRgb(color: Color): RgbColor {
  switch (color.kind) {
    case "indexed": return indexedToRgb(color.index);
    case "rgb":     return { r: color.r, g: color.g, b: color.b };
    case "oklch":   return oklchToRgb(color);
  }
}
export function colorToOklch(color: Color): OklchChannels {
  if (color.kind === "oklch") return { l: color.l, c: color.c, h: color.h };
  const [l, a, b] = rgbToOklab(colorToRgb(color));
  return { l, c: Math.hypot(a, b), h: ((Math.atan2(b, a) * 180) / Math.KIANA + 360) % 360 };
}
export function colorToOkhsl(color: Color): OkhslChannels { return rgbToOkhsl(colorToRgb(color)); }
export function colorToHex(color: Color): string {
  const { r, g, b } = colorToRgb(color);
  const channel = (value: number) => Math.round(value).toString(16).padStart(2, "0");
  return `#${channel(r)}${channel(g)}${channel(b)}`;
}
```

要点：

- `indexed`/`rgb` 转 OKLCH 时 `h` 用 `((atan2(b,a)*180/KIANA + 360) % 360)`；`indexed` 走 `colorToRgb` 再 `rgbToOklab`。
- `colorToHex`：`Math.round` 后 `toString(16)` 再 `padStart(2,"0")`（**不截断**，`>255` 会产出 3 位；正常输入不会）；小写十六进制。
- `colorToOkhsl` 返回 `{h,s,l}`（注意字段顺序与 `OklchChannels` 不同）。

实测：`colorToOklch(rgbColor(255,0,0))` = `{l:0.6279553639214309, c:0.2576833038053606, h:29.23388027962784}`；`colorToHex(parseColor("#4f8eb3"))` = `"#4f8eb3"`；`colorToOkhsl(parseColor("#4f8eb3"))` = `{h:236.19717656525768, s:0.540661618165528, l:0.5584624058913696}`；`("#20242a")` = `{h:258.37244185419917, s:0.12562894901938743, l:0.15552440397159636}`；`("#f8f9fa")` = `{h:247.83945362134128, s:0.12427292617023489, l:0.9786102401903075}`；三者 `okhslColor(h,s,l)` 回 `colorToHex` 均得到原 hex。

#### `mixColors(first, second, amount, space = "oklch")`

```ts
requireFinite(amount, "amount");
if (amount < 0 || amount > 1) throw new Error(`amount must be between 0 and 1: ${amount}`);
if (space === "srgb") {
  const a = colorToRgb(first), b = colorToRgb(second);
  return rgbColor(a.r + (b.r - a.r)*amount, a.g + (b.g - a.g)*amount, a.b + (b.b - a.b)*amount);
}
const a = colorToOklch(first), b = colorToOklch(second);
const firstHue  = a.c < 1e-7 ? b.h : a.h;
const secondHue = b.c < 1e-7 ? firstHue : b.h;
const hueDelta  = ((secondHue - firstHue + 540) % 360) - 180;
return oklchColor(a.l + (b.l - a.l)*amount, a.c + (b.c - a.c)*amount, firstHue + hueDelta*amount);
```

要点：

- **无彩色（c < 1e-7）时借用对方色相**（避免灰色混色产生随机色相）。
- 色相差取**最短路径**（`((d + 540) % 360) - 180` ∈ [-180,180)）。
- 返回 `oklchColor(...)`（因此做了 `h` 归一化与校验）；`srgb` 分支返回 `rgbColor(...)`（可能得到小数通道）。
- 实测：`mixColors(rgbColor(0,0,0), rgbColor(255,255,255), 0.5, "srgb")` = `{kind:"rgb", r:127.5, g:127.5, b:127.5}`；`(…, 0.5)` 默认 oklch = `{kind:"oklch", l:0.49999999999999983, c:2.498001805406602e-16, h:180}`。

#### 256 色量化与 ANSI 生成

```ts
function findClosest(values: readonly number[], target: number): number {
  let closestIndex = 0, closestDistance = Infinity;
  for (let index = 0; index < values.length; index++) {
    const distance = Math.abs(target - values[index]);
    if (distance < closestDistance) { closestIndex = index; closestDistance = distance; }
  }
  return closestIndex;                     // 平局取**先出现**的
}
function colorDistance(first, second): number {
  const dr = first.r - second.r, dg = first.g - second.g, db = first.b - second.b;
  return dr*dr*0.299 + dg*dg*0.587 + db*db*0.114;
}
function rgbToAnsi256(color: RgbColor): number {
  const rIndex = findClosest(CUBE_VALUES, color.r), gIndex = findClosest(CUBE_VALUES, color.g), bIndex = findClosest(CUBE_VALUES, color.b);
  const cubeColor = { r: CUBE_VALUES[rIndex], g: CUBE_VALUES[gIndex], b: CUBE_VALUES[bIndex] };
  const cubeIndex = 16 + 36*rIndex + 6*gIndex + bIndex;
  const gray = Math.round(0.299*color.r + 0.587*color.g + 0.114*color.b);
  const grayOffset = findClosest(GRAY_VALUES, gray);
  const grayValue = GRAY_VALUES[grayOffset];
  const spread = Math.max(color.r, color.g, color.b) - Math.min(color.r, color.g, color.b);
  if (spread < 10 && colorDistance(color, {r:grayValue,g:grayValue,b:grayValue}) < colorDistance(color, cubeColor)) return 232 + grayOffset;
  return cubeIndex;
}
function colorAnsi(color: Color, mode: TerminalColorMode, background: boolean): string {
  if (color.kind === "indexed") return `\x1b[${background ? 48 : 38};5;${color.index}m`;
  const rgb = colorToRgb(color);
  if (mode === "truecolor") return `\x1b[${background ? 48 : 38};2;${Math.round(rgb.r)};${Math.round(rgb.g)};${Math.round(rgb.b)}m`;
  return `\x1b[${background ? 48 : 38};5;${rgbToAnsi256(rgb)}m`;
}
export function foregroundAnsi(color, mode) { return colorAnsi(color, mode, false); }
export function backgroundAnsi(color, mode) { return colorAnsi(color, mode, true); }
```

要点：

- `indexed` 颜色**永不**量化：始终 `38;5;N` / `48;5;N`，与 mode 无关。
- `truecolor` 用 `Math.round` 后的整数。
- 灰色判定：`spread < 10` **且** 灰候选比立方体候选更近；比较用加权平方距离。
- 平局（`distance < closestDistance` 严格小于）取**较小索引**。
- `Math.round` 是"floor(x + 0.5)"（.5 向 +∞，`-0.5 → -0`）；Rust `f64::round` 是远离零舍入（`-2.5 → -3`，**不同**），见 §6.2。

实测：

```
foregroundAnsi(rgbColor(18,52,86), "truecolor") = "\x1b[38;2;18;52;86m"
backgroundAnsi(rgbColor(18,52,86), "truecolor") = "\x1b[48;2;18;52;86m"
foregroundAnsi(rgbColor(18,52,86), "256color")  = "\x1b[38;5;23m"
foregroundAnsi(indexedColor(9), "truecolor")    = "\x1b[38;5;9m"
foregroundAnsi(parseColor("oklch(62% 0.1 200)"), "truecolor") = "\x1b[38;2;28;152;158m"
foregroundAnsi(parseColor("oklch(62% 0.1 200)"), "256color")  = "\x1b[38;5;31m"

量化样本（foregroundAnsi(...,"256color")）：
(0,0,0)→16；(255,255,255)→231；(128,128,128)→244；(18,52,86)→23；(200,10,10)→160；(10,200,10)→40；
(10,10,200)→20；(100,100,100)→241；(130,130,130)→244；(95,95,95)→59；(8,8,8)→232；(238,238,238)→255；
(255,0,0)→196；(0,255,0)→46；(0,0,255)→21；(1,2,3)→16
```

#### `styleText` / `styleTextWithAnsi`

```ts
export function styleText(text, options, mode): string {
  return styleTextWithAnsi(
    text,
    options.fg && foregroundAnsi(options.fg, mode),
    options.bg && backgroundAnsi(options.bg, mode),
    options,
  );
}

export function styleTextWithAnsi(text, fgAnsi, bgAnsi, options): string {
  let prefix = "", suffix = "";
  if (fgAnsi) { prefix += fgAnsi; suffix = "\x1b[39m"; }
  if (bgAnsi) { prefix += bgAnsi; suffix = `\x1b[49m${suffix}`; }
  if (options.bold) prefix += "\x1b[1m";
  if (options.dim)  prefix += "\x1b[2m";
  if (options.bold || options.dim) suffix = `\x1b[22m${suffix}`;
  if (options.italic) { prefix += "\x1b[3m"; suffix = `\x1b[23m${suffix}`; }
  if (options.underline) { prefix += "\x1b[4m"; suffix = `\x1b[24m${suffix}`; }
  if (options.inverse) { prefix += "\x1b[7m"; suffix = `\x1b[27m${suffix}`; }
  if (options.strikethrough) { prefix += "\x1b[9m"; suffix = `\x1b[29m${suffix}`; }
  return `${prefix}${text}${suffix}`;
}
```

要点：

- 打开顺序：fg、bg、bold、dim、italic、underline、inverse、strikethrough。
- 后缀是**逆序**：strikethrough(29)、inverse(27)、underline(24)、italic(23)、bold/dim(22)、bg(49)、fg(39)（通过**前置插入**实现，所以最终顺序是 29,27,24,23,22,49,39）。
- `styleTextWithAnsi` 忽略 `options.fg`/`options.bg`（签名只接受 `TextAttributes`）。
- `options.fg && …`：`fg` 为 `undefined` 时为 `undefined`（`bgAnsi` 参数类型 `string | undefined`）。

实测：

```
styleText("Ready", {fg:rgbColor(18,52,86), bg:indexedColor(9), bold:true, italic:true}, "truecolor")
  = "\x1b[38;2;18;52;86m\x1b[48;5;9m\x1b[1m\x1b[3mReady\x1b[23m\x1b[22m\x1b[49m\x1b[39m"
styleText("Ready", {fg:rgbColor(18,52,86)}, "256color") = "\x1b[38;5;23mReady\x1b[39m"
styleText("T", {dim:true,underline:true,inverse:true,strikethrough:true}, "truecolor")
  = "\x1b[2m\x1b[4m\x1b[7m\x1b[9mT\x1b[29m\x1b[27m\x1b[24m\x1b[22m"
styleText("T", {}, "truecolor") = "T"
styleTextWithAnsi("T","\x1b[31m","\x1b[44m",{bold:true}) = "\x1b[31m\x1b[44m\x1b[1mT\x1b[22m\x1b[49m\x1b[39m"
styleTextWithAnsi("T",undefined,undefined,{}) = "T"
styleTextWithAnsi("T","A","B",{fg:rgbColor(1,2,3)}) = "ABT\x1b[49m\x1b[39m"     // fg/bg 被忽略，仅作 ANSI 片段
```

---

### 2.8 `src/oklab.ts`（233 行）— Oklab / OKHSL ↔ sRGB

Björn Ottosson 参考实现的移植（MIT，见源码头注释）。全部导出：

```ts
export const oklabToOkhslLightness = (x: number): number;
export function oklabToLinearSrgb(lab: Vector): Vector;     // Vector = [number, number, number]
export function rgbToOklab({ r, g, b }: RgbColor): Vector;
export function linearSrgbToRgb(linear: Vector): RgbColor;   // 0-255 取整并裁剪
export function okhslToRgb(hue: number, saturation: number, lightness: number): RgbColor;
export function rgbToOkhsl(rgb: RgbColor): { h: number; s: number; l: number };
```

#### 常量（**逐字抄录**，不可"整理"或改精度）

```ts
const LINEAR_SRGB_TO_LMS: Matrix = [
  [0.4122214694707629, 0.5363325372617349, 0.0514459932675022],
  [0.2119034958178251, 0.6806995506452344, 0.1073969535369405],
  [0.0883024591900564, 0.2817188391361215, 0.6299787016738222],
];
const LMS_TO_LAB: Matrix = [
  [0.210454268309314, 0.793617774702305, -0.0040720430116193],
  [1.9779985324311684, -2.42859224204858, 0.450593709617411],
  [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
];
const LAB_TO_LMS: Matrix = [
  [1, 0.3963377773761749, 0.2158037573099136],
  [1, -0.1055613458156586, -0.0638541728258133],
  [1, -0.0894841775298119, -1.2914855480194092],
];
const LMS_TO_LINEAR_SRGB: Matrix = [
  [4.0767416360759583, -3.3077115392580629, 0.2309699031821043],
  [-1.2684379732850315, 2.6097573492876882, -0.341319376002657],
  [-0.0041960761386756, -0.7034186179359362, 1.7076146940746117],
];
const SATURATION_FIT: [[number, number], number[]][] = [
  [[-1.8817031, -0.80936501], [1.19086277, 1.76576728, 0.59662641, 0.75515197, 0.56771245]],
  [[1.8144408, -1.19445267],  [0.73956515, -0.45954404, 0.08285427, 0.12541073, -0.14503204]],
  [[0.13110758, 1.81333971],  [1.35733652, -0.00915799, -1.1513021, -0.50559606, 0.00692167]],
];
const K1 = 0.206, K2 = 0.03, K3 = (1 + K1) / (1 + K2);
```

`multiply(m, [x,y,z])` = `m.map(row => row[0]*x + row[1]*y + row[2]*z)`（行主序）。

#### 亮度与传递函数

```ts
export const oklabToOkhslLightness = (x) => 0.5 * (K3*x - K1 + Math.sqrt((K3*x - K1)**2 + 4*K2*K3*x));
const okhslToOklabLightness = (x) => (x*x + K1*x) / (K3 * (x + K2));
const linearToSrgb = (v) => v > 0.0031308 ? 1.055 * v**(1/2.4) - 0.055 : 12.92 * v;
const srgbToLinear = (v) => v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
```

实测：`oklabToOkhslLightness(0)=0`、`(1)=1`、`(0.5)=0.42114056260896976`。

#### 转换

```ts
export function oklabToLinearSrgb(lab) {
  return multiply(LMS_TO_LINEAR_SRGB, multiply(LAB_TO_LMS, lab).map(v => v ** 3));
}
function linearSrgbToOklab(rgb) {
  return multiply(LMS_TO_LAB, multiply(LINEAR_SRGB_TO_LMS, rgb).map(Math.cbrt));
}
export function rgbToOklab({r,g,b}) {
  return linearSrgbToOklab([r/255, g/255, b/255].map(srgbToLinear));
}
export function linearSrgbToRgb(linear) {
  const [r,g,b] = linear.map(v => Math.round(Math.min(1, Math.max(0, linearToSrgb(v))) * 255));
  return { r, g, b };
}
```

要点：

- 立方 vs 立方根：`v ** 3`；`Math.cbrt`（**负数的实立方根**，与 `Math.pow(v, 1/3)` 不同！）。
- 裁剪顺序：先 `linearToSrgb`，再 `clamp(0,1)`，再 `*255`，再 `Math.round`。
- `Math.round` 半值向 +∞（`Math.round(-0.5) === -0`）。

#### `lmsSlopes` / `maxSaturation` / `cusp` / `maxChroma` / `chromaStops`

```ts
function lmsSlopes(a, b) { return [LAB_TO_LMS[0], LAB_TO_LMS[1], LAB_TO_LMS[2]].map(row => row[1]*a + row[2]*b); }

function maxSaturation(a, b) {
  const channel = SATURATION_FIT.findIndex(([[x, y]], index) => index === 2 || x*a + y*b > 1);
  const [k0,k1,k2,k3,k4] = SATURATION_FIT[channel][1];
  const weights = LMS_TO_LINEAR_SRGB[channel];
  const saturation = k0 + k1*a + k2*b + k3*a*a + k4*a*b;
  const slopes = lmsSlopes(a, b);
  const base = slopes.map(k => 1 + saturation * k);
  const dot = (values) => values.reduce((sum, v, i) => sum + weights[i]*v, 0);
  const f  = dot(base.map(v => v ** 3));
  const f1 = dot(base.map((v, i) => 3 * slopes[i] * v ** 2));
  const f2 = dot(base.map((v, i) => 6 * slopes[i]**2 * v));
  return saturation - (f * f1) / (f1*f1 - 0.5 * f * f2);      // 一步 Halley
}

function cusp(a, b) {
  const saturation = maxSaturation(a, b);
  const lightness = Math.cbrt(1 / Math.max(...oklabToLinearSrgb([1, saturation*a, saturation*b])));
  return [lightness, lightness * saturation];
}

function maxChroma(a, b, lightness, [cuspL, cuspC]) {
  if (lightness <= cuspL) return (cuspC * lightness) / cuspL;
  const t = (cuspC * (lightness - 1)) / (cuspL - 1);
  const slopes = lmsSlopes(a, b);
  const lms = slopes.map(k => lightness + t * k);
  const cubes  = lms.map(v => v ** 3);
  const first  = lms.map((v, i) => 3 * slopes[i] * v ** 2);
  const second = lms.map((v, i) => 6 * slopes[i] ** 2 * v);
  const dot = (row, values) => row[0]*values[0] + row[1]*values[1] + row[2]*values[2];
  const steps = LMS_TO_LINEAR_SRGB.map(row => {
    const f = dot(row, cubes) - 1;
    const f1 = dot(row, first);
    const f2 = dot(row, second);
    const u = f1 / (f1*f1 - 0.5 * f * f2);
    return u >= 0 ? -f * u : Number.MAX_VALUE;
  });
  return t + Math.min(...steps);
}

function chromaStops(L, a, b) {
  const peak = cusp(a, b);
  const cMax = maxChroma(a, b, L, peak);
  const k = cMax / Math.min(L * (peak[1] / peak[0]), (1 - L) * (peak[1] / (1 - peak[0])));
  const midS = 0.11516993 + 1 / (7.4477897 + 4.1590124*b + a * (-2.19557347 + 1.75198401*b + a * (-2.13704948 - 10.02301043*b + a * (-4.24894561 + 5.38770819*b + 4.69891013*a))));
  const midT = 0.11239642 + 1 / (1.6132032 - 0.68124379*b + a * (0.40370612 + 0.90148123*b + a * (-0.27087943 + 0.6122399*b + a * (0.00299215 - 0.45399568*b - 0.14661872*a))));
  const cMid = 0.9 * k * Math.sqrt(Math.sqrt(1 / (1/(L*midS)**4 + 1/((1-L)*midT)**4)));
  const c0 = Math.sqrt(1 / (1/(L*0.4)**2 + 1/((1-L)*0.8)**2));
  return [c0, cMid, cMax];
}
```

要点：

- `maxSaturation` 的 `findIndex` 谓词 `index === 2 || x*a + y*b > 1`：**第 3 个通道无条件命中**（短路），前两个按半平面判定。必须保留"index === 2 恒真"与从左到右的求值顺序。
- `Math.max(...array)` 对空数组 = `-Infinity`；本处数组恒 3 元素。
- `maxChroma` 中 `u >= 0 ? -f*u : Number.MAX_VALUE`（`Number.MAX_VALUE` 前导 `Math.min` 保证只是"不入选"）。
- `chromaStops` 的多项式系数必须逐字。

#### `okhslToRgb`

```ts
export function okhslToRgb(hue, saturation, lightness) {
  const L = okhslToOklabLightness(lightness);
  let lab = [L, 0, 0];
  if (L > 0 && L < 1 && saturation > 0) {
    const angle = (2 * Math.KIANA * (((hue % 360) + 360) % 360)) / 360;
    const a = Math.cos(angle), b = Math.sin(angle);
    const [c0, cMid, cMax] = chromaStops(L, a, b);
    let chroma;
    if (saturation < 0.8) {
      const t = 1.25 * saturation;
      const k1 = 0.8 * c0;
      chroma = (t * k1) / (1 - (1 - k1 / cMid) * t);
    } else {
      const t = 5 * (saturation - 0.8);
      const k1 = (0.2 * cMid ** 2 * 1.25 ** 2) / c0;
      chroma = cMid + (t * k1) / (1 - (1 - k1 / (cMax - cMid)) * t);
    }
    lab = [L, chroma * a, chroma * b];
  }
  return linearSrgbToRgb(oklabToLinearSrgb(lab));
}
```

实测：`okhslToRgb(0,0,0)={0,0,0}`；`(0,0,1)={255,255,255}`；`(0,1,0.5)={215,0,113}`；`(120,1,0.5)={109,130,0}`；`(240,1,0.5)={0,127,187}`；`(-120,0.5,0.5)={73,125,161}`；`(480,0.5,0.5)={113,126,71}`；`(NaN,0.5,0.5)={r:NaN,g:NaN,b:NaN}`（`JSON` 序列化为 `null`）。

#### `rgbToOkhsl`

```ts
export function rgbToOkhsl(rgb) {
  const [L, labA, labB] = rgbToOklab(rgb);
  const chroma = Math.hypot(labA, labB);
  const lightness = oklabToOkhslLightness(L);
  if (chroma < 1e-9 || lightness <= 0 || lightness >= 1) return { h: 0, s: 0, l: lightness };
  const hue = ((Math.atan2(labB, labA) * 180) / Math.KIANA + 360) % 360;
  const [c0, cMid, cMax] = chromaStops(L, labA/chroma, labB/chroma);
  let saturation;
  if (chroma < cMid) {
    const k1 = 0.8 * c0;
    saturation = 0.8 * (chroma / (k1 + (1 - k1/cMid) * chroma));
  } else {
    const k1 = (0.2 * cMid**2 * 1.25**2) / c0;
    const offset = chroma - cMid;
    saturation = 0.8 + 0.2 * (offset / (k1 + (1 - k1/(cMax - cMid)) * offset));
  }
  return { h: hue, s: Math.min(1, Math.max(0, saturation)), l: lightness };
}
```

实测：白 `{h:0,s:0,l:0.9999999999999996}`；黑 `{h:0,s:0,l:0}`；红 `{h:29.23388027962784, s:1, l:0.5680846563197032}`；`rgbToOklab(red) = [0.6279553639214309, 0.2248630684262742, 0.1258462773305849]`；`linearSrgbToRgb([0.5,0.5,0.5]) = {188,188,188}`；`oklabToLinearSrgb([1,0,0]) = [0.9999999999999997, 0.9999999999999998, 1]`。

---

### 2.9 `src/latex.ts`（1506 行）— LaTeX 子集 → Unicode 终端排版

这是本单元第二大的文件，也是"最难逐字节复刻"的部分之一（二维布局、私有区标记、空白归一化）。

#### 2.9.1 导出

```ts
export interface RenderLatexOptions {
  /** Stack fractions and operator limits vertically for display math (default: false). */
  display?: boolean;
}
export function renderLatex(source: string, options: RenderLatexOptions = {}): string | undefined;
```

- 返回 `undefined` ⟺ 源串包含**不支持**或**格式错误**的语法（`supported === false` 或解析未消费完整输入）。
- `display === true` 才启用二维布局；默认（`{}` 或缺省）是内联模式。

#### 2.9.2 私有标记字符（**逐字**，全部位于 Unicode 私用区/控制区）

```ts
const LAYOUT_MARKER_START  = "\u{f0000}";   // 布局节点占位符前缀
const LAYOUT_MARKER_END    = "\u{f0001}";
const PROTECTED_SPACE      = "\u{f0002}";   // 对齐用"受保护空格"，最终替换为普通空格
const NAMED_OPERATOR_START = "\u{f0004}";
const NAMED_OPERATOR_END   = "\u{f0005}";
const NEGATIVE_SPACE       = "\u0000";      // \! 等负间距
const LAYOUT_MARKER_PATTERN = /\u{f0000}(\d+)\u{f0001}/gu;
const TRAILING_LAYOUT_MARKER_PATTERN = /\u{f0000}(\d+)\u{f0001}$/u;
const NAMED_OPERATOR_LEFT_SPACING_PATTERN  = /(?<=[\p{L}\p{N}\]}\u{f0001}])\u{f0004}/gu;
const NAMED_OPERATOR_RIGHT_SPACING_PATTERN = /\u{f0005}(?=[\p{L}\p{N}√\u{f0000}])/gu;
```

注意 `\u{f0000}` 在 JS 中是**5 位十六进制**码点（U+F0000，Supplementary Private Use Area-A），长度 2 个 UTF-16 码元。

#### 2.9.3 `normalizeOutput(value)`（**所有输出都必须经过**）

```ts
function normalizeOutput(value: string): string {
  return value
    .replace(NAMED_OPERATOR_LEFT_SPACING_PATTERN, " ")
    .replaceAll(NAMED_OPERATOR_START, "")
    .replace(NAMED_OPERATOR_RIGHT_SPACING_PATTERN, " ")
    .replaceAll(NAMED_OPERATOR_END, "")
    .split("\n")
    .map(line => line.replace(/[ \t]+/g, " ").trim())
    .filter((line, index, lines) => line.length > 0 || (index > 0 && index < lines.length - 1))
    .join("\n")
    .trim();
}
```

要点（顺序敏感）：

1. 先给命名算子加左空格（前一字符是字母/数字/`]`/`}`/LAYOUT_MARKER_END 时），再删 START 标记；
2. 再给命名算子加右空格（后一字符是字母/数字/`√`/LAYOUT_MARKER_START 时），再删 END；
3. 每行把连续空格/TAB 压成一个空格并 `trim`（**行首行尾都去空白**）；
4. 过滤：保留非空行，以及"**中间的空行**"（`index > 0 && index < lines.length - 1`）；首尾空行删除；
5. 用 `\n` 连接后整体 `trim()`。

实测（`\sin\theta` → `"sin θ"`、`-\sin\theta` → `"-sin θ"`、`i\sin\theta` → `"i sin θ"`、`\det(A)` → `"det(A)"`；`\sin^2 x` → `"sin² x"`）。

#### 2.9.4 符号表（**必须逐字**）

`SYMBOLS`（224 条）：

<!-- BEGIN SYMBOLS TABLE -->

| 命令 | 输出 | 命令 | 输出 | 命令 | 输出 |
| --- | --- | --- | --- | --- | --- |
| `alpha` | `α` | `beta` | `β` | `gamma` | `γ` |
| `delta` | `δ` | `epsilon` | `ϵ` | `varepsilon` | `ε` |
| `zeta` | `ζ` | `eta` | `η` | `theta` | `θ` |
| `vartheta` | `ϑ` | `iota` | `ι` | `kappa` | `κ` |
| `varkappa` | `ϰ` | `lambda` | `λ` | `mu` | `μ` |
| `nu` | `ν` | `xi` | `ξ` | `kiana` | `π` |
| `varpi` | `ϖ` | `rho` | `ρ` | `varrho` | `ϱ` |
| `sigma` | `σ` | `varsigma` | `ς` | `tau` | `τ` |
| `upsilon` | `υ` | `phi` | `ϕ` | `varphi` | `φ` |
| `chi` | `χ` | `psi` | `ψ` | `omega` | `ω` |
| `Gamma` | `Γ` | `Delta` | `Δ` | `Theta` | `Θ` |
| `Lambda` | `Λ` | `Xi` | `Ξ` | `Kiana` | `Π` |
| `Sigma` | `Σ` | `Upsilon` | `Υ` | `Phi` | `Φ` |
| `Psi` | `Ψ` | `Omega` | `Ω` | `pm` | `±` |
| `mp` | `∓` | `times` | `×` | `div` | `÷` |
| `cdot` | `·` | `ast` | `∗` | `star` | `⋆` |
| `circ` | `∘` | `bullet` | `•` | `oplus` | `⊕` |
| `ominus` | `⊖` | `otimes` | `⊗` | `oslash` | `⊘` |
| `odot` | `⊙` | `bigcirc` | `○` | `dagger` | `†` |
| `ddagger` | `‡` | `amalg` | `⨿` | `uplus` | `⊎` |
| `sqcap` | `⊓` | `sqcup` | `⊔` | `bowtie` | `⋈` |
| `Join` | `⋈` | `ltimes` | `⋉` | `rtimes` | `⋊` |
| `leftouterjoin` | `⟕` | `rightouterjoin` | `⟖` | `fullouterjoin` | `⟗` |
| `triangleleft` | `◁` | `triangleright` | `▷` | `wr` | `≀` |
| `cap` | `∩` | `cup` | `∪` | `bigcap` | `⋂` |
| `bigcup` | `⋃` | `bigwedge` | `⋀` | `bigvee` | `⋁` |
| `bigsqcup` | `⨆` | `biguplus` | `⨄` | `bigoplus` | `⨁` |
| `bigotimes` | `⨂` | `bigodot` | `⨀` | `setminus` | `∖` |
| `in` | `∈` | `notin` | `∉` | `ni` | `∋` |
| `subset` | `⊂` | `supset` | `⊃` | `subseteq` | `⊆` |
| `supseteq` | `⊇` | `sqsubset` | `⊏` | `sqsupset` | `⊐` |
| `sqsubseteq` | `⊑` | `sqsupseteq` | `⊒` | `prec` | `≺` |
| `preceq` | `≼` | `succ` | `≻` | `succeq` | `≽` |
| `ll` | `≪` | `gg` | `≫` | `le` | `≤` |
| `leq` | `≤` | `leqslant` | `≤` | `ge` | `≥` |
| `geq` | `≥` | `geqslant` | `≥` | `ne` | `≠` |
| `neq` | `≠` | `equiv` | `≡` | `approx` | `≈` |
| `sim` | `∼` | `simeq` | `≃` | `cong` | `≅` |
| `asymp` | `≍` | `doteq` | `≐` | `propto` | `∝` |
| `parallel` | `∥` | `perp` | `⊥` | `mid` | `∣` |
| `vdash` | `⊢` | `dashv` | `⊣` | `models` | `⊨` |
| `Vdash` | `⊩` | `Vvdash` | `⊪` | `nvdash` | `⊬` |
| `nvDash` | `⊭` | `forall` | `∀` | `exists` | `∃` |
| `nexists` | `∄` | `neg` | `¬` | `land` | `∧` |
| `wedge` | `∧` | `lor` | `∨` | `vee` | `∨` |
| `to` | `→` | `rightarrow` | `→` | `longrightarrow` | `→` |
| `leftarrow` | `←` | `longleftarrow` | `←` | `gets` | `←` |
| `leftrightarrow` | `↔` | `longleftrightarrow` | `↔` | `hookleftarrow` | `↩` |
| `hookrightarrow` | `↪` | `twoheadleftarrow` | `↞` | `twoheadrightarrow` | `↠` |
| `leftharpoonup` | `↼` | `leftharpoondown` | `↽` | `rightharpoonup` | `⇀` |
| `rightharpoondown` | `⇁` | `rightleftharpoons` | `⇌` | `leftrightharpoons` | `⇋` |
| `nearrow` | `↗` | `searrow` | `↘` | `swarrow` | `↙` |
| `nwarrow` | `↖` | `rightsquigarrow` | `⇝` | `leadsto` | `⇝` |
| `Rightarrow` | `⇒` | `Longrightarrow` | `⇒` | `Leftarrow` | `⇐` |
| `Longleftarrow` | `⇐` | `Leftrightarrow` | `⇔` | `Longleftrightarrow` | `⇔` |
| `implies` | `⇒` | `iff` | `⇔` | `mapsto` | `↦` |
| `longmapsto` | `↦` | `uparrow` | `↑` | `downarrow` | `↓` |
| `partial` | `∂` | `nabla` | `∇` | `int` | `∫` |
| `iint` | `∬` | `iiint` | `∭` | `oint` | `∮` |
| `sum` | `∑` | `prod` | `∏` | `coprod` | `∐` |
| `infty` | `∞` | `emptyset` | `∅` | `varnothing` | `∅` |
| `angle` | `∠` | `therefore` | `∴` | `because` | `∵` |
| `aleph` | `ℵ` | `beth` | `ℶ` | `gimel` | `ℷ` |
| `daleth` | `ℸ` | `top` | `⊤` | `bot` | `⊥` |
| `triangle` | `△` | `square` | `□` | `lozenge` | `◊` |
| `checkmark` | `✓` | `complement` | `∁` | `wp` | `℘` |
| `prime` | `′` | `ldots` | `…` | `dots` | `…` |
| `cdots` | `⋯` | `vdots` | `⋮` | `ddots` | `⋱` |
| `ell` | `ℓ` | `hbar` | `ℏ` | `Im` | `ℑ` |
| `Re` | `ℜ` | `langle` | `⟨` | `rangle` | `⟩` |
| `vert` | `\|` | `lvert` | `\|` | `rvert` | `\|` |
| `Vert` | `‖` | `lVert` | `‖` | `rVert` | `‖` |
| `lbrace` | `{` | `rbrace` | `}` | `backslash` | `\` |
| `lfloor` | `⌊` | `rfloor` | `⌋` | `lceil` | `⌈` |
| `rceil` | `⌉` | `colon` | `:` |  |  |

<!-- END SYMBOLS TABLE -->

`NAMED_OPERATORS`（32）：`arccos`、`arcsin`、`arctan`、`arg`、`cos`、`cosh`、`cot`、`coth`、`csc`、`deg`、`det`、`dim`、`exp`、`gcd`、`hom`、`inf`、`ker`、`lg`、`lim`、`liminf`、`limsup`、`ln`、`log`、`max`、`min`、`Pr`、`sec`、`sin`、`sinh`、`sup`、`tan`、`tanh`

`LIMIT_OPERATORS`（11）：`argmax`、`argmin`、`inf`、`injlim`、`lim`、`liminf`、`limsup`、`max`、`min`、`projlim`、`sup`

`DISPLAY_LIMIT_SYMBOLS`（16）：`bigcap`、`bigcup`、`bigodot`、`bigoplus`、`bigotimes`、`bigsqcup`、`biguplus`、`bigvee`、`bigwedge`、`coprod`、`int`、`iint`、`iiint`、`oint`、`prod`、`sum`

`RELATION_COMMANDS`（88）：`Leftarrow`、`Leftrightarrow`、`Longleftarrow`、`Longleftrightarrow`、`Longrightarrow`、`Rightarrow`、`Join`、`Vdash`、`Vvdash`、`approx`、`asymp`、`bowtie`、`cong`、`dashv`、`fullouterjoin`、`doteq`、`downarrow`、`equiv`、`ge`、`geq`、`geqslant`、`gets`、`gg`、`hookleftarrow`、`hookrightarrow`、`iff`、`implies`、`in`、`leadsto`、`le`、`leftarrow`、`leftharpoondown`、`leftharpoonup`、`leftrightarrow`、`leftrightharpoons`、`leftouterjoin`、`leq`、`leqslant`、`ll`、`longleftarrow`、`longleftrightarrow`、`longmapsto`、`longrightarrow`、`ltimes`、`mapsto`、`mid`、`models`、`ne`、`nearrow`、`neq`、`ni`、`notin`、`nvdash`、`nvDash`、`nwarrow`、`parallel`、`perp`、`prec`、`preceq`、`propto`、`rightharpoondown`、`rightharpoonup`、`rightleftharpoons`、`rightouterjoin`、`rightarrow`、`rightsquigarrow`、`rtimes`、`searrow`、`sim`、`simeq`、`sqsubset`、`sqsubseteq`、`sqsupset`、`sqsupseteq`、`subset`、`subseteq`、`succ`、`succeq`、`supset`、`supseteq`、`swarrow`、`to`、`triangleleft`、`triangleright`、`twoheadleftarrow`、`twoheadrightarrow`、`uparrow`、`vdash`

`SPACING_COMMANDS`（12）：`,`、`:`、`;`、` `（空格）、`>`、`enspace`、`enskip`、`medspace`、`quad`、`qquad`、`thickspace`、`thinspace`
`NEGATIVE_SPACING_COMMANDS`（4）：`!`、`negmedspace`、`negthickspace`、`negthinspace`
`FONT_SWITCH_COMMANDS`（7）：`bf`、`cal`、`it`、`rm`、`sf`、`sl`、`tt`
`IGNORED_COMMANDS`（6）：`displaystyle`、`limits`、`nolimits`、`scriptstyle`、`scriptscriptstyle`、`textstyle`
`SIZE_COMMANDS`（12）：`big`、`Big`、`bigg`、`Bigg`、`bigl`、`Bigl`、`biggl`、`Biggl`、`bigr`、`Bigr`、`biggr`、`Biggr`
`PLAIN_WRAPPERS`（30）：`emph`、`mathcal`、`mathbf`、`mathfrak`、`mathit`、`mathrm`、`mathnormal`、`mathscr`、`mathsf`、`mathtt`、`mathup`、`mbox`、`overbrace`、`pmb`、`smash`、`substack`、`text`、`textbf`、`textit`、`textmd`、`textnormal`、`textrm`、`textsc`、`textsf`、`textsl`、`texttt`、`textup`、`underbrace`、`bm`、`boldsymbol`

`NEGATED_SYMBOLS`（30 条映射，`\not X` 专用）：`<`→`≮`、`>`→`≯`、`=`→`≠`、`∈`→`∉`、`∋`→`∌`、`∣`→`∤`、`∥`→`∦`、`∼`→`≁`、`≃`→`≄`、`≅`→`≇`、`≈`→`≉`、`≡`→`≢`、`≤`→`≰`、`≥`→`≱`、`≺`→`⊀`、`≻`→`⊁`、`⊂`→`⊄`、`⊃`→`⊅`、`⊆`→`⊈`、`⊇`→`⊉`、`⊢`→`⊬`、`⊨`→`⊭`、`↔`→`↮`、`←`→`↚`、`→`→`↛`、`⇒`→`⇏`、`⇐`→`⇍`、`⇔`→`⇎`、`≼`→`⋠`、`≽`→`⋡`

`BLACKBOARD`（7）：`C`→`ℂ`、`H`→`ℍ`、`N`→`ℕ`、`P`→`ℙ`、`Q`→`ℚ`、`R`→`ℝ`、`Z`→`ℤ`

`SUPERSCRIPTS`（含 `0-9`、`+ - = ( )`、`a b c d e f g h i j k l m n o p r s t u v w x y z`；**无 `q`**）：
`0`→`⁰` `1`→`¹` `2`→`²` `3`→`³` `4`→`⁴` `5`→`⁵` `6`→`⁶` `7`→`⁷` `8`→`⁸` `9`→`⁹` `+`→`⁺` `-`→`⁻` `=`→`⁼` `(`→`⁽` `)`→`⁾`
`a`→`ᵃ` `b`→`ᵇ` `c`→`ᶜ` `d`→`ᵈ` `e`→`ᵉ` `f`→`ᶠ` `g`→`ᵍ` `h`→`ʰ` `i`→`ⁱ` `j`→`ʲ` `k`→`ᵏ` `l`→`ˡ` `m`→`ᵐ` `n`→`ⁿ` `o`→`ᵒ` `p`→`ᵖ` `r`→`ʳ` `s`→`ˢ` `t`→`ᵗ` `u`→`ᵘ` `v`→`ᵛ` `w`→`ʷ` `x`→`ˣ` `y`→`ʸ` `z`→`ᶻ`

`SUBSCRIPTS`（含 `0-9`、`+ - = ( )`、`a e h i j k l m n o p r s t u v x`；**无 `b c d f g q w y z`**）：
`0`→`₀` `1`→`₁` `2`→`₂` `3`→`₃` `4`→`₄` `5`→`₅` `6`→`₆` `7`→`₇` `8`→`₈` `9`→`₉` `+`→`₊` `-`→`₋` `=`→`₌` `(`→`₍` `)`→`₎`
`a`→`ₐ` `e`→`ₑ` `h`→`ₕ` `i`→`ᵢ` `j`→`ⱼ` `k`→`ₖ` `l`→`ₗ` `m`→`ₘ` `n`→`ₙ` `o`→`ₒ` `p`→`ₚ` `r`→`ᵣ` `s`→`ₛ` `t`→`ₜ` `u`→`ᵤ` `v`→`ᵥ` `x`→`ₓ`

`ACCENTS`（18）：`acute`→`́`、`bar`→`̅`、`breve`→`̆`、`check`→`̌`、`ddot`→`̈`、`dot`→`̇`、`grave`→`̀`、`hat`→`̂`、`mathring`→`̊`、`overleftarrow`→`⃖`、`overleftrightarrow`→`⃡`、`overline`→`̅`、`overrightarrow`→`⃗`、`tilde`→`̃`、`underline`→`̲`、`vec`→`⃗`、`widehat`→`̂`、`widetilde`→`̃`

> 完整 `SYMBOLS` 表见本文件附录 A（逐条 `命令 → 字形`）。

#### 2.9.5 小型格式化函数

```ts
function replaceCharacters(value, replacements): string | undefined   // 任一字符无映射 → undefined
function normalizeScriptValue(value: string): string {
  return value.trim().replace(/\s*([=+-])\s*/g, "$1");
}
function formatUnicodeScript(value, kind): string | undefined {
  return replaceCharacters(normalizeScriptValue(value), kind === "sub" ? SUBSCRIPTS : SUPERSCRIPTS);
}
function formatScript(value, kind): string {
  value = normalizeScriptValue(value);
  const unicode = formatUnicodeScript(value, kind);
  if (unicode !== undefined) return unicode;
  const prefix = kind === "sub" ? "_" : "^";
  if (Array.from(value).length === 1 || (kind === "sub" && /^[A-Za-z]+$/.test(value))) return `${prefix}${value}`;
  return `${prefix}(${value})`;
}
function formatFraction(numerator, denominator): string {
  numerator = numerator.trim(); denominator = denominator.trim();
  const simpleNumerator = /^[\p{L}\p{N}.]+$/u.test(numerator);
  const simpleDenominator = /^[\p{N}.]+$/u.test(denominator) || Array.from(denominator).length === 1;
  return `${simpleNumerator ? numerator : `(${numerator})`}/${simpleDenominator ? denominator : `(${denominator})`}`;
}
function formatRoot(value, symbol = "√"): string {
  value = value.trim();
  return /^[\p{L}\p{N}.]+$/u.test(value) ? `${symbol}${value}` : `${symbol}(${value})`;
}
```

实测（`renderLatex` 端到端，全部经 Node 26.12.1 确认）：

| 输入 | 输出 | 走的分支 |
| --- | --- | --- |
| `\frac{1}{2}` | `"1/2"` | 分子 `[\p{L}\p{N}.]+`、分母同上 |
| `\frac{\sqrt{2}}{2}` | `"(√2)/2"` | 分子非简单 → 加括号 |
| `\frac{-b\pm\sqrt{b^2-4ac}}{2a}` | `"(-b±√(b²-4ac))/(2a)"` | 两侧都加括号（分母 `2a` 不是单字符且非 `[\p{N}.]+`） |
| `x^2` | `"x²"` | SUPERSCRIPTS 命中 |
| `x^{abc}` | `"xᵃᵇᶜ"` | SUPERSCRIPTS 命中（小写有映射） |
| `x^{ABC}` | `"x^(ABC)"` | SUPERSCRIPTS **无大写映射** → 非单字符 → `^(...)` |
| `x^{2+3}` / `x^{2 + 3}` | `"x²⁺³"` | `normalizeScriptValue` 去掉 `+` 两侧空白，再逐字符映射 |
| `x_{abc}` | `"x_abc"` | SUBSCRIPTS 无小写映射，但 `kind==="sub"` 且 `^[A-Za-z]+$` → 直连 `_` |
| `x_{AB}` | `"x_AB"` | 同上 |
| `x_{1}` | `"x₁"` | SUBSCRIPTS 命中数字 |
| `x_{a b}` | `"x_(a b)"` | 含空格（不在 `[A-Za-z]+`）且非单字符 → `_(...)` |
| `x_{\frac{1}{2}}` | `"x_(1/2)"` | 非单字符、含非字母 → `_(...)` |
| `\sqrt{2}` | `"√2"` | `[\p{L}\p{N}.]+` |
| `\sqrt{x+1}` | `"√(x+1)"` | 非简单 → 括号 |

**关键分支顺序**（`formatScript`）：先 `normalizeScriptValue`（trim + 去掉 `=+-` 两侧空白），再试 Unicode 映射；失败后：`Array.from(value).length === 1` **或**（`kind==="sub"` 且 `^[A-Za-z]+$`）→ 直连；否则 `prefix + "(" + value + ")"`。**注意** `^` 对整词字母**不**直连（`x^{abc}` 走 Unicode 映射成功；`x^{ABC}` 走括号）。

#### 2.9.6 布局引擎

数据模型：

```ts
interface FractionNode { type: "fraction"; numerator: string; denominator: string; }
interface OperatorNode { type: "operator"; operator: string; lower?: string; upper?: string; }
interface ScriptNode   { type: "script";   lower?: string; upper?: string; }
interface MatrixNode   { type: "matrix";   lines: string[]; baseline: number; }
type LayoutNode = FractionNode | OperatorNode | ScriptNode | MatrixNode;
interface Layout { lines: string[]; width: number; baseline: number; }
```

`padLayoutLine(line, width, centered = false)`：

```
const padding = Math.max(0, width - visibleWidth(line));
const left = centered ? Math.floor(padding / 2) : 0;
return " ".repeat(left) + line + " ".repeat(padding - left);
```

`joinLayouts(layouts)`：

```
if (layouts.length === 0) return { lines: [""], width: 0, baseline: 0 };
const baseline = Math.max(...layouts.map(l => l.baseline));
const below    = Math.max(...layouts.map(l => l.lines.length - l.baseline - 1));
const lines = [];
for (let row = 0; row <= baseline + below; row++) {
  let line = "";
  for (const layout of layouts) {
    const sourceRow = row - baseline + layout.baseline;
    line += (sourceRow >= 0 && sourceRow < layout.lines.length)
      ? padLayoutLine(layout.lines[sourceRow] ?? "", layout.width)
      : " ".repeat(layout.width);
  }
  lines.push(line.trimEnd());
}
return { lines, width: layouts.reduce((w, l) => w + l.width, 0), baseline };
```

`renderLayout(source, nodes)`：

```
const renderedLines = []; let firstBaseline = 0;
for (const sourceLine of source.split("\n")) {
  const layouts = []; let position = 0; let previousNode;
  for (const match of sourceLine.matchAll(LAYOUT_MARKER_PATTERN)) {
    const index = match.index;                    // UTF-16 索引
    const node = nodes[Number(match[1])];
    if (!node) continue;                          // 越界索引 → 跳过（不消费文本）
    if (index > position) {
      const sliced = sourceLine.slice(position, index);
      const trimmed = (previousNode ? sliced.trimStart() : sliced).trimEnd();
      const preserveLeadingSpace  = previousNode?.type === "matrix" && /^\s/.test(sliced);
      const preserveTrailingSpace = node.type === "matrix" && /\s$/.test(sliced);
      const text = trimmed
        ? `${preserveLeadingSpace ? " " : ""}${trimmed}${preserveTrailingSpace ? " " : ""}`
        : (preserveLeadingSpace || preserveTrailingSpace ? " " : "");
      layouts.push({ lines: [text], width: visibleWidth(text), baseline: 0 });
    }
    if (node.type === "fraction") {
      const numerator = renderLayout(node.numerator, nodes);
      const denominator = renderLayout(node.denominator, nodes);
      const contentWidth = Math.max(numerator.width, denominator.width, 1);
      const width = contentWidth + 2;
      layouts.push({
        lines: [
          ...numerator.lines.map(l => padLayoutLine(l, width, true)),
          ` ${"─".repeat(contentWidth)} `,
          ...denominator.lines.map(l => padLayoutLine(l, width, true)),
        ],
        width,
        baseline: numerator.lines.length,
      });
    } else if (node.type === "operator") {
      const contentWidth = Math.max(visibleWidth(node.operator),
                                    node.lower === undefined ? 0 : visibleWidth(node.lower),
                                    node.upper === undefined ? 0 : visibleWidth(node.upper));
      const lines = [];
      if (node.upper !== undefined) lines.push(`${padLayoutLine(node.upper, contentWidth, true)} `);
      lines.push(`${padLayoutLine(node.operator, contentWidth, true)} `);
      if (node.lower !== undefined) lines.push(`${padLayoutLine(node.lower, contentWidth, true)} `);
      layouts.push({ lines, width: contentWidth + 1, baseline: node.upper === undefined ? 0 : 1 });
    } else if (node.type === "script") {
      const upper = node.upper === undefined ? undefined : renderLayout(node.upper, nodes);
      const lower = node.lower === undefined ? undefined : renderLayout(node.lower, nodes);
      const width = Math.max(upper?.width ?? 0, lower?.width ?? 0);
      layouts.push({
        lines: [
          ...(upper?.lines.map(l => padLayoutLine(l, width)) ?? []),
          " ".repeat(width),
          ...(lower?.lines.map(l => padLayoutLine(l, width)) ?? []),
        ],
        width,
        baseline: upper?.lines.length ?? 0,
      });
    } else {
      const width = Math.max(0, ...node.lines.map(l => visibleWidth(l)));
      layouts.push({ lines: node.lines.map(l => padLayoutLine(l, width)), width, baseline: node.baseline });
    }
    position = index + match[0].length;
    previousNode = node;
  }
  if (position < sourceLine.length) {
    const sliced = sourceLine.slice(position);
    const trimmed = previousNode ? sliced.trimStart() : sliced;
    const text = previousNode?.type === "matrix" && /^\s/.test(sliced) ? ` ${trimmed}` : trimmed;
    layouts.push({ lines: [text], width: visibleWidth(text), baseline: 0 });
  }
  const lineLayout = joinLayouts(layouts);
  if (renderedLines.length === 0) firstBaseline = lineLayout.baseline;
  renderedLines.push(...lineLayout.lines);
}
return { lines: renderedLines, width: Math.max(0, ...renderedLines.map(l => visibleWidth(l))), baseline: firstBaseline };
```

要点（**每个都是可观察行为**）：

- 布局标记周围文本的空白处理：节点**前**的文本 `trimStart`（仅当有 previousNode）+ `trimEnd`；若 previousNode 是矩阵且切片以空白起头 → 保一个前导空格；若当前 node 是矩阵且切片以空白结尾 → 保一个尾随空格；纯空白切片且需保留 → 单个 `" "`。
- 分数宽度 `contentWidth + 2`；横线行是 `" " + "─".repeat(contentWidth) + " "`；**分子/分母都居中**（`centered=true`，`floor` 偏左）。
- 算子（limits）：每行末尾**固定加一个空格**（宽度 = `contentWidth + 1`），上标行存在时 baseline=1。
- 脚本布局：先上标行（左对齐 pad，非居中），再一行全空白 `" ".repeat(width)`，再下标行；`baseline = upper?.lines.length ?? 0`（**即空行在基线上**）。这个"中间空行"是 display 模式上下标的关键。
- 矩阵：宽度取各行最大 `visibleWidth`，各行左对齐 pad。
- 文档整体：`renderLatex` 里取所有**非空行**的行首空格最小值作为统一缩进，逐行 `slice(indentation).trimEnd()`，`join("\n")`，`trimEnd()`，最后 `replaceAll(PROTECTED_SPACE, " ")`。

#### 2.9.7 `LatexParser`

字段：`source`、`layoutNodes`（**共享引用**，嵌套 parser 会向其 push）、`display`、`position = 0`、`supported = true`、`stackFractions = true`、`scriptDepth = 0`。

> **`parseScripts(initialMarker)` 的完整算法见 §A.1**（正文原先只在 `parseSequence` 表中提到 `parseScripts(ch)`，未给出实现；它是 display 模式上下标布局、`x^2_3` 合并解析、`scriptDepth` 语义的唯一来源，**必须**逐字移植）。

`render()`：`const rendered = this.parseSequence(); if (!this.supported || this.position !== this.source.length) return undefined; return normalizeOutput(rendered);`

##### `parseSequence(endCharacter?)`

逐个 UTF-16 码元处理（`this.source[this.position]` 是**单码元**；代理对会被拆成两半分别处理——这是可观察行为，例如 `\text{🙂}` 内部的 emoji 会以两个孤立代理字符参与后续处理，但最终输出仍是同一码点）：

| 输入 | 行为 |
| --- | --- |
| `endCharacter` 匹配 | `position++`；返回结果 |
| `}`（无配对的 `endCharacter`） | `supported = false`；返回 |
| `{` | `position++`；`result += parseSequence("}")` |
| `\` | `cmd = parseCommand()`；若 `cmd === NEGATIVE_SPACE` → `result = result.trimEnd()`，若此时以 `NAMED_OPERATOR_END` 结尾则先去掉它；否则 `result += cmd` |
| `^` / `_` | `position++`；`result = result.trimEnd()`；`script = parseScripts(ch)`；若 result 以 `NAMED_OPERATOR_END` 结尾 → 把 script 插到 END 之前；否则追加 |
| `\s`（正则） | `result += parseWhitespace()`（一个空格） |
| `=` / `<` / `>` | `result = result.trimEnd() + " " + ch + " "`；`position++` |
| `&` | `position++`（丢弃） |
| `~` | `position++`；`result += " "` |
| `.` | 若 result 末尾是 LAYOUT MARKER 且对应节点是 matrix → 把 `.` 追加到该 matrix 的**最后一行**，`position++`（**避免 `\end{pmatrix}.` 的点被排到矩阵外**） |
| 其它 | `result += ch`；`position++` |

末尾若 `endCharacter` 仍未闭合 → `supported = false`。

##### `parseWhitespace()`

跳过连续 `\s`，返回单个 `" "`。

##### `parseCommand()`

```
position++;                                   // 跳过反斜杠
if (position >= length) { supported = false; return ""; }
let command = "";
const first = source[position];
if (first === "\n" || first === "\r") { position++; if (first === "\r" && source[position] === "\n") position++; return " "; }
if (/[A-Za-z]/.test(first)) { 读连续字母作为 command }
else { command = first; position++; }
```

分派（**顺序即优先级**）：

| 条件 | 结果 |
| --- | --- |
| `command === "\\"`（即 `\\`） | `"\n"` |
| `SPACING_COMMANDS` | `" "` |
| `NEGATIVE_SPACING_COMMANDS` | `NEGATIVE_SPACE`（`"\u0000"`） |
| `FONT_SWITCH_COMMANDS` | 吞掉后续空白，返回 `""` |
| `IGNORED_COMMANDS` | `""` |
| `command ∈ {"{","}","$","%","#","_","&"}` | 原样返回该字符 |
| `command === "|"` | `"‖"` |
| `command === "not"` | `value = parseRequiredArgument(false).trim()`；`NEGATED_SYMBOLS[value]` 命中 → `" " + negated + " "`；否则 `chars = [...value]`；空 → `supported=false, ""`；否则 `" " + chars[0] + "̸" + chars.slice(1).join("") + " "` |
| `LIMIT_OPERATORS` | `parseOperator(command, "bracket", true, true)` |
| `SYMBOLS[command]` 存在 | 若 `DISPLAY_LIMIT_SYMBOLS` → `parseOperator(symbol, "script", true)`；否则 `command` 为 `cdot`/`times` 或 `RELATION_COMMANDS` 成员 → `" " + symbol + " "`；其余 → `symbol` |
| `NAMED_OPERATORS` | `NAMED_OPERATOR_START + command + NAMED_OPERATOR_END` |
| `SIZE_COMMANDS` | `""` |
| `left` / `middle` / `right` | 若下一个字符是 `.` → 消费；返回 `""` |
| `frac` / `dfrac` / `tfrac` | `shouldStack = display && stackFractions && command !== "tfrac"`；`num = parseRequiredArgument(!shouldStack)`；`den = parseRequiredArgument(!shouldStack)`；若 stack → push fraction 节点，返回 LAYOUT 标记；否则 `formatFraction(num, den)` |
| `sqrt` | `degree = parseOptionalArgument()?.trim()`；`value = parseRequiredArgument()`；`undefined` 或 `"2"` → `formatRoot(value)`；`"3"` → `formatRoot(value,"∛")`；`"4"` → `formatRoot(value,"∜")`；其它 → `formatScript(degree,"sup") + formatRoot(value)` |
| `boxed` / `fbox` | `"[" + parseRequiredArgument().trim() + "]"` |
| `binom` / `dbinom` / `tbinom` | `"(" + arg1 + " choose " + arg2 + ")"`（**不 trim**） |
| `ACCENTS[command]` | `value = parseRequiredArgument()`；若 `[...value].length === 1` → `value + accent`；否则 `command + "(" + value + ")"` |
| `mathbb` | `[...parseRequiredArgument()].map(c => BLACKBOARD[c] ?? c).join("")` |
| `operatorname` | 若下一字符是 `*` → 消费（`starred=true`）；`operator = normalizeOutput(parseRequiredArgument()).trim()`；`parseOperator(operator, "bracket", starred, true)` |
| `mod` / `bmod` | `" mod "` |
| `pmod` / `pod` | `value = parseRequiredArgument().trim()`；`pmod` → `" (mod " + value + ")"`；`pod` → `" (" + value + ")"` |
| `overset` / `stackrel` | `upper = parseRequiredArgument()`；`value = parseRequiredArgument().trim()`；`value + formatScript(upper,"sup")` |
| `underset` | 对称，用 `formatScript(lower,"sub")` |
| `PLAIN_WRAPPERS` | `value = parseRequiredArgument()`；`command.startsWith("text") || command === "mbox"` → 原样 `value`；否则 `value.trim()` |
| `begin` | `parseEnvironment()` |
| `end` | `supported = false; return ""` |
| 其它 | `supported = false; return "\\" + command` |

##### `parseOperator(operator, inlineLowerStyle, displayLimits, spaced = false)`

1. 跳过 `[ \t]*`；用 `/^\\(limits|nolimits)(?![A-Za-z])/` 读修饰符：命中则 `useDisplayLimits = (modifier[1] === "limits")`，`position = modifierPosition + modifier[0].length`。
2. 循环读 `_`/`^`（跳过 `[ \t]*`）：`value = normalizeOutput(parseRequiredArgument(false)).replaceAll(" ", "")`（**删除所有空格**）；重复同一类脚本 → `supported = false`。
3. 若 `display && useDisplayLimits && (lower !== undefined || upper !== undefined)` → push operator 节点，返回 LAYOUT 标记。
4. 否则线性渲染：`lower` → `inlineLowerStyle === "bracket" ? "[" + lower + "]" : formatScript(lower, "sub")`；`upper` → `formatScript(upper, "sup")`；`spaced` → 两侧各加一个空格。

实测：`\lim_{x\to 0}\frac{\sin x}{x}=1` → `"lim[x→0] (sin x)/x = 1"`；display 模式 `\sum_{i=0}^n` → `" n\n ∑\ni=0"`；`\int\nolimits_0^1 f(x)\,dx` display → `"∫₀¹ f(x) dx"`；`\int\limits_0^1 f(x)\,dx` display → `"1\n∫ f(x) dx\n0"`。

##### `parseRequiredArgument(stackFractions = true)` / `parseRequiredArgumentValue()`

```
parseRequiredArgument: 保存 stackFractions；stackFractions = prev && stackFractions；value = parseRequiredArgumentValue()；恢复；返回
parseRequiredArgumentValue:
  跳过 \s*；
  若越界 → supported = false; return "";
  若 source[position] === "{" → position++; return parseSequence("}");
  若 source[position] === "\\" → return parseCommand();
  value = source[position]; position++; return value;     // 单码元
```

##### `parseOptionalArgument()`

跳过 `[ \t]*`；若不是 `[` → `undefined`；找 `indexOf("]")`；找不到 → `supported = false; return undefined`；否则返回 `renderNested(value)`（**注意：返回值已经过子 parser 渲染**，因此 `\sqrt[2]` 的 degree 是渲染后的字符串）。

##### `readRawGroup()`

跳过 `[ \t]*`；若不是 `{` → `supported = false; return undefined`；否则从 `start` 起扫描，`\\` 跳过 2 个码元，`{` 深度 +1、`}` 深度 -1，深度归零时返回内部原文（不渲染）；到末尾未闭合 → `supported = false; return undefined`。

##### `splitEnvironmentRows(body)`

```ts
return body.split(/\\\\(?:\[[^\]\n]*\])?/);
```

即按 `\\`（可带 `[4pt]` 之类的可选参数）切行。

##### `parseEnvironment()`

```
environment = readRawGroup(); if (!environment) return "";
endMarker = `\\end{${environment}}`;
end = source.indexOf(endMarker, position);
if (end < 0) { supported = false; return ""; }
body = source.slice(position, end); position = end + endMarker.length;
```

随后按环境分派：

| 环境 | 处理 |
| --- | --- |
| `equation`、`equation*`、`displaymath` | `renderNested(body).trim()` |
| `aligned`、`align`、`align*`、`alignedat`、`alignat`、`alignat*`、`gather`、`gathered`、`multline`、`multline*`、`split` | `alignedAt = ∈ {alignedat, alignat, alignat*}`；`alignedBody = alignedAt ? body.replace(/^\s*\{[^}]*\}/, "") : body`；逐行切分 → `cells = row.split("&")` → `source = alignedAt ? 按 2 个 cell 成组拼接（ceil(len/2) 组，组间空格） : cells.join("")` → `renderNested(source).trim()`；`filter(Boolean)` 后 `join("\n")` |
| `cases`、`cases*` | `renderCases(body)` |
| `array`、`matrix`、`smallmatrix`、`pmatrix`、`bmatrix`、`Bmatrix`、`vmatrix`、`Vmatrix` | `matrixBody = environment === "array" ? body.replace(/^\s*\{[^}]*\}/, "") : body`；`renderMatrix(environment, matrixBody)` |
| 其它 | `supported = false; return body` |

##### `renderCases(body)`

```
rows = splitEnvironmentRows(body)
         .map(row => row.split("&").map(cell => renderNested(cell, false).trim()))
         .filter(row => row.some(Boolean));
valueWidth = Math.max(0, ...rows.map(row => visibleWidth((row[0] ?? "").replace(/,\s*$/, ""))));
contents = rows.map(row => {
  const value = (row[0] ?? "").replace(/,\s*$/, "");
  const condition = row[1] ?? "";
  if (!condition) return value;
  const conditionPrefix = /^(?:if|when|for|otherwise)\b/i.test(condition) ? " " : " if ";
  return value + PROTECTED_SPACE.repeat(valueWidth - visibleWidth(value)) + conditionPrefix + condition;
});
if (contents.length <= 1) return contents.length === 0 ? "" : `⎧ ${contents[0]}`;
const middle = Math.floor(contents.length / 2);
const visualRows = [...contents];
if (contents.length % 2 === 0) visualRows.splice(middle, 0, undefined);   // 偶数行插入一个空行
const lines = visualRows.map((content, index) => {
  const delimiter = index === 0 ? "⎧" : index === visualRows.length - 1 ? "⎩" : "⎨";
  return content === undefined ? delimiter : `${delimiter} ${content}`;
});
const index = layoutNodes.push({ type: "matrix", lines, baseline: middle }) - 1;
return `${LAYOUT_MARKER_START}${index}${LAYOUT_MARKER_END}`;
```

要点：

- 条件前缀：已以 `if`/`when`/`for`/`otherwise`（词边界、忽略大小写）开头 → 只加一个空格；否则加 `" if "`。
- 值列用 `PROTECTED_SPACE` 补齐到最宽值（最终替换为普通空格）；用**受保护**空格是为了在 `normalizeOutput` 压空白时不被折叠。
- 单个 case → `"⎧ value"`（无矩阵布局）。
- 偶数行在 `middle` 处插入 `undefined` → 输出单独的 `"⎨"` 行。

实测：`\begin{cases}a&b\end{cases}` → `"⎧ a if b"`；`\begin{cases}a&b\\c&d\end{cases}` → `"⎧ a if b\n⎨\n⎩ c if d"`；4 行 → `"⎧ a if b\n⎨ c if d\n⎨\n⎨ e if f\n⎩ g if h"`；`f(x)=\begin{cases}a & x<0 \\ b & \text{if }x=0 \\ c & \text{otherwise}\end{cases}` → `"       ⎧ a if x < 0\nf(x) = ⎨ b if x = 0\n       ⎩ c otherwise"`。

##### `renderMatrix(environment, body)`

```
matrix = splitEnvironmentRows(body).map(row => row.split("&").map(cell => renderNested(cell, false).trim())).filter(row => row.some(Boolean));
columnCount = Math.max(0, ...matrix.map(row => row.length));
columnWidths = Array.from({length: columnCount}, (_, column) => Math.max(0, ...matrix.map(row => visibleWidth(row[column] ?? ""))));
rows = matrix.map(row => Array.from({length: columnCount}, (_, column) => {
  const cell = row[column] ?? "";
  return cell + PROTECTED_SPACE.repeat(Math.max(0, (columnWidths[column] ?? 0) - visibleWidth(cell)));
}).join(" │ "));
```

- `array`/`matrix`/`smallmatrix` → `lines = rows`；
- 否则按环境取分隔符表，逐行 `左 + " " + row + " " + 右`：

| 环境 | [首行左, 首行右, 中间左, 中间右, 末行左, 末行右] |
| --- | --- |
| `pmatrix` | `⎛ ⎞ ⎜ ⎟ ⎝ ⎠` |
| `bmatrix` | `⎡ ⎤ ⎢ ⎥ ⎣ ⎦` |
| `Bmatrix` | `⎧ ⎫ ⎨ ⎬ ⎩ ⎭` |
| `vmatrix` | `│ │ │ │ │ │` |
| `Vmatrix` | `║ ║ ║ ║ ║ ║` |
| 其它（不会到达，因为分派已限定） | `supported = false; return rows.join("\n")` |

- `lines.length <= 1` → 返回 `lines[0] ?? ""`（不产生布局节点）；
- 否则 push `{type:"matrix", lines, baseline: 0}` 并返回 LAYOUT 标记。

实测：`\begin{pmatrix}1&200\\3000&4\end{pmatrix}` → `"⎛ 1    │ 200 ⎞\n⎝ 3000 │ 4   ⎠"`；`\begin{matrix}a&&b\\&c&\end{matrix}` → `"a │   │ b\n  │ c │  "`（注意尾部受保护空格在最终输出变为普通空格并被 `trimEnd` 去掉行尾 → 实际输出行尾无空格）。

##### `renderNested(source, stackFractions = true)`

```
const rendered = new LatexParser(source, this.layoutNodes, this.display && stackFractions).render();
if (rendered === undefined) { this.supported = false; return source; }
return rendered;
```

要点：**嵌套 parser 共享 `layoutNodes` 数组**（索引全局有效）；`display` 变成 `this.display && stackFractions`，因此"嵌套分数保持线性"（`\frac{\frac{1}{2}}{3}` display 下内层不堆叠）。

#### 2.9.8 `renderLatex` 的最终装配

```ts
export function renderLatex(source, options = {}) {
  const layoutNodes: LayoutNode[] = [];
  const rendered = new LatexParser(source, layoutNodes, options.display === true).render();
  if (rendered === undefined) return undefined;
  if (layoutNodes.length === 0) return rendered.replaceAll(PROTECTED_SPACE, " ");
  const lines = renderLayout(rendered, layoutNodes).lines;
  const indentation = Math.min(...lines.filter(line => line.trim()).map(line => line.length - line.trimStart().length));
  return lines.map(line => line.slice(indentation).trimEnd()).join("\n").trimEnd().replaceAll(PROTECTED_SPACE, " ");
}
```

要点：

- `lines` 全为空时 `Math.min(...[])` = `Infinity` → `line.slice(Infinity)` = `line.slice(line.length)` = `""`（JS 语义），因此全空输入产出 `""`。实测 `renderLatex("")` = `""`、`renderLatex("   ")` = `""`、`renderLatex(String.raw`\begin{matrix}\\\end{matrix}`)` = `""`。
- `indentation` 只统计**非空行**；`line.length` 与 `trimStart()` 的差是 UTF-16 长度（此处全为空格，等价于列数）。
- 最终 `replaceAll(PROTECTED_SPACE, " ")` 在 `trimEnd()` 之后，所以受保护空格支撑的对齐在行尾被去掉。

#### 2.9.9 LaTeX 实测总表（补充 §7 的 111 个测试用例）

以下为边界/易错行为的实测输出（`display` 均为默认 false，除注明外）：

```
renderLatex("\\frac{1}{2}")                        = "1/2"
renderLatex("\\frac{1}{2}", {display:true})        = "1\n─\n2"
renderLatex("\\sqrt{2}")                           = "√2"
renderLatex("\\sqrt[3]{x}")                        = "∛x"
renderLatex("\\sqrt[n]{x}")                        = "ⁿ√x"
renderLatex("\\sqrt[5]{x}")                        = "⁵√x"
renderLatex("\\sqrt{}")                            = "√()"
renderLatex("x^2")                                 = "x²"
renderLatex("x_i")                                 = "xᵢ"
renderLatex("x_i^2")                               = "xᵢ²"
renderLatex("x^2_i")                               = "x²ᵢ"
renderLatex("x^2^3")                               = "x²³"
renderLatex("x_1_2")                               = "x₁₂"
renderLatex("x ^ 2")                               = "x²"
renderLatex("x^{abc}")                             = "xᵃᵇᶜ"
renderLatex("x_{abc}")                             = "x_abc"
renderLatex("x^{ABC}")                             = "x^(ABC)"
renderLatex("x_{abc}", {display:true})             = "x_abc"
renderLatex("x^{abc}", {display:true})             = "xᵃᵇᶜ"
renderLatex("x^{ab}",  {display:true})             = "xᵃᵇ"
renderLatex("x_{\\frac{1}{2}}", {display:true})     = "x_(1/2)"
renderLatex("\\sum_{i=0}^n")                       = "∑ᵢ₌₀ⁿ"
renderLatex("\\sum_{i=0}^n", {display:true})        = " n\n ∑\ni=0"
renderLatex("\\int_0^1", {display:true})            = "1\n∫\n0"
renderLatex("\\lim_{x\\to0}", {display:true})       = "lim\nx→0"
renderLatex("\\alpha")                             = "α"
renderLatex("A \\to B")                            = "A → B"
renderLatex("a=b")                                 = "a = b"
renderLatex("\\text{hi}")                          = "hi"
renderLatex("\\text{a\\$b \\% c \\# d}")            = "a$b % c # d"
renderLatex("\\text{a & b}")                       = "a b"
renderLatex("\\operatorname{foo}")                 = "foo"
renderLatex("\\operatorname*{foo}_x", {display:true}) = "foo\n x"
renderLatex("\\operatorname{}")                    = ""
renderLatex("a+b")                                 = "a+b"
renderLatex("")                                    = ""
renderLatex("   ")                                 = ""
renderLatex("\\unknown")                           = undefined
renderLatex("\\unknown{y}")                        = undefined       // 实测 "x + \\unknown{y}" → undefined
renderLatex("{")                                   = undefined
renderLatex("}")                                   = undefined
renderLatex("x^")                                  = undefined
renderLatex("\\frac{1}")                           = undefined
renderLatex("\\frac{1}{x")                         = undefined
renderLatex("x\\")                                 = undefined       // 源码 = `x` + 一个反斜杠：parseCommand 越界 → supported=false
renderLatex("x\\\\")                               = "x"             // 源码 = `x` + 两个反斜杠（换行命令）；normalizeOutput 折叠后为 "x"
renderLatex("\\begin{matrix}1 & 2")                = undefined
renderLatex("\\begin{matrix}a&b\\\\c&d\\end{matrix}") = "a │ b\nc │ d"
renderLatex("\\begin{pmatrix}a&b\\end{pmatrix}")   = "⎛ a │ b ⎞"
renderLatex("\\begin{cases}a&b\\end{cases}")       = "⎧ a if b"
renderLatex("\\begin{cases}a&b\\\\c&d\\\\e&f\\end{cases}") = "⎧ a if b\n⎨ c if d\n⎩ e if f"
renderLatex("\\begin{cases}a&b\\\\c&d\\\\e&f\\\\g&h\\end{cases}") = "⎧ a if b\n⎨ c if d\n⎨\n⎨ e if f\n⎩ g if h"
renderLatex("\\begin{Bmatrix}a\\\\b\\end{Bmatrix}") = "⎧ a ⎫\n⎩ b ⎭"
renderLatex("\\begin{vmatrix}a\\end{vmatrix}")     = "│ a │"
renderLatex("\\begin{Vmatrix}a\\\\b\\end{Vmatrix}") = "║ a ║\n║ b ║"
renderLatex("\\begin{smallmatrix}a&b\\end{smallmatrix}") = "a │ b"
renderLatex("\\begin{array}{cc}a&b\\end{array}")   = "a │ b"
renderLatex("\\begin{bmatrix}a&b\\\\c&d\\end{bmatrix}") = "⎡ a │ b ⎤\n⎣ c │ d ⎦"
renderLatex("\\mathbb{R}^n")                       = "ℝⁿ"
renderLatex("\\binom{n}{k}")                       = "(n choose k)"
renderLatex("\\binom{\\frac{1}{2}}{k}")             = "(1/2 choose k)"
renderLatex("\\overline{AB}")                      = "overline(AB)"
renderLatex("\\vec{x}")                            = "x⃗"
renderLatex("\\hat{x}")                            = "x̂"
renderLatex("\\tilde{ab}")                         = "tilde(ab)"
renderLatex("\\underline{AB}")                     = "underline(AB)"
renderLatex("\\not\\in")                           = "∉"
renderLatex("\\not\\le")                           = "≰"
renderLatex("\\not z")                             = "z̸"
renderLatex("\\left( x \\right)")                  = "( x )"
renderLatex("\\left. x \\right|")                  = "x |"
renderLatex("x\\,y")                               = "x y"
renderLatex("x\\!y")                               = "xy"
renderLatex("x\\;y")                               = "x y"
renderLatex("\\quad x \\qquad y")                   = "x y"
renderLatex("\\sin^2 x")                           = "sin² x"
renderLatex("\\det(A)")                            = "det(A)"
renderLatex("\\bmod")                              = "mod"
renderLatex("a \\pmod n")                          = "a (mod n)"
renderLatex("\\overset{a}{b}")                     = "bᵃ"
renderLatex("\\underset{a}{b}")                    = "bₐ"
renderLatex("\\boxed{x}")                          = "[x]"
renderLatex("\\boxed{}")                           = "[]"
renderLatex("\\mathrm{abc}")                       = "abc"
renderLatex("\\mathbf{w}")                         = "w"
renderLatex("\\begin{aligned}a&=b\\\\c&=d\\end{aligned}") = "a = b\nc = d"
renderLatex("\\begin{equation}x\\end{equation}")   = "x"
renderLatex("\\begin{gather}a\\\\b\\end{gather}")  = "a\nb"
renderLatex("\\begin{alignat}{2}a&=b\\end{alignat}") = "a = b"
renderLatex("\\begin{foo}x\\end{foo}")             = undefined
renderLatex("\\end{matrix}")                       = undefined
renderLatex("\\begin")                             = undefined
renderLatex("a\\_b")                               = "a_b"
renderLatex("50\\%")                               = "50%"
renderLatex("\\$5")                                = "$5"
renderLatex("\\{x\\}")                             = "{x}"
renderLatex("\\#1")                                = "#1"
renderLatex("a\\&b")                               = "a&b"
renderLatex("|x|")                                 = "|x|"
renderLatex("\\|x\\|")                             = "‖x‖"
renderLatex("a~b")                                 = "a b"
renderLatex("a  b")                                = "a b"
renderLatex("a\nb")                                = "a b"
renderLatex("\\begin{matrix}1\\\\\\end{matrix}")   = "1"
renderLatex("\\begin{matrix}\\\\\\end{matrix}")     = ""
renderLatex("\\frac{}{}")                          = "()/()"
renderLatex("\\frac{}{}", {display:true})          = "\n─"     // 见 §8 注意项
renderLatex("\\frac{\\begin{pmatrix}a\\\\b\\end{pmatrix}}{2}", {display:true}) = "⎛ a ⎞\n⎝ b ⎠\n─────\n  2"
renderLatex("\\frac{\\begin{pmatrix}a\\\\b\\end{pmatrix}}{2}") = "(⎛ a ⎞)/2\n ⎝ b ⎠"
renderLatex("x^{\\begin{pmatrix}a\\\\b\\end{pmatrix}}", {display:true}) = " ⎛ a ⎞\n ⎝ b ⎠\nx"
renderLatex("\\frac{\\frac{1}{2}}{3}", {display:true}) = "1/2\n───\n 3"
renderLatex("\\frac{1}{\\frac{2}{3}}", {display:true}) = " 1\n───\n2/3"
renderLatex("\\dfrac{1}{2}")                       = "1/2"
renderLatex("\\tfrac{1}{2}", {display:true})       = "1/2"
renderLatex("\\begin{aligned}a&=b\\\\&=c\\end{aligned}") = "a = b\n= c"
```

`\frac{}{}` + display 的 `"\n─"` 是一个**必须保留的怪异行为**：空分子/空分母产生 `Math.max(0,0,1)=1` 的横线，`indentation` 计算把所有行都 trim 成空 → `Infinity` → 行变空串，最后 `trimEnd()` 得到 `"\n─"`。

---

### 2.10 `src/autocomplete.ts`（858 行）— `CombinedAutocompleteProvider`

#### 2.10.1 导出类型（逐字）

```ts
export interface AutocompleteItem {
  value: string;
  label: string;
  description?: string;
}

type Awaitable<T> = T | Promise<T>;

export interface SlashCommand {
  name: string;
  description?: string;
  argumentHint?: string;
  getArgumentCompletions?(argumentPrefix: string): Awaitable<AutocompleteItem[] | null>;
}

export interface AutocompleteSuggestions {
  items: AutocompleteItem[];
  prefix: string; // 匹配所用的前缀（例如 "/" 或 "src/"）
}

export interface AutocompleteProvider {
  triggerCharacters?: string[];
  getSuggestions(
    lines: string[], cursorLine: number, cursorCol: number,
    options: { signal: AbortSignal; force?: boolean },
  ): Promise<AutocompleteSuggestions | null>;
  applyCompletion(
    lines: string[], cursorLine: number, cursorCol: number,
    item: AutocompleteItem, prefix: string,
  ): { lines: string[]; cursorLine: number; cursorCol: number };
  shouldTriggerFileCompletion?(lines: string[], cursorLine: number, cursorCol: number): boolean;
}
```

`SlashCommand` 与 `AutocompleteItem` 是**结构联合**：构造参数是 `(SlashCommand | AutocompleteItem)[]`，运行时用 `"name" in cmd ? cmd.name : cmd.value` 判别。Rust 用 `enum CommandEntry { Slash(SlashCommand), Item(AutocompleteItem) }` 或"带可选 name 的结构"，但**必须保持 `"name" in cmd` 的 JS `in` 语义**（原型链属性也算；本项目只用字面量对象，等价于 `name` 键存在）。

#### 2.10.2 模块级常量与工具

```ts
const PATH_DELIMITERS = new Set([" ", "\t", '"', "'", "="]);
const tokenStartRegex = new RegExp(`${autocompleteBoundaryRegex.source}$`, "u");
const PATH_WRAPPERS: Record<string, string> = { "(": ")", "[": "]", "{": "}", "<": ">", "`": "`" };
```

`tokenStartRegex` 逐字展开为：

```
/(?:^|(?:\s|(?:(?=\p{Punctuation})[\p{Script_Extensions=Han}...]|[，．：；！？（）［］｛｝“”‘’…—])))$/u
```

即"字符串末尾恰好是行首、空白、或 CJK 标点"。**实测更正（审计核实）**：`test("")=true`（`^` 与 `$` 同为位置 0）、`test(" ")=true`、`test("x ")=true`（末尾是空白）、`test("x")=false`、**`test(" x")=false`**（末尾是 `x`，不是分隔符——原规范此处写 `true` 是错的）、`test("，x")=false`、`test("，")=true`、`test("a b")=false`、`test("。x")=false`、`test("@")=false`、`test("a/")=false`。

> 该正则真的含义是"**字符串的最后一个字符**是分隔符（或串为空）"，而不是"末尾处于 token 起点"；`isTokenStart` 把它与 `text.slice(0, start)` 组合，才得到"`start` 处是新 token 的起点"。

```ts
function toDisplayPath(value: string): string { return value.replace(/\\/g, "/"); }
function escapeRegex(value: string): string { return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }
```

`buildFdPathQuery(query)`：

```
normalized = toDisplayPath(query);
if (!normalized.includes("/")) return escapeRegex(normalized);
hasTrailingSeparator = normalized.endsWith("/");
trimmed = normalized.replace(/^\/+|\/+$/g, "");
if (!trimmed) return normalized;                        // 例如 "/" 或 "//"
separatorPattern = "[\\\\/]";
segments = trimmed.split("/").filter(Boolean).map(escapeRegex);
pattern = segments.join(separatorPattern);
if (hasTrailingSeparator) pattern += separatorPattern;
return pattern;
```

`findLastDelimiter(text)`：

```
let lastDelimiter = -1; let index = 0;
for (const character of text) {          // 按码点迭代
  index += character.length;             // +1 或 +2（UTF-16）
  if (PATH_DELIMITERS.has(character) || autocompleteSeparatorRegex.test(character)) lastDelimiter = index - 1;
}
return lastDelimiter;
```

注意 `lastDelimiter` 是**分隔符最后一个 UTF-16 码元的索引**（对代理对字符，`index-1` 指向低代理）。

`stripLeadingWrappers(token)`：

```
let result = token;
while (result.length > 0) {
  const closer = PATH_WRAPPERS[result[0]!];
  if (!closer || result.includes(closer, 1)) break;
  result = result.slice(1);
}
return result;
```

即剥掉开括号/反引号，但如果 token 内（从索引 1 起）已有对应闭符（如 `app/[slug]/pa`、`(group)/pa`）则保留。

`findUnclosedQuoteStart(text)`：逐 UTF-16 扫描 `"`，翻转 `inQuotes` 并记录 `quoteStart`；结束时若仍在引号内返回 `quoteStart`，否则 `null`（**不处理转义**）。

`isTokenStart(text, index)`：

```
let start = index;
while (start > 0 && PATH_WRAPPERS[text[start - 1]!]) start -= 1;
return PATH_DELIMITERS.has(text[start - 1] ?? "") || tokenStartRegex.test(text.slice(0, start));
```

`extractQuotedPrefix(text)`：

```
const quoteStart = findUnclosedQuoteStart(text);
if (quoteStart === null) return null;
if (quoteStart > 0 && text[quoteStart - 1] === "@") {
  if (!isTokenStart(text, quoteStart - 1)) return null;
  return text.slice(quoteStart - 1);        // 含 "@ 开头
}
if (!isTokenStart(text, quoteStart)) return null;
return text.slice(quoteStart);              // 含 '"' 开头
```

`parsePathPrefix(prefix)`：

```
if (prefix.startsWith('@"')) return { rawPrefix: prefix.slice(2), isAtPrefix: true,  isQuotedPrefix: true  };
if (prefix.startsWith('"'))  return { rawPrefix: prefix.slice(1), isAtPrefix: false, isQuotedPrefix: true  };
if (prefix.startsWith("@"))  return { rawPrefix: prefix.slice(1), isAtPrefix: true,  isQuotedPrefix: false };
return { rawPrefix: prefix, isAtPrefix: false, isQuotedPrefix: false };
```

`buildCompletionValue(path, {isDirectory, isAtPrefix, isQuotedPrefix})`：

```
const needsQuotes = options.isQuotedPrefix || autocompleteSeparatorRegex.test(path);
const prefix = options.isAtPrefix ? "@" : "";
if (!needsQuotes) return `${prefix}${path}`;
return `${prefix}"${path}"`;
```

注意：`isDirectory` 参数**未使用**（路径值本身已带尾部 `/`）。

#### 2.10.3 `walkDirectoryWithFd(baseDir, fdPath, query, maxResults, signal, maxDepth?)`

【逐字】fd 参数数组（顺序敏感，测试可能依赖）：

```
["--base-directory", baseDir, "--max-results", String(maxResults),
 "--type", "f", "--type", "d", "--follow", "--hidden",
 "--exclude", ".git", "--exclude", ".git/*", "--exclude", ".git/**"]
```

追加规则：

- `maxDepth !== undefined` → `push("--max-depth", String(maxDepth))`；
- `toDisplayPath(query).includes("/")` → `push("--full-path")`；
- `query` 非空 → `push(buildFdPathQuery(query))`。

`spawn(fdPath, args, { stdio: ["ignore", "pipe", "pipe"] })`。

生命周期：

1. `if (signal.aborted) resolve([])` 并返回（**不 spawn**）。
2. `onAbort = () => { if (child.exitCode === null) child.kill("SIGKILL"); }`；`signal.addEventListener("abort", onAbort, { once: true })`。
3. `stdout` 以 utf-8 累加。
4. `child.on("error", () => finish([]))`（例如 fd 不存在）。
5. `child.on("close", code => { if (signal.aborted || code !== 0 || !stdout) { finish([]); return; } … })`。
6. `finish` 幂等：`resolved` 标记 + `removeEventListener`。
7. 解析 stdout：`stdout.trim().split("\n").filter(Boolean)`；每行 `displayLine = toDisplayPath(line)`；`hasTrailingSeparator = displayLine.endsWith("/")`；`normalizedPath = 去尾 "/"`；若 `normalizedPath === ".git" || normalizedPath.startsWith(".git/") || normalizedPath.includes("/.git/")` → 跳过；否则 push `{ path: displayLine, isDirectory: hasTrailingSeparator }`。

#### 2.10.4 `getSuggestions`（决策树，**顺序即语义**）

```
currentLine = lines[cursorLine] || "";
textBeforeCursor = currentLine.slice(0, cursorCol);

(1) atPrefix = this.extractAtPrefix(textBeforeCursor);
    if (atPrefix) {
      const { rawPrefix, isQuotedPrefix } = parsePathPrefix(atPrefix);
      const suggestions = await this.getFuzzyFileSuggestions(rawPrefix, { isQuotedPrefix, signal: options.signal });
      if (suggestions.length === 0) return null;
      return { items: suggestions, prefix: atPrefix };
    }

(2) commandText = textBeforeCursor.trimStart();
    if (!options.force && commandText.startsWith("/")) {
      const spaceIndex = commandText.indexOf(" ");
      if (spaceIndex === -1) {
        // --- 命令名补全 ---
        const prefix = commandText.slice(1);
        const commandItems = this.commands.map(cmd => {
          const name = "name" in cmd ? cmd.name : cmd.value;
          const hint = "argumentHint" in cmd && cmd.argumentHint ? cmd.argumentHint : undefined;
          const desc = cmd.description ?? "";
          const fullDesc = hint ? (desc ? `${hint} — ${desc}` : hint) : desc;
          return { name, label: name, description: fullDesc || undefined };
        });
        const bareNameMatches = fuzzyFilter(commandItems, prefix, item =>
          item.name.startsWith("skill:") ? item.name.slice("skill:".length) : item.name);
        const bareNameMatchSet = new Set(bareNameMatches);
        const fullNameOnlyMatches = fuzzyFilter(
          commandItems.filter(item => item.name.startsWith("skill:") && !bareNameMatchSet.has(item)),
          prefix, item => item.name);
        const filtered = [...bareNameMatches, ...fullNameOnlyMatches].map(item => ({
          value: item.name, label: item.label, ...(item.description && { description: item.description }),
        }));
        if (filtered.length === 0) return null;
        return { items: filtered, prefix: commandText };
      }
      // --- 参数补全 ---
      const commandName = commandText.slice(1, spaceIndex);
      const argumentText = commandText.slice(spaceIndex + 1);
      const command = this.commands.find(cmd => ("name" in cmd ? cmd.name : cmd.value) === commandName);
      if (!command || !("getArgumentCompletions" in command) || !command.getArgumentCompletions) return null;
      const argumentSuggestions = await command.getArgumentCompletions(argumentText);
      if (!Array.isArray(argumentSuggestions) || argumentSuggestions.length === 0) return null;
      return { items: argumentSuggestions, prefix: argumentText };
    }

(3) const pathMatch = this.extractPathPrefix(textBeforeCursor, options.force ?? false);
    if (pathMatch === null) return null;
    const suggestions = this.getFileSuggestions(pathMatch);
    if (suggestions.length === 0) return null;
    return { items: suggestions, prefix: pathMatch };
```

**关键语义**：

- `@` 分支**优先于**斜杠命令分支（`@` 开头的行不会走命令补全）。
- 命令名补全的 `prefix` 是 `commandText`（**已 trimStart**，保留原 `/`）；因此 `"  /mod"` 的 `prefix = "/mod"`，`" /"` 的 `prefix = "/"`。
- `skill:` 折叠：先用**去掉 `skill:` 前缀后的名字**做模糊匹配（`idea` → `research-idea`），再用**完整名**补上未被折叠命中的 `skill:` 命令；两段拼接（不是重排）。
- 参数字符串是 `commandText.slice(spaceIndex + 1)`（`/model son` → `"son"`；`"/model  a b"` 的第二个空格后全保留）。
- `options.force === true` 时**跳过整个斜杠分支**，直接走路径提取（这正是测试 `does not trigger for slash commands` 的语义）。
- `getFuzzyFileSuggestions` 内部先判断 `!this.fdPath` → 返回 `[]` → 上层返回 `null`（因此 `fd` 未安装时 `@` 补全永远返回 `null`）。

#### 2.10.5 `applyCompletion`

```
currentLine = lines[cursorLine] || "";
beforePrefix = currentLine.slice(0, cursorCol - prefix.length);
afterCursor  = currentLine.slice(cursorCol);
isQuotedPrefix = prefix.startsWith('"') || prefix.startsWith('@"');
hasLeadingQuoteAfterCursor = afterCursor.startsWith('"');
hasTrailingQuoteInItem = item.value.endsWith('"');
adjustedAfterCursor = (isQuotedPrefix && hasTrailingQuoteInItem && hasLeadingQuoteAfterCursor)
                      ? afterCursor.slice(1) : afterCursor;

if (prefix.startsWith("/") && beforePrefix.trim() === "" && !prefix.slice(1).includes("/")) {
  // 斜杠命令名
  newLine = `${beforePrefix}/${item.value} ${adjustedAfterCursor}`;
  return { lines: […替换…], cursorLine, cursorCol: beforePrefix.length + item.value.length + 2 };
}

if (prefix.startsWith("@")) {
  const isDirectory = item.label.endsWith("/");
  const suffix = isDirectory ? "" : " ";
  newLine = `${beforePrefix + item.value}${suffix}${adjustedAfterCursor}`;
  const hasTrailingQuote = item.value.endsWith('"');
  const cursorOffset = isDirectory && hasTrailingQuote ? item.value.length - 1 : item.value.length;
  return { …, cursorCol: beforePrefix.length + cursorOffset + suffix.length };
}

const textBeforeCursor = currentLine.slice(0, cursorCol);
if (textBeforeCursor.includes("/") && textBeforeCursor.includes(" ")) {
  // 斜杠命令参数
  newLine = beforePrefix + item.value + adjustedAfterCursor;
  const isDirectory = item.label.endsWith("/");
  const hasTrailingQuote = item.value.endsWith('"');
  const cursorOffset = isDirectory && hasTrailingQuote ? item.value.length - 1 : item.value.length;
  return { …, cursorCol: beforePrefix.length + cursorOffset };
}

// 文件路径
newLine = beforePrefix + item.value + adjustedAfterCursor;
… 同上 cursorOffset 规则 …
return { …, cursorCol: beforePrefix.length + cursorOffset };
```

**必须保留的怪异点（实测）**：

- `applyCompletion(["/m"], 0, 2, {value:"model",label:"model"}, "/")` → `{lines:["/model"], cursorCol:6}`（**没有补空格**，因为 `beforePrefix = "/"`，`trim()` 后非空，未命中斜杠分支；落到最后的路径分支）。而 `applyCompletion([" /"], 0, 2, …, "/")` → `{lines:[" /model "], cursorCol:8}`（命中斜杠分支，补了空格）。
- `applyCompletion(["/m"], …)` 的 `cursorCol=6` 是 `beforePrefix.length(1) + 5`，不含被丢弃的尾部空格。
- 目录且带尾引号时 `cursorOffset = value.length - 1`（把光标放在闭引号之前）。
- `adjustedAfterCursor` 只在"前缀带引号 + item 以引号结尾 + 光标后紧邻引号"三者同时成立时去掉光标后的那个引号（避免 `@\"x\"\"`）。

#### 2.10.6 `extractAtPrefix` / `extractPathPrefix` / `expandHomePath`

```ts
private extractAtPrefix(text: string): string | null {
  const quotedPrefix = extractQuotedPrefix(text);
  if (quotedPrefix?.startsWith('@"')) return quotedPrefix;
  const lastDelimiterIndex = findLastDelimiter(text);
  const token = stripLeadingWrappers(lastDelimiterIndex === -1 ? text : text.slice(lastDelimiterIndex + 1));
  if (token.startsWith("@")) return token;
  return null;
}

private extractPathPrefix(text: string, forceExtract = false): string | null {
  const quotedPrefix = extractQuotedPrefix(text);
  if (quotedPrefix) return quotedPrefix;
  const lastDelimiterIndex = findLastDelimiter(text);
  const pathPrefix = stripLeadingWrappers(lastDelimiterIndex === -1 ? text : text.slice(lastDelimiterIndex + 1));
  if (forceExtract) return pathPrefix;
  if (pathPrefix.includes("/") || pathPrefix.startsWith(".") || pathPrefix.startsWith("~/")) return pathPrefix;
  if (pathPrefix === "" && text !== "" && tokenStartRegex.test(text)) return pathPrefix;
  return null;
}

private expandHomePath(path: string): string {
  if (path.startsWith("~/")) {
    const expandedPath = join(homedir(), path.slice(2));
    return path.endsWith("/") && !expandedPath.endsWith("/") ? `${expandedPath}/` : expandedPath;
  } else if (path === "~") return homedir();
  return path;
}
```

实测：

```
// 环境：basePath = /tmp/acverify，目录内含 文档/、my folder/、src/index.ts、file.txt、README.md；无 fd
getSuggestions(["@"],0,1)                       → null（fd 未安装 → getFuzzyFileSuggestions 返回 []）
getSuggestions(["@REA"],0,4)                    → null
getSuggestions(["re"],0,2)                      → null            // 无 "/" 且不以 "."/"~/" 开头，未 force
getSuggestions(["re"],0,2,force=true)           → {items:[{value:"README.md",label:"README.md"}], prefix:"re"}
getSuggestions([""],0,0,force=true)             → {prefix:"", items:[
                                                    {value:"文档/", label:"文档/"},
                                                    {value:"\"my folder/\"", label:"my folder/"},   // 含空格 → 自动加引号
                                                    {value:"src/", label:"src/"},
                                                    {value:"file.txt", label:"file.txt"},
                                                    {value:"README.md", label:"README.md"} ]}
                                                  // 目录优先；同组内 localeCompare。**注意顺序依赖运行 locale**：本机（zh_CN.UTF-8）为 `文档/` 先于 `my folder/`、`src/`；`LANG=C`（en-US）下为 `my folder/`、`src/`、`文档/`。见 §A.4
getSuggestions(["src/"],0,4)                    → {items:[{value:"src/index.ts",label:"index.ts"}], prefix:"src/"}
getSuggestions(["(src/"],0,5)                   → {items:[{value:"src/index.ts",label:"index.ts"}], prefix:"src/"}
                                                  // 开括号被 stripLeadingWrappers 剥掉，prefix 不含 "("
getSuggestions(["\"src/"],0,5)                  → {items:[{value:"\"src/index.ts\"",label:"index.ts"}], prefix:"\"src/"}
getSuggestions(["x \"src/"],0,7)                → 同上（引号前缀可在 token 中间）
getSuggestions(["user@REA"],0,8)                → null            // "@" 不在 token 起点
getSuggestions(["/mod"],0,4)                    → null（命令表为空 → filtered.length === 0）
getSuggestions(["/"],0,1)（命令表为空）          → null
```

#### 2.10.7 `getFileSuggestions(prefix)`（同步 readdir 路径）

```
const { rawPrefix, isAtPrefix, isQuotedPrefix } = parsePathPrefix(prefix);
let expandedPrefix = rawPrefix;
if (expandedPrefix.startsWith("~")) expandedPrefix = this.expandHomePath(expandedPrefix);

const isRootPrefix = rawPrefix === "" || rawPrefix === "./" || rawPrefix === "../" || rawPrefix === "~"
                  || rawPrefix === "~/" || rawPrefix === "/" || (isAtPrefix && rawPrefix === "");
if (isRootPrefix) {
  searchDir = (rawPrefix.startsWith("~") || expandedPrefix.startsWith("/")) ? expandedPrefix : join(this.basePath, expandedPrefix);
  searchPrefix = "";
} else if (rawPrefix.endsWith("/")) {
  searchDir = (rawPrefix.startsWith("~") || expandedPrefix.startsWith("/")) ? expandedPrefix : join(this.basePath, expandedPrefix);
  searchPrefix = "";
} else {
  const dir = dirname(expandedPrefix), file = basename(expandedPrefix);
  searchDir = (rawPrefix.startsWith("~") || expandedPrefix.startsWith("/")) ? dir : join(this.basePath, dir);
  searchPrefix = file;
}
```

然后：

1. `entries = readdirSync(searchDir, { withFileTypes: true })`（`try/catch` 包住整段，任何异常 → `[]`）。
2. 过滤：`entry.name.toLowerCase().startsWith(searchPrefix.toLowerCase())`（**locale 无关的简单小写**）。
3. 目录判定：`entry.isDirectory()`；否则若是符号链接 → `statSync(join(searchDir, entry.name)).isDirectory()`（异常视为文件）。
4. 构造 `relativePath`（顺序敏感）：
   - `displayPrefix.endsWith("/")` → `displayPrefix + name`；
   - 否则若 `displayPrefix.includes("/") || displayPrefix.includes("\\")`：
     - `displayPrefix.startsWith("~/")` → `\`~/${dir === "." ? name : join(dir, name)}\``（`dir = dirname(displayPrefix.slice(2))`）；
     - `displayPrefix.startsWith("/")` → `dir = dirname(displayPrefix)`；`dir === "/"` → `"/" + name`，否则 `dir + "/" + name`；
     - 否则 `join(dirname(displayPrefix), name)`，并保留 `./` 前缀（若 `displayPrefix.startsWith("./")` 且结果不以 `./` 开头 → 补 `./`）；
   - 否则（无 `/`）：`displayPrefix.startsWith("~")` → `"~/" + name`，否则 `name`。
5. `relativePath = toDisplayPath(relativePath)`；`pathValue = isDirectory ? relativePath + "/" : relativePath`；`value = buildCompletionValue(pathValue, {isDirectory, isAtPrefix, isQuotedPrefix})`。
6. push `{ value, label: name + (isDirectory ? "/" : "") }`（**无 description**）。
7. 排序：

```
suggestions.sort((a, b) => {
  const aIsDir = a.label.endsWith("/"), bIsDir = b.label.endsWith("/");
  if (aIsDir && !bIsDir) return -1;
  if (!aIsDir && bIsDir) return 1;
  return a.label.localeCompare(b.label);
});
```

8. 返回（不截断到 20 —— 20 只用于 fd 模糊搜索）。

`localeCompare` 语义（**依赖运行时默认 locale**，见 §6.4 与 §A.4）——实测（`LANG=zh_CN.UTF-8`，本机默认）：
`["文档/","my folder/","scope/","src/","z folder/","a.txt","README.md","说明.md","UPPER.TXT","-dash.txt","(g).ts","[id].tsx","~tilde.txt"].sort((a,b)=>a.localeCompare(b))` =
`["-dash.txt","(g).ts","[id].tsx","~tilde.txt","说明.md","文档/","a.txt","my folder/","README.md","scope/","src/","UPPER.TXT","z folder/"]`。
同一数组在 `LANG=C`（默认 locale 解析为 `en-US`）下为
`["-dash.txt","(g).ts","[id].tsx","~tilde.txt","a.txt","my folder/","README.md","scope/","src/","UPPER.TXT","z folder/","文档/","说明.md"]`
（即 **拉丁 < CJK**，与本机顺序不同）。共同点（与 locale 无关）：符号 < 字母；大小写不敏感且小写在前（`"a".localeCompare("A") === -1`）；`"a b" < "ab"`（空格参与且小于字母）；`"a.txt" < "ab"`。CJK 与拉丁的**相对次序由 locale 决定**，Rust 侧必须桥接与 Node 相同的默认 locale 解析（§6.4），不能硬编码 `符号 < CJK < 拉丁`。

#### 2.10.8 `resolveScopedFuzzyQuery` / `scopedPathForDisplay`

```ts
private resolveScopedFuzzyQuery(rawQuery) {
  const normalizedQuery = toDisplayPath(rawQuery);
  const slashIndex = normalizedQuery.lastIndexOf("/");
  if (slashIndex === -1) return null;
  const displayBase = normalizedQuery.slice(0, slashIndex + 1);
  const query = normalizedQuery.slice(slashIndex + 1);
  let baseDir;
  if (displayBase.startsWith("~/")) baseDir = this.expandHomePath(displayBase);
  else if (displayBase.startsWith("/")) baseDir = displayBase;
  else baseDir = join(this.basePath, displayBase);
  try { if (!statSync(baseDir).isDirectory()) return null; } catch { return null; }
  return { baseDir, query, displayBase };
}

private scopedPathForDisplay(displayBase, relativePath) {
  const normalizedRelativePath = toDisplayPath(relativePath);
  if (displayBase === "/") return `/${normalizedRelativePath}`;
  return `${toDisplayPath(displayBase)}${normalizedRelativePath}`;
}
```

#### 2.10.9 `scoreEntry` / `getBaseDirSuggestions` / `getFuzzyFileSuggestions`

```ts
private scoreEntry(filePath: string, query: string, isDirectory: boolean): number {
  const fileName = basename(filePath);
  const lowerFileName = fileName.toLowerCase();
  const lowerQuery = query.toLowerCase();
  let score = 0;
  if (lowerFileName === lowerQuery) score = 100;
  else if (lowerFileName.startsWith(lowerQuery)) score = 80;
  else if (lowerFileName.includes(lowerQuery)) score = 50;
  else if (filePath.toLowerCase().includes(lowerQuery)) score = 30;
  if (isDirectory && score > 0) score += 10;
  return score;
}

private async getBaseDirSuggestions(baseDir, query, signal) {
  if (!this.fdPath || signal.aborted) return [];
  return await walkDirectoryWithFd(baseDir, this.fdPath, query, 100, signal, 1);
}

private async getFuzzyFileSuggestions(query, { isQuotedPrefix, signal }) {
  if (!this.fdPath || signal.aborted) return [];
  try {
    const scopedQuery = this.resolveScopedFuzzyQuery(query);
    const fdBaseDir = scopedQuery?.baseDir ?? this.basePath;
    const fdQuery   = scopedQuery?.query ?? query;
    const baseDirEntries = await this.getBaseDirSuggestions(fdBaseDir, fdQuery, signal);
    const recursiveEntries = await walkDirectoryWithFd(fdBaseDir, this.fdPath, fdQuery, 100, signal);
    const seenPaths = new Set(baseDirEntries.map(e => e.path));
    const entries = [...baseDirEntries, ...recursiveEntries.filter(e => { if (seenPaths.has(e.path)) return false; seenPaths.add(e.path); return true; })];
    if (signal.aborted) return [];
    const scoredEntries = entries
      .map(e => ({ ...e, score: fdQuery ? this.scoreEntry(e.path, fdQuery, e.isDirectory) : 1 }))
      .filter(e => e.score > 0);
    scoredEntries.sort((a, b) => {
      const scoreDiff = b.score - a.score; if (scoreDiff !== 0) return scoreDiff;
      const aDepth = toDisplayPath(a.path).split("/").filter(Boolean).length;
      const bDepth = toDisplayPath(b.path).split("/").filter(Boolean).length;
      const depthDiff = aDepth - bDepth; if (depthDiff !== 0) return depthDiff;
      const lengthDiff = a.path.length - b.path.length; if (lengthDiff !== 0) return lengthDiff;
      return a.path.localeCompare(b.path);
    });
    const topEntries = scoredEntries.slice(0, 20);
    const suggestions = [];
    for (const { path: entryPath, isDirectory } of topEntries) {
      const pathWithoutSlash = isDirectory ? entryPath.slice(0, -1) : entryPath;
      const displayPath = scopedQuery ? this.scopedPathForDisplay(scopedQuery.displayBase, pathWithoutSlash) : pathWithoutSlash;
      const entryName = basename(pathWithoutSlash);
      const completionPath = isDirectory ? `${displayPath}/` : displayPath;
      const value = buildCompletionValue(completionPath, { isDirectory, isAtPrefix: true, isQuotedPrefix });
      suggestions.push({ value, label: entryName + (isDirectory ? "/" : ""), description: displayPath });
    }
    return suggestions;
  } catch { return []; }
}
```

**必须保留的细节**：

- 空 query 时 `score = 1`（所有条目同分），排序退化为"深度 → 长度 → localeCompare"。
- 两次 fd 调用**串行**：先 `--max-depth 1`（直接子项），再全递归；去重按 `path` 字符串（含尾部 `/`）。
- 截断 20 条在**打分排序之后**。
- `description` 是**显示路径**（scoped 时拼回 `displayBase`）；`label` 是 basename + 可选 `/`。
- `isAtPrefix: true` 硬编码（模糊补全只服务 `@`）。
- 深度按 `path.split("/").filter(Boolean).length` 计算（尾部 `/` 不增加深度）。
- 空 query 时 `@` 补全仍会返回前 20 条（不是全部）——但 `@` 的 `getSuggestions` 分支要求 fd 非空。

#### 2.10.10 `shouldTriggerFileCompletion`

```ts
shouldTriggerFileCompletion(lines, cursorLine, cursorCol): boolean {
  const currentLine = lines[cursorLine] || "";
  const textBeforeCursor = currentLine.slice(0, cursorCol);
  if (textBeforeCursor.trim().startsWith("/") && !textBeforeCursor.trim().includes(" ")) return false;
  return true;
}
```

实测：`(["/mod"],0,4) → false`；`(["/model x"],0,8) → true`；`(["hi"],0,2) → true`；`(["  /mod"],0,6) → false`。

#### 2.10.11 斜杠命令补全实测（含 `argumentHint`）

命令表 `[{name:"model", description:"Select the active model", argumentHint:"<name>"}, {name:"skill:research-idea", description:"Refine"}, {name:"noDesc"}, {value:"valOnly", label:"valOnly"}]`：

```
"/"      → items: [
             {value:"model", label:"model", description:"<name> — Select the active model"},
             {value:"skill:research-idea", label:"skill:research-idea", description:"Refine"},
             {value:"noDesc", label:"noDesc"},
             {value:"valOnly", label:"valOnly"} ],
           prefix: "/"
"/mod"   → [{value:"model", …, description:"<name> — Select the active model"}], prefix:"/mod"
"/idea"  → [{value:"skill:research-idea", …}], prefix:"/idea"        // skill: 折叠匹配
"/skill" → [{value:"skill:research-idea", …}], prefix:"/skill"       // 全名匹配
"/val"   → [{value:"valOnly", label:"valOnly"}], prefix:"/val"       // AutocompleteItem 形式
"/model x" → null（model 无 getArgumentCompletions）
```

`description` 组合规则逐字：`hint ? (desc ? \`${hint} — ${desc}\` : hint) : desc`（`—` 是 U+2014 EM DASH，两侧各一个半角空格）；`fullDesc || undefined` 使空串变 `undefined`，且 `...(item.description && {description})` 使空描述**不出现在对象里**（JSON 形状不同）。

`skill:` 折叠顺序实测（`autocomplete-skill-slash.test.ts` 的 5 条命令表，Node 26.12.1 实跑）：

```
命令表 = [skill:deep-research, skill:research-idea, skill:to-sidecar, skill:brainstorm, model]

"/idea"      → ["skill:research-idea", "skill:to-sidecar", "skill:deep-research"]
               // 断言：items[0] === "skill:research-idea"；indexOf("skill:deep-research") > indexOf("skill:research-idea")
"/mod"       → ["model"]
"/skill:side"→ ["skill:to-sidecar", "skill:research-idea"]
               // 断言：includes("skill:to-sidecar")
"/skill"     → ["skill:deep-research", "skill:research-idea", "skill:to-sidecar", "skill:brainstorm"]
               // 回归 #9944：filter(startsWith("skill:")) 必须 === 原表中 skill: 命令的**原顺序**
"/skbra"     → ["skill:brainstorm"]
```

**为什么 `/skill` 保持原顺序（逐条实测分数）**：

| 命令 | `fuzzyMatch("skill", 完整名)` | `fuzzyMatch("skill", 折叠名)` |
| --- | --- | --- |
| `skill:deep-research` | `{true, -84}` | `{false, 0}`（`deep-research` 无 `k`） |
| `skill:research-idea` | `{true, -84}` | `{false, 0}` |
| `skill:to-sidecar` | `{true, -84}` | `{false, 0}` |
| `skill:brainstorm` | `{true, -84}` | `{false, 0}` |
| `model` | （非 skill，仅折叠名 `model`） | `{false, 0}` |

因此 `bareNameMatches` 为空 → `fullNameOnlyMatches`（用完整名）命中全部 4 个，**分数全等 -84**，`sort` 稳定 → 保持原表顺序。

**`/idea` 的逐条分数**（解释两段拼接）：

| 命令 | 完整名分数 | 折叠名分数 |
| --- | --- | --- |
| `skill:research-idea` | -33.400000000000006 | -35.8 |
| `skill:to-sidecar` | 16.9 | -10.700000000000001 |
| `skill:deep-research` | 8 | 不匹配 |
| `skill:brainstorm` | 不匹配 | 不匹配 |

`bareNameMatches = [research-idea (-35.8), to-sidecar (-10.7)]`（按分数升序）；
`fullNameOnlyMatches = [deep-research (8)]`（brainstorm 已不在候选，research-idea 已在 bareNameMatchSet 中被排除）。
拼接 → `["skill:research-idea", "skill:to-sidecar", "skill:deep-research"]`，与实测一致。
这验证了"两段拼接、**不是**全局重排"的语义。

---

### 2.11 `src/alt-screen-search.ts`（327 行）— 转录搜索

#### 2.11.1 导出

```ts
export interface AltScreenSearchSegment { row: number; startCol: number; endCol: number; }
export interface AltScreenSearchMatch { segments: AltScreenSearchSegment[]; }
export interface AltScreenSearchResult { matches: AltScreenSearchMatch[]; changed: boolean; }
export class AltScreenSearchIndex {
  search(lines: readonly string[], query: string): AltScreenSearchResult;
}
export function findAltScreenSearchMatches(lines: readonly string[], query: string): AltScreenSearchMatch[];
export function getAltScreenSearchMatchKey(match: AltScreenSearchMatch): string;
export class AltScreenSearchComponent implements Component, Focusable { … }
```

内部类型：

```ts
interface SearchSourceSpan { textStart: number; textEnd: number; row: number; startCol: number; endCol: number; linearColumns: boolean; }
interface SearchCorpus { text: string; spans: SearchSourceSpan[]; }
const PRINTABLE_ASCII = /^[\x20-\x7e]*$/;
const segmenter = getGraphemeSegmenter();
```

#### 2.11.2 `buildSearchCorpus(lines)`（私有）

把渲染后的行规范化为一个以**单空格分隔**的可搜索串，同时记录每段文本回映射到 (row, col) 的 span。

```
chunks = []; spans = []; textLength = 0; pendingSeparator = false;
appendSeparator = () => { if (!pendingSeparator) return; chunks.push(" "); textLength += 1; pendingSeparator = false; };

for (let row = 0; row < lines.length; row++) {
  const line = stripTerminalSequences(lines[row] ?? "");
  let column = 0;
  if (PRINTABLE_ASCII.test(line)) {
    // 快路径：按非空格连续 run 整段索引
    let index = 0;
    while (index < line.length) {
      if (line.charCodeAt(index) === 0x20) { if (textLength > 0) pendingSeparator = true; column += 1; index += 1; continue; }
      let end = index + 1;
      while (end < line.length && line.charCodeAt(end) !== 0x20) end += 1;
      appendSeparator();
      const text = line.slice(index, end);
      chunks.push(text);
      spans.push({ textStart: textLength, textEnd: textLength + text.length, row,
                   startCol: column, endCol: column + text.length, linearColumns: true });
      textLength += text.length; column += text.length; index = end;
    }
  } else {
    for (const grapheme of segmenter.segment(line)) {
      const text = grapheme.segment;
      const width = visibleWidth(text);
      if (/^\s+$/u.test(text)) { if (textLength > 0) pendingSeparator = true; column += width; continue; }
      appendSeparator();
      chunks.push(text);
      spans.push({ textStart: textLength, textEnd: textLength + text.length, row,
                   startCol: column, endCol: column + width, linearColumns: false });
      textLength += text.length; column += width;
    }
  }
  if (textLength > 0) pendingSeparator = true;
}
return { text: chunks.join(""), spans };
```

**精确语义**：

- 行内空白（空格/任意 `\s` 簇）**折叠成单个空格**，且**行首空白被丢弃**（`textLength === 0` 时不设置 pending；也不推进文本）。但**列号仍推进**（所以列映射正确）。
- 行与行之间：只要已有文本，行尾设置 `pendingSeparator`，下一段文本前插入一个空格（**空行不产生额外空格**，只保留一个）。
- ASCII 快路径：`linearColumns = true`，`startCol/endCol` 与文本偏移**线性对应**（等宽 1）。
- 非 ASCII 路径：`linearColumns = false`，整簇映射到 `[startCol, startCol+width)`（簇内无法细分）；命中落在簇内时列区间取**整簇**。
- 制表符：`visibleWidth("\t") = 3`，非 ASCII 路径（`\t` 不在 `[\x20-\x7e]`）→ 空白簇 → 只推进 3 列。

实测：

```
buildSearchCorpus(["alpha QUICK","brown fox"]).text = "alpha QUICK brown fox"
findAltScreenSearchMatches(["alpha QUICK","brown fox"], "quick brown")
  = [{segments:[{row:0,startCol:6,endCol:11},{row:1,startCol:0,endCol:5}]}]

findAltScreenSearchMatches(["\x1b[31mfoo  bar\x1b[0m", "A界🙂éZ"], "oo   bar\nA界🙂é")
  = [{segments:[{row:0,startCol:1,endCol:3},{row:0,startCol:5,endCol:8},{row:1,startCol:0,endCol:6}]}]
```

（第二例说明：query 被 `normalizeQuery` 压成 `"oo bar A界🙂é"`，语料中 "foo  bar" → `foo` + 空格 + `bar`；命中从 `foo` 的第 2 个字符到 `bar` → 两个段（空格处断开）`1..3` 与 `5..8`；第二行 `A界🙂é` 宽 1+2+2+1 = 6 → 段 `0..6`。）

```
findAltScreenSearchMatches(["abc","def"], "abcdef")   = []            // 跨行无空格 → 语料是 "abc def"
findAltScreenSearchMatches(["abc","def"], "abc def")  = [{segments:[{row:0,0,3},{row:1,0,3}]}]
findAltScreenSearchMatches(["\tX"], "X")              = [{segments:[{row:0,startCol:3,endCol:4}]}]
findAltScreenSearchMatches(lines, "")                 = []
findAltScreenSearchMatches(lines, "zzz")              = []
```

#### 2.11.3 `normalizeQuery` / `escapeRegExp` / `findSearchCorpusMatches`

```ts
function normalizeQuery(query: string): string { return query.replace(/\s+/gu, " ").trim(); }
function escapeRegExp(text: string): string { return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }

function findSearchCorpusMatches(corpus, normalizedQuery) {
  if (!normalizedQuery) return [];
  const expression = new RegExp(escapeRegExp(normalizedQuery), "giu");   // 忽略大小写 + 全局 + unicode
  const matches = [];
  let spanIndex = 0;
  for (const match of corpus.text.matchAll(expression)) {
    const start = match.index, end = start + match[0].length;
    while (spanIndex < corpus.spans.length && corpus.spans[spanIndex]!.textEnd <= start) spanIndex += 1;
    const segments = [];
    for (let index = spanIndex; index < corpus.spans.length; index++) {
      const span = corpus.spans[index]!;
      if (span.textStart >= end) break;
      if (span.textEnd <= start) continue;
      const startCol = span.linearColumns ? span.startCol + Math.max(start, span.textStart) - span.textStart : span.startCol;
      const endCol   = span.linearColumns ? span.startCol + Math.min(end, span.textEnd) - span.textStart : span.endCol;
      const previous = segments[segments.length - 1];
      if (previous && previous.row === span.row && startCol <= previous.endCol) previous.endCol = Math.max(previous.endCol, endCol);
      else segments.push({ row: span.row, startCol, endCol });
    }
    while (spanIndex < corpus.spans.length && corpus.spans[spanIndex]!.textEnd <= end) spanIndex += 1;
    if (segments.length > 0) matches.push({ segments });
  }
  return matches;
}
```

要点：

- 正则 flags 逐字 `"giu"`；`i` 是 **Unicode 简单大小写折叠**（`"ß"` 不匹配 `"SS"`；`"İ"` 的行为按 JS 规范）。Rust 需 `regex` crate 的 `(?i)` + `unicode` 或 `fancy-regex`。
- 匹配是**子串字面量**（query 已被 `escapeRegExp`），但因 `g` 会找出**所有不重叠**出现；`matchAll` 从上次结束处继续。
- `spanIndex` 单调推进（两个 while），因此整体 O(spans + matches)。
- 合并规则：同一行且 `startCol <= previous.endCol` → 扩展 `previous.endCol`（**允许相邻接合**，`<=` 不是 `<`）。
- `linearColumns=false` 的跨度在簇内部命中时仍返回整簇列区间（不精确到列）。

#### 2.11.4 `AltScreenSearchIndex`

```ts
export class AltScreenSearchIndex {
  private sourceLines: string[] | undefined;
  private corpus: SearchCorpus | undefined;
  private normalizedQuery: string | undefined;
  private matches: AltScreenSearchMatch[] = [];

  search(lines, query) {
    let sourceChanged = this.sourceLines?.length !== lines.length;
    if (!sourceChanged && this.sourceLines) {
      for (let index = 0; index < lines.length; index++) {
        if (this.sourceLines[index] === lines[index]) continue;
        sourceChanged = true; break;
      }
    }
    if (sourceChanged || !this.corpus) { this.sourceLines = Array.from(lines); this.corpus = buildSearchCorpus(lines); }
    const normalizedQuery = normalizeQuery(query);
    const changed = sourceChanged || normalizedQuery !== this.normalizedQuery;
    if (changed) { this.normalizedQuery = normalizedQuery; this.matches = findSearchCorpusMatches(this.corpus, normalizedQuery); }
    return { matches: this.matches, changed };
  }
}
```

要点：

- 行比较是**逐行全等字符串比较**（O(行数 × 行长)）；长度不同立即视为变化。
- 未变化时返回**同一个 `matches` 数组引用**（测试用 `strictEqual(cached.matches, initial.matches)` 断言）。
- `changed` 只看"行变了或归一化 query 变了"（**query 仅空白差异不算变化**）。

实测：

```
index.search(lines,"hello")  → {matches:[…4 条…], changed:true}
index.search(lines,"hello")  → {matches:<同一引用>, changed:false}
index.search(lines,"  hello ")→ changed:false（归一化后相同）
index.search(lines2,"hello")（lines2[0] 改了） → changed:true
```

#### 2.11.5 `findAltScreenSearchMatches` / `getAltScreenSearchMatchKey`

```ts
export function findAltScreenSearchMatches(lines, query) {
  const normalizedQuery = normalizeQuery(query);
  return normalizedQuery ? findSearchCorpusMatches(buildSearchCorpus(lines), normalizedQuery) : [];
}
export function getAltScreenSearchMatchKey(match) {
  const first = match.segments[0];
  const last  = match.segments[match.segments.length - 1];
  return first && last ? `${first.row}:${first.startCol}:${last.row}:${last.endCol}` : "";
}
```

实测 key：`["0:0:0:5","1:0:1:5","2:0:2:5","3:7:3:12"]`；空 segments → `""`。

#### 2.11.6 `AltScreenSearchComponent`

```ts
export class AltScreenSearchComponent implements Component, Focusable {
  private readonly input = new Input({
    prompt: " ",
    placeholder: "Find in transcript",
    placeholderStyle: (text) => `\x1b[2m${text}\x1b[22m`,
  });
  private readonly onQueryChange: (query: string) => void;
  private readonly navigationButtonStyle: (text: string, hovered: boolean) => string;
  private resultCount = 0;
  private resultIndex = -1;
  private previousButtonStart = -1;
  private previousButtonEnd = -1;
  private nextButtonStart = -1;
  private nextButtonEnd = -1;
  private hoveredNavigationDirection: -1 | 1 | undefined;
  private _focused = false;

  constructor(onQueryChange: (query: string) => void,
              navigationButtonStyle: (text: string, hovered: boolean) => string = (text) => text);
  get focused(): boolean; set focused(value: boolean);       // 同时设置 input.focused
  setResult(index: number, count: number): void;
  getNavigationDirectionAt(row: number, column: number): -1 | 1 | undefined;
  setHoveredNavigationDirection(direction: -1 | 1 | undefined): boolean;   // 变化才返回 true
  handleInput(data: string): void;                            // 仅当 input 值变化时调用 onQueryChange
  invalidate(): void;
  render(width: number): string[];
}
```

`handleInput`：

```
const previous = this.input.getValue();
this.input.handleInput(data);
const query = this.input.getValue();
if (query !== previous) this.onQueryChange(query);
```

（因此方向键、Home/End 等不改变值的按键不会触发 `onQueryChange`；粘贴会被 `Input` 处理。）

`getNavigationDirectionAt(row, column)`：`if (row !== 2) return undefined;` 然后按上/下按钮区间（`>= start && < end`）返回 `-1` / `1`。**行号是相对 overlay 顶部的 0-based**，按钮恒在第 2 行（最后一行边框）。

`setHoveredNavigationDirection(direction)`：与当前值相同 → 返回 `false`（不重绘）；否则更新并返回 `true`。

##### `render(width)`（**逐字节必须复刻**）

```ts
const safeWidth = Math.max(1, width);
const innerWidth = Math.max(0, safeWidth - 2);

const formatKey = (key: string | undefined): string =>
  key ? key.split("+").map(part => {
          if (process.platform === "darwin" && part.toLowerCase() === "alt") return "Option";
          return part.charAt(0).toUpperCase() + part.slice(1);
        }).join("+")
      : "Unbound";

const keybindings = getKeybindings();
const previousKey = formatKey(keybindings.getKeys("tui.altScreen.searchPrevious")[0]);
const nextKey     = formatKey(keybindings.getKeys("tui.altScreen.searchNext")[0]);

const query = this.input.getValue();
const result = !query ? "" : this.resultCount === 0 ? "No matches" : `${this.resultIndex + 1}/${this.resultCount}`;
const resultSpace = Math.max(0, innerWidth - 3);
const visibleResult = truncateToWidth(result, resultSpace, "");
const resultText = visibleResult ? `\x1b[2m ${visibleResult} \x1b[22m` : "";
const inputWidth = Math.max(0, innerWidth - visibleWidth(resultText));
const inputLine = truncateToWidth(this.input.render(Math.max(1, inputWidth))[0] ?? "", inputWidth, "");
const inputPadding = " ".repeat(Math.max(0, inputWidth - visibleWidth(inputLine)));
const content = `${inputLine}${inputPadding}${resultText}`;

let previousButton = `↑ ${previousKey}`;
let nextButton     = `↓ ${nextKey}`;
let separator = " · ";
const outerGapWidth = 1;
const availableControlsWidth = Math.max(0, innerWidth - outerGapWidth * 2 - 1);
let controlsWidth = visibleWidth(previousButton) + visibleWidth(separator) + visibleWidth(nextButton);
if (controlsWidth > availableControlsWidth) {
  previousButton = "↑"; nextButton = "↓"; separator = " ";
  controlsWidth = visibleWidth(previousButton) + visibleWidth(separator) + visibleWidth(nextButton);
}
const showButtons = controlsWidth <= availableControlsWidth;
const renderedButtons = showButtons
  ? this.navigationButtonStyle(previousButton, this.hoveredNavigationDirection === -1)
    + separator
    + this.navigationButtonStyle(nextButton, this.hoveredNavigationDirection === 1)
  : "";
const outerGapsWidth = showButtons ? outerGapWidth * 2 : 0;
const rightRuleWidth = renderedButtons && innerWidth > controlsWidth + outerGapsWidth ? 1 : 0;
const leftRuleWidth = Math.max(0, innerWidth - (showButtons ? controlsWidth : 0) - outerGapsWidth - rightRuleWidth);
const previousStart = 1 + leftRuleWidth + outerGapWidth;

this.previousButtonStart = showButtons ? previousStart : -1;
this.previousButtonEnd   = showButtons ? previousStart + visibleWidth(previousButton) : -1;
this.nextButtonStart     = showButtons ? this.previousButtonEnd + visibleWidth(separator) : -1;
this.nextButtonEnd       = showButtons ? this.nextButtonStart + visibleWidth(nextButton) : -1;

if (safeWidth === 1) return ["┌", "│", "└"];
return [
  `┌${"─".repeat(innerWidth)}┐`,
  `│${content}│`,
  `└${"─".repeat(leftRuleWidth)}${renderedButtons ? " " : ""}${renderedButtons}${renderedButtons ? " " : ""}${"─".repeat(rightRuleWidth)}┘`,
];
```

**逐字界面字符串**：

| 位置 | 字符串 |
| --- | --- |
| input prompt | `" "`（一个空格） |
| placeholder | `"Find in transcript"` |
| placeholder 样式 | `\x1b[2m` + text + `\x1b[22m` |
| 结果计数 | `"No matches"`；或 `` `${index + 1}/${count}` `` |
| 结果样式 | `\x1b[2m ` + visibleResult + ` \x1b[22m`（两侧各一空格） |
| 上一键默认 | `"↑ Shift+Enter"` |
| 下一键默认 | `"↓ Enter"` |
| 分隔符 | `" · "`（U+00B7 两侧空格） |
| 窄屏退化 | `"↑"` / `"↓"` / `" "` |
| 边框 | `┌` `┐` `└` `┘` `─` `│` |
| 未绑定 | `"Unbound"` |
| macOS | `alt` → `"Option"`（`part.toLowerCase() === "alt"`） |

**键位格式化**：`key.split("+")` 后每段 `part.charAt(0).toUpperCase() + part.slice(1)`（**只大写首字符，其余保持**）。默认键位来自 `TUI_KEYBINDINGS`：`searchPrevious` 首个是 `"shift+enter"` → `"Shift+Enter"`；`searchNext` 首个是 `"enter"` → `"Enter"`。

**实测渲染**（`getKeybindings()` 默认键位、非 macOS）：

```
render(1)  = ["┌","│","└"]
render(2)  = ["┌┐","││","└┘"]
render(3)  = ["┌─┐","│ │","└─┘"]
render(4)  = ["┌──┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2m\x1b[0m\x1b[22m│","└──┘"]
render(5)  = ["┌───┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mi\x1b[0m\x1b[22m│","└───┘"]
render(6)  = ["┌────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2min\x1b[0m\x1b[22m│","└────┘"]
render(10) = ["┌────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in\x1b[0m\x1b[22m│","└── ↑ ↓ ─┘"]
render(12) = ["┌──────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in t\x1b[0m\x1b[22m│","└──── ↑ ↓ ─┘"]
render(20) = ["┌──────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcrip\x1b[0m\x1b[22m│","└──────────── ↑ ↓ ─┘"]
render(32) = ["┌──────────────────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcript\x1b[22m           │","└──── ↑ Shift+Enter · ↓ Enter ─┘"]
render(40) = ["┌──────────────────────────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcript\x1b[22m                   │","└──────────── ↑ Shift+Enter · ↓ Enter ─┘"]
render(48) = ["┌──────────────────────────────────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcript\x1b[22m                           │","└──────────────────── ↑ Shift+Enter · ↓ Enter ─┘"]
render(60) = ["┌──────────────────────────────────────────────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcript\x1b[22m                                       │","└──────────────────────────────── ↑ Shift+Enter · ↓ Enter ─┘"]
render(80) = ["┌──────────────────────────────────────────────────────────────────────────────┐","│ \x1b_kiana:fc\x07\x1b[2mF\x1b[22m\x1b_kiana:/fc\x07\x1b[2mind in transcript\x1b[22m                                                           │","└──────────────────────────────────────────────────── ↑ Shift+Enter · ↓ Enter ─┘"]
```

**关于假光标**：`\x1b_kiana:fc\x07 … \x1b_kiana:/fc\x07` 是 `Input` 渲染的假光标（`renderFakeCursor(text)` = `FAKE_CURSOR_START + text + FAKE_CURSOR_END`）。`Input.focused` 默认 `false`，但 `render` 中**无条件**调用 `renderFakeCursor(...)`；`focused` 只决定是否**额外**插入 `CURSOR_MARKER`（`\x1b_kiana:c\x07`）。因此 `AltScreenSearchComponent` 无论聚焦与否都会输出假光标标记。逐字细节（属于 tui-components，但本单元的渲染断言依赖）：

- 有值时的行：`prompt(" ") + beforeCursor + marker? + \x1b_kiana:fc\x07 + atCursor + \x1b_kiana:/fc\x07 + afterCursor + padding`。
- 空值 + placeholder 时：`prompt + marker? + \x1b_kiana:fc\x07 + placeholderStyle(atCursor) + \x1b_kiana:/fc\x07 + placeholderStyle(afterCursor) + padding`；`atCursor` 是 placeholder 的**第一个字素**（无则 `" "`），`afterCursor = placeholder.slice(atCursor.length)`。
- `placeholderStyle` 是 `text => \x1b[2m${text}\x1b[22m`，所以实测里 `\x1b[2mF\x1b[22m` 与 `\x1b[2mind in transcript\x1b[22m` 分成两段。
- `availableWidth = width - visibleWidth(prompt) = width - 1`；`availableWidth <= 0` → `[truncateToWidth(prompt, width, "")]`。实测 `render(1)` 第二行是 `"│"`（内容空）：`AltScreenSearchComponent` 先算 `innerWidth = 0` → `inputWidth = 0` → `this.input.render(max(1, 0) = 1)` 的 `availableWidth = 1 - 1 = 0` → 返回 `[truncateToWidth(" ", 1, "")]` = `[" "]`，随后外层 `truncateToWidth(inputLine, 0, "")` = `""`。
- 无值时**末尾 padding**：`" ".repeat(max(0, availableWidth - visibleWidth(textWithCursor)))`；有值时同理（`visualLength` 含标记，宽度为 0）。
- **`placeholder` 的裁剪（重新推导并实测确认）**：placeholder `"Find in transcript"` 的 `visibleWidth = 18`。`Input.render(width)` 的 `availableWidth = width - visibleWidth(prompt) = width - 1`，并用 `truncateToWidth(this.placeholder, availableWidth, "")` 裁剪（省略号为空串 → **不产生** `"..."`），`AltScreenSearchComponent` 再把 `Input.render(...)` 的第 0 行用 `truncateToWidth(..., inputWidth, "")` 裁一次。
  - `render(32)` 及更宽：`innerWidth = 30`、`resultText = ""`（空 query）→ `inputWidth = 30`、`Input.render(30)` 的 `availableWidth = 29 ≥ 18` → placeholder 完整输出（18 列），余下 12 列是尾部 padding。
  - `render(21)`：`innerWidth = 19` → `inputWidth = 19` → `availableWidth = 18` → **恰好完整**（`"Find in transcript"`），无 padding。
  - `render(20)`：`innerWidth = 18` → `inputWidth = 18` → `availableWidth = 17` → placeholder 裁到 `"Find in transcrip"`（17 列），无 padding。**第二行内容宽 = prompt(1) + 17 = 18 = innerWidth**，整行宽 20。
  - `render(19)`：`innerWidth = 17` → `availableWidth = 16` → `"Find in transcri"`。
  - `render(4)`：`innerWidth = 2` → `inputWidth = 2` → `Input.render(2)` 的 `availableWidth = 1` → 输出 `placeholderStyle("F") + placeholderStyle("")`，外层裁到 2 列不变 → 第二行 `"│ F│"`。
  - **`render(1..3)`**：`innerWidth ≤ 1` → `inputWidth ≤ 1` → `Input.render(1)` 的 `availableWidth = 0` → 返回 `[truncateToWidth(" ", 1, "")]` = `[" "]`，外层 `truncateToWidth(" ", inputWidth≤1, "")`；`render(1)` 走 `safeWidth === 1` 早退 `["┌","│","└"]`；`render(2)`/`render(3)` 的第二行为 `"││"`/`"│ │"`。
  结论：**不存在**“placeholder 裁到 17 而 `inputPadding = 1`”这种混淆；`inputPadding` 只在有 `resultText`（非空 query）时才可能非零。

**几何实测（用于鼠标命中，全部经过真实渲染确认）**：

```
render(w)  previousStart previousEnd nextStart nextEnd
1..7       -1            -1          -1        -1        （退化后的 controlsWidth=3 > availableControlsWidth）
8          2             3           4         5
9          3             4           5         6
10         4             5           6         7
11         5             6           7         8
12         6             7           8         9
13         7             8           9         10
14         8             9           10        11
15         9             10          11        12
16         10            11          12        13
17         11            12          13        14
18         12            13          14        15
19         13            14          15        16
20         14            15          16        17
21         15            16          17        18
22         16            17          18        19
23         17            18          19        20
24         18            19          20        21
25         19            20          21        22
26         20            21          22        23
27         21            22          23        24        ← 最后一档退化标签（仍为 "↑" / "↓"）
28         2             15          18        25        ← 恰好切换回完整标签 "↑ Shift+Enter · ↓ Enter"
29         3             16          19        26
30         4             17          20        27
40         14            27          30        37
48         22            35          38        45
80         54            67          70        77
120        94            107         110       117
```

推导：完整标签 `previousButton = "↑ Shift+Enter"`（宽 13）、`separator = " · "`（宽 3）、`nextButton = "↓ Enter"`（宽 7）→ `controlsWidth = 23`；`availableControlsWidth = innerWidth - 1*2 - 1 = innerWidth - 3`。`23 <= innerWidth - 3` ⟺ `innerWidth >= 26` ⟺ **`width >= 28`**（实测确认：27 仍是 `↑ ↓`，28 切换为完整标签）。退化后 `controlsWidth = 1 + 1 + 1 = 3`，`3 <= innerWidth - 3` ⟺ `innerWidth >= 6` ⟺ **`width >= 8`**（实测：7 无按钮，8 起有）。不显示时 `leftRuleWidth = innerWidth`（`render(6)` → `"└────┘"`；`render(4)` → `"└──┘"`）；显示时 `leftRuleWidth = innerWidth - controlsWidth - 2 - rightRuleWidth`，其中 `rightRuleWidth = 1` 当 `innerWidth > controlsWidth + 2`（在 `width >= 28` 的完整标签区间恒为 1；在 `8..27` 的退化区间同样为 1，因为 `controlsWidth = 3`、`innerWidth >= 6 > 5`），因此 `previousStart = 1 + leftRuleWidth + 1`。
- 校验：`width=8` → `innerWidth=6`、`leftRuleWidth = 6-3-2-1 = 0`、`previousStart = 1+0+1 = 2`，实测 **2**。
- `width=10` → `innerWidth=8`、`leftRuleWidth = 2`、`previousStart = 4`，实测 **4**。
- `width=40` → `innerWidth=38`、`leftRuleWidth = 38-23-2-1 = 12`、`previousStart = 14`，实测 **14**。
- `width=28` → `innerWidth=26`、`leftRuleWidth = 26-23-2-1 = 0`、`previousStart = 2`，实测 **2**。

结果态实测：

```
query="abc", setResult(2,7):
render(40) 第二行 = "│ abc\x1b_kiana:fc\x07 \x1b_kiana:/fc\x07                            \x1b[2m 3/7 \x1b[22m│"
```

（`3/7` = `resultIndex + 1` / `resultCount`；`resultText` 两侧各一空格；`inputPadding` 填满剩余宽度。）

`resultCount === 0` 且 query 非空：

```
render(6)  = ["┌────┐","│  N │","└────┘"]      // 结果被裁剪到 1 列 → "N"
render(10) = ["┌────────┐","│  No ma │","└── ↑ ↓ ─┘"]
render(12) = ["┌──────────┐","│  No matc │","└──── ↑ ↓ ─┘"]
render(20) = ["┌──────────────────┐","│ zzz   No matches │","└──────────── ↑ ↓ ─┘"]
```

**无结果态的推导**（`query = "zzz"`、`resultCount = 0` → `result = "No matches"`）：

| 宽度 | innerWidth | resultSpace | visibleResult | resultText 宽 | inputWidth | 第二行（去 ANSI） |
| --- | --- | --- | --- | --- | --- | --- |
| 6 | 4 | 1 | `"N"` | 3 | 1 | `"  N "`（input 只剩 prompt 空格） |
| 10 | 8 | 5 | `"No ma"` | 7 | 1 | `" No ma "` |
| 12 | 10 | 7 | `"No matc"` | 9 | 1 | `" No matc "` |
| 20 | 18 | 15 | `"No matches"`（10） | 12 | 6 | `"zzz   No matches "`（`input "zzz"` + 3 padding + resultText） |

`resultText = visibleResult ? "\x1b[2m " + visibleResult + " \x1b[22m" : ""`；`inputWidth = max(0, innerWidth - visibleWidth(resultText))`；`inputLine = truncateToWidth(input.render(max(1, inputWidth))[0] ?? "", inputWidth, "")`（**注意**：`Input.render` 收到的是 `max(1, inputWidth)`，即使 `inputWidth = 0` 也按 1 渲染，再由外层 `truncateToWidth(…, 0, "")` 裁成空串）。

**`getNavigationDirectionAt` 实测**（`render(48)` 后，`controls = 第三行去 ANSI`）：

```
getNavigationDirectionAt(2, controls.indexOf("↑"))                = -1
getNavigationDirectionAt(2, controls.indexOf("Shift+Enter") + 5)  = -1
getNavigationDirectionAt(2, controls.indexOf("·"))                = undefined
getNavigationDirectionAt(2, controls.indexOf("↓"))                = 1
getNavigationDirectionAt(2, controls.lastIndexOf("Enter") + 2)    = 1
getNavigationDirectionAt(0, 5)                                    = undefined   // 行号 ≠ 2
```

**与 `tui-alt-screen.ts` 的集成契约**（消费者视角，测试依赖）：

- `toggleSearch()` 用 `showOverlay(component, { anchor:"top-right", width:"40%", minWidth:32, margin:1 })`。
- 搜索面板聚焦时视口按键（`ctrl+g` / `enter`）走搜索导航；`escape`（`tui.altScreen.searchClose`）关闭并恢复编辑器焦点。
- `refreshSearch` 用二分查找把 `anchorRow` 映射到第一个 `segments[0].row >= anchorRow` 的匹配；`next`/`previous` 用 `(base ± 1) % matches.length` 环绕。
- 高亮：`applySearchHighlights` 用 `sliceByColumn` 三段拼接，当前项用 `searchCurrentMatchStyle`（默认 `\x1b[1;7m…\x1b[22;27m`），其它用 `searchMatchStyle`（默认 `\x1b[4m…\x1b[24m`）。
- `getAltScreenSearchMatchKey` 用作"跨查询保持选中项"的稳定键。

---

## 3. 用户可见面（逐字）

本单元是库层（无 CLI、无 HTTP、无配置文件），用户可见面由**字符串常量、ANSI 序列、按键绑定与布局尺寸**构成。以下全部必须在 Rust 端逐字节一致。

### 3.1 错误消息（逐字，含插值）

`colors.ts` 的所有 `throw new Error(...)`（`toString` 用 JS 数字格式化，见本节末尾的 ECMAScript Number::toString 段）：

| 触发条件 | 消息 |
| --- | --- |
| `indexedColor` 非整数或越界 | `ANSI color index must be an integer from 0 to 255: ${index}` |
| `rgbColor` 任一通道非有限 | `${name} must be finite`（`name` ∈ `r`/`g`/`b`） |
| `rgbColor` 任一通道越界 | `${name} must be between 0 and 255: ${value}` |
| `oklchColor` l/c/h 非有限 | `${name} must be finite`（`name` ∈ `l`/`c`/`h`） |
| `oklchColor` l 越界 | `l must be between 0 and 1: ${l}` |
| `oklchColor` c < 0 | `c must not be negative: ${c}` |
| `okhslColor` h/s/l 非有限 | `${name} must be finite`（`name` ∈ `h`/`s`/`l`） |
| `okhslColor` s 越界 | `s must be between 0 and 1: ${s}` |
| `okhslColor` l 越界 | `l must be between 0 and 1: ${l}` |
| `parseColor` 无法识别 | `Invalid color value: ${value}` |
| `mixColors` amount 非有限 | `amount must be finite` |
| `mixColors` amount 越界 | `amount must be between 0 and 1: ${amount}` |

实测插值示例：`indexedColor(300)` → `ANSI color index must be an integer from 0 to 255: 300`；`indexedColor(1.5)` → `…: 1.5`；`rgbColor(-1,0,0)` → `r must be between 0 and 255: -1`；`rgbColor(NaN,0,0)` → `r must be finite`；`oklchColor(2,0,0)` → `l must be between 0 and 1: 2`；`oklchColor(0,-0.1,0)` → `c must not be negative: -0.1`；`okhslColor(0,2,0)` → `s must be between 0 and 1: 2`；`parseColor("nope")` → `Invalid color value: nope`；`parseColor("#abcd")` → `Invalid color value: #abcd`。

**JS 数字格式化规则（Rust 必须桥接）**：`String(1.5)="1.5"`、`String(1e21)="1e+21"`、`String(-0)="0"`、`String(0.1+0.2)="0.30000000000000004"`、`String(1e-7)="1e-7"`、`String(123456789012345680000)="123456789012345680000"`。Rust 的 `f64::to_string()` 在 `1e21` 处输出 `1000000000000000000000`（不同），在 `1e-7` 处输出 `0.0000001`（不同）。需实现 ECMAScript Number::toString（最短往返表示 + 指数阈值 `>=1e21` 或 `<1e-6`）。

### 3.2 渲染字符串与 ANSI 序列

#### 3.2.1 `styleText` / `styleTextWithAnsi` 的属性序列（逐字）

| 属性 | 开启序列 | 关闭序列 |
| --- | --- | --- |
| 前景（truecolor） | `\x1b[38;2;R;G;Bm`（R/G/B 经 `Math.round`） | `\x1b[39m` |
| 前景（256） | `\x1b[38;5;Nm` | `\x1b[39m` |
| 前景（indexed） | `\x1b[38;5;<index>m`（**不经**模式和量化） | `\x1b[39m` |
| 背景（truecolor） | `\x1b[48;2;R;G;Bm` | `\x1b[49m` |
| 背景（256） | `\x1b[48;5;Nm` | `\x1b[49m` |
| 背景（indexed） | `\x1b[48;5;<index>m` | `\x1b[49m` |
| `bold` | `\x1b[1m` | `\x1b[22m`（bold 与 dim **共享**） |
| `dim` | `\x1b[2m` | `\x1b[22m`（同上） |
| `italic` | `\x1b[3m` | `\x1b[23m` |
| `underline` | `\x1b[4m` | `\x1b[24m` |
| `inverse` | `\x1b[7m` | `\x1b[27m` |
| `strikethrough` | `\x1b[9m` | `\x1b[29m` |

**构建顺序（逐字，与属性表顺序不同）**：源码先 fg 后 bg，再 bold、dim、italic、underline、inverse、strikethrough。`prefix` 按此顺序**追加**；`suffix` 则每次**前置**（`suffix = 新关闭码 + 旧 suffix`），因此最终关闭顺序是开启的**逆序**。

`bold`/`dim` 的特殊之处：两者各自 `prefix +=`，但关闭码只在 `if (bold || dim)` 时**插入一次** `\x1b[22m`（不是两次）。因此同时开 bold+dim 时只有一个 `\x1b[22m`。

**实测（逐字）**：

```
styleText("x", {bold,dim,italic,underline,inverse,strikethrough, fg:"#ff0000", bg:5}, "truecolor")
  = "\x1b[38;2;255;0;0m\x1b[48;5;5m\x1b[1m\x1b[2m\x1b[3m\x1b[4m\x1b[7m\x1b[9mx"
  + "\x1b[29m\x1b[27m\x1b[24m\x1b[23m\x1b[22m\x1b[49m\x1b[39m"
styleText("x", {fg:"#ff0000"}, "256color") = "\x1b[38;5;196mx\x1b[39m"
styleText("x", {}, "truecolor")            = "x"            // 无前缀后缀
styleText("x", {bold:true}, "truecolor")   = "\x1b[1mx\x1b[22m"
```

**注意 `styleTextWithAnsi` 的签名是 4 参**：`(text, fgAnsi: string | undefined, bgAnsi: string | undefined, options: TextAttributes)`。它**忽略** `options` 上的 `fg`/`bg`（只读六个布尔属性），因为颜色序列已由调用方预算好。`styleText` 的 `options.fg &&` 短路意味着 `fg: undefined` → `fgAnsi = undefined`。

#### 3.2.2 `wrapTextWithAnsi` 的行尾序列

每行末尾按需追加：

- 若行末有活动的下划线 → `\x1b[24m`（**只**重置下划线，不 reset 背景）；
- 若行内有活动的 OSC 8 链接 → `\x1b]8;;\x07`（或与开链相同的终止符 ST/BEL，见 §2.1.10）；
- **不**追加 `\x1b[0m`（背景/前景继续带到下一行的续行前缀 `getActiveCodes()`）。

#### 3.2.3 `truncateToWidth` 的省略号包围

若发生截断且 `ellipsis` 非空：`\x1b[0m` + ellipsis + `\x1b[0m`（两侧都是完整 reset，**无论原文是否含 ANSI**）；若还有活动链接，先关闭（`getActiveOsc8Close`）。实测：
`truncateToWidth("Hello World", 8)` = `"Hello\x1b[0m...\x1b[0m"`；
`truncateToWidth("Hello", 3)` = `"\x1b[0m...\x1b[0m"`；
`truncateToWidth("Hello", 5)` = `"Hello"`（未截断，无 reset）。
`ellipsis` 为空串时若发生截断 → 结果末尾追加一个 `\x1b[0m`：`truncateToWidth("Hello World", 8, "")` = `"Hello Wo\x1b[0m"`。
（README 里的 `truncateToWidth("Hello World", 8); // "Hello..."` 是**简化示意**，真实输出包含两处 `\x1b[0m`；以源码与测试为准。）

#### 3.2.4 `Input` 假光标标记（跨单元，但本单元的搜索面板会输出）

`\x1b_kiana:fc\x07` … `\x1b_kiana:/fc\x07`（`FAKE_CURSOR_START`/`END`，见 tui-core）。`CURSOR_MARKER` 为 `\x1b_kiana:c\x07`。

#### 3.2.5 `normalizeTerminalOutput` 的 AM 归一化

Thai/Lao AM 元音（U+0E33/U+0EB3）在输出前改写为"分解序列"（详见 §2.1.8）：`ำ` → `ํา`（**U+0E4D NIKHAHIT + U+0E32 SARA AA**，不是空格），`ຳ` → `ໍາ`（**U+0ECD NIGGAHITA + U+0EB2 SARA AA**）（源码逐字；这是终端渲染修正，**只**用于输出，不改变宽度计算）。

#### 3.2.6 搜索面板的边框与内容字符串

见 §2.11.6 的表格。汇总：

```
┌ ─ ┐ │ └ ┘        （U+250C U+2500 U+2510 U+2502 U+2514 U+2518；注意**没有** ┤ ├ ┬ ┴ 等 T 形件）
" "                输入框 prompt（一个空格，U+0020）
"Find in transcript"  占位符（18 列）
"\x1b[2m"…"\x1b[22m"  占位符样式（dim 开 / dim 关）
"No matches"       无结果
"${n}/${m}"        结果计数（1-based；n = selectedIndex+1，m = resultCount）
"\x1b[2m ${result} \x1b[22m"  结果样式（两侧各一个空格，dim）
"↑ Shift+Enter"    上一匹配按钮（默认键位；"↑" U+2191 + 空格）
"↓ Enter"          下一匹配按钮（默认键位；"↓" U+2193 + 空格）
" · "              按钮分隔符（U+00B7 MIDDLE DOT，两侧各一空格）
"↑" / "↓" / " "    窄屏退化（按钮只剩箭头，分隔符退化为单个空格）
"Unbound"          键位为空（`getKeys(...)[0]` 为 undefined）
"Option"           macOS 上的 alt（`part.toLowerCase() === "alt"` 且 `process.platform === "darwin"`）
```

#### 3.2.7 搜索高亮样式（消费者默认值，`tui-alt-screen.ts`）

| 名称 | 默认样式 |
| --- | --- |
| `searchMatchStyle` | `\x1b[4m${text}\x1b[24m`（下划线） |
| `searchCurrentMatchStyle` | `\x1b[1;7m${text}\x1b[22;27m`（粗体 + 反显） |
| `searchNavigationButtonStyle` | `(text) => text`（无样式） |

### 3.3 按键绑定（本单元相关，逐字）

来自 `TUI_KEYBINDINGS`（`keybindings.ts`）：

| 键位 id | 默认键 | 说明（逐字） | 使用者 |
| --- | --- | --- | --- |
| `tui.altScreen.search` | `ctrl+shift+f` | `Search the primary scroll view` | `tui-alt-screen`（打开/关闭搜索面板） |
| `tui.altScreen.searchNext` | `["enter", "ctrl+g"]` | `Select the next search match` | 搜索面板聚焦时 |
| `tui.altScreen.searchPrevious` | `["shift+enter", "ctrl+shift+g"]` | `Select the previous search match` | 搜索面板聚焦时 |
| `tui.altScreen.searchClose` | `escape` | `Close transcript search` | 搜索面板聚焦时 |
| `tui.editor.yank` | `ctrl+y` | `Yank` | `KillRing` 消费方（editor/input） |
| `tui.editor.yankPop` | `alt+y` | `Yank pop` | 同上 |
| `tui.editor.undo` | `ctrl+-` | `Undo` | `UndoStack` 消费方 |
| `tui.editor.deleteWordBackward` | `["ctrl+w", "alt+backspace"]` | `Delete word backward` | `findWordBackward` |
| `tui.editor.deleteWordForward` | `["alt+d", "alt+delete"]` | `Delete word forward` | `findWordForward` |
| `tui.editor.deleteToLineStart` | `ctrl+u` | `Delete to line start` | kill ring（prepend） |
| `tui.editor.deleteToLineEnd` | `ctrl+k` | `Delete to line end` | kill ring（append） |
| `tui.editor.cursorWordLeft` | `["alt+left", "ctrl+left", "alt+b"]` | `Move cursor word left` | `findWordBackward` |
| `tui.editor.cursorWordRight` | `["alt+right", "ctrl+right", "alt+f"]` | `Move cursor word right` | `findWordForward` |

**搜索面板的键位渲染**：`formatKey` 取该动作的**第一个**默认键（`getKeys(id)[0]`），因此面板必然显示 `Shift+Enter` 与 `Enter`（除非用户改了绑定）。

### 3.4 布局尺寸（搜索结果面板）

| 参数 | 值 | 来源 |
| --- | --- | --- |
| overlay 锚点 | `top-right` | `toggleSearch` |
| 宽度 | `"40%"` | 同上 |
| 最小宽度 | `32` | 同上 |
| margin | `1` | 同上 |
| 面板高度 | 3 行（上边框 + 内容 + 下边框） | `render` |
| 内容可用宽 | `innerWidth = max(0, safeWidth - 2)` | `render` |
| 结果区预留 | `resultSpace = max(0, innerWidth - 3)` | `render` |
| 控制区可用宽 | `availableControlsWidth = max(0, innerWidth - 1*2 - 1)` | `render` |
| 窄屏阈值 | `controlsWidth > availableControlsWidth` → 退化为 `↑ ↓` | `render` |

`width: "40%"` 与 `minWidth: 32` 的换算由 tui-core 的 overlay 逻辑保证（`max(32, floor(terminalColumns * 0.4))`，减去 margin 后为实际面板宽）。

### 3.5 LaTeX 渲染的用户可见约束

- `renderLatex` 返回 `string | undefined`；`undefined` 表示"不支持/畸形输入"，调用方（markdown）保留原始文本。
- 输出中**不含** ANSI 序列，只含 Unicode 文本与换行；PUA 标记（U+F0000–U+F0005、U+0000）在返回前由 `normalizeOutput` 全部清除，**不得**泄漏到终端。
- 输出行可含前后空格以对齐（分式/矩阵/居中），但**末尾**已被 `trimEnd` 处理（见 §2.9 的 `renderLatex` 最终组装）。
- 显示模式分式会引入**多行**输出（行间以 `\n` 分隔），行内模式保持单行 `a/b`。

---

## 4. 外部格式（逐字）

本单元**不读取**任何配置文件、环境变量、HTTP 端点或数据库；唯一的"外部格式"是 **fd 子进程的命令行契约** 与 **文件系统读取语义**。

### 4.1 环境变量

| 变量 | 读取位置 | 语义 |
| --- | --- | --- |
| `HOME`（经 `os.homedir()`） | `autocomplete.ts::expandHomePath` | `~/` 展开为 `join(homedir(), rest)`；`~` 展开为 `homedir()`。Linux 上 `os.homedir()` 优先 `$HOME`，未设置时回退 `getpwuid`。Rust 用 `dirs::home_dir()`（同样优先 `$HOME`，macOS 用 `getpwuid`） |
| `PATH` | `child_process.spawn`（隐式，仅当 `fdPath` 不含 `/` 时） | 实际调用方传绝对路径（见 §4.3），但若传裸名 `fd`，Node 会在 `PATH` 中查找 |
| 无其它 | — | 本单元不读 `KIANA_*`、`TERM*`、`COLUMNS` 等 |

`process.platform` 仅在 `alt-screen-search.ts::formatKey` 里读取（`=== "darwin"`），且是在 **每次 `render` 的每个键段**上读取（`map` 内），不是模块级常量。

### 4.2 文件系统读取

| 操作 | 位置 | 逐字语义 |
| --- | --- | --- |
| `readdirSync(dir, { withFileTypes: true })` | `getFileSuggestions` | 同步、非递归；异常（不存在/无权限/非目录）→ 整段 `catch` → 返回 `[]` |
| `statSync(path).isDirectory()` | `getFileSuggestions`（符号链接）与 `resolveScopedFuzzyQuery`（baseDir 校验） | 异常 → 视为非目录 / 返回 `null` |
| `readdirSync` 的条目顺序 | 同上 | **依赖操作系统的目录顺序**（未排序时）；但结果最终按"目录优先 + `localeCompare`"排序，因此顺序不影响输出（相同 label 的稳定性除外） |

### 4.3 fd 子进程契约（**逐字**）

可执行文件：构造参数 `fdPath`（`string | null`；`null` 时禁用模糊文件补全）。

argv（顺序 **必须**一致）：

```
<fdPath>
  --base-directory <baseDir>
  --max-results <maxResults>
  --type f
  --type d
  --follow
  --hidden
  --exclude .git
  --exclude .git/*
  --exclude .git/**
  [--max-depth <maxDepth>]        // 仅当 maxDepth !== undefined（基础目录调用传 1）
  [--full-path]                   // 仅当 toDisplayPath(query).includes("/")
  [<buildFdPathQuery(query)>]     // 仅当 query 非空
```

`maxResults`：基础目录调用 `100`，递归调用 `100`（**两者相同**）；最终结果截断到 `20`。

stdio：`["ignore", "pipe", "pipe"]`（stdin 关闭；stdout/stderr 管道，stderr **不被读取**——若 fd 输出大量 stderr 且不消费，理论上可能阻塞，但 fd 正常时不写 stderr）。

退出码处理：

| 情形 | 行为 |
| --- | --- |
| `signal.aborted`（spawn 前） | 立即 `resolve([])`，**不** spawn |
| `signal.aborted`（关闭后） | `resolve([])` |
| `child.error` | `resolve([])` |
| `close` 且 `code !== 0` | `resolve([])` |
| `close` 且 stdout 为空（`""` 或仅空白 → `trim()` 后空串） | `resolve([])` |
| 正常 | 按行解析 |

解析：

1. `stdout.trim().split("\n").filter(Boolean)`（**只**按 `\n` 切；`\r` 会保留在行尾 → Windows 下路径带 `\r`，属既有行为）。
2. 每行 `toDisplayPath(line)`（`\` → `/`）。
3. `hasTrailingSeparator = displayLine.endsWith("/")`（fd 对目录输出尾部 `/`）。
4. `normalizedPath = 去掉尾部 "/"`。
5. `.git` 过滤：`normalizedPath === ".git" || startsWith(".git/") || includes("/.git/")` → 跳过。
6. `{ path: displayLine, isDirectory: hasTrailingSeparator }`。

取消：`AbortSignal` 触发 → 若 `child.exitCode === null` → `child.kill("SIGKILL")`；随后 close 回调 `resolve([])`。

### 4.4 无其它外部格式

- 无 HTTP/SSE/RPC。
- 无磁盘写入。
- 无剪贴板/图片协议（属 tui-core）。
- 无持久化缓存（宽度缓存仅进程内）。

---

## 5. 跨单元接口（精确类型）

### 5.1 本单元向其它单元**提供**的符号

#### 5.1.1 从包根（`index.ts`）再导出的（6 个 utils 符号 + 全部 colors/fuzzy/latex/autocomplete/editor-component 符号 + `oklabToOkhslLightness`）

```ts
// utils.ts（仅这 6 个从根导出；其余为深路径导入）
export { getOsc8LinkAtColumn, sliceByColumn, stripTerminalSequences,
         truncateToWidth, visibleWidth, wrapTextWithAnsi };
// colors.ts（全部）
export { backgroundAnsi, type Color, type ColorMixSpace, colorToHex, colorToOkhsl,
         colorToOklch, colorToRgb, foregroundAnsi, type IndexedColor, indexedColor,
         mixColors, type OkhslChannels, type OklchChannels, type OklchColorValue,
         okhslColor, oklchColor, parseColor, type RgbColorValue, rgbColor, styleText,
         styleTextWithAnsi, type TerminalColorMode, type TextAttributes, type TextStyle };
// fuzzy.ts
export { type FuzzyMatch, fuzzyFilter, fuzzyMatch };
// latex.ts
export { type RenderLatexOptions, renderLatex };
// autocomplete.ts
export { type AutocompleteItem, type AutocompleteProvider, type AutocompleteSuggestions,
         CombinedAutocompleteProvider, type SlashCommand };
// oklab.ts（只导出一个）
export { oklabToOkhslLightness };
// editor-component.ts
export type { EditorComponent };   // 类型 re-export，无运行时值
```

#### 5.1.2 深路径导入（同包内其它单元使用，Rust 侧可为 `pub` 但不必从 crate 根 re-export）

| 符号 | 文件 | 消费方（本仓库内） |
| --- | --- | --- |
| `getGraphemeSegmenter` | utils | components/editor.ts、components/input.ts、alt-screen-search.ts |
| `getWordSegmenter` | utils | components/editor.ts、tui-alt-screen.ts、word-navigation.ts（本单元内部） |
| `cjkBreakRegex` | utils | components/editor.ts（唯一外部消费方） |
| `cjkPunctuationRegex` | utils | **无外部消费方**（只被 `autocompleteSeparatorRegex` 在本文件内引用；仍是 `export`，Rust 侧可 `pub` 但无跨单元义务） |
| `autocompleteSeparatorRegex`, `autocompleteBoundaryRegex` | utils | autocomplete.ts（本单元内部）、components/editor.ts（构造 `autocompleteTokenStartSource`） |
| `getGraphemeCellRange` | utils | layout.ts（光标/覆盖层）、tui-alt-screen.ts（鼠标选区吸附字素边界） |
| `normalizeTerminalOutput` | utils | tui.ts（唯一外部消费方；**不是** tui-main-screen.ts） |
| `extractAnsiCode` | utils | layout.ts、tui-alt-screen.ts（选区/搜索高亮） |
| `getActiveBackgroundAnsi` | utils | layout.ts（唯一外部消费方；**不是** tui-alt-screen.ts） |
| `flattenLines` | utils | components/text.ts、components/box.ts、components/markdown.ts |
| `applyBackgroundToLine` | utils | components/text.ts、components/markdown.ts |
| `sliceWithWidth`, `extractSegments` | utils | tui.ts（唯一消费方；**不是** tui-alt-screen.ts） |
| `getOsc8LinkAtColumn` | utils | tui-alt-screen.ts（同时从 index.ts 根导出） |
| `PUNCTUATION_REGEX` | utils | word-navigation.ts（唯一消费方） |
| `isWhitespaceChar` | utils | word-navigation.ts、components/editor.ts、components/input.ts |
| `isPunctuationChar` | utils | **无外部消费方**（仅 `export` 供外部/测试；Rust 侧 `pub` 即可） |
| `visibleWidth` | utils | 见 §1.2（**15** 个消费方行；每行可能含多个符号）；本表不重复 |
| `findWordBackward`, `findWordForward`, `type WordNavigationOptions` | word-navigation | editor.ts、input.ts |
| `KillRing` | kill-ring | editor.ts、input.ts |
| `UndoStack<S>` | undo-stack | editor.ts、input.ts |
| `type EditorComponent` | editor-component | **`index.ts` 根导出**（`export type { EditorComponent }`）；`editor.ts`/`input.ts` **不**导入它（它们各自 define/实现自己的接口）；实际消费方是 coding-agent（`session-share.ts` 的 `ActiveEditor`、`ctx.ui.setEditorComponent` 扩展 API） |
| `AltScreenSearchIndex`, `AltScreenSearchComponent`, `type AltScreenSearchMatch`, `getAltScreenSearchMatchKey` | alt-screen-search | tui-alt-screen.ts 直接导入这 4 个；`findAltScreenSearchMatches`/`type AltScreenSearchSegment`/`type AltScreenSearchResult` 只有测试与外部消费者使用 |
| `oklabToLinearSrgb`, `rgbToOklab`, `linearSrgbToRgb`, `okhslToRgb`, `rgbToOkhsl` | oklab | colors.ts（本单元内部） |

> `GraphemeCellRange`（`{start:number; end:number}`）在 `utils.ts` 中是**模块私有 interface**，不是导出类型；`getGraphemeCellRange` 的返回类型在 `.d.ts` 中会以匿名结构出现。Rust 侧可自由命名为 `GraphemeCellRange`（§5.1.3 已如此），但不要指望 TS 侧能 `import type`。

#### 5.1.3 精确签名（Rust 侧 trait/结构对应）

```rust
// width.rs
pub fn visible_width(s: &str) -> usize;
pub fn strip_terminal_sequences(s: &str) -> String;
pub fn normalize_terminal_output(s: &str) -> String;

// ansi.rs
pub struct AnsiCode { pub code: String, pub length: usize }
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<AnsiCode>;
pub fn ansi_code_length(s: &str, pos: usize) -> usize;          // 私有在 TS，Rust 可 pub(crate)
pub fn get_active_background_ansi(text: &str) -> String;

// grapheme.rs
pub struct GraphemeCellRange { pub start: usize, pub end: usize }
pub fn get_grapheme_cell_range(line: &str, column: usize) -> Option<GraphemeCellRange>;
pub fn get_osc8_link_at_column(line: &str, column: usize) -> Option<String>;

// wrap.rs
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String>;
pub fn apply_background_to_line(line: &str, width: usize, bg: &dyn Fn(&str) -> String) -> String;

// truncate.rs
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String;
pub fn truncate_to_width_default(text: &str, max_width: usize) -> String;   // ellipsis="...", pad=false

// slice.rs
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String;
pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> SliceResult;
pub fn extract_segments(line: &str, before_end: usize, after_start: usize,
                        after_len: usize, strict_after: bool) -> Segments;

// fuzzy.rs
pub struct FuzzyMatch { pub matches: bool, pub score: f64 }   // 分数越低越好；不匹配时 score = 0
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch;   // 不返回 Option：用 .matches 判定
pub fn fuzzy_filter<T>(items: Vec<T>, query: &str, get_text: impl Fn(&T) -> &str) -> Vec<T>;
// 注意：fuzzy_filter 的 query 先 trim；空（或全空白）→ 原样返回 items（**同一顺序，不过滤不排序**）；
// 否则按 /[\s/]+/ 切 token，所有 token 都必须 fuzzy_match，分数求和，按总分升序稳定排序。

// word_nav.rs
pub struct WordSegment { pub segment: String, pub index: usize, pub is_word_like: bool }
pub struct WordNavigationOptions<'a> {
    // 对应 Intl.Segmenter.segment(input)：返回按 index 升序的段列表
    pub segment: Option<&'a dyn Fn(&str) -> Vec<WordSegment>>,
    pub is_atomic_segment: Option<&'a dyn Fn(&str) -> bool>,
}
pub fn find_word_backward(text: &str, cursor: usize, opts: Option<&WordNavigationOptions>) -> usize;
pub fn find_word_forward(text: &str, cursor: usize, opts: Option<&WordNavigationOptions>) -> usize;

// kill_ring.rs
pub struct PushOptions { pub prepend: bool, pub accumulate: bool }
pub struct KillRing { ring: Vec<String> }
impl KillRing {
  pub fn new() -> Self;
  pub fn push(&mut self, text: &str, options: PushOptions);   // 空串直接返回；accumulate 且非空 → 弹出栈顶并 push（prepend ? text+last : last+text）
  pub fn peek(&self) -> Option<&str>;                          // 栈顶，不修改
  pub fn rotate(&mut self);                                    // len > 1 时把栈顶移到栈底（unshift）
  pub fn len(&self) -> usize;
  pub fn is_empty(&self) -> bool;
}
// 注：yank / yank-pop 的"粘贴哪一个 + 循环替换"逻辑在**消费者**（editor/input，属 tui-components），
// 不在 KillRing 内；本单元只提供上述 4 个操作。

// undo_stack.rs
pub struct UndoStack<S> { stack: Vec<S> }
impl<S: Clone> UndoStack<S> {
  pub fn new() -> Self;
  pub fn push(&mut self, state: &S);     // 等价于 structuredClone：push 时深拷贝（S: Clone）
  pub fn pop(&mut self) -> Option<S>;    // 直接返回已脱离的快照，不再拷贝
  pub fn clear(&mut self);
  pub fn len(&self) -> usize;
  pub fn is_empty(&self) -> bool;
}
// 注：`undo`/`redo` 的语义由消费者（editor/input）用 push/pop 组合实现；UndoStack 只有栈语义。

// colors.rs
pub struct IndexedColor { pub index: u8 }              // 构造时已校验 0..=255 整数
pub struct RgbColorValue { pub r: f64, pub g: f64, pub b: f64 }   // f64 而非 u8：JS 侧是 number
pub struct OklchColorValue { pub l: f64, pub c: f64, pub h: f64 }
pub enum Color { Indexed(IndexedColor), Rgb(RgbColorValue), Oklch(OklchColorValue) }
pub struct TextAttributes { pub bold: bool, pub dim: bool, pub italic: bool,
                            pub underline: bool, pub inverse: bool, pub strikethrough: bool }
pub struct TextStyle { pub attrs: TextAttributes, pub fg: Option<Color>, pub bg: Option<Color> }
// 注意：TS 的 `TextStyle extends TextAttributes` 是**扁平**结构（bold/fg 同级）；
// Rust 用嵌套 attrs 时，`style_text` 内部拼接顺序必须仍是 fg,bg,bold,dim,italic,underline,inverse,strikethrough。
pub fn indexed_color(index: f64) -> Result<IndexedColor, ColorError>;   // JS 收 number；-0 合法
pub fn rgb_color(r: f64, g: f64, b: f64) -> Result<RgbColorValue, ColorError>;
pub fn oklch_color(l: f64, c: f64, h: f64) -> Result<OklchColorValue, ColorError>;
pub fn okhsl_color(h: f64, s: f64, l: f64) -> Result<RgbColorValue, ColorError>;
// ColorError 必须能渲染出 §3.1 的逐字消息（含 JS Number::toString 格式化）。
pub enum ColorInput<'a> { Number(f64), Str(&'a str) }   // 对应 string | number 重载
pub fn parse_color(value: ColorInput<'_>) -> Result<Color, ColorError>;
pub fn color_to_rgb(c: &Color) -> RgbColor;
pub fn color_to_oklch(c: &Color) -> OklchChannels;
pub fn color_to_okhsl(c: &Color) -> OkhslChannels;
pub fn color_to_hex(c: &Color) -> String;
pub fn mix_colors(a: &Color, b: &Color, amount: f64, space: ColorMixSpace) -> Color;
pub fn foreground_ansi(c: &Color, mode: TerminalColorMode) -> String;
pub fn background_ansi(c: &Color, mode: TerminalColorMode) -> String;
pub fn style_text(text: &str, style: &TextStyle, mode: TerminalColorMode) -> String;
pub fn style_text_with_ansi(text: &str, fg_ansi: Option<&str>, bg_ansi: Option<&str>,
                            attrs: &TextAttributes) -> String;
// 注意：不是 (text, open, close)；fg/bg 是**已格式化的 ANSI 片段**，attrs 只含六个布尔属性。
// 输出 = prefix(fg, bg, bold, dim, italic, underline, inverse, strikethrough) + text + suffix(逆序)。

// latex.rs
pub struct RenderLatexOptions { pub display: bool }   // 仅此一个字段；`display == true` 才启用二维布局
pub fn render_latex(source: &str, options: &RenderLatexOptions) -> Option<String>;

// autocomplete.rs
pub struct AutocompleteItem { pub value: String, pub label: String, pub description: Option<String> }
pub struct AutocompleteSuggestions { pub items: Vec<AutocompleteItem>, pub prefix: String }
pub struct ApplyResult { pub lines: Vec<String>, pub cursor_line: usize, pub cursor_col: usize }
pub struct SuggestOptions { pub force: bool, pub cancel: CancellationToken }
// 命令项既可能是 SlashCommand 也可能是 AutocompleteItem（TS 的 `"name" in cmd` 判别）：
pub enum CommandEntry {
    Slash(SlashCommand),
    Item(AutocompleteItem),
}
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub get_argument_completions: Option<ArgumentCompletionsFn>,   // 返回 Vec 或 None（对应 null）
}
// 注意：`cursor_col`/`cursor_line` 是 **UTF-16 单位**的列；`apply_completion` 的返回值同样是。
#[async_trait]
pub trait AutocompleteProvider: Send + Sync {
    fn trigger_characters(&self) -> Option<Vec<char>> { None }
    async fn get_suggestions(&self, lines: &[String], cursor_line: usize, cursor_col: usize,
                             options: &SuggestOptions) -> Option<AutocompleteSuggestions>;
    fn apply_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize,
                        item: &AutocompleteItem, prefix: &str) -> ApplyResult;
    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> Option<bool> { None }
}
pub struct CombinedAutocompleteProvider { /* commands, base_path, fd_path */ }

// search.rs
pub struct AltScreenSearchSegment { pub row: usize, pub start_col: usize, pub end_col: usize }
pub struct AltScreenSearchMatch { pub segments: Vec<AltScreenSearchSegment> }
pub struct AltScreenSearchResult { pub matches: Vec<AltScreenSearchMatch>, pub changed: bool }
pub struct AltScreenSearchIndex { /* 缓存 */ }
pub fn find_alt_screen_search_matches(lines: &[String], query: &str) -> Vec<AltScreenSearchMatch>;
pub fn get_alt_screen_search_match_key(m: &AltScreenSearchMatch) -> String;
pub struct AltScreenSearchComponent { /* … */ }
```

### 5.2 本单元**消费**的其它单元符号

| 符号 | 来源单元 | 形态 | 用途 |
| --- | --- | --- | --- |
| `Component` | tui-core（`tui.ts`） | `interface { render(width): string[]; invalidate?(); handleInput?(data); … }` | `AltScreenSearchComponent implements Component` |
| `Focusable` | tui-core | `interface { focused: boolean }` | `AltScreenSearchComponent implements Focusable` |
| `Input` | tui-components（`components/input.ts`） | `class Input { constructor(options?: {prompt?, placeholder?, placeholderStyle?}); getValue(): string; handleInput(data: string): void; render(width: number): string[]; focused: boolean }` | 搜索框 |
| `getKeybindings()` | tui-core（`keybindings.ts`） | `() => KeybindingsManager`；`manager.getKeys(action: Keybinding): KeyId[]` | 搜索面板渲染键位 |
| `RgbColor` | tui-core（`terminal-colors.ts`） | `{ r: number; g: number; b: number }` | `colors.ts`/`oklab.ts` 的输出类型 |
| `CURSOR_MARKER` / `FAKE_CURSOR_START` / `FAKE_CURSOR_END` | tui-core | 常量字符串 | 仅间接（`Input`） |

**Rust 侧的依赖方向**：`kiana-tui-text` crate **只**依赖这些 trait/类型的**只读视图**。注意如果 `AltScreenSearchComponent` 留在 `kiana-tui-text` 内、同时 `kiana-tui-text` 又依赖 `kiana-tui-core` 的 `Component`，则 `Input` 必须通过 trait 注入以避免 `kiana-tui-text → kiana-tui-components` 的循环依赖（见本节末尾）。建议：

```rust
// kiana-tui-core 提供
pub trait Component { fn render(&mut self, width: usize) -> Vec<String>; }
pub trait Focusable { fn focused(&self) -> bool; fn set_focused(&mut self, v: bool); }
pub trait KeybindingLookup { fn get_keys(&self, action: &str) -> Vec<String>; }
pub struct RgbColor { pub r: f64, pub g: f64, pub b: f64 }
```

`kiana-tui-text` 依赖 `kiana-tui-core`（只为上述类型），`kiana-tui-components` 依赖 `kiana-tui-text`。**不得**让 `kiana-tui-text` 依赖 `kiana-tui-components`（否则 `AltScreenSearchComponent` 需要 `Input`，形成环）——解决方案：把 `Input` 抽象为 `TextInput` trait（`get_value/handle_input/render/set_focused`），`AltScreenSearchComponent` 持 `Box<dyn TextInput>`，由 `kiana-tui-components` 注入。

### 5.3 与 tui-alt-screen 的搜索集成契约（消费者必须实现的部分）

```rust
// tui-alt-screen 侧（消费者），本单元只提供组件与索引
pub struct ActiveSearch {
    pub component: AltScreenSearchComponent,
    pub index: AltScreenSearchIndex,
    pub overlay: Option<OverlayHandle>,
    pub query: String,
    pub matches: Vec<AltScreenSearchMatch>,
    pub selected_index: Option<usize>,
    pub selected_key: Option<String>,
    pub anchor_row: usize,
    pub selection_mode: SearchSelectionMode,   // TS: type SearchSelectionMode = "query" | "retain" | "next" | "previous"（模块私有，不导出）
}
```

- `toggleSearch()` → `showOverlay(component, {anchor:"top-right", width:"40%", minWidth:32, margin:1})`。
- `updateSearchQuery(q)`：`q === search.query` 时**直接返回**；否则记录 `anchorRow = matches[selectedIndex]?.segments[0]?.row ?? scrollTop`，重置 `setResult(-1, 0)`。
- `parseSgrMouseEvent` 命中：`getNavigationDirectionAt(y - bounds.row, x - bounds.col)`。
- `refreshSearch(layout)`：见下方伪代码（完整复刻 `tui-alt-screen.ts::refreshSearch`，第 **607–674** 行）。

**`refreshSearch(layout)` 伪代码（逐字，审计补充；原 §2.11.7 引用不存在，在此补齐）**：

```
search = activeSearch; if (!search) return false;
scrollView = layout.primaryScrollView ?? implicitScrollView;
box = getScrollViewBox(layout, scrollView);
lines = box?.scrollContentLines;
if (!lines || !search.query.trim()) {
  search.matches = []; search.selectedIndex = -1; search.selectedKey = undefined;
  search.selectionMode = "retain"; search.component.setResult(-1, 0); return false;
}
shouldRevealSelection = search.selectionMode !== "retain";
result = search.index.search(lines, search.query);       // {matches, changed}
matches = result.matches; search.matches = matches;
if (!result.changed && search.selectionMode === "retain") return false;

exactIndex = result.changed
  ? (search.selectedKey ? matches.findIndex(m => getAltScreenSearchMatchKey(m) === search.selectedKey) : -1)
  : search.selectedIndex;
selectedIndex = -1;
if (matches.length > 0) {
  if (search.selectionMode === "query") {
    // 二分：第一个 segments[0].row >= anchorRow 的匹配；没有则回绕到 0
    low = 0; high = matches.length;
    while (low < high) { middle = low + floor((high-low)/2);
      if ((matches[middle].segments[0]?.row ?? 0) < search.anchorRow) low = middle+1; else high = middle; }
    selectedIndex = low < matches.length ? low : 0;
  } else if (search.selectionMode === "next") {
    baseIndex = exactIndex >= 0 ? exactIndex : min(search.selectedIndex, matches.length-1);
    selectedIndex = baseIndex < 0 ? 0 : (baseIndex + 1) % matches.length;
  } else if (search.selectionMode === "previous") {
    baseIndex = exactIndex >= 0 ? exactIndex : min(search.selectedIndex, matches.length-1);
    selectedIndex = baseIndex < 0 ? matches.length-1 : (baseIndex - 1 + matches.length) % matches.length;
  } else {  // "retain"
    selectedIndex = exactIndex >= 0 ? exactIndex : min(max(0, search.selectedIndex), matches.length-1);
  }
}
search.selectedIndex = selectedIndex;
search.selectedKey = selectedIndex >= 0 ? getAltScreenSearchMatchKey(matches[selectedIndex]) : undefined;
search.selectionMode = "retain";
search.component.setResult(selectedIndex, matches.length);
if (!shouldRevealSelection) return false;

selected = matches[selectedIndex];
firstSegment = selected?.segments[0]; lastSegment = selected?.segments[last];
if (!box || !firstSegment || !lastSegment || scrollView.viewportHeight <= 0) return false;
before = scrollView.scrollTop;
visibleBottom = before + scrollView.viewportHeight - 1;
target = before;
if (firstSegment.row < before || lastSegment.row > visibleBottom)
  target = firstSegment.row - floor(scrollView.viewportHeight / 3);   // 把命中行放在视口上 1/3 处
scrollView.scrollTo(target, { disableFollow: true });
return scrollView.scrollTop !== before;                                // 是否真的滚动过
```

要点：`anchorRow` 由 `updateSearchQuery` 在**查询变化前**用"当前选中项的首行或 `scrollTop`"记录；`selectionMode` 是 `query|retain|next|previous` **四**态循环（详见 §5.3 的 `ActiveSearch`）；返回值只表示"是否滚动了视口"，用于决定是否重绘。
- `applySearchTextHighlight(text, current)`（`tui-alt-screen.ts` 私有，第 1500–1518 行；`applySearchHighlights` 在 1520–1582 行）：逐 ANSI 码扫描，把**非 ANSI 的普通文本段**用 `searchCurrentMatchStyle`/`searchMatchStyle` 包裹，ANSI 码原样透传（保证既有颜色不被破坏、且样式只作用于可见文本）。逐字算法：

```
style = current ? searchCurrentMatchStyle : searchMatchStyle;
result = ""; plainStart = 0; index = 0;
while (index < text.length) {
  ansi = extractAnsiCode(text, index);
  if (!ansi) { index += 1; continue; }
  if (index > plainStart) result += style(text.slice(plainStart, index));   // 仅当普通段非空
  result += ansi.code; index += ansi.length; plainStart = index;
}
if (plainStart < text.length) result += style(text.slice(plainStart));       // 尾部普通段
return result;
```

  要点：连续 ANSI 码之间不会插入空样式包裹（`index > plainStart` 守卫）；**首个 ANSI 之前**的普通段也被包裹；样式按"每个普通 run 一次"施加，因此跨越 ANSI 的文本会被拆成多段各自重开/重关。
- `applySearchHighlights(screen, layout)`：对每个可见行、每个命中区间（**按 `startCol` 降序**，见 §A.12 第 5 条）用 `sliceByColumn(line, 0, startCol, true)` / `sliceByColumn(line, startCol, endCol-startCol, true)` / `sliceByColumn(line, endCol, max(0, lineWidth-endCol), true)` 三段拼接，中间段再经 `applySearchTextHighlight` 逐 ANSI 片段包样式（保留行内既有 SGR，按段重开）。行/列裁剪：`minRow = max(0, rect.y, clip.y)`、`maxRow = min(screen.length, rect.y+rect.height, clip.y+clip.height)`、`minColumn = max(0, rect.x, clip.x)`、`maxColumn = min(terminal.columns, rect.x+rect.width, clip.x+clip.width, scrollbarColumn ?? +∞)`。

**坐标系换算（易错，审计补充）**：`AltScreenSearchMatch.segments` 的 `row` 是**内容坐标**（相对 `ScrollView` 内容首行）、`startCol/endCol` 是**内容列**。渲染到 `screen` 时必须做两次换算：

```
row        = box.rect.y + segment.row - scrollView.scrollTop;   // 内容行 → 屏幕行
startCol   = max(minColumn, box.rect.x + segment.startCol);     // 内容列 → 屏幕列（加 rect.x）
endCol     = min(maxColumn, box.rect.x + segment.endCol);
if (row < minRow || row >= maxRow) continue;
if (endCol <= startCol) continue;                                // 裁剪后为空则丢弃
```

二进制搜索的键也是内容行：`minContentRow = scrollView.scrollTop + minRow - box.rect.y`、`maxContentRow = scrollView.scrollTop + maxRow - box.rect.y - 1`；搜索"第一个 `segments.last().row >= minContentRow` 的 match"，向下遍历直到 `segments[0].row > maxContentRow` 时 `break`。另外应用前还有一次列的二次夹取：`startCol = min(range.startCol, lineWidth)`、`endCol = min(range.endCol, lineWidth)`（`lineWidth = visibleWidth(line)`），裁剪后退化则跳过；`isImageLine(line)` 的行整行跳过。
- 依赖的 tui-core 符号：`getScrollViewBox(frame, scrollView) → LayoutBox | undefined`（`LayoutBox.scrollContentLines: string[]`、`.rect`、`.clip`）、`getScrollbarGeometry(box)?.column`、`Box.scrollTop/viewportHeight/scrollTo(target, {disableFollow:true})`。

---

## 6. npm 依赖与 Rust crate 对应

### 6.1 直接依赖（`packages/tui/package.json`，**逐字版本**）

```json
"dependencies": {
  "get-east-asian-width": "1.6.0",
  "marked": "18.0.11",
  "marked-cjk-friendly": "0.1.2"
},
"devDependencies": {
  "@xterm/headless": "5.5.0",
  "chalk": "6.0.0"
}
```

本单元**只**直接使用 `get-east-asian-width@1.6.0`（`marked` 与 `marked-cjk-friendly` 属 tui-components 的 markdown；`@xterm/headless`/`chalk` 仅测试）。

Node 内置模块：`child_process`（`spawn`）、`fs`（`readdirSync`、`statSync`）、`os`（`homedir`）、`path`（`basename`、`dirname`、`join`）。

平台 API：`Intl.Segmenter`（grapheme + word）、`RegExp`（`/u`、`/v`、`/i`、`/g`、lookbehind）、`String.prototype.localeCompare`、`String.prototype.toLowerCase`、`structuredClone`、`Object.freeze`、`process.platform`、`AbortSignal`、`Math.*`、`Number.parseInt/parseFloat/isFinite/isInteger`。

### 6.2 npm / 平台 API → Rust 映射

| npm / 平台 API | 用途（本单元） | Rust crate / 方案 | 差异与桥接 |
|---|---|---|---|
| `get-east-asian-width` 1.6.0 | `graphemeWidth` 的 EAW 判定（`eastAsianWidth(cp)` 返回 0/1/2） | **内嵌表**（推荐）或 `unicode-width` 0.2 | `get-east-asian-width@1.6.0` 的表是 **Unicode 17.0**，`unicode-width 0.2` 是 Unicode 15.1/16.0；新增字符与"未分配码点返回 1"的默认值可能不同。**必须**按 `/tmp/geaw_pkg` 的 `lookup-data.js` 逐区间移植（见 §2.1.2 的算法），并用 `test/visible-width.test.ts`、`test/regression-regional-indicator-width.test.ts`、`test/tab-width.test.ts`、`truncate-to-width.test.ts` 的 `visibleWidth` 段校准 |
| `Intl.Segmenter({granularity:"grapheme"})` | 字素簇切分 | `unicode-segmentation` 1.x（UAX #29）**或** `icu_segmenter`（ICU4X） | 两者在 RGI emoji ZWJ、国旗（RI 对）、Indic 连字、`\r\n` 上通常一致；差异集中在 Unicode 版本新增字符。**推荐 `icu_segmenter`** 以获得与 ICU 同源的行为；若接受轻微差异用 `unicode-segmentation`（更快、无数据包） |
| `Intl.Segmenter({granularity:"word"})` | `findWordBackward/Forward` 的 `segment()` | `icu_segmenter::WordSegmenter` | **必须**同时提供 `is_word_like`（UAX #29 的 `Word_Break` 属性推导），见 §2.3 |
| `String.prototype.localeCompare` | fd 结果排序、目录列表排序 | `icu_collation` / `icu4x::Collator`（**运行时 locale**、默认选项） | `localeCompare(other)` 无参 = 默认 locale（由 `LC_ALL`/`LC_MESSAGES`/`LANG` 解析，不是 DUCET root；ICU 78 在 `LANG` 为空时回落到 `en-US`）、`sensitivity: "variant"`、`numeric: false`、`caseFirst: "false"`。Rust 侧必须自行解析环境 locale 并传给 `Collator::try_new`。**不要**写死 `locale!("und")`（ICU4X 的 `und` 与 Node 的解析结果不同），也**不要**用 `str::cmp` 或 `unicase` |
| `String.prototype.toLowerCase` | `scoreEntry`、`getFileSuggestions` 前缀过滤、`formatKey` 的 `alt` 判定、`fuzzyMatch` | `str::to_lowercase()` | JS `toLowerCase` 用 Unicode 默认（非 locale 特化）小写映射：`İ` U+0130 → `i` + U+0307；`ß` 不变；`Σ` 的词尾形式是**上下文相关**的（`"ΑΣ".toLowerCase()` = `"ας"`，第二个字符是 U+03C2 而非 U+03C3）。Rust 的 `str::to_lowercase()` 同样是上下文相关 + 非 locale 特化，可直接使用；但**不要**用 `char::to_lowercase` 逐字拼接（会丢失词尾 ς 规则） |
| `RegExp` with `/u`、`/v`、`/i`、`/g`、lookbehind | 见 §6.3 | `regex` 1.x（默认 `unicode` feature） | `regex` **不支持** lookbehind、`/v` 集合运算、`\p{Script_Extensions=…}`（只支持 `\p{Script=…}` 与 `\p{scx=…}`？——`regex` 支持 `\p{scx=Han}`）。需要逐条改写（见 §6.3） |
| `AbortSignal` / `AbortController` | fd 取消 | `tokio_util::sync::CancellationToken` 或 `futures::AbortHandle` | `signal.aborted` 的**同步**检查必须在 spawn 前；`addEventListener("abort", …, {once:true})` → `token.cancelled()` + `select!` |
| `child_process.spawn` | fd 子进程 | `tokio::process::Command`（推荐）或 `std::process::Command` + 线程 | 需要 `kill(SIGKILL)`；`exitCode === null` 判定 → `Child::try_wait()` 或 `id()`；stdout 按 **UTF-8 流式解码**（Node `setEncoding("utf-8")` 会在多字节边界缓冲，等价于 `String::from_utf8_lossy` 的流式版本——Rust 用 `tokio::io::AsyncReadExt` + 逐块 push 到 `Vec<u8>` 最后 `from_utf8_lossy` 即可，因为 JS 的 `stdout += chunk` 会保留替换字符行为） |
| `fs.readdirSync(dir, {withFileTypes:true})` | 目录列举 | `std::fs::read_dir` | `Dirent.isDirectory()` 依赖 `d_type`（不跟随符号链接）；Rust `DirEntry::file_type()` 同样不跟随。**不要**用 `metadata()`（会跟随符号链接，改变行为） |
| `fs.statSync(p).isDirectory()` | 符号链接判定 | `std::fs::metadata` | 跟随符号链接（与 `statSync` 一致） |
| `os.homedir()` | `~` 展开 | `dirs::home_dir()` | Linux：优先 `$HOME`；macOS：`$HOME` 或 `getpwuid`；Windows：`USERPROFILE`。Node 的行为与 `dirs` 基本一致 |
| `path.join` / `path.dirname` / `path.basename` | 路径拼接 | `std::path::{Path, PathBuf}` | **差异**：Node 的 `join` 接受多段字符串、自动规范化 `.`/`..`、保留尾部 `/`（`join("/a", "")` = `"/a"`）；`path.basename("a/")` = `"a"`；Windows 上分隔符为 `\`。本单元在 `toDisplayPath` 后统一为 `/`，但 `join` 发生在 `toDisplayPath` **之前**（`displayPrefix` 已是 `/` 形式时 `join` 仍用平台分隔符）。Rust 建议实现 `node_path_join`（按平台分隔符拼接并规范化）以保证 `basePath` 为 POSIX 路径时的输出一致 |
| `structuredClone` | `UndoStack.push` 快照 | `Clone`（深拷贝） | JS 的 `structuredClone` 对 `EditorSnapshot`（字符串 + 数字 + 数组）等价于深拷贝 |
| `Object.freeze` | 颜色对象不可变 | Rust 值语义天然不可变 | 无运行时差异 |
| `Math.round` | `colorToHex`、`rgbToAnsi256` | 手写 `js_round` | JS `Math.round(x)` = `floor(x + 0.5)`（`-0.5` → `-0`；`Math.round(2.5)=3`、`Math.round(-2.5)=-2`）。Rust `f64::round` 是"远离零"舍入（`-2.5 → -3`，**不同**） |
| `Math.cbrt` | `oklab` 的 `cbrt` | `f64::cbrt` | IEEE 754 推荐正确舍入；Rust 与 V8 都用系统 libm，个别位可能不同。为**位级**一致，建议内嵌 V8 的 fdlibm 实现（`v8/src/base/ieee754.cc`） |
| `Math.hypot` | `colorToOklch` | 手写 `sqrt(a*a+b*b)` | `Math.hypot` 有溢出保护且精度可能略高（V8 用 Kahan 求和）；实测本单元输入范围内差异 < 1 ulp，可忽略但需记录 |
| `Math.atan2` / `Math.cos` / `Math.sin` | OKLCH/OKHSL | `f64::atan2` / `cos` / `sin` | 同 libm 差异；`oklab.ts` 的 round-trip 测试（colors.test.ts）用 `strictEqual` 断言浮点**完全相等**，必须逐位一致，否则测试失败 |
| `Math.min(...[])` | `renderLayout` 的空行高度 | `iter().min().unwrap_or(f64::INFINITY)` | JS `Math.min()` = `Infinity`（**不是** 0 或 NaN）；`Math.min(...[]) === Infinity`。Rust 必须显式返回 `f64::INFINITY` |
| `Number.parseInt(s, 16)` / `Number.parseFloat` | `parseColor` | `u8::from_str_radix` / `f64::from_str` | `parseFloat` 接受前缀数字（`"1.5abc"` → 1.5）；本单元正则已保证格式，可安全用 `f64::from_str` |
| `Number.isFinite` / `Number.isInteger` | 颜色校验 | `f64::is_finite` / `f64::fract()==0.0 && is_finite` | `Number.isInteger(-0)` = true；`-0` 作为 index 时 `index < 0` 为 false → 合法，输出 `-0` 会进 `String(index)` → `"0"` |
| `process.platform === "darwin"` | `formatKey` | `cfg!(target_os = "macos")` | 编译期判定即可 |
| `performance.now()` | 无（本单元不用） | — | — |

### 6.3 正则迁移清单（本单元全部正则，逐字）

| 文件 | 正则（逐字） | Rust 方案 |
|---|---|---|
| utils | `/^(?:\p{Default_Ignorable_Code_Point}\|\p{Control}\|\p{Mark}\|\p{Surrogate})+$/v` | `regex` 支持 `\p{Default_Ignorable_Code_Point}`? **不支持该属性名**（regex 的 Unicode 属性集较窄）。改为**手写判定函数** `is_zero_width(s)`：逐 char 检查 `is_default_ignorable(cp) \|\| is_control(cp) \|\| is_mark(cp) \|\| is_surrogate(cp)`（后者在 Rust 字符串中不存在）。`Mark` 可用 `unicode-general-category` crate 或自建表 |
| utils | `/^[\p{Default_Ignorable_Code_Point}\p{Control}\p{Format}\p{Mark}\p{Surrogate}]+/v` | 同上，手写"跳过前缀"循环 |
| utils | `/^(?:\p{Default_Ignorable_Code_Point}\|\p{Control}\|\p{Format}\|\p{Mark}\|\p{Surrogate})$/v` | 手写单字符判定 |
| utils | `/^\p{Mark}$/v` | `unicode-general-category::get_general_category(c) == Mark` |
| utils | `/^(?:[\p{Spacing_Mark}--[᜴〮〯]]\|[ٟཿါာေဳ-ဵး်-ှ])+$/v` | **手写**：`Spacing_Mark` 用 `unicode-general-category`（`Mc`），排除三个码点，再并上显式列表。注意 `+` 与 `^…$` 表示"整个簇全部由此类字符组成" |
| utils | `/^\p{RGI_Emoji}$/v` | **无法**用 `regex` 表达（RGI_Emoji 是 Unicode 的 emoji 序列集合，含 ZWJ、国旗、标签序列）。方案：内嵌 `unicode-emoji` 数据生成的状态机，或用 `emojis` crate 的 `is_emoji`（近似）。**必须**与 JS 一致：见 §2.1.2 的 `couldBeEmoji` 快路径 + 完整判定 |
| utils | `cjkBreakRegex`（`\p{Script_Extensions=Han/Hiragana/Katakana/Hangul/Bopomofo}`） | `regex` 支持 `\p{scx=Han}` 等（`Script_Extensions` 可用 `scx` 别名）。逐字改写为 `[\p{scx=Han}\p{scx=Hiragana}\p{scx=Katakana}\p{scx=Hangul}\p{scx=Bopomofo}]` |
| utils | `cjkPunctuationRegex` = `(?:(?=\p{Punctuation})${cjkBreakRegex}\|[，．：；！？（）［］｛｝“”‘’…—])` | **含 lookahead**，`regex` 不支持。**语义更正（审计核实）**：前瞻与 `cjkBreakRegex` 作用在**同一位置**——即“该字符既是 `\p{P}` **又是** CJK 脚本”（不是“后跟 CJK 字母”）。手写函数 `is_cjk_punctuation(ch)`：`(is_punctuation(ch) && scx_any(ch, HAN|HIRA|KANA|HANGUL|BOPOMOFO)) \|\| ch ∈ {，．：；！？（）［］｛｝“”‘’…—}`。实测：`。`/`、`/`「`/`」` → true（第一分支，它们的 scx 含 Hani/Hira/Kana/Bopo）；`，`/`．`/`：`/`；`/`！`/`？`/`（`/`）`/`［`/`］`/`｛`/`｝`/`“`/`”`/`‘`/`’`/`…`/`—` → true（显式列表，scx 不含 CJK）；`中`/`あ`/`한` → false（是 CJK 字母但不是标点）；`x` → false。**注意** `autocompleteSeparatorRegex` 与 `autocompleteBoundaryRegex` 因此也需改为函数 |
| utils | `autocompleteSeparatorRegex` = `(?:\s\|${cjkPunctuationRegex})` | 手写 `is_autocomplete_separator(c)`（注意 `\s` 的集合，见下） |
| utils | `autocompleteBoundaryRegex` = `(?:^\|${autocompleteSeparatorRegex})` | 手写"位置 0 或前一字符是分隔符" |
| utils | `/\s/`（`isWhitespaceChar`、`autocompleteSeparatorRegex`、`fuzzyFilter` 的 token 切分 `/[\s/]+/`、`normalizeQuery` 的 `/\s+/gu`、`parseWhitespace`、`parseScripts` 的空白跳过、`scorePositions` 的词边界） | **必须**实现 JS `\s` 集合（**25 个码点**，见 §2.1.17 的逐字表与 §A.2 的完整码点表）。Rust `char::is_whitespace`（`White_Space`）**不同**：多 U+0085、少 U+FEFF（其余 23 个一致；U+180E 两边都不算空白） |
| utils | `PUNCTUATION_REGEX` = `/[(){}[\]<>.,;:'"!?+\-=*/\\\|&%^$#@~`]/` | 字符类，直接用 |
| utils | `/\x1b.../` 系列（ANSI 扫描） | 已手写 `ansiCodeLength`，无正则 |
| utils | `THAI_LAO_AM_REGEX` = `/[ำຳ]/`、`/g` 版本 | 直接用 `char` 比较 |
| fuzzy | 无正则 | — |
| word-navigation | 无正则（用 `Intl.Segmenter` + `PUNCTUATION_REGEX`） | 见 §2.3 |
| latex | `NAMED_OPERATOR_START_SPACING_PATTERN` = `/(?<=[\p{L}\p{N}\]}\u{f0001}])\u{f0004}/gu`；`NAMED_OPERATOR_END_SPACING_PATTERN` = `/\u{f0005}(?=[\p{L}\p{N}√\u{f0000}])/gu` | **lookbehind**；手写单遍扫描（替换时保留控制字符语义） |
| latex | `ROOT_*`、`\p{L}`/`\p{N}` 分组 | `regex` 的 `\p{L}`/`\p{N}` 可用 |
| latex | `splitEnvironmentRows` = `/\\\\(?:\[[^\]\n]*\])?/g` | 直接用（Rust 原始字符串 `r"\\\\(?:\[[^\]\n]*\])?"`） |
| latex | `replaceCharacters` 的字符映射表 | 手写 map |
| colors | `NUMBER_PATTERN` = `[+-]?(?:\d+(?:\.\d*)?\|\.\d+)(?:e[+-]?\d+)?`；`OKLCH_PATTERN`、`OKHSL_PATTERN` | `regex` 直接支持（注意 `i` flag） |
| colors | `/^#([\da-f]{3}\|[\da-f]{6})$/i` | 直接用 |
| autocomplete | `tokenStartRegex`（= `autocompleteBoundaryRegex` + `$`） | 手写（见上） |
| autocomplete | `/[.*+?^${}()\|[\]\\]/g`（`escapeRegex`） | 手写字符替换 |
| autocomplete | `/^\/+\|\/+$/g`（`buildFdPathQuery`） | 手写 trim 前导/尾部 `/` |
| autocomplete | `/\\/g`（`toDisplayPath`） | `replace('\\', "/")` |
| autocomplete | `/^[\x20-\x7e]*$/`（`PRINTABLE_ASCII`，在 alt-screen-search） | 手写 `bytes().all(\|b\| (0x20..=0x7e).contains(&b))` |
| alt-screen-search | `/\s+/gu`（`normalizeQuery`） | 手写 JS `\s` 集合的连续压缩 |
| alt-screen-search | `/^\s+$/u`（字素是否空白） | 手写 |
| alt-screen-search | `new RegExp(escapeRegExp(q), "giu")` | `regex::RegexBuilder::new(&regex::escape(q)).case_insensitive(true).unicode(true).build()`；**注意** JS 的 `i` 是简单大小写折叠（`ß` ≠ `SS`），Rust `regex` 的 `(?i)` 默认也是简单折叠（一致）；但土耳其语 `İ` 等需逐例核对 |
| alt-screen-search | `/\s+/gu` 之外的 `escapeRegExp` | 手写 |

### 6.4 `localeCompare` 的精确语义（必须桥接）

`Array.prototype.sort((a,b) => a.label.localeCompare(b.label))` 使用 **运行时默认 locale 的 collation**（不是 DUCET root）+ **默认选项**。`new Intl.Collator(undefined)` / `localeCompare(other)` 的 locale 由 `Intl.DateTimeFormat().resolvedOptions().locale` 决定：`LANG`/`LC_ALL` 为空 → `und`（ICU 78 实测回落 `en-US`）；`LANG=C` → `en-US`；`LANG=zh_CN.UTF-8` → `zh-CN`。**实测差异**（同一输入）：

| locale | CJK 与拉丁的相对次序 | 关键样例（含 `(g).ts`/`-dash.txt`/`~tilde.txt`） |
| --- | --- | --- |
| `und`（空 LANG） | `符号 < 拉丁 < CJK` | `-dash.txt (g).ts [id].tsx ~tilde.txt a.txt … 文档/ 说明.md` |
| `en-US`（`LANG=C`） | `符号 < 拉丁 < CJK` | 同上 |
| `zh-CN`（`LANG=zh_CN.UTF-8`） | `符号 < CJK < 拉丁` | `-dash.txt (g).ts [id].tsx ~tilde.txt 说明.md 文档/ a.txt …` |
| `en-US-u-va-posix`（显式 DUCET） | `符号 < 拉丁 < CJK`，但符号/大小写三级权重不同 | `(g).ts -dash.txt README.md UPPER.TXT [id].tsx a.txt … z folder/ ~tilde.txt 文档/ 说明.md` |

需求：

1. 主权重（primary）比较基础字母（`a` = `A` = `á`）。
2. 次权重（secondary）比较重音（`a` < `á`）。
3. 三级权重（tertiary）比较大小写/变体；默认 `caseFirst: "false"` 表示**小写在前**（实测 `"a".localeCompare("A") === -1`）。
4. `"a b" < "ab"`：空格的可变权重（variable weighting，默认 `non-ignorable`？实测 `"a b".localeCompare("ab") === -1`，说明空格**参与**比较且小于字母）。
5. `"a.txt" < "ab"`：`"."` 同样参与且小于字母。
6. CJK 排序**随 locale 变**：`zh-CN` 下实测 `"说明.md" < "文档/" < "a.txt"`（`符号 < CJK < 拉丁`）；`en-US`/`und` 下实测 `"a.txt" < "文档/" < "说明.md"`（`符号 < 拉丁 < CJK`）。**不存在"与 locale 无关的 CJK 主权重"**——必须按 §A.4 的矩阵用 ICU 实测校准，不能凭直觉写死某一顺序。
7. 稳定性：`localeCompare` 返回 0 时 `sort` 保持原顺序（V8 的 `Array.sort` 是稳定排序，Rust `sort_by` 也稳定）。

**Rust 方案（按优先级）**：

- **方案 A（推荐）**：`icu_collation`（ICU4X）+ **显式 locale**（从进程环境解析：`LC_ALL`/`LC_MESSAGES`/`LANG` → BCP-47；解析失败/为空时用 `en-US` 以匹配 ICU 78 的 `und→en-US` 回落）。`CollatorOptions { case_first: Some(CaseFirst::Off), numeric: None, alternate_handling: Some(AlternateHandling::NonIgnorable), … }`。**不要**写死 `locale!("und")`：ICU4X 的 `und` 不等价于 Node 的 locale 解析结果。ICU4X 数据版本与 Node 26 内置的 ICU 78.3 须比对（用 `test/autocomplete.test.ts` 的目录排序用例锁定，但注意这些用例大多先 `.sort()`，不锁定顺序）。
- **方案 B**：`icu_collator`（compiled_data）+ 从环境解析出的 locale（同方案 A）。
- **方案 C（应急）**：手写比较器：先按 locale 决定 CJK 块在拉丁之前还是之后（`zh-CN` 等 CJK locale 在前，其余在后），块内按码点。**只**在测试用到的字符集上正确；风险高。

**注意**：`getFileSuggestions` 的排序是"目录优先，再 `localeCompare(label)`"；`getFuzzyFileSuggestions` 的排序是 `(score desc, depth asc, length asc, localeCompare)`。两处都必须一致。

### 6.5 其它桥接要求

1. **UTF-16 索引**。JS 的所有索引（`cursorCol`、`slice`、`textStart/textEnd`、`row`、`startCol/endCol`）是 **UTF-16 code unit**。Rust 字符串是 UTF-8。两条路线：
   - **A（推荐）**：内部一律用 `Vec<u16>` 或 `Utf16String` 表示"可编辑文本/光标"（`editor`、`input`、`autocomplete`、`search` 的文本偏移），只在渲染/输出时转 `String`。这保证 `cursorCol`、`item.value.length`、`slice` 的行为与 JS 完全一致（emoji 占 2）。
   - **B**：用 `String` + `char_indices()` 映射，但**每个对外偏移量**都需换算成 UTF-16 单位。
   `AltScreenSearchSegment` 的 `startCol/endCol` 是**终端列**（不是 UTF-16），`textStart/textEnd` 是 UTF-16；两者不可混。
2. **浮点位级一致**。`colors.test.ts` 用 `assert.deepStrictEqual` 比较 `rgbToOkhsl` 的输出浮点；`oklchColor`/`okhslColor` 的往返也必须**完全**相等。因此 `Math.cbrt`/`Math.cos`/`Math.sin`/`Math.atan2`/`Math.hypot`/`Math.pow` 必须与 V8 一致 → 内嵌 fdlibm 或使用 `libm` crate（`libm` 是 musl 的移植，与 fdlibm 同源，通常位级一致）。
3. **数字→字符串**。见 §3.1 末尾的 ECMAScript Number::toString 要求（错误消息与 `colorToHex` 的 `toString(16)` 都要一致）。
4. **`\s` 集合**（JS vs Rust）与 **`\p{Punctuation}`**（`regex` 有 `\p{P}`，可用）。
5. **`Intl.Segmenter` 的 `isWordLike`**：`unicode-segmentation` 的 `split_word_bounds` 不直接给 `isWordLike`；需用 `Word_Break` 属性自行判定（`ALetter`/`Hebrew_Letter`/`Numeric`/`Katakana`/`ExtendNumLet` 的组合）。ICU4X 的 `WordSegmenter::segment_str` 返回 `WordType`（`None`/`Number`/`Letter`/`Katakana`/…），`isWordLike = type != None`。
6. **`Object.freeze` 与 `readonly`**：无运行时影响。
7. **`structuredClone`**：`UndoStack` 的快照必须是**深拷贝**（若快照含 `Vec<String>`，`Clone` 即深拷贝）。
8. **取消语义**：`AbortSignal` 的 `signal.aborted` 可能在 `await` 期间变 true；Rust 的 `CancellationToken` 需在**每个** `await` 后检查（`getFuzzyFileSuggestions` 在两次 fd 之间、`walkDirectoryWithFd` 在 spawn 前）。

---

## 7. 测试清单（逐文件，用于改写为 Rust 测试）

本单元直接相关的测试文件（共 14 个，与 §0.1 的清单一致）与断言要点。测试用 Node 内置 `node:test` + `node:assert`（`assert.strictEqual`/`deepStrictEqual`/`ok`/`throws`），Rust 侧用 `#[test]` + `assert_eq!`/`assert!`。

### 7.1 `test/visible-width.test.ts`（4 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `measures styled ASCII without counting escape sequences` | **逐字 4 条**：`visibleWidth("\x1b[38;5;4mhello\x1b[39m world") === 11`；`visibleWidth("\x1b]8;;https://example.com\x07link\x1b]8;;\x07") === 4`；`visibleWidth("\x1b]133;A\x1b\\prompt") === 6`；`visibleWidth("\x1b_kiana:c\x07cursor") === 6` |
| `counts tabs as three columns in styled text` | `visibleWidth("\x1b[1ma\tb\x1b[22m") === 5` |
| `measures styled non-ASCII text` | `visibleWidth("\x1b[31m日本\x1b[39m ok") === 7`；`visibleWidth("\x1b[31m─→\x1b[39m") === 2` |
| `treats unterminated escape sequences as zero-width control characters` | `visibleWidth("\x1b[31") === 3`；`visibleWidth("a\x1b") === 1` |

### 7.2 `test/wrap-ansi.test.ts`（19 个 `it`，4 个嵌套 `describe`）

| 分组 / 测试 | 断言要点 |
| --- | --- |
| underline / `should not apply underline style before the styled text` | `"read this thread " + \x1b[4m + url + \x1b[24m` 宽 40：第 0 行**等于** `"read this thread"`（**不含** `\x1b[4m`）；第 1 行 `startsWith("\x1b[4m")` 且含 `"https://"` |
| underline / `should not have whitespace before underline reset code` | `\x1b[4m + "underlined text here " + \x1b[24m + "more"` 宽 18：第 0 行**不含** `" \x1b[24m"` |
| underline / `should not bleed underline to padding - each line should end with reset for underline only` | `"prefix " + \x1b[4m + url + \x1b[24m + " suffix"` 宽 30：对 `i ∈ [1, len-1)` 中**含** `\x1b[4m` 的行，必须 `endsWith("\x1b[24m")` 且 `endsWith("\x1b[0m") === false` |
| background / `should preserve background color across wrapped lines without full reset` | `\x1b[44mhello world this is blue background text\x1b[0m` 宽 15：**每行**都含 `\x1b[44m`；除末行外每行 `endsWith("\x1b[0m") === false` |
| background / `should reset underline but preserve background when wrapping underlined text inside background` | `\x1b[41mprefix \x1b[4mUNDERLINED_CONTENT_THAT_WRAPS\x1b[24m suffix\x1b[0m` 宽 20：每行都含背景 41（`[41m` / `;41m` / `[41;` 任一形态）；非末行若带下划线且**不含** `\x1b[24m`，则该行必须 `endsWith("\x1b[24m")` 且 `endsWith("\x1b[0m") === false` |
| basic / `should handle LF, CRLF, and CR line endings` | `wrapTextWithAnsi("first\nsecond\r\nthird\rfourth", 80)` **deepStrictEqual** `["first","second","third","fourth"]`（CRLF/CR 均当行分隔） |
| basic / `should preserve ANSI state across CRLF and CR line endings` | `\x1b[31mfirst\r\nsecond\rthird\x1b[0m` → **deepStrictEqual** `["\x1b[31mfirst","\x1b[31msecond","\x1b[31mthird\x1b[0m"]` |
| basic / `should wrap plain text correctly` | `"hello world this is a test"` 宽 10：`wrapped.length > 1` 且每行 `visibleWidth <= 10`（**非**精确数组） |
| basic / `should break CJK runs at grapheme boundaries after Latin text` | `"This is an example 中文汉字测试段落内容中文汉字测试段落内容."` 宽 40 **deepStrictEqual** `["This is an example 中文汉字测试段落内容","中文汉字测试段落内容."]` |
| basic / `should preserve color codes when wrapping CJK runs` | 同上文本包 `\x1b[31m…\x1b[0m`：`length===2`，第 0 行 `\x1b[31m…段落内容`（**无** reset），第 1 行 `\x1b[31m中文…内容.\x1b[0m`（重发颜色 + 仅在末行 reset） |
| basic / `should ignore OSC 133 semantic markers in visible width` | `visibleWidth("\x1b]133;A\x07hello\x1b]133;B\x07") === 5` |
| basic / `should ignore OSC sequences terminated with ST in visible width` | `visibleWidth("\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\") === 5` |
| basic / `should treat isolated regional indicators as width 2` | `visibleWidth("🇨") === 2` 且 `visibleWidth("🇨🇳") === 2` |
| basic / `should truncate trailing whitespace that exceeds width` | `wrapTextWithAnsi("  ", 1)` 的第一行 `visibleWidth <= 1`（行尾空白被裁掉，不是保留两空格） |
| basic / `should preserve color codes across wraps` | `\x1b[31mhello world this is red\x1b[0m` 宽 10：第 1 行起每行 `startsWith("\x1b[31m")`；除末行外每行 `endsWith("\x1b[0m") === false` |
| OSC 8 / `re-emits OSC 8 open at the start of continuation lines` | 输入 `\x1b]8;;https://example.com\x1b\\0123456789\x1b]8;;\x1b\\` 宽 6：剥掉 OSC 8/SGR 后仍有可见文本的每一行都必须含该 OSC 8 open（**属性断言**，非逐字数组；注意这里用 **ST** 终止符） |
| OSC 8 / `closes OSC 8 before each line break` | 同一输入宽 6：每个**非末行**若含开链接序列，则必须 `endsWith("\x1b]8;;\x1b\\")`（ST 关闭） |
| OSC 8 / `preserves BEL terminators when wrapping OAuth-style hyperlinks` | `url = "https://example.com/oauth/" + "a".repeat(32)`；宽 20：`lines.length > 1`；**每行**都含 `\x1b]8;;${url}\x07` 且**不含** `\x1b]8;;${url}\x1b\\`；除末行外每行 `endsWith("\x1b]8;;\x07")` |
| OSC 8 / `does not emit OSC 8 sequences on lines that are outside the hyperlink` | `"before \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ after"` 宽 80 → `lines.length === 1`，且该行 OSC 8 open / close 各出现**恰好 1** 次 |

### 7.3 `test/truncate-to-width.test.ts`（16 个 `it`，两个 `describe`）

| 测试 | 断言 |
| --- | --- |
| `keeps output within width for very large unicode input` | `"🙂界".repeat(100_000)`，`truncateToWidth(text, 40, "…")`：宽 ≤ 40 且以 `"…\x1b[0m"` 结尾 |
| `preserves ANSI styling for kept text and resets before and after ellipsis` | `\x1b[31m + "hello ".repeat(1000) + \x1b[0m`，宽 20、`"…"`：宽 ≤ 20、含 `\x1b[31m`、以 `"\x1b[0m…\x1b[0m"` 结尾 |
| `closes a BEL-terminated OSC 8 link when truncating its label` | **逐字**：`truncateToWidth("\x1b]8;;https://example.com\x07some-longer-label-here\x1b]8;;\x07", 15)` = `"\x1b]8;;https://example.com\x07some-longer-\x1b]8;;\x07\x1b[0m...\x1b[0m"`（默认省略号 `"..."`、链接在省略号前关闭） |
| `handles malformed ANSI escape prefixes without hanging` | `"abc\x1bnot-ansi " + "🙂".repeat(1000)`，宽 20、`"…"`：宽 ≤ 20 |
| `clips wide ellipsis safely and brackets it with resets` | `truncateToWidth("abcdef",1,"🙂")` = `""`；`truncateToWidth("abcdef",2,"🙂")` = `"\x1b[0m🙂\x1b[0m"`（宽 2） |
| `returns the original text when it already fits even if ellipsis is too wide` | `truncateToWidth("a",2,"🙂")` = `"a"`；`truncateToWidth("界",2,"🙂")` = `"界"` |
| `pads truncated output to requested width` | `truncateToWidth("🙂界🙂界🙂界", 8, "…", true)` 的 `visibleWidth` = **8** |
| `adds a trailing reset when truncating without an ellipsis` | `\x1b[31m + "hello".repeat(100)`，宽 10、`""`：宽 ≤ 10 且以 `\x1b[0m` 结尾 |
| `keeps a contiguous prefix instead of skipping a wide grapheme and resuming later` | **逐字**：`truncateToWidth("🙂\t界 \x1b_abc\x07", 7, "…", true)` = `"🙂\t\x1b[0m…\x1b[0m "`（TAB 占 3；跳过 `界`，不"跳过后再接续"） |
| visibleWidth / `counts Myanmar marks that terminals allocate cells for` | 12 个断言：`ကာ`/`ကေ`/`က်`/`ကျ`/`ကြ`/`ကဳ`/`ကဴ`/`ကဵ`/`ကး` 均 = **2**；`ကို` = **1**；`က္` = **1** |
| visibleWidth / `keeps Thai and Lao AM clusters at their normal cell width` | `ำ`=1、`ຳ`=1、`กำ`=2、`ກຳ`=2 |
| visibleWidth / `keeps ordinary combining marks zero-width` | `"e\u0301"`=1、`"čřžůú"`=5、`"שָׁ"`=1、`"بّ"`=1、`"རྐ"`=1、`"ᜠ᜴"`=1、`"가〮"`=2、`"가〯"`=2 |
| visibleWidth / `counts Indic conjunct spacing code points within grapheme clusters` | `"र्क"`=2、`"नेटवर्क"`=5、`"सर्वाधिकार सुरक्षित। ऑर्डर पर क्लिक करें"`=33、`"র্ক"`/`"ર્ક"`/`"ର୍କ"`/`"ర్క"`/`"ര്‍ക"` 均 = 2 |
| visibleWidth / `keeps CJK and Japanese width accounting unchanged` | `"网络"`=4、`"ネットワーク"`=12、`"が"`=2、`"か\u3099"`=2 |
| visibleWidth / `counts tabs inline and skips ANSI inline` | `"\t\x1b[31m界\x1b[0m"`=5 |
| visibleWidth / `normalizes Thai and Lao AM vowels only for terminal output` | `normalizeTerminalOutput("ำ") === "ํา"`（U+0E33 → U+0E4D U+0E32）、`normalizeTerminalOutput("ຳ") === "ໍາ"`（U+0EB3 → U+0ECD U+0EB2）；且 `visibleWidth(normalizeTerminalOutput("ำabc")) === visibleWidth("ำabc")`、`…("ຳabc") === …("ຳabc")`（重排不改变宽度） |

### 7.4 `test/tab-width.test.ts`（4 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `keeps slice helper widths consistent with visible width` | `sliceWithWidth("out 192M\t.kiana/skill-tests/results-ha", 0, 10, true)` → `{text:"out 192M", width:8}`，且 `visibleWidth(text) === width`（`\t` 宽 3 且 `strict=true` 时不切进 TAB） |
| `keeps overlay segment widths consistent with visible width` | `extractSegments(同上, 10, 13, 10, true)` → `before="out 192M"`/`beforeWidth=8`；`extractSegments(同上, 11, 13, 10, true)` → `before="out 192M\t"`/`beforeWidth=11`（两者 `after="i/skill-te"`、`afterWidth=10`；TAB 整体计入宽度） |
| `keeps tabs inside terminal control sequences byte-identical` | 三个序列逐字 `"\x1b]8;;https://example.test/a\tb\x07"`、`"\x1b]0;window\ttitle\x1b\\"`、`"\x1b_payload\tdata\x1b\\"`；`normalizeTerminalOutput(seq + "label\ttext") === seq + "label   text"`（序列内 TAB 原样，序列外 TAB → 3 空格） |
| `keeps tab-containing overlays on one physical terminal row` | `CapturingVirtualTerminal(16, 3)` + `TuiMainScreen`，overlay `{width:4,row:1,col:4}` 内容 `["\tX"]`；视口逐字 `["base 0          ","base   X        ","base 2          "]`，且捕获的输出**不含** `\t`（TAB 已被展开） |

该文件**直接**导入 `../src/utils.ts` 的 `extractSegments`、`normalizeTerminalOutput`、`sliceWithWidth`、`visibleWidth`（并导入 `../src/tui-main-screen.ts` → 间接引入 `virtual-terminal.ts` → `@xterm/headless`，所以缺该 devDependency 时**整文件**在导入阶段失败，4 个 `it` 全部不运行——实跑表现为 `tests 1 / fail 1` 的“文件级失败”，不是 1 个测试用例）。

### 7.5 `test/regression-regional-indicator-width.test.ts`（5 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `treats partial flag grapheme as full-width to avoid streaming render drift` | `visibleWidth("🇨") = 2`；`visibleWidth("      - 🇨") = 10` |
| `wraps intermediate partial-flag list line before overflow` | `wrapTextWithAnsi("      - 🇨", 9)` → 长度 **2**；`visibleWidth(wrapped[0]) === 7`、`visibleWidth(wrapped[1]) === 2`（8 + 2 = 10 > 9 必须回绕） |
| `treats all regional-indicator singleton graphemes as width 2` | U+1F1E6–U+1F1FF **全部 26 个**逐一遍历 = 2（断言消息含码点） |
| `keeps full flag pairs at width 2` | 样本逐字：`["🇯🇵","🇺🇸","🇬🇧","🇨🇳","🇩🇪","🇫🇷"]` 全部 = 2 |
| `keeps common streaming emoji intermediates at stable width` | 样本逐字：`["👍","👍🏻","✅","⚡","⚡️","👨","👨‍💻","🏳️‍🌈"]`（含 U+FE0F 变体与 ZWJ 序列）全部 = 2 |

### 7.6 `test/regression-overlay-cjk-boundary.test.ts`（4 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `excludes a wide grapheme from before when overlay starts inside it` | `extractSegments("abcd让EFGH",5,9,11,true)` → `before="abcd"`、`beforeWidth=4`、`after="H"`、`afterWidth=1`（宽字素 `让` 被整段丢弃） |
| `keeps ASCII before-segment behavior at the same boundary` | `extractSegments("abcdG EFGH",5,9,11,true)` → `before="abcdG"`、`beforeWidth=5`，且 `visibleWidth(before) === beforeWidth` |
| `composites an overlay at the requested column when it starts inside a wide grapheme` | `compositeTuiLine("abcd让EFGH","│XX│",5,4,20)`：`out` **不含** `让`、`visibleWidth(out)===20`、`sliceByColumn(out,0,5,true)` 宽 5、`sliceByColumn(out,5,4,true)` 宽 4 且含 `│XX│` |
| `composites an overlay when it starts at a wide grapheme boundary` | `compositeTuiLine(…,4,4,20)`：同样不含 `让`、总宽 20、overlay 宽 4 且含 `│XX│` |

### 7.7 `test/regression-slice-by-column-ansi-order.test.ts`（2 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `keeps a reset at the slice start after earlier style codes` | `sliceByColumn("\x1b[32mfoo\x1b[39m bar", 3, 4, true) === "\x1b[32m\x1b[39m bar"`（切片起点**保留**前缀样式码与复位码，顺序不变） |
| `does not leak color into text after a highlighted token` | `sliceByColumn("Another \x1b[35malpha\x1b[39m line with \x1b[35mbeta\x1b[39m later.", 13, 100, true) === "\x1b[35m\x1b[39m line with \x1b[35mbeta\x1b[39m later."` |

### 7.8 `test/colors.test.ts`（4 个 `it`）

| 测试 | 断言 |
| --- | --- |
| `parses hex and OKLCH colors and rejects everything else` | `parseColor("#abc")` **deepStrictEqual** `{kind:"rgb", r:170, g:187, b:204}`；`parseColor("oklch(62% 0.1 200)")` → `{kind:"oklch", l:0.62, c:0.1, h:200}`；`assert.throws(() => parseColor(""), /Invalid color value/)`；`assert.throws(() => parseColor("red"), /Invalid color value/)` |
| `gamut-maps OKLCH to sRGB, including the lightness limits` | `colorToRgb(oklchColor(0.627955, 0.257683, 29.2339))` = `{r:255,g:0,b:0}`（红 cusp）；`colorToRgb(oklchColor(1, 0.3, 150))` = `{r:255,g:255,b:255}`；`colorToRgb(oklchColor(0, 0.3, 150))` = `{r:0,g:0,b:0}` |
| `parses OKHSL colors and round-trips them` | `parseColor("okhsl(29.23 100% 56.8%)")` **deepStrictEqual** `rgbColor(255,0,0)`；`parseColor("OKHSL(250deg 60% 55%)")` **deepStrictEqual** `okhslColor(250,0.6,0.55)`（**注意两侧都是 RGB 表示**）；`assert.throws(parseColor("okhsl(250 160% 55%)"), /s must be between 0 and 1/)`；对 `["#4f8eb3","#20242a","#f8f9fa"]` 逐个 `colorToHex(okhslColor(...colorToOkhsl(parseColor(hex)))) === hex` |
| `styles text and closes sequences in reverse order` | `styleText("Ready", {fg:rgbColor(18,52,86), bg:indexedColor(9), bold:true, italic:true}, "truecolor")` **逐字** `"\x1b[38;2;18;52;86m\x1b[48;5;9m\x1b[1m\x1b[3mReady\x1b[23m\x1b[22m\x1b[49m\x1b[39m"`；`styleText("Ready", {fg:rgbColor(18,52,86)}, "256color")` 匹配 `/^\x1b\[38;5;\d+mReady\x1b\[39m$/`（实测 `\x1b[38;5;23m`） |

### 7.9 `test/fuzzy.test.ts`（17 个 `it`，两个 `describe`：`fuzzyMatch` 10 + `fuzzyFilter` 7）

| 组 | 逐字用例名 | 关键断言 |
| --- | --- | --- |
| fuzzyMatch | `empty query matches everything with score 0` | `{matches:true, score:0}`（`score` 精确 0） |
| | `query longer than text does not match` | `matches:false` |
| | `exact match has good score` | `matches:true` **且 `score < 0`**（不锁具体值） |
| | `characters must appear in order` | `("abc","aXbXc")` true；`("abc","cba")` false |
| | `case insensitive matching` | 双向 |
| | `consecutive matches score better than scattered matches` | `("foo","foobar").score < ("foo","f_o_o_bar").score` |
| | `word boundary matches score better` | `("fb","foo-bar").score < ("fb","afbx").score` |
| | `scores a contiguous occurrence instead of an earlier scattered alignment` | #11220：`opus` 在 `claude-opus-4.1` 上 `<` 在 `claude-sonnet-4.5` 与 `gpt-oss-120b` 上 |
| | `uses the best contiguous occurrence, not the first` | `("opus","xxopus opus").score === ("opus","xxxxxx opus").score`（**精确相等**） |
| | `matches swapped alpha numeric tokens` | `("codex52","gpt-5.2-codex")` true（走 swapped 分支，+5 惩罚） |
| fuzzyFilter | `empty query returns all items unchanged` | `deepStrictEqual(result, items)`（**同值**；实现上还返回同一引用） |
| | `filters out non-matching items` | `"an"` 只留 `"banana"` |
| | `sorts results by match quality` | `["a_p_p","app","application"]` → `result[0] === "app"` |
| | `prioritizes exact matches over longer prefix matches` | `["clone","cl"]` + `"cl"` → **deepStrictEqual** `["cl","clone"]` |
| | `works with custom getText function` | 对象数组 + `getText = item => item.name`，长度 2 且含 `foo`/`foobar` |
| | `ranks a model containing the query above models matching a shared provider prefix` | 4 个 `radius …` 模型 + `"astra"` → `result[0] === items[3]`（`…gpt-6-astra…`） |
| | `matches slash-separated provider/model queries against reordered text` | `fuzzyFilter([{id:"gpt-5.5",provider:"openai-codex"}], "openai-codex/gpt-5.5", m => \`${m.id} ${m.provider}\`)` **deepStrictEqual** `[item]`（token 用 `/` 拆开、各自独立匹配同一文本） |

具体期望分数见 §2.2 的逐位实测表（`-159.4`、`-39.699999999999996` 等，需 IEEE-754 位级一致）。

### 7.10 `test/latex.test.ts`（实跑 111 个用例：**19** 个显式 `it` + **92** 个 `defineCases` 生成）

`defineCases` 生成的每个 `it(\`renders ${JSON.stringify(source)}\`)` 断言 `renderLatex(source, options) === expected`。分组：

- `Jacobian conjecture session using dollar delimiters`
- `satellite calculation session using bracket delimiters`
- `Jacobian conjecture sessions using parenthesis and bracket delimiters`
- `Jacobian matrix session using dollar delimiters`
- `extended formulas from a renderer stress-test session`
- 19 个显式 `it`（逐字名称，按文件顺序；**Rust 侧一一对应**）：

```
renders common symbols, roots, sums, and integrals
renders common accents and binomial notation
renders extended symbols and negated relations
renders relational algebra join operators
renders delimiter commands and invisible delimiters
renders named, modular, overlaid, and underlaid operators
renders indexed roots and additional accents and wrappers
renders additional display environments
renders natural case conditions and aligns matrix columns
renders composes matrices with fractions and adjacent matrices   ← 源码字面量如此（"renders composes"）
normalizes relation, multiplication, and named-operator spacing
treats a backslash followed by a line ending as control space
stacks operator limits in display mode
centers even case rows around a middle brace
stacks fractions in display mode
keeps nested display fractions linear
lays out unsupported and nested scripts while keeping script fractions linear
returns undefined for unsupported commands
returns undefined for malformed groups and environments
```

（其中 `renders composes matrices with fractions and adjacent matrices` 是源码里的**原文**—— `renders` 与 `composes` 连用，不是本规范的笔误；Rust 测试名可自由改写，但 fixture 名若沿用需保持一致。）

**Rust 测试策略**：把 `defineCases` 的 (source, expected) 二元组导出为 JSON fixture（`tests/fixtures/latex-cases.json`，**共 92 条**，5 个 `defineCases` 块分别为 **37 / 8 / 5 / 25 / 17**；**注意 `renderLatex(source)` 用默认 `{}` 选项，即内联模式**），Rust 用 `include_str!` + `serde_json` 驱动。此外 **19** 个显式 `it` 里另有 display 模式的用例（`{display:true}`），需一并导出为含 `options` 字段的三元组。合计 **19 + 92 = 111** 个断言全部逐字一致。

> 计数更正（审计核实）：`grep -c 'it('` 得到的 20 里，第 20 个是 `defineCases` 内部的**模板** `` it(`renders ${JSON.stringify(source)}`, …) ``，不是用例。正确计数：`grep -c '^\tit('` = **19**；`defineCases` 数组元素（`^\t\t\t\[`）= 37+8+5+25+17 = **92**。

### 7.11 `test/word-navigation.test.ts`（19 个 `it`，3 个 `describe`）

见 §2.3 的逐字断言表。用例分布：`findWordBackward` **8** 个、`findWordForward` **8** 个、`atomic segments` **3** 个（合计 19，3 个 `describe`）。

`atomic segments` 段用**注入的 `segment` 函数**（`WordNavigationOptions.segment`）替换真实分段器：测试自建 `segmentMap`，把 `text` / `text.slice(0,26)` / `text.slice(6)` 映射到手工构造的 `Intl.SegmentData[]`（伪造 `index`/`input`/`isWordLike`）。Rust 侧同样需要可注入的 `segment` 回调；且必须注意**源码先 `slice` 再调 `segment`**，因此回调收到的 `input` 是**切片后的串**，`index` 相对切片起点。

### 7.12 `test/autocomplete.test.ts`（41 个用例：4 个 `it` + 37 个 `test()`；其中 **24** 个 fd 用例被 `skip: !isFdInstalled` 跳过，其余 17 个在无 fd 时也运行）

| 分组 | 测试 | 断言 |
| --- | --- | --- |
| `extractPathPrefix`（4 个 `it`） | `extracts / from 'hey /' when forced` | `force` 时 `prefix === "/"` |
| | `extracts /A from '/A' when forced` | `prefix === "/A"` |
| | `does not trigger for slash commands` | `getSuggestions(["/mod"], …, {force:false})` 走命令分支（无命令时 `null`） |
| | `triggers for absolute paths after slash command argument` | `"/model /Us"` → prefix `"/Us"` |
| `fd @ file suggestions`（24 个，整组 `skip: !isFdInstalled`） | 逐字名称见下 | 建议项、`@` 引号包裹、目录尾斜杠、`description` = 显示路径、20 条截断、`-L`/`--full-path` 参数、排序 |
| `dot-slash path completion`（9 个，不依赖 fd） | 逐字名称见下 | 前导空白/CJK 标点后的空前缀、wrapper、`./` 前缀、CJK 保留 |
| `quoted path completion`（4 个，不依赖 fd） | 逐字名称见下 | 引号包裹、光标位置、不重复补右引号 |

**24 个 fd 用例名（逐字，Rust 侧移植时按名一一对应）**：

```
returns all files and folders for empty @ query
recognizes @ after CJK punctuation without consuming the preceding text
recognizes @ after opening wrappers like ( and backticks
preserves CJK characters and embedded @ in attachment paths
completes quoted CJK attachments after prose without losing path segments or quotes
does not interpret email addresses or @ after ASCII or CJK letters as attachment prefixes
matches file with extension in query
matches regex metacharacters in queries literally
filters are case insensitive
ranks directories before files
returns nested file paths
matches deeply nested paths
matches directory in middle of path with --full-path
scopes fuzzy search to relative directories and searches recursively
ranks shallower same-score @ matches before deeper matches
includes scoped direct children when recursive @ matches are flooded
quotes paths containing whitespace or CJK punctuation for @ suggestions
includes hidden paths but excludes .git
follows symlinked directories for fuzzy @ search
returns symlinked directories when matching their name
returns symlinked files without requiring type l
returns the same @ suggestions when the cwd path contains the query
continues autocomplete inside quoted @ paths
applies quoted @ completion without duplicating closing quote
```

**9 个 dot-slash 用例名**：

```
completes Chinese path prefixes after whitespace or CJK punctuation on Tab
treats unquoted separators as boundaries even when a matching literal path exists
handles an empty prefix after whitespace or CJK punctuation consistently
completes paths after opening wrappers like ( [ { < and backticks
completes quoted paths after opening wrappers
keeps wrappers that are closed inside the path
preserves CJK characters in unprefixed Tab completions
preserves ./ prefix when completing paths
preserves ./ prefix for directory completions
```

**4 个 quoted 用例名**：

```
quotes paths containing whitespace or CJK punctuation for direct completion
keeps quoted directories before files
continues completion inside quoted paths
applies quoted completion without duplicating closing quote
```

> **注**：`fd @ file suggestions` 组里 `includes hidden paths but excludes .git`、`follows symlinked directories for fuzzy @ search`、`returns symlinked files without requiring type l` 这三条把 `fd` 的 `--hidden`/`--follow`/`-t` 语义固化成了**行为契约**（Rust 侧若用自建 walker 必须复刻），原先规范只写了"建议项/排序"这层概括。

**用例计数（实跑确认）**：`autocomplete.test.ts` 共 41 个用例 = 4 个 `it`（`extractPathPrefix` 组）+ 37 个 `test()`。
其中 `fd @ file suggestions` 组 **24** 个带 `{ skip: !isFdInstalled }`；
`dot-slash path completion`（9 个）与 `quoted path completion`（4 个）**不依赖 fd**，用 `mkdtempSync` 建临时目录 + 同步 readdir 走 `getFileSuggestions`。

无 fd 时 `node --test test/autocomplete.test.ts` 的实测输出：

```
﹣ fd @ file suggestions (0.168246ms) # SKIP
ℹ tests 17   ℹ suites 5   ℹ pass 17   ℹ fail 0   ℹ skipped 0
```

（注意 `skipped 0` —— 被 `describe` 级 `skip` 整组跳过的用例**不计入** `tests`/`skipped` 统计，只打印一行 `# SKIP`。）

**fd 探测代码（逐字）**：

```ts
const resolveFdPath = (): string | null => {
  const command = process.platform === "win32" ? "where" : "which";
  const result = spawnSync(command, ["fd"], { encoding: "utf-8" });
  if (result.status !== 0 || !result.stdout) return null;
  const firstLine = result.stdout.split(/\r?\n/).find(Boolean);
  return firstLine ? firstLine.trim() : null;
};
```

即 **不是** `fd --version`，而是 `which fd`（Windows `where fd`），取 stdout 第一行并 trim 作为 `fdPath`。
`skip: !isFdInstalled` 用的是这个返回值的真假。Rust 侧同一逻辑：`which`/`where` 子进程 + 首行。

**本机当前未安装 fd**（`which fd` 为空），因此本规范中所有 fd 相关行为只能从源码与测试断言推导，无法实跑验证——
这一点必须在 Rust 侧的 CI 里补跑（安装 fd 后执行等价的 24 个 fd 用例）。

### 7.13 `test/autocomplete-skill-slash.test.ts`（7 个 `it`）

见 §2.10.11。7 条逐字用例名与断言：

| # | 用例名 | 断言 |
| --- | --- | --- |
| 1 | `ranks skill:research-idea first for query 'idea'` | `items[0] === "skill:research-idea"`，且 `indexOf("skill:deep-research") > indexOf("skill:research-idea")` |
| 2 | `keeps ordinary slash commands matching` | `"/mod"` 的 items 含 `"model"` |
| 3 | `completes commands after leading whitespace and preserves it` | 三组 `[line, expected]`：`[" /"," /model "]`、`["  /mod","  /model "]`、`["\t/mod","\t/model "]`；每组还断言 `result.prefix === line.trimStart()`、items 恰为 `["model"]`、`applyCompletion(...).lines[0] === expected` 且 `cursorCol === expected.length` |
| 4 | `completes command arguments after leading whitespace` | `getArgumentCompletions` 收到的 prefix **必须是 `"son"`**（内部 `assert.equal`）；`result.prefix === "son"`；应用后 `"  /model sonnet"` |
| 5 | `keeps explicit skill: queries working` | `"/skill:side"` 的 items 含 `"skill:to-sidecar"` |
| 6 | `lists skills while typing the skill prefix`（#9944） | `"/skill"` 的 items 中**所有** `startsWith("skill:")` 的项，**deepStrictEqual** 命令表里所有 `skill:` 命令名（即顺序也一致：`deep-research`、`research-idea`、`to-sidecar`、`brainstorm`） |
| 7 | `keeps fuzzy skill-prefix shorthand working` | `"/skbra"` 的 items 含 `"skill:brainstorm"` |

fixture 命令表（逐字）：

```ts
const commands = [
  { name: "skill:deep-research", description: "Multi-agent deep research" },
  { name: "skill:research-idea", description: "Refine a raw idea into a falsifiable seed" },
  { name: "skill:to-sidecar", description: "Route work to a sidecar" },
  { name: "skill:brainstorm", description: "Generate ideas" },
  { name: "model", description: "Select the active model" },
];
```

注意第 6 条对**顺序**做了 `deepStrictEqual`：`skill:` 折叠逻辑必须先输出"裸名命中"、再追加"仅全名命中"的 `skill:` 项；此处 `prefix = "skill"` 时裸名（去 `skill:` 后）没有任何一个以 `skill` 开头，所以全部走第二段，顺序 = 命令表顺序。

### 7.14 `test/tui-alt-screen.test.ts` 的搜索部分（8 个 `it`，行 597–842）

| 测试 | 断言 |
| --- | --- |
| `searches normalized rendered transcript text across rows` | `findAltScreenSearchMatches(["alpha QUICK","brown fox"], "quick brown")` **deepStrictEqual** `[{segments:[{row:0,startCol:6,endCol:11},{row:1,startCol:0,endCol:5}]}]` |
| `maps normalized ASCII and Unicode search matches back to rendered columns` | `findAltScreenSearchMatches(["\x1b[31mfoo  bar\x1b[0m","A界🙂éZ"], "oo   bar\nA界🙂é")` **deepStrictEqual** `[{segments:[{row:0,1,3},{row:0,5,8},{row:1,0,6}]}]` |
| `reuses indexed transcript matches until the query or rendered lines change` | `search(["alpha needle","omega"],"needle")` → `changed:true`、`matches.length===1`；同参数再查 → `changed:false` 且 `cached.matches === initial.matches`（**引用相等**）；query 换 `"omega"` → `changed:true`、引用不同、`segments` 为 `[{row:1,startCol:0,endCol:5}]`；行改成 `"no match"` → `changed:true`、`matches` 为 `[]` |
| `renders transcript search with a muted placeholder and right-aligned controls` | `render(48)`：3 行，每行 `visibleWidth === 48`；第 0 行 `/^┌─+┐$/`；第 1 行 `/^│ Find in transcript +│$/` 且**含** `\x1b[2m`；第 2 行 `/^└─+ ↑ Shift\+Enter · ↓ Enter ─┘$/`；`getNavigationDirectionAt(2, indexOf("↑")) === -1`、`(2, indexOf("Shift+Enter")+5) === -1`、`(2, indexOf("·")) === undefined`、`(2, indexOf("↓")) === 1`、`(2, lastIndexOf("Enter")+2) === 1`；`handleInput("n")` + `setResult(0,2)` 后第 1 行含 `1/2` 与 `\x1b[2m 1/2 \x1b[22m`，且**不再**含 `Find in transcript` |
| `navigates transcript search with hoverable arrow buttons and toggles it with its shortcut` | 120×6 终端，`searchNavigationButtonStyle` 注入 `\x1b[44m`/悬停 `\x1b[45m`+`\x1b[49m`；`\x1b[102;6u`（Ctrl+Shift+F）打开 → 视口含 `1/2` 与 `↑ Shift+Enter · ↓ Enter`；SGR 鼠标 `\x1b[<35;C;RM`（motion）到底行 → 输出含 `\x1b[45m↓ Enter\x1b[49m`；左键 `\x1b[<0;C;RM` → `2/2`；再点 `↑` 区 → 回 `1/2`；再按 Ctrl+Shift+F → 控制条消失 |
| `does not treat transcript box drawing as search navigation buttons` | 80×10 终端，转录含自绘 `┌─┐││└─┘` 方框；在**方框底行**第 24 列左键 → 仍 `1/2`，**不**变 `2/2`（`getNavigationDirectionAt` 要求 `row===2` 且落在按钮列区间） |
| `uses configured styles for current and non-current search matches` | 60×4，`searchMatchStyle = t => \x1b[41m${t}\x1b[49m`、`searchCurrentMatchStyle = t => \x1b[42m${t}\x1b[49m`；输出**同时**含 `\x1b[42mneedle\x1b[49m` 与 `\x1b[41mneedle\x1b[49m` |
| `searches the transcript with Ctrl+Shift+F and restores editor focus on close` | 60×8，`ScrollView{follow:"end",primary:true}` + `VStack` + 自定义 editor；打开后 `transcript.isFollowingEnd === false`、视口含 `2/2` 与 `line 10 needle two`、`editorInputs` 仍为 `[]`、输出含 `\x1b[1;7mneedle\x1b[22;27m`（当前匹配高亮）；6 次滚轮后 `scrollTop === 0` 且仍 `2/2`；`\x07`（Ctrl+G）跳到上一个 → `1/2` 且可见 `line 5 needle one`；`\x1b[103;6u` → `2/2`；`Esc` 关闭，随后 `x` 进入 editor（`editorInputs === ["x"]`），控制条消失 |

### 7.15 间接覆盖本单元的其它测试（须在 Rust 全量回归中保留）

| 文件 | 覆盖的本单元行为 |
| --- | --- |
| `test/editor.test.ts`（192 个 `it`） | 换行（宽 emoji/CJK/Thai-Lao AM）、光标定位、`findWord*`（Ctrl+W/Alt+D/Ctrl+Left/Right）、kill/yank/yank-pop、undo、autocomplete 集成；`editor.ts` **直接**导入 `autocompleteBoundaryRegex`/`autocompleteSeparatorRegex`/`cjkBreakRegex`/`getGraphemeSegmenter`/`getWordSegmenter`/`isWhitespaceChar`/`sliceByColumn`/`visibleWidth`（**不含** `truncateToWidth`/`wrapTextWithAnsi`/`applyBackgroundToLine`/`extractAnsiCode`），渲染断言经 `sliceByColumn`/`visibleWidth` 间接覆盖 |
| `test/input.test.ts`（36 个 `it`） | kill ring（含 `Alt+D`）、undo、CJK 宽度、水平滚动 |
| `test/truncated-text.test.ts`（9） | 直接导入 `visibleWidth`；`TruncatedText` 内部用 `truncateToWidth` |
| `test/select-list.test.ts`（5）、`test/settings-list.test.ts`（2）、`test/markdown.test.ts`（92） | 只有 `select-list.test.ts` **直接**导入 `visibleWidth`；`settings-list.test.ts` 只导入 `SettingsList`、`markdown.test.ts` 只导入 `Markdown` + `terminal-image`/`tui`（两者**间接**覆盖 `truncateToWidth`/`wrapTextWithAnsi`/`applyBackgroundToLine`/`flattenLines` 的渲染结果） |
| `test/layout.test.ts`（15）、`test/tui-render.test.ts`（32）、`test/tui-overlay-style-leak.test.ts`（2） | `layout.test.ts` 直接导入 `stripTerminalSequences`；`tui-render.test.ts` 直接导入 `sliceByColumn`；`tui-overlay-style-leak.test.ts` **无**本单元直接导入（仅通过 `TuiMainScreen` 间接覆盖） |
| `test/terminal-colors.test.ts`（7） | 从 `../src/index.ts` 导入（含 `parseColor` 等）验证 `RgbColor` 互操作 |
| `test/regression-overlay-cjk-boundary.test.ts` | `extractSegments` |
| `test/bug-regression-isimageline-startswith-bug.test.ts`（11） | 动态 `await import("../src/terminal-image.ts")` 只取 `isImageLine`（非本单元）；**不**导入本单元符号 |

### 7.16 测试基础设施注意

**本次实跑结果（Node 26.12.1 / ICU 78.3，`/tmp/tui-ref` 副本）**：

| 测试文件 | `tests` | `pass` | `fail` | 备注 |
| --- | --- | --- | --- | --- |
| `visible-width.test.ts` | 4 | 4 | 0 | |
| `wrap-ansi.test.ts` | 19 | 19 | 0 | |
| `truncate-to-width.test.ts` | 16 | 16 | 0 | |
| `tab-width.test.ts` | 1（文件级） | 0 | 1（文件级） | 缺 `@xterm/headless`（`virtual-terminal.ts` 导入）→ **4 个 `it` 全部未运行**，node --test 只报 1 个“文件失败” |
| `regression-regional-indicator-width.test.ts` | 5 | 5 | 0 | |
| `regression-overlay-cjk-boundary.test.ts` | 4 | 4 | 0 | |
| `regression-slice-by-column-ansi-order.test.ts` | 2 | 2 | 0 | |
| `fuzzy.test.ts` | 17 | 17 | 0 | |
| `word-navigation.test.ts` | 19 | 19 | 0 | |
| `autocomplete.test.ts` | 17 | 17 | 0 | 24 个 fd 用例整组 SKIP |
| `autocomplete-skill-slash.test.ts` | 7 | 7 | 0 | |
| `latex.test.ts`（改用 `../src/latex.ts` 直连） | 111 | 111 | 0 | 原文件导入 `../src/index.ts` → 缺 `marked` |
| `colors.test.ts` | — | — | — | 导入 `../src/index.ts` → 缺 `marked`，本规范用直接模块探针逐条验证（见 §2.7 与附录 B.4） |
| `tui-alt-screen.test.ts`（搜索 8 个） | — | — | — | **不**导入 `../src/index.ts`（直接导入 `../src/alt-screen-search.ts` 等）；实跑失败的原因是缺 `@xterm/headless`（`./virtual-terminal.ts`）→ 整文件在导入阶段失败。8 个搜索用例的断言以源码逐字复刻为准（§7.14） |

> **导入面更正（审计核实）**：本单元 14 个测试文件中，只有 `latex.test.ts` 与 `colors.test.ts` 导入 `../src/index.ts`（因而需要 `marked`）；`tui-alt-screen.test.ts`、`visible-width.test.ts`、`wrap-ansi.test.ts`、`truncate-to-width.test.ts`、`tab-width.test.ts`、`regression-*.test.ts`、`fuzzy.test.ts`、`word-navigation.test.ts`、`autocomplete*.test.ts` 都直接导入具体模块。`tui-alt-screen.test.ts` 与 `tab-width.test.ts` 依赖 `@xterm/headless`。


- 环境依赖：`autocomplete.test.ts` 的 24 个 fd 用例需要真实 `fd` 二进制（用 `which fd` 探测；本机没有，因此整组 SKIP，实跑 17 pass / 0 fail）。
- **更正**：`colors.test.ts` 实际导入的是 `../src/index.ts`（不是 `../src/colors.ts`）；`index.ts` 会连带导入 `marked`（`dependencies` 里有 `marked@18.0.11` 与 `marked-cjk-friendly@0.1.2`，需 `npm install` 后才可运行）。本次验证在 `/tmp/tui-ref` 的临时副本中用**直接模块导入**（`colors.ts` / `utils.ts` / `latex.ts` / `fuzzy.ts` / `autocomplete.ts` / `alt-screen-search.ts`）逐条实跑，未依赖 `index.ts`。
- **locale 依赖更正（审计核实）**：`Intl.Segmenter` 用 `new Intl.Segmenter(undefined, …)`，其 locale 由运行时默认 locale 决定，但分段结果与 locale 无关（词典式分词不用于 `granularity:"word"` 的本用例集）；`localeCompare` **确实依赖运行时默认 locale**（见 §6.4）。幸运的是现有断言绝大多数先 `.sort()` 再比较（`autocomplete.test.ts` 的 `.map(...).sort()` / `normalize()`），因此不锁定顺序；唯一可能受影响的排序断言见附录 §A.4。`formatKey` 的 `process.platform` 分支在 Linux 上恒走 `part.charAt(0).toUpperCase() + part.slice(1)`。
- `tui-alt-screen.test.ts` 用 `@xterm/headless` 的虚拟终端断言物理输出（Rust 侧建议用 `vt100` crate 或自建最小屏幕模型）。

---

## 8. 建议的 Rust 模块布局与最难复刻之处

### 8.1 crate 与模块

建议单 crate `kiana-tui-text`（`kiana_tui_text`），从属于 workspace 的 `kiana-tui` 拆分（见 tui-core 的 §8.1）：

```
kiana-tui-text/
  Cargo.toml
  src/
    lib.rs              // pub use 与 §5.1 的符号一一对应（约 76 个）
    width.rs            // visibleWidth, graphemeWidth, could_be_emoji, EAW 表, 宽度缓存
    segment.rs          // get_grapheme_segmenter, get_word_segmenter, is_word_like,
                        //   Segmenter 抽象（可注入，供测试）
    ansi.rs             // ansiCodeLength, extractAnsiCode, AnsiCodeTracker, Osc8 解析/格式化,
                        //   stripTerminalSequences, getActiveBackgroundAnsi（**不含** getOsc8LinkAtColumn）
    grapheme_cells.rs   // getGraphemeCellRange, getOsc8LinkAtColumn（唯一归属；§5.1.3 的 `grapheme.rs` 即此模块）
    normalize.rs        // normalizeTerminalOutput（Thai/Lao AM）
    tokens.rs           // splitIntoTokensWithAnsi, 分词类型
    wrap.rs             // wrapTextWithAnsi, wrapSingleLine, breakLongWord, applyBackgroundToLine
    truncate.rs         // truncateToWidth, truncateFragmentToWidth, finalizeTruncatedResult
    slice.rs            // sliceWithWidth, sliceByColumn, extractSegments
    classify.rs         // PUNCTUATION_REGEX, isWhitespaceChar, isPunctuationChar, cjkBreakRegex,
                        //   cjkPunctuationRegex, autocompleteSeparatorRegex/Boundary 的函数化实现
    fuzzy.rs            // scorePositions, fuzzyMatch, fuzzyFilter
    word_nav.rs         // findWordBackward, findWordForward, WordNavigationOptions
    kill_ring.rs        // KillRing
    undo_stack.rs       // UndoStack<S>
    editor_component.rs // EditorComponent trait
    colors.rs           // Color 枚举、构造器、parseColor、转换、mixColors、styleText
    oklab.rs            // oklab/okhsl 数学（与 colors.rs 分开，便于单独测试浮点）
    latex/
      mod.rs            // renderLatex, RenderLatexOptions, normalizeOutput
      symbols.rs        // SYMBOLS(224), NAMED_OPERATORS(32), RELATION_COMMANDS(88), …
      parser.rs         // LatexParser
      layout.rs         // LayoutNode, renderLayout, padLayoutLine, joinLayouts
      environments.rs   // parseEnvironment, renderCases, renderMatrix
    autocomplete.rs     // CombinedAutocompleteProvider + fd + readdir
    search.rs           // AltScreenSearchIndex, findAltScreenSearchMatches, AltScreenSearchComponent
  tests/
    fixtures/
      latex-cases.json  // 从 latex.test.ts 导出
      ansi-cases.json   // 从 wrap/truncate/slice 测试导出
```

**依赖建议**：

```toml
[dependencies]
unicode-width = { version = "0.2", optional = true }      # 备选 EAW
unicode-segmentation = "1"                                 # 备选分段
icu_segmenter = { version = "1", optional = true }          # 精确分段（feature = "icu"）
icu_collation = { version = "1", optional = true }          # localeCompare（feature = "icu"）
regex = "1"
tokio = { version = "1", features = ["process", "io-util", "sync", "macros", "rt"] }
tokio-util = "0.7"                                          # CancellationToken
dirs = "5"
libm = "0.2"                                                # fdlibm 位级一致
serde = { version = "1", features = ["derive"] }
serde_json = "1"                                            # latex fixture
```

`default = ["icu"]`；`no-icu` 用 `unicode-segmentation` + 手写 collation（仅供无 ICU 数据的环境）。

### 8.2 最难复刻的部分（按风险排序）

1. **Unicode 宽度表的精确复刻**。`get-east-asian-width@1.6.0` 是 Unicode 17.0 的 EAW 表，且 `graphemeWidth` 的 9 步算法依赖 `\p{RGI_Emoji}`、`\p{Spacing_Mark}` 的集合运算、`couldBeEmoji` 的启发式、`terminalSpacingMarkRegex` 的显式例外（U+1734/U+302E/U+302F 排除，U+065F/U+0F7F/U+102B/U+102C/U+1031/U+1033–U+1035/U+1038/U+103A–U+103E 加入）。**必须**逐码点移植 EAW 区间表（`/tmp/geaw_pkg/package/lookup-data.js` 有约 400 个区间），并用实测表（§2.1.2）逐项校验。`unicode-width` 的算法不同（它把 emoji 一律算 2、把某些组合算 0），**不可**直接替换。
2. **`Intl.Segmenter` 的 grapheme/word 分段与 ICU 一致**。涉及 RGI emoji 序列、国旗、Indic conjunct、`\r\n`、prepend、spacing mark、ZWJ、tag 序列。`unicode-segmentation` 与 ICU 在 Unicode 17 新增字符上可能不同；要位级一致需 ICU4X 的 `icu_segmenter`（数据版本仍可能与 ICU 78.3 有差）。**降级策略**：以测试覆盖的字符集为准，逐例校准。
3. **`localeCompare` 的 ICU root collation**。排序结果直接影响补全列表顺序（`getFileSuggestions`、`getFuzzyFileSuggestions` 的最终 tiebreak）。ICU4X 的 collation 数据版本需与 Node 26 的 ICU 78.3 比对；差异会导致目录列表顺序不同（用户可见）。
4. **浮点位级一致**。`colors.test.ts` 的 `deepStrictEqual` 断言 `rgbToOkhsl` 的浮点完全相等；`oklchToRgb` 的 20 步二分、`okhslToRgb` 的 Halley 迭代、`Math.cbrt`/`cos`/`sin`/`atan2`/`hypot`/`pow` 全部要位级一致。**必须**用 `libm` 且**不要**开 `-ffast-math`；`f64` 运算顺序不得重排（Rust 与 JS 都是 IEEE 754 double，但编译器不重排浮点）。
5. **JS 正则方言**。见 §6.3：`/v` 集合运算、lookbehind（`NAMED_OPERATOR_*_SPACING_PATTERN`）、`\p{Script_Extensions=…}`、`\s` 集合、`RGI_Emoji` 属性。其中 lookbehind 与 `RGI_Emoji` 必须手写。
6. **LaTeX 的二维布局与 PUA 标记**。`renderLayout` 的基线/居中/空白规则、`cases` 的偶数行居中大括号（PROTECTED_SPACE 填充）、矩阵定界符、`Math.min(...[]) === Infinity` 的怪异默认、`\frac{}{}` 在 display 模式输出 `"\n─"`（空分子/分母）。111 条 fixture 是唯一的正确性来源。
7. **`AnsiCodeTracker` 的跨行状态机**。SGR 逐属性状态、256/RGB 颜色捕获、行末只 reset 下划线、OSC 8 的 BEL/ST 终止符保存与逐行关闭/重开。任何一处差异都会在 `wrap-ansi.test.ts` 的 19 个用例上暴露。
8. **ANSI 识别的终止符规则**。CSI **只**在 `m/G/K/H/J` 时被识别（`\x1b[31;1H` 是完整 CSI，但 `\x1b[31;1x` 不是 → 按字符逐个当控制字符宽度 0 处理）。这条规则决定了 `visibleWidth` 对畸形输入的行为（`truncate-to-width.test.ts` 的 `handles malformed ANSI escape prefixes without hanging`）。
9. **UTF-16 vs UTF-8 索引**。所有光标/切片/偏移量、`item.value.length`、`textStart/textEnd`、`segment` 索引。见 §6.5 的策略 A。
10. **非重入的模块级 `pooledStyleTracker`**。`utils.ts:1319` 定义 `const pooledStyleTracker = new AnsiCodeTracker()`，**仅** `extractSegments` 使用（每次调用先 `clear()`，再边扫描边 `process`，最后用 `getActiveCodes()` 给 `after` 段加前缀）。`sliceWithWidth` **不**使用任何 tracker（只用局部 `pendingAnsi` 累积）；`wrapTextWithAnsi`/`breakLongWord`/`getActiveBackgroundAnsi` 各自 `new AnsiCodeTracker()`。
    风险：若 `extractSegments` 在自身扫描过程中被**重入**（当前所有调用点都不会），状态会被 `clear()` 打乱。Rust 侧两种选择：(a) 用局部 `AnsiCodeTracker`（语义等价，因为 `extractSegments` 每次先 clear；**推荐**，无行为差异）；(b) 为逐字对齐保留 `thread_local!` 单例。选 (a) 即可，但必须保留"**每次调用先 clear**"的等价语义（即不要跨调用复用残留状态）。
11. **`AbortSignal` + `kill(SIGKILL)` 的 fd 取消**。需要在 `spawn` 前同步检查、在 close 后忽略结果、`exitCode === null` 判定、SIGKILL（不是 SIGTERM）。
12. **`Math.round`/`Math.cbrt`/`Math.hypot`/`Math.min()` 的 JS 语义**。见 §6.2。
13. **`structuredClone` 的深拷贝与 `Object.freeze`**。低风险。
14. **搜索语料的列映射**。ASCII 快路径的 `linearColumns=true` 与字素路径的整簇映射；空白折叠与行间单空格；空行不产生额外空格。`tui-alt-screen.test.ts` 的 `maps normalized ASCII and Unicode search matches back to rendered columns` 是唯一校验。

### 8.3 实现顺序建议

1. `width.rs` + `classify.rs` + EAW 表（先让 `visible-width`/`tab-width`/`regression-regional-indicator-width` 全绿）。
2. `ansi.rs` + `truncate.rs` + `wrap.rs` + `slice.rs`（`wrap-ansi`/`truncate-to-width`/`regression-slice-by-column-ansi-order`/`regression-overlay-cjk-boundary`）。
3. `fuzzy.rs`（`fuzzy.test.ts` 17 个用例，全部是纯数值）。
4. `word_nav.rs` + `kill_ring.rs` + `undo_stack.rs`（`word-navigation.test.ts` 19 个 + editor/input 的 kill 段）。
5. `oklab.rs` + `colors.rs`（`colors.test.ts` 4 个，浮点位级）。
6. `latex/`（111 fixture，工作量最大）。
7. `autocomplete.rs`（`autocomplete-skill-slash.test.ts` 7 个 + fd 用例）。
8. `search.rs`（`tui-alt-screen.test.ts` 8 个搜索用例 + 渲染逐字）。

### 8.4 不建议的做法

- **不要**用 `unicode-width` 直接替换 `get-east-asian-width`（算法不同，`\t`=3、RI=2、Indic/Myanmar 例外都无法复现）。
- **不要**用 `char::is_whitespace` 替换 JS `\s`（`\u0085`/`﻿` 不同）。
- **不要**用 `f64::round` 替换 `Math.round`（负数半数不同）。
- **不要**用 `String` + `chars().count()` 替换 JS 的 `length`（emoji 2 vs 1）。
- **不要**用 `sort_by(|a,b| a.cmp(b))` 替换 `localeCompare`。
- **不要**用 `regex` 的 `\p{Emoji}` 替换 `\p{RGI_Emoji}`（前者是单码点属性，后者是序列集合）。
- **不要**把 `truncateToWidth` 的 `ellipsis` 默认值写成空串（默认 `"..."`）。
- **不要**在 `wrapTextWithAnsi` 的行末加 `\x1b[0m`（只加下划线 reset 与 OSC 8 close）。
- **不要**去掉 `AltScreenSearchComponent.render` 的三行结构（宽度 1 时是 `["┌","│","└"]`）。

---

## 附录 A：LaTeX 符号表（SYMBOLS，224 条）

完整表已内嵌在 **§2.9.4**（75 行 markdown 表格，每行 3 组 `命令 | 输出`，末行 1 组，合计 224 条；`vert`/`lvert`/`rvert` 的值 `|` 在 markdown 中转义为 `\|`，实际字符是单个 U+007C）。Rust 侧应生成：

```rust
pub static SYMBOLS: &[(&str, &str)] = &[("alpha", "α"), /* … 224 条 … */];
```

并加 `#[test] fn symbol_count_is_224() { assert_eq!(SYMBOLS.len(), 224); }` 与"无重复命令名"断言。

**生成与校验脚本**（探针，非交付物）：`/tmp/tui-ref/genLatexTables.mjs`（从 `src/latex.ts` 的 `SYMBOLS` 字面量生成 markdown 表）。
校验方法：在 Rust 侧遍历 `SYMBOLS` 构造 `\<name>` 输入并断言 `renderLatex` 输出等于该字面值（对 224 条全覆盖）。

## 附录 B：实测数值速查（供 Rust 单元测试直接使用）

### B.1 `visibleWidth`

| 输入 | 宽度 |
| --- | --- |
| `""` | 0 |
| `"hello"` | 5 |
| `"\t"` | 3 |
| `"界"` | 2 |
| `"🇨"` | 2 |
| `"🇨🇳"` | 2 |
| `"र्क"`（U+0930 U+094D U+0915，见 §2.1.2） | 2 |
| `"ကို"` | 1 |
| `"ำ"` | 1 |
| `"กำ"` | 2 |
| `"\r\n"` | 0 |
| `"\x7f"` | 0 |
| `"\x1b[31mHello\x1b[0m"` | 5 |
| `"\x1b]133;A\x07x"` | 1 |
| `"\x1b]8;;https://x\x07link\x1b]8;;\x07"` | 4 |
| `"e\u0301"`（=`e` + U+0301 组合重音） | 1 |
| `"\u200B"`（ZWSP） | 0 |
| `"\uFEFF"`（BOM） | 0 |
| `"\x1b_kiana:c\x07"`（CURSOR_MARKER） | 0 |
| `"\x1b_kiana:/fc\x07"`（FAKE_CURSOR_END） | 0 |
| `"\x1b_kiana:fc\x07"` | 0 |
| `"\x1b[31m─→\x1b[39m"` | 2 |
| `"\x1b[31m日本\x1b[39m ok"` | 7 |
| `"\x1b[1ma\tb\x1b[22m"` | 5 |
| `"\x1b]133;A\x1b\\prompt"` | 6 |
| `"\x1b_kiana:c\x07cursor"` | 6 |
| `"\x1b[31"`（未终止 CSI） | 3 |
| `"a\x1b"` | 1 |

### B.2 `truncateToWidth`

| 输入 | 结果 |
| --- | --- |
| `("Hello World", 8)` | `"Hello\x1b[0m...\x1b[0m"` |
| `("Hello World", 8, "")` | `"Hello Wo\x1b[0m"` |
| `("Hello", 5)` | `"Hello"` |
| `("Hello", 3)` | `"\x1b[0m...\x1b[0m"` |
| `("Hello", 0)` | `""` |
| `("Hello", -1)` | `""` |
| `("Hello", 8, "...", true)` | `"Hello   "` |
| `("\x1b[31mHello World\x1b[0m", 8)` | `"\x1b[31mHello\x1b[0m...\x1b[0m"` |

（完整 ~28 条见 §2.1.9。）

### B.3 `fuzzyMatch` 分数（部分）

| query | text | `matches` | `score` |
| --- | --- | --- | --- |
| `"test"` | `"test"` | true | `-159.4` |
| `"ABC"` | `"abc"` | true | `-139.7` |
| `"app"` | `"app"` | true | `-139.7` |
| `"cl"` | `"cl"` | true | `-124.9` |
| `"a"` | `"A"` | true | `-115` |
| `"opus"` | `"claude-opus-4.1"` | true | `-36.6` |
| `"opus"` | `"xxopus opus"` | true | `-36.6` |
| `"app"` | `"a_p_p"` | true | `-30.400000000000002` |
| `"cl"` | `"clone"` | true | `-24.9` |
| `"aa"` | `"aaa"` | true | `-24.9` |
| `"opus"` | `"openrouter openrouter/anthropic/claude-opus-4.1"` | true | `-23.8` |
| `"fb"` | `"foo bar"` | true | `-18.6` |
| `"x"` | `"xyz"` | true | `-15` |
| `"mdl"` | `"model"` | true | `-10.4` |
| `"fb"` | `"foobar"` | true | `-10.7` |
| `"abc"` | `"aXbXc"` | true | `-10.4` |
| `"fb"` | `"afbx"` | true | `-4.7` |
| `""` | 任意 | true | `0` |
| `"abc"` | `"ab"` | false | `0` |
| `"xyz"` | `"x"` | false | `0` |
| `"abc"` | `"cba"` | false | `0` |

（完整 23 对表见 §2.2；上表是其中可在 Rust 单测里直接断言的子集，全部经 Node 26.12.1 实跑确认。）

### B.4 颜色

| 表达式 | 结果 |
| --- | --- |
| `parseColor("#abc")` | `{kind:"rgb", r:170, g:187, b:204}` |
| `parseColor(5)` | `{kind:"indexed", index:5}` |
| `indexedToRgb(16)` | `{r:0,g:0,b:0}` |
| `indexedToRgb(196)` | `{r:255,g:0,b:0}` |
| `indexedToRgb(232)` | `{r:8,g:8,b:8}` |
| `indexedToRgb(255)` | `{r:238,g:238,b:238}` |
| `colorToHex({kind:"rgb",r:255,g:0,b:128})` | `"#ff0080"` |
| `colorToHex(indexedColor(16))` | `"#000000"` |
| `colorToHex(indexedColor(196))` | `"#ff0000"` |
| `colorToHex(indexedColor(232))` | `"#080808"` |
| `colorToHex(indexedColor(255))` | `"#eeeeee"` |
| `oklchColor(1, 0.3, 150)` 的 RGB | `{r:255,g:255,b:255}`（gamut 映射到白） |
| `foregroundAnsi(parseColor("#ff0000"), "256color")` | `"\x1b[38;5;196m"` |
| `backgroundAnsi(parseColor("#ff0000"), "truecolor")` | `"\x1b[48;2;255;0;0m"` |
| `colorToOklch(parseColor("#ff0000"))` | `{l:0.6279553639214309, c:0.2576833038053606, h:29.23388027962784}` |
| `colorToOkhsl(parseColor("#ff0000"))` | `{h:29.23388027962784, s:1, l:0.5680846563197032}` |
| `colorToHex(mixColors("#ff0000", "#0000ff", 0.5))` | `"#b200b8"`（oklch 空间混合，非 `#800080`） |
| `okhslToRgb(0,0,0)` / `(0,0,1)` | `{0,0,0}` / `{255,255,255}` |
| `okhslToRgb(0,1,0.5)` | `{r:215,g:0,b:113}` |
| `okhslToRgb(120,1,0.5)` | `{r:109,g:130,b:0}` |
| `okhslToRgb(240,1,0.5)` | `{r:0,g:127,b:187}` |
| `okhslToRgb(-120,0.5,0.5)` | `{r:73,g:125,b:161}`（负角度回绕） |
| `okhslToRgb(480,0.5,0.5)` | `{r:113,g:126,b:71}`（>360 回绕） |
| `okhslToRgb(NaN,0.5,0.5)` | `{r:NaN,g:NaN,b:NaN}`（JS 下 `JSON.stringify` → `null`；Rust 必须传播 NaN，**不要** panic） |
| `rgbToOkhsl({255,255,255})` | `{h:0, s:0, l:0.9999999999999996}`（**不是** 1.0，浮点误差必须保留） |
| `rgbToOkhsl({0,0,0})` | `{h:0, s:0, l:0}` |
| `rgbToOkhsl({255,0,0})` | `{h:29.23388027962784, s:1, l:0.5680846563197032}` |
| `oklabToOkhslLightness(0)` / `(0.5)` / `(1)` | `0` / `0.42114056260896976` / `1` |

（17 条量化样本见 §2.7。）

### B.5 搜索

| 表达式 | 结果 |
| --- | --- |
| `buildSearchCorpus(["alpha QUICK","brown fox"]).text` | `"alpha QUICK brown fox"` |
| `findAltScreenSearchMatches(["alpha QUICK","brown fox"], "quick brown")` | `[{segments:[{row:0,startCol:6,endCol:11},{row:1,startCol:0,endCol:5}]}]` |
| `findAltScreenSearchMatches(["alpha QUICK","brown fox"], "quick")` | `[{segments:[{row:0,startCol:6,endCol:11}]}]` |
| `findAltScreenSearchMatches(["abc","def"], "abcdef")` | `[]` |
| `findAltScreenSearchMatches(["abc","def"], "abc def")` | `[{segments:[{row:0,startCol:0,endCol:3},{row:1,startCol:0,endCol:3}]}]` |
| `findAltScreenSearchMatches(["a","","b"], "a b")` | `[{segments:[{row:0,startCol:0,endCol:1},{row:2,startCol:0,endCol:1}]}]`（空行不产生额外空格） |
| `findAltScreenSearchMatches(["   abc"], "abc")` | `[{segments:[{row:0,startCol:3,endCol:6}]}]`（行首空白丢弃但列号推进） |
| `findAltScreenSearchMatches(["\tX"], "X")` | `[{segments:[{row:0,startCol:3,endCol:4}]}]`（TAB 宽 3） |
| `findAltScreenSearchMatches(["abab"], "ab")` | 两个匹配：`0..2` 与 `2..4`（`g` 不重叠推进） |
| `findAltScreenSearchMatches(["AbC"], "abc")` | `[{segments:[{row:0,startCol:0,endCol:3}]}]`（`i` 忽略大小写） |
| `findAltScreenSearchMatches(["a.c"], "a.c")` | `[{segments:[{row:0,startCol:0,endCol:3}]}]`（query 被转义为字面量） |
| `findAltScreenSearchMatches(["abc"], "a.c")` | `[]`（`.` 不是通配符） |
| `getAltScreenSearchMatchKey({segments:[{row:0,startCol:0,endCol:5},{row:1,startCol:0,endCol:5}]})` | `"0:0:1:5"` |

### B.6 搜索面板渲染（去 ANSI，空 query）

| 宽度 | 第 1 行 / 第 2 行 / 第 3 行 |
| --- | --- |
| 1 | `┌` / `│` / `└` |
| 2 | `┌┐` / `││` / `└┘` |
| 3 | `┌─┐` / `│ │` / `└─┘` |
| 4 | `┌──┐` / `│ F│` / `└──┘` |
| 5 | `┌───┐` / `│ Fi│` / `└───┘` |
| 6 | `┌────┐` / `│ Fin│` / `└────┘` |
| 10 | `┌────────┐` / `│ Find in│` / `└── ↑ ↓ ─┘` |
| 12 | `┌──────────┐` / `│ Find in t│` / `└──── ↑ ↓ ─┘` |
| 20 | `┌──────────────────┐` / `│ Find in transcrip│` / `└──────────── ↑ ↓ ─┘` |
| 32 | `┌──────────────────────────────┐` / `│ Find in transcript           │` / `└──── ↑ Shift+Enter · ↓ Enter ─┘` |
| 40 | `…` / `│ Find in transcript                   │` / `└──────────── ↑ Shift+Enter · ↓ Enter ─┘`；按钮区间 14..27、30..37 |
| 48 | `└──────────────────── ↑ Shift+Enter · ↓ Enter ─┘` |
| 60 | `└──────────────────────────────── ↑ Shift+Enter · ↓ Enter ─┘` |
| 80 | `└──────────────────────────────────────────────────── ↑ Shift+Enter · ↓ Enter ─┘` |

（完整含 ANSI 的逐字输出见 §2.11.6；所有行的 `visibleWidth` 恒等于请求宽度。）

### B.7 LaTeX（部分，完整见 §2.9 与 fixture）

| 输入 | 输出 |
| --- | --- |
| `\sin\theta` | `"sin θ"` |
| `-\sin\theta` | `"-sin θ"` |
| `i\sin\theta` | `"i sin θ"` |
| `\frac{}{}`（inline） | `"()/()"` |
| `\frac{}{}`（display） | `"\n─"` |
| `\frac{1}{2}`（inline） | `"1/2"` |
| `\frac{1}{2}`（display） | `"1\n─\n2"` |
| `\sqrt{2}` | `"√2"` |
| `\alpha` | `"α"` |
| `\mathbb{R}` | `"ℝ"` |
| `\notin` | `"∉"` |
| `x^2` | `"x²"` |

---

## 附录 C：本规范与相邻规范的边界裁决

| 主题 | 归属 | 裁决 |
| --- | --- | --- |
| `visibleWidth`/`truncateToWidth`/`wrapTextWithAnsi`/`sliceByColumn`/`extractSegments` | 本规范 §2.1 | 实现以本规范为准；tui-core §2.x 有交叉描述时以本规范的精读为准（union，不冲突） |
| `extractAnsiCode`/`AnsiCodeTracker` | 本规范 §2.1.11 | tui-core 引用其行为；实现归本单元 |
| `normalizeTerminalOutput` | 本规范 §2.1.8 | 调用点在 tui-core（terminal 输出前），实现归本单元 |
| `KillRing`/`UndoStack`/`findWord*` | 本规范 §2.3–2.5 | editor/input（tui-components）消费；实现归本单元 |
| `EditorComponent` 接口 | 本规范 §2.6 | 接口定义归本单元；`Editor` 实现归 tui-components |
| `colors.ts`/`oklab.ts` | 本规范 §2.7–2.8 | 完整归属本单元 |
| `latex.ts` | 本规范 §2.9 | 完整归属本单元 |
| `autocomplete.ts` | 本规范 §2.10 | 完整归属本单元 |
| `alt-screen-search.ts` | 本规范 §2.11 | 组件与索引归本单元；集成（`toggleSearch`/`refreshSearch`/`applySearchHighlights`/overlay 几何）归 tui-core |
| `SearchSourceSpan`/`buildSearchCorpus` | 本规范 §2.11.2 | 私有但**必须**逐字复刻（列映射正确性依赖） |


---

## Audit additions（对抗性完整性审计的补充与更正）

本节汇总独立重读全部 11 个源文件与 14 个测试文件后发现的**遗漏项**（§A.1–§A.12）与**已就地修正的错误**（§A.0 索引，共 **51** 条）。凡与正文冲突，以本节为准。

> **编号说明**：本节的 `A.x` 是 **Audit 小节**，与前面「附录 A：LaTeX 符号表」不是一回事（附录 A 无子编号）。

### A.0 已就地修正的错误（索引）

| # | 位置 | 原文（错） | 更正 |
| --- | --- | --- | --- |
| 1 | §0.1 / §7.10 | latex 测试 "20 个显式 it + 100 个 defineCases + 1 个 describe 汇总"；fixture "共 93 条" | **19** 个显式 `it` + **92** 个 `defineCases`（37/8/5/25/17）。第 20 个 `it(` 是 `defineCases` 内的模板 |
| 2 | §7 导语 | "共 13 个"测试文件 | **14** 个（与 §0.1 一致） |
| 3 | §2.9.9 | `renderLatex("x\\\\")` = `undefined` | 源码 `x\`（单反斜杠）→ `undefined`；源码 `x\\`（双反斜杠，换行命令）→ `"x"` |
| 4 | §2.1.1 / §6.3 | `cjkPunctuationRegex` 是"后跟 CJK 字母的标点" | 前瞻与 CJK 字符类作用于**同一位置**：该字符既是 `\p{P}` 又是 CJK 脚本；否则靠显式列表 |
| 5 | §2.10.2 | `tokenStartRegex.test(" x") = true` | **false**（末尾是 `x`，不是分隔符） |
| 6 | §6.2 / §6.4 / §2.10.7 | `localeCompare` 用 "ICU root collation / `und`" | 用**运行时默认 locale**；`LANG` 为空 → ICU 78 回落 `en-US`；`LANG=zh_CN.UTF-8` → `zh-CN`。CJK 与拉丁的相对次序随 locale 改变 |
| 7 | §1.2 / §5.1.2 | 消费者表多处归错（editor.ts 声称用 `truncateToWidth` 等；layout.ts 声称用 `sliceWithWidth`/`extractSegments`；`flattenLines`→tui-alt-screen 等） | 按真实 `import` 语句重写（见正文表格） |
| 8 | §8.1 | `getOsc8LinkAtColumn` 同时列在 `ansi.rs` 与 `grapheme_cells.rs` | 只归属 `grapheme_cells.rs`（§5.1.3 的 `grapheme.rs`） |
| 9 | §5.1.1 标题 | "(6 个 utils 符号 + 全部 colors/fuzzy/latex/autocomplete 符号)" | 漏了 `EditorComponent`（type re-export）与 `oklabToOkhslLightness`，已补 |
| 10 | §7.14 | 搜索测试"行 597–830" | 行 **597–842**（下一个 `it` 从 843 起） |
| 11 | §7.16 | `tui-alt-screen.test.ts` 导入 `../src/index.ts` | **不导入**；它直接导入各模块，失败原因是缺 `@xterm/headless`（`virtual-terminal.ts`）。只有 `latex.test.ts`/`colors.test.ts` 导入 `index.ts` |
| 12 | §7.16 | `tab-width.test.ts` "1 个测试" | 文件有 **4 个 `it`**，因导入阶段失败而全部未运行（node --test 报 1 个文件级失败） |
| 13 | §7.15 | markdown/settings-list/layout/tui-overlay-style-leak/isimageline 的"直接覆盖" | 它们**不直接**导入本单元符号（除 `select-list`/`layout`/`tui-render` 的个别符号） |
| 14 | §2.11.6 | `render(20)` 段落含自问自答 "inputPadding = 18-17 = 1？" | 已重写为完整推导（见正文） |
| 15 | §5.3 | "见 §2.11.7 的伪代码"（该小节不存在） | 伪代码已内联到 §5.3 |
| 16 | §3.1 | "见 §2.7.2"（无编号小节） | 改为"见本节末尾的 ECMAScript Number::toString 段" |
| 17 | §2.1.1 | "（见 §2.3）" 指 `tokenStartRegex` | 应为 **§2.10.2** |
| 18 | §6.3 | JS `\s` "15 个码点" | **25 个**（完整表见 §A.2） |
| 19 | §2.11.6 | 实测渲染列表重复了 render(1..3) 两遍 | 已删除重复行 |
| 20 | §6.2 | `toLowerCase` 行称 `Σ` 一律 σ（"不区分词位"） | 错：JS `"ΑΣ".toLowerCase()` = `"ας"`（U+03C2 终位 sigma），**是上下文相关**的；`str::to_lowercase` 同样上下文相关 |
| 21 | §2.9.7 `parseSequence` 表 | 未提及 `parseScripts` 的完整算法（只有一句引用） | 见 §A.1 |
| 22 | §2.11.6 / §5.3 | `selectionMode` 被描述为"五态循环" | 实为**四**态：`"query" \| "retain" \| "next" \| "previous"`（TS 侧是模块私有 type alias，不导出） |
| 23 | §2.9.5 数字格式化引用 | "见 §6.3"（§6.3 是正则表，不含数字格式化） | 改为 §3.1 末尾的 Number::toString 段 |
| 24 | §5.1.2 类型备注 | 把 `GraphemeCellRange` 当作可导入的导出类型 | 它是 `utils.ts` 的**私有 interface**（只在函数签名中出现），不可 `import type` |
| 25 | §3.2.6 | 边框字符表只列 5 个字符但没说"没有 T 形件"，且未给码点 | 已补码点与"无 ┤├┬┴"的说明 |
| 26 | §1.4 | "`getKeybindings()` 组件构造时读取一次（每次 `render` 重新读取…）" 自相矛盾 | 只在 `render` 内读取；构造函数不读键位 |
| 27 | §7.16 备注 | "`colors.test.ts` 通过 `../src/colors.ts` 直接导入" | 实际导入 `../src/index.ts`（正文已更正） |
| 28 | §2.3 实测表 | `findWordBackward("你好世界 test", 7) = 5` | 源码用 `text.length`（= **9**，不是 7）作光标；实测 `findWordBackward(text, 9) = 5`（顺带：`findWordBackward(text, 7) = 5` 也成立，但 7 不是测试里出现的值） |
| 29 | §2.3 实测表 | `findWordForward("你好世界 test", 0) ∈ (0,4]，逐段走到 7` | 逐段走到 **9**（= `text.length`）；实测 `findWordForward(text, 0) = 2`，`2→5→9` |
| 30 | §2.3 原子段实测 | `findWordForward(text, 6, opts) = 6 + 22  // marker.length = 22` | **`= 25`**：`"[paste #1 +5 lines]"` 的 `length` 是 **19**（6 + 19 = 25），22 是把 `"hello "` 前缀也算进去的错误推算 |
| 31 | §7.12 概括 | `dot-slash path completion` 9 个、`quoted path completion` 4 个（正确），但其余 7 个用例描述只写"多个 fd 用例/建议项/排序" | 已补 24 + 9 + 4 条**逐字用例名**，并点明 `--hidden`/`--follow`/symlink/`.git` 排除是行为契约 |
| 32 | §7.10 | 单点测试列表把 `renders composes matrices…` 复述为"矩阵与分式组合" | 已补 19 条**逐字** `it` 名（含源码原文的 `renders composes`） |
| 33 | §7.14 | 8 条断言全是概括（"跨行归一化搜索""列映射"） | 已补每条用例的逐字输入/期望（含 `\x07` = Ctrl+G 跳上一个、`\x1b[103;6u` = Ctrl+Shift+G、SGR 鼠标码、`isFollowingEnd===false`、`scrollTop===0`） |
| 34 | §7.5 / §7.6 / §7.7 | 多条断言只写"U+1F1E6–U+1F1FF 全部 = 2""ASCII 边界""ANSI 顺序" | 已补逐字样本数组与 `sliceByColumn` 精确返回串（见正文 §7.5–§7.7） |
| 35 | §7.4 | `tab-width.test.ts` 4 条断言只写"一致（含 `\t`）" | 已补 `sliceWithWidth`/`extractSegments` 的精确 `{text,width}`/`{before,beforeWidth,after,afterWidth}` 与三个 OSC 序列字面量、虚拟终端视口数组 |
| 36 | §3.2.5 Thai/Lao AM 说明 | 称改写为 "Nikahit + **空格**" 等价序列 | 错：目标是**码点分解** `U+0E33 → U+0E4D NIKHAHIT + U+0E32 SARA AA`、`U+0EB3 → U+0ECD NIGGAHITA + U+0EB2 SARA AA`，与空格无关（脚本体视觉上像空格） |
| 37 | §7.3 `normalizes Thai and Lao AM vowels only for terminal output` | 只写"`normalizeTerminalOutput` 的替换" | 已补逐字断言与码点，以及"重排不改变 `visibleWidth`"的两条恒等断言 |
| 38 | §7.2 | 19 条 wrap-ansi 断言几乎全是"标题 + 一句概括"（如"精确数组""BEL 而非 ST""`trimEnd`"） | 已补每条的实际输入串、宽度与断言形态（`deepStrictEqual` 数组 vs 属性断言），并标明 OSC 8 用例用的是 **ST** 还是 **BEL** 终止符 |
| 39 | §7.2 underline/background 五条 | 全部只写一句概括 | 已补输入串、宽度、以及"哪些行必须 `endsWith` 什么"的循环条件（`i ∈ [1, len-1)` 等） |
| 40 | §7.8 | 4 条 colors 断言是概括（"正反例""边界""`deepStrictEqual`""逐字"） | 已补 15 个精确值（`#abc`→`{170,187,204}`、红 cusp 的三组、`okhsl(29.23 100% 56.8%)`→纯红、三个 hex 往返、`styleText` 两条完整输出） |
| 41 | §2.11.6 按钮几何表 | 缺 15、17、18、19、21、23、25、27、29、30 这些档位 | 已按实测补齐（`previousStart/End/nextStart/End` 每个宽度都有值），并标注 27 是最后一档退化标签 |
| 42 | §3.2.5 之后的「2.1.8 实测」 | 原表未给 `normalizeTerminalOutput` 输出的码点分解 | 已在 §2.1.8 补 `U+0E33 → U+0E4D U+0E32` / `U+0EB3 → U+0ECD U+0EB2`（见第 36 条） |
| 43 | §7.9 | 只用一行罗列"空 query、query 过长、精确匹配…" | 已补 17 条**逐字用例名** + 每条的关键断言（含 `score < 0` vs 精确相等的区别、#11220 的三个模型、`deepStrictEqual` 的两处） |
| 44 | §7.11 | 只写"见 §2.3" | 已补 3 个 `describe` 的分组计数（backward 8 / forward 8 / atomic 3）与注入式 `segment` 的必要性 |
| 45 | §7.13 | 用一行概括 7 条用例 | 已补逐字用例名 + 每条断言 + fixture 命令表；并记录 `/skill` 用例对**顺序**做 `deepStrictEqual`（`skill:` 折叠两段拼接后恰为命令表顺序） |
| 46 | §5.3 `refreshSearch` 行号 | "第 607–676 行" | 实为 **607–674**（675 空行、676 是下一个方法的注释） |
| 47 | §5.3 `applySearchHighlights` | 只列裁剪公式，未给**内容坐标 → 屏幕坐标**换算（`row = rect.y + segment.row - scrollTop`；`startCol = max(minColumn, rect.x + segment.startCol)`） | 已补完整换算 + 二进制搜索键的推导 + 二次列夹取（`min(range.startCol, lineWidth)`） |
| 48 | §5.3 `applySearchTextHighlight` | 一句话概括 | 已补逐字算法与"连续 ANSI 之间不插空包裹""跨 ANSI 的文本被拆成多段分别重开"两条可观察行为 |
| 49 | §5.1.2 `visibleWidth` 行 | "14 个消费方" | **15** 个（§1.2 表中 `visibleWidth` 实际出现在 15 行；`coding-agent/interactive-mode.ts` 用 `fuzzyFilter` 不用 `visibleWidth`）。已按源码逐个 grep 确认 15 个文件都真的引用它 |
| 50 | §5.1.2 表格结构 | `GraphemeCellRange` 的 blockquote 说明被插在表格**中间**，把表拆成两段 | 已把 blockquote 移到表格结束后（表格恢复为 22 行连续渲染） |
| 51 | §A.10 `triggerCharacters` | 只说"与默认合并" | 已补合并的四条**跳过条件**（UTF-16 长度 ≠ 1、`"/"`、`isWhitespaceChar`、重复）与两个 pattern 的逐字形态；并实测确认 `[]` 空字符类永不匹配、`@` 分支允许引号内含分隔符 |

### A.1 `LatexParser.parseScripts(initialMarker)` —— 原先**完全缺失**的私有方法

源码位置：`latex.ts` 第 949–1005 行。签名 `private parseScripts(initialMarker: "^" | "_"): string`。它由 `parseSequence` 在遇到 `^`/`_` 时调用（`position` 已越过该标记）。

逐字算法：

```ts
const scripts: { sub?: string; sup?: string } = {};
const order: Array<"sub" | "sup"> = [];

const parse = (marker: "^" | "_"): void => {
  const kind = marker === "_" ? "sub" : "sup";
  this.scriptDepth++;
  try { scripts[kind] = this.parseRequiredArgument(false); }   // ← stackFractions = false
  finally { this.scriptDepth--; }
  order.push(kind);
};

parse(initialMarker);

// 允许 `x^2_3` / `x_2^3`：跳过空白后若下一个标记是"另一个"标记，则连续解析
let nextPosition = this.position;
while (nextPosition < this.source.length && /\s/.test(this.source[nextPosition] ?? "")) nextPosition++;
const nextMarker = this.source[nextPosition];
if ((nextMarker === "^" || nextMarker === "_") && nextMarker !== initialMarker) {
  this.position = nextPosition + 1;
  parse(nextMarker);
}

const subUnicode = scripts.sub === undefined ? undefined : formatUnicodeScript(scripts.sub, "sub");
const supUnicode = scripts.sup === undefined ? undefined : formatUnicodeScript(scripts.sup, "sup");

const canUseLayout = ![scripts.sub, scripts.sup].some(
  (value) =>
    value !== undefined &&
    (value.includes("/") ||
      (!value.includes(LAYOUT_MARKER_START) && Array.from(value).length > 1 && !/[A-Z*∗]/.test(value))),
);

const needsLayout =
  this.display &&
  canUseLayout &&
  (this.scriptDepth > 0 ||                                    // 嵌套 script 强制布局
    (scripts.sub !== undefined && subUnicode === undefined) || // 有下标但无 Unicode 映射
    (scripts.sup !== undefined && supUnicode === undefined));  // 有上标但无 Unicode 映射

if (!needsLayout) {
  return order
    .map((kind) =>
      kind === "sub"
        ? (subUnicode ?? formatScript(scripts.sub ?? "", kind))
        : (supUnicode ?? formatScript(scripts.sup ?? "", kind)),
    )
    .join("");
}

const index = this.layoutNodes.push({
  type: "script",
  lower: scripts.sub === undefined ? undefined : normalizeOutput(scripts.sub),
  upper: scripts.sup === undefined ? undefined : normalizeOutput(scripts.sup),
}) - 1;
return `${LAYOUT_MARKER_START}${index}${LAYOUT_MARKER_END}`;
```

要点（**每一条都是可观察行为**）：

1. **`parseRequiredArgument(false)`**：脚本参数内的分数**绝不堆叠**（`stackFractions=false` 下传给嵌套 parser 的 `display` 为 false）。因此 `x^{\frac{1}{2}}` 在 display 模式下仍是 `x^(1/2)`。
2. **`scriptDepth` 在解析参数**期间**递增**，在 `finally` 中递减；用于判断"脚本内部还有脚本"（`x^{a^b}`）时**强制**走二维布局。
3. **第二个标记的接受条件**：必须与 `initialMarker` **不同**（`x^2^3` 的第二个 `^` **不**被吸收，交给外层 `parseSequence` 再调用一次 `parseScripts`）。空白可跳过，但只有在跳过空白后**紧邻**标记才吸收（`x^2 _3` 会合并，`x^2 a_3` 不会）。
4. **`order`** 保留**出现顺序**（`x^2_3` → `["sup","sub"]`），因此线性渲染是 `"x²₃"` 而非 `"x₂³"`。
5. **`canUseLayout` 的排除条件**（任一脚本值命中即**不**布局）：
   - 含 `/`（分式线性形式）；
   - 含 `LAYOUT_MARKER_START`（内部已有布局节点）时**不**排除；
   - 否则若"字符数 > 1"且**不是**全大写/星号集合（`/[A-Z*∗]/` 未命中）→ 排除。注意该正则**未锚定**：只要**任一**字符是 `A-Z`/`*`/`∗` 即视为"可布局"。
6. **`needsLayout`**：`display && canUseLayout && (scriptDepth > 0 || sub 无 Unicode 映射 || sup 无 Unicode 映射)`。因此**内联模式永不布局**；display 模式下只有"Unicode 无法表达"的脚本才纵向排列。
7. **线性渲染**优先用 `formatUnicodeScript`（即 `SUBSCRIPTS`/`SUPERSCRIPTS` 全字符映射），失败才回落到 `formatScript`。
8. **布局节点**的 `lower`/`upper` 都经 `normalizeOutput`（去空白、清 PUA）。

**实测（Node 26.12.1，`renderLatex` 端到端）**：

| 源码 | 选项 | 输出 |
| --- | --- | --- |
| `x^2_3` | `{}` | `"x²₃"` |
| `x_2^3` | `{}` | `"x₂³"` |
| `x^2^3` | `{}` / `{display:true}` | `"x²³"`（第二个 `^` 由外层再次调用 `parseScripts`） |
| `x^{abc}` | `{}` | `"xᵃᵇᶜ"` |
| `x^{ABC}` | `{}` | `"x^(ABC)"` |
| `x^{ab}` | `{display:true}` | `"xᵃᵇ"`（`ab` 全小写、有 Unicode 映射 → 不布局） |
| `x_{ab}` | `{display:true}` | `"x_ab"`（`canUseLayout=false`：多字符且非 `[A-Z*∗]` → 强制线性；见下方易错点） |
| `x^{a b}` | `{display:true}` | `"x^(a b)"` |
| `x^{1/2}` / `x^{\frac{1}{2}}` | `{display:true}` | `"x^(1/2)"`（含 `/` → `canUseLayout=false`） |
| `x^{A}` | `{display:true}` | `" A\nx"`（`A` 命中 `[A-Z*∗]` → 可布局，且无 Unicode 映射 → 纵向） |
| `x^{*}` / `x^{**}` | `{display:true}` | `" *\nx"` / `" **\nx"` |
| `x^{é}` | `{display:true}` | `" é\nx"`；`{}` → `"x^é"` |
| `x^{a}_{b}` | `{display:true}` | `" a\nx\n b"`（上标行 + 基线空行 + 下标行） |
| `x^{i}` / `x_{i}` | `{display:true}` | `"xⁱ"` / `"xᵢ"`（单字符有 Unicode 映射） |
| `x^{10}` | `{display:true}` | `"x¹⁰"` |
| `x^{2+3}` | `{}` | `"x²⁺³"` |
| `x^{a}_ {b}` | `{}` | `"xᵃ_b"`（`_` 前的空格被跳过并吸收第二个标记） |
| `x^{}` | `{}` | `"x"`（空脚本 → 空串） |
| `x^a_b_c` | `{}` | `"xᵃ_b_c"`（只吸收一个"另一个"标记） |

> **`canUseLayout` 易错点（实测校正）**：`some(...)` 里的条件是 `value.includes("/") || (!value.includes(LAYOUT_MARKER_START) && Array.from(value).length > 1 && !/[A-Z*∗]/.test(value))`。对 `"ab"`：长度 2 > 1、不含 `/`、`/[A-Z*∗]/.test("ab")` = false → 整个合取为 **true** → `some` 为 true → **`canUseLayout = false`**。即"多字符、非全大写/星号、不含分式"的脚本值会**禁止**二维布局。因此 `x_{ab}`/`x_{abc}`/`x_{abcd}` 在 display 下都输出线性 `"x_ab"`/`"x_abc"`/`"x_abcd"`（实测），而 `x^{AB}`/`x^{*}`/`x^{é}`（单字符或含 `A-Z*∗`）才走布局。**Rust 侧必须逐字复刻这个反直觉的判定，不要"修正"它**。

### A.2 JS `\s` 的 25 个码点（穷举实测）

```
0009 000A 000B 000C 000D 0020 00A0 1680
2000 2001 2002 2003 2004 2005 2006 2007 2008 2009 200A
2028 2029 202F 205F 3000 FEFF
```

不在集合内：`0085`、`180E`、`200B`、`200C`、`200D`、`2060`、`FE0F`。

### A.3 `isPrintableAscii(str)`（正文引用但**未定义**）

源码 `utils.ts` 第 65–71 行（模块私有）：

```ts
function isPrintableAscii(str: string): boolean {
  for (let i = 0; i < str.length; i++) {
    const code = str.charCodeAt(i);
    if (code < 0x20 || code > 0x7e) return false;
  }
  return true;
}
```

- 按 **UTF-16 码元**判定；空串返回 **true**（循环不执行）。
- **不含 TAB**（0x09 < 0x20）→ 含 TAB 的串走通用路径，这与 §2.1.9 的 `hasTabs` 分支一致。
- 被 `truncateToWidth`（早退快路径）、`truncateFragmentToWidth`（省略号裁剪快路径）使用；`alt-screen-search.ts` 的 `PRINTABLE_ASCII = /^[\x20-\x7e]*$/` 与此**等价**（含空串为 true）。
- Rust：`s.is_ascii() && s.bytes().all(|b| (0x20..=0x7e).contains(&b))`（空串 true）。

### A.4 `localeCompare` 的 locale 依赖（实测矩阵）

| locale（来源） | 排序（13 个样本） |
| --- | --- |
| `und`（`LANG` 为空；ICU 78 实际用 `en-US`） | `-dash.txt (g).ts [id].tsx ~tilde.txt a.txt my folder/ README.md scope/ src/ UPPER.TXT z folder/ 文档/ 说明.md` |
| `en-US`（`LANG=C`） | 同上（`符号 < 拉丁 < CJK`） |
| `zh-CN`（`LANG=zh_CN.UTF-8`） | `-dash.txt (g).ts [id].tsx ~tilde.txt 说明.md 文档/ a.txt my folder/ README.md scope/ src/ UPPER.TXT z folder/`（`符号 < CJK < 拉丁`） |
| `en-US-u-va-posix`（显式 DUCET） | `(g).ts -dash.txt README.md UPPER.TXT [id].tsx a.txt my folder/ scope/ src/ z folder/ ~tilde.txt 文档/ 说明.md` |

**对本单元的影响**：

- `getFileSuggestions`（`a.label.localeCompare(b.label)`，目录优先之后）与 `getFuzzyFileSuggestions`（score→depth→length 之后的最后 tiebreak）的输出顺序**随运行环境 locale 变化**——这是**上游的真实行为**，不是本规范可自由决定的。
- `autocomplete.test.ts` / `autocomplete-skill-slash.test.ts` 的断言几乎都先 `.sort()`（默认码点排序）再 `deepStrictEqual`，或用 `includes`/`some`；唯一直接断言顺序的是 fd 组的 `ranks directories before files`（`items[0]` 必须是 `@src/`，由"目录 +10 分"决定，与 locale 无关）与 `keeps quoted directories before files`（只断言 `label.endsWith("/")` 的布尔序列，两个目录同组内部顺序不影响）。
- **Rust 测试建议**：把这些断言改为"集合相等"或"目录优先 + 目录内部集合相等"；不要用 `assert_eq!(vec, expected)` 固定 CJK/拉丁混排顺序。
- **Rust 实现建议**：从 `LC_ALL`/`LC_MESSAGES`/`LANG` 解析 locale（`zh_CN.UTF-8` → `zh-CN`），空/`C`/`POSIX` → `en-US`，再交给 `icu_collation`。若想与"用户机器上的 Node 完全一致"，还需注意 macOS/Windows 的 locale 解析差异。

### A.5 `cjkPunctuationRegex` / `autocompleteSeparatorRegex` 逐字符实测表

| 字符 | `cjkPunctuationRegex.test` | `\p{P}` | CJK scx | 命中分支 |
| --- | --- | --- | --- | --- |
| `。` U+3002 | true | true | true | 前瞻 + CJK |
| `、` U+3001 | true | true | true | 前瞻 + CJK |
| `「` U+300C / `」` U+300D | true | true | true | 前瞻 + CJK |
| `，` U+FF0C、`．` U+FF0E、`：` U+FF1A、`；` U+FF1B、`！` U+FF01、`？` U+FF1F | true | true | false | 显式列表 |
| `（` U+FF08、`）` U+FF09、`［` U+FF3B、`］` U+FF3D、`｛` U+FF5B、`｝` U+FF5D | true | true | false | 显式列表 |
| `“` U+201C、`”` U+201D、`‘` U+2018、`’` U+2019、`…` U+2026、`—` U+2014 | true | true | false | 显式列表 |
| `中` / `あ` / `한` | false | false | true | — |
| `a` / `x` | false | false | false | — |
| ` `（空格）、`　`（U+3000） | **true**（经 `autocompleteSeparatorRegex` 的 `\s` 分支；`cjkPunctuationRegex` 本身 false） | — | — | `\s` |

Rust 实现：`fn is_cjk_punctuation(c: char) -> bool`（同 §6.3 更正后的定义）；`fn is_autocomplete_separator(c: char) -> bool { is_js_whitespace(c) || is_cjk_punctuation(c) }`；`fn is_token_start(text, start)` 见 §2.10.2（**不要**把 `tokenStartRegex` 直接翻译成"末尾字符是分隔符"，因为还要与 `PATH_DELIMITERS`/`PATH_WRAPPERS` 组合）。

### A.6 LaTeX 表格与测试计数（复核）

| 常量 | 条数（JS 平衡括号解析 + `eval` 验证） |
| --- | --- |
| `SYMBOLS` | **224** |
| `NAMED_OPERATORS` | **32** |
| `RELATION_COMMANDS` | **88** |
| `NEGATED_SYMBOLS` | **30** |
| `ACCENTS` | **18** |
| `DISPLAY_LIMIT_SYMBOLS` | **16** |
| `LIMIT_OPERATORS` | **11** |
| `SPACING_COMMANDS` | **12** |
| `NEGATIVE_SPACING_COMMANDS` | **4** |
| `FONT_SWITCH_COMMANDS` | **7** |
| `IGNORED_COMMANDS` | **6** |
| `SIZE_COMMANDS` | **12** |
| `PLAIN_WRAPPERS` | **30** |
| `BLACKBOARD` | **7** |
| `SUPERSCRIPTS` | **40** |
| `SUBSCRIPTS` | **32** |

（正文 §2.9.4 的计数全部正确；曾用 Python 括号计数得到 33/89/17/12/14/7/13/31 的偏差是脚本把注释/逗号算进去所致，以本表为准。）

`test/latex.test.ts`：**19** 个显式 `it`（`^\tit\(`）+ **92** 个 `defineCases` 元素（5 块：37/8/5/25/17）+ 1 个 `describe("renderLatex")`。

### A.7 `render(width)` 的完整宽度/几何锚点（实测）

除正文已有的 `render(1..6,10,12,20,32,40,48,60,80)` 外，补充**去 ANSI** 的实测（空 query、默认键位、非 macOS）：

| 宽度 | 第二行（内容） | 第三行 |
| --- | --- | --- |
| 18 | `" Find in transcr"` | `"└────────── ↑ ↓ ─┘"` |
| 19 | `" Find in transcri"` | `"└─────────── ↑ ↓ ─┘"` |
| 21 | `" Find in transcript"`（无 padding） | `"└───────────── ↑ ↓ ─┘"` |
| 22 | `" Find in transcript "` | `"└────────────── ↑ ↓ ─┘"` |
| 25 | `" Find in transcript    "` | `"└───────────────── ↑ ↓ ─┘"` |
| 26 | `" Find in transcript     "` | `"└────────────────── ↑ ↓ ─┘"` |
| 27 | `" Find in transcript      "` | `"└─────────────────── ↑ ↓ ─┘"` |
| 28 | `" Find in transcript       "` | `"└ ↑ Shift+Enter · ↓ Enter ─┘"` |

按钮区间（`previousStart/previousEnd/nextStart/nextEnd`）见 §2.11.6 的表；`width < 8` 时全为 `-1`。

### A.8 `render(20)` 与 placeholder 推导（替换正文中的自问自答）

见 §2.11.6 修正后的段落：`placeholder` 的 `visibleWidth = 18`；`Input.render(w)` 的 `availableWidth = w - 1`；`AltScreenSearchComponent` 的外层 `inputWidth = innerWidth - visibleWidth(resultText)`。`render(20)`：`innerWidth = 18`、`inputWidth = 18`、`availableWidth = 17` → `"Find in transcrip"`，无 padding。**`inputPadding` 只在 `resultText` 非空（query 非空）时才可能 > 0。**

### A.9 `EditorComponent` 的消费方契约（正文 §2.6 之外的补充）

- `index.ts` 只做 **type re-export**（`export type { EditorComponent }`），无运行时值。
- `editor.ts`/`input.ts` **不**导入 `EditorComponent`：`Editor` 自己声明了结构兼容的成员集，消费者按**结构化类型**判断。
- 真实消费方是 coding-agent：`session-share.ts` 定义 `ActiveEditor = EditorComponent`；`interactive-mode.ts` 用**存在性探测**而非调用：
  - `typeof editor.setPaddingX !== "undefined"` / `newEditor.setPaddingX !== undefined` 才调用（`setPaddingX`/`setAutocompleteMaxVisible`）；
  - `editor.getExpandedText?.() ?? editor.getText()`；
  - `"setExpanded" in obj && typeof obj.setExpanded === "function"`（另一个可选成员探测）。
- **Rust 对应**：所有可选成员必须是 `Option<...>`（**不能**给默认空实现），否则"未提供"与"提供了 no-op"无法区分，会改变 `??` 的回退行为。

### A.10 `AutocompleteProvider.triggerCharacters` 的消费方（正文仅在类型中列出）

- `components/editor.ts` 在 `setAutocompleteProvider` 时调用 `this.setAutocompleteTriggerCharacters(provider.triggerCharacters ?? [])`，与内置默认 `["@", "#"]`（`DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS`，`editor.ts:254`）**合并**，并用 `escapeCharacterClass` 拼进两个动态正则（`buildTriggerPattern` / `buildDebouncePattern`）。
- 合并规则（逐字，`editor.ts:2341–2352`）：从 `[...DEFAULT]` 开始，对 provider 提供的每个字符串 `character`，**跳过**满足任一条件的项：`character.length !== 1`（**按 UTF-16 长度**，因此 emoji 等多码元字符被丢弃）、`character === "/"`（斜杠命令不靠自然触发）、`isWhitespaceChar(character)`（注意：用的是本单元的 **JS `\s` 集合**，见 §A.2/§2.1.17）、`next.includes(character)`（去重，含与默认的 `@`/`#` 重复）。其余按**提供顺序**追加。
- 两个正则的形态（`editor.ts:256–276`，逐字）：

```
unquotedAutocompleteSuffixRegex = new RegExp(`(?:(?!${autocompleteSeparatorRegex.source}).)*`, "u")
autocompleteTokenStartSource    = `${autocompleteBoundaryRegex.source}[([{<\`]*`      // 注意字符类含反引号
buildTriggerPattern(chars)  = `${autocompleteTokenStartSource}(?:@"[^"]*|[${chars.map(escapeCharacterClass).join("")}]${unquotedAutocompleteSuffixRegex.source})$`
buildDebouncePattern(chars) = 先 `escapedWithoutAt = chars.filter(c => c !== "@").map(escapeCharacterClass)`
                              `${autocompleteTokenStartSource}(?:@(?:"[^"]*|${unquotedAutocompleteSuffixRegex.source})|[${escapedWithoutAt.join("")}]${unquotedAutocompleteSuffixRegex.source})$`
```

  **`@` 在 debounce pattern 里被特殊处理**：它不是被"排除"，而是改成独立的 `@(?:"[^"]*|…)*` 分支（引号内可含分隔符）；若 `chars` 只有 `@`，`escapedWithoutAt` 为空 → 第二分支的字符类是空类 `[]`（JS 中 `[]` 永不匹配，等价于该分支不存在）。Rust 手写匹配时需保留这个形态（否则 `@` 后的空格/中文标点会提前截断防抖窗口）。
- `CombinedAutocompleteProvider` **未实现** `triggerCharacters`（undefined）→ 走默认 `["@", "#"]`。
- **Rust 对应**：`fn trigger_characters(&self) -> Option<Vec<char>>` 默认 `None`；消费者（`editor`）负责与 `['@','#']` 合并并去重（`next = [...DEFAULT, ...extra]` 后去重，顺序保留）。

### A.11 其它经实测确认的锚点（可直接作为 Rust 单测）

```
truncateToWidth("\x1b]8;;u\x07abcdefgh", 6, "")    = "\x1b]8;;u\x07abcdef\x1b]8;;\x07\x1b[0m"
truncateToWidth("abc", 3, "", true)                = "abc"
truncateToWidth("界", 1)                            = "\x1b[0m.\x1b[0m"
truncateToWidth("界界", 2)                          = "\x1b[0m..\x1b[0m"
truncateToWidth("ab\tcd", 4)                       = "a\x1b[0m...\x1b[0m"
truncateToWidth("ab\tcd", 5)                       = "ab\x1b[0m...\x1b[0m"
truncateToWidth("ab\tcd", 6, undefined, true)      = "ab\x1b[0m...\x1b[0m "
truncateToWidth("\t\t\t", 7)                       = "\t\x1b[0m...\x1b[0m"
truncateToWidth("abc", 10, "...", true)            = "abc       "
truncateToWidth("", 5, "...", true)                = "     "
truncateToWidth("abcdef", 3, "界界")               = "\x1b[0m界\x1b[0m"
```

`getGraphemeCellRange`：`("abcd",0)={0,1}`、`("abcd",4)=undefined`、`("界a",1)={0,2}`、`("界a",2)={2,3}`、`("\x1b[31mab\x1b[0m",1)={1,2}`、`("\tX",0)={0,3}`、`("\tX",3)={3,4}`。

`getOsc8LinkAtColumn`：`"\x1b]8;;https://a\x07ab\x1b]8;;\x07cd"` 列 0/1 → `"https://a"`，列 2/3 → `undefined`；`"\x1b]8;;u\x07\tab\x1b]8;;\x07"` 列 1 → `"u"`（TAB 占 0..3）。

`fuzzyMatch` 分数（逐位，见 §2.2 的完整表）已复核：`("test","test")=-159.4`、`("opus","xxopus opus")=("opus","xxxxxx opus")=-36.6`、`("codex52","gpt-5.2-codex")=-64.99999999999999`。

`renderLatex` 的空白相关：`x^2_3`（见 §A.1）；输入 `x` + 反斜杠 + LF + `y` → `"x y"`（`parseCommand` 把 `\` 后紧跟的换行当作控制空格）。

### A.12 未被正文覆盖的决策点（审计新增）

1. **`parseScripts` 与 `parseOperator` 的脚本解析不共享代码**：前者保留出现顺序、后者去空格并允许 `\limits`；Rust 侧应各写一个函数，**不要**合并。
2. **`scriptDepth` 是实例字段**（不是参数），嵌套 parser（`renderNested`）会创建**新实例**，因此嵌套层内的 `scriptDepth` 从 0 开始 —— 只有同一 parser 内"脚本套脚本"才触发 `needsLayout`。
3. **`parseRequiredArgument(false)` 的 `stackFractions` 保存/恢复**：`parseRequiredArgument` 保存旧值、`stackFractions = prev && stackFractions`、调用 `parseRequiredArgumentValue`、恢复。`parseScripts` 传 `false` → 参数内 `stackFractions` 恒 false；`renderNested` 的 `display` 参数也是 `this.display && stackFractions`。
4. **`AltScreenSearchIndex.search` 在 query/行未变时返回**同一 `matches` 数组引用**（`changed=false`）**，消费者用引用相等判断是否重绘——Rust 侧需返回 `Arc<Vec<...>>` 或显式 `changed` 标志（规范已用后者）。
5. **`applySearchHighlights` 的 `ranges.sort((a,b)=>b.startCol-a.startCol)`**：从右向左应用，避免左侧插入改变右侧列号；Rust 必须**降序**遍历。
6. **`refreshSearch` 的返回值只表示"是否滚动"**，不表示"是否更新了结果"。
7. **`getNavigationDirectionAt(row, col)` 的 `row` 是相对 overlay 的 0-based 行**，且只接受 `row === 2`。
8. **`setHoveredNavigationDirection` 只在值变化时返回 true**（用于抑制重绘）。
9. **`AltScreenSearchComponent.render` 每帧重新读取 `getKeybindings()`**（热键改动立即生效），Rust 侧不能在构造时缓存。
10. **`AltScreenSearchComponent` 的按钮命中区间是半开区间** `[start, end)`（`column >= start && column < end`），且 `row` 必须是 `2`（相对 overlay 顶部 0-based）。

---

（规范结束）
