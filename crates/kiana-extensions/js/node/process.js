// node:process — the global process object is the module export.
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;
  shims.process = globalThis.process;
  shims["node:process"] = globalThis.process;
  shims["node:process"].default = globalThis.process;
})();
