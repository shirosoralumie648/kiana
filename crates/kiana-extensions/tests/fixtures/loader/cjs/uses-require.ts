const dep = require("./dep.cjs");

export const value: number = dep.value;
export const resolved: string = require.resolve("./dep.cjs");
export const mainIsUndefined: boolean = require.main === undefined;
export const cacheHasDep: boolean =
	require.cache[require.resolve("./dep.cjs")] !== undefined;
export default value;
