"use strict";
var resolved = require.resolve("./quote");
var loaded = require("./quote");
module.exports = {
	resolved: resolved,
	loadedMatchesCache: loaded === require.cache[resolved].exports,
	cacheIsGlobal: require.cache === globalThis.__kiana_cjs,
	main: require.main,
	inCache: Object.prototype.hasOwnProperty.call(require.cache, resolved),
	requireType: typeof require,
	resolveType: typeof require.resolve,
};
