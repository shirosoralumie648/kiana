import { helperValue } from "./entry.js";

globalThis.__directCycleDepLoads = (globalThis.__directCycleDepLoads ?? 0) + 1;

export function helper() {
	return `b-${helperValue}`;
}
