// node:assert — minimal assertion helpers.
(function () {
  "use strict";

  var I = globalThis.__kiana_node_internal;
  var shims = globalThis.__kiana_node_shims;

  class AssertionError extends Error {
    constructor(options) {
      super(options && options.message ? options.message : "Assertion failed");
      this.name = "AssertionError";
      this.code = "ERR_ASSERTION";
      if (options) {
        this.actual = options.actual;
        this.expected = options.expected;
        this.operator = options.operator;
        this.generatedMessage = !options.message;
      }
    }
  }

  function fail(message) {
    throw new AssertionError({ message: message === undefined ? undefined : String(message) });
  }

  function ok(value, message) {
    if (!value) {
      throw new AssertionError({
        message: message === undefined ? "The expression evaluated to a falsy value" : String(message),
        actual: value,
        expected: true,
        operator: "==",
      });
    }
  }

  function equal(actual, expected, message) {
    if (actual != expected) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be loosely equal" : String(message), actual: actual, expected: expected, operator: "==" });
    }
  }

  function notEqual(actual, expected, message) {
    if (actual == expected) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be loosely not equal" : String(message), actual: actual, expected: expected, operator: "!=" });
    }
  }

  function strictEqual(actual, expected, message) {
    if (!Object.is(actual, expected)) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be strictly equal" : String(message), actual: actual, expected: expected, operator: "strictEqual" });
    }
  }

  function notStrictEqual(actual, expected, message) {
    if (Object.is(actual, expected)) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be strictly not equal" : String(message), actual: actual, expected: expected, operator: "notStrictEqual" });
    }
  }

  function deepEqual(actual, expected, message) {
    if (!shims.util.isDeepStrictEqual(actual, expected)) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be loosely deep-equal" : String(message), actual: actual, expected: expected, operator: "deepEqual" });
    }
  }

  function throws(fn, expected, message) {
    if (typeof fn !== "function") throw new TypeError("The \"fn\" argument must be of type function");
    try {
      fn();
    } catch (error) {
      if (expected instanceof RegExp && !expected.test(String(error.message !== undefined ? error.message : error))) {
        throw error;
      }
      if (typeof expected === "function" && !(error instanceof expected)) throw error;
      return error;
    }
    throw new AssertionError({ message: message === undefined ? "Missing expected exception" : String(message), operator: "throws" });
  }

  function rejects(promise, expected, message) {
    return Promise.resolve(promise).then(
      function () {
        throw new AssertionError({ message: message === undefined ? "Missing expected rejection" : String(message), operator: "rejects" });
      },
      function (error) {
        if (expected instanceof RegExp && !expected.test(String(error.message !== undefined ? error.message : error))) throw error;
        if (typeof expected === "function" && !(error instanceof expected)) throw error;
        return error;
      },
    );
  }

  function doesNotThrow(fn) {
    return fn();
  }

  var assertObject = ok;
  assertObject.ok = ok;
  assertObject.equal = equal;
  assertObject.notEqual = notEqual;
  assertObject.strictEqual = strictEqual;
  assertObject.notStrictEqual = notStrictEqual;
  assertObject.deepEqual = deepEqual;
  assertObject.deepStrictEqual = deepEqual;
  assertObject.notDeepEqual = function (actual, expected, message) {
    if (shims.util.isDeepStrictEqual(actual, expected)) {
      throw new AssertionError({ message: message === undefined ? "Expected values to be not deep-equal" : String(message) });
    }
  };
  assertObject.throws = throws;
  assertObject.rejects = rejects;
  assertObject.doesNotThrow = doesNotThrow;
  assertObject.fail = fail;
  assertObject.AssertionError = AssertionError;
  assertObject.strict = assertObject;
  assertObject.default = assertObject;
  void I;

  shims.assert = assertObject;
  shims["node:assert"] = assertObject;
  shims["assert/strict"] = assertObject;
  shims["node:assert/strict"] = assertObject;
})();
