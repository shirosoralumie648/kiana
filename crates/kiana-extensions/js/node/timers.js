// node:timers + node:timers/promises
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;

  function setTimeoutPromise(delay, value, options) {
    return new Promise(function (resolve, reject) {
      if (options && options.signal) {
        if (options.signal.aborted) {
          reject(new DOMException("The operation was aborted", "AbortError"));
          return;
        }
        options.signal.addEventListener("abort", function () {
          reject(new DOMException("The operation was aborted", "AbortError"));
        });
      }
      globalThis.setTimeout(function () { resolve(value); }, delay);
    });
  }

  var timersObject = {
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
    setInterval: globalThis.setInterval,
    clearInterval: globalThis.clearInterval,
    setImmediate: globalThis.setImmediate,
    clearImmediate: globalThis.clearImmediate,
    promises: {
      setTimeout: setTimeoutPromise,
      setImmediate: function (value) {
        return new Promise(function (resolve) { globalThis.setImmediate(function () { resolve(value); }); });
      },
      setInterval: function () {},
      scheduler: {
        wait: function (delay) { return setTimeoutPromise(delay); },
        yield: function () { return Promise.resolve(); },
      },
    },
  };

  shims.timers = timersObject;
  shims["node:timers"] = timersObject;
  shims["timers/promises"] = timersObject.promises;
  shims["node:timers/promises"] = timersObject.promises;
})();
