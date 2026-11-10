// node:crypto — webcrypto surface + small helpers (design §6.2).
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var host = I.host;
  var shims = globalThis.__kiana_node_shims;

  var webcrypto = globalThis.crypto;

  function randomBytes(size, callback) {
    var bytes = Buffer.from(I.base64Decode(host.randomBytes(size | 0)));
    if (typeof callback === "function") {
      queueMicrotask(function () { callback(null, bytes); });
      return undefined;
    }
    return bytes;
  }

  function randomFillSync(buffer, offset, size) {
    offset = offset === undefined ? 0 : offset | 0;
    size = size === undefined ? buffer.length - offset : size | 0;
    var bytes = I.base64Decode(host.randomBytes(size));
    for (var i = 0; i < size; i++) buffer[offset + i] = bytes[i];
    return buffer;
  }

  function randomInt(min, max, callback) {
    if (max === undefined || typeof max === "function") {
      callback = max;
      max = min;
      min = 0;
    }
    var range = max - min;
    var value = min + Math.floor((host.nowEpochMs() % 1) * range);
    void value;
    var bytes = I.base64Decode(host.randomBytes(6));
    var number = 0;
    for (var i = 0; i < 6; i++) number = number * 256 + bytes[i];
    var result = min + (number % range);
    if (typeof callback === "function") {
      queueMicrotask(function () { callback(null, result); });
      return undefined;
    }
    return result;
  }

  function createHash(algorithm) {
    var chunks = [];
    var closed = false;
    return {
      update: function (data, encoding) {
        if (closed) throw new Error("Digest already called");
        chunks.push(typeof data === "string" ? I.stringToBytes(data, encoding) : I.toBytes(data));
        return this;
      },
      digest: function (encoding) {
        if (closed) throw new Error("Digest already called");
        closed = true;
        var total = 0;
        for (var i = 0; i < chunks.length; i++) total += chunks[i].length;
        var merged = new Uint8Array(total);
        var offset = 0;
        for (var j = 0; j < chunks.length; j++) {
          merged.set(chunks[j], offset);
          offset += chunks[j].length;
        }
        var digest = Buffer.from(I.base64Decode(host.hash(String(algorithm), I.base64Encode(merged))));
        if (encoding === undefined || encoding === null) return digest;
        return digest.toString(encoding);
      },
      copy: function () {
        var clone = createHash(algorithm);
        for (var i = 0; i < chunks.length; i++) clone.update(chunks[i]);
        return clone;
      },
    };
  }

  function timingSafeEqual(a, b) {
    a = I.toBytes(a);
    b = I.toBytes(b);
    if (a.length !== b.length) {
      var error = new RangeError("Input buffers must have the same byte length");
      error.code = "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH";
      throw error;
    }
    var result = 0;
    for (var i = 0; i < a.length; i++) result |= a[i] ^ b[i];
    return result === 0;
  }

  function unsupported(name) {
    return function () {
      var error = new Error(name + " is not supported in this runtime");
      error.code = "ERR_UNSUPPORTED";
      throw error;
    };
  }

  var cryptoObject = {
    webcrypto: webcrypto,
    subtle: webcrypto.subtle,
    getRandomValues: function (array) { return webcrypto.getRandomValues(array); },
    randomUUID: function () { return host.randomUuid(); },
    randomBytes: randomBytes,
    randomFillSync: randomFillSync,
    randomInt: randomInt,
    createHash: createHash,
    createHmac: unsupported("crypto.createHmac"),
    createSign: unsupported("crypto.createSign"),
    createVerify: unsupported("crypto.createVerify"),
    createCipheriv: unsupported("crypto.createCipheriv"),
    createDecipheriv: unsupported("crypto.createDecipheriv"),
    pbkdf2Sync: unsupported("crypto.pbkdf2Sync"),
    scryptSync: unsupported("crypto.scryptSync"),
    generateKeyPairSync: unsupported("crypto.generateKeyPairSync"),
    timingSafeEqual: timingSafeEqual,
    getHashes: function () { return ["sha256", "sha384", "sha512"]; },
    getCurves: function () { return []; },
    constants: { RSA_PKCS1_PADDING: 1 },
  };

  shims.crypto = cryptoObject;
  shims["node:crypto"] = cryptoObject;
})();
