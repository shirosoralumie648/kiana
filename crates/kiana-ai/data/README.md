# kiana-ai model catalog data (vendored)

This directory holds the model catalog JSON that `kiana-ai` compiles into the static
`MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS` tables (`src/providers/catalog.rs`).

## Provenance

Generated on **2026-10-26** by the TypeScript reference implementation:

```
cd packages/ai
npm run hydrate-model-data      # = node scripts/generate-models.ts --strict --data-only
```

which writes `packages/ai/src/providers/data/`. That directory is `.gitignore`d in the
TypeScript repo, so the files are **vendored here** instead of being regenerated during
the Rust build (decision §8 第 5 条 in `ARCHITECTURE.md`).

Contents (43 files, 1.1 MiB):

- `<provider>.json` — 42 provider catalogs, one per builtin provider.
- `.manifest.json` — `schemaVersion: 6`, `generatedAt`, `structureHash`, and a
  sha256 per data file. Every hash was verified against the copied bytes when the
  directory was vendored.

## Build-time consumption

`crates/kiana-ai/build.rs` (owned by ai-team, stage 1) reads this directory and generates
the static tables. The directory is selected as:

1. `KIANA_MODEL_DATA_DIR` when set (env var, see `ARCHITECTURE.md` §4.5 and §8 第 21 条) — an absolute or
   workspace-relative path to a directory containing the same file set.
2. Otherwise the in-repo `crates/kiana-ai/data/` (this directory), so a plain
   `cargo build` never needs Node.

`build.rs` must re-emit `cargo:rerun-if-changed=` for every JSON file and verify each
sha256 against `.manifest.json` (the `check:model-data` gate of the TypeScript build).
`getBuiltinModelDataGeneratedAt()` parses `.manifest.json`'s `generatedAt`.

## Regenerating

Run the TypeScript command above in an up-to-date `kiana` checkout, then copy the directory
over this one and keep `.manifest.json`. Do not hand-edit these files: the Rust tables
must stay byte-identical to what the TypeScript generator emits (see
`spec/ai-core.md` §4.1).

## Provider order

`spec/ai-core.md` §2.3.1 fixes the 42-provider order used for `builtinProviders()` and
the CLI `list` output. `.manifest.json`'s `files` key order is **not** that order: the
generator writes it with `sortedRecord()` (code-unit order), so e.g. `google-vertex.json`
sorts before `google.json` and `minimax-cn.json` before `minimax.json`. `build.rs` /
`providers/catalog.rs` therefore take the order from their own `PROVIDER_ORDER` table
(copied from §2.3.1) and never from the manifest.
