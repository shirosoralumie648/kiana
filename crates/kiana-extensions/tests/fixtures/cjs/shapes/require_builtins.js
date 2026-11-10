const net = require("net");
const netAgain = require("net");
const events = require("node:events");
const typebox = require("typebox");
module.exports = {
	sameInstance: net === netAgain,
	hasCreateServer: typeof net.createServer,
	hasEventEmitter: typeof events.EventEmitter,
	virtualSame: typebox === globalThis.__kiana_virtual["typebox"],
	namespaceValue: typebox.Type.Object,
	resolvedBare: require.resolve("fs"),
	resolvedNode: require.resolve("node:fs"),
	builtinPaths: require.resolve.paths("net"),
};
