import { helper } from "./b.ts";

const state = globalThis as { __cycleEntryLoads?: number; __cycleFromB?: string };
state.__cycleEntryLoads = (state.__cycleEntryLoads ?? 0) + 1;

export const entryLoads: number = state.__cycleEntryLoads;
export const helperValue = 11;
export const fromB: string = helper();
state.__cycleFromB = fromB;
export default entryLoads;
