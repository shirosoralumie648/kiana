// node:stream — minimal Readable/Writable/Duplex/PassThrough used by fs,
// child_process, net and http (design §7.4 item 4: only on("data")/for await
// are required to match Node).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;

  function chunkToBuffer(chunk, encoding) {
    if (typeof chunk === "string") return Buffer.from(chunk, encoding || "utf8");
    if (chunk instanceof Uint8Array) return chunk;
    if (Buffer.isBuffer(chunk)) return chunk;
    return Buffer.from(String(chunk));
  }

  class Stream extends EventEmitter {
    pipe(destination, options) {
      var self = this;
      this.on("data", function (chunk) {
        destination.write(chunk);
      });
      this.on("end", function () {
        destination.end();
      });
      this.on("error", function (error) {
        if (typeof destination.destroy === "function") destination.destroy(error);
      });
      return destination;
    }
  }

  class Readable extends Stream {
    constructor(options) {
      super();
      options = options || {};
      this._readableState = {
        buffer: [],
        flowing: null,
        ended: false,
        encoding: null,
        objectMode: !!options.objectMode,
        highWaterMark: options.highWaterMark === undefined ? 16384 : options.highWaterMark,
        paused: false,
        started: false,
        endEmitted: false,
      };
      this.readable = true;
      this.destroyed = false;
      if (typeof options.read === "function") this._read = options.read;
      if (typeof options.destroy === "function") this._destroy = options.destroy;
    }

    push(chunk, encoding) {
      var state = this._readableState;
      if (state.ended) return false;
      if (chunk === null || chunk === undefined) {
        state.ended = true;
        this.readable = false;
        if (state.flowing === true) {
          // Node 在微任务里发 'end'（不在 push(null) 的调用栈上），否则
          // 先挂 'data' 再挂 'end' 的消费者会漏掉 end。
          var endedStream = this;
          queueMicrotask(function () { endedStream._emitEnd(); });
        } else if (state.flowing === null) {
          this.emit("readable");
        }
        return false;
      }
      var value = state.objectMode
        ? chunk
        : state.encoding
          ? (typeof chunk === "string" ? chunk : I.bytesToString(chunkToBuffer(chunk, encoding), state.encoding))
          : chunkToBuffer(chunk, encoding);
      if (state.flowing === true) {
        this.emit("data", value);
      } else {
        state.buffer.push(value);
        if (state.flowing === null) this.emit("readable");
      }
      return true;
    }

    unshift(chunk, encoding) {
      this._readableState.buffer.unshift(
        this._readableState.encoding
          ? I.bytesToString(chunkToBuffer(chunk, encoding), this._readableState.encoding)
          : chunkToBuffer(chunk, encoding),
      );
    }

    read(size) {
      var state = this._readableState;
      if (state.buffer.length === 0) return null;
      var chunk = state.buffer.shift();
      return chunk;
    }

    on(event, listener) {
      var result = super.on(event, listener);
      if (event === "data") this.resume();
      if (event === "readable") this.resume();
      return result;
    }
    addListener(event, listener) { return this.on(event, listener); }

    resume() {
      var state = this._readableState;
      state.flowing = true;
      state.paused = false;
      if (!state.started) {
        state.started = true;
        if (typeof this._read === "function") this._read();
      }
      var buffer = state.buffer;
      state.buffer = [];
      for (var i = 0; i < buffer.length; i++) this.emit("data", buffer[i]);
      if (state.ended) {
        var endedStream = this;
        queueMicrotask(function () { endedStream._emitEnd(); });
      }
      return this;
    }

    /// 只发一次 'end'，随后是 'close'（与 Node 的 emitClose 顺序一致）。
    _emitEnd() {
      var state = this._readableState;
      if (state.endEmitted) return;
      state.endEmitted = true;
      this.emit("end");
      this.emit("close");
    }

    pause() {
      this._readableState.flowing = false;
      this._readableState.paused = true;
      return this;
    }

    isPaused() { return this._readableState.paused; }

    setEncoding(encoding) {
      this._readableState.encoding = I.normalizeEncoding(encoding);
      return this;
    }

    unpipe(destination) {
      this.removeAllListeners("data");
      return this;
    }

    destroy(error) {
      if (this.destroyed) return this;
      this.destroyed = true;
      this.readable = false;
      if (typeof this._destroy === "function") {
        var self = this;
        this._destroy(error, function (failure) {
          if (failure) self.emit("error", failure);
          self.emit("close");
        });
      } else {
        if (error) this.emit("error", error);
        this.emit("close");
      }
      return this;
    }

    [Symbol.asyncIterator]() {
      var self = this;
      var queue = [];
      var waiters = [];
      var ended = false;
      var failure = null;
      function settle() {
        while (waiters.length > 0) {
          if (queue.length > 0) {
            waiters.shift().resolve({ value: queue.shift(), done: false });
          } else if (failure) {
            waiters.shift().reject(failure);
          } else if (ended) {
            waiters.shift().resolve({ value: undefined, done: true });
          } else {
            break;
          }
        }
      }
      this.on("data", function (chunk) {
        queue.push(chunk);
        settle();
      });
      this.on("end", function () {
        ended = true;
        settle();
      });
      this.on("error", function (error) {
        failure = error;
        settle();
      });
      this.resume();
      var iterator = {
        next: function () {
          if (queue.length > 0) return Promise.resolve({ value: queue.shift(), done: false });
          if (failure) return Promise.reject(failure);
          if (ended) return Promise.resolve({ value: undefined, done: true });
          return new Promise(function (resolve, reject) {
            waiters.push({ resolve: resolve, reject: reject });
          });
        },
        return: function () {
          ended = true;
          self.destroy();
          return Promise.resolve({ value: undefined, done: true });
        },
        [Symbol.asyncIterator]: function () { return this; },
      };
      void self;
      return iterator;
    }
  }

  class Writable extends Stream {
    constructor(options) {
      super();
      options = options || {};
      this._writableState = { ended: false, finished: false };
      this.writable = true;
      this.destroyed = false;
      this.writableEnded = false;
      this.writableFinished = false;
      if (typeof options.write === "function") this._write = options.write;
      if (typeof options.final === "function") this._final = options.final;
      if (typeof options.destroy === "function") this._destroy = options.destroy;
    }

    write(chunk, encoding, callback) {
      if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      var self = this;
      var done = typeof callback === "function" ? callback : function () {};
      if (this._writableState.ended) {
        var error = new Error("write after end");
        error.code = "ERR_STREAM_WRITE_AFTER_END";
        if (typeof callback === "function") callback(error);
        else this.emit("error", error);
        return false;
      }
      if (typeof this._write === "function") {
        try {
          this._write(chunk, encoding, function (failure) {
            if (failure) {
              self.emit("error", failure);
              done(failure);
            } else {
              done();
            }
          });
        } catch (failure) {
          this.emit("error", failure);
          done(failure);
          return false;
        }
      } else {
        done();
      }
      return true;
    }

    end(chunk, encoding, callback) {
      if (typeof chunk === "function") {
        callback = chunk;
        chunk = undefined;
      } else if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      var self = this;
      if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
      this.writableEnded = true;
      this._writableState.ended = true;
      function finish() {
        self._writableState.finished = true;
        self.writableFinished = true;
        self.writable = false;
        self.emit("finish");
        self.emit("close");
        if (typeof callback === "function") callback();
      }
      if (typeof this._final === "function") {
        try {
          this._final(function (failure) {
            if (failure) self.emit("error", failure);
            finish();
          });
        } catch (failure) {
          this.emit("error", failure);
          finish();
        }
      } else {
        finish();
      }
      return this;
    }

    cork() {}
    uncork() {}
    setDefaultEncoding() { return this; }
    destroy(error) {
      if (this.destroyed) return this;
      this.destroyed = true;
      this.writable = false;
      if (error) this.emit("error", error);
      this.emit("close");
      return this;
    }
  }

  class Duplex extends Readable {
    constructor(options) {
      super(options);
      options = options || {};
      this._writableState = { ended: false, finished: false };
      this.writable = true;
      this.writableEnded = false;
      this.writableFinished = false;
      if (typeof options.write === "function") this._write = options.write;
      if (typeof options.final === "function") this._final = options.final;
    }
  }
  // Copy the Writable surface onto Duplex (no multiple inheritance in JS).
  Object.getOwnPropertyNames(Writable.prototype).forEach(function (key) {
    if (key === "constructor") return;
    if (["_write", "_final", "_destroy"].indexOf(key) !== -1) return;
    Object.defineProperty(
      Duplex.prototype,
      key,
      Object.getOwnPropertyDescriptor(Writable.prototype, key),
    );
  });

  class PassThrough extends Duplex {
    constructor(options) {
      super(options);
    }
    _write(chunk, encoding, callback) {
      this.push(chunk, encoding);
      callback();
    }
  }

  function pipeline() {
    var args = Array.prototype.slice.call(arguments);
    var callback = typeof args[args.length - 1] === "function" ? args.pop() : function () {};
    if (args.length === 0) return callback();
    for (var i = 0; i < args.length - 1; i++) args[i].pipe(args[i + 1]);
    var last = args[args.length - 1];
    var finished = false;
    function done(error) {
      if (finished) return;
      finished = true;
      callback(error);
    }
    last.on("finish", function () { done(); });
    last.on("end", function () { done(); });
    last.on("error", done);
    for (var j = 0; j < args.length; j++) args[j].on("error", done);
    return last;
  }

  function finished(stream, callback) {
    var called = false;
    function done(error) {
      if (called) return;
      called = true;
      callback(error);
    }
    stream.on("end", function () { done(); });
    stream.on("finish", function () { done(); });
    stream.on("error", done);
    stream.on("close", function () { done(); });
    return function () {};
  }

  function isReadable(stream) { return !!stream && stream.readable !== false; }
  function isWritable(stream) { return !!stream && stream.writable !== false; }

  var exportsObject = {
    Stream: Stream,
    Readable: Readable,
    Writable: Writable,
    Duplex: Duplex,
    Transform: Duplex,
    PassThrough: PassThrough,
    pipeline: pipeline,
    finished: finished,
    addAbortSignal: function (signal, stream) { return stream; },
    isReadable: isReadable,
    isWritable: isWritable,
    ReadableStream: globalThis.ReadableStream,
    WritableStream: globalThis.WritableStream,
  };
  exportsObject.default = exportsObject;

  // `pipeline`/`finished` on node:stream/promises.
  var promises = {
    pipeline: function () {
      var args = Array.prototype.slice.call(arguments);
      return new Promise(function (resolve, reject) {
        args.push(function (error) { if (error) reject(error); else resolve(); });
        pipeline.apply(null, args);
      });
    },
    finished: function (stream) {
      return new Promise(function (resolve, reject) {
        finished(stream, function (error) { if (error) reject(error); else resolve(); });
      });
    },
  };
  shims.stream = exportsObject;
  shims["node:stream"] = exportsObject;
  shims["stream/promises"] = promises;
  shims["node:stream/promises"] = promises;
  shims["stream/web"] = {
    ReadableStream: globalThis.ReadableStream,
    WritableStream: globalThis.WritableStream,
    TransformStream: globalThis.TransformStream,
    ByteLengthQueuingStrategy: globalThis.ByteLengthQueuingStrategy,
    CountQueuingStrategy: globalThis.CountQueuingStrategy,
  };
  shims["node:stream/web"] = shims["stream/web"];
})();
