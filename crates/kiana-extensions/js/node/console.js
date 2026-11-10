// node:console — the global console object.
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;
  shims.console = globalThis.console;
  shims["node:console"] = globalThis.console;
})();
