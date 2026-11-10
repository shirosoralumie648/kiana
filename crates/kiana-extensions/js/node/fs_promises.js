// node:fs/promises — every operation is a `__kiana.promise("fs", {op, args})`
// host call (spawn_local + completion queue; design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;
  var fsObject = shims.fs;

  function optionsObject(options) {
    if (options === undefined || options === null) return {};
    if (typeof options === "string") return { encoding: options };
    return options;
  }

  function call(operation, args) {
    return I.promise("fs", { op: operation, args: args });
  }

  function validateEncoding(encoding) {
    if (encoding === undefined || encoding === null) return;
    if (typeof encoding !== "string" || !Buffer.isEncoding(encoding)) {
      var error = new TypeError(
        "The argument 'encoding' is invalid encoding. Received " +
        (typeof encoding === "string" ? "'" + encoding + "'" : String(encoding)),
      );
      error.code = "ERR_INVALID_ARG_VALUE";
      throw error;
    }
  }

  function decodeFile(result, options) {
    options = optionsObject(options);
    var bytes = Buffer.from(I.base64Decode(result.b64));
    if (options.encoding === undefined || options.encoding === null) return bytes;
    validateEncoding(options.encoding);
    return bytes.toString(options.encoding);
  }

  function dataToBase64(data, options) {
    options = optionsObject(options);
    if (typeof data === "string") {
      validateEncoding(options.encoding);
      return I.base64Encode(I.stringToBytes(data, options.encoding));
    }
    if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) return I.base64Encode(I.toBytes(data));
    if (data && typeof data === "object" && typeof data.length === "number") {
      return I.base64Encode(new Uint8Array(Array.prototype.slice.call(data)));
    }
    throw new TypeError("The \"data\" argument must be of type string or an instance of Buffer, TypedArray, or DataView.");
  }

  var promises = {
    constants: fsObject.constants,
    readFile: function (path, options) {
      return call("readFile", [String(path)]).then(function (result) {
        return decodeFile(result, options);
      });
    },
    writeFile: function (path, data, options) {
      options = optionsObject(options);
      var flag = options.flag === undefined ? "w" : String(options.flag);
      return call("writeFile", [
        String(path),
        dataToBase64(data, options),
        flag,
        options.mode === undefined ? 438 : options.mode,
      ]).then(function () {});
    },
    appendFile: function (path, data, options) {
      options = optionsObject(options);
      return call("writeFile", [
        String(path),
        dataToBase64(data, options),
        options.flag === undefined ? "a" : String(options.flag),
        options.mode === undefined ? 438 : options.mode,
      ]).then(function () {});
    },
    readdir: function (path, options) {
      options = optionsObject(options);
      var root = String(path);
      function list(dir, withTypes) {
        return call("readdir", [dir, withTypes]);
      }
      function wrap(entry) {
        if (typeof entry === "string") {
          return options.encoding === "buffer" ? Buffer.from(entry) : entry;
        }
        if (options.withFileTypes) {
          return new shims.fs.Dirent(entry.name, entry.type, root);
        }
        return options.encoding === "buffer" ? Buffer.from(entry.name) : entry.name;
      }
      if (!options.recursive) {
        return list(root, !!options.withFileTypes).then(function (entries) {
          return entries.map(wrap);
        });
      }
      // Node's recursive readdir reports each directory's own entries first
      // and then descends (breadth-first per level).
      var collected = [];
      function walk(dir, prefix) {
        return list(dir, true).then(function (entries) {
          var subdirectories = [];
          for (var i = 0; i < entries.length; i++) {
            var entry = entries[i];
            var name = prefix === null ? entry.name : prefix + "/" + entry.name;
            if (options.withFileTypes) {
              collected.push(new shims.fs.Dirent(name, entry.type, root));
            } else {
              collected.push(options.encoding === "buffer" ? Buffer.from(name) : name);
            }
            if (entry.type === "dir") subdirectories.push([dir, name, entry.name]);
          }
          var chain = Promise.resolve();
          for (var j = 0; j < subdirectories.length; j++) {
            (function (item) {
              chain = chain.then(function () {
                var childDir = item[0] + (item[0].charAt(item[0].length - 1) === "/" ? "" : "/") + item[2];
                return walk(childDir, item[1]);
              });
            })(subdirectories[j]);
          }
          return chain;
        });
      }
      return walk(root, null).then(function () { return collected; });
    },
    stat: function (path, options) {
      options = optionsObject(options);
      return call("stat", [String(path)]).then(function (json) {
        return new shims.fs.Stats(json);
      }, function (error) {
        if (options.throwIfNoEntry === false && error && error.code === "ENOENT") return undefined;
        throw error;
      });
    },
    lstat: function (path, options) {
      options = optionsObject(options);
      return call("lstat", [String(path)]).then(function (json) {
        return new shims.fs.Stats(json);
      }, function (error) {
        if (options.throwIfNoEntry === false && error && error.code === "ENOENT") return undefined;
        throw error;
      });
    },
    access: function (path, mode) {
      return call("access", [String(path), mode === undefined ? 0 : mode]).then(function () {});
    },
    mkdir: function (path, options) {
      options = optionsObject(options);
      return call("mkdir", [
        String(path),
        !!options.recursive,
        options.mode === undefined ? 511 : options.mode,
      ]).then(function (result) {
        return options.recursive && result !== null ? result : undefined;
      });
    },
    rmdir: function (path, options) {
      options = optionsObject(options);
      if (options.recursive !== undefined) {
        var error = new TypeError(
          "The property 'options.recursive' is no longer supported. Received " + String(options.recursive),
        );
        error.code = "ERR_INVALID_ARG_VALUE";
        return Promise.reject(error);
      }
      return call("rmdir", [String(path), false]).then(function () {});
    },
    rm: function (path, options) {
      options = optionsObject(options);
      return call("rm", [String(path), !!options.recursive, !!options.force]).then(function () {});
    },
    unlink: function (path) {
      return call("unlink", [String(path)]).then(function () {});
    },
    rename: function (from, to) {
      return call("rename", [String(from), String(to)]).then(function () {});
    },
    realpath: function (path, options) {
      return call("realpath", [String(path)]).then(function (result) {
        return options && options.encoding === "buffer" ? Buffer.from(result) : result;
      });
    },
    chmod: function (path, mode) {
      return call("chmod", [String(path), typeof mode === "string" ? parseInt(mode, 8) : mode]).then(function () {});
    },
    copyFile: function (from, to, mode) {
      return call("copyFile", [String(from), String(to), mode === undefined ? 0 : mode]).then(function () {});
    },
    mkdtemp: function (prefix, options) {
      return call("mkdtemp", [String(prefix)]).then(function (result) {
        return options && options.encoding === "buffer" ? Buffer.from(result) : result;
      });
    },
    readlink: function (path, options) {
      return call("readlink", [String(path)]).then(function (result) {
        return options && options.encoding === "buffer" ? Buffer.from(result) : result;
      });
    },
    symlink: function (target, path, type) {
      return call("symlink", [String(target), String(path), type || "file"]).then(function () {});
    },
    open: function () {
      var error = new Error("fs.promises.open is not supported by the kiana extension runtime");
      error.code = "ERR_UNSUPPORTED";
      return Promise.reject(error);
    },
  };

  shims["fs/promises"] = promises;
  shims["node:fs/promises"] = promises;
})();
