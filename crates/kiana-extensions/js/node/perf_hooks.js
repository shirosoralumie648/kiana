// node:perf_hooks
(function () {
  "use strict";

  var shims = globalThis.__kiana_node_shims;

  class PerformanceEntry {
    constructor(name, entryType, startTime, duration) {
      this.name = name;
      this.entryType = entryType;
      this.startTime = startTime;
      this.duration = duration;
    }
    toJSON() {
      return { name: this.name, entryType: this.entryType, startTime: this.startTime, duration: this.duration };
    }
  }

  var perfHooksObject = {
    performance: globalThis.performance,
    PerformanceObserver: class PerformanceObserver {
      constructor(callback) { this._callback = callback; }
      observe() {}
      disconnect() {}
      static get supportedEntryTypes() { return []; }
    },
    PerformanceObserverEntryList: class PerformanceObserverEntryList {},
    PerformanceEntry: PerformanceEntry,
    PerformanceMark: class PerformanceMark extends PerformanceEntry {
      constructor(name, options) { super(name, "mark", options && options.startTime || 0, 0); }
    },
    PerformanceMeasure: class PerformanceMeasure extends PerformanceEntry {
      constructor(name, options) { super(name, "measure", 0, 0); }
    },
    constants: {},
  };

  shims.perf_hooks = perfHooksObject;
  shims["node:perf_hooks"] = perfHooksObject;
})();
