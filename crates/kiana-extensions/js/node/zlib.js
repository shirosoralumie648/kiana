// node:zlib — synchronous compression backed by flate2 (design §6.1).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;

  function runSync(operation, data) {
    var bytes = typeof data === "string" ? I.stringToBytes(data) : I.toBytes(data);
    return Buffer.from(I.base64Decode(host.zlib(operation, I.base64Encode(bytes))));
  }

  function withEncoding(result, options) {
    // Node 的 *Sync 永远返回 Buffer（options.encoding 不是 zlib 选项）。
    void options;
    return result;
  }

  function zlibError(message, code, errno) {
    var error = new Error(message);
    if (code !== undefined) error.code = code;
    if (errno !== undefined) error.errno = errno;
    return error;
  }

  function checkGzip(data) {
    var bytes = typeof data === "string" ? I.stringToBytes(data) : I.toBytes(data);
    if (bytes.length < 2 || bytes[0] !== 0x1f || bytes[1] !== 0x8b) {
      throw zlibError("incorrect header check", "Z_DATA_ERROR", -3);
    }
    return data;
  }

  function mapZlibError(operation, data, error) {
    var text = error && error.message ? error.message : String(error);
    return zlibError(text.replace(/^[a-z-]+ failed: /, ""), "Z_DATA_ERROR", -3);
  }

  var constants = {
    Z_NO_FLUSH: 0, Z_PARTIAL_FLUSH: 1, Z_SYNC_FLUSH: 2, Z_FULL_FLUSH: 3,
    Z_FINISH: 4, Z_BLOCK: 5, Z_OK: 0, Z_STREAM_END: 1, Z_NEED_DICT: 2,
    Z_ERRNO: -1, Z_STREAM_ERROR: -2, Z_DATA_ERROR: -3, Z_MEM_ERROR: -4,
    Z_BUF_ERROR: -5, Z_VERSION_ERROR: -6, Z_NO_COMPRESSION: 0,
    Z_BEST_SPEED: 1, Z_BEST_COMPRESSION: 9, Z_DEFAULT_COMPRESSION: -1,
  };

  function unsupported(name) {
    return function () {
      var error = new Error(name + " is not supported in this runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    };
  }

  function guarded(operation, data, options, check) {
    try {
      if (check) checkGzip(data);
      return withEncoding(runSync(operation, data), options);
    } catch (error) {
      if (error && error.code === "Z_DATA_ERROR") throw error;
      throw mapZlibError(operation, data, error);
    }
  }

  var zlibObject = {
    gunzipSync: function (data, options) { return guarded("gunzip", data, options, true); },
    gzipSync: function (data, options) { return guarded("gzip", data, options, false); },
    inflateSync: function (data, options) { return guarded("inflate", data, options, false); },
    deflateSync: function (data, options) { return guarded("deflate", data, options, false); },
    inflateRawSync: function (data, options) { return guarded("inflate-raw", data, options, false); },
    deflateRawSync: function (data, options) { return guarded("deflate-raw", data, options, false); },
    unzipSync: function (data, options) {
      var bytes = typeof data === "string" ? I.stringToBytes(data) : I.toBytes(data);
      var operation = bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b
        ? "gunzip"
        : "inflate";
      return guarded(operation, data, options, operation === "gunzip");
    },
    brotliDecompressSync: unsupported("zlib.brotliDecompressSync"),
    brotliCompressSync: unsupported("zlib.brotliCompressSync"),
    createGunzip: unsupported("zlib.createGunzip"),
    createGzip: unsupported("zlib.createGzip"),
    createInflate: unsupported("zlib.createInflate"),
    createDeflate: unsupported("zlib.createDeflate"),
    constants: constants,
  };

  shims.zlib = zlibObject;
  shims["node:zlib"] = zlibObject;
})();
