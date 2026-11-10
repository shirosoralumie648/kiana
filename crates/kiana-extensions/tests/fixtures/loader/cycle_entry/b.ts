import { helperValue } from "./a.ts";

const state = globalThis as { __cycleDepLoads?: number };
state.__cycleDepLoads = (state.__cycleDepLoads ?? 0) + 1;

export const depLoads: number = state.__cycleDepLoads;
export function helper(): string {
	return `b-${helperValue}`;
}
