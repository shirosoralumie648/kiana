// node:util — promisify/format/inspect/types (design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  var promisifyCustom = Symbol.for("nodejs.util.promisify.custom");

  function promisify(original) {
    if (typeof original !== "function") {
      var typeError = new TypeError("The \"original\" argument must be of type function. Received " + typeof original);
      typeError.code = "ERR_INVALID_ARG_TYPE";
      throw typeError;
    }
    if (original[promisifyCustom] !== undefined) return original[promisifyCustom];
    function fn() {
      var args = Array.prototype.slice.call(arguments);
      var self = this;
      return new Promise(function (resolve, reject) {
        args.push(function (error, value) {
          if (error) reject(error);
          else resolve(value);
        });
        original.apply(self, args);
      });
    }
    Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
    Object.defineProperty(fn, promisifyCustom, { value: original[promisifyCustom] });
    return fn;
  }
  promisify.custom = promisifyCustom;

  function callbackify(original) {
    return function () {
      var args = Array.prototype.slice.call(arguments);
      var callback = args.pop();
      original.apply(this, args).then(
        function (value) { callback(null, value); },
        function (error) { callback(error); },
      );
    };
  }

  function inherits(constructor, superConstructor) {
    Object.setPrototypeOf(constructor.prototype, superConstructor.prototype);
    Object.setPrototypeOf(constructor, superConstructor);
    constructor.super_ = superConstructor;
  }

  function deprecate(fn, message, code) {
    var warned = false;
    return function () {
      if (!warned) {
        warned = true;
        var error = new Error(message);
        error.code = code || "DEP0000";
        globalThis.console.warn("DeprecationWarning: " + message);
        void error;
      }
      return fn.apply(this, arguments);
    };
  }

  function isDeepStrictEqual(a, b) {
    var seen = new Map();
    function equal(left, right) {
      if (Object.is(left, right)) return true;
      if (typeof left !== typeof right) return false;
      if (left === null || right === null) return false;
      if (typeof left !== "object") return false;
      if (seen.get(left) === right) return true;
      seen.set(left, right);
      var leftTag = Object.prototype.toString.call(left);
      var rightTag = Object.prototype.toString.call(right);
      if (leftTag !== rightTag) return false;
      if (left instanceof Date) return left.getTime() === right.getTime();
      if (left instanceof RegExp) return String(left) === String(right);
      if (left instanceof ArrayBuffer) {
        if (left.byteLength !== right.byteLength) return false;
        var lb = new Uint8Array(left);
        var rb = new Uint8Array(right);
        for (var i = 0; i < lb.length; i++) if (lb[i] !== rb[i]) return false;
        return true;
      }
      if (ArrayBuffer.isView(left)) {
        if (left.byteLength !== right.byteLength) return false;
        var lv = new Uint8Array(left.buffer, left.byteOffset, left.byteLength);
        var rv = new Uint8Array(right.buffer, right.byteOffset, right.byteLength);
        for (var j = 0; j < lv.length; j++) if (lv[j] !== rv[j]) return false;
        return true;
      }
      if (left instanceof Map) {
        if (left.size !== right.size) return false;
        var ok = true;
        left.forEach(function (value, key) {
          if (!right.has(key) || !equal(value, right.get(key))) ok = false;
        });
        return ok;
      }
      if (left instanceof Set) {
        if (left.size !== right.size) return false;
        var okSet = true;
        left.forEach(function (value) {
          if (!right.has(value)) okSet = false;
        });
        return okSet;
      }
      var leftKeys = Object.keys(left);
      var rightKeys = Object.keys(right);
      if (leftKeys.length !== rightKeys.length) return false;
      for (var k = 0; k < leftKeys.length; k++) {
        var key = leftKeys[k];
        if (!Object.prototype.hasOwnProperty.call(right, key)) return false;
        if (!equal(left[key], right[key])) return false;
      }
      return true;
    }
    return equal(a, b);
  }

  var types = {
    isPromise: function (value) { return value instanceof Promise; },
    isDate: function (value) { return value instanceof Date; },
    isRegExp: function (value) { return value instanceof RegExp; },
    isTypedArray: function (value) { return ArrayBuffer.isView(value) && !(value instanceof DataView); },
    isArrayBuffer: function (value) { return value instanceof ArrayBuffer; },
    isArrayBufferView: function (value) { return ArrayBuffer.isView(value); },
    isMap: function (value) { return value instanceof Map; },
    isSet: function (value) { return value instanceof Set; },
    isWeakMap: function (value) { return value instanceof WeakMap; },
    isWeakSet: function (value) { return value instanceof WeakSet; },
    isNativeError: function (value) { return value instanceof Error; },
    isError: function (value) { return value instanceof Error; },
    isBoxedPrimitive: function () { return false; },
    isBigIntObject: function (value) { return typeof value === "object" && Object.prototype.toString.call(value) === "[object BigInt]"; },
    isSymbolObject: function (value) { return typeof value === "object" && Object.prototype.toString.call(value) === "[object Symbol]"; },
    isNumberObject: function (value) { return typeof value === "object" && Object.prototype.toString.call(value) === "[object Number]"; },
    isStringObject: function (value) { return typeof value === "object" && Object.prototype.toString.call(value) === "[object String]"; },
    isBooleanObject: function (value) { return typeof value === "object" && Object.prototype.toString.call(value) === "[object Boolean]"; },
    isAsyncFunction: function (value) { return typeof value === "function" && value.constructor && value.constructor.name === "AsyncFunction"; },
    isGeneratorFunction: function (value) { return typeof value === "function" && value.constructor && value.constructor.name === "GeneratorFunction"; },
    isGeneratorObject: function (value) { return value !== null && typeof value === "object" && typeof value.next === "function" && typeof value.throw === "function"; },
    isProxy: function () { return false; },
  };

  var ERRNO_MESSAGES = {
    ENOENT: "no such file or directory",
    EACCES: "permission denied",
    EEXIST: "file already exists",
    ENOTDIR: "not a directory",
    EISDIR: "illegal operation on a directory",
    ENOTEMPTY: "directory not empty",
    EPERM: "operation not permitted",
    ECONNREFUSED: "connection refused",
    ECONNRESET: "connection reset by peer",
    EADDRINUSE: "address already in use",
    ETIMEDOUT: "connection timed out",
    EPIPE: "broken pipe",
    ESRCH: "no such process",
    EINVAL: "invalid argument",
  };

  // Linux/libuv errno 名（Node 的 getSystemErrorName 只接受负整数）。
  var SYSTEM_ERROR_NAMES = {
    "-1": "EPERM", "-2": "ENOENT", "-3": "ESRCH", "-4": "EINTR",
    "-5": "EIO", "-6": "ENXIO", "-7": "E2BIG", "-8": "ENOEXEC",
    "-9": "EBADF", "-10": "ECHILD", "-11": "EAGAIN", "-12": "ENOMEM",
    "-13": "EACCES", "-14": "EFAULT", "-15": "ENOTBLK", "-16": "EBUSY",
    "-17": "EEXIST", "-18": "EXDEV", "-19": "ENODEV", "-20": "ENOTDIR",
    "-21": "EISDIR", "-22": "EINVAL", "-23": "ENFILE", "-24": "EMFILE",
    "-25": "ENOTTY", "-26": "ETXTBSY", "-27": "EFBIG", "-28": "ENOSPC",
    "-29": "ESPIPE", "-30": "EROFS", "-31": "EMLINK", "-32": "EPIPE",
    "-33": "EDOM", "-34": "ERANGE", "-35": "EDEADLK", "-36": "ENAMETOOLONG",
    "-37": "ENOLCK", "-38": "ENOSYS", "-39": "ENOTEMPTY", "-40": "ELOOP",
    "-71": "EPROTO", "-75": "EOVERFLOW", "-88": "ENOTSOCK",
    "-89": "EDESTADDRREQ", "-90": "EMSGSIZE", "-91": "EPROTOTYPE",
    "-92": "ENOPROTOOPT", "-93": "EPROTONOSUPPORT", "-94": "ESOCKTNOSUPPORT",
    "-95": "EOPNOTSUPP", "-96": "EPFNOSUPPORT", "-97": "EAFNOSUPPORT",
    "-98": "EADDRINUSE", "-99": "EADDRNOTAVAIL", "-100": "ENETDOWN",
    "-101": "ENETUNREACH", "-102": "ENETRESET", "-103": "ECONNABORTED",
    "-104": "ECONNRESET", "-105": "ENOBUFS", "-106": "EISCONN",
    "-107": "ENOTCONN", "-108": "ESHUTDOWN", "-109": "ETOOMANYREFS",
    "-110": "ETIMEDOUT", "-111": "ECONNREFUSED", "-112": "EHOSTDOWN",
    "-113": "EHOSTUNREACH", "-114": "EALREADY", "-115": "EINPROGRESS",
    "-116": "ESTALE", "-122": "EDQUOT", "-125": "ECANCELED",
  };

  function getSystemErrorName(errno) {
    if (typeof errno !== "number") {
      var typeError = new TypeError(
        "The \"err\" argument must be of type number. Received " +
        (typeof errno === "string" ? "type string ('" + errno + "')" : typeof errno),
      );
      typeError.code = "ERR_INVALID_ARG_TYPE";
      throw typeError;
    }
    if (!Number.isInteger(errno) || errno >= 0) {
      var rangeError = new RangeError(
        "The value of \"err\" is out of range. It must be a negative integer. Received " + errno,
      );
      rangeError.code = "ERR_OUT_OF_RANGE";
      throw rangeError;
    }
    return SYSTEM_ERROR_NAMES[String(errno)] || "Unknown system error " + errno;
  }

  function getSystemErrorMap() {
    var map = new Map();
    Object.keys(ERRNO_MESSAGES).forEach(function (code) { map.set(code, [code, ERRNO_MESSAGES[code]]); });
    return map;
  }

  function debuglog() { return function () {}; }

  function parseArgs(config) {
    config = config || {};
    var args = config.args || [];
    var options = config.options || {};
    var values = { _: [] };
    var positionals = 0;
    for (var i = 0; i < args.length; i++) {
      var arg = args[i];
      if (arg.slice(0, 2) === "--") {
        var body = arg.slice(2);
        var eq = body.indexOf("=");
        var name = eq === -1 ? body : body.slice(0, eq);
        var inline = eq === -1 ? undefined : body.slice(eq + 1);
        var definition = options[name];
        if (definition === undefined) {
          if (config.strict) {
            var error = new Error("Unknown option '--" + name + "'");
            error.code = "ERR_PARSE_ARGS_UNKNOWN_OPTION";
            throw error;
          }
          continue;
        }
        var target = definition.multiple ? (values[name] = values[name] || []) : undefined;
        var value = inline;
        if (value === undefined) {
          if (definition.type === "boolean") value = true;
          else value = args[++i];
        }
        if (definition.multiple) target.push(value);
        else values[name] = value;
      } else if (arg.charAt(0) === "-" && arg.length > 1) {
        var short = arg.slice(1);
        var def = options[short];
        if (def === undefined) {
          if (config.strict) {
            var shortError = new Error("Unknown option '-" + short + "'");
            shortError.code = "ERR_PARSE_ARGS_UNKNOWN_OPTION";
            throw shortError;
          }
          continue;
        }
        values[short] = def.type === "boolean" ? true : args[++i];
      } else {
        values._.push(arg);
        positionals++;
      }
    }
    void positionals;
    return values;
  }

  function stripVTControlCharacters(text) {
    return String(text).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "").replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "");
  }

  var utilObject = {
    promisify: promisify,
    callbackify: callbackify,
    inherits: inherits,
    deprecate: deprecate,
    inspect: I.inspect,
    format: I.format,
    formatWithOptions: function (options) {
      var args = Array.prototype.slice.call(arguments, 1);
      return I.format.apply(null, args);
    },
    isDeepStrictEqual: isDeepStrictEqual,
    types: types,
    TextEncoder: globalThis.TextEncoder,
    TextDecoder: globalThis.TextDecoder,
    debuglog: debuglog,
    getSystemErrorName: getSystemErrorName,
    getSystemErrorMap: getSystemErrorMap,
    parseArgs: parseArgs,
    stripVTControlCharacters: stripVTControlCharacters,
    toUSVString: function (value) { return String(value).toWellFormed ? String(value).toWellFormed() : String(value); },
    transferableAbortSignal: function (signal) { return signal; },
    aborted: function () { return new Promise(function () {}); },
    deprecate2: undefined,
  };
  utilObject.inspect.defaultOptions = { depth: 2 };
  utilObject.inspect.custom = Symbol.for("nodejs.util.inspect.custom");

  shims.util = utilObject;
  shims["node:util"] = utilObject;
  shims["util/types"] = types;
  shims["node:util/types"] = types;
})();
