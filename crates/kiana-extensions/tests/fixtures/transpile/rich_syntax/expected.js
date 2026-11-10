export class Counter {
	#count = 0;
	static instances = 0;
	increment(by) {
		this.#count += by ?? 1;
		return this.#count;
	}
}
export const optional = { a: { b: 1 } }?.a?.b;
export const nested = (value) => value.x ?? "fallback";
