enum Color {
	Red,
	Green = 3,
	Blue,
}

const enum Direction {
	Up,
	Down,
}

namespace Geometry {
	export const origin = { x: 0, y: 0 };
}

export { Color, Direction, Geometry };
