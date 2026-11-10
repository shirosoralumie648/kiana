# `tui-components` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-tui` 中 **tui-components** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`tui-components`。
参考实现（只读）：`/home/ctyun/code/kiana_rev/kiana/packages/tui/src/components/`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串均按 TypeScript 源码**逐字**抄录，Rust 实现必须输出完全一致的文本（除另有说明）。中文用于解释语义。凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/tui/src/components/alt-screen-flash.ts      (51 行)
packages/tui/src/components/box.ts                   (172 行)
packages/tui/src/components/cancellable-loader.ts    (40 行)
packages/tui/src/components/editor.ts                (2473 行)
packages/tui/src/components/h-stack.ts               (44 行)
packages/tui/src/components/image.ts                 (167 行)
packages/tui/src/components/input.ts                 (501 行)
packages/tui/src/components/loader.ts                (101 行)
packages/tui/src/components/markdown.ts              (1065 行)
packages/tui/src/components/mouse-region.ts          (33 行)
packages/tui/src/components/scroll-view.ts           (224 行)
packages/tui/src/components/select-list.ts           (273 行)
packages/tui/src/components/settings-list.ts         (328 行)
packages/tui/src/components/spacer.ts                (28 行)
packages/tui/src/components/stack.ts                 (154 行)
packages/tui/src/components/text.ts                  (113 行)
packages/tui/src/components/truncated-text.ts        (65 行)
packages/tui/src/components/v-stack.ts               (33 行)
```

合计 5865 行。**本单元覆盖全部 18 个组件文件，不区分“主/次”。**

配套测试（本单元验收基准）：

```
packages/tui/test/editor.test.ts                    (192 个 it)
packages/tui/test/input.test.ts                     (36)
packages/tui/test/markdown.test.ts                  (92)
packages/tui/test/select-list.test.ts               (5)
packages/tui/test/settings-list.test.ts             (2)
packages/tui/test/truncated-text.test.ts            (9)
packages/tui/test/mouse-components.test.ts          (9)
packages/tui/test/editor-history-keybindings.test.ts (1)
packages/tui/test/word-navigation.test.ts           (19, 单元 tui-core 拥有函数，但本单元的 editor/input 依赖)
packages/tui/test/wrap-ansi.test.ts                 (19, 单元 tui-core)
packages/tui/test/visible-width.test.ts             (4, 单元 tui-core)
packages/tui/test/tab-width.test.ts                 (4, 单元 tui-core)
packages/tui/test/layout.test.ts                    (15, 单元 tui-core)
packages/tui/test/terminal-image.test.ts            (80, 单元 tui-terminal；其中 Image 组件相关用例属本单元验收)
packages/tui/test/tui-alt-screen.test.ts            (68, 单元 tui-core；消费本单元组件)
packages/tui/test/tui-render.test.ts                (32, 单元 tui-core；消费本单元组件)
packages/tui/test/test-themes.ts                    (测试主题，chalk level 3)
packages/tui/test/virtual-terminal.ts               (@xterm/headless 虚拟终端)
```

文档：`packages/tui/README.md`（949 行，公共 API 与逐组件用法，是用户可见契约的权威来源）、`packages/tui/CHANGELOG.md`（1.2.0 fake cursor APC、1.0.0 Markdown WeakRef 缓存、1.4.0 fuzzy 连续匹配重打分与未闭合 `$$` 表格修复）、`packages/coding-agent/docs/tui.md`（扩展开发者视角的组件契约）。

### 0.2 明确**不**属于本单元（由其他单元移植，但本单元依赖其符号）

- `src/tui.ts`——`Component`、`Focusable`、`CURSOR_MARKER`、`renderFakeCursor`、`Container`、`compositeTuiLine`、`dispatchMouseEvent`、`retargetMouseEvent`、`TUI` 接口、`TuiMouseEvent*` 类型（单元 **tui-core**）。
- `src/utils.ts`——`visibleWidth`、`wrapTextWithAnsi`、`truncateToWidth`、`sliceByColumn`、`sliceWithWidth`、`extractSegments`、`extractAnsiCode`、`flattenLines`、`applyBackgroundToLine`、`getGraphemeSegmenter`、`getWordSegmenter`、`cjkBreakRegex`、`cjkPunctuationRegex`、`autocompleteSeparatorRegex`、`autocompleteBoundaryRegex`、`isWhitespaceChar`、`isPunctuationChar`、`PUNCTUATION_REGEX`、`getGraphemeCellRange`（单元 **tui-core**）。
- `src/keybindings.ts`——`getKeybindings()`、`KeybindingsManager`、`TUI_KEYBINDINGS`（单元 **tui-core**）。
- `src/keys.ts`——`matchesKey`、`decodeKittyPrintable`、`decodePrintableKey`（单元 **tui-core**）。
- `src/kill-ring.ts`、`src/undo-stack.ts`、`src/word-navigation.ts`、`src/fuzzy.ts`（单元 **tui-core**）。
- `src/layout-node.ts`、`src/layout.ts`（单元 **tui-core**；`ScrollView`/`Stack` 的 `[LAYOUT_NODE]` 契约定义在此）。
- `src/terminal-image.ts`（单元 **tui-terminal**；`Image` 组件消费）。
- `src/latex.ts`（单元 **tui-latex**；`Markdown` 消费 `renderLatex`）。
- `src/autocomplete.ts`（单元 **tui-autocomplete**；`Editor` 消费 `AutocompleteProvider`）。
- `src/tui-main-screen.ts`、`src/tui-alt-screen.ts`、`src/alt-screen-search.ts`（单元 **tui-core**）。
- `src/editor-component.ts`（`EditorComponent` 接口，单元 **tui-core** 的导出面）。

**边界重叠的处理约定**：`tui-text.md` 把 `utils.ts`、`alt-screen-search.ts`、`fuzzy.ts`、`kill-ring.ts`、`undo-stack.ts`、`word-navigation.ts` 等划给 **tui-text**，与本文件“划给 tui-core”的写法不同。两份规格的处理策略一致：**实现只有一份**（Rust crate `kiana_tui_text`，或 `kiana_tui::text::*`），`tui_components` 直接依赖并复用，不得重复实现；若数值/边界有冲突，以 TS 源码为准。`terminal-image.ts` 同理（`tui-core.md` 与 `tui-terminal` 共享，取并集）。本单元对这些符号**只消费、不拥有**，签名以 §5.1 为准。

### 0.3 建议阅读顺序

`stack.ts` → `v-stack.ts` → `h-stack.ts` → `spacer.ts` → `truncated-text.ts` → `text.ts` → `box.ts` → `mouse-region.ts` → `alt-screen-flash.ts` → `loader.ts` → `cancellable-loader.ts` → `select-list.ts` → `settings-list.ts` → `input.ts` → `editor.ts` → `markdown.ts` → `scroll-view.ts` → `image.ts`。

---

## 1. 概览

### 1.1 包的用途

`@earendil-works/kiana-tui`（v1.4.0，ESM，Node ≥ 22.19.0，MIT）是 **终端 UI 组件库 + 差分渲染器**。本单元是其**组件层**：所有可被 `TUI.addChild()`、`showOverlay()`、`ScrollView`、`VStack`/`HStack` 承载的叶子与容器组件。

组件层的公共契约只有一条硬约束：**`render(width)` 返回的每一行可见宽度必须 ≤ width**。主屏渲染器在检测到超宽时会写崩溃日志并抛错（见 `tui-main-screen.ts`，单元 tui-core）：

**【逐字】**（错误消息由 tui-core 产生，但本单元每个组件都必须保证不触发它）

```
Rendered line ${i} exceeds terminal width (${visibleWidth(line)} > ${width}).

This is likely caused by a custom TUI component not truncating its output.
Use visibleWidth() to measure and truncateToWidth() to truncate lines.

Debug log written to: ${crashLogPath}
```

### 1.2 与其他包/单元的关系

```
kiana-tui (tui-core: Component/TUI/utils/keys/keybindings/layout/renderers)
   │
   ├── tui-terminal: terminal-image.ts (Kitty/iTerm2/尺寸解析/超链接)
   ├── tui-latex:    latex.ts (renderLatex)
   └── tui-components (本单元)
         ├── editor.ts   ← autocomplete.ts (provider), keys.ts, keybindings.ts, word-navigation.ts, kill-ring.ts, undo-stack.ts
         ├── markdown.ts ← marked, marked-cjk-friendly, latex, terminal-image
         ├── image.ts    ← terminal-image
         ├── scroll-view / stack / v-stack / h-stack ← layout-node.ts, layout.ts
         └── 其余组件    ← tui.ts, utils.ts
