export const relative: string[] = require.resolve.paths("./dep.cjs");
export const bare: string[] = require.resolve.paths("probe-pkg");
export const builtin: string[] | null = require.resolve.paths("node:path");
export const absolute: string[] = require.resolve.paths("/elsewhere/dep.cjs");
export const dotdot: string[] = require.resolve.paths("..foo");
export const dotfile: string[] = require.resolve.paths(".foo");
export default relative;
