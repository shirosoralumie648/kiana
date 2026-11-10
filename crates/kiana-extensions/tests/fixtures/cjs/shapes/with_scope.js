// `with` is a SyntaxError in strict mode; Node's CJS loader allows it.
var scope = { value: 1 };
with (scope) {
	exports.value = value;
}
