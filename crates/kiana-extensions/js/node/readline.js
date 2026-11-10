// node:readline — createInterface + async line iterator (design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;

  class Interface extends EventEmitter {
    constructor(input, output, options) {
      super();
      options = options || {};
      this.input = input;
      this.output = output;
      this.terminal = !!options.terminal;
      this.crlfDelay = options.crlfDelay === undefined ? 100 : options.crlfDelay;
      this.line = "";
      this.closed = false;
      this._buffer = "";
      this._queue = [];
      this._waiters = [];
      this._paused = false;
      var self = this;
      this._onData = function (chunk) {
        var text = typeof chunk === "string" ? chunk : I.bytesToString(I.toBytes(chunk), "utf8");
        self._consume(text);
      };
      this._onEnd = function () {
        if (self._buffer.length > 0) {
          var line = self._buffer;
          self._buffer = "";
          self._emitLine(line);
        }
        self.close();
      };
      this._onClose = function () {
        self.close();
      };
      if (input && typeof input.on === "function") {
        input.on("data", this._onData);
        input.on("end", this._onEnd);
        input.on("close", this._onClose);
        if (typeof input.resume === "function") input.resume();
      }
    }

    _consume(text) {
      this._buffer += text;
      while (true) {
        var index = this._buffer.indexOf("\n");
        if (index === -1) break;
        var line = this._buffer.slice(0, index);
        this._buffer = this._buffer.slice(index + 1);
        if (line.charAt(line.length - 1) === "\r") line = line.slice(0, -1);
        this._emitLine(line);
      }
    }

    _emitLine(line) {
      this.line = line;
      if (this._waiters.length > 0) this._waiters.shift().resolve({ value: line, done: false });
      else this._queue.push(line);
      this.emit("line", line);
    }

    question(query, callback) {
      if (this.output && typeof this.output.write === "function") this.output.write(query);
      if (typeof callback === "function") this.once("line", callback);
    }

    pause() {
      if (this.input && typeof this.input.pause === "function") this.input.pause();
      this._paused = true;
      return this;
    }

    resume() {
      if (this.input && typeof this.input.resume === "function") this.input.resume();
      this._paused = false;
      return this;
    }

    close() {
      if (this.closed) return;
      this.closed = true;
      if (this.input && typeof this.input.removeListener === "function") {
        this.input.removeListener("data", this._onData);
        this.input.removeListener("end", this._onEnd);
        this.input.removeListener("close", this._onClose);
      }
      while (this._waiters.length > 0) {
        this._waiters.shift().resolve({ value: undefined, done: true });
      }
      this.emit("close");
    }

    [Symbol.asyncIterator]() {
      var self = this;
      return {
        next: function () {
          if (self._queue.length > 0) {
            return Promise.resolve({ value: self._queue.shift(), done: false });
          }
          if (self.closed) return Promise.resolve({ value: undefined, done: true });
          return new Promise(function (resolve, reject) {
            self._waiters.push({ resolve: resolve, reject: reject });
          });
        },
        return: function () {
          self.close();
          return Promise.resolve({ value: undefined, done: true });
        },
        [Symbol.asyncIterator]: function () { return this; },
      };
    }
  }

  function createInterface(input, output, options) {
    if (input && typeof input === "object" && input.input !== undefined) {
      options = input;
      input = options.input;
      output = options.output;
    }
    return new Interface(input, output, options || {});
  }

  var readlineObject = {
    Interface: Interface,
    createInterface: createInterface,
    cursorTo: function () {},
    moveCursor: function () {},
    clearLine: function () {},
    clearScreenDown: function () {},
    emitKeypressEvents: function () {},
  };

  shims.readline = readlineObject;
  shims["node:readline"] = readlineObject;
  var promises = {
    createInterface: createInterface,
  };
  shims["readline/promises"] = promises;
  shims["node:readline/promises"] = promises;
})();
