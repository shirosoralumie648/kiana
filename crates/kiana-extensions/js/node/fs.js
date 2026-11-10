// node:fs — synchronous operations go straight to the Rust `__kiana_host.fsSync`
// primitive (std::fs); readdirSync is deliberately NOT sorted (design §6.3).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;

  function fsCall(operation, args) {
    return JSON.parse(host.fsSync(operation, JSON.stringify(args)));
  }

  var constants = {
    F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1,
    O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2, O_CREAT: 64, O_EXCL: 128,
    O_TRUNC: 512, O_APPEND: 1024, O_DIRECTORY: 65536, O_NOFOLLOW: 131072,
    O_SYNC: 1052672, O_DSYNC: 4096, O_NONBLOCK: 2048,
    COPYFILE_EXCL: 1, COPYFILE_FICLONE: 2, COPYFILE_FICLONE_FORCE: 4,
    UV_FS_SYMLINK_DIR: 1, UV_FS_SYMLINK_JUNCTION: 2,
    S_IFMT: 61440, S_IFREG: 32768, S_IFDIR: 16384, S_IFCHR: 8192,
    S_IFBLK: 24576, S_IFIFO: 4096, S_IFLNK: 40960, S_IFSOCK: 49152,
    S_IRWXU: 448, S_IRUSR: 256, S_IWUSR: 128, S_IXUSR: 64,
    S_IRWXG: 56, S_IRGRP: 32, S_IWGRP: 16, S_IXGRP: 8,
    S_IRWXO: 7, S_IROTH: 4, S_IWOTH: 2, S_IXOTH: 1,
  };

  class Stats {
    constructor(json) {
      // Own-key order/shape follows Node 26's fs.Stats.
      this.dev = json.dev;
      this.mode = json.mode;
      this.nlink = json.nlink;
      this.uid = json.uid;
      this.gid = json.gid;
      this.rdev = 0;
      this.blksize = json.blksize === undefined ? 4096 : json.blksize;
      this.ino = json.ino;
      this.size = json.size;
      this.blocks = json.blocks === undefined ? Math.ceil(json.size / 512) : json.blocks;
      this.atimeMs = json.atimeMs;
      this.mtimeMs = json.mtimeMs;
      this.ctimeMs = json.ctimeMs;
      this.birthtimeMs = json.birthtimeMs;
      // Node exposes the Date forms as non-enumerable accessors.
      Object.defineProperty(this, "atime", { value: new Date(json.atimeMs), enumerable: false });
      Object.defineProperty(this, "mtime", { value: new Date(json.mtimeMs), enumerable: false });
      Object.defineProperty(this, "ctime", { value: new Date(json.ctimeMs), enumerable: false });
      Object.defineProperty(this, "birthtime", { value: new Date(json.birthtimeMs), enumerable: false });
      Object.defineProperty(this, "_type", { value: json.type, enumerable: false });
    }
    isFile() { return this._type === "file"; }
    isDirectory() { return this._type === "dir"; }
    isSymbolicLink() { return this._type === "symlink"; }
    isBlockDevice() { return this._type === "block"; }
    isCharacterDevice() { return this._type === "char"; }
    isFIFO() { return this._type === "fifo"; }
    isSocket() { return this._type === "socket"; }
  }

  class Dirent {
    constructor(name, type, parentPath) {
      this.name = name;
      this.parentPath = parentPath;
      Object.defineProperty(this, "path", { value: parentPath, enumerable: false });
      Object.defineProperty(this, "_type", { value: type, enumerable: false });
    }
    isFile() { return this._type === "file"; }
    isDirectory() { return this._type === "dir"; }
    isSymbolicLink() { return this._type === "symlink"; }
    isBlockDevice() { return this._type === "block"; }
    isCharacterDevice() { return this._type === "char"; }
    isFIFO() { return this._type === "fifo"; }
    isSocket() { return this._type === "socket"; }
  }

  function optionsObject(options) {
    if (options === undefined || options === null) return {};
    if (typeof options === "string") return { encoding: options };
    return options;
  }

  function validateEncoding(encoding) {
    if (encoding === undefined || encoding === null) return;
    if (typeof encoding !== "string" || !Buffer.isEncoding(encoding)) {
      var error = new TypeError(
        "The argument 'encoding' is invalid encoding. Received " + jsReceived(encoding),
      );
      error.code = "ERR_INVALID_ARG_VALUE";
      throw error;
    }
  }

  function jsReceived(value) {
    if (typeof value === "string") return "'" + value + "'";
    return typeof value === "number" ? value : String(value);
  }

  function dataToBase64(data, options) {
    options = optionsObject(options);
    if (typeof data === "string") {
      validateEncoding(options.encoding);
      return I.base64Encode(I.stringToBytes(data, options.encoding));
    }
    if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
      return I.base64Encode(I.toBytes(data));
    }
    var received;
    if (typeof data === "number") received = "type number (" + data + ")";
    else if (Array.isArray(data)) received = "an instance of Array";
    else if (data === null) received = "null";
    else if (typeof data === "object") received = "an instance of " + (data.constructor && data.constructor.name ? data.constructor.name : "Object");
    else received = "type " + typeof data;
    var error = new TypeError(
      "The \"data\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received " + received,
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }

  function decodeFile(result, options) {
    options = optionsObject(options);
    var bytes = Buffer.from(I.base64Decode(result.b64));
    if (options.encoding === undefined || options.encoding === null) return bytes;
    validateEncoding(options.encoding);
    return bytes.toString(options.encoding);
  }

  function readFileSync(path, options) {
    return decodeFile(fsCall("readFile", [String(path)]), options);
  }
  readFileSync.native = function () { throw new Error("fs.readFileSync.native is not supported"); };

  function writeFileSync(path, data, options) {
    options = optionsObject(options);
    var flag = options.flag === undefined ? "w" : String(options.flag);
    var mode = options.mode === undefined ? 438 : options.mode;
    fsCall("writeFile", [String(path), dataToBase64(data, options), flag, mode]);
  }

  function appendFileSync(path, data, options) {
    options = optionsObject(options);
    var flag = options.flag === undefined ? "a" : String(options.flag);
    fsCall("writeFile", [
      String(path),
      dataToBase64(data, options),
      flag,
      options.mode === undefined ? 438 : options.mode,
    ]);
  }

  function existsSync(path) {
    try {
      return fsCall("exists", [String(path)]);
    } catch (_) {
      return false;
    }
  }

  function readdirSync(path, options) {
    options = optionsObject(options);
    if (options.recursive) {
      var results = [];
      var base = String(path);
      var entries = readdirSync(base, { withFileTypes: true });
      for (var i = 0; i < entries.length; i++) {
        var entry = entries[i];
        var full = base + (base.charAt(base.length - 1) === "/" ? "" : "/") + entry.name;
        if (options.withFileTypes) results.push(new Dirent(entry.name, entry._type, base));
        else results.push(options.encoding === "buffer" ? Buffer.from(entry.name) : entry.name);
        if (entry.isDirectory()) {
          var nested = readdirSync(full, { recursive: true, withFileTypes: options.withFileTypes, encoding: options.encoding });
          for (var j = 0; j < nested.length; j++) {
            if (options.withFileTypes) {
              var child = nested[j];
              results.push(new Dirent(entry.name + "/" + child.name, child._type, base));
            } else {
              results.push(entry.name + "/" + nested[j]);
            }
          }
        }
      }
      return results;
    }
    var list = fsCall("readdir", [String(path), !!options.withFileTypes]);
    if (options.withFileTypes) {
      return list.map(function (entry) { return new Dirent(entry.name, entry.type, String(path)); });
    }
    return list.map(function (name) {
      return options.encoding === "buffer" ? Buffer.from(name) : name;
    });
  }

  function statSync(path, options) {
    options = optionsObject(options);
    try {
      return new Stats(fsCall("stat", [String(path)]));
    } catch (error) {
      if (options.throwIfNoEntry === false && error && error.code === "ENOENT") return undefined;
      throw error;
    }
  }

  function lstatSync(path, options) {
    options = optionsObject(options);
    try {
      return new Stats(fsCall("lstat", [String(path)]));
    } catch (error) {
      if (options.throwIfNoEntry === false && error && error.code === "ENOENT") return undefined;
      throw error;
    }
  }

  function accessSync(path, mode) {
    fsCall("access", [String(path), mode === undefined ? 0 : mode]);
  }

  function mkdtempSync(prefix, options) {
    options = optionsObject(options);
    var result = fsCall("mkdtemp", [String(prefix)]);
    return options.encoding === "buffer" ? Buffer.from(result) : result;
  }

  function unlinkSync(path) { fsCall("unlink", [String(path)]); }
  function rmdirSync(path, options) {
    options = optionsObject(options);
    if (options.recursive !== undefined) {
      throw recursiveRmdirError(options.recursive);
    }
    fsCall("rmdir", [String(path), false]);
  }

  function recursiveRmdirError(value) {
    var error = new TypeError(
      "The property 'options.recursive' is no longer supported. Received " + String(value),
    );
    error.code = "ERR_INVALID_ARG_VALUE";
    return error;
  }
  function rmSync(path, options) {
    options = optionsObject(options);
    fsCall("rm", [String(path), !!options.recursive, !!options.force]);
  }
  function mkdirSync(path, options) {
    options = optionsObject(options);
    var mode = options.mode === undefined ? 511 : options.mode;
    var result;
    try {
      result = fsCall("mkdir", [String(path), !!options.recursive, mode]);
    } catch (error) {
      if (options.recursive && error.code === "EEXIST" && statSync(path).isDirectory()) return undefined;
      throw error;
    }
    // Node returns the first directory that was created for `recursive: true`;
    // an already-existing directory resolves to undefined.
    return options.recursive ? (result === null ? undefined : result) : undefined;
  }
  function renameSync(from, to) { fsCall("rename", [String(from), String(to)]); }
  function realpathSync(path, options) {
    var result = fsCall("realpath", [String(path)]);
    return options && options.encoding === "buffer" ? Buffer.from(result) : result;
  }
  function chmodSync(path, mode) { fsCall("chmod", [String(path), typeof mode === "string" ? parseInt(mode, 8) : mode]); }
  function copyFileSync(from, to, mode) {
    fsCall("copyFile", [String(from), String(to), mode === undefined ? 0 : mode]);
  }
  function readlinkSync(path, options) {
    var result = fsCall("readlink", [String(path)]);
    return options && options.encoding === "buffer" ? Buffer.from(result) : result;
  }
  function symlinkSync(target, path, type) {
    fsCall("symlink", [String(target), String(path), type || "file"]);
  }

  function callbackStyle(fn) {
    return function () {
      var args = Array.prototype.slice.call(arguments);
      var callback = args.pop();
      if (typeof callback !== "function") callback = function () {};
      try {
        var value = fn.apply(null, args);
        queueMicrotask(function () { callback(null, value); });
      } catch (error) {
        queueMicrotask(function () { callback(error); });
      }
    };
  }

  function readFile(path, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    try {
      var value = readFileSync(path, options);
      queueMicrotask(function () { callback(null, value); });
    } catch (error) {
      queueMicrotask(function () { callback(error); });
    }
  }

  function writeFile(path, data, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    try {
      writeFileSync(path, data, options);
      queueMicrotask(function () { callback(null); });
    } catch (error) {
      queueMicrotask(function () { callback(error); });
    }
  }

  function appendFile(path, data, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    try {
      appendFileSync(path, data, options);
      queueMicrotask(function () { callback(null); });
    } catch (error) {
      queueMicrotask(function () { callback(error); });
    }
  }

  var nextFakeFd = 18;

  function fakeFd() {
    return nextFakeFd++;
  }

  function createReadStream(path, options) {
    options = optionsObject(options);
    var stream = new shims.stream.Readable();
    var encoding = options.encoding;
    var start = options.start === undefined ? 0 : options.start | 0;
    var end = options.end === undefined ? -1 : options.end | 0;
    var highWaterMark = options.highWaterMark === undefined ? 65536 : options.highWaterMark | 0;
    stream.path = String(path);
    stream.bytesRead = 0;
    stream.pending = true;
    var destroyed = false;
    if (encoding) stream.setEncoding(encoding);
    setTimeout(function () {
      if (destroyed) return;
      var bytes;
      try {
        bytes = Buffer.from(I.base64Decode(fsCall("readFile", [stream.path]).b64));
      } catch (error) {
        stream.pending = false;
        // Node emits 'error' followed by 'close' for a stream that never opened.
        queueMicrotask(function () {
          stream.emit("error", I.nodeError(error && error.__kianaError ? error : { name: error.name, message: error.message, code: error.code }));
          stream.emit("close");
        });
        return;
      }
      stream.pending = false;
      stream.emit("open", fakeFd());
      stream.emit("ready");
      if (end === -1) end = bytes.length - 1;
      var offset = start;
      function pump() {
        if (destroyed) return;
        if (offset > end || offset >= bytes.length) {
          stream.bytesRead = offset - start;
          stream.push(null);
          return;
        }
        var count = Math.min(highWaterMark, end - offset + 1, bytes.length - offset);
        var chunk = bytes.subarray(offset, offset + count);
        offset += count;
        stream.bytesRead = offset - start;
        stream.push(chunk);
        setTimeout(pump, 0);
      }
      pump();
    }, 0);
    var originalDestroy = stream.destroy;
    stream.destroy = function (error) {
      destroyed = true;
      return originalDestroy.call(stream, error);
    };
    return stream;
  }

  function createWriteStream(path, options) {
    options = optionsObject(options);
    var stream = new shims.stream.Writable();
    var chunks = [];
    var opened = false;
    var failed = false;
    var finalCallback = null;
    var fd = fakeFd();
    stream.path = String(path);
    stream.bytesWritten = 0;
    var mode = options.mode === undefined ? 438 : options.mode;

    function append(bytes) {
      fsCall("writeFile", [stream.path, I.base64Encode(bytes), "a", mode]);
    }

    stream._write = function (chunk, encoding, callback) {
      var bytes = I.toBytes(chunk);
      stream.bytesWritten += bytes.length;
      if (opened) {
        try { append(bytes); } catch (error) { callback(error); return; }
      } else {
        chunks.push(bytes);
      }
      callback();
    };
    stream._final = function (callback) {
      if (opened || failed) { callback(); return; }
      finalCallback = callback;
    };
    queueMicrotask(function () {
      if (failed) return;
      try {
        // Node truncates/creates the file when the stream opens, then streams
        // each write; the observable event order is open, ready, finish, close.
        fsCall("writeFile", [stream.path, I.base64Encode(new Uint8Array(0)), "w", mode]);
        opened = true;
        stream.emit("open", fd);
        stream.emit("ready");
        for (var i = 0; i < chunks.length; i++) append(chunks[i]);
        chunks = [];
      } catch (error) {
        failed = true;
        stream.emit("error", error);
        if (finalCallback !== null) {
          var callback = finalCallback;
          finalCallback = null;
          callback(error);
        }
        return;
      }
      if (finalCallback !== null) {
        var done = finalCallback;
        finalCallback = null;
        done();
      }
    });
    return stream;
  }

  class FSWatcher extends EventEmitter {
    constructor() {
      super();
      this._handle = null;
      this._closed = false;
    }
    close() {
      if (this._closed) return;
      this._closed = true;
      if (this._handle !== null) host.watchClose(this._handle);
      this.emit("close");
    }
    ref() { return this; }
    unref() { return this; }
  }

  function watch(path, options, listener) {
    if (typeof options === "function") {
      listener = options;
      options = {};
    }
    options = options || {};
    var text = String(path);
    // Node throws synchronously when the watched path cannot be stat'ed.
    try {
      fsCall("stat", [text]);
    } catch (error) {
      if (error && error.code === "ENOENT") {
        var failure = new Error(
          String(error.message).replace(/, stat '.*'$/, ", watch '" + text + "'"),
        );
        failure.code = error.code;
        failure.errno = error.errno;
        failure.syscall = "watch";
        failure.path = text;
        failure.filename = "";
        throw failure;
      }
      throw error;
    }
    var watcher = new FSWatcher();
    if (typeof listener === "function") watcher.on("change", listener);
    I.promise("watch", {
      path: text,
      onEvent: function (event) {
        if (event.type === "change") watcher.emit("change", event.eventType, event.filename);
        else if (event.type === "error") {
          watcher.emit("error", I.nodeError(event));
        }
      },
    }).then(function (meta) {
      watcher._handle = meta.handle;
    }, function (error) {
      watcher.emit("error", error);
    });
    return watcher;
  }

  function watchFile() { return { close: function () {} }; }
  function unwatchFile() {}

  function unsupported(name) {
    return function () {
      var error = new Error(name + " is not supported by the kiana extension runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    };
  }

  var fsObject = {
    constants: constants,
    Stats: Stats,
    Dirent: Dirent,
    FSWatcher: FSWatcher,
    readFileSync: readFileSync,
    writeFileSync: writeFileSync,
    appendFileSync: appendFileSync,
    existsSync: existsSync,
    readdirSync: readdirSync,
    statSync: statSync,
    lstatSync: lstatSync,
    fstatSync: unsupported("fs.fstatSync"),
    accessSync: accessSync,
    mkdtempSync: mkdtempSync,
    unlinkSync: unlinkSync,
    rmdirSync: rmdirSync,
    rmSync: rmSync,
    mkdirSync: mkdirSync,
    renameSync: renameSync,
    realpathSync: realpathSync,
    chmodSync: chmodSync,
    copyFileSync: copyFileSync,
    readlinkSync: readlinkSync,
    symlinkSync: symlinkSync,
    readFile: readFile,
    writeFile: writeFile,
    appendFile: appendFile,
    access: callbackStyle(function (path, mode) { return accessSync(path, mode); }),
    mkdtemp: callbackStyle(function (prefix, options) { return mkdtempSync(prefix, options); }),
    unlink: callbackStyle(function (path) { return unlinkSync(path); }),
    rmdir: function (path, options, callback) {
      if (typeof options === "function") { callback = options; options = undefined; }
      try { rmdirSync(path, options); callback(null); } catch (error) { callback(error); }
    },
    rm: function (path, options, callback) {
      if (typeof options === "function") { callback = options; options = undefined; }
      try { rmSync(path, options); callback(null); } catch (error) { callback(error); }
    },
    mkdir: function (path, options, callback) {
      if (typeof options === "function") { callback = options; options = undefined; }
      try { callback(null, mkdirSync(path, options)); } catch (error) { callback(error); }
    },
    rename: callbackStyle(function (from, to) { return renameSync(from, to); }),
    realpath: callbackStyle(function (path, options) { return realpathSync(path, options); }),
    chmod: callbackStyle(function (path, mode) { return chmodSync(path, mode); }),
    copyFile: callbackStyle(function (from, to, mode) { return copyFileSync(from, to, mode); }),
    readlink: callbackStyle(function (path, options) { return readlinkSync(path, options); }),
    symlink: callbackStyle(function (target, path, type) { return symlinkSync(target, path, type); }),
    stat: callbackStyle(function (path, options) { return statSync(path, options); }),
    lstat: callbackStyle(function (path, options) { return lstatSync(path, options); }),
    createReadStream: createReadStream,
    createWriteStream: createWriteStream,
    watch: watch,
    watchFile: watchFile,
    unwatchFile: unwatchFile,
    openSync: unsupported("fs.openSync"),
    closeSync: unsupported("fs.closeSync"),
    readSync: unsupported("fs.readSync"),
    writeSync: unsupported("fs.writeSync"),
    readvSync: unsupported("fs.readvSync"),
    writevSync: unsupported("fs.writevSync"),
  };

  Object.defineProperty(fsObject, "promises", {
    enumerable: true,
    configurable: true,
    get: function () { return shims["fs/promises"]; },
  });

  shims.fs = fsObject;
  shims["node:fs"] = fsObject;
})();
