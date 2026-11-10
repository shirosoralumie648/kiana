export class Counter {
	#count = 0;
	static instances = 0;
	declare label: string;
	increment(by?: number): number {
		this.#count += by ?? 1;
		return this.#count;
	}
}

export const optional = { a: { b: 1 } }?.a?.b;
export const nested = (value: unknown) => (value as { x?: string }).x ?? "fallback";
