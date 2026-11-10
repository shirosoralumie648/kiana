// node:string_decoder
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  class StringDecoder {
    constructor(encoding) {
      this.encoding = I.normalizeEncoding(encoding === undefined ? "utf8" : encoding);
      this._pending = new Uint8Array(0);
      this.lastChar = "";
    }
    write(buffer) {
      var bytes = typeof buffer === "string" ? I.stringToBytes(buffer) : I.toBytes(buffer);
      var combined = new Uint8Array(this._pending.length + bytes.length);
      combined.set(this._pending, 0);
      combined.set(bytes, this._pending.length);
      var decoded = I.decodeUTF8(combined, false, true);
      this._pending = decoded.rest;
      return decoded.text;
    }
    end(buffer) {
      var text = buffer === undefined ? "" : this.write(buffer);
      return text + I.decodeUTF8(this._pending, false, false).text;
    }
  }

  var stringDecoderObject = { StringDecoder: StringDecoder };
  shims.string_decoder = stringDecoderObject;
  shims["node:string_decoder"] = stringDecoderObject;
})();
