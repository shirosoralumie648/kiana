// node:tty
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;

  var ttyObject = {
    isatty: function (fd) { return fd === 1 || fd === 2; },
    isTTY: true,
    ReadStream: class ReadStream {},
    WriteStream: class WriteStream {},
  };

  shims.tty = ttyObject;
  shims["node:tty"] = ttyObject;
})();
