// `ctx.ui` 的 JS 桥（设计文档 §4.5、§5.6、§9.2）。
//
// Installed by `kiana_extensions::ui::install()`. `runner/context.rs` calls the global
// `__kiana_ctx_make_ui(handle)` for `ctx.ui`; this file returns `undefined` for the no-op
// context (print/json), so `js/context.js` supplies its own `noopUi` single source.
//
// 原生类 `ExtensionUI` 实现 27 个同步成员；这里补齐 TS 对象字面量的语义：
//   - 对话框（select/confirm/input/editor）走 `__kiana.promise("kiana_ui", …)`，带
//     `signal`/`timeout`（signal 先由原生 `linkSignal` 换成句柄，预中止直接消解）；
//   - `custom(factory, options)` 把工厂/动态 overlay 选项/`onHandle` 存成引用 id；
//   - `setWidget` 三个重载（string[] / factory / undefined）；
//   - `theme` 是只读属性（原生方法是 `theme()`）。

(function () {
  "use strict";

  const uiNative = globalThis.__kiana_ui_native;
  const kiana = globalThis.__kiana;
  if (!uiNative) throw new Error("__kiana_ui_native is not installed");
  if (!kiana) throw new Error("__kiana is not installed");

  function call(handle, op, args) {
    return kiana.promise("kiana_ui", {
      handle,
      op,
      args: args === undefined ? null : args,
    });
  }

  function storeRef(value) {
    if (typeof value !== "function") {
      throw new TypeError("expected a function");
    }
    return uiNative.storeRef(value);
  }

  function linkSignal(signal) {
    if (signal === null || signal === undefined) return undefined;
    if (signal.aborted) return -1;
    try {
      return uiNative.linkSignal(signal);
    } catch (_) {
      return undefined;
    }
  }

  function dialogArgs(options) {
    const args = {};
    if (options !== null && options !== undefined) {
      if (options.signal !== undefined) {
        const id = linkSignal(options.signal);
        if (id !== undefined) args.signalId = id;
      }
      if (options.timeout !== undefined) args.timeout = options.timeout;
    }
    return args;
  }

  function withDialogArgs(args, options) {
    return Object.assign(args, dialogArgs(options));
  }

  // OverlayOptions 是普通对象；函数（`() => OverlayOptions`）走引用，`visible` 单独走引用
  // （原生 `overlay_options_from_json` 不解析函数字段）。
  function overlayArgs(options, args) {
    if (options.overlay !== undefined) args.overlay = options.overlay;
    const overlayOptions = options.overlayOptions;
    if (overlayOptions === undefined || overlayOptions === null) return;
    if (typeof overlayOptions === "function") {
      args.overlayOptionsRef = storeRef(overlayOptions);
      return;
    }
    if (typeof overlayOptions !== "object") return;
    let copy = overlayOptions;
    if (typeof overlayOptions.visible === "function") {
      args.overlayVisibleRef = storeRef(overlayOptions.visible);
      copy = Object.assign({}, overlayOptions);
      delete copy.visible;
    }
    try {
      args.overlayOptions = JSON.parse(JSON.stringify(copy));
    } catch (_) {
      args.overlayOptions = null;
    }
  }

  // TS 的对象字面量（`interactive-mode.ts:2633-2690`）：成员顺序与 no-op 版本一致。
  function wrap(handle, native) {
    return {
      select(title, options, opts) {
        // 提前取消的 dialog 仍要走宿主：TS 的早退在 `withUIPrompt` 内部（mode 实现），
        // `ui_prompt_start` / `ui_prompt_end` 这一对事件照发（见 Rust 的 UiPromptContext）。
        return call(
          handle,
          "select",
          withDialogArgs(
            {
              title: String(title),
              options: Array.from(options === undefined || options === null ? [] : options, String),
            },
            opts,
          ),
        );
      },

      confirm(title, message, opts) {
        return call(
          handle,
          "confirm",
          withDialogArgs({ title: String(title), message: String(message) }, opts),
        );
      },

      input(title, placeholder, opts) {
        const args = withDialogArgs({ title: String(title) }, opts);
        if (placeholder !== undefined && placeholder !== null) {
          args.placeholder = String(placeholder);
        }
        return call(handle, "input", args);
      },

      notify(message, type) {
        if (type === undefined) native.notify(message);
        else native.notify(message, type);
      },

      onTerminalInput(handler) {
        return native.onTerminalInput(handler);
      },

      setStatus(key, text) {
        if (text === undefined) native.setStatus(key);
        else native.setStatus(key, text);
      },

      setWorkingMessage(message) {
        if (message === undefined) native.setWorkingMessage();
        else native.setWorkingMessage(message);
      },

      setWorkingVisible(visible) {
        native.setWorkingVisible(visible);
      },

      setWorkingIndicator(options) {
        native.setWorkingIndicator(options);
      },

      setHiddenThinkingLabel(label) {
        if (label === undefined) native.setHiddenThinkingLabel();
        else native.setHiddenThinkingLabel(label);
      },

      setWidget(key, content, options) {
        const placement =
          options !== null && options !== undefined ? options.placement : undefined;
        // `undefined` 是“清除/隐藏”；RPC 的 setWidget 记录仍带 options.placement
        // （`rpc-mode.ts:203-212`），所以把它交给原生 clearWidget。
        if (content === undefined) {
          if (placement === undefined) native.clearWidget(key);
          else native.clearWidget(key, placement);
          return;
        }
        if (typeof content === "function") {
          if (placement === undefined) native.setWidgetFactory(key, content);
          else native.setWidgetFactory(key, content, placement);
          return;
        }
        if (!Array.isArray(content)) {
          // TS rpc-mode：content 既不是 undefined 也不是数组 -> 不发任何记录
          // （null/string/数字/对象都是这种形状）。TS interactive 会在 `content(...)`
          // 上抛 TypeError；类型化 API 无法表达这种非法调用，这里按 RPC 语义静默忽略。
          return;
        }
        const lines = Array.from(content, String);
        if (placement === undefined) native.setWidgetLines(key, lines);
        else native.setWidgetLines(key, lines, placement);
      },

      setFooter(factory) {
        native.setFooter(factory);
      },

      setHeader(factory) {
        native.setHeader(factory);
      },

      setTitle(title) {
        native.setTitle(title);
      },

      custom(factory, options) {
        const args = { factoryRef: storeRef(factory) };
        if (options !== null && options !== undefined) {
          overlayArgs(options, args);
          if (options.onHandle !== undefined && options.onHandle !== null) {
            args.onHandleRef = storeRef(options.onHandle);
          }
        }
        return call(handle, "custom", args);
      },

      pasteToEditor(text) {
        native.pasteToEditor(text);
      },

      setEditorText(text) {
        native.setEditorText(text);
      },

      getEditorText() {
        return native.getEditorText();
      },

      editor(title, prefill) {
        const args = { title: String(title) };
        if (prefill !== undefined && prefill !== null) args.prefill = String(prefill);
        return call(handle, "editor", args);
      },

      addAutocompleteProvider(factory) {
        native.addAutocompleteProvider(factory);
      },

      setEditorComponent(factory) {
        native.setEditorComponent(factory);
      },

      getEditorComponent() {
        return native.getEditorComponent();
      },

      get theme() {
        return native.theme();
      },

      getAllThemes() {
        return native.getAllThemes();
      },

      getTheme(name) {
        return native.getTheme(name);
      },

      setTheme(themeOrName) {
        return native.setTheme(themeOrName);
      },

      getToolsExpanded() {
        return native.getToolsExpanded();
      },

      setToolsExpanded(expanded) {
        native.setToolsExpanded(expanded);
      },
    };
  }

  // `context.js` 的 `makeUi(handle)`：无 UI（print/json / 未知句柄）返回 undefined，于是
  // 上下文回退到它自己的 `noopUi`（与 TS 的 `noOpUIContext` 逐字一致）。
  //
  // TS 的 `runner.uiContext` 是**单个**对象（`runner.ts:887-890`），所以 `ctx.ui === ctx.ui`
  // 必须成立、`setEditorComponent` 之后任意一次 `getEditorComponent()` 都看得见同一个工厂。
  // 这里按句柄缓存包装对象（句柄与 `Arc<dyn ExtensionUiContext>` 一一对应）。
  const uiObjects = new Map();

  globalThis.__kiana_ctx_make_ui = function makeUi(handle) {
    if (uiNative.isNoopUi(handle)) return undefined;
    const cached = uiObjects.get(handle);
    if (cached !== undefined) return cached;
    let ui;
    try {
      const native = new uiNative.ExtensionUI(handle);
      ui = wrap(handle, native);
    } catch (_) {
      return undefined;
    }
    uiObjects.set(handle, ui);
    return ui;
  };

  // TS `containsComponent(root, target)`（`tui.ts:742-746`）：`root === target` 或
  // `root` 的 `children` 数组里有目标。原生类（`Box` 等）的孩子由 Rust 侧的挂载树遍历覆盖。
  globalThis.__kiana_contains_component = function containsComponent(root, target) {
    if (root === target) return true;
    if (root === null || typeof root !== "object") return false;
    const children = root.children;
    if (Array.isArray(children)) {
      for (const child of children) {
        if (globalThis.__kiana_contains_component(child, target)) return true;
      }
    }
    return false;
  };

  // `ReadonlyMap<string, string>` 的 JS 外观（`footer-data-provider.ts:134-137`）。原生对象
  // 每次调用都读实时状态表；这里补上 `Map` 的迭代协议与 `forEach`。
  globalThis.__kiana_make_status_map = function makeStatusMap(native) {
    const wrapper = {
      get size() {
        return native.size;
      },
      get(key) {
        return native.get(String(key));
      },
      has(key) {
        return native.has(String(key));
      },
      keys() {
        return native.keys()[Symbol.iterator]();
      },
      values() {
        return native.values()[Symbol.iterator]();
      },
      entries() {
        return native.entries()[Symbol.iterator]();
      },
      forEach(callback, thisArg) {
        if (typeof callback !== "function") {
          throw new TypeError("callback is not a function");
        }
        for (const [key, value] of native.entries()) {
          callback.call(thisArg, value, key, wrapper);
        }
      },
    };
    wrapper[Symbol.iterator] = () => wrapper.entries();
    return wrapper;
  };
})();
