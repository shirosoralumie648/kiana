// node:events — EventEmitter (design §6.1). The EventBus error prefix
// `Event handler error (<channel>):` lives in the runner unit; this file only
// provides the emitter semantics.
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  function invalidListener() {
    var error = new TypeError("The \"listener\" argument must be of type function. Received " + typeof arguments[0]);
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }

  class EventEmitter {
    constructor(options) {
      this._events = Object.create(null);
      this._maxListeners = undefined;
      this._eventCount = 0;
      if (options && typeof options.maxListeners === "number") {
        this._maxListeners = options.maxListeners;
      }
    }

    _list(event) {
      var list = this._events[event];
      if (list === undefined) {
        list = [];
        this._events[event] = list;
        this._eventCount++;
      }
      return list;
    }

    _emitNewListener(event, listener) {
      // Node 在真正挂上监听器**之前**发 'newListener'（因此它自己的监听器
      // 也能收到 'newListener'/'removeListener' 的注册）。
      var existing = this._events["newListener"];
      if (existing === undefined || existing.length === 0) return;
      var snapshot = existing.slice();
      for (var i = 0; i < snapshot.length; i++) {
        snapshot[i].fn.call(this, String(event), listener);
      }
    }

    _emitRemoveListener(event, listener) {
      var existing = this._events["removeListener"];
      if (existing === undefined || existing.length === 0) return;
      var snapshot = existing.slice();
      for (var i = 0; i < snapshot.length; i++) {
        snapshot[i].fn.call(this, String(event), listener);
      }
    }

    on(event, listener) {
      if (typeof listener !== "function") throw invalidListener(listener);
      this._emitNewListener(event, listener);
      this._list(String(event)).push({ fn: listener, once: false });
      return this;
    }
    addListener(event, listener) { return this.on(event, listener); }

    prependListener(event, listener) {
      if (typeof listener !== "function") throw invalidListener(listener);
      this._emitNewListener(event, listener);
      this._list(String(event)).unshift({ fn: listener, once: false });
      return this;
    }

    once(event, listener) {
      if (typeof listener !== "function") throw invalidListener(listener);
      this._emitNewListener(event, listener);
      this._list(String(event)).push({ fn: listener, once: true });
      return this;
    }

    prependOnceListener(event, listener) {
      if (typeof listener !== "function") throw invalidListener(listener);
      this._emitNewListener(event, listener);
      this._list(String(event)).unshift({ fn: listener, once: true });
      return this;
    }

    off(event, listener) {
      event = String(event);
      var list = this._events[event];
      if (!list) return this;
      for (var i = list.length - 1; i >= 0; i--) {
        if (list[i].fn === listener) {
          list.splice(i, 1);
          if (list.length === 0) {
            delete this._events[event];
            this._eventCount--;
          }
          this._emitRemoveListener(event, listener);
          return this;
        }
      }
      return this;
    }
    removeListener(event, listener) { return this.off(event, listener); }

    removeAllListeners(event) {
      if (event === undefined) {
        this._events = Object.create(null);
        this._eventCount = 0;
      } else {
        event = String(event);
        if (this._events[event] !== undefined) {
          delete this._events[event];
          this._eventCount--;
        }
      }
      return this;
    }

    emit(event, ...args) {
      event = String(event);
      var list = this._events[event];
      if ((list === undefined || list.length === 0) && event === "error") {
        var error = args[0];
        if (error instanceof Error) throw error;
        // Node: `Unhandled error. (<inspect(args)>)`，字符串带引号。
        var rendered = [];
        for (var a = 0; a < args.length; a++) rendered.push(I.inspectValue(args[a], 2, []));
        var thrown = new Error(
          "Unhandled error." + (args.length === 0 ? "" : " (" + rendered.join(", ") + ")"),
        );
        thrown.code = "ERR_UNHANDLED_ERROR";
        thrown.context = error;
        throw thrown;
      }
      if (list === undefined || list.length === 0) return false;
      var snapshot = list.slice();
      for (var i = 0; i < snapshot.length; i++) {
        var entry = snapshot[i];
        if (entry.once) {
          var current = this._events[event];
          if (current) {
            var index = current.indexOf(entry);
            if (index !== -1) current.splice(index, 1);
            if (current.length === 0) {
              delete this._events[event];
              this._eventCount--;
            }
          }
        }
        entry.fn.apply(this, args);
      }
      return true;
    }

    listeners(event) {
      var list = this._events[String(event)];
      return list ? list.map(function (entry) { return entry.fn; }) : [];
    }
    rawListeners(event) {
      var list = this._events[String(event)];
      return list ? list.slice() : [];
    }
    listenerCount(event, listener) {
      var list = this._events[String(event)];
      if (!list) return 0;
      if (listener === undefined) return list.length;
      return list.filter(function (entry) { return entry.fn === listener; }).length;
    }
    eventNames() {
      return Object.keys(this._events).filter(function (name) {
        return this._events[name] !== undefined;
      }, this);
    }
    setMaxListeners(count) { this._maxListeners = count; return this; }
    getMaxListeners() {
      return this._maxListeners === undefined
        ? (EventEmitter.defaultMaxListeners || 10)
        : this._maxListeners;
    }
  }

  EventEmitter.defaultMaxListeners = 10;
  EventEmitter.EventEmitter = EventEmitter;
  EventEmitter.errorMonitor = Symbol("events.errorMonitor");

  function once(emitter, name) {
    return new Promise(function (resolve, reject) {
      if (name !== "error") {
        emitter.once("error", reject);
      }
      emitter.once(name, function (...args) {
        if (name !== "error") emitter.removeListener("error", reject);
        resolve(args);
      });
    });
  }

  function on(emitter, name) {
    var queue = [];
    var waiters = [];
    var done = false;
    var error = null;
    emitter.on(name, function (...args) {
      // Node 的 events.on() 把一次 emit 的全部参数作为数组产出。
      if (waiters.length > 0) waiters.shift().resolve({ value: args, done: false });
      else queue.push(args);
    });
    emitter.on("error", function (failure) {
      error = failure;
      while (waiters.length > 0) waiters.shift().reject(failure);
    });
    emitter.on("close", function () {
      done = true;
      while (waiters.length > 0) waiters.shift().resolve({ value: undefined, done: true });
    });
    return {
      next: function () {
        if (queue.length > 0) return Promise.resolve({ value: queue.shift(), done: false });
        if (error) return Promise.reject(error);
        if (done) return Promise.resolve({ value: undefined, done: true });
        return new Promise(function (resolve, reject) {
          waiters.push({ resolve: resolve, reject: reject });
        });
      },
      return: function () {
        done = true;
        emitter.removeAllListeners(name);
        return Promise.resolve({ value: undefined, done: true });
      },
      [Symbol.asyncIterator]: function () { return this; },
    };
  }

  function getEventListeners(emitter, name) {
    return emitter.listeners(name);
  }

  function listenerCount(emitter, event) {
    return emitter.listenerCount(event);
  }

  function setMaxListeners(count) {
    return count;
  }

  function addAbortListener(signal, listener) {
    if (signal.aborted) {
      queueMicrotask(listener);
      return { dispose: function () {} };
    }
    signal.addEventListener("abort", listener);
    return {
      dispose: function () { signal.removeEventListener("abort", listener); },
    };
  }

  var exportsObject = EventEmitter;
  exportsObject.EventEmitter = EventEmitter;
  exportsObject.once = once;
  exportsObject.on = on;
  exportsObject.getEventListeners = getEventListeners;
  exportsObject.getMaxListeners = function (emitter) { return emitter.getMaxListeners(); };
  exportsObject.setMaxListeners = setMaxListeners;
  exportsObject.listenerCount = listenerCount;
  exportsObject.addAbortListener = addAbortListener;
  exportsObject.captureRejections = false;
  exportsObject.captureRejectionSymbol = Symbol.for("nodejs.rejection");
  exportsObject.errorMonitor = EventEmitter.errorMonitor;
  exportsObject.defaultMaxListeners = 10;
  exportsObject.default = EventEmitter;

  shims.events = exportsObject;
  shims["node:events"] = exportsObject;
})();
