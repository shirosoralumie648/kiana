// node:buffer
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;

  var bufferObject = {
    Buffer: Buffer,
    SlowBuffer: Buffer,
    constants: { MAX_LENGTH: 4294967296, MAX_STRING_LENGTH: 536870888 },
    kMaxLength: 4294967296,
    INSPECT_MAX_BYTES: 50,
    isUtf8: function (input) {
      try {
        var bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
        var decoder = new TextDecoder("utf-8", { fatal: true });
        decoder.decode(bytes);
        return true;
      } catch (_) {
        return false;
      }
    },
    transcode: function () {
      var error = new Error("buffer.transcode is not supported by the kiana extension runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    },
    atob: globalThis.atob,
    btoa: globalThis.btoa,
    Blob: globalThis.Blob,
    File: globalThis.File,
  };

  shims.buffer = bufferObject;
  shims["node:buffer"] = bufferObject;
})();
