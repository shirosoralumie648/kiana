import { marker, depModuleLoads } from "./counter_dep.js";

const state = globalThis as {
	__counterEntryLoads?: number;
	__counterMarker?: { loads: number };
};
state.__counterEntryLoads = (state.__counterEntryLoads ?? 0) + 1;
marker.loads += 1;
state.__counterMarker = marker;

export const entryModuleLoads = state.__counterEntryLoads;
export const depLoads = depModuleLoads;
export { marker };
