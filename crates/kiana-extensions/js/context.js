// Runtime glue for the native `ExtensionContext` class
// (crates/kiana-extensions/src/runner/context.rs).
//
// Installed by `kiana_extensions::runner::context::install()` (design §9.2 step 8) after the
// core prelude. This file only depends on `__kiana` (the promise protocol) and the primitives
// installed by Rust (`__kiana_ctx_signal_aborted`, `__kiana_ctx_callback_done`,
// `__kiana_ctx_make_replaced`); it imports no virtual module.
//
// Responsibilities:
//   - `__kiana_ctx.invoke(handle, op, args)` → host promise for async context members
//     (executeTool, waitForIdle, newSession, fork, navigateTree, switchSession, reload,
//     sendMessage, sendUserMessage, model registry async members);
//   - `__kiana_ctx.rejected(message)` → a rejected promise (stale ctx in async members, which
//     mirrors TS: an `async` method never throws synchronously);
//   - `__kiana_ctx.signal(id)` → the host signal as a real `AbortSignal` (an object whose
//     prototype is `AbortSignal.prototype`, so `instanceof` and `AbortSignal.any()` work);
//   - `__kiana_ctx.noopUi()` → the JS equivalent of `noOpUIContext` (print/json, and the
//     fallback when the ui-adapter bridge is not installed);
//   - `__kiana_ctx.makeUi(handle)` → delegates to `globalThis.__kiana_ctx_make_ui(handle)`, the
//     hook that `ui-adapter` (ui/mod.rs) installs; returns undefined when absent;
//   - `__kiana_ctx.makeContext(native, kind)` → the JS-visible context object. TS
//     `createContext()` returns an object literal whose members are **own enumerable**
//     properties, and the four shapes only carry their own members (`ctx.tools` is
//     `undefined` on a base context, `ctx.newSession` on a tool context, …). The native
//     class keeps the machinery; this facade reproduces the literal's shape: own
//     enumerable getters/methods, no members from other kinds, `Object.keys`/spread work.
//     A tool context adds `tools`/`executeTool` with the non-enumerable, non-configurable
//     descriptors TS gets from `Object.defineProperties`.
//   - `__kiana_ctx.eventStream(openPromise)` → `AssistantMessageEventStream` for
//     `ctx.modelRegistry.stream`/`streamSimple` (`for await` + `result()`);
//   - `__kiana_ctx.registerCallback/applyCallback/releaseCallback` → host ↔ JS callback
//     bridge. Rust stores only numeric ids across the boundary; the functions live here.
//     `withSession`/`setup` arguments are hydrated from `__kianaCtxRef` markers
//     (`__kiana_ctx_make_replaced` / `__kiana_ctx_make_session_manager`).

