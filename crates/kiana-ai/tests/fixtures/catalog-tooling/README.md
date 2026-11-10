# catalog-tooling fixtures

Inputs and golden outputs for `crates/kiana-ai/tests/{fireworks_model_generation,generate_models_strict,
image_model_data,model_catalog_types,model_cost_tiers,model_data_validation,openrouter_reasoning_options,
reasoning_options}.rs`, which port `packages/ai/test/*.test.ts` for the model-catalog tooling.

The TypeScript tests run `node scripts/generate-models.ts` against the network. These fixtures replace
the network with captured JSON snapshots, so the Rust tests are hermetic and much smaller than the real
upstream payloads.

## `inputs/`

Captured `2026-10-26` from the endpoints `generate-models.ts` fetches:

| file | source |
|---|---|
| `models-dev.json` | `https://models.dev/api.json` |
| `models-dev-decision.json` | `https://models.dev/models.json?type=decision` |
| `openrouter.json` | `https://openrouter.ai/api/v1/models` |
| `openrouter-image.json` | `https://openrouter.ai/api/v1/models?output_modalities=image` |
| `openrouter-decisions.json` | `https://openrouter.ai/api/v1/models?output_modalities=decisions` |
| `ai-gateway.json` | `https://ai-gateway.vercel.sh/v1/models` |
| `nvidia.json` | `https://integrate.api.nvidia.com/v1/models` |
| `radius.json` | the Radius gateway config the generator consumes |
| `openrouter-image-empty.json` | `{"data": []}` — the `--strict` empty-image-listing case |

The `models-dev.json` / `openrouter*.json` snapshots are trimmed to the providers and fields the
generator reads. Trimming was verified not to change the output: the full capture (7.2 MB) and the
trimmed capture (1.1 MB) produced byte-identical golden output (`diff -rq` reported no differences).

## `golden/`

| path | produced by |
|---|---|
| `stdout.txt` | `generate-models.ts` stdout (the `Model Statistics:` block at the end) |
| `providers.json` | `--json-only --json-output` |
| `providers/<id>.{json,all.json}` | same run, 9 representative providers (fireworks, openrouter, github-copilot, qwen-token-plan-individual, typesafe, cloudflare-workers-ai, kimi-coding, moonshotai, amazon-bedrock) |
| `typescript/*.models.ts`, `typescript/models.generated.ts` | the checked-in generated TypeScript from `packages/ai/src/` (template golden, copied verbatim) |
| `field-orders.json` | per-model JSON key order of the TS output: `provider -> "api|type:id" -> "field,order"` (the data shards are hashed byte-for-byte, so key order is load-bearing) |

## `date-parse.json`, `user-agent.json`

Captured from Node 26 (`Date.parse` / `parseKianaUserAgent` over a differential corpus); they pin the V8
date parser and `KIANA_USER_AGENT_RE` semantics, including the non-ASCII and whitespace edge cases.

`models.json` / `models.all.json` are not stored: they are the same data as the per-provider shards, and
the tests check them against those shards instead of keeping a second 900 KB copy.

## Regenerating

```sh
# with a checkout of the TS repo
NODE_OPTIONS="--import <mock-fetch.mjs>" node packages/ai/scripts/generate-models.ts \
  --json-only --json-output <dir>
```

`<mock-fetch.mjs>` patches `globalThis.fetch` to serve `inputs/` for the URLs above. The Rust tests that
consume these files are `generate_models_strict.rs` (golden catalog + statistics) and
`generate_models_strict.rs::generator_options_and_templates` (TypeScript templates).
