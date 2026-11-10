// node:https — TLS is not provided (design §7.4 item 9); operations report
// ERR_UNSUPPORTED while keeping Node's object shapes.
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;
  var EventEmitter = shims.events.EventEmitter;

  function unsupportedError(operation) {
    var error = new Error(operation + " is not supported (TLS is not available in this runtime)");
    error.code = "ERR_UNSUPPORTED";
    return error;
  }

  function request(options, callback) {
    var emitter = new EventEmitter();
    emitter.setHeader = function () { return emitter; };
    emitter.getHeader = function () { return undefined; };
    emitter.removeHeader = function () {};
    emitter.write = function () { return true; };
    emitter.end = function () {
      queueMicrotask(function () {
        var error = unsupportedError("https.request");
        emitter.emit("error", error);
      });
      return emitter;
    };
    emitter.abort = function () {};
    emitter.destroy = function () {};
    if (typeof callback === "function") emitter.once("response", callback);
    return emitter;
  }

  function get(options, callback) {
    var req = request(options, callback);
    req.end();
    return req;
  }

  function createServer() {
    throw unsupportedError("https.createServer");
  }

  class Agent {
    constructor(options) { this.options = options || {}; }
    destroy() {}
  }

  var httpsObject = {
    createServer: createServer,
    Server: class Server extends EventEmitter {},
    Agent: Agent,
    globalAgent: null,
    request: request,
    get: get,
  };
  httpsObject.globalAgent = new httpsObject.Agent();

  shims.https = httpsObject;
  shims["node:https"] = httpsObject;
})();
