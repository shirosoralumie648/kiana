// node:http — HTTP/1.1 server and client over node:net (design §7.3).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;
  var Readable = shims.stream.Readable;
  var Writable = shims.stream.Writable;

  var STATUS_CODES = {
    100: "Continue", 101: "Switching Protocols", 102: "Processing", 103: "Early Hints",
    200: "OK", 201: "Created", 202: "Accepted", 203: "Non-Authoritative Information",
    204: "No Content", 205: "Reset Content", 206: "Partial Content", 207: "Multi-Status",
    208: "Already Reported", 226: "IM Used",
    300: "Multiple Choices", 301: "Moved Permanently", 302: "Found", 303: "See Other",
    304: "Not Modified", 305: "Use Proxy", 307: "Temporary Redirect", 308: "Permanent Redirect",
    400: "Bad Request", 401: "Unauthorized", 402: "Payment Required", 403: "Forbidden",
    404: "Not Found", 405: "Method Not Allowed", 406: "Not Acceptable",
    407: "Proxy Authentication Required", 408: "Request Timeout", 409: "Conflict",
    410: "Gone", 411: "Length Required", 412: "Precondition Failed", 413: "Payload Too Large",
    414: "URI Too Long", 415: "Unsupported Media Type", 416: "Range Not Satisfiable",
    417: "Expectation Failed", 418: "I'm a Teapot", 421: "Misdirected Request",
    422: "Unprocessable Entity", 423: "Locked", 424: "Failed Dependency", 425: "Too Early",
    426: "Upgrade Required", 428: "Precondition Required", 429: "Too Many Requests",
    431: "Request Header Fields Too Large", 451: "Unavailable For Legal Reasons",
    500: "Internal Server Error", 501: "Not Implemented", 502: "Bad Gateway",
    503: "Service Unavailable", 504: "Gateway Timeout", 505: "HTTP Version Not Supported",
    506: "Variant Also Negotiates", 507: "Insufficient Storage", 508: "Loop Detected",
    509: "Bandwidth Limit Exceeded", 510: "Not Extended", 511: "Network Authentication Required",
  };

  // Outgoing header store. Node keeps the caller's array values intact
  // (`getHeader` returns the array); on the wire arrays are sent as repeated
  // header lines, except `cookie`, which is joined with "; ".
  function createHeadersStore() {
    return { order: [], values: Object.create(null), names: Object.create(null), raw: [] };
  }

  function storeEnsure(store, name) {
    var lower = String(name).toLowerCase();
    var bucket = store.values[lower];
    if (bucket === undefined) {
      bucket = [];
      store.values[lower] = bucket;
      store.order.push(lower);
      store.names[lower] = String(name);
    }
    return bucket;
  }

  function storeSet(store, name, value) {
    var bucket = storeEnsure(store, name);
    bucket.length = 0;
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) bucket.push(String(value[i]));
    } else {
      bucket.push(String(value));
    }
  }

  function storeAppend(store, name, value) {
    var bucket = storeEnsure(store, name);
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length; i++) bucket.push(String(value[i]));
    } else {
      bucket.push(String(value));
    }
  }

  function storeGet(store, name) {
    var bucket = store.values[String(name).toLowerCase()];
    if (bucket === undefined) return undefined;
    return bucket.length === 1 ? bucket[0] : bucket.slice();
  }

  function storeHas(store, name) {
    return store.values[String(name).toLowerCase()] !== undefined;
  }

  function storeRemove(store, name) {
    var lower = String(name).toLowerCase();
    if (store.values[lower] === undefined) return;
    delete store.values[lower];
    delete store.names[lower];
    var index = store.order.indexOf(lower);
    if (index !== -1) store.order.splice(index, 1);
  }

  function storeHeaders(store) {
    var result = Object.create(null);
    for (var i = 0; i < store.order.length; i++) {
      var lower = store.order[i];
      var bucket = store.values[lower];
      result[lower] = bucket.length === 1 ? bucket[0] : bucket.slice();
    }
    return result;
  }

  function storeLines(store) {
    var lines = [];
    for (var i = 0; i < store.order.length; i++) {
      var lower = store.order[i];
      var name = store.names[lower];
      var bucket = store.values[lower];
      if (lower === "cookie") {
        lines.push([name, bucket.join("; ")]);
      } else {
        for (var j = 0; j < bucket.length; j++) lines.push([name, bucket[j]]);
      }
    }
    return lines;
  }

  // Incoming headers use Node's `_addHeaderLine` joining rules: set-cookie
  // becomes an array, cookie joins with "; ", everything else with ", ".
  function incomingSet(map, name, value) {
    var lower = String(name).toLowerCase();
    if (lower === "set-cookie") {
      if (map[lower] === undefined) map[lower] = [value];
      else map[lower].push(value);
    } else if (lower === "cookie") {
      map[lower] = map[lower] === undefined ? value : map[lower] + "; " + value;
    } else {
      map[lower] = map[lower] === undefined ? value : map[lower] + ", " + value;
    }
  }

  // ---------------------------------------------------------------------
  // Message parser (requests and responses)
  // ---------------------------------------------------------------------

  function Parser(handlers) {
    this.handlers = handlers;
    this.buffer = Buffer.alloc(0);
    this.state = "head";
    this.target = null;
    this.mode = "none";
    this.remaining = 0;
    this.chunkState = "size";
    this.chunkRemaining = 0;
    this.stopped = false;
    // Client-side: responses to HEAD never carry a body (Node's parser knows
    // the request method; ours must be told).
    this.requestMethod = handlers && handlers.requestMethod ? String(handlers.requestMethod) : null;
  }

  Parser.prototype.feed = function (bytes) {
    if (this.stopped) return;
    this.buffer = Buffer.concat([this.buffer, bytes]);
    this._process();
  };

  Parser.prototype._process = function () {
    while (!this.stopped) {
      if (this.state === "head") {
        var index = this.buffer.indexOf("\r\n\r\n");
        if (index === -1) return;
        var head = this.buffer.subarray(0, index).toString("latin1");
        this.buffer = this.buffer.subarray(index + 4);
        this._head(head);
        if (this.stopped) return;
        continue;
      }
      if (this.state === "body") {
        // A zero-length body completes as soon as the header block has been
        // parsed, even when no body bytes ever arrive (Content-Length: 0).
        if (this.mode === "length" && this.remaining === 0) {
          this._complete();
          continue;
        }
        if (this.buffer.length === 0) return;
        if (this.mode === "length") {
          var take = Math.min(this.remaining, this.buffer.length);
          this._push(this.buffer.subarray(0, take));
          this.buffer = this.buffer.subarray(take);
          this.remaining -= take;
          if (this.remaining === 0) {
            this._complete();
            continue;
          }
          return;
        }
        if (this.mode === "chunked") {
          if (!this._chunk()) return;
          continue;
        }
        // until-close: push everything and wait for socket end.
        this._push(this.buffer);
        this.buffer = Buffer.alloc(0);
        return;
      }
      return;
    }
  };

  Parser.prototype._chunk = function () {
    if (this.chunkState === "size") {
      var lineEnd = this.buffer.indexOf("\r\n");
      if (lineEnd === -1) return false;
      var line = this.buffer.subarray(0, lineEnd).toString("latin1");
      this.buffer = this.buffer.subarray(lineEnd + 2);
      var sizeText = line.split(";")[0].trim();
      var size = parseInt(sizeText, 16);
      if (isNaN(size)) size = 0;
      if (size === 0) {
        this.chunkState = "trailers";
      } else {
        this.chunkRemaining = size;
        this.chunkState = "data";
      }
      return true;
    }
    if (this.chunkState === "data") {
      if (this.buffer.length === 0) return false;
      var take = Math.min(this.chunkRemaining, this.buffer.length);
      this._push(this.buffer.subarray(0, take));
      this.buffer = this.buffer.subarray(take);
      this.chunkRemaining -= take;
      if (this.chunkRemaining === 0) this.chunkState = "data-crlf";
      return true;
    }
    if (this.chunkState === "data-crlf") {
      if (this.buffer.length < 2) return false;
      this.buffer = this.buffer.subarray(2);
      this.chunkState = "size";
      return true;
    }
    if (this.chunkState === "trailers") {
      var end = this.buffer.indexOf("\r\n");
      if (end === -1) return false;
      if (end === 0) {
        this.buffer = this.buffer.subarray(2);
        this._complete();
      } else {
        this.buffer = this.buffer.subarray(end + 2);
      }
      return true;
    }
    return false;
  };

  Parser.prototype._push = function (bytes) {
    if (this.target !== null && typeof this.target.push === "function") this.target.push(bytes);
  };

  Parser.prototype._complete = function () {
    var target = this.target;
    this.target = null;
    this.state = "head";
    this.mode = "none";
    this.remaining = 0;
    this.chunkState = "size";
    this.chunkRemaining = 0;
    if (target !== null && typeof target.end === "function") target.end();
  };

  Parser.prototype._head = function (head) {
    var lines = head.split("\r\n");
    if (lines.length === 0) {
      this.state = "head";
      return;
    }
    var map = Object.create(null);
    var rawHeaders = [];
    var first = lines[0];
    var isResponse = /^HTTP\//.test(first);
    var index = 1;
    for (; index < lines.length; index++) {
      var line = lines[index];
      if (line === "") continue;
      var colon = line.indexOf(":");
      if (colon === -1) continue;
      var headerName = line.slice(0, colon).trim();
      var headerValue = line.slice(colon + 1).trim();
      incomingSet(map, headerName, headerValue);
      rawHeaders.push(headerName, headerValue);
    }
    var bodyMode = "none";
    var bodyLength = 0;
    var transfer = map["transfer-encoding"];
    var contentLength = map["content-length"];
    var message;
    if (isResponse) {
      var responseMatch = /^HTTP\/(\d+)\.(\d+) (\d{3})(?: (.*))?$/.exec(first);
      if (!responseMatch) {
        this.stopped = true;
        return;
      }
      bodyMode = "until-close";
      if (transfer && /chunked/i.test(transfer)) bodyMode = "chunked";
      else if (contentLength !== undefined) {
        bodyMode = "length";
        bodyLength = parseInt(contentLength, 10) || 0;
      }
      var status = parseInt(responseMatch[3], 10);
      if (status === 204 || status === 304 || status === 101) bodyMode = "none";
      if (this.requestMethod === "HEAD") bodyMode = "none";
      message = this.handlers.onResponse({
        version: "HTTP/" + responseMatch[1] + "." + responseMatch[2],
        statusCode: status,
        statusMessage: responseMatch[4] || STATUS_CODES[status] || "",
        headers: map,
        rawHeaders: rawHeaders,
        headBytes: head + "\r\n\r\n",
      });
    } else {
      var requestMatch = /^(\S+) (\S+) HTTP\/(\d+)\.(\d+)$/.exec(first);
      if (!requestMatch) {
        this.stopped = true;
        if (this.handlers.onError) this.handlers.onError(new Error("Malformed HTTP request"));
        return;
      }
      bodyMode = "none";
      if (transfer && /chunked/i.test(transfer)) bodyMode = "chunked";
      else if (contentLength !== undefined) {
        bodyMode = "length";
        bodyLength = parseInt(contentLength, 10) || 0;
      }
      var requestInfo = {
        method: requestMatch[1],
        url: requestMatch[2],
        version: "HTTP/" + requestMatch[3] + "." + requestMatch[4],
        headers: map,
        rawHeaders: rawHeaders,
        headText: head,
      };
      if (requestMatch[1] === "CONNECT") {
        // CONNECT 之后这条连接变成隧道：不再产生 request 事件，剩余字节交给
        // 'connect' 监听器（Node 的 head 参数）。
        this.stopped = true;
        this.state = "head";
        if (this.handlers.onConnect) this.handlers.onConnect(requestInfo, this.buffer);
        this.buffer = Buffer.alloc(0);
        return;
      }
      message = this.handlers.onRequest(requestInfo);
    }
    this.target = message;
    if (bodyMode === "none") {
      this._complete();
      return;
    }
    this.state = "body";
    this.mode = bodyMode;
    this.remaining = bodyLength;
    this.chunkState = "size";
    this.chunkRemaining = 0;
  };

  Parser.prototype.finish = function () {
    if (this.stopped) return;
    if (this.state === "body" && this.mode === "until-close") {
      this._complete();
    }
  };

  // ---------------------------------------------------------------------
  // IncomingMessage / ServerResponse
  // ---------------------------------------------------------------------

  class IncomingMessage extends Readable {
    constructor(socket) {
      super();
      this.socket = socket;
      this.connection = socket;
      this.headers = {};
      this.rawHeaders = [];
      this.httpVersion = "1.1";
      this.httpVersionMajor = 1;
      this.httpVersionMinor = 1;
      this.complete = false;
      this.aborted = false;
      this.method = undefined;
      this.url = undefined;
      this.statusCode = undefined;
      this.statusMessage = undefined;
      this.trailers = {};
      this.trailer = this.trailers;
      this.readable = true;
    }
    setTimeout(milliseconds, callback) {
      if (typeof callback === "function") this.once("timeout", callback);
      if (this.socket) this.socket.setTimeout(milliseconds);
      return this;
    }
    destroy(error) {
      if (this.socket) this.socket.destroy();
      return super.destroy(error);
    }
  }

  class ServerResponse extends Writable {
    constructor(socket, request) {
      super();
      this.statusCode = 200;
      this.statusMessage = undefined;
      this.headersSent = false;
      this.sendDate = true;
      this.finished = false;
      this.socket = socket;
      this.connection = socket;
      this.req = request;
      this._store = createHeadersStore();
      this._chunked = false;
      this._closed = false;
      this._autoContentLength = undefined;
      // Node suppresses the response body for HEAD requests (headers stay identical).
      this._headRequest = !!(request && String(request.method).toUpperCase() === "HEAD");
    }

    setHeader(name, value) {
      if (this.headersSent) throw new Error("Cannot set headers after they are sent to the client");
      storeSet(this._store, name, value);
      return this;
    }
    appendHeader(name, value) {
      storeAppend(this._store, name, value);
      return this;
    }
    getHeader(name) {
      return storeGet(this._store, name);
    }
    getHeaders() {
      return storeHeaders(this._store);
    }
    getHeaderNames() {
      return this._store.order.slice();
    }
    hasHeader(name) {
      return storeHas(this._store, name);
    }
    removeHeader(name) {
      if (this.headersSent) throw new Error("Cannot remove headers after they are sent to the client");
      storeRemove(this._store, name);
    }

    writeHead(statusCode, statusMessage, headers) {
      if (typeof statusMessage === "object" && statusMessage !== null) {
        headers = statusMessage;
        statusMessage = undefined;
      }
      this.statusCode = statusCode;
      if (statusMessage !== undefined) this.statusMessage = statusMessage;
      if (headers) {
        if (Array.isArray(headers)) {
          for (var i = 0; i + 1 < headers.length; i += 2) this.setHeader(headers[i], headers[i + 1]);
        } else {
          var keys = Object.keys(headers);
          for (var j = 0; j < keys.length; j++) this.setHeader(keys[j], headers[keys[j]]);
        }
      }
      return this;
    }

    _ensureHead() {
      if (this.headersSent) return;
      if (!storeHas(this._store, "date") && this.sendDate) {
        storeSet(this._store, "Date", new Date().toUTCString());
      }
      // Node decides the response connection from the request's Connection
      // header/version unless the handler set one explicitly, and always keeps
      // its own notion of whether the socket must close afterwards.
      var requestConnection = this.req && this.req.headers
        ? this.req.headers.connection
        : undefined;
      var requestText = requestConnection === undefined ? "" : String(requestConnection);
      var requestWantsClose = /close/i.test(requestText) ||
        (this.req && String(this.req.httpVersion) === "1.0" && !/keep-alive/i.test(requestText));
      if (!storeHas(this._store, "connection")) {
        storeSet(this._store, "Connection", requestWantsClose ? "close" : "keep-alive");
      }
      if (!requestWantsClose && !storeHas(this._store, "keep-alive")) {
        storeSet(this._store, "Keep-Alive", "timeout=5");
      }
      if (this._chunked && !storeHas(this._store, "transfer-encoding")) {
        storeSet(this._store, "Transfer-Encoding", "chunked");
      }
      if (this._autoContentLength !== undefined &&
          !storeHas(this._store, "content-length") &&
          !storeHas(this._store, "transfer-encoding")) {
        storeSet(this._store, "Content-Length", String(this._autoContentLength));
      }
      this._shouldClose = requestWantsClose ||
        /close/i.test(this._store.values["connection"].join(","));
      this.headersSent = true;
      var message = this.statusMessage === undefined
        ? (STATUS_CODES[this.statusCode] || "")
        : this.statusMessage;
      var head = "HTTP/1.1 " + this.statusCode + (message ? " " + message : "") + "\r\n";
      var lines = storeLines(this._store);
      for (var i = 0; i < lines.length; i++) {
        head += lines[i][0] + ": " + lines[i][1] + "\r\n";
      }
      head += "\r\n";
      this.socket.write(head);
    }

    write(chunk, encoding, callback) {
      if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      if (chunk === undefined || chunk === null) chunk = Buffer.alloc(0);
      var bytes = I.toBytes(typeof chunk === "string" ? I.stringToBytes(chunk, encoding) : chunk);
      if (!this.headersSent && !this.hasHeader("content-length") &&
          !this.hasHeader("transfer-encoding") && this._autoContentLength === undefined) {
        this._chunked = true;
      }
      this._ensureHead();
      if (!this._headRequest && bytes.length > 0) {
        if (this._chunked) {
          this.socket.write(bytes.length.toString(16) + "\r\n");
          this.socket.write(bytes);
          this.socket.write("\r\n");
        } else {
          this.socket.write(bytes);
        }
      }
      if (typeof callback === "function") queueMicrotask(callback);
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
      if (chunk !== undefined && chunk !== null) {
        var chunkBytes = I.toBytes(
          typeof chunk === "string" ? I.stringToBytes(chunk, encoding) : chunk,
        );
        // Node can still choose Content-Length for a single final chunk, and
        // emits it after the connection headers.
        if (!this.headersSent && !this.hasHeader("content-length") &&
            !this.hasHeader("transfer-encoding")) {
          this._autoContentLength = chunkBytes.length;
        }
        this.write(chunk, encoding);
      } else if (!this.headersSent) {
        if (!this.hasHeader("content-length") && !this.hasHeader("transfer-encoding")) {
          this._autoContentLength = 0;
        }
        this._ensureHead();
      }
      if (this._chunked) this.socket.write("0\r\n\r\n");
      this.finished = true;
      this.writableEnded = true;
      this.writable = false;
      this.emit("finish");
      // Once the response is finished Node discards any unread request body,
      // which lets a pending 'end' listener fire.
      if (this.req && this.req._readableState && this.req._readableState.flowing !== true) {
        this.req.resume();
      }
      if (this._shouldClose) this.socket.end();
      if (typeof callback === "function") queueMicrotask(callback);
      return this;
    }

    flushHeaders() {
      this._chunked = !this.hasHeader("content-length") && !this._headRequest;
      this._ensureHead();
    }

    setTimeout(milliseconds, callback) {
      if (typeof callback === "function") this.once("timeout", callback);
      if (this.socket) this.socket.setTimeout(milliseconds);
      return this;
    }

    destroy(error) {
      if (this.socket) this.socket.destroy();
      return this;
    }
  }

  // ---------------------------------------------------------------------
  // Server
  // ---------------------------------------------------------------------

  class Server extends shims.net.Server {
    constructor(options, requestListener) {
      if (typeof options === "function") {
        requestListener = options;
        options = {};
      }
      super(options);
      this.timeout = 0;
      this.keepAliveTimeout = 5000;
      this.headersTimeout = 60000;
      this.requestTimeout = 300000;
      this.maxRequestsPerSocket = 0;
      this._connectionsMap = new Map();
      var self = this;
      if (typeof requestListener === "function") this.on("request", requestListener);
      this.on("connection", function (socket) { self._accept(socket); });
    }

    _accept(socket) {
      var self = this;
      var parser = new Parser({
        onRequest: function (info) {
          var req = new IncomingMessage(socket);
          req.method = info.method;
          req.url = info.url;
          req.httpVersion = info.version;
          var match = /^HTTP\/(\d+)\.(\d+)$/.exec(info.version);
          if (match) {
            req.httpVersionMajor = parseInt(match[1], 10);
            req.httpVersionMinor = parseInt(match[2], 10);
          }
          req.headers = info.headers;
          req.rawHeaders = info.rawHeaders;
          var res = new ServerResponse(socket, req);
          res.on("finish", function () { req.complete = true; });
          queueMicrotask(function () {
            try {
              self.emit("request", req, res);
            } catch (error) {
              if (!res.headersSent) {
                res.writeHead(500, { "Content-Type": "text/plain" });
                res.end("Internal Server Error");
              }
              self.emit("clientError", error, socket);
            }
          });
          return {
            push: function (bytes) { req.push(bytes); req._kianaBodyBytes = (req._kianaBodyBytes || 0) + bytes.length; },
            end: function () { req.complete = true; req.push(null); },
          };
        },
        onConnect: function (info, head) {
          var req = new IncomingMessage(socket);
          req.method = "CONNECT";
          req.url = info.url;
          req.httpVersion = info.version;
          req.headers = info.headers;
          req.rawHeaders = info.rawHeaders;
          self.emit("connect", req, socket, head);
          return null;
        },
        onError: function (error) {
          self.emit("clientError", error, socket);
        },
      });
      this._connectionsMap.set(socket, parser);
      socket.on("data", function (chunk) {
        try {
          parser.feed(chunk);
        } catch (error) {
          self.emit("clientError", error, socket);
        }
      });
      socket.on("end", function () { parser.finish(); });
      socket.on("close", function () { self._connectionsMap.delete(socket); });
    }
  }

  function createServer(options, requestListener) {
    return new Server(options, requestListener);
  }

  // ---------------------------------------------------------------------
  // Client
  // ---------------------------------------------------------------------

  class ClientRequest extends EventEmitter {
    constructor(options, callback) {
      super();
      if (typeof options === "string" || options instanceof globalThis.URL) {
        var parsed = new globalThis.URL(options);
        options = {
          protocol: parsed.protocol,
          hostname: parsed.hostname,
          port: parsed.port || (parsed.protocol === "https:" ? 443 : 80),
          path: parsed.pathname + parsed.search,
          auth: parsed.username
            ? decodeURIComponent(parsed.username) + (parsed.password ? ":" + decodeURIComponent(parsed.password) : "")
            : undefined,
        };
      }
      options = options || {};
      this.method = String(options.method === undefined ? "GET" : options.method).toUpperCase();
      this.path = options.path === undefined ? "/" : String(options.path);
      this.host = options.hostname || options.host || "localhost";
      if (this.host.indexOf(":") !== -1 && this.host.charAt(0) !== "[") {
        var hostMatch = /^([^:]+):(\d+)$/.exec(this.host);
        if (hostMatch) {
          this.host = hostMatch[1];
          options.port = options.port || hostMatch[2];
        }
      }
      this.port = options.port === undefined
        ? (options.protocol === "https:" ? 443 : 80)
        : options.port;
      this.protocol = options.protocol || "http:";
      this._store = createHeadersStore();
      this._body = [];
      this._started = false;
      this._response = null;
      this._socket = null;
      this._sawWrite = false;
      this._hasBody = !/^(GET|HEAD|DELETE|OPTIONS|TRACE|CONNECT)$/.test(this.method);
      this.aborted = false;
      this.finished = false;
      this._closeEmitted = false;
      this.timeout = options.timeout;
      this._timeout = null;
      if (options.headers) {
        var keys = Object.keys(options.headers);
        for (var i = 0; i < keys.length; i++) this.setHeader(keys[i], options.headers[keys[i]]);
      }
      if (options.auth) this.setHeader("Authorization", "Basic " + Buffer.from(options.auth).toString("base64"));
      if (typeof callback === "function") this.once("response", callback);
      if (this.timeout && this.timeout > 0) {
        var self = this;
        this._timeout = setTimeout(function () {
          var error = new Error("socket hang up");
          error.code = "ETIMEDOUT";
          self.emit("timeout");
          self.destroy(error);
        }, this.timeout);
      }
    }

    setHeader(name, value) {
      storeSet(this._store, name, value);
      return this;
    }
    getHeader(name) { return storeGet(this._store, name); }
    getHeaders() { return storeHeaders(this._store); }
    hasHeader(name) { return storeHas(this._store, name); }
    removeHeader(name) {
      storeRemove(this._store, name);
    }
    setTimeout(milliseconds, callback) {
      if (typeof callback === "function") this.once("timeout", callback);
      this.timeout = milliseconds;
      return this;
    }

    _start() {
      if (this._started) return;
      this._started = true;
      this._body = [];
      this._headSent = false;
      this._chunked = false;
      this._connected = false;
      var self = this;
      var socket = shims.net.connect(Number(this.port), this.host);
      this._socket = socket;
      socket.on("connect", function () {
        self._connected = true;
        self.emit("socket", socket);
        if (!self._headSent) self._flushHead();
        if (self.finished) {
          self._finishBody();
        }
      });
      var parser = new Parser({
        requestMethod: self.method,
        onResponse: function (info) {
          var response = new IncomingMessage(socket);
          response.statusCode = info.statusCode;
          response.statusMessage = info.statusMessage;
          response.headers = info.headers;
          response.rawHeaders = info.rawHeaders;
          response.httpVersion = info.version;
          self._response = response;
          self.emit("response", response);
          return {
            push: function (bytes) { response.push(bytes); },
            end: function () { response.complete = true; response.push(null); },
          };
        },
        onError: function (error) { self.emit("error", error); },
      });
      socket.on("data", function (chunk) { parser.feed(chunk); });
      socket.on("end", function () { parser.finish(); });
      socket.on("error", function (error) { self.emit("error", error); });
      socket.on("close", function () {
        if (self._timeout !== null) clearTimeout(self._timeout);
        self._emitClose();
      });
      if (self._timeout !== null && typeof socket.setTimeout === "function") {
        socket.setTimeout(self.timeout);
      }
    }

    _flushHead() {
      if (this._headSent || !this._socket) return;
      if (!this.hasHeader("host")) {
        this.setHeader(
          "Host",
          Number(this.port) === 80 || Number(this.port) === 443
            ? this.host
            : this.host + ":" + this.port,
        );
      }
      // Node 26's default agent keeps connections alive.
      if (!this.hasHeader("connection")) this.setHeader("Connection", "keep-alive");
      var total = 0;
      for (var t = 0; t < this._body.length; t++) total += this._body[t].length;
      if (!this.hasHeader("content-length") && !this.hasHeader("transfer-encoding")) {
        if (!this.finished || this._sawWrite) {
          if (total > 0 || this._hasBody) {
            this._chunked = true;
            this.setHeader("Transfer-Encoding", "chunked");
          }
        } else if (this._hasBody) {
          // Node sends `Content-Length: 0` for bodyless POST/PUT/PATCH…,
          // but never for GET/HEAD/DELETE/OPTIONS/TRACE.
          this.setHeader("Content-Length", String(total));
        }
      }
      var head = this.method + " " + this.path + " HTTP/1.1\r\n";
      var lines = storeLines(this._store);
      for (var i = 0; i < lines.length; i++) {
        head += lines[i][0] + ": " + lines[i][1] + "\r\n";
      }
      head += "\r\n";
      this._socket.write(head);
      this._headSent = true;
      for (var j = 0; j < this._body.length; j++) this._writeBody(this._body[j]);
      this._body = [];
    }

    _writeBody(bytes) {
      if (this._chunked) {
        if (bytes.length === 0) return;
        this._socket.write(bytes.length.toString(16) + "\r\n");
        this._socket.write(bytes);
        this._socket.write("\r\n");
      } else {
        this._socket.write(bytes);
      }
    }

    _finishBody() {
      if (!this._socket || !this._headSent) return;
      if (this._chunked) this._socket.write("0\r\n\r\n");
      // Node 不会在请求体写完后半关连接：响应仍要能通过同一条 TCP 连接回来。
      // 连接关闭由响应侧（`Connection: close`）或调用方决定。
    }

    write(chunk, encoding, callback) {
      if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      var bytes = I.toBytes(typeof chunk === "string" ? I.stringToBytes(chunk, encoding) : chunk);
      if (!this._started) this._start();
      if (!this._headSent) this._sawWrite = true;
      if (this._headSent) {
        this._writeBody(bytes);
      } else {
        this._body.push(bytes);
      }
      if (typeof callback === "function") queueMicrotask(callback);
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
      if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
      if (!this._started) this._start();
      this.finished = true;
      if (this._connected) {
        if (!this._headSent) this._flushHead();
        this._finishBody();
      }
      if (typeof callback === "function") queueMicrotask(callback);
      return this;
    }

    _emitClose() {
      if (this._closeEmitted) return;
      this._closeEmitted = true;
      this.emit("close");
    }

    abort() {
      this.aborted = true;
      if (this._socket) this._socket.destroy();
      this.emit("abort");
    }

    destroy(error) {
      this.aborted = true;
      if (this._socket) this._socket.destroy();
      if (error) this.emit("error", error);
      this._emitClose();
      return this;
    }
  }

  function request(options, callback) {
    return new ClientRequest(options, callback);
  }

  function get(options, callback) {
    var req = request(options, callback);
    req.end();
    return req;
  }

  class Agent {
    constructor(options) {
      this.options = options || {};
    }
    destroy() {}
  }

  var httpObject = {
    Server: Server,
    IncomingMessage: IncomingMessage,
    ServerResponse: ServerResponse,
    ClientRequest: ClientRequest,
    Agent: Agent,
    createServer: createServer,
    request: request,
    get: get,
    STATUS_CODES: STATUS_CODES,
    METHODS: ["DELETE", "GET", "HEAD", "POST", "PUT", "CONNECT", "OPTIONS", "TRACE", "PATCH"],
    globalAgent: new Agent(),
    validateHeaderName: function (name) {
      if (!/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(String(name))) {
        var error = new TypeError("Header name must be a valid HTTP token [\"" + name + "\"]");
        error.code = "ERR_INVALID_HTTP_TOKEN";
        throw error;
      }
    },
    validateHeaderValue: function (name, value) {
      if (value === undefined) {
        var error = new TypeError("Invalid value \"undefined\" for header \"" + name + "\"");
        error.code = "ERR_HTTP_INVALID_HEADER_VALUE";
        throw error;
      }
    },
    setMaxIdleHTTPParsers: function () {},
  };

  shims.http = httpObject;
  shims["node:http"] = httpObject;
})();
