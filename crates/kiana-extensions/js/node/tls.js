// node:tls — not provided (design §6.1/§7.4 item 9): every entry point throws
// ERR_UNSUPPORTED so callers see a clear failure instead of a silent undefined.
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;

  function unsupported(name) {
    return function () {
      var error = new Error(name + " is not supported (TLS is not available in this runtime)");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    };
  }

  var tlsObject = {
    connect: unsupported("tls.connect"),
    createServer: unsupported("tls.createServer"),
    createSecureContext: unsupported("tls.createSecureContext"),
    TLSSocket: class TLSSocket {},
    DEFAULT_MIN_VERSION: "TLSv1.2",
    DEFAULT_MAX_VERSION: "TLSv1.3",
  };

  shims.tls = tlsObject;
  shims["node:tls"] = tlsObject;
})();
