# `tui-core` 移植规范（TypeScript → Rust）

本文件是 `@earendil-works/kiana-tui` 中 **tui-core** 单元的完整移植规范，同时充当：

1. **验收清单**：Rust 实现必须逐条满足的行为契约；
2. **Rust 设计**：模块划分、类型、并发模型与最难复刻之处的说明。

单元 id：`tui-core`。
参考实现（只读，禁止修改）：`/home/ctyun/code/kiana_rev/kiana/packages/tui`。
目标工作区：`/home/ctyun/code/kiana_rev/kiana`。

> 约定：文中所有标识符、字段名、错误文案、用户可见字符串、ANSI 序列均按 TypeScript 源码**逐字**抄录。
> Rust 实现必须输出完全一致的字节序列（除另有说明）。中文用于解释语义。
> 凡标 **【逐字】** 的字符串，测试会逐字节比对。`\x1b` 表示 U+001B；`\x07` 表示 U+0007（BEL）；`\x1b\\` 表示 ESC + 反斜杠（ST）。

---

## 0. 单元边界与阅读顺序

### 0.1 属于本单元的源文件（全部必须移植）

```
packages/tui/src/index.ts                   (195 行)   公共导出面
packages/tui/src/tui.ts                     (1543 行)  Component / Container / TuiBase / overlay / 鼠标分发
packages/tui/src/tui-main-screen.ts         (656 行)   TuiMainScreen（主屏差分渲染）
packages/tui/src/tui-alt-screen.ts          (1798 行)  TuiAltScreen（备用屏 + 应用自管视口）
packages/tui/src/terminal.ts                (630 行)   Terminal 接口 + ProcessTerminal
packages/tui/src/stdin-buffer.ts            (480 行)   StdinBuffer（输入序列切分）
packages/tui/src/keys.ts                    (1455 行)  按键解析与匹配
packages/tui/src/keybindings.ts             (320 行)   全局键位注册表
packages/tui/src/layout.ts                  (453 行)   renderLayoutFrame / 滚动条 / 命中测试
packages/tui/src/layout-node.ts             (51 行)    LAYOUT_NODE 协议
packages/tui/src/program-status.ts          (53 行)    OSC 7501
packages/tui/src/wheel-scroll.ts            (82 行)    滚轮加速器
packages/tui/src/terminal-colors.ts         (91 行)    OSC 10/11/4 与 OSC 997 解析
packages/tui/src/terminal-image.ts          (776 行)   终端能力探测 / Kitty / iTerm2 / 尺寸解析
packages/tui/src/native-module-path.ts      (31 行)    原生模块候选路径
packages/tui/src/native-platform.ts         (65 行)    原生平台 helper 加载
packages/tui/src/native-modifiers.ts        (13 行)    修饰键查询
packages/tui/native/napi.h                  (91 行)    N-API 动态解析
packages/tui/native/clipboard.h             (208 行)  剪贴板作业队列（平台无关）
packages/tui/native/linux/src/clipboard-worker.h  (70 行)   Linux 私有工作线程
packages/tui/native/linux/src/linux-platform-x11.c (274 行) X11 XCB 剪贴板
packages/tui/native/darwin/src/darwin-platform.m   (132 行) AppKit 剪贴板 / 修饰键
packages/tui/native/win32/src/win32-platform.c     (241 行) Win32 剪贴板 / 修饰键 / VT 输入
packages/tui/native/linux/build.sh          (65 行)
packages/tui/native/darwin/build.sh         (69 行)
packages/tui/native/win32/build.mjs         (232 行)
packages/tui/native/{linux,darwin,win32}/README.md
packages/tui/README.md                      (949 行)   公共 API 与用户可见契约
```

合计源码 9 965 行（TS 8 583 + native C/ObjC/构建脚本 1 382；不含 components/、utils.ts、alt-screen-search.ts，也不含各 README）。

### 0.2 与相邻单元的边界重叠（重要，必须先读）

`packages/tui` 内的单元划分在本工作流中并非完全互斥。本单元任务文本明确列出上表文件；但
`tui-components.md`（同目录）把下列文件划归 **tui-core**，而它们**不在**本单元任务文本的清单里：

- `src/utils.ts`（1402 行）
- `src/alt-screen-search.ts`（327 行）
- `src/fuzzy.ts`、`src/kill-ring.ts`、`src/undo-stack.ts`、`src/word-navigation.ts`
- `src/editor-component.ts`（仅导出接口）

同时，本单元任务文本把 `src/terminal-image.ts` 列入本单元，而 `tui-components.md` 把它划给 **tui-terminal**。

处理策略（本规格采用）：

1. **`src/utils.ts`**：本单元**不实现**，但 `tui.ts` / `tui-main-screen.ts` / `tui-alt-screen.ts` / `layout.ts`
   的逐字节行为完全依赖它的若干函数。§2.0 给出这些函数**精确的行为契约**（签名 + 边界条件），
   Rust 侧必须先有等价的 `tui_utils` 模块。深入实现细节以拥有该单元的规格为准。
2. **`src/alt-screen-search.ts`**：`tui-alt-screen.ts` 直接依赖（搜索面板、索引、匹配高亮）。
   因其与备用屏渲染强耦合，本规格在 §2.12 完整记录；若后续由其他单元实现，只需保证该节契约。
3. **`src/terminal-image.ts`**：本规格完整记录（§2.15）。若 `tui-terminal` 单元同时实现，取并集，
   但 §2.15 中的能力探测、`isImageLine`、`getKittyImagePlacement`、`cropKittyImageLine`、
   `getMaxKittyImageRows`、`getCellDimensions`/`setCellDimensions` 是 tui-core 渲染器的硬依赖。
4. **`src/fuzzy.ts` / `kill-ring.ts` / `undo-stack.ts` / `word-navigation.ts` / `editor-component.ts`**：
   不属于本单元，`index.ts` 中继导出 `fuzzyFilter`/`fuzzyMatch`/`EditorComponent` 由对应单元提供。

### 0.3 明确**不**属于本单元

- `src/components/**`（18 个组件文件）——单元 **tui-components**。本单元只定义它们实现的
  `Component` / `Focusable` 协议，并在测试中作为消费者出现。
- `src/colors.ts`、`src/oklab.ts`、`src/latex.ts`、`src/autocomplete.ts`、`src/fuzzy.ts`、
  `src/kill-ring.ts`、`src/undo-stack.ts`、`src/word-navigation.ts`、`src/editor-component.ts`。
- `src/utils.ts` 的实现（见 §0.2）。

### 0.4 配套测试（本单元验收基准）

```
packages/tui/test/tui-render.test.ts                 32 个 it
packages/tui/test/tui-alt-screen.test.ts             68
packages/tui/test/overlay-options.test.ts            26
packages/tui/test/overlay-non-capturing.test.ts      44
packages/tui/test/overlay-short-content.test.ts       1
packages/tui/test/tui-overlay-style-leak.test.ts      2
packages/tui/test/tui-shrink.test.ts                  1
packages/tui/test/tui-cell-size-input.test.ts         2
packages/tui/test/layout.test.ts                     15
packages/tui/test/keys.test.ts                       61
packages/tui/test/keybindings.test.ts                 7
packages/tui/test/stdin-buffer.test.ts               66
packages/tui/test/terminal.test.ts                   27
packages/tui/test/terminal-colors.test.ts             6
packages/tui/test/program-status.test.ts              5
packages/tui/test/wheel-scroll.test.ts                6
packages/tui/test/terminal-image.test.ts             80（与 tui-terminal 共享）
packages/tui/test/native-module-path.test.ts          2
packages/tui/test/native-platform.test.ts             4（用 test( 而非 it(）
packages/tui/test/native-clipboard-linux.test.ts     10（含参数化模板）
packages/tui/test/mouse-components.test.ts            9（跨 tui-core / tui-components）
packages/tui/test/exit-restore.test.ts                2
packages/tui/test/regression-sigwinch-kill-eacces.test.ts 3
packages/tui/test/regression-slice-by-column-ansi-order.test.ts 2（utils 单元）
packages/tui/test/bug-regression-isimageline-startswith-bug.test.ts 11
packages/tui/test/visible-width.test.ts               4（utils 单元）
packages/tui/test/wrap-ansi.test.ts                  19（utils 单元）
packages/tui/test/truncate-to-width.test.ts          16（utils 单元）
packages/tui/test/tab-width.test.ts                   4（utils 单元）
packages/tui/test/regression-regional-indicator-width.test.ts 5（utils 单元）
packages/tui/test/regression-overlay-cjk-boundary.test.ts 4
断言行为见 §7。
```

测试基础设施（**必须一并移植或提供等价物**）：
`test/virtual-terminal.ts`（基于 `@xterm/headless` 5.5.0 的虚拟终端；`start` 时写 `\x1b[?2004h`，
`stop` 时写 `\x1b[?2004l`，`kittyProtocolActive` 恒为 `true`）、
`test/fixtures/exit-without-stop.ts`（子进程 fixture，验证进程 `exit` 时的终端恢复）。

### 0.5 建议阅读顺序

`layout-node.ts` → `terminal-colors.ts` → `program-status.ts` → `wheel-scroll.ts` → `keys.ts` →
`keybindings.ts` → `stdin-buffer.ts` → `terminal.ts` → `tui.ts` → `layout.ts` →
`tui-main-screen.ts` → `alt-screen-search.ts` → `tui-alt-screen.ts` → `terminal-image.ts` →
`native-module-path.ts` → `native-platform.ts` → `native-modifiers.ts` → `native/**` → `index.ts`。

---

## 1. 概览

### 1.1 包的用途

`@earendil-works/kiana-tui`（v1.4.0，ESM，`node >= 22.19.0`，MIT）是终端 UI 框架：

- **两种可互换渲染器**：`TuiMainScreen`（主屏，保留终端 scrollback，差分渲染）与
  `TuiAltScreen`（备用屏 `?1049`，固定高度视口，应用自管滚动）。
- **同步输出**（CSI `?2026`）包裹每帧，避免闪烁。
- **组件协议**：`Component.render(width): string[]`，每行可见宽度必须 ≤ width。
- **覆盖层系统**：anchor/百分比/绝对定位、焦点栈与恢复状态机、非捕获覆盖层。
- **键盘层**：legacy 序列、Kitty keyboard protocol（flags 7）、xterm `modifyOtherKeys` 回退；
  类型安全的 `KeyId` 与可配置键位表。
- **输入层**：`StdinBuffer` 把分片的 stdin 字节流切成完整序列（含括号粘贴）。
- **终端集成**：能力探测（images/trueColor/hyperlinks）、OSC 8 超链接、Kitty/iTerm2 内联图片、
  OSC 10/11/4 颜色查询、OSC 997 明暗主题上报、OSC 7501 程序状态、OSC 9;4 进度、OSC 133 语义提示区。
- **原生加速**：通过 dlsym 动态解析的 N-API 扩展提供剪贴板读取与修饰键状态（macOS/Windows/Linux X11）。

### 1.2 运行面特征（决定 Rust 设计）

| 特征 | TS 语义 | Rust 对应 |
|---|---|---|
| 单线程事件循环 | 所有状态无锁；`process.nextTick` / `setTimeout` 串行 | 单线程 `Rc<RefCell<...>>` 或 actor + 消息队列；**不要**用多线程共享可变状态 |
| 渲染节流 | `MIN_RENDER_INTERVAL_MS = 16`，`performance.now()` | `std::time::Instant`，等价 16 ms 节流 |
| `process.nextTick` | 当前 tick 末尾执行；用户输入用 immediate 路径抢占节流帧 | 事件循环的"当前批次末尾"队列 |
| 全局可变单例 | `_kittyProtocolActive`、`globalKeybindings`、`cachedCapabilities`、`capabilityOverrides`、`cellDimensions`、`kittyImageMetadata` | `thread_local!` + `Cell/RefCell`，或显式注入的 `Runtime`；测试需要能重置 |
| 输出即字符串 | 任意长度，1 MiB 分块写 | `String` + 分块 writer |
| 字符串 UTF-16 | `.length`、`.slice` 按 UTF-16 code unit | 见 §8.3：内部统一用 UTF-8 `String`，但**所有列宽/裁剪逻辑以"终端 cell"为单位**，不按字节或 char 计数 |
| `process.stdout` | 同步/异步混用 | 统一 `Write` trait；`restoreOnExit` 必须**同步**写 fd |

### 1.3 入口点与导出面

`src/index.ts` 是唯一公共入口（`package.json` 无 `exports` 子路径，`main = dist/index.js`）。
它导出 **176 个具名符号**（其中 91 个运行时值、85 个仅类型；含 3 个 `marked` 再导出：`Marked`、`Token`、`Tokens`）。
其中属于本单元的导出见 §2.18 与 §5.1；`colors`/`latex`/`fuzzy`/`autocomplete`/`components` 的导出由对应单元提供。

Rust 侧建议 `kiana_tui` crate 的 `lib.rs` 与 `index.ts` 一一对应（同名 `pub use`），
并在 `cargo doc` 中保持模块可见性分层：公共层（`index` 导出的）与内部层（`pub(crate)` 或 `#[doc(hidden)] pub`，
供同 workspace 的 tui-components / coding-agent 使用）。

### 1.4 与其他包/单元的关系

`packages/tui` 是 monorepo 中**最底层**的包之一：它不导入任何其它 `@earendil-works/*` 包，
只有三个 npm 依赖（`get-east-asian-width`、`marked`、`marked-cjk-friendly`）。依赖方向是单向的：

```
@earendil-works/kiana-tui  (本单元 tui-core + 相邻单元 tui-text / tui-components / tui-terminal / tui-latex / tui-autocomplete)
        ▲
        │ import
@earendil-works/kiana-coding-agent   (49 个测试文件、45 个交互组件、7 个工具渲染器直接依赖)
@earendil-works/kiana-agent / kiana-ai / kiana-mcp / kiana-telemetry / kiana-evals / kiana-chord / kiana-codemode
```

- **单元内部**：`tui-core` 被 `tui-components`（全部组件）、`tui-text`（`utils.ts`/`alt-screen-search.ts` 的归属方）、
  `tui-terminal`（`terminal-image.ts` 的另一归属方）、`tui-latex`、`tui-autocomplete` 共同依赖。
  本规格与 `tui-components.md`、`tui-text.md` 的边界约定见 §0.2。
- **对 `coding-agent` 的契约**：`TUI` / `Terminal` 必须是可替换的 trait（宿主同时使用
  `TuiMainScreen` 与 `TuiAltScreen`，并用 `Proxy` 转发），`Keybindings` 必须是可扩展的开放注册表
  （宿主用 declaration merging 增加 `app.*` 键位），`TuiAltScreenOptions` 的三个回调
  （`scrollToEndIndicator`/`openUrl`/`copySelection`）必须可注入。详见 §5.5。
- **无反向依赖**：tui-core 不感知 coding-agent 的任何类型；所有宿主定制都通过上述注入点完成。

### 1.5 运行平台矩阵

| 平台 | 终端 I/O | 原生剪贴板 | 备注 |
|---|---|---|---|
| Linux | termios raw mode + `SIGWINCH` | `linux-platform-x11.node`（`x11rb`/xcb，需 `DISPLAY`，私有 pthread + 3 s deadline） | 无 `DISPLAY` 时 `getNativeClipboard()` 返回 `undefined` |
| macOS | termios raw mode + `SIGWINCH` | `darwin-platform.node`（AppKit `NSPasteboard`） | Apple Terminal 有 Shift+Enter 特判 |
| Windows | `ENABLE_VIRTUAL_TERMINAL_INPUT`，`GetAsyncKeyState` 修饰键 | `win32-platform.node`（`CF_DIBV5`/`CF_DIB`/`PNG`/`CF_UNICODETEXT`） | 无 `SIGWINCH`；`refreshTerminalDimensions()` 直接 return |
| Termux (Android) | 同 Linux | 无 | 高度变化不触发全量重绘 |
| tmux / screen / Zellij | 同宿主平台 | 视宿主 | 鼠标用按键型上报（`?1002` 而非 `?1003`）；OSC 8 需探针；Kitty 图片默认关闭 |

---

## 2. 逐文件规范

### 2.0 `src/utils.ts` 的依赖契约（本单元不实现，但必须精确可用）

本单元四个渲染文件调用下列函数。Rust 侧需先提供 `tui_utils` 模块，签名与语义如下。
（实现细节、缓存策略与全部测试属 utils 单元；此处只钉死**调用点所依赖的契约**。）

| 函数 | 签名 | 契约 |
|---|---|---|
| `visibleWidth` | `(str: string) => number` | 终端列宽。空串 → 0。纯可打印 ASCII/`\t`/ANSI 走快路径：ASCII 1 列、`\t` 3 列、ANSI 序列 0 列。否则：`\t`→3 空格、剥离 ANSI/OSC/APC 后按 grapheme 累加 `graphemeWidth`。结果缓存 512 项（FIFO 驱逐）。**任何渲染断言都以它为准。** |
| `stripTerminalSequences` | `(str: string) => string` | 删除 ANSI/OSC/APC 序列，保留可见文本；不含 `\x1b` 时原样返回。 |
| `truncateToWidth` | `(text, maxWidth, ellipsis = "...", pad = false) => string` | 见 §2.0.1。 |
| `sliceByColumn` | `(line, startCol, length, strict = false) => string` | `sliceWithWidth(...).text`。 |
| `sliceWithWidth` | `(line, startCol, length, strict=false) => {text, width}` | `length <= 0` → `{"", 0}`。按终端列切；`strict` 影响边界处 ANSI 的取舍。 |
| `extractSegments` | `(line, beforeEnd, afterStart, afterLen, strictAfter = false) => {before, beforeWidth, after, afterWidth}` | `compositeTuiLine` 的核心。 |
| `extractAnsiCode` | `(str, pos) => {code, length} \| null` | 见 §2.0.2。 |
| `normalizeTerminalOutput` | `(str) => string` | Thai/Lao AM（U+0E33/U+0EB3）替换为兼容分解 `ํา` / `ໍາ`；ANSI 序列**之外**的 `\t` 展开为 3 空格（ANSI 内不动）。 |
| `getGraphemeCellRange` | `(line, column) => {start, end} \| undefined` | 返回覆盖可见列 `column` 的 grapheme 的 cell 区间。 |
| `getOsc8LinkAtColumn` | `(line, column) => string \| undefined` | 返回覆盖该列的 OSC 8 URL（`\t` 计 3 列）；无链接 → undefined。 |
| `getActiveBackgroundAnsi` | `(text) => string` | 返回文本末尾仍生效的背景色 SGR（`\x1b[<bg>m`），无 → `""`。 |
| `wrapTextWithAnsi` | `(text, width) => string[]` | 组件层使用；本单元仅在测试中消费。 |
| `PUNCTUATION_REGEX`、`cjkBreakRegex`、`cjkPunctuationRegex`、`autocompleteSeparatorRegex`、`autocompleteBoundaryRegex` | 正则常量 | 组件层使用。 |

#### 2.0.1 `truncateToWidth` 精确分支（逐字记录，因为渲染器会间接依赖）

```
maxWidth <= 0                          → ""
text 为空                              → pad ? " " * maxWidth : ""
ellipsisWidth >= maxWidth:
    textWidth <= maxWidth              → pad ? text + 空格补齐 : text
    clippedEllipsis.width === 0        → pad ? " " * maxWidth : ""
    否则                                → finalizeTruncatedResult("", 0, clippedEllipsis.text, …)
isPrintableAscii(text):
    text.length <= maxWidth            → pad ? text + 空格补齐 : text
    （否则落入通用分支，按 grapheme 累加）
通用分支：逐 grapheme 累加，超宽时用 finalizeTruncatedResult 追加省略号并可选补齐
```

`finalizeTruncatedResult(prefix, prefixWidth, ellipsis, ellipsisWidth, maxWidth, pad)` 输出：

```
`${prefix}${hyperlinkClose}${"\x1b[0m"}${ellipsis}${"\x1b[0m"}`      // ellipsis 非空
`${prefix}${hyperlinkClose}${"\x1b[0m"}`                             // ellipsis 为空
```

其中 `hyperlinkClose = getActiveOsc8Close(prefix)`：若 `prefix` 中含 OSC 8 打开序列，则追加
`\x1b]8;;<terminator>`（terminator 为 `\x07` 或 `\x1b\\`，沿用原序列）；否则为 `""`。
`pad` 为真时再补 `" " * max(0, maxWidth - prefixWidth - ellipsisWidth)`。

#### 2.0.2 `ansiCodeLength` 精确规则（**渲染逐字节一致的关键**）

```
pos >= len 或 str[pos] != ESC            → 0
next = str[pos+1]
next == "["（CSI）：
    从 pos+2 扫描，遇到 0x6d('m') / 0x47('G') / 0x4b('K') / 0x48('H') / 0x4a('J') → 返回 j+1-pos
    扫到结尾                           → 0（不是合法序列）
next == "]"（OSC）或 next == "_"（APC）：
    从 pos+2 扫描，遇 BEL(0x07) → 返回 j+1-pos
                    遇 ESC 且 str[j+1]=="\\" → 返回 j+2-pos
    扫到结尾                           → 0
其它（含 ESC P DCS、ESC O SS3、ESC + 单字符） → 0
```

**推论**（Rust 必须一致）：
- `\x1b[?25l` 以 `l` 结尾 → **不**被识别为 ANSI（`l` 不在 m/G/K/H/J 内），因此 `visibleWidth` 会把它
  当可见文本计入宽度（l 计 1 列）。这是 TS 实现的既有行为，测试（`visibleWidth`、渲染断言）依赖它。
- OSC/APC 未终止 → 该序列整体不计宽，后续文本仍被计入（`escapeIndex++` 逐字符试探）。
- `\x1b]8;;url\x1b\\` 与 `\x1b]8;;url\x07` 都识别。

---

### 2.1 `src/layout-node.ts`（51 行）

```ts
export const LAYOUT_NODE = Symbol.for("@earendil-works/kiana-tui/layout-node");
```

`LAYOUT_NODE` 是一个全局注册符号——**跨包实例共享**（多个 kiana-tui 副本之间也能识别）。
Rust 侧无法复刻 JS symbol；等价物：`pub const LAYOUT_NODE: &str = "@earendil-works/kiana-tui/layout-node";`
作为 `LayoutComponent::layout_node()` 的标记，并保证"同一布局协议可被 extern 组件实现"。

```ts
export interface LayoutViewport { width: number; height: number; }

export interface StackLayoutEntry {
  component: Component;
  basis?: number | "auto";
  grow?: number;
  shrink?: number;
  minSize?: number;
  maxSize?: number;
  visible?: (viewport: LayoutViewport) => boolean;
}

export interface StackLayoutNode {
  type: "vstack" | "hstack";
  entries: readonly StackLayoutEntry[];
  gap: number;
  align: "stretch" | "start" | "center" | "end";
}

export interface ScrollLayoutState {
  readonly scrollTop: number;
  readonly primary: boolean;
  readonly overscroll: "chain" | "contain";
  readonly viewportHeight: number;
  getContentWidth(width: number): number;
  updateLayout(contentHeight: number, viewportHeight: number, requestRender: () => void): void;
}

export interface ScrollLayoutNode { type: "scroll"; component: Component; state: ScrollLayoutState; }

export type LayoutNode = StackLayoutNode | ScrollLayoutNode;

export interface LayoutComponent extends Component { [LAYOUT_NODE](): LayoutNode; }

export function getLayoutNode(component: Component): LayoutNode | undefined {
  const candidate = component as Partial<LayoutComponent>;
  return typeof candidate[LAYOUT_NODE] === "function" ? candidate[LAYOUT_NODE]() : undefined;
}
```

`getLayoutNode` 每次调用都**重新**取节点（节点是即时构造的视图，不是缓存对象；
`ScrollLayoutNode.state` 就是 `ScrollView` 自身，因此布局过程中可直接读写 `scrollTop`）。

Rust 设计：
```rust
pub enum LayoutNode {
    Stack(StackLayoutNode),
    Scroll(ScrollLayoutNode),
}
pub trait LayoutComponent: Component {
    fn layout_node(&self) -> LayoutNode;
}
pub fn get_layout_node(c: &dyn Component) -> Option<LayoutNode>;
```
注意 `LayoutNode` 含 `Component` 引用 → 用 `Rc<dyn Component>`（或 arena 索引）而非 `&dyn`。

---

### 2.2 `src/terminal-colors.ts`（91 行）

```ts
export interface RgbColor { r: number; g: number; b: number; }
export type TerminalColorScheme = "dark" | "light";
export interface TerminalColors {
  foreground?: RgbColor;   // OSC 10
  background?: RgbColor;   // OSC 11
  palette?: RgbColor[];    // OSC 4，0–15；只有 16 个全到才设置
}
export type OscColorTarget = "foreground" | "background" | number;
```

`hexToRgb(hex)`：可带或不带 `#`；取前 6 位，`parseInt(...,16)` 三段。**不做长度校验**（调用方保证）。

`parseOscHexChannel(channel)`：
```
不匹配 /^[0-9a-f]+$/i        → undefined
max = 16**len - 1；max <= 0  → undefined
否则                          → round(parseInt(channel,16) / max * 255)
```

**【逐字】正则**
```js
const OSC_COLOR_RESPONSE_PATTERN = /^\x1b\](?:(1[01])|4;(\d{1,3}));([^\x07\x1b]*)(?:\x07|\x1b\\)$/i;
const COLOR_SCHEME_REPORT_PATTERN = /^(?:\x1b\[\?997;(1|2)n)+$/;
```

`parseOscColorResponse(data)`：
- 不匹配 → `undefined`
- `match[1] === "10"` → `"foreground"`；`"11"` → `"background"`；否则 `parseInt(match[2],10)`（0–999 数字）
- `rgb = parseOscColorValue(match[3])`

`parseOscColorValue(raw)`（**私有**）：
```
value = raw.trim()
以 "#" 开头：
    hex 长度 6 且 /^[0-9a-f]{6}$/i     → hexToRgb(value)
    hex 长度 12 且 /^[0-9a-f]{12}$/i   → 三段各 4 位走 parseOscHexChannel
    否则                               → undefined
否则：
    rgbValue = value.replace(/^rgba?:/i, "")
    按 "/" 切三段；缺任一段 → undefined
    三段各 parseOscHexChannel，任一 undefined → undefined
    否则 {r,g,b}
```

注意：`rgba:` 前缀也被接受（尽管 alpha 段会被忽略——它其实是第 4 个 `/` 段之外的文本，
若存在第 4 段则被 `split("/")` 忽略）。

`parseTerminalColorSchemeReport(data)`：匹配 `COLOR_SCHEME_REPORT_PATTERN` 后，
`match[1] === "2"` → `"light"`，否则（`"1"`）→ `"dark"`。正则要求**一次或多次重复**同一序列，
因此 `\x1b[?997;1n\x1b[?997;2n` 也匹配，取**最后一次**匹配组（JS `match` 中重复组的值为最后一次捕获）。

Rust 正则注意：`\x1b` 在 Rust `regex` 里写作 `\x1B` 或 `\u{1b}`；`.match` 的重复捕获组语义
（最后一次捕获）需手动实现为"逐段扫描，记录最后一个"。

---

### 2.3 `src/program-status.ts`（53 行）

文件头注释（**逐字**，含外部规范链接，Rust 侧建议在模块文档中保留）：
```
Program Status Protocol (OSC 7501): a program tells the terminal whether it is idle, working,
blocked on the user, done, or failed. Only the root record is supported.

Spec: https://www.superlogical.com/rex/docs/build/program-status
```
"Only the root record is supported" 是一条**行为约束**：OSC 7501 允许嵌套/子记录（如子任务状态），
本实现只处理最外层 record，不对子记录做任何递归或去重。

```ts
export interface ProgramStatus {
  state: "idle" | "working" | "blocked" | "done" | "error" | "clear";
  app?: string;                          // [A-Za-z0-9_.+-]{1,32}
  kind?: "permission" | "question" | "auth";   // 仅 blocked 时输出
  message?: string;                      // 控制字符变空格；UTF-8 截到 2048 字节
}

export const PROGRAM_STATUS_QUERY = "\x1b]7501;?\x1b\\";      // 【逐字】
export function isProgramStatusReply(sequence: string): boolean {
  return /^\x1b\]7501;\?[^\x07\x1b]*(?:\x07|\x1b\\)$/.test(sequence);
}
const APP_PATTERN = /^[A-Za-z0-9_.+-]{1,32}$/;
const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f-\u009f]+/g;
const MAX_MESSAGE_BYTES = 2048;
```

`truncateUtf8(text, maxBytes)`：
```
Buffer.byteLength(text,"utf8") <= maxBytes → text
否则按 code point 遍历（for...of 迭代 code point，但 `char.length` 是 **UTF-16 code unit** 数），
    bytes = 0; end = 0
    for (char of text) { size = Buffer.byteLength(char,"utf8")
                         if (bytes + size > maxBytes) break
                         bytes += size; end += char.length }
    return text.slice(0, end)
```
（切点永远落在 code point 边界，不会产生半个 UTF-8 序列。Rust 侧用 `char_indices` /
`char::len_utf8` 累加即可，但注意 TS 的 `end` 是 UTF-16 索引——只有做切片时才等价，
若 Rust 直接截 `&str[..end]` 必须用**字节**索引，两者数值不同。）

`formatProgramStatus(status)` **【逐字】**：
```
pairs = [`state=${status.state}`]
if (status.app !== undefined && APP_PATTERN.test(status.app)) pairs.push(`app=${status.app}`)
if (status.state === "blocked" && status.kind) pairs.push(`kind=${status.kind}`)
message = truncateUtf8((status.message ?? "").replace(CONTROL_CHARACTERS, " ").trim(), MAX_MESSAGE_BYTES)
if (message) pairs.push(`msg=${Buffer.from(message,"utf8").toString("base64")}`)
return `\x1b]7501;${pairs.join(":")}\x1b\\`
```

要点：
- 字段间分隔符是 **`:`**（不是 `;`），末尾终止符是 **`ESC \`**（不是 BEL）。
- `app` 非法时**静默省略**（不报错）。
- `kind` 只在 `state === "blocked"` 时输出，且必须是三个合法值之一（类型层面保证）。
- `message` 先替换控制字符为空格，再 `trim()`，再按 UTF-8 字节截断；空串不输出。
- `state === "clear"` 时输出 `\x1b]7501;state=clear\x1b\\`（可选 `app`/`msg` 仍会附带）。

测试（`program-status.test.ts`，5 个 it）：
1. `encodes state, app, kind, and a base64 message`
2. `omits kind outside blocked, invalid app names, and empty messages`
3. `replaces control characters, which make terminals discard the report`
4. `cuts long messages at a UTF-8 boundary within the spec limits`
5. `accepts the query echo with either terminator and future pairs`

---

### 2.4 `src/wheel-scroll.ts`（82 行）

```ts
export type WheelScrollLines = number | "auto";
const BURST_GAP_MS = 5;
const GESTURE_GAP_MS = 200;
const REFERENCE_GAP_MS = 100;
const MAX_AUTO_LINES = 6;

function terminalAcceleratesWheel(): boolean {
  const env = process.env;
  return process.platform === "darwin" && env.SSH_CONNECTION === undefined
      && env.SSH_CLIENT === undefined && env.SSH_TTY === undefined;
}

export class WheelScrollAccelerator {
  constructor(lines: WheelScrollLines = "auto", accelerate = !terminalAcceleratesWheel())
  setLines(lines): void          // 设置并 reset()
  next(direction: -1 | 1, now: number): number
}
```

状态：`lines`、`accelerate`、`lastTime = -Infinity`、`lastDirection = 0`、`averageGap: number|undefined`、`carry = 0`。

`next(direction, now)` **精确算法**：
```
if (lines !== "auto")
    return Number.isFinite(lines) ? Math.max(1, Math.floor(lines)) : 1
if (!accelerate) return 1

gap = now - lastTime
sameGesture = direction === lastDirection && gap <= GESTURE_GAP_MS
lastTime = now; lastDirection = direction
if (!sameGesture) { averageGap = undefined; carry = 0; return 1 }
if (gap < BURST_GAP_MS) return 1

averageGap = averageGap === undefined ? gap : (averageGap + gap) / 2
lines = Math.min(MAX_AUTO_LINES, Math.max(1, REFERENCE_GAP_MS / averageGap)) + carry
whole = Math.floor(lines)
carry = lines - whole
return whole
```
`reset()`（私有）恢复四个字段初值。

文档示例：间隔 100 ms → 每次 1 行；50 ms → 2 行；20 ms → 5 行。

测试（`wheel-scroll.test.ts`，6 个 it）：固定行数忽略时间；终端已加速时 auto 恒 1 行；
auto 随速度放大；单次 notch 的突发事件不加速（`< BURST_GAP_MS`）；方向改变与暂停重置；
小数行在事件间累积（carry）。

Rust 注意：`Number.NEGATIVE_INFINITY` 与浮点 `averageGap` 的 IEEE-754 行为需一致（`f64`）。

---

### 2.5 `src/keys.ts`（1455 行）

#### 2.5.1 全局协议状态

```ts
let _kittyProtocolActive = false;
export function setKittyProtocolActive(active: boolean): void;
export function isKittyProtocolActive(): boolean;
```
由 `ProcessTerminal` 在协商成功/停止/drainInput 时设置。**全局、进程级**。
Rust：`thread_local! { static KITTY_PROTOCOL_ACTIVE: Cell<bool> }`，并暴露
`set_kitty_protocol_active` / `is_kitty_protocol_active`；测试必须能重置。

另一个隐式全局：`let _lastEventType: KeyEventType = "press"`，仅由 `parseKittySequence` 写入，
供 `isKeyRelease()` 查询（历史遗留；当前 `isKeyRelease` 实际用子串判断，不读该变量）。

#### 2.5.2 键标识符类型

```ts
type Letter = "a" | … | "z";
type Digit  = "0" | … | "9";
type SymbolKey = "`"|"-"|"="|"["|"]"|"\\"|";"|"'"|","|"."|"/"
               | "!"|"@"|"#"|"$"|"%"|"^"|"&"|"*"|"("|")"
               | "_"|"+"|"|"|"~"|"{"|"}"|":"|"<"|">"|"?";
type SpecialKey = "escape"|"esc"|"enter"|"return"|"tab"|"space"|"backspace"|"delete"|"insert"|"clear"
                | "home"|"end"|"pageUp"|"pageDown"|"up"|"down"|"left"|"right"|"f1"…"f12";
type BaseKey = Letter | Digit | SymbolKey | SpecialKey;
type ModifierName = "ctrl" | "shift" | "alt" | "super";
export type KeyId = BaseKey | ModifiedKeyId<BaseKey>;   // 修饰符顺序任意，可 1–4 个
```

`Key` 辅助对象（运行时是普通对象，但生成的就是字符串）：
- 特殊键：`escape esc enter return tab space backspace delete insert clear home end pageUp pageDown up down left right f1..f12`
- 符号键（**名称 → 字面量**）：`backtick` `` ` ``、`hyphen` `-`、`equals` `=`、`leftbracket` `[`、
  `rightbracket` `]`、`backslash` `\`、`semicolon` `;`、`quote` `'`、`comma` `,`、`period` `.`、
  `slash` `/`、`exclamation` `!`、`at` `@`、`hash` `#`、`dollar` `$`、`percent` `%`、`caret` `^`、
  `ampersand` `&`、`asterisk` `*`、`leftparen` `(`、`rightparen` `)`、`underscore` `_`、`plus` `+`、
  `pipe` `|`、`tilde` `~`、`leftbrace` `{`、`rightbrace` `}`、`colon` `:`、`lessthan` `<`、
  `greaterthan` `>`、`question` `?`
- 单修饰：`ctrl(k) shift(k) alt(k) super(k)` → `` `ctrl+${k}` `` 等
- 组合修饰：`ctrlShift shiftCtrl ctrlAlt altCtrl shiftAlt altShift ctrlSuper superCtrl shiftSuper
  superShift altSuper superAlt`
- 三重：`ctrlShiftAlt ctrlShiftSuper`

（`percent: "%" as const`，`SymbolKey` 类型里同样只有 `"%"`（**无尾随空格**）。
Rust 侧用 `&'static str` 常量表；`SymbolKey` / `SYMBOL_KEYS` 共 **31** 个、`SHIFTED_SYMBOL_KEYS` 共 **20** 个，
两者都需逐一核对，因为 `matchesKey` 的 shifted-symbol 分支依赖这两个集合的**成员关系**而非大小。）

#### 2.5.3 常量

```ts
const SYMBOL_KEYS = new Set(["`","-","=","[","]","\\",";","'",",",".","/","!","@","#","$","%","^","&","*","(",
                              ")","_","+","|","~","{","}",":","<",">","?"]);          // 31 个
const SHIFTED_SYMBOL_KEYS = new Set("~!@#$%^&*()_+|{}:<>?");                             // 20 个
const MODIFIER_NAMES = ["shift","alt","ctrl","super"] as const;                          // 注意顺序
const MODIFIERS = { shift: 1, alt: 2, ctrl: 4, super: 8 } as const;
const LOCK_MASK = 64 + 128;              // Caps Lock + Num Lock = 192
const CODEPOINTS = { escape: 27, tab: 9, enter: 13, space: 32, backspace: 127, kpEnter: 57414 } as const;
const ARROW_CODEPOINTS = { up: -1, down: -2, right: -3, left: -4 } as const;
const FUNCTIONAL_CODEPOINTS = { delete: -10, insert: -11, pageUp: -12, pageDown: -13, home: -14, end: -15 } as const;
```

`KITTY_FUNCTIONAL_KEY_EQUIVALENTS`（Map<number,number>）逐条：

| Kitty codepoint | 等价 | 含义 |
|---|---|---|
| 57399–57408 | 48–57 | KP_0…KP_9 |
| 57409 | 46 | KP_DECIMAL → `.` |
| 57410 | 47 | KP_DIVIDE → `/` |
| 57411 | 42 | KP_MULTIPLY → `*` |
| 57412 | 45 | KP_SUBTRACT → `-` |
| 57413 | 43 | KP_ADD → `+` |
| 57415 | 61 | KP_EQUAL → `=` |
| 57416 | 44 | KP_SEPARATOR → `,` |
| 57417 | -4 | left |
| 57418 | -3 | right |
| 57419 | -1 | up |
| 57420 | -2 | down |
| 57421 | -12 | pageUp |
| 57422 | -13 | pageDown |
| 57423 | -14 | home |
| 57424 | -15 | end |
| 57425 | -11 | insert |
| 57426 | -10 | delete |

（注意：57414 kpEnter 不在此表内，由 `CODEPOINTS.kpEnter` 单独处理；57427 起无映射。）

```ts
function normalizeKittyFunctionalCodepoint(cp: number): number   // Map 查表，未命中返回 cp
function normalizeShiftedLetterIdentityCodepoint(cp: number, modifier: number): number {
  const eff = modifier & ~LOCK_MASK;
  if ((eff & MODIFIERS.shift) !== 0 && cp >= 65 && cp <= 90) return cp + 32;   // 'A'-'Z' → 'a'-'z'
  return cp;
}
function getLogicalShiftedSymbolCandidate(cp: number|undefined, modifier: number,
                                          allowed: ReadonlySet<string>)
  : { codepoint: number; modifier: number } | undefined {
  if (cp === undefined || (modifier & MODIFIERS.shift) === 0) return undefined;
  const normalized = normalizeKittyFunctionalCodepoint(cp);
  if (!allowed.has(String.fromCharCode(normalized))) return undefined;
  return { codepoint: normalized, modifier: modifier & ~MODIFIERS.shift };
}
```

#### 2.5.4 Legacy 序列表（**逐字**）

```ts
const LEGACY_KEY_SEQUENCES = {
  up:       ["\x1b[A", "\x1bOA"],
  down:     ["\x1b[B", "\x1bOB"],
  right:    ["\x1b[C", "\x1bOC"],
  left:     ["\x1b[D", "\x1bOD"],
  home:     ["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
  end:      ["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
  insert:   ["\x1b[2~"],
  delete:   ["\x1b[3~"],
  pageUp:   ["\x1b[5~", "\x1b[[5~"],
  pageDown: ["\x1b[6~", "\x1b[[6~"],
  clear:    ["\x1b[E", "\x1bOE"],
  f1: ["\x1bOP", "\x1b[11~", "\x1b[[A"],
  f2: ["\x1bOQ", "\x1b[12~", "\x1b[[B"],
  f3: ["\x1bOR", "\x1b[13~", "\x1b[[C"],
  f4: ["\x1bOS", "\x1b[14~", "\x1b[[D"],
  f5: ["\x1b[15~", "\x1b[[E"],
  f6: ["\x1b[17~"], f7: ["\x1b[18~"], f8: ["\x1b[19~"], f9: ["\x1b[20~"],
  f10: ["\x1b[21~"], f11: ["\x1b[23~"], f12: ["\x1b[24~"],
} as const;

const LEGACY_SHIFT_SEQUENCES = {
  up: ["\x1b[a"], down: ["\x1b[b"], right: ["\x1b[c"], left: ["\x1b[d"], clear: ["\x1b[e"],
  insert: ["\x1b[2$"], delete: ["\x1b[3$"], pageUp: ["\x1b[5$"], pageDown: ["\x1b[6$"],
  home: ["\x1b[7$"], end: ["\x1b[8$"],
} as const;

const LEGACY_CTRL_SEQUENCES = {
  up: ["\x1bOa"], down: ["\x1bOb"], right: ["\x1bOc"], left: ["\x1bOd"], clear: ["\x1bOe"],
  insert: ["\x1b[2^"], delete: ["\x1b[3^"], pageUp: ["\x1b[5^"], pageDown: ["\x1b[6^"],
  home: ["\x1b[7^"], end: ["\x1b[8^"],
} as const;
```

`LEGACY_SEQUENCE_KEY_IDS`（Record<string, KeyId>）逐条 **【逐字】**：

```
\x1bOA up ; \x1bOB down ; \x1bOC right ; \x1bOD left ; \x1bOH home ; \x1bOF end
\x1b[E clear ; \x1bOE clear ; \x1bOe ctrl+clear ; \x1b[e shift+clear
\x1b[2~ insert ; \x1b[2$ shift+insert ; \x1b[2^ ctrl+insert
\x1b[3$ shift+delete ; \x1b[3^ ctrl+delete
\x1b[[5~ pageUp ; \x1b[[6~ pageDown
\x1b[a shift+up ; \x1b[b shift+down ; \x1b[c shift+right ; \x1b[d shift+left
\x1bOa ctrl+up ; \x1bOb ctrl+down ; \x1bOc ctrl+right ; \x1bOd ctrl+left
\x1b[5$ shift+pageUp ; \x1b[6$ shift+pageDown ; \x1b[7$ shift+home ; \x1b[8$ shift+end
\x1b[5^ ctrl+pageUp ; \x1b[6^ ctrl+pageDown ; \x1b[7^ ctrl+home ; \x1b[8^ ctrl+end
\x1bOP f1 ; \x1bOQ f2 ; \x1bOR f3 ; \x1bOS f4
\x1b[11~ f1 ; \x1b[12~ f2 ; \x1b[13~ f3 ; \x1b[14~ f4
\x1b[[A f1 ; \x1b[[B f2 ; \x1b[[C f3 ; \x1b[[D f4 ; \x1b[[E f5
\x1b[15~ f5 ; \x1b[17~ f6 ; \x1b[18~ f7 ; \x1b[19~ f8 ; \x1b[20~ f9
\x1b[21~ f10 ; \x1b[23~ f11 ; \x1b[24~ f12
\x1bb alt+left ; \x1bf alt+right ; \x1bp alt+up ; \x1bn alt+down
```

#### 2.5.5 Kitty 序列解析

```ts
type KeyEventType = "press" | "repeat" | "release";
interface ParsedKittySequence { codepoint: number; shiftedKey?: number; baseLayoutKey?: number;
                                modifier: number; eventType: KeyEventType; }
interface ParsedModifyOtherKeysSequence { codepoint: number; modifier: number; }
```

`parseEventType(s)`：`undefined`→`"press"`；`"2"`→`"repeat"`；`"3"`→`"release"`；其它→`"press"`。

`parseKittySequence(data)` —— 依次尝试 4 个正则（**顺序不可变**）：

1. **CSI-u**（flag 1/2/4）：
   ```js
   /^\x1b\[(\d+)(?::(\d*))?(?::(\d+))?(?:;(\d+))?(?::(\d+))?u$/
   ```
   - `codepoint = parseInt(g1)`
   - `shiftedKey = (g2 && g2.length>0) ? parseInt(g2) : undefined`
   - `baseLayoutKey = g3 ? parseInt(g3) : undefined`
   - `modValue = g4 ? parseInt(g4) : 1`；`modifier = modValue - 1`
   - `eventType = parseEventType(g5)`；写 `_lastEventType`
2. **箭头键带修饰**：
   ```js
   /^\x1b\[1;(\d+)(?::(\d+))?([ABCD])$/
   ```
   `codepoint` 映射 `{A:-1,B:-2,C:-3,D:-4}`，`modifier = g1 - 1`，`eventType = parseEventType(g2)`
3. **功能键 `~`**：
   ```js
   /^\x1b\[(\d+)(?:;(\d+))?(?::(\d+))?~$/
   ```
   `funcCodes = {2:insert(-11), 3:delete(-10), 5:pageUp(-12), 6:pageDown(-13), 7:home(-14), 8:end(-15)}`；
   未在表中的 keyNum → **继续**尝试后续正则（返回 null 而非立即失败）。
4. **Home/End H/F**：
   ```js
   /^\x1b\[1;(\d+)(?::(\d+))?([HF])$/
   ```
   `H` → home(-14)，`F` → end(-15)。

全部失败 → `null`。

`matchesKittySequence(data, expectedCodepoint, expectedModifier)` **精确语义**：
```
parsed = parseKittySequence(data); 若 null → false
actualMod   = parsed.modifier & ~LOCK_MASK
expectedMod = expectedModifier & ~LOCK_MASK
normalizedCp       = normalizeShiftedLetterIdentityCodepoint(normalizeKittyFunctionalCodepoint(parsed.codepoint), parsed.modifier)
normalizedExpected = normalizeShiftedLetterIdentityCodepoint(normalizeKittyFunctionalCodepoint(expectedCodepoint), expectedModifier)

// 1) 物理匹配
if (actualMod === expectedMod && normalizedCp === normalizedExpected) return true

// 2) 逻辑 shifted symbol 匹配（Kitty 把 "+" 报成 Shift + "="）
logical = getLogicalShiftedSymbolCandidate(parsed.shiftedKey, actualMod, SYMBOL_KEYS)
if (logical && (logical.modifier & ~LOCK_MASK) === expectedMod
    && logical.codepoint === normalizeKittyFunctionalCodepoint(expectedCodepoint)) return true

if (actualMod !== expectedMod) return false

// 3) base layout 回退（非拉丁字母/数字/已知符号时才用）
if (parsed.baseLayoutKey !== undefined && parsed.baseLayoutKey === expectedCodepoint) {
    cp = normalizedCp
    isLatinLetter = cp >= 97 && cp <= 122
    isKnownSymbol = SYMBOL_KEYS.has(String.fromCharCode(cp))
    if (!isLatinLetter && !isKnownSymbol) return true
}
return false
```

`parseModifyOtherKeysSequence(data)`：
```js
/^\x1b\[27;(\d+);(\d+)~$/
```
`modifier = g1 - 1`，`codepoint = parseInt(g2)`。

`matchesModifyOtherKeys(data, expectedKeycode, expectedModifier)`：
`parsed && parsed.codepoint === expectedKeycode && parsed.modifier === expectedModifier`。

`matchesPrintableModifyOtherKeys(data, expectedKeycode, expectedModifier)`：
```
parsed = parseModifyOtherKeysSequence(data); 若 null → false
physical = parsed.modifier === expectedModifier
        && normalizeShiftedLetterIdentityCodepoint(parsed.codepoint, parsed.modifier)
        === normalizeShiftedLetterIdentityCodepoint(expectedKeycode, expectedModifier)
if (physical) return true
logical = getLogicalShiftedSymbolCandidate(parsed.codepoint, parsed.modifier, SHIFTED_SYMBOL_KEYS)
return logical?.modifier === expectedModifier && logical.codepoint === expectedKeycode
```

#### 2.5.6 键释放/重复检测

```ts
export function isKeyRelease(data: string): boolean {
  if (data.includes("\x1b[200~")) return false;      // 括号粘贴内容永不当作释放
  return [":3u",":3~",":3A",":3B",":3C",":3D",":3H",":3F"].some(s => data.includes(s));
}
export function isKeyRepeat(data: string): boolean {
  if (data.includes("\x1b[200~")) return false;
  return [":2u",":2~",":2A",":2B",":2C",":2D",":2H",":2F"].some(s => data.includes(s));
}
```

#### 2.5.7 `matchesKey(data, keyId)` — 完整分支

`parseKeyId(keyId)`：
```
key = keyId.toLowerCase()
循环：按 MODIFIER_NAMES 顺序 ["shift","alt","ctrl","super"] 找 key.startsWith(`${name}+`)；
      命中则若该修饰符已出现 → return null；否则加入集合并 key = key.slice(name.length+1)
若 key 为空，或 key 含 "+" 且 key !== "+" → null
返回 { key, ctrl, shift, alt, super }
```
注意 `key` 已小写化，因此 switch 里 `case "pageup"`、`case "pagedown"` 是小写。

修饰符位：`modifier = shift?1:0 | alt?2:0 | ctrl?4:0 | super?8:0`。

各分支（**逐字逻辑**；未列出的组合返回 false）：

- **escape / esc**：`modifier !== 0 → false`；否则 `data === "\x1b"` 或
  `matchesKittySequence(data, 27, 0)` 或 `matchesModifyOtherKeys(data, 27, 0)`
- **space**：
  - 若 `!_kittyProtocolActive`：`modifier === ctrl && data === "\x00"` → true；
    `modifier === alt && data === "\x1b "` → true
  - `modifier === 0`：`data === " "` 或 kitty(32,0) 或 modifyOtherKeys(32,0)
  - 否则：kitty(32,modifier) 或 modifyOtherKeys(32,modifier)
- **tab**：
  - `modifier === shift`：`data === "\x1b[Z"` 或 kitty(9,shift) 或 modifyOtherKeys(9,shift)
  - `modifier === 0`：`data === "\t"` 或 kitty(9,0)
  - 否则：kitty(9,modifier) 或 modifyOtherKeys(9,modifier)
- **enter / return**：
  - `modifier === shift`：kitty(13,shift) || kitty(57414,shift) || modifyOtherKeys(13,shift)；
    若 kitty 激活则 `data === "\x1b\r" || data === "\n"`，否则 false
  - `modifier === alt`：kitty(13,alt) || kitty(57414,alt) || modifyOtherKeys(13,alt)；
    kitty **未**激活时 `data === "\x1b\r"`；激活时 false
  - `modifier === 0`：`data === "\r"` 或（!kitty 且 `data === "\n"`）或 `data === "\x1bOM"`
    或 kitty(13,0) 或 kitty(57414,0)
  - 其它 modifier：kitty(13,m) || kitty(57414,m) || modifyOtherKeys(13,m)
- **backspace**：
  - `modifier === alt`：`data === "\x1b\x7f" || data === "\x1b\b"` → true；
    否则 kitty(127,alt) || modifyOtherKeys(127,alt)
  - `modifier === ctrl`：`matchesRawBackspace(data, ctrl)` → true；否则 kitty(127,ctrl) || mOK(127,ctrl)
  - `modifier === 0`：`matchesRawBackspace(data,0)` || kitty(127,0) || mOK(127,0)
  - 其它：kitty(127,m) || mOK(127,m)
- **insert**：0 → legacy insert 序列 || kitty(-11,0)；否则 legacyShift/Ctrl("insert",m) || kitty(-11,m)
- **delete**：0 → legacy delete || kitty(-10,0)；否则 legacyShift/Ctrl("delete",m) || kitty(-10,m)
- **clear**：0 → legacy clear；否则 legacyShift/Ctrl("clear",m)（**clear 没有 kitty 分支**）
- **home**：0 → legacy home || kitty(-14,0)；否则 legacyShift/Ctrl("home",m) || kitty(-14,m)
- **end**：同上，codepoint -15
- **pageup**：0 → legacy pageUp || kitty(-12,0)；否则 legacyShift/Ctrl("pageUp",m) || kitty(-12,m)
- **pagedown**：同上，-13
- **up**：
  - `modifier === alt`：`data === "\x1bp"` || kitty(-1,alt)
  - `modifier === 0`：legacy up || kitty(-1,0)
  - 否则 legacyShift/Ctrl("up",m) || kitty(-1,m)
- **down**：alt → `data === "\x1bn"` || kitty(-2,alt)；0 → legacy || kitty(-2,0)；否则 legacy || kitty(-2,m)
- **left**：
  - alt：`data === "\x1b[1;3D"` || (!kitty && `data === "\x1bB"`) || `data === "\x1bb"` || kitty(-4,alt)
  - ctrl：`data === "\x1b[1;5D"` || legacyCtrl("left") || kitty(-4,ctrl)
  - 0：legacy left || kitty(-4,0)
  - 否则 legacyShift/Ctrl || kitty(-4,m)
- **right**：
  - alt：`data === "\x1b[1;3C"` || (!kitty && `data === "\x1bF"`) || `data === "\x1bf"` || kitty(-3,alt)
  - ctrl：`data === "\x1b[1;5C"` || legacyCtrl("right") || kitty(-3,ctrl)
  - 0：legacy right || kitty(-3,0)
  - 否则 legacy || kitty(-3,m)
- **f1..f12**：`modifier !== 0 → false`；否则 `LEGACY_KEY_SEQUENCES[key].includes(data)`

**单字符分支**（`key.length === 1` 且（a-z / 0-9 / SYMBOL_KEYS））：
```
codepoint = key.charCodeAt(0); rawCtrl = rawCtrlChar(key)
isLetter = a-z；isDigit = 0-9
if (modifier === ctrl+alt && !kitty && rawCtrl && data === `\x1b${rawCtrl}`) return true
if (modifier === alt && !kitty && (isLetter||isDigit||symbol) && data === `\x1b${key}`) return true
if (modifier === ctrl)      return (rawCtrl && data===rawCtrl) || kitty(cp,ctrl) || matchesPrintableModifyOtherKeys(cp,ctrl)
if (modifier === shift+ctrl) return kitty(cp,shift+ctrl) || matchesPrintableModifyOtherKeys(cp,shift+ctrl)
if (modifier === shift)     return (isLetter && data === key.toUpperCase()) || kitty(cp,shift) || mPOK(cp,shift)
if (modifier !== 0)         return kitty(cp,modifier) || mPOK(cp,modifier)
return data === key || kitty(cp,0) || mPOK(cp,0)
```
最后 `return false`。

辅助函数：
```ts
function rawCtrlChar(key: string): string | null {
  const char = key.toLowerCase(); const code = char.charCodeAt(0);
  if ((code>=97&&code<=122) || char==="[" || char==="\\" || char==="]" || char==="_")
      return String.fromCharCode(code & 0x1f);
  if (char === "-") return String.fromCharCode(31);   // 与 Ctrl+_ 同键
  return null;
}
function isWindowsTerminalSession(): boolean {
  return Boolean(process.env.WT_SESSION) && !process.env.SSH_CONNECTION
      && !process.env.SSH_CLIENT && !process.env.SSH_TTY;
}
function matchesRawBackspace(data: string, expectedModifier: number): boolean {
  if (data === "\x7f") return expectedModifier === 0;
  if (data !== "\x08") return false;
  return isWindowsTerminalSession() ? expectedModifier === MODIFIERS.ctrl : expectedModifier === 0;
}
function matchesLegacyModifierSequence(data, key, modifier) {
  if (modifier === MODIFIERS.shift) return LEGACY_SHIFT_SEQUENCES[key].includes(data);
  if (modifier === MODIFIERS.ctrl)  return LEGACY_CTRL_SEQUENCES[key].includes(data);
  return false;
}
```

#### 2.5.8 `parseKey(data)` — 反向解析

`formatParsedKey(codepoint, modifier, baseLayoutKey?)`：
```
normalizedCodepoint = normalizeKittyFunctionalCodepoint(codepoint)
identityCodepoint   = normalizeShiftedLetterIdentityCodepoint(normalizedCodepoint, modifier)
isLatinLetter = 97..122 ; isDigit = 48..57 ; isKnownSymbol = SYMBOL_KEYS.has(char)
effectiveCodepoint = (三者之一) ? identityCodepoint : (baseLayoutKey ?? identityCodepoint)
keyName 映射顺序：
  27→"escape" ; 9→"tab" ; 13 或 57414→"enter" ; 32→"space" ; 127→"backspace"
  -10→"delete" ; -11→"insert" ; -14→"home" ; -15→"end" ; -12→"pageUp" ; -13→"pageDown"
  -1→"up" ; -2→"down" ; -4→"left" ; -3→"right"
  48..57→该数字 ; 97..122→该字母 ; SYMBOL_KEYS→该符号
无 keyName → undefined
return formatKeyNameWithModifiers(keyName, modifier)
```

`formatKeyNameWithModifiers(keyName, modifier)`：
```
eff = modifier & ~LOCK_MASK
supported = shift|ctrl|alt|super = 15
if ((eff & ~supported) !== 0) return undefined       // 未知修饰位 → 无法表示
按顺序压入 "shift","ctrl","alt","super"
return mods.length ? `${mods.join("+")}+${keyName}` : keyName
```
**修饰符顺序固定为 `shift+ctrl+alt+super+<key>`**（注意与 `MODIFIER_NAMES` = shift,alt,ctrl,super 不同）。
例：`ctrl+shift+p` 会格式化为 `shift+ctrl+p`。

`parseKey(data)` 主体顺序：
1. `parseKittySequence` 命中：取 `logicalShiftedSymbol = getLogicalShiftedSymbolCandidate(kitty.shiftedKey, kitty.modifier, SYMBOL_KEYS)`；
   `formatParsedKey(logical?.codepoint ?? kitty.codepoint, logical?.modifier ?? kitty.modifier, kitty.baseLayoutKey)`
2. `parseModifyOtherKeysSequence` 命中：`getLogicalShiftedSymbolCandidate(cp, mod, SHIFTED_SYMBOL_KEYS)`；同上（无 baseLayoutKey）
3. kitty 激活时：`data === "\x1b\r" || data === "\n"` → `"shift+enter"`
4. `LEGACY_SEQUENCE_KEY_IDS[data]` 命中 → 返回该 id
5. 逐条 **【逐字】**：
```
"\x1b"             → "escape"
"\x1c"             → "ctrl+\\"
"\x1d"             → "ctrl+]"
"\x1f"             → "ctrl+-"
"\x1b\x1b"         → "ctrl+alt+["
"\x1b\x1c"         → "ctrl+alt+\\"
"\x1b\x1d"         → "ctrl+alt+]"
"\x1b\x1f"         → "ctrl+alt+-"
"\t"               → "tab"
"\r" 或 (!kitty && "\n") 或 "\x1bOM" → "enter"
"\x00"             → "ctrl+space"
" "                → "space"
"\x7f"             → "backspace"
"\x08"             → isWindowsTerminalSession() ? "ctrl+backspace" : "backspace"
"\x1b[Z"           → "shift+tab"
!kitty && "\x1b\r" → "alt+enter"
!kitty && "\x1b "  → "alt+space"
"\x1b\x7f" 或 "\x1b\b" → "alt+backspace"
!kitty && "\x1bB"  → "alt+left"
!kitty && "\x1bF"  → "alt+right"
!kitty && len==2 && data[0]=="\x1b":
    code = data.charCodeAt(1)
    1..26       → `ctrl+alt+${String.fromCharCode(code+96)}`
    a-z/0-9/符号 → `alt+${char}`
"\x1b[A" → "up" ; "\x1b[B" → "down" ; "\x1b[C" → "right" ; "\x1b[D" → "left"
"\x1b[H" 或 "\x1bOH" → "home" ; "\x1b[F" 或 "\x1bOF" → "end"
"\x1b[3~" → "delete" ; "\x1b[5~" → "pageUp" ; "\x1b[6~" → "pageDown"
len==1 且 code 1..26   → `ctrl+${String.fromCharCode(code+96)}`
len==1 且 code 32..126 → data 原样
否则 undefined
```

#### 2.5.9 可打印字符解码

```ts
const KITTY_CSI_U_REGEX = /^\x1b\[(\d+)(?::(\d*))?(?::(\d+))?(?:;(\d+))?(?::(\d+))?u$/;
const KITTY_PRINTABLE_ALLOWED_MODIFIERS = MODIFIERS.shift | LOCK_MASK;   // 1 | 192 = 193

export function decodeKittyPrintable(data: string): string | undefined {
  match = KITTY_CSI_U_REGEX.exec(data);  无 → undefined
  codepoint = parseInt(g1);  非有限 → undefined
  shiftedKey = g2?.length ? parseInt(g2) : undefined
  modValue = g4 ? parseInt(g4) : 1 ;  modifier = modValue - 1
  if ((modifier & ~193) !== 0) return undefined
  if (modifier & (alt|ctrl)) return undefined
  effective = codepoint
  if (modifier & shift && shiftedKey !== undefined) effective = shiftedKey
  effective = normalizeKittyFunctionalCodepoint(effective)
  if (!finite || effective < 32) return undefined
  try { return String.fromCodePoint(effective) } catch { return undefined }
}

function decodeModifyOtherKeysPrintable(data): string | undefined {
  parsed = parseModifyOtherKeysSequence(data);  无 → undefined
  modifier = parsed.modifier & ~LOCK_MASK
  if ((modifier & ~MODIFIERS.shift) !== 0) return undefined
  if (!finite || parsed.codepoint < 32) return undefined
  try { return String.fromCodePoint(parsed.codepoint) } catch { return undefined }
}

export function decodePrintableKey(data: string): string | undefined {
  return decodeKittyPrintable(data) ?? decodeModifyOtherKeysPrintable(data);
}
```

测试（`keys.test.ts`，61 个 it，分 4 组）：`matchesKey`（47 = 16 + 13 + 18）、`decodeKittyPrintable`（1）、
`decodePrintableKey`（1）、`parseKey`（12 = 6 + 6）。**Rust 需逐条复刻**（见 §7）。

Rust 实现注意：
- `String.fromCharCode(cp)` 对 >0xFFFF 的 cp 会截断为 16 位（JS 语义）。`fromCharCode(normalized)`
  只用于 SYMBOL_KEYS 查表判等，normalized < 0x80 的实际取值下无差异；但 `String.fromCodePoint`
  在 `decodeKittyPrintable` 中对 >0x10FFFF 会抛异常 → 必须 `char::from_u32` 返回 None 等价。
- `parseKeyId` 的 lower-case 化：JS `toLowerCase()` 是 Unicode 全小写；key id 只含 ASCII，用
  `to_ascii_lowercase()` 即可（但为了逐字节一致，仍建议用 `to_lowercase()`——对 ASCII 结果相同）。
- 正则 `(?::(\d*))?` 允许空数字段（`:;`），`g2` 为 `""` 时视为 undefined。

---

### 2.6 `src/keybindings.ts`（320 行）

#### 2.6.1 `Keybindings` 接口（47 个 id，**全部**）

```ts
export interface Keybindings {
  // 编辑器导航与编辑（23）
  "tui.editor.cursorUp" / cursorDown / historyPrevious / historyNext / cursorLeft / cursorRight
  "tui.editor.cursorWordLeft" / cursorWordRight / cursorLineStart / cursorLineEnd
  "tui.editor.jumpForward" / jumpBackward / pageUp / pageDown
  "tui.editor.deleteCharBackward" / deleteCharForward / deleteWordBackward / deleteWordForward
  "tui.editor.deleteToLineStart" / deleteToLineEnd / yank / yankPop / undo
  // 通用输入（4）
  "tui.input.newLine" / submit / tab / copy
  // 通用选择（6）
  "tui.select.up" / down / pageUp / pageDown / confirm / cancel
  // 备用屏视口导航（14）
  "tui.altScreen.pageUp" / pageDown / halfPageUp / halfPageDown / lineUp / lineDown
  "tui.altScreen.previousPrompt" / nextPrompt / search / searchNext / searchPrevious / searchClose
  "tui.altScreen.top" / bottom
}
```
（注：上面把 23+4+6+14 = 47 项按路径缩写；源码中 `tui.editor.*` 实为 **23** 项
——`cursorUp, cursorDown, historyPrevious, historyNext, cursorLeft, cursorRight, cursorWordLeft,
cursorWordRight, cursorLineStart, cursorLineEnd, jumpForward, jumpBackward, pageUp, pageDown,
deleteCharBackward, deleteCharForward, deleteWordBackward, deleteWordForward, deleteToLineStart,
deleteToLineEnd, yank, yankPop, undo`；总数为 **47**，与源码逐项核对一致。）

#### 2.6.2 `TUI_KEYBINDINGS` 默认表（**逐字**，`as const satisfies KeybindingDefinitions`）

| id | defaultKeys | description |
|---|---|---|
| `tui.editor.cursorUp` | `"up"` | `Move cursor up` |
| `tui.editor.cursorDown` | `"down"` | `Move cursor down` |
| `tui.editor.historyPrevious` | `[]` | `Select previous prompt history entry` |
| `tui.editor.historyNext` | `[]` | `Select next prompt history entry` |
| `tui.editor.cursorLeft` | `["left","ctrl+b"]` | `Move cursor left` |
| `tui.editor.cursorRight` | `["right","ctrl+f"]` | `Move cursor right` |
| `tui.editor.cursorWordLeft` | `["alt+left","ctrl+left","alt+b"]` | `Move cursor word left` |
| `tui.editor.cursorWordRight` | `["alt+right","ctrl+right","alt+f"]` | `Move cursor word right` |
| `tui.editor.cursorLineStart` | `["home","ctrl+a"]` | `Move to line start` |
| `tui.editor.cursorLineEnd` | `["end","ctrl+e"]` | `Move to line end` |
| `tui.editor.jumpForward` | `"ctrl+]"` | `Jump forward to character` |
| `tui.editor.jumpBackward` | `"ctrl+alt+]"` | `Jump backward to character` |
| `tui.editor.pageUp` | `["pageUp","ctrl+pageUp"]` | `Page up` |
| `tui.editor.pageDown` | `["pageDown","ctrl+pageDown"]` | `Page down` |
| `tui.editor.deleteCharBackward` | `"backspace"` | `Delete character backward` |
| `tui.editor.deleteCharForward` | `["delete","ctrl+d"]` | `Delete character forward` |
| `tui.editor.deleteWordBackward` | `["ctrl+w","alt+backspace"]` | `Delete word backward` |
| `tui.editor.deleteWordForward` | `["alt+d","alt+delete"]` | `Delete word forward` |
| `tui.editor.deleteToLineStart` | `"ctrl+u"` | `Delete to line start` |
| `tui.editor.deleteToLineEnd` | `"ctrl+k"` | `Delete to line end` |
| `tui.editor.yank` | `"ctrl+y"` | `Yank` |
| `tui.editor.yankPop` | `"alt+y"` | `Yank pop` |
| `tui.editor.undo` | `"ctrl+-"` | `Undo` |
| `tui.input.newLine` | `["shift+enter","ctrl+j"]` | `Insert newline` |
| `tui.input.submit` | `"enter"` | `Submit input` |
| `tui.input.tab` | `"tab"` | `Tab / autocomplete` |
| `tui.input.copy` | `"ctrl+c"` | `Copy selection` |
| `tui.select.up` | `"up"` | `Move selection up` |
| `tui.select.down` | `"down"` | `Move selection down` |
| `tui.select.pageUp` | `"pageUp"` | `Selection page up` |
| `tui.select.pageDown` | `"pageDown"` | `Selection page down` |
| `tui.select.confirm` | `"enter"` | `Confirm selection` |
| `tui.select.cancel` | `["escape","ctrl+c"]` | `Cancel selection` |
| `tui.altScreen.pageUp` | `"pageUp"` | `Scroll viewport up one page` |
| `tui.altScreen.pageDown` | `"pageDown"` | `Scroll viewport down one page` |
| `tui.altScreen.halfPageUp` | `[]` | `Scroll viewport up half a page` |
| `tui.altScreen.halfPageDown` | `[]` | `Scroll viewport down half a page` |
| `tui.altScreen.lineUp` | `[]` | `Scroll viewport up one line` |
| `tui.altScreen.lineDown` | `[]` | `Scroll viewport down one line` |
| `tui.altScreen.previousPrompt` | `["ctrl+shift+up","ctrl+up"]` | `Jump to previous semantic prompt` |
| `tui.altScreen.nextPrompt` | `["ctrl+shift+down","ctrl+down"]` | `Jump to next semantic prompt` |
| `tui.altScreen.search` | `"ctrl+shift+f"` | `Search the primary scroll view` |
| `tui.altScreen.searchNext` | `["enter","ctrl+g"]` | `Select the next search match` |
| `tui.altScreen.searchPrevious` | `["shift+enter","ctrl+shift+g"]` | `Select the previous search match` |
| `tui.altScreen.searchClose` | `"escape"` | `Close transcript search` |
| `tui.altScreen.top` | `"ctrl+home"` | `Scroll viewport to top` |
| `tui.altScreen.bottom` | `"ctrl+end"` | `Scroll viewport to bottom` |

源码注释（**逐字**）：`// These intentionally shadow the unmodified editor bindings in fullscreen mode.`
（`tui.altScreen.pageUp/pageDown` 故意遮蔽编辑器的同名未修饰键。）

#### 2.6.3 `KeybindingsManager`

```ts
export interface KeybindingDefinition { defaultKeys: KeyId | KeyId[]; description?: string; }
export type KeybindingDefinitions = Record<string, KeybindingDefinition>;
export type KeybindingsConfig = Record<string, KeyId | KeyId[] | undefined>;
export interface KeybindingConflict { key: KeyId; keybindings: string[]; }

function normalizeKeys(keys): KeyId[] {
  undefined → []
  数组或单值 → 去重（保持首次出现顺序）
}
```

`rebuild()` **精确顺序**：
```
keysById.clear(); conflicts = []
userClaims = Map<KeyId, Set<Keybinding>>
for [keybinding, keys] of Object.entries(userBindings):
    if (!(keybinding in definitions)) continue          // 未知 id 忽略
    for key of normalizeKeys(keys): userClaims[key].add(keybinding)
for [key, keybindings] of userClaims:                   // Map 插入顺序
    if (keybindings.size > 1) conflicts.push({ key, keybindings: [...keybindings] })
for [id, definition] of Object.entries(definitions):     // 定义对象顺序
    userKeys = userBindings[id]
    keys = userKeys === undefined ? normalizeKeys(definition.defaultKeys) : normalizeKeys(userKeys)
    keysById.set(id, keys)
```
注意：`userKeys === undefined` 用默认值；显式 `[]` 表示"解绑"。冲突检测只看用户绑定之间的冲突
（用户绑定覆盖默认绑定后，与默认值的冲突不报）。

方法：
- `matches(data, keybinding)`：`keysById.get(id) ?? []`，任一 `matchesKey(data,key)` 命中即 true
- `getKeys(id)`：返回**副本**
- `getDefinition(id)`：原对象
- `getConflicts()`：深拷贝副本
- `setUserBindings(config)`：替换并 rebuild
- `getUserBindings()`：浅拷贝
- `getResolvedBindings()`：对每个定义 id，`keys.length === 1 ? keys[0] : [...keys]`（0 个时给 `[]`）

全局：
```ts
let globalKeybindings: KeybindingsManager | null = null;
export function setKeybindings(kb: KeybindingsManager): void;
export function getKeybindings(): KeybindingsManager;   // 懒初始化为 new KeybindingsManager(TUI_KEYBINDINGS)
```

测试（`keybindings.test.ts`，7 个 it）：默认解析、用户覆盖、显式空数组解绑、冲突检测与顺序、
未知 id 忽略、`getResolvedBindings` 形状、`matches` 走通 `matchesKey`。

Rust 设计：`KeybindingsManager { definitions: BTreeMap<String, KeybindingDefinition>, user: BTreeMap<String, Vec<KeyId>>, keys_by_id: HashMap<String, Vec<KeyId>>, conflicts: Vec<KeybindingConflict> }`。
**注意保持 JS 对象插入顺序**：定义表用 `Vec<(&'static str, KeybindingDefinition)>` 或 `IndexMap`，
否则 `getResolvedBindings()` 的键顺序与冲突列表顺序会不同。

---

### 2.7 `src/stdin-buffer.ts`（480 行）

#### 2.7.1 常量与类型

```ts
const ESC = "\x1b";
const DEFAULT_SEQUENCE_TIMEOUT_MS = 50;
const DEFAULT_ESCAPE_TIMEOUT_MS = 10;
const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END   = "\x1b[201~";
const TERMINAL_REPLY_HEADER_PATTERN = /^\x1b(?:\](?:1[01];|4;\d{1,3};|7501;\?)|P>\||_G(?:[a-z]=\d+,)*[a-z]=\d+;)/;
```
（该正则用于识别"这是终端回复"的头部：OSC 10/11、OSC 4;<idx>、OSC 7501;?、XTVersion `ESC P >|`、
Kitty graphics 回复 `ESC _ G...;`。）

```ts
export type StdinBufferOptions = { timeout?: number; escapeTimeout?: number };
export type StdinBufferEventMap = { data: [string]; paste: [string] };
export class StdinBuffer extends EventEmitter<StdinBufferEventMap> { ... }
```

`isControlString(data)`：`startsWith("\x1b]") || startsWith("\x1bP") || startsWith("\x1b_")`。

#### 2.7.2 序列完整性判定

`isCompleteSequence(data)` → `"complete" | "incomplete" | "not-escape"`：
```
不以 ESC 开头                       → "not-escape"
len === 1                          → "incomplete"
afterEsc = data.slice(1)
afterEsc 以 "[" 开头：
    afterEsc 以 "[M" 开头          → len >= 6 ? complete : incomplete      // 经典 X10 鼠标
    否则                           → isCompleteCsiSequence(data)
以 "]" 开头                        → isCompleteOscSequence(data)
以 "P" 开头                        → isCompleteDcsSequence(data)
以 "_" 开头                        → isCompleteApcSequence(data)
以 "O" 开头                        → afterEsc.length >= 2 ? complete : incomplete   // SS3
afterEsc.length === 1              → "complete"                           // Meta 键
其它                               → "complete"
```

`isCompleteCsiSequence(data)`：
```
不以 "\x1b[" 开头            → "complete"
len < 3                      → "incomplete"
payload = data.slice(2)
lastCharCode = payload 末字符码
0x40..0x7e 范围内：
    payload 以 "<" 开头：
        /^<\d+;\d+;\d+[Mm]$/ 匹配 payload → "complete"
        末字符是 "M" 或 "m"：
            payload.slice(1,-1).split(";") 长度 3 且每段 /^\d+$/ → "complete"
        否则 → "incomplete"
    否则 → "complete"
否则 → "incomplete"
```

`isCompleteOscSequence`：以 `\x1b]` 开头且（以 `\x1b\\` 或 `\x07` 结尾）→ complete，否则 incomplete。
`isCompleteDcsSequence`：以 `\x1bP` 开头且以 `\x1b\\` 结尾 → complete，否则 incomplete。
`isCompleteApcSequence`：以 `\x1b_` 开头且以 `\x1b\\` 结尾 → complete，否则 incomplete。

`parseUnmodifiedKittyPrintableCodepoint(sequence)`：
```js
/^\x1b\[(\d+)(?::\d*)?(?::\d+)?u$/     // 无修饰符形式
codepoint >= 32 ? codepoint : undefined
```

#### 2.7.3 `extractCompleteSequences(buffer)` — **核心切分算法**

```ts
while (pos < buffer.length) {
  remaining = buffer.slice(pos)
  if (remaining 以 ESC 开头) {
    terminalReply = isControlString(remaining) && TERMINAL_REPLY_HEADER_PATTERN.test(remaining)
    seqEnd = 1
    while (seqEnd <= remaining.length) {
      if (terminalReply && seqEnd > 2) {
        char = remaining[seqEnd - 1]
        if (char === "\x18" || char === "\x1a") { pos += seqEnd; break }          // CAN / SUB 取消字符串
        if (char === ESC && seqEnd < remaining.length && remaining[seqEnd] !== "\\") { pos += seqEnd - 1; break }
      }
      candidate = remaining.slice(0, seqEnd)
      status = isCompleteSequence(candidate)
      if (status === "complete") {
        // WezTerm 双 ESC 特例
        if (candidate === "\x1b\x1b") {
          nextChar = remaining[seqEnd]
          if (nextChar 是 "[" | "]" | "O" | "P" | "_") { sequences.push(ESC); pos += 1; break }
        }
        sequences.push(candidate); pos += seqEnd; break
      } else if (status === "incomplete") { seqEnd++ }
      else { sequences.push(candidate); pos += seqEnd; break }   // 理论上不可达
    }
    if (seqEnd > remaining.length) return { sequences, remainder: remaining }
  } else {
    sequences.push(remaining[0]); pos++
  }
}
return { sequences, remainder: "" }
```

要点：
- **CAN(0x18)/SUB(0x1A) 取消的只是这个终端回复字符串，后续键盘输入仍然有效**。
- 未终止的终端回复遇到新的 ESC 序列时中止（保留尾部 ESC 等待区分 ST）。
- `\x1b\x1b[` / `\x1b\x1b]` / `\x1b\x1bO` / `\x1b\x1bP` / `\x1b\x1b_` → 只输出第一个 ESC，从第二个重启。

#### 2.7.4 `StdinBuffer` 类

字段：`buffer=""`、`timeout: Timeout|null`、`timeoutMs`、`escapeTimeoutMs`、
`pasteMode=false`、`pasteBuffer=""`、`pendingKittyPrintableCodepoint: number|undefined`。

`process(data: string | Buffer)` **精确流程**：
```
1. 若有 timeout → clearTimeout, timeout=null
2. str 归一化：
   Buffer 且 length===1 且 data[0] > 127 → `\x1b${String.fromCharCode(data[0]-128)}`
   Buffer 其它 → data.toString()
   string → 原样
3. if (str.length === 0 && buffer.length === 0) { emitDataSequence(""); return }
4. buffer += str
5. if (pasteMode) {
     pasteBuffer += buffer; buffer = ""
     endIndex = pasteBuffer.indexOf("\x1b[201~")
     if (endIndex !== -1) {
        pastedContent = pasteBuffer.slice(0, endIndex)
        remaining = pasteBuffer.slice(endIndex + 6)
        pasteMode=false; pasteBuffer=""; pendingKittyPrintableCodepoint=undefined
        emit("paste", pastedContent)
        if (remaining.length > 0) process(remaining)
     }
     return
   }
6. startIndex = buffer.indexOf("\x1b[200~")
   if (startIndex !== -1) {
     if (startIndex > 0) { 对 buffer.slice(0,startIndex) 走 extractCompleteSequences 并逐条 emitDataSequence }
     pendingKittyPrintableCodepoint = undefined
     buffer = buffer.slice(startIndex + 6)
     pasteMode = true; pasteBuffer = buffer; buffer = ""
     endIndex = pasteBuffer.indexOf("\x1b[201~")
     if (endIndex !== -1) { 同 5 的收尾逻辑 }
     return
   }
7. result = extractCompleteSequences(buffer); buffer = result.remainder
   for (seq of result.sequences) emitDataSequence(seq)
8. if (buffer.length > 0) {
     if (TERMINAL_REPLY_HEADER_PATTERN.test(buffer)) return        // 等待终止符，不设超时
     timeoutMs = (buffer === ESC) ? escapeTimeoutMs : timeoutMs
     timeout = setTimeout(() => {
        flushed = flush()
        for (seq of flushed) {
          if (seq.length > 2 && isControlString(seq)) {
              emitDataSequence(seq.slice(0,2))     // 恢复 Alt 键
              process(seq.slice(2))                // 重新解析尾部
          } else emitDataSequence(seq)
        }
     }, timeoutMs)
   }
```

`emitDataSequence(sequence)`：
```
rawCodepoint = sequence.length === 1 ? sequence.codePointAt(0) : undefined
if (rawCodepoint !== undefined && rawCodepoint === pendingKittyPrintableCodepoint) {
    pendingKittyPrintableCodepoint = undefined
    return                                       // 去重：Kitty 可打印 codepoint 后的裸字符
}
pendingKittyPrintableCodepoint = parseUnmodifiedKittyPrintableCodepoint(sequence)
emit("data", sequence)
```
（去重动机：Kitty 协议下终端会先发 CSI-u 再发裸字符，二者只应产生一次输入。）

公开方法：
- `flush(): string[]`：清 timer；buffer 为空 → `[]`；否则 `[buffer]`，清空 buffer 与 pending
- `clear(): void`：清 timer、buffer、pasteMode、pasteBuffer、pending
- `getBuffer(): string`
- `destroy(): void` → `clear()`

测试（`stdin-buffer.test.ts`，66 个 it）：见 §7.4（分组：单字符/转义序列分片/OSC/DCS/APC/
括号粘贴/超时/Kitty 去重/CAN-SUB/双 ESC/粘贴重入）。

Rust 设计：`StdinBuffer { buffer: String, timeout: Option<Instant+deadline>, ... }`，
用事件循环注册定时器而非 `setTimeout`；对外暴露 `process(&mut self, data: &[u8]) -> Vec<StdinEvent>`，
事件枚举 `Data(String) | Paste(String)`。**Buffer 输入的高字节转换**需要原始的字节，
因此 `process` 必须接受字节切片（或区分 `&str`/`&[u8]` 两种入口）。

---

### 2.8 `src/terminal.ts`（630 行）

#### 2.8.1 常量（**逐字**）

```ts
const TERMINAL_PROGRESS_KEEPALIVE_MS = 1000;
const TERMINAL_PROGRESS_ACTIVE_SEQUENCE = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE  = "\x1b]9;4;0\x07";
const NATIVE_SHIFT_ENTER_SEQUENCE = "\x1b[13;2u";
const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS = 7;          // 1|2|4
const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS = 150;
const KITTY_KEYBOARD_PROTOCOL_QUERY = `\x1b[>${7}u\x1b[?u`;   // = "\x1b[>7u\x1b[?u"
const DEVICE_ATTRIBUTES_QUERY = "\x1b[c";
const DEFAULT_ESCAPE_TIMEOUT_MS = 10;
const DEFAULT_SSH_ESCAPE_TIMEOUT_MS = 100;
```

#### 2.8.2 顶层函数

```ts
export type KeyboardProtocolNegotiationSequence =
  | { type: "kitty-flags"; flags: number }
  | { type: "device-attributes" };

export function parseKeyboardProtocolNegotiationSequence(seq: string): KeyboardProtocolNegotiationSequence | undefined {
  const kittyFlags = seq.match(/^\x1b\[\?(\d+)u$/);
  if (kittyFlags) return { type: "kitty-flags", flags: Number.parseInt(kittyFlags[1], 10) };
  if (/^\x1b\[\?[\d;]*c$/.test(seq)) return { type: "device-attributes" };
  return undefined;
}

function isKeyboardProtocolNegotiationSequencePrefix(seq: string): boolean {
  return seq === "\x1b[" || /^\x1b\[\?[\d;]*$/.test(seq);
}

export function isAppleTerminalSession(): boolean {
  return process.platform === "darwin" && process.env.TERM_PROGRAM === "Apple_Terminal";
}

export function refreshTerminalDimensions(): void {
  if (process.platform === "win32" || process.pid <= 0) return;
  try { process.kill(process.pid, "SIGWINCH"); } catch { /* 忽略 EACCES 等 */ }
}

export function normalizeNativeShiftEnterInput(data: string, shouldDetect: boolean, isShiftPressed: boolean): string {
  if (shouldDetect && data === "\r" && isShiftPressed) return "\x1b[13;2u";
  return data;
}

export function normalizeAppleTerminalInput(data: string, isAppleTerminal: boolean, isShiftPressed: boolean): string {
  return normalizeNativeShiftEnterInput(data, isAppleTerminal, isShiftPressed);
}

export function resolveEscapeTimeoutMs(env: NodeJS.ProcessEnv = process.env): number {
  const configured = Number(env.KIANA_TUI_ESC_TIMEOUT);
  if (Number.isFinite(configured) && configured > 0) return configured;
  if (env.SSH_CONNECTION || env.SSH_TTY) return 100;
  return 10;
}
```
（`Number("")` 为 0 → 不生效；`Number(undefined)` 为 NaN → 不生效；`Number("abc")` 为 NaN。）

#### 2.8.3 `Terminal` 接口（**逐字**）

```ts
export interface Terminal {
  start(onInput: (data: string) => void, onResize: () => void): void;
  stop(): void;
  restoreOnExit?(before: string, after: string): void;
  drainInput(maxMs?: number, idleMs?: number): Promise<void>;
  write(data: string): void;
  get columns(): number;
  get rows(): number;
  get kittyProtocolActive(): boolean;
  moveBy(lines: number): void;
  hideCursor(): void;
  showCursor(): void;
  clearLine(): void;
  clearFromCursor(): void;
  clearScreen(): void;
  setTitle(title: string): void;
  setProgress(active: boolean): void;
  setProgramStatus(status: ProgramStatus): void;
}
```

#### 2.8.4 `ProcessTerminal`

字段：
```
wasRaw, inputHandler, resizeHandler, _kittyProtocolActive, _modifyOtherKeysActive,
keyboardProtocolPushed, pendingKeyboardProtocolDeviceAttributes (number),
keyboardProtocolNegotiationBuffer (string), keyboardProtocolBufferFlushTimer,
stdinBuffer?, stdinDataHandler?, progressInterval?,
programStatus?, programStatusSupported (bool), programStatusQueryPending (bool),
writeLogPath: string（构造时一次性求值）
```

`writeLogPath` 求值逻辑（构造时）：
```
env = process.env.KIANA_TUI_WRITE_LOG || "";  空 → ""
try { if (fs.statSync(env).isDirectory()) {
        ts = `${YYYY}-${MM}-${DD}_${HH}-${mm}-${ss}`  (本地时间，各段 padStart(2,"0"))
        return path.join(env, `tui-${ts}-${process.pid}.log`)
      } } catch {}
return env
```

`start(onInput, onResize)` **顺序**：
```
1. 保存 inputHandler / resizeHandler
2. wasRaw = process.stdin.isRaw || false
3. process.stdin.setRawMode?.(true)
4. process.stdin.setEncoding("utf8"); process.stdin.resume()
5. process.stdout.write("\x1b[?2004h")           // 括号粘贴
6. process.stdout.on("resize", resizeHandler)
7. refreshTerminalDimensions()
8. enableWindowsVTInput()                        // 必须在 setRawMode 之后
9. queryAndEnableKittyProtocol()
```

`setupStdinBuffer()`：
- `new StdinBuffer({ escapeTimeout: resolveEscapeTimeoutMs() })`
- `on("data", seq => …)` 顺序：
  1. `isProgramStatusReply(seq)` → 若 `programStatusQueryPending`：置 false、`programStatusSupported = true`、
     `writeProgramStatus()`；**始终 return**（不转发）
  2. `negotiation = readKeyboardProtocolNegotiationSequence(seq)`
     - `"pending"` → `scheduleKeyboardProtocolNegotiationBufferFlush()`，return
     - `negotiation && handleKeyboardProtocolNegotiationSequence(negotiation.parsed)` → return
  3. `forwardInputSequence(negotiation?.sequence ?? seq)`
- `on("paste", content => inputHandler?.(`\x1b[200~${content}\x1b[201~`))`
- `stdinDataHandler = data => stdinBuffer.process(data)`

`queryAndEnableKittyProtocol()`：
```
setupStdinBuffer(); process.stdin.on("data", stdinDataHandler)
keyboardProtocolPushed = true
pendingKeyboardProtocolDeviceAttributes += 1
clearKeyboardProtocolNegotiationBuffer()
override = process.env.KIANA_PROGRAM_STATUS
programStatusSupported = (override === "1")
programStatusQueryPending = (override !== "1" && override !== "0")
query = programStatusQueryPending ? PROGRAM_STATUS_QUERY : ""
process.stdout.write(`${KITTY_KEYBOARD_PROTOCOL_QUERY}${query}${DEVICE_ATTRIBUTES_QUERY}`)
writeProgramStatus()
```

`handleKeyboardProtocolNegotiationSequence(n)` **返回 boolean（是否已消费）**：
```
clearKeyboardProtocolNegotiationBuffer()
if (type === "device-attributes") {
   if (pendingKeyboardProtocolDeviceAttributes === 0) return false
   pendingKeyboardProtocolDeviceAttributes -= 1
   if (pendingKeyboardProtocolDeviceAttributes === 0) programStatusQueryPending = false
}
if (type === "kitty-flags") {
   if (flags !== 0) { disableModifyOtherKeys(); if (!_kittyProtocolActive) { _kittyProtocolActive = true; setKittyProtocolActive(true) } }
   else enableModifyOtherKeys()
   return true
}
if (!_kittyProtocolActive) enableModifyOtherKeys()
return true
```

`readKeyboardProtocolNegotiationSequence(seq)` → `{parsed, sequence} | "pending" | undefined`：
```
if (keyboardProtocolNegotiationBuffer) {
   buffered = buffer + seq
   parsed = parse(buffered); if (parsed) { clear(); return {parsed, sequence: buffered} }
   if (isPrefix(buffered)) { setBuffer(buffered); return "pending" }
   flushKeyboardProtocolNegotiationBufferAsInput()
}
parsed = parse(seq); if (parsed) return {parsed, sequence: seq}
if (isPrefix(seq)) { setBuffer(seq); return "pending" }
return undefined
```

缓冲区辅助：`setBuffer`（清 flush timer 后赋值）、`clearBuffer`（清 timer + 置空）、
`flushBufferAsInput`（若非空 → 清空并 `forwardInputSequence`）、
`scheduleFlush`（无 buffer 或已有 timer → 返回；否则 150ms 后清 timer 并 flush）、
`clearFlushTimer`。

`forwardInputSequence(seq)`：
```
shouldDetect = seq === "\r" && (isAppleTerminalSession() || platform === "win32")
input = normalizeNativeShiftEnterInput(seq, shouldDetect, shouldDetect && isNativeModifierPressed("shift"))
inputHandler(input)
```

`enableModifyOtherKeys()`：若 kitty 已激活或已启用 → return；否则写 `\x1b[>4;2m`，置位。
`disableModifyOtherKeys()`：未启用 → return；否则写 `\x1b[>4;0m`，清位。
`enableWindowsVTInput()`：非 win32 → return；try `getNativePlatformHelper()?.enableVirtualTerminalInput?.()` catch 忽略。

`drainInput(maxMs = 1000, idleMs = 50)` **异步**：
```
shouldDisable = keyboardProtocolPushed || _kittyProtocolActive
clearKeyboardProtocolNegotiationBuffer()
if (shouldDisable) { write("\x1b[<u"); keyboardProtocolPushed = false; _kittyProtocolActive = false; setKittyProtocolActive(false) }
disableModifyOtherKeys()
previousHandler = inputHandler; inputHandler = undefined
lastDataTime = Date.now(); onData = () => lastDataTime = Date.now()
process.stdin.on("data", onData)
endTime = Date.now() + maxMs
try { while (true) {
        now = Date.now(); timeLeft = endTime - now
        if (timeLeft <= 0) break
        if (now - lastDataTime >= idleMs) break
        await new Promise(r => setTimeout(r, Math.min(idleMs, timeLeft)))
      } }
finally { process.stdin.removeListener("data", onData); inputHandler = previousHandler }
```

`stop()` **顺序**（**逐字**）：
```
1. if (clearProgressInterval()) write(TERMINAL_PROGRESS_CLEAR_SEQUENCE)
2. if (programStatusSupported && programStatus) write(formatProgramStatus({state:"clear"}))
3. programStatusSupported = false; programStatusQueryPending = false
4. write("\x1b[?2004l")                                    // 关闭括号粘贴
5. shouldDisable = keyboardProtocolPushed || _kittyProtocolActive
6. clearKeyboardProtocolNegotiationBuffer()
7. if (shouldDisable) { write("\x1b[<u"); keyboardProtocolPushed=false; _kittyProtocolActive=false; setKittyProtocolActive(false) }
8. disableModifyOtherKeys()
9. stdinBuffer?.destroy(); stdinBuffer = undefined
10. removeListener("data", stdinDataHandler); stdinDataHandler = undefined
11. inputHandler = undefined
12. removeListener("resize", resizeHandler); resizeHandler = undefined
13. process.stdin.pause()
14. process.stdin.setRawMode?.(wasRaw)
```

`restoreOnExit(before, after)` **同步**：
```
seq = before
if (progressInterval) seq += TERMINAL_PROGRESS_CLEAR_SEQUENCE
seq += "\x1b[?2004l"
if (keyboardProtocolPushed || _kittyProtocolActive) seq += "\x1b[<u"
if (_modifyOtherKeysActive) seq += "\x1b[>4;0m"
seq += after
try { fs.writeSync(process.stdout.fd, seq) } catch {}
```

`write(data)`：`process.stdout.write(data)`；若 `writeLogPath` 非空则
`fs.appendFileSync(path, data, {encoding:"utf8"})`（错误忽略）。

尺寸：`columns = process.stdout.columns || Number(process.env.COLUMNS) || 80`；
`rows = process.stdout.rows || Number(process.env.LINES) || 24`。

光标/清屏（**逐字**）：
```
moveBy(n)      n>0 → `\x1b[${n}B` ; n<0 → `\x1b[${-n}A` ; 0 → 无输出
hideCursor()   "\x1b[?25l"
showCursor()   "\x1b[?25h"
clearLine()    "\x1b[K"
clearFromCursor() "\x1b[J"
clearScreen()  "\x1b[2J\x1b[H"
setTitle(t)    `\x1b]0;${t}\x07`
```

`setProgramStatus(status)`：`programStatus = status.state === "clear" ? undefined : status`；
若 `programStatusSupported` → `write(formatProgramStatus(status))`。
`writeProgramStatus()`（私有）：`programStatusSupported && programStatus` → 写当前状态。

`setProgress(active)`：
```
true  → write("\x1b]9;4;3\x07")；若 progressInterval 不存在 → setInterval(每 1000ms 重发，不 unref)
false → clearProgressInterval(); write("\x1b]9;4;0\x07")
```
`clearProgressInterval()`：有则 `clearInterval`、置 undefined、返回 true；否则 false。

测试（`terminal.test.ts`，27 个 it；6 组）：`resolveEscapeTimeoutMs`（4）、
`normalizeNativeShiftEnterInput`（4）、`normalizeAppleTerminalInput`（4）、
`ProcessTerminal Kitty keyboard protocol negotiation`（7）、`program status (OSC 7501)`（6）、
`ProcessTerminal progress`（1）、`ProcessTerminal dimensions`（1）。详见 §7.10。

Rust 设计：
- `trait Terminal`（`&self` 方法 + `&mut self` 的生命周期方法分开；`start`/`stop` 用 `&mut self`）。
- `ProcessTerminal` 需 `crossterm`/`termios` 的 raw mode + `std::io::stdin()` 读取线程 + `signal_hook` 的 SIGWINCH。
- `restoreOnExit` 用 `libc::write(1, ...)` 保证同步。
- `drainInput` 用 `tokio::time::timeout` 或阻塞 sleep；语义为"最多 maxMs，且连续 idleMs 无输入即退出"。

---

### 2.9 `src/tui.ts`（1543 行）

#### 2.9.1 鼠标类型

```ts
export type TuiMouseEventType = "press" | "release" | "move" | "drag" | "click" | "wheel";
export type TuiMouseButton = "left" | "middle" | "right" | "none";

export interface TuiMouseEvent {
  type: TuiMouseEventType;
  button: TuiMouseButton;
  x: number; y: number;              // 组件局部坐标（0 基）
  screenX: number; screenY: number;  // 绝对终端坐标
  width: number; height: number;     // 当前组件边界
  shift: boolean; alt: boolean; ctrl: boolean;
  wheelDelta?: number;               // 逻辑行数，负数向上
  clickCount?: number;               // 仅 type === "click"
}

export interface TuiMouseEventResult {
  handled?: boolean;   // 停止传播并抑制渲染器级回退
  capture?: boolean;   // 后续 drag/release 路由到本组件；隐含 handled
  focus?: boolean;     // 请求键盘焦点；隐含 handled
  render?: boolean;    // press/click/drag/wheel 默认 true；move/release 默认 false
}

export interface TuiMouseDispatchTarget {
  component: Component; originX: number; originY: number; width: number; height: number;
}

export interface TuiMouseDispatchResult extends TuiMouseEventResult {
  handled: true; target: TuiMouseDispatchTarget; focusTarget?: Component;
}
```

`dispatchMouseEvent(component, event)` **精确**：
```
result = component.handleMouse?.(event)
if (!result) return undefined
if ("target" in result) {                     // 组件已转发给子组件
    forwarded = result as TuiMouseDispatchResult
    return forwarded.focus && component.handleInput ? { ...forwarded, focusTarget: component } : forwarded
}
if (!result.handled && !result.capture && !result.focus) return undefined
return { ...result, handled: true,
         ...(result.focus ? { focusTarget: component } : {}),
         target: { component, originX: event.screenX - event.x, originY: event.screenY - event.y,
                   width: event.width, height: event.height } }
```

`retargetMouseEvent(event, target)`：
```
{ ...event, x: event.screenX - target.originX, y: event.screenY - target.originY,
  width: target.width, height: target.height }
```

#### 2.9.2 `Component` / `Focusable` / 光标标记

```ts
export interface Component {
  render(width: number): string[];
  handleInput?(data: string): void;
  handleMouse?(event: TuiMouseEvent): TuiMouseEventResult | undefined;
  wantsKeyRelease?: boolean;      // 默认 false，释放事件被过滤
  invalidate(): void;
}
export type TuiInputListenerResult = { consume?: boolean; data?: string } | undefined;
export type TuiInputListener = (data: string) => TuiInputListenerResult;

export interface Focusable { focused: boolean; }
export function isFocusable(component: Component | null): component is Component & Focusable {
  return component !== null && "focused" in component;
}
```

**【逐字】光标标记**
```ts
export const CURSOR_MARKER     = "\x1b_kiana:c\x07";
export const FAKE_CURSOR_START = "\x1b_kiana:fc\x07";
export const FAKE_CURSOR_END   = "\x1b_kiana:/fc\x07";
const FOCUSED_FAKE_CURSOR = /(\x1b_kiana:c\x07)\x1b_kiana:fc\x07(.*?)(?:\x1b_kiana:\/fc\x07|$)/s;
export function renderFakeCursor(text: string): string {
  return `${FAKE_CURSOR_START}${text}${FAKE_CURSOR_END}`;
}
```
注意 `FOCUSED_FAKE_CURSOR` 的 `/s` 标志（`.` 匹配换行——虽然逐行处理时行内无换行）。

#### 2.9.3 覆盖层类型

```ts
export type OverlayAnchor = "center" | "top-left" | "top-right" | "bottom-left" | "bottom-right"
                          | "top-center" | "bottom-center" | "left-center" | "right-center";
export interface OverlayMargin { top?: number; right?: number; bottom?: number; left?: number; }
export type SizeValue = number | `${number}%`;

function parseSizeValue(value: SizeValue | undefined, referenceSize: number): number | undefined {
  if (value === undefined) return undefined;
  if (typeof value === "number") return value;
  const match = value.match(/^(\d+(?:\.\d+)?)%$/);
  if (match) return Math.floor((referenceSize * parseFloat(match[1])) / 100);
  return undefined;
}

export interface OverlayOptions {
  width?: SizeValue; minWidth?: number; maxHeight?: SizeValue;
  anchor?: OverlayAnchor; offsetX?: number; offsetY?: number;
  row?: SizeValue; col?: SizeValue;
  margin?: OverlayMargin | number;
  visible?: (termWidth: number, termHeight: number) => boolean;
  nonCapturing?: boolean;
}
export interface OverlayUnfocusOptions { target: Component | null; }
export interface OverlayBounds { row: number; col: number; width: number; height: number; }
export interface OverlayHandle {
  hide(): void; setHidden(hidden: boolean): void; isHidden(): boolean;
  focus(): void; unfocus(options?: OverlayUnfocusOptions): void;
  isFocused(): boolean; getBounds(): OverlayBounds | undefined;
}
```
内部类型（不导出）：
```ts
type OverlayStackEntry = { component; options?; preFocus: Component|null; hidden: boolean;
                           focusOrder: number; bounds?: OverlayBounds };
type RenderedOverlayLayout = { entry; row; col; width; height };
type OverlayBlockedFocusResume = { status:"restore-overlay" } | { status:"focus-target"; target: Component|null };
type OverlayFocusRestoreState = { status:"inactive" }
  | { status:"eligible"; overlay: OverlayStackEntry }
  | { status:"blocked"; overlay: OverlayStackEntry; blockedBy: Component; resume: OverlayBlockedFocusResume };
type OverlayFocusRestorePolicy = "clear" | "preserve";
```

#### 2.9.4 `Container`

```ts
export class Container implements Component {
  children: Component[] = [];
  private mouseLayout?: { width: number; children: Array<{component: Component; height: number}> };
  addChild(c) / removeChild(c)（indexOf + splice）/ clear()（新数组）
  invalidate()            // 递归 child.invalidate?.()
  handleMouse(event): TuiMouseDispatchResult | undefined   // 见下
  render(width): string[] // 拼接所有子组件，并记录 mouseLayout
}
```

`Container.render(width)`：逐 child `render(width)`，push 所有行，并记录
`mouseLayout = { width, children: [{component, height: lines.length}] }`。

`Container.handleMouse(event)` **精确**：
```
if (event.y < 0 || event.y >= event.height) return undefined
mouseChildren = (mouseLayout?.width === event.width)
                  ? mouseLayout.children
                  : children.map(c => ({component: c, height: c.render(event.width).length}))
childY = 0
for ({component: child, height: childHeight} of mouseChildren) {
    if (event.y >= childY && event.y < childY + childHeight) {
        result = dispatchMouseEvent(child, { ...event, y: event.y - childY, height: childHeight })
        if (result?.focus && this.handleInput) return { ...result, focusTarget: this }
        return result
    }
    childY += childHeight
}
return undefined
```

#### 2.9.5 `compositeTuiLine`（**【逐字】输出关键**）

```ts
const SEGMENT_RESET = "\x1b[0m\x1b]8;;\x07";

export function compositeTuiLine(baseLine, overlayLine, startCol, overlayWidth, totalWidth): string {
  if (isImageLine(baseLine)) return baseLine;

  const afterStart = startCol + overlayWidth;
  const base = extractSegments(baseLine, startCol, afterStart, totalWidth - afterStart, true);
  const overlay = sliceWithWidth(overlayLine, 0, overlayWidth, true);
  const beforePad = Math.max(0, startCol - base.beforeWidth);
  const overlayPad = Math.max(0, overlayWidth - overlay.width);
  const actualBeforeWidth = Math.max(startCol, base.beforeWidth);
  const actualOverlayWidth = Math.max(overlayWidth, overlay.width);
  const afterTarget = Math.max(0, totalWidth - actualBeforeWidth - actualOverlayWidth);
  const afterPad = Math.max(0, afterTarget - base.afterWidth);
  const result = base.before + " ".repeat(beforePad) + SEGMENT_RESET
               + overlay.text + " ".repeat(overlayPad) + SEGMENT_RESET
               + base.after + " ".repeat(afterPad);
  return visibleWidth(result) <= totalWidth ? result : sliceByColumn(result, 0, totalWidth, true);
}
```
（`extractSegments(..., strictAfter=true)`：after 部分继承 before 末尾的活跃样式。）

#### 2.9.6 `TUI` / `ViewportTUI` 接口

```ts
export type TuiMode = "regular" | "fullscreen";
export interface TuiStopOptions {
  /** Leave renderer output in place for another TUI taking over the same terminal. */
  preserveScreen?: boolean;
}

export interface TUI extends Component {
  readonly mode: TuiMode;
  children: Component[];
  terminal: Terminal;
  onDebug?: () => void;
  readonly fullRedraws: number;
  addChild(component: Component): void;
  removeChild(component: Component): void;
  clear(): void;
  getShowHardwareCursor(): boolean;
  setShowHardwareCursor(enabled: boolean): void;
  getClearOnShrink(): boolean;
  setClearOnShrink(enabled: boolean): void;
  setFocus(component: Component | null): void;
  showOverlay(component: Component, options?: OverlayOptions): OverlayHandle;
  hideOverlay(): void;
  hasOverlay(): boolean;
  start(): void;
  stop(options?: TuiStopOptions): void;
  renderNow(force?: boolean): void;
  requestRender(force?: boolean): void;
  addInputListener(listener: TuiInputListener): () => void;
  removeInputListener(listener: TuiInputListener): void;
  onTerminalColorSchemeChange(listener: (scheme: TerminalColorScheme) => void): () => void;
  setTerminalColorSchemeNotifications(enabled: boolean): void;
  queryTerminalColors(options: { timeoutMs: number; onLateReply?: (colors: TerminalColors) => void }): Promise<TerminalColors>;
}

export const VIEWPORT_TUI = Symbol.for("@earendil-works/kiana-tui/viewport");
export interface ViewportTUI extends TUI {
  readonly [VIEWPORT_TUI]: true;
  setLayoutRoot(component: Component | undefined): void;
}
export function isViewportTUI(tui: TUI): tui is ViewportTUI {
  return (tui as Partial<ViewportTUI>)[VIEWPORT_TUI] === true;
}
```

#### 2.9.7 `TuiBase` 字段与生命周期

```ts
export abstract class TuiBase extends Container implements TUI {
  abstract readonly mode: TuiMode;
  public terminal: Terminal;
  private focusedComponent: Component | null = null;
  private inputListeners = new Set<TuiInputListener>();
  public onDebug?: () => void;
  private renderRequested = false;
  private immediateRenderScheduled = false;
  private renderTimer: NodeJS.Timeout | undefined;
  private lastRenderAt = 0;
  private static readonly MIN_RENDER_INTERVAL_MS = 16;
  private showHardwareCursor = false;
  private clearOnShrink = false;
  protected fullRedrawCount = 0;
  protected stopped = false;
  private pendingTerminalColorQueries: PendingTerminalColorQuery[] = [];
  private terminalColorSchemeListeners = new Set<(scheme) => void>();
  private terminalColorSchemeNotificationsEnabled = false;
  protected readonly logDirectory: string | undefined;
  private focusOrderCounter = 0;
  private overlayStack: OverlayStackEntry[] = [];
  private renderedOverlayLayouts: RenderedOverlayLayout[] = [];
  private overlayFocusRestore: OverlayFocusRestoreState = { status: "inactive" };
  // 注意字段声明顺序：源码里 `get hasOverlayEntries()` 与 `get fullRedraws()` 夹在字段之间
  get hasOverlayEntries(): boolean          // public，见 §2.9.7
  get fullRedraws(): number                 // public，返回 fullRedrawCount
  constructor(terminal: Terminal, showHardwareCursor?: boolean, logDirectory?: string)
  protected abstract doRender(): void;
  // 其余 protected 钩子（源码中紧跟 doRender 声明）：resetRenderState / beforeTerminalStart /
  // afterTerminalStart / beforeTerminalStop / afterTerminalStop / getExitRestorePrefix /
  // getExitRestoreSuffix / getMountedRoots —— 见下方钩子清单
  private readonly exitRestoreHandler: () => void   // 构造时绑定的箭头函数字段
}
```

私有常量：
```ts
const TERMINAL_PALETTE_SIZE = 16;
const TERMINAL_COLOR_REPLY_COUNT = 2 + TERMINAL_PALETTE_SIZE;    // 18
const TERMINAL_COLOR_QUERY =
  "\x1b]10;?\x07\x1b]11;?\x07"
  + "\x1b]4;0;?\x07\x1b]4;1;?\x07…\x1b]4;15;?\x07"                // 16 条，索引 0..15
  + "\x1b[c";
const DEVICE_ATTRIBUTES_RESPONSE_PATTERN = /^\x1b\[\?[\d;]*c$/;
```

`PendingTerminalColorQuery`：
```ts
{ foreground?: RgbColor; background?: RgbColor; palette: Array<RgbColor|undefined>;
  replied: Set<string>; deliver?: (colors: TerminalColors) => void; timer: Timeout|undefined }
```

钩子（全部可覆盖，默认空实现或固定值）：
```ts
protected resetRenderState(): void {}
protected beforeTerminalStart(): void {}
protected afterTerminalStart(): void {}
protected beforeTerminalStop(_options: TuiStopOptions): void {}
protected afterTerminalStop(_options: TuiStopOptions): void {}
protected getExitRestorePrefix(): string {
  return `${this.terminalColorSchemeNotificationsEnabled ? "\x1b[?2031l" : ""}\x1b[?25h`;
}
protected getExitRestoreSuffix(): string { return ""; }
protected getMountedRoots(): readonly Component[] { return this.children; }
```

退出处理器（构造时绑定的箭头函数字段）：
```ts
private readonly exitRestoreHandler = (): void => {
  if (this.stopped) return;
  try { this.terminal.restoreOnExit?.(this.getExitRestorePrefix(), this.getExitRestoreSuffix()); } catch {}
};
```

`start()`：
```
stopped = false
process.off("exit", exitRestoreHandler); process.on("exit", exitRestoreHandler)
beforeTerminalStart()
terminal.start(data => handleTerminalInput(data), () => requestRender())
afterTerminalStart()
terminal.hideCursor()
if (terminalColorSchemeNotificationsEnabled) terminal.write("\x1b[?2031h")
queryCellSize()
requestRender()
```

`stop(options = {})`：
```
stopped = true
process.off("exit", exitRestoreHandler)
cancelRenderTimer()
if (terminalColorSchemeNotificationsEnabled) terminal.write("\x1b[?2031l")
beforeTerminalStop(options)
terminal.showCursor()
terminal.stop()
afterTerminalStop(options)
```

`queryCellSize()`（私有）：`if (!getCapabilities().images) return;` 否则 `terminal.write("\x1b[16t")`
（CSI 16 t = 请求 cell 像素尺寸；回复格式 `CSI 6 ; height ; width t`）。

`setShowHardwareCursor(enabled)`：相同值 → return；否则赋值，`!enabled` 时 `hideTerminalCursor()`，
再 `requestRender()`。
`setClearOnShrink(enabled)`：直接赋值（**不**触发渲染）。
`getFocusedComponent()`：返回 `focusedComponent`。

`hideTerminalCursor()`（私有）：`if (!this.stopped) this.terminal.hideCursor()`。

`invalidate()`：对 `getMountedRoots()` 每个 root 调 `invalidate()`，再对 `overlayStack` 每项调 `invalidate()`。

`hasOverlay()`：`overlayStack.some(o => isOverlayVisible(o))`。
`get hasOverlayEntries()`（**public** getter，`get hasOverlayEntries(): boolean`）：`overlayStack.length > 0`。
注意两者语义不同：`hasOverlay()` 只统计**可见** overlay，`hasOverlayEntries` 统计**全部** entry（含被隐藏的）。
`TuiBase` 还有 public `get fullRedraws(): number`（累计全量重绘次数，供外部断言/调试）。

#### 2.9.8 焦点状态机（**最容易出错**）

```ts
setFocus(component) { this.setFocusInternal({ component, overlayFocusRestore: "clear" }); }

private setFocusInternal({ component, overlayFocusRestore }): void {
  const previousFocus = this.focusedComponent;
  let nextFocus = component;
  const previousFocusedOverlay = previousFocus
    ? this.overlayStack.find(e => e.component === previousFocus && this.isOverlayVisible(e)) : undefined;
  const nextFocusIsOverlay = nextFocus
    ? this.overlayStack.some(e => e.component === nextFocus) : false;
  const restoreState = this.getVisibleOverlayFocusRestore();

  if (nextFocus && !nextFocusIsOverlay) {
    if (restoreState.status === "blocked" && restoreState.blockedBy === previousFocus) {
      if (restoreState.resume.status === "focus-target" || !this.isComponentMounted(restoreState.blockedBy)) {
        nextFocus = this.resolveBlockedOverlayFocusResume(restoreState);
      } else {
        this.overlayFocusRestore = { status:"blocked", overlay: restoreState.overlay,
                                     blockedBy: nextFocus, resume: restoreState.resume };
      }
    } else if (previousFocusedOverlay && restoreState.status !== "inactive"
               && restoreState.overlay === previousFocusedOverlay
               && !this.isOverlayFocusAncestor(previousFocusedOverlay, nextFocus)) {
      this.overlayFocusRestore = { status:"blocked", overlay: previousFocusedOverlay,
                                   blockedBy: nextFocus, resume: { status:"restore-overlay" } };
    }
  } else if (nextFocus === null) {
    if (restoreState.status === "blocked" && restoreState.blockedBy === previousFocus) {
      nextFocus = this.resolveBlockedOverlayFocusResume(restoreState);
    } else if (overlayFocusRestore === "clear") {
      this.clearOverlayFocusRestore();
    }
  }

  if (isFocusable(this.focusedComponent)) this.focusedComponent.focused = false;
  this.focusedComponent = nextFocus;
  if (isFocusable(nextFocus)) nextFocus.focused = true;

  const focusedOverlay = nextFocus
    ? this.overlayStack.find(e => e.component === nextFocus && this.isOverlayVisible(e)) : undefined;
  if (focusedOverlay) this.overlayFocusRestore = { status:"eligible", overlay: focusedOverlay };
}
```

辅助：
- `clearOverlayFocusRestore()` → `{status:"inactive"}`
- `clearOverlayFocusRestoreFor(overlay)`：仅当 `overlayFocusRestore.overlay === overlay` 时清
- `resolveBlockedOverlayFocusResume(state)`：`"restore-overlay"` → `state.overlay.component`；
  `"focus-target"` → 先 `clearOverlayFocusRestore()` 再返回 `state.resume.target`
- `getVisibleOverlayFocusRestore()`：`inactive` 原样返回；overlay 已不在栈中或不可见 → `{status:"inactive"}`；
  否则原样
- `isOverlayFocusAncestor(entry, component)`：沿 `preFocus` 链上溯（`Set` 防环），
  链中任一等于 component → true。链式查找：`overlayStack.find(o => o.component === current)?.preFocus ?? null`
- `retargetOverlayPreFocus(removed)`：所有其它 overlay 若 `preFocus === removed.component` → 改为 `removed.preFocus`
- `isComponentMounted(component)`：`getMountedRoots().some(child => containsComponent(child, component))`
- `containsComponent(root, target)`：`root === target` → true；`root instanceof Container` 时递归
  `root.children`；否则 false

#### 2.9.9 `showOverlay` / `hideOverlay`

`showOverlay(component, options?)`：
```
entry = { component, ...(options===undefined?{}:{options}), preFocus: focusedComponent,
          hidden: false, focusOrder: ++focusOrderCounter }
overlayStack.push(entry)
if (!options?.nonCapturing && isOverlayVisible(entry)) setFocus(component)
hideTerminalCursor()
requestRender()
return handle
```

Handle 方法 **逐字逻辑**：

- `hide()`：
  ```
  index = overlayStack.indexOf(entry); if (index === -1) return
  clearOverlayFocusRestoreFor(entry); retargetOverlayPreFocus(entry)
  overlayStack.splice(index, 1)
  if (focusedComponent === component) {
      topVisible = getTopmostVisibleOverlay()
      setFocus(topVisible?.component ?? entry.preFocus)
  }
  if (overlayStack.length === 0) hideTerminalCursor()
  requestRender()
  ```
- `setHidden(hidden)`：
  ```
  if (entry.hidden === hidden) return
  entry.hidden = hidden
  if (hidden) {
      clearOverlayFocusRestoreFor(entry)
      if (focusedComponent === component) {
          topVisible = getTopmostVisibleOverlay()
          setFocus(topVisible?.component ?? entry.preFocus)
      }
  } else {
      if (!options?.nonCapturing && isOverlayVisible(entry)) {
          entry.focusOrder = ++focusOrderCounter
          setFocus(component)
      }
  }
  requestRender()
  ```
- `isHidden()` → `entry.hidden`
- `focus()`：不在栈中或不可见 → return；`entry.focusOrder = ++focusOrderCounter`；`setFocus(component)`；`requestRender()`
- `unfocus(unfocusOptions?)`：
  ```
  isFocused = focusedComponent === component
  restoreState = overlayFocusRestore
  hasPendingRestore = restoreState.status !== "inactive" && restoreState.overlay === entry
  if (!isFocused && !hasPendingRestore) return
  if (restoreState.status === "blocked" && restoreState.overlay === entry
      && focusedComponent === restoreState.blockedBy) {
      if (unfocusOptions) {
          overlayFocusRestore = { status:"blocked", overlay: entry, blockedBy: restoreState.blockedBy,
                                  resume: { status:"focus-target", target: unfocusOptions.target } }
      } else clearOverlayFocusRestore()
      requestRender(); return
  }
  clearOverlayFocusRestoreFor(entry)
  if (isFocused || unfocusOptions) {
      topVisible = getTopmostVisibleOverlay()
      fallbackTarget = (topVisible && topVisible !== entry) ? topVisible.component : entry.preFocus
      setFocus(unfocusOptions ? unfocusOptions.target : fallbackTarget)
  }
  requestRender()
  ```
- `isFocused()` → `focusedComponent === component`
- `getBounds()`：不在栈中 / 不可见 / 无 bounds → undefined；否则返回**副本**

`hideOverlay()`：取栈顶；无 → return；`clearOverlayFocusRestoreFor`、`retargetOverlayPreFocus`、`pop`；
若 `focusedComponent === overlay.component` → `setFocus(topVisible?.component ?? overlay.preFocus)`；
栈空 → `hideTerminalCursor()`；`requestRender()`。

`isOverlayVisible(entry)`（私有）：
```
if (entry.hidden) return false
if (entry.options?.visible) return entry.options.visible(terminal.columns, terminal.rows)
return true
```

`getTopmostVisibleOverlay()`：遍历，跳过 `options?.nonCapturing` 或不可见；`focusOrder` 更大者胜。

#### 2.9.10 渲染调度

```ts
renderNow(force = false) {
  if (force) this.resetRenderState();
  this.renderRequested = false;
  this.cancelRenderTimer();
  this.lastRenderAt = performance.now();
  this.doRender();
}

requestRender(force = false) {
  if (force) { this.resetRenderState(); this.requestImmediateRender(); return; }
  if (this.renderRequested) return;
  this.renderRequested = true;
  process.nextTick(() => this.scheduleRender());
}

private requestImmediateRender() {
  this.cancelRenderTimer();
  this.renderRequested = true;
  if (this.immediateRenderScheduled) return;
  this.immediateRenderScheduled = true;
  process.nextTick(() => {
    this.immediateRenderScheduled = false;
    if (this.stopped || !this.renderRequested) return;
    this.cancelRenderTimer();
    this.renderRequested = false;
    this.lastRenderAt = performance.now();
    this.doRender();
  });
}

private scheduleRender() {
  if (this.stopped || this.renderTimer || !this.renderRequested) return;
  const elapsed = performance.now() - this.lastRenderAt;
  const delay = Math.max(0, 16 - elapsed);
  this.renderTimer = setTimeout(() => {
    this.renderTimer = undefined;
    if (this.stopped || !this.renderRequested) return;
    this.renderRequested = false;
    this.lastRenderAt = performance.now();
    this.doRender();
    if (this.renderRequested) this.scheduleRender();
  }, delay);
}
```
`cancelRenderTimer()`：有 timer 则 `clearTimeout` 并置 undefined。

#### 2.9.11 输入处理 `handleTerminalInput(data)` — **顺序不可变**

```
1. consumeTerminalColorResponse(data) → return
2. consumeTerminalColorSchemeReport(data) → return
3. if (inputListeners.size > 0) {
     let current = data
     for (listener of inputListeners) {        // Set 插入顺序
        r = listener(current)
        if (r?.consume) return
        if (r?.data !== undefined) current = r.data
     }
     if (current.length === 0) return
     data = current
   }
4. consumeCellSizeResponse(data) → return
5. if (matchesKey(data, "shift+ctrl+d") && onDebug) { onDebug(); return }
6. focusedOverlay = overlayStack.find(o => o.component === focusedComponent)
   if (focusedOverlay && !isOverlayVisible(focusedOverlay)) {
      topVisible = getTopmostVisibleOverlay()
      if (topVisible) setFocus(topVisible.component)
      else setFocusInternal({ component: focusedOverlay.preFocus, overlayFocusRestore: "preserve" })
   }
7. focusIsOverlay = overlayStack.some(o => o.component === focusedComponent)
   if (!focusIsOverlay) {
      restoreState = getVisibleOverlayFocusRestore()
      if (restoreState.status === "eligible") setFocus(restoreState.overlay.component)
      else if (restoreState.status === "blocked" && restoreState.blockedBy !== focusedComponent) {
         if (restoreState.resume.status === "restore-overlay") setFocus(restoreState.overlay.component)
         else { clearOverlayFocusRestore(); setFocus(restoreState.resume.target) }
      }
   }
8. if (focusedComponent?.handleInput) {
      if (isKeyRelease(data) && !focusedComponent.wantsKeyRelease) return
      focusedComponent.handleInput(data)
      requestImmediateRender()
   }
```

`consumeTerminalColorResponse(data)`：
```
query = pendingTerminalColorQueries[0]; 无 → false
if (DEVICE_ATTRIBUTES_RESPONSE_PATTERN.test(data)) {
    shift(); completeTerminalColorQuery(query); return true
}
response = parseOscColorResponse(data); 无 → false
{target, rgb} = response; key = String(target)
if (!query.deliver || query.replied.has(key)) return true
query.replied.add(key)
if (target === "foreground") query.foreground = rgb
else if (target === "background") query.background = rgb
else if (target < 16) query.palette[target] = rgb
if (query.replied.size === 18) completeTerminalColorQuery(query)
return true
```

`terminalColorQueryResult(query)`：
```
palette = query.palette.every(c => c !== undefined) ? query.palette as RgbColor[] : undefined
return { foreground: query.foreground, background: query.background, palette }
```

`completeTerminalColorQuery(query)`：`deliver = query.deliver; query.deliver = undefined; clearTimeout(timer); deliver?.(result)`。

`consumeTerminalColorSchemeReport(data)`：`parseTerminalColorSchemeReport` 命中则通知所有监听器并返回 true。

`consumeCellSizeResponse(data)`：
```js
const match = data.match(/^\x1b\[6;(\d+);(\d+)t$/);  无 → false
heightPx = parseInt(match[1]); widthPx = parseInt(match[2])
if (heightPx <= 0 || widthPx <= 0) return true        // 消费但忽略
setCellDimensions({ widthPx, heightPx })
this.invalidate(); this.requestRender(); return true
```

`queryTerminalColors({timeoutMs, onLateReply})`：
```
new Promise(resolve => {
  query = { palette: Array(16).fill(undefined), replied: new Set(), deliver: resolve, timer: undefined }
  query.timer = setTimeout(() => { query.deliver = onLateReply; resolve(result(query)) }, timeoutMs)
  pendingTerminalColorQueries.push(query)
  terminal.write(TERMINAL_COLOR_QUERY)
})
```

#### 2.9.12 覆盖层布局与合成

`resolveOverlayLayout(options, overlayHeight, termWidth, termHeight)` → `{width,row,col,maxHeight}` **精确**：
```
opt = options ?? {}
margin = typeof opt.margin === "number"
           ? {top:m,right:m,bottom:m,left:m} : (opt.margin ?? {})
marginTop = max(0, margin.top ?? 0); marginRight/Bottom/Left 同理
availWidth  = max(1, termWidth  - marginLeft - marginRight)
availHeight = max(1, termHeight - marginTop  - marginBottom)

width = parseSizeValue(opt.width, termWidth) ?? min(80, availWidth)
if (opt.minWidth !== undefined) width = max(width, opt.minWidth)
width = max(1, min(width, availWidth))

maxHeight = parseSizeValue(opt.maxHeight, termHeight)
if (maxHeight !== undefined) maxHeight = max(1, min(maxHeight, availHeight))

effectiveHeight = maxHeight !== undefined ? min(overlayHeight, maxHeight) : overlayHeight

// row
if (opt.row !== undefined) {
   if (typeof opt.row === "string") {
      m = opt.row.match(/^(\d+(?:\.\d+)?)%$/)
      if (m) { maxRow = max(0, availHeight - effectiveHeight); row = marginTop + floor(maxRow * parseFloat(m[1])/100) }
      else row = resolveAnchorRow("center", effectiveHeight, availHeight, marginTop)
   } else row = opt.row
} else row = resolveAnchorRow(opt.anchor ?? "center", effectiveHeight, availHeight, marginTop)

// col 同构（用 availWidth、width、marginLeft）
// row 百分比非法时回退 center；col 同

if (opt.offsetY !== undefined) row += opt.offsetY
if (opt.offsetX !== undefined) col += opt.offsetX

row = max(marginTop,  min(row, termHeight - marginBottom - effectiveHeight))
col = max(marginLeft, min(col, termWidth  - marginRight  - width))
```

`resolveAnchorRow(anchor, height, availHeight, marginTop)`：
```
top-left | top-center | top-right        → marginTop
bottom-left | bottom-center | bottom-right → marginTop + availHeight - height
left-center | center | right-center      → marginTop + floor((availHeight - height)/2)
```

`resolveAnchorCol(anchor, width, availWidth, marginLeft)`：
```
top-left | left-center | bottom-left     → marginLeft
top-right | right-center | bottom-right  → marginLeft + availWidth - width
top-center | center | bottom-center      → marginLeft + floor((availWidth - width)/2)
```

`compositeOverlays(lines, termWidth, termHeight)` **精确**：
```
if (overlayStack.length === 0) { renderedOverlayLayouts = []; return lines }
result = [...lines]
for (entry of overlayStack) entry.bounds = undefined

rendered = []; minLinesNeeded = result.length
visibleEntries = overlayStack.filter(isOverlayVisible).sort((a,b) => a.focusOrder - b.focusOrder)
for (entry of visibleEntries) {
    { width, maxHeight } = resolveOverlayLayout(entry.options, 0, termWidth, termHeight)
    overlayLines = resolveFakeCursors(entry.component.render(width))
    if (maxHeight !== undefined && overlayLines.length > maxHeight) overlayLines = overlayLines.slice(0, maxHeight)
    { row, col } = resolveOverlayLayout(entry.options, overlayLines.length, termWidth, termHeight)
    entry.bounds = { row, col, width, height: overlayLines.length }
    rendered.push({ entry, overlayLines, row, col, w: width })
    minLinesNeeded = max(minLinesNeeded, row + overlayLines.length)
}
renderedOverlayLayouts = rendered.map(({entry,row,col,w,overlayLines}) =>
    ({ entry, row, col, width: w, height: overlayLines.length }))

workingHeight = max(result.length, termHeight, minLinesNeeded)
while (result.length < workingHeight) result.push("")
viewportStart = max(0, workingHeight - termHeight)

for ({overlayLines, row, col, w} of rendered)
  for (i = 0; i < overlayLines.length; i++) {
     idx = viewportStart + row + i
     if (idx >= 0 && idx < result.length) {
        truncated = visibleWidth(overlayLines[i]) > w ? sliceByColumn(overlayLines[i], 0, w, true) : overlayLines[i]
        result[idx] = compositeTuiLine(result[idx], truncated, col, w, termWidth)
     }
  }
return result
```

`applyLineResets(lines)`：逐行，非图片行 → `normalizeTerminalOutput(line) + SEGMENT_RESET`。

`extractCursorPosition(lines, height)`：
```
viewportTop = max(0, lines.length - height)
for (row = lines.length-1; row >= viewportTop; row--) {
    markerIndex = line.indexOf(CURSOR_MARKER)
    if (markerIndex !== -1) {
        col = visibleWidth(line.slice(0, markerIndex))
        lines[row] = line.slice(0,markerIndex) + line.slice(markerIndex + CURSOR_MARKER.length)
        return { row, col }
    }
}
return null
```

`resolveFakeCursors(lines)`：
```
lines.map(line => {
   if (!line.includes(FAKE_CURSOR_START)) return line
   resolved = showHardwareCursor ? line.replace(FOCUSED_FAKE_CURSOR, "$1$2") : line
   return resolved.replaceAll(FAKE_CURSOR_START, "\x1b[7m").replaceAll(FAKE_CURSOR_END, "\x1b[27m")
})
```

#### 2.9.13 鼠标分发辅助

`isOverlayFocused()`（protected）：`overlayStack.some(e => e.component === focusedComponent && isOverlayVisible(e))`。

`resolveMouseFocusTarget(component)`（protected）：
```
for (i = overlayStack.length-1; i >= 0; i--)
   if (isOverlayVisible(o) && containsComponent(o.component, component)) return o.component
return component
```

`dispatchMouseToOverlay(event)`（protected）→ `{hit: boolean, result?: TuiMouseDispatchResult}`：
```
for (i = renderedOverlayLayouts.length-1; i >= 0; i--) {
   l = renderedOverlayLayouts[i]
   if (event.screenX < l.col || event.screenX >= l.col + l.width
       || event.screenY < l.row || event.screenY >= l.row + l.height) continue
   result = dispatchMouseEvent(l.entry.component, { ...event, x: event.screenX - l.col,
              y: event.screenY - l.row, width: l.width, height: l.height })
   return result ? { hit: true, result: result.focus ? { ...result, focusTarget: l.entry.component } : result }
                 : { hit: true }
}
return { hit: false }
```

测试覆盖：`overlay-options.test.ts`（26）、`overlay-non-capturing.test.ts`（44）、
`overlay-short-content.test.ts`（1）、`tui-overlay-style-leak.test.ts`（2）、
`regression-overlay-cjk-boundary.test.ts`（4）。详见 §7.5–§7.6；
`tui-cell-size-input.test.ts`（2）覆盖 `\x1b[16t` 回复消费，见 §7.16。

---

### 2.10 `src/tui-main-screen.ts`（656 行）

```ts
const KITTY_SEQUENCE_PREFIX = "\x1b_G";
const MAX_RENDER_WRITE_CHARS = 1024 * 1024;      // 1 MiB（UTF-16 code unit 计数）
```

#### 2.10.1 `BoundedTerminalWriter`（**逐字**）

```ts
class BoundedTerminalWriter {
  private buffer = "";
  private writtenChars = 0;
  constructor(private readonly write: (data: string) => void)
  append(value: string): void {
    let offset = 0;
    while (offset < value.length) {
      const capacity = MAX_RENDER_WRITE_CHARS - this.buffer.length;
      if (capacity === 0) { this.flush(); continue; }
      let end = Math.min(value.length, offset + capacity);
      if (end < value.length
          && value.charCodeAt(end-1) >= 0xd800 && value.charCodeAt(end-1) <= 0xdbff
          && value.charCodeAt(end)   >= 0xdc00 && value.charCodeAt(end)   <= 0xdfff) end--;
      if (end === offset) { this.flush(); continue; }
      this.buffer += value.slice(offset, end);
      offset = end;
      if (this.buffer.length === MAX_RENDER_WRITE_CHARS) this.flush();
    }
  }
  flush(): void { if (!this.buffer) return; this.write(this.buffer); this.writtenChars += this.buffer.length; this.buffer = ""; }
  get length(): number { return this.writtenChars + this.buffer.length; }
}
```
目的：单次 `write` 不超过 1 MiB，且**不拆开 UTF-16 代理对**。Rust 中字符串是 UTF-8，
必须按 **char 边界**切分（绝不切 UTF-8 半个序列）。

**必须按 UTF-16 code unit 计数**，理由是两个测试都直接断言 JS 字符串长度：
- `splits a large full render without changing its output`：
  `assert.ok(terminal.writes.every(w => w.length <= MAX_RENDER_WRITE_CHARS))`，
  且 `terminal.writes.join("")` 必须**逐字**等于 `` `\x1b[?2026h${kittyLine}\r\n${kittyLine}\x1b[?2026l` ``；
  数据是 1.2M 个 `A` 的 Kitty 行 ×2，断言 `writes.length > 2`。
- `splits large differential updates without a full redraw`：同样的长度上限，输出以 `\x1b[?2026h` 开头、
  `\x1b[?2026l` 结尾，且**不含** `\x1b[2J`（必须走差分路径）。

若用字节计数，含多字节字符（CJK/emoji）的渲染会在块边界处与 JS 不同——虽然拼接结果相同，
`every(w => w.length <= MAX)` 仍会通过，但 `write` 次数（`writes.length > 2`）可能不同。
**Rust 侧推荐**：内部统计"UTF-16 code unit 数"（对 `String` 用 `chars().map(|c| c.len_utf16()).sum()`
或维护计数器），切分点落在 char 边界且累计 UTF-16 长度恰好不超过上限。

#### 2.10.2 Kitty 图片头解析

```ts
interface KittyImageHeader { ids: number[]; rows: number; }

function parseKittyImageHeader(line: string): KittyImageHeader | undefined {
  const sequenceStart = line.indexOf("\x1b_G");
  if (sequenceStart === -1) return undefined;
  const paramsStart = sequenceStart + 3;
  const paramsEnd = line.indexOf(";", paramsStart);
  if (paramsEnd === -1) return undefined;
  const ids: number[] = []; let rows = 1;
  for (const param of line.slice(paramsStart, paramsEnd).split(",")) {
    const [key, value] = param.split("=", 2);
    if (value === undefined) continue;
    const numberValue = Number(value);
    if (!Number.isInteger(numberValue) || numberValue <= 0 || numberValue > 0xffffffff) continue;
    if (key === "i") ids.push(numberValue); else if (key === "r") rows = numberValue;
  }
  return { ids, rows };
}
function extractKittyImageIds(line) { return parseKittyImageHeader(line)?.ids ?? []; }
function extractKittyImageRows(line) { return parseKittyImageHeader(line)?.rows ?? 1; }
function isTermuxSession(): boolean { return Boolean(process.env.TERMUX_VERSION); }
```

`Number(value)` 语义：`""`→0（被过滤）、`"1e3"`→1000（**接受**）、`"abc"`→NaN（过滤）。
`Number.isInteger` 排除小数。Rust 实现必须模拟"JS Number 解析"（至少：允许前导/尾随空白、
十进制、指数、`Infinity` 被 `isInteger` 排除）。

#### 2.10.3 状态结构与快照

```ts
export interface TuiMainScreenRenderState {
  previousLines: string[]; previousWidth: number; previousHeight: number;
  cursorRow: number; hardwareCursorRow: number; maxLinesRendered: number; previousViewportTop: number;
}

export class TuiMainScreen extends TuiBase implements TUI {
  readonly mode = "regular" as const;
  private previousLines: string[] = [];
  private previousKittyImageIds = new Set<number>();
  private previousWidth = 0; private previousHeight = 0;
  private cursorRow = 0; private hardwareCursorRow = 0;
  private maxLinesRendered = 0; private previousViewportTop = 0;
}
```

`captureRenderState()`：返回上述 7 个字段的副本（`previousLines` 浅拷贝数组）。
`restoreRenderState(state)`：`previousLines = state.previousLines.map(l => isImageLine(l) ? "" : l)`；
`previousKittyImageIds = new Set()`；其余字段直接赋值。
`resetRenderState()`（override）：`previousLines=[]`、`previousWidth=-1`、`previousHeight=-1`、
`cursorRow=0`、`hardwareCursorRow=0`、`maxLinesRendered=0`、`previousViewportTop=0`。

`beforeTerminalStop(options)`（override）：
```
if (options.preserveScreen || previousLines.length === 0) return
terminal.write(" ")
targetRow = previousLines.length
lineDiff = targetRow - hardwareCursorRow
if (lineDiff > 0) write(`\x1b[${lineDiff}B`) else if (lineDiff < 0) write(`\x1b[${-lineDiff}A`)
write("\r\n")
```

#### 2.10.4 Kitty 行辅助

- `collectKittyImageIds(lines)`：遍历所有行，收集 `extractKittyImageIds` 的 id 到 Set
- `deleteKittyImages(ids)`：拼接每个 `deleteKittyImage(id)`
- `getKittyImageReservedRows(lines, index, maxIndex = lines.length - 1)`：
  ```
  rows = extractKittyImageRows(lines[index] ?? "")
  if (rows <= 1) return 1
  maxRows = min(rows, maxIndex - index + 1, lines.length - index)
  reservedRows = 1
  while (reservedRows < maxRows) {
     line = lines[index + reservedRows] ?? ""
     if (isImageLine(line) || visibleWidth(line) > 0) break
     reservedRows++
  }
  return reservedRows
  ```
- `expandChangedRangeForKittyImages(firstChanged, lastChanged, newLines)`：
  ```
  expandForLines(lines): for i in 0..lines.length:
      if (extractKittyImageIds(lines[i]).length === 0) continue
      blockEnd = i + getKittyImageReservedRows(lines, i) - 1
      if (i >= firstChanged || (i <= lastChanged && blockEnd >= firstChanged)) {
          expandedFirstChanged = min(expandedFirstChanged, i)
          expandedLastChanged  = max(expandedLastChanged, blockEnd)
      }
  先 expandForLines(previousLines)，再 expandForLines(newLines)
  ```
- `deleteChangedKittyImages(firstChanged, lastChanged)`：越界 → `""`；收集
  `[firstChanged, min(lastChanged, previousLines.length-1)]` 区间内所有图片 id 的删除序列。

#### 2.10.5 `doRender()` — 逐句算法（**验收核心**）

```
if (stopped) return
width  = terminal.columns
height = terminal.rows
widthChanged  = previousWidth  !== 0 && previousWidth  !== width
heightChanged = previousHeight !== 0 && previousHeight !== height
previousBufferLength = previousHeight > 0 ? previousViewportTop + previousHeight : height
prevViewportTop = heightChanged ? max(0, previousBufferLength - height) : previousViewportTop
viewportTop = prevViewportTop
hardwareCursorRow = this.hardwareCursorRow        // 局部变量
computeLineDiff(targetRow) = (targetRow - viewportTop) - (hardwareCursorRow - prevViewportTop)

newLines = resolveFakeCursors(render(width))
if (hasOverlayEntries) newLines = compositeOverlays(newLines, width, height)
cursorPos = extractCursorPosition(newLines, height)
newLines = applyLineResets(newLines)
```

`fullRender(clear)` 内部函数（**逐字输出顺序**）：
```
this.fullRedrawCount += 1
output = new BoundedTerminalWriter(terminal.write)
output.append("\x1b[?2026h")
if (clear) { output.append(deleteKittyImages(previousKittyImageIds)); output.append("\x1b[2J\x1b[H\x1b[3J") }
for (i = 0; i < newLines.length; i++) {
    if (i > 0) output.append("\r\n")
    line = newLines[i]
    isImage = isImageLine(line)
    imageReservedRows = isImage ? getKittyImageReservedRows(newLines, i) : 1
    if (imageReservedRows > 1 && imageReservedRows <= height) {
        for (row = 1; row < imageReservedRows; row++) output.append("\r\n")
        output.append(`\x1b[${imageReservedRows - 1}A`)
        output.append(line)
        output.append(`\x1b[${imageReservedRows - 1}B`)
        i += imageReservedRows - 1
        continue
    }
    output.append(line)
}
output.append("\x1b[?2026l")
output.flush()
cursorRow = max(0, newLines.length - 1)
hardwareCursorRow = cursorRow
if (clear) maxLinesRendered = newLines.length
else       maxLinesRendered = max(maxLinesRendered, newLines.length)
bufferLength = max(height, newLines.length)
previousViewportTop = max(0, bufferLength - height)
positionHardwareCursor(cursorPos, newLines.length)
previousLines = newLines
previousKittyImageIds = collectKittyImageIds(newLines)
previousWidth = width; previousHeight = height
```

`logRedraw(reason)`（仅当 `KIANA_TUI_DEBUG_REDRAW === "1"` 且 `logDirectory !== undefined`）：
```
logPath = path.join(logDirectory, "kiana-tui-debug.log")
msg = `[${new Date().toISOString()}] fullRender: ${reason} (prev=${previousLines.length}, new=${newLines.length}, height=${height})\n`
fs.mkdirSync(path.dirname(logPath), { recursive: true })
fs.appendFileSync(logPath, msg)
```

**分支顺序（不可变）**：

| # | 条件 | 动作 | logRedraw reason |
|---|---|---|---|
| 1 | `previousLines.length === 0 && !widthChanged && !heightChanged` | `fullRender(false)`; return | `first render` |
| 2 | `widthChanged` | `fullRender(true)`; return | `terminal width changed (${previousWidth} -> ${width})` |
| 3 | `heightChanged && !isTermuxSession()` | `fullRender(true)`; return | `terminal height changed (${previousHeight} -> ${height})` |
| 4 | `getClearOnShrink() && newLines.length < maxLinesRendered && !hasOverlayEntries` | `fullRender(true)`; return | `clearOnShrink (maxLinesRendered=${maxLinesRendered})` |

差分计算：
```
firstChanged = -1; lastChanged = -1
maxLines = max(newLines.length, previousLines.length)
for (i = 0; i < maxLines; i++) {
   oldLine = i < previousLines.length ? previousLines[i] : ""
   newLine = i < newLines.length ? newLines[i] : ""
   if (oldLine !== newLine) { if (firstChanged === -1) firstChanged = i; lastChanged = i }
}
appendedLines = newLines.length > previousLines.length
if (appendedLines) { if (firstChanged === -1) firstChanged = previousLines.length; lastChanged = newLines.length - 1 }
if (firstChanged !== -1) { ({firstChanged, lastChanged} = expandChangedRangeForKittyImages(firstChanged, lastChanged, newLines)) }
appendStart = appendedLines && firstChanged === previousLines.length && firstChanged > 0
```

无变化分支：
```
if (firstChanged === -1) {
   positionHardwareCursor(cursorPos, newLines.length)
   previousViewportTop = prevViewportTop
   previousHeight = height
   return
}
```

全删除分支（`firstChanged >= newLines.length`）：
```
if (previousLines.length > newLines.length) {
   output = new BoundedTerminalWriter(terminal.write)
   output.append("\x1b[?2026h")
   output.append(deleteChangedKittyImages(firstChanged, lastChanged))
   targetRow = max(0, newLines.length - 1)
   if (targetRow < prevViewportTop) { logRedraw(`deleted lines moved viewport up (${targetRow} < ${prevViewportTop})`); fullRender(true); return }
   lineDiff = computeLineDiff(targetRow)
   if (lineDiff > 0) output.append(`\x1b[${lineDiff}B`) else if (lineDiff < 0) output.append(`\x1b[${-lineDiff}A`)
   output.append("\r")
   extraLines = previousLines.length - newLines.length
   if (extraLines > height) { logRedraw(`extraLines > height (${extraLines} > ${height})`); fullRender(true); return }
   clearStartOffset = newLines.length === 0 ? 0 : 1
   if (extraLines > 0 && clearStartOffset > 0) output.append(`\x1b[${clearStartOffset}B`)
   for (i = 0; i < extraLines; i++) { output.append("\r\x1b[2K"); if (i < extraLines - 1) output.append("\x1b[1B") }
   moveBack = max(0, extraLines - 1 + clearStartOffset)
   if (moveBack > 0) output.append(`\x1b[${moveBack}A`)
   output.append("\x1b[?2026l"); output.flush()
   cursorRow = targetRow; hardwareCursorRow = targetRow
}
positionHardwareCursor(cursorPos, newLines.length)
previousLines = newLines; previousKittyImageIds = collectKittyImageIds(newLines)
previousWidth = width; previousHeight = height; previousViewportTop = prevViewportTop
return
```

`firstChanged < prevViewportTop` 分支：
```
logRedraw(`firstChanged < viewportTop (${firstChanged} < ${prevViewportTop})`); fullRender(true); return
```

正常差分渲染（**逐字输出顺序**）：
```
output = new BoundedTerminalWriter(terminal.write)
output.append("\x1b[?2026h")
output.append(deleteChangedKittyImages(firstChanged, lastChanged))
prevViewportBottom = prevViewportTop + height - 1
moveTargetRow = appendStart ? firstChanged - 1 : firstChanged
if (moveTargetRow > prevViewportBottom) {
   currentScreenRow = max(0, min(height - 1, hardwareCursorRow - prevViewportTop))
   moveToBottom = height - 1 - currentScreenRow
   if (moveToBottom > 0) output.append(`\x1b[${moveToBottom}B`)
   scroll = moveTargetRow - prevViewportBottom
   output.append("\r\n".repeat(scroll))
   prevViewportTop += scroll; viewportTop += scroll; hardwareCursorRow = moveTargetRow
}
lineDiff = computeLineDiff(moveTargetRow)          // 用更新后的 prevViewportTop/viewportTop/hardwareCursorRow
if (lineDiff > 0) output.append(`\x1b[${lineDiff}B`) else if (lineDiff < 0) output.append(`\x1b[${-lineDiff}A`)
output.append(appendStart ? "\r\n" : "\r")
renderEnd = min(lastChanged, newLines.length - 1)
for (i = firstChanged; i <= renderEnd; i++) {
   if (i > firstChanged) output.append("\r\n")
   line = newLines[i]
   isImage = isImageLine(line)
   imageReservedRows = isImage ? getKittyImageReservedRows(newLines, i, renderEnd) : 1
   if (imageReservedRows > 1) {
      imageStartScreenRow = i - viewportTop
      if (imageStartScreenRow < 0 || imageStartScreenRow + imageReservedRows > height) {
          logRedraw(`kitty image pre-clear would scroll (${imageStartScreenRow} + ${imageReservedRows} > ${height})`)
          fullRender(true); return
      }
      output.append("\x1b[2K")
      for (row = 1; row < imageReservedRows; row++) output.append("\r\n\x1b[2K")
      output.append(`\x1b[${imageReservedRows - 1}A`)
      output.append(line)
      output.append(`\x1b[${imageReservedRows - 1}B`)
      i += imageReservedRows - 1
      continue
   }
   output.append("\x1b[2K")
   if (!isImage && visibleWidth(line) > width) { /* 见下 crash 分支 */ }
   output.append(line)
}
finalCursorRow = renderEnd
if (previousLines.length > newLines.length) {
   if (renderEnd < newLines.length - 1) { moveDown = newLines.length - 1 - renderEnd; output.append(`\x1b[${moveDown}B`); finalCursorRow = newLines.length - 1 }
   extraLines = previousLines.length - newLines.length
   for (i = newLines.length; i < previousLines.length; i++) output.append("\r\n\x1b[2K")
   output.append(`\x1b[${extraLines}A`)
}
output.append("\x1b[?2026l")
[KIANA_TUI_DEBUG === "1" 时写 /tmp/tui/render-<Date.now()>-<random>.log，内容见 §4.3]
output.flush()
cursorRow = max(0, newLines.length - 1)
hardwareCursorRow = finalCursorRow
maxLinesRendered = max(maxLinesRendered, newLines.length)
previousViewportTop = max(prevViewportTop, finalCursorRow - height + 1)
positionHardwareCursor(cursorPos, newLines.length)
previousLines = newLines; previousKittyImageIds = collectKittyImageIds(newLines)
previousWidth = width; previousHeight = height
```

**超宽 crash 分支**（`!isImage && visibleWidth(line) > width`）：
```
crashLogPath = path.join(logDirectory ?? os.tmpdir(), "kiana-tui-crash.log")
crashData = [
  `Crash at ${new Date().toISOString()}`,
  `Terminal width: ${width}`,
  `Line ${i} visible width: ${visibleWidth(line)}`,
  "",
  "=== All rendered lines ===",
  ...newLines.map((l, idx) => `[${idx}] (w=${visibleWidth(l)}) ${l}`),
  "",
].join("\n")
fs.mkdirSync(path.dirname(crashLogPath), { recursive: true })
fs.writeFileSync(crashLogPath, crashData)
this.stop()
errorMsg = [
  `Rendered line ${i} exceeds terminal width (${visibleWidth(line)} > ${width}).`,
  "",
  "This is likely caused by a custom TUI component not truncating its output.",
  "Use visibleWidth() to measure and truncateToWidth() to truncate lines.",
  "",
  `Debug log written to: ${crashLogPath}`,
].join("\n")
throw new Error(errorMsg)
```
**【逐字】错误消息全文**（`\n` 为换行）：
```
Rendered line ${i} exceeds terminal width (${visibleWidth(line)} > ${width}).

This is likely caused by a custom TUI component not truncating its output.
Use visibleWidth() to measure and truncateToWidth() to truncate lines.

Debug log written to: ${crashLogPath}
```

`positionHardwareCursor(cursorPos, totalLines)`：
```
if (!cursorPos || totalLines <= 0) { terminal.hideCursor(); return }
targetRow = max(0, min(cursorPos.row, totalLines - 1))
targetCol = max(0, cursorPos.col)
rowDelta = targetRow - this.hardwareCursorRow
buffer = ""
if (rowDelta > 0) buffer += `\x1b[${rowDelta}B` else if (rowDelta < 0) buffer += `\x1b[${-rowDelta}A`
buffer += `\x1b[${targetCol + 1}G`
if (buffer) terminal.write(buffer)
this.hardwareCursorRow = targetRow
if (getShowHardwareCursor()) terminal.showCursor() else terminal.hideCursor()
```

注释要点（源码逐字）："Excludes maxLinesRendered: the historical high-water mark caused
self-reinforcing inflation that pushed content into scrollback on terminal widen."（属 overlay 合成，在 §2.9.12）

---

### 2.11 `src/layout.ts`（453 行）

#### 2.11.1 类型

```ts
const OSC133_ZONE_PREFIX = /^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+/;

export interface LayoutRect { x: number; y: number; width: number; height: number; }
export interface LayoutBox {
  component: Component; rect: LayoutRect; clip: LayoutRect; children: LayoutBox[];
  parent?: LayoutBox; lines?: readonly string[]; lineOffset?: number;
  scrollView?: ScrollView; scrollContentLines?: readonly string[]; layer: number;
}
export interface LayoutFrame { root: LayoutBox; width: number; height: number; lines: string[];
                               primaryScrollView?: ScrollView; }
export interface ScrollbarGeometry { column: number; trackTop: number; trackHeight: number;
                                     thumbTop: number; thumbHeight: number; maxScrollTop: number; }
interface LayoutContext { viewport: {width,height}; renderCache: Map<Component, Map<number, string[]>>;
                          requestRender: () => void; primaryScrollView: ScrollView | undefined; }
```

#### 2.11.2 几何辅助

```ts
intersect(a, b) = { x: max(a.x,b.x), y: max(a.y,b.y),
                    width: max(0, min(a.x+a.width, b.x+b.width) - x),
                    height: max(0, min(a.y+a.height, b.y+b.height) - y) }

renderCached(ctx, component, width):
   safeWidth = max(1, floor(width))
   widths = ctx.renderCache.get(component) ?? new Map(); set
   lines = widths.get(safeWidth) ?? component.render(safeWidth); set
   return lines

measureHeight(ctx, c, w) = renderCached(ctx, c, w).length
measureWidth(ctx, c, w)  = renderCached(...).reduce((max, line) => max(max, visibleWidth(line)), 0)
withParent(box, parent)  → box.parent = parent; return box
translateBox(box, dy)    → box.rect.y += dy; 递归 children
updateClips(box, parentClip) → box.clip = intersect(parentClip, box.rect); 递归 children
```

#### 2.11.3 `layoutComponent(context, component, x, y, width, height|undefined, clip)` **精确**

```
safeWidth = max(1, floor(width))
node = getLayoutNode(component)

【无 layout node】
   lines = renderCached(ctx, component, safeWidth)
   allocatedHeight = height === undefined ? lines.length : max(0, floor(height))
   lineOffset = 0
   if (lines.length > allocatedHeight && allocatedHeight > 0) {
      cursorLine = lines.findIndex(l => l.includes(CURSOR_MARKER))
      if (cursorLine >= allocatedHeight) lineOffset = cursorLine - allocatedHeight + 1
   }
   return { component, rect:{x,y,width:safeWidth,height:allocatedHeight},
            clip: intersect(clip, {x,y,width:safeWidth,height:allocatedHeight}),
            children: [], lines, lineOffset, layer: 0 }

【scroll node】
   previousScrollTop = node.state.scrollTop
   contentWidth = node.state.getContentWidth(safeWidth)
   childBox = layoutComponent(ctx, node.component, x, y - previousScrollTop, contentWidth, undefined, clip)
   contentHeight = childBox.rect.height
   viewportHeight = height === undefined ? contentHeight : max(0, floor(height))
   node.state.updateLayout(contentHeight, viewportHeight, ctx.requestRender)
   translateBox(childBox, previousScrollTop - node.state.scrollTop)
   scrollView = node.state as ScrollView
   if (node.state.primary || !ctx.primaryScrollView) ctx.primaryScrollView = scrollView
   rect = { x, y, width: safeWidth, height: viewportHeight }
   childClip = intersect(clip, rect)
   box = { component, rect, clip: childClip, children: [childBox], scrollView,
           scrollContentLines: renderCached(ctx, node.component, contentWidth), layer: 0 }
   childBox.parent = box; updateClips(childBox, childClip); return box

【vstack / hstack】
   entries = visibleStackEntries(node.entries, ctx.viewport)
   gapTotal = max(0, entries.length - 1) * node.gap

   vstack:
      intrinsicHeights = entries.map(e => typeof e.basis === "number" ? e.basis
                                                                    : measureHeight(ctx, e.component, safeWidth))
      sizes = allocateStackSizes(entries, intrinsicHeights, height, node.gap)
      naturalHeight = sum(sizes) + gapTotal
      allocatedHeight = height === undefined ? naturalHeight : max(0, floor(height))
      rect = {x, y, width: safeWidth, height: allocatedHeight}
      box = { component, rect, clip: intersect(clip, rect), children: [], layer: 0 }
      childY = y
      for (index = 0..entries.length-1) {
         box.children.push(withParent(
            layoutComponent(ctx, entries[index].component, x, childY, safeWidth, sizes[index], box.clip), box))
         childY += sizes[index] + node.gap
      }
      return box

   hstack:
      intrinsicWidths = entries.map(e => typeof e.basis === "number" ? e.basis
                                                                   : measureWidth(ctx, e.component, safeWidth))
      widths = allocateStackSizes(entries, intrinsicWidths, safeWidth, node.gap)
      intrinsicHeights = entries.map((e, index) => measureHeight(ctx, e.component, max(1, widths[index])))
      allocatedHeight = height === undefined
                        ? intrinsicHeights.reduce((max, h) => max(max, h), 0) : max(0, height)
      rect = {x, y, width: safeWidth, height: allocatedHeight}
      box = { component, rect, clip: intersect(clip, rect), children: [], layer: 0 }
      childX = x
      for (index = 0..entries.length-1) {
         naturalChildHeight = intrinsicHeights[index]
         childHeight = node.align === "stretch" ? allocatedHeight : min(allocatedHeight, naturalChildHeight)
         childY = y
         if (node.align === "center") childY += floor((allocatedHeight - childHeight) / 2)
         else if (node.align === "end") childY += allocatedHeight - childHeight
         childWidth = widths[index]
         if (childWidth === 0) {
             box.children.push({ component: entries[index].component,
                                 rect: {x: childX, y: childY, width: 0, height: childHeight},
                                 clip: {x: childX, y: childY, width: 0, height: 0},
                                 children: [], parent: box, layer: 0 })       // 零宽子组件不 render
         } else {
             box.children.push(withParent(
                layoutComponent(ctx, entries[index].component, childX, childY, childWidth, childHeight, box.clip), box))
         }
         childX += childWidth + node.gap
      }
      return box
```

#### 2.11.4 `renderLayoutFrame(root, width, height, requestRender)`

```
safeWidth = max(1, floor(width)); safeHeight = max(1, floor(height))
ctx = { viewport: {width: safeWidth, height: safeHeight}, renderCache: new Map(),
        requestRender, primaryScrollView: undefined }
rootBox = layoutComponent(ctx, root, 0, 0, safeWidth, safeHeight, {x:0,y:0,width:safeWidth,height:safeHeight})
lines = Array.from({length: safeHeight}, () => "")
paintBox(rootBox, lines, safeWidth)
return { root: rootBox, width: safeWidth, height: safeHeight, lines,
         ...(ctx.primaryScrollView === undefined ? {} : { primaryScrollView: ctx.primaryScrollView }) }
```

#### 2.11.5 滚动条几何

`getScrollbarGeometry(box, includeHiddenAuto = false)`：
```
if (!box.scrollView || box.rect.width <= 0 || box.rect.height <= 0) return undefined
contentHeight = box.children[0]?.rect.height ?? box.scrollContentLines?.length ?? 0
trackHeight = box.rect.height
canRevealHiddenAuto = includeHiddenAuto && box.scrollView.scrollbar === "auto" && contentHeight > trackHeight
if (!box.scrollView.isScrollbarVisible && !canRevealHiddenAuto) return undefined
minThumbHeight = min(2, trackHeight)
thumbHeight = max(minThumbHeight, min(trackHeight, round((trackHeight * trackHeight) / contentHeight)))
maxScrollTop = max(0, contentHeight - trackHeight)
maxThumbTop  = trackHeight - thumbHeight
thumbOffset  = maxScrollTop === 0 ? 0 : round((box.scrollView.scrollTop / maxScrollTop) * maxThumbTop)
column = box.rect.x + box.rect.width - 1
if (column < box.clip.x || column >= box.clip.x + box.clip.width) return undefined
return { column, trackTop: box.rect.y, trackHeight, thumbTop: box.rect.y + thumbOffset,
         thumbHeight, maxScrollTop }
```

`replaceScrollbarCell(line, column, totalWidth, replacement, preserveTargetBackground)`（**逐字**）：
```
if (isImageLine(line)) return line
graphemeRange = getGraphemeCellRange(line, column)
start = graphemeRange?.start ?? column
end   = graphemeRange?.end ?? column + 1
before = sliceByColumn(line, 0, start, true)
target = sliceByColumn(line, start, end - start, true)
after  = sliceByColumn(line, end, max(0, totalWidth - end), true)
targetPrefix = target 开头的连续 ANSI 序列拼接（遇到非 ANSI 即停）
beforePadding     = " ".repeat(max(0, start - visibleWidth(before)))
cellPaddingBefore = " ".repeat(max(0, column - start))
cellPaddingAfter  = " ".repeat(max(0, end - column - 1))
targetStyle = `\x1b[0m\x1b]8;;\x07${preserveTargetBackground ? getActiveBackgroundAnsi(targetPrefix) : ""}`
return `${before}${beforePadding}${targetStyle}${cellPaddingBefore}${replacement}${cellPaddingAfter}${after}`
```

`paintScrollbar(box, screen, totalWidth)`：
```
geometry = getScrollbarGeometry(box); if (!geometry || !box.scrollView) return
for (offset = 0; offset < geometry.trackHeight; offset++) {
   row = geometry.trackTop + offset
   if (row < box.clip.y || row >= box.clip.y + box.clip.height || row < 0 || row >= screen.length) continue
   isThumb = row >= geometry.thumbTop && row < geometry.thumbTop + geometry.thumbHeight
   replacement = isThumb
       ? box.scrollView.scrollbarThumbStyle(box.scrollView.isScrollbarActive ? "█" : "┃")
       : box.scrollView.scrollbarTrackStyle("│")
   screen[row] = replaceScrollbarCell(screen[row] ?? "", geometry.column, totalWidth, replacement,
                                      box.scrollView.scrollbar !== "always")
}
```
（`█` U+2588、`┃` U+2503、`│` U+2502。）

#### 2.11.6 `paintBox`

```
if (box.lines) {
   offset = box.lineOffset ?? 0
   firstRow = max(box.rect.y, box.clip.y, 0)
   lastRow  = min(box.rect.y + box.rect.height, box.clip.y + box.clip.height, screen.length)
   for (row = firstRow; row < lastRow; row++) {
      sourceLine = box.lines[offset + row - box.rect.y]
      if (sourceLine === undefined) continue
      line = sourceLine.replace(OSC133_ZONE_PREFIX, "")
      imageMetadata = getKittyImageMetadata(line)
      if (imageMetadata) {
         clipBottom = min(screen.length, box.clip.y + box.clip.height)
         visibleRows = min(imageMetadata.rows, clipBottom - row)
         if (visibleRows < imageMetadata.rows) line = cropKittyImageLine(line, 0, visibleRows)
      }
      if (box.rect.x === 0 && box.rect.width >= totalWidth && (isImageLine(line) || !screen[row])) {
         screen[row] = line                                  // 全宽快路径
      } else {
         screen[row] = compositeTuiLine(screen[row] ?? "", line, box.rect.x, box.rect.width, totalWidth)
      }
   }
}
for (child of box.children) paintBox(child, screen, totalWidth)

if (box.scrollView && box.scrollContentLines && box.scrollView.scrollTop > 0 && box.rect.height > 0) {
   scrollTop = box.scrollView.scrollTop
   minRow = max(0, scrollTop - getMaxKittyImageRows())
   for (imageRow = scrollTop - 1; imageRow >= minRow; imageRow--) {
      imageLine = box.scrollContentLines[imageRow] ?? ""
      if (!isImageLine(imageLine)) continue
      metadata = getKittyImageMetadata(imageLine)
      if (!metadata) continue
      hiddenRows = scrollTop - imageRow
      if (hiddenRows < metadata.rows) {
         visibleRows = min(box.rect.height, metadata.rows - hiddenRows)
         cropped = cropKittyImageLine(imageLine, hiddenRows, visibleRows)
         if (box.rect.x === 0 && box.rect.width >= totalWidth) screen[box.rect.y] = cropped
      }
      break
   }
}
paintScrollbar(box, screen, totalWidth)
```

#### 2.11.7 命中测试

```ts
containsPoint(rect, x, y) = x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height

export function getLayoutBoxesAt(frame, x, y): LayoutBox[] {
   result = []
   visit(box, depth) { if (!containsPoint(box.clip, x, y)) return; result.push({box, depth});
                       for (child of box.children) visit(child, depth + 1) }
   visit(frame.root, 0)
   result.sort((a, b) => b.box.layer - a.box.layer || b.depth - a.depth)
   return result.map(r => r.box)
}

export function getScrollViewBox(frame, scrollView): LayoutBox | undefined   // DFS 首个 box.scrollView === scrollView
export function getScrollViewsAt(frame, x, y): ScrollView[] {
   result = []
   visit(box, depth) { if (!containsPoint(box.clip, x, y)) return;
                       if (box.scrollView && containsPoint(box.rect, x, y)) result.push({scrollView: box.scrollView, depth})
                       for (child of box.children) visit(child, depth + 1) }
   visit(frame.root, 0)
   result.sort((a, b) => b.depth - a.depth)
   return result.map(e => e.scrollView)
}
```

测试（`layout.test.ts`，15 个 it）：grow 分配确定性、固定 basis 的滚动内容在测量阶段不渲染、
超大滚动内容只绘制裁剪行、按 minSize 收缩、嵌套 minSize 计入固有测量、隐藏条目不含 gap、
滚动视图下边界裁剪 Kitty 图片、hstack 按分配宽度合成、零宽子组件不绘制、follow-end 状态与
未用滚动增量、比例滚动条与激活态加粗 thumb、覆盖层滚动条只保留底色、运行时更新保留列、
嵌套滚动内容按受限子几何测量、内容变化后重建几何。

---

### 2.12 `src/alt-screen-search.ts`（327 行，备用屏搜索）

> 若由其他单元实现，只需保证本节契约。

#### 2.12.1 类型与语料构建

```ts
interface SearchSourceSpan { textStart: number; textEnd: number; row: number; startCol: number;
                             endCol: number; linearColumns: boolean; }
interface SearchCorpus { text: string; spans: SearchSourceSpan[]; }
export interface AltScreenSearchSegment { row: number; startCol: number; endCol: number; }
export interface AltScreenSearchMatch { segments: AltScreenSearchSegment[]; }
const PRINTABLE_ASCII = /^[\x20-\x7e]*$/;
```

`buildSearchCorpus(lines)` **精确**：
```
chunks = []; spans = []; textLength = 0; pendingSeparator = false
appendSeparator() { if (!pendingSeparator) return; chunks.push(" "); textLength += 1; pendingSeparator = false }

for (row = 0; row < lines.length; row++) {
   line = stripTerminalSequences(lines[row] ?? "")
   column = 0
   if (PRINTABLE_ASCII.test(line)) {            // 快路径：整段非空格 run 一次索引
      index = 0
      while (index < line.length) {
         if (line.charCodeAt(index) === 0x20) {
            if (textLength > 0) pendingSeparator = true
            column += 1; index += 1; continue
         }
         end = index + 1
         while (end < line.length && line.charCodeAt(end) !== 0x20) end++
         appendSeparator()
         text = line.slice(index, end)
         chunks.push(text)
         spans.push({ textStart: textLength, textEnd: textLength + text.length, row,
                      startCol: column, endCol: column + text.length, linearColumns: true })
         textLength += text.length; column += text.length; index = end
      }
   } else {
      for (grapheme of segmenter.segment(line)) {     // grapheme 模式
         text = grapheme.segment
         width = visibleWidth(text)
         if (/^\s+$/u.test(text)) { if (textLength > 0) pendingSeparator = true; column += width; continue }
         appendSeparator()
         chunks.push(text)
         spans.push({ textStart: textLength, textEnd: textLength + text.length, row,
                      startCol: column, endCol: column + width, linearColumns: false })
         textLength += text.length; column += width
      }
   }
   if (textLength > 0) pendingSeparator = true       // 行间分隔
}
return { text: chunks.join(""), spans }
```
（`textLength > 0` 判断让首行不以空格开头；行尾 `pendingSeparator = true` 使跨行匹配以单个空格连接。）

#### 2.12.2 查询与匹配

```ts
function normalizeQuery(query: string): string { return query.replace(/\s+/gu, " ").trim(); }
function escapeRegExp(text: string): string { return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); }

function findSearchCorpusMatches(corpus, normalizedQuery): AltScreenSearchMatch[] {
   if (!normalizedQuery) return []
   expression = new RegExp(escapeRegExp(normalizedQuery), "giu")
   matches = []; spanIndex = 0
   for (match of corpus.text.matchAll(expression)) {
      start = match.index; end = start + match[0].length
      while (spanIndex < corpus.spans.length && corpus.spans[spanIndex].textEnd <= start) spanIndex++
      segments = []
      for (index = spanIndex; index < corpus.spans.length; index++) {
         span = corpus.spans[index]
         if (span.textStart >= end) break
         if (span.textEnd <= start) continue
         startCol = span.linearColumns ? span.startCol + max(start, span.textStart) - span.textStart : span.startCol
         endCol   = span.linearColumns ? span.startCol + min(end,   span.textEnd)   - span.textStart : span.endCol
         previous = segments[segments.length - 1]
         if (previous && previous.row === span.row && startCol <= previous.endCol)
            previous.endCol = max(previous.endCol, endCol)
         else segments.push({ row: span.row, startCol, endCol })
      }
      while (spanIndex < corpus.spans.length && corpus.spans[spanIndex].textEnd <= end) spanIndex++
      if (segments.length > 0) matches.push({ segments })
   }
   return matches
}
```
`"giu"` 标志：**忽略大小写 + 全局 + Unicode**。Rust `regex` 用 `(?i)` + 迭代 `find_iter`
（`i` 与 JS 的 `i` 在 Unicode 大小写折叠上有差异：JS 用 simple case folding；Rust `regex` 默认
也用 Unicode simple case folding，通常一致；但土耳其语 İ/ı 等边角需测试）。

`AltScreenSearchIndex`：
```ts
export interface AltScreenSearchResult { matches: AltScreenSearchMatch[]; changed: boolean; }

class AltScreenSearchIndex {
  private sourceLines?: string[]; private corpus?: SearchCorpus;
  private normalizedQuery?: string; private matches: AltScreenSearchMatch[] = [];

  search(lines: readonly string[], query: string): AltScreenSearchResult {
     let sourceChanged = this.sourceLines?.length !== lines.length
     if (!sourceChanged && this.sourceLines) {
        for (index = 0; index < lines.length; index++)
           if (this.sourceLines[index] !== lines[index]) { sourceChanged = true; break }
     }
     if (sourceChanged || !this.corpus) { this.sourceLines = Array.from(lines); this.corpus = buildSearchCorpus(lines) }
     normalizedQuery = normalizeQuery(query)
     changed = sourceChanged || normalizedQuery !== this.normalizedQuery
     if (changed) { this.normalizedQuery = normalizedQuery; this.matches = findSearchCorpusMatches(this.corpus, normalizedQuery) }
     return { matches: this.matches, changed }        // 注意：未 changed 时返回同一数组引用
  }
}

export function findAltScreenSearchMatches(lines, query): AltScreenSearchMatch[] {
   // 注意：先 normalizeQuery（`\s+` → 单个空格后 trim），**空/纯空白 query 直接返回 `[]`**
   // （不构建语料、不返回全部行）；Rust 侧不要退化成 "match everything"
   normalizedQuery = normalizeQuery(query)
   return normalizedQuery ? findSearchCorpusMatches(buildSearchCorpus(lines), normalizedQuery) : []
}
export function getAltScreenSearchMatchKey(match): string {
   first = match.segments[0]; last = match.segments[match.segments.length - 1]
   return first && last ? `${first.row}:${first.startCol}:${last.row}:${last.endCol}` : ""
}
```

#### 2.12.3 `AltScreenSearchComponent`

```ts
class AltScreenSearchComponent implements Component, Focusable {
  private readonly input = new Input({
     prompt: " ",
     placeholder: "Find in transcript",
     placeholderStyle: (text) => `\x1b[2m${text}\x1b[22m`,
  });
  private readonly onQueryChange: (query: string) => void;
  private readonly navigationButtonStyle: (text: string, hovered: boolean) => string;
  constructor(
     onQueryChange: (query: string) => void,
     navigationButtonStyle: (text: string, hovered: boolean) => string = (text) => text,  // 默认恒等
  ) { this.onQueryChange = onQueryChange; this.navigationButtonStyle = navigationButtonStyle; }
  private resultCount = 0; private resultIndex = -1;
  private previousButtonStart = -1; private previousButtonEnd = -1;
  private nextButtonStart = -1; private nextButtonEnd = -1;
  private hoveredNavigationDirection?: -1 | 1;
  private _focused = false;
}
```

- `focused` getter/setter：setter 同时设置 `input.focused`
- `setResult(index, count)`：赋值到 `resultIndex`/`resultCount`
- `getNavigationDirectionAt(row, column)`：`row !== 2` → undefined；
  `column ∈ [previousButtonStart, previousButtonEnd)` → -1；`[nextButtonStart, nextButtonEnd)` → 1；否则 undefined
- `setHoveredNavigationDirection(direction)`：值相同 → false；否则更新并 true
- `handleInput(data)`：记录 `previous = input.getValue()`；`input.handleInput(data)`；
  `query = input.getValue()`；`query !== previous` → `onQueryChange(query)`
- `invalidate()` → `input.invalidate()`

`render(width)` **精确布局**：
```
safeWidth = max(1, width); innerWidth = max(0, safeWidth - 2)
formatKey(key): key ? key.split("+").map(part =>
        (platform === "darwin" && part.toLowerCase() === "alt") ? "Option"
        : part.charAt(0).toUpperCase() + part.slice(1)).join("+")
              : "Unbound"
previousKey = formatKey(getKeybindings().getKeys("tui.altScreen.searchPrevious")[0])
nextKey     = formatKey(getKeybindings().getKeys("tui.altScreen.searchNext")[0])
query = input.getValue()
result = !query ? "" : (resultCount === 0 ? "No matches" : `${resultIndex + 1}/${resultCount}`)
resultSpace = max(0, innerWidth - 3)
visibleResult = truncateToWidth(result, resultSpace, "")
resultText = visibleResult ? `\x1b[2m ${visibleResult} \x1b[22m` : ""
inputWidth = max(0, innerWidth - visibleWidth(resultText))
inputLine = truncateToWidth(input.render(max(1, inputWidth))[0] ?? "", inputWidth, "")
inputPadding = " ".repeat(max(0, inputWidth - visibleWidth(inputLine)))
content = `${inputLine}${inputPadding}${resultText}`

previousButton = `↑ ${previousKey}`; nextButton = `↓ ${nextKey}`; separator = " · "
outerGapWidth = 1
availableControlsWidth = max(0, innerWidth - outerGapWidth * 2 - 1)
controlsWidth = w(previousButton) + w(separator) + w(nextButton)
if (controlsWidth > availableControlsWidth) {
   previousButton = "↑"; nextButton = "↓"; separator = " "
   controlsWidth = w(previousButton) + w(separator) + w(nextButton)
}
showButtons = controlsWidth <= availableControlsWidth
renderedButtons = showButtons
   ? navigationButtonStyle(previousButton, hovered === -1) + separator
     + navigationButtonStyle(nextButton, hovered === 1) : ""
   // 注意：样式函数只包住按钮文字，**不**包住 separator；默认恒等函数时输出不含任何 ANSI
outerGapsWidth = showButtons ? 2 : 0
rightRuleWidth = renderedButtons && innerWidth > controlsWidth + outerGapsWidth ? 1 : 0
leftRuleWidth = max(0, innerWidth - (showButtons ? controlsWidth : 0) - outerGapsWidth - rightRuleWidth)
previousStart = 1 + leftRuleWidth + outerGapWidth
previousButtonStart = showButtons ? previousStart : -1
previousButtonEnd   = showButtons ? previousStart + w(previousButton) : -1
nextButtonStart     = showButtons ? previousButtonEnd + w(separator) : -1
nextButtonEnd       = showButtons ? nextButtonStart + w(nextButton) : -1

if (safeWidth === 1) return ["┌", "│", "└"]
return [
  `┌${"─".repeat(innerWidth)}┐`,
  `│${content}│`,
  `└${"─".repeat(leftRuleWidth)}${renderedButtons ? " " : ""}${renderedButtons}${renderedButtons ? " " : ""}${"─".repeat(rightRuleWidth)}┘`,
]
```
**【逐字】字符串**：placeholder `"Find in transcript"`、prompt `" "`、`"No matches"`、`"Unbound"`、
`"Option"`（darwin 的 alt）、`"↑"`、`"↓"`、`" · "`、`"┌"`、`"┐"`、`"└"`、`"┘"`、`"│"`、`"─"`。

---

### 2.13 `src/tui-alt-screen.ts`（1798 行）

#### 2.13.1 常量（**逐字**）

```ts
const ENTER_ALT_SCREEN = "\x1b[?1049h";
const EXIT_ALT_SCREEN  = "\x1b[?1049l";
const DISABLE_AUTOWRAP = "\x1b[?7l";
const ENABLE_AUTOWRAP  = "\x1b[?7h";
const ENABLE_BUTTON_MOTION_MOUSE = "\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h";
const ENABLE_ALL_MOTION_MOUSE    = "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h";
const DISABLE_MOUSE = "\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const FOCUS_IN  = "\x1b[I";
const FOCUS_OUT = "\x1b[O";
const BEGIN_SYNCHRONIZED_OUTPUT = "\x1b[?2026h";
const END_SYNCHRONIZED_OUTPUT   = "\x1b[?2026l";
const OSC133_ZONE_PREFIX  = /^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+/;
const OSC133_PROMPT_START = /^\x1b\]133;A(?:\x07|\x1b\\)/;
const PAGE_SCROLL_OVERLAP = 4;
const ALT_WHEEL_SCROLL_MULTIPLIER = 5;
const MAX_CACHED_OFFSCREEN_KITTY_IMAGES = 16;
const MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES = 32 * 1024 * 1024;
const MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES      = 64 * 1024 * 1024;
const DOUBLE_CLICK_INTERVAL_MS = 500;
const COPY_ERROR_FLASH_DURATION_MS = 5000;
const TERMINAL_WORD_SELECTION_JOINERS = new Set(["/", "-", "+", "\\", "~", "_", "."]);
```

#### 2.13.2 选项

```ts
export interface TuiAltScreenOptions {
  wheelScrollLines?: WheelScrollLines;                    // 默认 1
  mouse?: boolean;                                        // 默认 true
  searchMatchStyle?: (text: string) => string;            // 默认 `\x1b[4m${text}\x1b[24m`
  searchCurrentMatchStyle?: (text: string) => string;     // 默认 `\x1b[1;7m${text}\x1b[22;27m`
  searchNavigationButtonStyle?: (text: string, hovered: boolean) => string;  // 默认 identity
  scrollToEndIndicator?: () => string;
  openUrl?: (url: string) => void;
  onRightClickPaste?: () => void;
  copyOnSelect?: boolean;                                 // 默认 true
  copySelection?: (text: string) => Promise<boolean | string>;
}
```

#### 2.13.3 内部状态

```
previousScreen: string[] = []; lastDocument: string[] = [];
previousScreenWidth = 0; previousScreenHeight = 0;
layoutRoot?: Component; currentLayout?: LayoutFrame;
implicitDocument: Component;      // {
                                  //   render: (w) => super.render(w),
                                  //   handleMouse: (e) => super.handleMouse(e),
                                  //   invalidate: () => this.children.forEach(c => c.invalidate())
                                  // }
implicitScrollView: ScrollView;   // new ScrollView(implicitDocument, { follow: "end", primary: true })
flashes: AltScreenFlashContainer; // new AltScreenFlashContainer(() => this.requestRender())
altScreenActive = false; imageProtocol: ImageProtocol = null; savedCapabilities?: TerminalCapabilities;
uploadedKittyImages: Map<number, CachedKittyImage>;   // 插入顺序即 LRU 顺序
selectionAnchor?; selectionFocus?; selectionGranularity: "character"; selectionInitialRange?;
lastClick?; selectionDragPointer?; selectionAutoScrollDirection: -1|0|1 = 0;
selectionAutoScrollTimer?; selectionPressActive = false;
scrollbarDrag?; scrollbarHover?: ScrollView; scrollToEndIndicatorRect?;
activeSearch?; pressedUrl?; selectionDragged = false;
mouseCapture?; mousePressTarget?; mousePressPoint?; mousePressMoved = false; lastComponentClick?;
wheelScroll: WheelScrollAccelerator;
```

构造后**固化为 `private readonly`** 的选项（构造时 `??` 求值一次；Rust 侧不要把它们留在可变 options 里，
运行期修改 options 对象不会生效）：
```
private readonly searchMatchStyle / searchCurrentMatchStyle / searchNavigationButtonStyle
private readonly scrollToEndIndicator? / openUrl? / onRightClickPaste?
private readonly copySelection?: (text: string) => Promise<boolean | string>
private mouseEnabled: boolean          // 例外：非 readonly
```

构造函数：
```ts
constructor(terminal, showHardwareCursor?, logDirectory?, options: TuiAltScreenOptions = {}) {
  super(terminal, showHardwareCursor, logDirectory);
  this.implicitDocument = {...};
  this.implicitScrollView = new ScrollView(this.implicitDocument, { follow: "end", primary: true });
  this.flashes = new AltScreenFlashContainer(() => this.requestRender());
  this.wheelScroll = new WheelScrollAccelerator(options.wheelScrollLines ?? 1);
  this.mouseEnabled = options.mouse ?? true;
  this.searchMatchStyle = options.searchMatchStyle ?? (text => `\x1b[4m${text}\x1b[24m`);
  this.searchCurrentMatchStyle = options.searchCurrentMatchStyle ?? (text => `\x1b[1;7m${text}\x1b[22;27m`);
  this.searchNavigationButtonStyle = options.searchNavigationButtonStyle ?? (text => text);
  this.scrollToEndIndicator = options.scrollToEndIndicator;
  this.openUrl = options.openUrl; this.onRightClickPaste = options.onRightClickPaste;
  this.copyOnSelect = options.copyOnSelect ?? true; this.copySelection = options.copySelection;
  this.addInputListener(data => this.handleViewportInput(data));       // 自己也是输入监听器
}
```

`mode = "fullscreen"`；`[VIEWPORT_TUI] = true`；`render(width) = layoutRoot?.render(width) ?? super.render(width)`；
`getMountedRoots() = layoutRoot ? [layoutRoot] : children`；`getPrimaryScrollView() = currentLayout?.primaryScrollView ?? implicitScrollView`。

#### 2.13.4 生命周期

`beforeTerminalStart()` **顺序**：
```
stopSelectionAutoScroll(); selectionPressActive = false; stopScrollbarHover(); stopScrollbarDrag()
flashes.dispose(); altScreenActive = true
capabilities = getCapabilities(); imageProtocol = capabilities.images; uploadedKittyImages.clear()
if (capabilities.images === "iterm2") {
   savedCapabilities = capabilities; setCapabilities({ ...capabilities, images: null }); this.invalidate()
}
lastDocument = []; selectionAnchor = undefined; selectionFocus = undefined
selectionGranularity = "character"; selectionInitialRange = undefined; lastClick = undefined
pressedUrl = undefined; selectionDragged = false; clearComponentMouseGesture(); lastComponentClick = undefined
resetRenderState()
term = process.env.TERM?.toLowerCase() ?? ""
mouseSequence = (TMUX !== undefined || ZELLIJ !== undefined || STY !== undefined
                 || term.startsWith("tmux") || term.startsWith("screen"))
                ? ENABLE_BUTTON_MOTION_MOUSE : ENABLE_ALL_MOTION_MOUSE
terminal.write(`${ENTER_ALT_SCREEN}${DISABLE_AUTOWRAP}${mouseEnabled ? mouseSequence : ""}\x1b[2J\x1b[H\x1b[?25l`)
```

`beforeTerminalStop(_options)`：
```
closeSearch(); stopSelectionAutoScroll(); selectionPressActive = false
stopScrollbarHover(); stopScrollbarDrag(); clearComponentMouseGesture(); flashes.dispose()
if (!altScreenActive) return
terminal.write(`${BEGIN_SYNCHRONIZED_OUTPUT}${deleteKittyImages()}${mouseEnabled ? DISABLE_MOUSE : ""}${ENABLE_AUTOWRAP}${END_SYNCHRONIZED_OUTPUT}`)
uploadedKittyImages.clear()
```

`afterTerminalStop(options)`：
```
if (!altScreenActive) return
altScreenActive = false
if (options.preserveScreen) {
    terminal.write(`${BEGIN_SYNCHRONIZED_OUTPUT}${EXIT_ALT_SCREEN}\x1b[?25h${END_SYNCHRONIZED_OUTPUT}`)
} else {
    width = max(1, terminal.columns)
    documentLines = resolveFakeCursors(render(width)).map(line => line.replace(OSC133_ZONE_PREFIX, ""))
    lastDocument = applyLineResets(documentLines.map(l => l.replaceAll(CURSOR_MARKER, "")))
        .map(line => (isImageLine(line) || visibleWidth(line) <= width) ? line : sliceByColumn(line, 0, width, true))
    buffer = `${BEGIN_SYNCHRONIZED_OUTPUT}${EXIT_ALT_SCREEN}${DISABLE_AUTOWRAP}`
    for (row = 0; row < lastDocument.length; row++) {
        if (row > 0) buffer += "\r\n"
        buffer += `\r\x1b[2K${lastDocument[row] ?? ""}`
    }
    buffer += `\x1b[0m${ENABLE_AUTOWRAP}\r\n\x1b[?25h${END_SYNCHRONIZED_OUTPUT}`
    terminal.write(buffer)
}
if (savedCapabilities) { setCapabilities(savedCapabilities); savedCapabilities = undefined }
```

`getExitRestorePrefix()`：`super.getExitRestorePrefix()`；若 `!altScreenActive` 直接返回；
否则 `${END_SYNCHRONIZED_OUTPUT}${mouseEnabled ? DISABLE_MOUSE : ""}${ENABLE_AUTOWRAP}${prefix}`。
`getExitRestoreSuffix()`：`super.getExitRestoreSuffix() + (altScreenActive ? `${EXIT_ALT_SCREEN}\x1b[?25h` : "")`。

`deleteKittyImages()`（私有）：`imageProtocol === "kitty" ? deleteAllKittyImages() : ""`。
`resetRenderState()`（override）：`previousScreen=[]; previousScreenWidth=0; previousScreenHeight=0; currentLayout=undefined`。

#### 2.13.5 Kitty 缓存 `prepareKittyScreen(screen)`

```
visibleImageIds = Set()
lines = screen.map(line => {
   placement = getKittyImagePlacement(line); if (!placement) return line
   visibleImageIds.add(placement.imageId)
   cached = uploadedKittyImages.get(placement.imageId)
   next = { transmissionGeneration: placement.transmissionGeneration,
            transmissionBytes: placement.transmissionBytes,
            estimatedDecodedBytes: placement.estimatedDecodedBytes }
   if (cached) uploadedKittyImages.delete(placement.imageId)     // 重新插入 → LRU 末尾
   uploadedKittyImages.set(placement.imageId, next)
   return cached?.transmissionGeneration === placement.transmissionGeneration
          ? placement.replacementLine : line
})

cachedOffscreenImageCount = 0; cachedOffscreenTransmissionBytes = 0; cachedOffscreenDecodedBytes = 0
for ([imageId, cached] of uploadedKittyImages) {
   if (visibleImageIds.has(imageId)) continue
   count += 1; transmissionBytes += cached.transmissionBytes; decodedBytes += cached.estimatedDecodedBytes
}
evictedImageDeletion = ""
for ([imageId, cached] of uploadedKittyImages) {
   if (count <= 16 && transmissionBytes <= 32MiB && decodedBytes <= 64MiB) break
   if (visibleImageIds.has(imageId)) continue
   evictedImageDeletion += deleteKittyImage(imageId)
   uploadedKittyImages.delete(imageId)
   count -= 1; transmissionBytes -= cached.transmissionBytes; decodedBytes -= cached.estimatedDecodedBytes
}
return { lines, evictedImageDeletion }
```

#### 2.13.6 视口输入 `handleViewportInput(data)` — **顺序不可变**

```
1. data === FOCUS_OUT ("\x1b[O")：
     hadActiveSelection = selectionPressActive
     hadNonEmptyActiveSelection = hadActiveSelection && getSelectionBounds() !== undefined
     selectionPressActive = false; stopSelectionAutoScroll(); stopScrollbarHover()
     if (activeSearch?.component.setHoveredNavigationDirection(undefined)) requestRender()
     stopScrollbarDrag(); pressedUrl = undefined; selectionDragged = false
     clearComponentMouseGesture(); lastComponentClick = undefined
     if (hadActiveSelection) { 清 selectionAnchor/Focus/Granularity/InitialRange
                               if (hadNonEmptyActiveSelection) requestRender() }
     lastClick = undefined
     return { consume: true }
2. data === FOCUS_IN ("\x1b[I") → { consume: true }
3. wheelEvent = parseWheelEvent(data); 命中：
     lines = wheelScroll.next(direction, performance.now())
     wheelDelta = direction * ((button & 8) !== 0 ? lines * 5 : lines)
     event = createMouseEvent("wheel", button, x, y, { wheelDelta })
     overlay = dispatchMouseToOverlay(event)
     result = overlay.result ?? (overlay.hit ? undefined : dispatchMouseToLayout(event))
     if (result) { if (applyMouseDispatchResult(event, result)) requestRender(); return { consume: true } }
     routeWheel(wheelEvent, wheelDelta)
     return { consume: true }
4. mouseEvent = parseSgrMouseEvent(data); 命中：handleMouseEvent(mouseEvent); return { consume: true }
5. isMouseSequence(data) → { consume: true }
6. keybindings = getKeybindings(); isRelease = isKeyRelease(data)
7. matches("tui.altScreen.search")            → !isRelease && toggleSearch(); return {consume:true}
8. if (activeSearch?.overlay?.isFocused()) {
      matches("tui.altScreen.searchNext")     → !isRelease && navigateSearch(1);  return {consume:true}
      matches("tui.altScreen.searchPrevious") → !isRelease && navigateSearch(-1); return {consume:true}
      matches("tui.altScreen.searchClose")    → !isRelease && closeSearch();      return {consume:true}
   }
9. if (shouldDeferViewportInputToOverlay()) return undefined
      // = isOverlayFocused() && activeSearch?.overlay?.isFocused() !== true
10. pageUp          → !isRelease && scrollBy(-max(1, viewportHeight - 4)); return {consume:true}
    pageDown        → !isRelease && scrollBy( max(1, viewportHeight - 4)); return {consume:true}
    halfPageUp      → !isRelease && scrollBy(-max(1, floor(viewportHeight / 2))); return {consume:true}
    halfPageDown    → !isRelease && scrollBy( max(1, floor(viewportHeight / 2))); return {consume:true}
    lineUp          → !isRelease && scrollBy(-1); return {consume:true}
    lineDown        → !isRelease && scrollBy( 1); return {consume:true}
    previousPrompt  → !isRelease && scrollToPrompt(-1); return {consume:true}
    nextPrompt      → !isRelease && scrollToPrompt(1);  return {consume:true}
    top             → !isRelease && scrollToTop();    return {consume:true}
    bottom          → !isRelease && scrollToBottom(); return {consume:true}
11. return undefined
```
（**释放事件仍然被 consume**，只是不执行动作。）

#### 2.13.7 滚轮解析

`parseWheelEvent(data)`：
```js
const sgr = /^\x1b\[<(\d+);(\d+);(\d+)[Mm]$/.exec(data);
if (sgr) {
   button = parseInt(sgr[1]); if ((button & 64) === 0) return undefined
   direction = button & 3; if (direction !== 0 && direction !== 1) return undefined
   return { direction: direction === 0 ? -1 : 1, x: parseInt(sgr[2]) - 1, y: parseInt(sgr[3]) - 1, button }
}
if (data.length === 6 && data.startsWith("\x1b[M")) {       // X10 编码
   button = data.charCodeAt(3) - 32; if ((button & 64) === 0) return undefined
   direction = button & 3; if (direction !== 0 && direction !== 1) return undefined
   return { direction: direction === 0 ? -1 : 1, x: data.charCodeAt(4) - 33, y: data.charCodeAt(5) - 33, button }
}
return undefined
```

`routeWheel(event, delta)`：
```
remaining = delta; seen = Set<ScrollView>()
for (scrollView of currentLayout ? getScrollViewsAt(currentLayout, event.x, event.y) : []) {
   seen.add(scrollView)
   remaining = scrollView.scrollBy(remaining)
   if (remaining === 0 || scrollView.overscroll === "contain") break
}
primary = getPrimaryScrollView()
if (remaining !== 0 && !seen.has(primary)) primary.scrollBy(remaining)
updateScrollbarHover(event.x, event.y)
requestRender()
```

#### 2.13.8 鼠标事件

`parseSgrMouseEvent(data)`：`/^\x1b\[<(\d+);(\d+);(\d+)([Mm])$/` → `{button, x: g2-1, y: g3-1, release: g4 === "m"}`。

`isMouseSequence(data)`：`/^\x1b\[<\d+;\d+;\d+[Mm]$/.test(data) || (data.length === 6 && data.startsWith("\x1b[M"))`。

`decodeMouseButton(button)`：`button & 3` → 0 left、1 middle、2 right、3 none。

`createMouseEvent(type, button, x, y, extra)`：
```
{ type, button: type === "wheel" ? "none" : decodeMouseButton(button), x, y,
  screenX: x, screenY: y,
  width: max(1, terminal.columns), height: max(1, terminal.rows),
  shift: (button & 4) !== 0, alt: (button & 8) !== 0, ctrl: (button & 16) !== 0,
  ...(extra.wheelDelta !== undefined ? {wheelDelta} : {}),
  ...(extra.clickCount !== undefined ? {clickCount} : {}) }
```

`handleMouseEvent(raw)` **精确流程**：
```
isMotion = (raw.button & 32) !== 0
type = raw.release ? "release"
     : isMotion ? (decodeMouseButton(raw.button) === "none" ? "move" : "drag")
     : "press"
event = createMouseEvent(type, raw.button, raw.x, raw.y)

【捕获/按下目标分支】
if (mouseCapture || mousePressTarget) {
   target = mouseCapture ?? mousePressTarget
   if (mousePressPoint && (raw.x !== mousePressPoint.x || raw.y !== mousePressPoint.y)) {
       mousePressMoved = true; lastComponentClick = undefined
   }
   render = false
   targetResult = dispatchMouseToTarget(event, target)
   if (targetResult) render = applyMouseDispatchResult(event, targetResult)
   if (raw.release) {
       if (!mousePressMoved && mousePressPoint?.x === raw.x && mousePressPoint.y === raw.y) {
           clickEvent = createMouseEvent("click", raw.button, raw.x, raw.y,
                         { clickCount: getComponentClickCount(target, raw.x, raw.y) })
           clickResult = dispatchMouseToTarget(clickEvent, target)
           if (clickResult) render = applyMouseDispatchResult(clickEvent, clickResult) || render
       }
       clearComponentMouseGesture()
   }
   if (render) requestRender()
   return
}

if (handleSearchMouseEvent(raw)) return

overlay = dispatchMouseToOverlay(event)
if (!overlay.hit) {
   if (handleScrollToEndIndicatorMouseEvent(raw)) return
   scrollbarHandled = handleScrollbarMouseEvent(raw)
   if (!scrollbarDrag) updateScrollbarHover(raw.x, raw.y)
   if (scrollbarHandled) return
} else stopScrollbarHover()

result = overlay.result ?? (overlay.hit ? undefined : dispatchMouseToLayout(event))
if (result) {
   render = applyMouseDispatchResult(event, result)
   if (type === "press") { clearTextSelection(); mousePressTarget = result.target
                           mousePressPoint = {x: raw.x, y: raw.y}; mousePressMoved = false }
   if (render) requestRender()
   return
}

if (handleRightClickPaste(raw)) return
handleSelectionMouseEvent(raw)
```

`dispatchMouseToLayout(event)`：
```
if (!currentLayout) return undefined
visited = Set<Component>()
for (box of getLayoutBoxesAt(currentLayout, event.screenX, event.screenY)) {
   if (visited.has(box.component)) continue
   if (getLayoutNode(box.component) && box.component.handleMouse === Container.prototype.handleMouse) continue
   visited.add(box.component)
   result = dispatchMouseEvent(box.component, { ...event, x: screenX - box.rect.x, y: screenY - box.rect.y,
                                                 width: box.rect.width, height: box.rect.height })
   if (result) return result
}
return undefined
```

`applyMouseDispatchResult(event, result)` → boolean（是否需要渲染）：
```
focusTarget = resolveMouseFocusTarget(result.focusTarget ?? result.target.component)
focusChanged = result.focus === true && getFocusedComponent() !== focusTarget
if (result.focus) setFocus(focusTarget)
if (result.capture) mouseCapture = result.target
return result.render ?? (focusChanged || type ∈ {press, click, drag, wheel})
```

`getComponentClickCount(target, x, y)`：
```
now = Date.now(); previous = lastComponentClick
count = (previous && now - previous.timestamp <= 500 && previous.component === target.component
         && previous.x === x && previous.y === y) ? (previous.count % 3) + 1 : 1
lastComponentClick = { timestamp: now, count, component: target.component, x, y }
return count
```

其它鼠标辅助：
- `handleRightClickPaste(raw)`：需 `onRightClickPaste` 存在、`platform === "win32"`、
  `TERM_PROGRAM?.toLowerCase() !== "vscode"`、`!raw.release`、`raw.button === 2`；调用时 try/catch；命中返回 true
- `handleScrollToEndIndicatorMouseEvent(raw)`：`rect` 存在、!release、`(button & 32) === 0`、`(button & 3) === 0`、
  `raw.y === rect.row`、`rect.column <= raw.x < rect.column + rect.width` → `scrollToBottom()`; true
- `getScrollbarTargetAt(x, y, includeHiddenAuto=false)`：`hasOverlay() || !currentLayout` → undefined；
  对 `getScrollViewsAt` 每个 scrollView 取 box + geometry，命中 `x === geometry.column` 且
  `trackTop <= y < trackTop + trackHeight` → `{scrollView, geometry}`
- `setScrollbarHover(sv)`：变化时旧 `setScrollbarActive(false)`、新 `setScrollbarActive(true)`
- `updateScrollbarHover(x,y)` → `setScrollbarHover(getScrollbarTargetAt(x,y,true)?.scrollView)`
- `stopScrollbarHover()` / `stopScrollbarDrag()`
- `scrollScrollbarToPointer(sv, geom, pointerY, grabOffset)`：
  ```
  maxThumbOffset = trackHeight - thumbHeight
  thumbOffset = max(0, min(maxThumbOffset, pointerY - trackTop - grabOffset))
  scrollTop = maxThumbOffset === 0 ? 0 : round((thumbOffset / maxThumbOffset) * maxScrollTop)
  sv.scrollTo(scrollTop)
  ```
- `handleScrollbarMouseEvent(raw)`：
  ```
  拖动中：release → stopScrollbarDrag(); true；否则按当前几何 scrollScrollbarToPointer(...); true
  if (release || (button & 32) !== 0 || (button & 3) !== 0) return false
  target = getScrollbarTargetAt(x, y); 无 → false
  清空选择状态（stopSelectionAutoScroll、selectionPressActive=false、anchor/focus/granularity/initialRange
    /lastClick/pressedUrl/selectionDragged）；setScrollbarHover(target.scrollView)
  onThumb = y ∈ [thumbTop, thumbTop + thumbHeight)
  grabOffset = onThumb ? y - thumbTop : floor(thumbHeight / 2)
  if (!onThumb) scrollScrollbarToPointer(...)
  scrollbarDrag = { scrollView, grabOffset }; return true
  ```

#### 2.13.9 文本选择

`getScrollSelectionPoint(sv, x, y)`：
```
if (!currentLayout) return undefined
box = getScrollViewBox(currentLayout, sv); if (!box || box.rect.height <= 0 || box.clip.height <= 0) return undefined
visibleTop = max(0, box.rect.y, box.clip.y)
visibleBottom = min(terminal.rows - 1, box.rect.y + box.rect.height - 1, box.clip.y + box.clip.height - 1)
if (visibleBottom < visibleTop) return undefined
pointerRow = max(visibleTop, min(visibleBottom, y))
maxContentRow = max(0, (box.scrollContentLines?.length ?? 1) - 1)
return { row: max(0, min(maxContentRow, sv.scrollTop + pointerRow - box.rect.y)),
         col: max(0, min(box.rect.width - 1, x - box.rect.x)), scrollView: sv }
```

`getSelectionPoint(event, scrollView?)`：优先 scrollView 版；否则
`{ row: clamp(0, rows-1, event.y), col: clamp(0, columns-1, event.x) }`。

`getSelectionSourceLine(point)`：有 scrollView 且 currentLayout → `scrollContentLines[point.row] ?? ""`；
否则 `previousScreen[point.row] ?? ""`。

`getWordSelection(point)`：
```
line = stripTerminalSequences(getSelectionSourceLine(point))
segments = []; start = 0
for (segment of wordSegmenter.segment(line)) {
   end = start + visibleWidth(segment.segment)
   joiner = TERMINAL_WORD_SELECTION_JOINERS.has(segment.segment)
   segments.push({ start, end, selectable: segment.isWordLike === true || joiner, joiner })
   start = end
}
clickedSegmentIndex = segments.findIndex(s => point.col >= s.start && point.col < s.end)
if (clickedSegmentIndex < 0) return undefined
canJoin(left, right) = left.selectable && right.selectable && (left.joiner || right.joiner)
selectionStart = segments[i].start; selectionEnd = segments[i].end
向左扩展：while (index > 0 && canJoin(segments[index-1], segments[index])) { selectionStart = segments[index-1].start; index-- }
向右扩展：while (index < len-1 && canJoin(segments[index], segments[index+1])) { selectionEnd = segments[index+1].end; index++ }
return { start: { ...point, col: selectionStart },
         end:   { ...point, col: selectionEnd, boundary: true } }
```
（iTerm2 默认"word 字符"集合：`/ - + \ ~ _ .`，这些符号**本身可作为连接符**与相邻 word 合并。）

`getLineSelection(point)`：`{ start: {...point, col: 0}, end: {...point, col: visibleWidth(sourceLine), boundary: true} }`。

`updateSelectionFocus(point)`：
```
if (granularity === "character" || !selectionInitialRange) { selectionFocus = point; return }
range = granularity === "word" ? getWordSelection(point) : getLineSelection(point)
if (!range) return
initial = selectionInitialRange
targetBeforeInitial = range.start.row < initial.start.row
                   || (range.start.row === initial.start.row && range.start.col < initial.start.col)
if (targetBeforeInitial) { selectionAnchor = initial.end;  selectionFocus = range.start }
else                     { selectionAnchor = initial.start; selectionFocus = range.end }
```

`getClickCount(point, word)`：与 `getComponentClickCount` 同构，但比较 `row/scrollView/wordStart/wordEnd`；
`word` 为 undefined 时返回 1 且 `lastClick = undefined`。

`updateSelectionAutoScroll(event)`：
```
sv = selectionAnchor?.scrollView
if (!sv || !currentLayout) { stopSelectionAutoScroll(); return }
box = getScrollViewBox(currentLayout, sv)
if (!box || box.rect.height <= 0 || box.clip.height <= 0) { stopSelectionAutoScroll(); return }
visibleTop = max(0, box.rect.y, box.clip.y)
visibleBottom = min(rows-1, box.rect.y + box.rect.height - 1, box.clip.y + box.clip.height - 1)
selectionDragPointer = { x: event.x, y: event.y }
direction = event.y <= visibleTop ? -1 : event.y >= visibleBottom ? 1 : 0
if (direction === 0) { stopSelectionAutoScroll(); return }
if (selectionAutoScrollTimer) return
selectionAutoScrollTimer = setInterval(() => autoScrollSelection(), 50)
selectionAutoScrollTimer.unref()
```

`autoScrollSelection()`：
```
sv = selectionAnchor?.scrollView; pointer = selectionDragPointer; direction = selectionAutoScrollDirection
if (!sv || !pointer || direction === 0) { stopSelectionAutoScroll(); return }
remaining = sv.scrollBy(direction)
if (remaining === direction) { stopSelectionAutoScroll(); return }      // 无法再滚
point = getScrollSelectionPoint(sv, pointer.x, pointer.y)
if (point) updateSelectionFocus(point)
requestRender()
```

`handleSelectionMouseEvent(event)` **精确**：
```
button = event.button & 3
if (button !== 0 && !(event.release && button === 3)) return
anchorScrollView = selectionAnchor?.scrollView
point = getSelectionPoint(event, anchorScrollView)

【release】
if (event.release) {
   if (!selectionPressActive) return
   selectionPressActive = false; stopSelectionAutoScroll()
   if (!selectionAnchor) return
   updateSelectionFocus(point)
   isClick = !selectionDragged && selectionAnchor.scrollView === point.scrollView
             && selectionAnchor.row === point.row && selectionAnchor.col === point.col
   clickedUrl = isClick ? pressedUrl : undefined; pressedUrl = undefined
   if (clickedUrl && openUrl) { 清 anchor/focus；try openUrl(clickedUrl) catch {}；requestRender(); return }
   if (isClick) {
       clickEvent = createMouseEvent("click", event.button, event.x, event.y,
                                      { clickCount: lastClick?.count ?? 1 })
       overlay = dispatchMouseToOverlay(clickEvent)
       result = overlay.result ?? (overlay.hit ? undefined : dispatchMouseToLayout(clickEvent))
       if (result) { render = applyMouseDispatchResult(clickEvent, result); clearTextSelection();
                     if (render) requestRender(); return }
   }
   if (copyOnSelect) void copySelectionToClipboard()
   requestRender(); return
}

【motion】
if ((event.button & 32) !== 0) {
   if (!selectionPressActive || !selectionAnchor) return
   selectionDragged = true; lastClick = undefined; pressedUrl = undefined
   updateSelectionFocus(point); updateSelectionAutoScroll(event); requestRender(); return
}

【press】
stopSelectionAutoScroll(); selectionPressActive = true
sv = !hasOverlay() && currentLayout ? getScrollViewsAt(currentLayout, event.x, event.y)[0] : undefined
anchor = getSelectionPoint(event, sv)
word = getWordSelection(anchor)
clickCount = getClickCount(anchor, word)
range = clickCount === 2 ? word : clickCount === 3 ? getLineSelection(anchor) : undefined
selectionGranularity = range ? (clickCount === 2 ? "word" : "line") : "character"
selectionInitialRange = range
selectionAnchor = range?.start ?? anchor
selectionFocus  = range?.end ?? anchor
selectionDragged = false
pressedUrl = range ? undefined
   : getOsc8LinkAtColumn(previousScreen[clamp(0, rows-1, event.y)] ?? "",
                         clamp(0, columns-1, event.x))
requestRender()
```

`getSelectionBounds()`：
```
无 anchor/focus → undefined；两者 scrollView 不同 → undefined
anchorBeforeFocus = anchor.row < focus.row || (row 相等 && anchor.col < focus.col)
若 row 与 col 都相等 → undefined
return anchorBeforeFocus ? {start: anchor, end: focus} : {start: focus, end: anchor}
```

`getSelectionColumns(line, row, selection, minColumn = 0, maxColumn = visibleWidth(line))`：
```
lineWidth = visibleWidth(line); start = max(0, minColumn); end = min(lineWidth, maxColumn)
if (row === selection.start.row) start = getGraphemeCellRange(line, selection.start.col)?.start
                                       ?? min(selection.start.col, lineWidth)
if (row === selection.end.row)   end   = selection.end.boundary ? min(selection.end.col, lineWidth)
                                       : (getGraphemeCellRange(line, selection.end.col)?.end
                                          ?? min(selection.end.col + 1, lineWidth))
return { start: max(minColumn, start), end: min(maxColumn, end) }
```

`getActiveSelectionText()`：
```
selection = getSelectionBounds(); 无 → undefined
sourceLines = previousScreen
if (selection.start.scrollView) {
   if (!currentLayout) return undefined
   box = getScrollViewBox(currentLayout, selection.start.scrollView)
   if (!box?.scrollContentLines) return undefined
   sourceLines = box.scrollContentLines
}
lines = []
for (row = start.row; row <= end.row; row++) {
   line = sourceLines[row] ?? ""
   columns = getSelectionColumns(line, row, selection)
   lines.push(stripTerminalSequences(sliceByColumn(line, columns.start,
              max(0, columns.end - columns.start), true)).trimEnd())
}
text = lines.join("\n")
return text.length === 0 ? undefined : text
```

`copyTextToClipboard(text)`（async）**【逐字】**：
```
if (copySelection) {
   result = await copySelection(text)
   ok = result === true
   flash(ok ? "Copied!" : (typeof result === "string" ? result : "Copy failed"),
         ok ? undefined : 5000)
   return ok
}
terminal.write(`\x1b]52;c;${Buffer.from(text).toString("base64")}\x07`)
flash("Copied!")
return true
```
（`Buffer.from(text)` 默认 UTF-8。）

`clearTextSelection()`：stopSelectionAutoScroll、selectionPressActive=false，清 anchor/focus/
granularity/initialRange/pressedUrl/selectionDragged。

#### 2.13.10 搜索

`toggleSearch()`：
```
已激活 → closeSearch() 并 return
component = new AltScreenSearchComponent(query => updateSearchQuery(query), searchNavigationButtonStyle)
search = { component, index: new AltScreenSearchIndex(), query: "", matches: [], selectedIndex: -1,
           anchorRow: getPrimaryScrollView().scrollTop, selectionMode: "query" }
activeSearch = search
search.overlay = showOverlay(component, { anchor: "top-right", width: "40%", minWidth: 32, margin: 1 })
```

`closeSearch()`：`activeSearch` 存在 → 置 undefined、`overlay?.hide()`、requestRender。
`updateSearchQuery(query)`：无 search 或 query 未变 → return；`selected = matches[selectedIndex]`；
`anchorRow = selected?.segments[0]?.row ?? primary.scrollTop`；`query = query`；`selectionMode = "query"`；
`component.setResult(-1, 0)`；requestRender。
`navigateSearch(direction)`：`!search?.query` → return；`selectionMode = direction < 0 ? "previous" : "next"`；requestRender。

`getSearchNavigationDirectionAt(x, y)`：search + bounds 存在且点在 bounds 内 → `component.getNavigationDirectionAt(y-bounds.row, x-bounds.col)`。

`handleSearchMouseEvent(event)`：
```
无 search → false
direction = getSearchNavigationDirectionAt(event.x, event.y)
if (component.setHoveredNavigationDirection(direction)) requestRender()
if (direction === undefined || event.release || (event.button & 32) !== 0 || (event.button & 3) !== 0) return false
navigateSearch(direction); return true
```

`refreshSearch(layout)` → boolean（是否需要重新布局）：
```
search = activeSearch; 无 → false
sv = layout.primaryScrollView ?? implicitScrollView
box = getScrollViewBox(layout, sv); lines = box?.scrollContentLines
if (!lines || !search.query.trim()) {
   matches = []; selectedIndex = -1; selectedKey = undefined; selectionMode = "retain"
   component.setResult(-1, 0); return false
}
shouldRevealSelection = selectionMode !== "retain"
result = index.search(lines, query); matches = result.matches; search.matches = matches
if (!result.changed && selectionMode === "retain") return false

exactIndex = result.changed
   ? (search.selectedKey ? matches.findIndex(m => getAltScreenSearchMatchKey(m) === search.selectedKey) : -1)
   : search.selectedIndex
selectedIndex = -1
if (matches.length > 0) {
   if (selectionMode === "query") {
      // 二分：找第一个 segments[0].row >= anchorRow
      low = 0; high = matches.length
      while (low < high) { middle = low + floor((high-low)/2)
                           if ((matches[middle].segments[0]?.row ?? 0) < anchorRow) low = middle+1; else high = middle }
      selectedIndex = low < matches.length ? low : 0
   } else if (selectionMode === "next") {
      base = exactIndex >= 0 ? exactIndex : min(selectedIndex, matches.length - 1)
      selectedIndex = base < 0 ? 0 : (base + 1) % matches.length
   } else if (selectionMode === "previous") {
      base = exactIndex >= 0 ? exactIndex : min(selectedIndex, matches.length - 1)
      selectedIndex = base < 0 ? matches.length - 1 : (base - 1 + matches.length) % matches.length
   } else {
      selectedIndex = exactIndex >= 0 ? exactIndex : min(max(0, selectedIndex), matches.length - 1)
   }
}
search.selectedIndex = selectedIndex
search.selectedKey = selectedIndex >= 0 ? getAltScreenSearchMatchKey(matches[selectedIndex]) : undefined
search.selectionMode = "retain"
component.setResult(selectedIndex, matches.length)
if (!shouldRevealSelection) return false

selected = matches[selectedIndex]; firstSegment = selected?.segments[0]
lastSegment = selected?.segments[selected.segments.length - 1]
if (!box || !firstSegment || !lastSegment || sv.viewportHeight <= 0) return false
before = sv.scrollTop; visibleBottom = before + sv.viewportHeight - 1
target = before
if (firstSegment.row < before || lastSegment.row > visibleBottom) target = firstSegment.row - floor(sv.viewportHeight / 3)
sv.scrollTo(target, { disableFollow: true })
return sv.scrollTop !== before
```

#### 2.13.11 高亮与合成

`applySearchTextHighlight(text, current)`：
```
style = current ? searchCurrentMatchStyle : searchMatchStyle
result = ""; plainStart = 0; index = 0
while (index < text.length) {
   ansi = extractAnsiCode(text, index)
   if (!ansi) { index++; continue }
   if (index > plainStart) result += style(text.slice(plainStart, index))
   result += ansi.code; index += ansi.length; plainStart = index
}
if (plainStart < text.length) result += style(text.slice(plainStart))
return result
```

`applySearchHighlights(screen, layout)`：
```
search 无 / selectedIndex < 0 / matches 空 → screen
sv = layout.primaryScrollView ?? implicitScrollView；box = getScrollViewBox(layout, sv)；无 → screen
rangesByRow = Map<number, SearchHighlightRange[]>
scrollbarColumn = getScrollbarGeometry(box)?.column
minRow = max(0, box.rect.y, box.clip.y)
maxRow = min(screen.length, box.rect.y + box.rect.height, box.clip.y + box.clip.height)
minColumn = max(0, box.rect.x, box.clip.x)
maxColumn = min(terminal.columns, box.rect.x + box.rect.width, box.clip.x + box.clip.width,
                scrollbarColumn ?? +Infinity)
minContentRow = sv.scrollTop + minRow - box.rect.y
maxContentRow = sv.scrollTop + maxRow - box.rect.y - 1
low = 二分找第一个 lastRow >= minContentRow
for (matchIndex = low; matchIndex < matches.length; matchIndex++) {
   match = matches[matchIndex]
   if ((match.segments[0]?.row ?? 0) > maxContentRow) break
   for (segment of match.segments) {
      row = box.rect.y + segment.row - sv.scrollTop
      if (row < minRow || row >= maxRow) continue
      startCol = max(minColumn, box.rect.x + segment.startCol)
      endCol   = min(maxColumn, box.rect.x + segment.endCol)
      if (endCol <= startCol) continue
      rangesByRow.get(row) ??= []; push({ startCol, endCol, current: matchIndex === search.selectedIndex })
   }
}
result = [...screen]
for ([row, ranges] of rangesByRow) {
   line = result[row] ?? ""; if (isImageLine(line)) continue
   lineWidth = visibleWidth(line)
   for (range of ranges.sort((a,b) => b.startCol - a.startCol)) {       // 从右往左
      startCol = min(range.startCol, lineWidth); endCol = min(range.endCol, lineWidth)
      if (endCol <= startCol) continue
      before = sliceByColumn(line, 0, startCol, true)
      highlighted = sliceByColumn(line, startCol, endCol - startCol, true)
      after = sliceByColumn(line, endCol, max(0, lineWidth - endCol), true)
      line = `${before}${applySearchTextHighlight(highlighted, range.current)}${after}`
   }
   result[row] = line
}
return result
```

`applySelectionHighlight(text)` **【逐字】**：
```
result = "\x1b[7m"; index = 0
while (index < text.length) {
   ansi = extractAnsiCode(text, index)
   if (!ansi) { result += text[index]; index++; continue }
   result += ansi.code
   if (ansi.code.endsWith("m")) result += "\x1b[7m"        // SGR 之后重新进入反显
   index += ansi.length
}
return `${result}\x1b[27m`
```

`applySelection(screen, layout = currentLayout)`：
```
selection = getSelectionBounds(); 无 → screen
screenSelection = selection; minRow=0; maxRow=screen.length-1; minColumn=0; maxColumn=terminal.columns
if (selection.start.scrollView) {
   if (!layout) return screen
   box = getScrollViewBox(layout, selection.start.scrollView); 无 → screen
   minRow = max(0, box.rect.y, box.clip.y)
   maxRow = min(screen.length-1, box.rect.y + box.rect.height - 1, box.clip.y + box.clip.height - 1)
   minColumn = max(0, box.rect.x, box.clip.x)
   maxColumn = min(terminal.columns, box.rect.x + box.rect.width, box.clip.x + box.clip.width)
   screenSelection = { start: { ...selection.start, row: box.rect.y + selection.start.row - sv.scrollTop,
                                col: box.rect.x + selection.start.col },
                       end:   { ...selection.end,   row: box.rect.y + selection.end.row   - sv.scrollTop,
                                col: box.rect.x + selection.end.col } }
}
return screen.map((line, row) => {
   if (row < minRow || row > maxRow || row < screenSelection.start.row || row > screenSelection.end.row
       || isImageLine(line)) return line
   lineWidth = visibleWidth(line)
   columns = getSelectionColumns(line, row, screenSelection, minColumn, maxColumn)
   if (columns.end <= columns.start) return line
   before = sliceByColumn(line, 0, columns.start, true)
   selected = sliceByColumn(line, columns.start, columns.end - columns.start, true)
   after = sliceByColumn(line, columns.end, max(0, lineWidth - columns.end), true)
   return `${before}${applySelectionHighlight(selected)}${after}`
})
```

`compositeScrollToEndIndicator(screen, layout, width)`：
```
scrollToEndIndicatorRect = undefined
sv = layout.primaryScrollView ?? implicitScrollView
if (!scrollToEndIndicator || !sv.followEnd || sv.isFollowingEnd) return screen
box = getScrollViewBox(layout, sv); clip = box?.clip
if (!clip || clip.width <= 0 || clip.height <= 0) return screen
row = clip.y + clip.height - 1
if (row >= screen.length || isImageLine(screen[row] ?? "")) return screen
scrollbarColumn = box ? getScrollbarGeometry(box)?.column : undefined
label = truncateToWidth(scrollToEndIndicator(), clip.width, "")
labelWidth = visibleWidth(label)
column = clip.x + floor((clip.width - labelWidth) / 2)
rightEdge = scrollbarColumn ?? clip.x + clip.width
availableWidth = max(0, rightEdge - column)
text = truncateToWidth(label, availableWidth, "")
textWidth = visibleWidth(text)
if (textWidth === 0) return screen
result = [...screen]
result[row] = compositeTuiLine(result[row] ?? "", text, column, textWidth, width)
scrollToEndIndicatorRect = { row, column, width: textWidth }
return result
```

`compositeFlashes(screen, width, height)`：
```
flashLines = flashes.render(width).slice(-height)
if (flashLines.length === 0) return screen
result = [...screen]; while (result.length < height) result.push("")
for (row = 0; row < flashLines.length; row++) {
   line = flashLines[row]; flashWidth = visibleWidth(line)
   if (flashWidth === 0) continue
   result[row] = compositeTuiLine(result[row] ?? "", line, width - flashWidth, flashWidth, width)
}
return result
```

#### 2.13.12 `doRender()` — **逐句**

```
if (stopped || !altScreenActive) return
width = max(1, terminal.columns); height = max(1, terminal.rows)
root = layoutRoot ?? implicitScrollView
nextLayout = renderLayoutFrame(root, width, height, () => requestRender())
if (refreshSearch(nextLayout)) nextLayout = renderLayoutFrame(root, width, height, () => requestRender())
screen = resolveFakeCursors(nextLayout.lines.map(line => line.replace(OSC133_ZONE_PREFIX, "")))
screen = applySearchHighlights(screen, nextLayout)
screen = compositeScrollToEndIndicator(screen, nextLayout, width)
screen = compositeOverlays(screen, width, height)
if (screen.length > height) screen = screen.slice(screen.length - height)
screen = applySelection(screen, nextLayout)
screen = compositeFlashes(screen, width, height)

cursorPos = extractCursorPosition(screen, height)
screen = applyLineResets(screen).map(line =>
   (isImageLine(line) || visibleWidth(line) <= width) ? line : sliceByColumn(line, 0, width, true))

fullRedraw = previousScreen.length === 0 || previousScreenWidth !== width || previousScreenHeight !== height
changedRows = screen.map((line, row) => line !== previousScreen[row])
imageAnchorsNeedRedraw = screen.some((line, row) =>
   changedRows[row] && (isImageLine(line) || isImageLine(previousScreen[row] ?? "")))
isWezTerm = Boolean(process.env.WEZTERM_PANE) || TERM_PROGRAM?.toLowerCase() === "wezterm"
imageCellsNeedRedraw = !imageAnchorsNeedRedraw && isWezTerm && imageProtocol === "kitty"
   && changedRows.some(Boolean)
   && screen.some((line, row) => {
        placementRows = getKittyImagePlacementRows(line); if (placementRows === undefined) return false
        for (coveredRow = row; coveredRow < row + placementRows; coveredRow++)
           if (changedRows[coveredRow]) return true
        return false
      })
imagesNeedRedraw = imageAnchorsNeedRedraw || imageCellsNeedRedraw
redrawImages = fullRedraw || imagesNeedRedraw
hadUploadedKittyImages = uploadedKittyImages.size > 0
preparedKittyScreen = (redrawImages && imageProtocol === "kitty")
   ? prepareKittyScreen(screen) : { lines: screen, evictedImageDeletion: "" }

buffer = BEGIN_SYNCHRONIZED_OUTPUT
if (fullRedraw) {
   fullRedrawCount += 1
   clearImages = (imageProtocol === "kitty" && hadUploadedKittyImages) ? deleteAllKittyPlacements()
                                                                     : deleteKittyImages()
   buffer += `${clearImages}\x1b[2J`
} else if (imagesNeedRedraw) {
   if (imageProtocol === "iterm2") buffer += "\x1b[2J"
   else if (imageProtocol === "kitty") buffer += deleteAllKittyPlacements()
}
buffer += preparedKittyScreen.evictedImageDeletion

drawKittyImagesLast = redrawImages && imageProtocol === "kitty" && screen.some(isImageLine) && isWezTerm
if (drawKittyImagesLast) {
   第一遍：对需要更新的行 buffer += `\x1b[${row+1};1H\x1b[2K`
   第二遍：非图片行 buffer += `\x1b[${row+1};1H${preparedKittyScreen.lines[row] ?? ""}`
   第三遍：图片行   buffer += `\x1b[${row+1};1H${preparedKittyScreen.lines[row] ?? ""}`
   （每遍都跳过"非 fullRedraw && !imagesNeedRedraw && screen[row] === previousScreen[row]"的行）
} else {
   for (row = 0; row < height; row++) {
      if (!fullRedraw && !imagesNeedRedraw && screen[row] === previousScreen[row]) continue
      buffer += `\x1b[${row+1};1H\x1b[2K${preparedKittyScreen.lines[row] ?? ""}`
   }
}

if (cursorPos) {
   buffer += `\x1b[${cursorPos.row+1};${min(width, cursorPos.col)+1}H`
   buffer += getShowHardwareCursor() ? "\x1b[?25h" : "\x1b[?25l"
} else buffer += "\x1b[?25l"
buffer += END_SYNCHRONIZED_OUTPUT
terminal.write(buffer)

previousScreen = screen; previousScreenWidth = width; previousScreenHeight = height
currentLayout = nextLayout
```

公开方法（视口控制）：
```ts
get viewportTop(): number                         // primary.scrollTop
get isFollowingOutput(): boolean                  // primary.isFollowingEnd
setWheelScrollLines(lines) → wheelScroll.setLines(lines)
getCopyOnSelect() / setCopyOnSelect(enabled)
hasActiveSelection(): boolean                     // getActiveSelectionText() !== undefined
async copyActiveSelectionToClipboard(): Promise<boolean>
resetTextSelection(): void                        // clearTextSelection() + lastClick = undefined
getScreenLines(): string[]                        // [...previousScreen]
setLayoutRoot(component)                          // 相同则 return；否则赋值、currentLayout=undefined、requestRender()
scrollBy(lines) / scrollToTop() / scrollToBottom()（后两者调 scrollToStart/scrollToEnd 并 requestRender）
flash(message, durationMs?)
```

`scrollToPrompt(direction)`：
```
if (!currentLayout) return
sv = getPrimaryScrollView(); lines = getScrollViewBox(currentLayout, sv)?.scrollContentLines
if (!lines) return
for (row = sv.scrollTop + direction; row >= 0 && row < lines.length; row += direction) {
   if (!OSC133_PROMPT_START.test(lines[row] ?? "")) continue
   sv.scrollTo(row); requestRender(); return
}
```

---

### 2.14 `src/components/alt-screen-flash.ts`（51 行，被 alt-screen 使用）

```ts
const DEFAULT_DURATION_MS = 1000;
class AltScreenFlashContainer implements Component {
  private readonly entries: FlashEntry[] = [];      // { id, message, timer }
  private nextId = 0;
  constructor(private readonly requestRender: () => void)
  flash(message, durationMs = DEFAULT_DURATION_MS): void {
     id = nextId++
     timer = setTimeout(() => { index = entries.findIndex(e => e.id === id); if (index === -1) return
                                entries.splice(index, 1); requestRender() }, max(0, durationMs))
     // 注意顺序：先设 timer 并 unref，再 push 进 entries，最后 requestRender()。
     // 先 push 后建 timer 会让 0ms 定时器有机会在 push 之前删除不到条目
     timer.unref()
     entries.push({ id, message, timer }); requestRender()
  }
  dispose(): void { 清理所有 timer；entries.length = 0 }
  invalidate(): void {}
  render(width): string[] {
     return entries.map(e => {
        message = truncateToWidth(` ${e.message} `, width, "")
        return `\x1b[7m${message}\x1b[27m`
     })
  }
}
```

---

### 2.15 `src/terminal-image.ts`（776 行）

> 若由 `tui-terminal` 单元实现，取其实现；本节钉死 tui-core 渲染器所依赖的行为。

#### 2.15.1 类型与全局状态

```ts
export type ImageProtocol = "kitty" | "iterm2" | null;
export interface TerminalCapabilities { images: ImageProtocol; trueColor: boolean; hyperlinks: boolean; }
export interface CellDimensions { widthPx: number; heightPx: number; }
export interface ImageDimensions { widthPx: number; heightPx: number; }
export interface ImageRenderOptions {
  maxWidthCells?: number; maxHeightCells?: number; preserveAspectRatio?: boolean;
  imageId?: number; moveCursor?: boolean;
}

let cachedCapabilities: TerminalCapabilities | null = null;
let capabilityOverrides: Partial<TerminalCapabilities> = {};
let cellDimensions: CellDimensions = { widthPx: 9, heightPx: 18 };     // 默认
const KITTY_PREFIX = "\x1b_G";
const ITERM2_PREFIX = "\x1b]1337;File=";
```

#### 2.15.2 能力探测（**分支顺序不可变**）

`probeTmuxHyperlinks()`：
```js
execSync("tmux display-message -p '#{client_termfeatures}'",
         { encoding: "utf8", timeout: 250, stdio: ["ignore","pipe","ignore"] })
  .split(",").map(f => f.trim()).includes("hyperlinks")
异常 → false
```

`detectCapabilitiesFromEnvironment(probe)`：
```
termProgram = TERM_PROGRAM?.toLowerCase() || ""
terminalEmulator = TERMINAL_EMULATOR?.toLowerCase() || ""
term = TERM?.toLowerCase() || ""
colorTerm = COLORTERM?.toLowerCase() || ""
hasTrueColorHint = colorTerm === "truecolor" || colorTerm === "24bit" || term.endsWith("-direct")
isWindowsConsole = platform === "win32"

1. if (TMUX || term.startsWith("tmux")) → { images: null, trueColor: hasTrueColorHint, hyperlinks: probe() }
2. if (term.startsWith("screen"))      → { images: null, trueColor: hasTrueColorHint, hyperlinks: false }
3. if (termProgram === "herdr")        → { images: "kitty",  trueColor: hasTrueColorHint, hyperlinks: true }
4. if (termProgram === "rex")          → { images: "kitty",  trueColor: true, hyperlinks: true }
5. if (KITTY_WINDOW_ID || termProgram === "kitty")
                                       → { images: "kitty",  trueColor: true, hyperlinks: true }
6. if (termProgram === "ghostty" || term.includes("ghostty") || GHOSTTY_RESOURCES_DIR)
                                       → { images: "kitty",  trueColor: true, hyperlinks: true }
7. if (WEZTERM_PANE || termProgram === "wezterm")
                                       → { images: "kitty",  trueColor: true, hyperlinks: true }
8. if (termProgram === "warpterminal" || WARP_SESSION_ID || WARP_TERMINAL_SESSION_UUID)
                                       → { images: "kitty",  trueColor: true, hyperlinks: true }
9. if (ITERM_SESSION_ID || termProgram === "iterm.app")
                                       → { images: "iterm2", trueColor: true, hyperlinks: true }
10. if (WT_SESSION)                    → { images: null, trueColor: true, hyperlinks: true }
11. if (termProgram ∈ {alacritty, vscode, zed})
                                       → { images: null, trueColor: true, hyperlinks: true }
12. if (terminalEmulator === "jetbrains-jediterm")
                                       → { images: null, trueColor: true, hyperlinks: false }
13. if (isWindowsConsole)              → { images: null, trueColor: true, hyperlinks: false }
14. 默认                               → { images: null, trueColor: hasTrueColorHint, hyperlinks: false }
```

`parseBooleanCapabilityOverride(v)`：`"1"` → true；`"0"` → false；其它 → undefined。

`detectCapabilities(probe = probeTmuxHyperlinks)`：
```
hyperlinks = parseBooleanCapabilityOverride(KIANA_HYPERLINKS)
detected = detectCapabilitiesFromEnvironment(hyperlinks === undefined ? probe : () => hyperlinks)
imageProtocol = KIANA_IMAGE_PROTOCOL?.toLowerCase()
images = (imageProtocol ∈ {"kitty","iterm2"}) ? imageProtocol
       : (imageProtocol ∈ {"none","0"}) ? null : undefined
trueColor = parseBooleanCapabilityOverride(KIANA_TRUE_COLOR)
return { ...detected,
         ...(images !== undefined ? { images } : {}),
         ...(trueColor !== undefined ? { trueColor } : {}),
         ...(hyperlinks !== undefined ? { hyperlinks } : {}) }
```

`getCapabilities()`：
```
if (!cachedCapabilities) {
   hyperlinks = capabilityOverrides.hyperlinks
   cachedCapabilities = { ...detectCapabilities(hyperlinks === undefined ? undefined : () => hyperlinks),
                          ...capabilityOverrides }
}
return cachedCapabilities
```

`getTerminalColorMode(caps = getCapabilities())` → `caps.trueColor ? "truecolor" : "256color"`。
`resetCapabilitiesCache()` → `cachedCapabilities = null`。
`setCapabilityOverrides(overrides)`：三个字段与当前相同 → return；否则替换并清缓存。
`setCapabilities(caps)`：直接设缓存（测试用）。
`getCellDimensions()` / `setCellDimensions(dims)`。

#### 2.15.3 图片行与 ID

```ts
export function isImageLine(line: string): boolean {
  if (line.startsWith("\x1b_G") || line.startsWith("\x1b]1337;File=")) return true;
  return line.includes("\x1b_G") || line.includes("\x1b]1337;File=");
}
export function allocateImageId(): number {
  return Math.floor(Math.random() * 0xfffffffe) + 1;     // [1, 0xfffffffe]
}
```
（回归测试 `bug-regression-isimageline-startswith-bug.test.ts` 覆盖：`line.startsWith` 在
含 ANSI 前缀/超长行时的历史 bug。）

#### 2.15.4 Kitty 编码

```ts
export function encodeKitty(base64Data, options: {columns?, rows?, imageId?, moveCursor?} = {}): string {
  const CHUNK_SIZE = 4096;
  const params = ["a=T", "f=100", "q=2"];
  if (options.moveCursor === false) params.push("C=1");
  if (options.columns) params.push(`c=${options.columns}`);
  if (options.rows) params.push(`r=${options.rows}`);
  if (options.imageId) params.push(`i=${options.imageId}`);
  if (base64Data.length <= CHUNK_SIZE) return `\x1b_G${params.join(",")};${base64Data}\x1b\\`;
  const chunks = []; let offset = 0; let isFirst = true;
  while (offset < base64Data.length) {
     const chunk = base64Data.slice(offset, offset + CHUNK_SIZE);
     const isLast = offset + CHUNK_SIZE >= base64Data.length;
     if (isFirst) { chunks.push(`\x1b_G${params.join(",")},m=1;${chunk}\x1b\\`); isFirst = false }
     else if (isLast) chunks.push(`\x1b_Gm=0;${chunk}\x1b\\`);
     else chunks.push(`\x1b_Gm=1;${chunk}\x1b\\`);
     offset += CHUNK_SIZE;
  }
  return chunks.join("");
}

export function deleteKittyImage(imageId)   { return `\x1b_Ga=d,d=I,i=${imageId},q=2\x1b\\`; }
export function deleteAllKittyImages()      { return "\x1b_Ga=d,d=A,q=2\x1b\\"; }
export function deleteAllKittyPlacements()  { return "\x1b_Ga=d,d=a,q=2\x1b\\"; }
```

`encodeITerm2(base64Data, {width?, height?, name?, preserveAspectRatio?, inline?})`：
```
params = [`inline=${options.inline !== false ? 1 : 0}`,
          `size=${Buffer.byteLength(base64Data, "base64")}`]
if (width !== undefined) params.push(`width=${width}`)
if (height !== undefined) params.push(`height=${height}`)
if (name) params.push(`name=${Buffer.from(name).toString("base64")}`)
if (preserveAspectRatio === false) params.push("preserveAspectRatio=0")
return `\x1b]1337;File=${params.join(";")}:${base64Data}\x07`
```
（`Buffer.byteLength(base64, "base64")` = 解码后字节数 = `floor(len*3/4) - padding`。）

#### 2.15.5 Kitty 元数据注册表

```ts
const kittyImageMetadata = new Map<number, RegisteredKittyImageMetadata>();
let kittyTransmissionGeneration = 0;
let maxKittyImageRows = 0;                 // 单调不减

export function registerKittyImageMetadata(metadata: KittyImageMetadata): void {
  kittyTransmissionGeneration += 1;
  maxKittyImageRows = Math.max(maxKittyImageRows, metadata.rows);
  kittyImageMetadata.delete(metadata.imageId);            // 重复 id：先删再插（更新插入顺序）
  kittyImageMetadata.set(metadata.imageId, { ...metadata, transmissionGeneration: kittyTransmissionGeneration });
  if (kittyImageMetadata.size > 1000) {
     oldest = kittyImageMetadata.keys().next().value
     if (oldest !== undefined) kittyImageMetadata.delete(oldest)
  }
}
export function getMaxKittyImageRows(): number { return maxKittyImageRows; }
```
（`maxKittyImageRows` **单调不减**；`resetCapabilitiesCache` 不会重置它。）

控件解析：
```ts
getRegisteredKittyImageMetadataFromControls(controls):
   id = /(?:^|,)i=(\d+)(?:,|$)/.exec(controls)?.[1]
   id === undefined ? undefined : kittyImageMetadata.get(parseInt(id))

getRegisteredKittyImageMetadata(line):
   controls = /\x1b_G([^;]*);/.exec(line)?.[1]
   controls === undefined ? undefined : getRegisteredKittyImageMetadataFromControls(controls)

getExplicitKittyImageRows(controls):
   v = /(?:^|,)r=(\d+)(?:,|$)/.exec(controls)?.[1]
   v === undefined ? undefined : (rows > 0 ? rows : undefined)

getKittyImageRowsFromControls(controls, fallback) = getExplicitKittyImageRows(controls) ?? fallback

export function getKittyImageMetadata(line): KittyImageMetadata | undefined   // 公共副本（无 generation）
const KITTY_PLACEMENT_CONTROL_KEYS = new Set(["i","p","x","y","w","h","X","Y","c","r","C","U","z","P","Q","H","V"]);

export function getKittyImagePlacementRows(line): number | undefined {
  controls = /\x1b_G([^;]*);/.exec(line)?.[1]; 无 → undefined
  explicit = getExplicitKittyImageRows(controls); if (explicit !== undefined) return explicit
  return getRegisteredKittyImageMetadataFromControls(controls)?.rows
}
```

`getKittyImagePlacement(line)` **精确**：
```
match = /\x1b_G([^;]*);/.exec(line); 无 → undefined
metadata = getRegisteredKittyImageMetadataFromControls(match[1]); 无 → undefined
commandStart = match.index; commandControls = match[1]
while (true) {
   terminator = line.indexOf("\x1b\\", commandStart + 3)     // KITTY_PREFIX.length === 3
   if (terminator === -1) return undefined
   transmissionEnd = terminator + 2
   if (!/(?:^|,)m=1(?:,|$)/.test(commandControls)) break
   commandStart = transmissionEnd
   if (!line.startsWith("\x1b_G", commandStart)) return undefined
   controlsEnd = line.indexOf(";", commandStart + 3); if (controlsEnd === -1) return undefined
   commandControls = line.slice(commandStart + 3, controlsEnd)
}
controls = match[1].split(",").filter(c => KITTY_PLACEMENT_CONTROL_KEYS.has(c.split("=",1)[0] ?? ""))
sequence = `\x1b_Ga=p,q=2,${controls.join(",")}\x1b\\`
return { imageId: metadata.imageId, transmissionGeneration: metadata.transmissionGeneration,
         transmissionBytes: transmissionEnd - match.index,
         estimatedDecodedBytes: metadata.widthPx * metadata.heightPx * 4,
         rows: getKittyImageRowsFromControls(match[1], metadata.rows),
         sequence,
         replacementLine: `${line.slice(0, match.index)}${sequence}${line.slice(transmissionEnd)}` }
```

`cropKittyImageLine(line, hiddenRows, visibleRows)`：
```
metadata = getKittyImageMetadata(line); match = /\x1b_G([^;]*);/.exec(line)
if (!metadata || !match || hiddenRows < 0 || hiddenRows >= metadata.rows || visibleRows <= 0) return line
croppedRows = min(visibleRows, metadata.rows - hiddenRows)
if (hiddenRows === 0 && croppedRows === metadata.rows) return line
sourceY    = floor((metadata.heightPx * hiddenRows) / metadata.rows)
sourceEnd  = ceil((metadata.heightPx * (hiddenRows + croppedRows)) / metadata.rows)
sourceHeight = max(1, min(metadata.heightPx, sourceEnd) - sourceY)
controls = match[1].split(",").filter(c => !/^[yhr]=/.test(c))
controls.push(`y=${sourceY}`, `h=${sourceHeight}`, `r=${croppedRows}`)
return `${line.slice(0, match.index)}\x1b_G${controls.join(",")};${line.slice(match.index + match[0].length)}`
```

#### 2.15.6 图片尺寸计算

```ts
function chooseLessDistortedCellCount(upperCount, idealCount): number {
  if (upperCount <= 1) return upperCount;
  lowerCount = upperCount - 1
  upperDistortion = max(upperCount / idealCount, idealCount / upperCount)
  lowerDistortion = max(lowerCount / idealCount, idealCount / lowerCount)
  return lowerDistortion < upperDistortion ? lowerCount : upperCount
}

export function calculateImageCellSize(imageDimensions, maxWidthCells, maxHeightCells?,
                                       cellDimensions = {widthPx:9, heightPx:18},
                                       optimizeAspectRatio = false): ImageCellSize {
  maxWidth = max(1, floor(maxWidthCells))
  maxHeight = maxHeightCells === undefined ? undefined : max(1, floor(maxHeightCells))
  imageWidth = max(1, imageDimensions.widthPx); imageHeight = max(1, imageDimensions.heightPx)
  widthScale = (maxWidth * cell.widthPx) / imageWidth
  heightScale = maxHeight === undefined ? widthScale : (maxHeight * cell.heightPx) / imageHeight
  scale = min(widthScale, heightScale)
  scaledWidthPx = imageWidth * scale; scaledHeightPx = imageHeight * scale
  columns = max(1, min(maxWidth, ceil(scaledWidthPx / cell.widthPx)))
  heightRows = scaledHeightPx / cell.heightPx
  rows = max(1, ceil(heightRows))
  if (maxHeight !== undefined) rows = min(maxHeight, rows)
  if (!optimizeAspectRatio) return { columns, rows }
  if (widthScale <= heightScale) {
     idealRows = (columns * cell.widthPx * imageHeight) / (imageWidth * cell.heightPx)
     rows = chooseLessDistortedCellCount(rows, idealRows)
  } else {
     idealColumns = (rows * cell.heightPx * imageWidth) / (imageHeight * cell.widthPx)
     columns = chooseLessDistortedCellCount(columns, idealColumns)
  }
  return { columns, rows }
}

export function calculateImageRows(imageDimensions, targetWidthCells,
                                   cellDimensions = {widthPx:9, heightPx:18}): number {
  return calculateImageCellSize(imageDimensions, targetWidthCells, undefined, cellDimensions).rows
}
```

#### 2.15.7 尺寸头解析（base64 → 二进制）

- `getPngDimensions`：`Buffer.from(b64, "base64")`；`len < 24` → null；签名 `89 50 4E 47`；大端 `readUInt32BE(16)`/`(20)`；异常 → null
- `getJpegDimensions`：`len < 2` → null；`FF D8`；从 `offset=2` 循环，**循环条件逐字为
  `while (offset < buffer.length - 9)`**（不是 `offset < len`；这保证进入循环体后
  `readUInt16BE(offset+7)` 一定在界内，也意味着尾部 9 字节内的 marker 永不解析——Rust 侧若写成
  `while offset < len` 会多走几轮并在 `readUInt16BE` 上越界 panic 或产生不同结果）：
  非 `0xFF` → offset++；`marker ∈ [0xC0,0xC2]` → `readUInt16BE(offset+5)`（height）、`readUInt16BE(offset+7)`（width）；
  `offset + 3 >= len` → null；`length = readUInt16BE(offset+2)`；`length < 2` → null；`offset += 2 + length`；
  循环结束后 return null
- `getGifDimensions`：`len < 10` → null；`buffer.slice(0,6).toString("ascii") ∈ {"GIF87a","GIF89a"}`；
  `readUInt16LE(6)`/`readUInt16LE(8)`
- `getWebpDimensions`：`len < 30` → null；`RIFF`（0..4）、`WEBP`（8..12）；
  `chunk = slice(12,16).toString("ascii")`，每个分支内还有**各自的冗余长度复检**（源码确实保留了它们，
  行为上被外层的 `< 30` 覆盖，但 Rust 侧若省略不影响结果，若保留须逐字一致）：
  `"VP8 "` → 分支内 `len < 30` → null；`readUInt16LE(26) & 0x3fff`、`readUInt16LE(28) & 0x3fff`
  `"VP8L"` → 分支内 `len < 25` → null；`bits = readUInt32LE(21)`；`width = (bits & 0x3fff) + 1`；`height = ((bits >> 14) & 0x3fff) + 1`
  `"VP8X"` → 分支内 `len < 30` → null；`width = (b[24] | b[25]<<8 | b[26]<<16) + 1`；`height = (b[27] | b[28]<<8 | b[29]<<16) + 1`
  其它 → null
- `getImageDimensions(b64, mime)`：按 mime 分派（png/jpeg/gif/webp），其它 → null

#### 2.15.8 `renderImage` / `hyperlink` / `imageFallback`

```ts
export function renderImage(base64Data, imageDimensions, options: ImageRenderOptions = {})
  : { sequence: string; columns: number; rows: number; imageId?: number } | null {
  caps = getCapabilities(); if (!caps.images) return null
  maxWidth = options.maxWidthCells ?? 80
  size = calculateImageCellSize(imageDimensions, maxWidth, options.maxHeightCells,
                                getCellDimensions(), caps.images === "kitty")
  if (caps.images === "kitty") {
     if (options.imageId !== undefined) registerKittyImageMetadata({
        imageId: options.imageId, columns: size.columns, rows: size.rows,
        widthPx: imageDimensions.widthPx, heightPx: imageDimensions.heightPx })
     sequence = encodeKitty(base64Data, { columns: size.columns, rows: size.rows,
                                          imageId: options.imageId, moveCursor: options.moveCursor })
     return { sequence, columns: size.columns, rows: size.rows, imageId: options.imageId }
  }
  if (caps.images === "iterm2") {
     sequence = encodeITerm2(base64Data, { width: size.columns, height: "auto",
                                           preserveAspectRatio: options.preserveAspectRatio ?? true })
     return { sequence, columns: size.columns, rows: size.rows }
  }
  return null
}

export function hyperlink(text, url): string { return `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\`; }

export function imageFallback(mimeType, dimensions?, filename?): string {
  parts = []
  if (filename) {
     display = shortenImagePath(filename)      // ~ 前缀压缩（home === filename 或 `${home}/` / `${home}\\` 前缀）
     if (getCapabilities().hyperlinks && isAbsolute(filename))
        parts.push(hyperlink(display, pathToFileURL(filename).href))
     else parts.push(display)
  }
  parts.push(`[${mimeType}]`)
  if (dimensions) parts.push(`${dimensions.widthPx}x${dimensions.heightPx}`)
  return `[Image: ${parts.join(" ")}]`
}
```
**【逐字】**：`[Image: ${parts.join(" ")}]`、`[${mimeType}]`、`${w}x${h}`。
`pathToFileURL(filename).href` 的百分号编码规则需与 Rust `url::Url::from_file_path` 对齐（见 §6）。

---

### 2.16 Native 扩展（`native/**`）

#### 2.16.1 总体架构

三个平台各有独立 `.node` 预编译产物（`native/<platform>/prebuilds/<platform>-<arch>/<platform>-platform[-x11].node`），
**不链接 Node 头文件**：`napi.h` 在运行时用 `dlsym`（POSIX）/`GetProcAddress`（Win32）解析 `napi_*` 符号。
导出函数 `napi_register_module_v1` 是 Node 查找的入口。

`native/napi.h`（91 行，**逐字**要点）：

```c
// 注释（逐字）：Resolve the small N-API surface we use from Node or Bun, without Node headers
//               or a link-time dependency on a particular runtime.
#include <stdbool.h> <stddef.h> <stdint.h>
#ifdef _WIN32
  #include <windows.h>
  #define KIANA_NAPI_CALL __cdecl
  #define KIANA_NAPI_EXPORT __declspec(dllexport)
#else
  #include <dlfcn.h>
  #define KIANA_NAPI_CALL
  #define KIANA_NAPI_EXPORT __attribute__((visibility("default")))
#endif
#define NAPI_AUTO_LENGTH ((size_t)-1)

typedef void* napi_env;  typedef void* napi_value;  typedef void* napi_callback_info;
typedef void* napi_async_work;  typedef void* napi_deferred;
typedef void (KIANA_NAPI_CALL *napi_async_execute_callback)(napi_env, void*);
typedef void (KIANA_NAPI_CALL *napi_async_complete_callback)(napi_env, int, void*);
// 其余约 20 个 napi_* 函数指针 typedef（create_function / create_string_utf8 / create_string_utf16 /
// create_string_latin1 / create_array / set_element / set_named_property / create_buffer_copy /
// create_promise / settle_deferred / create_error / throw_error / get_boolean / get_cb_info /
// get_value_string_utf8 / get_value_string_utf16 / get_undefined / get_null / create_async_work /
// queue_async_work / delete_async_work）

static void* node_symbol(const char* name);   // Win32: GetModuleHandleA(0) -> GetProcAddress，
                                              //        再回退 GetModuleHandleA("node.dll")；
                                              // POSIX: dlsym(RTLD_DEFAULT, name)
static napi_value undefined_value(napi_env env);   // napi_get_undefined
static napi_value null_value(napi_env env);        // napi_get_null
static napi_value fail(napi_env env, const char* message);   // napi_throw_error，返回 undefined
static void set_function_export(napi_env env, napi_value exports, const char* name, napi_callback callback);
```

**注意**：本规格早期版本假设存在 `KIANA_NAPI_CALL(env, call)` 宏；实际的 `KIANA_NAPI_CALL` 是**调用约定宏**
（`__cdecl`，POSIX 下为空），不是错误检查宏。源码中并无 `napi_ok` 检查宏。
Rust 侧不需要复刻 N-API：直接用 `napi-rs` 或在 `kiana-tui-native` crate 中提供**等价能力**，
并按 §8 的 trait 抽象注入到 `kiana_tui`。

#### 2.16.2 `native/clipboard.h`（208 行，平台无关，**逐字**）

```c
typedef enum { CLIPBOARD_TEXT, CLIPBOARD_IMAGE, CLIPBOARD_WRITE, CLIPBOARD_FILES } clipboard_operation;
// CLIPBOARD_PATHS stores concatenated, NUL-terminated UTF-8 paths in clipboard_job.data.
typedef enum {
    CLIPBOARD_UNAVAILABLE,
    CLIPBOARD_EMPTY,
    CLIPBOARD_UTF8,
    CLIPBOARD_LATIN1,
    CLIPBOARD_UTF16,
    CLIPBOARD_BUFFER,
    CLIPBOARD_PATHS
} clipboard_format;

typedef struct {
    napi_async_work work;
    napi_deferred deferred;
    clipboard_operation operation;
    clipboard_format format;
    void* data;
    size_t length;
    const char* error;
} clipboard_job;

static void clipboard_execute(clipboard_job* job);   // 唯一在工作线程执行；**不得**调用 N-API
static void* clipboard_alloc(size_t size);           // Win32: GlobalAlloc(GPTR) / POSIX: calloc(1, size)
static void clipboard_free(void* data);              // Win32: GlobalFree / POSIX: free
#ifdef KIANA_CLIPBOARD_WRITE
static void clipboard_copy(clipboard_job* job, const void* data, size_t length, clipboard_format format);
#endif
static void execute_clipboard_work(napi_env env, void* data);         // 转调 clipboard_execute
static int  create_clipboard_paths_value(napi_env env, const clipboard_job* job,
                                          napi_create_string_utf8_fn create_string, napi_value* result);
static void complete_clipboard_work(napi_env env, int status, void* data);
static napi_value queue_clipboard(napi_env env, napi_callback_info info, clipboard_operation operation);
static napi_value get_clipboard_text(napi_env env, napi_callback_info info);
static napi_value get_clipboard_image(napi_env env, napi_callback_info info);
#ifdef KIANA_CLIPBOARD_WRITE
static napi_value set_clipboard_text(napi_env env, napi_callback_info info);
#endif
```

**`complete_clipboard_work` 的结果映射（逐字语义，Rust 必须照此实现三态）**：
```
status != 0                      -> error = "Clipboard operation cancelled"
若 error 已存在                    -> reject(new Error(error))
否则 operation != CLIPBOARD_WRITE：
  CLIPBOARD_EMPTY                -> resolve(null)
  CLIPBOARD_PATHS                -> resolve(string[])，按 '\0' 切分 data；缺少终止符 -> error
                                    "Could not create clipboard value"
  CLIPBOARD_BUFFER               -> resolve(Buffer copy, length 字节)
  CLIPBOARD_UTF16                -> resolve(UTF-16 字符串，length 为 **code unit** 数)
  CLIPBOARD_UTF8 / CLIPBOARD_LATIN1 -> resolve(字符串；LATIN1 用 latin1 解码)
写入失败（create 失败）           -> error = "Could not create clipboard value"
CLIPBOARD_WRITE                  -> resolve(undefined)
最后：delete_work(env, job->work); clipboard_free(job->data); clipboard_free(job)
```

**`queue_clipboard` 的错误字符串（逐字）**：`Async clipboard API unavailable`、`Out of memory`、
`setText requires a string`、`Could not read clipboard text`、`Could not queue clipboard operation`。
异步作业名固定为 `kiana.clipboard`。

**`clipboard_copy` 的 `length` 语义（易错）**：`job->length = format == CLIPBOARD_UTF16 ? length / sizeof(uint16_t) : length;`
即 UTF-16 时存的是 **code unit 数**，其它格式是字节数。

各平台 `napi_register_module_v1` 实际导出的函数（**逐字**，逐平台）：
- Linux：`getText`、`getImage`
- Win32：`enableVirtualTerminalInput`、`isModifierPressed`、`getText`、`setText`、`getImage`
- Darwin：`isModifierPressed`、`getText`、`setText`、`getImage`、`getFilePaths`

语义（**必须保留**）：
- 读失败 → promise **reject**（`undefined` 表示"不支持/不可用"，`null` 表示"无内容"）。
  即：`getText(): Promise<string | null | undefined>`：
  `undefined` = unavailable；`null` = no text；reject = transfer failure。
- `getImage(): Promise<Uint8Array | null | undefined>`：同上。
- `getFilePaths(): Promise<string[] | null | undefined>`：POSIX 路径。
- `setText` 仅在 darwin/win32 导出（Linux 用命令行工具保留剪贴板所有权）。

#### 2.16.3 `native/linux/src/clipboard-worker.h`（70 行，**逐字**语义）

静态状态（单例，无队列）：
```c
static pthread_mutex_t worker_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t  worker_once  = PTHREAD_ONCE_INIT;
static pthread_cond_t  worker_changed;
static bool worker_ready, worker_busy, worker_waiting, worker_finished;
static clipboard_job worker_result;
static int (*signal_worker)(pthread_cond_t*);                                  // 运行时 dlsym
static int (*wait_for_worker)(pthread_cond_t*, pthread_mutex_t*, const struct timespec*);
```

`initialize_worker_condition()`（`pthread_once`）：
- 通过 `node_symbol` 动态解析 `pthread_cond_init` / `pthread_cond_signal` / `pthread_cond_timedwait`
  （注释：**逐字** `Unversioned ELF imports otherwise bind obsolete condition variables on glibc x64.`）；
- `pthread_condattr_setclock(&attributes, CLOCK_MONOTONIC)` **必须成功**，否则 `worker_ready = false`
  （后续所有调用直接"不可用"，返回 `undefined`）。

`read_on_private_thread()`：
- 调 `read_clipboard(&worker_result)`；
- 加锁后 `worker_finished = true`；若 `!worker_waiting`（调用方已超时）则 `free(worker_result.data)`
  并 `worker_busy = false`（**丢弃迟到结果**）；
- 无条件 `signal_worker` 后解锁。

`clipboard_execute(job)`（在 libuv 线程池线程上执行）：
```
pthread_once(initialize_worker_condition)
if (!worker_ready) return                      // -> 结果 undefined（不可用）
lock
if (!worker_busy):
    worker_busy = worker_waiting = true; worker_finished = false
    worker_result = { operation: job->operation }
    deadline = clock_gettime(CLOCK_MONOTONIC) + 3 秒
        // 注释（逐字）：Allow the two-second transfer deadline to report its own error first.
    if pthread_create(read_on_private_thread) 成功:
        pthread_detach
        while (!worker_finished && wait_for_worker(..., &deadline) == 0) {}
        if (worker_finished):
            job->{data, length, format, error} = worker_result.{...}
            worker_busy = false
    else:
        worker_busy = false
    worker_waiting = false
unlock
```
关键点：
- **只有一个私有线程**，忙时其它调用**立即返回不可用**（不排队、不阻塞）；
- 硬 deadline **3 秒**；超时后结果被丢弃，`transfer` 层面的 2000 ms 错误优先上报；
- `worker_result` 是**静态结构**，所以在返回前必须把字段复制到 `job`（Rust 侧同样需要 `Option` 拷贝语义）。

#### 2.16.4 `native/linux/src/linux-platform-x11.c`（274 行）

```c
#define MAX_CLIPBOARD_BYTES (50u * 1024u * 1024u)
#define CLIPBOARD_TIMEOUT_MS 2000

static const char* const text_types[] = {
    "text/plain;charset=utf-8", "text/plain;charset=UTF-8", "UTF8_STRING", "text/plain", "STRING",
};
static const char* const image_types[] = {
    "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp", "image/tiff",
};

typedef struct { unsigned char* data; size_t length; uint32_t items; uint8_t format; xcb_atom_t type; } property_data;
typedef struct { xcb_connection_t* connection; xcb_window_t window; xcb_atom_t clipboard, property, targets, incr;
                 int64_t deadline; } x11_clipboard;
```
（**注意文本类型顺序**：`text/plain;charset=utf-8` 在最前，`UTF8_STRING` 第三，`STRING` 最后。
上一版规格把顺序写错，已按源码更正。）

`append_bytes(result, bytes, length)`：
```
if (length > MAX_CLIPBOARD_BYTES - result->length) return false     // 超 50 MiB 失败
realloc(result->length + length + 1); memcpy；result->length += length; data[result->length] = 0
```

时间与超时（**逐字**）：
```
monotonic_ms() = clock_gettime(CLOCK_MONOTONIC) -> tv_sec*1000 + tv_nsec/1_000_000
// 注释（逐字）：Share one deadline across discovery, replies, and incremental chunks.
//               Connection setup and flushing are libxcb calls on the private thread.
wait_for_input(clipboard):
  loop:
    remaining = deadline - monotonic_ms()
    if (remaining <= 0 || xcb_connection_has_error(connection)) return false
    pollfd = { xcb_get_file_descriptor(connection), POLLIN, 0 }
    poll(..., remaining)  // 用剩余时间作为 poll 超时
```
`wait_for_reply` / `wait_for_event` 用同一 `deadline`（总预算 2000 ms）。

流程：`xcb_connect` → 建窗 → atoms `CLIPBOARD` / `KIANA_CLIPBOARD` / `TARGETS` / `INCR` →
`preferred_target(...)` 按 `text_types`/`image_types` 的**表顺序**选第一个可用者 →
`read_selection(...)`：
- `read_property` 校验 `format ∈ {8,16,32}`、`type` 与 `length`，以及
  `length <= MAX_CLIPBOARD_BYTES && length <= (uint64_t)reply->length * 4`；
- 支持 `INCR` 增量读取（每次 `PropertyNotify` 追加，零长度表示结束）；
- `STRING` → `CLIPBOARD_LATIN1`，其余文本 → `CLIPBOARD_UTF8`；图片 → `CLIPBOARD_BUFFER`；
- `TARGETS` 被拒绝时回退尝试 `UTF8_STRING`；
- 无内容 → `CLIPBOARD_EMPTY`（Node 侧得到 `null`）；
- 失败错误字符串 **【逐字】** `Could not read X11 clipboard`。

#### 2.16.5 `native/win32/src/win32-platform.c`（241 行）

文件首行 `#define KIANA_CLIPBOARD_WRITE`（启用写入路径）。常量（**逐字**）：
```c
#ifndef BI_ALPHABITFIELDS
#define BI_ALPHABITFIELDS 6
#endif
#define KEY_PRESSED_MASK 0x8000
#define OPEN_CLIPBOARD_ATTEMPTS 10
#define OPEN_CLIPBOARD_RETRY_MS 5
#define BITMAP_FILE_HEADER_SIZE 14
```

`is_modifier_name_pressed(name)`（**逐字**映射）：
| 名称 | 检查 |
| --- | --- |
| `shift` | `VK_SHIFT` / `VK_LSHIFT` / `VK_RSHIFT` |
| `control` | `VK_CONTROL` / `VK_LCONTROL` / `VK_RCONTROL` |
| `option` 或 `alt` | `VK_MENU` / `VK_LMENU` / `VK_RMENU` |
| `command` 或 `super` 或 `win` | `VK_LWIN` / `VK_RWIN` |
| 其它 | `0`（false） |

按键判定：`(GetAsyncKeyState(vk) & 0x8000) != 0`。名称缓冲为 `char name[16]`，超出截断。

`enable_virtual_terminal_input()`：
```
handle = GetStdHandle(STD_INPUT_HANDLE)
enabled = handle != INVALID_HANDLE_VALUE && GetConsoleMode(handle,&mode)
          && SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
返回布尔；取布尔失败 -> fail("Could not configure console input")
```

`open_clipboard(owner)`：`for attempt in 0..10 { if (OpenClipboard(owner)) return true; Sleep(5); } return false;`

`clipboard_execute(job)`（**顺序逐字**）：
1. `WRITE` 时先 `CreateWindowExW(0, L"STATIC", 0, 0, 0, 0, 0, 0, HWND_MESSAGE, 0, GetModuleHandleA(0), 0)`
   建消息窗（注释：**逐字** "A real clipboard owner is required for text writes, including under Node
   without a console. Create and destroy it on the same worker thread."）；失败 → `Could not create clipboard owner window`。
2. `open_clipboard(owner)` 失败 → `Could not open clipboard`（并销毁 owner）。
3. 分支：
   - `WRITE`：`size = (length+1)*2`；`GlobalAlloc(GMEM_MOVEABLE)` + `GlobalLock` + `copy_bytes`；
     `EmptyClipboard() && SetClipboardData(CF_UNICODETEXT, handle) != 0` 才算成功；失败 → `Could not set clipboard text`（并 `GlobalFree`）。
   - `TEXT`：`!IsClipboardFormatAvailable(CF_UNICODETEXT)` → `CLIPBOARD_EMPTY`；
     否则读 `GlobalSize(handle)/2` 个 code unit，遇 `NUL` 截断，`clipboard_copy(..., length*2, CLIPBOARD_UTF16)`；
     锁失败 → `Clipboard does not contain text`。
   - 其它（IMAGE）：`read_clipboard_image(job)`。
4. `CloseClipboard()`；销毁 owner。

`read_clipboard_image(job)`（**顺序逐字**）：
1. `png_format = RegisterClipboardFormatW(L"PNG")`；
   若 png 不可用且 `CF_DIBV5`、`CF_DIB` 都不可用 → `CLIPBOARD_EMPTY`。
2. 依次尝试 `png_format` → `CF_DIBV5` → `CF_DIB`；记录 `is_png = (png handle != 0)`。
3. `GlobalLock` + `GlobalSize`；失败 → `Clipboard does not contain an image`。
4. `is_png` → 原样 `CLIPBOARD_BUFFER`；否则 `dib_pixel_offset(source, size)` 计算像素偏移，
   分配 `size + 14` 字节并写 BMP 文件头：
   `bitmap[0]='B'`、`bitmap[1]='M'`、`write_u32(+2, size + 14)`、`write_u32(+10, 14 + offset)`，
   再 `copy_bytes(+14, source, size)`；分配失败 → `Could not create clipboard image buffer`。
5. `GlobalUnlock`。

`dib_pixel_offset(dib, size)`（**逐字**，注意 `header_size` 读的是 `dib[0..4]`）：
```
if (size < 12) return 0
header_size = read_u32(dib)                       // BITMAPFILEHEADER 之后的第一字段 biSize
if (header_size == 12) {                          // BITMAPCOREHEADER
    bits_per_pixel = read_u16(dib + 10)
    color_count = bits_per_pixel <= 8 ? 1u << bits_per_pixel : 0
    offset = 12 + color_count * 3                 // 注意是 *3，RGBTRIPLE
    return offset <= size ? offset : 0
}
if (header_size < 40 || header_size > size) return 0
bits_per_pixel = read_u16(dib + 14)
compression    = read_u32(dib + 16)
color_count    = read_u32(dib + 32)
if (!color_count && bits_per_pixel <= 8) color_count = 1u << bits_per_pixel
offset = header_size + color_count * 4
if (header_size == 40 && compression == BI_BITFIELDS)     offset += 3 * 4
if (header_size == 40 && compression == BI_ALPHABITFIELDS) offset += 4 * 4
return offset <= size ? offset : 0
```
另外导出 `_DllMainCRTStartup(HINSTANCE, DWORD, LPVOID)` 直接返回 `TRUE`。

错误字符串（**逐字**）：`Could not configure console input`、`Could not inspect modifier state`、
`Clipboard does not contain an image`、`Could not create clipboard image buffer`、
`Could not create clipboard owner window`、`Could not open clipboard`、
`Could not set clipboard text`、`Clipboard does not contain text`。

#### 2.16.6 `native/darwin/src/darwin-platform.m`（132 行）

头部 `#import <AppKit/AppKit.h>`、`#import <CoreGraphics/CoreGraphics.h>`、`#import <Foundation/Foundation.h>`、
`#define KIANA_CLIPBOARD_WRITE`。

`modifier_mask_for_name(name)`（**逐字**，用 `strcmp`）：
```
"shift"   -> kCGEventFlagMaskShift
"command" -> kCGEventFlagMaskCommand
"control" -> kCGEventFlagMaskControl
"option"  -> kCGEventFlagMaskAlternate
其它       -> 0
```
`is_modifier_pressed(name)`：掩码非 0 时取
`CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState)` 与掩码相与；
取布尔失败 → `Could not inspect modifier state`。名称缓冲 `char name[16]`。

`clipboard_execute(job)` 整体包在 `@autoreleasepool` 内，`pasteboard = [NSPasteboard generalPasteboard]`：
- 为 nil → `Could not open clipboard`。
- `WRITE`：`[[NSString alloc] initWithBytes:job->data length:job->length encoding:NSUTF8StringEncoding]`；
  为 nil → `Clipboard text is not valid UTF-8`（**直接 return，不 clear**）；否则
  `[pasteboard clearContents]` 后 `setString:forType:NSPasteboardTypeString`，失败 → `Could not set clipboard text`。
- `TEXT`：`stringForType:NSPasteboardTypeString`；nil → `CLIPBOARD_EMPTY`；
  `text.UTF8String` 为 NULL → `Could not encode clipboard text`；
  否则 `clipboard_copy(job, text.UTF8String, [text lengthOfBytesUsingEncoding:NSUTF8StringEncoding], CLIPBOARD_UTF8)`。
- `FILES`：`readObjectsForClasses:@[[NSURL class]] options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}`；
  先累计 `total = Σ(strlen(path)+1)`（溢出 → `Clipboard file paths are too large`）；
  `total == 0` → `CLIPBOARD_EMPTY`；分配失败 → `Out of memory`；
  否则写入**连续 NUL 终止**的路径块，`CLIPBOARD_PATHS`，`length = total`。
- 其它（IMAGE）：若 `availableTypeFromArray:@[NSPasteboardTypePNG, NSPasteboardTypeTIFF]` 为空 →
  `CLIPBOARD_EMPTY`；先试 `dataForType:NSPasteboardTypePNG`；否则
  `[[NSImage alloc] initWithPasteboard:pasteboard].TIFFRepresentation` →
  `[NSBitmapImageRep imageRepWithData:]` → `representationUsingType:NSBitmapImageFileTypePNG properties:@{}`；
  得到 PNG 则 `CLIPBOARD_BUFFER`，否则 `Clipboard does not contain an image`。

导出：`isModifierPressed`、`getText`、`setText`、`getImage`、`getFilePaths`
（`get_clipboard_file_paths` → `queue_clipboard(env, info, CLIPBOARD_FILES)`）。

错误字符串（**逐字**）：`Could not inspect modifier state`、`Could not open clipboard`、
`Clipboard text is not valid UTF-8`、`Could not set clipboard text`、`Could not encode clipboard text`、
`Clipboard file paths are too large`、`Out of memory`、`Clipboard does not contain an image`。

#### 2.16.7 构建脚本（产物路径与关键 flag 逐字）

**产物路径**（`getNativeModuleCandidates` 依赖）：
```
native/linux/prebuilds/linux-x64/linux-platform-x11.node
native/linux/prebuilds/linux-arm64/linux-platform-x11.node
native/darwin/prebuilds/darwin-x64/darwin-platform.node
native/darwin/prebuilds/darwin-arm64/darwin-platform.node
native/win32/prebuilds/win32-x64/win32-platform.node
native/win32/prebuilds/win32-arm64/win32-platform.node
```
注意 Linux 文件名带 `-x11` 后缀（`loadNativePlatformHelper("linux", "-x11")`），其它平台无后缀。

**`native/linux/build.sh`**：
- 非 Linux 平台直接失败：`Linux native helpers must be built on Linux`；
- `uname -m`：`x86_64`→`x64`，`aarch64|arm64`→`arm64`，其它 → `Unsupported Linux architecture: <m>`；
- 编译器 `${CC:-cc}`，缺失 → `Linux C compiler not found: <cc>`；
- flags（**逐字**）：`-std=c11 -D_POSIX_C_SOURCE=200809L -Wall -Wextra -Werror -Os -flto -fPIC -pthread
  -fvisibility=hidden -fno-stack-protector -shared -nostdlib -Wl,--unresolved-symbols=ignore-all
  -Wl,--no-as-needed -Wl,-O2 -Wl,--gc-sections -Wl,-z,nodelete -Wl,-s`，链接 `-lxcb`；
- `-Wl,-z,nodelete` 的注释（**逐字**）：`A timed-out private thread may outlive the Node environment that loaded us.`；
- 在 `mktemp -d "${TMPDIR:-/tmp}/kiana-tui-linux.XXXXXX"` 中构建，`trap rm -rf` 清理，
  `install -m 755` 到 `prebuilds/linux-<arch>/`，打印 `Built <path>`。

**`native/darwin/build.sh`**：
- 编译器优先级：`$CC` → `xcrun --find clang` → `clang`；缺失 → `Darwin C compiler not found: <cc>`；
- SDK：`$SDKROOT`（不存在 → `SDKROOT does not exist: <path>`）→ `xcrun --sdk macosx --show-sdk-path`，
  生成 `-isysroot <sdk> -F<sdk>/System/Library/Frameworks`；
- flags（**逐字**）：`-fobjc-arc -std=c11 -Wall -Wextra -Oz -flto -fvisibility=hidden --target=<target>
  <sdk_flags> -bundle -undefined dynamic_lookup -Wl,-dead_strip -Wl,-x
  -framework AppKit -framework CoreGraphics -framework Foundation`；
- 两个目标：`build arm64 arm64-apple-macos11.0`、`build x64 x86_64-apple-macos10.15`；
- 产物 `prebuilds/darwin-<arch>/darwin-platform.node`，打印 `Built <path>`。

**`native/win32/build.mjs`**（Node 脚本）：
- 目标表（**逐字**）：
  ```
  x64:   msvcArch "x64",   mingwTarget "x86_64-w64-windows-gnu",
         prefixedCompilers ["x86_64-w64-mingw32-gcc", "x86_64-w64-mingw32-clang"], envCompiler "CC_X64"
  arm64: msvcArch "arm64", mingwTarget "aarch64-w64-windows-gnu",
         prefixedCompilers ["aarch64-w64-mingw32-gcc", "aarch64-w64-mingw32-clang"], envCompiler "CC_ARM64"
  ```
- 临时目录 `mkdtempSync(path.join(tmpdir(), "kiana-tui-win32-"))`，`process.on("exit")` 与 `SIGINT`（退出码 130）清理。
- MSVC 探测：`vswhere.exe -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`
  → `Common7/Tools/VsDevCmd.bat`；回退搜索 `%ProgramFiles%`/`%ProgramFiles(x86)%` 下
  `Microsoft Visual Studio/2022/{BuildTools,Community,Professional,Enterprise}/Common7/Tools/VsDevCmd.bat`。
- MSVC 命令（**逐字**）：`cl /nologo /TC /std:c11 /W4 /O1 /GS- /LD /Fe:<out> <src> /link /NOENTRY /NODEFAULTLIB kernel32.lib user32.lib /OPT:REF /OPT:ICF`。
- MinGW 编译器解析顺序：`$CC_X64`/`$CC_ARM64` → `$CC`（需 `--target=`）→ 前缀编译器 → `clang`（需 `--target=`）。
- MinGW 参数（**逐字**）：`--target=<t> -std=c11 -Wall -Wextra -Oz -fno-builtin -shared -nostdlib
  -Wl,--no-entry -Wl,--strip-all <src> -lkernel32 -luser32 -o <out>`。
- 工具链选择：`KIANA_TUI_WIN32_TOOLCHAIN`（只允许 `msvc`/`mingw`，其它 →
  `KIANA_TUI_WIN32_TOOLCHAIN must be either 'msvc' or 'mingw'`）；未设置时
  `process.platform === "win32" && vsDevCmd ? "msvc" : "mingw"`。
- 错误文案（**逐字**）：
  `No Windows cross-compiler found for ${arch}. Install Microsoft C++ Build Tools on Windows, or set ${envCompiler} to a MinGW-compatible compiler.`、
  `Microsoft C++ Build Tools not found. Install the Visual Studio C++ workload or set KIANA_TUI_WIN32_TOOLCHAIN=mingw.`
- `--help` 输出 Environment 段（**逐字**）：`KIANA_TUI_WIN32_TOOLCHAIN=msvc|mingw`、`CC_X64=...`、`CC_ARM64=...`、`CC=...`。

**Rust 侧**：不需要复刻这些构建脚本；用 `cargo build` + 平台 feature 即可。但**产物查找逻辑**
（`getNativeModuleCandidates` 的 4 个候选位置与文件名）必须保留，以便渐进式替换。

#### 2.16.8 原生模块 README 中的行为契约（**逐字**要点）

- **Linux**（`native/linux/README.md`）：
  - "Provides asynchronous X11 text and image reads using `libxcb.so.1`. Prebuilds support x64 and arm64 on glibc and musl.
    Reads have bounded waits; if a native operation stalls, the helper remains unavailable until it finishes."
  - "Coding-agent falls back to command-line tools when native reads are unavailable. Wayland reads use `wl-paste`;
    all Linux writes use the existing command-line or terminal clipboard paths."
  - 测试依赖：C 编译器 + XCB 头 + `pkg-config`、`Xvfb`、`xclip`；用隔离的 X11 server，不碰桌面剪贴板；
    依赖缺失时 **skip**。
- **Darwin**（`native/darwin/README.md`）：提供修饰键状态与异步 text/image/file-path 读取；
  任一机器可交叉构建 arm64（macOS 11+）与 x64（macOS 10.15+）；交叉构建需 osxcross
  （`CC=/path/to/osxcross/clang SDKROOT=/path/to/MacOSX.sdk`）。
- **Win32**（`native/win32/README.md`）：控制台输入设置 + 修饰键 + 异步 text/image 剪贴板；
  链接 `kernel32` 与 `user32`；**不需要 Node 头文件**。README 的 `## Testing clipboard writes` 小节逐字内容：
  ```
  On a Windows test desktop, run from `packages/tui` in PowerShell:

  $env:KIANA_TEST_NATIVE_CLIPBOARD = "1"
  node --test test/native-platform.test.ts

  This opt-in test checks native text writes and reads. **It replaces the system clipboard contents.**
  ```
  构建小节还给出 `npm --prefix packages/tui run build:native:win32`（VS "Desktop development with C++"
  workload）与 `KIANA_TUI_WIN32_TOOLCHAIN=mingw` + `CC_X64`/`CC_ARM64` 的 MinGW 交叉编译配方。

这些"有界等待、失败即不可用、绝不阻塞事件循环"的承诺是 Rust 实现的**行为契约**（对应
`native-clipboard-linux.test.ts` 的 `keeps JS responsive during stalled X11 connection setup` 等用例）。

---

### 2.17 `src/native-module-path.ts` / `native-platform.ts` / `native-modifiers.ts`

#### 2.17.1 `getNativeModuleCandidates(nativePath, options = {})`

```ts
const TUI_PACKAGE_NAME = "@earendil-works/kiana-tui";
const moduleRequire = createRequire(import.meta.url);

export interface NativeModuleCandidateOptions {
  moduleUrl?: string; execPath?: string; resolvePackage?: (specifier: string) => string;
}

export function getNativeModuleCandidates(nativePath, options = {}): string[] {
  const moduleDir = dirname(fileURLToPath(options.moduleUrl ?? import.meta.url));
  const candidates: string[] = [];
  try {
    const packageEntry = (options.resolvePackage ?? moduleRequire.resolve)(TUI_PACKAGE_NAME);
    candidates.push(join(dirname(packageEntry), "..", nativePath));
  } catch { /* 独立二进制没有已安装的 TUI 包 */ }
  candidates.push(
    join(moduleDir, "..", nativePath),
    join(moduleDir, nativePath),
    join(dirname(options.execPath ?? process.execPath), nativePath),
  );
  return Array.from(new Set(candidates));
}
```
顺序与去重（**逐字**）：包安装位置 → `moduleDir/..` → `moduleDir` → 可执行文件目录；`Set` 保持首次出现顺序。
测试（`native-module-path.test.ts`，2 个 it）：候选顺序、去重、`resolvePackage` 抛异常时跳过。

#### 2.17.2 `native-platform.ts`

```ts
export type ModifierKey = "shift" | "command" | "control" | "option";
export interface NativeClipboard {
  getText(): Promise<string | null | undefined>;
  getImage(): Promise<Uint8Array | null | undefined>;
  getFilePaths?(): Promise<string[] | null | undefined>;
  setText?(text: string): Promise<void>;
}
type NativePlatformHelper = NativeClipboard & {
  enableVirtualTerminalInput?: () => boolean;
  isModifierPressed?: (name: ModifierKey) => boolean;
};

const helpers = new Map<string, NativePlatformHelper | undefined>();   // 缓存的是"模块加载"，不是 DISPLAY 可用性

function loadNativePlatformHelper(platform, suffix = ""): NativePlatformHelper | undefined {
  const arch = process.arch;
  if (arch !== "x64" && arch !== "arm64") return undefined;
  const nativePath = path.join("native", platform, "prebuilds", `${platform}-${arch}`,
                               `${platform}-platform${suffix}.node`);
  if (helpers.has(nativePath)) return helpers.get(nativePath);
  for (const modulePath of getNativeModuleCandidates(nativePath)) {
    try {
      const helper = cjsRequire(modulePath) as Partial<NativePlatformHelper> | null;
      if (typeof helper?.getText === "function" && typeof helper.getImage === "function") {
        helpers.set(nativePath, helper as NativePlatformHelper);
        return helper as NativePlatformHelper;
      }
    } catch { /* 尝试下一个位置 */ }
  }
  helpers.set(nativePath, undefined);     // 负缓存：避免反复尝试
  return undefined;
}

export function getNativePlatformHelper() {
  if (process.platform !== "darwin" && process.platform !== "win32") return undefined;
  return loadNativePlatformHelper(process.platform);
}

export function getNativeClipboard(): NativeClipboard | undefined {
  if (process.platform !== "linux") return getNativePlatformHelper();
  if (!process.env.DISPLAY) return undefined;         // 每次调用都重新检查 DISPLAY
  return loadNativePlatformHelper("linux", "-x11");
}
```
测试（`native-platform.test.ts`，4 个 `test(`，**名称为逐字**）：
1. `Linux ARM64 prebuild supports 64 KB system pages`（无 skip；直接读 ELF 头验证 `p_align`/`p_vaddr` 与
   64 KB 页对齐，要求 `native/linux/prebuilds/linux-arm64/linux-platform-x11.node` 存在）；
2. `writes Windows clipboard text through the native helper without command fallbacks`
   （skip 条件 `process.platform !== "win32" || process.env.KIANA_TEST_NATIVE_CLIPBOARD !== "1"`；
   依次写读 `"clipboard café 日本語"`、`""`、`"second write"`，并断言 `getImage()` 为 null——
   **会覆盖系统剪贴板**，见 §4.1 的 `KIANA_TEST_NATIVE_CLIPBOARD`）；
3. `uses the native platform helper directly as the clipboard API`
   （skip 条件 `!["darwin","win32"].includes(platform) || !["arm64","x64"].includes(arch)`；
   断言 `getNativeClipboard() === getNativePlatformHelper()` 恒等）；
4. `Linux loads X11 lazily and rechecks DISPLAY`（skip 条件 `!["arm64","x64"].includes(arch)`；
   注入 `require.cache` mock + 改写 `process.platform`，验证：无 `DISPLAY` 时返回 `undefined`、
   有 `DISPLAY` 时返回 helper 且 `getText` 未被提前调用、删除 `DISPLAY` 后重新变 undefined、
   `DISPLAY` 改成 `:1` 后返回**同一个**缓存实例）。

#### 2.17.3 `native-modifiers.ts`

```ts
export type { ModifierKey } from "./native-platform.ts";
export function isNativeModifierPressed(key: ModifierKey): boolean {
  const helper = getNativePlatformHelper();
  if (!helper?.isModifierPressed) return false;
  try { return helper.isModifierPressed(key) === true; } catch { return false; }
}
```

---

### 2.18 `src/index.ts` 导出面（176 个符号：91 运行时 + 85 类型）

按源码分组（**顺序即源码顺序**；`type` 前缀表示仅类型）：

```
marked:            Marked, Token, Tokens
autocomplete.ts:   AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
                   CombinedAutocompleteProvider, SlashCommand
colors.ts:         backgroundAnsi, Color, ColorMixSpace, colorToHex, colorToOkhsl, colorToOklch,
                   colorToRgb, foregroundAnsi, IndexedColor, indexedColor, mixColors, OkhslChannels,
                   OklchChannels, OklchColorValue, okhslColor, oklchColor, parseColor, RgbColorValue,
                   rgbColor, styleText, styleTextWithAnsi, TerminalColorMode, TextAttributes, TextStyle
components/box:    Box
cancellable-loader: CancellableLoader
editor:            Editor, EditorOptions, EditorTheme
h-stack:           HStack
image:             Image, ImageOptions, ImageTheme, ImageTranscoder, setImageTranscoder
input:             Input
loader:            Loader, LoaderIndicatorOptions
markdown:          DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme
mouse-region:      MouseRegion, MouseRegionHandler
scroll-view:       ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions
select-list:       SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
                   SelectListTruncatePrimaryContext
settings-list:     SettingItem, SettingsList, SettingsListTheme
spacer:            Spacer
text:              Text
truncated-text:    TruncatedText
v-stack:           StackChild, StackEntry, StackEntryOptions, StackOptions, VStack
editor-component:  EditorComponent
fuzzy:             FuzzyMatch, fuzzyFilter, fuzzyMatch
keybindings:       getKeybindings, Keybinding, KeybindingConflict, KeybindingDefinition,
                   KeybindingDefinitions, Keybindings, KeybindingsConfig, KeybindingsManager,
                   setKeybindings, TUI_KEYBINDINGS
keys:              decodeKittyPrintable, isKeyRelease, isKeyRepeat, isKittyProtocolActive, Key,
                   KeyEventType, KeyId, matchesKey, parseKey, setKittyProtocolActive
latex:             RenderLatexOptions, renderLatex
native-platform:   getNativeClipboard, NativeClipboard
oklab:             oklabToOkhslLightness
program-status:    formatProgramStatus, ProgramStatus
stdin-buffer:      StdinBuffer, StdinBufferEventMap, StdinBufferOptions
terminal:          isAppleTerminalSession, ProcessTerminal, Terminal
terminal-colors:   parseTerminalColorSchemeReport, RgbColor, TerminalColorScheme, TerminalColors
terminal-image:    allocateImageId, CellDimensions, calculateImageRows, deleteAllKittyImages,
                   deleteKittyImage, detectCapabilities, encodeITerm2, encodeKitty, getCapabilities,
                   getCellDimensions, getGifDimensions, getImageDimensions, getJpegDimensions,
                   getPngDimensions, getTerminalColorMode, getWebpDimensions, hyperlink,
                   ImageDimensions, ImageProtocol, ImageRenderOptions, imageFallback, renderImage,
                   resetCapabilitiesCache, setCapabilities, setCapabilityOverrides, setCellDimensions,
                   TerminalCapabilities
tui:               Component, Container, CURSOR_MARKER, compositeTuiLine, Focusable, isFocusable,
                   isViewportTUI, OverlayAnchor, OverlayBounds, OverlayHandle, OverlayMargin,
                   OverlayOptions, OverlayUnfocusOptions, renderFakeCursor, SizeValue, TUI,
                   TuiInputListener, TuiInputListenerResult, TuiMode, TuiMouseButton, TuiMouseEvent,
                   TuiMouseEventResult, TuiMouseEventType, TuiStopOptions, ViewportTUI
tui-alt-screen:    TuiAltScreen, TuiAltScreenOptions
tui-main-screen:   TuiMainScreen, TuiMainScreenRenderState
utils:             getOsc8LinkAtColumn, sliceByColumn, stripTerminalSequences, truncateToWidth,
                   visibleWidth, wrapTextWithAnsi
wheel-scroll:      WheelScrollLines
```

`index.ts` 的 6 个 utils 符号来自 `./utils.ts` 直接导入；但 **`src/tui.ts` 内部也有一条独立再导出**
```
// src/tui.ts:209
export { visibleWidth };
```
即 `tui.ts` 把自己的 `visibleWidth`（从 utils 导入后转出）也暴露在模块 API 上，`index.ts` **没有**使用它
（index 显式走 `./utils.ts`）。Rust 侧若要镜像模块级 API，`tui` 模块应 `pub use utils::visible_width;`；
若只镜像 `index.ts` 的导出面则无影响。**不要**因此重复计数（`visibleWidth` 在 index 中只出现一次，
总数仍是 176）。

**未从 index 导出但被测试/兄弟包直接 import 的内部符号**（Rust 需 `#[doc(hidden)] pub` 或
`pub(crate)` + 测试专用 `pub`）：

```
tui.ts:              dispatchMouseEvent, retargetMouseEvent, TuiBase, VIEWPORT_TUI
layout.ts:           getLayoutBoxesAt, renderLayoutFrame, getScrollbarGeometry, getScrollViewBox,
                     getScrollViewsAt, LayoutBox, LayoutFrame, LayoutRect, ScrollbarGeometry
layout-node.ts:      LAYOUT_NODE, getLayoutNode, LayoutNode, LayoutComponent, StackLayoutEntry,
                     ScrollLayoutNode, StackLayoutNode, LayoutViewport, ScrollLayoutState
terminal-image.ts:   registerKittyImageMetadata, cropKittyImageLine, getKittyImageMetadata,
                     getKittyImagePlacement, getKittyImagePlacementRows, getMaxKittyImageRows,
                     isImageLine, getKittyImageMetadata
alt-screen-search.ts: AltScreenSearchComponent, AltScreenSearchIndex, findAltScreenSearchMatches,
                     getAltScreenSearchMatchKey, AltScreenSearchMatch, AltScreenSearchSegment,
                     AltScreenSearchResult
components/scroll-view.ts: ScrollView（index 已导出）
components/stack.ts: allocateStackSizes, visibleStackEntries, Stack
components/alt-screen-flash.ts: AltScreenFlashContainer
utils.ts:            parseOscColorResponse（经 tui.ts）, extractAnsiCode, extractSegments,
                     getGraphemeCellRange, normalizeTerminalOutput, getWordSegmenter,
                     getGraphemeSegmenter, applyBackgroundToLine, sliceWithWidth, flattenLines,
                     PUNCTUATION_REGEX, cjkBreakRegex, cjkPunctuationRegex,
                     autocompleteSeparatorRegex, autocompleteBoundaryRegex
terminal.ts:         parseKeyboardProtocolNegotiationSequence, refreshTerminalDimensions,
                     normalizeAppleTerminalInput, normalizeNativeShiftEnterInput,
                     resolveEscapeTimeoutMs
terminal-colors.ts:  parseOscColorResponse（公共导出，但 index 未转出）
native-module-path.ts: getNativeModuleCandidates
native-platform.ts:  getNativePlatformHelper（类型 NativePlatformHelper 未导出）
native-modifiers.ts: isNativeModifierPressed
wheel-scroll.ts:     WheelScrollAccelerator
keys.ts:             parseKeyboardProtocolNegotiationSequence（见 terminal.ts）
```

Rust 的 `kiana_tui` crate 应导出同名符号（Rust 命名风格 snake_case，但**字符串常量、协议内容、
错误消息保持原文**；类型名可用 PascalCase）。

---

### 2.19 `src/components/scroll-view.ts`（224 行）与 `stack.ts` 的 tui-core 侧契约

> 完整的组件语义由 `tui-components.md` §2.1 / §2.17 定义。本节只记录 **tui-core 渲染器/布局器
> 直接依赖的接口面**，以及 `layout.ts` 与 `ScrollView` 的状态机耦合点；Rust 侧实现**必须同时满足**
> 两处规格（取并集，冲突以 TS 源码为准）。

#### 2.19.1 被 `layout.ts` / `tui-alt-screen.ts` 使用的成员

```
scrollTop: number                       // 当前滚动偏移（行）
viewportHeight: number                  // 最近一次 updateLayout 传入的视口高度
followEnd: boolean                      // 构造时固定（options.follow === "end"）
isFollowingEnd: boolean                 // 动态：当前是否贴住内容末尾
primary: boolean                        // 构造时固定；layout.ts 据此设 primaryScrollView
overscroll: "chain" | "contain"         // 默认 "chain"
scrollbar: ScrollViewScrollbar          // getter（可变）
isScrollbarVisible: boolean             // always: viewportHeight > 0；auto: contentHeight > viewportHeight 且 transientScrollbarVisible
isScrollbarActive: boolean
scrollbarTrackStyle / scrollbarThumbStyle: (text: string) => string
scrollTo(scrollTop: number, options?: { disableFollow?: boolean }): void
scrollBy(lines: number): number          // 返回**未消费**的行数（用于 chain）
setScrollbarActive(active: boolean): void
getContentWidth(width: number): number   // scrollbar === "always" && width > 1 ? width - 1 : width
updateLayout(contentHeight: number, viewportHeight: number, requestRender: () => void): void
```

`layout.ts` 的 `layoutComponent` 对 scroll 节点的处理（详见 §2.11）：
1. 以 `getContentWidth(width)` 渲染子组件得到 `scrollContentLines`；
2. 计算 `contentHeight = scrollContentLines.length`；
3. 调用 `state.updateLayout(contentHeight, allocatedHeight, ctx.requestRender)`；
4. 取 `scrollTop` 作为 `lineOffset`，把 `[scrollTop, scrollTop + height)` 的行放进 `LayoutBox.lines`；
5. 若 `state.primary && ctx.primaryScrollView === undefined` 则登记 `ctx.primaryScrollView`。
6. 滚动条绘制由 `renderLayoutFrame` → `paintScrollbar` 完成（不经过组件渲染）。

#### 2.19.2 `ScrollView` 状态机（供 Rust 复刻，语义逐字）

构造：
```ts
if (options.axis !== undefined && options.axis !== "vertical") throw new Error(`Unsupported ScrollView axis: ${options.axis}`);
this.followEnd = (options.follow ?? "none") === "end";
this.followingEnd = this.followEnd;
this.primary = options.primary ?? false;
this.overscroll = options.overscroll ?? "chain";
this.currentScrollbar = options.scrollbar ?? "hidden";
this.scrollbarTrackStyle = options.scrollbarTrackStyle ?? ((text) => `\x1b[90m${text}\x1b[39m`);
this.scrollbarThumbStyle = options.scrollbarThumbStyle ?? ((text) => `\x1b[37m${text}\x1b[39m`);
this.scrollbarHideDelayMs = Math.max(0, Math.floor(options.scrollbarHideDelayMs ?? 1000));
```
注意默认 `follow: "none"`、`scrollbar: "hidden"`、`overscroll: "chain"`、`primary: false`。
`TuiAltScreen` 的隐式滚动视图用 `{ follow: "end", primary: true }`。

`scrollTo`：
```ts
requested = Number.isFinite(scrollTop) ? Math.trunc(scrollTop) : currentScrollTop
maxScrollTop = max(0, contentHeight - viewportHeight)
next = clamp(requested, 0, maxScrollTop)
nextFollowSuppressedAtEnd = options.disableFollow === true && next === maxScrollTop
nextFollowingEnd = !nextFollowSuppressedAtEnd && followEnd && next === maxScrollTop
三者都没变 -> 直接 return（不触发渲染）
moved = next !== currentScrollTop
更新三个字段；moved 时 markScrollbarActivity()；requestRender()
```

`scrollBy`：
```ts
requested = Number.isFinite(lines) ? Math.trunc(lines) : 0
if (requested === 0) return 0
maxScrollTop = max(0, contentHeight - viewportHeight)
start = followingEnd ? maxScrollTop : currentScrollTop
next = clamp(start + requested, 0, maxScrollTop)
moved = next - start
currentScrollTop = next
followingEnd = followEnd && next === maxScrollTop
followSuppressedAtEnd = false
if (moved !== 0) markScrollbarActivity()
if (moved !== 0 || followingEnd !== wasFollowingEnd) requestRender()
return requested - moved          // 未消费部分交给外层（chain）
```

`scrollToStart`：`currentScrollTop = 0`；`followingEnd = followEnd && contentHeight <= viewportHeight`；
`followSuppressedAtEnd = false`；仅当 `scrollTop` 或 `followingEnd` 变化时 `markScrollbarActivity() + requestRender()`。

`scrollToEnd`：`next = max(0, contentHeight - viewportHeight)`；`followingEnd = followEnd`；
`followSuppressedAtEnd = false`；仅当 `scrollTop` 或 `followingEnd` 变化时触发渲染。

`updateLayout`：
```ts
contentHeight = max(0, floor(contentHeight))
currentViewportHeight = max(0, floor(viewportHeight))
requestRenderCallback = requestRender
maxScrollTop = max(0, contentHeight - currentViewportHeight)
if (followingEnd) currentScrollTop = maxScrollTop
else currentScrollTop = clamp(currentScrollTop, 0, maxScrollTop)
if (currentScrollTop < maxScrollTop) followSuppressedAtEnd = false
if (followEnd && currentScrollTop === maxScrollTop && !followSuppressedAtEnd) followingEnd = true
if (contentHeight <= currentViewportHeight) hideTransientScrollbar()
```

`markScrollbarActivity()`：
```ts
if (scrollbar !== "auto" || contentHeight <= currentViewportHeight) return
transientScrollbarVisible = true
clear existing timer
if (scrollbarActive) return            // 活动拖动期间不排隐藏定时器
timer = setTimeout(() => { timer = undefined; transientScrollbarVisible = false; requestRender() }, scrollbarHideDelayMs)
timer.unref()
```

`setScrollbar(scrollbar)`：相同则 no-op；`!= "auto"` → `hideTransientScrollbar()`；`== "auto"` 且
`scrollbarActive` → `markScrollbarActivity()`；最后 `requestRender()`。

`addChild`/`removeChild`/`clear` 全部抛错（**逐字**见 §3.3）。

`render(width)`：`contentWidth = getContentWidth(width)`；`lines = child.render(contentWidth)`；
`contentWidth === width ? lines : lines.map(line => line + " ")`（**每行末尾补一个空格**）。

`[LAYOUT_NODE]()` → `{ type: "scroll", component: this.child, state: this }`。

#### 2.19.3 `Stack` 的 tui-core 侧契约

`Stack`（抽象）→ `VStack` / `HStack`。`layout.ts` 只消费 `[LAYOUT_NODE]()` 返回的
`{ type: "vstack" | "hstack", entries, gap, align }`，其中
`entries: StackLayoutEntry[] = { component, basis?, grow?, shrink?, minSize?, maxSize?, visible? }`。
`allocateStackSizes` / `visibleStackEntries` 的算法见 `tui-components.md` §2.1（本规格不重复）。
`align` 在 `layout.ts` 中对 hstack 的水平对齐、vstack 的垂直对齐生效：
```ts
const childHeight = node.align === "stretch" ? allocatedHeight : Math.min(allocatedHeight, naturalChildHeight);
if (node.align === "center") childY += Math.floor((allocatedHeight - childHeight) / 2);
else if (node.align === "end") childY += allocatedHeight - childHeight;
```
该片段位于 `/home/ctyun/code/kiana_rev/kiana/packages/tui/src/layout.ts:222-225`。

---

## 3. 用户可见面（逐字）

本节收集所有用户可见的字符串、按键、ANSI 序列与布局尺寸，供 Rust 端逐字节复现。

### 3.1 终端模式序列

| 场景 | 序列（**逐字**） |
| --- | --- |
| 进入备用屏 | `\x1b[?1049h` |
| 退出备用屏 | `\x1b[?1049l` |
| 关闭自动换行 | `\x1b[?7l` |
| 打开自动换行 | `\x1b[?7h` |
| 鼠标（按键+拖动+焦点+SGR） | `\x1b[?1000h\x1b[?1002h\x1b[?1004h\x1b[?1006h` |
| 鼠标（全动作，含悬停） | `\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h` |
| 关闭鼠标 | `\x1b[?1006l\x1b[?1004l\x1b[?1003l\x1b[?1002l\x1b[?1000l` |
| 焦点进入/离开 | `\x1b[I` / `\x1b[O` |
| 同步输出开/关 | `\x1b[?2026h` / `\x1b[?2026l` |
| 清屏+归位+清滚动缓冲 | `\x1b[2J\x1b[H\x1b[3J`（仅 `fullRender`） |
| 主题通知开/关 | `\x1b[?2031h` / `\x1b[?2031l` |
| 光标隐藏/显示 | `\x1b[?25l` / `\x1b[?25h` |
| 清行 / 清到屏尾 / 清屏 | `\x1b[K` / `\x1b[J` / `\x1b[2J\x1b[H` |
| 上/下移 N 行 | `\x1b[${n}A` / `\x1b[${n}B` |
| 括号粘贴开/关 | `\x1b[?2004h` / `\x1b[?2004l` |
| Kitty 键盘推入/弹出 | `\x1b[>7u` / `\x1b[<u` |
| modifyOtherKeys 开/关 | `\x1b[>4;2m` / `\x1b[>4;0m` |
| Kitty 键盘查询 + 可选 OSC 7501 + DA1 哨兵 | `` `\x1b[>7u\x1b[?u${programStatusQuery}\x1b[c` ``，其中 `programStatusQuery` 为 `\x1b]7501;?\x1b\\` 或空串 |
| 程序状态查询 | `\x1b]7501;?\x1b\\` |
| 进度（活动/清除） | `\x1b]9;4;3\x07` / `\x1b]9;4;0\x07` |
| 窗口标题 | `\x1b]0;${title}\x07` |
| 单元尺寸查询 | `\x1b[16t` |
| 终端颜色查询 | `\x1b]10;?\x07\x1b]11;?\x07` + 16 个 `\x1b]4;${i};?\x07` + `\x1b[c` |
| OSC 52 复制 | `\x1b]52;c;${base64}\x07` |
| OSC 8 超链接 | `\x1b]8;;${url}\x1b\\${text}\x1b]8;;\x1b\\` |
| 备用屏停机的文档回放 | 每行 `\r\x1b[2K<line>`，行间 `\r\n`，末尾 `\x1b[0m\x1b[?7h\r\n\x1b[?25h\x1b[?2026l` |
| 备用屏启动 | `\x1b[?1049h\x1b[?7l<mouse>\x1b[2J\x1b[H\x1b[?25l` |

伪光标标记（**逐字**，APC 内）：
```
CURSOR_MARKER        = "\x1b_kiana:c\x07"
FAKE_CURSOR_START    = "\x1b_kiana:fc\x07"
FAKE_CURSOR_END      = "\x1b_kiana:/fc\x07"
FOCUSED_FAKE_CURSOR  = /(\x1b_kiana:c\x07)\x1b_kiana:fc\x07(.*?)(?:\x1b_kiana:\/fc\x07|$)/s
渲染时                START -> "\x1b[7m"，END -> "\x1b[27m"
SEGMENT_RESET        = "\x1b[0m\x1b]8;;\x07"
```

### 3.2 滚动条与边框字形

| 用途 | 字符 | 样式（默认） |
| --- | --- | --- |
| 滚动条轨道 | `│` | `\x1b[90m${text}\x1b[39m` |
| 滚动条拇指（非活动） | `┃` | `\x1b[37m${text}\x1b[39m` |
| 滚动条拇指（活动） | `█` | `\x1b[37m${text}\x1b[39m` |
| 搜索框角/边 | `┌` `─` `┐` `│` `└` `┘` | 无 |
| 搜索框宽度为 1 时 | `["┌", "│", "└"]` | 无 |
| 搜索导航按钮 | `↑ ${previousKey}` `↓ ${nextKey}`，分隔符 `" · "`；放不下时退化为 `↑` / `↓` / `" "` | 由 `searchNavigationButtonStyle` |
| 搜索占位符 | `Find in transcript`，样式 `\x1b[2m…\x1b[22m` | |
| 搜索结果计数 | `No matches` 或 `${index+1}/${count}`，样式 `\x1b[2m … \x1b[22m` | |
| 未绑定键显示 | `Unbound`（darwin 上 `alt` 显示为 `Option`，其余首字母大写，`+` 连接） | |
| 搜索高亮（非当前） | `\x1b[4m${text}\x1b[24m` | |
| 搜索高亮（当前） | `\x1b[1;7m${text}\x1b[22;27m` | |
| 选区高亮 | `\x1b[7m${text}\x1b[27m`（在每个 SGR `m` 之后重新发出） | |
| 闪烁消息 | `\x1b[7m ${message} \x1b[27m`（两侧各一个空格，`truncateToWidth(..., width, "")`） | |

滚动条拇指高度（**逐字算法**）：
```ts
const minThumbHeight = Math.min(2, trackHeight);
const thumbHeight = Math.max(minThumbHeight,
  Math.min(trackHeight, Math.round((trackHeight * trackHeight) / contentHeight)));
const maxScrollTop = Math.max(0, contentHeight - trackHeight);
const maxThumbTop = trackHeight - thumbHeight;
const thumbOffset = maxScrollTop === 0 ? 0 : Math.round((box.scrollView.scrollTop / maxScrollTop) * maxThumbTop);
const column = box.rect.x + box.rect.width - 1;
```
`column` 必须落在 `clip` 内，否则不画滚动条。

### 3.3 用户可见文案（**逐字**）

| 位置 | 文案 |
| --- | --- |
| 选区复制成功 | `Copied!` |
| 选区复制失败（无注入实现返回 false） | `Copy failed` |
| 选区复制失败（注入实现返回字符串） | 该字符串本身 |
| 备用屏搜索占位符 | `Find in transcript` |
| 搜索无匹配 | `No matches` |
| 未绑定快捷键 | `Unbound` |
| 图片回退 | `` `[Image: ${parts.join(" ")}]` ``，其中 `parts` = 可选文件名（`~` 缩短，超链接开启且绝对路径时包 OSC 8 `file://` URL）+ `[${mimeType}]` + 可选 `${widthPx}x${heightPx}` |
| 渲染崩溃错误 | 见下 |

渲染崩溃错误消息（**逐字**，`TuiMainScreen.fullRender` 抛出）：

```
Rendered line ${i} exceeds terminal width (${visibleWidth(line)} > ${width}).

This is likely caused by a custom TUI component not truncating its output.
Use visibleWidth() to measure and truncateToWidth() to truncate lines.

Debug log written to: ${crashLogPath}
```

`ScrollView` 抛出的三个错误（**逐字**）：
```
Unsupported ScrollView axis: ${options.axis}
ScrollView has exactly one child
ScrollView child cannot be removed
ScrollView child cannot be cleared
```

`ProgramStatus` 相关（见 §2.3）：`APP_PATTERN = /^[A-Za-z0-9_.+-]{1,32}$/`、控制字符替换为空格、`MAX_MESSAGE_BYTES = 2048`。

### 3.4 键位表（`TUI_KEYBINDINGS`，47 项，**逐字**）

| id | 默认键 | 描述 |
| --- | --- | --- |
| `tui.editor.cursorUp` | `up` | Move cursor up |
| `tui.editor.cursorDown` | `down` | Move cursor down |
| `tui.editor.historyPrevious` | `[]` | Select previous prompt history entry |
| `tui.editor.historyNext` | `[]` | Select next prompt history entry |
| `tui.editor.cursorLeft` | `left`, `ctrl+b` | Move cursor left |
| `tui.editor.cursorRight` | `right`, `ctrl+f` | Move cursor right |
| `tui.editor.cursorWordLeft` | `alt+left`, `ctrl+left`, `alt+b` | Move cursor word left |
| `tui.editor.cursorWordRight` | `alt+right`, `ctrl+right`, `alt+f` | Move cursor word right |
| `tui.editor.cursorLineStart` | `home`, `ctrl+a` | Move to line start |
| `tui.editor.cursorLineEnd` | `end`, `ctrl+e` | Move to line end |
| `tui.editor.jumpForward` | `ctrl+]` | Jump forward to character |
| `tui.editor.jumpBackward` | `ctrl+alt+]` | Jump backward to character |
| `tui.editor.pageUp` | `pageUp`, `ctrl+pageUp` | Page up |
| `tui.editor.pageDown` | `pageDown`, `ctrl+pageDown` | Page down |
| `tui.editor.deleteCharBackward` | `backspace` | Delete character backward |
| `tui.editor.deleteCharForward` | `delete`, `ctrl+d` | Delete character forward |
| `tui.editor.deleteWordBackward` | `ctrl+w`, `alt+backspace` | Delete word backward |
| `tui.editor.deleteWordForward` | `alt+d`, `alt+delete` | Delete word forward |
| `tui.editor.deleteToLineStart` | `ctrl+u` | Delete to line start |
| `tui.editor.deleteToLineEnd` | `ctrl+k` | Delete to line end |
| `tui.editor.yank` | `ctrl+y` | Yank |
| `tui.editor.yankPop` | `alt+y` | Yank pop |
| `tui.editor.undo` | `ctrl+-` | Undo |
| `tui.input.newLine` | `shift+enter`, `ctrl+j` | Insert newline |
| `tui.input.submit` | `enter` | Submit input |
| `tui.input.tab` | `tab` | Tab / autocomplete |
| `tui.input.copy` | `ctrl+c` | Copy selection |
| `tui.select.up` | `up` | Move selection up |
| `tui.select.down` | `down` | Move selection down |
| `tui.select.pageUp` | `pageUp` | Selection page up |
| `tui.select.pageDown` | `pageDown` | Selection page down |
| `tui.select.confirm` | `enter` | Confirm selection |
| `tui.select.cancel` | `escape`, `ctrl+c` | Cancel selection |
| `tui.altScreen.pageUp` | `pageUp` | Scroll viewport up one page |
| `tui.altScreen.pageDown` | `pageDown` | Scroll viewport down one page |
| `tui.altScreen.halfPageUp` | `[]` | Scroll viewport up half a page |
| `tui.altScreen.halfPageDown` | `[]` | Scroll viewport down half a page |
| `tui.altScreen.lineUp` | `[]` | Scroll viewport up one line |
| `tui.altScreen.lineDown` | `[]` | Scroll viewport down one line |
| `tui.altScreen.previousPrompt` | `ctrl+shift+up`, `ctrl+up` | Jump to previous semantic prompt |
| `tui.altScreen.nextPrompt` | `ctrl+shift+down`, `ctrl+down` | Jump to next semantic prompt |
| `tui.altScreen.search` | `ctrl+shift+f` | Search the primary scroll view |
| `tui.altScreen.searchNext` | `enter`, `ctrl+g` | Select the next search match |
| `tui.altScreen.searchPrevious` | `shift+enter`, `ctrl+shift+g` | Select the previous search match |
| `tui.altScreen.searchClose` | `escape` | Close transcript search |
| `tui.altScreen.top` | `ctrl+home` | Scroll viewport to top |
| `tui.altScreen.bottom` | `ctrl+end` | Scroll viewport to bottom |

固定行为（不通过 keybinding 表）：
- `shift+ctrl+d` → 若 `tui.onDebug` 已设置，调用它并**吞掉**该输入。
- `alt+wheel` → 滚动距离 ×5（`ALT_WHEEL_SCROLL_MULTIPLIER = 5`）。
- 翻页重叠 `PAGE_SCROLL_OVERLAP = 4` 行。
- 双击间隔 `DOUBLE_CLICK_INTERVAL_MS = 500`。
- 复制错误闪烁时长 `COPY_ERROR_FLASH_DURATION_MS = 5000`。
- 普通闪烁时长 `DEFAULT_DURATION_MS = 1000`。

### 3.5 布局尺寸常量

```
MIN_RENDER_INTERVAL_MS (TuiBase)          = 16
MAX_RENDER_WRITE_CHARS (main screen)      = 1024 * 1024
PAGE_SCROLL_OVERLAP                       = 4
ALT_WHEEL_SCROLL_MULTIPLIER               = 5
MAX_CACHED_OFFSCREEN_KITTY_IMAGES         = 16
MAX_CACHED_OFFSCREEN_KITTY_TRANSMISSION_BYTES = 32 MiB
MAX_CACHED_OFFSCREEN_KITTY_DECODED_BYTES  = 64 MiB
DOUBLE_CLICK_INTERVAL_MS                  = 500
COPY_ERROR_FLASH_DURATION_MS              = 5000
DEFAULT_DURATION_MS (flash)               = 1000
scrollbarHideDelayMs (ScrollView)         = 1000（默认，`max(0, floor(...))`）
TERMINAL_PALETTE_SIZE                     = 16
TERMINAL_COLOR_REPLY_COUNT                = 18
DEFAULT_SEQUENCE_TIMEOUT_MS (StdinBuffer) = 50
DEFAULT_ESCAPE_TIMEOUT_MS (StdinBuffer)   = 10
DEFAULT_SSH_ESCAPE_TIMEOUT_MS             = 100
Kitty 图片分块                            = CHUNK_SIZE 4096（**base64 字符**，非原始字节）
Kitty 元数据表上限                        = 1000 条（LRU 淘汰）
tmux 超链接探针超时                       = 250 ms
X11 剪贴板：整体 deadline 3 s，单次操作 2000 ms，上限 50 MiB
默认终端尺寸                              = 80 × 24（读不到时）
```

`GetCellDimensions` 查询：`\x1b[16t`，回复 `\x1b[6;${height};${width}t`。

### 3.6 备用屏搜索框布局

宽度 `width` → `safeWidth = max(1, width)`，`innerWidth = max(0, safeWidth - 2)`：
- 第 1 行：`┌${"─".repeat(innerWidth)}┐`
- 第 2 行：`│${content}│`，`content` = 输入行 + 右侧内边距 + 结果文本；结果文本最多占 `innerWidth - 3`
- 第 3 行：`└${"─".repeat(leftRuleWidth)}${按钮? " ": ""}${按钮}${按钮? " ": ""}${"─".repeat(rightRuleWidth)}┘`
- `safeWidth === 1` → `["┌", "│", "└"]`
- 按钮可用宽度 `availableControlsWidth = max(0, innerWidth - 1*2 - 1)`；放不下时按钮退化为 `↑`/`↓`、分隔符 `" "`；再放不下则不显示按钮。
- 鼠标命中：仅 `row === 2`，`previousButtonStart ≤ column < previousButtonEnd` → -1；`nextButtonStart ≤ column < nextButtonEnd` → +1。

---

## 4. 外部格式（逐字）

### 4.1 环境变量

| 变量 | 读取位置 | 语义（**逐字**） |
| --- | --- | --- |
| `KIANA_TUI_ESC_TIMEOUT` | `resolveEscapeTimeoutMs` | `Number(env)`，有限且 `> 0` 时作为 StdinBuffer 的 `escapeTimeout`；否则按 SSH 规则回退 |
| `KIANA_TUI_WRITE_LOG` | `ProcessTerminal.writeLogPath`（构造时一次性求值） | 非空时每次 `write()` 追加写日志。若该路径是**已存在的目录**，文件名为 `tui-YYYY-MM-DD_HH-MM-SS-<pid>.log`（本地时间，月/日/时/分/秒均 2 位补零）；否则视为文件路径本身 |
| `KIANA_TUI_DEBUG` | `TuiMainScreen` 差分渲染末尾 | `=== "1"` 时把渲染调试快照写到 `/tmp/tui/render-<Date.now()>-<random base36>.log` |
| `KIANA_TUI_DEBUG_REDRAW` | `TuiMainScreen.fullRender` | `=== "1"` 时把全量重绘原因追加到 `<logDirectory>/kiana-tui-debug.log` |
| `KIANA_PROGRAM_STATUS` | `ProcessTerminal.queryAndEnableKittyProtocol` | `"1"`/`"0"` 时跳过 OSC 7501 查询；`"1"` 视为终端已支持 |
| `KIANA_HYPERLINKS` | `detectCapabilities` | `"1"`/`"0"` 覆盖 `hyperlinks`；其它值忽略 |
| `KIANA_IMAGE_PROTOCOL` | `detectCapabilities` | 小写后 `"kitty"`/`"iterm2"` → 指定协议；`"none"`/`"0"` → `null` |
| `KIANA_TRUE_COLOR` | `detectCapabilities` | `"1"`/`"0"` 覆盖 `trueColor` |
| `COLUMNS` / `LINES` | `ProcessTerminal.columns/rows` | `process.stdout.columns \|\| Number(COLUMNS) \|\| 80`；`rows \|\| Number(LINES) \|\| 24` |
| `TERMUX_VERSION` | `isTermuxSession` | 存在即视为 Termux：高度变化**不**触发全量重绘 |
| `SSH_CONNECTION` / `SSH_TTY` | `resolveEscapeTimeoutMs`、`terminalAcceleratesWheel` | 任一存在 → 默认 escape 超时 100 ms（否则 10 ms）；**同时**任一存在（含 `SSH_CLIENT`）都会让 `terminalAcceleratesWheel()` 返回 false（darwin 上不再对滚轮做行数放大，见 §2.4）；`SSH_CLIENT` 也参与 Windows Terminal 判定 |
| `SSH_CLIENT` | `isWindowsTerminalSession`、`terminalAcceleratesWheel` | 存在则不视为本地 Windows Terminal；存在则关闭滚轮加速 |
| `TERM` | 多处 | 小写后：`tmux`/`screen` 前缀判定、`tmux`/`screen` 用按键型鼠标上报、`-direct` 后缀判 truecolor |
| `TERM_PROGRAM` | 能力探测 / `isAppleTerminalSession` | `herdr`/`rex`/`kitty`/`ghostty`/`wezterm`/`warpterminal`/`iterm.app`/`alacritty`/`vscode`/`zed`；`Apple_Terminal`（仅 darwin） |
| `TERMINAL_EMULATOR` | 能力探测 | 小写后 `jetbrains-jediterm` → truecolor + 无超链接 |
| `COLORTERM` | 能力探测 | `truecolor` 或 `24bit` → `hasTrueColorHint` |
| `KITTY_WINDOW_ID` | 能力探测 | 存在 → kitty 全能力 |
| `GHOSTTY_RESOURCES_DIR` | 能力探测 | 存在 → ghostty 全能力 |
| `WEZTERM_PANE` | 能力探测 | 存在 → wezterm 全能力 |
| `WARP_SESSION_ID` / `WARP_TERMINAL_SESSION_UUID` | 能力探测 | 存在 → warp 全能力 |
| `ITERM_SESSION_ID` | 能力探测 | 存在 → iterm2 图片 |
| `WT_SESSION` | 能力探测 / `isWindowsTerminalSession` | 存在 → truecolor + 超链接，无图片 |
| `TMUX` / `ZELLIJ` / `STY` | 能力探测 / 鼠标模式 | 存在 → tmux 分支 / 按键型鼠标 |
| `DISPLAY` | `getNativeClipboard`（Linux） | 每次调用都检查；无值 → 返回 `undefined` |
| `KIANA_TEST_NATIVE_CLIPBOARD` | `test/native-platform.test.ts`（仅测试） | `"1"` 且 `process.platform === "win32"` 时才运行 `writes Windows clipboard text through the native helper without command fallbacks`；**该用例会覆盖系统剪贴板**。Rust 侧等价测试应沿用同名变量 + 同样 skip 语义 |
| `CMUX_WORKSPACE_ID` | 仅测试（`test/terminal-image.test.ts` 的 `ENV_KEYS`） | 源码**不读取**；测试显式断言它存在时**不会**改变 Ghostty 的能力判定（`does not disable Ghostty images solely because cmux is present`）。Rust 侧不要新增对该变量的分支 |
| `WSL_DISTRO_NAME` / `WSL_INTEROP` | coding-agent（非本单元） | — |

能力探测的**判定顺序（严格）**：
1. `TMUX || TERM 以 tmux 开头` → `{ images: null, trueColor: hint, hyperlinks: probeTmuxHyperlinks() }`
2. `TERM 以 screen 开头` → `{ images: null, trueColor: hint, hyperlinks: false }`
3. `TERM_PROGRAM === "herdr"` → `{ images: "kitty", trueColor: hint, hyperlinks: true }`
4. `TERM_PROGRAM === "rex"` → `{ images: "kitty", trueColor: true, hyperlinks: true }`
5. `KITTY_WINDOW_ID || TERM_PROGRAM === "kitty"` → kitty 全能力
6. `TERM_PROGRAM === "ghostty" || TERM 含 "ghostty" || GHOSTTY_RESOURCES_DIR` → kitty 全能力
7. `WEZTERM_PANE || TERM_PROGRAM === "wezterm"` → kitty 全能力
8. `TERM_PROGRAM === "warpterminal" || WARP_SESSION_ID || WARP_TERMINAL_SESSION_UUID` → kitty 全能力
9. `ITERM_SESSION_ID || TERM_PROGRAM === "iterm.app"` → `{ images: "iterm2", trueColor: true, hyperlinks: true }`
10. `WT_SESSION` → `{ images: null, trueColor: true, hyperlinks: true }`
11. `TERM_PROGRAM ∈ {alacritty, vscode, zed}` → `{ images: null, trueColor: true, hyperlinks: true }`
12. `TERMINAL_EMULATOR === "jetbrains-jediterm"` → `{ images: null, trueColor: true, hyperlinks: false }`
13. `process.platform === "win32"` → `{ images: null, trueColor: true, hyperlinks: false }`
14. 兜底 → `{ images: null, trueColor: hint, hyperlinks: false }`

`probeTmuxHyperlinks()`：执行
```
tmux display-message -p '#{client_termfeatures}'
```
（`encoding: "utf8"`、`timeout: 250` ms、`stdio: ["ignore","pipe","ignore"]`），
按 `,` 分割并 trim，是否包含 `hyperlinks`；任何异常 → `false`。

### 4.2 磁盘文件

| 文件 | 写入方 | 内容格式（**逐字**） |
| --- | --- | --- |
| `<KIANA_TUI_WRITE_LOG>` 或 `<dir>/tui-YYYY-MM-DD_HH-MM-SS-<pid>.log` | `ProcessTerminal.write` | 每次 `write(data)` 原样追加 `data`（UTF-8），错误忽略 |
| `<logDirectory>/kiana-tui-debug.log` | `TuiMainScreen`（`KIANA_TUI_DEBUG_REDRAW=1`） | 每行 `[<ISO8601>] fullRender: <reason> (prev=<n>, new=<n>, height=<h>)\n` 追加 |
| `<logDirectory ?? os.tmpdir()>/kiana-tui-crash.log` | `TuiMainScreen.fullRender` 超宽时 | 见下 |
| `/tmp/tui/render-<Date.now()>-<base36>.log` | `KIANA_TUI_DEBUG=1` | 见下（固定 `/tmp/tui`，非 `os.tmpdir()`） |

`fullRender` 的 reason 取值（**逐字**）：
```
first render
terminal width changed (<prev> -> <new>)
terminal height changed (<prev> -> <new>)
clearOnShrink (maxLinesRendered=<n>)
deleted lines moved viewport up (<targetRow> < <prevViewportTop>)
extraLines > height (<extra> > <height>)
firstChanged < viewportTop (<firstChanged> < <prevViewportTop>)
kitty image pre-clear would scroll (<startRow> + <reservedRows> > <height>)
```

崩溃日志（**逐字**，`\n` 连接）：
```
Crash at <new Date().toISOString()>
Terminal width: <width>
Line <i> visible width: <visibleWidth(line)>

=== All rendered lines ===
[<idx>] (w=<visibleWidth>) <line>
...
(末尾一个空行)
```

渲染调试快照（**逐字**，`\n` 连接）：
```
firstChanged: <n>
viewportTop: <n>
cursorRow: <n>
height: <n>
lineDiff: <n>
hardwareCursorRow: <n>
renderEnd: <n>
finalCursorRow: <n>
cursorPos: <JSON.stringify(cursorPos)>
newLines.length: <n>
previousLines.length: <n>

=== newLines ===
<JSON.stringify(newLines, null, 2)>

=== previousLines ===
<JSON.stringify(previousLines, null, 2)>

=== buffer ===
[<output.length> chars written in bounded chunks]
```

### 4.3 终端协议报文

**OSC 7501 程序状态（发送）**（**逐字**）：
```
\x1b]7501;state=<state>[:app=<app>][:kind=<kind>][:msg=<base64>]\x1b\\
```
- `state ∈ {idle, working, blocked, done, error, clear}`
- `app` 仅当匹配 `^[A-Za-z0-9_.+-]{1,32}$`
- `kind` 仅当 `state === "blocked"` 且非空
- `msg` = 将 `[\u0000-\u001f\u007f-\u009f]+` 替换为空格、`trim()`、按 UTF-8 截断到 2048 字节，再 base64

**OSC 7501 回复（接收）**：`/^\x1b\]7501;\?[^\x07\x1b]*(?:\x07|\x1b\\)$/`

**终端颜色回复**（`parseOscColorResponse`，`OSC_COLOR_RESPONSE_PATTERN`）：
```
\x1b]10;rgb:RRRR/GGGG/BBBB<term>      -> { target: "foreground", rgb }
\x1b]11;rgb:RRRR/GGGG/BBBB<term>      -> { target: "background", rgb }
\x1b]4;<index>;rgb:RRRR/GGGG/BBBB<term> -> { target: <index>, rgb }
<term> ∈ { BEL, ESC\ }
```
每组 1–4 位十六进制；取**最后一个匹配组**（重复组语义）；1 位扩展到 8 位再右移 8 位得到 0–255。

**主题报告**：`COLOR_SCHEME_REPORT_PATTERN` 匹配 OSC 997 的 `light`/`dark` 报告。

**DA1 回复**：`/^\x1b\[\?[\d;]*c$/`

**Kitty 键盘协商回复**：`/^\x1b\[\?([\d;]*)u$/`（`parseKeyboardProtocolNegotiationSequence`），
前缀测试 `sequence === "\x1b[" || /^\x1b\[\?[\d;]*$/`。

**单元格尺寸回复**：`\x1b[6;<height>;<width>t`。

**Kitty 图形协议（发送）**：`\x1b_G<key=value,...>;<base64 payload>\x1b\\`，payload 按 **4096 个 base64 字符**分块
（源码 `CHUNK_SIZE = 4096`，切的是 `base64Data` **字符串**：`base64Data.slice(offset, offset + CHUNK_SIZE)`，
且 `base64Data.length <= CHUNK_SIZE` 时直接单条发送；等价于 3072 原始字节/块，见 §4 常量表）；
删除命令带 `q=2`（抑制回复）。**iTerm2 图片**：`\x1b]1337;File=<args>:<base64>\x07`，其中含
`inline=1`、`size=<bytes>`、`name=<base64>`、`width=`/`height=`。

**鼠标**：
- SGR：`\x1b[<${b};${x};${y}M`（按下/移动）与 `\x1b[<${b};${x};${y}m`（释放）
- X10：`\x1b[M` + 3 个字节（`code+32`、`x+33`、`y+33`）
- 滚轮：button 64（上） / 65（下）；Alt 为 `+8`；Shift `+4`；Ctrl `+16`

**括号粘贴**：`\x1b[200~` … `\x1b[201~`。

### 4.4 配置/日志中的数值约定

- 时间戳：`new Date().toISOString()`（UTC，`YYYY-MM-DDTHH:mm:ss.sssZ`）。
- 本地文件时间戳：`YYYY-MM-DD_HH-MM-SS`（本地时区，各段补零）。
- 随机后缀：`Math.random().toString(36).slice(2)`（**变长**，Rust 侧只需保证唯一性；若测试依赖格式需可注入）。

---

## 5. 跨单元接口（精确类型）

### 5.1 本单元对外提供的核心 trait/接口

Rust 侧建议以 trait 形式提供，字段语义与 TS 完全一致：

```rust
// 组件：唯一的硬约束是 render(width) 的每行可见宽度 <= width
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> { None }
    fn wants_key_release(&self) -> bool { false }
    fn invalidate(&mut self) {}
}

pub trait Focusable { fn focused(&self) -> bool; fn set_focused(&mut self, focused: bool); }
```

`isFocusable(component) = component !== null && "focused" in component`（鸭子判定；Rust 用 `Any` downcast
或显式 `as_focusable()` 方法）。

`Container`（非抽象，可直接实例化）：`children: Vec<Box<dyn Component>>`；`render(width)` 逐子渲染并拼接；
`handleInput` 逐个转发，命中即停；`handleMouse` 逆序（最后一个子组件优先）命中测试；
`invalidate()` 递归。

```ts
export interface TuiInputListenerResult { consume?: boolean; data?: string; }
export type TuiInputListener = (data: string) => TuiInputListenerResult | undefined;

export const VIEWPORT_TUI = Symbol.for("@earendil-works/kiana-tui/viewport");
export function isViewportTUI(tui: TUI): tui is ViewportTUI;
export interface ViewportTUI extends TUI {
  readonly [VIEWPORT_TUI]: true;
  setLayoutRoot(component: Component | undefined): void;
}
```

### 5.2 `TUI` 接口（跨包使用最广，**逐字**）

```ts
export interface TUI extends Component {
  readonly mode: TuiMode;                       // "regular" | "fullscreen"
  children: Component[];
  terminal: Terminal;
  onDebug?: () => void;
  readonly fullRedraws: number;
  addChild(component: Component): void;
  removeChild(component: Component): void;
  clear(): void;
  getShowHardwareCursor(): boolean;
  setShowHardwareCursor(enabled: boolean): void;
  getClearOnShrink(): boolean;
  setClearOnShrink(enabled: boolean): void;
  setFocus(component: Component | null): void;
  showOverlay(component: Component, options?: OverlayOptions): OverlayHandle;
  hideOverlay(): void;
  hasOverlay(): boolean;
  start(): void;
  stop(options?: TuiStopOptions): void;
  renderNow(force?: boolean): void;
  requestRender(force?: boolean): void;
  addInputListener(listener: TuiInputListener): () => void;
  removeInputListener(listener: TuiInputListener): void;
  onTerminalColorSchemeChange(listener: (scheme: TerminalColorScheme) => void): () => void;
  setTerminalColorSchemeNotifications(enabled: boolean): void;
  queryTerminalColors(options: { timeoutMs: number; onLateReply?: (colors: TerminalColors) => void }): Promise<TerminalColors>;
}
```

`TuiStopOptions = { preserveScreen?: boolean }`。`mode` 在 `TuiMainScreen` 为 `"regular"`，
在 `TuiAltScreen` 为 `"fullscreen"`。

`TuiBase` 额外公开（非 `TUI` 接口的一部分，但被测试和 coding-agent 使用）：
`getFocusedComponent(): Component | null`、`get hasOverlayEntries(): boolean`、`get fullRedraws(): number`；
另有 `protected isOverlayFocused(): boolean`（源码 `tui.ts:878`，**不是** `getFocusedOverlay`——
后者不存在，早期草稿里的这个名字是错的）。
`getKeybindings()` 不在此处（独立全局）、`setLayoutRoot(component)`（仅 `ViewportTUI`）。

### 5.3 `Terminal` 接口（**逐字**）

```ts
export interface Terminal {
  start(onInput: (data: string) => void, onResize: () => void): void;
  stop(): void;
  restoreOnExit?(before: string, after: string): void;
  drainInput(maxMs?: number, idleMs?: number): Promise<void>;   // 默认 1000 / 50
  write(data: string): void;
  get columns(): number;
  get rows(): number;
  get kittyProtocolActive(): boolean;
  moveBy(lines: number): void;
  hideCursor(): void;
  showCursor(): void;
  clearLine(): void;
  clearFromCursor(): void;
  clearScreen(): void;
  setTitle(title: string): void;
  setProgress(active: boolean): void;
  setProgramStatus(status: ProgramStatus): void;
}
```

`ProcessTerminal` 另外公开：`modifyOtherKeysActive: boolean`（getter）、`logDirectory`（构造参数）。

### 5.4 具体类型清单（供各单元共用，**逐字**）

```ts
export type TuiMode = "regular" | "fullscreen";
export type TuiMouseEventType = "press" | "release" | "move" | "drag" | "click" | "wheel";
export type TuiMouseButton = "left" | "middle" | "right" | "none";
export interface TuiMouseEvent {
  type: TuiMouseEventType; button: TuiMouseButton;
  x: number; y: number;               // 相对接收组件
  screenX: number; screenY: number;   // 终端绝对坐标
  width: number; height: number;      // 组件当前边界
  shift: boolean; alt: boolean; ctrl: boolean;
  wheelDelta?: number;                // 逻辑行，负值向上
  clickCount?: number;                // 仅 type === "click"
}
export interface TuiMouseEventResult {
  handled?: boolean; capture?: boolean; focus?: boolean; render?: boolean;
}
// 默认渲染语义：move/release 默认 false；press/click/drag/wheel 默认 true

export type OverlayAnchor = "center" | "top-left" | "top-right" | "bottom-left" | "bottom-right"
  | "top-center" | "bottom-center" | "left-center" | "right-center";
export interface OverlayMargin { top?: number; right?: number; bottom?: number; left?: number; }
export type SizeValue = number | `${number}%`;
export interface OverlayOptions {
  width?: SizeValue; minWidth?: number; maxHeight?: SizeValue;
  anchor?: OverlayAnchor; offsetX?: number; offsetY?: number;
  row?: SizeValue; col?: SizeValue;
  margin?: OverlayMargin | number;
  visible?: (termWidth: number, termHeight: number) => boolean;
  nonCapturing?: boolean;
}
export interface OverlayBounds { row: number; col: number; width: number; height: number; }
export interface OverlayUnfocusOptions { target: Component | null; }
export interface OverlayHandle {
  hide(): void; setHidden(hidden: boolean): void; isHidden(): boolean;
  focus(): void; unfocus(options?: OverlayUnfocusOptions): void;
  isFocused(): boolean; getBounds(): OverlayBounds | undefined;
}

export interface RgbColor { r: number; g: number; b: number; }
export interface TerminalColors { foreground?: RgbColor; background?: RgbColor; palette?: RgbColor[]; }
export type TerminalColorScheme = "dark" | "light";
export type ImageProtocol = "kitty" | "iterm2" | null;
export interface TerminalCapabilities { images: ImageProtocol; trueColor: boolean; hyperlinks: boolean; }
export interface CellDimensions { widthPx: number; heightPx: number; }
export interface ImageDimensions { widthPx: number; heightPx: number; }
export interface ImageRenderOptions {
  maxWidthCells?: number; maxHeightCells?: number; preserveAspectRatio?: boolean;
  imageId?: number; moveCursor?: boolean;
}
export interface ProgramStatus {
  state: "idle" | "working" | "blocked" | "done" | "error" | "clear";
  app?: string; kind?: "permission" | "question" | "auth"; message?: string;
}
export interface TuiMainScreenRenderState {
  previousLines: string[]; previousWidth: number; previousHeight: number;
  cursorRow: number; hardwareCursorRow: number; maxLinesRendered: number; previousViewportTop: number;
}
export interface TuiAltScreenOptions {
  wheelScrollLines?: WheelScrollLines;           // number | "auto"，默认 1
  mouse?: boolean;                                // 默认 true
  searchMatchStyle?: (text: string) => string;
  searchCurrentMatchStyle?: (text: string) => string;
  searchNavigationButtonStyle?: (text: string, hovered: boolean) => string;
  scrollToEndIndicator?: () => string;
  openUrl?: (url: string) => void;
  onRightClickPaste?: () => void;                 // 仅 Windows 非 VS Code 生效
  copyOnSelect?: boolean;                         // 默认 true
  copySelection?: (text: string) => Promise<boolean | string>;
}
```

### 5.5 下游（`packages/coding-agent`）实际依赖的符号

统计 100 个不同的符号（按引用次数降序，前 60）：

```
Container(52) TUI(47) Text(47) Spacer(32) Component(26) setKeybindings(24) getKeybindings(19)
visibleWidth(14) Focusable(12) Input(12) truncateToWidth(10) MarkdownTheme(9) TuiMainScreen(9)
fuzzyFilter(9) Markdown(8) Box(8) SelectItem(8) KeyId(7) Terminal(6) TuiMouseEvent(6) SelectList(6)
getCapabilities(5) MouseRegion(5) rgbColor(5) setCapabilityOverrides(4) WheelScrollLines(4)
ProcessTerminal(4) EditorComponent(4) Keybinding(4) hyperlink(4) SelectListLayoutOptions(4)
foregroundAnsi(4) RgbColor(4) resetCapabilitiesCache(4) TuiAltScreen(3) ScrollViewScrollbar(3)
AutocompleteProvider(3) matchesKey(3) TruncatedText(3) Editor(3) EditorTheme(3) Loader(3) Color(3)
parseColor(3) backgroundAnsi(3) colorToRgb(3) TerminalColors(3) colorToHex(3) colorToOkhsl(3)
okhslColor(3) setCapabilities(3) ScrollView(2) ProgramStatus(2) AutocompleteItem(2)
OverlayHandle(2) OverlayOptions(2) CombinedAutocompleteProvider(2) TuiMouseEventResult(2) Image(2)
EditorOptions(2) Key(2) mixColors(2) indexedColor(2) getTerminalColorMode(2) TerminalColorMode(2)
getNativeClipboard(2) NativeClipboard(2) VStack(1) SlashCommand(1) TuiMainScreenRenderState(1)
SettingItem(1) SettingsList(1) Marked(1) Token(1) fuzzyMatch(1) sliceByColumn(1)
wrapTextWithAnsi(1) CancellableLoader(1) isAppleTerminalSession(1) SelectListTheme(1)
```

**扩展点（Rust 必须保留）**：
1. `Keybindings` 接口通过 TypeScript declaration merging 由下游扩展（`app.*` 键位）。Rust 侧
   `Keybindings` 不能是封闭 enum；必须是 `String` id（或 newtype）的开放注册表。
   下游新增的 id 见 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/core/keybindings.ts`。
2. `TUI` 必须是 trait 而非具体类型，因为 coding-agent 用 `Proxy` 包了一层
   （`createInteractiveTuiReference`），并在运行时切换 `TuiMainScreen` / `TuiAltScreen`。
3. `TuiAltScreenOptions` 的回调（`scrollToEndIndicator`、`openUrl`、`copySelection`）必须能被宿主注入。
   实际使用见 `/home/ctyun/code/kiana_rev/kiana/packages/coding-agent/src/modes/interactive/tui-renderer.ts`：
   `scrollToEndIndicator` 返回 `` ` ↓ Jump to latest message${shortcut ? ` · ${shortcut}` : ""} ``
   （含主题着色），`wheelScrollLines` 默认 `"auto"`，`openUrl = openBrowser`。
4. `setCapabilities` / `setCapabilityOverrides` / `resetCapabilitiesCache` 被下游用于主题与测试。
5. `getNativeClipboard()` 返回 `NativeClipboard | undefined`，coding-agent 直接调用 `getText`/`setText`。

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
`engines.node = ">=22.19.0"`，`type = "module"`，包版本 `1.4.0`，MIT。

**`package.json` 其余字段（Rust 侧发布/packaging 需对齐的契约）**：
```json
"name": "@earendil-works/kiana-tui",
"description": "Terminal User Interface library with differential rendering for efficient text-based applications",
"main": "dist/index.js",
"types": "./dist/index.d.ts",          // 无 "exports" 字段 → 只有单入口，无子路径导出
"scripts": {
  "clean": "shx rm -rf dist",
  "build": "tsc -p tsconfig.build.json",
  "build:native:darwin": "native/darwin/build.sh",
  "build:native:linux":  "native/linux/build.sh",
  "build:native:win32":  "node native/win32/build.mjs",
  "test": "node --test --test-reporter=dot --test-reporter-destination=stdout test/*.test.ts",
  "prepublishOnly": "npm run clean && npm run build"
},
"author": "Mario Zechner",
"license": "MIT",
"repository": { "type": "git", "url": "git+https://github.com/earendil-works/kiana.git", "directory": "packages/tui" },
"keywords": ["tui","terminal","ui","text-editor","differential-rendering","typescript","cli"]
```
`files` 白名单（**决定 `.node` 预编译产物必须随包分发**，否则 `getNativeModuleCandidates` 在安装态找不到）：
`dist/**/*`、`native/napi.h`、`native/clipboard.h`、`native/{win32,darwin}/prebuilds/**/*.node`、
`native/win32/src/**/*.c`、`native/win32/build.mjs`、`native/darwin/src/**/*.m`、`native/darwin/build.sh`、
`native/linux/prebuilds/**/*.node`、`native/linux/src/**/*.{c,h}`、`native/linux/build.sh`、
三个平台的 `README.md`、包根 `README.md`。
注意：`test --test-reporter=dot` 默认 glob 为 `test/*.test.ts`（**只跑顶层**，不递归子目录）；
Rust 侧 `cargo test` 无此限制，但用例集合应以该 glob 覆盖的文件为准。
`marked` 仅在 `index.ts` 再导出（`Marked`/`Token`/`Tokens`）与 `components/markdown.ts`（单元 tui-components）使用；
`get-east-asian-width`、`Intl.Segmenter` 由 `src/utils.ts`（单元 tui-text）使用。
本单元（tui-core）直接使用的 **Node 内置模块**：`node:path`、`node:url`、`node:os`、`node:module`、
`node:fs`、`node:perf_hooks`（`performance.now`）、`node:child_process`（`execSync`，仅 tmux 探针）、
`node:events`（`EventEmitter`）、`Buffer`（base64）。

| npm / 平台 API | 用途（本单元） | Rust crate / 方案 | 差异与桥接 |
|---|---|---|---|
| `get-east-asian-width` 1.6.0 | `visibleWidth` 的东亚宽度判定（经 `utils.ts`） | `unicode-width` 0.2 **或内嵌 EAW 表** | `unicode-width` 的 EAW 表版本与 `get-east-asian-width@1.6.0` 可能不同（Unicode 15.1 vs 16.0 的新增字符）。**必须**逐码点比对 `test/visible-width.test.ts`、`test/regression-regional-indicator-width.test.ts`、`test/tab-width.test.ts`，不同处内嵌覆盖表。注意 `get-east-asian-width` 返回 0/1/2，且对未分配码点返回 1 |
| `Intl.Segmenter`（ICU） | grapheme / word 分段 | `unicode-segmentation`（UAX #29）**或** `icu_segmenter`（ICU4X） | `Intl.Segmenter` 用 ICU 数据，`unicode-segmentation` 用 UAX #29 默认表；在扩展字素簇（emoji ZWJ、国旗、Indic conjunct）上两者通常一致，但**版本差异**会导致边界不同。建议用 `icu_segmenter`（与 ICU 同源），并用 `test/regression-regional-indicator-width.test.ts` 校准。`utils.ts` 的 `getWordSegmenter()` 还需 `isWordLike` 语义（见 tui-text 规格） |
| `marked` 18.0.11 | 仅类型再导出（`Marked`/`Token`/`Tokens`） | 无（Rust 侧 tui-components 的 markdown 单元自实现） | `index.ts` 的这三个符号在 Rust 侧可省略；若保留需定义等价的 `Token` 结构 |
| `marked-cjk-friendly` 0.1.2 | markdown 的 CJK 断行（非本单元） | n/a | |
| `Buffer.from(str,"utf8").toString("base64")` | OSC 52 复制、OSC 7501 `msg`、iTerm2 `name=` | `base64` crate（`STANDARD`） | 标准字母表 + `=` 填充，无换行。**不要**用 URL-safe 变体 |
| `Buffer.byteLength(str,"utf8")` | `truncateUtf8`（2048 字节上限） | `str.len()`（UTF-8 字节） | 需按 UTF-8 边界截断且**不**产生半个码点 |
| `Buffer.from(data).toString("base64")`（分块） | Kitty 图片分块（`CHUNK_SIZE = 4096`） | `base64::encode` | 源码切的是 **base64 字符串**的下标区间（`base64Data.slice(offset, offset + 4096)`），**不是**原始字节；Rust 侧必须先整体 encode 再按 4096 **字符**切片（每块末块可短），否则 `m=1` 续块边界与 `payload` 字节数都会与原实现不同 |
| `node:path` | `join`、`dirname` | `std::path::Path` | Windows 分隔符与 `pathe` 差异；本单元只用于日志/模块路径 |
| `node:url` `pathToFileURL` / `fileURLToPath` | `imageFallback` 的 `file://` 链接、`getNativeModuleCandidates` 的 `import.meta.url` | `url` crate `Url::from_file_path` | `pathToFileURL` 对 `#`、`?`、空格、非 ASCII 做百分号编码；`Url::from_file_path` 的编码集合需逐字符核对（尤其 `%` 与空格 → `%20`） |
| `node:os` `homedir()` / `tmpdir()` | `shortenImagePath`、崩溃日志默认目录 | `dirs::home_dir` / `std::env::temp_dir` | `tmpdir()` 尊重 `TMPDIR`；崩溃日志路径会出现在**用户可见的错误消息**里，必须与 Node 行为一致 |
| `node:module` `createRequire` + `require.resolve` | `getNativeModuleCandidates` | `libloading` / 直接路径查找 | Rust 侧改为"可执行文件目录 + 若干候选路径"，无需 resolve 包 |
| `node:fs` `statSync/appendFileSync/writeFileSync/mkdirSync` | 日志写入 | `std::fs` | 全部为**同步**写入；Rust 侧保持同步以避免渲染顺序错乱 |
| `node:perf_hooks` `performance.now()` | 渲染节流、滚轮加速 | `std::time::Instant` | 单调时钟；`performance.now()` 是相对进程启动的浮点毫秒。Rust 用 `Instant::elapsed().as_secs_f64()*1000.0` |
| `node:child_process` `execSync` | tmux 超链接探针 | `std::process::Command` + 超时 | `timeout: 250` 需自己实现（`wait_timeout` crate 或 spawn + `try_wait` 轮询） |
| `node:events` `EventEmitter` | `StdinBuffer` 的 `data`/`paste` | 回调 / `tokio::sync::mpsc` | Rust 侧建议直接暴露 `fn process(&mut self, data: &[u8])` + 回调或返回 `Vec<Event>` |
| `setTimeout` / `clearTimeout` / `timer.unref()` | 渲染节流、闪烁、滚动条隐藏、StdinBuffer 超时、进度 keepalive | `tokio::time::sleep` + `JoinHandle::abort`，或自建单线程定时器堆 | **unref 语义**：定时器不得阻止进程退出。Rust 中用 `tokio::spawn` 且不 join 即满足 |
| `process.stdout.write` / `fs.writeSync(fd)` | 终端输出 | `std::io::stdout().write_all` + `flush` | `restoreOnExit` 必须用**同步**写（绕过缓冲） |
| `process.stdin` raw mode / `SIGWINCH` | 输入与 resize | `crossterm` 或 `termios` + `signal_hook` | macOS/Linux 的 raw mode 标志位需与 Node 的 `setRawMode(true)` 一致（尤其 `ICRNL`/`IXON`） |
| `process.kill(process.pid, "SIGWINCH")` | `refreshTerminalDimensions` | `libc::kill` | 必须吞掉 `EACCES`（见 `test/regression-sigwinch-kill-eacces.test.ts`），其它 errno 继续抛 |
| N-API 原生剪贴板 | `getNativeClipboard()` / `isNativeModifierPressed()` | `arboard`（跨平台）或 `x11rb` + `objc2-app-kit` + `windows` crate | **行为差异**：`arboard` 的 `get_text` 在 X11 上可能返回 `Err` 而非 `undefined`；需要区分"unavailable / empty / transfer failed"三态。Linux 的 3 秒私有线程 + 2000 ms XCB 超时需自行实现 |
| Kitty / iTerm2 图片 | 编码与裁剪 | 自研（纯字符串） | 无外部依赖；PNG/JPEG/GIF/WebP 头解析已在本单元内（§2.15） |

### 6.2 关键桥接点（按风险排序）

1. **JS 字符串是 UTF-16**。本单元受影响的算法：
   - `keys.ts`：`parseKey` 的 `data.length === 1`、`rawCtrlChar` 的 `charCodeAt(0) & 0x1f`、Kitty 序列里的 `String.fromCodePoint(codepoint)`；
   - `stdin-buffer.ts`：`buffer.length`、`slice`、`indexOf`、`startsWith`；**必须按 UTF-16 code unit 切分**，
     否则把增补平面字符（emoji）拆成两半的行为会不同；
   - `tui-main-screen.ts` 的 `BoundedTerminalWriter`：按 `MAX_RENDER_WRITE_CHARS = 1024*1024` 个 **UTF-16 code unit**
     切分，且**不得**拆开代理对（相邻两个 code unit 组合成 `\u{10000}` 以上字符时延后切）；
   - `tui.ts`：`extractCursorPosition` 的 `indexOf(CURSOR_MARKER)`、`resolveFakeCursors` 的正则。
   - **决策**：`stdin-buffer` 与 `BoundedTerminalWriter` 内部用 `Vec<u16>`/`Utf16String`；其余用 `String` +
     显式 UTF-16 索引适配层。**不要**把 `data.length === 1` 写成 `chars().count() == 1`（emoji 是 1 个 char、2 个 code unit）。

2. **正则方言**。本单元用到的全部正则（逐字）：
   | 文件 | 正则 | 用途 / Rust 注意 |
   |---|---|---|
   | `terminal-colors.ts` | `/^\x1b\](?:(1[01])|4;(\d{1,3}));([^\x07\x1b]*)(?:\x07|\x1b\\)$/i` | Rust `(?i)^\x1b\]...`；`.*` 对 `[^\x07\x1b]` 无跨行问题 |
   | `terminal-colors.ts` | `/^(?:\x1b\[\?997;(1|2)n)+$/` | 重复捕获组取最后一次，Rust 需手写扫描 |
   | `terminal-colors.ts` | `/^[0-9a-f]+$/i`、`/^[0-9a-f]{6}$/i`、`/^[0-9a-f]{12}$/i`、`/^rgba?:/i`、`/#/` | 直接可用 |
   | `program-status.ts` | `/^\x1b\]7501;\?[^\x07\x1b]*(?:\x07|\x1b\\)$/`、`/^[A-Za-z0-9_.+-]{1,32}$/`、`/[\u0000-\u001f\u007f-\u009f]+/g` | 直接可用 |
   | `tui.ts` | `/(\x1b_kiana:c\x07)\x1b_kiana:fc\x07(.*?)(?:\x1b_kiana:\/fc\x07\|$)/s` | `s` 标志 = Rust `(?s)`；非贪婪跨行 |
   | `tui.ts` | `/^\x1b\[\?[\d;]*c$/` | 直接可用 |
   | `tui-alt-screen.ts` | `/^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+/`、`/^\x1b\]133;A(?:\x07|\x1b\\)/` | 重复组语义 |
   | `keys.ts` | 4 个 Kitty 解析正则、legacy 序列表（非正则） | 见 §2.5 |
   | `stdin-buffer.ts` | `TERMINAL_REPLY_HEADER_PATTERN`、CSI/OSC/DCS 解析 | 见 §2.7 |
   | `terminal-image.ts` | 图片序列探测、`isImageLine` | 见 §2.15 |
   | `utils.ts`（非本单元） | `\p{...}`、`/v` 集合、`\p{RGI_Emoji}` | Rust `regex` **不支持** `/v` 集合与 `\p{RGI_Emoji}`；需展开为显式字符类或使用 `regex` + 自定义判定 |

3. **JSON 键顺序**：本单元只在 `KIANA_TUI_DEBUG` 快照里 `JSON.stringify(newLines, null, 2)`（数组，无键序问题）
   与 `JSON.stringify(cursorPos)`（对象 `{row, col}`，键序固定）。Rust 侧用 `serde_json` + 显式有序结构即可。

4. **数字格式化**：所有数值都通过模板字符串插入，等价于 JS 的 `Number::toString`：
   - 整数（尺寸、行列号、超时）→ Rust `{}` 对 `i64`/`u32` 一致；
   - `performance.now()` 差值只参与比较与 `Math.max(0, ...)`，不输出；
   - `(this.averageGap + gap) / 2` 与 `REFERENCE_GAP_MS / averageGap` 是浮点，`Math.floor` 后输出整数；
     需用 `f64` 并在 `.floor() as i64` 前保持 IEEE-754 语义（Rust 默认即为 IEEE-754）。
   - `new Date().toISOString()` → `YYYY-MM-DDTHH:mm:ss.sssZ`（毫秒**恒 3 位**）；Rust 用
     `time::OffsetDateTime::now_utc().format(&Rfc3339)` 会省略 `.000`，必须自行格式化为 3 位毫秒。

5. **日期格式化**：`KIANA_TUI_WRITE_LOG` 目录模式的时间戳是**本地时间** `YYYY-MM-DD_HH-MM-SS`
   （月/日/时/分/秒补零，年 4 位）。Rust 用 `chrono::Local` 或 `time` + `UtcOffset` 本地偏移。

6. **`String.fromCharCode` vs `String.fromCodePoint`**：`keys.ts` 里两者都出现。
   `fromCharCode(0x1f)` 之类的 BMP 值两者等价；但 `decodeKittyPrintable` 里对 > 0xFFFF 的码点**必须**用
   `fromCodePoint`（Rust `char::from_u32`）。见 §2.5。

7. **`Math.round` 的 .5 行为**：JS `Math.round(-0.5) === -0`，`Math.round(2.5) === 3`（向 +∞ 取整）。
   Rust `f64::round()` 是"远离零"，**不同**。出现在 `getScrollbarGeometry` 与 `calculateImageRows`，
   必须写成 `(x + 0.5).floor()`（对负数需保持 JS 语义）。

8. **`Number.isFinite` / `Math.trunc`**：JS 对 `NaN`/`Infinity` 有特判，Rust 侧 `Option<f64>` +
   `is_finite()` 对应；`Math.trunc(-1.7) === -1`，Rust `f64::trunc()` 一致。

9. **`Array.prototype.sort` 稳定性**：本单元未依赖排序稳定性（无 `sort` 调用），但 `Map` 的插入顺序语义
   被大量使用（`uploadedKittyImages`、`keysById`、`helpers`）。Rust 用 `IndexMap` 或 `Vec<(K,V)>`。

10. **`structuredClone` / `spread` 浅拷贝**：`getConflicts()`、`getUserBindings()`、`getKeys()` 返回副本，
    防止调用方改动内部状态。Rust 用 `clone()`。

---

## 7. 测试清单（逐文件，用于改写为 Rust 测试）

### 7.0 测试基础设施

- 框架：`node --test`（`node:test` 的 `describe`/`it`/`test`），断言用 `node:assert`。
- **`test/virtual-terminal.ts`**：用 `@xterm/headless` 5.5.0 做真实终端仿真。Rust 侧需要一个等价物：
  建议集成 `vt100` crate 或 `avt`（Alacritty 的 `vte` + 自写屏幕模型）并**逐字节**比较写入流。
  行为要点：
  - `start()` 写入 `\x1b[?2004h`；`stop()` 写入 `\x1b[?2004l`；
  - `kittyProtocolActive` **恒为 true**；
  - `flush()` 通过 `xterm.write("", cb)` 等待写入队列；
  - `waitForRender()` = `nextTick` + `sleep(20ms)` + `flush()`；
  - `getViewport()` 用 `line.translateToString(true)`（**trim 右侧空白**）；
  - `resize(cols, rows)` 同时更新 xterm 并调用 resize 回调。
  Rust 侧建议实现 `TestTerminal`：记录所有 `write` 字符串、用 `vt100::Parser` 维护屏幕、
  `send_input()` 回调注入、`resize()` 触发回调。
- **`test/fixtures/exit-without-stop.ts`**（**逐字**全文）：
  ```ts
  import { ProcessTerminal } from "../../src/terminal.ts";
  import { TuiAltScreen } from "../../src/tui-alt-screen.ts";

  const tui = new TuiAltScreen(new ProcessTerminal(), false, undefined, { mouse: true });
  tui.start();
  if (process.argv[2] === "stop") tui.stop({ preserveScreen: true });
  process.exit(0);
  ```
  即：`exit` 模式**不调用** `stop()` 直接 `process.exit(0)`；`stop` 模式调用
  `tui.stop({ preserveScreen: true })` 后退出。Rust 侧用 `std::process::Command` 运行一个测试二进制
  （或 `#[test]` 内 fork+exec 自身），断言 stdout 的序列计数与顺序（§7.15）。
- **native fixture 套件**（`test/fixtures/`，供 `native-clipboard-linux.test.ts` 使用）：
  `clipboard-reader.cjs`、`clipboard-worker-test.c`、`clipboard-worker-test.cjs`、`clipboard-x11-test.c`。
  这些是 C 程序 + Node 驱动，Rust 侧可改写为 `x11rb` 的集成测试（无 `DISPLAY` 时 skip）。
- 辅助类：`TestComponent`（`lines` 数组 + `render` 返回）、`InputComponent`（记录 `renderCount`，
  `handleInput` 把输入写进 `lines`）、`BoundedWriteTerminal`（记录 writes，80×24）、
  `LoggingVirtualTerminal extends VirtualTerminal`（保存写入流）、`StaticOverlay`、
  `CursorTrackingTerminal`（记录光标可见性）、`withEnv()`（临时改 `process.env` 并恢复）。
- **环境隔离**：大量用例依赖 `withEnv` 清空 `TMUX`/`STY`/`ZELLIJ`/`TERM`/`TERMUX_VERSION`/`WARP_*`。
  Rust 测试必须串行执行（或使用 `serial_test`），因为能力探测与 `getKeybindings()` 是**全局单例**。

### 7.1 本单元直接相关的测试文件（按归属）

| 文件 | it 数 | 归属 | 断言要点 |
|---|---|---|---|
| `test/tui-render.test.ts` | 32 | 本单元（主屏渲染） | 见 §7.2 |
| `test/tui-alt-screen.test.ts` | 68 | 本单元（备用屏） | 见 §7.3 |
| `test/layout.test.ts` | 15 | 本单元（布局器） | 见 §7.4 |
| `test/overlay-non-capturing.test.ts` | 44 | 本单元（焦点/overlay 状态机） | 见 §7.5 |
| `test/overlay-options.test.ts` | 26 | 本单元（overlay 几何） | 见 §7.6 |
| `test/overlay-short-content.test.ts` | 1 | 本单元 | 见 §7.6 |
| `test/tui-overlay-style-leak.test.ts` | 2 | 本单元（`compositeTuiLine`） | 见 §7.6 |
| `test/regression-overlay-cjk-boundary.test.ts` | 4 | 本单元（`compositeTuiLine` 宽字素） | 见 §7.6 |
| `test/program-status.test.ts` | 5 | 本单元 | 见 §7.7 |
| `test/wheel-scroll.test.ts` | 6 | 本单元 | 见 §7.7 |
| `test/keys.test.ts` | 61 | 本单元 | 见 §7.8 |
| `test/keybindings.test.ts` | 7 | 本单元 | 见 §7.8 |
| `test/stdin-buffer.test.ts` | 66（含参数化模板） | 本单元 | 见 §7.9 |
| `test/terminal.test.ts` | 27 | 本单元 | 见 §7.10 |
| `test/terminal-colors.test.ts` | 6 | 本单元 | 见 §7.11 |
| `test/terminal-image.test.ts` | 80 | 本单元（§2.15） | 见 §7.12 |
| `test/bug-regression-isimageline-startswith-bug.test.ts` | 11 | 本单元（`isImageLine`） | 见 §7.12 |
| `test/native-clipboard-linux.test.ts` | 10（含参数化模板） | 本单元（native） | 见 §7.13 |
| `test/native-module-path.test.ts` | 2 | 本单元 | 见 §7.13 |
| `test/native-platform.test.ts` | 4（用 `test(` 而非 `it(`） | 本单元 | 见 §7.13 |
| `test/mouse-components.test.ts` | 9（含参数化模板） | 本单元 + tui-components（鼠标分发 + Input/SelectList/SettingsList/Editor） | 见 §7.14 |
| `test/exit-restore.test.ts` | 2 | 本单元 | 见 §7.15 |
| `test/tui-shrink.test.ts` | 1 | 本单元 | 见 §7.2 |
| `test/regression-sigwinch-kill-eacces.test.ts` | 3 | 本单元（`refreshTerminalDimensions`） | 见 §7.16 |
| `test/tui-cell-size-input.test.ts` | 2 | 本单元（`\x1b[16t` 回复消费） | 见 §7.16 |
| `test/regression-regional-indicator-width.test.ts` | 5 | tui-text（宽度）+ 本单元（列表渲染漂移） | 见 §7.17 |
| `test/visible-width.test.ts` | 4 | tui-text | 见 §7.17 |
| `test/wrap-ansi.test.ts` | 19 | tui-text | 见 §7.17 |
| `test/truncate-to-width.test.ts` | 16 | tui-text | 见 §7.17 |
| `test/tab-width.test.ts` | 4 | tui-text + 本单元（overlay 切片） | 见 §7.17 |
| `test/regression-slice-by-column-ansi-order.test.ts` | 2 | tui-text | §7.17 |
| `test/colors.test.ts` | 4 | tui-text | §7.17 |
| `test/fuzzy.test.ts` | 17 | tui-text | §7.17 |
| `test/latex.test.ts` | 20 | tui-latex | 不在本单元 |
| `test/autocomplete.test.ts` | 41 | tui-autocomplete | 不在本单元 |
| `test/autocomplete-skill-slash.test.ts` | 7 | tui-autocomplete | 不在本单元 |
| `test/editor.test.ts` | 192 | tui-components | 不在本单元 |
| `test/editor-history-keybindings.test.ts` | 1 | tui-components + 本单元 keybindings | |
| `test/input.test.ts` | 36 | tui-components | |
| `test/markdown.test.ts` | 92 | tui-components | |
| `test/select-list.test.ts` | 5 | tui-components | |
| `test/settings-list.test.ts` | 2 | tui-components | |
| `test/truncated-text.test.ts` | 9 | tui-components | |
| `test/word-navigation.test.ts` | 19 | tui-text | |

### 7.2 `test/tui-render.test.ts`（32 it，**全部为断言级描述**）

`describe("TUI render scheduling")`
1. `renders keyboard input without waiting for a throttled frame` — 输入后 16 ms 节流不应延迟；`sendInput` 后立即渲染。

`describe("TUI debug logging")`
2. `writes redraw logs to the provided directory` — 传 `logDirectory` + `KIANA_TUI_DEBUG_REDRAW=1`；断言 `kiana-tui-debug.log` 存在且含 `fullRender: first render`。

`describe("TUI bounded render output")`
3. `splits a large full render without changing its output` — >1 MiB 全量渲染被切成多次 `write`，但拼接后与不切分完全一致。
4. `splits large differential updates without a full redraw` — 差分更新同样按 `MAX_RENDER_WRITE_CHARS` 切分，且不触发 `fullRender`。

`describe("TUI crash dump without configured log directory")`（**无** `logDirectory`；源码断言写入的是 `os.tmpdir()` 而非 home 目录）
5. `writes the crash dump to the OS temp directory instead of a home-directory default` — 未传 `logDirectory` 时写到 `os.tmpdir()/kiana-tui-crash.log`；同时抛出含 `Rendered line ... exceeds terminal width` 的错误。

`describe("TUI Kitty image cleanup")`（均 `setCapabilities({images:"kitty",...})` + `setCellDimensions({10,10})`）
6. `clears reserved Kitty image rows before drawing appended image placements` — 追加图片前先清空预留行（`\x1b[<n>A` + `\x1b[2K` + `\x1b[<n>B`）。
7. `falls back to full redraw when Kitty image pre-clear would scroll` — 预留行导致需要滚动时改走全量重绘。
8. `reserves Kitty image rows before drawing during full redraw fallbacks` — 全量重绘路径同样预留行。
9. `does not use cursor-up placement for Kitty images taller than the viewport` — 图片高度 ≥ 视口时不用上移放置。
10. `deletes changed image ids before drawing moved placements` — 放置移动前先 `deleteKittyImage(id)`。
11. `redraws image lines when an earlier reserved image row changes` — 预留行变化触发该图片行重绘。
12. `deletes previously rendered image ids during full redraws` — 全量重绘时删除上一帧所有图片 id。

`describe("TUI resize handling")`
13. `triggers full re-render when terminal height changes`。
14. `skips full re-render on height changes in Termux`（`TERMUX_VERSION=1`）。
15. `triggers full re-render when terminal width changes`。

`describe("TUI content shrinkage")`
16. `clears empty rows when content shrinks significantly`。
17. `handles shrink to single line`。
18. `handles shrink to empty`。

`describe("TUI differential rendering")`
19. `tracks cursor correctly when content shrinks with unchanged remaining lines`。
20. `renders correctly when only a middle line changes (spinner case)`。
21. `resets styles after each rendered line`（每行末尾补 `\x1b[0m`）。
22. `renders correctly when first line changes but rest stays same`。
23. `renders correctly when last line changes but rest stays same`。
24. `renders correctly when multiple non-adjacent lines change`。
25. `handles transition from content to empty and back to content`。
26. `full re-renders when deleted lines move the viewport upward`。
27. `appends after a shrink without another full redraw once the viewport is reset`。
28. `clears stale content when maxLinesRendered was inflated by a transient component`。

`describe("TUI fake cursor")`
29. `draws the focused fake cursor in reverse video and omits it when the hardware cursor is shown`。
30. `keeps the fake cursor of an unfocused input when the hardware cursor is shown`。
31. `omits the focused fake cursor when truncation dropped its end marker`。
32. `keeps the cursor when an overlay ends directly left of it`（`showHardwareCursor` 两种取值都测）。

`test/tui-shrink.test.ts`（1 it）：
- `clears all rendered lines when content shrinks to zero`。

### 7.3 `test/tui-alt-screen.test.ts`（68 it）

分组与断言要点（**每个 it 都是 Rust 侧必须复刻的用例**）：

**视口与滚动（1-6）**
1. `renders a terminal-height viewport and preserves manual scroll position`
2. `shows a clickable jump-to-end indicator on the transcript's last row while scrolled up`
3. `keeps the jump-to-end indicator centered as the auto scrollbar hides and reappears`
4. `leaves the scrollbar visible and clickable when the jump-to-end indicator spans the transcript`
5. `never shows the jump-to-end indicator for a primary scroll view without follow-end`
6. `keeps an explicit dock fixed while the transcript scrolls`（`setLayoutRoot`）
7. `invalidates overlays with an explicit layout root`

**滚轮路由（8-14）**
8. `routes wheel input to the scroll view under the pointer`
9. `scrolls faster while Alt is held during wheel input`（×5）
10. `does not vertically redispatch misses through horizontal layout containers`
11. `uses button-motion tracking inside terminal multiplexers`（`TMUX`/`ZELLIJ`/`STY`/`TERM=screen*`）
12. `invokes the right-click paste handler only on Windows outside VS Code`
13. `reveals an auto scrollbar when the pointer enters its hidden track`
14. `jumps to a scrollbar track position and continues dragging from there`
15. `applies runtime wheel line count updates`（`setWheelScrollLines`）
16. `chains unused wheel delta to an outer scroll view`（`overscroll: "chain"`）

**键盘导航（17-22）**
17. `supports configurable keyboard viewport navigation with four rows of page overlap`
18. `scrolls the transcript by half a page with custom bindings`
19. `scrolls the transcript by one line with custom bindings`
20. `routes Home and End to the focused component and Ctrl+Home/End to the transcript`
21. `jumps between OSC 133 semantic prompt markers`

**搜索（22-31）**
22. `searches normalized rendered transcript text across rows`
23. `maps normalized ASCII and Unicode search matches back to rendered columns`
24. `reuses indexed transcript matches until the query or rendered lines change`
25. `renders transcript search with a muted placeholder and right-aligned controls`
26. `navigates transcript search with hoverable arrow buttons and toggles it with its shortcut`
27. `does not treat transcript box drawing as search navigation buttons`
28. `uses configured styles for current and non-current search matches`
29. `searches the transcript with Ctrl+Shift+F and restores editor focus on close`

**图片（30-38）**
30. `does not emit Kitty graphics commands or OSC 133 zones in iTerm2`
31. `clears stale iTerm2 image placements when they leave the viewport`
32. `crops a Kitty image whose first line is above the viewport`
33. `crops a Kitty image in a padded, colored box whose first line is above the viewport`
34. `crops a Kitty image whose placeholder rows contain text when its first line is above the viewport`
35. `redraws WezTerm Kitty images after writes to covered rows`
36. `reuses moved Kitty images without dropping HStack siblings`
37. `retains recently offscreen Kitty images for placement-only reuse`
38. `evicts the least recently visible Kitty image when the cache is full`（16 张）
39. `evicts offscreen Kitty images when decoded raster memory exceeds the cache quota`（64 MiB）

**超链接与选择（40-55）**
40. `opens an OSC 8 hyperlink with specific or generic release codes, but not on drag`
41. `selects visible text with the mouse and copies it with OSC 52 after a generic release`
42. `keeps the selection highlight after a fake cursor cell`
43. `uses an injected copySelection handler instead of OSC 52 and reports success`
44. `leaves selections visible without copying when copyOnSelect is disabled`
45. `copies an active selection programmatically`
46. `flashes an error when the injected copySelection handler fails`（`Copy failed`）
47. `flashes a specific error returned by the injected copySelection handler`
48. `does not append whitespace to double-click word highlighting`
49. `coalesces slash and hyphen separated segments for double-click word selection`
50. `highlights a complete whitespace segment during a word drag`
51. `selects whole words on double click, extends word drags, and selects lines on triple click`
52. `does not repaint idle or zero-width selections on focus loss`
53. `clears an active visible selection on focus loss and ignores orphan events`
54. `retains a completed visible selection across focus changes`
55. `stacks flash messages and collapses them as they expire`
56. `auto-scrolls and extends a drag selection held at the viewport edge`
57. `snaps mouse selection to CJK, emoji, and combining grapheme boundaries`

**鼠标分发（58-68）**
58. `ignores horizontal trackpad wheel events`
59. `dispatches clicks to nested mouse regions without breaking drag selection`
60. `scrolls the transcript on wheel input a focused overlay does not handle`
61. `focuses and captures drag gestures for mouse-aware components`
62. `reports consecutive click counts to component-owned controls`
63. `does not rerender for handled no-op pointer motion`
64. `lets mouse-aware components consume wheel events before viewport scrolling`
65. `restores keyboard state before leaving alt mode and prints the full document`
66. `gives viewport keys to a focused overlay`
67. `keeps viewport scrolling when an overlay is not focused`
68. `keeps viewport scrolling while transcript search is focused`

### 7.4 `test/layout.test.ts`（15 it）

`describe("viewport layout")`：
1. `allocates vertical grow space deterministically`
2. `does not render fixed-basis scroll content during stack measurement`
3. `paints only clipped rows from very large scroll content`
4. `shrinks entries to their minimum sizes`
5. `includes nested minimum sizes in intrinsic stack measurement`
6. `omits gaps around invisible entries`
7. `crops Kitty images at a scroll view's lower boundary`
8. `composes horizontal children at allocated widths`
9. `does not paint zero-width horizontal children`
10. `tracks follow-end state and returns unused scroll delta`
11. `renders a proportional glyph scrollbar with an expanded active thumb`
12. `preserves only the underlying background beneath overlay scrollbar glyphs`
13. `updates reserved scrollbar layout at runtime`
14. `measures nested scroll content from constrained child geometry`
15. `rebuilds geometry after content changes`

### 7.5 `test/overlay-non-capturing.test.ts`（44 it）

全部是焦点状态机用例（细节见 §2.9 的 `setFocusInternal` 状态机）。**必须逐条复刻**：
- `focus management`（30 个）：`nonCapturing` overlay 创建时不夺焦；`focus()` 转移；`unfocus()` 恢复；
  `setHidden(false)` 不自动聚焦；`hide()` 非焦点时不动焦点；`hide()` 焦点时正确恢复；
  子 overlay 清理后恢复；被移除的焦点子 overlay 不作为父级回退；微任务延迟创建子 overlay；
  不可见时跳过输入重定向；base focus 替换先收到 close 输入；`blocked` 替换可在恢复前内部移动焦点；
  `unfocus` 释放 blocked overlay；不可见 focused overlay 暂时回退但保留恢复资格；
  `preFocus` 为 null 的不可见 overlay 重新可见后恢复；循环 `preFocus` 祖先不死循环；
  `handleInput` 恢复焦点顺序最上层；`hideOverlay()` 不因非捕获顶层重排焦点；
  多个捕获/非捕获 overlay 的移除链；顶层捕获 overlay `unfocus()` 回退到 `preFocus`。
- `no-op guards`（4 个）：隐藏 overlay `focus()` 无效；`hide()` 后 `focus()` 无效；
  未获焦 `unfocus()` 无效；`preFocus` 为 null 的 `unfocus()` 清空焦点且不把输入送回 overlay。
- `focus cycle prevention`（4 个）：两个非捕获 overlay 间切换焦点后 `unfocus` 回到 editor；
  显式 unfocus 目标支持三个 overlay 与 editor 循环；显式 null 目标清空焦点；
  隐藏 focused overlay 回退到视觉最前的 overlay。
- `rendering order`（6 个）：`focus()` 提升视觉顺序；默认按创建顺序；低层 overlay `focus()` 后置顶；
  中层 `focus()` 置顶且保持其余相对顺序；隐藏再显示置顶；`unfocus()` 不改变视觉顺序。

### 7.6 Overlay 几何与合成（26 + 1 + 2 + 4 it）

`overlay-options.test.ts`：
- **宽度溢出保护**（6）：超宽行被截断；复杂 ANSI 不崩；叠在带样式 base 上；宽字符边界；终端边缘；base 含 OSC。
- **宽度百分比**（2）：`width: "50%"`；`minWidth` 覆盖小的百分比结果。
- **锚点**（3）：`top-left`、`bottom-right`、`top-center`。
- **margin**（3）：负数夹到 0；数字；对象四边。
- **offset**（1）：`offsetX`/`offsetY`。
- **百分比定位**（3）：`rowPercent`/`colPercent`；`row: "0%"` = 顶部；`row: "100%"` = 底部。
- **maxHeight**（2）：`maxHeight` 数字；`maxHeightPercent`。
- **绝对定位**（1）：`row`/`col` 覆盖 `anchor`。
- **叠加**（3）：多个 overlay 后者在上；不同位置互不干扰；按栈顺序隐藏。
- **hide 之后**（2）：`hideOverlay()` 后光标可见；handle `hide()` 后光标可见。

`overlay-short-content.test.ts`（1）：内容短于终端高度时仍渲染。

`tui-overlay-style-leak.test.ts`（2）：
1. `should not leak styles when a trailing reset sits beyond the last visible column (no overlay)`
2. `should not leak styles when overlay slicing drops trailing SGR resets`

`regression-overlay-cjk-boundary.test.ts`（4）：
1. `excludes a wide grapheme from before when overlay starts inside it`
2. `keeps ASCII before-segment behavior at the same boundary`
3. `composites an overlay at the requested column when it starts inside a wide grapheme`
4. `composites an overlay when it starts at a wide grapheme boundary`

### 7.7 `program-status.test.ts`（5）与 `wheel-scroll.test.ts`（6）

`formatProgramStatus`：
1. `encodes state, app, kind, and a base64 message`
2. `omits kind outside blocked, invalid app names, and empty messages`
3. `replaces control characters, which make terminals discard the report`
4. `cuts long messages at a UTF-8 boundary within the spec limits`
`isProgramStatusReply`：
5. `accepts the query echo with either terminator and future pairs`

`WheelScrollAccelerator`：
1. `uses fixed line counts regardless of timing`
2. `keeps one line per event in auto mode when the terminal already accelerates`
3. `scales auto mode with wheel velocity`
4. `does not accelerate bursts of events for a single notch`
5. `resets acceleration on direction changes and pauses`
6. `carries fractional lines between events`

### 7.8 `keys.test.ts`（61）与 `keybindings.test.ts`（7）

`keys.test.ts` 分组（逐条名称见 §2.5 的对应实现）：
- `matchesKey` / `Kitty protocol with alternate keys (non-Latin layouts)`（16 个）：Cyrillic 基布局键、
  `Ctrl+Shift+p`、无基布局时直接码点、super 组合、数字 CSI-u、键盘区功能键归一化、
  移位符号按产出字符匹配、`shifted key`/`event type`/完整格式、拉丁字母优先码点、符号优先码点、
  错误键不匹配、错误修饰不匹配。
- `modifyOtherKeys matching`（13 个）：xterm 的 `Ctrl+c/d/z`、Enter/Tab/Backspace/Escape/Space 变体、
  符号与数字组合、移位大写字母、kitty 未激活时的 `Ctrl+Alt+letter`。
- `Legacy key matching`（18 个）：legacy `Ctrl+c/d`、escape、`\n` 作 enter、kitty 激活时 `\n` 为 shift+enter、
  `ctrl+space`、`Ctrl+symbol`、`Ctrl+Alt+symbol`、原始 `0x08` 的三态（非 WT / 本地 WT / WT over SSH）、
  kitty 未激活时的 alt 前缀序列、方向键、SS3 方向键与 home/end、xterm Ctrl 修饰视口导航、
  legacy 功能键与 clear、alt+方向键、rxvt 修饰序列。
- `decodeKittyPrintable`（1）：键盘区功能键 → 可打印字符。
- `decodePrintableKey`（1）：可打印 xterm modifyOtherKeys 序列。
- `parseKey`（12）：Kitty 备用键（6）与 legacy 解析（6）。

合计校验：47 + 1 + 1 + 12 = 61（与 `keys.test.ts` 的 `describe` 嵌套结构一致；
`matchesKey` 的 47 = 顶层 16（`Kitty protocol with alternate keys (non-Latin layouts)`）+
`modifyOtherKeys matching` 13 + `Legacy key matching` 18）。

`keybindings.test.ts`（7）：
1. `binds Ctrl+J as a default newline alias`
2. `binds modified and unmodified editor viewport navigation`
3. `leaves dedicated prompt history navigation unbound by default`
4. `binds unmodified terminal viewport shortcuts to alternate-screen navigation`
5. `does not evict selector confirm when input submit is rebound`
6. `does not evict cursor bindings when another action reuses the same key`
7. `still reports direct user binding conflicts without evicting defaults`

### 7.9 `stdin-buffer.test.ts`（66 it）

分组：`Regular Characters`（3）、`Complete Escape Sequences`（5）、`Partial Escape Sequences`（8）、
`Terminal Reply Strings`（7，含参数化用例）、`Mixed Content`（4）、`Kitty Keyboard Protocol`（15）、
`Mouse Events`（7）、`Edge Cases`（6）、`Flush`（3）、`Clear`（1）、`Bracketed Paste`（5）、`Destroy`（2）。
关键回归（必须复刻）：
- `does not apply the sequence timeout to a lone ESC`
- `keeps fragmented mouse sequences buffered across delayed chunks by default`
- `keeps delayed ${JSON.stringify(reply)} intact at every split after its reply header`（对所有已知回复头做逐位置切分）
- `recovers from an unterminated reply when a new escape sequence arrives`
- `discards a canceled reply without swallowing subsequent input`（CAN/SUB）
- `should split ESC+ESC+CSI into standalone ESC and the CSI sequence (WezTerm Escape key regression)`
- `should drop raw duplicate character after matching Kitty printable sequence`（含跨 chunk）
- `flushes a lone escape promptly with the longer default sequence timeout`

### 7.10 `terminal.test.ts`（27 it）

- `resolveEscapeTimeoutMs`（4）：`KIANA_TUI_ESC_TIMEOUT` 生效 / 非法值忽略 / SSH 默认 100 ms / 默认 10 ms。
- `normalizeNativeShiftEnterInput`（4）：Shift 按下且启用 → `\x1b[13;2u`；禁用不改；Shift 未按下不改；非 Return 不改。
- `normalizeAppleTerminalInput`（4）：同上但仅 Apple Terminal。
- `ProcessTerminal Kitty keyboard protocol negotiation`（7）：
  - `queries Kitty mode before enabling modifyOtherKeys fallback`
  - `activates Kitty mode for non-zero negotiated flags`
  - `falls back to modifyOtherKeys for zero Kitty flags`
  - `falls back to modifyOtherKeys for device attributes without Kitty flags`
  - `forwards device attributes replies that answer other queries`
  - `forwards normal input while waiting for Kitty response`
  - `tracks split Kitty confirmation`
- `program status (OSC 7501)`（6）：
  - `reports the latest status once the terminal answers the query before DA`
  - `reports nothing when DA arrives first and swallows late replies`
  - `skips the query when KIANA_PROGRAM_STATUS is set`
  - `does not let a DA reply from before a restart end the new query`
  - `clears the status on stop and reports it again after restart`
  - `replays buffered CSI-prefix input when it is not a Kitty response`
- `ProcessTerminal progress`（1）：写出合法的 OSC 9;4 清除序列。
- `ProcessTerminal dimensions`（1）：`COLUMNS`/`LINES` 优先于默认 80×24。

### 7.11 `terminal-colors.test.ts`（6 it）

- `parseTerminalColorSchemeReport` — `parses color scheme reports`
- `parseOscColorResponse` — `parses OSC 10, 11, and 4 replies`
- `TUI.queryTerminalColors`（4）：
  - `queries all colors in one write and consumes the replies`（单次 write）
  - `consumes delayed Ghostty color fragments without typing their tail or opening the editor`
  - `resolves on DA1 with the replies that arrived, in query order`
  - `reports late replies after a timeout and consumes them until DA1`

### 7.12 `terminal-image.test.ts`（80）与 `bug-regression-isimageline-startswith-bug.test.ts`（11）

分组（逐条名称见 §2.15）：
- `isImageLine`（22 = 5+3+4+7+3）：iTerm2/Kitty 任意位置、超长行（304k+）、ANSI 前后缀、负例（纯文本、仅 ANSI、
  光标移动码、部分序列、空行、仅换行）、混合内容、文件路径误报。
  （子组：`iTerm2 image protocol` 5、`Kitty image protocol` 3、`Bug regression tests` 4、
  `Negative cases - lines without images` 7、`Mixed content scenarios` 3。）
- `detectCapabilities`（27）：未知终端默认、环境覆盖、`auto` 保留自动探测、程序化覆盖的应用与清除、
  覆盖时跳过 tmux 探针、tmux 转发/不转发、`TERM=tmux*`、`TERM=screen*` 强制关闭、
  Herdr/Rex/Ghostty/cmux（`CMUX_WORKSPACE_ID` 存在**不得**影响 Ghostty 的 kitty 图片与超链接判定，
  测试名 `does not disable Ghostty images solely because cmux is present`）/Kitty/WezTerm/Warp（三种变量、tmux 内禁图片）/iTerm2/VSCode/
  Zed+Alacritty/Windows Terminal（多路复用器外）/JetBrains/tmux 不继承 WT truecolor/
  tmux 内信任显式 truecolor/`-direct` TERM。
- iTerm2 编码（1）：`includes the decoded payload size in OSC 1337 metadata`。
- Kitty 光标移动与裁剪（11）：`moveCursor` 两种取值、显式 placement 行、删除命令抑制回复、
  `renderImage` 默认、显式 placement 裁剪、只放置不上传、`maxHeightCells` 缩宽、
  Image 组件方形像素盒、首行放置 + 空填充行、回退行截断。
- `image cell sizing`（7 = Kitty 5 + iTerm2 2）：Kitty 细图至少 1 行、宽变化下 placement/预留行/裁剪元数据一致、
  向上取整减少失真、高度受限列数、按比例选宽且至少 1 列；iTerm2 向上取整宽度、
  向上取整预留行与光标偏移。
- `imageFallback`（4）：`~` 缩写绝对路径、OSC 8 `file://` 链接、裸文件名不改不链接、无文件名时省略。
- `Image transcoding`（4）：PNG 尺寸、无转码器时文本回退、每个图像只转一次、PNG 与 iTerm2 不转。
- `hyperlink`（4）：OSC 8 包裹、保留内部 ANSI、空文本、`file://` URI。

`bug-regression-isimageline-startswith-bug.test.ts`（11），四组（**逐字名称**）：
- `describe("Bug regression: isImageLine() crash with image escape sequences")`
  - `Bug scenario: Terminal without image support`（4）：
    `old implementation would return false, causing crash`、`new implementation returns true correctly`、
    `new implementation detects Kitty sequences in any position`、`new implementation detects iTerm2 sequences in any position`
  - `Integration: Tool execution scenario`（3）：`detects image sequences in read tool output`、
    `detects Kitty sequences from Image component`、`handles ANSI codes before image sequences`
  - `Crash scenario simulation`（2）：`does NOT crash on very long lines with image sequences`、
    `handles lines exactly matching crash log dimensions`
  - `Negative cases: Don...`（2）：`does not detect images in regular long text`、
    `does not detect images in lines with file paths`
核心是"任意位置探测 + 长行不崩 + 不误报"。

### 7.13 Native（10 + 2 + 4 it）

`native-clipboard-linux.test.ts`（10 个 `it`，其中 3 个是参数化模板）：
1. `validates X11 incremental property metadata before accumulating bytes`
2. `bounds private clipboard work and preserves cleanup: ${mode}`
3. `keeps JS responsive during stalled X11 connection setup: ${method}`
4. `decodes X11 STRING as Latin-1`
5. `reads UTF8_STRING when X11 TARGETS is refused`
6. `returns null for empty X11 clipboards and text-only selections`
7. `preserves X11 Unicode and incremental text/image transfers`
8. `reports X11 transfer ${mode} as an exception, not an unavailable display`
9. `frees partially received X11 ${content} after invalid metadata`
10. `times out stalled X11 ${content} after a partial transfer without blocking JS`

`native-module-path.test.ts`（2）：
1. `resolves native helpers from the installed TUI package when the module is bundled elsewhere`
2. `keeps standalone binary fallbacks when the TUI package is unavailable`

`native-platform.test.ts`（4 个 `test(`）：
1. `Linux ARM64 prebuild supports 64 KB system pages`（无 skip；校验 ELF LOAD 段 64 KB 对齐）
2. `writes Windows clipboard text through the native helper without command fallbacks`
   （skip：非 win32 或 `KIANA_TEST_NATIVE_CLIPBOARD !== "1"`；写读三组文本并断言 `getImage()` 为 null；
   **会覆盖系统剪贴板**）
3. `uses the native platform helper directly as the clipboard API`
   （skip：非 darwin/win32 或 arch 不在 arm64/x64；断言 `getNativeClipboard() === getNativePlatformHelper()`）
4. `Linux loads X11 lazily and rechecks DISPLAY`
   （skip：arch 不在 arm64/x64；mock `require.cache` + 改写 `process.platform`，验证惰性加载、
   `DISPLAY` 删除后归 undefined、改值后复用同一实例）

### 7.14 `mouse-components.test.ts`（9 it）

1. `positions a single-line input cursor on press`
2. `selects and activates list rows`
3. `activates settings rows`
4. `ignores hover and clicks visible select-list row ${row} after scrolling`（参数化）
5. `ignores hover and clicks visible settings row ${row} after scrolling`（参数化）
6. `keeps a delegating overlay focused when its nested input is clicked`
7. `keeps a settings list focused when a click in its submenu closes the submenu`
8. `positions and focuses the multiline editor through alternate-screen dispatch`
9. `selects and copies editor text on drag instead of moving the cursor`

（本文件跨本单元与 tui-components；本单元负责鼠标坐标转换与 `handleMouse` 分发。）

### 7.15 `exit-restore.test.ts`（2 it）

1. `restores terminal modes when the process exits without stop()`：
   输出中各出现**恰好一次**：`\x1b[?1006l`、`\x1b[?1004l`、`\x1b[?1003l`、`\x1b[?1000l`、`\x1b[<u`、`\x1b[?2004l`；
   且 `\x1b[<u` 在 `\x1b[?1049l` **之前**；输出以 `\x1b[?1049l\x1b[?25h` **结尾**。
2. `does not restore twice after a normal stop()`：断言集合与上面**不同**，为 `\x1b[?1006l`、`\x1b[?1004l`、`\x1b[<u`、`\x1b[?2004l`、`\x1b[?1049l` 各恰好一次
   （不含 `?1003l`/`?1000l`，但**包含** `\x1b[?1049l`）。
   两个用例都通过 `spawnSync(process.execPath, [fixture, mode])` 运行 `test/fixtures/exit-without-stop.ts`，
   env 中 `TMUX`/`STY`/`ZELLIJ` 置 undefined、`TERM="xterm-256color"`，`timeout: 10_000`，
   并先断言 `status === 0`。

### 7.16 `regression-sigwinch-kill-eacces.test.ts`（3）与 `tui-cell-size-input.test.ts`（2）

1. `does not throw when kill(2) returns EACCES for self-signal`
2. `does not call kill on win32`
3. `preserves other error codes`
4. `forwards bare escape even when a cell size query was sent at startup`
5. `consumes cell size responses and still forwards later user input`

### 7.17 相邻单元测试（Rust 侧联合验收）

`visible-width.test.ts`（4）：样式 ASCII、制表符 = 3 列、非 ASCII、未终止转义序列按 0 宽。
`wrap-ansi.test.ts`（19）：下划线样式、背景色跨行保留、LF/CRLF/CR、CJK 断行、OSC 133 零宽、ST 终止、
孤立区域指示符宽 2、尾随空白截断、OSC 8 续行重开与行尾关闭、BEL 终止符保留。
`truncate-to-width.test.ts`（16）：超大 Unicode、ANSI 保留与省略号前后 reset、BEL OSC 8 关闭标签、
畸形转义前缀不 hang、宽省略号、已适配时不改、补齐宽度、无省略号时补 reset、连续前缀不跳宽字素。
`tab-width.test.ts`（4）：slice 与 visibleWidth 一致、overlay 段宽一致、控制序列内制表符字节不变、
含制表符 overlay 保持单物理行。
`regression-regional-indicator-width.test.ts`（5）：部分国旗簇宽 2、流式中间态稳定。
`regression-slice-by-column-ansi-order.test.ts`（2）：切片起点保留 reset、高亮 token 后不漏色。
`colors.test.ts`（4）、`fuzzy.test.ts`（17）属于 tui-text，本单元仅经 `index.ts` 再导出。

### 7.18 Rust 测试改写建议

- 名称保留原英文（便于对照），放在 `#[cfg(test)] mod tests` 或 `tests/` 集成测试中；
- 需要 xterm 行为的用例（`tui-render`、`tui-alt-screen`、`overlay-*`、`mouse-components`）依赖
  `TestTerminal`（§7.0）——建议先写这个基础设施，再逐条移植；
- 涉及 `process.env` 的用例用 `serial_test::serial` + `temp_env`；
- 涉及子进程退出恢复的用例，编译一个 `examples/exit_without_stop.rs` 并用 `Command` 运行；
- 涉及 native 的用例（X11/剪贴板）在没有 `DISPLAY` 时应自动 skip（与 TS 一致）。

---

## 8. 建议的 Rust 模块布局与最难复刻之处

### 8.1 crate 与模块

单一 `kiana-tui` crate（`kiana_tui`），或 workspace 内 `kiana-tui` + `kiana-tui-native`：

```
kiana-tui/
  Cargo.toml
  src/
    lib.rs                  // 与 index.ts 的 176 个符号一一对应的 pub use
    tui.rs                  // Component, Focusable, Container, TuiBase, TUI, ViewportTUI,
                            // CURSOR_MARKER/FAKE_CURSOR_*, composite_tui_line, overlay 类型
    main_screen.rs          // TuiMainScreen, BoundedTerminalWriter, 差分渲染, kitty 预留行
    alt_screen.rs           // TuiAltScreen, 选区/搜索/滚动条/Kitty LRU/文档回放
    layout.rs               // LayoutRect/Box/Frame, layout_component, render_layout_frame,
                            // get_scrollbar_geometry, paint_scrollbar, get_layout_boxes_at
    layout_node.rs          // LayoutNode trait/enum, ScrollLayoutNode, StackLayoutNode, LayoutViewport
    terminal.rs             // Terminal trait, ProcessTerminal, resolve_escape_timeout_ms,
                            // refresh_terminal_dimensions, normalize_*_input
    stdin.rs                // StdinBuffer, is_complete_sequence, extract_complete_sequences
    keys.rs                 // KeyId, Key, matches_key, parse_key, Kitty/modifyOtherKeys 解析
    keybindings.rs          // KeybindingsManager, TUI_KEYBINDINGS, 全局单例
    program_status.rs       // ProgramStatus, format_program_status, is_program_status_reply
    wheel.rs                // WheelScrollAccelerator, WheelScrollLines
    colors.rs               // RgbColor, TerminalColors, TerminalColorScheme, parse_osc_color_response
    image.rs                // TerminalCapabilities, detect_capabilities, encode_kitty/iterm2,
                            // crop_kitty_image_line, calculate_image_rows, image_fallback, hyperlink
    search.rs               // AltScreenSearchIndex, AltScreenSearchComponent, find_*_matches
    flash.rs                // AltScreenFlashContainer
    components/
      mod.rs
      scroll_view.rs        // ScrollView（与 tui-components 共用实现）
      stack.rs              // Stack/VStack/HStack/allocate_stack_sizes（与 tui-components 共用）
    native/
      mod.rs                // NativeClipboard trait + get_native_clipboard()
      linux_x11.rs          // XCB 实现（可选，feature = "x11"）
      win32.rs              // feature = "win32"
      darwin.rs             // feature = "darwin"
```

**依赖建议**（全部可选 feature）：
```toml
[dependencies]
unicode-width = "0.2"
unicode-segmentation = "1"        # 或 icu_segmenter = "1"
regex = "1"
base64 = "0.22"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
indexmap = "2"
log = "0.4"
[target.'cfg(target_os = "linux")'.dependencies]
x11rb = { version = "0.13", optional = true }
[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", optional = true }
[target.'cfg(target_os = "macos")'.dependencies]
objc2-app-kit = { version = "0.2", optional = true }
```

### 8.2 关键类型/特征设计

```rust
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, _data: &str) {}
    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> { None }
    fn wants_key_release(&self) -> bool { false }
    fn invalidate(&mut self) {}
}

pub trait Terminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>);
    fn stop(&mut self);
    fn restore_on_exit(&mut self, before: &str, after: &str);
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) -> impl Future<Output = ()>;
    fn write(&mut self, data: &str);
    fn columns(&self) -> usize;
    fn rows(&self) -> usize;
    fn kitty_protocol_active(&self) -> bool;
    fn move_by(&mut self, lines: isize);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn clear_line(&mut self);
    fn clear_from_cursor(&mut self);
    fn clear_screen(&mut self);
    fn set_title(&mut self, title: &str);
    fn set_progress(&mut self, active: bool);
    fn set_program_status(&mut self, status: &ProgramStatus);
}

pub trait Tui: Component {
    fn mode(&self) -> TuiMode;
    fn children(&mut self) -> &mut Vec<Box<dyn Component>>;
    fn add_child(&mut self, child: Box<dyn Component>);
    fn remove_child(&mut self, child: &dyn Component);
    fn clear_children(&mut self);
    fn set_focus(&mut self, component: Option<Box<dyn Component>>);
    fn show_overlay(&mut self, component: Box<dyn Component>, options: OverlayOptions) -> OverlayHandle;
    fn hide_overlay(&mut self);
    fn has_overlay(&self) -> bool;
    fn start(&mut self);
    fn stop(&mut self, options: TuiStopOptions);
    fn render_now(&mut self, force: bool);
    fn request_render(&mut self, force: bool);
    fn add_input_listener(&mut self, listener: Box<dyn FnMut(&str) -> Option<TuiInputListenerResult>>) -> InputListenerId;
    fn query_terminal_colors(&mut self, timeout_ms: u64) -> impl Future<Output = TerminalColors>;
    // ...
}
```

设计要点：
1. **单线程所有权**：`TuiBase` 持有 `Vec<Box<dyn Component>>`、`Vec<OverlayEntry>`。
   焦点用 `Option<usize>`（overlay 索引用 `Option<OverlayId>`）而非 `Rc`，避免循环引用。
   `Component` 不需要 `Send`（全部在 UI 线程）。
2. **全局单例**用 `thread_local!` + `RefCell`（`keybindings`、`capabilities`、`cell_dimensions`、
   `kitty_metadata`、`image_id_counter`）——与 JS 模块级变量语义一致。若需要跨线程，改 `OnceLock<Mutex<...>>`。
3. **overlay 的"栈"与"视觉顺序"是两个正交概念**（**更正**：TS 并**不**在 `focus()` 时移动数组元素）。
   - `overlayStack: Vec<OverlayEntry>` 只按 `showOverlay()` 的**创建顺序**追加；`hideOverlay()` 弹出**末尾**一项
     （若末尾不可见/非捕获也一样弹出，`hideOverlay` 不检查可见性），handle `hide()` 则按 `indexOf` 删除任意位置。
   - 每项有 `focusOrder: number`（构造时 `++focusOrderCounter`，`focus()`/`setHidden(false)` 时刷新）。
   - **渲染顺序**：`visibleEntries.sort((a,b) => a.focusOrder - b.focusOrder)`（升序，后者在上）。
   - **"最上层可见 overlay"**：遍历所有项，跳过 `nonCapturing` 与不可见者，取 `focusOrder` **最大**者
     （不是数组末尾）。
   Rust 侧必须保留 `focusOrder` 单调计数器（`u64`），用 `Vec` 保存插入顺序 + 排序取渲染顺序，**不要**在
   focus 时重排 `Vec`。
4. **焦点恢复状态机**用 enum：
   ```rust
   enum OverlayFocusRestore {
       Inactive,
       Eligible { overlay: OverlayId },
       Blocked { overlay: OverlayId, blocked_by: FocusTarget, resume: RestoreResume },
   }
   enum RestoreResume { RestoreOverlay, Target(FocusTarget) }
   ```
   与 §2.9 的 `setFocusInternal` 逐分支对应。
5. **定时器抽象**：`trait Timer { fn set(&mut self, ms: u64, f: Box<dyn FnOnce()>); fn cancel(&mut self); }`，
   生产用 tokio/自建堆，测试用可手动触发的 fake timer（必须能测 16 ms 节流与 1000 ms 闪烁）。
6. **输出写入抽象**：`BoundedTerminalWriter` 单独成结构，接受 `&mut dyn Terminal` 与
   `max_chars: usize`，按 UTF-16 code unit 计数且不拆代理对。
7. **能力探测**：`detect_capabilities(env: &Env, platform: Platform, tmux_probe: impl Fn() -> bool)`，
   把 `process.env` / `process.platform` 显式参数化，便于测试。

### 8.3 最难复刻的部分（按难度降序）

1. **`Intl.Segmenter` 字素/词分段**。`utils.ts` 的 `visibleWidth`、`sliceByColumn`、
   `getGraphemeCellRange`，以及 alt-screen 选区的字素吸附（`snaps mouse selection to CJK, emoji,
   and combining grapheme boundaries`）都依赖它。`unicode-segmentation` 与 ICU 在 Indic conjunct、
   emoji ZWJ 序列、区域指示符、`\r\n`、Prepend 类字符上可能不同。必须用测试校准，
   必要时用 `icu_segmenter`（ICU4X）。
2. **东亚宽度表**。`get-east-asian-width@1.6.0` 的具体表版本未知；`unicode-width` 的表可能不同。
   受影响：`visibleWidth` 的默认宽度、Ambiguous 处理（该包默认把 Ambiguous 当 1）、
   `regression-regional-indicator-width`（区域指示符恒 2）。
   建议：把该 npm 包的 `eastAsianWidth` 数据导出为静态表嵌入 Rust（一次性生成，运行时不依赖 Node）。
3. **差分流式渲染的逐字节等价**。`doRender` 的输出顺序（同步输出开关、光标移动、逐行清写、
   Kitty 预留行前后移动、`\x1b[?2026h/l` 位置）必须与 TS 完全一致，否则 xterm 视口比较会失败。
   涉及 30+ 个分支与 8 种"强制全量重绘"条件（§2.10 的分支表）。
4. **备用屏渲染管线**。`doRender` 的 12 步顺序（布局 → 合成滚动到底提示 → 合成 overlay →
   截断到高度 → 应用选区 → 合成闪烁 → 提取光标 → 行 reset + 截宽 → 全量/差分判定 →
   WezTerm 三趟图片顺序 → 写入 → 记录状态）。任何一步顺序变化都会改变字节流。
5. **Kitty 图片裁剪与预留行**。`cropKittyImageLine`、`getKittyImageReservedRows`、
   `MAX_CACHED_OFFSCREEN_KITTY_*` 的 LRU、`transmissionGeneration` 判定、
   "图片行是否需要重绘"的 `imageAnchorsNeedRedraw`。需要 16 张/32 MiB/64 MiB 三级配额同时生效。
6. **OSC/CSI 解析的边界情况**。`extractCompleteSequences` 对 CAN/SUB、`\x1b\x1b`、`ESC [`/`ESC ]`/
   `ESC O`/`ESC P`/`ESC _` 的处理，7 种"终端回复头"的缓冲语义，Kitty 可打印字符去重
   （`pendingKittyPrintableCodepoint`）。66 个 stdin-buffer 用例是验收基准。
7. **键位匹配的完整矩阵**。`matchesKey` 的 3 阶段（Kitty → modifyOtherKeys → legacy）+ 31 个
   `SYMBOL_KEYS` + 20 个 `SHIFTED_SYMBOL_KEYS` + 功能键等价表 + rxvt/Ctrl 修饰变体。
   61 个 keys 用例。
8. **N-API 原生剪贴板的超时语义**。Linux：私有 pthread + 3 s 整体 deadline + 2000 ms 单操作超时 +
   忙时直接返回不可用；X11 `INCR` 增量读取与 50 MiB 上限；返回三态（`undefined`/`null`/reject）。
   Rust 侧用 `x11rb` 时需自己实现 `wait_for_event` 超时与 INCR 状态机。
9. **焦点恢复状态机的 44 个用例**（`overlay-non-capturing.test.ts` 内部子组为 30+4+4+6）。`setFocusInternal` 的 `eligible`/`blocked`/`restore-overlay`/
   `focus-target` 四态 + `preFocus` 环检测 + 不可见 overlay 的暂时回退。
10. **崩溃点与日志的副作用顺序**。超宽行的 `stop()` → 写崩溃日志 → 抛错；`restoreOnExit` 的同步写；
    `KIANA_TUI_DEBUG` 快照内容格式。任何顺序变化都会改变测试观察到的文件内容。

### 8.4 移植顺序建议

1. `keys.rs` + `keybindings.rs` + `stdin.rs`（纯函数，测试最多，先建立信心）
2. `program_status.rs` + `wheel.rs` + `colors.rs`（小文件，快速清零）
3. `terminal.rs`（`ProcessTerminal` 与协商状态机）+ `layout_node.rs`
4. `layout.rs`（布局器 + 滚动条）—— 依赖 `ScrollView`/`Stack` 的接口，但可先用 mock
5. `tui.rs`（`TuiBase`、overlay、焦点状态机、输入分发）
6. `main_screen.rs`（差分渲染 + 有界写 + Kitty 预留行）
7. `image.rs`（能力探测、编码、裁剪）
8. `search.rs` + `flash.rs`
9. `alt_screen.rs`（最后，聚合全部前置能力）
10. `native/`（可选 feature；无 `DISPLAY` 时自动降级）

---

## Audit additions

> 本节由对抗式完整性审计补充（逐文件重读 `packages/tui` 源码 + 44 个测试文件 + native 源码后新增）。
> 上文各节中**已知错误**已就地修正；下面按主题列出**补充事实**与**容易漏掉的契约**。
> 编号 A1… 便于交叉引用。

### A1 单元文件清单的补充（§0.1 的遗漏）

`packages/tui` 目录下还有以下文件**未列入 §0.1**，但它们影响本单元的构建与验收：

| 文件 | 作用 | 对本单元的影响 |
|---|---|---|
| `packages/tui/tsconfig.build.json` | `extends ../../tsconfig.base.json`，`outDir: ./dist`、`rootDir: ./src`、`include: ["src/**/*"]` | 目标 `ES2024` / `module: Node16` / `strict` / `erasableSyntaxOnly` / `verbatimModuleSyntax`；Rust 侧无对应，但 `dist/` 的目录结构决定了 `main`/`types` 路径与 `native/` 相对解析（`getNativeModuleCandidates` 依赖 `import.meta.url` 的相对深度） |
| `packages/tui/CHANGELOG.md`（1307 行） | 版本语义的唯一权威来源 | 本规格钉死的 1.4.0 行为对应 CHANGELOG 的 `[1.4.0]` 段；`[Unreleased]` 里已有两条**会改变本单元行为**的条目（见 A2） |
| `packages/tui/test/` 下的 7 个非 `.test.ts` 文件 | 手工示例/基准，不参与 `npm test` | `chat-simple.ts`（README 推荐的手工 demo）、`image-test.ts`、`key-tester.ts`、`render-churn-bench.ts`、`alt-screen-large-transcript-bench.ts`、`viewport-overwrite-repro.ts`、`test-themes.ts`。**`test-themes.ts` 被 README Quick Start 直接 import**（`import { defaultEditorTheme as editorTheme } from './test/test-themes.ts'`），因此它是公共文档的一部分；该文件用 `chalk`（devDependency 6.0.0）`new Chalk({ level: 3 })` 定义三个主题常量：`defaultSelectListTheme`、`defaultMarkdownTheme`、`defaultEditorTheme`（`{ borderColor, selectList }`）。Rust 侧复刻 README 示例时需提供等价主题常量，且颜色需按 `level: 3`（truecolor）展开 |

打包面（`package.json` 的 `files` 白名单）已在 §6.1 补齐；要点是 **`.node` 预编译产物必须随包分发**，
`native/*/src/**`、`native/*/build.*`、三个平台 README 都在白名单内，而 `test/` **不在**。

### A2 CHANGELOG 中与本单元直接相关的条目（版本语义）

- `1.4.0`：新增 `TERM_PROGRAM=rex` 检测（→ 分支 4，`images: "kitty"`, `trueColor: true`）；
  退出时不调用 `stop()` 的进程现在通过 `exit` 处理器同步恢复终端模式（→ §2.9 的 `exitRestoreHandler`）。
- `1.2.0`（**破坏性**）：`Editor.render()` / `Input.render()` 改为 APC 伪光标标记而不是直接写 `\x1b[7m`；
  新增 `renderFakeCursor()`；`showHardwareCursor` 只显示硬件光标。这三条共同决定 §2.9.5/§2.9.6 的
  `FOCUSED_FAKE_CURSOR` 解包语义——**若 Rust 侧按 1.2.0 之前的语义实现，`tui-render` 的 fake cursor 组会全灭**。
- `1.1.0`（**破坏性**）：`Terminal` 必须提供 `setProgramStatus(status)`；`KIANA_PROGRAM_STATUS=1|0` 覆盖探测；
  同版本新增 `TuiAltScreen.resetTextSelection()`。
- `1.0.3`：`Home`/`End` 只用于编辑器行首/行尾；全屏记录的 top/bottom 移到 `Ctrl+Home`/`Ctrl+End`
  （对应 §2.6.2 的 `tui.altScreen.top`/`bottom` 默认键）。
- `[Unreleased]`（**尚未发布但已在源码中**）：
  1. 双击选词的连接符集合修正为 iTerm2 默认的 `~ . \ +`（#10980）。这条就是
     `TERMINAL_WORD_SELECTION_JOINERS = new Set(["/", "-", "+", "\\", "~", "_", "."])` 的来历，
     源码注释还给出了 iTerm2 的永久链接（`iTermPreferences.m#L743`）。
     **注意集合里同时含 `/`、`-`、`_`、`.`，不等于"只有 `~ . \ +`"**——以源码字面量为准。
  2. `fuzzyMatch()` 排序修正（非本单元，属 tui-text）。

### A3 源码注释里携带的、规格未收录的契约

1. **`tui-alt-screen.ts:83-85`**（`TERMINAL_WORD_SELECTION_JOINERS` 上方）：
   ```
   Regular mode delegates double-click selection to the terminal emulator. Fullscreen owns mouse selection,
   so it uses iTerm2's default "characters considered part of a word" set:
   https://github.com/gnachman/iTerm2/blob/7f4ff739721f95d0b93003d8701c4523a7edf261/sources/Settings/iTermPreferences.m#L743
   ```
   即：**主屏模式（`TuiMainScreen`）不做双击选词**——选词是终端模拟器自己的事；只有 `TuiAltScreen` 实现。
   Rust 侧若把选词逻辑塞进 `TuiBase` 会与主屏行为冲突。
2. **`keys.ts:540-546`**（`isKeyRelease` 上方）：括号粘贴的内容即使含 `:3F` 之类的子串也**不得**判为释放，
   注释举的例子是蓝牙 MAC 地址 `90:62:3F:A5`；`terminal.ts` 会在把粘贴内容交给 TUI 前重新包上
   `\x1b[200~…\x1b[201~`，所以粘贴数据**一定**含 `\x1b[200~`。这是该函数的特判依据。
3. **`keys.ts:931-933`**（legacy `enter` 分支）：`\x1b\r` 是 Kitty 的
   `map shift+enter send_text all \e\r` 映射，`\n` 是 Ghostty 的 `keybind = shift+enter=text:\n`
   映射；因此 kitty 激活时两者都解释为 `shift+enter`。
4. **`terminal-image.ts:49-53`**（`probeTmuxHyperlinks` 上方）：tmux 只有在
   `client_termfeatures` 列出 `hyperlinks` 时才转发 OSC 8，否则**剥离**；任何异常回退 `false`。
5. **`terminal-image.ts`** `detectCapabilitiesFromEnvironment` 的三段注释解释了**顺序不可变**的原因：
   - tmux 分支注释："Image protocols are unreliable under tmux, so leave `images: null`"；
   - Herdr 分支注释："Check it before outer-terminal detection because variables like `KITTY_WINDOW_ID`
     can leak into its panes"——这就是 Herdr 必须排在 Kitty 分支**之前**的原因；
   - 兜底分支注释："OSC 8 is rendered invisibly as 'just text' on terminals that swallow it, which means
     the URL disappears from the rendered output. Default to the legacy `text (url)` behavior"。
   两个顺序敏感用例都**显式注入**了泄漏变量，所以重排分支会被抓住：
   `enables hyperlinks and Kitty images for Herdr` 用
   `withEnv({ TERM_PROGRAM: "herdr", TERM: "xterm-256color", COLORTERM: "truecolor", KITTY_WINDOW_ID: "1" })`，
   `enables hyperlinks without images for Rex` 用 `withEnv({ TERM_PROGRAM: "rex", TERM: "xterm-256color", KITTY_WINDOW_ID: "1" })`
   ——即"即使 `KITTY_WINDOW_ID` 存在，也必须走 Herdr/Rex 分支"。（Rex 分支的 `trueColor: true` 与
   Kitty 分支相同，但 `images` 也相同，故这两个用例真正钉住的是 Herdr 的 `trueColor: hint` 语义与
   Rex/Kitty 的优先级。）
6. **`terminal.ts:304-318`** 的注释解释了 `pendingKeyboardProtocolDeviceAttributes` 的计数语义：
   "The last owed DA answers the latest query, which got no program status reply first. Earlier DA
   replies belong to queries from before a restart."——这就是 `programStatusQueryPending` 只在计数器
   归零时清除的原因（测试 `does not let a DA reply from before a restart end the new query`）。
7. **`tui-alt-screen.ts:1481-1484`**（`copyTextToClipboard` 上方）：优先用注入的 `copySelection`，
   因为裸 OSC 52 可能在 macOS Terminal.app / 无 OSC 52 透传的 tmux 里显示 "Copied!" 却没有真正复制；
   **只有在真的复制成功时才报告成功**。
8. **`tui.ts:505-506`**（`pendingTerminalColorQueries` 上方）：终端按顺序回答，所以颜色回复属于**最老**的
   未决查询；查询超时后仍留在队列里以收集迟到的回复（对应 §7.11 的 `reports late replies after a timeout`）。
9. **`tui.ts:557-560`**（`exitRestoreHandler` 上方）：`stop()` 之外的退出路径（如直接 `process.exit()`）
   的最后兜底；**硬杀与 native abort 不执行任何 JavaScript，无法覆盖**（#11170）。
10. **`tui.ts:559-560`**（`logDirectory` 字段）：`undefined` 时禁用调试日志，崩溃转储回退到 OS 临时目录。
11. **`tui.ts` `compositeOverlays` 注释**：`workingHeight` 的 `max(result.length, termHeight, minLinesNeeded)`
    **故意不含 `maxLinesRendered`**——"the historical high-water mark caused self-reinforcing inflation that
    pushed content into scrollback on terminal widen"。Rust 侧不要"顺手"把高水位加回来。
12. **`tui-main-screen.ts` 的 `BoundedTerminalWriter`**：按 UTF-16 code unit 计数切分时**不拆代理对**
    （surrogate pair），否则会产生非法 UTF-8 写入。切分点落在代理对中间时 `end--`（**回退一格**），
    若回退后 `end === offset`（即 `capacity === 1` 且当前就是高代理项）则先 `flush()` 再继续——
    这一段在 §2.10.1 有逐字代码，Rust 侧照抄即可，但要注意 `get length()` 返回
    `writtenChars + buffer.length`（**已写 + 未写**），它是调试日志里 `[<n> chars written in bounded chunks]`
    的数据来源。

### A4 §2.5 键处理的补充事实

- `matchesLegacySequence = (data, sequences) => sequences.includes(data)`——**纯 `includes`**，
  没有前缀/正则匹配；Rust 侧用 `sequences.contains(&data)`，不要用 `starts_with`。
- `getLogicalShiftedSymbolCandidate(cp, modifier, allowedSymbols)` 的 `allowedSymbols` 参数是
  `ReadonlySet<string>`：`SYMBOL_KEYS`（Kitty 路径）与 `SHIFTED_SYMBOL_KEYS`（modifyOtherKeys 路径）
  **不同**，不能合并成一张表。
- `SHIFTED_SYMBOL_KEYS` 的 20 个字符为 `~!@#$%^&*()_+|{}:<>?`（注意**没有** `` ` ``、`-`、`=`、`[`、`]`、
  `\`、`;`、`'`、`,`、`.`、`/`）。
- `parseKey` 的 `LEGACY_SEQUENCE_KEY_IDS` 是 `data → KeyId` 的**精确**映射表（非前缀匹配），
  顺序在 kitty 的 `\x1b\r`/`\n` 特判**之后**。
- `Key` 对象的符号键名共 31 个，与 `SYMBOL_KEYS` 一一对应（`Key.percent === "%"`）。
- 修饰符**格式化顺序** `shift+ctrl+alt+super` 与**解析顺序** `MODIFIER_NAMES = shift,alt,ctrl,super`
  不同：`parseKey("ctrl+shift+p")` 返回 `"shift+ctrl+p"`。§2.5.8 已注明，但 Rust 侧极易写成同一个顺序。

### A5 §2.9 / §2.10 渲染器的补充事实

1. `TuiBase` 的公开成员以**源码为准**：`get hasOverlayEntries()` 是 **public**（§2.9.7 原文写 protected，已就地修正）；
   `get fullRedraws(): number` 返回 `protected fullRedrawCount`。
2. `TuiBase` 中**不存在** `getFocusedOverlay()`；只有 `protected isOverlayFocused(): boolean`（`tui.ts:878`）。
3. `compositeOverlays` 对每个 overlay 渲染后做**防御性截断**：
   `visibleWidth(overlayLines[i]) > w ? sliceByColumn(overlayLines[i], 0, w, true) : overlayLines[i]`。
   这一步发生在 `compositeTuiLine` **之前**，且用的是 `sliceByColumn(..., true)`（strict about）。
4. `compositeTuiLine` 对**图片行**有短路：`if (isImageLine(baseLine)) return baseLine;`
   （§2.9.5 已记录，但常被漏掉的是"返回**原始 baseLine**，连 overlay 都不画"）。
5. `TuiMainScreen` 的 `BoundedTerminalWriter` 与崩溃日志共享同一个 `logDirectory`；
   `KIANA_TUI_DEBUG_REDRAW` **只在 `logDirectory` 已配置时**生效（`KIANA_TUI_DEBUG` 则固定写 `/tmp/tui`）。
6. `fullRender` 的 8 种 reason 字符串**逐字**列在 §4.2；Rust 侧若改一个字，`writes redraw logs to the
   provided directory` 用例仍会过（它只断言 `first render`），但人工排查日志会失真。
7. **鼠标模式选择的理由**（`tui-alt-screen.ts:373-375` 注释）：
   "Multiplexers can lag when every pointer movement is forwarded. Button-motion tracking preserves
   clicks, wheel events, selections, and scrollbar dragging." —— 即 `?1003h`（全动作）在 tmux/screen
   里因逐指移动转发而卡顿，故多路复用器内降级为 `?1002h`（按键 + 拖动）。Rust 侧不要"统一用全动作"。
   条件为 `TMUX !== undefined || ZELLIJ !== undefined || STY !== undefined || term.startsWith("tmux")
   || term.startsWith("screen")`；**注意 `TERM` 已经是小写后的值**（`process.env.TERM?.toLowerCase() ?? ""`）。
8. `TuiAltScreen.doRender` 的 Kitty 图片顺序：**WezTerm 有三趟**（先删、再画被覆盖行、最后画图片），
   其它终端两趟；`drawKittyImagesLast = redrawImages && imageProtocol === "kitty" && screen.some(isImageLine)
   && isWezTerm`。§2.13 已记录条件，此处强调 `isWezTerm` 是**独立**的 `TERM_PROGRAM`/`TERM` 判定
   （与 `detectCapabilities` 的 wezterm 分支不是同一函数）。

### A6 §2.12 搜索的补充事实

1. `normalizeQuery(query) = query.replace(/\s+/gu, " ").trim()`——**`\s+` 折叠为单个空格**再 trim；
   因此查询 `"foo   bar"` 与 `"foo bar"` 等价，且**纯空白查询归一化为空串**。
2. `findAltScreenSearchMatches(lines, query)` 对空归一化查询返回 `[]`（不构建语料）。
3. `AltScreenSearchComponent` 的构造函数是
   `(onQueryChange: (q: string) => void, navigationButtonStyle: (text: string, hovered: boolean) => string = (text) => text)`；
   `navigationButtonStyle` **只包裹按钮文字**，不包裹 `" · "` 分隔符。
4. `getNavigationDirectionAt(row, column)` **硬编码 `row !== 2` → undefined**（第 3 行才是按钮行）。
5. `AltScreenSearchIndex.search` 在"未 changed"时返回**同一个数组引用**（`this.matches`），
   调用方不应假设每次都是新数组（Rust 侧需 `Rc<Vec<..>>` 或显式告知调用方）。

### A7 §2.15 图片协议/解析的补充事实

1. `getJpegDimensions` 的循环条件是 `while (offset < buffer.length - 9)`——**不是** `offset < len`。
   这既避免越界读，也意味着文件尾部 9 字节内的 marker 永远不会被解析。
2. `getWebpDimensions` 三个分支内各自还有 `< 30` / `< 25` / `< 30` 的**冗余**长度复检（外层已有 `< 30`）。
   行为上外层覆盖了它们，但 Rust 若照抄必须保留同样的短路顺序；若省略也不改变结果（`VP8L` 分支的 25
   在外层 30 之下确实是冗余）。
3. `getPngDimensions` 用**大端** `readUInt32BE(16)/(20)`，`getGifDimensions` 用**小端**
   `readUInt16LE(6)/(8)`，`getWebpDimensions` 用**小端**（`readUInt16LE(26)/(28)`、`readUInt32LE(21)`）。
   混用端序是本单元最容易被漏掉的字节级差异。
4. `getImageDimensions(b64, mime)` 按 **mime 精确字符串**分派：`image/png`、`image/jpeg`、`image/gif`、
   `image/webp`；其它（含 `image/jpg`、带参数 `image/png; charset=...`）一律 `null`。
5. Kitty 分块切的是**base64 字符串**（`CHUNK_SIZE = 4096` 个字符），不是原始字节。（§4.3/§6.1 已就地修正。）
6. `encodeITerm2` 的 `size=` 用的是 `Buffer.byteLength(base64Data, "base64")`（**解码后**的字节数），
   不是 base64 长度，也不是 `name` 的字节数。
7. 三个删除命令的 **`d` 参数不同**（`q=2` 相同）：
   - `deleteKittyImage(imageId)` = `\x1b_Ga=d,d=I,i=${imageId},q=2\x1b\\`（删单个 image id）
   - `deleteAllKittyPlacements()` = `\x1b_Ga=d,d=a,q=2\x1b\\`（**小写 a**：只删可见 placement，保留已上传数据）
   - `deleteAllKittyImages()` = `\x1b_Ga=d,d=A,q=2\x1b\\`（**大写 A**：连图片数据一起释放）
   `TuiAltScreen` 在差分帧里用 **`deleteAllKittyPlacements()`**（保留数据以支持"只放置不上传"的复用），
   只有 `stop()`/退出路径才用 `deleteAllKittyImages()`。Rust 侧若两者混用，`retains recently offscreen
   Kitty images for placement-only reuse` 会失败。
8. `allocateImageId()` = `Math.floor(Math.random() * 0xfffffffe) + 1`，值域 `[1, 0xfffffffe]`；
   Rust 侧不要用 `0`（0 在 Kitty 协议里有特殊含义）。

### A8 §2.16 native 的补充事实

1. **测试环境变量 `KIANA_TEST_NATIVE_CLIPBOARD`**：仅 `win32` + `"1"` 时运行写读用例，**会覆盖系统剪贴板**；
   `native/win32/README.md` 的 `## Testing clipboard writes` 小节给出了 PowerShell 跑法。已补入 §4.1/§2.16.8。
2. `native-platform.test.ts` 的 4 个用例**名称与 skip 条件**已按源码逐字补入 §7.13/§2.17.2：
   - `Linux ARM64 prebuild supports 64 KB system pages`（无 skip，需 arm64 预编译产物存在）
   - `writes Windows clipboard text through the native helper without command fallbacks`
   - `uses the native platform helper directly as the clipboard API`（断言与 `getNativePlatformHelper()` 恒等）
   - `Linux loads X11 lazily and rechecks DISPLAY`（mock `require.cache` + 改写 `process.platform`）
3. Linux 剪贴板**只读不写**：`linux-platform-x11.c` 只导出 `getText`/`getImage`，没有 `setText`
   （README 亦明确"all Linux writes use the existing command-line or terminal clipboard paths"）。
   Rust 侧不要给 X11 实现加写路径。
4. `getNativeClipboard()` 在 Linux 上**每次调用**都重查 `DISPLAY`；返回的是**缓存实例**，
   但 `DISPLAY` 从有到无会立即变成 `undefined`（测试 `Linux loads X11 lazily and rechecks DISPLAY` 正是钉这个）。
5. Linux ARM64 预编译必须支持 **64 KB 系统页**（`p_align`/`p_vaddr` 对齐），否则在 ARM64 服务器上加载失败。

### A9 测试清单的补充/修正清单

- §7.1 已修正：`latex.test.ts` **20**（原 19）、`autocomplete.test.ts` **41**（原 "7 + 4"）、
  `autocomplete-skill-slash.test.ts` 7（原表尾部重复列出，已删重复行）。
- §2.5.9 / §7.8 已修正 `keys.test.ts` 的分组计数：`matchesKey` **47**（16 + 13 + 18）、
  `decodeKittyPrintable` **1**、`decodePrintableKey` **1**、`parseKey` **12**（6 + 6），合计 61。
- §7.5 `overlay-non-capturing.test.ts` 的 `focus management` 子组实为 **30**（原 25），全文 44 = 30+4+4+6。
- §7.9 `stdin-buffer.test.ts` 的 `Mouse Events` 实为 **7**（原 6），合计仍是 66。
- §7.12 `terminal-image.test.ts` 的分组实际为：
  `isImageLine` **22**（iTerm2 5 + Kitty 3 + Bug regression 4 + Negative cases 7 + Mixed content 3）、
  `detectCapabilities` **27**、`iTerm2 image encoding` 1、`Kitty image cursor movement` **11**、
  `image cell sizing` **7**（Kitty 5 + iTerm2 2）、`imageFallback` 4、`Image transcoding` 4、`hyperlink` 4，
  合计 80。（原文的 23/24/12/9 均偏 1–3，已就地修正。）
- §7.15 `exit-restore.test.ts` 第二个用例断言的序列集合与第一个**不同**（不含 `?1003l`/`?1000l`，
  但含 `?1049l`），且两个用例都通过 `spawnSync` 跑 `test/fixtures/exit-without-stop.ts`。
- `bug-regression-isimageline-startswith-bug.test.ts` 的 11 个用例分四组，逐字名称已补入 §7.12。
- 顶层 `npm test` 的 glob 是 `test/*.test.ts`（**不递归**），`--test-reporter=dot`；Rust 侧 `cargo test` 无此限制。

### A10 环境变量与平台判定的补充

- `CMUX_WORKSPACE_ID`：源码**不读**；测试显式断言它存在时**不会**改变 Ghostty 的能力判定
  （`does not disable Ghostty images solely because cmux is present`）。Rust 侧不要新增该分支。
- `SSH_CONNECTION` / `SSH_CLIENT` / `SSH_TTY`：除 escape 超时与 Windows Terminal 判定外，
  **同时**让 `terminalAcceleratesWheel()` 返回 false（darwin 上不再对滚轮做行数放大）。已补入 §4.1/§2.4。
- `TERM` 的 `-direct` 后缀（如 `xterm-direct`）→ `hasTrueColorHint`；这是 14 条能力分支里
  **唯一**通过 `TERM` 后缀判 truecolor 的路径。
- `KIANA_TUI_ESC_TIMEOUT` 的有效值要求 `Number.isFinite(n) && n > 0`；`0`、负数、`NaN`、空串全部回退。
- 测试 `withEnv()` 会**清空** `ENV_KEYS` 里的变量（含 `CMUX_WORKSPACE_ID`），再注入；Rust 侧等价实现
  必须同样"先清后设"，否则开发机上的真实环境变量会让能力探测用例随机失败。

### A11 仍建议以源码为准的模糊点

1. `isKeyRelease`/`isKeyRepeat` 的**子串**判定（`[:3u]` 等）不区分大小写上下文，理论上会误判
   `\x1b[97;5:3u` 之外的畸形输入；TS 与 Rust 应保持**同样的误判**，不要"修正"。
2. `matchesKey` 的单字符分支（`key.length === 1`）要求 `key` 是 a-z / 0-9 / `SYMBOL_KEYS` 之一；
   其它单字符（例如中文、`é`）返回 false。Rust 侧不要用 `chars().count() == 1` 近似。
3. `parseSizeValue` 的正则 `^(\d+(?:\.\d+)?)%$` **不接受**负号与 `.5%` 形式；`"25%"` 合法、`"-25%"` 非法
   （非法时 overlay 定位回退 `center`，`width`/`maxHeight` 回退默认值）。
4. `applyLineResets`（定义在 `TuiBase`，两个渲染器共用）对**非图片行**补
   `SEGMENT_RESET = "\x1b[0m\x1b]8;;\x07"`——不是 `\x1b[0m`。它**先** `normalizeTerminalOutput(line)`
   再拼 reset；图片行**完全跳过**（既不 normalize 也不加 reset）。
5. `TuiAltScreen` 停机的文档回放**末尾**固定为 `\x1b[0m\x1b[?7h\r\n\x1b[?25h\x1b[?2026l`（顺序固定）。
   退出恢复被拆成**前缀/后缀**两段（`getExitRestorePrefix` / `getExitRestoreSuffix`），顺序是
   `[前缀][terminal 自身的模式复位][后缀]`：
   - 前缀（`TuiAltScreen` override）= `${END_SYNCHRONIZED_OUTPUT}${mouseEnabled ? DISABLE_MOUSE : ""}${ENABLE_AUTOWRAP}${super 前缀}`
     ——源码注释："**Kitty keyboard flags are per screen, so the terminal pops them before the alternate screen is left**"，
     这就是 `\x1b[<u` 必须出现在 `\x1b[?1049l` **之前**的原因（§7.15 的断言正是钉这个顺序）；
   - 后缀 = `${super 后缀}${altScreenActive ? "\x1b[?1049l\x1b[?25h" : ""}`；
   - `altScreenActive === false` 时前缀**原样返回**、后缀为空（未进备用屏时不得多发 `?7h`/鼠标复位）。

### A12 Rust 映射的补充建议

- `Map` 插入顺序语义（`uploadedKittyImages`、`kittyImageMetadata`、`entries`）在 Rust 里**必须**
  用 `indexmap::IndexMap` 或 `Vec` + 索引；换成 `HashMap` 会让 LRU 淘汰顺序与 `AltScreenFlashContainer`
  的渲染顺序变化，直接破坏 `evicts the least recently visible Kitty image when the cache is full`。
- `Math.round` 是**四舍五入（.5 向上）**：`Math.round(-0.5) === -0`、`Math.round(2.5) === 3`；
  Rust 的 `f64::round` 对 .5 是"远离零"，`(-2.5).round() == -3` 而 `Math.round(-2.5) === -2`。
  受影响：滚动条拇指高度/偏移（§3.2）、`calculateImageCellSize`、`getScrollbarGeometry`。
  建议实现 `js_round(x) = (x + 0.5).floor()` 并对负值保持 JS 语义。
- `Math.trunc`、`Number.isFinite`、`Math.max(0, ...)` 在 Rust 里分别对应
  `x.trunc()`、`x.is_finite()`、`.max(0.0)`；但 TS 的 `Number.isFinite(NaN) === false`，
  Rust 的 `f64::is_finite()` 对 NaN 也是 false，两者一致。
- JS 字符串的 `.length` 是 **UTF-16 code unit** 计数；`BoundedTerminalWriter` 的
  `MAX_RENDER_WRITE_CHARS = 1024 * 1024` 也是 code unit 计数。Rust 侧若用 `str::len()`（字节）
  会在含 emoji 的行上**提前切分**，改变写入次数（`splits a large full render without changing its output`
  只比较拼接结果，但 `splits large differential updates without a full redraw` 会观察 `\x1b[?2026h` 次数）。
- `Array.prototype.sort` 在 V8 上是**稳定**排序；`visibleEntries.sort((a,b) => a.focusOrder - b.focusOrder)`
  依赖稳定性（同 `focusOrder` 时保持插入顺序）。Rust 用 `sort_by_key`（稳定）而非 `sort_unstable_by_key`。
### A13 `layout.ts` 的补充事实（§2.11 未显式写出的）

1. `getScrollbarGeometry(box, includeHiddenAuto = false)` 的隐藏条件有三层：
   `!box.scrollView || box.rect.width <= 0 || box.rect.height <= 0` → undefined；
   `!isScrollbarVisible && !canRevealHiddenAuto` → undefined；最后
   `column < clip.x || column >= clip.x + clip.width` → undefined（**列越界时整条滚动条不画**）。
   `canRevealHiddenAuto = includeHiddenAuto && scrollbar === "auto" && contentHeight > trackHeight`。
   这个可选参数是给"鼠标悬停在隐藏轨道上"用的（§2.13 的 `getScrollbarTargetAt`）。
2. `contentHeight = box.children[0]?.rect.height ?? box.scrollContentLines?.length ?? 0`——
   **优先子 box 的布局高度**，其次才是原始渲染行数（滚动视图子组件可能被裁剪）。
3. `paintScrollbar` 对每一行都检查 `row` 是否同时落在 `box.clip` 与 `screen` 范围内；
   拇指用 `isScrollbarActive ? "█" : "┃"`，轨道用 `"│"`，且传给 `replaceScrollbarCell` 的
   `preserveTargetBackground` 参数是 `scrollbar !== "always"`（**注意是取反的语义**：
   `"always"` 时**不**保留背景，因为该列已是滚动条专用列）。
4. OSC 133 区域前缀在**两处**被剥掉：`paintBox` 对每个 `sourceLine` 先
   `sourceLine.replace(OSC133_ZONE_PREFIX, "")` 再判断 Kitty 图片元数据；`TuiAltScreen.afterTerminalStop`
   的停机回放也对 `documentLines` 做同样替换（`line.replace(OSC133_ZONE_PREFIX, "")`）。
   因此屏幕行与 `lastDocument` **都不含** OSC 133 区标记。回放顺序是：
   `resolveFakeCursors(render(width))` → 剥 OSC 133 → `applyLineResets` → `replaceAll(CURSOR_MARKER, "")`
   → 按宽度截断（图片行不截）→ 逐行 `\r\x1b[2K<line>`、行间 `\r\n`
   → 末尾 `\x1b[0m\x1b[?7h\r\n\x1b[?25h\x1b[?2026l`，整段由
   `\x1b[?2026h\x1b[?1049l\x1b[?7l` **开头**（`preserveScreen: true` 时只写
   `\x1b[?2026h\x1b[?1049l\x1b[?25h\x1b[?2026l`，不打印文档）。
5. `renderLayoutFrame` 的 `renderCache` 是**每个 `Component` → 每个宽度**的两级 `Map`，
   同帧内同组件同宽度只渲染一次；`requestRender` 被塞进 `LayoutContext` 供 `updateLayout` 使用。
6. `safeWidth = max(1, floor(width))`、`safeHeight = max(1, floor(height))`——布局帧的尺寸
   **向下取整且至少 1**；`lines` 初始化为 `Array.from({length: safeHeight}, () => "")`。
7. `getLayoutBoxesAt` 的排序与 `getScrollViewsAt` 的排序**不同**，不能共用一个比较器：
   - `getLayoutBoxesAt`：收集所有 `containsPoint(box.clip, x, y)` 的 box（**用 clip**，含自身），
     `sort((a,b) => b.box.layer - a.box.layer || b.depth - a.depth)`（先 layer 降序，再深度降序）；
   - `getScrollViewsAt`：只收集 `containsPoint(box.clip)` **且** `box.scrollView` **且**
     `containsPoint(box.rect, x, y)`（**rect 也要命中**）的 box，`sort((a,b) => b.depth - a.depth)`
     ——**完全不看 layer**。
   `getScrollViewBox(frame, scrollView)` 按前序遍历找 `box.scrollView === scrollView` 的第一个匹配
   （没有排序）。
8. `renderCached(context, component, width)` 是**私有**工具，`safeWidth = max(1, floor(width))`；
   它缓存的是**数组引用**（组件返回的 `string[]` 不被复制），因此组件后续原地修改返回值会污染缓存。
