export function getDep() {
	return require("./dep.cjs").value;
}
export const hasRequire = typeof require;
export const filenameType = typeof __filename;
export const keys = Object.keys(import.meta).sort().join(",");
export default "esmpkg-mjs";
