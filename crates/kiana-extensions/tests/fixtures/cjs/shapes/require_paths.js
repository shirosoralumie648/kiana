var paths = require.resolve.paths("ms");
module.exports = {
	pathsIsArray: Array.isArray(paths),
	firstSuffix: paths.length > 0 ? paths[0].slice(-13) : "",
	builtinPaths: require.resolve.paths("fs"),
	resolvedWithPaths: (function () {
		try {
			return require.resolve("./quote.js", { paths: globalThis.__test_paths });
		} catch (error) {
			return "ERR:" + error.message;
		}
	})(),
	builtinResolve: require.resolve("fs"),
};
