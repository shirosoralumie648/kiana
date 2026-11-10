// Node 26 surface probe shared by the engine test and the golden generator.
//
// The same source is evaluated by Node 26 (via `vm.runInThisContext`, whose
// completion value is the final `JSON.stringify(out)` expression) and by
// `JsEngine::eval_script::<String>`. `node26_surface.node26.json` is the Node
// output verbatim; `tests/engine_timers.rs` asserts the engine matches it.
//
// Surfaces: global enumerability, timer/setImmediate/queueMicrotask argument
// validation (message + code), AbortSignal.any validation, illegal AbortSignal
// constructor, AbortSignal.timeout validation, onabort/abort ordering, abort
// reason shape, Immediate/Timeout handle shape.
// Evaluated verbatim by both runtimes; the trailing expression is the output.
const out = {};
const cap = (fn) => {
  try { const value = fn(); return { ok: true, type: typeof value }; }
  catch (error) { return { ok: false, name: error.name, message: error.message, code: error.code }; }
};

out.globals = ["AbortController", "AbortSignal", "DOMException", "setTimeout", "setInterval", "queueMicrotask"]
  .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)?.enumerable]);

out.timerValidation = [
  ["string", () => setTimeout("x", 1)],
  ["number", () => setTimeout(3, 1)],
  ["null", () => setTimeout(null, 1)],
  ["object", () => setInterval({}, 1)],
  ["array", () => setTimeout([], 1)],
  ["bool", () => setTimeout(true, 1)],
  ["symbol", () => setTimeout(Symbol("s"), 1)],
  ["bigint", () => setTimeout(1n, 1)],
  ["undefined", () => setTimeout(undefined, 1)],
  ["immediate-number", () => setImmediate(1)],
  ["microtask-number", () => queueMicrotask(2)],
  ["microtask-null", () => queueMicrotask(null)],
].map(([label, fn]) => [label, cap(fn)]);

out.anyValidation = [
  ["noargs", () => AbortSignal.any()],
  ["object", () => AbortSignal.any({})],
  ["number", () => AbortSignal.any(3)],
  ["string", () => AbortSignal.any("abc")],
  ["nullElement", () => AbortSignal.any([null])],
  ["objectElement", () => AbortSignal.any([{}])],
  ["secondElement", () => AbortSignal.any([new AbortController().signal, 5])],
  ["empty", () => AbortSignal.any([]).aborted],
].map(([label, fn]) => [label, cap(fn)]);

out.constructor = cap(() => new AbortSignal());
out.constructorCode = (() => { try { new AbortSignal(); } catch (e) { return e.code; } })();

out.timeoutValidation = [
  ["neg", () => AbortSignal.timeout(-1)],
  ["fraction", () => AbortSignal.timeout(1.5)],
  ["string", () => AbortSignal.timeout("5")],
  ["null", () => AbortSignal.timeout(null)],
  ["big", () => AbortSignal.timeout(4294967296)],
].map(([label, fn]) => [label, cap(fn)]);

const controller = new AbortController();
const order = [];
controller.signal.addEventListener("abort", () => order.push("listener"));
controller.signal.onabort = function (event) { order.push("onabort:" + this.aborted + ":" + event.type); };
controller.abort();
out.abortOrder = order;
const c2 = new AbortController();
c2.signal.onabort = () => order.push("replaced");
c2.signal.onabort = null;
c2.abort();
out.onabortNull = order.includes("replaced") === false;
out.reason = (() => {
  const c = new AbortController();
  c.abort();
  const r = c.signal.reason;
  return [r.constructor.name, r.name, r.code, r instanceof Error, r instanceof DOMException,
    Object.prototype.toString.call(r), String(r), Object.keys(r)];
})();
out.timeoutReason = (() => {
  const r = AbortSignal.timeout(1000).reason;
  return r === undefined ? "undefined" : [r.name, r.code];
})();
const immediate = setImmediate(() => {});
out.immediate = [typeof immediate, immediate.constructor.name, typeof immediate.ref,
  typeof immediate.unref, typeof immediate.hasRef, immediate.hasRef(), Number.isNaN(Number(immediate))];
clearImmediate(immediate);
out.timeoutHandle = (() => {
  const t = setTimeout(() => {}, 1.9);
  const shape = [typeof t, t.constructor.name, Number(t) > 0, String(t) === String(Number(t)),
    t._idleTimeout, t.hasRef(), typeof t.refresh];
  clearTimeout(t);
  return shape;
})();

JSON.stringify(out);
