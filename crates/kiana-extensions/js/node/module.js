// node:module — createRequire backed by the loader's `__kiana_make_require`, builtin
// introspection (design §6.1; doom-overlay).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  var BUILTINS = [
    "_http_agent", "_http_client", "_http_common", "_http_incoming", "_http_outgoing",
    "_http_server", "_tls_common", "_tls_wrap", "assert", "assert/strict", "async_hooks",
    "buffer", "child_process", "cluster", "console", "constants", "crypto", "dgram",
    "diagnostics_channel", "dns", "dns/promises", "domain", "events", "fs", "fs/promises",
    "http", "http2", "https", "inspector", "inspector/promises", "module", "net", "os",
    "path", "path/posix", "path/win32", "perf_hooks", "process", "punycode", "querystring",
    "readline", "readline/promises", "repl", "stream", "stream/consumers", "stream/promises",
    "stream/web", "string_decoder", "sys", "timers", "timers/promises", "tls",
    "trace_events", "tty", "url", "util", "util/types", "v8", "vm", "wasi", "worker_threads",
    "zlib",
  ];

  function pathFromFilename(filename) {
    var text = String(filename);
    if (text.slice(0, 5) === "file:") return I.host.fileURLToPath(text);
    return text;
  }

  function dirname(path) {
    var index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    if (index === -1) return ".";
    if (index === 0) return path.slice(0, 1);
    var parent = path.slice(0, index);
    if (/^[A-Za-z]:$/.test(parent)) return parent + path.charAt(index);
    return parent;
  }

  function nodeModulePaths(from) {
    var path = shims.path ? shims.path.resolve(from) : from;
    var parts = path.split(/[\\/]/).filter(function (part) { return part.length > 0; });
    var paths = [];
    for (var i = parts.length; i > 0; i--) {
      paths.push(parts.slice(0, i).join("/") + "/node_modules");
    }
    paths.push("/node_modules");
    return paths;
  }

  function fallbackRequire(from, dir) {
    function req(spec) {
      var key = String(spec);
      var candidates = shims[key] !== undefined
        ? [key]
        : ["node:" + key.replace(/^node:/, "")];
      for (var i = 0; i < candidates.length; i++) {
        var candidate = candidates[i];
        if (shims[candidate] !== undefined) return shims[candidate];
      }
      var error = new Error("Cannot find module '" + key + "'\nRequire stack:\n- " + from);
      error.code = "MODULE_NOT_FOUND";
      throw error;
    }
    req.resolve = function (spec) {
      var key = String(spec);
      if (shims[key] !== undefined) return "node:" + key.replace(/^node:/, "");
      var error = new Error("Cannot find module '" + key + "'\nRequire stack:\n- " + from);
      error.code = "MODULE_NOT_FOUND";
      throw error;
    };
    req.cache = {};
    req.main = undefined;
    void dir;
    return req;
  }

  function createRequire(filename) {
    var from = pathFromFilename(filename);
    var dir = dirname(from);
    if (typeof globalThis.__kiana_make_require === "function") {
      return globalThis.__kiana_make_require(from, dir);
    }
    return fallbackRequire(from, dir);
  }

  function isBuiltin(name) {
    var text = String(name);
    if (text.slice(0, 5) === "node:") return shims[text] !== undefined || BUILTINS.indexOf(text.slice(5)) !== -1;
    return BUILTINS.indexOf(text) !== -1 || shims["node:" + text] !== undefined;
  }

  class Module {
    constructor(id) {
      this.id = id;
      this.exports = {};
      this.loaded = false;
      this.filename = id;
    }
    static createRequire(filename) { return createRequire(filename); }
    require(spec) { return createRequire(this.filename)(spec); }
    static _nodeModulePaths(from) { return nodeModulePaths(from); }
    static _resolveFilename(request) {
      return globalThis.__kiana_host ? globalThis.__kiana_host.resolve(process.cwd() + "/noop.js", request) : request;
    }
    static _load() { throw new Error("Module._load is not supported"); }
  }

  var moduleObject = {
    Module: Module,
    createRequire: createRequire,
    builtinModules: BUILTINS,
    isBuiltin: isBuiltin,
    syncBuiltinESMExports: function () {},
    findSourceMap: function () { return undefined; },
    SourceMap: class SourceMap {},
    register: function () {
      var error = new Error("module.register is not supported by the kiana extension runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    },
    registerHooks: function () {
      var error = new Error("module.registerHooks is not supported by the kiana extension runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    },
    _resolveFilename: Module._resolveFilename,
    _nodeModulePaths: nodeModulePaths,
  };

  shims.module = moduleObject;
  shims["node:module"] = moduleObject;
})();
