# CJS loader fixtures

`vendor/` contains two unmodified npm packages, vendored so the true-CJS regression from
`spec/design-extension-runtime.md` §10.4 item 1 can run without network access or an
`npm install`:

| package | version | license | why it is here |
|---|---|---|---|
| `shell-quote` | 1.12.0 | MIT (`vendor/shell-quote/LICENSE`) | multi-file CJS: `index.js` does `exports.quote = require("./quote")`, so it only works with a real recursive `require` (not a self-contained ESM wrapper) and its named exports must be synthesized for `import { quote }` |
| `ms` | 2.1.3 | MIT (`vendor/ms/license.md`) | `module.exports = function (...) {}`, a CJS package whose export is a function |

The remaining directories are synthetic and cover the shapes the loader must preserve:

- `esm/` — `require()` of an `.mjs` file throws `ERR_REQUIRE_ESM` (design §3.5 item 2; jiti 2.7
  on Node 26 would return the namespace, so this is the one deliberately design-frozen difference).
- `esm_pkg/` — `require()` of a `.js` file in a `"type": "module"` package throws
  `ERR_REQUIRE_ESM`, same as above.
- `json/` — `require("./data")` parses JSON and caches by absolute path.
- `circular/` — circular `require` sees partially-filled `module.exports`, like Node.
- `records/` — a wrapped entry is a Node module record: it is pre-registered in
  `globalThis.__kiana_cjs` (so a child's `require(entry)` reuses it instead of evaluating twice),
  `module.id/filename/path/loaded/parent/children/require` are populated, and `__filename` has no
  `?v=` cache buster.
- `interop/` — jiti/Node CJS→ESM interop: `compiled.cjs` is esbuild/tsc output
  (`exports.__esModule = true; exports.default = factory`), whose default import is `factory`;
  `reexport/index.cjs` follows `__exportStar(require("./external.cjs"), exports)` so named imports
  link the same way Node's `cjs-module-lexer` allows.
- `shapes/` — `exports.x`, `module.exports = fn`, `exports = module.exports = fn`
  (the `@anthropic-ai/sdk` pattern that requires a writable `exports` binding), top-level `this`,
  `require.resolve`/`require.cache`/`require.main`, `require.resolve.paths` and the
  `{ paths }` option, sloppy-mode-only code (`implicit_global`, `with_scope`), and a shebang file.
