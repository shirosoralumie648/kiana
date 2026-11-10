// Core prelude for the kiana extension runtime.
//
// Installed by `kiana_extensions::engine::install` right after `JsEngine::new()`:
// the `__kiana` promise protocol, Node-compatible timers, `AbortController`/
// `AbortSignal`, and the microtask glue (`setImmediate` / `process.nextTick`).
//
// Contract (design-extension-runtime.md §9.3):
//   - only depends on the `__kiana_host` object installed by Rust;
//   - imports no virtual module;
//   - business state (dispatch snapshots, registries) lives in api.js/dispatch.js.
//
// Everything is installed on `globalThis` because this file is evaluated as a
// global script (not a module), so the surrounding IIFE only keeps private
// helpers out of the global namespace.

(function () {
  "use strict";

  const host = globalThis.__kiana_host;

  // ---------------------------------------------------------------------
  // __kiana promise protocol (design §2.3)
  // ---------------------------------------------------------------------

  const kiana = {
    // Monotonic promise id; the Rust side receives it through hostStart.
    seq: 0,
    // id -> { resolve, reject }; Rust settles promises through settle().
    waiters: new Map(),
    // Injected by Rust (`__kiana_host.hostStart`).
    hostStart: host.hostStart,

    // `__kiana.promise(kind, payload)` is the only async entry from JS to the host.
    // Design §2.3: the Promise is created first; `hostStart` runs after it. An
    // unknown kind (or a payload parse failure) is therefore a SYNCHRONOUS throw
    // out of `__kiana.promise`, exactly as in the frozen protocol snippet; inside an
    // `async` function the throw still becomes a rejection at the call site.
    promise(kind, payload) {
      const id = ++this.seq;
      const promise = new Promise((resolve, reject) => this.waiters.set(id, { resolve, reject }));
      try {
        this.hostStart(id, kind, payload);
      } catch (error) {
        // Rust deletes its pending entry before throwing. Delete the JS waiter
        // too so a failed hostStart never leaks an id.
        this.waiters.delete(id);
        throw error;
      }
      return promise;
    },

    // Called by Rust from the main loop after a host task completed.
    settle(id, ok, value) {
      const waiter = this.waiters.get(id);
      if (waiter === undefined) return false;
      this.waiters.delete(id);
      if (ok) waiter.resolve(value);
      else waiter.reject(value);
      return true;
    },

    // report a caught JS error to the host error queue (`JsEngine::take_errors`).
    recordError(kind, value) {
      try {
        host.recordError(kind, value);
      } catch (_) {
        // The host object is always installed; never let bookkeeping throw
        // over the original error.
      }
    },

    // TS error sinks use `error instanceof Error ? error.message : String(error)`
    // (ca-extensions.md §2.8.4 / §3.8). Keep the conversion in JS so number and
    // object stringification stay byte-identical to the engine. The stack gets
    // the V8 header (`Name: message`) that `stack.split("\n").slice(1)` relies on;
    // QuickJS only stores frames.
    errorText(value) {
      try {
        if (value instanceof Error) {
          const name = String(value.name);
          const message = String(value.message);
          let stack = typeof value.stack === "string" ? value.stack : "";
          if (stack !== "") {
            const newline = stack.indexOf("\n");
            const first = newline === -1 ? stack : stack.slice(0, newline);
            const header = message === "" ? name : name + ": " + message;
            if (first !== header) stack = header + "\n" + stack;
          }
          return { name: name, message: message, stack: stack };
        }
        return { name: "Error", message: String(value), stack: "" };
      } catch (_) {
        return { name: "Error", message: "Failed to format error", stack: "" };
      }
    },

    // Build the error class Node uses for host rejections (`HostValue::Error`).
    // Node's abort errors are DOMExceptions with `code` 20/23, and network
    // failures are TypeErrors; extension code branches on `instanceof`,
    // `error.code` and `Object.prototype.toString`, so a plain Error with a
    // rewritten `name` is not enough.
    makeError(name, message) {
      const errorName = typeof name === "string" && name !== "" ? name : "Error";
      const text = String(message);
      if (Object.prototype.hasOwnProperty.call(DOM_EXCEPTION_CODES, errorName)) {
        return new DOMException(text, errorName);
      }
      const standard = Object.prototype.hasOwnProperty.call(
        STANDARD_ERROR_CONSTRUCTORS,
        errorName
      )
        ? STANDARD_ERROR_CONSTRUCTORS[errorName]
        : undefined;
      if (typeof standard === "function") {
        return new standard(text);
      }
      const error = new Error(text);
      if (errorName !== "Error") error.name = errorName;
      return error;
    },

    // `process.nextTick` implementation; loader-host wires it onto `process`.
    // Node turns a throwing nextTick into an uncaughtException (the process
    // exits). We cannot unwind like that here, so the error is recorded instead
    // of being swallowed by `execute_pending_job` — mirroring `setImmediate`.
    nextTick(callback, ...args) {
      validateCallback(callback);
      queueMicrotask(() => {
        try {
          callback(...args);
        } catch (error) {
          kiana.recordError("timer", error);
        }
      });
    },
  };

  Object.defineProperty(globalThis, "__kiana", {
    value: kiana,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Node-style argument errors (timers + AbortSignal validation)
  // ---------------------------------------------------------------------
  //
  // Node sets a stable `code` on argument errors (`ERR_INVALID_ARG_TYPE`,
  // `ERR_OUT_OF_RANGE`, `ERR_ILLEGAL_CONSTRUCTOR`) and formats the offending
  // value after `Received `; extension/libraries branch on both.

  const STANDARD_ERROR_CONSTRUCTORS = {
    Error: Error,
    EvalError: EvalError,
    RangeError: RangeError,
    ReferenceError: ReferenceError,
    SyntaxError: SyntaxError,
    TypeError: TypeError,
    URIError: URIError,
  };

  function makeNodeError(Constructor, message, code) {
    const error = new Constructor(message);
    if (code !== undefined) {
      Object.defineProperty(error, "code", {
        value: code,
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
    return error;
  }

  function typeError(message, code) {
    return makeNodeError(TypeError, message, code);
  }

  function rangeError(message, code) {
    return makeNodeError(RangeError, message, code);
  }

  // Node's `determineSpecificType` quoting for `Received type string (...)`
  // (probed on Node 26): a string containing a single quote is emitted with
  // JSON.stringify (double quotes, escaped control chars); otherwise it is
  // wrapped in single quotes verbatim.
  function inspectString(value) {
    if (value.indexOf("'") !== -1) return JSON.stringify(value);
    return "'" + value + "'";
  }

  // The `Received ...` suffix of Node's ERR_INVALID_ARG_TYPE message.
  function describeReceived(value) {
    if (value === null) return "null";
    if (value === undefined) return "undefined";
    const type = typeof value;
    if (type === "string") return "type string (" + inspectString(value) + ")";
    if (type === "number") {
      return "type number (" + (Object.is(value, -0) ? "-0" : String(value)) + ")";
    }
    if (type === "boolean") return "type boolean (" + String(value) + ")";
    if (type === "symbol") return "type symbol (" + String(value) + ")";
    if (type === "bigint") return "type bigint (" + String(value) + "n)";
    if (type === "function") return "function " + (value.name || "(anonymous)");
    if (Object.getPrototypeOf(value) === null) return "[Object: null prototype] {}";
    if (Array.isArray(value)) return "an instance of Array";
    const constructor = value.constructor;
    const name = constructor && constructor.name ? constructor.name : "Object";
    return "an instance of " + name;
  }

  // node:internal/timers + node:internal/validators
  function validateCallback(callback) {
    if (typeof callback !== "function") {
      throw typeError(
        'The "callback" argument must be of type function. Received ' +
          describeReceived(callback),
        "ERR_INVALID_ARG_TYPE"
      );
    }
  }

  // ---------------------------------------------------------------------
  // Timers (design §2.5)
  // ---------------------------------------------------------------------
  //
  // JS owns the callbacks; Rust owns the min-heap of deadlines. `setTimeout`
  // MUST actually wait: `@anthropic-ai/sdk`'s fetchWithTimeout uses
  // `setTimeout(() => controller.abort(), ms)`, and firing immediately aborts
  // every request (design §2.5 / §10.4).

  const TIMEOUT_MAX = 2 ** 31 - 1;
  // `AbortSignal.timeout` validates an [EnforceRange] unsigned long long
  // (0..2^32-1); values above setTimeout's 2^31-1 then overflow to 1 ms, which
  // matches Node (it warns and clamps inside setTimeout).
  const DELAY_RANGE_MAX = 4294967295;
  const timers = new Map();
  let timerSeq = 0;

  // Node's `getTimerDuration`: non-numbers / out-of-range values become 1 ms,
  // but in-range fractions are kept (`setTimeout(fn, 1.9)._idleTimeout === 1.9`;
  // the Rust heap schedules with the same f64).
  function normalizeDelay(value) {
    const delay = Number(value);
    if (!(delay >= 1 && delay <= TIMEOUT_MAX)) return 1;
    return delay;
  }

  // Node returns a `Timeout` object, not a number. `Symbol.toPrimitive` keeps
  // `clearTimeout(numberOrObject)`, comparisons and arithmetic on ids working;
  // unref/ref/hasRef/refresh/close make timer-handle code from libraries run.
  class Timeout {
    constructor(id, delay, repeat, callback, args) {
      this._id = id;
      this._delay = delay;
      // Node's public-ish timer fields; libraries read `_idleTimeout`.
      this._idleTimeout = delay;
      this._repeat = repeat;
      this._callback = callback;
      this._args = args;
      this._ref = true;
      this._destroyed = false;
    }

    ref() {
      this._ref = true;
      return this;
    }

    unref() {
      // The engine never exits because the loop is empty (the TUI owns the
      // loop), so unref only affects hasRef(); the timer still fires.
      this._ref = false;
      return this;
    }

    hasRef() {
      return this._ref;
    }

    refresh() {
      // Node reactivates a timer whose callback already ran; it does nothing
      // after clearTimeout/clearInterval (the handle is marked destroyed).
      // Like Node, the delay is recomputed from `_idleTimeout`, so code that
      // mutates the field sees the new delay.
      if (this._destroyed) return this;
      const delay = normalizeDelay(this._idleTimeout);
      this._delay = delay;
      this._idleTimeout = delay;
      timers.set(this._id, {
        fn: this._callback,
        args: this._args,
        repeat: this._repeat,
        delay,
        timeout: this,
      });
      host.timerStart(this._id, delay, this._repeat);
      return this;
    }

    close() {
      clearTimer(this._id);
    }

    [Symbol.toPrimitive]() {
      return this._id;
    }
  }

  function addTimer(callback, delay, args, repeat) {
    validateCallback(callback);
    const id = ++timerSeq;
    const normalized = normalizeDelay(delay);
    const timeout = new Timeout(id, normalized, repeat, callback, args);
    timers.set(id, {
      fn: callback,
      args,
      repeat,
      delay: normalized,
      timeout,
    });
    host.timerStart(id, normalized, repeat);
    return timeout;
  }

  function clearTimer(id) {
    // Accept Timeout objects (current API) and plain numbers (legacy/our tests).
    // Mark the handle first: Node's clearTimeout marks even an already-fired
    // Timeout as destroyed, so a later `refresh()` must not reactivate it.
    if (id instanceof Timeout) id._destroyed = true;
    let key = id;
    if (typeof key !== "number") {
      key = Number(key);
    }
    if (!Number.isFinite(key)) return;
    const timer = timers.get(key);
    if (timer !== undefined) timer.timeout._destroyed = true;
    timers.delete(key);
    host.timerCancel(key);
  }

  globalThis.setTimeout = function setTimeout(callback, delay, ...args) {
    return addTimer(callback, delay, args, false);
  };

  globalThis.setInterval = function setInterval(callback, delay, ...args) {
    return addTimer(callback, delay, args, true);
  };

  globalThis.clearTimeout = function clearTimeout(id) {
    clearTimer(id);
  };

  globalThis.clearInterval = function clearInterval(id) {
    clearTimer(id);
  };

  // `setImmediate`/`process.nextTick` map onto queueMicrotask (design §2.5);
  // the TUI scheduler uses a Rust queue and never goes through JS.
  //
  // Node returns an `Immediate` handle (`ref`/`unref`/`hasRef`), not a number:
  // `setImmediate(fn).unref()` is a common library pattern. Like Node,
  // `Number(immediate)` is NaN (no Symbol.toPrimitive), so `clearImmediate`
  // detects the handle instead of coercing it.
  class Immediate {
    constructor(id) {
      this._id = id;
      this._ref = true;
      this._destroyed = false;
    }

    ref() {
      this._ref = true;
      return this;
    }

    unref() {
      this._ref = false;
      return this;
    }

    hasRef() {
      return this._ref;
    }
  }

  const immediates = new Map();

  globalThis.setImmediate = function setImmediate(callback, ...args) {
    validateCallback(callback);
    const id = ++timerSeq;
    const immediate = new Immediate(id);
    immediates.set(id, immediate);
    queueMicrotask(() => {
      const handle = immediates.get(id);
      if (handle === undefined) return;
      immediates.delete(id);
      try {
        callback.apply(handle, args);
      } catch (error) {
        kiana.recordError("timer", error);
      }
    });
    return immediate;
  };

  globalThis.clearImmediate = function clearImmediate(id) {
    let key = id;
    if (key instanceof Immediate) {
      key._destroyed = true;
      key = key._id;
    } else if (typeof key !== "number") {
      key = Number(key);
    }
    if (!Number.isFinite(key)) return;
    const handle = immediates.get(key);
    if (handle !== undefined) handle._destroyed = true;
    immediates.delete(key);
  };

  // Driven by `JsEngine::fire_due_timers`. Returns whether the timer must be
  // re-armed (repeat intervals only). A `clearInterval` inside the callback
  // removes the id, so the interval stops.
  kiana.timerFire = function timerFire(id) {
    const timer = timers.get(id);
    if (timer === undefined) return false;
    if (!timer.repeat) timers.delete(id);
    try {
      // Node calls timer callbacks with `this === timeoutHandle`.
      timer.fn.apply(timer.timeout, timer.args);
    } catch (error) {
      kiana.recordError("timer", error);
    }
    return timer.repeat && timers.has(id);
  };

  // ---------------------------------------------------------------------
  // AbortController / AbortSignal (design §2.6)
  // ---------------------------------------------------------------------
  //
  // QuickJS has no DOMException/EventTarget; this is the ~80 line shim the
  // design asks for. A listener registered after abort MUST fire in a
  // microtask (design §2.6: the first /tmp/stream_probe version hung without
  // this).

  // QuickJS has no DOMException. Node's abort/timeout reasons are DOMExceptions
  // with a numeric `.code` (AbortError=20, TimeoutError=23), name/message that
  // match the old Error-based shim, and `[object DOMException]`.
  const DOM_EXCEPTION_CODES = {
    IndexSizeError: 1,
    HierarchyRequestError: 3,
    WrongDocumentError: 4,
    InvalidCharacterError: 5,
    NoModificationAllowedError: 7,
    NotFoundError: 8,
    NotSupportedError: 9,
    InUseAttributeError: 10,
    InvalidStateError: 11,
    SyntaxError: 12,
    InvalidModificationError: 13,
    NamespaceError: 14,
    InvalidAccessError: 15,
    TypeMismatchError: 17,
    SecurityError: 18,
    NetworkError: 19,
    AbortError: 20,
    URLMismatchError: 21,
    QuotaExceededError: 22,
    TimeoutError: 23,
    InvalidNodeTypeError: 24,
    DataCloneError: 25,
  };

  class DOMException extends Error {
    constructor(message = "", name = "Error") {
      super(String(message));
      Object.defineProperty(this, "_name", {
        value: String(name),
        writable: true,
        enumerable: false,
        configurable: true,
      });
      Object.defineProperty(this, "_code", {
        // hasOwnProperty: `__proto__`/`constructor` must fall back to 0, not to
        // Object.prototype's values.
        value: Object.prototype.hasOwnProperty.call(DOM_EXCEPTION_CODES, name)
          ? DOM_EXCEPTION_CODES[name]
          : 0,
        writable: true,
        enumerable: false,
        configurable: true,
      });
    }

    get name() {
      return this._name;
    }

    get code() {
      return this._code;
    }
  }
  Object.defineProperty(DOMException.prototype, Symbol.toStringTag, {
    value: "DOMException",
    configurable: true,
  });
  Object.defineProperty(globalThis, "DOMException", {
    value: DOMException,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  function abortError(message) {
    return new DOMException(message, "AbortError");
  }

  function timeoutError(message) {
    return new DOMException(message, "TimeoutError");
  }

  // Node's `validateTimeoutDelay`: the exact Type/Range error wording of
  // AbortSignal.timeout (node:internal/validators + webidl convert), including
  // the `code` values libraries branch on.
  function validateTimeoutDelay(value) {
    if (typeof value !== "number") {
      throw typeError(
        'The "delay" argument must be of type number. Received ' + describeReceived(value),
        "ERR_INVALID_ARG_TYPE"
      );
    }
    if (!Number.isInteger(value)) {
      throw rangeError(
        'The value of "delay" is out of range. It must be an integer. Received ' + String(value),
        "ERR_OUT_OF_RANGE"
      );
    }
    if (!(value >= 0 && value <= DELAY_RANGE_MAX)) {
      throw rangeError(
        'The value of "delay" is out of range. It must be >= 0 && <= 4294967295. Received ' +
          String(value),
        "ERR_OUT_OF_RANGE"
      );
    }
  }

  // `new AbortSignal()` is illegal in Node/WHATWG (`ERR_ILLEGAL_CONSTRUCTOR`);
  // internal construction goes through this token.
  const SIGNAL_TOKEN = Symbol("AbortSignal.internal");

  function createSignal() {
    return new AbortSignal(SIGNAL_TOKEN);
  }

  function makeAbortEvent(signal) {
    return { type: "abort", target: signal, currentTarget: signal };
  }

  class AbortSignal {
    constructor(token) {
      if (token !== SIGNAL_TOKEN) {
        throw typeError("Illegal constructor", "ERR_ILLEGAL_CONSTRUCTOR");
      }
      this._aborted = false;
      this._reason = undefined;
      this._listeners = [];
      // Listeners registered after abort are queued as microtasks (design §2.6);
      // this set dedupes identical registrations until they run.
      this._lateListeners = new Set();
      // EventTarget's `onabort` attribute (node:signal.onabort is supported).
      this._onabort = null;
      this._onabortListener = null;
    }

    get aborted() {
      return this._aborted;
    }

    get reason() {
      return this._reason;
    }

    throwIfAborted() {
      if (this._aborted) throw this._reason;
    }

    addEventListener(type, listener) {
      if (type !== "abort" || listener === null || listener === undefined) return;
      if (typeof listener !== "function" && typeof listener.handleEvent !== "function") {
        return;
      }
      if (this._aborted) {
        if (this._lateListeners.has(listener)) return;
        this._lateListeners.add(listener);
        queueMicrotask(() => {
          // A removeEventListener before the microtask cancels the delivery.
          if (!this._lateListeners.delete(listener)) return;
          this._dispatch(listener);
        });
        return;
      }
      // EventTarget dedupes identical (type, callback) registrations.
      if (this._listeners.indexOf(listener) >= 0) return;
      this._listeners.push(listener);
    }

    removeEventListener(type, listener) {
      if (type !== "abort") return;
      this._lateListeners.delete(listener);
      const index = this._listeners.indexOf(listener);
      if (index >= 0) this._listeners.splice(index, 1);
    }

    dispatchEvent(event) {
      if (event === null || typeof event !== "object" || typeof event.type !== "string") {
        throw new TypeError('The "event" argument must be an object');
      }
      if (event.type !== "abort") return true;
      for (const listener of this._listeners.slice()) this._notify(listener, event);
      return true;
    }

    _notify(listener, event) {
      try {
        if (typeof listener === "function") listener.call(this, event);
        else listener.handleEvent(event);
      } catch (error) {
        kiana.recordError("host", error);
      }
    }

    _dispatch(listener) {
      this._notify(listener, makeAbortEvent(this));
    }

    _abort(reason) {
      if (this._aborted) return;
      this._aborted = true;
      this._reason = reason;
      const listeners = this._listeners;
      this._listeners = [];
      for (const listener of listeners) this._dispatch(listener);
    }

    static abort(reason) {
      const signal = createSignal();
      signal._abort(reason === undefined ? abortError("This operation was aborted") : reason);
      return signal;
    }

    static timeout(milliseconds) {
      validateTimeoutDelay(milliseconds);
      const signal = createSignal();
      const delay = Math.trunc(milliseconds);
      const id = setTimeout(() => {
        signal._abort(timeoutError("The operation was aborted due to timeout"));
      }, delay);
      signal.addEventListener("abort", () => clearTimeout(id));
      return signal;
    }

    static any(signals) {
      // WebIDL: `signals` must be an object convertible to a sequence; every
      // element must be an AbortSignal. Node throws before combining, so the
      // whole list is validated before any abort state is inspected.
      const isObject =
        signals !== null && (typeof signals === "object" || typeof signals === "function");
      if (!isObject || typeof signals[Symbol.iterator] !== "function") {
        throw typeError("signals cannot be converted to sequence.", "ERR_INVALID_ARG_TYPE");
      }
      const list = Array.from(signals);
      for (let index = 0; index < list.length; index += 1) {
        if (!(list[index] instanceof AbortSignal)) {
          throw typeError(
            "signals[" + index + "] is not of type AbortSignal.",
            "ERR_INVALID_ARG_TYPE"
          );
        }
      }
      const signal = createSignal();
      const sources = [];
      const listeners = [];
      const cleanup = () => {
        for (let index = 0; index < sources.length; index += 1) {
          sources[index].removeEventListener("abort", listeners[index]);
        }
        sources.length = 0;
        listeners.length = 0;
      };
      for (const source of list) {
        if (source.aborted) {
          cleanup();
          signal._abort(source.reason);
          return signal;
        }
        const listener = () => {
          if (!signal.aborted) signal._abort(source.reason);
        };
        sources.push(source);
        listeners.push(listener);
        source.addEventListener("abort", listener);
      }
      if (sources.length > 0) signal.addEventListener("abort", cleanup);
      return signal;
    }
  }

  // EventTarget's `onabort` attribute. Node defines it on the prototype as an
  // enumerable getter/setter; assigning replaces the previous handler, null
  // removes it, and (like addEventListener) a handler assigned after abort
  // fires in a microtask.
  function getOnAbort() {
    return typeof this._onabort === "function" ? this._onabort : null;
  }

  function setOnAbort(handler) {
    if (this._onabortListener !== null) {
      this.removeEventListener("abort", this._onabortListener);
      this._onabortListener = null;
    }
    this._onabort = typeof handler === "function" ? handler : null;
    if (this._onabort !== null) {
      const signal = this;
      const listener = (event) => {
        signal._onabort.call(signal, event);
      };
      this._onabortListener = listener;
      this.addEventListener("abort", listener);
    }
  }

  Object.defineProperty(AbortSignal.prototype, "onabort", {
    get: getOnAbort,
    set: setOnAbort,
    enumerable: true,
    configurable: true,
  });

  class AbortController {
    constructor() {
      this._signal = createSignal();
    }

    get signal() {
      return this._signal;
    }

    abort(reason) {
      this._signal._abort(reason === undefined ? abortError("This operation was aborted") : reason);
    }
  }

  Object.defineProperty(AbortSignal.prototype, Symbol.toStringTag, {
    value: "AbortSignal",
    configurable: true,
  });
  Object.defineProperty(AbortController.prototype, Symbol.toStringTag, {
    value: "AbortController",
    configurable: true,
  });

  // WebIDL prototype attributes are enumerable in Node (`aborted`, `signal`);
  // class accessors are not. `reason` stays non-enumerable like Node.
  function makeEnumerable(target, name) {
    const descriptor = Object.getOwnPropertyDescriptor(target, name);
    if (descriptor === undefined) return;
    descriptor.enumerable = true;
    Object.defineProperty(target, name, descriptor);
  }
  makeEnumerable(AbortSignal.prototype, "aborted");
  makeEnumerable(AbortController.prototype, "signal");

  // Node installs both as non-enumerable globals (design §9.3); plain
  // assignment would make them show up in `Object.keys(globalThis)`.
  Object.defineProperty(globalThis, "AbortController", {
    value: AbortController,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "AbortSignal", {
    value: AbortSignal,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Microtask glue
  // ---------------------------------------------------------------------
  //
  // QuickJS provides `queueMicrotask` natively (design §2.5), but
  // `Ctx::execute_pending_job` swallows exceptions thrown by jobs and the
  // rejection tracker only sees promise rejections, so a throwing microtask
  // would vanish. Node raises uncaughtException; the engine's equivalent is
  // the error queue (same as setImmediate/process.nextTick). The wrapper also
  // reproduces Node's ERR_INVALID_ARG_TYPE validation and keeps the native
  // microtask ordering.
  const nativeQueueMicrotask =
    typeof globalThis.queueMicrotask === "function"
      ? globalThis.queueMicrotask
      : (callback) => {
          Promise.resolve().then(callback);
        };

  function kianaQueueMicrotask(callback) {
    validateCallback(callback);
    const job = () => {
      try {
        callback();
      } catch (error) {
        kiana.recordError("timer", error);
      }
    };
    nativeQueueMicrotask(job);
  }

  Object.defineProperty(globalThis, "queueMicrotask", {
    value: kianaQueueMicrotask,
    writable: true,
    enumerable: true,
    configurable: true,
  });

  // If a `process` object already exists (it is installed later by the node
  // globals unit), wire `nextTick` now; loader-host does the same on install.
  if (globalThis.process !== undefined && globalThis.process !== null) {
    try {
      globalThis.process.nextTick = kiana.nextTick;
    } catch (_) {
      // `process` may be frozen; loader-host retries during its own install.
    }
  }

  // Keep the host bridge out of enumeration/debug output (design §9.3).
  Object.defineProperty(globalThis, "__kiana_host", {
    value: host,
    writable: true,
    enumerable: false,
    configurable: true,
  });
})();