(function () {
  "use strict";

  const kiana = globalThis.__kiana;

  // ---------------------------------------------------------------------
  // Host bridge
  // ---------------------------------------------------------------------

  function invoke(handle, op, args) {
    return kiana.promise("kiana_ctx", { handle: handle, op: op, args: args === undefined ? null : args });
  }

  function rejected(message) {
    return Promise.reject(new Error(String(message)));
  }

  function makeUi(handle) {
    const factory = globalThis.__kiana_ctx_make_ui;
    if (typeof factory !== "function") return undefined;
    return factory(handle);
  }

  function makeReplaced(handle) {
    const factory = globalThis.__kiana_ctx_make_replaced;
    if (typeof factory !== "function") return undefined;
    return factory(handle);
  }

  // ---------------------------------------------------------------------
  // AbortSignal over a Rust CancellationToken handle
  // ---------------------------------------------------------------------
  //
  // The returned object's prototype is the engine's `AbortSignal.prototype`, so
  // `instanceof AbortSignal` is true and `AbortSignal.any([ctx.signal, …])` accepts it.
  // The actual abort is delivered through an internal `AbortController` (stable reason,
  // late listeners in a microtask, `once` / `{ signal }` options). Reads of `aborted` /
  // `reason` poll the Rust token synchronously; while an abort listener is registered a
  // coarse poll forwards the cancellation to the controller. The poll stops as soon as the
  // token aborts or the last abort listener is removed, so nothing stays alive afterwards.

  const signalObjects = new Map();

  function signal(handle) {
    const cached = signalObjects.get(handle);
    if (cached !== undefined) return cached;

    const controller = new AbortController();
    let poll = null;
    let abortListeners = 0;

    function stopPoll() {
      if (poll !== null) {
        clearInterval(poll);
        poll = null;
      }
    }

    function check() {
      if (!__kiana_ctx_signal_aborted(handle)) return false;
      stopPoll();
      if (!controller.signal.aborted) controller.abort();
      return true;
    }

    function ensurePoll() {
      if (poll !== null || controller.signal.aborted || abortListeners === 0) return;
      if (check()) return;
      poll = setInterval(check, 16);
    }

    const facade = Object.create(AbortSignal.prototype);
    Object.defineProperties(facade, {
      aborted: {
        get() {
          check();
          return controller.signal.aborted;
        },
        configurable: true,
      },
      reason: {
        get() {
          check();
          return controller.signal.reason;
        },
        configurable: true,
      },
      addEventListener: {
        value: function addEventListener(type, listener, options) {
          controller.signal.addEventListener(type, listener, options);
          if (type === "abort") {
            abortListeners += 1;
            if (!controller.signal.aborted) ensurePoll();
          }
        },
        writable: true,
        configurable: true,
      },
      removeEventListener: {
        value: function removeEventListener(type, listener, options) {
          controller.signal.removeEventListener(type, listener, options);
          if (type === "abort") {
            abortListeners = Math.max(0, abortListeners - 1);
            if (abortListeners === 0) stopPoll();
          }
        },
        writable: true,
        configurable: true,
      },
      dispatchEvent: {
        value: function dispatchEvent(event) {
          return controller.signal.dispatchEvent(event);
        },
        writable: true,
        configurable: true,
      },
      throwIfAborted: {
        value: function throwIfAborted() {
          check();
          return controller.signal.throwIfAborted();
        },
        writable: true,
        configurable: true,
      },
    });

    check();
    signalObjects.set(handle, facade);
    return facade;
  }

  // ---------------------------------------------------------------------
  // Host → JS callback bridge
  // ---------------------------------------------------------------------
  //
  // Rust keeps the callback as a numeric id (functions cannot cross the JSON payload).
  // The host invokes `applyCallback(id, argsJson, callId, ctxHandle)` through the engine's
  // completion queue (on the JS thread). The callback may return a promise; when it settles
  // `__kiana_ctx_callback_done(callId, ok, payload)` resolves the awaiting Rust future.

  const callbacks = new Map();
  let nextCallbackId = 1;

  function registerCallback(fn) {
    if (typeof fn !== "function") return 0;
    const id = nextCallbackId++;
    callbacks.set(id, fn);
    return id;
  }

  function releaseCallback(id) {
    callbacks.delete(id);
  }

  function errorMessage(error) {
    return error !== null && error !== undefined && error.message !== undefined
      ? String(error.message)
      : String(error);
  }

  function hydrateArg(value, ctxHandle) {
    if (Array.isArray(value)) {
      for (let index = 0; index < value.length; index += 1) {
        value[index] = hydrateArg(value[index], ctxHandle);
      }
      return value;
    }
    if (value === null || typeof value !== "object") return value;
    if (value.__kianaCtxRef === "replaced") {
      return ctxHandle > 0 ? makeReplaced(ctxHandle) : undefined;
    }
    if (value.__kianaCtxRef === "setupSessionManager") {
      const make = globalThis.__kiana_ctx_make_session_manager;
      return typeof make === "function" ? make(value.handle) : undefined;
    }
    if (typeof value.__kianaError === "string") {
      return new Error(value.__kianaError);
    }
    for (const key of Object.keys(value)) {
      value[key] = hydrateArg(value[key], ctxHandle);
    }
    return value;
  }

  function applyCallback(callbackId, rawArgs, callId, ctxHandle, recordError) {
    const fn = callbacks.get(callbackId);
    if (fn === undefined) {
      __kiana_ctx_callback_done(callId, false, "callback is no longer available");
      return;
    }
    let args;
    try {
      args = hydrateArg(rawArgs === undefined || rawArgs === null ? [] : rawArgs, ctxHandle || 0);
    } catch (error) {
      __kiana_ctx_callback_done(callId, false, errorMessage(error));
      return;
    }
    let result;
    try {
      result = fn.apply(undefined, args);
    } catch (error) {
      // `withSession`/`setup` 的拒绝会转成命令 Promise 的 rejection，不再重复记错误。
      if (recordError !== false) kiana.recordError("host", error);
      __kiana_ctx_callback_done(callId, false, errorMessage(error));
      return;
    }
    Promise.resolve(result).then(
      (value) => {
        let json;
        try {
          json = JSON.stringify(value === undefined ? null : value);
        } catch (error) {
          __kiana_ctx_callback_done(callId, false, errorMessage(error));
          return;
        }
        __kiana_ctx_callback_done(callId, true, json === undefined ? "null" : json);
      },
      (error) => {
        if (recordError !== false) kiana.recordError("host", error);
        __kiana_ctx_callback_done(callId, false, errorMessage(error));
      }
    );
  }

  // ---------------------------------------------------------------------
  // Value interning
  // ---------------------------------------------------------------------
  //
  // TS contexts hand out host-owned / session-owned values (`scopedModels`, `model`,
  // `getSystemPromptOptions()`), so repeated reads return the same object and extension
  // mutations stay visible. The Rust host serializes a snapshot per read; interning by
  // JSON keeps reference identity while the snapshot is unchanged and refreshes when the
  // host state changes (read-time evaluation).

  const internCache = new Map();
  const INTERN_LIMIT = 256;

  function internValue(value) {
    if (value === undefined) return undefined;
    let json;
    try {
      json = JSON.stringify(value);
    } catch {
      return value;
    }
    if (json === undefined) return value;
    const cached = internCache.get(json);
    if (cached !== undefined) return cached;
    if (internCache.size >= INTERN_LIMIT) {
      const oldest = internCache.keys().next();
      if (!oldest.done) internCache.delete(oldest.value);
    }
    const parsed = JSON.parse(json);
    internCache.set(json, parsed);
    return parsed;
  }

  // ---------------------------------------------------------------------
  // Context facade (own enumerable members, kind-specific shapes)
  // ---------------------------------------------------------------------

  const BASE_MEMBERS = [
    "ui",
    "mode",
    "hasUI",
    "cwd",
    "sessionManager",
    "modelRegistry",
    "model",
    "scopedModels",
    "thinkingLevel",
    "isIdle",
    "isProjectTrusted",
    "signal",
    "abort",
    "hasPendingMessages",
    "shutdown",
    "getContextUsage",
    "compact",
    "getSystemPrompt",
  ];
  const COMMAND_MEMBERS = [
    "getSystemPromptOptions",
    "waitForIdle",
    "newSession",
    "fork",
    "navigateTree",
    "switchSession",
    "reload",
  ];
  const REPLACED_MEMBERS = ["sendMessage", "sendUserMessage"];
  const GETTER_MEMBERS = new Set([
    "ui",
    "mode",
    "hasUI",
    "cwd",
    "sessionManager",
    "modelRegistry",
    "model",
    "scopedModels",
    "thinkingLevel",
    "signal",
    "tools",
  ]);

  // Members whose snapshot is interned so repeated reads keep reference identity.
  const INTERNED_GETTERS = new Set(["model", "scopedModels"]);
  // Host instances that do not change for the lifetime of a context (runner fields).
  // The cached object is still revalidated on every read: TS wraps every context member in
  // a getter that calls `runner.assertActive()` first, so a captured ctx must throw the
  // verbatim stale Error even when the member was read before the invalidation (#2860).
  const CACHED_GETTERS = new Set(["sessionManager", "modelRegistry"]);
  // Methods that return host-owned objects which TS returns by reference.
  const INTERNED_METHODS = new Set(["getSystemPromptOptions"]);

  // TS `createToolContext` uses `Object.defineProperties` on top of the base context:
  // `tools` is a non-enumerable non-configurable getter and `executeTool` a
  // non-enumerable non-configurable non-writable value. `Object.keys`/spread therefore
  // show the same 18 members as a plain context.
  const TOOL_HIDDEN_MEMBERS = new Set(["tools", "executeTool"]);

  function makeContext(native, kind) {
    const descriptors = {};
    const cached = Object.create(null);

    function addMember(name) {
      if (GETTER_MEMBERS.has(name)) {
        if (CACHED_GETTERS.has(name)) {
          descriptors[name] = {
            get() {
              native.assertActive();
              if (!(name in cached)) cached[name] = native[name];
              return cached[name];
            },
            enumerable: true,
            configurable: true,
          };
          return;
        }
        if (INTERNED_GETTERS.has(name)) {
          descriptors[name] = {
            get() {
              return internValue(native[name]);
            },
            enumerable: true,
            configurable: true,
          };
          return;
        }
        descriptors[name] = {
          get() {
            return native[name];
          },
          enumerable: true,
          configurable: true,
        };
        return;
      }
      if (INTERNED_METHODS.has(name)) {
        descriptors[name] = {
          value: function (...args) {
            return internValue(native[name](...args));
          },
          enumerable: true,
          writable: true,
          configurable: true,
        };
        return;
      }
      if (TOOL_HIDDEN_MEMBERS.has(name)) {
        descriptors[name] = {
          value: function (...args) {
            return native[name](...args);
          },
          enumerable: false,
          writable: false,
          configurable: false,
        };
        return;
      }
      descriptors[name] = {
        value: function (...args) {
          return native[name](...args);
        },
        enumerable: true,
        writable: true,
        configurable: true,
      };
    }

    for (const name of BASE_MEMBERS) addMember(name);
    if (kind === "tool") {
      // `tools` 是 getter（只读、不可配置、不可枚举）。
      descriptors.tools = {
        get() {
          return native.tools;
        },
        enumerable: false,
        configurable: false,
      };
      descriptors.executeTool = {
        value: function (...args) {
          return native.executeTool(...args);
        },
        enumerable: false,
        writable: false,
        configurable: false,
      };
    }
    if (kind === "command" || kind === "replaced") {
      for (const name of COMMAND_MEMBERS) addMember(name);
    }
    if (kind === "replaced") {
      for (const name of REPLACED_MEMBERS) addMember(name);
    }
    return Object.defineProperties({}, descriptors);
  }

  // ---------------------------------------------------------------------
  // noOpUIContext (runner.ts:331-362, verbatim defaults)
  // ---------------------------------------------------------------------

  const THEME_KEY = Symbol.for("@earendil-works/kiana-coding-agent:theme");
  const THEME_KEY_OLD = Symbol.for("@mariozechner/kiana-coding-agent:theme");

  // TS: `export const theme = new Proxy({} as Theme, { get() { const t = globalThis[KEY];
  // if (!t) throw new Error("Theme not initialized. Call initTheme() first."); return t[prop]; } })`.
  // The theme module registers the global instance under `THEME_KEY`; until then every
  // property access throws (and the object itself is truthy and stable).
  function makeThemeProxy() {
    return new Proxy(
      {},
      {
        get(_target, property) {
          const current =
            globalThis[THEME_KEY] !== undefined
              ? globalThis[THEME_KEY]
              : globalThis[THEME_KEY_OLD];
          if (!current) throw new Error("Theme not initialized. Call initTheme() first.");
          return current[property];
        },
      }
    );
  }

  const themeProxy = makeThemeProxy();

  const noopUi = {
    select: async () => undefined,
    confirm: async () => false,
    input: async () => undefined,
    notify: () => {},
    onTerminalInput: () => () => {},
    setStatus: () => {},
    setWorkingMessage: () => {},
    setWorkingVisible: () => {},
    setWorkingIndicator: () => {},
    setHiddenThinkingLabel: () => {},
    setWidget: () => {},
    setFooter: () => {},
    setHeader: () => {},
    setTitle: () => {},
    custom: async () => undefined,
    pasteToEditor: () => {},
    setEditorText: () => {},
    getEditorText: () => "",
    editor: async () => undefined,
    addAutocompleteProvider: () => {},
    setEditorComponent: () => {},
    getEditorComponent: () => undefined,
    // `get theme() { return theme; }` —— 位置与 TS 的对象字面量一致（getEditorComponent 与
    // getAllThemes 之间），`Object.keys` 顺序因此也一致。
    get theme() {
      return themeProxy;
    },
    getAllThemes: () => [],
    getTheme: () => undefined,
    setTheme: (_theme) => ({ success: false, error: "UI not available" }),
    getToolsExpanded: () => false,
    setToolsExpanded: () => {},
  };

  function noopUiContext() {
    return noopUi;
  }

  // ---------------------------------------------------------------------
  // AssistantMessageEventStream (ctx.modelRegistry.stream / streamSimple)
  // ---------------------------------------------------------------------
  //
  // The native `stream()`/`streamSimple()` return this object synchronously; `open` is the
  // host promise that resolves to a Rust stream id. Events are pulled one at a time
  // (`streamNext`) so `for await (...)` and `result()` match the TS class.

  class AssistantMessageEventStream {
    constructor(open) {
      this.__open = open;
      this.__id = undefined;
      this.__queue = [];
      this.__done = false;
      this.__resultPromise = undefined;
      this.__localResult = undefined;
      this.__closed = false;
    }

    async __streamId() {
      if (this.__id === undefined) this.__id = await this.__open;
      return this.__id;
    }

    push(event) {
      if (this.__done) return false;
      this.__queue.push(event);
      return true;
    }

    end(message) {
      this.__done = true;
      this.__localResult = message;
    }

    __rememberResult(event) {
      if (event.type === "done") this.__localResult = event.message;
      else if (event.type === "error") this.__localResult = event.error;
    }

    async __next() {
      if (this.__queue.length > 0) return { value: this.__queue.shift(), done: false };
      if (this.__done) {
        this.__maybeClose();
        return { value: undefined, done: true };
      }
      const id = await this.__streamId();
      const item = await invoke(0, "modelRegistry.streamNext", { stream: id });
      if (item.done) {
        this.__done = true;
        this.__maybeClose();
        return { value: undefined, done: true };
      }
      this.__rememberResult(item.value);
      return { value: item.value, done: false };
    }

    [Symbol.asyncIterator]() {
      return {
        next: () => this.__next(),
        return: () => {
          // `for await` 提前 break：消费者不再需要事件，释放 Rust 侧流。
          this.__close();
          return Promise.resolve({ value: undefined, done: true });
        },
      };
    }

    result() {
      if (this.__resultPromise === undefined) {
        this.__resultPromise = (async () => {
          if (this.__localResult !== undefined) return this.__localResult;
          const id = await this.__streamId();
          try {
            return await invoke(0, "modelRegistry.streamResult", { stream: id });
          } finally {
            this.__maybeClose();
          }
        })();
      }
      return this.__resultPromise;
    }

    __maybeClose() {
      if (!this.__done) return;
      this.__close();
    }

    __close() {
      if (this.__closed) return;
      this.__closed = true;
      if (this.__id !== undefined) {
        Promise.resolve(invoke(0, "modelRegistry.streamClose", { stream: this.__id })).catch(
          () => {}
        );
      }
    }
  }

  function eventStream(open) {
    return new AssistantMessageEventStream(open);
  }

  // ---------------------------------------------------------------------
  // Model registry helpers
  // ---------------------------------------------------------------------

  // TS `ModelsRefreshResult.errors` is a `ReadonlyMap<string, Error>`.
  function mapRefreshResult(result) {
    const errors = new Map();
    if (result && result.errors && typeof result.errors === "object") {
      for (const key of Object.keys(result.errors)) {
        errors.set(key, new Error(String(result.errors[key])));
      }
    }
    return { aborted: Boolean(result && result.aborted), errors };
  }

  function withRefreshMap(promise) {
    return promise.then(mapRefreshResult);
  }

  // Discard an internally created promise (TS `void` methods never expose one).
  function swallow(promise) {
    Promise.resolve(promise).catch(() => {});
  }

  const kianaCtx = {
    invoke: invoke,
    rejected: rejected,
    signal: signal,
    makeUi: makeUi,
    noopUi: noopUiContext,
    makeContext: makeContext,
    makeReplaced: makeReplaced,
    registerCallback: registerCallback,
    releaseCallback: releaseCallback,
    applyCallback: applyCallback,
    withRefreshMap: withRefreshMap,
    eventStream: eventStream,
    swallow: swallow,
  };

  Object.defineProperty(globalThis, "__kiana_ctx", {
    value: kianaCtx,
    writable: true,
    enumerable: false,
    configurable: true,
  });
})();
