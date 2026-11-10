const out = [];
for (const value of [5, null, undefined, true, {}, [1, 2]]) {
	try {
		require(value);
		out.push({ ok: true });
	} catch (error) {
		out.push({ name: error.name, code: error.code, message: error.message });
	}
}
try {
	require.resolve(5);
} catch (error) {
	out.push({ resolve: true, name: error.name, code: error.code, message: error.message });
}
try {
	require.resolve.paths(5);
} catch (error) {
	out.push({ paths: true, name: error.name, code: error.code, message: error.message });
}
module.exports = out;
