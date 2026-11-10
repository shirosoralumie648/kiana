interface Point {
	x: number;
	y: number;
}

type Pair<T> = [T, T];

const point: Point = { x: 1, y: 2 };
const pair = [point, point] satisfies Pair<Point>;
const literal = { a: 1 } as const;
const maybe: string | undefined = undefined;

function add(a: number, b: number): number {
	return a + b;
}

export { add, pair, literal, maybe };
