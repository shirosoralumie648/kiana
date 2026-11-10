// A local binding named `exports` (function parameter) is not the CJS export record.
export function fill(exports: Record<string, number>): Record<string, number> {
	exports.value = 1;
	return exports;
}
export default "local";
