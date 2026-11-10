// `@earendil-works/kiana-tui` 的运行期面（设计文档 §5.1、§9.2）。
//
// Installed by `kiana_extensions::ui::install()` after the native classes are defined into
// `globalThis.__kiana_tui_native`. The file only depends on that object; it defines the pure-JS
// composition classes (`Container`, `Stack`, `VStack`, `HStack`), the `Editor`/`CustomEditor`
// JS subclasses (so extension code can `extends`/`super`), `Key`, and the cursor markers.
//
// TS 源：`packages/tui/src/tui.ts:358-429`（Container）、
// `packages/tui/src/components/stack.ts`、`packages/coding-agent/src/modes/interactive/
// components/custom-editor.ts`、`packages/tui/src/keys.ts`（Key）。

(function () {
  "use strict";

  const native = globalThis.__kiana_tui_native;
  if (!native) throw new Error("kiana-tui native bindings are not installed");

  const visibleWidth = native.visibleWidth;

  // ---------------------------------------------------------------------
  // Cursor markers (`tui.ts:196-207`)
  // ---------------------------------------------------------------------

  const CURSOR_MARKER = "\x1b_kiana:c\x07";
  const FAKE_CURSOR_START = "\x1b_kiana:fc\x07";
  const FAKE_CURSOR_END = "\x1b_kiana:/fc\x07";

  // ---------------------------------------------------------------------
  // Layout nodes (`layout-node.ts`)
  // ---------------------------------------------------------------------

  const LAYOUT_NODE = Symbol.for("@earendil-works/kiana-tui/layout-node");

  function getLayoutNode(component) {
    if (component === null || component === undefined) return undefined;
    const candidate = component[LAYOUT_NODE];
    return typeof candidate === "function" ? candidate.call(component) : undefined;
  }

  function isFocusable(component) {
    return component !== null && component !== undefined && "focused" in component;
  }

  // ---------------------------------------------------------------------
  // Container (`tui.ts:358-429`)
  // ---------------------------------------------------------------------

  function dispatchMouseEvent(component, event) {
    const result = component.handleMouse ? component.handleMouse(event) : undefined;
    if (!result) return undefined;
    if ("target" in result) {
      return result.focus && component.handleInput
        ? Object.assign({}, result, { focusTarget: component })
        : result;
    }
    if (!result.handled && !result.capture && !result.focus) return undefined;
    const dispatched = Object.assign({}, result, { handled: true });
    if (result.focus) dispatched.focusTarget = component;
    dispatched.target = {
      component,
      originX: event.screenX - event.x,
      originY: event.screenY - event.y,
      width: event.width,
      height: event.height,
    };
    return dispatched;
  }

  class Container {
    constructor() {
      this.children = [];
      this.mouseLayout = undefined;
    }

    addChild(component) {
      this.children.push(component);
    }

    removeChild(component) {
      const index = this.children.indexOf(component);
      if (index !== -1) this.children.splice(index, 1);
    }

    clear() {
      this.children = [];
    }

    invalidate() {
      for (const child of this.children) {
        if (typeof child.invalidate === "function") child.invalidate();
      }
    }

    handleMouse(event) {
      if (event.y < 0 || event.y >= event.height) return undefined;
      const mouseChildren =
        this.mouseLayout && this.mouseLayout.width === event.width
          ? this.mouseLayout.children
          : this.children.map((component) => ({
              component,
              height: component.render(event.width).length,
            }));
      let childY = 0;
      for (const entry of mouseChildren) {
        const child = entry.component;
        const childHeight = entry.height;
        if (event.y >= childY && event.y < childY + childHeight) {
          const result = dispatchMouseEvent(
            child,
            Object.assign({}, event, { y: event.y - childY, height: childHeight }),
          );
          if (result && result.focus && typeof this.handleInput === "function") {
            return Object.assign({}, result, { focusTarget: this });
          }
          return result;
        }
        childY += childHeight;
      }
      return undefined;
    }

    render(width) {
      const lines = [];
      const mouseChildren = [];
      for (const child of this.children) {
        const childLines = child.render(width);
        mouseChildren.push({ component: child, height: childLines.length });
        for (const line of childLines) lines.push(line);
      }
      this.mouseLayout = { width, children: mouseChildren };
      return lines;
    }
  }

  // ---------------------------------------------------------------------
  // Stack / VStack / HStack (`components/stack.ts`)
  // ---------------------------------------------------------------------

  function isStackEntry(child) {
    return !("render" in child);
  }

  function normalizeSize(value, fallback) {
    return value === undefined || !Number.isFinite(value)
      ? fallback
      : Math.max(0, Math.floor(value));
  }

  // `components/stack.ts`：可见条目、尺寸分配（grow/shrink/clamp）。
  function visibleStackEntries(entries, viewport) {
    return entries.filter((entry) => (entry.visible ? entry.visible(viewport) : true));
  }

  function clampSize(size, entry) {
    const min = Math.max(0, Math.floor(entry.minSize ?? 0));
    const max = Math.max(min, Math.floor(entry.maxSize ?? Number.MAX_SAFE_INTEGER));
    return Math.max(min, Math.min(max, Math.max(0, Math.floor(size))));
  }

  function distribute(sizes, entries, amount, mode) {
    let remaining = amount;
    while (remaining > 0) {
      const candidates = entries
        .map((entry, index) => ({ entry, index }))
        .filter(({ entry, index }) => {
          if (mode === "grow") {
            return (entry.grow ?? 0) > 0 && sizes[index] < (entry.maxSize ?? Number.MAX_SAFE_INTEGER);
          }
          return (entry.shrink ?? 1) > 0 && sizes[index] > (entry.minSize ?? 0);
        });
      if (candidates.length === 0) return;

      const totalWeight = candidates.reduce((sum, { entry, index }) => {
        return sum + (mode === "grow" ? (entry.grow ?? 0) : (entry.shrink ?? 1) * Math.max(1, sizes[index]));
      }, 0);
      let distributed = 0;
      for (const { entry, index } of candidates) {
        if (remaining <= 0) break;
        const weight = mode === "grow" ? (entry.grow ?? 0) : (entry.shrink ?? 1) * Math.max(1, sizes[index]);
        const proposed = Math.max(1, Math.floor((remaining * weight) / totalWeight));
        const capacity =
          mode === "grow"
            ? (entry.maxSize ?? Number.MAX_SAFE_INTEGER) - sizes[index]
            : sizes[index] - (entry.minSize ?? 0);
        const delta = Math.min(remaining, proposed, capacity);
        if (delta <= 0) continue;
        sizes[index] = sizes[index] + (mode === "grow" ? delta : -delta);
        remaining -= delta;
        distributed += delta;
      }
      if (distributed === 0) return;
    }
  }

  function allocateStackSizes(entries, intrinsicSizes, availableSize, gap) {
    const sizes = entries.map((entry, index) =>
      clampSize(
        entry.basis === undefined || entry.basis === "auto"
          ? (intrinsicSizes[index] ?? 0)
          : entry.basis,
        entry,
      ),
    );
    if (availableSize === undefined) return sizes;

    const contentSize = Math.max(0, Math.floor(availableSize) - Math.max(0, entries.length - 1) * gap);
    const total = sizes.reduce((sum, size) => sum + size, 0);
    if (total < contentSize) distribute(sizes, entries, contentSize - total, "grow");
    else if (total > contentSize) distribute(sizes, entries, total - contentSize, "shrink");
    return sizes;
  }

  class Stack extends Container {
    constructor(children = [], options = {}) {
      super();
      this.entries = [];
      this.gap = normalizeSize(options.gap, 0);
      this.align = options.align ?? "stretch";
      for (const child of children) {
        if (isStackEntry(child)) this.addChild(child.component, child);
        else this.addChild(child);
      }
    }

    addChild(component, options = {}) {
      super.addChild(component);
      const entry = { component };
      if (options.basis !== undefined) entry.basis = options.basis;
      if (options.grow !== undefined) entry.grow = normalizeSize(options.grow, 0);
      if (options.shrink !== undefined) entry.shrink = normalizeSize(options.shrink, 1);
      if (options.minSize !== undefined) entry.minSize = normalizeSize(options.minSize, 0);
      if (options.maxSize !== undefined) {
        entry.maxSize = normalizeSize(options.maxSize, Number.MAX_SAFE_INTEGER);
      }
      if (options.visible !== undefined) entry.visible = options.visible;
      this.entries.push(entry);
    }

    removeChild(component) {
      super.removeChild(component);
      const index = this.entries.findIndex((entry) => entry.component === component);
      if (index !== -1) this.entries.splice(index, 1);
    }

    clear() {
      super.clear();
      this.entries.length = 0;
    }

    [LAYOUT_NODE]() {
      return {
        type: this.layoutType,
        entries: this.entries,
        gap: this.gap,
        align: this.align,
      };
    }
  }

  // `components/v-stack.ts`：纵向堆叠，`gap` 与条目尺寸（basis/grow/shrink/min/max）生效。
  class VStack extends Stack {
    layoutType = "vstack";

    render(width) {
      const viewport = { width: Math.max(1, width), height: Number.MAX_SAFE_INTEGER };
      const entries = visibleStackEntries(this.entries, viewport);
      const rendered = entries.map((entry) => entry.component.render(viewport.width));
      const sizes = allocateStackSizes(
        entries,
        rendered.map((lines) => lines.length),
        undefined,
        this.gap,
      );
      const lines = [];
      for (let index = 0; index < entries.length; index++) {
        if (index > 0) {
          for (let gap = 0; gap < this.gap; gap++) lines.push("");
        }
        const childLines = rendered[index].slice(0, sizes[index]);
        for (const line of childLines) lines.push(line);
        for (let padding = childLines.length; padding < sizes[index]; padding++) lines.push("");
      }
      return lines;
    }
  }

  // `components/h-stack.ts`：横向合成（`compositeTuiLine`），`align` 决定垂直对齐。
  class HStack extends Stack {
    layoutType = "hstack";

    render(width) {
      const safeWidth = Math.max(1, width);
      const viewport = { width: safeWidth, height: Number.MAX_SAFE_INTEGER };
      const entries = visibleStackEntries(this.entries, viewport);
      if (entries.length === 0) return [];

      const intrinsicWidths = entries.map((entry) => {
        const lines = entry.component.render(safeWidth);
        return lines.reduce((max, line) => Math.max(max, visibleWidth(line)), 0);
      });
      const widths = allocateStackSizes(entries, intrinsicWidths, safeWidth, this.gap);
      const rendered = entries.map((entry, index) =>
        widths[index] === 0 ? [] : entry.component.render(widths[index]),
      );
      const height = rendered.reduce((max, lines) => Math.max(max, lines.length), 0);
      const result = Array.from({ length: height }, () => "");
      let x = 0;
      for (let index = 0; index < rendered.length; index++) {
        const lines = rendered[index];
        const childWidth = widths[index];
        let offset = 0;
        if (this.align === "center") offset = Math.floor((height - lines.length) / 2);
        else if (this.align === "end") offset = height - lines.length;
        for (let row = 0; row < lines.length; row++) {
          const target = row + offset;
          if (target < 0 || target >= result.length) continue;
          result[target] = native.compositeTuiLine(result[target], lines[row], x, childWidth, safeWidth);
        }
        x += childWidth + this.gap;
      }
      return result;
    }
  }

  // ---------------------------------------------------------------------
  // Editor (`components/editor.ts` 的原生类 + `This` 派发的边框)
  // ---------------------------------------------------------------------

  // The native class renders the frame and, for every border line, calls
  // `this.renderTopBorder`/`this.renderBottomBorder` through `This` (design §5.4). The JS base
  // methods only apply `borderColor` to the uncolored border text produced by Rust, so an
  // extension override can call `super.renderTopBorder(...)` and keep the default frame.
  class Editor extends native.Editor {
    constructor(tui, theme, options = {}) {
      super(tui, theme, options);
      // TS `Editor` 把 TUI 存进 protected 字段（editor.ts:307、375）：子类的
      // `this.tui.requestRender()`（rainbow-editor.ts:56 等）与 `this.tui.terminal.rows`
      // （editor.ts:537、1963）都依赖它。
      this.tui = tui;
    }

    renderTopBorder(width, hiddenLineCount) {
      return this.borderColor(this.__topBorderBase(width, hiddenLineCount));
    }

    renderBottomBorder(width, hiddenLineCount) {
      return this.borderColor(this.__bottomBorderBase(width, hiddenLineCount));
    }
  }

  // `coding-agent/src/modes/interactive/components/custom-editor.ts`.
  class CustomEditor extends Editor {
    constructor(tui, theme, keybindings, options = {}) {
      super(tui, theme, options);
      this.keybindings = keybindings;
      this.workingStatusIndicator = undefined;
      this.embedWorkingStatus = options.embedWorkingStatus === true;
      this.actionHandlers = new Map();
      this.onEscape = undefined;
      this.onCtrlD = undefined;
      this.onPasteImage = undefined;
      this.onExtensionShortcut = undefined;
    }

    setWorkingStatusIndicator(indicator) {
      this.workingStatusIndicator = indicator;
    }

    renderTopBorder(width, hiddenLineCount) {
      if (!this.embedWorkingStatus || !this.workingStatusIndicator || width <= 0) {
        return super.renderTopBorder(width, hiddenLineCount);
      }

      let status = this.workingStatusIndicator.renderInBorder(Math.max(1, width - 5));
      let statusWidth = visibleWidth(status);
      if (statusWidth === 0) return super.renderTopBorder(width, hiddenLineCount);

      const overflowLabel = hiddenLineCount > 0 ? ` ↑ ${hiddenLineCount} more ` : undefined;
      const overflowLabelWidth = overflowLabel ? visibleWidth(overflowLabel) : 0;
      const overflowStart = Math.floor((width - overflowLabelWidth) / 2);
      const canFitOverflow = () =>
        overflowLabel !== undefined &&
        overflowLabelWidth + 2 <= width &&
        overflowStart - (3 + statusWidth + 1) >= 1;

      if (overflowLabel && !canFitOverflow()) {
        status = this.workingStatusIndicator.renderSpinnerInBorder(width);
        statusWidth = visibleWidth(status);
      }

      if (canFitOverflow()) {
        const leftBlockWidth = 3 + statusWidth + 1;
        return (
          this.borderColor("── ") +
          status +
          this.borderColor(
            ` ${"─".repeat(Math.max(0, overflowStart - leftBlockWidth))}` +
              overflowLabel +
              "─".repeat(Math.max(0, width - overflowStart - overflowLabelWidth)),
          )
        );
      }

      if (width >= statusWidth + 5) {
        return (
          this.borderColor("── ") +
          status +
          this.borderColor(` ${"─".repeat(Math.max(0, width - statusWidth - 4))}`)
        );
      }

      status = this.workingStatusIndicator.renderSpinnerInBorder(width);
      statusWidth = visibleWidth(status);
      const prefixWidth = Math.min(3, Math.max(0, width - statusWidth));
      return (
        this.borderColor("─".repeat(prefixWidth)) +
        status +
        this.borderColor("─".repeat(Math.max(0, width - prefixWidth - statusWidth)))
      );
    }

    onAction(action, handler) {
      this.actionHandlers.set(action, handler);
    }

    handleInput(data) {
      if (typeof this.onExtensionShortcut === "function" && this.onExtensionShortcut(data)) {
        return;
      }

      if (this.matchesBinding(data, "app.clipboard.pasteImage")) {
        if (typeof this.onPasteImage === "function") this.onPasteImage();
        return;
      }

      if (this.matchesBinding(data, "app.interrupt")) {
        if (!this.isShowingAutocomplete()) {
          const handler =
            this.onEscape !== undefined
              ? this.onEscape
              : this.actionHandlers.get("app.interrupt");
          if (handler !== undefined) {
            handler();
            return;
          }
        }
        super.handleInput(data);
        return;
      }

      if (this.matchesBinding(data, "app.exit")) {
        if (this.getText().length === 0) {
          const handler =
            this.onCtrlD !== undefined ? this.onCtrlD : this.actionHandlers.get("app.exit");
          if (handler !== undefined) handler();
          return;
        }
      }

      if (
        this.matchesBinding(data, "tui.editor.historyPrevious") ||
        this.matchesBinding(data, "tui.editor.historyNext")
      ) {
        super.handleInput(data);
        return;
      }

      for (const [action, handler] of this.actionHandlers) {
        if (
          action !== "app.interrupt" &&
          action !== "app.exit" &&
          this.matchesBinding(data, action)
        ) {
          handler();
          return;
        }
      }

      super.handleInput(data);
    }

    matchesBinding(data, action) {
      return this.keybindings !== null && this.keybindings !== undefined &&
        typeof this.keybindings.matches === "function"
        ? this.keybindings.matches(data, action)
        : false;
    }
  }

  // ---------------------------------------------------------------------
  // Key (`keys.ts:163-259`)
  // ---------------------------------------------------------------------

  const Key = {
    escape: "escape",
    esc: "esc",
    enter: "enter",
    return: "return",
    tab: "tab",
    space: "space",
    backspace: "backspace",
    delete: "delete",
    insert: "insert",
    clear: "clear",
    home: "home",
    end: "end",
    pageUp: "pageUp",
    pageDown: "pageDown",
    up: "up",
    down: "down",
    left: "left",
    right: "right",
    f1: "f1",
    f2: "f2",
    f3: "f3",
    f4: "f4",
    f5: "f5",
    f6: "f6",
    f7: "f7",
    f8: "f8",
    f9: "f9",
    f10: "f10",
    f11: "f11",
    f12: "f12",

    backtick: "`",
    hyphen: "-",
    equals: "=",
    leftbracket: "[",
    rightbracket: "]",
    backslash: "\\",
    semicolon: ";",
    quote: "'",
    comma: ",",
    period: ".",
    slash: "/",
    exclamation: "!",
    at: "@",
    hash: "#",
    dollar: "$",
    percent: "%",
    caret: "^",
    ampersand: "&",
    asterisk: "*",
    leftparen: "(",
    rightparen: ")",
    underscore: "_",
    plus: "+",
    pipe: "|",
    tilde: "~",
    leftbrace: "{",
    rightbrace: "}",
    colon: ":",
    lessthan: "<",
    greaterthan: ">",
    question: "?",

    ctrl: (key) => `ctrl+${key}`,
    shift: (key) => `shift+${key}`,
    alt: (key) => `alt+${key}`,
    super: (key) => `super+${key}`,

    ctrlShift: (key) => `ctrl+shift+${key}`,
    shiftCtrl: (key) => `shift+ctrl+${key}`,
    ctrlAlt: (key) => `ctrl+alt+${key}`,
    altCtrl: (key) => `alt+ctrl+${key}`,
    shiftAlt: (key) => `shift+alt+${key}`,
    altShift: (key) => `alt+shift+${key}`,
    ctrlSuper: (key) => `ctrl+super+${key}`,
    superCtrl: (key) => `super+ctrl+${key}`,
    shiftSuper: (key) => `shift+super+${key}`,
    superShift: (key) => `super+shift+${key}`,
    altSuper: (key) => `alt+super+${key}`,
    superAlt: (key) => `super+alt+${key}`,

    ctrlShiftAlt: (key) => `ctrl+shift+alt+${key}`,
    ctrlShiftSuper: (key) => `ctrl+shift+super+${key}`,
  };

  // ---------------------------------------------------------------------
  // Export surface
  // ---------------------------------------------------------------------

  globalThis.__kiana_tui = {
    // Composition (pure JS)
    Container,
    Stack,
    VStack,
    HStack,
    Editor,
    CustomEditor,

    // Components (native)
    Text: native.Text,
    Box: native.Box,
    Spacer: native.Spacer,
    TruncatedText: native.TruncatedText,
    DynamicBorder: native.DynamicBorder,
    Loader: native.Loader,
    CancellableLoader: native.CancellableLoader,
    BorderedLoader: native.BorderedLoader,
    Input: native.Input,
    SelectList: native.SelectList,
    SettingsList: native.SettingsList,
    Markdown: native.Markdown,
    Image: native.Image,
    ScrollView: native.ScrollView,

    // Host-side classes handed to factories
    KeybindingsManager: native.KeybindingsManager,
    FooterDataProvider: native.FooterDataProvider,
    TUIProxy: native.TUIProxy,
    OverlayHandle: native.OverlayHandle,
    Theme: native.Theme,

    // Text utilities (native)
    visibleWidth: native.visibleWidth,
    truncateToWidth: native.truncateToWidth,
    wrapTextWithAnsi: native.wrapTextWithAnsi,
    sliceByColumn: native.sliceByColumn,
    fuzzyFilter: native.fuzzyFilter,
    matchesKey: native.matchesKey,
    parseKey: native.parseKey,
    isKeyRelease: native.isKeyRelease,
    renderFakeCursor: native.renderFakeCursor,
    compositeTuiLine: native.compositeTuiLine,

    // Cursor markers
    CURSOR_MARKER,
    FAKE_CURSOR_START,
    FAKE_CURSOR_END,

    // Keys
    Key,

    // Layout / focus helpers
    LAYOUT_NODE,
    getLayoutNode,
    isFocusable,
  };
})();
