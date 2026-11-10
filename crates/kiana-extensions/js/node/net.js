// node:net — real TCP servers/clients (design §7.3).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;
  var Duplex = shims.stream.Duplex;

  function parseListenArgs(args) {
    var options = { port: 0, host: undefined, callback: undefined, backlog: undefined };
    var index = 0;
    if (typeof args[0] === "object" && args[0] !== null) {
      options.port = args[0].port === undefined ? 0 : args[0].port;
      options.host = args[0].host;
      options.backlog = args[0].backlog;
      index = 1;
    } else if (typeof args[0] === "string" && args[1] === undefined) {
      throw nodeError("EACCES", "listen EACCES: listen on a pipe path is not supported", "listen");
    } else {
      options.port = args[0] === undefined ? 0 : args[0];
      if (typeof args[1] === "string") {
        options.host = args[1];
        index = 2;
      } else {
        index = 1;
      }
    }
    while (index < args.length) {
      if (typeof args[index] === "function") options.callback = args[index];
      else if (typeof args[index] === "number") options.backlog = args[index];
      else if (typeof args[index] === "object" && args[index] !== null) {
        if (args[index].host !== undefined) options.host = args[index].host;
        if (args[index].port !== undefined) options.port = args[index].port;
      }
      index++;
    }
    return options;
  }

  function nodeError(code, message, syscall) {
    var error = new Error(message);
    error.code = code;
    error.syscall = syscall;
    return error;
  }

  function parseConnectArgs(args) {
    var options = { port: 0, host: "localhost", callback: undefined };
    var index = 0;
    if (typeof args[0] === "object" && args[0] !== null) {
      options.port = args[0].port === undefined ? 0 : args[0].port;
      options.host = args[0].host === undefined ? "localhost" : args[0].host;
      index = 1;
    } else {
      options.port = args[0] === undefined ? 0 : args[0];
      if (typeof args[1] === "string") {
        options.host = args[1];
        index = 2;
      } else {
        index = 1;
      }
    }
    while (index < args.length) {
      if (typeof args[index] === "function") options.callback = args[index];
      index++;
    }
    return options;
  }

  class Socket extends Duplex {
    constructor(options) {
      super(options || {});
      options = options || {};
      this.connecting = false;
      this.destroyed = false;
      this.pending = false;
      this.readyState = "closed";
      this.remoteAddress = undefined;
      this.remotePort = undefined;
      this.remoteFamily = undefined;
      this.localAddress = undefined;
      this.localPort = undefined;
      this.bytesRead = 0;
      this.bytesWritten = 0;
      this.bufferSize = 0;
      this._id = options._socketId === undefined ? null : options._socketId;
      this._server = null;
      this._timeout = null;
      this._timeoutMs = 0;
      this.allowHalfOpen = !!options.allowHalfOpen;
      this._closeEmitted = false;
      this.readable = options.readable !== false;
      this.writable = options.writable !== false;
    }

    _setId(id) {
      if (id !== undefined && id !== null) this._id = id;
    }

    connect() {
      var options = parseConnectArgs(arguments);
      var self = this;
      this.connecting = true;
      this.readyState = "opening";
      if (options.callback) this.once("connect", options.callback);
      I.promise("net.connect", {
        port: options.port,
        host: options.host,
        onEvent: function (event) { self._netEvent(event); },
      }).then(function (meta) {
        self._setId(meta.socketId);
      }, function (error) {
        self.connecting = false;
        self.readyState = "closed";
        self.emit("error", error);
        self._emitClose(true);
      });
      return this;
    }

    _emitClose(hadError) {
      if (this._closeEmitted) return;
      this._closeEmitted = true;
      this.readyState = "closed";
      this.destroyed = true;
      this.emit("close", !!hadError);
    }

    /// Node's net.Socket emits 'end' (readable side finished) but leaves
    /// 'close' to the socket teardown path below, so there is exactly one
    /// 'close' and it follows 'end'.
    _emitEnd() {
      var state = this._readableState;
      if (state.endEmitted) return;
      state.endEmitted = true;
      this.emit("end");
    }

    _netEvent(event) {
      if (!event || typeof event !== "object") return;
      if (event.type === "connect") {
        this._setId(event.socketId);
        this.connecting = false;
        this.pending = false;
        this.readyState = "open";
        this.remoteAddress = event.remoteAddress;
        this.remotePort = event.remotePort;
        this.remoteFamily = event.remoteFamily;
        this.localAddress = event.localAddress;
        this.localPort = event.localPort;
        this.emit("connect");
        this.emit("ready");
        return;
      }
      if (event.type === "data") {
        var bytes = Buffer.from(event.b64 || "", "base64");
        this.bytesRead += bytes.length;
        this.push(bytes);
        return;
      }
      if (event.type === "end") {
        this.push(null);
        return;
      }
      if (event.type === "close") {
        // Defer so a pending end-of-read microtask emits 'end' first (Node's
        // order) and never emit a second 'close'.
        var self = this;
        queueMicrotask(function () {
          if (self._readableState.ended) self._emitEnd();
          self._emitClose(!!event.hadError);
        });
        return;
      }
      if (event.type === "error") {
        this.emit("error", I.nodeError(event));
      }
    }

    _write(chunk, encoding, callback) {
      if (this._id === null || this.destroyed) {
        var error = nodeError("EPIPE", "This socket has been ended by the other party", "write");
        callback(error);
        return;
      }
      var bytes = I.toBytes(typeof chunk === "string" ? I.stringToBytes(chunk, encoding) : chunk);
      this.bytesWritten += bytes.length;
      host.netWrite(this._id, I.base64Encode(bytes));
      callback();
    }

    end(chunk, encoding, callback) {
      if (typeof chunk === "function") {
        callback = chunk;
        chunk = undefined;
      } else if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
      if (this._id !== null) host.netEnd(this._id);
      if (!this.writableEnded) {
        this.writableEnded = true;
        this.writable = false;
        var self = this;
        queueMicrotask(function () { self.emit("finish"); });
      }
      if (typeof callback === "function") queueMicrotask(callback);
      return this;
    }

    destroy(error) {
      if (this.destroyed) return this;
      this.destroyed = true;
      this.readyState = "closed";
      if (this._id !== null) host.netDestroy(this._id);
      if (error) this.emit("error", error);
      // A connected socket gets its single 'close' from the host event; a
      // socket that never connected has no host event to wait for.
      if (this._id === null) this._emitClose(!!error);
      return this;
    }

    setTimeout(milliseconds, callback) {
      var self = this;
      if (typeof callback === "function") this.once("timeout", callback);
      if (this._timeout !== null) clearTimeout(this._timeout);
      this._timeoutMs = milliseconds;
      if (milliseconds > 0) {
        this._timeout = setTimeout(function () {
          self._timeout = null;
          self.emit("timeout");
        }, milliseconds);
        if (this._timeout && typeof this._timeout.unref === "function") this._timeout.unref();
      }
      return this;
    }
    setNoDelay() { return this; }
    setKeepAlive() { return this; }
    ref() { return this; }
    unref() { return this; }
    address() {
      return {
        address: this.localAddress,
        family: this.remoteFamily,
        port: this.localPort,
      };
    }
    get bufferSizeValue() { return 0; }
  }

  class Server extends EventEmitter {
    constructor(options, connectionListener) {
      super();
      if (typeof options === "function") {
        connectionListener = options;
        options = {};
      }
      this._options = options || {};
      this._handle = null;
      this._address = null;
      this._sockets = new Map();
      this._connections = 0;
      this.listening = false;
      this.maxConnections = undefined;
      this._unref = false;
      if (typeof connectionListener === "function") this.on("connection", connectionListener);
    }

    listen() {
      var options = parseListenArgs(arguments);
      var self = this;
      if (options.callback) this.once("listening", options.callback);
      var hostValue = options.host === undefined || options.host === null ? "0.0.0.0" : String(options.host);
      I.promise("net.listen", {
        port: options.port || 0,
        host: hostValue,
        onEvent: function (event) { self._serverEvent(event); },
      }).then(function (meta) {
        self._handle = meta.handle;
        self._address = { address: meta.address, family: meta.family, port: meta.port };
        self.listening = true;
        self.emit("listening");
      }, function (error) {
        self.listening = false;
        self.emit("error", error);
      });
      return this;
    }

    _serverEvent(event) {
      if (!event || typeof event !== "object") return;
      if (event.type === "connection") {
        var socket = new Socket({
          _socketId: event.socketId,
          readable: true,
          writable: true,
        });
        socket._server = this;
        socket.connecting = false;
        socket.pending = false;
        socket.readyState = "open";
        socket.remoteAddress = event.remoteAddress;
        socket.remotePort = event.remotePort;
        socket.remoteFamily = event.remoteFamily;
        socket.localAddress = event.localAddress;
        socket.localPort = event.localPort;
        this._sockets.set(event.socketId, socket);
        this._connections++;
        var self = this;
        socket.on("close", function () {
          self._sockets.delete(event.socketId);
          self._connections = Math.max(0, self._connections - 1);
        });
        this.emit("connection", socket);
        return;
      }
      var target = this._sockets.get(event.socketId);
      if (target !== undefined) {
        target._netEvent(event);
        return;
      }
      if (event.type === "error") {
        this.emit("error", I.nodeError(event));
      }
    }

    address() {
      return this._address;
    }

    close(callback) {
      var self = this;
      if (this._handle !== null) host.netClose(this._handle);
      this._handle = null;
      this.listening = false;
      queueMicrotask(function () {
        self.emit("close");
        if (typeof callback === "function") callback();
      });
      return this;
    }

    getConnections(callback) {
      callback(null, this._connections);
    }

    ref() { this._unref = false; return this; }
    unref() { this._unref = true; return this; }
  }

  // Constructor-style use: `new net.Server(...)` / `net.createServer(...)`.
  Server.prototype.constructor = Server;

  function createServer(options, connectionListener) {
    return new Server(options, connectionListener);
  }

  function connect() {
    var socket = new Socket();
    return Socket.prototype.connect.apply(socket, arguments);
  }

  function isIPv4(input) {
    return /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/.test(String(input));
  }

  function isIPv6(input) {
    var text = String(input);
    if (text.indexOf(":") === -1) return false;
    try {
      return /^[0-9a-fA-F:.]+$/.test(text);
    } catch (_) {
      return false;
    }
  }

  function isIP(input) {
    if (isIPv4(input)) return 4;
    if (isIPv6(input)) return 6;
    return 0;
  }

  var netObject = {
    Server: Server,
    Socket: Socket,
    Stream: shims.stream.Stream,
    createServer: createServer,
    connect: connect,
    createConnection: connect,
    isIP: isIP,
    isIPv4: isIPv4,
    isIPv6: isIPv6,
    BlockList: class BlockList {
      constructor() {}
      addAddress() {}
      check() { return false; }
    },
    SocketAddress: class SocketAddress {
      constructor(options) {
        options = options || {};
        this.address = options.address;
        this.family = options.family || "ipv4";
        this.port = options.port;
        this.flowlabel = options.flowlabel || 0;
      }
    },
  };

  shims.net = netObject;
  shims["node:net"] = netObject;
})();
