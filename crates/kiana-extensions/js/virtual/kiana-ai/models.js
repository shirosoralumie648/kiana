// @earendil-works/kiana-ai/models | @mariozechner/kiana-ai/models (design §8.2, regression #11156).
//
// Both keys alias this module name, so `import * as models` is the same object under either scope.
//
// The export list is the complete runtime surface of `packages/ai/src/models.ts`.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-ai/models'];

export const AI_TELEMETRY_SCHEMA = ns.AI_TELEMETRY_SCHEMA;
export const ModelsError = ns.ModelsError;
export const calculateCost = ns.calculateCost;
export const clampThinkingLevel = ns.clampThinkingLevel;
export const createModels = ns.createModels;
export const createProvider = ns.createProvider;
export const getModelType = ns.getModelType;
export const getSupportedThinkingLevels = ns.getSupportedThinkingLevels;
export const hasApi = ns.hasApi;
export const isModelType = ns.isModelType;
export const modelsAreEqual = ns.modelsAreEqual;

// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;
