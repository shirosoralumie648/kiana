# Extension corpus fixtures

`extensions/` is a **verbatim** copy of
`packages/coding-agent/examples/extensions/` from the TypeScript reference
(80 entries: 70 single-file extensions + 9 directories + `README.md`, 86 `.ts`
files). Do not edit it here; changes must come from the reference checkout.

```sh
rsync -a packages/coding-agent/examples/extensions/ \
  crates/kiana-extensions/tests/fixtures/corpus/extensions/
```

## node_modules

The examples import real npm packages. They are vendored so `cargo test` needs no
network and no real `~/.kiana` (`ARCHITECTURE.md` §4.8 rule 8):

| package | version | used by |
|---|---|---|
| `ms` | 2.1.3 | `with-deps/` (its own package.json dependency) |
| `shell-quote` | 1.12.0 | `@anthropic-ai/sandbox-runtime`, §10.4 regression |
| `zod` | 3.25.76 | `@anthropic-ai/sandbox-runtime`, §10.4 regression |
| `@anthropic-ai/sdk` | 0.52.0 | `custom-provider-anthropic/`, §10.4 regression |
| `@anthropic-ai/sandbox-runtime` | 0.0.26 | `sandbox/` |
| `@pondwader/socks5-server` | 1.0.10 | `sandbox-runtime` dependency |
| `lodash-es` | 4.18.1 | `sandbox-runtime` dependency |
| `commander` | 12.1.0 | `sandbox-runtime` dependency (`cli.js`, not loaded) |

Each package keeps its own license file. `@earendil-works/gondolin`
(`gondolin/`) is **not** vendored: it needs the native `ssh2` addon
(`cpu-features`), so that example is level D (design §7.2).

## registrations.json

The registration matrix recorded for the 77 A-level entries (tools / commands /
flags / shortcuts / handlers / renderers / providers / virtual models). It is the
key-set checked against `ca-extensions.md` §3.11. The two C/D entries
(`doom-overlay/` needs WebAssembly, `gondolin/` needs the native `ssh2` addon)
are recorded in `KNOWN_SKIPS` with their reason and are absent from the matrix.
`preset.ts` and `plan-mode/` are present with their `shortcuts`
(`ctrl+shift+u` / `ctrl+alt+p`), so `kiana.registerShortcut` is verified by the
corpus test.