```

下游消费者（`packages/coding-agent`）对本单元有**继承级**依赖，Rust 侧必须保留可覆写点：

- `CustomEditor extends Editor`（`modes/interactive/components/custom-editor.ts`），覆写 `protected renderTopBorder(width, hiddenLineCount)`（**只此一个**；`renderBottomBorder` 未被覆写）。内部：`!embedWorkingStatus || !workingStatusIndicator || width <= 0` → 直接 `super.renderTopBorder(...)`；否则用 `this.borderColor(...)` 拼接 `"── "` + `workingStatusIndicator.renderInBorder(Math.max(1, width - 5))` + 剩余横线/`↑ n more` 溢出标签（宽度不足时退回 `renderSpinnerInBorder`）。另有 `this.embedWorkingStatus`、`setWorkingStatusIndicator()`、`onAction(action, handler)`、`onEscape`/`onCtrlD`/`onPasteImage`/`onExtensionShortcut`、`actionHandlers: Map` 等自身成员，且**重写了 `handleInput`** 先分派扩展快捷键与 app 级 action，再 `super.handleInput`。
- `StatusIndicator extends Loader`（`components/status-indicator.ts`），调用 `super.render(width + 2)[1]`、`protected getRenderedIndicator()` 与 `stop()`（封装为 `dispose()`）。
- `ThemedText extends Text`（`components/themed-text.ts`）、`WriteCallRenderComponent extends Text`（`core/tools/renderers/write.ts`）。
- `CompactionSummaryMessageComponent` / `BranchSummaryMessageComponent` / `SkillInvocationMessageComponent extends Box`。
- `show-images-selector` / `theme-selector` / `settings-submenu` / `thinking-selector` 直接构造 `SelectList` 并传 `SelectListLayoutOptions`（含 `minPrimaryColumnWidth`/`maxPrimaryColumnWidth`/`truncatePrimary`）。
- `settings-selector` 构造 `SettingsList`（含 `enableSearch` 与 `navigateTo` 子菜单回调）。
- `login-dialog` / `oauth-selector` / `scoped-models-selector` / `tree-selector` / `config-selector` 使用 `Input`、`TruncatedText`。

### 1.3 入口点

本单元没有独立包入口；全部符号经 `packages/tui/src/index.ts` 再导出：

```ts
export { Box } from "./components/box.ts";
export { CancellableLoader } from "./components/cancellable-loader.ts";
export { Editor, type EditorOptions, type EditorTheme } from "./components/editor.ts";
export { HStack } from "./components/h-stack.ts";
export { Image, type ImageOptions, type ImageTheme, type ImageTranscoder, setImageTranscoder } from "./components/image.ts";
export { Input } from "./components/input.ts";
export { Loader, type LoaderIndicatorOptions } from "./components/loader.ts";
export { type DefaultTextStyle, Markdown, type MarkdownOptions, type MarkdownTheme } from "./components/markdown.ts";
export { MouseRegion, type MouseRegionHandler } from "./components/mouse-region.ts";
export { ScrollView, type ScrollViewOptions, type ScrollViewScrollbar, type ScrollViewScrollToOptions } from "./components/scroll-view.ts";
export { type SelectItem, SelectList, type SelectListLayoutOptions, type SelectListTheme, type SelectListTruncatePrimaryContext } from "./components/select-list.ts";
export { type SettingItem, SettingsList, type SettingsListTheme } from "./components/settings-list.ts";
export { Spacer } from "./components/spacer.ts";
export { Text } from "./components/text.ts";
export { TruncatedText } from "./components/truncated-text.ts";
export { type StackChild, type StackEntry, type StackEntryOptions, type StackOptions, VStack } from "./components/v-stack.ts";
```

另外 `wordWrapLine`、`TextChunk`、`SLASH_COMMAND_SELECT_LIST_LAYOUT` 未从包入口导出（`wordWrapLine` 由测试直接 `import { wordWrapLine } from "../src/components/editor.ts"`）。`stack.ts` 的 `visibleStackEntries`/`allocateStackSizes`/`Stack` 供 `layout.ts` 使用，也不在包入口。

**导出面清点**（`src/components/*.ts` 共 **49** 条 `export` 语句，跨 18 个文件；其中 1 条是 `v-stack.ts` 的 `export type { StackChild, StackEntry, StackEntryOptions, StackOptions } from "./stack.ts"` 再导出）：

| 文件 | 导出的符号 |
|---|---|
| `stack.ts` | `StackEntryOptions`、`StackEntry`、`StackChild`、`StackOptions`、`Stack`（abstract class）、`visibleStackEntries`、`allocateStackSizes` |
| `v-stack.ts` | `VStack` + 4 个类型再导出 |
| `h-stack.ts` | `HStack` |
| `spacer.ts` | `Spacer` |
| `truncated-text.ts` | `TruncatedText` |
| `text.ts` | `Text` |
| `box.ts` | `Box` |
| `mouse-region.ts` | `MouseRegionHandler`、`MouseRegion` |
| `alt-screen-flash.ts` | `AltScreenFlashContainer`（**未**从包入口导出，由 `tui-alt-screen` 直接 import） |
| `loader.ts` | `LoaderIndicatorOptions`、`Loader` |
| `cancellable-loader.ts` | `CancellableLoader` |
| `select-list.ts` | `SelectItem`、`SelectListTheme`、`SelectListTruncatePrimaryContext`、`SelectListLayoutOptions`、`SelectList` |
| `settings-list.ts` | `SettingItem`、`SettingsListTheme`、`SettingsListOptions`、`SettingsList`（`SettingsListOptions` **未**从包入口导出） |
| `input.ts` | `InputOptions`、`Input`（`InputOptions` 亦未从包入口导出） |
| `editor.ts` | `TextChunk`、`wordWrapLine`、`EditorTheme`、`EditorOptions`、`Editor` |
| `markdown.ts` | `DefaultTextStyle`、`MarkdownTheme`、`MarkdownOptions`、`Markdown` |
| `scroll-view.ts` | `ScrollViewScrollbar`、`ScrollViewOptions`、`ScrollViewScrollToOptions`、`ScrollView` |
| `image.ts` | `ImageTranscoder`、`setImageTranscoder`、`ImageTheme`、`ImageOptions`、`Image` |

### 1.4 运行面特征（决定 Rust 设计）

- **单线程事件循环**。组件的定时器（Loader 动画、ScrollView 滚动条隐藏、AltScreenFlash 过期）与异步补全共享同一循环。
- **JS 字符串是 UTF-16**：`editor.ts` 的 `cursorCol`、`wordWrapLine` 的 `startIndex/endIndex`、paste marker 的 `slice` 全部以 UTF-16 code unit 计数。Rust 侧必须在内部统一表示（三种方案与取舍见 §6.3 第 1 条：推荐 `String` + 惰性 UTF-16↔字节偏移换算层 `Utf16Index`；**不要**直接把 `cursorCol` 当字节偏移或 code point 偏移）。
- **`Intl.Segmenter`**：grapheme 与 word 两套分段，是整个编辑器的光标/删除/换行语义基础。
- **`structuredClone`**：undo 快照深拷贝（`UndoStack.push`）。Rust 用 `Clone` + `Mutex`/所有权即可。
- **`NodeJS.Timeout.unref()`**：`ScrollView` 的滚动条隐藏定时器与 `AltScreenFlash` 的过期定时器都调用 `unref()`（不阻止进程退出）；`Loader` 的 `setInterval` 与 `Editor` 的 autocomplete 防抖 `setTimeout` **没有** unref。Rust 侧对应语义：“unref 的定时器在 drop 时静默取消、不阻塞退出”；未 unref 的定时器在 Rust 中无强制对应（进程退出由上层决定），按普通异步任务处理即可。
- **缓存**：`Text`/`Markdown`/`Box`/`Image` 缓存上次渲染结果；`Markdown` 用 `WeakRef` 缓存 token 树；`visibleWidth` 有 512 项全局 LRU 式（删最旧键）缓存。Rust 侧缓存需与 `invalidate()` 契约严格一致，否则测试的主题切换用例会失败。

---

## 2. 逐文件规范

### 2.1 `src/components/stack.ts`（154 行）— 栈布局基类与尺寸分配

#### 类型

```ts
export interface StackEntryOptions {
  basis?: number | "auto";
  grow?: number;
  shrink?: number;
  minSize?: number;
  maxSize?: number;
  visible?: (viewport: LayoutViewport) => boolean;   // LayoutViewport = { width: number; height: number }
}
export interface StackEntry extends StackEntryOptions { component: Component; }
export type StackChild = Component | StackEntry;
export interface StackOptions { gap?: number; align?: "stretch" | "start" | "center" | "end"; }
```

#### 内部函数

- `isStackEntry(child)`：`!("render" in child)`。即“没有 render 方法就是配置对象”。这是鸭子判定，Rust 侧必须用显式 enum（`StackChild::Component(Box<dyn Component>)` / `StackChild::Entry{...}`）替代，由构造 API 决定，不做运行时嗅探。
- `normalizeSize(value, fallback)`：`value === undefined || !Number.isFinite(value)` → fallback；否则 `max(0, floor(value))`。（注意 JS 的 `Number.isFinite(null)` 为 false；Rust 侧仅需处理 `Option<f64>`。）
- `clampSize(size, entry)`：`min = max(0, floor(entry.minSize ?? 0))`；`max = max(min, floor(entry.maxSize ?? MAX_SAFE_INTEGER))`；返回 `max(min, min(max, max(0, floor(size))))`。

#### `abstract class Stack extends Container`

字段：`protected readonly entries: StackLayoutEntry[] = []`、`protected readonly gap: number`、`protected readonly align`、`protected abstract readonly layoutType: "vstack" | "hstack"`。

构造 `constructor(children: StackChild[] = [], options: StackOptions = {})`：
- `gap = normalizeSize(options.gap, 0)`；`align = options.align ?? "stretch"`。
- 对每个 child：是 entry 则 `addChild(child.component, child)`，否则 `addChild(child)`。

`override addChild(component, options: StackEntryOptions = {})`：先 `super.addChild(component)`（推入 `Container.children`），再向 `entries` 推入一个对象，**只复制显式给出的字段**（undefined 的字段不写入，从而保持 `entry.minSize ?? 0` 等默认语义）：
- `basis`：原样（可为数字或 `"auto"`）。
- `grow`：`normalizeSize(options.grow, 0)`
- `shrink`：`normalizeSize(options.shrink, 1)`
- `minSize`：`normalizeSize(options.minSize, 0)`
- `maxSize`：`normalizeSize(options.maxSize, Number.MAX_SAFE_INTEGER)`
- `visible`：原样

`removeChild(component)`：`super.removeChild` 后从 `entries` 删除第一个 `entry.component === component` 的项（只删一个）。
`clear()`：`super.clear()` 且 `entries.length = 0`。
`[LAYOUT_NODE](): StackLayoutNode`：返回 `{ type: this.layoutType, entries: this.entries, gap: this.gap, align: this.align }`。`LAYOUT_NODE = Symbol.for("@earendil-works/kiana-tui/layout-node")`（单元 tui-core 定义）；Rust 侧对应 `fn layout_node(&self) -> Option<LayoutNode>` 特征方法（默认 `None`）。

#### `export function visibleStackEntries(entries, viewport): StackLayoutEntry[]`

`entries.filter(entry => entry.visible?.(viewport) ?? true)`。

#### `export function allocateStackSizes(entries, intrinsicSizes, availableSize, gap): number[]`

算法（逐字语义）：

1. 初始 `sizes[i] = clampSize(basis === undefined || basis === "auto" ? (intrinsicSizes[i] ?? 0) : basis, entries[i])`。
2. 若 `availableSize === undefined`，直接返回 `sizes`（“测量模式”：不生长、不收缩）。
3. `contentSize = max(0, floor(availableSize) - max(0, entries.length - 1) * gap)`。
4. `total = sum(sizes)`。`total < contentSize` → `distribute(sizes, entries, contentSize - total, "grow")`；`total > contentSize` → `distribute(..., total - contentSize, "shrink")`；相等不处理。

`distribute(sizes, entries, amount, mode)` 循环：
- `remaining = amount`；`while (remaining > 0)`：
  - 候选集合 = 满足以下条件的项：grow 模式：`(entry.grow ?? 0) > 0 && sizes[i] < (entry.maxSize ?? MAX_SAFE_INTEGER)`；shrink 模式：`(entry.shrink ?? 1) > 0 && sizes[i] > (entry.minSize ?? 0)`。
  - 无候选 → 直接 `return`。
  - `totalWeight`：grow 用 `entry.grow ?? 0`；shrink 用 `(entry.shrink ?? 1) * max(1, sizes[i])`。
  - 逐个候选（按 entries 顺序）：`weight` 同上；`proposed = max(1, floor(remaining * weight / totalWeight))`；`capacity`：grow 为 `maxSize - sizes[i]`，shrink 为 `sizes[i] - minSize`；`delta = min(remaining, proposed, capacity)`；`delta <= 0` 跳过；否则 `sizes[i] += delta`（shrink 时 `-=`），`remaining -= delta`，`distributed += delta`。
  - 若本轮 `distributed === 0` → `return`（防死循环）。
- 注意 `totalWeight` 可能为 0（grow 但权重全 0 已被候选条件排除，故实际非 0；shrink 权重恒 > 0）。`proposed` 至少 1，配合 `capacity` 与 `min(remaining,...)` 保证终止。

**测试基准**（`layout.test.ts`）：`allocates vertical grow space deterministically`、`shrinks entries to their minimum sizes`、`includes nested minimum sizes in intrinsic stack measurement`、`omits gaps around invisible entries`。

### 2.2 `src/components/v-stack.ts`（33 行）

```ts
export class VStack extends Stack {
  protected readonly layoutType = "vstack" as const;
  constructor(children: StackChild[] = [], options: StackOptions = {}) { super(children, options); }
  override render(width: number): string[]
}
export type { StackChild, StackEntry, StackEntryOptions, StackOptions } from "./stack.ts";
```

`render(width)`：
1. `viewport = { width: max(1, width), height: Number.MAX_SAFE_INTEGER }`。
2. `entries = visibleStackEntries(this.entries, viewport)`。
3. `rendered = entries.map(e => e.component.render(viewport.width))`。
4. `sizes = allocateStackSizes(entries, rendered.map(l => l.length), undefined, gap)` —— **不传可用高度**，因此尺寸就是每个孩子的自然高度（受 basis/min/max 夹取）。VStack 自身不裁剪高度；只有被放进 `layout.ts` 的 `layoutComponent`（有 height 参数）或 `ScrollView` 时才按分配高度裁剪。
5. 拼接：对每个 index，`index > 0` 时先推入 `gap` 个 `""`；再推入 `rendered[index].slice(0, sizes[index])`；再补 `""` 直到该孩子占满 `sizes[index]` 行。
6. 返回 `lines`（可能为 `[]`）。

注意：`Stack` 的构造顺序决定 `Container.children` 与 `entries` 同步；VStack 的 `render` 不使用 `Container.render`。

### 2.3 `src/components/h-stack.ts`（44 行）

```ts
export class HStack extends Stack {
  protected readonly layoutType = "hstack" as const;
  override render(width: number): string[]
}
```

`render(width)`：
1. `safeWidth = max(1, width)`；`viewport = { width: safeWidth, height: MAX_SAFE_INTEGER }`；`entries = visibleStackEntries(...)`；空则 `[]`。
2. **先测量**：每个 child 以 `safeWidth` 渲染一次，`intrinsicWidths[i] = max over lines(visibleWidth(line))`（0 行则为 0）。
3. `widths = allocateStackSizes(entries, intrinsicWidths, safeWidth, gap)`。
4. **再以分配宽度渲染**：`widths[i] === 0 ? [] : entry.component.render(widths[i])`。
5. `height = max(rendered[i].length)`；`result = Array(height).fill("")`。
6. `x = 0`；逐列：
   - `offset`：`align === "center"` → `floor((height - lines.length)/2)`；`"end"` → `height - lines.length`；`"start"`/`"stretch"` → 0。
   - 对每条 line `row`：`target = row + offset`，越界跳过；`result[target] = compositeTuiLine(result[target], lines[row], x, childWidth, safeWidth)`。
   - `x += childWidth + gap`。
7. 返回 `result`。

`compositeTuiLine(base, overlay, startCol, overlayWidth, totalWidth)` 语义（tui-core `tui.ts:426`，逐字）：

```ts
if (isImageLine(baseLine)) return baseLine;                    // 图片行从不被覆盖
afterStart = startCol + overlayWidth;
base = extractSegments(baseLine, startCol, afterStart, totalWidth - afterStart, true);
overlay = sliceWithWidth(overlayLine, 0, overlayWidth, true);
beforePad = max(0, startCol - base.beforeWidth);
overlayPad = max(0, overlayWidth - overlay.width);
actualBeforeWidth = max(startCol, base.beforeWidth);
actualOverlayWidth = max(overlayWidth, overlay.width);
afterTarget = max(0, totalWidth - actualBeforeWidth - actualOverlayWidth);
afterPad = max(0, afterTarget - base.afterWidth);
result = base.before + " ".repeat(beforePad) + SEGMENT_RESET + overlay.text + " ".repeat(overlayPad)
       + SEGMENT_RESET + base.after + " ".repeat(afterPad);
return visibleWidth(result) <= totalWidth ? result : sliceByColumn(result, 0, totalWidth, true);
```

- `SEGMENT_RESET = "\x1b[0m\x1b]8;;\x07"`（清 SGR + 关 OSC 8），出现在 overlay **前后各一次**。
- `extractSegments(line, beforeEnd, afterStart, afterLen, strictAfter)` 返回 `{before, beforeWidth, after, afterWidth}`，`after` 段会**继承 `afterStart` 处的样式状态**（内部用 `AnsiCodeTracker` 在 `after` 开头重放活跃 SGR），并把 `CURSOR_MARKER` 这类 APC（`\x1b_` 开头）保留在边界处。
- `sliceWithWidth(line, 0, overlayWidth, true)`：严格模式，宽字符跨越边界时用空格补齐（返回的 `overlay.width` 可能小于/等于 `overlayWidth`）。
- **HStack 依赖它的“空格补齐 + 两次 SEGMENT_RESET + 图片行短路”行为**；Rust 侧必须精确复刻（见 §8 硬点）。

### 2.4 `src/components/spacer.ts`（28 行）

```ts
export class Spacer implements Component {
  constructor(lines: number = 1);
  setLines(lines: number): void;
  invalidate(): void {}          // 空实现
  render(_width: number): string[]  // 返回 lines 个 ""
}
```
负数/小数未做归一化：`lines = 2.5` 时 `for (i < 2.5)` 实际推入 3 个 `""`（JS `i` 为整数，`0,1,2 < 2.5`）。负数 → 0 行。Rust 侧需保持 `while i < self.lines` 用浮点比较，或显式 `ceil` 后取 `max(0,..)`（推荐后者并记录差异；测试只覆盖正整数）。

### 2.5 `src/components/truncated-text.ts`（65 行）

```ts
export class TruncatedText implements Component {
  constructor(text: string, paddingX: number = 0, paddingY: number = 0);
  invalidate(): void {}
  render(width: number): string[]
}
```

`render(width)`：
1. `result = []`；`emptyLine = " ".repeat(width)`（width ≤ 0 时为 `""`，但 JS `" ".repeat(负数)` 会抛 RangeError；实际调用方保证 width ≥ 1；Rust 侧用 `saturating` 处理）。
2. 上方 padding：`paddingY` 个 `emptyLine`。
3. `availableWidth = max(1, width - paddingX * 2)`。
4. 只取第一行：`idx = text.indexOf("\n")`，`singleLineText = idx === -1 ? text : text.slice(0, idx)`（**只按 `\n` 切；`\r` 保留**）。
5. `displayText = truncateToWidth(singleLineText, availableWidth)`（默认省略号 `"..."`）。
6. `lineWithPadding = " ".repeat(paddingX) + displayText + " ".repeat(paddingX)`。
7. `paddingNeeded = max(0, width - visibleWidth(lineWithPadding))`；`finalLine = lineWithPadding + " ".repeat(paddingNeeded)`。
8. 推入 `finalLine`；下方 padding：`paddingY` 个 `emptyLine`。

**每个输出行可见宽度恰好 = width**（测试：`pads output lines to exactly match width`、`truncates long text and pads to width`、`handles empty text`、`stops at newline and only shows first line`、`truncates first line even with newlines in text`）。省略号前必须出现 `\x1b[0m`（`truncateToWidth` 行为，测试 `truncates styled text and adds reset code before ellipsis` 断言 `lines[0].includes("\x1b[0m...")`）。

### 2.6 `src/components/text.ts`（113 行）

```ts
export class Text implements Component {
  constructor(text: string = "", paddingX: number = 1, paddingY: number = 1,
              customBgFn?: (text: string) => string);
  setText(text: string): void;
  setCustomBgFn(customBgFn?: (text: string) => string): void;
  setPaddingX(paddingX: number): void;
  invalidate(): void;
  render(width: number): string[];
}
```

状态：`text`、`paddingX`、`paddingY`、`customBgFn?`、缓存 `{cachedText?, cachedWidth?, cachedLines?}`。

`setText` 与 `setCustomBgFn` 直接清空三份缓存；`setPaddingX` 调 `invalidate()`（注意：**`setPaddingX` 不清 `text`**）。

`render(width)`：
1. 缓存命中条件：`cachedLines && cachedText === this.text && cachedWidth === width` → 原样返回（同一 `string[]` 引用，Rust 侧返回 `Arc<[String]>` 或 `Rc<Vec<String>>`）。
2. **空文本短路**：`!text || text.trim() === ""` → 缓存并返回 `[]`（注意：不是 `[""]`）。这意味着全空白 Text 完全不渲染任何行。
3. `normalizedText = text.replace(/\t/g, "   ")`（tab → **3** 个空格）。
4. `paddingX = min(this.paddingX, max(0, floor((width - 1) / 2)))`（宽度不足时收窄 padding；`width=1` → 0）。
5. `contentWidth = max(1, width - paddingX * 2)`。
6. `wrappedLines = wrapTextWithAnsi(normalizedText, contentWidth)`（tui-core；不做 padding）。
7. 对每行：`lineWithMargins = " ".repeat(paddingX) + line + " ".repeat(paddingX)`；若有 `customBgFn` → `applyBackgroundToLine(lineWithMargins, width, customBgFn)`；否则 `lineWithMargins + " ".repeat(max(0, width - visibleWidth(lineWithMargins)))`。
8. `emptyLine = " ".repeat(width)`；`paddingY` 个（有 `customBgFn` 时经过 `applyBackgroundToLine(emptyLine, width, bgFn)`）。
9. `result = [...emptyLines, ...contentLines, ...emptyLines]`；`flattenLines(result)`（tui-core 的 V8 扁平化技巧，Rust 侧 no-op）；写缓存。
10. 若 `result.length > 0` 返回 `result`，否则返回 `[""]`。（步骤 2 已保证空文本提前返回，因此该分支仅在 `paddingY=0` 且 `wrapTextWithAnsi` 返回空数组时出现。）

### 2.7 `src/components/box.ts`（172 行）

```ts
export class Box implements Component {
  children: Component[] = [];
  constructor(paddingX = 1, paddingY = 1, bgFn?: (text: string) => string);
  addChild(component: Component): void;
  removeChild(component: Component): void;
  clear(): void;
  setBgFn(bgFn?: (text: string) => string): void;
  setPaddingX(paddingX: number): void;
  invalidate(): void;
  handleMouse(event: TuiMouseEvent): TuiMouseDispatchResult | undefined;
  render(width: number): string[];
}
```

缓存结构 `{ childLines: string[]; width: number; bgSample: string | undefined; lines: string[] }`；`mouseLayout?: { width, children: [{component, height}] }`。

- `setBgFn` **不**清缓存；改为在 render 时通过 `bgFn("test")` 采样比较（这样主题切换但输出不变时避免重排）。`setPaddingX` 清缓存（`invalidateCache()`）。
- `invalidate()`：清自身缓存并逐个 `child.invalidate?.()`。

`mouseLayout` 缓存：`{ width, children: [{component, height}] }`，由 `render` 填充，`handleMouse` 在 `mouseLayout.width === contentWidth` 时复用、否则现场以 `contentWidth` 渲染每个 child 取高度（**注意现场渲染会触发 child 的渲染副作用**）。

`render(width)`：
1. `children.length === 0` → `[]`。
2. `contentWidth = max(1, width - paddingX*2)`；`leftPad = " ".repeat(paddingX)`。
3. 逐个 child 渲染 `contentWidth`，收集未加 padding 的 `childLines`（注释说明：保持字符串对象同一性以便缓存比较走引用相等）；同时记录 `mouseChildren`（含 `height = lines.length`），写入 `this.mouseLayout`。
4. `childLines.length === 0` → `[]`。
5. `bgSample = bgFn ? bgFn("test") : undefined`。
6. 缓存命中（`width`、`bgSample` 全等、`childLines` 长度与逐行字符串相等）→ 返回缓存 `lines`。
7. 输出：`paddingY` 个 `applyBg("", width)`；每个 child 行 → `applyBg(leftPad + line, width)`；再 `paddingY` 个 `applyBg("", width)`。
8. `flattenLines(result)`；写缓存；返回。

`applyBg(line, width)`：`padded = line + " ".repeat(max(0, width - visibleWidth(line)))`；`bgFn ? bgFn(padded) : padded`。**每行宽度恰好 = width**。

`handleMouse(event)`：
- `contentWidth = max(1, event.width - paddingX*2)`；`contentY = event.y - paddingY`；`contentX = event.x - paddingX`。
- `contentY < 0 || contentX < 0 || contentX >= contentWidth` → `undefined`。
- 子布局：`mouseLayout.width === contentWidth` 则复用，否则现场以 `contentWidth` 渲染所有 child 求高度。
- 顺序累加 `childY`，命中 `contentY ∈ [childY, childY + height)` → `dispatchMouseEvent(child, { ...event, x: contentX, y: contentY - childY, width: contentWidth, height: childHeight })`。
- 未命中 → `undefined`。（Box 不做 focus 提升；focus 由 `dispatchMouseEvent` 的 `focusTarget` 机制处理。）

注意：Box **不**对 `event.height` 做 `paddingY` 裁剪；只在纵向 `contentY` 上偏移。

### 2.8 `src/components/mouse-region.ts`（33 行）

```ts
export type MouseRegionHandler = (event: TuiMouseEvent) => TuiMouseEventResult | undefined;
export class MouseRegion implements Component {
  constructor(child: Component, onMouse: MouseRegionHandler);
  render(width: number): string[];   // child.render(width)
  handleMouse(event): TuiMouseDispatchResult | TuiMouseEventResult | undefined;
  invalidate(): void;                // child.invalidate()
}
```
`handleMouse`：`dispatchMouseEvent(child, event) ?? this.onMouse(event)`。即子组件先处理；未处理才调用外部 handler。返回值可能是 `TuiMouseDispatchResult`（已带 target）。

### 2.9 `src/components/alt-screen-flash.ts`（51 行）

```ts
export class AltScreenFlashContainer implements Component {
  constructor(requestRender: () => void);
  flash(message: string, durationMs = 1000): void;
  dispose(): void;
  invalidate(): void {}
  render(width: number): string[];
}
```
- `flash(message, durationMs)`：`id = nextId++`；`setTimeout(..., Math.max(0, durationMs))`，回调里按 id 查找，命中则 `splice` 移除并 `requestRender()`；`timer.unref()`；推入 `{id, message, timer}`；立即 `requestRender()`。
- `dispose()`：`clearTimeout` 全部并清空（**不**调用 `requestRender`）。
- `render(width)`：每个 entry 一行：
  **【逐字】** `` `\x1b[7m${truncateToWidth(` ${message} `, width, "")}\x1b[27m` ``
  即“消息两侧各一个空格，按 width 截断（省略号为空串），整体反显，末尾 `\x1b[27m` 关闭反显”。
- 使用方 `TuiAltScreen.compositeFlashes`：取 `render(width).slice(-height)`，逐行按 `flashWidth = visibleWidth(line)` 右对齐到 `width - flashWidth` 处合成；宽度 0 的行跳过。因此多条 flash 会**从屏幕最底行往上堆叠**。

### 2.10 `src/components/loader.ts`（101 行）

```ts
export interface LoaderIndicatorOptions {
  frames?: string[];      // 空数组 = 隐藏指示器
  intervalMs?: number;    // 动画帧间隔
}
export class Loader extends Text {
  constructor(ui: TUI, spinnerColorFn: (s: string) => string, messageColorFn: (s: string) => string,
              message: string = "Loading...", indicator?: LoaderIndicatorOptions);
  render(width: number): string[];    // ["", ...super.render(width)]
  start(): void;
  stop(): void;
  setMessage(message: string): void;
  override invalidate(): void;
  setIndicator(indicator?: LoaderIndicatorOptions): void;
  protected getRenderedIndicator(): string;
}
```

常量 **【逐字】**：
```ts
const DEFAULT_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEFAULT_INTERVAL_MS = 80;
```
默认消息 **【逐字】** `"Loading..."`。

行为：
- 构造：`super("", 1, 0)`（Text 的 paddingX=1、paddingY=0）；保存 `ui`、两个着色函数、`message`；最后 `setIndicator(indicator)`。
- `render(width)`：总是先返回一个空行 `""`，再是 `super.render(width)`。即 Loader 占“空行 + 内容行”。
- `start()`：`updateDisplay()` 后 `restartAnimation()`。
- `stop()`：清 `intervalId`（不重置帧）。
- `setMessage(m)`：保存并 `updateDisplay()`。
- `invalidate()`：`super.invalidate()` 后 `updateDisplay()`（主题变化后重算着色）。
- `setIndicator(indicator)`：
  - `renderIndicatorVerbatim = indicator !== undefined`（**只要传了对象就逐字使用帧、不再着色**）。
  - `frames = indicator?.frames !== undefined ? [...indicator.frames] : [...DEFAULT_FRAMES]`。
  - `intervalMs = indicator?.intervalMs && indicator.intervalMs > 0 ? indicator.intervalMs : DEFAULT_INTERVAL_MS`。
  - `currentFrame = 0`；`start()`。
- `restartAnimation()`：`stop()`；`frames.length <= 1` 直接返回（**单帧不建定时器**）；否则 `setInterval(每 intervalMs 前进一帧并 updateDisplay)`。定时器**未 unref**（与 AltScreenFlash/ScrollView 不同）。
- `getRenderedIndicator()`：`frame = frames[currentFrame] ?? ""`；`renderIndicatorVerbatim ? frame : spinnerColorFn(frame)`。
- `updateDisplay()`：`renderedFrame = getRenderedIndicator()`；`indicator = renderedFrame.length > 0 ? `${renderedFrame} ` : ""`；`setText(`${indicator}${messageColorFn(this.message)}`)`；`ui.requestRender()`。

**下游关键点**：`StatusIndicator.renderInBorder(width)` 调用 `super.render(width + 2)[1]`（取第二行，即内容行），并 `truncateToWidth(line.startsWith(" ") ? line.slice(1).trimEnd() : line.trimEnd(), width, "")`。Rust 侧必须保证 `Loader::render` 的第二行是内容行（含 Text 的左 padding 空格）。

### 2.11 `src/components/cancellable-loader.ts`（40 行）

```ts
export class CancellableLoader extends Loader {
  onAbort?: () => void;
  get signal(): AbortSignal;
  get aborted(): boolean;
  handleInput(data: string): void;
  dispose(): void;
}
```
- 内部 `AbortController`（Rust：`tokio_util::sync::CancellationToken` 或 `AbortHandle` 风格的共享标志，需能提供“取消回调”语义）。
- `handleInput(data)`：`getKeybindings().matches(data, "tui.select.cancel")` → `abort()` 后 `onAbort?.()`。其它输入忽略。
- `dispose()`：`stop()`（不停 aborted 状态）。

### 2.12 `src/components/select-list.ts`（273 行）

#### 常量

```ts
const DEFAULT_PRIMARY_COLUMN_WIDTH = 32;
const PRIMARY_COLUMN_GAP = 2;
const MIN_DESCRIPTION_WIDTH = 10;
```
辅助：`normalizeToSingleLine(text) = text.replace(/[\r\n]+/g, " ").trim()`；`clamp(v,min,max)`。

#### 类型

```ts
export interface SelectItem { value: string; label: string; description?: string; }
export interface SelectListTheme {
  selectedPrefix: (text: string) => string;   // 声明但渲染中未使用
  selectedText: (text: string) => string;
  description: (text: string) => string;
  scrollInfo: (text: string) => string;
  noMatch: (text: string) => string;
}
export interface SelectListTruncatePrimaryContext {
  text: string; maxWidth: number; columnWidth: number; item: SelectItem; isSelected: boolean;
}
export interface SelectListLayoutOptions {
  minPrimaryColumnWidth?: number;
  maxPrimaryColumnWidth?: number;
  truncatePrimary?: (context: SelectListTruncatePrimaryContext) => string;
}
```

#### `class SelectList implements Component`

字段：`items`、`filteredItems`（初值 = items，**同一数组引用**）、`selectedIndex = 0`、`mousePressedIndex?: number`、`maxVisible`（字段声明处有 `= 5` 初值，但构造**总会**用参数覆盖，因此参数是必填且无默认值）、`theme`、`layout`、公开回调 `onSelect?`、`onCancel?`、`onSelectionChange?`。`items` 也保存原数组引用（`setFilter` 从 `this.items` 过滤，因此构造后修改传入数组会影响过滤结果）。

构造：`constructor(items, maxVisible, theme, layout = {})`。

`setFilter(filter)`：`filteredItems = items.filter(item => item.value.toLowerCase().startsWith(filter.toLowerCase()))`（**按 `value` 前缀匹配，大小写不敏感**）；`selectedIndex = 0`。

`setSelectedIndex(index)`：`max(0, min(index, filteredItems.length - 1))`（空列表 → `-1`，注意负值）。

`invalidate()`：空。

`render(width)`：
1. `filteredItems.length === 0` → `[ theme.noMatch("  No matching commands") ]`（**【逐字】** 两空格 + `No matching commands`），提前返回。
2. `primaryColumnWidth = getPrimaryColumnWidth()`。
3. `{startIndex, endIndex} = getVisibleRange()`。
4. 对 `i ∈ [startIndex, endIndex)`：`isSelected = i === selectedIndex`；`descriptionSingleLine = item.description ? normalizeToSingleLine(item.description) : undefined`；`lines.push(renderItem(item, isSelected, width, descriptionSingleLine, primaryColumnWidth))`。
5. 若 `startIndex > 0 || endIndex < filteredItems.length`：**【逐字】** `scrollText = `  (${selectedIndex + 1}/${filteredItems.length})``（两空格开头），推入 `theme.scrollInfo(truncateToWidth(scrollText, width - 2, ""))`。
6. 返回 lines。

`renderItem(item, isSelected, width, descriptionSingleLine, primaryColumnWidth)`：
- `prefix = isSelected ? "→ " : "  "`（**【逐字】** 箭头 U+2192 + 空格 / 两空格）；`prefixWidth = visibleWidth(prefix) = 2`。
- 若 `descriptionSingleLine && width > 40`：
  - `effectivePrimaryColumnWidth = max(1, min(primaryColumnWidth, width - prefixWidth - 4))`
  - `maxPrimaryWidth = max(1, effectivePrimaryColumnWidth - PRIMARY_COLUMN_GAP)`
  - `truncatedValue = truncatePrimary(item, isSelected, maxPrimaryWidth, effectivePrimaryColumnWidth)`
  - `truncatedValueWidth = visibleWidth(truncatedValue)`
  - `spacing = " ".repeat(max(1, effectivePrimaryColumnWidth - truncatedValueWidth))`
  - `descriptionStart = prefixWidth + truncatedValueWidth + spacing.length`
  - `remainingWidth = width - descriptionStart - 2`（注释“// -2 for safety”）
  - 若 `remainingWidth > MIN_DESCRIPTION_WIDTH`：
    - `truncatedDesc = truncateToWidth(descriptionSingleLine, remainingWidth, "")`
    - 选中：`theme.selectedText(`${prefix}${truncatedValue}${spacing}${truncatedDesc}`)`
    - 未选中：`prefix + truncatedValue + theme.description(spacing + truncatedDesc)`
- 退化路径：`maxWidth = width - prefixWidth - 2`；`truncatedValue = truncatePrimary(item, isSelected, maxWidth, maxWidth)`；选中 → `theme.selectedText(prefix + truncatedValue)`；否则 `prefix + truncatedValue`。

`getPrimaryColumnWidth()`：`{min, max} = getPrimaryColumnBounds()`；`widestPrimary = max over filteredItems(visibleWidth(getDisplayValue(item)) + PRIMARY_COLUMN_GAP)`；返回 `clamp(widestPrimary, min, max)`（**空列表 → clamp(0, min, max) = min**）。

`getPrimaryColumnBounds()`：
```ts
rawMin = layout.minPrimaryColumnWidth ?? layout.maxPrimaryColumnWidth ?? 32;
rawMax = layout.maxPrimaryColumnWidth ?? layout.minPrimaryColumnWidth ?? 32;
return { min: max(1, min(rawMin, rawMax)), max: max(1, max(rawMin, rawMax)) };
```
即只给其中一个时两者相同（固定列宽）。

`truncatePrimary(item, isSelected, maxWidth, columnWidth)`：`displayValue = getDisplayValue(item)`；有 `layout.truncatePrimary` → 用其返回值，否则 `truncateToWidth(displayValue, maxWidth, "")`；**最后再 `truncateToWidth(结果, maxWidth, "")` 兜底**（自定义函数即使返回超宽也被截断）。

`getDisplayValue(item)`：`item.label || item.value`。

`notifySelectionChange()`：仅当当前项存在且回调存在时调用。

`getSelectedItem(): SelectItem | null`：`filteredItems[selectedIndex] ?? null`。

`getVisibleRange()`（私有）：
```ts
startIndex = max(0, min(selectedIndex - floor(maxVisible / 2), filteredItems.length - maxVisible));
endIndex = min(startIndex + maxVisible, filteredItems.length);
```
（选中项尽量居中；靠近两端时贴边。）

`handleMouse(event)`：
- `filteredItems.length === 0` → `undefined`。
- `type === "wheel" && wheelDelta`：`delta = wheelDelta < 0 ? -1 : 1`；`selectedIndex` 夹取后若变化 → `notifySelectionChange()`；返回 `{ handled: true, render: selectedIndex !== previousIndex }`。**没有“未变化也吞掉”以外的行为**（始终 handled）。
- 只处理 `button === "left"` 且 `type ∈ {press, click}`；否则 `undefined`（**hover 不选中**，注释说明可见区以选中项为中心）。
- `itemIndex = startIndex + event.y`；`itemIndex < startIndex || itemIndex >= endIndex` → `undefined`。
- `press`：`mousePressedIndex = itemIndex`；若 `selectedIndex !== itemIndex` → 更新并 `notifySelectionChange()`；返回 `{ handled: true, focus: true }`。
- `click`：`clickedIndex = mousePressedIndex ?? itemIndex`；清 `mousePressedIndex`；`changed = selectedIndex !== clickedIndex`；更新；若变 → `notifySelectionChange()`；取选中项并 `onSelect?.(item)`；返回 `{ handled: true }`。
- 其它 → `undefined`。

`handleInput(data)`：
- `tui.select.up`：`selectedIndex = selectedIndex === 0 ? filteredItems.length - 1 : selectedIndex - 1`（**循环**，空列表时变 -1）；`notifySelectionChange()`。
- `tui.select.down`：`selectedIndex === filteredItems.length - 1 ? 0 : selectedIndex + 1`；`notifySelectionChange()`。
- `tui.select.confirm`：`filteredItems[selectedIndex]` 存在且 `onSelect` → 调用。
- `tui.select.cancel`：`onCancel?.()`。
- 其余忽略。

**测试基准**（`select-list.test.ts`）：
- `normalizes multiline descriptions to single line`：`"Line one\nLine two\nLine three"` → 行内出现 `"Line one Line two Line three"`，且行内无 `\n`。
- `keeps descriptions aligned when the primary text is truncated`：两个条目的 description 起始可见列相同。
- `uses the configured minimum primary column width`：`{min:12,max:20}` + 短标签 → `rendered[0].indexOf("first") === 14`（= 2 prefix + 12 列），`rendered[1].indexOf("second") === 14`。
- `uses the configured maximum primary column width`：长标签 → `visibleIndexOf("first") === 22`（= 2 + 20）。
- `allows overriding primary truncation while preserving description alignment`：自定义 `truncatePrimary` 用 `…`（U+2026）截断，两行 description 对齐。

`mouse-components.test.ts` 基准：`selects and activates list rows`（maxVisible=3，5 项；`press` 在 y=2 选中 `"c"`，`click` 触发 `onSelect("c")`）；`ignores hover and clicks visible select-list row {0,4} after scrolling`（滚动后可见行索引与 event.y 的换算必须正确）。

### 2.13 `src/components/settings-list.ts`（328 行）

#### 类型

```ts
export interface SettingItem {
  id: string;
  label: string;
  description?: string;
  currentValue: string;
  values?: string[];              // 有则 Enter/Space 循环取值
  submenu?: (currentValue: string,
             done: (selectedValue?: string, options?: { navigateTo?: string }) => void) => Component;
}
export interface SettingsListTheme {
  label: (text: string, selected: boolean) => string;
  value: (text: string, selected: boolean) => string;
  description: (text: string) => string;
  cursor: string;                 // 选中行的前缀字符串（不是函数）
  hint: (text: string) => string;
}
export interface SettingsListOptions { enableSearch?: boolean; }
```

#### `class SettingsList implements Component`

构造：`constructor(items, maxVisible, theme, onChange: (id, newValue) => void, onCancel: () => void, options = {})`。`searchEnabled = options.enableSearch ?? false`；开启时创建 `searchInput = new Input()`（**默认 prompt `"> "`、无 placeholder**）。

状态：`items`、`filteredItems`（初值 = items）、`selectedIndex = 0`、`mousePressedIndex?`、`maxVisible`、`submenuComponent: Component | null`、`submenuItemIndex: number | null`、`navigateAfterClose: string | null`。

`updateValue(id, newValue)`：找到 `items` 中 `id` 相同项并改 `currentValue`（**只改 items，不动 filteredItems 的引用关系**——filteredItems 是同一批对象引用，因此同步生效）。

`selectItem(id)`：候选列表为 `searchEnabled ? filteredItems : items`；`findIndex` 命中则设 `selectedIndex`；未命中无操作。

`invalidate()`：`submenuComponent?.invalidate?.()`（自身无缓存）。

`render(width)`：有 submenu → `submenuComponent.render(width)`；否则 `renderMainList(width)`。

`renderMainList(width)`：
1. 若 `searchEnabled && searchInput`：推入 `searchInput.render(width)` 的全部行，再推入 `""`。（因此**搜索框占 1 行 + 1 空行**；`Input.render` 通常 1 行。）
2. `items.length === 0`：推入 `theme.hint("  No settings available")`；若启用搜索再 `addHintLine`；返回。
3. `displayItems = getDisplayItems()`；为空：推入 `truncateToWidth(theme.hint("  No matching settings"), width)`，`addHintLine`，返回。
4. `{startIndex, endIndex} = getVisibleRange(displayItems)`（同样的居中夹取算法，`maxVisible`）。
5. `maxLabelWidth = min(36, max over this.items(visibleWidth(item.label)))`（**注意用的是 `items` 全量而非 displayItems**；空 items 时该步已被步骤 2 排除）。
6. 逐行（`i ∈ [startIndex, endIndex)`）：
   - `isSelected = i === selectedIndex`；
   - `prefix = isSelected ? theme.cursor : "  "`（**theme.cursor 是字符串**，不是函数；未选中是两个空格）；`prefixWidth = visibleWidth(prefix)`；
   - `labelPadded = item.label + " ".repeat(max(0, maxLabelWidth - visibleWidth(item.label)))`；
   - `labelText = theme.label(labelPadded, isSelected)`；
   - `separator = "  "`；`usedWidth = prefixWidth + maxLabelWidth + 2`；`valueMaxWidth = width - usedWidth - 2`；
   - `valueText = theme.value(truncateToWidth(item.currentValue, valueMaxWidth, ""), isSelected)`；
   - 推入 `truncateToWidth(prefix + labelText + separator + valueText, width)`（**整行最终再截断一次**；注意 `labelText`/`valueText` 已含 ANSI，`truncateToWidth` 会按可见宽度并补 `\x1b[0m`）。
7. 若 `startIndex > 0 || endIndex < displayItems.length`：**【逐字】** `scrollText = `  (${selectedIndex + 1}/${displayItems.length})``，推入 `theme.hint(truncateToWidth(scrollText, width - 2, ""))`。
8. 选中项有 `description`：推入 `""`；`wrappedDesc = wrapTextWithAnsi(description, width - 4)`；每行推入 `theme.description(`  ${line}`)`（两空格缩进）。
9. `addHintLine(lines, width)`。

`addHintLine(lines, width)`：
```ts
lines.push("");
lines.push(truncateToWidth(theme.hint(
  searchEnabled ? "  Type to search · Enter/Space to change · Esc to cancel"
                : "  Enter/Space to change · Esc to cancel"), width));
```
**【逐字】** 注意分隔符是 U+00B7 两侧各一个空格（`" · "`）。

`handleMouse(event)`：
- submenu 存在：`submenuComponent.handleMouse?.(event)`；有结果 → `{...result, focus: true}`；否则 `undefined`。
- 搜索框行：`event.y === 0` → 交给 `searchInput.handleMouse`，结果加 `focus: true`；`event.y === 1` → `undefined`（空行）。
- `displayItems` 为空 → `undefined`。
- `wheel`：同 SelectList（±1，夹取，`render` 仅变化时为 true，始终 handled）。
- 只处理 `left` + `press/click`。
- `rowOffset = searchEnabled ? 2 : 0`；`itemIndex = startIndex + event.y - rowOffset`；越界 → `undefined`。
- `press`：`mousePressedIndex = itemIndex`；`selectedIndex = itemIndex`（**无条件赋值**）；返回 `{ handled: true, focus: true }`。
- `click`：`selectedIndex = mousePressedIndex ?? itemIndex`；清；`activateItem()`；返回 `{ handled: true }`。

`handleInput(data)`：
1. submenu 存在 → 全部转发给 `submenuComponent.handleInput?.(data)` 并返回（子菜单的 Escape 会触发 `done()` 关闭）。
2. `kb.matches(data, "tui.select.up")`：displayItems 为空则 return；`selectedIndex = selectedIndex === 0 ? len - 1 : selectedIndex - 1`。
3. `tui.select.down`：对称。
4. `tui.select.confirm` **或**（`data === " "` 且（未启用搜索 或 `searchInput.getValue().length === 0`））→ `activateItem()`。
   注意：**空格在搜索框为空时是“切换值”快捷键；一旦搜索框有内容，空格进入搜索词**（测试 `includes spaces in an active search instead of changing the selected setting`、`keeps Space as a change shortcut before a search query is entered`）。
5. `tui.select.cancel` → `onCancel()`。
6. 否则若启用搜索：`searchInput.handleInput(data)` 后 `applyFilter(searchInput.getValue())`。

`getDisplayItems()`：`searchEnabled ? filteredItems : items`。

`activateItem()`：
- `item = getDisplayItems()[selectedIndex]`，无则 return。
- 有 `submenu`：`submenuItemIndex = selectedIndex`；`submenuComponent = item.submenu(item.currentValue, (selectedValue, options) => { if (selectedValue !== undefined) { item.currentValue = selectedValue; onChange(item.id, selectedValue); } if (options?.navigateTo) navigateAfterClose = options.navigateTo; closeSubmenu(); })`。
- 否则若 `item.values?.length > 0`：`currentIndex = values.indexOf(currentValue)`；`nextIndex = (currentIndex + 1) % values.length`；写入 `item.currentValue` 并 `onChange(item.id, newValue)`。**`currentValue` 不在 values 中时 `indexOf = -1`，下一项为 `values[0]`。**

`closeSubmenu()`：
- `submenuComponent = null`。
- 若 `navigateAfterClose !== null`：记下 id、清空 `navigateAfterClose` 与 `submenuItemIndex`、`selectItem(id)`、`activateItem()`（**自动打开目标项的子菜单**）。
- 否则若 `submenuItemIndex !== null`：`selectedIndex = submenuItemIndex`；清空。

`applyFilter(query)`：`filteredItems = fuzzyFilter(items, query, item => item.label)`；`selectedIndex = 0`。（`fuzzyFilter` 见 tui-core：按 `[\s/]+` 分词、全部 token 都要模糊匹配、分数升序，稳定排序。）

`getVisibleRange(displayItems)`：同 SelectList 的居中夹取算法。

**测试基准**：`settings-list.test.ts` 两个用例（空格语义）。`mouse-components.test.ts`：`activates settings rows`（maxVisible=3、4 项、`press` y=2 → 第三项，`click` → 值从 `"low"` 循环到 `"high"`，`onChange` 恰好一次）；`ignores hover and clicks visible settings row {0,4} after scrolling`；`keeps a settings list focused when a click in its submenu closes the submenu`（依赖 `handleMouse` 的 `focus: true` 与 `dispatchMouseEvent` 的 `focusTarget` 规则）。

### 2.14 `src/components/input.ts`（501 行）— 单行输入

#### 类型

```ts
export interface InputOptions {
  prompt?: string;                       // 默认 "> "
  placeholder?: string;                  // 默认 ""
  placeholderStyle?: (text: string) => string;  // 默认恒等
}
```

#### `class Input implements Component, Focusable`

字段：`value = ""`、`cursor = 0`（UTF-16 索引）、`prompt`、`placeholder`、`placeholderStyle`、`renderedStartColumn = 0`（滑动窗口起点，供鼠标列换算）、公开 `onSubmit?`、`onEscape?`、`focused = false`、粘贴缓冲 `pasteBuffer = ""`/`isInPaste = false`、`killRing = new KillRing()`、`lastAction: "kill" | "yank" | "type-word" | null = null`、`undoStack = new UndoStack<InputState>()`（`InputState = { value: string; cursor: number }`）。

`getValue()` / `setValue(value)`（`cursor = min(cursor, value.length)`；**不推 undo、不重置 lastAction**）。

`handleInput(data)` 分派顺序（**顺序即语义**）：

1. **bracketed paste**：若 `data.includes("\x1b[200~")` → `isInPaste = true`；`pasteBuffer = ""`；`data = data.replace("\x1b[200~", "")`（只替换首个）。
2. 若 `isInPaste`：`pasteBuffer += data`；找 `"\x1b[201~"`：
   - 找到：`pasteContent = buffer.slice(0, endIndex)`；`handlePaste(pasteContent)`；`isInPaste = false`；`remaining = buffer.slice(endIndex + 6)`（6 = `\x1b[201~` 长度）；清空 buffer；`remaining` 非空则**递归** `handleInput(remaining)`。无论是否找到都 `return`。
3. `tui.select.cancel` → `onEscape?.()`；return。
4. `tui.editor.undo` → `undo()`；return。
5. `tui.input.submit` **或** `data === "\n"` → `onSubmit?.(value)`；return。（Input **没有**反斜杠+回车换行技巧，测试 `submits value including backslash on Enter` 断言提交 `"hello\\"`。）
6. 删除：`tui.editor.deleteCharBackward` → `handleBackspace()`；`deleteCharForward` → `handleForwardDelete()`；`deleteWordBackward` → `deleteWordBackwards()`；`deleteWordForward` → `deleteWordForward()`；`deleteToLineStart` → `deleteToLineStart()`；`deleteToLineEnd` → `deleteToLineEnd()`（各自 return）。
7. `tui.editor.yank` → `yank()`；`tui.editor.yankPop` → `yankPop()`。
8. 光标：`cursorLeft`/`cursorRight`/`cursorLineStart`/`cursorLineEnd`/`cursorWordLeft`/`cursorWordRight`，其中左右与行首行尾先 `lastAction = null`。左右按 **grapheme** 步进（`[...segmenter.segment(beforeCursor)]` 取最后/最前一个，减/加其 `segment.length`）。
9. `decodeKittyPrintable(data)` 非 undefined → `insertCharacter(printable)`；return。（注释：Kitty flag 1 会对所有可打印键发 CSI-u，必须在控制字符检查之前解码。）
10. 控制字符过滤：`[...data].some(ch => code < 32 || code === 0x7f || (0x80 ≤ code ≤ 0x9f))` 为 false 时 `insertCharacter(data)`；否则丢弃。

`handleMouse(event)`：
- 仅 `type === "press" && button === "left" && y === 0`；否则 `undefined`。
- `visibleColumn = max(0, event.x - 2)`（**硬编码 2 = prompt 宽度假设**）。
- `targetColumn = renderedStartColumn + visibleColumn`。
- 从 `cursor = value.length` 起（默认落在末尾），遍历 grapheme：`nextColumn = currentColumn + visibleWidth(grapheme)`；若 `targetColumn < nextColumn` → `cursor = grapheme.index` 并 break；否则前进。**点击超出文本长度则停在末尾**。
- `lastAction = null`；返回 `{ handled: true, focus: true }`。

`insertCharacter(char)`：
- 撤销合并：`if (isWhitespaceChar(char) || lastAction !== "type-word") pushUndo()`。
- `lastAction = "type-word"`。
- 在 cursor 处插入，`cursor += char.length`（**不推 onChange**——Input 无 onChange）。

`handleBackspace()`：`lastAction = null`；`cursor > 0` 时 `pushUndo()`，按 grapheme 删除光标前一个簇。
`handleForwardDelete()`：`lastAction = null`；`cursor < value.length` 时 `pushUndo()`，按 grapheme 删除光标处一个簇（光标不动）。
`deleteToLineStart()`：`cursor === 0` 直接 return；`pushUndo()`；`deleted = value.slice(0, cursor)`；`killRing.push(deleted, { prepend: true, accumulate: lastAction === "kill" })`；`lastAction = "kill"`；`value = value.slice(cursor)`；`cursor = 0`。
`deleteToLineEnd()`：`cursor >= value.length` 直接 return；对称（`prepend: false`，`value = value.slice(0, cursor)`）。
`deleteWordBackwards()`：
- `cursor === 0` return；`wasKill = lastAction === "kill"`；`pushUndo()`；记 `oldCursor`；`moveWordBackwards()` → `deleteFrom`；`cursor = oldCursor`；`deleted = value.slice(deleteFrom, cursor)`；`killRing.push(deleted, {prepend:true, accumulate:wasKill})`；`lastAction="kill"`；删除区间；`cursor = deleteFrom`。
`deleteWordForward()`：`cursor >= length` return；对称（`prepend:false`，`deleted = value.slice(cursor, deleteTo)`）。
`yank()`：ring 空 → return；`pushUndo()`；插入 `peek()`；`cursor += text.length`；`lastAction = "yank"`。
`yankPop()`：`lastAction !== "yank" || killRing.length <= 1` → return；`pushUndo()`；`prevText = peek() ?? ""`；先从光标前删掉 `prevText.length` 个字符；`killRing.rotate()`；插入新 `peek()`；`cursor += text.length`；`lastAction = "yank"`。
`pushUndo()`：`undoStack.push({value, cursor})`（`UndoStack` 内部 `structuredClone`）。
`undo()`：`pop()` 无则 return；恢复 `value`/`cursor`；`lastAction = null`。
`moveWordBackwards()`/`moveWordForwards()`：`cursor === 0` / `cursor >= length` 直接 return；`lastAction = null`；`cursor = findWordBackward(value, cursor)` / `findWordForward(...)`（**不带 atomic 选项**；word-navigation 见 tui-core，按 `Intl.Segmenter` 词边界 + `PUNCTUATION_REGEX` 保留 ASCII 标点边界）。
`handlePaste(pastedText)`：`lastAction = null`；`pushUndo()`；清洗：`\r\n`→`""`、`\r`→`""`、`\n`→`""`、`\t`→`"    "`（**四空格**；即粘贴内容被压成单行）；插入并 `cursor += cleanText.length`。

`invalidate()`：空。

`render(width)`：
1. `availableWidth = width - visibleWidth(prompt)`。
2. `availableWidth <= 0` → `[truncateToWidth(prompt, width, "")]`。
3. **placeholder 分支**（`value.length === 0 && placeholder`）：
   - **注意：该分支不重置 `renderedStartColumn`**（普通分支会在开头置 0 并在滚动时改写）。因此“先渲染过长文本（写入非 0 的 `renderedStartColumn`）→ 再清空 value 使 placeholder 生效 → 鼠标点击”时，`handleMouse` 会带**过期的滚动偏移**换算列，落点偏右。这是既有实现细节，**Rust 侧应原样保留**（与 §8.5 的 `x - 2` 同类），不要在 placeholder 分支补 `renderedStartColumn = 0`。
   - `placeholder = truncateToWidth(this.placeholder, availableWidth, "")`；
   - `graphemes = [...segmenter.segment(placeholder)]`；`atCursor = graphemes[0]?.segment ?? " "`；`afterCursor = placeholder.slice(atCursor.length)`；
   - `marker = focused ? CURSOR_MARKER : ""`；
   - `cursorChar = renderFakeCursor(placeholderStyle(atCursor))`；
   - `textWithCursor = marker + cursorChar + placeholderStyle(afterCursor)`；
   - `padding = " ".repeat(max(0, availableWidth - visibleWidth(textWithCursor)))`；
   - 返回 `[prompt + textWithCursor + padding]`。
4. 普通分支：
   - `visibleText = ""`；`cursorDisplay = cursor`；`renderedStartColumn = 0`；`totalWidth = visibleWidth(value)`。
   - `totalWidth < availableWidth` → 全部显示。
   - 否则横向滚动：
     - `scrollWidth = cursor === value.length ? availableWidth - 1 : availableWidth`（光标在末尾时预留 1 列）。
     - `cursorCol = visibleWidth(value.slice(0, cursor))`。
     - `scrollWidth > 0`：
       - `halfWidth = floor(scrollWidth / 2)`；
       - `cursorCol < halfWidth` → `startCol = 0`；
       - `cursorCol > totalWidth - halfWidth` → `startCol = max(0, totalWidth - scrollWidth)`；
       - 否则 `startCol = max(0, cursorCol - halfWidth)`；
       - `renderedStartColumn = startCol`；`visibleText = sliceByColumn(value, startCol, scrollWidth, true)`；
       - `beforeCursor = sliceByColumn(value, startCol, max(0, cursorCol - startCol), true)`；`cursorDisplay = beforeCursor.length`。
     - 否则 `visibleText = ""`；`cursorDisplay = 0`。
   - 构造行：
     - `graphemes = [...segmenter.segment(visibleText.slice(cursorDisplay))]`；`atCursor = graphemes[0]?.segment ?? " "`；
     - `beforeCursor = visibleText.slice(0, cursorDisplay)`；`afterCursor = visibleText.slice(cursorDisplay + atCursor.length)`；
     - `marker = focused ? CURSOR_MARKER : ""`；`cursorChar = renderFakeCursor(atCursor)`；
     - `textWithCursor = beforeCursor + marker + cursorChar + afterCursor`；
     - `padding = " ".repeat(max(0, availableWidth - visibleWidth(textWithCursor)))`；
     - 返回 `[prompt + textWithCursor + padding]`。

**宽度保证**：`width - visibleWidth(prompt) - visibleWidth(textWithCursor)` 可能因宽字符边界而为 0，padding 取 `max(0, ...)`，因此行宽 ≤ width。测试 `does not overflow with wide CJK and fullwidth text`（width 93，三条长 CJK/Korean/Japanese 文本）与 `keeps the cursor visible when horizontally scrolling wide text` 覆盖。测试 `supports a custom prompt and styled placeholder`：`prompt=""`、placeholder `"Find transcript"`、`placeholderStyle` 为 `\x1b[2m...\x1b[22m`；`focused = true` 时行内含 `\x1b[2m`，去掉转义并 `trimEnd()` 后等于 `"Find transcript"`；输入 `"n"` 后等于 `"n"`。

### 2.15 `src/components/editor.ts`（2473 行）— 多行编辑器（本单元最大件）

#### 2.15.1 导出

```ts
export interface TextChunk { text: string; startIndex: number; endIndex: number; }
export function wordWrapLine(line: string, maxWidth: number, preSegmented?: Intl.SegmentData[]): TextChunk[];
export interface EditorTheme { borderColor: (str: string) => string; selectList: SelectListTheme; }
export interface EditorOptions { paddingX?: number; autocompleteMaxVisible?: number; }
export class Editor implements Component, Focusable
```

#### 2.15.2 模块级常量/工具

```ts
const PASTE_MARKER_REGEX = /\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]/g;       // 全局
const PASTE_MARKER_SINGLE = /^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$/;    // 单段测试
function isPasteMarker(segment: string): boolean {
  return segment.length >= 10 && PASTE_MARKER_SINGLE.test(segment);
}
```
**【逐字】** marker 形式：`[paste #<id> +<n> lines]` 或 `[paste #<id> <n> chars]`。

`segmentWithMarkers(text, baseSegmenter, validIds)`：
- 快速路径：`validIds.size === 0 || !text.includes("[paste #")` → 直接返回 `baseSegmenter.segment(text)`。
- 用 `PASTE_MARKER_REGEX` 的 `matchAll` 找所有 id ∈ `validIds` 的 marker 区间 `{start, end}`（`m.index` 为 UTF-16 偏移）。
- 无 marker → 返回 base 分段。
- 否则遍历 base 分段：跳过完全位于当前分段之前的 marker（`marker.end <= seg.index`）；若 `seg.index ∈ [marker.start, marker.end)`：仅当 `seg.index === marker.start` 时产出一个合并段 `{ segment: text.slice(start, end), index: start, input: text }`，其余丢弃；否则原样保留 base 分段。
- 结果：**有效 paste marker 成为单个原子 grapheme 段**（光标移动、删除、换行、词移动都视其为不可分割单位）。

`SLASH_COMMAND_SELECT_LIST_LAYOUT = { minPrimaryColumnWidth: 12, maxPrimaryColumnWidth: 32 }`（`SelectListLayoutOptions`）。

```ts
const ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS = 20;
const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS = ["@", "#"];
const unquotedAutocompleteSuffixRegex = new RegExp(`(?:(?!${autocompleteSeparatorRegex.source}).)*`, "u");
const autocompleteTokenStartSource = `${autocompleteBoundaryRegex.source}[([{<\`]*`;
function escapeCharacterClass(value) { return value.replace(/[\\^$.*+?()[\]{}|-]/g, "\\$&"); }
function buildTriggerPattern(triggerCharacters) {
  return new RegExp(
    `${autocompleteTokenStartSource}(?:@"[^"]*|[${triggerCharacters.map(escapeCharacterClass).join("")}]${unquotedAutocompleteSuffixRegex.source})$`,
    "u");
}
function buildDebouncePattern(triggerCharacters) {
  const escapedWithoutAt = triggerCharacters.filter(c => c !== "@").map(escapeCharacterClass);
  return new RegExp(
    `${autocompleteTokenStartSource}(?:@(?:"[^"]*|${unquotedAutocompleteSuffixRegex.source})|[${escapedWithoutAt.join("")}]${unquotedAutocompleteSuffixRegex.source})$`,
    "u");
}
```
语义（测试密集覆盖）：
- **trigger** 用于“刚输入触发字符时是否立刻请求”：token 起点 = 行首或分隔符（空白或 CJK 标点）之后，可跟 0..n 个 `([{<` 与反引号；随后是 `@"…"`（引号包裹路径，内部允许空白与 CJK 标点）**或** `[触发字符]` + 0..n 个非分隔符字符。
- **debounce** 用于“打字中途是否进入 20ms 防抖”：`@` 分支写作 `@(?:"[^"]*|<非分隔符>*)`（引号或裸路径），其它触发字符写成 `[非@触发字符]<非分隔符>*`。
- 注意两者的匹配语言**几乎等价**（`unquotedAutocompleteSuffixRegex` 用 `*`，所以刚敲下 `#` / `@` 且后缀为空时两者都匹配）；真正区别是：① debounce 的字符类排除了 `@`（由显式 `@` 分支覆盖，避免 `@` 进入“其它字符”分支时语义含糊）；② trigger 的引号形式写作 `@"[^"]*`，debounce 写作 `@(?:"[^"]*|…)`。移植时应**保留这两个独立正则**（`setAutocompleteTriggerCharacters` 会同时重建二者），不要合并，否则测试 `debounces @ autocomplete while typing` / `debounces # autocomplete while typing` / `debounces custom triggerCharacters autocomplete while typing` 的调用次数与时机可能偏离。
- 两者都要求 `autocompleteSeparatorRegex`（`(?:\s|CJK 标点)`）与 `autocompleteBoundaryRegex`（`(?:^|分隔符)`）来自 tui-core，**正则 dialect 差异是移植硬点**（JS 的 `\p{Punctuation}` 与 lookahead 组合、`--` 集合差运算在 `v` flag 中）。

`createScrollBorder(direction: "↑" | "↓", hiddenLineCount: number, width: number): string`：
```ts
availableWidth = max(0, width);
label = ` ${direction} ${hiddenLineCount} more `;        // 【逐字】前后各一空格
labelWidth = visibleWidth(label);
if (labelWidth + 2 <= availableWidth) {
  leftWidth = floor((availableWidth - labelWidth) / 2);
  return "─".repeat(leftWidth) + label + "─".repeat(availableWidth - leftWidth - labelWidth);
}
indicator = `─── ${direction} ${hiddenLineCount} more `;  // 【逐字】三横 + 空格
remaining = availableWidth - visibleWidth(indicator);
if (remaining >= 0) return indicator + "─".repeat(remaining);
ellipsis = "...".slice(0, availableWidth);
indicatorWidth = availableWidth - visibleWidth(ellipsis);
return sliceByColumn(indicator, 0, indicatorWidth, true) + ellipsis;
```
测试基准（`editor.test.ts` → `Scroll indicators`）：
- width 40、20 行文本、上移 10 次：顶边去色后 `"─────────────── ↑ 9 more ───────────────"`（两侧各 15 个 `─`），底边 `"─────────────── ↓ 4 more ───────────────"`。
- width 10：顶边匹配 `/^─── ↑/`，底边 `/^─── ↓/`，整行被 borderColor 完整包裹（`\x1b[35m...\x1b[39m`），且**所有行 visibleWidth === width**。

#### 2.15.3 `wordWrapLine(line, maxWidth, preSegmented?)`

1. `!line || maxWidth <= 0` → `[{ text: "", startIndex: 0, endIndex: 0 }]`。
2. `visibleWidth(line) <= maxWidth` → `[{ text: line, startIndex: 0, endIndex: line.length }]`（**UTF-16 长度**）。
3. `segments = preSegmented ?? [...graphemeSegmenter.segment(line)]`。
4. 状态：`currentWidth = 0`、`chunkStart = 0`、`wrapOppIndex = -1`、`wrapOppWidth = 0`。
5. 对每个 `seg`（`grapheme`、`gWidth`、`charIndex = seg.index`、`isWs = !isPasteMarker(grapheme) && isWhitespaceChar(grapheme)`）：
   - **溢出检查（在前进之前）**：若 `currentWidth + gWidth > maxWidth`：
     - 若 `wrapOppIndex >= 0 && currentWidth - wrapOppWidth + gWidth <= maxWidth`：回退到换行机会 —— `push({text: line.slice(chunkStart, wrapOppIndex), startIndex: chunkStart, endIndex: wrapOppIndex})`；`chunkStart = wrapOppIndex`；`currentWidth -= wrapOppWidth`。
     - 否则若 `chunkStart < charIndex`：强制在当前位置断行 —— `push({text: line.slice(chunkStart, charIndex), ...})`；`chunkStart = charIndex`；`currentWidth = 0`。
     - `wrapOppIndex = -1`。
   - **超宽原子段**：若 `gWidth > maxWidth`（例如窄终端里的 paste marker）→ 递归 `subChunks = wordWrapLine(grapheme, maxWidth)`；除最后一段外全部按 `charIndex + sc.startIndex/endIndex` 推入；`chunkStart = charIndex + last.startIndex`；`currentWidth = visibleWidth(last.text)`；`wrapOppIndex = -1`；`continue`（该段对编辑逻辑仍是原子的，只是视觉上被切分）。
   - `currentWidth += gWidth`。
   - **记录换行机会**：`next = segments[i+1]`；
     - `isWs && next && (isPasteMarker(next.segment) || !isWhitespaceChar(next.segment))` → `wrapOppIndex = next.index; wrapOppWidth = currentWidth`（连续空白合并，断点在最后一个空白之后；贴 marker 的空白也可断）。
     - 否则若 `!isWs && next && !isWhitespaceChar(next.segment)`：`isCjk = !isPasteMarker(grapheme) && cjkBreakRegex.test(grapheme)`；`nextIsCjk` 同理；任一为真 → `wrapOppIndex = next.index; wrapOppWidth = currentWidth`（CJK 可在任意相邻字符间断行）。
6. 收尾 `push({ text: line.slice(chunkStart), startIndex: chunkStart, endIndex: line.length })`。
7. 返回 chunks。

**测试基准**（`Word wrapping`，`editor.test.ts:877-1199`，共 22 个 `it`；其中 13 条含字面量 `chunks[i].text` 断言，其余 9 条断言“每段宽 <= maxWidth + 重建等于原文”）：
- `("hello world test", 11)` → 2 段：`"hello "`、`"world test"`（`"world"` 恰好在第 11 列结束也换行，尾随空格留在第一段）。
- `("hello world test", 12)` → `"hello world "`、`"test"`。
- `("aaaaaaaaaaaa aaaa", 12)` → `"aaaaaaaaaaaa"`、`" aaaa"`（**第二段保留前导空格**）。
- `("      aaaaaaaaaaaa", 12)` → `"      "`、`"aaaaaaaaaaaa"`。
- `("Lorem ipsum dolor sit amet,    consectetur", 30)` → `"Lorem ipsum dolor sit "`、`"amet,    consectetur"`。
- `("Lorem ipsum dolor sit amet,              consectetur", 30)`（14 空格）→ 2 段：`"Lorem ipsum dolor sit "`、`"amet,              consectetur"`。
- `("Lorem ipsum dolor sit amet,               consectetur", 30)`（15 空格）→ 3 段：`"Lorem ipsum dolor sit "`、`"amet,               "`（15 空格全留）、`"consectetur"`。
- `("Lorem ipsum dolor sit amet,                         consectetur", 30)`（25 空格）→ 3 段：`"Lorem ipsum dolor sit "`、`"amet,                         "`（25 空格）、`"consectetur"`。
- `("Lorem ipsum dolor sit amet,                          consectetur", 30)`（26 空格）→ 3 段：`"Lorem ipsum dolor sit "`、`"amet,                         "`（25 空格）、`" consectetur"`（**多出的 1 空格进入第三段**）。
- `("Lorem ipsum dolor sit amet,                                     consectetur", 30)`（37 空格）→ 3 段：`"Lorem ipsum dolor sit "`、`"amet,                         "`（25 空格）、`"            consectetur"`（12 空格）。
- `(" " + "a"*186 + "你", 187)`（`force-breaks when wide char after word boundary wrap still overflows`）→ 所有 chunk `visibleWidth <= 187`，且 `chunks.map(c => line.slice(c.startIndex, c.endIndex)).join("") === line`（**每个用例都做这个重建断言**）。
- 5 个预分段（`Intl.SegmentData[]`）用例，均 `maxWidth = 10`、marker 为 `"[paste #1 +20 lines]"`（21 字符），断言每个 chunk `visibleWidth <= 10` + 重建等于原文：
  - `A${marker}B`（中间）→ 仅宽度不变量；
  - `${marker}B`（起始）→ 最后一个 chunk 含 `"B"`；
  - `A${marker}`（结尾）→ `chunks[0].text === "A"`；
  - `${m1}${m2}`（连续两个 21 字符 marker）→ 仅宽度不变量；
  - `${marker} hello world`（其后恢复正常换行）→ **最后一个 chunk 恰好是 `"world"`**。
- 6 个渲染层用例（经 `Editor.render`）：`wraps at word boundaries instead of mid-word`、`does not start lines with leading whitespace after word wrap`、`breaks long words (URLs) at character level`、`preserves multiple spaces within words on same line`（`width=50`，`setText("Word1   Word2    Word3")`，`lines[1]` 去 ANSI 后 `trim()` 仍含 `"Word1   Word2"`）、`handles empty string`（`width=40` → 恰好 3 行）、`handles single word that fits exactly`（`width = 10 + 1`，`setText("1234567890")` → 恰好 3 行且 `lines[1]` 去 ANSI 含 `"1234567890"`）。
- 同族但属于 `Paste marker atomic behavior` describe 的回归用例（`editor.test.ts:4124`）：`wordWrapLine re-checks overflow after backtracking to wrap opportunity` —— 输入 `" " + "b"*35` + 27 行粘贴 marker + `"bbbb"`，`render(54)` 后所有行 `visibleWidth <= 54`（防止回溯后二次 force-break 缺失导致溢出崩溃）。

#### 2.15.4 内部类型

```ts
interface EditorState { lines: string[]; cursorLine: number; cursorCol: number; }
interface EditorSnapshot { state: EditorState; pastes: Map<number, string>; pasteCounter: number; }
interface LayoutLine { text: string; hasCursor: boolean; cursorPos?: number; }
```

#### 2.15.5 `class Editor` 字段与构造

```ts
focused = false;
protected tui: TUI;
private theme: EditorTheme;
private paddingX = 0;
private lastWidth = 80;
private renderedVisibleLineCount = 1;
private renderedAutocompleteHeight = 0;
private scrollOffset = 0;
public borderColor: (str: string) => string;      // 初值 = theme.borderColor
private autocompleteProvider?: AutocompleteProvider;
private autocompleteTriggerCharacters = ["@", "#"];
private autocompleteTriggerPattern / autocompleteDebouncePattern (由上面构造函数生成)
private autocompleteList?: SelectList;
private autocompleteState: "regular" | "force" | null = null;
private autocompletePrefix = "";
private autocompleteMaxVisible = 5;
private autocompleteAbort?: AbortController;
private autocompleteDebounceTimer?: Timeout;
private autocompleteRequestTask: Promise<void> = Promise.resolve();
private autocompleteStartToken = 0;
private autocompleteRequestId = 0;
private pastes = new Map<number, string>();
private pasteCounter = 0;
private pasteBuffer = ""; private isInPaste = false;
private history: string[] = []; private historyIndex = -1; private historyDraft: EditorState | null = null;
private killRing = new KillRing(); private lastAction: "kill" | "yank" | "type-word" | null = null;
private jumpMode: "forward" | "backward" | null = null;
private preferredVisualCol: number | null = null;
private snappedFromCursorCol: number | null = null;
private undoStack = new UndoStack<EditorSnapshot>();
public onSubmit?: (text: string) => void;
public onChange?: (text: string) => void;
public disableSubmit = false;
```

构造 `constructor(tui, theme, options = {})`：
- `paddingX = options.paddingX ?? 0`，`Number.isFinite` 则 `max(0, floor(...))` 否则 0。
- `autocompleteMaxVisible = options.autocompleteMaxVisible ?? 5`；`Number.isFinite` 则 `max(3, min(20, floor(...)))` 否则 5。
- `borderColor = theme.borderColor`。

**私有辅助（规格此前未列，但多处行为依赖）**：
- `validPasteIds(): Set<number>` = `new Set(this.pastes.keys())`。
- `segment(text, mode: "word" | "grapheme"): Iterable<Intl.SegmentData>` = `segmentWithMarkers(text, mode === "word" ? wordSegmenter : graphemeSegmenter, this.validPasteIds())`。**编辑器内所有光标/删除/换行/鼠标命中/词移动的 grapheme 迭代都应走它**（10 处调用点：`render` 的首个字形与 `layoutText` 的换行、`handleMouse`、`handleBackspace`、`handleForwardDelete`、`insertTextAtCursorInternal` 的两处、`moveCursor` 的左右、`moveToVisualLine` 的吸附、`moveWordBackwards/Forwards`），因此**有效 paste marker 在所有编辑路径上都是原子单位**；无效 id 的“形似 marker”文本按普通 grapheme 处理。
- `isEditorEmpty(): boolean` = `lines.length === 1 && lines[0] === ""`。仅用于 `cursorUp` 的历史分支判定（见 2.15.9 M）。**注意它与 `layoutText` 的空判定略有不同**：`layoutText` 用 `lines.length === 0 || (lines.length === 1 && lines[0] === "")`（多一个 `length === 0` 分支，但那只是防御，正常状态不会出现）。

#### 2.15.6 公共 API（逐字签名与语义）

- `getPaddingX(): number`；`setPaddingX(padding)`（归一化同上；变化时 `tui.requestRender()`）。
- `getAutocompleteMaxVisible(): number`；`setAutocompleteMaxVisible(n)`（夹取 3..20，变化时 requestRender）。
- `setAutocompleteProvider(provider)`：`cancelAutocomplete()`；保存；`setAutocompleteTriggerCharacters(provider.triggerCharacters ?? [])`。
- `addToHistory(text)`：`trimmed = text.trim()`；空则 return；与 `history[0]` 相同则 return；`unshift`；超过 100 条 `pop()`。
- `getText(): string` = `lines.join("\n")`。
- `getExpandedText(): string` = 把每个 paste 内容替换回其 marker（`new RegExp(`\\[paste #${id}( (\\+\\d+ lines|\\d+ chars))?\\]`, "g")`，按 `pastes` 插入顺序迭代）。
- `getLines(): string[]` = 副本。
- `getCursor(): {line, col}`。
- `setText(text)`：cancelAutocomplete；`lastAction = null`；`exitHistoryBrowsing()`；`normalized = normalizeText(text)`；若 `getText() !== normalized` → `pushUndoSnapshot()`；**清空 pastes 与 pasteCounter**；`setTextInternal(normalized)`。
- `insertTextAtCursor(text)`：空则 return；cancelAutocomplete；`pushUndoSnapshot()`；`lastAction = null`；exitHistoryBrowsing；`insertTextAtCursorInternal(text)`（原子撤销）。
- `invalidate()`：空（编辑器无渲染缓存）。
- `isShowingAutocomplete(): boolean`。
- `protected renderTopBorder(width, hiddenLineCount)`：`hiddenLineCount > 0 ? createScrollBorder("↑", hiddenLineCount, width) : "─".repeat(width)`，再 `borderColor(...)`。
- `protected renderBottomBorder(width, hiddenLineCount)`：对称（`"↓"`）。
- `render(width)`（见 2.15.7）。
- `handleMouse(event)`（见 2.15.8）。
- `handleInput(data)`（见 2.15.9）。

#### 2.15.7 `render(width)`

1. `maxPadding = max(0, floor((width - 1) / 2))`；`paddingX = min(this.paddingX, maxPadding)`；`contentWidth = max(1, width - paddingX * 2)`。
2. `layoutWidth = max(1, contentWidth - (paddingX ? 0 : 1))`（无 padding 时为光标预留 1 列）。
3. `lastWidth = layoutWidth`。
4. `layoutLines = layoutText(layoutWidth)`。
5. `terminalRows = tui.terminal.rows`；`maxVisibleLines = max(5, floor(terminalRows * 0.3))`。
6. `cursorLineIndex = layoutLines.findIndex(l => l.hasCursor)`；无则 0。
7. 滚动夹取：`cursorLineIndex < scrollOffset` → `scrollOffset = cursorLineIndex`；`cursorLineIndex >= scrollOffset + maxVisibleLines` → `scrollOffset = cursorLineIndex - maxVisibleLines + 1`；再 `scrollOffset = max(0, min(scrollOffset, max(0, layoutLines.length - maxVisibleLines)))`。
8. `visibleLines = layoutLines.slice(scrollOffset, scrollOffset + maxVisibleLines)`；`renderedVisibleLineCount = visibleLines.length`。
9. `leftPadding = rightPadding = " ".repeat(paddingX)`。
10. 首行：`renderTopBorder(width, scrollOffset)`。
11. 每条可见行：
    - `displayText = layoutLine.text`；`lineVisibleWidth = visibleWidth(text)`；`cursorInPadding = false`。
    - 若 `hasCursor && cursorPos !== undefined`：
      - `before = slice(0, cursorPos)`；`after = slice(cursorPos)`；`marker = focused ? CURSOR_MARKER : ""`。
      - `after.length > 0`：取 `after` 首个 grapheme，`displayText = before + marker + renderFakeCursor(firstGrapheme) + restAfter`；`lineVisibleWidth` 不变。
      - 否则：`displayText = before + marker + renderFakeCursor(" ")`；`lineVisibleWidth += 1`；若 `lineVisibleWidth > contentWidth && paddingX > 0` → `cursorInPadding = true`。
    - `padding = " ".repeat(max(0, contentWidth - lineVisibleWidth))`；`lineRightPadding = cursorInPadding ? rightPadding.slice(1) : rightPadding`。
    - 推入 `${leftPadding}${displayText}${padding}${lineRightPadding}`。
12. 末行：`linesBelow = layoutLines.length - (scrollOffset + visibleLines.length)`；`renderBottomBorder(width, linesBelow)`。
13. `renderedAutocompleteHeight = 0`；若有 `autocompleteState && autocompleteList`：`autocompleteResult = autocompleteList.render(contentWidth)`；记录高度；逐行推入 `${leftPadding}${line}${" ".repeat(max(0, contentWidth - visibleWidth(line)))}${rightPadding}`（**注意：autocomplete 行的 padding 按 contentWidth 计算，但左右 padding 也加上，因此总宽 = width**）。
14. 返回。

`layoutText(contentWidth)`：
- 空编辑器（`lines.length === 0` 或单行 `""`）→ `[{text:"", hasCursor:true, cursorPos:0}]`。
- 所有 grapheme 迭代（换行预分段与光标替换）都用 `this.segment(text, "grapheme")`（marker 感知），不是裸 `graphemeSegmenter`。
- 每逻辑行：
  - `lineVisibleWidth <= contentWidth`：当前行 → `{text: line, hasCursor:true, cursorPos: cursorCol}`；否则 `{text: line, hasCursor:false}`。
  - 否则 `chunks = wordWrapLine(line, contentWidth, [...segment(line, "grapheme")])`；对每个 chunk：
    - 当前行且**最后一个 chunk**：`hasCursorInChunk = cursorCol >= chunk.startIndex`；`adjustedCursorPos = cursorCol - chunk.startIndex`。
    - 当前行且非最后 chunk：`hasCursorInChunk = cursorCol >= startIndex && cursorCol < endIndex`；若命中，`adjusted = cursorCol - startIndex`，并夹到 `chunk.text.length`（光标在被 trim 的空白里时）。
    - 命中 → `{text: chunk.text, hasCursor:true, cursorPos: adjusted}`；否则 `{text, hasCursor:false}`。

#### 2.15.8 `handleMouse(event)`

1. `autocompleteStartRow = renderedVisibleLineCount + 2`（1 顶边 + N 内容 + 1 底边）。
2. 若 autocomplete 显示且 `event.y ∈ [autocompleteStartRow, autocompleteStartRow + renderedAutocompleteHeight)`：
   - 重算 `paddingX/contentWidth`（基于 `event.width`）；
   - 转发给 `autocompleteList.handleMouse({...event, x: event.x - paddingX, y: event.y - autocompleteStartRow, width: contentWidth, height: renderedAutocompleteHeight})`；
   - 有结果 → `{...result, focus: true}`；否则 `undefined`。
3. 仅处理 `type === "click" && button === "left"`（注释：press/drag/release 留给渲染器的文本选择；渲染器在“同格按下并释放且未移动”时合成 click）。
4. `event.y <= 0 || event.y > renderedVisibleLineCount` → `{ handled: true, focus: true }`（点在边框上也只聚焦）。
5. `visualLines = buildVisualLineMap(lastWidth)`；`visualLineIndex = scrollOffset + event.y - 1`；越界 → `{handled:true, focus:true}`。
6. `logicalLine = lines[visualLine.logicalLine] ?? ""`；`chunk = logicalLine.slice(startCol, startCol + length)`。
7. `targetColumn = max(0, event.x - paddingX)`；遍历 `this.segment(chunk, "grapheme")`（**marker 感知**：命中落在有效 paste marker 内部时整段算一个字形，`targetIndex` 会吸附到 marker 起点）：`nextColumn = visibleColumn + visibleWidth(grapheme)`；记录 `lastGraphemeIndex`；`targetColumn < nextColumn` → `targetIndex = grapheme.index` 并 break；否则前进。默认 `targetIndex = chunk.length`。
8. `isLastSegment = visualLineIndex === visualLines.length - 1 || visualLines[visualLineIndex + 1]?.logicalLine !== visualLine.logicalLine`；若 `!isLastSegment && targetIndex === chunk.length && chunk.length > 0` → `targetIndex = lastGraphemeIndex`（**点击非末段行尾时退到最后一个字形起点**，避免落到下一段）。
9. `cursorLine = visualLine.logicalLine`；`setCursorCol(visualLine.startCol + targetIndex)`；`lastAction = null`；`exitHistoryBrowsing()`；若 autocomplete → `updateAutocomplete()`；返回 `{handled:true, focus:true}`。

#### 2.15.9 `handleInput(data)` 全量分派

**A. jump 模式（等待目标字符）**：
- 若 `tui.editor.jumpForward` 或 `jumpBackward` 再次按下 → 取消 jump 并 return。
- `printable = decodePrintableKey(data) ?? (data.charCodeAt(0) >= 32 ? data : undefined)`；有 → 记 direction、清 jumpMode、`jumpToChar(printable, direction)`。
- 否则（控制字符）→ 清 jumpMode 并**继续往下走**。

**B. bracketed paste**：同 Input 的结构（`isInPaste`、`pasteBuffer`；只有 `pasteContent.length > 0` 才 `handlePaste`；剩余数据递归 `handleInput`）。

**C. 复制键**：`tui.input.copy`（默认 `ctrl+c`）→ **直接 return，交给父级处理**（退出/清空）。

**D. 撤销**：`tui.editor.undo` → `undo()`。

**E. autocomplete 可见时（`autocompleteState && autocompleteList`）**：
- `tui.select.cancel` → `cancelAutocomplete()`；return。
- `tui.select.up` / `down` → 转发给 `autocompleteList.handleInput(data)`；return（**不移动编辑器光标**）。
- `tui.input.tab`：取 `selected = list.getSelectedItem()`；有选中且有 provider → `pushUndoSnapshot()`；`lastAction = null`；`applyCompletion(lines, cursorLine, cursorCol, selected, autocompletePrefix)` → 写回 lines/cursorLine/cursorCol；`cancelAutocomplete()`；`onChange?.(getText())`；return。
- `tui.select.confirm`：若有选中项且有 provider → 同 Tab 的应用流程（pushUndoSnapshot → applyCompletion → 写回 → `cancelAutocomplete()`）；若 `autocompletePrefix.startsWith("/")` → **继续落到后续分派**（最终由 `tui.input.submit` 提交刚补全的命令）；否则 `onChange?.(getText())` + return。
  - **无选中项或无 provider** 时该分支什么都不做并**穿出 autocomplete 块**（此时 `autocompleteState` 仍非空，Tab 分支因 `!this.autocompleteState` 为假而跳过），继续走删除/提交等后续分派——即 Enter 会照常提交当前文本。

**F. Tab 触发补全**：`tui.input.tab` 且当前无 autocomplete → `handleTabCompletion()`；return。

**G. 删除**：`deleteToLineEnd` → `deleteToEndOfLine()`；`deleteToLineStart` → `deleteToStartOfLine()`；`deleteWordBackward` → `deleteWordBackwards()`；`deleteWordForward` → `deleteWordForward()`；`deleteCharBackward` **或** `matchesKey(data, "shift+backspace")` → `handleBackspace()`；`deleteCharForward` **或** `matchesKey(data, "shift+delete")` → `handleForwardDelete()`。

**H. kill ring**：`yank` → `yank()`；`yankPop` → `yankPop()`。

**I. 历史键**：`historyPrevious` → `cancelAutocomplete()` + `navigateHistory(-1)`；`historyNext` → `cancelAutocomplete()` + `navigateHistory(1)`。（默认绑定为空数组，需用户/应用配置，测试 `editor-history-keybindings.test.ts` 用 `ctrl+p`/`ctrl+n` 覆盖。）

**J. 光标**：`cursorLineStart` → `moveToLineStart()`；`cursorLineEnd` → `moveToLineEnd()`；`cursorWordLeft` → `moveWordBackwards()`；`cursorWordRight` → `moveWordForwards()`。

**K. 换行**：条件（任一成立）：
```ts
kb.matches(data, "tui.input.newLine") ||
data.charCodeAt(0) === 10 && data.length > 1 ||   // 多字节以 \n 开头（如 modifyOtherKeys）
data === "\x1b\r" ||
data === "\x1b[13;2~" ||
(data.length > 1 && data.includes("\x1b") && data.includes("\r")) ||
(data === "\n" && data.length === 1)
```
→ 若 `shouldSubmitOnBackslashEnter(data, kb)`：`handleBackspace()` + `submitValue()`；否则 `addNewLine()`；return。

**L. 提交**：`tui.input.submit`（默认 `enter`）：
- `disableSubmit` → return。
- **反斜杠工作区**：若 `cursorCol > 0 && currentLine[cursorCol - 1] === "\\"` → `handleBackspace()` + `addNewLine()`；return。
- 否则 `submitValue()`；return。

**M. 方向键**：
- `cursorUp`：若在首个视觉行且（编辑器为空 **或** `historyIndex > -1` **或** `cursorCol === 0`）→ `navigateHistory(-1)`；否则若在首个视觉行 → `moveToLineStart()`；否则 `moveCursor(-1, 0)`。
- `cursorDown`：若 `historyIndex > -1 && 在最后视觉行` → `navigateHistory(1)`；否则若在最后视觉行 → `moveToLineEnd()`；否则 `moveCursor(1, 0)`。
- `cursorRight` → `moveCursor(0, 1)`；`cursorLeft` → `moveCursor(0, -1)`。

**N. 翻页**：`pageUp` → `pageScroll(-1)`；`pageDown` → `pageScroll(1)`。

**O. jump 触发**：`jumpForward` → `jumpMode = "forward"`；`jumpBackward` → `"backward"`。

**P. Shift+Space**：`matchesKey(data, "shift+space")` → `insertCharacter(" ")`。

**Q. 可打印键**：`decodePrintableKey(data)` 有值 → `insertCharacter(printable)`。

**R. 兜底**：`data.charCodeAt(0) >= 32` → `insertCharacter(data)`。

#### 2.15.10 编辑操作（逐方法语义）

`navigateHistory(direction: 1 | -1)`：
- `lastAction = null`；`history.length === 0` → return。
- `newIndex = historyIndex - direction`（Up = -1 使索引增）；越界 `newIndex < -1 || newIndex >= history.length` → return。
- 首次进入浏览（`historyIndex === -1 && newIndex >= 0`）：`pushUndoSnapshot()`；`historyDraft = structuredClone(state)`。
- `historyIndex = newIndex`。
- `-1`：恢复 draft（无 draft 则 `setTextInternal("")`），并清 `preferredVisualCol`、`snappedFromCursorCol`、`scrollOffset = 0`、`onChange?.(getText())`。
- 否则 `setTextInternal(history[historyIndex] || "", direction === -1 ? "start" : "end")`（**向上游历史光标在开头，向下在末尾**；测试 `places cursor at start after browsing history upward` / `...downward`）。

`exitHistoryBrowsing()`：`historyIndex = -1`；`historyDraft = null`（**不触发 onChange、不清 undo**）。

`setTextInternal(text, cursorPlacement = "end")`：`lines = text.split("\n")`（`JS split` 对 `""` 返回 `[""]`；Rust 需显式处理空串）；`cursorLine = placement === "start" ? 0 : lines.length - 1`；`setCursorCol(placement === "start" ? 0 : lines[cursorLine].length)`；`scrollOffset = 0`；`onChange?.(getText())`。**不重置 history 状态**（`navigateHistory` 专用）。

`normalizeText(text)`（模块内私有；被 `setText`、`insertTextAtCursorInternal`、`handlePaste` 共用）：
```ts
text.replace(/\r\n/g, "\n").replace(/\r/g, "\n").replace(/\t/g, "    ")
```
即 CRLF→LF、裸 CR→LF、**tab→4 空格**（与 `Input.handlePaste` 的 tab→4 空格一致；注意 `Text`/`Markdown` 的 tab→**3** 空格不同）。

`insertTextAtCursorInternal(text)`（内部，调用方负责 undo 快照；**不触发 autocomplete**）：
- `!text` → return。
- `normalized = normalizeText(text)`（`\r\n`/`\r` → `\n`，`\t` → **4 空格**）；`insertedLines = normalized.split("\n")`。
- 单行：`lines[cursorLine] = before + normalized + after`；`setCursorCol(cursorCol + normalized.length)`。
- 多行：`lines = [...lines[0..cursorLine], before + insertedLines[0], ...insertedLines[1..len-1], insertedLines[len-1] + after, ...lines[cursorLine+1..]]`；`cursorLine += insertedLines.length - 1`；`setCursorCol(lastInsertedLine.length)`。
- 末尾 `onChange?.(getText())`（**只调一次**）。

`insertCharacter(char, skipUndoCoalescing?)`：
- `exitHistoryBrowsing()`。
- 非 skip 时：`if (isWhitespaceChar(char) || lastAction !== "type-word") pushUndoSnapshot()`；`lastAction = "type-word"`。
- **`skipUndoCoalescing` 目前没有任何调用方传 true**（3 处调用 `insertCharacter(" ")` / `insertCharacter(printable)` / `insertCharacter(data)` 都是单参；注释称其“供 handlePaste 之类的原子操作使用”，但 `handlePaste` 实际走 `insertTextAtCursorInternal`）。Rust 侧保留该参数以对齐签名即可，行为上等价于“总是 coalescing”，不必为它设计额外语义。
- 在当前行 cursor 处插入；`setCursorCol(cursorCol + char.length)`；`onChange?.(getText())`。
- 自动补全触发（仅当 `!autocompleteState`）：
  - `char === "/" && isAtStartOfMessage()` → `tryTriggerAutocomplete()`。
  - `autocompleteTriggerCharacters.includes(char)` 且 `textBeforeCursor` 匹配 `autocompleteTriggerPattern` → 触发。
  - `/[a-zA-Z0-9.\-_]/.test(char) || cjkBreakRegex.test(char)` 时：若 `isInSlashCommandContext(textBeforeCursor)` → 触发；否则若匹配 `autocompleteTriggerPattern` → 触发。
- 已有 autocomplete → `updateAutocomplete()`。

`handlePaste(pastedText)`：
1. `cancelAutocomplete()`；`exitHistoryBrowsing()`；`lastAction = null`；`pushUndoSnapshot()`。
2. **CSI-u Ctrl 解码**（tmux popup / extended-keys-format=csi-u）：`pastedText.replace(/\x1b\[(\d+);5u/g, (match, code) => cp 在 97..122 → String.fromCharCode(cp-96)；65..90 → cp-64；否则原样)`。
3. `cleanText = normalizeText(decodedText)`（`\r\n`→`\n`、`\r`→`\n`、`\t`→4 空格）。
4. 过滤：`split("").filter(c => c === "\n" || c.charCodeAt(0) >= 32).join("")`（**按 UTF-16 code unit 过滤**；emoji 代理对高位 ≥ 32 故保留）。
5. **路径前置空格**：若 `filteredText` 匹配 `/^[/~.]/` 且光标前一个字符匹配 `/\w/` → 前置一个空格。
6. `pastedLines = filteredText.split("\n")`；`totalChars = filteredText.length`。
7. **大粘贴阈值**：`pastedLines.length > 10 || totalChars > 1000` → `pasteCounter++`；`pastes.set(id, filteredText)`；marker = 行数超限时 `` `[paste #${id} +${pastedLines.length} lines]` ``，否则 `` `[paste #${id} ${totalChars} chars]` ``；`insertTextAtCursorInternal(marker)`；return。
8. 否则 `insertTextAtCursorInternal(filteredText)`（单行与多行同一路径；**不触发 autocomplete**）。

`addNewLine()`：cancelAutocomplete；exitHistoryBrowsing；`lastAction = null`；`pushUndoSnapshot()`；在当前光标处把行拆成两行；`cursorLine++`；`setCursorCol(0)`；`onChange?.(getText())`。

`shouldSubmitOnBackslashEnter(data, kb)`：
- `disableSubmit` → false；`!matchesKey(data, "enter")` → false。
- `submitKeys = kb.getKeys("tui.input.submit")`；若无 `shift+enter` 也无 `shift+return` → false（**只有用户把 shift+enter 绑到 submit 时才启用该技巧**）。
- 否则返回 `cursorCol > 0 && currentLine[cursorCol - 1] === "\\"`。

`submitValue()`：cancelAutocomplete；`result = expandPasteMarkers(lines.join("\n")).trim()`；重置 `state = {lines:[""], cursorLine:0, cursorCol:0}`；清 pastes / pasteCounter；exitHistoryBrowsing；`scrollOffset = 0`；`undoStack.clear()`；`lastAction = null`；`onChange?.("")`；`onSubmit?.(result)`。

`handleBackspace()`：
- exitHistoryBrowsing；`lastAction = null`。
- `cursorCol > 0`：`pushUndoSnapshot()`；取光标前最后一个 grapheme 段；**若是 paste marker**：
  - `targetId = Number(匹配组 1)`；`pastes.delete(targetId)`；`pasteCounter--`；
  - 把 `pastes` 中所有 `id > targetId` 的条目按**升序**逐个 `set(id-1, content)` 后 `delete(id)`；
  - 把所有行内 id > targetId 的 marker 重编号为 `[paste #<id-1><suffix>]`（`PASTE_MARKER_REGEX` 的 `idGroup`/`suffixGroup`）。
  - 然后按该 grapheme 长度删除文本（marker 整段删除）。
- `cursorCol === 0 && cursorLine > 0`：`pushUndoSnapshot()`；与前一行合并；`cursorLine--`；`setCursorCol(previousLine.length)`。
- `onChange?.(getText())`。
- autocomplete：有 → `updateAutocomplete()`；无 → 若 `isInSlashCommandContext(textBeforeCursor)` 或匹配 trigger pattern → `tryTriggerAutocomplete()`。

`handleForwardDelete()`：对称；`cursorCol < currentLine.length` 删一个 grapheme，否则 `cursorLine < lines.length - 1` 时合并下一行；之后同样的 autocomplete 刷新逻辑。

`deleteToStartOfLine()`：`cursorCol > 0` → `pushUndoSnapshot()`；`deleted = currentLine.slice(0, cursorCol)`；`killRing.push(deleted, {prepend:true, accumulate: lastAction === "kill"})`；`lastAction = "kill"`；行变 `currentLine.slice(cursorCol)`；`setCursorCol(0)`。`cursorCol === 0 && cursorLine > 0` → push undo；`killRing.push("\n", {prepend:true, accumulate})`；合并前行。之后 `onChange`。

`deleteToEndOfLine()`：`cursorCol < currentLine.length` → push undo；`deleted = slice(cursorCol)`；`push(deleted, {prepend:false, accumulate})`；截断行。`cursorCol >= length && cursorLine < lines.length - 1` → push undo；`push("\n", {prepend:false, accumulate})`；合并下一行。之后 `onChange`。

`deleteWordBackwards()`：
- exitHistoryBrowsing。
- `cursorCol === 0`：若有上一行 → push undo；`push("\n", {prepend:true, accumulate})`；合并；`cursorLine--`；`setCursorCol(previousLine.length)`；**无上一行则什么都不做（不 push undo）**。
- 否则：push undo；`wasKill = lastAction === "kill"`；`oldCursorCol`；`moveWordBackwards()` 得 `deleteFrom`；`setCursorCol(oldCursorCol)`；`deleted = currentLine.slice(deleteFrom, cursorCol)`；`push(deleted, {prepend:true, accumulate:wasKill})`；`lastAction = "kill"`；行变 `slice(0, deleteFrom) + slice(cursorCol)`；`setCursorCol(deleteFrom)`。
- `onChange`。

`deleteWordForward()`：对称：`cursorCol >= currentLine.length` → 有下一行则 push undo + `push("\n", {prepend:false, ...})` + 合并；否则 push undo；`wasKill`；`moveWordForwards()` 得 `deleteTo`；回退光标；`deleted = slice(cursorCol, deleteTo)`；`push(deleted, {prepend:false, ...})`；删除区间。之后 `onChange`。

`moveWordBackwards()`（**编辑器版本，此前规格遗漏**）：`lastAction = null`；`currentLine = lines[cursorLine] || ""`。
- `cursorCol === 0`：仅当 `cursorLine > 0` 时 `cursorLine--` 并 `setCursorCol(prevLine.length)`（**跨行跳到上一行行尾**），否则不动；return。
- 否则 `setCursorCol(findWordBackward(currentLine, cursorCol, { segment: t => this.segment(t, "word"), isAtomicSegment: isPasteMarker }))`——**带 atomic 选项**：有效 paste marker 是一个整词单位（测试 `treats paste marker as single unit for word movement` 断言右移一次跨过整段）。

`moveWordForwards()`：`lastAction = null`；`cursorCol >= currentLine.length` 时仅当 `cursorLine < lines.length - 1` 则 `cursorLine++` + `setCursorCol(0)`，否则不动；return。否则 `setCursorCol(findWordForward(currentLine, cursorCol, { segment: t => this.segment(t, "word"), isAtomicSegment: isPasteMarker }))`。

**与 Input 的差异（易错点）**：`Input.moveWordBackwards/Forwards` **不带** atomic/segment 选项（Input 无 paste marker，且不跨行）；`Editor` 的两者**跨行**并**带 marker 感知**。Rust 侧的 `find_word_backward/forward` 必须同时支持这两组调用形态。

`moveToLineStart()`：`lastAction = null`；`setCursorCol(0)`。
`moveToLineEnd()`：`lastAction = null`；`setCursorCol(currentLine.length)`。

`setCursorCol(col)`：`cursorCol = col`；**清 `preferredVisualCol` 与 `snappedFromCursorCol`**（所有非垂直移动都走这里，重置粘性列）。

`moveCursor(deltaLine, deltaCol)`：
- `lastAction = null`；`visualLines = buildVisualLineMap(lastWidth)`；`currentVisualLine = findCurrentVisualLine(visualLines)`。
- `deltaLine !== 0`：`target = currentVisualLine + deltaLine`；越界忽略；否则 `moveToVisualLine(visualLines, currentVisualLine, target)`。
- `deltaCol > 0`：`cursorCol < currentLine.length` → 前进一个 grapheme（`setCursorCol(cursorCol + firstGraphemeLen)`）；否则若 `cursorLine < lines.length - 1` → `cursorLine++` 后 `setCursorCol(0)`；否则（末行末位）→ `preferredVisualCol = cursorCol - currentVL.startCol`（右移到末尾时记住列；**此分支不调用 setCursorCol**，因此不清 preferredVisualCol，正是粘性列的种子）。
- `deltaCol < 0`：`cursorCol > 0` → 后退一个 grapheme；否则若 `cursorLine > 0` → 上一行行尾。
- `autocompleteState` 存在 → `updateAutocomplete()`。

`pageScroll(direction)`：`lastAction = null`；`pageSize = max(5, floor(tui.terminal.rows * 0.3))`；`target = clamp(currentVisualLine + direction*pageSize, 0, visualLines.length - 1)`；`moveToVisualLine(...)`。

`moveToVisualLine(visualLines, currentVisualLine, targetVisualLine)`（**粘性列核心，逐行复刻**）：
1. 取 `currentVL`、`targetVL`；任一缺失 return。
2. 计算 `currentVisualCol`：
   - 若 `snappedFromCursorCol !== null`：`vlIndex = findVisualLineAt(visualLines, currentVL.logicalLine, snappedFromCursorCol)`；`currentVisualCol = snappedFromCursorCol - visualLines[vlIndex].startCol`；
   - 否则 `currentVisualCol = cursorCol - currentVL.startCol`。
3. `isLastSourceSegment = currentVisualLine === visualLines.length - 1 || visualLines[currentVisualLine+1]?.logicalLine !== currentVL.logicalLine`；`sourceMaxVisualCol = isLastSourceSegment ? currentVL.length : max(0, currentVL.length - 1)`。
4. 目标同理：`isLastTargetSegment`；`targetMaxVisualCol = isLastTargetSegment ? targetVL.length : max(0, targetVL.length - 1)`。
5. `moveToVisualCol = computeVerticalMoveColumn(currentVisualCol, sourceMaxVisualCol, targetMaxVisualCol)`。
6. `cursorLine = targetVL.logicalLine`；`targetCol = targetVL.startCol + moveToVisualCol`；`cursorCol = min(targetCol, logicalLine.length)`（**直接赋值，不调 setCursorCol**，保留 preferredVisualCol）。
7. **原子段吸附**：遍历 `logicalLine` 的 grapheme 段（含 paste marker 合并）：
   - `seg.index > cursorCol` → break；`seg.segment.length <= 1` → continue；
   - 若 `cursorCol ∈ [seg.index, seg.index + seg.segment.length)`：
     - `isContinuation = seg.index < targetVL.startCol`；`isMovingDown = targetVisualLine > currentVisualLine`；
     - 若 `isContinuation && isMovingDown`：`segEnd = seg.index + seg.segment.length`；把 `next` 从 `targetVisualLine + 1` 起推进，跳过所有 `visualLines[next].logicalLine === targetVL.logicalLine && startCol < segEnd` 的续段；若 `next < visualLines.length` → 递归 `moveToVisualLine(visualLines, currentVisualLine, next)` 并 return（**跳过 marker 尾部续段，避免“卡住”**）。
     - 否则 `snappedFromCursorCol = cursorCol`；`cursorCol = seg.index`（**吸附到段首以便整段高亮**）；return。
8. 未吸附 → `snappedFromCursorCol = null`。

`computeVerticalMoveColumn(currentVisualCol, sourceMaxVisualCol, targetMaxVisualCol)`——**粘性列决策表（源码注释，逐字保留）**：

| P | S | T | U | 场景 | 设置 preferred | 移到 |
|---|---|---|---|---|---|---|
| 0 | * | 0 | - | 开始导航，目标够长 | null | current |
| 0 | * | 1 | - | 开始导航，目标更短 | current | target end |
| 1 | 0 | 0 | 0 | 被夹取，目标容得下 preferred | null | preferred |
| 1 | 0 | 0 | 1 | 被夹取，目标更长但仍容不下 preferred | keep | target end |
| 1 | 0 | 1 | - | 被夹取，目标更短 | keep | target end |
| 1 | 1 | 0 | - | 重排后，目标容得下当前列 | null | current |
| 1 | 1 | 1 | - | 重排后，目标比当前列短 | current | target end |

其中 P = `preferredVisualCol !== null`；S = `currentVisualCol < sourceMaxVisualCol`（光标在源行中间而非行尾）；T = `targetMaxVisualCol < currentVisualCol`；U = `targetMaxVisualCol < preferredVisualCol`。

实现（**逐字**，`editor.ts:1581`）：
```ts
const hasPreferred = this.preferredVisualCol !== null;          // P
const cursorInMiddle = currentVisualCol < sourceMaxVisualCol;   // S
const targetTooShort = targetMaxVisualCol < currentVisualCol;   // T
if (!hasPreferred || cursorInMiddle) {
  if (targetTooShort) { preferredVisualCol = currentVisualCol; return targetMaxVisualCol; }   // 2,7
  preferredVisualCol = null; return currentVisualCol;                                          // 1,6
}
const targetCantFitPreferred = targetMaxVisualCol < this.preferredVisualCol!;                 // U
if (targetTooShort || targetCantFitPreferred) return targetMaxVisualCol;                      // 4,5
const result = this.preferredVisualCol!; this.preferredVisualCol = null; return result;        // 3
```
注意案例 4/5 时 `preferredVisualCol` **保持不变**（"keep"），这使后续上下移动仍能回到原列；案例 2/7 会把它设为 `currentVisualCol`。

`buildVisualLineMap(width)`：逐逻辑行：`line.length === 0` → `{logicalLine:i, startCol:0, length:0}`；`visibleWidth(line) <= width` → `{i, 0, line.length}`；否则 `wordWrapLine(line, width, segment(line,"grapheme"))`，每 chunk → `{logicalLine:i, startCol: chunk.startIndex, length: chunk.endIndex - chunk.startIndex}`。

`findVisualLineAt(visualLines, line, col)`：遍历，跳过 `logicalLine !== line`；`offset = col - startCol`；`isLastSegmentOfLine = i === len-1 || next.logicalLine !== vl.logicalLine`；命中条件 `offset >= 0 && (offset < vl.length || (isLastSegmentOfLine && offset === vl.length))`；返回 `i`；都不命中返回 `visualLines.length - 1`。

`findCurrentVisualLine(visualLines)` = `findVisualLineAt(..., cursorLine, cursorCol)`。

`isOnFirstVisualLine()` / `isOnLastVisualLine()` 基于上述。

`jumpToChar(char, direction)`（`editor.ts:2138`，**逐字**）：
- `lastAction = null`；`isForward = direction === "forward"`；`end = isForward ? lines.length : -1`；`step = ±1`。
- `for (lineIdx = cursorLine; lineIdx !== end; lineIdx += step)`：
  - `isCurrentLine = lineIdx === cursorLine`；`searchFrom = isCurrentLine ? (isForward ? cursorCol + 1 : cursorCol - 1) : undefined`。
  - `idx = isForward ? line.indexOf(char, searchFrom) : line.lastIndexOf(char, searchFrom)`。
  - `idx !== -1` → `cursorLine = lineIdx`；`setCursorCol(idx)`；return。
- 未命中 → 光标**完全不动**（`lastAction` 仍已被置 null）。
- 特性：大小写敏感、可跨行、前向跳过当前光标位置、后向从 `cursorCol - 1` 开始（也跳过光标处）；`char` 可为多 code unit 字符串（`indexOf` 语义），但调用方只传单个“可打印键”（可能是多码元 emoji）。若 `char` 为空串，`indexOf("", from)` 恒命中于 `from`（JS 语义，需在 Rust 中显式复刻或断言不会发生）。

`yank()`：ring 空 return；`pushUndoSnapshot()`；`insertYankedText(peek())`；`lastAction = "yank"`。
`yankPop()`：`lastAction !== "yank" || ring.length <= 1` return；`pushUndoSnapshot()`；`deleteYankedText()`；`ring.rotate()`；`insertYankedText(peek())`；`lastAction = "yank"`。
`insertYankedText(text)`：exitHistoryBrowsing；单行 → 插入并 `setCursorCol(+len)`；多行 → 首行与光标前文本合并、中间行插入、末行与光标后文本合并；`cursorLine = 最后一行索引`；`setCursorCol(末行长度)`；`onChange`。
`deleteYankedText()`：`yankedText = peek()`；单行 → 从光标前删 `len` 个字符；多行 → `startLine = cursorLine - (yankLines.length - 1)`；`startCol = lines[startLine].length - yankLines[0].length`；`afterCursor = lines[cursorLine].slice(cursorCol)`；`beforeYank = lines[startLine].slice(0, startCol)`；`lines.splice(startLine, yankLines.length, beforeYank + afterCursor)`；`cursorLine = startLine`；`setCursorCol(startCol)`；`onChange`。

`pushUndoSnapshot()`：`undoStack.push({state, pastes, pasteCounter})`（`UndoStack` 会 `structuredClone`，因此 `Map` 也被深拷贝——Rust 侧 `EditorSnapshot: Clone`，`pastes: BTreeMap<usize, String>`）。
`undo()`：exitHistoryBrowsing；`pop()` 无则 return；`Object.assign(state, snapshot.state)`；`pastes = snapshot.pastes`；`pasteCounter = snapshot.pasteCounter`；`lastAction = null`；`preferredVisualCol = null`；`onChange?.(getText())`。

#### 2.15.11 自动补全机制

- `getBestAutocompleteMatchIndex(items, prefix)`：`!prefix` → -1；遍历：`value === prefix` 立即返回该索引（精确匹配优先）；否则记录第一个 `value.startsWith(prefix)` 的索引；返回该索引或 -1。**大小写敏感、只看 `value`**。
- `createAutocompleteList(prefix, items)`：`layout = prefix.startsWith("/") ? SLASH_COMMAND_SELECT_LIST_LAYOUT : undefined`；`new SelectList(items, autocompleteMaxVisible, theme.selectList, layout)`；`onSelect = selected => { applyCompletion(...); cancelAutocomplete(); onChange?.(getText()); }`。
- `tryTriggerAutocomplete(explicitTab = false)` → `requestAutocomplete({force:false, explicitTab})`。
- `handleTabCompletion()`：无 provider return；若 `isInSlashCommandContext(beforeCursor) && !beforeCursor.trimStart().includes(" ")` → `handleSlashCommandCompletion()`（`requestAutocomplete({force:false, explicitTab:true})`）；否则 `forceFileAutocomplete(true)`（`requestAutocomplete({force:true, explicitTab:true})`）。
- `requestAutocomplete({force, explicitTab})`：
  1. 无 provider return。
  2. `force`：若 provider 有 `shouldTriggerFileCompletion` 且返回 false → return。
  3. `cancelAutocompleteRequest()`；`startToken = ++autocompleteStartToken`。
  4. `debounceMs = getAutocompleteDebounceMs(options)`：`explicitTab || force` → 0；否则若 `autocompleteDebouncePattern.test(textBeforeCursor)` → 20，否则 0。
  5. `debounceMs > 0` → `setTimeout(() => { timer = undefined; void startAutocompleteRequest(startToken, options); }, debounceMs)`；return。否则立即 `void startAutocompleteRequest(...)`。
- `startAutocompleteRequest(startToken, options)`：串行化链——`previousTask = autocompleteRequestTask`；`autocompleteRequestTask = (async () => { await previousTask; if (startToken !== autocompleteStartToken || !provider) return; controller = new AbortController(); autocompleteAbort = controller; requestId = ++autocompleteRequestId; snapshotText = getText(); snapshotLine/Col; await runAutocompleteRequest(...); })()`；再 `await`.
- `runAutocompleteRequest(...)`：`await provider.getSuggestions(state.lines, cursorLine, cursorCol, { signal, force })`；`isAutocompleteRequestCurrent(...)` 不成立 → return；`autocompleteAbort = undefined`；若 `!suggestions || !Array.isArray(items) || items.length === 0` → `cancelAutocomplete()` + `tui.requestRender()`；若 `force && explicitTab && items.length === 1` → 直接 `applyCompletion` 并 requestRender（**单条 Tab 补全自动应用，不显示菜单**）；否则 `applyAutocompleteSuggestions(suggestions, force ? "force" : "regular")` + requestRender。
- `isAutocompleteRequestCurrent(requestId, controller, snapshotText, snapshotLine, snapshotCol)`：`!signal.aborted && requestId === autocompleteRequestId && getText() === snapshotText && cursorLine === snapshotLine && cursorCol === snapshotCol`（**快照校验，防止过期结果覆盖**）。
- `applyAutocompleteSuggestions(suggestions, state)`：`autocompletePrefix = suggestions.prefix`；`autocompleteList = createAutocompleteList(...)`；`bestMatchIndex >= 0` → `setSelectedIndex`；`autocompleteState = state`。
- `cancelAutocompleteRequest()`：`++autocompleteStartToken`；清 debounce 定时器；`abort()` 当前 controller；清引用。
- `clearAutocompleteUi()`：`state = null`；`list = undefined`；`prefix = ""`。
- `cancelAutocomplete()` = 二者组合。
- `setAutocompleteTriggerCharacters(chars)`：起点为 `["@", "#"]` 副本；对每个 `character`：`length !== 1 || character === "/" || isWhitespaceChar(character) || 已存在` 则跳过；否则追加。重建两个正则。
- `isSlashMenuAllowed()`：`cursorLine === 0`。
- `isAtStartOfMessage()`：需 `isSlashMenuAllowed()`；`beforeCursor.trim() === "" || beforeCursor.trim() === "/"`。
- `isInSlashCommandContext(textBeforeCursor)`：`isSlashMenuAllowed() && textBeforeCursor.trimStart().startsWith("/")`。

#### 2.15.12 Editor 测试清单与关键断言

`editor.test.ts` 共 192 个 `it`，分组与断言（Rust 侧必须逐条通过）：

- **Prompt history navigation**（15）：空历史 Up 无变化；Up 显示最近项；连续 Up 遍历；非空 draft 时先跳到行首再进入历史；Down 正向遍历；输入字符退出历史；`setText` 退出历史；不记录空串；不记录连续重复；允许非连续重复；有内容时 Up 移动光标而非历史（在非首视觉行）；上限 100；Up 后光标在开头；Down 后光标在末尾；多行历史项内可反向移动光标。
- **public state accessors**（2）：`getCursor()` 精确 `{line,col}`（UTF-16 列）；`getLines()` 返回副本（改动不影响内部）。
- **Backslash+Enter newline workaround**（5）：反斜杠立即插入（不缓冲）；独立反斜杠 + Enter 变换行；反斜杠后跟其它字符时正常插入；反斜杠不在光标紧前时不触发；多个反斜杠只删一个。
- **Kitty CSI-u handling**（3）：忽略不支持修饰符的可打印 CSI-u；Shift CSI-u 字母作为文本插入；xterm modifyOtherKeys shift 字母作为文本。
- **Unicode text editing**（12）：混合 ASCII/umlaut/emoji 原样插入；Backspace 删除单 code unit 的 umlaut；一次 Backspace 删多 code unit emoji；在 umlaut 上移动后插入位置正确；方向键一次跨过 emoji；跨行保留 umlaut；`setText` 整体替换；Ctrl+A 到文档开头插入；Ctrl+W / Alt+Backspace 删词；Ctrl+Left/Right 词移动；在全角中文标点处停止（issue #4972）；混合 CJK+ASCII 词移动。
- **Scroll indicators**（2）：见 2.15.2。
- **Grapheme-aware text wrapping**（8）：宽 emoji 行宽；长文本 emoji 断行位置；孤立 Thai/Lao AM 簇无宽度漂移；CJK 每字 2 列；混合 ASCII/宽字符；宽字符上的光标；emoji 在断行边界不超宽；行尾光标显示、下一字符才换行。
- **Word wrapping**（22）：见 2.15.3（10 个 `wordWrapLine` 纯文本用例：恰好满宽的 word/空格边界、unbreakable 词、多空格并列/恰好填满/超宽拆分、长空白在行边界处三段式；1 个 wide-char force-break；5 个预分段 oversized atomic segment 用例；6 个 `Editor.render` 层用例，其中 `width = 10 + 1` + `"1234567890"` → 恰好 3 行）。
- **Kill ring**（20）：Ctrl+W→Ctrl+Y；Ctrl+U；Ctrl+K；空 ring 的 Ctrl+Y 无效；Alt+Y 循环；未 yank 时 Alt+Y 无效；ring ≤1 时 Alt+Y 无效；连续 Ctrl+W 累积；Ctrl+U 跨行累积含换行；后向删除 prepend / 前向 append；非删除动作打断累积；非 yank 动作打断 Alt+Y 链；循环后旋转持久；跨行连续删除合并为一条；行尾 Ctrl+K 删换行并合并；文本中间 yank；中间 yank-pop；多行 yank/yank-pop；Alt+D 前向删词入 ring；Alt+D 行尾删换行。
- **Undo**（24）：空栈无效；连续词字符合并；空格逐个撤销；换行撤销且标记下一个词捕获状态；撤销 backspace / forward delete / Ctrl+W / Ctrl+K / Ctrl+U / yank；单行粘贴原子撤销且不触发 autocomplete；解码粘贴内 CSI-u Ctrl 序列（tmux popup）；多行粘贴原子撤销；`insertTextAtCursor` 原子撤销、支持多行、归一化 CRLF/CR；`setText("")` 可撤销；提交时清栈；撤销退出历史浏览；多次历史导航后仍回到历史前状态；光标移动开启新撤销单元；无操作删除不推快照；撤销 autocomplete。
- **Autocomplete**（24）：CJK 标点后触发并防抖符号补全；CJK 字母后或无前缀路径不自动触发；仅 Tab 时请求路径补全；Tab 补全中文路径前缀（空白/CJK 标点后）；未加引号 trigger/debounce 上下文在空白或 CJK 标点结束；接受目录后删除再重新触发；force-file 单建议自动应用；多条时显示菜单；force 模式（Tab 触发）打字保持菜单；`@`/`#`/自定义触发字符防抖（50ms 后 1 次调用、`isShowingAutocomplete()` 为 true）；光标移回命令名时重新查询（#5496）；切换 provider 重置自有触发字符；继续打字时中止进行中的 `@` 补全；slash 命令退格到空时隐藏；Enter 应用精确键入的参数值；非精确时选第一个前缀匹配；唯一前缀匹配高亮；多匹配选第一个；内置风格参数补全路径；异步参数补全被 await；非法结果被忽略；无参数补全器时不显示。
- **Character jump (Ctrl+])**（14）：同行前向第一个；光标后下一个；跨行前向；同行后向第一个（光标前）；跨行后向；找不到不动（前后）；大小写敏感；再次 Ctrl+] 取消；Escape 取消并处理该 Escape；Ctrl+Alt+] 再次取消；特殊字符可搜；空文本安全；跳转重置 lastAction。
- **Sticky column**（19）：上/下穿过更短行时保留目标列；左/右/打字/backspace/Ctrl+A/Ctrl+E/Ctrl+Left/Ctrl+Right/undo 重置粘性列；连续上下；穿过换行后的视觉行不卡住；`setText` 重置；末行末尾右移设置 preferredVisualCol；同/异行 resize；重排后目标够长/更短两种情形。
- **Paste marker atomic behavior**（22）：大粘贴生成 marker（`/\[paste #\d+ \+\d+ lines\]/`）；marker 对右/左箭头、Backspace、forward delete、词移动都是单个单位（右箭头一次跨过整段，`cursorCol = 1 + marker.length`）；backspace 删 marker 后可 undo 恢复 marker；删 marker 后可 undo 恢复 registry；删两个 marker 中第一个后 undo 恢复两者；**marker 在文本中乱序时 registry 按升序重编号**；`setText` 后 undo 恢复 marker 与 registry；同一行多个 marker；手打的形似 marker 文本不原子（无有效 id）；marker 宽于终端不崩；文本+marker 超宽且光标在 marker 上不崩；回退换行机会后重新检查溢出；`getExpandedText` 字面展开；向下进入 marker 时吸附到起点；穿过 marker 行保留粘性列；从多视觉行 marker 向下不卡住；preferred 落在 marker 尾部时跳过续段；提交大粘贴内容字面展开。

`editor-history-keybindings.test.ts`（1）：自定义 `tui.editor.historyPrevious = ctrl+p`、`historyNext = ctrl+n`；在 draft 上左移两次后 Ctrl+P 直接进入历史（不先移动光标），多行历史项向上时光标在 `{line:0,col:0}`、向下时在 `{line:1,col:16}`；再 Ctrl+N 回到 draft 且 `{line:0,col:3}`。

### 2.16 `src/components/markdown.ts`（1065 行）

#### 2.16.1 导出类型

```ts
export interface DefaultTextStyle {
  color?: (text: string) => string;
  bgColor?: (text: string) => string;
  bold?: boolean; italic?: boolean; strikethrough?: boolean; underline?: boolean;
}
export interface MarkdownTheme {
  heading: (text: string) => string;
  link: (text: string) => string;
  linkUrl: (text: string) => string;
  code: (text: string) => string;
  codeBlock: (text: string) => string;
  codeBlockBorder: (text: string) => string;
  quote: (text: string) => string;
  quoteBorder: (text: string) => string;
  hr: (text: string) => string;
  listBullet: (text: string) => string;
  bold: (text: string) => string;
  italic: (text: string) => string;
  strikethrough: (text: string) => string;
  underline: (text: string) => string;
  highlightCode?: (code: string, lang?: string) => string[];
  codeBlockIndent?: string;          // 默认 "  "
}
export interface MarkdownOptions {
  preserveOrderedListMarkers?: boolean;
  preserveBackslashEscapes?: boolean;
  transform?: (markdown: string, availableWidth: number) => string;
  renderLatex?: boolean;             // 默认 true
}
```

#### 2.16.2 解析器构建（模块级，逐字）

```ts
const STRICT_STRIKETHROUGH_REGEX = /^(~~)(?=[^\s~])((?:\\.|[^\\])*?(?:\\.|[^\s~\\]))\1(?=[^~]|$)/;
class StrictStrikethroughTokenizer extends Tokenizer { override del(src) { ... } }
const LATEX_MARKDOWN_EXTENSIONS: readonly TokenizerExtension[] = [ latexBlock(block), latex(inline) ];
const markdownParser = new Marked();
markdownParser.setOptions({ tokenizer: new StrictStrikethroughTokenizer() });
markdownParser.use({
  tokenizer: { emStrong: markedCjkFriendly().tokenizer?.emStrong },
  extensions: [...LATEX_MARKDOWN_EXTENSIONS],
});
```

**严格删除线**：只有 `~~` 包裹、内部首字符非空白/非 `~`、末字符非空白/非 `~`/非 `\`、且关闭后不是 `~` 时才成 `del` token；`~text~` 保持普通文本（测试 `keeps ~text~ as plain text`）。

**CJK 强调**：把 `marked-cjk-friendly` 的 `emStrong` tokenizer 注入，使 `**`/`*` 紧邻 CJK 标点时仍能解析（测试 `renders emphasis delimited next to CJK punctuation`、`keeps CommonMark flanking rules for non-CJK text`）。

**LaTeX tokenizer**（与 tui-latex 无耦合，只调用 `renderLatex`）：
- `isEscaped(source, index)`：向前数连续反斜杠，奇数表示被转义。
- `findClosingDelimiter(source, closing, start)`：`indexOf` 后跳过被转义的命中。
- `looksLikePendingDollarMath(source)`：`/\\[A-Za-z]+|[_^=+*/<>()[\]|±≤≥≠≈∈→⇒∞∫∑√-]/`。
- `tokenizeInlineLatex(source)`：识别 `$$`/`$$`、`\(`/`\)`、`\[`/`\]`、`$`/`$`（要求 `source.startsWith("$") && !/^\$\s/.test(source)`）。
  - 关闭符存在但**不构成数学**时返回 undefined：`opening === "$"` 且满足任一：内部以空白结尾、关闭后紧跟数字、内部形如 `[A-Z_][A-Z0-9_]*`（常量名）而关闭后紧跟标识符、内部含反引号。
  - 无关闭符：若 `opening` 以 `\` 开头（`\(`/`\[`）或 `looksLikePendingDollarMath(剩余)` → 返回 **pending** token（`{type:"latex", raw: source, text: pendingSource, pending: true}`），否则 undefined。
  - 内部含 `\n` 或为空 → undefined。
  - 否则 `{type:"latex", raw: source.slice(0, closingIndex+closing.length), text}`。
- `TABLE_DELIMITER_ROW_REGEX = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/`。
- `containsTableStart(source)`：相邻两行中，第二行匹配 delimiter 且（delimiter 含 `|` 或首行含 `|`）。
- `tokenizeBlockLatex(source)`：
  - `^ {0,3}\$\$[ \t]*(?:\n)?([\s\S]*?)\$\$[ \t]*(?:\n|$)` 且内容非空且 `!containsTableStart(content)` → `latexBlock`，`text = content.trim()`。
  - `^ {0,3}\\\[[ \t]*(?:\n)?([\s\S]*?)\\\][ \t]*(?:\n|$)` 同上。
  - pending 形态：`^ {0,3}\\\[[ \t]*(?:\n)?([\s\S]*)$` 与 `^ {0,3}\$\$[ \t]*(?:\n)?([\s\S]*)$`（后者还需 `looksLikePendingDollarMath`），且都要求 `!containsTableStart`。
  - **issue #11159 修复**：块级数学不得跨过 GFM 表格；否则未闭合 `$$` 会吞掉后续表格。
- `start` 函数：块级 `/(?:^|\n) {0,3}(?:\$\$|\\\[)/` 返回 `match.index + (match[0].startsWith("\n") ? 1 : 0)`；行内取 `$`、`\(`、`\[` 三处 `indexOf` 的最小值。

`trimPartialClosingFences(tokens)`：只检查**最后一个 token**；`list` → 递归最后一个 item 的 tokens；`blockquote` → 递归 tokens；`code` → 取 `raw` 首行的 fence marker `^(`{3,}|~{3,})`；取 `raw` 最后一行；若 `lastLine.length < marker.length` 且 `lastLine === marker[0].repeat(lastLine.length)` → `token.text = token.text.slice(0, -lastLine.length).replace(/\n$/, "")`。（流式渲染时不闪烁；测试 `stabilizes partial closing fence rendering`。）

#### 2.16.3 `class Markdown implements Component`

字段：`text`、`paddingX`、`paddingY`、`defaultTextStyle?`、`theme`、`options`（构造时浅拷贝）、`defaultStylePrefix?`、渲染缓存 `{cachedText?, cachedWidth?, cachedLines?}`、`cachedTokens?: WeakRef<{source, tokens}>`。

构造：`constructor(text, paddingX, paddingY, theme, defaultTextStyle?, options?)`（**paddingX/paddingY 无默认值，必填**）。

`setText(text)` → 赋值 + `invalidate()`。
`invalidate()` → 清三份渲染缓存（**不清 token 缓存，也不清 `defaultStylePrefix`**——后者只在首次 `getDefaultStylePrefix()` 时写入，此后即使主题函数变化也不会重算；既有行为，主题应在构造时确定）。

`render(width)`：
1. 缓存命中 → 返回。
2. `contentWidth = max(1, width - paddingX*2)`；`text = options.transform?.(this.text, contentWidth) ?? this.text`。
3. 空文本（`!text || text.trim() === ""`）→ 缓存并返回 `[]`。
4. `normalizedText = text.replace(/\t/g, "   ")`。
5. token 缓存：`cached?.source === normalizedText` 则复用；否则 `tokens = markdownParser.lexer(normalizedText)`；`trimPartialClosingFences(tokens)`；`cachedTokens = new WeakRef({source, tokens})`。
6. 逐 token：`renderToken(token, contentWidth, nextToken?.type)`，拼接 `renderedLines`。
7. 换行：`isImageLine(line)` 的行原样保留；否则 `wrapTextWithAnsi(line, contentWidth)` 展开。
8. 加边距与背景：`leftMargin = rightMargin = " ".repeat(paddingX)`；`bgFn = defaultTextStyle?.bgColor`；image 行原样；否则 `lineWithMargins = left + line + right`；有 `bgFn` → `applyBackgroundToLine(..., width, bgFn)`；否则补齐到 `width`。
9. `emptyLine = " ".repeat(width)`；`paddingY` 个（有 bgFn 时着色）。
10. `result = emptyLines.concat(contentLines, emptyLines)`；`flattenLines`；写缓存；`result.length > 0 ? result : [""]`。

`applyDefaultStyle(text)`：依次应用 `color` →（若有）`theme.bold` → `theme.italic` → `theme.strikethrough` → `theme.underline`（**背景不在此处**，在填充阶段）。
`getDefaultStylePrefix()`：用哨兵 `"\u0000"` 走同一串着色，取哨兵前的 ANSI 前缀并缓存到 `defaultStylePrefix`。
`getStylePrefix(styleFn)`：同样用哨兵取前缀（**每次调用都重新计算，不缓存**；仅 `getDefaultStylePrefix` 缓存到 `defaultStylePrefix`）。
**注意**：`applyDefaultStyle` 与两个 prefix 函数在 `defaultTextStyle` 为 `undefined` 时**直接返回原文/空串**（不做任何 `theme.bold` 等调用）；`defaultStylePrefix` 只在有 `defaultTextStyle` 时才可能非空。
`getDefaultInlineStyleContext()`：`{applyText: applyDefaultStyle, stylePrefix: getDefaultStylePrefix()}`。

`renderToken(token, width, nextTokenType?, styleContext?)` 分支：

- **heading**：`headingPrefix = "#".repeat(depth) + " "`；h1 样式 = `theme.heading(theme.bold(theme.underline(t)))`，其它 = `theme.heading(theme.bold(t))`；构建 heading 专用 `InlineStyleContext`（使内联 code/bold 的 ANSI 重置后恢复 heading 样式）；`styledHeading = depth >= 3 ? headingStyleFn(prefix) + headingText : headingText`（**h1/h2 不打印 `#` 前缀，h3+ 打印**）；若 `nextTokenType && nextTokenType !== "space"` 追加 `""`。
- **paragraph**：渲染内联；若 `nextTokenType && nextTokenType !== "list" && nextTokenType !== "space"` 追加 `""`。
- **text**：`renderInlineTokens([token], styleContext)`。
- **latexBlock**：非 pending 且 `renderLatex !== false` → `renderLatex(text, {display:true}) ?? raw.trim()`；否则 `raw.trim()`；按 `\n` 拆行并 `applyDefaultStyle`；若下一 token 非 space 追加 `""`。
- **code**：`indent = theme.codeBlockIndent ?? "  "`；首行 `theme.codeBlockBorder("\`\`\`" + (lang || ""))`（**【逐字】** 三个反引号 + 语言）；有 `highlightCode` → 每行 `${indent}${hlLine}`；否则 `token.text.split("\n")` 每行 `${indent}${theme.codeBlock(line)}`；末行 `theme.codeBlockBorder("\`\`\`")`；若下一 token 非 space 追加 `""`。
- **list**：`renderList(token, 0, width, styleContext)`。
- **table**：`renderTable(...)`。
- **blockquote**：
  - `quoteStyle = t => theme.quote(theme.italic(t))`；`quoteStylePrefix = getStylePrefix(quoteStyle)`；`applyQuoteStyle(line)`：若 prefix 为空 → `quoteStyle(line)`；否则 `line.replace(/\x1b\[0m/g, `\x1b[0m${quoteStylePrefix}`)` 后再 `quoteStyle(...)`（**每次内联重置后重新施加引用样式**）。
  - `width < 3` → 无边框渲染子 token（防止嵌套引用指数膨胀）。
  - 否则 `quoteContentWidth = width - 2`；子 token 用 `renderToken`（块级）并以 `{applyText: t => t, stylePrefix: quoteStylePrefix}` 的上下文渲染（**默认消息样式不进引用**）。
  - 去掉尾部空行；每行 `applyQuoteStyle` 后 `wrapTextWithAnsi(line, quoteContentWidth)`，再 `theme.quoteBorder("│ ") + wrappedLine`（**【逐字】** U+2502 + 空格）。
  - 若下一 token 非 space 追加 `""`。
- **hr**：`lines.push(theme.hr("─".repeat(Math.min(width, 80))))`；下一 token 非 space 追加 `""`。
- **html**：`applyDefaultStyle(token.raw.trim())`。
- **space**：`lines.push("")`。
- **default**：若有 `text` 字符串则原样推入。

`renderInlineTokens(tokens, styleContext?)`：`resolvedStyleContext = styleContext ?? getDefaultInlineStyleContext()`；`applyTextWithNewlines(text)` 按 `\n` 分段分别 `applyText` 再拼回。分支：
- **latex**：非 pending 且 renderLatex 开启 → `renderLatex(text) ?? raw`，否则 `raw`；`applyTextWithNewlines`。
- **escape**：`preserveBackslashEscapes ? token.raw : token.text`。
- **text**：有嵌套 tokens → 递归；否则 `applyTextWithNewlines(token.text)`。
- **paragraph**：递归 `token.tokens`。
- **strong**：`theme.bold(inner) + stylePrefix`。
- **em**：`theme.italic(inner) + stylePrefix`。
- **codespan**：`theme.code(token.text) + stylePrefix`。
- **link**：
  - `styledLink = theme.link(theme.underline(inner))`；
  - 若 `getCapabilities().hyperlinks` → `hyperlink(styledLink, token.href) + stylePrefix`（**不再内联打印 URL**）；
  - 否则：`hrefForComparison = href.startsWith("mailto:") ? href.slice(7) : href`；若 `token.text === token.href || token.text === hrefForComparison` → `styledLink + stylePrefix`；否则 `styledLink + theme.linkUrl(` (${token.href})`) + stylePrefix`（**【逐字】** 空格 + 左括号 + URL + 右括号）。
- **br**：`"\n"`。
- **del**：`theme.strikethrough(inner) + stylePrefix`。
- **html**：`applyTextWithNewlines(token.raw)`。
- **default**：`text` 原样。
- 收尾：`while (stylePrefix && result.endsWith(stylePrefix)) result = result.slice(0, -stylePrefix.length)`（去掉尾随样式前缀，避免污染 padding —— 测试 `should not leak h1 underline into padding when inline code is the last token`）。

`renderList(token, depth, width, styleContext?)`：
- `indent = "    ".repeat(depth)`（**4 空格/层**）。
- `startNumber = typeof token.start === "number" ? token.start : 1`。
- 每个 item（`isLastItem = i === token.items.length - 1`）：
  - `bullet`：有序 → `preserveOrderedListMarkers ? (getOrderedListMarker(item) ?? `${startNumber + i}. `) : `${startNumber + i}. ``；无序 → `preserveOrderedListMarkers ? (getUnorderedListMarker(item) ?? "- ") : "- "`。
  - `taskMarker = item.task ? `[${item.checked ? "x" : " "}] ` : ""`。
  - `marker = bullet + taskMarker`；`firstPrefix = indent + theme.listBullet(marker)`；`continuationPrefix = indent + " ".repeat(visibleWidth(marker))`。
  - `itemWidth = max(1, width - visibleWidth(firstPrefix))`；`renderedAnyLine = false`。
  - 遍历 `item.tokens`：
    - **嵌套 `list`**：直接 `lines.push(...renderList(itemToken, depth+1, width, styleContext))`（**嵌套列表行不加外层 prefix**），并置 `renderedAnyLine = true`；`continue`。
    - 其它：`itemLines = renderToken(itemToken, itemWidth, undefined, styleContext)`；对每行再 `wrapTextWithAnsi(line, itemWidth)`，逐行加 `renderedAnyLine ? continuationPrefix : firstPrefix` 后 push，并置 `renderedAnyLine = true`。
  - `!renderedAnyLine` → `lines.push(firstPrefix)`（空 item 也占一行）。
  - `token.loose && !isLastItem` → `lines.push("")`。
- 返回 `lines`（**列表自身不追加尾部空行**；间距由后续 `space` token 或下一块的规则决定）。

`getOrderedListMarker(item)`：在 **`item.raw`** 上匹配 `/^(?: {0,3})(\d{1,9}[.)])[ \t]+/`，命中返回 `${m[1]} `（**保留 `)` 分隔符与原数字**）。
`getUnorderedListMarker(item)`：在 `item.raw` 上匹配 `/^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|$))/`，命中返回 `${m[1]} `（**统一为 marker 字符 + 一个空格**，原多空格被归一）。

`getLongestWordWidth(text, maxWidth?)`：`text.split(/\s+/).filter(w => w.length > 0)` 取 `visibleWidth` 最大值；有 `maxWidth` 则 `min(longest, maxWidth)`。
`wrapCellText(text, maxWidth, stylePrefix = "")`：`wrapTextWithAnsi(text, max(1, maxWidth))`；每行若非最后一行追加 `"\x1b[22;23;24;25;27;28;29;39m"`（**重置粗/斜/下划线/闪烁/反显/隐藏/删除线/前景**），再追加 `stylePrefix`。

`renderTable(token, availableWidth, nextTokenType?, styleContext?)`：
1. `numCols = token.header.length`；0 → `[]`。
2. `borderOverhead = 3 * numCols + 1`（`│ ` + (n-1)*` │ ` + ` │`）；`availableForCells = availableWidth - borderOverhead`；若 `< numCols` → **回退为原始 markdown**：`token.raw ? wrapTextWithAnsi(token.raw, availableWidth) : []`，若下一 token 非 space 追加 `""`；返回。
3. `maxUnbrokenWordWidth = 30`。
4. 自然宽度 `naturalWidths[i]` 与最小词宽 `minWordWidths[i]`（表头先行，再逐行 `max`；`minWordWidths` 至少 1，且被 `getLongestWordWidth(..., 30)` 夹取）。
5. 若 `sum(minWordWidths) > availableForCells`：全部最小宽置 1；`remaining = availableForCells - numCols`；按 `max(0, w-1)` 权重分配 `floor(weight/totalWeight * remaining)`；剩余逐个 +1；重算 `minCellsWidth`。
6. 若 `sum(naturalWidths) + borderOverhead <= availableWidth` → `columnWidths = max(natural, min)`。
7. 否则按 `totalGrowPotential = sum(max(0, natural - min))`、`extraWidth = max(0, availableForCells - minCellsWidth)` 计算 `min + floor(minDelta/totalGrowPotential * extraWidth)`；再做**轮转补齐**：`remaining = availableForCells - allocated`，每轮对 `columnWidths[i] < naturalWidths[i]` 的列 +1 直到 `remaining` 用尽或无可增长。
8. 顶部边框：`┌─` + `"─".repeat(w)` 以 `─┬─` 连接 + `─┐`（**逐字** box drawing：`┌ ─ ┬ ┐`）。
9. 表头：每个 cell 内联渲染 + `wrapCellText(text, columnWidths[i], styleContext?.stylePrefix)`；`headerLineCount = max(行数)`；每行各列补空格到列宽并 `theme.bold(...)`；行格式 `` `│ ${parts.join(" │ ")} │` ``。
10. 分隔线：`├─` + `─`*w 以 `─┼─` 连接 + `─┤`；表头后必有一条。
11. 数据行同样换行渲染，行格式 `` `│ ${parts.join(" │ ")} │` ``；**相邻数据行之间**插入同一条分隔线（即 `rows.length - 1` 条）。
12. 底部：`└─` + `─`×w 以 `─┴─` 连接 + `─┘`。
13. 若下一 token 非 space 追加 `""`。

**测试基准**（`markdown.test.ts` 92 个 it）：
- Transforms（1）：同一 source 与 width 时 transform 结果被缓存（`transform` 调用次数不随重复 render 增长）。
- Lists（16）：简单嵌套、深层嵌套、有序嵌套、默认归一化有序 marker、`preserveOrderedListMarkers`、混合有序无序、loose 列表空行、任务列表 `[x] `/`[ ] `、LLM 输出中代码块不缩进时编号保持、换行缩进（无序/有序/多位数有序/嵌套/有序父级下的嵌套）、列表项内引用与代码块渲染并换行。
- Tables（15）：简单表格、数据行间分隔线数量为 2（表头 + 1）、列宽至少为最长单词、对齐语法、不同列宽、超宽时 cell 换行、换行链接样式不泄漏到边框/普通 cell、换行后恢复外层样式、长 cell 多行、长不可断 token 在 cell 内换行（不只在行首）、styled inline code 换行不破边框、极窄宽度优雅降级、自然放下时正确、`paddingX` 参与表格宽度、表格为最后块时不追加尾空行。
- Combined（1）。
- LaTeX（12）：行内 `$`/`\(`；display `$$` 不被 Markdown 转义破坏；display `\[`；矩阵行与开括号对齐；display 运算符下限；列表与表格中的数学；货币/Shell 变量/代码跨度不当作数学；不支持与不完整 LaTeX 原样保留；流式不完整反斜杠定界符保留；转义定界符与代码围栏内不渲染；可禁用（`renderLatex:false`）；**流式定界符闭合后从原文切换到渲染**（该 `it` 缩进为 2、位于 `unclosed display math…` 的 `describe` **之后**、仍属 `LaTeX math` describe）。
- unclosed display math before tables #11159（8）：孤儿 `$$` 后仍渲染表格；含数学样内容亦然；不跨表格配对后面的 `$$`；无管道对齐分隔符的单列表格；孤儿 `\[` 后表格；不跨表格配对 `\[`；无管道表头的表格；闭合的多行 display math 后跟表格仍渲染。
- Backslash escapes（2）：默认归一化转义标点；`preserveBackslashEscapes` 保留原文。
- Pre-styled text（3）：灰色斜体在 inline code 后保留、在 bold 后保留、在 TUI 中不泄漏到后续行。
- Spacing after code blocks（3）/ dividers（2）/ headings（2）/ blockquotes（2）：块与后续段落之间只有 1 个空行；块为最后块时无尾空行。
- Deeply nested blockquotes（1）：嵌套超过宽度不爆炸。
- Blockquotes with multiline content（6）：lazy continuation 与显式多行引用样式一致；引用内列表；长行换行后每行带边框；换行缩进正确；引用内内联格式并在每次 reset 后恢复引用样式。
- Heading with inline code（4）：h2/h1 后样式保留；h1 下划线不泄漏到 padding；heading 在 bold 后保留。
- Strikethrough（2）、CJK emphasis（2）、Links（7：邮件/bare URL 不重复、不支持超链接时括号 URL、mailto 括号、OSC 8 三种支持场景）、HTML tags（2）、Streaming code fences（1）。

### 2.17 `src/components/scroll-view.ts`（224 行）

```ts
export type ScrollViewScrollbar = "hidden" | "auto" | "always";
export interface ScrollViewOptions {
  axis?: "vertical";                  // 其它值抛错
  follow?: "none" | "end";
  primary?: boolean;
  overscroll?: "chain" | "contain";
  scrollbar?: ScrollViewScrollbar;
  scrollbarTrackStyle?: (text: string) => string;   // 默认 \x1b[90m…\x1b[39m
  scrollbarThumbStyle?: (text: string) => string;   // 默认 \x1b[37m…\x1b[39m
  scrollbarHideDelayMs?: number;                    // 默认 1000
}
export interface ScrollViewScrollToOptions { disableFollow?: boolean; }
```

`class ScrollView extends Container`：
- 构造：`axis !== undefined && axis !== "vertical"` → `throw new Error(`Unsupported ScrollView axis: ${axis}`)`（**【逐字】**）；`child = component`；**直接 `this.children.push(component)`**（不通过 `addChild`，因为 addChild 被重写为抛错）；其余字段与默认值。
- 属性访问器：`scrollTop`、`isFollowingEnd`、`viewportHeight`、`scrollbar`、`isScrollbarVisible`、`isScrollbarActive`。
  - `isScrollbarVisible`：`scrollbar === "always"` → `viewportHeight > 0`；否则 `scrollbar === "auto" && contentHeight > viewportHeight && transientScrollbarVisible`。
- `setScrollbar(s)`：相同则 return；设置；`s !== "auto"` → `hideTransientScrollbar()`；否则若 `scrollbarActive` → `markScrollbarActivity()`；`requestRenderCallback?.()`。
- `getContentWidth(width)`：`scrollbar === "always" && width > 1 ? width - 1 : width`（为滚动条列预留 1 列）。
- `markScrollbarActivity()`：仅 `scrollbar === "auto" && contentHeight > viewportHeight`；`transientScrollbarVisible = true`；清旧定时器；若已 active 直接 return（**active 时不安排隐藏**）；否则 `setTimeout(隐藏, scrollbarHideDelayMs)` 并 `unref()`。
- `hideTransientScrollbar()`：置 false 并清定时器。
- `setScrollbarActive(active)`：变化时设置 + `markScrollbarActivity()` + requestRender。
- `scrollTo(scrollTop, options = {})`：
  - `requested = Number.isFinite(scrollTop) ? Math.trunc(scrollTop) : currentScrollTop`；
  - `maxScrollTop = max(0, contentHeight - viewportHeight)`；`next = clamp(requested, 0, maxScrollTop)`；
  - `nextFollowSuppressedAtEnd = options.disableFollow === true && next === maxScrollTop`；
  - `nextFollowingEnd = !nextFollowSuppressedAtEnd && followEnd && next === maxScrollTop`；
  - 三态均无变化 → return；`moved = next !== currentScrollTop`；写状态；`moved` 时 `markScrollbarActivity()`；requestRender。
- `scrollBy(lines): number`（**返回未消费的 delta**）：
  - `requested = Number.isFinite(lines) ? trunc(lines) : 0`；0 → 0。
  - `maxScrollTop`；`start = followingEnd ? maxScrollTop : currentScrollTop`；`next = clamp(start + requested)`；`moved = next - start`；
  - `wasFollowingEnd`；写 `currentScrollTop`；`followingEnd = followEnd && next === maxScrollTop`；`followSuppressedAtEnd = false`；
  - `moved !== 0` → markActivity；`moved !== 0 || followingEnd 变化` → requestRender；
  - 返回 `requested - moved`。
- `scrollToStart()`：若 `currentScrollTop !== 0` 或 `followingEnd !== (followEnd && contentHeight <= viewportHeight)` → 置 0、`followingEnd = followEnd && contentHeight <= viewportHeight`、`followSuppressedAtEnd = false`、markActivity、requestRender。
- `scrollToEnd()`：`next = max(0, contentHeight - viewportHeight)`；变化时置位、`followingEnd = followEnd`、清 suppress、markActivity、requestRender。
- `updateLayout(contentHeight, viewportHeight, requestRender)`：
  - 归一化为非负整数；保存 `requestRenderCallback`；
  - `maxScrollTop`；`followingEnd` 时 `currentScrollTop = maxScrollTop`，否则夹取；
  - `currentScrollTop < maxScrollTop` → 清 `followSuppressedAtEnd`；
  - `followEnd && currentScrollTop === maxScrollTop && !followSuppressedAtEnd` → `followingEnd = true`；
  - `contentHeight <= viewportHeight` → `hideTransientScrollbar()`。
- `addChild` / `removeChild` / `clear`：分别抛 `new Error("ScrollView has exactly one child")`、`"ScrollView child cannot be removed"`、`"ScrollView child cannot be cleared"`。
- `render(width)`：`contentWidth = getContentWidth(width)`；`lines = child.render(contentWidth)`；若 `contentWidth === width` 原样返回，否则每行追加一个空格 `` `${line} ` ``（**注意：不截断、不加宽到 width，仅补 1 个空格；行宽因此 = child 行宽 + 1**）。
- `[LAYOUT_NODE]()`：`{ type: "scroll", component: child, state: this }`。

`ScrollLayoutState` 接口（tui-core 定义）由本类实现：`scrollTop`、`primary`、`overscroll`、`viewportHeight`、`getContentWidth`、`updateLayout`。

滚动条几何由 `layout.ts` 计算（单元 tui-core，但本单元测试覆盖）：`thumbHeight = max(min(2, trackHeight), min(trackHeight, round(trackHeight² / contentHeight)))`；`maxScrollTop = max(0, contentHeight - trackHeight)`；`thumbOffset = maxScrollTop === 0 ? 0 : round(scrollTop / maxScrollTop * (trackHeight - thumbHeight))`；列 = `rect.x + rect.width - 1`，若不在 clip 内则隐藏。绘制字符：thumb `█`（active）或 `┃`，track `│`，分别经 `scrollbarThumbStyle` / `scrollbarTrackStyle`。

`layout.test.ts` 基准：`tracks follow-end state and returns unused scroll delta`、`renders a proportional glyph scrollbar with an expanded active thumb`（含 hover 展开与拖拽）、`preserves only the underlying background beneath overlay scrollbar glyphs`、`updates reserved scrollbar layout at runtime`、`measures nested scroll content from constrained child geometry`、`rebuilds geometry after content changes`、`crops Kitty images at a scroll view's lower boundary`、`does not render fixed-basis scroll content during stack measurement`、`paints only clipped rows from very large scroll content`。

### 2.18 `src/components/image.ts`（167 行）

```ts
export type ImageTranscoder = (base64Data: string, mimeType: string) => string | null;
export function setImageTranscoder(transcoder: ImageTranscoder | undefined): void;
export interface ImageTheme { fallbackColor: (str: string) => string; }
export interface ImageOptions {
  maxWidthCells?: number;
  maxHeightCells?: number;
  filename?: string;
  imageId?: number;            // Kitty 动画/更新时复用
}
export class Image implements Component {
  constructor(base64Data: string, mimeType: string, theme: ImageTheme, options: ImageOptions = {},
              dimensions?: ImageDimensions);
  getImageId(): number | undefined;
  invalidate(): void;
  render(width: number): string[];
}
```

模块级：
- `let imageTranscoder: ImageTranscoder | undefined`。
- `const pngCache = new Map<string, string | null>()`（按源数据键，LRU，**上限 32**）。
- `setImageTranscoder(t)`：赋值并 `pngCache.clear()`。
- `toPng(base64Data, mimeType)`：无 transcoder → null；缓存命中（含 `null`）直接用；否则调用并写回；命中后**删除再插入**（移动到最新）；`size > 32` 时删最旧键。

实例：
- 构造：保存参数；`dimensions = dimensions || getImageDimensions(base64Data, mimeType) || { widthPx: 800, heightPx: 600 }`（**默认 800×600**）；`imageId = options.imageId`；`pngData` 未定义。
- `invalidate()`：清 `cachedLines`/`cachedWidth`。
- `render(width)`：
  1. 缓存命中 → 返回。
  2. `maxWidth = max(1, min(width - 2, options.maxWidthCells ?? 60))`。
  3. `cellDimensions = getCellDimensions()`；`defaultMaxHeight = max(1, ceil(maxWidth * cellW / cellH))`；`maxHeight = options.maxHeightCells ?? defaultMaxHeight`（**默认高度上限 = 按单元像素比例换算的方形像素框**；测试 `caps Image component height to a square pixel box by default`：cell 10×20、maxWidth 10、图 10×100 → 5 行，序列含 `,c=1,r=5`）。
  4. `caps = getCapabilities()`；`data = base64Data`；`dimensions = this.dimensions`。
  5. Kitty 且 mime ≠ `image/png`：`pngData ??= toPng(...) ?? undefined`。**失败不被记住**：`toPng` 返回 null 时 `??=` 结果为 `undefined`，`pngData` 仍为未定义，因此下一次 `render`（或 `invalidate()` 后）**会再次调用 transcoder**——这正是测试 `renders a text fallback until a working transcoder is registered` 依赖的语义（无 transcoder → 兜底；注册返回 null 的 → 仍兜底；再注册可用的 → 渲染 Kitty）。Rust 侧必须能区分“未尝试/失败”与“成功”，**不要**用 `OnceCell` 把失败也缓存住。`data = pngData ?? null`；若有 PNG 则 `dimensions = getPngDimensions(data) ?? dimensions`（**转码可能应用 EXIF 旋转**，故尺寸优先取 PNG 头）。
  6. 若 `caps.images && data`：
     - Kitty 且 `imageId === undefined` → `imageId = allocateImageId()`（**构造时给了 `options.imageId` 则跳过分配直接复用**，供动画/更新；`getImageId()` 暴露该值，iTerm2 下恒为构造时给的值或 `undefined`）。
     - `result = renderImage(data, dimensions, { maxWidthCells: maxWidth, maxHeightCells: maxHeight, imageId, moveCursor: false })`。
     - 有结果：`imageId = result.imageId ?? imageId`；
       - Kitty：`lines = [result.sequence]` + `result.rows - 1` 个 `""`（`C=1` 阻止光标移动；返回 rows 行让 TUI 计高）。
       - iTerm2：先 `rows - 1` 个 `""`，最后一行 = `moveUp + result.sequence`，其中 `rowOffset = rows - 1`、`moveUp = rowOffset > 0 ? `\x1b[${rowOffset}A` : ""`（**光标上移、绘制、TUI 账户在滚动区内**）。
     - 无结果：`lines = [truncateToWidth(theme.fallbackColor(imageFallback(mimeType, this.dimensions, options.filename)), width)]`（注意用**原始** dimensions）。
  7. 否则（无能力或无数据）：同上 fallback。
  8. 缓存并返回。

`imageFallback(mimeType, dimensions?, filename?)`（tui-terminal）：
**【逐字】** `[Image: ${parts.join(" ")}]`，其中 parts =（可选 filename，`~` 缩写，若支持 OSC 8 且路径为绝对路径则用 `hyperlink(display, pathToFileURL(filename).href)`）+ `[${mimeType}]` +（可选 `${widthPx}x${heightPx}`）。
`hyperlink(text, url)`：**逐字** `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\`。

`terminal-image.test.ts` 中 Image 相关用例：`truncates long image fallback lines to render width`、`caps Image component height to a square pixel box by default`、`places image sequence on first line with empty padding rows`，以及 Kitty 尺寸/裁剪一致性与 `setImageTranscoder` 的 1.0.1 行为。

---

## 3. 用户可见表面（逐字清单）

### 3.1 键绑定表（`TUI_KEYBINDINGS`，逐字）

| id | defaultKeys | description |
|---|---|---|
| `tui.editor.cursorUp` | `up` | Move cursor up |
| `tui.editor.cursorDown` | `down` | Move cursor down |
| `tui.editor.historyPrevious` | `[]` | Select previous prompt history entry |
| `tui.editor.historyNext` | `[]` | Select next prompt history entry |
| `tui.editor.cursorLeft` | `["left","ctrl+b"]` | Move cursor left |
| `tui.editor.cursorRight` | `["right","ctrl+f"]` | Move cursor right |
| `tui.editor.cursorWordLeft` | `["alt+left","ctrl+left","alt+b"]` | Move cursor word left |
| `tui.editor.cursorWordRight` | `["alt+right","ctrl+right","alt+f"]` | Move cursor word right |
| `tui.editor.cursorLineStart` | `["home","ctrl+a"]` | Move to line start |
| `tui.editor.cursorLineEnd` | `["end","ctrl+e"]` | Move to line end |
| `tui.editor.jumpForward` | `ctrl+]` | Jump forward to character |
| `tui.editor.jumpBackward` | `ctrl+alt+]` | Jump backward to character |
| `tui.editor.pageUp` | `["pageUp","ctrl+pageUp"]` | Page up |
| `tui.editor.pageDown` | `["pageDown","ctrl+pageDown"]` | Page down |
| `tui.editor.deleteCharBackward` | `backspace` | Delete character backward |
| `tui.editor.deleteCharForward` | `["delete","ctrl+d"]` | Delete character forward |
| `tui.editor.deleteWordBackward` | `["ctrl+w","alt+backspace"]` | Delete word backward |
| `tui.editor.deleteWordForward` | `["alt+d","alt+delete"]` | Delete word forward |
| `tui.editor.deleteToLineStart` | `ctrl+u` | Delete to line start |
| `tui.editor.deleteToLineEnd` | `ctrl+k` | Delete to line end |
| `tui.editor.yank` | `ctrl+y` | Yank |
| `tui.editor.yankPop` | `alt+y` | Yank pop |
| `tui.editor.undo` | `ctrl+-` | Undo |
| `tui.input.newLine` | `["shift+enter","ctrl+j"]` | Insert newline |
| `tui.input.submit` | `enter` | Submit input |
| `tui.input.tab` | `tab` | Tab / autocomplete |
| `tui.input.copy` | `ctrl+c` | Copy selection |
| `tui.select.up` | `up` | Move selection up |
| `tui.select.down` | `down` | Move selection down |
| `tui.select.pageUp` | `pageUp` | Selection page up |
| `tui.select.pageDown` | `pageDown` | Selection page down |
| `tui.select.confirm` | `enter` | Confirm selection |
| `tui.select.cancel` | `["escape","ctrl+c"]` | Cancel selection |
| `tui.altScreen.pageUp` | `pageUp` | Scroll viewport up one page |
| `tui.altScreen.pageDown` | `pageDown` | Scroll viewport down one page |
| `tui.altScreen.halfPageUp` | `[]` | Scroll viewport up half a page |
| `tui.altScreen.halfPageDown` | `[]` | Scroll viewport down half a page |
| `tui.altScreen.lineUp` | `[]` | Scroll viewport up one line |
| `tui.altScreen.lineDown` | `[]` | Scroll viewport down one line |
| `tui.altScreen.previousPrompt` | `["ctrl+shift+up","ctrl+up"]` | Jump to previous semantic prompt |
| `tui.altScreen.nextPrompt` | `["ctrl+shift+down","ctrl+down"]` | Jump to next semantic prompt |
| `tui.altScreen.search` | `ctrl+shift+f` | Search the primary scroll view |
| `tui.altScreen.searchNext` | `["enter","ctrl+g"]` | Select the next search match |
| `tui.altScreen.searchPrevious` | `["shift+enter","ctrl+shift+g"]` | Select the previous search match |
| `tui.altScreen.searchClose` | `escape` | Close transcript search |
| `tui.altScreen.top` | `ctrl+home` | Scroll viewport to top |
| `tui.altScreen.bottom` | `ctrl+end` | Scroll viewport to bottom |

（`KeybindingsManager` 与 `getKeybindings` 属 tui-core；本单元只**消费** `getKeybindings().matches(data, id)` 与 `getKeys(id)`。）

### 3.2 各组件额外硬编码按键（不经 keybinding 表）

| 组件 | 按键 | 行为 |
|---|---|---|
| Input | `\n`（裸换行） | 提交（与 `tui.input.submit` 等价） |
| Input | `\x1b[200~` / `\x1b[201~` | 括号粘贴开始/结束 |
| Editor | `\x1b\r`（Alt+Enter）、`\x1b[13;2~`（Shift+Enter 变体）、多字节且含 `\x1b` 与 `\r`、`"\n"` | 换行 |
| Editor | 行尾反斜杠 + `enter` | 删反斜杠 + 换行（**仅当 `tui.input.submit` 含 `shift+enter`/`shift+return`**） |
| Editor | `shift+backspace` | 等价 `deleteCharBackward` |
| Editor | `shift+delete` | 等价 `deleteCharForward` |
| Editor | `shift+space` | 插入一个空格（**覆盖 Kitty CSI-u 对 shift+space 的编码**） |
| Editor | `ctrl+]` 后再按任意可打印键 | 向前跳到该字符 |
| Editor | `ctrl+alt+]` 后再按任意可打印键 | 向后跳到该字符 |
| Editor | 再次按 jump 键 / `escape` | 取消 jump 模式（`escape` 取消后仍继续处理该 Escape） |
| SettingsList | `space`、`enter` | 激活选中项（循环 `values` 或打开 `submenu`） |
| SettingsList | `escape` | 关闭子菜单 / 触发 `onCancel` |
| SettingsList | 可打印字符（搜索开启时） | 追加到搜索框并模糊过滤 |
| SelectList | `up`/`down` | 循环选择（首项上 → 末项，末项下 → 首项） |
| SelectList | `enter` | `onSelect(selected)` |
| SelectList | `escape`/`ctrl+c` | `onCancel()` |
| ScrollView | 无 | 纯被动的布局/滚动状态对象，按键由父级（alt-screen）驱动 |
| CancellableLoader | `escape`/`ctrl+c`（`tui.select.cancel`） | `onCancel?.()` |

### 3.3 全部用户可见字符串（逐字）

**SelectList**
- 无匹配：`"  No matching commands"`（**两个前导空格**），经 `theme.noMatch`。
- 滚动信息：`` `  (${selectedIndex + 1}/${filteredItems.length})` ``（两个前导空格，经 `theme.scrollInfo`，先 `truncateToWidth(text, width - 2, "")`）。
- 选中前缀 `"→ "`（U+2192 + 空格），未选中 `"  "`。
- 主列与描述之间 `PRIMARY_COLUMN_GAP = 2` 个空格。

**SettingsList**
- 空列表：`"  No settings available"`（经 `theme.hint`，**不截断**）。
- 无匹配：`"  No matching settings"`（经 `theme.hint`，截断到 `width`）。
- 滚动信息：`` `  (${selectedIndex + 1}/${displayItems.length})` ``（经 `theme.hint`，先截断到 `width - 2`）。
- 提示行 1：`"  Type to search · Enter/Space to change · Esc to cancel"`（搜索开启）。
- 提示行 2：`"  Enter/Space to change · Esc to cancel"`（搜索关闭）。
  （分隔符是 **U+00B7 中点**，前后各一空格；两行都有两个前导空格；`addHintLine` 先推一个 `""`。）
- label 与 value 之间固定 `"  "`。
- 值列最大宽 `valueMaxWidth = width - (prefixWidth + maxLabelWidth + 2) - 2`。
- 描述行：先推 `""`，然后 `wrapTextWithAnsi(description, width - 4)` 每行加 `"  "` 前缀（经 `theme.description`）。

**Loader / CancellableLoader**
- 默认消息 `"Loading..."`（**三个英文句点**）。
- 默认帧 `["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"]`（U+280B, U+2819, U+2839, U+2838, U+283C, U+2834, U+2826, U+2827, U+2807, U+280F）。
- 默认间隔 80 ms。
- 渲染文本 = `indicator + " " + message`（indicator 非空时；帧后**一个空格**）；`render(width)` 前置一个 `""` 空行。

**AltScreenFlashContainer**
- 每条：`` `\x1b[7m${truncateToWidth(` ${message} `, width, "")}\x1b[27m` ``（反显 + 前后各一空格）。

**Editor 边框/指示**
- 无隐藏行：`"─".repeat(width)`，经 `borderColor`。
- 隐藏行（宽足够）：`` "─".repeat(leftWidth) + ` ↑ ${n} more ` + "─".repeat(right) `` 或 `↓`。
- 窄时降级：`` `─── ↑ ${n} more ` + "─".repeat(remaining) ``。
- 极窄：`sliceByColumn(indicator, 0, availableWidth - 3, true) + "..."`。
- **【逐字】** 指示文本形如 ` ↑ 9 more `（方向符 U+2191/U+2193，左右各一空格，`more` 前一个空格）。

**Editor 粘贴标记**
- 多行：`` `[paste #${id} +${lineCount} lines]` ``。
- 长单行：`` `[paste #${id} ${charCount} chars]` ``。
- （只有 id、无后缀的 `[paste #N]` 也能被正则识别，用于兼容旧快照。）

**Input**
- 默认 prompt `"> "`；默认 placeholder `""`。

**Image**
- 兜底：`"[Image: " + parts.join(" ") + "]"`，parts 顺序 = 文件名 → `"[mime]"` → `"WxH"`。
- 文件名按家目录缩写为 `~/...`；支持超链接且路径为绝对路径时包 OSC 8。

**Markdown**
- 代码块围栏：`` "```" + lang `` / `"```"`。
- 代码块缩进默认 `"  "`。
- 引用边框 `"│ "`（U+2502 + 空格）。
- 水平线：`"─".repeat(min(width, 80))`（U+2500）。
- 列表：无序默认 `"- "`（`preserveOrderedListMarkers` 时保留 `-`/`+`/`*` 与原有空格）；有序默认 `` `${n}. ` ``；任务标记 `"[x] "` / `"[ ] "`。
- 表格字符：`┌ ┬ ┐ │ ├ ┼ ┤ └ ┴ ┘ ─`。
- 链接无超链接时后缀 `` ` (${href})` ``。
- 标题前缀 `"#".repeat(depth) + " "`，**仅 depth ≥ 3 输出**。
- 引用/表格/代码块后若下一 token 非 `space` 则追加一个空行 `""`。

**ScrollView**
- 错误：`"Unsupported ScrollView axis: ${options.axis}"`、`"ScrollView has exactly one child"`、`"ScrollView child cannot be removed"`、`"ScrollView child cannot be cleared"`。
- 滚动条字形：thumb `"█"`（U+2588）、展开态 `"┃"`（U+2503）、track `"│"`（U+2502）。

**TUI（宽度违约，属 tui-core，但由组件保证不触发）**
```
Rendered line <i> exceeds terminal width (<w> > <width>).

This is likely caused by a custom TUI component not truncating its output.
Use visibleWidth() to measure and truncateToWidth() to truncate lines.

Debug log written to: <path>/kiana-tui-crash.log
```

### 3.4 组件用到的 ANSI 序列（逐字）

| 用途 | 序列 |
|---|---|
| 真实光标标记（APC） | `\x1b_kiana:c\x07`（`CURSOR_MARKER`） |
| 假光标开始/结束 | `\x1b_kiana:fc\x07` / `\x1b_kiana:/fc\x07` |
| 反显开/关（flash） | `\x1b[7m` / `\x1b[27m` |
| 滚动条 track 默认 | `\x1b[90m…\x1b[39m` |
| 滚动条 thumb 默认 | `\x1b[37m…\x1b[39m` |
| 表格 cell 续行样式重置 | `\x1b[22;23;24;25;27;28;29;39m` |
| Image 兜底超链接 | OSC 8：`\x1b]8;;<url>\x1b\\<text>\x1b]8;;\x1b\\` |
| Kitty 图形 | `\x1b_G<params>;<base64>\x1b\\`，参数顺序 `a=T,f=100,q=2[,C=1][,c=W][,r=H][,i=ID]`；分块 4096，首块 `,m=1`，中间 `m=1`，末块 `m=0` |
| Kitty 删除 | `\x1b_Ga=d,d=I,i=<id>,q=2\x1b\\` / `\x1b_Ga=d,d=A,q=2\x1b\\` / `\x1b_Ga=d,d=a,q=2\x1b\\` |
| iTerm2 图形 | `\x1b]1337;File=inline=1;size=<bytes>[;width=…][;height=…][;name=<b64>][;preserveAspectRatio=0]:<base64>\x07` |
| 光标上移 | `\x1b[<n>A` |

`SEGMENT_RESET`（tui-core 的 `compositeTuiLine` 用于换行内分段）：`"\x1b[0m\x1b]8;;\x07"`。

**假光标（v1.2.0 起的契约，规格此前未强调）**：`Editor.render`/`Input.render` **不再直接输出 `\x1b[7m`**，而是 `renderFakeCursor(text)` = `\x1b_kiana:fc\x07` + text + `\x1b_kiana:/fc\x07`（zero-width APC）；反显由 TUI 渲染器在 `resolveFakeCursors()` 阶段施加（`showHardwareCursor` 开启时则整段丢弃并在 `CURSOR_MARKER` 处露出硬件光标）。因此：
- 组件层测试必须断言 **APC 标记**而不是 `\x1b[7m`；`AltScreenFlashContainer` 是**唯一**仍直接输出 `\x1b[7m`/`\x1b[27m` 的组件。
- `CURSOR_MARKER` 只在该组件 `focused` 时发射，且**位于假光标之前**（`before + marker + renderFakeCursor(ch)`）。
- 在 TUI 之外单独渲染 `Editor`/`Input`（例如 Rust 单元测试）时，若要与“屏幕文本”比对，必须先剥离 `\x1b_kiana:fc\x07`/`\x1b_kiana:/fc\x07`（等价于 TS 侧 `stripTerminalSequences()` 的行为）。

### 3.5 布局宽度规则汇总（用户可见的“形状”）

- 每行 `visibleWidth(line) <= width` 是硬约束。
- SelectList：`width > 40` 且 item 有 description 才分两列；否则单列 `prefix + truncate(displayValue, width - prefixWidth - 2)`。
- SettingsList：label 列宽 `min(36, max(label 宽))`；值列至少 2 列安全边距。
- Editor：`maxVisibleLines = max(5, floor(rows * 0.3))`；左右各 `paddingX`；无 padding 时内容宽再减 1（光标位）。
- Input：prompt 固定占前部，文本区 `width - visibleWidth(prompt)`。
- Markdown：`contentWidth = max(1, width - paddingX*2)`；引用 `width - 2`；表格 `3n+1` 边框开销；hr 最多 80 列。
- Markdown 表格列最小词宽上限 30。
- Image：`maxWidth = max(1, min(width - 2, maxWidthCells ?? 60))`；默认高度按 cell 9×18 像素比例取方形像素框。
- ScrollView：`"always"` 滚动条占最右 1 列，内容每行补一个空格。

## 4. 外部格式与环境变量（逐字）

### 4.1 环境变量（组件间接依赖，经 `getCapabilities()`）

来源 `terminal-image.ts`（单元 tui-terminal），按下表顺序短路：

| 条件 | images | trueColor | hyperlinks |
|---|---|---|---|
| `TMUX` 或 `TERM` 以 `tmux` 开头 | `null` | `COLORTERM ∈ {truecolor,24bit}` 或 `TERM` 以 `-direct` 结尾 | 执行 `tmux display-message -p '#{client_termfeatures}'`（timeout 250 ms）输出含 `hyperlinks` |
| `TERM` 以 `screen` 开头 | `null` | 同上 | false |
| `TERM_PROGRAM=herdr` | `kitty` | 同上 | true |
| `TERM_PROGRAM=rex` | `kitty` | true | true |
| `KITTY_WINDOW_ID` 或 `TERM_PROGRAM=kitty` | `kitty` | true | true |
| `TERM_PROGRAM=ghostty` / `TERM` 含 `ghostty` / `GHOSTTY_RESOURCES_DIR` | `kitty` | true | true |
| `WEZTERM_PANE` 或 `TERM_PROGRAM=wezterm` | `kitty` | true | true |
| `TERM_PROGRAM=warpterminal` / `WARP_SESSION_ID` / `WARP_TERMINAL_SESSION_UUID` | `kitty` | true | true |
| `ITERM_SESSION_ID` 或 `TERM_PROGRAM=iterm.app` | `iterm2` | true | true |
| `WT_SESSION` | `null` | true | true |
| `TERM_PROGRAM ∈ {alacritty,vscode,zed}` | `null` | true | true |
| `TERMINAL_EMULATOR=jetbrains-jediterm` | `null` | true | false |
| `process.platform === "win32"` | `null` | true | false |
| 其它 | `null` | 颜色提示同上 | false |

覆盖变量（优先于检测）：
- `KIANA_HYPERLINKS`：`"1"` → true，`"0"` → false，其它 → 不覆盖。
- `KIANA_TRUE_COLOR`：同上。
- `KIANA_IMAGE_PROTOCOL`：`"kitty"`/`"iterm2"` 指定；`"none"`/`"0"` → `null`；其它 → 不覆盖。
- 影响 Markdown 链接渲染（是否 OSC 8）与 Image 兜底是否带超链接。

其它：
- `KIANA_TUI_DEBUG_REDRAW=1`：`tui-main-screen` 把重绘日志写到 `logDirectory`（tui-core）。
- `KIANA_TUI_DEBUG=1`：`tui-main-screen` 额外调试输出（tui-core）。
- `TERMUX_VERSION`：`tui-main-screen` 的 Termux 特殊分支（tui-core）。
- 本单元**不直接读取任何环境变量**（全部经 `getCapabilities()` / `getCellDimensions()`）。

### 4.2 崩溃日志文件

`<logDirectory ?? os.tmpdir()>/kiana-tui-crash.log`，UTF-8 文本，格式：
```
Crash at <ISO8601>
Terminal width: <width>
Line <i> visible width: <w>

=== All rendered lines ===
[0] (w=<w0>) <line0>
[1] (w=<w1>) <line1>
...

```
（最后一行是空行；写入前 `mkdirSync(dirname, {recursive:true})`。）

### 4.3 剪贴板 / 原生模块

组件层不直接调用剪贴板；`tui.input.copy` 在 Editor 中**直接返回**，由上层（coding-agent）处理。原生剪贴板模块（`native/`）不在本单元范围。

### 4.4 终端尺寸查询与单元格像素

`getCellDimensions()` 默认 `{widthPx: 9, heightPx: 18}`，由 `setCellDimensions` 在终端应答 CSI 16 t 后更新（单元 tui-core/terminal）。Image 组件的行列换算全部基于该值。


## 5. 跨单元接口

### 5.1 本单元**消费**的符号（必须由其它单元提供，签名逐字）

来自 **tui-core**（`src/tui.ts`）：

```rust
pub trait Component {
    fn render(&self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> { None }
    fn wants_key_release(&self) -> bool { false }
    fn invalidate(&mut self);
}
pub trait Focusable { fn focused(&self) -> bool; fn set_focused(&mut self, v: bool); }
// Container（tui.ts:358）：其它单元提供，本单元从中派生 Stack/ScrollView
pub struct Container { pub children: Vec<Box<dyn Component>> }   // 顺序 = 渲染顺序
impl Container {
    pub fn add_child(&mut self, c: Box<dyn Component>);            // push
    pub fn remove_child(&mut self, c: &dyn Component);             // splice 第一个相等者（Rust 需按指针/id 相等）
    pub fn clear(&mut self);                                       // children = []
    pub fn invalidate(&mut self);                                  // 逐个 child.invalidate()
    pub fn handle_mouse(&mut self, e: &TuiMouseEvent) -> Option<TuiMouseDispatchResult>;
    pub fn render(&mut self, width: usize) -> Vec<String>;         // 逐个 child.render(width) 顺序拼接
}
pub const CURSOR_MARKER: &str = "\x1b_kiana:c\x07";
pub fn render_fake_cursor(text: &str) -> String;
pub fn is_focusable(c: Option<&dyn Component>) -> bool;          // "focused" in component
pub fn dispatch_mouse_event(c: &mut dyn Component, e: &TuiMouseEvent) -> Option<TuiMouseDispatchResult>;
pub fn retarget_mouse_event(e: &TuiMouseEvent, t: &TuiMouseDispatchTarget) -> TuiMouseEvent;
pub fn composite_tui_line(base: &str, overlay: &str, start_col: usize, overlay_width: usize, total_width: usize) -> String;
pub trait TUI: Component {
    fn mode(&self) -> TuiMode;
    fn terminal(&self) -> &dyn Terminal;      // 需要 rows/cols
    fn request_render(&self, force: bool);
    fn set_focus(&mut self, c: Option<&mut dyn Component>);
    // ...（本单元实际只用 terminal().rows（Editor.render / pageScroll）、requestRender()（Editor 的 4 处 + Loader.updateDisplay），**setFocus 并未被本单元任何组件调用**；focus 由 tui-core 通过 Focusable 接口反向写入）
}
```

鼠标类型逐字（`tui.ts:25-100`）：

```rust
pub enum TuiMouseEventType { Press, Release, Move, Drag, Click, Wheel }
pub enum TuiMouseButton { Left, Middle, Right, None }
pub struct TuiMouseEvent {
    pub r#type: TuiMouseEventType, pub button: TuiMouseButton,
    pub x: i32, pub y: i32, pub screen_x: i32, pub screen_y: i32,
    pub width: usize, pub height: usize,
    pub shift: bool, pub alt: bool, pub ctrl: bool,
    pub wheel_delta: Option<i32>, pub click_count: Option<u32>,
}
pub struct TuiMouseEventResult { pub handled: bool, pub capture: bool, pub focus: bool, pub render: Option<bool> }
pub struct TuiMouseDispatchTarget { pub component: (), pub origin_x: i32, pub origin_y: i32, pub width: usize, pub height: usize }
pub struct TuiMouseDispatchResult { pub base: TuiMouseEventResult, pub target: TuiMouseDispatchTarget, pub focus_target: Option<...> }
```

`dispatch_mouse_event` 的关键规则（组件依赖它）：
- `handleMouse` 返回 `None` → `None`。
- 结果已含 `target`（即转发自子组件）：若 `focus && 组件有 handleInput` → `focus_target = 组件自身`（**保持委托容器焦点**）。
- 结果 `handled/capture/focus` 全 false → `None`。
- 否则 `handled = true`，`focus` 时 `focus_target = 组件`，`target.origin = (screenX - x, screenY - y)`，`target.width/height = event.width/height`。

来自 **tui-core**（`utils.ts`）：

```rust
pub fn visible_width(s: &str) -> usize;                       // 512 项缓存；tab=3
pub fn truncate_to_width(s: &str, max: usize, ellipsis: &str) -> String;   // 默认 "..."；先 \x1b[0m
pub fn slice_by_column(s: &str, start: usize, len: usize, strict: bool) -> String;
pub fn wrap_text_with_ansi(s: &str, width: usize) -> Vec<String>;
pub fn apply_background_to_line(line: &str, width: usize, bg: &dyn Fn(&str) -> String) -> String;
pub fn flatten_lines(lines: &mut Vec<String>);
pub fn is_whitespace_char(c: &str) -> bool;
pub fn get_grapheme_segmenter() -> &'static GraphemeSegmenter;   // Intl.Segmenter 等价
pub fn cjk_break_regex() -> &'static Regex;
pub const AUTOCOMPLETE_SEPARATOR_REGEX / AUTOCOMPLETE_BOUNDARY_REGEX: &'static Regex;
```

来自 **tui-core**（其它）：
- `keybindings.rs`：`get_keybindings() -> &KeybindingsManager`；`KeybindingsManager::matches(data, id) -> bool`；`get_keys(id) -> Vec<KeyId>`。
- `keys.rs`：`match_key(data, key: KeyId) -> bool`；`decode_kitty_printable(data) -> Option<&str>`；`decode_printable_key(data) -> Option<String>`。
- `kill_ring.rs`：`KillRing::{new, len, is_empty, push(text, {prepend, accumulate}), peek() -> Option<&str>, rotate()}`。语义（逐字）：`push` 对空串**直接 return**（不入 ring）；`accumulate && ring.length > 0` 时弹出最后一项并按 `prepend ? text + last : last + text` 合并再压回；否则直接压入。`rotate()` 仅当 `length > 1` 时把最后一项移到**最前**。`peek()` 取最后一项。**注意 Editor 与 Input 共享同一份实现**，而 `deleteWordBackwards` 在 `cursorCol === 0` 且无上一行时**不 push**；`deleteToStartOfLine` 在行首且有上一行时 push `"\n"`。
- `undo_stack.rs`：`UndoStack<S: Clone>::{new, len, push(state), pop() -> Option<S>, clear()}`。`push` 内部 `structuredClone(state)`（**深拷贝**；Rust 侧即 `state.clone()`），`pop` 返回已脱离的副本（不再克隆）。
- `word_navigation.rs`：`find_word_backward(text, cursor, opts?) -> usize`、`find_word_forward(...)`；`WordNavigationOptions { atomic?: Fn(&str) -> bool }`。
- `fuzzy.rs`：`fuzzy_filter(items, query, key_fn) -> Vec<Item>`（SettingsList 用）。
- `layout-node.rs`：`LAYOUT_NODE`、`StackLayoutNode { type, entries, gap, align }`、`ScrollLayoutState` trait、`ScrollLayoutNode`。
- `tui.rs`：`flattenLines`、`renderFakeCursor`、`Container`。

来自 **tui-terminal**（`terminal-image.ts`）：

```rust
pub struct TerminalCapabilities { pub images: Option<ImageProtocol>, pub true_color: bool, pub hyperlinks: bool }
pub fn get_capabilities() -> TerminalCapabilities;
pub fn get_cell_dimensions() -> CellDimensions;      // 默认 9x18
pub fn is_image_line(line: &str) -> bool;            // startsWith 或 includes "\x1b_G"/"\x1b]1337;File="
pub fn hyperlink(text: &str, url: &str) -> String;
pub fn image_fallback(mime: &str, dims: Option<ImageDimensions>, filename: Option<&str>) -> String;
pub fn get_image_dimensions(base64: &str, mime: &str) -> Option<ImageDimensions>;
pub fn get_png_dimensions(base64: &str) -> Option<ImageDimensions>;
pub fn render_image(base64_or_png: &str, dims: ImageDimensions, opts: ImageRenderOptions)
    -> Option<RenderedImage /* { sequence: String, rows: usize, image_id: Option<u64> } */>;
pub fn allocate_image_id() -> u64;
pub fn encode_kitty / encode_iterm2 / delete_kitty_image / delete_all_kitty_images / delete_all_kitty_placements;
```

来自 **tui-latex**：
```rust
pub fn render_latex(source: &str, options: RenderLatexOptions) -> Option<String>;   // display: bool
```

来自 **tui-autocomplete**：
```rust
pub struct AutocompleteItem { value: String, label: String, description: Option<String> }
pub struct AutocompleteSuggestions { items: Vec<AutocompleteItem>, prefix: String }
pub trait AutocompleteProvider {
    fn trigger_characters(&self) -> Option<Vec<char>>;
    fn get_suggestions(&mut self, lines: &[String], cursor_line: usize, cursor_col: usize,
                       opts: GetSuggestionsOptions) -> impl Future<Output = Option<AutocompleteSuggestions>>;
    fn apply_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize,
                        item: &AutocompleteItem, prefix: &str)
        -> ApplyCompletionResult /* { lines, cursor_line, cursor_col } */;
    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool { true }
}
pub struct GetSuggestionsOptions { pub signal: CancellationToken, pub force: bool }
```

### 5.2 本单元**暴露**给其它单元的符号

| 单元 | 使用的本单元符号 | 用途 |
|---|---|---|
| coding-agent（`custom-editor.ts`） | `Editor`（继承，重写 `renderTopBorder(width, hiddenLineCount)`；访问 `this.borderColor`、`super.renderTopBorder(...)`、`visibleWidth`；另有 `setWorkingStatusIndicator` 与自身字段 `embedWorkingStatus`、`onAction`、`onEscape`、`onCtrlD`、`onPasteImage`、`onExtensionShortcut`、`actionHandlers`） | 自定义 prompt 边框 |
| coding-agent（`status-indicator.ts`） | `Loader`（继承，重写 `getRenderedIndicator`） | 彩色状态指示 |
| coding-agent | `Text`、`Box`、`Spacer`、`VStack`、`HStack`、`TruncatedText`、`ScrollView`、`Markdown`、`Input`、`SelectList`、`SettingsList`、`Image` | UI 组装 |
| coding-agent | `MouseRegion` | 给非交互组件加鼠标处理（点击展开等） |
| tui-core（`layout.ts`） | `VStack`/`HStack` 的 `[LAYOUT_NODE]()`；`ScrollView` 的 `[LAYOUT_NODE]()` 与 `ScrollLayoutState` 实现 | 布局测量/绘制 |
| tui-alt-screen | `ScrollView`（`scrollTo/scrollBy/updateLayout/scrollbar`）、`AltScreenFlashContainer`（`flash/dispose`） | 全屏模式滚动与提示 |
| tui-core（`tui-main-screen`） | 只有 `Component` 契约 | 渲染循环 |

**继承/重写契约（Rust 侧必须保留可扩展性）**：
- `Editor`：`render_top_border(width, hidden)` / `render_bottom_border(width, hidden)` 必须是可覆盖的 trait 方法或 `pub fn` 字段上的钩子（coding-agent 实际只覆写 **`renderTopBorder`**，全仓没有覆写 `renderBottomBorder` 的子类；Rust 侧仍应两个都保留可覆写）；`border_color: Arc<dyn Fn(&str) -> String>` 必须 `pub`（`custom-editor.ts` 直接调用 `this.borderColor(...)`）。
  - **`lastWidth` 是 `private`，下游不读取**（全仓 grep 只在 `editor.ts` 内部使用）。Rust 侧无需为它开公开读接口，但 `Editor` 必须在 `render()` 时保存布局宽度供 `buildVisualLineMap` 使用。
  - `getPaddingX()`/`setPaddingX()` 的下游用途在 **`interactive-mode.ts:2861`**（`newEditor.setPaddingX(this.defaultEditor.getPaddingX())`，编辑器热替换时复制外观），**不在** `custom-editor.ts`；`setAutocompleteMaxVisible`/`getAutocompleteMaxVisible` 也在那里被同样复制。
- `Loader`：`get_rendered_indicator(&self) -> String` 必须可覆盖（`pub fn` + trait 或 `Loader` 上的 `#[doc(hidden)] pub` 字段）。
- `Text`：`set_text`、`set_custom_bg_fn` 必须 `pub`（`ThemedText`/`WriteCallRenderComponent` 继承）。
- `Box`：`children: Vec<...>`、`add_child`、`remove_child`、`clear`、`set_bg_fn`、`set_padding_x`、`invalidate` 必须 `pub`（下游 `CompactionSummaryMessageComponent` / `BranchSummaryMessageComponent` / `SkillInvocationMessageComponent` 继承后调用 `this.setPaddingX(...)` 并 `addChild`）。`Box` **没有** title / verticalPadding 之类的方法（源码只有 `paddingX`/`paddingY`/`bgFn` 三个可配置项，`paddingY` 只能在构造时给，之后无 setter）。
- `SelectList`/`SettingsList`/`Input`/`TruncatedText`：只作为具体类型被组合，无需 trait 化。

### 5.3 契约不变量（跨单元）

1. 任何组件 `render(w)` 返回的每行 `visible_width(line) <= w`（否则 tui-core 抛错并写崩溃日志）。
2. `invalidate()` 必须清空所有缓存，但**不改变**语义状态（光标、选择、滚动位置）。
3. `handleMouse` 只在需要时返回 `focus: true`；返回 `handled` 但无 `focus` 表示“吃掉事件但不抢焦点”。
4. 渲染函数不得有副作用（除 `Editor.render` 更新 `renderedVisibleLineCount`、`renderedAutocompleteHeight`、`scrollOffset` —— 这些是必要的渲染期状态，Rust 侧须用 `&mut self` 或 `Cell`）。
5. 定时器必须 `unref`（Node 语义）：Rust 侧对应“后台任务在 drop 时取消，不阻塞退出”。

## 6. npm 依赖与 Rust crate 对应

### 6.1 直接依赖（`packages/tui/package.json`）

| npm 包 | 版本 | 本单元用途 | Rust crate | 差异与桥接 |
|---|---|---|---|---|
| `marked` | 18.0.11 | Markdown lexer（`Marked`、`Tokenizer`、`TokenizerExtension`、`Tokens`、`Marked#use`） | 无直接等价。候选：`pulldown-cmark`（CommonMark + GFM tables/strikethrough/tasklist）、`comrak`（更全的 GFM）、`markdown-rs` | **最大的移植风险**。需要：① token 结构与 `marked` 对齐（`heading/paragraph/text/space/code/list/item/blockquote/hr/html/table/tableRow/tableCell/codespan/strong/em/link/del/br/escape/latex`）；② `lexer` 的 token 边界必须一致（列表 loose/tight、`item.task`、`ordered.start`、`code.lang`、`table.align`）；③ 扩展点（自定义 `del` tokenizer、latex block/inline extension、`emStrong` 替换）在 `pulldown-cmark` 中需**自写解析器或用 `markdown-rs` 的可插拔构造器**；④ 必须保留 `options.preserveOrderedListMarkers`/`preserveBackslashEscapes` 依赖的 raw 信息。**建议**：用 `markdown-rs`（可配置、token 级 API 接近 marked）或自写一个与 marked 对齐的 lexer，**不要**直接用 `comrak` 的 HTML 输出。 |
| `marked-cjk-friendly` | 0.1.2 | 替换 `emStrong` tokenizer，使 CJK 标点旁的 `**`/`*` 能解析 | 自研（无 crate） | 只用到其 `tokenizer.emStrong`。需要逐条移植其 flanking 规则（代码量小，约 100 行）：核心是当 `**` 紧邻 CJK 标点时放宽 CommonMark 的 left/right-flanking 判定。测试 `renders emphasis delimited next to CJK punctuation` 与 `keeps CommonMark flanking rules for non-CJK text` 是验收基准。 |
| `get-east-asian-width` | 1.6.0 | `eastAsianWidth(cp)` → 0/1/2 | `unicode-width`（`UnicodeWidthChar::width`） | **行为差异**：`get-east-asian-width` 返回 East Asian Width 属性的**离散值**（0=窄/模糊按 1，2=宽/全角），`unicode-width` 返回终端列宽（含 emoji 序列、零宽 joiner 处理）。必须逐码点对齐：`unicode-width` 的 `width()` 对 EastAsianWidth=W/F 给 2，对 A（ambiguous）给 1，对控制字符给 `None`，与本仓库的 `graphemeWidth` 组合后基本一致，但**区域指示符（RI）**、**emoji 序列**、**半宽/全角形式（U+FF00-FFEF）**必须专门验证。可考虑用 `unicode-width` + 自建 EAW 表（`unicode-width` 已内置）。另一条路：直接嵌入 `get-east-asian-width` 的生成表（UCD EastAsianWidth.txt）。 |
| `chalk`（dev） | 6.0.0 | 测试主题（level 3 truecolor） | `anstyle` / `owo-colors` / `nu-ansi-term` | 仅测试需要。Rust 测试主题建议直接用 `format!("\x1b[38;2;r;g;bm{}\x1b[39m", text)` 逐字构造，避免库的嵌套优化差异（chalk 会在某些情况合并/省略 reset，导致逐字不匹配）。 |
| `@xterm/headless`（dev） | 5.5.0 | `VirtualTerminal`（把渲染行的 ANSI 喂给 xterm 模型，断言屏幕文本/样式） | `vt100`（`vt100::Parser`）或 `avt`（`avt::Vt`） | **测试基础设施差异**：xterm 支持完整的 SGR/OSC/APC；`vt100` 忽略 OSC 8 与 APC，但本单元测试主要断言可见文本与 SGR 颜色/下划线。建议：自写一个轻量 “ANSI → 屏幕网格” 断言器（只处理 CSI m / K / H、OSC 8、APC 标记），才能逐字复刻 `VirtualTerminal.cell(x,y).getChars()`、`getFgColor()` 等 API。 |

### 6.2 标准库/运行时依赖（Node → Rust）

| Node API | 本单元用途 | Rust 对应 | 差异 |
|---|---|---|---|
| `Intl.Segmenter("grapheme")` | 所有光标/删除/换行/宽度 | `unicode-segmentation`（`graphemes(true)`） | **必须验证**：`unicode-segmentation` 遵循 UAX #29 的扩展 grapheme cluster，与 ICU 在 emoji ZWJ 序列、区域指示符对、`\r\n` 上一致；但在 **Prepend 类字符**（阿拉伯数字符号等）、**Indic conjunct break (GB9c, Unicode 15.1)** 上可能版本差异。仓库有针对 Thai/Lao AM 的专门处理（`graphemeWidth` 中 `0x0e33/0x0eb3` 记 1 列，且 AM 与其后辅音组成一个簇），需对齐 `unicode-segmentation` 的版本（用最新版并固定）。测试 `renders isolated Thai and Lao AM clusters without width drift`、`snaps mouse selection to CJK, emoji, and combining grapheme boundaries` 是验收。 |
| `Intl.Segmenter("word")` | 词移动（`findWordBackward/Forward`，tui-core） | `unicode-segmentation::UnicodeWordIndices` | ICU 词边界与本 crate 的 UAX #29 实现基本一致；但仓库另加 `PUNCTUATION_REGEX` 保留 ASCII 标点作为独立边界，需在 tui-core 层复刻（本单元只调用）。测试 `Ctrl+W preserves ASCII punctuation boundaries`、`stops at fullwidth Chinese punctuation (issue #4972)`。 |
| `String.prototype.length/slice` (UTF-16) | 编辑器所有偏移（`cursorCol`、`TextChunk.startIndex/endIndex`、paste marker 切片、`text.indexOf`） | **必须自建 UTF-16 索引层** | Rust `String` 是 UTF-8。三个选择：① 内部用 `Vec<u16>`（`widestring`）表示编辑器文本；② 用 `String` + 前缀索引表（`Vec<usize>` 把 UTF-16 偏移映射到字节偏移）；③ 用 `String` + 所有切片点都从 grapheme 迭代器取得（**最慢但最安全**）。**推荐 ②**：`Editor` 的 `lines: Vec<Utf16Line>`，`Utf16Line { text: String, utf16_len: usize }` + 按需线性扫描（行通常很短）。`wordWrapLine` 返回的 `startIndex/endIndex` 必须是 **UTF-16 偏移**，因为 `handleBackspace` 用它做 `slice`。 |
| `RegExp`（`u`/`v` flag，`\p{...}`，lookbehind，`matchAll`） | `PASTE_MARKER_REGEX`、`STRICT_STRIKETHROUGH_REGEX`、latex tokenizer、`containsTableStart`、`getOrderedListMarker`、`getUnorderedListMarker`、trigger/debounce 正则 | `regex` crate（无 lookaround/backreference） | **JS 正则方言差异**：① `\p{...}` 在 `regex` 中支持（`\p{Emoji}` 等），但 JS 的 `\p{RGI_Emoji}`（v flag）**不支持**，需自建 emoji 表或改用 `unicode-properties`；② `regex` 无 lookahead/lookbehind，`PASTE_MARKER_REGEX` 的 `(?=...)`（如 `STRICT_STRIKETHROUGH` 中 `(?=[^\s~])`、`(?=[^~]|$)`）需改写为手写解析；③ `v` flag 的集合差运算（`[\p{P}--\p{ASCII}]`）不支持；④ JS `.` 不含 `\n`，`regex` 默认同样；⑤ JS 的 `$` 与 `regex` 的 `$`（默认 `\z` 语义）需对齐。建议：对 marker/删除线/列表标记这些简单模式用**手写扫描器**，对 latex trigger 用 `regex` + 手工 lookahead 替代。 |
| `structuredClone` | 撤销快照 `{state, pastes: Map, pasteCounter}` | `#[derive(Clone)]` | 直接 `clone()`；`Map<number,string>` → `BTreeMap<u64, String>`（需按 id 排序语义，见 `handleBackspace` 的升序重编号）。 |
| `WeakRef` | Markdown token 缓存 | `Weak<...>` 或**直接不用缓存** | 语义上只是“内存压力下可回收”，不影响输出。Rust 侧建议保持一个 `Option<CachedTokens>` 强引用（按 source 字符串比较失效），或 `Weak`。 |
| `setTimeout` / `setInterval` / `unref` | loaders 动画、autocomplete 防抖 20ms、ScrollView 滚动条隐藏 1000ms、Flash 1000ms | `tokio::time` / `glommio` / 自建定时器线程 | TUI 是单线程事件循环 + 渲染循环；Rust 侧建议：① 组件内**不持有**异步运行时，暴露 `fn tick(&mut self, now: Instant) -> bool`（是否需要重绘）；② 或统一走 `tokio::time::sleep` 后台任务 + `mpsc` 通知重绘（对应 `unref` = drop 时 abort）。**严禁**让定时器阻塞进程退出（对应 Node 的 `unref`）。 |
| `AbortController` / `AbortSignal` | autocomplete 请求取消 | `tokio_util::sync::CancellationToken` | 语义：`abort()` 后 `signal.aborted === true`；`isAutocompleteRequestCurrent` 检查 `aborted`。Rust 用 `CancellationToken::is_cancelled()`。 |
| `Promise` 串行链（`autocompleteRequestTask`） | 保证同一时刻只有一个请求在跑 | `tokio::task` + `Mutex`/`mpsc`，或 `FutureExt::chain` | **顺序语义重要**：`previousTask` 完成后才启动下一个；`startToken` 递增使旧任务提前返回。Rust 侧用 `JoinHandle` 链 + `AtomicU64` token，或用 `futures::lock::Mutex` 保护 provider 调用。 |
| `Math.trunc` / `Math.floor` / `Math.round` | 尺寸/滚动计算 | `as i64`（注意负数截断）+ `f64::floor` | JS 的 `Math.round(-0.5) === -0`；ScrollView 中 `round` 只用于非负数，安全。 |

### 6.3 行为差异风险（必须在 Rust 侧显式桥接）

1. **UTF-16 索引**（高风险，见上）。凡出现 `.length`、`.slice`、`.indexOf`、`.lastIndexOf`、`seg.index`、`chunk.startIndex/endIndex` 的地方全部受影响。Rust 侧建议在整个编辑器模块内部使用 `Utf16Index` 适配层，并在 `TextChunk` 上保留 `start_index/end_index: usize`（UTF-16）。
2. **JS 字符串索引 = code unit**，`[...str]` = code point，`Intl.Segmenter` = grapheme。三者混用（如 `[...data].some(...)` 检查控制字符、`data.charCodeAt(0)`、`[...segmenter.segment(...)]`）必须分别对应到 Rust 的 `chars()`、`char_indices()`、`graphemes()`，不能统一。
3. **数字格式化**：`${widthPx}x${heightPx}` 是整数；`{}` 内无小数。Rust `format!` 一致。`ESC [ ${n} A` 等也是整数。
4. **JSON/顺序**：本单元不产出 JSON。但 `pastes: Map` 的插入顺序在 `getExpandedText` 的迭代中**无影响**（用正则逐个 replace），而在 `handleBackspace` 的重编号中**要求升序**（`sort` 后逐个 `set/delete`）。`BTreeMap` 天然升序。
5. **正则 `.` 与换行**：`wordWrapLine` 中 `line.split("")` 的行为在 Rust 无对应，需用 `graphemes()`；`tokenizeInlineLatex` 的 `[^\\]` 类需注意 CRLF。
6. **`Array.prototype.sort` 的稳定性**：本单元无依赖（`SelectList` 无排序；`SettingsList` 用 `fuzzyFilter`，tui-core 负责其稳定顺序）。
7. **浮点**：`floor((availableWidth - labelWidth) / 2)` 等全为整数除法，安全。ScrollView 的 `thumbHeight` 用 `Math.round(trackHeight*trackHeight/contentHeight)` —— Rust 用 `f64` + `round()` 后 `as usize`，注意 `round` 的 `.5` 向 +∞（Rust `f64::round` 同样是 half away from zero，对正数即 +∞，一致）。
8. **`String.prototype.trim()`**：JS trim 去掉的空白集合 = WhiteSpace + LineTerminator（含 U+FEFF 之外的全部 Zs、`\t\n\v\f\r`、U+00A0、U+1680、U+2000-200A、U+2028/2029、U+202F、U+205F、U+3000）。Rust `str::trim` 用 `char::is_whitespace`（Unicode White_Space 属性，**不含 U+200B ZWSP，也不含 U+FEFF**），集合基本一致但**不含 U+180E**（JS 2016 后也移除）。可接受；如需逐字一致可自写。
9. **`Intl.Segmenter` 的 `segment.length`**：grapheme 的 UTF-16 长度；Rust `grapheme.len_utf16()`。
10. **`String.prototype.normalize`**：本单元未使用。

## 7. 测试清单（本单元验收基准）

仓库使用 `node --test`（每文件 `test/*.test.ts`），断言经 `VirtualTerminal`（`@xterm/headless`）读屏幕字符与 SGR。Rust 侧应把每条用例转成 `#[test]`，并**逐字**比对渲染行的 ANSI 字符串（而非仅屏幕文本），因为本单元大量行为体现在转义序列的顺序与重置上。

### 7.1 直接覆盖本单元的测试文件

| 文件 | `it` 数 | 覆盖对象 | 关键断言 |
|---|---|---|---|
| `test/editor.test.ts` | 192 | `Editor`、`wordWrapLine` | 见 2.15.12（历史 15、访问器 2、反斜杠 5、Kitty 3、Unicode 12、滚动指示 2、grapheme 换行 8、词换行 22、kill ring 20、undo 24、autocomplete 24、字符跳转 14、粘性列 19、paste marker 22；合计 192） |
| `test/markdown.test.ts` | 92 | `Markdown` | 见 2.16.3 末尾（transforms 1、列表 16、表格 15、组合 1、LaTeX 12、#11159 8、反斜杠转义 2、预样式 3、块后间距 3、分隔线后 2、标题后 2、引用后 2、深层引用 1、多行引用 6、标题内联代码 4、删除线 2、CJK 强调 2、链接 7、HTML 2、流式围栏 1；合计 92） |
| `test/input.test.ts` | 36 | `Input` | 提交反斜杠、自定义 prompt/占位符、CJK 不溢出、横向滚动；kill ring 19；undo 12 |
| `test/select-list.test.ts` | 5 | `SelectList` | 多行描述归一化、截断时描述对齐、min/max 主列宽、自定义 truncatePrimary 保持对齐 |
| `test/settings-list.test.ts` | 2 | `SettingsList` | 搜索激活时 Space 进入查询而非修改；未查询时 Space 循环值 |
| `test/mouse-components.test.ts` | 9 | `Input`/`Editor`/`SelectList`/`SettingsList`/`MouseRegion` | 单行输入按下定位光标；列表行点击选择并激活；设置行点击激活；委托 overlay 内嵌 input 点击后保持 overlay 焦点；子菜单关闭后设置列表保持焦点；alt-screen 下编辑器定位并聚焦；拖拽选择编辑器文本而非移动光标 |
| `test/truncated-text.test.ts` | 9 | `TruncatedText` | 精确补宽、垂直 padding 行、长文本截断、保留 ANSI、带样式截断在省略号前加 reset、恰好放下、空文本、遇 `\n` 停止、含多行只取首行 |
| `test/layout.test.ts` | 15 | `VStack`/`HStack`/`ScrollView` + tui-core 布局 | 见 2.1/2.3/2.17 末 |
| `test/tab-width.test.ts` | 4 | `visibleWidth`/`sliceByColumn`（tab 语义） | tab 记 3 列；ANSI 内的 tab 字节不变 |
| `test/visible-width.test.ts` | 4 | `visibleWidth` | 样式 ASCII、tab=3、非 ASCII、未终止转义序列记 0 |
| `test/wrap-ansi.test.ts` | 19 | `wrapTextWithAnsi`（Markdown/Text 依赖） | 见 4.4/6.2：LF/CRLF/CR、CJK、OSC 133、OSC 8 BEL 保留、行内超链接不泄漏、背景跨行 |
| `test/editor-history-keybindings.test.ts` | 1 | `Editor` 历史键可配置 | 见 2.15.12 末 |
| `test/terminal-image.test.ts` | 80 | `Image` + `terminal-image`（单元 tui-terminal 主责） | 其中与 `Image` 直接相关 **9** 条（凡 `new Image(...)` 的用例）：方形像素框高度上限、序列在首行 + 空填充行、兜底截断、Kitty/iTerm2 预留行与裁剪元数据一致性、无 filename 时的兜底、transcoder 注册前后的兜底切换、每个图只转码一次、PNG/iTerm2 不走转码 |
| `test/tui-alt-screen.test.ts` | 68 | `ScrollView`/`Flash` 的集成行为（单元 tui-alt-screen 主责） | 滚动条显隐/拖拽、follow-end、图片裁剪、选择复制、flash 叠加与过期 |
| `test/tui-render.test.ts` | 32 | 宽度违约、差分渲染（单元 tui-core 主责） | 崩溃转储路径、假光标反显、行尾样式重置 |

### 7.2 间接依赖（本单元的底层契约，由其它单元测试）

`keys.test.ts`（61）、`keybindings.test.ts`（7）、`fuzzy.test.ts`（17）、`word-navigation.test.ts`（19）、`truncate-to-width.test.ts`（16）、`stdin-buffer.test.ts`（66）、`overlay-*.test.ts`（44+26+1）、`autocomplete.test.ts`（41）、`autocomplete-skill-slash.test.ts`（7）、`latex.test.ts`（20）、`terminal.test.ts`（27）、`colors.test.ts`（4）、`regression-regional-indicator-width.test.ts`（5）、`regression-slice-by-column-ansi-order.test.ts`（2）、`regression-overlay-cjk-boundary.test.ts`（4）、`regression-sigwinch-kill-eacces.test.ts`（3）、`bug-regression-isimageline-startswith-bug.test.ts`（11）、`wheel-scroll.test.ts`（6）、`tui-cell-size-input.test.ts`（2）、`tui-overlay-style-leak.test.ts`（2）、`tui-shrink.test.ts`（1）、`exit-restore.test.ts`（2）、`program-status.test.ts`（5）、`terminal-colors.test.ts`（6）、`native-*.test.ts`（16 = 10 + 2 + 4）。

其中三个文件此前遗漏：`autocomplete-skill-slash.test.ts`（`CombinedAutocompleteProvider` 的 `/` 前缀过滤与 `skill:` 命令排序，属 tui-autocomplete，但与本单元 Editor 的补全结果相关）、`regression-overlay-cjk-boundary.test.ts`（`extractSegments`/`compositeTuiLine` 在宽字符边界处的切片，HStack/overlay 合成契约）、`regression-sigwinch-kill-eacces.test.ts`（`refreshTerminalDimensions` 在 `kill(2)` 返回 EACCES 时不抛，属 tui-core 终端层）。

**计数口径提醒**：上表与 §7.1 的数量是**源码中 `it(`/`test(` 的声明数**（注释行不计；这是与规格其余部分一致的口径）。存在循环生成用例的情况：`mouse-components.test.ts` 声明 9 条，其中两条位于 `for (const row of [0, 4])` 内，**运行时注册 11 条**（`ignores hover and clicks visible select-list row {0,4} …` 与 `… settings row {0,4} …`）；`stdin-buffer.test.ts` 声明 66 条，其中 `for (const ambiguous of …)`（11 项）与 `for (const {header, reply} of replies)`（7 项）各生成一组，**运行时约 66 - 2 + 11 + 7 = 82 条**；`native-clipboard-linux.test.ts` 声明 10 条、**运行时 16 条**（4 个 `for…of` 循环分别展开 3/2/2/2 项，其中最后一个循环内含 2 个 `it`）。Rust 侧转写时应按**运行时**条数建测试（`mouse-components` 11 条），对比 `node --test` 用例名时按模板展开。

### 7.3 测试夹具（必须在 Rust 侧重建）

- `test/test-themes.ts`：导出 **`defaultSelectListTheme`、`defaultMarkdownTheme`、`defaultEditorTheme`**（没有 `createSelectListTheme()`/`createMarkdownTheme()` 之类的工厂函数），构造方式为 `const chalk = new Chalk({ level: 3 })`（chalk 6 的 `Chalk` 类构造函数，**不是** `chalk.level = 3` 赋值）；三者用 truecolor 的 `chalk.blue/bold/dim/cyan/yellow/green/italic/strikethrough/underline` 组合。`defaultEditorTheme = { borderColor: chalk.dim, selectList: defaultSelectListTheme }`。具体颜色值需逐字读取该文件并把生成的转义序列作为基准（Rust 测试建议**硬编码**这些转义串，而不是重新用 crate 生成）。测试对默认主题的**已证实**断言只有少量几条：`markdown.test.ts` 断言 h1/h2 内联恢复时出现 `\x1b[1m`（bold）与 `\x1b[36m`（cyan），并断言某些行**不含** `\x1b[36m`；`editor.test.ts` 的滚动指示器用例把 `borderColor` 换成自定义 `\x1b[35m…\x1b[39m` 后逐字比对整行（`borderColor(stripVTControlCharacters(line))`），并不依赖 chalk 的嵌套顺序。因此 Rust 侧应**用测试内联的自定义 styler** 复刻这些断言，而不是依赖硬编码 chalk 输出；`defaultEditorTheme.borderColor`（chalk.dim）只影响“不覆盖主题”的用例，而那些用例只做去色/宽度断言。
- `test/virtual-terminal.ts`：`export class VirtualTerminal implements Terminal`（基于 `@xterm/headless` 的 `Terminal`）；构造 `new VirtualTerminal(columns = 80, rows = 24)`。**真实公开 API**：
  - `Terminal` 契约：`start(onInput, onResize)`、`stop()`、`async drainInput(maxMs?, idleMs?)`、`write(data)`、`columns`/`rows`（getter）、`kittyProtocolActive`（getter，恒 false）、`moveBy(lines)`、`hideCursor()`、`showCursor()`、`clearLine()`、`clearFromCursor()`、`clearScreen()`、`setTitle(title)`、`setProgress(active)`（no-op）、`setProgramStatus()`（no-op）。
  - 测试专用：`sendInput(data)`（触发 `onInput`）、`resize(columns, rows)`（同时改 xterm 并触发 `onResize`）、`async flush()`（写空串等 xterm 排空）、`async flushAndGetViewport(): Promise<string[]>`、`getViewport(): string[]`（当前视口，`translateToString(true)`）、`getScrollBuffer(): string[]`（含 scrollback 的全部行）、`clear()`（清视口）、`reset()`（`xterm.reset()`）、`getCursorPosition(): {x, y}`、`async waitForRender()`（`process.nextTick` + 20ms `setTimeout` + `flush`，等节流渲染管线落定）。
  - **没有** `getLine(y)`/`getCell(x,y)`/`getCursor()`/`getScrollback()` 这些方法。需要逐格样式断言时，测试通过强制类型转换拿私有字段：`const xterm = (terminal as unknown as { xterm: XtermTerminalType }).xterm; xterm.buffer.active.getLine(buffer.viewportY + row).getCell(col)`，再用 `isFgDefault()`/`isBold()`/`isDim()`/`getFgColor()` 等 `@xterm/headless` 的 Cell API（见 `markdown.test.ts:15-20`、`tui-render.test.ts:110`）。
  - Rust 侧建议实现一个最小 ANSI 屏幕模型（支持 CSI m/K/H/J/A/B/C/D、OSC 8、APC 丢弃、宽字符占两格）以便逐字回放断言，并额外提供 `cell(row, col) -> Option<Cell { ch, fg, bg, bold, dim, italic, underline, strikethrough }>` 等价 API（因为 `markdown.test.ts` 的 7 个链接用例与引用样式用例依赖逐格颜色/粗体/暗淡断言）。
- `flushAutocomplete()`（`editor.test.ts:40`，模块内 helper，**不等待真实时间**）：`await Promise.resolve(); await new Promise(resolve => setImmediate(resolve));`——只排空微任务 + 一轮 macrotask，供**无防抖**路径（`explicitTab`/`force` 或未命中 debounce 正则）的 Promise 链落定。**20 ms 防抖路径不用它**：那些用例走 `t.mock.timers.enable({ apis: ["setTimeout"] })` + `t.mock.timers.tick(19)` / `tick(1)` 的边界断言（`editor.test.ts:2136-2340`）。Rust 侧对应两种：① 无防抖路径只要把内部 promise 链跑完（`poll`/`yield_now` 若干次）；② 防抖路径用可注入的 `Clock`/`tokio::time::advance` 精确推进 19/20 ms。

### 7.4 断言风格注意

- 多数断言先用 `stripAnsi`/`visibleWidth` 去色，但也有大量断言**包含**转义序列（如 `\x1b[35m...\x1b[39m` 边框色、`\x1b[0m` 前置于省略号、`\x1b[22;23;...m` 表格续行）。Rust 侧必须两种模式都支持。
- 行宽断言：`for (const line of lines) assert.equal(visibleWidth(line), width)`。
- 崩溃断言：`assert.throws(() => ..., /exceeds terminal width/)`。


## 8. 建议的 Rust 模块布局与最难复刻之处

### 8.1 crate 与模块

沿用 tui-core 规范的建议：单一 `kiana_tui` crate（或 `kiana-tui` workspace），本单元作为其中的 `components` 模块树。

```
kiana-tui/
  src/
    lib.rs                 // 与 packages/tui/src/index.ts 一一对应的 pub use
    components/
      mod.rs               // pub use 全部组件
      stack.rs             // StackChild, StackEntry, StackEntryOptions, StackOptions, Stack, visible_stack_entries, allocate_stack_sizes
      v_stack.rs           // VStack
      h_stack.rs           // HStack
      spacer.rs            // Spacer
      text.rs              // Text
      truncated_text.rs    // TruncatedText
      box_component.rs     // Box（避免与关键字冲突）
      mouse_region.rs      // MouseRegion, MouseRegionHandler
      alt_screen_flash.rs  // AltScreenFlashContainer
      loader.rs            // Loader, LoaderIndicatorOptions
      cancellable_loader.rs// CancellableLoader
      select_list.rs       // SelectList, SelectItem, SelectListTheme, SelectListLayoutOptions, SelectListTruncatePrimaryContext
      settings_list.rs     // SettingsList, SettingItem, SettingsListTheme, SettingsListOptions
      input.rs             // Input, InputOptions
      editor/
        mod.rs             // Editor, EditorTheme, EditorOptions
        word_wrap.rs       // TextChunk, word_wrap_line
        paste.rs           // PasteRegistry, PASTE_MARKER 解析/重编号/展开
        autocomplete.rs    // EditorAutocomplete 状态机（provider 串行化、防抖、正则）
        visual_lines.rs    // build_visual_line_map, find_visual_line_at, move_to_visual_line, compute_vertical_move_column
        kill_yank.rs       // 可留在 mod.rs，或抽为辅助
        scroll_border.rs   // create_scroll_border
      markdown/
        mod.rs             // Markdown, MarkdownTheme, MarkdownOptions, DefaultTextStyle
        lexer.rs           // marked 等价 lexer + latex 扩展 + 严格删除线 + CJK 强调
        inline.rs          // render_inline_tokens + 样式栈
        block.rs           // render_token 的块级分支
        list.rs            // render_list
        table.rs           // render_table, wrap_cell_text, get_longest_word_width
        latex_ext.rs       // tokenize_inline_latex / tokenize_block_latex / contains_table_start
      scroll_view.rs       // ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions
      image.rs             // Image, ImageTheme, ImageOptions, ImageTranscoder, set_image_transcoder
```

### 8.2 关键类型/特征设计

```rust
// 组件统一用 trait object；渲染无副作用的部分用 &self，Editor 用 &mut self。
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;   // 注意 &mut：Editor/Text/Box 有缓存或渲染期状态
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _e: &TuiMouseEvent) -> Option<MouseResult> { None }
    fn wants_key_release(&self) -> bool { false }
    fn invalidate(&mut self);
}
```

- **主题函数**：JS 的 `(text: string) => string` → `Arc<dyn Fn(&str) -> String + Send + Sync>`（可克隆、可共享）。需要 `Debug` 时用 `#[derive(Clone)] struct Styler(Arc<dyn Fn(&str) -> String + Send + Sync>)` 并手写 `Debug`。
- **`StackChild`**：JS 用 `"render" in child` 鸭子类型区分 `Component | StackEntry`；Rust 用显式枚举：
  ```rust
  pub enum StackChild { Component(Box<dyn Component>), Entry(StackEntry) }
  ```
  或统一包装为 `StackEntry`（`StackChild::from(component)` 生成 `basis: auto, grow: 0, shrink: 1` 的默认 entry —— 必须与 JS 的归一化规则一致，见 §2.1）。
- **`TextChunk`**：
  ```rust
  pub struct TextChunk { pub text: String, pub start_index: usize, pub end_index: usize }  // UTF-16 偏移
  ```
- **`EditorTheme`/`SelectListTheme`/`SettingsListTheme`/`MarkdownTheme`**：全是函数字段的 struct（`Clone`），不用 trait，以便逐字段构造与测试。
- **`ImageTranscoder`**：`Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>`；模块级 `OnceLock<Mutex<Option<...>>>` + PNG LRU 缓存（`HashMap<String, Option<String>>` + 插入顺序 `VecDeque`，上限 32）。
- **自动补全**：
  ```rust
  pub trait AutocompleteProvider: Send {
      fn trigger_characters(&self) -> Option<Vec<char>> { None }
      fn get_suggestions<'a>(&'a mut self, lines: &'a [String], line: usize, col: usize,
                             opts: &'a GetSuggestionsOptions)
          -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>>;
      fn apply_completion(&self, lines: &[String], line: usize, col: usize,
                          item: &AutocompleteItem, prefix: &str) -> ApplyCompletionResult;
      fn should_trigger_file_completion(&self, _l: &[String], _li: usize, _c: usize) -> bool { true }
  }
  ```
  编辑器内保存 `Option<Box<dyn AutocompleteProvider>>`；请求串行化用 `Arc<Mutex<RequestState>>` + `AtomicU64` token。
- **定时器抽象**：为可测试性引入 `Clock` trait（`now()`/`sleep_until()`），或把防抖实现为 `deadline: Option<Instant>`，由外部渲染循环调用 `editor.poll_debounce(now)`。建议后者（无运行时依赖，测试可直接推进时间）。

### 8.3 最难复刻之处（按风险从高到低）

1. **Markdown 的 token 流与样式穿插顺序**。`marked` 18 的 token 边界（尤其列表 loose/tight、嵌套 blockquote 的 `space` token、gfm 表格）与任何 Rust Markdown 库都不同。输出是**逐字符 ANSI 拼接**（`strong → theme.bold(inner) + stylePrefix`、块级样式在后处理阶段 apply），任何 token 差异都会改变换行位置与重置序列。若要求逐字一致，只能**移植 marked 的 lexer**（约 2000 行）或自写。同时 `marked-cjk-friendly` 的 emStrong 规则、严格删除线 tokenizer、LaTeX tokenizer 扩展的 pending 状态（流式）都必须一起移植。表格/列表的排版算法本身可移植，但依赖 token 的 `raw`。
2. **UTF-16 偏移与 grapheme 索引的双轨制**。`Editor` 内部所有偏移是 UTF-16；`wordWrapLine` 用 grapheme 迭代但记录 UTF-16 索引；`segmentWithMarkers` 用 `Intl.Segmenter` 的 `segment.index`（UTF-16）再按 marker 区间合并。Rust 若用 `String` + `char_indices` 会在 emoji/代理对上差 2，导致光标位置、粘贴标记切片、`TextChunk` 全部错位。**必须**做显式的 `Utf16Index` 层并为其写单元测试（对比 JS 的 `[...str]`/`slice` 行为）。
3. **`Intl.Segmenter` 的 grapheme/word 精确一致性**。ICU 与 UAX #29 实现在 emoji ZWJ、区域指示符、Indic conjunct、Thai/Lao AM 上有版本相关差异；仓库还有专门的 AM 宽度处理与「区域指示符宽 2」逻辑。`wordWrapLine` 的换行点、`graphemeWidth` 的宽度、鼠标吸附全部依赖它。**建议**：引入一套 golden 测试（对同一批字符串比较 JS 与 Rust 的分段结果与宽度）。
4. **`visibleWidth` 的 512 项缓存与 `asciiVisibleWidth` 快速路径**。缓存本身不影响输出，但 `asciiVisibleWidth` 的“可打印 ASCII + tab + ANSI”快速路径**必须与慢路径给出相同结果**（tab 记 3 列，ANSI 记 0）。若 Rust 侧省略快速路径，性能会下降但不影响正确性；若省略缓存，`Editor.render` 在长文本上会退化到 O(n²)。
5. **`marked` 之外的 `Word wrapping` 边界语义**。`wordWrapLine` 的“回退到换行机会”逻辑（`wrapOppIndex/wrapOppWidth`）与“oversized atomic segment 递归切分”非常微妙：`Word wrapping` describe 有 22 个用例，其中 13 个共含 27 条 `assert.strictEqual(chunks[…], …)`（24 条为 `chunks[i].text` 字面量、1 条 `chunks[0].text === "A"`、1 条末段 `"world"`、1 条末段 `.includes("B")`）；其余 9 个用例断言“每段 `visibleWidth <= maxWidth` 且 `chunks.map(c => line.slice(start,end)).join("") === line`”。实现顺序或 `<=` / `<` 写反都会失败。**必须逐行对照源码移植**，不要重写。
6. **粘贴标记（paste marker）的原子性与重编号**。`segmentWithMarkers` 把 marker 合成单个 grapheme 段；`handleBackspace` 删除后要**升序**重编号 registry 与文本中的 marker；`wordWrapLine` 对超宽 marker 递归切分但保持编辑原子性；`moveToVisualLine` 要跳过 marker 的续段。这是编辑器里最容易出“卡住/跳错”的地方（`editor.test.ts` 的 `Paste marker atomic behavior` describe 共 22 条，另有 `Word wrapping` 中的 5 条预分段用例）。
7. **粘性列决策表**。`computeVerticalMoveColumn` 的 P/S/T/U 四条件表与 `moveToVisualLine` 的 `snappedFromCursorCol` 交互（重排、resize、marker 吸附）需要逐分支移植；19 条测试覆盖但都不易从行为反推算法。
8. **自动补全的异步串行化 + 快照校验 + 防抖正则**。`startToken`/`requestId`/`aborted`/文本快照/cursor 快照五重校验；trigger 与 debounce 两个正则的差异（`@` 与其它触发字符、CJK 标点、引号包裹路径）依赖 `\p{...}`/lookahead，正则方言差异会直接改变触发时机（24 条测试）。
9. **ANSI 样式的“尾部前缀剥离”与“reset 后重新施加”**。`renderInlineTokens` 末尾的 `while result.endsWith(stylePrefix) { truncate }`、blockquote 的 `replace(/\x1b\[0m/g, "\x1b[0m" + quoteStylePrefix)`、表格续行的 `\x1b[22;23;...;39m` + 外层 stylePrefix。任何差异都会导致“下划线漏到 padding”这类测试失败。
10. **`ScrollView` 与 layout 的 follow-end/overscroll/滚动条几何联动**。逻辑在 tui-core 的 `layout.ts`（`renderLayoutFrame`/`paintBox`/scrollbar），本单元只提供状态对象与 `[LAYOUT_NODE]`。Rust 侧需保证 `ScrollView` 的 getter 语义（`scrollBy` 返回未消费 delta、`disableFollow`、`followSuppressedAtEnd`）与 layout 的调用序列一致。
11. **`Loader` 的定时动画与 `Super.render(width+2)[1]` 依赖**。`StatusIndicator`（coding-agent）依赖 `Loader` 继承 `Text` 且 `render` 前置空行这一实现细节；Rust 侧若把 `Loader` 做成组合而非继承，需要提供等价的 `first_line_is_blank` 契约或让 `StatusIndicator` 相应改写（会破坏下游逐字输出）。
12. **`Image` 的 Kitty/iTerm2 光标移动约定**。Kitty 用 `C=1` + `rows-1` 个空行；iTerm2 用 `rows-1` 个空行 + `\x1b[${rows-1}A` 前缀。行列换算依赖 `getCellDimensions()`（默认 9×18）与 `ceil` 取整方向；测试对裁剪、预留行、宽度上限有逐字断言（80 条 terminal-image 测试中的一部分）。
13. **`Text`/`Box` 的缓存键**。`Box` 以子行字符串身份 + `bgFn("test")` 采样作为缓存键（§2.7），Rust 侧若用 `Vec<String>` 相等比较会正确但慢；若省略缓存，测试仍通过但差分渲染性能下降。`Text` 的空白短路返回 `[]`（**不是 `[""]`**）影响上游布局高度。
14. **`Input`/`Editor` 的鼠标列换算硬编码 `- 2`**。`Input.handleMouse` 用 `event.x - 2` 假设 prompt 占 2 列（`"> "`），这与可配置 prompt 不一致——**这是既有 bug，必须原样保留**（测试不覆盖自定义 prompt 的鼠标，但逐字行为要求保留）。同理 `Editor` 的 `renderedVisibleLineCount + 2`。
15. **`Spacer` 接受非整数 `lines`**。JS 的 `for (let i = 0; i < lines; i++)` 对 `lines = 2.5` 产生 3 行；Rust 用 `usize` 会得到 2。必须决定：把参数做成 `f64` 并 `ceil`，或在构造时 `lines.ceil() as usize`（**记录为已知 delta**）。
16. **`truncateToWidth` 的省略号前 `\x1b[0m`**。Markdown/TruncatedText/SelectList 都依赖这一细节；Rust 侧必须与 tui-core 的 `truncate_to_width` 完全一致（含 `ellipsis = ""` 时不加 reset 的分支）。
17. **`SelectList` 的 `width > 40` 阈值与 `remainingWidth > 10`**。分两列/单列的切换点是硬编码数字，必须在 Rust 中保持，否则描述列出现/消失。
18. **`SettingsList` 搜索激活时的 Space 语义**。测试 2 条专门覆盖：搜索激活时 Space 进入查询；未激活时 Space 循环值。`isSearchActive` 依赖 `searchInput.getValue().length > 0`（见 §2.13），不是“焦点在搜索框”。
19. **`Markdown` 的 `renderLatex` 与 pending 状态**。流式场景下未闭合 `$$`/`\[` 返回 pending token 并输出原文，闭合后切换为渲染；`containsTableStart` 防止吞表格。这依赖 lexer 的 `raw` 与 `token.text` 的精确切分。
20. **`tabs = 3 列` 与 ANSI 内的 tab 字节保留**。`visibleWidth` 先把 `\t` 替换为 3 空格再计算；`sliceByColumn` 必须保持 ANSI 内的 tab 字节不变（测试 `keeps tabs inside terminal control sequences byte-identical`）。Rust 侧若用 `replace('\t', "   ")` 全局替换会破坏 ANSI 内容。

### 8.4 迁移顺序建议

1. 先做 **纯叶片组件**（无外部状态）：`Spacer`、`Text`、`TruncatedText`、`Box`、`MouseRegion`、`AltScreenFlashContainer`、`Loader`、`CancellableLoader`。它们只依赖 `utils` 的 4 个函数与 `Component` 契约，可快速建立测试骨架。
2. 再做 **布局**：`Stack`/`VStack`/`HStack`（依赖 `layout-node` 类型与 `compositeTuiLine`）。此时应与 tui-core 的 `layout.ts` 同步联调（15 条 layout 测试）。
3. 再做 `SelectList` → `SettingsList`（后者依赖前者主题类型与 `Input`）。
4. 再做 `Input`（依赖 kill-ring/undo-stack/word-navigation/grapheme）。
5. 再做 `ScrollView` + `Image`（依赖 tui-terminal 的 capabilities/renderImage）。
6. 最后做 `Markdown`（依赖 marked 移植）与 `Editor`（依赖 `SelectList`、`wordWrapLine`、autocomplete 类型、全部编辑器基础设施）。这两个是工作量与风险的最大头（合计约 3500 行 TS，占本单元的 60%）。

### 8.5 已知的“实现 bug 也需保留”的清单

- `Input.handleMouse` 的 `x - 2` 硬编码（见上）。
- `Loader.render` 前置空行（下游依赖）。
- `Editor.setAutocompleteTriggerCharacters` 从 `["@", "#"]` 起算（即使用户 provider 只声明 `["@"]`，`#` 仍会触发）。
- `SelectList` 无匹配时只返回一行、且不截断到 `width`（可能超宽，但由于文本短通常不触发；严格来说违反宽度契约）。
- `SettingsList` 空列表时 `theme.hint("  No settings available")` 不截断。
- `Markdown` 的 `hr` 宽度 `min(width, 80)`（宽终端下不铺满）。
- `Editor` 的 `renderTopBorder` 在 `hiddenLineCount === 0` 时不走 `createScrollBorder`，直接 `"─".repeat(width)`（无左右留白，与有指示器时不同）。
- `ScrollView.render` 在有滚动条列时**只追加一个空格**而非补齐到 `width`（依赖子组件自己保证行长 = width-1）。
- `wordWrapLine` 对 `maxWidth <= 0` 返回单个空 chunk `{text:"", startIndex:0, endIndex:0}`（而非空数组）。
- `Text` 与 `Markdown` 的空白文本都返回 `[]`（无行）；`Markdown` 在 `paddingY === 0` 且所有 token 都不产生行时才会走到 `result.length > 0 ? result : [""]` 的兜底（正常非空输入至少有一行）。

---

## Audit additions（对抗式完整性审计补遗）

本节记录对 `packages/tui/src/components/**`（18 文件、5865 行）+ `packages/tui/test/**` 逐行复核后发现的**新增/修正项**。正文已在原位修正的项目在此也列出，便于追溯；未改动正文的纯补充集中在此。

### A.1 已修正的**错误陈述**（正文已改）

1. **§7.2 测试计数**：`stdin-buffer.test.ts` 76→**66**、`autocomplete.test.ts` 42→**41**、`exit-restore.test.ts` 3→**2**、`terminal-colors.test.ts` 7→**6**；并补入 3 个此前完全遗漏的测试文件：`autocomplete-skill-slash.test.ts`（7）、`regression-overlay-cjk-boundary.test.ts`（4）、`regression-sigwinch-kill-eacces.test.ts`（3）。
2. **§7.1 `input.test.ts` 行**：`kill ring 16`→**19**、`undo 11`→**12**（合计仍 36）。
3. **§7.1 `terminal-image.test.ts` 行**：与 `Image` 直接相关 8 条→**9** 条（判据：用例体内出现 `new Image(`）。
4. **§2.16.3 / §7.1 `markdown.test.ts` 行**：LaTeX（11→**12**）、#11159（9→**8**）。原因：`it("switches from raw to rendered math when a streamed delimiter closes")`（`markdown.test.ts:1038`）缩进为 2、位于 `unclosed display math…` 的 `describe`（`970-1045`）**之后**，仍属外层 `LaTeX math` describe。
5. **§7.3 `test-themes.ts`**：实际导出是 `defaultSelectListTheme` / `defaultMarkdownTheme` / `defaultEditorTheme` 三个常量，**不存在** `createSelectListTheme()`/`createMarkdownTheme()`；主题用 `new Chalk({ level: 3 })` 构造，**不是** `chalk.level = 3` 赋值。
6. **§7.3 `virtual-terminal.ts`**：`VirtualTerminal` **没有** `getLine(y)`/`getCell(x,y)`/`getCursor()`/`getScrollback()`。真实 API 见正文（`getViewport`/`getScrollBuffer`/`getCursorPosition`/`sendInput`/`resize`/`flush`/`flushAndGetViewport`/`waitForRender`/`clear`/`reset`）；逐格断言经 `(terminal as unknown as {xterm}).xterm.buffer.active` 私有访问。
7. **§5.2 / §1.2 下游契约**：曾称 `custom-editor.ts` 访问 `lastWidth` 与 `getPaddingX()`。事实：`lastWidth` 是 `private` 且**全仓无下游读取**；`getPaddingX()` 的使用点在 `interactive-mode.ts:2861`；`renderBottomBorder` **没有任何下游覆写**（只有 `renderTopBorder` 被 `custom-editor.ts` 覆写）。
8. **§5.1 TUI 接口注记**：曾写“本单元只用 terminal().rows、requestRender()、setFocus 三类”；实际组件**不调用 `setFocus`**（focus 由 tui-core 反向写入 `Focusable`）。
9. **§7.3 `flushAutocomplete()`**：它**不等待 20 ms**，只 `await Promise.resolve()` + `setImmediate`；20 ms 防抖路径由 `t.mock.timers` 精确推进。

### A.2 新增的**行为/源码细节**（正文已插入）

1. **Editor `moveWordBackwards()` / `moveWordForwards()`**（§2.15.10）——此前规格完全没有这两个方法的语义。要点：**跨行**（行首→上一行行尾 / 行尾→下一行行首），且带 `{ segment: t => this.segment(t, "word"), isAtomicSegment: isPasteMarker }`；与 `Input` 的同名方法（不跨行、无 atomic）**不同**。
2. **Editor 的 marker 感知分段**：`private segment(text, "word" | "grapheme")` = `segmentWithMarkers(text, …, validPasteIds())`，被 **10 处**调用（含 `handleMouse`、`render`、`layoutText`、`handleBackspace`、`handleForwardDelete`、`moveCursor`、`moveToVisualLine`、两个词移动）。`handleMouse` 的命中列换算因此对 paste marker 整段吸附。
3. **`validPasteIds()` / `isEditorEmpty()`**：两个私有辅助此前未列。`isEditorEmpty = lines.length === 1 && lines[0] === ""` 只用于 `cursorUp` 的历史分支。
4. **`insertCharacter(char, skipUndoCoalescing?)`**：第二个参数**无调用方传 true**（`handlePaste` 走 `insertTextAtCursorInternal`），Rust 侧保留签名即可。
5. **Input placeholder 分支不重置 `renderedStartColumn`**：会导致后续点击用到过期滚动偏移（既有瑕疵，须原样保留）。
6. **Image 转码失败不缓存**：`pngData ??= toPng(...) ?? undefined`，失败后下次 `render` 会重试（`renders a text fallback until a working transcoder is registered` 依赖此语义）；且 `options.imageId` 存在时跳过 `allocateImageId()`。
7. **Box `mouseLayout` 缓存**：`{width, children:[{component,height}]}`，`handleMouse` 宽度不匹配时**现场渲染所有 child** 取高度（会触发渲染副作用）。
8. **`KillRing.push` 空串短路**与 `rotate()` 的“末项移到最前”语义（§5.1 已补）。
9. **v1.2.0 假光标契约**（§3.4 已补）：Editor/Input 输出 `\x1b_kiana:fc\x07…\x1b_kiana:/fc\x07` APC，反显由 TUI 的 `resolveFakeCursors()` 施加；`AltScreenFlashContainer` 是唯一仍直接写 `\x1b[7m` 的组件。
10. **测试计数口径**：声明数 vs 运行时注册数（`mouse-components.test.ts` 9 声明 / **11 运行时**；`stdin-buffer.test.ts` 66 声明 / 约 82 运行时）。`latex.test.ts` 声明 20 条，其中 1 条是 `defineCases()` 内的 `it` 模板（被调用 5 次、共 92 个用例），**运行时实际注册 19 + 92 = 111 条**。

### A.3 仍未纳入正文、但 Rust 侧应知道的**行为差异/风险**

1. **`README.md` 与源码矛盾**（README 是“用户可见契约的权威来源”，但这两处是**错的**）：SelectList/SettingsList 的 “Mouse move/wheel: Highlight rows in alternate-screen mode” 与源码注释 `Hover must not change selection: the visible range is centered on it` 相反；`EditorOptions` 的文档漏掉 `autocompleteMaxVisible`；`ImageOptions` 漏掉 `imageId`；`SettingsList` 的 `submenu` 文档签名漏掉 `done()` 的第二参 `options?: { navigateTo?: string }`。**以源码为准**。
2. **`Editor.handleInput` 的 `tui.input.copy` 分支是“空 return”**：`ctrl+c` 在编辑器内被**吞掉**（不传给父级），与注释“let parent handle”字面相反。Rust 侧照抄现有行为即可（`CustomEditor` 在其之前处理 `app.interrupt`）。
3. **`Editor` 的 `autocompleteStartToken`/`autocompleteRequestId` 双计数器**：前者防“排队中的旧任务启动”，后者防“已发出但已过期的结果写回”；两者都必须存在，合并会破坏 `continues typing aborts in-flight @ completion` 等用例。
4. **`SettingsList` 的 `maxLabelWidth` 用 `items` 而非 `filteredItems`**：搜索过滤时列宽仍按全量计算（可能与可见项不匹配），属既有行为。
5. **`SelectList` 无匹配时返回的行没有 `truncateToWidth`**（`theme.noMatch("  No matching commands")` 原样推入），窄终端下可能超宽——与 §8.5 的同类条目一起保留。
6. **`Markdown` 的 `highlightCode` 返回行不再经过 `theme.codeBlock`**，且不按宽度截断（后续统一 wrap 会处理）；`codeBlockIndent` 默认 `"  "` 会**附加在**高亮行之前（高亮实现需自行包含缩进语义）。
7. **`ScrollView` 的 `scrollToStart`/`scrollToEnd` 即使 `changed === false` 也会写状态**（`currentScrollTop`/`followingEnd`/`followSuppressedAtEnd` 无条件赋值），只是不 markActivity/requestRender；`updateLayout` 则在 `followingEnd` 时**强制** `currentScrollTop = maxScrollTop`。
8. **`VStack.render` 用 `allocateStackSizes(..., undefined, gap)`**：传 `undefined` 即“测量模式”，grow/shrink 全部忽略（只受 basis/min/max 夹取），所以 `grow` 选项只在被 `layout.ts` 承载时生效。
9. **`HStack.render` 对每个 child 渲染两次**（先按 `safeWidth` 测内在宽，再按分配宽渲染），且 `widths[i] === 0` 时**跳过第二次渲染**；`align` 的 `"stretch"` 与 `"start"` 在此实现中**完全等价**（offset 都为 0）。
10. **`Image` 的 `getCellDimensions()` 默认 9×18**，`maxHeight` 用 `ceil(maxWidth * cellW / cellH)`；测试会 `setCellDimensions({widthPx:10,heightPx:10})` 改变结果——Rust 侧需可注入。

### A.4 覆盖性结论

- **源文件覆盖**：18/18 个组件文件全部有专节（§2.1–§2.18），行数与源码一致（合计 5865）。导出面 49 条 `export` 语句、逐文件清单与源码一致。
- **符号覆盖**：所有 `export` 的 class/interface/type/function 均在规格中出现；所有 public/protected 方法均被点名；私有成员仅 `validPasteIds`/`isEditorEmpty` 此前缺失（已在 A.2 补）。
- **测试覆盖**：直接/间接测试文件的计数与结构已与源码逐一对齐；新发现 3 个遗漏文件并已补入；4 个错误计数已修正。
- **判断**：整体为 **mostly-complete**——主体（渲染算法、粘性列、paste marker、Markdown token 流、测试基准）相当详尽且准确，缺陷集中在“测试清单计数/夹具描述”“Editor 两个词移动方法”“下游继承契约的细节”三处，均已修复或补遗。
