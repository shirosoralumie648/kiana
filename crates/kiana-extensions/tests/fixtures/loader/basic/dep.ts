const state = globalThis as { __loaderDepLoads?: number };
state.__loaderDepLoads = (state.__loaderDepLoads ?? 0) + 1;

export const value = 41;
export const loads = state.__loaderDepLoads;
