// Node globals for the kiana extension runtime (spec/design-extension-runtime.md §6.2).
//
// Installed by `kiana_extensions::loader::js_host::install` after `js/intl.js` and
// `js/prelude_core.js`. Everything here is a real JS object; Rust only supplies
// the `__kiana_host.*` primitives:
//
//   console, process, Buffer, TextEncoder/TextDecoder, URL/URLSearchParams,
//   fetch (Response/Request/Headers/ReadableStream), crypto, performance,
//   atob/btoa, structuredClone, Web Streams, timers glue.
//
// Design §9.3: this file only depends on `__kiana_host` / `__kiana` and imports no
// virtual module. `__kiana_node_shims` (filled by the node/*.js files) is looked up
// lazily so console/util never capture a stale reference.

(function () {
  "use strict";

  var host = globalThis.__kiana_host;

  // ---------------------------------------------------------------------
  // UTF-8 (TextEncoder/TextDecoder + Buffer)
  // ---------------------------------------------------------------------

  function encodeUTF8(text) {
    text = String(text);
    var out = [];
    for (var i = 0; i < text.length; i++) {
      var code = text.charCodeAt(i);
      if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
        var low = text.charCodeAt(i + 1);
        if (low >= 0xdc00 && low <= 0xdfff) {
          code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
          i++;
        }
      }
      if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd;
      if (code < 0x80) {
        out.push(code);
      } else if (code < 0x800) {
        out.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
      } else if (code < 0x10000) {
        out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
      } else {
        out.push(
          0xf0 | (code >> 18),
          0x80 | ((code >> 12) & 0x3f),
          0x80 | ((code >> 6) & 0x3f),
          0x80 | (code & 0x3f),
        );
      }
    }
    return new Uint8Array(out);
  }

  // WHATWG UTF-8 decode. Returns { text, rest } where `rest` is the incomplete
  // tail kept for `{ stream: true }`.
  //
  // 逐字节复刻 WHATWG Encoding 的 UTF-8 decoder(含每个 lead byte 的合法
  // 续字节区间): 一个畸形序列按「maximal subpart」各产生一个 U+FFFD, 合法
  // 前缀之后的非法字节会重新处理 (E0 80 -> U+FFFD U+FFFD, F4 90 80 ->
  // U+FFFD U+FFFD U+FFFD)。
  function utf8Error(fatal) {
    if (fatal) throw new TypeError("The encoded data was not valid for encoding utf-8");
  }

  function decodeUTF8(bytes, fatal, stream) {
    var result = "";
    var index = 0;
    var length = bytes.length;
    while (index < length) {
      var first = bytes[index];
      if (first < 0x80) {
        result += String.fromCharCode(first);
        index++;
        continue;
      }
      var needed = 0;
      var lower = 0x80;
      var upper = 0xbf;
      var code = 0;
      if (first >= 0xc2 && first <= 0xdf) {
        needed = 1;
        code = first & 0x1f;
      } else if (first === 0xe0) {
        needed = 2;
        lower = 0xa0;
        code = first & 0x0f;
      } else if (first >= 0xe1 && first <= 0xec) {
        needed = 2;
        code = first & 0x0f;
      } else if (first === 0xed) {
        needed = 2;
        upper = 0x9f;
        code = first & 0x0f;
      } else if (first >= 0xee && first <= 0xef) {
        needed = 2;
        code = first & 0x0f;
      } else if (first === 0xf0) {
        needed = 3;
        lower = 0x90;
        code = first & 0x07;
      } else if (first >= 0xf1 && first <= 0xf3) {
        needed = 3;
        code = first & 0x07;
      } else if (first === 0xf4) {
        needed = 3;
        upper = 0x8f;
        code = first & 0x07;
      } else {
        utf8Error(fatal);
        result += "�";
        index++;
        continue;
      }
      var consumed = 0;
      var invalid = false;
      var truncated = false;
      for (var j = 1; j <= needed; j++) {
        if (index + j >= length) {
          truncated = true;
          break;
        }
        var next = bytes[index + j];
        var lowBound = j === 1 ? lower : 0x80;
        var highBound = j === 1 ? upper : 0xbf;
        if (next < lowBound || next > highBound) {
          invalid = true;
          break;
        }
        code = (code << 6) | (next & 0x3f);
        consumed = j;
      }
      if (truncated) {
        if (stream) return { text: result, rest: bytes.slice(index) };
        utf8Error(fatal);
        // 截断的序列整体是一个 maximal subpart: 一个 U+FFFD。
        result += "�";
        index = length;
        continue;
      }
      if (invalid) {
        utf8Error(fatal);
        result += "�";
        // 只消耗已经确认合法的续字节；非法字节留给下一轮重新处理。
        index += consumed + 1;
        continue;
      }
      if (code >= 0x10000) {
        code -= 0x10000;
        result += String.fromCharCode(0xd800 + (code >> 10), 0xdc00 + (code & 0x3ff));
      } else {
        result += String.fromCharCode(code);
      }
      index += needed + 1;
    }
    return { text: result, rest: new Uint8Array(0) };
  }

  // ---------------------------------------------------------------------
  // base64 / hex / latin1
  // ---------------------------------------------------------------------

  var B64_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

  function base64Encode(bytes) {
    var out = "";
    var i = 0;
    for (; i + 2 < bytes.length; i += 3) {
      var value = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
      out += B64_CHARS[(value >> 18) & 63] + B64_CHARS[(value >> 12) & 63] +
        B64_CHARS[(value >> 6) & 63] + B64_CHARS[value & 63];
    }
    var remaining = bytes.length - i;
    if (remaining === 1) {
      var one = bytes[i] << 16;
      out += B64_CHARS[(one >> 18) & 63] + B64_CHARS[(one >> 12) & 63] + "==";
    } else if (remaining === 2) {
      var two = (bytes[i] << 16) | (bytes[i + 1] << 8);
      out += B64_CHARS[(two >> 18) & 63] + B64_CHARS[(two >> 12) & 63] +
        B64_CHARS[(two >> 6) & 63] + "=";
    }
    return out;
  }

  function base64Decode(text) {
    text = String(text).replace(/[\t\n\f\r ]+/g, "");
    if (text.length % 4 === 0 && text.slice(-2) === "==") text = text.slice(0, -2);
    else if (text.length % 4 === 0 && text.slice(-1) === "=") text = text.slice(0, -1);
    var out = [];
    var buffer = 0;
    var bits = 0;
    for (var i = 0; i < text.length; i++) {
      var value = B64_CHARS.indexOf(text.charAt(i));
      if (value === -1) continue;
      buffer = (buffer << 6) | value;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out.push((buffer >> bits) & 0xff);
      }
    }
    return new Uint8Array(out);
  }

  function toBytes(value) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) {
      return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    if (Array.isArray(value)) return new Uint8Array(value);
    return encodeUTF8(String(value));
  }

  // ---------------------------------------------------------------------
  // Buffer
  // ---------------------------------------------------------------------

  var encodings = {
    utf8: "utf8",
    "utf-8": "utf8",
    utf16le: "utf16le",
    "utf-16le": "utf16le",
    ucs2: "utf16le",
    "ucs-2": "utf16le",
    latin1: "latin1",
    binary: "latin1",
    ascii: "ascii",
    base64: "base64",
    base64url: "base64url",
    hex: "hex",
  };

  function normalizeEncoding(encoding) {
    var name = String(encoding === undefined ? "utf8" : encoding).toLowerCase();
    var normalized = encodings[name];
    if (normalized === undefined) {
      var error = new TypeError("Unknown encoding: " + encoding);
      error.code = "ERR_UNKNOWN_ENCODING";
      throw error;
    }
    return normalized;
  }

  function stringToBytes(text, encoding) {
    encoding = normalizeEncoding(encoding);
    if (encoding === "utf8") return encodeUTF8(text);
    if (encoding === "latin1") {
      var bytes = new Uint8Array(text.length);
      for (var i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i) & 0xff;
      return bytes;
    }
    if (encoding === "ascii") {
      // Node 的 'ascii' 写入按 latin1 截断（只保留低 8 位）；读取时才 & 0x7f。
      var ascii = new Uint8Array(text.length);
      for (var j = 0; j < text.length; j++) ascii[j] = text.charCodeAt(j) & 0xff;
      return ascii;
    }
    if (encoding === "utf16le") {
      var out = new Uint8Array(text.length * 2);
      for (var k = 0; k < text.length; k++) {
        var code = text.charCodeAt(k);
        out[k * 2] = code & 0xff;
        out[k * 2 + 1] = (code >> 8) & 0xff;
      }
      return out;
    }
    if (encoding === "hex") {
      // Node 在第一个非十六进制字符处停止（不跳过、不忽略半个字节）。
      var hexOut = new Uint8Array(Math.floor(text.length / 2));
      var hexLength = 0;
      for (var h = 0; h + 1 < text.length; h += 2) {
        var pair = text.slice(h, h + 2);
        if (!/^[0-9a-fA-F]{2}$/.test(pair)) break;
        hexOut[hexLength++] = parseInt(pair, 16);
      }
      return hexOut.subarray(0, hexLength);
    }
    if (encoding === "base64" || encoding === "base64url") {
      var raw = encoding === "base64url"
        ? text.replace(/-/g, "+").replace(/_/g, "/")
        : text;
      return base64Decode(raw);
    }
    return encodeUTF8(String(text));
  }

  function bytesToString(bytes, encoding, start, end) {
    encoding = normalizeEncoding(encoding);
    start = start === undefined ? 0 : Math.max(0, start | 0);
    end = end === undefined ? bytes.length : Math.min(bytes.length, end | 0);
    if (start >= end) return "";
    var slice = bytes.subarray(start, end);
    if (encoding === "utf8") return decodeUTF8(slice, false, false).text;
    if (encoding === "latin1" || encoding === "ascii") {
      var mask = encoding === "ascii" ? 0x7f : 0xff;
      var text = "";
      for (var i = 0; i < slice.length; i++) {
        text += String.fromCharCode(slice[i] & mask);
      }
      return text;
    }
    if (encoding === "utf16le") {
      var units = "";
      for (var j = 0; j + 1 < slice.length; j += 2) {
        units += String.fromCharCode(slice[j] | (slice[j + 1] << 8));
      }
      return units;
    }
    if (encoding === "hex") {
      var hex = "";
      for (var k = 0; k < slice.length; k++) {
        hex += (slice[k] < 16 ? "0" : "") + slice[k].toString(16);
      }
      return hex;
    }
    if (encoding === "base64" || encoding === "base64url") {
      var encoded = base64Encode(slice);
      if (encoding === "base64url") {
        encoded = encoded.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
      }
      return encoded;
    }
    return "";
  }

  function describeReceived(value) {
    if (value === null) return "null";
    var type = typeof value;
    if (type === "undefined") return "undefined";
    if (type === "number") return "type number (" + value + ")";
    if (type === "string") return "type string ('" + value + "')";
    if (type === "boolean") return "type boolean (" + value + ")";
    if (type === "bigint") return "type bigint (" + value + "n)";
    if (Array.isArray(value)) return "an instance of Array";
    var name = value && value.constructor && value.constructor.name
      ? value.constructor.name
      : "Object";
    return "an instance of " + name;
  }

  function invalidArgType(argument, expected, value) {
    var error = new TypeError(
      "The \"" + argument + "\" argument must be " + expected +
      ". Received " + describeReceived(value),
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }

  function firstArgumentError(value) {
    var error = new TypeError(
      "The first argument must be of type string or an instance of Buffer, " +
      "ArrayBuffer, or Array or an Array-like Object. Received " + describeReceived(value),
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }

  function outOfRange(argument, min, max, value) {
    var error = new RangeError(
      "The value of \"" + argument + "\" is out of range. It must be >= " +
      min + " and <= " + max + ". Received " + value,
    );
    error.code = "ERR_OUT_OF_RANGE";
    return error;
  }

  function checkSize(size, name) {
    if (typeof size !== "number") {
      throw invalidArgType(name, "of type number", size);
    }
    if (!isFinite(size) || Math.floor(size) !== size || size < 0 || size > 4294967296) {
      throw outOfRange(name, 0, 4294967296, size);
    }
    return size;
  }

  /// TypedArray/DataView -> 新 Uint8Array，逐元素截断（Node 的 Buffer.from
  /// 语义：Uint16Array([0x1234]) -> <34>，不是原始字节 34 12）。
  function copyViewBytes(value) {
    if (value instanceof DataView) {
      var raw = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
      var viewCopy = new Uint8Array(raw.length);
      viewCopy.set(raw);
      return viewCopy;
    }
    var length = value.length === undefined ? 0 : value.length;
    var copy = new Uint8Array(length);
    for (var i = 0; i < length; i++) copy[i] = value[i];
    return copy;
  }

  function checkBounds(buffer, offset, size, name) {
    offset = offset === undefined ? 0 : offset | 0;
    if (offset < 0 || offset + size > buffer.length) {
      var error = new RangeError("Attempt to access memory outside buffer bounds");
      error.code = "ERR_BUFFER_OUT_OF_BOUNDS";
      throw error;
    }
    return offset;
  }

  function checkIntValue(value, min, max) {
    if (typeof value !== "number") throw invalidArgType("value", "of type number", value);
    if (Math.floor(value) !== value || value < min || value > max) {
      throw outOfRange("value", min, max, value);
    }
    return value;
  }

  class Buffer extends Uint8Array {
    constructor(value, offset, length) {
      if (typeof value === "number") {
        super(checkSize(value, "size"));
      } else if (value instanceof ArrayBuffer) {
        super(value, offset === undefined ? 0 : offset, length);
      } else if (ArrayBuffer.isView(value)) {
        var copied = copyViewBytes(value);
        super(copied.length);
        this.set(copied);
      } else if (Array.isArray(value)) {
        super(value.length);
        for (var i = 0; i < value.length; i++) this[i] = value[i];
      } else if (typeof value === "string") {
        var encoded = stringToBytes(value, offset);
        super(encoded.length);
        this.set(encoded);
      } else {
        super(value === undefined || value === null ? 0 : value);
      }
    }

    static from(value, encodingOrOffset, length) {
      if (typeof value === "string") {
        return new Buffer(value, encodingOrOffset);
      }
      if (value instanceof ArrayBuffer) {
        var start = encodingOrOffset === undefined ? 0 : encodingOrOffset | 0;
        var byteLength = length === undefined ? value.byteLength - start : length | 0;
        return new Buffer(value, start, byteLength);
      }
      if (ArrayBuffer.isView(value)) {
        // Node copies the element values（DataView 复制字节）。
        var bytes = copyViewBytes(value);
        var copy = new Buffer(bytes.length);
        copy.set(bytes);
        return copy;
      }
      if (Array.isArray(value)) {
        var fromArray = new Buffer(value.length);
        for (var i = 0; i < value.length; i++) fromArray[i] = value[i];
        return fromArray;
      }
      if (value && typeof value === "object" && typeof value.length === "number") {
        var fromLike = new Buffer(value.length);
        for (var j = 0; j < value.length; j++) fromLike[j] = value[j];
        return fromLike;
      }
      throw firstArgumentError(value);
    }

    static alloc(size, fill, encoding) {
      var buffer = new Buffer(checkSize(size, "size"));
      if (fill !== undefined) buffer.fill(fill, 0, buffer.length, encoding);
      return buffer;
    }

    static allocUnsafe(size) {
      return new Buffer(checkSize(size, "size"));
    }

    static allocUnsafeSlow(size) {
      return new Buffer(checkSize(size, "size"));
    }

    static isBuffer(value) {
      return value instanceof Buffer;
    }

    static isEncoding(encoding) {
      return encodings[String(encoding).toLowerCase()] !== undefined;
    }

    static byteLength(value, encoding) {
      if (typeof value === "string") return stringToBytes(value, encoding).length;
      if (value instanceof ArrayBuffer) return value.byteLength;
      if (ArrayBuffer.isView(value)) return value.byteLength;
      throw invalidArgType(
        "string",
        "of type string or an instance of Buffer or ArrayBuffer",
        value,
      );
    }

    static concat(list, totalLength) {
      if (!Array.isArray(list)) {
        throw invalidArgType("list", "an instance of Array", list);
      }
      var length = 0;
      for (var i = 0; i < list.length; i++) {
        if (!(list[i] instanceof Uint8Array)) {
          throw invalidArgType(
            "list[" + i + "]",
            "an instance of Buffer or Uint8Array",
            list[i],
          );
        }
        length += list[i].length;
      }
      if (totalLength === undefined) {
        totalLength = length;
      } else {
        if (typeof totalLength !== "number") {
          throw invalidArgType("totalLength", "of type number", totalLength);
        }
        totalLength = totalLength | 0;
        if (totalLength < 0 || totalLength > 4294967296) {
          throw outOfRange("totalLength", 0, 4294967296, totalLength);
        }
      }
      var out = new Buffer(totalLength);
      var offset = 0;
      for (var j = 0; j < list.length; j++) {
        var item = list[j];
        if (offset + item.length > totalLength) item = item.subarray(0, totalLength - offset);
        out.set(item, offset);
        offset += item.length;
        if (offset >= totalLength) break;
      }
      return out;
    }

    static compare(a, b) {
      return Buffer.from(a).compare(Buffer.from(b));
    }

    toString(encoding, start, end) {
      return bytesToString(this, encoding, start, end);
    }

    toJSON() {
      return { type: "Buffer", data: Array.prototype.slice.call(this) };
    }

    equals(other) {
      if (!(other instanceof Uint8Array)) return false;
      if (this.length !== other.length) return false;
      for (var i = 0; i < this.length; i++) if (this[i] !== other[i]) return false;
      return true;
    }

    compare(other) {
      other = toBytes(other);
      var length = Math.min(this.length, other.length);
      for (var i = 0; i < length; i++) {
        if (this[i] !== other[i]) return this[i] < other[i] ? -1 : 1;
      }
      if (this.length === other.length) return 0;
      return this.length < other.length ? -1 : 1;
    }

    copy(target, targetStart, sourceStart, sourceEnd) {
      targetStart = targetStart === undefined ? 0 : targetStart | 0;
      sourceStart = sourceStart === undefined ? 0 : sourceStart | 0;
      sourceEnd = sourceEnd === undefined ? this.length : sourceEnd | 0;
      var count = Math.min(sourceEnd - sourceStart, target.length - targetStart);
      if (count <= 0) return 0;
      target.set(this.subarray(sourceStart, sourceStart + count), targetStart);
      return count;
    }

    slice(start, end) {
      return this.subarray(start, end);
    }

    write(text, offset, length, encoding) {
      if (typeof offset === "string") {
        encoding = offset;
        offset = 0;
        length = this.length;
      } else if (typeof length === "string") {
        encoding = length;
        length = this.length - (offset | 0);
      }
      offset = offset === undefined ? 0 : offset | 0;
      length = length === undefined ? this.length - offset : length | 0;
      var encoded = stringToBytes(String(text), encoding);
      var count = Math.min(length, encoded.length, this.length - offset);
      this.set(encoded.subarray(0, count), offset);
      return count;
    }

    fill(value, start, end, encoding) {
      if (typeof start === "string") {
        encoding = start;
        start = 0;
        end = this.length;
      } else if (typeof end === "string") {
        encoding = end;
        end = this.length;
      }
      start = start === undefined ? 0 : start | 0;
      end = end === undefined ? this.length : end | 0;
      // Node 的 `fill()`（无参数）填 0；`fill('')` 同样填 0。
      if (value === undefined || value === null) value = 0;
      if (typeof value === "number") {
        for (var i = start; i < end; i++) this[i] = value & 0xff;
        return this;
      }
      var bytes = typeof value === "string" ? stringToBytes(value, encoding) : toBytes(value);
      if (bytes.length === 0) {
        for (var j = start; j < end; j++) this[j] = 0;
        return this;
      }
      for (var k = start; k < end; k++) this[k] = bytes[(k - start) % bytes.length];
      return this;
    }

    indexOf(needle, byteOffset, encoding) {
      var search = typeof needle === "number"
        ? new Uint8Array([needle & 0xff])
        : typeof needle === "string"
          ? stringToBytes(needle, encoding)
          : toBytes(needle);
      var from = byteOffset === undefined ? 0 : Math.max(0, byteOffset | 0);
      outer: for (var i = from; i + search.length <= this.length; i++) {
        for (var j = 0; j < search.length; j++) {
          if (this[i + j] !== search[j]) continue outer;
        }
        return i;
      }
      return -1;
    }

    includes(needle, byteOffset, encoding) {
      return this.indexOf(needle, byteOffset, encoding) !== -1;
    }

    lastIndexOf(needle, byteOffset, encoding) {
      var search = typeof needle === "number"
        ? new Uint8Array([needle & 0xff])
        : typeof needle === "string"
          ? stringToBytes(needle, encoding)
          : toBytes(needle);
      var from = byteOffset === undefined ? this.length - search.length : byteOffset | 0;
      outer: for (var i = Math.min(from, this.length - search.length); i >= 0; i--) {
        for (var j = 0; j < search.length; j++) {
          if (this[i + j] !== search[j]) continue outer;
        }
        return i;
      }
      return -1;
    }

    readUInt8(offset) { return this[checkBounds(this, offset, 1)]; }
    readInt8(offset) {
      var v = this[checkBounds(this, offset, 1)];
      return v & 0x80 ? v - 0x100 : v;
    }
    readUInt16LE(offset) {
      offset = checkBounds(this, offset, 2);
      return this[offset] | (this[offset + 1] << 8);
    }
    readUInt16BE(offset) {
      offset = checkBounds(this, offset, 2);
      return (this[offset] << 8) | this[offset + 1];
    }
    readInt16LE(offset) { var v = this.readUInt16LE(offset); return v & 0x8000 ? v - 0x10000 : v; }
    readInt16BE(offset) { var w = this.readUInt16BE(offset); return w & 0x8000 ? w - 0x10000 : w; }
    readUInt32LE(offset) {
      offset = checkBounds(this, offset, 4);
      return (this[offset] | (this[offset + 1] << 8) | (this[offset + 2] << 16)) +
        this[offset + 3] * 0x1000000;
    }
    readUInt32BE(offset) {
      offset = checkBounds(this, offset, 4);
      return this[offset] * 0x1000000 +
        ((this[offset + 1] << 16) | (this[offset + 2] << 8) | this[offset + 3]);
    }
    readInt32LE(offset) { return this.readUInt32LE(offset) | 0; }
    readInt32BE(offset) { return this.readUInt32BE(offset) | 0; }
    readFloatLE(offset) {
      offset = checkBounds(this, offset, 4);
      return new DataView(this.buffer, this.byteOffset + offset, 4).getFloat32(0, true);
    }
    readFloatBE(offset) {
      offset = checkBounds(this, offset, 4);
      return new DataView(this.buffer, this.byteOffset + offset, 4).getFloat32(0, false);
    }
    readDoubleLE(offset) {
      offset = checkBounds(this, offset, 8);
      return new DataView(this.buffer, this.byteOffset + offset, 8).getFloat64(0, true);
    }
    readDoubleBE(offset) {
      offset = checkBounds(this, offset, 8);
      return new DataView(this.buffer, this.byteOffset + offset, 8).getFloat64(0, false);
    }

    writeUInt8(value, offset) {
      offset = checkBounds(this, offset, 1);
      this[offset] = checkIntValue(value, 0, 0xff);
      return offset + 1;
    }
    writeInt8(value, offset) {
      offset = checkBounds(this, offset, 1);
      checkIntValue(value, -0x80, 0x7f);
      this[offset] = value & 0xff;
      return offset + 1;
    }
    writeUInt16LE(value, offset) {
      offset = checkBounds(this, offset, 2);
      checkIntValue(value, 0, 0xffff);
      this[offset] = value & 0xff;
      this[offset + 1] = (value >> 8) & 0xff;
      return offset + 2;
    }
    writeUInt16BE(value, offset) {
      offset = checkBounds(this, offset, 2);
      checkIntValue(value, 0, 0xffff);
      this[offset] = (value >> 8) & 0xff;
      this[offset + 1] = value & 0xff;
      return offset + 2;
    }
    writeInt16LE(value, offset) {
      offset = checkBounds(this, offset, 2);
      checkIntValue(value, -0x8000, 0x7fff);
      this[offset] = value & 0xff;
      this[offset + 1] = (value >> 8) & 0xff;
      return offset + 2;
    }
    writeInt16BE(value, offset) {
      offset = checkBounds(this, offset, 2);
      checkIntValue(value, -0x8000, 0x7fff);
      this[offset] = (value >> 8) & 0xff;
      this[offset + 1] = value & 0xff;
      return offset + 2;
    }
    writeUInt32LE(value, offset) {
      offset = checkBounds(this, offset, 4);
      checkIntValue(value, 0, 0xffffffff);
      this[offset] = value & 0xff;
      this[offset + 1] = (value >> 8) & 0xff;
      this[offset + 2] = (value >> 16) & 0xff;
      this[offset + 3] = (value >> 24) & 0xff;
      return offset + 4;
    }
    writeUInt32BE(value, offset) {
      offset = checkBounds(this, offset, 4);
      checkIntValue(value, 0, 0xffffffff);
      this[offset] = (value >> 24) & 0xff;
      this[offset + 1] = (value >> 16) & 0xff;
      this[offset + 2] = (value >> 8) & 0xff;
      this[offset + 3] = value & 0xff;
      return offset + 4;
    }
    writeInt32LE(value, offset) {
      offset = checkBounds(this, offset, 4);
      checkIntValue(value, -0x80000000, 0x7fffffff);
      this[offset] = value & 0xff;
      this[offset + 1] = (value >> 8) & 0xff;
      this[offset + 2] = (value >> 16) & 0xff;
      this[offset + 3] = (value >> 24) & 0xff;
      return offset + 4;
    }
    writeInt32BE(value, offset) {
      offset = checkBounds(this, offset, 4);
      checkIntValue(value, -0x80000000, 0x7fffffff);
      this[offset] = (value >> 24) & 0xff;
      this[offset + 1] = (value >> 16) & 0xff;
      this[offset + 2] = (value >> 8) & 0xff;
      this[offset + 3] = value & 0xff;
      return offset + 4;
    }
    writeFloatLE(value, offset) {
      offset = checkBounds(this, offset, 4);
      new DataView(this.buffer, this.byteOffset + offset, 4).setFloat32(0, value, true);
      return offset + 4;
    }
    writeFloatBE(value, offset) {
      offset = checkBounds(this, offset, 4);
      new DataView(this.buffer, this.byteOffset + offset, 4).setFloat32(0, value, false);
      return offset + 4;
    }
    writeDoubleLE(value, offset) {
      offset = checkBounds(this, offset, 8);
      new DataView(this.buffer, this.byteOffset + offset, 8).setFloat64(0, value, true);
      return offset + 8;
    }
    writeDoubleBE(value, offset) {
      offset = checkBounds(this, offset, 8);
      new DataView(this.buffer, this.byteOffset + offset, 8).setFloat64(0, value, false);
      return offset + 8;
    }
  }

  Object.defineProperty(Buffer, "name", { value: "Buffer", configurable: true });
  Object.defineProperty(Buffer.prototype, Symbol.toStringTag, {
    value: "Uint8Array",
    configurable: true,
  });
  Object.defineProperty(globalThis, "Buffer", {
    value: Buffer,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // inspect / format (used by console + node:util)
  // ---------------------------------------------------------------------

  function quoteString(text) {
    var quote = text.indexOf("'") === -1 ? "'" : '"';
    var escaped = text
      .replace(/\\/g, "\\\\")
      .replace(/\n/g, "\\n")
      .replace(/\r/g, "\\r")
      .replace(/\t/g, "\\t")
      .replace(/\u0008/g, "\\b")
      .replace(/\f/g, "\\f");
    if (quote === "'") escaped = escaped.replace(/'/g, "\\'");
    else escaped = escaped.replace(/"/g, '\\"');
    return quote + escaped + quote;
  }

  function inspectValue(value, depth, seen) {
    depth = depth === undefined ? 2 : depth;
    seen = seen || [];
    var type = typeof value;
    if (value === null) return "null";
    if (type === "undefined") return "undefined";
    if (type === "string") return quoteString(value);
    if (type === "number") {
      if (Object.is(value, -0)) return "-0";
      return String(value);
    }
    if (type === "bigint") return String(value) + "n";
    if (type === "boolean") return value ? "true" : "false";
    if (type === "symbol") return String(value);
    if (type === "function") {
      var name = value.name || "";
      if (!name) return "[Function (anonymous)]";
      return "[Function: " + name + "]";
    }
    if (value instanceof Error) {
      return value.stack || (value.name + ": " + value.message);
    }
    if (value instanceof Date) {
      return isNaN(value.getTime()) ? "Invalid Date" : value.toISOString();
    }
    if (value instanceof RegExp) return String(value);
    if (value instanceof Buffer) {
      var parts = [];
      for (var b = 0; b < value.length; b++) {
        parts.push((value[b] < 16 ? "0" : "") + value[b].toString(16));
      }
      return "<Buffer " + parts.join(" ") + ">";
    }
    if (seen.indexOf(value) !== -1) return "[Circular *" + (seen.indexOf(value) + 1) + "]";
    seen = seen.concat([value]);
    if (Array.isArray(value)) {
      if (depth < 0) return "[Array]";
      if (value.length === 0) return "[]";
      var items = [];
      for (var i = 0; i < value.length; i++) {
        items.push(inspectValue(value[i], depth - 1, seen));
      }
      return "[ " + items.join(", ") + " ]";
    }
    if (value instanceof Map) {
      if (depth < 0) return "[Map]";
      var entries = [];
      value.forEach(function (entryValue, entryKey) {
        entries.push(inspectValue(entryKey, depth - 1, seen) + " => " + inspectValue(entryValue, depth - 1, seen));
      });
      return "Map(" + value.size + ") { " + entries.join(", ") + " }";
    }
    if (value instanceof Set) {
      if (depth < 0) return "[Set]";
      var setItems = [];
      value.forEach(function (entry) {
        setItems.push(inspectValue(entry, depth - 1, seen));
      });
      return "Set(" + value.size + ") { " + setItems.join(", ") + " }";
    }
    if (ArrayBuffer.isView(value)) {
      var view = value.constructor && value.constructor.name ? value.constructor.name : "Uint8Array";
      return view + "(" + value.length + ") [ " + Array.prototype.slice.call(value).join(", ") + " ]";
    }
    if (value instanceof ArrayBuffer) return "ArrayBuffer { byteLength: " + value.byteLength + " }";
    if (depth < 0) {
      var tag = Object.prototype.toString.call(value).slice(8, -1);
      return "[" + tag + "]";
    }
    var keys = Object.keys(value);
    var constructorName = "";
    var proto = Object.getPrototypeOf(value);
    if (proto !== null && proto !== Object.prototype && proto.constructor && proto.constructor.name &&
        proto.constructor.name !== "Object") {
      constructorName = proto.constructor.name + " ";
    }
    if (keys.length === 0) return constructorName + "{}";
    var rendered = [];
    for (var k = 0; k < keys.length; k++) {
      var key = keys[k];
      var keyText = /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key) ? key : quoteString(key);
      rendered.push(keyText + ": " + inspectValue(value[key], depth - 1, seen));
    }
    return constructorName + "{ " + rendered.join(", ") + " }";
  }

  function inspect(value, options) {
    var depth = options && typeof options.depth === "number" ? options.depth : 2;
    return inspectValue(value, depth, []);
  }

  function format() {
    var args = Array.prototype.slice.call(arguments);
    if (args.length === 0) return "";
    var first = args[0];
    if (typeof first !== "string") {
      var rendered = [];
      for (var i = 0; i < args.length; i++) {
        rendered.push(typeof args[i] === "string" ? args[i] : inspectValue(args[i], 2, []));
      }
      return rendered.join(" ");
    }
    var index = 1;
    var output = first.replace(/%[sdifjoOc%]/g, function (match) {
      if (match === "%%") return "%";
      if (index >= args.length) return match;
      var value = args[index++];
      switch (match) {
        case "%s":
          return typeof value === "string" ? value : typeof value === "object" && value !== null
            ? inspectValue(value, 2, [])
            : String(value);
        case "%d":
          return String(Number(value));
        case "%i":
          return String(parseInt(value, 10));
        case "%f":
          return String(parseFloat(value));
        case "%j":
          try { return JSON.stringify(value); } catch (_) { return "[Circular]"; }
        case "%o":
        case "%O":
          return inspectValue(value, 2, []);
        case "%c":
          return "";
        default:
          return match;
      }
    });
    while (index < args.length) {
      var extra = args[index++];
      output += " " + (typeof extra === "string" ? extra : inspectValue(extra, 2, []));
    }
    return output;
  }

  // ---------------------------------------------------------------------
  // console
  // ---------------------------------------------------------------------

  var consoleTimers = new Map();
  var consoleCounts = new Map();

  function consoleWrite(level, args) {
    var text = format.apply(null, args);
    host.log(level, text);
  }

  var consoleObject = {
    log: function () { consoleWrite("log", arguments); },
    info: function () { consoleWrite("info", arguments); },
    debug: function () { consoleWrite("debug", arguments); },
    trace: function () { consoleWrite("trace", arguments); },
    warn: function () { consoleWrite("warn", arguments); },
    error: function () { consoleWrite("error", arguments); },
    dir: function (value, options) {
      host.log("dir", inspect(value, options));
    },
    assert: function (condition) {
      if (condition) return;
      var args = Array.prototype.slice.call(arguments, 1);
      var text = "Assertion failed";
      if (args.length > 0) text += ": " + format.apply(null, args);
      host.log("error", text);
    },
    table: function (value) {
      host.log("table", inspect(value));
    },
    time: function (label) {
      consoleTimers.set(label === undefined ? "default" : String(label), host.now());
    },
    timeEnd: function (label) {
      label = label === undefined ? "default" : String(label);
      var started = consoleTimers.get(label);
      if (started === undefined) return;
      consoleTimers.delete(label);
      host.log("log", label + ": " + (host.now() - started).toFixed(3) + "ms");
    },
    count: function (label) {
      label = label === undefined ? "default" : String(label);
      var count = (consoleCounts.get(label) || 0) + 1;
      consoleCounts.set(label, count);
      host.log("log", label + ": " + count);
    },
    countReset: function (label) {
      consoleCounts.delete(label === undefined ? "default" : String(label));
    },
    group: function () {},
    groupCollapsed: function () {},
    groupEnd: function () {},
    clear: function () {},
    timeLog: function () {},
  };

  class Console {
    constructor() {}
  }
  Object.keys(consoleObject).forEach(function (key) {
    Console.prototype[key] = consoleObject[key];
  });
  Console.prototype.log = consoleObject.log;
  consoleObject.Console = Console;

  Object.defineProperty(globalThis, "console", {
    value: consoleObject,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // process
  // ---------------------------------------------------------------------

  class ExitSignal extends Error {
    constructor(code) {
      super("process.exit(" + code + ")");
      this.name = "ExitSignal";
      this.code = typeof code === "number" ? code : 0;
      this.__kianaExitSignal = true;
    }
  }

  function writeStream(fd) {
    return {
      fd: fd,
      isTTY: true,
      columns: 80,
      rows: 24,
      writable: true,
      write: function (chunk, encoding, callback) {
        if (typeof encoding === "function") {
          callback = encoding;
          encoding = undefined;
        }
        var text = chunk instanceof Uint8Array ? bytesToString(chunk, "utf8") : String(chunk);
        host.write(fd, text);
        if (typeof callback === "function") {
          if (typeof queueMicrotask === "function") queueMicrotask(callback);
          else setTimeout(callback, 0);
        }
        return true;
      },
      end: function () {},
      on: function () { return this; },
      once: function () { return this; },
      off: function () { return this; },
      addListener: function () { return this; },
      removeListener: function () { return this; },
      removeAllListeners: function () { return this; },
      emit: function () { return false; },
      cork: function () {},
      uncork: function () {},
      destroy: function () {},
      setDefaultEncoding: function () { return this; },
    };
  }

  var processListeners = Object.create(null);

  function addProcessListener(event, listener, once) {
    if (!processListeners[event]) processListeners[event] = [];
    processListeners[event].push({ listener: listener, once: once });
    return processObject;
  }

  function removeProcessListener(event, listener) {
    var list = processListeners[event];
    if (!list) return processObject;
    for (var i = list.length - 1; i >= 0; i--) {
      if (list[i].listener === listener) list.splice(i, 1);
    }
    return processObject;
  }

  var envObject = {};
  try {
    var parsedEnv = JSON.parse(host.env());
    for (var envKey in parsedEnv) {
      if (Object.prototype.hasOwnProperty.call(parsedEnv, envKey)) envObject[envKey] = parsedEnv[envKey];
    }
  } catch (_) {}
  // Node coerces every assigned value to a string; a proxy keeps that behavior
  // while still exposing a plain, enumerable, writable object.
  if (typeof Proxy === "function") {
    envObject = new Proxy(envObject, {
      set: function (target, key, value) {
        target[key] = String(value);
        return true;
      },
      defineProperty: function (target, key, descriptor) {
        if ("value" in descriptor) descriptor.value = String(descriptor.value);
        return Reflect.defineProperty(target, key, descriptor);
      },
    });
  }

  var VALID_SIGNAL_NAMES = Object.create(null);
  [
    "SIGHUP", "SIGINT", "SIGQUIT", "SIGILL", "SIGTRAP", "SIGABRT", "SIGIOT",
    "SIGBUS", "SIGFPE", "SIGKILL", "SIGUSR1", "SIGSEGV", "SIGUSR2", "SIGPIPE",
    "SIGALRM", "SIGTERM", "SIGSTKFLT", "SIGCHLD", "SIGCONT", "SIGSTOP",
    "SIGTSTP", "SIGTTIN", "SIGTTOU", "SIGURG", "SIGXCPU", "SIGXFSZ",
    "SIGVTALRM", "SIGPROF", "SIGWINCH", "SIGIO", "SIGPOLL", "SIGPWR",
    "SIGSYS", "SIGUNUSED",
  ].forEach(function (name) { VALID_SIGNAL_NAMES[name] = true; });

  var processObject = {
    env: envObject,
    argv: ["kiana", host.execPath()],
    execPath: host.execPath(),
    execArgv: [],
    platform: host.platform(),
    arch: host.arch(),
    pid: host.pid(),
    ppid: 1,
    title: "kiana",
    version: "v22.0.0",
    versions: { node: "22.0.0", v8: "0.0.0", uv: "0.0.0", modules: "127" },
    release: { name: "node" },
    exitCode: 0,
    cwd: function () { return host.cwd(); },
    chdir: function (directory) { host.chdir(String(directory)); },
    exit: function (code) { throw new ExitSignal(code === undefined ? 0 : code); },
    stdout: writeStream(1),
    stderr: writeStream(2),
    stdin: {
      fd: 0,
      isTTY: false,
      readable: true,
      on: function () { return this; },
      once: function () { return this; },
      off: function () { return this; },
      addListener: function () { return this; },
      removeListener: function () { return this; },
      pause: function () { return this; },
      resume: function () { return this; },
      setEncoding: function () { return this; },
      read: function () { return null; },
      destroy: function () {},
    },
    nextTick: function (callback) {
      var args = Array.prototype.slice.call(arguments, 1);
      globalThis.__kiana.nextTick(function () { callback.apply(null, args); });
    },
    on: function (event, listener) { return addProcessListener(event, listener, false); },
    addListener: function (event, listener) { return addProcessListener(event, listener, false); },
    prependListener: function (event, listener) { return addProcessListener(event, listener, false); },
    once: function (event, listener) { return addProcessListener(event, listener, true); },
    prependOnceListener: function (event, listener) { return addProcessListener(event, listener, true); },
    off: function (event, listener) { return removeProcessListener(event, listener); },
    removeListener: function (event, listener) { return removeProcessListener(event, listener); },
    removeAllListeners: function (event) {
      if (event === undefined) processListeners = Object.create(null);
      else delete processListeners[event];
      return processObject;
    },
    listeners: function (event) {
      return (processListeners[event] || []).map(function (entry) { return entry.listener; });
    },
    listenerCount: function (event) { return (processListeners[event] || []).length; },
    emit: function (event) {
      var list = processListeners[event];
      if (!list || list.length === 0) return false;
      var args = Array.prototype.slice.call(arguments, 1);
      var snapshot = list.slice();
      for (var i = 0; i < snapshot.length; i++) {
        if (snapshot[i].once) removeProcessListener(event, snapshot[i].listener);
        snapshot[i].listener.apply(processObject, args);
      }
      return true;
    },
    kill: function (pid, signal) {
      if (typeof pid !== "number") {
        var pidError = new TypeError(
          "The \"pid\" argument must be of type number. Received " +
          (typeof pid === "string" ? "type string ('" + pid + "')" : "type " + typeof pid),
        );
        pidError.code = "ERR_INVALID_ARG_TYPE";
        throw pidError;
      }
      if (signal !== undefined && signal !== null) {
        var text = String(signal);
        if (!/^\d+$/.test(text) && !VALID_SIGNAL_NAMES[text]) {
          var signalError = new TypeError("Unknown signal: " + text);
          signalError.code = "ERR_UNKNOWN_SIGNAL";
          throw signalError;
        }
      }
      return signal === undefined || signal === null
        ? host.kill(pid)
        : host.kill(pid, String(signal));
    },
    hrtime: (function () {
      function hrtime(previous) {
        var now = host.now();
        var seconds = Math.floor(now / 1000);
        var nanoseconds = Math.floor((now % 1000) * 1e6);
        if (previous) {
          seconds -= previous[0];
          nanoseconds -= previous[1];
          if (nanoseconds < 0) {
            seconds -= 1;
            nanoseconds += 1e9;
          }
        }
        return [seconds, nanoseconds];
      }
      hrtime.bigint = function () { return BigInt(Math.round(host.now() * 1e6)); };
      return hrtime;
    })(),
    uptime: function () { return host.now() / 1000; },
    memoryUsage: function () {
      return { rss: 0, heapTotal: 0, heapUsed: 0, external: 0, arrayBuffers: 0 };
    },
    umask: function () { return 18; },
    getuid: function () { return 0; },
    getgid: function () { return 0; },
    geteuid: function () { return 0; },
    getegid: function () { return 0; },
    setuid: function () {},
    setgid: function () {},
    emitWarning: function (warning) { host.log("warn", String(warning)); },
    features: {
      inspector: false,
      debug: false,
      uv: true,
      ipv6: true,
      tls: true,
      tls_alpn: true,
      tls_sni: true,
      tls_ocsp: true,
      tls_psk: false,
      cached_builtins: true,
    },
    binding: function (name) {
      var error = new Error("process.binding is not supported by the kiana extension runtime (" + name + ")");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    },
  };

  Object.defineProperty(globalThis, "process", {
    value: processObject,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "ExitSignal", {
    value: ExitSignal,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  globalThis.__kiana.ExitSignal = ExitSignal;
  Object.defineProperty(globalThis, "global", {
    value: globalThis,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // TextEncoder / TextDecoder
  // ---------------------------------------------------------------------

  class TextEncoder {
    constructor() {
      this.encoding = "utf-8";
    }
    encode(input) {
      return encodeUTF8(input === undefined ? "" : String(input));
    }
    encodeInto(input, destination) {
      var text = String(input);
      var written = 0;
      var read = 0;
      var size = destination.length;
      while (read < text.length) {
        var code = text.charCodeAt(read);
        var units = 1;
        if (code >= 0xd800 && code <= 0xdbff && read + 1 < text.length) {
          var low = text.charCodeAt(read + 1);
          if (low >= 0xdc00 && low <= 0xdfff) {
            code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
            units = 2;
          }
        }
        if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd;
        var needed = code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
        if (written + needed > size) break;
        if (needed === 1) {
          destination[written] = code;
        } else if (needed === 2) {
          destination[written] = 0xc0 | (code >> 6);
          destination[written + 1] = 0x80 | (code & 0x3f);
        } else if (needed === 3) {
          destination[written] = 0xe0 | (code >> 12);
          destination[written + 1] = 0x80 | ((code >> 6) & 0x3f);
          destination[written + 2] = 0x80 | (code & 0x3f);
        } else {
          destination[written] = 0xf0 | (code >> 18);
          destination[written + 1] = 0x80 | ((code >> 12) & 0x3f);
          destination[written + 2] = 0x80 | ((code >> 6) & 0x3f);
          destination[written + 3] = 0x80 | (code & 0x3f);
        }
        written += needed;
        read += units;
      }
      return { read: read, written: written };
    }
  }

  class TextDecoder {
    constructor(label, options) {
      label = label === undefined ? "utf-8" : String(label);
      if (!/^(utf-?8|unicode-1-1-utf-8)$/i.test(label)) {
        throw new RangeError("The encoding \"" + label + "\" is not supported.");
      }
      options = options || {};
      this.encoding = "utf-8";
      this.fatal = !!options.fatal;
      this.ignoreBOM = !!options.ignoreBOM;
      this._pending = new Uint8Array(0);
      this._bomSeen = false;
    }
    decode(input, options) {
      var stream = !!(options && options.stream);
      var bytes = input === undefined || input === null ? new Uint8Array(0) : toBytes(input);
      if (this._pending.length > 0) {
        var combined = new Uint8Array(this._pending.length + bytes.length);
        combined.set(this._pending, 0);
        combined.set(bytes, this._pending.length);
        bytes = combined;
        this._pending = new Uint8Array(0);
      }
      var decoded = decodeUTF8(bytes, this.fatal, stream);
      if (stream) this._pending = decoded.rest;
      var text = decoded.text;
      // The BOM is only stripped once output actually starts: a BOM split across
      // `{stream:true}` chunks must still be recognized (WHATWG Encoding).
      if (!this._bomSeen && text.length > 0) {
        this._bomSeen = true;
        if (!this.ignoreBOM && text.charCodeAt(0) === 0xfeff) text = text.slice(1);
      }
      return text;
    }
  }

  Object.defineProperty(globalThis, "TextEncoder", {
    value: TextEncoder,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "TextDecoder", {
    value: TextDecoder,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // atob / btoa
  // ---------------------------------------------------------------------

  function btoa(input) {
    var text = String(input);
    for (var i = 0; i < text.length; i++) {
      if (text.charCodeAt(i) > 0xff) {
        throw new DOMException("Invalid character", "InvalidCharacterError");
      }
    }
    var bytes = new Uint8Array(text.length);
    for (var j = 0; j < text.length; j++) bytes[j] = text.charCodeAt(j);
    return base64Encode(bytes);
  }

  function atob(input) {
    var text = String(input).replace(/[\t\n\f\r ]+/g, "");
    // Forgiving-base64: only the last one/two characters may be "=".
    var padding = 0;
    while (padding < text.length && text.charAt(text.length - 1 - padding) === "=") padding++;
    if (padding > 2 || text.slice(0, text.length - padding).indexOf("=") !== -1 ||
        /[^A-Za-z0-9+/=]/.test(text)) {
      throw new DOMException("Invalid character", "InvalidCharacterError");
    }
    if (text.length % 4 === 1) {
      throw new DOMException(
        "The string to be decoded is not correctly encoded.",
        "InvalidCharacterError",
      );
    }
    var bytes = base64Decode(text);
    var output = "";
    for (var i = 0; i < bytes.length; i++) output += String.fromCharCode(bytes[i]);
    return output;
  }

  Object.defineProperty(globalThis, "atob", { value: atob, writable: true, enumerable: true, configurable: true });
  Object.defineProperty(globalThis, "btoa", { value: btoa, writable: true, enumerable: true, configurable: true });

  // ---------------------------------------------------------------------
  // crypto
  // ---------------------------------------------------------------------

  var cryptoObject = {
    getRandomValues: function (array) {
      if (!ArrayBuffer.isView(array) || array instanceof DataView) {
        throw new TypeError("The provided value is not of type '(ArrayBufferView)'");
      }
      var bytes = new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
      var random = base64Decode(host.randomBytes(bytes.length));
      bytes.set(random.subarray(0, bytes.length));
      return array;
    },
    randomUUID: function () {
      return host.randomUuid();
    },
    randomBytes: function (size, callback) {
      var bytes = Buffer.from(base64Decode(host.randomBytes(size | 0)));
      if (typeof callback === "function") {
        queueMicrotask(function () { callback(null, bytes); });
        return undefined;
      }
      return bytes;
    },
    subtle: {
      digest: function (algorithm, data) {
        return new Promise(function (resolve, reject) {
          try {
            var name = typeof algorithm === "string" ? algorithm : algorithm && algorithm.name;
            var bytes = toBytes(data);
            var digest = base64Decode(host.hash(String(name), base64Encode(bytes)));
            var buffer = new ArrayBuffer(digest.length);
            new Uint8Array(buffer).set(digest);
            resolve(buffer);
          } catch (error) {
            reject(error);
          }
        });
      },
    },
  };
  cryptoObject.webcrypto = cryptoObject;
  Object.defineProperty(globalThis, "crypto", {
    value: cryptoObject,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // performance
  // ---------------------------------------------------------------------

  var performanceTimeOrigin = host.nowEpochMs();
  var performanceObject = {
    timeOrigin: performanceTimeOrigin,
    now: function () { return host.now(); },
    mark: function (name) { return { name: name, entryType: "mark", startTime: host.now() }; },
    measure: function (name) { return { name: name, entryType: "measure", startTime: 0, duration: 0 }; },
    getEntries: function () { return []; },
    getEntriesByName: function () { return []; },
    getEntriesByType: function () { return []; },
    clearMarks: function () {},
    clearMeasures: function () {},
  };
  Object.defineProperty(globalThis, "performance", {
    value: performanceObject,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // __kiana_node_internal（node/*.js 共享的私有工具）
  // ---------------------------------------------------------------------

  function nodeError(json) {
    if (!json || typeof json !== "object") return new Error(String(json));
    var name = json.name || "Error";
    var error;
    if (name === "TypeError") error = new TypeError(json.message || "");
    else if (name === "RangeError") error = new RangeError(json.message || "");
    else if (name === "SyntaxError") error = new SyntaxError(json.message || "");
    else if (name === "AbortError") error = new DOMException(json.message || "", "AbortError");
    else {
      error = new Error(json.message || "");
      if (name !== "Error") error.name = name;
    }
    if (json.code !== undefined) error.code = json.code;
    if (json.errno !== undefined) error.errno = json.errno;
    if (json.syscall !== undefined) error.syscall = json.syscall;
    if (json.path !== undefined) error.path = json.path;
    if (json.dest !== undefined) error.dest = json.dest;
    if (json.src !== undefined) error.src = json.src;
    if (json.address !== undefined) error.address = json.address;
    if (json.port !== undefined) error.port = json.port;
    if (json.hostname !== undefined) error.hostname = json.hostname;
    return error;
  }

  function promiseWithNodeErrors(kind, payload) {
    return globalThis.__kiana.promise(kind, payload).catch(function (value) {
      if (value && value.__kianaError) throw nodeError(value);
      throw value;
    });
  }

  var internal = {
    encodeUTF8: encodeUTF8,
    decodeUTF8: decodeUTF8,
    base64Encode: base64Encode,
    base64Decode: base64Decode,
    toBytes: toBytes,
    stringToBytes: stringToBytes,
    bytesToString: bytesToString,
    normalizeEncoding: normalizeEncoding,
    inspect: inspect,
    inspectValue: inspectValue,
    format: format,
    nodeError: nodeError,
    promise: promiseWithNodeErrors,
    host: host,
    Buffer: Buffer,
    TextEncoder: TextEncoder,
    TextDecoder: TextDecoder,
  };
  Object.defineProperty(globalThis, "__kiana_node_internal", {
    value: internal,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // Node's Web Streams / fetch are installed at the end of this file.

  // ---------------------------------------------------------------------
  // URL / URLSearchParams (WHATWG; parsing via the `url` crate)
  // ---------------------------------------------------------------------

  function percentDecode(text) {
    try {
      return decodeURIComponent(text.replace(/\+/g, " "));
    } catch (_) {
      return text;
    }
  }

  function percentEncode(text) {
    return encodeURIComponent(String(text))
      .replace(/[!'()~]/g, function (ch) { return "%" + ch.charCodeAt(0).toString(16).toUpperCase(); })
      .replace(/%20/g, "+");
  }

  class URLSearchParams {
    constructor(init) {
      this._entries = [];
      this._owner = null;
      if (init === undefined || init === null) return;
      if (init instanceof URLSearchParams) {
        this._entries = init._entries.map(function (pair) { return [pair[0], pair[1]]; });
        return;
      }
      if (typeof init === "string") {
        var text = init.charAt(0) === "?" ? init.slice(1) : init;
        if (text === "") return;
        var parts = text.split("&");
        for (var i = 0; i < parts.length; i++) {
          if (parts[i] === "") continue;
          var index = parts[i].indexOf("=");
          var name = index === -1 ? parts[i] : parts[i].slice(0, index);
          var value = index === -1 ? "" : parts[i].slice(index + 1);
          this._entries.push([percentDecode(name), percentDecode(value)]);
        }
        return;
      }
      if (typeof init[Symbol.iterator] === "function") {
        var iterator = init[Symbol.iterator]();
        var step = iterator.next();
        while (!step.done) {
          var pair = step.value;
          this._entries.push([String(pair[0]), String(pair[1])]);
          step = iterator.next();
        }
        return;
      }
      if (typeof init === "object") {
        var keys = Object.keys(init);
        for (var j = 0; j < keys.length; j++) {
          this._entries.push([keys[j], String(init[keys[j]])]);
        }
      }
    }

    _changed() {
      if (this._owner) this._owner._syncFromSearchParams();
    }

    append(name, value) {
      this._entries.push([String(name), String(value)]);
      this._changed();
    }

    delete(name, value) {
      name = String(name);
      if (value === undefined) {
        this._entries = this._entries.filter(function (pair) { return pair[0] !== name; });
      } else {
        value = String(value);
        var found = false;
        this._entries = this._entries.filter(function (pair) {
          if (!found && pair[0] === name && pair[1] === value) {
            found = true;
            return false;
          }
          return true;
        });
      }
      this._changed();
    }

    get(name) {
      name = String(name);
      for (var i = 0; i < this._entries.length; i++) {
        if (this._entries[i][0] === name) return this._entries[i][1];
      }
      return null;
    }

    getAll(name) {
      name = String(name);
      return this._entries
        .filter(function (pair) { return pair[0] === name; })
        .map(function (pair) { return pair[1]; });
    }

    has(name, value) {
      name = String(name);
      if (value === undefined) {
        return this._entries.some(function (pair) { return pair[0] === name; });
      }
      value = String(value);
      return this._entries.some(function (pair) { return pair[0] === name && pair[1] === value; });
    }

    set(name, value) {
      name = String(name);
      value = String(value);
      for (var i = 0; i < this._entries.length; i++) {
        if (this._entries[i][0] === name) {
          this._entries[i][1] = value;
          this._entries = this._entries.slice(0, i + 1).concat(
            this._entries.slice(i + 1).filter(function (pair) { return pair[0] !== name; }),
          );
          this._changed();
          return;
        }
      }
      this._entries.push([name, value]);
      this._changed();
    }

    sort() {
      var indexed = this._entries.map(function (pair, index) { return { pair: pair, index: index }; });
      indexed.sort(function (a, b) {
        if (a.pair[0] < b.pair[0]) return -1;
        if (a.pair[0] > b.pair[0]) return 1;
        return a.index - b.index;
      });
      this._entries = indexed.map(function (entry) { return entry.pair; });
      this._changed();
    }

    get size() { return this._entries.length; }

    forEach(callback, thisArg) {
      for (var i = 0; i < this._entries.length; i++) {
        callback.call(thisArg, this._entries[i][1], this._entries[i][0], this);
      }
    }

    keys() { return this._entries.map(function (pair) { return pair[0]; })[Symbol.iterator](); }
    values() { return this._entries.map(function (pair) { return pair[1]; })[Symbol.iterator](); }
    entries() { return this._entries.map(function (pair) { return [pair[0], pair[1]]; })[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }

    toString() {
      var self = this;
      return this._entries
        .map(function (pair) { return percentEncode(pair[0]) + "=" + percentEncode(pair[1]); })
        .join("&");
    }
  }
  Object.defineProperty(URLSearchParams.prototype, Symbol.toStringTag, {
    value: "URLSearchParams",
    configurable: true,
  });

  function parseUrl(input, base) {
    try {
      var text = String(input);
      return JSON.parse(
        base === undefined ? host.urlParse(text) : host.urlParse(text, String(base)),
      );
    } catch (error) {
      var failure = new TypeError("Invalid URL");
      failure.code = "ERR_INVALID_URL";
      failure.input = String(input);
      throw failure;
    }
  }

  class URL {
    constructor(input, base) {
      if (input instanceof URL) input = input.href;
      this._parts = parseUrl(input, base);
      this._searchParams = new URLSearchParams(this._parts.search);
      this._searchParams._owner = this;
    }

    _setParts(parts) {
      this._parts = parts;
      var fresh = new URLSearchParams(parts.search);
      this._searchParams._entries = fresh._entries;
    }

    _syncFromSearchParams() {
      var text = this._searchParams.toString();
      var href = this._parts.href;
      var hashIndex = href.indexOf("#");
      var hash = hashIndex === -1 ? "" : href.slice(hashIndex);
      var noHash = hashIndex === -1 ? href : href.slice(0, hashIndex);
      var queryIndex = noHash.indexOf("?");
      var body = queryIndex === -1 ? noHash : noHash.slice(0, queryIndex);
      this._parts = parseUrl(body + (text ? "?" + text : "") + hash);
    }

    get href() { return this._parts.href; }
    set href(value) { this._setParts(parseUrl(value)); }
    get protocol() { return this._parts.protocol; }
    set protocol(value) {
      var text = String(value);
      if (text.slice(-1) !== ":") text += ":";
      this.href = text + this._parts.href.slice(this._parts.protocol.length);
    }
    get username() { return this._parts.username; }
    set username(value) {
      this._setCredentials(String(value), this._parts.password);
    }
    get password() { return this._parts.password; }
    set password(value) {
      this._setCredentials(this._parts.username, String(value));
    }
    _setCredentials(username, password) {
      var encodedUser = encodeURIComponent(username).replace(/[!'()*]/g, function (ch) {
        return "%" + ch.charCodeAt(0).toString(16).toUpperCase();
      });
      var encodedPassword = encodeURIComponent(password).replace(/[!'()*]/g, function (ch) {
        return "%" + ch.charCodeAt(0).toString(16).toUpperCase();
      });
      var userinfo = encodedUser;
      if (encodedPassword) userinfo += ":" + encodedPassword;
      this.href = this._parts.protocol + "//" +
        (userinfo ? userinfo + "@" : "") +
        this._parts.host + this._parts.pathname + this._parts.search + this._parts.hash;
    }
    get host() { return this._parts.host; }
    set host(value) {
      this.href = this._parts.protocol + "//" + value + this._parts.pathname + this._parts.search + this._parts.hash;
    }
    get hostname() { return this._parts.hostname; }
    set hostname(value) {
      this.href = this._parts.protocol + "//" + value +
        (this._parts.port ? ":" + this._parts.port : "") +
        this._parts.pathname + this._parts.search + this._parts.hash;
    }
    get port() { return this._parts.port; }
    set port(value) {
      this.href = this._parts.protocol + "//" + this._parts.hostname +
        (value ? ":" + value : "") + this._parts.pathname + this._parts.search + this._parts.hash;
    }
    get pathname() { return this._parts.pathname; }
    set pathname(value) {
      this.href = this._parts.protocol + "//" + this._parts.host + String(value) + this._parts.search + this._parts.hash;
    }
    get search() {
      // WHATWG: 空的 query (`?`) 在 `href` 中保留，但 `.search` 是空串。
      return this._parts.search === "?" ? "" : this._parts.search;
    }
    set search(value) {
      var text = String(value);
      if (text && text.charAt(0) !== "?") text = "?" + text;
      this.href = this._parts.href.split("#")[0].split("?")[0] + text + this._parts.hash;
    }
    get hash() {
      return this._parts.hash === "#" ? "" : this._parts.hash;
    }
    set hash(value) {
      var text = String(value);
      if (text && text.charAt(0) !== "#") text = "#" + text;
      this.href = this._parts.href.split("#")[0] + text;
    }
    get origin() { return this._parts.origin; }
    get searchParams() { return this._searchParams; }

    toString() { return this._parts.href; }
    toJSON() { return this._parts.href; }

    static canParse(input, base) {
      try {
        parseUrl(input, base);
        return true;
      } catch (_) {
        return false;
      }
    }

    static parse(input, base) {
      try {
        return new URL(input, base);
      } catch (_) {
        return null;
      }
    }
  }
  Object.defineProperty(URL.prototype, Symbol.toStringTag, {
    value: "URL",
    configurable: true,
  });

  Object.defineProperty(globalThis, "URL", { value: URL, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "URLSearchParams", {
    value: URLSearchParams,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Web Streams (design §6.5)
  // ---------------------------------------------------------------------

  class ReadableStream {
    constructor(source, strategy) {
      this._source = source || {};
      this._queue = [];
      this._waiters = [];
      this._closed = false;
      this._errored = null;
      this._pulling = false;
      this._closeWaiters = [];
      this._errorWaiters = [];
      this._locked = false;
      this._bom = strategy;
      var self = this;
      this._controller = {
        get desiredSize() { return 1; },
        enqueue: function (chunk) { self._enqueue(chunk); },
        close: function () { self._close(); },
        error: function (reason) { self._error(reason); },
      };
      if (typeof this._source.start === "function") {
        try {
          var result = this._source.start(this._controller);
          Promise.resolve(result).catch(function (error) { self._error(error); });
        } catch (error) {
          this._error(error);
        }
      }
      // Design §6.5: `pull` may enqueue synchronously before `read()` attaches a
      // waiter. Re-pumping here plus in `read()` keeps that ordering safe.
      this._pump();
    }

    _enqueue(chunk) {
      if (this._closed || this._errored) return;
      if (this._waiters.length > 0) {
        var waiter = this._waiters.shift();
        waiter.resolve({ value: chunk, done: false });
        this._pump();
      } else {
        this._queue.push(chunk);
      }
    }

    _close() {
      if (this._closed) return;
      this._closed = true;
      while (this._waiters.length > 0) {
        this._waiters.shift().resolve({ value: undefined, done: true });
      }
      while (this._closeWaiters.length > 0) this._closeWaiters.shift()(undefined);
      this._errorWaiters.length = 0;
    }

    _error(reason) {
      if (this._errored) return;
      this._errored = reason;
      while (this._waiters.length > 0) this._waiters.shift().reject(reason);
      while (this._errorWaiters.length > 0) this._errorWaiters.shift()(reason);
      this._closeWaiters.length = 0;
    }

    _pump() {
      if (this._pulling || this._closed || this._errored) return;
      if (this._waiters.length === 0) return;
      if (typeof this._source.pull !== "function") return;
      var self = this;
      this._pulling = true;
      var finish = function () {
        self._pulling = false;
        // A consumer may have called `read()` while the asynchronous `pull` was
        // still in flight. Re-pump so the new waiter is served (design §6.5).
        self._pump();
      };
      try {
        var result = this._source.pull(this._controller);
        Promise.resolve(result).then(finish, function (error) {
          self._pulling = false;
          self._error(error);
        });
      } catch (error) {
        this._pulling = false;
        this._error(error);
      }
    }

    read() {
      var self = this;
      this._disturbed = true;
      if (this._queue.length > 0) {
        return Promise.resolve({ value: this._queue.shift(), done: false });
      }
      if (this._errored) return Promise.reject(this._errored);
      if (this._closed) return Promise.resolve({ value: undefined, done: true });
      return new Promise(function (resolve, reject) {
        self._waiters.push({ resolve: resolve, reject: reject });
        // Re-pump **after** the waiter is attached (the required regression).
        self._pump();
      });
    }

    getReader() {
      var self = this;
      this._locked = true;
      return {
        read: function () { return self.read(); },
        cancel: function (reason) { return self.cancel(reason); },
        releaseLock: function () { self._locked = false; },
        get closed() {
          if (self._errored) return Promise.reject(self._errored);
          if (self._closed) return Promise.resolve(undefined);
          return new Promise(function (resolve, reject) {
            self._closeWaiters.push(resolve);
            self._errorWaiters.push(reject);
          });
        },
      };
    }

    cancel(reason) {
      this._close();
      if (typeof this._source.cancel === "function") {
        try {
          return Promise.resolve(this._source.cancel(reason));
        } catch (error) {
          return Promise.reject(error);
        }
      }
      return Promise.resolve();
    }

    get locked() { return this._locked; }

    [Symbol.asyncIterator]() {
      var self = this;
      return {
        next: function () { return self.read(); },
        return: function (value) {
          return Promise.resolve({ value: value, done: true });
        },
        [Symbol.asyncIterator]: function () { return this; },
      };
    }
  }
  Object.defineProperty(ReadableStream.prototype, Symbol.toStringTag, {
    value: "ReadableStream",
    configurable: true,
  });

  class WritableStream {
    constructor(sink, strategy) {
      this._sink = sink || {};
      this._bom = strategy;
      this._writer = null;
      this._closed = false;
    }
    getWriter() {
      var self = this;
      if (this._writer !== null) {
        throw new TypeError("This writable stream is locked to a writer.");
      }
      var writer = {
        write: function (chunk) {
          if (typeof self._sink.write === "function") return Promise.resolve(self._sink.write(chunk));
          return Promise.resolve();
        },
        close: function () {
          self._closed = true;
          if (typeof self._sink.close === "function") return Promise.resolve(self._sink.close());
          return Promise.resolve();
        },
        abort: function (reason) {
          if (typeof self._sink.abort === "function") return Promise.resolve(self._sink.abort(reason));
          return Promise.resolve();
        },
        releaseLock: function () { self._writer = null; },
        get closed() { return Promise.resolve(); },
      };
      this._writer = writer;
      return writer;
    }
    get locked() { return this._writer !== null; }
    abort(reason) {
      if (typeof this._sink.abort === "function") return Promise.resolve(this._sink.abort(reason));
      return Promise.resolve();
    }
  }
  Object.defineProperty(WritableStream.prototype, Symbol.toStringTag, {
    value: "WritableStream",
    configurable: true,
  });

  class TransformStream {
    constructor(transformer, writableStrategy, readableStrategy) {
      transformer = transformer || {};
      var self = this;
      this.readable = new ReadableStream({
        start: function (controller) { self._controller = controller; },
      });
      this.writable = new WritableStream({
        write: function (chunk) {
          if (typeof transformer.transform === "function") {
            return transformer.transform(chunk, self._controller);
          }
          if (self._controller) self._controller.enqueue(chunk);
          return undefined;
        },
        close: function () {
          if (typeof transformer.flush === "function") {
            return Promise.resolve(transformer.flush(self._controller)).then(function () {
              if (self._controller) self._controller.close();
            });
          }
          if (self._controller) self._controller.close();
          return undefined;
        },
      });
    }
  }

  class ByteLengthQueuingStrategy {
    constructor(options) { this.highWaterMark = options && options.highWaterMark; }
    size(chunk) { return chunk.byteLength; }
  }
  class CountQueuingStrategy {
    constructor(options) { this.highWaterMark = options && options.highWaterMark; }
    size() { return 1; }
  }

  Object.defineProperty(globalThis, "ReadableStream", {
    value: ReadableStream,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "WritableStream", {
    value: WritableStream,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "TransformStream", {
    value: TransformStream,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "ByteLengthQueuingStrategy", {
    value: ByteLengthQueuingStrategy,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(globalThis, "CountQueuingStrategy", {
    value: CountQueuingStrategy,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Headers / Response / Request / fetch (design §6.5)
  // ---------------------------------------------------------------------

  function normalizeHeaderName(name) {
    name = String(name);
    if (name === "" || /[^!#$%&'*+\-.^_`|~0-9A-Za-z]/.test(name)) {
      throw new TypeError("Invalid header name: " + name);
    }
    return name.toLowerCase();
  }

  var HEADER_NAME_PATTERN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;

  /// Node/undici 的 Headers 模型：内部按小写名分组，迭代时按名字排序并把
  /// 重复值用 ", " 合并（`set-cookie` 例外：逐条迭代、`getSetCookie()` 返回数组）。
  function validateHeaderName(name, method) {
    var text = String(name);
    if (text === "" || !HEADER_NAME_PATTERN.test(text)) {
      throw new TypeError(
        "Headers." + method + ": \"" + text + "\" is an invalid header name.",
      );
    }
    return text.toLowerCase();
  }

  class Headers {
    constructor(init) {
      this._map = Object.create(null);
      this._names = [];
      if (init === undefined || init === null) return;
      if (init instanceof Headers) {
        for (var i = 0; i < init._names.length; i++) {
          var name = init._names[i];
          var values = init._map[name];
          for (var j = 0; j < values.length; j++) this.append(name, values[j]);
        }
        return;
      }
      if (typeof init[Symbol.iterator] === "function") {
        var iterator = init[Symbol.iterator]();
        var step = iterator.next();
        while (!step.done) {
          var pair = step.value;
          this.append(pair[0], pair[1]);
          step = iterator.next();
        }
        return;
      }
      var keys = Object.keys(init);
      for (var k = 0; k < keys.length; k++) this.append(keys[k], init[keys[k]]);
    }

    _bucket(lower) {
      var values = this._map[lower];
      if (values === undefined) {
        values = [];
        this._map[lower] = values;
        this._names.push(lower);
      }
      return values;
    }

    append(name, value) {
      var lower = validateHeaderName(name, "append");
      this._bucket(lower).push(String(value));
    }

    set(name, value) {
      var lower = validateHeaderName(name, "set");
      this._map[lower] = [String(value)];
      if (this._names.indexOf(lower) === -1) this._names.push(lower);
    }

    get(name) {
      var lower = validateHeaderName(name, "get");
      var values = this._map[lower];
      return values === undefined ? null : values.join(", ");
    }

    getSetCookie() {
      var values = this._map["set-cookie"];
      return values === undefined ? [] : values.slice();
    }

    has(name) {
      var lower = validateHeaderName(name, "has");
      return this._map[lower] !== undefined;
    }

    delete(name) {
      var lower = validateHeaderName(name, "delete");
      if (this._map[lower] === undefined) return;
      delete this._map[lower];
      this._names = this._names.filter(function (entry) { return entry !== lower; });
    }

    _sortedNames() {
      return this._names.slice().sort();
    }

    forEach(callback, thisArg) {
      var names = this._sortedNames();
      for (var i = 0; i < names.length; i++) {
        var values = this._map[names[i]];
        if (names[i] === "set-cookie") {
          for (var j = 0; j < values.length; j++) {
            callback.call(thisArg, values[j], names[i], this);
          }
        } else {
          callback.call(thisArg, values.join(", "), names[i], this);
        }
      }
    }

    entries() {
      var pairs = [];
      var names = this._sortedNames();
      for (var i = 0; i < names.length; i++) {
        var values = this._map[names[i]];
        if (names[i] === "set-cookie") {
          for (var j = 0; j < values.length; j++) pairs.push([names[i], values[j]]);
        } else {
          pairs.push([names[i], values.join(", ")]);
        }
      }
      return pairs[Symbol.iterator]();
    }
    keys() {
      var names = this._sortedNames();
      return names[Symbol.iterator]();
    }
    values() {
      var values = [];
      var iterator = this.entries();
      var step = iterator.next();
      while (!step.done) {
        values.push(step.value[1]);
        step = iterator.next();
      }
      return values[Symbol.iterator]();
    }
    [Symbol.iterator]() { return this.entries(); }
    _pairs() {
      // fetch 传输用：保留每个 (name, value)，名字保留首次出现的写法。
      var pairs = [];
      for (var i = 0; i < this._names.length; i++) {
        var name = this._names[i];
        var values = this._map[name];
        for (var j = 0; j < values.length; j++) pairs.push([name, values[j]]);
      }
      return pairs;
    }
  }
  Object.defineProperty(Headers.prototype, Symbol.toStringTag, {
    value: "Headers",
    configurable: true,
  });

  function bodyToBytes(body) {
    if (body === undefined || body === null) return null;
    if (typeof body === "string") return encodeUTF8(body);
    if (body instanceof URLSearchParams) return encodeUTF8(body.toString());
    if (body instanceof Uint8Array) return body;
    if (body instanceof ArrayBuffer) return new Uint8Array(body);
    if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
    if (body instanceof ReadableStream) return body; // drained/handled by fetch
    if (typeof Blob !== "undefined" && body instanceof Blob) return body._bytes;
    if (typeof FormData !== "undefined" && body instanceof FormData) return encodeFormData(body).bytes;
    return encodeUTF8(String(body));
  }

  /// BodyInit -> { data, contentType } (undici's `extractBody`).
  function extractBody(body) {
    if (typeof FormData !== "undefined" && body instanceof FormData) {
      var encoded = encodeFormData(body);
      return {
        data: encoded.bytes,
        contentType: "multipart/form-data; boundary=" + encoded.boundary,
      };
    }
    var contentType = null;
    if (typeof body === "string") contentType = "text/plain;charset=UTF-8";
    else if (body instanceof URLSearchParams) contentType = "application/x-www-form-urlencoded;charset=UTF-8";
    else if (typeof Blob !== "undefined" && body instanceof Blob && body.type) contentType = body.type;
    return { data: bodyToBytes(body), contentType: contentType };
  }

  function consumeBody(response) {
    if (response.bodyUsed) {
      return Promise.reject(new TypeError("Body is unusable: Body has already been read"));
    }
    response._bodyUsed = true;
    if (response._bytes !== null) {
      return Promise.resolve(response._bytes);
    }
    var reader = response.body ? response.body.getReader() : null;
    if (reader === null) return Promise.resolve(new Uint8Array(0));
    var chunks = [];
    function pump() {
      return reader.read().then(function (result) {
        if (result.done) {
          var total = 0;
          for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
          var merged = new Uint8Array(total);
          var offset = 0;
          for (var j = 0; j < chunks.length; j++) {
            merged.set(chunks[j], offset);
            offset += chunks[j].length;
          }
          return merged;
        }
        chunks.push(toBytes(result.value));
        return pump();
      });
    }
    return pump();
  }

  // ---------------------------------------------------------------------
  // Blob / File（Node 18+ 全局；SDK 与 FormData 路径会做 `instanceof Blob`）
  // ---------------------------------------------------------------------

  class Blob {
    constructor(parts, options) {
      options = options || {};
      var list = parts === undefined || parts === null ? [] : parts;
      var chunks = [];
      var total = 0;
      for (var i = 0; i < list.length; i++) {
        var part = list[i];
        var bytes = part instanceof Blob ? part._bytes : toBytes(part);
        chunks.push(bytes);
        total += bytes.length;
      }
      var merged = new Uint8Array(total);
      var offset = 0;
      for (var j = 0; j < chunks.length; j++) {
        merged.set(chunks[j], offset);
        offset += chunks[j].length;
      }
      this._bytes = merged;
      this.size = total;
      this.type = options.type === undefined ? "" : String(options.type).toLowerCase();
    }

    arrayBuffer() {
      var copy = new Uint8Array(this._bytes.length);
      copy.set(this._bytes);
      return Promise.resolve(copy.buffer);
    }

    bytes() {
      var copy = new Uint8Array(this._bytes.length);
      copy.set(this._bytes);
      return Promise.resolve(copy);
    }

    text() {
      return Promise.resolve(decodeUTF8(this._bytes, false, false).text);
    }

    slice(start, end, type) {
      var length = this._bytes.length;
      var begin = start === undefined ? 0 : start < 0 ? Math.max(length + start, 0) : Math.min(start, length);
      var finish = end === undefined ? length : end < 0 ? Math.max(length + end, 0) : Math.min(end, length);
      if (finish < begin) finish = begin;
      return new Blob([this._bytes.subarray(begin, finish)], {
        type: type === undefined ? this.type : type,
      });
    }

    stream() {
      var bytes = this._bytes;
      return new ReadableStream({
        start: function (controller) {
          if (bytes.length > 0) controller.enqueue(bytes);
          controller.close();
        },
      });
    }
  }
  Object.defineProperty(Blob.prototype, Symbol.toStringTag, { value: "Blob", configurable: true });

  class File extends Blob {
    constructor(parts, name, options) {
      super(parts, options);
      this.name = String(name);
      this.lastModified = options !== undefined && options.lastModified !== undefined
        ? Number(options.lastModified)
        : Date.now();
    }
  }
  Object.defineProperty(File.prototype, Symbol.toStringTag, { value: "File", configurable: true });

  // ---------------------------------------------------------------------
  // FormData（Node 18+ 全局；fetch 的 multipart 编码用）
  // ---------------------------------------------------------------------

  function toFormFile(value, filename) {
    if (typeof File === "undefined") return value;
    if (value instanceof File) {
      if (filename === undefined) return value;
      return new File([value], String(filename), {
        type: value.type,
        lastModified: value.lastModified,
      });
    }
    if (value instanceof Blob) {
      var name = filename === undefined ? "blob" : String(filename);
      return new File([value], name, { type: value.type, lastModified: Date.now() });
    }
    return value;
  }

  class FormData {
    constructor() {
      this._entries = [];
    }

    _matches(name) {
      // FormData 的名字大小写敏感（与 Headers 不同）。
      var text = String(name);
      var matches = [];
      for (var i = 0; i < this._entries.length; i++) {
        if (String(this._entries[i][0]) === text) matches.push(i);
      }
      return matches;
    }

    append(name, value, filename) {
      this._entries.push([String(name), toFormFile(value, filename), filename]);
    }

    set(name, value, filename) {
      this.delete(name);
      this.append(name, value, filename);
    }

    get(name) {
      var matches = this._matches(name);
      return matches.length === 0 ? null : this._entries[matches[0]][1];
    }

    getAll(name) {
      var self = this;
      return this._matches(name).map(function (index) { return self._entries[index][1]; });
    }

    has(name) {
      return this._matches(name).length > 0;
    }

    delete(name) {
      var matches = this._matches(name);
      for (var i = matches.length - 1; i >= 0; i--) this._entries.splice(matches[i], 1);
    }

    forEach(callback, thisArg) {
      for (var i = 0; i < this._entries.length; i++) {
        callback.call(thisArg, this._entries[i][1], String(this._entries[i][0]), this);
      }
    }

    *entries() {
      for (var i = 0; i < this._entries.length; i++) { yield [String(this._entries[i][0]), this._entries[i][1]]; }
    }

    *keys() {
      for (var i = 0; i < this._entries.length; i++) yield String(this._entries[i][0]);
    }

    *values() {
      for (var i = 0; i < this._entries.length; i++) yield this._entries[i][1];
    }

    [Symbol.iterator]() {
      return this.entries();
    }
  }
  Object.defineProperty(FormData.prototype, Symbol.toStringTag, { value: "FormData", configurable: true });

  var formDataSequence = 0;

  /// `multipart/form-data` 序列化：`{ boundary, bytes }`。
  function encodeFormData(form) {
    var boundary = "----kianaFormBoundary" + (++formDataSequence) + String(Date.now());
    var chunks = [];
    var total = 0;
    function push(text) { var bytes = encodeUTF8(text); chunks.push(bytes); total += bytes.length; }
    function pushBytes(bytes) { chunks.push(bytes); total += bytes.length; }
    for (var i = 0; i < form._entries.length; i++) {
      var name = String(form._entries[i][0]);
      var value = form._entries[i][1];
      push("--" + boundary + "\r\n");
      if (value instanceof Blob) {
        var filename = form._entries[i][2] !== undefined
          ? String(form._entries[i][2])
          : value instanceof File ? value.name : "blob";
        push('Content-Disposition: form-data; name="' + name + '"; filename="' + filename + '"\r\n');
        push("Content-Type: " + (value.type || "application/octet-stream") + "\r\n\r\n");
        pushBytes(value._bytes);
        push("\r\n");
      } else {
        push('Content-Disposition: form-data; name="' + name + '"\r\n\r\n');
        push(String(value));
        push("\r\n");
      }
    }
    push("--" + boundary + "--\r\n");
    var merged = new Uint8Array(total);
    var offset = 0;
    for (var j = 0; j < chunks.length; j++) {
      merged.set(chunks[j], offset);
      offset += chunks[j].length;
    }
    return { boundary: boundary, bytes: merged };
  }

  /// Tee a `ReadableStream` into two independent branches (Response.clone).
  ///
  /// The source is pumped eagerly into per-branch queues; this runtime's streams
  /// have no backpressure, and `clone()` is synchronous, so buffering is the
  /// only way both branches can be read later.
  function teeReadableStream(stream) {
    var reader = stream.getReader();
    var controls = [null, null];
    var queues = [[], []];
    var done = false;
    var failure = null;
    var pulling = false;

    function push(index, chunk) {
      if (controls[index] !== null) controls[index].enqueue(chunk);
      else queues[index].push(chunk);
    }
    function finish() {
      done = true;
      for (var i = 0; i < 2; i++) {
        if (controls[i] !== null) controls[i].close();
      }
    }
    function fail(error) {
      failure = error;
      for (var i = 0; i < 2; i++) {
        if (controls[i] !== null) controls[i].error(error);
      }
    }
    function readNext() {
      if (pulling || done || failure !== null) return;
      pulling = true;
      reader.read().then(function (result) {
        pulling = false;
        if (result.done) {
          finish();
          return;
        }
        push(0, result.value);
        push(1, result.value);
        readNext();
      }, function (error) {
        pulling = false;
        fail(error);
      });
    }

    var branches = [0, 1].map(function (index) {
      return new ReadableStream({
        start: function (controller) {
          controls[index] = controller;
          while (queues[index].length > 0) controller.enqueue(queues[index].shift());
          if (done) controller.close();
          else if (failure !== null) controller.error(failure);
        },
      });
    });
    readNext();
    return branches;
  }

  class Response {
    constructor(body, init) {
      init = init || {};
      this.status = init.status === undefined ? 200 : init.status | 0;
      this.statusText = init.statusText === undefined ? "" : String(init.statusText);
      this.ok = this.status >= 200 && this.status < 300;
      this.url = init.url === undefined ? "" : String(init.url);
      this.redirected = !!init.redirected;
      this.type = init.type === undefined ? "default" : String(init.type);
      this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      this._bodyUsed = false;
      this._bytes = null;
      var extracted = extractBody(body);
      if (extracted.contentType && !this.headers.has("content-type")) {
        this.headers.set("Content-Type", extracted.contentType);
      }
      var bytes = extracted.data;
      if (bytes instanceof ReadableStream) {
        this.body = bytes;
      } else if (bytes === null) {
        this.body = null;
      } else {
        this._bytes = bytes;
        this.body = new ReadableStream({
          start: function (controller) {
            if (bytes.length > 0) controller.enqueue(bytes);
            controller.close();
          },
        });
      }
    }

    get bodyUsed() {
      // 通过 body.getReader() 读到 done 之后，Node 也把 bodyUsed 置为 true
      // （任何扰动 body 的读取都算）。
      return this._bodyUsed === true ||
        (this.body !== null && this.body !== undefined && this.body._disturbed === true);
    }

    text() {
      return consumeBody(this).then(function (bytes) {
        return decodeUTF8(bytes, false, false).text;
      });
    }
    json() {
      return this.text().then(function (text) { return JSON.parse(text); });
    }
    arrayBuffer() {
      return consumeBody(this).then(function (bytes) {
        var buffer = new ArrayBuffer(bytes.length);
        new Uint8Array(buffer).set(bytes);
        return buffer;
      });
    }
    blob() {
      var self = this;
      return consumeBody(this).then(function (bytes) {
        return new Blob([bytes], { type: self.headers.get("content-type") || "" });
      });
    }
    clone() {
      if (this.bodyUsed) throw new TypeError("Body is unusable: Body has already been read");
      var init = {
        status: this.status,
        statusText: this.statusText,
        url: this.url,
        redirected: this.redirected,
        headers: new Headers(this.headers),
      };
      if (this._bytes !== null) return new Response(this._bytes, init);
      if (this.body === null) return new Response(null, init);
      // fetch() builds the Response from the stream: clone must tee it, not drop it.
      var branches = teeReadableStream(this.body);
      this.body = branches[0];
      return new Response(branches[1], init);
    }
  }
  Object.defineProperty(Response.prototype, Symbol.toStringTag, {
    value: "Response",
    configurable: true,
  });

  class Request {
    constructor(input, init) {
      init = init || {};
      var base = input instanceof Request ? input : null;
      this.url = base !== null
        ? base.url
        : input instanceof URL ? input.toString() : String(input);
      this.method = base !== null ? base.method : "GET";
      this.headers = new Headers(base !== null ? base.headers : undefined);
      this.body = base !== null ? base.body : null;
      this.signal = base !== null ? base.signal : null;
      if (init.headers !== undefined) {
        this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      }
      if (init.method !== undefined) this.method = String(init.method).toUpperCase();
      if (init.signal !== undefined) this.signal = init.signal;
      if (init.body !== undefined && init.body !== null) {
        var extracted = extractBody(init.body);
        this.body = extracted.data;
        if (extracted.contentType && !this.headers.has("content-type")) {
          this.headers.set("content-type", extracted.contentType);
        }
      }
    }
  }

  var fetchSequence = 0;

  function fetchFailure(value) {
    if (value && value.name === "AbortError") return value;
    if (value && (value.code === "ABORT_ERR" || value.name === "AbortError")) {
      return new DOMException(value.message || "This operation was aborted", "AbortError");
    }
    var cause = value instanceof Error ? value : nodeError(value);
    var error = new TypeError("fetch failed");
    error.cause = cause;
    return error;
  }

  function dataUrlResponse(input) {
    var text = String(input);
    var match = /^data:([^,]*),([\s\S]*)$/i.exec(text);
    if (!match) return null;
    var meta = match[1];
    var payload = match[2];
    var base64 = /;base64$/i.test(meta);
    var mime = meta.replace(/;base64$/i, "") || "text/plain;charset=US-ASCII";
    var decoded = percentDecode(payload.replace(/\+/g, "%2B"));
    var bytes = base64 ? base64Decode(decoded) : encodeUTF8(decoded);
    var headers = new Headers();
    headers.set("content-type", mime);
    return new Response(bytes, {
      status: 200,
      statusText: "OK",
      url: text,
      headers: headers,
    });
  }

  function readBodyStream(stream) {
    var reader = stream.getReader();
    var chunks = [];
    function pump() {
      return reader.read().then(function (result) {
        if (result.done) {
          var total = 0;
          for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
          var merged = new Uint8Array(total);
          var offset = 0;
          for (var j = 0; j < chunks.length; j++) {
            merged.set(chunks[j], offset);
            offset += chunks[j].length;
          }
          return merged;
        }
        chunks.push(toBytes(result.value));
        return pump();
      });
    }
    return pump();
  }

  function fetch(input, init) {
    return new Promise(function (resolve, reject) {
      var request;
      try {
        request = new Request(input, init);
      } catch (error) {
        reject(error);
        return;
      }
      var dataResponse = dataUrlResponse(request.url);
      if (dataResponse !== null) {
        resolve(dataResponse);
        return;
      }
      var id = ++fetchSequence;
      var state = { chunks: [], ended: false, error: null, controller: null };
      var signal = request.signal;
      var aborted = false;
      var onAbort = function () {
        aborted = true;
        host.fetchAbort(id);
      };
      if (signal) {
        if (signal.aborted) {
          onAbort();
        } else if (typeof signal.addEventListener === "function") {
          signal.addEventListener("abort", onAbort);
        }
      }

      function onEvent(event) {
        if (!event || typeof event !== "object") return;
        if (event.type === "chunk") {
          var bytes = base64Decode(event.b64 || "");
          if (state.controller !== null) state.controller.enqueue(bytes);
          else state.chunks.push(bytes);
          return;
        }
        if (event.type === "end") {
          state.ended = true;
          if (state.controller !== null) state.controller.close();
          return;
        }
        if (event.type === "error") {
          state.error = event;
          var failure = event.name === "AbortError"
            ? new DOMException(event.message || "This operation was aborted", "AbortError")
            : nodeError(event);
          if (state.controller !== null) state.controller.error(failure);
        }
      }

      function dispatch(bodyBytes) {
        if (aborted) {
          reject(new DOMException("This operation was aborted", "AbortError"));
          return;
        }
        var payload = {
          id: id,
          url: request.url,
          method: request.method,
          headers: request.headers._pairs(),
          body: bodyBytes === null || bodyBytes === undefined ? null : base64Encode(bodyBytes),
          onEvent: onEvent,
        };

        globalThis.__kiana.promise("fetch", payload).then(function (meta) {
          var headers = new Headers();
          for (var i = 0; i < meta.headers.length; i++) {
            headers.append(meta.headers[i][0], meta.headers[i][1]);
          }
          var body = new ReadableStream({
            start: function (controller) {
              state.controller = controller;
              for (var i = 0; i < state.chunks.length; i++) controller.enqueue(state.chunks[i]);
              state.chunks = [];
              if (state.ended) controller.close();
              else if (state.error !== null) {
                controller.error(nodeError(state.error));
              }
            },
          });
          var response = new Response(null, {
            status: meta.status,
            statusText: meta.statusText,
            url: meta.url,
            redirected: meta.redirected,
            type: "basic",
            headers: headers,
          });
          response.body = body;
          resolve(response);
        }).catch(function (value) {
          reject(fetchFailure(value));
        });
        void aborted;
      }

      if (request.body instanceof ReadableStream) {
        // Request bodies may be streams; this host transport takes a complete
        // payload, so drain the stream before dispatching (never drop it).
        readBodyStream(request.body).then(dispatch, function (value) {
          reject(fetchFailure(value));
        });
      } else {
        dispatch(request.body);
      }
    });
  }

  Object.defineProperty(globalThis, "Headers", { value: Headers, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "Blob", { value: Blob, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "File", { value: File, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "FormData", { value: FormData, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "Response", { value: Response, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "Request", { value: Request, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "fetch", { value: fetch, writable: true, enumerable: true, configurable: true });

  // ---------------------------------------------------------------------
  // structuredClone (design §6.6)
  // ---------------------------------------------------------------------

  function dataCloneError(message) {
    return new DOMException(message, "DataCloneError");
  }

  function cloneValue(value, memory) {
    if (value === null) return null;
    var type = typeof value;
    if (type === "undefined" || type === "boolean" || type === "number" || type === "string" ||
        type === "bigint") {
      return value;
    }
    if (type === "symbol") {
      throw dataCloneError(String(value) + " could not be cloned.");
    }
    if (type === "function") {
      throw dataCloneError(String(value) + " could not be cloned.");
    }
    if (memory.has(value)) return memory.get(value);

    var tag = Object.prototype.toString.call(value).slice(8, -1);
    if (tag === "WeakMap" || tag === "WeakSet" || tag === "WeakRef" ||
        tag === "FinalizationRegistry" || tag === "Promise" || tag === "Symbol") {
      throw dataCloneError("#<" + tag + "> could not be cloned.");
    }
    if (tag === "Proxy") {
      throw dataCloneError("#<Object> could not be cloned.");
    }
    if (value instanceof Date) {
      var date = new Date(value.getTime());
      memory.set(value, date);
      return date;
    }
    if (value instanceof RegExp) {
      var regexp = new RegExp(value.source, value.flags);
      // structuredClone resets `lastIndex` (V8 serializes the pattern only).
      regexp.lastIndex = 0;
      memory.set(value, regexp);
      return regexp;
    }
    if (value instanceof ArrayBuffer) {
      if (tag === "SharedArrayBuffer") throw dataCloneError("SharedArrayBuffer could not be cloned.");
      var buffer = value.slice(0);
      memory.set(value, buffer);
      return buffer;
    }
    if (ArrayBuffer.isView(value)) {
      var clonedBuffer = cloneValue(value.buffer, memory);
      var view;
      if (tag === "DataView") {
        view = new DataView(clonedBuffer, value.byteOffset, value.byteLength);
      } else {
        view = new value.constructor(clonedBuffer, value.byteOffset, value.length);
      }
      memory.set(value, view);
      return view;
    }
    if (tag === "Map") {
      var map = new Map();
      memory.set(value, map);
      value.forEach(function (entryValue, entryKey) {
        map.set(cloneValue(entryKey, memory), cloneValue(entryValue, memory));
      });
      return map;
    }
    if (tag === "Set") {
      var set = new Set();
      memory.set(value, set);
      value.forEach(function (entry) { set.add(cloneValue(entry, memory)); });
      return set;
    }
    if (value instanceof Error) {
      var clonedError;
      if (value instanceof TypeError) clonedError = new TypeError(value.message);
      else if (value instanceof RangeError) clonedError = new RangeError(value.message);
      else if (value instanceof SyntaxError) clonedError = new SyntaxError(value.message);
      else if (value instanceof ReferenceError) clonedError = new ReferenceError(value.message);
      else if (value instanceof URIError) clonedError = new URIError(value.message);
      else if (value instanceof EvalError) clonedError = new EvalError(value.message);
      else clonedError = new Error(value.message);
      clonedError.name = value.name;
      if (value.stack) clonedError.stack = value.stack;
      memory.set(value, clonedError);
      if (value.cause !== undefined) clonedError.cause = cloneValue(value.cause, memory);
      return clonedError;
    }
    if (Array.isArray(value)) {
      var array = new Array(value.length);
      memory.set(value, array);
      for (var i = 0; i < value.length; i++) {
        if (Object.prototype.hasOwnProperty.call(value, i)) {
          array[i] = cloneValue(value[i], memory);
        }
      }
      return array;
    }
    var object = {};
    memory.set(value, object);
    var keys = Object.keys(value);
    for (var j = 0; j < keys.length; j++) {
      object[keys[j]] = cloneValue(value[keys[j]], memory);
    }
    return object;
  }

  function structuredClone(value, options) {
    return cloneValue(value, new Map());
  }

  Object.defineProperty(globalThis, "structuredClone", {
    value: structuredClone,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // ---------------------------------------------------------------------
  // Event / EventTarget / navigator / MessageChannel / URLPattern
  // ---------------------------------------------------------------------
  //
  // 这些是 Node 26 的全局对象。设计 §6.2 没把它们列进「必须提供」的最小集，
  // 但 AbortSignal 之外的库（undici、SDK 的 EventTarget 用法）会碰到，缺失
  // 时是 `undefined` 而不是一个可用对象。这里提供 WHATWG 的最小语义。

  var EVENT_PHASES = { NONE: 0, CAPTURING_PHASE: 1, AT_TARGET: 2, BUBBLING_PHASE: 3 };

  class Event {
    constructor(type, options) {
      if (arguments.length === 0) {
        throw new TypeError("Failed to construct 'Event': 1 argument required, but only 0 present.");
      }
      options = options || {};
      this.type = String(type);
      this.bubbles = !!options.bubbles;
      this.cancelable = !!options.cancelable;
      this.composed = !!options.composed;
      this.defaultPrevented = false;
      this.eventPhase = 0;
      this.isTrusted = false;
      this.timeStamp = host.now();
      this.target = null;
      this.currentTarget = null;
      this.srcElement = null;
      this.returnValue = true;
      this.cancelBubble = false;
      this._stop = false;
      this._stopImmediate = false;
    }
    preventDefault() {
      if (this.cancelable) {
        this.defaultPrevented = true;
        this.returnValue = false;
      }
    }
    stopPropagation() { this._stop = true; this.cancelBubble = true; }
    stopImmediatePropagation() { this._stop = true; this._stopImmediate = true; this.cancelBubble = true; }
    composedPath() { return this.currentTarget ? [this.currentTarget] : []; }
  }
  Event.NONE = EVENT_PHASES.NONE;
  Event.CAPTURING_PHASE = EVENT_PHASES.CAPTURING_PHASE;
  Event.AT_TARGET = EVENT_PHASES.AT_TARGET;
  Event.BUBBLING_PHASE = EVENT_PHASES.BUBBLING_PHASE;
  Object.defineProperty(Event.prototype, Symbol.toStringTag, { value: "Event", configurable: true });

  class EventTarget {
    constructor() {
      Object.defineProperty(this, "_listeners", {
        value: new Map(),
        writable: true,
        enumerable: false,
        configurable: true,
      });
    }

    addEventListener(type, callback, options) {
      if (callback === undefined || callback === null) return;
      options = options || {};
      var capture = typeof options === "boolean" ? options : !!options.capture;
      var once = typeof options === "object" && !!options.once;
      var signal = typeof options === "object" ? options.signal : undefined;
      if (signal && signal.aborted) return;
      var name = String(type);
      var list = this._listeners.get(name);
      if (list === undefined) {
        list = [];
        this._listeners.set(name, list);
      }
      for (var i = 0; i < list.length; i++) {
        if (list[i].callback === callback && list[i].capture === capture) return;
      }
      var entry = { callback: callback, capture: capture, once: once, signal: signal };
      list.push(entry);
      if (signal && typeof signal.addEventListener === "function") {
        var self = this;
        signal.addEventListener("abort", function () {
          self.removeEventListener(name, callback, { capture: capture });
        });
      }
    }

    removeEventListener(type, callback, options) {
      var capture = typeof options === "boolean" ? options : !!(options && options.capture);
      var list = this._listeners.get(String(type));
      if (list === undefined) return;
      for (var i = 0; i < list.length; i++) {
        if (list[i].callback === callback && list[i].capture === capture) {
          list.splice(i, 1);
          return;
        }
      }
    }

    dispatchEvent(event) {
      if (!(event instanceof Event) && (!event || typeof event.type !== "string")) {
        throw new TypeError("The \"event\" argument must be an instance of Event.");
      }
      event.target = this;
      event.srcElement = this;
      event.currentTarget = this;
      event.eventPhase = EVENT_PHASES.AT_TARGET;
      var list = this._listeners.get(event.type);
      if (list !== undefined && list.length > 0) {
        list = list.slice();
        for (var i = 0; i < list.length; i++) {
          var entry = list[i];
          if (entry.once) this.removeEventListener(event.type, entry.callback, { capture: entry.capture });
          try {
            if (typeof entry.callback === "function") entry.callback.call(this, event);
            else if (entry.callback && typeof entry.callback.handleEvent === "function") {
              entry.callback.handleEvent(event);
            }
          } catch (error) {
            // Node 在下一轮微任务里重新抛出监听器异常，不打断其余监听器。
            (function (failure) {
              queueMicrotask(function () { throw failure; });
            })(error);
          }
          if (event._stopImmediate) break;
        }
      }
      var handler = this["on" + event.type];
      if (typeof handler === "function") {
        try { handler.call(this, event); } catch (error) {
          (function (failure) { queueMicrotask(function () { throw failure; }); })(error);
        }
      }
      event.eventPhase = EVENT_PHASES.NONE;
      event.currentTarget = null;
      return !event.defaultPrevented;
    }
  }
  Object.defineProperty(EventTarget.prototype, Symbol.toStringTag, {
    value: "EventTarget",
    configurable: true,
  });

  var navigatorLanguage = (function () {
    try {
      var value = String(host.locale() || "en-US");
      return value || "en-US";
    } catch (_) {
      return "en-US";
    }
  })();
  var navigatorObject = {};
  var navigatorPlatform = host.platform() === "win32"
    ? "Win32"
    : host.platform() === "darwin"
      ? "MacIntel"
      : "Linux " + host.arch();
  Object.defineProperties(navigatorObject, {
    hardwareConcurrency: { get: function () { return host.cpuCount(); }, enumerable: true },
    language: { get: function () { return navigatorLanguage; }, enumerable: true },
    languages: { get: function () { return [navigatorLanguage]; }, enumerable: true },
    platform: { get: function () { return navigatorPlatform; }, enumerable: true },
    userAgent: { get: function () { return "Node.js/22"; }, enumerable: true },
  });
  Object.defineProperty(navigatorObject, Symbol.toStringTag, {
    value: "Navigator",
    configurable: true,
  });
  Object.defineProperty(globalThis, "navigator", {
    value: navigatorObject,
    writable: false,
    enumerable: false,
    configurable: true,
  });

  class MessagePort extends EventTarget {
    constructor() {
      super();
      this._other = null;
      this._closed = false;
      this._started = false;
      this.onmessage = null;
      this.onmessageerror = null;
    }
    postMessage(value, transfer) {
      if (this._closed) return;
      var target = this._other;
      setTimeout(function () {
        if (target === null || target._closed) return;
        var event = { type: "message", data: value, target: target, currentTarget: target };
        try {
          target.dispatchEvent(event);
        } catch (error) {
          (function (failure) { queueMicrotask(function () { throw failure; }); })(error);
        }
      }, 0);
    }
    start() { this._started = true; }
    close() { this._closed = true; }
    ref() { return this; }
    unref() { return this; }
  }
  Object.defineProperty(MessagePort.prototype, Symbol.toStringTag, {
    value: "MessagePort",
    configurable: true,
  });

  class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }
  Object.defineProperty(MessageChannel.prototype, Symbol.toStringTag, {
    value: "MessageChannel",
    configurable: true,
  });

  /// URLPattern 的最小实现：`:name` 命名组、`*` 通配、其余按字面量。
  class URLPattern {
    constructor(input, baseURL) {
      var init = input;
      if (typeof input === "string") {
        if (baseURL !== undefined) {
          try {
            init = { pathname: input };
            if (/^[a-z][a-z0-9+.-]*:\/\//i.test(input)) init = { pathname: new URL(input).pathname };
          } catch (_) {
            init = { pathname: input };
          }
        } else {
          init = { pathname: input };
        }
      }
      init = init || {};
      var keys = ["protocol", "username", "password", "hostname", "port", "pathname", "search", "hash"];
      this._patterns = {};
      this._base = baseURL === undefined
        ? (typeof globalThis.location !== "undefined" ? globalThis.location.href : undefined)
        : String(baseURL);
      for (var i = 0; i < keys.length; i++) {
        var key = keys[i];
        var value = init[key];
        if (value === undefined && key === "pathname") value = "*";
        if (value === undefined) {
          this._patterns[key] = null;
          continue;
        }
        this._patterns[key] = compileUrlPattern(String(value));
      }
    }

    _components(input) {
      var url;
      try {
        if (input instanceof URL) url = input;
        else if (this._base !== undefined) url = new URL(String(input), this._base);
        else url = new URL(String(input));
      } catch (_) {
        return null;
      }
      return {
        protocol: url.protocol.replace(/:$/, ""),
        username: url.username,
        password: url.password,
        hostname: url.hostname,
        port: url.port,
        pathname: url.pathname,
        search: url.search.replace(/^\?/, ""),
        hash: url.hash.replace(/^#/, ""),
      };
    }

    exec(input) {
      var values = this._components(input);
      if (values === null) return null;
      var result = { inputs: [String(input)] };
      var keys = ["protocol", "username", "password", "hostname", "port", "pathname", "search", "hash"];
      for (var i = 0; i < keys.length; i++) {
        var key = keys[i];
        var pattern = this._patterns[key];
        if (pattern === null) {
          result[key] = { input: values[key], groups: { 0: values[key] } };
          continue;
        }
        var match = pattern.regexp.exec(values[key]);
        if (match === null) return null;
        var groups = { 0: match[0] };
        for (var g = 0; g < pattern.names.length; g++) {
          groups[pattern.names[g]] = match[g + 1] === undefined ? "" : match[g + 1];
        }
        result[key] = { input: values[key], groups: groups };
      }
      return result;
    }

    test(input) {
      return this.exec(input) !== null;
    }
  }
  Object.defineProperty(URLPattern.prototype, Symbol.toStringTag, {
    value: "URLPattern",
    configurable: true,
  });

  function compileUrlPattern(pattern) {
    var names = [];
    var source = "^";
    var index = 0;
    while (index < pattern.length) {
      var char = pattern.charAt(index);
      if (char === ":") {
        var start = index + 1;
        var cursor = start;
        while (cursor < pattern.length && /[A-Za-z0-9_]/.test(pattern.charAt(cursor))) cursor++;
        if (cursor === start) {
          source += ":";
          index++;
          continue;
        }
        names.push(pattern.slice(start, cursor));
        source += "([^/]+)";
        index = cursor;
      } else if (char === "*") {
        names.push(String(names.length));
        source += ".*";
        index++;
      } else {
        source += char.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
        index++;
      }
    }
    source += "$";
    return { regexp: new RegExp(source), names: names };
  }

  Object.defineProperty(globalThis, "Event", { value: Event, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "EventTarget", { value: EventTarget, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "MessagePort", { value: MessagePort, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "MessageChannel", { value: MessageChannel, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(globalThis, "URLPattern", { value: URLPattern, writable: true, enumerable: false, configurable: true });

  // ---------------------------------------------------------------------
  // node:* shim registry (filled by js/node/*.js)
  // ---------------------------------------------------------------------

  if (globalThis.__kiana_node_shims === undefined) {
    Object.defineProperty(globalThis, "__kiana_node_shims", {
      value: Object.create(null),
      writable: true,
      enumerable: false,
      configurable: true,
    });
  }
  if (globalThis.__kiana_node_internal === undefined) {
    Object.defineProperty(globalThis, "__kiana_node_internal", {
      value: internal,
      writable: true,
      enumerable: false,
      configurable: true,
    });
  }
})();
