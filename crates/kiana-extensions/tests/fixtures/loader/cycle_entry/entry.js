import { helper } from "./dep.js";

globalThis.__directCycleEntryLoads = (globalThis.__directCycleEntryLoads ?? 0) + 1;

export const helperValue = 11;
export const fromB = helper();
globalThis.__directCycleFromB = fromB;
export default fromB;
