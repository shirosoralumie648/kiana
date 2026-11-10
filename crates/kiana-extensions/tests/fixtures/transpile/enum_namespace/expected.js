var Color = /* @__PURE__ */ function(Color) {
	Color[Color["Red"] = 0] = "Red";
	Color[Color["Green"] = 3] = "Green";
	Color[Color["Blue"] = 4] = "Blue";
	return Color;
}(Color || {});
var Direction = /* @__PURE__ */ function(Direction) {
	Direction[Direction["Up"] = 0] = "Up";
	Direction[Direction["Down"] = 1] = "Down";
	return Direction;
}(Direction || {});
let Geometry;
(function(_Geometry) {
	const origin = _Geometry.origin = {
		x: 0,
		y: 0
	};
})(Geometry || (Geometry = {}));
export { Color, Direction, Geometry };
