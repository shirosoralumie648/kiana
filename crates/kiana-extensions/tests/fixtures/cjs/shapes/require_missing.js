let shape = { ok: true };
try {
	require("./definitely-missing");
} catch (error) {
	shape = { ok: false, name: error.name, code: error.code, message: error.message };
}
module.exports = shape;
