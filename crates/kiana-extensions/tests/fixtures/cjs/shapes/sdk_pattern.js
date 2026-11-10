// Mirrors @anthropic-ai/sdk index.js:3: `exports = module.exports = function (...) {...}`.
// With the outer IIFE this is a writable parameter binding; a plain `const exports` would throw
// "'exports' is read-only" (design §3.5 item 6).
exports = module.exports = function double(value) {
	return value * 2;
};
exports.version = "1.0.0";
