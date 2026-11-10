const state = globalThis;
state.__cjsEntryLoads = (state.__cjsEntryLoads ?? 0) + 1;
module.exports = { loads: state.__cjsEntryLoads };
state.__cjsLast = module.exports;
