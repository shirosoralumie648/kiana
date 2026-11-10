// @earendil-works/kiana-ai/providers/all | @mariozechner/kiana-ai/providers/all (design §3.3).
//
// The export list is the complete runtime surface of `packages/ai/src/providers/all.ts`.

const ns = globalThis.__kiana_virtual['@earendil-works/kiana-ai/providers/all'];

export const builtinModels = ns.builtinModels;
export const builtinProviders = ns.builtinProviders;
export const getAllBuiltinModels = ns.getAllBuiltinModels;
export const getBuiltinClassifierModel = ns.getBuiltinClassifierModel;
export const getBuiltinClassifierModels = ns.getBuiltinClassifierModels;
export const getBuiltinImageModel = ns.getBuiltinImageModel;
export const getBuiltinImageModels = ns.getBuiltinImageModels;
export const getBuiltinModel = ns.getBuiltinModel;
export const getBuiltinModelDataGeneratedAt = ns.getBuiltinModelDataGeneratedAt;
export const getBuiltinModels = ns.getBuiltinModels;
export const getBuiltinProviders = ns.getBuiltinProviders;
export const radiusProvider = ns.radiusProvider;

// TS has no `default` export; jiti's interop hands the namespace object to
// `import x from "..."` (verified with jiti 2.7 in upstream-reference). QuickJS requires a real
// export, so the seed object itself is the default.
export default ns;
